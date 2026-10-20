"""Proposers: who puts intents in front of the supervisor.

- `AgentProposer`: the scripted MCP agent (`agents/policy_agent.py`'s session), source
  `agent-planner`. Its proposals in a human tier always wait in the inbox.
- `HumanProposer`: a second human operator (`op-planner`) over the operator API. With the
  evaluation policy (`submitter_authorizes = false`), its consequential intents wait for
  the supervising operator too, so the human-only arm exercises the same inbox.
- `ModelProposer`: a real model through the MCP server (`eval/model_agent.py`); optional,
  never used by tests or the default batch.

Each takes the same proposer-neutral request `{task, uids, center, radius_m, route, target,
reserve_pct, duration_s, extra}` (`extra`: keys passed through as they are, for request
fields this module doesn't know yet, such as Phase E's) and returns `{id, state,
dispatched}`, plus `refused` when the rules rejected it at submission (the intent exists).
"""

from __future__ import annotations

import asyncio
import math
import sys
from dataclasses import dataclass
from typing import Any

from . import REPO
from .bridge import Bridge, BridgeError

sys.path.insert(0, str(REPO / "agents"))


class ProposeError(RuntimeError):
    pass


def _rest_body(req: dict[str, Any], rationale: str | None) -> dict[str, Any]:
    body: dict[str, Any] = {"task": req["task"], "uids": list(req["uids"])}
    if req.get("center") is not None:
        body["center"] = req["center"]
    if req.get("radius_m") is not None:
        body["radius"] = req["radius_m"]
    if req.get("route") is not None:
        body["route"] = req["route"]
    if req.get("target") is not None:
        body["target"] = req["target"]
    constraints = {k: req[k] for k in ("reserve_pct", "duration_s") if req.get(k) is not None}
    if constraints:
        body["constraints"] = constraints
    if rationale:
        body["rationale"] = rationale
    body.update(req.get("extra") or {})  # keys a newer bridge takes (eval/engage.py)
    return body


def _mcp_args(req: dict[str, Any]) -> dict[str, Any]:
    args: dict[str, Any] = {"task": req["task"], "uids": list(req["uids"])}
    for k in ("center", "radius_m", "route", "target", "reserve_pct", "duration_s"):
        if req.get(k) is not None:
            args[k] = req[k]
    args.update(req.get("extra") or {})  # keys a newer bridge takes (eval/engage.py)
    # an engagement's track is `track` on the REST API and `track_id` on the MCP tool
    if args.get("task") == "engage" and "track" in args:
        args["track_id"] = args.pop("track")
    return args


class Proposer:
    kind = "none"
    source_id = ""
    # True for a proposer that may propose something other than the step's request (the
    # model): the storyline then records what it proposed, read after the proposal, and
    # keeps the request as `suggested` (eval/storyline.py `as_proposed`)
    adjusts: bool = False

    async def start(self) -> None:
        pass

    async def stop(self) -> None:
        pass

    async def propose(self, req: dict[str, Any], rationale: str) -> dict[str, Any]:
        raise NotImplementedError

    async def withdraw(self, id: int, reason: str) -> dict[str, Any] | None:
        return None


class HumanProposer(Proposer):
    kind = "human"

    def __init__(self, bridge: Bridge, operator: str = "op-planner"):
        self.bridge = bridge
        self.source_id = operator

    async def propose(self, req: dict[str, Any], rationale: str) -> dict[str, Any]:
        body = _rest_body(req, rationale)
        try:
            out = await asyncio.to_thread(self.bridge.submit, body, self.source_id)
        except BridgeError as e:
            # the rules rejected it at submission: the intent exists (and is in the audit
            # log), so keep its id for the labels
            if isinstance(e.body, dict) and e.body.get("intent_id") is not None:
                return {"id": e.body["intent_id"], "state": e.body.get("state"), "dispatched": False, "refused": e.body.get("detail")}
            raise ProposeError(str(e)) from e
        intent = out.get("intent") or {}
        return {"id": intent.get("id"), "state": intent.get("state"), "dispatched": bool(out.get("dispatched"))}

    async def withdraw(self, id: int, reason: str) -> dict[str, Any] | None:
        try:
            out = await asyncio.to_thread(
                self.bridge.post, f"/api/intents/{id}/cancel", {"note": reason}, self.source_id
            )
        except BridgeError as e:
            raise ProposeError(str(e)) from e
        return {"id": id, "state": (out.get("intent") or {}).get("state")}


class AgentProposer(Proposer):
    kind = "agent"

    def __init__(self, mcp_url: str, agent: str = "agent-planner", key: str | None = None):
        self.mcp_url = mcp_url
        self.source_id = agent
        # its API key on a bridge that authenticates (a credentials run)
        self.key = key
        self._session = None

    async def start(self) -> None:
        from policy_agent import AgentSession  # agents/policy_agent.py

        self._session = AgentSession(self.mcp_url, self.source_id, self.key)
        await self._session.__aenter__()

    async def stop(self) -> None:
        if self._session is not None:
            await self._session.__aexit__(None, None, None)
            self._session = None

    async def propose(self, req: dict[str, Any], rationale: str) -> dict[str, Any]:
        if self._session is None:
            raise ProposeError("agent session not started")
        try:
            out = await self._session.propose(rationale, **_mcp_args(req))
        except RuntimeError as e:
            raise ProposeError(str(e)) from e
        rec = {"id": out.get("id"), "state": out.get("state"), "dispatched": bool(out.get("dispatched"))}
        if out.get("refused"):
            rec["refused"] = out["refused"]
        return rec

    async def withdraw(self, id: int, reason: str) -> dict[str, Any] | None:
        if self._session is None:
            raise ProposeError("agent session not started")
        try:
            out = await self._session.withdraw(id, reason)
        except RuntimeError as e:
            raise ProposeError(str(e)) from e
        return {"id": id, "state": out.get("state")}

    async def tracks(self) -> list[dict]:
        if self._session is None:
            return []
        return await self._session.tracks()


SUSPICION = {"hostile": 3, "suspect": 2, "unknown": 1}


@dataclass
class AgentPolicy:
    """The agent arm's own proposer policy, written before any result was seen and the same
    in every scenario: every `every_s` seconds from `start_s`, read the tracks and the fleet
    through the MCP tools; among tracks that are not stale, estimated hostile, suspect or
    unknown, within `watch_km` of the watch point, not yet proposed on, take the most
    suspicious (then the nearest); put an armed aircraft that is navigation-ready, with a
    fresh link and no proposal of this agent in the last `rest_s`, in an orbit of
    `radius_m` over it, with a rationale naming the facts and their ages. At most one
    proposal per look and `max_proposals` per run."""

    start_s: float = 20.0
    every_s: float = 15.0
    max_proposals: int = 6
    radius_m: float = 300.0
    watch: tuple[float, float] = (-122.4783, 37.8199)  # the Golden Gate Bridge, mid-span
    watch_km: float = 6.0
    rest_s: float = 60.0


def _haversine_m(lon1: float, lat1: float, lon2: float, lat2: float) -> float:
    r = 6371000.0
    p1, p2 = math.radians(lat1), math.radians(lat2)
    a = math.sin((p2 - p1) / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(math.radians(lon2 - lon1) / 2) ** 2
    return 2 * r * math.asin(math.sqrt(a))


class AutonomousAgent(AgentProposer):
    """The agent arm: the same MCP session, but the agent chooses what to propose
    (`AgentPolicy`) instead of passing the storyline's proposals through."""

    autonomous = True

    def __init__(self, mcp_url: str, agent: str = "agent-planner", policy: AgentPolicy | None = None, key: str | None = None):
        super().__init__(mcp_url, agent, key)
        self.policy = policy or AgentPolicy()
        self.proposed: set[str] = set()
        self.last_use: dict[int, float] = {}

    async def choose(self, now: float) -> dict[str, Any] | None:
        if self._session is None:
            raise ProposeError("agent session not started")
        p = self.policy
        try:
            tracks = await self._session.tracks(include_stale=False)
            fleet = await self._session.fleet()
        except RuntimeError as e:
            raise ProposeError(str(e)) from e
        candidates = []
        for t in tracks:
            if t["id"] in self.proposed or t.get("stale") or t.get("affiliation") not in SUSPICION:
                continue
            d = _haversine_m(p.watch[0], p.watch[1], t["position"][0], t["position"][1])
            if d > p.watch_km * 1000:
                continue
            candidates.append((-SUSPICION[t["affiliation"]], d, t))
        if not candidates:
            return None
        candidates.sort(key=lambda c: (c[0], c[1]))
        armed = []
        for v in fleet.get("vehicles", []):
            plat = v.get("platform") or {}
            if not (plat.get("armed") and plat.get("domain") == "air"):
                continue
            if not (v.get("nav") or {}).get("ready") or (v.get("nav") or {}).get("stale") or (v.get("link") or {}).get("lost"):
                continue
            if now - self.last_use.get(int(v["uid"]), -1e9) < p.rest_s:
                continue
            armed.append(v)
        if not armed:
            return None
        _, d, t = candidates[0]
        v = armed[0]
        self.proposed.add(t["id"])
        self.last_use[int(v["uid"])] = now
        kind = (t.get("identity") or {}).get("kind") or "unclassified contact"
        rationale = (
            f"Track {t['id']} ({kind}, estimated {t['affiliation']} {float(t.get('affiliation_confidence') or 0):.2f}: "
            f"{t.get('affiliation_reason', '')}) is {d / 1000:.1f} km from the bridge, last seen {float(t.get('age_s') or 0):.0f} s ago. "
            f"Put {v.get('callsign')} in an orbit over it so an effect is ready if it is declared hostile."
        )
        return {"track_id": t["id"], "uid": int(v["uid"]), "center": list(t["position"]), "radius_m": p.radius_m, "rationale": rationale}


def make_proposer(
    kind: str, bridge: Bridge, mcp_url: str, model: str | None = None, brief: str = "", keys: dict[str, str] | None = None
) -> Proposer:
    """The proposer of an arm. `keys`: each agent's API key on a bridge that authenticates
    (empty on a dev bridge, which believes the agent header)."""
    keys = keys or {}
    if kind == "human":
        return HumanProposer(bridge)
    if kind == "agent":
        return AgentProposer(mcp_url, key=keys.get("agent-planner"))
    if kind == "agent-autonomous":
        return AutonomousAgent(mcp_url, key=keys.get("agent-planner"))
    if kind == "model":
        from .model_agent import AGENT_ID, ModelProposer

        return ModelProposer(mcp_url, model=model, brief=brief, key=keys.get(AGENT_ID))
    raise ValueError(f"unknown proposer {kind!r}")


__all__ = ["Proposer", "HumanProposer", "AgentProposer", "make_proposer", "ProposeError"]
