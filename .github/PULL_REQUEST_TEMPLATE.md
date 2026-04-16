<!--
Thanks for contributing to aics! Please fill out the sections below.
Keep the title short and imperative (e.g. "Fix panic when Codex session is empty").
-->

## Summary

<!-- What does this PR do and why? 1-3 sentences. -->

## Related issues

<!-- e.g. Closes #123, Refs #456. Delete if N/A. -->

## Changes

<!-- Bulleted list of notable changes. Omit noise. -->
-
-

## Area

<!-- Tick all that apply. -->
- [ ] Search / indexing (tantivy)
- [ ] TUI / rendering (ratatui)
- [ ] Parsers (Claude Code / Codex JSONL)
- [ ] Clipboard / export
- [ ] Configuration / CLI
- [ ] Packaging / release
- [ ] Documentation / tests only

## Testing

<!-- How did you verify this works? Include commands run and platforms tested. -->
- [ ] `cargo check`
- [ ] `cargo build`
- [ ] `cargo test`
- [ ] Manually exercised the TUI on: <!-- macOS / Linux / Windows / Termux -->

<!-- Paste relevant output, screenshots, or a GIF of TUI changes. -->

## Compatibility

<!-- Anything reviewers should know about: -->
- Breaking changes to CLI flags, config, or on-disk state? <!-- yes / no -->
- New dependencies or feature flags? <!-- list them -->
- MSRV change? <!-- yes / no -->

## Checklist

- [ ] Code follows idiomatic Rust conventions used in this repo.
- [ ] Parsers remain defensive (skip unrecognized entries, never panic on malformed input).
- [ ] JSONL readers still stream line-by-line (no whole-file loads).
- [ ] New behavior has tests; fixtures placed under `tests/fixtures/sessions/{claude,codex}/` when applicable.
- [ ] `cargo test` passes locally.
- [ ] User-visible changes reflected in `README.md` and/or help text.
- [ ] Commit messages are clear and do not include tooling/agent attribution.
