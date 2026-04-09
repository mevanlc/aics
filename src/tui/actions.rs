use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState};
use ratatui::Frame;

use crate::ring_cursor::RingCursor;
use crate::tui::layout;
use crate::tui::theme::Theme;
use crate::tui::util::block_title;

const ACTIONS: [ActionItem; 8] = [
    ActionItem::new(SessionAction::View, 'v', "View full conversation"),
    ActionItem::new(SessionAction::Export, 'e', "Export as .txt"),
    ActionItem::new(SessionAction::CopyId, 'i', "Copy session id"),
    ActionItem::new(SessionAction::CopyPath, 'p', "Copy session path"),
    ActionItem::new(SessionAction::CopyDir, 'o', "Copy session directory"),
    ActionItem::new(SessionAction::Delete, 'd', "Delete session file"),
    ActionItem::new(SessionAction::Resume, 'r', "Resume in CLI"),
    ActionItem::new(SessionAction::Fork, 'f', "Fork in CLI"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAction {
    View,
    Export,
    CopyId,
    CopyPath,
    CopyDir,
    Delete,
    Resume,
    Fork,
}

#[derive(Debug, Clone)]
pub struct ActionMenuState {
    pub selected: RingCursor<SessionAction>,
}

#[derive(Debug, Clone)]
pub enum ActionOutcome {
    Stay,
    Close,
    Run(SessionAction),
}

impl ActionMenuState {
    pub fn new() -> Self {
        Self {
            selected: RingCursor::new(ACTIONS.iter().map(|item| item.action).collect()),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ActionOutcome {
        match key.code {
            KeyCode::Esc => ActionOutcome::Close,
            KeyCode::Enter => ActionOutcome::Run(*self.selected.current()),
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected.move_prev();
                ActionOutcome::Stay
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected.move_next();
                ActionOutcome::Stay
            }
            KeyCode::Char(ch) => ACTIONS
                .iter()
                .find(|item| item.key == ch)
                .map(|item| ActionOutcome::Run(item.action))
                .unwrap_or(ActionOutcome::Stay),
            _ => ActionOutcome::Stay,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let popup = layout::centered_rect(area, 42, 50);
        frame.render_widget(Clear, popup);
        let items = ACTIONS
            .iter()
            .map(|item| {
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{} ", item.key),
                        Style::default()
                            .fg(theme.highlight)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(item.label, Style::default().fg(theme.text)),
                ]))
            })
            .collect::<Vec<_>>();
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(theme.border_style(true))
                    .title(block_title("Actions")),
            )
            .highlight_symbol("› ")
            .highlight_style(
                Style::default()
                    .fg(theme.text)
                    .bg(theme.selection)
                    .add_modifier(Modifier::BOLD),
            );

        let mut state = ListState::default();
        state.select(Some(self.selected.index()));
        frame.render_stateful_widget(list, popup, &mut state);
    }
}

#[derive(Debug, Clone, Copy)]
struct ActionItem {
    action: SessionAction,
    key: char,
    label: &'static str,
}

impl ActionItem {
    const fn new(action: SessionAction, key: char, label: &'static str) -> Self {
        Self { action, key, label }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{ActionMenuState, ActionOutcome, SessionAction};

    #[test]
    fn up_wraps_to_last_action() {
        let mut state = ActionMenuState::new();

        let outcome = state.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));

        assert!(matches!(outcome, ActionOutcome::Stay));
        assert!(state.selected == SessionAction::Fork);
    }

    #[test]
    fn down_wraps_from_last_action_to_first() {
        let mut state = ActionMenuState::new();
        assert!(state.selected.set(&SessionAction::Fork));

        let outcome = state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

        assert!(matches!(outcome, ActionOutcome::Stay));
        assert!(state.selected == SessionAction::View);
    }
}
