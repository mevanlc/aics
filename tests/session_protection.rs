use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use aics::index::{
    IndexManager, IndexPaths, Scope, SearchFilters, SearchRequest, SortMode, TrashFilter,
};
use aics::live::{LiveSessionPaths, LiveSessionTracker};
use aics::parse::Agent;
use aics::rules::{
    apply_rule_proposals, run_rules_with_progress, RuleAction, RuleProposal, RuleSelection,
    RulesMode, RulesOptions, RulesProgress,
};
use aics::scan::SessionRoots;
use aics::trash::{TrashPaths, TrashReason, TrashStore};
use anyhow::Result;
use tempfile::TempDir;

#[path = "common/lock_holder.rs"]
mod lock_holder;
use lock_holder::HeldLock;

const ID: &str = "shared-session-id";

struct Fixtures {
    roots: SessionRoots,
    options: RulesOptions,
    claude: PathBuf,
    codex: PathBuf,
    antigravity: PathBuf,
}

impl Fixtures {
    fn new(temp: &TempDir, source: &str) -> Result<Self> {
        let base = temp.path();
        let claude_home = base.join("claude");
        let codex_home = base.join("codex");
        let antigravity_home = base.join("antigravity");
        // Deliberately noncanonical filenames: protection must use transcript IDs.
        let claude = claude_home.join("projects/project/unusual.jsonl");
        let codex = codex_home.join("sessions/unusual.jsonl");
        let antigravity = antigravity_home.join(format!(
            "brain/{ID}/.system_generated/logs/transcript.jsonl"
        ));
        for path in [&claude, &codex, &antigravity] {
            fs::create_dir_all(path.parent().unwrap())?;
        }
        fs::write(
            &claude,
            serde_json::json!({"type":"user", "sessionId":ID,
            "message":{"role":"user","content":"searchable transcript"}})
            .to_string()
                + "\n",
        )?;
        fs::write(&codex, [
            serde_json::json!({"type":"session_meta", "payload":{"id":ID,"cwd":"/tmp/protection-fixture"}}),
            serde_json::json!({"type":"response_item", "payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"searchable transcript"}]}}),
        ].iter().map(ToString::to_string).collect::<Vec<_>>().join("\n") + "\n")?;
        fs::write(&antigravity, serde_json::json!({"step_index":0,"source":"USER_EXPLICIT", "type":"USER_INPUT","status":"DONE","content":"searchable transcript"}).to_string() + "\n")?;
        let live_sessions = LiveSessionPaths {
            claude_sessions_dir: Some(claude_home.join("sessions")),
            codex_writer_locks_dir: Some(codex_home.join("thread-writer-locks")),
            antigravity_presence_dir: Some(antigravity_home.join("presence")),
        };
        let rules_path = base.join("rules.js");
        fs::write(&rules_path, source)?;
        Ok(Self {
            roots: SessionRoots {
                live_sessions,
                claude_projects: claude_home.join("projects"),
                codex_sessions: codex_home.join("sessions"),
                antigravity_home,
                trash: Some(TrashPaths::from_data_root(base.join("data"))),
            },
            options: RulesOptions {
                rules_path,
                cache_path: Some(base.join("rules-cache.json")),
                mode: RulesMode::Preview,
                selection: RuleSelection::All,
                json: true,
                scope: Scope::Global,
                filters: SearchFilters::default(),
                supersession: BTreeMap::new(),
            },
            claude,
            codex,
            antigravity,
        })
    }

    fn marker(&self, value: serde_json::Value) -> Result<PathBuf> {
        let root = self
            .roots
            .live_sessions
            .claude_sessions_dir
            .as_ref()
            .unwrap();
        fs::create_dir_all(root)?;
        let path = root.join("owner.json");
        fs::write(&path, value.to_string())?;
        Ok(path)
    }

    fn codex_lock(&self) -> PathBuf {
        self.roots
            .live_sessions
            .codex_writer_locks_dir
            .as_ref()
            .unwrap()
            .join(format!("{ID}.lock"))
    }

    fn antigravity_lock(&self) -> PathBuf {
        self.roots
            .live_sessions
            .antigravity_presence_dir
            .as_ref()
            .unwrap()
            .join(format!("{ID}.lock"))
    }

    fn coordinate_codex(&self) -> Result<()> {
        let root = self
            .roots
            .live_sessions
            .codex_writer_locks_dir
            .as_ref()
            .unwrap();
        fs::create_dir_all(root)?;
        fs::write(root.join(".coordination.lock"), b"")?;
        Ok(())
    }
}

#[test]
fn protected_callbacks_never_run_and_become_eligible_after_exit() -> Result<()> {
    let temp = TempDir::new()?;
    let fixtures = Fixtures::new(
        &temp,
        r#"rule("must not execute", () => { throw new Error("callback executed"); });"#,
    )?;
    let marker = fixtures.marker(serde_json::json!({"pid":std::process::id(),"sessionId":ID}))?;
    let codex = HeldLock::new(&fixtures.codex_lock());
    let antigravity = HeldLock::new(&fixtures.antigravity_lock());
    for selection in [RuleSelection::All, RuleSelection::ApplyAtStartup] {
        let mut options = fixtures.options.clone();
        options.selection = selection;
        // Startup-only callbacks must also be suppressed.
        fs::write(
            &options.rules_path,
            r#"rule("must not execute", {applyAtStartup:true}, () => { throw new Error("callback executed"); });"#,
        )?;
        let report = run_rules_with_progress(&fixtures.roots, &options, |_| {})?;
        assert!(report.errors.is_empty());
        assert!(report.preview_matches.is_empty());
        assert_eq!(report.processing_skips.len(), 3);
    }
    let cached: serde_json::Value =
        serde_json::from_slice(&fs::read(fixtures.options.cache_path.as_ref().unwrap())?)?;
    assert!(cached["sessions"].as_object().unwrap().is_empty());
    drop(codex);
    drop(antigravity);
    fs::remove_file(marker)?;
    let eligible = run_rules_with_progress(&fixtures.roots, &fixtures.options, |_| {})?;
    assert_eq!(eligible.errors.len(), 3);
    assert!(eligible.processing_skips.is_empty());
    Ok(())
}

#[test]
fn cached_proposals_are_excluded_while_locked_then_reused_after_release() -> Result<()> {
    let temp = TempDir::new()?;
    let fixtures = Fixtures::new(&temp, r#"rule("trash", () => trash());"#)?;
    let first = run_rules_with_progress(&fixtures.roots, &fixtures.options, |_| {})?;
    assert_eq!(first.proposals.len(), 3);
    let cache_before = fs::read(fixtures.options.cache_path.as_ref().unwrap())?;
    let marker = fixtures.marker(serde_json::json!({"pid":std::process::id(),"sessionId":ID}))?;
    let codex = HeldLock::new(&fixtures.codex_lock());
    let antigravity = HeldLock::new(&fixtures.antigravity_lock());
    let protected = run_rules_with_progress(&fixtures.roots, &fixtures.options, |_| {})?;
    assert!(protected.proposals.is_empty());
    assert_eq!(protected.processing_skips.len(), 3);
    assert_eq!(
        fs::read(fixtures.options.cache_path.as_ref().unwrap())?,
        cache_before
    );
    drop(codex);
    drop(antigravity);
    fs::remove_file(marker)?;
    let released = run_rules_with_progress(&fixtures.roots, &fixtures.options, |_| {})?;
    assert_eq!(released.proposals, first.proposals);
    assert!(released.processing_skips.is_empty());
    Ok(())
}

#[test]
fn cached_no_action_and_informational_results_are_excluded_when_live() -> Result<()> {
    for source in [
        r#"rule("none", () => nothing());"#,
        r#"rule("note", () => nothing("informational"));"#,
    ] {
        let temp = TempDir::new()?;
        let fixtures = Fixtures::new(&temp, source)?;
        let first = run_rules_with_progress(&fixtures.roots, &fixtures.options, |_| {})?;
        assert!(first.proposals.is_empty());
        assert!(first.processing_skips.is_empty());
        let cache = fixtures.options.cache_path.as_ref().unwrap();
        let cached_bytes = fs::read(cache)?;
        let marker =
            fixtures.marker(serde_json::json!({"pid":std::process::id(),"sessionId":ID}))?;
        let codex = HeldLock::new(&fixtures.codex_lock());
        let antigravity = HeldLock::new(&fixtures.antigravity_lock());
        let protected = run_rules_with_progress(&fixtures.roots, &fixtures.options, |_| {})?;
        assert_eq!(protected.processing_skips.len(), 3);
        assert!(protected.preview_matches.is_empty());
        assert_eq!(fs::read(cache)?, cached_bytes);
        drop(codex);
        drop(antigravity);
        fs::remove_file(marker)?;
        let released = run_rules_with_progress(&fixtures.roots, &fixtures.options, |_| {})?;
        assert!(released.processing_skips.is_empty());
        assert_eq!(released.preview_matches.len(), first.preview_matches.len());
    }
    Ok(())
}

#[test]
fn stale_markers_and_unlocked_files_allow_rule_actions() -> Result<()> {
    let temp = TempDir::new()?;
    let fixtures = Fixtures::new(&temp, r#"rule("trash", () => trash());"#)?;
    let mut child = Command::new(std::env::current_exe()?)
        .args(["--exact", "lock_holder::lock_holder_process"])
        .env_remove("AICS_TEST_HELD_LOCK")
        .stdout(std::process::Stdio::null())
        .spawn()?;
    let dead_pid = child.id();
    assert!(child.wait()?.success());
    fixtures.marker(serde_json::json!({"pid":dead_pid,"sessionId":ID}))?;
    fixtures.coordinate_codex()?;
    fs::write(fixtures.codex_lock(), "unlocked stale marker")?;
    fs::create_dir_all(fixtures.antigravity_lock().parent().unwrap())?;
    fs::write(fixtures.antigravity_lock(), "unlocked stale marker")?;
    let report = run_rules_with_progress(&fixtures.roots, &fixtures.options, |_| {})?;
    assert_eq!(report.proposals.len(), 3);
    let (applied, skipped) = apply_rule_proposals(&fixtures.roots, &report.proposals);
    assert_eq!(applied.len(), 3);
    assert!(skipped.is_empty(), "{skipped:?}");
    assert_eq!(
        fs::read_to_string(fixtures.codex_lock())?,
        "unlocked stale marker"
    );
    assert_eq!(
        fs::read_to_string(fixtures.antigravity_lock())?,
        "unlocked stale marker"
    );
    Ok(())
}

#[test]
fn invalid_marker_directories_conservatively_block_only_their_provider() -> Result<()> {
    for agent in [Agent::Claude, Agent::Codex, Agent::Antigravity] {
        let temp = TempDir::new()?;
        let fixtures = Fixtures::new(&temp, r#"rule("trash", () => trash());"#)?;
        let initial = run_rules_with_progress(&fixtures.roots, &fixtures.options, |_| {})?;
        let root = match agent {
            Agent::Claude => &fixtures.roots.live_sessions.claude_sessions_dir,
            Agent::Codex => &fixtures.roots.live_sessions.codex_writer_locks_dir,
            Agent::Antigravity => &fixtures.roots.live_sessions.antigravity_presence_dir,
        }
        .as_ref()
        .unwrap();
        fs::write(root, "not a directory")?;
        let report = run_rules_with_progress(&fixtures.roots, &fixtures.options, |_| {})?;
        assert_eq!(report.proposals.len(), 2);
        assert_eq!(report.processing_skips.len(), 1);
        assert_eq!(report.processing_skips[0].agent, agent);
        assert!(report.processing_skips[0].indeterminate);
        let proposal = initial
            .proposals
            .into_iter()
            .find(|proposal| proposal.agent == agent)
            .unwrap();
        let bytes = fs::read(&proposal.path)?;
        let (applied, skipped) =
            apply_rule_proposals(&fixtures.roots, std::slice::from_ref(&proposal));
        assert!(applied.is_empty());
        assert_eq!(skipped.len(), 1);
        assert_eq!(fs::read(&proposal.path)?, bytes);
    }
    Ok(())
}

#[test]
fn proposals_recheck_activity_and_preserve_files_and_trash_metadata() -> Result<()> {
    let temp = TempDir::new()?;
    let fixtures = Fixtures::new(&temp, r#"rule("trash", () => trash());"#)?;
    let report = run_rules_with_progress(&fixtures.roots, &fixtures.options, |_| {})?;
    assert_eq!(report.proposals.len(), 3);
    let original_bytes = [&fixtures.claude, &fixtures.codex, &fixtures.antigravity]
        .map(|path| fs::read(path).unwrap());
    let metadata_path = &fixtures.roots.trash.as_ref().unwrap().metadata_file;
    let metadata_before = fs::read(metadata_path)?;
    fixtures.marker(serde_json::json!({"pid":std::process::id(),"sessionId":ID}))?;
    fixtures.coordinate_codex()?;
    let codex = HeldLock::new(&fixtures.codex_lock());
    let antigravity = HeldLock::new(&fixtures.antigravity_lock());
    let (applied, skipped) = apply_rule_proposals(&fixtures.roots, &report.proposals);
    assert!(applied.is_empty());
    assert_eq!(skipped.len(), 3);
    assert!(skipped
        .iter()
        .all(|skip| skip.skip_reason == "session is live or locked"));
    for (path, bytes) in [&fixtures.claude, &fixtures.codex, &fixtures.antigravity]
        .into_iter()
        .zip(original_bytes)
    {
        assert_eq!(fs::read(path)?, bytes);
    }
    assert_eq!(fs::read(metadata_path)?, metadata_before);
    drop(codex);
    drop(antigravity);
    Ok(())
}

#[test]
fn activity_starting_after_evaluation_removes_preview_and_blocks_application() -> Result<()> {
    let temp = TempDir::new()?;
    let fixtures = Fixtures::new(&temp, r#"rule("trash", () => trash());"#)?;
    let mut options = fixtures.options.clone();
    options.mode = RulesMode::Apply;
    let mut lock = None;
    let report = run_rules_with_progress(&fixtures.roots, &options, |progress| {
        if matches!(
            progress,
            RulesProgress::ProcessingProgress {
                processed: 3,
                total: 3
            }
        ) {
            fixtures
                .marker(serde_json::json!({"pid":std::process::id(),"sessionId":ID}))
                .unwrap();
            fixtures.coordinate_codex().unwrap();
            lock = Some((
                HeldLock::new(&fixtures.codex_lock()),
                HeldLock::new(&fixtures.antigravity_lock()),
            ));
        }
    })?;
    assert!(report.proposals.is_empty());
    assert!(report.applied.is_empty());
    assert_eq!(report.processing_skips.len(), 3);
    assert!(
        fixtures.claude.is_file() && fixtures.codex.is_file() && fixtures.antigravity.is_file()
    );
    drop(lock);
    Ok(())
}

#[test]
fn live_state_is_provider_specific_and_filters_all_search_paths() -> Result<()> {
    let temp = TempDir::new()?;
    let fixtures = Fixtures::new(&temp, r#"rule("trash", () => trash());"#)?;
    let tracker = LiveSessionTracker::new(fixtures.roots.live_sessions.clone());
    assert!(!tracker.snapshot().is_live(Agent::Claude, ID));
    let marker = fixtures.marker(serde_json::json!({"pid":std::process::id(),"sessionId":ID}))?;
    assert!(tracker.snapshot().is_live(Agent::Claude, ID));
    assert!(!tracker.snapshot().is_live(Agent::Codex, ID));
    let index_paths = IndexPaths::from_root(temp.path().join("index"));
    let manager = IndexManager::with_paths(index_paths);
    manager.sync_with_roots(&fixtures.roots, true)?;
    let engine = manager.open_search_engine_with_live_sessions(tracker)?;
    let mut request = SearchRequest {
        visibility_search: aics::search_query::VisibilitySearch::All,
        query: String::new(),
        scope: Scope::Global,
        limit: 10,
        sort: SortMode::Time,
        filters: SearchFilters {
            live_only: true,
            ..Default::default()
        },
    };
    let hits = engine.search(&request)?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session.agent, Agent::Claude);
    let codex = HeldLock::new(&fixtures.codex_lock());
    let antigravity = HeldLock::new(&fixtures.antigravity_lock());
    for query in ["", "searchable"] {
        for sort in [SortMode::Time, SortMode::Relevance] {
            request.query = query.to_owned();
            request.sort = sort;
            let hits = engine.search(&request)?;
            assert_eq!(hits.len(), 3, "query={query:?}, sort={sort:?}");
            assert!(hits.iter().all(|hit| hit.is_live));
        }
    }
    fs::remove_file(marker)?;
    drop(codex);
    drop(antigravity);
    assert!(engine.search(&request)?.is_empty());
    Ok(())
}

#[test]
fn uncertain_marker_excludes_only_its_provider_and_is_diagnostic() -> Result<()> {
    let temp = TempDir::new()?;
    let fixtures = Fixtures::new(&temp, r#"rule("trash", () => trash());"#)?;
    let marker = fixtures.marker(serde_json::json!({"sessionId":ID,"pid":"unknown"}))?;
    let snapshot = LiveSessionTracker::new(fixtures.roots.live_sessions.clone()).snapshot();
    assert!(snapshot.is_live(Agent::Claude, ID));
    let report = run_rules_with_progress(&fixtures.roots, &fixtures.options, |_| {})?;
    assert_eq!(report.processing_skips.len(), 1);
    assert!(report.processing_skips[0].indeterminate);
    assert_eq!(report.proposals.len(), 2);
    fs::write(marker, "{broken")?;
    let report = run_rules_with_progress(&fixtures.roots, &fixtures.options, |_| {})?;
    assert_eq!(report.processing_skips.len(), 1);
    assert!(report.processing_skips[0].session_id.is_none());
    assert_eq!(report.proposals.len(), 2);
    Ok(())
}

#[test]
fn active_session_prevents_restoring_its_trashed_copy_for_each_provider() -> Result<()> {
    for agent in [Agent::Claude, Agent::Codex, Agent::Antigravity] {
        let temp = TempDir::new()?;
        let mut fixtures = Fixtures::new(&temp, r#"rule("restore", () => untrash());"#)?;
        let original = match agent {
            Agent::Claude => &fixtures.claude,
            Agent::Codex => &fixtures.codex,
            Agent::Antigravity => &fixtures.antigravity,
        };
        let store = TrashStore::new(fixtures.roots.trash.clone().unwrap());
        let entry = store.trash_session(
            original,
            agent,
            &TrashReason::KeyBinding {
                key: "test".to_owned(),
            },
        )?;
        let trash_path = entry.trash_path(store.paths());
        let metadata_before = fs::read(&store.paths().metadata_file)?;
        fixtures.marker(serde_json::json!({"pid":std::process::id(),"sessionId":ID}))?;
        fixtures.coordinate_codex()?;
        let codex = HeldLock::new(&fixtures.codex_lock());
        let antigravity = HeldLock::new(&fixtures.antigravity_lock());
        let proposal = RuleProposal {
            rule: "restore".to_owned(),
            session_id: ID.to_owned(),
            path: trash_path.clone(),
            agent,
            action: RuleAction::Untrash { reason: None },
        };
        let (applied, skipped) = apply_rule_proposals(&fixtures.roots, &[proposal]);
        assert!(applied.is_empty());
        assert_eq!(skipped.len(), 1);
        assert_eq!(fs::read(&store.paths().metadata_file)?, metadata_before);
        assert!(trash_path.exists());
        assert!(!original.exists());
        fixtures.options.filters.trashed = TrashFilter::Both;
        let report = run_rules_with_progress(&fixtures.roots, &fixtures.options, |_| {})?;
        assert!(report.proposals.is_empty());
        assert_eq!(report.processing_skips.len(), 3);
        drop(codex);
        drop(antigravity);
    }
    Ok(())
}

#[test]
fn environment_marker_homes_are_independent_of_indexed_roots() -> Result<()> {
    let temp = TempDir::new()?;
    let mut fixtures = Fixtures::new(&temp, r#"rule("trash", () => trash());"#)?;
    let claude_home = temp.path().join("runtime-claude");
    let codex_home = temp.path().join("runtime-codex");
    let custom_registry = temp.path().join("custom-claude-registry");
    fixtures.roots.live_sessions.claude_sessions_dir = Some(custom_registry.clone());
    fixtures.roots.live_sessions.codex_writer_locks_dir =
        Some(codex_home.join("thread-writer-locks"));
    fixtures.marker(serde_json::json!({"pid":std::process::id(),"sessionId":ID}))?;
    let codex = HeldLock::new(&fixtures.codex_lock());
    let antigravity = HeldLock::new(&fixtures.antigravity_lock());
    let output = Command::new(env!("CARGO_BIN_EXE_aics"))
        .env("AICS_CONFIG_ROOT", temp.path().join("config"))
        .env("AICS_CACHE_ROOT", temp.path().join("cache"))
        .env("AICS_DATA_ROOT", temp.path().join("data"))
        .env("CLAUDE_CONFIG_DIR", claude_home)
        .env("CODEX_HOME", codex_home)
        .env("AICS_CLAUDE_SESSIONS_DIR", custom_registry)
        .env("AICS_CLAUDE_PROJECTS_DIR", &fixtures.roots.claude_projects)
        .env("AICS_CODEX_SESSIONS_DIR", &fixtures.roots.codex_sessions)
        .env("AICS_ANTIGRAVITY_HOME", &fixtures.roots.antigravity_home)
        .args([
            "--preview-rules",
            "--json",
            "-g",
            "--progress",
            "none",
            "--rules",
        ])
        .arg(&fixtures.options.rules_path)
        .output()?;
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    let rows = String::from_utf8(output.stderr)?
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|row| row["event"] == "rules_processing_skip")
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 3);
    drop(codex);
    drop(antigravity);
    Ok(())
}

#[test]
fn cli_home_overrides_protect_startup_and_json_output() -> Result<()> {
    let temp = TempDir::new()?;
    let fixtures = Fixtures::new(
        &temp,
        r#"rule("trash", {applyAtStartup:true}, () => trash());"#,
    )?;
    fixtures.marker(serde_json::json!({"pid":std::process::id(),"sessionId":ID}))?;
    fixtures.coordinate_codex()?;
    let codex = HeldLock::new(&fixtures.codex_lock());
    let antigravity = HeldLock::new(&fixtures.antigravity_lock());
    for mode in ["--preview-rules", "--apply-rules", "--json"] {
        let output = Command::new(env!("CARGO_BIN_EXE_aics"))
            .env("AICS_CONFIG_ROOT", temp.path().join("config"))
            .env("AICS_CACHE_ROOT", temp.path().join("cache"))
            .env("AICS_DATA_ROOT", temp.path().join("data"))
            // CLI homes take precedence over unrelated environment marker directories.
            .env("AICS_CLAUDE_SESSIONS_DIR", temp.path().join("unrelated"))
            .args([mode, "-g", "--progress", "none", "--rules"])
            .arg(&fixtures.options.rules_path)
            .arg("--claude-home")
            .arg(fixtures.roots.claude_projects.parent().unwrap())
            .arg("--codex-home")
            .arg(fixtures.roots.codex_sessions.parent().unwrap())
            .arg("--antigravity-home")
            .arg(&fixtures.roots.antigravity_home)
            .args(if mode == "--json" {
                vec!["--live"]
            } else {
                vec!["--json"]
            })
            .output()?;
        assert!(output.status.success(), "{output:?}");
        if mode == "--json" {
            let rows = String::from_utf8(output.stdout)?
                .lines()
                .map(serde_json::from_str::<serde_json::Value>)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            assert_eq!(rows.len(), 3);
            assert!(rows.iter().all(|row| row["is_live"] == true));
            assert!(!String::from_utf8(output.stderr)?.contains("startup rule actions failed"));
        } else {
            assert!(output.stdout.is_empty());
            let rows = String::from_utf8(output.stderr)?
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .collect::<Vec<_>>();
            assert_eq!(
                rows.iter()
                    .filter(|row| row["event"] == "rules_processing_skip")
                    .count(),
                3
            );
        }
        assert!(
            fixtures.claude.exists() && fixtures.codex.exists() && fixtures.antigravity.exists()
        );
    }
    drop(codex);
    drop(antigravity);
    Ok(())
}
