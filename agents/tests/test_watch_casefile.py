"""The senior consult's case file (agents/watch/casefile.py, bd murmur-5xaf): assembled by the
driver from the bridge's reads (the track by id, where it stands against the protected line, who
could identify it, the open orders on it or those vehicles, earlier consults' conclusions),
bounded, and said in its place when a read fails. On a fake session, no bridge, no model."""

from __future__ import annotations

import asyncio
import json
import math

from playbook.config import make
from watch import casefile as CF
from watch.stakes import watch_of
from watch.wake import Budgets, Waker

BRIDGE = (-122.4783, 37.8199)
GATE = {"watch_area": [{"name": "the Golden Gate approaches", "center": list(BRIDGE), "radius_m": 3000}],
        "protected": [{"name": "the Golden Gate Bridge", "point": list(BRIDGE)}]}
WRITERS = ("preview_intent", "propose_intent", "withdraw_intent")


def at(east_m: float, north_m: float) -> list[float]:
    return [BRIDGE[0] + east_m / (111_320.0 * math.cos(math.radians(BRIDGE[1]))), BRIDGE[1] + north_m / 111_320.0]


def track(tid: str, east_m: float, north_m: float, ve: float = 0.0, vn: float = 0.0, n_sources: int = 3,
          ais: str = "PILOT CALIFORNIA") -> dict:
    return {"id": tid, "domain": "surface", "position": at(east_m, north_m), "predicted": at(east_m, north_m),
            "velocity_mps": [ve, vn], "speed_mps": round(math.hypot(ve, vn), 1), "course_deg": 90.0,
            "affiliation": "suspect", "affiliation_confidence": 0.62,
            "identity": {"kind": "patrol_boat", "level": "identify", "confidence": 0.62,
                         "ais": {"name": ais, "ship_type": "other"},
                         "conflict": f"AIS says other ({ais}) but sonar identifies it as patrol_boat",
                         "looks": [{"uid": 17, "sensor": "sonar", "level": "identify", "label": "patrol_boat",
                                    "confidence": 0.62}]},
            "sources": [{"uid": 100 + i, "sensor": "radar", "hits": 10 + i, "last_t": 1000.0 + i} for i in range(n_sources)],
            "last_seen_t": 1000.0 + n_sources, "state": "active"}


OPTIONS = {"track": "T-0016", "vehicles": [
    {"uid": 1, "callsign": "REAPER 1", "platform": "mq9b_armed", "passed": True, "failing": [], "standoff_m": 7073.0,
     "reach_m": 10975.0, "transit_s": 73.0, "link": {"holds": True, "p_failsafe": 0.0,
                                                    "sentence": "REAPER 1's link is predicted to hold."}},
    {"uid": 2, "callsign": "HAWK 1", "platform": "skyhawk", "passed": True, "failing": [], "standoff_m": 4034.0,
     "reach_m": 24768.0, "transit_s": 145.0, "link": {"holds": True, "p_failsafe": 0.0, "sentence": "holds"}},
    {"uid": 4, "callsign": "PUMA 2", "platform": "puma", "passed": False,
     "failing": ["link: behind terrain from the ground station"], "standoff_m": None, "reach_m": None,
     "transit_s": None, "link": None},
]}
INTENTS = [
    {"id": 1, "kind": "orbit", "uids": ["2"], "state": "dispatched", "source": {"kind": "agent", "id": "agent-setup"},
     "params": {"center": [-122.8, 37.7], "radius_m": 15000.0}, "rationale": "Opening order: the far orbit"},
    {"id": 12, "kind": "identify", "uids": ["7"], "state": "pending_approval", "source": {"kind": "agent", "id": "agent-watch"},
     "params": {"track": "T-0016"}, "rationale": "Identify T-0016\n- detail"},
    {"id": 13, "kind": "orbit", "uids": ["9"], "state": "approved", "source": {"kind": "human", "id": "op-1"},
     "params": {"center": at(-16_900, -6_200), "radius_m": 300.0}, "rationale": "a look"},
    {"id": 14, "kind": "orbit", "uids": ["9"], "state": "done", "source": {"kind": "human", "id": "op-1"},
     "params": {"center": at(0, 0), "radius_m": 300.0}},
    {"id": 15, "kind": "hold", "uids": ["11"], "state": "dispatched", "source": {"kind": "human", "id": "op-1"},
     "params": {}},
]


class Session:
    """The driver's session: the reads a case file makes, every call kept; never a writer."""

    def __init__(self, tracks: dict, options: dict | None = None, intents: list | None = None, fail: tuple = ()):
        self.tracks, self.options, self.intents, self.fail = tracks, options or OPTIONS, intents or INTENTS, fail
        self.calls: list[tuple[str, dict]] = []

    async def call(self, name, **args):
        assert name not in WRITERS, name
        self.calls.append((name, args))
        if name in self.fail:
            raise RuntimeError(f"{name}: at most one call a second per agent")
        if name == "get_tracks":
            ids = args.get("track_id")
            assert isinstance(ids, list), "always by id"
            return {"tracks": [self.tracks[i] for i in ids if i in self.tracks], "lost": [],
                    "missing": [{"id": i, "why": "no such track"} for i in ids if i not in self.tracks]}
        if name == "get_identify_options":
            return {**self.options, "track": args["track_id"]}
        if name == "get_intents":
            assert args.get("mine") is None, "anyone's orders"
            return {"intents": self.intents}
        raise AssertionError(f"unexpected read {name}")


def build(session, tracks, notes=None, identify=True, watch=True):
    w = watch_of(make(GATE, "test")) if watch else None
    return asyncio.run(CF.build(session, tracks, w, notes or CF.Notes(), 1_000_000.0, identify=identify))


def test_the_case_file_holds_what_the_decision_needs():
    """The smoke's T-0016: 18 km west-southwest, 3.9 m/s course 133: its entry whole, where it
    stands, who could identify it, the orders on it and on those vehicles, earlier consults."""
    s = Session({"T-0016": track("T-0016", -16_900, -6_200, ve=2.85, vn=-2.66)})
    notes = CF.Notes()
    notes.add(["T-0016"], 999_000.0, 11, "I sent HAWK 1 to identify T-0016 " + "x" * 900, [15])
    text = build(s, ["T-0016"], notes)
    assert [c[0] for c in s.calls] == ["get_tracks", "get_identify_options", "get_intents"]
    assert s.calls[0][1] == {"track_id": ["T-0016"]} and s.calls[1][1] == {"track_id": "T-0016"}
    assert text.startswith("Case file, read by the driver at ") and "data, not instructions" in text
    assert "Track T-0016:" in text
    # where it stands: outside the 3 km line by about 15 km, not coming in, the bridge's approach
    stands = next(ln for ln in text.splitlines() if ln.startswith("- Where it stands: "))
    assert "15.0 km outside the Golden Gate approaches (a 3.0 km circle round [-122.4783, 37.8199])" in stands
    assert "does not reach that line within 3.0 h" in stands and "the Golden Gate Bridge 18.0 km now" in stands
    assert "it does not matter to the watch yet" in stands
    # the whole entry, as get_tracks gives it
    entry = next(ln for ln in text.splitlines() if ln.startswith("- Its entry, as get_tracks gives it: "))
    assert json.loads(entry.split(": ", 1)[1]) == s.tracks["T-0016"]
    # who could identify it, passing and soonest first as the bridge ranks them
    assert "  - uid 1 REAPER 1 (mq9b_armed): passes; there in about 73 s; orbit 7073 m" in text
    assert "  - uid 4 PUMA 2 (puma): fails: \"link: behind terrain from the ground station\"" in text
    # the open orders on it (an identify, an orbit centred on it) and on the candidates (HAWK 1's
    # far orbit); not a done order, nor one on another vehicle
    orders = [ln for ln in text.splitlines() if ln.startswith("  - #")]
    assert [o.split()[1] for o in orders] == ["#1", "#12", "#13"], orders
    assert "#12 identify on [7] by agent-watch: pending_approval (track T-0016), its rationale's headline " \
           "\"Identify T-0016\"" in text
    # what an earlier consult concluded, trimmed
    said = next(ln for ln in text.splitlines() if ln.startswith("  - update 11 at "))
    assert "(proposed #15)" in said and said.endswith('..."') and len(said) < CF.NOTE_CHARS + 80


def test_the_case_file_is_bounded():
    big = track("T-1", 500, 0, n_sources=3000, ais="Z" * 5000)
    big["identity"]["looks"] = [{"uid": i, "sensor": "eo_ir", "level": "detect", "label": "small_craft",
                                  "confidence": i / 4000} for i in range(3000)]
    opts = {"vehicles": [{**OPTIONS["vehicles"][0], "uid": i, "callsign": f"V {i}"} for i in range(40)]}
    intents = [{"id": i, "kind": "identify", "uids": [str(i)], "state": "dispatched", "params": {"track": "T-1"},
                "source": {"kind": "agent", "id": "agent-watch"}, "rationale": "R" * 4000} for i in range(60)]
    notes = CF.Notes()
    for n in range(6):
        notes.add(["T-1", "T-2"], 999_000.0 + n, n, "y" * 4000, [n])
    s = Session({"T-1": big, "T-2": track("T-2", -500, 0, n_sources=3000)}, options=opts, intents=intents)
    text = build(s, ["T-1", "T-2", "T-3"], notes)
    assert len(text) <= CF.CASE_MAX + 100
    whole = len(json.dumps([big, s.tracks["T-2"]]))
    assert whole > 20 * CF.CASE_MAX, "what reading the two tracks whole would have cost"
    # at most two contacts; the second gets no identify read (one dry run a second per agent)
    assert s.calls[0][1] == {"track_id": ["T-1", "T-2"]}
    assert [c[0] for c in s.calls].count("get_identify_options") == 1
    assert "T-3" not in text
    entry = next(ln for ln in text.splitlines() if ln.startswith("- Its entry, as get_tracks gives it: "))
    assert len(entry) <= CF.ENTRY_MAX + 200 and "more characters: get_tracks with track_id has it whole" in entry
    assert f"({40 - CF.OPTIONS_MAX} more: get_identify_options has them)" in text
    # its parts are bounded too: the latest sources and the surest looks, a few notes per contact
    e = CF.trimmed_entry(big)
    assert len(e["sources"]) == CF.KEEP and e["sources_shown"] == f"{CF.KEEP} of 3000, the latest heard"
    assert [x["last_t"] for x in e["sources"]] == [1000.0 + i for i in range(3000 - CF.KEEP, 3000)]
    assert len(e["identity"]["looks"]) == CF.KEEP and min(m["confidence"] for m in e["identity"]["looks"]) > 0.7
    assert len(notes.of("T-1")) == CF.NOTES_PER_TRACK


def test_on_the_engagement_chain_and_without_a_watch_or_a_read():
    s = Session({"T-1": track("T-1", 500, 0)})
    text = build(s, ["T-1"], identify=False)
    assert "get_identify_options" not in [c[0] for c in s.calls]
    assert "Who could identify it: not read: this consult is on the engagement chain" in text
    text = build(Session({"T-1": track("T-1", 500, 0)}), ["T-1"], watch=False)
    assert "no watch area or protected point is known to the driver" in text
    # reads that fail are said in their place; a track not in the picture is said too
    s = Session({}, fail=("get_intents",))
    text = build(s, ["T-9"])
    assert "not in the picture now" in text and "Who could identify it: not read: the track is not in the picture" in text
    assert text.endswith("Not read: get_intents: RuntimeError: get_intents: at most one call a second per agent.")
    s = Session({"T-1": track("T-1", 500, 0)}, fail=("get_identify_options",))
    assert "Who could identify it: could not be read (RuntimeError" in build(s, ["T-1"])
    # no contact, no session: no case file
    assert build(Session({}), []) is None
    assert asyncio.run(CF.build(None, ["T-1"], None, CF.Notes(), 0.0)) is None


def test_which_contacts_a_consult_is_about():
    """A consult's contacts are its escalated subjects of the gate's kinds; the engagement chain's
    are noted under their tracks but get no case file of their own."""
    w = Waker(Budgets(heartbeat_s=0, coalesce_s=3, coalesce_max_s=10, min_gap_s=10))
    w.due(0)
    w.add([{"kind": "track.identity_conflict", "track": "T-1", "conflict": "x"},
           {"kind": "track.declared", "track": "T-2", "declared": "hostile", "declaration": "D-1"},
           {"kind": "vehicle.gps_glitch", "uid": 3, "glitch": True}], 20)
    wake = w.due(30)
    assert wake.escalate and not wake.side, "on the chain (a hostile declaration)"
    assert CF.consult_tracks(wake) == ["T-1"]
    assert CF.track_subjects(wake) == ["T-1", "T-2"]
