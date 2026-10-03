# Source search and viewer Find validation

This implements the approved source-projection, provenance, shared-layout, and
viewer-only Find plan. The existing Tantivy query syntax, including `<...>` term
regexes, remains the session-search language. Find has its own readable-text
substring/regex expression and does not search or change the session list.

## Coordinate and cache contracts

- `SearchProjection` derives eligible source segments from `SessionCell`, with
  message-only fallback and source-record coordinates for Claude native summaries.
  All is the union of Visible and Hidden source segments. Parser semantic facets
  retain their exclusions; explicit fields bypass visibility selection.
- `QueryPlan` shares Tantivy's tokenizers, aliases, parsed expression, phrase
  positions, and term-regex automata. Positive clauses produce occurrences
  independently of the complete expression's Boolean result. Negative clauses
  produce no highlights. Invalid syntax follows Tantivy's lenient diagnostics.
  Local multi-term slop has the engine limitation documented below.
- `DocumentMap` records source components and byte ranges through Markdown,
  entities/escapes, code normalization, ANSI/tab sanitization, formatted JSON,
  structured tool/exec/patch cells, and composite sections. Unmapped positives are
  reported as hidden, metadata, or source-only; matching text in a different
  component does not supply a display position.
- `LayoutDocument` owns prepared visual rows, grapheme byte/column positions,
  block rows, and occurrence rows. The drawing widget consumes these rows
  directly. It does not run a second wrap/truncation pass. The width functions
  come from Ratatui.
- Viewer and preview retain base documents and source projections independently
  of queries and widths. Find edits scan the readable document and update
  overlays without Markdown, syntax, or layout regeneration. Reflow preserves
  source occurrences; session/summary content and in-flight summary changes
  invalidate the base cache.
- Index format 18 stores compact internal coordinate maps. Match metadata is
  absent from public session JSON and Markdown exports. AICS sidecars and Codex
  external autosummaries remain display-only; Claude native source summaries
  retain their indexed behavior.

## Regression coverage

The tests cover positive and negative clauses, role fields, source phrases,
phrase slop/prefixes, term regexes, path prefixes, ranges/sets, malformed syntax,
Unicode byte boundaries, escaped JSON keys/strings, repeated source components,
identical messages/tool cells, multiple occurrences on one row, reflow, zero-width
regex matches, invalid/empty Find, jump directives, the original incremental
scroll anchor, focus/mouse/key routing, filters, selection, source Markdown copy,
settings persistence, cache identity, live cache invalidation, native summaries,
sidecars, and metadata coincidences.

The layout differential compares prepared drawing with actual Ratatui wrapped
buffers at widths 0/1/2/3/5/10/20/40/80/120, all three alignments, mixed styles,
long words, tabs/whitespace, NBSP/zero-width space, CJK, combining marks, and emoji.
It exposed and fixed final wide-glyph truncation in narrow terminals.

The provider corpus audit exercises 10 tracked Claude/Codex/Antigravity fixtures,
SHOW_ALL and default visibility, content and semantic fields, and widths
20/40/80/120. Optional `AICS_PROVENANCE_SAMPLE_*` paths run the same read-only
audit on selected local sessions. Representative local samples included:

| Provider | Source bytes | Cells | Readable bytes (SHOW_ALL) | Sampled source/displayed occurrences |
| --- | ---: | ---: | ---: | ---: |
| Claude | 13,329 | 6 | 1,258 | 35 / 27 |
| Codex | 205,624 | 24 | 21,656 | 185 / 133 |
| Antigravity | 8,650 | 10 | 5,326 | 67 / 49 |

The audit checks source/display bounds, UTF-8 boundaries, hidden-source isolation,
and visual positions. Local source files were read only; settings and indexes
used explicit temporary roots.

## Engine limitation

The locked Tantivy 0.26.1 sorts phrase postings using segment document frequencies
before carrying a slop budget through positional intersections. For a phrase of three
or more terms with nonzero slop, changing an unrelated document can therefore
change whether a fixed transcript matches. A differential test reproduces this
with `"a b c"~2` against `b a c`: adding a document containing only `b`
changes the engine result without changing the source transcript.

Global retrieval and ranking continue to use Tantivy. Local source occurrences
cannot recover the segment's intersection order from one session projection, so
these expressions carry a diagnostic and an uncertain match-status flag. The
viewer retains local navigation and shows the limitation instead of claiming
that the query definitively fails. Exact phrases and two-term slop remain covered
by direct differential comparisons.

## Interactive checks and timing method

Real TTY checks use the tmux-tui-test private server with a deterministic Codex
fixture and isolated configuration/cache/data roots. Screenshots were rendered
with freeze and visually inspected. Checked side-by-side and stacked inputs,
single active same-row occurrences, no-jump Find plus manual navigation, Unicode
case mode, regex errors, Search Boolean mismatch reporting, viewer filter routing,
mouse block selection across filter cancellation/reflow, and the unchanged
session-list query after closing the viewer.

Profiling uses `AICS_TUI_PROFILE_FILE` with threshold 0. A preserved pre-change
debug binary and the new debug binary run the same fixture at 120x40. Warm edit
runs type `alpha` character by character and clear it, three times (18 frames).
The old viewer's only text-search path is compared with new Search and Find
separately; Find has no previous implementation. These are local debug timings,
not a general latency guarantee. Initial projection cost is measured separately
with a synthetic source-component probe so cache reuse cannot hide slow loading.

The matched warm-edit run recorded these median timings:

| Phase | Previous viewer Search | New viewer Search | New viewer Find |
| --- | ---: | ---: | ---: |
| Base Markdown renders across 18 edits | 18 | 0 | 0 |
| Source query matching | Not instrumented | 0.476 ms | — |
| Readable Find scan | — | — | 0.296 ms |
| Viewer render | 1.667 ms | 1.031 ms | 1.232 ms |
| App draw | 10.598 ms | 2.380 ms | 2.639 ms |
| Terminal draw | 10.981 ms | 2.753 ms | 3.025 ms |

Neither new input path rebuilt the base document or its layout during these edits.
Terminal drawing had an isolated 40 ms outlier in the new Search run; the medians
do not describe a worst-case bound.

The initial-load probe uses 1,000 synthetic messages (186 KB of source). Replacing
repeated semantic-source scans with authoritative component lookups reduced
projection construction from 6,763 ms to 6.926 ms in the debug build. At 5,000
messages (930 KB), construction took 36.265 ms and source matching took 61.002 ms
for 10,000 occurrences. These probe times exclude terminal drawing.

The dense JSON probe uses 500 cells and 103 KB of readable text. Compacting
identity origins reduced 61,000 origin records to 3,000. Grouping display origins
once for a complete match set reduced projection of 2,000 occurrences from
867 ms to 3.32 ms, with results checked against individual projection. Base
rendering measured approximately 121 ms separately in the final integration.

## Final integration checks

- `cargo fmt --check`: passed.
- `cargo check`: passed.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo nextest run --no-fail-fast`: 752 passed, 2 manual tests skipped.
  The manual renderer timing test was also run explicitly and passed.
- `cargo build`: passed.
- `cargo build --release`: passed.
- `git diff --check`: passed.
- The renderer/lexer review passed 67 focused tests, including 10 tracked fixtures
  and three read-only local provider samples across four widths and two display
  modes. Public JSON/export and temporary settings-root regressions passed.
- The release binary passed the final 120x40 and 60x30 TTY checks. Mouse selection,
  filter cancellation, focused occurrence navigation, and retained main-query
  behavior were verified. The preview showed no Find box. Private tmux sessions
  were stopped after checking.

Local validation artifacts are under `/private/tmp/aics-search-validation/`, with
the final full-suite log in `final-nextest.log`, the release log in
`final-release.log`, and screenshots in `final-find-second.png` and
`final-find-narrow.png`. The renderer corpus and timing evidence are in
`/tmp/aics-provenance-final.out` and `/tmp/aics-provenance-timing-final.out`.
