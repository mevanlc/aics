use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders};
use ratatui::Frame;

use crate::parse::{
    is_project_docs_autodump, is_skill_text_injection, Agent, ExecStatus, MessageRole, PatchFile,
    PatchOp, PlanItemStatus, RuntimeMetrics, Session, SessionCell, SessionInfo, ToolStatus,
};
use crate::settings::DisplayOptions;
use crate::summary::{
    AicsSummaryPreview, ClaudeAutosummaryPreview, SummaryPreview, SummarySources,
};
use crate::tui::app::App;
use crate::tui::markdown::{render_markdown_message, render_markdown_message_with_headings};
use crate::tui::profile;
use crate::tui::theme::Theme;
use crate::tui::util::{
    block_title, right_block_title, session_message_label, wrapped_text_height,
    FullLineBackgroundParagraph, StickyHeader, StickyHeaderWidget, StickyLineMarker,
    STICKY_HEADER_HEIGHT,
};

#[derive(Debug, Clone)]
pub struct DisplayDocument {
    pub text: Text<'static>,
    pub sticky_markers: Vec<StickyLineMarker>,
}

#[derive(Debug, Clone, Copy)]
pub struct SessionRenderOptions {
    pub display_options: DisplayOptions,
    pub hide_project_docs_autodump: bool,
}

impl SessionRenderOptions {
    pub fn new(display_options: DisplayOptions) -> Self {
        Self {
            display_options,
            hide_project_docs_autodump: display_options.hide_project_docs_autodump,
        }
    }
}

impl Default for SessionRenderOptions {
    fn default() -> Self {
        Self::new(DisplayOptions::default())
    }
}

pub fn render(frame: &mut Frame, app: &mut App, area: Rect, theme: &Theme) {
    let _profile = profile::scope("preview.render");
    let (mut text, max_scroll, active_match_row, sticky_header) =
        if let Some(state) = app.preview_render_state(area, theme) {
            (
                state.text.clone(),
                state.max_scroll,
                state.active_match_row,
                state.sticky_header.clone(),
            )
        } else {
            (
                Text::from(Line::from(Span::styled(
                    "Select a session to preview",
                    Style::default().fg(theme.muted),
                ))),
                0,
                None,
                None,
            )
        };
    app.preview_scroll = app.preview_scroll.min(max_scroll);
    if let Some(row) = active_match_row {
        let width = area.width.saturating_sub(2);
        crate::tui::viewer::highlight_active_match(&mut text, row, width, theme);
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border_style(false))
        .title(block_title(Line::from(vec![
            Span::styled(app.preview_title(), Style::default().fg(theme.accent)),
            Span::styled(" (^T)", Style::default().fg(theme.muted)),
        ])))
        .title(right_block_title(Line::from(Span::styled(
            "PgUp/PgDn",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))));

    let inner = block.inner(area);
    frame.render_widget(block, area);
    let (header_area, body_area) = split_sticky_body(inner);
    frame.render_widget(
        StickyHeaderWidget::new(sticky_header.as_ref(), theme),
        header_area,
    );
    frame.render_widget(
        FullLineBackgroundParagraph::new(text).scroll(app.preview_scroll),
        body_area,
    );
}

pub fn max_scroll(area: Rect, session: Option<&Session>, theme: &Theme, query: &str) -> usize {
    let highlight_query = normalize_highlight_query(query);
    let text = if let Some(session) = session {
        render_session_text(session, theme, highlight_query)
    } else {
        Text::from(Line::default())
    };
    scroll_limit_for_text(&text, area)
}

pub fn render_session_text(
    session: &Session,
    theme: &Theme,
    highlight_query: Option<&str>,
) -> Text<'static> {
    render_session_text_with_options(session, theme, highlight_query, DisplayOptions::default())
}

pub fn render_session_text_with_options(
    session: &Session,
    theme: &Theme,
    highlight_query: Option<&str>,
    display_options: DisplayOptions,
) -> Text<'static> {
    render_session_document_with_options(
        session,
        theme,
        highlight_query,
        SessionRenderOptions::new(display_options),
    )
    .text
}

pub fn render_session_document(
    session: &Session,
    theme: &Theme,
    highlight_query: Option<&str>,
) -> DisplayDocument {
    render_session_document_with_options(
        session,
        theme,
        highlight_query,
        SessionRenderOptions::default(),
    )
}

pub fn render_session_document_with_options(
    session: &Session,
    theme: &Theme,
    highlight_query: Option<&str>,
    options: SessionRenderOptions,
) -> DisplayDocument {
    let _profile = profile::scope("preview.render_session_text");
    let mut lines = Vec::new();
    let mut sticky_markers = Vec::new();

    if let Some(info) = session.session_info.as_ref() {
        let info_lines = render_session_info_block(info, theme, highlight_query);
        if !info_lines.is_empty() {
            sticky_markers.push(StickyLineMarker {
                line_index: lines.len(),
                header: StickyHeader::new("Session", String::new(), "Context"),
            });
            lines.extend(info_lines);
            lines.push(Line::default());
        }
    }

    if session.cells.is_empty() {
        // Backwards compatibility: if no cells were emitted, fall back to messages.
        for message in &session.messages {
            if should_hide_message(message.role, &message.content, options) {
                continue;
            }
            render_message_into(
                &mut lines,
                &mut sticky_markers,
                session.agent,
                message,
                theme,
                highlight_query,
            );
        }
    } else {
        for cell in &session.cells {
            if should_hide_cell(cell, options) {
                continue;
            }
            // SessionInfo is rendered separately above.
            if matches!(cell, SessionCell::SessionInfo(_)) {
                continue;
            }
            // Metrics are rendered as a footer (Phase 6); skip in main flow.
            if matches!(cell, SessionCell::Metrics(_)) {
                continue;
            }
            render_cell_into(
                &mut lines,
                &mut sticky_markers,
                session.agent,
                cell,
                theme,
                highlight_query,
                options.display_options,
            );
        }

        if let Some(metrics) = session.cells.iter().rev().find_map(|cell| match cell {
            SessionCell::Metrics(metrics) => Some(metrics),
            _ => None,
        }) {
            let metrics_lines = render_metrics_footer(metrics, theme);
            if !metrics_lines.is_empty() {
                sticky_markers.push(StickyLineMarker {
                    line_index: lines.len(),
                    header: StickyHeader::new("Metrics", String::new(), "Totals"),
                });
                lines.extend(metrics_lines);
            }
        }
    }
    DisplayDocument {
        text: Text::from(lines),
        sticky_markers,
    }
}

fn should_hide_message(role: MessageRole, content: &str, options: SessionRenderOptions) -> bool {
    !shows_message_role(options.display_options, role)
        || options.hide_project_docs_autodump && is_project_docs_autodump(role, content)
        || options.display_options.hide_skill_text_injection
            && is_skill_text_injection(role, content)
}

fn should_hide_cell(cell: &SessionCell, options: SessionRenderOptions) -> bool {
    match cell {
        SessionCell::Message { role, content, .. } => should_hide_message(*role, content, options),
        _ => false,
    }
}

pub fn render_composite_text(
    session: Option<&Session>,
    summaries: &SummarySources,
    theme: &Theme,
    highlight_query: Option<&str>,
    summary_inflight: bool,
) -> Text<'static> {
    render_composite_document(session, summaries, theme, highlight_query, summary_inflight).text
}

pub fn render_composite_document(
    session: Option<&Session>,
    summaries: &SummarySources,
    theme: &Theme,
    highlight_query: Option<&str>,
    summary_inflight: bool,
) -> DisplayDocument {
    render_composite_document_with_options(
        session,
        summaries,
        theme,
        highlight_query,
        summary_inflight,
        DisplayOptions::default(),
    )
}

pub fn render_composite_document_with_options(
    session: Option<&Session>,
    summaries: &SummarySources,
    theme: &Theme,
    highlight_query: Option<&str>,
    summary_inflight: bool,
    display_options: DisplayOptions,
) -> DisplayDocument {
    let mut summary =
        render_summary_sections_document(summaries, theme, highlight_query, summary_inflight);
    if !summary.text.lines.is_empty() && session.is_some() {
        summary.text.lines.push(Line::default());
    }

    let session_doc = render_session_section_document_with_options(
        session,
        theme,
        highlight_query,
        display_options,
    );
    let session_offset = summary.text.lines.len();
    summary.text.lines.extend(session_doc.text.lines);
    summary
        .sticky_markers
        .extend(
            session_doc
                .sticky_markers
                .into_iter()
                .map(|marker| StickyLineMarker {
                    line_index: marker.line_index + session_offset,
                    header: marker.header,
                }),
        );
    summary
}

pub fn render_summary_sections(
    summaries: &SummarySources,
    theme: &Theme,
    highlight_query: Option<&str>,
    summary_inflight: bool,
) -> Text<'static> {
    render_summary_sections_document(summaries, theme, highlight_query, summary_inflight).text
}

pub fn render_summary_sections_document(
    summaries: &SummarySources,
    theme: &Theme,
    highlight_query: Option<&str>,
    summary_inflight: bool,
) -> DisplayDocument {
    let mut lines = Vec::new();
    let mut sticky_markers = Vec::new();

    for (index, summary) in summaries.claude_autosummaries.iter().enumerate() {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        let title = if summaries.claude_autosummaries.len() > 1 {
            format!("# Claude Auto-summary #{}", index + 1)
        } else {
            "# Claude Auto-summary".to_owned()
        };
        let section_start = lines.len();
        let body = render_claude_summary_text(summary, theme, highlight_query);
        lines.extend(render_section(&title, &body, theme).lines);
        sticky_markers.push(StickyLineMarker {
            line_index: section_start,
            header: StickyHeader::new(
                "Claude autosummary",
                summary
                    .generated_at
                    .map(|generated_at| generated_at.format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or_default(),
                title.trim_start_matches("# "),
            ),
        });
        add_summary_heading_markers(
            &mut sticky_markers,
            &summary.body,
            theme,
            highlight_query,
            section_start + 2,
            StickyHeader::new(
                "Claude autosummary",
                summary
                    .generated_at
                    .map(|generated_at| generated_at.format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or_default(),
                "",
            ),
        );
    }

    if let Some(summary) = summaries.aics_sidecar.as_ref() {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        let section_start = lines.len();
        let body = render_aics_summary_text(summary, theme, highlight_query);
        lines.extend(render_section("# AICS summary", &body, theme).lines);
        let generated_at = summary
            .sidecar
            .generated_at
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        sticky_markers.push(StickyLineMarker {
            line_index: section_start,
            header: StickyHeader::new("AICS summary", generated_at.clone(), "AICS summary"),
        });
        add_summary_heading_markers(
            &mut sticky_markers,
            &summary.sidecar.body,
            theme,
            highlight_query,
            section_start + 2,
            StickyHeader::new("AICS summary", generated_at, ""),
        );
    } else if summary_inflight {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        let section_start = lines.len();
        lines.extend(
            render_section(
                "# AICS summary",
                &render_summary_missing(theme, "Summary is being generated…", false),
                theme,
            )
            .lines,
        );
        sticky_markers.push(StickyLineMarker {
            line_index: section_start,
            header: StickyHeader::new("AICS summary", "", "AICS summary"),
        });
    }

    DisplayDocument {
        text: Text::from(lines),
        sticky_markers,
    }
}

pub fn render_session_section(
    session: Option<&Session>,
    theme: &Theme,
    highlight_query: Option<&str>,
) -> Text<'static> {
    render_session_section_document(session, theme, highlight_query).text
}

pub fn render_session_section_document(
    session: Option<&Session>,
    theme: &Theme,
    highlight_query: Option<&str>,
) -> DisplayDocument {
    render_session_section_document_with_options(
        session,
        theme,
        highlight_query,
        DisplayOptions::default(),
    )
}

pub fn render_session_section_document_with_options(
    session: Option<&Session>,
    theme: &Theme,
    highlight_query: Option<&str>,
    display_options: DisplayOptions,
) -> DisplayDocument {
    let body = if let Some(session) = session {
        render_session_document_with_options(
            session,
            theme,
            highlight_query,
            SessionRenderOptions::new(display_options),
        )
    } else {
        DisplayDocument {
            text: render_summary_missing(
                theme,
                "Session log is unavailable for this entry.",
                false,
            ),
            sticky_markers: Vec::new(),
        }
    };
    let text = render_section("# Session Log", &body.text, theme);
    let mut sticky_markers = vec![StickyLineMarker {
        line_index: 0,
        header: StickyHeader::new("Session", "", "Session Log"),
    }];
    sticky_markers.extend(
        body.sticky_markers
            .into_iter()
            .map(|marker| StickyLineMarker {
                line_index: marker.line_index + 2,
                header: marker.header,
            }),
    );
    DisplayDocument {
        text,
        sticky_markers,
    }
}

pub(crate) fn shows_message_role(display_options: DisplayOptions, role: MessageRole) -> bool {
    match role {
        MessageRole::User => !display_options.hide_user_messages,
        MessageRole::Assistant => !display_options.hide_agent_replies,
        MessageRole::ToolCall => !display_options.hide_tool_calls,
        MessageRole::ToolResult => !display_options.hide_tool_results,
        MessageRole::System | MessageRole::Summary => true,
    }
}

fn render_section(title: &str, body: &Text<'static>, theme: &Theme) -> Text<'static> {
    let mut lines = Vec::with_capacity(body.lines.len() + 2);
    lines.push(Line::from(Span::styled(
        title.to_owned(),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::default());
    lines.extend(body.lines.clone());
    Text::from(lines)
}

/// Render a summary source into a `Text` suitable for the preview pane.
/// Prepends a single metadata line then the markdown body.
pub fn render_summary_text(
    summary: &SummaryPreview,
    theme: &Theme,
    highlight_query: Option<&str>,
) -> Text<'static> {
    let _profile = profile::scope("preview.render_summary_text");
    match summary {
        SummaryPreview::AicsSidecar(summary) => {
            render_aics_summary_text(summary, theme, highlight_query)
        }
        SummaryPreview::ClaudeAutosummary(summary) => {
            render_claude_summary_text(summary, theme, highlight_query)
        }
    }
}

/// Rendered when a requested summary source is unavailable.
pub fn render_summary_missing(
    theme: &Theme,
    message: &str,
    show_generate_hint: bool,
) -> Text<'static> {
    let mut lines = vec![
        Line::default(),
        Line::from(Span::styled(
            message.to_owned(),
            Style::default().fg(theme.muted),
        )),
        Line::default(),
    ];
    if show_generate_hint {
        lines.push(Line::from(Span::styled(
            "Press Enter → s to generate an AICS summary.",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )));
    }
    Text::from(lines)
}

fn render_aics_summary_text(
    summary: &AicsSummaryPreview,
    theme: &Theme,
    highlight_query: Option<&str>,
) -> Text<'static> {
    let sidecar = &summary.sidecar;
    let fingerprint = &summary.fingerprint;
    let base = Style::default().fg(theme.text);
    let fresh = sidecar.is_fresh(fingerprint);
    let (badge, badge_style) = if fresh {
        (
            "FRESH".to_owned(),
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        let added = fingerprint.line_count.saturating_sub(sidecar.line_count);
        let label = if added > 0 {
            format!("STALE · +{added} lines")
        } else {
            "STALE".to_owned()
        };
        (
            label,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
    };

    let header = Line::from(vec![
        Span::styled(
            "aics",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · ", Style::default().fg(theme.muted)),
        Span::styled(
            sidecar.backend.as_str().to_owned(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · ", Style::default().fg(theme.muted)),
        Span::styled(
            sidecar.generated_at.format("%Y-%m-%d %H:%M").to_string(),
            Style::default().fg(theme.muted),
        ),
        Span::styled(" · ", Style::default().fg(theme.muted)),
        Span::styled(badge, badge_style),
    ]);
    let body = render_markdown_message(&sidecar.body, theme, base, highlight_query);

    let mut lines = Vec::with_capacity(body.lines.len() + 2);
    lines.push(header);
    lines.push(Line::default());
    lines.extend(body.lines);
    Text::from(lines)
}

fn render_claude_summary_text(
    summary: &ClaudeAutosummaryPreview,
    theme: &Theme,
    highlight_query: Option<&str>,
) -> Text<'static> {
    let base = Style::default().fg(theme.text);
    let mut spans = vec![
        Span::styled(
            "built-in",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · ", Style::default().fg(theme.muted)),
        Span::styled(
            "claude autosummary",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(generated_at) = summary.generated_at {
        spans.push(Span::styled(" · ", Style::default().fg(theme.muted)));
        spans.push(Span::styled(
            generated_at.format("%Y-%m-%d %H:%M").to_string(),
            Style::default().fg(theme.muted),
        ));
    }
    let body = render_markdown_message(&summary.body, theme, base, highlight_query);

    let mut lines = Vec::with_capacity(body.lines.len() + 2);
    lines.push(Line::from(spans));
    lines.push(Line::default());
    lines.extend(body.lines);
    Text::from(lines)
}

pub(crate) fn render_message_body(
    agent: Agent,
    role: MessageRole,
    content: &str,
    theme: &Theme,
    highlight_query: Option<&str>,
) -> Text<'static> {
    render_message_body_document(agent, role, content, theme, highlight_query).text
}

pub(crate) fn render_message_body_document(
    agent: Agent,
    role: MessageRole,
    content: &str,
    theme: &Theme,
    highlight_query: Option<&str>,
) -> DisplayDocument {
    let (_, bubble_bg) = message_colors(agent, role, theme);
    let base = Style::default().fg(theme.text).bg(bubble_bg);
    let rendered = render_markdown_message_with_headings(content, theme, base, highlight_query);
    DisplayDocument {
        text: rendered.text,
        sticky_markers: rendered
            .headings
            .into_iter()
            .map(|heading| StickyLineMarker {
                line_index: heading.line_index,
                header: StickyHeader::new(
                    "",
                    "",
                    truncate_chars(&heading.breadcrumb, RESULT_SUBJECT_MAX_CHARS),
                ),
            })
            .collect(),
    }
}

/// Render the top-of-transcript Session context block.
///
/// Returns a list of lines (no trailing blank). Caller is responsible for
/// adding any spacing after the block.
fn render_session_info_block(
    info: &SessionInfo,
    theme: &Theme,
    highlight_query: Option<&str>,
) -> Vec<Line<'static>> {
    let mut rows: Vec<(&'static str, String)> = Vec::new();

    if let Some(model) = &info.model {
        let mut value = model.clone();
        if let Some(effort) = &info.reasoning_effort {
            value.push_str(" · ");
            value.push_str(effort);
        }
        if let Some(provider) = &info.model_provider {
            value.push_str(" (");
            value.push_str(provider);
            value.push(')');
        }
        rows.push(("model", value));
    } else if let Some(provider) = &info.model_provider {
        rows.push(("provider", provider.clone()));
    }

    if let Some(approval) = &info.approval_policy {
        let mut value = approval.clone();
        if let Some(sandbox) = &info.sandbox_mode {
            value.push_str(" · sandbox=");
            value.push_str(sandbox);
        }
        if let Some(false) = info.network_access {
            value.push_str(" · no-net");
        }
        rows.push(("policy", value));
    } else if let Some(sandbox) = &info.sandbox_mode {
        rows.push(("sandbox", sandbox.clone()));
    }

    if let Some(cwd) = &info.cwd {
        rows.push(("cwd", cwd.clone()));
    }

    let mut originator_bits: Vec<String> = Vec::new();
    if let Some(originator) = &info.originator {
        originator_bits.push(originator.clone());
    }
    if let Some(version) = &info.cli_version {
        originator_bits.push(format!("v{version}"));
    }
    if let Some(source) = &info.source {
        originator_bits.push(format!("via {source}"));
    }
    if !originator_bits.is_empty() {
        rows.push(("client", originator_bits.join(" ")));
    }

    if rows.is_empty() {
        return Vec::new();
    }

    let label_width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);

    let mut lines = Vec::with_capacity(rows.len() + 1);
    lines.push(Line::from(Span::styled(
        "── Session ──",
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    )));
    for (label, value) in rows {
        let mut spans = Vec::new();
        spans.push(Span::styled(
            format!("  {label:<width$}  ", width = label_width),
            Style::default().fg(theme.muted),
        ));
        spans.extend(highlight_into_spans(
            &value,
            highlight_query,
            Style::default().fg(theme.text),
            theme,
        ));
        lines.push(Line::from(spans));
    }

    lines
}

fn highlight_into_spans(
    text: &str,
    highlight_query: Option<&str>,
    base: Style,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let text = crate::tui::ansi::strip_terminal_escapes(text);
    let Some(query) = highlight_query.filter(|q| !q.is_empty()) else {
        return vec![Span::styled(text.to_owned(), base)];
    };

    let lower_text = text.to_lowercase();
    let lower_query = query.to_lowercase();
    let qlen = lower_query.len();
    if qlen == 0 {
        return vec![Span::styled(text.to_owned(), base)];
    }

    let mut spans = Vec::new();
    let mut cursor = 0usize;
    while let Some(found) = lower_text[cursor..].find(&lower_query) {
        let start = cursor + found;
        let end = start + qlen;
        if start > cursor {
            spans.push(Span::styled(text[cursor..start].to_owned(), base));
        }
        spans.push(Span::styled(
            text[start..end].to_owned(),
            base.bg(theme.search_match_bg).fg(theme.text),
        ));
        cursor = end;
    }
    if cursor < text.len() {
        spans.push(Span::styled(text[cursor..].to_owned(), base));
    }
    spans
}

/// Render a `SessionMessage` into the rolling lines/sticky list. Used as the
/// fallback path when cells are empty (legacy / Claude until cell-parity ships).
fn render_message_into(
    lines: &mut Vec<Line<'static>>,
    sticky_markers: &mut Vec<StickyLineMarker>,
    agent: Agent,
    message: &crate::parse::SessionMessage,
    theme: &Theme,
    highlight_query: Option<&str>,
) {
    let (label_color, _) = message_colors(agent, message.role, theme);
    let label = session_message_label(message);
    let header_line = lines.len();
    let base_header = sticky_header_for_message(agent, message);
    sticky_markers.push(StickyLineMarker {
        line_index: header_line,
        header: base_header.clone(),
    });
    lines.push(Line::from(vec![
        Span::styled(
            label,
            Style::default()
                .fg(label_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ", Style::default()),
        Span::styled(
            message
                .timestamp
                .map(|timestamp| timestamp.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default(),
            Style::default().fg(theme.muted),
        ),
    ]));

    let rendered = render_message_body_document(
        agent,
        message.role,
        message.content.as_str(),
        theme,
        highlight_query,
    );
    let body_start = lines.len();
    for heading in rendered.sticky_markers {
        let mut header = base_header.clone();
        header.subject = heading.header.subject;
        sticky_markers.push(StickyLineMarker {
            line_index: body_start + heading.line_index,
            header,
        });
    }
    lines.extend(rendered.text.lines);
    lines.push(Line::default());
}

/// Render a single cell into the rolling lines/sticky list.
fn render_cell_into(
    lines: &mut Vec<Line<'static>>,
    sticky_markers: &mut Vec<StickyLineMarker>,
    agent: Agent,
    cell: &SessionCell,
    theme: &Theme,
    highlight_query: Option<&str>,
    display_options: DisplayOptions,
) {
    match cell {
        SessionCell::Message {
            role,
            content,
            timestamp,
        } => {
            if !shows_message_role(display_options, *role) {
                return;
            }
            let synthetic = crate::parse::SessionMessage {
                role: *role,
                content: content.clone(),
                timestamp: *timestamp,
                tool_name: None,
            };
            render_message_into(
                lines,
                sticky_markers,
                agent,
                &synthetic,
                theme,
                highlight_query,
            );
        }
        SessionCell::Reasoning {
            header,
            body,
            timestamp,
        } => {
            if display_options.hide_agent_replies {
                return;
            }
            render_reasoning_into(
                lines,
                sticky_markers,
                header.as_deref(),
                body,
                *timestamp,
                theme,
                highlight_query,
            );
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
            render_tool_call_into(
                lines,
                sticky_markers,
                tool,
                raw_name,
                summary,
                input,
                *status,
                *timestamp,
                theme,
                highlight_query,
            );
        }
        SessionCell::ToolResult {
            tool,
            output,
            is_error,
            call_summary,
            timestamp,
        } => {
            if display_options.hide_tool_results {
                return;
            }
            render_tool_result_into(
                lines,
                sticky_markers,
                tool.as_deref(),
                output,
                *is_error,
                call_summary.as_deref(),
                *timestamp,
                theme,
                highlight_query,
            );
        }
        SessionCell::Exec {
            command,
            parsed_summary,
            stdout,
            stderr,
            exit_code,
            duration_ms,
            status,
            timestamp,
            ..
        } => {
            if display_options.hide_tool_calls {
                return;
            }
            render_exec_into(
                lines,
                sticky_markers,
                command,
                parsed_summary.as_deref(),
                stdout,
                stderr,
                *exit_code,
                *duration_ms,
                *status,
                *timestamp,
                theme,
                highlight_query,
                display_options,
            );
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
            render_patch_into(
                lines,
                sticky_markers,
                files,
                *success,
                stdout,
                stderr,
                *timestamp,
                theme,
                highlight_query,
                display_options,
            );
        }
        SessionCell::WebSearch {
            query,
            queries,
            timestamp,
        } => {
            if display_options.hide_tool_calls {
                return;
            }
            render_web_search_into(
                lines,
                sticky_markers,
                query,
                queries,
                *timestamp,
                theme,
                highlight_query,
            );
        }
        SessionCell::Plan { items, timestamp } => {
            render_plan_into(
                lines,
                sticky_markers,
                items,
                *timestamp,
                theme,
                highlight_query,
            );
        }
        SessionCell::SessionInfo(_) | SessionCell::Metrics(_) => {
            // Handled outside the main loop.
        }
    }
}

fn header_timestamp_string(timestamp: Option<chrono::DateTime<chrono::Utc>>) -> String {
    timestamp
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn push_cell_header(
    lines: &mut Vec<Line<'static>>,
    sticky_markers: &mut Vec<StickyLineMarker>,
    label: String,
    label_color: ratatui::style::Color,
    suffix: Option<String>,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
    sticky: StickyHeader,
    theme: &Theme,
) {
    sticky_markers.push(StickyLineMarker {
        line_index: lines.len(),
        header: sticky,
    });
    let mut spans = vec![Span::styled(
        label,
        Style::default()
            .fg(label_color)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(suffix) = suffix {
        spans.push(Span::styled(
            format!(" {suffix}"),
            Style::default().fg(theme.muted),
        ));
    }
    let ts = header_timestamp_string(timestamp);
    if !ts.is_empty() {
        spans.push(Span::styled(
            format!("  {ts}"),
            Style::default().fg(theme.muted),
        ));
    }
    lines.push(Line::from(spans));
}

#[allow(clippy::too_many_arguments)]
fn render_reasoning_into(
    lines: &mut Vec<Line<'static>>,
    sticky_markers: &mut Vec<StickyLineMarker>,
    header: Option<&str>,
    body: &str,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
    theme: &Theme,
    highlight_query: Option<&str>,
) {
    let label = match header {
        Some(h) if !h.is_empty() => format!("\u{21b3} thinking · {h}"),
        _ => "\u{21b3} thinking".to_owned(),
    };
    push_cell_header(
        lines,
        sticky_markers,
        label.clone(),
        theme.muted,
        None,
        timestamp,
        StickyHeader::new(
            "Reasoning",
            header_timestamp_string(timestamp),
            header.unwrap_or(""),
        ),
        theme,
    );
    let italic = Style::default()
        .fg(theme.muted)
        .add_modifier(Modifier::ITALIC);
    for line in body.lines() {
        let mut spans = Vec::new();
        spans.push(Span::styled("  ", italic));
        spans.extend(highlight_into_spans(line, highlight_query, italic, theme));
        lines.push(Line::from(spans));
    }
    lines.push(Line::default());
}

#[allow(clippy::too_many_arguments)]
fn render_tool_call_into(
    lines: &mut Vec<Line<'static>>,
    sticky_markers: &mut Vec<StickyLineMarker>,
    tool: &str,
    raw_name: &str,
    summary: &str,
    input: &serde_json::Value,
    status: ToolStatus,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
    theme: &Theme,
    highlight_query: Option<&str>,
) {
    let label = format!("\u{203a} {tool}");
    let suffix = match status {
        ToolStatus::Pending => Some("(pending)".to_owned()),
        ToolStatus::Failed => Some("(failed)".to_owned()),
        ToolStatus::Completed => None,
    };
    push_cell_header(
        lines,
        sticky_markers,
        label,
        theme.tool,
        suffix,
        timestamp,
        StickyHeader::new("Tool", header_timestamp_string(timestamp), tool),
        theme,
    );

    let base = Style::default().fg(theme.text);
    if is_generic_tool(raw_name)
        && matches!(
            input,
            serde_json::Value::Object(_) | serde_json::Value::Array(_)
        )
    {
        // Unknown / generic tool — pretty-print the raw arguments via syntect
        // instead of the compacted `summary` blurb.
        push_json_value_lines(lines, input, base, theme, highlight_query);
    } else if !summary.is_empty() {
        push_summary_or_json_lines(lines, summary, base, theme, highlight_query);
    }
    lines.push(Line::default());
}

#[allow(clippy::too_many_arguments)]
fn render_tool_result_into(
    lines: &mut Vec<Line<'static>>,
    sticky_markers: &mut Vec<StickyLineMarker>,
    tool: Option<&str>,
    output: &str,
    is_error: bool,
    call_summary: Option<&str>,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
    theme: &Theme,
    highlight_query: Option<&str>,
) {
    let label = match tool {
        Some(t) if !t.is_empty() => format!("\u{2039} {t}"),
        _ => "\u{2039} result".to_owned(),
    };
    let suffix = if is_error {
        Some("(error)".to_owned())
    } else {
        None
    };
    let subject = result_sticky_subject(tool, call_summary);
    push_cell_header(
        lines,
        sticky_markers,
        label,
        theme.tool,
        suffix,
        timestamp,
        StickyHeader::new("Result", header_timestamp_string(timestamp), subject),
        theme,
    );
    let base = if is_error {
        Style::default().fg(ratatui::style::Color::Red)
    } else {
        Style::default().fg(theme.muted)
    };
    push_summary_or_json_lines(lines, output, base, theme, highlight_query);
    lines.push(Line::default());
}

/// Maximum width (in chars) of a result cell's sticky-header subject. Chosen
/// to comfortably fit beside the from/datetime fields without overflowing the
/// header at typical terminal widths; longer values are truncated with an
/// ellipsis so the user still sees the head of the call.
const RESULT_SUBJECT_MAX_CHARS: usize = 120;

/// Build the sticky-header subject for a ToolResult. When the parser was able
/// to pair the result with its originating call, we echo the call's summary
/// (with the tool name as a prefix when both are present) so users scrolling
/// through long results still see *what* was invoked.
fn result_sticky_subject(tool: Option<&str>, call_summary: Option<&str>) -> String {
    let tool_clean = tool.map(str::trim).filter(|s| !s.is_empty());
    let summary_clean = call_summary.map(str::trim).filter(|s| !s.is_empty());
    let combined = match (tool_clean, summary_clean) {
        (Some(t), Some(s)) => format!("{t}: {s}"),
        (None, Some(s)) => s.to_owned(),
        (Some(t), None) => t.to_owned(),
        (None, None) => String::new(),
    };
    truncate_chars(&combined, RESULT_SUBJECT_MAX_CHARS)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let head: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{head}\u{2026}")
}

/// True when `raw_name` is a tool we don't know how to format specially —
/// `tool_label` returns the raw name unchanged for unknowns.
fn is_generic_tool(raw_name: &str) -> bool {
    let label = crate::parse::tool_format::tool_label(raw_name);
    label == raw_name.trim()
}

/// Push body lines for a tool summary/output. If the text parses as a JSON
/// object/array, pretty-print and syntect-highlight; otherwise render as
/// indented plain spans (existing behavior).
fn push_summary_or_json_lines(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    base: Style,
    theme: &Theme,
    highlight_query: Option<&str>,
) {
    if crate::tui::json_highlight::looks_like_json_object_or_array(text) {
        let terms = highlight_query
            .map(crate::search_query::extract_highlight_terms)
            .unwrap_or_default();
        let highlighted =
            crate::tui::json_highlight::highlight_json_string(text, base, theme, &terms);
        for line in highlighted {
            let mut spans = vec![Span::styled("  ", Style::default())];
            spans.extend(line.spans);
            let mut combined = Line::from(spans);
            combined.style = line.style;
            lines.push(combined);
        }
        return;
    }
    for line in text.lines() {
        let mut spans = vec![Span::styled("  ", Style::default())];
        spans.extend(highlight_into_spans(line, highlight_query, base, theme));
        lines.push(Line::from(spans));
    }
}

/// Push body lines for a structured `Value` that we already know is an
/// object/array — used when rendering a generic ToolCall's raw arguments.
fn push_json_value_lines(
    lines: &mut Vec<Line<'static>>,
    value: &serde_json::Value,
    base: Style,
    theme: &Theme,
    highlight_query: Option<&str>,
) {
    let terms = highlight_query
        .map(crate::search_query::extract_highlight_terms)
        .unwrap_or_default();
    let highlighted = crate::tui::json_highlight::highlight_json_value(value, base, theme, &terms);
    for line in highlighted {
        let mut spans = vec![Span::styled("  ", Style::default())];
        spans.extend(line.spans);
        let mut combined = Line::from(spans);
        combined.style = line.style;
        lines.push(combined);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_exec_into(
    lines: &mut Vec<Line<'static>>,
    sticky_markers: &mut Vec<StickyLineMarker>,
    command: &[String],
    parsed_summary: Option<&str>,
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    status: ExecStatus,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
    theme: &Theme,
    highlight_query: Option<&str>,
    display_options: DisplayOptions,
) {
    let display = parsed_summary
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| flatten_command(command));

    let mut suffix_bits: Vec<String> = Vec::new();
    if let Some(ms) = duration_ms {
        suffix_bits.push(format_duration(ms));
    }
    match exit_code {
        Some(0) => suffix_bits.push("exit 0".to_owned()),
        Some(n) => suffix_bits.push(format!("exit {n}")),
        None => {}
    }
    if matches!(status, ExecStatus::Pending) {
        suffix_bits.push("(running)".to_owned());
    }
    let suffix = if suffix_bits.is_empty() {
        None
    } else {
        Some(format!("({})", suffix_bits.join(", ")))
    };

    let label_color = match status {
        ExecStatus::Failed => ratatui::style::Color::Red,
        _ => theme.tool,
    };
    push_cell_header(
        lines,
        sticky_markers,
        format!("$ {display}"),
        label_color,
        suffix,
        timestamp,
        StickyHeader::new("Exec", header_timestamp_string(timestamp), &display),
        theme,
    );

    if !display_options.hide_tool_results {
        let stdout_style = Style::default().fg(theme.muted);
        for line in stdout.lines() {
            let mut spans = vec![Span::styled("  ", Style::default())];
            spans.extend(highlight_into_spans(
                line,
                highlight_query,
                stdout_style,
                theme,
            ));
            lines.push(Line::from(spans));
        }
        if !stderr.is_empty() {
            let stderr_style = Style::default().fg(ratatui::style::Color::Red);
            for line in stderr.lines() {
                let mut spans = vec![Span::styled("! ", stderr_style)];
                spans.extend(highlight_into_spans(
                    line,
                    highlight_query,
                    stderr_style,
                    theme,
                ));
                lines.push(Line::from(spans));
            }
        }
    }
    lines.push(Line::default());
}

#[allow(clippy::too_many_arguments)]
fn render_patch_into(
    lines: &mut Vec<Line<'static>>,
    sticky_markers: &mut Vec<StickyLineMarker>,
    files: &[PatchFile],
    success: bool,
    stdout: &str,
    stderr: &str,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
    theme: &Theme,
    highlight_query: Option<&str>,
    display_options: DisplayOptions,
) {
    let total_adds: usize = files.iter().map(|f| f.additions).sum();
    let total_dels: usize = files.iter().map(|f| f.deletions).sum();
    let header_summary = if files.is_empty() {
        "patch".to_owned()
    } else {
        format!("patch · {} file(s)", files.len())
    };
    let mut suffix_bits = Vec::new();
    if total_adds > 0 || total_dels > 0 {
        suffix_bits.push(format!("+{total_adds} -{total_dels}"));
    }
    if !success {
        suffix_bits.push("(failed)".to_owned());
    }
    let suffix = if suffix_bits.is_empty() {
        None
    } else {
        Some(suffix_bits.join(" "))
    };

    let label_color = if success {
        theme.tool
    } else {
        ratatui::style::Color::Red
    };
    push_cell_header(
        lines,
        sticky_markers,
        format!("\u{25c6} {header_summary}"),
        label_color,
        suffix,
        timestamp,
        StickyHeader::new("Patch", header_timestamp_string(timestamp), &header_summary),
        theme,
    );

    for file in files {
        let marker = match file.op {
            PatchOp::Add => "A",
            PatchOp::Update => "M",
            PatchOp::Delete => "D",
        };
        let marker_color = match file.op {
            PatchOp::Add => ratatui::style::Color::Green,
            PatchOp::Update => theme.accent,
            PatchOp::Delete => ratatui::style::Color::Red,
        };
        let mut spans = vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                format!("{marker} "),
                Style::default()
                    .fg(marker_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        spans.extend(highlight_into_spans(
            &file.path,
            highlight_query,
            Style::default().fg(theme.text),
            theme,
        ));
        if file.additions > 0 || file.deletions > 0 {
            spans.push(Span::styled(
                format!(" (+{} -{})", file.additions, file.deletions),
                Style::default().fg(theme.muted),
            ));
        }
        lines.push(Line::from(spans));
    }

    if !display_options.hide_tool_results {
        if !stdout.is_empty() {
            let dim = Style::default().fg(theme.muted);
            for line in stdout.lines() {
                let mut spans = vec![Span::styled("  ", Style::default())];
                spans.extend(highlight_into_spans(line, highlight_query, dim, theme));
                lines.push(Line::from(spans));
            }
        }
        if !stderr.is_empty() {
            let red = Style::default().fg(ratatui::style::Color::Red);
            for line in stderr.lines() {
                let mut spans = vec![Span::styled("! ", red)];
                spans.extend(highlight_into_spans(line, highlight_query, red, theme));
                lines.push(Line::from(spans));
            }
        }
    }
    lines.push(Line::default());
}

#[allow(clippy::too_many_arguments)]
fn render_web_search_into(
    lines: &mut Vec<Line<'static>>,
    sticky_markers: &mut Vec<StickyLineMarker>,
    query: &str,
    queries: &[String],
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
    theme: &Theme,
    highlight_query: Option<&str>,
) {
    push_cell_header(
        lines,
        sticky_markers,
        "\u{1f50e} web search".to_owned(),
        theme.tool,
        None,
        timestamp,
        StickyHeader::new("Web search", header_timestamp_string(timestamp), query),
        theme,
    );
    let primary = if !query.is_empty() { Some(query) } else { None };
    let body_style = Style::default().fg(theme.text);
    if let Some(q) = primary {
        let mut spans = vec![Span::styled(
            "  \u{25b8} ",
            Style::default().fg(theme.muted),
        )];
        spans.extend(highlight_into_spans(q, highlight_query, body_style, theme));
        lines.push(Line::from(spans));
    }
    for q in queries {
        if Some(q.as_str()) == primary {
            continue;
        }
        let mut spans = vec![Span::styled(
            "  \u{25b8} ",
            Style::default().fg(theme.muted),
        )];
        spans.extend(highlight_into_spans(q, highlight_query, body_style, theme));
        lines.push(Line::from(spans));
    }
    lines.push(Line::default());
}

fn render_plan_into(
    lines: &mut Vec<Line<'static>>,
    sticky_markers: &mut Vec<StickyLineMarker>,
    items: &[crate::parse::PlanItem],
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
    theme: &Theme,
    highlight_query: Option<&str>,
) {
    push_cell_header(
        lines,
        sticky_markers,
        "\u{2261} plan".to_owned(),
        theme.tool,
        None,
        timestamp,
        StickyHeader::new("Plan", header_timestamp_string(timestamp), ""),
        theme,
    );
    let body_style = Style::default().fg(theme.text);
    for item in items {
        let (marker, color) = match item.status {
            PlanItemStatus::Completed => ("[x]", theme.tool),
            PlanItemStatus::InProgress => ("[-]", theme.accent),
            PlanItemStatus::Pending => ("[ ]", theme.muted),
        };
        let mut spans = vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                format!("{marker} "),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
        ];
        spans.extend(highlight_into_spans(
            &item.step,
            highlight_query,
            body_style,
            theme,
        ));
        lines.push(Line::from(spans));
    }
    lines.push(Line::default());
}

fn render_metrics_footer(metrics: &RuntimeMetrics, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    let mut bits: Vec<String> = Vec::new();
    if metrics.total_tokens > 0 {
        let mut tokens = format!("{} tok total", format_int(metrics.total_tokens));
        if metrics.cached_input_tokens > 0 {
            tokens.push_str(&format!(
                " ({} cached)",
                format_int(metrics.cached_input_tokens)
            ));
        }
        bits.push(tokens);
    }
    if metrics.input_tokens > 0 || metrics.output_tokens > 0 {
        bits.push(format!(
            "in={} out={}",
            format_int(metrics.input_tokens),
            format_int(metrics.output_tokens),
        ));
    }
    if metrics.reasoning_output_tokens > 0 {
        bits.push(format!(
            "reasoning={}",
            format_int(metrics.reasoning_output_tokens)
        ));
    }
    if let Some(window) = metrics.model_context_window {
        bits.push(format!("ctx={}", format_int(window)));
    }
    if metrics.tool_call_count > 0 {
        let mut tools = format!("{} tool call(s)", metrics.tool_call_count);
        if metrics.tool_failure_count > 0 {
            tools.push_str(&format!(" — {} failed", metrics.tool_failure_count));
        }
        bits.push(tools);
    }
    if metrics.exec_count > 0 || metrics.patch_count > 0 || metrics.web_search_count > 0 {
        let mut by_kind = Vec::new();
        if metrics.exec_count > 0 {
            by_kind.push(format!("exec×{}", metrics.exec_count));
        }
        if metrics.patch_count > 0 {
            by_kind.push(format!("patch×{}", metrics.patch_count));
        }
        if metrics.web_search_count > 0 {
            by_kind.push(format!("search×{}", metrics.web_search_count));
        }
        bits.push(by_kind.join(" "));
    }
    if let Some(ms) = metrics.total_wall_ms {
        bits.push(format!("wall {}", format_duration(ms)));
    }

    if bits.is_empty() {
        return lines;
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "── Totals ──",
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    )));
    let label_style = Style::default().fg(theme.muted);
    for bit in bits {
        lines.push(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(bit, label_style),
        ]));
    }
    lines
}

fn format_int(n: u64) -> String {
    // Insert thousands separators.
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.insert(0, ',');
        }
        out.insert(0, c);
    }
    out
}

fn flatten_command(argv: &[String]) -> String {
    if argv.is_empty() {
        return "(no command)".to_owned();
    }
    // Strip a leading shell wrapper if present (`/bin/sh -c ...`, `/bin/zsh -lc ...`).
    if argv.len() >= 3 && argv[0].ends_with("sh") && argv[1].starts_with('-') {
        return argv[2].clone();
    }
    argv.join(" ")
}

fn format_duration(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let secs = ms / 1000;
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

fn message_colors(
    agent: Agent,
    role: MessageRole,
    theme: &Theme,
) -> (ratatui::style::Color, ratatui::style::Color) {
    match role {
        MessageRole::User => (theme.accent, theme.bubble_user),
        MessageRole::Assistant => match agent {
            Agent::Claude => (theme.claude, theme.bubble_claude),
            Agent::Codex => (theme.codex, theme.bubble_codex),
        },
        MessageRole::System => (theme.muted, theme.bubble_system),
        MessageRole::Summary => (theme.highlight, theme.bubble_summary),
        MessageRole::ToolCall | MessageRole::ToolResult => (theme.tool, theme.bubble_tool),
    }
}

fn normalize_highlight_query(query: &str) -> Option<&str> {
    (!query.is_empty()).then_some(query)
}

fn scroll_limit_for_text(text: &Text<'_>, area: Rect) -> usize {
    let viewport_height = area
        .height
        .saturating_sub(2)
        .saturating_sub(STICKY_HEADER_HEIGHT) as usize;
    let viewport_width = area.width.saturating_sub(2);
    wrapped_text_height(text, viewport_width).saturating_sub(viewport_height)
}

pub fn split_sticky_body(inner: Rect) -> (Rect, Rect) {
    let header_height = STICKY_HEADER_HEIGHT.min(inner.height);
    let header = Rect::new(inner.x, inner.y, inner.width, header_height);
    let body = Rect::new(
        inner.x,
        inner.y.saturating_add(header_height),
        inner.width,
        inner.height.saturating_sub(header_height),
    );
    (header, body)
}

fn sticky_header_for_message(agent: Agent, message: &crate::parse::SessionMessage) -> StickyHeader {
    let from = match message.role {
        MessageRole::User => "User",
        MessageRole::Assistant => "Agent",
        MessageRole::System => "System",
        MessageRole::Summary => "Summary",
        MessageRole::ToolCall | MessageRole::ToolResult => "Tool",
    };
    let datetime = message
        .timestamp
        .map(|timestamp| timestamp.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default();
    let subject = match message.role {
        MessageRole::ToolCall | MessageRole::ToolResult => {
            message.tool_name.clone().unwrap_or_default()
        }
        MessageRole::Assistant => match agent {
            Agent::Claude => "Claude".to_owned(),
            Agent::Codex => "Codex".to_owned(),
        },
        _ => String::new(),
    };
    StickyHeader::new(from, datetime, subject)
}

fn add_summary_heading_markers(
    markers: &mut Vec<StickyLineMarker>,
    markdown: &str,
    theme: &Theme,
    highlight_query: Option<&str>,
    line_offset: usize,
    base_header: StickyHeader,
) {
    let rendered = render_markdown_message_with_headings(
        markdown,
        theme,
        Style::default().fg(theme.text),
        highlight_query,
    );
    for heading in rendered.headings {
        let mut header = base_header.clone();
        header.subject = truncate_chars(&heading.breadcrumb, RESULT_SUBJECT_MAX_CHARS);
        markers.push(StickyLineMarker {
            line_index: line_offset + 2 + heading.line_index,
            header,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::{TimeZone, Utc};

    use crate::parse::{Agent, DerivationType, MessageRole, Session, SessionMessage};
    use crate::summary::{
        AicsSummaryPreview, ClaudeAutosummaryPreview, Fingerprint, SummarizeBackend,
        SummaryPreview, SummarySidecar, SummarySources,
    };

    use super::{
        normalize_highlight_query, render_composite_text, render_session_document,
        render_session_text, render_summary_missing, render_summary_text, result_sticky_subject,
        SessionRenderOptions, RESULT_SUBJECT_MAX_CHARS,
    };
    use crate::parse::{
        ExecStatus, PatchFile, PatchOp, PlanItem, PlanItemStatus, RuntimeMetrics, SessionCell,
        SessionInfo,
    };
    use crate::settings::DisplayOptions;
    use crate::tui::theme::Theme;

    fn empty_session(agent: Agent, cells: Vec<SessionCell>) -> Session {
        Session {
            session_id: "session-cells".to_owned(),
            agent,
            project: "/tmp/demo".to_owned(),
            branch: None,
            cwd: Some("/tmp/demo".to_owned()),
            created: None,
            modified: None,
            modified_ts: 0,
            lines: 0,
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
            cells,
            session_info: None,
        }
    }

    fn rendered_lines(text: &ratatui::text::Text<'_>) -> Vec<String> {
        text.lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn preview_section_honors_project_docs_autodump_display_option() {
        let theme = Theme::default();
        let session = empty_session(
            Agent::Codex,
            vec![
                SessionCell::Message {
                    role: MessageRole::User,
                    content: "# CLAUDE.md instructions for /tmp/demo\nUse cargo test.".to_owned(),
                    timestamp: None,
                },
                SessionCell::Message {
                    role: MessageRole::User,
                    content:
                        "<INSTRUCTIONS># Using `lat` to examine files\nPrefer lat.\n</INSTRUCTIONS>"
                            .to_owned(),
                    timestamp: None,
                },
                SessionCell::Message {
                    role: MessageRole::User,
                    content: "real request".to_owned(),
                    timestamp: None,
                },
            ],
        );

        let hidden = super::render_session_section_document_with_options(
            Some(&session),
            &theme,
            None,
            DisplayOptions::default(),
        );
        let hidden_text = rendered_lines(&hidden.text).join("\n");

        assert!(!hidden_text.contains("CLAUDE.md instructions"));
        assert!(!hidden_text.contains("Prefer lat."));
        assert!(hidden_text.contains("real request"));

        let visible = super::render_session_section_document_with_options(
            Some(&session),
            &theme,
            None,
            DisplayOptions {
                hide_project_docs_autodump: false,
                ..DisplayOptions::default()
            },
        );
        let visible_text = rendered_lines(&visible.text).join("\n");

        assert!(visible_text.contains("CLAUDE.md instructions"));
        assert!(visible_text.contains("Prefer lat."));
        assert!(visible_text.contains("real request"));
    }

    #[test]
    fn preview_honors_skill_text_injection_display_option() {
        let theme = Theme::default();
        let session = empty_session(
            Agent::Codex,
            vec![
                SessionCell::Message {
                    role: MessageRole::User,
                    content: "$commit --all".to_owned(),
                    timestamp: None,
                },
                SessionCell::Message {
                    role: MessageRole::User,
                    content: "<skill><name>commit</name>helper instructions</skill>".to_owned(),
                    timestamp: None,
                },
            ],
        );

        let visible = super::render_session_text_with_options(
            &session,
            &theme,
            None,
            DisplayOptions::default(),
        );
        let visible_text = rendered_lines(&visible).join("\n");
        assert!(visible_text.contains("$commit --all"));
        assert!(visible_text.contains("helper instructions"));

        let hidden = super::render_session_text_with_options(
            &session,
            &theme,
            None,
            DisplayOptions {
                hide_skill_text_injection: true,
                ..DisplayOptions::default()
            },
        );
        let hidden_text = rendered_lines(&hidden).join("\n");
        assert!(hidden_text.contains("$commit --all"));
        assert!(!hidden_text.contains("helper instructions"));
    }

    #[test]
    fn render_cells_emits_exec_patch_plan_websearch_markers() {
        let theme = Theme::default();
        let cells = vec![
            SessionCell::Exec {
                command: vec!["/bin/zsh".to_owned(), "-lc".to_owned(), "ls".to_owned()],
                cwd: None,
                parsed_summary: Some("ls".to_owned()),
                stdout: "alpha\nbeta".to_owned(),
                stderr: String::new(),
                exit_code: Some(0),
                duration_ms: Some(120),
                status: ExecStatus::Completed,
                timestamp: None,
            },
            SessionCell::Patch {
                files: vec![PatchFile {
                    path: "src/lib.rs".to_owned(),
                    op: PatchOp::Update,
                    content: None,
                    additions: 3,
                    deletions: 1,
                }],
                success: true,
                stdout: String::new(),
                stderr: String::new(),
                timestamp: None,
            },
            SessionCell::Plan {
                items: vec![
                    PlanItem {
                        status: PlanItemStatus::Completed,
                        step: "design".to_owned(),
                    },
                    PlanItem {
                        status: PlanItemStatus::InProgress,
                        step: "build".to_owned(),
                    },
                    PlanItem {
                        status: PlanItemStatus::Pending,
                        step: "ship".to_owned(),
                    },
                ],
                timestamp: None,
            },
            SessionCell::WebSearch {
                query: "rust serde tagged enum".to_owned(),
                queries: vec![
                    "rust serde tagged enum".to_owned(),
                    "serde untagged".to_owned(),
                ],
                timestamp: None,
            },
            SessionCell::Metrics(RuntimeMetrics {
                input_tokens: 1000,
                output_tokens: 250,
                total_tokens: 1250,
                tool_call_count: 4,
                exec_count: 1,
                patch_count: 1,
                web_search_count: 1,
                ..RuntimeMetrics::default()
            }),
        ];
        let session = empty_session(Agent::Codex, cells);
        let doc = render_session_document(&session, &theme, None);
        let lines = rendered_lines(&doc.text);
        let joined = lines.join("\n");

        assert!(joined.contains("$ ls"), "exec header missing: {joined}");
        assert!(joined.contains("exit 0"), "exec exit suffix missing");
        assert!(joined.contains("alpha"), "exec stdout missing");
        assert!(joined.contains("\u{25c6} patch"), "patch header missing");
        assert!(joined.contains("M src/lib.rs"), "patch file row missing");
        assert!(joined.contains("(+3 -1)"), "patch +/- counts missing");
        assert!(joined.contains("\u{2261} plan"), "plan header missing");
        assert!(joined.contains("[x] design"), "completed plan item missing");
        assert!(
            joined.contains("[-] build"),
            "in-progress plan item missing"
        );
        assert!(joined.contains("[ ] ship"), "pending plan item missing");
        assert!(joined.contains("web search"), "web search header missing");
        assert!(
            joined.contains("rust serde tagged enum"),
            "search query missing"
        );
        assert!(joined.contains("Totals"), "metrics footer missing");
        assert!(joined.contains("1,250"), "total_tokens not formatted");
        assert!(joined.contains("4 tool call(s)"), "tool count missing");
    }

    #[test]
    fn display_options_hide_selected_transcript_parts() {
        let theme = Theme::default();
        let session = empty_session(
            Agent::Codex,
            vec![
                SessionCell::Message {
                    role: MessageRole::User,
                    content: "user text".to_owned(),
                    timestamp: None,
                },
                SessionCell::Message {
                    role: MessageRole::Assistant,
                    content: "agent text".to_owned(),
                    timestamp: None,
                },
                SessionCell::ToolCall {
                    tool: "Read".to_owned(),
                    raw_name: "Read".to_owned(),
                    summary: "src/lib.rs".to_owned(),
                    input: serde_json::Value::Null,
                    status: crate::parse::ToolStatus::Completed,
                    timestamp: None,
                },
                SessionCell::ToolResult {
                    tool: Some("Read".to_owned()),
                    output: "tool result text".to_owned(),
                    is_error: false,
                    call_summary: Some("src/lib.rs".to_owned()),
                    timestamp: None,
                },
                SessionCell::Exec {
                    command: vec!["/bin/zsh".to_owned(), "-lc".to_owned(), "ls".to_owned()],
                    cwd: None,
                    parsed_summary: Some("ls".to_owned()),
                    stdout: "exec stdout".to_owned(),
                    stderr: String::new(),
                    exit_code: Some(0),
                    duration_ms: None,
                    status: ExecStatus::Completed,
                    timestamp: None,
                },
            ],
        );

        let doc = super::render_session_document_with_options(
            &session,
            &theme,
            None,
            SessionRenderOptions {
                display_options: DisplayOptions {
                    hide_tool_calls: true,
                    hide_tool_results: true,
                    hide_agent_replies: true,
                    hide_user_messages: true,
                    ..DisplayOptions::default()
                },
                ..SessionRenderOptions::default()
            },
        );
        let joined = rendered_lines(&doc.text).join("\n");

        assert!(!joined.contains("user text"));
        assert!(!joined.contains("agent text"));
        assert!(!joined.contains("src/lib.rs"));
        assert!(!joined.contains("tool result text"));
        assert!(!joined.contains("exec stdout"));
        assert!(!joined.contains("$ ls"));
    }

    #[test]
    fn display_options_hide_tool_results_keeps_exec_call_header() {
        let theme = Theme::default();
        let session = empty_session(
            Agent::Codex,
            vec![SessionCell::Exec {
                command: vec!["/bin/zsh".to_owned(), "-lc".to_owned(), "ls".to_owned()],
                cwd: None,
                parsed_summary: Some("ls".to_owned()),
                stdout: "exec stdout".to_owned(),
                stderr: String::new(),
                exit_code: Some(0),
                duration_ms: None,
                status: ExecStatus::Completed,
                timestamp: None,
            }],
        );

        let doc = super::render_session_document_with_options(
            &session,
            &theme,
            None,
            SessionRenderOptions {
                display_options: DisplayOptions {
                    hide_tool_calls: false,
                    hide_tool_results: true,
                    hide_agent_replies: false,
                    hide_user_messages: false,
                    ..DisplayOptions::default()
                },
                ..SessionRenderOptions::default()
            },
        );
        let joined = rendered_lines(&doc.text).join("\n");

        assert!(joined.contains("$ ls"));
        assert!(!joined.contains("exec stdout"));
    }

    #[test]
    fn render_cells_emits_session_info_block() {
        let theme = Theme::default();
        let mut session = empty_session(
            Agent::Codex,
            vec![SessionCell::Message {
                role: MessageRole::User,
                content: "hi".to_owned(),
                timestamp: None,
            }],
        );
        session.session_info = Some(SessionInfo {
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: Some("medium".to_owned()),
            sandbox_mode: Some("workspace-write".to_owned()),
            approval_policy: Some("on-request".to_owned()),
            cwd: Some("/tmp/demo".to_owned()),
            cli_version: Some("0.116.0".to_owned()),
            ..SessionInfo::default()
        });
        let doc = render_session_document(&session, &theme, None);
        let joined = rendered_lines(&doc.text).join("\n");
        assert!(joined.contains("Session"), "session block header missing");
        assert!(joined.contains("gpt-5-codex"), "model missing");
        assert!(joined.contains("medium"), "reasoning effort missing");
        assert!(joined.contains("workspace-write"), "sandbox mode missing");
        assert!(joined.contains("on-request"), "approval policy missing");
        assert!(joined.contains("v0.116.0"), "cli version missing");
    }

    #[test]
    fn render_tool_call_with_unknown_tool_pretty_prints_json_input() {
        use crate::parse::ToolStatus;

        let theme = Theme::default();
        let cells = vec![SessionCell::ToolCall {
            tool: "weather_lookup".to_owned(),
            raw_name: "weather_lookup".to_owned(),
            summary: "city: Seattle, units: c".to_owned(),
            input: serde_json::json!({"city": "Seattle", "units": "c"}),
            status: ToolStatus::Completed,
            timestamp: None,
        }];
        let session = empty_session(Agent::Codex, cells);
        let doc = render_session_document(&session, &theme, None);
        let lines = rendered_lines(&doc.text);
        let joined = lines.join("\n");

        assert!(
            joined.contains("\"city\": \"Seattle\""),
            "expected pretty-printed key/value, got:\n{joined}"
        );
        assert!(
            joined.contains("\"units\": \"c\""),
            "expected pretty-printed second key/value, got:\n{joined}"
        );

        let any_colored = doc.text.lines.iter().any(|l| {
            l.spans
                .iter()
                .any(|s| matches!(s.style.fg, Some(ratatui::style::Color::Rgb(..))))
        });
        assert!(
            any_colored,
            "expected syntect to assign at least one Rgb foreground color"
        );
    }

    #[test]
    fn render_tool_call_with_known_tool_keeps_summary_plain() {
        use crate::parse::ToolStatus;

        let theme = Theme::default();
        let cells = vec![SessionCell::ToolCall {
            tool: "bash".to_owned(),
            raw_name: "Bash".to_owned(),
            summary: "ls -la".to_owned(),
            input: serde_json::json!({"command": "ls -la"}),
            status: ToolStatus::Completed,
            timestamp: None,
        }];
        let session = empty_session(Agent::Claude, cells);
        let doc = render_session_document(&session, &theme, None);
        let joined = rendered_lines(&doc.text).join("\n");

        assert!(joined.contains("ls -la"), "expected raw command summary");
        assert!(
            !joined.contains("\"command\""),
            "should not pretty-print JSON for known tool: {joined}"
        );
    }

    #[test]
    fn render_tool_result_highlights_json_object_output() {
        let theme = Theme::default();
        let cells = vec![SessionCell::ToolResult {
            tool: Some("mcp_thing".to_owned()),
            output: "{\n  \"status\": \"ok\",\n  \"count\": 7\n}".to_owned(),
            is_error: false,
            call_summary: None,
            timestamp: None,
        }];
        let session = empty_session(Agent::Codex, cells);
        let doc = render_session_document(&session, &theme, None);
        let joined = rendered_lines(&doc.text).join("\n");

        assert!(joined.contains("\"status\": \"ok\""));
        assert!(joined.contains("\"count\": 7"));

        let has_colored_token = doc.text.lines.iter().any(|l| {
            l.spans
                .iter()
                .any(|s| matches!(s.style.fg, Some(ratatui::style::Color::Rgb(..))))
        });
        assert!(has_colored_token, "expected syntect colors on JSON tokens");
    }

    #[test]
    fn render_tool_result_with_plain_text_output_unchanged() {
        let theme = Theme::default();
        let cells = vec![SessionCell::ToolResult {
            tool: None,
            output: "hello world\nsecond line".to_owned(),
            is_error: false,
            call_summary: None,
            timestamp: None,
        }];
        let session = empty_session(Agent::Codex, cells);
        let doc = render_session_document(&session, &theme, None);
        let joined = rendered_lines(&doc.text).join("\n");
        assert!(joined.contains("hello world"));
        assert!(joined.contains("second line"));
    }

    #[test]
    fn render_tool_result_sticky_header_inherits_call_summary() {
        let theme = Theme::default();
        let cells = vec![SessionCell::ToolResult {
            tool: Some("bash".to_owned()),
            output: "alpha\nbeta".to_owned(),
            is_error: false,
            call_summary: Some(
                "git log --oneline origin/main..HEAD -- some/path/file.rs".to_owned(),
            ),
            timestamp: None,
        }];
        let session = empty_session(Agent::Codex, cells);
        let doc = render_session_document(&session, &theme, None);
        let result_marker = doc
            .sticky_markers
            .iter()
            .find(|m| m.header.from == "Result")
            .expect("expected a Result sticky marker");
        assert!(
            result_marker.header.subject.contains("git log --oneline"),
            "subject should echo the call's summary, got: {:?}",
            result_marker.header.subject
        );
        assert!(
            result_marker.header.subject.starts_with("bash:"),
            "subject should be tool-prefixed, got: {:?}",
            result_marker.header.subject
        );
    }

    #[test]
    fn render_tool_result_sticky_header_falls_back_to_tool_only() {
        let theme = Theme::default();
        let cells = vec![SessionCell::ToolResult {
            tool: Some("mcp_thing".to_owned()),
            output: "ok".to_owned(),
            is_error: false,
            call_summary: None,
            timestamp: None,
        }];
        let session = empty_session(Agent::Codex, cells);
        let doc = render_session_document(&session, &theme, None);
        let marker = doc
            .sticky_markers
            .iter()
            .find(|m| m.header.from == "Result")
            .expect("expected a Result sticky marker");
        assert_eq!(marker.header.subject, "mcp_thing");
    }

    #[test]
    fn result_sticky_subject_truncates_long_call_summary() {
        let long = "x".repeat(500);
        let subject = result_sticky_subject(Some("bash"), Some(&long));
        assert!(subject.chars().count() <= RESULT_SUBJECT_MAX_CHARS);
        assert!(subject.ends_with('\u{2026}'));
        assert!(subject.starts_with("bash: "));
    }

    #[test]
    fn render_session_text_renders_markdown_body_with_search_highlighting() {
        let theme = Theme::default();
        let session = Session {
            session_id: "session-1".to_owned(),
            agent: Agent::Claude,
            project: "/tmp/demo".to_owned(),
            branch: Some("main".to_owned()),
            cwd: Some("/tmp/demo".to_owned()),
            created: Some(Utc::now()),
            modified: Some(Utc::now()),
            modified_ts: 0,
            lines: 3,
            file_path: PathBuf::from("/tmp/demo/session.jsonl"),
            first_msg_role: Some(MessageRole::Assistant),
            first_msg_content: "**alpha**\n\n- beta".to_owned(),
            last_msg_role: Some(MessageRole::Assistant),
            last_msg_content: "**alpha**\n\n- beta".to_owned(),
            first_user_msg_content: String::new(),
            derivation_type: DerivationType::Original,
            is_sidechain: false,
            custom_title: None,
            messages: vec![SessionMessage {
                role: MessageRole::Assistant,
                content: "**alpha**\n\n- beta".to_owned(),
                timestamp: Some(Utc::now()),
                tool_name: None,
            }],
            content: "**alpha**\n\n- beta".to_owned(),
            cells: Vec::new(),
            session_info: None,
        };

        let text = render_session_text(&session, &theme, Some("alpha"));
        let rendered = text
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        assert!(rendered[0].starts_with("Assistant "));
        assert_eq!(rendered[1], "alpha");
        assert!(rendered.iter().any(|line| line == "• beta"));

        let alpha = text.lines[1]
            .spans
            .iter()
            .find(|span| span.content.as_ref() == "alpha")
            .expect("alpha span");
        assert!(alpha
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD));
        assert_eq!(alpha.style.fg, Some(theme.text));
        assert_eq!(alpha.style.bg, Some(theme.search_match_bg));
    }

    #[test]
    fn render_session_document_records_message_and_heading_sticky_markers() {
        let theme = Theme::default();
        let session = Session {
            session_id: "session-1".to_owned(),
            agent: Agent::Claude,
            project: "/tmp/demo".to_owned(),
            branch: Some("main".to_owned()),
            cwd: Some("/tmp/demo".to_owned()),
            created: None,
            modified: None,
            modified_ts: 0,
            lines: 3,
            file_path: PathBuf::from("/tmp/demo/session.jsonl"),
            first_msg_role: Some(MessageRole::User),
            first_msg_content: "# Topic\n\nBody".to_owned(),
            last_msg_role: Some(MessageRole::User),
            last_msg_content: "# Topic\n\nBody".to_owned(),
            first_user_msg_content: "# Topic\n\nBody".to_owned(),
            derivation_type: DerivationType::Original,
            is_sidechain: false,
            custom_title: None,
            messages: vec![SessionMessage {
                role: MessageRole::User,
                content: "# Topic\n\nBody".to_owned(),
                timestamp: Some(Utc.with_ymd_and_hms(2026, 4, 25, 10, 11, 12).unwrap()),
                tool_name: None,
            }],
            content: "# Topic\n\nBody".to_owned(),
            cells: Vec::new(),
            session_info: None,
        };

        let document = render_session_document(&session, &theme, None);

        assert_eq!(document.sticky_markers[0].line_index, 0);
        assert_eq!(document.sticky_markers[0].header.from, "User");
        assert_eq!(
            document.sticky_markers[0].header.datetime,
            "2026-04-25 10:11:12"
        );
        assert_eq!(document.sticky_markers[1].line_index, 1);
        assert_eq!(document.sticky_markers[1].header.subject, "Topic");
    }

    #[test]
    fn nested_assistant_headings_render_breadcrumb_in_sticky_subject() {
        let theme = Theme::default();
        let body = "# Top\n\n## Section\n\n### Detail\n\nlast";
        let session = Session {
            session_id: "session-bc".to_owned(),
            agent: Agent::Claude,
            project: "/tmp/demo".to_owned(),
            branch: None,
            cwd: Some("/tmp/demo".to_owned()),
            created: None,
            modified: None,
            modified_ts: 0,
            lines: 0,
            file_path: PathBuf::from("/tmp/demo/session.jsonl"),
            first_msg_role: None,
            first_msg_content: String::new(),
            last_msg_role: None,
            last_msg_content: String::new(),
            first_user_msg_content: String::new(),
            derivation_type: DerivationType::Original,
            is_sidechain: false,
            custom_title: None,
            messages: vec![SessionMessage {
                role: MessageRole::Assistant,
                content: body.to_owned(),
                timestamp: Some(Utc.with_ymd_and_hms(2026, 4, 27, 9, 0, 0).unwrap()),
                tool_name: None,
            }],
            content: body.to_owned(),
            cells: Vec::new(),
            session_info: None,
        };

        let doc = render_session_document(&session, &theme, None);
        let subjects: Vec<&str> = doc
            .sticky_markers
            .iter()
            .map(|m| m.header.subject.as_str())
            .collect();
        assert!(subjects.contains(&"Top"));
        assert!(subjects.contains(&"Top \u{203a} Section"));
        assert!(subjects.contains(&"Top \u{203a} Section \u{203a} Detail"));
    }

    #[test]
    fn normalize_highlight_query_uses_non_empty_search_text() {
        assert_eq!(normalize_highlight_query("alpha"), Some("alpha"));
        assert_eq!(normalize_highlight_query(""), None);
    }

    #[test]
    fn render_summary_text_labels_aics_sidecar_source() {
        let theme = Theme::default();
        let summary = SummaryPreview::AicsSidecar(AicsSummaryPreview {
            sidecar: SummarySidecar {
                schema: 1,
                source_file: "session.jsonl".to_owned(),
                line_count: 2,
                last_line_sha256: "abc".repeat(21) + "a",
                generated_at: Utc.with_ymd_and_hms(2026, 4, 15, 13, 25, 0).unwrap(),
                backend: SummarizeBackend::Claude,
                body: "Sidecar body".to_owned(),
            },
            fingerprint: Fingerprint {
                line_count: 2,
                last_line_sha256: "abc".repeat(21) + "a",
            },
        });

        let text = render_summary_text(&summary, &theme, None);
        let header = text.lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(header.contains("aics"));
        assert!(header.contains("claude"));
        assert!(header.contains("FRESH"));
    }

    #[test]
    fn render_summary_text_labels_claude_autosummary_source() {
        let theme = Theme::default();
        let summary = SummaryPreview::ClaudeAutosummary(ClaudeAutosummaryPreview {
            body: "Autosummary body".to_owned(),
            generated_at: Some(Utc.with_ymd_and_hms(2026, 4, 15, 13, 25, 59).unwrap()),
        });

        let text = render_summary_text(&summary, &theme, None);
        let header = text.lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(header.contains("built-in"));
        assert!(header.contains("claude autosummary"));
        assert!(!header.contains("aics summary"));
    }

    #[test]
    fn render_summary_missing_only_shows_generate_hint_when_requested() {
        let theme = Theme::default();
        let hidden = render_summary_missing(&theme, "missing", false);
        let shown = render_summary_missing(&theme, "missing", true);

        let hidden_lines = hidden
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let shown_lines = shown
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        assert!(hidden_lines
            .iter()
            .all(|line| !line.contains("generate an AICS summary")));
        assert!(shown_lines
            .iter()
            .any(|line| line.contains("generate an AICS summary")));
    }

    #[test]
    fn render_composite_text_includes_all_summary_sections_before_session_log() {
        let theme = Theme::default();
        let session = Session {
            session_id: "session-1".to_owned(),
            agent: Agent::Claude,
            project: "/tmp/demo".to_owned(),
            branch: Some("main".to_owned()),
            cwd: Some("/tmp/demo".to_owned()),
            created: Some(Utc::now()),
            modified: Some(Utc::now()),
            modified_ts: 0,
            lines: 3,
            file_path: PathBuf::from("/tmp/demo/session.jsonl"),
            first_msg_role: Some(MessageRole::Assistant),
            first_msg_content: "alpha".to_owned(),
            last_msg_role: Some(MessageRole::Assistant),
            last_msg_content: "alpha".to_owned(),
            first_user_msg_content: String::new(),
            derivation_type: DerivationType::Original,
            is_sidechain: false,
            custom_title: None,
            messages: vec![SessionMessage {
                role: MessageRole::Assistant,
                content: "alpha".to_owned(),
                timestamp: Some(Utc::now()),
                tool_name: None,
            }],
            content: "alpha".to_owned(),
            cells: Vec::new(),
            session_info: None,
        };
        let text = render_composite_text(
            Some(&session),
            &SummarySources {
                aics_sidecar: Some(AicsSummaryPreview {
                    sidecar: SummarySidecar {
                        schema: 1,
                        source_file: "session.jsonl".to_owned(),
                        line_count: 1,
                        last_line_sha256: "abc".repeat(21) + "a",
                        generated_at: Utc.with_ymd_and_hms(2026, 4, 15, 13, 25, 0).unwrap(),
                        backend: SummarizeBackend::Claude,
                        body: "AICS body".to_owned(),
                    },
                    fingerprint: Fingerprint {
                        line_count: 1,
                        last_line_sha256: "abc".repeat(21) + "a",
                    },
                }),
                claude_autosummaries: vec![
                    ClaudeAutosummaryPreview {
                        body: "Claude body 1".to_owned(),
                        generated_at: None,
                    },
                    ClaudeAutosummaryPreview {
                        body: "Claude body 2".to_owned(),
                        generated_at: None,
                    },
                ],
            },
            &theme,
            None,
            false,
        );
        let rendered = text
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        let joined = rendered.join("\n");
        let claude_one = joined.find("# Claude Auto-summary #1").unwrap();
        let claude_two = joined.find("# Claude Auto-summary #2").unwrap();
        let aics = joined.find("# AICS summary").unwrap();
        let session_log = joined.find("# Session Log").unwrap();

        assert!(claude_one < claude_two);
        assert!(claude_two < aics);
        assert!(aics < session_log);
        assert!(joined.contains("Claude body 1"));
        assert!(joined.contains("Claude body 2"));
        assert!(joined.contains("AICS body"));
        assert!(joined.contains("Assistant"));
    }
}
