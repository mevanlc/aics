# aics

`aics` (AI Chat Search) is a cross-platform Rust TUI for searching local Claude Code, Codex CLI, and Antigravity CLI chat session history.

It builds a local Tantivy index over your session JSONL files and gives you an interactive terminal UI with full-text search, live previews, filters, and a markdown-rendered session viewer. It can also emit JSONL results for scripting, manage supported session files, resume sessions, attach an AI-generated summary to sessions, and more.

## Features

- Full-text search across Claude Code, Codex CLI, and Antigravity CLI sessions
- Native Claude Code autosummaries and Codex rollout summaries in previews and summary snippets
- Incremental indexing — only new or changed sessions get re-indexed on startup
- Interactive TUI with session list, snippet preview, and scrollable full-session viewer
- Filter modal: scope, agent, date range, minimum length, session kind (original / trimmed / rollover / sub-agent), live, superseded, and trashed sessions
- Sort by time or text relevance
- Markdown rendering with syntax highlighting and search-term highlighting in the viewer
- Multiple themes (lazygit, aics, sunset, late), configurable via settings modal
- Configurable Claude, Codex, and Antigravity launch commands so `aics` can hand off to resume a session
- AICS trash, restore, and permanent deletion for session files and complete Antigravity bundles
- `--json` mode for scripting, and `--export DIR` to batch-export matching sessions as Markdown
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
# Search sessions for the current directory and open the TUI
aics

# Search globally with an initial query
aics -g "vector db"

# Emit relevance-sorted JSONL instead of opening the TUI
aics --json -g --sort-by relevance "vector db"

# Export every matching session to a directory as Markdown
aics -g --export ./transcripts "vector db"

# Include superseded sources and equivalent fork duplicates hidden by default
aics -g --superseded both

# Review JavaScript cleanup proposals in the TUI
aics --preview-rules -g
```

The optional `QUERY` starts a search immediately or prefills the TUI. Searches
default to the current directory; use `-g` / `--global` to search all indexed
sessions. Run `aics --help` for a compact flag list.

[See command-line usage, filters, modes, and the complete flag reference.](docs/command-line.md)

### JavaScript rules

JavaScript rules automate repeatable session cleanup. Rules live at
`~/.config/aics/rules.js` by default; preview their proposed actions with
`--preview-rules` or apply them with `--apply-rules`.

[Learn more about writing and running JavaScript rules.](docs/rules-js.md)

### Keybindings (TUI)

| Key | Action |
| --- | --- |
| `↑` / `↓` (Arrows) | Move selection |
| `⏎` (Enter) | Show actions for selected session |
| `PgUp` / `PgDn` | Preview/viewer page scroll |
| `Home` / `End` | Preview/viewer jump to beginning/end |
| `Esc` | Cancel / close modal / go back |
| `^F` | Edit Filters |
| `^S` | Edit Settings |
| `^L` | Help |
| `^C` | Quit |

[See detailed session-list and viewer keybindings.](docs/keybindings.md)

### Session summaries

AICS displays native Claude Code and Codex CLI summaries and can generate
Markdown sidecar summaries using a configurable AI CLI command and prompt.

[Learn more about session summaries.](docs/session-summaries.md)

### Indexing

AICS incrementally indexes Claude Code sessions from `~/.claude/projects/`,
Codex CLI sessions from `~/.codex/sessions/`, and Antigravity conversations from
`~/.gemini/antigravity-cli/brain/`. Index data is stored in a separate cache
profile for each discovered session-root set; set `AICS_CACHE_ROOT` to override
the cache location.

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
