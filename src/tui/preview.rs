use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders};
use ratatui::Frame;

use crate::parse::{Agent, MessageRole, Session};
use crate::summary::{
    AicsSummaryPreview, ClaudeAutosummaryPreview, SummaryPreview, SummarySources,
};
use crate::tui::app::App;
use crate::tui::markdown::{render_markdown_message, render_markdown_message_with_headings};
use crate::tui::profile;
use crate::tui::theme::Theme;
use crate::tui::util::{
    block_title, session_message_label, wrapped_text_height, FullLineBackgroundParagraph,
    StickyHeader, StickyHeaderWidget, StickyLineMarker, STICKY_HEADER_HEIGHT,
};

#[derive(Debug, Clone)]
pub struct DisplayDocument {
    pub text: Text<'static>,
    pub sticky_markers: Vec<StickyLineMarker>,
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
        .title(block_title(Span::styled(
            app.preview_title(),
            Style::default().fg(theme.accent),
        )));

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
    render_session_document(session, theme, highlight_query).text
}

pub fn render_session_document(
    session: &Session,
    theme: &Theme,
    highlight_query: Option<&str>,
) -> DisplayDocument {
    let _profile = profile::scope("preview.render_session_text");
    let mut lines = Vec::new();
    let mut sticky_markers = Vec::new();
    for message in &session.messages {
        let (label_color, _) = message_colors(session.agent, message.role, theme);
        let label = session_message_label(message);
        let header_line = lines.len();
        let base_header = sticky_header_for_message(session.agent, message);
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
            session.agent,
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
    DisplayDocument {
        text: Text::from(lines),
        sticky_markers,
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
    let mut summary =
        render_summary_sections_document(summaries, theme, highlight_query, summary_inflight);
    if !summary.text.lines.is_empty() && session.is_some() {
        summary.text.lines.push(Line::default());
    }

    let session_doc = render_session_section_document(session, theme, highlight_query);
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
    let body = if let Some(session) = session {
        render_session_document(session, theme, highlight_query)
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
                header: StickyHeader::new("", "", heading.text),
            })
            .collect(),
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
        header.subject = heading.text;
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
        render_session_text, render_summary_missing, render_summary_text,
    };
    use crate::tui::theme::Theme;

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
