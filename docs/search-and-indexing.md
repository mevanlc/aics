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

## Live sessions

The Live badge and `--live` filter use provider-specific runtime markers:

- Claude: `<claude-home>/sessions/*.json`, with a live owner PID. Confirmed dead
  owners and registrations older than a reused PID's process are ignored.
- Codex: held native locks at `<codex-home>/thread-writer-locks/<session-id>.lock`.
- Antigravity: held native locks at `<antigravity-home>/presence/<conversation-id>.lock`.

Leftover unlocked lock files do not make a session live. Marker locations follow
the resolved homes, even when `AICS_CLAUDE_PROJECTS_DIR` or
`AICS_CODEX_SESSIONS_DIR` changes the indexed root. `AICS_CLAUDE_SESSIONS_DIR`
overrides Claude's marker directory unless `--claude-home` is supplied. All three
CLI home overrides take precedence over their corresponding environment values.

[Rules processing](rules-js.md#live-and-locked-sessions) excludes live/locked
sessions and conservatively skips sessions whose markers cannot be verified. A
provider-wide inspection failure does not give every session a Live badge.

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
searching. Use `--delete-index` to delete it and exit. Index format 18 adds source
coordinates and user-command visibility fields; older profiles rebuild
automatically. These coordinates stay inside the index and are excluded from
session JSON and exports.

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

An empty query shows recent sessions. A non-empty interactive query defaults to
**Visible** content, following the current `^F` Visibility toggles. **Search
content** in the same dialog selects Visible, All, or Hidden; Enter applies it,
`^S` applies and saves it as the startup preference, and `^R` resets it to Visible.
All searches the union of the same source-text segments used by Visible and
Hidden. Custom thread titles remain searchable in All and Visible, independently
of message visibility. The transcript projection is
shared across indexing, query matching, and display provenance; it retains
Markdown source syntax and readable tool arguments rather than indexing the
pretty-rendered screen.

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
queries can match tool text according to the selected search-content mode and
Visibility toggles.

Three position-independent modifiers control how bare query terms interact with
the current ^F Visibility toggles:

- `visible:` searches only transcript content that the toggles currently show.
- `hidden:` searches only transcript content that the toggles currently hide.
- `all:` searches all indexed transcript content regardless of the toggles.

A modifier temporarily overrides the selected Search content preference. Removing
it returns to the selected mode; saving defaults while an override is active
saves the selector's value. The selected row's help identifies an active override.
These modes govern bare terms, not which transcript blocks the viewer displays.

JSON/export searches default to All and ignore saved filter/display preferences.
Use a query modifier with `--hide` to search visible or hidden content there.
The Search content selector is unavailable in rules preview, which uses a
separate search over proposal metadata.

The modifiers are mutually exclusive and may appear at the beginning, middle,
or end of a query. Explicit field clauses are not constrained by them, so
`visible: rust toolcall:cargo` still searches `toolcall:cargo` when tool calls
are hidden. For structured exec and patch cells, output counts as hidden when
either Tool Calls or Tool Results hides it, matching what the transcript viewer
can display. User exec commands follow the separate User Tool Calls and User Tool
Results toggles; their output requires both toggles to allow it.

Internal/goal wrapper messages count as hidden when either **Internal Context**
or **User Messages** hides them. Internal Context is hidden by default. Use
`hidden: "Continue working toward the active thread goal"` to find those messages
while they are hidden, or `all:` to include them alongside visible content.

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
and trash filters can exclude otherwise matching sessions. Query highlights use
matching positive clauses with their field restrictions; negative clauses do not
produce highlights. Phrases and term regexes retain their query semantics rather
than being split into substring highlights.

In the full viewer, **Search** starts with the list query and can be edited
independently. Matching positive clauses are highlighted even when the complete
Boolean expression does not match that session; the Search box reports that
status separately. Source matches hidden by filters, removed by rendering, or
located only in metadata remain search evidence without becoming unrelated
highlights in the visible body. Match navigation follows individual occurrences,
including repeated matches on the same wrapped row.

Tantivy's matching of phrases with three or more terms and nonzero slop can
depend on term frequencies in the index segment. Session retrieval keeps
Tantivy's results. For these expressions, local phrase highlights and match
status can differ; the viewer shows this limitation instead of a definitive
query-mismatch notice. Exact phrases and two-term slop use the shared token
positions without this qualification.

The separate viewer-only **Find** box searches displayed readable text with
literal substring or Rust regex matching. It does not change the list query or
Tantivy syntax, and is not present in the preview. See the
[viewer controls and jump preference](keybindings.md#session-viewer).

[Back to the README.](../README.md#indexing)
