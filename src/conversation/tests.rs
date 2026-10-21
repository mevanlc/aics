use super::*;
use crate::index::{
    IndexManager, IndexPaths, Scope, SearchFilters, SearchRequest, SortMode, SupersededFilter,
};
use crate::parse::{parse_scanned_session_file, parse_session_file};
use crate::rules::{run_rules, RuleSelection, RulesMode, RulesOptions};
use crate::scan::scan_session_files;
use crate::search_query::VisibilitySearch;
use serde_json::json;
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
    roots: SessionRoots,
}

struct Written {
    path: PathBuf,
    offsets: Vec<u64>,
}

impl Fixture {
    fn new() -> Result<Self> {
        crate::settings::isolate_config_root_for_tests();
        let temp = TempDir::new()?;
        let roots = SessionRoots {
            live_sessions: Default::default(),
            claude_projects: temp.path().join(".claude/projects"),
            codex_sessions: temp.path().join(".codex/sessions"),
            antigravity_home: temp.path().join(".gemini/antigravity-cli"),
            trash: None,
        };
        fs::create_dir_all(&roots.codex_sessions)?;
        Ok(Self { temp, roots })
    }

    fn write(
        &self,
        thread: &str,
        rollout: &str,
        parent: Option<&str>,
        base: Option<(&str, u64, u64)>,
        items: &[Value],
    ) -> Result<Written> {
        let ordinal = base.map_or(0, |(_, ordinal, _)| ordinal);
        let path = self.roots.codex_sessions.join(if thread == rollout {
            format!("{thread}.jsonl")
        } else {
            format!("{thread}_{rollout}.jsonl")
        });
        let mut payload =
            json!({"id": thread, "cwd":"/tmp/conversation", "history_mode":"paginated"});
        if let Some(parent) = parent {
            payload["forked_from_id"] = json!(parent);
        }
        if let Some((id, end, offset)) = base {
            payload["history_base"] =
                json!({"thread_id":id,"end_ordinal_exclusive":end,"end_byte_offset":offset});
        }
        let mut file = fs::File::create(&path)?;
        let mut offsets = Vec::new();
        let mut offset = 0;
        for (i, item) in std::iter::once(json!({"type":"session_meta","payload":payload}))
            .chain(items.iter().cloned())
            .enumerate()
        {
            let mut item = item;
            item["ordinal"] = json!(ordinal + i as u64);
            item["timestamp"] = json!("2026-10-20T10:00:00Z");
            let line = serde_json::to_string(&item)? + "\n";
            use std::io::Write;
            file.write_all(line.as_bytes())?;
            offset += line.len() as u64;
            offsets.push(offset);
        }
        Ok(Written { path, offsets })
    }

    fn select(&self, thread: &str, path: &Path) -> Result<()> {
        let connection = Connection::open(
            self.roots
                .codex_sessions
                .parent()
                .unwrap()
                .join("state_5.sqlite"),
        )?;
        connection.execute_batch("CREATE TABLE IF NOT EXISTS threads (id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL, history_mode TEXT NOT NULL)")?;
        connection.execute(
            "INSERT OR REPLACE INTO threads VALUES (?1, ?2, 'paginated')",
            rusqlite::params![thread, path.to_string_lossy()],
        )?;
        Ok(())
    }

    fn logical(&self) -> Result<Vec<SessionFile>> {
        Ok(logical_files(
            &self.roots,
            scan_session_files(&self.roots)?,
            &Default::default(),
        )?
        .0)
    }

    fn manager(&self) -> IndexManager {
        IndexManager::with_paths(IndexPaths::from_root(self.temp.path().join("cache")))
    }

    fn rules(&self, source: &str, mode: RulesMode) -> Result<RulesOptions> {
        let path = self.temp.path().join("rules.js");
        fs::write(&path, source)?;
        Ok(RulesOptions {
            rules_path: path,
            cache_path: Some(self.temp.path().join("rules-cache.json")),
            mode,
            selection: RuleSelection::All,
            json: false,
            scope: Scope::Global,
            filters: SearchFilters {
                superseded: SupersededFilter::Both,
                ..Default::default()
            },
            supersession: Default::default(),
        })
    }
}

fn message(id: &str, role: &str, text: &str) -> Value {
    json!({"type":"response_item","payload":{"type":"message","id":id,"role":role,"content":[{"type":"input_text","text":text}]}})
}

#[test]
fn codex_branch_follows_resume_metadata_and_refreshes_without_rollout_changes() -> Result<()> {
    let fixture = Fixture::new()?;
    let written = fixture.write(
        "thread",
        "thread",
        None,
        None,
        &[message("u", "user", "branch test")],
    )?;
    let contents = fs::read_to_string(&written.path)?;
    let mut records = contents
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<serde_json::Result<Vec<_>>>()?;
    records[0]["payload"]["git"] = json!({"branch":"recorded-branch"});
    fs::write(
        &written.path,
        records
            .iter()
            .map(|record| format!("{record}\n"))
            .collect::<String>(),
    )?;
    assert_eq!(
        parse_session_file(Agent::Codex, &written.path)?
            .unwrap()
            .branch
            .as_deref(),
        Some("recorded-branch")
    );
    assert_eq!(
        parse_scanned_session_file(&fixture.logical()?[0])?
            .unwrap()
            .branch
            .as_deref(),
        Some("recorded-branch")
    );

    // Older/missing branch columns leave rollout metadata usable.
    fixture.select("thread", &written.path)?;
    let source = fixture.logical()?[0].conversation.clone().unwrap();
    assert_eq!(
        source.parse()?.unwrap().branch.as_deref(),
        Some("recorded-branch")
    );
    let connection = Connection::open(
        fixture
            .roots
            .codex_sessions
            .parent()
            .unwrap()
            .join("state_5.sqlite"),
    )?;
    connection.execute_batch(
        "ALTER TABLE threads ADD COLUMN git_branch TEXT;
        UPDATE threads SET git_branch = 'picker-branch'",
    )?;
    assert!(!source.is_current());
    let manager = fixture.manager();
    manager.sync_with_roots(&fixture.roots, true)?;
    let hits = manager
        .open_search_engine()?
        .search(&request("", SupersededFilter::Both))?;
    assert_eq!(hits[0].session.branch.as_deref(), Some("picker-branch"));
    let source = fixture.logical()?[0].conversation.clone().unwrap();
    assert!(source.is_current());
    // Snapshot round trips preserve SQLite's explicit NULL clears too.
    let encoded = serde_json::to_vec(&*source)?;
    let restored: ConversationSource = serde_json::from_slice(&encoded)?;
    assert!(restored.is_current());
    let unchanged_rollout = fs::read(&written.path)?;
    connection.execute_batch("UPDATE threads SET git_branch = 'updated-branch'")?;
    assert!(!source.is_current());
    let stats = manager.sync_with_roots(&fixture.roots, false)?;
    assert_eq!(stats.updated, 1);
    let hits = manager
        .open_search_engine()?
        .search(&request("", SupersededFilter::Both))?;
    assert_eq!(hits[0].session.branch.as_deref(), Some("updated-branch"));
    let mut branch_request = request("", SupersededFilter::Both);
    branch_request.filters.branch = Some("updated-branch".into());
    assert_eq!(
        manager.open_search_engine()?.search(&branch_request)?.len(),
        1
    );
    connection.execute_batch("UPDATE threads SET git_branch = NULL")?;
    manager.sync_with_roots(&fixture.roots, false)?;
    assert_eq!(
        manager
            .open_search_engine()?
            .search(&request("", SupersededFilter::Both))?[0]
            .session
            .branch,
        None
    );
    let source = fixture.logical()?[0].conversation.clone().unwrap();
    let restored: ConversationSource = serde_json::from_slice(&serde_json::to_vec(&*source)?)?;
    assert!(restored.is_current());
    assert_eq!(restored.parse()?.unwrap().branch, None);
    // Legacy reconciliation fills missing Git fields from the rollout instead.
    connection.execute_batch("UPDATE threads SET history_mode = 'legacy'")?;
    assert_eq!(
        parse_scanned_session_file(&fixture.logical()?[0])?
            .unwrap()
            .branch
            .as_deref(),
        Some("recorded-branch")
    );
    assert_eq!(fs::read(&written.path)?, unchanged_rollout);
    Ok(())
}

fn request(query: &str, superseded: SupersededFilter) -> SearchRequest {
    SearchRequest {
        query: query.into(),
        scope: Scope::Global,
        limit: 100,
        sort: SortMode::Time,
        visibility_search: VisibilitySearch::All,
        filters: SearchFilters {
            superseded,
            ..Default::default()
        },
    }
}

#[test]
fn chain_reconstruction_excludes_aborted_suffix_and_preserves_physical_review() -> Result<()> {
    let fixture = Fixture::new()?;
    let original = fixture.write(
        "thread",
        "thread",
        None,
        None,
        &[
            message("u1", "user", "inherited needle"),
            message("a1", "assistant", "initial answer"),
            message("discarded", "user", "excluded aborted prompt"),
        ],
    )?;
    let second = fixture.write(
        "thread",
        "second",
        None,
        Some(("thread", 3, original.offsets[2])),
        &[
            message("u2", "user", "replacement prompt"),
            message("a2", "assistant", "second answer"),
        ],
    )?;
    let third = fixture.write(
        "thread",
        "third",
        None,
        Some(("second", 6, second.offsets[2])),
        &[message("u3", "user", "latest prompt")],
    )?;
    fixture.select("thread", &third.path)?;
    let files = fixture.logical()?;
    assert_eq!(files.len(), 1);
    let session = parse_scanned_session_file(&files[0])?.unwrap();
    assert_eq!(session.session_id, "thread");
    assert_eq!(session.file_path, third.path);
    assert_eq!(session.first_user_msg_content, "inherited needle");
    assert!(session.content.contains("second answer"));
    assert!(!session.content.contains("excluded aborted prompt"));
    assert!(session.history.as_ref().unwrap().is_complete());
    assert_eq!(session.history.as_ref().unwrap().source_paths.len(), 3);
    let physical = parse_session_file(Agent::Codex, &third.path)?.unwrap();
    assert!(!physical.content.contains("inherited needle"));

    let manager = fixture.manager();
    manager.sync_with_roots(&fixture.roots, true)?;
    let engine = manager.open_search_engine()?;
    for filter in [SupersededFilter::No, SupersededFilter::Both] {
        let hits = engine.search(&request("", filter))?;
        assert_eq!(hits.len(), 1);
        let parsed = hits[0].session.parse()?.unwrap();
        let export = crate::export::session_to_markdown(&parsed);
        assert!(export.contains("inherited needle"));
        assert!(!export.contains("excluded aborted prompt"));
        let json = serde_json::to_value(&hits[0])?;
        assert_eq!(json["session"]["history"]["status"], "complete");
        assert!(json["session"].get("source").is_none());
    }
    assert_eq!(
        engine
            .search(&request("inherited", SupersededFilter::No))?
            .len(),
        1
    );
    assert!(engine
        .search(&request("excluded", SupersededFilter::Both))?
        .is_empty());
    assert!(engine
        .search(&request("", SupersededFilter::Yes))?
        .is_empty());
    Ok(())
}

#[test]
fn database_selection_wins_over_newer_pages_and_changes_invalidate_sources() -> Result<()> {
    let fixture = Fixture::new()?;
    let original = fixture.write(
        "thread",
        "thread",
        None,
        None,
        &[message("u1", "user", "retained")],
    )?;
    let later = fixture.write(
        "thread",
        "later",
        None,
        Some(("thread", 2, original.offsets[1])),
        &[message("u2", "user", "reverted content")],
    )?;
    fixture.select("thread", &original.path)?;
    let files = fixture.logical()?;
    assert_eq!(files.len(), 1);
    let source = files[0].conversation.as_ref().unwrap();
    assert!(source.is_current());
    assert!(!source
        .parse()?
        .unwrap()
        .content
        .contains("reverted content"));
    fixture.select("other", &later.path)?;
    assert!(
        source.is_current(),
        "unrelated database changes do not invalidate a source"
    );
    fixture.select("thread", &later.path)?;
    assert!(!source.is_current());
    assert!(!source.parse()?.unwrap().history.unwrap().is_complete());
    let manager = fixture.manager();
    manager.sync_with_roots(&fixture.roots, true)?;
    assert_eq!(
        manager
            .open_search_engine()?
            .search(&request("reverted", SupersededFilter::Both))?
            .len(),
        1
    );
    fixture.select("thread", &original.path)?;
    manager.sync_with_roots(&fixture.roots, false)?;
    assert!(manager
        .open_search_engine()?
        .search(&request("reverted", SupersededFilter::Both))?
        .is_empty());
    Ok(())
}

#[test]
fn ambiguous_versions_remain_visible_and_rules_skip_them() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.write(
        "thread",
        "a",
        None,
        None,
        &[message("a", "user", "first branch")],
    )?;
    fixture.write(
        "thread",
        "b",
        None,
        None,
        &[message("b", "user", "second branch")],
    )?;
    let files = fixture.logical()?;
    assert_eq!(files.len(), 2);
    assert!(
        files
            .iter()
            .all(|file| file.conversation.as_ref().unwrap().history.status
                == HistoryStatus::Ambiguous)
    );
    let report = run_rules(
        &fixture.roots,
        &fixture.rules("rule('all', () => trash());", RulesMode::Preview)?,
    )?;
    assert_eq!(report.processing_skips.len(), 2);
    assert!(report.proposals.is_empty());
    Ok(())
}

#[test]
fn missing_authoritative_selection_never_falls_back_to_an_older_version() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.write(
        "thread",
        "thread",
        None,
        None,
        &[message("u", "user", "older content")],
    )?;
    fixture.select(
        "thread",
        &fixture.roots.codex_sessions.join("missing.jsonl"),
    )?;
    let files = fixture.logical()?;
    assert_eq!(files.len(), 1);
    assert_eq!(
        parse_scanned_session_file(&files[0])?
            .unwrap()
            .history
            .unwrap()
            .status,
        HistoryStatus::Ambiguous
    );
    Ok(())
}

#[test]
fn missing_and_invalid_history_are_searchable_but_never_cleanup_candidates() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.write(
        "missing",
        "missing",
        None,
        Some(("gone", 4, 100)),
        &[message("u", "user", "available suffix")],
    )?;
    fixture.write(
        "invalid",
        "invalid",
        None,
        Some(("missing", 0, 1)),
        &[message("u2", "user", "another suffix")],
    )?;
    let manager = fixture.manager();
    manager.sync_with_roots(&fixture.roots, true)?;
    let hits = manager
        .open_search_engine()?
        .search(&request("", SupersededFilter::Both))?;
    assert_eq!(hits.len(), 2);
    assert!(hits
        .iter()
        .all(|hit| !hit.session.history.as_ref().unwrap().is_complete()));
    let report = run_rules(
        &fixture.roots,
        &fixture.rules("rule('all', () => trash());", RulesMode::Preview)?,
    )?;
    assert!(report.proposals.is_empty());
    assert_eq!(report.processing_skips.len(), 2);
    Ok(())
}

#[test]
fn compressed_archived_ancestors_are_read_without_materializing_files() -> Result<()> {
    let fixture = Fixture::new()?;
    let root = fixture.write(
        "root",
        "root",
        None,
        None,
        &[message("u", "user", "archived ancestor")],
    )?;
    fixture.write(
        "child",
        "child",
        Some("root"),
        Some(("root", 2, root.offsets[1])),
        &[message("a", "assistant", "child answer")],
    )?;
    let archive = fixture
        .roots
        .codex_sessions
        .parent()
        .unwrap()
        .join("archived_sessions");
    fs::create_dir_all(&archive)?;
    let compressed = archive.join("root.jsonl.zst");
    let mut input = fs::File::open(&root.path)?;
    let mut output = fs::File::create(&compressed)?;
    zstd::stream::copy_encode(&mut input, &mut output, 1)?;
    fs::remove_file(root.path)?;
    let files = fixture.logical()?;
    assert_eq!(files.len(), 1);
    let session = parse_scanned_session_file(&files[0])?.unwrap();
    assert!(session.content.contains("archived ancestor"));
    assert!(session.history.unwrap().is_complete());
    assert!(!archive.join("root.jsonl").exists());
    Ok(())
}

#[test]
fn complete_forks_collapse_but_divergent_branches_and_dependencies_remain() -> Result<()> {
    let fixture = Fixture::new()?;
    let parent = fixture.write(
        "parent",
        "parent",
        None,
        None,
        &[message("u", "user", "common history")],
    )?;
    fixture.write(
        "child",
        "child",
        Some("parent"),
        Some(("parent", 2, parent.offsets[1])),
        &[message("a", "assistant", "new work")],
    )?;
    let manager = fixture.manager();
    manager.sync_with_roots(&fixture.roots, true)?;
    let hits = manager
        .open_search_engine()?
        .search(&request("", SupersededFilter::No))?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session.session_id, "child");
    assert_eq!(
        manager
            .supersession_map()?
            .get(&parent.path)
            .map(String::as_str),
        Some("child")
    );
    let graph = crate::lifecycle::DependencyGraph::discover(
        &fixture.roots,
        Agent::Codex,
        std::slice::from_ref(&parent.path),
    )?;
    let plan = graph.plan(
        std::collections::BTreeSet::from([parent.path.clone()]),
        crate::lifecycle::Operation::Trash,
    );
    assert_eq!(plan.session_count(), 2);
    use std::io::Write;
    let mut file = fs::OpenOptions::new().append(true).open(&parent.path)?;
    writeln!(
        file,
        "{}",
        json!({"ordinal":2,"type":"response_item","payload":{"type":"message","id":"divergent","role":"user","content":[{"type":"input_text","text":"parent only"}]}})
    )?;
    manager.sync_with_roots(&fixture.roots, false)?;
    assert_eq!(
        manager
            .open_search_engine()?
            .search(&request("", SupersededFilter::No))?
            .len(),
        2
    );
    Ok(())
}

#[test]
fn rules_evaluate_once_with_inherited_turns_and_invalidate_ancestor_changes() -> Result<()> {
    let fixture = Fixture::new()?;
    let root = fixture.write(
        "thread",
        "thread",
        None,
        None,
        &[message("u", "user", "inherited request")],
    )?;
    let tip = fixture.write(
        "thread",
        "tip",
        None,
        Some(("thread", 2, root.offsets[1])),
        &[message("a", "assistant", "answer")],
    )?;
    let options = fixture.rules("rule('full history', ({ turns }) => turns.user.some(t => t.text().includes('inherited request')) ? trash('matched inherited turn') : nothing());", RulesMode::Preview)?;
    for _ in 0..2 {
        let report = run_rules(&fixture.roots, &options)?;
        assert_eq!(report.proposals.len(), 1);
        assert_eq!(report.proposals[0].path, tip.path);
        assert!(
            report.proposals[0]
                .reviewed_group
                .as_ref()
                .unwrap()
                .blockers
                .is_empty(),
            "same-thread pages belong to the logical selection"
        );
        assert!(report.preview_matches[0]
            .hit
            .session
            .parse()?
            .unwrap()
            .content
            .contains("inherited request"));
    }
    let mut changed = fs::OpenOptions::new().write(true).open(&root.path)?;
    use std::io::{Seek, Write};
    changed.seek(std::io::SeekFrom::Start(root.offsets[0]))?;
    changed.write_all(b"invalid ancestor record\n")?;
    changed.set_len(root.offsets[0] + 24)?;
    let report = run_rules(&fixture.roots, &options)?;
    assert!(report.proposals.is_empty());
    assert_eq!(report.processing_skips.len(), 1);
    Ok(())
}

#[test]
fn rule_proposals_are_rejected_after_a_database_revert() -> Result<()> {
    let fixture = Fixture::new()?;
    let root = fixture.write(
        "thread",
        "thread",
        None,
        None,
        &[message("u", "user", "request")],
    )?;
    let tip = fixture.write(
        "thread",
        "tip",
        None,
        Some(("thread", 2, root.offsets[1])),
        &[message("a", "assistant", "answer")],
    )?;
    fixture.select("thread", &tip.path)?;
    let report = run_rules(
        &fixture.roots,
        &fixture.rules("rule('all', () => trash());", RulesMode::Preview)?,
    )?;
    assert_eq!(report.proposals.len(), 1);
    fixture.select("thread", &root.path)?;
    let (applied, skipped) = crate::rules::apply_rule_proposals(&fixture.roots, &report.proposals);
    assert!(applied.is_empty());
    assert_eq!(skipped.len(), 1);
    assert!(skipped[0].skip_reason.contains("history changed"));
    assert!(root.path.exists() && tip.path.exists());
    Ok(())
}

#[test]
fn logical_cleanup_and_restore_include_owned_pages_and_keep_full_history() -> Result<()> {
    let mut fixture = Fixture::new()?;
    fixture.roots.trash = Some(crate::trash::TrashPaths::from_data_root(
        fixture.temp.path().join("data"),
    ));
    let root = fixture.write(
        "thread",
        "thread",
        None,
        None,
        &[message("u", "user", "inherited request")],
    )?;
    let tip = fixture.write(
        "thread",
        "tip",
        None,
        Some(("thread", 2, root.offsets[1])),
        &[message("a", "assistant", "answer")],
    )?;
    fixture.select("thread", &tip.path)?;
    let options = fixture.rules("rule('all', () => trash());", RulesMode::Apply)?;
    let report = run_rules(&fixture.roots, &options)?;
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_eq!(report.applied.len(), 2);
    assert!(!root.path.exists() && !tip.path.exists());
    let files = fixture.logical()?;
    assert_eq!(files.len(), 1);
    assert!(files[0].trashed);
    let session = parse_scanned_session_file(&files[0])?.unwrap();
    assert!(session.history.unwrap().is_complete());
    assert!(session.content.contains("inherited request"));
    let mut options = fixture.rules("rule('restore', () => untrash());", RulesMode::Apply)?;
    options.filters.trashed = crate::index::TrashFilter::Yes;
    let report = run_rules(&fixture.roots, &options)?;
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_eq!(report.applied.len(), 2);
    assert!(root.path.exists() && tip.path.exists());
    Ok(())
}

#[test]
fn active_conversation_reports_trashed_ancestor_and_allows_inspection() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let trash = crate::trash::TrashPaths::from_data_root(fixture.temp.path().join("data"));
    fixture.roots.trash = Some(trash.clone());
    let root = fixture.write(
        "parent",
        "parent",
        None,
        None,
        &[message("u", "user", "recoverable history")],
    )?;
    fixture.write(
        "child",
        "child",
        Some("parent"),
        Some(("parent", 2, root.offsets[1])),
        &[message("a", "assistant", "surviving answer")],
    )?;
    // Simulate a partial removal left by an older version, bypassing lifecycle planning.
    crate::trash::TrashStore::new(trash).trash_file(
        &root.path,
        Agent::Codex,
        &crate::trash::TrashReason::KeyBinding {
            key: "fixture".into(),
        },
    )?;
    let files = fixture.logical()?;
    let child = files.iter().find(|file| !file.trashed).unwrap();
    let session = parse_scanned_session_file(child)?.unwrap();
    assert!(
        session.content.contains("recoverable history"),
        "{:?}",
        session.history
    );
    assert!(session
        .history
        .unwrap()
        .notice()
        .unwrap()
        .contains("in trash"));
    let report = run_rules(
        &fixture.roots,
        &fixture.rules("rule('all', () => trash());", RulesMode::Preview)?,
    )?;
    assert!(report.proposals.is_empty());
    assert_eq!(report.processing_skips.len(), 1);
    Ok(())
}

#[test]
fn migration_removes_obsolete_physical_documents() -> Result<()> {
    let fixture = Fixture::new()?;
    let root = fixture.write(
        "thread",
        "thread",
        None,
        None,
        &[message("u", "user", "request")],
    )?;
    let manager = fixture.manager();
    manager.sync_with_roots(&fixture.roots, true)?;
    let state_path = fixture.temp.path().join("cache/index_state.json");
    let mut state: Value = serde_json::from_reader(fs::File::open(&state_path)?)?;
    state["format_version"] = json!(23);
    serde_json::to_writer(fs::File::create(&state_path)?, &state)?;
    let tip = fixture.write(
        "thread",
        "tip",
        None,
        Some(("thread", 2, root.offsets[1])),
        &[message("a", "assistant", "answer")],
    )?;
    fixture.select("thread", &tip.path)?;
    manager.sync_with_roots(&fixture.roots, false)?;
    let hits = manager
        .open_search_engine()?
        .search(&request("", SupersededFilter::Both))?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session.file_path, tip.path);
    Ok(())
}

#[test]
fn cross_page_tool_pairs_and_summary_inputs_use_the_same_history() -> Result<()> {
    let fixture = Fixture::new()?;
    let root = fixture.write("thread", "thread", None, None, &[
        message("u", "user", "run a probe"),
        json!({"type":"response_item","payload":{"type":"function_call","call_id":"probe","name":"exec_command","arguments":"{\"cmd\":\"printf probe\"}"}}),
    ])?;
    fixture.write("thread", "tip", None, Some(("thread", 3, root.offsets[2])), &[
        json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"probe","output":"probe result"}}),
        message("a", "assistant", "finished"),
    ])?;
    let files = fixture.logical()?;
    let source = files[0].conversation.as_ref().unwrap();
    let session = source.parse()?.unwrap();
    assert!(session.cells.iter().any(|cell| matches!(cell, crate::parse::SessionCell::Exec { stdout, .. } if stdout.contains("probe result"))));
    let mut bytes = Vec::new();
    let count = source.write_jsonl(&mut bytes)?;
    assert_eq!(count, session.lines);
    let parsed = crate::parse::codex::parse_codex_session_lines(
        &source.selected_path,
        std::io::BufReader::new(bytes.as_slice()).lines(),
        false,
    )?
    .unwrap();
    assert_eq!(parsed.messages, session.messages);
    assert_eq!(source.summary_fingerprint(count).line_count, count);
    Ok(())
}

#[test]
fn conflicting_rollout_identities_and_cycles_are_incomplete() -> Result<()> {
    let fixture = Fixture::new()?;
    let root = fixture.write(
        "thread",
        "thread",
        None,
        None,
        &[message("u", "user", "request")],
    )?;
    // Noncanonical files with the same header ID claim the same immutable identity.
    fs::copy(
        &root.path,
        fixture.roots.codex_sessions.join("conflict.jsonl"),
    )?;
    fixture.select("thread", &root.path)?;
    let files = fixture.logical()?;
    assert_eq!(files.len(), 1);
    assert!(!files[0]
        .conversation
        .as_ref()
        .unwrap()
        .history
        .is_complete());
    let other = Fixture::new()?;
    let a = other.write(
        "a",
        "a",
        None,
        Some(("b", 3, 1000)),
        &[message("a", "user", "cycle a")],
    )?;
    other.write(
        "b",
        "b",
        None,
        Some(("a", 3, 1000)),
        &[message("b", "user", "cycle b")],
    )?;
    assert!(a.path.exists());
    assert!(other.logical()?.iter().all(|file| !file
        .conversation
        .as_ref()
        .unwrap()
        .history
        .is_complete()));
    Ok(())
}

#[test]
fn included_dependencies_are_logical_rows_and_unmarking_one_blocks_the_group() -> Result<()> {
    let mut fixture = Fixture::new()?;
    fixture.roots.trash = Some(crate::trash::TrashPaths::from_data_root(
        fixture.temp.path().join("data"),
    ));
    let parent = fixture.write(
        "parent",
        "parent",
        None,
        None,
        &[message("u", "user", "parent request")],
    )?;
    let child = fixture.write(
        "child",
        "child",
        Some("parent"),
        Some(("parent", 2, parent.offsets[1])),
        &[message("c", "user", "child request")],
    )?;
    let tip = fixture.write(
        "child",
        "tip",
        Some("parent"),
        Some(("child", 4, child.offsets[1])),
        &[message("a", "assistant", "answer")],
    )?;
    fixture.select("child", &tip.path)?;
    let options = fixture.rules("rule('parent', { dependencyPolicy: 'include' }, ({session}) => session.id === 'parent' ? trash() : nothing());", RulesMode::Preview)?;
    let report = run_rules(&fixture.roots, &options)?;
    assert_eq!(report.proposals.len(), 2);
    assert_eq!(report.preview_matches.len(), 2);
    let required = report
        .preview_matches
        .iter()
        .find(|matched| matched.hit.session.session_id == "child")
        .unwrap();
    assert_eq!(required.hit.session.file_path, tip.path);
    assert!(required
        .hit
        .session
        .parse()?
        .unwrap()
        .content
        .contains("parent request"));
    let (applied, skipped) = crate::rules::apply_rule_proposals(
        &fixture.roots,
        &report
            .proposals
            .iter()
            .filter(|proposal| proposal.session_id == "parent")
            .cloned()
            .collect::<Vec<_>>(),
    );
    assert!(applied.is_empty());
    assert!(skipped
        .iter()
        .any(|skip| skip.skip_reason.contains("unmarked")));
    let (applied, skipped) = crate::rules::apply_rule_proposals(&fixture.roots, &report.proposals);
    assert!(skipped.is_empty(), "{skipped:?}");
    assert_eq!(applied.len(), 3);
    Ok(())
}

#[test]
fn include_policy_does_not_remove_an_incomplete_dependent() -> Result<()> {
    let mut fixture = Fixture::new()?;
    fixture.roots.trash = Some(crate::trash::TrashPaths::from_data_root(
        fixture.temp.path().join("data"),
    ));
    let parent = fixture.write(
        "parent",
        "parent",
        None,
        None,
        &[message("u", "user", "request")],
    )?;
    let child = fixture.write(
        "child",
        "child",
        Some("parent"),
        Some(("parent", 2, parent.offsets[1] + 100)),
        &[message("c", "user", "incomplete child")],
    )?;
    let options = fixture.rules("rule('parent', { dependencyPolicy: 'include' }, ({session}) => session.id === 'parent' ? trash() : nothing());", RulesMode::Apply)?;
    let report = run_rules(&fixture.roots, &options)?;
    assert!(report.applied.is_empty());
    assert!(!report.skipped.is_empty());
    assert!(parent.path.exists() && child.path.exists());
    Ok(())
}

#[test]
fn renamed_conversations_invalidate_cached_rules_without_rollout_changes() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.write(
        "thread",
        "thread",
        None,
        None,
        &[message("u", "user", "request")],
    )?;
    let names = fixture
        .roots
        .codex_sessions
        .parent()
        .unwrap()
        .join("session_index.jsonl");
    fs::write(&names, "{\"id\":\"thread\",\"thread_name\":\"keep\"}\n")?;
    let options = fixture.rules(
        "rule('title', ({session}) => session.customTitle === 'remove' ? trash() : nothing());",
        RulesMode::Preview,
    )?;
    assert!(run_rules(&fixture.roots, &options)?.proposals.is_empty());
    let source = fixture.logical()?[0].conversation.clone().unwrap();
    fs::write(&names, "{\"id\":\"thread\",\"thread_name\":\"remove\"}\n")?;
    assert!(!source.is_current());
    let report = run_rules(&fixture.roots, &options)?;
    assert_eq!(report.proposals.len(), 1);
    assert_eq!(
        report.preview_matches[0]
            .hit
            .session
            .custom_title
            .as_deref(),
        Some("remove")
    );
    Ok(())
}

#[test]
fn rules_discard_supersession_if_either_compared_source_changes() -> Result<()> {
    let fixture = Fixture::new()?;
    let parent = fixture.write(
        "parent",
        "parent",
        None,
        None,
        &[
            message("u", "user", "original request"),
            message("a", "assistant", "original answer"),
        ],
    )?;
    let child = fixture.write(
        "child",
        "child",
        Some("parent"),
        Some(("parent", 3, parent.offsets[2])),
        &[message("a2", "assistant", "more work")],
    )?;
    let manager = fixture.manager();
    manager.sync_with_roots(&fixture.roots, true)?;
    let mut options = fixture.rules(
        "rule('superseded', ({session}) => session.supersededBy ? trash() : nothing());",
        RulesMode::Preview,
    )?;
    options.supersession = manager.supersession_snapshot()?;
    assert_eq!(run_rules(&fixture.roots, &options)?.proposals.len(), 1);
    let replacement = fixture.write(
        "child",
        "replacement",
        Some("parent"),
        Some(("child", 5, child.offsets[1])),
        &[message("a3", "assistant", "continued work")],
    )?;
    fixture.select("child", &replacement.path)?;
    assert!(
        run_rules(&fixture.roots, &options)?.proposals.is_empty(),
        "keeper selection changed since indexing"
    );
    manager.sync_with_roots(&fixture.roots, false)?;
    options.supersession = manager.supersession_snapshot()?;
    assert_eq!(run_rules(&fixture.roots, &options)?.proposals.len(), 1);
    use std::io::Write;
    let mut output = fs::OpenOptions::new().append(true).open(&parent.path)?;
    let mut extra = message("parent-only", "user", "new parent work");
    extra["ordinal"] = json!(3);
    writeln!(output, "{extra}")?;
    assert!(
        run_rules(&fixture.roots, &options)?.proposals.is_empty(),
        "source history changed since indexing"
    );
    Ok(())
}

#[test]
fn changed_keeper_blocks_a_previously_independent_cleanup_proposal() -> Result<()> {
    let mut fixture = Fixture::new()?;
    fixture.roots.trash = Some(crate::trash::TrashPaths::from_data_root(
        fixture.temp.path().join("data"),
    ));
    let parent = fixture.write(
        "parent",
        "parent",
        None,
        None,
        &[
            message("u", "user", "request"),
            message("a", "assistant", "important answer"),
        ],
    )?;
    // This copied fork is self-contained, so deleting its source has no physical dependency.
    let child = fixture.write(
        "child",
        "child",
        Some("parent"),
        None,
        &[
            message("u", "user", "request"),
            message("a", "assistant", "important answer"),
            message("a2", "assistant", "continued"),
        ],
    )?;
    let manager = fixture.manager();
    manager.sync_with_roots(&fixture.roots, true)?;
    let mut options = fixture.rules(
        "rule('superseded', ({session}) => session.supersededBy ? trash() : nothing());",
        RulesMode::Preview,
    )?;
    options.supersession = manager.supersession_snapshot()?;
    let report = run_rules(&fixture.roots, &options)?;
    assert_eq!(report.proposals.len(), 1);
    assert!(report.proposals[0]
        .reviewed_group
        .as_ref()
        .unwrap()
        .blockers
        .is_empty());
    let reverted = fixture.write(
        "child",
        "reverted",
        Some("parent"),
        Some(("child", 2, child.offsets[1])),
        &[message("new", "assistant", "a different answer")],
    )?;
    fixture.select("child", &reverted.path)?;
    let (applied, skipped) = crate::rules::apply_rule_proposals(&fixture.roots, &report.proposals);
    assert!(applied.is_empty());
    assert!(skipped[0].skip_reason.contains("history changed"));
    assert!(parent.path.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn summary_worker_receives_inherited_history_and_keeps_sidecar_at_selected_rollout() -> Result<()> {
    use crate::summary::{
        SummarizeBackend, SummaryCommand, SummaryEvent, SummarySidecar, SummaryWorker,
    };
    let fixture = Fixture::new()?;
    let root = fixture.write(
        "thread",
        "thread",
        None,
        None,
        &[message("u", "user", "inherited summary input")],
    )?;
    let tip = fixture.write(
        "thread",
        "tip",
        None,
        Some(("thread", 2, root.offsets[1])),
        &[message("a", "assistant", "latest summary input")],
    )?;
    let source = fixture.logical()?[0].conversation.clone().unwrap();
    let lines = source.parse()?.unwrap().lines;
    let worker = SummaryWorker::spawn()?;
    worker.send(SummaryCommand {
        jsonl_path: tip.path.clone(),
        source: Some(source.clone()),
        backend: SummarizeBackend::Custom,
        command_template: "cat '{{jsonl_path}}' > '{{output_file}}'".into(),
        prompt_template: "Summarize {{jsonl_path}}".into(),
        claude_command: "unused".into(),
        claude_args: String::new(),
        codex_command: "unused".into(),
        codex_args: String::new(),
        antigravity_command: "unused".into(),
        antigravity_args: String::new(),
    })?;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let sidecar_path = loop {
        match worker.try_recv() {
            Some(SummaryEvent::Completed { path, sidecar_path }) => {
                assert_eq!(path, tip.path);
                break sidecar_path;
            }
            Some(SummaryEvent::Failed { error, .. }) => anyhow::bail!("{error}"),
            _ if std::time::Instant::now() >= deadline => anyhow::bail!("summary worker timed out"),
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    assert_eq!(sidecar_path, crate::summary::sidecar_path(&tip.path));
    let sidecar = SummarySidecar::read(&sidecar_path)?;
    assert!(sidecar.body.contains("inherited summary input"));
    assert!(sidecar.body.contains("latest summary input"));
    assert!(sidecar.is_fresh(&source.summary_fingerprint(lines)));
    Ok(())
}
