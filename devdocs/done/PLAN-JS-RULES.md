# Plan: JavaScript Session Rules

## Goal

Add a small automation facility that lets AICS evaluate user-authored JavaScript rules over indexed chat sessions and apply Rust-owned actions such as moving matched sessions to trash.

The first useful workflow is bulk cleanup of low-value sessions, especially short command-only sessions such as `/commit` and `$gdf-commit` helper runs.

## CLI Shape

Use flags on the existing `aics` command rather than a subcommand.

Default rules file:

- `~/.config/aics/rules.js`

Planned flags:

- `--preview-rules` evaluates the rules and opens an interactive TUI preview without changing files.
- `--apply-rules` evaluates the rules and applies supported actions.
- `--rules PATH` overrides the rules file path for this run.

Rules mode should still perform the normal startup sync first, unless `--delete-index` exits earlier as it does today.

Initial CLI behavior:

1. `aics --preview-rules` uses `~/.config/aics/rules.js`.
2. `aics --apply-rules` uses `~/.config/aics/rules.js`.
3. `aics --preview-rules --rules ./rules.js` uses `./rules.js`.
4. `--preview-rules` and `--apply-rules` conflict.
5. `--preview-rules --json` stays headless and scriptable; plain `--preview-rules` launches the TUI.

Open decision:

- Whether `--preview-rules` should be implied when `--rules PATH` is provided. Prefer no for v1; require an explicit preview/apply flag so pointing at a rules file is never accidentally destructive.

## Why JavaScript

The KDL rule sketch is readable for simple AND-only criteria, but it becomes a language once we add:

- regex flags,
- OR/grouping,
- turn indexing,
- cross-role conditions,
- reusable predicates,
- rule-local explanations,
- action composition.

Use Boa for an embedded JavaScript runtime, but keep IO and mutations in Rust. JavaScript decides whether a session matches and returns action descriptions. Rust validates and performs those actions.

## Non-Goals

- No shelling out.
- No Node compatibility layer.
- No filesystem, network, subprocess, environment, or clipboard access from rules.
- No arbitrary mutation of session files from JavaScript.
- No interactive TUI rule editor in the first pass.
- No automatic background rule execution on ordinary searches.

## Rule File Contract

Start with a tiny global API. The rules file calls `rule(name, callback)` one or more times.

Example:

```js
rule("trash short spark commit sessions", ({ session, turns, re }) => {
  if (!/.*-spark.*/.test(session.model ?? "")) {
    return nothing();
  }
  if (turns.user.length !== 2) {
    return nothing();
  }
  if (turns.agent.length < 2 || turns.agent.length > 3) {
    return nothing();
  }
  if (!re(String.raw`\s*[/$](gdf-)?commit\b`, "m").test(turns.user[0].text(4096))) {
    return nothing();
  }

  return trash("short commit-helper session");
});
```

Rules can also return arrays for future composition:

```js
rule("example", ({ session }) => {
  if (session.agent !== "codex") {
    return nothing();
  }
  return [tag("codex"), trash("test rule")];
});
```

For v1, support only:

- `nothing()`
- `trash(reason?)`

Defer `tag(...)` until AICS has a tag storage model.

## Session Object

Expose stable, JSON-like data derived from `StoredSession` plus a small projection of parsed transcript cells.

Suggested shape:

```js
{
  session: {
    id,
    agent,              // "claude" | "codex"
    project,
    cwd,
    branch,
    path,
    modifiedTs,
    lines,
    derivationType,     // "original" | "trimmed" | "continued" | "sub_agent"
    isSidechain,
    customTitle,
    model,
    modelProvider,
    approvalPolicy,
    sandboxMode,
    trashed
  },
  turns: {
    user: [{ index, timestamp }],
    agent: [{ index, timestamp }],
    system: [{ index, timestamp }],
    toolCalls: [{ index, tool, summary, timestamp }],
    toolResults: [{ index, tool, isError, timestamp }],
    exec: [{ index, command, cwd, exitCode, timestamp }],
    patches: [{ index, files, success, timestamp }]
  }
}
```

Implementation note:

- `StoredSession` already carries cheap list/search metadata.
- Full `turns` requires reparsing the selected session file with `parse_session_file`.
- The data-heavy fields are message bodies, tool-result output, exec stdout/stderr, and patch file content. These values stay in a Rust-side detail map for the current session and are retrieved only when a rule calls `text(limit)`, `stdout(limit)`, `stderr(limit)`, or `content(limit)`. The limit argument is optional; the current stress-test default is effectively unbounded, but normal rules should pass explicit limits.
- In preview/apply mode, parse full sessions only after cheap metadata filters have been evaluated. A later two-stage API can avoid parsing sessions that only need indexed metadata.

## Runtime Safety

Configure the Boa runtime defensively:

1. Set loop iteration, recursion, and stack limits.
2. Add a wall-clock timeout guard if Boa exposes an interrupt-style hook or if rule evaluation moves behind a cancellable worker boundary.
3. Do not install module loaders unless needed.
4. Do not expose Rust functions that perform IO directly from JavaScript.
5. Treat thrown JS exceptions as rule failures, report them with rule name and file path, and continue unless a strict mode is added later.

Default behavior should be defensive and batch-friendly:

- malformed rules file: fail the command before evaluating sessions,
- malformed individual session: warn/skip, consistent with parser/indexer policy,
- rule exception for one session: warn/skip that rule result for that session.

## Action Model

Represent JavaScript results as Rust-owned action proposals.

```rust
pub enum RuleAction {
    Nothing,
    Trash { reason: Option<String> },
}

pub struct RuleProposal {
    pub rule: String,
    pub session_id: String,
    pub path: PathBuf,
    pub action: RuleAction,
}
```

Rules are pure from AICS's perspective. Applying happens after evaluation:

1. Evaluate rules and collect proposals.
2. De-duplicate conflicting proposals by session path.
3. Print proposal output.
4. If `--apply-rules`, apply Rust-owned actions.
5. Refresh the index after applying.

Conflict policy for v1:

- Multiple `trash` proposals for the same normal session collapse into one trash operation.
- `trash` proposals for already trashed sessions are ignored unless future actions explicitly target trash contents.
- `nothing` is not a proposal.

## Output

Preview mode has two surfaces:

- `aics --preview-rules` opens a TUI review surface for interactive inspection, marking, and confirmation.
- `aics --preview-rules --json` prints JSONL proposals.
- `aics --apply-rules --json` prints JSONL applied-action records.

Suggested JSONL preview record:

```json
{"rule":"trash short spark commit sessions","action":"trash","reason":"short commit-helper session","session_id":"...","path":"...","agent":"codex"}
```

Suggested TUI rule row under each matching session:

```text
[x] rule trash short spark commit sessions => trash · short commit-helper session
```

## Target Files

Likely implementation files:

- `Cargo.toml` - add `boa_engine`.
- `src/main.rs` - add `--preview-rules`, `--apply-rules`, and `--rules`.
- `src/lib.rs` - export a new automation module.
- `src/rules/mod.rs` - rule runtime, public entry points.
- `src/rules/js.rs` - Boa setup and JS API binding.
- `src/rules/projection.rs` - convert `StoredSession` / `Session` / `SessionCell` to JS-facing data.
- `src/rules/actions.rs` - action proposal types and application logic.
- `src/trash.rs` - reuse `TrashStore::trash_file`; avoid duplicating trash semantics.
- `tests/rules.rs` - CLI/runtime behavior tests.
- `tests/fixtures/rules/*.js` - rule fixtures.

## Implementation Phases

### Phase 1: CLI and Empty Runtime

1. Add CLI flags and conflicts.
2. Resolve default rules path to `config_dir()?.join("rules.js")` or equivalent.
3. Add a rules-mode branch before normal TUI launch.
4. Load missing default file as a clear error:
   - `rules file not found: ~/.config/aics/rules.js`
5. Add tests for flag parsing and conflicts.

Acceptance:

- `cargo test` passes.
- `aics --preview-rules --rules missing.js` fails before launching the preview TUI.

### Phase 2: JS Rule Registration

1. Add `boa_engine`.
2. Create a runtime with loop/recursion/stack limits.
3. Expose `rule`, `nothing`, and `trash`.
4. Evaluate the rules file once and collect registered callbacks.
5. Validate duplicate rule names as errors.

Acceptance:

- A fixture rules file registering two rules produces two registered rule names.
- Duplicate rule names fail with a clear error.
- A syntax error points at the rules file.

### Phase 3: Session Projection and Preview

1. Iterate indexed sessions through the existing search/index path.
2. Parse each candidate session file and build the JS-facing context.
3. Invoke each rule callback.
4. Convert returned actions into `RuleProposal`.
5. Populate preview records for both the interactive TUI and JSONL output.

Acceptance:

- A fixture rule matching a known test session emits one JSONL preview proposal.
- A rule returning `nothing()` emits no proposal.
- A rule that throws for one session reports the error and continues.

### Phase 4: Apply Trash

1. Reuse `TrashStore::trash_file` for normal sessions.
2. Skip already trashed sessions.
3. Refresh the index after successful applications.
4. Report applied/skipped/failed counts.

Acceptance:

- A temp session fixture is copied into AICS trash metadata and removed from its original location.
- Preview mode leaves files untouched.
- Apply mode is covered by a temp-dir test using `AICS_DATA_ROOT`.

### Phase 5: Polish and Docs

1. Add README usage notes.
2. Add a starter `rules.js` example.
3. Document the JS API and exposed session fields.
4. Consider `--rules-strict` if continuing after per-session JS exceptions is too surprising.

## Testing Notes

Use temp homes/cache/data roots for end-to-end tests:

- `AICS_DATA_ROOT`
- `AICS_CACHE_ROOT`
- `--claude-home`
- `--codex-home`

Keep tests focused:

- parser/projection unit tests for JS context shape,
- JS runtime unit tests for rule registration/action conversion,
- one integration test for preview,
- one integration test for apply/trash.

## Future Extensions

- `keep(reason?)` as an explicit rule-terminal action if rule ordering starts to matter.
- `tag(name)` after a tag storage model exists.
- `hide(reason?)` for non-destructive local filtering.
- `summarize()` only if summary-worker behavior becomes safe in headless mode.
- Rule packs under `~/.config/aics/rules.d/*.js`.
- Per-rule enabled/disabled config.
- Two-stage evaluation: cheap metadata predicate first, full transcript projection only for likely matches.
