//! AI-generated session summaries stored in sidecar files.
//!
//! The public surface is intentionally small: callers construct a
//! [`SummaryWorker`] once and send [`SummaryCommand`]s; completed or failed
//! jobs arrive as [`SummaryEvent`]s.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod prompt;
pub mod sidecar;
pub mod staleness;
pub mod template;
pub mod worker;

pub use sidecar::{sidecar_path, SummarySidecar};
pub use staleness::{fingerprint, Fingerprint};
pub use template::{expand, TemplateError};
pub use worker::{SummaryCommand, SummaryEvent, SummaryStatus, SummaryWorker};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AicsSummaryPreview {
    pub sidecar: SummarySidecar,
    pub fingerprint: Fingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeAutosummaryPreview {
    pub body: String,
    pub generated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SummarySources {
    pub aics_sidecar: Option<AicsSummaryPreview>,
    pub claude_autosummaries: Vec<ClaudeAutosummaryPreview>,
}

impl SummarySources {
    pub fn is_empty(&self) -> bool {
        self.aics_sidecar.is_none() && self.claude_autosummaries.is_empty()
    }

    pub fn latest_claude_autosummary(&self) -> Option<&ClaudeAutosummaryPreview> {
        self.claude_autosummaries.last()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummaryPreview {
    AicsSidecar(AicsSummaryPreview),
    ClaudeAutosummary(ClaudeAutosummaryPreview),
}

/// Which CLI the summarizer should invoke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SummarizeBackend {
    #[default]
    Claude,
    Codex,
    Custom,
}

impl SummarizeBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            SummarizeBackend::Claude => "claude",
            SummarizeBackend::Codex => "codex",
            SummarizeBackend::Custom => "custom",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(SummarizeBackend::Claude),
            "codex" => Some(SummarizeBackend::Codex),
            "custom" => Some(SummarizeBackend::Custom),
            _ => None,
        }
    }

    /// Built-in shell template for the built-in backends.
    /// `SummarizeBackend::Custom` returns `None` — callers must consult the user-provided command.
    pub fn builtin_template(self) -> Option<&'static str> {
        match self {
            SummarizeBackend::Claude => Some(CLAUDE_TEMPLATE),
            SummarizeBackend::Codex => Some(CODEX_TEMPLATE),
            SummarizeBackend::Custom => None,
        }
    }
}

const CLAUDE_TEMPLATE: &str = concat!(
    "cd \"{{jsonl_dir}}\" && ",
    "cat \"{{prompt_file}}\" | ",
    "{{claude_command}} -p --permission-mode bypassPermissions ",
    "> \"{{output_file}}\""
);

const CODEX_TEMPLATE: &str = concat!(
    "cat \"{{prompt_file}}\" | ",
    "{{codex_command}} exec --full-auto ",
    "--cd \"{{jsonl_dir}}\" --skip-git-repo-check ",
    "> \"{{output_file}}\""
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_roundtrip_strings() {
        for b in [
            SummarizeBackend::Claude,
            SummarizeBackend::Codex,
            SummarizeBackend::Custom,
        ] {
            assert_eq!(SummarizeBackend::parse(b.as_str()), Some(b));
        }
        assert_eq!(SummarizeBackend::parse("nope"), None);
    }

    #[test]
    fn builtin_templates_present_for_builtins() {
        assert!(SummarizeBackend::Claude.builtin_template().is_some());
        assert!(SummarizeBackend::Codex.builtin_template().is_some());
        assert!(SummarizeBackend::Custom.builtin_template().is_none());
    }

    #[test]
    fn default_is_claude() {
        assert_eq!(SummarizeBackend::default(), SummarizeBackend::Claude);
    }
}
