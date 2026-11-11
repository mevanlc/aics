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

## Trash items and migration

On macOS the default data directory is `~/Library/Application Support/aics/`.
Set `AICS_DATA_ROOT` to use another location. Each item under `trash/` holds
its native session files, any AICS summary, and a `metadata.json` sidecar.
Antigravity items include the conversation directory and database companions.

The sidecar records the metadata version, provider, trash time, reason, and
where each payload belongs. See the [version 1 schema](schemas/trash-item-v1.schema.json).
Reasons identify the keyboard shortcut, menu action, or JavaScript rule that
moved the session. Grouped operations also record the triggers and how the
session is related to the selection. An unknown reason does not block restore.

AICS migrates flat trash items automatically on TUI startup and before applying
rules or running a trash ZIP command. It retains the old catalogs under
`trash/.legacy-migration/` for manual recovery. Read-only searches can read both
layouts. Items with missing or damaged destination metadata can still be
browsed when their transcript is identifiable and included in a ZIP export.

## Export or archive trash

```bash
aics --export-trash trash-backup.zip
aics --archive-trash trash-archive.zip
```

Export keeps the items in trash. Archive publishes and verifies the ZIP before
removing its items. Archive uses the same dependency checks as permanent deletion.
If any item cannot safely be removed, use export to save a copy. Either command
prompts before overwriting an existing ZIP. Pass `--yes` to allow overwrite in a
script. Both commands can migrate legacy trash on first use.

The ZIP contains a `trash/` directory with payloads, metadata, and the schema.
To browse an archive separately, extract it to an empty directory and use that
directory as `AICS_DATA_ROOT`. Undo Trash restores to the recorded paths when
they match the configured provider homes. On another machine, or when a
recorded destination is missing, use the extracted payloads for manual recovery.
A Codex conversation may still need history that was outside trash at archive time.

Prefer the built-in ZIP commands. Manually zip `trash/` only while AICS is stopped.
Trash and restore operations require native exclusive rename support on the
filesystems involved.

[Back to the README.](../README.md)
