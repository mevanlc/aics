use std::fs;
use std::path::{Path, PathBuf};

use aics::index::{
    IndexManager, IndexPaths, Scope, SearchEngine, SearchFilters, SearchRequest, SortMode,
};
use aics::live::LiveSessionTracker;
use aics::parse::{Agent, DerivationType};
use aics::scan::SessionRoots;
use anyhow::Result;
use tempfile::TempDir;

#[test]
fn search_filters_respect_agent_date_and_min_lines() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let cache_root = temp.path().join("cache");
    let manager = IndexManager::with_paths(IndexPaths::from_root(&cache_root));
    manager.sync_with_roots(&roots, true)?;
    let engine = manager.open_search_engine()?;

    let claude_hits = engine.search(&SearchRequest {
        query: String::new(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters {
            agent: Some(Agent::Claude),
            ..SearchFilters::default()
        },
    })?;
    assert_eq!(claude_hits.len(), 1);
    assert!(claude_hits
        .iter()
        .all(|hit| matches!(hit.session.agent, Agent::Claude)));

    let recent_hits = engine.search(&SearchRequest {
        query: String::new(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters {
            after_ts: Some(1_772_323_200), // 2026-03-01T00:00:00Z
            ..SearchFilters::default()
        },
    })?;
    assert_eq!(recent_hits.len(), 1);
    assert_eq!(recent_hits[0].session.session_id, "c0d1e2f3-a4b5-4c6d-8e7f-9a0b1c2d3e4f");

    let long_hits = engine.search(&SearchRequest {
        query: String::new(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters {
            min_lines: Some(12),
            ..SearchFilters::default()
        },
    })?;
    assert!(!long_hits.is_empty());
    assert!(long_hits.iter().all(|hit| hit.session.lines >= 12));

    Ok(())
}

#[test]
fn sub_agent_sessions_are_hidden_unless_requested() -> Result<()> {
    let temp = TempDir::new()?;
    let original = copy_fixture(
        &temp,
        "tests/fixtures/sessions/claude/basic_session.jsonl",
        ".claude/projects/-Users-testuser-projects-myapp/basic_session.jsonl",
    )?;
    let sub_agent = copy_fixture(
        &temp,
        "tests/fixtures/sessions/claude/basic_session.jsonl",
        ".claude/projects/-Users-testuser-projects-myapp/c0d1e2f3-a4b5-4c6d-8e7f-9a0b1c2d3e4f/subagents/agent-1.jsonl",
    )?;
    let roots = SessionRoots {
        claude_projects: temp.path().join(".claude/projects"),
        codex_sessions: temp.path().join(".codex/sessions"),
    };
    let cache_root = temp.path().join("cache");
    let manager = IndexManager::with_paths(IndexPaths::from_root(&cache_root));
    manager.sync_with_roots(&roots, true)?;
    let engine = manager.open_search_engine()?;

    let default_hits = engine.search(&SearchRequest {
        query: String::new(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters::default(),
    })?;
    assert!(default_hits.iter().any(|hit| hit.session.file_path == original));
    assert!(default_hits.iter().all(|hit| hit.session.file_path != sub_agent));

    let all_hits = engine.search(&SearchRequest {
        query: String::new(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters {
            include_sub_agents: true,
            ..SearchFilters::default()
        },
    })?;
    assert!(all_hits.iter().any(|hit| hit.session.file_path == sub_agent));
    assert!(all_hits.iter().any(|hit| hit.session.derivation_type == DerivationType::SubAgent));

    Ok(())
}

#[test]
fn live_only_filter_uses_live_session_markers() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let live_dir = temp.path().join(".claude/sessions");
    fs::create_dir_all(&live_dir)?;
    fs::write(
        live_dir.join("321.json"),
        r#"{"pid":321,"sessionId":"c0d1e2f3-a4b5-4c6d-8e7f-9a0b1c2d3e4f","cwd":"/Users/testuser/projects/myapp","startedAt":"2026-03-21T00:00:00Z"}"#,
    )?;

    let cache_root = temp.path().join("cache");
    let manager = IndexManager::with_paths(IndexPaths::from_root(&cache_root));
    manager.sync_with_roots(&roots, true)?;
    let engine = SearchEngine::open_with_live_sessions(
        &IndexPaths::from_root(&cache_root),
        LiveSessionTracker::from_claude_sessions_dir(&live_dir),
    )?;

    let hits = engine.search(&SearchRequest {
        query: String::new(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters {
            live_only: true,
            ..SearchFilters::default()
        },
    })?;

    assert_eq!(hits.len(), 1);
    assert!(hits[0].is_live);
    assert_eq!(hits[0].session.session_id, "c0d1e2f3-a4b5-4c6d-8e7f-9a0b1c2d3e4f");
    Ok(())
}

#[test]
fn time_sort_keeps_query_results_in_modified_order() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let cache_root = temp.path().join("cache");
    let manager = IndexManager::with_paths(IndexPaths::from_root(&cache_root));
    manager.sync_with_roots(&roots, true)?;
    let engine = manager.open_search_engine()?;

    let hits = engine.search(&SearchRequest {
        query: "the".to_owned(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Time,
        filters: SearchFilters::default(),
    })?;

    assert!(hits.len() >= 2);
    for pair in hits.windows(2) {
        assert!(pair[0].session.modified_ts >= pair[1].session.modified_ts);
    }
    Ok(())
}

fn fixture_roots(temp: &TempDir) -> Result<SessionRoots> {
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
    copy_fixture(
        temp,
        "tests/fixtures/sessions/codex/minimal.jsonl",
        ".codex/sessions/2026/01/15/rollout-minimal.jsonl",
    )?;

    Ok(SessionRoots {
        claude_projects: temp.path().join(".claude/projects"),
        codex_sessions: temp.path().join(".codex/sessions"),
    })
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
