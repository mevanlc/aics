use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aics::lifecycle::{self, DependencyGraph, Operation};
use aics::parse::Agent;
use aics::scan::SessionRoots;
use aics::summary::sidecar_path;
use aics::trash::{TrashPaths, TrashReason, TrashStore};
use anyhow::Result;
use tempfile::TempDir;
use zip::ZipArchive;

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
            live_sessions: Default::default(),
            claude_projects: temp.path().join("claude"),
            codex_sessions: temp.path().join("codex/sessions"),
            antigravity_home: temp.path().join("antigravity"),
            trash: Some(trash.clone()),
        };
        Ok(Self { temp, roots, trash })
    }

    fn cli(&self, args: &[&str], destination: Option<&Path>) -> Result<Output> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aics"));
        command
            .current_dir(self.temp.path())
            .env("AICS_CONFIG_ROOT", self.temp.path().join("config"))
            .env("AICS_CACHE_ROOT", self.temp.path().join("cache"))
            .env("AICS_DATA_ROOT", &self.trash.data_root)
            .env("AICS_CLAUDE_PROJECTS_DIR", &self.roots.claude_projects)
            .env("AICS_CODEX_SESSIONS_DIR", &self.roots.codex_sessions)
            .env("AICS_ANTIGRAVITY_HOME", &self.roots.antigravity_home)
            .args(args);
        if let Some(path) = destination {
            command.arg(path);
        }
        Ok(command.output()?)
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

    assert_success(f.cli(&["--yes", "--archive-trash"], Some(&zip))?);
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
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("must be trashed first"));
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
