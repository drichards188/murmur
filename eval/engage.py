"""Phase E plug-in point: engagement steps in the evaluation storyline.

The engagement decision chain (docs/PLAN.md, Phase E: nominate, check, preview, authorize
with an optional second person, hold the picture, execute with abort, assess) is being
built in parallel with this harness. This module is the one place where the evaluation
meets it, so that engagement scenarios use the same spec, storyline, arms, metrics and
report as every other scenario:

- A spec that needs it says `requires = ["engage"]`; the batch skips it (and the report
  lists it as waiting) until `--include-placeholders` is given or the requirement is met.
- A storyline step `kind = "engage"` names a track and an armed platform:

      [[step]]
      id = "strike-hostile"
      kind = "engage"
      at_s = 40
      track = {contact = "fastboat-hostile"}   # any track selector (eval/storyline.py)
      vehicles = {callsign = "BLADE 1"}
      effect_radius_m = 50
      fallback = "abort"                       # link-loss fallback recorded in the approval
      label = "derived"                        # truth decides (contact_affiliation)
      rationale = "Track {track} ({kind}, {affiliation}) ..."

  The adapter resolves it with the storyline's own resolution (perception for the
  rationale, truth for the label) and sends the nomination through the arm's proposer, so
  an agent nominates over MCP and a human over the operator API, exactly like a proposal.
- Metrics already count `engage` intents: an engagement dispatched without two distinct
  approvers is an invariant violation (eval/metrics.py `engage_without_two_approvers`),
  and the should-reject, should-void and latency metrics treat an engagement like any
  other labelled intent.

Until the bridge takes engagements, `available()` is false and the storyline records each
engage step as `skipped` with the reason, which the report shows.

WIRE (for the coordinator, once Phase E lands; each item is marked in the code below):

1. `available()`: how to tell that the bridge accepts engagements. The guess here is an
   `engage` entry in some platform's task list on `GET /api/platforms`; replace it with
   whatever Phase E exposes.
2. `request()`: the request Phase E expects. The guess is a vehicle-task-like body with
   `task = "engage"`, the platform's uid, the track id, the aim point and the effect
   radius, plus the link-loss fallback. Unknown keys pass through `extra` to both the
   MCP `propose_intent` arguments and the REST body (eval/agent.py).
3. Two-person approval: the proxy approves as `op-1`. If Phase E requires a second,
   different approver, run a second `ProxyOperator` as `op-2` (eval/proxy.py takes the
   operator id) and list `op-2` in `MURMUR_OPERATORS` (eval/stack.py `OPERATORS`).
4. Truth-side events that Phase E scenarios need (the target moves after approval, a
   neutral enters the keep-out, a friendly sub near the target) are scripted in the
   contacts scenario (route, `start_s`, `dwell_s`, `end_s`), as the other scenarios do;
   label them `should_void` with a `reason`, and the void metrics pick them up.
"""

from __future__ import annotations

import asyncio
import time
from typing import Any

from .agent import ProposeError
from .bridge import Bridge
from .spec import Step
from .storyline import select_vehicles


class EngageAdapter:
    def __init__(self, bridge: Bridge):
        self.bridge = bridge
        self._available: bool | None = None

    async def available(self) -> bool:
        """WIRE 1: does the bridge take engagements?"""
        if self._available is None:
            platforms = await asyncio.to_thread(self.bridge.try_get, "/api/platforms")
            found = False
            items = platforms.get("platforms", platforms) if isinstance(platforms, dict) else platforms
            if isinstance(items, dict):
                items = list(items.values())
            for p in items or []:
                if isinstance(p, dict) and "engage" in (p.get("tasks") or []):
                    found = True
                    break
            self._available = found
        return self._available

    @staticmethod
    def request(p: dict[str, Any], uids: list[int], track: dict[str, Any], aim: list[float]) -> dict[str, Any]:
        """WIRE 2: the nomination as the bridge will want it (a guess until Phase E)."""
        return {
            "task": "engage",
            "uids": uids,
            "center": aim,
            "extra": {
                "track": track.get("track_id"),
                "effect_radius_m": float(p.get("effect_radius_m", 50)),
                "fallback": p.get("fallback", "abort"),
            },
        }

    async def run_step(self, story: Any, step: Step) -> dict[str, Any]:
        p = dict(step.params)
        sel = {"task": "engage", "vehicles": p["vehicles"], "center": {"track": p["track"]}, "rationale": p.get("rationale", "")}
        _, rationale, target, lead = await story.prepare_with_retry(step.id, {**p, **sel})
        vehicles = await story.world.vehicles()
        uids = [int(v["uid"]) for v in select_vehicles(p["vehicles"], vehicles)]
        aim = list(target.get("track_position") or [])
        req = self.request(p, uids, target, aim)
        label = p.get("label", "derived")
        rec: dict[str, Any] = {
            "kind": "propose",
            "engage": True,
            "step": step.id,
            "t_rel": story.t_rel(),
            "source": {"kind": story.proposer.kind, "id": story.proposer.source_id},
            "request": req,
            "rationale": rationale,
            "label": label,
            "verify": p.get("verify", "contact_affiliation" if label == "derived" else "none"),
            "reason": p.get("reason", ""),
            "target": target,
            "vehicle": {"uid": lead.get("uid"), "callsign": (lead.get("identity") or {}).get("callsign")},
            "vehicle_truth": None,
            "t": round(time.time(), 3),
        }
        try:
            out = await story.proposer.propose(req, rationale)
            rec.update({"intent_id": out.get("id"), "state": out.get("state"), "dispatched": out.get("dispatched")})
        except ProposeError as e:
            rec["error"] = str(e)
        return rec
