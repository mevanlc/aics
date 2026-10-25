//! Bounded discovery records, independent of the TUI's saved preferences.
use std::path::PathBuf;

use serde::Serialize;

use crate::conversation::HistoryStatus;
use crate::index::SearchHit;
use crate::parse::{strip_project_docs_autodump_preamble, Agent, DerivationType, MessageRole};
use crate::search_projection::visible_message_content;
use crate::settings::DisplayOptions;

pub const DISCOVERY_DISPLAY: DisplayOptions = DisplayOptions {
    hide_project_docs_autodump: true,
    hide_skill_text_injection: true,
    hide_internal_context: true,
    hide_agent_thinking: true,
    hide_tool_results: true,
    hide_user_tool_results: true,
    ..DisplayOptions::SHOW_ALL
};

#[derive(Debug, Serialize)]
pub struct DiscoveryHit {
    pub session: DiscoverySession,
    pub snippet: String,
    pub score: f32,
    pub is_live: bool,
}

#[derive(Debug, Serialize)]
pub struct DiscoverySession {
    pub session_id: String,
    pub agent: Agent,
    pub project: String,
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub custom_title: Option<String>,
    pub modified_ts: u64,
    pub lines: usize,
    pub file_path: PathBuf,
    pub first_user_msg_content: String,
    pub derivation_type: DerivationType,
    pub is_sidechain: bool,
    pub trashed: bool,
    pub superseded_by: Option<String>,
    pub history_status: Option<HistoryStatus>,
}

impl DiscoveryHit {
    pub(crate) fn new(hit: SearchHit, options: DisplayOptions) -> Self {
        let session = hit.session;
        let first = strip_project_docs_autodump_preamble(&session.first_user_msg_content)
            .unwrap_or(&session.first_user_msg_content);
        let first = visible_message_content(MessageRole::User, first, None, options).unwrap_or("");
        Self {
            session: DiscoverySession {
                session_id: session.session_id,
                agent: session.agent,
                project: session.project,
                cwd: session.cwd,
                branch: session.branch,
                custom_title: session.custom_title.map(|title| compact_text(&title, 512)),
                modified_ts: session.modified_ts,
                lines: session.lines,
                file_path: session.file_path,
                first_user_msg_content: compact_text(first, 512),
                derivation_type: session.derivation_type,
                is_sidechain: session.is_sidechain,
                trashed: session.trashed,
                superseded_by: session.superseded_by,
                history_status: session.history.map(|history| history.status),
            },
            snippet: compact_text(&hit.snippet_html, 512),
            score: hit.score,
            is_live: hit.is_live,
        }
    }
}

/// Collapse whitespace and cap Unicode scalars, including the omission marker.
pub(crate) fn compact_text(text: &str, limit: usize) -> String {
    if limit == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut count = 0;
    let mut space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            space = !result.is_empty();
            continue;
        }
        for ch in space.then_some(' ').into_iter().chain(std::iter::once(ch)) {
            if count == limit {
                result.pop();
                result.push('…');
                return result;
            }
            result.push(ch);
            count += 1;
        }
        space = false;
    }
    result
}
