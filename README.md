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
- JavaScript rules for previewing or applying batch session cleanup actions
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

By default, searches are scoped to the current working directory. Use `-g` / `--global` to search everything, or `--dir PATH[:BRANCH]` to target a specific project (optionally filtered by branch).

### Date filters

`--after` and `--before` accept `YYYY-MM-DD` or RFC3339 timestamps.

### JavaScript rules

Rules live at `~/.config/aics/rules.js` by default. Use `--preview-rules` to review proposed actions in the TUI without changing files, or add `--json` to print proposed actions as JSONL. Use `--apply-rules` to apply supported actions non-interactively. Use `--rules PATH` to test another rules file. Run `aics --write-rules-dts` to write TypeScript declarations for the rules API to `~/.config/aics/rules.d.ts`.
Rules receive session metadata such as `session.model`, `session.modelProvider`, `session.reasoningEffort`, `session.approvalPolicy`, and `session.sandboxMode`.

```js
rule("trash short commit helper sessions", ({ turns, re }) => {
  return turns.user.length === 2 &&
    re(String.raw`\s*[/$](gdf-)?commit\b`, "m").test(turns.user[0].text(4096))
    ? trash("commit helper")
    : nothing();
});
```

For the first implementation, rules can return `nothing()` or `trash(reason)`. Rules mode honors the usual scope/filter flags such as `-g`, `--dir`, `--agent`, `--after`, `--before`, `--min-lines`, and `--sub-agent`, but it does not accept a text search query yet.

Large transcript fields are fetched from Rust only when a rule calls one of the lazy methods. The limit argument is optional; omitting it uses a practically unbounded default for stress testing, but normal rules should pass an explicit byte limit:

- `session.firstUserText(limit)`, `session.firstText(limit)`, `session.lastText(limit)`
- `turns.user[n].text(limit)`, `turns.contextualUser[n].text(limit)`, `turns.agent[n].text(limit)`, `turns.system[n].text(limit)`, `turns.toolResults[n].text(limit)`
- `turns.exec[n].stdout(limit)`, `turns.exec[n].stderr(limit)`
- `turns.patches[n].files[m].content(limit)`

`turns.user` excludes Codex contextual user fragments such as automatically injected AGENTS.md content. Those entries are exposed separately as `turns.contextualUser`.

## Keybindings (TUI)

| Key | Action |
| --- | --- |
| `↑` / `↓` | Move selection |
| `PgUp` / `PgDn` | Scroll preview / viewer |
| `⏎` | Open actions menu for selected session |
| `^F` | Filters modal (`^S` inside the modal saves the current filter as the startup default) |
| `^S` | Settings modal |
| `^T` | Toggle preview panel |
| `^Y` | Toggle session/summary preview mode |
| `^X` | View menu |
| `^]` / `^\` | Resize list/preview split; `Ctrl+5` / `Ctrl+4` are aliases |
| `Shift+↑` / `Shift+↓` | Jump between messages in the viewer |
| `?` / `^L` | Help |
| `Esc` | Cancel / close modal |
| `^C` | Quit |

## Indexing

Session data is indexed from:

- `~/.claude/projects/` (Claude Code)
- `~/.codex/sessions/` (Codex CLI)

By default, index data is stored under the user's home-relative cache dir, with one profile per discovered session-root set:

- `{userhome}/.cache/aics/profiles/<profile-id>/`

Each profile stores:

- `index/` (Tantivy index files)
- `index_state.json`
- `profile.json`
- `hashed-input.txt`

Override the index/cache root with `AICS_CACHE_ROOT`.

## Configuration file

Settings are stored in `settings.json` under the user's home-relative config dir:

- `{userhome}/.config/aics/settings.json`

Override the config root with `AICS_CONFIG_ROOT`.

Writes to `settings.json` are atomic (temp file + rename). If the file exists
but cannot be parsed at startup, it is moved aside to
`settings.json.corrupt-<timestamp>` and defaults are used; a warning is printed
to stderr and shown in the TUI statusline.

Set `AICS_SETTINGS_LOGFILE=<path>` to append a diagnostic line for every
settings load/save. The process id is inserted into the filename so concurrent
instances do not clobber each other (`settings.log` → `settings.<pid>.log`).

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
- `default_filter`

## License

MIT. See [LICENSE](LICENSE).
