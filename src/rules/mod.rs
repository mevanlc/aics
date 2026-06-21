use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use log::warn;
use rquickjs::{CatchResultExt, Context as JsContext, Runtime};
use serde::{Deserialize, Serialize};

use crate::index::{Scope, SearchFilters, TrashFilter};
use crate::parse::{
    parse_session_file, Agent, DerivationType, MessageRole, PatchFile, Session, SessionCell,
};
use crate::scan::{scan_session_files, SessionFile, SessionRoots};
use crate::settings::config_dir;
use crate::trash::TrashStore;

const RULE_MEMORY_LIMIT: usize = 64 * 1024 * 1024;
const RULE_STACK_LIMIT: usize = 512 * 1024;
const RULE_EVAL_TIMEOUT: Duration = Duration::from_millis(250);

const RULES_HARNESS: &str = r#"
const __aicsRules = [];
const __aicsRuleNames = new Set();

globalThis.rule = function(name, callback) {
  if (typeof name !== "string" || name.trim() === "") {
    throw new TypeError("rule name must be a non-empty string");
  }
  if (typeof callback !== "function") {
    throw new TypeError(`rule ${name} callback must be a function`);
  }
  if (__aicsRuleNames.has(name)) {
    throw new Error(`duplicate rule name: ${name}`);
  }
  __aicsRuleNames.add(name);
  __aicsRules.push({ name, callback });
};

globalThis.nothing = function() {
  return { action: "nothing" };
};

globalThis.trash = function(reason) {
  return {
    action: "trash",
    reason: reason == null ? null : String(reason),
  };
};

globalThis.__aicsRuleNames = function() {
  return JSON.stringify(__aicsRules.map((entry) => entry.name));
};

globalThis.__aicsRunRules = function(contextJson) {
  const context = JSON.parse(contextJson);
  context.re = function(pattern, flags) {
    return new RegExp(pattern, flags);
  };

  const outcomes = [];
  for (const entry of __aicsRules) {
    try {
      const raw = entry.callback(context);
      const actions = Array.isArray(raw) ? raw : [raw];
      for (const action of actions) {
        if (action == null || action.action === "nothing") {
          continue;
        }
        outcomes.push({
          rule: entry.name,
          action: String(action.action),
          reason: action.reason == null ? null : String(action.reason),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulesMode {
    Preview,
    Apply,
}

#[derive(Debug, Clone)]
pub struct RulesOptions {
    pub rules_path: PathBuf,
    pub mode: RulesMode,
    pub json: bool,
    pub scope: Scope,
    pub filters: SearchFilters,
}

#[derive(Debug, Clone, Default)]
pub struct RulesReport {
    pub proposals: Vec<RuleProposal>,
    pub applied: Vec<AppliedRuleAction>,
    pub skipped: Vec<SkippedRuleAction>,
    pub errors: Vec<RuleEvaluationError>,
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

pub fn run_rules(roots: &SessionRoots, options: &RulesOptions) -> Result<RulesReport> {
    if !options.rules_path.exists() {
        bail!(
            "rules file not found: {}",
            display_rules_path(&options.rules_path)
        );
    }

    let engine = JsRuleEngine::load(&options.rules_path)?;
    let files = scan_session_files(roots)?;
    let mut report = RulesReport::default();

    for file in files {
        if !file_matches_filters(&file, &options.filters) {
            continue;
        }
        let parsed = match parse_session_file(file.agent, &file.path) {
            Ok(Some(session)) => session,
            Ok(None) => continue,
            Err(error) => {
                warn!(
                    "failed to parse {} for rules: {error:#}",
                    file.path.display()
                );
                continue;
            }
        };
        if !session_matches_scope(&options.scope, &parsed) {
            continue;
        }
        if !session_matches_filters(&parsed, &file, &options.filters) {
            continue;
        }

        let input = RuleInput::from_session(&parsed, &file);
        match engine.evaluate(&input) {
            Ok(outcomes) => collect_outcomes(&mut report, &parsed, &file, outcomes),
            Err(error) => report.errors.push(RuleEvaluationError {
                rule: None,
                path: file.path.clone(),
                error: format!("{error:#}"),
            }),
        }
    }

    report.proposals = dedupe_proposals(std::mem::take(&mut report.proposals));

    if matches!(options.mode, RulesMode::Apply) {
        apply_proposals(roots, &mut report);
    }

    Ok(report)
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
    runtime: Runtime,
    context: JsContext,
}

impl JsRuleEngine {
    fn load(path: &Path) -> Result<Self> {
        let runtime = Runtime::new().context("failed to create QuickJS runtime")?;
        runtime.set_memory_limit(RULE_MEMORY_LIMIT);
        runtime.set_max_stack_size(RULE_STACK_LIMIT);
        let context = JsContext::full(&runtime).context("failed to create QuickJS context")?;

        let source = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let engine = Self { runtime, context };
        engine
            .with_timeout(|| {
                engine.context.with(|ctx| -> Result<()> {
                    ctx.eval::<(), _>(RULES_HARNESS)
                        .catch(&ctx)
                        .map_err(|error| anyhow!("{error}"))?;
                    ctx.eval::<(), _>(source.clone())
                        .catch(&ctx)
                        .map_err(|error| anyhow!("{error}"))?;
                    Ok(())
                })
            })
            .with_context(|| format!("failed to load rules from {}", path.display()))?;

        engine.rule_names()?;
        Ok(engine)
    }

    fn rule_names(&self) -> Result<Vec<String>> {
        let json = self.with_timeout(|| {
            self.context.with(|ctx| -> Result<String> {
                ctx.eval("globalThis.__aicsRuleNames()")
                    .catch(&ctx)
                    .map_err(|error| anyhow!("{error}"))
            })
        })?;
        serde_json::from_str(&json).context("rules runtime returned invalid rule-name JSON")
    }

    fn evaluate(&self, input: &RuleInput) -> Result<Vec<RawRuleOutcome>> {
        let input_json = serde_json::to_string(input).context("failed to serialize rule input")?;
        let input_literal =
            serde_json::to_string(&input_json).context("failed to quote rule input")?;
        let script = format!("globalThis.__aicsRunRules({input_literal})");
        let output_json = self.with_timeout(|| {
            self.context.with(|ctx| -> Result<String> {
                ctx.eval(script)
                    .catch(&ctx)
                    .map_err(|error| anyhow!("{error}"))
            })
        })?;
        serde_json::from_str(&output_json).context("rules runtime returned invalid action JSON")
    }

    fn with_timeout<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce() -> Result<T>,
    {
        let start = Instant::now();
        self.runtime
            .set_interrupt_handler(Some(Box::new(move || start.elapsed() >= RULE_EVAL_TIMEOUT)));
        let result = f();
        self.runtime.set_interrupt_handler(None);
        result
    }
}

#[derive(Debug, Clone, Deserialize)]
struct RawRuleOutcome {
    rule: String,
    action: Option<String>,
    reason: Option<String>,
    error: Option<String>,
}

fn collect_outcomes(
    report: &mut RulesReport,
    session: &Session,
    file: &SessionFile,
    outcomes: Vec<RawRuleOutcome>,
) {
    for outcome in outcomes {
        if let Some(error) = outcome.error {
            report.errors.push(RuleEvaluationError {
                rule: Some(outcome.rule),
                path: file.path.clone(),
                error,
            });
            continue;
        }

        match outcome.action.as_deref() {
            Some("trash") => report.proposals.push(RuleProposal {
                rule: outcome.rule,
                session_id: session.session_id.clone(),
                path: file.path.clone(),
                agent: file.agent,
                action: RuleAction::Trash {
                    reason: outcome.reason,
                },
            }),
            Some(action) => report.errors.push(RuleEvaluationError {
                rule: Some(outcome.rule),
                path: file.path.clone(),
                error: format!("unsupported rule action: {action}"),
            }),
            None => {}
        }
    }
}

fn dedupe_proposals(proposals: Vec<RuleProposal>) -> Vec<RuleProposal> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for proposal in proposals {
        if seen.insert((proposal.path.clone(), proposal.action.label())) {
            deduped.push(proposal);
        }
    }
    deduped
}

fn apply_proposals(roots: &SessionRoots, report: &mut RulesReport) {
    let Some(paths) = roots.trash.clone() else {
        for proposal in &report.proposals {
            report.skipped.push(SkippedRuleAction {
                rule: proposal.rule.clone(),
                session_id: proposal.session_id.clone(),
                path: proposal.path.clone(),
                agent: proposal.agent,
                action: proposal.action.clone(),
                skip_reason: "trash store is unavailable".to_owned(),
            });
        }
        return;
    };

    let store = TrashStore::new(paths);
    for proposal in &report.proposals {
        match &proposal.action {
            RuleAction::Trash { .. } => {
                if proposal.path.starts_with(store.paths().trash_dir.as_path()) {
                    report.skipped.push(SkippedRuleAction {
                        rule: proposal.rule.clone(),
                        session_id: proposal.session_id.clone(),
                        path: proposal.path.clone(),
                        agent: proposal.agent,
                        action: proposal.action.clone(),
                        skip_reason: "session is already in trash".to_owned(),
                    });
                    continue;
                }
                match store.trash_file(&proposal.path, proposal.agent) {
                    Ok(_) => report.applied.push(AppliedRuleAction {
                        rule: proposal.rule.clone(),
                        session_id: proposal.session_id.clone(),
                        path: proposal.path.clone(),
                        agent: proposal.agent,
                        action: proposal.action.clone(),
                    }),
                    Err(error) => report.skipped.push(SkippedRuleAction {
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

fn session_matches_scope(scope: &Scope, session: &Session) -> bool {
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

fn session_matches_filters(session: &Session, file: &SessionFile, filters: &SearchFilters) -> bool {
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
    fn label(&self) -> &'static str {
        match self {
            Self::Trash { .. } => "trash",
        }
    }

    fn reason(&self) -> Option<&str> {
        match self {
            Self::Trash { reason } => reason.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct RuleInput {
    session: RuleSession,
    turns: RuleTurns,
}

impl RuleInput {
    fn from_session(session: &Session, file: &SessionFile) -> Self {
        Self {
            session: RuleSession::from_session(session, file),
            turns: RuleTurns::from_session(session),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuleSession {
    id: String,
    agent: &'static str,
    project: String,
    cwd: Option<String>,
    branch: Option<String>,
    path: String,
    modified_ts: u64,
    lines: usize,
    derivation_type: &'static str,
    is_sidechain: bool,
    custom_title: Option<String>,
    model: Option<String>,
    model_provider: Option<String>,
    approval_policy: Option<String>,
    sandbox_mode: Option<String>,
    first_user_text: String,
    first_text: String,
    last_text: String,
    trashed: bool,
}

impl RuleSession {
    fn from_session(session: &Session, file: &SessionFile) -> Self {
        let info = session.session_info.as_ref();
        Self {
            id: session.session_id.clone(),
            agent: session.agent.as_str(),
            project: session.project.clone(),
            cwd: session.cwd.clone(),
            branch: session.branch.clone(),
            path: file.path.to_string_lossy().into_owned(),
            modified_ts: session.modified_ts,
            lines: session.lines,
            derivation_type: session.derivation_type.as_str(),
            is_sidechain: session.is_sidechain,
            custom_title: session.custom_title.clone(),
            model: info.and_then(|info| info.model.clone()),
            model_provider: info.and_then(|info| info.model_provider.clone()),
            approval_policy: info.and_then(|info| info.approval_policy.clone()),
            sandbox_mode: info.and_then(|info| info.sandbox_mode.clone()),
            first_user_text: session.first_user_msg_content.clone(),
            first_text: session.first_msg_content.clone(),
            last_text: session.last_msg_content.clone(),
            trashed: file.trashed,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuleTurns {
    user: Vec<TextTurn>,
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
        let cells = if session.cells.is_empty() {
            crate::parse::session::cells_from_messages(&session.messages)
        } else {
            session.cells.clone()
        };

        for (index, cell) in cells.iter().enumerate() {
            match cell {
                SessionCell::Message {
                    role,
                    content,
                    timestamp,
                } => match role {
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
                } => turns.agent.push(TextTurn {
                    index,
                    text: header
                        .as_ref()
                        .map(|header| format!("{header}\n{body}"))
                        .unwrap_or_else(|| body.clone()),
                    timestamp: timestamp.map(|timestamp| timestamp.to_rfc3339()),
                }),
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
                    output,
                    is_error,
                    timestamp,
                    ..
                } => turns.tool_results.push(ToolResultTurn {
                    index,
                    tool: tool.clone(),
                    text: output.clone(),
                    is_error: *is_error,
                    timestamp: timestamp.map(|timestamp| timestamp.to_rfc3339()),
                }),
                SessionCell::Exec {
                    command,
                    cwd,
                    stdout,
                    stderr,
                    exit_code,
                    timestamp,
                    ..
                } => turns.exec.push(ExecTurn {
                    index,
                    command: command.clone(),
                    cwd: cwd.clone(),
                    stdout: stdout.clone(),
                    stderr: stderr.clone(),
                    exit_code: *exit_code,
                    timestamp: timestamp.map(|timestamp| timestamp.to_rfc3339()),
                }),
                SessionCell::Patch {
                    files,
                    success,
                    timestamp,
                    ..
                } => turns.patches.push(PatchTurn {
                    index,
                    files: files.clone(),
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
    text: String,
    timestamp: Option<String>,
}

impl TextTurn {
    fn new(index: usize, text: &str, timestamp: &Option<chrono::DateTime<chrono::Utc>>) -> Self {
        Self {
            index,
            text: text.to_owned(),
            timestamp: timestamp.map(|timestamp| timestamp.to_rfc3339()),
        }
    }
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
    text: String,
    is_error: bool,
    timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecTurn {
    index: usize,
    command: Vec<String>,
    cwd: Option<String>,
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PatchTurn {
    index: usize,
    files: Vec<PatchFile>,
    success: bool,
    timestamp: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{JsRuleEngine, RuleInput, RuleSession, RuleTurns};

    fn input(agent: &'static str, user_text: &str) -> RuleInput {
        RuleInput {
            session: RuleSession {
                id: "s1".to_owned(),
                agent,
                project: "/tmp/project".to_owned(),
                cwd: Some("/tmp/project".to_owned()),
                branch: None,
                path: "/tmp/session.jsonl".to_owned(),
                modified_ts: 0,
                lines: 1,
                derivation_type: "original",
                is_sidechain: false,
                custom_title: None,
                model: Some("test-spark".to_owned()),
                model_provider: None,
                approval_policy: None,
                sandbox_mode: None,
                first_user_text: user_text.to_owned(),
                first_text: user_text.to_owned(),
                last_text: user_text.to_owned(),
                trashed: false,
            },
            turns: RuleTurns {
                user: vec![super::TextTurn {
                    index: 0,
                    text: user_text.to_owned(),
                    timestamp: None,
                }],
                ..RuleTurns::default()
            },
        }
    }

    #[test]
    fn js_rule_returns_trash_action() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("rules.js");
        std::fs::write(
            &rules,
            r#"
            rule("commit sessions", ({ turns, re }) => {
              return re(String.raw`\s*[/$](gdf-)?commit\b`, "m").test(turns.user[0].text)
                ? trash("commit helper")
                : nothing();
            });
            "#,
        )
        .unwrap();

        let engine = JsRuleEngine::load(&rules).unwrap();
        let outcomes = engine
            .evaluate(&input("codex", "$gdf-commit --all"))
            .unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].rule, "commit sessions");
        assert_eq!(outcomes[0].action.as_deref(), Some("trash"));
        assert_eq!(outcomes[0].reason.as_deref(), Some("commit helper"));
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
