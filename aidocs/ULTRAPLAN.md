# ULTRAPLAN: aics

## Goal

`aics` is a single-purpose, cross-platform Rust TUI for searching Claude Code and Codex CLI chat session histories. It replaces the search functionality of `aichat search` from `claude-code-tools` with a standalone binary that handles indexing, searching, and display in-process — no shelling out to Python or to itself. The command surface is `aics [OPTIONS] [QUERY]`, aesthetically inspired by lazygit, built on ratatui + crossterm + tantivy.

**Target platforms**: macOS, Linux, Windows, Termux (Android).

## Key Findings

### Data Sources

Two completely different JSONL formats must be parsed:

**Claude sessions** (`~/.claude/projects/{project-path}/*.jsonl`):
- Each line is a JSON object with a top-level `type` field: `user`, `assistant`, `system`, `file-history-snapshot`, `summary`
- Conversation metadata lives on every user/assistant entry: `parentUuid`, `uuid`, `isSidechain`, `cwd`, `sessionId`, `version`, `gitBranch`, `timestamp`
- `message.content` varies: plain string, array of `{type: "text"}`, array of `{type: "tool_result"}`, XML command tags
- Assistant content blocks: `text`, `thinking` (with signature), `tool_use` (name, input, caller)
- Some session files contain only `file-history-snapshot` entries (no conversation) — must detect and skip
- `summary` type entries indicate compacted/error sessions
- Project path is encoded in the directory structure: `~/.claude/projects/-Users-em-p-my-foo/` → `/Users/em/p/my/foo`
- Subagent sessions live under `{session-uuid}/subagents/agent-{id}.jsonl`

**Codex sessions** (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`):
- Every line has 3 fields: `timestamp`, `type`, `payload`
- Top-level types: `session_meta`, `turn_context`, `response_item`, `event_msg`
- `response_item` payload subtypes: `message`, `reasoning`, `function_call`, `function_call_output`, `custom_tool_call`, `custom_tool_call_output`
- `event_msg` payload subtypes: `token_count`, `user_message`, `agent_message`, `agent_reasoning`, `task_started`, `task_complete`, `turn_aborted`
- `function_call.arguments` is a JSON **string** (double-serialized)
- Reasoning content is encrypted (`encrypted_content`); only `summary` array is plaintext
- Format evolved across versions: v0.72 (Aug 2025) had a simpler flat header + `{record_type: "state"}` delimiters; v0.77+ uses the full `session_meta`/`turn_context`/`response_item`/`event_msg` taxonomy; v0.116 added fields like `turn_id`, `personality`, `collaboration_mode`
- Session metadata (cwd, model, cli_version) comes from `session_meta` entry; no per-message cwd

### Tantivy (v0.25.0)

- Stable, latest published release. 0.26 is unreleased.
- Schema: `TEXT | STORED` for searchable content, `STRING | STORED` for exact-match fields, `FAST` for sort/score fields (especially `modified_ts` for recency)
- Incremental indexing: delete by unique term + add + commit is atomic from reader's view
- Reader/writer fully concurrent; readers see last committed snapshot
- `SnippetGenerator` requires field to be `STORED`; generates HTML with `<b>` tags
- `BoostQuery` for phrase boosting; `TopDocs::tweak_score()` for recency decay via fast fields
- `QueryParser::parse_query_lenient()` for forgiving user input
- Can read indices from 0.21+, so reusing the existing `~/.cctools/search-index/` is technically possible — but building our own gives us control over schema and avoids coupling

### Ratatui + Crossterm

- Layout: `Layout::vertical/horizontal` with `Constraint` types, nested by splitting `Rect`s
- No built-in text input — need `tui-input` or `tui-textarea`. The search bar requires readline-style emacs keybindings (Ctrl+A/E/U/K/W, Alt+B/F/D, etc.), so whichever crate provides better coverage wins
- No built-in popup — use `Clear` widget + overlay render
- `StatefulWidget` pattern for List/Table/Scrollbar; state persists in app struct
- Focus management is app-level (enum + dispatch + border style changes)
- Crossterm re-exported at `ratatui::crossterm`

### Unicode & Rendering Quality

The old TUI had lag and termcode artifacts. Lazygit avoids these through:
- Event-driven rendering (render on input, no frame rate cap)
- Explicit area clearing before redraw
- Grapheme-cluster-aware width calculation with ASCII fast path
- Truncation that respects grapheme boundaries

For Rust, the stack is:
- `unicode-width` — codepoint display width (standard, used by ratatui internally)
- `unicode-segmentation` — grapheme cluster iteration
- `unicode-truncate` — safe string truncation to display width
- Practical strategy: cap grapheme width at 2, filter control characters, ASCII fast path

Terminals disagree on emoji width — this can't be universally solved, but the goal is to be as correct as reasonably possible rather than just "good enough." The ASCII fast path handles the common case; the slow path should be thorough.

### Existing Tool's Search Behavior (What, Not How)

Features to carry forward:
- 200ms search debounce
- Lenient query parsing (no syntax errors for the user)
- Multi-word queries: OR semantics with 5x phrase boost for exact matches
- Recency decay: 7-day half-life exponential (`1.0 + exp(-age / half_life)`)
- Snippet generation: tantivy SnippetGenerator primary, manual keyword extraction fallback
- Filters: scope (global vs current project), agent (claude/codex/all), session type (original/trimmed/continued/sub-agent), date range, min-lines, branch, live-only
- Sort: by relevance (default) or by time
- Preview pane: first/last messages, colored by agent
- Full conversation view with inline search
- Action menu: view, resume, export, clone, trim, delete, copy ID/path

## Approach

### Architecture: Single Binary, Modular Internals

```
aics
├── main.rs          — CLI parsing, entrypoint
├── index/
│   ├── schema.rs    — tantivy schema definition
│   ├── writer.rs    — incremental indexing engine
│   └── reader.rs    — search, scoring, snippets
├── parse/
│   ├── claude.rs    — Claude JSONL parser
│   ├── codex.rs     — Codex JSONL parser
│   └── session.rs   — unified Session model
├── tui/
│   ├── app.rs       — app state, event loop
│   ├── layout.rs    — panel arrangement
│   ├── search.rs    — search bar widget/state
│   ├── list.rs      — session list widget/state
│   ├── preview.rs   — preview pane widget/state
│   ├── viewer.rs    — full conversation view
│   ├── filter.rs    — filter modal
│   ├── actions.rs   — action menu
│   ├── theme.rs     — color definitions
│   └── util.rs      — text truncation, width helpers
└── scan.rs          — filesystem scanner (find session files)
```

This is a starting point, not a rigid prescription. Modules may merge or split as implementation reveals natural boundaries.

### Index Strategy

**Own index, own location, clean break.** No compatibility with the Python tool's `~/.cctools/search-index/` and no migration path — the index builds fast enough that rebuilding from scratch isn't a pain point.

Use the `directories` crate (Rust equivalent of Python's `platformdirs`) for cross-platform cache/data paths. Index and state go in the platform-appropriate cache directory:
- Linux/macOS: `~/.cache/aics/index/` and `~/.cache/aics/index_state.json`
- Windows: `{FOLDERPATH_LOCAL_APP_DATA}/aics/cache/`

Track file path → (mtime, size) in `index_state.json` for incremental updates.

On startup, `aics` will:
1. Scan for session files (both Claude and Codex directories)
2. Diff against index state to find new/modified/deleted files
3. Incrementally update the tantivy index
4. Open a reader and launch the TUI

Indexing should be fast enough to run synchronously on startup. If it proves slow for very large session collections, it can be moved to a background thread that feeds results to the TUI incrementally — but don't prematurely optimize for this. Best-effort performance; hard optimization can come later.

### TUI Architecture

Event-driven loop (lazygit-style):
1. `terminal.draw()` — render current state
2. `crossterm::event::poll(timeout)` — check for input (short timeout for responsiveness)
3. Process event → mutate app state
4. Loop

App state holds:
- Current mode/focus (which panel or modal is active)
- Search query + debounce timer
- Filtered/sorted session list
- Selected session index + scroll offset
- Preview content for selected session
- Filter state
- Theme

Focus model: enum with variants for each panel/modal. Tab cycles between search bar, session list, and preview. Modals (filter, actions) capture focus exclusively until dismissed.

### CLI Surface

```
aics [OPTIONS] [QUERY]

Options:
  -g, --global              Search all projects (default: current directory)
  --dir <PATH[:BRANCH]>     Filter to directory
  --branch <BRANCH>         Filter to git branch
  -n, --num-results <N>     Limit results
  --agent <claude|codex>    Filter by agent
  --after <DATE>            Sessions modified after
  --before <DATE>           Sessions modified before
  --min-lines <N>           Minimum line count
  --no-original             Exclude original sessions
  --no-trimmed              Exclude trimmed sessions
  --no-rollover             Exclude rollover sessions
  --sub-agent               Include sub-agent sessions
  --live                    Show only running sessions
  --json                    Output as JSONL (non-interactive)
  --by-time                 Sort by time instead of relevance
  --rebuild-index           Force full index rebuild
```

Use `clap` for argument parsing.

### Action Dispatch

Since there's no Python parent to hand off to, `aics` must handle actions itself or delegate clearly:

- **View**: Render full conversation in the TUI (in-process)
- **Export**: Write session to `.txt` file (in-process)
- **Copy ID / Copy Path / Copy Dir**: Write to clipboard via `arboard` crate (in-process)
- **Resume / Clone / Trim / Continue**: These launch `claude` or `codex` CLI subprocesses. `aics` should exit cleanly and exec into the appropriate command (or spawn it and exit). The exact invocation will need to match what the respective CLIs expect.
- **Delete**: Remove the JSONL file (with confirmation) and update the index (in-process)
- **JSON output mode**: Skip TUI entirely, write JSONL results to stdout

Resume/clone/trim/continue are the actions that inherently require invoking external tools. The cleanest approach is probably to print the command and exit, or to `exec()` into it. This is a design area where the implementing agent should use judgment based on what feels right during development.

## Task Breakdown

### MVP — The First Usable Thing

The MVP story: user runs `aics -g`, sees all conversations sorted by last-modified descending, types a search query to filter, sees query terms highlighted in the list, views a readable form of the selected conversation in the right panel, and quits with Ctrl+C.

#### M1: Project Scaffold & Data Model

1. **Cargo project scaffold** — `cargo init`, set up `Cargo.toml` with dependencies: `tantivy`, `ratatui`, `crossterm`, `tui-input`, `serde`, `serde_json`, `clap`, `chrono`, `unicode-width`, `unicode-segmentation`, `unicode-truncate`, `directories`, `anyhow`, `env_logger`, `log`. Set binary name to `aics`. Edition 2021. Use `anyhow` for error propagation throughout. Initialize `env_logger` early in `main()` so `RUST_LOG=debug aics` works for diagnostics.

2. **Unified Session model** — Define the `Session` struct that both parsers produce and the index stores. Fields: `session_id`, `agent` (claude/codex), `project`, `branch`, `cwd`, `created`, `modified`, `modified_ts`, `lines`, `file_path`, `first_msg_role`, `first_msg_content`, `last_msg_role`, `last_msg_content`, `first_user_msg_content`, `derivation_type`, `is_sidechain`, `custom_title`. This is the common currency between parsing, indexing, and display.

3. **Claude JSONL parser** — Parse a Claude session file into a `Session`. Stream line-by-line via `BufReader`. Must handle: extracting user/assistant text from the various `message.content` shapes, counting lines, detecting derivation type from file path (subagents dir → sidechain), extracting first/last messages, skipping `file-history-snapshot`-only files, extracting project path from directory structure. Parser must be defensive — skip unrecognized entries, don't crash on malformed lines.

4. **Codex JSONL parser** — Parse a Codex session file into a `Session`. Stream line-by-line. Must handle: both old format (flat header, `record_type: "state"` delimiters) and new format (`session_meta`/`response_item`/`event_msg` taxonomy), extracting text from `response_item` messages (`input_text`/`output_text` content blocks), extracting cwd/model from `session_meta` or `turn_context`, handling encrypted reasoning (index summaries only), double-deserialized `function_call.arguments`. Must be defensive against format evolution — unrecognized entry types are silently skipped, not errors.

5. **Filesystem scanner** — Discover session files. Scan `~/.claude/projects/` recursively for `*.jsonl` and `~/.codex/sessions/` recursively for `*.jsonl`. Return file paths with metadata (mtime, size). Use `directories` crate for locating platform-appropriate paths.

#### M2: Search Index

6. **Tantivy schema** — Define the index schema. `content` as `TEXT | STORED` for full-text search + snippets. Metadata fields as `STRING | STORED`. `modified_ts` as `u64 | FAST | STORED` for recency scoring and time-based sorting. A unique identifier field for delete-by-term during updates.

7. **Index writer** — Incremental indexing engine. On startup: load state from `~/.cache/aics/index_state.json`, scan filesystem, diff to find new/modified/deleted files, parse and index changed files, prune deleted, commit, save updated state. Handle first-run (full build) gracefully. Index stored at `~/.cache/aics/index/`. Sub-agent sessions excluded from the index by default.

8. **Search engine** — Query the index. Lenient query parsing via `QueryParser::parse_query_lenient()`. Phrase boost (5x via `BoostQuery` when query has 2+ words). Recency decay via `TopDocs::tweak_score()` using the `modified_ts` fast field. Snippet generation via `SnippetGenerator` with manual fallback. Return ranked results.

#### M3: TUI — Enough to Be Useful

9. **Terminal setup + event loop** — Alternate screen, raw mode, mouse capture. Event-driven loop with `crossterm::event::poll()`. Install a panic hook that restores the terminal before printing the panic message. Clean restore on normal exit too.

10. **Theme** — Define color constants in one place. Dark theme inspired by lazygit: muted borders for unfocused panels, bright accent for focused panel, agent-colored indicators (orange for Claude, green for Codex), subtle selection highlight, yellow for search match highlights. `Color::Rgb()` for precision.

11. **Layout** — Three-region layout: search bar (top, 3 rows), main body (middle, fill), status bar (bottom, 1 row). Main body splits horizontally: session list (left, ~60%) and preview pane (right, ~40%). Handle narrow terminals gracefully (hide preview if too narrow, rather than rendering garbage).

12. **App state struct** — Central struct holding: focus state, search query, debounce timer, sessions list, selected index, scroll offsets, preview content cache, current mode. All TUI components read from and mutate this.

13. **Search bar** — Text input with readline-style emacs keybindings: Ctrl+A (start), Ctrl+E (end), Ctrl+U (kill line), Ctrl+K (kill to end), Ctrl+W (delete word back), Alt+D (delete word forward), Alt+B/Alt+F (word back/forward). Evaluate whether `tui-input` supports these or whether `tui-textarea` is a better fit — the implementing agent should pick whichever provides the best readline coverage out of the box. Shows current scope on the right ("All Projects" vs project name). Typing triggers debounced search (200ms).

14. **Session list** — Scrollable list of sessions. Default sort: by `modified_ts` descending. When a search query is active, sort by relevance instead. Each entry shows enough to identify the session: agent icon, project/cwd, snippet or first user message, line count, relative time. Selected row highlighted. Query terms highlighted in the list entries. Keybindings: j/k or arrows, Home/End, PgUp/PgDn.

15. **Preview pane** — Shows a usable/readable form of the selected conversation in the right panel. This will be iterated on later, so start with something functional: messages labeled by role, separated visually, scrollable. Loads lazily (only when selection changes).

16. **Status bar** — Bottom row showing result count and basic keybinding hints.

17. **CLI arg parsing (minimal)** — `clap` derive API. For MVP: `-g`/`--global` flag, optional positional `[QUERY]` to pre-fill search. Wire into initial app state.

18. **Quit** — Ctrl+C exits cleanly.

### Post-MVP — Feature Layers

These are ordered roughly by value, but the implementing agent should use judgment about sequencing based on what falls out naturally during development.

19. **Rendering quality pass** — Test with emoji-heavy content, CJK characters, non-BMP, long lines. Ensure truncation is grapheme-aware via `unicode-segmentation` + `unicode-truncate`. Ensure no background color bleeding or stale characters at widget boundaries. Clear widget areas fully before redraw. Test terminal resize. Lean towards getting this right rather than just "good enough" — the old tool's rendering artifacts were a significant annoyance.

20. **Full CLI flags** — Add the rest: `--dir`, `--branch`, `--agent`, `--after`, `--before`, `--min-lines`, `--no-original`, `--no-trimmed`, `--no-rollover`, `--sub-agent`, `--live`, `-n`, `--by-time`, `--json`, `--rebuild-index`.

21. **Filter system** — Filters applied as a pipeline over the session list. Toggleable via keybindings or a filter modal (Ctrl+F). Includes: session type toggles, agent filter, date range, min-lines, scope, branch.

22. **Action menu** — Popup triggered by Enter on a selected session. Single-key shortcuts. Actions: view (v), copy ID (i), copy path (p), export (e), delete (d), resume (r), etc.

23. **Full conversation viewer** — Modal showing the complete conversation. Scrollable. Inline search with `/`. Messages styled by role and agent. Escape returns to list view.

24. **Action dispatch** — In-process: view, export, copy (via `arboard`), delete. External (resume/clone/trim/continue): construct CLI invocation and exec into it or spawn + exit. Design decision left to implementing agent.

25. **JSON output mode** — `--json` skips TUI, writes JSONL results to stdout.

26. **Preview pane polish** — Colored message bubbles by agent, resizable with Ctrl+H/L, scrollable with keyboard.

27. **Live session detection** — Scan for running `claude`/`codex` processes with matching session IDs. Nice-to-have; defer if fiddly.

28. **`--rebuild-index`** — Delete existing index and state, rebuild from scratch.

### Roadmap (MVP3 / Phase 3 — not in scope for initial implementation)

- **Live session detection via `~/.claude/sessions/`** — Claude Code now writes lightweight JSON markers to `~/.claude/sessions/{pid}.json` containing `{pid, sessionId, cwd, startedAt}` for each running session. This is a much cleaner signal for live detection than scanning `ps` output. `aics` could read these files and match by `sessionId` to annotate which sessions are currently active. Codex may have an equivalent; investigate when implementing.
- `--claude-home` / `--codex-home` overrides for non-standard installations
- Configurable keybindings
- Custom theme support
- Session bookmarking / favorites
- Export formats beyond .txt

## Testing

Test fixtures are in `tests/fixtures/sessions/{claude,codex}/`. These are sanitized but structurally authentic JSONL files covering the major format variants.

Implement high-value tests during development as their value becomes apparent. The lists below are suggestions, not exhaustive checklists.

### MVP Tests

Tests should include, but not be limited to:

**Parsers** (highest value — two complex formats with known evolution):
- Claude basic session → produces a Session with correct fields (session_id, agent, cwd, project, first/last messages, line count)
- Claude snapshot-only file → detected and skipped (returns None or equivalent)
- Claude summary-only file → handled gracefully
- Claude rich content → all content shapes (plain string, text array, tool_result array, command XML, thinking blocks) are parsed without error; user/assistant text is extracted
- Codex old format (v0.72) → produces a Session with correct fields
- Codex new format (v0.77) → produces a Session; function_call arguments are correctly double-deserialized
- Codex latest format (v0.116) → new fields don't break parsing; custom_tool_call entries handled
- Codex minimal → smoke test
- Malformed/unknown lines → skipped without panic

**Index**:
- Round-trip: index a Session, query it back, verify fields match
- Incremental update: index, modify a file's mtime, re-index, verify update applied
- Empty query returns all sessions sorted by modified_ts descending

**Search**:
- Single-word query returns matching sessions
- Multi-word query returns results (phrase boost doesn't need to be tested precisely, just that it doesn't crash)
- Snippet generation produces non-empty snippets for matching queries

### Post-MVP Tests

Tests should include, but not be limited to:

- Filter pipeline: scope filtering (global vs directory), agent filtering, date range filtering, session type filtering — each independently and in combination
- Sort modes: by-time vs by-relevance produce different orderings
- Action dispatch: delete action removes file and index entry
- JSON output mode: `--json` produces valid JSONL on stdout
- CLI arg parsing: key flag combinations are wired correctly
- Project path decoding: `-Users-em-p-my-foo` → `/Users/em/p/my/foo` (and Windows path separator handling)

## Resolved Design Decisions

- **Index location**: `~/.cache/aics/` via `directories` crate (platformdirs equivalent). Cross-platform.
- **Index compatibility**: Clean break. No reuse of `~/.cctools/search-index/`, no migration path. Rebuilding from scratch is fast and painless.
- **Sub-agent sessions**: Hidden by default (excluded from index). Numerous and usually noise.
- **`--claude-home` / `--codex-home`**: Roadmapped, not in initial implementation.
- **MVP scope**: See task breakdown — global list, search with highlighting, preview, quit. Everything else is post-MVP.
- **Resume/continue actions**: Post-MVP. Design decision (exec vs spawn vs print) left to implementing agent.

## Risks & Watch-outs

- **Codex format evolution**: The Codex JSONL format changed across versions (v0.72 → v0.77 → v0.116). It may continue evolving. The parser should be defensive — log warnings for unrecognized entries rather than crashing. Consider a `parse_codex_line()` that returns `Option<ParsedEntry>` and silently skips unknowns.

- **Large session files**: Some JSONL files are 25MB+. Parsing must stream line-by-line from the start — use `BufReader` with `lines()`, never load the whole file into memory. For indexing, only the text content matters — skip binary/image data, very long tool outputs, etc. Consider a content size cap per session to keep the index reasonable.

- **Unicode rendering**: Terminals disagree on emoji and wide-character display widths, so there's no universally correct answer. That said, this should be done well — the old tool's rendering artifacts were a real annoyance. Use `unicode-width` + `unicode-segmentation` + `unicode-truncate`, cap grapheme width at 2, filter control characters, and test early with problematic content (emoji, CJK, ZWJ sequences). Lean towards correctness over speed for width calculations. The ASCII fast path handles the common case; the slow path should be thorough.

- **Terminal restore on panic**: If the app panics while in raw mode / alternate screen, the terminal is left in a broken state. Install a panic hook that restores the terminal before printing the panic message. This is critical for a good development experience too.

- **Index corruption**: If `aics` is killed during a commit, the tantivy index could be in a bad state. Tantivy is generally crash-safe (it uses a WAL), but the `index_state.json` might be stale. On startup, if the index looks corrupt, offer to rebuild.

- **Startup latency**: Scanning + indexing on every startup could be slow if there are thousands of session files. Best-effort performance for now; hard optimization can come later. The existing Python tool's `auto_index()` was fast enough in practice, and Rust should be faster, but measure don't assume.

- **Clipboard**: `arboard` uses `NSPasteboard` on macOS, X11/Wayland on Linux, Win32 on Windows. On Termux, clipboard access requires `termux-clipboard-set`/`termux-clipboard-get` — `arboard` may not support this natively. Clipboard is post-MVP, so this can be solved when we get there (e.g., fall back to shelling out to `termux-clipboard-set` on Android). Also test that clipboard works in alternate screen / raw mode contexts.

- **`file-history-snapshot`-only sessions**: Some Claude JSONL files contain no conversation at all — just file tracking snapshots. The parser must detect these early (check if any `user`/`assistant` entries exist) and skip them rather than creating an empty index entry.

- **Cross-platform considerations**: Target platforms are macOS, Linux, Windows, and Termux.
  - **Filesystem paths**: The `directories` crate handles platform-appropriate cache/config dirs. Session file locations (`~/.claude/`, `~/.codex/`) should resolve correctly on all platforms since Claude Code and Codex CLI use the same conventions.
  - **Termux**: Runs on Android with a Linux-like userspace. Crossterm + ratatui should work. Termux terminals may have limited width and non-standard font metrics — test that the layout degrades gracefully on narrow screens (e.g., 80 columns on a phone). Termux also lacks `/proc` in the normal sense, which affects live session detection (post-MVP).
  - **Windows**: Path separators, home directory resolution, and terminal behavior (Windows Terminal vs conhost) differ. Crossterm abstracts most of this. The session directory path encoding (`-Users-em-p-my-foo`) will use backslashes on Windows — the project-path decoder needs to handle both separators.
  - **Tantivy**: Pure Rust, cross-platform. No platform-specific concerns.
  - Avoid `#[cfg(unix)]`-only code where possible; when platform-specific code is unavoidable, provide a reasonable fallback or no-op for unsupported platforms rather than a compile error.
