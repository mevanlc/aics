use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use log::warn;
use serde_json::{Map, Value};

use super::session::{
    earliest_timestamp, fallback_session_id, first_message_fields, infer_derivation_type,
    last_message_fields, latest_timestamp, metadata_created, metadata_modified, modified_ts,
    push_tool_message, push_unique_chunk, push_unique_message, Agent, MessageRole, Session,
};
use super::tool_format;

pub fn parse_codex_session_file(path: impl AsRef<Path>) -> Result<Option<Session>> {
    let path = path.as_ref();
    let file = File::open(path)
        .with_context(|| format!("failed to open Codex session {}", path.display()))?;
    let reader = BufReader::new(file);

    let mut lines = 0usize;
    let mut session_id = None::<String>;
    let mut cwd = None::<String>;
    let mut created = None::<DateTime<Utc>>;
    let mut modified = None::<DateTime<Utc>>;
    let mut messages = Vec::new();
    let mut content_chunks = Vec::new();
    let mut resume_preview_first_user = None::<String>;
    let mut fallback_preview_first_user = None::<String>;

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                warn!(
                    "skipping unreadable Codex line in {}: {error}",
                    path.display()
                );
                continue;
            }
        };

        lines += 1;
        if line.trim().is_empty() {
            continue;
        }

        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                warn!(
                    "skipping malformed Codex JSON in {}: {error}",
                    path.display()
                );
                continue;
            }
        };

        if value
            .get("record_type")
            .and_then(Value::as_str)
            .is_some_and(|record_type| record_type == "state")
        {
            continue;
        }

        let top_level_timestamp = extract_timestamp(value.get("timestamp"));
        created = earliest_timestamp(created, top_level_timestamp);
        modified = latest_timestamp(modified, top_level_timestamp);

        match value.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                let payload = value.get("payload").unwrap_or(&Value::Null);
                session_id = session_id.or_else(|| string_field(payload, "id"));
                cwd = cwd.or_else(|| string_field(payload, "cwd"));
                created = earliest_timestamp(created, extract_timestamp(payload.get("timestamp")));
            }
            Some("turn_context") => {
                let payload = value.get("payload").unwrap_or(&Value::Null);
                cwd = cwd.or_else(|| string_field(payload, "cwd"));
            }
            Some("response_item") => {
                if let Some(payload) = value.get("payload") {
                    handle_response_item(
                        payload,
                        top_level_timestamp,
                        &mut messages,
                        &mut content_chunks,
                        &mut cwd,
                        &mut fallback_preview_first_user,
                    );
                }
            }
            Some("event_msg") => {
                if let Some(payload) = value.get("payload") {
                    handle_event_msg(
                        payload,
                        top_level_timestamp,
                        &mut messages,
                        &mut content_chunks,
                        &mut resume_preview_first_user,
                    );
                }
            }
            Some("message")
            | Some("reasoning")
            | Some("function_call")
            | Some("function_call_output")
            | Some("custom_tool_call")
            | Some("custom_tool_call_output") => {
                handle_response_item(
                    &value,
                    top_level_timestamp,
                    &mut messages,
                    &mut content_chunks,
                    &mut cwd,
                    &mut fallback_preview_first_user,
                );
            }
            Some(_) => {}
            None => {
                session_id = session_id.or_else(|| string_field(&value, "id"));
                created = earliest_timestamp(created, extract_timestamp(value.get("timestamp")));
            }
        }
    }

    if messages.is_empty() && content_chunks.is_empty() {
        return Ok(None);
    }

    let created = created.or_else(|| metadata_created(path));
    let modified = modified.or_else(|| metadata_modified(path)).or(created);
    let session_id = session_id.unwrap_or_else(|| fallback_session_id(path));
    let project = cwd.clone().unwrap_or_else(|| session_id.clone());
    let derivation_type = infer_derivation_type(path, false);
    let (first_msg_role, first_msg_content) = first_message_fields(&messages);
    let (last_msg_role, last_msg_content) = last_message_fields(&messages);
    let first_user_msg_content = resume_preview_first_user
        .or(fallback_preview_first_user)
        .unwrap_or_default();
    let custom_title = find_codex_thread_name(path, &session_id);

    Ok(Some(Session {
        session_id,
        agent: Agent::Codex,
        project,
        branch: None,
        cwd,
        created,
        modified,
        modified_ts: modified_ts(modified),
        lines,
        file_path: path.to_path_buf(),
        first_msg_role,
        first_msg_content,
        last_msg_role,
        last_msg_content,
        first_user_msg_content,
        derivation_type,
        is_sidechain: false,
        custom_title,
        content: content_chunks.join("\n\n"),
        messages,
    }))
}

fn handle_response_item(
    payload: &Value,
    timestamp: Option<DateTime<Utc>>,
    messages: &mut Vec<super::session::SessionMessage>,
    content_chunks: &mut Vec<String>,
    cwd: &mut Option<String>,
    fallback_preview_first_user: &mut Option<String>,
) {
    let Some(item_type) = payload.get("type").and_then(Value::as_str) else {
        return;
    };

    match item_type {
        "message" => {
            let role = payload
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("assistant");
            let text = extract_message_text(payload.get("content"));

            if let Some(text) = text {
                maybe_capture_response_item_preview(role, &text, fallback_preview_first_user);
                if !should_skip_display_message(role, &text) {
                    if let Some(display_role) = map_display_role(role) {
                        push_unique_message(messages, display_role, text.clone(), timestamp);
                    }
                }

                push_unique_chunk(content_chunks, text);
            }

            if let Some(found_cwd) = extract_cwd_from_message(payload) {
                *cwd = cwd.clone().or(Some(found_cwd));
            }
        }
        "reasoning" => {
            if let Some(summary) = extract_reasoning_summary(payload.get("summary")) {
                push_unique_chunk(content_chunks, summary);
            }
        }
        "function_call" => {
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("function_call");
            let args_value: Value = payload
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(Value::Null);
            let formatted = tool_format::format_tool_call(name, &args_value);
            let label = tool_format::tool_label(name).to_owned();
            push_tool_message(
                messages,
                MessageRole::ToolCall,
                Some(label),
                formatted.clone(),
                timestamp,
            );
            push_unique_chunk(content_chunks, formatted);
        }
        "function_call_output" => {
            if let Some(output) = payload.get("output").and_then(Value::as_str) {
                push_tool_message(messages, MessageRole::ToolResult, None, output, timestamp);
                push_unique_chunk(content_chunks, output);
            }
        }
        "custom_tool_call" => {
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("custom_tool_call");
            let input = payload.get("input").unwrap_or(&Value::Null);
            let formatted = tool_format::format_tool_call(name, input);
            let label = tool_format::tool_label(name).to_owned();
            push_tool_message(
                messages,
                MessageRole::ToolCall,
                Some(label),
                formatted.clone(),
                timestamp,
            );
            push_unique_chunk(content_chunks, formatted);
        }
        "custom_tool_call_output" => {
            if let Some(output) = payload.get("output").and_then(Value::as_str) {
                let result_value: Value = serde_json::from_str(output)
                    .unwrap_or_else(|_| Value::String(output.to_owned()));
                let formatted = tool_format::format_tool_result(&result_value);
                if !formatted.is_empty() {
                    push_tool_message(
                        messages,
                        MessageRole::ToolResult,
                        None,
                        formatted.clone(),
                        timestamp,
                    );
                    push_unique_chunk(content_chunks, formatted);
                }
            }
        }
        _ => {}
    }
}

fn handle_event_msg(
    payload: &Value,
    timestamp: Option<DateTime<Utc>>,
    messages: &mut Vec<super::session::SessionMessage>,
    content_chunks: &mut Vec<String>,
    resume_preview_first_user: &mut Option<String>,
) {
    let Some(event_type) = payload.get("type").and_then(Value::as_str) else {
        return;
    };

    match event_type {
        "user_message" => {
            let message = payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            maybe_capture_event_preview(message, resume_preview_first_user);
            if should_skip_display_message("user", message) {
                return;
            }

            push_unique_message(messages, MessageRole::User, message, timestamp);
            push_unique_chunk(content_chunks, message);
        }
        "agent_message" => {
            let message = payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            push_unique_message(messages, MessageRole::Assistant, message, timestamp);
            push_unique_chunk(content_chunks, message);
        }
        "agent_reasoning" => {
            if let Some(text) = payload.get("text").and_then(Value::as_str) {
                push_unique_chunk(content_chunks, text);
            }
        }
        _ => {}
    }
}

fn extract_message_text(content: Option<&Value>) -> Option<String> {
    let mut chunks = Vec::new();

    match content {
        Some(Value::Array(items)) => {
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("input_text") | Some("output_text") | Some("text") => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            push_unique_chunk(&mut chunks, text);
                        }
                    }
                    _ => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            push_unique_chunk(&mut chunks, text);
                        }
                    }
                }
            }
        }
        Some(Value::String(text)) => push_unique_chunk(&mut chunks, text),
        Some(other) => push_unique_chunk(&mut chunks, stringify_json(other)),
        None => {}
    }

    if chunks.is_empty() {
        None
    } else {
        Some(chunks.join("\n\n"))
    }
}

fn extract_reasoning_summary(summary: Option<&Value>) -> Option<String> {
    let mut chunks = Vec::new();
    let Some(Value::Array(items)) = summary else {
        return None;
    };

    for item in items {
        if let Some(text) = item.get("text").and_then(Value::as_str) {
            push_unique_chunk(&mut chunks, text);
        }
    }

    if chunks.is_empty() {
        None
    } else {
        Some(chunks.join("\n\n"))
    }
}

fn extract_cwd_from_message(payload: &Value) -> Option<String> {
    let text = extract_message_text(payload.get("content"))?;
    if !text.trim_start().starts_with("<environment_context>") {
        return None;
    }

    let cwd_line = text.lines().find(|line| line.contains("<cwd>"))?;
    let cwd = cwd_line
        .replace("<cwd>", "")
        .replace("</cwd>", "")
        .trim()
        .to_owned();

    if cwd.is_empty() {
        None
    } else {
        Some(cwd)
    }
}

fn should_skip_display_message(role: &str, text: &str) -> bool {
    if matches!(role, "developer" | "system") {
        return true;
    }

    let trimmed = text.trim_start();
    trimmed.starts_with("<environment_context>")
        || trimmed.starts_with("<permissions instructions>")
        || trimmed.starts_with("<collaboration_mode>")
}

fn map_display_role(role: &str) -> Option<MessageRole> {
    match role {
        "user" => Some(MessageRole::User),
        "assistant" => Some(MessageRole::Assistant),
        "system" => Some(MessageRole::System),
        _ => None,
    }
}

fn extract_timestamp(value: Option<&Value>) -> Option<DateTime<Utc>> {
    value
        .and_then(Value::as_str)
        .and_then(super::session::parse_timestamp_str)
}

fn string_field(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn stringify_json(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.trim().to_owned(),
        Value::Object(object) => compact_json_object(object),
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn compact_json_object(object: &Map<String, Value>) -> String {
    serde_json::to_string(object).unwrap_or_default()
}

const USER_MESSAGE_BEGIN: &str = "## My request for Codex:";
const SESSION_INDEX_FILE: &str = "session_index.jsonl";

#[derive(Debug, Clone, PartialEq, Eq)]
struct CachedThreadNames {
    modified: SystemTime,
    size: u64,
    names: HashMap<String, String>,
}

#[derive(Debug, serde::Deserialize)]
struct SessionIndexEntry {
    id: String,
    thread_name: String,
}

fn maybe_capture_event_preview(raw: &str, preview: &mut Option<String>) {
    if preview.is_some() || should_skip_display_message("user", raw) {
        return;
    }

    *preview = normalize_codex_user_message(raw);
}

fn maybe_capture_response_item_preview(
    role: &str,
    raw: &str,
    preview: &mut Option<String>,
) {
    if preview.is_some() || role != "user" || should_skip_display_message(role, raw) {
        return;
    }

    *preview = normalize_codex_user_message(raw);
}

fn normalize_codex_user_message(raw: &str) -> Option<String> {
    let text = match raw.find(USER_MESSAGE_BEGIN) {
        Some(index) => &raw[index + USER_MESSAGE_BEGIN.len()..],
        None => raw,
    };
    super::session::nonempty_trimmed(text)
}

fn find_codex_thread_name(path: &Path, session_id: &str) -> Option<String> {
    let index_path = codex_session_index_path(path)?;
    let names = load_thread_name_cache(&index_path).ok()?;
    names.get(session_id).cloned()
}

fn codex_session_index_path(path: &Path) -> Option<std::path::PathBuf> {
    let sessions_dir = path.ancestors().find(|ancestor| {
        ancestor
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "sessions")
    })?;
    Some(sessions_dir.parent()?.join(SESSION_INDEX_FILE))
}

fn load_thread_name_cache(index_path: &Path) -> Result<HashMap<String, String>> {
    let metadata = match std::fs::metadata(index_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to read metadata for {}", index_path.display())
            });
        }
    };
    let modified = metadata
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let size = metadata.len();
    let cache_key = index_path.to_path_buf();
    let cache = thread_name_cache();

    if let Some(cached) = cache
        .lock()
        .expect("thread name cache poisoned")
        .get(&cache_key)
        .filter(|cached| cached.modified == modified && cached.size == size)
        .cloned()
    {
        return Ok(cached.names);
    }

    let names = read_thread_names(index_path)
        .with_context(|| format!("failed to read {}", index_path.display()))?;
    cache.lock().expect("thread name cache poisoned").insert(
        cache_key,
        CachedThreadNames {
            modified,
            size,
            names: names.clone(),
        },
    );
    Ok(names)
}

fn thread_name_cache() -> &'static Mutex<HashMap<std::path::PathBuf, CachedThreadNames>> {
    static CACHE: OnceLock<Mutex<HashMap<std::path::PathBuf, CachedThreadNames>>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn read_thread_names(index_path: &Path) -> Result<HashMap<String, String>> {
    let file = File::open(index_path)?;
    let reader = BufReader::new(file);
    let mut names = HashMap::new();

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                warn!(
                    "skipping unreadable Codex session index line in {}: {error}",
                    index_path.display()
                );
                continue;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let entry: SessionIndexEntry = match serde_json::from_str(trimmed) {
            Ok(entry) => entry,
            Err(error) => {
                warn!(
                    "skipping malformed Codex session index JSON in {}: {error}",
                    index_path.display()
                );
                continue;
            }
        };

        let Some(name) = super::session::nonempty_trimmed(entry.thread_name) else {
            continue;
        };
        names.insert(entry.id, name);
    }

    Ok(names)
}
