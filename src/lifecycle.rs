//! Physical history dependencies, independent of search visibility and supersession.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::live::LiveSessionTracker;
use crate::parse::{parse_session_file, Agent};
use crate::scan::SessionRoots;
use crate::trash::{TrashEntry, TrashReason};

mod file_identity;
mod transaction;
pub use transaction::{execute, recover};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyPolicy {
    #[default]
    Skip,
    Include,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Trash,
    Delete,
    Restore,
}

impl Operation {
    pub fn label(self) -> &'static str {
        match self {
            Self::Trash => "Trash",
            Self::Delete => "Permanently delete",
            Self::Restore => "Restore",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub path: PathBuf,
    pub original_path: PathBuf,
    pub agent: Agent,
    pub thread_id: String,
    pub rollout_id: String,
    pub history_base: Option<String>,
    pub fork_parent: Option<String>,
    pub trashed: bool,
    pub archived: bool,
    pub size: u64,
    pub modified_ns: u128,
}

impl Member {
    pub fn search_hit(&self) -> crate::index::SearchHit {
        use crate::index::StoredSession;
        let parsed = parse_session_file(self.agent, &self.path).ok().flatten();
        let mut session =
            parsed
                .as_ref()
                .map(StoredSession::from)
                .unwrap_or_else(|| StoredSession {
                    session_id: self.thread_id.clone(),
                    agent: self.agent,
                    project: String::new(),
                    branch: None,
                    cwd: None,
                    modified_ts: (self.modified_ns / 1_000_000_000) as u64,
                    lines: 0,
                    file_path: self.path.clone(),
                    first_msg_role: None,
                    first_msg_content: String::new(),
                    last_msg_role: None,
                    last_msg_content: String::new(),
                    first_user_msg_content: String::new(),
                    derivation_type: crate::parse::DerivationType::Original,
                    is_sidechain: false,
                    custom_title: None,
                    session_info: None,
                    trashed: self.trashed,
                    original_path: Some(self.original_path.clone()),
                    superseded_by: None,
                });
        session.trashed = self.trashed;
        session.original_path = self.trashed.then(|| self.original_path.clone());
        crate::index::SearchHit {
            session,
            snippet_html: String::new(),
            score: 0.0,
            is_live: false,
        }
    }
    pub(crate) fn same_thread(&self, other: &Self) -> bool {
        self.agent == Agent::Codex && other.agent == self.agent && self.thread_id == other.thread_id
    }
    pub(crate) fn requires(&self, other: &Self) -> bool {
        self.agent == Agent::Codex
            && other.agent == self.agent
            && self.history_base.as_deref() == Some(other.rollout_id.as_str())
    }
    pub fn relation(&self, other: &Self) -> bool {
        self.same_thread(other)
            || self.requires(other)
            || other.requires(self)
            || (self.agent == other.agent
                && (self.fork_parent.as_deref() == Some(&other.thread_id)
                    || other.fork_parent.as_deref() == Some(&self.thread_id)))
    }
}

#[derive(Debug, Clone, Default)]
pub struct DependencyGraph {
    pub members: BTreeMap<PathBuf, Member>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecyclePlan {
    pub operation: Operation,
    pub seeds: BTreeSet<PathBuf>,
    pub members: BTreeMap<PathBuf, Member>,
    pub reasons: BTreeMap<PathBuf, String>,
    pub blockers: Vec<String>,
}

impl LifecyclePlan {
    pub fn session_count(&self) -> usize {
        self.members
            .values()
            .map(|m| (m.agent, &m.thread_id))
            .collect::<std::collections::HashSet<_>>()
            .len()
    }
    pub fn expanded(&self) -> bool {
        self.members.keys().any(|p| !self.seeds.contains(p))
    }
    pub fn require_safe(&self) -> Result<()> {
        if !self.blockers.is_empty() {
            bail!("{}", self.blockers.join("\n"));
        }
        if self.members.is_empty() {
            bail!("no sessions selected");
        }
        Ok(())
    }
}

/// Open plain or compressed Codex history without materializing or altering it.
pub fn open_rollout(path: &Path) -> Result<Box<dyn BufRead>> {
    let file = File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    if path.extension().is_some_and(|e| e == "zst") {
        Ok(Box::new(BufReader::new(zstd::stream::read::Decoder::new(
            file,
        )?)))
    } else {
        Ok(Box::new(BufReader::new(file)))
    }
}

pub(crate) fn rollout_id(path: &Path, thread: &str) -> String {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let stem = name.trim_end_matches(".zst").trim_end_matches(".jsonl");
    stem.split_once(&format!("{thread}_"))
        .map(|(_, id)| id.to_owned())
        .unwrap_or_else(|| thread.to_owned())
}

// Resolve existing ancestors too, so a trashed path under /private/var still belongs
// to a configured /var home after its original parent directory was removed.
fn resolved_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => resolved_path(parent).join(name),
        _ => path.to_path_buf(),
    }
}

fn walk(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("cannot inspect {}", path.display())),
    };
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            bail!("cannot verify symlink {}", entry.path().display());
        }
        if kind.is_dir() {
            walk(&entry.path(), out)?;
        } else if entry.file_name().to_string_lossy().ends_with(".jsonl")
            || entry.file_name().to_string_lossy().ends_with(".jsonl.zst")
        {
            out.push(entry.path());
        }
    }
    Ok(())
}

fn metadata(
    path: &Path,
    original: &Path,
    agent: Agent,
    trashed: bool,
    archived: bool,
) -> Result<Member> {
    let stat = fs::symlink_metadata(path)?;
    if !stat.is_file() {
        bail!("not a regular session file: {}", path.display());
    }
    let (thread_id, history_base, fork_parent) = if agent == Agent::Codex {
        let mut found = None;
        for (line_number, line) in open_rollout(path)?.lines().enumerate() {
            let line = line?;
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            // Early Codex rollouts wrote the session header without an envelope.
            let legacy_header = line_number == 0
                && value.get("type").is_none()
                && value.get("timestamp").is_some()
                && value.get("id").is_some();
            if value["type"] == "session_meta" || legacy_header {
                let p = if legacy_header {
                    &value
                } else {
                    &value["payload"]
                };
                let id = p["id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .context("session metadata has no thread ID")?;
                let base = p
                    .get("history_base")
                    .filter(|v| !v.is_null())
                    .map(|v| {
                        v["thread_id"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                            .context("invalid history_base")
                    })
                    .transpose()?;
                found = Some((
                    id.to_owned(),
                    base,
                    p["forked_from_id"].as_str().map(str::to_owned),
                ));
                break;
            }
        }
        found.context("session metadata is missing")?
    } else if agent == Agent::Antigravity {
        (
            crate::trash::AntigravityBundlePaths::from_transcript(original)?.session_id,
            None,
            None,
        )
    } else {
        let session = parse_session_file(agent, path)?.context("session metadata is missing")?;
        (
            session.session_id,
            None,
            session.lineage.forked_from_session_id,
        )
    };
    let rollout_id = if agent == Agent::Codex {
        rollout_id(original, &thread_id)
    } else {
        thread_id.clone()
    };
    Ok(Member {
        path: path.into(),
        original_path: original.into(),
        agent,
        thread_id,
        rollout_id,
        history_base,
        fork_parent,
        trashed,
        archived,
        size: stat.len(),
        modified_ns: stat
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos(),
    })
}

impl DependencyGraph {
    /// Only the selected provider is inspected. Trash from another home cannot join its graph.
    pub fn discover(roots: &SessionRoots, agent: Agent, seeds: &[PathBuf]) -> Result<Self> {
        let mut graph = Self::default();
        let root = match agent {
            Agent::Codex => &roots.codex_sessions,
            Agent::Claude => &roots.claude_projects,
            Agent::Antigravity => &roots.antigravity_home,
        };
        let archive = roots
            .codex_sessions
            .parent()
            .map(|p| p.join("archived_sessions"));
        let mut files = Vec::new();
        if agent != Agent::Antigravity {
            if let Err(e) = walk(root, &mut files) {
                graph.errors.push(format!("{e:#}"));
            }
            if agent == Agent::Codex {
                if let Some(p) = &archive {
                    if let Err(e) = walk(p, &mut files) {
                        graph.errors.push(format!("{e:#}"));
                    }
                }
            }
        }
        for path in files {
            let archived = archive.as_ref().is_some_and(|p| path.starts_with(p));
            graph.add(&path, &path, agent, false, archived);
        }
        if let Some(trash) = &roots.trash {
            match File::open(&trash.metadata_file) {
                Ok(file) => {
                    for line in BufReader::new(file).lines() {
                        let entry: TrashEntry = serde_json::from_str(&line?)?;
                        if entry.agent() != Some(agent) {
                            continue;
                        }
                        let original = PathBuf::from(&entry.op);
                        if !resolved_path(&original).starts_with(resolved_path(root))
                            && !(agent == Agent::Codex
                                && archive.as_ref().is_some_and(|p| {
                                    resolved_path(&original).starts_with(resolved_path(p))
                                }))
                        {
                            continue;
                        }
                        let mut path = entry.trash_path(trash);
                        if agent == Agent::Antigravity {
                            let bundle =
                                crate::trash::AntigravityBundlePaths::from_transcript(&original)?;
                            path = crate::trash::AntigravityBundlePaths::in_archive(
                                &path,
                                &bundle.session_id,
                            )
                            .transcript;
                        }
                        if path.try_exists()? {
                            graph.add(&path, &original, agent, true, false);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => graph
                    .errors
                    .push(format!("cannot inspect trash metadata: {e}")),
            }
        }
        for path in seeds {
            if !graph.members.contains_key(path) {
                if path.starts_with(root) {
                    graph.add(path, path, agent, false, false);
                } else {
                    graph.errors.push(format!(
                        "cannot resolve selected session {} in this provider home",
                        path.display()
                    ));
                }
            }
        }
        // Two representations of the same physical path are supported; distinct files claiming
        // the same immutable ID are ambiguous and must not silently become interchangeable.
        let mut identities = BTreeMap::new();
        for m in graph.members.values().filter(|m| m.agent == Agent::Codex) {
            let path = resolved_path(&m.original_path)
                .to_string_lossy()
                .trim_end_matches(".zst")
                .to_owned();
            if let Some(previous) = identities.insert(m.rollout_id.clone(), path.clone()) {
                if previous != path {
                    graph
                        .errors
                        .push(format!("ambiguous rollout ID {}", m.rollout_id));
                }
            }
        }
        graph.errors.sort();
        graph.errors.dedup();
        Ok(graph)
    }

    fn add(&mut self, path: &Path, original: &Path, agent: Agent, trashed: bool, archived: bool) {
        match metadata(path, original, agent, trashed, archived) {
            Ok(member) => {
                self.members.insert(path.into(), member);
            }
            Err(e) => self
                .errors
                .push(format!("cannot verify {}: {e:#}", path.display())),
        }
    }

    pub fn related(&self, seeds: &BTreeSet<PathBuf>) -> Vec<Member> {
        let mut included = seeds.clone();
        loop {
            let before = included.len();
            for m in self.members.values() {
                if included
                    .iter()
                    .filter_map(|p| self.members.get(p))
                    .any(|other| m.relation(other))
                {
                    included.insert(m.path.clone());
                }
            }
            if included.len() == before {
                break;
            }
        }
        included
            .iter()
            .filter_map(|p| self.members.get(p).cloned())
            .collect()
    }

    pub fn plan(&self, seeds: BTreeSet<PathBuf>, operation: Operation) -> LifecyclePlan {
        let mut plan = LifecyclePlan {
            operation,
            seeds: seeds.clone(),
            members: BTreeMap::new(),
            reasons: BTreeMap::new(),
            blockers: self.errors.clone(),
        };
        let purge = operation == Operation::Delete
            && seeds
                .iter()
                .all(|p| self.members.get(p).is_some_and(|m| m.trashed));
        for path in &seeds {
            match self.members.get(path) {
                Some(m)
                    if (operation == Operation::Restore) == m.trashed
                        || operation == Operation::Delete =>
                {
                    plan.members.insert(path.clone(), m.clone());
                    plan.reasons.insert(path.clone(), "selected".into());
                }
                Some(_) => plan.blockers.push(format!(
                    "{} is not eligible for {}",
                    path.display(),
                    operation.label()
                )),
                None => plan
                    .blockers
                    .push(format!("selected file is unavailable: {}", path.display())),
            }
        }
        // Already-trashed intermediates still connect active descendants to
        // removed history, even though Trash must not move those intermediates again.
        let mut context = plan.members.clone();
        loop {
            let before = context.len();
            for m in self.members.values() {
                if context.contains_key(&m.path) {
                    continue;
                }
                let eligible = match operation {
                    Operation::Trash => !m.trashed,
                    Operation::Restore => m.trashed,
                    Operation::Delete => true,
                };
                if !eligible && operation != Operation::Trash {
                    continue;
                }
                let reason = context.values().find_map(|selected| {
                    if m.same_thread(selected) {
                        Some(format!("same thread as {}", selected.path.display()))
                    } else if operation == Operation::Restore && selected.requires(m) {
                        // A usable active representation already satisfies the requirement.
                        if self
                            .members
                            .values()
                            .any(|a| !a.trashed && selected.requires(a))
                        {
                            None
                        } else {
                            Some(format!("history required by {}", selected.path.display()))
                        }
                    } else if operation != Operation::Restore && m.requires(selected) {
                        Some(format!("depends on {}", selected.path.display()))
                    } else {
                        None
                    }
                });
                if let Some(reason) = reason {
                    context.insert(m.path.clone(), m.clone());
                    if !eligible {
                        continue;
                    }
                    if purge && !m.trashed {
                        plan.blockers.push(format!(
                            "active dependent {} must be trashed first",
                            m.path.display()
                        ));
                    }
                    plan.members.insert(m.path.clone(), m.clone());
                    plan.reasons.insert(m.path.clone(), reason);
                }
            }
            if context.len() == before {
                break;
            }
        }
        if operation == Operation::Restore {
            // Follow active ancestors as well: an existing but broken ancestor is not sufficient.
            let mut required = plan.members.values().cloned().collect::<Vec<_>>();
            let mut visited = BTreeSet::new();
            while let Some(m) = required.pop() {
                if !visited.insert(m.path.clone()) {
                    continue;
                }
                if let Some(base) = &m.history_base {
                    let parents = self
                        .members
                        .values()
                        .filter(|p| m.requires(p))
                        .collect::<Vec<_>>();
                    if parents.is_empty() {
                        plan.blockers.push(format!(
                            "missing required history {base} for {}",
                            m.path.display()
                        ));
                    }
                    for parent in parents {
                        required.push(parent.clone());
                        if parent.trashed
                            && !self.members.values().any(|a| !a.trashed && m.requires(a))
                            && !plan.members.contains_key(&parent.path)
                        {
                            // Replan to include the ancestor's same-thread unit and its ancestry.
                            let mut more = seeds.clone();
                            more.insert(parent.path.clone());
                            let mut expanded = self.plan(more, operation);
                            expanded.seeds = seeds.clone();
                            expanded.reasons.insert(
                                parent.path.clone(),
                                format!("history required by {}", m.path.display()),
                            );
                            return expanded;
                        }
                    }
                }
            }
            for m in plan.members.values() {
                match m.original_path.try_exists() {
                    Ok(false) => {}
                    Ok(true) => plan.blockers.push(format!(
                        "restore destination already exists: {}",
                        m.original_path.display()
                    )),
                    Err(e) => plan
                        .blockers
                        .push(format!("cannot inspect restore destination: {e}")),
                }
            }
        }
        plan.blockers.sort();
        plan.blockers.dedup();
        plan
    }

    pub fn add_live_blockers(plan: &mut LifecyclePlan, roots: &SessionRoots) {
        let live = LiveSessionTracker::new(roots.live_sessions.clone()).snapshot();
        for m in plan.members.values() {
            if let Some(reason) = live.protection_reason(m.agent, &m.thread_id) {
                plan.blockers
                    .push(format!("{}: {}", m.path.display(), reason.message()));
            }
        }
    }
}

/// Reasons survive group expansion; every member records the originating request(s).
pub fn group_reason(reason: &TrashReason, plan: &LifecyclePlan, path: &Path) -> TrashReason {
    if plan.members.len() == 1 {
        return reason.clone();
    }
    TrashReason::DependencyGroup {
        triggers: vec![reason.clone()],
        selected_paths: plan.seeds.iter().map(|p| p.display().to_string()).collect(),
        relation: plan.reasons.get(path).cloned().unwrap_or_default(),
    }
}
