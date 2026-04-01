# Scripts Notes

These scripts are for repeatable TUI performance investigation on Termux.

## Which Script To Use

- `tui_profile.py`: primary tool for finding hot paths inside the app.
- `tui_latency.py`: coarse before/after comparison only.

If a question is "what is slow?", start with `tui_profile.py`.
If a question is "does this change feel faster?", `tui_latency.py` is enough.

## High-Value Invocations

Typing without waiting for debounce:

```bash
python3 scripts/tui_profile.py --threshold-ms 1 --actions type:abcde,wait
```

Selection change:

```bash
python3 scripts/tui_profile.py --threshold-ms 1 --actions Down,wait
```

Preview hidden control case:

```bash
python3 scripts/tui_profile.py --width 44 --threshold-ms 1 --actions type:abcde,wait
```

Coarse end-to-end comparison:

```bash
python3 scripts/tui_latency.py --reps 3
```

## What The Numbers Mean

- `terminal.draw`: total draw cost including terminal flush.
- `app.draw`: app-side work during a frame.
- `preview.render`: right-pane render cost.
- `preview.render_session_text`: expensive preview text generation.
- `preview.cache.hit` / `preview.cache.miss`: whether the preview reused the rendered text cache.

If `width 44` is much faster than the normal width, the preview pane is still the main suspect.

If `preview.render_session_text` is absent on a typing test, the cache is working for those frames.

If cache-hit frames are still slow, the remaining cost is likely widget/layout/rendering overhead rather than markdown parsing.

## Practical Tips

- Use `--threshold-ms 1` when investigating. The default threshold is better for summaries, but it hides cheap cache-hit frames.
- Use fixed dimensions so comparisons stay meaningful.
- Keep the action list short. Longer runs mix startup, debounce, search response, and steady-state redraws together.
- Separate "typing before debounce" from "wait for results" runs. They answer different questions.
- Compare one variable at a time: width, preview visibility, query highlighting, selection movement.
- The scripts default to `target/debug/aics`. Rebuild before profiling if you changed Rust code.
- If you want release-build numbers, pass a custom command:

```bash
python3 scripts/tui_profile.py -- -- target/release/aics -g
```

## Current Heuristic

Recent profiling showed:

- cache-hit typing frames are cheap
- preview rebuilds are expensive
- hiding the preview drops frame cost sharply

So the first question for any new lag report should be:
"Is this a preview rebuild, or should this have been a cache hit?"
