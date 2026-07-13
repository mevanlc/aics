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

When authoring or running tests that can load or save settings/config, protect
the user's real `~/.config/aics/` data. Prefer explicit temp paths with
`Settings::*_to_path` helpers. App-level/unit tests that may call
`Settings::save_patch`, spawn settings-sensitive workers, or otherwise resolve
the default config dir must call `crate::settings::isolate_config_root_for_tests()`
before constructing the app or touching settings; this sets `AICS_CONFIG_ROOT`
to a process-lifetime temp directory.

Binary name: `aics`

## Conventions

- Idiomatic Rust. Prefer clarity over cleverness.
- `anyhow` for error propagation. `thiserror` if custom error types are needed.
- `log4rs` for logging, with built-in `RUST_LOG=debug aics` filtering and an
  optional `~/.config/aics/log4rs.yaml` override.
- Parsers must be defensive: skip unrecognized entries, never crash on malformed input.
- Stream JSONL files line-by-line (`BufReader`), never load whole files into memory.
- Test fixtures live in `tests/fixtures/sessions/{claude,codex}/`.

## Task Tracking

The original plan is in devdocs/ULTRAPLAN.md
.md files for/by AI Agents should be read/written to devdocs/
