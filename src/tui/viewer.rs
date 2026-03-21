use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::parse::Session;
use crate::tui::layout;
use crate::tui::preview::render_session_text;
use crate::tui::theme::Theme;
use crate::tui::util::wrapped_text_height;

const VIEWER_PAGE_STEP: usize = 12;

#[derive(Debug, Clone)]
pub struct ViewerState {
    pub scroll: usize,
    search: Input,
    editing_search: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerOutcome {
    Stay,
    Close,
}

impl ViewerState {
    pub fn new() -> Self {
        Self {
            scroll: 0,
            search: Input::default(),
            editing_search: false,
        }
    }

    pub fn search_query(&self) -> &str {
        self.search.value()
    }

    pub fn is_editing_search(&self) -> bool {
        self.editing_search
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ViewerOutcome {
        if self.editing_search {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    self.editing_search = false;
                    ViewerOutcome::Stay
                }
                _ => {
                    self.search.handle_event(&Event::Key(key));
                    ViewerOutcome::Stay
                }
            }
        } else {
            match key.code {
                KeyCode::Esc => ViewerOutcome::Close,
                KeyCode::Char('/') if key.modifiers.is_empty() => {
                    self.editing_search = true;
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
        let popup = layout::centered_rect(area, 88, 88);
        frame.render_widget(Clear, popup);
        let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).split(popup);

        let title = session
            .custom_title
            .clone()
            .unwrap_or_else(|| session.project.clone());
        let text = render_session_text(
            session,
            theme,
            (!self.search.value().is_empty()).then_some(self.search.value()),
        );
        let viewport_height = chunks[0].height.saturating_sub(2) as usize;
        let viewport_width = chunks[0].width.saturating_sub(2);
        let scroll = self
            .scroll
            .min(wrapped_text_height(&text, viewport_width).saturating_sub(viewport_height));
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
            "Search (/ to edit, Esc to close)"
        };
        let footer = Paragraph::new(vec![
            Line::from(vec![
                Span::styled(search_label, Style::default().fg(theme.muted)),
                Span::styled(": ", Style::default().fg(theme.muted)),
                Span::styled(self.search.value(), Style::default().fg(theme.text)),
            ]),
            Line::from(Span::styled(
                "j/k scroll  PgUp/PgDn page  Home/End jump",
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
            let cursor_x = chunks[1]
                .x
                .saturating_add(11 + self.search.visual_cursor() as u16)
                .min(chunks[1].right().saturating_sub(1));
            frame.set_cursor_position((cursor_x, chunks[1].y.saturating_add(1)));
        }
    }
}
