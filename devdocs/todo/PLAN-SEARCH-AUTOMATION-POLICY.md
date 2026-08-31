# Plan: Search, Display, and Automation Policies

## Goal

Separate three related but distinct ideas:

1. Search indexing policy: what transcript text should contribute to full-text search.
2. Display policy: how transcript cells should render in preview/viewer, including hidden, collapsed, and expanded states.
3. Automation policy: rules that propose or apply durable actions such as trashing sessions.

The immediate motivating case is agent/tool turns that read or grep files under `<home>/.codex/memories/`. Those turns are useful context while the agent is working, but they are noisy in AICS search results and should not cause unrelated sessions to match memory-content queries.

## Current Architecture

Today AICS has session-level search documents:

- [src/index/writer.rs](/Users/mclark/p/my/aics/src/index/writer.rs) builds one Tantivy document per session.
- `searchable_content(session)` currently joins `custom_title`, `first_user_msg_content`, and `session.content`.
- [src/index/reader.rs](/Users/mclark/p/my/aics/src/index/reader.rs) returns session hits, not per-turn hits.
- [src/tui/preview.rs](/Users/mclark/p/my/aics/src/tui/preview.rs) and [src/tui/viewer.rs](/Users/mclark/p/my/aics/src/tui/viewer.rs) render structured `SessionCell`s and apply display options like hiding tool calls/results.
- [src/rules/mod.rs](/Users/mclark/p/my/aics/src/rules/mod.rs) evaluates JavaScript automation rules and currently produces action proposals such as `trash`.

Consequence: "omit hits from search results" is not a precise post-query operation yet. Tantivy tells us that a session matched; it does not tell us which structured cell should own the match unless we add match provenance or move to turn/cell-level indexing. Therefore the first implementation should omit known-noisy cell text from `searchable_content` during indexing.

## Recommendation

Do not force all of this through JavaScript immediately.

Use a small built-in Rust indexing/display policy for the memory-path case first, then evolve the JavaScript facility into a broader automation and policy layer once the boundaries are clearer.

Recommended naming:

- `automation.js`: action-producing rules such as `trash(...)`, `tag(...)`, or future batch actions.
- `policies.js` or built-in settings: indexing/display behavior such as omit, hide, collapse, summarize.
- `rules.js`: avoid as the long-term default because it is ambiguous between automation, search policy, display policy, and in-TUI behavior. Since this feature is still new, prefer migrating the default automation file to `~/.config/aics/automation.js` before it settles.

If backwards compatibility is desired later, `rules.js` can remain a fallback alias with a warning. If not, make the clean break now.

## Phase 1: Built-In Memory Tool Text Search Omission

Add a first-class Rust predicate that identifies transcript cells where the agent is reading/searching AICS/Codex memory files.

Target behavior:

- Omit the matching cell's text from `searchable_content`.
- Keep the session indexed if it has other useful text.
- Preserve stored session JSON and full viewer rendering.
- Do not trash, hide, or mutate source files.

Matching rule:

```text
(?<!\w)<home>[\\/]\.codex[\\/]memories[\\/](?!\w)
```

Implementation details:

- Expand `<home>` from the same home-dir source used by config/cache path resolution.
- Escape the home path before embedding it in the regex.
- Accept both `/` and `\` separators.
- Apply the predicate to tool-call text, exec command summaries, tool output metadata, and structured cells likely to contain file paths:
  - `SessionCell::ToolCall { summary, input, raw_name, ... }`
  - `SessionCell::ToolResult { output, call_summary, ... }`
  - `SessionCell::Exec { command, cwd, parsed_summary, stdout, stderr, ... }`
  - `SessionCell::Patch { files, stdout, stderr, ... }` if needed
- Prefer matching on the cell's command/summary/input/path fields first. Avoid scanning huge stdout/stderr unless needed.
- If a cell matches the memory path, omit that cell's text from index content.

This should be implemented in the indexing projection, not in the parser. The parser should preserve source truth; indexing decides what is searchable.

Suggested shape:

```rust
pub struct IndexPolicy {
    pub omit_memory_tool_text: bool,
}

fn searchable_content_with_policy(session: &Session, policy: &IndexPolicy) -> String;

fn cell_index_text(cell: &SessionCell, policy: &IndexPolicy) -> Option<String>;
```

Index format note:

- Bump `INDEX_FORMAT_VERSION` when introducing this policy so existing indexes rebuild.
- If the behavior is configurable, include the policy fingerprint in index state or force rebuild when the setting changes.

Tests:

- A fixture with a unique term only inside a memory-reading tool cell should not match after indexing.
- A fixture with the same session containing useful user/assistant text should still match that useful text.
- Slash and backslash variants of the memory path should both match.
- A similar path outside the user's home, or a path such as `<home>/.codex/memories2/`, should not match.

## Phase 2: Search Policy Controls

Decide whether the memory omission is default-only or user-configurable.

Recommended default:

- `omit_memory_tool_text_from_search = true`

Possible config surface:

```json
{
  "search_policy": {
    "omit_memory_tool_text": true
  }
}
```

Avoid adding a CLI flag first unless there is an immediate need. This is a persistent personal preference, not usually a per-invocation search option.

If users need to find memory-tool reads deliberately, add a later TUI/search toggle:

- "Include memory tool text"
- Requires index rebuild or a dual-field index design.

Dual-field alternative:

- Index ordinary content in `content`.
- Index omitted/noisy content in `noisy_content`.
- Default searches query only `content`.
- An opt-in search mode queries both.

This is more flexible but requires schema/index-format work. For v1, omit from the single content field.

## Phase 3: Collapsed Display Mode

The display concern is different from search indexing. Hiding tool calls with `^X` is too coarse; full rendering is often too noisy.

Introduce a tri-state render policy per `SessionCell`:

```rust
pub enum CellRenderMode {
    Full,
    Collapsed { summary: String },
    Hidden,
}
```

Initial built-in display policy:

- Memory-path tool cells render as collapsed by default.
- The collapsed line should show:
  - cell kind: `exec`, `tool`, `tool result`, `patch`
  - key path or command summary
  - approximate size if available
  - a clear marker such as `[collapsed]`
- Existing hide toggles still win:
  - if tool calls are hidden, hidden beats collapsed
  - if tool results are hidden, hidden beats collapsed for payloads

Expansion UX:

- In viewer: selected collapsed row can expand/collapse with `Enter` or a dedicated key.
- Mouse click can toggle later; keyboard support should come first.
- Expansion state should live in TUI state keyed by `(session_id, cell_index)`.
- Preview pane can either always show collapsed state or share the viewer expansion state. Prefer always collapsed in preview for stability.

Tests:

- Render a memory-path exec cell as one collapsed summary line by default.
- Expansion renders the full existing cell.
- Existing hide-tool-call/hide-tool-result options still hide the cell.
- Search highlighting does not break on collapsed rows.

## Phase 4: In-TUI Automation Preview

The current `--preview-rules` output is useful for scripts but clumsy for inspection. Debug logging inside `~/.config/aics/rules.js` is a sign that the preview surface is too weak.

Add a TUI automation preview mode that uses the same evaluation engine and proposal model as `--preview-rules`.

UX shape:

- Command-line:
  - `aics --preview-automation` prints headless preview output.
  - `aics --preview-automation --tui` opens a proposal list.
  - Or simpler: inside the TUI, an actions/menu item "Automation Preview".
- List panel shows:
  - proposed action
  - rule name
  - reason
  - agent
  - session title/path/time
- Preview/viewer panel shows the selected matched session.
- Apply controls:
  - apply selected
  - apply all visible
  - skip selected
  - write JSONL report
- Apply must keep an explicit confirmation gate.

The preview should not require rule authors to add console/debug output just to inspect what matched.

## Phase 5: Rename Rules to Automation

Before the feature settles, migrate action-producing JavaScript from `rules.js` to `automation.js`.

Proposed CLI/config changes:

- Default automation file: `~/.config/aics/automation.js`
- Override flag: `--automation PATH`
- Headless flags:
  - `--preview-automation`
  - `--apply-automation`

If compatibility is kept:

1. If `automation.js` exists, use it.
2. Else if `rules.js` exists, use it and print a deprecation warning.
3. `--rules PATH` remains an alias for one release or one local transition period.

If no compatibility is needed, switch directly and update README/devdocs/tests.

## Phase 6: JavaScript Policies, Not Just Automation

After the built-in memory case and collapsed rendering path exist, consider exposing policy hooks to JavaScript.

Potential API:

```js
indexPolicy("omit memory tool reads", ({ cell, session, path }) => {
  if (cell.kind === "exec" && cell.commandText().includes("/.codex/memories/")) {
    return omitFromSearch("memory read");
  }
  return keep();
});

displayPolicy("collapse memory tool reads", ({ cell }) => {
  if (cell.text(4096).includes("/.codex/memories/")) {
    return collapse("read Codex memory file");
  }
  return full();
});
```

Concerns:

- Running JavaScript during indexing adds performance and determinism risk.
- Indexing policies need a stable fingerprint so the index rebuilds when policies change.
- Display policies run often and need caching.
- Policy callbacks must avoid materializing large text by default, same as automation rules.

Recommendation:

- Do not make JS policy hooks part of the first memory-path fix.
- First build Rust policy traits and result types.
- Then optionally allow JS to produce those same Rust-owned decisions.

## Open Decisions

- Should memory-tool text be omitted by default, or only behind a setting?
- Should omitted memory-tool text be stored in a secondary `noisy_content` field for opt-in search?
- Should collapsed memory cells appear in preview, viewer, or both?
- Should expansion state persist only during the current TUI session, or be remembered per session?
- Should `rules.js` become `automation.js` immediately, with no fallback?
- Should action-producing automation and display/index policies live in one file or separate files?

## Proposed Order

1. Implement built-in memory-path search omission in Rust.
2. Add tests and bump index format so stale indexes rebuild.
3. Add collapsed render mode and use it for the same memory-path cells.
4. Add TUI automation preview for action-producing automation.
5. Rename `rules.js` to `automation.js`.
6. Revisit JS-driven index/display policies only after the Rust policy surface is proven.
