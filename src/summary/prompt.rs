//! Built-in default prompt used when the user has not overridden it.
//!
//! The JSONL file path is substituted for `{{jsonl_path}}` by the template
//! engine before the prompt is written to disk. Custom prompts supplied via
//! `Settings::summarize_prompt` may reference the same placeholder.

pub const DEFAULT_PROMPT: &str = r#"You are summarizing a saved AI coding assistant chat session.
The session is stored as JSONL at this absolute path:
{{jsonl_path}}

Read the entire file yourself. Produce a concise Markdown summary that
helps a developer triage this session later at a glance.

Structure the summary with these sections (keep each short):

## TL;DR
One or two sentences — what was the session about and what was the outcome.

## Goal
What the user was trying to accomplish.

## What happened
- Bullet list of the main actions, decisions, problems, and resolutions.

## Final state
What was working, broken, or left undone at the end of the session.

## Notable artifacts
- Files changed, commands run, URLs, error text — anything a future
  reader would want to grep for.

Constraints:
- Output ONLY the Markdown summary. No preamble, no closing remarks, no
  code fences around the whole document.
- Prefer concrete details (paths, errors, decisions) over abstractions.
- Keep the total under ~500 words.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_prompt_mentions_placeholder_and_is_nonempty() {
        assert!(DEFAULT_PROMPT.contains("{{jsonl_path}}"));
        assert!(DEFAULT_PROMPT.len() > 100);
    }
}
