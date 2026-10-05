# Gemini / Antigravity filtering candidates

Investigated and implemented 2026-10-04 against the AICS checkout and local
transcripts. The inventory records the behavior before implementation.

The best candidates are generated record types and complete provider envelopes.
The supplied cancellation messages previously bypassed every turn-visibility
toggle. Several other generated structures had the same problem, while user
metadata is already discarded before filtering can see it.

The implemented mapping uses **zero new visibility categories**, following the
user's preference to keep the filter UI small. Extend existing categories by the
purpose of the generated content, rather than adding a toggle for each provider
record type.

## Scope and counting

- 88 Antigravity conversation bundles under
  `/Users/mclark/.gemini/antigravity-cli/brain/` had transcript files.
- Read 176 files at `*/.system_generated/logs/transcript{,_full}.jsonl`, parsing
  JSONL line by line. Excluded chunk mirrors, `history.jsonl`, `recent.jsonl`,
  conversation databases, and non-transcript configuration files.
- Applied AICS's merge rule: use full records when available, then append regular
  records whose `step_index` is absent from full. No regular-only tail records
  were found. This produces 17,388 records, with no malformed JSON lines.
- Record timestamps span 2026-08-13T04:24:23Z through 2026-10-04T03:37:52Z.
- Another 15 brain directories had no transcripts and were outside this scan.
- Counts below are records containing the actual provider structure, not tag
  occurrence totals. Session counts overlap between categories. These describe
  the local scan snapshot, not all possible provider formats or archived bundles.

## Candidate inventory

| Actual structure | Records | Sessions | Behavior before implementation | Adopted facility |
| --- | ---: | ---: | --- | --- |
| `SYSTEM/SYSTEM_MESSAGE`, `<SYSTEM_MESSAGE>` envelope | 243 | 52 | System message, always visible | Route subtypes to Tool Results, Internal Context, or Agent Replies as below |
| `SYSTEM/DIRECTORY_RULES` with injected AGENTS/rules text | 17 | 4 | System message; provider preamble does not match Project Docs classifier | Extend existing Project Docs visibility |
| `SYSTEM/CHECKPOINT`, usually `{{ CHECKPOINT n }}` | 71 | 40 | Summary message, always visible; included in `agent:` search | Existing Agent Replies: generated conversation summaries |
| `SYSTEM/ERROR_MESSAGE` | 7 | 7 | System message, always visible | Existing Tool Results: execution/model/tool-call failure feedback |
| `<USER_REQUEST>` in `USER_EXPLICIT/USER_INPUT` | 372 | 88 | Body extracted as the user message | Keep existing normalization |
| `<ADDITIONAL_METADATA>` beside the request | 372 | 88 | Removed entirely | If retained later, use Skill Text Injection for instruction expansions and Internal Context for generated metadata |
| `<USER_SETTINGS_CHANGE>` beside the request | 91 | 86 | Removed; first settings block can infer session model | Existing Internal Context if these events are retained later |
| Empty `SYSTEM/CONVERSATION_HISTORY` | 54 | 31 | Ignored | No additional filter needed |

All 372 user-input records had one outer USER_REQUEST pair. Of the 372 metadata
blocks, 320 contained only local time and 52 contained additional material.

## SYSTEM_MESSAGE subtypes

Every actual SYSTEM_MESSAGE record has the explanatory preamble from the user's
example, followed by a complete `<SYSTEM_MESSAGE>...</SYSTEM_MESSAGE>` envelope.
238 contain a structured `[Message] timestamp=... sender=... priority=... content=...`
header; five contain hook feedback directly.

| Subtype | Records | Sessions | Evidence / identifying structure | Recommended treatment |
| --- | ---: | ---: | --- | --- |
| Task canceled | 27 | 18 | Task sender ending `/task-N`; payload `Task id "..." was canceled with result:` | Tool Results: cancellation is a task outcome |
| Task finished | 122 | 31 | Same sender; payload `Task id "..." finished with result:` and command output | Existing Tool Results category, preserving System provenance |
| Task polling reminders | 23 | 6 | Same sender; short payloads such as `Check test completion` | Internal Context: orchestration instructions |
| Task stalled / waiting for input | 3 | 3 | Same sender; `The command output has stabilized for 5s...` and captured output | Tool Results: execution state and captured output |
| Server restart notice | 39 | 23 | `sender=system`, `[Notice] All your subagents and background tasks have been stopped due to server restart.` | Internal Context: runtime orchestration notice |
| Agent-to-agent message | 17 | 2 | UUID conversation sender, with substantive message text | Agent Replies: substantive agent-authored messages |
| Subagent execution/quota error | 7 | 1 | `sender=system`, `The subagent ... encountered an error... RESOURCE_EXHAUSTED` | Tool Results: agent execution failure feedback |
| Stop-hook feedback | 5 | 4 | `Stop hook blocked termination: ...` | Internal Context: hook/orchestration feedback |

These rows total 243. The 175 task-sender records occur in 39 sessions; those
senders represent background tasks and do not by themselves identify subagents.
There are no separate failed-task payloads in this corpus: a “finished” result
can still contain a nonzero command exit code.

66 messages have MESSAGE_PRIORITY_LOW: all 27 cancellation notices and all 39
restart notices. The other 172 structured messages have MESSAGE_PRIORITY_HIGH.
A priority filter could remove the supplied examples, but would leave task
polling reminders and completions visible. Priority is useful supplementary
metadata, not a reliable semantic category.

The exact supplied example is present at
[transcript_full.jsonl:54](/Users/mclark/.gemini/antigravity-cli/brain/5c3e9fa8-09bc-4290-964d-977808110d7c/.system_generated/logs/transcript_full.jsonl:54)
and [line 67](/Users/mclark/.gemini/antigravity-cli/brain/5c3e9fa8-09bc-4290-964d-977808110d7c/.system_generated/logs/transcript_full.jsonl:67).
The second record is created at 20:30:13Z but its embedded message timestamp is
20:30:11Z, matching the two timestamps shown in the request.

Other representative evidence:

- Task reminder followed by command completion:
  [dtr conversation, line 81](/Users/mclark/.gemini/antigravity-cli/brain/10b7f1c2-92c4-4445-8eb0-814bc5eb066d/.system_generated/logs/transcript_full.jsonl:81)
  and [line 82](/Users/mclark/.gemini/antigravity-cli/brain/10b7f1c2-92c4-4445-8eb0-814bc5eb066d/.system_generated/logs/transcript_full.jsonl:82).
- Restart notice:
  [line 180](/Users/mclark/.gemini/antigravity-cli/brain/0b3113f5-ec11-4715-8186-abf309ac7e4d/.system_generated/logs/transcript_full.jsonl:180).
- Agent message and subagent quota failure:
  [line 32](/Users/mclark/.gemini/antigravity-cli/brain/6edeb93c-7de1-49f1-becf-6735b5dcc12e/.system_generated/logs/transcript_full.jsonl:32)
  and [line 78](/Users/mclark/.gemini/antigravity-cli/brain/6edeb93c-7de1-49f1-becf-6735b5dcc12e/.system_generated/logs/transcript_full.jsonl:78).
- Hook feedback:
  [line 13](/Users/mclark/.gemini/antigravity-cli/brain/0dcb81e0-c3bb-48ba-9d33-36093d95cc59/.system_generated/logs/transcript_full.jsonl:13).
- Checkpoint and directory rules:
  [line 5](/Users/mclark/.gemini/antigravity-cli/brain/08487612-1941-47d0-a460-768f6221b696/.system_generated/logs/transcript_full.jsonl:5)
  and [line 36](/Users/mclark/.gemini/antigravity-cli/brain/08487612-1941-47d0-a460-768f6221b696/.system_generated/logs/transcript_full.jsonl:36).

## Metadata already omitted, including nested tags

ADDITIONAL_METADATA is more than time boilerplate. Nested generated expansions
include `<PLAN>` in 21 records, `<SKILL>` in 10, and `<LEARN>` in one. These are
inside metadata, outside the actual request. They currently disappear together
with their outer block, so the Skill Text toggle cannot reveal them.

Other metadata includes resolved file mentions, uploaded-image paths, and running
terminal commands. Any future retention should separate these useful context
details from slash-command/skill instructions and timestamps. PLAN, SKILL, and
LEARN definition text can all use Skill Text Injection; settings/time/runtime
metadata can use Internal Context. Actual user request text stays under User
Messages. Attachment and mention details should remain associated with the user
request rather than being lost with hidden instruction expansions. Retaining
metadata is a separate parser change, not necessary for the immediate mapping.

- PLAN expansion: [line 1](/Users/mclark/.gemini/antigravity-cli/brain/08487612-1941-47d0-a460-768f6221b696/.system_generated/logs/transcript_full.jsonl:1).
- SKILL expansion: [line 1](/Users/mclark/.gemini/antigravity-cli/brain/225228dc-e059-435f-b528-39077a312e52/.system_generated/logs/transcript_full.jsonl:1).
- LEARN expansion: [line 43](/Users/mclark/.gemini/antigravity-cli/brain/f068b2d0-ee51-44ef-be12-1f53cbd574ed/.system_generated/logs/transcript_full.jsonl:43).
- Resolved file mention: [line 1](/Users/mclark/.gemini/antigravity-cli/brain/09f4e300-8e2f-40cc-a1ba-320666885605/.system_generated/logs/transcript_full.jsonl:1).
- Uploaded-image paths: [line 179](/Users/mclark/.gemini/antigravity-cli/brain/0b3113f5-ec11-4715-8186-abf309ac7e4d/.system_generated/logs/transcript_full.jsonl:179).
- Running terminal state: [line 294](/Users/mclark/.gemini/antigravity-cli/brain/5e8588d5-7ae1-444f-b54b-b544d7f3880a/.system_generated/logs/transcript_full.jsonl:294).

All 91 settings-change records concern Model Selection. AICS keeps inferred
session model metadata, but does not preserve these as individual settings events.

## Facility boundaries and preserved behavior

The Antigravity parser handles USER_INPUT, PLANNER_RESPONSE, CONVERSATION_HISTORY,
and CHECKPOINT explicitly, then maps other non-model records to System messages
([antigravity.rs:300](/Users/mclark/p/my/aics/src/parse/antigravity.rs:300)).
The implementation preserves these roles and adds an optional generated-message
kind derived from provider source/type and validated envelope metadata
([antigravity.rs:415](/Users/mclark/p/my/aics/src/parse/antigravity.rs:415)).
It passes `searchable=false` for System messages, but this does not exclude them
from the current index: the index builds a SearchProjection over session cells
([writer.rs:690](/Users/mclark/p/my/aics/src/index/writer.rs:690)).

System and Summary messages without a recognized annotation retain their original
visibility. Annotated messages use the adopted existing visibility category
([search_projection.rs:69](/Users/mclark/p/my/aics/src/search_projection.rs:69)).
DIRECTORY_RULES uses Project Docs without widening its legacy text preambles
([session.rs:487](/Users/mclark/p/my/aics/src/parse/session.rs:487),
[session.rs:707](/Users/mclark/p/my/aics/src/parse/session.rs:707)).

Internal Context specifically recognizes complete User-role
`codex_internal_context`/`goal_context` wrappers
([session.rs:505](/Users/mclark/p/my/aics/src/parse/session.rs:505)). The mapping
extends Internal Context to recognized provider-generated orchestration messages
while preserving its strict handling of ordinary user text. The Sub Agents session
filter selects whole subagent sessions, while Antigravity currently produces Original
sessions ([antigravity.rs:403](/Users/mclark/p/my/aics/src/parse/antigravity.rs:403)).

USER_REQUEST extraction and metadata removal happen before display, indexing,
and rules evaluation
([antigravity.rs:629](/Users/mclark/p/my/aics/src/parse/antigravity.rs:629)).
Thus `all:` cannot restore discarded metadata. The `user:`/`agent:` query fields
can already constrain searches away from System messages, but they do not hide
those messages in the viewer. Query exclusions operate on whole sessions.

JavaScript rules can inspect these messages through `turns.system[].text()`;
System and Summary are combined in that group
([rules/mod.rs:1812](/Users/mclark/p/my/aics/src/rules/mod.rs:1812)). Existing
rules return whole-session cleanup actions and cannot hide individual turns.
Text turns expose index, timestamp, and text, but no original Antigravity
source/type/sender/priority
([rules.d.ts:105](/Users/mclark/p/my/aics/src/rules/rules.d.ts:105)).

## Adopted mappings and recognition safeguards

| Existing visibility category | Additional content | Records in this scan |
| --- | --- | ---: |
| Project Docs (`AGENTS.md/CLAUDE.md` in the UI) | DIRECTORY_RULES injections | 17 |
| Tool Results | Task completion/cancellation/stall output, subagent execution errors, ERROR_MESSAGE feedback | 166 |
| Internal Context | Polling reminders, server restart notices, stop-hook feedback | 67 |
| Agent Replies | Agent-to-agent messages and conversation checkpoints | 88 |

These four rows cover all 338 previously unclassified nonempty generated records
in the inventory. Skill Text Injection and User Messages already supply sensible
categories for metadata instruction expansions and actual requests if metadata
retention is added later. No new toggle is needed for this mapping.

1. Extended Project Docs to DIRECTORY_RULES and Tool Results to asynchronous task
   outcomes, including the supplied cancellation examples.
2. Extended Internal Context to complete generated reminder/restart/hook records.
   Broadened its help text to explain provider-generated orchestration context.
3. Extended Agent Replies to substantive worker messages and generated summaries.
   Checkpoints already belong to `agent:` search; this aligns their visibility
   with the existing semantic grouping. Broadened the toggle description to include
   agent messages, reasoning, and summaries.
4. Deferred metadata retention, as selected by the user. USER_REQUEST extraction
   and metadata/settings removal remain unchanged.

The main tradeoff is deliberate coupling: users cannot hide only task
cancellations while leaving all other Tool Results visible, or hide checkpoints
separately from Agent Replies. That is acceptable for the requested small filter
set. Internal Context is hidden by default, so newly classified orchestration
notices are now hidden by default too; Tool Results and Agent Replies are
shown by default. In particular, the supplied examples become hideable with Tool
Results rather than disappearing solely because Internal Context is already off.

Classification uses original source/type plus parsed envelope metadata, retained
as an optional annotation in both messages and cells. SYSTEM_MESSAGE recognition
requires the proper provider source/type, full preamble/envelope, a valid timestamp
and sender, and known payload signatures. Unknown generated types and malformed
envelopes remain visible. User-authored quoted tags receive no annotation.

Generic tag matching is unsafe here. Literal HTML, CLI placeholders, Rust
generics, and AICS source shown by tools dominate many tag counts. For example,
`<line_number>` and `<original_line>` occur in 1,967 tool-result records across
56 sessions; existing Tool Results already covers those records.

A real user request asks whether AICS handles `<user_shell_command>` and quotes
an entire example inside USER_REQUEST
([line 1](/Users/mclark/.gemini/antigravity-cli/brain/cbbc0fe4-b591-456d-99c5-18d58055c07f/.system_generated/logs/transcript_full.jsonl:1)).
It must remain ordinary user content. Codex internal/goal/environment tags found
in this corpus occur in tool/source text, not genuine generated Gemini wrappers.
No `<system-reminder>` content was found.

Preview/viewer, filtered exports, selected-block copying, search projection, and
CLI `--hide` share the same visibility classifier. No settings keys or flags were
added. The standalone Internal Context requirement has a matching projection and
index field, separate from the existing User + Internal Context requirement
([search_projection.rs:49](/Users/mclark/p/my/aics/src/search_projection.rs:49)).
The index format advances from 18 to 19 so the next index sync rebuilds unchanged
sessions with the new visibility semantics
([writer.rs:737](/Users/mclark/p/my/aics/src/index/writer.rs:737)).

## Implementation validation

- `cargo check --all-targets`, `cargo build`, `cargo fmt --check`, and
  `cargo clippy --all-targets -- -D warnings` passed.
- Full `cargo nextest run`: 759 passed, 2 skipped. New coverage includes all eleven
  generated kinds, malformed/unknown envelopes, user-quoted examples, serialization
  and message-only fallbacks, index migration, semantic query fields, filtered
  exports, viewer Find, navigation, and block copying.
- The compiled parser classified all 338 generated messages from the original
  88-bundle corpus snapshot, matching every subtype count above.
- CLI fixture verification checked Visible/Hidden/All searches for all four mapped
  categories and complete/filtered Markdown exports.
- A real TTY check confirmed result exclusion, Hidden/All queries, preserved System
  headings, Find counts after toggling Tool Results/Internal Context/Agent Replies,
  and unknown messages remaining visible. A fresh viewer screenshot was inspected.

CLI and TTY checks used an isolated sanitized fixture, settings, and cache roots;
live transcripts and configuration were not modified.
