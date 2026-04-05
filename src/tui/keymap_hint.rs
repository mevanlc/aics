use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    Frame,
};

use super::theme::Theme;

/// A single key-binding hint (e.g. "^F" + "filters").
#[derive(Debug, Clone)]
pub struct KeymapHint {
    pub key: &'static str,
    pub desc: &'static str,
}

impl KeymapHint {
    pub const fn new(key: &'static str, desc: &'static str) -> Self {
        Self { key, desc }
    }

    /// Total display width: "key desc" (key + space + desc).
    pub fn width(&self) -> usize {
        self.key.len() + 1 + self.desc.len()
    }

    /// Render as styled spans: bold key, then muted description.
    pub fn spans(&self, theme: &Theme) -> Vec<Span<'static>> {
        vec![
            Span::styled(
                self.key,
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {}", self.desc), Style::default().fg(theme.muted)),
        ]
    }
}

const SEP: &str = "  ";
const SEP_WIDTH: usize = 2;
const PAD: &str = " ";
const PAD_WIDTH: usize = 1;

/// Lay out hints into lines that fit within `width`, wrapping atomically.
/// Returns up to `max_lines` lines.
pub fn layout_hints<'a>(
    hints: &[KeymapHint],
    width: usize,
    max_lines: usize,
    theme: &Theme,
    prefix: Option<Vec<Span<'a>>>,
) -> Vec<Line<'a>> {
    if max_lines == 0 || width == 0 {
        return vec![];
    }

    let mut lines: Vec<Line<'a>> = Vec::new();
    let mut spans: Vec<Span<'a>> = vec![Span::raw(PAD)];
    let mut line_width: usize = PAD_WIDTH;

    // If there's a prefix (status text), start with it on the first line.
    if let Some(prefix_spans) = prefix {
        let prefix_width: usize = prefix_spans.iter().map(|s| s.content.len()).sum();
        if prefix_width > 0 {
            spans.extend(prefix_spans);
            // Add separator after prefix
            spans.push(Span::styled(SEP, Style::default().fg(theme.muted)));
            line_width += prefix_width + SEP_WIDTH;
        }
    }

    for (i, hint) in hints.iter().enumerate() {
        let hint_w = hint.width();
        let need = if spans.is_empty() || (i == 0 && line_width == 0) {
            hint_w
        } else if line_width == 0 {
            hint_w
        } else {
            SEP_WIDTH + hint_w
        };

        if line_width + need > width && line_width > PAD_WIDTH {
            // Wrap to next line.
            lines.push(Line::from(spans));
            spans = vec![Span::raw(PAD)];
            line_width = PAD_WIDTH;
            if lines.len() >= max_lines {
                return lines;
            }
        }

        // Add separator before this hint (if not first on the line).
        if line_width > PAD_WIDTH {
            spans.push(Span::styled(SEP, Style::default().fg(theme.muted)));
            line_width += SEP_WIDTH;
        }

        spans.extend(hint.spans(theme));
        line_width += hint_w;
    }

    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }
    lines.truncate(max_lines);
    lines
}

/// Render the keymap hint bar into the given area.
pub fn render(frame: &mut Frame, area: Rect, hints: &[KeymapHint], theme: &Theme, status: &str) {
    let prefix = if status.is_empty() {
        None
    } else {
        Some(vec![Span::styled(
            status.to_owned(),
            Style::default().fg(theme.text),
        )])
    };

    let lines = layout_hints(hints, area.width as usize, area.height as usize, theme, prefix);

    // Render each line at the corresponding row.
    for (i, line) in lines.into_iter().enumerate() {
        let row = area.y + i as u16;
        if row >= area.bottom() {
            break;
        }
        let line_area = Rect::new(area.x, row, area.width, 1);
        frame.render_widget(ratatui::widgets::Paragraph::new(line), line_area);
    }
}
