use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::ring_cursor::RingCursor;
use crate::settings::DisplayOptions;
use crate::tui::theme::Theme;
use crate::tui::util::block_title;
use crate::tui::{keymap_hint, layout};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewField {
    ProjectDocsAutodump,
    ToolCalls,
    ToolResults,
    AgentReplies,
    UserMessages,
}

impl ViewField {
    fn label(self) -> &'static str {
        match self {
            Self::ProjectDocsAutodump => "Hide AGENTS.md/CLAUDE.md",
            Self::ToolCalls => "Hide Tool Calls",
            Self::ToolResults => "Hide Tool Results",
            Self::AgentReplies => "Hide Agent Replies",
            Self::UserMessages => "Hide User Messages",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ViewMenuState {
    selected: RingCursor<ViewField>,
    options: DisplayOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMenuOutcome {
    Stay,
    Close,
    Update(DisplayOptions),
}

impl ViewMenuState {
    pub fn new(options: DisplayOptions) -> Self {
        Self {
            selected: view_field_cursor(ViewField::ProjectDocsAutodump),
            options,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ViewMenuOutcome {
        match key.code {
            KeyCode::Esc => ViewMenuOutcome::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected.move_prev();
                ViewMenuOutcome::Stay
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected.move_next();
                ViewMenuOutcome::Stay
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.toggle_current();
                ViewMenuOutcome::Update(self.options)
            }
            _ => ViewMenuOutcome::Stay,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let popup = popup_area(area);
        frame.render_widget(Clear, popup);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border_style(true))
            .title(block_title("View"));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let chunks = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

        let fields = view_fields();
        let items = fields
            .iter()
            .map(|field| {
                let checked = if self.option_for(*field) { "x" } else { " " };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("[{checked}] "),
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(field.label(), Style::default().fg(theme.text)),
                ]))
            })
            .collect::<Vec<_>>();

        let list = List::new(items).highlight_symbol("> ").highlight_style(
            Style::default()
                .fg(theme.text)
                .bg(theme.selection)
                .add_modifier(Modifier::BOLD),
        );

        let mut state = ListState::default();
        state.select(Some(self.selected.index()));
        frame.render_stateful_widget(list, chunks[0], &mut state);

        frame.render_widget(
            Paragraph::new("-".repeat(inner.width as usize))
                .style(Style::default().fg(theme.focus_border)),
            chunks[1],
        );

        const HINTS: [keymap_hint::KeymapHint; 3] = [
            keymap_hint::KeymapHint::new("Up/Down", "select"),
            keymap_hint::KeymapHint::new("Enter", "toggle"),
            keymap_hint::KeymapHint::new("Esc", "back"),
        ];
        keymap_hint::render(frame, chunks[2], &HINTS, theme, "");
    }

    fn option_for(&self, field: ViewField) -> bool {
        match field {
            ViewField::ProjectDocsAutodump => self.options.hide_project_docs_autodump,
            ViewField::ToolCalls => self.options.hide_tool_calls,
            ViewField::ToolResults => self.options.hide_tool_results,
            ViewField::AgentReplies => self.options.hide_agent_replies,
            ViewField::UserMessages => self.options.hide_user_messages,
        }
    }

    fn toggle_current(&mut self) {
        match *self.selected.current() {
            ViewField::ProjectDocsAutodump => {
                self.options.hide_project_docs_autodump = !self.options.hide_project_docs_autodump;
            }
            ViewField::ToolCalls => {
                self.options.hide_tool_calls = !self.options.hide_tool_calls;
            }
            ViewField::ToolResults => {
                self.options.hide_tool_results = !self.options.hide_tool_results;
            }
            ViewField::AgentReplies => {
                self.options.hide_agent_replies = !self.options.hide_agent_replies;
            }
            ViewField::UserMessages => {
                self.options.hide_user_messages = !self.options.hide_user_messages;
            }
        }
    }
}

fn view_fields() -> [ViewField; 5] {
    [
        ViewField::ProjectDocsAutodump,
        ViewField::ToolCalls,
        ViewField::ToolResults,
        ViewField::AgentReplies,
        ViewField::UserMessages,
    ]
}

fn view_field_cursor(selected: ViewField) -> RingCursor<ViewField> {
    let mut cursor = RingCursor::new(view_fields().to_vec());
    assert!(cursor.set(&selected));
    cursor
}

fn popup_area(area: Rect) -> Rect {
    layout::centered_rect_fixed_width(area, 42, 36)
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{ViewField, ViewMenuOutcome, ViewMenuState};
    use crate::settings::DisplayOptions;

    #[test]
    fn project_docs_autodump_row_is_first_and_enabled_by_default() {
        let state = ViewMenuState::new(DisplayOptions::default());

        assert_eq!(*state.selected.current(), ViewField::ProjectDocsAutodump);
        assert!(state.option_for(ViewField::ProjectDocsAutodump));
        assert_eq!(
            ViewField::ProjectDocsAutodump.label(),
            "Hide AGENTS.md/CLAUDE.md"
        );
    }

    #[test]
    fn toggles_project_docs_autodump_option() {
        let mut state = ViewMenuState::new(DisplayOptions::default());

        let outcome = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(
            outcome,
            ViewMenuOutcome::Update(DisplayOptions {
                hide_project_docs_autodump: false,
                ..DisplayOptions::default()
            })
        );
    }
}
