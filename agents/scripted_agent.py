#!/usr/bin/env python3
"""A scripted agent for live testing the Murmur MCP server (Phase F).

Connects to the bridge's MCP endpoint as an agent, reads the fleet with every fact's age,
previews and proposes something sensible with a rationale, and follows what happens: an
auto-tier task (a hold on a routine, navigation-ready vehicle) is dispatched at once; a
human-tier task (an orbit for an aircraft around its own position, which for the stand-in
fleet lies outside the operator's approved area) waits in the operator's inbox until a human
approves or rejects it in the console or over the operator API. It never calls a model: the
"reasoning" is a few lines of Python, so the flow runs in demos and tests without one.
A real model connects the same way (agents/README.md).

    .venv/bin/python agents/scripted_agent.py                       # default ports
    .venv/bin/python agents/scripted_agent.py --url http://127.0.0.1:9865/mcp --agent agent-watch
    .venv/bin/python agents/scripted_agent.py --follow 180          # wait for the operator
    .venv/bin/python agents/scripted_agent.py --withdraw            # withdraw the pending one

Needs the official Python MCP SDK: `pip install "mcp>=2"` (in requirements.txt).
"""

import argparse
import asyncio
import json
import math
import sys
import time

from mcp import ClientSession, types
from mcp.client.streamable_http import streamable_http_client

try:  # the SDK's HTTP client (mcp >= 2 ships its own httpx fork)
    import httpx2 as httpx
except ImportError:  # pragma: no cover
    import httpx

AGENT_HEADER = "X-Murmur-Agent"


def content(result):
    """A tool result's JSON (structuredContent, else the first text block parsed)."""
    if getattr(result, "structuredContent", None) is not None:
        return result.structuredContent
    for block in result.content:
        if getattr(block, "type", "") == "text":
            try:
                return json.loads(block.text)
            except ValueError:
                return {"text": block.text}
    return {}


async def call(session, name, **args):
    result = await session.call_tool(name, args)
    body = content(result)
    if getattr(result, "isError", False):
        raise RuntimeError(f"{name}: {body.get('error', body)}")
    return body


def fmt_age(fact):
    age = fact.get("age_s")
    if age is None:
        return "unknown"
    return f"{age:.0f}s" + (" STALE" if fact.get("stale") else "")


def describe(v):
    p = v["platform"]
    pos = v["position"]
    where = "no position" if pos.get("lon") is None else f"{pos['lat']:.4f},{pos['lon']:.4f} ({fmt_age(pos)})"
    return (f"  uid {v['uid']:3d} {v['callsign']:12s} {p['id']:11s} {p['domain'] or '?':6s} "
            f"{'ARMED ' if p['armed'] else ''}nav {'ready' if v['nav']['ready'] else 'NOT READY'} "
            f"({v['nav']['source']}, {fmt_age(v['nav'])})  {where}  battery {v['battery'].get('pct')}% "
            f"({fmt_age(v['battery'])})  link {'LOST' if v['link']['lost'] else 'ok'}  "
            f"tasks {','.join(p['tasks'])}  on {v['current_op']['task'] if v['current_op'] else '-'}")


def taskable(v):
    return (v["nav"]["ready"] and not v["nav"]["stale"] and not v["position"]["stale"]
            and not v["link"]["lost"])


def pick_routine(vehicles):
    """A navigation-ready, unarmed vehicle that can hold, with fresh facts."""
    for v in vehicles:
        if taskable(v) and not v["platform"]["armed"] and "hold" in v["platform"]["tasks"]:
            return v
    return None


def pick_aircraft(vehicles):
    """A navigation-ready aircraft that can orbit (an orbit around its own position)."""
    for v in vehicles:
        if taskable(v) and v["platform"]["domain"] == "air" and "orbit" in v["platform"]["tasks"]:
            return v
    return None


def orbit_for(v):
    """An orbit around the aircraft's current position, wide enough for its airframe."""
    pos = v["position"]
    min_turn = v["platform"].get("min_turn_radius_m") or 0.0
    radius = max(2000.0, math.ceil(1.2 * min_turn / 100.0) * 100.0)
    return {"task": "orbit", "uids": [v["uid"]], "center": [pos["lon"], pos["lat"]], "radius_m": radius}


def show_preview(label, pv):
    print(f"preview {label}: passed={pv['passed']} would_reach={pv['would_reach']}")
    for c in pv["checks"]:
        if c["outcome"] != "pass":
            print(f"    {c['rule']} {c.get('uid') or ''}: {c['outcome']}: {c['detail']}")
    if pv.get("policy"):
        print(f"    policy {pv['policy']['tier']} ({pv['policy']['rule']}): {pv['policy']['reason']}")
    for why in pv.get("why_human") or []:
        print(f"    why a human: {why}")


async def follow(session, ids, seconds):
    """Poll explain_intent until every intent settles or time runs out."""
    last = {}
    deadline = time.monotonic() + seconds
    while ids and time.monotonic() < deadline:
        for i in list(ids):
            ex = await call(session, "explain_intent", id=i)
            state = ex["intent"]["state"]
            if last.get(i) != state:
                last[i] = state
                detail = ex["intent"]["history"][-1]["reason"]
                print(f"intent {i}: {state} ({detail})")
                if state == "pending_approval":
                    print(f"    waiting for an operator; approve with: curl -X POST "
                          f"{{bridge}}/api/intents/{i}/approve -H '{OPERATOR_HINT}'")
            if state in ("done", "failed", "rejected", "expired", "voided"):
                ids.remove(i)
        if ids:
            await asyncio.sleep(2)
    return last


OPERATOR_HINT = "X-Murmur-Operator: op-1"


async def run(args):
    http = httpx.AsyncClient(headers={AGENT_HEADER: args.agent}, timeout=httpx.Timeout(30, read=300))
    async with streamable_http_client(args.url, http_client=http) as (read, write):
        info = types.Implementation(name=args.agent, version="0.1")
        async with ClientSession(read, write, client_info=info) as session:
            init = await session.initialize()
            print(f"connected to {init.serverInfo.name} {init.serverInfo.version} as {args.agent}")
            tools = (await session.list_tools()).tools
            names = [t.name for t in tools]
            print(f"tools: {', '.join(names)}")
            assert not any("approve" in n for n in names), "an approve tool exists: that is a bug"

            fleet = await call(session, "get_fleet")
            print(f"fleet at t={fleet['t']:.0f}: {fleet['count']} vehicles")
            for v in fleet["vehicles"]:
                print(describe(v))
            inbox = await call(session, "get_inbox")
            print(f"inbox: {inbox['counts']['pending']} pending, {inbox['counts']['alerts']} alerts")

            proposals = []
            if args.uid is not None:
                v = next((v for v in fleet["vehicles"] if v["uid"] == args.uid), None)
                if v is None:
                    sys.exit(f"no vehicle with uid {args.uid}")
                req = orbit_for(v) if args.task == "orbit" else {"task": args.task, "uids": [args.uid]}
                proposals.append((args.task, req, f"operator-requested {args.task} test on {v['callsign']}"))
            else:
                if args.task in ("auto", "both"):
                    v = pick_routine(fleet["vehicles"])
                    if v is None:
                        print("no routine vehicle is ready for a hold")
                    else:
                        proposals.append(("hold", {"task": "hold", "uids": [v["uid"]]},
                                          f"{v['callsign']} is navigation-ready with fresh facts "
                                          f"(position {fmt_age(v['position'])}); hold it in place while "
                                          f"the picture settles. Routine: hold is an auto tier."))
                if args.task in ("human", "both"):
                    v = pick_aircraft(fleet["vehicles"])
                    if v is None:
                        print("no aircraft is ready for an orbit")
                    else:
                        proposals.append(("orbit", orbit_for(v),
                                          f"keep {v['callsign']} on station over its current position "
                                          f"({fmt_age(v['position'])} old) so its sensor keeps the "
                                          f"approach in view; the orbit is around where it already is."))

            ids = []
            for label, req, rationale in proposals:
                show_preview(label, await call(session, "preview_intent", **req))
                try:
                    out = await call(session, "propose_intent", rationale=rationale, **req)
                except RuntimeError as e:
                    print(f"propose {label}: refused: {e}")
                    continue
                print(f"propose {label}: intent {out['id']} is {out['state']} "
                      f"(dispatched={out['dispatched']})")
                print(f"    {out.get('summary')}")
                ids.append(out["id"])

            if ids and args.follow > 0:
                last = await follow(session, list(ids), args.follow)
            else:
                last = {}
            if args.withdraw:
                for i in ids:
                    if last.get(i) == "pending_approval":
                        out = await call(session, "withdraw_intent", id=i,
                                         reason="scripted agent finished; withdrawing its pending proposal")
                        print(f"withdrew intent {i}: {out['state']}")
            mine = await call(session, "get_intents", mine=True)
            print(f"my intents: " + ", ".join(f"{i['id']}:{i['state']}" for i in mine["intents"]))
            return ids


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--url", default="http://127.0.0.1:8765/mcp", help="the bridge's MCP endpoint")
    ap.add_argument("--agent", default="agent-planner", help="an id listed in MURMUR_AGENTS")
    ap.add_argument("--task", default="both",
                    help="auto (a hold), human (an orbit for an aircraft), both, or a task kind with --uid")
    ap.add_argument("--uid", type=int, help="task this vehicle instead of choosing one")
    ap.add_argument("--follow", type=float, default=60.0, help="seconds to follow the intents")
    ap.add_argument("--withdraw", action="store_true", help="withdraw a proposal still pending at the end")
    args = ap.parse_args()
    asyncio.run(run(args))


if __name__ == "__main__":
    main()
