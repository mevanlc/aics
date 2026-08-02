use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use directories::BaseDirs;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tantivy::directory::error::LockError;
use tantivy::{Index, TantivyDocument, TantivyError, Term};

use crate::index::reader::SearchEngine;
use crate::index::schema::IndexSchema;
use crate::live::LiveSessionTracker;
use crate::parse::{
    parse_session_file, Agent, DerivationType, MessageRole, Session, SessionInfo, SessionLineage,
};
use crate::scan::{
    default_session_roots, scan_session_files_with_progress, ResolvedPaths, SessionFile,
    SessionRoots,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSession {
    pub session_id: String,
    pub agent: Agent,
    pub project: String,
    pub branch: Option<String>,
    pub cwd: Option<String>,
    pub modified_ts: u64,
    pub lines: usize,
    pub file_path: PathBuf,
    pub first_msg_role: Option<MessageRole>,
    pub first_msg_content: String,
    pub last_msg_role: Option<MessageRole>,
    pub last_msg_content: String,
    pub first_user_msg_content: String,
    pub derivation_type: DerivationType,
    pub is_sidechain: bool,
    pub custom_title: Option<String>,
    /// Subset of SessionInfo promoted to the index for cheap list rendering.
    /// Older indexes deserialize this as `None` via `#[serde(default)]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_info: Option<SessionInfo>,
    #[serde(default)]
    pub trashed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
}

impl StoredSession {
    pub fn has_resume_preview(&self) -> bool {
        !self.first_user_msg_content.trim().is_empty()
    }
}

impl From<&Session> for StoredSession {
    fn from(session: &Session) -> Self {
        Self {
            session_id: session.session_id.clone(),
            agent: session.agent,
            project: session.project.clone(),
            branch: session.branch.clone(),
            cwd: session.cwd.clone(),
            modified_ts: session.modified_ts,
            lines: session.lines,
            file_path: session.file_path.clone(),
            first_msg_role: session.first_msg_role,
            first_msg_content: session.first_msg_content.clone(),
            last_msg_role: session.last_msg_role,
            last_msg_content: session.last_msg_content.clone(),
            first_user_msg_content: session.first_user_msg_content.clone(),
            derivation_type: session.derivation_type,
            is_sidechain: session.is_sidechain,
            custom_title: session.custom_title.clone(),
            session_info: session.session_info.clone(),
            trashed: false,
            original_path: None,
            superseded_by: None,
        }
    }
}

impl StoredSession {
    fn from_session_file(
        session: &Session,
        file: &SessionFile,
        superseded_by: Option<String>,
    ) -> Self {
        let mut stored = Self::from(session);
        stored.trashed = file.trashed;
        stored.original_path = file.original_path.clone();
        stored.superseded_by = superseded_by;
        stored
    }
}

#[derive(Debug, Clone, Default)]
pub struct SyncStats {
    pub scanned: usize,
    pub updated: usize,
    pub skipped: usize,
    pub removed: usize,
}

#[derive(Debug, Clone)]
pub enum SyncOutcome {
    Completed(SyncStats),
    Busy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncProgress {
    Discovering { discovered: usize },
    IndexingStarted { total: usize },
    IndexingProgress { processed: usize, total: usize },
}

trait SyncProgressObserver {
    fn on_progress(&mut self, progress: SyncProgress);
}

struct NoopSyncProgress;

impl SyncProgressObserver for NoopSyncProgress {
    fn on_progress(&mut self, _progress: SyncProgress) {}
}

impl<F> SyncProgressObserver for F
where
    F: FnMut(SyncProgress),
{
    fn on_progress(&mut self, progress: SyncProgress) {
        self(progress);
    }
}

struct ScanToSyncProgress<'a, P> {
    progress: &'a mut P,
}

impl<P: SyncProgressObserver> crate::scan::ScanProgressObserver for ScanToSyncProgress<'_, P> {
    fn on_discovered(&mut self, discovered: usize) {
        self.progress
            .on_progress(SyncProgress::Discovering { discovered });
    }
}

#[derive(Debug, Clone)]
pub struct IndexPaths {
    pub cache_root: PathBuf,
    pub index_dir: PathBuf,
    pub state_file: PathBuf,
    pub profile_file: PathBuf,
    pub hashed_input_file: PathBuf,
}

impl IndexPaths {
    pub fn discover() -> Result<Self> {
        let roots = default_session_roots()?;
        Self::discover_for_roots(&roots)
    }

    pub fn discover_for_roots(roots: &SessionRoots) -> Result<Self> {
        if let Some(cache_root) = env_override("AICS_CACHE_ROOT") {
            return Ok(Self::from_cache_root_and_roots(cache_root, roots));
        }
        let base_dirs = BaseDirs::new().context("failed to locate home directory")?;
        Ok(Self::from_cache_root_and_roots(
            default_cache_dir(base_dirs.home_dir()),
            roots,
        ))
    }

    pub fn from_root(cache_root: impl Into<PathBuf>) -> Self {
        let cache_root = cache_root.into();
        Self {
            index_dir: cache_root.join("index"),
            state_file: cache_root.join("index_state.json"),
            profile_file: cache_root.join("profile.json"),
            hashed_input_file: cache_root.join("hashed-input.txt"),
            cache_root,
        }
    }

    pub fn from_cache_root_and_roots(cache_root: impl Into<PathBuf>, roots: &SessionRoots) -> Self {
        let cache_root = cache_root.into();
        let profile_root = cache_root
            .join("profiles")
            .join(profile_id_from_roots(roots));
        Self::from_root(profile_root)
    }
}

#[derive(Debug, Clone)]
pub struct IndexManager {
    paths: IndexPaths,
}

impl IndexManager {
    pub fn new() -> Result<Self> {
        Ok(Self {
            paths: IndexPaths::discover()?,
        })
    }

    pub fn with_paths(paths: IndexPaths) -> Self {
        Self { paths }
    }

    pub fn write_profile_metadata(&self, resolved: &ResolvedPaths) -> Result<()> {
        fs::create_dir_all(&self.paths.cache_root)
            .with_context(|| format!("failed to create {}", self.paths.cache_root.display()))?;

        let metadata = ProfileMetadata {
            version: 1,
            claude_home: resolved.homes.claude_home.clone(),
            codex_home: resolved.homes.codex_home.clone(),
            claude_projects: resolved.roots.claude_projects.clone(),
            claude_sessions: resolved.claude_sessions.clone(),
            codex_sessions: resolved.roots.codex_sessions.clone(),
        };
        let metadata_json = serde_json::to_string_pretty(&metadata)
            .context("failed to serialize profile metadata")?;
        fs::write(&self.paths.profile_file, metadata_json)
            .with_context(|| format!("failed to write {}", self.paths.profile_file.display()))?;
        fs::write(
            &self.paths.hashed_input_file,
            resolved.roots.profile_hash_input(),
        )
        .with_context(|| format!("failed to write {}", self.paths.hashed_input_file.display()))?;
        Ok(())
    }

    pub fn sync(&self, rebuild: bool) -> Result<SyncStats> {
        let roots = default_session_roots()?;
        self.sync_with_roots(&roots, rebuild)
    }

    pub fn delete_index(&self) -> Result<()> {
        if self.paths.index_dir.exists() {
            fs::remove_dir_all(&self.paths.index_dir)
                .with_context(|| format!("failed to remove {}", self.paths.index_dir.display()))?;
        }
        if self.paths.state_file.exists() {
            fs::remove_file(&self.paths.state_file)
                .with_context(|| format!("failed to remove {}", self.paths.state_file.display()))?;
        }
        Ok(())
    }

    pub fn sync_best_effort_with_progress<F>(
        &self,
        rebuild: bool,
        mut on_progress: F,
    ) -> Result<SyncOutcome>
    where
        F: FnMut(SyncProgress),
    {
        let roots = default_session_roots()?;
        self.sync_with_roots_best_effort_impl(&roots, rebuild, &mut on_progress)
    }

    pub fn sync_best_effort(&self, rebuild: bool) -> Result<SyncOutcome> {
        let roots = default_session_roots()?;
        self.sync_with_roots_best_effort(&roots, rebuild)
    }

    pub fn sync_with_roots_and_progress<F>(
        &self,
        roots: &SessionRoots,
        rebuild: bool,
        mut on_progress: F,
    ) -> Result<SyncStats>
    where
        F: FnMut(SyncProgress),
    {
        self.sync_with_roots_impl(roots, rebuild, &mut on_progress)
    }

    pub fn sync_with_roots_best_effort(
        &self,
        roots: &SessionRoots,
        rebuild: bool,
    ) -> Result<SyncOutcome> {
        let mut progress = NoopSyncProgress;
        self.sync_with_roots_best_effort_impl(roots, rebuild, &mut progress)
    }

    fn sync_with_roots_best_effort_impl<P: SyncProgressObserver>(
        &self,
        roots: &SessionRoots,
        rebuild: bool,
        progress: &mut P,
    ) -> Result<SyncOutcome> {
        match self.sync_with_roots_impl(roots, rebuild, progress) {
            Ok(stats) => Ok(SyncOutcome::Completed(stats)),
            Err(error) if !rebuild && is_lock_busy_error(&error) => {
                warn!(
                    "index sync skipped because another aics process holds the writer lock; \
                     using the current on-disk snapshot"
                );
                Ok(SyncOutcome::Busy)
            }
            Err(error) => Err(error),
        }
    }

    pub fn sync_with_roots(&self, roots: &SessionRoots, rebuild: bool) -> Result<SyncStats> {
        let mut progress = NoopSyncProgress;
        self.sync_with_roots_impl(roots, rebuild, &mut progress)
    }

    fn sync_with_roots_impl<P: SyncProgressObserver>(
        &self,
        roots: &SessionRoots,
        rebuild: bool,
        progress: &mut P,
    ) -> Result<SyncStats> {
        fs::create_dir_all(&self.paths.cache_root)
            .with_context(|| format!("failed to create {}", self.paths.cache_root.display()))?;

        if rebuild {
            if self.paths.index_dir.exists() {
                fs::remove_dir_all(&self.paths.index_dir).with_context(|| {
                    format!("failed to remove {}", self.paths.index_dir.display())
                })?;
            }
            if self.paths.state_file.exists() {
                fs::remove_file(&self.paths.state_file).with_context(|| {
                    format!("failed to remove {}", self.paths.state_file.display())
                })?;
            }
        }

        let (index, fields) = self.open_or_create_index()?;
        let previous_state = if rebuild {
            IndexState::default()
        } else {
            load_state(&self.paths.state_file)?
        };
        let scanned_files = {
            let mut scan_progress = ScanToSyncProgress { progress };
            scan_session_files_with_progress(roots, &mut scan_progress)?
        };
        let mut next_state = IndexState {
            format_version: INDEX_FORMAT_VERSION,
            files: BTreeMap::new(),
        };
        let mut stats = SyncStats {
            scanned: scanned_files.len(),
            ..SyncStats::default()
        };
        let mut writer = index.writer(50_000_000)?;
        let mut changed = rebuild;
        let mut pending_files = Vec::new();
        let mut scanned_by_key = HashMap::new();

        for file in &scanned_files {
            let key = normalize_path_key(&file.path);
            scanned_by_key.insert(key.clone(), file.clone());
            let fingerprint = FileFingerprint::from(file);

            if let Some(previous) = previous_state
                .files
                .get(&key)
                .filter(|state| state.fingerprint() == fingerprint)
            {
                next_state.files.insert(key, previous.clone());
                stats.skipped += 1;
                continue;
            }

            pending_files.push(file.clone());
        }

        progress.on_progress(SyncProgress::IndexingStarted {
            total: pending_files.len(),
        });

        let mut parsed_sessions = HashMap::<String, Session>::new();
        for (index, file) in pending_files.iter().enumerate() {
            let key = normalize_path_key(&file.path);
            let state = match parse_session_file(file.agent, &file.path) {
                Ok(Some(session)) => {
                    let state = IndexedFileState::from_session(file, &session);
                    parsed_sessions.insert(key.clone(), session);
                    stats.updated += 1;
                    state
                }
                Ok(None) => IndexedFileState::from_file(file, false),
                Err(error) => {
                    warn!("failed to parse {}: {error:#}", file.path.display());
                    IndexedFileState::from_file(file, false)
                }
            };
            next_state.files.insert(key, state);
            progress.on_progress(SyncProgress::IndexingProgress {
                processed: index + 1,
                total: pending_files.len(),
            });
        }

        apply_supersession(&mut next_state);

        for file in &pending_files {
            let key = normalize_path_key(&file.path);
            writer.delete_term(Term::from_field_text(fields.file_path, &key));
            changed = true;
            if let Some(session) = parsed_sessions.get(&key) {
                let superseded_by = next_state
                    .files
                    .get(&key)
                    .and_then(|state| state.superseded_by.clone());
                add_session_document(&mut writer, &fields, session, file, superseded_by)?;
            }
        }

        let relation_changed = next_state
            .files
            .iter()
            .filter_map(|(key, state)| {
                let previous = previous_state.files.get(key)?;
                (state.indexed && state.superseded_by != previous.superseded_by)
                    .then(|| key.clone())
            })
            .collect::<Vec<_>>();
        for key in relation_changed {
            if parsed_sessions.contains_key(&key) {
                continue;
            }
            let Some(file) = scanned_by_key.get(&key) else {
                continue;
            };
            match parse_session_file(file.agent, &file.path) {
                Ok(Some(session)) => {
                    writer.delete_term(Term::from_field_text(fields.file_path, &key));
                    let superseded_by = next_state
                        .files
                        .get(&key)
                        .and_then(|state| state.superseded_by.clone());
                    add_session_document(&mut writer, &fields, &session, file, superseded_by)?;
                    stats.updated += 1;
                    stats.skipped = stats.skipped.saturating_sub(1);
                    changed = true;
                }
                Ok(None) => {
                    warn!(
                        "could not refresh supersession for {} because it no longer parses as a session",
                        file.path.display()
                    );
                    if let (Some(state), Some(previous)) = (
                        next_state.files.get_mut(&key),
                        previous_state.files.get(&key),
                    ) {
                        state.superseded_by.clone_from(&previous.superseded_by);
                    }
                }
                Err(error) => {
                    warn!(
                        "could not refresh supersession for {}: {error:#}",
                        file.path.display()
                    );
                    if let (Some(state), Some(previous)) = (
                        next_state.files.get_mut(&key),
                        previous_state.files.get(&key),
                    ) {
                        state.superseded_by.clone_from(&previous.superseded_by);
                    }
                }
            }
        }

        for deleted_path in previous_state.files.keys() {
            if next_state.files.contains_key(deleted_path) {
                continue;
            }
            writer.delete_term(Term::from_field_text(fields.file_path, deleted_path));
            stats.removed += 1;
            changed = true;
        }

        if changed {
            writer.commit().context("failed to commit tantivy index")?;
        }

        save_state(&self.paths.state_file, &next_state)?;
        info!(
            "indexed sessions: scanned={}, updated={}, skipped={}, removed={}",
            stats.scanned, stats.updated, stats.skipped, stats.removed
        );
        Ok(stats)
    }

    pub fn open_search_engine(&self) -> Result<SearchEngine> {
        SearchEngine::open(&self.paths)
    }

    pub fn open_search_engine_with_live_sessions(
        &self,
        live_sessions: LiveSessionTracker,
    ) -> Result<SearchEngine> {
        SearchEngine::open_with_live_sessions(&self.paths, live_sessions)
    }

    fn open_or_create_index(&self) -> Result<(Index, IndexSchema)> {
        let schema = IndexSchema::new();
        fs::create_dir_all(&self.paths.index_dir)
            .with_context(|| format!("failed to create {}", self.paths.index_dir.display()))?;

        let existing = Index::open_in_dir(&self.paths.index_dir)
            .ok()
            .and_then(|index| {
                IndexSchema::from_schema(&index.schema())
                    .ok()
                    .map(|fields| (index, fields))
            });
        if let Some(existing) = existing {
            return Ok(existing);
        }

        if self.paths.index_dir.exists() {
            fs::remove_dir_all(&self.paths.index_dir)
                .with_context(|| format!("failed to reset {}", self.paths.index_dir.display()))?;
            fs::create_dir_all(&self.paths.index_dir).with_context(|| {
                format!("failed to recreate {}", self.paths.index_dir.display())
            })?;
        }

        let index = Index::create_in_dir(&self.paths.index_dir, schema.schema.clone())
            .with_context(|| format!("failed to create {}", self.paths.index_dir.display()))?;
        Ok((index, schema))
    }
}

fn add_session_document(
    writer: &mut tantivy::IndexWriter,
    fields: &IndexSchema,
    session: &Session,
    file: &SessionFile,
    superseded_by: Option<String>,
) -> Result<()> {
    let mut document = TantivyDocument::default();
    let stored = StoredSession::from_session_file(session, file, superseded_by);
    let stored_json = serde_json::to_string(&stored).context("failed to serialize session")?;

    document.add_text(fields.file_path, normalize_path_key(&session.file_path));
    document.add_text(fields.content, searchable_content(session));
    document.add_u64(fields.modified_ts, session.modified_ts);
    document.add_text(fields.session_json, &stored_json);

    writer.add_document(document)?;
    Ok(())
}

fn searchable_content(session: &Session) -> String {
    let mut chunks = Vec::with_capacity(3);
    if let Some(title) = session.custom_title.as_deref() {
        if !title.trim().is_empty() {
            chunks.push(title.trim());
        }
    }
    if !session.first_user_msg_content.trim().is_empty() {
        chunks.push(session.first_user_msg_content.trim());
    }
    if !session.content.trim().is_empty() {
        chunks.push(session.content.trim());
    }
    chunks.join("\n\n")
}

/// Bump when stored fields or searchable-content semantics change so old state
/// files are discarded and the index is rebuilt against fresh data.
const INDEX_FORMAT_VERSION: u32 = 9;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct IndexState {
    #[serde(default)]
    format_version: u32,
    files: BTreeMap<String, IndexedFileState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct IndexedFileState {
    modified_secs: u64,
    size: u64,
    agent: Agent,
    trashed: bool,
    original_path: Option<PathBuf>,
    indexed: bool,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    lineage: SessionLineage,
    #[serde(default)]
    superseded_by: Option<String>,
}

impl IndexedFileState {
    fn from_file(file: &SessionFile, indexed: bool) -> Self {
        Self {
            modified_secs: file
                .modified
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            size: file.size,
            agent: file.agent,
            trashed: file.trashed,
            original_path: file.original_path.clone(),
            indexed,
            session_id: None,
            lineage: SessionLineage::default(),
            superseded_by: None,
        }
    }

    fn from_session(file: &SessionFile, session: &Session) -> Self {
        let mut state = Self::from_file(file, true);
        state.session_id = Some(session.session_id.clone());
        state.lineage = session.lineage.clone();
        state
    }

    fn fingerprint(&self) -> FileFingerprint {
        FileFingerprint {
            modified_secs: self.modified_secs,
            size: self.size,
            agent: self.agent,
            trashed: self.trashed,
            original_path: self.original_path.clone(),
        }
    }
}

fn apply_supersession(state: &mut IndexState) {
    for file in state.files.values_mut() {
        file.superseded_by = None;
    }

    // Declared parent IDs bound all semantic comparisons to actual fork families.
    let keys = state.files.keys().cloned().collect::<Vec<_>>();
    let mut indexes_by_session = HashMap::<(Agent, String), Vec<usize>>::new();
    for (index, key) in keys.iter().enumerate() {
        let Some(file) = state.files.get(key) else {
            continue;
        };
        if !file.indexed {
            continue;
        }
        if let Some(session_id) = file.session_id.as_ref() {
            indexes_by_session
                .entry((file.agent, session_id.clone()))
                .or_default()
                .push(index);
        }
    }

    let mut family_parents = (0..keys.len()).collect::<Vec<_>>();
    let mut fork_edges = Vec::<ForkEdge>::new();
    for (child_index, child_key) in keys.iter().enumerate() {
        let Some(child) = state.files.get(child_key) else {
            continue;
        };
        let Some(parent_id) = child.lineage.forked_from_session_id.as_ref() else {
            continue;
        };
        if !child.indexed || child.trashed {
            continue;
        }
        let Some(parent_indexes) = indexes_by_session.get(&(child.agent, parent_id.clone())) else {
            continue;
        };
        for &parent_index in parent_indexes {
            if parent_index == child_index {
                continue;
            }
            union_families(&mut family_parents, parent_index, child_index);

            let Some(parent) = state.files.get(&keys[parent_index]) else {
                continue;
            };
            if parent.lineage.semantic_event_ids.is_empty() {
                continue;
            }
            let relation = lineage_relation(parent.agent, &parent.lineage, &child.lineage);
            if relation != LineageRelation::Unrelated {
                fork_edges.push(ForkEdge {
                    parent: parent_index,
                    child: child_index,
                    relation,
                });
            }
        }
    }

    let family_roots = (0..keys.len())
        .map(|index| find_family(&mut family_parents, index))
        .collect::<Vec<_>>();
    let mut family_sizes = HashMap::<usize, usize>::new();
    for (index, key) in keys.iter().enumerate() {
        let file = state
            .files
            .get(key)
            .expect("fork-family session key must exist");
        if file.indexed && !file.trashed {
            *family_sizes.entry(family_roots[index]).or_default() += 1;
        }
    }
    // Equal normalized event sets collapse only within the same declared family.
    let mut equivalent_groups = HashMap::<(usize, Agent, Vec<String>), Vec<usize>>::new();
    for (index, key) in keys.iter().enumerate() {
        let Some(file) = state.files.get(key) else {
            continue;
        };
        if !file.indexed
            || file.trashed
            || family_sizes.get(&family_roots[index]).copied() == Some(1)
        {
            continue;
        }
        let semantic_key = semantic_equivalence_key(file.agent, &file.lineage);
        if semantic_key.is_empty() {
            continue;
        }
        equivalent_groups
            .entry((family_roots[index], file.agent, semantic_key))
            .or_default()
            .push(index);
    }

    let mut representative = (0..keys.len()).collect::<Vec<_>>();
    let mut superseded_by = vec![None::<String>; keys.len()];
    let parents_with_equivalent_children = fork_edges
        .iter()
        .filter(|edge| edge.relation == LineageRelation::Equivalent)
        .map(|edge| edge.parent)
        .collect::<HashSet<_>>();
    for members in equivalent_groups
        .values()
        .filter(|members| members.len() > 1)
    {
        // Parentage wins over timestamps. Timestamp and ID only choose among
        // equivalent leaves, such as sibling forks of the same session.
        let leaves = members
            .iter()
            .copied()
            .filter(|index| !parents_with_equivalent_children.contains(index))
            .collect::<Vec<_>>();
        let candidates = if leaves.is_empty() { members } else { &leaves };
        let Some(&winner) = candidates.iter().max_by(|left, right| {
            let left_file = state
                .files
                .get(&keys[**left])
                .expect("equivalent session key must exist");
            let right_file = state
                .files
                .get(&keys[**right])
                .expect("equivalent session key must exist");
            (
                left_file.modified_secs,
                left_file.session_id.as_deref().unwrap_or_default(),
                keys[**left].as_str(),
            )
                .cmp(&(
                    right_file.modified_secs,
                    right_file.session_id.as_deref().unwrap_or_default(),
                    keys[**right].as_str(),
                ))
        }) else {
            continue;
        };
        let winner_id = session_id_or_key(
            state
                .files
                .get(&keys[winner])
                .expect("equivalent session key must exist"),
            &keys[winner],
        );
        for &member in members {
            representative[member] = winner;
            if member != winner {
                superseded_by[member] = Some(winner_id.clone());
            }
        }
    }

    // Contract equivalent groups before choosing among strict, divergent
    // successors so any continuation of an equivalent member hides the group.
    type SuccessorRank = (usize, u64, String, String);
    let mut best_successors = HashMap::<usize, (SuccessorRank, usize)>::new();
    for edge in fork_edges
        .iter()
        .filter(|edge| edge.relation == LineageRelation::Supersedes)
    {
        let source = representative[edge.parent];
        let target = representative[edge.child];
        if source == target {
            continue;
        }
        let target_file = state
            .files
            .get(&keys[target])
            .expect("successor session key must exist");
        let rank = (
            target_file.lineage.semantic_event_ids.len(),
            target_file.modified_secs,
            target_file.session_id.clone().unwrap_or_default(),
            keys[target].clone(),
        );
        if best_successors
            .get(&source)
            .is_none_or(|(current, _)| rank > *current)
        {
            best_successors.insert(source, (rank, target));
        }
    }
    for (source, (_, target)) in best_successors {
        let target_id = session_id_or_key(
            state
                .files
                .get(&keys[target])
                .expect("successor session key must exist"),
            &keys[target],
        );
        superseded_by[source] = Some(target_id);
    }

    for (index, successor) in superseded_by.into_iter().enumerate() {
        if let Some(successor) = successor {
            state
                .files
                .get_mut(&keys[index])
                .expect("indexed session key must exist")
                .superseded_by = Some(successor);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineageRelation {
    Unrelated,
    Equivalent,
    Supersedes,
}

#[derive(Debug, Clone, Copy)]
struct ForkEdge {
    parent: usize,
    child: usize,
    relation: LineageRelation,
}

fn lineage_relation(
    agent: Agent,
    parent: &SessionLineage,
    child: &SessionLineage,
) -> LineageRelation {
    match agent {
        Agent::Codex => codex_lineage_relation(parent, child),
        Agent::Claude => {
            if !parent
                .semantic_event_ids
                .iter()
                .all(|id| child.inherited_event_ids.binary_search(id).is_ok())
            {
                LineageRelation::Unrelated
            } else if child.own_semantic_event_count > 0 {
                LineageRelation::Supersedes
            } else if child.inherited_event_ids == parent.semantic_event_ids {
                LineageRelation::Equivalent
            } else {
                LineageRelation::Unrelated
            }
        }
    }
}

fn codex_lineage_relation(parent: &SessionLineage, child: &SessionLineage) -> LineageRelation {
    let missing_parent_events = parent
        .semantic_event_ids
        .iter()
        .filter(|id| child.semantic_event_ids.binary_search(id).is_err())
        .collect::<Vec<_>>();

    if missing_parent_events.is_empty() {
        return match child
            .semantic_event_ids
            .len()
            .cmp(&parent.semantic_event_ids.len())
        {
            std::cmp::Ordering::Greater => LineageRelation::Supersedes,
            std::cmp::Ordering::Equal => LineageRelation::Equivalent,
            std::cmp::Ordering::Less => LineageRelation::Unrelated,
        };
    }

    let Some(aborted) = parent.trailing_aborted_turn.as_ref() else {
        return LineageRelation::Unrelated;
    };
    if !missing_parent_events.iter().all(|id| {
        aborted
            .semantic_event_ids
            .binary_search_by(|candidate| candidate.as_str().cmp(id.as_str()))
            .is_ok()
    }) {
        return LineageRelation::Unrelated;
    }

    let dropped_only_empty_aborted_turn = !aborted.had_assistant_or_tool_activity
        && missing_parent_events.len() == aborted.semantic_event_ids.len();
    let child_has_new_event = child
        .semantic_event_ids
        .iter()
        .any(|id| parent.semantic_event_ids.binary_search(id).is_err());
    if dropped_only_empty_aborted_turn && !child_has_new_event {
        return LineageRelation::Equivalent;
    }

    let child_has_new_activity = child
        .assistant_or_tool_event_ids
        .iter()
        .any(|id| is_child_only_event(parent, child, id));
    if !child_has_new_activity {
        return LineageRelation::Unrelated;
    }

    if dropped_only_empty_aborted_turn
        || (!aborted.user_message_line_multiset_sha256.is_empty()
            && child.codex_user_turns.iter().any(|turn| {
                turn.user_message_line_multiset_sha256 == aborted.user_message_line_multiset_sha256
                    && is_child_only_event(parent, child, &turn.last_assistant_or_tool_event_id)
            }))
    {
        LineageRelation::Supersedes
    } else {
        LineageRelation::Unrelated
    }
}

fn is_child_only_event(parent: &SessionLineage, child: &SessionLineage, event_id: &str) -> bool {
    child
        .semantic_event_ids
        .binary_search_by(|candidate| candidate.as_str().cmp(event_id))
        .is_ok()
        && parent
            .semantic_event_ids
            .binary_search_by(|candidate| candidate.as_str().cmp(event_id))
            .is_err()
}

fn semantic_equivalence_key(agent: Agent, lineage: &SessionLineage) -> Vec<String> {
    match agent {
        Agent::Codex => {
            let mut event_ids = lineage.semantic_event_ids.clone();
            if let Some(aborted) = lineage
                .trailing_aborted_turn
                .as_ref()
                .filter(|aborted| !aborted.had_assistant_or_tool_activity)
            {
                event_ids.retain(|id| aborted.semantic_event_ids.binary_search(id).is_err());
            }
            event_ids
        }
        Agent::Claude => {
            let mut event_ids = lineage.inherited_event_ids.clone();
            event_ids.extend(lineage.semantic_event_ids.iter().cloned());
            event_ids.sort_unstable();
            event_ids.dedup();
            event_ids
        }
    }
}

fn session_id_or_key(file: &IndexedFileState, key: &str) -> String {
    file.session_id.clone().unwrap_or_else(|| key.to_owned())
}

fn find_family(parents: &mut [usize], index: usize) -> usize {
    if parents[index] != index {
        parents[index] = find_family(parents, parents[index]);
    }
    parents[index]
}

fn union_families(parents: &mut [usize], left: usize, right: usize) {
    let left = find_family(parents, left);
    let right = find_family(parents, right);
    if left != right {
        parents[right] = left;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileFingerprint {
    modified_secs: u64,
    size: u64,
    agent: Agent,
    trashed: bool,
    original_path: Option<PathBuf>,
}

impl From<&SessionFile> for FileFingerprint {
    fn from(file: &SessionFile) -> Self {
        Self {
            modified_secs: file
                .modified
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            size: file.size,
            agent: file.agent,
            trashed: file.trashed,
            original_path: file.original_path.clone(),
        }
    }
}

fn load_state(path: &Path) -> Result<IndexState> {
    if !path.exists() {
        return Ok(IndexState::default());
    }

    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let state: IndexState = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if state.format_version != INDEX_FORMAT_VERSION {
        info!(
            "index format version mismatch ({} != {INDEX_FORMAT_VERSION}); rebuilding",
            state.format_version
        );
        return Ok(IndexState::default());
    }
    Ok(state)
}

fn save_state(path: &Path, state: &IndexState) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let raw = serde_json::to_string_pretty(state).context("failed to serialize index state")?;
    fs::write(path, raw).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn normalize_path_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProfileMetadata {
    version: u32,
    claude_home: PathBuf,
    codex_home: PathBuf,
    claude_projects: PathBuf,
    claude_sessions: PathBuf,
    codex_sessions: PathBuf,
}

fn profile_id_from_roots(roots: &SessionRoots) -> String {
    let digest = Sha256::digest(roots.profile_hash_input().as_bytes());
    let mut output = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn default_cache_dir(home: &Path) -> PathBuf {
    home.join(".cache").join("aics")
}

fn env_override(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn is_lock_busy_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<TantivyError>(),
            Some(TantivyError::LockFailure(LockError::LockBusy, _))
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{CodexUserTurn, TrailingAbortedTurn};

    #[test]
    fn default_cache_dir_is_home_relative_dot_cache() {
        let home = PathBuf::from("home").join("alice");
        assert_eq!(
            default_cache_dir(&home),
            PathBuf::from("home")
                .join("alice")
                .join(".cache")
                .join("aics")
        );
    }

    #[test]
    fn codex_supersession_accepts_only_trailing_aborted_parent_turns() {
        let parent = SessionLineage {
            semantic_event_ids: sorted_ids(&[
                "message:abort-parent",
                "message:assistant-1",
                "message:retry-parent",
                "message:user-1",
            ]),
            trailing_aborted_turn: Some(TrailingAbortedTurn {
                user_message_line_multiset_sha256: "retry-hash".to_owned(),
                semantic_event_ids: sorted_ids(&["message:abort-parent", "message:retry-parent"]),
                had_assistant_or_tool_activity: false,
            }),
            ..SessionLineage::default()
        };
        let child = SessionLineage {
            semantic_event_ids: sorted_ids(&[
                "message:assistant-1",
                "message:assistant-child",
                "message:user-1",
            ]),
            assistant_or_tool_event_ids: vec!["message:assistant-child".to_owned()],
            ..SessionLineage::default()
        };

        assert_eq!(
            codex_lineage_relation(&parent, &child),
            LineageRelation::Supersedes
        );

        let mut no_continuation = child.clone();
        no_continuation.assistant_or_tool_event_ids.clear();
        assert_eq!(
            codex_lineage_relation(&parent, &no_continuation),
            LineageRelation::Unrelated
        );

        let mut extra_parent_event = parent.clone();
        extra_parent_event
            .semantic_event_ids
            .push("message:continued-parent".to_owned());
        extra_parent_event.semantic_event_ids.sort_unstable();
        assert_eq!(
            codex_lineage_relation(&extra_parent_event, &child),
            LineageRelation::Unrelated
        );

        let mut not_trailing = parent;
        not_trailing.trailing_aborted_turn = None;
        assert_eq!(
            codex_lineage_relation(&not_trailing, &child),
            LineageRelation::Unrelated
        );
    }

    #[test]
    fn codex_supersession_accepts_retried_aborted_turn_with_partial_work() {
        let parent = SessionLineage {
            semantic_event_ids: sorted_ids(&[
                "message:assistant-1",
                "message:parent-ack",
                "reasoning:parent-reasoning",
                "custom_tool_call:parent-tool",
                "custom_tool_call_output:parent-tool",
            ]),
            assistant_or_tool_event_ids: sorted_ids(&[
                "message:assistant-1",
                "message:parent-ack",
                "reasoning:parent-reasoning",
                "custom_tool_call:parent-tool",
                "custom_tool_call_output:parent-tool",
            ]),
            trailing_aborted_turn: Some(TrailingAbortedTurn {
                user_message_line_multiset_sha256: "retry-hash".to_owned(),
                semantic_event_ids: sorted_ids(&[
                    "message:parent-ack",
                    "reasoning:parent-reasoning",
                    "custom_tool_call:parent-tool",
                    "custom_tool_call_output:parent-tool",
                ]),
                had_assistant_or_tool_activity: true,
            }),
            ..SessionLineage::default()
        };
        let child = SessionLineage {
            semantic_event_ids: sorted_ids(&[
                "message:assistant-1",
                "message:child-finished",
                "reasoning:child-reasoning",
            ]),
            assistant_or_tool_event_ids: sorted_ids(&[
                "message:assistant-1",
                "message:child-finished",
                "reasoning:child-reasoning",
            ]),
            codex_user_turns: vec![CodexUserTurn {
                user_message_line_multiset_sha256: "retry-hash".to_owned(),
                last_assistant_or_tool_event_id: "message:child-finished".to_owned(),
            }],
            ..SessionLineage::default()
        };

        assert_eq!(
            codex_lineage_relation(&parent, &child),
            LineageRelation::Supersedes
        );

        let mut different_prompt = child.clone();
        different_prompt.codex_user_turns[0].user_message_line_multiset_sha256 =
            "different-hash".to_owned();
        assert_eq!(
            codex_lineage_relation(&parent, &different_prompt),
            LineageRelation::Unrelated
        );

        let mut no_new_turn_activity = child.clone();
        no_new_turn_activity.codex_user_turns[0].last_assistant_or_tool_event_id =
            "message:assistant-1".to_owned();
        assert_eq!(
            codex_lineage_relation(&parent, &no_new_turn_activity),
            LineageRelation::Unrelated
        );

        let mut extra_parent_event = parent;
        extra_parent_event
            .semantic_event_ids
            .push("message:other-parent-turn".to_owned());
        extra_parent_event.semantic_event_ids.sort_unstable();
        assert_eq!(
            codex_lineage_relation(&extra_parent_event, &child),
            LineageRelation::Unrelated
        );
    }

    fn sorted_ids(ids: &[&str]) -> Vec<String> {
        let mut ids = ids.iter().map(|id| (*id).to_owned()).collect::<Vec<_>>();
        ids.sort_unstable();
        ids
    }
}
