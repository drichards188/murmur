"""The calibration of CRITERIA.md: three process start offsets, and nothing else, from the
full-stack calibration runs (seeds 101 to 103, never the gate's): each the median over the runs.

    .venv/bin/python -m eval.headless.calibrate logs/headless-g1/calibration --write

- `contacts_start_s`: the contacts process's start (its /proc start time) after the situation's
  zero (the link emulator's start, as the runner takes it);
- `standins_start_s`: the stand-in engine's start, likewise;
- `ai_first_tick_s`: the playbook's `bridge.connected` line (its first tick follows at once)
  after `ai_start_s`.

`--write` writes eval/headless/calibration.toml (what the simulator reads); the values also go
into a dated section of CRITERIA.md, by hand, before the first gate run.
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
from pathlib import Path

from . import HERE, REPO
from .fullstack import _jsonl


def offsets(run: Path) -> dict[str, float]:
    sit = _jsonl(run / "situation.jsonl")
    zero = next(r["zero_unix"] for r in sit if r.get("event") == "zero")
    ai_start = next((r["t_s"] for r in sit if r.get("event") == "ai_start"), None)
    starts = json.loads((run / "proc-starts.json").read_text())["starts"]
    connected = next(r["t"] for r in _jsonl(run / "agent.jsonl") if r.get("kind") == "bridge.connected")
    planned_ai = 45.0
    return {
        "contacts_start_s": round(starts["contacts"] - zero, 3),
        "standins_start_s": round(starts["standins"] - zero, 3),
        "ai_first_tick_s": round(connected - zero - planned_ai, 3),
        "bridge_start_s": round(starts["bridge"] - zero, 3),
        "ai_process_start_s": ai_start,
    }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("root", type=Path)
    ap.add_argument("--write", action="store_true")
    args = ap.parse_args(argv)
    root = args.root if args.root.is_absolute() else REPO / args.root
    runs = sorted(d for d in root.glob("seed-*") if d.is_dir() and (d / "proc-starts.json").exists())
    per = {d.name: offsets(d) for d in runs}
    for k, v in per.items():
        print(k, v)
    med = {k: round(statistics.median(v[k] for v in per.values()), 3)
           for k in ("contacts_start_s", "standins_start_s", "ai_first_tick_s")}
    print("median", med)
    if args.write:
        lines = [f"# CRITERIA.md calibration: medians over {', '.join(per)} ({root.relative_to(REPO)})"]
        lines += [f"{k} = {v}" for k, v in med.items()]
        lines.append(f'source = "full-stack calibration runs {", ".join(per)}, medians"')
        (HERE / "calibration.toml").write_text("\n".join(lines) + "\n")
        print(HERE / "calibration.toml")
    return 0


if __name__ == "__main__":
    sys.exit(main())
