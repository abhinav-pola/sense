#!/usr/bin/env python3
"""Startup benchmarks with hyperfine, checked against perf/baseline.json.

Each metric is the median of repeated runs. A run fails when a metric's median
exceeds its budget, or regresses past the baseline median by more than its
tolerance. Raising the baseline is an explicit, committed step.

usage: scripts/perf.py [--update-baseline] [--runs N]
Needs a release build and scripts/bundle.sh to have run.
"""
import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BASELINE = ROOT / "perf/baseline.json"
OUT = ROOT / "target/perf"
QUIT_AFTER_FIRST_FRAME = {"SENSE_EXIT_AFTER_FIRST_FRAME": "1", "SENSE_BACKGROUND": "1"}

app = ROOT / "target/release/Sense.app"
METRICS = {
    # Process start through the first rendered frame, bypassing LaunchServices.
    "startup_binary_ms": {
        "command": str(ROOT / "target/release/sense"),
        "env": QUIT_AFTER_FIRST_FRAME,
        "budget_ms": 200,
        "tolerance": 0.10,
    },
    # Through LaunchServices and RunningBoard like a dock click, but in the
    # background (-g, no activation) and including process exit (-W).
    "startup_bundle_ms": {
        "command": "open -W -n -g "
        + " ".join(f"--env {k}={v}" for k, v in QUIT_AFTER_FIRST_FRAME.items())
        + f" {app}",
        "env": {},
        "budget_ms": 450,
        # LaunchServices adds tens of ms of jitter under normal desktop load.
        "tolerance": 0.25,
    },
}


def measure(name, spec, runs):
    export = OUT / f"{name}.json"
    subprocess.run(
        ["hyperfine", "-N", "--warmup", "3", "--runs", str(runs), "--style", "none",
         "--export-json", str(export), spec["command"]],
        env=dict(os.environ, **spec["env"]),
        check=True,
        stdout=subprocess.DEVNULL,
    )
    result = json.loads(export.read_text())["results"][0]
    return {"median": result["median"] * 1000, "stddev": result["stddev"] * 1000}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--update-baseline", action="store_true")
    parser.add_argument("--force", action="store_true", help="save a baseline even if it regressed")
    parser.add_argument("--runs", type=int, default=20)
    args = parser.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)

    baseline = json.loads(BASELINE.read_text()) if BASELINE.exists() else {}
    current, failures = {}, []
    for name, spec in METRICS.items():
        result = measure(name, spec, args.runs)
        current[name] = result
        median = result["median"]
        line = f"{name:20} {median:7.1f} ms ±{result['stddev']:5.1f}"
        if name in baseline:
            base = baseline[name]["median"]
            line += f"   baseline {base:7.1f} ({(median - base) / base:+.1%})"
            if median > base * (1 + spec["tolerance"]):
                failures.append(f"{name} regressed {median:.1f} ms vs baseline {base:.1f} ms")
        if median > spec["budget_ms"]:
            failures.append(f"{name} {median:.1f} ms is over its {spec['budget_ms']} ms budget")
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
