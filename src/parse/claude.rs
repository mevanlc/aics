use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use log::warn;
use serde_json::Value;

use super::session::{
    decode_claude_project_from_path, default_project_for_cwd, earliest_timestamp,
    fallback_session_id, first_message_fields, first_user_message, infer_derivation_type,
    last_message_fields, latest_timestamp, metadata_created, metadata_modified, modified_ts,
    push_unique_chunk, push_unique_message, Agent, MessageRole, Session,
};

pub fn parse_claude_session_file(path: impl AsRef<Path>) -> Result<Option<Session>> {
    let path = path.as_ref();
    let file = File::open(path)
        .with_context(|| format!("failed to open Claude session {}", path.display()))?;
    let reader = BufReader::new(file);

    let mut lines = 0usize;
    let mut session_id = None::<String>;
    let mut cwd = None::<String>;
    let mut branch = None::<String>;
    let mut created = None::<DateTime<Utc>>;
    let mut modified = None::<DateTime<Utc>>;
    let mut is_sidechain = path
        .components()
        .any(|component| component.as_os_str() == "subagents");
    let mut custom_title = None::<String>;
    let mut messages = Vec::new();
    let mut content_chunks = Vec::new();
    let mut summary_chunks = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                warn!(
                    "skipping unreadable Claude line in {}: {error}",
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
                    "skipping malformed Claude JSON in {}: {error}",
                    path.display()
                );
                continue;
            }
        };

        let Some(entry_type) = value.get("type").and_then(Value::as_str) else {
            continue;
        };

        if let Some(title) = value.get("slug").and_then(Value::as_str) {
            custom_title = Some(title.to_owned());
        }

        if value
            .get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            is_sidechain = true;
        }

        let entry_timestamp = extract_timestamp(&value);
        created = earliest_timestamp(created, entry_timestamp);
        modified = latest_timestamp(
            modified,
            entry_timestamp.or_else(|| extract_snapshot_timestamp(&value)),
        );

        match entry_type {
            "user" | "assistant" | "system" => {
                session_id = session_id.or_else(|| string_field(&value, "sessionId"));
                cwd = cwd.or_else(|| string_field(&value, "cwd"));
                branch = branch.or_else(|| string_field(&value, "gitBranch"));

                let Some(role) = extract_message_role(&value) else {
                    continue;
                };

                let Some(message) = value.get("message") else {
                    continue;
                };

                let Some(text) = extract_message_text(
                    message.get("content"),
                    value.get("toolUseResult"),
                    value
                        .get("isMeta")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                ) else {
                    continue;
                };

                push_unique_message(&mut messages, role, text.clone(), entry_timestamp);
                push_unique_chunk(&mut content_chunks, text);
            }
            "summary" => {
                if let Some(summary) = value.get("summary").and_then(Value::as_str) {
                    push_unique_chunk(&mut summary_chunks, summary);
                    push_unique_chunk(&mut content_chunks, summary);
                }

                session_id = session_id
                    .or_else(|| string_field(&value, "leafUuid"))
                    .or_else(|| string_field(&value, "messageId"));
            }
            "file-history-snapshot" => {}
            _ => {}
        }
    }

    if messages.is_empty() {
        if summary_chunks.is_empty() {
            return Ok(None);
        }

        push_unique_message(
            &mut messages,
            MessageRole::Summary,
            summary_chunks.join("\n\n"),
            modified,
        );
    }

    let created = created.or_else(|| metadata_created(path));
    let modified = modified.or_else(|| metadata_modified(path)).or(created);

    let project = decode_claude_project_from_path(path)
        .unwrap_or_else(|| default_project_for_cwd(cwd.as_deref()));
    let derivation_type = infer_derivation_type(path, is_sidechain);
    let (first_msg_role, first_msg_content) = first_message_fields(&messages);
    let (last_msg_role, last_msg_content) = last_message_fields(&messages);

    Ok(Some(Session {
        session_id: session_id.unwrap_or_else(|| fallback_session_id(path)),
        agent: Agent::Claude,
        project,
        branch,
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
        is_sidechain,
        custom_title,
        content: content_chunks.join("\n\n"),
        messages,
    }))
}

fn extract_message_role(value: &Value) -> Option<MessageRole> {
    match value
        .get("message")
        .and_then(|message| message.get("role"))
        .and_then(Value::as_str)
        .or_else(|| value.get("type").and_then(Value::as_str))
    {
        Some("user") => Some(MessageRole::User),
        Some("assistant") => Some(MessageRole::Assistant),
        Some("system") => Some(MessageRole::System),
        _ => None,
    }
}

fn extract_message_text(
    content: Option<&Value>,
    tool_use_result: Option<&Value>,
    is_meta: bool,
) -> Option<String> {
    let mut chunks = Vec::new();

    match content {
        Some(Value::String(text)) => push_unique_chunk(&mut chunks, normalize_claude_text(text)),
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(text) = extract_content_block_text(item) {
                    push_unique_chunk(&mut chunks, text);
                }
            }
        }
        Some(other) => push_unique_chunk(&mut chunks, stringify_json(other)),
        None => {}
    }

    if chunks.is_empty() {
        if let Some(stdout) = tool_use_result
            .and_then(|result| result.get("stdout"))
            .and_then(Value::as_str)
        {
            push_unique_chunk(&mut chunks, stdout);
        }
    }

    let text = chunks.join("\n\n");
    if text.is_empty() {
        return None;
    }

    if is_meta && text == "exit" {
        return None;
    }

    Some(text)
}

fn extract_content_block_text(item: &Value) -> Option<String> {
    let block_type = item.get("type").and_then(Value::as_str);
    match block_type {
        Some("text") => item
            .get("text")
            .and_then(Value::as_str)
            .map(normalize_claude_text),
        Some("thinking") => item
            .get("thinking")
            .and_then(Value::as_str)
            .map(normalize_claude_text),
        Some("tool_result") => item
            .get("content")
            .and_then(extract_nested_text)
            .or_else(|| item.get("content").map(stringify_json)),
        Some("tool_use") => {
            let name = item.get("name").and_then(Value::as_str).unwrap_or("tool");
            let input = item
                .get("input")
                .map(stringify_json)
                .filter(|value| !value.is_empty());

            Some(match input {
                Some(input) => format!("Tool {name}: {input}"),
                None => format!("Tool {name}"),
            })
        }
        _ => item
            .get("text")
            .and_then(Value::as_str)
            .map(normalize_claude_text)
            .or_else(|| item.get("content").and_then(extract_nested_text))
            .or_else(|| item.get("content").map(stringify_json)),
    }
}

fn extract_nested_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(normalize_claude_text(text)),
        Value::Array(items) => {
            let mut chunks = Vec::new();
            for item in items {
                if let Some(text) = extract_content_block_text(item) {
                    push_unique_chunk(&mut chunks, text);
                }
            }
            if chunks.is_empty() {
                None
            } else {
                Some(chunks.join("\n\n"))
            }
        }
        _ => None,
    }
}

fn normalize_claude_text(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.contains('<') && trimmed.contains('>') && trimmed.contains("</") {
        return strip_xml_tags(trimmed);
    }

    trimmed.to_owned()
}

fn strip_xml_tags(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut inside_tag = false;

    for character in text.chars() {
        match character {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if !inside_tag => output.push(character),
            _ => {}
        }
    }

    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn extract_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(super::session::parse_timestamp_str)
}

fn extract_snapshot_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("snapshot")
        .and_then(|snapshot| snapshot.get("timestamp"))
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
        Value::String(text) => normalize_claude_text(text),
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}
