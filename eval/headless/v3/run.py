"""A scoring v3 batch and its dev check (eval/SCORING-V3.md sections 8 and 10). MODEL RESULTS ONLY:
agents on generated situations, never human data. The free agents are deterministic and never call
an API; the model watch officer (`--agent model`) calls the Anthropic API and costs money.

    .venv/bin/python -m eval.headless.v3.run batch --families all --seeds 5000-5009 \\
        --agent none random-valid oracle-v3-shown --workers 20 --out <dir>
    .venv/bin/python -m eval.headless.v3.run report <dir>
    MURMUR_EVAL_MODEL=1 .venv/bin/python -m eval.headless.v3.run batch --families v3-f1-fast-boat-gg \\
        --seeds 6600-6601 --agent model --model-config '{"max_usd": 5}' --max-usd 15 --workers 2 --out <dir>

**batch** runs every (family, situation seed, agent) once (replicate 0; its seed
sha256("{family}:{situation_seed}:0") mod 2^31, as v2's), the approve-all operator, the paused
clock, the picture sampler on (`Options.scoring = "v3"`), each record gzipped into the directory
with its index (`episodes_v3.json`); an episode that fails is run again once, a second failure is a
`harness_error`. A `none` replicate is added for every situation when the agents lack one (labels
judge `useful` against it). **report** labels every episode against its `none` replicate (labels.py),
and writes `dev_v3.json` and `dev_v3.md`: per family and world, the coverage, need served per agent
(pooled over the family's needs) with time to serve, 10.1's room test (the ceiling, `oracle-v3-shown`,
measurably above `none`), and the single-cause analysis of the needs `none` and the oracle lost
(`causes.py`).

**The model arm** (SCORING-V3.md 17.8; officer.py, the shipped watch officer's design through the
API: Sonnet 5.5 routine at medium effort, Opus 5.5 consulted at high): `--agent model` needs
MURMUR_EVAL_MODEL=1 and `--max-usd`, the batch's hard cap in US dollars, kept in the batch
directory's spend ledger (`model-ledger.json`, shared by every worker under its file lock, and by a
later batch into the same directory); each episode is capped too (`max_usd` in `--model-config`,
default 3.00). It runs only on dev seeds (5000-5499, a smoke) and the model pilot's (6600-6699),
never dev-validate or test, on the paused clock (v2's primary condition), with the leakage scan of
every input (v2/leakage.py). A model episode the cost guard stopped (`budget_stop`), whose call the
API refused as such (`model_failed`) or whose input hit the scan (`harness_error`, after one rerun)
is listed apart in the report and left out of every metric; its index entry carries the officer's
totals (`model`: dollars, tokens, wakes, errors, refusals and their categories). A free batch is as
it was.
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
#: the situation seeds a model episode may use (SCORING-V3.md 0.1): dev's, for a smoke, and the
#: model pilot's; never dev-validate's (5500-5999), test's (from 7000) or v2's
MODEL_SEEDS = (range(5000, 5500), range(6600, 6700))
LEDGER = "model-ledger.json"


def job_name(job: dict[str, Any]) -> str:
    return f"{job['family']}-{job['agent']}-s{job['situation_seed']}-r{job['r']}"


def _episode(job: dict[str, Any], wd: Path) -> dict[str, Any]:
    from ..episode import Episode, Options
    from .families import generate

    spec, contacts = generate(job["family"], job["situation_seed"], wd)
    spec.seed = int(job["seed"])
    spec.replicate = int(job["r"])
    # a family's own scripted operators and effects (F15, F16: families_ops.py `set_options`)
    extra = dict(getattr(spec, "options", None) or {})
    opts = Options(agent=job["agent"], operator=job.get("operator"), calibration=Calibration(**job["calibration"]),
                   scoring="v3", replicate={"r": job["r"], "seed": job["seed"], "situation_seed": job["situation_seed"]},
                   model=job.get("model"), **extra)
    return Episode(spec, job["seed"], wd, opts, contacts_path=contacts).run()


def run_job(job: dict[str, Any]) -> dict[str, Any]:
    """One episode, run again once if it failed or (the model) its leakage scan hit an input; the
    record gzipped; its index entry."""
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
        if rec.get("episode_class") == "harness_error" and attempt == 1:
            # the model's leakage scan hit an input (v2's rule, SCORING-V2.md 7.1): run again once on
            # the same seeds; a second hit is scored as the class says
            hits = ((rec.get("v3") or {}).get("leakage") or {}).get("hits") or []
            errors.append(f"leakage: {len(hits)} hits: {json.dumps(hits[:3])[:400]}")
            continue
        fid = fidelity_digest({**rec, "v2": rec.get("v3")})
        rec.pop("audit_text", None)
        if errors:
            rec["harness_reruns"] = errors
        path = out / f"{name}.json.gz"
        size = dump(rec, path)
        meta = {"name": name, "path": str(path), "family": job["family"], "agent": job["agent"],
                "situation_seed": job["situation_seed"], "r": job["r"], "seed": job["seed"], "class": rec.get("episode_class"),
                "attempts": attempt, "record_bytes": size, "wall_s": round(time.perf_counter() - t0, 3),
                "fidelity": fid, "reruns": errors}
        if job["agent"] == "model":
            # the officer's totals (dollars, tokens, wakes, errors, refusals), whatever the class
            meta.update(model=(rec.get("model") or {}).get("totals"), budget_stopped=bool(rec.get("budget_stopped")),
                        model_failed=bool(rec.get("model_failed")),
                        leakage_hits=int(((rec.get("v3") or {}).get("leakage") or {}).get("n_hits") or 0))
        return meta
    return {"name": name, "path": None, "family": job["family"], "agent": job["agent"],
            "situation_seed": job["situation_seed"], "r": job["r"], "seed": job["seed"], "class": "harness_error",
            "attempts": 2, "errors": errors, "wall_s": round(time.perf_counter() - t0, 3)}


def jobs_for(families: list[str], seeds: list[int], agents: list[str], out: Path, order_seed: int = 0,
             model: dict[str, Any] | None = None) -> list[dict[str, Any]]:
    calib = Calibration.load().as_dict()
    jobs = []
    for fam in families:
        for s in seeds:
            seed = replicate_seed(fam, s, 0)
            for a in agents:
                jobs.append({"family": fam, "situation_seed": s, "r": 0, "seed": seed, "agent": a, "operator": APPROVE_ALL,
                             "calibration": calib, "out": str(out)})
                if a == "model":
                    jobs[-1]["model"] = model
    random.Random(order_seed).shuffle(jobs)
    return jobs


def model_config(args: argparse.Namespace, seeds: list[int], out: Path) -> dict[str, Any]:
    """The model arm's gate and its configuration (officer.ModelConfig's names): MURMUR_EVAL_MODEL=1,
    a batch cap (`--max-usd`) kept in the directory's ledger, the model's seeds only, and no refusal
    fallback; checked whole (`ModelConfig.resolve`) before any episode starts."""
    from ..officer import ModelArmError, ModelConfig

    if not os.environ.get("MURMUR_EVAL_MODEL"):
        raise SystemExit("--agent model calls the Anthropic API: set MURMUR_EVAL_MODEL=1 to allow it (never in tests)")
    if args.max_usd is None or not args.max_usd > 0:
        raise SystemExit("--agent model needs --max-usd: the batch's hard cap in US dollars")
    bad = [s for s in seeds if not any(s in r for r in MODEL_SEEDS)]
    if bad:
        raise SystemExit(f"--agent model runs on dev seeds (5000-5499) and the model pilot's (6600-6699) only, "
                         f"never dev-validate or test (SCORING-V3.md 0.1): not {bad[:5]}")
    given = json.loads(args.model_config) if args.model_config else {}
    if not isinstance(given, dict):
        raise SystemExit("--model-config must be a JSON object (officer.ModelConfig's names)")
    for k in ("ledger", "batch_max_usd"):
        if k in given:
            raise SystemExit(f"--model-config may not set {k}: the batch's cap is --max-usd, its ledger the batch's")
    cfg = {**given, "ledger": str(out / LEDGER), "batch_max_usd": float(args.max_usd)}
    try:
        ModelConfig.resolve(cfg)
    except ModelArmError as e:
        raise SystemExit(f"--model-config: {e}") from e
    return cfg


def cmd_batch(args: argparse.Namespace) -> int:
    from .families import all_names

    fams = all_names() if args.families == ["all"] else list(args.families)
    agents = list(dict.fromkeys(args.agent))
    if "none" not in agents:
        agents.append("none")
    seeds = parse_seeds(args.seeds)
    out = args.out
    model = model_config(args, seeds, out) if "model" in agents else None
    if not args.no_build:
        build()
    out.mkdir(parents=True, exist_ok=True)
    jobs = jobs_for(fams, seeds, agents, out, args.order_seed, model)
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
    if model is not None:
        ms = [m for m in metas if m["agent"] == "model"]
        ledger = out / LEDGER
        state = json.loads(ledger.read_text()) if ledger.exists() and ledger.read_text().strip() else None
        print(f"model: ${sum(float((m.get('model') or {}).get('usd') or 0.0) for m in ms):.4f} over {len(ms)} episodes; "
              f"ledger {ledger}: {state}; budget-stopped: {sorted(m['name'] for m in ms if m.get('budget_stopped')) or 'none'}; "
              f"failed: {sorted(m['name'] for m in ms if m.get('model_failed')) or 'none'}")
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
    b.add_argument("--agent", nargs="+", default=["none", "random-valid", "oracle-v3-shown"])
    b.add_argument("--workers", type=int, default=max(1, (os.cpu_count() or 2) // 2))
    b.add_argument("--out", type=Path, default=REPO / "eval" / "results" / "headless" / "v3-dev")
    b.add_argument("--order-seed", type=int, default=0)
    b.add_argument("--no-build", action="store_true")
    b.add_argument("--max-usd", type=float, default=None,
                   help="--agent model: the batch's hard cap in US dollars (required; the directory's ledger keeps it)")
    b.add_argument("--model-config", default=None,
                   help="--agent model: overrides as JSON (officer.ModelConfig: model, effort, strong_model, strong_effort, "
                        "max_tokens, max_usd per episode, escalation)")
    r = sub.add_parser("report")
    r.add_argument("dir", type=Path)
    r.add_argument("--workers", type=int, default=max(1, (os.cpu_count() or 2) // 2))
    args = ap.parse_args(argv)
    return cmd_batch(args) if args.cmd == "batch" else cmd_report(args)


if __name__ == "__main__":
    sys.exit(main())
