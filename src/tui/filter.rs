use anyhow::{anyhow, Result};
use chrono::{Local, TimeZone, Utc};
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::index::{Scope, SearchFilters, SortMode, SupersededFilter, TrashFilter};
use crate::parse::Agent;
use crate::ring_cursor::RingCursor;
use crate::search_query::{extract_visibility_search, VisibilitySearch};
use crate::settings::DisplayOptions;
use crate::tui::keymap_hint::{self, KeymapHint};
use crate::tui::layout;
use crate::tui::theme::Theme;
use crate::tui::util::block_title;

const FIELD_ORDER: [FilterField; 16] = [
    FilterField::Scope,
    FilterField::SearchContent,
    FilterField::Agent,
    FilterField::Session,
    FilterField::Branch,
    FilterField::After,
    FilterField::Before,
    FilterField::MinLines,
    FilterField::Original,
    FilterField::Trimmed,
    FilterField::Continued,
    FilterField::SubAgents,
    FilterField::LiveOnly,
    FilterField::Superseded,
    FilterField::Trashed,
    FilterField::Sort,
];

const DISPLAY_ORDER: [DisplayField; 7] = [
    DisplayField::ProjectDocsAutodump,
    DisplayField::SkillTextInjection,
    DisplayField::ToolCalls,
    DisplayField::ToolResults,
    DisplayField::AgentReplies,
    DisplayField::UserMessages,
    DisplayField::InternalContext,
];

const FILTER_LABEL_WIDTH: usize = 15;
const FILTER_COLUMN_WIDTH: u16 = 31;
const DISPLAY_LABEL_WIDTH: usize = "AGENTS.md/CLAUDE.md".len() + 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterField {
    Scope,
    SearchContent,
    Agent,
    Session,
    Branch,
    After,
    Before,
    MinLines,
    Original,
    Trimmed,
    Continued,
    SubAgents,
    LiveOnly,
    Superseded,
    Trashed,
    Sort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisplayField {
    ProjectDocsAutodump,
    SkillTextInjection,
    ToolCalls,
    ToolResults,
    AgentReplies,
    UserMessages,
    InternalContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterSide {
    Filters,
    Display,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MnemonicTarget {
    Filter(FilterField),
    Display(DisplayField),
}

#[derive(Debug, Clone)]
pub struct FilterModalState {
    pub selected: RingCursor<FilterField>,
    display_selected: RingCursor<DisplayField>,
    selected_side: FilterSide,
    scope_global: bool,
    visibility_search: VisibilitySearch,
    query_override: Option<VisibilitySearch>,
    indexed_search: bool,
    agent: Option<Agent>,
    session_id: Input,
    branch: Input,
    after: Input,
    before: Input,
    min_lines: Input,
    include_original: bool,
    include_trimmed: bool,
    include_continued: bool,
    include_sub_agents: bool,
    live_only: bool,
    superseded: SupersededFilter,
    trashed: TrashFilter,
    sort: SortMode,
    display_options: DisplayOptions,
}

#[derive(Debug, Clone)]
pub struct FilterUpdate {
    pub scope: Scope,
    pub visibility_search: VisibilitySearch,
    pub filters: SearchFilters,
    pub sort: SortMode,
    pub display_options: DisplayOptions,
}

#[derive(Debug, Clone)]
pub enum FilterOutcome {
    Stay,
    Apply(FilterUpdate),
    SaveDefault(FilterUpdate),
    Close,
}

impl FilterModalState {
    pub fn new(
        scope: &Scope,
        filters: &SearchFilters,
        sort: SortMode,
        display_options: DisplayOptions,
        visibility_search: VisibilitySearch,
    ) -> Self {
        Self {
            selected: filter_field_cursor(FilterField::Scope),
            display_selected: display_field_cursor(DisplayField::ProjectDocsAutodump),
            selected_side: FilterSide::Filters,
            scope_global: matches!(scope, Scope::Global),
            visibility_search,
            query_override: None,
            indexed_search: true,
            agent: filters.agent,
            session_id: Input::default().with_value(filters.session_id.clone().unwrap_or_default()),
            branch: Input::default().with_value(filters.branch.clone().unwrap_or_default()),
            after: Input::default().with_value(format_optional_date(filters.after_ts)),
            before: Input::default().with_value(format_optional_date(filters.before_ts)),
            min_lines: Input::default().with_value(
                filters
                    .min_lines
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            include_original: filters.include_original,
            include_trimmed: filters.include_trimmed,
            include_continued: filters.include_continued,
            include_sub_agents: filters.include_sub_agents,
            live_only: filters.live_only,
            superseded: filters.superseded,
            trashed: filters.trashed,
            sort,
            display_options,
        }
    }

    pub fn with_search_context(mut self, query: &str, indexed_search: bool) -> Self {
        self.query_override = extract_visibility_search(query)
            .ok()
            .and_then(|(_, modifier)| modifier);
        self.indexed_search = indexed_search;
        self
    }

    pub fn handle_key(&mut self, key: KeyEvent, local_scope: &Scope) -> Result<FilterOutcome> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('f') {
            return Ok(FilterOutcome::Close);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
            let mut reset = Self::new(
                local_scope,
                &SearchFilters::default(),
                SortMode::Time,
                DisplayOptions::default(),
                VisibilitySearch::default(),
            );
            reset.query_override = self.query_override;
            reset.indexed_search = self.indexed_search;
            if !self.indexed_search {
                reset.visibility_search = self.visibility_search;
            }
            *self = reset;
            return Ok(FilterOutcome::Stay);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            return Ok(FilterOutcome::SaveDefault(self.build_update(local_scope)?));
        }
        if let Some(target) = self.mnemonic_target(key) {
            self.handle_mnemonic(target);
            return Ok(FilterOutcome::Stay);
        }

        match key.code {
            KeyCode::Esc => Ok(FilterOutcome::Close),
            KeyCode::Enter => Ok(FilterOutcome::Apply(self.build_update(local_scope)?)),
            KeyCode::Tab | KeyCode::Down | KeyCode::Char('j')
                if self.can_use_navigation_key(key) =>
            {
                self.move_selection(true);
                Ok(FilterOutcome::Stay)
            }
            KeyCode::BackTab | KeyCode::Up | KeyCode::Char('k')
                if self.can_use_navigation_key(key) =>
            {
                self.move_selection(false);
                Ok(FilterOutcome::Stay)
            }
            KeyCode::Left => {
                self.selected_side = FilterSide::Filters;
                Ok(FilterOutcome::Stay)
            }
            KeyCode::Right => {
                self.selected_side = FilterSide::Display;
                Ok(FilterOutcome::Stay)
            }
            KeyCode::Char(' ') => {
                self.toggle_current();
                Ok(FilterOutcome::Stay)
            }
            _ => {
                if let Some(input) = self.current_input_mut() {
                    input.handle_event(&Event::Key(key));
                }
                Ok(FilterOutcome::Stay)
            }
        }
    }

    pub fn handle_mouse(
        &mut self,
        area: Rect,
        kind: MouseEventKind,
        column: u16,
        row: u16,
    ) -> FilterOutcome {
        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(field) = field_at_position(area, column, row) {
                    self.focus_or_adjust_filter(field);
                } else if let Some(field) = display_at_position(area, column, row) {
                    self.focus_or_toggle_display(field);
                }
            }
            MouseEventKind::ScrollDown => {
                if contains(field_rows_area(area), column, row) {
                    self.selected_side = FilterSide::Filters;
                    self.selected.move_next();
                } else if contains(display_rows_area(area), column, row) {
                    self.selected_side = FilterSide::Display;
                    self.display_selected.move_next();
                }
            }
            MouseEventKind::ScrollUp => {
                if contains(field_rows_area(area), column, row) {
                    self.selected_side = FilterSide::Filters;
                    self.selected.move_prev();
                } else if contains(display_rows_area(area), column, row) {
                    self.selected_side = FilterSide::Display;
                    self.display_selected.move_prev();
                }
            }
            _ => {}
        }
        FilterOutcome::Stay
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme, scope_label: &str) {
        let popup = popup_area(area);
        frame.render_widget(Clear, popup);

        let turn_visibility_title_gap = "─".repeat(
            (FILTER_COLUMN_WIDTH + 1).saturating_sub("Session Filters".len() as u16) as usize,
        );
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border_style(true))
            .title(block_title(format!(
                "Session Filters{turn_visibility_title_gap}Turn Visibility"
            )));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let chunks = filter_chunks(inner);
        let (left_rows, column_separator, right_rows) = split_filter_columns(chunks.top);

        let mut rows = Vec::new();
        for field in FIELD_ORDER {
            let selected = self.selected_side == FilterSide::Filters && self.selected == field;
            let prefix = if selected { "›" } else { " " };
            let style = if selected {
                Style::default()
                    .fg(theme.text)
                    .bg(theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text)
            };

            rows.push(Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(format!("[{}] ", field.mnemonic()), style),
                Span::styled(format!("{:<FILTER_LABEL_WIDTH$}", field.label()), style),
                Span::styled(field.value(self, scope_label), style),
            ]));
        }

        const FILTER_HINTS: [KeymapHint; 6] = [
            KeymapHint::new("←↑↓→", "nav"),
            KeymapHint::new("Space", "toggle"),
            KeymapHint::new("⏎", "apply"),
            KeymapHint::new("^S", "save"),
            KeymapHint::new("^R", "reset"),
            KeymapHint::new("Esc", "cancel"),
        ];

        frame.render_widget(Paragraph::new(rows), left_rows);

        let display_rows = DISPLAY_ORDER
            .iter()
            .map(|field| {
                let selected =
                    self.selected_side == FilterSide::Display && self.display_selected == *field;
                let prefix = if selected { "›" } else { " " };
                let style = if selected {
                    Style::default()
                        .fg(theme.text)
                        .bg(theme.selection)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text)
                };
                Line::from(vec![
                    Span::styled(prefix, style),
                    Span::styled(format!("[{}] ", field.mnemonic()), style),
                    Span::styled(format!("{:<DISPLAY_LABEL_WIDTH$}", field.label()), style),
                    Span::styled(field.value(self.display_options), style),
                ])
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(display_rows), right_rows);
        render_vertical_separator(frame, column_separator, theme);

        render_separator(frame, chunks.top_separator, theme);

        let description = Paragraph::new(Span::styled(
            self.selected_description(),
            Style::default().fg(theme.muted),
        ))
        .wrap(Wrap { trim: false });
        frame.render_widget(description, chunks.description);

        render_separator(frame, chunks.hint_separator, theme);
        keymap_hint::render(frame, chunks.hints, &FILTER_HINTS, theme, "");

        if let Some((cursor_x, cursor_y)) = self.cursor_position(left_rows) {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }

    fn selected_description(&self) -> String {
        if self.selected_side == FilterSide::Filters && self.selected == FilterField::SearchContent
        {
            if !self.indexed_search {
                return "Search content is unavailable in rules preview, which searches proposal metadata.".to_owned();
            }
            if let Some(mode) = self.query_override {
                return format!(
                    "Current query overrides this with {}:. Apply/save changes the preference; remove the modifier to use it.",
                    mode.label().to_ascii_lowercase()
                );
            }
        }
        match self.selected_side {
            FilterSide::Filters => self.selected.current().description(),
            FilterSide::Display => self.display_selected.current().description(),
        }
        .to_owned()
    }

    fn build_update(&self, local_scope: &Scope) -> Result<FilterUpdate> {
        let scope = if self.scope_global {
            Scope::Global
        } else {
            local_scope.clone()
        };
        Ok(FilterUpdate {
            scope,
            visibility_search: self.visibility_search,
            sort: self.sort,
            display_options: self.display_options,
            filters: SearchFilters {
                agent: self.agent,
                session_id: optional_string(self.session_id.value()),
                branch: optional_string(self.branch.value()),
                after_ts: parse_optional_date(self.after.value(), false)?,
                before_ts: parse_optional_date(self.before.value(), true)?,
                min_lines: parse_optional_usize(self.min_lines.value())?,
                include_original: self.include_original,
                include_trimmed: self.include_trimmed,
                include_continued: self.include_continued,
                include_sub_agents: self.include_sub_agents,
                live_only: self.live_only,
                superseded: self.superseded,
                trashed: self.trashed,
            },
        })
    }

    fn current_input_mut(&mut self) -> Option<&mut Input> {
        if self.selected_side != FilterSide::Filters {
            return None;
        }
        match *self.selected.current() {
            FilterField::Session => Some(&mut self.session_id),
            FilterField::Branch => Some(&mut self.branch),
            FilterField::After => Some(&mut self.after),
            FilterField::Before => Some(&mut self.before),
            FilterField::MinLines => Some(&mut self.min_lines),
            _ => None,
        }
    }

    fn can_use_navigation_key(&self, key: KeyEvent) -> bool {
        self.selected_side == FilterSide::Display
            || !self.selected.current().is_text()
            || key.modifiers.is_empty()
    }

    fn move_selection(&mut self, forward: bool) {
        match (self.selected_side, forward) {
            (FilterSide::Filters, true) => {
                self.selected.move_next();
            }
            (FilterSide::Filters, false) => {
                self.selected.move_prev();
            }
            (FilterSide::Display, true) => {
                self.display_selected.move_next();
            }
            (FilterSide::Display, false) => {
                self.display_selected.move_prev();
            }
        }
    }

    fn toggle_current(&mut self) {
        match self.selected_side {
            FilterSide::Filters => self.adjust_current(true),
            FilterSide::Display => self.toggle_display_current(),
        }
    }

    fn toggle_display_current(&mut self) {
        match *self.display_selected.current() {
            DisplayField::ProjectDocsAutodump => {
                self.display_options.hide_project_docs_autodump =
                    !self.display_options.hide_project_docs_autodump;
            }
            DisplayField::SkillTextInjection => {
                self.display_options.hide_skill_text_injection =
                    !self.display_options.hide_skill_text_injection;
            }
            DisplayField::ToolCalls => {
                self.display_options.hide_tool_calls = !self.display_options.hide_tool_calls;
            }
            DisplayField::ToolResults => {
                self.display_options.hide_tool_results = !self.display_options.hide_tool_results;
            }
            DisplayField::AgentReplies => {
                self.display_options.hide_agent_replies = !self.display_options.hide_agent_replies;
            }
            DisplayField::UserMessages => {
                self.display_options.hide_user_messages = !self.display_options.hide_user_messages;
            }
            DisplayField::InternalContext => {
                self.display_options.hide_internal_context =
                    !self.display_options.hide_internal_context;
            }
        }
    }

    fn mnemonic_target(&self, key: KeyEvent) -> Option<MnemonicTarget> {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return None;
        }

        let is_alt = key.modifiers.contains(KeyModifiers::ALT);
        let is_plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
        let is_editing_text =
            self.selected_side == FilterSide::Filters && self.selected.current().is_text();
        if !is_alt && (!is_plain || is_editing_text) {
            return None;
        }

        let KeyCode::Char(ch) = key.code else {
            return None;
        };
        FilterField::from_mnemonic(ch)
            .map(MnemonicTarget::Filter)
            .or_else(|| DisplayField::from_mnemonic(ch).map(MnemonicTarget::Display))
    }

    fn handle_mnemonic(&mut self, target: MnemonicTarget) {
        match target {
            MnemonicTarget::Filter(field) => self.focus_or_adjust_filter(field),
            MnemonicTarget::Display(field) => self.focus_or_toggle_display(field),
        }
    }

    fn focus_or_adjust_filter(&mut self, field: FilterField) {
        let was_selected = self.selected_side == FilterSide::Filters && self.selected == field;
        self.selected_side = FilterSide::Filters;
        self.selected.set(&field);
        if was_selected && !field.is_text() {
            self.adjust_current(true);
        }
    }

    fn focus_or_toggle_display(&mut self, field: DisplayField) {
        let was_selected =
            self.selected_side == FilterSide::Display && self.display_selected == field;
        self.selected_side = FilterSide::Display;
        self.display_selected.set(&field);
        if was_selected {
            self.toggle_display_current();
        }
    }

    fn adjust_current(&mut self, forward: bool) {
        match *self.selected.current() {
            FilterField::Scope => self.scope_global = !self.scope_global,
            FilterField::SearchContent if self.indexed_search => {
                self.visibility_search = match (self.visibility_search, forward) {
                    (VisibilitySearch::Visible, true) | (VisibilitySearch::Hidden, false) => {
                        VisibilitySearch::All
                    }
                    (VisibilitySearch::All, true) | (VisibilitySearch::Visible, false) => {
                        VisibilitySearch::Hidden
                    }
                    (VisibilitySearch::Hidden, true) | (VisibilitySearch::All, false) => {
                        VisibilitySearch::Visible
                    }
                };
            }
            FilterField::Agent => {
                self.agent = match (self.agent, forward) {
                    (None, true) => Some(Agent::Claude),
                    (Some(Agent::Claude), true) => Some(Agent::Codex),
                    (Some(Agent::Codex), true) => Some(Agent::Antigravity),
                    (Some(Agent::Antigravity), true) => None,
                    (None, false) => Some(Agent::Antigravity),
                    (Some(Agent::Antigravity), false) => Some(Agent::Codex),
                    (Some(Agent::Codex), false) => Some(Agent::Claude),
                    (Some(Agent::Claude), false) => None,
                };
            }
            FilterField::Original => self.include_original = !self.include_original,
            FilterField::Trimmed => self.include_trimmed = !self.include_trimmed,
            FilterField::Continued => self.include_continued = !self.include_continued,
            FilterField::SubAgents => self.include_sub_agents = !self.include_sub_agents,
            FilterField::LiveOnly => self.live_only = !self.live_only,
            FilterField::Superseded => {
                self.superseded = match (self.superseded, forward) {
                    (SupersededFilter::No, true) => SupersededFilter::Yes,
                    (SupersededFilter::Yes, true) => SupersededFilter::Both,
                    (SupersededFilter::Both, true) => SupersededFilter::No,
                    (SupersededFilter::No, false) => SupersededFilter::Both,
                    (SupersededFilter::Both, false) => SupersededFilter::Yes,
                    (SupersededFilter::Yes, false) => SupersededFilter::No,
                };
            }
            FilterField::Trashed => {
                self.trashed = match (self.trashed, forward) {
                    (TrashFilter::No, true) => TrashFilter::Yes,
                    (TrashFilter::Yes, true) => TrashFilter::Both,
                    (TrashFilter::Both, true) => TrashFilter::No,
                    (TrashFilter::No, false) => TrashFilter::Both,
                    (TrashFilter::Both, false) => TrashFilter::Yes,
                    (TrashFilter::Yes, false) => TrashFilter::No,
                };
            }
            FilterField::Sort => {
                self.sort = match self.sort {
                    SortMode::Relevance => SortMode::Time,
                    SortMode::Time => SortMode::Relevance,
                }
            }
            _ => {}
        }
    }

    fn cursor_position(&self, rows_area: Rect) -> Option<(u16, u16)> {
        if self.selected_side != FilterSide::Filters {
            return None;
        }
        let row_index = FIELD_ORDER
            .iter()
            .position(|field| self.selected == *field)? as u16;
        let input = match *self.selected.current() {
            FilterField::Session => &self.session_id,
            FilterField::Branch => &self.branch,
            FilterField::After => &self.after,
            FilterField::Before => &self.before,
            FilterField::MinLines => &self.min_lines,
            _ => return None,
        };

        // Prefix, mnemonic, and padded label precede the editable value.
        Some((
            rows_area
                .x
                .saturating_add(6 + FILTER_LABEL_WIDTH as u16 + input.visual_cursor() as u16),
            rows_area.y.saturating_add(row_index),
        ))
    }
}

fn render_separator(frame: &mut Frame, area: Rect, theme: &Theme) {
    let sep_width = area.width as usize;
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(sep_width),
            Style::default().fg(theme.border),
        ))),
        area,
    );
}

fn render_vertical_separator(frame: &mut Frame, area: Rect, theme: &Theme) {
    let rows = (0..area.height)
        .map(|_| Line::from(Span::styled("│", Style::default().fg(theme.border))))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(rows), area);
}

#[derive(Debug, Clone, Copy)]
struct FilterChunks {
    top: Rect,
    top_separator: Rect,
    description: Rect,
    hint_separator: Rect,
    hints: Rect,
}

fn filter_chunks(inner: Rect) -> FilterChunks {
    let chunks = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);

    FilterChunks {
        top: chunks[0],
        top_separator: chunks[1],
        description: chunks[2],
        hint_separator: chunks[3],
        hints: chunks[4],
    }
}

fn popup_area(area: Rect) -> Rect {
    let mut popup = layout::centered_rect(area, 92, 72);
    // Keep every filter row and the description/hints visible at 80x24.
    popup.height = popup
        .height
        .max(FIELD_ORDER.len() as u16 + 7)
        .min(area.height);
    popup.y = area.y + area.height.saturating_sub(popup.height) / 2;
    popup
}

fn filter_columns(area: Rect) -> (Rect, Rect) {
    let popup = popup_area(area);
    let inner = Block::default().borders(Borders::ALL).inner(popup);
    let chunks = filter_chunks(inner);
    let (left, _, right) = split_filter_columns(chunks.top);
    (left, right)
}

fn split_filter_columns(area: Rect) -> (Rect, Rect, Rect) {
    let columns = Layout::horizontal([
        Constraint::Length(FILTER_COLUMN_WIDTH),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(area);
    (columns[0], columns[1], columns[2])
}

fn field_rows_area(area: Rect) -> Rect {
    filter_columns(area).0
}

fn display_rows_area(area: Rect) -> Rect {
    filter_columns(area).1
}

fn field_at_position(area: Rect, column: u16, row: u16) -> Option<FilterField> {
    let rows = field_rows_area(area);
    if !contains(rows, column, row) {
        return None;
    }
    FIELD_ORDER.get((row - rows.y) as usize).copied()
}

fn display_at_position(area: Rect, column: u16, row: u16) -> Option<DisplayField> {
    let rows = display_rows_area(area);
    if !contains(rows, column, row) {
        return None;
    }
    DISPLAY_ORDER.get((row - rows.y) as usize).copied()
}

fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.right() && row >= area.y && row < area.bottom()
}

impl FilterField {
    fn is_text(self) -> bool {
        matches!(
            self,
            FilterField::Branch
                | FilterField::After
                | FilterField::Before
                | FilterField::MinLines
                | FilterField::Session
        )
    }

    fn label(self) -> &'static str {
        match self {
            FilterField::Scope => "Scope",
            FilterField::SearchContent => "Search content",
            FilterField::Agent => "Agent",
            FilterField::Session => "Session",
            FilterField::Branch => "Branch",
            FilterField::After => "After",
            FilterField::Before => "Before",
            FilterField::MinLines => "Min lines",
            FilterField::Original => "Original",
            FilterField::Trimmed => "Trimmed",
            FilterField::Continued => "Continued",
            FilterField::SubAgents => "Sub-agents",
            FilterField::LiveOnly => "Live only",
            FilterField::Superseded => "Superseded",
            FilterField::Trashed => "Trashed",
            FilterField::Sort => "Sort",
        }
    }

    fn mnemonic(self) -> char {
        match self {
            FilterField::Scope => 's',
            FilterField::SearchContent => 'v',
            FilterField::Agent => 'a',
            FilterField::Session => 'i',
            FilterField::Branch => 'b',
            FilterField::After => 'f',
            FilterField::Before => 'e',
            FilterField::MinLines => 'm',
            FilterField::Original => 'o',
            FilterField::Trimmed => 't',
            FilterField::Continued => 'c',
            FilterField::SubAgents => 'u',
            FilterField::LiveOnly => 'l',
            FilterField::Superseded => 'p',
            FilterField::Trashed => 'h',
            FilterField::Sort => 'r',
        }
    }

    fn from_mnemonic(ch: char) -> Option<Self> {
        let ch = ch.to_ascii_lowercase();
        FIELD_ORDER.into_iter().find(|field| field.mnemonic() == ch)
    }

    fn description(self) -> &'static str {
        match self {
            FilterField::Scope => "Limit results to sessions from the launch directory or search globally across all sessions.",
            FilterField::SearchContent => "Search shown (Visible), excluded (Hidden), or All content. visible:/all:/hidden: override this; field: queries ignore it.",
            FilterField::Agent => {
                "Filter by agent type: Claude, Codex, Antigravity, or all. Use Space to cycle."
            }
            FilterField::Session => "Filter to one exact session id. Leave empty to show all sessions.",
            FilterField::Branch => "Filter sessions by git branch name. Leave empty to show all branches.",
            FilterField::After => "Only show sessions modified after this date. Use YYYY-MM-DD or RFC3339 format.",
            FilterField::Before => "Only show sessions modified before this date. Use YYYY-MM-DD or RFC3339 format.",
            FilterField::MinLines => "Only show sessions with at least this many lines of conversation.",
            FilterField::Original => "Include original (non-derived) sessions in results.",
            FilterField::Trimmed => "Include trimmed sessions (sessions that were compacted by the agent).",
            FilterField::Continued => "Include continued sessions (also called rollover sessions), which are continuations from a previous session.",
            FilterField::SubAgents => "Include sub-agent sessions (child sessions spawned by the agent tool).",
            FilterField::LiveOnly => "Only show sessions that are currently live (have an active agent process).",
            FilterField::Superseded => "Choose whether to show sessions collapsed as equivalent fork duplicates or superseded fork sources.",
            FilterField::Trashed => "Choose whether to search normal sessions, trashed sessions, or both.",
            FilterField::Sort => "Sort results by relevance to the search query or by modification time.",
        }
    }

    fn value(self, state: &FilterModalState, scope_label: &str) -> String {
        match self {
            FilterField::SearchContent => {
                if state.indexed_search {
                    state.visibility_search.label().to_owned()
                } else {
                    "N/A".to_owned()
                }
            }
            FilterField::Scope => {
                if state.scope_global {
                    "Global".to_owned()
                } else {
                    scope_label.to_owned()
                }
            }
            FilterField::Agent => state
                .agent
                .map(|agent| agent.to_string())
                .unwrap_or_else(|| "all".to_owned()),
            FilterField::Session => state.session_id.value().to_owned(),
            FilterField::Branch => state.branch.value().to_owned(),
            FilterField::After => state.after.value().to_owned(),
            FilterField::Before => state.before.value().to_owned(),
            FilterField::MinLines => state.min_lines.value().to_owned(),
            FilterField::Original => on_off(state.include_original),
            FilterField::Trimmed => on_off(state.include_trimmed),
            FilterField::Continued => on_off(state.include_continued),
            FilterField::SubAgents => on_off(state.include_sub_agents),
            FilterField::LiveOnly => on_off(state.live_only),
            FilterField::Superseded => state.superseded.label().to_owned(),
            FilterField::Trashed => state.trashed.label().to_owned(),
            FilterField::Sort => match state.sort {
                SortMode::Relevance => "relevance".to_owned(),
                SortMode::Time => "time".to_owned(),
            },
        }
    }
}

impl DisplayField {
    fn mnemonic(self) -> char {
        match self {
            Self::ProjectDocsAutodump => '1',
            Self::SkillTextInjection => '2',
            Self::ToolCalls => '3',
            Self::ToolResults => '4',
            Self::AgentReplies => '5',
            Self::UserMessages => '6',
            Self::InternalContext => '7',
        }
    }

    fn from_mnemonic(ch: char) -> Option<Self> {
        DISPLAY_ORDER
            .into_iter()
            .find(|field| field.mnemonic() == ch)
    }

    fn label(self) -> &'static str {
        match self {
            Self::ProjectDocsAutodump => "AGENTS.md/CLAUDE.md",
            Self::SkillTextInjection => "Skill Text Injection",
            Self::ToolCalls => "Tool Calls",
            Self::ToolResults => "Tool Results",
            Self::AgentReplies => "Agent Replies",
            Self::UserMessages => "User Messages",
            Self::InternalContext => "Internal Context",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::ProjectDocsAutodump => {
                "Hide harness injections of AGENTS.md/CLAUDE.md into session context."
            }
            Self::SkillTextInjection => "Hide skill definition text injected into session context.",
            Self::ToolCalls => "Hide tool call request blocks from previews and viewers.",
            Self::ToolResults => "Hide tool result blocks from previews and viewers.",
            Self::AgentReplies => "Hide assistant reply messages from previews and viewers.",
            Self::UserMessages => "Hide user messages from previews and viewers.",
            Self::InternalContext => {
                "Hide generated codex_internal_context and goal_context messages."
            }
        }
    }

    fn value(self, options: DisplayOptions) -> String {
        let hidden = match self {
            DisplayField::ProjectDocsAutodump => options.hide_project_docs_autodump,
            DisplayField::SkillTextInjection => options.hide_skill_text_injection,
            DisplayField::ToolCalls => options.hide_tool_calls,
            DisplayField::ToolResults => options.hide_tool_results,
            DisplayField::AgentReplies => options.hide_agent_replies,
            DisplayField::UserMessages => options.hide_user_messages,
            DisplayField::InternalContext => options.hide_internal_context,
        };
        on_off(!hidden)
    }
}

fn filter_field_cursor(selected: FilterField) -> RingCursor<FilterField> {
    let mut cursor = RingCursor::new(FIELD_ORDER.to_vec());
    assert!(cursor.set(&selected));
    cursor
}

fn display_field_cursor(selected: DisplayField) -> RingCursor<DisplayField> {
    let mut cursor = RingCursor::new(DISPLAY_ORDER.to_vec());
    assert!(cursor.set(&selected));
    cursor
}

fn on_off(value: bool) -> String {
    if value {
        "on".to_owned()
    } else {
        "off".to_owned()
    }
}

fn optional_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn parse_optional_usize(value: &str) -> Result<Option<usize>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed
        .parse::<usize>()
        .map(Some)
        .map_err(|_| anyhow!("invalid number `{trimmed}`"))
}

fn parse_optional_date(value: &str, end_of_day: bool) -> Result<Option<u64>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    if let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return Ok(Some(timestamp.with_timezone(&Utc).timestamp().max(0) as u64));
    }

    let date = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
        .map_err(|_| anyhow!("invalid date `{trimmed}`"))?;
    let time = if end_of_day {
        date.and_hms_opt(23, 59, 59)
    } else {
        date.and_hms_opt(0, 0, 0)
    }
    .expect("valid date");
    let local = Local
        .from_local_datetime(&time)
        .single()
        .ok_or_else(|| anyhow!("ambiguous local date `{trimmed}`"))?;
    Ok(Some(local.with_timezone(&Utc).timestamp().max(0) as u64))
}

fn format_optional_date(value: Option<u64>) -> String {
    value
        .and_then(|timestamp| Local.timestamp_opt(timestamp as i64, 0).single())
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
    use ratatui::layout::Rect;
    use ratatui::Terminal;

    use super::{
        display_rows_area, field_rows_area, DisplayField, FilterField, FilterModalState, FilterSide,
    };
    use crate::index::{Scope, SearchFilters, SortMode, SupersededFilter};
    use crate::parse::Agent;
    use crate::settings::DisplayOptions;
    use crate::tui::theme::Theme;

    #[test]
    fn display_options_are_presented_as_visibility_settings() {
        let defaults = DisplayOptions::default();

        assert_eq!(
            DisplayField::ProjectDocsAutodump.label(),
            "AGENTS.md/CLAUDE.md"
        );
        assert_eq!(DisplayField::ProjectDocsAutodump.value(defaults), "off");
        assert_eq!(
            DisplayField::SkillTextInjection.label(),
            "Skill Text Injection"
        );
        assert_eq!(DisplayField::SkillTextInjection.value(defaults), "on");
        assert_eq!(DisplayField::ToolResults.label(), "Tool Results");
        assert_eq!(DisplayField::ToolResults.value(defaults), "on");
        assert_eq!(DisplayField::InternalContext.value(defaults), "off");
        for (field, mnemonic) in super::DISPLAY_ORDER.into_iter().zip('1'..='7') {
            assert_eq!(field.mnemonic(), mnemonic);
        }
    }

    #[test]
    fn filter_modal_renders_visibility_header_and_column_divider() {
        let area = Rect::new(0, 0, 80, 24);
        let scope = Scope::current_dir(PathBuf::from("/tmp/demo"));
        let state = FilterModalState::new(
            &scope,
            &SearchFilters::default(),
            SortMode::Time,
            DisplayOptions::default(),
            super::VisibilitySearch::Visible,
        );
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();

        terminal
            .draw(|frame| state.render(frame, area, &Theme::default(), "demo"))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let left = field_rows_area(area);
        let right = display_rows_area(area);
        let filters_header = (0.."Session Filters".len() as u16)
            .map(|offset| buffer[(left.x + 1 + offset, left.y - 1)].symbol())
            .collect::<String>();
        let visibility_header = (0.."Turn Visibility".len() as u16)
            .map(|offset| buffer[(right.x + 1 + offset, left.y - 1)].symbol())
            .collect::<String>();
        let left_mnemonic_x = left.x + 1;
        let right_mnemonic_x = right.x + 1;
        let first_value_x = right_mnemonic_x + 4 + super::DISPLAY_LABEL_WIDTH as u16;
        assert_eq!(filters_header, "Session Filters");
        assert_eq!(visibility_header, "Turn Visibility");
        assert_eq!(buffer[(left.x, left.y - 1)].symbol(), "─");
        assert_eq!(buffer[(right.x, left.y - 1)].symbol(), "─");
        assert_eq!(right.x, left.right() + 1);
        assert_eq!(buffer[(left.x, left.y)].symbol(), "›");
        assert_eq!(buffer[(left_mnemonic_x, left.y)].symbol(), "[");
        assert_eq!(buffer[(left_mnemonic_x + 1, left.y)].symbol(), "s");
        assert_eq!(buffer[(left_mnemonic_x + 2, left.y)].symbol(), "]");
        for (row_offset, mnemonic) in ('1'..='7').enumerate() {
            assert_eq!(buffer[(right.x, right.y + row_offset as u16)].symbol(), " ");
            assert_eq!(
                buffer[(right_mnemonic_x, right.y + row_offset as u16)].symbol(),
                "["
            );
            assert_eq!(
                buffer[(right_mnemonic_x + 1, right.y + row_offset as u16)].symbol(),
                mnemonic.to_string()
            );
            assert_eq!(
                buffer[(right_mnemonic_x + 2, right.y + row_offset as u16)].symbol(),
                "]"
            );
        }
        assert_eq!(buffer[(first_value_x - 2, right.y)].symbol(), " ");
        assert_eq!(buffer[(first_value_x - 1, right.y)].symbol(), " ");
        assert_eq!(buffer[(first_value_x, right.y)].symbol(), "o");
        for row in left.y..left.bottom() {
            assert_eq!(buffer[(left.right(), row)].symbol(), "│");
        }
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Search content Visible"));
        assert!(text.contains("^R reset"));
        assert!(text.contains("Esc cancel"));
    }

    #[test]
    fn search_content_cycles_with_keyboard_and_mouse_and_retains_override_on_reset() {
        use super::VisibilitySearch;

        let scope = Scope::Global;
        let mut state = FilterModalState::new(
            &scope,
            &SearchFilters::default(),
            SortMode::Time,
            DisplayOptions::default(),
            VisibilitySearch::Visible,
        )
        .with_search_context("all: needle", true);
        let key = |ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE);
        state.handle_key(key('v'), &scope).unwrap();
        assert_eq!(state.visibility_search, VisibilitySearch::Visible);
        assert!(state
            .selected_description()
            .contains("overrides this with all:"));
        state.handle_key(key('v'), &scope).unwrap();
        assert_eq!(state.visibility_search, VisibilitySearch::All);
        let rows = field_rows_area(Rect::new(0, 0, 80, 24));
        state.handle_mouse(
            Rect::new(0, 0, 80, 24),
            MouseEventKind::Down(MouseButton::Left),
            rows.x + 8,
            rows.y + 1,
        );
        assert_eq!(state.visibility_search, VisibilitySearch::Hidden);
        state.handle_key(key(' '), &scope).unwrap();
        assert_eq!(state.visibility_search, VisibilitySearch::Visible);
        state.handle_key(key(' '), &scope).unwrap();
        state
            .handle_key(
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
                &scope,
            )
            .unwrap();
        assert_eq!(state.visibility_search, VisibilitySearch::Visible);
        state.handle_key(key('v'), &scope).unwrap();
        assert!(state
            .selected_description()
            .contains("overrides this with all:"));
        assert!(rows.height >= super::FIELD_ORDER.len() as u16);
    }

    #[test]
    fn search_content_is_unavailable_in_rules_preview() {
        use super::VisibilitySearch;

        let scope = Scope::Global;
        let mut state = FilterModalState::new(
            &scope,
            &SearchFilters::default(),
            SortMode::Time,
            DisplayOptions::default(),
            VisibilitySearch::Hidden,
        )
        .with_search_context("", false);
        for ch in ['v', 'v', ' '] {
            state
                .handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE), &scope)
                .unwrap();
        }
        assert_eq!(state.visibility_search, VisibilitySearch::Hidden);
        assert_eq!(FilterField::SearchContent.value(&state, ""), "N/A");
        assert!(state
            .selected_description()
            .contains("unavailable in rules preview"));
        state
            .handle_key(
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
                &scope,
            )
            .unwrap();
        assert_eq!(state.visibility_search, VisibilitySearch::Hidden);
        assert!(!state.indexed_search);
    }

    #[test]
    fn ctrl_r_resets_sort_to_time() {
        let scope = Scope::current_dir(PathBuf::from("/tmp/demo"));
        let mut state = FilterModalState::new(
            &scope,
            &SearchFilters::default(),
            SortMode::Relevance,
            DisplayOptions::default(),
            super::VisibilitySearch::Visible,
        );

        let outcome = state
            .handle_key(
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
                &scope,
            )
            .unwrap();

        assert!(matches!(outcome, super::FilterOutcome::Stay));

        let update = state.build_update(&scope).unwrap();
        assert_eq!(update.sort, SortMode::Time);
    }

    #[test]
    fn ctrl_s_requests_save_default_filter() {
        let scope = Scope::current_dir(PathBuf::from("/tmp/demo"));
        let mut state = FilterModalState::new(
            &scope,
            &SearchFilters {
                branch: Some("main".to_owned()),
                ..SearchFilters::default()
            },
            SortMode::Relevance,
            DisplayOptions::default(),
            super::VisibilitySearch::Visible,
        );

        let outcome = state
            .handle_key(
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
                &scope,
            )
            .unwrap();

        match outcome {
            super::FilterOutcome::SaveDefault(update) => {
                assert_eq!(update.filters.branch.as_deref(), Some("main"));
                assert_eq!(update.sort, SortMode::Relevance);
            }
            other => panic!("expected save-default outcome, got {other:?}"),
        }
    }

    #[test]
    fn agent_filter_cycles_through_antigravity() {
        let scope = Scope::current_dir(PathBuf::from("/tmp/demo"));
        let mut state = FilterModalState::new(
            &scope,
            &SearchFilters::default(),
            SortMode::Time,
            DisplayOptions::default(),
            super::VisibilitySearch::Visible,
        );
        assert!(state.selected.set(&FilterField::Agent));

        let mut values = Vec::new();
        for _ in 0..4 {
            state.adjust_current(true);
            values.push(state.agent);
        }
        assert_eq!(
            values,
            vec![
                Some(Agent::Claude),
                Some(Agent::Codex),
                Some(Agent::Antigravity),
                None,
            ]
        );
    }

    #[test]
    fn preserves_session_filter_in_update() {
        let scope = Scope::current_dir(PathBuf::from("/tmp/demo"));
        let state = FilterModalState::new(
            &scope,
            &SearchFilters {
                session_id: Some("session-123".to_owned()),
                ..SearchFilters::default()
            },
            SortMode::Time,
            DisplayOptions::default(),
            super::VisibilitySearch::Visible,
        );

        let update = state.build_update(&scope).unwrap();

        assert_eq!(update.filters.session_id.as_deref(), Some("session-123"));
    }

    #[test]
    fn superseded_filter_cycles_from_no_to_yes_and_is_preserved() {
        let scope = Scope::current_dir(PathBuf::from("/tmp/demo"));
        let mut state = FilterModalState::new(
            &scope,
            &SearchFilters::default(),
            SortMode::Time,
            DisplayOptions::default(),
            super::VisibilitySearch::Visible,
        );
        assert!(state.selected.set(&FilterField::Superseded));

        state
            .handle_key(
                KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
                &scope,
            )
            .unwrap();

        let update = state.build_update(&scope).unwrap();
        assert_eq!(update.filters.superseded, SupersededFilter::Yes);
    }

    #[test]
    fn plain_mnemonic_focuses_then_cycles_non_text_field() {
        let scope = Scope::current_dir(PathBuf::from("/tmp/demo"));
        let mut state = FilterModalState::new(
            &scope,
            &SearchFilters::default(),
            SortMode::Time,
            DisplayOptions::default(),
            super::VisibilitySearch::Visible,
        );

        let outcome = state
            .handle_key(
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
                &scope,
            )
            .unwrap();

        assert!(matches!(outcome, super::FilterOutcome::Stay));
        assert_eq!(*state.selected.current(), FilterField::Sort);
        assert_eq!(state.sort, SortMode::Time);

        state
            .handle_key(
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
                &scope,
            )
            .unwrap();

        assert_eq!(*state.selected.current(), FilterField::Sort);
        assert_eq!(state.sort, SortMode::Relevance);
    }

    #[test]
    fn plain_mnemonic_types_into_text_field() {
        let scope = Scope::current_dir(PathBuf::from("/tmp/demo"));
        let mut state = FilterModalState::new(
            &scope,
            &SearchFilters::default(),
            SortMode::Time,
            DisplayOptions::default(),
            super::VisibilitySearch::Visible,
        );
        assert!(state.selected.set(&FilterField::Branch));

        state
            .handle_key(
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
                &scope,
            )
            .unwrap();

        assert_eq!(*state.selected.current(), FilterField::Branch);
        assert_eq!(state.branch.value(), "s");
    }

    #[test]
    fn alt_mnemonic_jumps_from_text_field() {
        let scope = Scope::current_dir(PathBuf::from("/tmp/demo"));
        let mut state = FilterModalState::new(
            &scope,
            &SearchFilters::default(),
            SortMode::Time,
            DisplayOptions::default(),
            super::VisibilitySearch::Visible,
        );
        assert!(state.selected.set(&FilterField::Branch));

        state
            .handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT), &scope)
            .unwrap();

        assert_eq!(*state.selected.current(), FilterField::Scope);
        assert_eq!(state.branch.value(), "");
    }

    #[test]
    fn display_mnemonic_focuses_then_toggles() {
        let scope = Scope::current_dir(PathBuf::from("/tmp/demo"));
        let mut state = FilterModalState::new(
            &scope,
            &SearchFilters::default(),
            SortMode::Time,
            DisplayOptions::default(),
            super::VisibilitySearch::Visible,
        );
        assert!(state.selected.set(&FilterField::Branch));
        state.selected_side = FilterSide::Display;

        state
            .handle_key(
                KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE),
                &scope,
            )
            .unwrap();

        assert_eq!(state.selected_side, FilterSide::Display);
        assert_eq!(
            *state.display_selected.current(),
            DisplayField::SkillTextInjection
        );
        assert!(!state.display_options.hide_skill_text_injection);

        state
            .handle_key(
                KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE),
                &scope,
            )
            .unwrap();

        assert_eq!(
            *state.display_selected.current(),
            DisplayField::SkillTextInjection
        );
        assert!(state.display_options.hide_skill_text_injection);
    }

    #[test]
    fn internal_context_toggle_applies_saves_and_resets() {
        let scope = Scope::Global;
        let mut state = FilterModalState::new(
            &scope,
            &SearchFilters::default(),
            SortMode::Time,
            DisplayOptions::default(),
            super::VisibilitySearch::Visible,
        );
        for _ in 0..2 {
            state
                .handle_key(
                    KeyEvent::new(KeyCode::Char('7'), KeyModifiers::NONE),
                    &scope,
                )
                .unwrap();
        }
        assert_eq!(
            *state.display_selected.current(),
            DisplayField::InternalContext
        );
        assert!(
            !state
                .build_update(&scope)
                .unwrap()
                .display_options
                .hide_internal_context
        );
        let super::FilterOutcome::SaveDefault(update) = state
            .handle_key(
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
                &scope,
            )
            .unwrap()
        else {
            panic!("expected save-default action");
        };
        assert!(!update.display_options.hide_internal_context);
        state
            .handle_key(
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
                &scope,
            )
            .unwrap();
        assert!(
            state
                .build_update(&scope)
                .unwrap()
                .display_options
                .hide_internal_context
        );
    }

    #[test]
    fn clicking_text_filter_row_focuses_that_field() {
        let scope = Scope::current_dir(PathBuf::from("/tmp/demo"));
        let mut state = FilterModalState::new(
            &scope,
            &SearchFilters::default(),
            SortMode::Time,
            DisplayOptions::default(),
            super::VisibilitySearch::Visible,
        );
        let area = Rect::new(0, 0, 120, 40);
        let rows = field_rows_area(area);

        state.handle_mouse(
            area,
            MouseEventKind::Down(MouseButton::Left),
            rows.x,
            rows.y + 4,
        );
        state
            .handle_key(
                KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
                &scope,
            )
            .unwrap();

        assert_eq!(*state.selected.current(), FilterField::Branch);
        assert_eq!(state.branch.value(), "z");
    }

    #[test]
    fn clicking_unfocused_non_text_filter_row_only_focuses() {
        let scope = Scope::current_dir(PathBuf::from("/tmp/demo"));
        let mut state = FilterModalState::new(
            &scope,
            &SearchFilters::default(),
            SortMode::Time,
            DisplayOptions::default(),
            super::VisibilitySearch::Visible,
        );
        let area = Rect::new(0, 0, 120, 40);
        let rows = field_rows_area(area);

        state.handle_mouse(
            area,
            MouseEventKind::Down(MouseButton::Left),
            rows.x,
            rows.y + 2,
        );

        assert_eq!(*state.selected.current(), FilterField::Agent);
        assert_eq!(state.agent, None);
    }

    #[test]
    fn clicking_focused_non_text_filter_row_cycles_value() {
        let scope = Scope::current_dir(PathBuf::from("/tmp/demo"));
        let mut state = FilterModalState::new(
            &scope,
            &SearchFilters::default(),
            SortMode::Time,
            DisplayOptions::default(),
            super::VisibilitySearch::Visible,
        );
        let area = Rect::new(0, 0, 120, 40);
        let rows = field_rows_area(area);
        assert!(state.selected.set(&FilterField::Agent));

        state.handle_mouse(
            area,
            MouseEventKind::Down(MouseButton::Left),
            rows.x,
            rows.y + 2,
        );

        assert_eq!(*state.selected.current(), FilterField::Agent);
        assert_eq!(state.agent, Some(Agent::Claude));
    }

    #[test]
    fn right_arrow_moves_to_display_options_and_space_toggles_selected_option() {
        let scope = Scope::current_dir(PathBuf::from("/tmp/demo"));
        let mut state = FilterModalState::new(
            &scope,
            &SearchFilters::default(),
            SortMode::Time,
            DisplayOptions::default(),
            super::VisibilitySearch::Visible,
        );

        state
            .handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &scope)
            .unwrap();
        state
            .handle_key(
                KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
                &scope,
            )
            .unwrap();

        assert_eq!(
            *state.display_selected.current(),
            DisplayField::ProjectDocsAutodump
        );
        assert!(!state.display_options.hide_project_docs_autodump);
        let update = state.build_update(&scope).unwrap();
        assert!(!update.display_options.hide_project_docs_autodump);
    }

    #[test]
    fn clicking_unfocused_display_option_only_focuses() {
        let scope = Scope::current_dir(PathBuf::from("/tmp/demo"));
        let mut state = FilterModalState::new(
            &scope,
            &SearchFilters::default(),
            SortMode::Time,
            DisplayOptions::default(),
            super::VisibilitySearch::Visible,
        );
        let area = Rect::new(0, 0, 120, 40);
        let rows = display_rows_area(area);

        state.handle_mouse(
            area,
            MouseEventKind::Down(MouseButton::Left),
            rows.x,
            rows.y + 1,
        );

        assert_eq!(
            *state.display_selected.current(),
            DisplayField::SkillTextInjection
        );
        assert!(!state.display_options.hide_skill_text_injection);
    }

    #[test]
    fn clicking_focused_display_option_toggles_without_applying() {
        let scope = Scope::current_dir(PathBuf::from("/tmp/demo"));
        let mut state = FilterModalState::new(
            &scope,
            &SearchFilters::default(),
            SortMode::Time,
            DisplayOptions::default(),
            super::VisibilitySearch::Visible,
        );
        let area = Rect::new(0, 0, 120, 40);
        let rows = display_rows_area(area);
        state.selected_side = FilterSide::Display;
        assert!(state
            .display_selected
            .set(&DisplayField::SkillTextInjection));

        state.handle_mouse(
            area,
            MouseEventKind::Down(MouseButton::Left),
            rows.x,
            rows.y + 1,
        );

        assert_eq!(
            *state.display_selected.current(),
            DisplayField::SkillTextInjection
        );
        assert!(state.display_options.hide_skill_text_injection);
        let update = state.build_update(&scope).unwrap();
        assert!(update.display_options.hide_skill_text_injection);
    }
}
