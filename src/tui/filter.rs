use anyhow::{anyhow, Result};
use chrono::{Local, TimeZone, Utc};
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::index::{Scope, SearchFilters, SortMode};
use crate::parse::Agent;
use crate::tui::layout;
use crate::tui::theme::Theme;

const FIELD_ORDER: [FilterField; 12] = [
    FilterField::Scope,
    FilterField::Agent,
    FilterField::Branch,
    FilterField::After,
    FilterField::Before,
    FilterField::MinLines,
    FilterField::Original,
    FilterField::Trimmed,
    FilterField::Continued,
    FilterField::SubAgents,
    FilterField::LiveOnly,
    FilterField::Sort,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterField {
    Scope,
    Agent,
    Branch,
    After,
    Before,
    MinLines,
    Original,
    Trimmed,
    Continued,
    SubAgents,
    LiveOnly,
    Sort,
}

#[derive(Debug, Clone)]
pub struct FilterModalState {
    pub selected: FilterField,
    scope_global: bool,
    agent: Option<Agent>,
    branch: Input,
    after: Input,
    before: Input,
    min_lines: Input,
    include_original: bool,
    include_trimmed: bool,
    include_continued: bool,
    include_sub_agents: bool,
    live_only: bool,
    sort: SortMode,
}

#[derive(Debug, Clone)]
pub struct FilterUpdate {
    pub scope: Scope,
    pub filters: SearchFilters,
    pub sort: SortMode,
}

#[derive(Debug, Clone)]
pub enum FilterOutcome {
    Stay,
    Apply(FilterUpdate),
    Close,
}

impl FilterModalState {
    pub fn new(scope: &Scope, filters: &SearchFilters, sort: SortMode) -> Self {
        Self {
            selected: FilterField::Scope,
            scope_global: matches!(scope, Scope::Global),
            agent: filters.agent,
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
            sort,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, local_scope: &Scope) -> Result<FilterOutcome> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('f') {
            return Ok(FilterOutcome::Close);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
            *self = Self::new(local_scope, &SearchFilters::default(), SortMode::Time);
            return Ok(FilterOutcome::Stay);
        }

        match key.code {
            KeyCode::Esc => Ok(FilterOutcome::Close),
            KeyCode::Enter => Ok(FilterOutcome::Apply(self.build_update(local_scope)?)),
            KeyCode::Tab | KeyCode::Down | KeyCode::Char('j')
                if !self.selected.is_text() || key.modifiers.is_empty() =>
            {
                self.selected = self.selected.next();
                Ok(FilterOutcome::Stay)
            }
            KeyCode::BackTab | KeyCode::Up | KeyCode::Char('k')
                if !self.selected.is_text() || key.modifiers.is_empty() =>
            {
                self.selected = self.selected.previous();
                Ok(FilterOutcome::Stay)
            }
            KeyCode::Left => {
                self.adjust_current(false);
                Ok(FilterOutcome::Stay)
            }
            KeyCode::Right | KeyCode::Char(' ') => {
                self.adjust_current(true);
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

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme, scope_label: &str) {
        let popup = layout::centered_rect(area, 68, 72);
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border_style(true))
            .title("Filters");
        frame.render_widget(block, popup);

        let inner = popup.inner(ratatui::layout::Margin {
            horizontal: 1,
            vertical: 1,
        });
        let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).split(inner);

        let mut rows = Vec::new();
        for field in FIELD_ORDER {
            let selected = field == self.selected;
            let prefix = if selected { "› " } else { "  " };
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
                Span::styled(format!("{:<12}", field.label()), style),
                Span::styled(field.value(self, scope_label), style),
            ]));
        }

        let instructions = Paragraph::new(vec![
            Line::from(Span::styled(
                "Enter apply  Esc close  Ctrl+R reset  Left/Right toggle  Up/Down move",
                Style::default().fg(theme.muted),
            )),
            Line::from(Span::styled(
                "Dates use YYYY-MM-DD or RFC3339. Scope toggles between Global and the launch directory.",
                Style::default().fg(theme.muted),
            )),
        ]);

        frame.render_widget(Paragraph::new(rows), chunks[0]);
        frame.render_widget(instructions, chunks[1]);

        if let Some((cursor_x, cursor_y)) = self.cursor_position(chunks[0]) {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }

    fn build_update(&self, local_scope: &Scope) -> Result<FilterUpdate> {
        let scope = if self.scope_global {
            Scope::Global
        } else {
            local_scope.clone()
        };
        Ok(FilterUpdate {
            scope,
            sort: self.sort,
            filters: SearchFilters {
                agent: self.agent,
                branch: optional_string(self.branch.value()),
                after_ts: parse_optional_date(self.after.value(), false)?,
                before_ts: parse_optional_date(self.before.value(), true)?,
                min_lines: parse_optional_usize(self.min_lines.value())?,
                include_original: self.include_original,
                include_trimmed: self.include_trimmed,
                include_continued: self.include_continued,
                include_sub_agents: self.include_sub_agents,
                live_only: self.live_only,
            },
        })
    }

    fn current_input_mut(&mut self) -> Option<&mut Input> {
        match self.selected {
            FilterField::Branch => Some(&mut self.branch),
            FilterField::After => Some(&mut self.after),
            FilterField::Before => Some(&mut self.before),
            FilterField::MinLines => Some(&mut self.min_lines),
            _ => None,
        }
    }

    fn adjust_current(&mut self, forward: bool) {
        match self.selected {
            FilterField::Scope => self.scope_global = !self.scope_global,
            FilterField::Agent => {
                self.agent = match (self.agent, forward) {
                    (None, true) => Some(Agent::Claude),
                    (Some(Agent::Claude), true) => Some(Agent::Codex),
                    (Some(Agent::Codex), true) => None,
                    (None, false) => Some(Agent::Codex),
                    (Some(Agent::Codex), false) => Some(Agent::Claude),
                    (Some(Agent::Claude), false) => None,
                };
            }
            FilterField::Original => self.include_original = !self.include_original,
            FilterField::Trimmed => self.include_trimmed = !self.include_trimmed,
            FilterField::Continued => self.include_continued = !self.include_continued,
            FilterField::SubAgents => self.include_sub_agents = !self.include_sub_agents,
            FilterField::LiveOnly => self.live_only = !self.live_only,
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
        let row_index = FIELD_ORDER
            .iter()
            .position(|field| *field == self.selected)? as u16;
        let input = match self.selected {
            FilterField::Branch => &self.branch,
            FilterField::After => &self.after,
            FilterField::Before => &self.before,
            FilterField::MinLines => &self.min_lines,
            _ => return None,
        };

        Some((
            rows_area
                .x
                .saturating_add(17 + input.visual_cursor() as u16),
            rows_area.y.saturating_add(row_index),
        ))
    }
}

impl FilterField {
    fn next(self) -> Self {
        let index = FIELD_ORDER
            .iter()
            .position(|field| *field == self)
            .expect("field should exist");
        FIELD_ORDER[(index + 1) % FIELD_ORDER.len()]
    }

    fn previous(self) -> Self {
        let index = FIELD_ORDER
            .iter()
            .position(|field| *field == self)
            .expect("field should exist");
        FIELD_ORDER[(index + FIELD_ORDER.len() - 1) % FIELD_ORDER.len()]
    }

    fn is_text(self) -> bool {
        matches!(
            self,
            FilterField::Branch | FilterField::After | FilterField::Before | FilterField::MinLines
        )
    }

    fn label(self) -> &'static str {
        match self {
            FilterField::Scope => "Scope",
            FilterField::Agent => "Agent",
            FilterField::Branch => "Branch",
            FilterField::After => "After",
            FilterField::Before => "Before",
            FilterField::MinLines => "Min lines",
            FilterField::Original => "Original",
            FilterField::Trimmed => "Trimmed",
            FilterField::Continued => "Rollover",
            FilterField::SubAgents => "Sub-agents",
            FilterField::LiveOnly => "Live only",
            FilterField::Sort => "Sort",
        }
    }

    fn value(self, state: &FilterModalState, scope_label: &str) -> String {
        match self {
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
            FilterField::Branch => state.branch.value().to_owned(),
            FilterField::After => state.after.value().to_owned(),
            FilterField::Before => state.before.value().to_owned(),
            FilterField::MinLines => state.min_lines.value().to_owned(),
            FilterField::Original => on_off(state.include_original),
            FilterField::Trimmed => on_off(state.include_trimmed),
            FilterField::Continued => on_off(state.include_continued),
            FilterField::SubAgents => on_off(state.include_sub_agents),
            FilterField::LiveOnly => on_off(state.live_only),
            FilterField::Sort => match state.sort {
                SortMode::Relevance => "relevance".to_owned(),
                SortMode::Time => "time".to_owned(),
            },
        }
    }
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

    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::FilterModalState;
    use crate::index::{Scope, SearchFilters, SortMode};

    #[test]
    fn ctrl_r_resets_sort_to_time() {
        let scope = Scope::CurrentDir(PathBuf::from("/tmp/demo"));
        let mut state = FilterModalState::new(&scope, &SearchFilters::default(), SortMode::Relevance);

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
}
