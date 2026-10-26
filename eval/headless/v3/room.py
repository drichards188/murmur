"""Scoring v3's room test (eval/SCORING-V3.md 10.1; the owner's decision of 2026-10-25, replacing v2's
fixed world conditions): per family and world, the ceiling must do measurably better than `none` on
need served, so the family leaves agents room between the floor and the ceiling. MODEL RESULTS ONLY.

**The test.** Per situation, the ceiling's need served (served over eligible needs) minus `none`'s, on
the situations where both have an eligible need. The room is shown when

1. the paired 95 % interval of the mean difference lies wholly above zero (the paired cluster
   bootstrap, BCa, seed 0, at 20 situations or more; else the exact sign test: v2/metrics.py
   `compare`, as every v2 and v3 paired comparison), and
2. the mean difference is at least `MIN_ROOM` (10 points of need served).

`MIN_ROOM` is the smallest room worth a family (SCORING-V3.md 10.1 gives the argument): at the test's
planned size (v2 8.6: about 150 situations a cell), the paired interval of a difference in need served
is about 8 to 11 points either side with the dev batches' discordance between the floor and the
ceiling, so a family whose ceiling stands less than 10 points above its floor cannot separate even
the ceiling from the floor at that size, and a family can only rank agents inside the room it
leaves. A family-world without it is reported (`no room`) and examined, never tuned on the seeds
that showed it; its agents' numbers are still reported, but it is not used to rank agents.

The verdicts: `room` (both hold), `small` (the interval is above zero, the mean under `MIN_ROOM`),
`none shown` (the interval reaches zero), `reversed` (the interval lies wholly below zero), `no data`.
"""

from __future__ import annotations

from typing import Any

from ..v2 import metrics as V2M

#: the smallest room worth a family, in points of need served (module doc; SCORING-V3.md 10.1)
MIN_ROOM = 0.10
CEILING = "oracle-v3"
FLOOR = "none"


def served_by_situation(scored: list[dict[str, Any]], family: str, agent: str) -> dict[Any, float]:
    """Per situation seed of a family (`v3-<family>-<world>`), the agent's need served (served over
    eligible needs), where it has an eligible need."""
    out: dict[Any, float] = {}
    for s in scored:
        if s["family"] != family or s["agent"] != agent:
            continue
        el = [n for n in s["needs"] if n["eligible"]]
        if el:
            out[s["seed"]] = sum(1.0 for n in el if n["served"]) / len(el)
    return out


def verdict(c: dict[str, Any], min_room: float = MIN_ROOM) -> str:
    lo, hi, d = c.get("lo"), c.get("hi"), c.get("mean_difference")
    if lo is None or hi is None or d is None:
        return "no data"
    if hi < 0:
        return "reversed"
    if lo <= 0:
        return "none shown"
    return "room" if d >= min_room - 1e-12 else "small"


def room(scored: list[dict[str, Any]], family: str, ceiling: str = CEILING, floor: str = FLOOR, seed: int = 0,
         min_room: float = MIN_ROOM) -> dict[str, Any]:
    """The room test for one family-world (`v3-<family>-<gg|sd>`)."""
    a = served_by_situation(scored, family, ceiling)
    b = served_by_situation(scored, family, floor)
    c = V2M.compare(a, b, "served_share", seed=seed)
    both = sorted(set(a) & set(b), key=str)
    out = {k: c.get(k) for k in ("n_situations", "mean_difference", "lo", "hi", "method", "p")}
    out.update(ceiling=ceiling, floor=floor, min_room=min_room, verdict=verdict(c, min_room),
               ceiling_share=round(sum(a[s] for s in both) / len(both), 3) if both else None,
               floor_share=round(sum(b[s] for s in both) / len(both), 3) if both else None,
               discordant=sum(1 for s in both if a[s] != b[s]))
    return out


def room_all(scored: list[dict[str, Any]], seed: int = 0) -> dict[str, dict[str, Any]]:
    """The room test for every family-world of a batch that ran the ceiling and the floor."""
    fams = sorted({s["family"] for s in scored})
    agents = {s["agent"] for s in scored}
    if CEILING not in agents or FLOOR not in agents:
        return {}
    return {f: room(scored, f, seed=seed) for f in fams}
