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

Keep path-sensitive tests portable: derive absolute filesystem/recovery fixture
paths from `TempDir::path()` (for example, `temp.path().join("original/session.jsonl")`).
On Windows, `/tmp/...` is rooted on the current drive but is not absolute, so
absolute-path validation correctly rejects it. Compare paths with `Path` APIs
rather than slash-specific strings. Serialize JSON paths with serde and construct
file URIs with `url::Url::from_file_path` to handle native separators and escaping.

Binary name: `aics`

## Conventions

- Idiomatic Rust. Prefer clarity over cleverness.
- `anyhow` for error propagation. `thiserror` if custom error types are needed.
- `tracing` for diagnostics, with `tracing-subscriber` for `RUST_LOG` filtering
  and formatting, and `tracing-appender` for non-blocking daily TUI log files.
  Keep the logging handle alive until application teardown to flush the writers.
- Parsers must be defensive: skip unrecognized entries, never crash on malformed input.
- Stream JSONL files line-by-line (`BufReader`), never load whole files into memory.
- Test fixtures live in `tests/fixtures/sessions/{claude,codex}/`.

## Task Tracking

The original plan is archived in devdocs/done/ULTRAPLAN.md. Remaining follow-ups
are tracked in devdocs/todo/TODO-ULTRAPLAN.md.
.md files for/by AI Agents should be read/written to devdocs/
