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
- `open`: a small number of plan items remain, mostly around snippet generation strategy and a few planned actions

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
- `done` CLI sort wiring now uses `--sort-by <time|relevance>` with `time` as the default
- `changed` Search snippets use the manual fallback path rather than Tantivy `SnippetGenerator` as the primary mechanism
- `changed` Multi-word search intentionally behaves as AND-by-default; ULTRAPLAN has been updated to match that decision
- `changed` Recency weighting exists for relevance search, but the implementation uses score post-processing rather than the originally described `TopDocs::tweak_score()`
- `open` SnippetGenerator remains unimplemented as the primary snippet path

### TUI Foundation

- `done` Alternate screen, raw mode, mouse capture, panic-safe restore
- `done` Theme module and app-wide theme usage
- `done` Three-region layout with preview hiding on narrow terminals
- `done` App state, debounced search worker, status bar, result list, preview pane
- `done` Full conversation viewer with inline search
- `done` Filter modal with scope, agent, branch, date, min-lines, derivation toggles, live-only, and sort
- `changed` The original Tab-cycling focus model was intentionally dropped in favor of a simpler shared-keybinding interaction model; ULTRAPLAN has been updated to reflect that decision

### CLI Surface

- `done` Full flag surface from the plan is present in `clap`
- `done` JSON output mode
- `done` `--sort-by <time|relevance>` is honored distinctly, with `time` as the default

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

1. Plan and implement Tantivy `SnippetGenerator` as the primary snippet source.
Context: keep the current manual snippet logic as a fallback, but teach the primary path to skip noisy preambles and system-style boilerplate so list snippets surface distinctive per-session text.
2. Decide whether `fork` is the final replacement for clone/continue, or whether separate trim/continue actions should still be added.
3. Decide whether live detection should stay Claude-marker-only or be extended to Codex as well.

## SnippetGenerator Next Step

The next snippet pass should stay modest and keep the current fallback path intact.

1. Add a Tantivy-backed snippet path in the search engine for matched queries only.
2. Keep the current manual fallback for empty queries, snippet generation failures, and documents whose generated snippet is clearly low-signal.
3. Add a snippet post-processing layer that can reject or trim boilerplate before the final snippet is shown.
4. Add tests that prove snippets prefer distinctive matched text over generic preambles.

## SnippetGenerator Future Heuristics

Once the primary path exists, these are reasonable follow-ups:

- Drop or de-prioritize snippets that start with repeated assistant boilerplate like "I’ll", "I'll", "Let me", or generic task restatements.
- Drop or trim snippets dominated by system/setup text, shell wrappers, or repeated tool framing.
- Prefer snippets that contain exact matched terms near uncommon surrounding text.
- Fall back immediately when the generated snippet becomes too short or too generic after filtering.
