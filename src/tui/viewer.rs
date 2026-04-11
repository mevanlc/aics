use std::path::PathBuf;

use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::parse::Session;
use crate::search_query::extract_highlight_terms;
use crate::settings::ThemeName;
use crate::tui::keymap_hint::{self, KeymapHint};
use crate::tui::preview::{render_message_body, render_session_text};
use crate::tui::profile;
use crate::tui::theme::Theme;
use crate::tui::util::{block_title, session_message_label, wrapped_text_height};

const VIEWER_PAGE_STEP: usize = 12;
const VIEWER_SEARCH_HEIGHT: u16 = 3; // bordered search input
const VIEWER_HINTS_HEIGHT: u16 = 2; // keymap hints (2 wrapping lines)
const VIEWER_FOOTER_HEIGHT: u16 = VIEWER_SEARCH_HEIGHT + VIEWER_HINTS_HEIGHT;
const VIEWER_MATCH_SCROLLOFF: usize = 3;

#[derive(Debug, Clone)]
pub struct ViewerState {
    pub scroll: usize,
    search: Input,
    editing_search: bool,
    active_match: Option<usize>,
    render_cache: Option<ViewerRenderCache>,
}

#[derive(Debug, Clone)]
struct ViewerRenderCache {
    path: PathBuf,
    query: String,
    width: u16,
    theme_name: ThemeName,
    total_rows: usize,
    text: Text<'static>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerOutcome {
    Stay,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchDirection {
    Next,
    Previous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageDirection {
    Next,
    Previous,
}

impl ViewerState {
    const HINTS: [KeymapHint; 6] = [
        KeymapHint::new("↑↓/PgUp/PgDn/Home/End", "scroll"),
        KeymapHint::new("^Up/^Dn", "message"),
        KeymapHint::new("n/p", "matches"),
        KeymapHint::new("Enter", "done"),
        KeymapHint::new("?", "help"),
        KeymapHint::new("Esc", "close"),
    ];

    pub fn new() -> Self {
        Self::with_search("")
    }

    pub fn with_search(query: &str) -> Self {
        Self {
            scroll: 0,
            search: Input::default().with_value(query.to_owned()),
            editing_search: false,
            active_match: None,
            render_cache: None,
        }
    }

    pub fn search_query(&self) -> &str {
        self.search.value()
    }

    pub fn is_editing_search(&self) -> bool {
        self.editing_search
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        area: Rect,
        session: Option<&Session>,
        theme: &Theme,
        theme_name: ThemeName,
    ) -> ViewerOutcome {
        if self.editing_search {
            match key.code {
                KeyCode::Esc => {
                    self.editing_search = false;
                    ViewerOutcome::Stay
                }
                KeyCode::Enter => {
                    self.editing_search = false;
                    ViewerOutcome::Stay
                }
                _ => {
                    let before = self.search.value().to_owned();
                    self.search.handle_event(&Event::Key(key));
                    if self.search.value() != before {
                        self.active_match = None;
                        self.render_cache = None;
                    }
                    ViewerOutcome::Stay
                }
            }
        } else {
            match key.code {
                KeyCode::Char('q') if key.modifiers.is_empty() => ViewerOutcome::Close,
                KeyCode::Esc => self.handle_escape(),
                KeyCode::Char('/') if key.modifiers.is_empty() => {
                    self.editing_search = true;
                    ViewerOutcome::Stay
                }
                KeyCode::Char('n') => {
                    self.jump_to_match(MatchDirection::Next, area, session, theme, theme_name);
                    ViewerOutcome::Stay
                }
                KeyCode::Char('p') => {
                    self.jump_to_match(MatchDirection::Previous, area, session, theme, theme_name);
                    ViewerOutcome::Stay
                }
                KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.jump_to_message(
                        MessageDirection::Previous,
                        area,
                        session,
                        theme,
                        theme_name,
                    );
                    ViewerOutcome::Stay
                }
                KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.jump_to_message(MessageDirection::Next, area, session, theme, theme_name);
                    ViewerOutcome::Stay
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.scroll = self.scroll.saturating_sub(1);
                    ViewerOutcome::Stay
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.scroll = self.scroll.saturating_add(1);
                    ViewerOutcome::Stay
                }
                KeyCode::PageUp => {
                    self.scroll = self.scroll.saturating_sub(VIEWER_PAGE_STEP);
                    ViewerOutcome::Stay
                }
                KeyCode::PageDown => {
                    self.scroll = self.scroll.saturating_add(VIEWER_PAGE_STEP);
                    ViewerOutcome::Stay
                }
                KeyCode::Home | KeyCode::Char('g') if key.modifiers.is_empty() => {
                    self.scroll = 0;
                    ViewerOutcome::Stay
                }
                KeyCode::End | KeyCode::Char('G')
                    if key.modifiers.contains(KeyModifiers::SHIFT) || key.code == KeyCode::End =>
                {
                    self.scroll = usize::MAX / 4;
                    ViewerOutcome::Stay
                }
                _ => ViewerOutcome::Stay,
            }
        }
    }

    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        session: &Session,
        theme: &Theme,
        theme_name: ThemeName,
    ) {
        let _profile = profile::scope("viewer.render");
        frame.render_widget(Clear, area);
        let chunks = split_viewer(area);
        let body_area = chunks[0];

        let (total_rows, mut text) = {
            let cache = self.render_cache(area, session, theme, theme_name);
            (cache.total_rows, cache.text.clone())
        };
        if self.active_match.is_some() {
            let viewport_width = body_area.width.saturating_sub(2);
            highlight_active_match(&mut text, self.scroll, viewport_width, theme);
        }
        let viewport_height = body_area.height.saturating_sub(2) as usize;
        let scroll = self.scroll.min(total_rows.saturating_sub(viewport_height));
        let scroll_percent = scroll_progress_percent(scroll, viewport_height, total_rows);
        let body = Paragraph::new(text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(theme.border_style(true))
                    .title(block_title(format!("Viewer · {scroll_percent}%"))),
            )
            .wrap(Wrap { trim: false })
            .scroll((scroll.min(u16::MAX as usize) as u16, 0));
        frame.render_widget(body, body_area);

        // Split footer into search bar (bordered) and keymap hints.
        let footer_chunks = Layout::vertical([
            Constraint::Length(VIEWER_SEARCH_HEIGHT),
            Constraint::Length(VIEWER_HINTS_HEIGHT),
        ])
        .split(chunks[1]);

        let search_bar = Paragraph::new(Line::from(Span::styled(
            self.search.value().to_owned(),
            Style::default().fg(theme.text),
        )))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(theme.border_style(self.editing_search))
                .title(block_title(Span::styled(
                    "Search",
                    Style::default().fg(theme.accent),
                )))
                .title(search_focus_title(self.editing_search, theme)),
        );
        frame.render_widget(search_bar, footer_chunks[0]);

        keymap_hint::render(frame, footer_chunks[1], &Self::HINTS, theme, "");

        if self.editing_search {
            let cursor_x = footer_chunks[0]
                .x
                .saturating_add(1 + self.search.visual_cursor() as u16)
                .min(footer_chunks[0].right().saturating_sub(1));
            frame.set_cursor_position((cursor_x, footer_chunks[0].y.saturating_add(1)));
        }
    }

    pub fn max_scroll(
        &mut self,
        area: Rect,
        session: &Session,
        theme: &Theme,
        theme_name: ThemeName,
    ) -> usize {
        let body_area = split_viewer(area)[0];
        let cache = self.render_cache(area, session, theme, theme_name);
        let viewport_height = body_area.height.saturating_sub(2) as usize;
        cache.total_rows.saturating_sub(viewport_height)
    }

    pub fn body_area(area: Rect) -> Rect {
        split_viewer(area)[0]
    }

    fn handle_escape(&mut self) -> ViewerOutcome {
        if self.search.value().is_empty() {
            return ViewerOutcome::Close;
        }

        self.search = Input::default();
        self.editing_search = false;
        self.active_match = None;
        self.render_cache = None;
        ViewerOutcome::Stay
    }

    fn jump_to_match(
        &mut self,
        direction: MatchDirection,
        area: Rect,
        session: Option<&Session>,
        theme: &Theme,
        theme_name: ThemeName,
    ) {
        let Some(session) = session else {
            self.active_match = None;
            return;
        };

        let query = self.search.value();
        let body_area = Self::body_area(area);
        let viewport_width = body_area.width.saturating_sub(2);
        let matches = collect_match_rows(session, theme, query, viewport_width);
        if matches.is_empty() {
            self.active_match = None;
            return;
        }

        let next_index = match direction {
            MatchDirection::Next => next_match_index(&matches, self.active_match, self.scroll),
            MatchDirection::Previous => {
                previous_match_index(&matches, self.active_match, self.scroll)
            }
        };

        self.active_match = Some(next_index);
        let max_scroll = self.max_scroll(area, session, theme, theme_name);
        let viewport_height = body_area.height.saturating_sub(2) as usize;
        self.scroll = scroll_for_match(matches[next_index], viewport_height, max_scroll);
    }

    fn jump_to_message(
        &mut self,
        direction: MessageDirection,
        area: Rect,
        session: Option<&Session>,
        theme: &Theme,
        theme_name: ThemeName,
    ) {
        let Some(session) = session else {
            return;
        };

        let body_area = Self::body_area(area);
        let viewport_width = body_area.width.saturating_sub(2);
        let rows = collect_message_rows(session, theme, viewport_width);
        let Some(target_row) = message_row_for_scroll(&rows, self.scroll, direction) else {
            return;
        };

        let max_scroll = self.max_scroll(area, session, theme, theme_name);
        self.scroll = target_row.min(max_scroll);
    }

    fn render_cache(
        &mut self,
        area: Rect,
        session: &Session,
        theme: &Theme,
        theme_name: ThemeName,
    ) -> &ViewerRenderCache {
        let body_area = Self::body_area(area);
        let width = body_area.width.saturating_sub(2);
        let path = session.file_path.clone();
        let query = self.search.value().to_owned();

        let cache_miss = self.render_cache.as_ref().is_none_or(|cache| {
            cache.path != path
                || cache.query != query
                || cache.width != width
                || cache.theme_name != theme_name
        });
        if cache_miss {
            profile::event("viewer.cache.miss");
            let highlight_query = (!query.is_empty()).then_some(query.as_str());
            let text = render_session_text(session, theme, highlight_query);
            let total_rows = wrapped_text_height(&text, width).max(1);
            self.render_cache = Some(ViewerRenderCache {
                path,
                query,
                width,
                theme_name,
                total_rows,
                text,
            });
        } else {
            profile::event("viewer.cache.hit");
        }

        self.render_cache
            .as_ref()
            .expect("viewer cache should exist")
    }
}

fn split_viewer(area: Rect) -> [Rect; 2] {
    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(VIEWER_FOOTER_HEIGHT)])
        .split(area);
    [chunks[0], chunks[1]]
}

fn search_focus_title(editing_search: bool, theme: &Theme) -> Line<'static> {
    if editing_search {
        return Line::from(vec![
            Span::styled(
                "Focused",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ),
            Span::styled("─", Style::default().fg(theme.border)),
        ])
        .right_aligned();
    }

    Line::from(vec![
        Span::styled("/", Style::default().fg(theme.accent)),
        Span::styled(" Focus", Style::default().fg(theme.muted)),
        Span::styled("─", Style::default().fg(theme.border)),
    ])
    .right_aligned()
}

fn scroll_progress_percent(scroll: usize, viewport_height: usize, total_rows: usize) -> usize {
    if total_rows == 0 {
        return 100;
    }

    let farthest_displayed_row = scroll.saturating_add(viewport_height).min(total_rows);
    farthest_displayed_row.saturating_mul(100) / total_rows
}

fn match_scrolloff(viewport_height: usize) -> usize {
    VIEWER_MATCH_SCROLLOFF.min(viewport_height / 3)
}

fn scroll_for_match(match_row: usize, viewport_height: usize, max_scroll: usize) -> usize {
    match_row
        .saturating_sub(match_scrolloff(viewport_height))
        .min(max_scroll)
}

/// Re-style search-match spans on the source line containing the active match
/// row so the "current" match stands out from the rest.
fn highlight_active_match(text: &mut Text<'_>, active_row: usize, width: u16, theme: &Theme) {
    if width == 0 {
        return;
    }
    let width = width as usize;
    let mut row_offset = 0usize;
    for line in text.lines.iter_mut() {
        let h = wrapped_rendered_line_height(line, width);
        if active_row >= row_offset && active_row < row_offset + h {
            // This source line contains the active match row — promote highlights.
            for span in &mut line.spans {
                if span.style.bg == Some(theme.search_match_bg) {
                    span.style.bg = Some(theme.active_match_bg);
                }
            }
            return;
        }
        row_offset += h;
    }
}

fn collect_match_rows(session: &Session, theme: &Theme, query: &str, width: u16) -> Vec<usize> {
    let terms = extract_highlight_terms(query);
    if terms.is_empty() || width == 0 {
        return Vec::new();
    }

    let width = width as usize;
    let mut rows = Vec::new();
    let mut row_offset = 0usize;

    for message in &session.messages {
        row_offset += 1;
        let rendered = render_message_body(
            session.agent,
            message.role,
            message.content.as_str(),
            theme,
            Some(query),
        );
        for line in &rendered.lines {
            let content_line = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            for relative_row in collect_line_match_rows(&content_line, width, &terms) {
                let absolute_row = row_offset + relative_row;
                if rows.last().copied() != Some(absolute_row) {
                    rows.push(absolute_row);
                }
            }
            row_offset += wrapped_rendered_line_height(line, width);
        }
        row_offset += 1;
    }

    rows
}

fn collect_message_rows(session: &Session, theme: &Theme, width: u16) -> Vec<usize> {
    if width == 0 {
        return Vec::new();
    }

    let width = width as usize;
    let mut rows = Vec::with_capacity(session.messages.len());
    let mut row_offset = 0usize;

    for message in &session.messages {
        rows.push(row_offset);
        row_offset += wrapped_line_height(&message_header_text(message), width);

        let rendered = render_message_body(
            session.agent,
            message.role,
            message.content.as_str(),
            theme,
            None,
        );
        for line in &rendered.lines {
            row_offset += wrapped_rendered_line_height(line, width);
        }
        row_offset += 1;
    }

    rows
}

fn message_header_text(message: &crate::parse::SessionMessage) -> String {
    let label = session_message_label(message);
    let timestamp = message
        .timestamp
        .map(|timestamp| timestamp.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default();
    if timestamp.is_empty() {
        label
    } else {
        format!("{label} {timestamp}")
    }
}

fn collect_line_match_rows(line: &str, width: usize, terms: &[String]) -> Vec<usize> {
    if width == 0 {
        return Vec::new();
    }

    let lower = line.to_ascii_lowercase();
    let mut rows = Vec::new();
    let mut index = 0usize;

    while index < line.len() {
        let mut matched_len = 0usize;
        for term in terms {
            if lower[index..].starts_with(term) {
                matched_len = matched_len.max(term.len());
            }
        }

        if matched_len > 0 {
            let row = UnicodeWidthStr::width(&line[..index]) / width;
            if rows.last().copied() != Some(row) {
                rows.push(row);
            }
            index += matched_len;
            continue;
        }

        index = line[index..]
            .grapheme_indices(true)
            .nth(1)
            .map(|(offset, _)| index + offset)
            .unwrap_or(line.len());
    }

    rows
}

fn wrapped_line_height(line: &str, width: usize) -> usize {
    let display_width = UnicodeWidthStr::width(line);
    if display_width == 0 {
        1
    } else {
        ((display_width - 1) / width) + 1
    }
}

fn wrapped_rendered_line_height(line: &Line<'_>, width: usize) -> usize {
    let content = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    wrapped_line_height(&content, width)
}

fn next_match_index(matches: &[usize], active_match: Option<usize>, scroll: usize) -> usize {
    if let Some(index) = active_match.filter(|index| *index < matches.len()) {
        return (index + 1) % matches.len();
    }

    matches.iter().position(|row| *row >= scroll).unwrap_or(0)
}

fn previous_match_index(matches: &[usize], active_match: Option<usize>, scroll: usize) -> usize {
    if let Some(index) = active_match.filter(|index| *index < matches.len()) {
        return if index == 0 {
            matches.len() - 1
        } else {
            index - 1
        };
    }

    matches
        .iter()
        .rposition(|row| *row < scroll)
        .unwrap_or(matches.len() - 1)
}

fn message_row_for_scroll(
    rows: &[usize],
    scroll: usize,
    direction: MessageDirection,
) -> Option<usize> {
    match direction {
        MessageDirection::Next => rows
            .iter()
            .copied()
            .find(|row| *row > scroll)
            .or_else(|| rows.first().copied()),
        MessageDirection::Previous => rows
            .iter()
            .copied()
            .rfind(|row| *row < scroll)
            .or_else(|| rows.last().copied()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::Utc;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Alignment;
    use ratatui::layout::Rect;
    use tui_input::Input;

    use crate::parse::{Agent, DerivationType, MessageRole, Session, SessionMessage};
    use crate::settings::ThemeName;
    use crate::tui::theme::Theme;

    use super::{
        collect_match_rows, collect_message_rows, match_scrolloff, message_row_for_scroll,
        next_match_index, previous_match_index, scroll_for_match, scroll_progress_percent,
        search_focus_title, MessageDirection, ViewerOutcome, ViewerState,
    };

    #[test]
    fn collect_match_rows_tracks_wrapped_content_lines() {
        let session = sample_session();
        let rows = collect_match_rows(&session, &Theme::default(), "alpha", 12);

        assert_eq!(rows, vec![1, 2, 6]);
    }

    #[test]
    fn collect_match_rows_follow_rendered_markdown_instead_of_raw_source() {
        let session = markdown_code_session();
        let rows = collect_match_rows(&session, &Theme::default(), "alpha", 80);

        assert_eq!(rows, vec![1]);
    }

    #[test]
    fn collect_message_rows_tracks_header_boundaries() {
        let session = sample_session();
        let rows = collect_message_rows(&session, &Theme::default(), 12);

        assert_eq!(rows, vec![0, 6]);
    }

    #[test]
    fn match_navigation_wraps_in_both_directions() {
        let matches = vec![1, 5, 9];

        assert_eq!(next_match_index(&matches, None, 0), 0);
        assert_eq!(next_match_index(&matches, Some(0), 0), 1);
        assert_eq!(next_match_index(&matches, Some(2), 0), 0);

        assert_eq!(previous_match_index(&matches, None, 5), 0);
        assert_eq!(previous_match_index(&matches, Some(0), 0), 2);
        assert_eq!(previous_match_index(&matches, Some(2), 0), 1);
    }

    #[test]
    fn message_navigation_wraps_in_both_directions() {
        let rows = vec![0, 4, 9];

        assert_eq!(
            message_row_for_scroll(&rows, 0, MessageDirection::Next),
            Some(4)
        );
        assert_eq!(
            message_row_for_scroll(&rows, 3, MessageDirection::Next),
            Some(4)
        );
        assert_eq!(
            message_row_for_scroll(&rows, 9, MessageDirection::Next),
            Some(0)
        );
        assert_eq!(
            message_row_for_scroll(&rows, 9, MessageDirection::Previous),
            Some(4)
        );
        assert_eq!(
            message_row_for_scroll(&rows, 3, MessageDirection::Previous),
            Some(0)
        );
        assert_eq!(
            message_row_for_scroll(&rows, 0, MessageDirection::Previous),
            Some(9)
        );
    }

    #[test]
    fn scroll_progress_tracks_farthest_visible_row() {
        assert_eq!(scroll_progress_percent(0, 9, 100), 9);
        assert_eq!(scroll_progress_percent(40, 9, 100), 49);
        assert_eq!(scroll_progress_percent(91, 9, 100), 100);
        assert_eq!(scroll_progress_percent(0, 20, 12), 100);
    }

    #[test]
    fn match_scrolloff_scales_for_short_viewports() {
        assert_eq!(match_scrolloff(2), 0);
        assert_eq!(match_scrolloff(6), 2);
        assert_eq!(match_scrolloff(12), 3);
    }

    #[test]
    fn scroll_for_match_applies_scrolloff_until_clamped() {
        assert_eq!(scroll_for_match(10, 12, 40), 7);
        assert_eq!(scroll_for_match(2, 12, 40), 0);
        assert_eq!(scroll_for_match(39, 12, 30), 30);
    }

    #[test]
    fn viewer_starts_without_active_match() {
        let state = ViewerState::new();

        assert!(state.active_match.is_none());
    }

    #[test]
    fn search_focus_title_is_right_aligned() {
        let idle = search_focus_title(false, &Theme::default());
        let focused = search_focus_title(true, &Theme::default());

        assert_eq!(idle.alignment, Some(Alignment::Right));
        assert_eq!(focused.alignment, Some(Alignment::Right));
        assert_eq!(
            idle.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "/ Focus─"
        );
        assert_eq!(
            focused
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "Focused─"
        );
    }

    #[test]
    fn viewer_hints_no_longer_include_search_hint() {
        assert!(ViewerState::HINTS.iter().all(|hint| hint.key != "/"));
    }

    #[test]
    fn escape_clears_search_before_closing_viewer() {
        let area = Rect::new(0, 0, 80, 20);
        let session = sample_session();
        let mut state = ViewerState::new();
        state.search = Input::default().with_value("alpha".to_owned());

        let outcome = state.handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            area,
            Some(&session),
            &Theme::default(),
            ThemeName::Lazygit,
        );
        assert_eq!(outcome, ViewerOutcome::Stay);
        assert!(state.search_query().is_empty());
        assert!(!state.is_editing_search());

        let outcome = state.handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            area,
            Some(&session),
            &Theme::default(),
            ThemeName::Lazygit,
        );
        assert_eq!(outcome, ViewerOutcome::Close);
    }

    #[test]
    fn escape_while_editing_clears_search_and_stops_editing() {
        let area = Rect::new(0, 0, 80, 20);
        let session = sample_session();
        let mut state = ViewerState::new();
        state.search = Input::default().with_value("alpha".to_owned());
        state.editing_search = true;

        let outcome = state.handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            area,
            Some(&session),
            &Theme::default(),
            ThemeName::Lazygit,
        );
        assert_eq!(outcome, ViewerOutcome::Stay);
        assert_eq!(state.search_query(), "alpha");
        assert!(!state.is_editing_search());
    }

    #[test]
    fn q_closes_viewer_when_search_is_unfocused() {
        let area = Rect::new(0, 0, 80, 20);
        let session = sample_session();
        let mut state = ViewerState::new();

        let outcome = state.handle_key(
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            area,
            Some(&session),
            &Theme::default(),
            ThemeName::Lazygit,
        );

        assert_eq!(outcome, ViewerOutcome::Close);
    }

    #[test]
    fn control_n_and_p_jump_between_matches() {
        let session = sample_session();
        let area = Rect::new(0, 0, 80, 20);
        let mut state = ViewerState::new();
        state.search = Input::default().with_value("alpha".to_owned());

        let next = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL);
        let previous = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);

        state.handle_key(
            next,
            area,
            Some(&session),
            &Theme::default(),
            ThemeName::Lazygit,
        );
        assert_eq!(state.active_match, Some(0));
        assert_eq!(state.scroll, 0);

        state.handle_key(
            next,
            area,
            Some(&session),
            &Theme::default(),
            ThemeName::Lazygit,
        );
        assert_eq!(state.active_match, Some(1));
        assert_eq!(state.scroll, 0);

        state.handle_key(
            previous,
            area,
            Some(&session),
            &Theme::default(),
            ThemeName::Lazygit,
        );
        assert_eq!(state.active_match, Some(0));
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn control_up_and_down_jump_between_message_boundaries() {
        let session = sample_session();
        let area = Rect::new(0, 0, 14, 10);
        let mut state = ViewerState::new();

        state.handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL),
            area,
            Some(&session),
            &Theme::default(),
            ThemeName::Lazygit,
        );
        assert_eq!(state.scroll, 6);

        state.handle_key(
            KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL),
            area,
            Some(&session),
            &Theme::default(),
            ThemeName::Lazygit,
        );
        assert_eq!(state.scroll, 0);
    }

    fn sample_session() -> Session {
        Session {
            session_id: "session-1".to_owned(),
            agent: Agent::Claude,
            project: "/tmp/demo".to_owned(),
            branch: Some("main".to_owned()),
            cwd: Some("/tmp/demo".to_owned()),
            created: Some(Utc::now()),
            modified: Some(Utc::now()),
            modified_ts: 0,
            lines: 6,
            file_path: PathBuf::from("/tmp/demo/session.jsonl"),
            first_msg_role: Some(MessageRole::User),
            first_msg_content: "alpha beta gamma delta".to_owned(),
            last_msg_role: Some(MessageRole::Assistant),
            last_msg_content: "omega alpha".to_owned(),
            first_user_msg_content: "alpha beta gamma delta".to_owned(),
            derivation_type: DerivationType::Original,
            is_sidechain: false,
            custom_title: Some("demo".to_owned()),
            messages: vec![
                SessionMessage {
                    role: MessageRole::User,
                    content: "alpha beta gamma delta alpha".to_owned(),
                    timestamp: Some(Utc::now()),
                    tool_name: None,
                },
                SessionMessage {
                    role: MessageRole::Assistant,
                    content: "omega alpha".to_owned(),
                    timestamp: Some(Utc::now()),
                    tool_name: None,
                },
            ],
            content: "alpha beta gamma delta alpha\nomega alpha".to_owned(),
        }
    }

    fn markdown_code_session() -> Session {
        Session {
            session_id: "session-md".to_owned(),
            agent: Agent::Claude,
            project: "/tmp/demo".to_owned(),
            branch: Some("main".to_owned()),
            cwd: Some("/tmp/demo".to_owned()),
            created: Some(Utc::now()),
            modified: Some(Utc::now()),
            modified_ts: 0,
            lines: 3,
            file_path: PathBuf::from("/tmp/demo/session-md.jsonl"),
            first_msg_role: Some(MessageRole::Assistant),
            first_msg_content: "```rust\nfn alpha() {}\n```".to_owned(),
            last_msg_role: Some(MessageRole::Assistant),
            last_msg_content: "```rust\nfn alpha() {}\n```".to_owned(),
            first_user_msg_content: String::new(),
            derivation_type: DerivationType::Original,
            is_sidechain: false,
            custom_title: Some("demo markdown".to_owned()),
            messages: vec![SessionMessage {
                role: MessageRole::Assistant,
                content: "```rust\nfn alpha() {}\n```".to_owned(),
                timestamp: Some(Utc::now()),
                tool_name: None,
            }],
            content: "```rust\nfn alpha() {}\n```".to_owned(),
        }
    }
}
