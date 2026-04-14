//! Background worker that runs the user's summarizer CLI.
//!
//! Architecturally this is a small sibling of `SearchWorker`: one thread
//! owns the channel, commands are processed serially, and the TUI main
//! loop drains events via `try_recv`.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use log::{debug, warn};

use crate::summary::sidecar::{sidecar_path, SummarySidecar};
use crate::summary::staleness::fingerprint;
use crate::summary::template::expand;
use crate::summary::SummarizeBackend;

/// A request to summarize a specific session file.
#[derive(Debug, Clone)]
pub struct SummaryCommand {
    pub jsonl_path: PathBuf,
    pub backend: SummarizeBackend,
    /// Shell template to execute. For built-in backends, caller should pass
    /// `backend.builtin_template()`; for `Custom`, pass the user's command.
    pub command_template: String,
    /// Prompt template. Can reference `{{jsonl_path}}`.
    pub prompt_template: String,
    /// Value to substitute for `{{claude_command}}`.
    pub claude_command: String,
    /// Value to substitute for `{{codex_command}}`.
    pub codex_command: String,
}

/// Lifecycle events emitted by the worker.
#[derive(Debug, Clone)]
pub enum SummaryEvent {
    Started {
        path: PathBuf,
    },
    Completed {
        path: PathBuf,
        sidecar_path: PathBuf,
    },
    Failed {
        path: PathBuf,
        error: String,
    },
}

/// Short enum so callers can render a one-line status for the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummaryStatus {
    Running,
    Completed,
    Failed,
}

pub struct SummaryWorker {
    request_tx: Sender<SummaryCommand>,
    response_rx: Receiver<SummaryEvent>,
}

impl SummaryWorker {
    pub fn spawn() -> Result<Self> {
        let (request_tx, request_rx) = mpsc::channel::<SummaryCommand>();
        let (response_tx, response_rx) = mpsc::channel::<SummaryEvent>();
        thread::Builder::new()
            .name("aics-summary".to_owned())
            .spawn(move || worker_loop(request_rx, response_tx))
            .context("failed to spawn summary worker")?;
        Ok(Self {
            request_tx,
            response_rx,
        })
    }

    pub fn send(&self, command: SummaryCommand) -> Result<()> {
        self.request_tx
            .send(command)
            .map_err(|_| anyhow!("summary worker exited unexpectedly"))
    }

    pub fn try_recv(&self) -> Option<SummaryEvent> {
        self.response_rx.try_recv().ok()
    }
}

fn worker_loop(rx: Receiver<SummaryCommand>, tx: Sender<SummaryEvent>) {
    while let Ok(command) = rx.recv() {
        let path = command.jsonl_path.clone();
        let _ = tx.send(SummaryEvent::Started { path: path.clone() });
        let event = match run_one(&command) {
            Ok(sidecar_path) => SummaryEvent::Completed {
                path: path.clone(),
                sidecar_path,
            },
            Err(error) => {
                warn!("summary job failed for {}: {error:#}", path.display());
                SummaryEvent::Failed {
                    path: path.clone(),
                    error: format!("{error:#}"),
                }
            }
        };
        if tx.send(event).is_err() {
            // Receiver gone; just stop.
            break;
        }
    }
    debug!("summary worker loop exiting");
}

fn run_one(command: &SummaryCommand) -> Result<PathBuf> {
    let jsonl_path = command.jsonl_path.as_path();
    if !jsonl_path.exists() {
        bail!("jsonl does not exist: {}", jsonl_path.display());
    }

    let fp = fingerprint(jsonl_path)
        .with_context(|| format!("failed to fingerprint {}", jsonl_path.display()))?;

    let work_dir = create_work_dir()?;
    let prompt_file = work_dir.join("prompt.txt");
    let output_file = work_dir.join("output.md");

    // Expand the prompt first. Only `{{jsonl_path}}` is meaningful here.
    let jsonl_path_str = jsonl_path.to_string_lossy().into_owned();
    let jsonl_dir_str = jsonl_path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let prompt_file_str = prompt_file.to_string_lossy().into_owned();
    let output_file_str = output_file.to_string_lossy().into_owned();

    let mut prompt_vars: HashMap<&str, &str> = HashMap::new();
    prompt_vars.insert("jsonl_path", &jsonl_path_str);
    prompt_vars.insert("jsonl_dir", &jsonl_dir_str);
    let prompt_text = expand(&command.prompt_template, &prompt_vars)
        .context("failed to expand summary prompt template")?;
    fs::write(&prompt_file, &prompt_text)
        .with_context(|| format!("failed to write prompt to {}", prompt_file.display()))?;

    // Expand the shell command template.
    let mut cmd_vars: HashMap<&str, &str> = HashMap::new();
    cmd_vars.insert("jsonl_path", &jsonl_path_str);
    cmd_vars.insert("jsonl_dir", &jsonl_dir_str);
    cmd_vars.insert("prompt_file", &prompt_file_str);
    cmd_vars.insert("output_file", &output_file_str);
    cmd_vars.insert("claude_command", &command.claude_command);
    cmd_vars.insert("codex_command", &command.codex_command);
    let expanded_cmd = expand(&command.command_template, &cmd_vars)
        .context("failed to expand summary command template")?;
    debug!("summary exec: {expanded_cmd}");

    let output = shell_exec(&expanded_cmd)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code();
        let _ = fs::remove_dir_all(&work_dir);
        bail!(
            "summarizer exited with code {code:?}; stderr:\n{}",
            stderr.trim()
        );
    }

    let body = fs::read_to_string(&output_file).with_context(|| {
        format!(
            "failed to read summarizer output at {}",
            output_file.display()
        )
    })?;
    if body.trim().is_empty() {
        let _ = fs::remove_dir_all(&work_dir);
        bail!("summarizer produced empty output");
    }

    let sidecar = SummarySidecar::new(jsonl_path, &fp, command.backend, body);
    let target = sidecar_path(jsonl_path);
    sidecar.write_atomic(&target)?;

    let _ = fs::remove_dir_all(&work_dir);
    Ok(target)
}

fn create_work_dir() -> Result<PathBuf> {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("aics-summary-{pid}-{nanos}"));
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create work dir {}", dir.display()))?;
    Ok(dir)
}

#[cfg(unix)]
fn shell_exec(cmd: &str) -> Result<std::process::Output> {
    Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .context("failed to spawn shell for summarizer command")
}

#[cfg(not(unix))]
fn shell_exec(cmd: &str) -> Result<std::process::Output> {
    // Windows: try `sh` (git-bash) first, fall back to cmd.
    match Command::new("sh").arg("-c").arg(cmd).output() {
        Ok(out) => Ok(out),
        Err(_) => Command::new("cmd")
            .arg("/C")
            .arg(cmd)
            .output()
            .context("failed to spawn shell for summarizer command"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_sample_jsonl(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("sample.jsonl");
        fs::write(&path, "{\"a\":1}\n{\"b\":2}\n").unwrap();
        path
    }

    #[test]
    #[cfg(unix)]
    fn worker_produces_sidecar_with_fake_backend() {
        // A trivial "summarizer" that just copies the prompt to output.
        let template = "cat \"{{prompt_file}}\" > \"{{output_file}}\"";
        let prompt = "summary of {{jsonl_path}}";

        let dir = tempfile::tempdir().unwrap();
        let jsonl = write_sample_jsonl(dir.path());

        let worker = SummaryWorker::spawn().unwrap();
        worker
            .send(SummaryCommand {
                jsonl_path: jsonl.clone(),
                backend: SummarizeBackend::Custom,
                command_template: template.to_owned(),
                prompt_template: prompt.to_owned(),
                claude_command: "claude".to_owned(),
                codex_command: "codex".to_owned(),
            })
            .unwrap();

        // Drain up to Completed/Failed with a bounded wait.
        let start = std::time::Instant::now();
        let deadline = std::time::Duration::from_secs(5);
        let mut completion: Option<SummaryEvent> = None;
        while start.elapsed() < deadline {
            if let Some(ev) = worker.try_recv() {
                match &ev {
                    SummaryEvent::Started { .. } => {}
                    SummaryEvent::Completed { .. } | SummaryEvent::Failed { .. } => {
                        completion = Some(ev);
                        break;
                    }
                }
            } else {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        let completion = completion.expect("worker did not emit terminal event within 5s");
        match completion {
            SummaryEvent::Completed { sidecar_path, .. } => {
                let parsed = SummarySidecar::read(&sidecar_path).unwrap();
                assert_eq!(parsed.backend, SummarizeBackend::Custom);
                assert!(parsed.body.contains(&jsonl.display().to_string()));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn worker_emits_failure_when_output_empty() {
        let template = "true > \"{{output_file}}\"";
        let prompt = "ignored";
        let dir = tempfile::tempdir().unwrap();
        let jsonl = write_sample_jsonl(dir.path());

        let worker = SummaryWorker::spawn().unwrap();
        worker
            .send(SummaryCommand {
                jsonl_path: jsonl,
                backend: SummarizeBackend::Custom,
                command_template: template.to_owned(),
                prompt_template: prompt.to_owned(),
                claude_command: "claude".to_owned(),
                codex_command: "codex".to_owned(),
            })
            .unwrap();

        let start = std::time::Instant::now();
        let deadline = std::time::Duration::from_secs(5);
        let mut saw_failed = false;
        while start.elapsed() < deadline {
            if let Some(ev) = worker.try_recv() {
                if let SummaryEvent::Failed { error, .. } = ev {
                    assert!(error.contains("empty"));
                    saw_failed = true;
                    break;
                }
            } else {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        assert!(saw_failed, "expected Failed event");
    }
}
