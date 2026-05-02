use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use directories::BaseDirs;
use log::warn;

use crate::parse::session::{system_time_or_epoch, Agent};
use crate::trash::{TrashPaths, TrashStore};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRoots {
    pub claude_projects: PathBuf,
    pub codex_sessions: PathBuf,
    pub trash: Option<TrashPaths>,
}

impl SessionRoots {
    pub fn profile_hash_input(&self) -> String {
        format!(
            "claude_projects={}\ncodex_sessions={}\ntrash_dir={}\ntrash_metadata={}\n",
            self.claude_projects.display(),
            self.codex_sessions.display(),
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
            },
            roots: SessionRoots {
                claude_projects,
                codex_sessions,
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
}

pub(crate) trait ScanProgressObserver {
    fn on_discovered(&mut self, discovered: usize);
}

struct NoopScanProgress;

impl ScanProgressObserver for NoopScanProgress {
    fn on_discovered(&mut self, _discovered: usize) {}
}

pub fn default_session_roots() -> Result<SessionRoots> {
    Ok(ResolvedPaths::discover(None, None)?.roots)
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
        });
        progress.on_discovered(output.len());
    }

    Ok(())
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
        });
        progress.on_discovered(output.len());
    }

    Ok(())
}
