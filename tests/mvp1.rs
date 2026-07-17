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
fn claude_away_summary_contributes_content_without_transcript_message() -> Result<()> {
    let temp = TempDir::new()?;
    let path = temp
        .path()
        .join(".claude/projects/-Users-testuser-projects-myapp/away-summary.jsonl");
    write_text_file(
        &path,
        concat!(
            "{\"parentUuid\":null,\"isSidechain\":false,\"userType\":\"external\",\"cwd\":\"/Users/testuser/projects/myapp\",\"sessionId\":\"away-summary-session\",\"version\":\"2.1.109\",\"gitBranch\":\"main\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"show git status\"},\"uuid\":\"uuid-1\",\"timestamp\":\"2026-04-15T13:20:00.000Z\"}\n",
            "{\"parentUuid\":\"uuid-1\",\"isSidechain\":false,\"userType\":\"external\",\"cwd\":\"/Users/testuser/projects/myapp\",\"sessionId\":\"away-summary-session\",\"version\":\"2.1.109\",\"gitBranch\":\"main\",\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":\"working on it\"},\"uuid\":\"uuid-2\",\"timestamp\":\"2026-04-15T13:20:01.000Z\"}\n",
            "{\"parentUuid\":\"uuid-2\",\"isSidechain\":false,\"type\":\"system\",\"subtype\":\"away_summary\",\"content\":\"We implemented responsive cell variant-ladder truncation.\",\"timestamp\":\"2026-04-15T13:25:59.006Z\",\"uuid\":\"uuid-3\",\"isMeta\":false,\"userType\":\"external\",\"entrypoint\":\"cli\",\"cwd\":\"/Users/testuser/projects/myapp\",\"sessionId\":\"away-summary-session\",\"version\":\"2.1.109\",\"gitBranch\":\"main\",\"slug\":\"cryptic-popping-pie\"}\n"
        ),
    )?;

    let session = parse_claude_session_file(&path)?.expect("expected Claude session");
    assert_eq!(session.first_msg_role, Some(MessageRole::User));
    assert_eq!(session.custom_title.as_deref(), Some("cryptic-popping-pie"));
    assert!(session
        .content
        .contains("responsive cell variant-ladder truncation"));
    assert!(session
        .messages
        .iter()
        .all(|message| message.role != MessageRole::Summary));
    assert_eq!(session.last_msg_content, "working on it");
    Ok(())
}

#[test]
fn claude_away_summary_only_file_becomes_summary_session() -> Result<()> {
    let temp = TempDir::new()?;
    let path = temp
        .path()
        .join(".claude/projects/-Users-testuser-projects-myapp/away-summary-only.jsonl");
    write_text_file(
        &path,
        "{\"parentUuid\":null,\"isSidechain\":false,\"type\":\"system\",\"subtype\":\"away_summary\",\"content\":\"All 60 tests pass. Next: review the rendering.\",\"timestamp\":\"2026-04-15T13:25:59.006Z\",\"uuid\":\"uuid-1\",\"isMeta\":false,\"userType\":\"external\",\"entrypoint\":\"cli\",\"cwd\":\"/Users/testuser/projects/myapp\",\"sessionId\":\"away-summary-only\",\"version\":\"2.1.109\",\"gitBranch\":\"main\",\"slug\":\"away-only\"}\n",
    )?;

    let session = parse_claude_session_file(&path)?.expect("expected summary-only session");
    assert_eq!(session.first_msg_role, Some(MessageRole::Summary));
    assert!(session
        .first_msg_content
        .contains("All 60 tests pass. Next: review the rendering."));
    assert_eq!(session.custom_title.as_deref(), Some("away-only"));
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
    assert!(session
        .content
        .contains("cargo test --lib auth 2>&1 | head -20"));
    assert!(session.content.contains("test_token_refresh"));
    assert_eq!(session.custom_title.as_deref(), Some("test-rich-content"));
    Ok(())
}

#[test]
fn claude_uses_initial_cwd_for_project_on_termux_paths() -> Result<()> {
    let temp = TempDir::new()?;
    let path = temp
        .path()
        .join(".claude/projects/-data-data-com-termux-files-home-p-my-aics/termux-session.jsonl");
    write_text_file(
        &path,
        concat!(
            "{\"parentUuid\":null,\"isSidechain\":false,\"userType\":\"external\",\"cwd\":\"/data/data/com.termux/files/home/p/my/aics\",\"sessionId\":\"termux-session\",\"version\":\"2.1.63\",\"gitBranch\":\"main\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"where am i\"},\"uuid\":\"uuid-1\",\"timestamp\":\"2026-04-05T12:00:00.000Z\"}\n",
            "{\"parentUuid\":\"uuid-1\",\"isSidechain\":false,\"userType\":\"external\",\"cwd\":\"/data/data/com.termux/files/home/p/my/aics\",\"sessionId\":\"termux-session\",\"version\":\"2.1.63\",\"gitBranch\":\"main\",\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":\"inside the repo\"},\"uuid\":\"uuid-2\",\"timestamp\":\"2026-04-05T12:00:01.000Z\"}\n"
        ),
    )?;

    let session = parse_claude_session_file(&path)?.expect("expected Claude session");

    assert_eq!(
        session.project,
        "/data/data/com.termux/files/home/p/my/aics"
    );
    Ok(())
}

#[test]
fn claude_prefers_last_valid_relocated_cwd() -> Result<()> {
    let temp = TempDir::new()?;
    let path = temp
        .path()
        .join(".claude/projects/-Users-testuser-projects-new/relocated-session.jsonl");
    write_text_file(
        &path,
        concat!(
            "{\"type\":\"user\",\"sessionId\":\"relocated-session\",\"cwd\":\"/Users/testuser/projects/old\",\"message\":{\"role\":\"user\",\"content\":\"move this session\"}}\n",
            "{\"type\":\"relocated\",\"sessionId\":\"relocated-session\",\"relocatedCwd\":\"/Users/testuser/projects/intermediate\"}\n",
            "{\"type\":\"relocated\",\"sessionId\":\"relocated-session\",\"relocatedCwd\":\" /Users/testuser/projects/new \"}\n",
            "{\"type\":\"relocated\",\"sessionId\":\"relocated-session\",\"relocatedCwd\":\"   \"}\n",
            "{\"type\":\"assistant\",\"sessionId\":\"relocated-session\",\"cwd\":\"/Users/testuser/projects/old\",\"message\":{\"role\":\"assistant\",\"content\":\"moved\"}}\n",
        ),
    )?;

    let session = parse_claude_session_file(&path)?.expect("expected Claude session");

    assert_eq!(session.cwd.as_deref(), Some("/Users/testuser/projects/new"));
    assert_eq!(session.project, "/Users/testuser/projects/new");
    Ok(())
}

#[test]
fn claude_falls_back_to_session_id_when_cwd_is_missing() -> Result<()> {
    let temp = TempDir::new()?;
    let path = temp
        .path()
        .join(".claude/projects/-Users-testuser-projects-myapp/missing-cwd.jsonl");
    write_text_file(
        &path,
        concat!(
            "{\"parentUuid\":null,\"isSidechain\":false,\"userType\":\"external\",\"sessionId\":\"missing-cwd-session\",\"version\":\"2.1.63\",\"gitBranch\":\"main\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello\"},\"uuid\":\"uuid-1\",\"timestamp\":\"2026-04-05T12:00:00.000Z\"}\n",
            "{\"parentUuid\":\"uuid-1\",\"isSidechain\":false,\"userType\":\"external\",\"sessionId\":\"missing-cwd-session\",\"version\":\"2.1.63\",\"gitBranch\":\"main\",\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":\"hi\"},\"uuid\":\"uuid-2\",\"timestamp\":\"2026-04-05T12:00:01.000Z\"}\n"
        ),
    )?;

    let session = parse_claude_session_file(&path)?.expect("expected Claude session");

    assert_eq!(session.project, "missing-cwd-session");
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
    copy_fixture(
        &temp,
        "tests/fixtures/sessions/codex/session_index.jsonl",
        ".codex/session_index.jsonl",
    )?;
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
    assert_eq!(
        session.custom_title.as_deref(),
        Some("express server rename")
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
fn codex_prefers_event_preview_and_strips_user_message_prefix() -> Result<()> {
    let temp = TempDir::new()?;
    let path = temp
        .path()
        .join(".codex/sessions/2026/04/08/rollout-prefix.jsonl");
    write_text_file(
        &path,
        concat!(
            "{\"timestamp\":\"2026-04-08T12:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"prefix-session\",\"timestamp\":\"2026-04-08T12:00:00.000Z\",\"cwd\":\"/work/demo\"}}\n",
            "{\"timestamp\":\"2026-04-08T12:00:00.100Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"fallback response item prompt\"}]}}\n",
            "{\"timestamp\":\"2026-04-08T12:00:00.200Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"prefix noise ## My request for Codex: actual prompt\",\"images\":[]}}\n",
            "{\"timestamp\":\"2026-04-08T12:00:00.300Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"done\"}]}}\n"
        ),
    )?;

    let session = parse_codex_session_file(&path)?.expect("expected Codex session");
    assert_eq!(session.first_user_msg_content, "actual prompt");
    assert_eq!(session.first_msg_content, "fallback response item prompt");
    assert!(session.has_resume_preview());
    Ok(())
}

#[test]
fn codex_uses_response_item_fallback_when_event_preview_is_missing() -> Result<()> {
    let temp = TempDir::new()?;
    let path = copy_fixture(
        &temp,
        "tests/fixtures/sessions/codex/minimal.jsonl",
        ".codex/sessions/2026/01/15/rollout-minimal.jsonl",
    )?;

    let session = parse_codex_session_file(&path)?.expect("expected Codex session");
    assert_eq!(session.first_user_msg_content, "What is 2 + 2?");
    assert!(session.has_resume_preview());
    Ok(())
}

#[test]
fn codex_without_real_user_preview_stays_parseable_but_is_not_resume_eligible() -> Result<()> {
    let temp = TempDir::new()?;
    let path = temp
        .path()
        .join(".codex/sessions/2026/04/08/rollout-no-user.jsonl");
    write_text_file(
        &path,
        concat!(
            "{\"timestamp\":\"2026-04-08T12:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"no-user-session\",\"timestamp\":\"2026-04-08T12:00:00.000Z\",\"cwd\":\"/work/demo\"}}\n",
            "{\"timestamp\":\"2026-04-08T12:00:00.100Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"<environment_context>\\n  <cwd>/work/demo</cwd>\\n</environment_context>\"}]}}\n",
            "{\"timestamp\":\"2026-04-08T12:00:00.200Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"still parse me\"}]}}\n"
        ),
    )?;

    let session = parse_codex_session_file(&path)?.expect("expected Codex session");
    assert_eq!(session.first_user_msg_content, "");
    assert!(!session.has_resume_preview());
    assert_eq!(session.last_msg_content, "still parse me");
    Ok(())
}

#[test]
fn codex_response_item_preview_skips_agents_contextual_user_message() -> Result<()> {
    let temp = TempDir::new()?;
    let path = temp
        .path()
        .join(".codex/sessions/2026/04/08/rollout-agents-first.jsonl");
    write_text_file(
        &path,
        concat!(
            "{\"timestamp\":\"2026-04-08T12:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"agents-first-session\",\"timestamp\":\"2026-04-08T12:00:00.000Z\",\"cwd\":\"/work/demo\"}}\n",
            "{\"timestamp\":\"2026-04-08T12:00:00.100Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"# AGENTS.md instructions for /work/demo\\n\\n<INSTRUCTIONS>Memory mentions $commit.\\n</INSTRUCTIONS>\"}]}}\n",
            "{\"timestamp\":\"2026-04-08T12:00:00.200Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"$commit --all\"}]}}\n",
            "{\"timestamp\":\"2026-04-08T12:00:00.300Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"done\"}]}}\n"
        ),
    )?;

    let session = parse_codex_session_file(&path)?.expect("expected Codex session");
    assert_eq!(session.first_user_msg_content, "$commit --all");
    assert!(session.has_resume_preview());
    Ok(())
}

#[test]
fn parses_codex_latest_format_and_custom_tool_calls() -> Result<()> {
    let temp = TempDir::new()?;
    copy_fixture(
        &temp,
        "tests/fixtures/sessions/codex/session_index.jsonl",
        ".codex/session_index.jsonl",
    )?;
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
    assert_eq!(session.custom_title.as_deref(), Some("health check thread"));
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
        trash: None,
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
