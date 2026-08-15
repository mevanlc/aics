# Session summaries

AICS can display summaries produced by Claude Code and Codex CLI and can generate
its own Markdown summary for any indexed session. Provider-native summaries are
read-only inputs; AICS-generated summaries are stored next to the source session
as sidecar files.

## Summary sources

AICS distinguishes three sources:

- **AICS summary** — generated on request using the command and prompt configured
  in AICS, then stored in a `.aics-summary.md` sidecar.
- **Claude Auto-summary** — an autosummary embedded in a Claude Code session.
  AICS displays every autosummary found in the session.
- **Codex Auto-summary** — a Codex rollout summary read from
  `CODEX_HOME/memories/rollout_summaries/*.md` and matched to the session by
  thread ID.

The “Builtin summary” snippet mode refers to the provider-native Claude or Codex
summary. It does not mean an AICS summary generated with one of the command
template presets.

## Generating an AICS summary

There is no default summarizer command. Configure one before generating your
first summary:

1. Press `Ctrl+S` to open Settings.
2. Open **Edit session summarizer settings**.
3. Enter a command template and prompt, or press `Ctrl+T` to choose a command
   template.
4. Press `Ctrl+S` to save the summarizer draft, then save Settings.
5. Select a session, press `Enter` to open its actions, then press `s` or choose
   **Summarize session (AI)**.

Summary jobs run in the background so the TUI remains usable. Jobs are processed
serially. If you try to quit while summaries are running, AICS warns that
quitting will discard the in-flight work.

## Command templates

The `Ctrl+T` template picker can build a Bash or Zsh command for Claude Code or
Codex CLI. It also offers model and reasoning-effort choices. The selected model
and effort flags are resolved when the template is inserted, after which the
command remains fully editable.

AICS expands these placeholders when it runs the command:

| Placeholder | Value |
| --- | --- |
| `{{jsonl_path}}` | Absolute path to the session JSONL file |
| `{{jsonl_dir}}` | Directory containing the session file |
| `{{prompt_file}}` | Temporary file containing the expanded prompt |
| `{{output_file}}` | Temporary file where the command must write its Markdown result |
| `{{claude_command}}` | Configured Claude Code executable |
| `{{claude_args}}` | Configured Claude Code arguments |
| `{{codex_command}}` | Configured Codex CLI executable |
| `{{codex_args}}` | Configured Codex CLI arguments |
| `{{antigravity_command}}` | Configured Antigravity CLI executable |
| `{{antigravity_args}}` | Configured Antigravity CLI arguments |

`{{model_flag}}` and `{{effort_flag}}` appear only in the template picker. The
picker replaces them before inserting the command.

The command is run verbatim through the user's shell. Path-like placeholder
values are escaped for use inside single quotes; `{{claude_args}}`,
`{{codex_args}}`, and `{{antigravity_args}}` are inserted as raw shell fragments.
The command must write a non-empty result to `{{output_file}}`; writing only to
stdout is not sufficient. Unknown or unterminated placeholders cause the job to
fail instead of silently producing a malformed command.

## Prompt template

The default prompt asks the selected AI CLI to read the complete JSONL session
and produce a short Markdown report with TL;DR, goal, events, final state, and
notable-artifact sections. A custom prompt can use `{{jsonl_path}}` and
`{{jsonl_dir}}`.

The summarizer may send session content, local paths, commands, and other
sensitive transcript data to the configured AI provider. Review the command,
prompt, and provider policy before running it.

## Sidecar files and freshness

An AICS summary for `session.jsonl` is stored beside it as:

```text
session.jsonl.aics-summary.md
```

The sidecar contains readable Markdown plus frontmatter recording its schema,
source filename, generation time, backend, and a source fingerprint. AICS writes
the file atomically.

The fingerprint contains the source's non-empty line count and SHA-256 hash of
its last non-empty line. A matching fingerprint is shown as **FRESH**. If the
session changes, the existing summary remains visible but is marked **STALE**;
AICS does not regenerate it automatically.

## Display and snippet behavior

The preview shows every available summary before the session log, with each
source labeled separately and rendered as Markdown. The full-session viewer
includes the AICS sidecar summary before the conversation; provider-native
summaries remain available in the preview and session-card snippets.

Press `Ctrl+Y` in the session list to cycle the card snippet through:

1. Session content
2. AICS summary
3. Builtin provider summary

If the selected summary kind is unavailable, AICS falls back to the other
summary kind when possible.

## Failures and diagnostics

A summary job fails if the source file is missing, template expansion fails, the
command cannot be started, the command exits unsuccessfully, the output file is
missing, or the generated output is empty. The TUI reports the failure in its
status line.

Failures are also sent to the dedicated summary-error log, which remains durable
even when ordinary `RUST_LOG` filtering is disabled. See [Logging](logging.md)
for file locations, retention, and custom log4rs configuration.

[Back to the README.](../README.md#session-summaries)
