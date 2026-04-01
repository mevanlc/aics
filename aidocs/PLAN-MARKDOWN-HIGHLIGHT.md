# PLAN: Markdown Rendering + Syntax Highlighting

## Goal

Add Markdown-aware message rendering to the preview pane and full viewer, including syntax-highlighted fenced code blocks and preserved search-term highlighting.

This plan assumes:

- `pulldown-cmark` will be used for Markdown parsing.
- `syntect-tui` will be used for syntax-highlighted fenced code blocks.
- Search-term highlighting remains a first-class behavior and is applied during rendering, not as a lossy post-processing pass over already-rendered output.

## Why This Approach

`tui-markdown` would be faster for a quick drop-in render, but it becomes less attractive once we require:

- search highlighting over rendered output
- tight theme control inside the existing Ratatui pipeline
- predictable behavior for mixed Markdown/plain-text content
- room to extend rendering behavior over time

This codebase already has custom Ratatui text rendering in:

- `src/tui/preview.rs`
- `src/tui/viewer.rs`
- `src/tui/util.rs`

Owning the render pipeline directly is the cleaner fit.

## Current State

### Rendering

- Preview and viewer currently render messages as plain text.
- Search highlighting is implemented by splitting plain text spans in `src/tui/util.rs`.
- Message rendering is line-oriented and role-aware, but not Markdown-aware.

### Search Highlighting

- Query terms are extracted via `src/search_query.rs`.
- Text highlighting is applied directly to raw message text before it reaches `Paragraph`.
- This works well for plain text but does not account for Markdown structure or syntax-highlighted code.

### Implication

If we adopt Markdown rendering, we should not convert Markdown to plain terminal text and then try to rediscover structure afterward. The renderer should produce styled `Line`/`Span` values directly.

## Non-Goals

- Full CommonMark/GFM fidelity on the first pass
- Rich table rendering
- Inline HTML support
- Image rendering
- Links as interactive widgets
- Per-language theme customization beyond a single syntect theme selection

The first target is "useful and robust", not "complete Markdown engine".

## Proposed Dependencies

Add:

- `pulldown-cmark`
- `syntect`
- `syntect-tui`

Likely no additional renderer crate is necessary.

## Design Overview

Introduce a dedicated markdown renderer module that converts a message body into `ratatui::text::Text<'static>`.

Suggested module:

- `src/tui/markdown.rs`

Primary responsibilities:

1. Parse Markdown into events with `pulldown-cmark`.
2. Render block structure into Ratatui `Line`/`Span` output.
3. Render fenced code blocks with syntect styling when a language token is recognized.
4. Apply search-term highlighting as a style overlay while generating spans.
5. Fall back safely for unsupported or malformed content.

## Rendering Model

### Message Framing

Keep the existing outer message framing:

- role label
- timestamp
- per-role bubble/background styling
- blank-line separation between messages

Only replace the inner content rendering of each message body.

This preserves the current app structure and avoids rewriting layout/scroll behavior.

### Markdown vs Plain Text

Use a lightweight detection strategy:

- Always parse with `pulldown-cmark`.
- If the content contains no meaningful Markdown structure, the rendered output should still look acceptable as plain text.

Avoid heuristic gates unless parsing overhead becomes measurable. `pulldown-cmark` is cheap enough that unconditional parsing is likely fine.

### Style Composition

Each rendered span may carry several independent style layers:

1. Base text style from the current theme
2. Bubble background derived from message role/agent
3. Markdown semantic style
   - heading
   - emphasis
   - strong
   - inline code
   - block quote
   - list marker
   - code fence body
4. Search highlight overlay, when query terms match

The search overlay must patch over the existing style rather than replace it. That is the key reason to integrate search highlighting into the renderer.

## Supported Markdown Surface: Phase 1

Implement first:

- paragraphs
- soft/hard breaks
- headings
- emphasis / strong / strikethrough
- inline code
- fenced code blocks
- indented code blocks rendered as plain code blocks without syntax highlighting
- unordered lists
- ordered lists
- block quotes
- thematic breaks

Defer initially:

- tables
- footnotes
- task lists
- definition lists
- inline HTML / HTML blocks
- images

For deferred elements, render conservatively as plain text or simplified structure. Never panic and never drop the whole message.

## Code Fence Highlighting

### Language Handling

For fenced blocks with an info string:

- extract the first language token
- normalize common cases where useful
  - `rs` -> `rust`
  - `js` -> `javascript`
  - `ts` -> `typescript`
  - `sh` -> `bash`
  - `zsh` -> `bash` or `zsh` depending on syntect support
- ask syntect for a syntax by token

If the token is unknown:

- render as a code block with code styling
- do not fail
- no syntax coloring

### Theme Choice

Pick one syntect theme and keep it stable for the first implementation.

Good default:

- a dark theme that does not fight the existing TUI colors too hard

The renderer should isolate syntect theme selection behind one function so later theme integration is easy.

### Background Interaction

Be careful with backgrounds:

- the message bubble already has a background color
- syntect styles may include their own background colors

Preferred first-pass behavior:

- preserve syntect foreground colors
- suppress syntect background colors if they produce visual clashes
- keep the message bubble background authoritative

This usually makes the result more coherent inside a TUI message bubble.

## Search Highlighting Strategy

### Requirement

Search highlighting must continue to work in:

- preview pane
- full viewer
- markdown paragraphs
- inline code
- fenced code blocks

### Implementation

Build a helper that applies query highlighting to text segments while preserving the incoming style.

Instead of today's plain-text-only approach, the new renderer should:

1. receive a piece of text plus a base style
2. split it into matching and non-matching spans
3. emit spans where matches are `base_style.patch(theme.highlight_style())`

For code fences:

- apply highlighting after syntect tokenization, not before
- split each already-colored token span into highlighted/non-highlighted pieces as needed

This preserves syntax colors while still surfacing query matches.

### Match Semantics

Reuse existing query term extraction from `src/search_query.rs` so the list, preview, and viewer stay consistent.

## Suggested Module Shape

### `src/tui/markdown.rs`

Suggested public API:

```rust
pub fn render_markdown_message(
    content: &str,
    theme: &Theme,
    base_style: Style,
    highlight_style: Option<Style>,
    query: Option<&str>,
) -> Text<'static>
```

Possible internal pieces:

- `MarkdownRenderer`
- `RenderContext`
- `render_events(...)`
- `render_paragraph_text(...)`
- `render_inline_code(...)`
- `render_code_block(...)`
- `highlight_text_runs(...)`
- `highlight_styled_spans(...)`
- `normalize_code_fence_lang(...)`

## Integration Points

### Preview

Update `src/tui/preview.rs`:

- keep role/timestamp header rendering as-is
- replace per-line `highlight_spans(...)` calls for message content
- append markdown-rendered lines for each message body

### Viewer

Update `src/tui/viewer.rs`:

- reuse the same content renderer as preview
- ensure inline search continues to use the same rendered content path

### Existing Utilities

Refactor `src/tui/util.rs`:

- keep the current plain-text highlighter for list/snippet rendering
- add style-preserving helpers usable by the markdown renderer

Do not force the list/snippet path onto the new markdown renderer. Search snippets come from Tantivy HTML and are a separate concern.

## Scroll and Match Navigation

The viewer currently computes match rows against raw plain text. That logic will become inaccurate once Markdown rendering changes wrapping and structure.

This needs an explicit second step.

### Phase 1

Ship markdown rendering first, but keep viewer match navigation behavior conservative if necessary.

Two acceptable options:

1. Temporarily keep match navigation based on raw message text and accept approximate jumps.
2. Disable precise next/previous match row jumping for markdown-rendered bodies until row mapping is implemented.

### Phase 2

Implement rendered-row-aware match navigation by computing matches from rendered lines/spans instead of raw message text.

That work should operate on the final `Text` output for the message body or on an intermediate render tree that knows line boundaries.

## Error Handling

The renderer must be defensive:

- malformed Markdown should render as best-effort text
- unknown code fence languages should fall back cleanly
- syntect failures should degrade to plain code styling
- no panics from invalid UTF-8 assumptions or parser edge cases

This is consistent with the parser philosophy in `ULTRAPLAN.md`.

## Performance Notes

This should be fast enough for preview/viewer rendering, but there are a few constraints:

- avoid reparsing the selected session more than necessary
- avoid repeated syntax-set/theme-set loading
- cache syntect assets behind `LazyLock`
- consider caching rendered message `Text` for the active query in the viewer if redraw cost becomes noticeable

Do not preemptively overbuild caching before measuring.

## Implementation Phases

### Phase A: Scaffold

1. Add dependencies to `Cargo.toml`.
2. Create `src/tui/markdown.rs`.
3. Add a minimal renderer that supports paragraphs, emphasis, strong, and line breaks.
4. Wire preview and viewer to use it for message bodies.

### Phase B: Code Rendering

1. Add inline code styling.
2. Add fenced code block rendering.
3. Integrate syntect-based syntax highlighting for recognized languages.
4. Add graceful fallback for unknown languages and indented code blocks.

### Phase C: Search Overlay

1. Replace plain-text-only highlight logic in preview/viewer message bodies.
2. Add style-preserving query highlighting for markdown text spans.
3. Add style-preserving query highlighting for syntax-colored code spans.
4. Verify that search highlighting remains visually legible against all bubble backgrounds.

### Phase D: Structure Completion

1. Add block quotes.
2. Add ordered and unordered lists.
3. Add headings and thematic breaks.
4. Improve spacing rules so rendered messages read well inside bubbles.

### Phase E: Viewer Search Navigation

1. Rework match-row computation to use rendered output instead of raw text.
2. Keep `Ctrl+N` / `Ctrl+P` behavior correct for wrapped markdown content.

## Testing Plan

### Unit Tests

Add tests for:

- plain text rendering still behaving sensibly
- emphasis and strong rendering
- fenced code block language detection
- unknown code fence languages falling back without error
- search highlighting preserving non-highlight styles
- search highlighting preserving syntax-highlighted code spans
- Unicode content and grapheme safety

### Snapshot-Oriented Tests

Add rendering tests that assert produced `Line`/`Span` content for:

- paragraphs and wrapping-sensitive content
- mixed prose and code fences
- nested emphasis cases
- list rendering
- quote rendering

### Manual Verification

Check in the actual TUI:

- a Rust code fence
- a shell code fence
- an unknown-language code fence
- a message with backticks but no code fence
- search query active vs inactive
- light and dark theme variants if applicable
- narrow terminal widths
- long wrapped code lines

## Risks

### Search Highlight vs Syntax Highlight Style Conflicts

This is the main integration risk. The renderer must merge styles carefully so query matches remain visible without wiping out syntax colors.

### Viewer Match Navigation Drift

Current row math assumes plain text. That will need follow-up work if exact jump-to-match behavior matters immediately.

### Markdown Over-Interpretation

Some chat content uses markdown-like punctuation casually. Parsing everything as Markdown is still the right default, but spacing and code-block rules should be tested against real session data.

## Recommendation

Proceed with `pulldown-cmark + syntect-tui`.

Implementation order:

1. renderer scaffold
2. code fences with syntax coloring
3. search-highlight overlay
4. remaining block structure
5. viewer match-navigation correction

This yields usable value early without painting the rendering pipeline into a corner.
