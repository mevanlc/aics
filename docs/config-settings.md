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
- `snippet_line_count`: number of lines shown in session-card snippets
- `history_save_count`: maximum saved searches, default `100`; also editable in
  `Ctrl+S` as **Search History Count**. `0` clears and disables history.
- `history_save_dwell_ms`: milliseconds a main search query must remain active
  before automatic recording, default `1000`; JSON-only. `0` saves immediately.
- `summarize_command`: command used to generate a session summary
- `summarize_prompt`: prompt template supplied to the summarizer
- `display_options`: visibility of skill injection, tool calls, tool results,
  agent replies, user messages, project-document boilerplate, and internal context
- `default_filter`: saved startup scope, sort order, and search filters
- `viewer_filter_exclusion`: `ask` (default), `keep`, or `close`; controls what happens
  when updated filters exclude the session open in the full viewer

Unknown theme names fall back to `lazygit` without discarding the other settings
in the file.

`default_filter.visibility_search` selects `visible` (default), `all`, or `hidden`
for interactive searches. Missing values, including in existing settings, use
Visible. Change **Search content** (`v`) in `Ctrl+F`; Enter applies it and `Ctrl+S`
also saves it. Query modifiers override the selection temporarily, without
changing the saved preference. JSON/export modes ignore saved defaults.

`display_options.hide_internal_context` defaults to `true`, including when the
key is absent from existing settings. The **Internal Context** visibility toggle
in `Ctrl+F` (mnemonic `7`) shows or hides user messages consisting entirely of
`<codex_internal_context …>` or `<goal_context>` blocks. Messages with ordinary
prose outside the blocks or incomplete wrappers remain visible. Other generated
context, such as environment blocks, keeps its existing visibility behavior.

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
