//! Read/write the sidecar markdown file that holds a session's AI summary.
//!
//! The sidecar lives next to the JSONL (e.g. `session.jsonl.aics-summary.md`).
//! It uses a minimal YAML-ish frontmatter so humans can read it, staleness
//! can be detected cheaply, and a legacy/external tool could even hand-write
//! one without pulling in a YAML parser.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};

use crate::summary::{Fingerprint, SummarizeBackend};

pub const SIDECAR_SUFFIX: &str = ".aics-summary.md";
pub const SIDECAR_SCHEMA: u32 = 1;

/// Derive the sidecar path for a JSONL file: appends the suffix to the full
/// filename (not replacing `.jsonl`). E.g.
/// `/p/foo.jsonl` -> `/p/foo.jsonl.aics-summary.md`.
pub fn sidecar_path(jsonl_path: &Path) -> PathBuf {
    let mut s = jsonl_path.as_os_str().to_owned();
    s.push(SIDECAR_SUFFIX);
    PathBuf::from(s)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummarySidecar {
    pub schema: u32,
    pub source_file: String,
    pub line_count: usize,
    pub last_line_sha256: String,
    pub generated_at: DateTime<Utc>,
    pub backend: SummarizeBackend,
    pub body: String,
}

impl SummarySidecar {
    pub fn new(
        jsonl_path: &Path,
        fingerprint: &Fingerprint,
        backend: SummarizeBackend,
        body: String,
    ) -> Self {
        let source_file = jsonl_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| jsonl_path.display().to_string());
        Self {
            schema: SIDECAR_SCHEMA,
            source_file,
            line_count: fingerprint.line_count,
            last_line_sha256: fingerprint.last_line_sha256.clone(),
            generated_at: Utc::now(),
            backend,
            body,
        }
    }

    /// True when the stored fingerprint still matches `current`.
    pub fn is_fresh(&self, current: &Fingerprint) -> bool {
        self.line_count == current.line_count && self.last_line_sha256 == current.last_line_sha256
    }

    pub fn serialize(&self) -> String {
        let mut out = String::new();
        out.push_str("---\n");
        out.push_str(&format!("aics_schema: {}\n", self.schema));
        out.push_str(&format!(
            "source_file: {}\n",
            yaml_scalar(&self.source_file)
        ));
        out.push_str(&format!("line_count: {}\n", self.line_count));
        out.push_str(&format!("last_line_sha256: {}\n", self.last_line_sha256));
        out.push_str(&format!(
            "generated_at: {}\n",
            self.generated_at.to_rfc3339()
        ));
        out.push_str(&format!("backend: {}\n", self.backend.as_str()));
        out.push_str("---\n");
        out.push_str(&self.body);
        if !self.body.ends_with('\n') {
            out.push('\n');
        }
        out
    }

    pub fn parse(contents: &str) -> Result<Self> {
        let rest = contents
            .strip_prefix("---\n")
            .ok_or_else(|| anyhow!("missing frontmatter"))?;
        let end = rest
            .find("\n---\n")
            .ok_or_else(|| anyhow!("unterminated frontmatter"))?;
        let frontmatter = &rest[..end];
        let body = &rest[end + "\n---\n".len()..];

        let mut schema: Option<u32> = None;
        let mut source_file: Option<String> = None;
        let mut line_count: Option<usize> = None;
        let mut last_line_sha256: Option<String> = None;
        let mut generated_at: Option<DateTime<Utc>> = None;
        let mut backend: Option<SummarizeBackend> = None;

        for line in frontmatter.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let (k, v) = line
                .split_once(':')
                .ok_or_else(|| anyhow!("bad frontmatter line: {line}"))?;
            let k = k.trim();
            let v = v.trim().trim_matches('"');
            match k {
                "aics_schema" => {
                    schema = Some(v.parse().context("aics_schema is not a number")?);
                }
                "source_file" => source_file = Some(v.to_owned()),
                "line_count" => {
                    line_count = Some(v.parse().context("line_count is not a number")?);
                }
                "last_line_sha256" => last_line_sha256 = Some(v.to_owned()),
                "generated_at" => {
                    generated_at = Some(
                        DateTime::parse_from_rfc3339(v)
                            .context("generated_at is not valid rfc3339")?
                            .with_timezone(&Utc),
                    );
                }
                "backend" => {
                    backend = Some(
                        SummarizeBackend::from_str(v)
                            .ok_or_else(|| anyhow!("unknown backend `{v}`"))?,
                    );
                }
                _ => {} // forward-compat: ignore unknown keys
            }
        }

        Ok(Self {
            schema: schema.ok_or_else(|| anyhow!("missing aics_schema"))?,
            source_file: source_file.unwrap_or_default(),
            line_count: line_count.ok_or_else(|| anyhow!("missing line_count"))?,
            last_line_sha256: last_line_sha256
                .ok_or_else(|| anyhow!("missing last_line_sha256"))?,
            generated_at: generated_at.ok_or_else(|| anyhow!("missing generated_at"))?,
            backend: backend.ok_or_else(|| anyhow!("missing backend"))?,
            body: body.to_owned(),
        })
    }

    pub fn read(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::parse(&contents)
            .with_context(|| format!("failed to parse sidecar {}", path.display()))
    }

    /// Write the sidecar atomically (write to `.tmp` then rename).
    pub fn write_atomic(&self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("sidecar path has no parent: {}", path.display()))?;
        if !parent.exists() {
            bail!(
                "sidecar parent directory does not exist: {}",
                parent.display()
            );
        }
        let contents = self.serialize();
        let tmp = tmp_path(path);
        fs::write(&tmp, &contents).with_context(|| format!("failed to write {}", tmp.display()))?;
        fs::rename(&tmp, path)
            .with_context(|| format!("failed to rename {} to {}", tmp.display(), path.display()))?;
        Ok(())
    }
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

/// Quote a scalar if it contains characters that need escaping in simple YAML.
/// For our controlled schema this keeps paths safe without a full YAML library.
fn yaml_scalar(value: &str) -> String {
    let needs_quote = value.is_empty()
        || value.starts_with(char::is_whitespace)
        || value.ends_with(char::is_whitespace)
        || value
            .chars()
            .any(|c| matches!(c, ':' | '#' | '"' | '\\' | '\n' | '\r'));
    if needs_quote {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample_fingerprint() -> Fingerprint {
        Fingerprint {
            line_count: 42,
            last_line_sha256: "deadbeef".repeat(8),
        }
    }

    #[test]
    fn sidecar_path_appends_suffix_to_full_filename() {
        let got = sidecar_path(Path::new("/p/x.jsonl"));
        assert_eq!(got, PathBuf::from("/p/x.jsonl.aics-summary.md"));
    }

    #[test]
    fn serialize_then_parse_roundtrips() {
        let sidecar = SummarySidecar::new(
            Path::new("/p/x.jsonl"),
            &sample_fingerprint(),
            SummarizeBackend::Claude,
            "# Title\n\nBody paragraph.".to_owned(),
        );
        let raw = sidecar.serialize();
        let parsed = SummarySidecar::parse(&raw).unwrap();
        assert_eq!(parsed.schema, sidecar.schema);
        assert_eq!(parsed.source_file, sidecar.source_file);
        assert_eq!(parsed.line_count, sidecar.line_count);
        assert_eq!(parsed.last_line_sha256, sidecar.last_line_sha256);
        assert_eq!(parsed.backend, sidecar.backend);
        assert_eq!(parsed.body.trim_end(), sidecar.body.trim_end());
    }

    #[test]
    fn is_fresh_tracks_fingerprint_equality() {
        let sidecar = SummarySidecar::new(
            Path::new("/p/x.jsonl"),
            &sample_fingerprint(),
            SummarizeBackend::Codex,
            "body".to_owned(),
        );
        assert!(sidecar.is_fresh(&sample_fingerprint()));
        let changed = Fingerprint {
            line_count: 43,
            ..sample_fingerprint()
        };
        assert!(!sidecar.is_fresh(&changed));
    }

    #[test]
    fn parse_rejects_missing_frontmatter() {
        let err = SummarySidecar::parse("no frontmatter here\n").unwrap_err();
        assert!(format!("{err:#}").contains("missing frontmatter"));
    }

    #[test]
    fn parse_rejects_missing_required_field() {
        let raw = "---\naics_schema: 1\nline_count: 1\nlast_line_sha256: x\ngenerated_at: 2026-04-14T10:00:00Z\n---\nbody\n";
        let err = SummarySidecar::parse(raw).unwrap_err();
        assert!(format!("{err:#}").contains("missing backend"));
    }

    #[test]
    fn write_atomic_and_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.jsonl.aics-summary.md");
        let sidecar = SummarySidecar::new(
            Path::new("/p/x.jsonl"),
            &sample_fingerprint(),
            SummarizeBackend::Custom,
            "Body with *markdown*.\n".to_owned(),
        );
        sidecar.write_atomic(&path).unwrap();
        let read = SummarySidecar::read(&path).unwrap();
        assert_eq!(read, sidecar);
    }
}
