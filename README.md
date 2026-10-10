# aics

`aics` (AI Chat Search) is a cross-platform Rust TUI for searching local Claude Code, Codex CLI, and Antigravity CLI chat session history.

It builds a local Tantivy index over your session JSONL files and gives you an interactive terminal UI with full-text search, live previews, filters, and a markdown-rendered session viewer. It can also emit JSONL results for scripting, manage supported session files, resume sessions, attach an AI-generated summary to sessions, and more.

## Features

- Full-text search across Claude Code, Codex CLI, and Antigravity CLI sessions
- Persistent search history with a fuzzy-filtered Ctrl+R recall modal
- Native Claude Code autosummaries and Codex rollout summaries in previews and summary snippets
- Incremental indexing with parallel reading of new or changed sessions on startup
- Interactive TUI with session list, snippet preview, and scrollable full-session viewer
- Native session names centered in the preview border, or the session-list border when preview is hidden
- Filter modal: search content (Visible / All / Hidden), scope, agent, date range, minimum length, session kind (original / trimmed / rollover / sub-agent), live, superseded, and trashed sessions
- Existing visibility toggles also cover Antigravity task outcomes, orchestration notices, directory rules, worker messages, and checkpoints
- Internal Context hides appended memory citation footers while preserving the reply text
- Sort by time or text relevance
- Markdown rendering with syntax highlighting and search-term highlighting in the viewer
- Separate viewer Search and Find boxes, with substring/regex Find over readable text and individual match navigation
- Ctrl/Shift-click block selection, optional Alt-modified alternatives, and Alt+C copying as source Markdown
- Multiple themes (lazygit, aics, sunset, late), configurable via settings modal
- Configurable Claude, Codex, and Antigravity launch commands so `aics` can hand off to resume a session
- AICS trash, restore, and permanent deletion for session files and complete Antigravity bundles, with recorded trash reasons
- `--json` mode for scripting, and `--export DIR` to batch-export matching sessions as Markdown
- JavaScript rules for previewing or applying batch session cleanup actions
- Cross-platform: Windows, macOS, Linux, Android (Termux), FreeBSD, and NetBSD
  (path matching handles symlinks and Windows case-insensitivity)

## Install

Download binaries from the [releases page](https://github.com/mevanlc/aics/releases).
New releases cover Linux (static musl), macOS, and Windows (x64/ARM64),
plus Android ARM64 for Termux. FreeBSD builds are currently disabled.

From a checkout: `cargo install --path . --locked`.
After the first crates.io publication: `cargo install aics --locked`.

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
sessions. Run `aics --help` for a compact flag list, or `aics --manpage` for the
full reference manual.

Interactive searches default to **Visible** content, following the `^F`
Visibility toggles. Choose **Search content** in `^F` and use `^S` to save a
preference. Add `all:`, `visible:`, or `hidden:` to a query for a temporary
override. JSON/export searches default to All and ignore saved filters.

[See command-line usage, filters, modes, and the complete flag reference.](docs/command-line.md)

### JavaScript rules

JavaScript rules automate repeatable session cleanup. Rules live at
`~/.config/aics/rules.js` by default; preview their proposed actions with
`--preview-rules` or apply them with `--apply-rules`.
Rules automatically exclude live or locked Claude, Codex, and Antigravity sessions.

[Learn more about writing and running JavaScript rules.](docs/rules-js.md)

### Keybindings (TUI)

| Key | Action |
| --- | --- |
| `↑` / `↓` (Arrows) | Move selection |
| `⏎` (Enter) | Show actions for selected session |
| `^T` or `Enter` → `d` | Move selected session to trash |
| `PgUp` / `PgDn` | Preview/viewer page scroll |
| `Home` / `End` | Preview/viewer jump to beginning/end |
| `Esc` | Cancel / close modal / go back |
| `^F` | Edit filters in the session list; focus Find in the viewer |
| `^Shift+F` | Edit filters in the viewer |
| `^S` | Edit Settings |
| `^R` | Recall search history |
| `^L` | Help |
| `^C` | Quit |

[See detailed session-list and viewer keybindings.](docs/keybindings.md)

[See trash recovery and recorded reasons.](docs/trash.md)

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

## Development and releases

PRs and pushes to `main` run quality checks, build all seven enabled targets, and
verify installation from the crate package. Desktop/server builds execute tests and
smoke-test the extracted download, including an isolated fixture search.
Android builds receive ELF and dependency checks.

Tags supply release versions; committed versions remain `0.0.0`. Releases include
the exact published crate, checksums, a source manifest, and immutable GitHub
downloads. See [the release guide](devdocs/RELEASING.md) for local checks,
credentials, trusted publishing, and recovery.

## License

MIT. See [LICENSE](LICENSE).
