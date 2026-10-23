"""The ruler check (eval/SCORING-V2.md 0.3): the ladder of handicapped oracles on one scoring v2
batch, read with the scorer's own labels, metrics and paired comparisons (v2/aggregate.py). MODEL
RESULTS: a scripted operator on generated situations; never human data.

    .venv/bin/python -m eval.headless.ladder eval/results/headless/v2-ladder-1100

For each condition and each family, and pooled over the families with eligible needs: every agent's need
served (served over eligible needs, pooled; and the mean over situations with its cluster
bootstrap interval), its time to serve (the restricted mean, 3.2) and cards per 10 minutes; then
each predicted order of the ladder as a paired difference on need served (`comparison`: the paired
cluster bootstrap, BCa, seed 0), judged by 0.3's rule:

- `holds`: the interval of A minus B lies above zero;
- `tie`: it contains zero (a tie is not a violation; for a strict prediction, A over none, the
  advantage is then not shown);
- `fails`: it lies below zero, a reversal outside its interval.

The check passes when no judged prediction fails in any family with needs. The pooled row is
reported, never judged. `--rule` picks the orders judged (`RULES`): `0.3` judges every one, as 0.3,
0.4 and 0.5 ran (kept so those runs can be re-read exactly); `0.6` (the default) computes and
reports the orders in `NOT_JUDGED` but does not judge them: the truth step in stuck-vehicle, where
the rungs' handicaps interact (SCORING-V2.md 0.6). Every other order is judged in every family.
Agents the batch did not run are left out of the predictions that name them. Writes `ladder.json`
and `ladder.md` beside the batch's records.
"""

from __future__ import annotations

import argparse
import json
import statistics
from pathlib import Path
from typing import Any

from .v2 import metrics as M
from .v2.aggregate import ALONE, base_name, comparison, cond_key, pooled_share, score, situations, values
from .v2.labels import Params

RUNGS = ("oracle", "oracle-late", "oracle-linkblind", "oracle-honest")
#: 0.3's predicted order on need served: (A, B, strict). A >= B, or A > B when strict.
PREDICTIONS: tuple[tuple[str, str, bool], ...] = (
    ("oracle", "oracle-late", False),
    ("oracle", "oracle-linkblind", False),
    ("oracle-linkblind", "oracle-honest", False),
    ("oracle", "none", True),
    ("oracle-late", "none", True),
    ("oracle-linkblind", "none", True),
    ("oracle-honest", "none", True),
)
#: 0.6: orders computed and reported per family but not judged, as (family, A, B): never in
#: `failures`, never counted toward `passed`. The truth step in stuck-vehicle: there truth without
#: the link map sends the replacement Puma into link shadow, so the prediction, which assumed the
#: rungs' handicaps do not interact, was mis-specified (SCORING-V2.md 0.6)
NOT_JUDGED: frozenset[tuple[str, str, str]] = frozenset({("stuck-vehicle", "oracle-linkblind", "oracle-honest")})
#: each rule the ladder is judged by, and the orders it reports but does not judge
RULES: dict[str, frozenset[tuple[str, str, str]]] = {"0.3": frozenset(), "0.6": NOT_JUDGED}
DEFAULT_RULE = "0.6"
#: the gap between the oracle and the honest ceiling, and what each rung's step removes
STEPS = (("link", "oracle", "oracle-linkblind"), ("truth", "oracle-linkblind", "oracle-honest"),
         ("timing", "oracle", "oracle-late"))
METRIC = "served_share"


def verdict(c: dict[str, Any]) -> str:
    """0.3's rule on a paired difference A minus B (predicted A at least B)."""
    lo, hi = c.get("lo"), c.get("hi")
    if lo is None or hi is None:
        return "no data"
    if hi < 0:
        return "fails"
    if lo > 0:
        return "holds"
    return "tie"


def arm(scored: list[dict[str, Any]], agent: str, cond: str, bases: list[str]) -> dict[str, Any] | None:
    """An agent's numbers over the situations of these families."""
    sits = situations(scored, agent, cond, bases)
    if not sits:
        return None
    shares = [pooled_share(scored, agent, cond, f) for f in bases]
    k, n = sum(x["served"] for x in shares), sum(x["eligible"] for x in shares)
    served = [t for sm in sits.values() for t in sm["time_to_serve"]]
    return {
        "served": k, "eligible": n, "share": round(k / n, 6) if n else None,
        "served_share": M.summarize(values(sits, "served_share"), "served_share"),
        "rmst": M.summarize(values(sits, "rmst"), "rmst"),
        "time_to_serve_median": round(statistics.median(served), 3) if served else None,
        "pre_served_share": M.summarize(values(sits, "pre_served_share"), "pre_served_share")["estimate"],
        "cards_per_10min": M.summarize(values(sits, "cards_per_10min"), "cards_per_10min")["estimate"],
        "useless_per_hour": M.summarize(values(sits, "per_hour.useless"), "per_hour.useless")["estimate"],
        "n_situations": len(sits),
    }


def families_with_needs(scored: list[dict[str, Any]], cond: str) -> list[str]:
    fams = sorted({base_name(e["base"]) for e in scored if e.get("base") and cond_key(e) == cond} - set(ALONE))
    return [f for f in fams if any((e.get("metrics") or {}).get("eligible") for e in scored
                                   if cond_key(e) == cond and base_name(e.get("base")) == f)]


def check(scored: list[dict[str, Any]], seed: int = 0, rule: str = DEFAULT_RULE) -> dict[str, Any]:
    """Per condition: the arms per family (every family) and pooled over the families with needs,
    each prediction's paired difference and verdict per family with needs (and pooled), whether it
    is judged (the pooled row and the rule's `RULES` orders are reported only), the steps of the
    gap, and whether the check passed."""
    reported = RULES[rule]
    out: dict[str, Any] = {}
    for cond in sorted({cond_key(e) for e in scored}):
        agents = sorted({e["agent"] for e in scored if cond_key(e) == cond and e.get("agent")})
        fams = sorted({base_name(e["base"]) for e in scored if e.get("base") and cond_key(e) == cond} - set(ALONE))
        needy = families_with_needs(scored, cond)
        block: dict[str, Any] = {"rule": rule, "agents": agents, "families_with_needs": needy, "families": {}}
        for fam in fams + ["*"]:
            f = None if fam == "*" else fam
            bases = needy if f is None else [f]
            row: dict[str, Any] = {"arms": {a: arm(scored, a, cond, bases) for a in agents}}
            if f is None or f in needy:
                preds = []
                for a, b, strict in PREDICTIONS:
                    if a not in agents or b not in agents:
                        continue
                    c = comparison(scored, a, b, cond, METRIC, bases, seed=seed)
                    preds.append({"a": a, "b": b, "strict": strict, "verdict": verdict(c),
                                  "judged": f is not None and (f, a, b) not in reported,
                                  **{k: c.get(k) for k in ("mean_difference", "lo", "hi", "n_situations", "method",
                                                           "excluded_share")}})
                row["predictions"] = preds
                row["steps"] = {name: _step(row["arms"], hi_, lo_) for name, hi_, lo_ in STEPS
                                if hi_ in agents and lo_ in agents}
            block["families"][fam] = row
        per_family = [(fam, p) for fam, r in block["families"].items() if fam != "*" for p in r.get("predictions") or []]
        judged = [p for _, p in per_family if p["judged"]]
        block["failures"] = [{"family": fam, **p} for fam, p in per_family if p["judged"] and p["verdict"] == "fails"]
        # the rule's orders, computed in each family with needs and reported, never judged (0.6)
        block["reported"] = [{"family": fam, **p} for fam, p in per_family if not p["judged"]]
        block["passed"] = bool(judged) and not block["failures"]
        out[cond] = block
    return out


def _step(arms: dict[str, Any], a: str, b: str) -> float | None:
    """Points of need served (pooled shares) lost from rung a to rung b."""
    x, y = arms.get(a), arms.get(b)
    if not x or not y or x["share"] is None or y["share"] is None:
        return None
    return round(100.0 * (x["share"] - y["share"]), 1)


def _pct(x: float | None) -> str:
    return "n/a" if x is None else f"{100.0 * x:.1f}%"


def _num(x: float | None, nd: int = 1) -> str:
    return "n/a" if x is None else f"{x:.{nd}f}"


def markdown(res: dict[str, Any]) -> str:
    lines = ["# The ruler check: the ladder of handicapped oracles", "",
             "Model results: a scripted operator on generated situations; not human data. Need served pooled over "
             "eligible needs, with the mean over situations and its 95 % cluster bootstrap interval; time to serve "
             "the restricted mean (s, unserved at 60). Verdicts on need served by paired cluster bootstrap "
             "(SCORING-V2.md 0.3).", ""]
    for cond, block in res.items():
        lines += [f"## {cond}", "", f"Passed: **{'yes' if block['passed'] else 'no'}**"
                  + (f" ({len(block['failures'])} reversal(s))" if block["failures"] else "")
                  + "".join(f"; reported, not judged ({block['rule']}): {x['a']} >= {x['b']} in {x['family']} "
                            f"{x['verdict']}" for x in block.get("reported") or []), ""]
        for fam, row in block["families"].items():
            title = "pooled over the families with needs" if fam == "*" else fam
            lines += [f"### {title}", "", "| agent | served | need served | situation mean [95 % CI] | time to serve "
                      "(RMST) | median time when served | pre-served | cards / 10 min |", "|---|---|---|---|---|---|---|---|"]
            for a, x in row["arms"].items():
                if not x:
                    continue
                s = x["served_share"]
                lines.append(f"| {a} | {x['served']}/{x['eligible']} | {_pct(x['share'])} | {_pct(s['estimate'])} "
                             f"[{_pct(s['lo'])}, {_pct(s['hi'])}] | {_num(x['rmst']['estimate'])} | "
                             f"{_num(x['time_to_serve_median'])} | {_pct(x['pre_served_share'])} | "
                             f"{_num(x['cards_per_10min'], 2)} |")
            if row.get("predictions"):
                lines += ["", "| prediction | difference [95 % CI] | n | verdict |", "|---|---|---|---|"]
                for p in row["predictions"]:
                    op = ">" if p["strict"] else ">="
                    v = p["verdict"] + (" (advantage not shown)" if p["strict"] and p["verdict"] == "tie" else "")
                    if not p["judged"]:
                        v += " (reported, not judged)" if fam == "*" else f" (reported, not judged: {block['rule']})"
                    lines.append(f"| {p['a']} {op} {p['b']} | {_num(None if p['mean_difference'] is None else 100 * p['mean_difference'])} "
                                 f"[{_num(None if p['lo'] is None else 100 * p['lo'])}, "
                                 f"{_num(None if p['hi'] is None else 100 * p['hi'])}] pts | {p['n_situations']} | {v} |")
            if row.get("steps"):
                lines += ["", "Steps (points of need served, pooled shares): "
                          + ", ".join(f"{k} {_num(v)}" for k, v in row["steps"].items())]
            lines.append("")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("batch", type=Path, help="a scoring v2 batch directory (its episodes_v2.json)")
    ap.add_argument("--rule", choices=sorted(RULES), default=DEFAULT_RULE,
                    help="the orders judged (SCORING-V2.md 0.3: every one; 0.6: all but NOT_JUDGED)")
    args = ap.parse_args(argv)
    metas = json.loads((args.batch / "episodes_v2.json").read_text())
    res = check(score(metas, Params(), multiverse=False), rule=args.rule)
    (args.batch / "ladder.json").write_text(json.dumps(res, indent=1, sort_keys=True, default=str))
    md = markdown(res)
    (args.batch / "ladder.md").write_text(md)
    print(md)
    return 0 if all(b["passed"] for b in res.values()) else 1


if __name__ == "__main__":
    raise SystemExit(main())
