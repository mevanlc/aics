use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use tantivy::directory::error::LockError;
use tantivy::{Index, TantivyDocument, TantivyError, Term};

use crate::index::reader::SearchEngine;
use crate::index::schema::IndexSchema;
use crate::parse::{parse_session_file, Agent, DerivationType, MessageRole, Session};
use crate::scan::{
    default_session_roots, scan_session_files_with_progress, SessionFile, SessionRoots,
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
        }
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
}

impl IndexPaths {
    pub fn discover() -> Result<Self> {
        if let Some(cache_root) = env_override("AICS_CACHE_ROOT") {
            return Ok(Self::from_root(cache_root));
        }
        let project_dirs =
            ProjectDirs::from("", "", "aics").context("failed to locate cache directory")?;
        Ok(Self::from_root(project_dirs.cache_dir()))
    }

    pub fn from_root(cache_root: impl Into<PathBuf>) -> Self {
        let cache_root = cache_root.into();
        Self {
            index_dir: cache_root.join("index"),
            state_file: cache_root.join("index_state.json"),
            cache_root,
        }
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
        let mut next_state = IndexState::default();
        let mut stats = SyncStats {
            scanned: scanned_files.len(),
            ..SyncStats::default()
        };
        let mut writer = index.writer(50_000_000)?;
        let mut changed = rebuild;
        let mut pending_files = Vec::new();

        for file in &scanned_files {
            let key = normalize_path_key(&file.path);
            let fingerprint = FileFingerprint::from(file);

            if previous_state
                .files
                .get(&key)
                .is_some_and(|state| state.fingerprint() == fingerprint)
            {
                let indexed = previous_state
                    .files
                    .get(&key)
                    .map(|state| state.indexed)
                    .unwrap_or(false);
                next_state
                    .files
                    .insert(key, IndexedFileState::from_file(file, indexed));
                stats.skipped += 1;
                continue;
            }

            pending_files.push(file.clone());
        }

        progress.on_progress(SyncProgress::IndexingStarted {
            total: pending_files.len(),
        });

        for (index, file) in pending_files.iter().enumerate() {
            let key = normalize_path_key(&file.path);
            writer.delete_term(Term::from_field_text(fields.file_path, &key));
            changed = true;

            let indexed = match parse_session_file(file.agent, &file.path) {
                Ok(Some(session)) => {
                    add_session_document(&mut writer, &fields, &session)?;
                    stats.updated += 1;
                    true
                }
                Ok(None) => false,
                Err(error) => {
                    warn!("failed to parse {}: {error:#}", file.path.display());
                    false
                }
            };
            next_state
                .files
                .insert(key, IndexedFileState::from_file(file, indexed));
            progress.on_progress(SyncProgress::IndexingProgress {
                processed: index + 1,
                total: pending_files.len(),
            });
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
) -> Result<()> {
    let mut document = TantivyDocument::default();
    let stored = StoredSession::from(session);
    let stored_json = serde_json::to_string(&stored).context("failed to serialize session")?;

    document.add_text(fields.file_path, &normalize_path_key(&session.file_path));
    document.add_text(fields.content, &searchable_content(session));
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct IndexState {
    files: BTreeMap<String, IndexedFileState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct IndexedFileState {
    modified_secs: u64,
    size: u64,
    agent: Agent,
    indexed: bool,
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
            indexed,
        }
    }

    fn fingerprint(&self) -> FileFingerprint {
        FileFingerprint {
            modified_secs: self.modified_secs,
            size: self.size,
            agent: self.agent,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileFingerprint {
    modified_secs: u64,
    size: u64,
    agent: Agent,
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
        }
    }
}

fn load_state(path: &Path) -> Result<IndexState> {
    if !path.exists() {
        return Ok(IndexState::default());
    }

    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
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
