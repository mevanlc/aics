# aics

`aics` (AI Chat Search) is a cross-platform Rust TUI for searching local Claude Code and Codex CLI chat session history.

It builds a local Tantivy index over your session JSONL files and gives you an interactive terminal UI with full-text search, live previews, filters, and a markdown-rendered session viewer. It can also emit JSONL results for scripting, delete unwanted sessions, resume sessions, attach an AI-generated (Claude Code or Codex CLI) summary to sessions, and more.

## Features

- Full-text search across Claude Code and Codex CLI sessions
- Native Claude Code autosummaries and Codex rollout summaries in previews and summary snippets
- Incremental indexing — only new or changed sessions get re-indexed on startup
- Interactive TUI with session list, snippet preview, and scrollable full-session viewer
- Filter modal: scope, agent, date range, minimum length, session kind (original / trimmed / rollover / sub-agent), live sessions
- Sort by time or text relevance
- Markdown rendering with syntax highlighting and search-term highlighting in the viewer
- Multiple themes (lazygit, aics, sunset, late), configurable via settings modal
- Configurable claude/codex launch commands so `aics` can hand off to resume a session
- `--json` mode for scripting
- JavaScript rules for previewing or applying batch session cleanup actions
- Cross-platform: Windows, macOS, Linux, Android (Termux), FreeBSD, and NetBSD
  (path matching handles symlinks and Windows case-insensitivity)

## Install

Pre-built binaries for Linux, macOS (Intel + Apple Silicon), and Windows are published on the [releases page](https://github.com/mevanlc/aics/releases).

From source: `cargo install --path .`

## Screenshots

<img src="https://i.imgur.com/AnwmZGF.png">

<img src="https://i.imgur.com/V2s9irA.png">

<img src="https://i.imgur.com/ZgzA25c.png">

<img src="https://i.imgur.com/ePGG5sp.png">


## Usage

```bash
# Search sessions for the current directory, open the TUI
aics
# Search across all indexed sessions
aics -g
# Emit JSONL instead of launching the TUI
aics --json -g "vector db"
# Review JavaScript cleanup rules from ~/.config/aics/rules.js in the TUI
aics --preview-rules -g
# Print rule proposals as JSONL instead
aics --preview-rules --json -g
# Write TypeScript declarations for JavaScript cleanup rules
aics --write-rules-dts
# Delete or rebuild the index
aics <--rebuild-index|--delete-index>
```

Run `aics --help` for the full flag list.

### Scope

By default, searches are scoped to the current working directory. Use `-g` / `--global` to search everything, `--no-global` to start in project-local mode even when the saved default scope is global, or `--dir PATH[:BRANCH]` to target a specific project (optionally filtered by branch). `--no-global` only selects the startup scope; it does not prevent switching between global and local scope in the TUI.

### Date filters

`--after` and `--before` accept `YYYY-MM-DD` or RFC3339 timestamps.

### JavaScript rules

JavaScript rules automate repeatable session cleanup. Rules live at
`~/.config/aics/rules.js` by default; preview their proposed actions with
`--preview-rules` or apply them with `--apply-rules`.

[Learn more about writing and running JavaScript rules.](docs/rules-js.md)

### Keybindings (TUI)

| Key | Action |
| --- | --- |
| `↑` / `↓` | Move selection |
| `PgUp` / `PgDn` | Scroll preview / viewer |
| `⏎` | Open actions menu for selected session |
| `^F` | Filters modal, including preview/viewer display toggles (`^S` inside the modal saves startup defaults) |
| `^S` | Settings modal |
| `^T` | Toggle preview panel |
| `^Y` | Cycle the session-card snippet between session text and summaries |
| `Shift+←` / `Shift+→` | Resize list/preview split |
| `Shift+↑` / `Shift+↓` | Jump between messages in the preview / viewer |
| `?` / `^L` | Help |
| `Esc` | Cancel / close modal |
| `^C` | Quit |

[See detailed session-list and viewer keybindings.](docs/keybindings.md)

### Session summaries

AICS displays native Claude Code and Codex CLI summaries and can generate
Markdown sidecar summaries using a configurable AI CLI command and prompt.

[Learn more about session summaries.](docs/session-summaries.md)

### Indexing

AICS incrementally indexes Claude Code sessions from `~/.claude/projects/` and
Codex CLI sessions from `~/.codex/sessions/`. Index data is stored in a separate
cache profile for each discovered session-root set; set `AICS_CACHE_ROOT` to
override the cache location.

[Learn more about search behavior and index storage.](docs/search-and-indexing.md)

### Configuration

Use `^S` in the TUI to edit settings. They are stored in
`~/.config/aics/settings.json` by default; set `AICS_CONFIG_ROOT` to relocate the
configuration directory.

[Learn more about configuration and available settings.](docs/config-settings.md)

### Logging

The built-in logging configuration defaults to `warn`; set `RUST_LOG` to enable
more diagnostics. Interactive sessions write per-process rolling files under
the AICS configuration directory, while command and JSON modes log to stderr.

[Learn more about log files and log4rs configuration.](docs/logging.md)

## License

MIT. See [LICENSE](LICENSE).
