"""The calibration of CRITERIA.md: process start offsets, and nothing else, from the full-stack
calibration runs (seeds 101 to 106, never the gate's).

    .venv/bin/python -m eval.headless.calibrate logs/headless-g1/calibration --write

Per run, on the situation's clock (zero: the link emulator's start, as the runner takes it):

- `contacts_start_s`, `standins_start_s`: the contacts process's and the stand-in engine's
  starts (their /proc start times);
- `bridge_start_s`: the bridge's decision layer starting, its loops with it (the run's first
  anchor, `why: start`, on UTC);
- `ai_first_tick_s`: the playbook's `bridge.connected` line (its first tick follows at once)
  after `ai_start_s`.

The simulator takes the median of each of the first three, and for the first tick the range
(lowest to highest) over the runs, from which each episode draws its own (2026-10-10 section of
CRITERIA.md). `--write` writes eval/headless/calibration.toml; the values also go into a dated
section of CRITERIA.md, by hand, before a gate run.
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
from pathlib import Path

from . import HERE, REPO
from .fullstack import _jsonl

MEDIANS = ("contacts_start_s", "standins_start_s", "bridge_start_s")


def offsets(run: Path) -> dict[str, float]:
    sit = _jsonl(run / "situation.jsonl")
    zero = next(r["zero_unix"] for r in sit if r.get("event") == "zero")
    ai_start = next((r["t_s"] for r in sit if r.get("event") == "ai_start"), None)
    starts = json.loads((run / "proc-starts.json").read_text())["starts"]
    connected = next(r["t"] for r in _jsonl(run / "agent.jsonl") if r.get("kind") == "bridge.connected")
    anchors = next(iter(sorted(run.glob("situation-*.anchors"))), None)
    first = json.loads(anchors.read_text().splitlines()[0]) if anchors else None
    planned_ai = 45.0
    return {
        "contacts_start_s": round(starts["contacts"] - zero, 3),
        "standins_start_s": round(starts["standins"] - zero, 3),
        "bridge_start_s": round(first["utc"] / 1000.0 - zero, 3) if first else None,
        "ai_first_tick_s": round(connected - zero - planned_ai, 3),
        "bridge_process_start_s": round(starts["bridge"] - zero, 3),
        "ai_process_start_s": ai_start,
    }


def fit(per: dict[str, dict[str, float]]) -> dict[str, float]:
    out = {k: round(statistics.median(v[k] for v in per.values()), 3) for k in MEDIANS}
    ticks = [v["ai_first_tick_s"] for v in per.values()]
    out["ai_first_tick_lo_s"], out["ai_first_tick_hi_s"] = round(min(ticks), 3), round(max(ticks), 3)
    return out


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("root", type=Path)
    ap.add_argument("--seeds", type=int, nargs="*", default=None, help="the runs to fit (default: every seed-* under root)")
    ap.add_argument("--write", action="store_true")
    args = ap.parse_args(argv)
    root = args.root if args.root.is_absolute() else REPO / args.root
    runs = sorted(d for d in root.glob("seed-*") if d.is_dir() and (d / "proc-starts.json").exists())
    if args.seeds:
        runs = [d for d in runs if int(d.name.split("-")[1]) in args.seeds]
    if any(not 101 <= int(d.name.split("-")[1]) <= 106 for d in runs):
        raise SystemExit("calibration fits seeds 101 to 106 only (CRITERIA.md)")
    per = {d.name: offsets(d) for d in runs}
    for k, v in per.items():
        print(k, v)
    out = fit(per)
    print("fit", out)
    if args.write:
        lines = [f"# CRITERIA.md calibration (2026-10-10 section): medians of the process starts, the range of the",
                 f"# playbook's first tick, over {', '.join(per)} ({root.relative_to(REPO)})"]
        lines += [f"{k} = {v}" for k, v in out.items()]
        lines.append(f'source = "full-stack calibration runs {", ".join(per)}: medians, first-tick range"')
        (HERE / "calibration.toml").write_text("\n".join(lines) + "\n")
        print(HERE / "calibration.toml")
    return 0


if __name__ == "__main__":
    sys.exit(main())
