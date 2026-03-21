use std::fs;
use std::path::{Path, PathBuf};

use aics::parse::{
    parse_claude_session_file, parse_codex_session_file, Agent, DerivationType, MessageRole,
};
use aics::scan::{scan_session_files, SessionRoots};
use anyhow::Result;
use tempfile::TempDir;

#[test]
fn parses_claude_basic_session() -> Result<()> {
    let temp = TempDir::new()?;
    let path = copy_fixture(
        &temp,
        "tests/fixtures/sessions/claude/basic_session.jsonl",
        ".claude/projects/-Users-testuser-projects-myapp/basic_session.jsonl",
    )?;

    let session = parse_claude_session_file(&path)?.expect("expected Claude session");
    assert_eq!(session.agent, Agent::Claude);
    assert_eq!(session.derivation_type, DerivationType::Original);
    assert_eq!(session.session_id, "c0d1e2f3-a4b5-4c6d-8e7f-9a0b1c2d3e4f");
    assert_eq!(session.project, "/Users/testuser/projects/myapp");
    assert_eq!(
        session.cwd.as_deref(),
        Some("/Users/testuser/projects/myapp")
    );
    assert_eq!(session.branch.as_deref(), Some("main"));
    assert_eq!(session.lines, 10);
    assert_eq!(session.first_msg_role, Some(MessageRole::User));
    assert!(session
        .first_msg_content
        .contains("Show me the current git status"));
    assert_eq!(
        session.first_user_msg_content,
        "Show me the current git status and recent commits"
    );
    assert_eq!(session.custom_title.as_deref(), Some("test-basic-session"));
    assert!(session.last_msg_content.contains("Bye!"));
    Ok(())
}

#[test]
fn skips_claude_snapshot_only_files() -> Result<()> {
    let temp = TempDir::new()?;
    let path = copy_fixture(
        &temp,
        "tests/fixtures/sessions/claude/snapshot_only.jsonl",
        ".claude/projects/-Users-testuser-projects-myapp/snapshot_only.jsonl",
    )?;

    assert!(parse_claude_session_file(&path)?.is_none());
    Ok(())
}

#[test]
fn parses_claude_summary_session_without_crashing() -> Result<()> {
    let temp = TempDir::new()?;
    let path = copy_fixture(
        &temp,
        "tests/fixtures/sessions/claude/summary_session.jsonl",
        ".claude/projects/-Users-testuser-projects-myapp/summary_session.jsonl",
    )?;

    let session = parse_claude_session_file(&path)?.expect("expected summary session");
    assert_eq!(session.first_msg_role, Some(MessageRole::Summary));
    assert!(session.content.contains("Invalid API key"));
    assert!(session.content.contains("token rotation"));
    assert_eq!(session.lines, 3);
    Ok(())
}

#[test]
fn parses_claude_rich_content_blocks() -> Result<()> {
    let temp = TempDir::new()?;
    let path = copy_fixture(
        &temp,
        "tests/fixtures/sessions/claude/rich_content.jsonl",
        ".claude/projects/-Users-testuser-projects-webapp/rich_content.jsonl",
    )?;

    let session = parse_claude_session_file(&path)?.expect("expected Claude session");
    assert_eq!(session.project, "/Users/testuser/projects/webapp");
    assert_eq!(
        session.first_user_msg_content,
        "Refactor the authentication module to support token rotation. The current implementation in src/auth.rs uses a single static token."
    );
    assert!(session
        .content
        .contains("Let me analyze the authentication module"));
    assert!(session.content.contains("Tool Bash"));
    assert!(session.content.contains("test_token_refresh"));
    assert_eq!(session.custom_title.as_deref(), Some("test-rich-content"));
    Ok(())
}

#[test]
fn parses_codex_old_format() -> Result<()> {
    let temp = TempDir::new()?;
    let path = copy_fixture(
        &temp,
        "tests/fixtures/sessions/codex/old_format.jsonl",
        ".codex/sessions/2025/08/15/rollout-old.jsonl",
    )?;

    let session = parse_codex_session_file(&path)?.expect("expected Codex session");
    assert_eq!(session.agent, Agent::Codex);
    assert_eq!(session.session_id, "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d");
    assert_eq!(session.project, "/Users/testuser/projects/myapp");
    assert_eq!(
        session.first_user_msg_content,
        "What files are in the current directory?"
    );
    assert!(session.content.contains("Listing directory contents"));
    assert!(session.content.contains("Reading Cargo.toml"));
    assert!(session
        .content
        .contains("Here are the contents of `Cargo.toml`"));
    assert_eq!(session.lines, 15);
    Ok(())
}

#[test]
fn parses_codex_new_format_and_decodes_tool_arguments() -> Result<()> {
    let temp = TempDir::new()?;
    let path = copy_fixture(
        &temp,
        "tests/fixtures/sessions/codex/new_format.jsonl",
        ".codex/sessions/2025/12/10/rollout-new.jsonl",
    )?;

    let session = parse_codex_session_file(&path)?.expect("expected Codex session");
    assert_eq!(session.project, "/Users/testuser/projects/webapp");
    assert_eq!(
        session.first_user_msg_content,
        "Create a simple hello world Express server in index.js"
    );
    assert!(session
        .content
        .contains("npm init -y && npm install express"));
    assert!(session.content.contains("cat > index.js"));
    assert!(session
        .content
        .contains("Created `index.js` with a simple Express hello-world server"));
    Ok(())
}

#[test]
fn parses_codex_latest_format_and_custom_tool_calls() -> Result<()> {
    let temp = TempDir::new()?;
    let path = copy_fixture(
        &temp,
        "tests/fixtures/sessions/codex/latest_format.jsonl",
        ".codex/sessions/2026/03/18/rollout-latest.jsonl",
    )?;

    let session = parse_codex_session_file(&path)?.expect("expected Codex session");
    assert_eq!(session.project, "/Users/testuser/projects/rustapp");
    assert_eq!(session.first_msg_role, Some(MessageRole::User));
    assert_eq!(
        session.first_user_msg_content,
        "Add a health check endpoint to src/main.rs that returns JSON {\"status\": \"ok\"}"
    );
    assert!(session.content.contains("/health"));
    assert!(session.content.contains("cargo check 2>&1"));
    assert!(session.content.contains("\"status\": \"ok\""));
    Ok(())
}

#[test]
fn parses_codex_minimal_session() -> Result<()> {
    let temp = TempDir::new()?;
    let path = copy_fixture(
        &temp,
        "tests/fixtures/sessions/codex/minimal.jsonl",
        ".codex/sessions/2026/01/15/rollout-minimal.jsonl",
    )?;

    let session = parse_codex_session_file(&path)?.expect("expected Codex session");
    assert_eq!(session.first_user_msg_content, "What is 2 + 2?");
    assert_eq!(session.last_msg_content, "4");
    Ok(())
}

#[test]
fn parsers_skip_malformed_lines_without_panicking() -> Result<()> {
    let temp = TempDir::new()?;
    let path = temp
        .path()
        .join(".codex/sessions/2026/04/01/rollout-malformed.jsonl");
    let fixture = fs::read_to_string(fixture_path("tests/fixtures/sessions/codex/minimal.jsonl"))?;
    let malformed = format!(
        "{{\"timestamp\":\"2026-04-01T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"bad-session\",\"cwd\":\"/tmp/demo\"}}}}\nnot-json\n{}\n{{\"timestamp\":\"2026-04-01T00:00:03Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"unknown_event\"}}}}",
        fixture
    );

    write_text_file(&path, &malformed)?;

    let session =
        parse_codex_session_file(&path)?.expect("expected session despite malformed lines");
    assert!(session
        .content
        .contains("Simple arithmetic question. The answer is 4."));
    Ok(())
}

#[test]
fn scanner_discovers_both_session_roots_recursively() -> Result<()> {
    let temp = TempDir::new()?;
    let claude = copy_fixture(
        &temp,
        "tests/fixtures/sessions/claude/basic_session.jsonl",
        ".claude/projects/-Users-testuser-projects-myapp/basic_session.jsonl",
    )?;
    let codex = copy_fixture(
        &temp,
        "tests/fixtures/sessions/codex/minimal.jsonl",
        ".codex/sessions/2026/01/15/rollout-minimal.jsonl",
    )?;
    write_text_file(
        &temp.path().join(".codex/sessions/2026/01/15/ignore.txt"),
        "skip me",
    )?;

    let roots = SessionRoots {
        claude_projects: temp.path().join(".claude/projects"),
        codex_sessions: temp.path().join(".codex/sessions"),
    };
    let files = scan_session_files(&roots)?;

    assert_eq!(files.len(), 2);
    assert!(files
        .iter()
        .any(|file| file.path == claude && file.agent == Agent::Claude));
    assert!(files
        .iter()
        .any(|file| file.path == codex && file.agent == Agent::Codex));
    assert!(files.iter().all(|file| file.size > 0));
    Ok(())
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

fn write_text_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    Ok(())
}

fn fixture_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}
