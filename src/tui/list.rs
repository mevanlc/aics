use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, ListState};
use ratatui::Frame;

use crate::index::SearchHit;
use crate::tui::app::{App, Focus};
use crate::tui::theme::Theme;
use crate::tui::util::{
    agent_badge, list_meta, list_title, parse_highlighted_html, truncate_plain,
};

pub const ITEM_HEIGHT: usize = 3;

pub fn render(frame: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let focused = matches!(app.focus, Focus::List);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border_style(focused))
        .title("Sessions");

    let items = if app.results.is_empty() {
        let empty_state = if app.is_searching() {
            "Searching..."
        } else {
            "No matching sessions"
        };
        vec![ListItem::new(Line::from(Span::styled(
            empty_state,
            Style::default().fg(theme.muted),
        )))]
    } else {
        let visible_slots = visible_slots(area);
        let (visible_hits, _) = app.list_window(visible_slots);
        visible_hits
            .iter()
            .map(|hit| render_item(hit, theme))
            .collect::<Vec<_>>()
    };

    let list = List::new(items)
        .block(block)
        .highlight_symbol("› ")
        .highlight_style(
            Style::default()
                .bg(theme.selection)
                .add_modifier(Modifier::BOLD),
        );

    let selected = if app.results.is_empty() {
        None
    } else {
        let visible_slots = visible_slots(area);
        let (_, selected) = app.list_window(visible_slots);
        selected
    };
    let mut state = ListState::default();
    state.select(selected);
    frame.render_stateful_widget(list, area, &mut state);
}

pub fn visible_slots(area: Rect) -> usize {
    let content_height = area.height.saturating_sub(2) as usize;
    (content_height / ITEM_HEIGHT).max(1)
}

pub fn slot_at_row(area: Rect, row: u16) -> Option<usize> {
    let inner_top = area.y.saturating_add(1);
    let inner_bottom = area.bottom().saturating_sub(1);
    if row < inner_top || row >= inner_bottom {
        return None;
    }

    Some(((row - inner_top) as usize) / ITEM_HEIGHT)
}

fn render_item(hit: &SearchHit, theme: &Theme) -> ListItem<'static> {
    let (badge, badge_color) = agent_badge(hit.session.agent, theme);
    let title_text = truncate_plain(&list_title(hit), 56);
    let title = Line::from(vec![
        Span::styled(format!("[{badge}] "), Style::default().fg(badge_color)),
        Span::styled(title_text, Style::default().fg(theme.text)),
    ]);
    let snippet = parse_highlighted_html(
        &hit.snippet_html,
        Style::default().fg(theme.text),
        theme.highlight_style(),
    );
    let meta = Line::from(Span::styled(
        truncate_plain(&list_meta(hit), 72),
        Style::default().fg(theme.muted),
    ));

    ListItem::new(vec![title, snippet, meta])
}
