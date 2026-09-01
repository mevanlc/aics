# Search and indexing

AICS builds a local Tantivy index of Claude Code, Codex CLI, and Antigravity CLI
sessions. It synchronizes the index at startup, then searches the indexed
metadata and parsed transcript content.

## Session sources

The default session roots are:

- `~/.claude/projects/` for Claude Code
- `~/.codex/sessions/` for Codex CLI
- `~/.gemini/antigravity-cli/` for Antigravity CLI

Claude and Codex homes follow `CLAUDE_CONFIG_DIR` and `CODEX_HOME`. For a single
run, `--claude-home PATH` and `--codex-home PATH` override those homes. When the
corresponding CLI home override is not used, `AICS_CLAUDE_PROJECTS_DIR` and
`AICS_CODEX_SESSIONS_DIR` override the indexed roots directly.

`AICS_CLAUDE_SESSIONS_DIR` separately overrides the Claude session directory
used for live-session detection.

Set `AICS_ANTIGRAVITY_HOME` or pass `--antigravity-home PATH` to override the
Antigravity home. Each `brain/<conversation-id>/` directory is one logical
session. AICS requires its `.system_generated/logs/transcript.jsonl`, uses
`transcript_full.jsonl` as a richer companion when present, and reads title,
preview, and workspace metadata from the Antigravity cache and `history.jsonl`.
When regular and full transcripts contain the same `step_index`, the full record
wins; regular-only tail records remain visible.

Moving an Antigravity session to AICS Trash preserves its complete
`brain/<conversation-id>/` artifact directory and local
`conversations/<conversation-id>.db` companions. Trashed bundles remain
searchable in AICS with the trash filter, cannot be resumed while trashed, and
can be restored to their original Antigravity home. Permanent deletion removes
the same complete local bundle.

## Incremental indexing

On startup, AICS scans the session roots and compares each logical session with
its saved index state. Unchanged sessions are skipped, new and changed sessions
are parsed and indexed, and records for deleted sessions are removed. For an
Antigravity bundle, changes to either transcript or the cache metadata invalidate
the indexed record. Malformed or unrecognized session data is skipped rather
than crashing the scan.

Fork lineage and stable semantic event IDs are cached in the same state. AICS
uses declared parent session IDs to form fork families, then checks direct
parent/child candidates for event-set coverage and groups equal semantic event
sets within each family. This avoids comparing unrelated transcripts. An
ordinary search reads the cached `superseded_by` property; when a changed or
deleted fork alters the family collapse, affected family members are refreshed
in addition to the changed files.

Some paginated Codex forks store only a local suffix and name another rollout in
`session_meta.payload.history_base` for their inherited prefix. If any member of
a declared fork/reference family has such an external history dependency, AICS
does not mark any session in that family as superseded. This keeps the visible
set of required source rollouts out of the superseded review set, where users may
choose sessions for deletion.

Codex may leave a final aborted turn in the parent while creating a fork. AICS
accepts two narrow forms of this exception. It ignores an otherwise-empty
trailing user/`<turn_aborted>` pair when comparing semantic equivalence or when
the child contains new assistant or tool activity. For older Codex records,
where those boundary messages have no stable IDs and the parent may have begun
working, AICS requires every unmatched parent
event to belong to that trailing aborted turn, requires the child to retry the
same multiset of nonempty user-message lines (allowing reordered lists or table
rows), and requires new assistant or tool activity in that retry. An unmatched
event outside the aborted turn or a changed retry line still prevents
supersession.

Use `--rebuild-index` to discard and rebuild the current profile's index before
searching. Use `--delete-index` to delete it and exit.

## Index profiles and files

By default, AICS stores one profile per discovered session-root set under:

```text
~/.cache/aics/profiles/<profile-id>/
```

Each profile can contain:

- `index/` — Tantivy index files
- `index_state.json` — fingerprints and indexing state for scanned files
- `profile.json` — profile metadata
- `hashed-input.txt` — the session-root data used to identify the profile
- `rules-cache.json` — explicit all-rules determinations
- `startup-rules-cache.json` — automatic startup-rule determinations

Set `AICS_CACHE_ROOT` to override the cache root. The profile directory is still
created beneath `<AICS_CACHE_ROOT>/profiles/`.

## What is searched

An empty query shows recent sessions. A non-empty query searches an indexed
content field containing the custom thread title, first user or resume-preview
text, and the full parsed transcript.

Field prefixes narrow a query to semantic parts of the source session:

- `user:TEXT` searches user-authored prompt text. Source-generated context and
  meta messages are excluded.
- `agent:TEXT` searches assistant prose, plaintext reasoning, and native
  in-session summaries or checkpoints. It excludes system/developer context,
  tool/MCP/skill traffic, and AICS-generated summary sidecars.
- `toolcall:TEXT` searches readable tool names, inputs, and actions.
- `toolresult:TEXT` searches readable tool output. Opaque call IDs, signatures,
  binary/media payloads, and internal metadata are excluded.
- `dirs:PATH` searches JSON properties known to hold directory paths, including
  working directories, workspace roots, and writable roots.
- `files:PATH` searches properties known to hold file paths, including tool file
  arguments and path-keyed change or backup maps.
- `paths:PATH` searches the union of `dirs:` and `files:` plus properties whose
  values can be either files or directories, such as `SearchPath`,
  `AbsolutePath`, generic sandbox paths, and similar ambiguous path properties.

The three path fields use the same case-insensitive, path-component-prefix
matching as `wd:`. They come from a semantic property allowlist; AICS does not
guess from slashes in arbitrary text or whether a path currently exists. Bare
queries retain the existing `content` behavior and therefore can still match
tool text as part of the full parsed transcript.

Three position-independent modifiers control how bare query terms interact with
the current ^F Visibility toggles:

- `visible:` searches only transcript content that the toggles currently show.
- `hidden:` searches only transcript content that the toggles currently hide.
- `all:` searches all indexed transcript content regardless of the toggles. This
  is also the behavior when no visibility modifier is present.

The modifiers are mutually exclusive and may appear at the beginning, middle,
or end of a query. Explicit field clauses are not constrained by them, so
`visible: rust toolcall:cargo` still searches `toolcall:cargo` when tool calls
are hidden. For structured exec and patch cells, output counts as hidden when
either Tool Calls or Tool Results hides it, matching what the transcript viewer
can display.

Queries use Tantivy's lenient query parser:

- Bare words are token searches and multiple bare words are ANDed by default.
- Use uppercase `AND`, `OR`, and `NOT` for explicit boolean logic.
- Use parentheses to group clauses, such as `(rust OR go) parser`.
- Use quotes for an exact phrase, such as `"vector db"`.
- Use `working_dir:PATH` or its `wd:PATH` alias to match a case-insensitive
  working-directory prefix beginning at any path-component boundary. For example,
  `wd:my/ja` matches `/Users/me/p/my/javafx-ax` and `/Users/me/p/my/jave7`.
- The same component-prefix behavior applies to `dirs:PATH`, `files:PATH`, and
  `paths:PATH`.
- Wrap a Tantivy term regex in `<` and `>`, optionally after a field name. Slashes
  are ordinary regex characters and need no query-language escaping, as in
  `wd:<.*codex/.*8ba3f7e.*>`. Regexes match whole indexed terms, so use `.*` for
  substring matching. Without a field prefix they target the `content` field's
  lowercase word-like terms. Write `\>` for a literal `>` in the regex; an odd
  run of backslashes escapes the delimiter and the outer parser removes exactly
  one.
- Malformed input is handled leniently; usable portions can still be searched.

For bare multi-word queries without explicit boolean operators, AICS also adds
an exact-phrase query with a 5x boost. Time sort orders matches by modification
time. Relevance sort starts with Tantivy relevance, applies an AICS recency
boost, and uses timestamps as tie-breakers.

Scope, agent, branch, date, line-count, derivation, sub-agent, live, superseded,
and trash filters can exclude otherwise matching sessions. Snippets prefer
Tantivy-selected fragments and fall back to session text when no fragment is
available.

[Back to the README.](../README.md#indexing)
