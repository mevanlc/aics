use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::panic;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
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
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::{Frame, Terminal};
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::index::{
    IndexManager, Scope, SearchEngine, SearchFilters, SearchHit, SearchRequest, SortMode,
    SyncOutcome,
};
use crate::parse::{parse_session_file, Session};
use crate::settings::Settings;
use crate::tui::actions::{ActionMenuState, ActionOutcome, SessionAction};
use crate::tui::filter::{FilterModalState, FilterOutcome};
use crate::tui::settings::{SettingsModalState, SettingsOutcome};
use crate::tui::theme::Theme;
use crate::tui::viewer::{ViewerOutcome, ViewerState};
use crate::tui::{layout, list, preview, search};

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(200);
const SEARCH_POLL_INTERVAL: Duration = Duration::from_millis(50);
const PAGE_STEP: usize = 8;
const LIST_MOUSE_SCROLL_STEP: isize = 1;
const PANEL_MOUSE_SCROLL_STEP: usize = 3;
const PREVIEW_WIDTH_DEFAULT: u16 = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Search,
    List,
    Preview,
}

#[derive(Debug, Clone)]
enum Overlay {
    None,
    Filters(FilterModalState),
    Actions(ActionMenuState),
    Viewer(ViewerState),
    Settings(SettingsModalState),
    ConfirmDelete,
}

#[derive(Debug, Clone)]
struct ExternalCommand {
    program: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
}

#[derive(Debug)]
enum AppExit {
    Normal,
    Handoff(ExternalCommand),
}

pub fn run_app(
    manager: IndexManager,
    search_engine: SearchEngine,
    initial_request: SearchRequest,
    settings: Settings,
) -> Result<()> {
    let worker = SearchWorker::spawn(search_engine)?;
    let mut app = App::new(manager, worker, initial_request, settings);
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
    scope: Scope,
    local_scope: Scope,
    filters: SearchFilters,
    sort: SortMode,
    result_limit: usize,
    selected: usize,
    preview_cache: HashMap<PathBuf, Option<Session>>,
    pending_search: bool,
    last_edit_at: Option<Instant>,
    next_search_id: u64,
    latest_search_id: Option<u64>,
    search_in_flight: bool,
    search_error: Option<String>,
    should_quit: bool,
    preview_visible: bool,
    preview_width_pct: u16,
    last_frame_area: Rect,
    last_layout: Option<layout::AppLayout>,
    overlay: Overlay,
    status_message: Option<String>,
    handoff: Option<ExternalCommand>,
    settings: Settings,
    theme: Theme,
}

impl App {
    fn new(
        manager: IndexManager,
        worker: SearchWorker,
        initial_request: SearchRequest,
        settings: Settings,
    ) -> Self {
        let local_scope = match &initial_request.scope {
            Scope::CurrentDir(path) => Scope::CurrentDir(path.clone()),
            Scope::Global => env::current_dir()
                .map(Scope::CurrentDir)
                .unwrap_or(Scope::Global),
        };

        Self {
            focus: Focus::Search,
            query: Input::default().with_value(initial_request.query),
            results: Vec::new(),
            preview_scroll: 0,
            manager,
            worker,
            scope: initial_request.scope,
            local_scope,
            filters: initial_request.filters,
            sort: initial_request.sort,
            result_limit: initial_request.limit.max(1),
            selected: 0,
            preview_cache: HashMap::new(),
            pending_search: true,
            last_edit_at: None,
            next_search_id: 0,
            latest_search_id: None,
            search_in_flight: false,
            search_error: None,
            should_quit: false,
            preview_visible: true,
            preview_width_pct: PREVIEW_WIDTH_DEFAULT,
            last_frame_area: Rect::default(),
            last_layout: None,
            overlay: Overlay::None,
            status_message: None,
            handoff: None,
            theme: Theme::from_name(settings.theme),
            settings,
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

                if needs_redraw {
                    terminal.draw(|frame| self.draw(frame))?;
                    needs_redraw = false;
                }

                if event::poll(self.poll_timeout())? {
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
            Scope::Global => "All Projects".to_owned(),
            Scope::CurrentDir(path) => path
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| path.to_string_lossy().into_owned()),
        }
    }

    pub fn selected_index(&self) -> Option<usize> {
        (!self.results.is_empty()).then_some(self.selected)
    }

    pub fn is_searching(&self) -> bool {
        self.pending_search || self.search_in_flight
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

    pub fn list_window(&self, max_items: usize) -> (&[SearchHit], Option<usize>) {
        if self.results.is_empty() {
            return (&[], None);
        }

        let max_items = max_items.max(1);
        let offset = self.selected.saturating_sub(max_items.saturating_sub(1));
        let end = (offset + max_items).min(self.results.len());
        (&self.results[offset..end], Some(self.selected - offset))
    }

    fn draw(&mut self, frame: &mut Frame) {
        let theme = self.theme.clone();
        frame.render_widget(Clear, frame.area());
        self.last_frame_area = frame.area();
        let area = frame.area();
        if area.height < 5 || area.width < 20 {
            let msg = Paragraph::new("Terminal too small").style(Style::default().fg(theme.muted));
            frame.render_widget(msg, area);
            return;
        }
        let areas = layout::split(area, self.preview_width_pct);
        self.last_layout = Some(areas);
        self.preview_visible = areas.preview.is_some();
        self.clamp_scroll_state(areas);
        search::render(frame, self, areas.search, &theme);
        list::render(frame, self, areas.list, &theme);

        if let Some(preview_area) = areas.preview {
            preview::render(frame, self, preview_area, &theme);
        }

        let status_base = self.status_text();
        let w = areas.status.width as usize;
        let status_str = if w >= 110 {
            format!("{status_base}  |  Tab focus  |  Ctrl+F filters  |  Ctrl+O settings  |  Enter actions  |  Ctrl+H/L resize  |  Ctrl+C quit")
        } else if w >= 60 {
            format!("{status_base}  |  Tab  ^F  ^O  Enter  ^C")
        } else {
            status_base
        };
        let status = Paragraph::new(status_str)
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_type(BorderType::Rounded),
            )
            .style(ratatui::style::Style::default().fg(theme.muted));
        frame.render_widget(status, areas.status);

        match self.overlay.clone() {
            Overlay::None => {}
            Overlay::Filters(filter_state) => {
                filter_state.render(frame, frame.area(), &theme, &self.local_scope_label());
            }
            Overlay::Actions(action_menu) => action_menu.render(frame, frame.area(), &theme),
            Overlay::Viewer(viewer_state) => {
                if let Some(session) = self.selected_preview() {
                    viewer_state.render(frame, frame.area(), session, &theme);
                }
            }
            Overlay::Settings(settings_state) => {
                // Render settings modal with a live preview of the selected theme.
                let preview_theme = Theme::from_name(settings_state.current_theme());
                settings_state.render(frame, frame.area(), &preview_theme);
            }
            Overlay::ConfirmDelete => self.render_delete_confirm(frame, frame.area(), &theme),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return Ok(());
        }

        if !matches!(self.overlay, Overlay::None) {
            return self.handle_overlay_key(key);
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
                    self.should_quit = true;
                } else {
                    self.clear_query();
                }
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_settings()
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_filters()
            }
            KeyCode::Tab | KeyCode::Enter => self.focus_list(),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.resize_preview(-5)
            }
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.resize_preview(5)
            }
            _ => {
                if self.query.handle_event(&Event::Key(key)).is_some() {
                    self.pending_search = true;
                    self.last_edit_at = Some(Instant::now());
                }
            }
        }
        Ok(())
    }

    fn handle_list_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => {
                if self.query.value().is_empty() {
                    self.should_quit = true;
                } else {
                    self.focus = Focus::Search;
                }
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_settings()
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_filters()
            }
            KeyCode::Tab => self.focus = next_focus(self.focus, self.preview_available()),
            KeyCode::Enter => self.overlay = Overlay::Actions(ActionMenuState::new()),
            KeyCode::Char('v') if key.modifiers.is_empty() => self.open_viewer(),
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Home => self.select_absolute(0),
            KeyCode::End => self.select_absolute(self.results.len().saturating_sub(1)),
            KeyCode::PageDown => self.move_selection(PAGE_STEP as isize),
            KeyCode::PageUp => self.move_selection(-(PAGE_STEP as isize)),
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.resize_preview(-5)
            }
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.resize_preview(5)
            }
            KeyCode::Backspace | KeyCode::Delete => {
                self.focus = Focus::Search;
                self.handle_search_key(key)?;
            }
            KeyCode::Char(_) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.focus = Focus::Search;
                self.handle_search_key(key)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_preview_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => self.focus = Focus::Search,
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_settings()
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_filters()
            }
            KeyCode::Tab => self.focus = next_focus(self.focus, self.preview_available()),
            KeyCode::Enter => self.overlay = Overlay::Actions(ActionMenuState::new()),
            KeyCode::Char('v') if key.modifiers.is_empty() => self.open_viewer(),
            KeyCode::Char('j') | KeyCode::Down => {
                self.preview_scroll = self.preview_scroll.saturating_add(1)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.preview_scroll = self.preview_scroll.saturating_sub(1)
            }
            KeyCode::PageDown => {
                self.preview_scroll = self.preview_scroll.saturating_add(PAGE_STEP)
            }
            KeyCode::PageUp => self.preview_scroll = self.preview_scroll.saturating_sub(PAGE_STEP),
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.resize_preview(-5)
            }
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.resize_preview(5)
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_overlay_key(&mut self, key: KeyEvent) -> Result<()> {
        let viewer_area = self.last_frame_area;
        let viewer_session = if matches!(self.overlay, Overlay::Viewer(_)) {
            self.selected_preview().cloned()
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
                ActionOutcome::Close => self.overlay = Overlay::None,
                ActionOutcome::Run(action) => {
                    self.overlay = Overlay::None;
                    if let Err(error) = self.run_session_action(action) {
                        self.status_message = Some(format!("{error:#}"));
                    }
                }
            },
            Overlay::Viewer(state) => {
                match state.handle_key(key, viewer_area, viewer_session.as_ref(), &self.theme) {
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
                        self.status_message = Some(format!("{error:#}"));
                    }
                }
                _ => {}
            },
            Overlay::None => {}
        }
        Ok(())
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        if !matches!(self.overlay, Overlay::None) {
            return self.handle_overlay_mouse(mouse);
        }

        let Some(areas) = self.last_layout else {
            return Ok(());
        };

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if contains(areas.search, mouse.column, mouse.row) {
                    self.focus = Focus::Search;
                } else if contains(areas.list, mouse.column, mouse.row) {
                    self.focus = Focus::List;
                    if let Some(index) = self.list_index_at(areas.list, mouse.row) {
                        self.select_absolute(index);
                    }
                } else if areas
                    .preview
                    .is_some_and(|preview| contains(preview, mouse.column, mouse.row))
                {
                    self.focus = Focus::Preview;
                }
            }
            MouseEventKind::ScrollDown => {
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
            _ => {}
        }
        Ok(())
    }

    fn handle_overlay_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        if let Overlay::Viewer(state) = &mut self.overlay {
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
        self.worker
            .request_tx
            .send(SearchCommand {
                request_id,
                request: SearchRequest {
                    query: self.query.value().to_owned(),
                    scope: self.scope.clone(),
                    limit: self.result_limit.max(1),
                    sort: self.sort,
                    filters: self.filters.clone(),
                },
            })
            .map_err(|_| anyhow::anyhow!("search worker exited unexpectedly"))?;

        self.latest_search_id = Some(request_id);
        self.search_in_flight = true;
        self.search_error = None;
        self.pending_search = false;
        self.last_edit_at = None;
        Ok(())
    }

    fn collect_search_responses(&mut self) -> Result<bool> {
        let mut changed = false;

        loop {
            match self.worker.response_rx.try_recv() {
                Ok(response) => {
                    if Some(response.request_id) != self.latest_search_id {
                        continue;
                    }

                    self.search_in_flight = false;
                    match response.result {
                        Ok(results) => {
                            self.results = results;
                            if self.results.is_empty() {
                                self.selected = 0;
                            } else {
                                self.selected =
                                    self.selected.min(self.results.len().saturating_sub(1));
                            }
                            self.preview_scroll = 0;
                            self.search_error = None;
                        }
                        Err(error) => {
                            self.search_error = Some(error);
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
            self.preview_scroll = 0;
        }
    }

    fn focus_list(&mut self) {
        self.focus = Focus::List;
    }

    fn select_absolute(&mut self, index: usize) {
        if self.results.is_empty() {
            return;
        }
        self.selected = index.min(self.results.len().saturating_sub(1));
        self.preview_scroll = 0;
    }

    fn list_index_at(&self, area: Rect, row: u16) -> Option<usize> {
        let slot = list::slot_at_row(area, row)?;
        let visible_slots = list::visible_slots(area);
        let offset = self.list_offset(visible_slots);
        let index = offset + slot;
        (index < self.results.len()).then_some(index)
    }

    fn list_offset(&self, max_items: usize) -> usize {
        self.selected.saturating_sub(max_items.saturating_sub(1))
    }

    fn preview_available(&self) -> bool {
        self.preview_visible
    }

    fn trigger_search_now(&mut self) -> Result<()> {
        self.pending_search = false;
        self.last_edit_at = None;
        self.dispatch_search()
    }

    fn open_settings(&mut self) {
        self.overlay = Overlay::Settings(SettingsModalState::new(&self.settings));
    }

    fn apply_settings(&mut self, new_settings: Settings) -> Result<()> {
        self.theme = Theme::from_name(new_settings.theme);
        self.settings = new_settings;
        if let Err(err) = self.settings.save() {
            self.status_message = Some(format!("settings error: {err:#}"));
        } else {
            self.status_message = Some("settings saved".to_owned());
        }
        Ok(())
    }

    fn open_filters(&mut self) {
        self.overlay =
            Overlay::Filters(FilterModalState::new(&self.scope, &self.filters, self.sort));
    }

    fn open_viewer(&mut self) {
        if self.selected_index().is_some() {
            self.overlay = Overlay::Viewer(ViewerState::new());
        }
    }

    fn clear_query(&mut self) {
        if self.query.value().is_empty() {
            return;
        }

        self.query = Input::default();
        self.pending_search = true;
        self.last_edit_at = Some(Instant::now());
    }

    fn resize_preview(&mut self, delta: i16) {
        let next = (self.preview_width_pct as i16 + delta).clamp(25, 60);
        self.preview_width_pct = next as u16;
    }

    fn clamp_scroll_state(&mut self, areas: layout::AppLayout) {
        if let Some(preview_area) = areas.preview {
            let theme = self.theme.clone();
            let query = self.query.value().to_owned();
            let max_scroll = {
                let session = self.selected_preview();
                preview::max_scroll(preview_area, session, &theme, &query)
            };
            self.preview_scroll = self.preview_scroll.min(max_scroll);
        } else {
            self.preview_scroll = 0;
        }

        let viewer_snapshot = match &self.overlay {
            Overlay::Viewer(state) => Some(state.clone()),
            _ => None,
        };
        let Some(viewer_state) = viewer_snapshot else {
            return;
        };

        let theme = self.theme.clone();
        let frame_area = self.last_frame_area;
        let max_scroll = {
            let session = self.selected_preview();
            session
                .map(|session| viewer_state.max_scroll(frame_area, session, &theme))
                .unwrap_or(0)
        };
        if let Overlay::Viewer(state) = &mut self.overlay {
            state.scroll = state.scroll.min(max_scroll);
        }
    }

    fn selected_hit(&self) -> Option<SearchHit> {
        self.results.get(self.selected).cloned()
    }

    fn local_scope_label(&self) -> String {
        match &self.local_scope {
            Scope::Global => "All Projects".to_owned(),
            Scope::CurrentDir(path) => path
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| path.to_string_lossy().into_owned()),
        }
    }

    fn run_session_action(&mut self, action: SessionAction) -> Result<()> {
        match action {
            SessionAction::View => self.open_viewer(),
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
                self.handoff = Some(build_resume_command(&hit, &self.settings)?);
                self.should_quit = true;
            }
            SessionAction::Fork => {
                let Some(hit) = self.selected_hit() else {
                    return Ok(());
                };
                self.handoff = Some(build_fork_command(&hit, &self.settings)?);
                self.should_quit = true;
            }
        }
        Ok(())
    }

    fn export_selected_session(&mut self) -> Result<()> {
        let Some(session) = self.selected_preview().cloned() else {
            bail!("no session selected");
        };
        let path = export_path_for_session(&session)?;
        let rendered = session_to_plain_text(&session);
        fs::write(&path, rendered)
            .with_context(|| format!("failed to write {}", path.display()))?;
        self.status_message = Some(format!("exported {}", path.display()));
        Ok(())
    }

    fn delete_selected_session(&mut self) -> Result<()> {
        let Some(hit) = self.selected_hit() else {
            return Ok(());
        };
        let file_already_missing = match fs::remove_file(&hit.session.file_path) {
            Ok(()) => false,
            Err(error) if error.kind() == io::ErrorKind::NotFound => true,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to delete {}", hit.session.file_path.display()));
            }
        };
        self.results
            .retain(|result| result.session.file_path != hit.session.file_path);
        if self.selected >= self.results.len() {
            self.selected = self.results.len().saturating_sub(1);
        }
        self.preview_scroll = 0;
        self.preview_cache.remove(&hit.session.file_path);
        match self.manager.sync_best_effort(false)? {
            SyncOutcome::Completed(_) => {
                self.status_message = Some(if file_already_missing {
                    format!(
                        "removed missing session {}",
                        hit.session.file_path.display()
                    )
                } else {
                    format!("deleted {}", hit.session.file_path.display())
                });
                self.trigger_search_now()?;
            }
            SyncOutcome::Busy => {
                self.status_message = Some(if file_already_missing {
                    format!(
                        "removed missing session {} · index refresh deferred",
                        hit.session.file_path.display()
                    )
                } else {
                    format!(
                        "deleted {} · index refresh deferred",
                        hit.session.file_path.display()
                    )
                });
            }
        }
        Ok(())
    }

    fn copy_to_clipboard(&mut self, value: &str, label: &str) -> Result<()> {
        crate::clipboard::set_text(value).context("failed to set clipboard contents")?;
        self.status_message = Some(format!("copied {label}"));
        Ok(())
    }

    fn render_delete_confirm(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let popup = layout::centered_rect(area, 44, 20);
        frame.render_widget(Clear, popup);
        let hit = self.selected_hit();
        let title = hit
            .as_ref()
            .map(|hit| {
                hit.session
                    .custom_title
                    .clone()
                    .unwrap_or_else(|| hit.session.project.clone())
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
            Line::from(Span::styled(
                "Press y to delete or Esc to cancel.",
                Style::default().fg(theme.muted),
            )),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(theme.border_style(true))
                .title("Confirm Delete"),
        );
        frame.render_widget(paragraph, popup);
    }

    fn status_text(&self) -> String {
        let mut text = format!("{} results", self.results.len());
        let filter_count = self.filters.active_count();
        if filter_count > 0 {
            text.push_str(&format!(" · {filter_count} filters"));
        }
        if matches!(self.sort, SortMode::Time) {
            text.push_str(" · time sort");
        }
        if self.pending_search {
            text.push_str(" · pending");
        } else if self.search_in_flight {
            text.push_str(" · searching");
        }
        if let Some(error) = &self.search_error {
            text.push_str(" · ");
            text.push_str(error);
        }
        if let Some(message) = &self.status_message {
            text.push_str(" · ");
            text.push_str(message);
        }
        text
    }
}

fn search_worker_loop(
    search_engine: SearchEngine,
    request_rx: Receiver<SearchCommand>,
    response_tx: Sender<SearchResponse>,
) {
    while let Ok(mut command) = request_rx.recv() {
        for newer in request_rx.try_iter() {
            command = newer;
        }

        let result = search_engine
            .search(&command.request)
            .map_err(|error| format!("{error:#}"));
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

fn next_focus(current: Focus, preview_available: bool) -> Focus {
    match (current, preview_available) {
        (Focus::Search, _) => Focus::List,
        (Focus::List, true) => Focus::Preview,
        (Focus::List, false) => Focus::Search,
        (Focus::Preview, _) => Focus::Search,
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

fn build_resume_command(hit: &SearchHit, settings: &Settings) -> Result<ExternalCommand> {
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
            ExternalCommand { program, args, cwd }
        }
        crate::parse::Agent::Codex => {
            let (program, mut args) = settings.codex_program_and_args();
            args.extend(["resume".to_owned(), hit.session.session_id.clone()]);
            ExternalCommand { program, args, cwd }
        }
    };
    Ok(command)
}

fn build_fork_command(hit: &SearchHit, settings: &Settings) -> Result<ExternalCommand> {
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
            ExternalCommand { program, args, cwd }
        }
        crate::parse::Agent::Codex => {
            let (program, mut args) = settings.codex_program_and_args();
            args.extend(["fork".to_owned(), hit.session.session_id.clone()]);
            ExternalCommand { program, args, cwd }
        }
    };
    Ok(command)
}

fn export_path_for_session(session: &Session) -> Result<PathBuf> {
    let stem = session
        .custom_title
        .clone()
        .unwrap_or_else(|| session.session_id.clone());
    let sanitized = sanitize_filename(&stem);
    let cwd = env::current_dir().context("failed to resolve current directory")?;
    let mut candidate = cwd.join(format!("{sanitized}.txt"));
    let mut suffix = 1usize;
    while candidate.exists() {
        candidate = cwd.join(format!("{sanitized}-{suffix}.txt"));
        suffix += 1;
    }
    Ok(candidate)
}

fn sanitize_filename(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    let trimmed = sanitized.trim_matches('-');
    if trimmed.is_empty() {
        "aics-session".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn session_to_plain_text(session: &Session) -> String {
    let mut output = String::new();
    for message in &session.messages {
        let timestamp = message
            .timestamp
            .map(|time| time.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        if timestamp.is_empty() {
            output.push_str(&format!("{}\n", message.role));
        } else {
            output.push_str(&format!("{} {}\n", message.role, timestamp));
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
    use std::sync::mpsc;
    use std::path::PathBuf;

    use anyhow::anyhow;
    use ratatui::crossterm::event::KeyCode;
    use tempfile::TempDir;
    use tui_input::Input;

    use crate::index::{IndexManager, IndexPaths, Scope, SearchFilters, SearchRequest, SortMode};
    use crate::index::writer::StoredSession;
    use crate::index::SearchHit;
    use crate::parse::{Agent, DerivationType};
    use crate::settings::Settings;

    use super::{
        build_fork_command, build_resume_command, finalize_run_result, App, AppExit, Focus,
        SearchWorker,
    };

    #[test]
    fn builds_resume_commands_for_both_agents() {
        let settings = Settings::default();
        let claude = build_resume_command(&sample_hit(Agent::Claude), &settings).unwrap();
        assert_eq!(claude.program, "claude");
        assert_eq!(claude.args, vec!["--resume", "session-123"]);

        let codex = build_resume_command(&sample_hit(Agent::Codex), &settings).unwrap();
        assert_eq!(codex.program, "codex");
        assert_eq!(codex.args, vec!["resume", "session-123"]);
    }

    #[test]
    fn builds_fork_commands_for_both_agents() {
        let settings = Settings::default();
        let claude = build_fork_command(&sample_hit(Agent::Claude), &settings).unwrap();
        assert_eq!(
            claude.args,
            vec!["--resume", "session-123", "--fork-session"]
        );

        let codex = build_fork_command(&sample_hit(Agent::Codex), &settings).unwrap();
        assert_eq!(codex.args, vec!["fork", "session-123"]);
    }

    #[test]
    fn custom_command_prepends_args() {
        let settings = Settings {
            claude_command: "claude --profile work".to_owned(),
            codex_command: "/usr/local/bin/codex".to_owned(),
            ..Settings::default()
        };
        let claude = build_resume_command(&sample_hit(Agent::Claude), &settings).unwrap();
        assert_eq!(claude.program, "claude");
        assert_eq!(
            claude.args,
            vec!["--profile", "work", "--resume", "session-123"]
        );

        let codex = build_resume_command(&sample_hit(Agent::Codex), &settings).unwrap();
        assert_eq!(codex.program, "/usr/local/bin/codex");
        assert_eq!(codex.args, vec!["resume", "session-123"]);
    }

    #[test]
    fn down_from_search_moves_selection_without_changing_focus() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude), sample_hit(Agent::Codex)];
        app.selected = 0;

        app.handle_search_key(crossterm_key(KeyCode::Down)).unwrap();

        assert_eq!(app.focus, Focus::Search);
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn enter_from_search_focuses_list_without_skipping_first_result() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude), sample_hit(Agent::Codex)];
        app.selected = 0;

        app.handle_search_key(crossterm_key(KeyCode::Enter)).unwrap();

        assert_eq!(app.focus, Focus::List);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn escape_from_search_quits_when_query_is_empty() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude)];
        app.focus = Focus::Search;

        app.handle_search_key(crossterm_key(KeyCode::Esc)).unwrap();

        assert!(app.should_quit);
        assert_eq!(app.focus, Focus::Search);
    }

    #[test]
    fn escape_from_search_clears_query_when_query_exists() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude)];
        app.focus = Focus::Search;
        app.query = Input::default().with_value("alpha".to_owned());

        app.handle_search_key(crossterm_key(KeyCode::Esc)).unwrap();

        assert!(!app.should_quit);
        assert_eq!(app.query.value(), "");
        assert_eq!(app.focus, Focus::Search);
    }

    #[test]
    fn escape_from_list_quits_when_search_query_is_empty() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude)];
        app.focus = Focus::List;

        app.handle_list_key(crossterm_key(KeyCode::Esc)).unwrap();

        assert!(app.should_quit);
        assert_eq!(app.focus, Focus::List);
    }

    #[test]
    fn escape_from_list_returns_to_search_when_query_exists() {
        let mut app = test_app();
        app.results = vec![sample_hit(Agent::Claude)];
        app.focus = Focus::List;
        app.query = Input::default().with_value("alpha".to_owned());

        app.handle_list_key(crossterm_key(KeyCode::Esc)).unwrap();

        assert!(!app.should_quit);
        assert_eq!(app.focus, Focus::Search);
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
            .status_message
            .as_deref()
            .is_some_and(|message| message.contains("removed missing session")));
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
            .status_message
            .as_deref()
            .is_some_and(|message| message.contains("failed to delete")));
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
        let temp = TempDir::new().unwrap();
        let manager = IndexManager::with_paths(IndexPaths::from_root(temp.path()));
        let (request_tx, request_rx) = mpsc::channel();
        std::thread::spawn(move || while request_rx.recv().is_ok() {});
        let (_response_tx, response_rx) = mpsc::channel();
        let worker = SearchWorker {
            request_tx,
            response_rx,
        };

        App::new(
            manager,
            worker,
            SearchRequest {
                query: String::new(),
                scope: Scope::Global,
                limit: 10,
                sort: SortMode::Time,
                filters: SearchFilters::default(),
            },
            Settings::default(),
        )
    }

    fn crossterm_key(code: KeyCode) -> ratatui::crossterm::event::KeyEvent {
        ratatui::crossterm::event::KeyEvent::new(code, ratatui::crossterm::event::KeyModifiers::NONE)
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
}
