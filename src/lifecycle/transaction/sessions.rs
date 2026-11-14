//! Filtered recovery ZIPs. Copying history and removing its owners are separate decisions.
use std::collections::{HashMap, HashSet};

use crate::index::StoredSession;
use crate::lifecycle::Member;
use crate::live::{LiveSessionGuard, LiveSessionSnapshot};

use super::archive::{self, Destination};
use super::*;

#[cfg(test)]
thread_local! {
    static AFTER_COPY: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SessionArchiveOptions {
    pub remove: bool,
    pub yes: bool,
    pub include_related: bool,
}

#[derive(Debug)]
pub struct SessionArchiveSkip {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct SessionArchiveReport {
    pub selected: usize,
    pub items: usize,
    pub history_files: usize,
    pub removed_items: usize,
    pub included: Vec<PathBuf>,
    pub skipped: Vec<SessionArchiveSkip>,
}

#[derive(Debug, PartialEq, Eq)]
struct Group {
    selected: BTreeSet<PathBuf>,
    copies: BTreeMap<PathBuf, Member>,
    removals: BTreeSet<PathBuf>,
}

/// Export selected logical sessions, optionally retiring their native payloads.
/// The caller refreshes the index before selecting and initializes legacy trash first.
pub fn archive_sessions(
    roots: &SessionRoots,
    selected: &[StoredSession],
    destination: &Path,
    options: SessionArchiveOptions,
) -> Result<SessionArchiveReport> {
    ensure!(
        options.remove || !options.include_related,
        "--include-related requires --archive-sessions"
    );
    let mut report = SessionArchiveReport::default();
    if selected.is_empty() {
        return Ok(report);
    }
    let paths = roots.trash.as_ref().context("trash store is unavailable")?;
    let destination = archive::destination_path(paths, destination)?;
    let (_lock, operations) = lock(roots)?;
    recover_locked(roots, &operations)?;
    migrate_locked(paths, &operations)?;
    let tracker = LiveSessionTracker::new(roots.live_sessions.clone());
    let mut remaining = selected.iter().collect::<Vec<_>>();
    let mut forced_skips = HashMap::new();
    let mut overwrite: Option<Option<Destination>> = None;

    loop {
        // A protected selected session need not be parsed or inspected further.
        let live = tracker.snapshot();
        remaining.retain(|session| {
            if let Some(reason) = live.protection_reason(session.agent, &session.session_id) {
                report.skipped.push(SessionArchiveSkip {
                    path: session.file_path.clone(),
                    reason: reason.message().to_owned(),
                });
                false
            } else {
                true
            }
        });
        if remaining.is_empty() {
            return Ok(report);
        }
        let groups = groups(roots, &remaining, options.remove)?;
        if skip_groups(&groups, &live, &forced_skips, &mut remaining, &mut report) {
            continue;
        }
        let copies = groups
            .iter()
            .flat_map(|g| g.copies.iter().map(|(p, m)| (p.clone(), m.clone())))
            .collect::<BTreeMap<_, _>>();
        let removals = groups
            .iter()
            .flat_map(|g| g.removals.iter().cloned())
            .collect::<BTreeSet<_>>();
        let selected_ids = remaining
            .iter()
            .map(|s| (s.agent, s.session_id.clone()))
            .collect::<HashSet<_>>();
        let included = removals
            .iter()
            .filter_map(|path| {
                let member = &copies[path];
                (!selected_ids.contains(&(member.agent, member.thread_id.clone()))).then(|| {
                    (
                        (member.agent.to_string(), member.thread_id.clone()),
                        path.clone(),
                    )
                })
            })
            .collect::<BTreeMap<_, _>>();
        let guards = match acquire_guards(&tracker, &copies) {
            Ok(guards) => guards,
            Err((agent, reason)) => {
                forced_skips.insert(agent, reason);
                continue;
            }
        };
        ensure!(options.include_related || included.is_empty(),
            "archive requires additional sessions:\n{}\nSelect them explicitly or pass --include-related",
            included.iter().map(|((agent, id), path)| format!("  {agent} {id}: {}", path.display())).collect::<Vec<_>>().join("\n"));
        let items = package_items(paths, &copies, &removals)?;
        for item in &items {
            for source in &item.sources {
                ensure!(
                    !destination.starts_with(super::super::resolved_path(&source.path))
                        && !same_destination_file(&destination, &source.path)?,
                    "ZIP destination overlaps a source payload: {}",
                    source.path.display()
                );
            }
        }
        if overwrite.is_none() {
            overwrite = Some(archive::confirm_destination(&destination, options.yes)?);
        }
        let scratch = scratch_directory(parent(&destination)?)?;
        let staged = scratch.path().join("sessions.zip");
        let copied = archive::write_verified_zip(&staged, |writer, entries| {
            for item in &items {
                if let Some(metadata) = &item.metadata {
                    archive::add_directory(writer, entries, format!("{}/", item.name))?;
                    archive::add_directory(writer, entries, format!("{}/payload/", item.name))?;
                    archive::add_file(
                        writer,
                        entries,
                        format!("{}/metadata.json", item.name),
                        metadata.len() as u64,
                        metadata.as_slice(),
                    )?;
                }
                for source in &item.sources {
                    ensure!(
                        archive::add_tree_as(writer, entries, &source.path, source.name.clone())?
                            == source.stamp.digest,
                        "source changed while archiving {}",
                        source.path.display()
                    );
                }
            }
            Ok(())
        });
        #[cfg(test)]
        AFTER_COPY.with(|hook| {
            let callback = hook.borrow_mut().take();
            if let Some(callback) = callback {
                callback();
            }
        });
        // Claude has presence markers rather than writer locks. If it became live
        // during copying, drop the affected groups and rebuild before publication.
        if skip_groups(
            &groups,
            &tracker.snapshot_with_guards(&guards),
            &forced_skips,
            &mut remaining,
            &mut report,
        ) {
            drop(guards);
            continue;
        }
        copied?;
        verify_items(&items)?;
        ensure!(
            groups == self::groups(roots, &remaining, options.remove)?,
            "session group changed; run the command again to select current sessions"
        );
        for session in &remaining {
            if let Some(source) = &session.source {
                ensure!(
                    !source.history.is_complete() || source.is_current(),
                    "conversation changed since selection: {}",
                    session.file_path.display()
                );
            }
        }
        let plans = removal_plans(roots, &copies, &removals)?;
        let stamp = Stamp::read(&staged)?;
        archive::publish_zip(
            &staged,
            &destination,
            overwrite.as_ref().and_then(Option::as_ref),
            &stamp,
        )?;
        if options.remove {
            verify_items(&items)?;
            let live = tracker.snapshot_with_guards(&guards);
            for member in copies.values() {
                ensure!(
                    live.protection_reason(member.agent, &member.thread_id)
                        .is_none(),
                    "session became protected; originals retained and verified ZIP remains: {}",
                    member.path.display()
                );
            }
            for plan in &plans {
                recheck(roots, plan)?;
            }
            let mut staging = Staging::new(plans);
            staging.journal.archive = Some(VerifiedArchive {
                path: destination,
                stamp,
            });
            for item in &items {
                if removals.contains(&item.primary) {
                    for source in &item.sources {
                        staging.retire(source.path.clone(), source.stamp.clone())?;
                    }
                }
            }
            staging.execute(&operations)?;
        }
        report.selected = remaining.len();
        report.items = items.len();
        report.removed_items = removals.len();
        report.history_files = copies
            .values()
            .filter(|m| {
                !selected_ids.contains(&(m.agent, m.thread_id.clone()))
                    && !removals.contains(&m.path)
            })
            .count();
        report.included = included.into_values().collect();
        return Ok(report);
    }
}

fn same_destination_file(destination: &Path, source: &Path) -> Result<bool> {
    Ok(exists(destination)? && entry_identity(destination)? == entry_identity(source)?)
}

fn groups(roots: &SessionRoots, selected: &[&StoredSession], remove: bool) -> Result<Vec<Group>> {
    let mut groups: Vec<Group> = Vec::new();
    for agent in [Agent::Claude, Agent::Codex, Agent::Antigravity] {
        let selected = selected
            .iter()
            .filter(|s| s.agent == agent)
            .collect::<Vec<_>>();
        if selected.is_empty() {
            continue;
        }
        let seeds = selected
            .iter()
            .map(|s| s.file_path.clone())
            .collect::<Vec<_>>();
        let graph = DependencyGraph::discover(roots, agent, &seeds)?;
        ensure!(graph.errors.is_empty(), "{}", graph.errors.join("\n"));
        for session in selected {
            if let Some(history) = &session.history {
                ensure!(
                    history.status != crate::conversation::HistoryStatus::Ambiguous,
                    "cannot resolve selected history for {}: {}",
                    session.file_path.display(),
                    history.issues.join("; ")
                );
            }
            let member = graph.members.get(&session.file_path).with_context(|| {
                graph
                    .member_errors
                    .get(&session.file_path)
                    .cloned()
                    .unwrap_or_else(|| {
                        format!(
                            "selected session is unavailable: {}",
                            session.file_path.display()
                        )
                    })
            })?;
            ensure!(
                member.thread_id == session.session_id,
                "selected session identity changed: {}",
                session.file_path.display()
            );
            let owned = graph
                .members
                .values()
                .filter(|m| m.path == member.path || m.same_thread(member))
                .map(|m| m.path.clone())
                .collect::<BTreeSet<_>>();
            let removals = if remove {
                let expanded = graph.plan(owned.clone(), Operation::Delete);
                // Unlike whole-store trash purge, this command can explicitly include
                // active dependents. Replan with that complete removal set as seeds.
                let plan = graph.plan(
                    expanded.members.keys().cloned().collect(),
                    Operation::Delete,
                );
                plan.require_safe()?;
                plan.members.into_keys().collect()
            } else {
                BTreeSet::new()
            };
            let mut group = Group {
                selected: [session.file_path.clone()].into(),
                copies: history_members(&graph, if remove { &removals } else { &owned })?,
                removals,
            };
            // One protected member must skip every overlapping removal request.
            while let Some(index) = groups
                .iter()
                .position(|g| g.removals.iter().any(|p| group.removals.contains(p)))
            {
                let other = groups.remove(index);
                group.selected.extend(other.selected);
                group.copies.extend(other.copies);
                group.removals.extend(other.removals);
            }
            groups.push(group);
        }
    }
    groups.sort_by(|a, b| a.selected.cmp(&b.selected));
    Ok(groups)
}

fn history_members(
    graph: &DependencyGraph,
    seeds: &BTreeSet<PathBuf>,
) -> Result<BTreeMap<PathBuf, Member>> {
    let mut pending = seeds.iter().cloned().collect::<Vec<_>>();
    let mut members = BTreeMap::new();
    while let Some(path) = pending.pop() {
        if members.contains_key(&path) {
            continue;
        }
        let member = graph
            .members
            .get(&path)
            .context("required session disappeared")?;
        if let Some(base) = &member.history_base {
            let header = crate::conversation::metadata_line(&member.path)?
                .context("missing Codex metadata")?;
            let cutoff = &header["payload"]["history_base"];
            ensure!(
                cutoff["end_ordinal_exclusive"]
                    .as_u64()
                    .is_some_and(|n| n > 0)
                    && cutoff["end_byte_offset"].as_u64().is_some_and(|n| n > 0),
                "invalid or missing history cutoff in {}",
                path.display()
            );
            let parents = graph
                .members
                .values()
                .filter(|m| member.requires(m))
                .collect::<Vec<_>>();
            ensure!(
                !parents.is_empty(),
                "missing required history {base} for {}",
                path.display()
            );
            pending.extend(parents.iter().map(|m| m.path.clone()));
        }
        members.insert(path, member.clone());
    }
    // Detect cycles iteratively, including histories added through related sessions.
    let bases = members
        .values()
        .map(|m| (m.rollout_id.as_str(), m.history_base.as_deref()))
        .collect::<BTreeMap<_, _>>();
    let mut done = BTreeSet::new();
    for start in bases.keys() {
        let mut chain = BTreeSet::new();
        let mut next = Some(*start);
        while let Some(id) = next {
            if done.contains(id) {
                break;
            }
            ensure!(chain.insert(id), "cycle in required history at {id}");
            next = bases.get(id).copied().flatten();
        }
        done.extend(chain);
    }
    Ok(members)
}

fn skip_groups(
    groups: &[Group],
    live: &LiveSessionSnapshot,
    forced: &HashMap<Agent, String>,
    remaining: &mut Vec<&StoredSession>,
    report: &mut SessionArchiveReport,
) -> bool {
    let mut skipped = BTreeMap::new();
    for group in groups {
        let reason = group.copies.values().find_map(|m| {
            forced
                .get(&m.agent)
                .cloned()
                .or_else(|| {
                    live.protection_reason(m.agent, &m.thread_id)
                        .map(|r| r.message().to_owned())
                })
                .map(|reason| format!("required session {}: {reason}", m.path.display()))
        });
        if let Some(reason) = reason {
            for path in &group.selected {
                skipped.insert(path.clone(), reason.clone());
            }
        }
    }
    let before = remaining.len();
    remaining.retain(|session| {
        if let Some(reason) = skipped.remove(&session.file_path) {
            report.skipped.push(SessionArchiveSkip {
                path: session.file_path.clone(),
                reason,
            });
            false
        } else {
            true
        }
    });
    before != remaining.len()
}

fn acquire_guards(
    tracker: &LiveSessionTracker,
    copies: &BTreeMap<PathBuf, Member>,
) -> std::result::Result<Vec<LiveSessionGuard>, (Agent, String)> {
    let mut guards = Vec::new();
    for agent in [Agent::Claude, Agent::Codex, Agent::Antigravity] {
        let ids = copies
            .values()
            .filter(|m| m.agent == agent)
            .map(|m| m.thread_id.clone())
            .collect::<Vec<_>>();
        if !ids.is_empty() {
            guards.push(
                tracker
                    .acquire_group_guard(agent, &ids)
                    .map_err(|r| (agent, r.message().to_owned()))?,
            );
        }
    }
    Ok(guards)
}

fn removal_plans(
    roots: &SessionRoots,
    copies: &BTreeMap<PathBuf, Member>,
    removals: &BTreeSet<PathBuf>,
) -> Result<Vec<LifecyclePlan>> {
    let mut plans = Vec::new();
    for agent in [Agent::Claude, Agent::Codex, Agent::Antigravity] {
        let seeds = removals
            .iter()
            .filter(|p| copies[*p].agent == agent)
            .cloned()
            .collect::<BTreeSet<_>>();
        if seeds.is_empty() {
            continue;
        }
        let graph =
            DependencyGraph::discover(roots, agent, &seeds.iter().cloned().collect::<Vec<_>>())?;
        let plan = graph.plan(seeds.clone(), Operation::Delete);
        plan.require_safe()?;
        ensure!(
            plan.members.keys().eq(seeds.iter()),
            "session group changed before archive publication"
        );
        plans.push(plan);
    }
    Ok(plans)
}

struct Source {
    path: PathBuf,
    name: String,
    stamp: Stamp,
}

struct Item {
    primary: PathBuf,
    name: String,
    metadata: Option<Vec<u8>>,
    sources: Vec<Source>,
}

fn package_items(
    paths: &trash::TrashPaths,
    copies: &BTreeMap<PathBuf, Member>,
    removals: &BTreeSet<PathBuf>,
) -> Result<Vec<Item>> {
    let trash = TrashStore::new(paths.clone()).snapshot()?;
    let mut names = trash
        .iter()
        .filter_map(|i| i.path.file_name())
        .map(|n| n.to_owned())
        .collect::<BTreeSet<_>>();
    let mut next_id = 0u64;
    let mut items = Vec::new();
    for member in copies.values() {
        if member.trashed {
            let item = trash
                .iter()
                .find(|i| i.primary.as_ref() == Some(&member.path))
                .context("trash item disappeared")?;
            ensure!(
                !removals.contains(&member.path) || item.original_path().is_some(),
                "cannot verify trash item for removal: {}",
                item.path.display()
            );
            let name = archive::zip_name(
                "trash",
                Path::new(item.path.file_name().context("missing item name")?),
            )?;
            items.push(Item {
                primary: member.path.clone(),
                name: name.clone(),
                metadata: None,
                sources: vec![Source {
                    path: item.path.clone(),
                    name,
                    stamp: Stamp::read(&item.path)?,
                }],
            });
        } else {
            let name = loop {
                let name = format!("item-{next_id:016x}");
                next_id += 1;
                if names.insert(name.clone().into()) {
                    break format!("trash/{name}");
                }
            };
            let (metadata, payload) = trash::recovery_item(member.agent, &member.path)?;
            let sources = payload
                .into_iter()
                .zip(&metadata.members)
                .map(|(path, mapping)| {
                    Ok(Source {
                        stamp: Stamp::read(&path)?,
                        name: archive::zip_name(&name, &mapping.stored_path)?,
                        path,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            items.push(Item {
                primary: member.path.clone(),
                name,
                metadata: Some(serde_json::to_vec_pretty(&metadata)?),
                sources,
            });
        }
    }
    Ok(items)
}

fn verify_items(items: &[Item]) -> Result<()> {
    for item in items {
        for source in &item.sources {
            source.stamp.verify(&source.path)?;
        }
        if let Some(metadata) = &item.metadata {
            let metadata: TrashMetadata = serde_json::from_slice(metadata)?;
            let (current, payload) =
                trash::recovery_item(metadata.agent.context("missing provider")?, &item.primary)?;
            ensure!(
                current == metadata && payload.iter().eq(item.sources.iter().map(|s| &s.path)),
                "native payload changed: {}",
                item.primary.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::LiveSessionPaths;
    use crate::parse::parse_session_file;
    use crate::trash::TrashPaths;

    struct Fixture {
        temp: TempDir,
        roots: SessionRoots,
        sessions: Vec<StoredSession>,
    }

    impl Fixture {
        fn new() -> Result<Self> {
            crate::settings::isolate_config_root_for_tests();
            let temp = TempDir::new()?;
            let roots = SessionRoots {
                live_sessions: LiveSessionPaths {
                    claude_sessions_dir: Some(temp.path().join("presence")),
                    ..Default::default()
                },
                claude_projects: temp.path().join("claude"),
                codex_sessions: temp.path().join("codex/sessions"),
                antigravity_home: temp.path().join("antigravity"),
                trash: Some(TrashPaths::from_data_root(temp.path().join("data"))),
            };
            fs::create_dir_all(&roots.claude_projects)?;
            let mut sessions = Vec::new();
            for id in ["one", "two"] {
                let path = roots.claude_projects.join(format!("{id}.jsonl"));
                fs::write(&path, format!("{{\"type\":\"user\",\"sessionId\":\"{id}\",\"message\":{{\"role\":\"user\",\"content\":\"fixture\"}}}}\n"))?;
                sessions.push(StoredSession::from(
                    &parse_session_file(Agent::Claude, &path)?.unwrap(),
                ));
            }
            Ok(Self {
                temp,
                roots,
                sessions,
            })
        }

        fn run(&self) -> Result<SessionArchiveReport> {
            archive_sessions(
                &self.roots,
                &self.sessions,
                &self.temp.path().join("sessions.zip"),
                SessionArchiveOptions {
                    remove: true,
                    ..Default::default()
                },
            )
        }
    }

    #[test]
    fn changed_payload_aborts_before_publication_or_removal() -> Result<()> {
        let f = Fixture::new()?;
        let path = f.sessions[0].file_path.clone();
        AFTER_COPY.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                fs::write(path, b"changed after copying").unwrap()
            }))
        });
        assert!(f.run().unwrap_err().to_string().contains("entry changed"));
        assert!(!f.temp.path().join("sessions.zip").exists());
        assert!(f.sessions.iter().all(|s| s.file_path.is_file()));
        Ok(())
    }

    #[test]
    fn newly_live_session_is_excluded_from_rebuilt_zip_and_removal() -> Result<()> {
        let f = Fixture::new()?;
        let marker = f.temp.path().join("presence/one.json");
        let live_path = f.sessions[0].file_path.clone();
        AFTER_COPY.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                fs::create_dir_all(marker.parent().unwrap()).unwrap();
                OpenOptions::new().append(true).open(live_path).unwrap()
                    .write_all(b"{\"type\":\"user\",\"sessionId\":\"one\",\"message\":{\"role\":\"user\",\"content\":\"new live message\"}}\n").unwrap();
                fs::write(
                    marker,
                    format!("{{\"sessionId\":\"one\",\"pid\":{}}}", std::process::id()),
                )
                .unwrap();
            }))
        });
        let report = f.run()?;
        assert_eq!(report.selected, 1);
        assert_eq!(report.skipped.len(), 1);
        assert!(f.sessions[0].file_path.is_file());
        assert!(!f.sessions[1].file_path.exists());
        let extracted = f.temp.path().join("extracted");
        zip::ZipArchive::new(File::open(f.temp.path().join("sessions.zip"))?)?
            .extract(&extracted)?;
        let items = TrashStore::new(TrashPaths::from_data_root(extracted)).snapshot()?;
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].primary.as_ref().unwrap().file_name().unwrap(),
            "two.jsonl"
        );
        Ok(())
    }

    #[test]
    fn retirement_failure_rolls_back_every_original_and_keeps_verified_zip() -> Result<()> {
        let f = Fixture::new()?;
        let before = f
            .sessions
            .iter()
            .map(|s| Ok((s.file_path.clone(), fs::read(&s.file_path)?)))
            .collect::<Result<Vec<_>>>()?;
        super::super::FAIL_AFTER_RETIREMENT.with(|flag| flag.set(true));
        let result = f.run();
        super::super::FAIL_AFTER_RETIREMENT.with(|flag| flag.set(false));
        assert!(result.unwrap_err().to_string().contains("rolled back"));
        for (path, bytes) in before {
            assert_eq!(fs::read(path)?, bytes);
        }
        let extracted = f.temp.path().join("extracted");
        zip::ZipArchive::new(File::open(f.temp.path().join("sessions.zip"))?)?
            .extract(&extracted)?;
        assert_eq!(
            TrashStore::new(TrashPaths::from_data_root(extracted))
                .snapshot()?
                .len(),
            2
        );
        assert!(trash::children(&f.temp.path().join("data/operations"))?.is_empty());
        Ok(())
    }
}
