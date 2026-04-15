use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::panic;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use log::{debug, trace, warn};
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::{Frame, Terminal};
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::fs_safename::{
    validate_windows_filename_component, validate_windows_stem_with_extension,
};
use crate::index::{
    IndexManager, Scope, SearchEngine, SearchFilters, SearchHit, SearchRequest, SortMode,
    SyncOutcome,
};
use crate::parse::claude::read_claude_autosummaries;
use crate::parse::{parse_session_file, Agent, MessageRole, Session};
use crate::scan::AgentHomes;
use crate::settings::{Settings, ThemeName};
use crate::summary::sidecar::sidecar_path;
use crate::summary::staleness::fingerprint as compute_fingerprint;
use crate::summary::{
    AicsSummaryPreview, ClaudeAutosummaryPreview, SummaryCommand, SummaryEvent, SummarySidecar,
    SummarySources, SummaryWorker,
};
use crate::tui::actions::{self, ActionMenuState, ActionOutcome, SessionAction};
use crate::tui::filter::{FilterModalState, FilterOutcome};
use crate::tui::help::{HelpModalState, HelpOutcome, HelpTab};
use crate::tui::profile;
use crate::tui::settings::{SettingsModalState, SettingsOutcome};
use crate::tui::statusline;
use crate::tui::theme::Theme;
use crate::tui::util::{
    block_title, highlight_spans, parse_highlighted_html, session_display_title,
    wrapped_text_height,
};
use crate::tui::viewer::{
    collect_message_rows, message_row_for_scroll, MessageDirection, MessageJumpScope,
    ViewerOutcome, ViewerState,
};
use crate::tui::{keymap_hint, layout, list, preview, search};

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(200);
const SEARCH_POLL_INTERVAL: Duration = Duration::from_millis(50);
const PAGE_STEP: usize = 8;
const LIST_MOUSE_SCROLL_STEP: isize = 1;
const PANEL_MOUSE_SCROLL_STEP: usize = 3;
const PREVIEW_WIDTH_MIN: u16 = 25;
const PREVIEW_WIDTH_MAX: u16 = 75;
const LIST_DOUBLE_CLICK_THRESHOLD: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy)]
struct PendingListClick {
    index: usize,
    at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Search,
    List,
    Preview,
}

#[derive(Debug)]
enum Overlay {
    None,
    Filters(FilterModalState),
    Actions(ActionMenuState),
    Viewer(ViewerState),
    Settings(SettingsModalState),
    ConfirmDelete,
    ConfirmExit,
}

#[derive(Debug, Clone)]
struct ExternalCommand {
    program: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    env: Vec<(String, String)>,
}

struct PreviewRenderCache {
    path: PathBuf,
    query: String,
    width: u16,
    theme_name: ThemeName,
    wrapped_height: Option<usize>,
    text: Text<'static>,
}

pub struct PreviewRenderState<'a> {
    pub text: &'a Text<'static>,
    pub max_scroll: usize,
}

#[derive(Debug)]
enum AppExit {
    Normal,
    Handoff(ExternalCommand),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnippetMode {
    ContentPreview,
    AicsSummary,
    BuiltinSummary,
}

impl Default for SnippetMode {
    fn default() -> Self {
        Self::ContentPreview
    }
}

impl SnippetMode {
    fn label(self) -> &'static str {
        match self {
            Self::ContentPreview => "Content preview",
            Self::AicsSummary => "AICS summary",
            Self::BuiltinSummary => "Builtin summary",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::ContentPreview => Self::AicsSummary,
            Self::AicsSummary => Self::BuiltinSummary,
            Self::BuiltinSummary => Self::ContentPreview,
        }
    }
}

pub fn run_app(
    manager: IndexManager,
    search_engine: SearchEngine,
    initial_request: SearchRequest,
    settings: Settings,
    homes: AgentHomes,
) -> Result<()> {
    let worker = SearchWorker::spawn(search_engine)?;
    let summary_worker = SummaryWorker::spawn()?;
    let mut app = App::new(
        manager,
        worker,
        summary_worker,
        initial_request,
        settings,
        homes,
    );
    match app.run()? {
        AppExit::Normal => Ok(()),
        AppExit::Handoff(command) => execute_handoff(command),
    }
}

struct SearchWorker {
    request_tx: Sender<SearchCommand>,
    response_rx: Receiver<SearchResponse>,
}

#[derive(Debug)]
struct SearchCommand {
    request_id: u64,
    request: SearchRequest,
}

#[derive(Debug)]
struct SearchResponse {
    request_id: u64,
    result: std::result::Result<Vec<SearchHit>, String>,
}

impl SearchWorker {
    fn spawn(search_engine: SearchEngine) -> Result<Self> {
        let (request_tx, request_rx) = mpsc::channel();
        let (response_tx, response_rx) = mpsc::channel();

        thread::Builder::new()
            .name("aics-search".to_owned())
            .spawn(move || search_worker_loop(search_engine, request_rx, response_tx))
            .context("failed to spawn search worker")?;

        Ok(Self {
            request_tx,
            response_rx,
        })
    }
}

pub struct App {
    pub focus: Focus,
    pub query: Input,
    pub results: Vec<SearchHit>,
    pub preview_scroll: usize,
    manager: IndexManager,
    worker: SearchWorker,
    summary_worker: SummaryWorker,
    summary_cache: HashMap<PathBuf, SummarySources>,
    summary_inflight: HashSet<PathBuf>,
    snippet_mode: SnippetMode,
    statusline: Option<statusline::Entry>,
    scope: Scope,
    local_scope: Scope,
    filters: SearchFilters,
    sort: SortMode,
    result_limit: usize,
    selected: usize,
    list_offset: usize,
    preview_cache: HashMap<PathBuf, Option<Session>>,
    hidden_deleted_paths: HashSet<PathBuf>,
    preview_render_cache: Option<PreviewRenderCache>,
    committed_query: String,
    pending_search: bool,
    last_edit_at: Option<Instant>,
    next_search_id: u64,
    latest_search_id: Option<u64>,
    search_in_flight: bool,
    should_quit: bool,
    preview_visible: bool,
    preview_width_pct: u16,
    last_frame_area: Rect,
    last_layout: Option<layout::AppLayout>,
    overlay: Overlay,
    help: Option<HelpModalState>,
    handoff: Option<ExternalCommand>,
    settings: Settings,
    theme: Theme,
    homes: AgentHomes,
    pending_main_menu_action: bool,
    pending_list_click: Option<PendingListClick>,
    pending_action_menu_click: Option<PendingListClick>,
}

impl App {
    const MAIN_HINTS: [keymap_hint::KeymapHint; 10] = [
        keymap_hint::KeymapHint::new("?", "help"),
        keymap_hint::KeymapHint::new("↑↓", "select"),
        keymap_hint::KeymapHint::new("⏎", "actions"),
        keymap_hint::KeymapHint::new("^F", "filters"),
        keymap_hint::KeymapHint::new("^S", "settings"),
        keymap_hint::KeymapHint::new("^Y", "cycle snippet"),
        keymap_hint::KeymapHint::new("^P", "toggle preview"),
        keymap_hint::KeymapHint::new("Esc", "clear/cancel"),
        keymap_hint::KeymapHint::new("^C", "quit"),
        keymap_hint::KeymapHint::new("PgUp/PgDn", "scroll"),
    ];
    const MAIN_MENU_HINTS: [keymap_hint::KeymapHint; 11] = [
        keymap_hint::KeymapHint::new("Esc", "cancel ^x"),
        keymap_hint::KeymapHint::new("v", "view"),
        keymap_hint::KeymapHint::new("s", "summarize"),
        keymap_hint::KeymapHint::new("e", "export"),
        keymap_hint::KeymapHint::new("i", "copy id"),
        keymap_hint::KeymapHint::new("p", "copy path"),
        keymap_hint::KeymapHint::new("o", "copy dir"),
        keymap_hint::KeymapHint::new("d", "delete session"),
        keymap_hint::KeymapHint::new("r", "resume"),
        keymap_hint::KeymapHint::new("f", "fork"),
        keymap_hint::KeymapHint::new("?", "help"),
    ];

    fn new(
        manager: IndexManager,
        worker: SearchWorker,
        summary_worker: SummaryWorker,
        initial_request: SearchRequest,
        settings: Settings,
        homes: AgentHomes,
    ) -> Self {
        let local_scope = match &initial_request.scope {
            Scope::CurrentDir(path, canonical) => {
                Scope::CurrentDir(path.clone(), canonical.clone())
            }
            Scope::Global => env::current_dir()
                .map(Scope::current_dir)
                .unwrap_or(Scope::Global),
        };

        Self {
            focus: Focus::Search,
            query: Input::default().with_value(initial_request.query.clone()),
            results: Vec::new(),
            preview_scroll: 0,
            manager,
            worker,
            summary_worker,
            summary_cache: HashMap::new(),
            summary_inflight: HashSet::new(),
            snippet_mode: SnippetMode::default(),
            statusline: None,
            scope: initial_request.scope,
            local_scope,
            filters: initial_request.filters,
            sort: initial_request.sort,
            result_limit: initial_request.limit.max(1),
            selected: 0,
            list_offset: 0,
            preview_cache: HashMap::new(),
            hidden_deleted_paths: HashSet::new(),
            preview_render_cache: None,
            committed_query: initial_request.query,
            pending_search: true,
            last_edit_at: None,
            next_search_id: 0,
            latest_search_id: None,
            search_in_flight: false,
            should_quit: false,
            preview_visible: settings.show_preview,
            preview_width_pct: settings
                .preview_width_pct
                .clamp(PREVIEW_WIDTH_MIN, PREVIEW_WIDTH_MAX),
            last_frame_area: Rect::default(),
            last_layout: None,
            overlay: Overlay::None,
            help: None,
            handoff: None,
            theme: Theme::from_name(settings.theme),
            settings,
            homes,
            pending_main_menu_action: false,
            pending_list_click: None,
            pending_action_menu_click: None,
        }
    }

    fn run(&mut self) -> Result<AppExit> {
        let mut terminal = setup_terminal()?;
        install_panic_hook();
        let run_result = (|| -> Result<AppExit> {
            self.dispatch_search()?;
            let mut needs_redraw = true;

            while !self.should_quit {
                if self.maybe_dispatch_search()? {
                    needs_redraw = true;
                }
                if self.collect_search_responses()? {
                    needs_redraw = true;
                }
                if self.collect_summary_events() {
                    needs_redraw = true;
                }

                if needs_redraw {
                    let _draw_profile = profile::scope("terminal.draw");
                    terminal.draw(|frame| self.draw(frame))?;
                    needs_redraw = false;
                }

                // Drain all pending events before redrawing so that
                // rapid input (e.g. fast mouse scrolling) doesn't back up
                // the event queue and cause escape-sequence mis-parsing.
                if event::poll(self.poll_timeout())? {
                    while !self.should_quit {
                        match event::read()? {
                            Event::Key(key) if key.kind == KeyEventKind::Press => {
                                self.handle_key(key)?;
                                needs_redraw = true;
                            }
                            Event::Mouse(mouse) => {
                                self.handle_mouse(mouse)?;
                                needs_redraw = true;
                            }
                            Event::Resize(_, _) => needs_redraw = true,
                            _ => {}
                        }
                        if !event::poll(std::time::Duration::ZERO)? {
                            break;
                        }
                    }
                }
            }

            if let Some(command) = self.handoff.take() {
                return Ok(AppExit::Handoff(command));
            }
            Ok(AppExit::Normal)
        })();

        finalize_run_result(run_result, restore_terminal(&mut terminal))
    }

    pub fn scope_label(&self) -> String {
        match &self.scope {
            Scope::Global => "Global".to_owned(),
            Scope::CurrentDir(path, _) => path
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| path.to_string_lossy().into_owned()),
        }
    }

    pub fn selected_index(&self) -> Option<usize> {
        (!self.results.is_empty()).then_some(self.selected)
    }

    pub fn committed_query(&self) -> &str {
        &self.committed_query
    }

    pub fn title_status_text(&self) -> String {
        let truncated = self.results.len() >= self.result_limit;
        let mut text = if truncated {
            format!("{}+ results", self.results.len())
        } else {
            format!("{} results", self.results.len())
        };
        let filter_count = self.filters.active_count();
        if filter_count > 0 {
            text.push_str(&format!(" · {filter_count} filters"));
        }
        if matches!(self.sort, SortMode::Time) {
            text.push_str(" · time sort");
        }
        if !matches!(self.snippet_mode, SnippetMode::ContentPreview) {
            text.push_str(" · ");
            text.push_str(self.snippet_mode.label());
        }
        let inflight = self.summary_inflight.len();
        if inflight > 0 {
            text.push_str(&format!(" · summarizing {inflight}"));
        }
        if let Some(entry) = self.statusline.as_ref() {
            if !entry.expired() {
                text.push_str(" · ");
                text.push_str(&entry.label);
            }
        }
        text
    }

    pub fn is_searching(&self) -> bool {
        self.pending_search || self.search_in_flight
    }

    pub fn show_search_cursor(&self) -> bool {
        matches!(self.overlay, Overlay::None)
    }

    pub fn preview_title(&self) -> &'static str {
        "Preview"
    }

    pub fn selected_preview(&mut self) -> Option<&Session> {
        let hit = self.results.get(self.selected)?;
        let path = hit.session.file_path.clone();

        if !self.preview_cache.contains_key(&path) {
            let parsed = parse_session_file(hit.session.agent, &path).ok().flatten();
            self.preview_cache.insert(path.clone(), parsed);
        }

        self.preview_cache
            .get(&path)
            .and_then(|session| session.as_ref())
    }

    pub fn preview_render_state<'a>(
        &'a mut self,
        area: Rect,
        theme: &Theme,
    ) -> Option<PreviewRenderState<'a>> {
        let hit = self.results.get(self.selected)?;
        let agent = hit.session.agent;
        let path = hit.session.file_path.clone();
        let query = self.committed_query.clone();
        let width = area.width.saturating_sub(2);
        let theme_name = self.current_frame_theme_name();

        let cache_miss = self.preview_render_cache.as_ref().is_none_or(|cache| {
            cache.path != path
                || cache.query != query
                || cache.width != width
                || cache.theme_name != theme_name
        });
        if cache_miss {
            profile::event("preview.cache.miss");
            let highlight_query = (!query.is_empty()).then_some(query.as_str());
            let session = self.selected_preview().cloned();
            self.ensure_summary_cache(agent, &path);
            let text = preview::render_composite_text(
                session.as_ref(),
                self.summary_cache.get(&path)?,
                theme,
                highlight_query,
                self.summary_inflight.contains(&path),
            );
            self.preview_render_cache = Some(PreviewRenderCache {
                path: path.clone(),
                query: query.clone(),
                width,
                theme_name,
                wrapped_height: None,
                text,
            });
        } else {
            profile::event("preview.cache.hit");
        }

        if self.preview_scroll > 0
            && self
                .preview_render_cache
                .as_ref()
                .is_some_and(|cache| cache.wrapped_height.is_none())
        {
            if let Some(cache) = self.preview_render_cache.as_mut() {
                cache.wrapped_height = Some(wrapped_text_height(&cache.text, cache.width));
            }
        }

        let cache = self.preview_render_cache.as_ref()?;
        let max_scroll = if self.preview_scroll == 0 {
            0
        } else {
            let viewport_height = area.height.saturating_sub(2) as usize;
            cache
                .wrapped_height
                .unwrap_or_default()
                .saturating_sub(viewport_height)
        };
        Some(PreviewRenderState {
            text: &cache.text,
            max_scroll,
        })
    }

    pub fn list_window(&self, max_items: usize) -> (&[SearchHit], Option<usize>) {
        if self.results.is_empty() {
            return (&[], None);
        }

        let max_items = max_items.max(1);
        let max_offset = self.results.len().saturating_sub(max_items);
        let offset = self.list_offset.min(max_offset);
        let end = (offset + max_items).min(self.results.len());
        (&self.results[offset..end], Some(self.selected - offset))
    }

    pub fn list_snippet_line(
        &mut self,
        hit: &SearchHit,
        theme: &Theme,
    ) -> Line<'static> {
        if let Some(snippet) = self.active_summary_snippet_text(hit) {
            return Line::from(highlight_spans(
                &snippet,
                &self.committed_query,
                Style::default().fg(theme.text),
                theme.search_match_style(),
            ));
        }

        parse_highlighted_html(
            &hit.snippet_html,
            Style::default().fg(theme.text),
            theme.search_match_style(),
        )
    }

    fn active_summary_snippet_text(&mut self, hit: &SearchHit) -> Option<String> {
        let path = hit.session.file_path.clone();
        self.ensure_summary_cache(hit.session.agent, &path);
        let sources = self.summary_cache.get(&path)?;
        let text = match self.snippet_mode {
            SnippetMode::ContentPreview => return None,
            SnippetMode::AicsSummary => sources
                .aics_sidecar
                .as_ref()
                .map(|summary| summary.sidecar.body.trim().to_owned())
                .or_else(|| {
                    sources
                        .latest_claude_autosummary()
                        .map(|summary| summary.body.trim().to_owned())
                }),
            SnippetMode::BuiltinSummary => sources
                .latest_claude_autosummary()
                .map(|summary| summary.body.trim().to_owned())
                .or_else(|| {
                    sources
                        .aics_sidecar
                        .as_ref()
                        .map(|summary| summary.sidecar.body.trim().to_owned())
                }),
        }?;

        (!text.is_empty()).then_some(text)
    }

    fn draw(&mut self, frame: &mut Frame) {
        let _draw_profile = profile::scope("app.draw");
        let theme = self.current_frame_theme();
        frame.render_widget(Clear, frame.area());
        self.last_frame_area = frame.area();
        let area = frame.area();
        if area.height < 5 || area.width < 20 {
            let msg = Paragraph::new("Terminal too small").style(Style::default().fg(theme.muted));
            frame.render_widget(msg, area);
            return;
        }
        let areas = layout::split(area, self.preview_width_pct, self.preview_visible);
        self.last_layout = Some(areas);
        self.clamp_scroll_state(areas);
        search::render(frame, self, areas.search, &theme);
        let session_separator = self.settings.session_separator.clone();
        let snippet_line_count = self.settings.snippet_line_count;
        list::render(
            frame,
            self,
            areas.list,
            &theme,
            &session_separator,
            snippet_line_count,
        );

        if let Some(preview_area) = areas.preview {
            preview::render(frame, self, preview_area, &theme);
        }

        let hints = if self.pending_main_menu_action {
            &Self::MAIN_MENU_HINTS[..]
        } else {
            &Self::MAIN_HINTS[..]
        };
        keymap_hint::render(frame, areas.status, hints, &theme, "");

        let local_scope_label = self.local_scope_label();
        let viewer_theme_name = self.current_frame_theme_name();
        let selected = self.selected;
        let viewer_summary_target = self
            .results
            .get(selected)
            .map(|hit| (hit.session.agent, hit.session.file_path.clone()));
        let viewer_summary = viewer_summary_target
            .and_then(|(agent, path)| self.summary_sidecar_for_path(agent, &path));
        let results = &self.results;
        let preview_cache = &self.preview_cache;
        match &mut self.overlay {
            Overlay::None => {}
            Overlay::Filters(filter_state) => {
                filter_state.render(frame, frame.area(), &theme, &local_scope_label);
            }
            Overlay::Actions(action_menu) => action_menu.render(frame, frame.area(), &theme),
            Overlay::Viewer(viewer_state) => {
                let session = results
                    .get(selected)
                    .and_then(|hit| preview_cache.get(&hit.session.file_path))
                    .and_then(|session| session.as_ref());
                if let Some(session) = session {
                    viewer_state.render(
                        frame,
                        frame.area(),
                        session,
                        viewer_summary.as_ref(),
                        &theme,
                        viewer_theme_name,
                    );
                }
            }
            Overlay::Settings(settings_state) => settings_state.render(frame, frame.area(), &theme),
            Overlay::ConfirmDelete => self.render_delete_confirm(frame, frame.area(), &theme),
            Overlay::ConfirmExit => self.render_exit_confirm(frame, frame.area(), &theme),
        }
        if let Some(help_state) = &mut self.help {
            help_state.render(frame, frame.area(), &theme);
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.request_quit();
            return Ok(());
        }

        if self.help.is_some() {
            return self.handle_help_key(key);
        }

        if !matches!(self.overlay, Overlay::None) {
            return self.handle_overlay_key(key);
        }

        if self.handle_pending_main_menu_key(key)? {
            return Ok(());
        }

        if is_help_key(key) {
            self.open_help(HelpTab::SessionList);
            return Ok(());
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('x') {
            self.pending_main_menu_action = self.selected_index().is_some();
            return Ok(());
        }

        match self.focus {
            Focus::Search => self.handle_search_key(key),
            Focus::List => self.handle_list_key(key),
            Focus::Preview => self.handle_preview_key(key),
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => {
                if self.query.value().is_empty() {
                    self.request_quit();
                } else {
                    self.clear_query();
                }
            }
            KeyCode::Down if key.modifiers == (KeyModifiers::CONTROL | KeyModifiers::SHIFT) => {
                if self.preview_available() {
                    self.jump_preview_message(MessageDirection::Next, MessageJumpScope::UserOnly);
                }
            }
            KeyCode::Up if key.modifiers == (KeyModifiers::CONTROL | KeyModifiers::SHIFT) => {
                if self.preview_available() {
                    self.jump_preview_message(
                        MessageDirection::Previous,
                        MessageJumpScope::UserOnly,
                    );
                }
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_settings()
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_filters()
            }
            KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_scope()?
            }
            KeyCode::Down if key.modifiers == KeyModifiers::SHIFT => {
                if self.preview_available() {
                    self.jump_preview_message(MessageDirection::Next, MessageJumpScope::Any);
                }
            }
            KeyCode::Up if key.modifiers == KeyModifiers::SHIFT => {
                if self.preview_available() {
                    self.jump_preview_message(MessageDirection::Previous, MessageJumpScope::Any);
                }
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_selection(1)
            }
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_selection(-1)
            }
            KeyCode::Down => self.move_selection(1),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::PageDown => {
                if self.preview_available() {
                    self.preview_scroll = self.preview_scroll.saturating_add(PAGE_STEP);
                } else {
                    self.move_selection(PAGE_STEP as isize);
                }
            }
            KeyCode::PageUp => {
                if self.preview_available() {
                    self.preview_scroll = self.preview_scroll.saturating_sub(PAGE_STEP);
                } else {
                    self.move_selection(-(PAGE_STEP as isize));
                }
            }
            KeyCode::Home => {
                if self.preview_available() {
                    self.preview_scroll = 0;
                } else {
                    self.select_absolute(0);
                }
            }
            KeyCode::End => {
                if self.preview_available() {
                    self.preview_scroll = usize::MAX / 4;
                } else {
                    self.select_absolute(self.results.len().saturating_sub(1));
                }
            }
            KeyCode::Enter => {
                if self.selected_index().is_some() {
                    self.overlay = Overlay::Actions(ActionMenuState::new());
                }
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_preview()
            }
            KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cycle_snippet_source()
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.resize_preview(-5)
            }
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.resize_preview(5)
            }
            _ => {
                let before = self.query.value().to_owned();
                if self.query.handle_event(&Event::Key(key)).is_some()
                    && self.query.value() != before
                {
                    self.pending_search = true;
                    self.last_edit_at = Some(Instant::now());
                }
            }
        }
        Ok(())
    }

    fn handle_list_key(&mut self, key: KeyEvent) -> Result<()> {
        self.handle_search_key(key)
    }

    fn handle_preview_key(&mut self, key: KeyEvent) -> Result<()> {
        self.handle_search_key(key)
    }

    fn handle_overlay_key(&mut self, key: KeyEvent) -> Result<()> {
        if matches!(self.overlay, Overlay::Viewer(_)) && is_help_key(key) {
            self.open_help(HelpTab::Viewer);
            return Ok(());
        }

        let viewer_area = self.last_frame_area;
        let viewer_theme_name = self.current_frame_theme_name();
        let viewer_session = if matches!(self.overlay, Overlay::Viewer(_)) {
            self.selected_preview().cloned()
        } else {
            None
        };
        let viewer_summary = if let Some(session) = viewer_session.as_ref() {
            self.summary_sidecar_for_path(session.agent, &session.file_path)
        } else {
            None
        };

        match &mut self.overlay {
            Overlay::Filters(state) => match state.handle_key(key, &self.local_scope)? {
                FilterOutcome::Stay => {}
                FilterOutcome::Close => self.overlay = Overlay::None,
                FilterOutcome::Apply(update) => {
                    self.scope = update.scope;
                    self.filters = update.filters;
                    self.sort = update.sort;
                    self.overlay = Overlay::None;
                    self.trigger_search_now()?;
                }
            },
            Overlay::Actions(state) => match state.handle_key(key) {
                ActionOutcome::Stay => {}
                ActionOutcome::Close => {
                    self.pending_action_menu_click = None;
                    self.overlay = Overlay::None;
                }
                ActionOutcome::Run(action) => {
                    self.pending_action_menu_click = None;
                    self.overlay = Overlay::None;
                    if let Err(error) = self.run_session_action(action) {
                        self.statusline = Some(statusline::Entry::failed(format!("{error:#}")));
                    }
                }
            },
            Overlay::Viewer(state) => {
                match state.handle_key(
                    key,
                    viewer_area,
                    viewer_session.as_ref(),
                    viewer_summary.as_ref(),
                    &self.theme,
                    viewer_theme_name,
                ) {
                    ViewerOutcome::Stay => {}
                    ViewerOutcome::Close => self.overlay = Overlay::None,
                }
            }
            Overlay::Settings(state) => match state.handle_key(key) {
                SettingsOutcome::Stay => {}
                SettingsOutcome::Close => self.overlay = Overlay::None,
                SettingsOutcome::Apply(new_settings) => {
                    self.apply_settings(new_settings)?;
                    self.overlay = Overlay::None;
                }
            },
            Overlay::ConfirmDelete => match key.code {
                KeyCode::Esc | KeyCode::Char('n') => self.overlay = Overlay::None,
                KeyCode::Char('y') | KeyCode::Enter => {
                    self.overlay = Overlay::None;
                    if let Err(error) = self.delete_selected_session() {
                        self.statusline = Some(statusline::Entry::failed(format!("{error:#}")));
                    }
                }
                _ => {}
            },
            Overlay::ConfirmExit => match key.code {
                KeyCode::Esc | KeyCode::Char('n') => self.overlay = Overlay::None,
                KeyCode::Char('y') | KeyCode::Enter => {
                    self.overlay = Overlay::None;
                    self.should_quit = true;
                }
                _ => {}
            },
            Overlay::None => {}
        }
        Ok(())
    }

    fn request_quit(&mut self) {
        if self.summary_inflight.is_empty() {
            self.should_quit = true;
        } else {
            self.overlay = Overlay::ConfirmExit;
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        if self.help.is_some() {
            return Ok(());
        }

        if !matches!(self.overlay, Overlay::None) {
            return self.handle_overlay_mouse(mouse);
        }

        let Some(areas) = self.last_layout else {
            return Ok(());
        };

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if contains(areas.list, mouse.column, mouse.row) {
                    if let Some(index) = self.list_index_at(areas.list, mouse.row) {
                        self.select_absolute(index);
                        let now = Instant::now();
                        let double_click = self.pending_list_click.is_some_and(|click| {
                            click.index == index
                                && now.duration_since(click.at) <= LIST_DOUBLE_CLICK_THRESHOLD
                        });
                        if double_click {
                            self.pending_list_click = None;
                            self.open_viewer();
                        } else {
                            self.pending_list_click = Some(PendingListClick { index, at: now });
                        }
                    }
                } else {
                    self.pending_list_click = None;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {}
            MouseEventKind::ScrollDown => {
                self.pending_list_click = None;
                if contains(areas.list, mouse.column, mouse.row) {
                    self.move_selection(LIST_MOUSE_SCROLL_STEP);
                } else if areas
                    .preview
                    .is_some_and(|preview| contains(preview, mouse.column, mouse.row))
                {
                    self.preview_scroll =
                        self.preview_scroll.saturating_add(PANEL_MOUSE_SCROLL_STEP);
                }
            }
            MouseEventKind::ScrollUp => {
                self.pending_list_click = None;
                if contains(areas.list, mouse.column, mouse.row) {
                    self.move_selection(-LIST_MOUSE_SCROLL_STEP);
                } else if areas
                    .preview
                    .is_some_and(|preview| contains(preview, mouse.column, mouse.row))
                {
                    self.preview_scroll =
                        self.preview_scroll.saturating_sub(PANEL_MOUSE_SCROLL_STEP);
                }
            }
            _ => {
                self.pending_list_click = None;
            }
        }
        Ok(())
    }

    fn handle_overlay_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        match &mut self.overlay {
            Overlay::Viewer(state) => {
                let body = ViewerState::body_area(self.last_frame_area);
                if contains(body, mouse.column, mouse.row) {
                    match mouse.kind {
                        MouseEventKind::ScrollDown => {
                            state.scroll = state.scroll.saturating_add(PANEL_MOUSE_SCROLL_STEP);
                        }
                        MouseEventKind::ScrollUp => {
                            state.scroll = state.scroll.saturating_sub(PANEL_MOUSE_SCROLL_STEP);
                        }
                        _ => {}
                    }
                }
            }
            Overlay::Actions(state) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let mut action_to_run = None;
                    let mut close_overlay = false;
                    if let Some(index) = actions::index_at_row(self.last_frame_area, mouse.row) {
                        state.select_index(index);
                        let now = Instant::now();
                        let double_click = self.pending_action_menu_click.is_some_and(|click| {
                            click.index == index
                                && now.duration_since(click.at) <= LIST_DOUBLE_CLICK_THRESHOLD
                        });
                        if double_click {
                            self.pending_action_menu_click = None;
                            close_overlay = true;
                            action_to_run = actions::action_at(index);
                        } else {
                            self.pending_action_menu_click =
                                Some(PendingListClick { index, at: now });
                        }
                    } else {
                        self.pending_action_menu_click = None;
                    }
                    if close_overlay {
                        self.overlay = Overlay::None;
                    }
                    if let Some(action) = action_to_run {
                        if let Err(error) = self.run_session_action(action) {
                            self.statusline = Some(statusline::Entry::failed(format!("{error:#}")));
                        }
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {}
                _ => {
                    self.pending_action_menu_click = None;
                }
            },
            _ => {}
        }
        Ok(())
    }

    fn maybe_dispatch_search(&mut self) -> Result<bool> {
        if self.pending_search
            && self
                .last_edit_at
                .is_some_and(|instant| instant.elapsed() >= SEARCH_DEBOUNCE)
        {
            self.dispatch_search()?;
            return Ok(true);
        }
        Ok(false)
    }

    fn dispatch_search(&mut self) -> Result<()> {
        let request_id = self.next_search_id;
        self.next_search_id = self.next_search_id.saturating_add(1);
        self.committed_query = self.query.value().to_owned();
        debug!(
            "dispatch_search id={} query={:?} scope={:?} sort={:?} limit={} current_results={} hidden_deleted_paths={}",
            request_id,
            self.committed_query,
            self.scope,
            self.sort,
            self.result_limit.max(1),
            self.results.len(),
            self.hidden_deleted_paths.len()
        );
        self.worker
            .request_tx
            .send(SearchCommand {
                request_id,
                request: SearchRequest {
                    query: self.committed_query.clone(),
                    scope: self.scope.clone(),
                    limit: self.result_limit.max(1),
                    sort: self.sort,
                    filters: self.filters.clone(),
                },
            })
            .map_err(|_| anyhow::anyhow!("search worker exited unexpectedly"))?;

        self.latest_search_id = Some(request_id);
        self.search_in_flight = true;
        self.pending_search = false;
        self.last_edit_at = None;
        Ok(())
    }

    fn collect_search_responses(&mut self) -> Result<bool> {
        let mut changed = false;

        loop {
            match self.worker.response_rx.try_recv() {
                Ok(response) => {
                    trace!(
                        "collect_search_responses received id={} latest_search_id={:?}",
                        response.request_id,
                        self.latest_search_id
                    );
                    if Some(response.request_id) != self.latest_search_id {
                        debug!(
                            "ignoring stale search response id={} latest_search_id={:?}",
                            response.request_id, self.latest_search_id
                        );
                        continue;
                    }

                    self.search_in_flight = false;
                    match response.result {
                        Ok(results) => {
                            let raw_count = results.len();
                            let mut hidden_still_present = HashSet::new();
                            let mut dropped_missing = 0usize;
                            let mut dropped_hidden = 0usize;
                            let mut next_results = Vec::with_capacity(raw_count);
                            for result in results {
                                if !result.session.file_path.exists() {
                                    dropped_missing += 1;
                                    trace!(
                                        "dropping missing-file hit id={} path={}",
                                        response.request_id,
                                        result.session.file_path.display()
                                    );
                                    continue;
                                }
                                if self
                                    .hidden_deleted_paths
                                    .contains(&result.session.file_path)
                                {
                                    dropped_hidden += 1;
                                    hidden_still_present.insert(result.session.file_path.clone());
                                    trace!(
                                        "dropping hidden-deleted hit id={} path={}",
                                        response.request_id,
                                        result.session.file_path.display()
                                    );
                                    continue;
                                }
                                next_results.push(result);
                            }
                            self.results = next_results;
                            self.hidden_deleted_paths
                                .retain(|path| hidden_still_present.contains(path));
                            debug!(
                                "applied search response id={} raw_results={} kept={} dropped_missing={} dropped_hidden={} hidden_deleted_paths_remaining={} first_result={}",
                                response.request_id,
                                raw_count,
                                self.results.len(),
                                dropped_missing,
                                dropped_hidden,
                                self.hidden_deleted_paths.len(),
                                self.results
                                    .first()
                                    .map(|hit| hit.session.file_path.display().to_string())
                                    .unwrap_or_else(|| "<none>".to_owned())
                            );
                            self.preview_render_cache = None;
                            if self.results.is_empty() {
                                self.selected = 0;
                                self.list_offset = 0;
                            } else {
                                self.selected =
                                    self.selected.min(self.results.len().saturating_sub(1));
                                self.ensure_selection_visible();
                            }
                            self.preview_scroll = 0;
                        }
                        Err(error) => {
                            debug!(
                                "search response id={} failed: {}",
                                response.request_id, error
                            );
                            self.statusline = Some(statusline::Entry::failed(error));
                        }
                    }
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err(anyhow::anyhow!("search worker exited unexpectedly"));
                }
            }
        }

        Ok(changed)
    }

    fn poll_timeout(&self) -> Duration {
        if self.pending_search {
            if let Some(last_edit_at) = self.last_edit_at {
                return SEARCH_DEBOUNCE.saturating_sub(last_edit_at.elapsed());
            }
        }
        if self.search_in_flight {
            return SEARCH_POLL_INTERVAL;
        }
        if !self.summary_inflight.is_empty()
            || self
                .statusline
                .as_ref()
                .is_some_and(|entry| !entry.expired())
        {
            return SEARCH_POLL_INTERVAL;
        }
        Duration::from_secs(5)
    }

    fn move_selection(&mut self, delta: isize) {
        if self.results.is_empty() {
            return;
        }
        let max_index = self.results.len().saturating_sub(1) as isize;
        let next = (self.selected as isize + delta).clamp(0, max_index);
        if next as usize != self.selected {
            self.selected = next as usize;
            self.ensure_selection_visible();
            self.preview_scroll = 0;
        }
    }

    fn select_absolute(&mut self, index: usize) {
        if self.results.is_empty() {
            return;
        }
        self.selected = index.min(self.results.len().saturating_sub(1));
        self.ensure_selection_visible();
        self.preview_scroll = 0;
    }

    fn list_index_at(&self, area: Rect, row: u16) -> Option<usize> {
        let sep = &self.settings.session_separator;
        let snip = self.settings.snippet_line_count;
        let slot = list::slot_at_row(area, row, snip, sep)?;
        let visible_slots = list::visible_slots(area, snip, sep);
        let offset = self.list_offset(visible_slots);
        let index = offset + slot;
        (index < self.results.len()).then_some(index)
    }

    fn list_offset(&self, max_items: usize) -> usize {
        let max_items = max_items.max(1);
        let max_offset = self.results.len().saturating_sub(max_items);
        self.list_offset.min(max_offset)
    }

    fn ensure_selection_visible(&mut self) {
        let visible_slots = self
            .last_layout
            .map(|layout| {
                list::visible_slots(
                    layout.list,
                    self.settings.snippet_line_count,
                    &self.settings.session_separator,
                )
            })
            .unwrap_or(1)
            .max(1);

        let max_offset = self.results.len().saturating_sub(visible_slots);
        if self.selected < self.list_offset {
            self.list_offset = self.selected;
        } else {
            let bottom = self.list_offset + visible_slots;
            if self.selected >= bottom {
                self.list_offset = self
                    .selected
                    .saturating_sub(visible_slots.saturating_sub(1));
            }
        }
        self.list_offset = self.list_offset.min(max_offset);
    }

    fn preview_available(&self) -> bool {
        self.last_layout.map_or(false, |l| l.preview.is_some())
    }

    fn jump_preview_message(&mut self, direction: MessageDirection, scope: MessageJumpScope) {
        let Some(layout) = self.last_layout else {
            return;
        };
        let Some(preview_area) = layout.preview else {
            return;
        };
        let width = preview_area.width.saturating_sub(2);
        let theme = self.current_frame_theme();
        let rows = {
            let Some(hit) = self.selected_hit() else {
                return;
            };
            let Some(session) = self.selected_preview().cloned() else {
                return;
            };
            self.ensure_summary_cache(hit.session.agent, &hit.session.file_path);
            let summary_offset = self
                .summary_cache
                .get(&hit.session.file_path)
                .map(|sources| {
                    let summary_text = preview::render_summary_sections(
                        sources,
                        &theme,
                        None,
                        self.summary_inflight.contains(&hit.session.file_path),
                    );
                    if summary_text.lines.is_empty() {
                        0
                    } else {
                        wrapped_text_height(&summary_text, width)
                    }
                })
                .unwrap_or(0);
            collect_message_rows(&session, None, &theme, width, scope)
                .into_iter()
                .map(|row| row + summary_offset)
                .collect::<Vec<_>>()
        };
        if let Some(target) = message_row_for_scroll(&rows, self.preview_scroll, direction) {
            let moved = match direction {
                MessageDirection::Next => target > self.preview_scroll,
                MessageDirection::Previous => target < self.preview_scroll,
            };
            if moved {
                self.preview_scroll = target;
            }
        }
    }

    fn trigger_search_now(&mut self) -> Result<()> {
        self.pending_search = false;
        self.last_edit_at = None;
        self.dispatch_search()
    }

    fn cancel_pending_searches(&mut self) {
        debug!(
            "cancel_pending_searches latest_search_id={:?} next_search_id={} pending_search={} search_in_flight={}",
            self.latest_search_id,
            self.next_search_id,
            self.pending_search,
            self.search_in_flight
        );
        self.latest_search_id = Some(self.next_search_id);
        self.next_search_id = self.next_search_id.saturating_add(1);
        self.pending_search = false;
        self.last_edit_at = None;
        self.search_in_flight = false;
    }

    fn open_settings(&mut self) {
        self.overlay = Overlay::Settings(SettingsModalState::new(&self.settings));
    }

    fn current_frame_theme_name(&self) -> ThemeName {
        match &self.overlay {
            Overlay::Settings(state) => state.current_theme(),
            _ => self.settings.theme,
        }
    }

    fn current_frame_theme(&self) -> Theme {
        Theme::from_name(self.current_frame_theme_name())
    }

    fn apply_settings(&mut self, new_settings: Settings) -> Result<()> {
        self.theme = Theme::from_name(new_settings.theme);
        self.preview_render_cache = None;
        self.settings = new_settings;
        if let Err(err) = self.settings.save() {
            self.statusline = Some(statusline::Entry::failed(format!(
                "settings error: {err:#}"
            )));
        } else {
            self.statusline = Some(statusline::Entry::completed("settings saved"));
        }
        Ok(())
    }

    fn open_filters(&mut self) {
        self.overlay =
            Overlay::Filters(FilterModalState::new(&self.scope, &self.filters, self.sort));
    }

    fn toggle_scope(&mut self) -> Result<()> {
        self.scope = match &self.scope {
            Scope::Global => self.local_scope.clone(),
            Scope::CurrentDir(..) => Scope::Global,
        };
        self.trigger_search_now()
    }

    fn open_viewer(&mut self) {
        if self.selected_index().is_some() {
            let _ = self.selected_preview();
            self.pending_main_menu_action = false;
            self.pending_list_click = None;
            self.pending_action_menu_click = None;
            self.overlay = Overlay::Viewer(ViewerState::with_search(&self.committed_query));
        }
    }

    fn handle_pending_main_menu_key(&mut self, key: KeyEvent) -> Result<bool> {
        if !self.pending_main_menu_action {
            return Ok(false);
        }

        match key.code {
            KeyCode::Esc => {
                self.pending_main_menu_action = false;
                Ok(true)
            }
            KeyCode::Char('?') if key.modifiers.is_empty() => {
                self.pending_main_menu_action = false;
                self.open_help(HelpTab::SessionList);
                Ok(true)
            }
            KeyCode::Char(ch)
                if key.modifiers.is_empty()
                    || (key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.modifiers.difference(KeyModifiers::CONTROL).is_empty()) =>
            {
                if let Some(action) = actions::action_for_key(ch) {
                    self.pending_main_menu_action = false;
                    self.run_session_action(action)?;
                    return Ok(true);
                }
                self.pending_main_menu_action = false;
                Ok(false)
            }
            _ => {
                self.pending_main_menu_action = false;
                Ok(false)
            }
        }
    }

    fn open_help(&mut self, tab: HelpTab) {
        self.help = Some(HelpModalState::new(tab));
    }

    fn handle_help_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(help_state) = &mut self.help else {
            return Ok(());
        };
        if matches!(help_state.handle_key(key), HelpOutcome::Close) {
            self.help = None;
        }
        Ok(())
    }

    fn clear_query(&mut self) {
        if self.query.value().is_empty() {
            return;
        }

        self.query = Input::default();
        self.pending_search = true;
        self.last_edit_at = Some(Instant::now());
    }

    fn toggle_preview(&mut self) {
        self.preview_visible = !self.preview_visible;
        self.preview_render_cache = None;
        self.save_layout_prefs();
    }

    fn cycle_snippet_source(&mut self) {
        self.snippet_mode = self.snippet_mode.next();
        self.preview_scroll = 0;
    }

    fn ensure_summary_cache(&mut self, agent: Agent, path: &Path) {
        if self.summary_cache.contains_key(path) {
            return;
        }
        let entry = load_summary_sources(agent, path);
        self.summary_cache.insert(path.to_path_buf(), entry);
    }

    fn summary_sidecar_for_path(&mut self, agent: Agent, path: &Path) -> Option<SummarySidecar> {
        self.ensure_summary_cache(agent, path);
        self.summary_cache
            .get(path)
            .and_then(|sources| sources.aics_sidecar.as_ref())
            .map(|summary| summary.sidecar.clone())
    }

    fn invalidate_summary_cache(&mut self, path: &Path) {
        self.summary_cache.remove(path);
        if self
            .preview_render_cache
            .as_ref()
            .is_some_and(|cache| cache.path == path)
        {
            self.preview_render_cache = None;
        }
    }

    fn dispatch_summarize(&mut self, hit: &SearchHit) -> Result<()> {
        let path = hit.session.file_path.clone();
        if self.summary_inflight.contains(&path) {
            return Ok(());
        }

        let template = self.settings.summarize_command.clone();
        if template.trim().is_empty() {
            self.statusline = Some(statusline::Entry::failed(
                "Summarize command missing. Configure it in Settings.".to_owned(),
            ));
            return Ok(());
        }

        let command = SummaryCommand {
            jsonl_path: path.clone(),
            backend: crate::summary::SummarizeBackend::Custom,
            command_template: template,
            prompt_template: self.settings.summarize_prompt.clone(),
            claude_command: self.settings.claude_command.clone(),
            claude_args: self.settings.claude_args.clone(),
            codex_command: self.settings.codex_command.clone(),
            codex_args: self.settings.codex_args.clone(),
        };
        self.summary_worker.send(command)?;
        self.summary_inflight.insert(path);
        Ok(())
    }

    fn collect_summary_events(&mut self) -> bool {
        let mut changed = false;
        while let Some(event) = self.summary_worker.try_recv() {
            changed = true;
            match event {
                SummaryEvent::Started { path } => {
                    debug!("summary started: {}", path.display());
                }
                SummaryEvent::Completed { path, sidecar_path } => {
                    self.summary_inflight.remove(&path);
                    self.invalidate_summary_cache(&path);
                    self.statusline = Some(statusline::Entry::completed(format!(
                        "summarized {}",
                        file_label(&path)
                    )));
                    debug!(
                        "summary completed: {} (sidecar {})",
                        path.display(),
                        sidecar_path.display()
                    );
                }
                SummaryEvent::Failed { path, error } => {
                    self.summary_inflight.remove(&path);
                    self.statusline = Some(statusline::Entry::failed(format!(
                        "summary failed for {}: {error}",
                        file_label(&path)
                    )));
                }
            }
        }
        // Clear expired terminal entries so the statusline reclaims its row.
        if self
            .statusline
            .as_ref()
            .is_some_and(|entry| entry.expired())
        {
            self.statusline = None;
            changed = true;
        }
        changed
    }

    fn resize_preview(&mut self, delta: i16) {
        let next = (self.preview_width_pct as i16 + delta)
            .clamp(PREVIEW_WIDTH_MIN as i16, PREVIEW_WIDTH_MAX as i16);
        self.preview_width_pct = next as u16;
        self.save_layout_prefs();
    }

    fn save_layout_prefs(&mut self) {
        self.settings.show_preview = self.preview_visible;
        self.settings.preview_width_pct = self.preview_width_pct;
        if let Err(err) = self.settings.save() {
            self.statusline = Some(statusline::Entry::failed(format!(
                "settings error: {err:#}"
            )));
        }
    }

    fn clamp_scroll_state(&mut self, areas: layout::AppLayout) {
        if areas.preview.is_none() {
            self.preview_scroll = 0;
        }

        if !matches!(self.overlay, Overlay::Viewer(_)) {
            return;
        }

        let theme = self.current_frame_theme();
        let frame_area = self.last_frame_area;
        let theme_name = self.current_frame_theme_name();
        let selected = self.selected;
        let viewer_summary_target = self
            .results
            .get(selected)
            .map(|hit| (hit.session.agent, hit.session.file_path.clone()));
        let viewer_summary = viewer_summary_target
            .and_then(|(agent, path)| self.summary_sidecar_for_path(agent, &path));
        let viewer_session = self
            .results
            .get(selected)
            .and_then(|hit| self.preview_cache.get(&hit.session.file_path))
            .and_then(|session| session.as_ref());
        if let (Overlay::Viewer(state), Some(session)) = (&mut self.overlay, viewer_session) {
            let max_scroll =
                state.max_scroll(frame_area, session, viewer_summary.as_ref(), &theme, theme_name);
            state.scroll = state.scroll.min(max_scroll);
        }
    }

    fn selected_hit(&self) -> Option<SearchHit> {
        self.results.get(self.selected).cloned()
    }

    fn local_scope_label(&self) -> String {
        match &self.local_scope {
            Scope::Global => "Global".to_owned(),
            Scope::CurrentDir(path, _) => path
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| path.to_string_lossy().into_owned()),
        }
    }

    fn run_session_action(&mut self, action: SessionAction) -> Result<()> {
        match action {
            SessionAction::View => self.open_viewer(),
            SessionAction::Summarize => {
                let Some(hit) = self.selected_hit() else {
                    return Ok(());
                };
                self.dispatch_summarize(&hit)?;
            }
            SessionAction::Export => self.export_selected_session()?,
            SessionAction::CopyId => {
                let Some(hit) = self.selected_hit() else {
                    return Ok(());
                };
                self.copy_to_clipboard(&hit.session.session_id, "session id")?;
            }
            SessionAction::CopyPath => {
                let Some(hit) = self.selected_hit() else {
                    return Ok(());
                };
                self.copy_to_clipboard(
                    &hit.session.file_path.display().to_string(),
                    "session path",
                )?;
            }
            SessionAction::CopyDir => {
                let Some(hit) = self.selected_hit() else {
                    return Ok(());
                };
                let value = hit
                    .session
                    .file_path
                    .parent()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| hit.session.file_path.display().to_string());
                self.copy_to_clipboard(&value, "session directory")?;
            }
            SessionAction::Delete => self.overlay = Overlay::ConfirmDelete,
            SessionAction::Resume => {
                let Some(hit) = self.selected_hit() else {
                    return Ok(());
                };
                self.handoff = Some(build_resume_command(&hit, &self.settings, &self.homes)?);
                self.should_quit = true;
            }
            SessionAction::Fork => {
                let Some(hit) = self.selected_hit() else {
                    return Ok(());
                };
                self.handoff = Some(build_fork_command(&hit, &self.settings, &self.homes)?);
                self.should_quit = true;
            }
        }
        Ok(())
    }

    fn export_selected_session(&mut self) -> Result<()> {
        let Some(session) = self.selected_preview().cloned() else {
            bail!("no session selected");
        };
        let rendered = session_to_plain_text(&session);
        let path = write_session_export(&session, &rendered)?;
        self.statusline = Some(statusline::Entry::completed(format!(
            "exported {}",
            file_label(&path)
        )));
        Ok(())
    }

    fn delete_selected_session(&mut self) -> Result<()> {
        let Some(hit) = self.selected_hit() else {
            return Ok(());
        };
        debug!(
            "delete_selected_session selected={} results_before={} path={}",
            self.selected,
            self.results.len(),
            hit.session.file_path.display()
        );
        let file_already_missing = match fs::remove_file(&hit.session.file_path) {
            Ok(()) => false,
            Err(error) if error.kind() == io::ErrorKind::NotFound => true,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to delete {}", hit.session.file_path.display())
                });
            }
        };
        self.results
            .retain(|result| result.session.file_path != hit.session.file_path);
        if self.selected >= self.results.len() {
            self.selected = self.results.len().saturating_sub(1);
        }
        self.preview_scroll = 0;
        self.preview_cache.remove(&hit.session.file_path);
        self.hidden_deleted_paths
            .insert(hit.session.file_path.clone());
        self.preview_render_cache = None;
        self.cancel_pending_searches();
        debug!(
            "delete_selected_session local_remove path={} file_already_missing={} results_after={} hidden_deleted_paths={}",
            hit.session.file_path.display(),
            file_already_missing,
            self.results.len(),
            self.hidden_deleted_paths.len()
        );
        match self.manager.sync_best_effort(false)? {
            SyncOutcome::Completed(_) => {
                debug!(
                    "delete_selected_session sync_completed path={}",
                    hit.session.file_path.display()
                );
                self.statusline = Some(statusline::Entry::completed(if file_already_missing {
                    format!(
                        "removed missing session {}",
                        file_label(&hit.session.file_path)
                    )
                } else {
                    format!("deleted {}", file_label(&hit.session.file_path))
                }));
            }
            SyncOutcome::Busy => {
                debug!(
                    "delete_selected_session sync_busy path={}",
                    hit.session.file_path.display()
                );
                self.statusline = Some(statusline::Entry::completed(if file_already_missing {
                    format!(
                        "removed missing session {} · index refresh deferred",
                        file_label(&hit.session.file_path)
                    )
                } else {
                    format!(
                        "deleted {} · index refresh deferred",
                        file_label(&hit.session.file_path)
                    )
                }));
            }
        }
        Ok(())
    }

    fn copy_to_clipboard(&mut self, value: &str, label: &str) -> Result<()> {
        crate::clipboard::set_text(value).context("failed to set clipboard contents")?;
        self.statusline = Some(statusline::Entry::completed(format!("copied {label}")));
        Ok(())
    }

    fn render_delete_confirm(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let popup = layout::centered_rect(area, 44, 20);
        frame.render_widget(Clear, popup);
        let hit = self.selected_hit();
        let title = hit
            .as_ref()
            .map(|hit| {
                session_display_title(
                    hit.session.agent,
                    &hit.session.project,
                    hit.session.custom_title.as_deref(),
                )
            })
            .unwrap_or_else(|| "selected session".to_owned());
        let paragraph = Paragraph::new(vec![
            Line::from(Span::styled(
                format!("Delete {title}?"),
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "This removes the JSONL file and refreshes the index.",
                Style::default().fg(theme.muted),
            )),
            Line::from(vec![
                Span::styled("Press ", Style::default().fg(theme.muted)),
                Span::styled(
                    "y",
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" to delete or ", Style::default().fg(theme.muted)),
                Span::styled(
                    "Esc",
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" to cancel.", Style::default().fg(theme.muted)),
            ]),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(theme.border_style(true))
                .title(block_title("Confirm Delete")),
        );
        frame.render_widget(paragraph, popup);
    }

    fn render_exit_confirm(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let popup = layout::centered_rect(area, 50, 22);
        frame.render_widget(Clear, popup);
        let count = self.summary_inflight.len();
        let noun = if count == 1 { "summary" } else { "summaries" };
        let paragraph = Paragraph::new(vec![
            Line::from(Span::styled(
                format!("{count} {noun} still running"),
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Quitting now will discard the in-flight work.",
                Style::default().fg(theme.muted),
            )),
            Line::default(),
            Line::from(vec![
                Span::styled("Press ", Style::default().fg(theme.muted)),
                Span::styled(
                    "y",
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" to quit anyway or ", Style::default().fg(theme.muted)),
                Span::styled(
                    "Esc",
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" to keep waiting.", Style::default().fg(theme.muted)),
            ]),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(theme.border_style(true))
                .title(block_title("Confirm Exit")),
        );
        frame.render_widget(paragraph, popup);
    }
}

fn search_worker_loop(
    search_engine: SearchEngine,
    request_rx: Receiver<SearchCommand>,
    response_tx: Sender<SearchResponse>,
) {
    while let Ok(mut command) = request_rx.recv() {
        trace!(
            "search_worker_loop received request id={} query={:?}",
            command.request_id,
            command.request.query
        );
        for newer in request_rx.try_iter() {
            debug!(
                "search_worker_loop superseding request id={} with newer id={}",
                command.request_id, newer.request_id
            );
            command = newer;
        }

        debug!(
            "search_worker_loop executing request id={} query={:?} scope={:?} sort={:?} limit={}",
            command.request_id,
            command.request.query,
            command.request.scope,
            command.request.sort,
            command.request.limit
        );
        let result = match search_engine.search(&command.request) {
            Ok(results) => {
                debug!(
                    "search_worker_loop completed request id={} results={} first_result={}",
                    command.request_id,
                    results.len(),
                    results
                        .first()
                        .map(|hit| hit.session.file_path.display().to_string())
                        .unwrap_or_else(|| "<none>".to_owned())
                );
                Ok(results)
            }
            Err(error) => {
                debug!(
                    "search_worker_loop failed request id={}: {error:#}",
                    command.request_id
                );
                Err(format!("{error:#}"))
            }
        };
        if response_tx
            .send(SearchResponse {
                request_id: command.request_id,
                result,
            })
            .is_err()
        {
            break;
        }
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode().context("failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("failed to create terminal")?;
    terminal.hide_cursor()?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode().context("failed to disable raw mode")?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn finalize_run_result(run_result: Result<AppExit>, restore_result: Result<()>) -> Result<AppExit> {
    match (run_result, restore_result) {
        (Ok(exit), Ok(())) => Ok(exit),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(restore_error)) => Err(restore_error),
        (Err(error), Err(restore_error)) => Err(error).context(format!(
            "also failed to restore terminal state: {restore_error:#}"
        )),
    }
}

fn install_panic_hook() {
    let previous_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen, DisableMouseCapture);
        previous_hook(panic_info);
    }));
}

fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.right() && row >= area.y && row < area.bottom()
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn load_summary_sources(agent: Agent, path: &Path) -> SummarySources {
    let mut sources = SummarySources::default();
    let sidecar_target = sidecar_path(path);
    if sidecar_target.exists() {
        match crate::summary::SummarySidecar::read(&sidecar_target) {
            Ok(sidecar) => match compute_fingerprint(path) {
                Ok(fingerprint) => {
                    sources.aics_sidecar = Some(AicsSummaryPreview {
                        sidecar,
                        fingerprint,
                    });
                }
                Err(err) => {
                    warn!("failed to fingerprint {}: {err:#}", path.display());
                }
            },
            Err(err) => {
                warn!(
                    "failed to read sidecar {}: {err:#}",
                    sidecar_target.display()
                );
            }
        }
    }

    if agent != Agent::Claude {
        return sources;
    }

    match read_claude_autosummaries(path) {
        Ok(summaries) => {
            sources.claude_autosummaries = summaries
                .into_iter()
                .map(|summary| ClaudeAutosummaryPreview {
                    body: summary.body,
                    generated_at: summary.timestamp,
                })
                .collect();
            sources
        }
        Err(err) => {
            warn!(
                "failed to read Claude autosummaries from {}: {err:#}",
                path.display()
            );
            sources
        }
    }
}

fn is_help_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('?')
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

fn build_resume_command(
    hit: &SearchHit,
    settings: &Settings,
    homes: &AgentHomes,
) -> Result<ExternalCommand> {
    let cwd = hit
        .session
        .cwd
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| hit.session.file_path.parent().map(Path::to_path_buf));

    let command = match hit.session.agent {
        crate::parse::Agent::Claude => {
            let (program, mut args) = settings.claude_program_and_args();
            args.extend(["--resume".to_owned(), hit.session.session_id.clone()]);
            ExternalCommand {
                program,
                args,
                cwd,
                env: vec![(
                    "CLAUDE_CONFIG_DIR".to_owned(),
                    homes.claude_home.display().to_string(),
                )],
            }
        }
        crate::parse::Agent::Codex => {
            let (program, mut args) = settings.codex_program_and_args();
            args.extend(["resume".to_owned(), hit.session.session_id.clone()]);
            ExternalCommand {
                program,
                args,
                cwd,
                env: vec![(
                    "CODEX_HOME".to_owned(),
                    homes.codex_home.display().to_string(),
                )],
            }
        }
    };
    Ok(command)
}

fn build_fork_command(
    hit: &SearchHit,
    settings: &Settings,
    homes: &AgentHomes,
) -> Result<ExternalCommand> {
    let cwd = hit
        .session
        .cwd
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| hit.session.file_path.parent().map(Path::to_path_buf));

    let command = match hit.session.agent {
        crate::parse::Agent::Claude => {
            let (program, mut args) = settings.claude_program_and_args();
            args.extend([
                "--resume".to_owned(),
                hit.session.session_id.clone(),
                "--fork-session".to_owned(),
            ]);
            ExternalCommand {
                program,
                args,
                cwd,
                env: vec![(
                    "CLAUDE_CONFIG_DIR".to_owned(),
                    homes.claude_home.display().to_string(),
                )],
            }
        }
        crate::parse::Agent::Codex => {
            let (program, mut args) = settings.codex_program_and_args();
            args.extend(["fork".to_owned(), hit.session.session_id.clone()]);
            ExternalCommand {
                program,
                args,
                cwd,
                env: vec![(
                    "CODEX_HOME".to_owned(),
                    homes.codex_home.display().to_string(),
                )],
            }
        }
    };
    Ok(command)
}

fn write_session_export(session: &Session, rendered: &str) -> Result<PathBuf> {
    let stem = export_stem_for_session(session)?;
    let cwd = env::current_dir().context("failed to resolve current directory")?;
    for suffix in 0usize.. {
        let candidate_stem = if suffix == 0 {
            stem.clone()
        } else {
            format!("{stem}-{suffix}")
        };
        validate_windows_stem_with_extension(&candidate_stem, "txt").with_context(|| {
            format!("export filename `{candidate_stem}.txt` is not Windows-safe")
        })?;

        let path = cwd.join(format!("{candidate_stem}.txt"));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(rendered.as_bytes())
                    .with_context(|| format!("failed to write {}", path.display()))?;
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("failed to create {}", path.display()));
            }
        }
    }

    bail!("failed to allocate a unique export filename")
}

fn export_stem_for_session(session: &Session) -> Result<String> {
    let stem = session
        .custom_title
        .clone()
        .unwrap_or_else(|| session.session_id.clone());
    validate_windows_filename_component(&stem)
        .with_context(|| format!("export stem `{stem}` is not Windows-safe"))?;
    Ok(stem)
}

fn session_to_plain_text(session: &Session) -> String {
    let mut output = String::new();
    for message in &session.messages {
        let role_display = match (&message.role, &message.tool_name) {
            (MessageRole::ToolCall, Some(name)) => format!("tool_call({name})"),
            (MessageRole::ToolResult, Some(name)) => format!("tool_result({name})"),
            _ => message.role.to_string(),
        };
        let timestamp = message
            .timestamp
            .map(|time| time.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        if timestamp.is_empty() {
            output.push_str(&format!("{role_display}\n"));
        } else {
            output.push_str(&format!("{role_display} {timestamp}\n"));
        }
        output.push_str(&message.content);
        output.push_str("\n\n");
    }
    output
}

#[cfg(unix)]
fn execute_handoff(command: ExternalCommand) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let mut process = Command::new(&command.program);
    process.args(&command.args);
    for (key, value) in &command.env {
        process.env(key, value);
    }
    if let Some(cwd) = command.cwd {
        process.current_dir(cwd);
    }
    let error = process.exec();
    Err(error).with_context(|| format!("failed to exec {}", command.program))
}

#[cfg(not(unix))]
fn execute_handoff(command: ExternalCommand) -> Result<()> {
    let mut process = Command::new(&command.program);
    process.args(&command.args);
    for (key, value) in &command.env {
        process.env(key, value);
    }
    if let Some(cwd) = command.cwd {
        process.current_dir(cwd);
    }
    let status = process
        .status()
        .with_context(|| format!("failed to spawn {}", command.program))?;
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::mpsc;

    use anyhow::anyhow;
    use ratatui::crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
    use ratatui::layout::Rect;
    use tempfile::TempDir;
    use tui_input::Input;

    use crate::index::writer::StoredSession;
    use crate::index::SearchHit;
    use crate::index::{IndexManager, IndexPaths, Scope, SearchFilters, SearchRequest, SortMode};
    use crate::parse::{Agent, DerivationType, MessageRole};
    use crate::settings::{Settings, ThemeName};
    use crate::tui::help::HelpTab;
    use crate::tui::layout;
    use crate::tui::settings::SettingsModalState;

    use super::{
        build_fork_command, build_resume_command, export_stem_for_session, finalize_run_result,
        write_session_export, ActionMenuState, App, AppExit, SearchResponse, SearchWorker,
        PAGE_STEP,
    };
    use crate::scan::AgentHomes;
    use crate::summary::{
        AicsSummaryPreview, ClaudeAutosummaryPreview, Fingerprint, SummarizeBackend,
        SummarySidecar, SummarySources, SummaryWorker,
    };

    #[test]
    fn builds_resume_commands_for_both_agents() {
        let settings = Settings::default();
        let homes = sample_homes();
        let claude = build_resume_command(&sample_hit(Agent::Claude), &settings, &homes).unwrap();
        assert_eq!(claude.program, "claude");
        assert_eq!(
            claude.args,
            vec![
                "--dangerously-skip-permissions",
                "--resume",
                "session-123",
            ]
        );
        assert_eq!(
            claude.env,
            vec![(
                "CLAUDE_CONFIG_DIR".to_owned(),
                "/tmp/claude-home".to_owned()
            )]
        );

        let codex = build_resume_command(&sample_hit(Agent::Codex), &settings, &homes).unwrap();
        assert_eq!(codex.program, "codex");
        assert_eq!(codex.args, vec!["--yolo", "resume", "session-123"]);
        assert_eq!(
            codex.env,
            vec![("CODEX_HOME".to_owned(), "/tmp/codex-home".to_owned())]
        );
    }

    #[test]
    fn builds_fork_commands_for_both_agents() {
        let settings = Settings::default();
        let homes = sample_homes();
        let claude = build_fork_command(&sample_hit(Agent::Claude), &settings, &homes).unwrap();
        assert_eq!(
            claude.args,
            vec![
                "--dangerously-skip-permissions",
                "--resume",
                "session-123",
                "--fork-session",
            ]
        );

        let codex = build_fork_command(&sample_hit(Agent::Codex), &settings, &homes).unwrap();
        assert_eq!(codex.args, vec!["--yolo", "fork", "session-123"]);
    }

    #[test]
    fn custom_command_prepends_args() {
        let settings = Settings {
            claude_command: "claude".to_owned(),
            claude_args: "--profile work".to_owned(),
            codex_command: "/usr/local/bin/codex".to_owned(),
            codex_args: String::new(),
            ..Settings::default()
        };
        let homes = sample_homes();
        let claude = build_resume_command(&sample_hit(Agent::Claude), &settings, &homes).unwrap();
        assert_eq!(claude.program, "claude");
        assert_eq!(
            claude.args,
            vec!["--profile", "work", "--resume", "session-123"]
        );

        let codex = build_resume_command(&sample_hit(Agent::Codex), &settings, &homes).unwrap();
        assert_eq!(codex.program, "/usr/local/bin/codex");
        assert_eq!(codex.args, vec!["resume", "session-123"]);
    }

    #[test]
    fn down_moves_selection_on_main_screen() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude), sample_hit(Agent::Codex)];
        app.selected = 0;

        app.handle_search_key(crossterm_key(KeyCode::Down)).unwrap();

        assert_eq!(app.selected, 1);
    }

    #[test]
    fn enter_opens_actions_from_main_screen_without_skipping_first_result() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude), sample_hit(Agent::Codex)];
        app.selected = 0;

        app.handle_search_key(crossterm_key(KeyCode::Enter))
            .unwrap();

        assert_eq!(app.selected, 0);
        assert!(matches!(app.overlay, super::Overlay::Actions(_)));
    }

    #[test]
    fn opening_viewer_prepopulates_search_with_list_query() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude)];
        app.committed_query = "alpha beta".to_owned();

        app.open_viewer();

        match &app.overlay {
            super::Overlay::Viewer(state) => assert_eq!(state.search_query(), "alpha beta"),
            _ => panic!("viewer should be open"),
        }
    }

    #[test]
    fn action_shortcuts_do_not_run_when_actions_overlay_is_closed() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude)];

        app.handle_search_key(crossterm_key(KeyCode::Char('v')))
            .unwrap();

        assert!(matches!(app.overlay, super::Overlay::None));
    }

    #[test]
    fn ctrl_j_moves_selection_on_main_screen() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude), sample_hit(Agent::Codex)];
        app.selected = 0;

        app.handle_key(crossterm_key_mods(
            KeyCode::Char('j'),
            KeyModifiers::CONTROL,
        ))
        .unwrap();

        assert_eq!(app.selected, 1);
    }

    #[test]
    fn ctrl_k_moves_selection_on_main_screen() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude), sample_hit(Agent::Codex)];
        app.selected = 1;

        app.handle_key(crossterm_key_mods(
            KeyCode::Char('k'),
            KeyModifiers::CONTROL,
        ))
        .unwrap();

        assert_eq!(app.selected, 0);
    }

    #[test]
    fn ctrl_x_then_v_opens_viewer() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude)];

        app.handle_key(crossterm_key_mods(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL,
        ))
        .unwrap();
        app.handle_key(crossterm_key(KeyCode::Char('v'))).unwrap();

        assert!(matches!(app.overlay, super::Overlay::Viewer(_)));
    }

    #[test]
    fn ctrl_x_then_d_opens_delete_confirmation() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude)];

        app.handle_key(crossterm_key_mods(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL,
        ))
        .unwrap();
        app.handle_key(crossterm_key(KeyCode::Char('d'))).unwrap();

        assert!(matches!(app.overlay, super::Overlay::ConfirmDelete));
    }

    #[test]
    fn ctrl_x_then_ctrl_d_opens_delete_confirmation() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude)];

        app.handle_key(crossterm_key_mods(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL,
        ))
        .unwrap();
        app.handle_key(crossterm_key_mods(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        ))
        .unwrap();

        assert!(!app.pending_main_menu_action);
        assert!(matches!(app.overlay, super::Overlay::ConfirmDelete));
    }

    #[test]
    fn ctrl_x_then_ctrl_g_cancels_prefix_and_toggles_scope() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude)];
        app.scope = Scope::Global;
        let (expected_local_path, expected_local_canonical) = match &app.local_scope {
            Scope::CurrentDir(path, canonical) => (path.clone(), canonical.clone()),
            Scope::Global => panic!("test app should have a local cwd scope"),
        };

        app.handle_key(crossterm_key_mods(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL,
        ))
        .unwrap();
        app.handle_key(crossterm_key_mods(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL,
        ))
        .unwrap();

        assert!(!app.pending_main_menu_action);
        match &app.scope {
            Scope::CurrentDir(path, canonical) => {
                assert_eq!(path, &expected_local_path);
                assert_eq!(canonical, &expected_local_canonical);
            }
            Scope::Global => panic!("scope should switch to local"),
        }
    }

    #[test]
    fn ctrl_p_toggles_preview_visibility() {
        let mut app = test_app();
        let visible_before = app.preview_visible;

        app.handle_key(crossterm_key_mods(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
        ))
        .unwrap();

        assert_eq!(app.preview_visible, !visible_before);
    }

    #[test]
    fn ctrl_g_toggles_scope_between_global_and_local() {
        let mut app = test_app();
        app.scope = Scope::Global;
        app.pending_search = false;
        app.last_edit_at = Some(std::time::Instant::now());
        let (expected_local_path, expected_local_canonical) = match &app.local_scope {
            Scope::CurrentDir(path, canonical) => (path.clone(), canonical.clone()),
            Scope::Global => panic!("test app should have a local cwd scope"),
        };

        app.handle_key(crossterm_key_mods(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL,
        ))
        .unwrap();

        match &app.scope {
            Scope::CurrentDir(path, canonical) => {
                assert_eq!(path, &expected_local_path);
                assert_eq!(canonical, &expected_local_canonical);
            }
            Scope::Global => panic!("scope should switch to local"),
        }
        assert!(!app.pending_search);
        assert!(app.last_edit_at.is_none());

        app.handle_key(crossterm_key_mods(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL,
        ))
        .unwrap();

        assert!(matches!(app.scope, Scope::Global));
    }

    #[test]
    fn question_mark_opens_help_from_main_screen() {
        let mut app = test_app();

        app.handle_key(crossterm_key(KeyCode::Char('?'))).unwrap();

        assert_eq!(
            app.help.as_ref().map(|state| state.tab()),
            Some(HelpTab::SessionList)
        );
    }

    #[test]
    fn question_mark_opens_viewer_help_when_viewer_is_visible() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude)];
        app.open_viewer();

        app.handle_key(crossterm_key(KeyCode::Char('?'))).unwrap();

        assert!(matches!(app.overlay, super::Overlay::Viewer(_)));
        assert_eq!(
            app.help.as_ref().map(|state| state.tab()),
            Some(HelpTab::Viewer)
        );
    }

    #[test]
    fn cursor_motion_in_search_does_not_trigger_new_search() {
        let mut app = test_app();
        app.query = Input::default().with_value("alpha".to_owned());
        app.pending_search = false;
        app.last_edit_at = None;

        app.handle_search_key(crossterm_key(KeyCode::Left)).unwrap();

        assert!(!app.pending_search);
        assert!(app.last_edit_at.is_none());
        assert_eq!(app.query.value(), "alpha");
    }

    #[test]
    fn settings_overlay_uses_live_preview_theme() {
        let mut app = test_app();
        app.overlay = super::Overlay::Settings(SettingsModalState::new(&Settings {
            theme: ThemeName::Lazygit,
            ..Settings::default()
        }));

        if let super::Overlay::Settings(state) = &mut app.overlay {
            *state = SettingsModalState::new(&Settings {
                theme: ThemeName::Sunset,
                ..Settings::default()
            });
        }

        assert_eq!(app.current_frame_theme_name(), ThemeName::Sunset);
    }

    #[test]
    fn escape_on_main_screen_quits_when_query_is_empty() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude)];

        app.handle_search_key(crossterm_key(KeyCode::Esc)).unwrap();

        assert!(app.should_quit);
    }

    #[test]
    fn escape_on_main_screen_clears_query_when_query_exists() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude)];
        app.query = Input::default().with_value("alpha".to_owned());

        app.handle_search_key(crossterm_key(KeyCode::Esc)).unwrap();

        assert!(!app.should_quit);
        assert_eq!(app.query.value(), "");
    }

    #[test]
    fn page_down_scrolls_preview_without_changing_selected_session() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude), sample_hit(Agent::Codex)];
        app.selected = 0;
        app.last_layout = Some(layout::AppLayout {
            search: Rect::new(0, 0, 120, 3),
            list: Rect::new(0, 3, 60, 20),
            preview: Some(Rect::new(60, 3, 60, 20)),
            status: Rect::new(0, 23, 120, 2),
        });

        app.handle_search_key(crossterm_key(KeyCode::PageDown))
            .unwrap();

        assert_eq!(app.selected, 0);
        assert_eq!(app.preview_scroll, PAGE_STEP);
    }

    #[test]
    fn ctrl_shift_down_jumps_preview_between_user_messages() {
        let mut app = test_app();
        let path = PathBuf::from("/tmp/demo/session-users.jsonl");
        app.results = vec![sample_hit_with_path(Agent::Claude, path.clone())];
        app.selected = 0;
        app.last_layout = Some(layout::AppLayout {
            search: Rect::new(0, 0, 120, 3),
            list: Rect::new(0, 3, 60, 20),
            preview: Some(Rect::new(60, 3, 60, 20)),
            status: Rect::new(0, 23, 120, 2),
        });
        app.preview_cache.insert(path, Some(sample_preview_session()));

        app.handle_search_key(crossterm_key_mods(
            KeyCode::Down,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ))
        .unwrap();

        assert_eq!(app.preview_scroll, 12);
    }

    #[test]
    fn double_clicking_selected_list_item_opens_viewer() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude), sample_hit(Agent::Codex)];
        app.last_layout = Some(layout::AppLayout {
            search: Rect::new(0, 0, 120, 3),
            list: Rect::new(0, 3, 60, 20),
            preview: Some(Rect::new(60, 3, 60, 20)),
            status: Rect::new(0, 23, 120, 2),
        });

        let row = app.last_layout.as_ref().unwrap().list.y + 2;
        app.handle_mouse(crossterm_mouse(
            MouseEventKind::Down(MouseButton::Left),
            5,
            row,
        ))
        .unwrap();
        app.handle_mouse(crossterm_mouse(
            MouseEventKind::Down(MouseButton::Left),
            5,
            row,
        ))
        .unwrap();

        assert!(matches!(app.overlay, super::Overlay::Viewer(_)));
    }

    #[test]
    fn double_click_survives_intermediate_mouse_up() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude), sample_hit(Agent::Codex)];
        app.last_layout = Some(layout::AppLayout {
            search: Rect::new(0, 0, 120, 3),
            list: Rect::new(0, 3, 60, 20),
            preview: Some(Rect::new(60, 3, 60, 20)),
            status: Rect::new(0, 23, 120, 2),
        });

        let row = app.last_layout.as_ref().unwrap().list.y + 2;
        app.handle_mouse(crossterm_mouse(
            MouseEventKind::Down(MouseButton::Left),
            5,
            row,
        ))
        .unwrap();
        app.handle_mouse(crossterm_mouse(
            MouseEventKind::Up(MouseButton::Left),
            5,
            row,
        ))
        .unwrap();
        app.handle_mouse(crossterm_mouse(
            MouseEventKind::Down(MouseButton::Left),
            5,
            row,
        ))
        .unwrap();

        assert!(matches!(app.overlay, super::Overlay::Viewer(_)));
    }

    #[test]
    fn double_clicking_action_menu_item_runs_it() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude)];
        app.overlay = super::Overlay::Actions(ActionMenuState::new());

        let list = crate::tui::actions::list_area(Rect::new(0, 0, 120, 30));
        let row = list.y;
        let column = list.x;
        app.last_frame_area = Rect::new(0, 0, 120, 30);

        app.handle_overlay_mouse(crossterm_mouse(
            MouseEventKind::Down(MouseButton::Left),
            column,
            row,
        ))
        .unwrap();
        app.handle_overlay_mouse(crossterm_mouse(
            MouseEventKind::Down(MouseButton::Left),
            column,
            row,
        ))
        .unwrap();

        assert!(matches!(app.overlay, super::Overlay::Viewer(_)));
    }

    #[test]
    fn moving_up_from_bottom_visible_item_does_not_scroll_list_immediately() {
        let mut app = test_app();
        app.results = (0..6).map(|_| sample_hit(Agent::Claude)).collect();
        app.last_layout = Some(layout::AppLayout {
            search: Rect::new(0, 0, 120, 3),
            list: Rect::new(0, 3, 60, 8),
            preview: Some(Rect::new(60, 3, 60, 8)),
            status: Rect::new(0, 11, 120, 2),
        });

        app.select_absolute(5);
        let (_, selected_within) = app.list_window(3);
        assert_eq!(app.list_offset(3), 3);
        assert_eq!(selected_within, Some(2));

        app.handle_search_key(crossterm_key(KeyCode::Up)).unwrap();

        let (_, selected_within) = app.list_window(3);
        assert_eq!(app.selected, 4);
        assert_eq!(app.list_offset(3), 3);
        assert_eq!(selected_within, Some(1));
    }

    #[test]
    fn home_and_end_fall_back_to_list_when_preview_is_hidden() {
        let mut app = test_app();
        app.results = vec![
            sample_hit(Agent::Claude),
            sample_hit(Agent::Codex),
            sample_hit(Agent::Claude),
        ];
        app.preview_visible = false;
        app.selected = 1;

        app.handle_search_key(crossterm_key(KeyCode::End)).unwrap();
        assert_eq!(app.selected, 2);

        app.handle_search_key(crossterm_key(KeyCode::Home)).unwrap();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn summary_loader_keeps_aics_sidecar_and_claude_autosummaries() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("session.jsonl");
        fs::write(
            &path,
            "{\"type\":\"system\",\"subtype\":\"away_summary\",\"content\":\"Claude autosummary\",\"timestamp\":\"2026-04-15T13:25:59.006Z\",\"sessionId\":\"summary-test\"}\n",
        )
        .unwrap();
        let fingerprint = super::compute_fingerprint(&path).unwrap();
        let sidecar = SummarySidecar::new(
            &path,
            &fingerprint,
            SummarizeBackend::Claude,
            "AICS sidecar".to_owned(),
        );
        sidecar.write_atomic(&super::sidecar_path(&path)).unwrap();

        let sources = super::load_summary_sources(Agent::Claude, &path);
        assert_eq!(
            sources
                .aics_sidecar
                .as_ref()
                .map(|summary| summary.sidecar.body.trim()),
            Some("AICS sidecar")
        );
        assert_eq!(sources.claude_autosummaries.len(), 1);
        assert_eq!(sources.claude_autosummaries[0].body, "Claude autosummary");
    }

    #[test]
    fn summary_loader_keeps_all_claude_autosummaries_when_sidecar_is_invalid() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("session.jsonl");
        fs::write(
            &path,
            concat!(
                "{\"type\":\"system\",\"subtype\":\"away_summary\",\"content\":\"Earlier summary\",\"timestamp\":\"2026-04-15T13:20:00.000Z\",\"sessionId\":\"summary-test\"}\n",
                "{\"type\":\"system\",\"subtype\":\"away_summary\",\"content\":\"Latest summary\",\"timestamp\":\"2026-04-15T13:25:59.006Z\",\"sessionId\":\"summary-test\"}\n"
            ),
        )
        .unwrap();
        fs::write(super::sidecar_path(&path), "not frontmatter").unwrap();

        let sources = super::load_summary_sources(Agent::Claude, &path);
        assert!(sources.aics_sidecar.is_none());
        assert_eq!(sources.claude_autosummaries.len(), 2);
        assert_eq!(sources.claude_autosummaries[0].body, "Earlier summary");
        assert_eq!(sources.claude_autosummaries[1].body, "Latest summary");
    }

    #[test]
    fn snippet_mode_cycles_through_fixed_sequence() {
        let mut app = test_app();

        assert_eq!(app.snippet_mode, super::SnippetMode::ContentPreview);
        app.cycle_snippet_source();
        assert_eq!(app.snippet_mode, super::SnippetMode::AicsSummary);
        app.cycle_snippet_source();
        assert_eq!(app.snippet_mode, super::SnippetMode::BuiltinSummary);
        app.cycle_snippet_source();
        assert_eq!(app.snippet_mode, super::SnippetMode::ContentPreview);
    }

    #[test]
    fn snippet_modes_follow_summary_fallback_rules() {
        let mut app = test_app();
        let hit = sample_hit(Agent::Claude);
        let path = hit.session.file_path.clone();
        app.summary_cache.insert(
            path.clone(),
            SummarySources {
                aics_sidecar: Some(AicsSummaryPreview {
                    sidecar: SummarySidecar::new(
                        &path,
                        &Fingerprint {
                            line_count: 1,
                            last_line_sha256: "abc".repeat(21) + "a",
                        },
                        SummarizeBackend::Claude,
                        "AICS body".to_owned(),
                    ),
                    fingerprint: Fingerprint {
                        line_count: 1,
                        last_line_sha256: "abc".repeat(21) + "a",
                    },
                }),
                claude_autosummaries: vec![
                    ClaudeAutosummaryPreview {
                        body: "Older builtin body".to_owned(),
                        generated_at: None,
                    },
                    ClaudeAutosummaryPreview {
                        body: "Newest builtin body".to_owned(),
                        generated_at: None,
                    },
                ],
            },
        );

        app.snippet_mode = super::SnippetMode::ContentPreview;
        assert_eq!(app.active_summary_snippet_text(&hit), None);

        app.snippet_mode = super::SnippetMode::AicsSummary;
        assert_eq!(app.active_summary_snippet_text(&hit).as_deref(), Some("AICS body"));

        app.snippet_mode = super::SnippetMode::BuiltinSummary;
        assert_eq!(
            app.active_summary_snippet_text(&hit).as_deref(),
            Some("Newest builtin body")
        );

        app.summary_cache.insert(
            path.clone(),
            SummarySources {
                aics_sidecar: None,
                claude_autosummaries: vec![ClaudeAutosummaryPreview {
                    body: "Only builtin body".to_owned(),
                    generated_at: None,
                }],
            },
        );
        app.snippet_mode = super::SnippetMode::AicsSummary;
        assert_eq!(
            app.active_summary_snippet_text(&hit).as_deref(),
            Some("Only builtin body")
        );

        app.summary_cache.insert(
            path,
            SummarySources {
                aics_sidecar: Some(AicsSummaryPreview {
                    sidecar: SummarySidecar::new(
                        &PathBuf::from("/tmp/demo/session.jsonl"),
                        &Fingerprint {
                            line_count: 1,
                            last_line_sha256: "abc".repeat(21) + "a",
                        },
                        SummarizeBackend::Claude,
                        "Only AICS body".to_owned(),
                    ),
                    fingerprint: Fingerprint {
                        line_count: 1,
                        last_line_sha256: "abc".repeat(21) + "a",
                    },
                }),
                claude_autosummaries: Vec::new(),
            },
        );
        app.snippet_mode = super::SnippetMode::BuiltinSummary;
        assert_eq!(
            app.active_summary_snippet_text(&hit).as_deref(),
            Some("Only AICS body")
        );
    }

    #[test]
    fn deleting_missing_session_file_removes_stale_result_without_error() {
        let temp = TempDir::new().unwrap();
        let missing_path = temp.path().join("missing-session.jsonl");
        let mut app = test_app();
        app.results = vec![sample_hit_with_path(Agent::Claude, missing_path.clone())];
        app.selected = 0;

        app.delete_selected_session().unwrap();

        assert!(app.results.is_empty());
        assert_eq!(app.selected_index(), None);
        assert!(app
            .statusline
            .as_ref()
            .is_some_and(|entry| entry.label.contains("removed missing session")));
    }

    #[test]
    fn delete_cancels_pending_and_in_flight_searches() {
        let temp = TempDir::new().unwrap();
        let deleted_path = temp.path().join("deleted-session.jsonl");
        let surviving_path = temp.path().join("surviving-session.jsonl");
        fs::write(&deleted_path, "{}\n").unwrap();
        fs::write(&surviving_path, "{}\n").unwrap();
        let (mut app, response_tx) = test_app_with_response_sender();
        app.results = vec![sample_hit_with_path(Agent::Claude, deleted_path.clone())];
        app.selected = 0;
        app.pending_search = true;
        app.search_in_flight = true;
        app.latest_search_id = Some(4);
        app.next_search_id = 5;

        app.delete_selected_session().unwrap();

        assert!(app.results.is_empty());
        assert!(!app.pending_search);
        assert!(!app.search_in_flight);
        assert_eq!(app.latest_search_id, Some(5));
        assert_eq!(app.next_search_id, 6);

        response_tx
            .send(SearchResponse {
                request_id: 4,
                result: Ok(vec![sample_hit_with_path(Agent::Codex, surviving_path)]),
            })
            .unwrap();

        assert!(!app.collect_search_responses().unwrap());
        assert!(app.results.is_empty());
    }

    #[test]
    fn stale_search_response_cannot_restore_deleted_session() {
        let temp = TempDir::new().unwrap();
        let deleted_path = temp.path().join("deleted-session.jsonl");
        let surviving_path = temp.path().join("surviving-session.jsonl");
        fs::write(&deleted_path, "{}\n").unwrap();
        fs::write(&surviving_path, "{}\n").unwrap();
        let (mut app, response_tx) = test_app_with_response_sender();
        app.latest_search_id = Some(7);
        app.hidden_deleted_paths.insert(deleted_path.clone());

        response_tx
            .send(SearchResponse {
                request_id: 7,
                result: Ok(vec![sample_hit_with_path(
                    Agent::Claude,
                    deleted_path.clone(),
                )]),
            })
            .unwrap();

        assert!(app.collect_search_responses().unwrap());
        assert!(app.results.is_empty());
        assert!(app.hidden_deleted_paths.contains(&deleted_path));

        response_tx
            .send(SearchResponse {
                request_id: 7,
                result: Ok(vec![sample_hit_with_path(Agent::Codex, surviving_path)]),
            })
            .unwrap();

        assert!(app.collect_search_responses().unwrap());
        assert_eq!(app.results.len(), 1);
        assert!(!app.hidden_deleted_paths.contains(&deleted_path));
    }

    #[test]
    fn search_response_skips_hits_whose_files_are_missing() {
        let missing_path = PathBuf::from("/tmp/demo/missing-session.jsonl");
        let (mut app, response_tx) = test_app_with_response_sender();
        app.latest_search_id = Some(11);

        response_tx
            .send(SearchResponse {
                request_id: 11,
                result: Ok(vec![sample_hit_with_path(Agent::Claude, missing_path)]),
            })
            .unwrap();

        assert!(app.collect_search_responses().unwrap());
        assert!(app.results.is_empty());
    }

    #[test]
    fn export_rejects_windows_reserved_titles() {
        let mut hit = sample_hit(Agent::Claude);
        hit.session.custom_title = Some("NUL".to_owned());

        let error = export_stem_for_session_from_hit(&hit).expect_err("reserved title");
        assert!(format!("{error:#}").contains("not Windows-safe"));
    }

    #[test]
    fn export_uses_create_new_to_avoid_overwriting_existing_files() {
        let temp = TempDir::new().unwrap();
        let previous_dir = env::current_dir().unwrap();
        env::set_current_dir(temp.path()).unwrap();

        let session = sample_session_for_export("session-export");
        fs::write(temp.path().join("session-export.txt"), "existing").unwrap();

        let path = write_session_export(&session, "new contents").unwrap();
        let first = fs::read_to_string(temp.path().join("session-export.txt")).unwrap();
        let second = fs::read_to_string(&path).unwrap();

        env::set_current_dir(previous_dir).unwrap();

        assert_eq!(first, "existing");
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            "session-export-1.txt"
        );
        assert_eq!(second, "new contents");
    }

    #[test]
    fn confirm_delete_keeps_tui_running_when_delete_fails() {
        let temp = TempDir::new().unwrap();
        let mut app = test_app();
        app.results = vec![sample_hit_with_path(
            Agent::Claude,
            temp.path().to_path_buf(),
        )];
        app.overlay = super::Overlay::ConfirmDelete;

        app.handle_overlay_key(crossterm_key(KeyCode::Char('y')))
            .unwrap();

        assert!(matches!(app.overlay, super::Overlay::None));
        assert!(app
            .statusline
            .as_ref()
            .is_some_and(|entry| entry.label.contains("failed to delete")));
    }

    #[test]
    fn finalize_run_result_preserves_original_error_and_mentions_restore_failure() {
        let result = finalize_run_result(
            Err(anyhow!("event loop failed")),
            Err(anyhow!("restore failed")),
        );

        let error = result.expect_err("combined failure");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("event loop failed"));
        assert!(rendered.contains("also failed to restore terminal state"));
        assert!(rendered.contains("restore failed"));
    }

    #[test]
    fn finalize_run_result_returns_success_when_both_steps_succeed() {
        let result = finalize_run_result(Ok(AppExit::Normal), Ok(()));
        assert!(matches!(result, Ok(AppExit::Normal)));
    }

    fn test_app() -> App {
        test_app_with_response_sender().0
    }

    fn test_app_with_response_sender() -> (App, mpsc::Sender<SearchResponse>) {
        let temp = TempDir::new().unwrap();
        let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path()));
        let (request_tx, request_rx) = mpsc::channel();
        std::thread::spawn(move || while request_rx.recv().is_ok() {});
        let (response_tx, response_rx) = mpsc::channel();
        let worker = SearchWorker {
            request_tx,
            response_rx,
        };

        let summary_worker = SummaryWorker::spawn().expect("spawn summary worker in test harness");
        (
            App::new(
                manager,
                worker,
                summary_worker,
                SearchRequest {
                    query: String::new(),
                    scope: Scope::Global,
                    limit: 10,
                    sort: SortMode::Time,
                    filters: SearchFilters::default(),
                },
                Settings::default(),
                sample_homes(),
            ),
            response_tx,
        )
    }

    fn sample_homes() -> AgentHomes {
        AgentHomes {
            claude_home: PathBuf::from("/tmp/claude-home"),
            codex_home: PathBuf::from("/tmp/codex-home"),
        }
    }

    fn crossterm_key(code: KeyCode) -> ratatui::crossterm::event::KeyEvent {
        crossterm_key_mods(code, ratatui::crossterm::event::KeyModifiers::NONE)
    }

    fn crossterm_key_mods(
        code: KeyCode,
        modifiers: ratatui::crossterm::event::KeyModifiers,
    ) -> ratatui::crossterm::event::KeyEvent {
        ratatui::crossterm::event::KeyEvent::new(code, modifiers)
    }

    fn crossterm_mouse(
        kind: MouseEventKind,
        column: u16,
        row: u16,
    ) -> ratatui::crossterm::event::MouseEvent {
        ratatui::crossterm::event::MouseEvent {
            kind,
            column,
            row,
            modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
        }
    }

    fn sample_hit(agent: Agent) -> SearchHit {
        sample_hit_with_path(agent, PathBuf::from("/tmp/demo/session.jsonl"))
    }

    fn sample_hit_with_path(agent: Agent, file_path: PathBuf) -> SearchHit {
        SearchHit {
            session: StoredSession {
                session_id: "session-123".to_owned(),
                agent,
                project: "/tmp/demo".to_owned(),
                branch: Some("main".to_owned()),
                cwd: Some("/tmp/demo".to_owned()),
                modified_ts: 0,
                lines: 1,
                file_path,
                first_msg_role: None,
                first_msg_content: String::new(),
                last_msg_role: None,
                last_msg_content: String::new(),
                first_user_msg_content: String::new(),
                derivation_type: DerivationType::Original,
                is_sidechain: false,
                custom_title: None,
            },
            snippet_html: String::new(),
            score: 0.0,
            is_live: false,
        }
    }

    fn sample_session_for_export(stem: &str) -> crate::parse::Session {
        crate::parse::Session {
            session_id: "session-123".to_owned(),
            agent: Agent::Claude,
            project: "/tmp/demo".to_owned(),
            branch: Some("main".to_owned()),
            cwd: Some("/tmp/demo".to_owned()),
            created: None,
            modified: None,
            modified_ts: 0,
            lines: 1,
            file_path: PathBuf::from("/tmp/demo/session.jsonl"),
            first_msg_role: None,
            first_msg_content: String::new(),
            last_msg_role: None,
            last_msg_content: String::new(),
            first_user_msg_content: String::new(),
            derivation_type: DerivationType::Original,
            is_sidechain: false,
            custom_title: Some(stem.to_owned()),
            messages: Vec::new(),
            content: String::new(),
        }
    }

    fn sample_preview_session() -> crate::parse::Session {
        crate::parse::Session {
            session_id: "session-users".to_owned(),
            agent: Agent::Claude,
            project: "/tmp/demo".to_owned(),
            branch: Some("main".to_owned()),
            cwd: Some("/tmp/demo".to_owned()),
            created: None,
            modified: None,
            modified_ts: 0,
            lines: 6,
            file_path: PathBuf::from("/tmp/demo/session-users.jsonl"),
            first_msg_role: Some(MessageRole::User),
            first_msg_content: "first user".to_owned(),
            last_msg_role: Some(MessageRole::Assistant),
            last_msg_content: "second assistant".to_owned(),
            first_user_msg_content: "first user".to_owned(),
            derivation_type: DerivationType::Original,
            is_sidechain: false,
            custom_title: Some("preview users".to_owned()),
            messages: vec![
                crate::parse::SessionMessage {
                    role: MessageRole::User,
                    content: "first user".to_owned(),
                    timestamp: None,
                    tool_name: None,
                },
                crate::parse::SessionMessage {
                    role: MessageRole::Assistant,
                    content: "first assistant".to_owned(),
                    timestamp: None,
                    tool_name: None,
                },
                crate::parse::SessionMessage {
                    role: MessageRole::ToolCall,
                    content: "run tool".to_owned(),
                    timestamp: None,
                    tool_name: Some("Read".to_owned()),
                },
                crate::parse::SessionMessage {
                    role: MessageRole::ToolResult,
                    content: "tool output".to_owned(),
                    timestamp: None,
                    tool_name: Some("Read".to_owned()),
                },
                crate::parse::SessionMessage {
                    role: MessageRole::User,
                    content: "second user".to_owned(),
                    timestamp: None,
                    tool_name: None,
                },
                crate::parse::SessionMessage {
                    role: MessageRole::Assistant,
                    content: "second assistant".to_owned(),
                    timestamp: None,
                    tool_name: None,
                },
            ],
            content: "first user\nfirst assistant\nrun tool\ntool output\nsecond user\nsecond assistant"
                .to_owned(),
        }
    }

    fn export_stem_for_session_from_hit(hit: &SearchHit) -> Result<String, anyhow::Error> {
        let session = sample_session_for_export(hit.session.custom_title.as_deref().unwrap_or(""));
        export_stem_for_session(&session)
    }
}
