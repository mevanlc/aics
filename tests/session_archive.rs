use std::collections::BTreeSet;
use std::fs::{self, File, FileTimes};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, UNIX_EPOCH};

use aics::lifecycle::{self, DependencyGraph, Operation};
use aics::live::LiveSessionPaths;
use aics::parse::Agent;
use aics::scan::SessionRoots;
use aics::summary::sidecar_path;
use aics::trash::{TrashItem, TrashPaths, TrashReason, TrashStore};
use anyhow::Result;
use serde_json::{json, Value};
use tempfile::TempDir;
use zip::ZipArchive;

#[path = "common/lock_holder.rs"]
mod lock_holder;
#[path = "support/trash.rs"]
mod trash_fixture;

struct Fixture {
    temp: TempDir,
    roots: SessionRoots,
}

impl Fixture {
    fn new() -> Result<Self> {
        let temp = TempDir::new()?;
        let roots = SessionRoots {
            live_sessions: LiveSessionPaths {
                claude_sessions_dir: Some(temp.path().join("claude/sessions")),
                codex_writer_locks_dir: Some(temp.path().join("codex/thread-writer-locks")),
                antigravity_presence_dir: Some(temp.path().join("antigravity/presence")),
            },
            claude_projects: temp.path().join("claude/projects"),
            codex_sessions: temp.path().join("codex/sessions"),
            antigravity_home: temp.path().join("antigravity"),
            trash: Some(TrashPaths::from_data_root(temp.path().join("data"))),
        };
        Ok(Self { temp, roots })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aics"));
        command
            .current_dir(self.temp.path())
            .env("PWD", self.temp.path())
            .env("AICS_CONFIG_ROOT", self.temp.path().join("config"))
            .env("AICS_CACHE_ROOT", self.temp.path().join("cache"))
            .env(
                "AICS_DATA_ROOT",
                &self.roots.trash.as_ref().unwrap().data_root,
            )
            .arg("--claude-home")
            .arg(self.temp.path().join("claude"))
            .arg("--codex-home")
            .arg(self.temp.path().join("codex"))
            .arg("--antigravity-home")
            .arg(&self.roots.antigravity_home)
            .args(["--progress", "none"]);
        command
    }

    fn run(&self, args: &[&str], destination: &Path) -> Output {
        self.command().args(args).arg(destination).output().unwrap()
    }

    fn claude(&self, id: &str, cwd: &str, text: &str) -> Result<PathBuf> {
        let path = self
            .roots
            .claude_projects
            .join(format!("project/{id}.jsonl"));
        write(&path, format!("{}\n", json!({"type":"user","sessionId":id,"cwd":cwd,"gitBranch":"main","message":{"role":"user","content":text}})).as_bytes())?;
        Ok(path)
    }

    fn codex(
        &self,
        thread: &str,
        rollout: &str,
        base: Option<(&str, &Path)>,
        text: &str,
    ) -> Result<PathBuf> {
        let name = if thread == rollout {
            thread.to_owned()
        } else {
            format!("{thread}_{rollout}")
        };
        let path = self
            .roots
            .codex_sessions
            .join(format!("rollout-{name}.jsonl"));
        let mut payload = json!({"id":thread,"cwd":self.temp.path(),"history_mode":"paginated"});
        let ordinal = if let Some((parent, source)) = base {
            let lines = fs::read_to_string(source)?;
            let last: Value = serde_json::from_str(lines.lines().last().unwrap())?;
            let end = last["ordinal"].as_u64().unwrap() + 1;
            payload["history_base"] = json!({"thread_id":parent,"end_ordinal_exclusive":end,"end_byte_offset":fs::metadata(source)?.len()});
            end
        } else {
            0
        };
        let records = [
            json!({"ordinal":ordinal,"type":"session_meta","payload":payload}),
            json!({"ordinal":ordinal+1,"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":text}]}}),
        ];
        write(
            &path,
            records
                .iter()
                .map(|r| format!("{r}\n"))
                .collect::<String>()
                .as_bytes(),
        )?;
        Ok(path)
    }

    fn trash(&self, path: &Path, agent: Agent) -> Result<()> {
        trash_fixture::trash_session(
            &self.roots,
            path,
            agent,
            TrashReason::KeyBinding { key: "^T".into() },
        )?;
        Ok(())
    }

    fn items(&self, zip: &Path) -> Result<Vec<TrashItem>> {
        let extracted = zip.with_extension("unpacked");
        ZipArchive::new(File::open(zip)?)?.extract(&extracted)?;
        TrashStore::new(TrashPaths::from_data_root(extracted)).snapshot()
    }
}

fn write(path: &Path, content: &[u8]) -> Result<()> {
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(path, content)?;
    Ok(())
}

fn success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn originals(items: &[TrashItem]) -> BTreeSet<String> {
    items
        .iter()
        .map(|i| {
            i.original_path()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

#[test]
fn filters_limits_saved_settings_and_startup_rules_select_only_cli_matches() -> Result<()> {
    let f = Fixture::new()?;
    let cwd = f.temp.path().to_str().unwrap();
    let old = f.claude("old", cwd, "needle")?;
    let recent = f.claude("recent", cwd, "needle")?;
    f.claude("unmatched", cwd, "other text")?;
    f.claude("elsewhere", "/other/project", "needle")?;
    File::options().write(true).open(old)?.set_times(
        FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
    )?;
    File::options().write(true).open(&recent)?.set_times(
        FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1_800_000_000)),
    )?;
    write(
        &f.temp.path().join("config/settings.json"),
        br#"{"default_filter":{"scope":"global","filters":{"agent":"codex","min_lines":200}},"display_options":{"hide_user_messages":true}}"#,
    )?;
    write(
        &f.temp.path().join("config/rules.js"),
        b"rule('unexpected', { applyAtStartup: true }, () => trash('startup must not run'));",
    )?;

    for sort in ["time", "relevance"] {
        let zip = f.temp.path().join(format!("{sort}.zip"));
        let output = success(f.run(
            &[
                "visible: needle",
                "--agent",
                "claude",
                "--branch",
                "main",
                "--after",
                "2026-01-01",
                "--sort-by",
                sort,
                "-n",
                "1",
                "--export-sessions",
            ],
            &zip,
        ));
        assert_eq!(
            String::from_utf8(output.stdout)?.trim(),
            zip.to_str().unwrap(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(originals(&f.items(&zip)?), ["recent.jsonl".into()].into());
    }
    assert!(recent.is_file());
    Ok(())
}

#[test]
fn zip_commands_process_more_than_2000_matches_unless_explicitly_limited() -> Result<()> {
    let f = Fixture::new()?;
    for n in 0..2001 {
        f.claude(&format!("session-{n}"), "/project", "batch needle")?;
    }
    for (name, query, limit, expected) in [
        ("all", "", None, 2001),
        ("all-query", "batch", None, 2001),
        ("limited", "batch", Some("2"), 2),
    ] {
        let zip = f.temp.path().join(format!("{name}.zip"));
        let mut command = f.command();
        command
            .args(["-g", query, "--sort-by", "relevance", "--export-sessions"])
            .arg(&zip);
        if let Some(limit) = limit {
            command.args(["-n", limit]);
        }
        success(command.output()?);
        assert_eq!(f.items(&zip)?.len(), expected);
    }
    Ok(())
}

#[test]
fn regular_trash_and_mixed_provider_archives_restore_complete_payloads() -> Result<()> {
    let f = Fixture::new()?;
    let cwd = f.temp.path().to_str().unwrap();
    let claude = f.claude("claude", cwd, "regular session")?;
    let codex = f.codex("codex", "codex", None, "trashed session")?;
    let agy = f
        .roots
        .antigravity_home
        .join("brain/agy/.system_generated/logs/transcript.jsonl");
    write(&agy, b"{\"step_index\":0,\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"status\":\"DONE\",\"content\":\"antigravity session\"}\n")?;
    let db = f.roots.antigravity_home.join("conversations/agy.db");
    write(&db, b"database bytes")?;
    let companion = f.roots.antigravity_home.join("conversations/agy.db-wal");
    write(&companion, b"wal bytes")?;
    for path in [&claude, &codex, &agy] {
        write(&sidecar_path(path), b"summary bytes")?;
    }
    let originals_before = [&claude, &codex, &agy]
        .into_iter()
        .map(|p| Ok((p.clone(), fs::read(p)?)))
        .collect::<Result<Vec<_>>>()?;
    f.trash(&codex, Agent::Codex)?;
    let reason_before = TrashStore::new(f.roots.trash.clone().unwrap())
        .snapshot()?
        .pop()
        .unwrap()
        .metadata;
    let only_trash = f.temp.path().join("only-trash.zip");
    success(f.run(
        &["-g", "--trashed", "yes", "--export-sessions"],
        &only_trash,
    ));
    let trash_items = f.items(&only_trash)?;
    assert_eq!(trash_items.len(), 1);
    assert_eq!(trash_items[0].metadata, reason_before);
    let regular = f.temp.path().join("regular.zip");
    success(f.run(&["-g", "--export-sessions"], &regular));
    assert_eq!(f.items(&regular)?.len(), 2);

    let zip = f.temp.path().join("mixed.zip");
    success(f.run(&["-g", "--trashed", "both", "--archive-sessions"], &zip));
    assert!(!claude.exists() && !agy.exists() && !db.exists() && !companion.exists());
    assert!(TrashStore::new(f.roots.trash.clone().unwrap())
        .snapshot()?
        .is_empty());
    let items = f.items(&zip)?;
    assert_eq!(items.len(), 3);
    for item in &items {
        if item.metadata.agent != Some(Agent::Codex) {
            assert!(item.metadata.trashed_at.is_none() && item.metadata.reason.is_none());
        }
    }
    let mut roots = f.roots.clone();
    roots.trash = Some(TrashPaths::from_data_root(zip.with_extension("unpacked")));
    for item in items {
        let primary = item.primary.unwrap();
        let plan = DependencyGraph::discover(
            &roots,
            item.metadata.agent.unwrap(),
            std::slice::from_ref(&primary),
        )?
        .plan([primary].into(), Operation::Restore);
        lifecycle::execute(&roots, &plan, &Default::default())?;
    }
    for (path, bytes) in originals_before {
        assert_eq!(fs::read(&path)?, bytes);
        assert_eq!(fs::read(sidecar_path(&path))?, b"summary bytes");
    }
    assert_eq!(fs::read(db)?, b"database bytes");
    assert_eq!(fs::read(companion)?, b"wal bytes");
    Ok(())
}

#[test]
fn codex_pages_and_compressed_ancestors_are_copied_but_only_selected_thread_is_removed(
) -> Result<()> {
    let f = Fixture::new()?;
    let parent = f.codex("parent", "parent", None, "parent history")?;
    let first = f.codex("child", "first", Some(("parent", &parent)), "first page")?;
    let last = f.codex("child", "last", Some(("first", &first)), "last page")?;
    let archived = f
        .temp
        .path()
        .join("codex/archived_sessions/rollout-parent.jsonl.zst");
    write(
        &archived,
        &zstd::stream::encode_all(fs::read(&parent)?.as_slice(), 1)?,
    )?;
    fs::remove_file(parent)?;
    let bytes = fs::read(&archived)?;
    let zip = f.temp.path().join("child.zip");
    success(f.run(&["-g", "--session", "child", "--archive-sessions"], &zip));
    assert!(!first.exists() && !last.exists());
    assert_eq!(fs::read(&archived)?, bytes);
    let items = f.items(&zip)?;
    assert_eq!(items.len(), 3);
    let parent = items
        .iter()
        .find(|i| i.primary.as_ref().unwrap().extension().unwrap() == "zst")
        .unwrap();
    assert_eq!(fs::read(parent.primary.as_ref().unwrap())?, bytes);
    Ok(())
}

#[test]
fn related_removal_requires_opt_in_and_fork_context_does_not_expand() -> Result<()> {
    let f = Fixture::new()?;
    let parent = f.codex("parent", "parent", None, "parent history")?;
    let child = f.codex("child", "child", Some(("parent", &parent)), "child history")?;
    let unrelated = f.codex("fork", "fork", None, "fork history")?;
    let text = fs::read_to_string(&unrelated)?.replace(
        "\"id\":\"fork\"",
        "\"id\":\"fork\",\"forked_from_id\":\"parent\"",
    );
    fs::write(&unrelated, text)?;
    let zip = f.temp.path().join("related.zip");
    let denied = f.run(&["-g", "--session", "parent", "--archive-sessions"], &zip);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("--include-related"));
    assert!(!zip.exists() && parent.exists() && child.exists());
    success(f.run(
        &[
            "-g",
            "--session",
            "parent",
            "--include-related",
            "--archive-sessions",
        ],
        &zip,
    ));
    assert!(!parent.exists() && !child.exists() && unrelated.exists());
    assert_eq!(f.items(&zip)?.len(), 2);
    Ok(())
}

#[test]
fn live_locked_and_indeterminate_sessions_skip_with_their_required_groups() -> Result<()> {
    let f = Fixture::new()?;
    let cwd = f.temp.path().to_str().unwrap();
    let idle = f.claude("idle", cwd, "idle session")?;
    let live = f.claude("live", cwd, "live session")?;
    write(
        &f.temp.path().join("claude/sessions/live.json"),
        format!("{}", json!({"sessionId":"live","pid":std::process::id()})).as_bytes(),
    )?;
    let parent = f.codex("parent", "parent", None, "parent")?;
    let child = f.codex("child", "child", Some(("parent", &parent)), "child")?;
    write(
        &f.temp
            .path()
            .join("codex/thread-writer-locks/.coordination.lock"),
        b"",
    )?;
    let _held =
        lock_holder::HeldLock::new(&f.temp.path().join("codex/thread-writer-locks/child.lock"));
    let export = f.temp.path().join("export.zip");
    success(f.run(&["-g", "--export-sessions"], &export));
    assert_eq!(f.items(&export)?.len(), 2); // inactive parent can be copied
    let archive = f.temp.path().join("archive.zip");
    success(f.run(&["-g", "--include-related", "--archive-sessions"], &archive));
    assert_eq!(originals(&f.items(&archive)?), ["idle.jsonl".into()].into());
    assert!(!idle.exists() && live.exists() && parent.exists() && child.exists());
    let none = f.temp.path().join("none.zip");
    let out = success(f.run(&["-g", "--live", "--export-sessions"], &none));
    assert!(out.stdout.is_empty() && !none.exists());
    write(
        &f.temp.path().join("claude/sessions/broken.json"),
        b"malformed marker",
    )?;
    let unknown = f.temp.path().join("unknown.zip");
    success(f.run(&["-g", "--agent", "claude", "--export-sessions"], &unknown));
    assert!(!unknown.exists());
    Ok(())
}

#[test]
fn live_ancestor_skips_a_child_even_for_export() -> Result<()> {
    let f = Fixture::new()?;
    let parent = f.codex("parent", "parent", None, "parent")?;
    let child = f.codex("child", "child", Some(("parent", &parent)), "child")?;
    let _held =
        lock_holder::HeldLock::new(&f.temp.path().join("codex/thread-writer-locks/parent.lock"));
    let zip = f.temp.path().join("protected.zip");
    success(f.run(&["-g", "--session", "child", "--export-sessions"], &zip));
    assert!(!zip.exists() && parent.exists() && child.exists());
    Ok(())
}

#[test]
fn filtered_trash_archive_can_explicitly_include_active_dependents() -> Result<()> {
    let f = Fixture::new()?;
    let parent = f.codex("parent", "parent", None, "parent")?;
    f.trash(&parent, Agent::Codex)?;
    let trashed = TrashStore::new(f.roots.trash.clone().unwrap())
        .snapshot()?
        .pop()
        .unwrap()
        .primary
        .unwrap();
    let child = f.codex("child", "child", Some(("parent", &trashed)), "child")?;
    let zip = f.temp.path().join("trash-related.zip");
    assert!(!f
        .run(&["-g", "--trashed", "yes", "--archive-sessions"], &zip)
        .status
        .success());
    assert!(trashed.exists() && child.exists() && !zip.exists());
    success(f.run(
        &[
            "-g",
            "--trashed",
            "yes",
            "--include-related",
            "--archive-sessions",
        ],
        &zip,
    ));
    assert!(!trashed.exists() && !child.exists());
    assert_eq!(f.items(&zip)?.len(), 2);
    Ok(())
}

#[test]
fn explicit_limits_are_applied_before_protection_skips_without_backfilling() -> Result<()> {
    let f = Fixture::new()?;
    f.claude("idle", "/project", "idle")?;
    let live = f.claude("live", "/project", "live")?;
    File::options().write(true).open(&live)?.set_times(
        FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1_800_000_000)),
    )?;
    write(
        &f.temp.path().join("claude/sessions/live.json"),
        format!("{{\"sessionId\":\"live\",\"pid\":{}}}", std::process::id()).as_bytes(),
    )?;
    let zip = f.temp.path().join("limited-live.zip");
    success(f.run(&["-g", "-n", "1", "--export-sessions"], &zip));
    assert!(!zip.exists());
    Ok(())
}

#[test]
fn visibility_flags_do_not_redact_original_zip_payloads() -> Result<()> {
    let f = Fixture::new()?;
    let session = f.claude("source", "/project", "complete original message")?;
    let original = fs::read(&session)?;
    let zip = f.temp.path().join("complete.zip");
    success(f.run(
        &["-g", "--hide", "user-messages", "--export-sessions"],
        &zip,
    ));
    let items = f.items(&zip)?;
    assert_eq!(items.len(), 1);
    assert_eq!(fs::read(items[0].primary.as_ref().unwrap())?, original);
    Ok(())
}

#[test]
fn unlocked_native_markers_remain_idle_while_archive_holds_guards() -> Result<()> {
    let f = Fixture::new()?;
    let codex = f.codex("idle-codex", "idle-codex", None, "idle Codex session")?;
    let agy = f
        .roots
        .antigravity_home
        .join("brain/idle-agy/.system_generated/logs/transcript.jsonl");
    write(&agy, b"{\"step_index\":0,\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"status\":\"DONE\",\"content\":\"idle Antigravity session\"}\n")?;
    let markers = [
        f.temp
            .path()
            .join("codex/thread-writer-locks/.coordination.lock"),
        f.temp
            .path()
            .join("codex/thread-writer-locks/idle-codex.lock"),
        f.temp.path().join("antigravity/presence/idle-agy.lock"),
    ];
    for marker in &markers {
        write(marker, b"idle marker")?;
    }
    let export = f.temp.path().join("idle-export.zip");
    success(f.run(&["-g", "--export-sessions"], &export));
    assert_eq!(f.items(&export)?.len(), 2);
    let archive = f.temp.path().join("idle-archive.zip");
    success(f.run(&["-g", "--archive-sessions"], &archive));
    assert_eq!(f.items(&archive)?.len(), 2);
    assert!(!codex.exists() && !agy.exists());
    for marker in markers {
        assert_eq!(fs::read(marker)?, b"idle marker");
    }
    Ok(())
}

#[test]
fn empty_batches_preserve_destinations_and_nonempty_overwrites_require_yes() -> Result<()> {
    let f = Fixture::new()?;
    f.claude("session", "/project", "hello")?;
    let zip = f.temp.path().join("existing.zip");
    fs::write(&zip, b"original destination")?;
    let empty = success(f.run(&["-g", "--session", "missing", "--export-sessions"], &zip));
    assert!(empty.stdout.is_empty());
    assert_eq!(fs::read(&zip)?, b"original destination");
    assert!(!f.run(&["-g", "--export-sessions"], &zip).status.success());
    assert_eq!(fs::read(&zip)?, b"original destination");
    success(f.run(&["-g", "--yes", "--export-sessions"], &zip));
    assert_eq!(f.items(&zip)?.len(), 1);
    Ok(())
}

#[test]
fn source_destination_overlap_and_missing_history_leave_originals_intact() -> Result<()> {
    let f = Fixture::new()?;
    let session = f.claude("source", "/project", "original source")?;
    let original = fs::read(&session)?;
    assert!(!f
        .run(&["-g", "--yes", "--archive-sessions"], &session)
        .status
        .success());
    assert_eq!(fs::read(&session)?, original);
    let parent = f.codex("parent", "parent", None, "parent")?;
    let child = f.codex("child", "child", Some(("parent", &parent)), "child")?;
    fs::remove_file(parent)?;
    let zip = f.temp.path().join("missing-history.zip");
    assert!(!f
        .run(&["-g", "--session", "child", "--archive-sessions"], &zip)
        .status
        .success());
    assert!(child.exists() && !zip.exists());
    Ok(())
}
