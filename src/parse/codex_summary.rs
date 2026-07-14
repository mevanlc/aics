use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use log::warn;

const ROLLOUT_SUMMARIES_DIR: &str = "rollout_summaries";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexAutosummary {
    pub thread_id: String,
    pub updated_at: Option<DateTime<Utc>>,
    pub rollout_path: PathBuf,
    pub body: String,
}

pub fn read_codex_autosummaries(codex_home: impl AsRef<Path>) -> Result<Vec<CodexAutosummary>> {
    let summaries_dir = codex_home
        .as_ref()
        .join("memories")
        .join(ROLLOUT_SUMMARIES_DIR);
    let entries = match fs::read_dir(&summaries_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to read Codex autosummary directory {}",
                    summaries_dir.display()
                )
            });
        }
    };

    let mut summaries = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warn!(
                    "skipping unreadable entry in Codex autosummary directory {}: {error}",
                    summaries_dir.display()
                );
                continue;
            }
        };
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
            continue;
        }

        match read_codex_autosummary_file(&path) {
            Ok(Some(summary)) => summaries.push(summary),
            Ok(None) => {}
            Err(error) => warn!(
                "skipping unreadable Codex autosummary {}: {error:#}",
                path.display()
            ),
        }
    }

    summaries.sort_by(|left, right| {
        left.updated_at
            .cmp(&right.updated_at)
            .then_with(|| left.thread_id.cmp(&right.thread_id))
    });
    Ok(summaries)
}

fn read_codex_autosummary_file(path: &Path) -> Result<Option<CodexAutosummary>> {
    let file = File::open(path)
        .with_context(|| format!("failed to open Codex autosummary {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut thread_id = None;
    let mut updated_at = None;
    let mut rollout_path = None;
    let mut body_lines = Vec::new();
    let mut reading_body = false;

    for line in reader.lines() {
        let line =
            line.with_context(|| format!("failed to read Codex autosummary {}", path.display()))?;
        if reading_body {
            body_lines.push(line);
            continue;
        }
        if line.trim().is_empty() {
            reading_body = true;
            continue;
        }

        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "thread_id" if !value.is_empty() => thread_id = Some(value.to_owned()),
            "updated_at" => {
                updated_at = DateTime::parse_from_rfc3339(value)
                    .ok()
                    .map(|timestamp| timestamp.with_timezone(&Utc));
            }
            "rollout_path" if !value.is_empty() => {
                rollout_path = Some(PathBuf::from(value));
            }
            _ => {}
        }
    }

    let Some(thread_id) = thread_id else {
        warn!(
            "skipping malformed Codex autosummary {}: missing thread_id",
            path.display()
        );
        return Ok(None);
    };
    let Some(rollout_path) = rollout_path else {
        warn!(
            "skipping malformed Codex autosummary {}: missing rollout_path",
            path.display()
        );
        return Ok(None);
    };
    let body = body_lines.join("\n").trim().to_owned();
    if body.is_empty() {
        warn!(
            "skipping malformed Codex autosummary {}: empty body",
            path.display()
        );
        return Ok(None);
    }

    Ok(Some(CodexAutosummary {
        thread_id,
        updated_at,
        rollout_path,
        body,
    }))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::read_codex_autosummaries;

    #[test]
    fn reads_rollout_summary_and_skips_malformed_neighbors() {
        let temp = TempDir::new().unwrap();
        let summaries_dir = temp.path().join("memories/rollout_summaries");
        fs::create_dir_all(&summaries_dir).unwrap();
        fs::write(
            summaries_dir.join("valid.md"),
            include_str!("../../tests/fixtures/summaries/codex/rollout_summary.md"),
        )
        .unwrap();
        fs::write(summaries_dir.join("malformed.md"), "# unrelated markdown\n").unwrap();
        fs::write(summaries_dir.join("ignore.txt"), "not a summary").unwrap();

        let summaries = read_codex_autosummaries(temp.path()).unwrap();

        assert_eq!(summaries.len(), 1);
        assert_eq!(
            summaries[0].thread_id,
            "019f5e02-d840-79b1-9ed1-6ce12c5e25f8"
        );
        assert_eq!(
            summaries[0].rollout_path,
            PathBuf::from(
                "/Users/testuser/.codex/sessions/2026/07/13/rollout-2026-07-13T18-24-32-019f5e02-d840-79b1-9ed1-6ce12c5e25f8.jsonl"
            )
        );
        assert_eq!(
            summaries[0]
                .updated_at
                .map(|timestamp| timestamp.to_rfc3339()),
            Some("2026-07-14T00:34:49+00:00".to_owned())
        );
        assert!(summaries[0]
            .body
            .starts_with("# Added Codex rollout summaries to search previews"));
        assert!(summaries[0].body.contains("## Task 1"));
    }

    #[test]
    fn missing_summary_directory_is_empty() {
        let temp = TempDir::new().unwrap();
        assert!(read_codex_autosummaries(temp.path()).unwrap().is_empty());
    }
}
