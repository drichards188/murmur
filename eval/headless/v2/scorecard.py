"""Scoring v2's scorecard (eval/SCORING-V2.md, section 9.1): one row per test area, each agent's
area metrics beside the floor (`none`) and the ceiling (`oracle`) when they ran, and the app
and system checks for that area. No composite score. Every cell carries its n_situations and
interval, and is marked "descriptive, underpowered" when n is below the power analysis's size
(8.6: 150 situations for a rate, 30 for burden). One scorecard per condition (the operator, the
policy, the clock); S10 (policy) compares the conditions on its families instead.

    .venv/bin/python -m eval.headless.v2.scorecard <batch dir>    # from its episodes_v2.json

MODEL RESULTS: a scripted operator on generated situations; never human data.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from . import metrics as M
from . import stats as S
from .aggregate import AREAS, area_of, base_name, cond_key, situations, values

UNDERPOWERED = "descriptive, underpowered"
ANCHORS = ("none", "oracle")
#: the checks shown for every area (3.6, 3.7), and per area besides
CHECKS = ("pre_served_share", "sensing_error_share")
AREA_CHECKS = {"S7": ("voids",), "S8": ("justified_useless_share",), "S9": ("scripted_cards_per_10min",)}


def _in_area(e: dict[str, Any], area: dict[str, Any]) -> bool:
    if area["area"] == "S10":
        return base_name(e.get("base")) in area["families"]
    return area_of(e) == area["area"]


def _bases(scored: list[dict[str, Any]], area: dict[str, Any]) -> list[str]:
    return sorted({e["base"] for e in scored if e.get("base") and _in_area(e, area)})


def cell(sits: dict[str, dict[str, Any]], metric: str, seed: int = 0) -> dict[str, Any]:
    s = M.summarize(values(sits, metric), metric, seed=seed)
    out = {k: s.get(k) for k in ("estimate", "lo", "hi", "n_situations", "method", "power_n")}
    if s.get("underpowered"):
        out["mark"] = UNDERPOWERED
    return out


def checks(sits: dict[str, dict[str, Any]], area: str, seed: int = 0) -> dict[str, Any]:
    out: dict[str, Any] = {}
    n = len(sits)
    for k in ("invariant_violations", "harmful", "refused"):
        out[k] = S.clopper_pearson(sum(1 for sm in sits.values() if sm["constraints"][k] > 0), n)
    for k in CHECKS + AREA_CHECKS.get(area, ()):
        out[k] = cell(sits, k, seed)
    # 8.4: the counts beside n_situations, and n_eff where needs share a situation
    out["needs"] = sum(len(sm["need_outcomes"]) for sm in sits.values())
    out["cards"] = sum(sm["cards"] for sm in sits.values())
    out["n_eff_needs"] = S.n_eff([sm["need_outcomes"] for sm in sits.values()]).get("n_eff")
    return out


def scorecard(scored: list[dict[str, Any]], r0_only: tuple[str, ...] = ("model",), seed: int = 0) -> dict[str, Any]:
    conds = sorted({cond_key(e) for e in scored if e.get("agent")})
    out: dict[str, Any] = {"caveat": "Model results: a scripted operator on generated situations; not human data.",
                           "power_sizes": S.POWER_SIZES, "conditions": {}}
    for cond in conds:
        rows = []
        for area in AREAS:
            if area.get("by_condition"):
                continue
            bases = _bases([e for e in scored if cond_key(e) == cond], area)
            if not bases:
                continue
            agents = sorted({e["agent"] for e in scored if cond_key(e) == cond and e["base"] in bases})
            row: dict[str, Any] = {"area": area["area"], "name": area["name"], "subject": area["subject"],
                                   "families": bases, "metrics": {}, "checks": {}, "excluded": {}}
            for agent in agents:
                sits = situations(scored, agent, cond, bases, agent in r0_only)
                for mn in area["metrics"]:
                    row["metrics"].setdefault(mn, {})[agent] = cell(sits, mn, seed)
                row["checks"][agent] = checks(sits, area["area"], seed)
                ex = [e for e in scored if e["agent"] == agent and cond_key(e) == cond and e["base"] in bases
                      and e.get("class", "scored") != "scored"]
                if ex:
                    row["excluded"][agent] = {c: sum(1 for e in ex if e["class"] == c) for c in sorted({e["class"] for e in ex})}
            rows.append(row)
        out["conditions"][cond] = {"rows": rows}
    # S10: the team and policy rows, every condition side by side on S1, S2 and S9's families
    s10 = next(a for a in AREAS if a["area"] == "S10")
    team = []
    for cond in conds:
        bases = _bases([e for e in scored if cond_key(e) == cond], s10)
        if not bases:
            continue
        for agent in sorted({e["agent"] for e in scored if cond_key(e) == cond and e["base"] in bases}):
            sits = situations(scored, agent, cond, bases, agent in r0_only)
            team.append({"condition": cond, "agent": agent, "families": bases,
                         "metrics": {mn: cell(sits, mn, seed) for mn in s10["metrics"]}})
    if team:
        out["S10"] = {"area": "S10", "name": s10["name"], "subject": s10["subject"], "rows": team,
                      "note": "3.8: the same agent under each operator and policy; hypotheses under D6"}
    return out


def fmt(c: dict[str, Any] | None, pct: bool = False) -> str:
    if not c or c.get("estimate") is None:
        return "n/a"
    f = (lambda x: f"{100 * x:.0f}%") if pct else (lambda x: f"{x:.3g}")
    iv = f" [{f(c['lo'])}, {f(c['hi'])}]" if c.get("lo") is not None and c.get("hi") is not None else ""
    mark = " †" if c.get("mark") else ""
    return f"{f(c['estimate'])}{iv} n={c.get('n_situations')}{mark}"


SHARES = ("served_share", "served_late_share", "bad_card_share", "flood_share", "fault_met_share", "sensing_error_share",
          "justified_useless_share", "pre_served_share", "served_after_stop")


def markdown(card: dict[str, Any]) -> str:
    lines = ["# Scoring v2 scorecard", "",
             "Model results: a scripted operator on generated situations; not human data. "
             "Each cell: estimate [95 % interval] n=situations. "
             f"† {UNDERPOWERED}: n below the power analysis's size "
             f"({card['power_sizes']['rate']} situations for a rate, {card['power_sizes']['burden']} for burden).", ""]
    for cond, block in card["conditions"].items():
        operator, policy, clock = (cond.split(";") + ["", "", ""])[:3]
        lines += [f"## Operator {operator}, policy {policy}, clock {clock}", ""]
        agents = sorted({a for r in block["rows"] for m in r["metrics"].values() for a in m})
        order = [a for a in agents if a not in ANCHORS] + [a for a in ANCHORS if a in agents]
        lines += ["| area | metric | " + " | ".join(order) + " | checks |",
                  "|---|---|" + "---|" * len(order) + "---|"]
        for r in block["rows"]:
            first = True
            for mn, by_agent in r["metrics"].items():
                cells = [fmt(by_agent.get(a), mn in SHARES) for a in order]
                chk = ""
                if first:
                    parts = []
                    for a in order:
                        c = r["checks"].get(a) or {}
                        inv = c.get("invariant_violations") or {}
                        parts.append(f"{a}: needs {c.get('needs', 0)} (n_eff {c.get('n_eff_needs')}), cards "
                                     f"{c.get('cards', 0)}, invariants {inv.get('k', 0)}/{inv.get('n', 0)}, refused "
                                     f"{(c.get('refused') or {}).get('k', 0)}, pre-served {fmt(c.get('pre_served_share'), True)}")
                    if r.get("excluded"):
                        parts.append(f"excluded {r['excluded']}")
                    chk = "; ".join(parts)
                label = f"{r['area']} {r['name']} ({', '.join(r['families'])})" if first else ""
                lines.append(f"| {label} | {mn} | " + " | ".join(cells) + f" | {chk} |")
                first = False
        lines.append("")
    if card.get("S10"):
        lines += ["## S10 policy: team and policy rows (3.8)", "", "| condition | agent | " +
                  " | ".join(next(iter(card["S10"]["rows"]))["metrics"]) + " |",
                  "|---|---|" + "---|" * len(next(iter(card["S10"]["rows"]))["metrics"])]
        for r in card["S10"]["rows"]:
            lines.append(f"| {r['condition']} | {r['agent']} | " +
                         " | ".join(fmt(c, mn in SHARES) for mn, c in r["metrics"].items()) + " |")
        lines.append("")
    return "\n".join(lines) + "\n"


def write(card: dict[str, Any], out: Path) -> tuple[Path, Path]:
    j, m = out / "scorecard.json", out / "scorecard.md"
    j.write_text(json.dumps(card, indent=1, sort_keys=True))
    m.write_text(markdown(card))
    return j, m


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("batch", type=Path, help="a batch directory with episodes_v2.json (batch.py --scoring v2)")
    args = ap.parse_args(argv)
    from .aggregate import score

    metas = json.loads((args.batch / "episodes_v2.json").read_text())
    scored = score(metas)
    j, m = write(scorecard(scored), args.batch)
    print(m.read_text())
    print(j)
    return 0


if __name__ == "__main__":
    sys.exit(main())
