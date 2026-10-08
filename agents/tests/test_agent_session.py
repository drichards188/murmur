"""AgentSession (agents/policy_agent.py) closes what it opened: on a failed start (N5: a caller
retrying every few seconds left a server session and a connection behind each time), and its
HTTP client on a normal exit too (the third review)."""

from __future__ import annotations

import asyncio

import pytest

import policy_agent as P


class Recorder:
    def __init__(self):
        self.events: list[str] = []


def fakes(rec: Recorder, fail_at: str):
    class Http:
        def __init__(self, **_):
            rec.events.append("http.open")

        async def aclose(self):
            rec.events.append("http.close")

    class Transport:
        async def __aenter__(self):
            rec.events.append("transport.open")
            return "read", "write"

        async def __aexit__(self, *exc):
            rec.events.append("transport.close")

    class Session:
        def __init__(self, *a, **k):
            pass

        async def __aenter__(self):
            rec.events.append("session.open")
            return self

        async def __aexit__(self, *exc):
            rec.events.append("session.close")

        async def initialize(self):
            if fail_at == "initialize":
                raise ConnectionError("the bridge went away")

        async def list_tools(self):
            if fail_at == "list_tools":
                raise TimeoutError("no answer")

            class T:
                def __init__(self, name):
                    self.name = name

            class R:
                tools = [T("get_fleet"), T("approve_intent")] if fail_at == "approve" else [T("get_fleet")]

            return R()

    return Http, (lambda url, http_client=None: Transport()), Session


@pytest.mark.parametrize("fail_at", ["initialize", "list_tools", "approve"])
def test_a_failed_start_closes_what_it_opened(monkeypatch, fail_at):
    rec = Recorder()
    http, transport, session = fakes(rec, fail_at)
    monkeypatch.setattr(P.httpx, "AsyncClient", http)
    monkeypatch.setattr(P, "streamable_http_client", transport)
    monkeypatch.setattr(P, "ClientSession", session)

    async def go():
        async with P.AgentSession("http://127.0.0.1:9/mcp", "agent-watch"):
            pass

    with pytest.raises((ConnectionError, TimeoutError, RuntimeError)):
        asyncio.run(go())
    assert rec.events == ["http.open", "transport.open", "session.open", "session.close", "transport.close",
                          "http.close"], rec.events


def test_a_good_start_keeps_the_session_open_until_the_block_ends(monkeypatch):
    rec = Recorder()
    http, transport, session = fakes(rec, "never")
    monkeypatch.setattr(P.httpx, "AsyncClient", http)
    monkeypatch.setattr(P, "streamable_http_client", transport)
    monkeypatch.setattr(P, "ClientSession", session)

    async def go():
        async with P.AgentSession("http://127.0.0.1:9/mcp", "agent-watch") as a:
            assert a.tools == ["get_fleet"]
            rec.events.append("inside")

    asyncio.run(go())
    assert rec.events[:4] == ["http.open", "transport.open", "session.open", "inside"]
    assert rec.events[4:] == ["session.close", "transport.close", "http.close"], "third review, item 9"
