# Trash and recovery

Press **^T** to trash the selected session from the session list or viewer.
**Enter, then d** performs the same action through the actions menu and works
without function keys, including on Termux. Ctrl+D does not trash sessions or
process rules. In rules preview, ^T opens confirmation to process marked proposals.

Show trashed sessions with the **Trashed** filter or `--trashed yes`, then use
**Undo Trash** in the actions menu to restore one. Trashing an already trashed
session permanently deletes it. Antigravity actions include the whole conversation
bundle and its database companions.

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
The rule explanation is omitted when the rule provides none. Startup rules,
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
record their reason before removing the original. Recovery remains available;
failures to clean up a reason after recovery are logged as warnings.

[Back to the README.](../README.md)
