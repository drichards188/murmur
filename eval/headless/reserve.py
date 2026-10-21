"""Scoring v2's world calibration (eval/SCORING-V2.md section 0.1): where PUMA 1 waits in reserve,
chosen by a rule that looks only at the anchors `none` and `oracle`, never at an agent under test.
MODEL RESULTS ONLY.

    .venv/bin/python -m eval.headless.reserve run --out eval/results/headless/v2-calibration --workers 12
    .venv/bin/python -m eval.headless.reserve choose --out eval/results/headless/v2-calibration

`run`: for each candidate committed in eval/headless/reserve_candidates.toml (before any run), one
scoring v2 batch (batch.py `--scoring v2 --reserve <candidate>`) of `none` and `oracle` under the
approve-all operator, replicate 0, on the dev-build situation seeds 1000-1049, in every v2 family
(S1 to S9), into `<out>/<candidate>/`.

`choose`: the rule, as 0.1 states it. Per candidate and family, each anchor's needs served by their
deadline over its eligible needs, pooled over the family's situations. The families with eligible
needs are the ones it judges (a family whose situations have none, `quiet` and `conflict` by
construction, has no share). A candidate qualifies if in every such family the oracle serves at
least 80 % and `none` at most 50 %. Among the qualifying, the lowest pooled `none` share (every
judged family's needs together) wins; a tie goes to the candidate nearest PUMA 1's spawn before the
calibration (the candidate `spawn`). If none qualifies, nothing is chosen: the mission's 60 s window
may be infeasible for this fleet, and that goes to the owner (0.1, item 4). Writes
`<out>/calibration.json` and `<out>/calibration.md`. The playbook and every agent under test are
neither run nor read.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from .families import FAMILIES_V2, GATE, dist_m, reserve_candidates

SEEDS = "1000-1049"
ANCHORS = ("none", "oracle")
NONE_MAX = 0.5
ORACLE_MIN = 0.8
OPERATOR = "approve-all"
#: PUMA 1's spawn before the calibration: the tie-break's point
CURRENT = "spawn"


def run(out: Path, workers: int, seeds: str = SEEDS, names: list[str] | None = None, build: bool = False) -> None:
    from . import batch

    for name in names or list(reserve_candidates()):
        d = out / name
        if (d / "episodes_v2.json").exists():
            print(f"{name}: already run ({d})")
            continue
        args = ["--scoring", "v2", "--families", *[f"v2-{f}" for f in FAMILIES_V2], "--seeds", seeds,
                "--agent", "oracle", "none", "--operators", OPERATOR, "--workers", str(workers), "--no-multiverse",
                "--reserve", name, "--out", str(d)]
        if not build:
            args.append("--no-build")
        print(f"{name}: {' '.join(args)}", flush=True)
        batch.main(args)


def shares(batch_dir: Path) -> dict[str, Any]:
    """Per family, each anchor's pooled served share, from a batch's records (its labels)."""
    from .v2.aggregate import base_name, score

    metas = json.loads((batch_dir / "episodes_v2.json").read_text())
    bad = [m.get("name") for m in metas if m.get("class") != "scored" or m.get("agent") not in ANCHORS]
    scored = score(metas, multiverse=False)
    out: dict[str, Any] = {}
    for e in scored:
        if e["agent"] not in ANCHORS or not e.get("metrics"):
            continue
        f = base_name(e["base"])
        row = out.setdefault(f, {a: {"served": 0, "eligible": 0, "situations": set()} for a in ANCHORS})
        row[e["agent"]]["served"] += int(e["metrics"]["served"])
        row[e["agent"]]["eligible"] += int(e["metrics"]["eligible"])
        row[e["agent"]]["situations"].add(e["situation"])
    for row in out.values():
        for a in ANCHORS:
            r = row[a]
            r["n_situations"] = len(r.pop("situations"))
            r["share"] = round(r["served"] / r["eligible"], 6) if r["eligible"] else None
    return {"families": dict(sorted(out.items())), "not_scored_or_other": bad}


def choose(out: Path) -> dict[str, Any]:
    cands = reserve_candidates()
    here = (float(cands[CURRENT]["lon"]), float(cands[CURRENT]["lat"]))
    table: dict[str, Any] = {}
    for name, c in cands.items():
        d = out / name
        if not (d / "episodes_v2.json").exists():
            table[name] = {"missing": str(d)}
            continue
        s = shares(d)
        judged = {f: r for f, r in s["families"].items() if r["none"]["eligible"] and r["oracle"]["eligible"]}
        fails = {f: {"none": r["none"]["share"], "oracle": r["oracle"]["share"]} for f, r in judged.items()
                 if not (r["none"]["share"] <= NONE_MAX and r["oracle"]["share"] >= ORACLE_MIN)}
        pooled = {a: {"served": sum(r[a]["served"] for r in judged.values()),
                      "eligible": sum(r[a]["eligible"] for r in judged.values())} for a in ANCHORS}
        for a in ANCHORS:
            pooled[a]["share"] = round(pooled[a]["served"] / pooled[a]["eligible"], 6) if pooled[a]["eligible"] else None
        table[name] = {"lon": c["lon"], "lat": c["lat"], "place": c.get("place"),
                       "from_spawn_m": round(dist_m((float(c["lon"]), float(c["lat"])), here)),
                       "from_bridge_m": round(dist_m((float(c["lon"]), float(c["lat"])), GATE)),
                       "families": s["families"], "judged": sorted(judged), "fails": fails,
                       "qualifies": bool(judged) and not fails, "pooled": pooled,
                       "not_scored_or_other": s["not_scored_or_other"]}
    ok = [n for n, r in table.items() if r.get("qualifies")]
    chosen = min(ok, key=lambda n: (table[n]["pooled"]["none"]["share"], table[n]["from_spawn_m"], n)) if ok else None
    res = {"rule": "SCORING-V2.md 0.1: in every family with eligible needs the oracle serves at least 80 % and none at "
                   "most 50 %; among qualifying candidates the lowest pooled none share; ties to the nearest the current "
                   "spawn", "seeds": SEEDS, "operator": OPERATOR, "replicate": 0, "anchors": list(ANCHORS),
           "candidates": table, "qualifying": ok, "chosen": chosen,
           "note": ("no candidate qualifies: the mission's 60 s window may be infeasible for this fleet; reported to "
                    "the owner, the mission and the rule unchanged (0.1, item 4)") if chosen is None else None,
           "caveat": "Model results: anchors on generated situations; the playbook and agents under test not consulted."}
    (out / "calibration.json").write_text(json.dumps(res, indent=1, sort_keys=True))
    (out / "calibration.md").write_text(markdown(res))
    return res


def markdown(res: dict[str, Any]) -> str:
    fams = sorted({f for r in res["candidates"].values() for f in (r.get("families") or {})})
    lines = ["# Scoring v2 world calibration: PUMA 1's reserve point (SCORING-V2.md 0.1)", "",
             f"Anchors only: none and oracle, {res['operator']} operator, dev-build seeds {res['seeds']}, replicate 0. "
             "Each cell: none / oracle served share of eligible needs (served/eligible), pooled over the family's "
             "situations. Model results, not human data.", "",
             "| candidate | from spawn | " + " | ".join(fams) + " | pooled | qualifies |",
             "|---|---|" + "---|" * len(fams) + "---|---|"]
    for name, r in res["candidates"].items():
        if r.get("missing"):
            lines.append(f"| {name} | missing | " + " | ".join("" for _ in fams) + " | | |")
            continue
        cells = []
        for f in fams:
            x = (r["families"] or {}).get(f)
            if not x or not x["none"]["eligible"]:
                cells.append("no eligible needs")
                continue
            cells.append(f"{_pct(x['none']['share'])} / {_pct(x['oracle']['share'])} "
                         f"({x['none']['served']}/{x['none']['eligible']}, {x['oracle']['served']}/{x['oracle']['eligible']})")
        p = r["pooled"]
        lines.append(f"| {name} | {r['from_spawn_m']} m | " + " | ".join(cells) +
                     f" | {_pct(p['none']['share'])} / {_pct(p['oracle']['share'])} | {'yes' if r['qualifies'] else 'no'} |")
    lines += ["", f"Chosen: **{res['chosen']}**" if res["chosen"] else f"Chosen: none. {res['note']}", ""]
    return "\n".join(lines)


def _pct(x: float | None) -> str:
    return "n/a" if x is None else f"{100 * x:.1f}%"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("step", choices=["run", "choose"])
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--workers", type=int, default=8)
    ap.add_argument("--candidates", nargs="*", default=None)
    ap.add_argument("--build", action="store_true", help="run: build murmur-headless first")
    args = ap.parse_args(argv)
    if args.step == "run":
        args.out.mkdir(parents=True, exist_ok=True)
        run(args.out, args.workers, names=args.candidates, build=args.build)
        return 0
    res = choose(args.out)
    print((args.out / "calibration.md").read_text())
    return 0 if res["chosen"] else 2


if __name__ == "__main__":
    sys.exit(main())
