# aics

`aics` is a cross-platform Rust TUI for searching local Claude Code and Codex CLI chat session history.

It builds a local index, supports interactive terminal search with preview, and can also emit JSONL results for scripting.

## Status

Early-stage, but usable. The current codebase includes:

- Claude and Codex JSONL parsers
- Incremental Tantivy indexing
- Terminal UI with search and preview
- `--json` output mode

## Install

```bash
cargo install --path .
```

## Usage

```bash
# Search sessions for the current directory
aics "deploy"

# Search all sessions
aics -g

# Emit JSONL instead of opening the TUI
aics --json "deploy"
```

Session data is read from:

- `~/.claude/projects/`
- `~/.codex/sessions/`

## License

MIT. See [LICENSE](LICENSE).
