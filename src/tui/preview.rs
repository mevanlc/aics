use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::parse::{Agent, MessageRole, Session};
use crate::tui::app::App;
use crate::tui::markdown::render_markdown_message;
use crate::tui::profile;
use crate::tui::theme::Theme;
use crate::tui::util::{role_label, wrapped_text_height};

pub fn render(frame: &mut Frame, app: &mut App, area: Rect, theme: &Theme) {
    let _profile = profile::scope("preview.render");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border_style(false))
        .title("Preview");

    let (text, max_scroll) = if let Some(state) = app.preview_render_state(area, theme) {
        (state.text.clone(), state.max_scroll)
    } else {
        (
            Text::from(Line::from(Span::styled(
                "Select a session to preview",
                Style::default().fg(theme.muted),
            ))),
            0,
        )
    };
    app.preview_scroll = app.preview_scroll.min(max_scroll);

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
        let label = match (&message.role, &message.tool_name) {
            (MessageRole::ToolCall, Some(name)) => format!("\u{203a} {name}"),
            (MessageRole::ToolResult, Some(name)) => format!("\u{2039} {name}"),
            (MessageRole::ToolCall, None) => "\u{203a} tool".to_owned(),
            (MessageRole::ToolResult, None) => "\u{2039} result".to_owned(),
            _ => role_label(message.role).to_owned(),
        };
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

    use chrono::Utc;

    use crate::parse::{Agent, DerivationType, MessageRole, Session, SessionMessage};

    use super::{normalize_highlight_query, render_session_text};
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
}
