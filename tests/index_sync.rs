use std::fs;
use std::path::{Path, PathBuf};

use aics::index::{
    IndexManager, IndexPaths, Scope, SearchFilters, SearchRequest, SortMode, SyncOutcome,
    SyncProgress,
};
use aics::parse::Agent;
use aics::scan::SessionRoots;
use anyhow::Result;
use tempfile::TempDir;

#[test]
fn codex_renames_refresh_only_the_affected_session_without_rollout_changes() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("cache")));
    let request = SearchRequest {
        visibility_search: aics::search_query::VisibilitySearch::All,
        query: String::new(),
        scope: Scope::Global,
        limit: 20,
        sort: SortMode::Relevance,
        filters: SearchFilters::default(),
    };
    manager.sync_with_roots(&roots, true)?;
    let engine = manager.open_search_engine()?;
    let codex_session = engine
        .search(&request)?
        .into_iter()
        .find(|hit| hit.session.agent == Agent::Codex)
        .expect("Codex fixture")
        .session;
    let rollout_before = fs::read(&codex_session.file_path)?;
    let names_path = temp.path().join(".codex/session_index.jsonl");

    for name in [Some("Original name"), Some("Renamed session"), None] {
        if let Some(name) = name {
            use std::io::Write;
            let entry = serde_json::json!({
                "id": codex_session.session_id,
                "thread_name": name,
                "updated_at": "2026-10-02T10:00:00Z",
            });
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&names_path)?;
            writeln!(file, "{entry}")?;
        } else {
            fs::remove_file(&names_path)?;
        }
        let stats = manager.sync_with_roots(&roots, false)?;
        assert_eq!(stats.updated, 1);
        assert_eq!(stats.skipped, 1);
        let updated = engine
            .search(&request)?
            .into_iter()
            .find(|hit| hit.session.agent == Agent::Codex)
            .expect("Codex session remains indexed");
        assert_eq!(updated.session.custom_title.as_deref(), name);
        assert_eq!(fs::read(&codex_session.file_path)?, rollout_before);
    }

    fs::write(
        &names_path,
        "{\"id\":\"unrelated\",\"thread_name\":\"Other session\"}\n",
    )?;
    let stats = manager.sync_with_roots(&roots, false)?;
    assert_eq!(stats.updated, 0);
    assert_eq!(stats.skipped, 2);
    Ok(())
}

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
            visibility_search: aics::search_query::VisibilitySearch::All,
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
            visibility_search: aics::search_query::VisibilitySearch::All,
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
fn sync_progress_covers_reading_writing_and_finalization() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let cache_root = temp.path().join("cache");
    let manager = IndexManager::with_paths(IndexPaths::from_root(&cache_root));

    let mut first_events = Vec::new();
    manager.sync_with_roots_and_progress(&roots, true, |event| first_events.push(event))?;
    assert_eq!(first_events.first(), Some(&SyncProgress::OpeningIndex));
    assert!(first_events.iter().any(
        |event| matches!(event, SyncProgress::Discovering { discovered } if *discovered >= 1)
    ));
    assert!(first_events
        .iter()
        .any(|event| matches!(event, SyncProgress::IndexingStarted { total } if *total == 2)));
    let reading_complete = first_events
        .iter()
        .position(|event| {
            *event
                == SyncProgress::IndexingProgress {
                    processed: 2,
                    total: 2,
                }
        })
        .expect("all changed sources are read");
    assert_eq!(
        &first_events[reading_complete + 1..],
        &[
            SyncProgress::ResolvingSupersession,
            SyncProgress::WritingStarted { total: 2 },
            SyncProgress::WritingProgress {
                processed: 1,
                total: 2
            },
            SyncProgress::WritingProgress {
                processed: 2,
                total: 2
            },
            SyncProgress::Committing,
            SyncProgress::SavingState,
        ]
    );

    let mut second_events = Vec::new();
    manager.sync_with_roots_and_progress(&roots, false, |event| second_events.push(event))?;
    assert!(second_events
        .iter()
        .any(|event| matches!(event, SyncProgress::IndexingStarted { total } if *total == 0)));
    assert!(!second_events
        .iter()
        .any(|event| matches!(event, SyncProgress::IndexingProgress { .. })));
    assert!(second_events.contains(&SyncProgress::WritingStarted { total: 0 }));
    assert!(!second_events.contains(&SyncProgress::Committing));
    assert_eq!(second_events.last(), Some(&SyncProgress::SavingState));
    Ok(())
}

#[test]
fn sync_progress_counts_removed_sessions_as_index_writes() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("cache")));
    manager.sync_with_roots(&roots, true)?;
    fs::remove_dir_all(&roots.codex_sessions)?;

    let mut events = Vec::new();
    let stats = manager.sync_with_roots_and_progress(&roots, false, |event| events.push(event))?;
    assert_eq!(stats.removed, 1);
    assert!(events.contains(&SyncProgress::IndexingStarted { total: 0 }));
    assert!(events.contains(&SyncProgress::WritingStarted { total: 1 }));
    assert!(events.contains(&SyncProgress::WritingProgress {
        processed: 1,
        total: 1
    }));
    assert!(events.contains(&SyncProgress::Committing));
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
        live_sessions: Default::default(),
        claude_projects: temp.path().join(".claude/projects"),
        codex_sessions: temp.path().join(".codex/sessions"),
        antigravity_home: temp.path().join(".gemini/antigravity-cli"),
        trash: None,
    };
    let cache_root = temp.path().join("cache");
    let manager = IndexManager::with_paths(IndexPaths::from_root(&cache_root));
    let request = SearchRequest {
        visibility_search: aics::search_query::VisibilitySearch::All,
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
        live_sessions: Default::default(),
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
