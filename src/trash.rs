use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result};
use chrono::Utc;
use directories::BaseDirs;
use log::warn;
use serde::{Deserialize, Serialize};

use crate::parse::Agent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashPaths {
    pub data_root: PathBuf,
    pub trash_dir: PathBuf,
    pub metadata_file: PathBuf,
}

impl TrashPaths {
    pub fn discover() -> Result<Self> {
        let data_root =
            if let Some(root) = env::var_os("AICS_DATA_ROOT").filter(|value| !value.is_empty()) {
                PathBuf::from(root)
            } else {
                BaseDirs::new()
                    .context("failed to locate data directory")?
                    .data_dir()
                    .join("aics")
            };
        Ok(Self::from_data_root(data_root))
    }

    pub fn from_data_root(data_root: impl Into<PathBuf>) -> Self {
        let data_root = data_root.into();
        Self {
            trash_dir: data_root.join("trash"),
            metadata_file: data_root.join("trash.jsonl"),
            data_root,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrashEntry {
    pub ts: String,
    pub nm: String,
    pub op: String,
    pub tn: String,
}

impl TrashEntry {
    pub fn agent(&self) -> Option<Agent> {
        Agent::from_str(&self.tn).ok()
    }

    pub fn trash_path(&self, paths: &TrashPaths) -> PathBuf {
        paths.trash_dir.join(&self.nm)
    }

    pub fn original_path(&self) -> Option<PathBuf> {
        (!self.op.trim().is_empty()).then(|| PathBuf::from(&self.op))
    }
}

#[derive(Debug, Clone)]
pub struct TrashStore {
    paths: TrashPaths,
}

impl TrashStore {
    pub fn discover() -> Result<Self> {
        Ok(Self::new(TrashPaths::discover()?))
    }

    pub fn new(paths: TrashPaths) -> Self {
        Self { paths }
    }

    pub fn paths(&self) -> &TrashPaths {
        &self.paths
    }

    pub fn sync(&self) -> Result<Vec<TrashEntry>> {
        fs::create_dir_all(&self.paths.trash_dir)
            .with_context(|| format!("failed to create {}", self.paths.trash_dir.display()))?;
        if let Some(parent) = self.paths.metadata_file.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let mut by_name = BTreeMap::new();
        for entry in read_metadata(&self.paths.metadata_file)? {
            by_name.insert(entry.nm.clone(), entry);
        }

        let discovered = discovered_names(&self.paths.trash_dir)?;
        by_name.retain(|name, _| discovered.contains(name));

        for name in &discovered {
            by_name.entry(name.clone()).or_insert_with(|| TrashEntry {
                ts: now_timestamp(),
                nm: name.clone(),
                op: String::new(),
                tn: String::new(),
            });
        }

        let entries = by_name.into_values().collect::<Vec<_>>();
        write_metadata(&self.paths.metadata_file, &entries)?;
        Ok(entries)
    }

    pub fn trash_file(&self, path: &Path, agent: Agent) -> Result<TrashEntry> {
        let mut entries = self.sync()?;
        fs::create_dir_all(&self.paths.trash_dir)
            .with_context(|| format!("failed to create {}", self.paths.trash_dir.display()))?;

        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .context("session path has no file name")?;
        let occupied = entries
            .iter()
            .map(|entry| entry.nm.as_str())
            .collect::<BTreeSet<_>>();
        let trash_name = disambiguate_name(file_name, &occupied, &self.paths.trash_dir);
        let trash_path = self.paths.trash_dir.join(&trash_name);

        fs::copy(path, &trash_path).with_context(|| {
            format!(
                "failed to copy {} to {}",
                path.display(),
                trash_path.display()
            )
        })?;

        let original_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let entry = TrashEntry {
            ts: now_timestamp(),
            nm: trash_name,
            op: original_path.to_string_lossy().into_owned(),
            tn: agent.as_str().to_owned(),
        };
        entries.push(entry.clone());
        write_metadata(&self.paths.metadata_file, &entries)?;
        fs::remove_file(path)
            .with_context(|| format!("failed to delete original {}", path.display()))?;
        Ok(entry)
    }
}

fn read_metadata(path: &Path) -> Result<Vec<TrashEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut entries = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<TrashEntry>(line) {
            Ok(entry) if !entry.nm.trim().is_empty() => entries.push(entry),
            Ok(_) => warn!(
                "skipping trash metadata entry with empty name at {}:{}",
                path.display(),
                index + 1
            ),
            Err(error) => warn!(
                "skipping malformed trash metadata at {}:{}: {error}",
                path.display(),
                index + 1
            ),
        }
    }
    Ok(entries)
}

fn write_metadata(path: &Path, entries: &[TrashEntry]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut buffer = Vec::new();
    for entry in entries {
        serde_json::to_writer(&mut buffer, entry).context("failed to serialize trash metadata")?;
        buffer.write_all(b"\n")?;
    }
    fs::write(path, buffer).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn discovered_names(trash_dir: &Path) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    let entries = match fs::read_dir(trash_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(names),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", trash_dir.display()))
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warn!(
                    "skipping unreadable trash entry in {}: {error}",
                    trash_dir.display()
                );
                continue;
            }
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                warn!(
                    "skipping trash entry with unreadable file type {}: {error}",
                    entry.path().display()
                );
                continue;
            }
        };
        if !file_type.is_file() {
            continue;
        }
        if let Some(name) = entry
            .file_name()
            .to_str()
            .filter(|name| !name.trim().is_empty())
        {
            names.insert(name.to_owned());
        }
    }

    Ok(names)
}

fn disambiguate_name(base: &str, occupied: &BTreeSet<&str>, trash_dir: &Path) -> String {
    if !occupied.contains(base) && !trash_dir.join(base).exists() {
        return base.to_owned();
    }

    let path = Path::new(base);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(base);
    let extension = path.extension().and_then(|value| value.to_str());
    for index in 1usize.. {
        let candidate = match extension {
            Some(extension) if !extension.is_empty() => format!("{stem}-{index}.{extension}"),
            _ => format!("{stem}-{index}"),
        };
        if !occupied.contains(candidate.as_str()) && !trash_dir.join(&candidate).exists() {
            return candidate;
        }
    }
    unreachable!("unbounded disambiguation loop must return")
}

fn now_timestamp() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::{TrashPaths, TrashStore};
    use crate::parse::Agent;
    use anyhow::Result;
    use tempfile::TempDir;

    #[test]
    fn sync_adds_discovered_files_and_removes_missing_metadata() -> Result<()> {
        let temp = TempDir::new()?;
        let paths = TrashPaths::from_data_root(temp.path());
        std::fs::create_dir_all(&paths.trash_dir)?;
        std::fs::write(paths.trash_dir.join("kept.jsonl"), "{}\n")?;
        std::fs::write(
            &paths.metadata_file,
            r#"{"ts":"old","nm":"missing.jsonl","op":"/tmp/missing.jsonl","tn":"claude"}"#,
        )?;

        let entries = TrashStore::new(paths.clone()).sync()?;

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].nm, "kept.jsonl");
        assert_eq!(entries[0].op, "");
        assert_eq!(entries[0].tn, "");
        Ok(())
    }

    #[test]
    fn trash_file_copies_metadata_then_removes_original() -> Result<()> {
        let temp = TempDir::new()?;
        let paths = TrashPaths::from_data_root(temp.path().join("data"));
        let original = temp.path().join("session.jsonl");
        std::fs::write(&original, "{}\n")?;
        let expected_original = original.canonicalize()?;

        let entry = TrashStore::new(paths.clone()).trash_file(&original, Agent::Codex)?;

        assert!(!original.exists());
        assert_eq!(entry.nm, "session.jsonl");
        assert_eq!(entry.op, expected_original.to_string_lossy());
        assert_eq!(entry.tn, "codex");
        assert_eq!(
            std::fs::read_to_string(paths.trash_dir.join(entry.nm))?,
            "{}\n"
        );
        let metadata = std::fs::read_to_string(paths.metadata_file)?;
        assert!(metadata.contains(r#""tn":"codex""#));
        Ok(())
    }
}
