use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use log::warn;
use serde_json::Value;

use super::session::{
    cells_from_messages, earliest_timestamp, fallback_session_id, first_message_fields,
    first_user_message, infer_derivation_type, last_message_fields, latest_timestamp,
    metadata_created, metadata_modified, modified_ts, nonempty_trimmed, push_tool_message,
    push_unique_chunk, push_unique_message, Agent, MessageRole, Session,
};
use super::tool_format;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeAutosummary {
    pub body: String,
    pub timestamp: Option<DateTime<Utc>>,
}

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

        session_id = session_id.or_else(|| string_field(&value, "sessionId"));
        cwd = cwd.or_else(|| string_field(&value, "cwd"));
        branch = branch.or_else(|| string_field(&value, "gitBranch"));

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
            "system" if is_away_summary(&value) => {
                if let Some(summary) = extract_claude_summary_text(&value) {
                    push_unique_chunk(&mut summary_chunks, summary.clone());
                    push_unique_chunk(&mut content_chunks, summary);
                }
            }
            "user" | "assistant" | "system" => {
                let Some(role) = extract_message_role(&value) else {
                    continue;
                };

                let Some(message) = value.get("message") else {
                    continue;
                };

                let is_meta = value
                    .get("isMeta")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let tool_use_result = value.get("toolUseResult");

                let blocks =
                    extract_message_blocks(message.get("content"), tool_use_result, is_meta);

                for block in blocks {
                    match block {
                        MessageBlock::Text(text) => {
                            push_unique_message(&mut messages, role, text.clone(), entry_timestamp);
                            push_unique_chunk(&mut content_chunks, text);
                        }
                        MessageBlock::ToolCall { name, text } => {
                            let label = tool_format::tool_label(&name).to_owned();
                            push_tool_message(
                                &mut messages,
                                MessageRole::ToolCall,
                                Some(label),
                                text.clone(),
                                entry_timestamp,
                            );
                            push_unique_chunk(&mut content_chunks, text);
                        }
                        MessageBlock::ToolResult(text) => {
                            push_tool_message(
                                &mut messages,
                                MessageRole::ToolResult,
                                None,
                                text.clone(),
                                entry_timestamp,
                            );
                            push_unique_chunk(&mut content_chunks, text);
                        }
                    }
                }
            }
            "summary" => {
                if let Some(summary) = extract_claude_summary_text(&value) {
                    push_unique_chunk(&mut summary_chunks, summary.clone());
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

    let session_id = session_id.unwrap_or_else(|| fallback_session_id(path));
    let project = cwd.clone().unwrap_or_else(|| session_id.clone());
    let derivation_type = infer_derivation_type(path, is_sidechain);
    let (first_msg_role, first_msg_content) = first_message_fields(&messages);
    let (last_msg_role, last_msg_content) = last_message_fields(&messages);
    let cells = cells_from_messages(&messages);

    Ok(Some(Session {
        session_id,
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
        cells,
        session_info: None,
    }))
}

pub fn read_claude_autosummaries(path: impl AsRef<Path>) -> Result<Vec<ClaudeAutosummary>> {
    let path = path.as_ref();
    let file = File::open(path)
        .with_context(|| format!("failed to open Claude session {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut summaries = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                warn!(
                    "skipping unreadable Claude line in {} while reading autosummary: {error}",
                    path.display()
                );
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                warn!(
                    "skipping malformed Claude JSON in {} while reading autosummary: {error}",
                    path.display()
                );
                continue;
            }
        };

        if let Some(body) = extract_claude_summary_text(&value) {
            summaries.push(ClaudeAutosummary {
                body,
                timestamp: extract_timestamp(&value),
            });
        }
    }

    Ok(summaries)
}

pub fn read_claude_autosummary(path: impl AsRef<Path>) -> Result<Option<ClaudeAutosummary>> {
    Ok(read_claude_autosummaries(path)?.into_iter().last())
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

fn is_away_summary(value: &Value) -> bool {
    value.get("subtype").and_then(Value::as_str) == Some("away_summary")
}

fn extract_claude_summary_text(value: &Value) -> Option<String> {
    match value.get("type").and_then(Value::as_str) {
        Some("summary") => value.get("summary").and_then(Value::as_str),
        Some("system") if is_away_summary(value) => value.get("content").and_then(Value::as_str),
        _ => None,
    }
    .map(normalize_claude_text)
    .and_then(nonempty_trimmed)
}

enum MessageBlock {
    Text(String),
    ToolCall { name: String, text: String },
    ToolResult(String),
}

fn extract_message_blocks(
    content: Option<&Value>,
    tool_use_result: Option<&Value>,
    is_meta: bool,
) -> Vec<MessageBlock> {
    let mut blocks = Vec::new();
    let mut text_chunks = Vec::new();

    // Helper closure: flush accumulated text chunks as a single Text block
    let flush_text = |chunks: &mut Vec<String>, blocks: &mut Vec<MessageBlock>, is_meta: bool| {
        if chunks.is_empty() {
            return;
        }
        let text = chunks.join("\n\n");
        chunks.clear();
        if is_meta && text == "exit" {
            return;
        }
        blocks.push(MessageBlock::Text(text));
    };

    match content {
        Some(Value::String(text)) => {
            let normalized = normalize_claude_text(text);
            if !normalized.trim().is_empty() {
                push_unique_chunk(&mut text_chunks, normalized);
            }
        }
        Some(Value::Array(items)) => {
            for item in items {
                let block_type = item.get("type").and_then(Value::as_str);
                match block_type {
                    Some("text") => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            push_unique_chunk(&mut text_chunks, normalize_claude_text(text));
                        }
                    }
                    Some("thinking") => {
                        if let Some(text) = item.get("thinking").and_then(Value::as_str) {
                            push_unique_chunk(&mut text_chunks, normalize_claude_text(text));
                        }
                    }
                    Some("tool_use") => {
                        // Flush any accumulated text before emitting tool block
                        flush_text(&mut text_chunks, &mut blocks, is_meta);

                        let name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("tool")
                            .to_owned();
                        let input = item.get("input").unwrap_or(&Value::Null);
                        let formatted = tool_format::format_tool_call(&name, input);
                        blocks.push(MessageBlock::ToolCall {
                            name,
                            text: formatted,
                        });
                    }
                    Some("tool_result") => {
                        // Flush any accumulated text before emitting tool block
                        flush_text(&mut text_chunks, &mut blocks, is_meta);

                        // Prefer toolUseResult.stdout when available
                        let text = tool_use_result
                            .and_then(|r| r.get("stdout"))
                            .and_then(Value::as_str)
                            .map(|s| s.trim().to_owned())
                            .filter(|s| !s.is_empty())
                            .or_else(|| item.get("content").map(tool_format::format_tool_result))
                            .unwrap_or_default();

                        if !text.is_empty() {
                            blocks.push(MessageBlock::ToolResult(text));
                        }
                    }
                    _ => {
                        // Unknown block types: try to extract text
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            push_unique_chunk(&mut text_chunks, normalize_claude_text(text));
                        } else if let Some(text) = item.get("content").and_then(extract_nested_text)
                        {
                            push_unique_chunk(&mut text_chunks, text);
                        }
                    }
                }
            }
        }
        Some(other) => {
            let text = stringify_json(other);
            if !text.trim().is_empty() {
                push_unique_chunk(&mut text_chunks, text);
            }
        }
        None => {}
    }

    // If no blocks were emitted yet and there's a toolUseResult, use its stdout
    if blocks.is_empty() && text_chunks.is_empty() {
        if let Some(stdout) = tool_use_result
            .and_then(|result| result.get("stdout"))
            .and_then(Value::as_str)
        {
            let trimmed = stdout.trim();
            if !trimmed.is_empty() {
                blocks.push(MessageBlock::ToolResult(trimmed.to_owned()));
                return blocks;
            }
        }
    }

    // Flush remaining text
    flush_text(&mut text_chunks, &mut blocks, is_meta);

    blocks
}

fn extract_nested_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(normalize_claude_text(text)),
        Value::Array(items) => {
            let mut chunks = Vec::new();
            for item in items {
                if let Some(text) = item
                    .get("text")
                    .and_then(Value::as_str)
                    .map(normalize_claude_text)
                {
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
    if is_claude_wrapper_markup(trimmed) {
        return strip_xml_tags(trimmed);
    }

    trimmed.to_owned()
}

fn is_claude_wrapper_markup(text: &str) -> bool {
    let trimmed = text.trim_start();
    if !trimmed.starts_with('<') {
        return false;
    }

    let mut saw_tag = false;
    let mut rest = trimmed;
    while let Some(start) = rest.find('<') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('>') else {
            return false;
        };
        let tag = rest[..end].trim();
        rest = &rest[end + 1..];

        if tag.is_empty() || tag.starts_with('!') || tag.starts_with('?') {
            return false;
        }

        let tag_name = tag
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or_default();
        if !is_claude_wrapper_tag(tag_name) {
            return false;
        }
        saw_tag = true;
    }

    saw_tag
}

fn is_claude_wrapper_tag(tag: &str) -> bool {
    matches!(
        tag,
        "bash-input"
            | "bash-stderr"
            | "bash-stdout"
            | "command-args"
            | "command-message"
            | "command-name"
            | "local-command-caveat"
            | "local-command-stderr"
            | "local-command-stdout"
    )
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

#[cfg(test)]
mod tests {
    use super::normalize_claude_text;

    #[test]
    fn normalizes_claude_local_command_wrappers() {
        let text = "<local-command-stdout>Bye!</local-command-stdout>";

        assert_eq!(normalize_claude_text(text), "Bye!");
    }

    #[test]
    fn preserves_markdown_newlines_when_body_contains_xml_diff() {
        let text = concat!(
            "# Git Commit - Stage All Mode\n\n",
            "## Global Instructions\n\n",
            "Identity: `Mike Clark <mevanlc@gmail.com>`\n\n",
            "# Diff\n\n",
            "```diff\n",
            "+<mxfile host=\"hand-authored\">\n",
            "+  <diagram name=\"example\">\n",
            "+  </diagram>\n",
            "+</mxfile>\n",
            "```\n\n",
            "# Instructions\n\n",
            "Keep formatting intact.\n"
        );

        let normalized = normalize_claude_text(text);

        assert_eq!(normalized, text.trim());
        assert!(normalized.contains("\n\n## Global Instructions\n\n"));
        assert!(normalized.contains("+</mxfile>\n```\n\n# Instructions"));
    }
}
