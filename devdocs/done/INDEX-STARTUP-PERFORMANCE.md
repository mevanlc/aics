# Index startup progress and performance

Verified on 2026-10-03 against the local history: 6,777 scanned files,
approximately 1.47 GB of source data, and 6,137 indexed sessions.

The original `Indexing` bar counted only parsing. After reaching its total,
startup still resolved fork relationships, constructed search projections and
documents, waited for Tantivy's commit, and saved the fingerprint state. A live
stack sample during the apparent hang showed the main thread waiting in
`IndexWriter::commit`, with worker and merge threads still processing segments.

Progress now labels reading and document construction separately, counts
relationship refreshes and removals in the document stage, and displays animated
spinners with elapsed time for committing and saving. Debug logging records the
duration of each sync stage.

The writer now budgets 50 MB per worker, capped at four workers, instead of
splitting 50 MB across all workers. Parsed transcripts are released as their
documents are queued. Search semantics and the index format remain unchanged.

| Measurement | Original release | Updated release |
| --- | ---: | ---: |
| Fresh startup | 107.9 s | 26.5 s; visual verification run 25.2 s |
| Peak resident memory | 2.92 GB | 1.88 GB |
| Indexed sessions | 6,137 | 6,137 |
| Committed segments | 53 | 21 |

Measurements used separate empty `AICS_CACHE_ROOT` and `AICS_CONFIG_ROOT`
directories, JSON mode, and `--no-apply-rules`. The original installed binary was
preserved for comparison. One active Codex transcript changed during the runs;
the set of source files and searchable session count stayed the same. A first
updated run overlapped test compilation and took 34.6 seconds; the table uses
the subsequent runs without competing build activity. These are local timings,
not a guarantee for other machines or corpora.

Validation: `cargo build --release`, `cargo test --release` (753 passed, two
existing ignored tests), `cargo fmt --all --check`, and `git diff --check`.
Progress was captured and visually inspected in an isolated 80-column tmux
terminal during the actual commit. Regression tests cover phase ordering,
unchanged syncs, removals, and the extra write when a new fork changes an
unchanged parent's supersession metadata. Tests used temporary config roots.

To collect phase timings for another isolated rebuild:

```sh
aics_profile_root=$(mktemp -d)
AICS_CONFIG_ROOT="$aics_profile_root/config" \
  AICS_CACHE_ROOT="$aics_profile_root/cache" \
  RUST_LOG=aics::index::writer=debug \
  aics --json --no-apply-rules --progress none >/dev/null
```
