"""The poller over a stand-in MCP session (agents/watch/picture.py): paging the change feed,
a bridge restart, the hostile tracks it keeps, nominations opening and closing."""

from __future__ import annotations

import asyncio
from types import SimpleNamespace

import pytest

from watch.picture import NoChangesTool, Poller

TOOLS = ["get_fleet", "get_tracks", "get_changes", "get_nomination_options", "propose_intent"]
NEAR = (-122.4800, 37.8200)
PLATFORMS = {"platforms": [{"id": "fpv", "cruise_mps": 25}, {"id": "magura", "cruise_mps": 12},
                           {"id": "x10", "cruise_mps": 12}, {"id": "mystery"}]}


def veh(uid, platform, armed=True, dx_km=1.0, link_lost=False, nav_ready=True):
    """A compact fleet entry dx_km east of NEAR."""
    return {"uid": uid, "callsign": f"{platform.upper()} {uid}", "platform": platform, "armed": armed,
            "lon": NEAR[0] + dx_km / 88.0, "lat": NEAR[1], "link_lost": link_lost, "nav_ready": nav_ready}


FLEET = [veh(2, "fpv")]


class FakeAgentSession:
    """What Poller uses of policy_agent.AgentSession, scripted."""

    def __init__(self, feed: list[dict], run: str = "run-a", tools=TOOLS, options=None, fleet=None):
        self.feed = feed  # get_changes answers, in order
        self.fleet = FLEET if fleet is None else fleet
        self.policy = {"policy": {"rules": {}}}  # no engagement section: the 900 s default
        self.run = run
        self.tools = list(tools)
        self.options = options or []  # get_nomination_options answers, in order
        self.calls: list[tuple] = []
        self.session = SimpleNamespace(call_tool=self._call_tool)

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc):
        return None

    async def call(self, name, **args):
        self.calls.append((name, args))
        if name == "get_changes" and "since" not in args:
            return {"cursor": 10, "run": self.run, "changes": []}
        if name == "get_platforms":
            return PLATFORMS
        if name == "get_policy":
            return self.policy
        if name == "get_fleet":
            return {"compact": args.get("compact"), "vehicles": self.fleet}
        return self.feed.pop(0)

    async def tracks(self, **_):
        at = {"position": list(NEAR), "speed_mps": 5.0}
        return [{"id": "T-0001", "declared": {"affiliation": "hostile"}, **at}, {"id": "T-0002", "declared": None, **at},
                {"id": "T-0003", "declared": None, **at}]

    async def intents(self, mine=True):
        return []

    async def _call_tool(self, name, args):
        self.calls.append((name, args))
        body = self.options.pop(0)
        return SimpleNamespace(structured_content=body, is_error=body.get("error") is not None, content=[])


def run(coro):
    return asyncio.run(coro)


def test_it_pages_the_feed_and_keeps_the_hostile_tracks():
    feed = [
        {"cursor": 12, "run": "run-a", "more": True, "changes": [
            {"seq": 11, "kind": "track.declared", "track": "T-0005", "declared": "hostile"},
            {"seq": 12, "kind": "vehicle.joined", "uid": 3}]},
        {"cursor": 13, "run": "run-a", "more": False, "changes": [
            {"seq": 13, "kind": "track.declaration_revoked", "track": "T-0001"}]},
    ]
    s = FakeAgentSession(feed)
    p = Poller(lambda: s)
    info = run(p.open())
    assert info == {"cursor": 10, "run": "run-a", "tools": TOOLS, "reset": False}
    assert p.hostile == {"T-0001"}, "declared hostile at the start"
    got = run(p.poll())
    assert [c["seq"] for c in got] == [11, 12, 13] and p.cursor == 13
    assert [a for n, a in s.calls if n == "get_changes"][1:] == [{"since": 10, "limit": 500}, {"since": 12, "limit": 500}]
    assert p.hostile == {"T-0005"}


def test_a_bridge_restart_is_a_change_and_a_reconnect_keeps_the_cursor():
    s = FakeAgentSession([{"cursor": 0, "run": "run-b", "reset": True, "changes": []}])
    p = Poller(lambda: s)
    run(p.open())
    got = run(p.poll())
    assert got[0]["kind"] == "feed.reset" and got[0]["was"] == "run-a" and p.run == "run-b"
    # a dropped session on the same bridge: the cursor stays where it was
    p.cursor = 42
    s.run = "run-b"
    assert run(p.open())["reset"] is False and p.cursor == 42
    # a new bridge: start again from its cursor
    s.run = "run-c"
    assert run(p.open())["reset"] is True and p.cursor == 10


def test_nominations_open_and_close():
    passing = {"options": [{"uid": 2, "callsign": "FPV 2", "passed": True}, {"uid": 3, "passed": False}]}
    none = {"options": [{"uid": 2, "callsign": "FPV 2", "passed": False}]}
    s = FakeAgentSession([], options=[passing, passing, none])
    p = Poller(lambda: s, nomination_every_s=15)

    run(p.open())
    got = run(p.nominations(now=100))
    assert got == [{"kind": "nomination.possible", "t": 100, "track": "T-0001",
                    "platforms": [{"uid": 2, "callsign": "FPV 2"}]}]
    assert run(p.nominations(now=105)) == [], "read at most every 15 s"
    assert run(p.nominations(now=116)) == [], "still possible: nothing new"
    assert run(p.nominations(now=132)) == [{"kind": "nomination.closed", "t": 132, "track": "T-0001"}]
    assert [a for n, a in s.calls if n == "get_nomination_options"] == [
        {"track_id": "T-0001", "uids": [2], "passed_only": True}] * 3
    assert [a for n, a in s.calls if n == "get_platforms"] == [{}], "the catalog read once"


def test_the_poll_asks_only_about_platforms_that_could_pass():
    """Third review, item 4: the driver's own poll asked about every armed platform, the
    213 557-character dry run at 100 stand-ins, every 15 s per hostile track. Now: armed, link
    up, navigation ready, within reach (cruise plus the track's speed over 900 s, or 30 km
    without a cruise figure), nearest first, and only whether each passes."""
    fleet = [veh(2, "fpv", dx_km=1), veh(3, "magura", dx_km=40),  # 40 km: beyond (12 + 5) x 900 s
             veh(4, "fpv", link_lost=True), veh(5, "fpv", nav_ready=False), veh(6, "x10", armed=False),
             veh(7, "mystery", dx_km=20), veh(8, "mystery", dx_km=35), veh(9, "magura", dx_km=0.5)]
    s = FakeAgentSession([], options=[{"options": [{"uid": 9, "callsign": "MAGURA 9", "passed": True}]}],
                         fleet=fleet)
    p = Poller(lambda: s, nomination_every_s=15)
    run(p.open())
    got = run(p.nominations(now=100))
    assert got[0]["kind"] == "nomination.possible" and got[0]["platforms"] == [{"uid": 9, "callsign": "MAGURA 9"}]
    assert [a for n, a in s.calls if n == "get_nomination_options"] == [
        {"track_id": "T-0001", "uids": [9, 2, 7], "passed_only": True}]
    assert ("get_fleet", {"compact": True}) in s.calls
    # nothing in reach any more: no dry run, and the nomination closes
    s.fleet = [veh(3, "magura", dx_km=40)]
    assert run(p.nominations(now=116)) == [{"kind": "nomination.closed", "t": 116, "track": "T-0001"}]
    assert len([n for n, _ in s.calls if n == "get_nomination_options"]) == 1
    assert [a for n, a in s.calls if n == "get_policy"] == [{}], "read once a session"


def test_the_reach_follows_the_policys_longest_transit():
    """Final review, B: the filter hard-coded 900 s; a policy with a longer max_transit_s let a
    platform pass that the driver never asked about, so nomination.possible never fired."""
    from watch.picture import max_transit_s

    assert max_transit_s({"policy": {"rules": {"engage": {"max_transit_s": 3600}}}}) == 3600.0
    assert max_transit_s({"policy": {"rules": {}}}) == 900.0 and max_transit_s({}) == 900.0
    far = [veh(3, "magura", dx_km=40)]  # beyond (12 + 5) x 900 s, within (12 + 5) x 3600 s
    s = FakeAgentSession([], options=[{"options": [{"uid": 3, "callsign": "MAGURA 3", "passed": True}]}], fleet=far)
    s.policy = {"policy": {"rules": {"engage": {"max_transit_s": 3600}}}}
    p = Poller(lambda: s, nomination_every_s=15)
    run(p.open())
    got = run(p.nominations(now=100))
    assert got and got[0]["kind"] == "nomination.possible", got
    assert [a["uids"] for n, a in s.calls if n == "get_nomination_options"] == [[3]]


def test_nomination_dry_runs_are_one_a_poll_and_never_stall_it():
    """Finding 19: the driver read every hostile track in one poll with a 1.05 s sleep after
    each, stalling change polling (and using the model's one-a-second allowance in a burst)."""
    passing = {"options": [{"uid": 2, "callsign": "FPV 2", "passed": True}]}
    s = FakeAgentSession([], options=[passing] * 3)
    p = Poller(lambda: s, nomination_every_s=15)
    run(p.open())
    p.hostile = {"T-0001", "T-0002", "T-0003"}
    import time as _t

    start = _t.monotonic()
    got = [run(p.nominations(now=100 + i)) for i in range(4)]
    assert _t.monotonic() - start < 0.5, "no sleeping"
    assert [[c["track"] for c in g] for g in got] == [["T-0001"], ["T-0002"], ["T-0003"], []]
    assert len([n for n, _ in s.calls if n == "get_nomination_options"]) == 3, "one a call"


def test_a_bridge_without_get_changes_is_refused():
    s = FakeAgentSession([], tools=[t for t in TOOLS if t != "get_changes"])
    with pytest.raises(NoChangesTool, match="rebuild it"):
        run(Poller(lambda: s).open())
