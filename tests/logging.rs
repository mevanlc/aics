use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::Result;
use tempfile::TempDir;

#[test]
fn built_in_command_logging_keeps_stdout_clean_and_uses_stderr() -> Result<()> {
    let temp = TempDir::new()?;
    let output = Command::new(env!("CARGO_BIN_EXE_aics"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("AICS_CONFIG_ROOT", temp.path().join("config"))
        .env("RUST_LOG", "aics=invalid")
        .arg("--print-palettes")
        .output()?;

    assert!(output.status.success(), "{output:#?}");
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stdout.contains("lazygit"));
    assert!(!stdout.contains("logging warning"));
    assert!(stderr.contains("invalid RUST_LOG"), "{stderr}");
    assert!(!temp.path().join("config/logs").exists());
    Ok(())
}

#[test]
fn built_in_json_warning_obeys_rust_log_without_contaminating_stdout() -> Result<()> {
    let temp = TempDir::new()?;
    let claude = temp.path().join("claude/project");
    let codex = temp.path().join("codex");
    fs::create_dir_all(&claude)?;
    fs::create_dir_all(&codex)?;
    fs::write(claude.join("malformed.jsonl"), "this is not JSON\n")?;

    let run = |rust_log: &str, cache: &str| -> std::io::Result<std::process::Output> {
        Command::new(env!("CARGO_BIN_EXE_aics"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .env("AICS_CONFIG_ROOT", temp.path().join("config"))
            .env("AICS_CACHE_ROOT", temp.path().join(cache))
            .env("AICS_DATA_ROOT", temp.path().join("data"))
            .env("AICS_CLAUDE_PROJECTS_DIR", temp.path().join("claude"))
            .env("AICS_CODEX_SESSIONS_DIR", &codex)
            .env("AICS_ANTIGRAVITY_HOME", temp.path().join("antigravity"))
            .env("RUST_LOG", rust_log)
            .args(["--json", "--progress", "none", "-g"])
            .output()
    };

    let warn = run("warn", "cache-warn")?;
    assert!(warn.status.success(), "{warn:#?}");
    assert!(warn.stdout.is_empty(), "{warn:#?}");
    let stderr = String::from_utf8(warn.stderr)?;
    assert!(
        stderr.contains("skipping malformed Claude JSON"),
        "{stderr}"
    );

    let off = run("off", "cache-off")?;
    assert!(off.status.success(), "{off:#?}");
    assert!(off.stdout.is_empty(), "{off:#?}");
    assert!(off.stderr.is_empty(), "{off:#?}");
    Ok(())
}

#[test]
fn malformed_custom_config_warns_and_falls_back_before_installing_logger() -> Result<()> {
    let temp = TempDir::new()?;
    let config = temp.path().join("config");
    fs::create_dir_all(&config)?;
    fs::write(config.join("log4rs.yaml"), "appenders: [not valid")?;

    let output = Command::new(env!("CARGO_BIN_EXE_aics"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("AICS_CONFIG_ROOT", &config)
        .arg("--print-palettes")
        .output()?;

    assert!(output.status.success(), "{output:#?}");
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stdout.contains("lazygit"));
    assert!(stderr.contains("could not load"), "{stderr}");
    assert!(stderr.contains("using built-in logging"), "{stderr}");
    Ok(())
}

#[test]
fn simultaneous_interactive_processes_own_distinct_log_groups() -> Result<()> {
    let temp = TempDir::new()?;
    let config = temp.path().join("config");
    let claude = temp.path().join("claude");
    let codex = temp.path().join("codex");
    fs::create_dir_all(&claude)?;
    fs::create_dir_all(&codex)?;

    let first = interactive_command(&temp, &config, &claude, &codex).spawn()?;
    let second = interactive_command(&temp, &config, &claude, &codex).spawn()?;
    let first = first.wait_with_output()?;
    let second = second.wait_with_output()?;
    assert!(
        !first.status.success(),
        "a piped invocation should reject TUI mode"
    );
    assert!(
        !second.status.success(),
        "a piped invocation should reject TUI mode"
    );

    let log_dir = config.join("logs");
    let main_instances = active_instances(&log_dir, "aics-")?;
    let summary_instances = active_instances(&log_dir, "summarizer-errors-")?;
    assert_eq!(main_instances.len(), 2, "{main_instances:#?}");
    assert_eq!(summary_instances, main_instances);
    Ok(())
}

#[test]
fn copied_template_is_authoritative_and_uses_reserved_process_paths() -> Result<()> {
    let temp = TempDir::new()?;
    let config = temp.path().join("config");
    let claude = temp.path().join("claude/project");
    let codex = temp.path().join("codex");
    fs::create_dir_all(&config)?;
    fs::create_dir_all(&claude)?;
    fs::create_dir_all(&codex)?;
    fs::write(claude.join("malformed.jsonl"), "this is not JSON\n")?;
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/log4rs.yaml"),
        config.join("log4rs.yaml"),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_aics"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("AICS_CONFIG_ROOT", &config)
        .env("AICS_CACHE_ROOT", temp.path().join("cache"))
        .env("AICS_DATA_ROOT", temp.path().join("data"))
        .env("AICS_CLAUDE_PROJECTS_DIR", temp.path().join("claude"))
        .env("AICS_CODEX_SESSIONS_DIR", &codex)
        .env("AICS_ANTIGRAVITY_HOME", temp.path().join("antigravity"))
        .env("RUST_LOG", "off")
        .args(["--json", "--progress", "none", "-g"])
        .output()?;

    assert!(output.status.success(), "{output:#?}");
    assert!(output.stdout.is_empty(), "{output:#?}");
    assert!(output.stderr.is_empty(), "{output:#?}");
    let main_instances = active_instances(&config.join("logs"), "aics-")?;
    assert_eq!(main_instances.len(), 1);
    assert_eq!(
        active_instances(&config.join("logs"), "summarizer-errors-")?.len(),
        1
    );
    let instance = main_instances.into_iter().next().unwrap();
    let main = fs::read_to_string(config.join(format!("logs/aics-{instance}.log")))?;
    assert!(main.contains("skipping malformed Claude JSON"), "{main}");
    Ok(())
}

#[test]
fn unavailable_interactive_log_directory_degrades_without_panicking() -> Result<()> {
    let temp = TempDir::new()?;
    let config = temp.path().join("config-is-a-file");
    fs::write(&config, "not a directory")?;
    let claude = temp.path().join("claude");
    let codex = temp.path().join("codex");
    fs::create_dir_all(&claude)?;
    fs::create_dir_all(&codex)?;

    let output = interactive_command(&temp, &config, &claude, &codex).output()?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(!output.status.success());
    assert!(
        stderr.contains("could not create AICS config directory"),
        "{stderr}"
    );
    assert!(stderr.contains("using stderr"), "{stderr}");
    assert!(!stderr.to_lowercase().contains("panicked"), "{stderr}");
    Ok(())
}

#[test]
fn startup_reaps_only_enough_old_dead_groups_to_reach_ten() -> Result<()> {
    let temp = TempDir::new()?;
    let config = temp.path().join("config");
    let log_dir = config.join("logs");
    fs::create_dir_all(&log_dir)?;
    for day in 1..=12 {
        let instance = format!("202601{day:02}T010203.004Z-p{}", 900_000_000 + day);
        fs::write(log_dir.join(format!("aics-{instance}.log")), "old main")?;
        fs::write(
            log_dir.join(format!("summarizer-errors-{instance}.log.1")),
            "old summary",
        )?;
    }
    fs::write(log_dir.join("user-notes.txt"), "do not remove")?;

    let output = Command::new(env!("CARGO_BIN_EXE_aics"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("AICS_CONFIG_ROOT", &config)
        .arg("--print-palettes")
        .output()?;

    assert!(output.status.success(), "{output:#?}");
    assert_eq!(active_instances(&log_dir, "aics-")?.len(), 10);
    assert!(log_dir.join("user-notes.txt").exists());
    assert!(!log_dir
        .join("summarizer-errors-20260101T010203.004Z-p900000001.log.1")
        .exists());
    assert!(log_dir
        .join("summarizer-errors-20260103T010203.004Z-p900000003.log.1")
        .exists());
    Ok(())
}

fn interactive_command(temp: &TempDir, config: &Path, claude: &Path, codex: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aics"));
    command
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("AICS_CONFIG_ROOT", config)
        .env("AICS_CACHE_ROOT", temp.path().join("cache"))
        .env("AICS_DATA_ROOT", temp.path().join("data"))
        .env("AICS_CLAUDE_PROJECTS_DIR", claude)
        .env("AICS_CODEX_SESSIONS_DIR", codex)
        .env("AICS_ANTIGRAVITY_HOME", temp.path().join("antigravity"))
        .env("RUST_LOG", "off")
        .arg("--progress")
        .arg("none")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn active_instances(log_dir: &Path, prefix: &str) -> Result<BTreeSet<String>> {
    let mut instances = BTreeSet::new();
    for entry in fs::read_dir(log_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if let Some(instance) = name
            .strip_prefix(prefix)
            .and_then(|name| name.strip_suffix(".log"))
        {
            instances.insert(instance.to_owned());
        }
    }
    Ok(instances)
}
