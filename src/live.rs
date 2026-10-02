use std::collections::HashMap;
use std::env;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use directories::BaseDirs;
use serde_json::Value;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use crate::parse::Agent;

/// Runtime markers belong to agent homes, which can differ from indexed roots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveSessionPaths {
    pub claude_sessions_dir: Option<PathBuf>,
    pub codex_writer_locks_dir: Option<PathBuf>,
    pub antigravity_presence_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct LiveSessionTracker {
    paths: LiveSessionPaths,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtectionReason {
    Live,
    Indeterminate(String),
}

impl ProtectionReason {
    pub fn message(&self) -> &str {
        match self {
            Self::Live => "session is live or locked",
            Self::Indeterminate(message) => message,
        }
    }

    pub fn is_indeterminate(&self) -> bool {
        matches!(self, Self::Indeterminate(_))
    }
}

#[derive(Debug, Clone, Default)]
pub struct LiveSessionSnapshot {
    sessions: HashMap<Agent, HashMap<String, ProtectionReason>>,
    provider_errors: HashMap<Agent, String>,
}

impl LiveSessionSnapshot {
    /// A provider-wide inspection failure is not evidence that every session is live.
    pub fn is_live(&self, agent: Agent, session_id: &str) -> bool {
        self.sessions
            .get(&agent)
            .is_some_and(|sessions| sessions.contains_key(session_id))
    }

    pub fn protection_reason(&self, agent: Agent, session_id: &str) -> Option<ProtectionReason> {
        self.sessions
            .get(&agent)
            .and_then(|sessions| sessions.get(session_id))
            .cloned()
            .or_else(|| self.provider_error(agent))
    }

    pub fn provider_error(&self, agent: Agent) -> Option<ProtectionReason> {
        self.provider_errors
            .get(&agent)
            .cloned()
            .map(ProtectionReason::Indeterminate)
    }

    fn protect(&mut self, agent: Agent, id: String, reason: ProtectionReason) {
        let sessions = self.sessions.entry(agent).or_default();
        // A live owner wins over an uncertain second registration for the same session.
        if sessions.get(&id) != Some(&ProtectionReason::Live) {
            sessions.insert(id, reason);
        }
    }

    fn failed(&mut self, agent: Agent, path: &Path, error: impl std::fmt::Display) {
        self.provider_errors.entry(agent).or_insert_with(|| {
            format!(
                "cannot verify live session markers at {}: {error}",
                path.display()
            )
        });
    }
}

/// Closing these handles releases ownership without modifying provider files.
pub(crate) struct LiveSessionGuard {
    _locks: Vec<File>,
}

impl LiveSessionTracker {
    pub fn new(paths: LiveSessionPaths) -> Self {
        Self { paths }
    }

    pub fn discover() -> Self {
        let home = BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
        let claude_home = env_override("CLAUDE_CONFIG_DIR")
            .or_else(|| home.as_ref().map(|home| home.join(".claude")));
        let codex_home =
            env_override("CODEX_HOME").or_else(|| home.as_ref().map(|home| home.join(".codex")));
        let antigravity_home = env_override("AICS_ANTIGRAVITY_HOME")
            .or_else(|| home.map(|home| home.join(".gemini/antigravity-cli")));
        Self::new(LiveSessionPaths {
            claude_sessions_dir: env_override("AICS_CLAUDE_SESSIONS_DIR")
                .or_else(|| claude_home.map(|home| home.join("sessions"))),
            codex_writer_locks_dir: codex_home.map(|home| home.join("thread-writer-locks")),
            antigravity_presence_dir: antigravity_home.map(|home| home.join("presence")),
        })
    }

    pub fn from_claude_sessions_dir(path: impl Into<PathBuf>) -> Self {
        Self::new(LiveSessionPaths {
            claude_sessions_dir: Some(path.into()),
            ..LiveSessionPaths::default()
        })
    }

    pub fn snapshot(&self) -> LiveSessionSnapshot {
        let mut snapshot = LiveSessionSnapshot::default();
        self.collect_claude(&mut snapshot);
        for (agent, root) in [
            (Agent::Codex, &self.paths.codex_writer_locks_dir),
            (Agent::Antigravity, &self.paths.antigravity_presence_dir),
        ] {
            if let Some(root) = root {
                collect_locks(&mut snapshot, agent, root);
            }
        }
        snapshot
    }

    fn collect_claude(&self, snapshot: &mut LiveSessionSnapshot) {
        let Some(root) = &self.paths.claude_sessions_dir else {
            return;
        };
        let mut markers = Vec::new();
        for path in marker_paths(snapshot, Agent::Claude, root, "json") {
            let raw = match fs::read(&path) {
                Ok(raw) => raw,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    snapshot.failed(Agent::Claude, &path, error);
                    continue;
                }
            };
            match serde_json::from_slice::<Value>(&raw) {
                Ok(marker) => {
                    let Some(id) = marker
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .filter(|id| !id.trim().is_empty())
                    else {
                        snapshot.failed(Agent::Claude, &path, "marker has no session ID");
                        continue;
                    };
                    let pid = marker
                        .get("pid")
                        .and_then(Value::as_u64)
                        .and_then(|pid| u32::try_from(pid).ok())
                        .filter(|pid| *pid > 0);
                    let registered_at = marker.get("startedAt").and_then(marker_timestamp);
                    markers.push((id.to_owned(), pid, registered_at));
                }
                Err(error) => snapshot.failed(Agent::Claude, &path, error),
            }
        }

        let pids = markers
            .iter()
            .filter_map(|(_, pid, _)| *pid)
            .collect::<Vec<_>>();
        let processes = observe_processes(&pids);
        for (id, pid, registered_at) in markers {
            let state = pid
                .and_then(|pid| processes.get(&pid).copied())
                .unwrap_or(ProcessObservation::Unknown);
            if let Some(reason) = claude_protection(state, registered_at) {
                snapshot.protect(Agent::Claude, id, reason);
            }
        }
    }

    /// Recheck immediately before mutation, retaining existing OS locks until it finishes.
    pub(crate) fn acquire_action_guard(
        &self,
        agent: Agent,
        session_id: &str,
    ) -> Result<LiveSessionGuard, ProtectionReason> {
        if agent == Agent::Claude {
            let mut snapshot = LiveSessionSnapshot::default();
            self.collect_claude(&mut snapshot);
            return match snapshot.protection_reason(agent, session_id) {
                Some(reason) => Err(reason),
                None => Ok(LiveSessionGuard { _locks: Vec::new() }),
            };
        }
        let root = match agent {
            Agent::Codex => &self.paths.codex_writer_locks_dir,
            Agent::Antigravity => &self.paths.antigravity_presence_dir,
            Agent::Claude => unreachable!(),
        };
        let mut locks = Vec::new();
        if let Some(root) = root {
            if !safe_session_id(session_id) {
                return Err(ProtectionReason::Indeterminate(
                    "cannot verify lock for invalid session ID".to_owned(),
                ));
            }
            if agent == Agent::Codex {
                let coordination = root.join(".coordination.lock");
                match lock_existing(&coordination)? {
                    Some(file) => locks.push(file),
                    None => match fs::metadata(root) {
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        _ => {
                            return Err(ProtectionReason::Indeterminate(format!(
                                "cannot verify Codex writer coordination at {}",
                                coordination.display()
                            )))
                        }
                    },
                }
            }
            if let Some(file) = lock_existing(&root.join(format!("{session_id}.lock")))? {
                locks.push(file);
            }
        }
        Ok(LiveSessionGuard { _locks: locks })
    }
}

fn marker_paths(
    snapshot: &mut LiveSessionSnapshot,
    agent: Agent,
    root: &Path,
    extension: &str,
) -> Vec<PathBuf> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            snapshot.failed(agent, root, error);
            return Vec::new();
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some(extension)
                    && !entry.file_name().to_string_lossy().starts_with('.')
                {
                    paths.push(path);
                }
            }
            Err(error) => snapshot.failed(agent, root, error),
        }
    }
    paths.sort();
    paths
}

fn collect_locks(snapshot: &mut LiveSessionSnapshot, agent: Agent, root: &Path) {
    for path in marker_paths(snapshot, agent, root, "lock") {
        let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
            snapshot.failed(agent, &path, "lock filename is not UTF-8");
            continue;
        };
        if let Err(reason) = lock_existing(&path) {
            snapshot.protect(agent, id.to_owned(), reason);
        }
    }
}

fn lock_existing(path: &Path) -> Result<Option<File>, ProtectionReason> {
    let file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ProtectionReason::Indeterminate(format!(
                "cannot inspect session lock {}: {error}",
                path.display()
            )))
        }
    };
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Err(ProtectionReason::Live),
        Err(TryLockError::Error(error)) => Err(ProtectionReason::Indeterminate(format!(
            "cannot inspect session lock {}: {error}",
            path.display()
        ))),
    }
}

fn safe_session_id(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && !id.contains(['/', '\\'])
}

fn env_override(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn marker_timestamp(value: &Value) -> Option<u64> {
    value.as_u64().map(|millis| millis / 1000).or_else(|| {
        chrono::DateTime::parse_from_rfc3339(value.as_str()?)
            .ok()?
            .timestamp()
            .try_into()
            .ok()
    })
}

#[derive(Debug, Clone, Copy)]
enum ProcessObservation {
    Alive(u64),
    Dead,
    Unknown,
}

fn claude_protection(
    state: ProcessObservation,
    registered_at: Option<u64>,
) -> Option<ProtectionReason> {
    match state {
        ProcessObservation::Dead => None,
        ProcessObservation::Alive(start)
            if start != 0 && registered_at.is_some_and(|at| start > at) =>
        {
            None
        }
        ProcessObservation::Alive(_) => Some(ProtectionReason::Live),
        ProcessObservation::Unknown => Some(ProtectionReason::Indeterminate(
            "cannot verify Claude session owner PID".to_owned(),
        )),
    }
}

fn observe_processes(pids: &[u32]) -> HashMap<u32, ProcessObservation> {
    if pids.is_empty() || !sysinfo::IS_SUPPORTED_SYSTEM {
        return HashMap::new();
    }
    let Ok(current_pid) = sysinfo::get_current_pid() else {
        return HashMap::new();
    };
    let mut requested = pids.iter().copied().map(Pid::from_u32).collect::<Vec<_>>();
    requested.push(current_pid);
    requested.sort_unstable();
    requested.dedup();
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&requested),
        true,
        ProcessRefreshKind::nothing().without_tasks(),
    );
    // A failed platform snapshot must not make an absent PID look conclusively dead.
    if system.process(current_pid).is_none() {
        return HashMap::new();
    }
    pids.iter()
        .map(|pid| {
            (
                *pid,
                match system.process(Pid::from_u32(*pid)) {
                    Some(process) => ProcessObservation::Alive(process.start_time()),
                    None => ProcessObservation::Dead,
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn claude_pid_reuse_and_uncertainty() {
        assert_eq!(claude_protection(ProcessObservation::Dead, None), None);
        assert_eq!(
            claude_protection(ProcessObservation::Alive(200), Some(100)),
            None
        );
        assert_eq!(
            claude_protection(ProcessObservation::Alive(100), Some(200)),
            Some(ProtectionReason::Live)
        );
        assert_eq!(
            claude_protection(ProcessObservation::Alive(0), Some(100)),
            Some(ProtectionReason::Live)
        );
        assert!(claude_protection(ProcessObservation::Unknown, None)
            .unwrap()
            .is_indeterminate());
        assert_eq!(
            marker_timestamp(&serde_json::json!(1720000000123u64)),
            Some(1720000000)
        );
        assert_eq!(
            marker_timestamp(&serde_json::json!("2024-07-03T09:46:40Z")),
            Some(1720000000)
        );
    }

    #[test]
    fn codex_guard_holds_coordination_without_rewriting_markers() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let coordination = root.join(".coordination.lock");
        fs::write(&coordination, "coordination bytes").unwrap();
        fs::write(root.join("idle.lock"), "lock bytes").unwrap();
        let tracker = LiveSessionTracker::new(LiveSessionPaths {
            codex_writer_locks_dir: Some(root.to_path_buf()),
            ..Default::default()
        });
        let guard = tracker.acquire_action_guard(Agent::Codex, "idle").unwrap();
        assert!(matches!(
            lock_existing(&coordination),
            Err(ProtectionReason::Live)
        ));
        assert!(matches!(
            lock_existing(&root.join("idle.lock")),
            Err(ProtectionReason::Live)
        ));
        drop(guard);
        assert!(lock_existing(&coordination).is_ok());
        assert_eq!(
            fs::read_to_string(coordination).unwrap(),
            "coordination bytes"
        );
        assert_eq!(
            fs::read_to_string(root.join("idle.lock")).unwrap(),
            "lock bytes"
        );
        let _guard = tracker
            .acquire_action_guard(Agent::Codex, "missing")
            .unwrap();
        assert!(!root.join("missing.lock").exists());
    }

    #[test]
    fn malformed_markers_protect_provider_without_blanket_live_badges() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("bad.json"), "{not json").unwrap();
        let tracker = LiveSessionTracker::from_claude_sessions_dir(temp.path());
        let snapshot = tracker.snapshot();
        assert!(!snapshot.is_live(Agent::Claude, "unrelated"));
        assert!(snapshot
            .protection_reason(Agent::Claude, "unrelated")
            .unwrap()
            .is_indeterminate());
        assert!(snapshot
            .protection_reason(Agent::Codex, "unrelated")
            .is_none());
        assert!(tracker
            .acquire_action_guard(Agent::Claude, "unrelated")
            .is_err());
    }
}
