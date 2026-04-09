use ratatui::layout::{Constraint, Layout, Rect};

#[derive(Debug, Clone, Copy)]
pub struct AppLayout {
    pub search: Rect,
    pub list: Rect,
    pub preview: Option<Rect>,
    pub status: Rect,
}

pub fn split(area: Rect, preview_width_pct: u16, show_preview: bool) -> AppLayout {
    let vertical = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(2), // 2 rows for keymap hints, no border
    ])
    .split(area);

    let show_preview = show_preview && vertical[1].width > 44;
    let body = if show_preview {
        let preview_width_pct = preview_width_pct.clamp(25, 75);
        let list_width_pct = 100u16.saturating_sub(preview_width_pct);
        Layout::horizontal([
            Constraint::Percentage(list_width_pct),
            Constraint::Percentage(preview_width_pct),
        ])
        .split(vertical[1])
    } else {
        Layout::horizontal([Constraint::Percentage(100)]).split(vertical[1])
    };

    AppLayout {
        search: vertical[0],
        list: body[0],
        preview: show_preview.then(|| body[1]),
        status: vertical[2],
    }
}

pub fn centered_rect(area: Rect, width_pct: u16, height_pct: u16) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Percentage((100u16.saturating_sub(height_pct)).saturating_div(2)),
        Constraint::Percentage(height_pct),
        Constraint::Percentage((100u16.saturating_sub(height_pct)).saturating_div(2)),
    ])
    .split(area);

    Layout::horizontal([
        Constraint::Percentage((100u16.saturating_sub(width_pct)).saturating_div(2)),
        Constraint::Percentage(width_pct),
        Constraint::Percentage((100u16.saturating_sub(width_pct)).saturating_div(2)),
    ])
    .split(popup_layout[1])[1]
}
