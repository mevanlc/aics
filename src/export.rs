//! Rendering sessions to standalone files.
//!
//! Two envelopes share one set of naming and collision rules: the TUI's export
//! action writes plain text into the current directory, and `--export` writes
//! Markdown into a chosen directory.
//!
//! Message bodies are already Markdown as the agent wrote them, so the Markdown
//! envelope has only two jobs: keep its own headings out of the way, and fence
//! everything that is *not* Markdown. Tool output routinely contains fenced code
//! blocks and lines starting with `#`, so unfenced payloads would restructure
//! the surrounding document.
//!
//! An export is an archive, not a view, so it starts from
//! `DisplayOptions::SHOW_ALL` rather than the saved viewer preferences — losing
//! every tool result from an archive because a preview pane was tidier without
//! them would be a bad trade. Callers that do want less pass their own
//! `DisplayOptions`, and the visibility rules then match the preview's exactly.

use std::fs;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local, Utc};
use serde_json::Value;

use crate::fs_safename::{
    validate_windows_filename_component, validate_windows_stem_with_extension,
};
use crate::parse::{
    is_project_docs_autodump, is_skill_text_injection, ExecStatus, MessageRole, PatchOp,
    PlanItemStatus, RuntimeMetrics, Session, SessionCell, SessionInfo, SessionMessage, ToolStatus,
};
use crate::settings::DisplayOptions;

/// Cap on the title slug appended to a Markdown export filename. Long enough to
/// stay recognizable, short enough that the date and session id stay visible in
/// a narrow terminal.
const MAX_TITLE_SLUG_CHARS: usize = 48;

/// Render a session as plain text: a role/timestamp line per message, then the
/// body verbatim. This is what the TUI's export action writes.
pub fn session_to_plain_text(session: &Session) -> String {
    let mut output = String::new();
    for message in &session.messages {
        let role_display = match (&message.role, &message.tool_name) {
            (MessageRole::ToolCall, Some(name)) => format!("tool_call({name})"),
            (MessageRole::ToolResult, Some(name)) => format!("tool_result({name})"),
            _ => message.role.to_string(),
        };
        let timestamp = message
            .timestamp
            .map(|time| time.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        if timestamp.is_empty() {
            output.push_str(&format!("{role_display}\n"));
        } else {
            output.push_str(&format!("{role_display} {timestamp}\n"));
        }
        output.push_str(&message.content);
        output.push_str("\n\n");
    }
    output
}

/// Render a whole session as a Markdown document, hiding nothing.
pub fn session_to_markdown(session: &Session) -> String {
    session_to_markdown_with_options(session, DisplayOptions::SHOW_ALL)
}

/// Render a session as a Markdown document, omitting the parts `display_options`
/// hides.
///
/// Renders from `cells` when the parser produced them, since those preserve
/// exec pairing, patch contents, reasoning, and plans. Falls back to the flat
/// `messages` list otherwise, matching how the preview and viewer dispatch.
pub fn session_to_markdown_with_options(
    session: &Session,
    display_options: DisplayOptions,
) -> String {
    let mut output = String::new();
    // Provenance is not part of the transcript, so `--hide` never touches it.
    push_document_header(&mut output, session);

    if session.cells.is_empty() {
        for message in &session.messages {
            if hides_message(display_options, message.role, &message.content) {
                continue;
            }
            push_message(&mut output, message);
        }
    } else {
        let mut context = CellContext::default();
        for cell in &session.cells {
            push_cell(&mut output, cell, display_options, &mut context);
        }
    }

    output
}

/// Carried across cells so a result can tell when it would merely repeat the
/// call above it. The TUI shows that repetition on purpose — it is what the
/// sticky header displays once a long result scrolls the call off screen — but
/// in a linear document the call is right there.
#[derive(Default)]
struct CellContext {
    last_call_summary: Option<String>,
}

/// Write `session` into `dir` as Markdown, creating `dir` if needed.
pub fn write_session_markdown(
    dir: &Path,
    session: &Session,
    display_options: DisplayOptions,
) -> Result<PathBuf> {
    fs::create_dir_all(dir)
        .with_context(|| format!("failed to create directory {}", dir.display()))?;
    write_unique(
        dir,
        &markdown_export_stem(session),
        "md",
        &session_to_markdown_with_options(session, display_options),
    )
}

/// Mirror the preview's per-message visibility rules so `--hide` means the same
/// thing in an export as it does in the TUI.
fn hides_message(display_options: DisplayOptions, role: MessageRole, content: &str) -> bool {
    let role_hidden = match role {
        MessageRole::User => display_options.hide_user_messages,
        MessageRole::Assistant => display_options.hide_agent_replies,
        MessageRole::ToolCall => display_options.hide_tool_calls,
        MessageRole::ToolResult => display_options.hide_tool_results,
        MessageRole::System | MessageRole::Summary => false,
    };

    role_hidden
        || display_options.hide_project_docs_autodump && is_project_docs_autodump(role, content)
        || display_options.hide_skill_text_injection && is_skill_text_injection(role, content)
}

/// Write `rendered` as a `.txt` file in the current directory.
pub fn write_session_export(session: &Session, rendered: &str) -> Result<PathBuf> {
    let stem = export_stem_for_session(session)?;
    let cwd = std::env::current_dir().context("failed to resolve current directory")?;
    write_unique(&cwd, &stem, "txt", rendered)
}

/// Filename stem for the TUI's plain-text export: the custom title when the
/// session has one, else the session id.
pub fn export_stem_for_session(session: &Session) -> Result<String> {
    let stem = session
        .custom_title
        .clone()
        .unwrap_or_else(|| session.session_id.clone());
    validate_windows_filename_component(&stem)
        .with_context(|| format!("export stem `{stem}` is not Windows-safe"))?;
    Ok(stem)
}

/// Filename stem for a Markdown export: `YYYY-MM-DD-<agent>-<short id>` plus a
/// title slug when the session has a custom title.
///
/// The date sorts a batch chronologically, and the id ties each file back to the
/// session it came from.
///
/// It is the *modified* date, because that is the field `--after` and `--before`
/// filter on and the field time sort orders by. Naming files by creation date
/// would put a session created in May but continued in July outside the range
/// its own `--after July` export selected.
///
/// The date is local so it matches how `--after` and `--before` read a bare
/// `YYYY-MM-DD`.
pub fn markdown_export_stem(session: &Session) -> String {
    let date = session
        .modified
        .or(session.created)
        .map(|timestamp| {
            timestamp
                .with_timezone(&Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_else(|| "undated".to_owned());
    let mut stem = format!(
        "{date}-{}-{}",
        session.agent,
        short_session_id(&session.session_id)
    );
    if let Some(slug) = title_slug(session.custom_title.as_deref()) {
        stem.push('-');
        stem.push_str(&slug);
    }
    stem
}

/// Write `contents` to `dir/<stem>.<extension>`, appending `-1`, `-2`, … until a
/// name is free. `create_new` makes the check and the claim one atomic step, so
/// a concurrent export cannot silently overwrite this one.
fn write_unique(dir: &Path, stem: &str, extension: &str, contents: &str) -> Result<PathBuf> {
    for suffix in 0usize.. {
        let candidate_stem = if suffix == 0 {
            stem.to_owned()
        } else {
            format!("{stem}-{suffix}")
        };
        validate_windows_stem_with_extension(&candidate_stem, extension).with_context(|| {
            format!("export filename `{candidate_stem}.{extension}` is not Windows-safe")
        })?;

        let path = dir.join(format!("{candidate_stem}.{extension}"));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(contents.as_bytes())
                    .with_context(|| format!("failed to write {}", path.display()))?;
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("failed to create {}", path.display()));
            }
        }
    }

    bail!("failed to allocate a unique export filename")
}

fn short_session_id(session_id: &str) -> String {
    let short: String = session_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    if short.is_empty() {
        "session".to_owned()
    } else {
        short
    }
}

/// Lowercase the title and collapse every run of non-alphanumerics to a single
/// dash. Titles with no ASCII alphanumerics slug to nothing, and the caller
/// falls back to the date and id alone.
fn title_slug(title: Option<&str>) -> Option<String> {
    let title = title?.trim();
    let mut slug = String::new();
    let mut pending_separator = false;
    for character in title.chars() {
        if character.is_ascii_alphanumeric() {
            if pending_separator && !slug.is_empty() {
                slug.push('-');
            }
            pending_separator = false;
            slug.push(character.to_ascii_lowercase());
            if slug.chars().count() >= MAX_TITLE_SLUG_CHARS {
                break;
            }
        } else {
            pending_separator = true;
        }
    }

    (!slug.is_empty()).then_some(slug)
}

fn push_document_header(output: &mut String, session: &Session) {
    let title = session
        .custom_title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or(&session.session_id);
    output.push_str(&format!("# {title}\n\n"));

    push_meta(
        output,
        &[
            ("Session", inline_code(&session.session_id)),
            ("Agent", session.agent.to_string()),
            ("Project", inline_code(&session.project)),
            (
                "Branch",
                session
                    .branch
                    .as_deref()
                    .map(inline_code)
                    .unwrap_or_default(),
            ),
            (
                "Working directory",
                session.cwd.as_deref().map(inline_code).unwrap_or_default(),
            ),
            ("Derivation", session.derivation_type.to_string()),
            (
                "Created",
                session.created.map(format_timestamp).unwrap_or_default(),
            ),
            (
                "Modified",
                session.modified.map(format_timestamp).unwrap_or_default(),
            ),
            (
                "Source",
                inline_code(&session.file_path.display().to_string()),
            ),
        ],
    );

    // A SessionInfo cell renders in transcript order; only fall back to the
    // session-level copy when the parser did not emit one.
    let has_info_cell = session
        .cells
        .iter()
        .any(|cell| matches!(cell, SessionCell::SessionInfo(_)));
    if !has_info_cell {
        if let Some(info) = session
            .session_info
            .as_ref()
            .filter(|info| !info.is_empty())
        {
            push_session_info(output, info);
        }
    }

    output.push_str("---\n\n");
}

fn push_message(output: &mut String, message: &SessionMessage) {
    let label = match (message.role, message.tool_name.as_deref()) {
        (MessageRole::ToolCall, Some(name)) => format!("tool call: {name}"),
        (MessageRole::ToolResult, Some(name)) => format!("tool result: {name}"),
        (role, _) => role.to_string(),
    };
    push_heading(output, &label, message.timestamp);

    if matches!(
        message.role,
        MessageRole::ToolCall | MessageRole::ToolResult
    ) {
        push_block(output, "", "text", &message.content);
    } else {
        push_body(output, &message.content);
    }
}

/// Cell visibility mirrors `preview::render_cell_into`: `hide_tool_calls` drops
/// whole exec, patch, and web-search cells (they are tool invocations), while
/// `hide_tool_results` drops tool results outright and suppresses the output
/// streams inside exec and patch cells.
fn push_cell(
    output: &mut String,
    cell: &SessionCell,
    display_options: DisplayOptions,
    context: &mut CellContext,
) {
    match cell {
        SessionCell::Message {
            role,
            content,
            timestamp,
        } => {
            if hides_message(display_options, *role, content) {
                return;
            }
            push_heading(output, role.as_str(), *timestamp);
            push_body(output, content);
        }
        SessionCell::Reasoning {
            header,
            body,
            timestamp,
        } => {
            if display_options.hide_agent_replies {
                return;
            }
            let label = match header.as_deref().map(str::trim) {
                Some(header) if !header.is_empty() => format!("reasoning: {header}"),
                _ => "reasoning".to_owned(),
            };
            push_heading(output, &label, *timestamp);
            push_body(output, body);
        }
        SessionCell::ToolCall {
            tool,
            raw_name,
            summary,
            input,
            status,
            timestamp,
        } => {
            if display_options.hide_tool_calls {
                return;
            }
            push_heading(output, &format!("tool call: {tool}"), *timestamp);
            push_meta(
                output,
                &[
                    ("Status", tool_status_label(*status).to_owned()),
                    (
                        "Raw name",
                        if raw_name == tool {
                            String::new()
                        } else {
                            inline_code(raw_name)
                        },
                    ),
                ],
            );
            push_block(output, "Call", "text", summary);
            if let Some(input) = pretty_json(input) {
                push_block(output, "Input", "json", &input);
            }
            context.last_call_summary = Some(summary.clone());
        }
        SessionCell::ToolResult {
            tool,
            output: result,
            is_error,
            call_summary,
            timestamp,
        } => {
            if display_options.hide_tool_results {
                return;
            }
            let label = match tool.as_deref() {
                Some(tool) => format!("tool result: {tool}"),
                None => "tool result".to_owned(),
            };
            push_heading(output, &label, *timestamp);
            push_meta(
                output,
                &[("Status", if *is_error { "error" } else { "ok" }.to_owned())],
            );
            let call_summary = call_summary.as_deref().unwrap_or_default();
            if context.last_call_summary.as_deref() != Some(call_summary) {
                push_block(output, "Call", "text", call_summary);
            }
            push_block(output, "Output", "text", result);
            context.last_call_summary = None;
        }
        SessionCell::Exec {
            command,
            cwd,
            parsed_summary,
            stdout,
            stderr,
            exit_code,
            duration_ms,
            status,
            timestamp,
        } => {
            if display_options.hide_tool_calls {
                return;
            }
            push_heading(output, "exec", *timestamp);
            push_meta(
                output,
                &[
                    ("Status", exec_status_label(*status).to_owned()),
                    (
                        "Exit code",
                        exit_code.map(|code| code.to_string()).unwrap_or_default(),
                    ),
                    (
                        "Duration",
                        duration_ms
                            .filter(|milliseconds| *milliseconds > 0)
                            .map(format_duration)
                            .unwrap_or_default(),
                    ),
                    (
                        "Directory",
                        cwd.as_deref().map(inline_code).unwrap_or_default(),
                    ),
                ],
            );

            // The parsed summary is an alternative rendering of the same
            // command, so it earns a block only when it says something else.
            let command_line = flatten_command(command);
            push_block(output, "Command", "shell", &command_line);
            let summary = parsed_summary.as_deref().unwrap_or_default();
            if summary.trim() != command_line.trim() {
                push_block(output, "Summary", "text", summary);
            }
            if !display_options.hide_tool_results {
                push_block(output, "stdout", "text", stdout);
                push_block(output, "stderr", "text", stderr);
            }
        }
        SessionCell::Patch {
            files,
            success,
            stdout,
            stderr,
            timestamp,
        } => {
            if display_options.hide_tool_calls {
                return;
            }
            push_heading(output, "patch", *timestamp);
            let additions: usize = files.iter().map(|file| file.additions).sum();
            let deletions: usize = files.iter().map(|file| file.deletions).sum();
            push_meta(
                output,
                &[
                    (
                        "Status",
                        if *success { "applied" } else { "failed" }.to_owned(),
                    ),
                    (
                        "Changes",
                        if additions == 0 && deletions == 0 {
                            String::new()
                        } else {
                            format!("+{additions} -{deletions}")
                        },
                    ),
                ],
            );

            for file in files {
                let operation = match file.op {
                    PatchOp::Add => "add",
                    PatchOp::Update => "update",
                    PatchOp::Delete => "delete",
                };
                output.push_str(&format!(
                    "- **{operation}** {} (+{} -{})\n",
                    inline_code(&file.path),
                    file.additions,
                    file.deletions
                ));
            }
            if !files.is_empty() {
                output.push('\n');
            }

            for file in files {
                let Some(content) = file
                    .content
                    .as_deref()
                    .filter(|content| !content.trim().is_empty())
                else {
                    continue;
                };
                output.push_str(&format!("**{} after change**\n\n", inline_code(&file.path)));
                push_fenced(output, "text", content);
            }

            if !display_options.hide_tool_results {
                push_block(output, "stdout", "text", stdout);
                push_block(output, "stderr", "text", stderr);
            }
        }
        SessionCell::WebSearch {
            query,
            queries,
            timestamp,
        } => {
            if display_options.hide_tool_calls {
                return;
            }
            push_heading(output, "web search", *timestamp);
            let mut seen: Vec<&str> = Vec::new();
            for candidate in std::iter::once(query.as_str())
                .chain(queries.iter().map(String::as_str))
                .map(str::trim)
            {
                if !candidate.is_empty() && !seen.contains(&candidate) {
                    seen.push(candidate);
                }
            }
            for candidate in &seen {
                output.push_str(&format!("- {}\n", inline_code(candidate)));
            }
            if !seen.is_empty() {
                output.push('\n');
            }
        }
        SessionCell::Plan { items, timestamp } => {
            push_heading(output, "plan", *timestamp);
            for item in items {
                let marker = match item.status {
                    PlanItemStatus::Completed => "[x]",
                    PlanItemStatus::InProgress => "[~]",
                    PlanItemStatus::Pending => "[ ]",
                };
                output.push_str(&format!("- {marker} {}\n", item.step.trim()));
            }
            if !items.is_empty() {
                output.push('\n');
            }
        }
        SessionCell::SessionInfo(info) => {
            if !info.is_empty() {
                push_session_info(output, info);
            }
        }
        SessionCell::Metrics(metrics) => {
            if !metrics.is_empty() {
                push_metrics(output, metrics);
            }
        }
    }
}

fn push_session_info(output: &mut String, info: &SessionInfo) {
    output.push_str("## session info\n\n");
    push_meta(
        output,
        &[
            ("Model", info.model.clone().unwrap_or_default()),
            ("Provider", info.model_provider.clone().unwrap_or_default()),
            (
                "Reasoning effort",
                info.reasoning_effort.clone().unwrap_or_default(),
            ),
            (
                "Approval policy",
                info.approval_policy.clone().unwrap_or_default(),
            ),
            ("Sandbox", info.sandbox_mode.clone().unwrap_or_default()),
            (
                "Working directory",
                info.cwd.as_deref().map(inline_code).unwrap_or_default(),
            ),
            ("CLI version", info.cli_version.clone().unwrap_or_default()),
            ("Source", info.source.clone().unwrap_or_default()),
            ("Originator", info.originator.clone().unwrap_or_default()),
            (
                "Writable roots",
                info.writable_roots
                    .iter()
                    .map(|root| inline_code(root))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            (
                "Network access",
                info.network_access
                    .map(|allowed| allowed.to_string())
                    .unwrap_or_default(),
            ),
        ],
    );
    push_block(
        output,
        "Instructions",
        "text",
        info.instructions.as_deref().unwrap_or_default(),
    );
}

fn push_metrics(output: &mut String, metrics: &RuntimeMetrics) {
    output.push_str("## metrics\n\n");
    push_meta(
        output,
        &[
            ("Input tokens", non_zero(metrics.input_tokens)),
            ("Cached input tokens", non_zero(metrics.cached_input_tokens)),
            ("Output tokens", non_zero(metrics.output_tokens)),
            (
                "Reasoning output tokens",
                non_zero(metrics.reasoning_output_tokens),
            ),
            ("Total tokens", non_zero(metrics.total_tokens)),
            (
                "Model context window",
                metrics
                    .model_context_window
                    .map(|window| window.to_string())
                    .unwrap_or_default(),
            ),
            ("Tool calls", non_zero(metrics.tool_call_count)),
            ("Tool failures", non_zero(metrics.tool_failure_count)),
            ("Exec calls", non_zero(metrics.exec_count)),
            ("Patches", non_zero(metrics.patch_count)),
            ("Web searches", non_zero(metrics.web_search_count)),
            (
                "Wall time",
                metrics
                    .total_wall_ms
                    .map(format_duration)
                    .unwrap_or_default(),
            ),
        ],
    );
}

fn push_heading(output: &mut String, label: &str, timestamp: Option<DateTime<Utc>>) {
    output.push_str("## ");
    output.push_str(label);
    if let Some(timestamp) = timestamp {
        output.push_str(" — ");
        output.push_str(&format_timestamp(timestamp));
    }
    output.push_str("\n\n");
}

/// Emit a message body verbatim. It is already Markdown; reformatting it would
/// only corrupt code blocks and indentation.
fn push_body(output: &mut String, body: &str) {
    let body = body.trim_end();
    if body.is_empty() {
        return;
    }
    output.push_str(body);
    output.push_str("\n\n");
}

/// Emit a captioned fenced block, skipping it entirely when the payload is blank.
fn push_block(output: &mut String, caption: &str, info: &str, body: &str) {
    if body.trim().is_empty() {
        return;
    }
    if !caption.is_empty() {
        output.push_str(&format!("**{caption}**\n\n"));
    }
    push_fenced(output, info, body);
}

fn push_fenced(output: &mut String, info: &str, body: &str) {
    let fence = fence_for(body);
    output.push_str(&fence);
    output.push_str(info);
    output.push('\n');
    output.push_str(body.trim_end_matches('\n'));
    output.push('\n');
    output.push_str(&fence);
    output.push_str("\n\n");
}

/// Pick a fence longer than any backtick run that opens a line inside `body`, so
/// output containing its own fenced blocks cannot close ours early.
fn fence_for(body: &str) -> String {
    let longest = body
        .lines()
        .map(|line| {
            line.trim_start()
                .chars()
                .take_while(|character| *character == '`')
                .count()
        })
        .max()
        .unwrap_or(0);
    "`".repeat(longest.saturating_add(1).max(3))
}

/// Wrap `value` in backticks, widening the delimiter when the value contains
/// backticks of its own.
fn inline_code(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }
    let longest = value
        .split(|character: char| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let ticks = "`".repeat(longest + 1);
    if value.starts_with('`') || value.ends_with('`') {
        format!("{ticks} {value} {ticks}")
    } else {
        format!("{ticks}{value}{ticks}")
    }
}

/// Emit non-empty rows as a bullet list. Callers pass an empty value for fields
/// the session did not record.
fn push_meta(output: &mut String, rows: &[(&str, String)]) {
    let mut wrote_any = false;
    for (label, value) in rows {
        if value.is_empty() {
            continue;
        }
        output.push_str(&format!("- **{label}:** {value}\n"));
        wrote_any = true;
    }
    if wrote_any {
        output.push('\n');
    }
}

fn pretty_json(input: &Value) -> Option<String> {
    let empty = match input {
        Value::Null => true,
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    };
    if empty {
        return None;
    }
    Some(serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string()))
}

/// Strip a leading shell wrapper so `bash -lc '…'` exports as the script itself.
/// Mirrors the preview's command flattening.
fn flatten_command(argv: &[String]) -> String {
    if argv.is_empty() {
        return String::new();
    }
    if argv.len() >= 3 && argv[0].ends_with("sh") && argv[1].starts_with('-') {
        return argv[2].clone();
    }
    argv.join(" ")
}

fn format_timestamp(timestamp: DateTime<Utc>) -> String {
    timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

fn format_duration(milliseconds: u64) -> String {
    if milliseconds < 1_000 {
        format!("{milliseconds}ms")
    } else if milliseconds < 60_000 {
        format!("{:.1}s", milliseconds as f64 / 1000.0)
    } else {
        let seconds = milliseconds / 1000;
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    }
}

fn non_zero(value: u64) -> String {
    if value == 0 {
        String::new()
    } else {
        value.to_string()
    }
}

fn tool_status_label(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Pending => "pending",
        ToolStatus::Completed => "completed",
        ToolStatus::Failed => "failed",
    }
}

fn exec_status_label(status: ExecStatus) -> &'static str {
    match status {
        ExecStatus::Pending => "pending",
        ExecStatus::Completed => "completed",
        ExecStatus::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use chrono::TimeZone;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::parse::{Agent, DerivationType, PatchFile};

    fn timestamp(hour: u32) -> Option<DateTime<Utc>> {
        Some(Utc.with_ymd_and_hms(2026, 7, 14, hour, 30, 0).unwrap())
    }

    fn sample_session() -> Session {
        Session {
            session_id: "a3f21c8e-4b02-49d1-9f30-2c7b155e0a11".to_owned(),
            agent: Agent::Claude,
            project: "/tmp/demo".to_owned(),
            branch: Some("main".to_owned()),
            cwd: Some("/tmp/demo".to_owned()),
            created: timestamp(12),
            modified: timestamp(13),
            modified_ts: 0,
            lines: 1,
            file_path: PathBuf::from("/tmp/demo/session.jsonl"),
            first_msg_role: None,
            first_msg_content: String::new(),
            last_msg_role: None,
            last_msg_content: String::new(),
            first_user_msg_content: String::new(),
            derivation_type: DerivationType::Original,
            is_sidechain: false,
            custom_title: None,
            messages: Vec::new(),
            content: String::new(),
            cells: Vec::new(),
            session_info: None,
            lineage: Default::default(),
        }
    }

    #[test]
    fn export_stem_rejects_windows_reserved_titles() {
        let mut session = sample_session();
        session.custom_title = Some("NUL".to_owned());

        let error = export_stem_for_session(&session).expect_err("reserved title");

        assert!(format!("{error:#}").contains("not Windows-safe"));
    }

    #[test]
    fn export_uses_create_new_to_avoid_overwriting_existing_files() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("session-export.txt"), "existing").unwrap();

        let path = write_unique(temp.path(), "session-export", "txt", "new contents").unwrap();

        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            "session-export-1.txt"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("session-export.txt")).unwrap(),
            "existing"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "new contents");
    }

    #[test]
    fn markdown_stem_carries_date_agent_and_short_id() {
        let session = sample_session();

        let stem = markdown_export_stem(&session);

        assert!(stem.ends_with("-claude-a3f21c8e"), "unexpected stem {stem}");
    }

    #[test]
    fn markdown_stem_dates_by_modification_so_it_matches_the_date_filters() {
        let mut session = sample_session();
        // Created in May, still being continued in July: `--after 2026-07-01`
        // selects it on modified_ts, so the name must not say May.
        session.created = Some(Utc.with_ymd_and_hms(2026, 5, 27, 12, 0, 0).unwrap());
        session.modified = Some(Utc.with_ymd_and_hms(2026, 7, 20, 12, 0, 0).unwrap());

        assert_eq!(markdown_export_stem(&session), "2026-07-20-claude-a3f21c8e");
    }

    #[test]
    fn markdown_stem_falls_back_to_the_creation_date_when_unmodified() {
        let mut session = sample_session();
        session.modified = None;

        assert_eq!(markdown_export_stem(&session), "2026-07-14-claude-a3f21c8e");
    }

    #[test]
    fn markdown_stem_appends_slugified_custom_title() {
        let mut session = sample_session();
        session.custom_title = Some("  tmux TUI harness / rework!  ".to_owned());

        let stem = markdown_export_stem(&session);

        assert!(
            stem.ends_with("-claude-a3f21c8e-tmux-tui-harness-rework"),
            "unexpected stem {stem}"
        );
    }

    #[test]
    fn markdown_stem_falls_back_when_the_session_has_no_timestamps() {
        let mut session = sample_session();
        session.created = None;
        session.modified = None;
        session.session_id = "!!!".to_owned();

        assert_eq!(markdown_export_stem(&session), "undated-claude-session");
    }

    #[test]
    fn markdown_writes_message_bodies_verbatim() {
        let mut session = sample_session();
        session.cells = vec![SessionCell::Message {
            role: MessageRole::Assistant,
            content: "## Findings\n\n- one\n- two".to_owned(),
            timestamp: timestamp(12),
        }];

        let rendered = session_to_markdown(&session);

        assert!(rendered.contains("## assistant — 2026-07-14 12:30:00 UTC"));
        assert!(rendered.contains("## Findings\n\n- one\n- two"));
    }

    #[test]
    fn markdown_widens_fences_around_output_containing_code_blocks() {
        let mut session = sample_session();
        session.cells = vec![SessionCell::ToolResult {
            tool: Some("bash".to_owned()),
            output: "```rust\nfn main() {}\n```".to_owned(),
            is_error: false,
            call_summary: None,
            timestamp: None,
        }];

        let rendered = session_to_markdown(&session);

        // A three-backtick fence would be closed by the payload's own fence.
        assert!(rendered.contains("````text\n```rust\nfn main() {}\n```\n````"));
    }

    #[test]
    fn markdown_renders_exec_cells_with_streams_and_exit_code() {
        let mut session = sample_session();
        session.cells = vec![SessionCell::Exec {
            command: vec!["bash".to_owned(), "-lc".to_owned(), "ls -la".to_owned()],
            cwd: Some("/tmp/demo".to_owned()),
            parsed_summary: None,
            stdout: "total 0".to_owned(),
            stderr: "warning: none".to_owned(),
            exit_code: Some(2),
            duration_ms: Some(1_500),
            status: ExecStatus::Failed,
            timestamp: None,
        }];

        let rendered = session_to_markdown(&session);

        assert!(rendered.contains("## exec"));
        assert!(rendered.contains("- **Status:** failed"));
        assert!(rendered.contains("- **Exit code:** 2"));
        assert!(rendered.contains("- **Duration:** 1.5s"));
        // The shell wrapper is stripped so the script itself is what shows up.
        assert!(rendered.contains("```shell\nls -la\n```"));
        assert!(rendered.contains("**stdout**\n\n```text\ntotal 0\n```"));
        assert!(rendered.contains("**stderr**\n\n```text\nwarning: none\n```"));
    }

    #[test]
    fn markdown_drops_an_exec_summary_that_only_repeats_the_command() {
        let mut session = sample_session();
        session.cells = vec![SessionCell::Exec {
            command: vec!["bash".to_owned(), "-lc".to_owned(), "ls -la".to_owned()],
            cwd: None,
            parsed_summary: Some("ls -la".to_owned()),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
            duration_ms: Some(0),
            status: ExecStatus::Completed,
            timestamp: None,
        }];

        let rendered = session_to_markdown(&session);

        assert!(!rendered.contains("**Summary**"));
        // A zero duration means "not measured", not "instant".
        assert!(!rendered.contains("Duration"));
        assert!(rendered.contains("- **Exit code:** 0"));
    }

    #[test]
    fn markdown_drops_a_result_call_block_that_repeats_the_call_above_it() {
        let mut session = sample_session();
        session.cells = vec![
            SessionCell::ToolCall {
                tool: "bash".to_owned(),
                raw_name: "bash".to_owned(),
                summary: "cargo test".to_owned(),
                input: Value::Null,
                status: ToolStatus::Completed,
                timestamp: None,
            },
            SessionCell::ToolResult {
                tool: Some("bash".to_owned()),
                output: "ok".to_owned(),
                is_error: false,
                call_summary: Some("cargo test".to_owned()),
                timestamp: None,
            },
        ];

        let rendered = session_to_markdown(&session);

        assert_eq!(rendered.matches("**Call**").count(), 1);
        assert_eq!(rendered.matches("cargo test").count(), 1);
        assert!(rendered.contains("**Output**\n\n```text\nok\n```"));
    }

    #[test]
    fn markdown_keeps_a_result_call_block_that_differs_from_the_call_above_it() {
        let mut session = sample_session();
        session.cells = vec![
            SessionCell::ToolCall {
                tool: "bash".to_owned(),
                raw_name: "bash".to_owned(),
                summary: "cargo test".to_owned(),
                input: Value::Null,
                status: ToolStatus::Completed,
                timestamp: None,
            },
            SessionCell::ToolResult {
                tool: Some("bash".to_owned()),
                output: "ok".to_owned(),
                is_error: false,
                call_summary: Some("cargo build".to_owned()),
                timestamp: None,
            },
        ];

        let rendered = session_to_markdown(&session);

        assert_eq!(rendered.matches("**Call**").count(), 2);
        assert!(rendered.contains("cargo build"));
    }

    #[test]
    fn markdown_renders_patch_cells_with_per_file_content() {
        let mut session = sample_session();
        session.cells = vec![SessionCell::Patch {
            files: vec![PatchFile {
                path: "src/main.rs".to_owned(),
                op: PatchOp::Update,
                content: Some("fn main() {}".to_owned()),
                additions: 3,
                deletions: 1,
            }],
            success: true,
            stdout: String::new(),
            stderr: String::new(),
            timestamp: None,
        }];

        let rendered = session_to_markdown(&session);

        assert!(rendered.contains("- **update** `src/main.rs` (+3 -1)"));
        assert!(rendered.contains("- **Changes:** +3 -1"));
        assert!(rendered.contains("**`src/main.rs` after change**"));
        assert!(rendered.contains("```text\nfn main() {}\n```"));
    }

    #[test]
    fn markdown_falls_back_to_messages_when_no_cells_were_parsed() {
        let mut session = sample_session();
        session.messages = vec![
            SessionMessage {
                role: MessageRole::User,
                content: "run the tests".to_owned(),
                timestamp: timestamp(12),
                tool_name: None,
            },
            SessionMessage {
                role: MessageRole::ToolResult,
                content: "# not a heading".to_owned(),
                timestamp: None,
                tool_name: Some("Bash".to_owned()),
            },
        ];

        let rendered = session_to_markdown(&session);

        assert!(rendered.contains("## user — 2026-07-14 12:30:00 UTC"));
        assert!(rendered.contains("run the tests"));
        // Tool payloads are fenced, so a leading `#` cannot become a heading.
        assert!(rendered.contains("## tool result: Bash"));
        assert!(rendered.contains("```text\n# not a heading\n```"));
    }

    #[test]
    fn markdown_header_carries_session_provenance() {
        let session = sample_session();

        let rendered = session_to_markdown(&session);

        assert!(rendered.starts_with("# a3f21c8e-4b02-49d1-9f30-2c7b155e0a11\n\n"));
        assert!(rendered.contains("- **Agent:** claude"));
        assert!(rendered.contains("- **Branch:** `main`"));
        assert!(rendered.contains("- **Source:** `/tmp/demo/session.jsonl`"));
        assert!(rendered.contains("- **Created:** 2026-07-14 12:30:00 UTC"));
    }

    #[test]
    fn markdown_omits_the_session_info_block_when_a_cell_already_carries_it() {
        let mut session = sample_session();
        let info = SessionInfo {
            model: Some("claude-opus-5".to_owned()),
            ..SessionInfo::default()
        };
        session.session_info = Some(info.clone());
        session.cells = vec![SessionCell::SessionInfo(info)];

        let rendered = session_to_markdown(&session);

        assert_eq!(rendered.matches("## session info").count(), 1);
        assert!(rendered.contains("- **Model:** claude-opus-5"));
    }

    #[test]
    fn markdown_pretty_prints_tool_call_input() {
        let mut session = sample_session();
        session.cells = vec![SessionCell::ToolCall {
            tool: "bash".to_owned(),
            raw_name: "Bash".to_owned(),
            summary: "cargo test".to_owned(),
            input: json!({"command": "cargo test"}),
            status: ToolStatus::Completed,
            timestamp: None,
        }];

        let rendered = session_to_markdown(&session);

        assert!(rendered.contains("## tool call: bash"));
        assert!(rendered.contains("- **Raw name:** `Bash`"));
        assert!(rendered.contains("**Input**\n\n```json\n{\n  \"command\": \"cargo test\"\n}\n```"));
    }

    #[test]
    fn markdown_skips_empty_tool_call_input() {
        assert_eq!(pretty_json(&json!({})), None);
        assert_eq!(pretty_json(&Value::Null), None);
        assert!(pretty_json(&json!({"a": 1})).is_some());
    }

    #[test]
    fn inline_code_widens_delimiters_around_embedded_backticks() {
        assert_eq!(inline_code("plain"), "`plain`");
        assert_eq!(inline_code("a`b"), "``a`b``");
        assert_eq!(inline_code("`lead"), "`` `lead ``");
        assert_eq!(inline_code("  "), "");
    }

    /// One cell of every kind `--hide` can touch, so each test can assert what
    /// survives rather than what a single narrow fixture happens to contain.
    fn session_with_every_cell_kind() -> Session {
        let mut session = sample_session();
        session.cells = vec![
            SessionCell::Message {
                role: MessageRole::User,
                content: "run the tests".to_owned(),
                timestamp: None,
            },
            SessionCell::Message {
                role: MessageRole::Assistant,
                content: "on it".to_owned(),
                timestamp: None,
            },
            SessionCell::Reasoning {
                header: None,
                body: "thinking about it".to_owned(),
                timestamp: None,
            },
            SessionCell::ToolCall {
                tool: "bash".to_owned(),
                raw_name: "bash".to_owned(),
                summary: "cargo test".to_owned(),
                input: Value::Null,
                status: ToolStatus::Completed,
                timestamp: None,
            },
            SessionCell::ToolResult {
                tool: Some("bash".to_owned()),
                output: "all green".to_owned(),
                is_error: false,
                call_summary: None,
                timestamp: None,
            },
            SessionCell::Exec {
                command: vec!["ls".to_owned()],
                cwd: None,
                parsed_summary: None,
                stdout: "exec stdout".to_owned(),
                stderr: String::new(),
                exit_code: Some(0),
                duration_ms: None,
                status: ExecStatus::Completed,
                timestamp: None,
            },
            SessionCell::Patch {
                files: Vec::new(),
                success: true,
                stdout: "patch stdout".to_owned(),
                stderr: String::new(),
                timestamp: None,
            },
            SessionCell::WebSearch {
                query: "rust fences".to_owned(),
                queries: Vec::new(),
                timestamp: None,
            },
        ];
        session
    }

    #[test]
    fn show_all_keeps_every_cell_kind() {
        let rendered = session_to_markdown_with_options(
            &session_with_every_cell_kind(),
            DisplayOptions::SHOW_ALL,
        );

        for expected in [
            "run the tests",
            "on it",
            "thinking about it",
            "## tool call: bash",
            "all green",
            "exec stdout",
            "patch stdout",
            "## web search",
        ] {
            assert!(rendered.contains(expected), "missing {expected}");
        }
    }

    #[test]
    fn hiding_tool_calls_drops_exec_patch_and_web_search_too() {
        let options = DisplayOptions {
            hide_tool_calls: true,
            ..DisplayOptions::SHOW_ALL
        };

        let rendered = session_to_markdown_with_options(&session_with_every_cell_kind(), options);

        assert!(!rendered.contains("## tool call"));
        assert!(!rendered.contains("## exec"));
        assert!(!rendered.contains("## patch"));
        assert!(!rendered.contains("## web search"));
        // Results are a separate toggle, and conversation is untouched.
        assert!(rendered.contains("all green"));
        assert!(rendered.contains("run the tests"));
    }

    #[test]
    fn hiding_tool_results_also_suppresses_exec_and_patch_streams() {
        let options = DisplayOptions {
            hide_tool_results: true,
            ..DisplayOptions::SHOW_ALL
        };

        let rendered = session_to_markdown_with_options(&session_with_every_cell_kind(), options);

        assert!(!rendered.contains("## tool result"));
        assert!(!rendered.contains("exec stdout"));
        assert!(!rendered.contains("patch stdout"));
        // The invocations themselves survive: only their output is gone.
        assert!(rendered.contains("## exec"));
        assert!(rendered.contains("## patch"));
        assert!(rendered.contains("## tool call: bash"));
    }

    #[test]
    fn hiding_agent_replies_drops_reasoning_cells() {
        let options = DisplayOptions {
            hide_agent_replies: true,
            ..DisplayOptions::SHOW_ALL
        };

        let rendered = session_to_markdown_with_options(&session_with_every_cell_kind(), options);

        assert!(!rendered.contains("on it"));
        assert!(!rendered.contains("thinking about it"));
        assert!(rendered.contains("run the tests"));
    }

    #[test]
    fn hiding_user_messages_keeps_the_rest_of_the_conversation() {
        let options = DisplayOptions {
            hide_user_messages: true,
            ..DisplayOptions::SHOW_ALL
        };

        let rendered = session_to_markdown_with_options(&session_with_every_cell_kind(), options);

        assert!(!rendered.contains("run the tests"));
        assert!(rendered.contains("on it"));
    }

    #[test]
    fn hiding_applies_to_the_messages_fallback_as_well() {
        let mut session = sample_session();
        session.messages = vec![
            SessionMessage {
                role: MessageRole::User,
                content: "run the tests".to_owned(),
                timestamp: None,
                tool_name: None,
            },
            SessionMessage {
                role: MessageRole::ToolResult,
                content: "all green".to_owned(),
                timestamp: None,
                tool_name: Some("Bash".to_owned()),
            },
        ];
        let options = DisplayOptions {
            hide_tool_results: true,
            ..DisplayOptions::SHOW_ALL
        };

        let rendered = session_to_markdown_with_options(&session, options);

        assert!(rendered.contains("run the tests"));
        assert!(!rendered.contains("all green"));
    }

    #[test]
    fn provenance_survives_even_when_everything_is_hidden() {
        let options = DisplayOptions {
            hide_skill_text_injection: true,
            hide_tool_calls: true,
            hide_tool_results: true,
            hide_agent_replies: true,
            hide_user_messages: true,
            hide_project_docs_autodump: true,
        };

        let rendered = session_to_markdown_with_options(&session_with_every_cell_kind(), options);

        assert!(rendered.contains("- **Agent:** claude"));
        assert!(rendered.contains("- **Source:** `/tmp/demo/session.jsonl`"));
        assert!(!rendered.contains("run the tests"));
    }

    #[test]
    fn write_session_markdown_creates_the_target_directory() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("nested/exports");
        let session = sample_session();

        let path = write_session_markdown(&target, &session, DisplayOptions::SHOW_ALL).unwrap();

        assert!(path.starts_with(&target));
        assert_eq!(path.extension().unwrap(), "md");
        assert!(fs::read_to_string(&path).unwrap().starts_with("# a3f21c8e"));
    }
}
