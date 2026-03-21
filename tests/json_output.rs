use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;
use tempfile::TempDir;

#[test]
fn json_mode_emits_valid_jsonl_hits() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let cache_root = temp.path().join("cache");

    let output = Command::new(env!("CARGO_BIN_EXE_aics"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("AICS_CACHE_ROOT", &cache_root)
        .env("AICS_CLAUDE_PROJECTS_DIR", &roots.0)
        .env("AICS_CODEX_SESSIONS_DIR", &roots.1)
        .args(["--json", "-g", "-n", "2", "--agent", "claude", "git status"])
        .output()?;

    assert!(output.status.success(), "{output:#?}");
    let stdout = String::from_utf8(output.stdout)?;
    let lines = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1);

    let value: serde_json::Value = serde_json::from_str(lines[0])?;
    assert_eq!(value["session"]["agent"], "claude");
    assert_eq!(
        value["session"]["session_id"],
        "c0d1e2f3-a4b5-4c6d-8e7f-9a0b1c2d3e4f"
    );
    assert!(value["snippet_html"].as_str().is_some_and(|snippet| !snippet.is_empty()));
    Ok(())
}

fn fixture_roots(temp: &TempDir) -> Result<(PathBuf, PathBuf)> {
    copy_fixture(
        temp,
        "tests/fixtures/sessions/claude/basic_session.jsonl",
        ".claude/projects/-Users-testuser-projects-myapp/basic_session.jsonl",
    )?;
    copy_fixture(
        temp,
        "tests/fixtures/sessions/codex/new_format.jsonl",
        ".codex/sessions/2025/12/10/rollout-new.jsonl",
    )?;

    Ok((
        temp.path().join(".claude/projects"),
        temp.path().join(".codex/sessions"),
    ))
}

fn copy_fixture(temp: &TempDir, from: &str, to: &str) -> Result<PathBuf> {
    let source = fixture_path(from);
    let destination = temp.path().join(to);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, &destination)?;
    Ok(destination)
}

fn fixture_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}
