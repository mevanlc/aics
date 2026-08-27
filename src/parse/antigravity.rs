use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use log::warn;
use serde_json::Value;

use super::search_fields::{authored_user_text, SessionSearchFields};
use super::session::{
    first_message_fields, first_user_message, last_message_fields, metadata_created,
    metadata_modified, modified_ts, parse_timestamp_str, DerivationType, ExecStatus, MessageRole,
    RuntimeMetrics, Session, SessionCell, SessionInfo, SessionLineage, SessionMessage, ToolStatus,
};
use super::tool_format;
use crate::scan::{antigravity_session_file, SessionFile};

#[derive(Debug)]
struct TranscriptRecord {
    value: Value,
    source_order: usize,
}

#[derive(Debug)]
struct PendingCall {
    cell_index: usize,
    raw_name: String,
    normalized_name: String,
    summary: String,
    is_exec: bool,
}

#[derive(Default)]
struct SessionBuilder {
    messages: Vec<SessionMessage>,
    cells: Vec<SessionCell>,
    searchable: Vec<String>,
    pending: Vec<PendingCall>,
    search_fields: SessionSearchFields,
}

impl SessionBuilder {
    fn push_message(
        &mut self,
        role: MessageRole,
        content: impl Into<String>,
        timestamp: Option<DateTime<Utc>>,
        searchable: bool,
    ) {
        let content = content.into();
        let content = content.trim();
        if content.is_empty() {
            return;
        }
        let content = content.to_owned();
        self.messages.push(SessionMessage {
            role,
            content: content.clone(),
            timestamp,
            tool_name: None,
        });
        self.cells.push(SessionCell::Message {
            role,
            content: content.clone(),
            timestamp,
        });
        match role {
            MessageRole::User => {
                if let Some(text) = authored_user_text(&content) {
                    self.search_fields.push_user(text);
                }
            }
            MessageRole::Assistant | MessageRole::Summary => {
                self.search_fields.push_agent(content.clone());
            }
            MessageRole::System | MessageRole::ToolCall | MessageRole::ToolResult => {}
        }
        if searchable {
            self.searchable.push(content);
        }
    }

    fn push_reasoning(&mut self, content: &str, timestamp: Option<DateTime<Utc>>) {
        let content = content.trim();
        if content.is_empty() {
            return;
        }
        self.cells.push(SessionCell::Reasoning {
            header: None,
            body: content.to_owned(),
            timestamp,
        });
        self.search_fields.push_agent(content);
        self.searchable.push(content.to_owned());
    }

    fn push_call(&mut self, call: ParsedToolCall, timestamp: Option<DateTime<Utc>>) {
        self.search_fields.push_tool_call_text(&call.name);
        self.search_fields.push_tool_call(&call.input);
        let raw_name = call.name;
        let normalized_name = normalize_tool_name(&raw_name);
        let summary = tool_summary(&raw_name, &call.input);
        let is_exec = normalized_name == "run_command";
        let cell_index = self.cells.len();
        if is_exec {
            let (command, cwd, parsed_summary) = exec_fields(&call.input);
            self.cells.push(SessionCell::Exec {
                command,
                cwd,
                parsed_summary,
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                duration_ms: None,
                status: ExecStatus::Pending,
                timestamp,
            });
        } else {
            self.cells.push(SessionCell::ToolCall {
                tool: tool_format::tool_label(&raw_name).to_owned(),
                raw_name: raw_name.clone(),
                summary: summary.clone(),
                input: call.input.clone(),
                status: ToolStatus::Pending,
                timestamp,
            });
        }
        self.messages.push(SessionMessage {
            role: MessageRole::ToolCall,
            content: summary.clone(),
            timestamp,
            tool_name: Some(raw_name.clone()),
        });
        self.searchable.push(summary.clone());
        if !call.input.is_null() {
            self.searchable.push(compact_json(&call.input));
        }
        self.pending.push(PendingCall {
            cell_index,
            raw_name,
            normalized_name,
            summary,
            is_exec,
        });
    }

    fn push_result(&mut self, record: &Value, timestamp: Option<DateTime<Utc>>) {
        let step_type = string_field(record, "type").unwrap_or("tool");
        let result_name = normalize_tool_name(step_type);
        let pending_index = self
            .pending
            .iter()
            .position(|call| call.normalized_name == result_name)
            .or_else(|| (!self.pending.is_empty()).then_some(0));
        if pending_index.is_none() {
            self.push_call(
                ParsedToolCall {
                    name: step_type.to_ascii_lowercase(),
                    input: Value::Null,
                },
                timestamp,
            );
        }
        let pending_index = pending_index.unwrap_or(self.pending.len().saturating_sub(1));
        let pending = self.pending.remove(pending_index);
        let output = display_value(record.get("content").unwrap_or(&Value::Null));
        self.search_fields
            .push_tool_result(record.get("content").unwrap_or(&Value::Null));
        let exit_code = record
            .get("exit_code")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok());
        let failed = record_failed(record, exit_code);
        let running = string_field(record, "status").is_some_and(|value| {
            matches!(value.to_ascii_uppercase().as_str(), "RUNNING" | "PENDING")
        });

        if pending.is_exec {
            if let Some(SessionCell::Exec {
                stdout,
                exit_code: cell_exit_code,
                status,
                ..
            }) = self.cells.get_mut(pending.cell_index)
            {
                stdout.clone_from(&output);
                *cell_exit_code = exit_code;
                *status = if running {
                    ExecStatus::Pending
                } else if failed {
                    ExecStatus::Failed
                } else {
                    ExecStatus::Completed
                };
            }
        } else {
            if let Some(SessionCell::ToolCall { status, .. }) =
                self.cells.get_mut(pending.cell_index)
            {
                *status = if running {
                    ToolStatus::Pending
                } else if failed {
                    ToolStatus::Failed
                } else {
                    ToolStatus::Completed
                };
            }
            if !output.trim().is_empty() {
                self.cells.push(SessionCell::ToolResult {
                    tool: Some(tool_format::tool_label(&pending.raw_name).to_owned()),
                    output: output.clone(),
                    is_error: failed,
                    call_summary: Some(pending.summary.clone()),
                    timestamp,
                });
            }
        }

        if !output.trim().is_empty() {
            self.messages.push(SessionMessage {
                role: MessageRole::ToolResult,
                content: output.clone(),
                timestamp,
                tool_name: Some(pending.raw_name),
            });
            self.searchable.push(output);
        }
    }
}

#[derive(Debug)]
struct ParsedToolCall {
    name: String,
    input: Value,
}

pub fn parse_antigravity_session_file(path: impl AsRef<Path>) -> Result<Option<Session>> {
    let path = path.as_ref();
    if let Some(file) = antigravity_session_file(path) {
        return parse_antigravity_session(&file);
    }
    let full = path.with_file_name("transcript_full.jsonl");
    let companions = full.is_file().then_some(full).into_iter().collect();
    let file = SessionFile {
        path: path.to_path_buf(),
        agent: super::Agent::Antigravity,
        modified: fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::UNIX_EPOCH),
        size: fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0),
        trashed: false,
        original_path: None,
        companion_paths: companions,
        source_signature: 0,
        antigravity_metadata: None,
    };
    parse_antigravity_session(&file)
}

pub fn parse_antigravity_session(file: &SessionFile) -> Result<Option<Session>> {
    let records = read_merged_records(file)?;
    if records.is_empty() {
        return Ok(None);
    }

    let mut builder = SessionBuilder::default();
    let mut created = None;
    let mut modified = None;
    let mut model = None;

    for record in &records {
        let value = &record.value;
        builder
            .search_fields
            .capture_paths(super::Agent::Antigravity, value);
        let timestamp = string_field(value, "created_at").and_then(parse_timestamp_str);
        if let Some(timestamp) = timestamp {
            created =
                Some(created.map_or(timestamp, |current: DateTime<Utc>| current.min(timestamp)));
            modified =
                Some(modified.map_or(timestamp, |current: DateTime<Utc>| current.max(timestamp)));
        }
        let source = string_field(value, "source").unwrap_or_default();
        let step_type = string_field(value, "type").unwrap_or_default();
        let content = display_value(value.get("content").unwrap_or(&Value::Null));
        let thinking = string_field(value, "thinking").unwrap_or_default();

        match step_type {
            "USER_INPUT" => {
                let (request, settings) = extract_user_request(&content);
                if model.is_none() {
                    model = settings.as_deref().and_then(model_from_settings);
                }
                builder.push_message(MessageRole::User, request, timestamp, true);
            }
            "PLANNER_RESPONSE" => {
                builder.push_reasoning(thinking, timestamp);
                builder.push_message(MessageRole::Assistant, content, timestamp, true);
                for call in parse_tool_calls(value) {
                    builder.push_call(call, timestamp);
                }
            }
            "CONVERSATION_HISTORY" => {}
            "CHECKPOINT" => {
                builder.push_message(MessageRole::Summary, content, timestamp, true);
            }
            _ if source == "MODEL" => {
                builder.push_reasoning(thinking, timestamp);
                for call in parse_tool_calls(value) {
                    builder.push_call(call, timestamp);
                }
                builder.push_result(value, timestamp);
            }
            _ => {
                builder.push_message(MessageRole::System, content, timestamp, false);
            }
        }
    }

    if builder.messages.is_empty() && builder.cells.is_empty() {
        return Ok(None);
    }

    let session_id = antigravity_session_id(&file.path);
    let metadata = file.antigravity_metadata.clone().unwrap_or_default();
    let cwd = metadata.cwd;
    if let Some(cwd) = cwd.as_deref() {
        builder.search_fields.add_dir(cwd);
    }
    for path in &metadata.workspace_dirs {
        builder.search_fields.add_dir(path);
    }
    let project = cwd.clone().unwrap_or_else(|| session_id.clone());
    let created = created.or_else(|| metadata_created(&file.path));
    let modified = modified
        .or_else(|| metadata_modified(&file.path))
        .or(created);
    let (first_msg_role, first_msg_content) = first_message_fields(&builder.messages);
    let (last_msg_role, last_msg_content) = last_message_fields(&builder.messages);
    let first_user_msg_content = {
        let parsed = first_user_message(&builder.messages);
        if parsed.is_empty() {
            metadata.preview.unwrap_or_default()
        } else {
            parsed
        }
    };
    let session_info = SessionInfo {
        model,
        cwd: cwd.clone(),
        source: Some("antigravity".to_owned()),
        originator: Some("agy".to_owned()),
        ..SessionInfo::default()
    };
    builder
        .cells
        .insert(0, SessionCell::SessionInfo(session_info.clone()));
    let metrics = runtime_metrics(&builder.cells, created, modified);
    if !metrics.is_empty() {
        builder.cells.push(SessionCell::Metrics(metrics));
    }

    Ok(Some(Session {
        session_id,
        agent: super::Agent::Antigravity,
        project,
        branch: None,
        cwd,
        created,
        modified,
        modified_ts: modified_ts(modified),
        lines: records.len(),
        file_path: file.path.clone(),
        first_msg_role,
        first_msg_content,
        last_msg_role,
        last_msg_content,
        first_user_msg_content,
        derivation_type: DerivationType::Original,
        is_sidechain: false,
        custom_title: metadata.custom_title,
        messages: builder.messages,
        content: builder.searchable.join("\n"),
        search_fields: builder.search_fields,
        cells: builder.cells,
        session_info: Some(session_info),
        lineage: SessionLineage::default(),
    }))
}

fn read_merged_records(file: &SessionFile) -> Result<Vec<TranscriptRecord>> {
    let regular = read_records(&file.path)?;
    let full_path = file.companion_paths.iter().find(|path| {
        path.file_name()
            .is_some_and(|name| name == "transcript_full.jsonl")
    });
    let full = match full_path {
        Some(path) => read_records(path)?,
        None => Vec::new(),
    };
    let mut records = if full.is_empty() {
        regular
    } else {
        let full_steps = full
            .iter()
            .filter_map(|record| step_index(&record.value))
            .collect::<HashSet<_>>();
        let mut merged = full;
        merged.extend(regular.into_iter().filter(|record| {
            step_index(&record.value).is_some_and(|step| !full_steps.contains(&step))
        }));
        merged
    };
    records.sort_by_key(|record| {
        (
            step_index(&record.value).unwrap_or(i64::MAX),
            record.source_order,
        )
    });
    Ok(records)
}

fn read_records(path: &Path) -> Result<Vec<TranscriptRecord>> {
    let input = fs::File::open(path)
        .with_context(|| format!("failed to open Antigravity transcript {}", path.display()))?;
    let mut records = Vec::new();
    for (line_index, line) in BufReader::new(input).lines().enumerate() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                warn!(
                    "skipping unreadable Antigravity transcript line {}:{}: {error}",
                    path.display(),
                    line_index + 1
                );
                continue;
            }
        };
        match serde_json::from_str::<Value>(&line) {
            Ok(value) => records.push(TranscriptRecord {
                value,
                source_order: line_index,
            }),
            Err(error) => warn!(
                "skipping malformed Antigravity transcript line {}:{}: {error}",
                path.display(),
                line_index + 1
            ),
        }
    }
    Ok(records)
}

fn parse_tool_calls(record: &Value) -> Vec<ParsedToolCall> {
    record
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|call| {
            let name = call
                .get("name")
                .or_else(|| call.get("tool_name"))
                .or_else(|| call.pointer("/function/name"))
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| string_field(record, "type").unwrap_or("tool"))
                .to_owned();
            let input = call
                .get("arguments")
                .or_else(|| call.get("args"))
                .or_else(|| call.get("input"))
                .or_else(|| call.pointer("/function/arguments"))
                .cloned()
                .map(normalize_argument_value)
                .unwrap_or(Value::Null);
            ParsedToolCall { name, input }
        })
        .collect()
}

fn normalize_argument_value(value: Value) -> Value {
    match value {
        Value::String(text) => serde_json::from_str::<Value>(&text)
            .map(normalize_argument_value)
            .unwrap_or(Value::String(text)),
        Value::Array(values) => {
            Value::Array(values.into_iter().map(normalize_argument_value).collect())
        }
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, normalize_argument_value(value)))
                .collect(),
        ),
        value => value,
    }
}

fn extract_user_request(content: &str) -> (String, Option<String>) {
    let requests = extract_tagged_blocks(content, "USER_REQUEST");
    let settings = extract_tagged_blocks(content, "USER_SETTINGS_CHANGE")
        .into_iter()
        .next();
    let request = if requests.is_empty() {
        strip_known_user_wrappers(content)
    } else {
        requests.join("\n\n")
    };
    (request, settings)
}

fn extract_tagged_blocks(content: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut output = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else {
            break;
        };
        let value = after[..end].trim();
        if !value.is_empty() {
            output.push(value.to_owned());
        }
        rest = &after[end + close.len()..];
    }
    output
}

fn strip_known_user_wrappers(content: &str) -> String {
    let mut output = content.to_owned();
    for tag in ["ADDITIONAL_METADATA", "USER_SETTINGS_CHANGE"] {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        while let Some(start) = output.find(&open) {
            let after = start + open.len();
            let Some(relative_end) = output[after..].find(&close) else {
                break;
            };
            let end = after + relative_end + close.len();
            output.replace_range(start..end, "");
        }
    }
    output.trim().to_owned()
}

fn model_from_settings(settings: &str) -> Option<String> {
    let search_from = settings
        .find(" from ")
        .map_or(0, |index| index + " from ".len());
    let relative = settings[search_from..].find(" to ")?;
    let tail = &settings[search_from + relative + " to ".len()..];
    let end = tail.find(". ").unwrap_or(tail.len());
    let model = tail[..end].trim().trim_end_matches('.').trim();
    (!model.is_empty() && !model.eq_ignore_ascii_case("none")).then(|| model.to_owned())
}

fn exec_fields(input: &Value) -> (Vec<String>, Option<String>, Option<String>) {
    let command_value = object_value_ignore_ascii_case(input, &["CommandLine", "command", "cmd"]);
    let command = match command_value {
        Some(Value::String(command)) if !command.trim().is_empty() => vec![command.clone()],
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    };
    let parsed_summary = command_value
        .and_then(Value::as_str)
        .map(|command| command.lines().next().unwrap_or(command).to_owned());
    let cwd = object_value_ignore_ascii_case(input, &["Cwd", "cwd", "workdir"])
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|cwd| !cwd.trim().is_empty());
    (command, cwd, parsed_summary)
}

fn tool_summary(name: &str, input: &Value) -> String {
    object_value_ignore_ascii_case(input, &["toolSummary", "toolAction"])
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            let formatted = tool_format::format_tool_call(name, input);
            if formatted.trim().is_empty() {
                name.to_owned()
            } else {
                formatted
            }
        })
}

fn object_value_ignore_ascii_case<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let object = value.as_object()?;
    keys.iter().find_map(|wanted| {
        object
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(wanted))
            .map(|(_, value)| value)
    })
}

fn runtime_metrics(
    cells: &[SessionCell],
    created: Option<DateTime<Utc>>,
    modified: Option<DateTime<Utc>>,
) -> RuntimeMetrics {
    let mut metrics = RuntimeMetrics::default();
    for cell in cells {
        match cell {
            SessionCell::ToolCall { status, .. } => {
                metrics.tool_call_count += 1;
                if matches!(status, ToolStatus::Failed) {
                    metrics.tool_failure_count += 1;
                }
            }
            SessionCell::Exec { status, .. } => {
                metrics.tool_call_count += 1;
                metrics.exec_count += 1;
                if matches!(status, ExecStatus::Failed) {
                    metrics.tool_failure_count += 1;
                }
            }
            _ => {}
        }
    }
    if let (Some(created), Some(modified)) = (created, modified) {
        let duration = modified.signed_duration_since(created).num_milliseconds();
        if duration > 0 {
            metrics.total_wall_ms = Some(duration as u64);
        }
    }
    metrics
}

fn antigravity_session_id(path: &Path) -> String {
    path.parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("unknown-session")
        .to_owned()
}

fn normalize_tool_name(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

fn record_failed(record: &Value, exit_code: Option<i32>) -> bool {
    exit_code.is_some_and(|code| code != 0)
        || string_field(record, "status").is_some_and(|status| {
            matches!(
                status.to_ascii_uppercase().as_str(),
                "ERROR" | "FAILED" | "CANCELLED" | "CANCELED"
            )
        })
}

fn step_index(value: &Value) -> Option<i64> {
    value.get("step_index").and_then(Value::as_i64)
}

fn string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.trim().to_owned(),
        Value::Null => String::new(),
        value => compact_json(value),
    }
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}
