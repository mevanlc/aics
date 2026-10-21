use std::io::{self, BufRead};

use anyhow::{bail, Result};
use serde_json::Value;

use super::{metadata_line, ConversationSource, SessionHistory};
use crate::parse::{codex::parse_codex_session_lines, Session};

pub(super) fn write_jsonl(
    source: &ConversationSource,
    mut output: impl io::Write,
) -> Result<usize> {
    if !source.is_current() {
        bail!("conversation history is incomplete or has changed");
    }
    let mut lines = HistoryLines::new(source)?;
    let mut count = 0;
    for line in &mut lines {
        let line = line?;
        output.write_all(line.as_bytes())?;
        if !line.ends_with('\n') {
            output.write_all(b"\n")?;
        }
        count += 1;
    }
    if let Some(notice) = lines.history.notice() {
        bail!("{notice}");
    }
    if !source.is_current() {
        bail!("conversation history changed while it was read");
    }
    Ok(count)
}

pub(super) fn parse(source: &ConversationSource) -> Result<Option<Session>> {
    let mut lines = HistoryLines::new(source)?;
    let mut session = parse_codex_session_lines(
        &source.selected_path,
        &mut lines,
        !source.history.is_complete(),
    )?;
    if session.is_none() && !lines.history.is_complete() {
        lines = HistoryLines::new(source)?;
        session = parse_codex_session_lines(&source.selected_path, &mut lines, true)?;
    }
    if lines.history.is_complete() && !source.is_current() {
        lines
            .history
            .issue("history changed since this source snapshot was created".into());
    }
    if let Some(session) = &mut session {
        // The logical identity and title belong to the selected conversation,
        // even when every user-facing record is inherited from another thread.
        session.session_id.clone_from(&source.thread_id);
        session.custom_title.clone_from(&source.custom_title);
        session.branch.clone_from(&source.branch);
        session.lineage.rollout_id = Some(source.history.selected_rollout_id.clone());
        session.history = Some(lines.history);
    }
    Ok(session)
}

struct HistoryLines<'a> {
    source: &'a ConversationSource,
    header: Option<String>,
    segment: usize,
    input: Option<Box<dyn BufRead>>,
    offset: u64,
    next_ordinal: Option<u64>,
    history: SessionHistory,
}

impl<'a> HistoryLines<'a> {
    fn new(source: &'a ConversationSource) -> Result<Self> {
        let mut history = source.history.clone();
        let header = match metadata_line(&source.selected_path) {
            Ok(Some(header)) => header,
            result => {
                history.issue(format!(
                    "cannot read selected session metadata: {}",
                    match result {
                        Err(error) => format!("{error:#}"),
                        _ => "missing header".into(),
                    }
                ));
                serde_json::json!({"type":"session_meta","payload":{"id":source.thread_id}})
            }
        };
        Ok(Self {
            source,
            header: Some(serde_json::to_string(&header)?),
            segment: 0,
            input: None,
            offset: 0,
            next_ordinal: None,
            history,
        })
    }

    fn finish_segment(&mut self) {
        let segment = &self.source.segments[self.segment];
        if segment
            .end_byte_offset
            .is_some_and(|end| self.offset != end)
        {
            self.history.issue(format!(
                "history cutoff is outside a complete record in {}",
                segment.rollout_id
            ));
        }
        if let (Some(expected), Some(actual)) = (segment.end_ordinal_exclusive, self.next_ordinal) {
            if actual != expected {
                self.history.issue(format!(
                    "history ordinal cutoff does not match {}",
                    segment.rollout_id
                ));
            }
        }
        self.input = None;
        self.segment += 1;
    }
}

impl Iterator for HistoryLines<'_> {
    type Item = io::Result<String>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(header) = self.header.take() {
            return Some(Ok(header));
        }
        loop {
            let segment = self.source.segments.get(self.segment)?;
            if self.input.is_none() {
                match crate::lifecycle::open_rollout(&segment.path) {
                    Ok(reader) => self.input = Some(reader),
                    Err(error) => {
                        self.history
                            .issue(format!("cannot read {}: {error:#}", segment.rollout_id));
                        self.segment += 1;
                        continue;
                    }
                }
                self.offset = 0;
                self.next_ordinal = None;
            }
            if segment.end_byte_offset == Some(self.offset) {
                self.finish_segment();
                continue;
            }
            let mut bytes = Vec::new();
            match self
                .input
                .as_mut()
                .expect("segment reader is open")
                .read_until(b'\n', &mut bytes)
            {
                Ok(0) => {
                    self.finish_segment();
                    continue;
                }
                Err(error) => {
                    self.history.issue(format!(
                        "unreadable history in {}: {error}",
                        segment.rollout_id
                    ));
                    self.input = None;
                    self.segment += 1;
                    continue;
                }
                Ok(_) => {}
            }
            let end = self.offset.saturating_add(bytes.len() as u64);
            if segment.end_byte_offset.is_some_and(|cutoff| end > cutoff) {
                self.finish_segment();
                continue;
            }
            self.offset = end;
            if !bytes.ends_with(b"\n") {
                self.history.issue(format!(
                    "unterminated history record in {}",
                    segment.rollout_id
                ));
            }
            if bytes.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let value: Value = match serde_json::from_slice(&bytes) {
                Ok(value) => value,
                Err(error) => {
                    self.history.issue(format!(
                        "malformed history record in {}: {error}",
                        segment.rollout_id
                    ));
                    continue;
                }
            };
            // State checkpoints are storage records, not conversation items.
            if value["record_type"] == "state" {
                continue;
            }
            if let Some(ordinal) = value["ordinal"].as_u64() {
                if self.next_ordinal.is_some_and(|expected| ordinal < expected) {
                    self.history.issue(format!(
                        "regressed history ordinal in {}",
                        segment.rollout_id
                    ));
                    continue;
                }
                self.next_ordinal = ordinal.checked_add(1);
                if segment
                    .end_ordinal_exclusive
                    .is_some_and(|cutoff| ordinal >= cutoff)
                {
                    self.history.issue(format!(
                        "record exceeds history ordinal cutoff in {}",
                        segment.rollout_id
                    ));
                    self.finish_segment();
                    continue;
                }
            } else if segment.paginated {
                self.history
                    .issue(format!("missing history ordinal in {}", segment.rollout_id));
            }
            if value["type"] == "session_meta"
                || value.get("type").is_none() && value["id"].is_string()
            {
                continue;
            }
            // Preserve the original record for the existing parser; one parser
            // instance spans every segment so cross-page tool pairs work.
            return Some(
                String::from_utf8(bytes)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            );
        }
    }
}
