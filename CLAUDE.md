# aics

AI Chat Search

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

The original plan is in aidocs/ULTRAPLAN.md
.md files for/by AI Agents should be read/written to aidocs/

