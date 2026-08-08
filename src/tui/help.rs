use std::fmt;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use ratatui_textarea::TextArea;

use crate::ring_cursor::RingCursor;
use crate::tui::keymap_hint::{self, KeymapHint};
use crate::tui::layout;
use crate::tui::theme::Theme;
use crate::tui::util::block_title;

const HELP_HINTS: [KeymapHint; 4] = [
    KeymapHint::new("Esc", "clear/close"),
    KeymapHint::new("Tab", "switch tab"),
    KeymapHint::new("↑↓", "move"),
    KeymapHint::new("Type", "filter"),
];
const QUERY_HELP_HINTS: [KeymapHint; 3] = [
    KeymapHint::new("Esc", "close"),
    KeymapHint::new("Tab", "switch tab"),
    KeymapHint::new("↑↓", "scroll"),
];
const HELP_TABS: [HelpTab; 3] = [HelpTab::SessionList, HelpTab::Viewer, HelpTab::SearchQuery];

const PAGE_STEP: usize = 8;
const MOUSE_SCROLL_STEP: isize = 3;
const LEFT_PANEL_PREFERRED_WIDTH: u16 = 36;
const RIGHT_PANEL_MIN_CONTENT_WIDTH: u16 = 12;
const RIGHT_PANEL_HORIZONTAL_CHROME: u16 = 3;
const RIGHT_PANEL_MIN_WIDTH: u16 = RIGHT_PANEL_MIN_CONTENT_WIDTH + RIGHT_PANEL_HORIZONTAL_CHROME;
const HELP_KEY_COLUMN_WIDTH: usize = 12;

const SESSION_LIST_ITEMS: [HelpItem; 20] = [
    HelpItem::new(
        "Type",
        "filter sessions",
        "Type in the main search box to filter sessions. Searches dispatch after a short debounce, and the committed query is reused when you open the full viewer.",
    ),
    HelpItem::new(
        "? / ^L",
        "open this help",
        "Open the contextual hotkey help for the session list screen. This tab is selected by default when help is opened from the main search screen.",
    ),
    HelpItem::new(
        "Esc",
        "clear query / quit",
        "If the main query is not empty, Esc clears it and runs a fresh search. If the query is already empty, Esc exits the TUI.",
    ),
    HelpItem::new(
        "↑↓",
        "select session",
        "Move the highlighted session up or down in the result list without opening the actions menu.",
    ),
    HelpItem::new(
        "PgUp / PgDn",
        "page preview or list",
        "When the preview pane is visible, Page Up and Page Down scroll the preview. If the preview is hidden, they move the result selection by one page.",
    ),
    HelpItem::new(
        "Home / End",
        "jump to top / bottom",
        "When the preview pane is visible, Home jumps to the top of the preview and End jumps to the bottom. If the preview is hidden, they jump to the first or last result instead.",
    ),
    HelpItem::new(
        "Enter",
        "open actions",
        "Open the session actions menu for the selected result. From there you can view, export, copy metadata, delete, resume, or fork the session.",
    ),
    HelpItem::new(
        "^F",
        "filters",
        "Open the filter modal to change search filters and preview/viewer display toggles. Enter applies the current modal values; ^S applies them and also saves them as startup defaults.",
    ),
    HelpItem::new(
        "^G",
        "toggle scope",
        "Toggle the Scope Filter between Global and Local/cwd, then rerun the current search with the new scope immediately.",
    ),
    HelpItem::new(
        "^S",
        "settings",
        "Open settings to change theme, CLI handoff commands, session separators, and snippet line count.",
    ),
    HelpItem::new(
        "^T",
        "preview",
        "Show or hide the preview pane. This preference is saved so the next launch uses the same preview visibility.",
    ),
    HelpItem::new(
        "^N / ^P",
        "preview matches",
        "When the preview pane is visible and the search query is non-empty, jump to the first/next or previous/last highlighted match inside the preview, including matches outside the visible viewport.",
    ),
    HelpItem::new(
        "Shift ↑ / ↓",
        "jump message/event",
        "When the preview pane is visible, jump to the previous or next message or event boundary in the preview.",
    ),
    HelpItem::new(
        "^Shift ↑ / ↓",
        "jump user message",
        "When the preview pane is visible, jump backward or forward to the previous or next user-authored message, skipping assistant replies, tool calls, and tool results.",
    ),
    HelpItem::new(
        "^Y",
        "cycle snippet",
        "Cycle the selected session card snippet between session text and available summaries. The preview pane always shows all available summaries plus the session log.",
    ),
    HelpItem::new(
        "Shift ← / →",
        "resize preview",
        "Resize the preview pane width. Shift+Left moves the divider left; Shift+Right moves it right.",
    ),
    HelpItem::new(
        "^C",
        "quit immediately",
        "Exit the TUI without changing the current query or selection state first.",
    ),
    HelpItem::new(
        "Double click",
        "view session",
        "Double-click a session card in the result list to open the full-session viewer directly.",
    ),
    HelpItem::new(
        "Mouse scroll",
        "scroll sessions",
        "Scroll over the session list to move the selection up or down without opening the actions menu.",
    ),
    HelpItem::new(
        "Mouse scroll",
        "scroll preview",
        "Scroll over the preview pane to move through the preview content while leaving the selected session unchanged.",
    ),
];

const VIEWER_ITEMS: [HelpItem; 16] = [
    HelpItem::new(
        "?",
        "open this help",
        "Open the contextual hotkey help while the dedicated viewer is visible. The viewer tab is selected by default when help is opened from the viewer.",
    ),
    HelpItem::new(
        "Esc",
        "close viewer",
        "Close the dedicated viewer and return to the session list. Esc no longer clears the viewer search field first.",
    ),
    HelpItem::new(
        "Enter",
        "open actions menu",
        "Open the session actions menu for the session currently shown in the dedicated viewer. Cancelling that menu returns you to the same viewer state.",
    ),
    HelpItem::new(
        "Type",
        "edit viewer search",
        "The viewer search field is always focused. Typing updates the inline search immediately and highlights matching text inside the full conversation.",
    ),
    HelpItem::new(
        "^N",
        "next match",
        "Jump to the next highlighted match in the viewer. Navigation wraps when you reach the last match.",
    ),
    HelpItem::new(
        "^P",
        "previous match",
        "Jump to the previous highlighted match in the viewer. Navigation wraps when you move backward from the first match.",
    ),
    HelpItem::new(
        "^U / ^E",
        "edit line",
        "Use readline-style editing in the always-focused search box, such as Ctrl+U to clear backward and Ctrl+E to move to the end.",
    ),
    HelpItem::new(
        "Shift ↑",
        "jump to previous message/event",
        "Jump to the previous message or event boundary in the viewer, with wraparound at the ends.",
    ),
    HelpItem::new(
        "Shift ↓",
        "jump to next message/event",
        "Jump to the next message or event boundary in the viewer, with wraparound at the ends.",
    ),
    HelpItem::new(
        "^Shift ↑",
        "jump to previous user message",
        "Jump backward to the previous user-authored message, skipping assistant messages, tool calls, and tool results.",
    ),
    HelpItem::new(
        "^Shift ↓",
        "jump to next user message",
        "Jump forward to the next user-authored message, skipping assistant messages, tool calls, and tool results.",
    ),
    HelpItem::new(
        "↑ / ↓",
        "scroll line",
        "Scroll the viewer one line at a time.",
    ),
    HelpItem::new(
        "PgUp / PgDn",
        "scroll page",
        "Scroll the full conversation by a page at a time.",
    ),
    HelpItem::new(
        "Home",
        "jump to top",
        "Jump to the top of the viewer.",
    ),
    HelpItem::new(
        "End",
        "jump to bottom",
        "Jump to the bottom of the viewer.",
    ),
    HelpItem::new(
        "Mouse",
        "scroll viewer",
        "Use the mouse wheel over the viewer body to scroll the conversation without leaving the modal.",
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpTab {
    SessionList,
    Viewer,
    SearchQuery,
}

impl HelpTab {
    fn label(self) -> &'static str {
        match self {
            Self::SessionList => "Session List",
            Self::Viewer => "Viewer",
            Self::SearchQuery => "Search Query",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::SessionList => "Session List Hotkeys",
            Self::Viewer => "Viewer Hotkeys",
            Self::SearchQuery => "Search Query Help",
        }
    }

    fn items(self) -> Option<&'static [HelpItem]> {
        match self {
            Self::SessionList => Some(&SESSION_LIST_ITEMS),
            Self::Viewer => Some(&VIEWER_ITEMS),
            Self::SearchQuery => None,
        }
    }

    fn is_hotkey_tab(self) -> bool {
        self.items().is_some()
    }
}

#[derive(Clone, Copy)]
struct HelpItem {
    key: &'static str,
    short: &'static str,
    detail: &'static str,
}

impl HelpItem {
    const fn new(key: &'static str, short: &'static str, detail: &'static str) -> Self {
        Self { key, short, detail }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpOutcome {
    Stay,
    Close,
}

pub struct HelpModalState {
    tab: RingCursor<HelpTab>,
    selected: usize,
    query_scroll: u16,
    filter: TextArea<'static>,
}

impl fmt::Debug for HelpModalState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HelpModalState")
            .field("tab", &self.tab)
            .field("selected", &self.selected)
            .field("query_scroll", &self.query_scroll)
            .field("filter", &self.filter_text())
            .finish()
    }
}

impl HelpModalState {
    pub fn new(tab: HelpTab) -> Self {
        Self {
            tab: help_tab_cursor(tab),
            selected: 0,
            query_scroll: 0,
            filter: build_filter_input(),
        }
    }

    pub fn tab(&self) -> HelpTab {
        *self.tab.current()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> HelpOutcome {
        if !self.tab().is_hotkey_tab() {
            return self.handle_query_help_key(key);
        }

        match key.code {
            KeyCode::Esc => {
                if self.filter_text().is_empty() {
                    HelpOutcome::Close
                } else {
                    self.clear_filter();
                    HelpOutcome::Stay
                }
            }
            KeyCode::Tab => {
                self.switch_tab(true);
                HelpOutcome::Stay
            }
            KeyCode::BackTab => {
                self.switch_tab(false);
                HelpOutcome::Stay
            }
            KeyCode::Up | KeyCode::Char('k') if key.modifiers.is_empty() => {
                self.move_selection(-1);
                HelpOutcome::Stay
            }
            KeyCode::Down | KeyCode::Char('j') if key.modifiers.is_empty() => {
                self.move_selection(1);
                HelpOutcome::Stay
            }
            KeyCode::PageUp => {
                self.move_selection(-(PAGE_STEP as isize));
                HelpOutcome::Stay
            }
            KeyCode::PageDown => {
                self.move_selection(PAGE_STEP as isize);
                HelpOutcome::Stay
            }
            KeyCode::Home => {
                self.selected = 0;
                HelpOutcome::Stay
            }
            KeyCode::End => {
                self.selected = self.filtered_items().len().saturating_sub(1);
                HelpOutcome::Stay
            }
            KeyCode::Enter => HelpOutcome::Stay,
            KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                HelpOutcome::Stay
            }
            _ => {
                if self.filter.input(key) {
                    self.collapse_filter_to_single_line();
                    self.selected = 0;
                }
                HelpOutcome::Stay
            }
        }
    }

    pub fn handle_mouse(
        &mut self,
        area: Rect,
        kind: MouseEventKind,
        column: u16,
        row: u16,
    ) -> HelpOutcome {
        if !self.tab().is_hotkey_tab() {
            return self.handle_query_help_mouse(area, kind, column, row);
        }

        let chunks = hotkey_left_chunks(area);
        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(tab) = tab_at_position(chunks[0], column, row) {
                    self.select_tab(tab);
                } else if let Some(index) = self.hotkey_index_at_position(chunks[2], column, row) {
                    self.selected = index;
                }
            }
            MouseEventKind::ScrollDown => {
                if contains(chunks[2], column, row) {
                    self.move_selection(MOUSE_SCROLL_STEP);
                }
            }
            MouseEventKind::ScrollUp if contains(chunks[2], column, row) => {
                self.move_selection(-MOUSE_SCROLL_STEP);
            }
            _ => {}
        }
        HelpOutcome::Stay
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let popup = help_popup_area(area);
        frame.render_widget(Clear, popup);

        if !self.tab().is_hotkey_tab() {
            self.render_query_help(frame, popup, theme);
            return;
        }

        let [left_area, right_area] = split_help_columns(popup);

        let left_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border_style(true))
            .title(block_title(self.tab.current().title()));
        frame.render_widget(left_block.clone(), left_area);

        let left_inner = padded_inner(left_area);
        let left_chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(left_inner);

        frame.render_widget(Paragraph::new(self.render_tabs(theme)), left_chunks[0]);

        self.configure_filter_widget(theme);
        frame.render_widget(&self.filter, left_chunks[1]);

        let filtered = self.filtered_items();
        let max_rows = left_chunks[2].height as usize;
        let (start, end) = visible_window(filtered.len(), self.selected, max_rows);
        let rows = if filtered.is_empty() {
            vec![Line::from(Span::styled(
                "No matching keys",
                Style::default().fg(theme.muted),
            ))]
        } else {
            filtered[start..end]
                .iter()
                .enumerate()
                .map(|(visible_index, item)| {
                    let absolute_index = start + visible_index;
                    let selected = absolute_index == self.selected;
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
                        Span::styled(format!("{:<HELP_KEY_COLUMN_WIDTH$} ", item.key), style),
                        Span::styled(item.short, style),
                    ])
                })
                .collect::<Vec<_>>()
        };
        frame.render_widget(Paragraph::new(rows), left_chunks[2]);
        keymap_hint::render(frame, left_chunks[3], &HELP_HINTS, theme, "");

        let right_block = Block::default()
            .borders(Borders::TOP | Borders::RIGHT | Borders::BOTTOM)
            .border_type(BorderType::Rounded)
            .border_style(theme.border_style(false));
        frame.render_widget(right_block, right_area);

        let right_inner = padded_inner(right_area);
        let desc_chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(right_inner);

        let title = if let Some(item) = filtered.get(self.selected) {
            Line::from(vec![
                Span::styled(
                    item.key,
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("  ", Style::default()),
                Span::styled(
                    item.short,
                    Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                ),
            ])
        } else {
            Line::from(Span::styled(
                "No matching keys",
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ))
        };
        frame.render_widget(Paragraph::new(title), desc_chunks[0]);

        let sep_width = desc_chunks[1].width as usize;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "─".repeat(sep_width),
                Style::default().fg(theme.border),
            ))),
            desc_chunks[1],
        );

        let description = if let Some(item) = filtered.get(self.selected) {
            format!(
                "{}\n\nTip: the filter box supports readline-style editing shortcuts such as Ctrl+A, Ctrl+E, Ctrl+W, Alt+B, and Alt+F.",
                item.detail
            )
        } else {
            "No keys match the current filter.\n\nTip: press Esc once to clear the filter box, then Esc again to close help.".to_owned()
        };
        frame.render_widget(
            Paragraph::new(Span::styled(description, Style::default().fg(theme.muted)))
                .wrap(Wrap { trim: false }),
            desc_chunks[2],
        );
    }

    fn handle_query_help_key(&mut self, key: KeyEvent) -> HelpOutcome {
        match key.code {
            KeyCode::Esc => HelpOutcome::Close,
            KeyCode::Tab => {
                self.switch_tab(true);
                HelpOutcome::Stay
            }
            KeyCode::BackTab => {
                self.switch_tab(false);
                HelpOutcome::Stay
            }
            KeyCode::Up | KeyCode::Char('k') if key.modifiers.is_empty() => {
                self.query_scroll = self.query_scroll.saturating_sub(1);
                HelpOutcome::Stay
            }
            KeyCode::Down | KeyCode::Char('j') if key.modifiers.is_empty() => {
                self.query_scroll = self.query_scroll.saturating_add(1);
                HelpOutcome::Stay
            }
            KeyCode::PageUp => {
                self.query_scroll = self.query_scroll.saturating_sub(PAGE_STEP as u16);
                HelpOutcome::Stay
            }
            KeyCode::PageDown => {
                self.query_scroll = self.query_scroll.saturating_add(PAGE_STEP as u16);
                HelpOutcome::Stay
            }
            KeyCode::Home => {
                self.query_scroll = 0;
                HelpOutcome::Stay
            }
            KeyCode::End => {
                self.query_scroll = u16::MAX / 4;
                HelpOutcome::Stay
            }
            KeyCode::Enter => HelpOutcome::Stay,
            KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                HelpOutcome::Stay
            }
            _ => HelpOutcome::Stay,
        }
    }

    fn handle_query_help_mouse(
        &mut self,
        area: Rect,
        kind: MouseEventKind,
        column: u16,
        row: u16,
    ) -> HelpOutcome {
        let chunks = query_help_chunks(area);
        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(tab) = tab_at_position(chunks[0], column, row) {
                    self.select_tab(tab);
                }
            }
            MouseEventKind::ScrollDown => {
                if contains(chunks[2], column, row) {
                    self.query_scroll = self.query_scroll.saturating_add(MOUSE_SCROLL_STEP as u16);
                }
            }
            MouseEventKind::ScrollUp if contains(chunks[2], column, row) => {
                self.query_scroll = self.query_scroll.saturating_sub(MOUSE_SCROLL_STEP as u16);
            }
            _ => {}
        }
        HelpOutcome::Stay
    }

    fn render_query_help(&mut self, frame: &mut Frame, popup: Rect, theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border_style(true))
            .title(block_title(self.tab.current().title()));
        frame.render_widget(block, popup);

        let chunks = query_help_chunks_for_popup(popup);

        frame.render_widget(Paragraph::new(self.render_tabs(theme)), chunks[0]);

        let sep_width = chunks[1].width as usize;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "─".repeat(sep_width),
                Style::default().fg(theme.border),
            ))),
            chunks[1],
        );

        frame.render_widget(
            Paragraph::new(search_query_help_lines(theme))
                .style(Style::default().fg(theme.text))
                .scroll((self.query_scroll, 0))
                .wrap(Wrap { trim: false }),
            chunks[2],
        );
        keymap_hint::render(frame, chunks[3], &QUERY_HELP_HINTS, theme, "");
    }

    fn switch_tab(&mut self, forward: bool) {
        if forward {
            self.tab.move_next();
        } else {
            self.tab.move_prev();
        }
        self.selected = 0;
        self.query_scroll = 0;
    }

    fn select_tab(&mut self, tab: HelpTab) {
        if self.tab.set(&tab) {
            self.selected = 0;
            self.query_scroll = 0;
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.filtered_items().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let max_index = len.saturating_sub(1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, max_index) as usize;
    }

    fn filtered_items(&self) -> Vec<&'static HelpItem> {
        let filter = self.filter_text().trim().to_ascii_lowercase();
        self.tab
            .current()
            .items()
            .unwrap_or_default()
            .iter()
            .filter(|item| {
                filter.is_empty()
                    || item.key.to_ascii_lowercase().contains(&filter)
                    || item.short.to_ascii_lowercase().contains(&filter)
                    || item.detail.to_ascii_lowercase().contains(&filter)
            })
            .collect()
    }

    fn hotkey_index_at_position(&self, list: Rect, column: u16, row: u16) -> Option<usize> {
        if !contains(list, column, row) {
            return None;
        }
        let filtered = self.filtered_items();
        let max_rows = list.height as usize;
        let (start, end) = visible_window(filtered.len(), self.selected, max_rows);
        let index = start + (row - list.y) as usize;
        (index < end).then_some(index)
    }

    fn filter_text(&self) -> &str {
        self.filter
            .lines()
            .first()
            .map(String::as_str)
            .unwrap_or("")
    }

    fn clear_filter(&mut self) {
        self.filter = build_filter_input();
        self.selected = 0;
    }

    fn collapse_filter_to_single_line(&mut self) {
        let joined = self.filter.lines().join(" ");
        if self.filter.lines().len() > 1 {
            self.filter = build_filter_input();
            if !joined.is_empty() {
                self.filter.insert_str(joined);
            }
        }
    }

    fn configure_filter_widget(&mut self, theme: &Theme) {
        self.filter.set_style(Style::default().fg(theme.text));
        self.filter.set_cursor_line_style(Style::default());
        self.filter.set_cursor_style(
            Style::default()
                .fg(theme.list_body_bg)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD),
        );
        self.filter
            .set_placeholder_style(Style::default().fg(theme.muted_greater));
        self.filter.set_block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(theme.border_style(true)),
        );
    }

    fn render_tabs(&self, theme: &Theme) -> Line<'static> {
        let mut spans = vec![Span::raw(" ")];
        for (index, tab) in HELP_TABS.into_iter().enumerate() {
            if index > 0 {
                spans.push(Span::styled("  ", Style::default().fg(theme.muted)));
            }
            let selected = self.tab == tab;
            let style = if selected {
                Style::default()
                    .fg(theme.text)
                    .bg(theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted)
            };
            spans.push(Span::styled(format!(" {} ", tab.label()), style));
        }
        Line::from(spans)
    }
}

fn build_filter_input() -> TextArea<'static> {
    let mut filter = TextArea::default();
    filter.set_cursor_line_style(Style::default());
    filter.set_placeholder_text("type to filter");
    filter
}

fn help_popup_area(area: Rect) -> Rect {
    layout::centered_rect(area, 80, 72)
}

fn hotkey_left_chunks(area: Rect) -> Vec<Rect> {
    let popup = help_popup_area(area);
    let [left_area, _] = split_help_columns(popup);
    let left_inner = padded_inner(left_area);
    Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .split(left_inner)
    .to_vec()
}

fn query_help_chunks(area: Rect) -> Vec<Rect> {
    query_help_chunks_for_popup(help_popup_area(area))
}

fn query_help_chunks_for_popup(popup: Rect) -> Vec<Rect> {
    let inner = padded_inner(popup);
    Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .split(inner)
    .to_vec()
}

fn tab_at_position(area: Rect, column: u16, row: u16) -> Option<HelpTab> {
    HELP_TABS.into_iter().find(|tab| {
        tab_span(area, *tab)
            .is_some_and(|(start, end)| row == area.y && column >= start && column < end)
    })
}

fn tab_span(area: Rect, target: HelpTab) -> Option<(u16, u16)> {
    let mut cursor = area.x.saturating_add(1);
    for (index, tab) in HELP_TABS.into_iter().enumerate() {
        if index > 0 {
            cursor = cursor.saturating_add(2);
        }
        let width = (tab.label().len() as u16).saturating_add(2);
        let start = cursor;
        let end = cursor.saturating_add(width).min(area.right());
        if tab == target {
            return Some((start, end));
        }
        cursor = cursor.saturating_add(width);
    }
    None
}

fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.right() && row >= area.y && row < area.bottom()
}

fn help_tab_cursor(selected: HelpTab) -> RingCursor<HelpTab> {
    let mut cursor = RingCursor::new(HELP_TABS.to_vec());
    assert!(cursor.set(&selected));
    cursor
}

fn search_query_help_lines(theme: &Theme) -> Vec<Line<'static>> {
    let heading = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let body = Style::default().fg(theme.text);
    let muted = Style::default().fg(theme.muted);

    search_query_help_text()
        .lines()
        .map(|line| {
            if line.starts_with("## ") {
                Line::from(Span::styled(line.trim_start_matches("## "), heading))
            } else if line.starts_with("- ") {
                Line::from(vec![
                    Span::styled("  - ", muted),
                    Span::styled(line.trim_start_matches("- "), body),
                ])
            } else if line.is_empty() {
                Line::default()
            } else {
                Line::from(Span::styled(line, body))
            }
        })
        .collect()
}

fn search_query_help_text() -> &'static str {
    "## What AICS searches\n\
- The query is trimmed before it is sent to the search engine.\n\
- An empty query shows recent sessions instead of running a text query.\n\
- Non-empty queries run through Tantivy's lenient QueryParser against AICS's default content field.\n\
- That content field contains the custom thread title, the first user/resume-preview text, and the full parsed transcript.\n\
\n\
## Query syntax\n\
- Bare words are token searches. Tantivy's default tokenizer handles case and punctuation normalization.\n\
- Multiple bare words are AND by default: rust serde means both terms must match.\n\
- Use uppercase AND, OR, and NOT for explicit boolean logic. Lowercase and/or/not are ordinary words.\n\
- Use parentheses to group boolean clauses, for example (rust OR go) parser.\n\
- Use quotes for an exact phrase, for example \"vector db\".\n\
- Use working_dir:PATH or wd:PATH to match a case-insensitive working-directory prefix from any path-component boundary; for example wd:my/ja.\n\
- Wrap a Tantivy term regex in < and >, optionally after a field name; for example wd:<.*codex/.*8ba3f7e.*>. Slashes need no escaping. Regexes match whole indexed terms, and \\> puts a literal > in the regex.\n\
- AICS parses leniently, so malformed input should not open an error screen; Tantivy returns the usable parts it can parse.\n\
\n\
## AICS ranking and display\n\
- Bare multi-word queries without explicit boolean operators get an extra quoted-phrase query boosted 5x, so exact adjacent wording ranks higher without making the phrase required.\n\
- Sort: Time orders matching sessions by modified timestamp. Relevance uses Tantivy scores, then AICS applies a recency boost and timestamp tie-breaks.\n\
- Filters still apply after text search: scope, agent, branch, dates, line count, derivation type, sub-agent, live-only, and trash settings can hide otherwise matching sessions.\n\
- Snippets prefer Tantivy-selected fragments. If that is unavailable, AICS falls back to first user text, first message, then last message.\n\
- Highlighting is AICS post-processing: query terms are extracted independently, uppercase boolean operators are not highlighted, and fallback snippets strip leading AGENTS.md/CLAUDE.md/environment boilerplate."
}

fn padded_inner(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

fn split_help_columns(area: Rect) -> [Rect; 2] {
    let left_width = preferred_left_panel_width(area.width);
    let columns =
        Layout::horizontal([Constraint::Length(left_width), Constraint::Min(0)]).split(area);
    [columns[0], columns[1]]
}

fn preferred_left_panel_width(total_width: u16) -> u16 {
    if total_width == 0 {
        return 0;
    }

    if total_width >= LEFT_PANEL_PREFERRED_WIDTH + RIGHT_PANEL_MIN_WIDTH {
        return LEFT_PANEL_PREFERRED_WIDTH;
    }

    total_width.saturating_sub(RIGHT_PANEL_MIN_WIDTH).max(1)
}

fn visible_window(total: usize, selected: usize, max_rows: usize) -> (usize, usize) {
    if total == 0 {
        return (0, 0);
    }
    let max_rows = max_rows.max(1);
    let mut start = 0usize;
    if selected >= max_rows {
        start = selected + 1 - max_rows;
    }
    let end = (start + max_rows).min(total);
    (start, end)
}

#[cfg(test)]
fn help_item_summary_text(item: &HelpItem) -> String {
    format!("{:<HELP_KEY_COLUMN_WIDTH$} {}", item.key, item.short)
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
    use ratatui::layout::Rect;

    use super::{
        help_item_summary_text, hotkey_left_chunks, preferred_left_panel_width, query_help_chunks,
        search_query_help_text, split_help_columns, tab_span, HelpItem, HelpModalState,
        HelpOutcome, HelpTab, LEFT_PANEL_PREFERRED_WIDTH, RIGHT_PANEL_MIN_CONTENT_WIDTH,
        RIGHT_PANEL_MIN_WIDTH,
    };

    #[test]
    fn esc_clears_filter_before_closing() {
        let mut help = HelpModalState::new(HelpTab::SessionList);
        help.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));

        assert_eq!(
            help.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            HelpOutcome::Stay
        );
        assert_eq!(help.filter_text(), "");

        assert_eq!(
            help.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            HelpOutcome::Close
        );
    }

    #[test]
    fn tab_switches_help_tabs() {
        let mut help = HelpModalState::new(HelpTab::SessionList);

        help.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(help.tab(), HelpTab::Viewer);

        help.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(help.tab(), HelpTab::SessionList);
    }

    #[test]
    fn tab_reaches_search_query_help() {
        let mut help = HelpModalState::new(HelpTab::SessionList);

        help.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        help.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

        assert_eq!(help.tab(), HelpTab::SearchQuery);
    }

    #[test]
    fn clicking_help_tab_switches_tab() {
        let mut help = HelpModalState::new(HelpTab::SessionList);
        let area = Rect::new(0, 0, 120, 40);
        let tabs = hotkey_left_chunks(area)[0];
        let (start, _) = tab_span(tabs, HelpTab::Viewer).expect("viewer tab span");

        help.handle_mouse(area, MouseEventKind::Down(MouseButton::Left), start, tabs.y);

        assert_eq!(help.tab(), HelpTab::Viewer);
        assert_eq!(help.selected, 0);
    }

    #[test]
    fn clicking_help_item_selects_visible_row() {
        let mut help = HelpModalState::new(HelpTab::SessionList);
        let area = Rect::new(0, 0, 120, 40);
        let list = hotkey_left_chunks(area)[2];

        help.handle_mouse(
            area,
            MouseEventKind::Down(MouseButton::Left),
            list.x,
            list.y + 2,
        );

        assert_eq!(help.selected, 2);
    }

    #[test]
    fn mouse_scrolls_search_query_help() {
        let mut help = HelpModalState::new(HelpTab::SearchQuery);
        let area = Rect::new(0, 0, 120, 40);
        let content = query_help_chunks(area)[2];

        help.handle_mouse(area, MouseEventKind::ScrollDown, content.x, content.y);
        assert_eq!(help.query_scroll, 3);

        help.handle_mouse(area, MouseEventKind::ScrollUp, content.x, content.y);
        assert_eq!(help.query_scroll, 0);
    }

    #[test]
    fn search_query_help_ignores_filter_typing() {
        let mut help = HelpModalState::new(HelpTab::SearchQuery);

        help.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));

        assert_eq!(help.tab(), HelpTab::SearchQuery);
        assert_eq!(help.filter_text(), "");
        assert!(help.filtered_items().is_empty());
    }

    #[test]
    fn search_query_help_mentions_aics_processing() {
        let text = search_query_help_text();

        assert!(text.contains("Tantivy's lenient QueryParser"));
        assert!(text.contains("custom thread title"));
        assert!(text.contains("first user/resume-preview text"));
        assert!(text.contains("full parsed transcript"));
        assert!(text.contains("wd:my/ja"));
        assert!(text.contains("wd:<.*codex/.*8ba3f7e.*>"));
        assert!(text.contains("boosted 5x"));
        assert!(text.contains("recency boost"));
        assert!(text.contains("AGENTS.md/CLAUDE.md/environment boilerplate"));
    }

    #[test]
    fn filtering_restricts_visible_entries() {
        let mut help = HelpModalState::new(HelpTab::Viewer);
        for ch in "mouse".chars() {
            help.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }

        let items = help.filtered_items();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].key, "Mouse");
    }

    #[test]
    fn help_item_summary_keeps_space_after_full_width_key() {
        let item = HelpItem::new(
            "Double click",
            "view session",
            "Double-click a session card in the result list.",
        );

        assert_eq!(help_item_summary_text(&item), "Double click view session");
    }

    #[test]
    fn right_panel_absorbs_shrink_until_minimum_content_width() {
        let total_width = LEFT_PANEL_PREFERRED_WIDTH + RIGHT_PANEL_MIN_WIDTH + 7;
        assert_eq!(
            preferred_left_panel_width(total_width),
            LEFT_PANEL_PREFERRED_WIDTH
        );

        let [left, right] = split_help_columns(Rect::new(0, 0, total_width, 20));
        assert_eq!(left.width, LEFT_PANEL_PREFERRED_WIDTH);
        assert_eq!(right.width, total_width - LEFT_PANEL_PREFERRED_WIDTH);
        assert!(right.width.saturating_sub(3) >= RIGHT_PANEL_MIN_CONTENT_WIDTH);
    }

    #[test]
    fn left_panel_shrinks_after_right_panel_hits_minimum_width() {
        let total_width = LEFT_PANEL_PREFERRED_WIDTH + RIGHT_PANEL_MIN_WIDTH - 4;
        let [left, right] = split_help_columns(Rect::new(0, 0, total_width, 20));

        assert_eq!(right.width, RIGHT_PANEL_MIN_WIDTH);
        assert_eq!(left.width, total_width - RIGHT_PANEL_MIN_WIDTH);
    }
}
