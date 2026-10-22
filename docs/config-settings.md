# Configuration and settings

Open the settings modal with `Ctrl+S`. Layout, display, and default-filter
preferences can also be changed from the main screen and filter modal.

## Settings file

Settings are stored at:

```text
~/.config/aics/settings.json
```

Set `AICS_CONFIG_ROOT` to override the configuration directory. This also
relocates other AICS configuration data, including `rules.js`, generated
`rules.d.ts`, logging configuration, and logs.

Writes to `settings.json` are atomic: AICS writes a temporary file and renames
it into place. If the file exists but cannot be parsed at startup, AICS moves it
to `settings.json.corrupt-<timestamp>`, uses defaults, and reports a warning on
stderr and in the TUI status line.

## Available settings

- `theme`: `lazygit`, `aics`, `sunset`, or `late`
- `claude_command` and `claude_args`: the Claude Code resume command and
  arguments; defaults are `claude` and `--dangerously-skip-permissions`
- `codex_command` and `codex_args`: the Codex CLI resume command and arguments;
  defaults are `codex` and `--yolo`
- `antigravity_command` and `antigravity_args`: the Antigravity CLI resume
  command and arguments; defaults are `agy` and
  `--dangerously-skip-permissions`
- `show_preview`: whether the preview panel is visible
- `preview_width_pct`: the preview panel's percentage width
- `session_separator`: separator shown between session messages
- `snippet_line_count`: number of lines shown in session-card snippets. The TUI
  requests enough text for the card width and line count, refreshing excerpts
  when more space becomes available. With no search query, previews continue
  beyond a short opening request into later visible messages. Long words and
  URLs wrap across rows rather than stopping the snippet early.
- `two_line_topbar`: show session titles and recorded Git branches in a second
  card header row; default `false`, toggled and saved with `Ctrl+O`. The snippet
  keeps its configured line count and omits the title prefix in this view.
  Codex branches follow its resume picker: state database metadata takes
  precedence over the rollout's recorded branch when available.
- `history_save_count`: maximum saved searches, default `100`; also editable in
  `Ctrl+S` as **Search History Count**. `0` clears and disables history.
- `history_save_dwell_ms`: milliseconds a main search query must remain active
  before automatic recording, default `1000`; JSON-only. `0` saves immediately.
- `summarize_command`: command used to generate a session summary
- `summarize_prompt`: prompt template supplied to the summarizer
- `display_options`: visibility of skill injection, tool calls, tool results,
  agent replies, agent thinking, user messages, project-document boilerplate, and
  internal context
- `default_filter`: saved startup scope, sort order, and search filters
- `viewer_filter_exclusion`: `ask` (default), `keep`, or `close`; controls what happens
  when updated filters exclude the session open in the full viewer
- `viewer_find_jump`: whether local Find jumps to matches while typing, default
  `true`; editable as **Find: jump while typing** in Settings. Leading `(?j)` or
  `(?-j)` in Find temporarily overrides it in either substring or regex mode.

Unknown theme names fall back to `lazygit` without discarding the other settings
in the file.

`default_filter.visibility_search` selects `visible` (default), `all`, or `hidden`
for interactive searches. Missing values, including in existing settings, use
Visible. Change **Search content** (`v`) in `Ctrl+F`; Enter applies it and `Ctrl+S`
also saves it. Query modifiers override the selection temporarily, without
changing the saved preference. JSON/export modes ignore saved defaults.

`display_options.hide_agent_thinking` defaults to `true`, including when the key
is absent from existing settings. The **Agent Thinking** visibility toggle in
`Ctrl+F` (mnemonic `8`) shows or hides Claude, Codex, and Antigravity thinking
and reasoning blocks independently of **Agent Replies** (`7`).

`display_options.hide_internal_context` defaults to `true`, including when the
key is absent from existing settings. The **Internal Context** visibility toggle
in `Ctrl+F` (mnemonic `0`) shows or hides user messages consisting entirely of
`<codex_internal_context …>` or `<goal_context>` blocks. Messages with ordinary
prose outside the blocks or incomplete wrappers remain visible. Other generated
context, such as environment blocks, keeps its existing visibility behavior.
It also hides recognized Antigravity task polling reminders, server restart
notices, and stop-hook feedback.

Complete `<oai-mem-citation>` footers at the end of assistant messages also follow
**Internal Context**. Only the footer is hidden; the reply prose, heading, and
timestamp remain visible. Showing the footer requires both Internal Context and
Agent Replies to be on. Citation-only messages disappear when their footer is
hidden. Quoted/code examples, incomplete wrappers, and blocks followed by prose
remain visible. Original session content is retained for full exports and
troubleshooting.

Antigravity generated records use the existing visibility toggles:

| Toggle | Antigravity content |
| --- | --- |
| AGENTS.md/CLAUDE.md | Injected directory rules |
| Tool Results | Task completion/cancellation/stall output and execution errors |
| Internal Context | Polling reminders, restart notices, and hook feedback |
| Agent Replies | Worker messages and compaction checkpoints |
| Agent Thinking | Planner thinking blocks |

Original System/Summary roles are preserved. Unknown or malformed provider
envelopes remain visible. User metadata/settings wrappers are still omitted
before filtering; changing visibility cannot recover them.

## Search history

History lives in `search_history.json` beside `settings.json`, including when
`AICS_CONFIG_ROOT` is set. It is a JSON array of `query` and `saved_at` entries;
`saved_at` is the UTC RFC3339 time the search was recorded. Saves sort entries
newest first and keep at most `history_save_count` unique searches. An exact
repeat refreshes the existing entry's timestamp. Query text is preserved;
blank queries are omitted.

A query records once after its dwell, including while viewing a session or
using another modal. Changing its text restarts the timer; moving the cursor,
changing filters, and browsing results do not. `Ctrl+R` records the current query
immediately before opening history. Recall starts a new dwell. Quitting does not
force an unfinished dwell to save.

Reducing the count through `Ctrl+S` prunes immediately. JSON changes apply on the
next TUI startup, which also prunes existing history. A count of zero empties an
existing history file and makes main-screen `Ctrl+R` inert. Increasing the count
cannot restore pruned searches.

History writes use an atomic replacement and a sibling lock file to merge
concurrent saves. Invalid entries are skipped with a warning; an entirely
corrupt file is preserved as `search_history.json.corrupt-<timestamp>` before
starting fresh. Disk errors are reported without closing the TUI.

## Viewer filter exclusion

The exclusion dialog's **Remember my choice** checkbox saves `keep` or `close` in
`viewer_filter_exclusion`. Escape never saves the choice, even if checked. This
preference is configured in `settings.json`, with no Settings-modal control.

To restore the dialog, set `"viewer_filter_exclusion": "ask"` or remove the property
before launching AICS. Unknown values also use `ask`.

See [Session summaries](session-summaries.md) for summarizer setup, command
templates, placeholders, and sidecar behavior.

[Back to the README.](../README.md#configuration)
