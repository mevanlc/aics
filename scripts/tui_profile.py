#!/usr/bin/env python3

"""Tmux-backed profiler runner for the interactive TUI.

This launches the app with env-gated in-process profiling enabled and then
drives a short action sequence through the tmux harness. Use it when you need
to answer "which path is slow?" rather than just "does it feel slow?".

Typical uses:
  python3 scripts/tui_profile.py --threshold-ms 1 --actions type:abcde,wait
  python3 scripts/tui_profile.py --threshold-ms 1 --actions Down,wait
  python3 scripts/tui_profile.py --width 44 --actions type:abcde,wait
"""

import argparse
import json
import subprocess
import sys
import tempfile
from collections import defaultdict
from pathlib import Path


HARNESS = Path.home() / ".codex" / "skills" / "tmux-tui-test" / "scripts" / "tmux_tui_harness.py"


def run_harness(*args):
    output = subprocess.check_output(["python3", str(HARNESS), *args], text=True)
    return json.loads(output)


def send_key(session: str, key: str):
    run_harness("send", session, "--key", key)


def send_literal(session: str, text: str):
    run_harness("send", session, "--literal", text)


def wait_stable(session: str, timeout_ms: int):
    run_harness("wait", session, "--mode", "stable", "--timeout-ms", str(timeout_ms))


def summarize_profile(path: Path):
    groups = defaultdict(list)
    events = defaultdict(int)
    with path.open() as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            if line.startswith("event"):
                _, label = line.split(None, 1)
                events[label] += 1
                continue
            amount, label = line.split(" ms  ", 1)
            groups[label].append(float(amount))

    summary = {}
    for label, samples in sorted(groups.items()):
        summary[label] = {
            "count": len(samples),
            "avg_ms": round(sum(samples) / len(samples), 3),
            "max_ms": round(max(samples), 3),
        }
    if events:
        summary["events"] = dict(sorted(events.items()))
    return summary


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Run aics under tmux and summarize in-process TUI timings.",
        epilog=(
            "This is the primary investigation tool for redraw cost. Lower "
            "--threshold-ms to 1 when you want to see cheap cache-hit frames too."
        ),
    )
    parser.add_argument("--cwd", default=str(Path.cwd()))
    parser.add_argument("--width", type=int, default=120)
    parser.add_argument("--height", type=int, default=40)
    parser.add_argument("--timeout-ms", type=int, default=4000)
    parser.add_argument("--threshold-ms", type=int, default=8)
    parser.add_argument(
        "--actions",
        default="type:a,wait,backspace,wait,down,wait",
        help="Comma-separated actions: type:TEXT, key-name, wait",
    )
    parser.add_argument(
        "--command",
        nargs=argparse.REMAINDER,
        default=["target/debug/aics", "-g"],
        help="Command to run after '--'. Defaults to target/debug/aics -g",
    )
    args = parser.parse_args()

    # The script accepts a custom command, but defaulting to the debug binary
    # keeps the profiling loop fast during development.
    command = args.command
    if command and command[0] == "--":
        command = command[1:]
    if not command:
        parser.error("command must not be empty")

    with tempfile.NamedTemporaryFile(prefix="aics-tui-profile-", suffix=".log", delete=False) as handle:
        profile_path = Path(handle.name)

    env_pairs = [
        "env",
        f"AICS_TUI_PROFILE_FILE={profile_path}",
        f"AICS_TUI_PROFILE_THRESHOLD_MS={args.threshold_ms}",
    ]
    start = run_harness(
        "start",
        "--cwd",
        args.cwd,
        "--width",
        str(args.width),
        "--height",
        str(args.height),
        "--",
        *env_pairs,
        *command,
    )
    session = start["session"]

    try:
        # Wait for the initial frame before sending actions so the profile log
        # reflects the requested interaction rather than startup churn.
        wait_stable(session, args.timeout_ms)
        for raw_action in args.actions.split(","):
            action = raw_action.strip()
            if not action:
                continue
            if action == "wait":
                wait_stable(session, args.timeout_ms)
            elif action.startswith("type:"):
                send_literal(session, action.removeprefix("type:"))
            else:
                send_key(session, action)
        wait_stable(session, args.timeout_ms)
    finally:
        try:
            run_harness("stop", session)
        except subprocess.CalledProcessError:
            pass

    summary = summarize_profile(profile_path)
    print(
        json.dumps(
            {
                "session": session,
                "profile_file": str(profile_path),
                "summary": summary,
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
