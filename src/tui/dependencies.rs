use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use super::theme::Theme;
use crate::lifecycle::{DependencyGraph, LifecyclePlan, Member, Operation};
use crate::parse::Agent;
use crate::scan::SessionRoots;
use crate::trash::TrashReason;

#[derive(Debug)]
pub struct Review {
    pub graph: DependencyGraph,
    pub plan: LifecyclePlan,
    pub reason: TrashReason,
    roots: SessionRoots,
    agent: Agent,
    related: Vec<Member>,
    selected: usize,
    query: String,
    searching: bool,
    pub confirming: bool,
    pub notice: String,
    detail_scroll: u16,
    list: ListState,
}

pub enum Outcome {
    Stay,
    Close,
    View(Member),
    Execute,
}

impl Review {
    pub fn new(
        roots: &SessionRoots,
        agent: Agent,
        path: PathBuf,
        operation: Operation,
        reason: TrashReason,
        confirming: bool,
    ) -> Result<Self> {
        let graph = DependencyGraph::discover(roots, agent, std::slice::from_ref(&path))?;
        let mut plan = graph.plan(BTreeSet::from([path]), operation);
        let related = graph.related(&plan.seeds);
        DependencyGraph::add_live_blockers(&mut plan, roots);
        Ok(Self {
            graph,
            plan,
            reason,
            roots: roots.clone(),
            agent,
            related,
            selected: 0,
            query: String::new(),
            searching: false,
            confirming,
            notice: String::new(),
            detail_scroll: 0,
            list: ListState::default(),
        })
    }

    fn visible(&self) -> Vec<&Member> {
        let query = self.query.to_lowercase();
        self.related
            .iter()
            .filter(|m| {
                query.is_empty()
                    || m.path.to_string_lossy().to_lowercase().contains(&query)
                    || m.thread_id.contains(&query)
            })
            .collect()
    }

    fn replan(&mut self) {
        self.plan = self
            .graph
            .plan(self.plan.seeds.clone(), self.plan.operation);
        DependencyGraph::add_live_blockers(&mut self.plan, &self.roots);
    }

    pub fn refresh(&mut self) -> Result<bool> {
        self.graph = DependencyGraph::discover(
            &self.roots,
            self.agent,
            &self.plan.seeds.iter().cloned().collect::<Vec<_>>(),
        )?;
        let previous = self.plan.clone();
        self.replan();
        self.related = self.graph.related(&self.plan.seeds);
        Ok(previous != self.plan)
    }

    pub fn key(&mut self, key: KeyEvent) -> Outcome {
        if self.searching {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.searching = false,
                KeyCode::Backspace => {
                    self.query.pop();
                    self.selected = 0;
                }
                KeyCode::Char(c) => {
                    self.query.push(c);
                    self.selected = 0;
                }
                _ => {}
            }
            return Outcome::Stay;
        }
        if key.code == KeyCode::Esc {
            return Outcome::Close;
        }
        match key.code {
            KeyCode::PageDown => {
                self.detail_scroll = self.detail_scroll.saturating_add(3);
                return Outcome::Stay;
            }
            KeyCode::PageUp => {
                self.detail_scroll = self.detail_scroll.saturating_sub(3);
                return Outcome::Stay;
            }
            _ => {}
        }
        if self.confirming {
            match key.code {
                KeyCode::Up => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down => {
                    self.selected =
                        (self.selected + 1).min(self.plan.members.len().saturating_sub(1))
                }
                KeyCode::Char('r') => {
                    self.confirming = false;
                    self.selected = 0;
                }
                KeyCode::Char('y')
                    if self.plan.blockers.is_empty() && !self.plan.members.is_empty() =>
                {
                    return Outcome::Execute
                }
                _ => {}
            }
            return Outcome::Stay;
        }
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.visible().len().saturating_sub(1))
            }
            KeyCode::Char('/') => self.searching = true,
            KeyCode::Char('v') | KeyCode::Enter => {
                if let Some(m) = self.visible().get(self.selected) {
                    return Outcome::View((*m).clone());
                }
            }
            KeyCode::Char(' ') => {
                if let Some(m) = self.visible().get(self.selected) {
                    let path = m.path.clone();
                    if self.plan.seeds.contains(&path) {
                        self.plan.seeds.remove(&path);
                    } else if self.plan.members.contains_key(&path) {
                        self.notice =
                            "Required by the selection; unselect the originating session first."
                                .into();
                        return Outcome::Stay;
                    } else {
                        self.plan.seeds.insert(path);
                    }
                    self.notice.clear();
                    self.replan();
                }
            }
            KeyCode::Char('t') => {
                self.plan.operation = Operation::Trash;
                self.replan();
            }
            KeyCode::Char('u') => {
                self.plan.operation = Operation::Restore;
                self.replan();
            }
            KeyCode::Char('D') => {
                self.plan.operation = Operation::Delete;
                self.replan();
            }
            KeyCode::Char('c') => {
                self.confirming = true;
                self.selected = 0;
            }
            _ => {}
        }
        Outcome::Stay
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        frame.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(if self.confirming {
                " Confirm session group "
            } else {
                " Related sessions "
            })
            .border_style(theme.border_style(true));
        let inside = block.inner(area);
        frame.render_widget(block, area);
        let chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(2),
            Constraint::Length(5),
            Constraint::Length(2),
        ])
        .split(inside);
        let summary = format!(
            "{} {} sessions ({} files) · {} related files\n{}",
            self.plan.operation.label(),
            self.plan.session_count(),
            self.plan.members.len(),
            self.related.len(),
            if self.confirming {
                "Only selected and required members will be changed.".to_owned()
            } else {
                format!(
                    "Filter: {}{}",
                    self.query,
                    if self.searching { "▏" } else { "" }
                )
            }
        );
        frame.render_widget(
            Paragraph::new(summary).wrap(Wrap { trim: false }),
            chunks[0],
        );
        let rows = if self.confirming {
            self.plan.members.values().collect::<Vec<_>>()
        } else {
            self.visible()
        };
        let details = rows
            .get(self.selected)
            .map(|m| {
                format!(
                    "{}\n{}",
                    m.path.display(),
                    self.plan
                        .reasons
                        .get(&m.path)
                        .map(String::as_str)
                        .unwrap_or("Related but optional; not selected.")
                )
            })
            .unwrap_or_default();
        let items = rows
            .iter()
            .map(|m| {
                let label = if self.plan.seeds.contains(&m.path) {
                    "selected"
                } else if self.plan.members.contains_key(&m.path) {
                    "required"
                } else {
                    "optional"
                };
                let location = if m.trashed {
                    "trash"
                } else if m.archived {
                    "archive"
                } else {
                    "active"
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("[{label} · {location}] "),
                        ratatui::style::Style::default().fg(theme.accent),
                    ),
                    Span::raw(
                        m.path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                    ),
                ]))
            })
            .collect::<Vec<_>>();
        self.list
            .select(Some(self.selected.min(items.len().saturating_sub(1))));
        frame.render_stateful_widget(
            List::new(items).highlight_symbol("› "),
            chunks[1],
            &mut self.list,
        );
        let details = if !self.plan.blockers.is_empty() {
            format!(
                "Blocked: {}\n{}",
                self.plan.blockers.join("\n"),
                self.notice
            )
        } else if !self.notice.is_empty() {
            self.notice.clone()
        } else {
            details
        };
        frame.render_widget(
            Paragraph::new(details)
                .wrap(Wrap { trim: false })
                .scroll((self.detail_scroll, 0)),
            chunks[2],
        );
        frame.render_widget(Paragraph::new(if self.confirming { "↑↓ Select · PgUp/PgDn Details · r Review related sessions · y Confirm group action · Esc Cancel" }
            else { "↑↓ Select · Enter View · Space Toggle · / Filter\nt Trash · u Restore · D Delete · c Review operation · Esc Back" }).wrap(Wrap { trim: false }), chunks[3]);
    }
}
