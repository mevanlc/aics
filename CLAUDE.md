# aics

Read `aidocs/ULTRAPLAN.md` before starting work. It contains the full research, architecture, and task breakdown.

## Project

Cross-platform Rust TUI for searching Claude Code and Codex CLI chat session histories. Single binary, no shelling out.

## Build & Test

```
cargo check          # type-check
cargo build          # debug build
cargo test           # run tests
cargo build --release  # release build
```

Binary name: `aics`

## Conventions

- Idiomatic Rust. Prefer clarity over cleverness.
- `anyhow` for error propagation. `thiserror` if custom error types are needed.
- `env_logger` for logging via `RUST_LOG=debug aics` etc.
- Parsers must be defensive: skip unrecognized entries, never crash on malformed input.
- Stream JSONL files line-by-line (`BufReader`), never load whole files into memory.
- Test fixtures live in `tests/fixtures/sessions/{claude,codex}/`.

## Task Tracking

The plan has two phases:
- **MVP** (M1→M2→M3): scaffold, parsers, index, basic TUI with search + preview
- **Post-MVP**: filters, actions, full viewer, polish

Work through MVP steps in order. Post-MVP steps can be reordered based on what falls out naturally.
