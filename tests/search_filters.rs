use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use aics::index::{
    IndexManager, IndexPaths, Scope, SearchEngine, SearchFilters, SearchRequest, SortMode,
    SupersededFilter, TrashFilter,
};
use aics::live::LiveSessionTracker;
use aics::parse::{Agent, DerivationType};
use aics::scan::SessionRoots;
use aics::trash::TrashPaths;
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
    assert_eq!(
        recent_hits[0].session.session_id,
        "c0d1e2f3-a4b5-4c6d-8e7f-9a0b1c2d3e4f"
    );

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
fn search_filters_respect_session_id() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let cache_root = temp.path().join("cache");
    let manager = IndexManager::with_paths(IndexPaths::from_root(&cache_root));
    manager.sync_with_roots(&roots, true)?;
    let engine = manager.open_search_engine()?;

    let hits = engine.search(&SearchRequest {
        query: String::new(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Time,
        filters: SearchFilters {
            session_id: Some("c0d1e2f3-a4b5-4c6d-8e7f-9a0b1c2d3e4f".to_owned()),
            ..SearchFilters::default()
        },
    })?;

    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].session.session_id,
        "c0d1e2f3-a4b5-4c6d-8e7f-9a0b1c2d3e4f"
    );

    let misses = engine.search(&SearchRequest {
        query: String::new(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Time,
        filters: SearchFilters {
            session_id: Some("missing-session".to_owned()),
            ..SearchFilters::default()
        },
    })?;

    assert!(misses.is_empty());
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
        trash: None,
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
    assert!(default_hits
        .iter()
        .any(|hit| hit.session.file_path == original));
    assert!(default_hits
        .iter()
        .all(|hit| hit.session.file_path != sub_agent));

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
    assert!(all_hits
        .iter()
        .any(|hit| hit.session.file_path == sub_agent));
    assert!(all_hits
        .iter()
        .any(|hit| hit.session.derivation_type == DerivationType::SubAgent));

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
    assert_eq!(
        hits[0].session.session_id,
        "c0d1e2f3-a4b5-4c6d-8e7f-9a0b1c2d3e4f"
    );
    Ok(())
}

#[test]
fn trashed_filter_defaults_to_normal_sessions_and_can_include_trash() -> Result<()> {
    let temp = TempDir::new()?;
    let trash_paths = TrashPaths::from_data_root(temp.path().join("data"));
    fs::create_dir_all(&trash_paths.trash_dir)?;
    let trashed = copy_fixture(
        &temp,
        "tests/fixtures/sessions/claude/basic_session.jsonl",
        "data/trash/basic_session.jsonl",
    )?;
    fs::write(
        &trash_paths.metadata_file,
        format!(
            "{}\n",
            serde_json::json!({
                "ts": "2026-05-01T00:00:00Z",
                "nm": "basic_session.jsonl",
                "op": "/tmp/original/basic_session.jsonl",
                "tn": "claude",
            })
        ),
    )?;
    let roots = SessionRoots {
        claude_projects: temp.path().join(".claude/projects"),
        codex_sessions: temp.path().join(".codex/sessions"),
        trash: Some(trash_paths),
    };
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("cache")));
    manager.sync_with_roots(&roots, true)?;
    let engine = manager.open_search_engine()?;

    let default_hits = engine.search(&SearchRequest {
        query: String::new(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Time,
        filters: SearchFilters::default(),
    })?;
    assert!(default_hits.is_empty());

    let trashed_hits = engine.search(&SearchRequest {
        query: String::new(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Time,
        filters: SearchFilters {
            trashed: TrashFilter::Yes,
            ..SearchFilters::default()
        },
    })?;
    assert_eq!(trashed_hits.len(), 1);
    assert_eq!(trashed_hits[0].session.file_path, trashed);
    assert!(trashed_hits[0].session.trashed);
    assert_eq!(
        trashed_hits[0].session.original_path.as_deref(),
        Some(Path::new("/tmp/original/basic_session.jsonl"))
    );

    let both_hits = engine.search(&SearchRequest {
        query: String::new(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Time,
        filters: SearchFilters {
            trashed: TrashFilter::Both,
            ..SearchFilters::default()
        },
    })?;
    assert_eq!(both_hits.len(), 1);
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

#[test]
fn superseded_filter_tracks_direct_codex_forks_across_incremental_sync() -> Result<()> {
    let temp = TempDir::new()?;
    let sessions = temp.path().join(".codex/sessions/2026/08/01");
    fs::create_dir_all(&sessions)?;
    let parent = sessions.join("parent.jsonl");
    let child = sessions.join("child.jsonl");
    fs::write(
        &parent,
        concat!(
            "{\"timestamp\":\"2026-08-01T10:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"parent\",\"cwd\":\"/tmp/demo\"}}\n",
            "{\"timestamp\":\"2026-08-01T10:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"user-1\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hello\"}]}}\n",
            "{\"timestamp\":\"2026-08-01T10:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"assistant-1\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}}\n",
        ),
    )?;
    fs::write(
        &child,
        concat!(
            "{\"timestamp\":\"2026-08-01T10:01:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"child\",\"forked_from_id\":\"parent\",\"cwd\":\"/tmp/demo\"}}\n",
            "{\"timestamp\":\"2026-08-01T10:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"user-1\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hello\"}]}}\n",
            "{\"timestamp\":\"2026-08-01T10:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"assistant-1\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}}\n",
            "{\"timestamp\":\"2026-08-01T10:01:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"user-2\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"continue here\"}]}}\n",
        ),
    )?;
    let roots = SessionRoots {
        claude_projects: temp.path().join(".claude/projects"),
        codex_sessions: temp.path().join(".codex/sessions"),
        trash: None,
    };
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("cache")));
    manager.sync_with_roots(&roots, true)?;

    let search = |superseded| -> Result<Vec<_>> {
        manager.open_search_engine()?.search(&SearchRequest {
            query: String::new(),
            scope: Scope::Global,
            limit: 10,
            sort: SortMode::Time,
            filters: SearchFilters {
                superseded,
                ..SearchFilters::default()
            },
        })
    };

    let all = search(SupersededFilter::Both)?;
    assert_eq!(all.len(), 2);
    let current = search(SupersededFilter::No)?;
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].session.session_id, "child");
    let superseded = search(SupersededFilter::Yes)?;
    assert_eq!(superseded.len(), 1);
    assert_eq!(superseded[0].session.session_id, "parent");
    assert_eq!(
        superseded[0].session.superseded_by.as_deref(),
        Some("child")
    );
    assert_eq!(
        serde_json::to_value(&superseded[0])?
            .pointer("/session/superseded_by")
            .and_then(serde_json::Value::as_str),
        Some("child")
    );

    fs::OpenOptions::new().append(true).open(&parent)?.write_all(
        b"{\"timestamp\":\"2026-08-01T10:02:00Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"parent-only\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"continued in parent\"}]}}\n",
    )?;
    manager.sync_with_roots(&roots, false)?;

    assert_eq!(search(SupersededFilter::No)?.len(), 2);
    assert!(search(SupersededFilter::Yes)?.is_empty());
    Ok(())
}

#[test]
fn superseded_filter_collapses_equivalent_codex_fork_siblings() -> Result<()> {
    let temp = TempDir::new()?;
    let sessions = temp.path().join(".codex/sessions/2026/08/01");
    fs::create_dir_all(&sessions)?;
    fs::write(
        sessions.join("parent.jsonl"),
        concat!(
            "{\"timestamp\":\"2026-08-01T10:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"parent\",\"cwd\":\"/tmp/demo\"}}\n",
            "{\"timestamp\":\"2026-08-01T10:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"user-1\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hello\"}]}}\n",
            "{\"timestamp\":\"2026-08-01T10:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"assistant-1\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}}\n",
        ),
    )?;
    for child_id in ["child-a", "child-b"] {
        fs::write(
            sessions.join(format!("{child_id}.jsonl")),
            format!(
                concat!(
                    "{{\"timestamp\":\"2026-08-01T10:01:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{child_id}\",\"forked_from_id\":\"parent\",\"cwd\":\"/tmp/demo\"}}}}\n",
                    "{{\"timestamp\":\"2026-08-01T10:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"parent\",\"cwd\":\"/tmp/demo\"}}}}\n",
                    "{{\"timestamp\":\"2026-08-01T10:00:01Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"id\":\"user-1\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"hello\"}}]}}}}\n",
                    "{{\"timestamp\":\"2026-08-01T10:00:02Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"id\":\"assistant-1\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"hi\"}}]}}}}\n",
                ),
                child_id = child_id,
            ),
        )?;
    }
    let roots = SessionRoots {
        claude_projects: temp.path().join(".claude/projects"),
        codex_sessions: temp.path().join(".codex/sessions"),
        trash: None,
    };
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("cache")));
    manager.sync_with_roots(&roots, true)?;

    let search = |superseded| -> Result<Vec<_>> {
        manager.open_search_engine()?.search(&SearchRequest {
            query: String::new(),
            scope: Scope::Global,
            limit: 10,
            sort: SortMode::Time,
            filters: SearchFilters {
                superseded,
                ..SearchFilters::default()
            },
        })
    };

    assert_eq!(search(SupersededFilter::Both)?.len(), 3);
    let current = search(SupersededFilter::No)?;
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].session.session_id, "child-b");
    let superseded = search(SupersededFilter::Yes)?;
    assert_eq!(superseded.len(), 2);
    for session_id in ["parent", "child-a"] {
        let hit = superseded
            .iter()
            .find(|hit| hit.session.session_id == session_id)
            .expect("equivalent fork should be collapsed");
        assert_eq!(hit.session.superseded_by.as_deref(), Some("child-b"));
    }
    Ok(())
}

#[test]
fn superseded_filter_collapses_empty_aborted_codex_fork_parent() -> Result<()> {
    let temp = TempDir::new()?;
    let sessions = temp.path().join(".codex/sessions/2026/07/30");
    fs::create_dir_all(&sessions)?;
    fs::write(
        sessions.join("parent.jsonl"),
        concat!(
            "{\"timestamp\":\"2026-07-30T18:44:51Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"parent\",\"cwd\":\"/tmp/demo\"}}\n",
            "{\"timestamp\":\"2026-07-30T18:44:52Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"user-1\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hello\"}]}}\n",
            "{\"timestamp\":\"2026-07-30T18:44:53Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"assistant-1\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}}\n",
            "{\"timestamp\":\"2026-07-30T18:58:41Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"fork-user\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"ok\"}]}}\n",
            "{\"timestamp\":\"2026-07-30T18:58:41Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"fork-abort\",\"role\":\"developer\",\"content\":[{\"type\":\"input_text\",\"text\":\"<turn_aborted>\\nThe previous turn was interrupted on purpose.\\n</turn_aborted>\"}]}}\n",
        ),
    )?;
    fs::write(
        sessions.join("child.jsonl"),
        concat!(
            "{\"timestamp\":\"2026-07-30T18:58:43Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"child\",\"forked_from_id\":\"parent\",\"cwd\":\"/tmp/demo\"}}\n",
            "{\"timestamp\":\"2026-07-30T18:44:51Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"parent\",\"cwd\":\"/tmp/demo\"}}\n",
            "{\"timestamp\":\"2026-07-30T18:44:52Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"user-1\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hello\"}]}}\n",
            "{\"timestamp\":\"2026-07-30T18:44:53Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"assistant-1\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}}\n",
        ),
    )?;
    let roots = SessionRoots {
        claude_projects: temp.path().join(".claude/projects"),
        codex_sessions: temp.path().join(".codex/sessions"),
        trash: None,
    };
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("cache")));
    manager.sync_with_roots(&roots, true)?;

    let search = |superseded| -> Result<Vec<_>> {
        manager.open_search_engine()?.search(&SearchRequest {
            query: String::new(),
            scope: Scope::Global,
            limit: 10,
            sort: SortMode::Time,
            filters: SearchFilters {
                superseded,
                ..SearchFilters::default()
            },
        })
    };

    assert_eq!(search(SupersededFilter::Both)?.len(), 2);
    let current = search(SupersededFilter::No)?;
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].session.session_id, "child");
    let superseded = search(SupersededFilter::Yes)?;
    assert_eq!(superseded.len(), 1);
    assert_eq!(superseded[0].session.session_id, "parent");
    assert_eq!(
        superseded[0].session.superseded_by.as_deref(),
        Some("child")
    );
    Ok(())
}

#[test]
fn superseded_filter_recognizes_trailing_aborted_codex_parent_turn() -> Result<()> {
    let temp = TempDir::new()?;
    let sessions = temp.path().join(".codex/sessions/2026/08/01");
    fs::create_dir_all(&sessions)?;
    fs::write(
        sessions.join("parent.jsonl"),
        concat!(
            "{\"timestamp\":\"2026-08-01T10:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"parent\",\"cwd\":\"/tmp/demo\"}}\n",
            "{\"timestamp\":\"2026-08-01T10:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"user-1\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hello\"}]}}\n",
            "{\"timestamp\":\"2026-08-01T10:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"assistant-1\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}}\n",
            "{\"timestamp\":\"2026-08-01T10:01:00Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"retry-parent\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"continue here\"}]}}\n",
            "{\"timestamp\":\"2026-08-01T10:01:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"abort-parent\",\"role\":\"developer\",\"content\":[{\"type\":\"input_text\",\"text\":\"<turn_aborted>\\nThe previous turn was interrupted on purpose.\\n</turn_aborted>\"}]}}\n",
            "{\"timestamp\":\"2026-08-01T10:01:01Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"turn_aborted\",\"reason\":\"interrupted\"}}\n",
        ),
    )?;
    fs::write(
        sessions.join("child.jsonl"),
        concat!(
            "{\"timestamp\":\"2026-08-01T10:01:05Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"child\",\"forked_from_id\":\"parent\",\"cwd\":\"/tmp/demo\"}}\n",
            "{\"timestamp\":\"2026-08-01T10:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"parent\",\"cwd\":\"/tmp/demo\"}}\n",
            "{\"timestamp\":\"2026-08-01T10:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"user-1\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hello\"}]}}\n",
            "{\"timestamp\":\"2026-08-01T10:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"assistant-1\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}}\n",
            "{\"timestamp\":\"2026-08-01T10:01:06Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"assistant-child\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"continued\"}]}}\n",
        ),
    )?;
    let roots = SessionRoots {
        claude_projects: temp.path().join(".claude/projects"),
        codex_sessions: temp.path().join(".codex/sessions"),
        trash: None,
    };
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("cache")));
    manager.sync_with_roots(&roots, true)?;

    let search = |superseded| -> Result<Vec<_>> {
        manager.open_search_engine()?.search(&SearchRequest {
            query: String::new(),
            scope: Scope::Global,
            limit: 10,
            sort: SortMode::Time,
            filters: SearchFilters {
                superseded,
                ..SearchFilters::default()
            },
        })
    };

    assert_eq!(search(SupersededFilter::Both)?.len(), 2);
    let current = search(SupersededFilter::No)?;
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].session.session_id, "child");
    let superseded = search(SupersededFilter::Yes)?;
    assert_eq!(superseded.len(), 1);
    assert_eq!(superseded[0].session.session_id, "parent");
    assert_eq!(
        superseded[0].session.superseded_by.as_deref(),
        Some("child")
    );
    Ok(())
}

#[test]
fn superseded_filter_recognizes_retried_legacy_codex_aborted_turn() -> Result<()> {
    let temp = TempDir::new()?;
    let sessions = temp.path().join(".codex/sessions/2026/07/16");
    fs::create_dir_all(&sessions)?;
    fs::write(
        sessions.join("parent.jsonl"),
        concat!(
            "{\"timestamp\":\"2026-07-17T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"parent\",\"cwd\":\"/tmp/demo\"}}\n",
            "{\"timestamp\":\"2026-07-17T00:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hello\"}]}}\n",
            "{\"timestamp\":\"2026-07-17T00:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"base-assistant\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}}\n",
            "{\"timestamp\":\"2026-07-17T00:01:00Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"replace the README keybindings\\n\\n| Up | move |\\n| Enter | select |\"}]}}\n",
            "{\"timestamp\":\"2026-07-17T00:01:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"reasoning\",\"id\":\"parent-reasoning\",\"summary\":[]}}\n",
            "{\"timestamp\":\"2026-07-17T00:01:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"parent-ack\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"I will update it\"}]}}\n",
            "{\"timestamp\":\"2026-07-17T00:01:03Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"call_id\":\"parent-tool\",\"name\":\"exec\",\"input\":{}}}\n",
            "{\"timestamp\":\"2026-07-17T00:01:04Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call_output\",\"call_id\":\"parent-tool\",\"output\":\"{}\"}}\n",
            "{\"timestamp\":\"2026-07-17T00:01:05Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"developer\",\"content\":[{\"type\":\"input_text\",\"text\":\"<turn_aborted>\\ninterrupted\\n</turn_aborted>\"}]}}\n",
        ),
    )?;
    fs::write(
        sessions.join("child.jsonl"),
        concat!(
            "{\"timestamp\":\"2026-07-17T00:02:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"child\",\"forked_from_id\":\"parent\",\"cwd\":\"/tmp/demo\"}}\n",
            "{\"timestamp\":\"2026-07-17T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"parent\",\"cwd\":\"/tmp/demo\"}}\n",
            "{\"timestamp\":\"2026-07-17T00:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hello\"}]}}\n",
            "{\"timestamp\":\"2026-07-17T00:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"base-assistant\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}}\n",
            "{\"timestamp\":\"2026-07-17T00:02:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"replace the README keybindings\\n\\n| Enter | select |\\n| Up | move |\"}]}}\n",
            "{\"timestamp\":\"2026-07-17T00:02:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"reasoning\",\"id\":\"child-reasoning\",\"summary\":[]}}\n",
            "{\"timestamp\":\"2026-07-17T00:02:03Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"child-finished\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"README updated\"}]}}\n",
        ),
    )?;
    let roots = SessionRoots {
        claude_projects: temp.path().join(".claude/projects"),
        codex_sessions: temp.path().join(".codex/sessions"),
        trash: None,
    };
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("cache")));
    manager.sync_with_roots(&roots, true)?;

    let search = |superseded| -> Result<Vec<_>> {
        manager.open_search_engine()?.search(&SearchRequest {
            query: String::new(),
            scope: Scope::Global,
            limit: 10,
            sort: SortMode::Time,
            filters: SearchFilters {
                superseded,
                ..SearchFilters::default()
            },
        })
    };

    assert_eq!(search(SupersededFilter::Both)?.len(), 2);
    let current = search(SupersededFilter::No)?;
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].session.session_id, "child");
    let superseded = search(SupersededFilter::Yes)?;
    assert_eq!(superseded.len(), 1);
    assert_eq!(superseded[0].session.session_id, "parent");
    assert_eq!(
        superseded[0].session.superseded_by.as_deref(),
        Some("child")
    );
    Ok(())
}

#[test]
fn superseded_filter_recognizes_claude_fork_lineage() -> Result<()> {
    let temp = TempDir::new()?;
    let sessions = temp.path().join(".claude/projects/-tmp-demo");
    fs::create_dir_all(&sessions)?;
    fs::write(
        sessions.join("parent.jsonl"),
        concat!(
            "{\"type\":\"user\",\"sessionId\":\"parent\",\"uuid\":\"parent-user\",\"cwd\":\"/tmp/demo\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n",
            "{\"type\":\"assistant\",\"sessionId\":\"parent\",\"uuid\":\"parent-reply\",\"cwd\":\"/tmp/demo\",\"message\":{\"role\":\"assistant\",\"content\":\"hi\"}}\n",
        ),
    )?;
    fs::write(
        sessions.join("child.jsonl"),
        concat!(
            "{\"type\":\"user\",\"sessionId\":\"child\",\"uuid\":\"copied-user\",\"cwd\":\"/tmp/demo\",\"forkedFrom\":{\"sessionId\":\"parent\",\"messageUuid\":\"parent-user\"},\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n",
            "{\"type\":\"assistant\",\"sessionId\":\"child\",\"uuid\":\"copied-reply\",\"cwd\":\"/tmp/demo\",\"forkedFrom\":{\"sessionId\":\"parent\",\"messageUuid\":\"parent-reply\"},\"message\":{\"role\":\"assistant\",\"content\":\"hi\"}}\n",
            "{\"type\":\"user\",\"sessionId\":\"child\",\"uuid\":\"child-user\",\"cwd\":\"/tmp/demo\",\"message\":{\"role\":\"user\",\"content\":\"continue here\"}}\n",
        ),
    )?;
    let roots = SessionRoots {
        claude_projects: temp.path().join(".claude/projects"),
        codex_sessions: temp.path().join(".codex/sessions"),
        trash: None,
    };
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("cache")));
    manager.sync_with_roots(&roots, true)?;

    let hits = manager.open_search_engine()?.search(&SearchRequest {
        query: String::new(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Time,
        filters: SearchFilters {
            superseded: SupersededFilter::Yes,
            ..SearchFilters::default()
        },
    })?;

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session.session_id, "parent");
    assert_eq!(hits[0].session.superseded_by.as_deref(), Some("child"));
    Ok(())
}

#[test]
fn superseded_filter_collapses_equivalent_claude_fork() -> Result<()> {
    let temp = TempDir::new()?;
    let sessions = temp.path().join(".claude/projects/-tmp-demo");
    fs::create_dir_all(&sessions)?;
    fs::write(
        sessions.join("parent.jsonl"),
        concat!(
            "{\"type\":\"user\",\"sessionId\":\"parent\",\"uuid\":\"parent-user\",\"cwd\":\"/tmp/demo\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n",
            "{\"type\":\"assistant\",\"sessionId\":\"parent\",\"uuid\":\"parent-reply\",\"cwd\":\"/tmp/demo\",\"message\":{\"role\":\"assistant\",\"content\":\"hi\"}}\n",
        ),
    )?;
    fs::write(
        sessions.join("child.jsonl"),
        concat!(
            "{\"type\":\"user\",\"sessionId\":\"child\",\"uuid\":\"copied-user\",\"cwd\":\"/tmp/demo\",\"forkedFrom\":{\"sessionId\":\"parent\",\"messageUuid\":\"parent-user\"},\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n",
            "{\"type\":\"assistant\",\"sessionId\":\"child\",\"uuid\":\"copied-reply\",\"cwd\":\"/tmp/demo\",\"forkedFrom\":{\"sessionId\":\"parent\",\"messageUuid\":\"parent-reply\"},\"message\":{\"role\":\"assistant\",\"content\":\"hi\"}}\n",
        ),
    )?;
    let roots = SessionRoots {
        claude_projects: temp.path().join(".claude/projects"),
        codex_sessions: temp.path().join(".codex/sessions"),
        trash: None,
    };
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("cache")));
    manager.sync_with_roots(&roots, true)?;

    let search = |superseded| -> Result<Vec<_>> {
        manager.open_search_engine()?.search(&SearchRequest {
            query: String::new(),
            scope: Scope::Global,
            limit: 10,
            sort: SortMode::Time,
            filters: SearchFilters {
                superseded,
                ..SearchFilters::default()
            },
        })
    };

    assert_eq!(search(SupersededFilter::Both)?.len(), 2);
    let current = search(SupersededFilter::No)?;
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].session.session_id, "child");
    let superseded = search(SupersededFilter::Yes)?;
    assert_eq!(superseded.len(), 1);
    assert_eq!(superseded[0].session.session_id, "parent");
    assert_eq!(
        superseded[0].session.superseded_by.as_deref(),
        Some("child")
    );
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
        trash: None,
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
