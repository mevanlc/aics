use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use crate::tui::app::App;
use crate::tui::theme::Theme;
use crate::tui::util::block_title;

pub fn render(frame: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border_style(false))
        .title(block_title(Line::from(vec![
            Span::styled("Search", Style::default().fg(theme.accent)),
            Span::styled(
                format!(" · {}", app.scope_label()),
                Style::default().fg(theme.muted),
            ),
            Span::styled(" · ", Style::default().fg(theme.muted)),
            Span::styled(
                app.title_status_text(),
                Style::default().fg(theme.muted).add_modifier(Modifier::DIM),
            ),
        ])));

    let widget = Paragraph::new(app.query.value())
        .style(Style::default().fg(theme.text))
        .block(block);
    frame.render_widget(widget, area);

    if app.show_search_cursor() {
        let cursor_x = area
            .x
            .saturating_add(1 + app.query.visual_cursor() as u16)
            .min(area.right().saturating_sub(1));
        frame.set_cursor_position((cursor_x, area.y.saturating_add(1)));
    }
}
