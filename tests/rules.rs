use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use aics::index::{Scope, SearchFilters};
use aics::rules::{run_rules_with_progress, RulesMode, RulesOptions, RulesProgress};
use aics::scan::SessionRoots;
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
    assert!(contents.contains("interface AicsRuleSession"));
    assert!(contents.contains("declare function rule("));
    assert!(contents.contains("declare function trash(reason?: string): AicsTrashAction;"));
    Ok(())
}

#[test]
fn preview_rules_emits_jsonl_proposals_without_modifying_files() -> Result<()> {
    let temp = TempDir::new()?;
    let roots = fixture_roots(&temp)?;
    let rules = write_rules(
        &temp,
        r#"
        rule("trash claude basic", ({ session }) => {
          return session.agent === "claude" && session.id === "c0d1e2f3-a4b5-4c6d-8e7f-9a0b1c2d3e4f"
            ? trash("matched fixture")
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
        trash: None,
    };
    let mut events = Vec::new();

    let report = run_rules_with_progress(
        &session_roots,
        &RulesOptions {
            rules_path: rules,
            mode: RulesMode::Preview,
            json: true,
            scope: Scope::Global,
            filters: SearchFilters::default(),
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
}

fn fixture_roots(temp: &TempDir) -> Result<FixtureRoots> {
    let claude_session = copy_fixture(
        temp,
        "tests/fixtures/sessions/claude/basic_session.jsonl",
        ".claude/projects/-Users-testuser-projects-myapp/basic_session.jsonl",
    )?;
    copy_fixture(
        temp,
        "tests/fixtures/sessions/codex/new_format.jsonl",
        ".codex/sessions/2025/12/10/rollout-new.jsonl",
    )?;

    Ok(FixtureRoots {
        claude_projects: temp.path().join(".claude/projects"),
        codex_sessions: temp.path().join(".codex/sessions"),
        claude_session,
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
