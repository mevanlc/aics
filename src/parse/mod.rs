pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod codex_summary;
pub mod session;
pub mod tool_format;

pub use claude::parse_claude_session_file;
pub use codex::parse_codex_session_file;
pub(crate) use codex::parse_codex_session_meta_lineage_file;
pub use session::{
    decode_claude_project_dir, decode_claude_project_from_path, default_project_for_cwd,
    is_contextual_user_message_content, is_project_docs_autodump, is_skill_text_injection,
    normalize_session_path, strip_project_docs_autodump_preamble, Agent, CodexUserTurn,
    DerivationType, ExecStatus, MessageRole, PatchFile, PatchOp, PlanItem, PlanItemStatus,
    RuntimeMetrics, Session, SessionCell, SessionInfo, SessionLineage, SessionMessage, ToolStatus,
    TrailingAbortedTurn,
};

use anyhow::Result;
use std::path::Path;

use crate::scan::SessionFile;

pub fn parse_session_file(agent: Agent, path: impl AsRef<Path>) -> Result<Option<Session>> {
    match agent {
        Agent::Claude => parse_claude_session_file(path),
        Agent::Codex => parse_codex_session_file(path),
        Agent::Antigravity => parse_antigravity_session_file(path),
    }
}

pub fn parse_scanned_session_file(file: &SessionFile) -> Result<Option<Session>> {
    match file.agent {
        Agent::Claude => parse_claude_session_file(&file.path),
        Agent::Codex => parse_codex_session_file(&file.path),
        Agent::Antigravity => antigravity::parse_antigravity_session(file),
    }
}
pub use antigravity::parse_antigravity_session_file;
