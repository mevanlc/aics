use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    use super::{decode_claude_project_dir, normalize_session_path};

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
}
