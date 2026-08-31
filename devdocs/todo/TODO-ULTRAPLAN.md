# TODO: ULTRAPLAN Follow-ups

Last reviewed: 2026-08-31

The completed majority and original design record are archived in
[done/ULTRAPLAN.md](done/ULTRAPLAN.md).

The previous remaining-status pass was dated 2026-04-06. Its largest open item,
using Tantivy `SnippetGenerator` as the primary matched-query snippet path with
a manual fallback, has since been implemented in `src/index/reader.rs`.

## Remaining work

1. Decide whether the existing fork actions are the final replacement for the
   originally planned clone/trim/continue actions, or add distinct trim and
   continue actions.
2. Decide whether live-session detection should remain based on Claude's
   `~/.claude/sessions/*.json` markers or gain a Codex equivalent.
3. Complete and document an explicit rendering-quality validation pass for
   emoji, CJK, non-BMP characters, ZWJ sequences, narrow terminals, and terminal
   resize behavior.

## Deferred roadmap items

These were explicitly outside the initial implementation scope and remain
optional follow-ups:

- Configurable keybindings.
- User-defined custom themes.
- Session bookmarks or favorites.

The other original roadmap items are complete or superseded: custom Claude and
Codex roots exist, Claude marker-based live detection replaced process scanning,
and Markdown export provides a format beyond plain text.
