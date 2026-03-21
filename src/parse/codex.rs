use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use log::warn;
use serde_json::{Map, Value};

use super::session::{
    default_project_for_cwd, earliest_timestamp, fallback_session_id, first_message_fields,
    first_user_message, infer_derivation_type, last_message_fields, latest_timestamp,
    metadata_created, metadata_modified, modified_ts, push_unique_chunk, push_unique_message,
    Agent, MessageRole, Session,
};

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
    let project = default_project_for_cwd(cwd.as_deref());
    let derivation_type = infer_derivation_type(path, false);
    let (first_msg_role, first_msg_content) = first_message_fields(&messages);
    let (last_msg_role, last_msg_content) = last_message_fields(&messages);

    Ok(Some(Session {
        session_id: session_id.unwrap_or_else(|| fallback_session_id(path)),
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
        first_user_msg_content: first_user_message(&messages),
        derivation_type,
        is_sidechain: false,
        custom_title: None,
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
            push_tool_call_chunk(
                content_chunks,
                payload
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("function_call"),
                payload.get("arguments"),
            );
        }
        "function_call_output" => {
            if let Some(output) = payload.get("output").and_then(Value::as_str) {
                push_unique_chunk(content_chunks, output);
            }
        }
        "custom_tool_call" => {
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("custom_tool_call");
            let input = payload
                .get("input")
                .map(stringify_json)
                .filter(|value| !value.is_empty());
            let content = input
                .map(|input| format!("{name}: {input}"))
                .unwrap_or_else(|| name.to_owned());
            push_unique_chunk(content_chunks, content);
        }
        "custom_tool_call_output" => {
            if let Some(output) = payload.get("output").and_then(Value::as_str) {
                push_unique_chunk(content_chunks, output);
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

fn push_tool_call_chunk(content_chunks: &mut Vec<String>, name: &str, arguments: Option<&Value>) {
    let parsed_arguments = arguments
        .and_then(Value::as_str)
        .map(parse_embedded_json)
        .unwrap_or_default();

    let chunk = if parsed_arguments.is_empty() {
        name.to_owned()
    } else {
        format!("{name}: {parsed_arguments}")
    };
    push_unique_chunk(content_chunks, chunk);
}

fn parse_embedded_json(raw: &str) -> String {
    let parsed: Value = serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()));
    stringify_json(&parsed)
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
