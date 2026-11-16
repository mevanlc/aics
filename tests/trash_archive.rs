use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aics::lifecycle::{self, DependencyGraph, Operation};
use aics::live::LiveSessionPaths;
use aics::parse::Agent;
use aics::scan::SessionRoots;
use aics::summary::sidecar_path;
use aics::trash::{TrashPaths, TrashReason, TrashStore};
use anyhow::Result;
use tempfile::TempDir;
use zip::ZipArchive;

#[path = "common/lock_holder.rs"]
mod lock_holder;
#[path = "support/trash.rs"]
mod trash_fixture;

struct Fixture {
    temp: TempDir,
    roots: SessionRoots,
    trash: TrashPaths,
}

impl Fixture {
    fn new() -> Result<Self> {
        let temp = TempDir::new()?;
        let trash = TrashPaths::from_data_root(temp.path().join("data"));
        let roots = SessionRoots {
            live_sessions: LiveSessionPaths {
                claude_sessions_dir: Some(temp.path().join("claude/sessions")),
                codex_writer_locks_dir: Some(temp.path().join("codex/thread-writer-locks")),
                antigravity_presence_dir: Some(temp.path().join("antigravity/presence")),
            },
            claude_projects: temp.path().join("claude/projects"),
            codex_sessions: temp.path().join("codex/sessions"),
            antigravity_home: temp.path().join("antigravity"),
            trash: Some(trash.clone()),
        };
        Ok(Self { temp, roots, trash })
    }

    fn cli(&self, args: &[&str], destination: Option<&Path>) -> Result<Output> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aics"));
        self.configure_command(&mut command);
        command.args(args);
        if let Some(path) = destination {
            command.arg(path);
        }
        Ok(command.output()?)
    }

    fn configure_command(&self, command: &mut Command) {
        command
            .current_dir(self.temp.path())
            .env("AICS_CONFIG_ROOT", self.temp.path().join("config"))
            .env("AICS_CACHE_ROOT", self.temp.path().join("cache"))
            .env("AICS_DATA_ROOT", &self.trash.data_root)
            .env("AICS_CLAUDE_PROJECTS_DIR", &self.roots.claude_projects)
            .env("AICS_CODEX_SESSIONS_DIR", &self.roots.codex_sessions)
            .env("AICS_ANTIGRAVITY_HOME", &self.roots.antigravity_home)
            .env("RUST_LOG", "off")
            .arg("--claude-home")
            .arg(self.temp.path().join("claude"))
            .arg("--codex-home")
            .arg(self.temp.path().join("codex"))
            .arg("--antigravity-home")
            .arg(&self.roots.antigravity_home);
    }

    fn codex(&self, name: &str, history: Option<&str>) -> Result<PathBuf> {
        let path = self.roots.codex_sessions.join(format!("{name}.jsonl"));
        let mut payload = serde_json::json!({"id": name});
        if let Some(parent) = history {
            payload["history_base"] = serde_json::json!({"thread_id": parent});
        }
        write(
            &path,
            &serde_json::to_vec(&serde_json::json!({"type": "session_meta", "payload": payload}))?,
        )?;
        Ok(path)
    }

    fn store(&self) -> TrashStore {
        TrashStore::new(self.trash.clone())
    }

    fn claude(&self, id: &str) -> Result<PathBuf> {
        let path = self.roots.claude_projects.join(format!("{id}.jsonl"));
        write(&path, format!("{{\"type\":\"user\",\"sessionId\":\"{id}\",\"message\":{{\"role\":\"user\",\"content\":\"fixture\"}}}}\n").as_bytes())?;
        Ok(path)
    }

    fn archived(&self, zip: &Path) -> Result<Vec<aics::trash::TrashItem>> {
        let extracted = self.temp.path().join("extracted");
        ZipArchive::new(File::open(zip)?)?.extract(&extracted)?;
        TrashStore::new(TrashPaths::from_data_root(extracted)).snapshot()
    }

    #[cfg(unix)]
    fn locked_trash(&self, count: usize) -> Result<()> {
        let mut seeds = std::collections::BTreeSet::new();
        for i in 0..count {
            let path = self.roots.antigravity_home.join(format!(
                "brain/idle-{i}/.system_generated/logs/transcript.jsonl"
            ));
            write(&path, b"{}\n")?;
            seeds.insert(path);
        }
        let paths = seeds.iter().cloned().collect::<Vec<_>>();
        let plan = DependencyGraph::discover(&self.roots, Agent::Antigravity, &paths)?
            .plan(seeds, Operation::Trash);
        let reasons = plan
            .members
            .keys()
            .map(|path| (path.clone(), reason()))
            .collect();
        lifecycle::execute(&self.roots, &plan, &reasons)?;
        for i in 0..count {
            write(
                &self
                    .roots
                    .antigravity_home
                    .join(format!("presence/idle-{i}.lock")),
                b"idle marker",
            )?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn archive_with_descriptor_limit(&self, hard: bool, zip: &Path) -> Result<Output> {
        let mut command = Command::new("/bin/sh");
        let script = if hard {
            "ulimit -n 64; exec \"$@\""
        } else {
            "ulimit -S -n 64; exec \"$@\""
        };
        command.args([
            "-c",
            script,
            "aics-descriptor-test",
            env!("CARGO_BIN_EXE_aics"),
        ]);
        self.configure_command(&mut command);
        Ok(command.arg("--archive-trash").arg(zip).output()?)
    }
}

fn write(path: &Path, contents: &[u8]) -> Result<()> {
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(path, contents)?;
    Ok(())
}

fn reason() -> TrashReason {
    TrashReason::KeyBinding { key: "^T".into() }
}

fn assert_success(output: Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_skipped(output: &Output, count: usize) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let warnings = stderr
        .lines()
        .filter(|line| line.starts_with("warning: skipped"))
        .collect::<Vec<_>>();
    assert_eq!(warnings, [format!("warning: skipped {count} trash entries that could not be safely removed; omitted from the archive and left in trash")]);
}

fn contents(path: &Path) -> Result<std::collections::BTreeMap<PathBuf, Vec<u8>>> {
    fn collect(
        root: &Path,
        path: &Path,
        files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
    ) -> Result<()> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                collect(root, &entry.path(), files)?;
            } else {
                files.insert(
                    entry.path().strip_prefix(root)?.into(),
                    fs::read(entry.path())?,
                );
            }
        }
        Ok(())
    }
    let mut files = Default::default();
    collect(path, path, &mut files)?;
    Ok(files)
}

#[test]
#[cfg(unix)]
fn archive_handles_many_markers_with_a_low_soft_descriptor_limit() -> Result<()> {
    let f = Fixture::new()?;
    f.locked_trash(96)?;
    let _held = lock_holder::HeldLock::new(&f.roots.antigravity_home.join("presence/idle-95.lock"));
    let zip = f.temp.path().join("archive.zip");
    let output = f.archive_with_descriptor_limit(false, &zip)?;
    assert_skipped(&output, 1);
    assert_eq!(f.archived(&zip)?.len(), 95);
    assert_eq!(f.store().snapshot()?.len(), 1);
    assert_eq!(
        fs::read(f.roots.antigravity_home.join("presence/idle-0.lock"))?,
        b"idle marker"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn archive_keeps_io_headroom_under_a_low_hard_descriptor_limit() -> Result<()> {
    let f = Fixture::new()?;
    f.locked_trash(96)?;
    let zip = f.temp.path().join("archive.zip");
    let output = f.archive_with_descriptor_limit(true, &zip)?;
    let remaining = f.store().snapshot()?;
    assert_skipped(&output, remaining.len());
    let archived = f.archived(&zip)?;
    assert!(!archived.is_empty() && !remaining.is_empty());
    assert_eq!(archived.len() + remaining.len(), 96);
    for item in &remaining {
        assert!(item.primary.as_ref().unwrap().is_file());
    }
    Ok(())
}

#[test]
fn trash_zip_modes_warn_for_global_and_still_reject_filters() -> Result<()> {
    for operation in ["--export-trash", "--archive-trash"] {
        for global in ["-g", "--global"] {
            let f = Fixture::new()?;
            let source = f.codex("codex", None)?;
            let item = trash_fixture::trash_session(&f.roots, &source, Agent::Codex, reason())?;
            let zip = f.temp.path().join("trash.zip");

            let rejected = f.cli(&[global, "--dir", ".", operation], Some(&zip))?;
            assert!(!rejected.status.success());
            assert!(String::from_utf8_lossy(&rejected.stderr)
                .contains("trash ZIP commands do not accept dir"));
            assert!(!zip.exists() && item.path.is_dir());

            let output = f.cli(&[global, operation], Some(&zip))?;
            assert!(String::from_utf8_lossy(&output.stderr).contains(&format!(
                "warning: {operation} always operates on all trash items: --global has no effect on it"
            )));
            assert_success(output);
            let extracted = f.temp.path().join("extracted");
            ZipArchive::new(File::open(zip)?)?.extract(&extracted)?;
            assert!(extracted
                .join("trash")
                .join(item.path.file_name().unwrap())
                .join("metadata.json")
                .is_file());
            assert_eq!(item.path.exists(), operation == "--export-trash");
        }
    }
    Ok(())
}

#[test]
fn both_zip_modes_preserve_complete_items_and_archived_items_restore() -> Result<()> {
    let f = Fixture::new()?;
    let codex = f.codex("codex", None)?;
    let claude = f.roots.claude_projects.join("project/claude.jsonl");
    write(
        &claude,
        br#"{"type":"user","sessionId":"claude","message":{"role":"user","content":"fixture"}}"#,
    )?;
    let antigravity = f
        .roots
        .antigravity_home
        .join("brain/agy/.system_generated/logs/transcript.jsonl");
    write(&antigravity, b"{}\n")?;
    let db = f.roots.antigravity_home.join("conversations/agy.db");
    write(&db, b"database")?;
    let originals = [
        (Agent::Codex, codex),
        (Agent::Claude, claude),
        (Agent::Antigravity, antigravity),
    ];
    for (agent, path) in &originals {
        write(&sidecar_path(path), b"summary")?;
        trash_fixture::trash_session(&f.roots, path, *agent, reason())?;
        assert!(!path.exists() && !sidecar_path(path).exists());
    }
    let zip = f.temp.path().join("trash.zip");
    assert_success(f.cli(&["--export-trash"], Some(&zip))?);
    let before = fs::read(&zip)?;
    let denied = f.cli(&["--archive-trash"], Some(&zip))?;
    assert!(!denied.status.success());
    assert_eq!(fs::read(&zip)?, before);
    assert_eq!(f.store().snapshot()?.len(), 3);

    let output = f.cli(&["--yes", "--archive-trash"], Some(&zip))?;
    assert!(!String::from_utf8_lossy(&output.stderr).contains("warning: skipped"));
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("3 trash items in "));
    assert_success(output);
    assert!(f.store().snapshot()?.is_empty());
    let restored_data = f.temp.path().join("extracted");
    ZipArchive::new(File::open(zip)?)?.extract(&restored_data)?;
    assert!(restored_data
        .join("trash/.schemas/trash-item-v1.schema.json")
        .is_file());
    let mut roots = f.roots;
    roots.trash = Some(TrashPaths::from_data_root(restored_data));
    lifecycle::initialize_trash(&roots)?;
    let store = TrashStore::new(roots.trash.clone().unwrap());
    assert_eq!(store.snapshot()?.len(), 3);
    for (agent, original) in originals {
        let item = store
            .snapshot()?
            .into_iter()
            .find(|i| i.metadata.agent == Some(agent))
            .unwrap();
        assert_eq!(item.metadata.reason, Some(reason()));
        let path = item.primary.unwrap();
        let plan = DependencyGraph::discover(&roots, agent, std::slice::from_ref(&path))?
            .plan([path].into(), Operation::Restore);
        lifecycle::execute(&roots, &plan, &Default::default())?;
        assert!(original.is_file());
        assert_eq!(fs::read(sidecar_path(&original))?, b"summary");
    }
    assert_eq!(fs::read(db)?, b"database");
    assert!(store.snapshot()?.is_empty());
    Ok(())
}

#[test]
fn archive_respects_active_dependents_and_export_includes_damaged_items() -> Result<()> {
    let f = Fixture::new()?;
    let parent = f.codex("parent", None)?;
    let item = trash_fixture::trash_session(&f.roots, &parent, Agent::Codex, reason())?;
    let child = f.codex("child", Some("parent"))?;
    let zip = f.temp.path().join("trash.zip");
    let blocked = f.cli(&["--archive-trash"], Some(&zip))?;
    assert_skipped(&blocked, 1);
    assert!(blocked.stdout.is_empty());
    assert!(!zip.exists() && item.path.exists() && child.exists());

    fs::write(item.path.join("metadata.json"), b"broken JSON")?;
    let inferred = f.store().snapshot()?.pop().unwrap();
    assert_eq!(inferred.primary, item.primary);
    assert!(inferred.original_path().is_none());
    assert_success(f.cli(&["--export-trash"], Some(&zip))?);
    let extracted = f.temp.path().join("extracted");
    ZipArchive::new(File::open(zip)?)?.extract(&extracted)?;
    assert_eq!(
        fs::read(
            extracted
                .join("trash")
                .join(item.path.file_name().unwrap())
                .join("metadata.json")
        )?,
        b"broken JSON"
    );
    assert!(item.path.is_dir() && child.is_file());
    Ok(())
}

#[test]
fn archive_skips_dependency_blocked_items_and_preserves_their_bytes() -> Result<()> {
    let f = Fixture::new()?;
    let parent = f.codex("parent", None)?;
    let parent = trash_fixture::trash_session(&f.roots, &parent, Agent::Codex, reason())?;
    let child = f.codex("active-child", Some("parent"))?;
    let idle = f.codex("idle", None)?;
    let idle = trash_fixture::trash_session(&f.roots, &idle, Agent::Codex, reason())?;
    let before = contents(&parent.path)?;
    let zip = f.temp.path().join("partial.zip");
    let output = f.cli(&["--archive-trash"], Some(&zip))?;
    assert_skipped(&output, 1);
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("1 trash items in "));
    assert_eq!(contents(&parent.path)?, before);
    assert!(child.is_file() && !idle.path.exists());
    let archived = f.archived(&zip)?;
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].path.file_name(), idle.path.file_name());
    Ok(())
}

#[test]
fn archive_skips_live_locked_and_damaged_entries_but_keeps_independent_items() -> Result<()> {
    let f = Fixture::new()?;
    let mut protected = Vec::new();
    for id in ["live", "damaged"] {
        let source = f.claude(id)?;
        protected.push(trash_fixture::trash_session(
            &f.roots,
            &source,
            Agent::Claude,
            reason(),
        )?);
    }
    let parent = f.codex("parent", None)?;
    let parent = trash_fixture::trash_session(&f.roots, &parent, Agent::Codex, reason())?;
    let child = f.codex("child", Some("parent"))?;
    let child = trash_fixture::trash_session(&f.roots, &child, Agent::Codex, reason())?;
    protected.extend([parent, child]);
    fs::write(protected[1].path.join("metadata.json"), b"broken JSON")?;
    let idle = f.codex("idle", None)?;
    let idle = trash_fixture::trash_session(&f.roots, &idle, Agent::Codex, reason())?;
    write(
        &f.temp.path().join("claude/sessions/live.json"),
        format!("{{\"sessionId\":\"live\",\"pid\":{}}}", std::process::id()).as_bytes(),
    )?;
    write(
        &f.temp
            .path()
            .join("codex/thread-writer-locks/.coordination.lock"),
        b"idle coordination",
    )?;
    let idle_marker = f.temp.path().join("codex/thread-writer-locks/idle.lock");
    write(&idle_marker, b"idle marker")?;
    let _held =
        lock_holder::HeldLock::new(&f.temp.path().join("codex/thread-writer-locks/child.lock"));
    let before = protected
        .iter()
        .map(|item| contents(&item.path))
        .collect::<Result<Vec<_>>>()?;
    let zip = f.temp.path().join("partial.zip");
    let output = f.cli(&["--archive-trash"], Some(&zip))?;
    assert_skipped(&output, 4);
    for (item, before) in protected.iter().zip(before) {
        assert_eq!(contents(&item.path)?, before);
    }
    assert!(!idle.path.exists());
    assert_eq!(fs::read(idle_marker)?, b"idle marker");
    let archived = f.archived(&zip)?;
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].path.file_name(), idle.path.file_name());
    Ok(())
}

#[test]
fn archive_can_remove_a_leaf_while_its_live_parent_stays_in_trash() -> Result<()> {
    let f = Fixture::new()?;
    let parent = f.codex("parent", None)?;
    let parent = trash_fixture::trash_session(&f.roots, &parent, Agent::Codex, reason())?;
    let child = f.codex("child", Some("parent"))?;
    let child = trash_fixture::trash_session(&f.roots, &child, Agent::Codex, reason())?;
    write(
        &f.temp
            .path()
            .join("codex/thread-writer-locks/.coordination.lock"),
        b"idle coordination",
    )?;
    let _held =
        lock_holder::HeldLock::new(&f.temp.path().join("codex/thread-writer-locks/parent.lock"));
    let zip = f.temp.path().join("leaf.zip");
    assert_skipped(&f.cli(&["--archive-trash"], Some(&zip))?, 1);
    assert!(parent.path.is_dir() && !child.path.exists());
    assert_eq!(f.archived(&zip)?.len(), 1);
    Ok(())
}

#[test]
fn indeterminate_provider_is_skipped_without_blocking_other_providers() -> Result<()> {
    let f = Fixture::new()?;
    let source = f.claude("uncertain")?;
    let uncertain = trash_fixture::trash_session(&f.roots, &source, Agent::Claude, reason())?;
    let source = f.codex("idle", None)?;
    let idle = trash_fixture::trash_session(&f.roots, &source, Agent::Codex, reason())?;
    write(
        &f.temp.path().join("claude/sessions/broken.json"),
        b"broken marker",
    )?;
    let zip = f.temp.path().join("partial.zip");
    assert_skipped(&f.cli(&["--archive-trash"], Some(&zip))?, 1);
    assert!(uncertain.path.is_dir() && !idle.path.exists());
    assert_eq!(f.archived(&zip)?.len(), 1);
    Ok(())
}

#[test]
fn same_thread_pages_stay_in_trash_when_one_page_cannot_be_verified() -> Result<()> {
    let f = Fixture::new()?;
    let mut pages = Vec::new();
    for page in ["first", "second"] {
        let path = f.roots.codex_sessions.join(format!("shared_{page}.jsonl"));
        write(
            &path,
            br#"{"type":"session_meta","payload":{"id":"shared"}}"#,
        )?;
        pages.push(path);
    }
    let plan = DependencyGraph::discover(&f.roots, Agent::Codex, &pages)?
        .plan([pages[0].clone()].into(), Operation::Trash);
    let reasons = plan
        .members
        .keys()
        .map(|path| (path.clone(), reason()))
        .collect();
    lifecycle::execute(&f.roots, &plan, &reasons)?;
    let items = f.store().snapshot()?;
    assert_eq!(items.len(), 2);
    fs::write(items[0].path.join("metadata.json"), b"broken JSON")?;
    let before = items
        .iter()
        .map(|item| contents(&item.path))
        .collect::<Result<Vec<_>>>()?;
    let idle = f.codex("idle", None)?;
    let idle = trash_fixture::trash_session(&f.roots, &idle, Agent::Codex, reason())?;
    let zip = f.temp.path().join("partial.zip");
    assert_skipped(&f.cli(&["--archive-trash"], Some(&zip))?, 2);
    for (item, before) in items.iter().zip(before) {
        assert_eq!(contents(&item.path)?, before);
    }
    assert!(!idle.path.exists());
    let archived = f.archived(&zip)?;
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].path.file_name(), idle.path.file_name());
    Ok(())
}

#[test]
fn empty_or_entirely_skipped_archive_never_touches_the_destination() -> Result<()> {
    let f = Fixture::new()?;
    let zip = f.temp.path().join("existing.zip");
    let empty = f.cli(&["--archive-trash"], Some(&zip))?;
    assert!(empty.stdout.is_empty() && !zip.exists());
    assert!(!String::from_utf8_lossy(&empty.stderr).contains("warning: skipped"));
    assert_success(empty);
    let source = f.codex("parent", None)?;
    let item = trash_fixture::trash_session(&f.roots, &source, Agent::Codex, reason())?;
    f.codex("child", Some("parent"))?;
    let before = contents(&item.path)?;
    for existing in [false, true] {
        if existing {
            fs::write(&zip, b"existing destination")?;
        }
        assert_skipped(&f.cli(&["--archive-trash"], Some(&zip))?, 1);
        assert_eq!(zip.exists(), existing);
        if existing {
            assert_eq!(fs::read(&zip)?, b"existing destination");
        }
        assert_eq!(contents(&item.path)?, before);
    }
    Ok(())
}

#[test]
fn migration_keeps_raw_catalogs_and_orphans_and_is_idempotent() -> Result<()> {
    let f = Fixture::new()?;
    let original = f.roots.codex_sessions.join("rollout-known.jsonl");
    let legacy = f.trash.trash_dir.join("1-rollout-known.jsonl");
    write(
        &legacy,
        br#"{"type":"session_meta","payload":{"id":"known"}}"#,
    )?;
    write(&sidecar_path(&legacy), b"summary")?;
    write(
        &f.trash.trash_dir.join("orphan.jsonl"),
        br#"{"type":"user","sessionId":"orphan","message":{"role":"user","content":"orphan"}}"#,
    )?;
    let record = serde_json::json!({
        "nm": "1-rollout-known.jsonl", "ts": "2026-11-10T00:00:00Z",
        "op": original, "tn": "codex",
    });
    let catalog = format!("bad JSON\n{record}\n");
    fs::write(&f.trash.metadata_file, &catalog)?;
    fs::write(&f.trash.reasons_file, b"{ broken")?;
    assert_success(f.cli(&["--json", "--trashed", "yes", "-g"], None)?);
    assert!(legacy.exists());
    assert_eq!(fs::read_to_string(&f.trash.metadata_file)?, catalog);

    assert_success(f.cli(
        &["--export-trash"],
        Some(&f.temp.path().join("migrated.zip")),
    )?);
    assert!(!legacy.exists() && !f.trash.metadata_file.exists() && !f.trash.reasons_file.exists());
    assert_eq!(
        fs::read_to_string(f.trash.trash_dir.join(".legacy-migration/trash.jsonl"))?,
        catalog
    );
    assert_eq!(
        fs::read(f.trash.trash_dir.join(".legacy-migration/trash.json"))?,
        b"{ broken"
    );
    let items = f.store().snapshot()?;
    assert_eq!(items.len(), 2);
    let known = items
        .iter()
        .find(|i| i.metadata.agent == Some(Agent::Codex))
        .unwrap();
    assert_eq!(known.original_path(), Some(original));
    let primary = known.primary.as_ref().unwrap();
    assert!(primary.ends_with("rollout-known.jsonl"));
    assert_eq!(fs::read(sidecar_path(primary))?, b"summary");
    let orphan = items
        .iter()
        .find(|i| i.metadata.agent == Some(Agent::Claude))
        .unwrap();
    assert!(orphan.primary.is_some() && orphan.original_path().is_none());
    lifecycle::initialize_trash(&f.roots)?;
    assert_eq!(
        f.store()
            .snapshot()?
            .into_iter()
            .map(|i| i.path)
            .collect::<Vec<_>>(),
        items.into_iter().map(|i| i.path).collect::<Vec<_>>()
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn unsafe_zip_entry_stops_archive_without_removing_any_items() -> Result<()> {
    let f = Fixture::new()?;
    let source = f.codex("codex", None)?;
    let item = trash_fixture::trash_session(&f.roots, &source, Agent::Codex, reason())?;
    let unexpected = item.path.join("payload/name\\with-backslash");
    fs::write(&unexpected, b"preserve me")?;
    let zip = f.temp.path().join("trash.zip");
    let output = f.cli(&["--archive-trash"], Some(&zip))?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsafe ZIP entry name"));
    assert!(!zip.exists());
    assert_eq!(fs::read(unexpected)?, b"preserve me");
    assert!(item.primary.unwrap().is_file());
    Ok(())
}
