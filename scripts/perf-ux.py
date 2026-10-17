#!/usr/bin/env python3
"""Interaction latency: what each user action costs, in an optimized build.

Drives the app over the automation socket (replayed fixtures, no network)
and times each action with `timed`: the action itself plus laying out and
painting the frame that shows it. Reports the median and p95 of each,
against perf/ux-baseline.json, plus the app's memory once it has run them.

usage: scripts/perf-ux.py [--update-baseline] [--force] [--runs N]
"""
import argparse
import importlib.util
import json
import shutil
import statistics
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BASELINE = ROOT / "perf/ux-baseline.json"
BINARY = ROOT / "target/profiling/sense"

spec = importlib.util.spec_from_file_location("e2e", ROOT / "scripts/e2e.py")
e2e = importlib.util.module_from_spec(spec)
spec.loader.exec_module(e2e)
e2e.BINARY = BINARY

# Frames are 8.3 ms at 120 Hz: an action that fits shows on the next one.
BUDGET_MS = 8.3
# Run-to-run noise on a busy laptop is large at this scale.
TOLERANCE = 0.30
RSS_TOLERANCE = 0.15


def timed(app, **command):
    return app.send(timed=True, **command)["ms"]


def summary(samples):
    samples = sorted(samples)
    return {"median": statistics.median(samples), "p95": samples[int(len(samples) * 0.95) - 1]}


def run(runs):
    results = {}
    app = e2e.App("steps.jsonl", e2e.fresh_workspace("perf-ux"), pace="1")
    try:
        # Streaming: the frame cost while text and tool calls arrive.
        app.send(cmd="type", text="First read calc.py and say in one sentence what's wrong.")
        app.send(cmd="submit")
        frames = []
        while app.state()["chat"]["busy"] or not app.state()["chat"]["session"]:
            frames.append(timed(app, cmd="frame"))
        results["streaming_frame"] = summary(frames)

        results["keystroke"] = summary([timed(app, cmd="key", keys="a") for _ in range(runs)])
        results["fold_toggle"] = summary([timed(app, cmd="toggle_fold", index=0) for _ in range(runs)])
        results["panel_toggle"] = summary([timed(app, cmd="toggle_panel") for _ in range(runs)])
        # Between the finished chat (transcript, diff panel) and a new one.
        app.send(cmd="new_chat")
        results["chat_switch"] = summary([timed(app, cmd="select_chat", index=i % 2) for i in range(runs)])
        results["new_chat"] = summary([timed(app, cmd="new_chat") for _ in range(runs)])
        # A drag is 11 events (press, 8 moves, release), each drawn: per frame.
        results["divider_drag_frame"] = summary([
            timed(app, cmd="drag", **{"from": [260 + 10 * (i % 2), 400], "to": [270 - 10 * (i % 2), 400]}) / 11
            for i in range(runs)
        ])
        results["rss_mb"] = {"median": rss_mb(app)}
    finally:
        app.close()

    # Math: a reply of inline and display formulas streams in. Only frames that
    # show new text are sampled, so the frames that typeset its formulas count.
    app = e2e.App("math.jsonl", e2e.fresh_workspace("perf-ux-math"))
    try:
        app.send(cmd="type", text="What is a hertz?")
        app.send(cmd="submit")
        frames, shown = [], ""
        while (s := app.state())["chat"]["busy"] or not s["chat"]["session"]:
            ms = timed(app, cmd="frame")
            reply = next((e["agent"] for e in reversed(s["chat"]["entries"]) if "agent" in e), "")
            if reply != shown:
                frames.append(ms)
                shown = reply
        assert len(frames) >= 10, f"only {len(frames)} streaming frames"
        results["math_streaming_frame"] = summary(frames)
        # Every text size step is a size no formula was drawn at, so each
        # visible formula is typeset in the measured frame: the cost of first
        # showing a chat full of math, as when opening one from the library.
        results["math_first_raster"] = summary([timed(app, cmd="key", keys="cmd-=") for _ in range(12)])
    finally:
        app.close()
    return results


def rss_mb(app):
    rss = subprocess.run(["ps", "-o", "rss=", "-p", str(app.process.pid)], capture_output=True, text=True)
    return int(rss.stdout.strip()) / 1024


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--update-baseline", action="store_true")
    parser.add_argument("--force", action="store_true", help="save a baseline even if it regressed")
    parser.add_argument("--runs", type=int, default=40)
    args = parser.parse_args()
    subprocess.run(["cargo", "build", "-q", "--profile", "profiling", "-p", "sense", "--features", "automation"],
                   cwd=ROOT, check=True)
    e2e.OUT.mkdir(parents=True, exist_ok=True)

    baseline = json.loads(BASELINE.read_text()) if BASELINE.exists() else {}
    current = run(args.runs)
    failures = []
    for name, result in current.items():
        median = result["median"]
        if name == "rss_mb":
            line = f"{name:22} {median:7.1f} MB"
        else:
            line = f"{name:22} {median:7.2f} ms  p95 {result['p95']:7.2f}"
            if result["p95"] > BUDGET_MS:
                failures.append(f"{name} p95 {result['p95']:.2f} ms misses the {BUDGET_MS} ms frame")
        if name in baseline:
            base = baseline[name]["median"]
            line += f"   baseline {base:7.2f} ({(median - base) / base:+.0%})"
            if median > base * (1 + (RSS_TOLERANCE if name == "rss_mb" else TOLERANCE)):
                failures.append(f"{name} regressed: {median:.2f} vs baseline {base:.2f}")
        elif baseline and not args.update_baseline:
            failures.append(f"{name} has no baseline: run with --update-baseline and commit {BASELINE.relative_to(ROOT)}")
        print(line)

    if args.update_baseline and failures and not args.force:
        print("refusing to save a regressed baseline; fix it or pass --force")
    elif args.update_baseline:
        BASELINE.parent.mkdir(parents=True, exist_ok=True)
        BASELINE.write_text(json.dumps(current, indent=2) + "\n")
        print(f"wrote {BASELINE.relative_to(ROOT)}")
    for failure in failures:
        print(f"FAIL  {failure}")
    sys.exit(1 if failures and not (args.update_baseline and args.force) else 0)


if __name__ == "__main__":
    main()
