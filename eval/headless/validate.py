"""The validation (eval/SCORING-V2.md 10.2, read under relative comparison as section 0.4 states it)
on one scoring v2 batch, with the scorer's own labels, metrics and paired comparisons
(v2/aggregate.py), as eval/headless/ladder.py reads the ladder. MODEL RESULTS: a scripted operator
on generated situations; never human data.

    .venv/bin/python -m eval.headless.validate eval/results/headless/v2-validate-1500

Per condition and family:

- **The anchors' order**: section 5's expected order with the playbook taken out (`ANCHOR_ORDERS`,
  each a chain from the lowest value to the highest). Each link is a paired difference, higher
  minus lower (`comparison`: the paired cluster bootstrap, BCa, seed 0, at 20 situations or more,
  else the exact sign test), judged by 0.3's rule: `holds` wholly above zero, `tie` containing it
  (for a strict link the advantage is not shown), `fails` wholly below (a reversal). An order
  `holds` when no link fails and its ends' interval lies wholly in the stated direction (10.2),
  is `not shown` when nothing is reversed but the ends' interval contains zero, `fails` on a
  reversal, and is `not testable` where the metric has no value for an agent in it (no eligible
  need, no fault need). Judged in the admissible families (3.1's rule on this batch's none and
  oracle, `admissibility`), reported in every family.
- **The perturbations**: each against the plain playbook on section 5's metrics (`MOVES`): `up`
  or `down` is `moved` when the paired interval lies wholly in that direction, `moved the wrong
  way` when wholly in the other, else `did not move`; `unchanged` is `unchanged` when the interval
  lies inside the equivalence margin (5 points of a share, 0.25 cards per 10 min), `changed` when it
  excludes zero, else `inconclusive`. A predicted fall is `not testable` where the playbook is not
  shown above none on that metric (the playbook minus none interval not wholly above zero): there
  is nothing the perturbation is known to take away; a fall shown there is still reported as
  `moved`. A metric with no value in a family is `not testable` there. 0.4 sets the conflict family
  aside from need served.
- **Null fidelity** (4.5): `null` against its `none` replicate (aggregate.py `null_fidelity`).

Writes `validate.json` and `validate.md` beside the batch's records.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

from .ladder import families_with_needs
from .v2.aggregate import ALONE, admissibility, base_name, comparison, cond_key, null_fidelity, score, situations, values
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
#: reported beside each perturbation, never judged
DESCRIPTIVE: dict[str, tuple[str, ...]] = {
    "playbook+spam": ("per_hour.useful",),
    "playbook+behind": ("rmst",),
    "playbook+skip-first": ("rmst",),
    "playbook+reserve-blind": ("per_hour.harmful_a", "per_hour.fault_response"),
    "playbook+ferries": ("cards_per_10min",),
}
#: "unchanged": section 5's equivalence margins
MARGINS = {"served_share": 0.05, "cards_per_10min": 0.25}
#: need served is set aside in these families (0.4)
SET_ASIDE = {"conflict": ("served_share", "rmst", "per_hour.useful")}


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


def move(scored: list[dict[str, Any]], cond: str, fam: str, agent: str, metric: str, want: str,
         agents: set[str], seed: int = 0) -> dict[str, Any]:
    out: dict[str, Any] = {"metric": metric, "predicted": want,
                           "means": {a: _mean(scored, a, cond, metric, fam) for a in ("none", "playbook", agent)
                                     if a in agents}}
    if fam in SET_ASIDE and metric in SET_ASIDE[fam]:
        return {**out, "verdict": "set aside (0.4)"}
    if out["means"].get(agent) is None or out["means"].get("playbook") is None:
        return {**out, "verdict": "not testable", "why": "no value in this family"}
    c = _cmp(scored, agent, "playbook", cond, metric, fam, seed)
    out["difference"] = c
    lo, hi = c.get("lo"), c.get("hi")
    if lo is None or hi is None:
        return {**out, "verdict": "not testable", "why": "no paired situations"}
    if want == "unchanged":
        m = MARGINS[metric]
        out["margin"] = m
        out["verdict"] = "unchanged" if -m <= lo and hi <= m else ("changed" if lo > 0 or hi < 0 else "inconclusive")
        return out
    up = want == "up"
    if (lo > 0) if up else (hi < 0):
        out["verdict"] = "moved"
    elif (hi < 0) if up else (lo > 0):
        out["verdict"] = "moved the wrong way"
    else:
        out["verdict"] = "did not move"
    if not up and "none" in agents:
        room = _cmp(scored, "playbook", "none", cond, metric, fam, seed)
        out["playbook_minus_none"] = room
        if out["verdict"] == "did not move" and not (room.get("lo") is not None and room["lo"] > 0):
            out["verdict"] = "not testable"
            out["why"] = "the playbook is not shown above none here: nothing known for it to take away"
    return out


def check(scored: list[dict[str, Any]], seed: int = 0) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for cond in sorted({cond_key(e) for e in scored}):
        agents = {e["agent"] for e in scored if cond_key(e) == cond and e.get("agent")}
        fams = sorted({base_name(e["base"]) for e in scored if e.get("base") and cond_key(e) == cond} - set(ALONE))
        adm = admissibility(scored, cond)
        block: dict[str, Any] = {"agents": sorted(agents), "families_with_needs": families_with_needs(scored, cond),
                                 "admissible": [f for f in fams if (adm.get(f) or {}).get("admissible") is True],
                                 "admissibility": adm, "families": {}}
        for fam in fams:
            row: dict[str, Any] = {"anchors": [anchor_order(scored, cond, fam, mn, ch, agents, seed) for mn, ch in ANCHOR_ORDERS],
                                   "perturbations": {}}
            for p, moves in MOVES.items():
                if p not in agents:
                    continue
                row["perturbations"][p] = {
                    "moves": [move(scored, cond, fam, p, mn, want, agents, seed) for mn, want in moves],
                    "beside": {mn: {"means": {a: _mean(scored, a, cond, mn, fam) for a in ("playbook", p)},
                                    "difference": _cmp(scored, p, "playbook", cond, mn, fam, seed)}
                               for mn in DESCRIPTIVE.get(p, ())}}
            row["null_fidelity"] = null_fidelity(scored, cond, fam) if "null" in agents else None
            block["families"][fam] = row
        block["null_fidelity"] = null_fidelity(scored, cond, None) if "null" in agents else None
        judged = [(f, a) for f in block["admissible"] for a in block["families"][f]["anchors"]]
        block["anchor_failures"] = [{"family": f, "metric": a["metric"], "verdict": a["verdict"]} for f, a in judged
                                    if a["verdict"] in ("fails", "not shown")]
        block["perturbation_failures"] = [
            {"family": f, "perturbation": p, "metric": m["metric"], "verdict": m["verdict"]}
            for f, r in block["families"].items() for p, x in r["perturbations"].items() for m in x["moves"]
            if m["verdict"] in ("did not move", "moved the wrong way", "changed", "inconclusive")]
        out[cond] = block
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


def markdown(res: dict[str, Any]) -> str:
    lines = ["# The validation: anchors and perturbations", "",
             "Model results: a scripted operator on generated situations; not human data. Differences are paired by "
             "situation, with the scorer's 95 % interval (cluster bootstrap, BCa, seed 0; SCORING-V2.md 0.3, 0.4, 5, "
             "10.2). Shares in points, RMST in seconds, rates per hour, cards per 10 min.", ""]
    for cond, block in res.items():
        lines += [f"## {cond}", "", f"Admissible families (3.1, this batch's none and oracle): "
                  f"{', '.join(block['admissible']) or 'none'}.", "",
                  f"Anchor orders failed or not shown in admissible families: {len(block['anchor_failures'])}. "
                  f"Perturbation moves not as predicted: {len(block['perturbation_failures'])}.", ""]
        nf = block.get("null_fidelity")
        if nf:
            lines += [f"Null fidelity: {nf['pairs'] - nf['differ']} of {nf['pairs']} pairs identical "
                      f"({nf['unpaired']} unpaired): {'passed' if nf['pass'] else 'FAILED'}.", ""]
        for fam, row in block["families"].items():
            lines += [f"### {fam}{'' if fam in block['admissible'] else ' (not admissible: reported, not judged)'}", "",
                      "| metric | order | means | ends: higher minus lower [95 % CI] | links | verdict |",
                      "|---|---|---|---|---|---|"]
            for a in row["anchors"]:
                means = ", ".join(f"{k} {_num(v, *_scale(a['metric'])[:2])}" for k, v in a["means"].items())
                links = "; ".join(f"{x['higher']} - {x['lower']}: {x['verdict']}"
                                  + (" (not shown)" if x["strict"] and x["verdict"] == "tie" else "")
                                  for x in a.get("links") or [])
                lines.append(f"| {a['metric']} | {a['order']} | {means} | {_diff(a.get('ends'), a['metric'])} | "
                             f"{links or 'n/a'} | {a['verdict']} |")
            if row["perturbations"]:
                lines += ["", "| perturbation | metric | predicted | playbook | perturbed | perturbed minus playbook "
                          "[95 % CI] | verdict |", "|---|---|---|---|---|---|---|"]
                for p, x in row["perturbations"].items():
                    for m in x["moves"]:
                        s, nd, _ = _scale(m["metric"])
                        lines.append(f"| {p} | {m['metric']} | {m['predicted']} | {_num(m['means'].get('playbook'), s, nd)} "
                                     f"| {_num(m['means'].get(p), s, nd)} | {_diff(m.get('difference'), m['metric'])} | "
                                     f"{m['verdict']} |")
                    for mn, b in x["beside"].items():
                        s, nd, _ = _scale(mn)
                        lines.append(f"| {p} | {mn} | (reported) | {_num(b['means'].get('playbook'), s, nd)} | "
                                     f"{_num(b['means'].get(p), s, nd)} | {_diff(b['difference'], mn)} | - |")
            lines.append("")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("batch", type=Path, help="a scoring v2 batch directory (its episodes_v2.json)")
    args = ap.parse_args(argv)
    metas = json.loads((args.batch / "episodes_v2.json").read_text())
    res = check(score(metas, Params(), multiverse=False))
    (args.batch / "validate.json").write_text(json.dumps(res, indent=1, sort_keys=True, default=str))
    md = markdown(res)
    (args.batch / "validate.md").write_text(md)
    print(md)
    return 0 if all(not b["anchor_failures"] and not b["perturbation_failures"] for b in res.values()) else 1


if __name__ == "__main__":
    raise SystemExit(main())
