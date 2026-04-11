use std::env;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Result};
use chrono::{Local, NaiveDate, TimeZone, Utc};
use clap::{Parser, ValueEnum};
use env_logger::Env;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

use aics::index::{
    IndexManager, Scope, SearchFilters, SearchRequest, SortMode, SyncOutcome, SyncProgress,
};
use aics::live::LiveSessionTracker;
use aics::parse::Agent;
use aics::scan::ResolvedPaths;
use aics::settings::Settings;
use aics::tui::run_app;

#[derive(Debug, Parser)]
#[command(
    name = "aics",
    version,
    about = "Search local Claude Code and Codex CLI session history",
    after_help = "Examples:\n  aics deploy\n      Search sessions for the current directory and open the TUI.\n\n  aics -g --agent claude --after 2026-03-01 deploy\n      Search all Claude sessions after 2026-03-01.\n\n  aics --json -g --sort-by relevance \"vector db\"\n      Print matching sessions as JSONL instead of launching the TUI.\n\nDate filters:\n  --after and --before accept YYYY-MM-DD or RFC3339 timestamps.\n\nScope:\n  By default, searches are scoped to the current directory.\n  Use --global to search everything, or --dir PATH[:BRANCH] to target a project."
)]
struct Cli {
    #[arg(
        short = 'g',
        long = "global",
        help = "Search across all indexed sessions instead of scoping to the current directory"
    )]
    global: bool,
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
        long = "json",
        help = "Print JSONL hits to stdout instead of launching the interactive TUI"
    )]
    json: bool,
    #[arg(
        long = "sort-by",
        value_enum,
        default_value_t = CliSort::Time,
        help = "Order results by time or text relevance"
    )]
    sort_by: CliSort,
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
    #[arg(long = "progress", value_enum, default_value_t = CliProgress::Err)]
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
    Err,
    Out,
}

impl From<CliSort> for SortMode {
    fn from(value: CliSort) -> Self {
        match value {
            CliSort::Time => SortMode::Time,
            CliSort::Relevance => SortMode::Relevance,
        }
    }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("warn")).init();
    let cli = Cli::parse();
    let resolved_paths =
        ResolvedPaths::discover(cli.claude_home.as_deref(), cli.codex_home.as_deref())?;
    let manager = IndexManager::with_paths(aics::index::IndexPaths::discover_for_roots(
        &resolved_paths.roots,
    )?);
    if cli.delete_index {
        manager.delete_index()?;
        return Ok(());
    }
    validate_terminal_mode(&cli)?;
    let settings = Settings::load().unwrap_or_default();
    manager.write_profile_metadata(&resolved_paths)?;
    let sync_outcome = if let Some(draw_target) = progress_draw_target(cli.progress, cli.json) {
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
    let search_engine = manager.open_search_engine_with_live_sessions(
        LiveSessionTracker::from_claude_sessions_dir(resolved_paths.claude_sessions.clone()),
    )?;

    if cli.json {
        for hit in search_engine.search(&request)? {
            println!("{}", serde_json::to_string(&hit)?);
        }
        return Ok(());
    }

    run_app(
        manager,
        search_engine,
        request,
        settings,
        resolved_paths.homes,
    )
}

fn validate_terminal_mode(cli: &Cli) -> Result<()> {
    if cli.json && cli.progress == CliProgress::Out {
        bail!("--progress out conflicts with --json because stdout is reserved for JSON output");
    }

    if !cli.delete_index && !cli.json && !std::io::stdout().is_terminal() {
        bail!("stdout is not a terminal; use --json for non-interactive output");
    }

    Ok(())
}

fn progress_draw_target(mode: CliProgress, json_mode: bool) -> Option<ProgressDrawTarget> {
    match mode {
        CliProgress::None => None,
        CliProgress::Err => std::io::stderr()
            .is_terminal()
            .then(ProgressDrawTarget::stderr),
        CliProgress::Out if json_mode => None,
        CliProgress::Out => std::io::stdout()
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
        bar.set_style(bar_style());
        bar.enable_steady_tick(Duration::from_millis(100));
        Self { bar }
    }

    fn update(&mut self, event: SyncProgress) {
        match event {
            SyncProgress::Discovering { .. } => {}
            SyncProgress::IndexingStarted { total } => {
                self.bar.set_length(total as u64);
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

fn bar_style() -> ProgressStyle {
    ProgressStyle::with_template("Indexing {bar:40}")
        .expect("valid progress template")
        .progress_chars("=> ")
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
        sort: cli.sort_by.into(),
        filters: SearchFilters {
            agent: cli.agent.as_deref().and_then(parse_agent_arg),
            branch,
            after_ts: cli.after.as_deref().map(parse_after_date).transpose()?,
            before_ts: cli.before.as_deref().map(parse_before_date).transpose()?,
            min_lines: cli.min_lines,
            include_original: !cli.no_original,
            include_trimmed: !cli.no_trimmed,
            include_continued: !cli.no_rollover,
            include_sub_agents: cli.sub_agent,
            live_only: cli.live,
        },
    })
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

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::{
        build_request, parse_after_date, parse_before_date, parse_dir_arg, validate_terminal_mode,
        Cli, CliProgress,
    };
    use aics::index::{Scope, SortMode};
    use aics::parse::Agent;

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
        assert_eq!(request.filters.branch.as_deref(), Some("main"));
        assert_eq!(request.filters.min_lines, Some(12));
        assert!(!request.filters.include_trimmed);
        assert!(request.filters.include_sub_agents);
        assert!(request.filters.live_only);
        assert!(request.filters.after_ts.is_some());
        assert!(request.filters.before_ts.is_some());
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
        assert_eq!(cli.progress, CliProgress::Err);
    }

    #[test]
    fn rejects_stdout_progress_in_json_mode() {
        let cli = Cli::parse_from(["aics", "--json", "--progress", "out", "deploy"]);
        let error = validate_terminal_mode(&cli).unwrap_err();
        assert!(error
            .to_string()
            .contains("--progress out conflicts with --json"));
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
        assert!(help.contains("Examples:"));
        assert!(help.contains("YYYY-MM-DD or RFC3339"));
    }
}
