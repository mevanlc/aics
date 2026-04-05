pub mod claude;
pub mod codex;
pub mod session;
pub mod tool_format;

pub use claude::parse_claude_session_file;
pub use codex::parse_codex_session_file;
pub use session::{
    decode_claude_project_dir, decode_claude_project_from_path, default_project_for_cwd,
    normalize_session_path, Agent, DerivationType, MessageRole, Session, SessionMessage,
};

use anyhow::Result;
use std::path::Path;

pub fn parse_session_file(agent: Agent, path: impl AsRef<Path>) -> Result<Option<Session>> {
    match agent {
        Agent::Claude => parse_claude_session_file(path),
        Agent::Codex => parse_codex_session_file(path),
    }
}
