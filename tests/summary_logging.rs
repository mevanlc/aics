use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use aics::logging::{self, LoggingMode};
use aics::summary::worker::{SummaryCommand, SummaryEvent, SummaryWorker};
use aics::summary::SummarizeBackend;
use anyhow::{bail, Result};
use tempfile::TempDir;

#[test]
fn summary_failure_is_flushed_even_when_rust_log_is_off() -> Result<()> {
    let temp = TempDir::new()?;
    std::env::set_var("AICS_CONFIG_ROOT", temp.path().join("config"));
    std::env::set_var("RUST_LOG", "off");
    let first_date = chrono::Utc::now().date_naive();
    let logging = logging::init(LoggingMode::Interactive)?;
    let paths = logging.paths.clone().expect("built-in managed paths");

    let missing_session = temp.path().join("missing-session.jsonl");
    let worker = SummaryWorker::spawn()?;
    worker.send(SummaryCommand {
        jsonl_path: missing_session.clone(),
        trash_paths: None,
        source: None,
        backend: SummarizeBackend::Custom,
        command_template: "unused".to_owned(),
        prompt_template: "unused".to_owned(),
        claude_command: "claude".to_owned(),
        claude_args: String::new(),
        codex_command: "codex".to_owned(),
        codex_args: String::new(),
        antigravity_command: "agy".to_owned(),
        antigravity_args: String::new(),
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

    drop(worker);
    drop(logging);
    let dates = [first_date, chrono::Utc::now().date_naive()]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    let mut summary = String::new();
    for date in dates {
        let summary_path = paths.summary_errors_for_date(date);
        if summary_path.exists() {
            summary.push_str(&fs::read_to_string(summary_path)?);
        }
        let main_path = paths.main_for_date(date);
        if main_path.exists() {
            assert_eq!(fs::read_to_string(main_path)?, "");
        }
    }
    assert!(summary.contains(&format!("session={}", missing_session.display())));
    assert!(summary.contains("error=jsonl does not exist"));
    assert!(summary.ends_with('\n'));
    assert!(!summary.ends_with("\n\n"));

    Ok(())
}
