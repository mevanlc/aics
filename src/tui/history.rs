use std::time::{Duration, Instant};

use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32String};
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;
use unicode_segmentation::UnicodeSegmentation;

use super::{
    keymap_hint::{self, KeymapHint},
    layout,
    theme::Theme,
    util::block_title,
};
use crate::search_history::HistoryEntry;

#[derive(Debug, PartialEq, Eq)]
pub enum HistoryOutcome {
    Stay,
    Close,
    Select(String),
}

#[derive(Debug)]
struct HistoryMatch {
    entry: usize,
    score: u32,
    indices: Vec<u32>,
}

#[derive(Debug)]
pub struct HistoryModalState {
    input: Input,
    entries: Vec<HistoryEntry>,
    candidates: Vec<Utf32String>,
    matcher: Matcher,
    matches: Vec<HistoryMatch>,
    selected: usize,
    offset: usize,
    page_size: usize,
    last_click: Option<(usize, Instant)>,
}

impl HistoryModalState {
    pub fn new(entries: Vec<HistoryEntry>, query: &str) -> Self {
        let candidates = entries
            .iter()
            .map(|entry| Utf32String::from(entry.query.as_str()))
            .collect();
        let mut state = Self {
            input: Input::new(query.to_owned()),
            entries,
            candidates,
            matcher: Matcher::new(Config::DEFAULT),
            matches: Vec::new(),
            selected: 0,
            offset: 0,
            page_size: 1,
            last_click: None,
        };
        state.filter();
        state
    }

    fn filter(&mut self) {
        let pattern = Pattern::parse(
            self.input.value(),
            CaseMatching::Smart,
            Normalization::Smart,
        );
        self.matches.clear();
        for (entry, candidate) in self.candidates.iter().enumerate() {
            let mut indices = Vec::new();
            if let Some(score) =
                pattern.indices(candidate.slice(..), &mut self.matcher, &mut indices)
            {
                indices.sort_unstable();
                indices.dedup();
                self.matches.push(HistoryMatch {
                    entry,
                    score,
                    indices,
                });
            }
        }
        self.matches.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then(
                    self.entries[b.entry]
                        .saved_at
                        .cmp(&self.entries[a.entry].saved_at),
                )
                .then(
                    self.entries[a.entry]
                        .query
                        .cmp(&self.entries[b.entry].query),
                )
        });
        self.selected = 0;
        self.offset = 0;
        self.last_click = None;
    }

    fn accept(&self) -> HistoryOutcome {
        self.matches
            .get(self.selected)
            .map_or(HistoryOutcome::Stay, |matched| {
                HistoryOutcome::Select(self.entries[matched.entry].query.clone())
            })
    }

    fn move_selection(&mut self, delta: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.matches.len().saturating_sub(1));
        self.ensure_visible();
        self.last_click = None;
    }

    fn ensure_visible(&mut self) {
        self.offset = self.offset.min(self.selected);
        if self.selected >= self.offset + self.page_size {
            self.offset = self.selected + 1 - self.page_size;
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> HistoryOutcome {
        match key.code {
            KeyCode::Esc => return HistoryOutcome::Close,
            KeyCode::Enter => return self.accept(),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('n') if key.modifiers == KeyModifiers::CONTROL => self.move_selection(1),
            KeyCode::Char('p') if key.modifiers == KeyModifiers::CONTROL => self.move_selection(-1),
            KeyCode::PageDown => self.move_selection(self.page_size as isize),
            KeyCode::PageUp => self.move_selection(-(self.page_size as isize)),
            _ => {
                if self
                    .input
                    .handle_event(&Event::Key(key))
                    .is_some_and(|change| change.value)
                {
                    self.filter();
                }
            }
        }
        HistoryOutcome::Stay
    }

    pub fn handle_mouse(&mut self, area: Rect, mouse: MouseEvent) -> HistoryOutcome {
        let (_, _, list, _) = areas(area);
        if !list.contains(Position::new(mouse.column, mouse.row)) {
            self.last_click = None;
            return HistoryOutcome::Stay;
        }
        self.page_size = usize::from(list.height).max(1);
        match mouse.kind {
            MouseEventKind::ScrollDown => self.move_selection(1),
            MouseEventKind::ScrollUp => self.move_selection(-1),
            MouseEventKind::Down(MouseButton::Left) => {
                let index = self.offset + usize::from(mouse.row - list.y);
                if index < self.matches.len() {
                    self.selected = index;
                    let now = Instant::now();
                    if self.last_click.is_some_and(|(previous, at)| {
                        previous == index && now.duration_since(at) <= Duration::from_millis(500)
                    }) {
                        return self.accept();
                    }
                    self.last_click = Some((index, now));
                }
            }
            _ => {}
        }
        HistoryOutcome::Stay
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let (popup, input, list, hints) = areas(area);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(theme.border_style(true))
                .title(block_title(format!(
                    "Search History · {}/{}",
                    self.matches.len(),
                    self.entries.len()
                ))),
            popup,
        );
        if input.width > 2 && input.height > 0 {
            let width = usize::from(input.width - 2);
            let scroll = self.input.visual_scroll(width);
            frame.render_widget(
                Paragraph::new("> ").style(Style::default().fg(theme.accent)),
                input,
            );
            let text_area = Rect::new(input.x + 2, input.y, input.width - 2, input.height);
            frame.render_widget(
                Paragraph::new(self.input.value())
                    .scroll((0, scroll.min(u16::MAX as usize) as u16))
                    .style(Style::default().fg(theme.text)),
                text_area,
            );
            let cursor = self
                .input
                .visual_cursor()
                .saturating_sub(scroll)
                .min(width.saturating_sub(1));
            frame.set_cursor_position((text_area.x + cursor as u16, text_area.y));
        }
        self.page_size = usize::from(list.height).max(1);
        self.ensure_visible();
        if self.matches.is_empty() {
            frame.render_widget(
                Paragraph::new(if self.entries.is_empty() {
                    "No search history"
                } else {
                    "No matches"
                })
                .style(Style::default().fg(theme.muted)),
                list,
            );
        }
        for (row, matched) in self
            .matches
            .iter()
            .skip(self.offset)
            .take(list.height as usize)
            .enumerate()
        {
            let selected = self.offset + row == self.selected;
            let style = if selected {
                Style::default().fg(theme.text).bg(theme.selection)
            } else {
                Style::default().fg(theme.text)
            };
            let mut spans = vec![Span::styled(if selected { "> " } else { "  " }, style)];
            spans.extend(highlighted_query(
                &self.entries[matched.entry].query,
                &matched.indices,
                style,
                theme,
            ));
            frame.render_widget(
                Paragraph::new(Line::from(spans)).style(style),
                Rect::new(list.x, list.y + row as u16, list.width, 1),
            );
        }
        const HINTS: [KeymapHint; 3] = [
            KeymapHint::new("↑↓/^P/^N", "select"),
            KeymapHint::new("⏎", "recall"),
            KeymapHint::new("Esc", "cancel"),
        ];
        keymap_hint::render(frame, hints, &HINTS, theme, "");
    }
}

fn areas(area: Rect) -> (Rect, Rect, Rect, Rect) {
    let popup = layout::centered_rect(area, 80, 80);
    let inner = Block::default().borders(Borders::ALL).inner(popup);
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(inner);
    (popup, rows[0], rows[2], rows[3])
}

fn highlighted_query<'a>(
    query: &'a str,
    indices: &[u32],
    style: Style,
    theme: &Theme,
) -> Vec<Span<'a>> {
    // Nucleo represents each grapheme by its first codepoint. Highlight the
    // corresponding original grapheme, preserving combining marks and emoji.
    query
        .graphemes(true)
        .enumerate()
        .map(|(index, grapheme)| {
            let style = if indices.binary_search(&(index as u32)).is_ok() {
                style.fg(theme.highlight).add_modifier(Modifier::BOLD)
            } else {
                style
            };
            if grapheme.chars().any(char::is_control) {
                Span::styled(" ", style)
            } else {
                Span::styled(grapheme, style)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    use ratatui::{backend::TestBackend, Terminal};

    fn entry(query: &str, timestamp: i64) -> HistoryEntry {
        HistoryEntry {
            query: query.into(),
            saved_at: DateTime::from_timestamp(timestamp, 0).unwrap(),
        }
    }

    #[test]
    fn filter_ranks_matches_then_recency_and_accepts_selection() {
        let mut state = HistoryModalState::new(
            vec![entry("a long b", 3), entry("ab", 1), entry("AB", 2)],
            "ab",
        );
        assert_eq!(state.input.value(), "ab");
        assert_eq!(state.accept(), HistoryOutcome::Select("AB".into()));
        state.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(state.accept(), HistoryOutcome::Select("ab".into()));
        state.input = Input::new("missing".into());
        state.filter();
        assert_eq!(
            state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            HistoryOutcome::Stay
        );
        assert_eq!(
            state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            HistoryOutcome::Close
        );
        state.input = Input::default();
        state.filter();
        assert_eq!(state.accept(), HistoryOutcome::Select("a long b".into()));
    }

    #[test]
    fn smart_case_patterns_and_unicode_highlights_preserve_graphemes() {
        let mut state = HistoryModalState::new(
            vec![entry("cafe\u{301} 界 👩‍💻", 1), entry("CAFÉ", 2)],
            "cafe",
        );
        assert_eq!(state.matches.len(), 2);
        state.input = Input::new("^cafe !missing".into());
        state.filter();
        let matched = state.matches.iter().find(|m| m.entry == 0).unwrap();
        let spans = highlighted_query(
            &state.entries[0].query,
            &matched.indices,
            Style::default(),
            &Theme::default(),
        );
        assert!(spans
            .iter()
            .any(|span| span.content == "e\u{301}"
                && span.style.add_modifier.contains(Modifier::BOLD)));
        assert!(spans.iter().any(|span| span.content == "👩‍💻"));
        state.input = Input::new("CAFÉ".into());
        state.filter();
        assert_eq!(state.matches.len(), 1);
    }

    #[test]
    fn rendering_handles_small_terminals_scrolling_and_double_click() {
        let entries = (0..30).map(|i| entry(&format!("search {i}"), i)).collect();
        let mut state = HistoryModalState::new(entries, "");
        for (width, height) in [(1, 1), (20, 5), (80, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| state.render(frame, frame.area(), &Theme::default()))
                .unwrap();
        }
        state.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert!(state.offset > 0);
        let area = Rect::new(0, 0, 80, 24);
        let (_, _, list, _) = areas(area);
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: list.x,
            row: list.y,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(state.handle_mouse(area, click), HistoryOutcome::Stay);
        assert!(matches!(
            state.handle_mouse(area, click),
            HistoryOutcome::Select(_)
        ));
    }
}
