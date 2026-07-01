use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::ring_cursor::RingCursor;
use crate::tui::theme::Theme;
use crate::tui::util::block_title;
use crate::tui::{keymap_hint, layout};

const RULES_ACTIONS: [RulesActionItem; 8] = [
    RulesActionItem::new(RulesAction::MarkSelected, 'm', "Mark selected"),
    RulesActionItem::new(RulesAction::UnmarkSelected, 'u', "Unmark selected"),
    RulesActionItem::new(RulesAction::MarkVisible, 'v', "Mark visible"),
    RulesActionItem::new(RulesAction::UnmarkVisible, 'V', "Unmark visible"),
    RulesActionItem::new(RulesAction::MarkAll, 'a', "Mark all"),
    RulesActionItem::new(RulesAction::UnmarkAll, 'A', "Unmark all"),
    RulesActionItem::new(
        RulesAction::ProcessMarked,
        'p',
        "Process marked sessions (no undo)",
    ),
    RulesActionItem::new(RulesAction::Quit, 'q', "Quit (cancels all actions)"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulesAction {
    MarkSelected,
    UnmarkSelected,
    MarkVisible,
    UnmarkVisible,
    MarkAll,
    UnmarkAll,
    ProcessMarked,
    Quit,
}

#[derive(Debug, Clone)]
pub struct RulesActionMenuState {
    pub selected: RingCursor<RulesAction>,
}

#[derive(Debug, Clone)]
pub enum RulesActionOutcome {
    Stay,
    Close,
    Run(RulesAction),
}

impl Default for RulesActionMenuState {
    fn default() -> Self {
        Self::new()
    }
}

impl RulesActionMenuState {
    pub fn new() -> Self {
        Self {
            selected: RingCursor::new(RULES_ACTIONS.iter().map(|item| item.action).collect()),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> RulesActionOutcome {
        match key.code {
            KeyCode::Esc => RulesActionOutcome::Close,
            KeyCode::Enter => RulesActionOutcome::Run(*self.selected.current()),
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected.move_prev();
                RulesActionOutcome::Stay
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected.move_next();
                RulesActionOutcome::Stay
            }
            KeyCode::Char(ch) => action_for_key(ch)
                .map(RulesActionOutcome::Run)
                .unwrap_or(RulesActionOutcome::Stay),
            _ => RulesActionOutcome::Stay,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let popup = popup_area(area);
        frame.render_widget(Clear, popup);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border_style(true))
            .title(block_title("Rule Actions"));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let chunks = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

        let items = RULES_ACTIONS
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

        let list = List::new(items).highlight_symbol("› ").highlight_style(
            Style::default()
                .fg(theme.text)
                .bg(theme.selection)
                .add_modifier(Modifier::BOLD),
        );

        let mut state = ListState::default();
        state.select(Some(self.selected.index()));
        frame.render_stateful_widget(list, chunks[0], &mut state);

        frame.render_widget(
            Paragraph::new("─".repeat(inner.width as usize))
                .style(Style::default().fg(theme.focus_border)),
            chunks[1],
        );

        const HINTS: [keymap_hint::KeymapHint; 3] = [
            keymap_hint::KeymapHint::new("↑↓", "select"),
            keymap_hint::KeymapHint::new("⏎", "OK"),
            keymap_hint::KeymapHint::new("Esc", "cancel"),
        ];
        keymap_hint::render(frame, chunks[2], &HINTS, theme, "");

        frame.render_widget(
            Paragraph::new(Span::styled(
                " marked sessions are processed together after confirmation",
                Style::default().fg(theme.muted),
            )),
            chunks[3],
        );
    }

    pub fn select_index(&mut self, index: usize) {
        if let Some(action) = action_at(index) {
            self.selected.set(&action);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RulesActionItem {
    action: RulesAction,
    key: char,
    label: &'static str,
}

impl RulesActionItem {
    const fn new(action: RulesAction, key: char, label: &'static str) -> Self {
        Self { action, key, label }
    }
}

pub fn action_for_key(ch: char) -> Option<RulesAction> {
    RULES_ACTIONS
        .iter()
        .find(|item| item.key == ch)
        .map(|item| item.action)
}

pub fn action_at(index: usize) -> Option<RulesAction> {
    RULES_ACTIONS.get(index).map(|item| item.action)
}

pub fn popup_area(area: Rect) -> Rect {
    layout::centered_rect_fixed_width(area, 72, 50)
}

pub fn list_area(area: Rect) -> Rect {
    let popup = popup_area(area);
    let inner = Block::default().borders(Borders::ALL).inner(popup);
    let chunks = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);
    chunks[0]
}

pub fn index_at_row(area: Rect, row: u16) -> Option<usize> {
    let list = list_area(area);
    if row < list.y || row >= list.bottom() {
        return None;
    }
    let index = (row - list.y) as usize;
    (index < RULES_ACTIONS.len()).then_some(index)
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{RulesAction, RulesActionMenuState, RulesActionOutcome};

    #[test]
    fn down_wraps_from_last_action_to_first() {
        let mut state = RulesActionMenuState::new();
        assert!(state.selected.set(&RulesAction::Quit));

        let outcome = state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

        assert!(matches!(outcome, RulesActionOutcome::Stay));
        assert!(state.selected == RulesAction::MarkSelected);
    }

    #[test]
    fn process_hotkey_runs_process_action() {
        let mut state = RulesActionMenuState::new();

        let outcome = state.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));

        assert!(matches!(
            outcome,
            RulesActionOutcome::Run(RulesAction::ProcessMarked)
        ));
    }
}
