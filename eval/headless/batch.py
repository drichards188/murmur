"""N headless episodes across cores, one JSON record each, and the aggregate (docs/HEADLESS.md,
section 10). MODEL RESULTS from a scripted operator on scripted or generated situations: never
human data.

    .venv/bin/python -m eval.headless.batch --situation harbor-watch --seeds 1-10 --workers 10
    .venv/bin/python -m eval.headless.batch --families fast-unknown decoy --seeds 1-30 --workers 20
    .venv/bin/python -m eval.headless.batch --situation harbor-watch --seeds 1-200 --workers 14 --speed

Each episode runs in its own `murmur-headless` process (target/release, built first). Records go
to `eval/results/headless/<batch>/` (one `<situation>-s<seed>.json` each, the audit log beside it
with `--keep-audit`), the aggregate to `aggregate.json`: per situation or family (never pooling
tuning and held-out families), each metric's mean with a Student t 95 % interval clustered by
seed and, for proportions, the pooled Wilson interval (eval/stats.py), and the speed: episodes
per core-hour (episodes / (wall hours x workers)) beside the single-worker rate.
"""

from __future__ import annotations

import argparse
import json
import multiprocessing as mp
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

from . import REPO
from .situations import Calibration, from_situation

RESULTS = REPO / "eval" / "results" / "headless"


def parse_seeds(spec: list[str]) -> list[int]:
    out: list[int] = []
    for s in spec:
        if "-" in s:
            a, b = s.split("-", 1)
            out += list(range(int(a), int(b) + 1))
        else:
            out.append(int(s))
    return out


def build() -> None:
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    subprocess.run([cargo, "build", "--release", "-q", "-p", "murmur-headless"], cwd=REPO, check=True)


def _job(job: dict[str, Any]) -> dict[str, Any]:
    """One episode in a worker: the record (without the audit text, kept beside it on request)."""
    from .episode import Episode, Options
    from .families import generate
    from .metrics import episode_metrics

    t0 = time.perf_counter()
    calib = Calibration(**job["calibration"])
    with tempfile.TemporaryDirectory(prefix="hl-", dir=job.get("tmp")) as td:
        wd = Path(td)
        if job.get("family"):
            spec, contacts = generate(job["family"], job["situation_seed"], wd)
        else:
            spec, contacts = from_situation(job["situation"]), None
        opts = Options(operator=job.get("operator"), calibration=calib, anchors=job.get("anchors", True))
        try:
            rec = Episode(spec, job["seed"], wd, opts, contacts_path=contacts).run()
        except Exception as e:  # noqa: BLE001 (a failed episode is a record too)
            return {"failed": f"{type(e).__name__}: {e}", "job": job, "wall_s": round(time.perf_counter() - t0, 3)}
        rec["metrics"] = episode_metrics(rec)
        out_dir = Path(job["out"])
        name = f"{job.get('family') or job['situation']}-s{job['seed']}"
        if job.get("keep_audit"):
            (out_dir / f"{name}.audit.jsonl").write_text(rec["audit_text"])
        rec.pop("audit_text", None)
        (out_dir / f"{name}.json").write_text(json.dumps(rec, sort_keys=True))
    return {"name": name, "metrics": rec["metrics"], "perf": rec["perf"], "situation": rec["situation"],
            "seed": rec["seed"], "wall_s": round(time.perf_counter() - t0, 3)}


def t_interval(xs: list[float]) -> tuple[float, float] | None:
    from eval.stats import t_interval as ti

    if len(xs) < 2:
        return None
    return ti(xs)


def wilson(k: int, n: int) -> tuple[float, float] | None:
    from eval.stats import wilson as wi

    return wi(k, n) if n else None


def perf_breakdown(rs: list[dict[str, Any]]) -> dict[str, Any]:
    """Where an episode's wall time goes, mean seconds per episode: the process start (spawn and
    assembly), the simulator serving requests (`server_s`, the sum of its `call.*` slots: the
    bridge's tasks, the stand-ins, contacts and the emulator run inside them, broken down under
    `tasks`), the pipe and JSON on both ends (`ipc_s`: the driver's wait minus the server's own
    time), and the driver (`python_s`, the playbook's decide calls `playbook_s` among it)."""
    if not rs:
        return {}

    def mean(xs: list[float]) -> float:
        return round(statistics.fmean(xs), 4) if xs else 0.0

    perfs = [r["perf"] for r in rs if r.get("perf")]
    rust = [p.get("rust") or {} for p in perfs]
    names = sorted({k for x in rust for k in x})
    server = [sum(v["s"] for k, v in x.items() if k.startswith("call.")) for x in rust]
    return {
        "episodes": len(perfs),
        "wall_s": mean([r["wall_s"] for r in rs]),
        "start_s": mean([p["start_s"] for p in perfs]),
        "server_s": mean(server),
        "ipc_s": mean([p["sim_wait_s"] - s for p, s in zip(perfs, server)]),
        "python_s": mean([p["python_s"] for p in perfs]),
        "playbook_s": mean([p.get("playbook_s", 0.0) for p in perfs]),
        "calls": mean([float(p["calls"]) for p in perfs]),
        "ticks": mean([float(p["ticks"]) for p in perfs]),
        "tasks": {k: mean([x.get(k, {}).get("s", 0.0) for x in rust]) for k in names},
    }


def aggregate(results: list[dict[str, Any]]) -> dict[str, Any]:
    groups: dict[str, list[dict[str, Any]]] = {}
    for r in results:
        if "failed" in r:
            continue
        s = r["situation"]
        key = f"{s.get('family')} ({s.get('split')})" if s.get("family") else s["name"]
        groups.setdefault(key, []).append(r)
    out: dict[str, Any] = {}
    for key, rs in sorted(groups.items()):
        m = [r["metrics"] for r in rs]
        cov = [x["look_covered"] for x in m if x["look_covered"] is not None]
        ttl = [v for x in m for v in x["time_to_look"].values() if v is not None]
        fl = [x["false_looks"] for x in m]
        pk = sum(x["precision"]["legit"] for x in m)
        pn = sum(x["precision"]["n"] for x in m)
        hc = [x["human_cards"] for x in m]
        out[key] = {
            "episodes": len(rs),
            "look_covered": {"k": sum(cov), "n": len(cov), "wilson": wilson(sum(cov), len(cov))},
            "time_to_look_s": {"mean": round(statistics.fmean(ttl), 3) if ttl else None, "n": len(ttl),
                               "t95": t_interval(ttl)},
            "false_looks": {"mean": round(statistics.fmean(fl), 3) if fl else None, "t95": t_interval([float(v) for v in fl])},
            "precision": {"k": pk, "n": pn, "wilson": wilson(pk, pn)},
            "human_cards": {"mean": round(statistics.fmean(hc), 3) if hc else None, "t95": t_interval([float(v) for v in hc])},
            "unfaithful": sum(x["unfaithful"] for x in m),
            "objected": sum(x["objected"] for x in m),
            "reproposals_after_rejection": sum(x["reproposals_after_rejection"] for x in m),
            "perf": perf_breakdown(rs),
        }
    return out


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--situation", default=None, help="a situation file or name (sim/situations)")
    ap.add_argument("--families", nargs="*", default=None, help="generated families (eval/headless/split.toml)")
    ap.add_argument("--seeds", nargs="+", default=["1-10"])
    ap.add_argument("--workers", type=int, default=max(1, (os.cpu_count() or 2) // 2))
    ap.add_argument("--operator", default=None, help="operator model as JSON (operator.py); default the situation's")
    ap.add_argument("--out", type=Path, default=None)
    ap.add_argument("--keep-audit", action="store_true")
    ap.add_argument("--no-anchors", action="store_true")
    ap.add_argument("--no-build", action="store_true")
    ap.add_argument("--speed", action="store_true", help="also report episodes per core-hour (G1b)")
    args = ap.parse_args(argv)
    if not args.no_build:
        build()
    seeds = parse_seeds(args.seeds)
    batch = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    out = args.out or RESULTS / batch
    out.mkdir(parents=True, exist_ok=True)
    calib = Calibration.load().as_dict()
    operator = json.loads(args.operator) if args.operator else None
    jobs = []
    if args.families:
        for fam in args.families:
            for s in seeds:
                jobs.append({"family": fam, "situation_seed": s, "seed": s})
    else:
        for s in seeds:
            jobs.append({"situation": args.situation or "harbor-watch", "seed": s})
    for j in jobs:
        j.update(calibration=calib, operator=operator, out=str(out), keep_audit=args.keep_audit,
                 anchors=not args.no_anchors)
    t0 = time.time()
    if args.workers <= 1:
        results = [_job(j) for j in jobs]
    else:
        with mp.get_context("spawn").Pool(args.workers) as pool:
            results = list(pool.imap_unordered(_job, jobs, chunksize=1))
    wall = time.time() - t0
    done = [r for r in results if "failed" not in r]
    agg = {
        "batch": batch, "jobs": len(jobs), "done": len(done), "failed": [r for r in results if "failed" in r],
        "workers": args.workers, "wall_s": round(wall, 3), "calibration": calib, "operator": operator,
        "groups": aggregate(results),
        "speed": {
            "episodes_per_core_hour": round(len(done) / (wall / 3600.0 * args.workers), 1) if wall > 0 else None,
            "episode_wall_s_mean": round(statistics.fmean([r["wall_s"] for r in done]), 3) if done else None,
            "episode_wall_s_p95": round(sorted(r["wall_s"] for r in done)[int(0.95 * (len(done) - 1))], 3) if done else None,
            "single_worker_rate_per_hour": round(3600.0 / statistics.fmean([r["wall_s"] for r in done]), 1) if done else None,
        },
        "caveat": "Model results from a scripted operator on scripted or generated situations: not human data.",
    }
    (out / "aggregate.json").write_text(json.dumps(agg, indent=1, sort_keys=True))
    print(json.dumps({k: agg[k] for k in ("jobs", "done", "workers", "wall_s", "speed")}, indent=1))
    for k, v in agg["groups"].items():
        print(k, json.dumps({x: y for x, y in v.items() if x != "perf"}))
        print("  perf", json.dumps(v["perf"]))
    if agg["failed"]:
        print(f"{len(agg['failed'])} failed:", [f["failed"] for f in agg["failed"]][:5], file=sys.stderr)
    print(out)
    return 0 if not agg["failed"] else 1


if __name__ == "__main__":
    sys.exit(main())
