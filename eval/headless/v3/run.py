"""A scoring v3 batch and its dev check (eval/SCORING-V3.md sections 8 and 10). MODEL RESULTS ONLY:
free, deterministic agents on generated situations; never human data, never an API call.

    .venv/bin/python -m eval.headless.v3.run batch --families all --seeds 5000-5009 \\
        --agent none random-valid oracle-v3 --workers 20 --out <dir>
    .venv/bin/python -m eval.headless.v3.run report <dir>

**batch** runs every (family, situation seed, agent) once (replicate 0; its seed
sha256("{family}:{situation_seed}:0") mod 2^31, as v2's), the approve-all operator, the paused
clock, the picture sampler on (`Options.scoring = "v3"`), each record gzipped into the directory
with its index (`episodes_v3.json`); an episode that fails is run again once, a second failure is a
`harness_error`. A `none` replicate is added for every situation when the agents lack one (labels
judge `useful` against it). **report** labels every episode against its `none` replicate (labels.py),
and writes `dev_v3.json` and `dev_v3.md`: per family and world, the coverage, need served per agent
(pooled over the family's needs) with time to serve, 10.1's conditions (`none` at most 50 %, the oracle
at least 80 %), and the single-cause analysis of the needs `none` and the oracle lost (`causes.py`).
"""

from __future__ import annotations

import argparse
import json
import multiprocessing as mp
import os
import random
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

from .. import REPO
from ..batch import build, fidelity_digest, parse_seeds, replicate_seed
from ..situations import Calibration

APPROVE_ALL = {"mode": "approve-all"}


def job_name(job: dict[str, Any]) -> str:
    return f"{job['family']}-{job['agent']}-s{job['situation_seed']}-r{job['r']}"


def _episode(job: dict[str, Any], wd: Path) -> dict[str, Any]:
    from ..episode import Episode, Options
    from .families import generate

    spec, contacts = generate(job["family"], job["situation_seed"], wd)
    spec.seed = int(job["seed"])
    spec.replicate = int(job["r"])
    opts = Options(agent=job["agent"], operator=job.get("operator"), calibration=Calibration(**job["calibration"]),
                   scoring="v3", replicate={"r": job["r"], "seed": job["seed"], "situation_seed": job["situation_seed"]})
    return Episode(spec, job["seed"], wd, opts, contacts_path=contacts).run()


def run_job(job: dict[str, Any]) -> dict[str, Any]:
    """One episode, run again once if it failed; the record gzipped; its index entry."""
    from ..v2.record import dump

    t0 = time.perf_counter()
    out = Path(job["out"])
    name = job_name(job)
    errors = []
    for attempt in (1, 2):
        with tempfile.TemporaryDirectory(prefix="hl3-") as td:
            try:
                rec = _episode(job, Path(td))
            except Exception as e:  # noqa: BLE001 (a harness error, counted)
                errors.append(f"{type(e).__name__}: {e}")
                continue
        fid = fidelity_digest({**rec, "v2": rec.get("v3")})
        rec.pop("audit_text", None)
        if errors:
            rec["harness_reruns"] = errors
        path = out / f"{name}.json.gz"
        size = dump(rec, path)
        return {"name": name, "path": str(path), "family": job["family"], "agent": job["agent"],
                "situation_seed": job["situation_seed"], "r": job["r"], "seed": job["seed"], "class": rec.get("episode_class"),
                "attempts": attempt, "record_bytes": size, "wall_s": round(time.perf_counter() - t0, 3),
                "fidelity": fid, "reruns": errors}
    return {"name": name, "path": None, "family": job["family"], "agent": job["agent"],
            "situation_seed": job["situation_seed"], "r": job["r"], "seed": job["seed"], "class": "harness_error",
            "attempts": 2, "errors": errors, "wall_s": round(time.perf_counter() - t0, 3)}


def jobs_for(families: list[str], seeds: list[int], agents: list[str], out: Path, order_seed: int = 0) -> list[dict[str, Any]]:
    calib = Calibration.load().as_dict()
    jobs = []
    for fam in families:
        for s in seeds:
            seed = replicate_seed(fam, s, 0)
            for a in agents:
                jobs.append({"family": fam, "situation_seed": s, "r": 0, "seed": seed, "agent": a, "operator": APPROVE_ALL,
                             "calibration": calib, "out": str(out)})
    random.Random(order_seed).shuffle(jobs)
    return jobs


def cmd_batch(args: argparse.Namespace) -> int:
    from .families import all_names

    fams = all_names() if args.families == ["all"] else list(args.families)
    agents = list(dict.fromkeys(args.agent))
    if "none" not in agents:
        agents.append("none")
    if not args.no_build:
        build()
    out = args.out
    out.mkdir(parents=True, exist_ok=True)
    jobs = jobs_for(fams, parse_seeds(args.seeds), agents, out, args.order_seed)
    t0 = time.time()
    if args.workers <= 1:
        metas = [run_job(j) for j in jobs]
    else:
        with mp.get_context("spawn").Pool(args.workers) as pool:
            metas = list(pool.imap_unordered(run_job, jobs, chunksize=1))
    metas.sort(key=lambda m: m["name"])
    idx = out / "episodes_v3.json"
    old = json.loads(idx.read_text()) if idx.exists() else []
    keep = {m["name"]: m for m in old}
    keep.update({m["name"]: m for m in metas})
    idx.write_text(json.dumps(sorted(keep.values(), key=lambda m: m["name"]), indent=1, sort_keys=True))
    bad = [m for m in metas if m["class"] == "harness_error"]
    print(json.dumps({"jobs": len(jobs), "harness_errors": len(bad), "wall_s": round(time.time() - t0, 1),
                      "workers": args.workers, "out": str(out)}))
    for m in bad[:5]:
        print("harness error:", m["name"], m.get("errors"), file=sys.stderr)
    return 0 if not bad else 1


def cmd_report(args: argparse.Namespace) -> int:
    from .report import write

    j, md = write(args.dir, workers=args.workers)
    print(md.read_text())
    return 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    b = sub.add_parser("batch")
    b.add_argument("--families", nargs="+", default=["all"])
    b.add_argument("--seeds", nargs="+", default=["5000-5009"])
    b.add_argument("--agent", nargs="+", default=["none", "random-valid", "oracle-v3"])
    b.add_argument("--workers", type=int, default=max(1, (os.cpu_count() or 2) // 2))
    b.add_argument("--out", type=Path, default=REPO / "eval" / "results" / "headless" / "v3-dev")
    b.add_argument("--order-seed", type=int, default=0)
    b.add_argument("--no-build", action="store_true")
    r = sub.add_parser("report")
    r.add_argument("dir", type=Path)
    r.add_argument("--workers", type=int, default=max(1, (os.cpu_count() or 2) // 2))
    args = ap.parse_args(argv)
    return cmd_batch(args) if args.cmd == "batch" else cmd_report(args)


if __name__ == "__main__":
    sys.exit(main())
