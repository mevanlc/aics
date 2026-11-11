use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::parse::Agent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashPaths {
    pub data_root: PathBuf,
    pub trash_dir: PathBuf,
    pub metadata_file: PathBuf,
    pub reasons_file: PathBuf,
}

impl TrashPaths {
    pub fn discover() -> Result<Self> {
        let data_root =
            if let Some(root) = env::var_os("AICS_DATA_ROOT").filter(|value| !value.is_empty()) {
                PathBuf::from(root)
            } else {
                BaseDirs::new()
                    .context("failed to locate data directory")?
                    .data_dir()
                    .join("aics")
            };
        Ok(Self::from_data_root(std::path::absolute(data_root)?))
    }

    pub fn from_data_root(data_root: impl Into<PathBuf>) -> Self {
        let data_root = data_root.into();
        Self {
            trash_dir: data_root.join("trash"),
            metadata_file: data_root.join("trash.jsonl"),
            reasons_file: data_root.join("trash.json"),
            data_root,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum TrashReason {
    DependencyGroup {
        triggers: Vec<TrashReason>,
        selected_paths: Vec<String>,
        relation: String,
    },
    KeyBinding {
        key: String,
    },
    ActionMenu {
        action: String,
    },
    Rule {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

pub const TRASH_SCHEMA_VERSION: u32 = 1;
pub(crate) const METADATA_NAME: &str = "metadata.json";
pub(crate) const ITEM_PREFIX: &str = "item-";
pub(crate) const ITEM_ID_LEN: usize = 16;
pub(crate) const SCRATCH_PREFIX: &str = ".aics-trash-";
pub(crate) const LEGACY_METADATA_DIR: &str = ".legacy-migration";
pub(crate) const SCHEMAS_DIR: &str = ".schemas";

/// Paths in the payload are relative to the item directory. A missing original
/// path represents a legacy item whose destination could not be recovered.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrashMember {
    #[serde(serialize_with = "serialize_stored_path")]
    pub stored_path: PathBuf,
    pub original_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrashMetadata {
    pub schema_version: u32,
    pub agent: Option<Agent>,
    pub trashed_at: Option<String>,
    #[serde(serialize_with = "serialize_session_path")]
    pub session_path: Option<PathBuf>,
    pub members: Vec<TrashMember>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<TrashReason>,
}

fn stored_path_text(path: &Path) -> Option<String> {
    path.iter()
        .map(|part| part.to_str())
        .collect::<Option<Vec<_>>>()
        .map(|parts| parts.join("/"))
}

fn serialize_stored_path<S: serde::Serializer>(
    path: &Path,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let text = stored_path_text(path)
        .ok_or_else(|| serde::ser::Error::custom("non-UTF-8 payload path"))?;
    serializer.serialize_str(&text)
}

fn serialize_session_path<S: serde::Serializer>(
    path: &Option<PathBuf>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match path {
        Some(path) => serialize_stored_path(path, serializer),
        None => serializer.serialize_none(),
    }
}

impl TrashMetadata {
    fn empty() -> Self {
        Self {
            schema_version: TRASH_SCHEMA_VERSION,
            agent: None,
            trashed_at: None,
            session_path: None,
            members: Vec::new(),
            reason: None,
        }
    }

    pub fn original_path(&self) -> Option<PathBuf> {
        let session = self.session_path.as_ref()?;
        let mut matches = self.members.iter().filter_map(|member| {
            let suffix = session.strip_prefix(&member.stored_path).ok()?;
            let original = member.original_path.as_ref()?;
            Some(if suffix.as_os_str().is_empty() {
                original.clone()
            } else {
                original.join(suffix)
            })
        });
        let original = matches.next()?;
        matches.next().is_none().then_some(original)
    }

    fn validate_paths(&self) -> Result<()> {
        if let Some(path) = &self.session_path {
            ensure_payload_path(path)?;
        }
        for (i, member) in self.members.iter().enumerate() {
            ensure_payload_path(&member.stored_path)?;
            if let Some(original) = &member.original_path {
                ensure_original_path(original)?;
            }
            anyhow::ensure!(
                !self.members[..i].iter().any(|other| {
                    member.stored_path.starts_with(&other.stored_path)
                        || other.stored_path.starts_with(&member.stored_path)
                }),
                "overlapping trash payload mappings"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct TrashItem {
    pub path: PathBuf,
    pub primary: Option<PathBuf>,
    pub metadata: TrashMetadata,
    /// Source and item-relative destination for each legacy payload to copy.
    pub(crate) legacy_copies: Option<Vec<(PathBuf, PathBuf)>>,
}

impl TrashItem {
    pub fn original_path(&self) -> Option<PathBuf> {
        self.metadata.original_path()
    }

    pub(crate) fn restore_pairs(&self) -> Result<Vec<(PathBuf, PathBuf)>> {
        anyhow::ensure!(
            self.legacy_copies.is_none(),
            "trash item still needs migration"
        );
        let agent = self.metadata.agent.context("unknown trash item provider")?;
        let source = self.primary.as_ref().context("unknown trash transcript")?;
        let original = self.original_path().context("original path is unknown")?;
        anyhow::ensure!(
            is_native_filename(&original, agent) && source.file_name() == original.file_name(),
            "original path does not match the native session filename"
        );
        let (base, paths) = native_payload(agent, source)?;
        let original_base = native_base(agent, &original)?;
        let mut pairs = Vec::new();
        for path in paths {
            let suffix = path.strip_prefix(&base)?;
            let target = if agent != Agent::Antigravity && path == *source {
                original.clone()
            } else if agent != Agent::Antigravity {
                crate::summary::sidecar_path(&original)
            } else {
                original_base.join(suffix)
            };
            let stored = path.strip_prefix(&self.path)?;
            let is_summary = path == crate::summary::sidecar_path(source);
            anyhow::ensure!(
                self.metadata.members.iter().any(|m| {
                    m.stored_path == stored && m.original_path.as_ref() == Some(&target)
                }) || is_summary,
                "missing or invalid restore mapping for {}",
                path.display()
            );
            pairs.push((path, target));
        }
        // A summary may have been generated in trash or removed since the move.
        // Every other recorded payload must still exist before we retire the item.
        for member in &self.metadata.members {
            let path = self.path.join(&member.stored_path);
            anyhow::ensure!(
                path == crate::summary::sidecar_path(source)
                    || pairs.iter().any(|(stored, _)| stored == &path),
                "unexpected or missing trash payload {}",
                path.display()
            );
        }
        check_payload_coverage(&self.path.join("payload"), &pairs)?;
        for path in children(&self.path)? {
            anyhow::ensure!(
                path.file_name()
                    .is_some_and(|name| name == METADATA_NAME || name == "payload"),
                "unexpected file in trash item {}",
                path.display()
            );
        }
        Ok(pairs)
    }
}

fn check_payload_coverage(path: &Path, pairs: &[(PathBuf, PathBuf)]) -> Result<()> {
    if pairs.iter().any(|(source, _)| source == path) {
        return Ok(());
    }
    anyhow::ensure!(
        fs::symlink_metadata(path)?.is_dir(),
        "unmapped trash payload {}",
        path.display()
    );
    for entry in fs::read_dir(path)? {
        check_payload_coverage(&entry?.path(), pairs)?;
    }
    Ok(())
}

pub(crate) fn ensure_original_path(path: &Path) -> Result<()> {
    use std::path::Component;
    anyhow::ensure!(
        path.is_absolute()
            && path.to_str().is_some()
            && !path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir)),
        "invalid original path {}",
        path.display()
    );
    Ok(())
}

fn ensure_payload_path(path: &Path) -> Result<()> {
    use std::path::Component;
    anyhow::ensure!(
        path.starts_with("payload")
            && path.components().count() > 1
            && path.to_str().is_some()
            && path.components().all(|c| matches!(c, Component::Normal(_))),
        "invalid trash payload path {}",
        path.display()
    );
    Ok(())
}

/// Enumerate the exact files or directories covered by the native session.
pub(crate) fn native_payload(agent: Agent, primary: &Path) -> Result<(PathBuf, Vec<PathBuf>)> {
    if agent == Agent::Antigravity {
        let bundle = AntigravityBundlePaths::from_transcript(primary)?;
        let mut paths = bundle.database_paths()?;
        paths.insert(0, bundle.conversation_dir);
        Ok((bundle.home, paths))
    } else {
        let mut paths = vec![primary.to_path_buf()];
        let summary = crate::summary::sidecar_path(primary);
        if exists(&summary)? {
            paths.push(summary);
        }
        Ok((native_base(agent, primary)?, paths))
    }
}

fn native_base(agent: Agent, primary: &Path) -> Result<PathBuf> {
    if agent == Agent::Antigravity {
        Ok(AntigravityBundlePaths::from_transcript(primary)?.home)
    } else {
        Ok(primary
            .parent()
            .context("session has no parent")?
            .to_path_buf())
    }
}

pub(crate) fn new_item(
    agent: Agent,
    primary: &Path,
    reason: TrashReason,
) -> Result<(TrashMetadata, Vec<PathBuf>)> {
    ensure_original_path(primary)?;
    let (base, sources) = native_payload(agent, primary)?;
    let metadata = TrashMetadata {
        schema_version: TRASH_SCHEMA_VERSION,
        agent: Some(agent),
        trashed_at: Some(chrono::Utc::now().to_rfc3339()),
        session_path: Some(Path::new("payload").join(primary.strip_prefix(&base)?)),
        members: sources
            .iter()
            .map(|path| {
                Ok(TrashMember {
                    stored_path: Path::new("payload").join(path.strip_prefix(&base)?),
                    original_path: Some(crate::lifecycle::resolved_path(path)),
                })
            })
            .collect::<Result<_>>()?,
        reason: Some(reason),
    };
    metadata.validate_paths()?;
    Ok((metadata, sources))
}

#[derive(Debug, Clone)]
pub struct TrashStore {
    paths: TrashPaths,
}

impl TrashStore {
    pub fn new(paths: TrashPaths) -> Self {
        Self { paths }
    }

    /// Discovery never changes trash. Flat legacy entries can coexist with
    /// migrated items until the last migration transaction finishes.
    pub fn snapshot(&self) -> Result<Vec<TrashItem>> {
        let mut items = self.legacy_items()?;
        for path in children(&self.paths.trash_dir)? {
            if is_item(&path) {
                items.push(read_item(path));
            }
        }
        Ok(items)
    }

    pub(crate) fn legacy_items(&self) -> Result<Vec<TrashItem>> {
        let paths = children(&self.paths.trash_dir)?
            .into_iter()
            .filter(|path| !is_item(path) && !is_reserved(path))
            .collect::<Vec<_>>();
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let entries = legacy_entries(&self.paths.metadata_file)?;
        let reasons = legacy_reasons(&self.paths.reasons_file)?;
        paths
            .iter()
            .filter(|path| {
                let is_paired_summary = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.strip_suffix(crate::summary::sidecar::SIDECAR_SUFFIX))
                    .is_some_and(|name| paths.iter().any(|p| p == &path.with_file_name(name)));
                !is_paired_summary
            })
            .map(|path| {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .context("non-UTF-8 legacy trash name")?;
                legacy_item(path, entries.get(name), reasons.get(name))
            })
            .collect()
    }
}

pub(crate) fn is_item(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix(ITEM_PREFIX))
        .is_some_and(|id| id.len() == ITEM_ID_LEN && id.bytes().all(|c| c.is_ascii_alphanumeric()))
        && fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

pub(crate) fn is_reserved(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
        n == LEGACY_METADATA_DIR || n == SCHEMAS_DIR || n.starts_with(SCRATCH_PREFIX)
    })
}

pub(crate) fn children(path: &Path) -> Result<Vec<PathBuf>> {
    let iter = match fs::read_dir(path) {
        Ok(iter) => iter,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot list {}", path.display())),
    };
    let mut paths = iter
        .map(|entry| entry.map(|e| e.path()))
        .collect::<io::Result<Vec<_>>>()?;
    paths.sort();
    Ok(paths)
}

pub(crate) fn exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn read_item(path: PathBuf) -> TrashItem {
    let parsed = (|| -> Result<TrashMetadata> {
        let metadata_path = path.join(METADATA_NAME);
        anyhow::ensure!(
            fs::symlink_metadata(&metadata_path)?.is_file(),
            "metadata must be a regular file"
        );
        let mut value: serde_json::Value = serde_json::from_reader(File::open(metadata_path)?)?;
        anyhow::ensure!(
            value["schema_version"].as_u64() == Some(u64::from(TRASH_SCHEMA_VERSION)),
            "unsupported trash metadata version"
        );
        // An invalid reason must not prevent recovery with valid path mappings.
        if let Some(reason) = value.as_object_mut().and_then(|v| v.get_mut("reason")) {
            if serde_json::from_value::<Option<TrashReason>>(reason.clone()).is_err() {
                warn!("ignoring invalid trash reason for {}", path.display());
                *reason = serde_json::Value::Null;
            }
        }
        let metadata: TrashMetadata = serde_json::from_value(value)?;
        metadata.validate_paths()?;
        Ok(metadata)
    })();
    let mut metadata = parsed.unwrap_or_else(|e| {
        warn!("cannot read trash metadata for {}: {e:#}", path.display());
        TrashMetadata::empty()
    });
    let primary = metadata
        .session_path
        .as_ref()
        .and_then(|p| regular_beneath(&path, p));
    let primary = primary.or_else(|| {
        let (primary, agent) = infer_primary(&path.join("payload"), metadata.agent)?;
        // Browsing an inferred transcript must never invent a restore mapping.
        metadata.session_path = None;
        metadata.agent = Some(agent);
        Some(primary)
    });
    TrashItem {
        path,
        primary,
        metadata,
        legacy_copies: None,
    }
}

fn infer_primary(base: &Path, known_agent: Option<Agent>) -> Option<(PathBuf, Agent)> {
    if !fs::symlink_metadata(base).is_ok_and(|m| m.is_dir()) {
        return None;
    }
    let mut candidates = Vec::new();
    let brain = base.join("brain");
    if fs::symlink_metadata(&brain).is_ok_and(|m| m.is_dir()) {
        for dir in children(&brain).ok()? {
            let transcript = dir.join(".system_generated/logs/transcript.jsonl");
            if let Some(path) = regular_beneath(base, transcript.strip_prefix(base).ok()?) {
                candidates.push((path, Agent::Antigravity));
            }
        }
    }
    for path in children(base).ok()? {
        if let Some(agent) = infer_file_agent(&path, known_agent) {
            candidates.push((path, agent));
        }
    }
    candidates.retain(|(_, agent)| known_agent.is_none_or(|known| known == *agent));
    (candidates.len() == 1).then(|| candidates.remove(0))
}

fn regular_beneath(base: &Path, relative: &Path) -> Option<PathBuf> {
    let mut path = base.to_path_buf();
    let mut parts = relative.components().peekable();
    while let Some(std::path::Component::Normal(part)) = parts.next() {
        path.push(part);
        let meta = fs::symlink_metadata(&path).ok()?;
        if parts.peek().is_none() {
            return meta.is_file().then_some(path);
        }
        if !meta.is_dir() {
            return None;
        }
    }
    None
}

fn infer_file_agent(path: &Path, known_agent: Option<Agent>) -> Option<Agent> {
    if !fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    if name.ends_with(".jsonl.zst") {
        return Some(Agent::Codex);
    }
    if !name.ends_with(".jsonl") {
        return None;
    }
    if let Some(agent @ (Agent::Codex | Agent::Claude)) = known_agent {
        return Some(agent);
    }
    let reader = BufReader::new(File::open(path).ok()?);
    for (i, line) in reader.lines().take(32).enumerate() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line.ok()?) else {
            continue;
        };
        if value["type"] == "session_meta"
            || (i == 0
                && value.get("type").is_none()
                && value["id"].is_string()
                && value["timestamp"].is_string())
        {
            return Some(Agent::Codex);
        }
        if value["sessionId"].is_string() {
            return Some(Agent::Claude);
        }
    }
    name.starts_with("rollout-").then_some(Agent::Codex)
}

fn legacy_entries(path: &Path) -> Result<BTreeMap<String, serde_json::Value>> {
    let mut entries = BTreeMap::new();
    if !exists(path)? {
        return Ok(entries);
    }
    anyhow::ensure!(
        fs::symlink_metadata(path)?.is_file(),
        "legacy catalog is not a file"
    );
    // Read bytes per line so a bad UTF-8 record cannot hide later good records.
    for (i, line) in BufReader::new(File::open(path)?).split(b'\n').enumerate() {
        let line = line?;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(e) => {
                warn!(
                    "invalid legacy trash record {}:{}: {e}",
                    path.display(),
                    i + 1
                );
                continue;
            }
        };
        if let Some(name) = value["nm"].as_str() {
            if let Some(previous) = entries.get(name) {
                if previous != &value {
                    warn!("conflicting legacy trash records for {name}");
                    entries.insert(name.to_owned(), serde_json::Value::Null);
                }
            } else {
                entries.insert(name.to_owned(), value);
            }
        }
    }
    Ok(entries)
}

fn legacy_reasons(path: &Path) -> Result<serde_json::Value> {
    if !exists(path)? {
        return Ok(serde_json::Value::Null);
    }
    anyhow::ensure!(
        fs::symlink_metadata(path)?.is_file(),
        "legacy reason catalog is not a file"
    );
    Ok(
        serde_json::from_reader(File::open(path)?).unwrap_or_else(|e| {
            warn!("invalid legacy trash reasons {}: {e}", path.display());
            serde_json::Value::Null
        }),
    )
}

fn legacy_item(
    path: &Path,
    catalog: Option<&serde_json::Value>,
    reason: Option<&serde_json::Value>,
) -> Result<TrashItem> {
    let entry = catalog.unwrap_or(&serde_json::Value::Null);
    let provenance = reason.unwrap_or(&serde_json::Value::Null);
    let mut metadata = TrashMetadata::empty();
    let known_agent = entry["tn"].as_str().and_then(|s| Agent::from_str(s).ok());
    let reason_agent = provenance["agent"]
        .as_str()
        .and_then(|s| Agent::from_str(s).ok());
    let original = |v: &serde_json::Value| {
        v.as_str()
            .map(PathBuf::from)
            .filter(|p| ensure_original_path(p).is_ok())
    };
    let catalog_path = original(&entry["op"]);
    let reason_path = original(&provenance["original_path"]);
    let consistent = !catalog.is_some_and(serde_json::Value::is_null)
        && catalog_path
            .as_ref()
            .zip(reason_path.as_ref())
            .is_none_or(|(a, b)| a == b)
        && known_agent.zip(reason_agent).is_none_or(|(a, b)| a == b);
    let original = consistent.then(|| catalog_path.or(reason_path)).flatten();
    metadata.trashed_at = entry["ts"]
        .as_str()
        .or_else(|| provenance["trashed_at"].as_str())
        .filter(|s| chrono::DateTime::parse_from_rfc3339(s).is_ok())
        .map(str::to_owned);
    if consistent {
        metadata.reason = serde_json::from_value(provenance["reason"].clone()).unwrap_or_default();
    }

    let stat = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        stat.is_dir() || stat.is_file(),
        "unsupported legacy trash entry {}",
        path.display()
    );
    let mut copies = Vec::new();
    let mut primary = None;
    if stat.is_dir() {
        let inferred = infer_primary(path, Some(Agent::Antigravity));
        if let Some((transcript, agent)) = inferred {
            metadata.agent = Some(agent);
            let archived = AntigravityBundlePaths::from_transcript(&transcript)?;
            let orig = original
                .as_deref()
                .and_then(|p| AntigravityBundlePaths::from_transcript(p).ok())
                .filter(|p| p.session_id == archived.session_id);
            metadata.session_path = Some(Path::new("payload").join(transcript.strip_prefix(path)?));
            primary = Some(transcript.clone());
            let (_, parts) = native_payload(agent, &transcript)?;
            for part in parts {
                let suffix = part.strip_prefix(path)?;
                metadata.members.push(TrashMember {
                    stored_path: Path::new("payload").join(suffix),
                    original_path: orig.as_ref().map(|b| b.home.join(suffix)),
                });
            }
        }
        copies.push((path.to_path_buf(), PathBuf::from("payload")));
    } else {
        metadata.agent = infer_file_agent(path, known_agent.or(reason_agent));
        let original = original.filter(|p| {
            metadata
                .agent
                .is_some_and(|agent| is_native_filename(p, agent))
        });
        let name = original
            .as_deref()
            .unwrap_or(path)
            .file_name()
            .context("missing trash filename")?;
        let stored = Path::new("payload").join(name);
        if metadata.agent.is_some() {
            primary = Some(path.to_path_buf());
            metadata.session_path = Some(stored.clone());
        }
        metadata.members.push(TrashMember {
            stored_path: stored.clone(),
            original_path: original.clone(),
        });
        copies.push((path.to_path_buf(), stored.clone()));
        let sidecar = crate::summary::sidecar_path(path);
        if exists(&sidecar)? {
            let stored_path = crate::summary::sidecar_path(&stored);
            metadata.members.push(TrashMember {
                stored_path: stored_path.clone(),
                original_path: original.as_deref().map(crate::summary::sidecar_path),
            });
            copies.push((sidecar, stored_path));
        }
    }
    metadata.validate_paths()?;
    Ok(TrashItem {
        path: path.to_path_buf(),
        primary,
        metadata,
        legacy_copies: Some(copies),
    })
}

pub(crate) fn is_native_filename(path: &Path, agent: Agent) -> bool {
    path.extension().is_some_and(|e| e == "jsonl")
        || (agent == Agent::Codex
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".jsonl.zst")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AntigravityBundlePaths {
    pub session_id: String,
    pub home: PathBuf,
    pub conversation_dir: PathBuf,
    pub conversations_dir: PathBuf,
    pub transcript: PathBuf,
}

impl AntigravityBundlePaths {
    pub fn from_transcript(path: &Path) -> Result<Self> {
        let logs = named_parent(path, "transcript.jsonl", "logs")?;
        let generated = named_parent(logs, "logs", ".system_generated")?;
        let conversation_dir = generated
            .parent()
            .context("Antigravity transcript has no conversation directory")?;
        let brain = conversation_dir
            .parent()
            .filter(|parent| parent.file_name().and_then(|name| name.to_str()) == Some("brain"))
            .context("Antigravity conversation is not under a brain directory")?;
        let home = brain
            .parent()
            .context("Antigravity brain directory has no parent")?
            .to_path_buf();
        let session_id = conversation_dir
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .context("Antigravity conversation has no session ID")?
            .to_owned();

        Ok(Self {
            session_id,
            conversations_dir: home.join("conversations"),
            home,
            conversation_dir: conversation_dir.to_path_buf(),
            transcript: path.to_path_buf(),
        })
    }

    pub fn database_paths(&self) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        let base = format!("{}.db", self.session_id);
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let path = self.conversations_dir.join(format!("{base}{suffix}"));
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_file() => paths.push(path),
                Ok(_) => anyhow::bail!("invalid Antigravity database companion {}", path.display()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to inspect {}", path.display()))
                }
            }
        }
        Ok(paths)
    }
}

fn named_parent<'a>(path: &'a Path, name: &str, parent_name: &str) -> Result<&'a Path> {
    if path.file_name().and_then(|value| value.to_str()) != Some(name) {
        anyhow::bail!("expected {name} path, got {}", path.display());
    }
    path.parent()
        .filter(|parent| parent.file_name().and_then(|value| value.to_str()) == Some(parent_name))
        .with_context(|| format!("{name} is not under a {parent_name} directory"))
}
