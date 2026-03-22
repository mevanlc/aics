use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, HighlightSpacing, List, ListItem, ListState};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::index::SearchHit;
use crate::tui::app::{App, Focus};
use crate::tui::theme::Theme;
use crate::tui::util::{agent_badge, list_title, parse_highlighted_html, relative_time, truncate_plain};

const PREVIEW_LINES: usize = 3;
pub const ITEM_HEIGHT: usize = PREVIEW_LINES + 2;

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
        let content_width = area.width.saturating_sub(4) as usize;
        visible_hits
            .iter()
            .map(|hit| render_item(hit, theme, content_width))
            .collect::<Vec<_>>()
    };

    let list = List::new(items)
        .block(block)
        .highlight_symbol("› ")
        .highlight_spacing(HighlightSpacing::Always)
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));

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

fn render_item(hit: &SearchHit, theme: &Theme, width: usize) -> ListItem<'static> {
    let (badge, badge_color) = agent_badge(hit.session.agent, theme);
    let meta_prefix = format!("{{{badge}}}");
    let mut meta_suffix = format!(
        " · {} · {}",
        relative_time(hit.session.modified_ts),
        format_line_count(hit.session.lines)
    );
    if hit.is_live {
        meta_suffix.push_str(" · live");
    }
    let meta_width = UnicodeWidthStr::width(meta_prefix.as_str())
        + UnicodeWidthStr::width(meta_suffix.as_str());

    let title_budget = width.saturating_sub(meta_width.saturating_add(1));
    let title_text = truncate_with_ellipsis(&list_title(hit), title_budget);
    let title_width = UnicodeWidthStr::width(title_text.as_str());
    let padding = " ".repeat(width.saturating_sub(meta_width + title_width).max(1));
    let header = Line::from(vec![
        Span::styled(meta_prefix, Style::default().fg(badge_color).add_modifier(Modifier::BOLD)),
        Span::styled(meta_suffix, Style::default().fg(theme.muted)),
        Span::raw(padding),
        Span::styled(title_text, Style::default().fg(theme.text)),
    ])
    .patch_style(Style::default().bg(theme.list_header_bg));

    let snippet = parse_highlighted_html(
        &hit.snippet_html,
        Style::default().fg(theme.text),
        theme.highlight_style(),
    );
    let mut snippet_lines = wrap_line(snippet, width, PREVIEW_LINES);
    while snippet_lines.len() < PREVIEW_LINES {
        snippet_lines.push(Line::default());
    }

    let body_style = Style::default().bg(theme.list_body_bg);
    let mut lines = vec![header];
    lines.extend(
        snippet_lines
            .into_iter()
            .map(|line| line.patch_style(body_style)),
    );
    lines.push(Line::default());
    ListItem::new(lines)
}

fn format_line_count(lines: usize) -> String {
    let digits = lines.to_string();
    let grouped = digits
        .chars()
        .rev()
        .enumerate()
        .flat_map(|(index, ch)| {
            let mut chunk = Vec::new();
            if index > 0 && index % 3 == 0 {
                chunk.push(',');
            }
            chunk.push(ch);
            chunk
        })
        .collect::<Vec<_>>();
    let grouped = grouped.into_iter().rev().collect::<String>();
    format!("{grouped} lines")
}

fn truncate_with_ellipsis(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(value) <= width {
        return truncate_plain(value, width);
    }
    if width == 1 {
        return "…".to_owned();
    }
    let truncated = truncate_plain(value, width.saturating_sub(1));
    format!("{truncated}…")
}

fn wrap_line(line: Line<'static>, width: usize, max_lines: usize) -> Vec<Line<'static>> {
    if width == 0 || max_lines == 0 {
        return Vec::new();
    }

    let mut rows = vec![Vec::<Span<'static>>::new()];
    let mut row_widths = vec![0usize];
    let mut row_index = 0usize;
    let mut truncated = false;

    for span in line.spans {
        for (segment, is_whitespace) in split_segments(span.content.as_ref()) {
            if is_whitespace && row_widths[row_index] == 0 {
                continue;
            }

            let segment_width = UnicodeWidthStr::width(segment.as_str());
            if row_widths[row_index] + segment_width <= width {
                rows[row_index].push(Span::styled(segment, span.style));
                row_widths[row_index] += segment_width;
                continue;
            }

            if row_index + 1 >= max_lines {
                truncated = true;
                break;
            }

            row_index += 1;
            rows.push(Vec::new());
            row_widths.push(0);
            if is_whitespace {
                continue;
            }

            if segment_width > width {
                rows[row_index].push(Span::styled(truncate_with_ellipsis(&segment, width), span.style));
                row_widths[row_index] = width;
                truncated = true;
                break;
            }

            rows[row_index].push(Span::styled(segment, span.style));
            row_widths[row_index] = segment_width;
        }

        if truncated {
            break;
        }
    }

    if truncated {
        append_ellipsis(&mut rows, &mut row_widths, width);
    }

    rows.into_iter().map(Line::from).collect()
}

fn split_segments(value: &str) -> Vec<(String, bool)> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut current_is_whitespace = None;

    for ch in value.chars() {
        let is_whitespace = ch.is_whitespace();
        if current_is_whitespace.is_some_and(|state| state != is_whitespace) && !current.is_empty() {
            segments.push((current.clone(), current_is_whitespace.unwrap_or(false)));
            current.clear();
        }
        current.push(ch);
        current_is_whitespace = Some(is_whitespace);
    }

    if !current.is_empty() {
        segments.push((current, current_is_whitespace.unwrap_or(false)));
    }

    segments
}

fn append_ellipsis(rows: &mut [Vec<Span<'static>>], row_widths: &mut [usize], width: usize) {
    let Some(last_row) = rows.last_mut() else {
        return;
    };
    let Some(last_width) = row_widths.last_mut() else {
        return;
    };

    while *last_width >= width {
        let Some(last_span) = last_row.last_mut() else {
            break;
        };
        let next = truncate_plain(last_span.content.as_ref(), UnicodeWidthStr::width(last_span.content.as_ref()).saturating_sub(1));
        if next.is_empty() {
            let removed = last_row.pop().expect("last span should exist");
            *last_width = last_width.saturating_sub(UnicodeWidthStr::width(removed.content.as_ref()));
            continue;
        }
        *last_width = last_width.saturating_sub(UnicodeWidthStr::width(last_span.content.as_ref()));
        *last_span = Span::styled(next, last_span.style);
        *last_width += UnicodeWidthStr::width(last_span.content.as_ref());
    }

    if *last_width < width {
        last_row.push(Span::styled(
            "…",
            last_row.last().map(|span| span.style).unwrap_or_default(),
        ));
        *last_width += 1;
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    use super::{format_line_count, slot_at_row, truncate_with_ellipsis, wrap_line, PREVIEW_LINES};

    #[test]
    fn formats_large_line_counts_with_grouping() {
        assert_eq!(format_line_count(1_234), "1,234 lines");
    }

    #[test]
    fn truncates_with_ellipsis_when_needed() {
        assert_eq!(truncate_with_ellipsis("cozy-sleeping-quasar", 8), "cozy-sl…");
    }

    #[test]
    fn wraps_snippet_into_three_lines_with_ellipsis() {
        let line = crate::tui::util::parse_highlighted_html(
            "copy the skills commands and prompts from everywhere forever and ever",
            Style::default(),
            Style::default(),
        );
        let wrapped = wrap_line(line, 24, PREVIEW_LINES);

        assert_eq!(wrapped.len(), PREVIEW_LINES);
        let last = wrapped[PREVIEW_LINES - 1]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(last.ends_with('…'));
    }

    #[test]
    fn slot_at_row_tracks_five_row_cards() {
        let area = Rect::new(0, 4, 72, 20);

        assert_eq!(slot_at_row(area, 4), None);
        assert_eq!(slot_at_row(area, 5), Some(0));
        assert_eq!(slot_at_row(area, 9), Some(0));
        assert_eq!(slot_at_row(area, 10), Some(1));
        assert_eq!(slot_at_row(area, 14), Some(1));
        assert_eq!(slot_at_row(area, 15), Some(2));
    }
}
