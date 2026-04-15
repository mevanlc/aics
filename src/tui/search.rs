use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use crate::tui::app::App;
use crate::tui::theme::Theme;
use crate::tui::util::block_title;

pub fn render(frame: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let scope_label = app.scope_label();
    let status_text = app.title_status_text();

    // ─Search · {scope} · {status}
    // The top border is area.width chars wide; corners take 1 char each, so
    // the title content must fit within area.width - 2.  The "─" prepended by
    // block_title counts as 1 char.
    let fixed_width = 1  // "─" from block_title
        + "Search".chars().count()
        + " · ".chars().count()
        + scope_label.chars().count()
        + " · ".chars().count();
    let status_budget = (area.width as usize)
        .saturating_sub(2 + fixed_width); // 2 for the two border corners
    let status_text: std::borrow::Cow<str> = if status_text.chars().count() <= status_budget {
        status_text.into()
    } else {
        // Truncate, leaving room for the ellipsis character.
        let truncated: String = status_text
            .chars()
            .take(status_budget.saturating_sub(1))
            .collect();
        format!("{truncated}…").into()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border_style(false))
        .title(block_title(Line::from(vec![
            Span::styled("Search", Style::default().fg(theme.accent)),
            Span::styled(
                format!(" · {scope_label}"),
                Style::default().fg(theme.muted),
            ),
            Span::styled(" · ", Style::default().fg(theme.muted)),
            Span::styled(
                status_text,
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
