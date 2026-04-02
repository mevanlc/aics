use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::parse::Session;
use crate::search_query::extract_highlight_terms;
use crate::tui::preview::{render_message_body, render_session_text};
use crate::tui::theme::Theme;
use crate::tui::util::{session_display_title, wrapped_text_height};

const VIEWER_PAGE_STEP: usize = 12;
const VIEWER_FOOTER_HEIGHT: u16 = 4;

#[derive(Debug, Clone)]
pub struct ViewerState {
    pub scroll: usize,
    search: Input,
    editing_search: bool,
    active_match: Option<usize>,
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

impl ViewerState {
    pub fn new() -> Self {
        Self {
            scroll: 0,
            search: Input::default(),
            editing_search: false,
            active_match: None,
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
    ) -> ViewerOutcome {
        if self.editing_search {
            match key.code {
                KeyCode::Esc => self.handle_escape(),
                KeyCode::Enter => {
                    self.editing_search = false;
                    ViewerOutcome::Stay
                }
                _ => {
                    let before = self.search.value().to_owned();
                    self.search.handle_event(&Event::Key(key));
                    if self.search.value() != before {
                        self.active_match = None;
                    }
                    ViewerOutcome::Stay
                }
            }
        } else {
            match key.code {
                KeyCode::Esc => self.handle_escape(),
                KeyCode::Char('/') if key.modifiers.is_empty() => {
                    self.editing_search = true;
                    ViewerOutcome::Stay
                }
                KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.jump_to_match(MatchDirection::Next, area, session, theme);
                    ViewerOutcome::Stay
                }
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.jump_to_match(MatchDirection::Previous, area, session, theme);
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

    pub fn render(&self, frame: &mut Frame, area: Rect, session: &Session, theme: &Theme) {
        frame.render_widget(Clear, area);
        let chunks = split_viewer(area);

        let title = session_display_title(
            session.agent,
            &session.project,
            session.custom_title.as_deref(),
        );
        let text = render_session_text(
            session,
            theme,
            (!self.search.value().is_empty()).then_some(self.search.value()),
        );
        let scroll = self.scroll.min(self.max_scroll(area, session, theme));
        let body = Paragraph::new(text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(theme.border_style(true))
                    .title(format!("Viewer · {title}")),
            )
            .wrap(Wrap { trim: false })
            .scroll((scroll.min(u16::MAX as usize) as u16, 0));
        frame.render_widget(body, chunks[0]);

        let search_label = if self.editing_search {
            "Search (/)"
        } else {
            "Search (/ to edit)"
        };
        let footer = Paragraph::new(vec![
            Line::from(vec![
                Span::styled(search_label, Style::default().fg(theme.muted)),
                Span::styled(": ", Style::default().fg(theme.muted)),
                Span::styled(self.search.value(), Style::default().fg(theme.text)),
            ]),
            Line::from(Span::styled(
                "j/k scroll  Ctrl+N/P matches  PgUp/PgDn page  Home/End jump  Enter done  Esc close",
                Style::default().fg(theme.muted),
            )),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(theme.border_style(self.editing_search)),
        );
        frame.render_widget(footer, chunks[1]);

        if self.editing_search {
            // border(1) + "Search (/): "(12) = 13
            let cursor_x = chunks[1]
                .x
                .saturating_add(13 + self.search.visual_cursor() as u16)
                .min(chunks[1].right().saturating_sub(1));
            frame.set_cursor_position((cursor_x, chunks[1].y.saturating_add(1)));
        }
    }

    pub fn max_scroll(&self, area: Rect, session: &Session, theme: &Theme) -> usize {
        let chunks = split_viewer(area);
        let text = render_session_text(
            session,
            theme,
            (!self.search.value().is_empty()).then_some(self.search.value()),
        );
        let viewport_height = chunks[0].height.saturating_sub(2) as usize;
        let viewport_width = chunks[0].width.saturating_sub(2);
        wrapped_text_height(&text, viewport_width).saturating_sub(viewport_height)
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
        ViewerOutcome::Stay
    }

    fn jump_to_match(
        &mut self,
        direction: MatchDirection,
        area: Rect,
        session: Option<&Session>,
        theme: &Theme,
    ) {
        let Some(session) = session else {
            self.active_match = None;
            return;
        };

        let query = self.search.value();
        let viewport_width = Self::body_area(area).width.saturating_sub(2);
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
        self.scroll = matches[next_index];
    }
}

fn split_viewer(area: Rect) -> [Rect; 2] {
    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(VIEWER_FOOTER_HEIGHT)])
        .split(area);
    [chunks[0], chunks[1]]
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::Utc;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Rect;
    use tui_input::Input;

    use crate::parse::{Agent, DerivationType, MessageRole, Session, SessionMessage};
    use crate::tui::theme::Theme;

    use super::{
        collect_match_rows, next_match_index, previous_match_index, ViewerOutcome, ViewerState,
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
    fn viewer_starts_without_active_match() {
        let state = ViewerState::new();

        assert!(state.active_match.is_none());
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
        );
        assert_eq!(outcome, ViewerOutcome::Stay);
        assert!(state.search_query().is_empty());
        assert!(!state.is_editing_search());

        let outcome = state.handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            area,
            Some(&session),
            &Theme::default(),
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
        );
        assert_eq!(outcome, ViewerOutcome::Stay);
        assert!(state.search_query().is_empty());
        assert!(!state.is_editing_search());
    }

    #[test]
    fn control_n_and_p_jump_between_matches() {
        let session = sample_session();
        let area = Rect::new(0, 0, 80, 20);
        let mut state = ViewerState::new();
        state.search = Input::default().with_value("alpha".to_owned());

        let next = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL);
        let previous = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);

        state.handle_key(next, area, Some(&session), &Theme::default());
        assert_eq!(state.active_match, Some(0));
        assert_eq!(state.scroll, 1);

        state.handle_key(next, area, Some(&session), &Theme::default());
        assert_eq!(state.active_match, Some(1));
        assert_eq!(state.scroll, 4);

        state.handle_key(previous, area, Some(&session), &Theme::default());
        assert_eq!(state.active_match, Some(0));
        assert_eq!(state.scroll, 1);
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
                },
                SessionMessage {
                    role: MessageRole::Assistant,
                    content: "omega alpha".to_owned(),
                    timestamp: Some(Utc::now()),
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
            }],
            content: "```rust\nfn alpha() {}\n```".to_owned(),
        }
    }
}
