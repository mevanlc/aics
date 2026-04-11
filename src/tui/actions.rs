use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::ring_cursor::RingCursor;
use crate::tui::keymap_hint;
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
        let popup = layout::centered_rect_fixed_width(area, 66, 50);
        frame.render_widget(Clear, popup);

        // Outer block provides the border; list is rendered inside separately.
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border_style(true))
            .title(block_title("Actions"));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        // Split inner area: list (fills available space) | separator | hint line | hotkey note
        let chunks = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

        // List (no block — border lives on the outer block above)
        let items = ACTIONS
            .iter()
            .map(|item| {
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{} ", item.key),
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(item.label, Style::default().fg(theme.text)),
                ]))
            })
            .collect::<Vec<_>>();

        let list = List::new(items)
            .highlight_symbol("› ")
            .highlight_style(
                Style::default()
                    .fg(theme.text)
                    .bg(theme.selection)
                    .add_modifier(Modifier::BOLD),
            );

        let mut state = ListState::default();
        state.select(Some(self.selected.index()));
        frame.render_stateful_widget(list, chunks[0], &mut state);

        // Separator line
        frame.render_widget(
            Paragraph::new("─".repeat(inner.width as usize))
                .style(Style::default().fg(theme.focus_border)),
            chunks[1],
        );

        // Hint line 1: navigation keys
        const HINTS: [keymap_hint::KeymapHint; 3] = [
            keymap_hint::KeymapHint::new("↑↓", "select"),
            keymap_hint::KeymapHint::new("⏎", "OK"),
            keymap_hint::KeymapHint::new("Esc", "cancel"),
        ];
        keymap_hint::render(frame, chunks[2], &HINTS, theme, "");

        // Hint line 2: hotkey shortcut note
        frame.render_widget(
            Paragraph::new(Span::styled(
                " or press the menu item's hotkey to execute it immediately",
                Style::default().fg(theme.muted),
            )),
            chunks[3],
        );
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
