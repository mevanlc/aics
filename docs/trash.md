# Trash and recovery

Press **^T** to trash the selected session from the session list or viewer.
**Enter, then d** performs the same action through the actions menu and works
without function keys, including on Termux. Ctrl+D does not trash sessions or
process rules. In rules preview, ^T opens confirmation to process marked proposals.

Show trashed sessions with the **Trashed** filter or `--trashed yes`, then use
**Undo Trash** in the actions menu to restore one. Trashing an already trashed
session opens confirmation for permanent deletion. Antigravity actions include the whole conversation
bundle and its database companions.

## Dependency review

Codex rollouts sharing a thread ID are changed together. Removing history also
requires removing its transitive dependents; an independent leaf can be removed
alone. Restoring a session includes its trashed same-thread rollouts and missing
history ancestors. Fork relationships alone do not force group removal.

Normal search presents a paginated Codex conversation as one session. Related
review exposes its individual physical pages and their original transcripts.
A superseded conversation can still supply required history to another
conversation; its visibility does not bypass dependency checks.

An expanded group opens a confirmation with selected and required members,
counts, and blockers. Use `r` to review the related chain, `y` to confirm a safe
group, or `Esc` to cancel. **Enter → g** opens the chain directly. Its local `/`
filter and transcript viewer preserve your main search. Optional related members
can be selected with Space; required members cannot be omitted.

Discovery includes active, archived, compressed, and trashed Codex rollouts,
including metadata-only files hidden from normal search. Unreadable or ambiguous
metadata blocks removal. A purge from trash cannot silently delete active
dependents: trash them first. Restore refuses existing destinations, including
files, and never overwrites them.

All UI and rules removal/restore paths recheck the group and native session locks
before moving anything. AICS stages verified backups and records a recovery
journal under `operations/` in its data directory. Failed operations roll back;
startup retries interrupted recovery. Changed external files and uncertain
publication are preserved with the backups and reported for manual recovery.
On platforms without stable file identity support, destination cleanup during
rollback also requires manual recovery. These checks cannot stop external tools
that ignore session locks from changing files.

## Recorded reasons

AICS stores `trash.json` beside `trash.jsonl` and the `trash/` directory in its
data directory. On macOS this is `~/Library/Application Support/aics/`; use
`AICS_DATA_ROOT` to override it.

`trash.json` is an object keyed by the actual trash filename (or Antigravity
archive directory name). Each record includes the time, original path, agent,
and cause:

```json
{
  "rollout-example.jsonl": {
    "trashed_at": "2026-09-19T00:02:29+00:00",
    "original_path": "/home/user/.codex/sessions/rollout-example.jsonl",
    "agent": "codex",
    "reason": {
      "source": "key_binding",
      "key": "^T"
    }
  }
}
```

The other causes are `{"source":"action_menu","action":"Move session to Trash"}`
and `{"source":"rule","name":"rule name","reason":"rule explanation"}`.
Grouped operations use `source: "dependency_group"` with `triggers`,
`selected_paths`, and each member's `relation`. The rule explanation is omitted
when the rule provides none. Startup rules,
explicit rule application, and processing proposals in the TUI all record the
rule that caused the move.

This records currently trashed items, rather than keeping an event history.
Successful restoration or permanent deletion through AICS removes the record;
a failed restoration keeps it. Files removed outside AICS can leave stale records.

Existing trash items may have no record, which means their reason is unknown.
AICS does not invent reasons for them. `trash.jsonl` continues to provide the
original paths for recovery, independently of `trash.json`. Missing reason
records never prevent browsing, restoration, or deletion.

Reason updates use a lock and atomic file replacement. AICS preserves an
unreadable or malformed reason file and refuses new trash operations that cannot
record their reason before removing the original. Lifecycle operations preserve malformed metadata and report the problem before
changing session files.

[Back to the README.](../README.md)
