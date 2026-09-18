use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::settings::{backup_corrupt_settings, write_atomic};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub query: String,
    pub saved_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct SearchHistory {
    path: PathBuf,
    pub entries: Vec<HistoryEntry>,
}

impl SearchHistory {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            entries: Vec::new(),
        }
    }

    /// Reload and prune, or save one query against the latest on-disk history.
    /// Never merge a stale snapshot: doing so would resurrect pruned entries.
    pub fn update(
        &mut self,
        query: Option<&str>,
        saved_at: DateTime<Utc>,
        limit: usize,
    ) -> Result<Option<String>> {
        let entry = query
            .filter(|query| limit > 0 && !query.trim().is_empty())
            .map(|query| HistoryEntry {
                query: query.to_owned(),
                saved_at,
            });
        // Keep recall usable when disk access fails.
        update_entries(&mut self.entries, entry.clone(), limit);
        if !self.path.exists() && entry.is_none() {
            return Ok(None);
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Lock a stable sibling, since atomic replacement changes the JSON inode.
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.path.with_extension("json.lock"))?;
        lock.lock().context("failed to lock search history")?;

        let (mut entries, warning) = self.read_entries()?;
        let before = entries.clone();
        update_entries(&mut entries, entry, limit);
        if entries != before || warning.is_some() {
            write_atomic(&self.path, &serde_json::to_string_pretty(&entries)?)?;
        }
        self.entries = entries;
        Ok(warning)
    }

    fn read_entries(&self) -> Result<(Vec<HistoryEntry>, Option<String>)> {
        let contents = match fs::read_to_string(&self.path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Vec::new(), None));
            }
            Err(err) => return Err(err).context("failed to read search history"),
        };
        let values: Vec<serde_json::Value> = match serde_json::from_str(&contents) {
            Ok(values) => values,
            Err(err) => {
                let backup = backup_corrupt_settings(&self.path)?;
                return Ok((
                    Vec::new(),
                    Some(format!(
                        "search history reset ({err}); previous file kept at {}",
                        backup.display()
                    )),
                ));
            }
        };
        let mut skipped = 0;
        let entries = values
            .into_iter()
            .filter_map(|value| match serde_json::from_value(value) {
                Ok(entry) => Some(entry),
                Err(_) => {
                    skipped += 1;
                    None
                }
            })
            .collect();
        Ok((
            entries,
            (skipped > 0).then(|| format!("skipped {skipped} invalid search history entries")),
        ))
    }
}

fn update_entries(entries: &mut Vec<HistoryEntry>, entry: Option<HistoryEntry>, limit: usize) {
    if let Some(entry) = entry {
        entries.retain(|old| old.query != entry.query);
        entries.push(entry);
    }
    entries.sort_by(|a, b| b.saved_at.cmp(&a.saved_at).then(a.query.cmp(&b.query)));
    let mut seen = HashSet::new();
    entries.retain(|entry| !entry.query.trim().is_empty() && seen.insert(entry.query.clone()));
    entries.truncate(limit);
}

/// The recording deadline is independent of search dispatch and UI focus.
#[derive(Debug)]
pub struct HistoryDwell {
    active_at: Instant,
    recorded: bool,
}

impl HistoryDwell {
    pub fn new(now: Instant) -> Self {
        Self {
            active_at: now,
            recorded: false,
        }
    }

    pub fn remaining(&self, now: Instant, dwell_ms: u64) -> Option<Duration> {
        (!self.recorded).then(|| {
            Duration::from_millis(dwell_ms).saturating_sub(now.duration_since(self.active_at))
        })
    }

    pub fn mark_recorded(&mut self) {
        // Also consume failed automatic attempts, avoiding an idle retry loop.
        self.recorded = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(seconds, 0).unwrap()
    }

    #[test]
    fn history_round_trip_refreshes_exact_duplicates_and_prunes_by_timestamp() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("search_history.json");
        let mut history = SearchHistory::new(path.clone());
        history.update(None, time(0), 100).unwrap();
        history.update(Some("  "), time(0), 100).unwrap();
        assert!(!path.exists());
        for (query, seconds) in [("alpha", 2), ("βeta", 1), ("alpha", 3), (" alpha ", 4)] {
            history.update(Some(query), time(seconds), 100).unwrap();
        }
        let mut loaded = SearchHistory::new(path.clone());
        loaded.update(None, time(5), 2).unwrap();
        assert_eq!(
            loaded
                .entries
                .iter()
                .map(|e| e.query.as_str())
                .collect::<Vec<_>>(),
            [" alpha ", "alpha"]
        );
        assert_eq!(loaded.entries[1].saved_at, time(3));
        let json: Vec<HistoryEntry> =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json, loaded.entries);
        loaded.update(None, time(6), 0).unwrap();
        loaded.update(Some("ignored"), time(7), 0).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "[]");
    }

    #[test]
    fn concurrent_writers_merge_without_resurrecting_pruned_entries() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("search_history.json");
        std::thread::scope(|scope| {
            for index in 0..12 {
                let path = path.clone();
                scope.spawn(move || {
                    SearchHistory::new(path)
                        .update(Some(&format!("query {index}")), time(index), 100)
                        .unwrap();
                });
            }
        });
        let mut stale = SearchHistory::new(path.clone());
        stale.update(None, time(20), 100).unwrap();
        assert_eq!(stale.entries.len(), 12);
        SearchHistory::new(path).update(None, time(20), 1).unwrap();
        stale.update(Some("new"), time(21), 100).unwrap();
        assert_eq!(stale.entries.len(), 2);
        assert_eq!(stale.entries[1].query, "query 11");
    }

    #[test]
    fn malformed_entries_are_skipped_and_corrupt_files_are_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("search_history.json");
        fs::write(
            &path,
            r#"[{"query":"valid","saved_at":"2026-09-16T00:00:00Z"},{"query":"invalid"}]"#,
        )
        .unwrap();
        let mut history = SearchHistory::new(path.clone());
        assert!(history
            .update(None, time(0), 100)
            .unwrap()
            .unwrap()
            .contains("skipped 1"));
        assert_eq!(history.entries.len(), 1);
        fs::write(&path, "broken").unwrap();
        let warning = history.update(Some("new"), time(1), 100).unwrap().unwrap();
        assert!(warning.contains("previous file kept"));
        let backup = fs::read_dir(temp.path())
            .unwrap()
            .flatten()
            .find(|f| f.file_name().to_string_lossy().contains(".corrupt-"))
            .unwrap();
        assert_eq!(fs::read_to_string(backup.path()).unwrap(), "broken");
        assert_eq!(history.entries[0].query, "new");
    }

    #[test]
    fn failed_write_keeps_in_memory_history_and_does_not_truncate_disk_history() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("search_history.json");
        let mut history = SearchHistory::new(path.clone());
        history.update(Some("old"), time(1), 100).unwrap();
        let original = fs::read_to_string(&path).unwrap();
        fs::create_dir(
            path.with_file_name(format!("search_history.json.tmp-{}", std::process::id())),
        )
        .unwrap();
        assert!(history.update(Some("new"), time(2), 100).is_err());
        assert_eq!(history.entries[0].query, "new");
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    fn dwell_deadline_is_consumed_once_and_can_restart() {
        let start = Instant::now();
        let mut dwell = HistoryDwell::new(start);
        assert_eq!(
            dwell.remaining(start + Duration::from_millis(999), 1000),
            Some(Duration::from_millis(1))
        );
        assert_eq!(
            dwell.remaining(start + Duration::from_millis(1000), 1000),
            Some(Duration::ZERO)
        );
        dwell.mark_recorded();
        assert_eq!(dwell.remaining(start + Duration::from_secs(20), 1000), None);
        let dwell = HistoryDwell::new(start);
        assert_eq!(dwell.remaining(start, 0), Some(Duration::ZERO));
    }
}
