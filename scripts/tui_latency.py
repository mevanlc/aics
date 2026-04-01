#!/usr/bin/env python3

"""Coarse end-to-end latency probe for the interactive TUI.

Use this when you want a quick "does this feel faster/slower" comparison.
It measures visible screen changes through the tmux harness, so the numbers
include harness polling overhead and should be treated as comparative, not
absolute render timings.

Typical uses:
  python3 scripts/tui_latency.py --reps 3
  python3 scripts/tui_latency.py --width 44 --reps 3
"""

import argparse
import json
import statistics
import subprocess
import sys
import time
from pathlib import Path


HARNESS = Path.home() / ".codex" / "skills" / "tmux-tui-test" / "scripts" / "tmux_tui_harness.py"


def run_harness(*args):
    output = subprocess.check_output(["python3", str(HARNESS), *args], text=True)
    return json.loads(output)


def read_screen(session: str, lines: str) -> str:
    result = run_harness("read", session, "--plain", "--lines", lines)
    return result["plain_text"]


def send_input(session: str, key: str | None, literal: str | None):
    args = ["send", session]
    if key is not None:
        args.extend(["--key", key])
    else:
        args.extend(["--literal", literal or ""])
    run_harness(*args)


def wait_for_stable_screen(session: str, lines: str, polls: int = 20, sleep_s: float = 0.05):
    try:
        run_harness("wait", session, "--mode", "stable", "--timeout-ms", "5000")
        return
    except subprocess.CalledProcessError:
        pass

    for _ in range(polls):
        try:
            first = read_screen(session, lines)
            time.sleep(sleep_s)
            second = read_screen(session, lines)
        except subprocess.CalledProcessError:
            time.sleep(sleep_s)
            continue
        if first == second:
            return

    raise RuntimeError(f"screen for {session} did not become readable/stable")


def measure_action(session: str, lines: str, kind: str, value: str, stable_ms: int, timeout_s: float, poll_s: float):
    baseline = read_screen(session, lines)
    start = time.monotonic()
    if kind == "key":
        send_input(session, key=value, literal=None)
    else:
        send_input(session, key=None, literal=value)

    first_change = None
    last_text = baseline
    stable_since = None

    while time.monotonic() - start < timeout_s:
        current = read_screen(session, lines)
        now = time.monotonic()

        if current != baseline and first_change is None:
            first_change = now

        if current != last_text:
            stable_since = now
            last_text = current
        elif first_change is not None:
            if stable_since is None:
                stable_since = now
            if (now - stable_since) * 1000 >= stable_ms:
                return {
                    "first_change_ms": (first_change - start) * 1000,
                    "stable_ms": (now - start) * 1000,
                }

        time.sleep(poll_s)

    return {"timeout": True, "first_change_ms": None, "stable_ms": None}


def summarize(samples):
    first = [sample["first_change_ms"] for sample in samples if sample.get("first_change_ms") is not None]
    stable = [sample["stable_ms"] for sample in samples if sample.get("stable_ms") is not None]
    return {
        "samples": samples,
        "avg_first_change_ms": statistics.mean(first) if first else None,
        "avg_stable_ms": statistics.mean(stable) if stable else None,
    }


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Measure coarse visible TUI latency through the tmux harness.",
        epilog=(
            "Best for before/after comparisons. For actual hot-path attribution, use "
            "scripts/tui_profile.py instead."
        ),
    )
    parser.add_argument("--cwd", default=str(Path.cwd()), help="Working directory for the TUI process.")
    parser.add_argument("--width", type=int, default=120, help="tmux pane width")
    parser.add_argument("--height", type=int, default=40, help="tmux pane height")
    parser.add_argument("--lines", default="1:12", help="Visible rows to poll when detecting screen changes")
    parser.add_argument("--stable-ms", type=int, default=150, help="How long the screen must remain unchanged")
    parser.add_argument("--timeout-ms", type=int, default=3000, help="Timeout per action")
    parser.add_argument("--poll-ms", type=int, default=10, help="Polling interval")
    parser.add_argument(
        "--reps",
        type=int,
        default=3,
        help="Repetitions per action",
    )
    parser.add_argument(
        "--command",
        nargs=argparse.REMAINDER,
        default=["target/debug/aics", "-g"],
        help="Command to run after '--'. Defaults to target/debug/aics -g",
    )
    args = parser.parse_args()

    # Keep the default target aligned with the current debug binary so this is
    # cheap to rerun during local iteration.
    command = args.command
    if command and command[0] == "--":
        command = command[1:]
    if not command:
        parser.error("command must not be empty")

    start = run_harness(
        "start",
        "--cwd",
        args.cwd,
        "--width",
        str(args.width),
        "--height",
        str(args.height),
        "--",
        *command,
    )
    session = start["session"]

    try:
        wait_for_stable_screen(session, args.lines)

        results = {
            "down": [
                measure_action(
                    session,
                    args.lines,
                    "key",
                    "Down",
                    stable_ms=args.stable_ms,
                    timeout_s=args.timeout_ms / 1000,
                    poll_s=args.poll_ms / 1000,
                )
                for _ in range(args.reps)
            ],
            "type_a": [
                measure_action(
                    session,
                    args.lines,
                    "literal",
                    "a",
                    stable_ms=args.stable_ms,
                    timeout_s=args.timeout_ms / 1000,
                    poll_s=args.poll_ms / 1000,
                )
                for _ in range(args.reps)
            ],
            "backspace": [
                measure_action(
                    session,
                    args.lines,
                    "key",
                    "BSpace",
                    stable_ms=args.stable_ms,
                    timeout_s=args.timeout_ms / 1000,
                    poll_s=args.poll_ms / 1000,
                )
                for _ in range(args.reps)
            ],
        }

        summary = {name: summarize(samples) for name, samples in results.items()}
        summary["session"] = session
        summary["command"] = command
        print(json.dumps(summary, indent=2))
    finally:
        run_harness("stop", session)

    return 0


if __name__ == "__main__":
    sys.exit(main())
