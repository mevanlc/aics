# Plan: Codex Session Presentation Enhancements

## Goal

Adopt the most useful "rendering ideas" from upstream Codex (`~/p/my/codex/codex-rs/tui`) so that AICS shows a Codex rollout the way a human would actually want to read one — distinct, well-shaped cells for tool calls, diffs, reasoning, plans, approvals, etc. — instead of the current flat role-based transcript.

## Real-Shape Adjustments (verified against `~/.codex/sessions/`)

After sampling ~750 real rollouts spanning 2025-08 through 2026-04:

- **No git metadata in rollouts** — `session_meta.payload` does NOT include git sha/branch/origin. Dropped from `SessionInfo`.
- **No approval events in rollouts** — `exec_approval_request` etc. live in-memory only and are not persisted. Dropped Approval cell.
- **`exec_command_end` (event_msg) is fully structured** — has `stdout`, `stderr`, `exit_code`, `duration{secs,nanos}`, `parsed_cmd`, `command`, `cwd`. Pair with `function_call` (`exec_command` / `shell_command`) by `call_id`. Far cleaner than parsing the legacy "Exit code: N\nWall time: X seconds\n..." text in `function_call_output`.
- **`patch_apply_end` (event_msg) has structured `changes`** — `{path: {type: "add"|"update"|"delete", content}}`. Pair with `custom_tool_call apply_patch` by `call_id`.
- **`turn_context` is where model/effort/approval_policy/sandbox_policy live** — not `session_meta`.
- **Reasoning may be encrypted-only** — `response_item.reasoning` sometimes has `summary: []` and only `encrypted_content`. Skip gracefully (no panic, no display).
- **`update_plan` shape confirmed** — `function_call` name=`update_plan`, `arguments` JSON with `{"plan": [{"status": "pending"|"in_progress"|"completed", "step": "..."}]}`.
- **`web_search_call` (response_item)** has `action.query` + `status`. **`web_search_end` (event_msg)** has `query`, `action.queries[]`, `call_id`. Pair by call_id; web_search_end's `queries` array supersedes single-query when present.
- Other observed event types worth tolerating but not specially rendering yet: `task_started`, `task_complete` (has `last_agent_message`), `context_compacted`, `turn_aborted`, `thread_rolled_back`, `ghost_snapshot`.

## Inspiration & Citations

All references below are inside `~/p/my/codex/`.

| # | Idea | Codex source |
|---|---|---|
| 1 | Runtime metrics block (token usage, timings, tool-call counts) | `codex-rs/tui/src/history_cell.rs:2750` |
| 2 | MCP / tool-call cells (active vs. completed, error badges, image output) | `codex-rs/tui/src/history_cell.rs:1491-1525, 3727-3970` |
| 3 | Diff rendering for `apply_patch` | `codex-rs/tui/src/diff_render.rs:302-475` |
| 4 | Reasoning summary blocks (parses `**header**` markers) | `codex-rs/tui/src/history_cell.rs:2660, 4727` |
| 5 | Session info / context cell (model, sandbox, instructions) | `codex-rs/tui/src/history_cell.rs:1163-1267, 1372-1430` |
| 6 | Approval decision cells | `codex-rs/tui/src/history_cell.rs:811` |
| 7 | Plan update cells | `codex-rs/tui/src/history_cell.rs:2456-2592` |
| 8 | Web search cell (query + result count) | `codex-rs/tui/src/history_cell.rs:1692-1758` |
| 9 | Request-user-input cell (multi-choice, masked secrets) | `codex-rs/tui/src/history_cell.rs:2313-2450` |
| 10 | Sensitive data redaction in tool I/O | `codex-rs/tui/src/history_cell.rs:3313` |
| 11 | Richer list metadata (sha, origin, model, provider, version) | `codex-rs/rollout/src/list.rs:44-74` |
| 12 | Cursor-paginated, sortable picker | `codex-rs/tui/src/resume_picker.rs:39-156` |
| 13 | Structured exec cells (command / stdin / stdout / exit) | `codex-rs/tui/src/history_cell.rs:562` |

## Current State In AICS

Relevant files:

- `src/parse/session.rs` — `Session` and `SessionMessage` data model
- `src/parse/codex.rs` — rollout JSONL parser
- `src/parse/tool_format.rs` — pretty-printers for tool name / input / output
- `src/tui/preview.rs`, `src/tui/viewer.rs` — transcript rendering
- `src/tui/list.rs` — session picker rows
- `src/tui/markdown.rs` — markdown body rendering
- `src/index/schema.rs` — Tantivy schema (`content`, `file_path`, `modified_ts`, `session_json`)

Today the parser produces a flat `Vec<SessionMessage>` keyed only by `MessageRole` (`User`, `Assistant`, `System`, `Summary`, `ToolCall`, `ToolResult`). The renderer iterates that vector and prints role label + body. Tool calls get a one-line summary from `tool_format::format_tool_call`. There is no concept of:

- a typed event/cell richer than role+text,
- patch / diff display,
- reasoning vs. assistant separation,
- token / latency metrics,
- session-level "context block" rendered up-front,
- approval / plan / web-search / user-input events,
- redaction.

The list rows show: agent badge, custom_title-or-cwd, relative time, line count, live status. They do not carry git sha, origin URL, model, or provider.

## Design Principle

Do the data-model work once, then render. Most of these features cost very little in the renderer if the parser produces a typed cell. The current `MessageRole`-only model is the thing forcing the renderer to be flat.

Therefore: introduce a `SessionCell` enum (or extend `SessionMessage` with a typed `payload` enum) as a one-time foundational change, and keep `Vec<SessionMessage>` for backwards compatibility with search/snippet code paths.

## Non-Goals

- Reproducing Codex's full TUI widget set or color scheme.
- Wire-compatible adoption of Codex's protocol types — we map on read.
- Live / streaming rendering of in-progress cells (AICS only reads completed rollouts).
- Image rendering inside the terminal. Detect images, mark "[image: NxM]".
- Per-cell collapse/expand state persistence.

## Data-Model Foundation (Phase 0)

Target files:

- `src/parse/session.rs`
- `src/parse/codex.rs`
- `src/parse/claude.rs` (only where the same payload exists)

Steps:

1. Add a `SessionCell` enum in `src/parse/session.rs`:

   ```rust
   pub enum SessionCell {
       Message { role: MessageRole, content: String },
       Reasoning { header: Option<String>, body: String },
       ToolCall {
           tool: String,           // canonical label from tool_format::tool_label
           raw_name: String,       // original
           summary: String,        // single-line summary for list/snippet
           input: serde_json::Value,
           status: ToolStatus,     // Pending | Completed | Failed
       },
       ToolResult {
           tool: Option<String>,
           output: String,
           is_error: bool,
           output_kind: ToolOutputKind, // Text | Diff | Image { w, h } | Json
       },
       Exec {
           command: String,
           stdout: String,
           stderr: String,
           exit_code: Option<i32>,
           duration_ms: Option<u64>,
       },
       Patch { files: Vec<PatchFile> },         // parsed apply_patch
       WebSearch { query: String, result_count: Option<usize> },
       Plan { items: Vec<PlanItem> },
       Approval { request: String, decision: ApprovalDecision },
       UserInputRequest { prompt: String, choices: Vec<String>, secret: bool, response: Option<String> },
       SessionInfo(SessionInfo),                // see Phase 1
       Metrics(RuntimeMetrics),                 // see Phase 6
   }
   ```

2. Add `cells: Vec<SessionCell>` alongside the existing `messages: Vec<SessionMessage>` on `Session`. Do NOT remove `messages` — keep it as the de-duplicated, role-only projection used by search snippets, custom-title fallback, and Phase 4 of `PLAN-CODEX-SESSION-PARSE-PARITY.md`.

3. Extend `parse/codex.rs::handle_response_item` and `handle_event_msg` so each branch also pushes the appropriate `SessionCell`. The existing `messages` push logic stays unchanged so existing snippets / sticky-headers / index format do not regress.

4. For Claude, populate the subset that maps cleanly (`Message`, `Reasoning`, `ToolCall`, `ToolResult`, `Patch` from `apply_patch`). Skip Codex-only cells (`Plan`, `Approval`, `WebSearch`, `UserInputRequest`) until/unless we identify Claude analogues.

5. Renderer impact: introduce a `render_cell(&SessionCell, ...) -> DisplayDocument` dispatcher in `src/tui/preview.rs` and route through it from `render_session_document`. The default arm falls back to the current message renderer.

Acceptance:

- `cargo test` still passes — no behavior change for sessions that produce only `Message`/`ToolCall`/`ToolResult` cells.
- A new fixture-driven test confirms that a known Codex rollout produces at least one `Reasoning`, one `ToolCall`, and one `ToolResult` cell of expected shape.

## Phase 1: Session Info Block

Target files: `src/parse/codex.rs`, `src/tui/viewer.rs`.

Render an info block as the first cell of the viewer (and optionally the first preview line).

1. Define `SessionInfo`:

   ```rust
   pub struct SessionInfo {
       pub model: Option<String>,
       pub model_provider: Option<String>,
       pub reasoning_effort: Option<String>,
       pub approval_policy: Option<String>,
       pub sandbox_mode: Option<String>,
       pub cwd: Option<String>,
       pub git_branch: Option<String>,
       pub git_sha: Option<String>,
       pub git_origin: Option<String>,
       pub cli_version: Option<String>,
       pub source: Option<String>,        // CLI / VSCode / Custom
       pub agent_nickname: Option<String>,
   }
   ```

2. Populate from `session_meta` and `turn_context` records (already partially read for `cwd`). Search the rollout for `originator`, `cli_version`, `model`, `provider`, `approval_policy`, `sandbox_policy`, and any `git` payload (codex stores these on `session_meta.payload`).

3. Surface as both a viewer cell (top of transcript, dim background) and as fields on `Session`. The list / preview can use any of them later.

Acceptance:

- A real Codex rollout that includes `session_meta.payload.git.{branch,sha,origin}` produces a populated `SessionInfo`.
- Viewer renders a 2-3 line summary block above the first message.

## Phase 2: Structured Tool / Exec / Patch Cells

Target files: `src/parse/codex.rs`, `src/parse/tool_format.rs`, `src/tui/preview.rs`.

Steps:

1. **Exec cells (idea 13):** when a `function_call` has tool label `bash` and the next `function_call_output` matches it, fold both into a single `Exec` cell. Parse stdout/stderr/exit_code from the codex output shape (`{ "output": "...", "metadata": { "exit_code": N, "duration_ms": N } }` — verify shape against fixtures). Render as:

   ```
   $ <cmd>                                         (1.2s, exit 0)
   ┊ <stdout, dim>
   ! <stderr, red>
   ```

2. **Patch cells (idea 3):** when an `apply_patch` tool call body is captured, parse the V4A patch envelope (`*** Update File:` / `*** Add File:` / `*** Delete File:`) into `PatchFile { path, op, hunks: Vec<Hunk> }`. Render with green/red lines and a per-file `+N -M` summary on the header line. Keep this self-contained in `src/tui/patch_render.rs` to mirror codex's `diff_render.rs`.

3. **Tool-call cells (idea 2):** generalize the existing tool path. Track `ToolStatus` by pairing call to output via `call_id` (codex emits one). When pairing fails, render as `Pending`. When the output payload contains `"is_error": true` or stderr-only output, mark `Failed` and render with a red badge.

4. **Image outputs:** detect `data:image/...` or `{ "type": "image", "image_url": ... }` blocks in tool output. Replace the bytes with `[image: <kind>]` so we never spam the terminal.

Acceptance:

- Bash exec round-trip renders as one folded cell with timing.
- An `apply_patch` rollout entry renders as a colored diff with per-file summary line.
- A failing tool call shows a "FAILED" badge.

## Phase 3: Reasoning, Plan, Approval, Web-Search, User-Input

Target files: `src/parse/codex.rs`, `src/tui/preview.rs`.

These are smaller, mostly mechanical mappings on top of Phase 0:

1. **Reasoning (idea 4):** map `response_item.reasoning.summary[*].text` and `event_msg.agent_reasoning.text` into `SessionCell::Reasoning`. Split on the first `**...**` line into `header` and `body`. Render dim, italicized, prefixed with `↳ thinking`. Currently this content is folded into `content_chunks` — keep that for search but push a structured cell as well.

2. **Plan (idea 7):** intercept `function_call` whose tool name normalizes to `update_plan` / `plan` and parse the JSON arguments into `Vec<PlanItem { status, text }>`. Render as a checklist (`[x] / [ ] / [-]`).

3. **Approval (idea 6):** Codex emits `event_msg.exec_approval_request` / `apply_patch_approval_request` plus a corresponding response. Map both into a single `SessionCell::Approval`.

4. **Web search (idea 8):** intercept the `web_search` tool family. Pull `query` from input and `result_count` from output JSON when shaped as `{ "results": [...] }`.

5. **User-input request (idea 9):** intercept the codex "request user input" function-call shape. Mark fields whose name contains "secret"/"token"/"password" as `secret: true` and render their `response` as `••••••`.

Each of these is a `match`-arm addition on top of Phase 0 plumbing — they should not require new cross-cutting infrastructure.

Acceptance: per-feature fixture test that the cell type appears with correct fields.

## Phase 4: Sensitive Data Redaction (DEFERRED)

Not in scope for this pass. Codex's `mcp_tools_output_masks_sensitive_values` (`history_cell.rs:3313`) is a display-layer heuristic that masks string values under sensitive-looking keys (`token`, `secret`, `api_key`, etc.). Worth revisiting later if AICS gains sharing/screenshot workflows; for a local-only tool the value is low.

## Phase 5: List Metadata Enrichment

Target files: `src/parse/session.rs`, `src/parse/codex.rs`, `src/tui/list.rs`, `src/index/reader.rs`.

1. Promote a subset of `SessionInfo` to top-level `Session` fields so list rows can read them without parsing the body: `model`, `model_provider`, `git_sha`, `git_origin`, `cli_version`, `source`. (`branch` and `cwd` already exist.)

2. Update `tui/list.rs::render_item` to render a second metadata line in non-compact mode:

   ```
   ⏱ 14m ago · 245 lines · live
   ⌥ opus-4.7 · main@a3f71c · github.com/me/aics
   ```

   Only show fields that are populated; fall back gracefully.

3. Tantivy storage: these are encoded inside `session_json`, so no schema migration is required. Bump an internal `INDEX_FORMAT_VERSION` (already present, see `src/index/schema.rs` if extended) so stale indexes are rebuilt on load.

Acceptance:

- Codex rows show model + sha + origin when present in the rollout.
- Old indexes rebuild on first launch without crashing.

## Phase 6: Runtime Metrics

Target files: `src/parse/codex.rs`, `src/tui/viewer.rs`.

Codex rollouts contain per-turn token usage and inference timings. Aggregate them across the rollout into:

```rust
pub struct RuntimeMetrics {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub tool_call_count: u64,
    pub tool_failure_count: u64,
    pub total_wall_ms: u64,
    pub api_inference_ms: u64,
}
```

Steps:

1. Walk `event_msg.token_count` (or whichever field codex emits — verify against fixtures) on parse and sum.
2. Compute wall time from min/max of message timestamps (already tracked).
3. Render as a bottom-of-viewer footer cell.

Acceptance: a real rollout produces non-zero metrics matching `wc -l` order-of-magnitude expectations.

## Phase 7: Pagination & Sorting In Picker (Optional / Stretch)

Target files: `src/index/reader.rs`, `src/tui/list.rs`, `src/tui/app.rs`.

Today the picker loads all hits at once. Codex's resume picker uses cursor-based paging. AICS only needs this if/when users actually have thousands of rollouts. Defer until that happens; treat as a follow-on plan rather than part of this work.

If pursued: add a `(modified_ts, file_path)` cursor, fetch in chunks of N, and add `sort_mode` toggle in `Settings` (`Modified` vs. `Created`).

## Implementation Order

1. **Phase 0** — `SessionCell` enum + dispatcher (foundation; nothing user-visible yet).
2. **Phase 1** — `SessionInfo` cell (immediate visible win, low risk).
3. **Phase 2** — Exec / Patch / Tool-call cells (high impact for readability).
4. **Phase 3** — Reasoning / Plan / Approval / WebSearch / UserInput (small per-cell, batch them).
5. **Phase 4** — Deferred.
6. **Phase 5** — List metadata (cosmetic but cheap).
7. **Phase 6** — Runtime metrics.
8. **Phase 7** — Pagination, only on demand.

Each phase should land as its own commit with passing `cargo test` and fresh fixture coverage.

## Risks

### Cell duplication vs. messages duplication

`Session.cells` and `Session.messages` will hold overlapping data. That is intentional — `messages` is the search/snippet projection, `cells` is the render projection. Document this clearly in `session.rs` so future contributors do not "deduplicate" one into the other.

### Codex format drift across versions

The plan assumes `function_call` / `function_call_output` / `event_msg` shapes from current Codex. Older fixtures in `tests/fixtures/sessions/codex/old_format.jsonl` and `latest_format.jsonl` must remain green; new cell extraction must be tolerant (`if let Some(...)` everywhere, no required fields beyond `type`).

### Patch parser scope creep

V4A patch parsing is well-defined but has edge cases (binary files, renames). Cap initial scope to text additions/updates/deletions; mark binary patches as `[binary patch]`.

### Index format churn

Adding cells to the stored `session_json` invalidates old indexes. Make sure the version bump triggers a clean rebuild rather than a panic on deserialize.

## Testing Plan

For each phase, add a fixture and a parser unit test:

- `tests/fixtures/sessions/codex/with_exec.jsonl` — single bash round-trip
- `tests/fixtures/sessions/codex/with_patch.jsonl` — apply_patch with two files
- `tests/fixtures/sessions/codex/with_reasoning.jsonl` — reasoning summary
- `tests/fixtures/sessions/codex/with_plan.jsonl` — plan tool calls
- `tests/fixtures/sessions/codex/with_approval.jsonl` — approval request + decision
- `tests/fixtures/sessions/codex/with_websearch.jsonl` — search query + results

Render tests can assert against rendered `Text` content to lock in cell shape.

## Definition Of Done

- `Session.cells: Vec<SessionCell>` is populated from Codex rollouts.
- Viewer renders typed cells: session info, reasoning, exec, patch, tool call, plan, approval, web search, user input — each visually distinct.
- List rows show model + sha + origin when present.
- Bottom of viewer shows runtime metrics for Codex sessions.
- All existing search / snippet / sticky-header behavior is unchanged.
- All fixture tests in `tests/fixtures/sessions/{claude,codex}/` continue to parse.
