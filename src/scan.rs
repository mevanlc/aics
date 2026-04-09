use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use directories::BaseDirs;
use log::warn;

use crate::parse::session::{system_time_or_epoch, Agent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRoots {
    pub claude_projects: PathBuf,
    pub codex_sessions: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFile {
    pub path: PathBuf,
    pub agent: Agent,
    pub modified: SystemTime,
    pub size: u64,
}

pub(crate) trait ScanProgressObserver {
    fn on_discovered(&mut self, discovered: usize);
}

struct NoopScanProgress;

impl ScanProgressObserver for NoopScanProgress {
    fn on_discovered(&mut self, _discovered: usize) {}
}

pub fn default_session_roots() -> Result<SessionRoots> {
    let base_dirs = BaseDirs::new().context("failed to locate home directory")?;
    let home = base_dirs.home_dir();

    Ok(SessionRoots {
        claude_projects: env_override("AICS_CLAUDE_PROJECTS_DIR")
            .unwrap_or_else(|| home.join(".claude").join("projects")),
        codex_sessions: env_override("AICS_CODEX_SESSIONS_DIR")
            .unwrap_or_else(|| home.join(".codex").join("sessions")),
    })
}

fn env_override(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
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
        });
        progress.on_discovered(output.len());
    }

    Ok(())
}
