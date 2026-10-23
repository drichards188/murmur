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

**Scoring v2** (`--scoring v2`, eval/SCORING-V2.md): every episode runs the picture sampler and
gets its class (section 7); each situation (family, situation seed) runs `--replicates K` times,
replicate r on the seed sha256("{family}:{situation_seed}:{r}") mod 2^31, which drives the link
emulator, the stand-ins, the agent's first tick and the operator's draws (the situation seed
still drives the contacts and the sensing); the jobs run in a random order (`--order-seed`); a
`none` replicate is added for every situation, replicate and condition when the agents lack one
(`useful` is judged against it); `--operators`, `--policy` and `--clocks` (paused, the default,
or charged) add conditions. An episode that fails in the simulator or the driver, or whose model
input hit the leakage scan, is run again once on the same seeds; a second failure is a
`harness_error`. Records go gzipped (`<family>-<agent>-s<seed>-r<r>.json.gz`), their index to
`episodes_v2.json`, the labels' metrics, comparisons, primary endpoint, Holm and multiverse to
`aggregate_v2.json`, and the scorecard to `scorecard.json` and `scorecard.md`
(eval/headless/v2/). Without `--scoring v2` nothing here changes.

Replicate r's episode gets `spec.replicate = r`, so its opaque run id is per (family, situation
seed, replicate) and the same for every agent. The agents include scoring v2's anchors (`null`,
`random-valid`, `oracle`), the ruler check's ladder (`oracle-linkblind`, `oracle-honest`,
`oracle-late`; judged by eval/headless/ladder.py, SCORING-V2.md 0.3), the playbook's
perturbations (`playbook+spam`, `playbook+behind`, `playbook+skip-first`, `playbook+reserve-blind`,
`playbook+ferries`) and the honest oracle's (`oracle-honest+spam`, `+behind`, `+skip-first`,
`+reserve-blind`, `+ferries`, `+pull-off`; SCORING-V2.md 0.5): eval/headless/agents_v2.py.
Each episode's index entry carries its fidelity digests (`fidelity_digest`: truth, picture samples,
the audit log bar `agent.action`), which the world pilot's gate compares between `null` and `none`
(aggregate_v2.json `world_pilot`). The primary pools only the families admitted by an
admissibility verdict (resolution 21): `--admissibility <batch>` takes an admissibility batch's
(none and oracle on the admissibility seeds), else this batch's own none and oracle if both ran;
`primary_families` says which and why. `--reserve <candidate>` (the world's calibration only,
eval/headless/reserve.py) puts PUMA 1 at a committed reserve candidate.

    .venv/bin/python -m eval.headless.batch --scoring v2 --families fast-unknown --seeds 1000-1009 \\
        --replicates 4 --agent playbook none --operators approve-all proxy --workers 8
    .venv/bin/python -m eval.headless.batch --scoring v2 --families v2-fast-unknown v2-decoy \\
        --seeds 1000-1099 --agent random-valid playbook oracle null --operators approve-all --workers 12
"""

from __future__ import annotations

import argparse
import hashlib
import json
import multiprocessing as mp
import os
import random
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


def replicate_seed(family: str, situation_seed: int, r: int) -> int:
    """SCORING-V2.md section 1: replicate r's seed, sha256("{family}:{situation_seed}:{r}") mod 2^31."""
    return int(hashlib.sha256(f"{family}:{situation_seed}:{r}".encode()).hexdigest(), 16) % (2 ** 31)


def parse_seeds(spec: list[str]) -> list[int]:
    out: list[int] = []
    for s in spec:
        if "-" in s:
            a, b = s.split("-", 1)
            out += list(range(int(a), int(b) + 1))
        else:
            out.append(int(s))
    return out


#: docs/HEADLESS.md section 16's options (episode.Options), each off unless given
SECTION16 = ("signers", "review", "scripted", "effects", "space_snapshot")


def build() -> None:
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    subprocess.run([cargo, "build", "--release", "-q", "-p", "murmur-headless"], cwd=REPO, check=True)


def _v2_episode(job: dict[str, Any], wd: Path) -> dict[str, Any]:
    """One scoring v2 episode: the situation from its situation seed, everything else drawn from
    the replicate seed (the operator's draws too), the picture sampler on."""
    from .episode import Episode, Options
    from .families import generate

    if job.get("family"):
        spec, contacts = generate(job["family"], job["situation_seed"], wd, reserve=job.get("reserve"))
    else:
        spec, contacts = from_situation(job["situation"]), None
    spec.seed = int(job["seed"])  # the operator's draws: the replicate's (section 1)
    spec.replicate = int(job["r"])  # the opaque run id: (family, situation seed, replicate) (4.4)
    if job.get("policy"):
        spec.policy = Path(job["policy"])
    opts = Options(agent=job.get("agent") or "playbook", operator=job.get("operator"),
                   calibration=Calibration(**job["calibration"]), anchors=job.get("anchors", True),
                   model=job.get("model"), scoring="v2", clock=job.get("clock") or "paused",
                   replicate={"r": job["r"], "seed": job["seed"], "situation_seed": job["situation_seed"]})
    return Episode(spec, job["seed"], wd, opts, contacts_path=contacts).run()


def normalized_audit(text: str) -> list[dict[str, Any]]:
    """The audit log as the null agent must reproduce `none`'s (SCORING-V2.md 4.5): every event
    but `agent.action` (the bridge's record of an agent's session: its connect, previews,
    proposals), in order, without what those events take a place in: the event counter (`seq`)
    and the hash chain (`prev`, `hash`); and without the binary's build stamp (`run.started`'s
    `build`: the commit it was built from and whether the tree was dirty, which a rebuild between
    two episodes changes and which the record's provenance carries; tests/test_episode.py's
    golden episode leaves it out too)."""
    out = []
    for line in text.splitlines():
        if not line.strip():
            continue
        e = json.loads(line)
        if e.get("type") == "agent.action":
            continue
        out.append({k: v for k, v in e.items() if k not in ("seq", "prev", "hash")
                    and not (k == "build" and e.get("type") == "run.started")})
    return out


def fidelity_digest(rec: dict[str, Any]) -> dict[str, str]:
    """SHA-256 of what a `null` episode must share with its `none` replicate (4.5): the truth
    (the contacts' samples, our vehicles' samples, the tracker's association), the picture
    samples, and the audit log (`normalized_audit`). The labels follow from these."""

    def h(x: Any) -> str:
        return hashlib.sha256(json.dumps(x, sort_keys=True, separators=(",", ":")).encode()).hexdigest()

    truth = rec.get("truth") or {}
    return {"truth": h({k: truth.get(k) for k in ("contacts", "samples", "ours", "association")}),
            "picture": h((rec.get("v2") or {}).get("picture")),
            "audit": h(normalized_audit(rec.get("audit_text") or ""))}


def _job_v2(job: dict[str, Any]) -> dict[str, Any]:
    """A v2 job: the episode, run again once on the same seeds if the simulator or the driver
    failed or the model's leakage scan hit an input (a second failure is a `harness_error`, section
    7); the record gzipped."""
    from .v2.aggregate import meta_of
    from .v2.record import dump

    t0 = time.perf_counter()
    out_dir = Path(job["out"])
    name = job_name(job)
    errors = []
    for attempt in (1, 2):
        with tempfile.TemporaryDirectory(prefix="hl-", dir=job.get("tmp")) as td:
            try:
                rec = _v2_episode(job, Path(td))
            except Exception as e:  # noqa: BLE001 (a failed episode is a harness error, counted)
                errors.append(f"{type(e).__name__}: {e}")
                continue
        if rec.get("episode_class") == "harness_error" and attempt == 1:
            # a leakage hit on an input (section 7.1, 4.4): run again once on the same seeds; the
            # hits are kept in the rerun's `harness_reruns`, a second hit is scored as the class says
            hits = ((rec.get("v2") or {}).get("leakage") or {}).get("hits") or []
            errors.append(f"leakage: {len(hits)} hits: {json.dumps(hits[:3])[:400]}")
            continue
        if job.get("keep_audit"):
            (out_dir / f"{name}.audit.jsonl").write_text(rec["audit_text"])
        fidelity = fidelity_digest(rec)
        rec.pop("audit_text", None)
        if errors:
            rec["harness_reruns"] = errors
        path = out_dir / f"{name}.json.gz"
        size = dump(rec, path)
        meta = meta_of(rec, path)
        meta.update(name=name, wall_s=round(time.perf_counter() - t0, 3), attempts=attempt, record_bytes=size,
                    fidelity=fidelity, reserve=job.get("reserve"),
                    picture_bytes=len(json.dumps(rec["v2"]["picture"])), reruns=errors,
                    budget_stopped=bool(rec.get("budget_stopped")), model_failed=bool(rec.get("model_failed")))
        if rec.get("model"):
            meta["model"] = (rec.get("model") or {}).get("totals")
        return meta
    s = job.get("family") or job["situation"]
    policy = job.get("policy")
    if not policy:  # the situation's own policy, as a record would name it
        try:
            from .families import generate

            with tempfile.TemporaryDirectory(prefix="hl-", dir=job.get("tmp")) as td:
                policy = (generate(job["family"], job["situation_seed"], Path(td))[0] if job.get("family")
                          else from_situation(job["situation"])).policy
        except Exception:  # noqa: BLE001
            policy = None
    return {"path": None, "agent": job.get("agent"), "base": s, "family": job.get("family"),
            "situation": f"{s}:{job['situation_seed']}", "situation_seed": job["situation_seed"], "r": job["r"],
            "seed": job["seed"], "operator": (job.get("operator") or {}).get("mode", "proxy"),
            "policy": Path(policy).name if policy else None, "clock": job.get("clock") or "paused",
            "class": "harness_error", "name": name, "errors": errors, "attempts": 2,
            "wall_s": round(time.perf_counter() - t0, 3)}


def _job(job: dict[str, Any]) -> dict[str, Any]:
    """One episode in a worker: the record (without the audit text, kept beside it on request)."""
    from .episode import Episode, Options
    from .families import generate
    from .metrics import episode_metrics

    if job.get("scoring") == "v2":
        return _job_v2(job)
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
                       model=job.get("model"), **{k: job[k] for k in SECTION16 if job.get(k) is not None})
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
    if job.get("scoring") == "v2":
        cond = f"-{job['condition']}" if job.get("condition") else ""
        return f"{base}-{agent}-s{job['situation_seed']}-r{job['r']}{cond}"
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
    from .episode import AGENTS

    ap.add_argument("--agent", nargs="+", default=["playbook"], choices=list(AGENTS),
                    help="the AI: the playbook (default), the model watch officer (officer.py: the Anthropic "
                         "API, needs MURMUR_EVAL_MODEL=1), none; scoring v2's anchors null, random-valid, oracle "
                         "and the playbook's perturbations (agents_v2.py); several run each situation and seed "
                         "once per agent")
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
    # docs/HEADLESS.md section 16 (v1 only), each off unless given
    ap.add_argument("--signers", default=None, help='the engagement signers as JSON (signers.py: {"preset": ...})')
    ap.add_argument("--review", default=None, help='the reviewer as JSON (review.py: {"preset": ...})')
    ap.add_argument("--scripted", default=None, help="the scripted agent's plan as JSON (scripted.py; --agent scripted)")
    ap.add_argument("--effects", default=None,
                    help='engagement effects as JSON ({"p_hit", "spend_expended", "survives_miss"})')
    ap.add_argument("--space-snapshot", default=None, help="the pinned element sets of this world "
                    "(sim/headless/space/<world>/): the bridge's space segment and the emulator's SATCOM read them")
    ap.add_argument("--scoring", choices=["v1", "v2"], default="v1",
                    help="v2: the picture sampler, replicates, labels against the paired none replicate, "
                         "aggregate_v2.json and the scorecard (eval/SCORING-V2.md); v1 (default): as before")
    ap.add_argument("--replicates", type=int, default=1, help="v2: K replicates per situation (seeds from "
                    "sha256(family:situation_seed:r))")
    ap.add_argument("--order-seed", type=int, default=0, help="v2: the seed of the random run order")
    ap.add_argument("--operators", nargs="*", default=None,
                    help="v2: operator modes, one condition each (proxy: the situation's own autoplay preset)")
    ap.add_argument("--policy", nargs="*", default=None, help="v2: policy files, one condition each "
                    "(default: the situation's own)")
    ap.add_argument("--clocks", nargs="*", default=None, choices=["paused", "charged"],
                    help="v2: clock conditions, one each (default paused; charged: the model's thinking costs "
                         "simulated time, latency.py, SCORING-V2.md 4.2)")
    ap.add_argument("--primary", nargs=2, default=["model", "playbook"], metavar=("A", "B"),
                    help="v2: the primary pair (A minus B on need served; A's replicate 0 only when A is the model)")
    ap.add_argument("--no-multiverse", action="store_true", help="v2: skip the multiverse cells")
    ap.add_argument("--admissibility", type=Path, default=None,
                    help="v2: an admissibility batch (its directory or aggregate_v2.json: none and oracle on the "
                         "admissibility seeds, SCORING-V2.md 9.2) whose verdicts admit families to the primary "
                         "(3.1); without it, this batch's own none and oracle, if both ran")
    ap.add_argument("--reserve", default=None,
                    help="v2 families only, the world's calibration (SCORING-V2.md 0.1): PUMA 1 waits at this "
                         "candidate (eval/headless/reserve_candidates.toml) instead of its spawn")
    args = ap.parse_args(argv)
    if args.scoring == "v2":
        return main_v2(args)
    if args.replicates != 1 or args.operators or args.policy or args.clocks or args.admissibility or args.reserve:
        raise SystemExit("--replicates, --operators, --policy, --clocks, --admissibility and --reserve are scoring "
                         "v2's: add --scoring v2")
    if args.scripted and "scripted" not in args.agent:
        raise SystemExit("--scripted is the scripted agent's plan: add --agent scripted")
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
    section16 = {"signers": json.loads(args.signers) if args.signers else None,
                 "review": json.loads(args.review) if args.review else None,
                 "scripted": json.loads(args.scripted) if args.scripted else None,
                 "effects": json.loads(args.effects) if args.effects else None,
                 "space_snapshot": args.space_snapshot}
    for j in jobs:
        j.update(calibration=calib, operator=operator, out=str(out), keep_audit=args.keep_audit,
                 anchors=not args.no_anchors, model=model_cfg if j["agent"] == "model" else None, **section16)
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


def v2_jobs(args: argparse.Namespace, agents: list[str], calib: dict[str, Any], out: Path) -> list[dict[str, Any]]:
    """Every (situation, replicate, condition, agent) of a v2 batch, in a random order."""
    seeds = parse_seeds(args.seeds)
    bases: list[dict[str, Any]] = []
    for fam in args.families or []:
        bases += [{"family": fam, "situation_seed": s} for s in seeds]
    if args.situation or not args.families:
        bases += [{"situation": args.situation or "harbor-watch", "situation_seed": s} for s in seeds]
    ops: list[tuple[str, dict[str, Any] | None]]
    if args.operators:
        ops = [(m, None if m == "proxy" else {"mode": m}) for m in args.operators]
    else:
        op = json.loads(args.operator) if args.operator else None
        ops = [((op or {}).get("mode", "proxy"), op)]
    pols: list[str | None] = [str(Path(p).resolve()) for p in args.policy] if args.policy else [None]
    clocks: list[str] = list(dict.fromkeys(getattr(args, "clocks", None) or ["paused"]))
    multi = len(ops) * len(pols) > 1
    model_cfg = json.loads(args.model_config) if args.model_config else None
    jobs = []
    for b in bases:
        fam = b.get("family") or b["situation"]
        for r in range(max(1, args.replicates)):
            seed = replicate_seed(fam, b["situation_seed"], r)
            for mode, op in ops:
                for pol, clock in [(p, c) for p in pols for c in clocks]:
                    cond = f"{mode}-{Path(pol).stem}" if multi and pol else (mode if multi else None)
                    if clock != "paused":  # a paused job keeps its name
                        cond = f"{cond}-{clock}" if cond else clock
                    for a in agents:
                        jobs.append({**b, "r": r, "seed": seed, "agent": a, "operator": op, "policy": pol,
                                     "clock": clock, "condition": cond, "scoring": "v2", "calibration": calib,
                                     "out": str(out),
                                     "keep_audit": args.keep_audit, "anchors": not args.no_anchors,
                                     "model": model_cfg if a == "model" else None,
                                     "reserve": getattr(args, "reserve", None) if b.get("family") else None})
    random.Random(args.order_seed).shuffle(jobs)
    return jobs


def main_v2(args: argparse.Namespace) -> int:
    """`--scoring v2`: the episodes, then their labels, metrics, aggregate and scorecard."""
    from .v2.aggregate import aggregate, score
    from .v2.labels import Params
    from .v2.scorecard import write as write_scorecard
    from .v2.scorecard import scorecard

    agents = list(dict.fromkeys(args.agent))
    added_none = "none" not in agents
    if added_none:
        agents.append("none")  # `useful` is judged against the paired none replicate (2.5)
    if "model" in agents and not os.environ.get("MURMUR_EVAL_MODEL"):
        raise SystemExit("--agent model calls the Anthropic API: set MURMUR_EVAL_MODEL=1 to allow it (never in tests)")
    if not args.no_build:
        build()
    batch = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    out = args.out or RESULTS / batch
    out.mkdir(parents=True, exist_ok=True)
    if "model" in agents:
        os.environ.setdefault("MURMUR_EVAL_LEDGER", str(out / "model-ledger.json"))
    calib = Calibration.load().as_dict()
    jobs = v2_jobs(args, agents, calib, out)
    t0 = time.time()
    if args.workers <= 1:
        metas = [_job(j) for j in jobs]
    else:
        with mp.get_context("spawn").Pool(args.workers) as pool:
            metas = list(pool.imap_unordered(_job, jobs, chunksize=1))
    wall = time.time() - t0
    metas.sort(key=lambda m: m.get("name") or "")
    (out / "episodes_v2.json").write_text(json.dumps(metas, indent=1, sort_keys=True))
    t1 = time.time()
    scored = score(metas, Params(), multiverse=not args.no_multiverse)
    admissibility = None
    if args.admissibility is not None:
        p = args.admissibility / "aggregate_v2.json" if args.admissibility.is_dir() else args.admissibility
        admissibility = {"source": str(p), "verdicts": json.loads(p.read_text())["admissibility"]}
    agg = aggregate(scored, primary=tuple(args.primary), admissible=admissibility)  # type: ignore[arg-type]
    done = [m for m in metas if m.get("path")]
    sizes = [m["record_bytes"] for m in done if m.get("record_bytes")]
    pics = [m["picture_bytes"] for m in done if m.get("picture_bytes")]
    agg.update({
        "batch": batch, "jobs": len(jobs), "done": len(done), "workers": args.workers, "wall_s": round(wall, 3),
        "scoring_s": round(time.time() - t1, 3), "calibration": calib, "agents": agents,
        "none_added_for_pairing": added_none, "replicates": args.replicates, "order_seed": args.order_seed,
        "harness_errors": [m for m in metas if m.get("class") == "harness_error"],
        "reruns": sum(1 for m in metas if m.get("reruns")),
        "record_bytes": {"mean": round(statistics.fmean(sizes)) if sizes else None, "max": max(sizes) if sizes else None,
                         "gzipped": True},
        "picture_bytes": {"mean": round(statistics.fmean(pics)) if pics else None, "max": max(pics) if pics else None,
                          "gzipped": False},
    })
    (out / "aggregate_v2.json").write_text(json.dumps(agg, indent=1, sort_keys=True, default=str))
    card = scorecard(scored)
    j, md = write_scorecard(card, out)
    print(json.dumps({k: agg[k] for k in ("jobs", "done", "workers", "wall_s", "scoring_s", "record_bytes",
                                         "picture_bytes", "reruns")}, indent=1))
    print(md.read_text())
    if agg["harness_errors"]:
        print(f"{len(agg['harness_errors'])} harness errors:", [m.get("errors") for m in agg["harness_errors"]][:5],
              file=sys.stderr)
    print(out)
    return 0 if not agg["harness_errors"] else 1


if __name__ == "__main__":
    sys.exit(main())
