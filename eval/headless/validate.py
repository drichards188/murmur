"""The validation (eval/SCORING-V2.md 10.2, read under relative comparison as sections 0.4 and 0.5
state it) on one scoring v2 batch, with the scorer's own labels, metrics and paired comparisons
(v2/aggregate.py), as eval/headless/ladder.py reads the ladder. MODEL RESULTS: a scripted operator
on generated situations; never human data.

    .venv/bin/python -m eval.headless.validate eval/results/headless/v2-revalidate-2000             # 0.5
    .venv/bin/python -m eval.headless.validate eval/results/headless/v2-validate-1500 --section 0.4

`--section` picks the pre-registration the batch is judged by (`SECTIONS`): `0.4` as it ran on
seeds 1500-1599 (kept so that run can be re-read exactly), `0.5` (the default) the revalidation
after labels v2 on seeds 2000-2099. Per condition and family:

- **The anchors' order**: section 5's expected order with the playbook taken out, each a chain from
  the lowest value to the highest. Each link is a paired difference, higher minus lower
  (`comparison`: the paired cluster bootstrap, BCa, seed 0, at 20 situations or more, else the exact
  sign test), judged by 0.3's rule: `holds` wholly above zero, `tie` containing it (for a strict link
  the advantage is not shown), `fails` wholly below (a reversal). An order `holds` when no link fails
  and its ends' interval lies wholly in the stated direction (10.2), is `not shown` when nothing is
  reversed but the ends' interval contains zero, `fails` on a reversal, and is `not testable` where
  the metric has no value for an agent in it (no eligible need, no fault need). Judged in the
  admissible families (3.1's rule on this batch's none and oracle, `admissibility`), reported in
  every family. 0.5 judges every order of 0.4 but cards and harm, which it reports (`REPORTED`),
  and judges useless as a share of the actions with an outcome label (`useless_share`), per hour
  reported (`ANCHOR_ORDERS_05`).
- **The perturbations**: each against its base (0.4: the plain playbook; 0.5: `oracle-honest`, the
  playbook's perturbations reported beside, not judged) on its metrics (`MOVES`): `up` or `down` is
  `moved` when the paired interval lies wholly in that direction, `moved the wrong way` when wholly
  in the other, else `did not move`; `unchanged` is `unchanged` when the interval lies inside the
  equivalence margin (5 points of a share, 0.25 cards per 10 min), `changed` when it excludes zero,
  else `inconclusive`. A predicted fall is `not testable` where the base is not shown above none on
  that metric (the base minus none interval not wholly above zero): there is nothing the
  perturbation is known to take away; a fall shown there is still reported as `moved`. A metric with
  no value in a family is `not testable` there. 0.4 sets the conflict family aside from need served.
  0.5 (re-specified) judges each move once, pooled over the families with eligible needs
  (`pooled_move`: paired by situation, the cluster bootstrap over situations stratified by family,
  stats.py `stratified_bca`), the room rule on the pooled base minus none; per family the moves are
  reported. Spam's need served is non-inferiority there (`not worse`: the interval's lower bound
  above minus 5 points), not two-sided equivalence.
- **The ladder** (0.5): 0.3's predicted order of the rungs, re-checked by 0.3's rule (ladder.py).
- **Null fidelity** (4.5): `null` against its `none` replicate (aggregate.py `null_fidelity`).
- **The world's conditions** (0.5, 10.1 as 0.4 left it): in each family with eligible needs, `none`
  serves at most 50 % and the oracle at least 80 % (pooled shares).

**What blocks the freeze** (0.5, `blocks`): an anchor order that fails or is not shown in an
admissible family; a pooled move that did not move, moved the wrong way, or (spam's need served)
is not shown not worse; a pooled move not testable (an untested check is not a pass); a judged
move whose per-family interval lies wholly the wrong way (for spam's need served, wholly below
minus 5 points) in any family with eligible needs, the pooled move notwithstanding (`WRONG_WAY`);
a ladder reversal; a null fidelity difference; a family with eligible needs where none serves over
50 % or the oracle under 80 %.

Writes `validate.json` and `validate.md` beside the batch's records.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

from . import ladder as LD
from .ladder import families_with_needs
from .v2 import stats as S
from .v2.aggregate import (ALONE, NONE_MAX, ORACLE_MIN, admissibility, base_name, comparison, cond_key,
                           excluded_situations, null_fidelity, pooled_share, score, situations, values)
from .v2.labels import Params

#: section 5's expected order on the anchors, the playbook taken out (0.4): per metric, the chain
#: from the lowest value to the highest, `<` strict and `<=` weak; a link through the playbook keeps
#: what section 5's chain implies (strict if either of its two links was)
ANCHOR_ORDERS: tuple[tuple[str, tuple[str, ...]], ...] = (
    ("served_share", ("none", "<=", "random-valid", "<", "oracle")),
    ("per_hour.useful", ("none", "<=", "random-valid", "<", "oracle")),
    ("per_hour.useless", ("oracle", "<", "random-valid")),
    ("per_hour.harmful", ("oracle", "<", "random-valid")),
    ("cards_per_10min", ("none", "<", "oracle", "<", "random-valid")),
    ("rmst", ("oracle", "<", "random-valid", "<=", "none")),
    ("fault_met_share", ("none", "<", "oracle")),
)
#: 0.5's anchor orders: 0.4's, with useless judged as a share of the actions with an outcome label
#: (`useless_share`, metrics.py) and reported per hour (0.5 item 5, re-specified as the cards
#: order was: a rate per hour mostly measures how much an agent acts)
ANCHOR_ORDERS_05: tuple[tuple[str, tuple[str, ...]], ...] = (
    ("served_share", ("none", "<=", "random-valid", "<", "oracle")),
    ("per_hour.useful", ("none", "<=", "random-valid", "<", "oracle")),
    ("useless_share", ("oracle", "<", "random-valid")),
    ("per_hour.useless", ("oracle", "<", "random-valid")),
    ("per_hour.harmful", ("oracle", "<", "random-valid")),
    ("cards_per_10min", ("none", "<", "oracle", "<", "random-valid")),
    ("rmst", ("oracle", "<", "random-valid", "<=", "none")),
    ("fault_met_share", ("none", "<", "oracle")),
)
#: 0.5: orders reported, never judged: cards (mis-specified in 0.4: the oracle acts more, and cards
#: are a cost, not a quality ranking), harm (no anchor harms; harm is judged on the perturbations)
#: and useless per hour (judged as a share instead)
REPORTED = ("per_hour.harmful", "cards_per_10min", "per_hour.useless")
#: section 5's perturbations: what each must move against the plain playbook. `unchanged` on need
#: served stands for "useful unchanged" (0.4: section 5 gives no margin for a rate per hour; useful
#: per hour is reported beside it)
MOVES: dict[str, tuple[tuple[str, str], ...]] = {
    "playbook+spam": (("served_share", "unchanged"), ("per_hour.follow_up", "up"), ("repeats", "up"),
                      ("cards_per_10min", "up")),
    "playbook+behind": (("served_share", "down"), ("per_hour.useless", "up")),
    "playbook+skip-first": (("served_share", "down"),),
    "playbook+reserve-blind": (("fault_met_share", "down"),),
    "playbook+ferries": (("per_hour.useless", "up"), ("bad_card_share", "up")),
}
#: 0.5: the same moves on the honest oracle (the realistic ceiling: room for a fall), and the
#: harm-inducing pull-off. reserve-blind's harmful (a) is reported, not judged: the bridge's
#: reserves check refuses any order that would leave a Puma under 20 % after 900 s on task, so no
#: harmful (a) order can be dispatched through the C2 (agents_v2.py)
HONEST_MOVES: dict[str, tuple[tuple[str, str], ...]] = {
    "oracle-honest+spam": (("served_share", "not worse"), ("per_hour.follow_up", "up"), ("repeats", "up"),
                           ("cards_per_10min", "up")),
    "oracle-honest+behind": (("served_share", "down"), ("per_hour.useless", "up")),
    "oracle-honest+skip-first": (("served_share", "down"),),
    "oracle-honest+reserve-blind": (("fault_met_share", "down"),),
    "oracle-honest+ferries": (("per_hour.useless", "up"), ("bad_card_share", "up")),
    "oracle-honest+pull-off": (("served_share", "down"), ("per_hour.harmful_b", "up")),
}
#: reported beside each perturbation, never judged
DESCRIPTIVE: dict[str, tuple[str, ...]] = {
    "playbook+spam": ("per_hour.useful",),
    "playbook+behind": ("rmst",),
    "playbook+skip-first": ("rmst",),
    "playbook+reserve-blind": ("per_hour.harmful_a", "per_hour.fault_response"),
    "playbook+ferries": ("cards_per_10min",),
    "oracle-honest+spam": ("per_hour.useful", "per_hour.redundant", "per_hour.refused"),
    "oracle-honest+behind": ("rmst",),
    "oracle-honest+skip-first": ("rmst",),
    "oracle-honest+reserve-blind": ("per_hour.harmful_a", "per_hour.refused", "per_hour.fault_actions"),
    "oracle-honest+ferries": ("cards_per_10min",),
    "oracle-honest+pull-off": ("per_hour.harmful", "rmst"),
}
#: "unchanged": section 5's equivalence margins
MARGINS = {"served_share": 0.05, "cards_per_10min": 0.25}
#: need served is set aside in these families (0.4)
SET_ASIDE = {"conflict": ("served_share", "rmst", "per_hour.useful")}
#: each pre-registration: its anchor orders (judged, and reported only), its perturbations (judged
#: against a base, and reported against another), whether the ladder and the world's conditions are
#: part of it
SECTIONS: dict[str, dict[str, Any]] = {
    "0.4": {"orders": ANCHOR_ORDERS, "reported": (), "judged": ("playbook", MOVES), "reported_moves": None,
            "ladder": False, "world": False, "pooled": False},
    "0.5": {"orders": ANCHOR_ORDERS_05, "reported": REPORTED, "judged": ("oracle-honest", HONEST_MOVES),
            "reported_moves": ("playbook", MOVES), "ladder": True, "world": True, "pooled": True},
}
DEFAULT_SECTION = "0.5"
#: a judged move's verdicts that are failures
MOVE_FAILS = ("did not move", "moved the wrong way", "changed", "inconclusive", "not shown not worse", "worse")
#: 0.5's guard: a judged move's per-family verdicts that block although the pooled move holds
WRONG_WAY = ("moved the wrong way", "worse")


def link_verdict(c: dict[str, Any]) -> str:
    """0.3's rule on a paired difference, higher minus lower (predicted at least zero)."""
    lo, hi = c.get("lo"), c.get("hi")
    if lo is None or hi is None:
        return "no data"
    if hi < 0:
        return "fails"
    if lo > 0:
        return "holds"
    return "tie"


def _cmp(scored: list[dict[str, Any]], a: str, b: str, cond: str, metric: str, fam: str, seed: int) -> dict[str, Any]:
    c = comparison(scored, a, b, cond, metric, [fam], seed=seed)
    return {k: c.get(k) for k in ("mean_difference", "lo", "hi", "n_situations", "method", "excluded_share")}


def _mean(scored: list[dict[str, Any]], agent: str, cond: str, metric: str, fam: str) -> float | None:
    vs = [v for v in values(situations(scored, agent, cond, [fam]), metric).values() if v is not None]
    return round(sum(vs) / len(vs), 6) if vs else None


def anchor_order(scored: list[dict[str, Any]], cond: str, fam: str, metric: str, chain: tuple[str, ...],
                 agents: set[str], seed: int = 0) -> dict[str, Any]:
    names, rels = list(chain[0::2]), list(chain[1::2])
    out: dict[str, Any] = {"metric": metric, "order": " ".join(chain), "means": {}}
    if any(n not in agents for n in names):
        return {**out, "verdict": "not run"}
    out["means"] = {n: _mean(scored, n, cond, metric, fam) for n in names}
    if fam in SET_ASIDE and metric in SET_ASIDE[fam]:
        return {**out, "verdict": "set aside (0.4)"}
    if any(v is None for v in out["means"].values()):
        return {**out, "verdict": "not testable"}
    links = []
    for lo_, rel, hi_ in zip(names, rels, names[1:]):
        c = _cmp(scored, hi_, lo_, cond, metric, fam, seed)
        links.append({"higher": hi_, "lower": lo_, "strict": rel == "<", "verdict": link_verdict(c), **c})
    ends = _cmp(scored, names[-1], names[0], cond, metric, fam, seed)
    out.update(links=links, ends={"higher": names[-1], "lower": names[0], "verdict": link_verdict(ends), **ends})
    if any(x["verdict"] == "fails" for x in links) or out["ends"]["verdict"] == "fails":
        out["verdict"] = "fails"
    elif out["ends"]["verdict"] == "holds":
        out["verdict"] = "holds"
    elif out["ends"]["verdict"] == "no data":
        out["verdict"] = "not testable"
    else:
        out["verdict"] = "not shown"
    return out


def _equivalence(want: str, metric: str, lo: float, hi: float) -> dict[str, Any]:
    """`unchanged` (0.4: two-sided, the interval inside the margin) or `not worse` (0.5,
    non-inferiority: the interval's lower bound above minus the margin)."""
    m = MARGINS[metric]
    if want == "unchanged":
        return {"margin": m, "verdict": "unchanged" if -m <= lo and hi <= m else
                ("changed" if lo > 0 or hi < 0 else "inconclusive")}
    return {"margin": m, "verdict": "not worse" if lo > -m else ("worse" if hi < -m else "not shown not worse")}


def _diffs(scored: list[dict[str, Any]], a: str, b: str, cond: str, metric: str, fam: str) -> list[float]:
    """a minus b on one metric per situation of a family, paired, the situations section 7 leaves
    out (an episode of either not scored) left out."""
    va = values(situations(scored, a, cond, [fam]), metric)
    vb = values(situations(scored, b, cond, [fam]), metric)
    out_s = excluded_situations(scored, (a, b), cond, [fam])
    return [va[s] - vb[s] for s in sorted(set(va) & set(vb), key=str)
            if va[s] is not None and vb[s] is not None and s not in out_s]


def _pooled(scored: list[dict[str, Any]], a: str, b: str, cond: str, metric: str, fams: list[str],
            seed: int) -> dict[str, Any]:
    """a minus b pooled over families: the mean over every paired situation, its interval the
    cluster bootstrap over situations stratified by family (stats.py `stratified_bca`)."""
    strata = {f: _diffs(scored, a, b, cond, metric, f) for f in fams}
    c = S.stratified_bca({f: d for f, d in strata.items() if d}, seed=seed)
    return {"mean_difference": c.get("estimate"), "lo": c.get("lo"), "hi": c.get("hi"), "n_situations": c.get("n"),
            "method": "paired cluster bootstrap stratified by family (BCa)", "families": c.get("strata")}


def pooled_move(scored: list[dict[str, Any]], cond: str, fams: list[str], agent: str, metric: str, want: str,
                agents: set[str], seed: int = 0, base: str = "oracle-honest") -> dict[str, Any]:
    """0.5: a move judged pooled over the families with eligible needs, by `move`'s verdicts; a
    predicted fall is not testable where the base is not shown above none on the pooled interval
    (unless it shows anyway)."""
    fams = [f for f in fams if not (f in SET_ASIDE and metric in SET_ASIDE[f])]
    out: dict[str, Any] = {"metric": metric, "predicted": want, "base": base, "families": fams}
    c = _pooled(scored, agent, base, cond, metric, fams, seed)
    out["difference"] = c
    lo, hi = c.get("lo"), c.get("hi")
    if lo is None or hi is None:
        return {**out, "verdict": "not testable", "why": "no paired situations with a value"}
    if want in ("unchanged", "not worse"):
        return {**out, **_equivalence(want, metric, lo, hi)}
    up = want == "up"
    if (lo > 0) if up else (hi < 0):
        out["verdict"] = "moved"
    elif (hi < 0) if up else (lo > 0):
        out["verdict"] = "moved the wrong way"
    else:
        out["verdict"] = "did not move"
    if not up and "none" in agents:
        room = _pooled(scored, base, "none", cond, metric, fams, seed)
        out[f"{base}_minus_none"] = room
        if out["verdict"] == "did not move" and not (room.get("lo") is not None and room["lo"] > 0):
            out["verdict"] = "not testable"
            out["why"] = f"{base} is not shown above none: nothing known for it to take away"
    return out


def move(scored: list[dict[str, Any]], cond: str, fam: str, agent: str, metric: str, want: str,
         agents: set[str], seed: int = 0, base: str = "playbook") -> dict[str, Any]:
    out: dict[str, Any] = {"metric": metric, "predicted": want, "base": base,
                           "means": {a: _mean(scored, a, cond, metric, fam) for a in ("none", base, agent)
                                     if a in agents}}
    if fam in SET_ASIDE and metric in SET_ASIDE[fam]:
        return {**out, "verdict": "set aside (0.4)"}
    if out["means"].get(agent) is None or out["means"].get(base) is None:
        return {**out, "verdict": "not testable", "why": "no value in this family"}
    c = _cmp(scored, agent, base, cond, metric, fam, seed)
    out["difference"] = c
    lo, hi = c.get("lo"), c.get("hi")
    if lo is None or hi is None:
        return {**out, "verdict": "not testable", "why": "no paired situations"}
    if want in ("unchanged", "not worse"):
        return {**out, **_equivalence(want, metric, lo, hi)}
    up = want == "up"
    if (lo > 0) if up else (hi < 0):
        out["verdict"] = "moved"
    elif (hi < 0) if up else (lo > 0):
        out["verdict"] = "moved the wrong way"
    else:
        out["verdict"] = "did not move"
    if not up and "none" in agents:
        room = _cmp(scored, base, "none", cond, metric, fam, seed)
        out[f"{base}_minus_none"] = room
        if out["verdict"] == "did not move" and not (room.get("lo") is not None and room["lo"] > 0):
            out["verdict"] = "not testable"
            out["why"] = f"{base} is not shown above none here: nothing known for it to take away"
    return out


def _moves(scored: list[dict[str, Any]], cond: str, fam: str, agents: set[str], base: str,
           moves: dict[str, tuple[tuple[str, str], ...]], seed: int) -> dict[str, Any]:
    out = {}
    for p, ms in moves.items():
        if p not in agents or base not in agents:
            continue
        out[p] = {"base": base,
                  "moves": [move(scored, cond, fam, p, mn, want, agents, seed, base) for mn, want in ms],
                  "beside": {mn: {"means": {a: _mean(scored, a, cond, mn, fam) for a in (base, p)},
                                  "difference": _cmp(scored, p, base, cond, mn, fam, seed)}
                             for mn in DESCRIPTIVE.get(p, ())}}
    return out


def world(scored: list[dict[str, Any]], cond: str) -> dict[str, Any]:
    """10.1's conditions as 0.4 left them, per family with eligible needs: none at most 50 %, the
    oracle at least 80 % (pooled shares)."""
    out = {}
    for fam in families_with_needs(scored, cond):
        n, o = (pooled_share(scored, a, cond, fam)["share"] for a in ("none", "oracle"))
        out[fam] = {"none": n, "oracle": o, "none_at_most_50": None if n is None else n <= NONE_MAX,
                    "oracle_at_least_80": None if o is None else o >= ORACLE_MIN}
    return out


def check(scored: list[dict[str, Any]], seed: int = 0, section: str = DEFAULT_SECTION) -> dict[str, Any]:
    sec = SECTIONS[section]
    base, judged = sec["judged"]
    out: dict[str, Any] = {}
    for cond in sorted({cond_key(e) for e in scored}):
        agents = {e["agent"] for e in scored if cond_key(e) == cond and e.get("agent")}
        fams = sorted({base_name(e["base"]) for e in scored if e.get("base") and cond_key(e) == cond} - set(ALONE))
        adm = admissibility(scored, cond)
        block: dict[str, Any] = {"section": section, "agents": sorted(agents),
                                 "families_with_needs": families_with_needs(scored, cond),
                                 "admissible": [f for f in fams if (adm.get(f) or {}).get("admissible") is True],
                                 "admissibility": adm, "families": {}}
        for fam in fams:
            row: dict[str, Any] = {"anchors": [anchor_order(scored, cond, fam, mn, ch, agents, seed)
                                               for mn, ch in sec["orders"]],
                                   "perturbations": _moves(scored, cond, fam, agents, base, judged, seed)}
            for a in row["anchors"]:
                a["judged"] = a["metric"] not in sec["reported"]
            if sec["reported_moves"] is not None:
                rb, rm = sec["reported_moves"]
                row["reported_perturbations"] = _moves(scored, cond, fam, agents, rb, rm, seed)
            row["null_fidelity"] = null_fidelity(scored, cond, fam) if "null" in agents else None
            block["families"][fam] = row
        block["null_fidelity"] = null_fidelity(scored, cond, None) if "null" in agents else None
        judged_orders = [(f, a) for f in block["admissible"] for a in block["families"][f]["anchors"] if a["judged"]]
        block["anchor_failures"] = [{"family": f, "metric": a["metric"], "verdict": a["verdict"]} for f, a in judged_orders
                                    if a["verdict"] in ("fails", "not shown")]
        if sec["pooled"]:
            # 0.5 (re-specified): each move judged once, pooled over the families with eligible
            # needs (paired by situation, the bootstrap stratified by family); per family reported
            needy = block["families_with_needs"]
            for r in block["families"].values():
                for x in r["perturbations"].values():
                    for m in x["moves"]:
                        m["judged"] = False
            block["pooled_moves"] = {p: [pooled_move(scored, cond, needy, p, mn, want, agents, seed, base) for mn, want in ms]
                                     for p, ms in judged.items() if p in agents and base in agents}
            rb, rm = sec["reported_moves"]
            block["pooled_reported_moves"] = {p: [pooled_move(scored, cond, needy, p, mn, want, agents, seed, rb)
                                                  for mn, want in ms]
                                              for p, ms in rm.items() if p in agents and rb in agents}
            block["perturbation_failures"] = [
                {"family": "pooled", "perturbation": p, "metric": m["metric"], "verdict": m["verdict"]}
                for p, ms in block["pooled_moves"].items() for m in ms if m["verdict"] in MOVE_FAILS]
            block["untested"] = [{"perturbation": p, "metric": m["metric"]} for p, ms in block["pooled_moves"].items()
                                 for m in ms if m["verdict"] in ("not testable", "set aside (0.4)")]
            # the guard (0.5): a judged move whose per-family interval lies wholly the wrong way (or,
            # for spam's need served, wholly below the margin) in any family with eligible needs
            block["wrong_way"] = [
                {"family": f, "perturbation": p, "metric": m["metric"], "verdict": m["verdict"]}
                for f in needy for p, x in block["families"][f]["perturbations"].items() for m in x["moves"]
                if m["verdict"] in WRONG_WAY]
        else:
            # 0.4 as it ran: every family judged
            for r in block["families"].values():
                for x in r["perturbations"].values():
                    for m in x["moves"]:
                        m["judged"] = True
            block["perturbation_failures"] = [
                {"family": f, "perturbation": p, "metric": m["metric"], "verdict": m["verdict"]}
                for f, r in block["families"].items() for p, x in r["perturbations"].items() for m in x["moves"]
                if m["verdict"] in MOVE_FAILS]
            tested: dict[tuple[str, str], bool] = {}
            for r in block["families"].values():
                for p, x in r["perturbations"].items():
                    for m in x["moves"]:
                        k = (p, m["metric"])
                        tested[k] = tested.get(k, False) or m["verdict"] not in ("not testable", "set aside (0.4)")
            block["untested"] = [{"perturbation": p, "metric": mn} for (p, mn), t in sorted(tested.items()) if not t]
        if sec["ladder"]:
            lad = LD.check([e for e in scored if cond_key(e) == cond]).get(cond) or {}
            block["ladder"] = {"failures": lad.get("failures") or [], "passed": lad.get("passed"),
                               "families": {f: r.get("predictions") for f, r in (lad.get("families") or {}).items()}}
        if sec["world"]:
            block["world"] = world(scored, cond)
        block["blocks"] = blocks(block, section)
        out[cond] = block
    return out


def blocks(block: dict[str, Any], section: str) -> list[str]:
    """0.5's list of what blocks the freeze, each in words (0.4 had none: its failures were the
    record)."""
    if section == "0.4":
        return []
    out = [f"anchor order {x['metric']} {x['verdict']} in {x['family']}" for x in block["anchor_failures"]]
    out += [f"{x['perturbation']} {x['metric']} {x['verdict']} ({x['family']})" for x in block["perturbation_failures"]]
    out += [f"{x['perturbation']} {x['metric']} not testable" for x in block["untested"]]
    out += [f"{x['perturbation']} {x['metric']} {x['verdict']} in {x['family']} (per family)"
            for x in block.get("wrong_way") or []]
    out += [f"ladder: {x['a']} below {x['b']} in {x['family']}" for x in (block.get("ladder") or {}).get("failures") or []]
    nf = block.get("null_fidelity")
    if nf is not None and not nf["pass"]:
        out.append(f"null fidelity: {nf['differ']} of {nf['pairs']} pairs differ")
    for fam, w in (block.get("world") or {}).items():
        if w["none_at_most_50"] is False:
            out.append(f"world: none serves {w['none']:.0%} in {fam}")
        if w["oracle_at_least_80"] is False:
            out.append(f"world: the oracle serves {w['oracle']:.0%} in {fam}")
    return out


def _num(x: float | None, scale: float = 1.0, nd: int = 2) -> str:
    return "n/a" if x is None else f"{scale * x:.{nd}f}"


def _scale(metric: str) -> tuple[float, int, str]:
    if metric.endswith("_share"):
        return 100.0, 1, "pts"
    if metric == "rmst":
        return 1.0, 1, "s"
    return 1.0, 2, ""


def _diff(c: dict[str, Any] | None, metric: str) -> str:
    if not c or c.get("mean_difference") is None:
        return "n/a"
    s, nd, unit = _scale(metric)
    return f"{_num(c['mean_difference'], s, nd)} [{_num(c.get('lo'), s, nd)}, {_num(c.get('hi'), s, nd)}]{' ' + unit if unit else ''}"


def _move_rows(perts: dict[str, Any], judged: bool) -> list[str]:
    lines = []
    for p, x in perts.items():
        b = x["base"]
        for m in x["moves"]:
            s, nd, _ = _scale(m["metric"])
            lines.append(f"| {p} | {m['metric']} | {m['predicted']} | {_num(m['means'].get(b), s, nd)} "
                         f"| {_num(m['means'].get(p), s, nd)} | {_diff(m.get('difference'), m['metric'])} | "
                         f"{m['verdict'] if judged and m.get('judged', True) else m['verdict'] + ' (reported)'} |")
        for mn, d in x["beside"].items():
            s, nd, _ = _scale(mn)
            lines.append(f"| {p} | {mn} | (reported) | {_num(d['means'].get(b), s, nd)} | "
                         f"{_num(d['means'].get(p), s, nd)} | {_diff(d['difference'], mn)} | - |")
    return lines


def markdown(res: dict[str, Any]) -> str:
    lines = ["# The validation: anchors and perturbations", "",
             "Model results: a scripted operator on generated situations; not human data. Differences are paired by "
             "situation, with the scorer's 95 % interval (cluster bootstrap, BCa, seed 0; SCORING-V2.md 0.3, 0.4, 0.5, 5, "
             "10.2). Shares in points, RMST in seconds, rates per hour, cards per 10 min.", ""]
    for cond, block in res.items():
        lines += [f"## {cond} (section {block['section']})", "", f"Admissible families (3.1, this batch's none and oracle): "
                  f"{', '.join(block['admissible']) or 'none'}.", "",
                  f"Anchor orders failed or not shown in admissible families: {len(block['anchor_failures'])}. "
                  f"Perturbation moves not as predicted: {len(block['perturbation_failures'])}. "
                  f"Judged predictions not testable anywhere: {len(block['untested'])}.", ""]
        if block["section"] != "0.4":
            lines += [f"**Blocks the freeze: {len(block['blocks'])}**" + (":" if block["blocks"] else " (nothing).")]
            lines += [f"- {b}" for b in block["blocks"]]
            lines.append("")
        nf = block.get("null_fidelity")
        if nf:
            lines += [f"Null fidelity: {nf['pairs'] - nf['differ']} of {nf['pairs']} pairs identical "
                      f"({nf['unpaired']} unpaired): {'passed' if nf['pass'] else 'FAILED'}.", ""]
        if block.get("ladder") is not None:
            lad = block["ladder"]
            lines += [f"Ladder (0.3's rule): {'passed' if lad['passed'] else 'FAILED'}"
                      + (f", reversals: {[(x['family'], x['a'], x['b']) for x in lad['failures']]}" if lad["failures"] else "")
                      + ".", ""]
        if block.get("world"):
            lines += ["World (10.1): " + "; ".join(f"{f} none {_num(w['none'], 100, 1)} %, oracle {_num(w['oracle'], 100, 1)} %"
                                                   for f, w in block["world"].items()), ""]
        for key, title in (("pooled_moves", "The moves, judged pooled over the families with eligible needs"),
                           ("pooled_reported_moves", "The playbook's moves, pooled (reported, not judged)")):
            if not block.get(key):
                continue
            lines += [f"### {title}", "", "| perturbation | metric | predicted | base | perturbed minus base [95 % CI] | "
                      "situations | verdict |", "|---|---|---|---|---|---|---|"]
            for p, ms in block[key].items():
                for m in ms:
                    v = m["verdict"] if key == "pooled_moves" else f"{m['verdict']} (reported)"
                    lines.append(f"| {p} | {m['metric']} | {m['predicted']} | {m['base']} | "
                                 f"{_diff(m.get('difference'), m['metric'])} | {(m.get('difference') or {}).get('n_situations')} | {v} |")
            lines.append("")
        for fam, row in block["families"].items():
            lines += [f"### {fam}{'' if fam in block['admissible'] else ' (not admissible: reported, not judged)'}", "",
                      "| metric | order | means | ends: higher minus lower [95 % CI] | links | verdict |",
                      "|---|---|---|---|---|---|"]
            for a in row["anchors"]:
                means = ", ".join(f"{k} {_num(v, *_scale(a['metric'])[:2])}" for k, v in a["means"].items())
                links = "; ".join(f"{x['higher']} - {x['lower']}: {x['verdict']}"
                                  + (" (not shown)" if x["strict"] and x["verdict"] == "tie" else "")
                                  for x in a.get("links") or [])
                v = a["verdict"] if a.get("judged", True) else f"{a['verdict']} (reported, not judged)"
                lines.append(f"| {a['metric']} | {a['order']} | {means} | {_diff(a.get('ends'), a['metric'])} | "
                             f"{links or 'n/a'} | {v} |")
            head = ["", "| perturbation | metric | predicted | base | perturbed | perturbed minus base [95 % CI] | verdict |",
                    "|---|---|---|---|---|---|---|"]
            if row["perturbations"]:
                lines += head + _move_rows(row["perturbations"], True)
            if row.get("reported_perturbations"):
                lines += head + _move_rows(row["reported_perturbations"], False)
            lines.append("")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("batch", type=Path, help="a scoring v2 batch directory (its episodes_v2.json)")
    ap.add_argument("--section", choices=sorted(SECTIONS), default=DEFAULT_SECTION,
                    help="the pre-registration to judge by (SCORING-V2.md 0.4 or 0.5)")
    args = ap.parse_args(argv)
    metas = json.loads((args.batch / "episodes_v2.json").read_text())
    res = check(score(metas, Params(), multiverse=False), section=args.section)
    (args.batch / "validate.json").write_text(json.dumps(res, indent=1, sort_keys=True, default=str))
    md = markdown(res)
    (args.batch / "validate.md").write_text(md)
    print(md)
    if args.section == "0.4":
        return 0 if all(not b["anchor_failures"] and not b["perturbation_failures"] for b in res.values()) else 1
    return 0 if all(not b["blocks"] for b in res.values()) else 1


if __name__ == "__main__":
    raise SystemExit(main())
