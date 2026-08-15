# Plan: Antigravity Session Support

## Goal

Add Antigravity CLI (`agy`) as a first-class AICS session source without
treating its multi-file conversation directory as a disposable single JSONL
file.

Canonical public agent value: `antigravity`. The CLI also accepts `agy` as an
input alias.

## Source contract

- Default home: `~/.gemini/antigravity-cli`
- Override: `--antigravity-home PATH` or `AICS_ANTIGRAVITY_HOME`
- One logical session per `brain/<conversation-id>/`
- Required primary source:
  `.system_generated/logs/transcript.jsonl`
- Optional richer companion:
  `.system_generated/logs/transcript_full.jsonl`
- Optional metadata:
  `cache/conversation_metadata.json` and `history.jsonl`

The regular and full transcripts are merged by `step_index`. The full record
wins when both sources contain the same step. Regular-only tail steps are kept,
which covers a full transcript that stopped updating before the regular one.
Malformed lines and unknown record kinds are skipped defensively.

## Parsing

- Unwrap `<USER_REQUEST>` while excluding Antigravity metadata/settings wrappers
  from searchable user text.
- Map planner content to assistant messages and planner thinking to reasoning.
- Pair planner `tool_calls` with following model result records.
- Support the current argument representation and the older result-record
  layout, including JSON-string-encoded arguments.
- Map `run_command` to typed exec cells and preserve other tools as generic tool
  call/result cells.
- Include checkpoint summaries in search; omit `CONVERSATION_HISTORY`.
- Populate model, title, preview, working directory, timestamps, tool metrics,
  and the conversation-directory session ID when available.

## Discovery, indexing, and rules

- Represent each conversation as one scanned `SessionFile` with companion paths,
  resolved metadata, and a bundle source signature.
- Include the Antigravity root in cache-profile identity.
- Invalidate index and rules cache entries when either transcript or resolved
  metadata changes.
- Keep Antigravity sessions unrelated for Claude/Codex fork-supersession logic.
- Expose `session.agent === "antigravity"` to JavaScript rules.
- Allow rules to inspect Antigravity sessions, but safely skip `trash` and
  `untrash` proposals because those actions currently operate on single files.

## CLI and TUI

- Add Antigravity to CLI and modal agent filters, badges, themes, list titles,
  previews, viewer rendering, exports, and summaries.
- Add `antigravity_command` and `antigravity_args` settings, defaulting to `agy`
  and `--dangerously-skip-permissions`.
- Resume with `agy --conversation <conversation-id>` plus configured arguments.
- Hide resume for a non-default Antigravity home because `agy` exposes no
  equivalent data-root override.
- Offer only safe actions: resume, view, summarize, export, and copy ID/path/
  conversation directory.
- Do not offer fork, resume-in-current-CWD, trash, or immediate delete. Guard
  direct deletion shortcuts and rule application as well as the menu.

## Validation

- Add a fixture with regular/full transcript overlap, regular-only tail,
  metadata, history, wrapped user input, tool execution, and checkpoint data.
- Cover discovery, metadata resolution, full/regular merge, current and older
  parser forms, malformed lines, search/filter behavior, bundle reindexing,
  settings, resume command construction, safe action lists, and rules skips.
- Run `cargo fmt --check`, `cargo check --all-targets`, `cargo nextest run`, and
  `git diff --check`.

## Explicitly out of scope

- Reverse engineering Antigravity's undocumented SQLite/protobuf state
- Synthesizing or rewriting conversation state
- Forking or changing a conversation's stored working directory
- Trash/delete semantics for a whole Antigravity conversation bundle
- Live-state detection from persistent presence-lock files
