use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, ListState};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::index::SearchHit;
use crate::tui::app::App;
use crate::tui::theme::Theme;
use crate::tui::util::{
    agent_badge, block_title, format_line_count, list_title, relative_time, truncate_plain,
};

fn card_height(snippet_line_count: usize, separator: &str, extra_row_count: usize) -> usize {
    let body = if snippet_line_count == 0 {
        0
    } else {
        snippet_line_count
    };
    let sep = if separator.is_empty() { 0 } else { 1 };
    1 + body + extra_row_count + sep // header + snippet + extra rows + separator
}

/// Use compact 1-line items when the content area can't fit a single card.
fn effective_item_height(
    area: Rect,
    snippet_line_count: usize,
    separator: &str,
    extra_row_count: usize,
) -> usize {
    let content_height = area.height.saturating_sub(2) as usize;
    let full = card_height(snippet_line_count, separator, extra_row_count);
    if content_height >= full {
        full
    } else {
        1
    }
}

pub fn render(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    theme: &Theme,
    separator: &str,
    snippet_line_count: usize,
    extra_row_count: usize,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border_style(false))
        .title(block_title(Span::styled(
            "Sessions",
            Style::default().fg(theme.accent),
        )));

    let compact = effective_item_height(area, snippet_line_count, separator, extra_row_count) == 1;
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
        let visible_slots = visible_slots(area, snippet_line_count, separator, extra_row_count);
        let (visible_hits, selected_within) = app.list_window(visible_slots);
        let visible_hits = visible_hits.to_vec();
        let content_width = area.width.saturating_sub(3) as usize;
        if compact {
            visible_hits
                .iter()
                .enumerate()
                .map(|(i, hit)| {
                    render_item_compact(hit, theme, content_width, selected_within == Some(i))
                })
                .collect::<Vec<_>>()
        } else {
            visible_hits
                .iter()
                .enumerate()
                .map(|(i, hit)| {
                    let item_separator = if i + 1 == visible_hits.len() {
                        ""
                    } else {
                        separator
                    };
                    let snippet = app.list_snippet_line(hit, theme);
                    let extra_line = app.list_rule_line(hit, theme);
                    let render_ctx = RenderItemContext {
                        theme,
                        width: content_width,
                        selected: selected_within == Some(i),
                        separator: item_separator,
                        snippet_line_count,
                    };
                    render_item(hit, snippet, extra_line, &render_ctx)
                })
                .collect::<Vec<_>>()
        }
    };

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));

    let selected = if app.results.is_empty() {
        None
    } else {
        let visible_slots = visible_slots(area, snippet_line_count, separator, extra_row_count);
        let (_, selected) = app.list_window(visible_slots);
        selected
    };
    let mut state = ListState::default();
    state.select(selected);
    frame.render_stateful_widget(list, area, &mut state);
}

pub fn visible_slots(
    area: Rect,
    snippet_line_count: usize,
    separator: &str,
    extra_row_count: usize,
) -> usize {
    let content_height = area.height.saturating_sub(2) as usize;
    let ih = effective_item_height(area, snippet_line_count, separator, extra_row_count);
    if ih == 1 {
        return content_height.max(1);
    }

    if separator.is_empty() {
        return (content_height / ih).max(1);
    }

    // Full cards include a separator, but the last visible card can omit it.
    ((content_height.saturating_add(1)) / ih).max(1)
}

pub fn slot_at_row(
    area: Rect,
    row: u16,
    snippet_line_count: usize,
    separator: &str,
    extra_row_count: usize,
) -> Option<usize> {
    let inner_top = area.y.saturating_add(1);
    let inner_bottom = area.bottom().saturating_sub(1);
    if row < inner_top || row >= inner_bottom {
        return None;
    }

    let ih = effective_item_height(area, snippet_line_count, separator, extra_row_count);
    let inner_row = (row - inner_top) as usize;
    if ih == 1 {
        return Some(inner_row);
    }

    let slots = visible_slots(area, snippet_line_count, separator, extra_row_count);
    let max_rows_used = if separator.is_empty() {
        slots.saturating_mul(ih)
    } else {
        slots.saturating_mul(ih).saturating_sub(1)
    };
    if inner_row >= max_rows_used {
        return None;
    }

    let slot = inner_row / ih;
    (slot < slots).then_some(slot)
}

#[derive(Clone, Copy)]
struct RenderItemContext<'a> {
    theme: &'a Theme,
    width: usize,
    selected: bool,
    separator: &'a str,
    snippet_line_count: usize,
}

fn render_item(
    hit: &SearchHit,
    snippet: Line<'static>,
    extra_line: Option<Line<'static>>,
    render_ctx: &RenderItemContext<'_>,
) -> ListItem<'static> {
    let chevron = if render_ctx.selected { "⟩" } else { " " };
    let chevron_style = Style::default()
        .fg(render_ctx.theme.accent)
        .add_modifier(Modifier::BOLD);
    let item_width = render_ctx.width.saturating_sub(1); // 1 for chevron

    let (badge, badge_color) = agent_badge(hit.session.agent, render_ctx.theme);
    let meta_prefix = format!("{{{badge}}}");
    let mut meta_suffix = format!(
        " · {} · {}",
        relative_time(hit.session.modified_ts),
        format_line_count(hit.session.lines)
    );
    if hit.is_live {
        meta_suffix.push_str(" · live");
    }
    if let Some(info) = hit.session.session_info.as_ref() {
        if let Some(model) = info.model.as_deref().filter(|s| !s.is_empty()) {
            meta_suffix.push_str(" · ");
            meta_suffix.push_str(model);
        }
    }
    let meta_width =
        UnicodeWidthStr::width(meta_prefix.as_str()) + UnicodeWidthStr::width(meta_suffix.as_str());

    let title_budget = item_width.saturating_sub(meta_width.saturating_add(1));
    let title_text = truncate_with_ellipsis(&list_title(hit), title_budget);
    let title_width = UnicodeWidthStr::width(title_text.as_str());
    let padding = " ".repeat(item_width.saturating_sub(meta_width + title_width).max(1));
    let header_fg = if render_ctx.selected {
        render_ctx.theme.accent
    } else {
        render_ctx.theme.text
    };
    let header_bg = if render_ctx.selected {
        render_ctx.theme.selected_list_header_bg()
    } else {
        render_ctx.theme.list_header_bg
    };
    let header = Line::from(vec![
        Span::styled(chevron, chevron_style),
        Span::styled(
            meta_prefix,
            Style::default()
                .fg(badge_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            meta_suffix,
            Style::default().fg(if render_ctx.selected {
                render_ctx.theme.accent
            } else {
                render_ctx.theme.muted
            }),
        ),
        Span::raw(padding),
        Span::styled(title_text, Style::default().fg(header_fg)),
    ])
    .patch_style(Style::default().bg(header_bg));

    let mut snippet_rows = wrap_line(snippet, item_width, render_ctx.snippet_line_count);

    let body_style = Style::default().bg(if render_ctx.selected {
        render_ctx.theme.selected_list_body_bg()
    } else {
        render_ctx.theme.list_body_bg
    });
    let separator_style = Style::default().bg(render_ctx.theme.list_body_bg);
    while snippet_rows.len() < render_ctx.snippet_line_count {
        snippet_rows.push(Line::styled(" ".repeat(item_width), body_style));
    }

    let mut lines = vec![header];
    lines.extend(
        snippet_rows
            .into_iter()
            .map(|mut line| {
                line.spans.insert(0, Span::raw(" "));
                line
            })
            .map(|line| line.patch_style(body_style)),
    );
    if let Some(extra_line) = extra_line {
        let mut extra_rows = wrap_line(extra_line, item_width, 1);
        let mut line = extra_rows.pop().unwrap_or_default();
        line.spans.insert(0, Span::raw(" "));
        lines.push(line.patch_style(body_style));
    }
    if !render_ctx.separator.is_empty() {
        lines.push(
            build_separator_line(render_ctx.separator, item_width + 1, render_ctx.theme)
                .patch_style(separator_style),
        );
    }
    ListItem::new(lines)
}

fn build_separator_line(separator: &str, width: usize, theme: &Theme) -> Line<'static> {
    if separator.trim().is_empty() {
        return Line::default();
    }
    let mut text = String::from(" ");
    let content_width = width.saturating_sub(1);
    let sep_width = UnicodeWidthStr::width(separator);
    if sep_width == 0 || content_width == 0 {
        return Line::from(Span::styled(text, Style::default().fg(theme.muted_greater)));
    }
    let full_repeats = content_width / sep_width;
    let remainder = content_width % sep_width;
    text.push_str(&separator.repeat(full_repeats));
    if remainder > 0 {
        text.push_str(&truncate_plain(separator, remainder));
    }
    Line::from(Span::styled(text, Style::default().fg(theme.muted_greater)))
}

fn render_item_compact(
    hit: &SearchHit,
    theme: &Theme,
    width: usize,
    selected: bool,
) -> ListItem<'static> {
    let chevron = if selected { "⟩" } else { " " };
    let item_width = width.saturating_sub(1);
    let (badge, badge_color) = agent_badge(hit.session.agent, theme);
    let time = relative_time(hit.session.modified_ts);
    let prefix = format!("{{{badge}}} {time} ");
    let prefix_width = UnicodeWidthStr::width(prefix.as_str());
    let title_budget = item_width.saturating_sub(prefix_width);
    let title_text = truncate_with_ellipsis(&list_title(hit), title_budget);
    ListItem::new(
        Line::from(vec![
            Span::styled(
                chevron,
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(prefix, Style::default().fg(badge_color)),
            Span::styled(
                title_text,
                Style::default().fg(if selected { theme.accent } else { theme.text }),
            ),
        ])
        .patch_style(Style::default().bg(if selected {
            theme.selected_list_header_bg()
        } else {
            theme.list_header_bg
        })),
    )
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
                rows[row_index].push(Span::styled(
                    truncate_with_ellipsis(&segment, width),
                    span.style,
                ));
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
        if current_is_whitespace.is_some_and(|state| state != is_whitespace) && !current.is_empty()
        {
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
        let next = truncate_plain(
            last_span.content.as_ref(),
            UnicodeWidthStr::width(last_span.content.as_ref()).saturating_sub(1),
        );
        if next.is_empty() {
            let removed = last_row.pop().expect("last span should exist");
            *last_width =
                last_width.saturating_sub(UnicodeWidthStr::width(removed.content.as_ref()));
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
    use std::path::PathBuf;

    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Style};
    use ratatui::text::Line;
    use ratatui::widgets::{List, ListState};
    use ratatui::Terminal;

    use super::{
        build_separator_line, format_line_count, render_item, slot_at_row, truncate_with_ellipsis,
        visible_slots, wrap_line, RenderItemContext,
    };
    use crate::index::{SearchHit, StoredSession};
    use crate::parse::{Agent, DerivationType};
    use crate::tui::theme::Theme;

    fn plain_snippet(text: &str) -> Line<'static> {
        crate::tui::util::parse_highlighted_html(text, Style::default(), Style::default())
    }

    #[test]
    fn formats_large_line_counts_with_grouping() {
        assert_eq!(format_line_count(1_234), "1,234 lines");
    }

    #[test]
    fn truncates_with_ellipsis_when_needed() {
        assert_eq!(
            truncate_with_ellipsis("cozy-sleeping-quasar", 8),
            "cozy-sl…"
        );
    }

    #[test]
    fn wraps_snippet_into_three_lines_with_ellipsis() {
        let line = crate::tui::util::parse_highlighted_html(
            "copy the skills commands and prompts from everywhere forever and ever",
            Style::default(),
            Style::default(),
        );
        let wrapped = wrap_line(line, 24, 3);

        assert_eq!(wrapped.len(), 3);
        let last = wrapped[2]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(last.ends_with('…'));
    }

    #[test]
    fn snippet_highlight_preserves_base_text_color() {
        let line = crate::tui::util::parse_highlighted_html(
            "plain <b>match</b>",
            Style::default().fg(Color::White),
            Style::default().bg(Color::Blue),
        );

        assert_eq!(line.spans[1].content.as_ref(), "match");
        assert_eq!(line.spans[1].style.fg, Some(Color::White));
        assert_eq!(line.spans[1].style.bg, Some(Color::Blue));
    }

    #[test]
    fn slot_at_row_tracks_five_row_cards() {
        let area = Rect::new(0, 4, 72, 20);

        assert_eq!(slot_at_row(area, 4, 3, " ", 0), None);
        assert_eq!(slot_at_row(area, 5, 3, " ", 0), Some(0));
        assert_eq!(slot_at_row(area, 9, 3, " ", 0), Some(0));
        assert_eq!(slot_at_row(area, 10, 3, " ", 0), Some(1));
        assert_eq!(slot_at_row(area, 14, 3, " ", 0), Some(1));
        assert_eq!(slot_at_row(area, 15, 3, " ", 0), Some(2));
    }

    #[test]
    fn slot_at_row_without_separator() {
        let area = Rect::new(0, 4, 72, 20);

        assert_eq!(slot_at_row(area, 4, 3, "", 0), None);
        assert_eq!(slot_at_row(area, 5, 3, "", 0), Some(0));
        assert_eq!(slot_at_row(area, 8, 3, "", 0), Some(0));
        assert_eq!(slot_at_row(area, 9, 3, "", 0), Some(1));
        assert_eq!(slot_at_row(area, 12, 3, "", 0), Some(1));
        assert_eq!(slot_at_row(area, 13, 3, "", 0), Some(2));
    }

    #[test]
    fn slot_at_row_with_fewer_snippet_line_count() {
        let area = Rect::new(0, 4, 72, 20);
        // 1 snippet line + separator = 3 rows per card
        assert_eq!(slot_at_row(area, 5, 1, " ", 0), Some(0));
        assert_eq!(slot_at_row(area, 7, 1, " ", 0), Some(0));
        assert_eq!(slot_at_row(area, 8, 1, " ", 0), Some(1));
    }

    #[test]
    fn extra_rule_row_affects_geometry_without_changing_snippet_count() {
        let area = Rect::new(0, 4, 72, 20);
        // 1 snippet line + 1 rule row + separator = 4 rows per card
        assert_eq!(slot_at_row(area, 5, 1, " ", 1), Some(0));
        assert_eq!(slot_at_row(area, 8, 1, " ", 1), Some(0));
        assert_eq!(slot_at_row(area, 9, 1, " ", 1), Some(1));
        assert_eq!(visible_slots(area, 1, " ", 1), 4);
    }

    #[test]
    fn slot_at_row_zero_snippet_line_count() {
        let area = Rect::new(0, 4, 72, 20);
        // 0 snippet lines + separator = 2 rows per card (header + sep)
        assert_eq!(slot_at_row(area, 5, 0, " ", 0), Some(0));
        assert_eq!(slot_at_row(area, 6, 0, " ", 0), Some(0));
        assert_eq!(slot_at_row(area, 7, 0, " ", 0), Some(1));
    }

    #[test]
    fn visible_slots_uses_last_card_without_separator_when_needed() {
        // content height = 9 rows (11 total minus 2 borders)
        // full card height with 3 snippet lines + separator = 5 rows
        // two full cards would be 10 rows, but two cards with no trailing separator is 9 rows.
        let area = Rect::new(0, 0, 72, 11);
        assert_eq!(visible_slots(area, 3, " ", 0), 2);
    }

    #[test]
    fn slot_at_row_ignores_trailing_blank_rows() {
        // content height = 12 rows. With 5-row cards and last separator omitted,
        // only 9 rows are used for two cards, so bottom rows are blank.
        let area = Rect::new(0, 4, 72, 14);
        assert_eq!(visible_slots(area, 3, " ", 0), 2);
        assert_eq!(slot_at_row(area, 5, 3, " ", 0), Some(0));
        assert_eq!(slot_at_row(area, 13, 3, " ", 0), Some(1));
        assert_eq!(slot_at_row(area, 14, 3, " ", 0), None);
        assert_eq!(slot_at_row(area, 15, 3, " ", 0), None);
    }

    #[test]
    fn build_separator_repeats_pattern_to_fill_width() {
        let theme = Theme::lazygit();
        let line = build_separator_line("-·", 7, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, " -·-·-·");
    }

    #[test]
    fn build_separator_single_column_width_is_left_padding_only() {
        let theme = Theme::lazygit();
        let line = build_separator_line("-·", 1, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, " ");
    }

    #[test]
    fn render_item_separator_fills_full_row_width() {
        let theme = Theme::lazygit();
        let hit = SearchHit {
            session: StoredSession {
                session_id: "session-123".to_owned(),
                agent: Agent::Claude,
                project: "/tmp/demo".to_owned(),
                branch: Some("main".to_owned()),
                cwd: Some("/tmp/demo".to_owned()),
                modified_ts: 0,
                lines: 71,
                file_path: PathBuf::from("/tmp/demo/session.jsonl"),
                first_msg_role: None,
                first_msg_content: String::new(),
                last_msg_role: None,
                last_msg_content: String::new(),
                first_user_msg_content: String::new(),
                derivation_type: DerivationType::Original,
                is_sidechain: false,
                custom_title: Some("commit /commit --all".to_owned()),
                session_info: None,
                trashed: false,
                original_path: None,
            },
            snippet_html: String::new(),
            score: 0.0,
            is_live: false,
        };

        let render_ctx = RenderItemContext {
            theme: &theme,
            width: 32,
            selected: false,
            separator: "·",
            snippet_line_count: 0,
        };
        let item = render_item(&hit, plain_snippet(""), None, &render_ctx);
        let backend = TestBackend::new(32, 2);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ListState::default();

        terminal
            .draw(|f| {
                f.render_stateful_widget(List::new(vec![item.clone()]), f.area(), &mut state);
            })
            .unwrap();

        let rendered = terminal.backend().buffer();
        let separator = (0..32)
            .map(|x| rendered[(x, 1)].symbol())
            .collect::<String>();

        assert_eq!(separator, " ·······························");
    }

    #[test]
    fn selected_item_separator_uses_unselected_body_background() {
        let theme = Theme::lazygit();
        let hit = SearchHit {
            session: StoredSession {
                session_id: "session-123".to_owned(),
                agent: Agent::Claude,
                project: "/tmp/demo".to_owned(),
                branch: Some("main".to_owned()),
                cwd: Some("/tmp/demo".to_owned()),
                modified_ts: 0,
                lines: 71,
                file_path: PathBuf::from("/tmp/demo/session.jsonl"),
                first_msg_role: None,
                first_msg_content: String::new(),
                last_msg_role: None,
                last_msg_content: String::new(),
                first_user_msg_content: String::new(),
                derivation_type: DerivationType::Original,
                is_sidechain: false,
                custom_title: Some("commit /commit --all".to_owned()),
                session_info: None,
                trashed: false,
                original_path: None,
            },
            snippet_html: String::new(),
            score: 0.0,
            is_live: false,
        };

        let render_ctx = RenderItemContext {
            theme: &theme,
            width: 32,
            selected: true,
            separator: "·",
            snippet_line_count: 0,
        };
        let item = render_item(&hit, plain_snippet(""), None, &render_ctx);
        let backend = TestBackend::new(32, 2);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ListState::default();

        terminal
            .draw(|f| {
                f.render_stateful_widget(List::new(vec![item.clone()]), f.area(), &mut state);
            })
            .unwrap();

        let rendered = terminal.backend().buffer();
        for x in 0..32 {
            assert_eq!(rendered[(x, 1)].bg, theme.list_body_bg);
        }
    }

    #[test]
    fn build_separator_blank_string_produces_empty_line() {
        let theme = Theme::lazygit();
        let line = build_separator_line(" ", 10, &theme);
        assert!(line.spans.is_empty());
    }
}
