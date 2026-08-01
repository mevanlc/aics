use std::ops::Range;
use std::path::PathBuf;

use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::parse::{is_project_docs_autodump, MessageRole, Session, SessionCell};
use crate::search_query::extract_highlight_terms;
use crate::settings::{DisplayOptions, ThemeName};
use crate::summary::SummarySidecar;
use crate::tui::keymap_hint::{self, KeymapHint};
use crate::tui::markdown::render_markdown_message;
use crate::tui::preview::{
    render_message_body, render_session_document_with_options, split_sticky_body, DisplayDocument,
    SessionRenderOptions,
};
use crate::tui::profile;
use crate::tui::theme::Theme;
use crate::tui::util::{
    abbreviate_home_path, agent_badge, block_title, format_line_count, relative_time,
    session_display_title, session_message_label, sticky_header_for_scroll,
    sticky_rows_from_line_markers, wrapped_text_height, FullLineBackgroundParagraph,
    StickyHeaderWidget, StickyRowMarker, STICKY_HEADER_HEIGHT,
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
    active_match: Option<usize>,
    render_cache: Option<ViewerRenderCache>,
}

#[derive(Debug, Clone)]
struct ViewerRenderCache {
    path: PathBuf,
    query: String,
    width: u16,
    theme_name: ThemeName,
    display_options: DisplayOptions,
    summary_stamp: Option<(i64, usize, usize)>,
    total_rows: usize,
    text: Text<'static>,
    match_rows: Vec<usize>,
    sticky_rows: Vec<StickyRowMarker>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerOutcome {
    Stay,
    Close,
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
    const HINTS: [KeymapHint; 6] = [
        KeymapHint::new("↑↓/PgUp/PgDn/Home/End", "scroll"),
        KeymapHint::new("^Up/^Dn", "message"),
        KeymapHint::new("^⇧Up/^⇧Dn", "user"),
        KeymapHint::new("^N/^P", "matches"),
        KeymapHint::new("^U/^E", "edit"),
        KeymapHint::new("Esc", "close"),
    ];

    pub fn new() -> Self {
        Self::with_search("")
    }

    pub fn with_search(query: &str) -> Self {
        Self {
            scroll: 0,
            search: Input::default().with_value(query.to_owned()),
            active_match: None,
            render_cache: None,
        }
    }

    pub fn search_query(&self) -> &str {
        self.search.value()
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
        match key.code {
            KeyCode::Esc => ViewerOutcome::Close,
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
                let before = self.search.value().to_owned();
                self.search.handle_event(&Event::Key(key));
                if self.search.value() != before {
                    self.active_match = None;
                    self.render_cache = None;
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

        let (total_rows, mut text, match_rows, sticky_rows) = {
            let cache =
                self.render_cache(area, session, summary, theme, theme_name, display_options);
            (
                cache.total_rows,
                cache.text.clone(),
                cache.match_rows.clone(),
                cache.sticky_rows.clone(),
            )
        };
        if let Some(active_match_row) = self
            .active_match
            .and_then(|index| match_rows.get(index).copied())
        {
            let viewport_width = body_area.width.saturating_sub(2);
            highlight_active_match(&mut text, active_match_row, viewport_width, theme);
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
        frame.render_widget(
            FullLineBackgroundParagraph::new(text).scroll(scroll),
            body_content_area,
        );

        // Split footer into search bar (bordered) and keymap hints.
        let footer_chunks = Layout::vertical([
            Constraint::Length(VIEWER_SEARCH_HEIGHT),
            Constraint::Length(VIEWER_HINTS_HEIGHT),
        ])
        .split(chunks[1]);

        let search_bar = Paragraph::new(Line::from(Span::styled(
            self.search.value().to_owned(),
            Style::default().fg(theme.text),
        )))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(theme.border_style(false))
                .title(block_title(Span::styled(
                    "Search",
                    Style::default().fg(theme.accent),
                ))),
        );
        frame.render_widget(search_bar, footer_chunks[0]);

        keymap_hint::render(frame, footer_chunks[1], &Self::HINTS, theme, "");

        let cursor_x = footer_chunks[0]
            .x
            .saturating_add(1 + self.search.visual_cursor() as u16)
            .min(footer_chunks[0].right().saturating_sub(1));
        frame.set_cursor_position((cursor_x, footer_chunks[0].y.saturating_add(1)));
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
        let body_area = split_viewer(area)[0];
        let cache = self.render_cache(area, session, summary, theme, theme_name, display_options);
        let viewport_height = body_area
            .height
            .saturating_sub(2)
            .saturating_sub(STICKY_HEADER_HEIGHT) as usize;
        cache.total_rows.saturating_sub(viewport_height)
    }

    pub fn body_area(area: Rect) -> Rect {
        split_viewer(area)[0]
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
            self.active_match = None;
            return;
        };

        let body_area = Self::body_area(area);
        let matches = self
            .render_cache(area, session, summary, theme, theme_name, display_options)
            .match_rows
            .clone();
        if matches.is_empty() {
            self.active_match = None;
            return;
        }

        let next_index = match direction {
            MatchDirection::Next => next_match_index(&matches, self.active_match, self.scroll),
            MatchDirection::Previous => {
                previous_match_index(&matches, self.active_match, self.scroll)
            }
        };

        self.active_match = Some(next_index);
        let max_scroll =
            self.max_scroll(area, session, summary, theme, theme_name, display_options);
        let viewport_height = body_area
            .height
            .saturating_sub(2)
            .saturating_sub(STICKY_HEADER_HEIGHT) as usize;
        self.scroll = scroll_for_match(matches[next_index], viewport_height, max_scroll);
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

        let body_area = Self::body_area(area);
        let viewport_width = body_area.width.saturating_sub(2);
        let rows = collect_message_rows_with_options(
            session,
            summary,
            theme,
            viewport_width,
            scope,
            self.render_options(display_options),
        );
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
        let query = self.search.value().to_owned();
        let summary_stamp = summary.map(summary_stamp);

        let cache_miss = self.render_cache.as_ref().is_none_or(|cache| {
            cache.path != path
                || cache.query != query
                || cache.width != width
                || cache.theme_name != theme_name
                || cache.display_options != display_options
                || cache.summary_stamp != summary_stamp
        });
        if cache_miss {
            profile::event("viewer.cache.miss");
            let highlight_query = (!query.is_empty()).then_some(query.as_str());
            let document = render_viewer_document(
                session,
                summary,
                theme,
                highlight_query,
                self.render_options(display_options),
            );
            let text = document.text;
            let total_rows = wrapped_text_height(&text, width).max(1);
            let match_rows = collect_match_rows_in_text(&text, &query, width);
            let sticky_rows = sticky_rows_from_line_markers(&text, &document.sticky_markers, width);
            self.render_cache = Some(ViewerRenderCache {
                path,
                query,
                width,
                theme_name,
                display_options,
                summary_stamp,
                total_rows,
                text,
                match_rows,
                sticky_rows,
            });
        } else {
            profile::event("viewer.cache.hit");
        }

        self.render_cache
            .as_ref()
            .expect("viewer cache should exist")
    }

    fn render_options(&self, display_options: DisplayOptions) -> SessionRenderOptions {
        SessionRenderOptions::new(display_options)
    }
}

fn split_viewer(area: Rect) -> [Rect; 2] {
    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(VIEWER_FOOTER_HEIGHT)])
        .split(area);
    [chunks[0], chunks[1]]
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
    if let Some(summary) = summary {
        let summary_start = lines.len();
        lines.extend(render_summary_leadin(summary, theme, highlight_query).lines);
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
    lines.extend(session_doc.text.lines);
    sticky_markers.extend(session_doc.sticky_markers.into_iter().map(|marker| {
        crate::tui::util::StickyLineMarker {
            line_index: marker.line_index + session_start,
            header: marker.header,
        }
    }));
    DisplayDocument {
        text: Text::from(lines),
        sticky_markers,
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

fn summary_stamp(summary: &SummarySidecar) -> (i64, usize, usize) {
    (
        summary.generated_at.timestamp(),
        summary.line_count,
        summary.body.len(),
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

/// Collect wrapped-row indices that contain a match for `query` within an
/// already-rendered `Text`. Shared with the preview pane so it can navigate
/// matches without re-rendering the session.
pub(crate) fn collect_match_rows_in_text(text: &Text<'_>, query: &str, width: u16) -> Vec<usize> {
    let terms = extract_highlight_terms(query);
    if terms.is_empty() || width == 0 {
        return Vec::new();
    }

    let width = width as usize;
    let mut rows = Vec::new();
    let mut row_offset = 0usize;
    for line in &text.lines {
        let content = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        for relative_row in collect_line_match_rows(&content, width, &terms) {
            let absolute_row = row_offset + relative_row;
            if rows.last().copied() != Some(absolute_row) {
                rows.push(absolute_row);
            }
        }
        row_offset += wrapped_rendered_line_height(line, width);
    }
    rows
}

/// Re-style search-match spans on the source line containing the active match
/// row so the "current" match stands out from the rest.
pub(crate) fn highlight_active_match(
    text: &mut Text<'_>,
    active_row: usize,
    width: u16,
    theme: &Theme,
) {
    if width == 0 {
        return;
    }
    let width = width as usize;
    let mut row_offset = 0usize;
    let active_match_style = theme.active_match_style();
    for line in text.lines.iter_mut() {
        let h = wrapped_rendered_line_height(line, width);
        if active_row >= row_offset && active_row < row_offset + h {
            // This source line contains the active match row — promote highlights.
            for span in &mut line.spans {
                if span.style.bg == Some(theme.search_match_bg) {
                    span.style = span.style.patch(active_match_style);
                }
            }
            return;
        }
        row_offset += h;
    }
}

#[cfg(test)]
fn collect_match_rows(
    session: &Session,
    summary: Option<&SummarySidecar>,
    theme: &Theme,
    query: &str,
    width: u16,
) -> Vec<usize> {
    let highlight_query = (!query.is_empty()).then_some(query);
    let document = render_viewer_document(
        session,
        summary,
        theme,
        highlight_query,
        SessionRenderOptions::default(),
    );
    collect_match_rows_in_text(&document.text, query, width)
}

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

    let width = width as usize;
    let mut rows = Vec::with_capacity(session.messages.len().max(session.cells.len()));
    let mut row_offset = summary
        .map(|summary| {
            wrapped_text_height(&render_summary_leadin(summary, theme, None), width as u16) + 2
        })
        .unwrap_or(0);

    if session.cells.is_empty() {
        for message in &session.messages {
            if should_skip_message_row(message.role, &message.content, options) {
                continue;
            }
            if matches!(scope, MessageJumpScope::Any)
                || matches!(scope, MessageJumpScope::UserOnly) && message.role == MessageRole::User
            {
                rows.push(row_offset);
            }
            row_offset += wrapped_line_height(&message_header_text(message), width);

            let rendered = render_message_body(
                session.agent,
                message.role,
                message.content.as_str(),
                theme,
                None,
            );
            for line in &rendered.lines {
                row_offset += wrapped_rendered_line_height(line, width);
            }
            row_offset += 1;
        }
        return rows;
    }

    for cell in &session.cells {
        if matches!(cell, SessionCell::SessionInfo(_) | SessionCell::Metrics(_)) {
            continue;
        }
        let SessionCell::Message {
            role,
            content,
            timestamp,
        } = cell
        else {
            let before = row_offset;
            row_offset += wrapped_text_height(
                &render_session_document_with_options(
                    &Session {
                        cells: vec![cell.clone()],
                        messages: Vec::new(),
                        session_info: None,
                        ..session.clone()
                    },
                    theme,
                    None,
                    options,
                )
                .text,
                width as u16,
            );
            if row_offset == before {
                row_offset += 1;
            }
            continue;
        };
        if should_skip_message_row(*role, content, options) {
            continue;
        }
        if matches!(scope, MessageJumpScope::Any)
            || matches!(scope, MessageJumpScope::UserOnly) && *role == MessageRole::User
        {
            rows.push(row_offset);
        }
        let message = crate::parse::SessionMessage {
            role: *role,
            content: content.clone(),
            timestamp: *timestamp,
            tool_name: None,
        };
        row_offset += wrapped_line_height(&message_header_text(&message), width);
        let rendered = render_message_body(session.agent, *role, content.as_str(), theme, None);
        for line in &rendered.lines {
            row_offset += wrapped_rendered_line_height(line, width);
        }
        row_offset += 1;
    }

    rows
}

fn should_skip_message_row(
    role: MessageRole,
    content: &str,
    options: SessionRenderOptions,
) -> bool {
    !crate::tui::preview::shows_message_role(options.display_options, role)
        || options.hide_project_docs_autodump && is_project_docs_autodump(role, content)
        || options.display_options.hide_skill_text_injection
            && crate::parse::is_skill_text_injection(role, content)
}

fn message_header_text(message: &crate::parse::SessionMessage) -> String {
    let label = session_message_label(message);
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

fn collect_line_match_rows(line: &str, width: usize, terms: &[String]) -> Vec<usize> {
    if width == 0 {
        return Vec::new();
    }

    let lower = line.to_ascii_lowercase();
    let wrapped_ranges = wrapped_line_byte_ranges(line, width);
    let mut rows = Vec::new();
    let mut index = 0usize;

    while index < line.len() {
        let mut matched_len = 0usize;
        for term in terms {
            if lower[index..].starts_with(term) {
                matched_len = matched_len.max(term.len());
            }
        }

        if matched_len > 0 {
            let row = wrapped_row_for_match(&wrapped_ranges, index, index + matched_len);
            if rows.last().copied() != Some(row) {
                rows.push(row);
            }
            index += matched_len;
            continue;
        }

        index = line[index..]
            .grapheme_indices(true)
            .nth(1)
            .map(|(offset, _)| index + offset)
            .unwrap_or(line.len());
    }

    rows
}

#[derive(Debug, Clone, Copy)]
struct WrappedGrapheme {
    start: usize,
    end: usize,
    width: usize,
    is_whitespace: bool,
}

fn wrapped_line_byte_ranges(line: &str, width: usize) -> Vec<Range<usize>> {
    if width == 0 {
        return Vec::new();
    }

    let mut rows = Vec::new();
    let mut pending_line = Vec::new();
    let mut line_width = 0usize;
    let mut pending_word = Vec::new();
    let mut word_width = 0usize;
    let mut pending_whitespace = std::collections::VecDeque::<WrappedGrapheme>::new();
    let mut whitespace_width = 0usize;
    let mut non_whitespace_previous = false;

    for (start, grapheme) in line.grapheme_indices(true) {
        let symbol_width = UnicodeWidthStr::width(grapheme);
        if symbol_width > width {
            continue;
        }

        let grapheme = WrappedGrapheme {
            start,
            end: start + grapheme.len(),
            width: symbol_width,
            is_whitespace: is_wrapping_whitespace(grapheme),
        };
        let word_found = non_whitespace_previous && grapheme.is_whitespace;
        let untrimmed_overflow =
            pending_line.is_empty() && word_width + whitespace_width + grapheme.width > width;

        if word_found || untrimmed_overflow {
            pending_line.extend(pending_whitespace.drain(..));
            line_width += whitespace_width;
            pending_line.append(&mut pending_word);
            line_width += word_width;

            whitespace_width = 0;
            word_width = 0;
        }

        let line_full = line_width >= width;
        let pending_word_overflow =
            grapheme.width > 0 && line_width + whitespace_width + word_width >= width;

        if line_full || pending_word_overflow {
            let mut remaining_width = width.saturating_sub(line_width);
            push_wrapped_range(&mut rows, &pending_line);
            pending_line.clear();
            line_width = 0;

            while let Some(grapheme) = pending_whitespace.front() {
                if grapheme.width > remaining_width {
                    break;
                }

                whitespace_width = whitespace_width.saturating_sub(grapheme.width);
                remaining_width = remaining_width.saturating_sub(grapheme.width);
                pending_whitespace.pop_front();
            }

            if grapheme.is_whitespace && pending_whitespace.is_empty() {
                continue;
            }
        }

        if grapheme.is_whitespace {
            whitespace_width += grapheme.width;
            pending_whitespace.push_back(grapheme);
        } else {
            word_width += grapheme.width;
            pending_word.push(grapheme);
        }

        non_whitespace_previous = !grapheme.is_whitespace;
    }

    pending_line.extend(pending_whitespace);
    pending_line.append(&mut pending_word);
    if pending_line.is_empty() {
        if rows.is_empty() {
            rows.push(0..0);
        }
    } else {
        push_wrapped_range(&mut rows, &pending_line);
    }

    rows
}

fn is_wrapping_whitespace(grapheme: &str) -> bool {
    grapheme == "\u{200b}" || grapheme.chars().all(char::is_whitespace) && grapheme != "\u{00a0}"
}

fn push_wrapped_range(rows: &mut Vec<Range<usize>>, graphemes: &[WrappedGrapheme]) {
    let Some(first) = graphemes.first() else {
        rows.push(0..0);
        return;
    };
    let start = first.start;
    let end = graphemes
        .last()
        .map(|grapheme| grapheme.end)
        .unwrap_or(first.end);
    rows.push(start..end);
}

fn wrapped_row_for_match(ranges: &[Range<usize>], start: usize, end: usize) -> usize {
    ranges
        .iter()
        .position(|range| start < range.end && end > range.start)
        .or_else(|| ranges.iter().position(|range| start <= range.end))
        .unwrap_or_else(|| ranges.len().saturating_sub(1))
}

fn wrapped_line_height(line: &str, width: usize) -> usize {
    wrapped_text_height(&Text::from(Line::from(line.to_owned())), width as u16)
}

fn wrapped_rendered_line_height(line: &Line<'_>, width: usize) -> usize {
    wrapped_text_height(&Text::from(line.clone()), width as u16)
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
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Rect;
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span, Text};
    use tui_input::Input;

    use crate::parse::{Agent, DerivationType, MessageRole, Session, SessionCell, SessionMessage};
    use crate::settings::{DisplayOptions, ThemeName};
    use crate::summary::{Fingerprint, SummarizeBackend, SummarySidecar};
    use crate::tui::preview::render_message_body;
    use crate::tui::preview::SessionRenderOptions;
    use crate::tui::theme::Theme;
    use crate::tui::util::wrapped_text_height;

    use super::{
        collect_line_match_rows, collect_match_rows, collect_message_rows,
        collect_message_rows_with_options, match_scrolloff, message_header_text,
        message_row_for_scroll, next_match_index, previous_match_index, scroll_for_match,
        scroll_progress_percent, viewer_title, MessageDirection, MessageJumpScope, ViewerOutcome,
        ViewerState,
    };

    #[test]
    fn active_match_uses_theme_foreground() {
        let theme = Theme::lazygit();
        let mut text = Text::from(Line::from(vec![
            Span::styled(
                "alpha",
                Style::default()
                    .fg(theme.text)
                    .bg(theme.search_match_bg)
                    .add_modifier(Modifier::ITALIC),
            ),
            Span::styled(" beta", Style::default().fg(theme.text)),
        ]));

        super::highlight_active_match(&mut text, 0, 80, &theme);

        let alpha = &text.lines[0].spans[0];
        assert_eq!(alpha.style.fg, Some(theme.active_match_fg));
        assert_eq!(alpha.style.bg, Some(theme.active_match_bg));
        assert!(alpha.style.add_modifier.contains(Modifier::ITALIC));
        assert!(alpha.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn collect_match_rows_tracks_wrapped_content_lines() {
        let session = sample_session();
        let rows = collect_match_rows(&session, None, &Theme::default(), "alpha", 12);

        assert_eq!(rows, vec![3, 5, 10]);
    }

    #[test]
    fn collect_line_match_rows_follows_word_wrap_boundaries() {
        let terms = vec!["world".to_owned()];
        let rows = collect_line_match_rows("Hello World", 10, &terms);

        assert_eq!(rows, vec![1]);
    }

    #[test]
    fn collect_match_rows_follow_rendered_markdown_instead_of_raw_source() {
        let session = markdown_code_session();
        let rows = collect_match_rows(&session, None, &Theme::default(), "alpha", 80);

        assert_eq!(rows, vec![1]);
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
        assert_eq!(visible_rows, vec![0, 5]);
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
    fn viewer_hints_match_always_focused_search() {
        let keys = ViewerState::HINTS
            .iter()
            .map(|hint| hint.key)
            .collect::<Vec<_>>();

        assert!(!keys.contains(&"/"));
        assert!(!keys.contains(&"n/p"));
        assert!(keys.contains(&"^N/^P"));
        assert!(keys.contains(&"^U/^E"));
    }

    #[test]
    fn summary_leadin_offsets_message_boundaries_and_match_rows() {
        let session = sample_session();
        let summary = sample_summary("alpha summary");

        let match_rows =
            collect_match_rows(&session, Some(&summary), &Theme::default(), "alpha", 80);
        let message_rows = collect_message_rows(
            &session,
            Some(&summary),
            &Theme::default(),
            80,
            MessageJumpScope::Any,
            DisplayOptions::default(),
        );

        assert_eq!(match_rows, vec![2, 6, 9]);
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
            cells: Vec::new(),
            session_info: None,
            lineage: Default::default(),
        }
    }
}
