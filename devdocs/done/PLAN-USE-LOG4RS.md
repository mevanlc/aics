# Plan: Migrate All Logging to log4rs

## Goal

Replace `env_logger` and the handwritten summary-error file writer with one
`log4rs` backend while preserving AICS's existing `log` macros, default
`RUST_LOG` behavior, TUI-safe file output, CLI-safe stderr output, and dedicated
summary-error artifact.

Default file logging will use one timestamp-plus-PID log group per AICS process
that writes log files, so each process can rotate its own files without
coordinating with another instance. New AICS processes will reap the oldest
stopped-process groups when more than 10 process-instance groups are retained.
Users may optionally copy a checked-in reference `log4rs.yaml` to
`~/.config/aics/log4rs.yaml` and take full control of the log4rs configuration.

The migration should centralize logging policy without turning user-facing
stdout/stderr or progress rendering into log records.

## Current State

### `log` facade events

AICS already emits diagnostics through the `log` facade. The current tree has
52 calls across nine source files:

| Level | Calls | Main uses |
| --- | ---: | --- |
| `trace!` | 4 | Detailed TUI search/render state |
| `debug!` | 18 | TUI lifecycle, search state, and summary execution |
| `info!` | 2 | Indexing completion summaries |
| `warn!` | 28 | Defensive parse, scan, index, trash, rules, and summary failures |
| `error!` | 0 | None today |

The call sites should remain on `log`; `log4rs` becomes the installed backend.
There is no reason to rewrite these sites to a second facade such as `tracing`
as part of this migration.

### Main logger initialization

[src/main.rs](../../src/main.rs) currently:

- builds `env_logger` after `Cli::parse()`;
- defaults to `warn` and reads `RUST_LOG` through `env_logger::Env`;
- sends logs to `{config_dir}/aics.log` whenever `--json` is absent;
- appends without rotation or retention;
- silently falls back to stderr if the config directory or log file cannot be
  opened.

The `!cli.json` test is only an approximation of "will launch a TUI." Commands
such as `--write-rules-dts`, `--delete-index`, `--apply-rules`, and
`--benchmark-rules` are non-TUI even without `--json`, but their logs currently
go to `aics.log`.

### Summary error log

[src/summary/worker.rs](../../src/summary/worker.rs) has a second logging path:

1. A failed summary job emits `warn!` through the normal logger.
2. `append_error_log()` independently opens
   `{config_dir}/summarizer_errors.log`.
3. It writes a custom epoch-seconds/session/error block with `std::fs` and
   `std::io::Write`.

This duplicates routing, formatting, directory creation, and error handling.
It is also outside the global log-level and backend configuration.

### Output that is not logging

The following are process/UI contracts and must stay outside log4rs:

- JSONL search hits and rule records on stdout;
- human-readable rules reports on stdout/stderr;
- palette and generated-path output;
- the settings-recovery warning shown to the user and TUI statusline;
- `indicatif` progress on the selected stdout/stderr draw target;
- errors propagated from `main() -> anyhow::Result<()>`;
- test-only diagnostic `eprintln!` calls.

Moving these to `log` would make output dependent on `RUST_LOG`, risk corrupting
JSONL, or hide required user feedback.

## Target Design

### 1. Keep the `log` facade

Retain the `log = "0.4"` dependency and all existing `trace!`, `debug!`,
`info!`, and `warn!` call sites. Replace only the backend and the one handwritten
summary log writer.

This keeps library modules independent of a concrete logging implementation
and makes the migration narrow.

### 2. Add one logging and retention module

Add [src/logging.rs](../../src/logging.rs) and export it from
[src/lib.rs](../../src/lib.rs). It should own:

- the `log4rs::config::Config` construction;
- optional `log4rs.yaml` discovery and loading;
- `RUST_LOG` parsing and filtering;
- console/rolling-file appender construction;
- the common encoder pattern;
- the dedicated summary-error target and appender;
- process-instance naming and reserved template variables;
- stopped-process log reaping;
- bootstrap fallback behavior;
- mode-independent constants for file names and log targets.

Expose a small API such as:

```rust
pub enum LoggingMode {
    Interactive,
    Command,
}

pub struct LoggingHandle {
    pub handle: log4rs::Handle,
    pub paths: Option<ProcessLogPaths>,
}

pub fn init(mode: LoggingMode) -> anyhow::Result<LoggingHandle>;
```

`main()` should keep the returned handle alive for the process lifetime, even
though the first implementation does not dynamically reconfigure the logger.
`paths` identifies only built-in managed files; it is `None` for command-mode
stderr logging and for an authoritative custom configuration whose actual paths
must not be inferred.

The built-in configuration remains programmatic, so installed binaries require
no companion files. File-based configuration is an optional override loaded
only when `{config_dir}/log4rs.yaml` exists.

### 3. Preserve full `RUST_LOG` filtering in the built-in configuration

`log4rs` supports per-logger levels, but it does not itself provide
`env_logger`'s `RUST_LOG` directive parser. Add a direct `env_filter` dependency
and wrap its filter in a small type implementing `log4rs::filter::Filter`.

Required behavior:

- absent `RUST_LOG` means `warn`;
- `RUST_LOG=debug` enables all debug-or-higher records;
- module directives such as `RUST_LOG=aics=debug,tantivy=warn` continue to work;
- any message-text filter syntax supported by the selected `env_filter`
  version is applied to the full record, not only its metadata;
- the log facade's maximum level is the maximum required by every configured
  route. In command mode that is the parsed main filter's maximum. Interactive
  mode must remain at least `Warn` so `RUST_LOG=off` cannot suppress the
  dedicated summary-error route; disabled trace/debug records should still be
  skipped;
- invalid directives follow a documented, tested policy rather than panicking.

Attach this adapter to the main appender. Do not keep `env_logger` merely for
filter parsing. When an optional `log4rs.yaml` is present, that file is
authoritative: its `root.level`, logger levels, and appender filters replace
`RUST_LOG` for that invocation. This precedence must be explicit in README and
in the reference file comments.

### 4. Route by actual execution mode

Add a pure `Cli` helper that identifies whether the invocation will enter a TUI.
Use it to select logging mode:

| Invocation | Main log destination |
| --- | --- |
| Normal search TUI | Per-instance rolling file under `{config_dir}/logs/` |
| Non-JSON `--preview-rules` TUI | Per-instance rolling file under `{config_dir}/logs/` |
| `--json` search/rules output | stderr |
| `--apply-rules` | stderr |
| `--benchmark-rules` | stderr |
| `--write-rules-dts` | stderr |
| `--delete-index` | stderr |
| `--print-palettes` | stderr |

File routing prevents log output from bleeding through the alternate screen.
Console routing must use stderr and a non-ANSI encoder so stdout remains a clean
data channel. An optional user configuration is allowed to override these
destinations intentionally, but the reference template must never send logs to
stdout because that could corrupt JSONL output.

### 5. Use a stable, diagnostic format

Use the same plain-text pattern for the main console and file appenders:

```text
{d} {l:<5} [{T}] {t} - {m}{n}
```

This records an ISO-8601 timestamp, level, thread name, target/module, and
message. Thread names are useful because indexing/search and summary work run
outside the TUI thread. Do not use log4rs highlighting for either destination;
ANSI sequences do not belong in files or captured stderr.

Keep the default level at `warn`. In particular, do not make the existing
`debug!("summary exec: ...")` record visible by default because expanded
commands and local paths can be sensitive.

### 6. Route summary failures through log4rs

Define a dedicated target, for example:

```rust
pub const SUMMARY_ERROR_TARGET: &str = "aics::summary::errors";
```

Replace the ordinary warning plus `append_error_log()` pair with one targeted
record containing the session path and full error chain:

```rust
warn!(
    target: SUMMARY_ERROR_TARGET,
    "session={} error={error:#}",
    path.display(),
);
```

In the built-in interactive configuration, configure `SUMMARY_ERROR_TARGET`
with:

- a per-instance rolling-file appender under `{config_dir}/logs/`;
- a minimum logger level of `warn`, independent of `RUST_LOG`, preserving the
  current behavior that summary failures are always recorded durably;
- additivity enabled so the same event also reaches the normal main appender
  when the main `RUST_LOG` filter accepts it.

Then delete `append_error_log()` and its direct `OpenOptions`, `Write`, and
epoch-time code. The built-in paths for one interactive process instance are:

```text
logs/aics-<UTC-startup-timestamp>-p<PID>.log
logs/aics-<UTC-startup-timestamp>-p<PID>.log.<archive-index>
logs/summarizer-errors-<UTC-startup-timestamp>-p<PID>.log
logs/summarizer-errors-<UTC-startup-timestamp>-p<PID>.log.<archive-index>
```

An empty per-instance summary-error log may be created during interactive logger
initialization; this is an acceptable and documented difference from today's
lazy first-error creation. Command-only invocations do not start a summary
worker, so their built-in configurations do not need this appender.

### 7. Give every process a distinct log identity

Generate a filesystem-safe process-instance identifier once, before logger
initialization:

```text
<YYYYMMDD>T<HHMMSS>.<milliseconds>Z-p<PID>
```

For example:

```text
20260713T142530.417Z-p12345
```

The UTC timestamp disambiguates PID reuse. PID existence is sufficient for
startup reaping: if an old PID has been reused, its historical log group may be
retained temporarily, but the timestamp prevents the new process from opening,
rotating, or deleting that old group as its own.

Retention counts full process-instance identifiers, not distinct numeric PID
values. Two timestamped groups containing the same reused PID therefore count as
two groups.

The built-in configuration should use size-triggered fixed-window rotation.
Start with conservative constants shared by the programmatic config and
reference template:

| Stream | Active-file limit | Archives per process |
| --- | ---: | ---: |
| Main AICS log | 2 MiB | 2 |
| Summary errors | 1 MiB | 1 |

Do not enable gzip or zstd. Each rolling appender has exactly one owning
process, so its length accounting and rename operations do not race with another
AICS instance.

### 8. Reap stopped-process groups at startup

Set `MAX_RETAINED_PROCESS_LOG_GROUPS` to 10. After the current process has opened
its appenders, scan only `{config_dir}/logs/` and group recognized active/archive
files by their full timestamp-plus-PID instance identifier.

Use this policy:

```text
target_count = max(MAX_RETAINED_PROCESS_LOG_GROUPS, live_group_count)

delete oldest dead groups until:
    total_group_count <= target_count
    or no dead groups remain
```

Consequences:

- With 20 groups, 5 live and 15 dead, delete the 10 oldest dead groups and keep
  10 total.
- With 20 groups, 12 live and 8 dead, delete all 8 dead groups and keep the 12
  live groups.
- If all 20 groups are live, delete nothing.

Reaper requirements:

- Determine liveness from the PID encoded in the group name.
- Treat a positive liveness result as live regardless of timestamp.
- Treat permission errors, unsupported-platform results, and other
  indeterminate checks as live; cleanup must prefer retention over deleting a
  potentially active log.
- Sort dead groups by the startup timestamp encoded in the filename, not file
  modification time.
- Delete the entire recognized group: main active/archives and summary-error
  active/archives.
- Ignore unrecognized files and subdirectories. Never infer deletion targets
  from a user configuration file.
- Treat `NotFound` as success because simultaneous AICS startups may attempt to
  reap the same dead group.
- Treat all other scan/liveness/delete errors as warnings and continue startup.
- Do not add an inter-process reaper lock. Unique process paths plus idempotent
  deletion make concurrent reapers safe, while a lock would add stale-lock and
  cross-platform failure modes.

Factor process inspection behind an injectable interface so tests can supply
live, dead, and indeterminate PIDs deterministically. The production
implementation must work on Linux, Android, macOS, and Windows and must not rely
solely on `/proc`. Implement it with `kill(pid, 0)` on Unix and
`OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` plus `GetExitCodeProcess` on
Windows. Map only a definite "no such process" result to dead; permission and
unexpected API failures are live/indeterminate and preserve the group. Do not
verify that the live PID belongs to AICS, because temporary retention under PID
reuse is the intended conservative behavior.

### 9. Support an optional user log4rs configuration

Add a checked-in [examples/log4rs.yaml](../../examples/log4rs.yaml) reference file.
Users can enable it with:

```bash
mkdir -p ~/.config/aics
cp examples/log4rs.yaml ~/.config/aics/log4rs.yaml
```

The actual discovery path is `{config_dir}/log4rs.yaml`, so
`AICS_CONFIG_ROOT` continues to relocate all AICS configuration during tests and
custom installations.

Before loading either configuration, set these AICS-owned environment variables
once, before any worker threads start:

```text
AICS_LOG_DIR=<absolute config-dir>/logs
AICS_LOG_INSTANCE=<UTC-startup-timestamp>-p<PID>
```

The reference file should use them without placing the archive `{}` placeholder
inside an environment-variable value:

```yaml
appenders:
  aics:
    kind: rolling_file
    path: "$ENV{AICS_LOG_DIR}/aics-$ENV{AICS_LOG_INSTANCE}.log"
    encoder:
      pattern: "{d} {l:<5} [{T}] {t} - {m}{n}"
    policy:
      kind: compound
      trigger:
        kind: size
        limit: 2 mib
      roller:
        kind: fixed_window
        base: 1
        # Keep aics-....log.1 and aics-....log.2.
        count: 2
        pattern: "$ENV{AICS_LOG_DIR}/aics-$ENV{AICS_LOG_INSTANCE}.log.{}"

  summary_errors:
    kind: rolling_file
    path: "$ENV{AICS_LOG_DIR}/summarizer-errors-$ENV{AICS_LOG_INSTANCE}.log"
    encoder:
      pattern: "{d} {l:<5} [{T}] {t} - {m}{n}"
    policy:
      kind: compound
      trigger:
        kind: size
        limit: 1 mib
      roller:
        kind: fixed_window
        base: 1
        # Keep summarizer-errors-....log.1.
        count: 1
        pattern: "$ENV{AICS_LOG_DIR}/summarizer-errors-$ENV{AICS_LOG_INSTANCE}.log.{}"

root:
  level: warn
  appenders:
    - aics

loggers:
  aics::summary::errors:
    level: warn
    appenders:
      - summary_errors
    additive: true
```

The final template must be parsed in a test rather than treated as illustrative
pseudocode. A rotation test must also assert that log4rs interprets `count` as
the maximum number of archives, so the comments and "archives per process"
table cannot drift from actual behavior.

Configuration precedence and failure policy:

1. If `{config_dir}/log4rs.yaml` does not exist, use the programmatic built-in
   configuration and `RUST_LOG` filtering.
2. If it exists and parses, use it as the complete log4rs configuration. Its
   levels and filters supersede `RUST_LOG`.
3. If it exists but cannot be read or parsed, print one precise bootstrap
   warning, fall back to the built-in configuration, and surface the warning in
   the TUI statusline when applicable.
4. A custom config may route outside the managed log directory, use shared
   paths, disable rotation, or omit the summary appender. AICS reaps only files
   matching its managed naming grammar under `{config_dir}/logs/`; users own
   retention and concurrency consequences for all other paths.
5. Read custom configuration once during startup. Changes take effect on the
   next AICS launch; runtime config-file refresh is deferred. A
   `refresh_rate` field may be accepted by the parser but is intentionally not
   activated because AICS does not use `init_file`; omit it from the reference
   template and document that it has no effect.

Load and deserialize the optional file into a `log4rs::config::Config` with
`log4rs::config::load_config_file` before installing the global logger. Install
that config with the same mode-aware runtime error handler as the built-in
config. Do not call `log4rs::init_file`: keeping parsing, fallback, and the single
global installation step separate ensures that a bad optional file cannot leave
AICS unable to install its built-in fallback.

### 10. Degrade safely when a sink is unavailable

Initialization must not panic merely because a log file cannot be opened.
Build the configuration in stages:

1. Resolve and create the config directory.
2. Generate the process-instance identifier and set template variables.
3. Load the optional config or build the appropriate built-in configuration.
4. Install the complete valid config once.
5. Reap stopped-process groups after the current appenders are open.

Fallback rules:

- If the interactive main file cannot be built, install the stderr main
  appender and print one bootstrap warning before entering the TUI.
- If only the summary appender fails, keep the main appender and report that the
  dedicated summary sink is unavailable; summary failures still reach the main
  route when enabled.
- If global logger installation fails, return an `anyhow` error from `init()`;
  do not call `unwrap()` or attempt a second global logger installation.
- Use `init_config_with_err_handler` so runtime appender failures have an
  explicit policy. In command mode they may be reported on stderr. In
  interactive mode they must not repeatedly write through the alternate screen;
  keep the handler silent or retain one in-memory error for later TUI/statusline
  reporting.

Bootstrap warnings are not normal log events because no reliable logger exists
yet. This is the narrow exception to "all logging through log4rs."

## Dependency Changes

Update [Cargo.toml](../../Cargo.toml) and [Cargo.lock](../../Cargo.lock):

```toml
env_filter = "2"
log = "0.4"
log4rs = { version = "1.4", default-features = false, features = [
    "compound_policy",
    "config_parsing",
    "console_appender",
    "fixed_window_roller",
    "pattern_encoder",
    "rolling_file_appender",
    "size_trigger",
    "yaml_format",
] }

[target.'cfg(unix)'.dependencies]
libc = "0.2"

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.61", features = [
    "Win32_Foundation",
    "Win32_System_Threading",
] }
```

Remove `env_logger`.

Use the smallest feature set that supports the built-in rolling appenders and
optional YAML file. Do not enable TOML/JSON configuration, JSON encoding,
compression, time/on-startup triggers, delete rollers, or background compression.
Keep the PID-liveness dependencies target-specific; they replace any temptation
to shell out to platform process-list commands.

log4rs 1.4.0 declares Rust 1.75 as its minimum supported version. AICS currently
builds releases with stable Rust, but the dependency change still needs to be
checked for every release target, especially `aarch64-linux-android` and both
Windows targets.

## Multi-Instance Ownership Rationale

log4rs's file appenders synchronize threads inside one process, not independent
processes. Its rolling appender also maintains a process-local size estimate and
renames an archive window without inter-process coordination. Sharing one
rolling path would therefore create stale handles, conflicting rotations, and
different failure behavior across Unix and Windows.

Timestamp-plus-PID paths follow the log4rs maintainer's recommendation to give
separate processes separate output locations. Each process owns both of its
active paths and archive windows. Startup reaping bounds the number of stopped
process groups without touching any live process's files.

## Implementation Phases

### Phase 1: Capture compatibility behavior

Before changing dependencies, add or identify tests for:

- default `warn` filtering;
- global and module-specific `RUST_LOG` directives;
- clean JSONL stdout with diagnostics on stderr;
- interactive versus command-mode classification;
- `AICS_CONFIG_ROOT` path selection;
- a failed summary job writing its dedicated error record;
- timestamp-plus-PID instance parsing;
- deterministic retention decisions over fake live/dead PID sets.

Keep subprocess config/cache/data roots under `tempfile` directories. Any unit
test that calls the real config resolver must use
`settings::isolate_config_root_for_tests()` first.

### Phase 2: Add the logging foundation

1. Add `log4rs` and `env_filter`; remove `env_logger`.
2. Implement `src/logging.rs` with pure helpers for:
   - filter parsing;
   - process-instance generation and filename parsing;
   - encoder construction;
   - console/rolling-file appender construction;
   - full `Config` construction.
3. Keep explicit paths as parameters below the top-level `init()` function so
   unit tests never resolve or write the real config directory.
4. Set `AICS_LOG_DIR` and `AICS_LOG_INSTANCE` before loading log4rs or spawning
   threads.
5. Install the logger once from `main()` and retain its handle.
6. Remove the old `env_logger::Env` import and `init_logging()` builder code.

### Phase 3: Add built-in and optional-file configurations

1. Build the default console or per-instance rolling configuration
   programmatically.
2. Add optional `{config_dir}/log4rs.yaml` discovery with the documented
   precedence and fallback behavior.
3. Add [examples/log4rs.yaml](../../examples/log4rs.yaml) with comments explaining
   template variables, `RUST_LOG` precedence, rotation, managed filenames, and
   stdout/JSON safety. Also state that the copied template routes every mode to
   per-instance files; unlike the built-in configuration, it does not switch
   command-mode diagnostics to stderr.
4. Parse the checked-in template in a test using temp environment values.
5. Document that configuration is read once and changes apply on the next AICS
   launch.

### Phase 4: Migrate summary-error logging

1. Add the dedicated summary-error appender/logger to the log4rs config.
2. Change the summary worker failure path to one targeted `warn!`.
3. Remove `append_error_log()` and its unused imports.
4. Verify multiline error chains remain readable and each entry ends with
   exactly one newline.
5. Verify the dedicated record is written even when `RUST_LOG=off`.

### Phase 5: Add stopped-process reaping

1. Scan and parse only managed files immediately under `{config_dir}/logs/`.
2. Group main and summary active/archive files by process-instance identifier.
3. Implement injectable PID liveness checks for every release platform.
4. Apply `target_count = max(10, live_group_count)` and remove oldest dead
   groups first.
5. Keep reaping best-effort and idempotent under concurrent startups.
6. Run reaping only after the current logger has opened its files.

### Phase 6: Audit event quality without broad rewrites

Review the existing 52 records in place:

- retain levels unless a call is clearly misclassified;
- keep warning paths defensive and non-fatal;
- do not add raw transcript bodies or JSONL lines;
- keep expanded summary commands at `debug`;
- make messages useful with the new timestamp/thread/target prefix;
- avoid duplicate summary failure records after additivity is enabled.

Any broad logging expansion should be a separate change after the backend
migration is stable.

### Phase 7: Update documentation

Update:

- [README.md](../../README.md) with process-instance log locations, rotation and
  reaping policy, `AICS_CONFIG_ROOT`, default level, `RUST_LOG` and optional-file
  precedence, the template copy command, and the fact that logs can contain
  local paths/commands;
- [CLAUDE.md](../../CLAUDE.md) (also reached through the `AGENTS.md` symlink) to
  name log4rs instead of env_logger;
- [.github/ISSUE_TEMPLATE/bug_report.yml](../../.github/ISSUE_TEMPLATE/bug_report.yml)
  so TUI bug reports explain how to find the current process-instance log, while
  built-in command/JSON invocations capture stderr;
- [ULTRAPLAN.md](ULTRAPLAN.md) only if historical dependency lists are
  intended to describe the current implementation. Otherwise leave the
  original scaffold record historical and add a short superseding note.

## Testing Plan

### Unit tests

Add focused tests around the new logging module:

1. No `RUST_LOG` produces a maximum level of `Warn`.
2. `debug` and module-specific directives produce the expected matches.
3. Invalid `RUST_LOG` input follows the chosen fallback policy without panic.
4. Interactive and command-mode classification covers every early-exit flag.
5. Instance identifiers are UTC-sortable, filesystem-safe, and include the
   current PID.
6. Active/archive filenames round-trip into the correct main/summary group.
7. File paths are derived from an explicit temp config root.
8. The summary target is configured at `Warn` and remains independent from the
   main environment filter.
9. The checked-in YAML template parses with temp `AICS_LOG_DIR` and
   `AICS_LOG_INSTANCE` values.
10. Missing custom config chooses the built-in config; valid custom config wins;
    invalid custom config reports a warning and falls back.
11. Reaping with fake liveness results covers:
    - fewer than 10 groups;
    - oldest-dead-first deletion above 10;
    - more than 10 live groups;
    - reused/live PIDs protecting historical timestamped groups;
    - indeterminate PIDs being retained;
    - unrecognized files being ignored;
    - concurrent-delete `NotFound` being accepted.
12. Config construction failure returns context-rich errors.

Avoid repeatedly installing the global logger in parallel unit tests. Test
filter/config builders directly; reserve full initialization for isolated child
processes.

### Integration tests

Add [tests/logging.rs](../../tests/logging.rs) using `CARGO_BIN_EXE_aics` and temp
roots:

1. A JSON invocation leaves stdout as valid JSONL and writes an induced warning
   only to stderr.
2. `RUST_LOG=off` suppresses the same ordinary warning.
3. `RUST_LOG=aics=debug` accepts AICS debug records without enabling unrelated
   dependencies.
4. `AICS_CONFIG_ROOT` controls the optional config path and managed log
   directory.
5. Two simultaneous processes produce different timestamp-plus-PID paths and
   never share an active or archive file.
6. A summary failure reaches the current process's summary-error log regardless
   of built-in `RUST_LOG` and reaches the main route only when its filter accepts
   the event.
7. Per-process size rotation preserves the configured archive count.
8. Startup removes only enough oldest dead groups to reach the target count and
   never removes a live process's group.
9. A copied template loads successfully, uses the reserved path variables, and
   overrides `RUST_LOG` with its configured levels.
10. A malformed `log4rs.yaml` warns and falls back to built-in logging.
11. An unwritable interactive log path follows the documented stderr fallback
   without panicking.

If entering the TUI is required to test file routing, use a deterministic real
TTY/tmux smoke test rather than weakening terminal detection in production.

### Validation commands

Run:

```bash
cargo fmt --check
cargo check
cargo build
cargo nextest run
cargo check --no-default-features --features termux
cargo tree -e features -p log4rs
```

Also rely on the release matrix to build Linux GNU, macOS Intel/ARM, Windows
x86_64/ARM64, and Android ARM64 artifacts with `--locked`.

The feature-tree check should confirm that YAML/config parsing and the selected
rolling components are enabled, while gzip/zstd, JSON/TOML formats, time
triggers, and unrelated components remain disabled.

## Risks and Mitigations

### Global logger state in tests

The `log` facade can install only one global logger per process. Factor config
building from installation, and use subprocess tests for end-to-end behavior.

### `RUST_LOG` semantic drift

Replacing env_logger with only a root `LevelFilter` would silently break module
directives. Keep `env_filter` and test both target matching and maximum-level
calculation. Also make it conspicuous that a user `log4rs.yaml` is authoritative
and therefore replaces `RUST_LOG` rather than merging with it.

### TUI corruption during logger failure

Console fallback or a runtime appender error can print through the alternate
screen. Make fallback visible once before entering the TUI and prevent repeated
runtime logger diagnostics from writing directly to the interactive terminal.

### Duplicate summary errors

The dedicated target is intentionally additive, but there should be one log
event, not one ordinary warning plus one targeted warning. Remove the original
duplicate when deleting `append_error_log()`.

### Sensitive local data

Existing diagnostics include local file paths and expanded summarizer commands.
Keep debug disabled by default, avoid logging transcript contents, and tell bug
reporters to redact logs before sharing them.

### Cross-platform file behavior

Use path APIs and log4rs appenders rather than hand-built separators or shell
commands. Test per-process file creation, rotation, PID liveness checks, and
startup reaping on Windows and Android through the release/CI matrix. A liveness
error must retain the group rather than making startup destructive.

### PID reuse and reaper races

A reused live PID can temporarily protect an older timestamped group. This is an
acceptable retention delay: the timestamp keeps old and new process ownership
separate. Concurrent startup reapers may race to remove the same dead files, so
`NotFound` must be success and all other deletion failures must be non-fatal.

### Custom configuration paths

Users can deliberately route logs outside AICS's managed directory or back into
a shared file. Cleanup must remain grammar-bound to managed filenames directly
under `{config_dir}/logs/`; never parse arbitrary custom paths into deletion
work. The reference config should model the safe per-instance layout.

## Deferred Work

- TOML/JSON log4rs configuration formats.
- JSON-formatted diagnostic logs.
- Compression.
- User-configurable built-in size/archive/group-retention constants.
- More advanced retention by total bytes or age.
- Runtime refresh of the optional log4rs configuration file.
- A TUI command for viewing/revealing the current log.
- Structured key-value logging or migration to `tracing`.
- Broad changes to the number or level of existing log events.

## Definition of Done

The migration is complete when:

- `env_logger` is absent from the manifest, lockfile, source, and current
  project documentation;
- log4rs is the only installed backend for `log` records;
- all existing `log` macro call sites continue to work;
- the built-in configuration defaults `RUST_LOG` to `warn` and retains
  global/module directive behavior;
- every process receives a distinct timestamp-plus-PID identity and every
  built-in managed file incorporates it;
- built-in TUI diagnostics use the current process's rolling file without
  terminal bleed;
- built-in command/JSON diagnostics go to stderr without contaminating stdout;
- summary failures are routed by log4rs to the current process's summary-error
  log and no handwritten append function remains;
- each process rotates only its own files with the documented size/archive
  limits;
- startup reaping keeps at most 10 groups unless more groups are still live and
  never deletes indeterminate or unrecognized files;
- `examples/log4rs.yaml` is valid, documented, copyable to
  `{config_dir}/log4rs.yaml`, and uses the reserved per-instance variables;
- valid custom configuration is authoritative, while malformed custom
  configuration warns and safely falls back;
- unavailable file sinks degrade according to the documented fallback policy;
- tests never touch the user's real config directory;
- the full nextest suite and relevant cross-platform builds pass;
- README and bug-report guidance describe the new behavior accurately.

## References

- [log4rs architecture and programmatic configuration](https://docs.rs/log4rs/1.4.0/log4rs/)
- [log4rs appenders, logger hierarchy, and configuration model](https://docs.rs/log4rs/1.4.0/log4rs/config/)
- [load a file configuration without installing the global logger](https://docs.rs/log4rs/1.4.0/log4rs/config/fn.load_config_file.html)
- [fixed-window archive count semantics](https://docs.rs/log4rs/1.4.0/log4rs/append/rolling_file/policy/compound/roll/fixed_window/struct.FixedWindowRoller.html)
- [log4rs feature flags](https://docs.rs/crate/log4rs/1.4.0/features)
- [log4rs pattern encoder fields](https://docs.rs/log4rs/1.4.0/log4rs/encode/pattern/)
- [env_filter directive parsing and record matching](https://docs.rs/env_filter/latest/env_filter/)
- [log4rs maintainer guidance for separate process output paths](https://github.com/estk/log4rs/issues/172)
- [log4rs rolling-file implementation and concurrent-modification caveat](https://docs.rs/log4rs/latest/src/log4rs/append/rolling_file/mod.rs.html)
- [log4rs 1.4.0 manifest and Rust-version requirement](https://github.com/estk/log4rs/blob/v1.4.0/Cargo.toml)
