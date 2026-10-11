# TODO: Codex Session Presentation Enhancements

Last reviewed: 2026-10-10

The completed majority and original design record are archived in
[done/PLAN-CODEX-SESSION-PRESENTATION-ENHANCEMENTS.md](done/PLAN-CODEX-SESSION-PRESENTATION-ENHANCEMENTS.md).

The current implementation has the typed `SessionCell` foundation, session-info
rendering, reasoning, structured tool/exec/patch cells, plan and web-search
cells, and a runtime-metrics footer. Persisted `request_user_input` and
`request_user_input_async` prompts now render as assistant turns with choices;
blocking answers and wrapped async replies render as user turns. These follow
conversation visibility/search filters, and known secret answers are masked.
The remaining work is narrower than the original plan.

## Remaining work

1. Re-scope list metadata enrichment around fields that rollouts actually
   persist, such as model, provider, CLI version, and source. Git SHA and origin
   were removed from the design after real rollout inspection showed that they
   are not present.

## Deferred or optional work

- Sensitive-key redaction in tool input and output remains deferred for the
  local-only viewer.
- Cursor pagination and additional picker sorting remain optional until result
  volume demonstrates a need.

Approval cells are not a remaining task: approval requests are in-memory Codex
events and are not persisted in rollout files.
