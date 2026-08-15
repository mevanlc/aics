use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::parse::Agent;
use crate::ring_cursor::RingCursor;
use crate::tui::theme::Theme;
use crate::tui::util::block_title;
use crate::tui::{keymap_hint, layout};

const REGULAR_ACTIONS: [ActionItem; 13] = [
    ActionItem::new(SessionAction::Resume, 'r', "Resume in CLI"),
    ActionItem::new(SessionAction::ResumeInCwd, 'R', "Resume in CLI in CWD"),
    ActionItem::new(SessionAction::Fork, 'f', "Fork in CLI"),
    ActionItem::new(SessionAction::ForkInCwd, 'F', "Fork in CLI in CWD"),
    ActionItem::new(SessionAction::View, 'v', "View full conversation"),
    ActionItem::new(SessionAction::Summarize, 's', "Summarize session (AI)"),
    ActionItem::new(SessionAction::Export, 'e', "Export as .txt"),
    ActionItem::new(
        SessionAction::ExportFiltered,
        'E',
        "Export as filtered .txt",
    ),
    ActionItem::new(SessionAction::CopyId, 'i', "Copy session id"),
    ActionItem::new(SessionAction::CopyPath, 'p', "Copy session path"),
    ActionItem::new(SessionAction::CopyDir, 'o', "Copy session directory"),
    ActionItem::new(SessionAction::Delete, 'd', "Move session to Trash"),
    ActionItem::new(
        SessionAction::DeleteImmediately,
        'D',
        "Delete session immediately",
    ),
];

const TRASHED_ACTIONS: [ActionItem; 14] = [
    ActionItem::new(SessionAction::Resume, 'r', "Resume in CLI"),
    ActionItem::new(SessionAction::ResumeInCwd, 'R', "Resume in CLI in CWD"),
    ActionItem::new(SessionAction::Fork, 'f', "Fork in CLI"),
    ActionItem::new(SessionAction::ForkInCwd, 'F', "Fork in CLI in CWD"),
    ActionItem::new(SessionAction::View, 'v', "View full conversation"),
    ActionItem::new(SessionAction::Summarize, 's', "Summarize session (AI)"),
    ActionItem::new(SessionAction::Export, 'e', "Export as .txt"),
    ActionItem::new(
        SessionAction::ExportFiltered,
        'E',
        "Export as filtered .txt",
    ),
    ActionItem::new(SessionAction::CopyId, 'i', "Copy session id"),
    ActionItem::new(SessionAction::CopyPath, 'p', "Copy session path"),
    ActionItem::new(SessionAction::CopyDir, 'o', "Copy session directory"),
    ActionItem::new(SessionAction::UndoTrash, 'u', "Undo trash"),
    ActionItem::new(SessionAction::Delete, 'd', "Delete from Trash"),
    ActionItem::new(
        SessionAction::DeleteImmediately,
        'D',
        "Delete session immediately",
    ),
];

const ANTIGRAVITY_ACTIONS: [ActionItem; 8] = [
    ActionItem::new(SessionAction::Resume, 'r', "Resume in CLI"),
    ActionItem::new(SessionAction::View, 'v', "View full conversation"),
    ActionItem::new(SessionAction::Summarize, 's', "Summarize session (AI)"),
    ActionItem::new(SessionAction::Export, 'e', "Export as .txt"),
    ActionItem::new(
        SessionAction::ExportFiltered,
        'E',
        "Export as filtered .txt",
    ),
    ActionItem::new(SessionAction::CopyId, 'i', "Copy session id"),
    ActionItem::new(SessionAction::CopyPath, 'p', "Copy session path"),
    ActionItem::new(SessionAction::CopyDir, 'o', "Copy session directory"),
];

const ANTIGRAVITY_ACTIONS_WITHOUT_RESUME: [ActionItem; 7] = [
    ActionItem::new(SessionAction::View, 'v', "View full conversation"),
    ActionItem::new(SessionAction::Summarize, 's', "Summarize session (AI)"),
    ActionItem::new(SessionAction::Export, 'e', "Export as .txt"),
    ActionItem::new(
        SessionAction::ExportFiltered,
        'E',
        "Export as filtered .txt",
    ),
    ActionItem::new(SessionAction::CopyId, 'i', "Copy session id"),
    ActionItem::new(SessionAction::CopyPath, 'p', "Copy session path"),
    ActionItem::new(SessionAction::CopyDir, 'o', "Copy session directory"),
];

fn actions(agent: Agent, trashed: bool) -> &'static [ActionItem] {
    if agent == Agent::Antigravity {
        return &ANTIGRAVITY_ACTIONS;
    }
    if trashed {
        &TRASHED_ACTIONS
    } else {
        &REGULAR_ACTIONS
    }
}

fn actions_with_resume(
    agent: Agent,
    trashed: bool,
    antigravity_resume_supported: bool,
) -> &'static [ActionItem] {
    if agent == Agent::Antigravity && !antigravity_resume_supported {
        &ANTIGRAVITY_ACTIONS_WITHOUT_RESUME
    } else {
        actions(agent, trashed)
    }
}

pub fn action_for_key(ch: char, trashed: bool) -> Option<SessionAction> {
    actions(Agent::Claude, trashed)
        .iter()
        .find(|item| item.key == ch)
        .map(|item| item.action)
}

pub fn action_at(index: usize, trashed: bool) -> Option<SessionAction> {
    actions(Agent::Claude, trashed)
        .get(index)
        .map(|item| item.action)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAction {
    View,
    Summarize,
    Export,
    ExportFiltered,
    CopyId,
    CopyPath,
    CopyDir,
    UndoTrash,
    Delete,
    DeleteImmediately,
    Resume,
    ResumeInCwd,
    Fork,
    ForkInCwd,
}

#[derive(Debug, Clone)]
pub struct ActionMenuState {
    pub selected: RingCursor<SessionAction>,
    agent: Agent,
    trashed: bool,
    antigravity_resume_supported: bool,
}

#[derive(Debug, Clone)]
pub enum ActionOutcome {
    Stay,
    Close,
    Run(SessionAction),
}

impl Default for ActionMenuState {
    fn default() -> Self {
        Self::new(false)
    }
}

impl ActionMenuState {
    pub fn new(trashed: bool) -> Self {
        Self::new_for_agent(Agent::Claude, trashed)
    }

    pub fn new_for_agent(agent: Agent, trashed: bool) -> Self {
        Self::new_for_agent_with_resume(agent, trashed, true)
    }

    pub fn new_for_agent_with_resume(
        agent: Agent,
        trashed: bool,
        antigravity_resume_supported: bool,
    ) -> Self {
        Self {
            selected: RingCursor::new(
                actions_with_resume(agent, trashed, antigravity_resume_supported)
                    .iter()
                    .map(|item| item.action)
                    .collect(),
            ),
            agent,
            trashed,
            antigravity_resume_supported,
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
            KeyCode::Char(ch) => {
                actions_with_resume(self.agent, self.trashed, self.antigravity_resume_supported)
                    .iter()
                    .find(|item| item.key == ch)
                    .map(|item| item.action)
                    .map(ActionOutcome::Run)
                    .unwrap_or(ActionOutcome::Stay)
            }
            _ => ActionOutcome::Stay,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let popup = popup_area(area);
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
        let items =
            actions_with_resume(self.agent, self.trashed, self.antigravity_resume_supported)
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

    pub fn select_index(&mut self, index: usize) {
        if let Some(action) = self.action_at(index) {
            self.selected.set(&action);
        }
    }

    pub fn action_at(&self, index: usize) -> Option<SessionAction> {
        actions_with_resume(self.agent, self.trashed, self.antigravity_resume_supported)
            .get(index)
            .map(|item| item.action)
    }

    pub fn action_count(&self) -> usize {
        actions_with_resume(self.agent, self.trashed, self.antigravity_resume_supported).len()
    }

    pub fn trashed(&self) -> bool {
        self.trashed
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

pub fn popup_area(area: Rect) -> Rect {
    layout::centered_rect_fixed_width(area, 66, 50)
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

pub fn index_at_row(area: Rect, column: u16, row: u16, action_count: usize) -> Option<usize> {
    let list = list_area(area);
    if column < list.x || column >= list.right() || row < list.y || row >= list.bottom() {
        return None;
    }
    let index = (row - list.y) as usize;
    (index < action_count).then_some(index)
}

#[cfg(test)]
mod tests {
    use crate::parse::Agent;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Rect;

    use super::{
        actions, actions_with_resume, index_at_row, list_area, ActionMenuState, ActionOutcome,
        SessionAction,
    };

    #[test]
    fn regular_actions_follow_the_expected_order() {
        let items = actions(Agent::Claude, false)
            .iter()
            .map(|item| (item.key, item.label))
            .collect::<Vec<_>>();

        assert_eq!(
            items,
            vec![
                ('r', "Resume in CLI"),
                ('R', "Resume in CLI in CWD"),
                ('f', "Fork in CLI"),
                ('F', "Fork in CLI in CWD"),
                ('v', "View full conversation"),
                ('s', "Summarize session (AI)"),
                ('e', "Export as .txt"),
                ('E', "Export as filtered .txt"),
                ('i', "Copy session id"),
                ('p', "Copy session path"),
                ('o', "Copy session directory"),
                ('d', "Move session to Trash"),
                ('D', "Delete session immediately"),
            ]
        );
    }

    #[test]
    fn antigravity_actions_exclude_unsafe_bundle_mutations() {
        let actions = actions(Agent::Antigravity, false)
            .iter()
            .map(|item| item.action)
            .collect::<Vec<_>>();

        assert_eq!(
            actions,
            vec![
                SessionAction::Resume,
                SessionAction::View,
                SessionAction::Summarize,
                SessionAction::Export,
                SessionAction::ExportFiltered,
                SessionAction::CopyId,
                SessionAction::CopyPath,
                SessionAction::CopyDir,
            ]
        );
        assert!(!actions.contains(&SessionAction::ResumeInCwd));
        assert!(!actions.contains(&SessionAction::Fork));
        assert!(!actions.contains(&SessionAction::Delete));
    }

    #[test]
    fn antigravity_alternate_root_hides_resume() {
        assert!(!actions_with_resume(Agent::Antigravity, false, false)
            .iter()
            .any(|item| item.action == SessionAction::Resume));

        let mut state =
            ActionMenuState::new_for_agent_with_resume(Agent::Antigravity, false, false);
        assert!(matches!(
            state.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)),
            ActionOutcome::Stay
        ));
    }

    #[test]
    fn up_wraps_to_last_action() {
        let mut state = ActionMenuState::new(false);

        let outcome = state.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));

        assert!(matches!(outcome, ActionOutcome::Stay));
        assert!(state.selected == SessionAction::DeleteImmediately);
    }

    #[test]
    fn down_wraps_from_last_action_to_first() {
        let mut state = ActionMenuState::new(false);
        assert!(state.selected.set(&SessionAction::DeleteImmediately));

        let outcome = state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

        assert!(matches!(outcome, ActionOutcome::Stay));
        assert!(state.selected == SessionAction::Resume);
    }

    #[test]
    fn capital_d_runs_delete_immediately() {
        let mut state = ActionMenuState::new(false);

        let outcome = state.handle_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT));

        assert!(matches!(
            outcome,
            ActionOutcome::Run(SessionAction::DeleteImmediately)
        ));
    }

    #[test]
    fn capital_r_runs_resume_in_cwd() {
        let mut state = ActionMenuState::new(false);

        assert!(matches!(
            state.handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT)),
            ActionOutcome::Run(SessionAction::ResumeInCwd)
        ));
    }

    #[test]
    fn capital_e_runs_filtered_export() {
        let mut state = ActionMenuState::new(false);

        assert!(matches!(
            state.handle_key(KeyEvent::new(KeyCode::Char('E'), KeyModifiers::SHIFT)),
            ActionOutcome::Run(SessionAction::ExportFiltered)
        ));
    }

    #[test]
    fn capital_f_runs_fork_in_cwd() {
        let mut state = ActionMenuState::new(false);

        assert!(matches!(
            state.handle_key(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT)),
            ActionOutcome::Run(SessionAction::ForkInCwd)
        ));
    }

    #[test]
    fn undo_trash_only_appears_for_trashed_sessions() {
        let mut regular = ActionMenuState::new(false);
        let mut trashed = ActionMenuState::new(true);

        assert!(matches!(
            regular.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE)),
            ActionOutcome::Stay
        ));
        assert!(matches!(
            trashed.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE)),
            ActionOutcome::Run(SessionAction::UndoTrash)
        ));
    }

    #[test]
    fn index_at_row_rejects_columns_outside_list() {
        let area = Rect::new(0, 0, 120, 30);
        let list = list_area(area);

        assert_eq!(index_at_row(area, list.x, list.y, 3), Some(0));
        assert_eq!(
            index_at_row(area, list.x.saturating_sub(1), list.y, 3),
            None
        );
        assert_eq!(index_at_row(area, list.right(), list.y, 3), None);
    }
}
