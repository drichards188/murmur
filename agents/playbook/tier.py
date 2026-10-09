"""The policy tier a would-be card would get, estimated from `get_policy` and `get_areas` without
asking the bridge (shadow mode only).

The live engine learns a card's tier from `preview_intent`. The shadow never previews: a preview
is logged under the model's agent id, and the audit log must stay about the run. So it walks the
policy's tiers itself, first match wins, as `sim/policy.toml` describes them, on facts it can
read: the task kind, whether a vehicle is armed (the compact fleet read), the source (an agent),
whether the task leaves the approved areas (the task's points, an orbit's circle at four points,
and the vehicle's position, against the approved polygons; no approved area counts as leaving,
as the bridge counts it), and unverified reserves (no endurance figure in the catalog). It does
not see the return leg home (the compact read has no home) or a check that would fail, so it is
an estimate, said so in every shadow line; nothing decides on it but the shadow's own budget.
"""

from __future__ import annotations

import math
from typing import Any

from .beliefs import in_polygon

M_PER_DEG = 111_320.0


def approved_rings(areas: dict[str, Any]) -> list[list[list[float]]]:
    out = []
    for a in (areas or {}).get("areas") or []:
        if a.get("kind") != "approved":
            continue
        for p in a.get("polygons") or []:
            ring = p.get("exterior") if isinstance(p, dict) else None
            if isinstance(ring, list) and len(ring) >= 3:
                out.append(ring)
    return out


def task_points(request: dict[str, Any], vehicle_pos: tuple[float, float] | None) -> list[tuple[float, float]]:
    pts: list[tuple[float, float]] = []
    if vehicle_pos is not None:
        pts.append(vehicle_pos)
    c, r = request.get("center"), request.get("radius_m")
    if c and r:
        dlat = float(r) / M_PER_DEG
        dlon = float(r) / (M_PER_DEG * math.cos(math.radians(c[1])))
        pts += [(c[0], c[1] + dlat), (c[0] + dlon, c[1]), (c[0], c[1] - dlat), (c[0] - dlon, c[1])]
    elif c:
        pts.append((c[0], c[1]))
    for q in request.get("route") or []:
        pts.append((q[0], q[1]))
    return pts


def estimate(request: dict[str, Any], *, armed: bool, endurance_known: bool, vehicle_pos: tuple[float, float] | None,
             policy: dict[str, Any], areas: dict[str, Any]) -> dict[str, Any]:
    """{tier, rule, facts} as the policy would route this agent's request (an estimate)."""
    rings = approved_rings(areas)
    pts = task_points(request, vehicle_pos)
    leaves = (not rings) or any(not any(in_polygon(p, ring) for ring in rings) for p in pts)
    facts = {"kind": request.get("task"), "armed": armed, "source": "agent", "leaves_approved_area": leaves,
             "unverified": [] if endurance_known or request.get("task") in ("hold", "rtl") else ["reserves"]}
    for t in ((policy or {}).get("policy") or {}).get("tier") or []:
        when = t.get("when") or {}
        if "kind" in when and facts["kind"] not in when["kind"]:
            continue
        if "armed" in when and bool(when["armed"]) != armed:
            continue
        if "source" in when and when["source"] != "agent":
            continue
        if "leaves_approved_area" in when and bool(when["leaves_approved_area"]) != leaves:
            continue
        if "unverified" in when and not set(when["unverified"]) & set(facts["unverified"]):
            continue
        return {"tier": t.get("tier"), "rule": t.get("name"), "facts": facts, "estimated": True}
    return {"tier": "human", "rule": None, "facts": facts, "estimated": True}
