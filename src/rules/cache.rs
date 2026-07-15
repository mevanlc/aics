use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use crc32fast::Hasher;
use log::warn;
use serde::{Deserialize, Serialize};

use super::RawRuleOutcome;
use crate::index::StoredSession;

const RULES_CACHE_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ContentFingerprint {
    byte_len: u64,
    crc32: u32,
}

impl ContentFingerprint {
    pub(super) fn byte_len(self) -> u64 {
        self.byte_len
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum CachedDetermination {
    Ignored,
    ParseError {
        error: String,
    },
    Evaluated {
        session: StoredSession,
        outcomes: Vec<RawRuleOutcome>,
    },
    EvaluationError {
        session: StoredSession,
        error: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedSession {
    content: ContentFingerprint,
    determination: CachedDetermination,
}

#[derive(Debug, Serialize, Deserialize)]
struct RulesCacheState {
    format_version: u32,
    aics_bin: ContentFingerprint,
    rules_js: ContentFingerprint,
    sessions: BTreeMap<String, CachedSession>,
}

pub(super) struct RulesCache {
    path: PathBuf,
    state: RulesCacheState,
    dirty: bool,
    reused: bool,
}

impl RulesCache {
    pub(super) fn open(path: PathBuf, rules_path: &Path) -> Result<Self> {
        let aics_bin =
            std::env::current_exe().context("failed to locate the running aics binary")?;
        Self::open_with_binary(path, rules_path, &aics_bin)
    }

    fn open_with_binary(path: PathBuf, rules_path: &Path, aics_bin_path: &Path) -> Result<Self> {
        let aics_bin = fingerprint_file(aics_bin_path).with_context(|| {
            format!(
                "failed to fingerprint aics binary {}",
                aics_bin_path.display()
            )
        })?;
        let rules_js = fingerprint_file(rules_path).with_context(|| {
            format!("failed to fingerprint rules file {}", rules_path.display())
        })?;

        let loaded = match load_state(&path) {
            Ok(state) => state,
            Err(error) => {
                warn!(
                    "ignoring unreadable rules cache {}: {error:#}",
                    path.display()
                );
                None
            }
        };
        let reusable = loaded.as_ref().is_some_and(|state| {
            state.format_version == RULES_CACHE_FORMAT_VERSION
                && state.aics_bin == aics_bin
                && state.rules_js == rules_js
        });
        let state = if reusable {
            loaded.expect("reusable cache state must be present")
        } else {
            RulesCacheState {
                format_version: RULES_CACHE_FORMAT_VERSION,
                aics_bin,
                rules_js,
                sessions: BTreeMap::new(),
            }
        };

        Ok(Self {
            path,
            state,
            dirty: !reusable,
            reused: reusable,
        })
    }

    pub(super) fn was_reused(&self) -> bool {
        self.reused
    }

    pub(super) fn get(
        &self,
        path: &Path,
        content: ContentFingerprint,
    ) -> Option<&CachedDetermination> {
        self.state
            .sessions
            .get(&normalize_path_key(path))
            .filter(|cached| cached.content == content)
            .map(|cached| &cached.determination)
    }

    pub(super) fn cached_byte_len(&self, path: &Path) -> Option<u64> {
        self.state
            .sessions
            .get(&normalize_path_key(path))
            .map(|cached| cached.content.byte_len())
    }

    pub(super) fn insert(
        &mut self,
        path: &Path,
        content: ContentFingerprint,
        determination: CachedDetermination,
    ) {
        self.state.sessions.insert(
            normalize_path_key(path),
            CachedSession {
                content,
                determination,
            },
        );
        self.dirty = true;
    }

    pub(super) fn retain_files<'a>(&mut self, paths: impl IntoIterator<Item = &'a Path>) {
        let current = paths
            .into_iter()
            .map(normalize_path_key)
            .collect::<BTreeSet<_>>();
        let previous_len = self.state.sessions.len();
        self.state.sessions.retain(|path, _| current.contains(path));
        self.dirty |= self.state.sessions.len() != previous_len;
    }

    pub(super) fn save(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }

        let mut raw =
            serde_json::to_string_pretty(&self.state).context("failed to serialize rules cache")?;
        raw.push('\n');
        write_atomic(&self.path, raw.as_bytes())?;
        self.dirty = false;
        Ok(())
    }
}

pub(super) fn fingerprint_file(path: &Path) -> Result<ContentFingerprint> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("failed to open {} for fingerprinting", path.display()))?;
    let mut hasher = Hasher::new();
    let mut byte_len = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];

    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read {} for fingerprinting", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        byte_len = byte_len
            .checked_add(read as u64)
            .context("file length overflow while fingerprinting")?;
    }

    Ok(ContentFingerprint {
        byte_len,
        crc32: hasher.finalize(),
    })
}

fn load_state(path: &Path) -> Result<Option<RulesCacheState>> {
    if !path.exists() {
        return Ok(None);
    }

    let raw =
        fs::read(path).with_context(|| format!("failed to read rules cache {}", path.display()))?;
    serde_json::from_slice(&raw)
        .with_context(|| format!("failed to parse rules cache {}", path.display()))
        .map(Some)
}

fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut temp_name = path.file_name().unwrap_or_default().to_os_string();
    temp_name.push(format!(".tmp-{}", std::process::id()));
    let temp_path = path.with_file_name(temp_name);
    let result = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&temp_path)?;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&temp_path, path)
    })()
    .with_context(|| format!("failed to write rules cache {}", path.display()));
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn normalize_path_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Scope, SearchFilters};
    use crate::rules::{run_rules, RulesMode, RulesOptions};
    use crate::scan::SessionRoots;
    use tempfile::TempDir;

    #[test]
    fn cache_reuses_unchanged_session_and_rejects_same_length_rewrite() -> Result<()> {
        let temp = TempDir::new()?;
        let binary = temp.path().join("aics");
        let rules = temp.path().join("rules.js");
        let session = temp.path().join("session.jsonl");
        let cache_path = temp.path().join("rules-cache.json");
        fs::write(&binary, b"binary-v1")?;
        fs::write(&rules, b"rules-v1")?;
        fs::write(&session, b"alpha")?;

        let mut cache = RulesCache::open_with_binary(cache_path.clone(), &rules, &binary)?;
        cache.insert(
            &session,
            fingerprint_file(&session)?,
            CachedDetermination::Ignored,
        );
        cache.save()?;

        let cache = RulesCache::open_with_binary(cache_path, &rules, &binary)?;
        assert!(cache.get(&session, fingerprint_file(&session)?).is_some());

        fs::write(&session, b"bravo")?;
        assert!(cache.get(&session, fingerprint_file(&session)?).is_none());
        Ok(())
    }

    #[test]
    fn cache_invalidates_on_same_length_binary_or_rules_rewrite() -> Result<()> {
        let temp = TempDir::new()?;
        let binary = temp.path().join("aics");
        let rules = temp.path().join("rules.js");
        let session = temp.path().join("session.jsonl");
        let cache_path = temp.path().join("rules-cache.json");
        fs::write(&binary, b"bin-one")?;
        fs::write(&rules, b"rules-a")?;
        fs::write(&session, b"session")?;

        let mut cache = RulesCache::open_with_binary(cache_path.clone(), &rules, &binary)?;
        cache.insert(
            &session,
            fingerprint_file(&session)?,
            CachedDetermination::Ignored,
        );
        cache.save()?;

        fs::write(&binary, b"bin-two")?;
        let mut cache = RulesCache::open_with_binary(cache_path.clone(), &rules, &binary)?;
        assert!(cache.get(&session, fingerprint_file(&session)?).is_none());
        cache.insert(
            &session,
            fingerprint_file(&session)?,
            CachedDetermination::Ignored,
        );
        cache.save()?;

        fs::write(&rules, b"rules-b")?;
        let cache = RulesCache::open_with_binary(cache_path, &rules, &binary)?;
        assert!(cache.get(&session, fingerprint_file(&session)?).is_none());
        Ok(())
    }

    #[test]
    fn cache_file_records_requested_fingerprints() -> Result<()> {
        let temp = TempDir::new()?;
        let binary = temp.path().join("aics");
        let rules = temp.path().join("rules.js");
        let session = temp.path().join("session.jsonl");
        let cache_path = temp.path().join("rules-cache.json");
        fs::write(&binary, b"binary")?;
        fs::write(&rules, b"rules")?;
        fs::write(&session, b"session")?;

        let mut cache = RulesCache::open_with_binary(cache_path.clone(), &rules, &binary)?;
        cache.insert(
            &session,
            fingerprint_file(&session)?,
            CachedDetermination::Ignored,
        );
        cache.save()?;

        let state: serde_json::Value = serde_json::from_slice(&fs::read(cache_path)?)?;
        assert_eq!(state["aics_bin"]["byte_len"], 6);
        assert!(state["aics_bin"]["crc32"].is_u64());
        assert_eq!(state["rules_js"]["byte_len"], 5);
        let cached_session = state["sessions"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap();
        assert_eq!(cached_session["content"]["byte_len"], 7);
        assert!(cached_session["content"]["crc32"].is_u64());
        Ok(())
    }

    #[test]
    fn rules_runner_trusts_a_cached_determination() -> Result<()> {
        let temp = TempDir::new()?;
        let session = temp
            .path()
            .join(".claude/projects/-tmp-project/session.jsonl");
        fs::create_dir_all(session.parent().unwrap())?;
        fs::write(
            &session,
            concat!(
                r#"{"type":"user","sessionId":"cached","message":{"role":"user","content":"first"}}"#,
                "\n",
                r#"{"type":"user","sessionId":"cached","message":{"role":"user","content":"second"}}"#,
                "\n",
            ),
        )?;
        let rules = temp.path().join("rules.js");
        fs::write(&rules, r#"rule("match", () => trash("evaluated"));"#)?;
        let cache_path = temp.path().join("rules-cache.json");
        let roots = SessionRoots {
            claude_projects: temp.path().join(".claude/projects"),
            codex_sessions: temp.path().join(".codex/sessions"),
            trash: None,
        };
        let options = RulesOptions {
            rules_path: rules.clone(),
            cache_path: Some(cache_path.clone()),
            mode: RulesMode::Preview,
            json: true,
            scope: Scope::Global,
            filters: SearchFilters::default(),
        };

        let first = run_rules(&roots, &options)?;
        assert_eq!(first.proposals.len(), 1);

        let cached = run_rules(&roots, &options)?;
        assert_eq!(cached.proposals, first.proposals);

        let mut cache = RulesCache::open(cache_path, &rules)?;
        cache.insert(
            &session,
            fingerprint_file(&session)?,
            CachedDetermination::Ignored,
        );
        cache.save()?;

        let second = run_rules(&roots, &options)?;
        assert!(second.proposals.is_empty());
        Ok(())
    }
}
