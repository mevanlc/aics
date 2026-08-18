# JavaScript rules

AICS can evaluate JavaScript rules over session metadata and transcript content.
Rules propose an action for each matching session; you can review those proposals
before changing files or apply supported actions non-interactively.

## Running rules

Rules live at `~/.config/aics/rules.js` by default.

- `aics --preview-rules` reviews proposed actions in the TUI without changing
  files.
- `aics --preview-rules --json` prints proposed actions as JSONL.
- `aics --apply-rules` applies supported actions non-interactively.
- `aics --rules PATH` selects another rules file for startup or an explicit
  rules mode.
- `aics --write-rules-dts` writes TypeScript declarations for the rules API to
  `~/.config/aics/rules.d.ts`.

Rules mode honors the usual scope and filter flags, including `-g`, `--dir`,
`--agent`, `--after`, `--before`, `--min-lines`, and `--sub-agent`, but it does
not accept a text search query yet.

## Defining rules

Register a rule with either `rule(name, callback)` or
`rule(name, config, callback)`:

```js
rule("trash short commit helper sessions", ({ turns }) => {
  return turns.user.length === 2 &&
    /\s*[/$](?:gdf-)?commit\b/m.test(turns.user[0].text(4096))
    ? trash("commit helper")
    : nothing();
});
```

A rule can return `nothing()`, `trash(reason)`, or `untrash(reason)`. To evaluate
trashed sessions for `untrash`, use `--trashed yes` or `--trashed both`.
Applying `untrash` to a normal session is skipped as already untrashed.
Antigravity conversations are multi-file bundles, so AICS safely skips both
`trash` and `untrash` proposals for them.

Rules receive session metadata such as `session.model`,
`session.modelProvider`, `session.reasoningEffort`, `session.approvalPolicy`, and
`session.sandboxMode`. `session.supersededBy` contains the keeper session ID when
the session has been superseded by fork succession or equivalent-family
collapse. Optional string properties on `session`, including `supersededBy`, are
empty strings when their values are unavailable.

## Transcript access

Large transcript fields are fetched from Rust only when a rule calls one of the
lazy methods. The limit argument is optional; omitting it uses a practically
unbounded default for stress testing, but normal rules should pass an explicit
byte limit:

- `turns.user[n].text(limit)`, `turns.contextualUser[n].text(limit)`,
  `turns.agent[n].text(limit)`, `turns.system[n].text(limit)`, and
  `turns.toolResults[n].text(limit)`
- `turns.exec[n].stdout(limit)` and `turns.exec[n].stderr(limit)`
- `turns.patches[n].files[m].content(limit)`

`turns.user` excludes Codex contextual user fragments such as automatically
injected AGENTS.md content. Those entries are exposed separately as
`turns.contextualUser`.

## Exporting rule data

Choose **Export as rules.js JSON** from a session's actions menu, or press its
`J` hotkey, to write the callback data for that session to the current
directory. The file uses the same top-level `{ "session": ..., "turns": ... }`
structure and camel-case property names documented by `rules.d.ts`.

JSON cannot contain the lazy accessor functions available at runtime. Their
complete values are materialized as strings under the corresponding property
names: `text`, `stdout`, `stderr`, and patch-file `content`. Export filenames use
the session's custom title when available, otherwise its session ID, and gain a
numeric suffix instead of overwriting an existing file.

## Startup rules

Set `config.applyAtStartup` to `true` to apply a rule automatically during
ordinary startup, including JSON search startup:

```js
rule("trash startup noise", { applyAtStartup: true }, ({ session }) => {
  return session.lines < 3 ? trash("too short") : nothing();
});
```

Startup application is global over normal, non-trashed sessions and is
independent of search scope and filters. Use `--no-apply-rules` to disable it.
`--preview-rules` and `--apply-rules` always evaluate all registered rules,
regardless of `applyAtStartup`.

## Caching

Rule determinations from the default `~/.config/aics/rules.js` are cached per
index profile so unchanged sessions do not need to be parsed or evaluated again.
Explicit all-rules evaluation and automatic startup evaluation use separate
caches.

Each cache tracks the byte length, modification time, and CRC32 of the running
`aics` binary, `rules.js`, and each session source. Antigravity fingerprints
include both transcript files and cache metadata. Matching byte length and
modification time provide a metadata-only fast path. A byte-length difference is
an immediate miss, while CRC32 checks same-length files whose modification time
changed.

Custom `--rules PATH` files bypass the cache completely: they neither read nor
write cache data, and they leave the default rules caches intact.

[Back to the README.](../README.md#javascript-rules)
