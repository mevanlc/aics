//! Recovery for journals created before trash items became directories.
use super::*;

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

pub(super) fn recover_legacy(roots: &SessionRoots, path: &Path) -> Result<()> {
    let journal: Journal = serde_json::from_reader(File::open(path.join("operation.json"))?)?;
    if !journal.committed {
        let _guards = guards(roots, std::slice::from_ref(&journal.plan))?;
        rollback(&journal)?;
    } else {
        cleanup_archives(roots, &journal.plan);
    }
    remove(path)
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
mod tests {
    use super::*;

    #[test]
    fn startup_recovers_old_journals_before_migrating_legacy_catalogs() -> Result<()> {
        let temp = TempDir::new()?;
        let paths = trash::TrashPaths::from_data_root(temp.path().join("data"));
        let roots = SessionRoots {
            live_sessions: Default::default(),
            claude_projects: temp.path().join("claude"),
            codex_sessions: temp.path().join("codex"),
            antigravity_home: temp.path().join("agy"),
            trash: Some(paths.clone()),
        };
        let source = roots.codex_sessions.join("old.jsonl");
        let target = paths.trash_dir.join("old.jsonl");
        let operation = paths.data_root.join("operations/123-456");
        let backup = operation.join("source-0");
        create_directories(&operation)?;
        create_directories(&paths.trash_dir)?;
        fs::write(&backup, b"original content")?;
        copy_new(&backup, &target)?;
        let journal = Journal {
            committed: false,
            plan: LifecyclePlan {
                operation: Operation::Trash,
                seeds: Default::default(),
                members: Default::default(),
                reasons: Default::default(),
                blockers: Default::default(),
            },
            transfers: vec![Transfer {
                source: source.clone(),
                target: Some(target.clone()),
                backup: backup.clone(),
                digest: digest(&backup)?,
                published_identity: identity(&target)?,
            }],
            metadata: vec![MetadataChange {
                path: paths.metadata_file.clone(),
                before: Some(b"\n".to_vec()),
                after: b"temporary entry\n".to_vec(),
            }],
        };
        fs::write(&paths.metadata_file, &journal.metadata[0].after)?;
        fs::write(
            operation.join("operation.json"),
            serde_json::to_vec(&journal)?,
        )?;
        initialize_trash(&roots)?;
        assert_eq!(fs::read(source)?, b"original content");
        assert!(!target.exists() && !operation.exists());
        assert_eq!(
            fs::read(paths.trash_dir.join(".legacy-migration/trash.jsonl"))?,
            b"\n"
        );
        assert!(!paths.metadata_file.exists());
        Ok(())
    }
}
