#!/usr/bin/env python3
"""A policy-driven scripted agent for the Murmur MCP server (Phase F/G). No model.

Two uses:

- As a library: `AgentSession` wraps the official MCP Python SDK session with the agent
  header and typed helpers (fleet, tracks, preview, propose, withdraw, explain). The
  evaluation harness (`eval/agent.py`) proposes through it.
- As a program: a rule-driven watch loop that reads the tracks and proposes a sensor orbit
  over every suspect or unknown track that comes within a radius of a point, once per
  track, with a rationale naming the facts and their ages. A proposal in a human tier
  waits in the operator's inbox; the agent cannot approve it (no such tool exists).

    .venv/bin/python agents/policy_agent.py --watch 120 --near=-122.478,37.818 --radius-km 4
    .venv/bin/python agents/policy_agent.py --url http://127.0.0.1:10600/mcp --once

Needs `mcp>=2` (requirements.txt). Nothing here calls an external model API.
"""

from __future__ import annotations

import argparse
import asyncio
import math
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from scripted_agent import agent_headers, attr, call, content, fmt_age  # noqa: E402

from mcp import ClientSession, types  # noqa: E402
from mcp.client.streamable_http import streamable_http_client  # noqa: E402

try:  # the SDK's HTTP client (mcp >= 2 ships its own httpx fork)
    import httpx2 as httpx
except ImportError:  # pragma: no cover
    import httpx

SUSPICION = {"hostile": 4, "suspect": 3, "unknown": 2, "neutral": 1, "assumed_friend": 0}


def haversine_m(lon1: float, lat1: float, lon2: float, lat2: float) -> float:
    r = 6371000.0
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dp = p2 - p1
    dl = math.radians(lon2 - lon1)
    a = math.sin(dp / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(dl / 2) ** 2
    return 2 * r * math.asin(math.sqrt(a))


class AgentSession:
    """An MCP session as one agent id. Use as `async with AgentSession(url, agent) as a:`."""

    def __init__(self, url: str = "http://127.0.0.1:8765/mcp", agent: str = "agent-planner", key: str | None = None):
        self.url = url
        self.agent = agent
        # the agent's API key on a bridge that authenticates (MURMUR_AGENT_KEY by default)
        self.key = key
        self.session: ClientSession | None = None
        self.tools: list[str] = []
        self._stack: list = []

    async def __aenter__(self) -> "AgentSession":
        http = httpx.AsyncClient(headers=agent_headers(self.agent, self.key), timeout=httpx.Timeout(30, read=300))
        transport = streamable_http_client(self.url, http_client=http)
        read, write = await transport.__aenter__()
        self._stack.append(transport)
        session = ClientSession(read, write, client_info=types.Implementation(name=self.agent, version="0.1"))
        await session.__aenter__()
        self._stack.append(session)
        await session.initialize()
        self.session = session
        self.tools = [t.name for t in (await session.list_tools()).tools]
        if any("approve" in n for n in self.tools):
            raise RuntimeError(f"the server offers an approve tool: {self.tools}")
        return self

    async def __aexit__(self, *exc) -> None:
        for cm in reversed(self._stack):
            try:
                await cm.__aexit__(None, None, None)
            except Exception:  # noqa: BLE001 (closing is best effort)
                pass
        self._stack.clear()
        self.session = None

    async def call(self, name: str, **args):
        if self.session is None:
            raise RuntimeError("session not open")
        return await call(self.session, name, **args)

    async def fleet(self) -> dict:
        return await self.call("get_fleet")

    async def tracks(self, **filters) -> list[dict]:
        return list((await self.call("get_tracks", **filters)).get("tracks", []))

    async def inbox(self) -> dict:
        return await self.call("get_inbox")

    async def intents(self, mine: bool = True) -> list[dict]:
        return list((await self.call("get_intents", mine=mine)).get("intents", []))

    async def preview(self, **req) -> dict:
        return await self.call("preview_intent", **req)

    async def propose(self, rationale: str, **req) -> dict:
        """propose_intent. A refusal that still created an intent (its checks failed: it
        ends rejected by the rules) comes back as that intent with `refused` and the
        reason, not as an exception, so the caller can follow it in the audit log."""
        if self.session is None:
            raise RuntimeError("session not open")
        result = await self.session.call_tool("propose_intent", {"rationale": rationale, **req})
        body = content(result)
        if attr(result, "is_error", "isError"):
            intent = body.get("intent") if isinstance(body, dict) else None
            if isinstance(intent, dict) and intent.get("id") is not None:
                return {"id": intent["id"], "state": intent.get("state"), "dispatched": False,
                        "intent": intent, "refused": body.get("error")}
            raise RuntimeError(f"propose_intent: {body.get('error', body) if isinstance(body, dict) else body}")
        return body

    async def withdraw(self, id: int, reason: str) -> dict:
        return await self.call("withdraw_intent", id=id, reason=reason)

    async def explain(self, id: int) -> dict:
        return await self.call("explain_intent", id=id)


def pick_sensor(vehicles: list[dict], platform: str | None) -> dict | None:
    """An unarmed, navigation-ready aircraft that can orbit (or the named platform)."""
    for v in vehicles:
        p = v["platform"]
        ok = v["nav"]["ready"] and not v["nav"]["stale"] and not v["link"]["lost"]
        if not ok or "orbit" not in p["tasks"]:
            continue
        if platform and p["id"] != platform:
            continue
        if not platform and (p["armed"] or p["domain"] != "air"):
            continue
        return v
    return None


def orbit_over(track: dict, sensor: dict) -> dict:
    min_turn = sensor["platform"].get("min_turn_radius_m") or 0.0
    radius = max(1000.0, math.ceil(1.2 * min_turn / 100.0) * 100.0)
    return {"task": "orbit", "uids": [sensor["uid"]], "center": list(track["position"]), "radius_m": radius}


def rationale_for(track: dict, sensor: dict, near: tuple[float, float], dist_m: float) -> str:
    ident = track.get("identity", {})
    kind = ident.get("kind") or "unclassified contact"
    return (
        f"Track {track['id']} ({kind}, estimated {track['affiliation']} "
        f"{track.get('affiliation_confidence', 0):.2f}: {track.get('affiliation_reason', '')}) is "
        f"{dist_m / 1000:.1f} km from the watch point at {near[0]:.4f},{near[1]:.4f}, last seen "
        f"{fmt_age(track)} ago. Put {sensor['callsign']}'s sensor on it to classify it before it "
        f"gets closer; the aircraft's position is {fmt_age(sensor['position'])} old."
    )


async def watch(a: AgentSession, near: tuple[float, float], radius_km: float, platform: str | None,
                seconds: float, every: float) -> list[int]:
    """Propose one sensor orbit per new suspect or unknown track inside the radius."""
    done: set[str] = set()
    ids: list[int] = []
    end = time.monotonic() + seconds
    while True:
        fleet = await a.fleet()
        sensor = pick_sensor(fleet["vehicles"], platform)
        tracks = await a.tracks(include_stale=False)
        candidates = []
        for t in tracks:
            if t["id"] in done or SUSPICION.get(t["affiliation"], 0) < 2:
                continue
            d = haversine_m(t["position"][0], t["position"][1], near[0], near[1])
            if d <= radius_km * 1000:
                candidates.append((SUSPICION[t["affiliation"]], -d, t, d))
        candidates.sort(key=lambda c: (c[0], c[1]), reverse=True)
        for _, _, t, d in candidates:
            if sensor is None:
                print("no sensor aircraft is ready for an orbit")
                break
            req = orbit_over(t, sensor)
            pv = await a.preview(**req)
            print(f"preview orbit over {t['id']}: passed={pv['passed']} would_reach={pv['would_reach']}")
            if not pv["passed"]:
                done.add(t["id"])
                continue
            out = await a.propose(rationale_for(t, sensor, near, d), **req)
            print(f"proposed intent {out['id']} ({out['state']}, dispatched={out['dispatched']}): {out.get('summary')}")
            ids.append(out["id"])
            done.add(t["id"])
            break  # one proposal per look: the operator's inbox is not a firehose
        if seconds <= 0 or time.monotonic() >= end:
            return ids
        await asyncio.sleep(every)


async def main_async(args) -> None:
    near = tuple(float(x) for x in args.near.split(","))
    if len(near) != 2:
        sys.exit("--near wants lon,lat")
    async with AgentSession(args.url, args.agent, args.key) as a:
        print(f"connected as {a.agent}; tools: {', '.join(a.tools)}")
        ids = await watch(a, (near[0], near[1]), args.radius_km, args.platform,
                          0 if args.once else args.watch, args.every)
        for i in ids:
            ex = await a.explain(i)
            print(f"intent {i}: {ex['intent']['state']}: {ex.get('summary')}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--url", default="http://127.0.0.1:8765/mcp")
    ap.add_argument("--agent", default="agent-planner")
    ap.add_argument("--key", help="the agent's API key (murmur-auth agent-add; default MURMUR_AGENT_KEY)")
    ap.add_argument("--near", default="-122.4780,37.8180", help="watch point, --near=lon,lat (one token: a longitude starts with a minus; default: the Gate)")
    ap.add_argument("--radius-km", type=float, default=4.0)
    ap.add_argument("--platform", help="the sensor platform id to task (default: any unarmed aircraft)")
    ap.add_argument("--watch", type=float, default=120.0, help="seconds to keep watching")
    ap.add_argument("--every", type=float, default=10.0, help="seconds between looks")
    ap.add_argument("--once", action="store_true", help="one look, then exit")
    asyncio.run(main_async(ap.parse_args()))


if __name__ == "__main__":
    main()
