use std::fs;
use std::path::{Path, PathBuf};

use aics::index::{
    IndexManager, IndexPaths, Scope, SearchFilters, SearchRequest, SortMode, SyncOutcome,
    SyncProgress,
};
use aics::scan::SessionRoots;
use anyhow::Result;
use tempfile::TempDir;

#[test]
fn rebuild_reindexes_sessions_even_when_fingerprints_match() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let cache_root = temp.path().join("cache");
    let manager = IndexManager::with_paths(IndexPaths::from_root(&cache_root));

    manager.sync_with_roots(&roots, true)?;
    let first_count = manager
        .open_search_engine()?
        .search(&SearchRequest {
            query: String::new(),
            scope: Scope::Global,
            limit: 20,
            sort: SortMode::Relevance,
            filters: SearchFilters::default(),
        })?
        .len();

    manager.sync_with_roots(&roots, true)?;
    let second_count = manager
        .open_search_engine()?
        .search(&SearchRequest {
            query: String::new(),
            scope: Scope::Global,
            limit: 20,
            sort: SortMode::Relevance,
            filters: SearchFilters::default(),
        })?
        .len();

    assert_eq!(first_count, second_count);
    assert!(second_count >= 2);
    Ok(())
}

#[test]
fn sync_best_effort_reports_busy_when_writer_lock_is_held() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let cache_root = temp.path().join("cache");
    let manager = IndexManager::with_paths(IndexPaths::from_root(&cache_root));

    manager.sync_with_roots(&roots, true)?;
    let index = tantivy::Index::open_in_dir(cache_root.join("index"))?;
    let _writer = index.writer::<tantivy::TantivyDocument>(50_000_000)?;

    let outcome = manager.sync_with_roots_best_effort(&roots, false)?;
    assert!(matches!(outcome, SyncOutcome::Busy));
    Ok(())
}

#[test]
fn sync_progress_reports_discovery_then_reindex_count() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let cache_root = temp.path().join("cache");
    let manager = IndexManager::with_paths(IndexPaths::from_root(&cache_root));

    let mut first_events = Vec::new();
    manager.sync_with_roots_and_progress(&roots, true, |event| first_events.push(event))?;
    assert!(first_events.iter().any(
        |event| matches!(event, SyncProgress::Discovering { discovered } if *discovered >= 1)
    ));
    assert!(first_events
        .iter()
        .any(|event| matches!(event, SyncProgress::IndexingStarted { total } if *total == 2)));
    assert!(matches!(
        first_events.last(),
        Some(SyncProgress::IndexingProgress {
            processed: 2,
            total: 2
        })
    ));

    let mut second_events = Vec::new();
    manager.sync_with_roots_and_progress(&roots, false, |event| second_events.push(event))?;
    assert!(second_events
        .iter()
        .any(|event| matches!(event, SyncProgress::IndexingStarted { total } if *total == 0)));
    assert!(!second_events
        .iter()
        .any(|event| matches!(event, SyncProgress::IndexingProgress { .. })));
    Ok(())
}

#[test]
fn delete_index_removes_index_directory_and_state_file() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let cache_root = temp.path().join("cache");
    let manager = IndexManager::with_paths(IndexPaths::from_root(&cache_root));

    manager.sync_with_roots(&roots, true)?;
    assert!(cache_root.join("index").exists());
    assert!(cache_root.join("index_state.json").exists());

    manager.delete_index()?;

    assert!(!cache_root.join("index").exists());
    assert!(!cache_root.join("index_state.json").exists());
    Ok(())
}

#[test]
fn long_lived_search_engine_drops_deleted_sessions_after_sync() -> Result<()> {
    let temp = TempDir::new()?;
    let deleted = copy_fixture(
        &temp,
        "tests/fixtures/sessions/claude/basic_session.jsonl",
        ".claude/projects/-Users-testuser-projects-myapp/basic_session.jsonl",
    )?;
    copy_fixture(
        &temp,
        "tests/fixtures/sessions/codex/new_format.jsonl",
        ".codex/sessions/2025/12/10/rollout-new.jsonl",
    )?;
    let roots = SessionRoots {
        claude_projects: temp.path().join(".claude/projects"),
        codex_sessions: temp.path().join(".codex/sessions"),
        antigravity_home: temp.path().join(".gemini/antigravity-cli"),
        trash: None,
    };
    let cache_root = temp.path().join("cache");
    let manager = IndexManager::with_paths(IndexPaths::from_root(&cache_root));
    let request = SearchRequest {
        query: String::new(),
        scope: Scope::Global,
        limit: 20,
        sort: SortMode::Relevance,
        filters: SearchFilters::default(),
    };

    manager.sync_with_roots(&roots, true)?;
    let engine = manager.open_search_engine()?;
    let before = engine.search(&request)?;
    assert!(before.iter().any(|hit| hit.session.file_path == deleted));

    fs::remove_file(&deleted)?;
    manager.sync_with_roots(&roots, false)?;

    let after = engine.search(&request)?;
    assert!(!after.iter().any(|hit| hit.session.file_path == deleted));
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

    Ok(SessionRoots {
        claude_projects: temp.path().join(".claude/projects"),
        codex_sessions: temp.path().join(".codex/sessions"),
        antigravity_home: temp.path().join(".gemini/antigravity-cli"),
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
