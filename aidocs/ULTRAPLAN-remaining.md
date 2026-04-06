# ULTRAPLAN Remaining Status

Last reviewed: 2026-04-06

This is a short status pass against [ULTRAPLAN.md](/Users/mclark/p/my/aics/aidocs/ULTRAPLAN.md).

## Status Legend

- `done`: implemented substantially as planned
- `changed`: implemented, but the current behavior differs from the original plan
- `open`: still missing or only partially implemented

## Summary

- `done`: most parser, indexing, TUI, filter, viewer, JSON output, and action-dispatch foundations
- `changed`: some original search semantics and action names evolved during implementation
- `open`: a small number of plan items remain, mostly around sort wiring, focus behavior, and a few planned actions

## Checklist

### Core Build and Data Model

- `done` Cargo scaffold and dependency setup
- `done` Unified session model
- `done` Claude parser
- `done` Codex parser across old/new/latest fixture formats
- `done` Recursive filesystem scanner

### Indexing and Search

- `done` Tantivy schema and incremental index writer
- `done` Index rebuild support via `--rebuild-index`
- `done` Lenient query parsing
- `changed` Search snippets use the manual fallback path rather than Tantivy `SnippetGenerator` as the primary mechanism
- `changed` Multi-word search currently behaves as AND-by-default, not the OR semantics described in ULTRAPLAN
- `changed` Recency weighting exists for relevance search, but the implementation uses score post-processing rather than the originally described `TopDocs::tweak_score()`
- `open` CLI sort wiring does not currently expose the planned relevance default vs `--by-time` switch correctly

### TUI Foundation

- `done` Alternate screen, raw mode, mouse capture, panic-safe restore
- `done` Theme module and app-wide theme usage
- `done` Three-region layout with preview hiding on narrow terminals
- `done` App state, debounced search worker, status bar, result list, preview pane
- `done` Full conversation viewer with inline search
- `done` Filter modal with scope, agent, branch, date, min-lines, derivation toggles, live-only, and sort
- `changed` Focus enum exists, but the original Tab-cycling focus model is not fully wired; list and preview handlers currently reuse search handling

### CLI Surface

- `done` Full flag surface from the plan is present in `clap`
- `done` JSON output mode
- `open` `--by-time` is parsed but not honored distinctly because requests are always built with time sort

### Actions

- `done` View
- `done` Export
- `done` Copy session ID / path / directory
- `done` Delete with confirmation and index refresh
- `done` Resume handoff
- `changed` Fork exists instead of the originally named clone/continue combination
- `open` No distinct trim action
- `open` No distinct continue action

### Live Sessions

- `changed` Live session detection exists for Claude via `~/.claude/sessions/*.json` markers
- `open` No Codex live detection
- `open` The original process-scanning version of live detection is not implemented

### Quality and Testing

- `done` High-value parser tests
- `done` Search/index/filter/JSON output coverage
- `changed` Rendering-quality work appears partially addressed in code structure, but the explicit ULTRAPLAN validation pass for emoji/CJK/non-BMP behavior is not clearly documented as complete

## Small Open List

These are the main remaining ULTRAPLAN items worth treating as still open:

1. Wire sort mode correctly so relevance is the normal search sort and `--by-time` switches to time sort.
2. Decide whether to keep the current AND-style multi-word semantics or restore the original OR-style semantics from ULTRAPLAN.
3. Decide whether to keep the manual snippet path or add Tantivy `SnippetGenerator` as a primary snippet source.
4. Either finish the planned Tab-based focus model or update ULTRAPLAN to reflect the simpler interaction model now in use.
5. Decide whether `fork` is the final replacement for clone/continue, or whether separate trim/continue actions should still be added.
6. Decide whether live detection should stay Claude-marker-only or be extended to Codex as well.
