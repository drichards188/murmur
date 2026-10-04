"""What the C2 shows against a task: the generic rule book.

One function, used twice: the proxy operator applies it when it reads a card
(eval/proxy.py), and the metrics apply it to what was recorded at proposal time to sort each
wrong proposal into "the C2 showed something against it" (catchable from the picture) or
"the C2 showed nothing against it" (the picture itself was wrong) (eval/metrics.py).

Written from what the console and the API show, before the held-out scenarios
(eval/scenarios/heldout-*.toml) existed, and not tuned to any scenario. It reads only:

- the card's checks: any check that failed or could not be verified;
- for a task on an armed platform aimed at a point (an orbit): the tracks the C2 holds
  within the task's radius of its aim point, as displayed (estimated or declared
  affiliation, affiliation confidence, stale flag):
  - no track there: the task is aimed at nothing the C2 holds;
  - any of them shown neutral or assumed friend: a protected track inside the task's area;
  - the nearest shown stale: the picture of the target is out of date;
  - the nearest shown with an affiliation confidence below `C_MIN` (0.5, "more likely than
    not"), unless an operator declared it: the picture does not support the task.

Nothing here reads the rationale, truth, or anything a scenario scripts.
"""

from __future__ import annotations

import math
from typing import Any

RULEBOOK_VERSION = "v2, 2026-10-04"
C_MIN = 0.5
PROTECTED = ("neutral", "assumed_friend")


def haversine_m(lon1: float, lat1: float, lon2: float, lat2: float) -> float:
    r = 6371000.0
    p1, p2 = math.radians(lat1), math.radians(lat2)
    a = math.sin((p2 - p1) / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(math.radians(lon2 - lon1) / 2) ** 2
    return 2 * r * math.asin(math.sqrt(a))


def near_aim(center: list[float] | None, radius_m: float | None, tracks: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """The tracks within the task's radius of its aim point, nearest first. Takes tracks as
    the API serves them (`position`) or as recorded (`distance_m`)."""
    if center is None or radius_m is None:
        return []
    out = []
    for t in tracks:
        d = t.get("distance_m")
        if d is None and t.get("position"):
            d = haversine_m(center[0], center[1], t["position"][0], t["position"][1])
        if d is not None and d <= float(radius_m):
            out.append({**t, "distance_m": d})
    return sorted(out, key=lambda t: t["distance_m"])


def evidence(
    armed: bool,
    checks: list[dict[str, Any]],
    center: list[float] | None,
    radius_m: float | None,
    tracks: list[dict[str, Any]],
    c_min: float = C_MIN,
) -> list[str]:
    """Every reason what the C2 shows speaks against the task; empty when nothing does."""
    reasons = [
        f"check {c.get('rule')}{':' + str(c['uid']) if c.get('uid') is not None else ''} {c.get('outcome')}"
        for c in checks
        if c.get("outcome") in ("fail", "unverified")
    ]
    if not armed or center is None or radius_m is None:
        return reasons
    near = near_aim(center, radius_m, tracks)
    if not near:
        reasons.append(f"no track the C2 holds is within {float(radius_m):.0f} m of the aim point")
        return reasons
    for t in near:
        aff = (t.get("declared") if isinstance(t.get("declared"), str) else (t.get("declared") or {}).get("affiliation")) or t.get("affiliation")
        if aff in PROTECTED:
            reasons.append(f"{t.get('id')} is shown {aff} {t['distance_m']:.0f} m from the aim point")
    t = near[0]
    declared = t.get("declared") if isinstance(t.get("declared"), str) else (t.get("declared") or {}).get("affiliation")
    if t.get("state") == "stale":
        reasons.append(f"{t.get('id')} is shown stale")
    conf = t.get("affiliation_confidence")
    if not declared and conf is not None and float(conf) < c_min:
        reasons.append(f"{t.get('id')} is shown {t.get('affiliation')} with affiliation confidence {float(conf):.2f} (below {c_min})")
    return reasons
