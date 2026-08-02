# Search and indexing

AICS builds a local Tantivy index of Claude Code and Codex CLI session files. It
synchronizes the index at startup, then searches the indexed metadata and parsed
transcript content.

## Session sources

The default session roots are:

- `~/.claude/projects/` for Claude Code
- `~/.codex/sessions/` for Codex CLI

Claude and Codex homes follow `CLAUDE_CONFIG_DIR` and `CODEX_HOME`. For a single
run, `--claude-home PATH` and `--codex-home PATH` override those homes. When the
corresponding CLI home override is not used, `AICS_CLAUDE_PROJECTS_DIR` and
`AICS_CODEX_SESSIONS_DIR` override the indexed roots directly.

`AICS_CLAUDE_SESSIONS_DIR` separately overrides the Claude session directory
used for live-session detection.

## Incremental indexing

On startup, AICS scans the session roots and compares each file with its saved
index state. Unchanged files are skipped, new and changed files are parsed and
indexed, and records for deleted files are removed. Malformed or unrecognized
session data is skipped rather than crashing the scan.

Fork lineage and stable semantic event IDs are cached in the same state. AICS
groups forks by their declared parent session ID, then checks only those direct
parent/child candidates for strict event-set coverage. This avoids all-pairs
transcript comparison. An ordinary search reads the cached `superseded_by`
property; when a changed or deleted fork alters a parent's status, only that
parent is refreshed in addition to the changed files.

Codex may leave a final aborted turn in the parent while creating a fork. AICS
accepts two narrow forms of this exception. It can ignore an otherwise-empty
trailing user/`<turn_aborted>` pair when the child contains new assistant or tool
activity. For older Codex records, where those boundary messages have no stable
IDs and the parent may have begun working, AICS requires every unmatched parent
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
