# Logging

AICS uses log4rs behind the standard Rust `log` facade. Its built-in logging
configuration defaults to `warn`.

## Log levels

Set `RUST_LOG` to enable global or module-specific diagnostics:

```bash
RUST_LOG=debug aics
RUST_LOG=aics=debug,tantivy=warn aics
```

An invalid `RUST_LOG` value produces a startup warning and safely falls back to
`warn`.

## Interactive log files

Interactive TUI processes write separate rolling files under the AICS
configuration directory so simultaneous instances never share a file:

```text
logs/aics-<UTC-startup-timestamp>-p<PID>.log
logs/aics-<UTC-startup-timestamp>-p<PID>.log.<archive-index>
logs/summarizer-errors-<UTC-startup-timestamp>-p<PID>.log
logs/summarizer-errors-<UTC-startup-timestamp>-p<PID>.log.<archive-index>
```

The main file rolls at 2 MiB and keeps two archives. The summary-error file
rolls at 1 MiB and keeps one archive.

At startup, AICS takes one system process snapshot and removes the oldest log
groups for processes that are definitely no longer running until at most 10
groups remain. More than 10 groups are retained when their PIDs are still live
or cannot be checked safely. When available, process start times distinguish an
old log group from a newer process that reused its PID; an unavailable start
time is handled conservatively by retaining the group.

Built-in command and JSON modes send diagnostics to stderr, leaving stdout safe
for JSONL and other command output. `AICS_CONFIG_ROOT` relocates the log
directory and logging configuration along with `settings.json`.

If an interactive file sink cannot be opened, AICS reports the problem before
entering the TUI and falls back to stderr for the main route. If only the
dedicated summary sink is unavailable, summary failures can still reach the main
route when its `RUST_LOG` filter enables them.

## Custom log4rs configuration

For full customization, copy the checked-in reference configuration:

```bash
mkdir -p ~/.config/aics
cp examples/log4rs.yaml ~/.config/aics/log4rs.yaml
```

When `{config_dir}/log4rs.yaml` exists and is valid, it is authoritative: its
levels, filters, destinations, and retention replace the built-in configuration
and `RUST_LOG`. The file is read once per launch. If it is malformed, AICS prints
a startup warning and falls back to the built-in configuration.

The reference file intentionally routes every mode to per-process files. Custom
configurations should never log to stdout because that can corrupt JSONL output.

Diagnostic logs can contain local paths and expanded summarizer commands. Review
and redact them before sharing.

[Back to the README.](../README.md#logging)
