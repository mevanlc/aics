use std::env;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Result};
use chrono::{Local, NaiveDate, TimeZone, Utc};
use clap::{Parser, ValueEnum};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use log::warn;
use ratatui::style::Color;

use aics::export::write_session_markdown;
use aics::index::{
    IndexManager, Scope, SearchFilters, SearchHit, SearchRequest, SortMode, SyncOutcome,
    SyncProgress, TrashFilter,
};
use aics::live::LiveSessionTracker;
use aics::logging::{LoggingHandle, LoggingMode};
use aics::parse::{parse_session_file, Agent};
use aics::rules::{
    default_rules_path, print_report, run_rules_with_progress, write_default_rules_dts,
    RuleSelection, RulesMode, RulesOptions, RulesProgress,
};
use aics::scan::ResolvedPaths;
use aics::settings::{DefaultFilter, DefaultFilterScope, DisplayOptions, LoadedSettings, Settings};
use aics::tui::theme::{PaletteEntry, Theme};
use aics::tui::{run_app, run_rules_preview_app};

#[derive(Debug, Parser)]
#[command(
    name = "aics",
    version,
    about = "Search local Claude Code and Codex CLI session history",
    after_help = "Examples:\n  aics deploy\n      Search sessions for the current directory and open the TUI.\n\n  aics -g --agent claude --after 2026-03-01 deploy\n      Search all Claude sessions after 2026-03-01.\n\n  aics --json -g --sort-by relevance \"vector db\"\n      Print matching sessions as JSONL instead of launching the TUI.\n\n  aics -g --export ./transcripts \"vector db\"\n      Write every matching session to ./transcripts as Markdown.\n\nDate filters:\n  --after and --before accept YYYY-MM-DD or RFC3339 timestamps.\n\nScope:\n  By default, searches are scoped to the current directory.\n  Use --global to search everything, --no-global to override a saved global startup scope,\n  or --dir PATH[:BRANCH] to target a project."
)]
struct Cli {
    #[arg(
        long = "print-palettes",
        help = "Print built-in theme palettes as ANSI color cards and exit"
    )]
    print_palettes: bool,
    #[arg(
        short = 'g',
        long = "global",
        help = "Search across all indexed sessions instead of scoping to the current directory"
    )]
    global: bool,
    #[arg(
        long = "no-global",
        conflicts_with = "global",
        help = "Start in project-local mode even when the saved default scope is global"
    )]
    no_global: bool,
    #[arg(
        long = "dir",
        value_name = "PATH[:BRANCH]",
        help = "Search sessions for a specific project directory; append :BRANCH to also filter by branch"
    )]
    dir: Option<String>,
    #[arg(
        long = "branch",
        help = "Limit results to a branch name; conflicts with a different branch embedded in --dir"
    )]
    branch: Option<String>,
    #[arg(
        short = 'n',
        long = "num-results",
        default_value_t = 2000,
        help = "Maximum number of results to load into the TUI or emit as JSONL"
    )]
    num_results: usize,
    #[arg(
        long = "agent",
        value_parser = ["claude", "codex"],
        help = "Only include sessions recorded by one agent"
    )]
    agent: Option<String>,
    #[arg(
        long = "session",
        value_name = "SESSIONID",
        help = "Only include the session with this exact session id"
    )]
    session: Option<String>,
    #[arg(
        long = "after",
        help = "Only include sessions on or after this date or timestamp"
    )]
    after: Option<String>,
    #[arg(
        long = "before",
        help = "Only include sessions on or before this date or timestamp"
    )]
    before: Option<String>,
    #[arg(
        long = "min-lines",
        help = "Only include sessions with at least this many content lines"
    )]
    min_lines: Option<usize>,
    #[arg(long = "no-original", help = "Exclude original/root sessions")]
    no_original: bool,
    #[arg(long = "no-trimmed", help = "Exclude trimmed sessions")]
    no_trimmed: bool,
    #[arg(
        long = "no-rollover",
        visible_alias = "no-continued",
        help = "Exclude continued sessions (also called rollover sessions)"
    )]
    no_rollover: bool,
    #[arg(
        long = "sub-agent",
        help = "Include sub-agent or sidechain sessions in the result set"
    )]
    sub_agent: bool,
    #[arg(
        long = "live",
        help = "Only include sessions that currently appear to be live"
    )]
    live: bool,
    #[arg(
        long = "trashed",
        value_enum,
        help = "Search trashed sessions: no, yes, or both"
    )]
    trashed: Option<CliTrashFilter>,
    #[arg(
        long = "json",
        help = "Print JSONL hits or rule records to stdout instead of launching the interactive TUI"
    )]
    json: bool,
    #[arg(
        long = "export",
        value_name = "DIR",
        conflicts_with_all = ["json", "preview_rules", "apply_rules", "benchmark_rules"],
        help = "Write every matching session to DIR as Markdown instead of launching the TUI"
    )]
    export: Option<PathBuf>,
    #[arg(
        long = "hide",
        value_name = "ITEM",
        value_enum,
        help = "Hide a transcript part; repeatable. Adds to saved display options in the TUI, and is the only thing hidden from an export"
    )]
    hide: Vec<CliHideItem>,
    #[arg(
        long = "preview-rules",
        conflicts_with = "apply_rules",
        help = "Evaluate JavaScript rules and review proposed actions without changing files"
    )]
    preview_rules: bool,
    #[arg(
        long = "apply-rules",
        conflicts_with = "preview_rules",
        help = "Evaluate JavaScript rules and apply supported actions"
    )]
    apply_rules: bool,
    #[arg(
        long = "no-apply-rules",
        conflicts_with_all = ["preview_rules", "apply_rules", "benchmark_rules", "rules"],
        help = "Disable automatic application of rules configured with applyAtStartup: true"
    )]
    no_apply_rules: bool,
    #[arg(
        long = "benchmark-rules",
        hide = true,
        conflicts_with_all = ["preview_rules", "apply_rules"]
    )]
    benchmark_rules: bool,
    #[arg(
        long = "rules",
        value_name = "PATH",
        help = "Use a custom JavaScript rules file without reading or writing the rules cache"
    )]
    rules: Option<PathBuf>,
    #[arg(
        long = "write-rules-dts",
        help = "Write JavaScript rules TypeScript declarations to ~/.config/aics/rules.d.ts and exit"
    )]
    write_rules_dts: bool,
    #[arg(
        long = "sort-by",
        value_enum,
        help = "Order results by time or text relevance"
    )]
    sort_by: Option<CliSort>,
    #[arg(
        long = "rebuild-index",
        help = "Rebuild the local search index before searching"
    )]
    rebuild_index: bool,
    #[arg(
        long = "claude-home",
        value_name = "PATH",
        help = "Override the Claude Code home directory for this run"
    )]
    claude_home: Option<PathBuf>,
    #[arg(
        long = "codex-home",
        value_name = "PATH",
        help = "Override the Codex CLI home directory for this run"
    )]
    codex_home: Option<PathBuf>,
    #[arg(long = "delete-index", conflicts_with = "rebuild_index")]
    delete_index: bool,
    #[arg(
        long = "progress",
        value_enum,
        default_value_t = CliProgress::Stderr
    )]
    progress: CliProgress,
    #[arg(help = "Optional search query to run immediately or prefill in the TUI")]
    query: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliSort {
    Time,
    Relevance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliProgress {
    None,
    Stderr,
    Stdout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliTrashFilter {
    No,
    Yes,
    Both,
}

/// The six transcript parts the filter modal can toggle. Each name is the
/// matching `display_options` settings key with `hide_` stripped, so what is
/// typed here is greppable in `settings.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliHideItem {
    ProjectDocsAutodump,
    SkillTextInjection,
    ToolCalls,
    ToolResults,
    AgentReplies,
    UserMessages,
}

impl CliHideItem {
    /// Turn hiding on for this item. `--hide` never turns hiding *off*, so it
    /// composes the same way against an all-visible export baseline and against
    /// the saved display options the TUI starts from.
    fn apply(self, options: &mut DisplayOptions) {
        match self {
            Self::ProjectDocsAutodump => options.hide_project_docs_autodump = true,
            Self::SkillTextInjection => options.hide_skill_text_injection = true,
            Self::ToolCalls => options.hide_tool_calls = true,
            Self::ToolResults => options.hide_tool_results = true,
            Self::AgentReplies => options.hide_agent_replies = true,
            Self::UserMessages => options.hide_user_messages = true,
        }
    }
}

fn hidden_from(base: DisplayOptions, items: &[CliHideItem]) -> DisplayOptions {
    let mut options = base;
    for item in items {
        item.apply(&mut options);
    }
    options
}

impl From<CliSort> for SortMode {
    fn from(value: CliSort) -> Self {
        match value {
            CliSort::Time => SortMode::Time,
            CliSort::Relevance => SortMode::Relevance,
        }
    }
}

impl From<CliTrashFilter> for TrashFilter {
    fn from(value: CliTrashFilter) -> Self {
        match value {
            CliTrashFilter::No => TrashFilter::No,
            CliTrashFilter::Yes => TrashFilter::Yes,
            CliTrashFilter::Both => TrashFilter::Both,
        }
    }
}

impl Cli {
    fn will_enter_tui(&self) -> bool {
        !self.json
            && self.export.is_none()
            && !self.apply_rules
            && !self.benchmark_rules
            && !self.write_rules_dts
            && !self.delete_index
            && !self.print_palettes
    }

    /// Modes that own stdout, so progress bars and other chatter must not go there.
    fn stdout_reserved(&self) -> bool {
        self.json || self.export.is_some()
    }

    fn logging_mode(&self) -> LoggingMode {
        if self.will_enter_tui() {
            LoggingMode::Interactive
        } else {
            LoggingMode::Command
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let logging = aics::logging::init(cli.logging_mode())?;
    if cli.print_palettes {
        print!("{}", render_palettes());
        return Ok(());
    }
    if cli.write_rules_dts {
        let path = write_default_rules_dts()?;
        println!("{}", path.display());
        return Ok(());
    }
    let resolved_paths =
        ResolvedPaths::discover(cli.claude_home.as_deref(), cli.codex_home.as_deref())?;
    let index_paths = aics::index::IndexPaths::discover_for_roots(&resolved_paths.roots)?;
    let rules_cache_path = index_paths.cache_root.join("rules-cache.json");
    let startup_rules_cache_path = index_paths.cache_root.join("startup-rules-cache.json");
    let manager = IndexManager::with_paths(index_paths);
    if cli.delete_index {
        manager.delete_index()?;
        return Ok(());
    }
    validate_terminal_mode(&cli)?;
    let LoadedSettings { settings, warning } = Settings::load_with_recovery();
    let mut settings_warning = combine_startup_warnings(&logging, warning.clone());
    if let Some(warning) = warning.as_deref() {
        eprintln!("aics: {warning}");
    }
    manager.write_profile_metadata(&resolved_paths)?;
    let sync_outcome =
        if let Some(draw_target) = progress_draw_target(cli.progress, cli.stdout_reserved()) {
            let mut progress = StartupProgress::new(draw_target);
            let outcome = manager.sync_with_roots_and_progress(
                &resolved_paths.roots,
                cli.rebuild_index,
                |event| progress.update(event),
            );
            progress.finish();
            SyncOutcome::Completed(outcome?)
        } else {
            manager.sync_with_roots_best_effort(&resolved_paths.roots, cli.rebuild_index)?
        };
    match sync_outcome {
        SyncOutcome::Completed(_) | SyncOutcome::Busy => {}
    }
    let request = build_request(&cli)?;

    if let Some(mode) = rules_mode(&cli) {
        let rules_path = match cli.rules.clone() {
            Some(path) => path,
            None => default_rules_path()?,
        };
        let rules_scope = request.scope.clone();
        let rules_filters = request.filters.clone();
        let rules_options = RulesOptions {
            rules_path,
            cache_path: (!cli.benchmark_rules && cli.rules.is_none())
                .then_some(rules_cache_path.clone()),
            mode,
            selection: RuleSelection::All,
            json: cli.json,
            scope: rules_scope.clone(),
            filters: rules_filters.clone(),
        };
        let report = if let Some(draw_target) =
            progress_draw_target(cli.progress, cli.stdout_reserved())
        {
            let mut progress = RulesProcessingProgress::new(draw_target);
            let report = run_rules_with_progress(&resolved_paths.roots, &rules_options, |event| {
                progress.update(event)
            });
            progress.finish();
            report?
        } else {
            run_rules_with_progress(&resolved_paths.roots, &rules_options, |_| {})?
        };
        if cli.benchmark_rules {
            return Ok(());
        }
        if mode == RulesMode::Preview && !cli.json {
            let initial_request = SearchRequest {
                query: String::new(),
                scope: rules_scope,
                limit: report.preview_matches.len().max(1),
                sort: request.sort,
                filters: rules_filters,
            };
            return run_rules_preview_app(
                manager,
                report,
                initial_request,
                settings,
                resolved_paths.homes,
                resolved_paths.roots,
                settings_warning,
            );
        }

        print_report(&report, cli.json, mode)?;
        if mode == RulesMode::Apply && !report.applied.is_empty() {
            manager.sync_with_roots_best_effort(&resolved_paths.roots, false)?;
        }
        return Ok(());
    }

    if !cli.no_apply_rules {
        let rules_path = match cli.rules.clone() {
            Some(path) => Some(path),
            None => {
                let path = default_rules_path()?;
                path.exists().then_some(path)
            }
        };
        if let Some(rules_path) = rules_path {
            let startup_options = RulesOptions {
                rules_path,
                cache_path: cli.rules.is_none().then_some(startup_rules_cache_path),
                mode: RulesMode::Apply,
                selection: RuleSelection::ApplyAtStartup,
                json: false,
                scope: Scope::Global,
                filters: SearchFilters::default(),
            };
            let report = if let Some(draw_target) =
                progress_draw_target(cli.progress, cli.stdout_reserved())
            {
                let mut progress = RulesProcessingProgress::new(draw_target);
                let report =
                    run_rules_with_progress(&resolved_paths.roots, &startup_options, |event| {
                        progress.update(event)
                    });
                progress.finish();
                report?
            } else {
                run_rules_with_progress(&resolved_paths.roots, &startup_options, |_| {})?
            };

            for error in &report.errors {
                warn!(
                    "startup rule failed for {}: {}",
                    error.path.display(),
                    error.error
                );
            }
            for skipped in &report.skipped {
                warn!(
                    "startup rule action skipped for {}: {}",
                    skipped.path.display(),
                    skipped.skip_reason
                );
            }
            let startup_issue_count = report.errors.len() + report.skipped.len();
            if startup_issue_count > 0 {
                append_startup_warning(
                    &mut settings_warning,
                    format!("{startup_issue_count} startup rule actions failed or were skipped"),
                );
            }
            if !report.applied.is_empty() {
                manager.sync_with_roots_best_effort(&resolved_paths.roots, false)?;
            }
        }
    }

    let search_engine = manager.open_search_engine_with_live_sessions(
        LiveSessionTracker::from_claude_sessions_dir(resolved_paths.claude_sessions.clone()),
    )?;

    if cli.json {
        for hit in search_engine.search(&request)? {
            println!("{}", serde_json::to_string(&hit)?);
        }
        return Ok(());
    }

    // Both non-interactive modes run before the saved startup filter is applied,
    // so a scripted search selects exactly what its flags asked for. Exports
    // likewise start from an all-visible baseline rather than saved display
    // options, so only an explicit `--hide` removes anything.
    if let Some(export_dir) = cli.export.as_deref() {
        return export_sessions(
            search_engine.search(&request)?,
            export_dir,
            hidden_from(DisplayOptions::SHOW_ALL, &cli.hide),
        );
    }

    let mut request = request;
    apply_default_filter_to_request(&mut request, &cli, settings.default_filter.as_ref());
    let mut settings = settings;
    settings.display_options = hidden_from(settings.display_options, &cli.hide);

    run_app(
        manager,
        search_engine,
        request,
        settings,
        resolved_paths.homes,
        resolved_paths.roots,
        settings_warning,
    )
}

/// Write each hit to `dir` as Markdown, listing the written paths on stdout so
/// the run composes with a shell pipeline. An unreadable session is reported and
/// skipped rather than aborting the batch.
fn export_sessions(
    hits: Vec<SearchHit>,
    dir: &Path,
    display_options: DisplayOptions,
) -> Result<()> {
    let mut exported = 0usize;
    let mut skipped = 0usize;

    for hit in hits {
        let source = hit.session.file_path.as_path();
        let session = match parse_session_file(hit.session.agent, source) {
            Ok(Some(session)) => session,
            Ok(None) => {
                skipped += 1;
                eprintln!("aics: skipped {}: no session content", source.display());
                continue;
            }
            Err(error) => {
                skipped += 1;
                warn!("failed to parse {}: {error:#}", source.display());
                eprintln!("aics: skipped {}: {error:#}", source.display());
                continue;
            }
        };

        let written = write_session_markdown(dir, &session, display_options)?;
        println!("{}", written.display());
        exported += 1;
    }

    if exported == 0 {
        eprintln!("aics: no sessions matched; nothing exported");
    } else {
        eprintln!("aics: exported {exported} session(s) to {}", dir.display());
    }
    if skipped > 0 {
        eprintln!("aics: skipped {skipped} unreadable session(s)");
    }

    Ok(())
}

fn combine_startup_warnings(logging: &LoggingHandle, settings: Option<String>) -> Option<String> {
    match (logging.bootstrap_warning.as_deref(), settings) {
        (Some(logging), Some(settings)) => Some(format!("{logging}; {settings}")),
        (Some(logging), None) => Some(logging.to_owned()),
        (None, settings) => settings,
    }
}

fn append_startup_warning(warning: &mut Option<String>, extra: String) {
    *warning = Some(match warning.take() {
        Some(existing) => format!("{existing}; {extra}"),
        None => extra,
    });
}

fn validate_terminal_mode(cli: &Cli) -> Result<()> {
    if cli.stdout_reserved() && cli.progress == CliProgress::Stdout {
        bail!("--progress stdout conflicts with --json and --export because stdout is reserved for their output");
    }

    if rules_mode(cli).is_some() && cli.query.is_some() {
        bail!("rules mode does not support a search query yet");
    }

    if rules_mode(cli).is_some() && cli.live {
        bail!("rules mode does not support --live yet");
    }

    if !cli.delete_index
        && !cli.json
        && cli.export.is_none()
        && rules_mode(cli).is_none()
        && !std::io::stdout().is_terminal()
    {
        bail!("stdout is not a terminal; use --json for non-interactive output");
    }

    Ok(())
}

fn rules_mode(cli: &Cli) -> Option<RulesMode> {
    if cli.preview_rules || cli.benchmark_rules {
        Some(RulesMode::Preview)
    } else if cli.apply_rules {
        Some(RulesMode::Apply)
    } else {
        None
    }
}

fn progress_draw_target(mode: CliProgress, stdout_reserved: bool) -> Option<ProgressDrawTarget> {
    match mode {
        CliProgress::None => None,
        CliProgress::Stderr => std::io::stderr()
            .is_terminal()
            .then(ProgressDrawTarget::stderr),
        CliProgress::Stdout if stdout_reserved => None,
        CliProgress::Stdout => std::io::stdout()
            .is_terminal()
            .then(ProgressDrawTarget::stdout),
    }
}

struct StartupProgress {
    bar: ProgressBar,
}

impl StartupProgress {
    fn new(draw_target: ProgressDrawTarget) -> Self {
        let bar = ProgressBar::with_draw_target(None, draw_target);
        bar.set_style(discovering_style());
        bar.enable_steady_tick(Duration::from_millis(100));
        Self { bar }
    }

    fn update(&mut self, event: SyncProgress) {
        match event {
            SyncProgress::Discovering { discovered } => {
                self.bar.set_message(format!("{discovered} found"));
            }
            SyncProgress::IndexingStarted { total } => {
                self.bar.set_style(indexing_style());
                self.bar.set_message(String::new());
                self.bar.set_length(total as u64);
                self.bar.set_position(0);
            }
            SyncProgress::IndexingProgress { processed, .. } => {
                self.bar.set_position(processed as u64);
            }
        }
    }

    fn finish(self) {
        self.bar.finish_and_clear();
    }
}

fn discovering_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner:.green} Discovering sessions {msg:.dim}")
        .expect("valid progress template")
}

fn indexing_style() -> ProgressStyle {
    ProgressStyle::with_template("Indexing {wide_bar:.green/blue} {pos}/{len} ({eta})")
        .expect("valid progress template")
        .progress_chars("█▉▊▋▌▍▎▏ ")
}

struct RulesProcessingProgress {
    bar: ProgressBar,
}

impl RulesProcessingProgress {
    fn new(draw_target: ProgressDrawTarget) -> Self {
        let bar = ProgressBar::with_draw_target(None, draw_target);
        bar.set_style(rules_processing_style());
        Self { bar }
    }

    fn update(&mut self, event: RulesProgress) {
        match event {
            RulesProgress::ProcessingStarted { total } => {
                self.bar.set_length(total as u64);
                self.bar.set_position(0);
            }
            RulesProgress::ProcessingProgress { processed, .. } => {
                self.bar.set_position(processed as u64);
            }
        }
    }

    fn finish(self) {
        self.bar.finish_and_clear();
    }
}

fn rules_processing_style() -> ProgressStyle {
    ProgressStyle::with_template("Processing rules {wide_bar:.green/blue} {pos}/{len} ({eta})")
        .expect("valid progress template")
        .progress_chars("█▉▊▋▌▍▎▏ ")
}

fn build_request(cli: &Cli) -> Result<SearchRequest> {
    let (scope, dir_branch) = scope_from_cli(cli)?;
    let branch = match (cli.branch.clone(), dir_branch) {
        (Some(from_flag), Some(from_dir)) if from_flag != from_dir => {
            bail!("--branch conflicts with the branch embedded in --dir")
        }
        (Some(from_flag), _) => Some(from_flag),
        (None, from_dir) => from_dir,
    };

    Ok(SearchRequest {
        query: cli.query.clone().unwrap_or_default(),
        scope,
        limit: cli.num_results.max(1),
        sort: cli.sort_by.unwrap_or(CliSort::Time).into(),
        filters: SearchFilters {
            agent: cli.agent.as_deref().and_then(parse_agent_arg),
            session_id: cli.session.as_deref().and_then(optional_cli_string),
            branch,
            after_ts: cli.after.as_deref().map(parse_after_date).transpose()?,
            before_ts: cli.before.as_deref().map(parse_before_date).transpose()?,
            min_lines: cli.min_lines,
            include_original: !cli.no_original,
            include_trimmed: !cli.no_trimmed,
            include_continued: !cli.no_rollover,
            include_sub_agents: cli.sub_agent,
            live_only: cli.live,
            trashed: cli.trashed.unwrap_or(CliTrashFilter::No).into(),
        },
    })
}

fn apply_default_filter_to_request(
    request: &mut SearchRequest,
    cli: &Cli,
    default_filter: Option<&DefaultFilter>,
) {
    let Some(default_filter) = default_filter else {
        return;
    };

    if !cli.global
        && !cli.no_global
        && cli.dir.is_none()
        && matches!(default_filter.scope, DefaultFilterScope::Global)
    {
        request.scope = Scope::Global;
    }

    if cli.sort_by.is_none() {
        request.sort = default_filter.sort;
    }

    if cli.agent.is_none() {
        request.filters.agent = default_filter.filters.agent;
    }
    if cli.session.is_none() {
        request.filters.session_id = default_filter.filters.session_id.clone();
    }
    if cli.branch.is_none() && !dir_arg_has_branch(cli.dir.as_deref()) {
        request.filters.branch = default_filter.filters.branch.clone();
    }
    if cli.after.is_none() {
        request.filters.after_ts = default_filter.filters.after_ts;
    }
    if cli.before.is_none() {
        request.filters.before_ts = default_filter.filters.before_ts;
    }
    if cli.min_lines.is_none() {
        request.filters.min_lines = default_filter.filters.min_lines;
    }
    if !cli.no_original {
        request.filters.include_original = default_filter.filters.include_original;
    }
    if !cli.no_trimmed {
        request.filters.include_trimmed = default_filter.filters.include_trimmed;
    }
    if !cli.no_rollover {
        request.filters.include_continued = default_filter.filters.include_continued;
    }
    if !cli.sub_agent {
        request.filters.include_sub_agents = default_filter.filters.include_sub_agents;
    }
    if !cli.live {
        request.filters.live_only = default_filter.filters.live_only;
    }
    if cli.trashed.is_none() {
        request.filters.trashed = default_filter.filters.trashed;
    }
}

fn dir_arg_has_branch(raw_dir: Option<&str>) -> bool {
    raw_dir
        .and_then(|raw_dir| parse_dir_arg(raw_dir).1)
        .is_some()
}

fn scope_from_cli(cli: &Cli) -> Result<(Scope, Option<String>)> {
    if cli.global {
        return Ok((Scope::Global, None));
    }

    if let Some(raw_dir) = &cli.dir {
        let (path, branch) = parse_dir_arg(raw_dir);
        return Ok((Scope::current_dir(path), branch));
    }

    Ok((Scope::current_dir(env::current_dir()?), None))
}

fn parse_agent_arg(raw: &str) -> Option<Agent> {
    match raw {
        "claude" => Some(Agent::Claude),
        "codex" => Some(Agent::Codex),
        _ => None,
    }
}

fn optional_cli_string(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn parse_dir_arg(raw: &str) -> (PathBuf, Option<String>) {
    let split_index = if cfg!(windows) && raw.chars().nth(1) == Some(':') {
        raw[2..].rfind(':').map(|index| index + 2)
    } else {
        raw.rfind(':')
    };

    let Some(index) = split_index else {
        return (PathBuf::from(raw), None);
    };

    let path = &raw[..index];
    let branch = raw[index + 1..].trim();
    if branch.is_empty() {
        (PathBuf::from(path), None)
    } else {
        (PathBuf::from(path), Some(branch.to_owned()))
    }
}

fn parse_after_date(raw: &str) -> Result<u64> {
    parse_date(raw, false)
}

fn parse_before_date(raw: &str) -> Result<u64> {
    parse_date(raw, true)
}

fn parse_date(raw: &str, end_of_day: bool) -> Result<u64> {
    if let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Ok(timestamp.with_timezone(&Utc).timestamp().max(0) as u64);
    }

    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .map_err(|_| anyhow::anyhow!("invalid date `{raw}`; use RFC3339 or YYYY-MM-DD"))?;
    let time = if end_of_day {
        date.and_hms_opt(23, 59, 59)
    } else {
        date.and_hms_opt(0, 0, 0)
    }
    .expect("valid date");
    let local = Local
        .from_local_datetime(&time)
        .single()
        .ok_or_else(|| anyhow::anyhow!("ambiguous local date `{raw}`"))?;
    Ok(local.with_timezone(&Utc).timestamp().max(0) as u64)
}

fn render_palettes() -> String {
    let theme_specs: Vec<_> = aics::settings::ThemeName::ALL
        .into_iter()
        .map(|name| (name.label(), Theme::from_name(name)))
        .collect();
    let shared_pairs = palette_pairs(
        &theme_specs
            .iter()
            .map(|(_, theme)| theme.palette_entries().to_vec())
            .collect::<Vec<_>>(),
    );
    let columns: Vec<_> = theme_specs
        .into_iter()
        .map(|(label, theme)| render_palette_column(label, theme, &shared_pairs))
        .collect();
    let body_rows = columns
        .iter()
        .map(|column| column.lines.len())
        .max()
        .unwrap_or(0);

    let mut out = String::new();
    out.push_str(&join_row(
        &columns
            .iter()
            .map(|column| pad_plain(column.label, column.width))
            .collect::<Vec<_>>(),
        " │ ",
    ));
    out.push('\n');
    out.push_str(&join_row(
        &columns
            .iter()
            .map(|column| "─".repeat(column.width))
            .collect::<Vec<_>>(),
        "─┼─",
    ));
    out.push('\n');

    for row in 0..body_rows {
        out.push_str(&join_row(
            &columns
                .iter()
                .map(|column| {
                    column
                        .lines
                        .get(row)
                        .cloned()
                        .unwrap_or_else(|| pad_plain("", column.width))
                })
                .collect::<Vec<_>>(),
            " │ ",
        ));
        out.push('\n');
    }

    out
}

#[derive(Debug, Clone)]
struct PaletteColumn {
    label: &'static str,
    width: usize,
    lines: Vec<String>,
}

fn render_palette_column(
    label: &'static str,
    theme: Theme,
    shared_pairs: &[(&'static str, &'static str)],
) -> PaletteColumn {
    let entries = theme.palette_entries();
    let width = shared_pairs
        .iter()
        .flat_map(|(bg_name, fg_name)| [format!("fg={fg_name}"), format!("bg={bg_name}")])
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0)
        .max(label.chars().count());
    let mut lines = Vec::with_capacity(shared_pairs.len() * 2);

    for (bg_name, fg_name) in shared_pairs {
        let bg = find_palette_entry(&entries, bg_name);
        let fg = find_palette_entry(&entries, fg_name);
        lines.push(colorize_line(
            &format!("fg={fg_name}"),
            fg.color,
            bg.color,
            width,
        ));
        lines.push(colorize_line(
            &format!("bg={bg_name}"),
            fg.color,
            bg.color,
            width,
        ));
    }

    PaletteColumn {
        label,
        width,
        lines,
    }
}

fn find_palette_entry(entries: &[PaletteEntry], name: &str) -> PaletteEntry {
    entries
        .iter()
        .find(|entry| entry.name == name)
        .copied()
        .unwrap_or_else(|| panic!("missing palette entry `{name}`"))
}

fn palette_pairs(theme_entries: &[Vec<PaletteEntry>]) -> Vec<(&'static str, &'static str)> {
    let backgrounds = theme_entries
        .first()
        .expect("at least one theme palette is required");
    let mut foregrounds = backgrounds.to_vec();
    foregrounds.sort_by(|left, right| {
        average_brightness(theme_entries, right.name)
            .partial_cmp(&average_brightness(theme_entries, left.name))
            .unwrap()
            .then_with(|| left.name.cmp(right.name))
    });

    let best_rotation = (0..foregrounds.len())
        .filter_map(|rotation| {
            let mut score = 0.0;
            for (index, background) in backgrounds.iter().enumerate() {
                let foreground = foregrounds[(index + rotation) % foregrounds.len()];
                if foreground.name == background.name {
                    return None;
                }
                score += average_contrast(theme_entries, background.name, foreground.name);
            }
            Some((rotation, score))
        })
        .max_by(|left, right| left.1.partial_cmp(&right.1).unwrap())
        .map(|(rotation, _)| rotation)
        .unwrap_or(0);

    backgrounds
        .iter()
        .enumerate()
        .map(|(index, background)| {
            let foreground = foregrounds[(index + best_rotation) % foregrounds.len()];
            (background.name, foreground.name)
        })
        .collect()
}

fn colorize_line(text: &str, fg: Color, bg: Color, width: usize) -> String {
    let (fg_r, fg_g, fg_b) = rgb_components(fg);
    let (bg_r, bg_g, bg_b) = rgb_components(bg);
    format!(
        "\x1b[38;2;{fg_r};{fg_g};{fg_b}m\x1b[48;2;{bg_r};{bg_g};{bg_b}m{}\x1b[0m",
        pad_plain(text, width)
    )
}

fn join_row(cells: &[String], separator: &str) -> String {
    cells.join(separator)
}

fn pad_plain(text: &str, width: usize) -> String {
    format!("{text:<width$}")
}

fn contrast_score(left: Color, right: Color) -> f32 {
    (brightness(left) - brightness(right)).abs()
}

fn average_contrast(
    theme_entries: &[Vec<PaletteEntry>],
    background_name: &str,
    foreground_name: &str,
) -> f32 {
    let total: f32 = theme_entries
        .iter()
        .map(|entries| {
            let background = find_palette_entry(entries, background_name);
            let foreground = find_palette_entry(entries, foreground_name);
            contrast_score(background.color, foreground.color)
        })
        .sum();
    total / theme_entries.len() as f32
}

fn average_brightness(theme_entries: &[Vec<PaletteEntry>], name: &str) -> f32 {
    let total: f32 = theme_entries
        .iter()
        .map(|entries| brightness(find_palette_entry(entries, name).color))
        .sum();
    total / theme_entries.len() as f32
}

fn brightness(color: Color) -> f32 {
    let (r, g, b) = rgb_components(color);
    (0.2126 * r as f32) + (0.7152 * g as f32) + (0.0722 * b as f32)
}

fn rgb_components(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::Red => (128, 0, 0),
        Color::Green => (0, 128, 0),
        Color::Yellow => (128, 128, 0),
        Color::Blue => (0, 0, 128),
        Color::Magenta => (128, 0, 128),
        Color::Cyan => (0, 128, 128),
        Color::Gray => (192, 192, 192),
        Color::DarkGray => (128, 128, 128),
        Color::LightRed => (255, 0, 0),
        Color::LightGreen => (0, 255, 0),
        Color::LightYellow => (255, 255, 0),
        Color::LightBlue => (0, 0, 255),
        Color::LightMagenta => (255, 0, 255),
        Color::LightCyan => (0, 255, 255),
        Color::White => (255, 255, 255),
        Color::Reset => (0, 0, 0),
        Color::Indexed(index) => (index, index, index),
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};
    use std::collections::HashSet;

    use super::{
        apply_default_filter_to_request, build_request, hidden_from, palette_pairs,
        parse_after_date, parse_before_date, parse_dir_arg, render_palettes, rules_mode,
        validate_terminal_mode, Cli, CliProgress, DisplayOptions,
    };
    use aics::index::{Scope, SearchFilters, SortMode, TrashFilter};
    use aics::parse::Agent;
    use aics::rules::RulesMode;
    use aics::settings::{DefaultFilter, DefaultFilterScope};
    use aics::tui::theme::Theme;

    #[test]
    fn parses_dir_branch_and_builds_request() {
        let cli = Cli::parse_from([
            "aics",
            "--dir",
            "/tmp/demo:main",
            "--agent",
            "claude",
            "--after",
            "2026-03-01",
            "--before",
            "2026-03-31",
            "--min-lines",
            "12",
            "--no-trimmed",
            "--sub-agent",
            "--live",
            "--sort-by",
            "time",
            "deploy",
        ]);

        let request = build_request(&cli).unwrap();
        assert!(matches!(request.scope, Scope::CurrentDir(..)));
        assert_eq!(request.query, "deploy");
        assert_eq!(request.sort, SortMode::Time);
        assert_eq!(request.filters.agent, Some(Agent::Claude));
        assert_eq!(request.filters.session_id, None);
        assert_eq!(request.filters.branch.as_deref(), Some("main"));
        assert_eq!(request.filters.min_lines, Some(12));
        assert!(!request.filters.include_trimmed);
        assert!(request.filters.include_sub_agents);
        assert!(request.filters.live_only);
        assert!(request.filters.after_ts.is_some());
        assert!(request.filters.before_ts.is_some());
    }

    #[test]
    fn parses_session_filter() {
        let cli = Cli::parse_from(["aics", "--session", " session-123 ", "deploy"]);

        let request = build_request(&cli).unwrap();

        assert_eq!(request.query, "deploy");
        assert_eq!(request.filters.session_id.as_deref(), Some("session-123"));
    }

    #[test]
    fn defaults_to_time_sort_without_flag() {
        let cli = Cli::parse_from(["aics", "deploy"]);

        let request = build_request(&cli).unwrap();

        assert_eq!(request.query, "deploy");
        assert_eq!(request.sort, SortMode::Time);
    }

    #[test]
    fn parses_relevance_sort_from_sort_by_flag() {
        let cli = Cli::parse_from(["aics", "--sort-by", "relevance", "deploy"]);

        let request = build_request(&cli).unwrap();

        assert_eq!(request.query, "deploy");
        assert_eq!(request.sort, SortMode::Relevance);
    }

    #[test]
    fn saved_default_filter_applies_to_interactive_request() {
        let cli = Cli::parse_from(["aics", "deploy"]);
        let mut request = build_request(&cli).unwrap();
        let default_filter = DefaultFilter {
            scope: DefaultFilterScope::Global,
            sort: SortMode::Relevance,
            filters: SearchFilters {
                agent: Some(Agent::Codex),
                branch: Some("main".to_owned()),
                include_trimmed: false,
                include_sub_agents: true,
                trashed: TrashFilter::Both,
                ..SearchFilters::default()
            },
        };

        apply_default_filter_to_request(&mut request, &cli, Some(&default_filter));

        assert!(matches!(request.scope, Scope::Global));
        assert_eq!(request.sort, SortMode::Relevance);
        assert_eq!(request.filters.agent, Some(Agent::Codex));
        assert_eq!(request.filters.branch.as_deref(), Some("main"));
        assert!(!request.filters.include_trimmed);
        assert!(request.filters.include_sub_agents);
        assert_eq!(request.filters.trashed, TrashFilter::Both);
    }

    #[test]
    fn no_global_overrides_saved_global_startup_scope() {
        let cli = Cli::parse_from(["aics", "--no-global", "deploy"]);
        let mut request = build_request(&cli).unwrap();
        let default_filter = DefaultFilter {
            scope: DefaultFilterScope::Global,
            sort: SortMode::Time,
            filters: SearchFilters::default(),
        };

        apply_default_filter_to_request(&mut request, &cli, Some(&default_filter));

        assert!(matches!(request.scope, Scope::CurrentDir(..)));
    }

    #[test]
    fn global_and_no_global_conflict() {
        let error = Cli::try_parse_from(["aics", "--global", "--no-global"]).unwrap_err();

        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn cli_filter_flags_override_saved_default_filter() {
        let cli = Cli::parse_from([
            "aics",
            "--dir",
            "/tmp/demo:feature",
            "--agent",
            "claude",
            "--sort-by",
            "time",
            "--sub-agent",
            "deploy",
        ]);
        let mut request = build_request(&cli).unwrap();
        let default_filter = DefaultFilter {
            scope: DefaultFilterScope::Global,
            sort: SortMode::Relevance,
            filters: SearchFilters {
                agent: Some(Agent::Codex),
                branch: Some("main".to_owned()),
                include_sub_agents: false,
                ..SearchFilters::default()
            },
        };

        apply_default_filter_to_request(&mut request, &cli, Some(&default_filter));

        assert!(matches!(request.scope, Scope::CurrentDir(..)));
        assert_eq!(request.sort, SortMode::Time);
        assert_eq!(request.filters.agent, Some(Agent::Claude));
        assert_eq!(request.filters.branch.as_deref(), Some("feature"));
        assert!(request.filters.include_sub_agents);
    }

    #[test]
    fn parses_dates_from_day_and_rfc3339_inputs() {
        let day_start = parse_after_date("2026-03-01").unwrap();
        let day_end = parse_before_date("2026-03-01").unwrap();
        let exact = parse_after_date("2026-03-01T12:34:56Z").unwrap();

        assert!(day_end > day_start);
        assert_eq!(exact, 1_772_368_496);
    }

    #[test]
    fn splits_branch_from_dir_argument() {
        let (path, branch) = parse_dir_arg("/tmp/project:feature-x");
        assert_eq!(path.to_string_lossy(), "/tmp/project");
        assert_eq!(branch.as_deref(), Some("feature-x"));
    }
    #[test]
    fn defaults_progress_to_stderr() {
        let cli = Cli::parse_from(["aics", "deploy"]);
        assert_eq!(cli.progress, CliProgress::Stderr);
    }

    #[test]
    fn rejects_stdout_progress_in_json_mode() {
        let cli = Cli::parse_from(["aics", "--json", "--progress", "stdout", "deploy"]);
        let error = validate_terminal_mode(&cli).unwrap_err();
        assert!(error
            .to_string()
            .contains("--progress stdout conflicts with --json"));
    }

    #[test]
    fn rejects_short_progress_value_forms() {
        for value in ["err", "out"] {
            let error = Cli::try_parse_from(["aics", "--progress", value]).unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
        }
    }

    #[test]
    fn parses_rules_mode_flags() {
        let preview = Cli::parse_from(["aics", "--preview-rules"]);
        assert_eq!(rules_mode(&preview), Some(RulesMode::Preview));

        let apply = Cli::parse_from(["aics", "--apply-rules", "--rules", "rules.js"]);
        assert_eq!(rules_mode(&apply), Some(RulesMode::Apply));
        assert_eq!(
            apply.rules.as_deref(),
            Some(std::path::Path::new("rules.js"))
        );

        let benchmark = Cli::parse_from(["aics", "--benchmark-rules", "--rules", "rules.js"]);
        assert_eq!(rules_mode(&benchmark), Some(RulesMode::Preview));
        assert!(benchmark.benchmark_rules);
    }

    #[test]
    fn parses_write_rules_dts_flag() {
        let cli = Cli::parse_from(["aics", "--write-rules-dts"]);
        assert!(cli.write_rules_dts);
    }

    #[test]
    fn classifies_interactive_and_command_logging_modes() {
        for args in [
            vec!["aics"],
            vec!["aics", "deploy"],
            vec!["aics", "--no-apply-rules"],
            vec!["aics", "--preview-rules"],
        ] {
            assert!(Cli::parse_from(args).will_enter_tui());
        }

        for args in [
            vec!["aics", "--json"],
            vec!["aics", "--export", "./out"],
            vec!["aics", "--apply-rules"],
            vec!["aics", "--benchmark-rules"],
            vec!["aics", "--write-rules-dts"],
            vec!["aics", "--delete-index"],
            vec!["aics", "--print-palettes"],
        ] {
            assert!(!Cli::parse_from(args).will_enter_tui());
        }
    }

    #[test]
    fn export_conflicts_with_other_non_interactive_modes() {
        for conflicting in ["--json", "--preview-rules", "--apply-rules"] {
            let error =
                Cli::try_parse_from(["aics", "--export", "./out", conflicting]).unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        }
    }

    #[test]
    fn export_reserves_stdout_for_written_paths() {
        let cli = Cli::parse_from([
            "aics",
            "--export",
            "./out",
            "--progress",
            "stdout",
            "deploy",
        ]);

        assert!(cli.stdout_reserved());
        let error = validate_terminal_mode(&cli).unwrap_err();
        assert!(error
            .to_string()
            .contains("--progress stdout conflicts with --json"));
    }

    #[test]
    fn hide_flag_is_repeatable_and_only_turns_hiding_on() {
        let cli = Cli::parse_from([
            "aics",
            "--hide",
            "tool-results",
            "--hide",
            "agent-replies",
            "deploy",
        ]);

        let options = hidden_from(DisplayOptions::SHOW_ALL, &cli.hide);

        assert!(options.hide_tool_results);
        assert!(options.hide_agent_replies);
        assert!(!options.hide_tool_calls);
        assert!(!options.hide_user_messages);
        assert!(!options.hide_project_docs_autodump);
    }

    #[test]
    fn hide_flag_adds_to_saved_display_options_rather_than_replacing_them() {
        let cli = Cli::parse_from(["aics", "--hide", "tool-calls", "deploy"]);
        let saved = DisplayOptions {
            hide_tool_results: true,
            ..DisplayOptions::SHOW_ALL
        };

        let options = hidden_from(saved, &cli.hide);

        assert!(options.hide_tool_calls, "the flag applies");
        assert!(options.hide_tool_results, "the saved option survives");
    }

    #[test]
    fn omitting_hide_leaves_an_export_fully_visible() {
        let cli = Cli::parse_from(["aics", "--export", "./out", "deploy"]);

        assert_eq!(
            hidden_from(DisplayOptions::SHOW_ALL, &cli.hide),
            DisplayOptions::SHOW_ALL
        );
    }

    #[test]
    fn hide_accepts_every_display_toggle_the_filter_modal_offers() {
        let cli = Cli::parse_from([
            "aics",
            "--hide",
            "project-docs-autodump",
            "--hide",
            "skill-text-injection",
            "--hide",
            "tool-calls",
            "--hide",
            "tool-results",
            "--hide",
            "agent-replies",
            "--hide",
            "user-messages",
        ]);

        let options = hidden_from(DisplayOptions::SHOW_ALL, &cli.hide);

        assert!(options.hide_project_docs_autodump);
        assert!(options.hide_skill_text_injection);
        assert!(options.hide_tool_calls);
        assert!(options.hide_tool_results);
        assert!(options.hide_agent_replies);
        assert!(options.hide_user_messages);
    }

    #[test]
    fn hide_rejects_unknown_items() {
        let error = Cli::try_parse_from(["aics", "--hide", "everything"]).unwrap_err();

        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
    }

    #[test]
    fn export_composes_with_the_ordinary_search_filters() {
        let cli = Cli::parse_from([
            "aics",
            "--global",
            "--export",
            "./out",
            "--agent",
            "claude",
            "--sub-agent",
            "tmux_tui_harness",
        ]);

        let request = build_request(&cli).unwrap();

        assert_eq!(cli.export.as_deref(), Some(std::path::Path::new("./out")));
        assert!(matches!(request.scope, Scope::Global));
        assert_eq!(request.query, "tmux_tui_harness");
        assert_eq!(request.filters.agent, Some(Agent::Claude));
        assert!(request.filters.include_sub_agents);
    }

    #[test]
    fn accepts_rules_path_for_ordinary_startup() {
        let cli = Cli::parse_from(["aics", "--rules", "rules.js", "--json"]);
        assert!(rules_mode(&cli).is_none());
        assert_eq!(cli.rules.as_deref(), Some(std::path::Path::new("rules.js")));
        validate_terminal_mode(&cli).unwrap();
    }

    #[test]
    fn no_apply_rules_conflicts_with_explicit_rules_options() {
        for conflicting in [
            "--preview-rules",
            "--apply-rules",
            "--benchmark-rules",
            "--rules",
        ] {
            let mut args = vec!["aics", "--no-apply-rules", conflicting];
            if conflicting == "--rules" {
                args.push("rules.js");
            }
            let error = Cli::try_parse_from(args).unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        }
    }

    #[test]
    fn rejects_query_in_rules_mode() {
        let cli = Cli::parse_from(["aics", "--preview-rules", "deploy"]);
        let error = validate_terminal_mode(&cli).unwrap_err();
        assert!(error
            .to_string()
            .contains("rules mode does not support a search query yet"));
    }

    #[test]
    fn rejects_live_filter_in_rules_mode() {
        let cli = Cli::parse_from(["aics", "--preview-rules", "--live"]);
        let error = validate_terminal_mode(&cli).unwrap_err();
        assert!(error
            .to_string()
            .contains("rules mode does not support --live yet"));
    }

    #[test]
    fn parses_delete_index_flag() {
        let cli = Cli::parse_from(["aics", "--delete-index"]);
        assert!(cli.delete_index);
        assert!(!cli.rebuild_index);
    }

    #[test]
    fn help_text_includes_descriptions_and_examples() {
        let mut command = Cli::command();
        let help = command.render_help().to_string();

        assert!(help.contains("Search across all indexed sessions"));
        assert!(help.contains("Optional search query to run immediately"));
        assert!(help.contains("--claude-home <PATH>"));
        assert!(help.contains("--codex-home <PATH>"));
        assert!(help.contains("--no-apply-rules"));
        assert!(help.contains("--export <DIR>"));
        assert!(!help.contains("--benchmark-rules"));
        assert!(help.contains("Examples:"));
        assert!(help.contains("YYYY-MM-DD or RFC3339"));
    }

    #[test]
    fn renders_palette_headers() {
        let rendered = render_palettes();

        assert!(rendered.contains("lazygit"));
        assert!(rendered.contains("aics"));
        assert!(rendered.contains("sunset"));
        assert!(rendered.contains("late"));
        assert!(rendered.contains("fg="));
        assert!(rendered.contains("bg="));
    }

    #[test]
    fn palette_pairs_use_each_constant_once_for_fg_and_bg() {
        let themes = aics::settings::ThemeName::ALL
            .into_iter()
            .map(|name| Theme::from_name(name).palette_entries().to_vec())
            .collect::<Vec<_>>();
        let entries = Theme::aics().palette_entries();
        let pairs = palette_pairs(&themes);
        let bg_names: Vec<_> = pairs.iter().map(|(bg, _)| *bg).collect();
        let fg_names: Vec<_> = pairs.iter().map(|(_, fg)| *fg).collect();

        assert_eq!(pairs.len(), entries.len());
        assert_eq!(
            bg_names.iter().copied().collect::<HashSet<_>>().len(),
            entries.len()
        );
        assert_eq!(
            fg_names.iter().copied().collect::<HashSet<_>>().len(),
            entries.len()
        );
        assert!(pairs.iter().all(|(bg, fg)| bg != fg));
    }

    #[test]
    fn palette_pairs_keep_background_order_stable_across_themes() {
        let themes = aics::settings::ThemeName::ALL
            .into_iter()
            .map(|name| Theme::from_name(name).palette_entries().to_vec())
            .collect::<Vec<_>>();
        let pairs = palette_pairs(&themes);
        let names: Vec<_> = Theme::aics()
            .palette_entries()
            .into_iter()
            .map(|entry| entry.name)
            .collect();

        assert_eq!(pairs.iter().map(|(bg, _)| *bg).collect::<Vec<_>>(), names);
    }
}
