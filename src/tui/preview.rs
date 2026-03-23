use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::parse::{Agent, MessageRole, Session};
use crate::tui::app::{App, Focus};
use crate::tui::theme::Theme;
use crate::tui::util::{highlight_spans, role_label, wrapped_text_height};

pub fn render(frame: &mut Frame, app: &mut App, area: Rect, theme: &Theme) {
    let focused = matches!(app.focus, Focus::Preview);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border_style(focused))
        .title("Preview");

    let text = if let Some(session) = app.selected_preview() {
        render_session_text(session, theme, None)
    } else {
        Text::from(Line::from(Span::styled(
            "Select a session to preview",
            Style::default().fg(theme.muted),
        )))
    };
    let max_scroll = scroll_limit_for_text(&text, area);
    app.preview_scroll = app.preview_scroll.min(max_scroll);

    let paragraph = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.preview_scroll as u16, 0));
    frame.render_widget(paragraph, area);
}

pub fn max_scroll(area: Rect, session: Option<&Session>, theme: &Theme) -> usize {
    let text = if let Some(session) = session {
        render_session_text(session, theme, None)
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
    let mut lines = Vec::new();
    for message in &session.messages {
        let (label_color, bubble_bg) = message_colors(session.agent, message.role, theme);
        lines.push(Line::from(vec![
            Span::styled(
                role_label(message.role),
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

        for content_line in message.content.lines() {
            let base = Style::default().fg(theme.text).bg(bubble_bg);
            let highlight = theme.highlight_style().bg(bubble_bg);
            lines.push(Line::from(highlight_spans(
                content_line,
                highlight_query.unwrap_or_default(),
                base,
                highlight,
            )));
        }
        lines.push(Line::default());
    }
    Text::from(lines)
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
    }
}

fn scroll_limit_for_text(text: &Text<'_>, area: Rect) -> usize {
    let viewport_height = area.height.saturating_sub(2) as usize;
    let viewport_width = area.width.saturating_sub(2);
    wrapped_text_height(text, viewport_width).saturating_sub(viewport_height)
}
