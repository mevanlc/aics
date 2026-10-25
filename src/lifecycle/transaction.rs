//! Recoverable, no-overwrite publication of approved lifecycle groups.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::file_identity::{identity, FileIdentity};
use super::{DependencyGraph, LifecyclePlan, Operation};
use crate::live::LiveSessionTracker;
use crate::parse::Agent;
use crate::scan::SessionRoots;
use crate::trash::{AntigravityBundlePaths, TrashEntry, TrashReason, TrashRecord};

#[derive(Serialize, Deserialize)]
struct Transfer {
    source: PathBuf,
    target: Option<PathBuf>,
    backup: PathBuf,
    digest: String,
    published_identity: Option<FileIdentity>,
}

#[derive(Serialize, Deserialize)]
struct MetadataChange {
    path: PathBuf,
    before: Option<Vec<u8>>,
    after: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    committed: bool,
    plan: LifecyclePlan,
    transfers: Vec<Transfer>,
    metadata: Vec<MetadataChange>,
}

fn lock(roots: &SessionRoots) -> Result<(File, PathBuf)> {
    let paths = roots
        .trash
        .as_ref()
        .context("trash store is unavailable; refusing unprotected removal")?;
    create_directories(&paths.data_root)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(paths.data_root.join("lifecycle.lock"))?;
    file.try_lock()
        .context("another AICS lifecycle operation is running")?;
    let operations = paths.data_root.join("operations");
    create_directories(&operations)?;
    Ok((file, operations))
}

#[derive(Debug)]
pub(crate) struct RecoveryRequired;

impl std::fmt::Display for RecoveryRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an interrupted lifecycle operation awaits recovery; open the TUI to recover before refreshing the index")
    }
}
impl std::error::Error for RecoveryRequired {}

/// Readers share this guard while refreshing, and never recover source files.
pub(crate) fn read_guard(roots: &SessionRoots, wait: bool) -> Result<Option<File>> {
    let Some(paths) = &roots.trash else {
        return Ok(None);
    };
    create_directories(&paths.data_root)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(paths.data_root.join("lifecycle.lock"))?;
    if wait {
        file.lock_shared()?;
    } else {
        file.try_lock_shared()?;
    }
    match fs::read_dir(paths.data_root.join("operations")) {
        Ok(mut entries) => {
            if entries.next().transpose()?.is_some() {
                return Err(RecoveryRequired.into());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(Some(file))
}

fn guards(
    roots: &SessionRoots,
    plan: &LifecyclePlan,
) -> Result<Vec<crate::live::LiveSessionGuard>> {
    let tracker = LiveSessionTracker::new(roots.live_sessions.clone());
    let mut guards = Vec::new();
    for agent in [Agent::Claude, Agent::Codex, Agent::Antigravity] {
        let ids = plan
            .members
            .values()
            .filter(|m| m.agent == agent)
            .map(|m| m.thread_id.clone())
            .collect::<Vec<_>>();
        if !ids.is_empty() {
            guards.push(
                tracker
                    .acquire_group_guard(agent, &ids)
                    .map_err(|e| anyhow::anyhow!("{}", e.message()))?,
            );
        }
    }
    Ok(guards)
}

pub fn recover(roots: &SessionRoots) -> Result<()> {
    let Some(paths) = &roots.trash else {
        return Ok(());
    };
    match fs::read_dir(paths.data_root.join("operations")) {
        Ok(mut entries) => {
            if entries.next().transpose()?.is_none() {
                return Ok(());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let (_lock, operations) = lock(roots)?;
    recover_locked(roots, &operations)
}

fn recover_locked(roots: &SessionRoots, operations: &Path) -> Result<()> {
    for entry in fs::read_dir(operations)? {
        let path = entry?.path();
        let journal_path = path.join("operation.json");
        if !journal_path.exists() {
            // Staging did not reach the durable journal; no originals were touched.
            if path.is_dir() {
                fs::remove_dir_all(&path)?;
            }
            continue;
        }
        let journal: Journal = serde_json::from_slice(&fs::read(&journal_path)?)
            .with_context(|| format!("unreadable recovery record {}", journal_path.display()))?;
        if !journal.committed {
            let _guards = guards(roots, &journal.plan)?;
            rollback(&journal).with_context(|| {
                format!(
                    "incomplete lifecycle recovery; preserved backups at {}",
                    path.display()
                )
            })?;
        }
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

pub fn execute(
    roots: &SessionRoots,
    approved: &LifecyclePlan,
    reasons: &BTreeMap<PathBuf, TrashReason>,
) -> Result<()> {
    approved.require_safe()?;
    let (_lock, operations) = lock(roots)?;
    recover_locked(roots, &operations)?;
    let _guards = guards(roots, approved)?;
    let agent = approved
        .members
        .values()
        .next()
        .context("empty lifecycle group")?
        .agent;
    let seeds = approved.seeds.iter().cloned().collect::<Vec<_>>();
    let fresh = DependencyGraph::discover(roots, agent, &seeds)?
        .plan(approved.seeds.clone(), approved.operation);
    fresh.require_safe()?;
    if &fresh != approved {
        bail!("session group changed; review the updated group before trying again");
    }
    let id = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    let directory = operations.join(id);
    fs::create_dir(&directory)?;
    sync_directory(&operations)?;
    let result = prepare(roots, approved, reasons, &directory);
    let mut journal = match result {
        Ok(journal) => journal,
        Err(e) => {
            let _ = fs::remove_dir_all(&directory);
            return Err(e);
        }
    };
    save(&directory, &journal)?;
    let result = apply(&mut journal, &directory);
    if let Err(error) = result {
        if let Err(recovery) = rollback(&journal) {
            bail!(
                "{error:#}; rollback incomplete: {recovery:#}; backups retained at {}",
                directory.display()
            );
        }
        fs::remove_dir_all(&directory)?;
        return Err(error.context("group operation rolled back"));
    }
    journal.committed = true;
    save(&directory, &journal)?;
    cleanup_archives(roots, &journal.plan);
    fs::remove_dir_all(directory)
        .context("operation completed; staged backup cleanup will retry on next startup")?;
    Ok(())
}

fn cleanup_archives(roots: &SessionRoots, plan: &LifecyclePlan) {
    if plan.operation != Operation::Restore {
        return;
    }
    let Some(paths) = &roots.trash else {
        return;
    };
    for m in plan
        .members
        .values()
        .filter(|m| m.agent == Agent::Antigravity)
    {
        if let Ok(bundle) = AntigravityBundlePaths::from_transcript(&m.path) {
            if let Some(archive) = bundle.conversation_dir.parent().and_then(Path::parent) {
                if archive.parent() == Some(paths.trash_dir.as_path()) {
                    let _ = fs::remove_dir(archive.join("brain"));
                    let _ = fs::remove_dir(archive.join("conversations"));
                    let _ = fs::remove_dir(archive);
                }
            }
        }
    }
}

fn bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn prepare(
    roots: &SessionRoots,
    plan: &LifecyclePlan,
    reasons: &BTreeMap<PathBuf, TrashReason>,
    dir: &Path,
) -> Result<Journal> {
    let paths = roots.trash.as_ref().context("trash store unavailable")?;
    let old_entries = bytes(&paths.metadata_file)?;
    let mut entries: Vec<TrashEntry> = match &old_entries {
        Some(data) => std::str::from_utf8(data)?
            .lines()
            .filter(|s| !s.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()?,
        None => Vec::new(),
    };
    let old_reasons = bytes(&paths.reasons_file)?;
    let mut records: BTreeMap<String, TrashRecord> = old_reasons
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()?
        .unwrap_or_default();
    let mut occupied = entries
        .iter()
        .map(|e| e.nm.clone())
        .collect::<BTreeSet<_>>();
    let mut transfers = Vec::new();
    for m in plan.members.values() {
        let existing = entries
            .iter()
            .find(|e| {
                m.path == e.trash_path(paths)
                    || (m.agent == Agent::Antigravity && m.path.starts_with(e.trash_path(paths)))
            })
            .cloned();
        let target = match plan.operation {
            Operation::Trash => {
                let base = if m.agent == Agent::Antigravity {
                    format!("{}.antigravity", m.thread_id)
                } else {
                    m.path
                        .file_name()
                        .context("missing filename")?
                        .to_string_lossy()
                        .into_owned()
                };
                let mut name = base.clone();
                let mut n = 1;
                while occupied.contains(&name) || paths.trash_dir.join(&name).try_exists()? {
                    name = format!("{n}-{base}");
                    n += 1;
                }
                occupied.insert(name.clone());
                let entry = TrashEntry {
                    ts: chrono::Utc::now().to_rfc3339(),
                    nm: name.clone(),
                    op: m.original_path.display().to_string(),
                    tn: m.agent.as_str().into(),
                };
                records.insert(
                    name,
                    TrashRecord {
                        trashed_at: entry.ts.clone(),
                        original_path: entry.op.clone(),
                        agent: entry.tn.clone(),
                        reason: reasons
                            .get(&m.path)
                            .cloned()
                            .context("missing action provenance")?,
                    },
                );
                let target = entry.trash_path(paths);
                entries.push(entry);
                Some(target)
            }
            Operation::Restore => Some(m.original_path.clone()),
            Operation::Delete => None,
        };
        if plan.operation != Operation::Trash && m.trashed {
            let entry = existing.as_ref().context("trash metadata disappeared")?;
            entries.retain(|e| e.nm != entry.nm);
            records.remove(&entry.nm);
        }
        let mut pairs = Vec::new();
        if m.agent == Agent::Antigravity {
            let source = AntigravityBundlePaths::from_transcript(&m.path)?;
            match plan.operation {
                Operation::Trash => {
                    let dest = AntigravityBundlePaths::in_archive(
                        target.as_ref().context("missing bundle target")?,
                        &m.thread_id,
                    );
                    pairs.push((source.conversation_dir.clone(), Some(dest.conversation_dir)));
                    for db in source.database_paths()? {
                        pairs.push((
                            db.clone(),
                            Some(
                                dest.conversations_dir
                                    .join(db.file_name().context("invalid database filename")?),
                            ),
                        ));
                    }
                }
                Operation::Restore => {
                    let dest = AntigravityBundlePaths::from_transcript(&m.original_path)?;
                    pairs.push((source.conversation_dir.clone(), Some(dest.conversation_dir)));
                    for db in source.database_paths()? {
                        pairs.push((
                            db.clone(),
                            Some(
                                dest.conversations_dir
                                    .join(db.file_name().context("invalid database filename")?),
                            ),
                        ));
                    }
                }
                Operation::Delete => {
                    if let Some(entry) = existing {
                        pairs.push((entry.trash_path(paths), None));
                    } else {
                        pairs.push((source.conversation_dir.clone(), None));
                        for db in source.database_paths()? {
                            pairs.push((db, None));
                        }
                    }
                }
            }
        } else {
            pairs.push((m.path.clone(), target));
        }
        for (source, target) in pairs {
            if let Some(target) = &target {
                if target.try_exists()? {
                    bail!("destination already exists: {}", target.display());
                }
            }
            let backup = dir.join(format!("source-{}", transfers.len()));
            let before = digest(&source)?;
            copy_new(&source, &backup)?;
            if digest(&backup)? != before || digest(&source)? != before {
                bail!("source changed while staging {}", source.display());
            }
            transfers.push(Transfer {
                source,
                target,
                backup,
                digest: before,
                published_identity: None,
            });
        }
    }
    let mut after_entries = Vec::new();
    for entry in entries {
        serde_json::to_writer(&mut after_entries, &entry)?;
        after_entries.push(b'\n');
    }
    Ok(Journal {
        committed: false,
        plan: plan.clone(),
        transfers,
        metadata: vec![
            MetadataChange {
                path: paths.metadata_file.clone(),
                before: old_entries,
                after: after_entries,
            },
            MetadataChange {
                path: paths.reasons_file.clone(),
                before: old_reasons,
                after: serde_json::to_vec_pretty(&records)?,
            },
        ],
    })
}

fn apply(journal: &mut Journal, directory: &Path) -> Result<()> {
    // All originals remain available until every destination is published and verified.
    for index in 0..journal.transfers.len() {
        let transfer = &mut journal.transfers[index];
        if digest(&transfer.source)? != transfer.digest {
            bail!("source changed: {}", transfer.source.display());
        }
        if let Some(target) = &transfer.target {
            copy_new(&transfer.backup, target)?;
            transfer.published_identity = identity(target)?;
            save(directory, journal)?;
        }
    }
    for change in &journal.metadata {
        if bytes(&change.path)? != change.before {
            bail!("trash metadata changed during operation");
        }
        atomic_write(&change.path, &change.after)?;
    }
    for (index, transfer) in journal.transfers.iter().enumerate() {
        if digest(&transfer.source)? != transfer.digest {
            bail!(
                "source changed before removal: {}",
                transfer.source.display()
            );
        }
        remove(&transfer.source)?;
        #[cfg(test)]
        if FAIL_AFTER_REMOVAL.with(|value| value.get()) == Some(index) {
            bail!("injected removal failure");
        }
        #[cfg(not(test))]
        let _ = index;
    }
    Ok(())
}

fn rollback(journal: &Journal) -> Result<()> {
    for transfer in &journal.transfers {
        if digest(&transfer.backup)? != transfer.digest {
            bail!("staged backup changed: {}", transfer.backup.display());
        }
        if transfer.source.try_exists()? {
            if digest(&transfer.source)? != transfer.digest {
                bail!(
                    "source was changed externally: {}",
                    transfer.source.display()
                );
            }
        } else {
            copy_new(&transfer.backup, &transfer.source)?;
        }
    }
    for transfer in &journal.transfers {
        if let Some(target) = &transfer.target {
            if target.try_exists()? {
                if transfer.published_identity.is_none()
                    || identity(target)? != transfer.published_identity
                    || digest(target)? != transfer.digest
                {
                    bail!(
                        "destination changed or is incomplete; preserved {}",
                        target.display()
                    );
                }
                remove(target)?;
            }
        }
    }
    for change in &journal.metadata {
        let current = bytes(&change.path)?;
        if current == change.before {
            continue;
        }
        if current.as_deref() != Some(change.after.as_slice()) {
            bail!(
                "trash metadata changed externally: {}",
                change.path.display()
            );
        }
        if let Some(before) = &change.before {
            atomic_write(&change.path, before)?;
        } else {
            fs::remove_file(&change.path)?;
        }
    }
    Ok(())
}

fn save(directory: &Path, journal: &Journal) -> Result<()> {
    atomic_write(
        &directory.join("operation.json"),
        &serde_json::to_vec_pretty(journal)?,
    )
}

fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path.parent().context("missing parent")?;
    create_directories(parent)?;
    let temporary = path.with_extension("lifecycle-tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)?;
    file.write_all(data)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    })?
    .sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn create_directories(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_dir() {
        return Ok(());
    }
    let parent = path.parent().context("missing directory parent")?;
    create_directories(parent)?;
    match fs::create_dir(path) {
        Ok(()) => sync_directory(parent),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn copy_new(source: &Path, target: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.is_symlink() {
        bail!("refusing symlink {}", source.display());
    }
    create_directories(target.parent().context("missing destination parent")?)?;
    if metadata.is_dir() {
        fs::create_dir(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_new(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else if metadata.is_file() {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        std::io::copy(&mut File::open(source)?, &mut output)?;
        output.set_permissions(metadata.permissions())?;
        output.sync_all()?;
    } else {
        bail!("unsupported source {}", source.display());
    }
    #[cfg(unix)]
    {
        if metadata.is_dir() {
            File::open(target)?.sync_all()?;
        }
        File::open(target.parent().context("missing destination parent")?)?.sync_all()?;
    }
    Ok(())
}

fn remove(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    sync_directory(path.parent().context("missing source parent")?)?;
    Ok(())
}

fn digest(path: &Path) -> Result<String> {
    fn hash(path: &Path, output: &mut Sha256) -> Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_symlink() {
            bail!("refusing symlink {}", path.display());
        }
        if metadata.is_dir() {
            output.update(b"dir\0");
            let mut entries = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                output.update(entry.file_name().as_encoded_bytes());
                output.update(b"\0");
                hash(&entry.path(), output)?;
            }
        } else if metadata.is_file() {
            output.update(b"file\0");
            let mut input = File::open(path)?;
            let mut buffer = [0; 65536];
            loop {
                let n = input.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                output.update(&buffer[..n]);
            }
        } else {
            bail!("unsupported source {}", path.display());
        }
        Ok(())
    }
    let mut output = Sha256::new();
    hash(path, &mut output)?;
    Ok(output
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
thread_local! { static FAIL_AFTER_REMOVAL: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) }; }

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::trash::TrashPaths;
    use tempfile::TempDir;

    fn fixture() -> Result<(TempDir, SessionRoots, LifecyclePlan)> {
        let temp = TempDir::new()?;
        let roots = SessionRoots {
            live_sessions: Default::default(),
            claude_projects: temp.path().join("claude"),
            codex_sessions: temp.path().join("codex/sessions"),
            antigravity_home: temp.path().join("antigravity"),
            trash: Some(TrashPaths::from_data_root(temp.path().join("data"))),
        };
        fs::create_dir_all(&roots.codex_sessions)?;
        let parent = roots.codex_sessions.join("parent.jsonl");
        fs::write(
            &parent,
            r#"{"type":"session_meta","payload":{"id":"parent"}}"#,
        )?;
        fs::write(
            roots.codex_sessions.join("child.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"child","history_base":{"thread_id":"parent"}}}"#,
        )?;
        let plan = DependencyGraph::discover(&roots, Agent::Codex, &[])?
            .plan(BTreeSet::from([parent]), Operation::Trash);
        Ok((temp, roots, plan))
    }

    fn reasons(plan: &LifecyclePlan) -> BTreeMap<PathBuf, TrashReason> {
        plan.members
            .keys()
            .map(|p| (p.clone(), TrashReason::KeyBinding { key: "test".into() }))
            .collect()
    }

    #[test]
    fn failures_after_removal_roll_back_trash_restore_and_purge() -> Result<()> {
        let (_temp, roots, plan) = fixture()?;
        for operation in [Operation::Trash, Operation::Restore, Operation::Delete] {
            let plan = if operation == Operation::Trash {
                plan.clone()
            } else {
                DependencyGraph::discover(&roots, Agent::Codex, &[])?.plan(
                    BTreeSet::from([roots.trash.as_ref().unwrap().trash_dir.join("parent.jsonl")]),
                    operation,
                )
            };
            let before = plan
                .members
                .keys()
                .map(|p| Ok((p.clone(), fs::read(p)?)))
                .collect::<Result<BTreeMap<_, _>>>()?;
            let metadata_before = bytes(&roots.trash.as_ref().unwrap().metadata_file)?;
            FAIL_AFTER_REMOVAL.with(|value| value.set(Some(0)));
            let result = execute(&roots, &plan, &reasons(&plan));
            FAIL_AFTER_REMOVAL.with(|value| value.set(None));
            assert!(result.unwrap_err().to_string().contains("rolled back"));
            for (path, contents) in before {
                assert_eq!(fs::read(path)?, contents);
            }
            assert_eq!(
                bytes(&roots.trash.as_ref().unwrap().metadata_file)?,
                metadata_before
            );
            if operation == Operation::Trash {
                let fresh = DependencyGraph::discover(&roots, Agent::Codex, &[])?
                    .plan(plan.seeds, operation);
                execute(&roots, &fresh, &reasons(&fresh))?;
            }
        }
        Ok(())
    }

    #[test]
    fn startup_recovers_a_durable_journal_after_partial_removal() -> Result<()> {
        let (_temp, roots, plan) = fixture()?;
        let (_lock, operations) = lock(&roots)?;
        let directory = operations.join("interrupted");
        fs::create_dir(&directory)?;
        let mut journal = prepare(&roots, &plan, &reasons(&plan), &directory)?;
        save(&directory, &journal)?;
        FAIL_AFTER_REMOVAL.with(|value| value.set(Some(0)));
        let result = apply(&mut journal, &directory);
        FAIL_AFTER_REMOVAL.with(|value| value.set(None));
        assert!(result.is_err());
        assert!(!journal.transfers[0].source.exists());
        drop(_lock);
        recover(&roots)?;
        for transfer in &journal.transfers {
            assert_eq!(digest(&transfer.source)?, transfer.digest);
            assert!(!transfer.target.as_ref().unwrap().exists());
        }
        assert!(!directory.exists());
        Ok(())
    }

    #[test]
    fn rollback_preserves_an_external_destination_even_with_identical_bytes() -> Result<()> {
        let (_temp, roots, plan) = fixture()?;
        let (_lock, operations) = lock(&roots)?;
        let directory = operations.join("collision");
        fs::create_dir(&directory)?;
        let mut journal = prepare(&roots, &plan, &reasons(&plan), &directory)?;
        save(&directory, &journal)?;
        let target = journal.transfers[0].target.clone().unwrap();
        copy_new(&journal.transfers[0].backup, &target)?;
        assert!(apply(&mut journal, &directory).is_err());
        assert!(rollback(&journal).is_err());
        assert!(target.exists());
        assert!(journal
            .transfers
            .iter()
            .all(|t| t.source.exists() && t.backup.exists()));
        Ok(())
    }
}
