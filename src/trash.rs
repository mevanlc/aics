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

    pub fn trash_session(&self, path: &Path, agent: Agent) -> Result<TrashEntry> {
        match agent {
            Agent::Antigravity => self.trash_antigravity_bundle(path),
            Agent::Claude | Agent::Codex => self.trash_file(path, agent),
        }
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

    pub fn restore_file(&self, path: &Path) -> Result<PathBuf> {
        let mut entries = self.sync()?;
        let entry_index = find_entry_index(&entries, &self.paths, path)
            .with_context(|| format!("trash metadata not found for {}", path.display()))?;
        if entries[entry_index].agent() == Some(Agent::Antigravity) {
            return self.restore_antigravity_bundle(entries, entry_index);
        }
        let target = entries[entry_index]
            .original_path()
            .with_context(|| format!("original path is unknown for {}", path.display()))?;
        let parent = target.parent().with_context(|| {
            format!(
                "restore target {} has no parent directory",
                target.display()
            )
        })?;

        if !parent.is_dir() {
            anyhow::bail!("restore parent does not exist: {}", parent.display());
        }
        if target.exists() {
            anyhow::bail!("restore target already exists: {}", target.display());
        }

        fs::copy(path, &target).with_context(|| {
            format!(
                "failed to restore {} to {}",
                path.display(),
                target.display()
            )
        })?;
        if let Err(error) = fs::remove_file(path) {
            let _ = fs::remove_file(&target);
            return Err(error).with_context(|| {
                format!("failed to remove restored trash item {}", path.display())
            });
        }

        entries.remove(entry_index);
        if let Err(error) = write_metadata(&self.paths.metadata_file, &entries) {
            warn!(
                "failed to update trash metadata after restoring {}: {error:#}",
                target.display()
            );
        }
        Ok(target)
    }

    pub fn restore_target(&self, path: &Path) -> Result<PathBuf> {
        let entries = self.sync()?;
        let entry = find_entry_index(&entries, &self.paths, path)
            .and_then(|index| entries.get(index))
            .with_context(|| format!("trash metadata not found for {}", path.display()))?;
        let original = entry
            .original_path()
            .with_context(|| format!("original path is unknown for {}", path.display()))?;
        if entry.agent() == Some(Agent::Antigravity) {
            return Ok(AntigravityBundlePaths::from_transcript(&original)?.conversation_dir);
        }
        Ok(original)
    }

    pub fn delete_session(&self, path: &Path, agent: Agent, trashed: bool) -> Result<()> {
        if trashed {
            let mut entries = self.sync()?;
            if let Some(entry_index) = find_entry_index(&entries, &self.paths, path) {
                let trash_path = entries[entry_index].trash_path(&self.paths);
                remove_path(&trash_path)?;
                entries.remove(entry_index);
                if let Err(error) = write_metadata(&self.paths.metadata_file, &entries) {
                    warn!(
                        "failed to update trash metadata after deleting {}: {error:#}",
                        trash_path.display()
                    );
                }
                return Ok(());
            }
        }

        match agent {
            Agent::Antigravity => delete_session_immediately(path, agent),
            Agent::Claude | Agent::Codex => fs::remove_file(path)
                .with_context(|| format!("failed to delete {}", path.display())),
        }
    }

    fn trash_antigravity_bundle(&self, path: &Path) -> Result<TrashEntry> {
        let bundle = AntigravityBundlePaths::from_transcript(path)?;
        let original_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mut entries = self.sync()?;
        fs::create_dir_all(&self.paths.trash_dir)
            .with_context(|| format!("failed to create {}", self.paths.trash_dir.display()))?;

        let base_name = format!("{}.antigravity", bundle.session_id);
        let occupied = entries
            .iter()
            .map(|entry| entry.nm.as_str())
            .collect::<BTreeSet<_>>();
        let trash_name = disambiguate_name(&base_name, &occupied, &self.paths.trash_dir);
        let archive = self.paths.trash_dir.join(&trash_name);
        let archived_bundle = AntigravityBundlePaths::in_archive(&archive, &bundle.session_id);

        let mut moves = vec![(
            bundle.conversation_dir.clone(),
            archived_bundle.conversation_dir.clone(),
        )];
        let database_paths = bundle.database_paths()?;
        moves.extend(database_paths.iter().map(|source| {
            (
                source.clone(),
                archived_bundle
                    .conversations_dir
                    .join(source.file_name().expect("database path has a file name")),
            )
        }));

        let entry = TrashEntry {
            ts: now_timestamp(),
            nm: trash_name,
            op: original_path.to_string_lossy().into_owned(),
            tn: Agent::Antigravity.as_str().to_owned(),
        };
        entries.push(entry.clone());
        write_metadata(&self.paths.metadata_file, &entries)
            .context("failed to record Antigravity trash metadata")?;

        if let Err(error) = move_all_or_rollback(&moves) {
            if moves
                .iter()
                .all(|(source, target)| source.exists() && !target.exists())
            {
                entries.pop();
                let _ = fs::remove_dir_all(&archive);
                if let Err(metadata_error) = write_metadata(&self.paths.metadata_file, &entries) {
                    warn!(
                        "failed to remove rolled-back Antigravity trash metadata for {}: {metadata_error:#}",
                        bundle.session_id
                    );
                }
            }
            return Err(error).with_context(|| {
                format!(
                    "failed to move Antigravity conversation {} to Trash",
                    bundle.session_id
                )
            });
        }

        Ok(entry)
    }

    fn restore_antigravity_bundle(
        &self,
        mut entries: Vec<TrashEntry>,
        entry_index: usize,
    ) -> Result<PathBuf> {
        let entry = &entries[entry_index];
        let original = entry.original_path().with_context(|| {
            format!(
                "original path is unknown for {}",
                entry.trash_path(&self.paths).display()
            )
        })?;
        let bundle = AntigravityBundlePaths::from_transcript(&original)?;
        let archive = entry.trash_path(&self.paths);
        let archived_bundle = AntigravityBundlePaths::in_archive(&archive, &bundle.session_id);
        if bundle.conversation_dir.exists() {
            anyhow::bail!(
                "restore target already exists: {}",
                bundle.conversation_dir.display()
            );
        }

        if let Some(target) = bundle.database_paths()?.first() {
            anyhow::bail!("restore target already exists: {}", target.display());
        }
        let archived_databases = archived_bundle.database_paths()?;

        let mut moves = vec![(
            archived_bundle.conversation_dir,
            bundle.conversation_dir.clone(),
        )];
        moves.extend(archived_databases.into_iter().map(|source| {
            let target = bundle
                .conversations_dir
                .join(source.file_name().expect("database path has a file name"));
            (source, target)
        }));
        move_all_or_rollback(&moves).with_context(|| {
            format!(
                "failed to restore Antigravity conversation {}",
                bundle.session_id
            )
        })?;

        let _ = fs::remove_dir_all(&archive);
        entries.remove(entry_index);
        if let Err(error) = write_metadata(&self.paths.metadata_file, &entries) {
            warn!(
                "failed to update trash metadata after restoring {}: {error:#}",
                bundle.conversation_dir.display()
            );
        }
        Ok(original)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AntigravityBundlePaths {
    pub session_id: String,
    pub home: PathBuf,
    pub conversation_dir: PathBuf,
    pub conversations_dir: PathBuf,
    pub transcript: PathBuf,
}

impl AntigravityBundlePaths {
    pub fn from_transcript(path: &Path) -> Result<Self> {
        let logs = named_parent(path, "transcript.jsonl", "logs")?;
        let generated = named_parent(logs, "logs", ".system_generated")?;
        let conversation_dir = generated
            .parent()
            .context("Antigravity transcript has no conversation directory")?;
        let brain = conversation_dir
            .parent()
            .filter(|parent| parent.file_name().and_then(|name| name.to_str()) == Some("brain"))
            .context("Antigravity conversation is not under a brain directory")?;
        let home = brain
            .parent()
            .context("Antigravity brain directory has no parent")?
            .to_path_buf();
        let session_id = conversation_dir
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .context("Antigravity conversation has no session ID")?
            .to_owned();

        Ok(Self {
            session_id,
            conversations_dir: home.join("conversations"),
            home,
            conversation_dir: conversation_dir.to_path_buf(),
            transcript: path.to_path_buf(),
        })
    }

    pub fn in_archive(archive: &Path, session_id: &str) -> Self {
        let conversation_dir = archive.join("brain").join(session_id);
        Self {
            session_id: session_id.to_owned(),
            home: archive.to_path_buf(),
            conversations_dir: archive.join("conversations"),
            transcript: conversation_dir.join(".system_generated/logs/transcript.jsonl"),
            conversation_dir,
        }
    }

    pub fn database_paths(&self) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        let base = format!("{}.db", self.session_id);
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let path = self.conversations_dir.join(format!("{base}{suffix}"));
            match fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => paths.push(path),
                Ok(_) => warn!(
                    "skipping non-file Antigravity database companion {}",
                    path.display()
                ),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to inspect {}", path.display()))
                }
            }
        }
        Ok(paths)
    }
}

fn named_parent<'a>(path: &'a Path, name: &str, parent_name: &str) -> Result<&'a Path> {
    if path.file_name().and_then(|value| value.to_str()) != Some(name) {
        anyhow::bail!("expected {name} path, got {}", path.display());
    }
    path.parent()
        .filter(|parent| parent.file_name().and_then(|value| value.to_str()) == Some(parent_name))
        .with_context(|| format!("{name} is not under a {parent_name} directory"))
}

fn find_entry_index(entries: &[TrashEntry], paths: &TrashPaths, path: &Path) -> Option<usize> {
    entries.iter().position(|entry| {
        let trash_path = entry.trash_path(paths);
        trash_path == path
            || (entry.agent() == Some(Agent::Antigravity) && path.starts_with(&trash_path))
    })
}

pub fn delete_session_immediately(path: &Path, agent: Agent) -> Result<()> {
    match agent {
        Agent::Antigravity => delete_antigravity_bundle(path),
        Agent::Claude | Agent::Codex => {
            fs::remove_file(path).with_context(|| format!("failed to delete {}", path.display()))
        }
    }
}

fn delete_antigravity_bundle(path: &Path) -> Result<()> {
    let bundle = AntigravityBundlePaths::from_transcript(path)?;
    let database_paths = bundle.database_paths()?;
    let mut removed = false;

    match fs::remove_dir_all(&bundle.conversation_dir) {
        Ok(()) => removed = true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to delete {}", bundle.conversation_dir.display()))
        }
    }
    for database in database_paths {
        fs::remove_file(&database)
            .with_context(|| format!("failed to delete {}", database.display()))?;
        removed = true;
    }

    if !removed {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Antigravity conversation {} is missing", bundle.session_id),
        ))
        .with_context(|| format!("failed to delete {}", path.display()));
    }
    Ok(())
}

fn move_all_or_rollback(moves: &[(PathBuf, PathBuf)]) -> Result<()> {
    for (completed, (source, target)) in moves.iter().enumerate() {
        if let Err(error) = move_path(source, target) {
            let rollback_error = rollback_moves(&moves[..completed]).err();
            if let Some(rollback_error) = rollback_error {
                return Err(error).context(format!("rollback also failed: {rollback_error:#}"));
            }
            return Err(error);
        }
    }
    Ok(())
}

fn rollback_moves(moves: &[(PathBuf, PathBuf)]) -> Result<()> {
    for (source, target) in moves.iter().rev() {
        move_path(target, source).with_context(|| {
            format!(
                "failed to roll back {} to {}",
                target.display(),
                source.display()
            )
        })?;
    }
    Ok(())
}

fn move_path(source: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    match fs::rename(source, target) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() != io::ErrorKind::CrossesDevices => {
            return Err(error).with_context(|| {
                format!(
                    "failed to move {} to {}",
                    source.display(),
                    target.display()
                )
            })
        }
        Err(_) => {}
    }

    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("failed to inspect {}", source.display()))?;
    if metadata.file_type().is_symlink() {
        anyhow::bail!(
            "refusing to move symlink across filesystems: {}",
            source.display()
        );
    }
    if metadata.is_dir() {
        copy_directory(source, target)?;
        if let Err(error) = fs::remove_dir_all(source) {
            let _ = fs::remove_dir_all(target);
            return Err(error)
                .with_context(|| format!("failed to remove {} after copy", source.display()));
        }
    } else if metadata.is_file() {
        fs::copy(source, target).with_context(|| {
            format!(
                "failed to copy {} to {}",
                source.display(),
                target.display()
            )
        })?;
        if let Err(error) = fs::remove_file(source) {
            let _ = fs::remove_file(target);
            return Err(error)
                .with_context(|| format!("failed to remove {} after copy", source.display()));
        }
    } else {
        anyhow::bail!("unsupported Antigravity bundle entry: {}", source.display());
    }
    Ok(())
}

fn copy_directory(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir(target)
        .with_context(|| format!("failed to create directory {}", target.display()))?;
    let result = (|| {
        for entry in fs::read_dir(source)
            .with_context(|| format!("failed to read directory {}", source.display()))?
        {
            let entry = entry.with_context(|| format!("failed to read {}", source.display()))?;
            let source_path = entry.path();
            let target_path = target.join(entry.file_name());
            let file_type = entry.file_type().with_context(|| {
                format!("failed to inspect bundle entry {}", source_path.display())
            })?;
            if file_type.is_symlink() {
                anyhow::bail!(
                    "refusing to copy symlink in Antigravity bundle: {}",
                    source_path.display()
                );
            }
            if file_type.is_dir() {
                copy_directory(&source_path, &target_path)?;
            } else if file_type.is_file() {
                fs::copy(&source_path, &target_path).with_context(|| {
                    format!(
                        "failed to copy {} to {}",
                        source_path.display(),
                        target_path.display()
                    )
                })?;
            } else {
                anyhow::bail!("unsupported bundle entry: {}", source_path.display());
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(target);
    }
    result
}

fn remove_path(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if metadata.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("failed to delete {}", path.display()))
    } else {
        fs::remove_file(path).with_context(|| format!("failed to delete {}", path.display()))
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
        if !file_type.is_file()
            && !(file_type.is_dir()
                && entry
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    == Some("antigravity"))
        {
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
    use std::path::{Path, PathBuf};

    use super::{delete_session_immediately, TrashPaths, TrashStore};
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

    #[test]
    fn restore_file_returns_session_to_original_path() -> Result<()> {
        let temp = TempDir::new()?;
        let paths = TrashPaths::from_data_root(temp.path().join("data"));
        let original = temp.path().join("session.jsonl");
        std::fs::write(&original, "{}\n")?;
        let store = TrashStore::new(paths.clone());
        let entry = store.trash_file(&original, Agent::Codex)?;
        let trashed = entry.trash_path(&paths);

        let restored = store.restore_file(&trashed)?;

        assert_eq!(restored, original.canonicalize()?);
        assert_eq!(std::fs::read_to_string(&restored)?, "{}\n");
        assert!(!trashed.exists());
        assert_eq!(std::fs::read_to_string(paths.metadata_file)?, "");
        Ok(())
    }

    #[test]
    fn trash_and_restore_antigravity_bundle_preserves_database_and_artifacts() -> Result<()> {
        let temp = TempDir::new()?;
        let paths = TrashPaths::from_data_root(temp.path().join("data"));
        let transcript = create_antigravity_bundle(temp.path(), "conversation-123")?;
        let expected_transcript = transcript.canonicalize()?;
        let conversation = temp.path().join("brain/conversation-123");
        let database = temp.path().join("conversations/conversation-123.db");
        let wal = temp.path().join("conversations/conversation-123.db-wal");
        let store = TrashStore::new(paths.clone());

        let entry = store.trash_session(&transcript, Agent::Antigravity)?;
        let archive = entry.trash_path(&paths);
        let archived_transcript =
            archive.join("brain/conversation-123/.system_generated/logs/transcript.jsonl");

        assert!(!conversation.exists());
        assert!(!database.exists());
        assert!(!wal.exists());
        assert!(archived_transcript.exists());
        assert!(archive.join("brain/conversation-123/artifact.md").exists());
        assert!(archive.join("conversations/conversation-123.db").exists());
        assert!(archive
            .join("conversations/conversation-123.db-wal")
            .exists());

        let restored = store.restore_file(&archived_transcript)?;

        assert_eq!(restored, expected_transcript);
        assert!(conversation.join("artifact.md").exists());
        assert_eq!(std::fs::read_to_string(database)?, "database");
        assert_eq!(std::fs::read_to_string(wal)?, "wal");
        assert!(!archive.exists());
        assert_eq!(std::fs::read_to_string(paths.metadata_file)?, "");
        Ok(())
    }

    #[test]
    fn delete_antigravity_session_removes_bundle_and_database_companions() -> Result<()> {
        let temp = TempDir::new()?;
        let transcript = create_antigravity_bundle(temp.path(), "conversation-456")?;

        delete_session_immediately(&transcript, Agent::Antigravity)?;

        assert!(!temp.path().join("brain/conversation-456").exists());
        assert!(!temp
            .path()
            .join("conversations/conversation-456.db")
            .exists());
        assert!(!temp
            .path()
            .join("conversations/conversation-456.db-wal")
            .exists());
        Ok(())
    }

    #[test]
    fn delete_trashed_antigravity_session_removes_archive_and_metadata() -> Result<()> {
        let temp = TempDir::new()?;
        let paths = TrashPaths::from_data_root(temp.path().join("data"));
        let transcript = create_antigravity_bundle(temp.path(), "conversation-789")?;
        let store = TrashStore::new(paths.clone());
        let entry = store.trash_session(&transcript, Agent::Antigravity)?;
        let archive = entry.trash_path(&paths);
        let archived_transcript =
            archive.join("brain/conversation-789/.system_generated/logs/transcript.jsonl");

        store.delete_session(&archived_transcript, Agent::Antigravity, true)?;

        assert!(!archive.exists());
        assert_eq!(std::fs::read_to_string(paths.metadata_file)?, "");
        assert!(!temp.path().join("brain/conversation-789").exists());
        assert!(!temp
            .path()
            .join("conversations/conversation-789.db")
            .exists());
        Ok(())
    }

    fn create_antigravity_bundle(home: &Path, session_id: &str) -> Result<PathBuf> {
        let conversation = home.join("brain").join(session_id);
        let transcript = conversation.join(".system_generated/logs/transcript.jsonl");
        let conversations = home.join("conversations");
        std::fs::create_dir_all(transcript.parent().expect("transcript has a parent"))?;
        std::fs::create_dir_all(&conversations)?;
        std::fs::write(&transcript, "{}\n")?;
        std::fs::write(conversation.join("artifact.md"), "artifact")?;
        std::fs::write(conversations.join(format!("{session_id}.db")), "database")?;
        std::fs::write(conversations.join(format!("{session_id}.db-wal")), "wal")?;
        Ok(transcript)
    }
}
