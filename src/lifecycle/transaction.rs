//! Recoverable publication and retirement of session files and trash items.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::{Builder, TempDir};

use super::file_identity::{identity, FileIdentity};
use super::{DependencyGraph, LifecyclePlan, Operation};
use crate::live::LiveSessionTracker;
use crate::parse::Agent;
use crate::scan::SessionRoots;
use crate::trash::{self, exists, AntigravityBundlePaths, TrashMetadata, TrashReason, TrashStore};

mod archive;
mod legacy;
pub use archive::archive_trash;

const JOURNAL: &str = "moves.json";
const JOURNAL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stamp {
    identity: FileIdentity,
    digest: [u8; 32],
}

impl Stamp {
    fn read(path: &Path) -> Result<Self> {
        Ok(Self {
            identity: entry_identity(path)?,
            digest: digest(path)?,
        })
    }

    fn verify(&self, path: &Path) -> Result<()> {
        ensure!(
            entry_identity(path)? == self.identity && digest(path)? == self.digest,
            "entry changed: {}",
            path.display()
        );
        Ok(())
    }
}

fn entry_identity(path: &Path) -> Result<FileIdentity> {
    identity(path)?.with_context(|| format!("file identity unavailable for {}", path.display()))
}

#[derive(Serialize, Deserialize)]
struct Move {
    from: PathBuf,
    to: PathBuf,
    stamp: Stamp,
}

impl Move {
    fn apply(&self) -> Result<()> {
        self.stamp.verify(&self.from)?;
        move_exclusive(&self.from, &self.to)?;
        self.stamp.verify(&self.to)
    }

    fn undo(&self, retired_source: bool) -> Result<()> {
        if !exists(&self.to)? {
            ensure!(
                !retired_source || exists(&self.from)?,
                "original and staged backup are both missing: {}",
                self.from.display()
            );
            return Ok(());
        }
        if exists(&self.from)? {
            // The rename never happened if the stamped object is still at its
            // original path. In particular, leave a competing destination alone.
            return self.stamp.verify(&self.from);
        }
        self.stamp.verify(&self.to)?;
        move_exclusive(&self.to, &self.from)
    }
}

#[derive(Serialize, Deserialize)]
struct Scratch {
    path: PathBuf,
    identity: FileIdentity,
}

#[derive(Serialize, Deserialize)]
struct VerifiedArchive {
    path: PathBuf,
    stamp: Stamp,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    version: u32,
    committed: bool,
    plans: Vec<LifecyclePlan>,
    publications: Vec<Move>,
    retirements: Vec<Move>,
    scratch: Vec<Scratch>,
    archive: Option<VerifiedArchive>,
}

impl Journal {
    fn save(&self, directory: &Path) -> Result<()> {
        atomic_write(&directory.join(JOURNAL), &serde_json::to_vec(self)?)
    }

    fn rollback(&self) -> Result<()> {
        for entry in self.retirements.iter().rev() {
            entry.undo(true)?;
        }
        for entry in self.publications.iter().rev() {
            entry.undo(false)?;
        }
        Ok(())
    }

    fn cleanup(&self, directory: &Path) -> Result<()> {
        // After an archive purge commits, its ZIP is the surviving copy.
        // Preserve any remaining backups if that copy is no longer verifiable.
        if let Some(archive) = &self.archive {
            if self.committed && self.scratch.iter().any(|s| s.path.exists()) {
                archive.stamp.verify(&archive.path)?;
            }
        }
        for scratch in &self.scratch {
            if exists(&scratch.path)? {
                ensure!(
                    entry_identity(&scratch.path)? == scratch.identity,
                    "scratch directory changed: {}",
                    scratch.path.display()
                );
                remove(&scratch.path)?;
            }
        }
        remove(directory)
    }
}

struct Staging {
    journal: Journal,
    temporary: Vec<TempDir>,
}

impl Staging {
    fn new(plans: Vec<LifecyclePlan>) -> Self {
        Self {
            journal: Journal {
                version: JOURNAL_VERSION,
                committed: false,
                plans,
                publications: Vec::new(),
                retirements: Vec::new(),
                scratch: Vec::new(),
                archive: None,
            },
            temporary: Vec::new(),
        }
    }

    fn scratch(&mut self, parent: &Path) -> Result<PathBuf> {
        let temp = scratch_directory(parent)?;
        let path = temp.path().to_path_buf();
        self.journal.scratch.push(Scratch {
            identity: entry_identity(&path)?,
            path: path.clone(),
        });
        self.temporary.push(temp);
        Ok(path)
    }

    fn publish(&mut self, from: PathBuf, to: PathBuf) -> Result<()> {
        ensure!(
            !exists(&to)?,
            "destination already exists: {}",
            to.display()
        );
        ensure!(
            !self.journal.publications.iter().any(|entry| entry.to == to),
            "duplicate publication destination {}",
            to.display()
        );
        self.journal.publications.push(Move {
            stamp: Stamp::read(&from)?,
            from,
            to,
        });
        Ok(())
    }

    fn publish_copy(&mut self, source: &Path, target: PathBuf) -> Result<Stamp> {
        let scratch = self.scratch(parent(&target)?)?;
        let staged = scratch.join("payload");
        let stamp = copy_verified(source, &staged)?;
        self.publish(staged, target)?;
        Ok(stamp)
    }

    fn retire(&mut self, source: PathBuf, stamp: Stamp) -> Result<()> {
        ensure!(
            !self
                .journal
                .retirements
                .iter()
                .any(|entry| entry.from == source),
            "duplicate retirement source {}",
            source.display()
        );
        let scratch = self.scratch(parent(&source)?)?;
        self.journal.retirements.push(Move {
            from: source,
            to: scratch.join("original"),
            stamp,
        });
        Ok(())
    }

    fn publish_item(
        &mut self,
        root: &Path,
        metadata: &TrashMetadata,
        copies: &[(PathBuf, PathBuf)],
    ) -> Result<()> {
        let scratch = self.scratch(root)?;
        let staged = scratch.join("item");
        create_directories(&staged)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&staged, fs::Permissions::from_mode(0o700))?;
        }
        let mut sources = Vec::new();
        for (source, relative) in copies {
            sources.push((
                source.clone(),
                copy_verified(source, &staged.join(relative))?,
            ));
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(staged.join(trash::METADATA_NAME))?;
        serde_json::to_writer_pretty(&mut file, metadata)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        sync_directory(&staged)?;
        let suffix = scratch
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix(trash::SCRATCH_PREFIX))
            .context("invalid scratch name")?;
        self.publish(staged, root.join(format!("{}{suffix}", trash::ITEM_PREFIX)))?;
        for (source, stamp) in sources {
            self.retire(source, stamp)?;
        }
        Ok(())
    }

    fn persist(mut self, operations: &Path) -> Result<(PathBuf, Journal)> {
        let directory = Builder::new().prefix("txn-").tempdir_in(operations)?;
        sync_directory(operations)?;
        // Once the record is durable, scratch must survive unwinding and exit.
        // Keep before saving so an uncertain fsync result cannot discard it.
        let directory = directory.keep();
        for temp in self.temporary.drain(..) {
            let _ = temp.keep();
        }
        self.journal.save(&directory).with_context(|| {
            format!(
                "could not persist lifecycle journal at {}",
                directory.display()
            )
        })?;
        Ok((directory, self.journal))
    }

    fn execute(self, operations: &Path) -> Result<()> {
        if self.journal.publications.is_empty() && self.journal.retirements.is_empty() {
            return Ok(());
        }
        let (directory, mut journal) = self.persist(operations)?;
        let apply = (|| -> Result<()> {
            if let Some(archive) = &journal.archive {
                archive.stamp.verify(&archive.path)?;
            }
            for entry in &journal.publications {
                entry.apply()?;
            }
            for entry in &journal.retirements {
                entry.apply()?;
                #[cfg(test)]
                if FAIL_AFTER_RETIREMENT.with(|flag| flag.get()) {
                    bail!("injected removal failure");
                }
            }
            if let Some(archive) = &journal.archive {
                archive.stamp.verify(&archive.path)?;
            }
            Ok(())
        })();
        if let Err(error) = apply {
            journal
                .rollback()
                .and_then(|()| journal.cleanup(&directory))
                .with_context(|| {
                    format!(
                        "{error:#}; rollback incomplete, recovery record at {}",
                        directory.display()
                    )
                })?;
            return Err(error.context("operation rolled back"));
        }
        journal.committed = true;
        // If saving fails, the on-disk journal determines recovery. Both
        // originals and publications are still recoverable at this point.
        journal.save(&directory)?;
        journal
            .cleanup(&directory)
            .context("operation committed; cleanup will retry on next startup")
    }
}

fn parent(path: &Path) -> Result<&Path> {
    path.parent()
        .with_context(|| format!("no parent for {}", path.display()))
}

fn scratch_directory(parent: &Path) -> Result<TempDir> {
    create_directories(parent)?;
    let scratch = Builder::new()
        .prefix(trash::SCRATCH_PREFIX)
        .rand_bytes(trash::ITEM_ID_LEN)
        .tempdir_in(parent)?;
    sync_directory(parent)?;
    Ok(scratch)
}

fn move_exclusive(from: &Path, to: &Path) -> Result<()> {
    renamest::rename_exclusive(from, to).with_context(|| {
        format!(
            "cannot rename {} to {} without overwriting",
            from.display(),
            to.display()
        )
    })?;
    sync_directory(parent(to)?)?;
    if parent(from)? != parent(to)? {
        sync_directory(parent(from)?)?;
    }
    Ok(())
}

fn copy_verified(source: &Path, target: &Path) -> Result<Stamp> {
    let stamp = Stamp::read(source)?;
    copy_new(source, target)?;
    ensure!(
        digest(target)? == stamp.digest,
        "staged copy differs from {}",
        source.display()
    );
    stamp.verify(source)?;
    Ok(stamp)
}

fn open_lock(paths: &trash::TrashPaths) -> Result<File> {
    create_directories(&paths.data_root)?;
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(paths.data_root.join("lifecycle.lock"))?)
}

fn lock(roots: &SessionRoots) -> Result<(File, PathBuf)> {
    let paths = roots.trash.as_ref().context("trash store is unavailable")?;
    let file = open_lock(paths)?;
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

pub(crate) fn read_guard(roots: &SessionRoots, wait: bool) -> Result<Option<File>> {
    roots
        .trash
        .as_ref()
        .map(|paths| read_trash_guard(paths, wait))
        .transpose()
}

pub(crate) fn read_trash_guard(paths: &trash::TrashPaths, wait: bool) -> Result<File> {
    let file = open_lock(paths)?;
    if wait {
        file.lock_shared()?;
    } else {
        file.try_lock_shared()?;
    }
    if !trash::children(&paths.data_root.join("operations"))?.is_empty() {
        return Err(RecoveryRequired.into());
    }
    Ok(file)
}

fn guards(
    roots: &SessionRoots,
    plans: &[LifecyclePlan],
) -> Result<Vec<crate::live::LiveSessionGuard>> {
    let tracker = LiveSessionTracker::new(roots.live_sessions.clone());
    let mut guards = Vec::new();
    for agent in [Agent::Claude, Agent::Codex, Agent::Antigravity] {
        let ids = plans
            .iter()
            .flat_map(|p| p.members.values())
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
    if trash::children(&paths.data_root.join("operations"))?.is_empty() {
        return Ok(());
    }
    let (_lock, operations) = lock(roots)?;
    recover_locked(roots, &operations)
}

fn recover_locked(roots: &SessionRoots, operations: &Path) -> Result<()> {
    for directory in trash::children(operations)? {
        (|| -> Result<()> {
            let path = directory.join(JOURNAL);
            if exists(&path)? {
                let journal: Journal = serde_json::from_reader(File::open(path)?)?;
                ensure!(
                    journal.version == JOURNAL_VERSION,
                    "unsupported lifecycle journal version"
                );
                if !journal.committed {
                    let _guards = guards(roots, &journal.plans)?;
                    journal.rollback()?;
                }
                journal.cleanup(&directory)?;
            } else if exists(&directory.join("operation.json"))? {
                legacy::recover_legacy(roots, &directory)?;
            } else {
                // Nothing could have been published before the first journal.
                remove(&directory)?;
            }
            Ok(())
        })()
        .with_context(|| format!("lifecycle recovery incomplete at {}", directory.display()))?;
    }
    Ok(())
}

pub fn initialize_trash(roots: &SessionRoots) -> Result<()> {
    let Some(paths) = &roots.trash else {
        return Ok(());
    };
    if !paths.data_root.exists() {
        return Ok(());
    }
    let (_lock, operations) = lock(roots)?;
    recover_locked(roots, &operations)?;
    migrate_locked(paths, &operations)
}

fn migrate_locked(paths: &trash::TrashPaths, operations: &Path) -> Result<()> {
    let legacy = TrashStore::new(paths.clone()).legacy_items()?;
    for item in legacy {
        let mut staging = Staging::new(Vec::new());
        staging.publish_item(
            &paths.trash_dir,
            &item.metadata,
            &item.legacy_copies.context("missing legacy payload")?,
        )?;
        staging
            .execute(operations)
            .with_context(|| format!("migrating {}", item.path.display()))?;
    }
    let mut staging = Staging::new(Vec::new());
    for source in [&paths.metadata_file, &paths.reasons_file] {
        if exists(source)? {
            ensure!(
                fs::symlink_metadata(source)?.is_file(),
                "invalid legacy catalog {}",
                source.display()
            );
            let target = paths
                .trash_dir
                .join(trash::LEGACY_METADATA_DIR)
                .join(source.file_name().context("missing catalog name")?);
            let stamp = staging.publish_copy(source, target)?;
            staging.retire(source.clone(), stamp)?;
        }
    }
    staging.execute(operations)
}

pub fn execute(
    roots: &SessionRoots,
    approved: &LifecyclePlan,
    reasons: &BTreeMap<PathBuf, TrashReason>,
) -> Result<()> {
    approved.require_safe()?;
    let (_lock, operations) = lock(roots)?;
    recover_locked(roots, &operations)?;
    let _guards = guards(roots, std::slice::from_ref(approved))?;
    recheck(roots, approved)?;
    prepare(roots, approved, reasons)?.execute(&operations)
}

fn recheck(roots: &SessionRoots, approved: &LifecyclePlan) -> Result<()> {
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
    ensure!(
        &fresh == approved,
        "session group changed; review the updated group before trying again"
    );
    Ok(())
}

fn prepare(
    roots: &SessionRoots,
    plan: &LifecyclePlan,
    reasons: &BTreeMap<PathBuf, TrashReason>,
) -> Result<Staging> {
    let paths = roots.trash.as_ref().context("trash store unavailable")?;
    let mut staging = Staging::new(vec![plan.clone()]);
    let items = if plan.operation == Operation::Trash {
        Vec::new()
    } else {
        TrashStore::new(paths.clone()).snapshot()?
    };
    for member in plan.members.values() {
        match plan.operation {
            Operation::Trash => {
                let reason = reasons
                    .get(&member.path)
                    .cloned()
                    .context("missing action provenance")?;
                let (metadata, sources) = trash::new_item(member.agent, &member.path, reason)?;
                let copies = sources
                    .into_iter()
                    .zip(metadata.members.iter().map(|p| p.stored_path.clone()))
                    .collect::<Vec<_>>();
                staging.publish_item(&paths.trash_dir, &metadata, &copies)?;
            }
            Operation::Restore | Operation::Delete if member.trashed => {
                let item = items
                    .iter()
                    .find(|i| i.primary.as_ref() == Some(&member.path))
                    .context("trash item disappeared")?;
                ensure!(
                    item.legacy_copies.is_none(),
                    "trash item still needs migration"
                );
                let stamp = Stamp::read(&item.path)?;
                if plan.operation == Operation::Restore {
                    ensure!(
                        item.original_path().as_ref() == Some(&member.original_path),
                        "restore destination changed"
                    );
                    for (source, target) in item.restore_pairs()? {
                        ensure!(
                            super::in_provider_root(roots, member.agent, &target),
                            "restore destination is outside the provider home: {}",
                            target.display()
                        );
                        staging.publish_copy(&source, target)?;
                    }
                }
                staging.retire(item.path.clone(), stamp)?;
            }
            Operation::Delete => {
                let (_, sources) = trash::native_payload(member.agent, &member.path)?;
                for source in sources {
                    let stamp = Stamp::read(&source)?;
                    staging.retire(source, stamp)?;
                }
            }
            Operation::Restore => bail!("cannot restore an active session"),
        }
    }
    Ok(staging)
}

fn digest(path: &Path) -> Result<[u8; 32]> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        let children = trash::children(path)?;
        let mut output = directory_digest(children.len());
        for child in children {
            hash_child(&mut output, &child, digest(&child)?)?;
        }
        Ok(output.finalize().into())
    } else if metadata.is_file() {
        let (size, bytes) = copy_hash(File::open(path)?, io::sink())?;
        ensure!(
            size == metadata.len(),
            "file changed while reading {}",
            path.display()
        );
        Ok(file_digest(size, bytes))
    } else {
        bail!("unsupported source {}", path.display());
    }
}

fn directory_digest(children: usize) -> Sha256 {
    let mut output = Sha256::new();
    output.update(b"dir\0");
    output.update((children as u64).to_le_bytes());
    output
}

fn hash_child(output: &mut Sha256, path: &Path, digest: [u8; 32]) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("non-UTF-8 name in {}", path.display()))?;
    output.update((name.len() as u64).to_le_bytes());
    output.update(name.as_bytes());
    output.update(digest);
    Ok(())
}

fn file_digest(size: u64, bytes: [u8; 32]) -> [u8; 32] {
    let mut output = Sha256::new();
    output.update(b"file\0");
    output.update(size.to_le_bytes());
    output.update(bytes);
    output.finalize().into()
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

#[cfg(test)]
thread_local! {
    static FAIL_AFTER_RETIREMENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn copy_hash(mut input: impl Read, mut output: impl Write) -> Result<(u64, [u8; 32])> {
    let mut digest = Sha256::new();
    let mut size = 0;
    let mut buffer = [0; 65536];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count])?;
        digest.update(&buffer[..count]);
        size += count as u64;
    }
    Ok((size, digest.finalize().into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trash::TrashPaths;

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
            .plan([parent].into(), Operation::Trash);
        Ok((temp, roots, plan))
    }

    fn reasons(plan: &LifecyclePlan) -> BTreeMap<PathBuf, TrashReason> {
        plan.members
            .keys()
            .map(|p| (p.clone(), TrashReason::KeyBinding { key: "test".into() }))
            .collect()
    }

    #[test]
    fn rollback_after_retirement_preserves_trash_restore_and_purge_sources() -> Result<()> {
        let (_temp, roots, trash_plan) = fixture()?;
        let store = TrashStore::new(roots.trash.clone().unwrap());
        for operation in [Operation::Trash, Operation::Restore, Operation::Delete] {
            let plan = if operation == Operation::Trash {
                trash_plan.clone()
            } else {
                let item = store.snapshot()?.into_iter().next().unwrap();
                DependencyGraph::discover(&roots, Agent::Codex, &[])?
                    .plan([item.primary.unwrap()].into(), operation)
            };
            let originals = plan
                .members
                .keys()
                .map(|p| Ok((p.clone(), fs::read(p)?)))
                .collect::<Result<BTreeMap<_, _>>>()?;
            let before = store
                .snapshot()?
                .into_iter()
                .map(|i| Ok((i.path.clone(), digest(&i.path)?)))
                .collect::<Result<BTreeMap<_, _>>>()?;
            FAIL_AFTER_RETIREMENT.with(|flag| flag.set(true));
            let result = execute(&roots, &plan, &reasons(&plan));
            FAIL_AFTER_RETIREMENT.with(|flag| flag.set(false));
            assert!(result.unwrap_err().to_string().contains("rolled back"));
            for (p, contents) in originals {
                assert_eq!(fs::read(p)?, contents);
            }
            let after = store
                .snapshot()?
                .into_iter()
                .map(|i| Ok((i.path.clone(), digest(&i.path)?)))
                .collect::<Result<BTreeMap<_, _>>>()?;
            assert_eq!(after, before);
            if operation == Operation::Trash {
                execute(&roots, &plan, &reasons(&plan))?;
            }
        }
        Ok(())
    }

    #[test]
    fn startup_rolls_back_a_rename_even_without_a_post_rename_journal_write() -> Result<()> {
        let (_temp, roots, plan) = fixture()?;
        let (lock, operations) = lock(&roots)?;
        let (directory, journal) = prepare(&roots, &plan, &reasons(&plan))?.persist(&operations)?;
        // Simulate a stop after publish and the first source retirement.
        journal.publications[0].apply()?;
        journal.retirements[0].apply()?;
        assert!(!journal.retirements[0].from.exists());
        drop(lock);
        recover(&roots)?;
        for entry in &journal.retirements {
            entry.stamp.verify(&entry.from)?;
        }
        assert!(journal.publications.iter().all(|entry| !entry.to.exists()));
        assert!(!directory.exists());
        Ok(())
    }

    #[test]
    fn collision_rolls_back_without_removing_an_identical_foreign_item() -> Result<()> {
        let (_temp, roots, plan) = fixture()?;
        let (lock, operations) = lock(&roots)?;
        let (directory, journal) = prepare(&roots, &plan, &reasons(&plan))?.persist(&operations)?;
        let publication = &journal.publications[0];
        copy_new(&publication.from, &publication.to)?;
        let foreign = entry_identity(&publication.to)?;
        assert!(publication.apply().is_err());
        drop(lock);
        recover(&roots)?;
        assert_eq!(entry_identity(&publication.to)?, foreign);
        assert!(journal.retirements.iter().all(|entry| entry.from.exists()));
        assert!(!directory.exists());
        Ok(())
    }

    #[test]
    fn recovery_preserves_a_changed_published_item() -> Result<()> {
        let (_temp, roots, plan) = fixture()?;
        let (lock, operations) = lock(&roots)?;
        let (directory, journal) = prepare(&roots, &plan, &reasons(&plan))?.persist(&operations)?;
        let publication = &journal.publications[0];
        publication.apply()?;
        journal.retirements[0].apply()?;
        let external = publication.to.join("external.txt");
        fs::write(&external, "external edit")?;
        drop(lock);
        assert!(recover(&roots).is_err());
        assert_eq!(fs::read_to_string(external)?, "external edit");
        assert!(journal.retirements[0].from.exists());
        assert!(directory.join(JOURNAL).exists());
        Ok(())
    }

    #[test]
    fn committed_archive_recovery_keeps_backups_when_the_zip_is_missing() -> Result<()> {
        let (temp, roots, plan) = fixture()?;
        execute(&roots, &plan, &reasons(&plan))?;
        let item = TrashStore::new(roots.trash.clone().unwrap())
            .snapshot()?
            .pop()
            .unwrap();
        let zip = temp.path().join("published.zip");
        fs::write(&zip, b"verified archive fixture")?;
        let (lock, operations) = lock(&roots)?;
        let mut staging = Staging::new(Vec::new());
        staging.journal.archive = Some(VerifiedArchive {
            path: zip.clone(),
            stamp: Stamp::read(&zip)?,
        });
        staging.retire(item.path.clone(), Stamp::read(&item.path)?)?;
        let (directory, mut journal) = staging.persist(&operations)?;
        journal.retirements[0].apply()?;
        journal.committed = true;
        journal.save(&directory)?;
        fs::remove_file(zip)?;
        drop(lock);
        assert!(recover(&roots).is_err());
        journal.retirements[0]
            .stamp
            .verify(&journal.retirements[0].to)?;
        assert!(directory.join(JOURNAL).is_file());
        Ok(())
    }

    #[test]
    fn archive_cleanup_failure_keeps_both_the_zip_and_the_trash_items() -> Result<()> {
        let (temp, roots, plan) = fixture()?;
        execute(&roots, &plan, &reasons(&plan))?;
        let store = TrashStore::new(roots.trash.clone().unwrap());
        let before = store
            .snapshot()?
            .into_iter()
            .map(|item| Ok((item.path.clone(), digest(&item.path)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let zip = temp.path().join("archive.zip");
        FAIL_AFTER_RETIREMENT.with(|flag| flag.set(true));
        let result = archive_trash(&roots, &zip, true, false);
        FAIL_AFTER_RETIREMENT.with(|flag| flag.set(false));
        assert!(result.unwrap_err().to_string().contains("rolled back"));
        let after = store
            .snapshot()?
            .into_iter()
            .map(|item| Ok((item.path.clone(), digest(&item.path)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        assert_eq!(before, after);
        let extracted = temp.path().join("extracted");
        zip::ZipArchive::new(File::open(zip)?)?.extract(&extracted)?;
        for (path, expected) in before {
            assert_eq!(
                digest(&extracted.join("trash").join(path.file_name().unwrap()))?,
                expected
            );
        }
        Ok(())
    }
}
