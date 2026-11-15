use std::io::{IsTerminal, Seek};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::live::LiveSessionSnapshot;

use super::*;

const SCHEMA: &[u8] = include_bytes!("../../../docs/schemas/trash-item-v1.schema.json");

#[cfg(test)]
thread_local! {
    static AFTER_COPY: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[derive(Debug, Default)]
pub struct TrashArchiveReport {
    /// Number of physical trash entries included in the ZIP.
    pub items: usize,
    /// Number of entries retained because removal could not be verified as safe.
    pub skipped: usize,
}

/// Export the current trash store or archive it and retire the verified items.
pub fn archive_trash(
    roots: &SessionRoots,
    destination: &Path,
    purge: bool,
    yes: bool,
) -> Result<TrashArchiveReport> {
    let paths = roots.trash.as_ref().context("trash store is unavailable")?;
    let destination = destination_path(paths, destination)?;
    let mut overwrite = if purge {
        None
    } else {
        Some(confirm_destination(&destination, yes)?)
    };
    let (_lock, operations) = lock(roots)?;
    recover_locked(roots, &operations)?;
    migrate_locked(paths, &operations)?;
    let items = TrashStore::new(paths.clone()).snapshot()?;
    let mut remaining = items.iter().collect::<Vec<_>>();
    let tracker = LiveSessionTracker::new(roots.live_sessions.clone());
    loop {
        let plans = if purge {
            purge_plans(roots, &mut remaining, &tracker.snapshot())?
        } else {
            Vec::new()
        };
        let report = TrashArchiveReport {
            items: remaining.len(),
            skipped: items.len() - remaining.len(),
        };
        if purge && remaining.is_empty() {
            return Ok(report);
        }
        let mut held = Vec::new();
        for plan in &plans {
            let agent = plan
                .members
                .values()
                .next()
                .context("empty lifecycle group")?
                .agent;
            let ids = plan
                .members
                .values()
                .map(|m| m.thread_id.clone())
                .collect::<Vec<_>>();
            let (guard, skipped) = tracker.acquire_available_group_guard(agent, &ids);
            held.push(guard);
            remaining.retain(|item| {
                let member = item
                    .primary
                    .as_ref()
                    .and_then(|path| plan.members.get(path));
                !member.is_some_and(|member| skipped.iter().any(|(id, _)| *id == member.thread_id))
            });
        }
        if remaining.len() != report.items {
            continue;
        }
        if overwrite.is_none() {
            overwrite = Some(confirm_destination(&destination, yes)?);
        }
        let snapshot = remaining
            .iter()
            .map(|item| Ok((item.path.clone(), Stamp::read(&item.path)?)))
            .collect::<Result<Vec<_>>>()?;
        let scratch = scratch_directory(parent(&destination)?)?;
        let staged = scratch.path().join("trash.zip");
        let copied = write_zip(paths, &snapshot, &staged);
        #[cfg(test)]
        AFTER_COPY.with(|hook| {
            let callback = hook.borrow_mut().take();
            if let Some(callback) = callback {
                callback();
            }
        });
        if purge {
            let fresh = purge_plans(roots, &mut remaining, &tracker.snapshot_with_guards(&held))?;
            // A newly protected session must not appear in the published ZIP.
            if remaining.len() != report.items {
                continue;
            }
            ensure!(
                fresh == plans,
                "session group changed while archiving trash"
            );
        }
        copied?;
        let stamp = Stamp::read(&staged)?;
        for (path, before) in &snapshot {
            before.verify(path)?;
        }

        publish_zip(
            &staged,
            &destination,
            overwrite.as_ref().and_then(Option::as_ref),
            &stamp,
        )?;

        if purge {
            // Only retire the exact item directories that the verified ZIP contains.
            // The ZIP remains available if revalidation or removal fails.
            let live = tracker.snapshot_with_guards(&held);
            for plan in &plans {
                let mut protected = plan.clone();
                DependencyGraph::add_live_blockers_from(&mut protected, &live);
                protected.require_safe()?;
                recheck(roots, plan)?;
            }
            let mut staging = Staging::new(plans);
            staging.journal.archive = Some(VerifiedArchive {
                path: destination,
                stamp,
            });
            for (path, before) in snapshot {
                staging.retire(path, before)?;
            }
            staging.execute(&operations)?;
        }
        return Ok(report);
    }
}

pub(super) fn destination_path(paths: &trash::TrashPaths, destination: &Path) -> Result<PathBuf> {
    let destination = std::path::absolute(destination)?;
    let destination = super::super::resolved_path(parent(&destination)?).join(
        destination
            .file_name()
            .context("ZIP path has no filename")?,
    );
    ensure!(
        !destination.starts_with(super::super::resolved_path(&paths.trash_dir))
            && !destination.starts_with(super::super::resolved_path(
                &paths.data_root.join("operations")
            ))
            && destination != super::super::resolved_path(&paths.data_root.join("lifecycle.lock"))
            && destination != super::super::resolved_path(&paths.metadata_file)
            && destination != super::super::resolved_path(&paths.reasons_file),
        "ZIP destination overlaps the trash store: {}",
        destination.display()
    );
    Ok(destination)
}

pub(super) fn publish_zip(
    staged: &Path,
    destination: &Path,
    overwrite: Option<&Destination>,
    stamp: &Stamp,
) -> Result<()> {
    if let Some(before) = overwrite {
        ensure!(
            &Destination::read(destination)? == before,
            "ZIP destination changed: {}",
            destination.display()
        );
        fs::rename(staged, destination)?;
        sync_directory(parent(destination)?)?;
        sync_directory(parent(staged)?)?;
    } else {
        move_exclusive(staged, destination)?;
    }
    stamp.verify(destination)
}

#[derive(PartialEq, Eq)]
pub(super) struct Destination {
    identity: FileIdentity,
    length: u64,
    modified: std::time::SystemTime,
}

impl Destination {
    fn read(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        ensure!(
            metadata.is_file(),
            "ZIP destination is not a regular file: {}",
            path.display()
        );
        Ok(Self {
            identity: entry_identity(path)?,
            length: metadata.len(),
            modified: metadata.modified()?,
        })
    }
}

pub(super) fn confirm_destination(path: &Path, yes: bool) -> Result<Option<Destination>> {
    if !exists(path)? {
        return Ok(None);
    }
    let existing = Destination::read(path)?;
    if !yes {
        ensure!(
            io::stdin().is_terminal(),
            "ZIP destination exists: {}; pass --yes to overwrite",
            path.display()
        );
        eprint!("Overwrite {}? [y/N] ", path.display());
        io::stderr().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        ensure!(
            matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
            "overwrite declined"
        );
    }
    Ok(Some(existing))
}

fn purge_plans(
    roots: &SessionRoots,
    items: &mut Vec<&trash::TrashItem>,
    live: &LiveSessionSnapshot,
) -> Result<Vec<LifecyclePlan>> {
    items.retain(|item| {
        item.primary.is_some() && item.original_path().is_some() && item.metadata.agent.is_some()
    });
    let mut plans = Vec::new();
    for agent in [Agent::Claude, Agent::Codex, Agent::Antigravity] {
        let seeds = items
            .iter()
            .filter(|item| item.metadata.agent == Some(agent))
            .filter_map(|item| item.primary.clone())
            .collect::<BTreeSet<_>>();
        if seeds.is_empty() {
            continue;
        }
        let seed_paths = seeds.iter().cloned().collect::<Vec<_>>();
        let graph = DependencyGraph::discover(roots, agent, &seed_paths)?;
        let mut eligible = seeds;
        if !graph.errors.is_empty() {
            eligible.clear();
        }
        // Removing one candidate may make its required history ineligible too.
        // Keep pruning until every removal closure fits inside the archived set.
        loop {
            let before = eligible.len();
            let allowed = eligible.clone();
            eligible.retain(|path| {
                let mut plan = graph.plan([path.clone()].into(), Operation::Delete);
                DependencyGraph::add_live_blockers_from(&mut plan, live);
                plan.require_safe().is_ok()
                    && plan
                        .members
                        .values()
                        .all(|m| m.trashed && allowed.contains(&m.path))
            });
            if eligible.len() == before {
                break;
            }
        }
        items.retain(|item| {
            item.metadata.agent != Some(agent)
                || item.primary.as_ref().is_some_and(|p| eligible.contains(p))
        });
        if eligible.is_empty() {
            continue;
        }
        let plan = graph.plan(eligible.clone(), Operation::Delete);
        plan.require_safe()?;
        ensure!(
            plan.members.keys().eq(eligible.iter()) && plan.members.values().all(|m| m.trashed),
            "archive removal must include only the archived trash items"
        );
        plans.push(plan);
    }
    Ok(plans)
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ZipEntry {
    name: String,
    content: Option<(u64, [u8; 32])>,
}

fn write_zip(
    paths: &trash::TrashPaths,
    snapshot: &[(PathBuf, Stamp)],
    staged: &Path,
) -> Result<()> {
    write_verified_zip(staged, |writer, entries| {
        for (path, before) in snapshot {
            ensure!(
                add_tree(writer, entries, &paths.trash_dir, path)? == before.digest,
                "trash item changed while archiving {}",
                path.display()
            );
        }
        let legacy = paths.trash_dir.join(trash::LEGACY_METADATA_DIR);
        if exists(&legacy)? {
            add_tree(writer, entries, &paths.trash_dir, &legacy)?;
        }
        Ok(())
    })
}

pub(super) fn write_verified_zip(
    staged: &Path,
    contents: impl FnOnce(&mut ZipWriter<File>, &mut Vec<ZipEntry>) -> Result<()>,
) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(staged)?;
    let mut writer = ZipWriter::new(file);
    let mut entries = Vec::new();
    add_directory(&mut writer, &mut entries, "trash/".to_owned())?;
    add_directory(&mut writer, &mut entries, "trash/.schemas/".to_owned())?;
    add_file(
        &mut writer,
        &mut entries,
        "trash/.schemas/trash-item-v1.schema.json".to_owned(),
        SCHEMA.len() as u64,
        SCHEMA,
    )?;
    contents(&mut writer, &mut entries)?;
    writer.finish()?.sync_all()?;
    sync_directory(parent(staged)?)?;

    let mut archive = ZipArchive::new(File::open(staged)?)?;
    ensure!(
        archive.len() == entries.len(),
        "ZIP entry count differs from the snapshot"
    );
    for (i, expected) in entries.into_iter().enumerate() {
        let mut entry = archive.by_index(i)?;
        ensure!(
            entry.name()? == expected.name,
            "ZIP member differs from {}",
            expected.name
        );
        let content = if entry.is_dir() {
            None
        } else {
            Some(copy_hash(&mut entry, &mut io::sink())?)
        };
        ensure!(
            content == expected.content,
            "ZIP verification failed for {}",
            expected.name
        );
    }
    Ok(())
}

fn add_tree<W: Write + Seek>(
    writer: &mut ZipWriter<W>,
    entries: &mut Vec<ZipEntry>,
    root: &Path,
    path: &Path,
) -> Result<[u8; 32]> {
    add_tree_as(
        writer,
        entries,
        path,
        zip_name("trash", path.strip_prefix(root)?)?,
    )
}

pub(super) fn zip_name(prefix: &str, relative: &Path) -> Result<String> {
    let mut name = prefix.to_owned();
    for component in relative.components() {
        let part = component
            .as_os_str()
            .to_str()
            .context("non-UTF-8 ZIP entry name")?;
        ensure!(
            matches!(component, std::path::Component::Normal(_))
                && !part.contains(['\\', ':', '\0']),
            "unsafe ZIP entry name: {}",
            relative.display()
        );
        name.push('/');
        name.push_str(part);
    }
    Ok(name)
}

pub(super) fn add_tree_as<W: Write + Seek>(
    writer: &mut ZipWriter<W>,
    entries: &mut Vec<ZipEntry>,
    path: &Path,
    mut name: String,
) -> Result<[u8; 32]> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        name.push('/');
        add_directory(writer, entries, name.clone())?;
        let children = trash::children(path)?;
        let mut digest = directory_digest(children.len());
        for child in children {
            hash_child(
                &mut digest,
                &child,
                add_tree_as(
                    writer,
                    entries,
                    &child,
                    zip_name(
                        name.trim_end_matches('/'),
                        Path::new(child.file_name().context("missing ZIP filename")?),
                    )?,
                )?,
            )?;
        }
        Ok(digest.finalize().into())
    } else if metadata.is_file() {
        let (size, bytes) = add_file(writer, entries, name, metadata.len(), File::open(path)?)?;
        Ok(file_digest(size, bytes))
    } else {
        bail!("unsupported ZIP source {}", path.display());
    }
}

pub(super) fn add_directory<W: Write + Seek>(
    writer: &mut ZipWriter<W>,
    entries: &mut Vec<ZipEntry>,
    name: String,
) -> Result<()> {
    writer.add_directory(&name, SimpleFileOptions::default().unix_permissions(0o700))?;
    entries.push(ZipEntry {
        name,
        content: None,
    });
    Ok(())
}

pub(super) fn add_file<W: Write + Seek>(
    writer: &mut ZipWriter<W>,
    entries: &mut Vec<ZipEntry>,
    name: String,
    size: u64,
    mut input: impl Read,
) -> Result<(u64, [u8; 32])> {
    let method = if name.ends_with(".zst") {
        CompressionMethod::Stored
    } else {
        CompressionMethod::Deflated
    };
    writer.start_file(
        &name,
        SimpleFileOptions::default()
            .compression_method(method)
            .unix_permissions(0o600)
            .large_file(size >= u64::from(u32::MAX) - 65536),
    )?;
    let content = copy_hash(&mut input, writer)?;
    ensure!(
        content.0 == size,
        "file size changed while archiving {name}"
    );
    entries.push(ZipEntry {
        name,
        content: Some(content),
    });
    Ok(content)
}

#[cfg(test)]
mod tests {
    use crate::live::LiveSessionPaths;

    use super::*;

    struct Fixture {
        temp: TempDir,
        roots: SessionRoots,
        items: Vec<trash::TrashItem>,
    }

    impl Fixture {
        fn new(agent: Agent) -> Result<Self> {
            let temp = TempDir::new()?;
            let roots = SessionRoots {
                live_sessions: LiveSessionPaths {
                    claude_sessions_dir: Some(temp.path().join("presence")),
                    ..Default::default()
                },
                claude_projects: temp.path().join("claude"),
                codex_sessions: temp.path().join("codex/sessions"),
                antigravity_home: temp.path().join("antigravity"),
                trash: Some(trash::TrashPaths::from_data_root(temp.path().join("data"))),
            };
            let root = if agent == Agent::Claude {
                &roots.claude_projects
            } else {
                &roots.codex_sessions
            };
            fs::create_dir_all(root)?;
            for id in ["one", "two"] {
                let path = root.join(format!("{id}.jsonl"));
                let entry = if agent == Agent::Claude {
                    serde_json::json!({"type":"user","sessionId":id,"message":{"role":"user","content":"fixture"}})
                } else {
                    serde_json::json!({"type":"session_meta","payload":{"id":id}})
                };
                fs::write(&path, serde_json::to_vec(&entry)?)?;
                let plan = DependencyGraph::discover(&roots, agent, std::slice::from_ref(&path))?
                    .plan([path.clone()].into(), Operation::Trash);
                execute(
                    &roots,
                    &plan,
                    &BTreeMap::from([(path, TrashReason::KeyBinding { key: "^T".into() })]),
                )?;
            }
            let items = TrashStore::new(roots.trash.clone().unwrap()).snapshot()?;
            Ok(Self { temp, roots, items })
        }

        fn run(&self) -> Result<TrashArchiveReport> {
            archive_trash(
                &self.roots,
                &self.temp.path().join("trash.zip"),
                true,
                false,
            )
        }

        fn assert_one_archived(&self) -> Result<()> {
            let extracted = self.temp.path().join("extracted");
            ZipArchive::new(File::open(self.temp.path().join("trash.zip"))?)?
                .extract(&extracted)?;
            let items = TrashStore::new(trash::TrashPaths::from_data_root(extracted)).snapshot()?;
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].path.file_name(), self.items[1].path.file_name());
            assert!(self.items[0].path.is_dir() && !self.items[1].path.exists());
            Ok(())
        }
    }

    #[test]
    fn newly_live_trash_is_excluded_from_rebuilt_zip() -> Result<()> {
        let f = Fixture::new(Agent::Claude)?;
        let marker = f.temp.path().join("presence/live.json");
        let primary = f.items[0].primary.clone().unwrap();
        let id = crate::parse::parse_session_file(Agent::Claude, &primary)?
            .unwrap()
            .session_id;
        AFTER_COPY.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                fs::create_dir_all(marker.parent().unwrap()).unwrap();
                fs::write(
                    marker,
                    format!("{{\"sessionId\":\"{id}\",\"pid\":{}}}", std::process::id()),
                )
                .unwrap();
                OpenOptions::new()
                    .append(true)
                    .open(primary)
                    .unwrap()
                    .write_all(b"\nnew live data")
                    .unwrap();
            }))
        });
        let report = f.run()?;
        assert_eq!((report.items, report.skipped), (1, 1));
        f.assert_one_archived()
    }

    #[test]
    fn new_active_dependent_is_excluded_before_publication() -> Result<()> {
        let f = Fixture::new(Agent::Codex)?;
        let id = f.items[0]
            .original_path()
            .unwrap()
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let child = f.roots.codex_sessions.join("child.jsonl");
        let contents = serde_json::to_vec(
            &serde_json::json!({"type":"session_meta","payload":{"id":"child","history_base":{"thread_id":id}}}),
        )?;
        AFTER_COPY.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || fs::write(child, contents).unwrap()))
        });
        let report = f.run()?;
        assert_eq!((report.items, report.skipped), (1, 1));
        f.assert_one_archived()
    }

    #[test]
    fn changed_trash_payload_aborts_without_publication_or_removal() -> Result<()> {
        let f = Fixture::new(Agent::Claude)?;
        let metadata = f.items[0].path.join("metadata.json");
        AFTER_COPY.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                OpenOptions::new()
                    .append(true)
                    .open(metadata)
                    .unwrap()
                    .write_all(b"\n ")
                    .unwrap();
            }))
        });
        assert!(f.run().unwrap_err().to_string().contains("entry changed"));
        assert!(!f.temp.path().join("trash.zip").exists());
        assert!(f.items.iter().all(|item| item.path.is_dir()));
        Ok(())
    }
}
