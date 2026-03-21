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

pub fn default_session_roots() -> Result<SessionRoots> {
    let base_dirs = BaseDirs::new().context("failed to locate home directory")?;
    let home = base_dirs.home_dir();

    Ok(SessionRoots {
        claude_projects: home.join(".claude").join("projects"),
        codex_sessions: home.join(".codex").join("sessions"),
    })
}

pub fn scan_default_session_files() -> Result<Vec<SessionFile>> {
    let roots = default_session_roots()?;
    scan_session_files(&roots)
}

pub fn scan_session_files(roots: &SessionRoots) -> Result<Vec<SessionFile>> {
    let mut files = Vec::new();
    scan_agent_root(&roots.claude_projects, Agent::Claude, &mut files)?;
    scan_agent_root(&roots.codex_sessions, Agent::Codex, &mut files)?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn scan_agent_root(root: &Path, agent: Agent, output: &mut Vec<SessionFile>) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }

    scan_directory(root, agent, output)
}

fn scan_directory(directory: &Path, agent: Agent, output: &mut Vec<SessionFile>) -> Result<()> {
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
            scan_directory(&path, agent, output)?;
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
    }

    Ok(())
}
