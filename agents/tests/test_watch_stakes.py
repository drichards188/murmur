"""The stakes gate (agents/watch/stakes.py, bd murmur-5xaf): an identity conflict, and a conflict
coming and going, goes to the stronger model only when its contact is inside a watch area,
closing on a protected point or coming into a watch area within the playbook's 10 minutes; the
engagement chain and every other escalating trigger are unchanged; without a position or a
watch area known, every escalation goes through as before. On a made-up clock, no bridge."""

from __future__ import annotations

import asyncio
import math

from playbook.config import make
from watch.stakes import Watch, watch_of
from watch.wake import Budgets, Waker

BRIDGE = (-122.4783, 37.8199)
GATE = {"watch_area": [{"name": "the Golden Gate approaches", "center": list(BRIDGE), "radius_m": 3000}],
        "protected": [{"name": "the Golden Gate Bridge", "point": list(BRIDGE)}]}


def at(east_m: float, north_m: float) -> list[float]:
    """A point this many metres east and north of mid-span (the playbook's local plane)."""
    return [BRIDGE[0] + east_m / (111_320.0 * math.cos(math.radians(BRIDGE[1]))), BRIDGE[1] + north_m / 111_320.0]


def conflict(track: str, east_m: float, north_m: float, speed: float | None = 0.0, course: float | None = 0.0,
             as_of: float = 20.0, **kw) -> dict:
    """A `track.identity_conflict` change as the feed gives it: the track's summary."""
    c = {"kind": "track.identity_conflict", "track": track, "conflict": "AIS says cargo but sonar identifies it as ferry",
         "position": at(east_m, north_m), "speed_mps": speed, "course_deg": course, "as_of": as_of, **kw}
    return {k: v for k, v in c.items() if v is not None}


def watch() -> Watch:
    w = watch_of(make(GATE, "test"))
    assert w is not None and w.known
    return w


def quiet() -> Budgets:
    return Budgets(heartbeat_s=0, coalesce_s=3, coalesce_max_s=10, min_gap_s=10, max_wakes_per_hour=60)


def wake_on(changes: list[dict], stakes=None, at_s: float = 20.0, escalation: bool = True):
    w = Waker(quiet(), escalation=escalation, stakes=stakes)
    assert w.due(0).reason == "start"
    w.add(changes, at_s)
    wake = w.due(at_s + 10)
    assert wake is not None and wake.reason == "changes"
    return w, wake


def test_a_conflict_inside_the_watch_area_escalates():
    w, wake = wake_on([conflict("T-1", 800, -400)], stakes=watch().gate)
    assert wake.escalate and wake.escalate_why == ["an identity conflict (possible AIS spoof)"]
    assert wake.gated == [] and w.escalations == 1


def test_a_conflict_closing_within_the_horizon_escalates():
    # 4 km west, east at 10 m/s: through mid-span in 400 s (the playbook's CPA within 457 m in 10 min)
    s = watch().of_change(conflict("T-2", -4000, 0, speed=10.0, course=90.0), 20.0)
    assert s.matters and s.closing is not None and s.why.startswith("closing on the Golden Gate Bridge")
    _, wake = wake_on([conflict("T-2", -4000, 0, speed=10.0, course=90.0)], stakes=watch().gate)
    assert wake.escalate
    # 1 km east and 4 km north, south at 5 m/s: passes 1 km from mid-span (no CPA alert) but comes
    # into the 3 km circle in about 234 s, inside the horizon: a stake the playbook's rule alone
    # would see only once it is in
    s = watch().of_change(conflict("T-3", 1000, 4000, speed=5.0, course=180.0), 20.0)
    assert s.closing is None and s.matters and s.why.startswith("coming into the Golden Gate approaches in 23")
    _, wake = wake_on([conflict("T-3", 1000, 4000, speed=5.0, course=180.0)], stakes=watch().gate)
    assert wake.escalate


def test_a_conflict_far_out_and_not_closing_is_a_routine_wake():
    """The smoke's case: a contact 18 km west-southwest of the bridge, 3.9 m/s course 133: it
    gets nearer, but not within 10 minutes. And one 25 km out opening."""
    for change in (conflict("T-0016", -16_900, -6_200, speed=3.9, course=133.0),
                   conflict("T-0007", -20_000, -15_000, speed=8.0, course=225.0)):
        w, wake = wake_on([change], stakes=watch().gate)
        assert not wake.escalate and wake.escalate_why == [] and w.escalations == 0
        assert wake.why == ["an identity conflict (possible AIS spoof)"], "it still wakes the routine model"
        (why,) = wake.gated
        assert why.startswith(f"track {change['track']}: ") and why.endswith("a routine wake, not the senior consult")
        assert "outside the Golden Gate approaches" in why and "not coming within 457 m" in why
        # nothing of the subject's escalation budget was spent: in the area later, it escalates
        w.add([conflict(change["track"], 500, 0, as_of=400.0, conflict="AIS says tanker")], 400.0)
        later = w.due(410.0)
        assert later is not None and later.escalate and w.escalations == 1


def test_the_position_is_carried_to_when_the_wake_is_judged():
    # last fixed 5 km west 200 s before, east at 20 m/s: about 1 km west now, inside
    c = conflict("T-4", -5000, 0, speed=20.0, course=90.0, as_of=-170.0)
    s = watch().of_change(c, 30.0)
    assert s.lines[0].inside and abs(s.lines[0].to_edge_m + 2000) < 50
    _, wake = wake_on([c], stakes=watch().gate)
    assert wake.escalate


def test_the_engagement_chain_and_other_escalations_are_unchanged():
    far = at(-25_000, 0)
    for change in (
        {"kind": "track.declared", "track": "T-9", "declared": "hostile", "by": "op-1", "position": far},
        {"kind": "nomination.possible", "track": "T-9", "platforms": [{"uid": 1}]},
        {"kind": "vehicle.gps_glitch", "uid": 3, "glitch": True},
        {"kind": "vehicle.link_lost", "uid": 1, "armed": True, "engaging": True},
        {"kind": "vehicle.link_lost", "uid": 1, "armed": True},
    ):
        _, wake = wake_on([change], stakes=watch().gate)
        assert wake.escalate and wake.gated == [], change


def test_without_a_position_or_an_area_known_every_conflict_escalates_as_before():
    no_pos = {"kind": "track.identity_conflict", "track": "T-5", "conflict": "AIS says tanker"}
    _, wake = wake_on([no_pos], stakes=watch().gate)
    assert wake.escalate
    # no table, or one with neither a watch area nor a protected point: no gate at all
    assert watch_of(None) is None and watch_of(make({}, "empty")) is None
    _, wake = wake_on([conflict("T-0016", -16_900, -6_200, speed=3.9, course=133.0)], stakes=None)
    assert wake.escalate
    # a judge that fails lets it through
    def broken(c, now):
        raise ValueError("no")
    _, wake = wake_on([conflict("T-0016", -16_900, -6_200, speed=3.9, course=133.0)], stakes=broken)
    assert wake.escalate


def test_with_escalation_off_the_gate_says_nothing():
    _, wake = wake_on([conflict("T-0016", -16_900, -6_200, speed=3.9, course=133.0)], stakes=watch().gate,
                      escalation=False)
    assert not wake.escalate and wake.gated == []


def test_a_conflict_coming_and_going_is_judged_where_the_contact_is_now():
    def flap(track: str, east_m: float) -> tuple[Waker, object]:
        w = Waker(quiet(), stakes=watch().gate)
        w.due(0)
        t = 20.0
        for _ in range(2):  # raised and cleared twice inside the flap window
            w.add([conflict(track, east_m, 0, as_of=t)], t)
            w.add([{"kind": "track.identity_cleared", "track": track, "position": at(east_m, 0), "speed_mps": 0.0,
                    "course_deg": 0.0, "as_of": t + 5}], t + 5)
            t += 20
        wake = w.due(t + 20)
        assert wake is not None
        flapping = [c for c in wake.changes if c["kind"] == "condition.flapping"]
        assert flapping and flapping[0]["position"] == at(east_m, 0), "the clear's summary rides on it"
        return w, wake

    _, near = flap("T-6", 1500)
    assert near.escalate and "a condition keeps coming and going" in near.escalate_why
    _, far = flap("T-7", -20_000)
    assert not far.escalate and any(g.startswith("track T-7: ") for g in far.gated)


def test_the_standing_reads_a_get_tracks_entry_too():
    w = watch()
    t = {"id": "T-8", "position": at(-6000, 0), "predicted": at(-5000, 0), "velocity_mps": [10.0, 0.0],
         "speed_mps": 10.0, "course_deg": 90.0}
    s = w.of_track(t)
    assert s is not None and not s.lines[0].inside
    assert abs(s.lines[0].to_edge_m - 2000) < 50, "judged at the position carried to now"
    assert s.lines[0].enters_s is not None and abs(s.lines[0].enters_s - 200) < 5
    d = s.as_dict()
    assert d["lines"][0]["area"] == "the Golden Gate approaches" and d["protected"][0]["point"] == "the Golden Gate Bridge"
    assert w.of_track({"id": "T-9"}) is None
    # a polygon watch area: in, and the way in found along the course
    square = make({"watch_area": [{"name": "the box", "polygon": [at(-1000, -1000), at(1000, -1000), at(1000, 1000),
                                                                  at(-1000, 1000)]}]}, "box")
    pw = watch_of(square)
    assert pw.standing(tuple(at(0, 0)), None).lines[0].inside
    s = pw.standing(tuple(at(-3000, 0)), (10.0, 0.0))
    assert not s.lines[0].inside and s.lines[0].enters_s == 200.0 and s.matters
    assert abs(s.lines[0].to_edge_m - 2000) < 20


class Session:
    """The driver's session as `Poller.stakes` reads it: get_tracks by id, every call kept."""

    def __init__(self, tracks: dict):
        self.tracks = tracks
        self.calls: list[tuple[str, dict]] = []

    async def call(self, name, **args):
        self.calls.append((name, args))
        assert name == "get_tracks" and isinstance(args.get("track_id"), list)
        return {"tracks": [self.tracks[i] for i in args["track_id"] if i in self.tracks]}


def entry(track: str, east_m: float, north_m: float, ve: float = 0.0, vn: float = 0.0, conflict: str | None = "x"):
    return {"id": track, "domain": "surface", "position": at(east_m, north_m), "predicted": at(east_m, north_m),
            "velocity_mps": [ve, vn], "speed_mps": math.hypot(ve, vn), "course_deg": 0.0, "last_seen_t": 100.0,
            "affiliation": "suspect", "identity": {"kind": "patrol_boat", "level": "identify", "conflict": conflict}}


def test_a_gated_contact_that_comes_in_escalates_then():
    """The gate judges a change once: a contact gated far out is read again by id until it matters
    (with its conflict standing: a `track.conflict_matters` change, which escalates) or its
    conflict clears; nothing is read without a gated contact, or more often than every 10 s."""
    import asyncio

    from watch.picture import Poller, render

    w = watch()
    p = Poller(lambda: None)
    p.session = Session({"T-1": entry("T-1", -20_000, 0), "T-2": entry("T-2", -15_000, 0, conflict="y")})
    assert asyncio.run(p.stakes(w, now=0.0)) == [] and p.session.calls == [], "nothing gated: nothing read"
    # the Waker gates both far out; the driver watches what the wake names
    waker, wake = wake_on([conflict("T-1", -20_000, 0), conflict("T-2", -15_000, 0)], stakes=w.gate)
    assert not wake.escalate and wake.gated_tracks == ["T-1", "T-2"]
    p.watch_gated(wake.gated_tracks, 30.0)
    assert asyncio.run(p.stakes(w, now=40.0)) == []
    assert p.session.calls[-1] == ("get_tracks", {"track_id": ["T-1", "T-2"]})
    assert asyncio.run(p.stakes(w, now=45.0)) == [] and len(p.session.calls) == 1, "every 10 s at most"
    # T-1 comes in; T-2's conflict clears (its next read lets it go)
    p.session.tracks["T-1"] = entry("T-1", -2000, 0, ve=5.0)
    p.session.tracks["T-2"] = entry("T-2", -14_000, 0, conflict=None)
    (c,) = asyncio.run(p.stakes(w, now=50.0))
    assert c["kind"] == "track.conflict_matters" and c["track"] == "T-1"
    assert c["standing"] == "inside the Golden Gate approaches"
    assert p.gated == {}, "both let go: one came in, one's conflict cleared"
    assert "its identity conflict still stands and it now matters to the watch" in render(c)
    waker.add([c], 60.0)
    later = waker.due(70.0)
    assert later is not None and later.escalate
    assert later.escalate_why == ["an identity conflict on a contact that now matters to the watch (possible AIS spoof)"]
    # a cleared conflict or a dropped track in the feed lets a gated contact go at once
    p.watch_gated(["T-3", "T-4"], 80.0)
    p.cursor, p.run = "f:1", "r"

    class Feed:
        async def call(self, name, **args):
            return {"changes": [{"kind": "track.identity_cleared", "track": "T-3"},
                                {"kind": "track.dropped", "track": "T-4"}], "cursor": "f:3", "more": False}

    p.session = Feed()
    asyncio.run(p.poll())
    assert p.gated == {}


# ---- a contact in doubt at the watch (bd murmur-1h2r, murmur-jhgn) ---------------------------------


def air(track: str, east_m: float, north_m: float, velocity=None, affiliation: str = "unknown",
        confidence: float | None = 0.3, conflict: str | None = None) -> dict:
    """A `get_tracks` entry of a small drone as the tracker gives it before it has a velocity
    (`velocity_mps` and `course_deg` null; the feed's speed is 0.0 then)."""
    e = entry(track, east_m, north_m, conflict=conflict)
    e.update({"domain": "air", "affiliation": affiliation, "velocity_mps": velocity, "pos_sigma_now_m": 60.0,
              "speed_mps": 0.0 if velocity is None else math.hypot(*velocity),
              "course_deg": None if velocity is None else 90.0})
    e["identity"] = {"kind": "drone", "level": "detect", "confidence": confidence, "conflict": conflict}
    return e


def follow(p, *tracks: str, affiliation: str = "unknown"):
    p.cursor, p.run = "f:1", "r"

    class Feed:
        async def call(self, name, **args):
            return {"changes": [{"kind": "track.new", "track": t, "affiliation": affiliation} for t in tracks],
                    "cursor": "f:2", "more": False}

    p.session = Feed()
    asyncio.run(p.poll())


def test_an_air_track_with_no_velocity_wakes_once_approaching_then_once_inside():
    from watch.picture import Poller, render

    w = watch()
    p = Poller(lambda: None)
    follow(p, "T-1")
    p.session = Session({"T-1": air("T-1", -4500, 0)})  # first sight, 1.5 km outside the 3 km circle
    (c,) = asyncio.run(p.approach(w, now=0.0))
    assert c["kind"] == "track.approaching" and c["track"] == "T-1"
    assert c["enters_s"] == 60 and abs(c["to_edge_m"] - 1500) < 20
    assert c["basis"] == "no velocity yet: at 25 m/s straight in"
    line = render(c)
    assert "track T-1 (air) is 1495 m outside the Golden Gate approaches and predicted to come inside in 60 s " \
           "(no velocity yet: at 25 m/s straight in)" in line
    assert "identity drone (detect, confidence 0.30), estimated unknown" in line
    assert "no velocity yet, position uncertainty 60 m" in line and "0.0 m/s" not in line
    # once: still outside, still approaching, read again: nothing
    assert asyncio.run(p.approach(w, now=10.0)) == []
    p.session.tracks["T-1"] = air("T-1", -3500, 0)
    assert asyncio.run(p.approach(w, now=20.0)) == []
    # inside: once, then it is let go
    p.session.tracks["T-1"] = air("T-1", -2000, 0)
    (d,) = asyncio.run(p.approach(w, now=30.0))
    assert d["kind"] == "track.in_watch" and "is inside the Golden Gate approaches, 1002 m past its edge" in render(d)
    assert p.following == {}
    assert asyncio.run(p.approach(w, now=40.0)) == []
    # the Waker wakes the routine model for each, never the stronger one
    for ch in (c, d):
        wk = Waker(quiet())
        wk.due(0)
        wk.add([ch], 20.0)
        x = wk.due(24.0)
        assert x is not None and not x.escalate


def test_a_track_straight_inside_wakes_inside_only():
    from watch.picture import Poller

    p = Poller(lambda: None)
    follow(p, "T-1")
    p.session = Session({"T-1": air("T-1", -1000, 0)})
    (c,) = asyncio.run(p.approach(watch(), now=0.0))
    assert c["kind"] == "track.in_watch" and p.following == {}


def test_a_neutral_or_identified_contact_does_not_wake():
    from watch.picture import Poller

    w = watch()
    p = Poller(lambda: None)
    follow(p, "T-1", "T-2", "T-3")
    p.session = Session({"T-1": air("T-1", -4500, 0, affiliation="neutral"),
                         "T-2": air("T-2", -4500, 0, confidence=0.9),
                         "T-3": air("T-3", -4500, 0, confidence=0.9, conflict="AIS says cargo")})
    out = asyncio.run(p.approach(w, now=0.0))
    assert [c["track"] for c in out] == ["T-3"], "a conflict is in doubt whatever the confidence"
    follow(p, "T-4", affiliation="friendly")
    assert "T-4" not in p.following and "T-1" not in p.following and "T-2" not in p.following


def test_a_ferry_far_off_and_moving_away_does_not_wake():
    from watch.picture import Poller

    w = watch()
    p = Poller(lambda: None)
    follow(p, "T-1", "T-2", "T-3")
    ferry = entry("T-1", -8000, 0, ve=-6.0, vn=0.0)  # 8 km out, going away
    ferry["identity"]["confidence"] = 0.3
    still = entry("T-2", -8000, 0)  # surface, no velocity: judged where it is
    still["velocity_mps"], still["course_deg"], still["identity"]["confidence"] = None, None, 0.3
    far_drone = air("T-3", -8000, 0)  # in the air, no velocity, 8 km: 5000 m at 25 m/s is 200 s, past 180
    p.session = Session({"T-1": ferry, "T-2": still, "T-3": far_drone})
    assert asyncio.run(p.approach(w, now=0.0)) == []
    assert set(p.following) == {"T-1", "T-2", "T-3"}, "still followed"
    p.session.tracks["T-3"] = air("T-3", -7000, 0)  # 4000 m past the edge is 160 s: within the horizon
    (c,) = asyncio.run(p.approach(w, now=10.0))
    assert c["track"] == "T-3" and c["enters_s"] == 160


def test_a_moving_track_is_predicted_on_its_velocity_and_nothing_without_a_watch_area():
    from watch.picture import Poller, render

    w = watch()
    p = Poller(lambda: None)
    follow(p, "T-1")
    p.session = Session({"T-1": air("T-1", -5000, 0, velocity=[20.0, 0.0])})
    (c,) = asyncio.run(p.approach(w, now=0.0))
    assert c["enters_s"] == 100 and c["basis"] == "its velocity"
    assert "20.0 m/s course 90" in render(c)
    q = Poller(lambda: None)
    follow(q, "T-1")
    q.session = Session({"T-1": air("T-1", -1000, 0)})
    assert asyncio.run(q.approach(None, now=0.0)) == [] and q.session.calls == []


def test_a_contact_inside_is_not_told_again_when_an_affiliation_change_follows_it_anew():
    """T-0008 of the paused v2 record was told `track.in_watch` at 243 s and again at 546 s: it was
    let go once inside, an affiliation change followed it again, and the next read told it again."""
    from watch.picture import Poller

    w = watch()
    p = Poller(lambda: None)
    follow(p, "T-1")
    p.session = Session({"T-1": air("T-1", -2000, 0)})
    (c,) = asyncio.run(p.approach(w, now=0.0))
    assert c["kind"] == "track.in_watch" and p.following == {}
    # an affiliation change follows it again
    p.session = Session({"T-1": air("T-1", -1500, 0)})
    p.cursor, p.run = "f:2", "r"
    poll_session = p.session

    class Feed:
        async def call(self, name, **args):
            return {"changes": [{"kind": "track.affiliation", "track": "T-1", "from": "unknown", "to": "suspect"}],
                    "cursor": "f:3", "more": False}

    p.session = Feed()
    asyncio.run(p.poll())
    assert "T-1" in p.following
    p.session = poll_session
    assert asyncio.run(p.approach(w, now=100.0)) == [] and p.following == {}
    # and one told approaching is not told it again after being followed anew
    follow(p, "T-2")
    p.session = Session({"T-2": air("T-2", -4500, 0)})
    assert [x["kind"] for x in asyncio.run(p.approach(w, now=200.0))] == ["track.approaching"]
    follow(p, "T-2")
    assert asyncio.run(p.approach(w, now=300.0)) == []
