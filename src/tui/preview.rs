use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::parse::{Agent, MessageRole, Session};
use crate::summary::{
    AicsSummaryPreview, ClaudeAutosummaryPreview, SummaryPreview, SummarySources,
};
use crate::tui::app::App;
use crate::tui::markdown::render_markdown_message;
use crate::tui::profile;
use crate::tui::theme::Theme;
use crate::tui::util::{block_title, session_message_label, wrapped_text_height};

pub fn render(frame: &mut Frame, app: &mut App, area: Rect, theme: &Theme) {
    let _profile = profile::scope("preview.render");
    let (mut text, max_scroll, active_match_row) =
        if let Some(state) = app.preview_render_state(area, theme) {
            (state.text.clone(), state.max_scroll, state.active_match_row)
        } else {
            (
                Text::from(Line::from(Span::styled(
                    "Select a session to preview",
                    Style::default().fg(theme.muted),
                ))),
                0,
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

    let paragraph = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.preview_scroll as u16, 0));
    frame.render_widget(paragraph, area);
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
    let _profile = profile::scope("preview.render_session_text");
    let mut lines = Vec::new();
    for message in &session.messages {
        let (label_color, _) = message_colors(session.agent, message.role, theme);
        let label = session_message_label(message);
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

        let rendered = render_message_body(
            session.agent,
            message.role,
            message.content.as_str(),
            theme,
            highlight_query,
        );
        lines.extend(rendered.lines);
        lines.push(Line::default());
    }
    Text::from(lines)
}

pub fn render_composite_text(
    session: Option<&Session>,
    summaries: &SummarySources,
    theme: &Theme,
    highlight_query: Option<&str>,
    summary_inflight: bool,
) -> Text<'static> {
    let mut lines = render_summary_sections(summaries, theme, highlight_query, summary_inflight).lines;
    if !lines.is_empty() && session.is_some() {
        lines.push(Line::default());
    }

    lines.extend(render_session_section(session, theme, highlight_query).lines);
    Text::from(lines)
}

pub fn render_summary_sections(
    summaries: &SummarySources,
    theme: &Theme,
    highlight_query: Option<&str>,
    summary_inflight: bool,
) -> Text<'static> {
    let mut lines = Vec::new();

    for (index, summary) in summaries.claude_autosummaries.iter().enumerate() {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        let title = if summaries.claude_autosummaries.len() > 1 {
            format!("# Claude Auto-summary #{}", index + 1)
        } else {
            "# Claude Auto-summary".to_owned()
        };
        lines.extend(render_section(&title, &render_claude_summary_text(summary, theme, highlight_query), theme).lines);
    }

    if let Some(summary) = summaries.aics_sidecar.as_ref() {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.extend(render_section("# AICS summary", &render_aics_summary_text(summary, theme, highlight_query), theme).lines);
    } else if summary_inflight {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.extend(render_section(
            "# AICS summary",
            &render_summary_missing(theme, "Summary is being generated…", false),
            theme,
        ).lines);
    }

    Text::from(lines)
}

pub fn render_session_section(
    session: Option<&Session>,
    theme: &Theme,
    highlight_query: Option<&str>,
) -> Text<'static> {
    let body = if let Some(session) = session {
        render_session_text(session, theme, highlight_query)
    } else {
        render_summary_missing(theme, "Session log is unavailable for this entry.", false)
    };
    render_section("# Session Log", &body, theme)
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
    let (_, bubble_bg) = message_colors(agent, role, theme);
    let base = Style::default().fg(theme.text).bg(bubble_bg);
    render_markdown_message(content, theme, base, highlight_query)
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
    let viewport_height = area.height.saturating_sub(2) as usize;
    let viewport_width = area.width.saturating_sub(2);
    wrapped_text_height(text, viewport_width).saturating_sub(viewport_height)
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
        normalize_highlight_query, render_composite_text, render_session_text, render_summary_missing,
        render_summary_text,
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
