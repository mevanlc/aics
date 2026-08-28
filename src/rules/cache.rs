use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use crc32fast::Hasher;
use log::warn;
use serde::{Deserialize, Serialize};

use super::RawRuleOutcome;
use crate::index::StoredSession;
use crate::scan::SessionFile;

const RULES_CACHE_FORMAT_VERSION: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ContentFingerprint {
    byte_len: u64,
    modified_ns: u64,
    crc32: u32,
    #[serde(default)]
    source_signature: u64,
}

impl ContentFingerprint {
    fn metadata(self) -> FileMetadataFingerprint {
        FileMetadataFingerprint {
            byte_len: self.byte_len,
            modified_ns: self.modified_ns,
            source_signature: self.source_signature,
        }
    }

    pub(super) fn has_same_content(self, other: Self) -> bool {
        self.byte_len == other.byte_len
            && self.crc32 == other.crc32
            && self.source_signature == other.source_signature
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileMetadataFingerprint {
    byte_len: u64,
    modified_ns: u64,
    source_signature: u64,
}

impl FileMetadataFingerprint {
    fn from_session(file: &SessionFile) -> Self {
        Self {
            byte_len: file.size,
            modified_ns: modified_ns(file.modified),
            source_signature: file.source_signature,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum CachedDetermination {
    Ignored,
    NoMatch,
    Unevaluated {
        session: Box<StoredSession>,
    },
    ParseError {
        error: String,
    },
    Evaluated {
        session: Box<StoredSession>,
        outcomes: Vec<RawRuleOutcome>,
    },
    EvaluationError {
        session: Box<StoredSession>,
        error: String,
    },
}

pub(super) enum CacheLookup {
    Hit {
        determination: CachedDetermination,
        fingerprint: ContentFingerprint,
    },
    Validate {
        determination: CachedDetermination,
        fingerprint: ContentFingerprint,
    },
    Miss(Option<ContentFingerprint>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedSession {
    content: ContentFingerprint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    superseded_by: Option<String>,
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
        let mut loaded = match load_state(&path) {
            Ok(state) => state,
            Err(error) => {
                warn!(
                    "ignoring unreadable rules cache {}: {error:#}",
                    path.display()
                );
                None
            }
        };

        let mut metadata_refreshed = false;
        let (reusable, aics_bin, rules_js) = if let Some(state) = loaded
            .as_mut()
            .filter(|state| state.format_version == RULES_CACHE_FORMAT_VERSION)
        {
            let current_aics_bin = validate_cached_file(aics_bin_path, state.aics_bin)
                .with_context(|| {
                    format!("failed to validate aics binary {}", aics_bin_path.display())
                })?;
            let current_rules_js =
                validate_cached_file(rules_path, state.rules_js).with_context(|| {
                    format!("failed to validate rules file {}", rules_path.display())
                })?;
            let reusable = state.aics_bin.has_same_content(current_aics_bin)
                && state.rules_js.has_same_content(current_rules_js);
            metadata_refreshed = reusable
                && (state.aics_bin != current_aics_bin || state.rules_js != current_rules_js);
            (reusable, current_aics_bin, current_rules_js)
        } else {
            let aics_bin = fingerprint_file(aics_bin_path).with_context(|| {
                format!(
                    "failed to fingerprint aics binary {}",
                    aics_bin_path.display()
                )
            })?;
            let rules_js = fingerprint_file(rules_path).with_context(|| {
                format!("failed to fingerprint rules file {}", rules_path.display())
            })?;
            (false, aics_bin, rules_js)
        };
        let state = if reusable {
            let mut state = loaded.expect("reusable cache state must be present");
            state.aics_bin = aics_bin;
            state.rules_js = rules_js;
            state
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
            dirty: !reusable || metadata_refreshed,
            reused: reusable,
        })
    }

    pub(super) fn was_reused(&self) -> bool {
        self.reused
    }

    pub(super) fn lookup(&self, file: &SessionFile, superseded_by: Option<&str>) -> CacheLookup {
        let Some(cached) = self.state.sessions.get(&normalize_path_key(&file.path)) else {
            return CacheLookup::Miss(None);
        };
        if cached.superseded_by.as_deref() != superseded_by {
            return CacheLookup::Miss(None);
        }
        let current_metadata = FileMetadataFingerprint::from_session(file);
        if cached.content.metadata() == current_metadata {
            return CacheLookup::Hit {
                determination: cached.determination.clone(),
                fingerprint: cached.content,
            };
        }
        if cached.content.byte_len != current_metadata.byte_len {
            return CacheLookup::Miss(None);
        }

        CacheLookup::Validate {
            determination: cached.determination.clone(),
            fingerprint: cached.content,
        }
    }

    pub(super) fn insert(
        &mut self,
        path: &Path,
        content: ContentFingerprint,
        superseded_by: Option<&str>,
        determination: CachedDetermination,
    ) {
        self.state.sessions.insert(
            normalize_path_key(path),
            CachedSession {
                content,
                superseded_by: superseded_by.map(str::to_owned),
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
    let metadata_before = fingerprint_metadata(path)?;
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

    let metadata_after = fingerprint_metadata(path)?;
    if metadata_before != metadata_after || metadata_after.byte_len != byte_len {
        bail!(
            "{} changed while it was being fingerprinted",
            path.display()
        );
    }

    Ok(ContentFingerprint {
        byte_len: metadata_after.byte_len,
        modified_ns: metadata_after.modified_ns,
        crc32: hasher.finalize(),
        source_signature: 0,
    })
}

pub(super) fn fingerprint_session(file: &SessionFile) -> Result<ContentFingerprint> {
    let mut hasher = Hasher::new();
    let mut byte_len = 0_u64;
    for path in file.source_paths() {
        hasher.update(path.as_os_str().to_string_lossy().as_bytes());
        hasher.update(&[0]);
        let mut input = fs::File::open(path)
            .with_context(|| format!("failed to open {} for fingerprinting", path.display()))?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = input
                .read(&mut buffer)
                .with_context(|| format!("failed to read {} for fingerprinting", path.display()))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            byte_len = byte_len
                .checked_add(read as u64)
                .context("session source length overflow while fingerprinting")?;
        }
    }
    hasher.update(&file.source_signature.to_be_bytes());
    Ok(ContentFingerprint {
        byte_len,
        modified_ns: modified_ns(file.modified),
        crc32: hasher.finalize(),
        source_signature: file.source_signature,
    })
}

fn validate_cached_file(path: &Path, cached: ContentFingerprint) -> Result<ContentFingerprint> {
    let metadata = fingerprint_metadata(path)?;
    if cached.metadata() == metadata {
        return Ok(cached);
    }
    fingerprint_file(path)
}

fn fingerprint_metadata(path: &Path) -> Result<FileMetadataFingerprint> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to stat {} for fingerprinting", path.display()))?;
    let modified = metadata
        .modified()
        .with_context(|| format!("failed to read modification time for {}", path.display()))?;
    Ok(FileMetadataFingerprint {
        byte_len: metadata.len(),
        modified_ns: modified_ns(modified),
        source_signature: 0,
    })
}

fn modified_ns(modified: SystemTime) -> u64 {
    modified
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .try_into()
        .unwrap_or(u64::MAX)
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
    use crate::parse::Agent;
    use crate::rules::{run_rules, RuleSelection, RulesMode, RulesOptions};
    use crate::scan::{SessionFile, SessionRoots};
    use tempfile::TempDir;

    fn scanned_session(path: &Path) -> Result<SessionFile> {
        let metadata = fs::metadata(path)?;
        Ok(SessionFile {
            path: path.to_path_buf(),
            agent: Agent::Claude,
            modified: metadata.modified()?,
            size: metadata.len(),
            trashed: false,
            original_path: None,
            companion_paths: Vec::new(),
            source_signature: 0,
            antigravity_metadata: None,
        })
    }

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
            None,
            CachedDetermination::Ignored,
        );
        cache.save()?;

        let mut cache = RulesCache::open_with_binary(cache_path, &rules, &binary)?;
        assert!(matches!(
            cache.lookup(&scanned_session(&session)?, None),
            CacheLookup::Hit {
                determination: CachedDetermination::Ignored,
                ..
            }
        ));

        cache
            .state
            .sessions
            .get_mut(&normalize_path_key(&session))
            .unwrap()
            .content
            .modified_ns = 0;
        fs::write(&session, b"bravo")?;
        let scanned = scanned_session(&session)?;
        let CacheLookup::Validate {
            fingerprint,
            determination: CachedDetermination::Ignored,
        } = cache.lookup(&scanned, None)
        else {
            panic!("same-length rewrite should require worker validation");
        };
        assert!(!fingerprint.has_same_content(fingerprint_session(&scanned)?));
        Ok(())
    }

    #[test]
    fn cache_trusts_matching_metadata_without_recomputing_crc32() -> Result<()> {
        let temp = TempDir::new()?;
        let binary = temp.path().join("aics");
        let rules = temp.path().join("rules.js");
        let session = temp.path().join("session.jsonl");
        let cache_path = temp.path().join("rules-cache.json");
        fs::write(&binary, b"binary-v1")?;
        fs::write(&rules, b"rules-v1")?;
        fs::write(&session, b"alpha")?;

        let mut cache = RulesCache::open_with_binary(cache_path, &rules, &binary)?;
        cache.insert(
            &session,
            fingerprint_file(&session)?,
            None,
            CachedDetermination::Ignored,
        );
        cache
            .state
            .sessions
            .get_mut(&normalize_path_key(&session))
            .unwrap()
            .content
            .crc32 ^= u32::MAX;

        assert!(matches!(
            cache.lookup(&scanned_session(&session)?, None),
            CacheLookup::Hit {
                determination: CachedDetermination::Ignored,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn cache_invalidates_when_supersession_changes() -> Result<()> {
        let temp = TempDir::new()?;
        let binary = temp.path().join("aics");
        let rules = temp.path().join("rules.js");
        let session = temp.path().join("session.jsonl");
        let cache_path = temp.path().join("rules-cache.json");
        fs::write(&binary, b"binary-v1")?;
        fs::write(&rules, b"rules-v1")?;
        fs::write(&session, b"session")?;

        let mut cache = RulesCache::open_with_binary(cache_path, &rules, &binary)?;
        cache.insert(
            &session,
            fingerprint_file(&session)?,
            None,
            CachedDetermination::NoMatch,
        );
        assert!(matches!(
            cache.lookup(&scanned_session(&session)?, Some("keeper")),
            CacheLookup::Miss(None)
        ));
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
            None,
            CachedDetermination::Ignored,
        );
        cache.state.aics_bin.modified_ns = 0;
        cache.state.rules_js.modified_ns = 0;
        cache.save()?;

        fs::write(&binary, b"bin-two")?;
        let mut cache = RulesCache::open_with_binary(cache_path.clone(), &rules, &binary)?;
        assert!(matches!(
            cache.lookup(&scanned_session(&session)?, None),
            CacheLookup::Miss(None)
        ));
        cache.insert(
            &session,
            fingerprint_file(&session)?,
            None,
            CachedDetermination::Ignored,
        );
        cache.state.rules_js.modified_ns = 0;
        cache.save()?;

        fs::write(&rules, b"rules-b")?;
        let cache = RulesCache::open_with_binary(cache_path, &rules, &binary)?;
        assert!(matches!(
            cache.lookup(&scanned_session(&session)?, None),
            CacheLookup::Miss(None)
        ));
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
            None,
            CachedDetermination::Ignored,
        );
        cache.save()?;

        let state: serde_json::Value = serde_json::from_slice(&fs::read(cache_path)?)?;
        assert_eq!(state["format_version"], RULES_CACHE_FORMAT_VERSION);
        assert_eq!(state["aics_bin"]["byte_len"], 6);
        assert!(state["aics_bin"]["modified_ns"].is_u64());
        assert!(state["aics_bin"]["crc32"].is_u64());
        assert_eq!(state["rules_js"]["byte_len"], 5);
        assert!(state["rules_js"]["modified_ns"].is_u64());
        let cached_session = state["sessions"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap();
        assert_eq!(cached_session["content"]["byte_len"], 7);
        assert!(cached_session["content"]["modified_ns"].is_u64());
        assert!(cached_session["content"]["crc32"].is_u64());
        Ok(())
    }

    #[test]
    fn no_match_determination_has_compact_serialization() -> Result<()> {
        let serialized = serde_json::to_value(CachedDetermination::NoMatch)?;
        assert_eq!(serialized, serde_json::json!({ "status": "no_match" }));
        Ok(())
    }

    #[test]
    fn rules_runner_defers_filtered_session_until_filter_includes_it() -> Result<()> {
        let temp = TempDir::new()?;
        let session = temp
            .path()
            .join(".claude/projects/-tmp-project/sidechain.jsonl");
        fs::create_dir_all(session.parent().unwrap())?;
        fs::write(
            &session,
            concat!(
                r#"{"type":"user","sessionId":"sidechain","isSidechain":true,"message":{"role":"user","content":"first"}}"#,
                "\n",
            ),
        )?;
        let rules = temp.path().join("rules.js");
        fs::write(&rules, r#"rule("match", () => trash("evaluated"));"#)?;
        let cache_path = temp.path().join("rules-cache.json");
        let roots = SessionRoots {
            claude_projects: temp.path().join(".claude/projects"),
            codex_sessions: temp.path().join(".codex/sessions"),
            antigravity_home: temp.path().join(".gemini/antigravity-cli"),
            trash: None,
        };
        let mut options = RulesOptions {
            rules_path: rules,
            cache_path: Some(cache_path.clone()),
            mode: RulesMode::Preview,
            selection: RuleSelection::All,
            json: true,
            scope: Scope::Global,
            filters: SearchFilters::default(),
            supersession: BTreeMap::new(),
        };

        let filtered = run_rules(&roots, &options)?;
        assert!(filtered.proposals.is_empty());
        let state: serde_json::Value = serde_json::from_slice(&fs::read(&cache_path)?)?;
        assert_eq!(
            state["sessions"][normalize_path_key(&session)]["determination"]["status"],
            "unevaluated"
        );

        let cached = run_rules(&roots, &options)?;
        assert!(cached.proposals.is_empty());

        options.filters.include_sub_agents = true;
        let included = run_rules(&roots, &options)?;
        assert_eq!(included.proposals.len(), 1);
        let state: serde_json::Value = serde_json::from_slice(&fs::read(cache_path)?)?;
        assert_eq!(
            state["sessions"][normalize_path_key(&session)]["determination"]["status"],
            "evaluated"
        );
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
            antigravity_home: temp.path().join(".gemini/antigravity-cli"),
            trash: None,
        };
        let options = RulesOptions {
            rules_path: rules.clone(),
            cache_path: Some(cache_path.clone()),
            mode: RulesMode::Preview,
            selection: RuleSelection::All,
            json: true,
            scope: Scope::Global,
            filters: SearchFilters::default(),
            supersession: BTreeMap::new(),
        };

        let first = run_rules(&roots, &options)?;
        assert_eq!(first.proposals.len(), 1);

        let mut cache = RulesCache::open(cache_path.clone(), &rules)?;
        cache
            .state
            .sessions
            .get_mut(&normalize_path_key(&session))
            .unwrap()
            .content
            .modified_ns = 0;
        cache.dirty = true;
        cache.save()?;

        let cached = run_rules(&roots, &options)?;
        assert_eq!(cached.proposals, first.proposals);
        let cache = RulesCache::open(cache_path.clone(), &rules)?;
        assert!(matches!(
            cache.lookup(&scanned_session(&session)?, None),
            CacheLookup::Hit { .. }
        ));

        let mut cache = RulesCache::open(cache_path, &rules)?;
        cache.insert(
            &session,
            fingerprint_file(&session)?,
            None,
            CachedDetermination::Ignored,
        );
        cache.save()?;

        let second = run_rules(&roots, &options)?;
        assert!(second.proposals.is_empty());
        Ok(())
    }
}
