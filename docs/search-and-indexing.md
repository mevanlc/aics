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

Queries use Tantivy's lenient query parser:

- Bare words are token searches and multiple bare words are ANDed by default.
- Use uppercase `AND`, `OR`, and `NOT` for explicit boolean logic.
- Use parentheses to group clauses, such as `(rust OR go) parser`.
- Use quotes for an exact phrase, such as `"vector db"`.
- Use `working_dir:PATH` or its `wd:PATH` alias to match a case-insensitive
  working-directory prefix beginning at any path-component boundary. For example,
  `wd:my/ja` matches `/Users/me/p/my/javafx-ax` and `/Users/me/p/my/jave7`.
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
