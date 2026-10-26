"""Scoring v3's ruler: the dev batches of the ceiling, its ladder and the honest ceiling's perturbations,
and their report (eval/SCORING-V3.md sections 10, 13 and 14; bd murmur-1z6.7). MODEL RESULTS ONLY: free,
deterministic agents on generated situations; never human data, never an API call.

    .venv/bin/python -m eval.headless.v3.ruler batch --seeds 5160-5229 --workers 8 --out <dir>
    .venv/bin/python -m eval.headless.v3.run report <dir> --workers 8      # labels: dev_v3.json
    .venv/bin/python -m eval.headless.v3.ruler report <dir> [--prev <dir of an earlier batch>]

**batch**: every family of F1-F6 on both worlds (`FAMILIES`), every agent of `AGENTS`, one replicate
(run.py's jobs and episodes, unchanged), resumable: a job whose record is in the directory is not
run again, and the index (`episodes_v3.json`) is written after every `CHUNK` jobs, so a batch
stopped part way loses at most a chunk. Its workers run numpy on one thread each.

**report** reads the batch's `dev_v3.json` (run.py `report`) and writes `ruler_v3.json` and
`ruler_v3.md`, opening with the batch's coverage (lesson 1):

1. the room test per family and world (room.py; SCORING-V3.md 10.1);
2. lesson 3's single-cause share per family and world, for `none` and the ceiling (`none`'s
   `no_look` apart, choice 13; report.py `flags`);
3. the ceiling's losses by cause: its plan's feasibility verdict at its first tick (planner.py
   `feasibility`: `fleet_gap`, `night`, `fog`, `no_time`, `terrain`, `link`, `reserve`) for the
   needs no plan could serve, else the loss's cause on the picture (causes.py) for the needs its
   plan could have served;
4. the ladder per family and world, and pooled per family: each rung's need served, the judged
   orders (`JUDGED`) by v2 0.3's rule and the reported ones (`REPORTED`), with their paired
   differences;
5. each perturbation's moves against the honest ceiling (`MOVES`), pooled over the family-worlds
   with eligible needs (the stratified bootstrap, v2 0.5 a), and per family-world;
6. the per-family guard (lesson 4, SCORING-V3.md 10.5): within each family, every judged cell of
   either world (the ladder's judged orders, the perturbations' judged moves) tested for leaning the
   wrong way (a one-sided sign-flip permutation test), Holm's correction across the family's cells;
7. with `--prev`, the audit (lesson 2, SCORING-V3.md 10.3): every judged cell (the room test, the
   ladder's judged orders, the moves pooled and per family-world) whose point estimate leans the
   wrong way in both batches; with `--halves`, in both halves of this batch (the owner's decision of
   2026-10-25: one dev batch, its seeds split in two).
"""

from __future__ import annotations

import argparse
import itertools
import json
import multiprocessing as mp
import os
import statistics
import sys
import time
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

import numpy as np

from .. import REPO
from ..v2 import metrics as V2M
from ..v2 import stats as S
from .room import MIN_ROOM, room

FAMILIES = tuple(f"v3-{f}-{w}" for f in ("f1-fast-boat", "f2-drone", "f3-low-aircraft", "f4-uuv", "f5-diver",
                                         "f6-shore-vehicle") for w in ("gg", "sd"))
CEILING = "oracle-v3"
HONEST = "oracle-v3-honest"
RUNGS = ("oracle-v3", "oracle-v3-late", "oracle-v3-linkblind", "oracle-v3-sensorblind", "oracle-v3-honest")
PERTURBATIONS = ("oracle-v3-honest+spam", "oracle-v3-honest+behind", "oracle-v3-honest+skip-first",
                 "oracle-v3-honest+reserve-blind", "oracle-v3-honest+ferries", "oracle-v3-honest+pull-off",
                 "oracle-v3-honest+wrong-sensor")
AGENTS = ("none", "random-valid", "playbook") + RUNGS + PERTURBATIONS
CHUNK = 200
#: SCORING-V3.md 13.3: the orders judged, (A, B): A at least B on need served. Each removes one
#: advantage from the same planner, with no second handicap to interact with
JUDGED: tuple[tuple[str, str], ...] = (("oracle-v3", "oracle-v3-late"), ("oracle-v3", "oracle-v3-linkblind"),
                                       ("oracle-v3", "oracle-v3-sensorblind"))
#: computed and reported, never judged: two handicaps that may interact (the truth step, as v2 0.6
#: found), or a rung against the floor (an agent that moves vehicles can take one off a need the
#: floor's standing orders would have served)
REPORTED: tuple[tuple[str, str], ...] = (("oracle-v3-linkblind", "oracle-v3-honest"),
                                         ("oracle-v3-sensorblind", "oracle-v3-honest"),
                                         ("oracle-v3-late", "oracle-v3-honest"),
                                         ("oracle-v3-late", "none"), ("oracle-v3-linkblind", "none"),
                                         ("oracle-v3-sensorblind", "none"), ("oracle-v3-honest", "none"))
#: SCORING-V3.md 13.4: each perturbation's predicted moves against the honest ceiling (metric, want);
#: `not worse`: the pooled interval's lower bound above -`NOT_WORSE` (non-inferiority, v2 0.5 b)
MOVES: dict[str, tuple[tuple[str, str], ...]] = {
    "oracle-v3-honest+spam": (("served_share", "not worse"), ("per_hour.follow_up", "up"), ("repeats", "up"),
                              ("cards_per_10min", "up")),
    "oracle-v3-honest+behind": (("served_share", "down"), ("per_hour.useless", "up")),
    "oracle-v3-honest+skip-first": (("served_share", "down"),),
    "oracle-v3-honest+reserve-blind": (("fault_met_share", "down"),),
    "oracle-v3-honest+ferries": (("per_hour.useless", "up"), ("bad_card_share", "up")),
    "oracle-v3-honest+pull-off": (("served_share", "down"), ("per_hour.harmful_b", "up")),
    # useless as a share (v2 0.5 c): the honest ceiling looks at traffic it cannot tell from a need
    # (about 36 looks an episode on the smoke seed 5100, most useless), and wrong-sensor drops the
    # looks no wrong vehicle can take, so its useless looks per hour fall while every look it sends
    # is useless (seen on the smoke seed, before any run on the dev seeds)
    "oracle-v3-honest+wrong-sensor": (("served_share", "down"), ("useless_share", "up")),
}
NOT_WORSE = 0.05
#: lesson 4: the guard's level, Holm across each family's cells
GUARD_ALPHA = 0.05


# ---- the batch ---------------------------------------------------------------------------------------

def _one_thread() -> None:
    for k in ("OPENBLAS_NUM_THREADS", "OMP_NUM_THREADS", "MKL_NUM_THREADS", "NUMEXPR_NUM_THREADS"):
        os.environ[k] = "1"


def recover(out: Path) -> int:
    """A batch stopped part way leaves records its index does not hold (the chunk in flight): each is
    read back into the index from the record itself (its fidelity digest and wall time are not
    kept), and one that cannot be read (cut off while written) is removed, to be run again."""
    import gzip

    idx = out / "episodes_v3.json"
    old = json.loads(idx.read_text()) if idx.exists() else []
    known = {m["name"] for m in old}
    added = 0
    for p in sorted(out.glob("*.json.gz")):
        name = p.name[: -len(".json.gz")]
        if name in known:
            continue
        try:
            rec = json.loads(gzip.decompress(p.read_bytes()))
        except Exception:  # noqa: BLE001 (cut off while written: run it again)
            p.unlink()
            continue
        rep = rec.get("replicate") or {}
        agent = name.rsplit("-s", 1)[0]
        fam = next(f for f in FAMILIES if agent.startswith(f + "-"))
        old.append({"name": name, "path": str(p), "family": fam, "agent": agent[len(fam) + 1:],
                    "situation_seed": int(rep.get("situation_seed")), "r": int(rep.get("r") or 0),
                    "seed": int(rep.get("seed")), "class": rec.get("episode_class"), "attempts": 1,
                    "record_bytes": p.stat().st_size, "wall_s": None, "fidelity": None, "reruns": [],
                    "recovered": True})
        added += 1
    if added:
        idx.write_text(json.dumps(sorted(old, key=lambda m: m["name"]), indent=1, sort_keys=True))
    return added


def cmd_batch(args: argparse.Namespace) -> int:
    from ..batch import build, parse_seeds
    from .run import job_name, jobs_for, run_job

    _one_thread()
    if args.out.exists():
        print(json.dumps({"recovered": recover(args.out)}), flush=True)
    fams = list(args.families or FAMILIES)
    agents = list(dict.fromkeys(args.agent or AGENTS))
    if "none" not in agents:
        agents.append("none")
    if not args.no_build:
        build()
    out = args.out
    out.mkdir(parents=True, exist_ok=True)
    jobs = [j for j in jobs_for(fams, parse_seeds(args.seeds), agents, out, args.order_seed)
            if not (out / f"{job_name(j)}.json.gz").exists()]
    idx = out / "episodes_v3.json"
    t0 = time.time()
    bad = 0
    print(json.dumps({"jobs_left": len(jobs), "workers": args.workers, "out": str(out)}), flush=True)
    with mp.get_context("spawn").Pool(args.workers) as pool:
        for k in range(0, len(jobs), CHUNK):
            metas = list(pool.imap_unordered(run_job, jobs[k: k + CHUNK], chunksize=1))
            old = json.loads(idx.read_text()) if idx.exists() else []
            keep = {m["name"]: m for m in old}
            keep.update({m["name"]: m for m in metas})
            tmp = idx.with_suffix(".tmp")
            tmp.write_text(json.dumps(sorted(keep.values(), key=lambda m: m["name"]), indent=1, sort_keys=True))
            tmp.replace(idx)
            bad += sum(1 for m in metas if m["class"] == "harness_error")
            print(json.dumps({"done": min(k + CHUNK, len(jobs)), "of": len(jobs), "harness_errors": bad,
                              "wall_s": round(time.time() - t0, 1)}), flush=True)
    return 0 if not bad else 1


# ---- reading a scored batch -----------------------------------------------------------------------------

def load_scored(d: Path) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    j = json.loads((d / "dev_v3.json").read_text())
    return j["summary"], j["episodes"]


def _served(s: dict[str, Any]) -> float | None:
    el = [n for n in s["needs"] if n["eligible"]]
    return sum(1.0 for n in el if n["served"]) / len(el) if el else None


def value(s: dict[str, Any], metric: str) -> float | None:
    """An episode's value of a metric (served share from its needs; the rest from its metrics)."""
    if metric == "served_share":
        return _served(s)
    return V2M.metric_value(s.get("metrics") or {}, metric)


def by_situation(scored: list[dict[str, Any]], families: list[str], agent: str, metric: str) -> dict[Any, float]:
    out = {}
    for s in scored:
        if s["agent"] != agent or s["family"] not in families:
            continue
        v = value(s, metric)
        if v is not None:
            out[(s["family"], s["seed"])] = float(v)
    return out


def paired(scored: list[dict[str, Any]], families: list[str], a: str, b: str, metric: str, seed: int = 0) -> dict[str, Any]:
    """A minus B on one metric, paired by situation, over these family-worlds (v2's `compare`)."""
    va, vb = by_situation(scored, families, a, metric), by_situation(scored, families, b, metric)
    c = V2M.compare(va, vb, metric, seed=seed)
    c["diffs"] = {f"{k[0]}:{k[1]}": round(va[k] - vb[k], 6) for k in sorted(set(va) & set(vb), key=str)}
    return c


def one_sided_p(diffs: list[float], seed: int = 0, n_perm: int = S.N_BOOT, exact_max: int = 16) -> float | None:
    """The one-sided sign-flip permutation test that the mean of `diffs` is below zero (each diff
    oriented so the prediction is positive): the share of sign patterns whose mean is at most the
    observed one (exact up to `exact_max`, else Monte Carlo with the observed one counted)."""
    x = np.asarray([float(v) for v in diffs], dtype=float)
    n = len(x)
    if n == 0:
        return None
    obs = float(x.mean())
    tol = 1e-12 * max(1.0, abs(obs))
    if n <= exact_max:
        signs = np.array(list(itertools.product((1.0, -1.0), repeat=n)))
        return float(np.mean((signs * x).mean(axis=1) <= obs + tol))
    rng = np.random.default_rng(seed)
    signs = rng.choice((-1.0, 1.0), size=(n_perm, n))
    return (1.0 + float(np.sum((signs * x).mean(axis=1) <= obs + tol))) / (n_perm + 1.0)


def move_verdict(c: dict[str, Any], want: str) -> str:
    lo, hi, d = c.get("lo"), c.get("hi"), c.get("mean_difference")
    if lo is None or hi is None or d is None:
        return "no data"
    if want == "up":
        return "moved" if lo > 0 else ("wrong way" if hi < 0 else "not moved")
    if want == "down":
        return "moved" if hi < 0 else ("wrong way" if lo > 0 else "not moved")
    if want == "not worse":
        return "not worse" if lo > -NOT_WORSE else ("worse" if hi < -NOT_WORSE else "not shown not worse")
    return "?"


def ladder_verdict(c: dict[str, Any]) -> str:
    lo, hi = c.get("lo"), c.get("hi")
    if lo is None or hi is None:
        return "no data"
    return "fails" if hi < 0 else ("holds" if lo > 0 else "tie")


def orient(want: str) -> int:
    return -1 if want == "down" else 1


def leans_wrong(d: float | None, want: str) -> bool:
    """The point estimate leans against the prediction (lesson 2's audit)."""
    if d is None:
        return False
    if want == "not worse":
        return d < -NOT_WORSE
    return orient(want) * d < 0


# ---- the report -----------------------------------------------------------------------------------------

def families_with_needs(scored: list[dict[str, Any]]) -> list[str]:
    return sorted({s["family"] for s in scored if any(n["eligible"] for n in s["needs"])})


def family_of(fw: str) -> str:
    return fw.rsplit("-", 1)[0]


def ceiling_losses(scored: list[dict[str, Any]], fw: str) -> dict[str, Any]:
    """The ceiling's lost needs in a family-world by cause: its plan's verdict when no plan could
    serve the need (a capability finding about the fleet), else the loss's cause on the picture."""
    out: Counter = Counter()
    plan: Counter = Counter()
    for s in scored:
        if s["family"] != fw or s["agent"] != CEILING:
            continue
        feas = s.get("feasibility") or {}
        for n in s["needs"]:
            if not n["eligible"] or n["served"]:
                continue
            f = (feas.get(n["contact"]) or {})
            v = f.get("verdict") or "unknown"
            if v != "feasible":
                out[f"plan:{v}"] += 1
                plan[v] += 1
            else:
                out[f"execution:{str(n.get('cause') or 'unknown').split('(')[0]}"] += 1
    lost = sum(out.values())
    return {"lost": lost, "by_cause": dict(out.most_common()), "infeasible": dict(plan.most_common()),
            "infeasible_share": round(sum(plan.values()) / lost, 3) if lost else None}


def feasibility_counts(scored: list[dict[str, Any]], fw: str) -> dict[str, int]:
    """Every eligible need of the family-world by the ceiling's plan verdict (served or not)."""
    c: Counter = Counter()
    for s in scored:
        if s["family"] != fw or s["agent"] != CEILING:
            continue
        for n in s["needs"]:
            if n["eligible"]:
                c[((s.get("feasibility") or {}).get(n["contact"]) or {}).get("verdict") or "unknown"] += 1
    return dict(c.most_common())


def report(scored: list[dict[str, Any]], summary: dict[str, Any], seed: int = 0) -> dict[str, Any]:
    agents = sorted({s["agent"] for s in scored})
    fws = [f for f in FAMILIES if any(s["family"] == f for s in scored)]
    needy = families_with_needs(scored)
    out: dict[str, Any] = {"coverage": summary.get("coverage"), "agents": agents, "family_worlds": fws,
                           "with_needs": needy, "min_room": MIN_ROOM}
    # 1-3: the room, lesson 3, the ceiling's losses
    out["room"] = {f: room(scored, f, seed=seed) for f in fws if CEILING in agents and "none" in agents}
    out["lesson3"] = {f: (summary.get("families") or {}).get(f, {}).get("flags") for f in fws}
    out["ceiling_losses"] = {f: ceiling_losses(scored, f) for f in fws}
    out["feasibility"] = {f: feasibility_counts(scored, f) for f in fws}
    # 4: the ladder
    ladder: dict[str, Any] = {}
    fams = sorted({family_of(f) for f in fws})
    for cell in fws + [f"{fam}*" for fam in fams]:
        members = [cell] if not cell.endswith("*") else [f for f in fws if family_of(f) == cell[:-1]]
        members = [m for m in members if m in needy]
        if not members:
            continue
        arms = {}
        for a in agents:
            vals = list(by_situation(scored, members, a, "served_share").values())
            if vals:
                arms[a] = {"share": round(statistics.fmean(vals), 3), "n": len(vals)}
        preds = []
        for (a, b), judged in [(p, True) for p in JUDGED] + [(p, False) for p in REPORTED]:
            if a not in agents or b not in agents:
                continue
            c = paired(scored, members, a, b, "served_share", seed=seed)
            preds.append({"a": a, "b": b, "judged": judged and not cell.endswith("*"), "verdict": ladder_verdict(c),
                          **{k: c.get(k) for k in ("mean_difference", "lo", "hi", "n_situations", "method", "p")},
                          "p_wrong": one_sided_p(list(c["diffs"].values()), seed=seed)})
        ladder[cell] = {"arms": arms, "predictions": preds}
    out["ladder"] = ladder
    # 5: the moves
    honest_up: dict[str, Any] = {}
    if HONEST in agents and "none" in agents:
        for metric in ("served_share", "fault_met_share"):
            va, vb = by_situation(scored, needy, HONEST, metric), by_situation(scored, needy, "none", metric)
            both = sorted(set(va) & set(vb), key=str)
            strata: dict[str, list[float]] = defaultdict(list)
            for k in both:
                strata[k[0]].append(va[k] - vb[k])
            b = S.stratified_bca(strata, seed=seed) if both else {"estimate": None}
            honest_up[metric] = {**b, "mean_difference": b.get("estimate")}
    out["honest_above_none"] = honest_up
    moves: dict[str, Any] = {}
    for pert, ms in MOVES.items():
        if pert not in agents or HONEST not in agents:
            continue
        rows = []
        for metric, want in ms:
            va, vb = by_situation(scored, needy, pert, metric), by_situation(scored, needy, HONEST, metric)
            both = sorted(set(va) & set(vb), key=str)
            strata = defaultdict(list)
            for k in both:
                strata[k[0]].append(va[k] - vb[k])
            pooled = S.stratified_bca(strata, seed=seed) if both else {"estimate": None, "lo": None, "hi": None, "n": 0}
            pc = {"mean_difference": pooled.get("estimate"), "lo": pooled.get("lo"), "hi": pooled.get("hi")}
            v = move_verdict(pc, want)
            base = honest_up.get(metric) or {}
            if want == "down" and v != "moved" and not ((base.get("lo") or 0.0) > 0):
                v = "not testable" if both else "no data"
            per = {}
            for f in needy:
                c = paired(scored, [f], pert, HONEST, metric, seed=seed)
                diffs = [orient(want) * (x + (NOT_WORSE if want == "not worse" else 0.0)) for x in c["diffs"].values()]
                per[f] = {"verdict": move_verdict(c, want), "p_wrong": one_sided_p(diffs, seed=seed),
                          **{k: c.get(k) for k in ("mean_difference", "lo", "hi", "n_situations")}}
            rows.append({"metric": metric, "want": want, "verdict": v, "n": pooled.get("n"), **pc, "per_family": per})
        moves[pert] = rows
    out["moves"] = moves
    # 6: the per-family guard, Holm across each family's cells (both worlds)
    guard: dict[str, Any] = {}
    for fam in fams:
        cells: dict[str, float | None] = {}
        for f in [x for x in needy if family_of(x) == fam]:
            for p in (ladder.get(f) or {}).get("predictions") or []:
                if p["judged"]:
                    cells[f"{f} ladder {p['a']} >= {p['b']}"] = p["p_wrong"]
            for pert, rows in moves.items():
                for r in rows:
                    if r["metric"] == "fault_met_share" or r["verdict"] == "not testable":
                        continue
                    cells[f"{f} {pert} {r['metric']} {r['want']}"] = (r["per_family"].get(f) or {}).get("p_wrong")
        h = S.holm(cells, alpha=GUARD_ALPHA) if cells else {}
        guard[fam] = {"cells": len([v for v in cells.values() if v is not None]),
                      "flagged": sorted(k for k, v in h.items() if v.get("reject")),
                      "smallest": sorted(((k, v["p_holm"]) for k, v in h.items() if v.get("p_holm") is not None),
                                         key=lambda kv: kv[1])[:3]}
    out["guard"] = guard
    return out


def judged_cells(r: dict[str, Any]) -> dict[str, tuple[float | None, str]]:
    """Every judged cell's point estimate and predicted direction (the audit's input)."""
    out: dict[str, tuple[float | None, str]] = {}
    for f, x in (r.get("room") or {}).items():
        out[f"room {f}"] = (x.get("mean_difference"), "up")
    for cell, x in (r.get("ladder") or {}).items():
        for p in x["predictions"]:
            if p["judged"]:
                out[f"ladder {cell} {p['a']} >= {p['b']}"] = (p.get("mean_difference"), "up")
    for pert, rows in (r.get("moves") or {}).items():
        for m in rows:
            out[f"move {pert} {m['metric']} pooled"] = (m.get("mean_difference"), m["want"])
            for f, c in m["per_family"].items():
                out[f"move {pert} {m['metric']} {f}"] = (c.get("mean_difference"), m["want"])
    return out


def audit(now: dict[str, Any], prev: dict[str, Any]) -> dict[str, Any]:
    """Lesson 2: the judged cells whose point estimate leans the wrong way in both batches."""
    a, b = judged_cells(now), judged_cells(prev)
    both = sorted(k for k in set(a) & set(b) if leans_wrong(a[k][0], a[k][1]) and leans_wrong(b[k][0], b[k][1]))
    once = sorted(k for k in set(a) & set(b) if leans_wrong(a[k][0], a[k][1]) != leans_wrong(b[k][0], b[k][1]))
    return {"cells": len(set(a) & set(b)), "wrong_way_both": [{"cell": k, "now": a[k][0], "prev": b[k][0], "want": a[k][1]}
                                                             for k in both],
            "wrong_way_once": len(once)}


def _num(x: float | None) -> str:
    return "n/a" if x is None else f"{x:+.3f}"


def pct(x: float | None, nd: int = 0) -> str:
    return "n/a" if x is None else f"{100 * x:.{nd}f}"


def ci(x: dict[str, Any], scale: float = 100.0, nd: int = 1) -> str:
    d, lo, hi = x.get("mean_difference"), x.get("lo"), x.get("hi")
    if d is None:
        return "n/a"
    f = lambda v: "?" if v is None else f"{scale * v:+.{nd}f}"  # noqa: E731
    return f"{f(d)} [{f(lo)}, {f(hi)}]"


def markdown(r: dict[str, Any], d: Path) -> str:
    cov = r.get("coverage") or {}
    L = ["# Scoring v3: the ruler (dev)", "",
         f"Batch `{d.name}`. MODEL RESULTS ONLY: free deterministic agents on generated situations, the approve-all "
         "operator, the paused clock; never human data.", "", "## Coverage", ""]
    for w, x in (cov.get("worlds") or {}).items():
        L.append(f"- **{w}**: {x['vehicles']} vehicles ({', '.join(f'{k} {v}' for k, v in x['by_domain'].items())}); "
                 f"sensors {', '.join(x['sensors'])}; {x['contacts']} contacts in {', '.join(x['contact_domains'])}.")
    L += [f"- Conditions: {', '.join(f'{k} {v}' for k, v in sorted((cov.get('light') or {}).items()))} (situations by "
          f"light); {cov.get('situations')} situations, {cov.get('episodes')} episodes.",
          f"- Agents: {', '.join(r['agents'])}.", ""]
    L += ["## Room (10.1)", "", f"Ceiling minus `none` on need served, paired by situation; room when the interval lies "
          f"above zero and the mean is at least {100 * MIN_ROOM:.0f} points.", "",
          "| family-world | none | ceiling | ceiling - none [95 % CI] | n | verdict |", "|---|---|---|---|---|---|"]
    for f, x in r["room"].items():
        L.append(f"| {f} | {pct(x.get('floor_share'))} % | {pct(x.get('ceiling_share'))} % | {ci(x)} | "
                 f"{x.get('n_situations')} | {x['verdict']} |")
    L += ["", "## The ceiling's losses by cause", "", "| family-world | plan verdicts (eligible needs) | lost | by cause |",
          "|---|---|---|---|"]
    for f, x in r["ceiling_losses"].items():
        L.append(f"| {f} | {', '.join(f'{k} {v}' for k, v in (r['feasibility'].get(f) or {}).items())} | {x['lost']} | "
                 f"{', '.join(f'{k} {v}' for k, v in x['by_cause'].items()) or 'none lost'} |")
    L += ["", "## Lesson 3 (single cause)", ""]
    for f, fl in r["lesson3"].items():
        L.append(f"- {f}: {'; '.join(fl or []) or 'no flag'}")
    short = {"oracle-v3": "ceiling", "oracle-v3-late": "late", "oracle-v3-linkblind": "linkblind",
             "oracle-v3-sensorblind": "sensorblind", "oracle-v3-honest": "honest", "none": "none"}
    L += ["", "## The ladder", "", "Need served (%), per family-world and pooled per family (`*`).", "",
          "| cell | " + " | ".join(short[a] for a in RUNGS + ("none",)) + " | playbook | random-valid |",
          "|---|" + "---|" * (len(RUNGS) + 3)]
    for cell, x in r["ladder"].items():
        arms = x["arms"]
        L.append(f"| {cell} | " + " | ".join(f"{pct((arms.get(a) or {}).get('share'))}"
                                            for a in RUNGS + ("none", "playbook", "random-valid")) + " |")
    L += ["", "Judged orders (A at least B; v2 0.3's rule), A minus B in points [95 % CI]:", "",
          "| cell | " + " | ".join(f"ceiling >= {short[b]}" for _, b in JUDGED) + " |", "|---|" + "---|" * len(JUDGED)]
    for cell, x in r["ladder"].items():
        by = {(q["a"], q["b"]): q for q in x["predictions"]}
        L.append(f"| {cell} | " + " | ".join(f"{(by.get(j) or {}).get('verdict', 'n/a')} {ci(by.get(j) or {})}"
                                            for j in JUDGED) + " |")
    L += ["", "Reported, never judged (A minus B, points), pooled per family:", "",
          "| family | " + " | ".join(f"{short[a]} - {short[b]}" for a, b in REPORTED) + " |", "|---|" + "---|" * len(REPORTED)]
    for cell, x in r["ladder"].items():
        if not cell.endswith("*"):
            continue
        by = {(q["a"], q["b"]): q for q in x["predictions"]}
        L.append(f"| {cell[:-1]} | " + " | ".join(ci(by.get(j) or {}) for j in REPORTED) + " |")
    L += ["", "## Perturbations (against the honest ceiling)", "",
          f"Honest above `none` (pooled, need served): {ci(r['honest_above_none'].get('served_share') or {}, 100)}.", "",
          "| perturbation | metric | predicted | pooled [95 % CI] | verdict | per family-world |", "|---|---|---|---|---|---|"]
    for pert, rows in r["moves"].items():
        for m in rows:
            scale = 100.0 if m["metric"].endswith("share") else 1.0
            counts = Counter(c["verdict"] for c in m["per_family"].values())
            wrong = [f.replace("v3-", "") for f, c in m["per_family"].items() if c["verdict"] in ("wrong way", "worse")]
            per = ", ".join(f"{k} {v}" for k, v in counts.most_common()) + (f" (wrong way: {', '.join(wrong)})" if wrong else "")
            L.append(f"| {pert.replace('oracle-v3-honest', 'honest')} | {m['metric']} | {m['want']} | "
                     f"{ci(m, scale, 1 if scale == 100.0 else 2)} | {m['verdict']} | {per} |")
    L += ["", "## The per-family guard (Holm)", ""]
    for fam, g in r["guard"].items():
        L.append(f"- {fam}: {g['cells']} cells; flagged: {', '.join(g['flagged']) or 'none'}; smallest adjusted p: "
                 f"{', '.join(f'{k} {v:.3f}' for k, v in g['smallest']) or 'n/a'}")
    if r.get("audit"):
        a = r["audit"]
        what = f"halves {' and '.join(a['halves'])}" if a.get("halves") else "batches"
        L += ["", "## Audit (lesson 2)", "", f"{a['cells']} judged cells in both {what}; "
              f"{len(a['wrong_way_both'])} lean the wrong way in both; {a['wrong_way_once']} in one only.", ""]
        for x in a["wrong_way_both"]:
            L.append(f"- {x['cell']}: {_num(x['prev'])} in the first, {_num(x['now'])} in the second (want {x['want']})")
    return "\n".join(L) + "\n"


def halves(scored: list[dict[str, Any]], spec: list[str]) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """The batch's episodes on two disjoint seed ranges (`lo-hi lo-hi`), each a full set of families,
    worlds and agents: the audit's two halves (the owner's decision of 2026-10-25, SCORING-V3.md 10.3)."""
    from ..batch import parse_seeds

    a, b = (set(parse_seeds([x])) for x in spec)
    return [s for s in scored if s["seed"] in a], [s for s in scored if s["seed"] in b]


def cmd_report(args: argparse.Namespace) -> int:
    summary, scored = load_scored(args.dir)
    r = report(scored, summary, seed=0)
    if args.prev:
        ps, pscored = load_scored(args.prev)
        r["audit"] = audit(r, report(pscored, ps, seed=0))
    elif args.halves:
        first, second = halves(scored, args.halves)
        ra, rb = report(first, summary, seed=0), report(second, summary, seed=0)
        r["audit"] = {**audit(rb, ra), "halves": args.halves, "situations": [
            len({(s["family"], s["seed"]) for s in first}), len({(s["family"], s["seed"]) for s in second})]}
    (args.dir / "ruler_v3.json").write_text(json.dumps(r, indent=1, sort_keys=True, default=str))
    md = markdown(r, args.dir)
    (args.dir / "ruler_v3.md").write_text(md)
    print(md)
    return 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    b = sub.add_parser("batch")
    b.add_argument("--families", nargs="+")
    b.add_argument("--seeds", nargs="+", default=["5160-5229"])
    b.add_argument("--agent", nargs="+")
    b.add_argument("--workers", type=int, default=8)
    b.add_argument("--out", type=Path, default=REPO / "eval" / "results" / "headless" / "v3-ruler")
    b.add_argument("--order-seed", type=int, default=0)
    b.add_argument("--no-build", action="store_true")
    r = sub.add_parser("report")
    r.add_argument("dir", type=Path)
    r.add_argument("--prev", type=Path, help="an earlier batch to audit against")
    r.add_argument("--halves", nargs=2, metavar="SEEDS", help="audit two halves of this batch (lo-hi lo-hi)")
    args = ap.parse_args(argv)
    return cmd_batch(args) if args.cmd == "batch" else cmd_report(args)


if __name__ == "__main__":
    sys.exit(main())
