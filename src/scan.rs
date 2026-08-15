use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use directories::BaseDirs;
use log::warn;
use serde_json::Value;
use sha2::{Digest, Sha256};
use url::Url;

use crate::parse::session::{system_time_or_epoch, Agent};
use crate::trash::{TrashPaths, TrashStore};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRoots {
    pub claude_projects: PathBuf,
    pub codex_sessions: PathBuf,
    pub antigravity_home: PathBuf,
    pub trash: Option<TrashPaths>,
}

impl SessionRoots {
    pub fn profile_hash_input(&self) -> String {
        format!(
            "claude_projects={}\ncodex_sessions={}\nantigravity_home={}\ntrash_dir={}\ntrash_metadata={}\n",
            self.claude_projects.display(),
            self.codex_sessions.display(),
            self.antigravity_home.display(),
            self.trash
                .as_ref()
                .map(|paths| paths.trash_dir.display().to_string())
                .unwrap_or_default(),
            self.trash
                .as_ref()
                .map(|paths| paths.metadata_file.display().to_string())
                .unwrap_or_default()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentHomes {
    pub claude_home: PathBuf,
    pub codex_home: PathBuf,
    pub antigravity_home: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPaths {
    pub homes: AgentHomes,
    pub roots: SessionRoots,
    pub claude_sessions: PathBuf,
}

impl ResolvedPaths {
    pub fn discover(
        claude_home_override: Option<&Path>,
        codex_home_override: Option<&Path>,
        antigravity_home_override: Option<&Path>,
    ) -> Result<Self> {
        let base_dirs = BaseDirs::new().context("failed to locate home directory")?;
        let home = base_dirs.home_dir();
        let current_dir = env::current_dir().context("failed to resolve current directory")?;

        let cli_claude_home = claude_home_override
            .map(|path| resolve_path(path.to_path_buf(), &current_dir))
            .transpose()?;
        let cli_codex_home = codex_home_override
            .map(|path| resolve_path(path.to_path_buf(), &current_dir))
            .transpose()?;
        let cli_antigravity_home = antigravity_home_override
            .map(|path| resolve_path(path.to_path_buf(), &current_dir))
            .transpose()?;

        let claude_home = match cli_claude_home.clone() {
            Some(path) => path,
            None => {
                let path =
                    env_override("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude"));
                resolve_path(path, &current_dir)?
            }
        };
        let codex_home = match cli_codex_home.clone() {
            Some(path) => path,
            None => {
                let path = env_override("CODEX_HOME").unwrap_or_else(|| home.join(".codex"));
                resolve_path(path, &current_dir)?
            }
        };
        let antigravity_home = match cli_antigravity_home {
            Some(path) => path,
            None => {
                let path = env_override("AICS_ANTIGRAVITY_HOME")
                    .unwrap_or_else(|| home.join(".gemini").join("antigravity-cli"));
                resolve_path(path, &current_dir)?
            }
        };

        let claude_projects = if cli_claude_home.is_some() {
            claude_home.join("projects")
        } else {
            env_override("AICS_CLAUDE_PROJECTS_DIR").unwrap_or_else(|| claude_home.join("projects"))
        };
        let claude_sessions = if cli_claude_home.is_some() {
            claude_home.join("sessions")
        } else {
            env_override("AICS_CLAUDE_SESSIONS_DIR").unwrap_or_else(|| claude_home.join("sessions"))
        };
        let codex_sessions = if cli_codex_home.is_some() {
            codex_home.join("sessions")
        } else {
            env_override("AICS_CODEX_SESSIONS_DIR").unwrap_or_else(|| codex_home.join("sessions"))
        };

        Ok(Self {
            homes: AgentHomes {
                claude_home,
                codex_home,
                antigravity_home: antigravity_home.clone(),
            },
            roots: SessionRoots {
                claude_projects,
                codex_sessions,
                antigravity_home,
                trash: Some(TrashPaths::discover()?),
            },
            claude_sessions,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFile {
    pub path: PathBuf,
    pub agent: Agent,
    pub modified: SystemTime,
    pub size: u64,
    pub trashed: bool,
    pub original_path: Option<PathBuf>,
    pub companion_paths: Vec<PathBuf>,
    pub source_signature: u64,
    pub antigravity_metadata: Option<AntigravitySessionMetadata>,
}

impl SessionFile {
    pub fn source_paths(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.path.as_path())
            .chain(self.companion_paths.iter().map(PathBuf::as_path))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AntigravitySessionMetadata {
    pub cwd: Option<String>,
    pub custom_title: Option<String>,
    pub preview: Option<String>,
}

pub(crate) trait ScanProgressObserver {
    fn on_discovered(&mut self, discovered: usize);
}

struct NoopScanProgress;

impl ScanProgressObserver for NoopScanProgress {
    fn on_discovered(&mut self, _discovered: usize) {}
}

pub fn default_session_roots() -> Result<SessionRoots> {
    Ok(ResolvedPaths::discover(None, None, None)?.roots)
}

pub fn is_default_antigravity_home(path: &Path) -> bool {
    let Some(base_dirs) = BaseDirs::new() else {
        return false;
    };
    let default = base_dirs.home_dir().join(".gemini").join("antigravity-cli");
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let default = default.canonicalize().unwrap_or(default);
    resolved == default
}

fn env_override(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn resolve_path(path: PathBuf, current_dir: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path
    } else {
        current_dir.join(path)
    };
    match path.canonicalize() {
        Ok(canonical) => Ok(canonical),
        Err(_) => Ok(path),
    }
}

pub fn scan_default_session_files() -> Result<Vec<SessionFile>> {
    let roots = default_session_roots()?;
    scan_session_files(&roots)
}

pub fn scan_session_files(roots: &SessionRoots) -> Result<Vec<SessionFile>> {
    let mut progress = NoopScanProgress;
    scan_session_files_with_progress(roots, &mut progress)
}

pub(crate) fn scan_session_files_with_progress<P: ScanProgressObserver>(
    roots: &SessionRoots,
    progress: &mut P,
) -> Result<Vec<SessionFile>> {
    let mut files = Vec::new();
    scan_agent_root(&roots.claude_projects, Agent::Claude, &mut files, progress)?;
    scan_agent_root(&roots.codex_sessions, Agent::Codex, &mut files, progress)?;
    scan_antigravity_root(&roots.antigravity_home, &mut files, progress)?;
    if let Some(trash) = roots.trash.as_ref() {
        scan_trash_root(trash, &mut files, progress)?;
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn scan_agent_root<P: ScanProgressObserver>(
    root: &Path,
    agent: Agent,
    output: &mut Vec<SessionFile>,
    progress: &mut P,
) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }

    scan_directory(root, agent, output, progress)
}

fn scan_directory<P: ScanProgressObserver>(
    directory: &Path,
    agent: Agent,
    output: &mut Vec<SessionFile>,
    progress: &mut P,
) -> Result<()> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            warn!(
                "skipping unreadable directory {}: {error}",
                directory.display()
            );
            return Ok(());
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warn!(
                    "skipping unreadable directory entry in {}: {error}",
                    directory.display()
                );
                continue;
            }
        };

        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                warn!(
                    "skipping path with unreadable file type {}: {error}",
                    path.display()
                );
                continue;
            }
        };

        if file_type.is_dir() {
            scan_directory(&path, agent, output, progress)?;
            continue;
        }

        if !file_type.is_file()
            || path.extension().and_then(|extension| extension.to_str()) != Some("jsonl")
        {
            continue;
        }

        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                warn!(
                    "skipping path with unreadable metadata {}: {error}",
                    path.display()
                );
                continue;
            }
        };

        output.push(SessionFile {
            path,
            agent,
            modified: system_time_or_epoch(metadata.modified().ok()),
            size: metadata.len(),
            trashed: false,
            original_path: None,
            companion_paths: Vec::new(),
            source_signature: 0,
            antigravity_metadata: None,
        });
        progress.on_discovered(output.len());
    }

    Ok(())
}

fn scan_antigravity_root<P: ScanProgressObserver>(
    root: &Path,
    output: &mut Vec<SessionFile>,
    progress: &mut P,
) -> Result<()> {
    let brain = root.join("brain");
    if !brain.exists() {
        return Ok(());
    }

    let mut metadata = load_antigravity_metadata(root);
    apply_antigravity_history(root, &mut metadata);
    let entries = match fs::read_dir(&brain) {
        Ok(entries) => entries,
        Err(error) => {
            warn!(
                "skipping unreadable Antigravity brain {}: {error}",
                brain.display()
            );
            return Ok(());
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warn!(
                    "skipping unreadable Antigravity conversation in {}: {error}",
                    brain.display()
                );
                continue;
            }
        };
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }

        let session_id = entry.file_name().to_string_lossy().into_owned();
        let path = entry
            .path()
            .join(".system_generated")
            .join("logs")
            .join("transcript.jsonl");
        let session_metadata = metadata.remove(&session_id).unwrap_or_default();
        let Some(file) = build_antigravity_session_file(path, session_metadata) else {
            continue;
        };
        output.push(file);
        progress.on_discovered(output.len());
    }

    Ok(())
}

/// Builds a complete Antigravity bundle descriptor from its regular transcript.
///
/// This is also used by direct parse/export paths so they receive the same full
/// transcript and cache metadata enrichment as indexed discovery.
pub fn antigravity_session_file(path: &Path) -> Option<SessionFile> {
    let logs = path.parent()?;
    let generated = logs.parent()?;
    let conversation = generated.parent()?;
    let brain = conversation.parent()?;
    if path.file_name()?.to_str()? != "transcript.jsonl"
        || logs.file_name()?.to_str()? != "logs"
        || generated.file_name()?.to_str()? != ".system_generated"
        || brain.file_name()?.to_str()? != "brain"
    {
        return None;
    }
    let root = brain.parent()?;
    let session_id = conversation.file_name()?.to_string_lossy();
    let mut metadata = load_antigravity_metadata(root);
    apply_antigravity_history(root, &mut metadata);
    build_antigravity_session_file(
        path.to_path_buf(),
        metadata.remove(session_id.as_ref()).unwrap_or_default(),
    )
}

fn build_antigravity_session_file(
    path: PathBuf,
    session_metadata: AntigravitySessionMetadata,
) -> Option<SessionFile> {
    let primary_metadata = fs::metadata(&path).ok()?;
    if !primary_metadata.is_file() {
        return None;
    }
    let full_path = path.with_file_name("transcript_full.jsonl");
    let full_metadata = fs::metadata(&full_path).ok().filter(|item| item.is_file());
    let companion_paths = full_metadata
        .as_ref()
        .map(|_| vec![full_path])
        .unwrap_or_default();
    let mut modified = system_time_or_epoch(primary_metadata.modified().ok());
    let mut size = primary_metadata.len();
    if let Some(item) = full_metadata.as_ref() {
        modified = modified.max(system_time_or_epoch(item.modified().ok()));
        size = size.saturating_add(item.len());
    }
    let source_signature = antigravity_source_signature(
        &path,
        &primary_metadata,
        companion_paths.first().zip(full_metadata.as_ref()),
        &session_metadata,
    );

    Some(SessionFile {
        path,
        agent: Agent::Antigravity,
        modified,
        size,
        trashed: false,
        original_path: None,
        companion_paths,
        source_signature,
        antigravity_metadata: Some(session_metadata),
    })
}

fn load_antigravity_metadata(root: &Path) -> HashMap<String, AntigravitySessionMetadata> {
    let path = root.join("cache").join("conversation_metadata.json");
    let raw = match fs::read(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return HashMap::new(),
        Err(error) => {
            warn!(
                "failed to read Antigravity metadata {}: {error}",
                path.display()
            );
            return HashMap::new();
        }
    };
    let value: Value = match serde_json::from_slice(&raw) {
        Ok(value) => value,
        Err(error) => {
            warn!(
                "failed to parse Antigravity metadata {}: {error}",
                path.display()
            );
            return HashMap::new();
        }
    };
    let Some(conversations) = value.get("conversations").and_then(Value::as_object) else {
        return HashMap::new();
    };

    conversations
        .iter()
        .map(|(session_id, entry)| {
            let summary = entry.get("summary").unwrap_or(&Value::Null);
            let cwd = summary
                .get("WorkspaceURIs")
                .and_then(Value::as_array)
                .and_then(|uris| {
                    uris.iter()
                        .filter_map(Value::as_str)
                        .find_map(workspace_uri_path)
                });
            (
                session_id.clone(),
                AntigravitySessionMetadata {
                    cwd,
                    custom_title: nonempty_string(summary.get("Title")),
                    preview: nonempty_string(summary.get("Preview")),
                },
            )
        })
        .collect()
}

fn apply_antigravity_history(
    root: &Path,
    metadata: &mut HashMap<String, AntigravitySessionMetadata>,
) {
    let path = root.join("history.jsonl");
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            warn!(
                "failed to read Antigravity history {}: {error}",
                path.display()
            );
            return;
        }
    };

    let mut latest_workspaces = HashMap::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(session_id) = value.get("conversationId").and_then(Value::as_str) else {
            continue;
        };
        let Some(cwd) = value
            .get("workspace")
            .and_then(Value::as_str)
            .and_then(workspace_uri_path)
        else {
            continue;
        };
        latest_workspaces.insert(session_id.to_owned(), cwd);
    }
    for (session_id, cwd) in latest_workspaces {
        let entry = metadata.entry(session_id).or_default();
        if entry.cwd.is_none() {
            entry.cwd = Some(cwd);
        }
    }
}

fn workspace_uri_path(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.starts_with("file:") {
        return Url::parse(value)
            .ok()?
            .to_file_path()
            .ok()
            .map(|path| path.to_string_lossy().into_owned());
    }
    Path::new(value)
        .is_absolute()
        .then(|| PathBuf::from(value).to_string_lossy().into_owned())
}

fn nonempty_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn antigravity_source_signature(
    primary_path: &Path,
    primary: &fs::Metadata,
    full: Option<(&PathBuf, &fs::Metadata)>,
    metadata: &AntigravitySessionMetadata,
) -> u64 {
    let mut hasher = Sha256::new();
    update_source_signature(&mut hasher, primary_path, primary);
    if let Some((path, item)) = full {
        update_source_signature(&mut hasher, path, item);
    }
    for value in [
        metadata.cwd.as_deref(),
        metadata.custom_title.as_deref(),
        metadata.preview.as_deref(),
    ] {
        hasher.update(value.unwrap_or_default().as_bytes());
        hasher.update([0]);
    }
    let digest = hasher.finalize();
    u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 prefix has eight bytes"),
    )
}

fn update_source_signature(hasher: &mut Sha256, path: &Path, metadata: &fs::Metadata) {
    hasher.update(path.as_os_str().to_string_lossy().as_bytes());
    hasher.update([0]);
    hasher.update(metadata.len().to_be_bytes());
    let modified_ns = metadata
        .modified()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    hasher.update(modified_ns.to_be_bytes());
}

fn scan_trash_root<P: ScanProgressObserver>(
    paths: &TrashPaths,
    output: &mut Vec<SessionFile>,
    progress: &mut P,
) -> Result<()> {
    let store = TrashStore::new(paths.clone());
    let entries = store.sync()?;

    for entry in entries {
        let Some(agent) = entry.agent() else {
            continue;
        };
        let path = entry.trash_path(store.paths());
        if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
            continue;
        }
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                warn!(
                    "skipping trashed session with unreadable metadata {}: {error}",
                    path.display()
                );
                continue;
            }
        };
        if !metadata.is_file() {
            continue;
        }
        output.push(SessionFile {
            path,
            agent,
            modified: system_time_or_epoch(metadata.modified().ok()),
            size: metadata.len(),
            trashed: true,
            original_path: entry.original_path(),
            companion_paths: Vec::new(),
            source_signature: 0,
            antigravity_metadata: None,
        });
        progress.on_discovered(output.len());
    }

    Ok(())
}
