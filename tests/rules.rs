use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use aics::index::{IndexManager, IndexPaths, Scope, SearchFilters};
use aics::parse::Agent;
use aics::rules::{
    apply_rule_proposals, run_rules_with_progress, RuleAction, RuleProposal, RuleSelection,
    RulesMode, RulesOptions, RulesProgress,
};
use aics::scan::SessionRoots;
use aics::trash::{TrashPaths, TrashStore};
use anyhow::Result;
use tempfile::TempDir;

#[test]
fn write_rules_dts_creates_default_config_file() -> Result<()> {
    let temp = TempDir::new()?;
    let config_root = temp.path().join("config");

    let output = Command::new(env!("CARGO_BIN_EXE_aics"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("AICS_CONFIG_ROOT", &config_root)
        .args(["--write-rules-dts"])
        .output()?;

    assert!(output.status.success(), "{output:#?}");

    let rules_dts = config_root.join("rules.d.ts");
    assert!(rules_dts.exists());
    let stdout = String::from_utf8(output.stdout)?;
    assert_eq!(stdout.trim(), rules_dts.display().to_string());

    let contents = fs::read_to_string(rules_dts)?;
    assert_eq!(contents, include_str!("../src/rules/rules.d.ts"));
    assert!(contents.contains("interface AicsRuleSession"));
    let session_declaration = contents
        .split_once("interface AicsRuleSession {")
        .unwrap()
        .1
        .split_once("\n}")
        .unwrap()
        .0;
    for field in [
        "cwd",
        "branch",
        "customTitle",
        "model",
        "modelProvider",
        "reasoningEffort",
        "approvalPolicy",
        "sandboxMode",
        "supersededBy",
    ] {
        assert!(session_declaration.contains(&format!("{field}: string;")));
        assert!(!session_declaration.contains(&format!("{field}: string | null;")));
    }
    assert!(contents.contains("declare function rule("));
    assert!(contents.contains("interface AicsRuleConfig"));
    assert!(contents.contains("applyAtStartup?: boolean;"));
    assert!(contents.contains("config: AicsRuleConfig,"));
    assert!(contents.contains("declare function trash(reason?: string): AicsTrashAction;"));
    assert!(contents.contains("declare function untrash(reason?: string): AicsUntrashAction;"));
    Ok(())
}

#[test]
fn rules_expose_missing_session_strings_as_empty() -> Result<()> {
    let temp = TempDir::new()?;
    let claude_session = temp
        .path()
        .join(".claude/projects/-tmp-project/model-less.jsonl");
    fs::create_dir_all(claude_session.parent().unwrap())?;
    fs::write(
        &claude_session,
        concat!(
            r#"{"type":"user","sessionId":"model-less","message":{"role":"user","content":"first"}}"#,
            "\n",
            r#"{"type":"user","sessionId":"model-less","message":{"role":"user","content":"second"}}"#,
            "\n",
        ),
    )?;
    let rules = write_rules(
        &temp,
        r#"
        rule("missing strings", ({ session }) => {
          const optionalStrings = [
            session.cwd,
            session.branch,
            session.customTitle,
            session.model,
            session.modelProvider,
            session.reasoningEffort,
            session.approvalPolicy,
            session.sandboxMode,
            session.supersededBy,
          ];
          return optionalStrings.every(value => value === "" && !value.includes("present"))
            ? trash("empty strings")
            : nothing();
        });
        "#,
    )?;
    let session_roots = SessionRoots {
        claude_projects: temp.path().join(".claude/projects"),
        codex_sessions: temp.path().join(".codex/sessions"),
        antigravity_home: temp.path().join(".gemini/antigravity-cli"),
        trash: None,
    };

    let report = run_rules_with_progress(
        &session_roots,
        &RulesOptions {
            rules_path: rules,
            cache_path: None,
            mode: RulesMode::Preview,
            selection: RuleSelection::All,
            json: true,
            scope: Scope::Global,
            filters: SearchFilters::default(),
            supersession: BTreeMap::new(),
        },
        |_| {},
    )?;

    assert!(report.errors.is_empty());
    assert_eq!(report.proposals.len(), 1);
    assert_eq!(report.proposals[0].rule, "missing strings");
    Ok(())
}

#[test]
fn rules_expose_supersession_keeper_id_and_invalidate_cached_outcomes() -> Result<()> {
    let temp = TempDir::new()?;
    let sessions = temp.path().join(".codex/sessions/2026/08/05");
    fs::create_dir_all(&sessions)?;
    let parent = sessions.join("parent.jsonl");
    fs::write(
        &parent,
        concat!(
            "{\"timestamp\":\"2026-08-05T10:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"parent\",\"cwd\":\"/tmp/demo\"}}\n",
            "{\"timestamp\":\"2026-08-05T10:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"user-1\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hello\"}]}}\n",
            "{\"timestamp\":\"2026-08-05T10:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"assistant-1\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}}\n",
        ),
    )?;
    let child = sessions.join("child.jsonl");
    fs::write(
        &child,
        concat!(
            "{\"timestamp\":\"2026-08-05T10:01:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"child\",\"forked_from_id\":\"parent\",\"cwd\":\"/tmp/demo\"}}\n",
            "{\"timestamp\":\"2026-08-05T10:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"user-1\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hello\"}]}}\n",
            "{\"timestamp\":\"2026-08-05T10:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"assistant-1\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}}\n",
            "{\"timestamp\":\"2026-08-05T10:01:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"user-2\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"continue here\"}]}}\n",
        ),
    )?;
    let roots = SessionRoots {
        claude_projects: temp.path().join(".claude/projects"),
        codex_sessions: temp.path().join(".codex/sessions"),
        antigravity_home: temp.path().join(".gemini/antigravity-cli"),
        trash: None,
    };
    let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path().join("index-cache")));
    manager.sync_with_roots(&roots, true)?;
    let supersession = manager.supersession_map()?;
    assert_eq!(supersession.get(&parent).map(String::as_str), Some("child"));
    assert!(!supersession.contains_key(&child));

    let rules = write_rules(
        &temp,
        r#"
        rule("superseded", ({ session }) => {
          return session.id === "parent" && session.supersededBy === "child"
            ? trash("superseded")
            : nothing();
        });
        rule("keeper", ({ session }) => {
          return session.id === "child" && session.supersededBy === ""
            ? trash("keeper")
            : nothing();
        });
        "#,
    )?;
    let mut options = RulesOptions {
        rules_path: rules,
        cache_path: Some(temp.path().join("rules-cache.json")),
        mode: RulesMode::Preview,
        selection: RuleSelection::All,
        json: true,
        scope: Scope::Global,
        filters: SearchFilters::default(),
        supersession: BTreeMap::new(),
    };

    let before = aics::rules::run_rules(&roots, &options)?;
    assert_eq!(before.proposals.len(), 1);
    assert_eq!(before.proposals[0].rule, "keeper");

    options.supersession = supersession;
    let after = aics::rules::run_rules(&roots, &options)?;
    assert_eq!(after.proposals.len(), 2);
    assert!(after
        .proposals
        .iter()
        .any(|proposal| proposal.rule == "superseded"));
    assert!(after
        .proposals
        .iter()
        .any(|proposal| proposal.rule == "keeper"));
    Ok(())
}

#[test]
fn preview_rules_emits_jsonl_proposals_without_modifying_files() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let config_root = temp.path().join("config");
    fs::create_dir_all(&config_root)?;
    fs::write(
        config_root.join("rules.js"),
        r#"
        rule("default codex rule", ({ session }) => {
          return session.agent === "codex" ? trash("default cache") : nothing();
        });
        "#,
    )?;
    let rules = write_rules(
        &temp,
        r#"
        rule("trash claude basic", { applyAtStartup: false }, ({ session }) => {
          return session.agent === "claude" && session.id === "c0d1e2f3-a4b5-4c6d-8e7f-9a0b1c2d3e4f"
            ? trash("matched fixture")
            : nothing();
        });
        "#,
    )?;
    let cache_root = temp.path().join("cache");
    let data_root = temp.path().join("data");

    let default_output = Command::new(env!("CARGO_BIN_EXE_aics"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("AICS_CONFIG_ROOT", &config_root)
        .env("AICS_CACHE_ROOT", &cache_root)
        .env("AICS_DATA_ROOT", &data_root)
        .env("AICS_CLAUDE_PROJECTS_DIR", &roots.claude_projects)
        .env("AICS_CODEX_SESSIONS_DIR", &roots.codex_sessions)
        .env("AICS_ANTIGRAVITY_HOME", temp.path().join("antigravity"))
        .args(["--preview-rules", "--json", "--progress", "none", "-g"])
        .output()?;
    assert!(default_output.status.success(), "{default_output:#?}");
    let rules_caches = profile_rules_caches(&cache_root)?;
    assert_eq!(rules_caches.len(), 1);
    let cached_default_rules = fs::read(&rules_caches[0])?;

    let output = Command::new(env!("CARGO_BIN_EXE_aics"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("AICS_CONFIG_ROOT", &config_root)
        .env("AICS_CACHE_ROOT", &cache_root)
        .env("AICS_DATA_ROOT", &data_root)
        .env("AICS_CLAUDE_PROJECTS_DIR", &roots.claude_projects)
        .env("AICS_CODEX_SESSIONS_DIR", &roots.codex_sessions)
        .env("AICS_ANTIGRAVITY_HOME", temp.path().join("antigravity"))
        .args([
            "--preview-rules",
            "--rules",
            rules.to_str().unwrap(),
            "--json",
            "--progress",
            "none",
            "-g",
        ])
        .output()?;

    assert!(output.status.success(), "{output:#?}");
    assert!(roots.claude_session.exists());

    let stdout = String::from_utf8(output.stdout)?;
    let lines = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1);
    let value: serde_json::Value = serde_json::from_str(lines[0])?;
    assert_eq!(value["rule"], "trash claude basic");
    assert_eq!(value["action"], "trash");
    assert_eq!(value["reason"], "matched fixture");
    assert_eq!(value["agent"], "claude");
    assert_eq!(profile_rules_caches(&cache_root)?, rules_caches);
    assert_eq!(fs::read(&rules_caches[0])?, cached_default_rules);
    Ok(())
}

#[test]
fn ordinary_json_startup_applies_only_rules_enabled_at_startup() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let config_root = temp.path().join("config");
    fs::create_dir_all(&config_root)?;
    fs::write(
        config_root.join("rules.js"),
        r#"
        rule("default startup rule", { applyAtStartup: true }, () => nothing());
        "#,
    )?;
    let rules = write_rules(
        &temp,
        r#"
        rule("startup claude", { applyAtStartup: true }, ({ session }) => {
          return session.agent === "claude" ? trash("startup cleanup") : nothing();
        });
        rule("manual codex", { applyAtStartup: false }, ({ session }) => {
          return session.agent === "codex" ? trash("manual cleanup") : nothing();
        });
        "#,
    )?;
    let cache_root = temp.path().join("cache");
    let data_root = temp.path().join("data");

    let default_output = Command::new(env!("CARGO_BIN_EXE_aics"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("AICS_CONFIG_ROOT", &config_root)
        .env("AICS_CACHE_ROOT", &cache_root)
        .env("AICS_DATA_ROOT", &data_root)
        .env("AICS_CLAUDE_PROJECTS_DIR", &roots.claude_projects)
        .env("AICS_CODEX_SESSIONS_DIR", &roots.codex_sessions)
        .env("AICS_ANTIGRAVITY_HOME", temp.path().join("antigravity"))
        .args(["--json", "--progress", "none", "-g"])
        .output()?;
    assert!(default_output.status.success(), "{default_output:#?}");
    let startup_caches = profile_startup_rules_caches(&cache_root)?;
    assert_eq!(startup_caches.len(), 1);
    let cached_default_rules = fs::read(&startup_caches[0])?;

    let output = Command::new(env!("CARGO_BIN_EXE_aics"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("AICS_CONFIG_ROOT", &config_root)
        .env("AICS_CACHE_ROOT", &cache_root)
        .env("AICS_DATA_ROOT", &data_root)
        .env("AICS_CLAUDE_PROJECTS_DIR", &roots.claude_projects)
        .env("AICS_CODEX_SESSIONS_DIR", &roots.codex_sessions)
        .env("AICS_ANTIGRAVITY_HOME", temp.path().join("antigravity"))
        .args([
            "--rules",
            rules.to_str().unwrap(),
            "--json",
            "--progress",
            "none",
        ])
        .output()?;

    assert!(output.status.success(), "{output:#?}");
    assert!(!roots.claude_session.exists());
    assert!(roots.codex_session.exists());
    assert!(fs::read_to_string(data_root.join("trash.jsonl"))?.contains("basic_session.jsonl"));
    assert_eq!(profile_startup_rules_caches(&cache_root)?, startup_caches);
    assert_eq!(fs::read(&startup_caches[0])?, cached_default_rules);
    assert!(profile_rules_caches(&cache_root)?.is_empty());
    Ok(())
}

#[test]
fn no_apply_rules_disables_automatic_startup_rules() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let config_root = temp.path().join("config");
    fs::create_dir_all(&config_root)?;
    fs::write(
        config_root.join("rules.js"),
        r#"
        rule("startup claude", { applyAtStartup: true }, ({ session }) => {
          return session.agent === "claude" ? trash("startup cleanup") : nothing();
        });
        "#,
    )?;
    let cache_root = temp.path().join("cache");
    let data_root = temp.path().join("data");

    let output = Command::new(env!("CARGO_BIN_EXE_aics"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("AICS_CONFIG_ROOT", config_root)
        .env("AICS_CACHE_ROOT", &cache_root)
        .env("AICS_DATA_ROOT", &data_root)
        .env("AICS_CLAUDE_PROJECTS_DIR", &roots.claude_projects)
        .env("AICS_CODEX_SESSIONS_DIR", &roots.codex_sessions)
        .env("AICS_ANTIGRAVITY_HOME", temp.path().join("antigravity"))
        .args(["--no-apply-rules", "--json", "--progress", "none"])
        .output()?;

    assert!(output.status.success(), "{output:#?}");
    assert!(roots.claude_session.exists());
    assert!(roots.codex_session.exists());
    assert!(profile_startup_rules_caches(&cache_root)?.is_empty());
    Ok(())
}

#[test]
fn preview_rules_exposes_reasoning_effort() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let rules = write_rules(
        &temp,
        r#"
        rule("trash medium effort codex", ({ session }) => {
          return session.agent === "codex" && session.reasoningEffort === "medium"
            ? trash("matched reasoning effort")
            : nothing();
        });
        "#,
    )?;
    let cache_root = temp.path().join("cache");
    let data_root = temp.path().join("data");

    let output = Command::new(env!("CARGO_BIN_EXE_aics"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("AICS_CONFIG_ROOT", temp.path().join("config"))
        .env("AICS_CACHE_ROOT", &cache_root)
        .env("AICS_DATA_ROOT", &data_root)
        .env("AICS_CLAUDE_PROJECTS_DIR", &roots.claude_projects)
        .env("AICS_CODEX_SESSIONS_DIR", &roots.codex_sessions)
        .env("AICS_ANTIGRAVITY_HOME", temp.path().join("antigravity"))
        .args([
            "--preview-rules",
            "--rules",
            rules.to_str().unwrap(),
            "--json",
            "--progress",
            "none",
            "-g",
        ])
        .output()?;

    assert!(output.status.success(), "{output:#?}");
    assert!(roots.claude_session.exists());

    let stdout = String::from_utf8(output.stdout)?;
    let lines = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1);
    let value: serde_json::Value = serde_json::from_str(lines[0])?;
    assert_eq!(value["rule"], "trash medium effort codex");
    assert_eq!(value["action"], "trash");
    assert_eq!(value["reason"], "matched reasoning effort");
    assert_eq!(value["agent"], "codex");
    Ok(())
}

#[test]
fn apply_rules_moves_matching_session_to_trash() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let rules = write_rules(
        &temp,
        r#"
        rule("trash claude basic", { applyAtStartup: false }, ({ session }) => {
          return session.agent === "claude" ? trash("cleanup") : nothing();
        });
        "#,
    )?;
    let cache_root = temp.path().join("cache");
    let data_root = temp.path().join("data");

    let output = Command::new(env!("CARGO_BIN_EXE_aics"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("AICS_CONFIG_ROOT", temp.path().join("config"))
        .env("AICS_CACHE_ROOT", &cache_root)
        .env("AICS_DATA_ROOT", &data_root)
        .env("AICS_CLAUDE_PROJECTS_DIR", &roots.claude_projects)
        .env("AICS_CODEX_SESSIONS_DIR", &roots.codex_sessions)
        .env("AICS_ANTIGRAVITY_HOME", temp.path().join("antigravity"))
        .args([
            "--apply-rules",
            "--rules",
            rules.to_str().unwrap(),
            "--json",
            "--progress",
            "none",
            "-g",
        ])
        .output()?;

    assert!(output.status.success(), "{output:#?}");
    assert!(!roots.claude_session.exists());

    let trash_dir = data_root.join("trash");
    let trash_entries = fs::read_dir(&trash_dir)?.collect::<Result<Vec<_>, _>>()?;
    assert_eq!(trash_entries.len(), 1);

    let metadata = fs::read_to_string(data_root.join("trash.jsonl"))?;
    assert!(metadata.contains("basic_session.jsonl"));
    assert!(metadata.contains("\"tn\":\"claude\""));

    let stdout = String::from_utf8(output.stdout)?;
    let lines = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1);
    let value: serde_json::Value = serde_json::from_str(lines[0])?;
    assert_eq!(value["action"], "trash");
    assert_eq!(value["reason"], "cleanup");
    Ok(())
}

#[test]
fn apply_rules_restores_matching_trashed_session() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let original = roots.claude_session.clone();
    let data_root = temp.path().join("data");
    let trash_paths = TrashPaths::from_data_root(&data_root);
    let entry = TrashStore::new(trash_paths.clone()).trash_file(&original, Agent::Claude)?;
    let trashed = entry.trash_path(&trash_paths);
    let rules = write_rules(
        &temp,
        r#"
        rule("restore claude basic", ({ session }) => {
          return session.trashed ? untrash("restore") : nothing();
        });
        "#,
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_aics"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("AICS_CONFIG_ROOT", temp.path().join("config"))
        .env("AICS_CACHE_ROOT", temp.path().join("cache"))
        .env("AICS_DATA_ROOT", &data_root)
        .env("AICS_CLAUDE_PROJECTS_DIR", &roots.claude_projects)
        .env("AICS_CODEX_SESSIONS_DIR", &roots.codex_sessions)
        .env("AICS_ANTIGRAVITY_HOME", temp.path().join("antigravity"))
        .args([
            "--apply-rules",
            "--rules",
            rules.to_str().unwrap(),
            "--trashed",
            "yes",
            "--json",
            "--progress",
            "none",
            "-g",
        ])
        .output()?;

    assert!(output.status.success(), "{output:#?}");
    assert!(original.exists());
    assert!(!trashed.exists());
    assert_eq!(fs::read_to_string(data_root.join("trash.jsonl"))?, "");

    let stdout = String::from_utf8(output.stdout)?;
    let value: serde_json::Value = serde_json::from_str(stdout.trim())?;
    assert_eq!(value["rule"], "restore claude basic");
    assert_eq!(value["action"], "untrash");
    assert_eq!(value["reason"], "restore");
    Ok(())
}

#[test]
fn apply_rules_skips_untrash_for_normal_session() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let rules = write_rules(
        &temp,
        r#"
        rule("restore claude basic", ({ session }) => {
          return session.agent === "claude" ? untrash("restore") : nothing();
        });
        "#,
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_aics"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("AICS_CONFIG_ROOT", temp.path().join("config"))
        .env("AICS_CACHE_ROOT", temp.path().join("cache"))
        .env("AICS_DATA_ROOT", temp.path().join("data"))
        .env("AICS_CLAUDE_PROJECTS_DIR", &roots.claude_projects)
        .env("AICS_CODEX_SESSIONS_DIR", &roots.codex_sessions)
        .env("AICS_ANTIGRAVITY_HOME", temp.path().join("antigravity"))
        .args([
            "--apply-rules",
            "--rules",
            rules.to_str().unwrap(),
            "--json",
            "--progress",
            "none",
            "-g",
        ])
        .output()?;

    assert!(output.status.success(), "{output:#?}");
    assert!(roots.claude_session.exists());

    let stdout = String::from_utf8(output.stdout)?;
    let value: serde_json::Value = serde_json::from_str(stdout.trim())?;
    assert_eq!(value["action"], "untrash");
    assert_eq!(value["reason"], "restore");
    assert_eq!(value["skip_reason"], "session is already untrashed");
    Ok(())
}

#[test]
fn benchmark_rules_evaluates_without_output_or_applying_actions() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let rules = write_rules(
        &temp,
        r#"
        rule("trash claude basic", ({ session }) => {
          return session.agent === "claude" ? trash("cleanup") : nothing();
        });
        "#,
    )?;
    let cache_root = temp.path().join("cache");
    let data_root = temp.path().join("data");

    let output = Command::new(env!("CARGO_BIN_EXE_aics"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("AICS_CONFIG_ROOT", temp.path().join("config"))
        .env("AICS_CACHE_ROOT", &cache_root)
        .env("AICS_DATA_ROOT", &data_root)
        .env("AICS_CLAUDE_PROJECTS_DIR", &roots.claude_projects)
        .env("AICS_CODEX_SESSIONS_DIR", &roots.codex_sessions)
        .env("AICS_ANTIGRAVITY_HOME", temp.path().join("antigravity"))
        .args([
            "--benchmark-rules",
            "--rules",
            rules.to_str().unwrap(),
            "--progress",
            "none",
            "-g",
        ])
        .output()?;

    assert!(output.status.success(), "{output:#?}");
    assert!(output.stdout.is_empty(), "{output:#?}");
    assert!(output.stderr.is_empty(), "{output:#?}");
    assert!(roots.claude_session.exists());
    assert_eq!(fs::read_to_string(data_root.join("trash.jsonl"))?, "");
    assert_eq!(fs::read_dir(data_root.join("trash"))?.count(), 0);
    assert!(profile_rules_caches(&cache_root)?.is_empty());
    Ok(())
}

#[test]
fn applying_antigravity_lifecycle_action_is_safely_skipped() -> Result<()> {
    let temp = TempDir::new()?;
    let transcript = temp.path().join("transcript.jsonl");
    fs::write(&transcript, "bundle stays intact")?;
    let roots = SessionRoots {
        claude_projects: temp.path().join("claude"),
        codex_sessions: temp.path().join("codex"),
        antigravity_home: temp.path().join("antigravity"),
        trash: Some(TrashPaths::from_data_root(temp.path().join("data"))),
    };
    let proposal = RuleProposal {
        rule: "cleanup".to_owned(),
        session_id: "agy-session".to_owned(),
        path: transcript.clone(),
        agent: Agent::Antigravity,
        action: RuleAction::Trash {
            reason: Some("test".to_owned()),
        },
    };

    let (applied, skipped) = apply_rule_proposals(&roots, &[proposal]);

    assert!(applied.is_empty());
    assert_eq!(skipped.len(), 1);
    assert_eq!(
        skipped[0].skip_reason,
        "Antigravity bundle lifecycle actions are unsupported"
    );
    assert!(transcript.exists());
    Ok(())
}

#[test]
fn rules_progress_reports_processing_count() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let rules = write_rules(
        &temp,
        r#"
        rule("noop", () => nothing());
        "#,
    )?;
    let session_roots = SessionRoots {
        claude_projects: roots.claude_projects,
        codex_sessions: roots.codex_sessions,
        antigravity_home: temp.path().join(".gemini/antigravity-cli"),
        trash: None,
    };
    let mut events = Vec::new();

    let report = run_rules_with_progress(
        &session_roots,
        &RulesOptions {
            rules_path: rules,
            cache_path: None,
            mode: RulesMode::Preview,
            selection: RuleSelection::All,
            json: true,
            scope: Scope::Global,
            filters: SearchFilters::default(),
            supersession: BTreeMap::new(),
        },
        |event| events.push(event),
    )?;

    assert!(report.proposals.is_empty());
    assert_eq!(
        events.first(),
        Some(&RulesProgress::ProcessingStarted { total: 2 })
    );
    assert_eq!(
        events.last(),
        Some(&RulesProgress::ProcessingProgress {
            processed: 2,
            total: 2
        })
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, RulesProgress::ProcessingProgress { .. }))
            .count(),
        2
    );
    Ok(())
}

fn write_rules(temp: &TempDir, source: &str) -> Result<PathBuf> {
    let path = temp.path().join("rules.js");
    fs::write(&path, source)?;
    Ok(path)
}

struct FixtureRoots {
    claude_projects: PathBuf,
    codex_sessions: PathBuf,
    claude_session: PathBuf,
    codex_session: PathBuf,
}

fn fixture_roots(temp: &TempDir) -> Result<FixtureRoots> {
    let claude_session = copy_fixture(
        temp,
        "tests/fixtures/sessions/claude/basic_session.jsonl",
        ".claude/projects/-Users-testuser-projects-myapp/basic_session.jsonl",
    )?;
    let codex_session = copy_fixture(
        temp,
        "tests/fixtures/sessions/codex/new_format.jsonl",
        ".codex/sessions/2025/12/10/rollout-new.jsonl",
    )?;

    Ok(FixtureRoots {
        claude_projects: temp.path().join(".claude/projects"),
        codex_sessions: temp.path().join(".codex/sessions"),
        claude_session,
        codex_session,
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

fn profile_rules_caches(cache_root: &Path) -> Result<Vec<PathBuf>> {
    profile_cache_files(cache_root, "rules-cache.json")
}

fn profile_startup_rules_caches(cache_root: &Path) -> Result<Vec<PathBuf>> {
    profile_cache_files(cache_root, "startup-rules-cache.json")
}

fn profile_cache_files(cache_root: &Path, filename: &str) -> Result<Vec<PathBuf>> {
    let profiles = cache_root.join("profiles");
    let mut caches = fs::read_dir(profiles)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|entry| entry.path().join(filename))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    caches.sort();
    Ok(caches)
}
