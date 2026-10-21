"""N headless episodes across cores, one JSON record each, and the aggregate (docs/HEADLESS.md,
section 10). MODEL RESULTS from a scripted operator on scripted or generated situations: never
human data.

    .venv/bin/python -m eval.headless.batch --situation harbor-watch --seeds 1-10 --workers 10
    .venv/bin/python -m eval.headless.batch --families fast-unknown decoy --seeds 1-30 --workers 20
    .venv/bin/python -m eval.headless.batch --situation harbor-watch --seeds 1-200 --workers 14 --speed
    MURMUR_EVAL_MODEL=1 .venv/bin/python -m eval.headless.batch --situation harbor-watch \
        --families fast-unknown decoy --seeds 1-3 --agent model playbook none --workers 8

Each episode runs in its own `murmur-headless` process (target/release, built first). Records go
to `eval/results/headless/<batch>/` (one `<situation>-s<seed>.json` each, `<situation>-<agent>-s<seed>.json`
for an agent other than the playbook, the audit log beside it with `--keep-audit`), the aggregate
to `aggregate.json`: per situation or family and agent (never pooling tuning and held-out
families, nor agents), each metric's mean with a Student t 95 % interval clustered by seed and,
for proportions, the pooled Wilson interval (eval/stats.py), and the speed: episodes per
core-hour (episodes / (wall hours x workers)) beside the single-worker rate. With more than one
agent, `paired` puts each family and seed's metrics side by side, agent by agent.

`--agent model` (officer.py) calls the Anthropic API and costs money: it needs
MURMUR_EVAL_MODEL=1; the batch's workers share one spend ledger (`model-ledger.json` in the
batch's directory unless MURMUR_EVAL_LEDGER names one) capped at MURMUR_EVAL_BATCH_MAX_USD
(default 20.00), and each episode is capped at MURMUR_EVAL_MAX_USD (default 3.00). An episode
whose wake the guard refused is `budget_stopped`, one whose model call the API refused as such
(bad parameters, credentials) `model_failed`: both listed apart and left out of every metric.
Model episodes wait on the network, not the CPU, so more workers than cores are fine (the
ledger's file lock keeps the shared cap).
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
    agent = job.get("agent") or "playbook"
    with tempfile.TemporaryDirectory(prefix="hl-", dir=job.get("tmp")) as td:
        wd = Path(td)
        if job.get("family"):
            spec, contacts = generate(job["family"], job["situation_seed"], wd)
        else:
            spec, contacts = from_situation(job["situation"]), None
        opts = Options(agent=agent, operator=job.get("operator"), calibration=calib, anchors=job.get("anchors", True),
                       model=job.get("model"))
        try:
            rec = Episode(spec, job["seed"], wd, opts, contacts_path=contacts).run()
        except Exception as e:  # noqa: BLE001 (a failed episode is a record too)
            return {"failed": f"{type(e).__name__}: {e}", "job": job, "wall_s": round(time.perf_counter() - t0, 3)}
        rec["metrics"] = episode_metrics(rec)
        out_dir = Path(job["out"])
        name = job_name(job)
        if job.get("keep_audit"):
            (out_dir / f"{name}.audit.jsonl").write_text(rec["audit_text"])
        rec.pop("audit_text", None)
        (out_dir / f"{name}.json").write_text(json.dumps(rec, sort_keys=True))
    out = {"name": name, "metrics": rec["metrics"], "perf": rec["perf"], "situation": rec["situation"],
           "seed": rec["seed"], "wall_s": round(time.perf_counter() - t0, 3)}
    if agent != "playbook":
        out["agent"] = agent
    if agent == "model":
        out["model"] = (rec.get("model") or {}).get("totals")
        out["budget_stopped"] = bool(rec.get("budget_stopped"))
        out["model_failed"] = bool(rec.get("model_failed"))
    return out


def job_name(job: dict[str, Any]) -> str:
    base = job.get("family") or job["situation"]
    agent = job.get("agent") or "playbook"
    return f"{base}-s{job['seed']}" if agent == "playbook" else f"{base}-{agent}-s{job['seed']}"


def group_key(r: dict[str, Any]) -> str:
    """The aggregate's group: the situation or family (and split), and the agent when it is
    not the playbook (a playbook batch keeps its keys)."""
    s = r["situation"]
    key = f"{s.get('family')} ({s.get('split')})" if s.get("family") else s["name"]
    agent = r.get("agent") or "playbook"
    return key if agent == "playbook" else f"{key} [{agent}]"


def model_costs(rs: list[dict[str, Any]]) -> dict[str, Any]:
    """What the model episodes of a group cost and did: wakes, model and tool calls, tokens and
    US dollars per episode (mean, total), and the guard's stops."""
    ms = [r["model"] for r in rs if r.get("model")]
    if not ms:
        return {}

    def mean(xs: list[float]) -> float | None:
        return round(statistics.fmean(xs), 4) if xs else None

    usd = [float(m.get("usd") or 0.0) for m in ms]
    toks = {k: [float((m.get("tokens") or {}).get(k, 0)) for m in ms] for k in ("input", "cache_write", "cache_read", "output")}
    stops: dict[str, int] = {}
    for m in ms:
        for k, n in (m.get("stops") or {}).items():
            stops[k] = stops.get(k, 0) + int(n)
    return {
        "episodes": len(ms), "usd_total": round(sum(usd), 4), "usd_per_episode": mean(usd),
        "usd_max": round(max(usd), 4),
        "wakes_per_episode": mean([float(m.get("wakes") or 0) for m in ms]),
        "consults_per_episode": mean([float(m.get("consults") or 0) for m in ms]),
        "api_calls_per_episode": mean([float(m.get("api_calls") or 0) for m in ms]),
        "tool_calls_per_episode": mean([float(m.get("tool_calls") or 0) for m in ms]),
        "tokens_per_episode": {k: mean(v) for k, v in toks.items()},
        "model_wall_s_per_episode": mean([float(m.get("wall_s") or 0.0) for m in ms]),
        "stops": dict(sorted(stops.items())),
        "budget_stopped": sum(1 for m in ms if m.get("budget_stopped")),
        "model_failed": sum(1 for m in ms if m.get("model_failed")),
        # episodes kept in the metrics although a wake's model call failed (and was told again)
        "with_errors": sum(1 for m in ms if m.get("errors")),
    }


def excluded(r: dict[str, Any]) -> str | None:
    """Why an episode is not the model's run to judge: the cost guard refused a call
    (`budget_stopped`) or the API refused one as such (`model_failed`)."""
    if r.get("budget_stopped"):
        return "budget_stopped"
    if r.get("model_failed"):
        return "model_failed"
    return None


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
    """Per group, the metrics over its episodes. An episode whose model wake the cost guard
    refused (`budget_stopped`), or whose model call the API refused as such (`model_failed`), is
    left out of every metric and listed apart under its reason, with its cost counted: neither
    is the model choosing to do nothing."""
    groups: dict[str, list[dict[str, Any]]] = {}
    stopped: dict[str, list[dict[str, Any]]] = {}
    for r in results:
        if "failed" in r:
            continue
        (stopped if excluded(r) else groups).setdefault(group_key(r), []).append(r)
    out: dict[str, Any] = {}
    for key in sorted(set(groups) | set(stopped)):
        rs = groups.get(key, [])
        cut = stopped.get(key, [])
        m = [r["metrics"] for r in rs]
        cov = [x["look_covered"] for x in m if x["look_covered"] is not None]
        ttl = [v for x in m for v in x["time_to_look"].values() if v is not None]
        fl = [x["false_looks"] for x in m]
        pk = sum(x["precision"]["legit"] for x in m)
        pn = sum(x["precision"]["n"] for x in m)
        hc = [x["human_cards"] for x in m]
        hpm = [float(x["human_cards_per_operator_min"]) for x in m if x.get("human_cards_per_operator_min") is not None]
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
            # scripted link cuts (link-cut-mid-look): what each cut fell on, episodes counted
            "faults": {k: sum((x.get("faults") or {}).get(k, 0) for x in m)
                       for k in ("link_cuts", "look_in_progress_cut", "no_look_to_cut", "recovering_before_cut")},
            "human_cards_per_operator_min": {
                "mean": round(statistics.fmean(hpm), 4) if hpm else None, "t95": t_interval(hpm)},
            "by_play": _sum_counts([x.get("by_play") or {} for x in m]),
            "unjudged": sorted(u for x in m for u in (x.get("precision") or {}).get("unjudged") or []),
            "perf": perf_breakdown(rs),
        }
        notes = {"budget_stopped": "a wake the cost guard refused: left out of every metric above",
                 "model_failed": "a model call the API refused as such: left out of every metric above"}
        for why, note in notes.items():
            names = sorted(r["name"] for r in cut if excluded(r) == why)
            if names:
                out[key][why] = {"episodes": len(names), "names": names, "note": note}
        costs = model_costs(rs + cut)
        if costs:
            out[key]["model_cost"] = costs
    return out


def _sum_counts(xs: list[dict[str, int]]) -> dict[str, int]:
    out: dict[str, int] = {}
    for x in xs:
        for k, n in x.items():
            out[k] = out.get(k, 0) + int(n)
    return dict(sorted(out.items()))


def paired(results: list[dict[str, Any]]) -> dict[str, Any]:
    """Each family (or situation) and seed, agent by agent: the per-episode metrics side by
    side (the same frozen situation and seed for every agent; a budget-stopped episode shows as
    such, without metrics)."""
    out: dict[str, dict[str, dict[str, Any]]] = {}
    for r in results:
        if "failed" in r:
            continue
        s = r["situation"]
        base = f"{s.get('family')} ({s.get('split')})" if s.get("family") else s["name"]
        agent = r.get("agent") or "playbook"
        cell: dict[str, Any]
        if excluded(r):
            cell = {excluded(r): True}
        else:
            x = r["metrics"]
            cell = {"look_covered": x["look_covered"], "time_to_look": x["time_to_look"], "false_looks": x["false_looks"],
                    "precision": [x["precision"]["legit"], x["precision"]["n"]], "human_cards": x["human_cards"],
                    "human_cards_per_operator_min": x["human_cards_per_operator_min"], "by_play": x.get("by_play")}
        if r.get("model"):
            cell["usd"] = r["model"].get("usd")
            cell["wakes"] = r["model"].get("wakes")
            if r["model"].get("errors"):
                cell["model_errors"] = r["model"]["errors"]
        out.setdefault(base, {}).setdefault(str(r["seed"]), {})[agent] = cell
    return {k: dict(sorted(v.items(), key=lambda kv: int(kv[0]))) for k, v in sorted(out.items())}


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--situation", default=None, help="a situation file or name (sim/situations); with "
                    "--families, run beside them (default harbor-watch when no family is given)")
    ap.add_argument("--families", nargs="*", default=None, help="generated families (eval/headless/split.toml)")
    ap.add_argument("--agent", nargs="+", default=["playbook"], choices=["playbook", "model", "none"],
                    help="the AI: the playbook (default), the model watch officer (officer.py: the Anthropic "
                         "API, needs MURMUR_EVAL_MODEL=1), none; several run each situation and seed once per agent")
    ap.add_argument("--model-config", default=None,
                    help="model agent overrides as JSON (officer.ModelConfig: model, effort, strong_model, "
                         "strong_effort, max_tokens, max_usd, fallbacks, escalation)")
    ap.add_argument("--seeds", nargs="+", default=["1-10"])
    ap.add_argument("--workers", type=int, default=max(1, (os.cpu_count() or 2) // 2))
    ap.add_argument("--operator", default=None, help="operator model as JSON (operator.py); default the situation's")
    ap.add_argument("--out", type=Path, default=None)
    ap.add_argument("--keep-audit", action="store_true")
    ap.add_argument("--no-anchors", action="store_true")
    ap.add_argument("--no-build", action="store_true")
    ap.add_argument("--speed", action="store_true", help="also report episodes per core-hour (G1b)")
    args = ap.parse_args(argv)
    agents = list(dict.fromkeys(args.agent))
    if "model" in agents and not os.environ.get("MURMUR_EVAL_MODEL"):
        raise SystemExit("--agent model calls the Anthropic API: set MURMUR_EVAL_MODEL=1 to allow it (never in tests)")
    model_cfg = json.loads(args.model_config) if args.model_config else None
    if not args.no_build:
        build()
    seeds = parse_seeds(args.seeds)
    batch = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    out = args.out or RESULTS / batch
    out.mkdir(parents=True, exist_ok=True)
    if "model" in agents:
        # one spend ledger for the whole batch, every worker under its file lock
        os.environ.setdefault("MURMUR_EVAL_LEDGER", str(out / "model-ledger.json"))
    calib = Calibration.load().as_dict()
    operator = json.loads(args.operator) if args.operator else None
    base = []
    for fam in args.families or []:
        for s in seeds:
            base.append({"family": fam, "situation_seed": s, "seed": s})
    if args.situation or not args.families:
        for s in seeds:
            base.append({"situation": args.situation or "harbor-watch", "seed": s})
    # every situation and seed once per agent, agents interleaved (a capped batch stops evenly)
    jobs = [{**b, "agent": a} for b in base for a in agents]
    for j in jobs:
        j.update(calibration=calib, operator=operator, out=str(out), keep_audit=args.keep_audit,
                 anchors=not args.no_anchors, model=model_cfg if j["agent"] == "model" else None)
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
        "agents": agents, "groups": aggregate(results),
        "speed": {
            "episodes_per_core_hour": round(len(done) / (wall / 3600.0 * args.workers), 1) if wall > 0 else None,
            "episode_wall_s_mean": round(statistics.fmean([r["wall_s"] for r in done]), 3) if done else None,
            "episode_wall_s_p95": round(sorted(r["wall_s"] for r in done)[int(0.95 * (len(done) - 1))], 3) if done else None,
            "single_worker_rate_per_hour": round(3600.0 / statistics.fmean([r["wall_s"] for r in done]), 1) if done else None,
        },
        "caveat": "Model results from a scripted operator on scripted or generated situations: not human data.",
    }
    if len(agents) > 1:
        agg["paired"] = paired(results)
    if "model" in agents:
        stopped = sorted(r["name"] for r in done if r.get("budget_stopped"))
        broken = sorted(r["name"] for r in done if r.get("model_failed"))
        ledger = Path(os.environ["MURMUR_EVAL_LEDGER"])
        agg["model"] = {
            "config": model_cfg, "ledger": str(ledger),
            "ledger_state": json.loads(ledger.read_text()) if ledger.exists() and ledger.read_text().strip() else None,
            "usd_total": round(sum(float((r.get("model") or {}).get("usd") or 0.0) for r in done), 4),
            "episodes": sum(1 for r in done if r.get("model") is not None),
            "budget_stopped": stopped, "model_failed": broken,
            "with_errors": sorted(r["name"] for r in done if (r.get("model") or {}).get("errors")),
            "note": ("the model's output is not deterministic: two runs of one seed can differ in what it "
                     "proposed; the world, the operator and every draw are the same for every agent"),
        }
    (out / "aggregate.json").write_text(json.dumps(agg, indent=1, sort_keys=True))
    print(json.dumps({k: agg[k] for k in ("jobs", "done", "workers", "wall_s", "speed")}, indent=1))
    for k, v in agg["groups"].items():
        print(k, json.dumps({x: y for x, y in v.items() if x not in ("perf", "model_cost")}))
        print("  perf", json.dumps(v["perf"]))
        if v.get("model_cost"):
            print("  cost", json.dumps(v["model_cost"]))
    if agg.get("model"):
        m = agg["model"]
        print(f"model: ${m['usd_total']:.4f} over {m['episodes']} episodes; ledger {m['ledger']}: "
              f"{m['ledger_state']}; budget-stopped: {m['budget_stopped'] or 'none'}; "
              f"failed: {m['model_failed'] or 'none'}")
    if agg["failed"]:
        print(f"{len(agg['failed'])} failed:", [f["failed"] for f in agg["failed"]][:5], file=sys.stderr)
    print(out)
    return 0 if not agg["failed"] else 1


if __name__ == "__main__":
    sys.exit(main())
