use std::io::{IsTerminal, Seek};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use super::*;

const SCHEMA: &[u8] = include_bytes!("../../../docs/schemas/trash-item-v1.schema.json");

/// Export the current trash store or archive it and retire the verified items.
pub fn archive_trash(
    roots: &SessionRoots,
    destination: &Path,
    purge: bool,
    yes: bool,
) -> Result<usize> {
    let paths = roots.trash.as_ref().context("trash store is unavailable")?;
    let destination = std::path::absolute(destination)?;
    let resolved_parent = super::super::resolved_path(parent(&destination)?);
    let destination = resolved_parent.join(
        destination
            .file_name()
            .context("ZIP path has no filename")?,
    );
    ensure!(
        !destination.starts_with(super::super::resolved_path(&paths.trash_dir))
            && !destination.starts_with(super::super::resolved_path(
                &paths.data_root.join("operations")
            ))
            && destination != super::super::resolved_path(&paths.data_root.join("lifecycle.lock"))
            && destination != super::super::resolved_path(&paths.metadata_file)
            && destination != super::super::resolved_path(&paths.reasons_file),
        "ZIP destination overlaps the trash store: {}",
        destination.display()
    );
    let overwrite = confirm_destination(&destination, yes)?;

    let (_lock, operations) = lock(roots)?;
    recover_locked(roots, &operations)?;
    migrate_locked(paths, &operations)?;
    let items = TrashStore::new(paths.clone()).snapshot()?;
    let plans = if purge {
        purge_plans(roots, &items)
            .context("trash cannot be archived and removed; use --export-trash to keep a copy")?
    } else {
        Vec::new()
    };
    let _guards = guards(roots, &plans)?;
    for plan in &plans {
        recheck(roots, plan)?;
    }
    let snapshot = items
        .iter()
        .map(|item| Ok((item.path.clone(), Stamp::read(&item.path)?)))
        .collect::<Result<Vec<_>>>()?;
    let scratch = scratch_directory(parent(&destination)?)?;
    let staged = scratch.path().join("trash.zip");
    write_zip(paths, &snapshot, &staged)?;
    let stamp = Stamp::read(&staged)?;
    for (path, before) in &snapshot {
        before.verify(path)?;
    }

    if let Some(before) = overwrite {
        ensure!(
            Destination::read(&destination)? == before,
            "ZIP destination changed: {}",
            destination.display()
        );
        fs::rename(&staged, &destination)?;
        sync_directory(parent(&destination)?)?;
        sync_directory(scratch.path())?;
    } else {
        move_exclusive(&staged, &destination)?;
    }
    stamp.verify(&destination)?;

    if purge {
        // Only retire the exact item directories that the verified ZIP contains.
        // The ZIP remains available if revalidation or removal fails.
        for plan in &plans {
            recheck(roots, plan)?;
        }
        let mut staging = Staging::new(plans);
        staging.journal.archive = Some(VerifiedArchive {
            path: destination,
            stamp,
        });
        for (path, before) in snapshot {
            staging.retire(path, before)?;
        }
        staging.execute(&operations)?;
    }
    Ok(items.len())
}

#[derive(PartialEq, Eq)]
struct Destination {
    identity: FileIdentity,
    length: u64,
    modified: std::time::SystemTime,
}

impl Destination {
    fn read(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        ensure!(
            metadata.is_file(),
            "ZIP destination is not a regular file: {}",
            path.display()
        );
        Ok(Self {
            identity: entry_identity(path)?,
            length: metadata.len(),
            modified: metadata.modified()?,
        })
    }
}

fn confirm_destination(path: &Path, yes: bool) -> Result<Option<Destination>> {
    if !exists(path)? {
        return Ok(None);
    }
    let existing = Destination::read(path)?;
    if !yes {
        ensure!(
            io::stdin().is_terminal(),
            "ZIP destination exists: {}; pass --yes to overwrite",
            path.display()
        );
        eprint!("Overwrite {}? [y/N] ", path.display());
        io::stderr().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        ensure!(
            matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
            "overwrite declined"
        );
    }
    Ok(Some(existing))
}

fn purge_plans(roots: &SessionRoots, items: &[trash::TrashItem]) -> Result<Vec<LifecyclePlan>> {
    for item in items {
        ensure!(
            item.primary.is_some()
                && item.original_path().is_some()
                && item.metadata.agent.is_some(),
            "cannot verify trash item {}",
            item.path.display()
        );
    }
    let mut plans = Vec::new();
    for agent in [Agent::Claude, Agent::Codex, Agent::Antigravity] {
        let seeds = items
            .iter()
            .filter(|item| item.metadata.agent == Some(agent))
            .filter_map(|item| item.primary.clone())
            .collect::<BTreeSet<_>>();
        if seeds.is_empty() {
            continue;
        }
        let seed_paths = seeds.iter().cloned().collect::<Vec<_>>();
        let plan = DependencyGraph::discover(roots, agent, &seed_paths)?
            .plan(seeds.clone(), Operation::Delete);
        plan.require_safe()?;
        ensure!(
            plan.members.keys().eq(seeds.iter()) && plan.members.values().all(|m| m.trashed),
            "archive removal must include only the archived trash items"
        );
        plans.push(plan);
    }
    Ok(plans)
}

#[derive(Debug, PartialEq, Eq)]
struct ZipEntry {
    name: String,
    content: Option<(u64, [u8; 32])>,
}

fn write_zip(
    paths: &trash::TrashPaths,
    snapshot: &[(PathBuf, Stamp)],
    staged: &Path,
) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(staged)?;
    let mut writer = ZipWriter::new(file);
    let mut entries = Vec::new();
    add_directory(&mut writer, &mut entries, "trash/".to_owned())?;
    add_directory(&mut writer, &mut entries, "trash/.schemas/".to_owned())?;
    add_file(
        &mut writer,
        &mut entries,
        "trash/.schemas/trash-item-v1.schema.json".to_owned(),
        SCHEMA.len() as u64,
        SCHEMA,
    )?;
    for (path, before) in snapshot {
        ensure!(
            add_tree(&mut writer, &mut entries, &paths.trash_dir, path)? == before.digest,
            "trash item changed while archiving {}",
            path.display()
        );
    }
    let legacy = paths.trash_dir.join(trash::LEGACY_METADATA_DIR);
    if exists(&legacy)? {
        add_tree(&mut writer, &mut entries, &paths.trash_dir, &legacy)?;
    }
    writer.finish()?.sync_all()?;
    sync_directory(parent(staged)?)?;

    let mut archive = ZipArchive::new(File::open(staged)?)?;
    ensure!(
        archive.len() == entries.len(),
        "ZIP entry count differs from the snapshot"
    );
    for (i, expected) in entries.into_iter().enumerate() {
        let mut entry = archive.by_index(i)?;
        ensure!(
            entry.name()? == expected.name,
            "ZIP member differs from {}",
            expected.name
        );
        let content = if entry.is_dir() {
            None
        } else {
            Some(copy_hash(&mut entry, &mut io::sink())?)
        };
        ensure!(
            content == expected.content,
            "ZIP verification failed for {}",
            expected.name
        );
    }
    Ok(())
}

fn add_tree<W: Write + Seek>(
    writer: &mut ZipWriter<W>,
    entries: &mut Vec<ZipEntry>,
    root: &Path,
    path: &Path,
) -> Result<[u8; 32]> {
    let mut name = String::from("trash");
    for component in path.strip_prefix(root)?.components() {
        let part = component
            .as_os_str()
            .to_str()
            .context("non-UTF-8 ZIP entry name")?;
        ensure!(
            matches!(component, std::path::Component::Normal(_))
                && !part.contains(['\\', ':', '\0']),
            "unsafe ZIP entry name: {}",
            path.display()
        );
        name.push('/');
        name.push_str(part);
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        name.push('/');
        add_directory(writer, entries, name)?;
        let children = trash::children(path)?;
        let mut digest = directory_digest(children.len());
        for child in children {
            hash_child(
                &mut digest,
                &child,
                add_tree(writer, entries, root, &child)?,
            )?;
        }
        Ok(digest.finalize().into())
    } else if metadata.is_file() {
        let (size, bytes) = add_file(writer, entries, name, metadata.len(), File::open(path)?)?;
        Ok(file_digest(size, bytes))
    } else {
        bail!("unsupported ZIP source {}", path.display());
    }
}

fn add_directory<W: Write + Seek>(
    writer: &mut ZipWriter<W>,
    entries: &mut Vec<ZipEntry>,
    name: String,
) -> Result<()> {
    writer.add_directory(&name, SimpleFileOptions::default().unix_permissions(0o700))?;
    entries.push(ZipEntry {
        name,
        content: None,
    });
    Ok(())
}

fn add_file<W: Write + Seek>(
    writer: &mut ZipWriter<W>,
    entries: &mut Vec<ZipEntry>,
    name: String,
    size: u64,
    mut input: impl Read,
) -> Result<(u64, [u8; 32])> {
    let method = if name.ends_with(".zst") {
        CompressionMethod::Stored
    } else {
        CompressionMethod::Deflated
    };
    writer.start_file(
        &name,
        SimpleFileOptions::default()
            .compression_method(method)
            .unix_permissions(0o600)
            .large_file(size >= u64::from(u32::MAX) - 65536),
    )?;
    let content = copy_hash(&mut input, writer)?;
    ensure!(
        content.0 == size,
        "file size changed while archiving {name}"
    );
    entries.push(ZipEntry {
        name,
        content: Some(content),
    });
    Ok(content)
}
