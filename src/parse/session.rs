use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

impl std::fmt::Display for Agent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Agent {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Summary,
    ToolCall,
    ToolResult,
}

impl MessageRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::Summary => "summary",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
        }
    }
}

impl std::fmt::Display for MessageRole {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for MessageRole {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            "system" => Ok(Self::System),
            "summary" => Ok(Self::Summary),
            "tool_call" => Ok(Self::ToolCall),
            "tool_result" => Ok(Self::ToolResult),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivationType {
    Original,
    Trimmed,
    Continued,
    SubAgent,
}

impl DerivationType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Trimmed => "trimmed",
            Self::Continued => "continued",
            Self::SubAgent => "sub_agent",
        }
    }
}

impl std::fmt::Display for DerivationType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMessage {
    pub role: MessageRole,
    pub content: String,
    pub timestamp: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

/// Status of a tool call — pending until paired with output, then completed/failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Pending,
    Completed,
    Failed,
}

/// Outcome of a structured exec invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecStatus {
    Pending,
    Completed,
    Failed,
}

/// One change inside a structured patch result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchOp {
    Add,
    Update,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchFile {
    pub path: String,
    pub op: PatchOp,
    /// For `Add`/`Update`: the post-change content if available. For `Delete`: typically empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default)]
    pub additions: usize,
    #[serde(default)]
    pub deletions: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanItemStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    pub status: PlanItemStatus,
    pub step: String,
}

/// Top-of-transcript context block: model, sandbox, instructions, etc.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub originator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writable_roots: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_access: Option<bool>,
}

impl SessionInfo {
    pub fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.model_provider.is_none()
            && self.reasoning_effort.is_none()
            && self.approval_policy.is_none()
            && self.sandbox_mode.is_none()
            && self.cwd.is_none()
            && self.cli_version.is_none()
            && self.source.is_none()
            && self.originator.is_none()
            && self.instructions.is_none()
            && self.writable_roots.is_empty()
            && self.network_access.is_none()
    }
}

/// Aggregated runtime metrics across the rollout.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeMetrics {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub reasoning_output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_context_window: Option<u64>,
    #[serde(default)]
    pub tool_call_count: u64,
    #[serde(default)]
    pub tool_failure_count: u64,
    #[serde(default)]
    pub exec_count: u64,
    #[serde(default)]
    pub patch_count: u64,
    #[serde(default)]
    pub web_search_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_wall_ms: Option<u64>,
}

impl RuntimeMetrics {
    pub fn is_empty(&self) -> bool {
        self.total_tokens == 0
            && self.tool_call_count == 0
            && self.exec_count == 0
            && self.patch_count == 0
            && self.web_search_count == 0
            && self.total_wall_ms.is_none()
    }
}

/// Typed transcript cell. The renderer dispatches on this; the parser builds
/// these alongside (or, in later phases, instead of) `SessionMessage`s.
///
/// Cells are intentionally richer than `SessionMessage`: they preserve pairing
/// between calls and outputs (`Exec`, `Patch`, `WebSearch`) and capture
/// session-scoped context (`SessionInfo`, `Metrics`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionCell {
    Message {
        role: MessageRole,
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp: Option<DateTime<Utc>>,
    },
    Reasoning {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        header: Option<String>,
        body: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp: Option<DateTime<Utc>>,
    },
    ToolCall {
        tool: String,
        raw_name: String,
        summary: String,
        #[serde(default)]
        input: Value,
        status: ToolStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp: Option<DateTime<Utc>>,
    },
    ToolResult {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool: Option<String>,
        output: String,
        #[serde(default)]
        is_error: bool,
        /// Pre-formatted summary of the originating tool call (e.g. the bash
        /// command, file path, or args blurb). Used so the sticky header on
        /// long results still tells the user *what* was invoked. Populated by
        /// the parser via call_id pairing (codex) or message adjacency (claude).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_summary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp: Option<DateTime<Utc>>,
    },
    Exec {
        command: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parsed_summary: Option<String>,
        stdout: String,
        stderr: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        status: ExecStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp: Option<DateTime<Utc>>,
    },
    Patch {
        files: Vec<PatchFile>,
        #[serde(default)]
        success: bool,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        stdout: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        stderr: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp: Option<DateTime<Utc>>,
    },
    WebSearch {
        query: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        queries: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp: Option<DateTime<Utc>>,
    },
    Plan {
        items: Vec<PlanItem>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp: Option<DateTime<Utc>>,
    },
    SessionInfo(SessionInfo),
    Metrics(RuntimeMetrics),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub session_id: String,
    pub agent: Agent,
    pub project: String,
    pub branch: Option<String>,
    pub cwd: Option<String>,
    pub created: Option<DateTime<Utc>>,
    pub modified: Option<DateTime<Utc>>,
    pub modified_ts: u64,
    pub lines: usize,
    pub file_path: PathBuf,
    pub first_msg_role: Option<MessageRole>,
    pub first_msg_content: String,
    pub last_msg_role: Option<MessageRole>,
    pub last_msg_content: String,
    pub first_user_msg_content: String,
    pub derivation_type: DerivationType,
    pub is_sidechain: bool,
    pub custom_title: Option<String>,
    pub messages: Vec<SessionMessage>,
    pub content: String,
    /// Typed transcript cells. Populated by parsers (Phase 0+); renderers
    /// dispatch on these and fall back to `messages` when empty.
    #[serde(default)]
    pub cells: Vec<SessionCell>,
    /// Session-scoped context (model, sandbox, instructions, etc.).
    /// Rendered as a header block above the transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_info: Option<SessionInfo>,
}

impl Session {
    pub fn has_resume_preview(&self) -> bool {
        !self.first_user_msg_content.trim().is_empty()
    }
}

pub fn is_project_docs_autodump(role: MessageRole, content: &str) -> bool {
    if !matches!(role, MessageRole::User | MessageRole::System) {
        return false;
    }

    strip_project_docs_autodump_preamble(content).is_some()
}

pub fn strip_project_docs_autodump_preamble(content: &str) -> Option<&str> {
    let trimmed = content.trim_start();
    strip_project_docs_header_line(trimmed).or_else(|| strip_bare_instructions_block(trimmed))
}

fn strip_project_docs_header_line(text: &str) -> Option<&str> {
    let (line, rest) = match text.split_once('\n') {
        Some((line, rest)) => (line.trim_end(), rest),
        None => (text.trim_end(), ""),
    };

    if is_project_docs_header_line(line) {
        Some(rest)
    } else {
        None
    }
}

fn is_project_docs_header_line(line: &str) -> bool {
    [
        "AGENTS.md instructions",
        "# AGENTS.md instructions",
        "CLAUDE.md instructions",
        "# CLAUDE.md instructions",
    ]
    .into_iter()
    .any(|header| line == header || line.starts_with(&format!("{header} for ")))
}

fn strip_bare_instructions_block(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("<INSTRUCTIONS>")?;
    let end = rest.find("</INSTRUCTIONS>")?;
    Some(&rest[end + "</INSTRUCTIONS>".len()..])
}

pub fn parse_timestamp_str(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

pub fn modified_ts(timestamp: Option<DateTime<Utc>>) -> u64 {
    timestamp
        .map(|value| value.timestamp())
        .unwrap_or_default()
        .max(0) as u64
}

pub fn metadata_modified(path: &Path) -> Option<DateTime<Utc>> {
    std::fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .map(DateTime::<Utc>::from)
}

pub fn metadata_created(path: &Path) -> Option<DateTime<Utc>> {
    std::fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.created().ok().or_else(|| metadata.modified().ok()))
        .map(DateTime::<Utc>::from)
}

pub fn fallback_session_id(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.trim().is_empty())
        .unwrap_or("unknown-session")
        .to_owned()
}

pub fn default_project_for_cwd(cwd: Option<&str>) -> String {
    cwd.unwrap_or("unknown-project").to_owned()
}

pub fn infer_derivation_type(path: &Path, is_sidechain: bool) -> DerivationType {
    if is_sidechain
        || path
            .components()
            .any(|component| component.as_os_str() == "subagents")
    {
        return DerivationType::SubAgent;
    }

    let normalized = path.to_string_lossy().to_ascii_lowercase();
    if normalized.contains("trimmed") {
        DerivationType::Trimmed
    } else if normalized.contains("continued") || normalized.contains("rollover") {
        DerivationType::Continued
    } else {
        DerivationType::Original
    }
}

pub fn decode_claude_project_from_path(path: &Path) -> Option<String> {
    let components = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    let projects_index = components
        .iter()
        .position(|component| component == "projects")?;
    decode_claude_project_dir(components.get(projects_index + 1)?)
}

pub fn decode_claude_project_dir(encoded: &str) -> Option<String> {
    let trimmed = encoded.trim();
    if trimmed.is_empty() {
        return None;
    }

    let parts = trimmed
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return None;
    }

    if trimmed.starts_with('-') {
        return Some(normalize_session_path(&format!("/{}", parts.join("/"))));
    }

    let drive = parts.first()?;
    if drive.len() == 1
        && drive
            .chars()
            .all(|character| character.is_ascii_alphabetic())
    {
        let suffix = parts.get(1..).unwrap_or(&[]).join("\\");
        if suffix.is_empty() {
            return Some(format!("{}:\\", drive.to_ascii_uppercase()));
        }

        return Some(format!("{}:\\{}", drive.to_ascii_uppercase(), suffix));
    }

    None
}

pub fn normalize_session_path(path: &str) -> String {
    normalize_android_app_data_path(path).unwrap_or_else(|| path.to_owned())
}

fn normalize_android_app_data_path(path: &str) -> Option<String> {
    let stripped = path.strip_prefix("/data/data/")?;
    let components = stripped.split('/').collect::<Vec<_>>();
    let boundary = components.iter().position(|component| {
        matches!(
            *component,
            "files" | "cache" | "code_cache" | "no_backup" | "shared_prefs" | "databases"
        )
    })?;
    if boundary == 0 {
        return None;
    }

    let package = components[..boundary].join(".");
    let suffix = &components[boundary..];
    Some(format!("/data/data/{package}/{}", suffix.join("/")))
}

/// Build a baseline `Vec<SessionCell>` from a flat message list.
///
/// This is the Phase 0 fallback path: each `SessionMessage` becomes one `SessionCell`
/// of the corresponding kind. Phase 2+ may bypass this helper and emit richer cells
/// directly (paired exec/patch/web-search) — when they do, the parser can build
/// `cells` inline and skip the message-to-cell mapping entirely.
pub fn cells_from_messages(messages: &[SessionMessage]) -> Vec<SessionCell> {
    let mut cells = Vec::with_capacity(messages.len());
    // Track the most recent ToolCall's pre-formatted content so the next
    // ToolResult can echo it as its `call_summary`. Reset when an unrelated
    // role lands between the pair.
    let mut last_tool_call_summary: Option<String> = None;
    for message in messages {
        match message.role {
            MessageRole::ToolCall => {
                let summary = message.content.clone();
                last_tool_call_summary = Some(summary.clone());
                cells.push(SessionCell::ToolCall {
                    tool: message.tool_name.clone().unwrap_or_default(),
                    raw_name: message.tool_name.clone().unwrap_or_default(),
                    summary,
                    input: Value::Null,
                    status: ToolStatus::Completed,
                    timestamp: message.timestamp,
                });
            }
            MessageRole::ToolResult => {
                let call_summary = last_tool_call_summary.take();
                cells.push(SessionCell::ToolResult {
                    tool: message.tool_name.clone(),
                    output: message.content.clone(),
                    is_error: false,
                    call_summary,
                    timestamp: message.timestamp,
                });
            }
            MessageRole::User
            | MessageRole::Assistant
            | MessageRole::System
            | MessageRole::Summary => {
                last_tool_call_summary = None;
                cells.push(SessionCell::Message {
                    role: message.role,
                    content: message.content.clone(),
                    timestamp: message.timestamp,
                });
            }
        }
    }
    cells
}

pub fn first_message_fields(messages: &[SessionMessage]) -> (Option<MessageRole>, String) {
    messages
        .first()
        .map(|message| (Some(message.role), message.content.clone()))
        .unwrap_or((None, String::new()))
}

pub fn last_message_fields(messages: &[SessionMessage]) -> (Option<MessageRole>, String) {
    messages
        .last()
        .map(|message| (Some(message.role), message.content.clone()))
        .unwrap_or((None, String::new()))
}

pub fn first_user_message(messages: &[SessionMessage]) -> String {
    messages
        .iter()
        .find(|message| message.role == MessageRole::User)
        .map(|message| message.content.clone())
        .unwrap_or_default()
}

pub fn nonempty_trimmed(text: impl Into<String>) -> Option<String> {
    let text = text.into();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

pub fn push_unique_chunk(chunks: &mut Vec<String>, chunk: impl Into<String>) {
    let Some(chunk) = nonempty_trimmed(chunk.into()) else {
        return;
    };

    if chunks.last().is_some_and(|last| last == &chunk) {
        return;
    }

    chunks.push(chunk);
}

pub fn push_unique_message(
    messages: &mut Vec<SessionMessage>,
    role: MessageRole,
    content: impl Into<String>,
    timestamp: Option<DateTime<Utc>>,
) {
    let Some(content) = nonempty_trimmed(content.into()) else {
        return;
    };

    if messages
        .last()
        .is_some_and(|last| last.role == role && last.content == content)
    {
        return;
    }

    messages.push(SessionMessage {
        role,
        content,
        timestamp,
        tool_name: None,
    });
}

pub fn push_tool_message(
    messages: &mut Vec<SessionMessage>,
    role: MessageRole,
    tool_name: Option<String>,
    content: impl Into<String>,
    timestamp: Option<DateTime<Utc>>,
) {
    let Some(content) = nonempty_trimmed(content.into()) else {
        return;
    };

    if messages
        .last()
        .is_some_and(|last| last.role == role && last.content == content)
    {
        return;
    }

    messages.push(SessionMessage {
        role,
        content,
        timestamp,
        tool_name,
    });
}

pub fn latest_timestamp(
    current: Option<DateTime<Utc>>,
    candidate: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    match (current, candidate) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

pub fn earliest_timestamp(
    current: Option<DateTime<Utc>>,
    candidate: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    match (current, candidate) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

pub fn system_time_or_epoch(timestamp: Option<SystemTime>) -> SystemTime {
    timestamp.unwrap_or(SystemTime::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::{
        decode_claude_project_dir, is_project_docs_autodump, normalize_session_path,
        strip_project_docs_autodump_preamble, MessageRole,
    };

    #[test]
    fn decodes_termux_project_dir_to_package_path() {
        let decoded = decode_claude_project_dir("-data-data-com-termux-files-home-p-my-aics");

        assert_eq!(
            decoded.as_deref(),
            Some("/data/data/com.termux/files/home/p/my/aics")
        );
    }

    #[test]
    fn normalizes_android_app_data_paths() {
        let normalized = normalize_session_path("/data/data/com/termux/files/home/p/my/aics");

        assert_eq!(normalized, "/data/data/com.termux/files/home/p/my/aics");
    }

    #[test]
    fn detects_project_docs_autodump_headers() {
        assert!(is_project_docs_autodump(
            MessageRole::User,
            "# AGENTS.md instructions for /repo\n\n<INSTRUCTIONS>Use cargo test</INSTRUCTIONS>",
        ));
        assert!(is_project_docs_autodump(
            MessageRole::System,
            "CLAUDE.md instructions for /repo\nFollow local rules.",
        ));
        assert!(is_project_docs_autodump(
            MessageRole::User,
            "AGENTS.md instructions\n\n<INSTRUCTIONS>Use cargo test</INSTRUCTIONS>",
        ));
        assert!(is_project_docs_autodump(
            MessageRole::User,
            "<INSTRUCTIONS># Using `lat` to examine files\nPrefer lat.\n</INSTRUCTIONS>",
        ));
        assert!(!is_project_docs_autodump(
            MessageRole::User,
            "Please update AGENTS.md with the new commands.",
        ));
        assert!(!is_project_docs_autodump(
            MessageRole::Assistant,
            "# AGENTS.md instructions for /repo\nThis is quoted back to the user.",
        ));
    }

    #[test]
    fn strips_project_docs_autodump_preamble_variants() {
        assert_eq!(
            strip_project_docs_autodump_preamble(
                "AGENTS.md instructions\n\n<INSTRUCTIONS>Use cargo test.</INSTRUCTIONS>\n\nreal request",
            ),
            Some("\n<INSTRUCTIONS>Use cargo test.</INSTRUCTIONS>\n\nreal request")
        );
        assert_eq!(
            strip_project_docs_autodump_preamble(
                "<INSTRUCTIONS># Using `lat` to examine files\nPrefer lat.\n</INSTRUCTIONS>\n\nreal request",
            ),
            Some("\n\nreal request")
        );
    }
}
