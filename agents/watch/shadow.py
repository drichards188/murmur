"""The shadow: a deterministic agent run beside the model, whose would-be proposals are logged
for comparison and never proposed.

The interface is one method, so a future playbook engine can replace today's stand-in:

    class Shadow(Protocol):
        name: str
        def observe(self, fleet: dict, tracks: list[dict]) -> list[dict]

`observe` gets the picture the driver read at a wake (`get_fleet`, `get_tracks`) and returns
what it would propose now: `{"track", "request", "rationale"}` each. It must not call the
bridge: a preview would be logged under the model's agent id, and a proposal is out of the
question.

Today's stand-in is `agents/policy_agent.py`'s rule (one sensor orbit per new suspect or
unknown track within a radius of a point, once per track), in a no-propose mode.
"""

from __future__ import annotations

from typing import Protocol

from policy_agent import SUSPICION, haversine_m, orbit_over, pick_sensor, rationale_for


class Shadow(Protocol):
    name: str

    def observe(self, fleet: dict, tracks: list[dict]) -> list[dict]: ...


class PolicyShadow:
    name = "policy_agent"

    def __init__(self, near: tuple[float, float] = (-122.4780, 37.8180), radius_km: float = 4.0,
                 platform: str | None = None):
        self.near = near
        self.radius_km = radius_km
        self.platform = platform
        self.done: set[str] = set()

    def observe(self, fleet: dict, tracks: list[dict]) -> list[dict]:
        sensor = pick_sensor(fleet.get("vehicles") or [], self.platform)
        candidates = []
        for t in tracks:
            if t.get("stale") or t["id"] in self.done or SUSPICION.get(t.get("affiliation"), 0) < 2:
                continue
            d = haversine_m(t["position"][0], t["position"][1], self.near[0], self.near[1])
            if d <= self.radius_km * 1000:
                candidates.append((SUSPICION[t["affiliation"]], -d, t, d))
        candidates.sort(key=lambda c: (c[0], c[1]), reverse=True)
        out = []
        for _, _, t, d in candidates[:1]:  # one a look, as policy_agent proposes
            self.done.add(t["id"])
            if sensor is None:
                out.append({"track": t["id"], "request": None,
                            "rationale": "no unarmed, navigation-ready aircraft to put on it"})
                continue
            out.append({"track": t["id"], "request": orbit_over(t, sensor),
                        "rationale": rationale_for(t, sensor, self.near, d)})
        return out
