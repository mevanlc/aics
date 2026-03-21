use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use crate::tui::app::{App, Focus};
use crate::tui::theme::Theme;

pub fn render(frame: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let focused = matches!(app.focus, Focus::Search);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border_style(focused))
        .title(Line::from(vec![
            Span::styled("Search", Style::default().fg(theme.text)),
            Span::styled(
                format!(" · {}", app.scope_label()),
                Style::default().fg(theme.muted),
            ),
        ]));

    let widget = Paragraph::new(app.query.value())
        .style(Style::default().fg(theme.text))
        .block(block);
    frame.render_widget(widget, area);

    if focused {
        let cursor_x = area
            .x
            .saturating_add(1 + app.query.visual_cursor() as u16)
            .min(area.right().saturating_sub(1));
        frame.set_cursor_position((cursor_x, area.y.saturating_add(1)));
    }
}
