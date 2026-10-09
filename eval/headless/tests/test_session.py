"""The session adapter answers as the playbook's real session does (agents/policy_agent.py's
AgentSession, agents/scripted_agent.py's call): same objects, same exceptions."""

from __future__ import annotations

import asyncio
import json
from types import SimpleNamespace

import pytest

import scripted_agent
from eval.headless.sim import HeadlessSession


class FakeSim:
    tools = ["get_fleet", "propose_intent", "withdraw_intent"]

    def __init__(self, answers):
        self.answers = answers
        self.calls = []

    def mcp(self, agent, client, tool, args):
        self.calls.append((agent, client, tool, args))
        return self.answers[tool]


def sdk_result(structured, is_error):
    """What the MCP SDK hands AgentSession for the same answer (structured content and text)."""
    return SimpleNamespace(structured_content=structured, is_error=is_error,
                           content=[SimpleNamespace(type="text", text=json.dumps(structured))])


def test_a_read_is_the_structured_content():
    body = {"t": 1.0, "vehicles": [{"uid": 1}]}
    s = HeadlessSession(FakeSim({"get_fleet": {"is_error": False, "structured": body}}), "agent-playbook", "v")
    got = asyncio.run(s.call("get_fleet", compact=True))
    assert got == scripted_agent.content(sdk_result(body, False))
    assert s._sim.calls == [("agent-playbook", ("agent-playbook", "v"), "get_fleet", {"compact": True})]


def test_a_tool_error_raises_as_call_does():
    body = {"error": "unknown intent 7"}
    s = HeadlessSession(FakeSim({"withdraw_intent": {"is_error": True, "structured": body}}), "agent-playbook")
    with pytest.raises(RuntimeError) as here:
        asyncio.run(s.call("withdraw_intent", id=7, reason="x"))

    class Sess:
        async def call_tool(self, name, args):
            return sdk_result(body, True)

    with pytest.raises(RuntimeError) as there:
        asyncio.run(scripted_agent.call(Sess(), "withdraw_intent", id=7, reason="x"))
    assert str(here.value) == str(there.value)


def test_a_refused_proposal_comes_back_with_refused():
    body = {"error": "checks failed: geofence", "intent": {"id": 12, "state": "rejected"}}
    s = HeadlessSession(FakeSim({"propose_intent": {"is_error": True, "structured": body}}), "agent-playbook")
    out = asyncio.run(s.propose("why", task="orbit", uids=[3]))
    assert out == {"id": 12, "state": "rejected", "dispatched": False, "intent": body["intent"],
                   "refused": "checks failed: geofence"}
    assert s._sim.calls[0][3] == {"rationale": "why", "task": "orbit", "uids": [3]}


def test_a_protocol_error_raises():
    s = HeadlessSession(FakeSim({"get_fleet": {"protocol_error": "unknown tool"}}), "agent-playbook")
    with pytest.raises(RuntimeError, match="unknown tool"):
        asyncio.run(s.call("get_fleet"))
