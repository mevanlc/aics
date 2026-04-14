//! Short-term status entry shown inside the Search titlebar.
//!
//! An [`Entry`] is a transient success/failure message (e.g. "deleted X",
//! "summary ready for Y") that auto-expires after [`AUTO_HIDE`]. Continuous
//! state indicators (like "summarizing N") are rendered separately from the
//! in-flight counter; they do not use this type.

use std::time::{Duration, Instant};

pub const AUTO_HIDE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub enum EntryKind {
    Completed,
    Failed,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub kind: EntryKind,
    pub label: String,
    pub at: Instant,
}

impl Entry {
    pub fn completed(label: impl Into<String>) -> Self {
        Self {
            kind: EntryKind::Completed,
            label: label.into(),
            at: Instant::now(),
        }
    }

    pub fn failed(label: impl Into<String>) -> Self {
        Self {
            kind: EntryKind::Failed,
            label: label.into(),
            at: Instant::now(),
        }
    }

    pub fn expired(&self) -> bool {
        self.at.elapsed() >= AUTO_HIDE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_entry_is_not_expired() {
        let e = Entry::completed("done");
        assert!(!e.expired());
    }

    #[test]
    fn entry_expires_after_window() {
        let mut e = Entry::completed("done");
        e.at = Instant::now()
            .checked_sub(AUTO_HIDE + Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        assert!(e.expired());
    }

    #[test]
    fn failed_entry_expires_same_way() {
        let mut e = Entry::failed("boom");
        assert!(!e.expired());
        e.at = Instant::now()
            .checked_sub(AUTO_HIDE + Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        assert!(e.expired());
    }
}
