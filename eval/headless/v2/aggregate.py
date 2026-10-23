"""Scoring v2 over a batch (eval/SCORING-V2.md, sections 3, 7 and 8): every v2 episode labelled
against its paired `none` replicate, its metrics, each situation's replicates averaged, the
arms' summaries and paired comparisons per family and pooled, the primary endpoint, Holm over
the secondary family, the multiverse, and the excluded episodes. MODEL RESULTS: a scripted
operator on generated situations; never human data.

Keys used throughout:

- an **arm** is an agent under a condition: the operator's mode and the policy file (and the
  clock condition when a record states one, `paused` by default);
- a **situation** is `<family or situation name>:<situation seed>`; its replicates are averaged
  before anything else (8.1);
- an episode whose class is not `scored` (section 7) is left out of every metric; a comparison
  leaves out every situation where either arm has one, counts them, scores budget stops and
  model failures as every need unserved in a sensitivity line, and is `incomplete` when more
  than 5 % of its situations are out.
"""

from __future__ import annotations

from collections import defaultdict
from dataclasses import replace
from pathlib import Path
from typing import Any

import numpy as np

from . import metrics as M
from . import stats as S
from .labels import Params, episode_labels
from .record import load

#: SCORING-V2.md 9.1: one family per test area (`HW`: Harbor watch, reported alone)
AREAS: list[dict[str, Any]] = [
    {"area": "S1", "name": "identify", "families": ["fast-unknown"], "subject": "officer",
     "metrics": ["served_share", "rmst"]},
    {"area": "S2", "name": "allocate", "families": ["two-at-once"], "subject": "officer",
     "metrics": ["served_share", "per_hour.harmful_b", "per_hour.duplicate"]},
    {"area": "S3", "name": "restraint", "families": ["decoy"], "subject": "officer, app (picture)",
     "metrics": ["per_hour.useless", "per_hour.useless_unjustified", "bad_card_share", "served_share"]},
    {"area": "S4", "name": "quiet watch", "families": ["quiet"], "subject": "officer, app",
     "metrics": ["per_hour.useless", "cards_per_10min"]},
    {"area": "S5", "name": "reserve", "families": ["low-battery"], "subject": "officer, app (policy)",
     "metrics": ["fault_met_share", "per_hour.harmful_a", "served_share"]},
    {"area": "S6", "name": "vehicle failure", "families": ["stuck-vehicle"], "subject": "officer, system",
     "metrics": ["served_after_stop", "per_hour.fault_actions"]},
    {"area": "S7", "name": "link loss", "families": ["link-cut-mid-look"], "subject": "officer, app, system",
     "metrics": ["served_share", "voids"]},
    {"area": "S8", "name": "wrong picture", "families": ["conflict"], "subject": "system (sensing, display), officer",
     "metrics": ["sensing_error_share", "justified_useless_share", "per_hour.useless"]},
    {"area": "S9", "name": "load", "families": ["approval-flood"], "subject": "app (inbox, policy), officer",
     "metrics": ["flood_share", "bad_card_share", "time_margin_median", "served_share"]},
    {"area": "S10", "name": "policy", "families": ["fast-unknown", "two-at-once", "approval-flood"],
     "subject": "app (policy)", "metrics": ["served_share", "harmful_dispatched", "useless_dispatched", "cards_per_10min"],
     "by_condition": True},
    {"area": "sealed", "name": "sealed", "families": [], "subject": "all", "metrics": ["served_share", "rmst"]},
    {"area": "HW", "name": "Harbor watch", "families": ["harbor-watch"], "subject": "all",
     "metrics": ["served_share", "rmst", "cards_per_10min"]},
]
#: 8.3: the secondary family, Holm-adjusted together (the primary pair's differences, and each
#: agent minus none on 3.1)
SECONDARY = ("rmst", "per_hour.useful", "per_hour.useless", "per_hour.harmful", "cards_per_10min", "flood_share",
             "bad_card_share", "fault_met_share")
#: 8.5: each dimension's values and the metrics it bears on
MULTIVERSE: dict[str, tuple[tuple[float, ...], tuple[str, ...]]] = {
    "late_s": ((60.0, 90.0, 120.0), ("served_late_share",)),
    "justify_horizon_s": ((150.0, 300.0, 600.0), ("per_hour.useless_unjustified", "justified_useless_share")),
    "aim_fallback_m": ((500.0, 1000.0, 2000.0), ("per_hour.useless", "per_hour.redundant", "bad_card_share")),
    "low_reserve_open_pct": ((30.0, 35.0, 40.0), ("fault_met_share",)),
}
EXCLUDED_CLASSES = ("harness_error", "budget_stop", "model_failed")
INCOMPLETE_SHARE = 0.05
#: 3.1's admissibility rule and 10.1's world pilot: none serves at most this share, the oracle at
#: least that one, of a family's eligible needs
NONE_MAX = 0.5
ORACLE_MIN = 0.8
#: 8.1: reported alone, never pooled
ALONE = ("harbor-watch",)


def base_name(name: str | None) -> str:
    """A family's name without a version mark (`v2-x`, `x-v2`, `x@2`), for the area table."""
    n = str(name or "")
    for pre in ("v2-", "v2/", "v2_"):
        if n.startswith(pre):
            n = n[len(pre):]
    for suf in ("-v2", "_v2", "@v2", "@2"):
        if n.endswith(suf):
            n = n[: -len(suf)]
    return n


def area_of(meta: dict[str, Any]) -> str | None:
    """The test area (9.1) of an episode: its explicit `area` (an area's code, or as the v2
    generators state it, `S1 identify` or `Harbor watch`), else its family's."""
    if meta.get("area"):
        a = str(meta["area"])
        codes = {x["area"] for x in AREAS}
        names = {str(x["name"]).lower(): x["area"] for x in AREAS}
        if a in codes:
            return a
        if a.split()[0] in codes:
            return a.split()[0]
        return names.get(a.lower(), a)
    fam = base_name(meta.get("family") or meta.get("base"))
    for a in AREAS:
        if a["area"] != "S10" and fam in a["families"]:
            return a["area"]
    return None


def meta_of(rec: dict[str, Any], path: Path | None = None) -> dict[str, Any]:
    """What the aggregate needs to place an episode: its agent, situation, replicate, arm."""
    s = rec.get("situation") or {}
    v2 = rec.get("v2") or {}
    cond = v2.get("condition") or {}
    rep = rec.get("replicate") or {}
    base = s.get("family") or s.get("name")
    sseed = s.get("situation_seed") if s.get("situation_seed") is not None else rep.get("situation_seed", rec.get("seed"))
    stated = ((rec.get("truth") or {}).get("stated") or {})
    return {
        "path": str(path) if path else None, "agent": v2.get("agent"), "base": base, "family": s.get("family"),
        "situation": f"{base}:{sseed}", "situation_seed": sseed, "r": rep.get("r", 0), "seed": rec.get("seed"),
        "operator": cond.get("operator"), "policy": cond.get("policy"), "clock": cond.get("clock", "paused"),
        "class": rec.get("episode_class", "scored"), "area": s.get("area") or stated.get("area"),
    }


def cond_key(m: dict[str, Any]) -> str:
    return f"{m.get('operator')};{m.get('policy')};{m.get('clock', 'paused')}"


def cells(base: Params) -> dict[str, Params]:
    """The multiverse's cells: the default, and each dimension's other values one at a time."""
    out = {"default": base}
    for dim, (values, _) in MULTIVERSE.items():
        for v in values:
            p = replace(base, **{dim: float(v)})
            if p != base:
                out[f"{dim}={v:g}"] = p
    return out


def score(metas: list[dict[str, Any]], params: Params | None = None, multiverse: bool = True,
          keep_labels: bool = False) -> list[dict[str, Any]]:
    """Every episode's labels and metrics (each multiverse cell's too), each paired with its
    `none` replicate (same situation, replicate and condition). `metas` carry each record's
    `path` (meta_of); a record is loaded once, the `none` records first."""
    base = params or Params()
    cs = cells(base) if multiverse else {"default": base}
    out = []
    none_labels: dict[tuple[str, int, str], dict[str, dict[str, Any]]] = {}
    order = sorted(metas, key=lambda m: (m.get("agent") != "none", m.get("path") or ""))
    for m in order:
        if m.get("class", "scored") != "scored" or not m.get("path"):
            out.append({**m, "metrics": None, "cells": {}})
            continue
        rec = load(m["path"])
        key = (m["situation"], int(m.get("r") or 0), cond_key(m))
        labs: dict[str, dict[str, Any]] = {}
        for name, p in cs.items():
            if m["agent"] == "none":
                labs[name] = episode_labels(rec, params=p)
            else:
                nl = (none_labels.get(key) or {}).get(name)
                labs[name] = episode_labels(rec, params=p, none_labels=nl)
        if m["agent"] == "none":
            none_labels[key] = labs
        mets = {name: M.episode_metrics(rec, lab) for name, lab in labs.items()}
        item = {**m, "paired": labs["default"]["paired"], "metrics": mets["default"],
                "cells": {name: _cell_values(x) for name, x in mets.items() if name != "default"}}
        if keep_labels:
            item["labels"] = labs["default"]
        out.append(item)
    return out


def _cell_values(m: dict[str, Any]) -> dict[str, Any]:
    names = {n for _, (_, ms) in MULTIVERSE.items() for n in ms}
    return {n: M.metric_value(m, n) for n in names}


# ---- situations and arms ----------------------------------------------------------------------

#: situations() by its arguments; each entry holds its `scored` list, so the list's id cannot be
#: reused by another list while the entry stands
_CACHE: dict[tuple[Any, ...], tuple[list[dict[str, Any]], dict[str, dict[str, Any]]]] = {}


def situations(scored: list[dict[str, Any]], agent: str, cond: str, bases: list[str] | None = None,
               r0_only: bool = False, cell: str = "default") -> dict[str, dict[str, Any]]:
    """Per situation, the arm's situation metrics (replicates averaged) over its scored episodes."""
    key = (id(scored), len(scored), agent, cond, tuple(bases) if bases is not None else None, r0_only, cell)
    hit = _CACHE.get(key)
    if hit is not None and hit[0] is scored:
        return hit[1]
    if len(_CACHE) > 4096:
        _CACHE.clear()
    eps: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for e in scored:
        if e["agent"] != agent or cond_key(e) != cond or e["metrics"] is None:
            continue
        if bases is not None and base_name(e["base"]) not in bases and e["base"] not in bases:
            continue
        if r0_only and int(e.get("r") or 0) != 0:
            continue
        if cell == "default":
            eps[e["situation"]].append(e["metrics"])
        else:
            cv = _unflatten(e["cells"].get(cell) or {})
            m = {**e["metrics"], **{k: v for k, v in cv.items() if not isinstance(v, dict)}}
            m["per_hour"] = {**e["metrics"]["per_hour"], **cv.get("per_hour", {})}
            eps[e["situation"]].append(m)
    out = {s: M.situation_metrics(v) for s, v in sorted(eps.items())}
    _CACHE[key] = (scored, out)
    return out


def _unflatten(vals: dict[str, Any]) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for k, v in vals.items():
        if "." in k:
            a, b = k.split(".", 1)
            out.setdefault(a, {})[b] = v
        else:
            out[k] = v
    return out


def values(sits: dict[str, dict[str, Any]], metric: str) -> dict[str, float | None]:
    return {s: M.metric_value(sm, metric) for s, sm in sits.items()}


def excluded_situations(scored: list[dict[str, Any]], agents: tuple[str, ...], cond: str,
                        bases: list[str] | None = None, r0_agents: tuple[str, ...] = ()) -> dict[str, list[str]]:
    """Situations with an episode of these arms whose class is not `scored`, by class (an arm
    read at replicate 0 only counts its replicate 0)."""
    out: dict[str, list[str]] = defaultdict(list)
    for e in scored:
        if e["agent"] not in agents or cond_key(e) != cond or e.get("class", "scored") == "scored":
            continue
        if bases is not None and base_name(e.get("base")) not in bases and e.get("base") not in bases:
            continue
        if e["agent"] in r0_agents and int(e.get("r") or 0) != 0:
            continue
        out[e["situation"]].append(f"{e['agent']}:{e['class']}")
    return dict(out)


def comparison(scored: list[dict[str, Any]], a: str, b: str, cond: str, metric: str, bases: list[str] | None = None,
               r0_only_a: bool = False, cell: str = "default", seed: int = 0) -> dict[str, Any]:
    """Arm a minus arm b on one metric, paired by situation (section 8; 7's exclusions)."""
    sa = situations(scored, a, cond, bases, r0_only_a, cell)
    sb = situations(scored, b, cond, bases, False, cell)
    out_s = excluded_situations(scored, (a, b), cond, bases, (a,) if r0_only_a else ())
    va = {s: v for s, v in values(sa, metric).items() if s not in out_s}
    vb = {s: v for s, v in values(sb, metric).items() if s not in out_s}
    c = M.compare(va, vb, metric, seed=seed)
    both = set(sa) | set(sb) | set(out_s)
    c.update(a=a, b=b, metric=metric, condition=cond, excluded=out_s,
             excluded_share=round(len(out_s) / len(both), 6) if both else 0.0,
             incomplete=bool(both) and len(out_s) / len(both) > INCOMPLETE_SHARE)
    if metric == "served_share" and any(any(x.endswith(("budget_stop", "model_failed")) for x in v) for v in out_s.values()):
        # 7.2's sensitivity: a budget stop or a model failure scored as every need unserved
        # (only where nothing else excluded the situation: no harness error, nothing of b's)
        va2, vb2 = dict(va), dict(vb)
        all_b = values(sb, metric)
        for s, why in out_s.items():
            if all(x.startswith(f"{a}:") and x.endswith(("budget_stop", "model_failed")) for x in why) and s in all_b:
                va2[s] = 0.0
                vb2[s] = all_b[s]
        c["sensitivity_every_need_unserved"] = M.compare(va2, vb2, metric, seed=seed)
    return c


def arms(scored: list[dict[str, Any]]) -> list[tuple[str, str]]:
    return sorted({(e["agent"], cond_key(e)) for e in scored if e.get("agent")})


def pooled_share(scored: list[dict[str, Any]], agent: str, cond: str, fam: str | None) -> dict[str, Any]:
    """An arm's needs served by their deadline over its eligible needs, pooled over the
    situations (and replicates) of a family, or of every family but Harbor watch (`fam` None)."""
    served = eligible = situations_n = 0
    sits = set()
    for e in scored:
        if e["agent"] != agent or cond_key(e) != cond or not e.get("metrics") or not e.get("base"):
            continue
        b = base_name(e["base"])
        if (fam is None and b in ALONE) or (fam is not None and b != fam):
            continue
        served += e["metrics"]["served"]
        eligible += e["metrics"]["eligible"]
        sits.add(e["situation"])
    situations_n = len(sits)
    return {"served": served, "eligible": eligible, "share": round(served / eligible, 6) if eligible else None,
            "n_situations": situations_n}


def admissibility(scored: list[dict[str, Any]], cond: str) -> dict[str, Any]:
    """3.1's rule per family: `none` serves at most 50 % and `oracle` at least 80 % of its
    eligible needs (pooled over its situations), when both ran."""
    out = {}
    for fam in sorted({base_name(e["base"]) for e in scored if e.get("base")}):
        row: dict[str, Any] = {agent: pooled_share(scored, agent, cond, fam) for agent in ("none", "oracle")}
        for agent in ("none", "oracle"):
            row[agent].pop("n_situations", None)
        n, o = row["none"]["share"], row["oracle"]["share"]
        row["admissible"] = None if n is None or o is None else (n <= NONE_MAX and o >= ORACLE_MIN)
        out[fam] = row
    return out


def admitted_families(scored: list[dict[str, Any]], cond: str, pair: tuple[str, str],
                      given: dict[str, Any] | None = None) -> dict[str, Any]:
    """Resolution 21: the families the primary pools. A family is admitted when its admissibility
    verdict (3.1) is true: from `given` (an admissibility batch's `admissibility`, under this
    condition or its only one: `{"source", "verdicts"}`), else from this batch's own `none` and
    `oracle` when both ran under this condition (said so: not the admissibility seeds). A family
    without a true verdict is excluded, with why; Harbor watch is reported alone (8.1), never
    pooled."""
    fams = sorted({base_name(e["base"]) for e in scored if e.get("base") and cond_key(e) == cond
                   and e["agent"] in pair})
    if given is not None:
        v = given.get("verdicts") or {}
        verdicts = v.get(cond) if cond in v else (next(iter(v.values())) if len(v) == 1 else {})
        source = f"given: {given.get('source')}" + ("" if cond in v else " (its only condition)")
    else:
        verdicts = admissibility(scored, cond)
        source = "this batch's none and oracle (not the admissibility seeds)"
    admitted, excluded = [], {}
    for f in fams:
        if f in ALONE:
            excluded[f] = "reported alone (8.1)"
            continue
        row = (verdicts or {}).get(f) or {}
        if row.get("admissible") is True:
            admitted.append(f)
        elif row.get("admissible") is False:
            excluded[f] = (f"not admissible: none {row.get('none', {}).get('share')}, oracle "
                           f"{row.get('oracle', {}).get('share')}")
        else:
            excluded[f] = "no admissibility verdict (none or oracle missing, or no eligible needs)"
    return {"admitted": admitted, "excluded": excluded, "source": source}


def aggregate(scored: list[dict[str, Any]], primary: tuple[str, str] = ("model", "playbook"),
              primary_operator: str = "approve-all", r0_only: tuple[str, ...] = ("model",),
              seed: int = 0, admissible: dict[str, Any] | None = None) -> dict[str, Any]:
    """aggregate_v2.json: per family and arm, each metric over situations; the comparisons
    (each agent minus none; the primary pair); the primary endpoint over the admitted families
    (`admitted_families`, from `admissible` when given: an admissibility batch's verdicts);
    Holm; the multiverse; the excluded episodes; admissibility; the world pilot's gate when its
    agents ran (10.1)."""
    out: dict[str, Any] = {"caveat": "Model results: a scripted operator on generated situations; not human data.",
                           "episodes": len(scored), "excluded": [], "families": {}, "pooled": {}}
    for e in scored:
        if e.get("class", "scored") != "scored":
            out["excluded"].append({k: e.get(k) for k in ("path", "agent", "situation", "r", "class")})
    conds = sorted({cond_key(e) for e in scored})
    fams = sorted({base_name(e["base"]) for e in scored if e.get("base")})
    metric_names = sorted({*M.SCALARS, "served_after_stop", *[f"per_hour.{k}" for k in M.RATE_LABELS],
                           "per_hour.harmful_a", "per_hour.harmful_b", "per_hour.harmful_c"})
    for cond in conds:
        agents = sorted({e["agent"] for e in scored if cond_key(e) == cond and e.get("agent")})
        for fam in fams + ["*"]:
            bases = None if fam == "*" else [fam]
            block: dict[str, Any] = {"arms": {}, "comparisons": {}}
            for agent in agents:
                # the model's replicate 0 only when it is the primary pair's first (3.1); its
                # other replicates are the reliability subset (3.9)
                r0 = agent in r0_only and agent == primary[0]
                sits = situations(scored, agent, cond, bases, r0)
                every = situations(scored, agent, cond, bases, False) if r0 else sits
                if not sits:
                    continue
                arm: dict[str, Any] = {"n_situations": len(sits), "replicate_0_only": r0, "metrics": {},
                                       "n_needs": sum(len(sm["need_outcomes"]) for sm in sits.values()),
                                       "n_cards": sum(sm["cards"] for sm in sits.values())}
                for mn in metric_names:
                    arm["metrics"][mn] = M.summarize(values(sits, mn), mn, seed=seed)
                arm["n_eff_needs"] = S.n_eff([sm["need_outcomes"] for sm in sits.values()])
                tts = [t for sm in sits.values() for t in sm["time_to_serve"]]
                evs = [x == 1.0 for sm in sits.values() for x in sm["need_outcomes"]]
                arm["kaplan_meier"] = S.rmst(tts, evs, tau=60.0)["curve"] if tts else []
                margins = [t for sm in sits.values() for t in sm["time_margins"]]
                arm["time_margins"] = {"n": len(margins), **({k: round(float(v), 3) for k, v in zip(
                    ("min", "p10", "median", "p90", "max"), np.quantile(margins, [0, 0.1, 0.5, 0.9, 1]))}
                    if margins else {})}
                cons = {k: sum(1 for sm in sits.values() if sm["constraints"][k] > 0)
                        for k in ("invariant_violations", "harmful", "refused")}
                arm["constraints"] = {k: S.clopper_pearson(v, len(sits)) for k, v in cons.items()}
                arm["reliability"] = {s: sm["reliability"] for s, sm in every.items() if sm["replicates"] > 1}
                costs = [sm["cost"] for sm in every.values() if sm.get("cost")]
                if costs:
                    arm["cost_per_situation"] = {
                        "usd": M._mean([c.get("usd") for c in costs]), "wakes": M._mean([c.get("wakes") for c in costs]),
                        "model_wall_s": M._mean([c.get("model_wall_s") for c in costs]),
                        "tokens": {k: M._mean([(c.get("tokens") or {}).get(k) for c in costs])
                                   for k in ("input", "cache_write", "cache_read", "output")}}
                arm["episode_wall_s"] = M._mean([sm.get("wall_s") for sm in every.values()])
                block["arms"][agent] = arm
            for agent in agents:
                if agent == "none" or "none" not in agents:
                    continue
                block["comparisons"][f"{agent} - none"] = {
                    mn: comparison(scored, agent, "none", cond, mn, bases, agent in r0_only, seed=seed)
                    for mn in ("served_share", *SECONDARY)}
            if primary[0] in agents and primary[1] in agents:
                block["comparisons"][f"{primary[0]} - {primary[1]}"] = {
                    mn: comparison(scored, primary[0], primary[1], cond, mn, bases, primary[0] in r0_only, seed=seed)
                    for mn in ("served_share", *SECONDARY)}
            if block["arms"]:
                (out["pooled"] if fam == "*" else out["families"].setdefault(fam, {}))[cond] = block
        out.setdefault("admissibility", {})[cond] = admissibility(scored, cond)
        gate = world_pilot(scored, cond)
        if gate is not None:
            out.setdefault("world_pilot", {})[cond] = gate
    fams = {cond: admitted_families(scored, cond, primary, admissible) for cond in primary_conds(scored, primary_operator)}
    out["primary_families"] = fams
    out["primary"] = primary_endpoint(scored, primary, primary_operator, r0_only, seed,
                                      admitted={c: f["admitted"] for c, f in fams.items()})
    out["holm"] = holm_family(out, primary, primary_operator)
    out["multiverse"] = multiverse(scored, primary, r0_only, seed)
    return out


def primary_conds(scored: list[dict[str, Any]], operator: str) -> list[str]:
    return [c for c in sorted({cond_key(e) for e in scored}) if c.startswith(f"{operator};") and c.endswith(";paused")]


def primary_endpoint(scored: list[dict[str, Any]], primary: tuple[str, str], operator: str,
                     r0_only: tuple[str, ...], seed: int,
                     admitted: dict[str, list[str]] | None = None) -> dict[str, Any] | None:
    """3.1's primary: the mean over situations of (a's served share minus b's), the approve-all
    operator, paused clock, a's replicate 0 only when it is the model, b's replicates averaged;
    the paired cluster bootstrap at 20 or more situations, else the exact sign test. With
    `admitted` (per condition, the families admitted: resolution 21) only their situations are
    pooled, and a condition with none admitted has no primary (said so); without it, every
    situation (a caller's own selection)."""
    conds = primary_conds(scored, operator)
    if not conds:
        return None
    out: dict[str, Any] = {}
    for cond in conds:
        agents = {e["agent"] for e in scored if cond_key(e) == cond}
        if primary[0] in agents and primary[1] in agents:
            bases = None if admitted is None else list(admitted.get(cond) or [])
            if bases is not None and not bases:
                out[cond] = {"skipped": "no admitted family (resolution 21): see primary_families"}
                continue
            out[cond] = comparison(scored, primary[0], primary[1], cond, "served_share", bases,
                                   primary[0] in r0_only, seed=seed)
            out[cond]["families"] = bases
    return out or None


def world_pilot(scored: list[dict[str, Any]], cond: str) -> dict[str, Any] | None:
    """10.1's gate (free agents, before the freeze), per family and pooled over every family but
    Harbor watch: `none` serves at most 50 % of eligible needs, `random-valid` fewer than the
    `playbook`, the `oracle` at least 80 %; and `null` reproduces `none` in every situation and
    replicate it ran (the same truth, picture samples and audit log bar `agent.action`, by their
    digests, `batch.fidelity_digest`; the same needs served; no action). Shares pooled over the
    situations' eligible needs. None if none of these agents ran under `cond`."""
    agents = {e["agent"] for e in scored if cond_key(e) == cond}
    wanted = ("none", "random-valid", "playbook", "oracle", "null")
    if not agents & set(wanted[1:]):
        return None
    fams = sorted({base_name(e["base"]) for e in scored if e.get("base") and cond_key(e) == cond} - set(ALONE))

    def row(fam: str | None) -> dict[str, Any]:
        r: dict[str, Any] = {a: pooled_share(scored, a, cond, fam) for a in wanted[:4] if a in agents}
        n, rv, pb, o = (r.get(a, {}).get("share") for a in wanted[:4])
        r["checks"] = {
            "none_at_most_50": None if n is None else n <= NONE_MAX,
            "random_valid_below_playbook": None if rv is None or pb is None else rv < pb,
            "oracle_at_least_80": None if o is None else o >= ORACLE_MIN,
        }
        if "null" in agents:
            r["null_fidelity"] = null_fidelity(scored, cond, fam)
            r["checks"]["null_fidelity"] = r["null_fidelity"]["pass"]
        r["pass"] = all(v is not False for v in r["checks"].values()) and any(v is not None for v in r["checks"].values())
        return r

    out = {"families": {f: row(f) for f in fams}, "pooled": row(None)}
    out["pass"] = out["pooled"]["pass"] and all(r["pass"] or all(v is None for v in r["checks"].values())
                                                for r in out["families"].values())
    return out


def null_fidelity(scored: list[dict[str, Any]], cond: str, fam: str | None) -> dict[str, Any]:
    """4.5: each `null` episode against its `none` replicate (same situation, replicate and
    condition): the fidelity digests equal (truth, picture, audit bar `agent.action`), the same
    needs and needs served, no action of its own."""
    nones = {(e["situation"], int(e.get("r") or 0)): e for e in scored
             if e["agent"] == "none" and cond_key(e) == cond}
    pairs = differ = unpaired = 0
    diffs: list[dict[str, Any]] = []
    for e in scored:
        if e["agent"] != "null" or cond_key(e) != cond or not e.get("base"):
            continue
        b = base_name(e["base"])
        if (fam is None and b in ALONE) or (fam is not None and b != fam):
            continue
        o = nones.get((e["situation"], int(e.get("r") or 0)))
        if o is None:
            unpaired += 1
            continue
        pairs += 1
        bad = [k for k in ("truth", "picture", "audit") if (e.get("fidelity") or {}).get(k) is None
               or (e.get("fidelity") or {}).get(k) != (o.get("fidelity") or {}).get(k)]
        m, mo = e.get("metrics") or {}, o.get("metrics") or {}
        for k in ("needs", "eligible", "served", "fault_met"):
            if m.get(k) != mo.get(k):
                bad.append(k)
        if m.get("actions"):
            bad.append("actions")
        if bad:
            differ += 1
            diffs.append({"situation": e["situation"], "r": e.get("r"), "differs": bad})
    return {"pairs": pairs, "differ": differ, "unpaired": unpaired, "pass": pairs > 0 and differ == 0,
            "differences": diffs[:20]}


def holm_family(agg: dict[str, Any], primary: tuple[str, str], operator: str) -> dict[str, Any]:
    """8.3: Holm across the secondary family (pooled over families): each agent minus none on
    3.1, and the primary pair's differences on the secondary metrics, every condition."""
    ps: dict[str, float | None] = {}
    for cond, block in (agg.get("pooled") or {}).items():
        for pair, comps in block["comparisons"].items():
            for mn, c in comps.items():
                if pair.endswith("- none") and mn != "served_share":
                    continue
                if pair == f"{primary[0]} - {primary[1]}" and mn == "served_share" and cond.startswith(f"{operator};"):
                    continue  # the primary itself, tested alone
                ps[f"{cond} {pair} {mn}"] = c.get("p")
    return S.holm(ps)


def multiverse(scored: list[dict[str, Any]], primary: tuple[str, str], r0_only: tuple[str, ...], seed: int) -> dict[str, Any]:
    """8.5: every comparison (each agent minus none, the primary pair) on each dimension's
    metrics, in each of its cells; robust if no cell's interval excludes zero the other way."""
    out: dict[str, Any] = {}
    cell_names = sorted({c for e in scored for c in (e.get("cells") or {})})
    if not cell_names:
        return out
    for cond in sorted({cond_key(e) for e in scored}):
        agents = sorted({e["agent"] for e in scored if cond_key(e) == cond and e.get("agent")})
        pairs = [(a, "none") for a in agents if a != "none" and "none" in agents]
        if primary[0] in agents and primary[1] in agents:
            pairs.append(primary)
        for a, b in pairs:
            for dim, (vals, mets) in MULTIVERSE.items():
                for mn in mets:
                    res: dict[str, dict[str, Any]] = {}
                    for v in vals:
                        name = f"{dim}={v:g}"
                        cell = name if name in cell_names else "default"
                        res[name] = comparison(scored, a, b, cond, mn, None, a in r0_only and (a, b) == primary,
                                               cell=cell, seed=seed)
                    default = next((c for n, c in res.items() if n.endswith(f"={_default(dim):g}")), None)
                    est = (default or {}).get("mean_difference")
                    direction = 0 if not est else (1 if est > 0 else -1)
                    verdict = S.multiverse(res, direction) if direction else {"verdict": "no difference in the default cell"}
                    out[f"{cond} {a} - {b} {mn} over {dim}"] = {
                        "cells": {n: {k: c.get(k) for k in ("mean_difference", "lo", "hi", "n_situations", "method")}
                                  for n, c in res.items()}, **verdict}
    return out


def _default(dim: str) -> float:
    return float(getattr(Params(), dim))
