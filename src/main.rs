use std::env;
use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{bail, Result};
use chrono::{Local, NaiveDate, TimeZone, Utc};
use clap::Parser;
use env_logger::Env;

use aics::index::{IndexManager, Scope, SearchFilters, SearchRequest, SortMode, SyncOutcome};
use aics::parse::Agent;
use aics::settings::Settings;
use aics::tui::run_app;

#[derive(Debug, Parser)]
#[command(name = "aics", about = "Search Claude Code and Codex session history")]
struct Cli {
    #[arg(short = 'g', long = "global")]
    global: bool,
    #[arg(long = "dir", value_name = "PATH[:BRANCH]")]
    dir: Option<String>,
    #[arg(long = "branch")]
    branch: Option<String>,
    #[arg(short = 'n', long = "num-results", default_value_t = 100)]
    num_results: usize,
    #[arg(long = "agent", value_parser = ["claude", "codex"])]
    agent: Option<String>,
    #[arg(long = "after")]
    after: Option<String>,
    #[arg(long = "before")]
    before: Option<String>,
    #[arg(long = "min-lines")]
    min_lines: Option<usize>,
    #[arg(long = "no-original")]
    no_original: bool,
    #[arg(long = "no-trimmed")]
    no_trimmed: bool,
    #[arg(long = "no-rollover")]
    no_rollover: bool,
    #[arg(long = "sub-agent")]
    sub_agent: bool,
    #[arg(long = "live")]
    live: bool,
    #[arg(long = "json")]
    json: bool,
    #[arg(long = "by-time")]
    by_time: bool,
    #[arg(long = "rebuild-index")]
    rebuild_index: bool,
    #[arg()]
    query: Option<String>,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("warn")).init();
    let cli = Cli::parse();
    let settings = Settings::load().unwrap_or_default();
    let manager = IndexManager::new()?;
    match manager.sync_best_effort(cli.rebuild_index)? {
        SyncOutcome::Completed(_) | SyncOutcome::Busy => {}
    }
    let request = build_request(&cli)?;
    let search_engine = manager.open_search_engine()?;

    if cli.json {
        for hit in search_engine.search(&request)? {
            println!("{}", serde_json::to_string(&hit)?);
        }
        return Ok(());
    }

    if !std::io::stdout().is_terminal() {
        bail!("stdout is not a terminal; use --json for non-interactive output");
    }

    run_app(manager, search_engine, request, settings)
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
        sort: if cli.by_time {
            SortMode::Time
        } else {
            SortMode::Relevance
        },
        filters: SearchFilters {
            agent: cli.agent.as_deref().and_then(parse_agent_arg),
            branch,
            after_ts: cli
                .after
                .as_deref()
                .map(parse_after_date)
                .transpose()?,
            before_ts: cli
                .before
                .as_deref()
                .map(parse_before_date)
                .transpose()?,
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
        return Ok((Scope::CurrentDir(path), branch));
    }

    Ok((Scope::CurrentDir(env::current_dir()?), None))
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
    use clap::Parser;

    use super::{build_request, parse_after_date, parse_before_date, parse_dir_arg, Cli};
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
            "--by-time",
            "deploy",
        ]);

        let request = build_request(&cli).unwrap();
        assert!(matches!(request.scope, Scope::CurrentDir(_)));
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
}
