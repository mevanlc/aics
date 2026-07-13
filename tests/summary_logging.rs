use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use aics::logging::{self, LoggingMode};
use aics::summary::worker::{SummaryCommand, SummaryEvent, SummaryWorker};
use aics::summary::SummarizeBackend;
use anyhow::{bail, Result};
use tempfile::TempDir;

#[test]
fn summary_failure_is_durable_even_when_rust_log_is_off() -> Result<()> {
    let temp = TempDir::new()?;
    std::env::set_var("AICS_CONFIG_ROOT", temp.path().join("config"));
    std::env::set_var("RUST_LOG", "off");
    let logging = logging::init(LoggingMode::Interactive)?;
    let paths = logging.paths.as_ref().expect("built-in managed paths");

    let missing_session = temp.path().join("missing-session.jsonl");
    let worker = SummaryWorker::spawn()?;
    worker.send(SummaryCommand {
        jsonl_path: missing_session.clone(),
        backend: SummarizeBackend::Custom,
        command_template: "unused".to_owned(),
        prompt_template: "unused".to_owned(),
        claude_command: "claude".to_owned(),
        claude_args: String::new(),
        codex_command: "codex".to_owned(),
        codex_args: String::new(),
    })?;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(SummaryEvent::Failed { path, error }) = worker.try_recv() {
            assert_eq!(path, missing_session);
            assert!(error.contains("jsonl does not exist"));
            break;
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for summary failure");
        }
        thread::sleep(Duration::from_millis(10));
    }

    let summary = fs::read_to_string(&paths.summary_errors)?;
    assert!(summary.contains(&format!("session={}", missing_session.display())));
    assert!(summary.contains("error=jsonl does not exist"));
    assert!(summary.ends_with('\n'));
    assert!(!summary.ends_with("\n\n"));
    assert_eq!(fs::read_to_string(&paths.main)?, "");
    Ok(())
}
