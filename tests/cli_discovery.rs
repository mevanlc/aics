use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::Result;
use serde_json::{json, Value};
use tempfile::TempDir;

#[path = "common/lock_holder.rs"]
mod lock_holder;

struct Fixture(TempDir);

impl Fixture {
    fn new() -> Result<Self> {
        let fixture = Self(TempDir::new()?);
        fs::create_dir_all(fixture.path("codex/sessions"))?;
        Ok(fixture)
    }
    fn path(&self, path: &str) -> PathBuf {
        self.0.path().join(path)
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aics"));
        command
            .current_dir(self.0.path())
            .env("AICS_CONFIG_ROOT", self.path("config"))
            .env("AICS_CACHE_ROOT", self.path("cache"))
            .env("AICS_DATA_ROOT", self.path("data"))
            .env("AICS_CLAUDE_PROJECTS_DIR", self.path("claude/projects"))
            .env("AICS_CODEX_SESSIONS_DIR", self.path("codex/sessions"))
            .env("AICS_ANTIGRAVITY_HOME", self.path("antigravity"));
        command
    }
    fn session(&self, id: &str, cwd: &str, prompt: &str) -> Result<PathBuf> {
        let records = [
            json!({"type":"session_meta", "payload":{"id":id,"cwd":cwd,"source":"cli","timestamp":"2026-01-01T12:00:00Z"}}),
            message("user", &format!("# AGENTS.md instructions\n\n<INSTRUCTIONS>InjectedDocsNeedle {} </INSTRUCTIONS>", "irrelevant instruction ".repeat(2000))),
            message("user", prompt),
            json!({"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"ThinkingOnlyNeedle"}]}}),
            json!({"type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call1","arguments":"{\"cmd\":\"rg ToolCallNeedle /work/reusable-path\"}"}}),
            json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"call1","output":"ToolResultNeedle"}}),
            message("assistant", "Completed the useful work. <b>literal markup</b>"),
        ];
        let path = self.path(&format!("codex/sessions/rollout-{id}.jsonl"));
        fs::write(
            &path,
            records
                .iter()
                .map(|record| format!("{record}\n"))
                .collect::<String>(),
        )?;
        Ok(path)
    }
    fn hits(&self, args: &[&str]) -> Result<Vec<Value>> {
        hits(
            self.command()
                .args(["--json", "--progress", "none"])
                .args(args)
                .output()?,
        )
    }
    fn profile(&self) -> Result<PathBuf> {
        Ok(fs::read_dir(self.path("cache/profiles"))?
            .next()
            .unwrap()?
            .path())
    }
}

fn message(role: &str, text: &str) -> Value {
    json!({"type":"response_item","payload":{"type":"message","role":role,"content":[{"type":"input_text","text":text}]}})
}

fn hits(output: Output) -> Result<Vec<Value>> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)?
        .lines()
        .map(|line| Ok(serde_json::from_str(line)?))
        .collect()
}

#[test]
fn compact_output_is_bounded_and_full_records_remain_available() -> Result<()> {
    let f = Fixture::new()?;
    f.session(
        "bounded",
        "/work/project",
        &format!("DiscoveryNeedle {}", "🦀 \n\t".repeat(10000)),
    )?;
    let records = f.hits(&["-g", "DiscoveryNeedle"])?;
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert!(record.to_string().len() < 7000);
    assert!(record["session"].get("first_msg_content").is_none());
    assert!(record["session"].get("last_msg_content").is_none());
    assert!(record["session"].get("session_info").is_none());
    for text in [
        record["snippet"].as_str().unwrap(),
        record["session"]["first_user_msg_content"]
            .as_str()
            .unwrap(),
    ] {
        assert!(text.chars().count() <= 512);
        assert!(!text.contains('\n'));
        assert!(!text.contains("  "));
    }
    let full = hits(
        f.command()
            .args(["--json=full", "-g", "DiscoveryNeedle"])
            .output()?,
    )?;
    assert!(full[0]["session"]["first_msg_content"]
        .as_str()
        .unwrap()
        .contains("InjectedDocsNeedle"));
    assert!(full[0].get("snippet_html").is_some());
    Ok(())
}

#[test]
fn visibility_covers_search_snippets_overrides_and_no_query_fallbacks() -> Result<()> {
    let f = Fixture::new()?;
    f.session("visible", "/work/project", "A short real prompt.")?;
    for needle in [
        "InjectedDocsNeedle",
        "ThinkingOnlyNeedle",
        "ToolResultNeedle",
    ] {
        assert!(f.hits(&["-g", needle])?.is_empty(), "{needle}");
        assert_eq!(f.hits(&["-g", &format!("all: {needle}")])?.len(), 1);
    }
    assert!(f.hits(&["-g", "ToolCallNeedle"])?[0]["snippet"]
        .as_str()
        .unwrap()
        .contains("ToolCallNeedle"));
    assert_eq!(f.hits(&["-g", "toolresult:ToolResultNeedle"])?.len(), 1);
    assert_eq!(
        f.hits(&[
            "-g",
            "--hide",
            "tool-results",
            "--show",
            "tool-results",
            "ToolResultNeedle"
        ])?
        .len(),
        1
    );
    assert!(f
        .hits(&[
            "-g",
            "--show",
            "tool-results",
            "--hide",
            "tool-results",
            "ToolResultNeedle"
        ])?
        .is_empty());
    let records = f.hits(&["-g"])?;
    let text = records[0].to_string();
    for needle in [
        "InjectedDocsNeedle",
        "ThinkingOnlyNeedle",
        "ToolResultNeedle",
    ] {
        assert!(!text.contains(needle), "{needle}");
    }
    assert!(text.contains("ToolCallNeedle"));
    let records = f.hits(&["-g", "--hide", "user-messages", "--hide", "tool-calls"])?;
    assert_eq!(records[0]["session"]["first_user_msg_content"], "");
    assert!(records[0]["snippet"]
        .as_str()
        .unwrap()
        .contains("<b>literal markup</b>"));
    Ok(())
}

#[test]
fn json_defaults_to_twenty_and_ignores_tui_preferences_and_startup_rules() -> Result<()> {
    let f = Fixture::new()?;
    for i in 0..25 {
        f.session(&format!("session-{i}"), "/work/project", "SearchableNeedle")?;
    }
    fs::create_dir_all(f.path("config"))?;
    fs::write(
        f.path("config/settings.json"),
        r#"{"display_options":{"hide_user_messages":true,"hide_tool_calls":true,"hide_agent_replies":true},"default_filter":{"scope":"local","sort":"time","filters":{"agent":"claude"}}}"#,
    )?;
    fs::write(
        f.path("config/rules.js"),
        "rule('remove', { applyAtStartup: true }, () => trash('must not run'));",
    )?;
    fs::create_dir_all(f.path("data"))?;
    fs::write(f.path("data/trash.jsonl"), b"\n")?;
    assert_eq!(f.hits(&["-g", "SearchableNeedle"])?.len(), 20);
    assert_eq!(f.hits(&["-g", "-n", "100", "SearchableNeedle"])?.len(), 25);
    assert_eq!(fs::read(f.path("data/trash.jsonl"))?, b"\n");
    assert_eq!(fs::read_dir(f.path("codex/sessions"))?.count(), 25);
    assert!(!f.profile()?.join("startup-rules-cache.json").exists());
    Ok(())
}

#[test]
fn parallel_cold_and_warm_json_searches_succeed() -> Result<()> {
    let f = Fixture::new()?;
    f.session("parallel", "/work/project", "ParallelNeedle")?;
    for _ in 0..2 {
        let mut children = Vec::new();
        for _ in 0..6 {
            children.push(
                f.command()
                    .args(["--json", "-g", "ParallelNeedle"])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()?,
            );
        }
        for child in children {
            assert_eq!(hits(child.wait_with_output()?)?.len(), 1);
        }
    }
    Ok(())
}

#[test]
fn searches_use_snapshot_during_lifecycle_or_cache_contention_and_pending_recovery() -> Result<()> {
    let f = Fixture::new()?;
    let source = f.session("snapshot", "/work/project", "SnapshotNeedle")?;
    let before = fs::read(&source)?;
    f.hits(&["-g"])?;
    for lock in [
        f.path("data/lifecycle.lock"),
        f.profile()?.join("maintenance.lock"),
    ] {
        let _held = lock_holder::HeldLock::new(&lock);
        let output = f
            .command()
            .args(["--json", "-g", "SnapshotNeedle"])
            .output()?;
        assert!(String::from_utf8_lossy(&output.stderr).contains("last committed snapshot"));
        assert_eq!(hits(output)?.len(), 1);
    }
    let operation = f.path("data/operations/interrupted");
    fs::create_dir_all(&operation)?;
    fs::write(
        operation.join("operation.json"),
        "deliberately unreadable journal",
    )?;
    assert_eq!(f.hits(&["-g", "SnapshotNeedle"])?.len(), 1);
    assert_eq!(fs::read(source)?, before);
    assert_eq!(
        fs::read_to_string(operation.join("operation.json"))?,
        "deliberately unreadable journal"
    );
    fs::remove_dir_all(f.path("cache"))?;
    let output = f.command().args(["--json", "-g"]).output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("awaits recovery"));
    Ok(())
}

#[test]
fn directory_matching_is_insensitive_with_an_explicit_sensitive_override() -> Result<()> {
    let f = Fixture::new()?;
    for (id, path) in [
        ("case", "/Work/CAFÉ"),
        ("sibling", "/Work/CAFÉ-extra"),
        ("child", "/Work/CAFÉ/child"),
    ] {
        f.session(id, path, "DirectoryNeedle")?;
    }
    for query in [vec![], vec!["DirectoryNeedle"]] {
        let mut args = vec!["--dir", "/work/café/"];
        args.extend(query);
        let records = f.hits(&args)?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["session"]["session_id"], "case");
        args.push("--case-sensitive-dir");
        assert!(f.hits(&args)?.is_empty());
    }
    assert_eq!(
        f.hits(&["--dir", "/Work/CAFÉ", "--case-sensitive-dir"])?
            .len(),
        1
    );
    assert_eq!(f.hits(&["-g", "wd:café"])?.len(), 3);
    Ok(())
}

#[test]
fn skill_printing_does_not_initialize_storage() -> Result<()> {
    let f = Fixture::new()?;
    let output = f.command().arg("--print-skill").output()?;
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout)?,
        aics::manpage::render_skill()
    );
    for directory in ["data", "cache", "config"] {
        assert!(!f.path(directory).exists());
    }
    Ok(())
}

#[test]
fn older_directory_matches_survive_small_limits_and_both_sort_modes() -> Result<()> {
    let f = Fixture::new()?;
    // Keep these fixtures small: this regression exercises candidate selection,
    // not the transcript parser's large-message handling.
    for i in 0..5101 {
        let cwd = if i == 0 {
            "/old/project"
        } else {
            "/unrelated/project"
        };
        let path = f.path(&format!("codex/sessions/rollout-{i}.jsonl"));
        fs::write(
            &path,
            format!(
                "{}\n{}\n",
                json!({"type":"session_meta","payload":{"id":format!("old-{i}"),"cwd":cwd}}),
                message("user", "SharedNeedle")
            ),
        )?;
        fs::File::options().write(true).open(path)?.set_times(
            fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1700000000 + i)),
        )?;
    }
    for sort in ["time", "relevance"] {
        for query in [vec![], vec!["SharedNeedle"]] {
            let mut args = vec!["--dir", "/old/project", "-n", "1", "--sort-by", sort];
            args.extend(query);
            let records = f.hits(&args)?;
            assert_eq!(records.len(), 1);
            assert_eq!(records[0]["session"]["session_id"], "old-0");
        }
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn strict_scope_preserves_symlink_aliases_without_accepting_case_only_aliases() -> Result<()> {
    let f = Fixture::new()?;
    let real = f.path("RealProject");
    fs::create_dir(&real)?;
    let real = real.canonicalize()?;
    let link = f.path("alias");
    std::os::unix::fs::symlink(&real, &link)?;
    f.session("alias", real.to_str().unwrap(), "AliasNeedle")?;
    assert_eq!(
        f.hits(&["--dir", link.to_str().unwrap(), "--case-sensitive-dir"])?
            .len(),
        1
    );
    let wrong_case = f.path("realproject");
    assert!(f
        .hits(&[
            "--dir",
            wrong_case.to_str().unwrap(),
            "--case-sensitive-dir"
        ])?
        .is_empty());
    Ok(())
}
