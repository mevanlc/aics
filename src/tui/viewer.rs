use std::collections::{BTreeMap, BTreeSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::path::PathBuf;

use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::export::{selected_blocks_to_markdown, SessionBlockId};
use crate::parse::{MessageRole, Session, SessionCell};
use crate::search_match::{DocumentMap, SourceBlock};
use crate::search_projection::SearchProjection;
use crate::search_query::{QueryPlan, VisibilitySearch};
use crate::settings::{DisplayOptions, ThemeName};
use crate::summary::SummarySidecar;
use crate::tui::keymap_hint::{self, KeymapHint};
use crate::tui::markdown::render_markdown_message;
use crate::tui::preview::{
    render_session_document_with_options, split_sticky_body, DisplayBlock, DisplayDocument,
    SessionRenderOptions,
};
use crate::tui::profile;
use crate::tui::statusline;
use crate::tui::text_layout::LayoutDocument;
use crate::tui::theme::Theme;
use crate::tui::util::{
    abbreviate_home_path, agent_badge, block_title, format_line_count, relative_time,
    right_block_title, session_display_title, sticky_header_for_scroll,
    FullLineBackgroundParagraph, StickyHeaderWidget, StickyRowMarker, STICKY_HEADER_HEIGHT,
};

const VIEWER_PAGE_STEP: usize = 12;
const VIEWER_SEARCH_HEIGHT: u16 = 3; // bordered search input
const VIEWER_HINTS_HEIGHT: u16 = 2; // keymap hints (2 wrapping lines)
const VIEWER_FOOTER_HEIGHT: u16 = VIEWER_SEARCH_HEIGHT + VIEWER_HINTS_HEIGHT;
const VIEWER_MATCH_SCROLLOFF: usize = 3;

#[derive(Debug, Clone)]
pub struct ViewerState {
    pub scroll: usize,
    search: Input,
    find: Input,
    input_focus: ViewerInputFocus,
    find_regex: bool,
    find_case_sensitive: bool,
    find_title_hits: [Rect; 2],
    find_jump: bool,
    find_anchor: Option<usize>,
    active_find: Option<usize>,
    visibility_search: VisibilitySearch,
    active_match: Option<usize>,
    render_cache: Option<ViewerRenderCache>,
    filter_scroll_snapshot: Option<FilterScrollSnapshot>,
    selection_path: Option<PathBuf>,
    selected_blocks: BTreeSet<SessionBlockId>,
    selection_anchor: Option<SessionBlockId>,
    pub(crate) status: Option<statusline::Entry>,
}

#[derive(Debug, Clone)]
struct ViewerRenderCache {
    path: PathBuf,
    query: String,
    visibility_search: VisibilitySearch,
    session: Session,
    projection: SearchProjection,
    map: DocumentMap,
    layout: LayoutDocument,
    query_ranges: Vec<Vec<Range<usize>>>,
    query_match: bool,
    query_match_is_certain: bool,
    query_undisplayed: usize,
    query_hidden: usize,
    query_metadata: usize,
    query_source_only: usize,
    query_error: Option<String>,
    find_signature: Option<(String, bool, bool)>,
    find_ranges: Vec<Range<usize>>,
    find_rows: Vec<usize>,
    find_error: Option<String>,
    width: u16,
    theme_name: ThemeName,
    display_options: DisplayOptions,
    summary_stamp: Option<(i64, usize, u64)>,
    total_rows: usize,
    text: Text<'static>,
    match_rows: Vec<usize>,
    sticky_rows: Vec<StickyRowMarker>,
    sticky_markers: Vec<crate::tui::util::StickyLineMarker>,
    blocks: Vec<ViewerBlock>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewerInputFocus {
    Search,
    Find,
}

#[derive(Debug, Clone)]
struct ViewerBlock {
    source: DisplayBlock,
    rows: Range<usize>,
}

#[derive(Debug, Clone)]
struct FilterScrollSnapshot {
    path: PathBuf,
    top_row: usize,
    blocks: Vec<(SessionBlockId, Range<usize>)>,
}

impl FilterScrollSnapshot {
    fn restored_scroll(&self, blocks: &[ViewerBlock]) -> usize {
        let new_rows: BTreeMap<_, _> = blocks
            .iter()
            .map(|block| (block.source.id, block.rows.start))
            .collect();
        self.blocks
            .iter()
            .filter_map(|(id, rows)| {
                let new_row = *new_rows.get(id)?;
                // Distance to the nearest occupied row, not just the block's heading.
                let distance = rows
                    .start
                    .saturating_sub(self.top_row)
                    .max(self.top_row.saturating_sub(rows.end.saturating_sub(1)));
                // At equal distance prefer the block following the old viewport top.
                Some(((distance, rows.end <= self.top_row), new_row))
            })
            .min_by_key(|(rank, _)| *rank)
            .map_or(0, |(_, row)| row)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerOutcome {
    Stay,
    Close,
    CopySelection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MatchDirection {
    Next,
    Previous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MessageDirection {
    Next,
    Previous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MessageJumpScope {
    Any,
    UserOnly,
}

impl Default for ViewerState {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewerState {
    const HINTS: [KeymapHint; 9] = [
        KeymapHint::new("↑↓/Pg", "scroll"),
        KeymapHint::new("⇧↑↓/^⇧↑↓", "message/user"),
        KeymapHint::new("^N/^P", "matches"),
        KeymapHint::new("Tab/^F", "focus"),
        KeymapHint::new("Alt+R/I", "regex/case"),
        KeymapHint::new("^⇧F", "filters"),
        KeymapHint::new("^/⇧Click", "select"),
        KeymapHint::new("Alt+C", "copy"),
        KeymapHint::new("Esc", "close"),
    ];

    pub fn new() -> Self {
        Self::with_search("")
    }

    pub fn with_search(query: &str) -> Self {
        Self {
            scroll: 0,
            search: Input::default().with_value(query.to_owned()),
            find: Input::default(),
            input_focus: ViewerInputFocus::Find,
            find_regex: false,
            find_case_sensitive: false,
            find_title_hits: [Rect::ZERO; 2],
            find_jump: true,
            find_anchor: None,
            active_find: None,
            visibility_search: VisibilitySearch::default(),
            active_match: None,
            render_cache: None,
            filter_scroll_snapshot: None,
            selection_path: None,
            selected_blocks: BTreeSet::new(),
            selection_anchor: None,
            status: None,
        }
    }

    pub fn search_query(&self) -> &str {
        self.search.value()
    }

    pub fn find_query(&self) -> &str {
        self.find.value()
    }

    pub(crate) fn search_is_focused(&self) -> bool {
        self.input_focus == ViewerInputFocus::Search
    }

    pub(crate) fn set_search_options(&mut self, visibility: VisibilitySearch, find_jump: bool) {
        if self.visibility_search != visibility {
            self.visibility_search = visibility;
        }
        self.find_jump = find_jump;
    }

    pub(crate) fn reset_find_anchor(&mut self) {
        self.find_anchor = None;
    }

    fn toggle_find_regex(&mut self) {
        self.find_regex = !self.find_regex;
        self.active_find = None;
    }

    fn toggle_find_case(&mut self) {
        self.find_case_sensitive = !self.find_case_sensitive;
        self.active_find = None;
    }

    #[allow(clippy::too_many_arguments)]
    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        area: Rect,
        session: Option<&Session>,
        summary: Option<&SummarySidecar>,
        theme: &Theme,
        theme_name: ThemeName,
        display_options: DisplayOptions,
    ) -> ViewerOutcome {
        if matches!(
            key.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End
        ) {
            self.reset_find_anchor();
        }
        match key.code {
            KeyCode::Esc => ViewerOutcome::Close,
            KeyCode::Tab | KeyCode::BackTab => {
                self.input_focus = match self.input_focus {
                    ViewerInputFocus::Search => ViewerInputFocus::Find,
                    ViewerInputFocus::Find => ViewerInputFocus::Search,
                };
                ViewerOutcome::Stay
            }
            KeyCode::Char('f') if key.modifiers == KeyModifiers::CONTROL => {
                self.input_focus = ViewerInputFocus::Find;
                ViewerOutcome::Stay
            }
            KeyCode::Char('r')
                if key.modifiers == KeyModifiers::ALT
                    && self.input_focus == ViewerInputFocus::Find =>
            {
                self.toggle_find_regex();
                if let Some(session) = session {
                    self.render_cache(area, session, summary, theme, theme_name, display_options);
                }
                ViewerOutcome::Stay
            }
            KeyCode::Char('i')
                if key.modifiers == KeyModifiers::ALT
                    && self.input_focus == ViewerInputFocus::Find =>
            {
                self.toggle_find_case();
                if let Some(session) = session {
                    self.render_cache(area, session, summary, theme, theme_name, display_options);
                }
                ViewerOutcome::Stay
            }
            KeyCode::Enter if self.input_focus == ViewerInputFocus::Find => {
                let direction = if key.modifiers.contains(KeyModifiers::SHIFT) {
                    MatchDirection::Previous
                } else {
                    MatchDirection::Next
                };
                self.jump_to_match(
                    direction,
                    area,
                    session,
                    summary,
                    theme,
                    theme_name,
                    display_options,
                );
                ViewerOutcome::Stay
            }
            KeyCode::Char('c') if key.modifiers == KeyModifiers::ALT => {
                if let Some(session) = session {
                    self.render_cache(area, session, summary, theme, theme_name, display_options);
                }
                ViewerOutcome::CopySelection
            }
            KeyCode::Up if key.modifiers == (KeyModifiers::CONTROL | KeyModifiers::SHIFT) => {
                self.jump_to_message(
                    MessageDirection::Previous,
                    MessageJumpScope::UserOnly,
                    area,
                    session,
                    summary,
                    theme,
                    theme_name,
                    display_options,
                );
                ViewerOutcome::Stay
            }
            KeyCode::Down if key.modifiers == (KeyModifiers::CONTROL | KeyModifiers::SHIFT) => {
                self.jump_to_message(
                    MessageDirection::Next,
                    MessageJumpScope::UserOnly,
                    area,
                    session,
                    summary,
                    theme,
                    theme_name,
                    display_options,
                );
                ViewerOutcome::Stay
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.jump_to_match(
                    MatchDirection::Next,
                    area,
                    session,
                    summary,
                    theme,
                    theme_name,
                    display_options,
                );
                ViewerOutcome::Stay
            }
            KeyCode::Char('p') if key.modifiers == KeyModifiers::CONTROL => {
                self.jump_to_match(
                    MatchDirection::Previous,
                    area,
                    session,
                    summary,
                    theme,
                    theme_name,
                    display_options,
                );
                ViewerOutcome::Stay
            }
            KeyCode::Up if key.modifiers == KeyModifiers::SHIFT => {
                self.jump_to_message(
                    MessageDirection::Previous,
                    MessageJumpScope::Any,
                    area,
                    session,
                    summary,
                    theme,
                    theme_name,
                    display_options,
                );
                ViewerOutcome::Stay
            }
            KeyCode::Down if key.modifiers == KeyModifiers::SHIFT => {
                self.jump_to_message(
                    MessageDirection::Next,
                    MessageJumpScope::Any,
                    area,
                    session,
                    summary,
                    theme,
                    theme_name,
                    display_options,
                );
                ViewerOutcome::Stay
            }
            KeyCode::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                ViewerOutcome::Stay
            }
            KeyCode::Down => {
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
            KeyCode::Home => {
                self.scroll = 0;
                ViewerOutcome::Stay
            }
            KeyCode::End => {
                self.scroll = usize::MAX / 4;
                ViewerOutcome::Stay
            }
            _ => {
                let input = match self.input_focus {
                    ViewerInputFocus::Search => &mut self.search,
                    ViewerInputFocus::Find => &mut self.find,
                };
                let before = input.value().to_owned();
                input.handle_event(&Event::Key(key));
                if input.value() != before {
                    match self.input_focus {
                        ViewerInputFocus::Search => self.active_match = None,
                        ViewerInputFocus::Find => {
                            self.active_find = None;
                            self.find_anchor.get_or_insert(self.scroll);
                        }
                    }
                    if let Some(session) = session {
                        self.render_cache(
                            area,
                            session,
                            summary,
                            theme,
                            theme_name,
                            display_options,
                        );
                    }
                }
                ViewerOutcome::Stay
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        session: &Session,
        trashed: bool,
        summary: Option<&SummarySidecar>,
        theme: &Theme,
        theme_name: ThemeName,
        display_options: DisplayOptions,
    ) {
        let _profile = profile::scope("viewer.render");
        frame.render_widget(Clear, area);
        let chunks = split_viewer(area);
        let body_area = chunks[0];

        self.render_cache(area, session, summary, theme, theme_name, display_options);
        let (total_rows, mut text, sticky_rows, blocks) = {
            let cache = self
                .render_cache
                .as_ref()
                .expect("viewer cache should exist");
            let mut text = cache.layout.text.clone();
            let query_ranges: Vec<_> = cache.query_ranges.iter().flatten().cloned().collect();
            cache
                .layout
                .overlay_ranges(&mut text, &query_ranges, theme.search_match_style());
            cache.layout.overlay_ranges(
                &mut text,
                &cache.find_ranges,
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::UNDERLINED),
            );
            let active_ranges = match self.input_focus {
                ViewerInputFocus::Search => self
                    .active_match
                    .and_then(|index| cache.query_ranges.get(index))
                    .cloned()
                    .unwrap_or_default(),
                ViewerInputFocus::Find => self
                    .active_find
                    .and_then(|index| cache.find_ranges.get(index))
                    .cloned()
                    .into_iter()
                    .collect(),
            };
            cache
                .layout
                .overlay_ranges(&mut text, &active_ranges, theme.active_match_style());
            (
                cache.total_rows,
                text,
                cache.sticky_rows.clone(),
                cache.blocks.clone(),
            )
        };
        for block in &blocks {
            if self.selected_blocks.contains(&block.source.id) {
                for line in &mut text.lines[block.rows.clone()] {
                    line.style = line.style.bg(theme.selection);
                    for span in &mut line.spans {
                        if span.style.bg != Some(theme.search_match_bg)
                            && span.style.bg != Some(theme.active_match_bg)
                        {
                            span.style = span.style.bg(theme.selection);
                        }
                    }
                }
            }
        }
        let viewport_height = body_area
            .height
            .saturating_sub(2)
            .saturating_sub(STICKY_HEADER_HEIGHT) as usize;
        let scroll = self.scroll.min(total_rows.saturating_sub(viewport_height));
        let sticky_header = sticky_header_for_scroll(&sticky_rows, scroll);
        let scroll_percent = scroll_progress_percent(scroll, viewport_height, total_rows);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border_style(false))
            .title(block_title(viewer_title(
                session,
                trashed,
                theme,
                scroll_percent,
            )));
        let inner = block.inner(body_area);
        frame.render_widget(block, body_area);
        let (header_area, body_content_area) = split_sticky_body(inner);
        frame.render_widget(
            StickyHeaderWidget::new(sticky_header.as_ref(), theme),
            header_area,
        );
        if sticky_rows
            .iter()
            .rev()
            .find(|marker| marker.row <= scroll)
            .or_else(|| sticky_rows.first())
            .and_then(|marker| blocks.iter().find(|block| block.rows.contains(&marker.row)))
            .is_some_and(|block| self.selected_blocks.contains(&block.source.id))
        {
            frame
                .buffer_mut()
                .set_style(header_area, Style::default().bg(theme.selection));
        }
        frame.render_widget(
            FullLineBackgroundParagraph::prewrapped(text).scroll(scroll),
            body_content_area,
        );

        let (input_areas, hints_area) = viewer_inputs(area);
        let status = self.status.as_ref().filter(|entry| !entry.expired());
        let selection_label = status.map(|entry| entry.label.clone()).unwrap_or_else(|| {
            if self.selected_blocks.is_empty() {
                String::new()
            } else {
                format!("{} selected · Alt+C copy", self.selected_blocks.len())
            }
        });
        let cache = self
            .render_cache
            .as_ref()
            .expect("viewer cache should exist");
        let search_label = {
            let mut label = match_count(self.active_match, cache.query_ranges.len());
            if cache.query_undisplayed > 0 {
                label.push_str(&format!(" · {} undisplayed", cache.query_undisplayed));
            }
            label
        };
        render_viewer_input(
            frame,
            input_areas[0],
            &self.search,
            Line::from("Search"),
            &search_label,
            self.input_focus == ViewerInputFocus::Search,
            theme,
        );
        let find_label = if cache.find_error.is_some() {
            "Invalid regex".to_owned()
        } else {
            match_count(self.active_find, cache.find_ranges.len())
        };
        let (find_title, hits) = find_input_title(
            input_areas[1],
            self.find_regex,
            self.find_case_sensitive,
            &find_label,
        );
        self.find_title_hits = hits;
        render_viewer_input(
            frame,
            input_areas[1],
            &self.find,
            find_title,
            &find_label,
            self.input_focus == ViewerInputFocus::Find,
            theme,
        );
        let notice = cache
            .find_error
            .as_ref()
            .or(cache.query_error.as_ref())
            .map(String::as_str)
            .or_else(|| {
                (!cache.query_match
                    && cache.query_match_is_certain
                    && !self.search.value().is_empty())
                .then_some("Query does not match this session")
            });
        if let Some(notice) = notice {
            frame.render_widget(
                Paragraph::new(notice.lines().last().unwrap_or(notice))
                    .style(Style::default().fg(theme.accent)),
                Rect::new(
                    hints_area.x,
                    hints_area.y,
                    hints_area.width,
                    hints_area.height.min(1),
                ),
            );
            keymap_hint::render(
                frame,
                Rect::new(
                    hints_area.x,
                    hints_area.y + 1,
                    hints_area.width,
                    hints_area.height.saturating_sub(1),
                ),
                &Self::HINTS,
                theme,
                &selection_label,
            );
        } else {
            let mut labels = Vec::new();
            if cache.query_hidden > 0 {
                labels.push(format!("{} hidden", cache.query_hidden));
            }
            if cache.query_metadata > 0 {
                labels.push(format!("{} metadata", cache.query_metadata));
            }
            if cache.query_source_only > 0 {
                labels.push(format!("{} source-only", cache.query_source_only));
            }
            if !selection_label.is_empty() {
                labels.push(selection_label);
            }
            keymap_hint::render(frame, hints_area, &Self::HINTS, theme, &labels.join(" · "));
        }
    }

    pub fn max_scroll(
        &mut self,
        area: Rect,
        session: &Session,
        summary: Option<&SummarySidecar>,
        theme: &Theme,
        theme_name: ThemeName,
        display_options: DisplayOptions,
    ) -> usize {
        let cache = self.render_cache(area, session, summary, theme, theme_name, display_options);
        cache.total_rows.saturating_sub(viewport_height(area))
    }

    /// Capture the old layout before committing display filters. The next layout
    /// consumes this snapshot independently of the block-selection anchor.
    pub(crate) fn capture_filter_scroll(
        &mut self,
        area: Rect,
        session: &Session,
        summary: Option<&SummarySidecar>,
        theme: &Theme,
        theme_name: ThemeName,
        display_options: DisplayOptions,
    ) {
        self.render_cache(area, session, summary, theme, theme_name, display_options);
        let cache = self
            .render_cache
            .as_ref()
            .expect("viewer cache should exist");
        self.filter_scroll_snapshot = Some(FilterScrollSnapshot {
            path: cache.path.clone(),
            top_row: self
                .scroll
                .min(cache.total_rows.saturating_sub(viewport_height(area))),
            blocks: cache
                .blocks
                .iter()
                .map(|block| (block.source.id, block.rows.clone()))
                .collect(),
        });
    }

    pub fn body_area(area: Rect) -> Rect {
        split_viewer(area)[0]
    }

    /// Hit test against the same wrapped rows and clamped scroll used by rendering.
    pub(crate) fn handle_mouse(&mut self, area: Rect, mouse: MouseEvent) {
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            let (inputs, _) = viewer_inputs(area);
            for (index, input) in inputs.iter().enumerate() {
                if input.contains((mouse.column, mouse.row).into()) {
                    self.input_focus = if index == 0 {
                        ViewerInputFocus::Search
                    } else {
                        ViewerInputFocus::Find
                    };
                    if index == 1 && mouse.modifiers.is_empty() {
                        let position = (mouse.column, mouse.row).into();
                        if self.find_title_hits[0].contains(position) {
                            self.toggle_find_regex();
                        } else if self.find_title_hits[1].contains(position) {
                            self.toggle_find_case();
                        }
                    }
                    return;
                }
            }
        }
        // Alt is optional for toggle/range gestures, but Alt-click alone is ignored.
        let modifiers = mouse.modifiers.difference(KeyModifiers::ALT);
        if mouse.kind != MouseEventKind::Down(MouseButton::Left)
            || !(KeyModifiers::CONTROL | KeyModifiers::SHIFT).contains(modifiers)
            || mouse.modifiers == KeyModifiers::ALT
        {
            return;
        }
        let body = Self::body_area(area);
        let inner = Block::default().borders(Borders::ALL).inner(body);
        if !inner.contains((mouse.column, mouse.row).into()) {
            return;
        }
        let Some(cache) = self.render_cache.as_ref() else {
            return;
        };
        let (header, content) = split_sticky_body(inner);
        let scroll = self
            .scroll
            .min(cache.total_rows.saturating_sub(content.height as usize));
        let row = if header.contains((mouse.column, mouse.row).into()) {
            // A heading within a message still belongs to the full message block.
            cache
                .sticky_rows
                .iter()
                .rev()
                .find(|marker| marker.row <= scroll)
                .or_else(|| cache.sticky_rows.first())
                .map(|marker| marker.row)
                .unwrap_or(scroll)
        } else {
            scroll + mouse.row.saturating_sub(content.y) as usize
        };
        let index = cache
            .blocks
            .iter()
            .position(|block| block.rows.contains(&row));
        self.select_block(index, modifiers);
    }

    fn select_block(&mut self, index: Option<usize>, modifiers: KeyModifiers) {
        let control = modifiers.contains(KeyModifiers::CONTROL);
        let shift = modifiers.contains(KeyModifiers::SHIFT);
        let Some(cache) = self.render_cache.as_ref() else {
            return;
        };
        let Some(index) = index else {
            if !control && !shift {
                self.selected_blocks.clear();
                self.selection_anchor = None;
                self.status = None;
            }
            return;
        };
        let id = cache.blocks[index].source.id;
        self.status = None;
        if shift {
            let anchor = self
                .selection_anchor
                .and_then(|id| cache.blocks.iter().position(|block| block.source.id == id))
                .unwrap_or(index);
            if !control {
                self.selected_blocks.clear();
            }
            self.selected_blocks.extend(
                cache.blocks[anchor.min(index)..=anchor.max(index)]
                    .iter()
                    .map(|block| block.source.id),
            );
            self.selection_anchor.get_or_insert(id);
        } else {
            if control {
                if !self.selected_blocks.remove(&id) {
                    self.selected_blocks.insert(id);
                }
            } else {
                self.selected_blocks.clear();
                self.selected_blocks.insert(id);
            }
            self.selection_anchor = Some(id);
        }
    }

    pub(crate) fn copy_selected(
        &mut self,
        session: &Session,
        summary: Option<&SummarySidecar>,
        options: DisplayOptions,
        write: impl FnOnce(&str) -> anyhow::Result<()>,
    ) {
        let (count, markdown) = self.selected_markdown(session, summary, options);
        self.status = Some(if count == 0 {
            statusline::Entry::failed("No blocks selected")
        } else {
            match write(&markdown) {
                Ok(()) => statusline::Entry::completed(format!(
                    "Copied {count} block{}",
                    if count == 1 { "" } else { "s" }
                )),
                Err(error) => statusline::Entry::failed(format!("Clipboard error: {error:#}")),
            }
        });
    }

    pub(crate) fn selected_markdown(
        &self,
        session: &Session,
        summary: Option<&SummarySidecar>,
        options: DisplayOptions,
    ) -> (usize, String) {
        let ids: Vec<_> = self
            .render_cache
            .iter()
            .flat_map(|cache| cache.blocks.iter())
            .map(|block| block.source.id)
            .filter(|id| self.selected_blocks.contains(id))
            .collect();
        (
            ids.len(),
            selected_blocks_to_markdown(session, summary, ids, options),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn jump_to_match(
        &mut self,
        direction: MatchDirection,
        area: Rect,
        session: Option<&Session>,
        summary: Option<&SummarySidecar>,
        theme: &Theme,
        theme_name: ThemeName,
        display_options: DisplayOptions,
    ) {
        let Some(session) = session else {
            match self.input_focus {
                ViewerInputFocus::Search => self.active_match = None,
                ViewerInputFocus::Find => self.active_find = None,
            }
            return;
        };

        let body_area = Self::body_area(area);
        self.render_cache(area, session, summary, theme, theme_name, display_options);
        let cache = self
            .render_cache
            .as_ref()
            .expect("viewer cache should exist");
        let (matches, active) = match self.input_focus {
            ViewerInputFocus::Search => (&cache.match_rows, self.active_match),
            ViewerInputFocus::Find => (&cache.find_rows, self.active_find),
        };
        if matches.is_empty() {
            match self.input_focus {
                ViewerInputFocus::Search => self.active_match = None,
                ViewerInputFocus::Find => self.active_find = None,
            }
            return;
        }

        let next_index = match direction {
            MatchDirection::Next => next_match_index(matches, active, self.scroll),
            MatchDirection::Previous => previous_match_index(matches, active, self.scroll),
        };

        let row = matches[next_index];
        match self.input_focus {
            ViewerInputFocus::Search => self.active_match = Some(next_index),
            ViewerInputFocus::Find => self.active_find = Some(next_index),
        }
        let max_scroll =
            self.max_scroll(area, session, summary, theme, theme_name, display_options);
        let viewport_height = body_area
            .height
            .saturating_sub(2)
            .saturating_sub(STICKY_HEADER_HEIGHT) as usize;
        self.scroll = scroll_for_match(row, viewport_height, max_scroll);
        self.reset_find_anchor();
    }

    #[allow(clippy::too_many_arguments)]
    fn jump_to_message(
        &mut self,
        direction: MessageDirection,
        scope: MessageJumpScope,
        area: Rect,
        session: Option<&Session>,
        summary: Option<&SummarySidecar>,
        theme: &Theme,
        theme_name: ThemeName,
        display_options: DisplayOptions,
    ) {
        let Some(session) = session else {
            return;
        };

        let rows: Vec<_> = self
            .render_cache(area, session, summary, theme, theme_name, display_options)
            .blocks
            .iter()
            .filter_map(|block| {
                navigable_block(session, block.source.id, scope).then_some(block.rows.start)
            })
            .collect();
        let Some(target_row) = message_row_for_scroll(&rows, self.scroll, direction) else {
            return;
        };

        let max_scroll =
            self.max_scroll(area, session, summary, theme, theme_name, display_options);
        self.scroll = target_row.min(max_scroll);
    }

    fn render_cache(
        &mut self,
        area: Rect,
        session: &Session,
        summary: Option<&SummarySidecar>,
        theme: &Theme,
        theme_name: ThemeName,
        display_options: DisplayOptions,
    ) -> &ViewerRenderCache {
        let body_area = Self::body_area(area);
        let width = body_area.width.saturating_sub(2);
        let path = session.file_path.clone();
        if self.selection_path.as_ref() != Some(&path) {
            self.selected_blocks.clear();
            self.selection_anchor = None;
            self.selection_path = Some(path.clone());
        }
        let query = self.search.value().to_owned();
        let summary_stamp = summary.map(summary_stamp);
        let previous_find_signature = self
            .render_cache
            .as_ref()
            .and_then(|cache| cache.find_signature.clone());

        let base_changed = self.render_cache.as_ref().is_none_or(|cache| {
            cache.path != path
                || cache.session != *session
                || cache.theme_name != theme_name
                || cache.display_options != display_options
                || cache.summary_stamp != summary_stamp
        });
        if base_changed {
            profile::event("viewer.cache.miss");
            let document = {
                let _profile = profile::scope("viewer.base.render");
                render_viewer_document(
                    session,
                    summary,
                    theme,
                    None,
                    self.render_options(display_options),
                )
            };
            let text = document.text;
            let layout = {
                let _profile = profile::scope("viewer.layout");
                LayoutDocument::new(&text, width)
            };
            let total_rows = layout.height().max(1);
            let sticky_rows = document
                .sticky_markers
                .iter()
                .map(|marker| StickyRowMarker {
                    row: layout
                        .rows_for_lines(marker.line_index..marker.line_index + 1)
                        .start,
                    header: marker.header.clone(),
                })
                .collect();
            let blocks: Vec<_> = document
                .blocks
                .into_iter()
                .map(|source| ViewerBlock {
                    rows: layout.rows_for_lines(source.lines.clone()),
                    source,
                })
                .collect();
            let visible_ids: BTreeSet<_> = blocks.iter().map(|block| block.source.id).collect();
            self.selected_blocks.retain(|id| visible_ids.contains(id));
            if self
                .selection_anchor
                .is_some_and(|id| !visible_ids.contains(&id))
            {
                self.selection_anchor = None;
            }
            self.render_cache = Some(ViewerRenderCache {
                path,
                query: query.clone(),
                visibility_search: self.visibility_search,
                session: session.clone(),
                projection: SearchProjection::from_session(session),
                map: document.map,
                layout,
                query_ranges: Vec::new(),
                query_match: true,
                query_match_is_certain: true,
                query_undisplayed: 0,
                query_hidden: 0,
                query_metadata: 0,
                query_source_only: 0,
                query_error: None,
                find_signature: previous_find_signature,
                find_ranges: Vec::new(),
                find_rows: Vec::new(),
                find_error: None,
                width,
                theme_name,
                display_options,
                summary_stamp,
                total_rows,
                text,
                match_rows: Vec::new(),
                sticky_rows,
                sticky_markers: document.sticky_markers,
                blocks,
            });
        } else {
            profile::event("viewer.cache.hit");
        }

        let cache = self
            .render_cache
            .as_mut()
            .expect("viewer cache should exist");
        let layout_changed = cache.width != width;
        if layout_changed {
            let _profile = profile::scope("viewer.layout");
            cache.layout = LayoutDocument::new(&cache.text, width);
            cache.width = width;
            cache.total_rows = cache.layout.height().max(1);
            cache.sticky_rows = cache
                .sticky_markers
                .iter()
                .map(|marker| StickyRowMarker {
                    row: cache
                        .layout
                        .rows_for_lines(marker.line_index..marker.line_index + 1)
                        .start,
                    header: marker.header.clone(),
                })
                .collect();
            for block in &mut cache.blocks {
                block.rows = cache.layout.rows_for_lines(block.source.lines.clone());
            }
        }
        let query_changed = base_changed
            || cache.query != query
            || cache.visibility_search != self.visibility_search;
        if query_changed {
            let _profile = profile::scope("viewer.query.match");
            cache.query = query;
            cache.visibility_search = self.visibility_search;
            cache.query_ranges.clear();
            cache.query_error = None;
            cache.query_undisplayed = 0;
            cache.query_hidden = 0;
            cache.query_metadata = 0;
            cache.query_source_only = 0;
            match QueryPlan::compile(&cache.query, self.visibility_search, display_options) {
                Ok(plan) => {
                    let matches = plan.matches(&cache.projection);
                    cache.query_match = matches.is_match;
                    cache.query_match_is_certain = matches.match_is_certain;
                    if !matches.match_is_certain {
                        cache.query_error = Some(
                            "Phrase highlights and match status may differ from indexed results"
                                .to_owned(),
                        );
                    }
                    let projected = cache.map.project_matches(&matches.matches);
                    for (matched, ranges) in matches.matches.into_iter().zip(projected) {
                        if ranges.is_empty() {
                            cache.query_undisplayed += 1;
                            if matched.sources.iter().any(|source| {
                                cache.projection.segments.iter().any(|segment| {
                                    segment.source == source.source
                                        && !segment.visibility.is_visible(display_options)
                                })
                            }) {
                                cache.query_hidden += 1;
                            } else if matched.sources.iter().any(|source| {
                                matches!(
                                    source.source.block,
                                    SourceBlock::Title | SourceBlock::Context
                                )
                            }) {
                                cache.query_metadata += 1;
                            } else {
                                cache.query_source_only += 1;
                            }
                        } else {
                            cache.query_ranges.push(ranges);
                        }
                    }
                    cache.query_ranges.sort_by(|left, right| {
                        left.iter()
                            .map(|range| (range.start, range.end))
                            .cmp(right.iter().map(|range| (range.start, range.end)))
                    });
                    cache.query_ranges.dedup();
                }
                Err(error) => {
                    cache.query_error = Some(error.to_string());
                    cache.query_match = false;
                    cache.query_match_is_certain = true;
                }
            }
        }
        if query_changed || layout_changed {
            cache.match_rows = cache
                .query_ranges
                .iter()
                .filter_map(|ranges| {
                    ranges
                        .first()
                        .map(|range| cache.layout.row_for_offset(range.start))
                })
                .collect();
        }
        let signature = (
            self.find.value().to_owned(),
            self.find_regex,
            self.find_case_sensitive,
        );
        let find_changed = cache.find_signature.as_ref() != Some(&signature);
        if base_changed || find_changed {
            let _profile = profile::scope("viewer.find.match");
            profile::event("viewer.find.match");
            cache.find_signature = Some(signature);
            match find_occurrences(
                &cache.map.plain,
                self.find.value(),
                self.find_regex,
                self.find_case_sensitive,
            ) {
                Ok(ranges) => {
                    cache.find_ranges = ranges;
                    cache.find_error = None;
                }
                Err(error) => {
                    cache.find_ranges.clear();
                    cache.find_error = Some(format!("Invalid regex: {error}"));
                }
            }
        }
        if base_changed || find_changed || layout_changed {
            cache.find_rows = cache
                .find_ranges
                .iter()
                .map(|range| cache.layout.row_for_offset(range.start))
                .collect();
        }
        if self
            .active_match
            .is_some_and(|index| index >= cache.query_ranges.len())
        {
            self.active_match = None;
        }
        if self
            .active_find
            .is_some_and(|index| index >= cache.find_ranges.len())
        {
            self.active_find = None;
        }
        if find_changed {
            self.active_find = None;
            let (_, jump) = find_directives(self.find.value(), self.find_jump);
            if jump && !cache.find_rows.is_empty() {
                let anchor = *self.find_anchor.get_or_insert(self.scroll);
                let index = next_match_index(&cache.find_rows, None, anchor);
                self.active_find = Some(index);
                self.scroll = scroll_for_match(
                    cache.find_rows[index],
                    viewport_height(area),
                    cache.total_rows.saturating_sub(viewport_height(area)),
                );
            }
        }

        let cache = self
            .render_cache
            .as_ref()
            .expect("viewer cache should exist");
        if let Some(snapshot) = self.filter_scroll_snapshot.take() {
            if snapshot.path == cache.path {
                self.scroll = snapshot
                    .restored_scroll(&cache.blocks)
                    .min(cache.total_rows.saturating_sub(viewport_height(area)));
            }
        }
        cache
    }

    fn render_options(&self, display_options: DisplayOptions) -> SessionRenderOptions {
        SessionRenderOptions::new(display_options)
    }
}

fn split_viewer(area: Rect) -> [Rect; 2] {
    let footer_height = VIEWER_FOOTER_HEIGHT
        + if area.width < 80 {
            VIEWER_SEARCH_HEIGHT
        } else {
            0
        };
    let chunks =
        Layout::vertical([Constraint::Min(0), Constraint::Length(footer_height)]).split(area);
    [chunks[0], chunks[1]]
}

fn viewer_inputs(area: Rect) -> ([Rect; 2], Rect) {
    let footer = split_viewer(area)[1];
    let input_height = VIEWER_SEARCH_HEIGHT * if area.width < 80 { 2 } else { 1 };
    let chunks = Layout::vertical([
        Constraint::Length(input_height),
        Constraint::Length(VIEWER_HINTS_HEIGHT),
    ])
    .split(footer);
    let inputs = if area.width < 80 {
        Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).split(chunks[0])
    } else {
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(chunks[0])
    };
    ([inputs[0], inputs[1]], chunks[1])
}

fn match_count(active: Option<usize>, count: usize) -> String {
    format!(
        "{}/{}",
        active
            .filter(|index| *index < count)
            .map_or(0, |index| index + 1),
        count
    )
}

/// Title controls use the same spans as drawing, clipped before the right title
/// that Ratatui renders over them. Save these regions for the displayed frame.
fn find_input_title(
    area: Rect,
    regex: bool,
    case_sensitive: bool,
    counter: &str,
) -> (Line<'static>, [Rect; 2]) {
    let prefix = "Find · ";
    let separator = " · ";
    let mode = if regex { "Regex" } else { "Substring" };
    let case = if case_sensitive {
        "Case"
    } else {
        "Ignore case"
    };
    let clickable = Style::default().add_modifier(Modifier::UNDERLINED);
    let title = Line::from(vec![
        Span::raw(prefix),
        Span::styled(mode, clickable),
        Span::raw(separator),
        Span::styled(case, clickable),
    ]);
    if area.is_empty() {
        return (title, [Rect::ZERO; 2]);
    }
    let left = area.x.saturating_add(1);
    let right = area.right().saturating_sub(1);
    let counter_width = right_block_title(counter.to_owned()).width() as u16;
    let visible_end = right.saturating_sub(counter_width).max(left);
    let mode_start = left.saturating_add(block_title(prefix).width() as u16);
    let case_start = mode_start
        .saturating_add(Span::raw(mode).width() as u16)
        .saturating_add(Span::raw(separator).width() as u16);
    let visible_label = |start: u16, label: &str| {
        let end = start
            .saturating_add(Span::raw(label).width() as u16)
            .min(visible_end);
        Rect::new(start.min(end), area.y, end.saturating_sub(start), 1)
    };
    (
        title,
        [
            visible_label(mode_start, mode),
            visible_label(case_start, case),
        ],
    )
}

#[allow(clippy::too_many_arguments)]
fn render_viewer_input(
    frame: &mut Frame,
    area: Rect,
    input: &Input,
    title: Line<'static>,
    counter: &str,
    focused: bool,
    theme: &Theme,
) {
    let available = area.width.saturating_sub(2) as usize;
    let scroll = input.visual_scroll(available);
    frame.render_widget(
        Paragraph::new(input.value().to_owned())
            .style(Style::default().fg(theme.text))
            .scroll((0, scroll.min(u16::MAX as usize) as u16))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(theme.border_style(focused))
                    .title(block_title(title.style(Style::default().fg(theme.accent))))
                    .title(right_block_title(Span::styled(
                        counter.to_owned(),
                        Style::default().fg(theme.accent),
                    ))),
            ),
        area,
    );
    if focused && area.width >= 3 && area.height >= 3 {
        let cursor_x = area
            .x
            .saturating_add(1 + input.visual_cursor().saturating_sub(scroll) as u16)
            .min(area.right().saturating_sub(2));
        frame.set_cursor_position((cursor_x, area.y.saturating_add(1)));
    }
}

/// A leading backslash quotes a jump directive in Substring mode. Regex mode
/// can use the ordinary `\(\?j\)` escaping to search for a literal directive.
fn find_directives(mut expression: &str, default_jump: bool) -> (&str, bool) {
    let mut jump = default_jump;
    loop {
        if let Some(quoted) = expression.strip_prefix('\\') {
            if quoted.starts_with("(?j)") || quoted.starts_with("(?-j)") {
                return (quoted, jump);
            }
        }
        if let Some(rest) = expression.strip_prefix("(?j)") {
            expression = rest;
            jump = true;
        } else if let Some(rest) = expression.strip_prefix("(?-j)") {
            expression = rest;
            jump = false;
        } else {
            return (expression, jump);
        }
    }
}

fn find_occurrences(
    document: &str,
    expression: &str,
    regex_mode: bool,
    case_sensitive: bool,
) -> Result<Vec<Range<usize>>, regex::Error> {
    let (pattern, _) = find_directives(expression, true);
    if pattern.is_empty() {
        return Ok(Vec::new());
    }
    let pattern = if regex_mode {
        pattern.to_owned()
    } else {
        regex::escape(pattern)
    };
    let regex = regex::RegexBuilder::new(&pattern)
        .case_insensitive(!case_sensitive)
        .build()?;
    Ok(regex
        .find_iter(document)
        .map(|matched| matched.range())
        .collect())
}

fn viewport_height(area: Rect) -> usize {
    ViewerState::body_area(area)
        .height
        .saturating_sub(2)
        .saturating_sub(STICKY_HEADER_HEIGHT) as usize
}

fn render_viewer_document(
    session: &Session,
    summary: Option<&SummarySidecar>,
    theme: &Theme,
    highlight_query: Option<&str>,
    options: SessionRenderOptions,
) -> DisplayDocument {
    let mut lines = Vec::new();
    let mut sticky_markers = Vec::new();
    let mut blocks = Vec::new();
    if let Some(summary) = summary {
        let summary_start = lines.len();
        lines.extend(render_summary_leadin(summary, theme, highlight_query).lines);
        blocks.push(DisplayBlock {
            id: SessionBlockId::Summary,
            lines: summary_start..lines.len(),
        });
        sticky_markers.push(crate::tui::util::StickyLineMarker {
            line_index: summary_start,
            header: crate::tui::util::StickyHeader::new(
                "AICS summary",
                summary.generated_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                "Summary",
            ),
        });
        lines.push(Line::default());
        lines.push(Line::default());
    }
    let session_start = lines.len();
    let session_doc =
        render_session_document_with_options(session, theme, highlight_query, options);
    blocks.extend(session_doc.blocks.into_iter().map(|mut block| {
        block.lines = block.lines.start + session_start..block.lines.end + session_start;
        block
    }));
    lines.extend(session_doc.text.lines);
    sticky_markers.extend(session_doc.sticky_markers.into_iter().map(|marker| {
        crate::tui::util::StickyLineMarker {
            line_index: marker.line_index + session_start,
            header: marker.header,
        }
    }));
    let text = Text::from(lines);
    let mut map = DocumentMap::from_text(&text);
    let session_offset = map.line_starts.get(session_start).copied().unwrap_or(0);
    map.extend_origins(&session_doc.map, session_offset);
    DisplayDocument {
        text,
        map,
        sticky_markers,
        blocks,
    }
}

fn render_summary_leadin(
    summary: &SummarySidecar,
    theme: &Theme,
    highlight_query: Option<&str>,
) -> Text<'static> {
    render_markdown_message(
        &format!("# Summary\n\n{}", summary.body),
        theme,
        Style::default().fg(theme.text),
        highlight_query,
    )
}

fn summary_stamp(summary: &SummarySidecar) -> (i64, usize, u64) {
    let mut hasher = DefaultHasher::new();
    summary.body.hash(&mut hasher);
    (
        summary.generated_at.timestamp(),
        summary.line_count,
        hasher.finish(),
    )
}

fn viewer_title(
    session: &Session,
    trashed: bool,
    theme: &Theme,
    scroll_percent: usize,
) -> Line<'static> {
    let (badge, badge_color) = agent_badge(session.agent, theme);
    let mut title = abbreviate_home_path(&session_display_title(session.agent, &session.project));
    if trashed {
        title = format!("trashed · {title}");
    }
    let time = relative_time(session.modified_ts);
    let line_count = format_line_count(session.lines);

    Line::from(vec![
        Span::styled(
            format!("{{{badge}}}"),
            Style::default()
                .fg(badge_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · ", Style::default().fg(theme.muted)),
        Span::styled(title, Style::default().fg(theme.text)),
        Span::styled(" · ", Style::default().fg(theme.muted)),
        Span::styled(time, Style::default().fg(theme.muted)),
        Span::styled(" · ", Style::default().fg(theme.muted)),
        Span::styled(line_count, Style::default().fg(theme.muted)),
        Span::styled(" · ", Style::default().fg(theme.muted)),
        Span::styled(
            format!("{scroll_percent}% scrolled"),
            Style::default().fg(theme.accent),
        ),
    ])
}

fn scroll_progress_percent(scroll: usize, viewport_height: usize, total_rows: usize) -> usize {
    if total_rows == 0 {
        return 100;
    }

    let farthest_displayed_row = scroll.saturating_add(viewport_height).min(total_rows);
    farthest_displayed_row.saturating_mul(100) / total_rows
}

fn match_scrolloff(viewport_height: usize) -> usize {
    VIEWER_MATCH_SCROLLOFF.min(viewport_height / 3)
}

pub(crate) fn scroll_for_match(
    match_row: usize,
    viewport_height: usize,
    max_scroll: usize,
) -> usize {
    match_row
        .saturating_sub(match_scrolloff(viewport_height))
        .min(max_scroll)
}

#[cfg(test)]
pub(crate) fn collect_message_rows(
    session: &Session,
    summary: Option<&SummarySidecar>,
    theme: &Theme,
    width: u16,
    scope: MessageJumpScope,
    display_options: DisplayOptions,
) -> Vec<usize> {
    let options = SessionRenderOptions::new(display_options);
    collect_message_rows_with_options(session, summary, theme, width, scope, options)
}

#[cfg(test)]
pub(crate) fn collect_message_rows_with_options(
    session: &Session,
    summary: Option<&SummarySidecar>,
    theme: &Theme,
    width: u16,
    scope: MessageJumpScope,
    options: SessionRenderOptions,
) -> Vec<usize> {
    if width == 0 {
        return Vec::new();
    }

    let document = render_viewer_document(session, summary, theme, None, options);
    let layout = LayoutDocument::new(&document.text, width);
    document
        .blocks
        .iter()
        .filter(|block| navigable_block(session, block.id, scope))
        .map(|block| layout.rows_for_lines(block.lines.clone()).start)
        .collect()
}

fn navigable_block(session: &Session, id: SessionBlockId, scope: MessageJumpScope) -> bool {
    let is_user = match id {
        SessionBlockId::Message(index) => session
            .messages
            .get(index)
            .map(|message| message.role == MessageRole::User),
        SessionBlockId::Cell(index) => match session.cells.get(index) {
            Some(SessionCell::SessionInfo(_) | SessionCell::Metrics(_)) | None => None,
            Some(SessionCell::Message { role, .. }) => Some(*role == MessageRole::User),
            Some(_) => Some(false),
        },
        _ => None,
    };
    is_user.is_some_and(|is_user| scope == MessageJumpScope::Any || is_user)
}

#[cfg(test)]
fn message_header_text(message: &crate::parse::SessionMessage) -> String {
    let label = crate::tui::util::session_message_label(message);
    let timestamp = message
        .timestamp
        .map(|timestamp| timestamp.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default();
    if timestamp.is_empty() {
        label
    } else {
        format!("{label} {timestamp}")
    }
}

pub(crate) fn next_match_index(
    matches: &[usize],
    active_match: Option<usize>,
    scroll: usize,
) -> usize {
    if let Some(index) = active_match.filter(|index| *index < matches.len()) {
        return (index + 1) % matches.len();
    }

    matches.iter().position(|row| *row >= scroll).unwrap_or(0)
}

pub(crate) fn previous_match_index(
    matches: &[usize],
    active_match: Option<usize>,
    scroll: usize,
) -> usize {
    if let Some(index) = active_match.filter(|index| *index < matches.len()) {
        return if index == 0 {
            matches.len() - 1
        } else {
            index - 1
        };
    }

    matches
        .iter()
        .rposition(|row| *row < scroll)
        .unwrap_or(matches.len() - 1)
}

pub(crate) fn message_row_for_scroll(
    rows: &[usize],
    scroll: usize,
    direction: MessageDirection,
) -> Option<usize> {
    match direction {
        MessageDirection::Next => rows
            .iter()
            .copied()
            .find(|row| *row > scroll)
            .or_else(|| rows.first().copied()),
        MessageDirection::Previous => rows.iter().copied().rfind(|row| *row < scroll),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::Utc;
    use ratatui::crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::layout::Rect;
    use ratatui::style::Modifier;
    use ratatui::text::{Line, Text};
    use tui_input::Input;

    use crate::export::SessionBlockId;
    use crate::parse::{Agent, DerivationType, MessageRole, Session, SessionCell, SessionMessage};
    use crate::settings::{DisplayOptions, ThemeName};
    use crate::summary::{Fingerprint, SummarizeBackend, SummarySidecar};
    use crate::tui::preview::render_message_body;
    use crate::tui::preview::SessionRenderOptions;
    use crate::tui::theme::Theme;
    use crate::tui::util::wrapped_text_height;

    use super::{
        collect_message_rows, collect_message_rows_with_options, match_scrolloff,
        message_header_text, message_row_for_scroll, next_match_index, previous_match_index,
        scroll_for_match, scroll_progress_percent, viewer_title, MessageDirection,
        MessageJumpScope, ViewerOutcome, ViewerState,
    };

    #[test]
    #[allow(clippy::single_range_in_vec_init)] // Expected byte ranges, not integer collections.
    fn find_patterns_support_unicode_literals_regex_directives_and_zero_width() {
        assert_eq!(
            super::find_occurrences("K k K", "k", false, false)
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            super::find_occurrences("K k K", "k", false, true).unwrap(),
            [4..5]
        );
        assert_eq!(
            super::find_occurrences("a.*b aZZb", "a.*b", false, false).unwrap(),
            [0..4]
        );
        assert_eq!(
            super::find_occurrences("foo\nbar", "foo\\nbar", true, false).unwrap(),
            [0..7]
        );
        assert_eq!(
            super::find_occurrences("x\nx", "(?m)^", true, false).unwrap(),
            [0..0, 2..2]
        );
        assert!(super::find_occurrences("x", "[", true, false).is_err());
        assert!(super::find_occurrences("x", "(?-j)", false, false)
            .unwrap()
            .is_empty());
        assert_eq!(
            super::find_directives("(?j)(?-j)(?j)needle", false),
            ("needle", true)
        );
        assert_eq!(
            super::find_directives("(?j)(?-j)needle", true),
            ("needle", false)
        );
        assert_eq!(
            super::find_occurrences("(?j)", "\\(?j)", false, false).unwrap(),
            [0..4]
        );
        assert_eq!(super::find_directives("(?-j)\\(?j)", true), ("(?j)", false));
        assert_eq!(
            super::find_occurrences("(?j)", "\\(\\?j\\)", true, false).unwrap(),
            [0..4]
        );
    }

    #[test]
    fn viewer_find_is_initially_focused_and_layout_stacks_below_eighty_columns() {
        let viewer = ViewerState::with_search("alpha");
        assert_eq!(viewer.search_query(), "alpha");
        assert_eq!(viewer.find_query(), "");
        assert!(!viewer.search_is_focused());
        let (wide, _) = super::viewer_inputs(Rect::new(0, 0, 80, 30));
        assert_eq!(wide[0].y, wide[1].y);
        let (narrow, _) = super::viewer_inputs(Rect::new(0, 0, 79, 30));
        assert_eq!(narrow[0].x, narrow[1].x);
        assert_eq!(narrow[0].bottom(), narrow[1].y);
    }

    #[test]
    fn viewer_fields_and_find_render_in_tiny_viewports() {
        use ratatui::{backend::TestBackend, Terminal};
        let session = sample_session();
        for (width, height) in [(1, 1), (2, 3), (20, 5), (79, 8), (80, 5)] {
            let mut viewer = ViewerState::with_search("alpha");
            viewer.find = Input::default().with_value("alpha".to_owned());
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    viewer.render(
                        frame,
                        frame.area(),
                        &session,
                        false,
                        None,
                        &Theme::default(),
                        ThemeName::default(),
                        DisplayOptions::SHOW_ALL,
                    )
                })
                .unwrap();
        }
    }

    #[test]
    fn find_navigation_preserves_individual_same_row_occurrences_and_base_cache() {
        let session = sample_session();
        let area = Rect::new(0, 0, 100, 30);
        let mut viewer = selection_viewer(&session, area);
        viewer.find_jump = false;
        let base_lines = viewer.render_cache.as_ref().unwrap().text.lines.as_ptr();
        let layout_rows = viewer.render_cache.as_ref().unwrap().layout.rows.as_ptr();
        viewer.find = Input::default().with_value("alpha".to_owned());
        refresh_viewer(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        let cache = viewer.render_cache.as_ref().unwrap();
        assert_eq!(cache.find_ranges.len(), 3);
        assert_eq!(cache.find_rows[0], cache.find_rows[1]);
        assert_eq!(cache.text.lines.as_ptr(), base_lines);
        assert_eq!(cache.layout.rows.as_ptr(), layout_rows);
        assert_eq!(viewer.active_find, None);
        for expected in [Some(0), Some(1), Some(2), Some(0)] {
            viewer.handle_key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                area,
                Some(&session),
                None,
                &Theme::default(),
                ThemeName::default(),
                DisplayOptions::SHOW_ALL,
            );
            assert_eq!(viewer.active_find, expected);
        }
        viewer.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            area,
            Some(&session),
            None,
            &Theme::default(),
            ThemeName::default(),
            DisplayOptions::SHOW_ALL,
        );
        assert_eq!(viewer.active_find, Some(2));
        viewer.find_regex = true;
        viewer.find = Input::default().with_value("[".to_owned());
        refresh_viewer(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        assert!(viewer.render_cache.as_ref().unwrap().find_ranges.is_empty());
        assert!(viewer.render_cache.as_ref().unwrap().find_error.is_some());
        assert_eq!(viewer.active_find, None);
    }

    #[test]
    fn find_jump_directives_override_preference_and_navigation_remains_available() {
        let session = multi_turn_session();
        let area = Rect::new(0, 0, 80, 12);
        let mut viewer = selection_viewer(&session, area);
        viewer.scroll = 3;
        viewer.find = Input::default().with_value("(?-j)second".to_owned());
        refresh_viewer(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        assert_eq!(viewer.scroll, 3);
        assert_eq!(viewer.active_find, None);
        viewer.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            area,
            Some(&session),
            None,
            &Theme::default(),
            ThemeName::default(),
            DisplayOptions::SHOW_ALL,
        );
        assert_eq!(viewer.active_find, Some(0));
        viewer.find_jump = false;
        viewer.find = Input::default().with_value("(?j)first".to_owned());
        refresh_viewer(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        assert!(viewer.active_find.is_some());
    }

    #[test]
    fn incremental_find_keeps_original_anchor_until_explicit_scrolling() {
        let mut session = sample_session();
        let paragraphs = (0..30)
            .map(|index| match index {
                5 => "needy".to_owned(),
                6 => "needle".to_owned(),
                _ => format!("filler {index}"),
            })
            .collect::<Vec<_>>();
        session.messages.truncate(1);
        session.messages[0].content = paragraphs.join("\n\n");
        let area = Rect::new(0, 0, 100, 18);
        let mut viewer = selection_viewer(&session, area);
        viewer.find_jump = false;
        viewer.find = Input::default().with_value("nee".to_owned());
        refresh_viewer(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        let target_row = viewer.render_cache.as_ref().unwrap().find_rows[1];
        viewer.scroll = target_row - 1;
        viewer.find_jump = true;
        viewer.find = Input::default().with_value("needle".to_owned());
        refresh_viewer(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        assert_eq!(viewer.active_find, Some(0));
        viewer.find = Input::default().with_value("nee".to_owned());
        refresh_viewer(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        assert_eq!(viewer.active_find, Some(1));
        viewer.handle_key(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            area,
            Some(&session),
            None,
            &Theme::default(),
            ThemeName::default(),
            DisplayOptions::SHOW_ALL,
        );
        viewer.find = Input::default().with_value("ne".to_owned());
        refresh_viewer(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        assert_eq!(viewer.active_find, Some(0));
    }

    #[test]
    fn viewer_mouse_and_keyboard_switch_focus_without_editing_other_box() {
        let session = sample_session();
        let area = Rect::new(0, 0, 100, 30);
        let mut viewer = selection_viewer(&session, area);
        viewer.handle_key(
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
            area,
            Some(&session),
            None,
            &Theme::default(),
            ThemeName::default(),
            DisplayOptions::SHOW_ALL,
        );
        assert_eq!(viewer.find_query(), "z");
        assert_eq!(viewer.search_query(), "");
        let (inputs, _) = super::viewer_inputs(area);
        viewer.handle_mouse(
            area,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: inputs[0].x + 1,
                row: inputs[0].y + 1,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(viewer.search_is_focused());
        viewer.handle_key(
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
            area,
            Some(&session),
            None,
            &Theme::default(),
            ThemeName::default(),
            DisplayOptions::SHOW_ALL,
        );
        assert!(!viewer.search_is_focused());
        viewer.handle_key(
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::ALT),
            area,
            Some(&session),
            None,
            &Theme::default(),
            ThemeName::default(),
            DisplayOptions::SHOW_ALL,
        );
        assert!(viewer.find_regex);
        viewer.handle_key(
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::ALT),
            area,
            Some(&session),
            None,
            &Theme::default(),
            ThemeName::default(),
            DisplayOptions::SHOW_ALL,
        );
        assert!(viewer.find_case_sensitive);
    }

    #[test]
    fn viewer_query_occurrences_keep_positive_matches_when_expression_fails() {
        let session = sample_session();
        let area = Rect::new(0, 0, 100, 30);
        let mut viewer = ViewerState::with_search("alpha absentword");
        refresh_viewer(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        let cache = viewer.render_cache.as_ref().unwrap();
        assert!(!cache.query_match);
        assert_eq!(cache.query_ranges.len(), 3);
        assert_eq!(cache.match_rows[0], cache.match_rows[1]);
        viewer.search = Input::default().with_value("all: alpha".to_owned());
        refresh_viewer(
            &mut viewer,
            &session,
            area,
            DisplayOptions {
                hide_user_messages: true,
                ..DisplayOptions::SHOW_ALL
            },
        );
        let cache = viewer.render_cache.as_ref().unwrap();
        assert_eq!(cache.query_ranges.len(), 1);
        assert_eq!(cache.query_hidden, 2);
        viewer.search = Input::default().with_value("demo".to_owned());
        refresh_viewer(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        assert_eq!(viewer.render_cache.as_ref().unwrap().query_metadata, 1);
    }

    #[test]
    fn query_source_only_match_has_no_display_navigation_location() {
        let mut session = sample_session();
        session.messages[0].content =
            "[label](https://example.invalid/unrenderedneedle)".to_owned();
        session.content = session.messages[0].content.clone();
        let area = Rect::new(0, 0, 100, 30);
        let mut viewer = ViewerState::with_search("unrenderedneedle");
        refresh_viewer(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        let cache = viewer.render_cache.as_ref().unwrap();
        assert!(cache.query_match);
        assert!(cache.query_ranges.is_empty());
        assert_eq!(cache.query_source_only, 1);
    }

    #[test]
    fn corpus_dependent_phrase_keeps_occurrences_and_shows_provisional_status() {
        let session = sample_session();
        let area = Rect::new(0, 0, 120, 30);
        let mut viewer = ViewerState::with_search("\"alpha beta gamma\"~2");
        refresh_viewer(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        let cache = viewer.render_cache.as_ref().unwrap();
        assert!(!cache.query_match_is_certain);
        assert!(!cache.query_ranges.is_empty());
        assert!(cache
            .query_error
            .as_deref()
            .is_some_and(|diagnostic| diagnostic.contains("may differ from indexed results")));

        viewer.search = Input::default().with_value("alpha absentword".into());
        refresh_viewer(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        let cache = viewer.render_cache.as_ref().unwrap();
        assert!(cache.query_match_is_certain);
        assert!(!cache.query_match);
        assert!(cache.query_error.is_none());
        assert_eq!(cache.query_ranges.len(), 3);
    }

    #[test]
    fn query_navigation_deduplicates_component_aliases_but_keeps_json_occurrences() {
        let mut session = sample_session();
        session.messages.clear();
        session.cells = vec![SessionCell::ToolCall {
            tool: "alpha".into(),
            raw_name: "alpha".into(),
            summary: String::new(),
            input: serde_json::json!({"first": "alpha alpha", "second": "alpha"}),
            status: crate::parse::ToolStatus::Completed,
            timestamp: None,
        }];
        let mut viewer = ViewerState::with_search("alpha");
        refresh_viewer(
            &mut viewer,
            &session,
            Rect::new(0, 0, 100, 30),
            DisplayOptions::SHOW_ALL,
        );
        let cache = viewer.render_cache.as_ref().unwrap();
        assert_eq!(cache.query_ranges.len(), 4);
        assert_eq!(cache.match_rows[1], cache.match_rows[2]);
        assert_ne!(cache.query_ranges[1], cache.query_ranges[2]);
        assert!(cache.query_ranges.iter().all(|ranges| {
            ranges
                .iter()
                .all(|range| &cache.map.plain[range.clone()] == "alpha")
        }));
    }

    #[test]
    fn find_preserves_occurrence_identity_during_resize_and_search_highlights_are_independent() {
        let session = sample_session();
        let mut viewer = ViewerState::with_search("alpha");
        let area = Rect::new(0, 0, 100, 30);
        viewer.find_jump = false;
        viewer.find = Input::default().with_value("alpha".to_owned());
        refresh_viewer(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        viewer.active_find = Some(1);
        let range = viewer.render_cache.as_ref().unwrap().find_ranges[1].clone();
        let base_lines = viewer.render_cache.as_ref().unwrap().text.lines.as_ptr();
        refresh_viewer(
            &mut viewer,
            &session,
            Rect::new(0, 0, 20, 30),
            DisplayOptions::SHOW_ALL,
        );
        let cache = viewer.render_cache.as_ref().unwrap();
        assert_eq!(cache.find_ranges[1], range);
        assert_eq!(cache.query_ranges.len(), 3);
        assert_eq!(viewer.active_find, Some(1));
        assert_eq!(cache.text.lines.as_ptr(), base_lines);
    }

    #[test]
    fn overlapping_search_and_find_promote_only_the_focused_active_occurrence() {
        use ratatui::{backend::TestBackend, Terminal};
        let session = sample_session();
        let theme = Theme::default();
        let area = Rect::new(0, 0, 100, 30);
        let mut viewer = ViewerState::with_search("alpha");
        viewer.find_jump = false;
        viewer.find = Input::default().with_value("alpha".to_owned());
        refresh_viewer(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        viewer.active_find = Some(1);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                viewer.render(
                    frame,
                    area,
                    &session,
                    false,
                    None,
                    &theme,
                    ThemeName::default(),
                    DisplayOptions::SHOW_ALL,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(
            buffer
                .content
                .iter()
                .filter(|cell| cell.bg == theme.active_match_bg)
                .count(),
            5
        );
        assert_eq!(
            buffer
                .content
                .iter()
                .filter(|cell| cell.bg == theme.search_match_bg)
                .count(),
            10
        );
        assert!(buffer
            .content
            .iter()
            .filter(|cell| cell.bg == theme.search_match_bg)
            .all(|cell| cell.modifier.contains(Modifier::UNDERLINED)));
    }

    fn selection_viewer(session: &Session, area: Rect) -> ViewerState {
        let mut viewer = ViewerState::new();
        viewer.render_cache(
            area,
            session,
            None,
            &Theme::default(),
            ThemeName::default(),
            DisplayOptions::SHOW_ALL,
        );
        viewer
    }

    fn selected_indices(viewer: &ViewerState) -> Vec<usize> {
        viewer
            .render_cache
            .as_ref()
            .unwrap()
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(i, block)| {
                viewer
                    .selected_blocks
                    .contains(&block.source.id)
                    .then_some(i)
            })
            .collect()
    }

    fn refresh_viewer(
        viewer: &mut ViewerState,
        session: &Session,
        area: Rect,
        options: DisplayOptions,
    ) {
        viewer.render_cache(
            area,
            session,
            None,
            &Theme::default(),
            ThemeName::default(),
            options,
        );
    }

    fn capture_scroll(
        viewer: &mut ViewerState,
        session: &Session,
        area: Rect,
        options: DisplayOptions,
    ) {
        viewer.capture_filter_scroll(
            area,
            session,
            None,
            &Theme::default(),
            ThemeName::default(),
            options,
        );
    }

    fn block_rows(viewer: &ViewerState, id: SessionBlockId) -> std::ops::Range<usize> {
        viewer
            .render_cache
            .as_ref()
            .unwrap()
            .blocks
            .iter()
            .find(|block| block.source.id == id)
            .unwrap()
            .rows
            .clone()
    }

    #[test]
    fn filter_scroll_uses_old_ranges_and_prefers_following_block_on_ties() {
        let mut snapshot = super::FilterScrollSnapshot {
            path: PathBuf::from("session.jsonl"),
            top_row: 0,
            blocks: vec![
                (SessionBlockId::Message(0), 0..11),
                (SessionBlockId::Message(1), 12..29),
                (SessionBlockId::Message(2), 30..41),
            ],
        };
        let new_block = |index, rows| super::ViewerBlock {
            source: crate::tui::preview::DisplayBlock {
                id: SessionBlockId::Message(index),
                lines: 0..1,
            },
            rows,
        };
        // The middle block is hidden; a newly revealed block has no old position.
        let blocks = [
            new_block(9, 0..3),
            new_block(0, 4..15),
            new_block(2, 16..27),
        ];
        for (top, expected) in [(5, 4), (11, 4), (18, 4), (20, 16), (25, 16), (29, 16)] {
            snapshot.top_row = top;
            assert_eq!(snapshot.restored_scroll(&blocks), expected, "old top {top}");
        }
        // A surviving block containing the top wins even when its heading is far away.
        snapshot.top_row = 28;
        assert_eq!(
            snapshot.restored_scroll(&[new_block(1, 0..17), new_block(2, 18..29)]),
            0
        );
        assert_eq!(snapshot.restored_scroll(&[new_block(9, 0..3)]), 0);
        assert_eq!(snapshot.restored_scroll(&[]), 0);
    }

    #[test]
    fn filter_scroll_tracks_wrapped_block_when_earlier_content_is_hidden_or_revealed() {
        let mut session = multi_turn_session();
        session.messages[4].content = "Reading 界 👩‍💻 e\u{301} location. ".repeat(80);
        let area = Rect::new(0, 0, 32, 16);
        let mut viewer = selection_viewer(&session, area);
        let target = SessionBlockId::Message(4);
        let original = block_rows(&viewer, target).start;
        viewer.scroll = original + 6;
        viewer.select_block(Some(0), KeyModifiers::NONE);
        capture_scroll(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        let filtered = DisplayOptions {
            hide_agent_replies: true,
            ..DisplayOptions::SHOW_ALL
        };
        refresh_viewer(&mut viewer, &session, area, filtered);
        let restored = block_rows(&viewer, target).start;
        assert!(restored < original);
        assert_eq!(viewer.scroll, restored);
        assert_eq!(viewer.selection_anchor, Some(SessionBlockId::Message(0)));
        assert!(viewer.filter_scroll_snapshot.is_none());

        // Hit testing immediately uses the restored layout and viewport.
        viewer.handle_mouse(
            area,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 2,
                row: 1 + super::STICKY_HEADER_HEIGHT,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(viewer.selected_blocks, [target].into());
        viewer.scroll += 2;
        refresh_viewer(&mut viewer, &session, area, filtered);
        assert_eq!(viewer.scroll, restored + 2);

        capture_scroll(&mut viewer, &session, area, filtered);
        refresh_viewer(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        assert_eq!(viewer.scroll, original);
        viewer.scroll += 3;
        viewer.handle_key(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            area,
            Some(&session),
            None,
            &Theme::default(),
            ThemeName::default(),
            DisplayOptions::SHOW_ALL,
        );
        refresh_viewer(
            &mut viewer,
            &session,
            Rect::new(0, 0, 48, 16),
            DisplayOptions::SHOW_ALL,
        );
        assert_eq!(
            viewer.scroll,
            original + 3,
            "search and resize must not restore again"
        );
    }

    #[test]
    fn filter_scroll_clamps_bottom_and_resets_when_no_old_blocks_survive() {
        let mut session = multi_turn_session();
        session.messages[4].content = "long user message\n\n".repeat(30);
        session.messages[5].content = "long reply\n\n".repeat(30);
        let area = Rect::new(0, 0, 60, 20);
        let mut viewer = selection_viewer(&session, area);
        viewer.scroll = usize::MAX;
        capture_scroll(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        let snapshot = viewer.filter_scroll_snapshot.as_ref().unwrap();
        let cache = viewer.render_cache.as_ref().unwrap();
        assert_eq!(
            snapshot.top_row,
            cache.total_rows - super::viewport_height(area)
        );
        let users_only = DisplayOptions {
            hide_agent_replies: true,
            hide_tool_calls: true,
            hide_tool_results: true,
            ..DisplayOptions::SHOW_ALL
        };
        refresh_viewer(&mut viewer, &session, area, users_only);
        assert_eq!(
            viewer.scroll,
            block_rows(&viewer, SessionBlockId::Message(4)).start
        );

        // Replacing all previously visible blocks starts at the new document's beginning.
        capture_scroll(&mut viewer, &session, area, users_only);
        let replies_only = DisplayOptions {
            hide_user_messages: true,
            hide_agent_replies: false,
            ..users_only
        };
        refresh_viewer(&mut viewer, &session, area, replies_only);
        assert_eq!(viewer.scroll, 0);
        capture_scroll(&mut viewer, &session, area, replies_only);
        refresh_viewer(
            &mut viewer,
            &session,
            area,
            DisplayOptions {
                hide_agent_replies: true,
                ..replies_only
            },
        );
        assert_eq!(viewer.scroll, 0);
        assert!(viewer.render_cache.as_ref().unwrap().blocks.is_empty());

        // A short final survivor cannot be aligned above the normal bottom limit.
        session.messages[5].content = "short final reply".into();
        let mut viewer = selection_viewer(&session, area);
        viewer.scroll = usize::MAX;
        capture_scroll(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        let filtered = DisplayOptions {
            hide_user_messages: true,
            ..DisplayOptions::SHOW_ALL
        };
        refresh_viewer(&mut viewer, &session, area, filtered);
        let cache = viewer.render_cache.as_ref().unwrap();
        assert_eq!(
            viewer.scroll,
            cache
                .total_rows
                .saturating_sub(super::viewport_height(area))
        );
    }

    #[test]
    fn filter_scroll_returns_to_exec_start_when_output_shrinks() {
        let mut session = sample_session();
        session.cells = vec![
            SessionCell::Exec {
                command: vec!["echo".into(), "value".into()],
                cwd: None,
                parsed_summary: None,
                stdout: "a line of command output\n".repeat(40),
                stderr: String::new(),
                exit_code: Some(0),
                duration_ms: None,
                status: crate::parse::ExecStatus::Completed,
                timestamp: None,
                is_user: false,
            },
            SessionCell::Message {
                role: MessageRole::User,
                content: "following message\n\n".repeat(30),
                timestamp: None,
            },
        ];
        let area = Rect::new(0, 0, 50, 20);
        let mut viewer = selection_viewer(&session, area);
        let old_range = block_rows(&viewer, SessionBlockId::Cell(0));
        viewer.scroll = old_range.start + 20;
        capture_scroll(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        refresh_viewer(
            &mut viewer,
            &session,
            area,
            DisplayOptions {
                hide_tool_results: true,
                ..DisplayOptions::SHOW_ALL
            },
        );
        let new_range = block_rows(&viewer, SessionBlockId::Cell(0));
        assert!(new_range.len() < old_range.len());
        assert_eq!(viewer.scroll, new_range.start);
    }

    #[test]
    fn filter_scroll_snapshot_is_discarded_when_session_changes() {
        let session = multi_turn_session();
        let area = Rect::new(0, 0, 50, 16);
        let mut viewer = selection_viewer(&session, area);
        viewer.scroll = 8;
        capture_scroll(&mut viewer, &session, area, DisplayOptions::SHOW_ALL);
        let mut other = session.clone();
        other.file_path = PathBuf::from("another-session.jsonl");
        viewer.scroll = 3;
        refresh_viewer(&mut viewer, &other, area, DisplayOptions::SHOW_ALL);
        assert_eq!(viewer.scroll, 3);
        assert!(viewer.filter_scroll_snapshot.is_none());
    }

    #[test]
    fn copy_empty_selection_skips_clipboard_and_failure_keeps_selection() {
        let session = multi_turn_session();
        let mut viewer = selection_viewer(&session, Rect::new(0, 0, 80, 30));
        viewer.copy_selected(&session, None, DisplayOptions::SHOW_ALL, |_| {
            panic!("empty selection must not write")
        });
        assert_eq!(viewer.status.as_ref().unwrap().label, "No blocks selected");
        viewer.select_block(Some(0), KeyModifiers::NONE);
        viewer.copy_selected(&session, None, DisplayOptions::SHOW_ALL, |_| {
            Err(anyhow::anyhow!("unavailable"))
        });
        assert_eq!(selected_indices(&viewer), [0]);
        assert!(viewer
            .status
            .as_ref()
            .unwrap()
            .label
            .contains("unavailable"));
        viewer.copy_selected(&session, None, DisplayOptions::SHOW_ALL, |text| {
            assert!(text.contains("first user"));
            Ok(())
        });
        assert_eq!(viewer.status.as_ref().unwrap().label, "Copied 1 block");
        assert_eq!(selected_indices(&viewer), [0]);
    }

    #[test]
    fn all_structured_blocks_and_summary_context_metrics_have_source_identity() {
        use crate::parse::{ExecStatus, RuntimeMetrics, SessionInfo};
        let mut session = sample_session();
        session.session_info = Some(SessionInfo {
            model: Some("test model".into()),
            ..Default::default()
        });
        session.cells = vec![
            SessionCell::SessionInfo(session.session_info.clone().unwrap()),
            SessionCell::Metrics(RuntimeMetrics {
                total_tokens: 10,
                ..Default::default()
            }),
            SessionCell::Message {
                role: MessageRole::User,
                content: "# Inside message\n\n**request**".into(),
                timestamp: None,
            },
            SessionCell::Exec {
                command: vec!["echo".into(), "value".into()],
                cwd: None,
                parsed_summary: None,
                stdout: "hidden stdout".into(),
                stderr: "hidden stderr".into(),
                exit_code: Some(0),
                duration_ms: None,
                status: ExecStatus::Completed,
                timestamp: None,
                is_user: false,
            },
            SessionCell::Metrics(RuntimeMetrics {
                total_tokens: 20,
                ..Default::default()
            }),
        ];
        let summary = sample_summary("# Summary heading\n\n**summary source**");
        let options = DisplayOptions {
            hide_tool_results: true,
            ..DisplayOptions::SHOW_ALL
        };
        let mut viewer = ViewerState::new();
        viewer.render_cache(
            Rect::new(0, 0, 80, 30),
            &session,
            Some(&summary),
            &Theme::default(),
            ThemeName::default(),
            options,
        );
        viewer.select_block(Some(0), KeyModifiers::NONE);
        viewer.select_block(Some(4), KeyModifiers::SHIFT);
        let (count, text) = viewer.selected_markdown(&session, Some(&summary), options);
        assert_eq!(count, 5); // summary, context, message, exec, final metrics
        assert!(text.contains("**summary source**"));
        assert!(text.contains("test model"));
        assert!(text.contains("# Inside message\n\n**request**"));
        assert!(text.contains("echo value"));
        assert!(!text.contains("hidden stdout"));
        assert!(!text.contains("hidden stderr"));
        assert_eq!(text.matches("## metrics").count(), 1);
        assert!(text.contains("20"));
    }

    #[test]
    fn viewer_hints_fit_two_rows_at_standard_terminal_widths() {
        for width in [80, 120] {
            let lines = crate::tui::keymap_hint::layout_hints(
                &ViewerState::HINTS,
                width,
                2,
                &Theme::default(),
                None,
            );
            assert!(lines.iter().all(|line| line.width() <= width));
            let text = lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            for hint in &ViewerState::HINTS {
                assert!(
                    text.contains(hint.key),
                    "missing {} at width {width}",
                    hint.key
                );
            }
        }
    }

    #[test]
    fn block_selection_follows_explorer_anchor_and_range_rules() {
        let session = multi_turn_session();
        let area = Rect::new(0, 0, 80, 80);
        for alt in [KeyModifiers::NONE, KeyModifiers::ALT] {
            let mut viewer = selection_viewer(&session, area);
            let click = |viewer: &mut ViewerState, index: Option<usize>, modifiers| {
                let cache = viewer.render_cache.as_ref().unwrap();
                let row = index.map_or(cache.total_rows + 1, |i| cache.blocks[i].rows.start);
                assert!(row < super::viewport_height(area));
                viewer.handle_mouse(
                    area,
                    MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: 2,
                        row: row as u16 + 1 + super::STICKY_HEADER_HEIGHT,
                        modifiers,
                    },
                );
            };
            click(&mut viewer, Some(1), KeyModifiers::NONE);
            click(&mut viewer, Some(4), KeyModifiers::CONTROL | alt);
            assert_eq!(selected_indices(&viewer), [1, 4]);
            click(&mut viewer, Some(4), KeyModifiers::CONTROL | alt);
            assert_eq!(selected_indices(&viewer), [1]);
            // Both forms share the anchor: start with a conventional Shift-click.
            click(&mut viewer, Some(2), KeyModifiers::SHIFT);
            assert_eq!(selected_indices(&viewer), [2, 3, 4]);
            click(&mut viewer, Some(5), KeyModifiers::SHIFT | alt);
            assert_eq!(selected_indices(&viewer), [4, 5]);
            click(
                &mut viewer,
                Some(0),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT | alt,
            );
            assert_eq!(selected_indices(&viewer), [0, 1, 2, 3, 4, 5]);
            for modifiers in [
                KeyModifiers::CONTROL,
                KeyModifiers::SHIFT,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ] {
                click(&mut viewer, None, modifiers | alt);
                assert_eq!(selected_indices(&viewer).len(), 6);
                assert_eq!(viewer.selection_anchor, Some(SessionBlockId::Message(4)));
            }
            click(&mut viewer, None, KeyModifiers::NONE);
            assert!(selected_indices(&viewer).is_empty());
            assert!(viewer.selection_anchor.is_none());
            click(&mut viewer, Some(3), KeyModifiers::SHIFT | alt);
            assert_eq!(selected_indices(&viewer), [3]);
            click(&mut viewer, Some(2), KeyModifiers::NONE);
            assert_eq!(selected_indices(&viewer), [2]);
        }
    }

    #[test]
    fn unsupported_mouse_gestures_preserve_block_selection_and_anchor() {
        let session = sample_session();
        let area = Rect::new(0, 0, 80, 30);
        let mut viewer = selection_viewer(&session, area);
        viewer.select_block(Some(1), KeyModifiers::NONE);
        viewer.status = Some(crate::tui::statusline::Entry::completed("unchanged"));
        let empty_row = viewer.render_cache.as_ref().unwrap().total_rows + 1;
        for row in [0, empty_row] {
            for (kind, modifiers) in [
                (MouseEventKind::Down(MouseButton::Left), KeyModifiers::ALT),
                (MouseEventKind::Down(MouseButton::Left), KeyModifiers::SUPER),
                (
                    MouseEventKind::Down(MouseButton::Left),
                    KeyModifiers::ALT | KeyModifiers::CONTROL | KeyModifiers::META,
                ),
                (
                    MouseEventKind::Down(MouseButton::Left),
                    KeyModifiers::ALT | KeyModifiers::SHIFT | KeyModifiers::SUPER,
                ),
                (
                    MouseEventKind::Down(MouseButton::Right),
                    KeyModifiers::ALT | KeyModifiers::CONTROL,
                ),
                (
                    MouseEventKind::Up(MouseButton::Left),
                    KeyModifiers::ALT | KeyModifiers::CONTROL,
                ),
                (
                    MouseEventKind::Drag(MouseButton::Left),
                    KeyModifiers::ALT | KeyModifiers::SHIFT,
                ),
            ] {
                viewer.handle_mouse(
                    area,
                    MouseEvent {
                        kind,
                        modifiers,
                        column: 2,
                        row: row as u16 + 1 + super::STICKY_HEADER_HEIGHT,
                    },
                );
                assert_eq!(selected_indices(&viewer), [1]);
                assert_eq!(viewer.selection_anchor, Some(SessionBlockId::Message(1)));
                assert_eq!(viewer.status.as_ref().unwrap().label, "unchanged");
            }
        }
    }

    #[test]
    fn block_selection_survives_reflow_and_search_but_drops_hidden_sources() {
        let session = multi_turn_session();
        let area = Rect::new(0, 0, 80, 30);
        let mut viewer = selection_viewer(&session, area);
        viewer.select_block(Some(0), KeyModifiers::NONE);
        viewer.select_block(Some(3), KeyModifiers::CONTROL);
        viewer.handle_key(
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
            area,
            Some(&session),
            None,
            &Theme::default(),
            ThemeName::default(),
            DisplayOptions::SHOW_ALL,
        );
        let options = DisplayOptions {
            hide_tool_results: true,
            ..DisplayOptions::SHOW_ALL
        };
        viewer.render_cache(
            Rect::new(0, 0, 20, 12),
            &session,
            None,
            &Theme::default(),
            ThemeName::default(),
            options,
        );
        assert_eq!(
            viewer.selected_blocks,
            [crate::export::SessionBlockId::Message(0)].into()
        );
        assert!(viewer.selection_anchor.is_none());
        viewer.select_block(Some(3), KeyModifiers::SHIFT);
        assert_eq!(
            viewer.selected_blocks,
            [crate::export::SessionBlockId::Message(4)].into()
        );
        let (_, text) = viewer.selected_markdown(&session, None, options);
        assert!(text.contains("second user"));
        assert!(!text.contains("tool output"));
    }

    #[test]
    fn mouse_selects_wrapped_rows_and_sticky_heading_owner_without_selecting_footer() {
        let mut session = multi_turn_session();
        session.messages[0].content = "# Heading\n\n界面 emoji 🐈 and é words ".repeat(12);
        let area = Rect::new(4, 3, 32, 16);
        let mut viewer = selection_viewer(&session, area);
        viewer.scroll = 7;
        let body = ViewerState::body_area(area);
        let mouse = |row, modifiers| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: body.x + 3,
            row,
            modifiers,
        };
        viewer.handle_mouse(area, mouse(body.y + 4, KeyModifiers::NONE));
        assert_eq!(selected_indices(&viewer), [0]);
        viewer.handle_mouse(area, mouse(body.y + 1, KeyModifiers::CONTROL));
        assert!(selected_indices(&viewer).is_empty());
        viewer.handle_mouse(area, mouse(body.y + 1, KeyModifiers::SHIFT));
        assert_eq!(selected_indices(&viewer), [0]);
        viewer.handle_mouse(area, mouse(area.bottom() - 2, KeyModifiers::NONE));
        assert_eq!(selected_indices(&viewer), [0]);
        // Huge scroll is clamped to the rendered bottom, including mouse hit testing.
        viewer.scroll = usize::MAX;
        let cache = viewer.render_cache.as_ref().unwrap();
        let content_height =
            body.height
                .saturating_sub(2 + crate::tui::util::STICKY_HEADER_HEIGHT) as usize;
        let start = cache.total_rows.saturating_sub(content_height);
        let last = cache.blocks.last().unwrap();
        let row =
            (last.rows.start - start) as u16 + body.y + 1 + crate::tui::util::STICKY_HEADER_HEIGHT;
        viewer.handle_mouse(area, mouse(row, KeyModifiers::NONE));
        assert_eq!(selected_indices(&viewer), [5]);
    }

    #[test]
    fn selected_markdown_follows_display_order_and_alt_c_does_not_edit_search() {
        let mut session = multi_turn_session();
        session.messages[0].content = "**bold**\n\n```rust\n    let x = 1;\n```".into();
        for message in &mut session.messages {
            message.timestamp = None;
        }
        let area = Rect::new(0, 0, 80, 30);
        let mut viewer = selection_viewer(&session, area);
        viewer.select_block(Some(4), KeyModifiers::NONE);
        viewer.select_block(Some(0), KeyModifiers::CONTROL);
        assert_eq!(
            viewer.handle_key(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::ALT),
                area,
                Some(&session),
                None,
                &Theme::default(),
                ThemeName::default(),
                DisplayOptions::SHOW_ALL
            ),
            ViewerOutcome::CopySelection
        );
        assert_eq!(viewer.search_query(), "");
        let (count, markdown) = viewer.selected_markdown(&session, None, DisplayOptions::SHOW_ALL);
        assert_eq!(count, 2);
        assert_eq!(
            markdown,
            "## user\n\n**bold**\n\n```rust\n    let x = 1;\n```\n\n## user\n\nsecond user\n\n"
        );
    }

    #[test]
    fn selection_highlights_full_width_and_preserves_search_highlights() {
        use ratatui::{backend::TestBackend, Terminal};
        let session = multi_turn_session();
        let area = Rect::new(0, 0, 80, 30);
        let theme = Theme::default();
        let mut viewer = ViewerState::with_search("first");
        viewer.render_cache(
            area,
            &session,
            None,
            &theme,
            ThemeName::default(),
            DisplayOptions::SHOW_ALL,
        );
        viewer.select_block(Some(0), KeyModifiers::NONE);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                viewer.render(
                    frame,
                    area,
                    &session,
                    false,
                    None,
                    &theme,
                    ThemeName::default(),
                    DisplayOptions::SHOW_ALL,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let body_y = 1 + crate::tui::util::STICKY_HEADER_HEIGHT;
        assert_eq!(buffer[(78, body_y)].bg, theme.selection);
        assert_eq!(buffer[(78, body_y + 1)].bg, theme.selection);
        assert!(buffer
            .content
            .iter()
            .any(|cell| cell.bg == theme.search_match_bg));
        assert!(buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
            .contains("1 selected"));
    }

    #[test]
    fn query_occurrence_rows_track_wrapped_content_lines() {
        let session = sample_session();
        let mut viewer = ViewerState::with_search("alpha");
        refresh_viewer(
            &mut viewer,
            &session,
            Rect::new(0, 0, 14, 60),
            DisplayOptions::SHOW_ALL,
        );
        assert_eq!(
            viewer.render_cache.as_ref().unwrap().match_rows,
            vec![3, 5, 10]
        );
    }

    #[test]
    fn query_occurrence_rows_follow_markdown_code_provenance() {
        let session = markdown_code_session();
        let mut viewer = ViewerState::with_search("alpha");
        refresh_viewer(
            &mut viewer,
            &session,
            Rect::new(0, 0, 82, 30),
            DisplayOptions::SHOW_ALL,
        );
        assert_eq!(viewer.render_cache.as_ref().unwrap().match_rows, vec![1]);
    }

    #[test]
    fn collect_message_rows_tracks_header_boundaries() {
        let session = sample_session();
        let rows = collect_message_rows(
            &session,
            None,
            &Theme::default(),
            12,
            MessageJumpScope::Any,
            DisplayOptions::default(),
        );

        assert_eq!(rows, vec![0, 7]);
    }

    #[test]
    fn collect_message_rows_can_limit_to_user_messages() {
        let session = multi_turn_session();
        let rows = collect_message_rows(
            &session,
            None,
            &Theme::default(),
            80,
            MessageJumpScope::UserOnly,
            DisplayOptions::default(),
        );

        assert_eq!(rows, vec![0, 12]);
    }

    #[test]
    fn collect_message_rows_matches_wrapped_render_height_at_narrow_width() {
        let session = wrapped_navigation_session();
        let theme = Theme::default();
        let width = 9;
        let rows = collect_message_rows(
            &session,
            None,
            &theme,
            width,
            MessageJumpScope::Any,
            DisplayOptions::default(),
        );

        let first = &session.messages[0];
        let expected_second_start =
            wrapped_text_height(&Text::from(Line::from(message_header_text(first))), width)
                + wrapped_text_height(
                    &render_message_body(
                        session.agent,
                        first.role,
                        first.content.as_str(),
                        &theme,
                        None,
                    ),
                    width,
                )
                + 1;

        assert_eq!(rows, vec![0, expected_second_start]);
    }

    #[test]
    fn match_navigation_wraps_in_both_directions() {
        let matches = vec![1, 5, 9];

        assert_eq!(next_match_index(&matches, None, 0), 0);
        assert_eq!(next_match_index(&matches, Some(0), 0), 1);
        assert_eq!(next_match_index(&matches, Some(2), 0), 0);

        assert_eq!(previous_match_index(&matches, None, 5), 0);
        assert_eq!(previous_match_index(&matches, Some(0), 0), 2);
        assert_eq!(previous_match_index(&matches, Some(2), 0), 1);
    }

    #[test]
    fn message_navigation_next_wraps_previous_clamps() {
        let rows = vec![0, 4, 9];

        assert_eq!(
            message_row_for_scroll(&rows, 0, MessageDirection::Next),
            Some(4)
        );
        assert_eq!(
            message_row_for_scroll(&rows, 3, MessageDirection::Next),
            Some(4)
        );
        assert_eq!(
            message_row_for_scroll(&rows, 9, MessageDirection::Next),
            Some(0)
        );
        assert_eq!(
            message_row_for_scroll(&rows, 9, MessageDirection::Previous),
            Some(4)
        );
        assert_eq!(
            message_row_for_scroll(&rows, 3, MessageDirection::Previous),
            Some(0)
        );
        // Previous at the first boundary returns None (no wraparound)
        assert_eq!(
            message_row_for_scroll(&rows, 0, MessageDirection::Previous),
            None
        );
    }

    #[test]
    fn scroll_progress_tracks_farthest_visible_row() {
        assert_eq!(scroll_progress_percent(0, 9, 100), 9);
        assert_eq!(scroll_progress_percent(40, 9, 100), 49);
        assert_eq!(scroll_progress_percent(91, 9, 100), 100);
        assert_eq!(scroll_progress_percent(0, 20, 12), 100);
    }

    #[test]
    fn match_scrolloff_scales_for_short_viewports() {
        assert_eq!(match_scrolloff(2), 0);
        assert_eq!(match_scrolloff(6), 2);
        assert_eq!(match_scrolloff(12), 3);
    }

    #[test]
    fn scroll_for_match_applies_scrolloff_until_clamped() {
        assert_eq!(scroll_for_match(10, 12, 40), 7);
        assert_eq!(scroll_for_match(2, 12, 40), 0);
        assert_eq!(scroll_for_match(39, 12, 30), 30);
    }

    #[test]
    fn viewer_starts_without_active_match() {
        let state = ViewerState::new();

        assert!(state.active_match.is_none());
    }

    #[test]
    fn internal_context_visibility_updates_viewer_and_message_navigation() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sessions/codex/internal_context.jsonl");
        let mut session = crate::parse::parse_codex_session_file(path)
            .unwrap()
            .unwrap();
        let theme = Theme::default();
        let area = Rect::new(0, 0, 100, 30);
        for fallback in [false, true] {
            if fallback {
                session.cells.clear();
            }
            let mut state = ViewerState::new();
            for hidden in [true, false, true] {
                let options = DisplayOptions {
                    hide_internal_context: hidden,
                    ..DisplayOptions::default()
                };
                let text = rendered_lines(
                    &state
                        .render_cache(area, &session, None, &theme, ThemeName::default(), options)
                        .text,
                )
                .join("\n");
                assert_eq!(text.contains("InternalGoalNeedle"), !hidden);
                assert_eq!(text.contains("LegacyGoalNeedle"), !hidden);
                assert!(text.contains("MixedPromptNeedle"));
                for (scope, count) in [(MessageJumpScope::Any, 6), (MessageJumpScope::UserOnly, 5)]
                {
                    let rows = collect_message_rows(&session, None, &theme, 96, scope, options);
                    assert_eq!(rows.len(), count - if hidden { 2 } else { 0 });
                    assert!(rows.windows(2).all(|pair| pair[0] < pair[1]));
                }
            }
        }
    }

    #[test]
    fn viewer_honors_project_docs_autodump_display_option() {
        let session = project_docs_session();
        let area = Rect::new(0, 0, 80, 20);
        let theme = Theme::default();
        let mut state = ViewerState::new();

        let hidden_text = rendered_lines(
            &state
                .render_cache(
                    area,
                    &session,
                    None,
                    &theme,
                    ThemeName::Lazygit,
                    DisplayOptions::default(),
                )
                .text,
        )
        .join("\n");

        assert!(!hidden_text.contains("AGENTS.md instructions"));
        assert!(hidden_text.contains("real request"));

        let visible_options = DisplayOptions {
            hide_project_docs_autodump: false,
            ..DisplayOptions::default()
        };

        let visible_text = rendered_lines(
            &state
                .render_cache(
                    area,
                    &session,
                    None,
                    &theme,
                    ThemeName::Lazygit,
                    visible_options,
                )
                .text,
        )
        .join("\n");

        assert!(visible_text.contains("AGENTS.md instructions"));
        assert!(visible_text.contains("real request"));
    }

    #[test]
    fn message_navigation_skips_hidden_project_docs_autodump() {
        let session = project_docs_session();
        let theme = Theme::default();

        let hidden_rows = collect_message_rows_with_options(
            &session,
            None,
            &theme,
            80,
            MessageJumpScope::UserOnly,
            SessionRenderOptions {
                hide_project_docs_autodump: true,
                ..SessionRenderOptions::default()
            },
        );
        let visible_rows = collect_message_rows_with_options(
            &session,
            None,
            &theme,
            80,
            MessageJumpScope::UserOnly,
            SessionRenderOptions {
                hide_project_docs_autodump: false,
                ..SessionRenderOptions::default()
            },
        );

        assert_eq!(hidden_rows, vec![0]);
        assert_eq!(visible_rows, vec![0, 7]); // HTML source lines keep their logical newlines.
    }

    #[test]
    fn message_navigation_skips_hidden_skill_text_injection() {
        let mut session = project_docs_session();
        let SessionCell::Message { content, .. } = &mut session.cells[0] else {
            panic!("expected a message cell");
        };
        *content = "<skill><name>commit</name>helper instructions</skill>".to_owned();
        let theme = Theme::default();

        let hidden_rows = collect_message_rows(
            &session,
            None,
            &theme,
            80,
            MessageJumpScope::UserOnly,
            DisplayOptions {
                hide_skill_text_injection: true,
                ..DisplayOptions::default()
            },
        );
        let visible_rows = collect_message_rows(
            &session,
            None,
            &theme,
            80,
            MessageJumpScope::UserOnly,
            DisplayOptions::default(),
        );

        assert_eq!(hidden_rows.len(), 1);
        assert_eq!(visible_rows.len(), 2);
    }

    #[test]
    fn viewer_title_matches_card_style_with_scroll_suffix() {
        let session = sample_session();
        let title = viewer_title(&session, false, &Theme::default(), 9);
        let rendered = title
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(
            rendered,
            "{C} · /tmp/demo · 1969-12-31 · 6 lines · 9% scrolled"
        );
    }

    #[test]
    fn viewer_title_prefixes_trashed_sessions() {
        let session = sample_session();
        let title = viewer_title(&session, true, &Theme::default(), 9);
        let rendered = title
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(
            rendered,
            "{C} · trashed · /tmp/demo · 1969-12-31 · 6 lines · 9% scrolled"
        );
    }

    #[test]
    fn viewer_hints_match_focused_search_and_find() {
        let keys = ViewerState::HINTS
            .iter()
            .map(|hint| hint.key)
            .collect::<Vec<_>>();

        assert!(!keys.contains(&"/"));
        assert!(!keys.contains(&"n/p"));
        assert!(keys.contains(&"^N/^P"));
        assert!(keys.contains(&"Tab/^F"));
        assert!(keys.contains(&"Alt+R/I"));
        assert!(keys.contains(&"^⇧F"));
    }

    #[test]
    fn summary_leadin_offsets_messages_and_is_searchable_only_by_find() {
        let session = sample_session();
        let summary = sample_summary("alpha summary");

        let mut viewer = ViewerState::with_search("alpha");
        viewer.find_jump = false;
        viewer.find = Input::default().with_value("alpha".to_owned());
        viewer.render_cache(
            Rect::new(0, 0, 82, 30),
            &session,
            Some(&summary),
            &Theme::default(),
            ThemeName::default(),
            DisplayOptions::SHOW_ALL,
        );
        let cache = viewer.render_cache.as_ref().unwrap();
        let message_rows = collect_message_rows(
            &session,
            Some(&summary),
            &Theme::default(),
            80,
            MessageJumpScope::Any,
            DisplayOptions::default(),
        );

        assert_eq!(cache.match_rows, vec![6, 6, 9]);
        assert_eq!(cache.find_rows, vec![2, 6, 6, 9]);
        assert_eq!(message_rows, vec![5, 8]);
    }

    #[test]
    fn escape_closes_viewer_even_with_search_text() {
        let area = Rect::new(0, 0, 80, 20);
        let session = sample_session();
        let mut state = ViewerState::new();
        state.search = Input::default().with_value("alpha".to_owned());

        let outcome = state.handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            area,
            Some(&session),
            None,
            &Theme::default(),
            ThemeName::Lazygit,
            DisplayOptions::default(),
        );
        assert_eq!(outcome, ViewerOutcome::Close);
    }

    #[test]
    fn plain_n_and_p_edit_search_instead_of_navigating_matches() {
        let area = Rect::new(0, 0, 80, 20);
        let session = sample_session();
        let mut state = ViewerState::new();
        state.input_focus = super::ViewerInputFocus::Search;

        state.handle_key(
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
            area,
            Some(&session),
            None,
            &Theme::default(),
            ThemeName::Lazygit,
            DisplayOptions::default(),
        );

        state.handle_key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
            area,
            Some(&session),
            None,
            &Theme::default(),
            ThemeName::Lazygit,
            DisplayOptions::default(),
        );

        assert_eq!(state.search_query(), "np");
        assert!(state.active_match.is_none());
    }

    #[test]
    fn plain_q_edits_search_instead_of_closing_viewer() {
        let area = Rect::new(0, 0, 80, 20);
        let session = sample_session();
        let mut state = ViewerState::new();
        state.input_focus = super::ViewerInputFocus::Search;

        let outcome = state.handle_key(
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            area,
            Some(&session),
            None,
            &Theme::default(),
            ThemeName::Lazygit,
            DisplayOptions::default(),
        );

        assert_eq!(outcome, ViewerOutcome::Stay);
        assert_eq!(state.search_query(), "q");
    }

    #[test]
    fn control_n_and_p_jump_between_matches() {
        let session = sample_session();
        let area = Rect::new(0, 0, 80, 20);
        let mut state = ViewerState::new();
        state.input_focus = super::ViewerInputFocus::Search;
        state.search = Input::default().with_value("alpha".to_owned());

        let next = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL);
        let previous = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);

        state.handle_key(
            next,
            area,
            Some(&session),
            None,
            &Theme::default(),
            ThemeName::Lazygit,
            DisplayOptions::default(),
        );
        assert_eq!(state.active_match, Some(0));
        assert_eq!(state.scroll, 0);

        state.handle_key(
            next,
            area,
            Some(&session),
            None,
            &Theme::default(),
            ThemeName::Lazygit,
            DisplayOptions::default(),
        );
        assert_eq!(state.active_match, Some(1));
        assert_eq!(state.scroll, 0);

        state.handle_key(
            previous,
            area,
            Some(&session),
            None,
            &Theme::default(),
            ThemeName::Lazygit,
            DisplayOptions::default(),
        );
        assert_eq!(state.active_match, Some(0));
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn shift_up_and_down_jump_between_message_boundaries() {
        let session = sample_session();
        let area = Rect::new(0, 0, 14, 10);
        let mut state = ViewerState::new();

        state.handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT),
            area,
            Some(&session),
            None,
            &Theme::default(),
            ThemeName::Lazygit,
            DisplayOptions::default(),
        );
        assert_eq!(state.scroll, 7);

        state.handle_key(
            KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT),
            area,
            Some(&session),
            None,
            &Theme::default(),
            ThemeName::Lazygit,
            DisplayOptions::default(),
        );
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn control_shift_up_and_down_jump_between_user_messages() {
        let session = multi_turn_session();
        let area = Rect::new(0, 0, 80, 10);
        let mut state = ViewerState::new();

        state.handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
            area,
            Some(&session),
            None,
            &Theme::default(),
            ThemeName::Lazygit,
            DisplayOptions::default(),
        );
        assert_eq!(state.scroll, 12);

        state.handle_key(
            KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
            area,
            Some(&session),
            None,
            &Theme::default(),
            ThemeName::Lazygit,
            DisplayOptions::default(),
        );
        assert_eq!(state.scroll, 0);
    }

    fn rendered_lines(text: &Text<'_>) -> Vec<String> {
        text.lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    fn sample_session() -> Session {
        Session {
            session_id: "session-1".to_owned(),
            agent: Agent::Claude,
            project: "/tmp/demo".to_owned(),
            branch: Some("main".to_owned()),
            cwd: Some("/tmp/demo".to_owned()),
            created: Some(Utc::now()),
            modified: Some(Utc::now()),
            modified_ts: 0,
            lines: 6,
            file_path: PathBuf::from("/tmp/demo/session.jsonl"),
            first_msg_role: Some(MessageRole::User),
            first_msg_content: "alpha beta gamma delta".to_owned(),
            last_msg_role: Some(MessageRole::Assistant),
            last_msg_content: "omega alpha".to_owned(),
            first_user_msg_content: "alpha beta gamma delta".to_owned(),
            derivation_type: DerivationType::Original,
            is_sidechain: false,
            custom_title: Some("demo".to_owned()),
            messages: vec![
                SessionMessage {
                    role: MessageRole::User,
                    content: "alpha beta gamma delta alpha".to_owned(),
                    timestamp: Some(Utc::now()),
                    tool_name: None,
                },
                SessionMessage {
                    role: MessageRole::Assistant,
                    content: "omega alpha".to_owned(),
                    timestamp: Some(Utc::now()),
                    tool_name: None,
                },
            ],
            content: "alpha beta gamma delta alpha\nomega alpha".to_owned(),
            search_fields: Default::default(),
            cells: Vec::new(),
            session_info: None,
            lineage: Default::default(),
        }
    }

    fn project_docs_session() -> Session {
        let docs = "# AGENTS.md instructions for /tmp/demo\n\n<INSTRUCTIONS>\nUse cargo test.\n</INSTRUCTIONS>";
        let request = "real request";
        Session {
            session_id: "session-docs".to_owned(),
            agent: Agent::Codex,
            project: "/tmp/demo".to_owned(),
            branch: None,
            cwd: Some("/tmp/demo".to_owned()),
            created: Some(Utc::now()),
            modified: Some(Utc::now()),
            modified_ts: 0,
            lines: 8,
            file_path: PathBuf::from("/tmp/demo/session-docs.jsonl"),
            first_msg_role: Some(MessageRole::User),
            first_msg_content: docs.to_owned(),
            last_msg_role: Some(MessageRole::User),
            last_msg_content: request.to_owned(),
            first_user_msg_content: docs.to_owned(),
            derivation_type: DerivationType::Original,
            is_sidechain: false,
            custom_title: Some("demo docs".to_owned()),
            messages: Vec::new(),
            content: format!("{docs}\n{request}"),
            search_fields: Default::default(),
            cells: vec![
                SessionCell::Message {
                    role: MessageRole::User,
                    content: docs.to_owned(),
                    timestamp: Some(Utc::now()),
                },
                SessionCell::Message {
                    role: MessageRole::User,
                    content: request.to_owned(),
                    timestamp: Some(Utc::now()),
                },
            ],
            session_info: None,
            lineage: Default::default(),
        }
    }

    fn markdown_code_session() -> Session {
        Session {
            session_id: "session-md".to_owned(),
            agent: Agent::Claude,
            project: "/tmp/demo".to_owned(),
            branch: Some("main".to_owned()),
            cwd: Some("/tmp/demo".to_owned()),
            created: Some(Utc::now()),
            modified: Some(Utc::now()),
            modified_ts: 0,
            lines: 3,
            file_path: PathBuf::from("/tmp/demo/session-md.jsonl"),
            first_msg_role: Some(MessageRole::Assistant),
            first_msg_content: "```rust\nfn alpha() {}\n```".to_owned(),
            last_msg_role: Some(MessageRole::Assistant),
            last_msg_content: "```rust\nfn alpha() {}\n```".to_owned(),
            first_user_msg_content: String::new(),
            derivation_type: DerivationType::Original,
            is_sidechain: false,
            custom_title: Some("demo markdown".to_owned()),
            messages: vec![SessionMessage {
                role: MessageRole::Assistant,
                content: "```rust\nfn alpha() {}\n```".to_owned(),
                timestamp: Some(Utc::now()),
                tool_name: None,
            }],
            content: "```rust\nfn alpha() {}\n```".to_owned(),
            search_fields: Default::default(),
            cells: Vec::new(),
            session_info: None,
            lineage: Default::default(),
        }
    }

    fn sample_summary(body: &str) -> SummarySidecar {
        SummarySidecar::new(
            &PathBuf::from("/tmp/demo/session.jsonl"),
            &Fingerprint {
                line_count: 6,
                last_line_sha256: "a".repeat(64),
            },
            SummarizeBackend::Codex,
            body.to_owned(),
        )
    }

    fn multi_turn_session() -> Session {
        Session {
            session_id: "session-2".to_owned(),
            agent: Agent::Claude,
            project: "/tmp/demo".to_owned(),
            branch: Some("main".to_owned()),
            cwd: Some("/tmp/demo".to_owned()),
            created: Some(Utc::now()),
            modified: Some(Utc::now()),
            modified_ts: 0,
            lines: 12,
            file_path: PathBuf::from("/tmp/demo/session-2.jsonl"),
            first_msg_role: Some(MessageRole::User),
            first_msg_content: "first user".to_owned(),
            last_msg_role: Some(MessageRole::Assistant),
            last_msg_content: "second assistant".to_owned(),
            first_user_msg_content: "first user".to_owned(),
            derivation_type: DerivationType::Original,
            is_sidechain: false,
            custom_title: Some("demo multi turn".to_owned()),
            messages: vec![
                SessionMessage {
                    role: MessageRole::User,
                    content: "first user".to_owned(),
                    timestamp: Some(Utc::now()),
                    tool_name: None,
                },
                SessionMessage {
                    role: MessageRole::Assistant,
                    content: "first assistant".to_owned(),
                    timestamp: Some(Utc::now()),
                    tool_name: None,
                },
                SessionMessage {
                    role: MessageRole::ToolCall,
                    content: "run tool".to_owned(),
                    timestamp: Some(Utc::now()),
                    tool_name: Some("Read".to_owned()),
                },
                SessionMessage {
                    role: MessageRole::ToolResult,
                    content: "tool output".to_owned(),
                    timestamp: Some(Utc::now()),
                    tool_name: Some("Read".to_owned()),
                },
                SessionMessage {
                    role: MessageRole::User,
                    content: "second user".to_owned(),
                    timestamp: Some(Utc::now()),
                    tool_name: None,
                },
                SessionMessage {
                    role: MessageRole::Assistant,
                    content: "second assistant".to_owned(),
                    timestamp: Some(Utc::now()),
                    tool_name: None,
                },
            ],
            content:
                "first user\nfirst assistant\nrun tool\ntool output\nsecond user\nsecond assistant"
                    .to_owned(),
            search_fields: Default::default(),
            cells: Vec::new(),
            session_info: None,
            lineage: Default::default(),
        }
    }

    fn wrapped_navigation_session() -> Session {
        Session {
            session_id: "session-wrap".to_owned(),
            agent: Agent::Claude,
            project: "/tmp/demo".to_owned(),
            branch: Some("main".to_owned()),
            cwd: Some("/tmp/demo".to_owned()),
            created: Some(Utc::now()),
            modified: Some(Utc::now()),
            modified_ts: 0,
            lines: 4,
            file_path: PathBuf::from("/tmp/demo/session-wrap.jsonl"),
            first_msg_role: Some(MessageRole::User),
            first_msg_content: "This line wraps hard in the preview and viewer.".to_owned(),
            last_msg_role: Some(MessageRole::Assistant),
            last_msg_content: "Short reply".to_owned(),
            first_user_msg_content: "This line wraps hard in the preview and viewer.".to_owned(),
            derivation_type: DerivationType::Original,
            is_sidechain: false,
            custom_title: Some("wrapped navigation".to_owned()),
            messages: vec![
                SessionMessage {
                    role: MessageRole::User,
                    content: "This line wraps hard in the preview and viewer.".to_owned(),
                    timestamp: Some(Utc::now()),
                    tool_name: None,
                },
                SessionMessage {
                    role: MessageRole::Assistant,
                    content: "Short reply".to_owned(),
                    timestamp: Some(Utc::now()),
                    tool_name: None,
                },
            ],
            content: "This line wraps hard in the preview and viewer.\nShort reply".to_owned(),
            search_fields: Default::default(),
            cells: Vec::new(),
            session_info: None,
            lineage: Default::default(),
        }
    }
}
