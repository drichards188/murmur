"""The policy tier a would-be card would get, estimated from `get_policy` and `get_areas` without
asking the bridge (shadow mode only).

The live engine learns a card's tier from `preview_intent`. The shadow never previews: a preview
is logged under the model's agent id, and the audit log must stay about the run. So it walks the
policy's tiers itself, first match wins, as `sim/policy.toml` describes them, on facts it can
read: the task kind, whether a vehicle is armed (the compact fleet read), the source (an agent),
whether the task leaves the approved areas (the task's points, an orbit's circle at four points,
and the vehicle's position, against the approved polygons; no approved area counts as leaving,
as the bridge counts it), and unverified reserves (no endurance figure in the catalog). Then the
floors the bridge applies whatever the file says (murmur-decide policy.rs): an auto tier with an
unverified check is human ("(unverified)"); an agent's order on a vehicle with an engagement in
play is human ("(engagement in play)"); and one on a vehicle with an order a human approved
waiting in the queue is human ("(human order waiting)").

What it does not model, so it stays an estimate, said so in every shadow line, and nothing
decides on it but the shadow's own budget:
- a check that would fail (the shadow cannot tell a card the bridge would reject);
- the leg home and the transit the bridge routes (it sees the task's points and where the vehicle
  is, not the planned path or the home position, which the compact read does not carry);
- any unverified outcome other than reserves without an endurance figure (dead reckoning on a
  fixed-wing, a stale fact, terrain or space data missing);
- the rules version gates (it assumes the current rules: the queue floor of rules 5 included);
- an operator's own orders and at-submission authorization (the shadow is always an agent).
"""

from __future__ import annotations

import math
from typing import Any, Iterable

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


def waiting_uids(inbox: dict[str, Any]) -> set[int]:
    """Vehicles with an order a human approved waiting in the queue (get_inbox's `queued`)."""
    out: set[int] = set()
    for q in (inbox or {}).get("queued") or []:
        ap = q.get("approval") if isinstance(q.get("approval"), dict) else {}
        if ap.get("by") and ap.get("by") != "policy":
            out |= {int(u) for u in q.get("uids") or [] if str(u).isdigit()}
    return out


def estimate(request: dict[str, Any], *, armed: bool, endurance_known: bool, vehicle_pos: tuple[float, float] | None,
             policy: dict[str, Any], areas: dict[str, Any], engaged: Iterable[int] = (),
             waiting: Iterable[int] = ()) -> dict[str, Any]:
    """{tier, rule, facts} as the policy would route this agent's request (an estimate)."""
    rings = approved_rings(areas)
    pts = task_points(request, vehicle_pos)
    leaves = (not rings) or any(not any(in_polygon(p, ring) for ring in rings) for p in pts)
    facts = {"kind": request.get("task"), "armed": armed, "source": "agent", "leaves_approved_area": leaves,
             "unverified": [] if endurance_known or request.get("task") in ("hold", "rtl") else ["reserves"]}
    found = None
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
        found = {"tier": t.get("tier"), "rule": t.get("name"), "facts": facts, "estimated": True}
        break
    if found is None:
        return {"tier": "human", "rule": None, "facts": facts, "estimated": True}
    uids = {int(u) for u in request.get("uids") or [] if str(u).isdigit()}
    if found["tier"] == "auto":
        if facts["unverified"]:
            found.update(tier="human", rule="(unverified)")
        elif uids & set(engaged):
            found.update(tier="human", rule="(engagement in play)")
        elif uids & set(waiting):
            found.update(tier="human", rule="(human order waiting)")
    return found
