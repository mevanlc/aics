//! Comprehensive Markdown reference manual for `aics`.
//! Emitted by `aics --manpage`.
//!
//! Embedded directly from `docs/command-line.md` at compile time so the CLI manual
//! and repository documentation never diverge.

pub fn render_manpage() -> &'static str {
    include_str!("../docs/command-line.md")
}
