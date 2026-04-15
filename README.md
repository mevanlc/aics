# aics

`aics` (AI Chat Search) is a cross-platform Rust TUI for searching local Claude Code and Codex CLI chat session history.

It builds a local Tantivy index over your session JSONL files and gives you an interactive terminal UI with full-text search, live previews, filters, and a markdown-rendered session viewer. It can also emit JSONL results for scripting.

## Features

- Full-text search across Claude Code and Codex CLI sessions
- Incremental indexing — only new or changed sessions get re-indexed on startup
- Interactive TUI with session list, snippet preview, and scrollable full-session viewer
- Filter modal: scope, agent, date range, minimum length, session kind (original / trimmed / rollover / sub-agent), live sessions
- Sort by time or text relevance
- Markdown rendering with syntax highlighting and search-term highlighting in the viewer
- Multiple themes (lazygit, aics, sunset, late.sh), configurable via settings modal
- Configurable claude/codex launch commands so `aics` can hand off to resume a session
- `--json` mode for scripting
- Cross-platform: Linux, macOS, Windows (path matching handles symlinks and Windows case-insensitivity)

## Install

Pre-built binaries for Linux, macOS (Intel + Apple Silicon), and Windows are published on the [releases page](https://github.com/mevanlc/aics/releases).

From source:

```bash
cargo install --path .
```

## Usage

```bash
# Search sessions for the current directory, open the TUI
aics deploy

# Search across all indexed sessions
aics -g

# Filter to Claude sessions after a date, sorted by relevance
aics -g --agent claude --after 2026-03-01 --sort-by relevance "vector db"

# Emit JSONL instead of launching the TUI
aics --json -g "vector db"

# Rebuild the index from scratch
aics --rebuild-index

# Nuke the index and exit
aics --delete-index
```

Run `aics --help` for the full flag list.

### Scope

By default, searches are scoped to the current working directory. Use `-g` / `--global` to search everything, or `--dir PATH[:BRANCH]` to target a specific project (optionally filtered by branch).

### Date filters

`--after` and `--before` accept `YYYY-MM-DD` or RFC3339 timestamps.

## Keybindings (TUI)

| Key | Action |
| --- | --- |
| `↑` / `↓` | Move selection |
| `PgUp` / `PgDn` | Scroll preview / viewer |
| `⏎` | Open actions menu for selected session |
| `^F` | Filters modal |
| `^S` | Settings modal |
| `^P` | Toggle preview panel |
| `^Y` | Toggle session/summary preview mode |
| `^H` / `^L` | Resize list/preview split |
| `^X` + action letter | Run a session action directly; in `^X` mode the action letter wins even if `Ctrl` is still held |
| `Shift+↑` / `Shift+↓` | Jump between messages in the viewer |
| `?` | Help |
| `Esc` | Cancel / close modal |
| `^C` | Quit |

## Data sources

Session data is read from:

- `~/.claude/projects/` (Claude Code)
- `~/.codex/sessions/` (Codex CLI)

The local index and settings live under the platform config dir (e.g. `~/.config/aics/` on Linux, `~/Library/Application Support/aics/` on macOS). Override with `AICS_CONFIG_ROOT`.

## License

MIT. See [LICENSE](LICENSE).
