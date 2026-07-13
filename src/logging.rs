//! Logging configuration, per-process log ownership, and startup retention.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use env_filter::{Builder as EnvFilterBuilder, Filter as EnvFilter};
use log::LevelFilter;
use log4rs::append::console::{ConsoleAppender, Target};
use log4rs::append::rolling_file::policy::compound::roll::fixed_window::FixedWindowRoller;
use log4rs::append::rolling_file::policy::compound::trigger::size::SizeTrigger;
use log4rs::append::rolling_file::policy::compound::CompoundPolicy;
use log4rs::append::rolling_file::RollingFileAppender;
use log4rs::config::{Appender, Config, Deserializers, Logger, Root};
use log4rs::encode::pattern::PatternEncoder;
use log4rs::filter::{Filter, Response};

use crate::settings::config_dir;

pub const SUMMARY_ERROR_TARGET: &str = "aics::summary::errors";
pub const MAX_RETAINED_PROCESS_LOG_GROUPS: usize = 10;
pub const MAIN_LOG_LIMIT_BYTES: u64 = 2 * 1024 * 1024;
pub const MAIN_LOG_ARCHIVES: u32 = 2;
pub const SUMMARY_LOG_LIMIT_BYTES: u64 = 1024 * 1024;
pub const SUMMARY_LOG_ARCHIVES: u32 = 1;

const LOG_PATTERN: &str = "{d} {l:<5} [{T}] {t} - {m}{n}";
const MAIN_APPENDER: &str = "aics";
const SUMMARY_APPENDER: &str = "summary_errors";
const CUSTOM_CONFIG_NAME: &str = "log4rs.yaml";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggingMode {
    Interactive,
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessLogPaths {
    pub directory: PathBuf,
    pub instance: String,
    pub main: PathBuf,
    pub summary_errors: PathBuf,
}

impl ProcessLogPaths {
    pub fn new(config_root: &Path, instance: String) -> Self {
        let directory = config_root.join("logs");
        let main = directory.join(format!("aics-{instance}.log"));
        let summary_errors = directory.join(format!("summarizer-errors-{instance}.log"));
        Self {
            directory,
            instance,
            main,
            summary_errors,
        }
    }
}

pub struct LoggingHandle {
    pub handle: log4rs::Handle,
    pub paths: Option<ProcessLogPaths>,
    /// A startup warning to also show in the TUI statusline.
    pub bootstrap_warning: Option<String>,
}

#[derive(Debug)]
struct EnvLogFilter(EnvFilter);

impl Filter for EnvLogFilter {
    fn filter(&self, record: &log::Record<'_>) -> Response {
        if self.0.matches(record) {
            Response::Neutral
        } else {
            Response::Reject
        }
    }
}

struct ParsedFilter {
    filter: EnvFilter,
    max_level: LevelFilter,
    warning: Option<String>,
}

pub fn init(mode: LoggingMode) -> Result<LoggingHandle> {
    let root = config_dir().context("failed to locate AICS config directory for logging")?;
    init_in(mode, &root)
}

fn init_in(mode: LoggingMode, config_root: &Path) -> Result<LoggingHandle> {
    let config_root = absolute_path(config_root)?;
    let mut warnings = Vec::new();
    if let Err(error) = fs::create_dir_all(&config_root) {
        warnings.push(format!(
            "could not create AICS config directory {}: {error}",
            config_root.display()
        ));
    }
    let instance = process_instance(Utc::now(), std::process::id());
    let paths = ProcessLogPaths::new(&config_root, instance);
    let log_dir = paths.directory.clone();

    env::set_var("AICS_LOG_DIR", &paths.directory);
    env::set_var("AICS_LOG_INSTANCE", &paths.instance);

    let custom_path = config_root.join(CUSTOM_CONFIG_NAME);
    let (config, managed_paths) = if custom_path.exists() {
        match log4rs::config::load_config_file(&custom_path, Deserializers::default()) {
            Ok(config) => (config, None),
            Err(error) => {
                warnings.push(format!(
                    "could not load {}: {error:#}; using built-in logging",
                    custom_path.display()
                ));
                let (config, paths, warning) = built_in_config(mode, paths)?;
                warnings.extend(warning);
                (config, paths)
            }
        }
    } else {
        let (config, paths, warning) = built_in_config(mode, paths)?;
        warnings.extend(warning);
        (config, paths)
    };

    for warning in &warnings {
        eprintln!("aics: logging warning: {warning}");
    }

    let err_handler: Box<dyn Send + Sync + Fn(&anyhow::Error)> = match mode {
        LoggingMode::Command => Box::new(|error| eprintln!("aics: logging error: {error:#}")),
        LoggingMode::Interactive => Box::new(|_| {}),
    };
    let handle = log4rs::config::init_config_with_err_handler(config, err_handler)
        .context("failed to install log4rs global logger")?;

    if let Err(error) = reap_stopped_process_logs(&log_dir, &SystemProcessLiveness) {
        log::warn!("failed to reap stopped-process logs: {error:#}");
    }

    Ok(LoggingHandle {
        handle,
        paths: managed_paths,
        bootstrap_warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
    })
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

fn built_in_config(
    mode: LoggingMode,
    paths: ProcessLogPaths,
) -> Result<(Config, Option<ProcessLogPaths>, Vec<String>)> {
    let parsed = parse_env_filter(env::var("RUST_LOG").ok().as_deref());
    let mut warnings = parsed.warning.into_iter().collect::<Vec<_>>();

    match mode {
        LoggingMode::Command => {
            let appender = filtered_appender(console_appender(), parsed.filter);
            let config = Config::builder()
                .appender(appender)
                .build(
                    Root::builder()
                        .appender(MAIN_APPENDER)
                        .build(parsed.max_level),
                )
                .context("failed to build command logging configuration")?;
            Ok((config, None, warnings))
        }
        LoggingMode::Interactive => {
            let (main, main_is_managed) =
                match rolling_appender(&paths.main, MAIN_LOG_LIMIT_BYTES, MAIN_LOG_ARCHIVES) {
                    Ok(appender) => (appender, true),
                    Err(error) => {
                        warnings.push(format!(
                            "could not open interactive log {}: {error:#}; using stderr",
                            paths.main.display()
                        ));
                        (console_appender(), false)
                    }
                };
            let mut builder = Config::builder().appender(filtered_appender(main, parsed.filter));

            match rolling_appender(
                &paths.summary_errors,
                SUMMARY_LOG_LIMIT_BYTES,
                SUMMARY_LOG_ARCHIVES,
            ) {
                Ok(summary) => {
                    builder = builder
                        .appender(Appender::builder().build(SUMMARY_APPENDER, summary))
                        .logger(
                            Logger::builder()
                                .appender(SUMMARY_APPENDER)
                                .additive(true)
                                .build(SUMMARY_ERROR_TARGET, LevelFilter::Warn),
                        );
                }
                Err(error) => warnings.push(format!(
                    "could not open summary error log {}: {error:#}; summary errors will use the main route only",
                    paths.summary_errors.display()
                )),
            }

            let root_level = std::cmp::max(parsed.max_level, LevelFilter::Warn);
            let config = builder
                .build(Root::builder().appender(MAIN_APPENDER).build(root_level))
                .context("failed to build interactive logging configuration")?;
            Ok((config, main_is_managed.then_some(paths), warnings))
        }
    }
}

fn parse_env_filter(value: Option<&str>) -> ParsedFilter {
    let value = value.unwrap_or("warn");
    let mut builder = EnvFilterBuilder::new();
    let warning = match builder.try_parse(value) {
        Ok(_) => None,
        Err(error) => {
            builder = EnvFilterBuilder::new();
            builder.filter_level(LevelFilter::Warn);
            Some(format!(
                "invalid RUST_LOG value {value:?}: {error}; using warn"
            ))
        }
    };
    let filter = builder.build();
    let max_level = filter.filter();
    ParsedFilter {
        filter,
        max_level,
        warning,
    }
}

fn console_appender() -> Box<dyn log4rs::append::Append> {
    Box::new(
        ConsoleAppender::builder()
            .target(Target::Stderr)
            .encoder(Box::new(PatternEncoder::new(LOG_PATTERN)))
            .build(),
    )
}

fn rolling_appender(
    path: &Path,
    limit: u64,
    archives: u32,
) -> Result<Box<dyn log4rs::append::Append>> {
    let archive_pattern = format!("{}.{{}}", path.display());
    let roller = FixedWindowRoller::builder()
        .base(1)
        .build(&archive_pattern, archives)
        .with_context(|| format!("failed to build roller for {}", path.display()))?;
    let policy = CompoundPolicy::new(Box::new(SizeTrigger::new(limit)), Box::new(roller));
    let appender = RollingFileAppender::builder()
        .encoder(Box::new(PatternEncoder::new(LOG_PATTERN)))
        .build(path, Box::new(policy))
        .with_context(|| format!("failed to open rolling log {}", path.display()))?;
    Ok(Box::new(appender))
}

fn filtered_appender(appender: Box<dyn log4rs::append::Append>, filter: EnvFilter) -> Appender {
    Appender::builder()
        .filter(Box::new(EnvLogFilter(filter)))
        .build(MAIN_APPENDER, appender)
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
    fn state(&self, pid: u32) -> ProcessState;
}

struct SystemProcessLiveness;

impl ProcessLiveness for SystemProcessLiveness {
    fn state(&self, pid: u32) -> ProcessState {
        process_state(pid)
    }
}

#[cfg(unix)]
fn process_state(pid: u32) -> ProcessState {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return ProcessState::Indeterminate;
    };
    // SAFETY: signal 0 performs only an existence/permission check.
    if unsafe { libc::kill(pid, 0) } == 0 {
        ProcessState::Live
    } else {
        match io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => ProcessState::Dead,
            _ => ProcessState::Indeterminate,
        }
    }
}

#[cfg(windows)]
fn process_state(pid: u32) -> ProcessState {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_INVALID_PARAMETER, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: the returned handle is checked and closed before returning.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        // SAFETY: immediately reads the calling thread's last-error value.
        return if unsafe { GetLastError() } == ERROR_INVALID_PARAMETER {
            ProcessState::Dead
        } else {
            ProcessState::Indeterminate
        };
    }
    let mut exit_code = 0;
    // SAFETY: handle is valid and exit_code points to writable storage.
    let result = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
    // SAFETY: handle was returned by OpenProcess and has not yet been closed.
    unsafe { CloseHandle(handle) };
    if result == 0 {
        ProcessState::Indeterminate
    } else if exit_code == STILL_ACTIVE {
        ProcessState::Live
    } else {
        ProcessState::Dead
    }
}

#[cfg(not(any(unix, windows)))]
fn process_state(_pid: u32) -> ProcessState {
    ProcessState::Indeterminate
}

#[derive(Debug)]
struct ManagedFile {
    instance: String,
    pid: u32,
}

fn parse_managed_file(name: &str) -> Option<ManagedFile> {
    let rest = name
        .strip_prefix("aics-")
        .or_else(|| name.strip_prefix("summarizer-errors-"))?;
    let instance = rest.strip_suffix(".log").or_else(|| {
        let (base, archive) = rest.rsplit_once(".log.")?;
        (!archive.is_empty() && archive.bytes().all(|byte| byte.is_ascii_digit())).then_some(base)
    })?;
    let (stamp, pid) = instance.rsplit_once("-p")?;
    if stamp.len() != 20
        || chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%S%.3fZ").is_err()
    {
        return None;
    }
    let pid = pid.parse::<u32>().ok().filter(|pid| *pid != 0)?;
    Some(ManagedFile {
        instance: instance.to_owned(),
        pid,
    })
}

#[derive(Debug)]
struct LogGroup {
    pid: u32,
    files: Vec<PathBuf>,
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
                log::warn!(
                    "failed to inspect an entry in {}: {error}",
                    log_dir.display()
                );
                continue;
            }
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                log::warn!(
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
            files: Vec::new(),
        });
        group.files.push(entry.path());
    }

    let groups = groups
        .into_iter()
        .map(|(instance, group)| {
            let state = liveness.state(group.pid);
            if state == ProcessState::Indeterminate {
                log::warn!(
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
                    log::warn!(
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
    use std::sync::Mutex;

    use chrono::TimeZone;
    use tempfile::TempDir;

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct FakeLiveness(HashMap<u32, ProcessState>);

    impl ProcessLiveness for FakeLiveness {
        fn state(&self, pid: u32) -> ProcessState {
            self.0
                .get(&pid)
                .copied()
                .unwrap_or(ProcessState::Indeterminate)
        }
    }

    struct RemoveDuringCheck {
        path: PathBuf,
        removed: std::sync::Once,
    }

    impl ProcessLiveness for RemoveDuringCheck {
        fn state(&self, _pid: u32) -> ProcessState {
            self.removed.call_once(|| {
                fs::remove_file(&self.path).unwrap();
            });
            ProcessState::Dead
        }
    }

    fn matches(
        filter: &EnvFilter,
        level: log::Level,
        target: &'static str,
        message: &'static str,
    ) -> bool {
        filter.matches(
            &log::Record::builder()
                .level(level)
                .target(target)
                .args(format_args!("{message}"))
                .build(),
        )
    }

    #[test]
    fn default_filter_is_warn() {
        let parsed = parse_env_filter(None);
        assert_eq!(parsed.max_level, LevelFilter::Warn);
        assert!(matches(&parsed.filter, log::Level::Warn, "aics", "warning"));
        assert!(!matches(&parsed.filter, log::Level::Info, "aics", "info"));
    }

    #[test]
    fn env_filter_preserves_global_module_and_message_directives() {
        let global = parse_env_filter(Some("debug"));
        assert!(matches(&global.filter, log::Level::Debug, "dep", "message"));

        let modules = parse_env_filter(Some("aics=debug,tantivy=warn"));
        assert!(matches(
            &modules.filter,
            log::Level::Debug,
            "aics::tui",
            "message"
        ));
        assert!(!matches(
            &modules.filter,
            log::Level::Info,
            "tantivy",
            "message"
        ));

        let message = parse_env_filter(Some("debug/needle"));
        assert!(matches(
            &message.filter,
            log::Level::Debug,
            "aics",
            "needle here"
        ));
        assert!(!matches(
            &message.filter,
            log::Level::Debug,
            "aics",
            "other"
        ));
    }

    #[test]
    fn invalid_env_filter_falls_back_to_warn() {
        let parsed = parse_env_filter(Some("aics=invalid"));
        assert_eq!(parsed.max_level, LevelFilter::Warn);
        assert!(parsed.warning.is_some());
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
    fn managed_active_and_archive_names_round_trip() {
        let id = "20260713T142530.417Z-p12345";
        for name in [
            format!("aics-{id}.log"),
            format!("aics-{id}.log.2"),
            format!("summarizer-errors-{id}.log"),
            format!("summarizer-errors-{id}.log.1"),
        ] {
            let parsed = parse_managed_file(&name).unwrap();
            assert_eq!(parsed.instance, id);
            assert_eq!(parsed.pid, 12345);
        }
        assert!(parse_managed_file(&format!("other-{id}.log")).is_none());
        assert!(parse_managed_file(&format!("aics-{id}.log.old")).is_none());
    }

    #[test]
    fn explicit_config_root_derives_all_paths() {
        let root = Path::new("/tmp/aics-test-root");
        let paths = ProcessLogPaths::new(root, "instance".to_owned());
        assert_eq!(paths.main, root.join("logs/aics-instance.log"));
        assert_eq!(
            paths.summary_errors,
            root.join("logs/summarizer-errors-instance.log")
        );
    }

    #[test]
    fn checked_in_yaml_template_parses_with_reserved_variables() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new().unwrap();
        let log_dir = temp.path().join("logs");
        let old_dir = env::var_os("AICS_LOG_DIR");
        let old_instance = env::var_os("AICS_LOG_INSTANCE");
        env::set_var("AICS_LOG_DIR", &log_dir);
        env::set_var("AICS_LOG_INSTANCE", "20260713T142530.417Z-p12345");

        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/log4rs.yaml");
        let config = log4rs::config::load_config_file(path, Deserializers::default()).unwrap();
        assert_eq!(config.root().level(), LevelFilter::Warn);
        assert_eq!(config.root().appenders(), &[MAIN_APPENDER.to_owned()]);
        assert_eq!(config.appenders().len(), 2);
        assert!(config.loggers().iter().any(|logger| {
            logger.name() == SUMMARY_ERROR_TARGET
                && logger.level() == LevelFilter::Warn
                && logger.additive()
        }));
        assert!(log_dir
            .join("aics-20260713T142530.417Z-p12345.log")
            .exists());

        match old_dir {
            Some(value) => env::set_var("AICS_LOG_DIR", value),
            None => env::remove_var("AICS_LOG_DIR"),
        }
        match old_instance {
            Some(value) => env::set_var("AICS_LOG_INSTANCE", value),
            None => env::remove_var("AICS_LOG_INSTANCE"),
        }
    }

    #[test]
    fn interactive_summary_route_remains_warn_when_rust_log_is_off() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new().unwrap();
        let old_rust_log = env::var_os("RUST_LOG");
        env::set_var("RUST_LOG", "off");
        let paths = ProcessLogPaths::new(temp.path(), "20260713T142530.417Z-p12345".to_owned());

        let (config, managed, warnings) = built_in_config(LoggingMode::Interactive, paths).unwrap();
        assert!(warnings.is_empty());
        assert!(managed.is_some());
        assert_eq!(config.root().level(), LevelFilter::Warn);
        let summary = config
            .loggers()
            .iter()
            .find(|logger| logger.name() == SUMMARY_ERROR_TARGET)
            .unwrap();
        assert_eq!(summary.level(), LevelFilter::Warn);
        assert_eq!(summary.appenders(), &[SUMMARY_APPENDER.to_owned()]);
        assert!(summary.additive());
        let main = config
            .appenders()
            .iter()
            .find(|appender| appender.name() == MAIN_APPENDER)
            .unwrap();
        let summary_record = log::Record::builder()
            .level(log::Level::Warn)
            .target(SUMMARY_ERROR_TARGET)
            .args(format_args!("summary failed"))
            .build();
        assert_eq!(main.filters()[0].filter(&summary_record), Response::Reject);

        match old_rust_log {
            Some(value) => env::set_var("RUST_LOG", value),
            None => env::remove_var("RUST_LOG"),
        }
    }

    #[test]
    fn fixed_window_rotation_keeps_exact_archive_count() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("rotation.log");
        let appender = rolling_appender(&path, 128, 2).unwrap();
        let message = "x".repeat(256);
        for _ in 0..6 {
            appender
                .append(
                    &log::Record::builder()
                        .level(log::Level::Warn)
                        .target("aics")
                        .args(format_args!("{message}"))
                        .build(),
                )
                .unwrap();
        }
        appender
            .append(
                &log::Record::builder()
                    .level(log::Level::Warn)
                    .target("aics")
                    .args(format_args!("final"))
                    .build(),
            )
            .unwrap();

        assert!(path.exists());
        assert!(temp.path().join("rotation.log.1").exists());
        assert!(temp.path().join("rotation.log.2").exists());
        assert!(!temp.path().join("rotation.log.3").exists());
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
    fn reused_live_pid_protects_every_timestamped_group() {
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
}
