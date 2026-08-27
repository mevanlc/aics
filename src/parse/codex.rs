use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use log::warn;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::search_fields::{authored_user_text, SessionSearchFields};
use super::session::{
    earliest_timestamp, fallback_session_id, first_message_fields, infer_derivation_type,
    is_contextual_user_message_content, last_message_fields, latest_timestamp, metadata_created,
    metadata_modified, modified_ts, push_tool_message, push_unique_chunk, push_unique_message,
    Agent, CodexUserTurn, ExecStatus, MessageRole, PatchFile, PatchOp, PlanItem, PlanItemStatus,
    RuntimeMetrics, Session, SessionCell, SessionInfo, SessionLineage, ToolStatus,
    TrailingAbortedTurn,
};
use super::tool_format;

pub(crate) fn parse_codex_session_meta_lineage_file(
    path: impl AsRef<Path>,
) -> Result<(Option<String>, SessionLineage)> {
    let path = path.as_ref();
    let file = File::open(path)
        .with_context(|| format!("failed to open Codex session {}", path.display()))?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let Ok(line) = line else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        let payload = value.get("payload").unwrap_or(&Value::Null);
        return Ok((
            string_field(payload, "id"),
            SessionLineage {
                forked_from_session_id: string_field(payload, "forked_from_id"),
                history_base_session_id: payload
                    .get("history_base")
                    .and_then(|history_base| string_field(history_base, "thread_id")),
                ..SessionLineage::default()
            },
        ));
    }

    Ok((None, SessionLineage::default()))
}

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
    let mut search_fields = SessionSearchFields::default();
    let mut resume_preview_first_user = None::<String>;
    let mut fallback_preview_first_user = None::<String>;
    let mut session_info = SessionInfo::default();
    let mut cell_builder = CodexCellBuilder::default();
    let mut forked_from_session_id = None::<String>;
    let mut history_base_session_id = None::<String>;
    let mut lineage_tracker = CodexLineageTracker::default();

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
        search_fields.capture_paths(Agent::Codex, &value);

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
                if session_id.is_none() {
                    forked_from_session_id = string_field(payload, "forked_from_id");
                    history_base_session_id = payload
                        .get("history_base")
                        .and_then(|history_base| string_field(history_base, "thread_id"));
                }
                session_id = session_id.or_else(|| string_field(payload, "id"));
                cwd = cwd.or_else(|| string_field(payload, "cwd"));
                created = earliest_timestamp(created, extract_timestamp(payload.get("timestamp")));
                update_session_info_from_meta(&mut session_info, payload);
            }
            Some("turn_context") => {
                let payload = value.get("payload").unwrap_or(&Value::Null);
                cwd = cwd.or_else(|| string_field(payload, "cwd"));
                update_session_info_from_turn_context(&mut session_info, payload);
            }
            Some("response_item") => {
                if let Some(payload) = value.get("payload") {
                    lineage_tracker.capture(payload);
                    handle_response_item(
                        payload,
                        top_level_timestamp,
                        &mut messages,
                        &mut content_chunks,
                        &mut cwd,
                        &mut fallback_preview_first_user,
                        &mut search_fields,
                    );
                    cell_builder.handle_response_item(payload, top_level_timestamp);
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
                        &mut search_fields,
                    );
                    cell_builder.handle_event_msg(payload, top_level_timestamp);
                }
            }
            Some("message")
            | Some("reasoning")
            | Some("function_call")
            | Some("function_call_output")
            | Some("custom_tool_call")
            | Some("custom_tool_call_output") => {
                lineage_tracker.capture(&value);
                handle_response_item(
                    &value,
                    top_level_timestamp,
                    &mut messages,
                    &mut content_chunks,
                    &mut cwd,
                    &mut fallback_preview_first_user,
                    &mut search_fields,
                );
                cell_builder.handle_response_item(&value, top_level_timestamp);
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

    if session_info.cwd.is_none() {
        session_info.cwd = cwd.clone();
    }
    let session_info = if session_info.is_empty() {
        None
    } else {
        Some(session_info)
    };

    let mut cells = Vec::with_capacity(cell_builder.cells.len() + 1);
    if let Some(info) = session_info.clone() {
        cells.push(SessionCell::SessionInfo(info));
    }
    cells.extend(cell_builder.finish());
    let (semantic_event_ids, assistant_or_tool_event_ids, codex_user_turns, trailing_aborted_turn) =
        lineage_tracker.finish();

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
        search_fields,
        messages,
        cells,
        session_info,
        lineage: SessionLineage {
            forked_from_session_id,
            history_base_session_id,
            semantic_event_ids,
            assistant_or_tool_event_ids,
            codex_user_turns,
            trailing_aborted_turn,
            ..SessionLineage::default()
        },
    }))
}

fn response_item_identity(payload: &Value) -> Option<String> {
    let item_type = payload.get("type").and_then(Value::as_str)?;
    let id_field = match item_type {
        "message" | "reasoning" => "id",
        "function_call"
        | "function_call_output"
        | "custom_tool_call"
        | "custom_tool_call_output" => "call_id",
        _ => return None,
    };
    string_field(payload, id_field).map(|id| format!("{item_type}:{id}"))
}

#[derive(Debug, Default)]
struct CodexLineageTracker {
    semantic_event_ids: HashSet<String>,
    assistant_or_tool_event_ids: Vec<String>,
    user_turns: Vec<CodexUserTurn>,
    current_user_turn: Option<PendingCodexUserTurn>,
}

#[derive(Debug)]
struct PendingCodexUserTurn {
    user_message_line_multiset_sha256: String,
    semantic_event_ids: Vec<String>,
    last_assistant_or_tool_event_id: Option<String>,
    ended_with_abort: bool,
}

impl CodexLineageTracker {
    fn capture(&mut self, payload: &Value) {
        let Some(item_type) = payload.get("type").and_then(Value::as_str) else {
            return;
        };
        let event_id = response_item_identity(payload)
            .filter(|event_id| self.semantic_event_ids.insert(event_id.clone()));

        if let Some(user_message) = lineage_user_message(payload) {
            self.finish_current_user_turn();
            self.current_user_turn = Some(PendingCodexUserTurn {
                user_message_line_multiset_sha256: user_message_line_multiset_sha256(&user_message),
                semantic_event_ids: event_id.into_iter().collect(),
                last_assistant_or_tool_event_id: None,
                ended_with_abort: false,
            });
            return;
        }

        if is_turn_aborted_message(payload) {
            if let Some(turn) = self.current_user_turn.as_mut() {
                if let Some(event_id) = event_id {
                    turn.semantic_event_ids.push(event_id);
                }
                turn.ended_with_abort = true;
            }
            return;
        }

        let is_assistant_or_tool = is_assistant_or_tool_item(item_type, payload);
        if is_assistant_or_tool {
            if let Some(event_id) = event_id.as_ref() {
                self.assistant_or_tool_event_ids.push(event_id.clone());
            }
        }

        if let Some(turn) = self.current_user_turn.as_mut() {
            turn.ended_with_abort = false;
            if let Some(event_id) = event_id {
                turn.semantic_event_ids.push(event_id.clone());
                if is_assistant_or_tool {
                    turn.last_assistant_or_tool_event_id = Some(event_id);
                }
            }
        }
    }

    fn finish(
        mut self,
    ) -> (
        Vec<String>,
        Vec<String>,
        Vec<CodexUserTurn>,
        Option<TrailingAbortedTurn>,
    ) {
        let trailing_aborted_turn = self.current_user_turn.take().and_then(|mut turn| {
            if !turn.ended_with_abort {
                self.record_completed_user_turn(turn);
                return None;
            }

            turn.semantic_event_ids.sort_unstable();
            Some(TrailingAbortedTurn {
                user_message_line_multiset_sha256: turn.user_message_line_multiset_sha256,
                semantic_event_ids: turn.semantic_event_ids,
                had_assistant_or_tool_activity: turn.last_assistant_or_tool_event_id.is_some(),
            })
        });

        let mut semantic_event_ids = self.semantic_event_ids.into_iter().collect::<Vec<_>>();
        semantic_event_ids.sort_unstable();
        self.assistant_or_tool_event_ids.sort_unstable();

        (
            semantic_event_ids,
            self.assistant_or_tool_event_ids,
            self.user_turns,
            trailing_aborted_turn,
        )
    }

    fn finish_current_user_turn(&mut self) {
        let Some(turn) = self.current_user_turn.take() else {
            return;
        };
        if !turn.ended_with_abort {
            self.record_completed_user_turn(turn);
        }
    }

    fn record_completed_user_turn(&mut self, turn: PendingCodexUserTurn) {
        let Some(last_assistant_or_tool_event_id) = turn.last_assistant_or_tool_event_id else {
            return;
        };
        self.user_turns.push(CodexUserTurn {
            user_message_line_multiset_sha256: turn.user_message_line_multiset_sha256,
            last_assistant_or_tool_event_id,
        });
    }
}

fn lineage_user_message(payload: &Value) -> Option<String> {
    if payload.get("type").and_then(Value::as_str) != Some("message")
        || payload.get("role").and_then(Value::as_str) != Some("user")
    {
        return None;
    }

    extract_message_text(payload.get("content")).filter(|text| {
        !text.trim().is_empty()
            && !is_contextual_user_message_content(MessageRole::User, text.as_str())
    })
}

fn is_assistant_or_tool_item(item_type: &str, payload: &Value) -> bool {
    match item_type {
        "message" => payload.get("role").and_then(Value::as_str) == Some("assistant"),
        "reasoning"
        | "function_call"
        | "function_call_output"
        | "custom_tool_call"
        | "custom_tool_call_output" => true,
        _ => false,
    }
}

fn user_message_line_multiset_sha256(message: &str) -> String {
    let mut lines = message
        .lines()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    lines.sort_unstable();

    let mut hasher = Sha256::new();
    for line in lines {
        hasher.update((line.len() as u64).to_le_bytes());
        hasher.update(line.as_bytes());
    }
    let digest = hasher.finalize();
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn is_turn_aborted_message(payload: &Value) -> bool {
    if payload.get("role").and_then(Value::as_str) != Some("developer") {
        return false;
    }
    extract_message_text(payload.get("content")).is_some_and(|text| {
        let text = text.trim();
        text.starts_with("<turn_aborted>") && text.ends_with("</turn_aborted>")
    })
}

fn update_session_info_from_meta(info: &mut SessionInfo, payload: &Value) {
    if info.cwd.is_none() {
        info.cwd = string_field(payload, "cwd");
    }
    if info.cli_version.is_none() {
        info.cli_version = string_field(payload, "cli_version");
    }
    if info.source.is_none() {
        info.source = string_field(payload, "source");
    }
    if info.originator.is_none() {
        info.originator = string_field(payload, "originator");
    }
    if info.model_provider.is_none() {
        info.model_provider = string_field(payload, "model_provider");
    }
    if info.instructions.is_none() {
        // Either `instructions` (older) or `base_instructions` (newer).
        info.instructions = string_field(payload, "instructions")
            .or_else(|| string_field(payload, "base_instructions"));
    }
}

fn update_session_info_from_turn_context(info: &mut SessionInfo, payload: &Value) {
    if info.cwd.is_none() {
        info.cwd = string_field(payload, "cwd");
    }
    // `turn_context` may appear multiple times — the latest model/effort wins.
    if let Some(model) = string_field(payload, "model") {
        info.model = Some(model);
    }
    if let Some(effort) = string_field(payload, "effort") {
        info.reasoning_effort = Some(effort);
    }
    if let Some(approval) = string_field(payload, "approval_policy") {
        info.approval_policy = Some(approval);
    }
    if let Some(sandbox) = payload.get("sandbox_policy") {
        if let Some(mode) = sandbox.get("mode").and_then(Value::as_str) {
            info.sandbox_mode = Some(mode.to_owned());
        }
        if let Some(net) = sandbox.get("network_access").and_then(Value::as_bool) {
            info.network_access = Some(net);
        }
        if let Some(roots) = sandbox.get("writable_roots").and_then(Value::as_array) {
            let collected: Vec<String> = roots
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect();
            if !collected.is_empty() {
                info.writable_roots = collected;
            }
        }
    }
}

fn handle_response_item(
    payload: &Value,
    timestamp: Option<DateTime<Utc>>,
    messages: &mut Vec<super::session::SessionMessage>,
    content_chunks: &mut Vec<String>,
    cwd: &mut Option<String>,
    fallback_preview_first_user: &mut Option<String>,
    search_fields: &mut SessionSearchFields,
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
                match role {
                    "user" => {
                        if let Some(text) = authored_user_text(&text) {
                            search_fields.push_user(text);
                        }
                    }
                    "assistant" => search_fields.push_agent(text.clone()),
                    _ => {}
                }
                if !should_skip_display_message(role, &text) {
                    if let Some(display_role) = map_display_role(role) {
                        push_unique_message(messages, display_role, text.clone(), timestamp);
                    }
                }

                if should_index_message(role) {
                    push_unique_chunk(content_chunks, text);
                }
            }

            if let Some(found_cwd) = extract_cwd_from_message(payload) {
                *cwd = cwd.clone().or(Some(found_cwd));
            }
        }
        "reasoning" => {
            if let Some(summary) = extract_reasoning_summary(payload.get("summary")) {
                search_fields.push_agent(summary.clone());
                push_unique_chunk(content_chunks, summary);
            }
        }
        "function_call" => {
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("function_call");
            let args_value = payload
                .get("arguments")
                .map(parse_embedded_json)
                .unwrap_or(Value::Null);
            let formatted = tool_format::format_tool_call(name, &args_value);
            search_fields.push_tool_call_text(name);
            search_fields.push_tool_call(&args_value);
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
            if let Some(output) = payload.get("output") {
                let result = parse_embedded_json(output);
                search_fields.push_tool_result(&result);
                let output_text = output
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| stringify_json(output));
                push_tool_message(
                    messages,
                    MessageRole::ToolResult,
                    None,
                    output_text.clone(),
                    timestamp,
                );
                push_unique_chunk(content_chunks, output_text);
            }
        }
        "custom_tool_call" => {
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("custom_tool_call");
            let input = payload.get("input").unwrap_or(&Value::Null);
            search_fields.push_tool_call_text(name);
            search_fields.push_tool_call(input);
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
            if let Some(output) = payload.get("output") {
                let result_value = parse_embedded_json(output);
                search_fields.push_tool_result(&result_value);
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
        "web_search_call" => {
            search_fields.push_tool_call_text("web_search");
            if let Some(action) = payload.get("action") {
                search_fields.push_tool_call(action);
            }
        }
        _ => {}
    }
}

fn parse_embedded_json(value: &Value) -> Value {
    value
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_else(|| value.clone())
}

fn handle_event_msg(
    payload: &Value,
    timestamp: Option<DateTime<Utc>>,
    messages: &mut Vec<super::session::SessionMessage>,
    content_chunks: &mut Vec<String>,
    resume_preview_first_user: &mut Option<String>,
    search_fields: &mut SessionSearchFields,
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
            if let Some(text) = authored_user_text(message) {
                search_fields.push_user(text);
            }
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
            search_fields.push_agent(message);
            push_unique_message(messages, MessageRole::Assistant, message, timestamp);
            push_unique_chunk(content_chunks, message);
        }
        "agent_reasoning" => {
            if let Some(text) = payload.get("text").and_then(Value::as_str) {
                search_fields.push_agent(text);
                push_unique_chunk(content_chunks, text);
            }
        }
        "exec_command_end" | "patch_apply_end" => {
            if let Some(command) = payload.get("command") {
                search_fields.push_tool_call(command);
            }
            for key in ["stdout", "stderr", "aggregated_output", "error"] {
                if let Some(value) = payload.get(key) {
                    search_fields.push_tool_result(value);
                }
            }
        }
        "web_search_end" => {
            search_fields.push_tool_call_text("web_search");
            if let Some(action) = payload.get("action") {
                search_fields.push_tool_call(action);
            }
            if let Some(query) = payload.get("query") {
                search_fields.push_tool_call(query);
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
    if is_internal_message_role(role) {
        return true;
    }

    let trimmed = text.trim_start();
    trimmed.starts_with("<environment_context>")
        || trimmed.starts_with("<permissions instructions>")
        || trimmed.starts_with("<collaboration_mode>")
}

fn should_index_message(role: &str) -> bool {
    !is_internal_message_role(role)
}

fn is_internal_message_role(role: &str) -> bool {
    matches!(role, "developer" | "system")
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

/// Builds a typed `Vec<SessionCell>` from a Codex rollout, pairing tool calls
/// with their outputs (and any later end-event enrichment) by `call_id`.
///
/// The pairing is single-pass: each pending call records the index in `cells`
/// of its placeholder cell, which is mutated when the matching output arrives.
/// Calls whose output never arrives are left in their pending state.
#[derive(Debug, Default)]
struct CodexCellBuilder {
    cells: Vec<SessionCell>,
    /// call_id -> cell index for `Exec` placeholders awaiting `function_call_output`
    /// or `event_msg.exec_command_end`.
    pending_exec: HashMap<String, usize>,
    /// call_id -> cell index for `Patch` placeholders.
    pending_patch: HashMap<String, usize>,
    /// call_id -> cell index for `WebSearch` placeholders.
    pending_search: HashMap<String, usize>,
    /// call_id -> cell index for generic `ToolCall` placeholders, used to flip
    /// status when the matching output arrives.
    pending_tool: HashMap<String, usize>,
    /// call_ids whose exec has been finalized via `event_msg.exec_command_end`.
    /// Codex emits a trailing `function_call_output` for the same call_id
    /// carrying a Codex-formatted text wrapper of the same stdout that
    /// `exec_command_end.aggregated_output` already gave us. We track those
    /// finalized call_ids here so the `function_call_output` handler can
    /// suppress the redundant cell instead of double-rendering.
    completed_exec: HashSet<String>,
    /// Latest non-null `info.total_token_usage` payload seen.
    latest_token_info: Option<Value>,
    /// Earliest event timestamp (used to compute wall time).
    first_event_ts: Option<DateTime<Utc>>,
    /// Latest event timestamp.
    last_event_ts: Option<DateTime<Utc>>,
}

impl CodexCellBuilder {
    fn finish(mut self) -> Vec<SessionCell> {
        let metrics = self.compute_metrics();
        if !metrics.is_empty() {
            self.cells.push(SessionCell::Metrics(metrics));
        }
        self.cells
    }

    fn compute_metrics(&self) -> RuntimeMetrics {
        let mut metrics = RuntimeMetrics::default();
        if let Some(info) = self.latest_token_info.as_ref() {
            if let Some(total) = info.get("total_token_usage") {
                metrics.input_tokens = total
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                metrics.cached_input_tokens = total
                    .get("cached_input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                metrics.output_tokens = total
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                metrics.reasoning_output_tokens = total
                    .get("reasoning_output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                metrics.total_tokens = total
                    .get("total_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
            }
            metrics.model_context_window = info.get("model_context_window").and_then(Value::as_u64);
        }
        for cell in &self.cells {
            match cell {
                SessionCell::ToolCall { status, .. } => {
                    metrics.tool_call_count += 1;
                    if matches!(status, ToolStatus::Failed) {
                        metrics.tool_failure_count += 1;
                    }
                }
                SessionCell::Exec { status, .. } => {
                    metrics.exec_count += 1;
                    metrics.tool_call_count += 1;
                    if matches!(status, ExecStatus::Failed) {
                        metrics.tool_failure_count += 1;
                    }
                }
                SessionCell::Patch { success, .. } => {
                    metrics.patch_count += 1;
                    metrics.tool_call_count += 1;
                    if !success {
                        metrics.tool_failure_count += 1;
                    }
                }
                SessionCell::WebSearch { .. } => {
                    metrics.web_search_count += 1;
                    metrics.tool_call_count += 1;
                }
                _ => {}
            }
        }
        if let (Some(start), Some(end)) = (self.first_event_ts, self.last_event_ts) {
            let delta = end.signed_duration_since(start).num_milliseconds();
            if delta > 0 {
                metrics.total_wall_ms = Some(delta as u64);
            }
        }
        metrics
    }

    fn handle_response_item(&mut self, payload: &Value, timestamp: Option<DateTime<Utc>>) {
        if let Some(ts) = timestamp {
            self.first_event_ts = Some(self.first_event_ts.map_or(ts, |first| first.min(ts)));
            self.last_event_ts = Some(self.last_event_ts.map_or(ts, |last| last.max(ts)));
        }

        let Some(item_type) = payload.get("type").and_then(Value::as_str) else {
            return;
        };

        match item_type {
            "message" => self.push_message_cell(payload, timestamp),
            "reasoning" => self.push_reasoning_cell(payload, timestamp),
            "function_call" => self.push_function_call_cell(payload, timestamp),
            "function_call_output" => self.handle_function_call_output(payload, timestamp),
            "custom_tool_call" => self.push_custom_tool_call_cell(payload, timestamp),
            "custom_tool_call_output" => self.handle_custom_tool_call_output(payload, timestamp),
            "web_search_call" => self.push_web_search_cell(payload, timestamp),
            _ => {}
        }
    }

    fn handle_event_msg(&mut self, payload: &Value, timestamp: Option<DateTime<Utc>>) {
        if let Some(ts) = timestamp {
            self.first_event_ts = Some(self.first_event_ts.map_or(ts, |first| first.min(ts)));
            self.last_event_ts = Some(self.last_event_ts.map_or(ts, |last| last.max(ts)));
        }

        let Some(event_type) = payload.get("type").and_then(Value::as_str) else {
            return;
        };

        match event_type {
            "user_message" => {
                let raw = payload
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if should_skip_display_message("user", raw) {
                    return;
                }
                let normalized =
                    normalize_codex_user_message(raw).unwrap_or_else(|| raw.to_owned());
                self.push_unique_message_cell(MessageRole::User, normalized, timestamp);
            }
            "agent_message" => {
                let message = payload
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if message.trim().is_empty() {
                    return;
                }
                self.push_unique_message_cell(
                    MessageRole::Assistant,
                    message.to_owned(),
                    timestamp,
                );
            }
            "agent_reasoning" => {
                if let Some(text) = payload.get("text").and_then(Value::as_str) {
                    self.push_reasoning_text(text, timestamp);
                }
            }
            "exec_command_end" => self.handle_exec_command_end(payload),
            "patch_apply_end" => self.handle_patch_apply_end(payload),
            "web_search_end" => self.handle_web_search_end(payload),
            "token_count" => {
                if let Some(info) = payload.get("info") {
                    if !info.is_null() {
                        self.latest_token_info = Some(info.clone());
                    }
                }
            }
            _ => {}
        }
    }

    fn push_message_cell(&mut self, payload: &Value, timestamp: Option<DateTime<Utc>>) {
        let role = payload
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("assistant");
        let Some(text) = extract_message_text(payload.get("content")) else {
            return;
        };
        if should_skip_display_message(role, &text) {
            return;
        }
        let Some(display_role) = map_display_role(role) else {
            return;
        };
        self.push_unique_message_cell(display_role, text, timestamp);
    }

    fn push_unique_message_cell(
        &mut self,
        role: MessageRole,
        content: String,
        timestamp: Option<DateTime<Utc>>,
    ) {
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return;
        }
        // De-dup against last cell if it's the same role+content (a Codex rollout
        // may carry the same user/agent message both as a `response_item.message`
        // and as an `event_msg.user_message`/`agent_message`).
        if let Some(SessionCell::Message {
            role: prev_role,
            content: prev_content,
            ..
        }) = self.cells.last()
        {
            if *prev_role == role && prev_content == trimmed {
                return;
            }
        }
        self.cells.push(SessionCell::Message {
            role,
            content: trimmed.to_owned(),
            timestamp,
        });
    }

    fn push_reasoning_cell(&mut self, payload: &Value, timestamp: Option<DateTime<Utc>>) {
        // `summary[*].text` is the plaintext form. `encrypted_content` is opaque.
        let Some(text) = extract_reasoning_summary(payload.get("summary")) else {
            return;
        };
        self.push_reasoning_text(&text, timestamp);
    }

    fn push_reasoning_text(&mut self, raw: &str, timestamp: Option<DateTime<Utc>>) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return;
        }
        // De-dup: response_item.reasoning and event_msg.agent_reasoning often
        // carry the same text in the same turn.
        if let Some(SessionCell::Reasoning { body, .. }) = self.cells.last() {
            if body == trimmed {
                return;
            }
        }
        let (header, body) = split_reasoning_header_body(trimmed);
        self.cells.push(SessionCell::Reasoning {
            header,
            body,
            timestamp,
        });
    }

    fn push_function_call_cell(&mut self, payload: &Value, timestamp: Option<DateTime<Utc>>) {
        let raw_name = payload
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("function_call")
            .to_owned();
        let label = tool_format::tool_label(&raw_name).to_owned();
        let call_id = string_field(payload, "call_id");
        let args_value: Value = payload
            .get("arguments")
            .and_then(Value::as_str)
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);

        if label == "bash" {
            // Build an Exec placeholder.
            let (command, exec_cwd, parsed_summary) = parse_exec_arguments(&raw_name, &args_value);
            let cell = SessionCell::Exec {
                command,
                cwd: exec_cwd,
                parsed_summary,
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                duration_ms: None,
                status: ExecStatus::Pending,
                timestamp,
            };
            let index = self.cells.len();
            self.cells.push(cell);
            if let Some(id) = call_id {
                self.pending_exec.insert(id, index);
            }
            return;
        }

        if raw_name == "update_plan" {
            if let Some(items) = parse_plan_items(&args_value) {
                self.cells.push(SessionCell::Plan { items, timestamp });
                return;
            }
            // Fall through to generic ToolCall if the shape was unexpected.
        }

        // Generic tool call.
        let summary = tool_format::format_tool_call(&raw_name, &args_value);
        let index = self.cells.len();
        self.cells.push(SessionCell::ToolCall {
            tool: label,
            raw_name,
            summary,
            input: args_value,
            status: ToolStatus::Pending,
            timestamp,
        });
        if let Some(id) = call_id {
            self.pending_tool.insert(id, index);
        }
    }

    fn handle_function_call_output(&mut self, payload: &Value, timestamp: Option<DateTime<Utc>>) {
        let Some(output) = payload.get("output").and_then(Value::as_str) else {
            return;
        };
        let call_id = string_field(payload, "call_id");

        // 1. If this matches a pending Exec, finalize from the legacy text format.
        if let Some(id) = call_id.as_ref() {
            if let Some(&index) = self.pending_exec.get(id) {
                self.update_exec_from_text_output(index, output);
                // Stay registered so a later `exec_command_end` can enrich further.
                return;
            }
        }

        // 2. If the matching exec was already finalized via
        //    `exec_command_end`, this `function_call_output` is the redundant
        //    text-wrapped duplicate. Drop it.
        if let Some(id) = call_id.as_ref() {
            if self.completed_exec.remove(id) {
                return;
            }
        }

        // 3. Otherwise, if this matches a pending generic ToolCall, finalize
        //    its status and push a paired ToolResult cell.
        let formatted_output = output.trim().to_owned();
        let mut paired_tool: Option<String> = None;
        let mut call_summary: Option<String> = None;
        if let Some(id) = call_id.as_ref() {
            if let Some(&index) = self.pending_tool.get(id) {
                if let Some(SessionCell::ToolCall {
                    tool,
                    summary,
                    status,
                    ..
                }) = self.cells.get_mut(index)
                {
                    paired_tool = Some(tool.clone());
                    if !summary.is_empty() {
                        call_summary = Some(summary.clone());
                    }
                    *status = ToolStatus::Completed;
                }
                self.pending_tool.remove(id);
            }
        }

        if !formatted_output.is_empty() {
            self.cells.push(SessionCell::ToolResult {
                tool: paired_tool,
                output: formatted_output,
                is_error: false,
                call_summary,
                timestamp,
            });
        }
    }

    fn push_custom_tool_call_cell(&mut self, payload: &Value, timestamp: Option<DateTime<Utc>>) {
        let raw_name = payload
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("custom_tool_call")
            .to_owned();
        let label = tool_format::tool_label(&raw_name).to_owned();
        let call_id = string_field(payload, "call_id");
        let input_value = payload.get("input").cloned().unwrap_or(Value::Null);

        if label == "patch" {
            let files = input_value
                .as_str()
                .map(parse_v4a_patch)
                .unwrap_or_default();
            let cell = SessionCell::Patch {
                files,
                success: false,
                stdout: String::new(),
                stderr: String::new(),
                timestamp,
            };
            let index = self.cells.len();
            self.cells.push(cell);
            if let Some(id) = call_id {
                self.pending_patch.insert(id, index);
            }
            return;
        }

        let summary = tool_format::format_tool_call(&raw_name, &input_value);
        let index = self.cells.len();
        self.cells.push(SessionCell::ToolCall {
            tool: label,
            raw_name,
            summary,
            input: input_value,
            status: ToolStatus::Pending,
            timestamp,
        });
        if let Some(id) = call_id {
            self.pending_tool.insert(id, index);
        }
    }

    fn handle_custom_tool_call_output(
        &mut self,
        payload: &Value,
        timestamp: Option<DateTime<Utc>>,
    ) {
        let Some(output) = payload.get("output").and_then(Value::as_str) else {
            return;
        };
        let call_id = string_field(payload, "call_id");
        let result_value: Value =
            serde_json::from_str(output).unwrap_or_else(|_| Value::String(output.to_owned()));

        // Patch path: look up the pending Patch cell.
        if let Some(id) = call_id.as_ref() {
            if let Some(&index) = self.pending_patch.get(id) {
                if let Some(SessionCell::Patch {
                    success,
                    stdout,
                    stderr,
                    ..
                }) = self.cells.get_mut(index)
                {
                    let exit_code = result_value
                        .get("metadata")
                        .and_then(|meta| meta.get("exit_code"))
                        .and_then(Value::as_i64);
                    *success = exit_code == Some(0);
                    if let Some(text) = result_value.get("output").and_then(Value::as_str) {
                        if !text.trim().is_empty() {
                            *stdout = text.trim().to_owned();
                        }
                    }
                    if let Some(text) = result_value.get("stderr").and_then(Value::as_str) {
                        if !text.trim().is_empty() {
                            *stderr = text.trim().to_owned();
                        }
                    }
                }
                // Don't drop pending — `event_msg.patch_apply_end` may enrich further.
                return;
            }
        }

        // Generic ToolResult fallback. When the value didn't carry a known
        // text field (`stdout` / `output` / `text` / `content`), prefer storing
        // pretty-printed full JSON so the viewer can syntect-highlight it
        // rather than landing on a truncated single-line blob.
        let human = tool_format::format_tool_result(&result_value);
        let formatted = if matches!(&result_value, Value::Object(_) | Value::Array(_))
            && (human.starts_with('{') || human.starts_with('['))
        {
            serde_json::to_string_pretty(&result_value).unwrap_or(human)
        } else {
            human
        };
        if formatted.trim().is_empty() {
            return;
        }
        let mut paired_tool: Option<String> = None;
        let mut call_summary: Option<String> = None;
        if let Some(id) = call_id.as_ref() {
            if let Some(&index) = self.pending_tool.get(id) {
                if let Some(SessionCell::ToolCall {
                    tool,
                    summary,
                    status,
                    ..
                }) = self.cells.get_mut(index)
                {
                    paired_tool = Some(tool.clone());
                    if !summary.is_empty() {
                        call_summary = Some(summary.clone());
                    }
                    *status = ToolStatus::Completed;
                }
                self.pending_tool.remove(id);
            }
        }
        self.cells.push(SessionCell::ToolResult {
            tool: paired_tool,
            output: formatted,
            is_error: false,
            call_summary,
            timestamp,
        });
    }

    fn push_web_search_cell(&mut self, payload: &Value, timestamp: Option<DateTime<Utc>>) {
        let query = payload
            .get("action")
            .and_then(|action| action.get("query"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let call_id = string_field(payload, "call_id");
        let index = self.cells.len();
        self.cells.push(SessionCell::WebSearch {
            query,
            queries: Vec::new(),
            timestamp,
        });
        if let Some(id) = call_id {
            self.pending_search.insert(id, index);
        }
    }

    fn handle_exec_command_end(&mut self, payload: &Value) {
        let Some(call_id) = string_field(payload, "call_id") else {
            return;
        };
        let Some(&index) = self.pending_exec.get(&call_id) else {
            return;
        };

        let stdout_text = payload
            .get("stdout")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let stderr_text = payload
            .get("stderr")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let aggregated = payload
            .get("aggregated_output")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let exit_code = payload
            .get("exit_code")
            .and_then(Value::as_i64)
            .map(|n| n as i32);
        let duration_ms = payload.get("duration").and_then(duration_to_ms);
        let parsed_summary = payload
            .get("parsed_cmd")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(|first| first.get("cmd"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let command = payload
            .get("command")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if let Some(SessionCell::Exec {
            command: cmd_field,
            stdout,
            stderr,
            exit_code: exit_field,
            duration_ms: dur_field,
            parsed_summary: parsed_field,
            status,
            ..
        }) = self.cells.get_mut(index)
        {
            if !command.is_empty() {
                *cmd_field = command;
            }
            // Prefer dedicated stdout/stderr fields; fall back to aggregated_output.
            if !stdout_text.is_empty() {
                *stdout = stdout_text;
            } else if stdout.is_empty() {
                if let Some(agg) = aggregated.clone() {
                    if !agg.is_empty() {
                        *stdout = agg;
                    }
                }
            }
            if !stderr_text.is_empty() {
                *stderr = stderr_text;
            }
            if exit_code.is_some() {
                *exit_field = exit_code;
            }
            if duration_ms.is_some() {
                *dur_field = duration_ms;
            }
            if parsed_summary.is_some() {
                *parsed_field = parsed_summary;
            }
            *status = match exit_code {
                Some(0) => ExecStatus::Completed,
                Some(_) => ExecStatus::Failed,
                None => *status,
            };
        }
        // Mark this call_id as finalized. Codex always emits a trailing
        // `function_call_output` carrying a text-wrapped duplicate of the
        // stdout we just absorbed; that handler will see this set and skip
        // pushing a redundant cell.
        self.completed_exec.insert(call_id.clone());
        self.pending_exec.remove(&call_id);
    }

    fn handle_patch_apply_end(&mut self, payload: &Value) {
        let Some(call_id) = string_field(payload, "call_id") else {
            return;
        };
        let Some(&index) = self.pending_patch.get(&call_id) else {
            return;
        };

        let success = payload
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let stdout_text = payload
            .get("stdout")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let stderr_text = payload
            .get("stderr")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let mut new_files: Vec<PatchFile> = Vec::new();
        if let Some(changes) = payload.get("changes").and_then(Value::as_object) {
            for (path, change) in changes {
                let kind = change.get("type").and_then(Value::as_str).unwrap_or("");
                let op = match kind {
                    "add" => PatchOp::Add,
                    "delete" => PatchOp::Delete,
                    _ => PatchOp::Update,
                };
                let content = change
                    .get("content")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                new_files.push(PatchFile {
                    path: path.clone(),
                    op,
                    content,
                    additions: 0,
                    deletions: 0,
                });
            }
        }

        if let Some(SessionCell::Patch {
            files,
            success: success_field,
            stdout,
            stderr,
            ..
        }) = self.cells.get_mut(index)
        {
            if !new_files.is_empty() {
                *files = new_files;
            }
            *success_field = success;
            if !stdout_text.is_empty() {
                *stdout = stdout_text;
            }
            if !stderr_text.is_empty() {
                *stderr = stderr_text;
            }
        }
        self.pending_patch.remove(&call_id);
    }

    fn handle_web_search_end(&mut self, payload: &Value) {
        let Some(call_id) = string_field(payload, "call_id") else {
            return;
        };
        let Some(&index) = self.pending_search.get(&call_id) else {
            return;
        };

        let mut new_query = payload
            .get("query")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let mut new_queries: Vec<String> = Vec::new();
        if let Some(action) = payload.get("action") {
            if new_query.is_none() {
                new_query = action
                    .get("query")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
            }
            if let Some(arr) = action.get("queries").and_then(Value::as_array) {
                new_queries = arr
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect();
            }
        }

        if let Some(SessionCell::WebSearch { query, queries, .. }) = self.cells.get_mut(index) {
            if let Some(q) = new_query {
                if !q.trim().is_empty() {
                    *query = q;
                }
            }
            if !new_queries.is_empty() {
                *queries = new_queries;
            }
        }
        self.pending_search.remove(&call_id);
    }

    /// Parse the legacy text-formatted exec output emitted by older Codex versions.
    /// Recognized prefixes (in either order):
    ///   "Command: <cmd>\n"
    ///   "Wall time: <secs> seconds\n"
    ///   "Process exited with code <N>\n"
    ///   "Exit code: <N>\n"
    ///   "Output:\n<stdout-rest>"
    fn update_exec_from_text_output(&mut self, index: usize, raw: &str) {
        let mut wall_ms: Option<u64> = None;
        let mut exit_code: Option<i32> = None;
        let mut output_body: Option<String> = None;
        let mut iter = raw.lines().peekable();
        while let Some(line) = iter.next() {
            if let Some(rest) = line.strip_prefix("Wall time: ") {
                let secs = rest.trim_end_matches(" seconds").trim();
                if let Ok(secs_f) = secs.parse::<f64>() {
                    wall_ms = Some((secs_f * 1000.0) as u64);
                }
            } else if let Some(rest) = line.strip_prefix("Exit code: ") {
                if let Ok(n) = rest.trim().parse::<i32>() {
                    exit_code = Some(n);
                }
            } else if let Some(rest) = line.strip_prefix("Process exited with code ") {
                if let Ok(n) = rest.trim().parse::<i32>() {
                    exit_code = Some(n);
                }
            } else if line.trim() == "Output:" {
                let rest: Vec<&str> = iter.by_ref().collect();
                output_body = Some(rest.join("\n"));
                break;
            }
        }

        if let Some(SessionCell::Exec {
            stdout,
            exit_code: exit_field,
            duration_ms: dur_field,
            status,
            ..
        }) = self.cells.get_mut(index)
        {
            if let Some(body) = output_body {
                let trimmed = body.trim_end();
                if !trimmed.is_empty() {
                    *stdout = trimmed.to_owned();
                }
            } else if !raw.trim().is_empty() && stdout.is_empty() {
                // No "Output:" prefix found — store whole payload as stdout.
                *stdout = raw.trim().to_owned();
            }
            if exit_code.is_some() {
                *exit_field = exit_code;
            }
            if wall_ms.is_some() {
                *dur_field = wall_ms;
            }
            *status = match exit_code {
                Some(0) => ExecStatus::Completed,
                Some(_) => ExecStatus::Failed,
                None => *status,
            };
        }
    }
}

fn duration_to_ms(value: &Value) -> Option<u64> {
    let secs = value.get("secs").and_then(Value::as_u64).unwrap_or(0);
    let nanos = value.get("nanos").and_then(Value::as_u64).unwrap_or(0);
    Some(secs.saturating_mul(1000).saturating_add(nanos / 1_000_000))
}

fn parse_exec_arguments(
    raw_name: &str,
    args: &Value,
) -> (Vec<String>, Option<String>, Option<String>) {
    let mut command_str: Option<String> = None;
    let mut cwd: Option<String> = None;

    if let Value::Object(map) = args {
        for key in &["command", "cmd"] {
            if let Some(Value::String(value)) = map.get(*key) {
                if !value.trim().is_empty() {
                    command_str = Some(value.clone());
                    break;
                }
            }
        }
        for key in &["workdir", "cwd"] {
            if let Some(Value::String(value)) = map.get(*key) {
                if !value.trim().is_empty() {
                    cwd = Some(value.clone());
                    break;
                }
            }
        }
    } else if let Value::String(text) = args {
        if !text.trim().is_empty() {
            command_str = Some(text.clone());
        }
    }

    let command = match command_str.as_deref() {
        Some(text) => vec!["/bin/sh".to_owned(), "-c".to_owned(), text.to_owned()],
        None => Vec::new(),
    };
    let parsed_summary = command_str.map(|s| s.lines().next().unwrap_or(&s).to_owned());

    let _ = raw_name;
    (command, cwd, parsed_summary)
}

fn parse_plan_items(args: &Value) -> Option<Vec<PlanItem>> {
    let array = args.get("plan").and_then(Value::as_array)?;
    let mut items = Vec::with_capacity(array.len());
    for entry in array {
        let step = entry.get("step").and_then(Value::as_str)?;
        let status_text = entry
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        let status = match status_text {
            "in_progress" => PlanItemStatus::InProgress,
            "completed" => PlanItemStatus::Completed,
            _ => PlanItemStatus::Pending,
        };
        items.push(PlanItem {
            status,
            step: step.to_owned(),
        });
    }
    if items.is_empty() {
        None
    } else {
        Some(items)
    }
}

fn split_reasoning_header_body(text: &str) -> (Option<String>, String) {
    // Codex sometimes emits `**Header**\n\nbody...`; capture the header.
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix("**") {
        if let Some(end) = rest.find("**") {
            let header = rest[..end].trim().to_owned();
            let body_start = end + 2;
            let body = rest[body_start..].trim_start_matches('\n').to_owned();
            if !header.is_empty() {
                return (Some(header), body);
            }
        }
    }
    (None, text.to_owned())
}

/// Parse a V4A patch text envelope into structured `PatchFile`s.
///
/// Recognized markers (one per file):
///   `*** Add File: <path>`
///   `*** Update File: <path>`
///   `*** Delete File: <path>`
/// We count `+`/`-` lines between markers as additions/deletions; for `Add`,
/// the content body is captured.
fn parse_v4a_patch(text: &str) -> Vec<PatchFile> {
    let mut files: Vec<PatchFile> = Vec::new();
    let mut cur_path: Option<String> = None;
    let mut cur_op: Option<PatchOp> = None;
    let mut cur_content: Vec<String> = Vec::new();
    let mut cur_adds: usize = 0;
    let mut cur_dels: usize = 0;

    let flush = |files: &mut Vec<PatchFile>,
                 path: &mut Option<String>,
                 op: &mut Option<PatchOp>,
                 content: &mut Vec<String>,
                 adds: &mut usize,
                 dels: &mut usize| {
        if let (Some(p), Some(o)) = (path.take(), op.take()) {
            let body = if matches!(o, PatchOp::Add) && !content.is_empty() {
                Some(content.join("\n"))
            } else {
                None
            };
            files.push(PatchFile {
                path: p,
                op: o,
                content: body,
                additions: *adds,
                deletions: *dels,
            });
        }
        content.clear();
        *adds = 0;
        *dels = 0;
    };

    for line in text.lines() {
        let trimmed = line.trim_end();
        if let Some(rest) = trimmed.strip_prefix("*** Add File: ") {
            flush(
                &mut files,
                &mut cur_path,
                &mut cur_op,
                &mut cur_content,
                &mut cur_adds,
                &mut cur_dels,
            );
            cur_path = Some(rest.trim().to_owned());
            cur_op = Some(PatchOp::Add);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("*** Update File: ") {
            flush(
                &mut files,
                &mut cur_path,
                &mut cur_op,
                &mut cur_content,
                &mut cur_adds,
                &mut cur_dels,
            );
            cur_path = Some(rest.trim().to_owned());
            cur_op = Some(PatchOp::Update);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("*** Delete File: ") {
            flush(
                &mut files,
                &mut cur_path,
                &mut cur_op,
                &mut cur_content,
                &mut cur_adds,
                &mut cur_dels,
            );
            cur_path = Some(rest.trim().to_owned());
            cur_op = Some(PatchOp::Delete);
            continue;
        }
        if trimmed == "*** End Patch" || trimmed == "*** Begin Patch" {
            continue;
        }
        if cur_op.is_none() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            cur_adds += 1;
            if matches!(cur_op, Some(PatchOp::Add)) {
                cur_content.push(rest.to_owned());
            }
            continue;
        }
        if line.starts_with('-') {
            cur_dels += 1;
            continue;
        }
        if matches!(cur_op, Some(PatchOp::Add)) {
            cur_content.push(line.to_owned());
        }
    }
    flush(
        &mut files,
        &mut cur_path,
        &mut cur_op,
        &mut cur_content,
        &mut cur_adds,
        &mut cur_dels,
    );

    files
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
    if preview.is_some()
        || should_skip_display_message("user", raw)
        || is_contextual_user_message_content(MessageRole::User, raw)
    {
        return;
    }

    *preview = normalize_codex_user_message(raw);
}

fn maybe_capture_response_item_preview(role: &str, raw: &str, preview: &mut Option<String>) {
    if preview.is_some()
        || role != "user"
        || should_skip_display_message(role, raw)
        || is_contextual_user_message_content(MessageRole::User, raw)
    {
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
            return Err(error)
                .with_context(|| format!("failed to read metadata for {}", index_path.display()));
        }
    };
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
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
    static CACHE: OnceLock<Mutex<HashMap<std::path::PathBuf, CachedThreadNames>>> = OnceLock::new();
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

#[cfg(test)]
mod cell_tests {
    use super::{parse_codex_session_file, user_message_line_multiset_sha256};
    use crate::parse::{PatchOp, SessionCell};
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        let manifest = env!("CARGO_MANIFEST_DIR");
        PathBuf::from(manifest)
            .join("tests")
            .join("fixtures")
            .join("sessions")
            .join("codex")
            .join(name)
    }

    #[test]
    fn parser_emits_at_least_one_cell_for_real_fixtures() {
        for name in ["new_format.jsonl", "latest_format.jsonl"] {
            let session = parse_codex_session_file(fixture(name))
                .expect("parse")
                .unwrap_or_else(|| panic!("{name}: parser returned None"));
            assert!(
                !session.cells.is_empty(),
                "{name}: cells should not be empty"
            );
        }
    }

    #[test]
    fn parser_excludes_developer_and_system_messages_from_searchable_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.jsonl");
        let body = concat!(
            "{\"timestamp\":\"2026-04-26T18:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"timestamp\":\"2026-04-26T18:00:00Z\",\"cwd\":\"/tmp\"}}\n",
            "{\"timestamp\":\"2026-04-26T18:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"developer\",\"content\":[{\"type\":\"input_text\",\"text\":\"developer-only needle\"}]}}\n",
            "{\"timestamp\":\"2026-04-26T18:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"system\",\"content\":[{\"type\":\"input_text\",\"text\":\"system-only needle\"}]}}\n",
            "{\"timestamp\":\"2026-04-26T18:00:03Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"searchable user text\"}]}}\n",
            "{\"timestamp\":\"2026-04-26T18:00:04Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"searchable assistant text\"}]}}\n",
        );
        std::fs::write(&path, body).expect("write fixture");

        let session = parse_codex_session_file(&path)
            .expect("parse")
            .expect("session");

        assert!(!session.content.contains("developer-only needle"));
        assert!(!session.content.contains("system-only needle"));
        assert!(session.content.contains("searchable user text"));
        assert!(session.content.contains("searchable assistant text"));
    }

    #[test]
    fn latest_format_produces_paired_exec_and_patch_cells() {
        use crate::parse::ExecStatus;
        let session = parse_codex_session_file(fixture("latest_format.jsonl"))
            .expect("parse")
            .expect("session");
        let exec_cells: Vec<_> = session
            .cells
            .iter()
            .filter_map(|c| match c {
                SessionCell::Exec {
                    parsed_summary,
                    exit_code,
                    duration_ms,
                    status,
                    ..
                } => Some((parsed_summary.clone(), *exit_code, *duration_ms, *status)),
                _ => None,
            })
            .collect();
        assert!(!exec_cells.is_empty(), "expected at least one Exec cell");
        // First exec in the fixture is `cat src/main.rs`, exit 0.
        let first = &exec_cells[0];
        assert_eq!(first.1, Some(0));
        assert!(first.3 == ExecStatus::Completed);
        assert!(first.0.as_deref().unwrap_or("").contains("cat"));

        let patch_cells: Vec<_> = session
            .cells
            .iter()
            .filter_map(|c| match c {
                SessionCell::Patch { files, success, .. } => Some((files.clone(), *success)),
                _ => None,
            })
            .collect();
        assert!(!patch_cells.is_empty(), "expected at least one Patch cell");
        let (files, success) = &patch_cells[0];
        assert!(*success, "patch should be marked successful");
        assert!(!files.is_empty(), "patch should list at least one file");
        assert!(matches!(files[0].op, PatchOp::Update | PatchOp::Add));
    }

    #[test]
    fn parser_emits_metrics_cell_with_token_totals() {
        let session = parse_codex_session_file(fixture("latest_format.jsonl"))
            .expect("parse")
            .expect("session");
        let metrics = session
            .cells
            .iter()
            .find_map(|c| match c {
                SessionCell::Metrics(m) => Some(m.clone()),
                _ => None,
            })
            .expect("metrics cell present");
        assert!(metrics.total_tokens > 0, "expected non-zero total_tokens");
        assert!(
            metrics.tool_call_count >= 1,
            "expected at least one tool call"
        );
        assert!(metrics.exec_count >= 1, "expected at least one exec");
        assert!(metrics.patch_count >= 1, "expected at least one patch");
    }

    /// Walks the user's actual `~/.codex/sessions` and parses each rollout.
    /// Asserts no panic; counts cell types as evidence.
    ///
    /// Gated with `#[ignore]` so it doesn't run unless requested:
    ///   `cargo test --lib smoke -- --ignored`
    #[test]
    #[ignore]
    fn smoke_parse_real_codex_sessions_without_panic() {
        let home = match std::env::var("HOME") {
            Ok(h) => h,
            Err(_) => return,
        };
        let dir = PathBuf::from(home).join(".codex").join("sessions");
        if !dir.is_dir() {
            return;
        }

        let mut count = 0usize;
        let mut errors = 0usize;
        let mut none_returned = 0usize;
        let mut total_cells = 0usize;
        let mut by_kind: std::collections::HashMap<&'static str, usize> =
            std::collections::HashMap::new();
        walk_jsonl(&dir, &mut |path| {
            count += 1;
            match parse_codex_session_file(path) {
                Ok(Some(session)) => {
                    total_cells += session.cells.len();
                    for cell in &session.cells {
                        let kind = match cell {
                            SessionCell::Message { .. } => "message",
                            SessionCell::Reasoning { .. } => "reasoning",
                            SessionCell::ToolCall { .. } => "tool_call",
                            SessionCell::ToolResult { .. } => "tool_result",
                            SessionCell::Exec { .. } => "exec",
                            SessionCell::Patch { .. } => "patch",
                            SessionCell::WebSearch { .. } => "web_search",
                            SessionCell::Plan { .. } => "plan",
                            SessionCell::SessionInfo(_) => "session_info",
                            SessionCell::Metrics(_) => "metrics",
                        };
                        *by_kind.entry(kind).or_default() += 1;
                    }
                }
                Ok(None) => none_returned += 1,
                Err(_) => errors += 1,
            }
        });

        assert!(count > 0, "no real rollouts found at expected path");
        eprintln!(
            "smoke: parsed {count} files, {errors} errors, {none_returned} None, {total_cells} total cells"
        );
        let mut kinds: Vec<_> = by_kind.iter().collect();
        kinds.sort();
        for (kind, n) in kinds {
            eprintln!("  {kind}: {n}");
        }
        assert_eq!(errors, 0, "{errors} files failed to parse");
    }

    fn walk_jsonl(dir: &std::path::Path, on_file: &mut dyn FnMut(&std::path::Path)) {
        let Ok(read_dir) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk_jsonl(&path, on_file);
            } else if path.extension().is_some_and(|ext| ext == "jsonl") {
                on_file(&path);
            }
        }
    }

    #[test]
    fn patch_v4a_parser_recognises_add_and_update() {
        use super::parse_v4a_patch;
        let text = "*** Begin Patch\n\
                    *** Add File: foo.rs\n\
                    +pub fn foo() {}\n\
                    *** Update File: bar.rs\n\
                    @@\n\
                    -let a = 1;\n\
                    +let a = 2;\n\
                    *** End Patch";
        let files = parse_v4a_patch(text);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "foo.rs");
        assert!(matches!(files[0].op, PatchOp::Add));
        assert_eq!(files[0].additions, 1);
        assert!(files[0].content.is_some());
        assert_eq!(files[1].path, "bar.rs");
        assert!(matches!(files[1].op, PatchOp::Update));
        assert_eq!(files[1].additions, 1);
        assert_eq!(files[1].deletions, 1);
    }

    #[test]
    fn parser_pairs_tool_result_with_call_summary_for_generic_tool() {
        // function_call (unknown tool name) -> function_call_output should
        // produce a ToolResult cell whose `call_summary` echoes the formatted
        // call args (since the tool isn't bash/read/etc., the generic-fallback
        // path runs and stores key/value pairs as the summary).
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.jsonl");
        let body = concat!(
            "{\"timestamp\":\"2026-04-26T18:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"timestamp\":\"2026-04-26T18:00:00Z\",\"cwd\":\"/tmp\"}}\n",
            "{\"timestamp\":\"2026-04-26T18:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"name\":\"my_mcp_tool\",\"call_id\":\"c1\",\"arguments\":\"{\\\"endpoint\\\":\\\"/v1/x\\\",\\\"verb\\\":\\\"GET\\\"}\"}}\n",
            "{\"timestamp\":\"2026-04-26T18:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"function_call_output\",\"call_id\":\"c1\",\"output\":\"hello body\"}}\n",
        );
        std::fs::write(&path, body).expect("write fixture");

        let session = parse_codex_session_file(&path)
            .expect("parse")
            .expect("session");
        let result = session
            .cells
            .iter()
            .find_map(|c| match c {
                SessionCell::ToolResult {
                    call_summary, tool, ..
                } => Some((call_summary.clone(), tool.clone())),
                _ => None,
            })
            .expect("ToolResult cell missing");
        let (summary, tool) = result;
        assert_eq!(tool.as_deref(), Some("my_mcp_tool"));
        let summary = summary.expect("call_summary should be populated");
        assert!(
            summary.contains("endpoint") && summary.contains("/v1/x"),
            "call_summary should echo the call args: {summary}"
        );
    }

    #[test]
    fn parser_suppresses_redundant_function_call_output_after_exec_command_end() {
        // exec_command function_call -> exec_command_end (finalizes the Exec
        // cell) -> trailing function_call_output. The trailing output is just
        // a text-wrapped duplicate of stdout the Exec already absorbed, so the
        // parser should drop it instead of pushing a duplicate ToolResult.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.jsonl");
        let body = concat!(
            "{\"timestamp\":\"2026-04-26T18:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"timestamp\":\"2026-04-26T18:00:00Z\",\"cwd\":\"/tmp\"}}\n",
            "{\"timestamp\":\"2026-04-26T18:02:36Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"name\":\"exec_command\",\"call_id\":\"call_x\",\"arguments\":\"{\\\"cmd\\\":\\\"git log --oneline\\\"}\"}}\n",
            "{\"timestamp\":\"2026-04-26T18:02:36Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"exec_command_end\",\"call_id\":\"call_x\",\"command\":[\"/bin/zsh\",\"-lc\",\"git log --oneline\"],\"stdout\":\"abc Commit\",\"stderr\":\"\",\"exit_code\":0,\"duration\":{\"secs\":0,\"nanos\":12000000},\"parsed_cmd\":[{\"cmd\":\"git log --oneline\"}]}}\n",
            "{\"timestamp\":\"2026-04-26T18:02:36Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"function_call_output\",\"call_id\":\"call_x\",\"output\":\"Chunk ID: abc\\nWall time: 0.0000 seconds\\nProcess exited with code 0\\nOriginal token count: 7\\nOutput:\\nabc Commit\\n\"}}\n",
        );
        std::fs::write(&path, body).expect("write fixture");

        let session = parse_codex_session_file(&path)
            .expect("parse")
            .expect("session");

        let exec_count = session
            .cells
            .iter()
            .filter(|c| matches!(c, SessionCell::Exec { .. }))
            .count();
        assert_eq!(exec_count, 1, "expected exactly one Exec cell");

        let result_count = session
            .cells
            .iter()
            .filter(|c| matches!(c, SessionCell::ToolResult { .. }))
            .count();
        assert_eq!(
            result_count, 0,
            "trailing function_call_output for finalized exec should be suppressed, not rendered as a ToolResult"
        );
    }

    #[test]
    fn parser_populates_session_info_when_meta_present() {
        let session = parse_codex_session_file(fixture("latest_format.jsonl"))
            .expect("parse")
            .expect("session");
        let info = session.session_info.expect("session_info populated");
        assert_eq!(info.cli_version.as_deref(), Some("0.116.0"));
        assert_eq!(info.model_provider.as_deref(), Some("openai"));
        assert_eq!(info.source.as_deref(), Some("cli"));
        assert_eq!(info.originator.as_deref(), Some("codex_cli_rs"));
        assert!(info.cwd.is_some(), "cwd should be set");
        // Latest format also has turn_context with model + sandbox.
        assert!(
            info.model.is_some(),
            "model should be populated from turn_context"
        );
        // First cell should be SessionInfo.
        assert!(matches!(
            session.cells.first(),
            Some(SessionCell::SessionInfo(_))
        ));
    }

    #[test]
    fn parser_captures_current_codex_fork_lineage() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fork.jsonl");
        let body = concat!(
            "{\"timestamp\":\"2026-04-26T18:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"child\",\"forked_from_id\":\"parent\",\"cwd\":\"/tmp\"}}\n",
            "{\"timestamp\":\"2026-04-26T17:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"parent\",\"forked_from_id\":\"grandparent\",\"cwd\":\"/tmp\"}}\n",
            "{\"timestamp\":\"2026-04-26T18:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"item-1\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hello\"}]}}\n",
            "{\"timestamp\":\"2026-04-26T18:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"call_id\":\"call-1\",\"name\":\"demo\",\"arguments\":\"{}\"}}\n",
        );
        std::fs::write(&path, body).expect("write fixture");

        let session = parse_codex_session_file(&path)
            .expect("parse")
            .expect("session");

        assert_eq!(session.session_id, "child");
        assert_eq!(
            session.lineage.forked_from_session_id.as_deref(),
            Some("parent")
        );
        assert_eq!(session.lineage.history_base_session_id, None);
        assert_eq!(
            session.lineage.semantic_event_ids,
            ["function_call:call-1", "message:item-1"]
        );
    }

    #[test]
    fn parser_captures_reference_backed_codex_fork_lineage() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fork.jsonl");
        let body = concat!(
            "{\"timestamp\":\"2026-08-07T18:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"child\",\"forked_from_id\":\"logical-parent\",\"history_mode\":\"paginated\",\"history_base\":{\"thread_id\":\"physical-parent\",\"end_ordinal_exclusive\":42,\"end_byte_offset\":4096},\"cwd\":\"/tmp\"}}\n",
            "{\"timestamp\":\"2026-08-07T18:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"item-1\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"continue\"}]}}\n",
        );
        std::fs::write(&path, body).expect("write fixture");

        let session = parse_codex_session_file(&path)
            .expect("parse")
            .expect("session");

        assert_eq!(
            session.lineage.forked_from_session_id.as_deref(),
            Some("logical-parent")
        );
        assert_eq!(
            session.lineage.history_base_session_id.as_deref(),
            Some("physical-parent")
        );
    }

    #[test]
    fn user_message_fingerprint_ignores_line_order_but_preserves_the_multiset() {
        let original = "update the table\n\n| Up | move |\n| Enter | select |";
        let reordered = "update the table\n\n| Enter | select |\n| Up | move |";
        let duplicated = "update the table\n\n| Up | move |\n| Up | move |\n| Enter | select |";

        assert_eq!(
            user_message_line_multiset_sha256(original),
            user_message_line_multiset_sha256(reordered)
        );
        assert_ne!(
            user_message_line_multiset_sha256(original),
            user_message_line_multiset_sha256(duplicated)
        );
        assert_ne!(
            user_message_line_multiset_sha256(original),
            user_message_line_multiset_sha256("update a different table")
        );
    }

    #[test]
    fn parser_captures_trailing_aborted_turn_lineage() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("aborted.jsonl");
        let body = concat!(
            "{\"timestamp\":\"2026-08-01T10:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"parent\",\"cwd\":\"/tmp\"}}\n",
            "{\"timestamp\":\"2026-08-01T10:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"base-user\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hello\"}]}}\n",
            "{\"timestamp\":\"2026-08-01T10:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"base-assistant\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}}\n",
            "{\"timestamp\":\"2026-08-01T10:01:00Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"retry-user\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"continue here\"}]}}\n",
            "{\"timestamp\":\"2026-08-01T10:01:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"abort\",\"role\":\"developer\",\"content\":[{\"type\":\"input_text\",\"text\":\"<turn_aborted>\\nThe previous turn was interrupted on purpose.\\n</turn_aborted>\"}]}}\n",
        );
        std::fs::write(&path, body).expect("write fixture");

        let session = parse_codex_session_file(&path)
            .expect("parse")
            .expect("session");
        let aborted = session
            .lineage
            .trailing_aborted_turn
            .as_ref()
            .expect("trailing aborted turn");

        assert_eq!(
            aborted.user_message_line_multiset_sha256,
            user_message_line_multiset_sha256("continue here")
        );
        assert_eq!(
            aborted.semantic_event_ids,
            ["message:abort", "message:retry-user"]
        );
        assert!(!aborted.had_assistant_or_tool_activity);
        assert_eq!(
            session.lineage.assistant_or_tool_event_ids,
            ["message:base-assistant"]
        );
    }

    #[test]
    fn parser_captures_idless_aborted_turn_with_partial_activity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("legacy-aborted.jsonl");
        let body = concat!(
            "{\"timestamp\":\"2026-07-17T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"parent\",\"cwd\":\"/tmp\"}}\n",
            "{\"timestamp\":\"2026-07-17T00:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hello\"}]}}\n",
            "{\"timestamp\":\"2026-07-17T00:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"base-assistant\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}}\n",
            "{\"timestamp\":\"2026-07-17T00:01:00Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"continue here\"}]}}\n",
            "{\"timestamp\":\"2026-07-17T00:01:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"reasoning\",\"id\":\"parent-reasoning\",\"summary\":[]}}\n",
            "{\"timestamp\":\"2026-07-17T00:01:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"parent-ack\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"working\"}]}}\n",
            "{\"timestamp\":\"2026-07-17T00:01:03Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"call_id\":\"parent-tool\",\"name\":\"exec\",\"input\":{}}}\n",
            "{\"timestamp\":\"2026-07-17T00:01:04Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call_output\",\"call_id\":\"parent-tool\",\"output\":\"{}\"}}\n",
            "{\"timestamp\":\"2026-07-17T00:01:05Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"developer\",\"content\":[{\"type\":\"input_text\",\"text\":\"<turn_aborted>\\ninterrupted\\n</turn_aborted>\"}]}}\n",
        );
        std::fs::write(&path, body).expect("write fixture");

        let session = parse_codex_session_file(&path)
            .expect("parse")
            .expect("session");
        let aborted = session
            .lineage
            .trailing_aborted_turn
            .as_ref()
            .expect("trailing aborted turn");

        assert_eq!(
            aborted.user_message_line_multiset_sha256,
            user_message_line_multiset_sha256("continue here")
        );
        assert_eq!(
            aborted.semantic_event_ids,
            [
                "custom_tool_call:parent-tool",
                "custom_tool_call_output:parent-tool",
                "message:parent-ack",
                "reasoning:parent-reasoning",
            ]
        );
        assert!(aborted.had_assistant_or_tool_activity);
        assert_eq!(session.lineage.codex_user_turns.len(), 1);
        assert_eq!(
            session.lineage.codex_user_turns[0].last_assistant_or_tool_event_id,
            "message:base-assistant"
        );
    }
}
