# Plan: Codex Session Parse Parity

## Goal

Bring AICS's Codex session parsing and display metadata into parity with how Codex itself chooses content for its session resume picker.

In Codex, the picker preview is not "latest message" and not an assistant summary. It is:

1. The first real user message in the thread.
2. With Codex protocol boilerplate stripped.
3. Hidden entirely for sessions that never captured a real user message.
4. Overridden visually by a user-set thread name when one exists.

This plan focuses on importing that logic into AICS in a way that fits the current parser/index/TUI architecture.

## Current State In AICS

Relevant current code:

- [src/parse/codex.rs](/data/data/com.termux/files/home/p/my/aics/src/parse/codex.rs)
- [src/parse/session.rs](/data/data/com.termux/files/home/p/my/aics/src/parse/session.rs)
- [src/index/reader.rs](/data/data/com.termux/files/home/p/my/aics/src/index/reader.rs)
- [src/tui/preview.rs](/data/data/com.termux/files/home/p/my/aics/src/tui/preview.rs)

Current behavior is close, but not Codex-parity:

- AICS already stores `first_user_msg_content` on `Session`.
- The Codex parser currently derives that from the first parsed `MessageRole::User` message, not from a dedicated "resume-preview" extraction pipeline.
- It does not strip Codex's user-message prefix marker.
- It does not distinguish between:
  - "first user message for preview/search/listing"
  - "first parsed visible user message in the transcript"
- It does not currently populate `custom_title` for Codex sessions.
- It does not currently model the Codex rule that sessions without a real first user message should be omitted from resume-style listing logic.

## Codex Behavior To Match

From local Codex source investigation:

- Filesystem fallback extracts the first `event_msg.user_message.message`, strips `USER_MESSAGE_BEGIN`, trims, and stores that as `first_user_message`.
- SQLite-backed listing persists the same value as `first_user_message`.
- DB listing filters out rows where `first_user_message` is empty.
- App-server `Thread.preview` is just that stored `first_user_message`.
- TUI resume picker displays `thread.name` if present, otherwise `thread.preview`.
- Picker search matches both preview text and thread name.

## Scope

In scope:

- Codex parser parity for "first user preview" extraction.
- Codex session title parsing/loading into `custom_title`.
- Search/list/snippet behavior that prefers parity preview text.
- Tests for old/new/latest Codex rollout variants.
- A small amount of session-model cleanup if needed to express the distinction clearly.

Out of scope:

- Reproducing Codex's entire resume picker UI.
- Importing Codex's SQLite/database layer.
- Exact app-server wire compatibility.
- Claude-side changes except where shared helpers naturally benefit both parsers.

## Desired End State

For Codex sessions in AICS:

- `first_user_msg_content` means "Codex resume-preview equivalent", not merely "first visible user transcript entry".
- That field is extracted from the same semantic source Codex uses when possible:
  - Prefer `event_msg.user_message.message`.
  - Normalize by stripping Codex's user prefix marker and trimming.
- Sessions with no meaningful first user message remain parseable for viewer/debugging, but can be excluded from resume-style behaviors.
- `custom_title` is populated for Codex if a persisted thread name exists.
- Fallback snippets and list labeling can prefer:
  - `custom_title` for label/title,
  - `first_user_msg_content` for preview/search/snippet base.

## Implementation Phases

### Phase 1: Make Preview Extraction Explicit

Update the Codex parser so "resume preview" is a first-class concept instead of an incidental byproduct of `messages`.

Target files:

- [src/parse/codex.rs](/data/data/com.termux/files/home/p/my/aics/src/parse/codex.rs)
- [src/parse/session.rs](/data/data/com.termux/files/home/p/my/aics/src/parse/session.rs)

Steps:

1. Add a small helper for Codex user-message normalization.
   - Example responsibility: strip the `USER_MESSAGE_BEGIN` marker if present and trim whitespace.
   - Keep it Codex-specific, not a generic transcript cleaner.

2. Track a dedicated parser-local field such as:
   - `resume_preview_first_user: Option<String>`

3. Populate that field from `event_msg.user_message`.
   - This should happen even if the same message is later suppressed or transformed for transcript display.
   - Only set it once, using the first meaningful normalized value.

4. Keep transcript message extraction separate.
   - `messages` should remain optimized for preview/viewer display.
   - `first_user_msg_content` should come from the dedicated preview field, not from `first_user_message(&messages)`.

5. Fall back only when necessary.
   - If a rollout variant lacks `event_msg.user_message` but clearly contains a real user message in a `response_item.message`, use that as a fallback.
   - Keep the precedence explicit:
     1. `event_msg.user_message`
     2. user-role `response_item.message`
     3. empty

Acceptance criteria:

- `Session.first_user_msg_content` for Codex is sourced from a dedicated parity pipeline.
- Prefix boilerplate no longer appears in that field.
- Transcript rendering remains unchanged unless explicitly desired.

### Phase 2: Parse Codex Thread Names

Codex's resume screen prefers a user-set thread name over preview text. AICS already has `custom_title`, but Codex parsing does not fill it.

Target files:

- [src/parse/session.rs](/data/data/com.termux/files/home/p/my/aics/src/parse/session.rs)
- [src/parse/codex.rs](/data/data/com.termux/files/home/p/my/aics/src/parse/codex.rs)
- Possibly scanner/index-side code that can cheaply load a sidecar index file if needed.

Steps:

1. Decide the lookup source for Codex thread names.
   - Codex stores names in `sessions_index.jsonl`, keyed by thread id.
   - AICS should read that file lazily or via a small cache when parsing/indexing Codex sessions.

2. Add a focused loader for Codex thread names.
   - Input: Codex home or discovered thread id.
   - Output: latest thread name for that id, if any.
   - Do not fail the parse if the index file is missing or malformed.

3. Populate `Session.custom_title` for Codex sessions when a thread name exists.

4. Keep `custom_title` semantically agent-neutral.
   - For Claude it can still mean custom/session title.
   - For Codex it becomes the thread name.

Acceptance criteria:

- Named Codex threads show a title in AICS metadata.
- Missing index data is tolerated cleanly.

### Phase 3: Define "Resume-Style Eligibility"

Codex does not list sessions with no first user message in its DB-backed picker flow. AICS should decide where to mirror that behavior.

Target files:

- [src/parse/session.rs](/data/data/com.termux/files/home/p/my/aics/src/parse/session.rs)
- [src/index/writer.rs](/data/data/com.termux/files/home/p/my/aics/src/index/writer.rs)
- [src/index/reader.rs](/data/data/com.termux/files/home/p/my/aics/src/index/reader.rs)

Steps:

1. Add a small helper on `Session`, for example:
   - `has_resume_preview() -> bool`
   - true when `first_user_msg_content` is non-empty.

2. Decide policy by feature area:
   - Search indexing:
     - Recommended: still index the session if it has useful content.
   - Resume/fork actions and resume-oriented list labeling:
     - Recommended: treat missing preview as lower-quality or ineligible, matching Codex more closely.

3. Document the difference clearly.
   - "Searchability" and "resume-preview eligibility" should not be conflated.

Acceptance criteria:

- AICS has an explicit concept for whether a session is valid for resume-preview style UX.
- This does not accidentally hide otherwise valuable sessions from the main search index unless intentionally chosen.

### Phase 4: Use Title-Then-Preview In UI Surfaces

Codex's picker displays thread name first, preview second. AICS should mirror that order anywhere it presents a single identifying line for Codex sessions.

Target files:

- [src/index/reader.rs](/data/data/com.termux/files/home/p/my/aics/src/index/reader.rs)
- [src/tui/list.rs](/data/data/com.termux/files/home/p/my/aics/src/tui/list.rs)
- [src/tui/preview.rs](/data/data/com.termux/files/home/p/my/aics/src/tui/preview.rs)

Steps:

1. Introduce a shared display helper, e.g.:
   - `session_display_title(session) -> &str`
   - `session_display_preview(session) -> &str`

2. Use precedence:
   - Title/label: `custom_title`, else project/session fallback.
   - Preview/snippet base: `first_user_msg_content`, then `first_msg_content`, then `last_msg_content`.

3. Review list row rendering.
   - If list rows currently over-emphasize snippet text, consider splitting:
     - title line
     - snippet line
   - Do not cram thread name into the snippet field itself if the layout already has a better title slot.

4. Review preview pane header.
   - If useful, show `custom_title` in the block title or metadata header.
   - Do not replace the full conversation preview body with only the title.

Acceptance criteria:

- Named Codex sessions are visually identifiable by thread name first.
- Snippets still reflect the first user prompt, preserving Codex parity.

### Phase 5: Align Search/Snippet Semantics

Codex picker search matches both thread name and preview text. AICS search is broader, but the stored metadata should still support that behavior well.

Target files:

- [src/index/writer.rs](/data/data/com.termux/files/home/p/my/aics/src/index/writer.rs)
- [src/index/reader.rs](/data/data/com.termux/files/home/p/my/aics/src/index/reader.rs)

Steps:

1. Ensure `custom_title` is indexed as searchable/stored metadata if not already.

2. Ensure `first_user_msg_content` is preserved as a high-signal searchable field.

3. Review snippet fallback behavior in [src/index/reader.rs](/data/data/com.termux/files/home/p/my/aics/src/index/reader.rs#L546).
   - This is already close to Codex parity because it prefers `first_user_msg_content`.
   - After parser changes, verify that this now yields the true Codex-style preview text.

4. If field-level boosting exists or is added later, boost:
   - `custom_title`
   - `first_user_msg_content`

Acceptance criteria:

- Queries matching thread names or first prompts find the expected Codex sessions.
- Snippet fallback uses the cleaned parity preview text.

## Testing Plan

Target files:

- [tests/fixtures/sessions/codex/old_format.jsonl](/data/data/com.termux/files/home/p/my/aics/tests/fixtures/sessions/codex/old_format.jsonl)
- [tests/fixtures/sessions/codex/new_format.jsonl](/data/data/com.termux/files/home/p/my/aics/tests/fixtures/sessions/codex/new_format.jsonl)
- [tests/fixtures/sessions/codex/latest_format.jsonl](/data/data/com.termux/files/home/p/my/aics/tests/fixtures/sessions/codex/latest_format.jsonl)
- Add parser/unit tests near [src/parse/codex.rs](/data/data/com.termux/files/home/p/my/aics/src/parse/codex.rs)

Tests to add:

1. `event_msg.user_message` wins over transcript-derived fallback.

2. Codex user prefix marker is stripped from `first_user_msg_content`.

3. Empty or whitespace-only normalized user messages do not populate preview.

4. Old-format Codex rollouts still produce a sensible fallback preview when possible.

5. Sessions with no real user message:
   - parse successfully if they have other content, but
   - report `has_resume_preview() == false` if that helper is introduced.

6. Thread name lookup:
   - latest matching entry wins
   - missing/malformed index file does not fail parsing

7. Snippet fallback prefers cleaned first-user preview over first/last message content.

8. List/title helpers prefer `custom_title` over prompt text.

## Risks

### Old-format Codex compatibility

Some older rollout formats may not expose `event_msg.user_message` consistently. The plan should preserve existing parse coverage by using a clear fallback path rather than hard-switching to one source.

### Extra I/O for thread-name lookup

Reading `sessions_index.jsonl` per session parse would be wasteful. Prefer:

- one pass cache per indexing run, or
- lazy shared lookup keyed by thread id.

### Over-coupling transcript display to resume-preview logic

Codex's resume preview and transcript rendering are different concerns. Keep them separate in AICS to avoid regressions in the full viewer.

## Recommended Order

1. Phase 1: explicit preview extraction
2. Phase 3: resume-preview eligibility helper
3. Phase 2: thread name lookup
4. Phase 4: title-then-preview UI usage
5. Phase 5: search/snippet alignment
6. Add tests as each phase lands

## Definition Of Done

This work is done when:

- AICS extracts Codex `first_user_msg_content` using Codex-equivalent logic.
- Codex protocol boilerplate is stripped from that field.
- Codex thread names populate `custom_title`.
- Resume-style UI surfaces prefer thread name over prompt text.
- Snippet fallback uses the cleaned first-user preview.
- Regression tests cover old/new/latest Codex fixtures and the key parity rules above.
