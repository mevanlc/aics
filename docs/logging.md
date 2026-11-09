# Logging

AICS uses `tracing` events, `tracing-subscriber` for formatting and filtering,
and `tracing-appender` for background file output. Dependency messages emitted
through Rust's `log` facade are forwarded into the same subscriber.

## Log levels

Set `RUST_LOG` to enable global or module-specific diagnostics:

```bash
RUST_LOG=debug aics
RUST_LOG=aics=debug,tantivy=warn aics
RUST_LOG=off aics
```

Missing or empty `RUST_LOG` defaults to `warn`. Invalid directives produce a
startup warning and fall back to `warn`. Filtering uses tracing-subscriber's
native [EnvFilter syntax](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html#directives),
including target, span, and field directives. The former env_logger-style
`level/message-regex` syntax is no longer supported.

Log output is readable text with UTC timestamps, level, thread name/ID, target,
and event fields. ANSI colors are disabled for both files and stderr.

## Interactive log files

Each interactive TUI process writes separate daily files under the AICS
configuration directory, so simultaneous instances never share a file:

```text
logs/aics-<UTC-startup-timestamp>-p<PID>.log.YYYY-MM-DD
logs/summarizer-errors-<UTC-startup-timestamp>-p<PID>.log.YYYY-MM-DD
```

Files rotate on the first write after midnight UTC. Each process retains up to
3 daily main files and 2 daily summary-error files, including the current files.
These are file-count limits, not age or byte limits; verbose logging can make a
single day's file large.

At startup, AICS takes one system process snapshot and removes the oldest log
groups for processes that are definitely no longer running until at most 10
groups remain. More than 10 groups are retained when their PIDs are still live
or cannot be checked safely. Process start times distinguish an old log group
from a newer process that reused its PID; unavailable start times are handled
conservatively. Cleanup also recognizes the previous numbered-archive filenames.
Unrelated files are left alone.

Main and summary files use independent bounded queues with the appender's default
capacity. Ordinary diagnostics may be dropped when their queue fills, keeping
the TUI responsive. Summary failures at warn/error always go to the dedicated
summary file, even with `RUST_LOG=off`; that queue waits for space instead of
dropping failures. They also reach the main output when its filter permits.

The logging handle drains queued records during normal shutdown, early error
returns, and panic unwinding, subject to the appender's bounded shutdown wait.
Records are buffered rather than written immediately. This is not an fsync
guarantee: a stalled writer, forced termination, process abort, power loss, or
disk errors can still lose records.

## Command output and failures

Command and JSON modes send diagnostics synchronously to stderr, leaving stdout
safe for JSONL and other command output. `AICS_CONFIG_ROOT` relocates the log
directory along with `settings.json`.

If an interactive file sink cannot be opened, AICS reports the problem before
entering the TUI and falls back to stderr for the main route. If only the
dedicated summary sink is unavailable, summary failures can still reach the main
route when its `RUST_LOG` filter enables them. The upstream rolling appender may
print rotation or retention failures directly to stderr while the TUI is running.

`RUST_LOG` is the sole logging configuration. The former `log4rs.yaml` override
is no longer loaded, and `AICS_LOG_DIR`/`AICS_LOG_INSTANCE` are no longer exported.

Diagnostic logs can contain local paths and expanded summarizer commands. Review
and redact them before sharing.

[Back to the README.](../README.md#logging)
