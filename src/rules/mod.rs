use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Condvar, Mutex};
use std::thread;

use anyhow::{anyhow, bail, Context, Result};
use boa_engine::{
    js_string, Context as BoaContext, JsArgs, JsNativeError, JsResult as BoaJsResult, JsString,
    JsValue, NativeFunction, Source,
};
use log::warn;
use serde::{Deserialize, Serialize};

use crate::index::reader::fallback_snippet;
use crate::index::{Scope, SearchFilters, SearchHit, StoredSession, TrashFilter};
use crate::parse::{
    is_contextual_user_message_content, parse_scanned_session_file, Agent, DerivationType,
    MessageRole, PatchFile, Session, SessionCell,
};
use crate::scan::{scan_session_files, SessionFile, SessionRoots};
use crate::settings::config_dir;
use crate::trash::TrashStore;

mod cache;

use cache::{
    fingerprint_session, CacheLookup, CachedDetermination, ContentFingerprint, RulesCache,
};

const RULE_STACK_LIMIT: usize = 512 * 1024;
const BOA_RULE_LOOP_ITERATION_LIMIT: u64 = 10_000_000;
const BOA_RULE_RECURSION_LIMIT: usize = 512;
const TRUNCATED_SUFFIX: &str = "\n... [truncated for rules]";

thread_local! {
    static BOA_RULE_DETAILS: RefCell<Option<RuleDetails>> = const { RefCell::new(None) };
}

const RULES_HARNESS: &str = r#"
const __aicsRules = [];
const __aicsRuleNames = new Set();

globalThis.rule = function(name, configOrCallback, callback) {
  if (typeof name !== "string" || name.trim() === "") {
    throw new TypeError("rule name must be a non-empty string");
  }
  const hasConfig = arguments.length >= 3;
  const config = hasConfig ? configOrCallback : {};
  callback = hasConfig ? callback : configOrCallback;
  if (config === null || typeof config !== "object" || Array.isArray(config)) {
    throw new TypeError(`rule ${name} config must be an object`);
  }
  if (config.applyAtStartup !== undefined && typeof config.applyAtStartup !== "boolean") {
    throw new TypeError(`rule ${name} config.applyAtStartup must be a boolean`);
  }
  if (typeof callback !== "function") {
    throw new TypeError(`rule ${name} callback must be a function`);
  }
  if (__aicsRuleNames.has(name)) {
    throw new Error(`duplicate rule name: ${name}`);
  }
  __aicsRuleNames.add(name);
  __aicsRules.push({ name, config, callback });
};

globalThis.nothing = function(reason) {
  return {
    action: "nothing",
    reason: reason == null ? null : String(reason),
  };
};

globalThis.trash = function(reason) {
  return {
    action: "trash",
    reason: reason == null ? null : String(reason),
  };
};

globalThis.untrash = function(reason) {
  return {
    action: "untrash",
    reason: reason == null ? null : String(reason),
  };
};

globalThis.__aicsRuleNames = function() {
  return JSON.stringify(__aicsRules.map((entry) => entry.name));
};

function __aicsNormalizeLimit(limit) {
  if (limit == null) {
    return 1000000000;
  }
  const n = Number(limit);
  if (!Number.isFinite(n) || n < 0) {
    throw new TypeError("rule text limit must be a non-negative finite number");
  }
  return Math.floor(n);
}

globalThis.__aicsRunRules = function(contextJson, applyAtStartupOnly) {
  const context = JSON.parse(contextJson);

  for (const kind of ["user", "contextualUser", "agent", "system", "toolResults"]) {
    for (const turn of context.turns[kind] || []) {
      const fetchKind =
        kind === "toolResults" ? "tool_result" :
        kind === "contextualUser" ? "contextual_user" :
        kind;
      turn.text = function(limit) {
        return globalThis.__aicsFetchText(fetchKind, Number(turn.index), "text", __aicsNormalizeLimit(limit));
      };
    }
  }
  for (const turn of context.turns.exec || []) {
    turn.stdout = function(limit) {
      return globalThis.__aicsFetchText("exec", Number(turn.index), "stdout", __aicsNormalizeLimit(limit));
    };
    turn.stderr = function(limit) {
      return globalThis.__aicsFetchText("exec", Number(turn.index), "stderr", __aicsNormalizeLimit(limit));
    };
  }
  for (const turn of context.turns.patches || []) {
    for (const [fileIndex, file] of (turn.files || []).entries()) {
      file.content = function(limit) {
        return globalThis.__aicsFetchText("patch", Number(turn.index), String(fileIndex), __aicsNormalizeLimit(limit));
      };
    }
  }

  const outcomes = [];
  for (const entry of __aicsRules) {
    if (applyAtStartupOnly && entry.config.applyAtStartup !== true) {
      continue;
    }
    try {
      const raw = entry.callback(context);
      const actions = Array.isArray(raw) ? raw : [raw];
      for (const action of actions) {
        if (action == null) {
          continue;
        }
        const actionName = String(action.action);
        const reason = action.reason == null ? null : String(action.reason);
        if (actionName === "nothing" && (reason == null || reason.trim() === "")) {
          continue;
        }
        outcomes.push({
          rule: entry.name,
          action: actionName,
          reason,
        });
      }
    } catch (error) {
      outcomes.push({
        rule: entry.name,
        error: String((error && (error.stack || error.message)) || error),
      });
    }
  }
  return JSON.stringify(outcomes);
};
"#;

const RULES_DTS: &str = include_str!("rules.d.ts");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulesMode {
    Preview,
    Apply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleSelection {
    All,
    ApplyAtStartup,
}

#[derive(Debug, Clone)]
pub struct RulesOptions {
    pub rules_path: PathBuf,
    pub cache_path: Option<PathBuf>,
    pub mode: RulesMode,
    pub selection: RuleSelection,
    pub json: bool,
    pub scope: Scope,
    pub filters: SearchFilters,
    pub supersession: BTreeMap<PathBuf, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulesProgress {
    ProcessingStarted {
        total: usize,
    },
    /// `processed` is the number of completed sessions, not a traversal position.
    ProcessingProgress {
        processed: usize,
        total: usize,
    },
}

#[derive(Debug, Clone, Default)]
pub struct RulesReport {
    pub preview_matches: Vec<RulePreviewMatch>,
    pub proposals: Vec<RuleProposal>,
    pub applied: Vec<AppliedRuleAction>,
    pub skipped: Vec<SkippedRuleAction>,
    pub errors: Vec<RuleEvaluationError>,
}

#[derive(Debug, Clone)]
pub struct RulePreviewMatch {
    pub proposals: Vec<RuleProposal>,
    pub decisions: Vec<RuleDecision>,
    pub hit: SearchHit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleDecision {
    pub rule: String,
    pub action: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuleProposal {
    pub rule: String,
    pub session_id: String,
    pub path: PathBuf,
    pub agent: Agent,
    #[serde(flatten)]
    pub action: RuleAction,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RuleAction {
    Trash {
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Untrash {
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppliedRuleAction {
    pub rule: String,
    pub session_id: String,
    pub path: PathBuf,
    pub agent: Agent,
    #[serde(flatten)]
    pub action: RuleAction,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SkippedRuleAction {
    pub rule: String,
    pub session_id: String,
    pub path: PathBuf,
    pub agent: Agent,
    #[serde(flatten)]
    pub action: RuleAction,
    pub skip_reason: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuleEvaluationError {
    pub rule: Option<String>,
    pub path: PathBuf,
    pub error: String,
}

pub fn default_rules_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("rules.js"))
}

pub fn default_rules_dts_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("rules.d.ts"))
}

pub fn write_default_rules_dts() -> Result<PathBuf> {
    let path = default_rules_dts_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(&path, RULES_DTS).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

/// Render the data exposed to a `rules.js` callback as standalone JSON.
///
/// The callback API uses functions for potentially large transcript fields so
/// rules can fetch them lazily. JSON cannot represent functions, so this export
/// keeps the same property names and materializes their complete values as
/// strings instead.
pub fn session_to_rule_context_json(
    session: &Session,
    path: &Path,
    trashed: bool,
    superseded_by: Option<&str>,
) -> Result<String> {
    let input = RuleInput::from_source(session, path, trashed, superseded_by);
    let details = RuleDetails::from_session(session);
    let mut context =
        serde_json::to_value(input).context("failed to serialize rules session data")?;
    materialize_rule_context(&mut context, &details)?;
    let mut rendered = serde_json::to_string_pretty(&context)
        .context("failed to render rules session data as JSON")?;
    rendered.push('\n');
    Ok(rendered)
}

pub fn run_rules(roots: &SessionRoots, options: &RulesOptions) -> Result<RulesReport> {
    run_rules_with_progress(roots, options, |_| {})
}

pub fn run_rules_with_progress<F>(
    roots: &SessionRoots,
    options: &RulesOptions,
    mut on_progress: F,
) -> Result<RulesReport>
where
    F: FnMut(RulesProgress),
{
    if !options.rules_path.exists() {
        bail!(
            "rules file not found: {}",
            display_rules_path(&options.rules_path)
        );
    }

    let mut cache = options.cache_path.clone().and_then(|path| {
        match RulesCache::open(path, &options.rules_path) {
            Ok(cache) => Some(cache),
            Err(error) => {
                warn!("rules cache disabled for this run: {error:#}");
                None
            }
        }
    });
    let rules_must_be_validated = cache.as_ref().is_none_or(|cache| !cache.was_reused());
    let files = scan_session_files(roots)?;
    let total = files.len();
    on_progress(RulesProgress::ProcessingStarted { total });
    let mut completed = 0;
    let mut mark_completed = || {
        completed += 1;
        on_progress(RulesProgress::ProcessingProgress {
            processed: completed,
            total,
        });
    };
    let mut results = (0..total).map(|_| None).collect::<Vec<_>>();
    let mut jobs = Vec::new();

    for (file_index, file) in files.iter().enumerate() {
        if !file_matches_filters(file, &options.filters) {
            mark_completed();
            continue;
        }
        let superseded_by = options.supersession.get(&file.path).map(String::as_str);
        let lookup = cache
            .as_ref()
            .map(|cache| cache.lookup(file, superseded_by));
        match lookup {
            Some(CacheLookup::Hit {
                determination,
                fingerprint,
            }) if cached_determination_needs_evaluation(
                &determination,
                file,
                &options.scope,
                &options.filters,
            ) =>
            {
                jobs.push(RuleJob {
                    file_index,
                    superseded_by: superseded_by.map(str::to_owned),
                    kind: RuleJobKind::Evaluate {
                        fingerprint: Some(fingerprint),
                    },
                })
            }
            Some(CacheLookup::Hit { determination, .. }) => {
                results[file_index] = Some(CompletedRuleFile {
                    determination,
                    cache_fingerprint: None,
                });
                mark_completed();
            }
            Some(CacheLookup::Validate {
                determination,
                fingerprint,
            }) => jobs.push(RuleJob {
                file_index,
                superseded_by: superseded_by.map(str::to_owned),
                kind: RuleJobKind::Validate {
                    evaluate_if_unchanged: cached_determination_needs_evaluation(
                        &determination,
                        file,
                        &options.scope,
                        &options.filters,
                    ),
                    determination,
                    fingerprint,
                },
            }),
            Some(CacheLookup::Miss(fingerprint)) => jobs.push(RuleJob {
                file_index,
                superseded_by: superseded_by.map(str::to_owned),
                kind: RuleJobKind::Evaluate { fingerprint },
            }),
            None => jobs.push(RuleJob {
                file_index,
                superseded_by: superseded_by.map(str::to_owned),
                kind: RuleJobKind::Evaluate { fingerprint: None },
            }),
        }
    }

    if jobs.is_empty() {
        if rules_must_be_validated {
            JsRuleEngine::load(&options.rules_path)?;
        }
    } else {
        let rules_source = fs::read_to_string(&options.rules_path)
            .with_context(|| format!("failed to read {}", options.rules_path.display()))?;
        let worker_count = rules_worker_count(jobs.len());
        let cache_enabled = cache.is_some();
        run_parallel_jobs(
            &jobs,
            worker_count,
            || JsRuleEngine::from_source(&options.rules_path, &rules_source),
            |engine, job| {
                process_rule_job(
                    engine,
                    &files[job.file_index],
                    job,
                    options.selection,
                    &options.scope,
                    &options.filters,
                    cache_enabled,
                )
            },
            |job_index, result| {
                results[jobs[job_index].file_index] = Some(result);
                mark_completed();
            },
        )?;
    }

    let mut report = RulesReport::default();
    for (file_index, result) in results.into_iter().enumerate() {
        let Some(result) = result else {
            continue;
        };
        let file = &files[file_index];
        collect_determination(
            &mut report,
            file,
            &result.determination,
            &options.scope,
            &options.filters,
        );
        if let (Some(cache), Some(fingerprint)) = (cache.as_mut(), result.cache_fingerprint) {
            let superseded_by = options.supersession.get(&file.path).map(String::as_str);
            cache.insert(&file.path, fingerprint, superseded_by, result.determination);
        }
    }

    if let Some(cache) = cache.as_mut() {
        cache.retain_files(files.iter().map(|file| file.path.as_path()));
        if let Err(error) = cache.save() {
            warn!("failed to save rules cache: {error:#}");
        }
    }

    report.preview_matches = dedupe_preview_matches(std::mem::take(&mut report.preview_matches));
    report.proposals = report
        .preview_matches
        .iter()
        .flat_map(|matched| matched.proposals.iter().cloned())
        .collect();

    if matches!(options.mode, RulesMode::Apply) {
        apply_proposals(roots, &mut report);
    }

    Ok(report)
}

#[derive(Debug)]
struct RuleJob {
    file_index: usize,
    superseded_by: Option<String>,
    kind: RuleJobKind,
}

#[derive(Debug)]
enum RuleJobKind {
    Evaluate {
        fingerprint: Option<ContentFingerprint>,
    },
    Validate {
        determination: CachedDetermination,
        fingerprint: ContentFingerprint,
        evaluate_if_unchanged: bool,
    },
}

#[derive(Debug)]
struct CompletedRuleFile {
    determination: CachedDetermination,
    cache_fingerprint: Option<ContentFingerprint>,
}

fn cached_determination_needs_evaluation(
    determination: &CachedDetermination,
    file: &SessionFile,
    scope: &Scope,
    filters: &SearchFilters,
) -> bool {
    matches!(
        determination,
        CachedDetermination::Unevaluated { session }
            if session_matches_scope(scope, session)
                && session_matches_filters(session, file, filters)
    )
}

fn rules_worker_count(job_count: usize) -> usize {
    thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(job_count)
        .max(1)
}

fn process_rule_job(
    engine: &JsRuleEngine,
    file: &SessionFile,
    job: &RuleJob,
    selection: RuleSelection,
    scope: &Scope,
    filters: &SearchFilters,
    cache_enabled: bool,
) -> CompletedRuleFile {
    match &job.kind {
        RuleJobKind::Evaluate { fingerprint } => evaluate_rule_file(
            engine,
            file,
            job.superseded_by.as_deref(),
            selection,
            scope,
            filters,
            cache_enabled,
            *fingerprint,
        ),
        RuleJobKind::Validate {
            determination,
            fingerprint,
            evaluate_if_unchanged,
        } => match fingerprint_session(file) {
            Ok(current) if fingerprint.has_same_content(current) && !evaluate_if_unchanged => {
                CompletedRuleFile {
                    determination: determination.clone(),
                    cache_fingerprint: Some(current),
                }
            }
            Ok(current) => evaluate_rule_file(
                engine,
                file,
                job.superseded_by.as_deref(),
                selection,
                scope,
                filters,
                cache_enabled,
                Some(current),
            ),
            Err(error) => {
                warn!(
                    "could not validate {} against the rules cache: {error:#}",
                    file.path.display()
                );
                evaluate_rule_file(
                    engine,
                    file,
                    job.superseded_by.as_deref(),
                    selection,
                    scope,
                    filters,
                    cache_enabled,
                    None,
                )
            }
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_rule_file(
    engine: &JsRuleEngine,
    file: &SessionFile,
    superseded_by: Option<&str>,
    selection: RuleSelection,
    scope: &Scope,
    filters: &SearchFilters,
    cache_enabled: bool,
    mut cache_fingerprint: Option<ContentFingerprint>,
) -> CompletedRuleFile {
    let determination = match parse_scanned_session_file(file) {
        Ok(Some(session)) => {
            let stored = stored_rule_session(&session, file, superseded_by);
            if !session_matches_scope(scope, &stored)
                || !session_matches_filters(&stored, file, filters)
            {
                CachedDetermination::Unevaluated {
                    session: Box::new(stored),
                }
            } else {
                let input = RuleInput::from_session(&session, file, superseded_by);
                let details = RuleDetails::from_session(&session);
                match engine.evaluate(&input, details, selection) {
                    Ok(outcomes) if outcomes.is_empty() => CachedDetermination::NoMatch,
                    Ok(outcomes) => CachedDetermination::Evaluated {
                        session: Box::new(stored),
                        outcomes,
                    },
                    Err(error) => CachedDetermination::EvaluationError {
                        session: Box::new(stored),
                        error: format!("{error:#}"),
                    },
                }
            }
        }
        Ok(None) => CachedDetermination::Ignored,
        Err(error) => CachedDetermination::ParseError {
            error: format!("{error:#}"),
        },
    };

    if cache_enabled && cache_fingerprint.is_none() {
        cache_fingerprint = fingerprint_session_for_cache(file);
    }
    CompletedRuleFile {
        determination,
        cache_fingerprint,
    }
}

fn run_parallel_jobs<J, S, R, Init, Work, Consume>(
    jobs: &[J],
    worker_count: usize,
    init: Init,
    work: Work,
    mut consume: Consume,
) -> Result<()>
where
    J: Sync,
    R: Send,
    Init: Fn() -> Result<S> + Sync,
    Work: Fn(&mut S, &J) -> R + Sync,
    Consume: FnMut(usize, R),
{
    debug_assert!(!jobs.is_empty());
    debug_assert!((1..=jobs.len()).contains(&worker_count));

    let next_job = AtomicUsize::new(0);
    let start = (Mutex::new(WorkerStart::Waiting), Condvar::new());

    thread::scope(|scope| -> Result<()> {
        let (init_tx, init_rx) = mpsc::channel::<std::result::Result<(), String>>();
        let (result_tx, result_rx) = mpsc::sync_channel::<(usize, R)>(worker_count);

        for worker_index in 0..worker_count {
            let init_tx = init_tx.clone();
            let result_tx = result_tx.clone();
            let start = &start;
            let next_job = &next_job;
            let init = &init;
            let work = &work;
            if let Err(error) = thread::Builder::new()
                .name(format!("aics-rules-{worker_index}"))
                .spawn_scoped(scope, move || {
                    let mut state = match init() {
                        Ok(state) => {
                            if init_tx.send(Ok(())).is_err() {
                                return;
                            }
                            Some(state)
                        }
                        Err(error) => {
                            let _ = init_tx.send(Err(format!("{error:#}")));
                            None
                        }
                    };

                    if wait_for_worker_start(start) == WorkerStart::Cancel {
                        return;
                    }
                    let Some(state) = state.as_mut() else {
                        return;
                    };

                    loop {
                        let job_index = next_job.fetch_add(1, Ordering::Relaxed);
                        let Some(job) = jobs.get(job_index) else {
                            break;
                        };
                        if result_tx.send((job_index, work(state, job))).is_err() {
                            break;
                        }
                    }
                })
            {
                release_workers(start, WorkerStart::Cancel);
                return Err(error).context("failed to spawn rules worker");
            }
        }
        drop(init_tx);
        drop(result_tx);

        let mut init_error = None;
        for _ in 0..worker_count {
            let status = match init_rx.recv() {
                Ok(status) => status,
                Err(error) => {
                    release_workers(&start, WorkerStart::Cancel);
                    return Err(error).context("rules worker stopped during initialization");
                }
            };
            match status {
                Ok(()) => {}
                Err(error) if init_error.is_none() => init_error = Some(error),
                Err(_) => {}
            }
        }
        release_workers(
            &start,
            if init_error.is_some() {
                WorkerStart::Cancel
            } else {
                WorkerStart::Run
            },
        );
        if let Some(error) = init_error {
            bail!("failed to initialize rules worker: {error}");
        }

        for _ in 0..jobs.len() {
            let (job_index, result) = result_rx
                .recv()
                .context("rules worker stopped before completing its job")?;
            consume(job_index, result);
        }
        Ok(())
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerStart {
    Waiting,
    Run,
    Cancel,
}

fn wait_for_worker_start(start: &(Mutex<WorkerStart>, Condvar)) -> WorkerStart {
    let (command, ready) = start;
    let mut command = command.lock().unwrap_or_else(|error| error.into_inner());
    while *command == WorkerStart::Waiting {
        command = ready
            .wait(command)
            .unwrap_or_else(|error| error.into_inner());
    }
    *command
}

fn release_workers(start: &(Mutex<WorkerStart>, Condvar), command: WorkerStart) {
    let (current, ready) = start;
    *current.lock().unwrap_or_else(|error| error.into_inner()) = command;
    ready.notify_all();
}

fn fingerprint_session_for_cache(file: &SessionFile) -> Option<ContentFingerprint> {
    match fingerprint_session(file) {
        Ok(fingerprint) => Some(fingerprint),
        Err(error) => {
            warn!(
                "could not fingerprint {} for the rules cache: {error:#}",
                file.path.display()
            );
            None
        }
    }
}

pub fn print_report(report: &RulesReport, json: bool, mode: RulesMode) -> Result<()> {
    if json {
        match mode {
            RulesMode::Preview => {
                for proposal in &report.proposals {
                    println!("{}", serde_json::to_string(proposal)?);
                }
            }
            RulesMode::Apply => {
                for applied in &report.applied {
                    println!("{}", serde_json::to_string(applied)?);
                }
                for skipped in &report.skipped {
                    println!("{}", serde_json::to_string(skipped)?);
                }
            }
        }
        for error in &report.errors {
            eprintln!("{}", serde_json::to_string(error)?);
        }
        return Ok(());
    }

    for proposal in &report.proposals {
        println!(
            "{}  {}  {}  {}{}",
            proposal.action.label(),
            proposal.agent,
            file_label(&proposal.path),
            proposal.rule,
            proposal
                .action
                .reason()
                .map(|reason| format!("  {reason}"))
                .unwrap_or_default()
        );
    }

    if matches!(mode, RulesMode::Apply) {
        for skipped in &report.skipped {
            println!(
                "skip   {}  {}  {}  {}",
                skipped.agent,
                file_label(&skipped.path),
                skipped.rule,
                skipped.skip_reason
            );
        }
    }

    for error in &report.errors {
        eprintln!("rule error  {}  {}", file_label(&error.path), error.error);
    }

    println!();
    println!("{} proposed actions", report.proposals.len());
    println!("{} applied", report.applied.len());
    if !report.skipped.is_empty() {
        println!("{} skipped", report.skipped.len());
    }
    if !report.errors.is_empty() {
        println!("{} errors", report.errors.len());
    }
    Ok(())
}

fn display_rules_path(path: &Path) -> String {
    if let Ok(config) = config_dir() {
        let default = config.join("rules.js");
        if path == default {
            return "~/.config/aics/rules.js".to_owned();
        }
    }
    path.display().to_string()
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

struct JsRuleEngine {
    context: RefCell<BoaContext>,
}

impl JsRuleEngine {
    fn load(path: &Path) -> Result<Self> {
        let source = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::from_source(path, &source)
    }

    fn from_source(path: &Path, source: &str) -> Result<Self> {
        let mut context = BoaContext::default();
        context
            .runtime_limits_mut()
            .set_loop_iteration_limit(BOA_RULE_LOOP_ITERATION_LIMIT);
        context
            .runtime_limits_mut()
            .set_stack_size_limit(RULE_STACK_LIMIT);
        context
            .runtime_limits_mut()
            .set_recursion_limit(BOA_RULE_RECURSION_LIMIT);
        context
            .register_global_builtin_callable(
                js_string!("__aicsFetchText"),
                4,
                NativeFunction::from_fn_ptr(boa_fetch_text),
            )
            .map_err(boa_error)
            .context("failed to install Boa rules runtime")?;

        let engine = Self {
            context: RefCell::new(context),
        };
        engine
            .eval_ignore_result(RULES_HARNESS)
            .and_then(|_| engine.eval_ignore_result(source))
            .with_context(|| format!("failed to load rules from {}", path.display()))?;
        engine.rule_names()?;
        Ok(engine)
    }

    fn rule_names(&self) -> Result<Vec<String>> {
        let json = self.eval_string("globalThis.__aicsRuleNames()")?;
        serde_json::from_str(&json).context("rules runtime returned invalid rule-name JSON")
    }

    fn evaluate(
        &self,
        input: &RuleInput,
        details: RuleDetails,
        selection: RuleSelection,
    ) -> Result<Vec<RawRuleOutcome>> {
        let input_json = serde_json::to_string(input).context("failed to serialize rule input")?;
        let input_literal =
            serde_json::to_string(&input_json).context("failed to quote rule input")?;
        let apply_at_startup_only = matches!(selection, RuleSelection::ApplyAtStartup);
        let script = format!("globalThis.__aicsRunRules({input_literal}, {apply_at_startup_only})");
        BOA_RULE_DETAILS.with(|slot| {
            *slot.borrow_mut() = Some(details);
        });
        let output_json = self.eval_string(&script);
        BOA_RULE_DETAILS.with(|slot| {
            *slot.borrow_mut() = None;
        });
        let output_json = output_json?;
        serde_json::from_str(&output_json).context("rules runtime returned invalid action JSON")
    }

    fn eval_ignore_result(&self, source: &str) -> Result<()> {
        self.context
            .borrow_mut()
            .eval(Source::from_bytes(source))
            .map(|_| ())
            .map_err(boa_error)
    }

    fn eval_string(&self, source: &str) -> Result<String> {
        let mut context = self.context.borrow_mut();
        let value = context
            .eval(Source::from_bytes(source))
            .map_err(boa_error)?;
        Ok(value
            .to_string(&mut context)
            .map_err(boa_error)?
            .to_std_string_lossy())
    }
}

fn boa_fetch_text(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut BoaContext,
) -> BoaJsResult<JsValue> {
    let kind = boa_arg_string(args, 0, context)?;
    let index = boa_arg_usize(args, 1, context)?;
    let field = boa_arg_string(args, 2, context)?;
    let limit = boa_arg_usize(args, 3, context)?;
    let text = BOA_RULE_DETAILS.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|details| details.fetch(&kind, index, &field, limit))
            .unwrap_or_default()
    });
    Ok(JsValue::from(JsString::from(text)))
}

fn boa_arg_string(args: &[JsValue], index: usize, context: &mut BoaContext) -> BoaJsResult<String> {
    Ok(args
        .get_or_undefined(index)
        .to_string(context)?
        .to_std_string_lossy())
}

fn boa_arg_usize(args: &[JsValue], index: usize, context: &mut BoaContext) -> BoaJsResult<usize> {
    let value = args.get_or_undefined(index).to_number(context)?;
    if !value.is_finite() || value < 0.0 {
        return Err(JsNativeError::typ()
            .with_message("rule text limit must be a non-negative finite number")
            .into());
    }
    Ok(if value >= usize::MAX as f64 {
        usize::MAX
    } else {
        value.floor() as usize
    })
}

fn boa_error(error: boa_engine::JsError) -> anyhow::Error {
    anyhow!("{error}")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawRuleOutcome {
    rule: String,
    action: Option<String>,
    reason: Option<String>,
    error: Option<String>,
}

fn collect_outcomes(
    report: &mut RulesReport,
    session: &StoredSession,
    file: &SessionFile,
    outcomes: &[RawRuleOutcome],
) {
    let mut proposals = Vec::new();
    let mut decisions = Vec::new();
    let mut seen_actions = BTreeSet::new();

    for outcome in outcomes {
        if let Some(error) = outcome.error.as_ref() {
            report.errors.push(RuleEvaluationError {
                rule: Some(outcome.rule.clone()),
                path: file.path.clone(),
                error: error.clone(),
            });
            continue;
        }

        match outcome.action.as_deref() {
            Some("trash") => {
                let proposal = RuleProposal {
                    rule: outcome.rule.clone(),
                    session_id: session.session_id.clone(),
                    path: file.path.clone(),
                    agent: file.agent,
                    action: RuleAction::Trash {
                        reason: outcome.reason.clone(),
                    },
                };
                if seen_actions.insert(proposal.action.label()) {
                    proposals.push(proposal);
                }
                collect_rule_decision(&mut decisions, outcome, "trash");
            }
            Some("untrash") => {
                let proposal = RuleProposal {
                    rule: outcome.rule.clone(),
                    session_id: session.session_id.clone(),
                    path: file.path.clone(),
                    agent: file.agent,
                    action: RuleAction::Untrash {
                        reason: outcome.reason.clone(),
                    },
                };
                if seen_actions.insert(proposal.action.label()) {
                    proposals.push(proposal);
                }
                collect_rule_decision(&mut decisions, outcome, "untrash");
            }
            Some("nothing") => collect_rule_decision(&mut decisions, outcome, "nothing"),
            Some(action) => report.errors.push(RuleEvaluationError {
                rule: Some(outcome.rule.clone()),
                path: file.path.clone(),
                error: format!("unsupported rule action: {action}"),
            }),
            None => {}
        }
    }

    if !proposals.is_empty() || !decisions.is_empty() {
        report.preview_matches.push(RulePreviewMatch {
            proposals,
            decisions,
            hit: rule_search_hit(session.clone()),
        });
    }
}

fn collect_rule_decision(
    decisions: &mut Vec<RuleDecision>,
    outcome: &RawRuleOutcome,
    action: &str,
) {
    let Some(text) = outcome
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    decisions.push(RuleDecision {
        rule: outcome.rule.clone(),
        action: action.to_owned(),
        text: text.to_owned(),
    });
}

fn stored_rule_session(
    session: &Session,
    file: &SessionFile,
    superseded_by: Option<&str>,
) -> StoredSession {
    let mut stored = StoredSession::from(session);
    stored.trashed = file.trashed;
    stored.original_path = file.original_path.clone();
    stored.superseded_by = superseded_by.map(str::to_owned);
    stored
}

fn rule_search_hit(session: StoredSession) -> SearchHit {
    let snippet_html = fallback_snippet(&session, "");
    SearchHit {
        score: session.modified_ts as f32,
        is_live: false,
        session,
        snippet_html,
    }
}

fn collect_determination(
    report: &mut RulesReport,
    file: &SessionFile,
    determination: &CachedDetermination,
    scope: &Scope,
    filters: &SearchFilters,
) {
    match determination {
        CachedDetermination::Ignored
        | CachedDetermination::NoMatch
        | CachedDetermination::Unevaluated { .. } => {}
        CachedDetermination::ParseError { error } => {
            warn!("failed to parse {} for rules: {error}", file.path.display())
        }
        CachedDetermination::Evaluated { session, outcomes } => {
            if session_matches_scope(scope, session)
                && session_matches_filters(session, file, filters)
            {
                collect_outcomes(report, session, file, outcomes);
            }
        }
        CachedDetermination::EvaluationError { session, error } => {
            if session_matches_scope(scope, session)
                && session_matches_filters(session, file, filters)
            {
                report.errors.push(RuleEvaluationError {
                    rule: None,
                    path: file.path.clone(),
                    error: error.clone(),
                });
            }
        }
    }
}

fn dedupe_preview_matches(matches: Vec<RulePreviewMatch>) -> Vec<RulePreviewMatch> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for matched in matches {
        if seen.insert(matched.hit.session.file_path.clone()) {
            deduped.push(matched);
        }
    }
    deduped
}

fn apply_proposals(roots: &SessionRoots, report: &mut RulesReport) {
    let (applied, skipped) = apply_rule_proposals(roots, &report.proposals);
    report.applied.extend(applied);
    report.skipped.extend(skipped);
}

pub fn apply_rule_proposals(
    roots: &SessionRoots,
    proposals: &[RuleProposal],
) -> (Vec<AppliedRuleAction>, Vec<SkippedRuleAction>) {
    let mut applied = Vec::new();
    let mut skipped = Vec::new();
    let Some(paths) = roots.trash.clone() else {
        for proposal in proposals {
            skipped.push(SkippedRuleAction {
                rule: proposal.rule.clone(),
                session_id: proposal.session_id.clone(),
                path: proposal.path.clone(),
                agent: proposal.agent,
                action: proposal.action.clone(),
                skip_reason: "trash store is unavailable".to_owned(),
            });
        }
        return (applied, skipped);
    };

    let store = TrashStore::new(paths);
    for proposal in proposals {
        match &proposal.action {
            RuleAction::Trash { .. } => {
                if proposal.path.starts_with(store.paths().trash_dir.as_path()) {
                    skipped.push(SkippedRuleAction {
                        rule: proposal.rule.clone(),
                        session_id: proposal.session_id.clone(),
                        path: proposal.path.clone(),
                        agent: proposal.agent,
                        action: proposal.action.clone(),
                        skip_reason: "session is already in trash".to_owned(),
                    });
                    continue;
                }
                match store.trash_session(&proposal.path, proposal.agent) {
                    Ok(_) => applied.push(AppliedRuleAction {
                        rule: proposal.rule.clone(),
                        session_id: proposal.session_id.clone(),
                        path: proposal.path.clone(),
                        agent: proposal.agent,
                        action: proposal.action.clone(),
                    }),
                    Err(error) => skipped.push(SkippedRuleAction {
                        rule: proposal.rule.clone(),
                        session_id: proposal.session_id.clone(),
                        path: proposal.path.clone(),
                        agent: proposal.agent,
                        action: proposal.action.clone(),
                        skip_reason: format!("{error:#}"),
                    }),
                }
            }
            RuleAction::Untrash { .. } => {
                if !proposal.path.starts_with(store.paths().trash_dir.as_path()) {
                    skipped.push(SkippedRuleAction {
                        rule: proposal.rule.clone(),
                        session_id: proposal.session_id.clone(),
                        path: proposal.path.clone(),
                        agent: proposal.agent,
                        action: proposal.action.clone(),
                        skip_reason: "session is already untrashed".to_owned(),
                    });
                    continue;
                }
                match store.restore_file(&proposal.path) {
                    Ok(_) => applied.push(AppliedRuleAction {
                        rule: proposal.rule.clone(),
                        session_id: proposal.session_id.clone(),
                        path: proposal.path.clone(),
                        agent: proposal.agent,
                        action: proposal.action.clone(),
                    }),
                    Err(error) => skipped.push(SkippedRuleAction {
                        rule: proposal.rule.clone(),
                        session_id: proposal.session_id.clone(),
                        path: proposal.path.clone(),
                        agent: proposal.agent,
                        action: proposal.action.clone(),
                        skip_reason: format!("{error:#}"),
                    }),
                }
            }
        }
    }
    (applied, skipped)
}

fn file_matches_filters(file: &SessionFile, filters: &SearchFilters) -> bool {
    if let Some(agent) = filters.agent {
        if file.agent != agent {
            return false;
        }
    }

    match filters.trashed {
        TrashFilter::No if file.trashed => return false,
        TrashFilter::Yes if !file.trashed => return false,
        TrashFilter::No | TrashFilter::Yes | TrashFilter::Both => {}
    }

    true
}

fn session_matches_scope(scope: &Scope, session: &StoredSession) -> bool {
    match scope {
        Scope::Global => true,
        Scope::CurrentDir(original, canonical) => {
            let stored = [Some(session.project.as_str()), session.cwd.as_deref()];
            let original = original.to_string_lossy();
            stored.iter().flatten().any(|candidate| {
                paths_equal(&original, candidate)
                    || canonical
                        .as_ref()
                        .is_some_and(|path| paths_equal(&path.to_string_lossy(), candidate))
            })
        }
    }
}

fn session_matches_filters(
    session: &StoredSession,
    file: &SessionFile,
    filters: &SearchFilters,
) -> bool {
    if let Some(session_id) = filters.session_id.as_deref() {
        if session.session_id != session_id {
            return false;
        }
    }

    if let Some(branch) = filters.branch.as_deref() {
        if session.branch.as_deref() != Some(branch) {
            return false;
        }
    }

    if let Some(after_ts) = filters.after_ts {
        if session.modified_ts < after_ts {
            return false;
        }
    }

    if let Some(before_ts) = filters.before_ts {
        if session.modified_ts > before_ts {
            return false;
        }
    }

    if let Some(min_lines) = filters.min_lines {
        if session.lines < min_lines {
            return false;
        }
    }

    if !allows_derivation(filters, session.derivation_type) {
        return false;
    }

    match filters.trashed {
        TrashFilter::No if file.trashed => return false,
        TrashFilter::Yes if !file.trashed => return false,
        TrashFilter::No | TrashFilter::Yes | TrashFilter::Both => {}
    }

    true
}

fn allows_derivation(filters: &SearchFilters, derivation: DerivationType) -> bool {
    match derivation {
        DerivationType::Original => filters.include_original,
        DerivationType::Trimmed => filters.include_trimmed,
        DerivationType::Continued => filters.include_continued,
        DerivationType::SubAgent => filters.include_sub_agents,
    }
}

fn paths_equal(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        let normalize = |path: &str| {
            path.replace('\\', "/")
                .trim_end_matches('/')
                .to_ascii_lowercase()
        };
        normalize(a) == normalize(b)
    } else {
        Path::new(a) == Path::new(b)
    }
}

impl RuleAction {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Trash { .. } => "trash",
            Self::Untrash { .. } => "untrash",
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Trash { reason } | Self::Untrash { reason } => reason.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct RuleInput {
    session: RuleSession,
    turns: RuleTurns,
}

impl RuleInput {
    fn from_session(session: &Session, file: &SessionFile, superseded_by: Option<&str>) -> Self {
        Self::from_source(session, &file.path, file.trashed, superseded_by)
    }

    fn from_source(
        session: &Session,
        path: &Path,
        trashed: bool,
        superseded_by: Option<&str>,
    ) -> Self {
        Self {
            session: RuleSession::from_source(session, path, trashed, superseded_by),
            turns: RuleTurns::from_session(session),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct RuleDetails {
    values: BTreeMap<(String, usize, String), String>,
}

impl RuleDetails {
    fn from_session(session: &Session) -> Self {
        let mut details = Self::default();
        let cells = rule_cells(session);
        for (index, cell) in cells.iter().enumerate() {
            match cell {
                SessionCell::Message { role, content, .. } => match role {
                    MessageRole::User if is_contextual_user_message_content(*role, content) => {
                        details.insert("contextual_user", index, "text", content);
                    }
                    MessageRole::User => details.insert("user", index, "text", content),
                    MessageRole::Assistant => details.insert("agent", index, "text", content),
                    MessageRole::System | MessageRole::Summary => {
                        details.insert("system", index, "text", content);
                    }
                    MessageRole::ToolCall | MessageRole::ToolResult => {}
                },
                SessionCell::Reasoning { header, body, .. } => {
                    let text = header
                        .as_ref()
                        .map(|header| format!("{header}\n{body}"))
                        .unwrap_or_else(|| body.clone());
                    details.insert("agent", index, "text", text);
                }
                SessionCell::ToolResult { output, .. } => {
                    details.insert("tool_result", index, "text", output);
                }
                SessionCell::Exec { stdout, stderr, .. } => {
                    details.insert("exec", index, "stdout", stdout);
                    details.insert("exec", index, "stderr", stderr);
                }
                SessionCell::Patch { files, .. } => {
                    for (file_index, file) in files.iter().enumerate() {
                        if let Some(content) = &file.content {
                            details.insert("patch", index, file_index.to_string(), content);
                        }
                    }
                }
                SessionCell::ToolCall { .. }
                | SessionCell::WebSearch { .. }
                | SessionCell::Plan { .. }
                | SessionCell::SessionInfo(_)
                | SessionCell::Metrics(_) => {}
            }
        }

        details
    }

    fn fetch(&self, kind: &str, index: usize, field: &str, limit: usize) -> Option<String> {
        self.values
            .get(&(kind.to_owned(), index, field.to_owned()))
            .map(|text| RuleText::limit(text, limit).text)
    }

    fn insert(
        &mut self,
        kind: impl Into<String>,
        index: usize,
        field: impl Into<String>,
        text: impl AsRef<str>,
    ) {
        self.values
            .insert((kind.into(), index, field.into()), text.as_ref().to_owned());
    }
}

fn rule_cells(session: &Session) -> Vec<SessionCell> {
    if session.cells.is_empty() {
        crate::parse::session::cells_from_messages(&session.messages)
    } else {
        session.cells.clone()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuleSession {
    id: String,
    agent: &'static str,
    project: String,
    cwd: String,
    branch: String,
    path: String,
    modified_ts: u64,
    lines: usize,
    derivation_type: &'static str,
    is_sidechain: bool,
    custom_title: String,
    model: String,
    model_provider: String,
    reasoning_effort: String,
    approval_policy: String,
    sandbox_mode: String,
    superseded_by: String,
    trashed: bool,
}

impl RuleSession {
    fn from_source(
        session: &Session,
        path: &Path,
        trashed: bool,
        superseded_by: Option<&str>,
    ) -> Self {
        let info = session.session_info.as_ref();
        Self {
            id: session.session_id.clone(),
            agent: session.agent.as_str(),
            project: session.project.clone(),
            cwd: session.cwd.clone().unwrap_or_default(),
            branch: session.branch.clone().unwrap_or_default(),
            path: path.to_string_lossy().into_owned(),
            modified_ts: session.modified_ts,
            lines: session.lines,
            derivation_type: session.derivation_type.as_str(),
            is_sidechain: session.is_sidechain,
            custom_title: session.custom_title.clone().unwrap_or_default(),
            model: info.and_then(|info| info.model.clone()).unwrap_or_default(),
            model_provider: info
                .and_then(|info| info.model_provider.clone())
                .unwrap_or_default(),
            reasoning_effort: info
                .and_then(|info| info.reasoning_effort.clone())
                .unwrap_or_default(),
            approval_policy: info
                .and_then(|info| info.approval_policy.clone())
                .unwrap_or_default(),
            sandbox_mode: info
                .and_then(|info| info.sandbox_mode.clone())
                .unwrap_or_default(),
            superseded_by: superseded_by.unwrap_or_default().to_owned(),
            trashed,
        }
    }
}

fn materialize_rule_context(context: &mut serde_json::Value, details: &RuleDetails) -> Result<()> {
    let turns = context
        .get_mut("turns")
        .and_then(serde_json::Value::as_object_mut)
        .context("serialized rules session data is missing `turns`")?;

    for (collection, kind) in [
        ("user", "user"),
        ("contextualUser", "contextual_user"),
        ("agent", "agent"),
        ("system", "system"),
        ("toolResults", "tool_result"),
    ] {
        materialize_rule_turn_field(turns, details, collection, kind, "text")?;
    }
    materialize_rule_turn_field(turns, details, "exec", "exec", "stdout")?;
    materialize_rule_turn_field(turns, details, "exec", "exec", "stderr")?;
    materialize_rule_patch_content(turns, details)?;
    Ok(())
}

fn materialize_rule_turn_field(
    turns: &mut serde_json::Map<String, serde_json::Value>,
    details: &RuleDetails,
    collection: &str,
    kind: &str,
    field: &str,
) -> Result<()> {
    let entries = turns
        .get_mut(collection)
        .and_then(serde_json::Value::as_array_mut)
        .with_context(|| {
            format!("serialized rules session data is missing `turns.{collection}`")
        })?;
    for entry in entries {
        let index = entry
            .get("index")
            .and_then(serde_json::Value::as_u64)
            .and_then(|index| usize::try_from(index).ok())
            .with_context(|| format!("rules turn in `{collection}` has no valid index"))?;
        let text = details
            .fetch(kind, index, field, usize::MAX)
            .unwrap_or_default();
        entry
            .as_object_mut()
            .with_context(|| format!("rules turn in `{collection}` is not an object"))?
            .insert(field.to_owned(), serde_json::Value::String(text));
    }
    Ok(())
}

fn materialize_rule_patch_content(
    turns: &mut serde_json::Map<String, serde_json::Value>,
    details: &RuleDetails,
) -> Result<()> {
    let patches = turns
        .get_mut("patches")
        .and_then(serde_json::Value::as_array_mut)
        .context("serialized rules session data is missing `turns.patches`")?;
    for patch in patches {
        let index = patch
            .get("index")
            .and_then(serde_json::Value::as_u64)
            .and_then(|index| usize::try_from(index).ok())
            .context("rules patch turn has no valid index")?;
        let files = patch
            .get_mut("files")
            .and_then(serde_json::Value::as_array_mut)
            .context("rules patch turn is missing `files`")?;
        for (file_index, file) in files.iter_mut().enumerate() {
            let content = details
                .fetch("patch", index, &file_index.to_string(), usize::MAX)
                .unwrap_or_default();
            file.as_object_mut()
                .context("rules patch file is not an object")?
                .insert("content".to_owned(), serde_json::Value::String(content));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuleTurns {
    user: Vec<TextTurn>,
    contextual_user: Vec<TextTurn>,
    agent: Vec<TextTurn>,
    system: Vec<TextTurn>,
    tool_calls: Vec<ToolCallTurn>,
    tool_results: Vec<ToolResultTurn>,
    exec: Vec<ExecTurn>,
    patches: Vec<PatchTurn>,
}

impl RuleTurns {
    fn from_session(session: &Session) -> Self {
        let mut turns = Self::default();
        let cells = rule_cells(session);

        for (index, cell) in cells.iter().enumerate() {
            match cell {
                SessionCell::Message {
                    role,
                    content,
                    timestamp,
                } => match role {
                    MessageRole::User if is_contextual_user_message_content(*role, content) => {
                        turns
                            .contextual_user
                            .push(TextTurn::new(index, content, timestamp));
                    }
                    MessageRole::User => turns.user.push(TextTurn::new(index, content, timestamp)),
                    MessageRole::Assistant => {
                        turns.agent.push(TextTurn::new(index, content, timestamp));
                    }
                    MessageRole::System | MessageRole::Summary => {
                        turns.system.push(TextTurn::new(index, content, timestamp));
                    }
                    MessageRole::ToolCall | MessageRole::ToolResult => {}
                },
                SessionCell::Reasoning {
                    header,
                    body,
                    timestamp,
                } => {
                    let text = header
                        .as_ref()
                        .map(|header| format!("{header}\n{body}"))
                        .unwrap_or_else(|| body.clone());
                    turns.agent.push(TextTurn::new(index, &text, timestamp));
                }
                SessionCell::ToolCall {
                    tool,
                    summary,
                    timestamp,
                    ..
                } => turns.tool_calls.push(ToolCallTurn {
                    index,
                    tool: tool.clone(),
                    summary: summary.clone(),
                    timestamp: timestamp.map(|timestamp| timestamp.to_rfc3339()),
                }),
                SessionCell::ToolResult {
                    tool,
                    output: _,
                    is_error,
                    timestamp,
                    ..
                } => {
                    turns.tool_results.push(ToolResultTurn {
                        index,
                        tool: tool.clone(),
                        is_error: *is_error,
                        timestamp: timestamp.map(|timestamp| timestamp.to_rfc3339()),
                    });
                }
                SessionCell::Exec {
                    command,
                    cwd,
                    stdout: _,
                    stderr: _,
                    exit_code,
                    timestamp,
                    ..
                } => {
                    turns.exec.push(ExecTurn {
                        index,
                        command: command.clone(),
                        cwd: cwd.clone(),
                        exit_code: *exit_code,
                        timestamp: timestamp.map(|timestamp| timestamp.to_rfc3339()),
                    });
                }
                SessionCell::Patch {
                    files,
                    success,
                    timestamp,
                    ..
                } => turns.patches.push(PatchTurn {
                    index,
                    files: preview_patch_files(files),
                    success: *success,
                    timestamp: timestamp.map(|timestamp| timestamp.to_rfc3339()),
                }),
                SessionCell::WebSearch { .. }
                | SessionCell::Plan { .. }
                | SessionCell::SessionInfo(_)
                | SessionCell::Metrics(_) => {}
            }
        }

        turns
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TextTurn {
    index: usize,
    timestamp: Option<String>,
}

impl TextTurn {
    fn new(index: usize, _text: &str, timestamp: &Option<chrono::DateTime<chrono::Utc>>) -> Self {
        Self {
            index,
            timestamp: timestamp.map(|timestamp| timestamp.to_rfc3339()),
        }
    }
}

struct RuleText {
    text: String,
}

impl RuleText {
    fn limit(text: &str, limit: usize) -> Self {
        if text.len() <= limit {
            return Self {
                text: text.to_owned(),
            };
        }

        let mut end = limit.min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        let mut preview = text[..end].to_owned();
        preview.push_str(TRUNCATED_SUFFIX);
        Self { text: preview }
    }
}

fn preview_patch_files(files: &[PatchFile]) -> Vec<RulePatchFile> {
    files
        .iter()
        .map(|file| RulePatchFile {
            path: file.path.clone(),
            op: file.op.clone(),
            additions: file.additions,
            deletions: file.deletions,
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolCallTurn {
    index: usize,
    tool: String,
    summary: String,
    timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolResultTurn {
    index: usize,
    tool: Option<String>,
    is_error: bool,
    timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecTurn {
    index: usize,
    command: Vec<String>,
    cwd: Option<String>,
    exit_code: Option<i32>,
    timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PatchTurn {
    index: usize,
    files: Vec<RulePatchFile>,
    success: bool,
    timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RulePatchFile {
    path: String,
    op: crate::parse::PatchOp,
    additions: usize,
    deletions: usize,
}

#[cfg(test)]
mod tests {
    use super::{
        run_parallel_jobs, session_to_rule_context_json, JsRuleEngine, RuleDetails, RuleInput,
        RuleSelection, RuleSession, RuleTurns,
    };
    use crate::parse::{
        Agent, DerivationType, ExecStatus, MessageRole, PatchFile, PatchOp, Session, SessionCell,
        ToolStatus,
    };
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    fn input(agent: &'static str, _user_text: &str) -> RuleInput {
        RuleInput {
            session: RuleSession {
                id: "s1".to_owned(),
                agent,
                project: "/tmp/project".to_owned(),
                cwd: "/tmp/project".to_owned(),
                branch: String::new(),
                path: "/tmp/session.jsonl".to_owned(),
                modified_ts: 0,
                lines: 1,
                derivation_type: "original",
                is_sidechain: false,
                custom_title: String::new(),
                model: "test-spark".to_owned(),
                model_provider: String::new(),
                reasoning_effort: String::new(),
                approval_policy: String::new(),
                sandbox_mode: String::new(),
                superseded_by: String::new(),
                trashed: false,
            },
            turns: RuleTurns {
                user: vec![super::TextTurn {
                    index: 0,
                    timestamp: None,
                }],
                ..RuleTurns::default()
            },
        }
    }

    fn details(user_text: &str) -> RuleDetails {
        let mut details = RuleDetails::default();
        details.insert("user", 0, "text", user_text);
        details
    }

    fn message(role: MessageRole, content: &str) -> SessionCell {
        SessionCell::Message {
            role,
            content: content.to_owned(),
            timestamp: None,
        }
    }

    fn session_with_cells(cells: Vec<SessionCell>) -> Session {
        Session {
            session_id: "s1".to_owned(),
            agent: Agent::Codex,
            project: "/tmp/project".to_owned(),
            branch: None,
            cwd: Some("/tmp/project".to_owned()),
            created: None,
            modified: None,
            modified_ts: 0,
            lines: cells.len(),
            file_path: PathBuf::from("/tmp/session.jsonl"),
            first_msg_role: None,
            first_msg_content: String::new(),
            last_msg_role: None,
            last_msg_content: String::new(),
            first_user_msg_content: String::new(),
            derivation_type: DerivationType::Original,
            is_sidechain: false,
            custom_title: None,
            messages: Vec::new(),
            content: String::new(),
            search_fields: Default::default(),
            cells,
            session_info: None,
            lineage: Default::default(),
        }
    }

    #[test]
    fn parallel_jobs_initialize_workers_and_consume_results_on_caller() {
        let jobs = (0..64).collect::<Vec<_>>();
        let worker_threads = Arc::new(Mutex::new(HashSet::new()));
        let caller = thread::current().id();
        let mut seen = vec![false; jobs.len()];

        run_parallel_jobs(
            &jobs,
            4,
            || {
                worker_threads
                    .lock()
                    .unwrap()
                    .insert(thread::current().id());
                Ok(())
            },
            |_, job| (*job, thread::current().id()),
            |job_index, (value, worker)| {
                assert_eq!(thread::current().id(), caller);
                assert_ne!(worker, caller);
                assert_eq!(value, jobs[job_index]);
                assert!(!seen[job_index]);
                seen[job_index] = true;
            },
        )
        .unwrap();

        assert_eq!(worker_threads.lock().unwrap().len(), 4);
        assert!(seen.into_iter().all(|value| value));
    }

    #[test]
    fn parallel_jobs_do_not_start_when_a_worker_fails_to_initialize() {
        let jobs = [1, 2, 3, 4];
        let initializations = AtomicUsize::new(0);
        let evaluations = AtomicUsize::new(0);

        let error = run_parallel_jobs(
            &jobs,
            2,
            || -> anyhow::Result<()> {
                if initializations.fetch_add(1, Ordering::Relaxed) == 0 {
                    anyhow::bail!("test initialization failure");
                }
                Ok(())
            },
            |_, job| {
                evaluations.fetch_add(1, Ordering::Relaxed);
                *job
            },
            |_, _| panic!("no result should be consumed"),
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("failed to initialize rules worker"));
        assert_eq!(initializations.load(Ordering::Relaxed), 2);
        assert_eq!(evaluations.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn js_rule_preserves_non_empty_nothing_decisions() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("rules.js");
        std::fs::write(
            &rules,
            r#"
            rule("explained no-op", () => nothing("more than 3 user turns"));
            rule("silent no-op", () => nothing());
            rule("blank no-op", () => nothing("  "));
            "#,
        )
        .unwrap();

        let engine = JsRuleEngine::load(&rules).unwrap();
        let outcomes = engine
            .evaluate(
                &input("claude", "preview"),
                details("preview"),
                RuleSelection::All,
            )
            .unwrap();

        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].rule, "explained no-op");
        assert_eq!(outcomes[0].action.as_deref(), Some("nothing"));
        assert_eq!(
            outcomes[0].reason.as_deref(),
            Some("more than 3 user turns")
        );
    }

    #[test]
    fn rule_turns_split_contextual_user_messages_from_real_user_messages() {
        let docs = "# AGENTS.md instructions for /tmp/project\n\n<INSTRUCTIONS>\nMemory mentions $commit.\n</INSTRUCTIONS>";
        let request = "$commit --all";
        let session = session_with_cells(vec![
            message(MessageRole::User, docs),
            message(MessageRole::User, request),
        ]);

        let turns = RuleTurns::from_session(&session);
        assert_eq!(turns.contextual_user.len(), 1);
        assert_eq!(turns.user.len(), 1);
        assert_eq!(turns.contextual_user[0].index, 0);
        assert_eq!(turns.user[0].index, 1);

        let details = RuleDetails::from_session(&session);
        assert_eq!(
            details.fetch("contextual_user", 0, "text", usize::MAX),
            Some(docs.to_owned())
        );
        assert_eq!(
            details.fetch("user", 1, "text", usize::MAX),
            Some(request.to_owned())
        );
        assert_eq!(details.fetch("user", 0, "text", usize::MAX), None);
    }

    #[test]
    fn rule_context_json_materializes_lazy_transcript_fields() {
        let docs = "# AGENTS.md instructions\n\n<INSTRUCTIONS>project context</INSTRUCTIONS>";
        let mut session = session_with_cells(vec![
            message(MessageRole::User, docs),
            message(MessageRole::User, "real request"),
            SessionCell::Reasoning {
                header: Some("analysis".to_owned()),
                body: "consider the request".to_owned(),
                timestamp: None,
            },
            message(MessageRole::System, "system context"),
            SessionCell::ToolCall {
                tool: "read".to_owned(),
                raw_name: "Read".to_owned(),
                summary: "src/main.rs".to_owned(),
                input: serde_json::Value::Null,
                status: ToolStatus::Completed,
                timestamp: None,
            },
            SessionCell::ToolResult {
                tool: Some("read".to_owned()),
                output: "file contents".to_owned(),
                is_error: false,
                call_summary: None,
                timestamp: None,
            },
            SessionCell::Exec {
                command: vec!["cargo check".to_owned()],
                cwd: Some("/tmp/project".to_owned()),
                parsed_summary: None,
                stdout: "check stdout".to_owned(),
                stderr: "check stderr".to_owned(),
                exit_code: Some(0),
                duration_ms: None,
                status: ExecStatus::Completed,
                timestamp: None,
            },
            SessionCell::Patch {
                files: vec![PatchFile {
                    path: "src/main.rs".to_owned(),
                    op: PatchOp::Update,
                    content: Some("updated file".to_owned()),
                    additions: 1,
                    deletions: 1,
                }],
                success: true,
                stdout: String::new(),
                stderr: String::new(),
                timestamp: None,
            },
        ]);
        session.custom_title = Some("Rules export".to_owned());

        let rendered = session_to_rule_context_json(
            &session,
            std::path::Path::new("/tmp/session.jsonl"),
            true,
            Some("keeper-session"),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();

        assert_eq!(value["session"]["id"], "s1");
        assert_eq!(value["session"]["agent"], "codex");
        assert_eq!(value["session"]["path"], "/tmp/session.jsonl");
        assert_eq!(value["session"]["customTitle"], "Rules export");
        assert_eq!(value["session"]["supersededBy"], "keeper-session");
        assert_eq!(value["session"]["trashed"], true);
        assert_eq!(value["turns"]["contextualUser"][0]["text"], docs);
        assert_eq!(value["turns"]["user"][0]["text"], "real request");
        assert_eq!(
            value["turns"]["agent"][0]["text"],
            "analysis\nconsider the request"
        );
        assert_eq!(value["turns"]["system"][0]["text"], "system context");
        assert_eq!(value["turns"]["toolCalls"][0]["tool"], "read");
        assert_eq!(value["turns"]["toolResults"][0]["text"], "file contents");
        assert_eq!(value["turns"]["exec"][0]["stdout"], "check stdout");
        assert_eq!(value["turns"]["exec"][0]["stderr"], "check stderr");
        assert_eq!(
            value["turns"]["patches"][0]["files"][0]["content"],
            "updated file"
        );
        assert!(rendered.ends_with('\n'));
        assert!(!rendered.contains(super::TRUNCATED_SUFFIX));
    }

    #[test]
    fn js_rule_returns_trash_action() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("rules.js");
        std::fs::write(
            &rules,
            r#"
            rule("commit sessions", ({ turns }) => {
              return /\s*[/$](?:gdf-)?commit\b/m.test(turns.user[0].text(4096))
                ? trash("commit helper")
                : nothing();
            });
            "#,
        )
        .unwrap();

        let engine = JsRuleEngine::load(&rules).unwrap();
        let outcomes = engine
            .evaluate(
                &input("codex", "$gdf-commit --all"),
                details("$gdf-commit --all"),
                RuleSelection::All,
            )
            .unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].rule, "commit sessions");
        assert_eq!(outcomes[0].action.as_deref(), Some("trash"));
        assert_eq!(outcomes[0].reason.as_deref(), Some("commit helper"));
    }

    #[test]
    fn js_rule_context_omits_removed_helpers() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("rules.js");
        std::fs::write(
            &rules,
            r#"
            rule("removed helpers are absent", (context) => {
              const contextHelpersAbsent =
                !("re" in context) &&
                !("text" in context) &&
                !("turnText" in context);
              const sessionHelpersAbsent =
                !("firstUserText" in context.session) &&
                !("firstText" in context.session) &&
                !("lastText" in context.session);
              return contextHelpersAbsent && sessionHelpersAbsent
                ? trash("removed helpers are absent")
                : nothing();
            });
            "#,
        )
        .unwrap();

        let engine = JsRuleEngine::load(&rules).unwrap();
        let outcomes = engine
            .evaluate(
                &input("codex", "preview"),
                details("preview"),
                RuleSelection::All,
            )
            .unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].action.as_deref(), Some("trash"));
        assert_eq!(
            outcomes[0].reason.as_deref(),
            Some("removed helpers are absent")
        );
    }

    #[test]
    fn js_rule_accepts_config_argument() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("rules.js");
        std::fs::write(
            &rules,
            r#"
            rule("configured", { applyAtStartup: true }, ({ session }) => {
              return session.agent === "codex" ? trash("configured rule") : nothing();
            });
            rule("disabled", { applyAtStartup: false }, () => trash("disabled rule"));
            rule("unconfigured", () => trash("unconfigured rule"));
            "#,
        )
        .unwrap();

        let engine = JsRuleEngine::load(&rules).unwrap();
        assert_eq!(
            engine.rule_names().unwrap(),
            vec!["configured", "disabled", "unconfigured"]
        );

        let all_outcomes = engine
            .evaluate(
                &input("codex", "preview"),
                details("preview"),
                RuleSelection::All,
            )
            .unwrap();
        assert_eq!(all_outcomes.len(), 3);

        let startup_outcomes = engine
            .evaluate(
                &input("codex", "preview"),
                details("preview"),
                RuleSelection::ApplyAtStartup,
            )
            .unwrap();
        assert_eq!(startup_outcomes.len(), 1);
        assert_eq!(startup_outcomes[0].rule, "configured");
        assert_eq!(startup_outcomes[0].action.as_deref(), Some("trash"));
        assert_eq!(
            startup_outcomes[0].reason.as_deref(),
            Some("configured rule")
        );
    }

    #[test]
    fn js_rule_user_turns_exclude_contextual_user_messages() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("rules.js");
        std::fs::write(
            &rules,
            r#"
            rule("commit sessions", ({ turns }) => {
              return turns.user.length > 0 &&
                /\s*[/$](?:gdf-)?commit\b/m.test(turns.user[0].text(4096))
                ? trash("commit helper")
                : nothing();
            });

            rule("context available", ({ turns }) => {
              return turns.contextualUser.length === 1 &&
                turns.contextualUser[0].text(4096).includes("memory mentions $commit")
                ? trash("context was split")
                : nothing();
            });
            "#,
        )
        .unwrap();

        let input = RuleInput {
            session: RuleSession {
                id: "s1".to_owned(),
                agent: "codex",
                project: "/tmp/project".to_owned(),
                cwd: "/tmp/project".to_owned(),
                branch: String::new(),
                path: "/tmp/session.jsonl".to_owned(),
                modified_ts: 0,
                lines: 2,
                derivation_type: "original",
                is_sidechain: false,
                custom_title: String::new(),
                model: "test-spark".to_owned(),
                model_provider: String::new(),
                reasoning_effort: String::new(),
                approval_policy: String::new(),
                sandbox_mode: String::new(),
                superseded_by: String::new(),
                trashed: false,
            },
            turns: RuleTurns {
                user: vec![super::TextTurn {
                    index: 1,
                    timestamp: None,
                }],
                contextual_user: vec![super::TextTurn {
                    index: 0,
                    timestamp: None,
                }],
                ..RuleTurns::default()
            },
        };
        let mut details = RuleDetails::default();
        details.insert(
            "contextual_user",
            0,
            "text",
            "# AGENTS.md instructions\n\n<INSTRUCTIONS>memory mentions $commit</INSTRUCTIONS>",
        );
        details.insert("user", 1, "text", "real first user request");

        let engine = JsRuleEngine::load(&rules).unwrap();
        let outcomes = engine
            .evaluate(&input, details, RuleSelection::All)
            .unwrap();

        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].rule, "context available");
        assert_eq!(outcomes[0].action.as_deref(), Some("trash"));
        assert_eq!(outcomes[0].reason.as_deref(), Some("context was split"));
    }

    #[test]
    fn js_rule_can_fetch_full_text_on_demand() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("rules.js");
        std::fs::write(
            &rules,
            r#"
            rule("lazy", ({ turns }) => {
              return turns.user[0].text(64).includes("full-only marker")
                ? trash("lazy text")
                : nothing();
            });
            "#,
        )
        .unwrap();

        let engine = JsRuleEngine::load(&rules).unwrap();
        let outcomes = engine
            .evaluate(
                &input("codex", "preview"),
                details("full-only marker"),
                RuleSelection::All,
            )
            .unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].rule, "lazy");
        assert_eq!(outcomes[0].action.as_deref(), Some("trash"));
        assert_eq!(outcomes[0].reason.as_deref(), Some("lazy text"));
    }

    #[test]
    fn duplicate_rule_names_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("rules.js");
        std::fs::write(
            &rules,
            r#"
            rule("same", () => nothing());
            rule("same", () => nothing());
            "#,
        )
        .unwrap();

        let error = match JsRuleEngine::load(&rules) {
            Ok(_) => panic!("duplicate rule names should fail"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("failed to load rules"));
    }
}
