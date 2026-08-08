use std::fs;
use std::path::{Path, PathBuf};

use aics::index::{IndexManager, IndexPaths, Scope, SearchFilters, SearchRequest, SortMode};
use aics::scan::SessionRoots;
use anyhow::Result;
use tempfile::TempDir;

#[test]
fn empty_query_returns_sessions_sorted_by_modified_time() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("cache")));
    manager.sync_with_roots(&roots, true)?;
    let engine = manager.open_search_engine()?;

    let hits = engine.search(&SearchRequest {
        query: String::new(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters::default(),
    })?;

    assert!(hits.len() >= 2);
    for pair in hits.windows(2) {
        assert!(pair[0].session.modified_ts >= pair[1].session.modified_ts);
    }
    Ok(())
}

#[test]
fn search_query_returns_matching_sessions() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("cache")));
    manager.sync_with_roots(&roots, true)?;
    let engine = manager.open_search_engine()?;

    let hits = engine.search(&SearchRequest {
        query: "Express server".to_owned(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters::default(),
    })?;

    assert!(!hits.is_empty());
    assert!(hits.iter().any(|hit| hit
        .session
        .first_user_msg_content
        .contains("hello world Express server")));
    Ok(())
}

#[test]
fn quoted_phrase_search_highlights_each_normalized_token() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("cache")));
    manager.sync_with_roots(&roots, true)?;
    let engine = manager.open_search_engine()?;

    let hits = engine.search(&SearchRequest {
        query: "\"current git status\"".to_owned(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters::default(),
    })?;

    let hit = hits
        .iter()
        .find(|hit| hit.session.file_path.ends_with("basic_session.jsonl"))
        .expect("basic_session should match the quoted phrase");
    assert!(hit.snippet_html.contains("<b>current</b>"));
    assert!(hit.snippet_html.contains("<b>git</b>"));
    assert!(hit.snippet_html.contains("<b>status</b>"));
    Ok(())
}

#[test]
fn search_excludes_codex_developer_messages() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("cache")));
    manager.sync_with_roots(&roots, true)?;
    let engine = manager.open_search_engine()?;

    let developer_hits = engine.search(&SearchRequest {
        query: "Filesystem sandboxing".to_owned(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters::default(),
    })?;
    assert!(developer_hits.is_empty());

    let user_hits = engine.search(&SearchRequest {
        query: "health check endpoint".to_owned(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters::default(),
    })?;
    assert!(user_hits
        .iter()
        .any(|hit| hit.session.file_path.ends_with("rollout-latest.jsonl")));
    Ok(())
}

#[test]
fn search_excludes_claude_local_command_artifacts() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("cache")));
    manager.sync_with_roots(&roots, true)?;
    let engine = manager.open_search_engine()?;

    for query in ["Caveat", "Bye"] {
        let hits = engine.search(&SearchRequest {
            query: query.to_owned(),
            scope: Scope::Global,
            limit: 10,
            sort: SortMode::Relevance,
            filters: SearchFilters::default(),
        })?;

        assert!(hits.is_empty(), "unexpected search hit for {query}");
    }
    Ok(())
}

#[test]
fn multi_word_queries_default_to_and() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("cache")));
    manager.sync_with_roots(&roots, true)?;
    let engine = manager.open_search_engine()?;

    let hits = engine.search(&SearchRequest {
        query: "Express git".to_owned(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters::default(),
    })?;

    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn explicit_or_operator_broadens_query() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("cache")));
    manager.sync_with_roots(&roots, true)?;
    let engine = manager.open_search_engine()?;

    let hits = engine.search(&SearchRequest {
        query: "Express OR git".to_owned(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters::default(),
    })?;

    assert!(hits.len() >= 2);
    assert!(hits.iter().any(|hit| hit
        .session
        .first_user_msg_content
        .contains("Express server")));
    assert!(hits.iter().any(|hit| hit
        .session
        .first_user_msg_content
        .contains("current git status")));
    assert!(hits
        .iter()
        .all(|hit| !hit.snippet_html.contains("<b>OR</b>")));
    assert!(hits
        .iter()
        .any(|hit| hit.snippet_html.contains("<b>Express</b>")));
    Ok(())
}

#[test]
fn working_dir_field_and_wd_alias_match_component_prefixes() -> Result<()> {
    let temp = TempDir::new()?;
    let target_cwds = [
        "/Users/mclark/p/my/javafx-ax",
        "/Users/mclark/p/my/jave7",
        "/Users/mclark/p/my/jave7-dizzy",
    ];
    for (index, cwd) in target_cwds.iter().enumerate() {
        write_codex_session(&temp, &format!("target-{index}"), cwd, "shared marker")?;
    }
    write_codex_session(
        &temp,
        "unrelated",
        "/Users/mclark/p/gh/java-project",
        "wd my ja marker",
    )?;
    write_codex_session(
        &temp,
        "regex-target",
        "/Users/mclark/p/my/codex/worktrees/8ba3f7e-topic",
        "regex target",
    )?;
    write_codex_session(&temp, "escaped-regex-target", "/tmp/c>zzz", "escape target")?;

    let roots = SessionRoots {
        claude_projects: temp.path().join(".claude/projects"),
        codex_sessions: temp.path().join(".codex/sessions"),
        trash: None,
    };
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("cache")));
    manager.sync_with_roots(&roots, true)?;
    let engine = manager.open_search_engine()?;

    for query in ["wd:my/ja", "working_dir:my/ja", "wd:MY/JA"] {
        for sort in [SortMode::Relevance, SortMode::Time] {
            let hits = engine.search(&SearchRequest {
                query: query.to_owned(),
                scope: Scope::Global,
                limit: 10,
                sort,
                filters: SearchFilters::default(),
            })?;
            let mut actual = hits
                .into_iter()
                .filter_map(|hit| hit.session.cwd)
                .collect::<Vec<_>>();
            actual.sort();

            assert_eq!(
                actual, target_cwds,
                "unexpected matches for {query} / {sort:?}"
            );
        }
    }

    let narrowed = engine.search(&SearchRequest {
        query: "wd:my/jave7".to_owned(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters::default(),
    })?;
    assert_eq!(narrowed.len(), 2);
    assert!(narrowed.iter().all(|hit| hit
        .session
        .cwd
        .as_deref()
        .is_some_and(|cwd| cwd.starts_with("/Users/mclark/p/my/jave7"))));

    let combined = engine.search(&SearchRequest {
        query: "wd:my/ja marker".to_owned(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters::default(),
    })?;
    assert_eq!(combined.len(), 3);
    assert!(combined.iter().all(|hit| hit
        .session
        .cwd
        .as_deref()
        .is_some_and(|cwd| cwd.contains("/p/my/ja"))));

    let grouped_alias = engine.search(&SearchRequest {
        query: "wd:(my/ja OR gh/java)".to_owned(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters::default(),
    })?;
    assert_eq!(grouped_alias.len(), 4);

    for query in [
        "wd:<.*codex/.*8ba3f7e.*>",
        "working_dir:<.*codex/.*8ba3f7e.*>",
    ] {
        let regex_hits = engine.search(&SearchRequest {
            query: query.to_owned(),
            scope: Scope::Global,
            limit: 10,
            sort: SortMode::Relevance,
            filters: SearchFilters::default(),
        })?;
        assert_eq!(regex_hits.len(), 1, "unexpected matches for {query}");
        assert_eq!(
            regex_hits[0].session.cwd.as_deref(),
            Some("/Users/mclark/p/my/codex/worktrees/8ba3f7e-topic")
        );
    }

    let escaped_delimiter = engine.search(&SearchRequest {
        query: r"wd:<c\>.*>".to_owned(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters::default(),
    })?;
    assert_eq!(escaped_delimiter.len(), 1);
    assert_eq!(
        escaped_delimiter[0].session.cwd.as_deref(),
        Some("/tmp/c>zzz")
    );

    let default_content_regex = engine.search(&SearchRequest {
        query: "<mark.*>".to_owned(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters::default(),
    })?;
    assert_eq!(default_content_regex.len(), 4);

    Ok(())
}

#[test]
fn snippet_is_drawn_from_body_when_first_user_msg_does_not_match() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("cache")));
    manager.sync_with_roots(&roots, true)?;
    let engine = manager.open_search_engine()?;

    // `basic_session.jsonl` opens with "Show me the current git status…", but
    // "validation" only appears deeper in the assistant's reply ("Add input
    // validation"). The snippet should highlight the hit wherever it is.
    let hits = engine.search(&SearchRequest {
        query: "validation".to_owned(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters::default(),
    })?;

    let hit = hits
        .iter()
        .find(|hit| hit.session.file_path.ends_with("basic_session.jsonl"))
        .expect("basic_session should match `validation`");
    assert!(
        !hit.session.first_user_msg_content.contains("validation"),
        "fixture precondition: first user message should not contain the query term"
    );
    assert!(
        hit.snippet_html.contains("<b>validation</b>"),
        "expected highlighted snippet drawn from session body, got {:?}",
        hit.snippet_html
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
        "tests/fixtures/sessions/codex/session_index.jsonl",
        ".codex/session_index.jsonl",
    )?;
    copy_fixture(
        temp,
        "tests/fixtures/sessions/codex/minimal.jsonl",
        ".codex/sessions/2026/01/15/rollout-minimal.jsonl",
    )?;
    copy_fixture(
        temp,
        "tests/fixtures/sessions/codex/latest_format.jsonl",
        ".codex/sessions/2026/03/18/rollout-latest.jsonl",
    )?;

    Ok(SessionRoots {
        claude_projects: temp.path().join(".claude/projects"),
        codex_sessions: temp.path().join(".codex/sessions"),
        trash: None,
    })
}

#[test]
fn search_query_matches_codex_thread_name() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("cache")));
    manager.sync_with_roots(&roots, true)?;
    let engine = manager.open_search_engine()?;

    let hits = engine.search(&SearchRequest {
        query: "server rename".to_owned(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Relevance,
        filters: SearchFilters::default(),
    })?;

    assert!(hits.iter().any(|hit| {
        hit.session.custom_title.as_deref() == Some("express server rename")
            && hit
                .session
                .first_user_msg_content
                .contains("hello world Express server")
    }));
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

fn write_codex_session(temp: &TempDir, id: &str, cwd: &str, content: &str) -> Result<PathBuf> {
    let destination = temp
        .path()
        .join(".codex/sessions/2026/08/07")
        .join(format!("rollout-{id}.jsonl"));
    fs::create_dir_all(destination.parent().expect("session path has a parent"))?;
    let lines = [
        serde_json::json!({
            "timestamp": "2026-08-07T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": id,
                "timestamp": "2026-08-07T12:00:00Z",
                "cwd": cwd,
                "originator": "codex_cli_rs",
                "source": "cli"
            }
        }),
        serde_json::json!({
            "timestamp": "2026-08-07T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": content}]
            }
        }),
    ];
    let body = lines
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&destination, format!("{body}\n"))?;
    Ok(destination)
}

fn fixture_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}
