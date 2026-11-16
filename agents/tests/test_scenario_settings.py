"""The agents read the scenario's watch settings (docs/SCENARIO-SETTINGS.md, stage S5): MCP
`get_policy`'s top-level `watch`, else v0's (sim/policy-v0-watch.toml, through agents/roe.py's one
loader). Each reader is shown following a tailored `get_policy` (bar 0.75, approach horizon 240 s,
an aircraft without velocity at 30 m/s, a look within 700 m), and running on v0's when the bridge
gives no `watch`: the officer's doubt (picture.py `Poller.approach`), its approach wake (stakes.py
`Watch.entry`), its watch line (`of_run`), its case file (casefile.py) and the headless mapping of
its proposals (eval/headless/officer.py `map_proposal`); the playbook's config (`under`). On a
made-up clock, no bridge, no model."""

from __future__ import annotations

import asyncio
import math
from types import SimpleNamespace

import pytest

import roe as ROE
from playbook.config import ConfigError, make, under
from playbook.engine import Engine
from playbook_world import POLICY as PLAYBOOK_POLICY
from playbook_world import config, reads
from roe import FROM_POLICY, FROM_V0, Roe, v0, v0_watch
from watch import casefile as CF
from watch.picture import Poller
from watch.stakes import of_run, watch_of
from watch.wake import Budgets, Waker

BRIDGE = (-122.4783, 37.8199)
GATE = {"watch_area": [{"name": "the Golden Gate approaches", "center": list(BRIDGE), "radius_m": 3000}],
        "protected": [{"name": "the Golden Gate Bridge", "point": list(BRIDGE)}]}
#: a scenario's own settings: every value the officer reads changed from v0's
TAILORED = {**v0_watch(), "bar": 0.75, "approach_horizon_s": 240, "no_velocity_mps": {"air": 30.0}, "look_m": 700}
POLICY = {"t": 0.0, "policy": {"rules": {"engage": {"max_transit_s": 1800}}}, "watch": TAILORED}
OLD = {"t": 0.0, "policy": {"rules": {"engage": {"max_transit_s": 1800}}}}  # a bridge before rules 33


def at(east_m: float, north_m: float = 0.0) -> list[float]:
    return [BRIDGE[0] + east_m / (111_320.0 * math.cos(math.radians(BRIDGE[1]))), BRIDGE[1] + north_m / 111_320.0]


def contact(tid: str, east_m: float, domain: str = "surface", kind: str = "fishing_vessel", level: str = "identify",
            conf: float = 0.7, support=("eo_ir",), affiliation: str = "neutral", vel=None) -> dict:
    """A `get_tracks` entry east_m east of mid-span (west when negative)."""
    p = at(east_m)
    return {"id": tid, "domain": domain, "affiliation": affiliation, "position": p, "predicted": p,
            "velocity_mps": list(vel) if vel is not None else None,
            "speed_mps": round(math.hypot(*vel), 1) if vel is not None else 0.0,
            "course_deg": round(math.degrees(math.atan2(vel[0], vel[1])) % 360) if vel is not None else None,
            "last_seen_t": 100.0, "sources": [],
            "identity": {"kind": kind, "level": level, "confidence": conf, "conflict": None,
                         "supported_by": list(support)}}


class Session:
    """The driver's session: get_policy, get_changes and get_tracks by id; every call kept."""

    tools = ["get_changes", "get_tracks", "get_policy"]

    def __init__(self, policy: dict, tracks: dict | None = None):
        self.policy, self.by_id, self.calls = policy, tracks or {}, []

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc):
        return None

    async def call(self, name, **args):
        self.calls.append((name, args))
        if name == "get_policy":
            return self.policy
        if name == "get_changes":
            if "since" not in args:
                return {"cursor": "f:1", "run": "r", "changes": []}
            return {"cursor": "f:2", "run": "r", "more": False,
                    "changes": [{"kind": "track.new", "track": t, "affiliation": self.by_id[t]["affiliation"]}
                                for t in sorted(self.by_id)]}
        assert name == "get_tracks" and isinstance(args.get("track_id"), list)
        return {"tracks": [self.by_id[i] for i in args["track_id"] if i in self.by_id]}

    async def tracks(self, **_):
        return []


def officer(policy: dict, tracks: dict, table=GATE) -> tuple[Poller, Roe, object]:
    """A poller connected to a bridge serving `policy`, and what the officer takes from it."""
    s = Session(policy, tracks)
    p = Poller(lambda: s)
    asyncio.run(p.open())
    r, w = of_run(p.policy, make(table, "test") if table is not None else None, "the test's table", None)
    asyncio.run(p.poll())
    return p, r, w


def test_the_officer_reads_the_policy_once_a_session_and_takes_its_watch():
    p, r, w = officer(POLICY, {})
    assert [n for n, _ in p.session.calls].count("get_policy") == 1 and p.policy == POLICY
    assert p.transit_s == 1800.0, "the longest intercept comes from the same read"
    assert r.source == FROM_POLICY and (r.bar, r.horizon_s, r.look_m) == (0.75, 240.0, 700.0)
    assert r.no_velocity_mps == {"air": 30.0} and r.stale_s == 10.0 and r.still_mps == 0.5
    assert w is not None and w.roe is r
    # the stakes gate's horizon is the playbook's: twice the approach horizon
    assert w.cfg.t["tcpa_max_s"] == 480.0 and w.describe()["tcpa_max_s"] == 480.0
    # the live Officer keeps them on its state, and the stakes gate on the new watch
    import watch_officer as W

    o = W.Officer.__new__(W.Officer)
    o.poller, o.table, o.table_source, o.watch = SimpleNamespace(policy=POLICY), make(GATE, "test"), "t", None
    o.waker = Waker(Budgets())
    said = o.adopt()
    assert o.roe.bar == 0.75 and o.watch.roe is o.roe and o.waker.stakes == o.watch.gate
    assert said["source"] == "get_policy's watch" and said["area"] is None
    assert said["watch"]["watch_areas"] == ["the Golden Gate approaches"]


def test_without_watch_the_officer_runs_on_v0s():
    p, r, w = officer(OLD, {})
    assert r is v0() and r.source == FROM_V0
    v = v0_watch()
    assert (r.bar, r.horizon_s, r.look_m, r.stale_s, r.still_mps) == (
        v["bar"], v["approach_horizon_s"], v["look_m"], v["stale_s"], v["still_mps"])
    assert r.no_velocity_mps == v["no_velocity_mps"] and r.area is None
    table = make(GATE, "test")
    assert watch_of(table, "t", r).cfg is table, "v0's settings and a table with its areas: the table's own config"
    assert "the bridge's get_policy has no watch" in r.describe()["source"]
    assert ROE.V0_FILE.name == "policy-v0-watch.toml" and ROE.V0_FILE.is_file()


def test_a_garbled_watch_is_refused_never_replaced_by_v0s():
    for bad in ({"bar": 0.7}, "0.6", {**TAILORED, "look_m": "far"},
                {**TAILORED, "area": {"name": "x", "center": [1.0], "radius_m": 10}}):
        with pytest.raises(ValueError):
            Roe.of_policy({"watch": bad})


def test_doubt_follows_the_bar():
    """A fishing boat shown neutral, identified by our camera at 0.7, coming in: under v0's 0.6 it
    is out of doubt and let go; under the scenario's 0.75 it is in doubt and wakes the model."""
    boat = contact("T-1", -4000, vel=(10.0, 0.0))
    p, _, w = officer(POLICY, {"T-1": boat})
    (c,) = asyncio.run(p.approach(w, now=0.0))
    assert c["kind"] == "track.approaching" and c["identity_confidence"] == 0.7
    p, _, w = officer(OLD, {"T-1": boat})
    assert asyncio.run(p.approach(w, now=0.0)) == [] and p.following == {}


def test_the_approach_wake_follows_the_horizon_and_the_no_velocity_speed():
    tracks = {
        # 2800 m outside the circle, 10 m/s straight in: 280 s, within v0's 300, past the scenario's 240
        "T-1": contact("T-1", -5800, kind="small_fast_boat", conf=0.3, affiliation="unknown", vel=(10.0, 0.0)),
        # a drone 6600 m outside, no velocity: 220 s at the scenario's 30 m/s, 264 s at v0's 25 m/s
        "T-2": contact("T-2", -9600, domain="air", kind="small_air", level="detect", conf=0.9, support=("radar",),
                       affiliation="unknown"),
        # a drone 7400 m outside, slower than still_mps: 247 s at 30 (past 240), 296 s at 25 (within 300)
        "T-3": contact("T-3", -10_400, domain="air", kind="small_air", level="detect", conf=0.9, support=("radar",),
                       affiliation="unknown", vel=(0.3, 0.1)),
    }
    p, _, w = officer(POLICY, tracks)
    got = {c["track"]: c for c in asyncio.run(p.approach(w, now=0.0))}
    assert sorted(got) == ["T-2"]
    assert got["T-2"]["enters_s"] == 220 and got["T-2"]["basis"] == "no velocity yet: at 30 m/s straight in"
    p, _, w = officer(OLD, tracks)
    got = {c["track"]: c for c in asyncio.run(p.approach(w, now=0.0))}
    assert sorted(got) == ["T-1", "T-2", "T-3"]
    assert got["T-1"]["enters_s"] == 280 and got["T-2"]["enters_s"] == 264
    assert got["T-3"]["basis"] == "slower than 0.5 m/s: at 25 m/s straight in" and got["T-3"]["enters_s"] == 296


def test_the_watch_line_comes_from_the_policys_area():
    area = {"name": "the bay entrance", "center": at(-6000), "radius_m": 2000.0}
    policy = {**POLICY, "watch": {**TAILORED, "area": area}}
    _, r, w = officer(policy, {})
    assert r.area == {**area, "center": [float(x) for x in area["center"]]}
    assert [(a["name"], a["radius_m"]) for a in w.cfg["watch_area"]] == [("the bay entrance", 2000.0)]
    assert [p["name"] for p in w.cfg["protected"]] == ["the Golden Gate Bridge"], "the table's protected point"
    assert w.source.startswith("the scenario's [watch.area]")
    # no table: the circle, and its centre as the protected point
    _, _, w = officer(policy, {}, table=None)
    assert w is not None and [p["name"] for p in w.cfg["protected"]] == ["the bay entrance"]
    # no area: the table's, as before
    _, _, w = officer(POLICY, {})
    assert [a["name"] for a in w.cfg["watch_area"]] == ["the Golden Gate approaches"]
    assert officer(POLICY, {}, table=None)[2] is None


def test_the_case_file_follows_the_bar_and_the_look_distance():
    tailored, old = Roe.of_policy(POLICY), Roe.of_policy(OLD)
    t = contact("T-1", 1500, conf=0.7)
    assert CF.unsettled(t, tailored) == "fishing_vessel at 0.7, under 0.75"
    assert CF.unsettled(t, old) is None
    assert CF.unsettled({**t, "identity": {**t["identity"], "supported_by": ["sonar"]}}, old) == \
        "fishing_vessel by sonar, not by eo_ir or ais"
    where = tuple(t["position"])

    def orbit(off_m: float) -> dict:
        return {"id": 7, "kind": "orbit", "uids": ["9"], "state": "dispatched", "source": {"id": "op-1"},
                "params": {"center": at(1500 + off_m), "radius_m": 200.0}}

    assert [i["id"] for i in CF.touching([orbit(600)], "T-1", where, set(), tailored.look_m)] == [7]
    assert CF.touching([orbit(800)], "T-1", where, set(), tailored.look_m) == []
    assert [i["id"] for i in CF.touching([orbit(800)], "T-1", where, set(), old.look_m)] == [7]
    task = {"intent_id": 7, "task": "orbit", "state": "active"}
    assert CF.covers(9, orbit(800), task, [t], 100.0, tailored.stale_s, tailored.look_m) == {}
    assert list(CF.covers(9, orbit(800), task, [t], 100.0, old.stale_s, old.look_m)) == ["T-1"]


def test_the_headless_mapping_follows_the_look_distance():
    from eval.headless.officer import map_proposal

    t = {"id": "T-1", "affiliation": "unknown", "predicted": at(0), "identity": {"kind": "small_fast_boat"}}
    pic = {"tracks": [t], "vehicles": [{"uid": 3, "task": None}], "alerts": []}
    req = {"task": "orbit", "uids": [3], "center": at(800), "radius_m": 300}
    assert map_proposal(req, pic, {}, look_m=Roe.of_policy(POLICY).look_m)["play"] == "unjudged:orbit"
    assert map_proposal(req, pic, {}, look_m=Roe.of_policy(OLD).look_m)["play"] == "isr.investigate"
    assert map_proposal(req, pic, {})["target"] == "T-1", "without the officer's settings: v0's"


def test_the_playbook_runs_on_the_watch_settings():
    """classifier_fresh_s is the watch's stale_s and tcpa_max_s twice its approach horizon; the area
    where the table names none; the file's config, hash and all, under v0's."""
    cfg = config()
    assert under(cfg, v0()) is cfg
    run = under(cfg, Roe.of_policy({"watch": {**TAILORED, "stale_s": 12}}))
    assert run.t["classifier_fresh_s"] == 12.0 and run.t["tcpa_max_s"] == 480.0
    assert run.hash != cfg.hash and run.name == cfg.name and cfg.t["tcpa_max_s"] == 600.0
    area = {"name": "the bay entrance", "center": at(-6000), "radius_m": 2000.0}
    bare = make({}, "bare")
    run = under(bare, Roe.of_policy({"watch": {**TAILORED, "area": area}}))
    assert [a["name"] for a in run["watch_area"]] == ["the bay entrance"]
    assert run["protected"] == [{"name": "the bay entrance", "point": [float(x) for x in area["center"]]}]
    named = under(cfg, Roe.of_policy({"watch": {**TAILORED, "area": area}}))
    assert [a["name"] for a in named["watch_area"]] == ["the Golden Gate approaches"], "the table's own first"
    with pytest.raises(ConfigError, match="watch settings"):
        make({"thresholds": {"tcpa_max_s": 900}}, "x")
    # the engine plans on the config under each tick's policy; the file's stays for the label at connect
    e = Engine(cfg)
    e.plan(reads(policy={**PLAYBOOK_POLICY, "watch": TAILORED}))
    assert e.cfg.t["tcpa_max_s"] == 480.0 and e.file_cfg is cfg
    e.plan(reads())
    assert e.cfg is cfg, "a bridge without watch: v0's, the file's config itself"
