use std::fs;
use std::path::{Path, PathBuf};

use aics::index::{IndexManager, IndexPaths, Scope, SearchFilters, SearchRequest, SortMode};
use aics::parse::{
    parse_antigravity_session_file, parse_scanned_session_file, Agent, ExecStatus, MessageRole,
    SessionCell,
};
use aics::scan::{scan_session_files, SessionRoots};
use anyhow::Result;
use tempfile::TempDir;

const SESSION_ID: &str = "aaaa1111-bbbb-2222-cccc-333344445555";

#[test]
fn scans_and_parses_antigravity_bundle_with_metadata_and_full_merge() -> Result<()> {
    let root = fixture_path("tests/fixtures/sessions/antigravity");
    let roots = roots_for(root.clone());

    let files = scan_session_files(&roots)?;
    assert_eq!(files.len(), 1);
    let file = &files[0];
    assert_eq!(file.agent, Agent::Antigravity);
    assert_eq!(file.companion_paths.len(), 1);
    assert!(file.source_signature != 0);
    assert_eq!(
        file.antigravity_metadata
            .as_ref()
            .and_then(|metadata| metadata.cwd.as_deref()),
        Some("/tmp/agy workspace")
    );

    let session = parse_scanned_session_file(file)?.expect("expected Antigravity session");
    assert_eq!(session.session_id, SESSION_ID);
    assert_eq!(session.agent, Agent::Antigravity);
    assert_eq!(session.cwd.as_deref(), Some("/tmp/agy workspace"));
    assert_eq!(session.project, "/tmp/agy workspace");
    assert_eq!(
        session.custom_title.as_deref(),
        Some("Antigravity fixture title")
    );
    assert_eq!(session.first_msg_role, Some(MessageRole::User));
    assert_eq!(
        session.first_user_msg_content,
        "Find the ORBITAL_ANCHOR marker."
    );
    assert_eq!(
        session
            .session_info
            .as_ref()
            .and_then(|info| info.model.as_deref()),
        Some("Gemini 3.1 Pro (High)")
    );
    assert_eq!(session.lines, 6);
    assert!(session.content.contains("FULL_TRANSCRIPT_DETAIL"));
    assert!(session.content.contains("full-only-command"));
    assert!(session.content.contains("CHECKPOINT_BEACON"));
    assert!(session
        .content
        .contains("Regular tail retained after the full transcript stopped."));
    assert!(!session.content.contains("regular-only-command"));
    assert!(!session.content.contains("internal conversation history"));
    assert!(!session.content.contains("USER_SETTINGS_CHANGE"));

    let exec = session.cells.iter().find_map(|cell| match cell {
        SessionCell::Exec {
            command,
            cwd,
            stdout,
            exit_code,
            status,
            ..
        } => Some((command, cwd, stdout, exit_code, status)),
        _ => None,
    });
    let (command, cwd, stdout, exit_code, status) = exec.expect("expected run_command cell");
    assert_eq!(command, &["printf full-only-command"]);
    assert_eq!(cwd.as_deref(), Some("/tmp/agy workspace"));
    assert_eq!(stdout, "complete command output");
    assert_eq!(*exit_code, Some(0));
    assert_eq!(*status, ExecStatus::Completed);

    let direct = parse_antigravity_session_file(&file.path)?
        .expect("direct parse should retain bundle enrichment");
    assert_eq!(direct.cwd, session.cwd);
    assert_eq!(direct.custom_title, session.custom_title);
    assert_eq!(direct.content, session.content);
    Ok(())
}

#[test]
fn antigravity_index_searches_full_content_and_reindexes_bundle_changes() -> Result<()> {
    let temp = TempDir::new()?;
    let antigravity_home = temp.path().join("antigravity-home");
    copy_tree(
        &fixture_path("tests/fixtures/sessions/antigravity"),
        &antigravity_home,
    )?;
    let roots = roots_for(antigravity_home.clone());
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("index-cache")));

    manager.sync_with_roots(&roots, true)?;
    let engine = manager.open_search_engine()?;
    let hits = search(&engine, "FULL_TRANSCRIPT_DETAIL", Some(Agent::Antigravity))?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session.session_id, SESSION_ID);

    let full_path = antigravity_home
        .join("brain")
        .join(SESSION_ID)
        .join(".system_generated/logs/transcript_full.jsonl");
    let full = fs::read_to_string(&full_path)?;
    fs::write(
        &full_path,
        full.replace("FULL_TRANSCRIPT_DETAIL", "REINDEXED_FULL_DETAIL"),
    )?;
    let stats = manager.sync_with_roots(&roots, false)?;
    assert_eq!(stats.updated, 1);
    assert!(search(&engine, "FULL_TRANSCRIPT_DETAIL", None)?.is_empty());
    assert_eq!(search(&engine, "REINDEXED_FULL_DETAIL", None)?.len(), 1);

    let metadata_path = antigravity_home.join("cache/conversation_metadata.json");
    let metadata = fs::read_to_string(&metadata_path)?;
    fs::write(
        &metadata_path,
        metadata.replace("Antigravity fixture title", "Updated Antigravity title"),
    )?;
    let stats = manager.sync_with_roots(&roots, false)?;
    assert_eq!(stats.updated, 1);
    let hits = search(&engine, "", Some(Agent::Antigravity))?;
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].session.custom_title.as_deref(),
        Some("Updated Antigravity title")
    );
    Ok(())
}

#[test]
fn parses_older_result_records_and_skips_malformed_lines() -> Result<()> {
    let temp = TempDir::new()?;
    let transcript = temp
        .path()
        .join("brain/older-session/.system_generated/logs/transcript.jsonl");
    fs::create_dir_all(transcript.parent().expect("transcript has parent"))?;
    fs::write(
        &transcript,
        concat!(
            "{\"step_index\":0,\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"status\":\"DONE\",\"created_at\":\"2026-06-11T20:14:42Z\",\"content\":\"Search for FLYWHEEL\"}\n",
            "not json\n",
            "{\"step_index\":1,\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"created_at\":\"2026-06-11T20:14:43Z\",\"content\":\"Searching now\"}\n",
            "{\"step_index\":2,\"source\":\"MODEL\",\"type\":\"RUN_COMMAND\",\"status\":\"DONE\",\"created_at\":\"2026-06-11T20:14:44Z\",\"content\":\"FLYWHEEL found\",\"tool_calls\":[{\"name\":\"grep_search\",\"arguments\":{\"query\":\"FLYWHEEL\"}}]}\n"
        ),
    )?;

    let session = parse_antigravity_session_file(&transcript)?.expect("expected older session");
    assert_eq!(session.session_id, "older-session");
    assert_eq!(session.lines, 3);
    assert!(session.content.contains("FLYWHEEL found"));
    assert!(session.cells.iter().any(|cell| matches!(
        cell,
        SessionCell::ToolCall {
            raw_name,
            status: aics::parse::ToolStatus::Completed,
            ..
        } if raw_name == "grep_search"
    )));
    Ok(())
}

fn roots_for(antigravity_home: PathBuf) -> SessionRoots {
    SessionRoots {
        claude_projects: antigravity_home.join("missing-claude"),
        codex_sessions: antigravity_home.join("missing-codex"),
        antigravity_home,
        trash: None,
    }
}

fn search(
    engine: &aics::index::SearchEngine,
    query: &str,
    agent: Option<Agent>,
) -> Result<Vec<aics::index::SearchHit>> {
    engine.search(&SearchRequest {
        query: query.to_owned(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters {
            agent,
            ..SearchFilters::default()
        },
    })
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn fixture_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}
