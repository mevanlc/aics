//! Logging configuration, per-process log ownership, and startup retention.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{NaiveDate, Utc};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tracing::{Level, Subscriber};
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::filter::{filter_fn, LevelFilter};
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter, Layer, Registry};

use crate::settings::config_dir;

pub const SUMMARY_ERROR_TARGET: &str = "aics::summary::errors";
pub const MAX_RETAINED_PROCESS_LOG_GROUPS: usize = 10;
pub const MAIN_LOG_FILES: usize = 3;
pub const SUMMARY_LOG_FILES: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggingMode {
    Interactive,
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessLogPaths {
    pub directory: PathBuf,
    pub instance: String,
}

impl ProcessLogPaths {
    pub fn new(config_root: &Path, instance: String) -> Self {
        Self {
            directory: config_root.join("logs"),
            instance,
        }
    }

    pub fn main_for_date(&self, date: NaiveDate) -> PathBuf {
        self.directory
            .join(format!("{}.{}", self.main_prefix(), date))
    }

    pub fn summary_errors_for_date(&self, date: NaiveDate) -> PathBuf {
        self.directory
            .join(format!("{}.{}", self.summary_prefix(), date))
    }

    fn main_prefix(&self) -> String {
        format!("aics-{}.log", self.instance)
    }

    fn summary_prefix(&self) -> String {
        format!("summarizer-errors-{}.log", self.instance)
    }
}

/// Keep alive until application teardown to drain buffered file diagnostics.
#[must_use = "dropping the logging handle shuts down the file writers"]
pub struct LoggingHandle {
    _guards: Vec<WorkerGuard>,
    /// Managed paths when at least one interactive file sink opened successfully.
    pub paths: Option<ProcessLogPaths>,
    /// A startup warning to also show in the TUI statusline.
    pub bootstrap_warning: Option<String>,
}

struct ParsedFilter {
    filter: EnvFilter,
    warning: Option<String>,
}

pub fn init(mode: LoggingMode) -> Result<LoggingHandle> {
    let root = config_dir().context("failed to locate AICS config directory for logging")?;
    let config_root = absolute_path(&root)?;
    let paths = ProcessLogPaths::new(
        &config_root,
        process_instance(Utc::now(), std::process::id()),
    );
    let log_dir = paths.directory.clone();
    let mut warnings = Vec::new();
    if let Err(error) = fs::create_dir_all(&config_root) {
        warnings.push(format!(
            "could not create AICS config directory {}: {error}",
            config_root.display()
        ));
    }
    let (subscriber, mut handle) =
        build_subscriber(mode, paths, env::var("RUST_LOG").ok().as_deref());
    warnings.extend(handle.bootstrap_warning.take());
    for warning in &warnings {
        eprintln!("aics: logging warning: {warning}");
    }
    handle.bootstrap_warning = (!warnings.is_empty()).then(|| warnings.join("; "));
    subscriber
        .try_init()
        .context("failed to install tracing global subscriber and log bridge")?;

    if let Err(error) = reap_stopped_process_logs(&log_dir, &SystemProcessLiveness) {
        tracing::warn!("failed to reap stopped-process logs: {error:#}");
    }
    Ok(handle)
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(env::current_dir()
            .context("failed to resolve current directory for logging")?
            .join(path))
    }
}

// Separate construction from global installation so tests can use scoped subscribers
// and explicit paths/filters without mutating the environment or real config files.
fn build_subscriber(
    mode: LoggingMode,
    paths: ProcessLogPaths,
    rust_log: Option<&str>,
) -> (impl Subscriber + Send + Sync, LoggingHandle) {
    let parsed = parse_env_filter(rust_log);
    let mut warnings = parsed.warning.into_iter().collect::<Vec<_>>();
    let mut guards = Vec::new();
    let mut managed = false;
    let main = if mode == LoggingMode::Interactive {
        match rolling_appender(&paths.directory, &paths.main_prefix(), MAIN_LOG_FILES) {
            Ok(appender) => {
                let (writer, guard) = NonBlockingBuilder::default()
                    .thread_name("aics-log")
                    .lossy(true)
                    .finish(appender);
                guards.push(guard);
                managed = true;
                BoxMakeWriter::new(writer)
            }
            Err(error) => {
                warnings.push(format!(
                    "could not open interactive log in {}: {error:#}; using stderr",
                    paths.directory.display()
                ));
                BoxMakeWriter::new(io::stderr)
            }
        }
    } else {
        BoxMakeWriter::new(io::stderr)
    };
    let mut layers = vec![text_layer(main, mode).with_filter(parsed.filter).boxed()];

    if mode == LoggingMode::Interactive {
        match rolling_appender(&paths.directory, &paths.summary_prefix(), SUMMARY_LOG_FILES) {
            Ok(appender) => {
                let (writer, guard) = NonBlockingBuilder::default()
                    .thread_name("aics-summary-log")
                    .lossy(false)
                    .finish(appender);
                guards.push(guard);
                managed = true;
                layers.push(
                    text_layer(writer, mode)
                        .with_filter(
                            filter_fn(|metadata| {
                                metadata.target() == SUMMARY_ERROR_TARGET
                                    && *metadata.level() <= Level::WARN
                            })
                            .with_max_level_hint(LevelFilter::WARN),
                        )
                        .boxed(),
                );
            }
            Err(error) => warnings.push(format!(
                "could not open summary error log in {}: {error:#}; summary errors will use the main route only",
                paths.directory.display()
            )),
        }
    }

    (
        Registry::default().with(layers),
        LoggingHandle {
            _guards: guards,
            paths: managed.then_some(paths),
            bootstrap_warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
        },
    )
}

fn text_layer<W>(writer: W, mode: LoggingMode) -> impl Layer<Registry> + Send + Sync
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_thread_names(true)
        .with_thread_ids(true)
        .log_internal_errors(mode == LoggingMode::Command)
}

fn parse_env_filter(value: Option<&str>) -> ParsedFilter {
    let value = value
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("warn");
    match EnvFilter::builder()
        .with_default_directive(LevelFilter::WARN.into())
        .parse(value)
    {
        Ok(filter) => ParsedFilter {
            filter,
            warning: None,
        },
        Err(error) => ParsedFilter {
            filter: EnvFilter::new("warn"),
            warning: Some(format!(
                "invalid RUST_LOG value {value:?}: {error}; using warn"
            )),
        },
    }
}

fn rolling_appender(directory: &Path, prefix: &str, files: usize) -> Result<RollingFileAppender> {
    // The upstream retention scan happens before opening the first file.
    fs::create_dir_all(directory)
        .with_context(|| format!("failed to create log directory {}", directory.display()))?;
    RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        // Include `.log` to delimit the PID: p123 must not match p1234.
        .filename_prefix(prefix)
        .max_log_files(files)
        .build(directory)
        .with_context(|| {
            format!(
                "failed to open rolling log {prefix} in {}",
                directory.display()
            )
        })
}

pub fn process_instance(started: chrono::DateTime<Utc>, pid: u32) -> String {
    format!("{}-p{pid}", started.format("%Y%m%dT%H%M%S%.3fZ"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessState {
    Live,
    Dead,
    Indeterminate,
}

trait ProcessLiveness {
    fn observe(&self, pids: &[u32]) -> BTreeMap<u32, ProcessObservation>;
}

struct SystemProcessLiveness;

impl ProcessLiveness for SystemProcessLiveness {
    fn observe(&self, pids: &[u32]) -> BTreeMap<u32, ProcessObservation> {
        if pids.is_empty() {
            return BTreeMap::new();
        }
        let indeterminate = || {
            pids.iter()
                .copied()
                .map(|pid| (pid, ProcessObservation::indeterminate()))
                .collect()
        };
        if !sysinfo::IS_SUPPORTED_SYSTEM {
            return indeterminate();
        }

        let mut system = System::new();
        let Ok(current_pid) = sysinfo::get_current_pid() else {
            return indeterminate();
        };
        let mut system_pids = pids.iter().copied().map(Pid::from_u32).collect::<Vec<_>>();
        system_pids.push(current_pid);
        system_pids.sort_unstable();
        system_pids.dedup();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&system_pids),
            true,
            ProcessRefreshKind::nothing().without_tasks(),
        );

        // A targeted refresh has no error result. Seeing our own process proves the
        // platform snapshot succeeded; otherwise absence cannot safely mean death.
        if system.process(current_pid).is_none() {
            return indeterminate();
        }

        pids.iter()
            .copied()
            .map(|pid| {
                let observation = match system.process(Pid::from_u32(pid)) {
                    Some(process) => ProcessObservation::live(process.start_time()),
                    None => ProcessObservation::dead(),
                };
                (pid, observation)
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessObservation {
    state: ProcessState,
    started_at_epoch_seconds: Option<u64>,
}

impl ProcessObservation {
    fn live(started_at_epoch_seconds: u64) -> Self {
        Self {
            state: ProcessState::Live,
            started_at_epoch_seconds: (started_at_epoch_seconds != 0)
                .then_some(started_at_epoch_seconds),
        }
    }

    const fn dead() -> Self {
        Self {
            state: ProcessState::Dead,
            started_at_epoch_seconds: None,
        }
    }

    const fn indeterminate() -> Self {
        Self {
            state: ProcessState::Indeterminate,
            started_at_epoch_seconds: None,
        }
    }
}

#[derive(Debug)]
struct ManagedFile {
    instance: String,
    pid: u32,
    started: chrono::DateTime<Utc>,
}

fn parse_managed_file(name: &str) -> Option<ManagedFile> {
    let rest = name
        .strip_prefix("aics-")
        .or_else(|| name.strip_prefix("summarizer-errors-"))?;
    let instance = rest.strip_suffix(".log").or_else(|| {
        let (base, archive) = rest.rsplit_once(".log.")?;
        let numbered = !archive.is_empty() && archive.bytes().all(|byte| byte.is_ascii_digit());
        let daily = archive.len() == 10 && NaiveDate::parse_from_str(archive, "%Y-%m-%d").is_ok();
        (numbered || daily).then_some(base)
    })?;
    let (stamp, pid) = instance.rsplit_once("-p")?;
    if stamp.len() != 20 {
        return None;
    }
    let started = chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%S%.3fZ")
        .ok()?
        .and_utc();
    let pid = pid.parse::<u32>().ok().filter(|pid| *pid != 0)?;
    Some(ManagedFile {
        instance: instance.to_owned(),
        pid,
        started,
    })
}

#[derive(Debug)]
struct LogGroup {
    pid: u32,
    started: chrono::DateTime<Utc>,
    files: Vec<PathBuf>,
}

fn group_process_state(group: &LogGroup, observation: ProcessObservation) -> ProcessState {
    if observation.state == ProcessState::Live
        && observation
            .started_at_epoch_seconds
            .zip(u64::try_from(group.started.timestamp()).ok())
            .is_some_and(|(process_started, group_started)| process_started > group_started)
    {
        ProcessState::Dead
    } else {
        observation.state
    }
}

fn reap_stopped_process_logs(log_dir: &Path, liveness: &dyn ProcessLiveness) -> Result<usize> {
    let entries = match fs::read_dir(log_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to scan {}", log_dir.display()))
        }
    };
    let mut groups = BTreeMap::<String, LogGroup>::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(
                    "failed to inspect an entry in {}: {error}",
                    log_dir.display()
                );
                continue;
            }
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                tracing::warn!(
                    "failed to inspect log entry type for {}: {error}",
                    entry.path().display()
                );
                continue;
            }
        };
        if !file_type.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(managed) = parse_managed_file(&name) else {
            continue;
        };
        let group = groups.entry(managed.instance).or_insert_with(|| LogGroup {
            pid: managed.pid,
            started: managed.started,
            files: Vec::new(),
        });
        group.files.push(entry.path());
    }

    let mut pids = groups.values().map(|group| group.pid).collect::<Vec<_>>();
    pids.sort_unstable();
    pids.dedup();
    let observations = liveness.observe(&pids);
    let groups = groups
        .into_iter()
        .map(|(instance, group)| {
            let observation = observations
                .get(&group.pid)
                .copied()
                .unwrap_or_else(ProcessObservation::indeterminate);
            let state = group_process_state(&group, observation);
            if state == ProcessState::Indeterminate {
                tracing::warn!(
                    "could not determine whether pid {} is live; retaining log group {}",
                    group.pid,
                    instance
                );
            }
            (instance, group, state)
        })
        .collect::<Vec<_>>();
    let live_count = groups
        .iter()
        .filter(|(_, _, state)| *state != ProcessState::Dead)
        .count();
    let target_count = MAX_RETAINED_PROCESS_LOG_GROUPS.max(live_count);
    let delete_count = groups.len().saturating_sub(target_count);
    let dead = groups
        .into_iter()
        .filter(|(_, _, state)| *state == ProcessState::Dead)
        .take(delete_count);

    let mut removed = 0;
    for (_instance, group, _state) in dead {
        let mut group_removed = true;
        for path in group.files {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    group_removed = false;
                    tracing::warn!(
                        "failed to remove stopped-process log {}: {error}",
                        path.display()
                    );
                }
            }
        }
        if group_removed {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use chrono::TimeZone;
    use tempfile::TempDir;

    use super::*;

    struct FakeLiveness(HashMap<u32, ProcessState>);

    impl ProcessLiveness for FakeLiveness {
        fn observe(&self, pids: &[u32]) -> BTreeMap<u32, ProcessObservation> {
            pids.iter()
                .copied()
                .map(|pid| {
                    let observation = match self
                        .0
                        .get(&pid)
                        .copied()
                        .unwrap_or(ProcessState::Indeterminate)
                    {
                        ProcessState::Live => ProcessObservation::live(0),
                        ProcessState::Dead => ProcessObservation::dead(),
                        ProcessState::Indeterminate => ProcessObservation::indeterminate(),
                    };
                    (pid, observation)
                })
                .collect()
        }
    }

    struct RemoveDuringCheck {
        path: PathBuf,
        removed: std::sync::Once,
    }

    impl ProcessLiveness for RemoveDuringCheck {
        fn observe(&self, pids: &[u32]) -> BTreeMap<u32, ProcessObservation> {
            self.removed.call_once(|| {
                fs::remove_file(&self.path).unwrap();
            });
            pids.iter()
                .copied()
                .map(|pid| (pid, ProcessObservation::dead()))
                .collect()
        }
    }

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn filtered_output(value: Option<&str>) -> String {
        let output = Capture::default();
        let writer = output.clone();
        let subscriber = Registry::default().with(
            text_layer(move || writer.clone(), LoggingMode::Command)
                .with_filter(parse_env_filter(value).filter),
        );
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(target: "aics::tui", "aics-debug");
            tracing::info!(target: "tantivy", "dependency-info");
            tracing::warn!(target: "tantivy", "dependency-warning");
        });
        let bytes = output.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn default_and_empty_filters_are_warn() {
        for value in [None, Some(""), Some(" ")] {
            let output = filtered_output(value);
            assert!(output.contains("dependency-warning"));
            assert!(!output.contains("dependency-info"));
            assert!(!output.contains("aics-debug"));
            assert!(!output.contains('\x1b'));
        }
    }

    #[test]
    fn env_filter_supports_global_module_and_off_directives() {
        let global = filtered_output(Some("debug"));
        assert!(global.contains("aics-debug"));
        assert!(global.contains("dependency-info"));
        let modules = filtered_output(Some("aics=debug,tantivy=warn"));
        assert!(modules.contains("aics-debug"));
        assert!(modules.contains("dependency-warning"));
        assert!(!modules.contains("dependency-info"));
        assert!(filtered_output(Some("off")).is_empty());
    }

    #[test]
    fn invalid_env_filter_falls_back_to_warn() {
        assert!(parse_env_filter(Some("aics=invalid")).warning.is_some());
        let output = filtered_output(Some("aics=invalid"));
        assert!(output.contains("dependency-warning"));
        assert!(!output.contains("dependency-info"));
        assert!(!output.contains("aics-debug"));
    }

    #[test]
    fn process_instance_is_sortable_safe_and_contains_pid() {
        let time = Utc.with_ymd_and_hms(2026, 7, 13, 14, 25, 30).unwrap()
            + chrono::Duration::milliseconds(417);
        let instance = process_instance(time, 12345);
        assert_eq!(instance, "20260713T142530.417Z-p12345");
        assert!(instance
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-".contains(&byte)));
    }

    #[test]
    fn system_liveness_observes_current_process_and_start_time() {
        if !sysinfo::IS_SUPPORTED_SYSTEM {
            return;
        }
        let pid = std::process::id();
        let observations = SystemProcessLiveness.observe(&[pid]);
        let observation = observations.get(&pid).unwrap();
        assert_eq!(observation.state, ProcessState::Live);
        assert!(observation.started_at_epoch_seconds.is_some());
    }

    #[test]
    fn managed_active_and_archive_names_round_trip() {
        let id = "20260713T142530.417Z-p12345";
        for name in [
            format!("aics-{id}.log"),
            format!("aics-{id}.log.2"),
            format!("aics-{id}.log.2026-07-13"),
            format!("summarizer-errors-{id}.log"),
            format!("summarizer-errors-{id}.log.1"),
            format!("summarizer-errors-{id}.log.2026-07-14"),
        ] {
            let parsed = parse_managed_file(&name).unwrap();
            assert_eq!(parsed.instance, id);
            assert_eq!(parsed.pid, 12345);
            assert_eq!(
                parsed.started,
                Utc.with_ymd_and_hms(2026, 7, 13, 14, 25, 30).unwrap()
                    + chrono::Duration::milliseconds(417)
            );
        }
        assert!(parse_managed_file(&format!("other-{id}.log")).is_none());
        assert!(parse_managed_file(&format!("aics-{id}.log.old")).is_none());
        assert!(parse_managed_file(&format!("aics-{id}.log.2026-02-30")).is_none());
    }

    #[test]
    fn explicit_config_root_derives_daily_paths() {
        let root = Path::new("/tmp/aics-test-root");
        let paths = ProcessLogPaths::new(root, "instance".to_owned());
        let date = NaiveDate::from_ymd_opt(2026, 7, 13).unwrap();
        assert_eq!(
            paths.main_for_date(date),
            root.join("logs/aics-instance.log.2026-07-13")
        );
        assert_eq!(
            paths.summary_errors_for_date(date),
            root.join("logs/summarizer-errors-instance.log.2026-07-13")
        );
    }

    #[test]
    fn interactive_summary_filter_is_independent_and_writers_drain_on_drop() {
        for filter in ["off", "warn"] {
            let temp = TempDir::new().unwrap();
            let paths = ProcessLogPaths::new(temp.path(), "20260713T142530.417Z-p12345".to_owned());
            let before = Utc::now().date_naive();
            let (subscriber, handle) =
                build_subscriber(LoggingMode::Interactive, paths.clone(), Some(filter));
            assert!(handle.bootstrap_warning.is_none());
            tracing::subscriber::with_default(subscriber, || {
                tracing::warn!("ordinary-warning");
                let error = anyhow::anyhow!("root cause").context("summary context");
                tracing::warn!(target: SUMMARY_ERROR_TARGET, session = %"session.jsonl", error = %format_args!("{error:#}"), "summary-failure");
                tracing::info!(target: SUMMARY_ERROR_TARGET, "summary-info");
                tracing::warn!(target: "aics::summary::errors::other", "other-target");
            });
            drop(handle);
            let read_route = |main| {
                [before, Utc::now().date_naive()]
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .filter_map(|date| {
                        fs::read_to_string(if main {
                            paths.main_for_date(date)
                        } else {
                            paths.summary_errors_for_date(date)
                        })
                        .ok()
                    })
                    .collect::<String>()
            };
            let summary = read_route(false);
            assert_eq!(summary.matches("summary-failure").count(), 1);
            assert!(summary.contains("session=session.jsonl"));
            assert!(summary.contains("error=summary context: root cause"));
            assert!(!summary.contains("ordinary-warning"));
            assert!(!summary.contains("summary-info"));
            assert!(!summary.contains("other-target"));
            let main = read_route(true);
            if filter == "off" {
                assert!(main.is_empty());
            } else {
                assert_eq!(main.matches("summary-failure").count(), 1);
                assert!(main.contains("ordinary-warning"));
            }
        }
    }

    #[test]
    fn daily_retention_limits_each_route_without_touching_another_pid() {
        let temp = TempDir::new().unwrap();
        let paths = ProcessLogPaths::new(temp.path(), "20260713T142530.417Z-p123".to_owned());
        let other = ProcessLogPaths::new(temp.path(), "20260713T142530.417Z-p1234".to_owned());
        fs::create_dir_all(&paths.directory).unwrap();
        for day in 1..=8 {
            let date = NaiveDate::from_ymd_opt(2026, 1, day).unwrap();
            fs::write(paths.main_for_date(date), "old main").unwrap();
            fs::write(paths.summary_errors_for_date(date), "old summary").unwrap();
            fs::write(other.main_for_date(date), "other main").unwrap();
            fs::write(other.summary_errors_for_date(date), "other summary").unwrap();
        }
        let (subscriber, handle) =
            build_subscriber(LoggingMode::Interactive, paths.clone(), Some("warn"));
        drop(subscriber);
        drop(handle);
        let count = |prefix: &str| {
            fs::read_dir(&paths.directory)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with(prefix))
                .count()
        };
        assert_eq!(count(&paths.main_prefix()), MAIN_LOG_FILES);
        assert_eq!(count(&paths.summary_prefix()), SUMMARY_LOG_FILES);
        assert_eq!(count(&other.main_prefix()), 8);
        assert_eq!(count(&other.summary_prefix()), 8);
    }

    #[test]
    fn remaining_file_sink_still_works_when_one_sink_cannot_open() {
        for block_main in [false, true] {
            let temp = TempDir::new().unwrap();
            let paths = ProcessLogPaths::new(temp.path(), "20260713T142530.417Z-p12345".to_owned());
            let date = Utc::now().date_naive();
            // A directory in place of a file makes opening that sink fail on every OS.
            let blocked = if block_main {
                paths.main_for_date(date)
            } else {
                paths.summary_errors_for_date(date)
            };
            fs::create_dir_all(blocked).unwrap();
            let (subscriber, handle) =
                build_subscriber(LoggingMode::Interactive, paths.clone(), Some("warn"));
            let warning = handle.bootstrap_warning.as_deref().unwrap();
            assert!(warning.contains(if block_main {
                "using stderr"
            } else {
                "main route only"
            }));
            assert!(handle.paths.is_some());
            tracing::subscriber::with_default(subscriber, || {
                tracing::warn!(target: SUMMARY_ERROR_TARGET, "surviving-sink");
            });
            drop(handle);
            let surviving = if block_main {
                paths.summary_errors_for_date(date)
            } else {
                paths.main_for_date(date)
            };
            assert!(fs::read_to_string(surviving)
                .unwrap()
                .contains("surviving-sink"));
        }
    }

    #[test]
    fn buffered_records_flush_on_early_error_and_unwinding() {
        for unwind in [false, true] {
            let temp = TempDir::new().unwrap();
            let paths = ProcessLogPaths::new(temp.path(), "20260713T142530.417Z-p12345".to_owned());
            let date = Utc::now().date_naive();
            let result = std::panic::catch_unwind(|| -> Result<()> {
                let (subscriber, _logging) =
                    build_subscriber(LoggingMode::Interactive, paths.clone(), Some("warn"));
                let _default = tracing::subscriber::set_default(subscriber);
                for ordinal in 0..100 {
                    tracing::warn!(target: SUMMARY_ERROR_TARGET, ordinal, "before-exit");
                }
                if unwind {
                    panic!("simulated application panic");
                }
                anyhow::bail!("simulated application error");
            });
            if unwind {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_err());
            }
            let summary = fs::read_to_string(paths.summary_errors_for_date(date)).unwrap();
            assert_eq!(summary.matches("before-exit").count(), 100);
            let main = fs::read_to_string(paths.main_for_date(date)).unwrap();
            assert_eq!(main.matches("before-exit").count(), 100);
        }
    }

    #[test]
    fn reaper_deletes_oldest_dead_groups_until_ten_remain() {
        let temp = TempDir::new().unwrap();
        let log_dir = temp.path().join("logs");
        fs::create_dir(&log_dir).unwrap();
        let mut states = HashMap::new();
        for day in 1..=12 {
            let pid = 1000 + day;
            let instance = format!("202607{day:02}T010203.004Z-p{pid}");
            fs::write(log_dir.join(format!("aics-{instance}.log")), "main").unwrap();
            fs::write(
                log_dir.join(format!("summarizer-errors-{instance}.log.1")),
                "summary",
            )
            .unwrap();
            states.insert(
                pid,
                if day <= 3 {
                    ProcessState::Dead
                } else {
                    ProcessState::Live
                },
            );
        }
        fs::write(log_dir.join("do-not-delete.txt"), "unknown").unwrap();

        let removed = reap_stopped_process_logs(&log_dir, &FakeLiveness(states)).unwrap();
        assert_eq!(removed, 2);
        assert!(!log_dir.join("aics-20260701T010203.004Z-p1001.log").exists());
        assert!(!log_dir.join("aics-20260702T010203.004Z-p1002.log").exists());
        assert!(log_dir.join("aics-20260703T010203.004Z-p1003.log").exists());
        assert!(log_dir.join("do-not-delete.txt").exists());
    }

    #[test]
    fn reaper_does_not_delete_when_fewer_than_ten_groups_exist() {
        let temp = TempDir::new().unwrap();
        let log_dir = temp.path().join("logs");
        fs::create_dir(&log_dir).unwrap();
        let mut states = HashMap::new();
        for day in 1..=3 {
            let pid = 3000 + day;
            let instance = format!("202604{day:02}T010203.004Z-p{pid}");
            fs::write(log_dir.join(format!("aics-{instance}.log")), "main").unwrap();
            states.insert(pid, ProcessState::Dead);
        }

        assert_eq!(
            reap_stopped_process_logs(&log_dir, &FakeLiveness(states)).unwrap(),
            0
        );
        assert_eq!(fs::read_dir(log_dir).unwrap().count(), 3);
    }

    #[test]
    fn reaper_accepts_not_found_from_a_concurrent_delete() {
        let temp = TempDir::new().unwrap();
        let log_dir = temp.path().join("logs");
        fs::create_dir(&log_dir).unwrap();
        let mut first_path = None;
        for day in 1..=11 {
            let instance = format!("202603{day:02}T010203.004Z-p{}", 4000 + day);
            let path = log_dir.join(format!("aics-{instance}.log"));
            fs::write(&path, "main").unwrap();
            first_path.get_or_insert(path);
        }
        let liveness = RemoveDuringCheck {
            path: first_path.unwrap(),
            removed: std::sync::Once::new(),
        };

        assert_eq!(reap_stopped_process_logs(&log_dir, &liveness).unwrap(), 1);
        assert_eq!(fs::read_dir(log_dir).unwrap().count(), 10);
    }

    #[test]
    fn reaper_keeps_more_than_ten_live_and_indeterminate_groups() {
        let temp = TempDir::new().unwrap();
        let log_dir = temp.path().join("logs");
        fs::create_dir(&log_dir).unwrap();
        let mut states = HashMap::new();
        for day in 1..=13 {
            let pid = 2000 + day;
            let instance = format!("202606{day:02}T010203.004Z-p{pid}");
            fs::write(log_dir.join(format!("aics-{instance}.log")), "main").unwrap();
            states.insert(
                pid,
                if day == 1 {
                    ProcessState::Dead
                } else if day == 2 {
                    ProcessState::Indeterminate
                } else {
                    ProcessState::Live
                },
            );
        }

        let removed = reap_stopped_process_logs(&log_dir, &FakeLiveness(states)).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(fs::read_dir(&log_dir).unwrap().count(), 12);
    }

    #[test]
    fn live_pid_without_start_time_protects_every_timestamped_group() {
        let temp = TempDir::new().unwrap();
        let log_dir = temp.path().join("logs");
        fs::create_dir(&log_dir).unwrap();
        for day in 1..=12 {
            let instance = format!("202605{day:02}T010203.004Z-p4242");
            fs::write(log_dir.join(format!("aics-{instance}.log")), "main").unwrap();
        }
        let liveness = FakeLiveness(HashMap::from([(4242, ProcessState::Live)]));

        assert_eq!(reap_stopped_process_logs(&log_dir, &liveness).unwrap(), 0);
        assert_eq!(fs::read_dir(log_dir).unwrap().count(), 12);
    }

    #[test]
    fn live_pid_started_after_log_group_is_reused_and_therefore_dead() {
        let group = LogGroup {
            pid: 4242,
            started: Utc.with_ymd_and_hms(2026, 5, 1, 1, 2, 3).unwrap(),
            files: Vec::new(),
        };
        let reused = Utc.with_ymd_and_hms(2026, 5, 2, 1, 2, 3).unwrap();
        assert_eq!(
            group_process_state(
                &group,
                ProcessObservation::live(reused.timestamp().try_into().unwrap()),
            ),
            ProcessState::Dead
        );
    }

    #[test]
    fn live_pid_started_before_log_group_stays_live() {
        let group = LogGroup {
            pid: 4242,
            started: Utc.with_ymd_and_hms(2026, 5, 2, 1, 2, 3).unwrap(),
            files: Vec::new(),
        };
        let same_process = Utc.with_ymd_and_hms(2026, 5, 1, 1, 2, 3).unwrap();
        assert_eq!(
            group_process_state(
                &group,
                ProcessObservation::live(same_process.timestamp().try_into().unwrap()),
            ),
            ProcessState::Live
        );
    }
}
