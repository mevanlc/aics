# aics

`aics` (AI Chat Search) is a cross-platform Rust TUI for searching local Claude Code and Codex CLI chat session history.

It builds a local Tantivy index over your session JSONL files and gives you an interactive terminal UI with full-text search, live previews, filters, and a markdown-rendered session viewer. It can also emit JSONL results for scripting, delete unwanted sessions, resume sessions, attach an AI-generated (Claude Code or Codex CLI) summary to sessions, and more.

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
# Delete or rebuild the index
aics <--rebuild-index|--delete-index>
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
| `^T` | Toggle preview panel |
| `^Y` | Toggle session/summary preview mode |
| `^H` / `^L` | Resize list/preview split |
| `^X` + action letter | Run a session action directly; in `^X` mode the action letter wins even if `Ctrl` is still held |
| `Shift+↑` / `Shift+↓` | Jump between messages in the viewer |
| `?` | Help |
| `Esc` | Cancel / close modal |
| `^C` | Quit |

## Indexing

Session data is indexed from:

- `~/.claude/projects/` (Claude Code)
- `~/.codex/sessions/` (Codex CLI)

By default, index data is stored under the platform cache dir, with one profile per discovered session-root set:

- Linux: `~/.cache/aics/profiles/<profile-id>/`
- macOS: `~/Library/Caches/aics/profiles/<profile-id>/`
- Windows: `%LOCALAPPDATA%\aics\cache\profiles\<profile-id>\`

Each profile stores:

- `index/` (Tantivy index files)
- `index_state.json`
- `profile.json`
- `hashed-input.txt`

Override the index/cache root with `AICS_CACHE_ROOT`.

## Configuration file

Settings are stored in `settings.json` under the platform config dir:

- Linux: `~/.config/aics/settings.json`
- macOS: `~/Library/Application Support/aics/settings.json`
- Windows: `%APPDATA%\aics\config\settings.json`

Override the config root with `AICS_CONFIG_ROOT`.

Available options:

- `theme` (`lazygit`, `aics`, `sunset`, `late.sh`)
- `claude_command` default: `claude`, `claude_args` default: `--dangerously-skip-permissions`
- `codex_command` default: `codex`, `codex_args` default: `--yolo`
- `show_preview`
- `preview_width_pct`
- `session_separator`
- `snippet_line_count`
- `summarize_command`
- `summarize_prompt`

## License

MIT. See [LICENSE](LICENSE).

