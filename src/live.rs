use std::env;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use directories::BaseDirs;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct LiveSessionTracker {
    claude_sessions_dir: Option<PathBuf>,
}

impl LiveSessionTracker {
    pub fn discover() -> Self {
        Self {
            claude_sessions_dir: env_override("AICS_CLAUDE_SESSIONS_DIR")
                .or_else(|| BaseDirs::new().map(|dirs| dirs.home_dir().join(".claude/sessions"))),
        }
    }

    pub fn from_claude_sessions_dir(path: impl Into<PathBuf>) -> Self {
        Self {
            claude_sessions_dir: Some(path.into()),
        }
    }

    pub fn live_session_ids(&self) -> HashSet<String> {
        let mut session_ids = HashSet::new();
        let Some(root) = &self.claude_sessions_dir else {
            return session_ids;
        };
        let Ok(entries) = fs::read_dir(root) else {
            return session_ids;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }

            let Ok(raw) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(marker) = serde_json::from_str::<ClaudeLiveSessionMarker>(&raw) else {
                continue;
            };
            if !marker.session_id.trim().is_empty() {
                session_ids.insert(marker.session_id);
            }
        }

        session_ids
    }
}

fn env_override(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[derive(Debug, Deserialize)]
struct ClaudeLiveSessionMarker {
    #[serde(rename = "sessionId")]
    session_id: String,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::LiveSessionTracker;

    #[test]
    fn collects_claude_live_session_markers() {
        let temp = TempDir::new().unwrap();
        fs::write(
            temp.path().join("123.json"),
            r#"{"pid":123,"sessionId":"live-session","cwd":"/tmp/demo","startedAt":"2026-03-21T00:00:00Z"}"#,
        )
        .unwrap();
        fs::write(temp.path().join("ignore.txt"), "not json").unwrap();
        fs::write(temp.path().join("bad.json"), "{not-json").unwrap();

        let tracker = LiveSessionTracker::from_claude_sessions_dir(temp.path());
        let live = tracker.live_session_ids();

        assert!(live.contains("live-session"));
        assert_eq!(live.len(), 1);
    }
}
