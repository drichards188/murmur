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
import sys
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
    return args


class Proposer:
    kind = "none"
    source_id = ""

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

    def __init__(self, mcp_url: str, agent: str = "agent-planner"):
        self.mcp_url = mcp_url
        self.source_id = agent
        self._session = None

    async def start(self) -> None:
        from policy_agent import AgentSession  # agents/policy_agent.py

        self._session = AgentSession(self.mcp_url, self.source_id)
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


def make_proposer(kind: str, bridge: Bridge, mcp_url: str, model: str | None = None, brief: str = "") -> Proposer:
    if kind == "human":
        return HumanProposer(bridge)
    if kind == "agent":
        return AgentProposer(mcp_url)
    if kind == "model":
        from .model_agent import ModelProposer

        return ModelProposer(mcp_url, model=model, brief=brief)
    raise ValueError(f"unknown proposer {kind!r}")


__all__ = ["Proposer", "HumanProposer", "AgentProposer", "make_proposer", "ProposeError"]
