//! Logical Codex conversations and their immutable, bounded history sources.
//!
//! A source manifest describes what to read, not what may be removed. File
//! ownership and removal remain the responsibility of the lifecycle graph.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::parse::{Agent, Session};
use crate::scan::{SessionFile, SessionRoots};

mod reader;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryStatus {
    #[default]
    Complete,
    Incomplete,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionHistory {
    pub status: HistoryStatus,
    pub issues: Vec<String>,
    pub selected_rollout_id: String,
    pub source_paths: Vec<PathBuf>,
}

impl SessionHistory {
    pub fn is_complete(&self) -> bool {
        self.status == HistoryStatus::Complete
    }

    pub fn notice(&self) -> Option<String> {
        (!self.is_complete()).then(|| {
            format!(
                "History {}: {}",
                if self.status == HistoryStatus::Ambiguous {
                    "unresolved"
                } else {
                    "incomplete"
                },
                self.issues.join("; ")
            )
        })
    }

    fn issue(&mut self, issue: String) {
        if self.is_complete() {
            self.status = HistoryStatus::Incomplete;
        }
        if self.issues.len() < 16 && !self.issues.contains(&issue) {
            self.issues.push(issue);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistorySegment {
    pub path: PathBuf,
    pub rollout_id: String,
    pub start_ordinal: Option<u64>,
    pub end_ordinal_exclusive: Option<u64>,
    pub end_byte_offset: Option<u64>,
    pub paginated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SourceStamp {
    path: PathBuf,
    size: u64,
    modified_ns: u128,
}

impl SourceStamp {
    fn read(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            size: metadata.len(),
            modified_ns: metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos(),
        })
    }

    fn is_current(&self) -> bool {
        Self::read(&self.path).is_ok_and(|current| current == *self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSource {
    pub thread_id: String,
    pub custom_title: Option<String>,
    pub selected_path: PathBuf,
    pub segments: Vec<HistorySegment>,
    pub owned_paths: Vec<PathBuf>,
    pub history: SessionHistory,
    pub signature: u64,
    codex_home: PathBuf,
    selected_db_path: Option<PathBuf>,
    stamps: Vec<SourceStamp>,
}

impl ConversationSource {
    pub fn parse(&self) -> Result<Option<Session>> {
        reader::parse(self)
    }

    pub fn write_jsonl(&self, output: impl std::io::Write) -> Result<usize> {
        reader::write_jsonl(self, output)
    }

    pub fn summary_fingerprint(&self, line_count: usize) -> crate::summary::Fingerprint {
        use std::fmt::Write;
        let mut hash = String::with_capacity(64);
        for byte in Sha256::digest(self.signature.to_be_bytes()) {
            let _ = write!(&mut hash, "{byte:02x}");
        }
        crate::summary::Fingerprint {
            line_count,
            last_line_sha256: hash,
        }
    }

    /// Check both transcript inputs and Codex's current selection. A revert can
    /// change the selected page without modifying any transcript file.
    pub fn is_current(&self) -> bool {
        if !self.history.is_complete() || !self.stamps.iter().all(SourceStamp::is_current) {
            return false;
        }
        match crate::parse::codex::read_codex_thread_names(&self.codex_home.join("sessions")) {
            Ok(names) if names.get(&self.thread_id) == self.custom_title.as_ref() => {}
            _ => return false,
        }
        match read_selections(&self.codex_home, Some(&self.thread_id)) {
            Ok(selections) => {
                selections.get(&self.thread_id).map(|s| &s.path) == self.selected_db_path.as_ref()
            }
            Err(_) => self.selected_db_path.is_none(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct HistoryBase {
    rollout_id: String,
    end_ordinal_exclusive: Option<u64>,
    end_byte_offset: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RolloutMetadata {
    thread_id: String,
    rollout_id: String,
    base: Option<HistoryBase>,
    paginated: bool,
    ordinal: Option<u64>,
    issue: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CachedRollout {
    stamp: SourceStamp,
    original_path: Option<PathBuf>,
    metadata: Option<RolloutMetadata>,
}

pub(crate) type HeaderCache = BTreeMap<String, CachedRollout>;

struct Rollout {
    file: SessionFile,
    metadata: RolloutMetadata,
    stamp: SourceStamp,
    eligible: bool,
}

#[derive(Debug)]
struct Selection {
    path: PathBuf,
    paginated: bool,
}

fn read_selections(home: &Path, thread: Option<&str>) -> Result<HashMap<String, Selection>> {
    let path = home.join("state_5.sqlite");
    if !path.try_exists()? {
        return Ok(HashMap::new());
    }
    let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("cannot read {}", path.display()))?;
    connection.busy_timeout(Duration::from_millis(250))?;
    let has_mode = connection
        .prepare("PRAGMA table_info(threads)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|column| column == "history_mode");
    let query = if has_mode {
        "SELECT id, rollout_path, history_mode FROM threads"
    } else {
        "SELECT id, rollout_path, 'legacy' FROM threads"
    };
    let query = if thread.is_some() {
        format!("{query} WHERE id = ?1")
    } else {
        query.to_owned()
    };
    let mut statement = connection.prepare(&query)?;
    let rows = statement.query_map(rusqlite::params_from_iter(thread), |row| {
        let path = PathBuf::from(row.get::<_, String>(1)?);
        Ok((
            row.get::<_, String>(0)?,
            Selection {
                path: if path.is_absolute() {
                    path
                } else {
                    home.join(path)
                },
                paginated: row.get::<_, String>(2)? == "paginated",
            },
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn metadata_line(path: &Path) -> Result<Option<Value>> {
    for (number, line) in crate::lifecycle::open_rollout(path)?.lines().enumerate() {
        let line = line?;
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value["type"] == "session_meta" {
            return Ok(Some(value));
        }
        if number == 0 && value.get("type").is_none() && value["id"].is_string() {
            return Ok(Some(serde_json::json!({
                "type": "session_meta", "timestamp": value["timestamp"], "payload": value
            })));
        }
    }
    Ok(None)
}

fn read_metadata(file: &SessionFile) -> Result<Option<RolloutMetadata>> {
    let Some(value) = metadata_line(&file.path)? else {
        return Ok(None);
    };
    let payload = &value["payload"];
    let Some(id) = payload["id"].as_str().filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let base = payload.get("history_base").filter(|value| !value.is_null());
    let mut issue = None;
    let base = base.and_then(|base| {
        let Some(id) = base["thread_id"].as_str().filter(|id| !id.is_empty()) else {
            issue = Some("history_base has no rollout ID".into());
            return None;
        };
        Some(HistoryBase {
            rollout_id: id.to_owned(),
            end_ordinal_exclusive: base["end_ordinal_exclusive"].as_u64(),
            end_byte_offset: base["end_byte_offset"].as_u64(),
        })
    });
    Ok(Some(RolloutMetadata {
        thread_id: id.to_owned(),
        rollout_id: crate::lifecycle::rollout_id(
            file.original_path.as_deref().unwrap_or(&file.path),
            id,
        ),
        paginated: payload["history_mode"] == "paginated" || base.is_some(),
        base,
        ordinal: value["ordinal"].as_u64(),
        issue,
    }))
}

fn identity_path(path: &Path) -> PathBuf {
    let path = crate::lifecycle::resolved_path(path);
    #[cfg(windows)]
    let path = PathBuf::from(path.to_string_lossy().to_ascii_lowercase());
    if path.extension().is_some_and(|extension| extension == "zst") {
        path.with_extension("")
    } else {
        path
    }
}

fn selected_matches(file: &SessionFile, selected: &Path) -> bool {
    identity_path(file.original_path.as_deref().unwrap_or(&file.path)) == identity_path(selected)
}

fn archive_files(path: &Path, output: &mut Vec<SessionFile>) -> Result<()> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() && !entry.file_type()?.is_symlink() {
            archive_files(&entry.path(), output)?;
        } else if metadata.is_file() && entry.file_name().to_string_lossy().ends_with(".jsonl")
            || metadata.is_file() && entry.file_name().to_string_lossy().ends_with(".jsonl.zst")
        {
            output.push(SessionFile {
                path: entry.path(),
                agent: Agent::Codex,
                modified: metadata.modified().unwrap_or(UNIX_EPOCH),
                size: metadata.len(),
                trashed: false,
                original_path: None,
                companion_paths: Vec::new(),
                source_signature: 0,
                antigravity_metadata: None,
                conversation: None,
            });
        }
    }
    Ok(())
}

/// Resolve a scan into logical sources, retaining a separate cache of physical
/// headers. Both indexing and rules use this catalog before scheduling workers.
pub(crate) fn logical_files(
    roots: &SessionRoots,
    files: Vec<SessionFile>,
    previous: &HeaderCache,
) -> Result<(Vec<SessionFile>, HeaderCache)> {
    let home = codex_home(roots);
    let names = crate::parse::codex::read_codex_thread_names(&home.join("sessions"))
        .unwrap_or_else(|error| {
            log::warn!("Codex conversation names unavailable: {error:#}");
            Arc::new(HashMap::new())
        });
    let archive = home.join("archived_sessions");
    let selections = read_selections(home, None).unwrap_or_else(|error| {
        log::warn!("Codex current history selection unavailable: {error:#}");
        HashMap::new()
    });
    let mut physical = files
        .iter()
        .filter(|file| file.agent == Agent::Codex)
        .cloned()
        .collect::<Vec<_>>();
    let eligible_paths = physical
        .iter()
        .map(|file| file.path.clone())
        .collect::<HashSet<_>>();
    if let Err(error) = archive_files(&archive, &mut physical) {
        log::warn!("cannot discover archived Codex history: {error:#}");
    }
    physical.sort_by(|left, right| left.path.cmp(&right.path));
    physical.dedup_by(|left, right| left.path == right.path);
    let mut headers = HeaderCache::new();
    let mut rollouts = Vec::new();
    let mut grouped_paths = HashSet::new();
    for file in physical {
        // Trash can contain sessions belonging to a different configured home.
        let original = file.original_path.as_deref().unwrap_or(&file.path);
        if file.trashed
            && !identity_path(original).starts_with(identity_path(&roots.codex_sessions))
            && !identity_path(original).starts_with(identity_path(&archive))
        {
            continue;
        }
        let stamp = match SourceStamp::read(&file.path) {
            Ok(stamp) => stamp,
            Err(error) => {
                log::warn!("cannot inspect {}: {error:#}", file.path.display());
                continue;
            }
        };
        let key = file.path.to_string_lossy().into_owned();
        let cached = previous
            .get(&key)
            .filter(|cached| cached.stamp == stamp && cached.original_path == file.original_path);
        let metadata = match cached {
            Some(cached) => cached.metadata.clone(),
            None => match read_metadata(&file) {
                Ok(metadata) => metadata,
                Err(error) => {
                    log::warn!(
                        "cannot read Codex metadata {}: {error:#}",
                        file.path.display()
                    );
                    None
                }
            },
        };
        headers.insert(
            key,
            CachedRollout {
                stamp: stamp.clone(),
                original_path: file.original_path.clone(),
                metadata: metadata.clone(),
            },
        );
        if let Some(metadata) = metadata {
            let eligible = eligible_paths.contains(&file.path);
            grouped_paths.insert(file.path.clone());
            rollouts.push(Rollout {
                file,
                metadata,
                stamp,
                eligible,
            });
        }
    }
    let mut by_thread = BTreeMap::<String, Vec<usize>>::new();
    let mut by_rollout = HashMap::<String, Vec<usize>>::new();
    for (index, rollout) in rollouts.iter().enumerate() {
        by_rollout
            .entry(rollout.metadata.rollout_id.clone())
            .or_default()
            .push(index);
        if rollout.eligible {
            by_thread
                .entry(rollout.metadata.thread_id.clone())
                .or_default()
                .push(index);
        }
    }
    let mut logical = files
        .into_iter()
        .filter(|file| !grouped_paths.contains(&file.path))
        .collect::<Vec<_>>();
    for (thread, members) in by_thread {
        let selection = selections.get(&thread);
        let selected = selection.and_then(|selection| {
            members
                .iter()
                .copied()
                .find(|&i| selected_matches(&rollouts[i].file, &selection.path))
        });
        let authoritative_missing =
            selected.is_none() && selection.is_some_and(|selection| selection.paginated);
        let mut candidates = match selected {
            Some(index) => vec![index],
            None => members
                .iter()
                .copied()
                .filter(|index| {
                    // A file referenced by another version of this same thread is
                    // an ancestor, not a candidate current version.
                    !members.iter().any(|&other| {
                        other != *index
                            && rollouts[other].metadata.base.as_ref().is_some_and(|base| {
                                base.rollout_id == rollouts[*index].metadata.rollout_id
                            })
                    })
                })
                .collect::<Vec<_>>(),
        };
        // Cyclic histories still need a visible, inspectable diagnostic entry.
        if candidates.is_empty() {
            candidates.clone_from(&members);
        }
        candidates.sort_by_key(|&i| (rollouts[i].file.trashed, rollouts[i].file.path.clone()));
        candidates.dedup_by(|left, right| {
            identity_path(
                rollouts[*left]
                    .file
                    .original_path
                    .as_deref()
                    .unwrap_or(&rollouts[*left].file.path),
            ) == identity_path(
                rollouts[*right]
                    .file
                    .original_path
                    .as_deref()
                    .unwrap_or(&rollouts[*right].file.path),
            )
        });
        let ambiguous = candidates.len() > 1 || authoritative_missing;
        for candidate in candidates {
            let tip = &rollouts[candidate];
            let mut history = SessionHistory {
                status: if ambiguous {
                    HistoryStatus::Ambiguous
                } else {
                    HistoryStatus::Complete
                },
                issues: if authoritative_missing {
                    vec![
                        "Codex's selected rollout is unavailable; this is an unresolved version"
                            .into(),
                    ]
                } else if ambiguous {
                    vec![
                        "multiple current history versions; Codex's selection is unavailable"
                            .into(),
                    ]
                } else {
                    Vec::new()
                },
                selected_rollout_id: tip.metadata.rollout_id.clone(),
                source_paths: Vec::new(),
            };
            let mut segments = Vec::new();
            let mut stamps = Vec::new();
            let mut next = candidate;
            let mut cutoff = None::<HistoryBase>;
            let mut visited = HashSet::new();
            loop {
                let rollout = &rollouts[next];
                if !visited.insert(rollout.metadata.rollout_id.clone()) {
                    history.issue("cycle in history references".into());
                    break;
                }
                if let Some(issue) = &rollout.metadata.issue {
                    history.issue(issue.clone());
                }
                let identities = by_rollout[&rollout.metadata.rollout_id]
                    .iter()
                    .map(|&i| {
                        identity_path(
                            rollouts[i]
                                .file
                                .original_path
                                .as_deref()
                                .unwrap_or(&rollouts[i].file.path),
                        )
                    })
                    .collect::<HashSet<_>>();
                if identities.len() > 1 {
                    history.issue(format!(
                        "conflicting files claim rollout {}",
                        rollout.metadata.rollout_id
                    ));
                }
                if !tip.file.trashed && rollout.file.trashed {
                    history.issue(format!(
                        "required history is in trash: {}",
                        rollout.metadata.rollout_id
                    ));
                }
                let start_ordinal = rollout
                    .metadata
                    .ordinal
                    .and_then(|ordinal| ordinal.checked_add(1));
                if let Some(end) = &cutoff {
                    if end.end_ordinal_exclusive.is_none_or(|end| {
                        end == 0 || start_ordinal.is_some_and(|start| end < start)
                    }) || end.end_byte_offset.is_none_or(|offset| offset == 0)
                    {
                        history.issue(format!(
                            "invalid or missing history cutoff for {}",
                            rollout.metadata.rollout_id
                        ));
                        // No cutoff is not permission to expose the full ancestor.
                        break;
                    }
                }
                segments.push(HistorySegment {
                    path: rollout.file.path.clone(),
                    rollout_id: rollout.metadata.rollout_id.clone(),
                    start_ordinal,
                    end_ordinal_exclusive: cutoff
                        .as_ref()
                        .and_then(|end| end.end_ordinal_exclusive),
                    end_byte_offset: cutoff.as_ref().and_then(|end| end.end_byte_offset),
                    paginated: rollout.metadata.paginated,
                });
                stamps.push(rollout.stamp.clone());
                let Some(base) = &rollout.metadata.base else {
                    break;
                };
                let Some(parents) = by_rollout.get(&base.rollout_id) else {
                    history.issue(format!("missing source rollout {}", base.rollout_id));
                    break;
                };
                let identities = parents
                    .iter()
                    .map(|&i| {
                        identity_path(
                            rollouts[i]
                                .file
                                .original_path
                                .as_deref()
                                .unwrap_or(&rollouts[i].file.path),
                        )
                    })
                    .collect::<HashSet<_>>();
                if identities.len() != 1 {
                    history.issue(format!("ambiguous source rollout {}", base.rollout_id));
                    break;
                }
                next = *parents
                    .iter()
                    .min_by_key(|&&i| {
                        (
                            rollouts[i].file.trashed,
                            rollouts[i]
                                .file
                                .path
                                .extension()
                                .is_some_and(|extension| extension == "zst"),
                        )
                    })
                    .expect("source candidates are not empty");
                cutoff = Some(base.clone());
            }
            segments.reverse();
            stamps.reverse();
            history.source_paths = segments
                .iter()
                .map(|segment| segment.path.clone())
                .collect();
            let mut source = ConversationSource {
                thread_id: thread.clone(),
                custom_title: names.get(&thread).cloned(),
                selected_path: tip.file.path.clone(),
                segments,
                owned_paths: members
                    .iter()
                    .map(|&i| rollouts[i].file.path.clone())
                    .collect(),
                history,
                signature: 0,
                codex_home: home.to_path_buf(),
                selected_db_path: selection.map(|selection| selection.path.clone()),
                stamps,
            };
            let digest = Sha256::digest(serde_json::to_vec(&source)?);
            source.signature =
                u64::from_be_bytes(digest[..8].try_into().expect("eight-byte SHA-256 prefix"));
            let mut file = tip.file.clone();
            file.size = source.stamps.iter().map(|stamp| stamp.size).sum();
            file.source_signature = source.signature;
            file.conversation = Some(Arc::new(source));
            logical.push(file);
        }
    }
    logical.sort_by(|left, right| left.path.cmp(&right.path));
    Ok((logical, headers))
}

pub(crate) fn codex_home(roots: &SessionRoots) -> &Path {
    roots
        .live_sessions
        .codex_writer_locks_dir
        .as_deref()
        .and_then(Path::parent)
        .or_else(|| roots.codex_sessions.parent())
        .unwrap_or(&roots.codex_sessions)
}

#[cfg(test)]
mod tests;
