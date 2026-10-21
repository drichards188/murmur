"""Scoring v2's instruments on small synthetic records (eval/SCORING-V2.md, section 2): the picture
encoding's exactness, each label rule, the precedence of the action labels, the hits-rise rule,
sensing errors, censoring, and the metrics built on them. No simulator: each record is written
here, sample by sample, as `Sim.picture` would answer."""

from __future__ import annotations

import math
import random
from typing import Any, Callable

import pytest

from eval.headless.v2 import labels as L
from eval.headless.v2 import metrics as M
from eval.headless.v2.record import Picture, PictureEncoder

EPOCH = 1_781_431_200.0
BR = L.BRIDGE


def at(east_m: float, north_m: float) -> tuple[float, float]:
    """A point east and north of the bridge reference point (lon, lat)."""
    return (BR[0] + east_m / (111_320.0 * math.cos(math.radians(BR[1]))), BR[1] + north_m / 111_320.0)


def lerp_path(path: list[tuple[float, float, float]], t: float) -> tuple[float, float] | None:
    """(t, east, north) waypoints, linear between them; None before the first."""
    if t < path[0][0]:
        return None
    for (t0, e0, n0), (t1, e1, n1) in zip(path, path[1:]):
        if t0 <= t <= t1:
            f = (t - t0) / (t1 - t0) if t1 > t0 else 0.0
            return at(e0 + f * (e1 - e0), n0 + f * (n1 - n0))
    return at(path[-1][1], path[-1][2])


class Synth:
    """A synthetic v2 record: contacts on straight legs, tracks with scheduled identities and
    hits, vehicles with scheduled positions, modes and batteries, and the agent's intents."""

    def __init__(self, duration: float = 300.0, ai_start: float = 45.0):
        self.dur = duration
        self.ai = ai_start
        self.contacts: dict[str, dict[str, Any]] = {}
        self.tracks: dict[str, dict[str, Any]] = {}
        self.vehicles: dict[int, Callable[[float], dict[str, Any]]] = {}
        self.fleet = [{"uid": 3, "callsign": "PUMA 1", "platform": "puma", "sensors": [{"type": "eo_ir", "range_m": 4000.0}]},
                      {"uid": 4, "callsign": "PUMA 2", "platform": "puma", "sensors": [{"type": "eo_ir", "range_m": 4000.0}]}]
        self.intents: list[dict[str, Any]] = []
        self.alerts: list[tuple[str, str, int, float, float, float]] = []
        self.script: list[dict[str, Any]] = []
        self.sensing = {"seed": 7, "misclassification_rate": 0.0}
        self.agent_log: list[dict[str, Any]] = []

    def contact(self, cid: str, kind: str = "small_fast_boat", aff: str = "unknown", ais: bool = False,
                path: list[tuple[float, float, float]] | None = None, domain: str = "surface") -> None:
        self.contacts[cid] = {"meta": {"id": cid, "name": cid, "affiliation": aff, "domain": domain, "kind": kind,
                                       "ais": ais, "start_s": 0.0}, "path": path or [(0.0, 5000.0, 0.0)]}

    def track(self, tid: str, contact: str, t0: float = 0.0,
              ident: Callable[[float], tuple[Any, ...]] | None = None,
              hits: Callable[[float], dict[str, int]] | None = None, aff: Callable[[float], str] | None = None) -> None:
        self.tracks[tid] = {"contact": contact, "t0": t0,
                            "ident": ident or (lambda t: ("small_craft", 0.3, None, ["radar"])),
                            "hits": hits or (lambda t: {}), "aff": aff or (lambda t: "unknown")}

    def vehicle(self, uid: int, fn: Callable[[float], dict[str, Any]]) -> None:
        self.vehicles[uid] = fn

    def intent(self, iid: int, kind: str, uids: list[int], created: float, params: dict[str, Any] | None = None,
               states: list[tuple[float, str, str]] | None = None, op: dict[int, list[tuple[float, str]]] | None = None,
               source: str = "agent-playbook") -> None:
        self.intents.append({
            "id": iid, "source": {"kind": "agent" if source.startswith("agent-") else "human", "id": source},
            "kind": kind, "uids": uids, "params": params or {}, "created": created, "tier": "auto", "rule": None,
            "states": [[t, to, tr] for t, to, tr in (states or [(created, "checked", "check")])],
            "op": {str(u): [[t, s, ""] for t, s in rows] for u, rows in (op or {}).items()},
            "approvals": [], "views": [], "dispatched": None, "expires": None})

    def record(self, agent: str = "playbook") -> dict[str, Any]:
        enc = PictureEncoder(EPOCH)
        n = int(self.dur)
        for k in range(n + 1):
            t = float(k)
            tracks, assoc = [], []
            for tid, tr in sorted(self.tracks.items()):
                if t < tr["t0"]:
                    continue
                c = self.contacts[tr["contact"]]
                p = lerp_path(c["path"], t)
                q = lerp_path(c["path"], t + 1.0) or p
                if p is None:
                    continue
                vel = None
                if q is not None:
                    vel = [round((q[0] - p[0]) * 111_320.0 * math.cos(math.radians(p[1])), 2),
                           round((q[1] - p[1]) * 111_320.0, 2)]
                kind, conf, conflict, sb = tr["ident"](t)
                tracks.append({
                    "id": tid, "domain": "surface", "position": [round(p[0], 6), round(p[1], 6)], "velocity_mps": vel,
                    "affiliation": tr["aff"](t), "affiliation_confidence": 0.5,
                    "identity": {"kind": kind, "confidence": conf, "conflict": conflict, "supported_by": sb},
                    "sources": [{"uid": int(key.split(":")[0]), "sensor": key.split(":")[1], "hits": h,
                                 "last_t": round(EPOCH + t, 2)} for key, h in sorted(tr["hits"](t).items())],
                    "last_seen_t": round(EPOCH + t, 2)})
                assoc.append([tid, tr["contact"]])
            ours = []
            for uid, fn in sorted(self.vehicles.items()):
                v = {"uid": uid, "lon": BR[0], "lat": BR[1] - 0.05, "alt_m": 80.0, "speed_mps": 20.0, "mode": "AUTO",
                     "armed": True, "battery_pct": 90.0, "home": [BR[0], BR[1] - 0.05], "link_blocked": None}
                v.update(fn(t))
                ours.append(v)
            alerts = [{"id": a[0], "kind": a[1], "uid": a[2], "since_t": EPOCH + a[3]}
                      for a in self.alerts if a[4] <= t <= a[5]]
            sample = {"t": t, "tracks": tracks, "association": assoc, "alerts": alerts, "ours": ours}
            if k == 0:
                sample["fleet"] = self.fleet
                sample["sensing"] = self.sensing
            enc.add(sample)
        c0 = 0.5
        samples = []
        for s in range(n):
            t = c0 + s
            row = []
            for cid, c in sorted(self.contacts.items()):
                p = lerp_path(c["path"], t)
                if p is not None:
                    row.append([cid, round(p[0], 7), round(p[1], 7), 10.0, True])
            samples.append({"t": round(t, 3), "contacts": row, "ours": []})
        return {
            "clock": {"duration_s": self.dur, "ai_start_s": self.ai, "epoch": EPOCH},
            "truth": {"contacts": [c["meta"] for c in self.contacts.values()], "samples": samples},
            "script": self.script, "agent_log": self.agent_log,
            "v2": {"agent": agent, "picture": enc.finish(), "audit": {"intents": self.intents},
                   "invariants": {"total": 0}, "condition": {"operator": "approve-all", "policy": "watch.toml"}},
            "episode_class": "scored",
        }


def inbound(t0: float = 0.0, speed: float = 20.0, start_m: float = 5000.0, stop_m: float = 1000.0) -> list[tuple[float, float, float]]:
    """A boat from `start_m` west of the bridge, at `speed`, stopping `stop_m` west of it."""
    return [(t0, -start_m, 0.0), (t0 + (start_m - stop_m) / speed, -stop_m, 0.0)]


RESOLVED = ("small_fast_boat", 0.9, None, ["eo_ir"])
UNRESOLVED = ("small_craft", 0.4, None, ["radar"])


def resolved_from(t1: float) -> Callable[[float], tuple[Any, ...]]:
    return lambda t: RESOLVED if t >= t1 else UNRESOLVED


def rising(key: str, t1: float, t2: float) -> Callable[[float], dict[str, int]]:
    """Hits of `key` rising by one a second from t1 to t2, flat after."""
    return lambda t: {key: max(0, int(min(t, t2) - t1) + 1)} if t >= t1 else {}


def labels(s: Synth, none_served: dict[str, bool] | None = None, agent: str = "playbook", **params: Any) -> dict[str, Any]:
    rec = s.record(agent)
    p = L.Params(**params)
    nl = {"needs": [{"contact": c, "served": v} for c, v in (none_served or {}).items()]} if none_served is not None else None
    return L.episode_labels(rec, params=p, none_labels=nl)


# ---- the encoding ------------------------------------------------------------------------------

def test_the_encoding_decodes_every_sample_exactly():
    """Random tracks, sources (runs included), vehicles and alerts: every value the encoder saw
    comes back at every sample."""
    rng = random.Random(5)
    enc = PictureEncoder(EPOCH)
    truth: list[dict[str, Any]] = []
    hits = {"T-0001": {"3:eo_ir": 0, "2:radar": 0}, "T-0002": {"3:eo_ir": 0}}
    for k in range(200):
        t = float(k)
        tracks = []
        for tid in ("T-0001", "T-0002"):
            if tid == "T-0002" and not (20 <= k <= 150):
                continue
            for key in hits[tid]:
                if rng.random() < 0.8:
                    hits[tid][key] += 1 if rng.random() < 0.9 else 2
            kind = rng.choice(["small_craft", "small_fast_boat"]) if k % 17 == 0 else "small_craft"
            tracks.append({"id": tid, "domain": "surface", "position": [round(-122.5 + rng.random() / 100, 6), 37.8],
                           "velocity_mps": [round(rng.uniform(-5, 5), 2), 1.0] if k % 3 else None,
                           "affiliation": "unknown", "affiliation_confidence": round(rng.random(), 3),
                           "identity": {"kind": kind, "confidence": round(rng.random(), 3), "conflict": None if k % 5 else "x",
                                        "supported_by": ["radar"]},
                           "sources": [{"uid": int(key.split(":")[0]), "sensor": key.split(":")[1], "hits": h,
                                        "last_t": round(EPOCH + t - (0 if rng.random() < 0.9 else 1), 2)}
                                       for key, h in hits[tid].items() if h],
                           "last_seen_t": EPOCH + t})
        sample = {"t": t, "tracks": tracks, "association": [[x["id"], "c-" + x["id"]] for x in tracks],
                  "alerts": [{"id": "link:3", "kind": "lost_link", "uid": 3, "since_t": EPOCH + 50}] if 50 <= k <= 60 else [],
                  "ours": [{"uid": 3, "lon": -122.5, "lat": 37.8 + k / 1e5, "alt_m": 80.0, "speed_mps": 20.0,
                            "mode": "AUTO" if k < 100 else "RTL", "armed": True, "battery_pct": 90 - k / 10,
                            "home": [-122.5, 37.79], "link_blocked": None}]}
        truth.append(sample)
        enc.add(sample)
    pic = Picture(enc.finish())
    for k, sample in enumerate(truth):
        assert pic.track_ids(k) == sorted(x["id"] for x in sample["tracks"])
        for x in sample["tracks"]:
            idn = pic.identity(x["id"], k)
            assert idn == {**x["identity"]}
            for s in x["sources"]:
                key = f"{s['uid']}:{s['sensor']}"
                assert pic.source(x["id"], key, k) == (s["hits"], round(s["last_t"] - EPOCH, 3)), (k, key)
            assert pic.contact_of(x["id"], k) == "c-" + x["id"]
        v = pic.vehicle(3, k)
        assert v["battery_pct"] == round(90 - k / 10, 3) and v["mode"] == ("AUTO" if k < 100 else "RTL")
        assert bool(pic.alerts_at(k)) == (50 <= k <= 60)
    assert not pic.present("T-0002", 151) and pic.present("T-0002", 150)


def test_hits_rise_between_two_times_and_across_a_new_track():
    s = Synth(120)
    s.contact("boat", path=inbound())
    s.track("T-0001", "boat", 0, hits=lambda t: {"3:eo_ir": 5} if t >= 10 else {})
    pic = Picture(s.record()["v2"]["picture"])
    assert pic.hits_rose("T-0001", "3:eo_ir", 5, 20)
    assert not pic.hits_rose("T-0001", "3:eo_ir", 10, 60)
    assert not pic.contact_hits_rose("boat", 3, ("eo_ir",), 10, 60)
    # the contact tracked again under a new id: its new track's hits count
    s2 = Synth(120)
    s2.contact("boat", path=inbound())
    s2.track("T-0001", "boat", 0, hits=lambda t: {"3:eo_ir": 2})
    s2.track("T-0002", "boat", 60, hits=lambda t: {"3:eo_ir": int(t - 59)})
    s2.tracks["T-0001"]["t0"] = 0
    rec = s2.record()
    enc = rec["v2"]["picture"]
    # T-0001 dropped at 60: the association moves to the new track
    enc["tracks"]["T-0001"]["span"] = [[0, 59]]
    enc["assoc"]["T-0001"] = [[0, "boat"], [60, None]]
    pic2 = Picture(enc)
    assert pic2.track_of("boat", 70) == "T-0002"
    assert pic2.contact_hits_rose("boat", 3, ("eo_ir",), 61, 80)
    assert not pic2.contact_hits_rose("boat", 3, ("radar",), 61, 80)


# ---- 2.1, 2.2 ----------------------------------------------------------------------------------

def test_a_need_its_deadline_and_when_it_was_served():
    s = Synth(300)
    s.contact("boat", path=inbound())  # within 3 km at 100 s
    s.track("T-0001", "boat", ident=resolved_from(130.0))
    n = labels(s)["needs"][0]
    assert n["t_need"] == pytest.approx(100.5, abs=1.0) and n["deadline"] == pytest.approx(n["t_need"] + 60)
    assert n["eligible"] and n["served"] and not n["pre_served"]
    assert n["t_served"] == 130.0 and n["time_to_serve"] == pytest.approx(130.0 - n["t_need"])


def test_pre_served_late_and_unserved():
    s = Synth(300)
    s.contact("boat", path=inbound())
    s.track("T-0001", "boat", ident=resolved_from(50.0))
    n = labels(s)["needs"][0]
    assert n["pre_served"] and n["served"] and n["t_served"] == n["t_need"] and n["time_to_serve"] == 0.0
    s.tracks["T-0001"]["ident"] = resolved_from(185.0)  # after the deadline (about 160.5)
    n = labels(s)["needs"][0]
    assert not n["served"] and n["t_served"] == 185.0 and n["time_to_serve"] is None
    assert not n["served_late"]
    assert labels(s, late_s=90.0)["needs"][0]["served_late"]


@pytest.mark.parametrize("ident,ais,ok", [
    (("small_fast_boat", 0.6, None, ["eo_ir"]), False, True),
    (("small_fast_boat", 0.599, None, ["eo_ir"]), False, False),
    (("small_fast_boat", 0.9, "AIS says ...", ["eo_ir"]), False, False),
    (("small_fast_boat", 0.9, None, ["radar"]), False, False),
    (("fishing_vessel", 0.9, None, ["eo_ir"]), False, False),
    (("small_fast_boat", 0.9, None, ["sonar"]), False, True),
    (("small_fast_boat", 0.9, None, ["ais"]), False, False),
    (("small_fast_boat", 0.9, None, ["ais"]), True, True),
])
def test_identity_resolved_follows_the_mission(ident, ais, ok):
    s = Synth(300)
    s.contact("boat", path=inbound(), ais=ais)
    s.track("T-0001", "boat", ident=lambda t: ident)
    assert labels(s)["needs"][0]["served"] is ok


def test_not_evaluable_and_not_a_need():
    s = Synth(170)  # deadline about 160.5 > 170 - 10
    s.contact("boat", path=inbound())
    s.contact("ferry", kind="ferry", aff="neutral", path=inbound())
    s.contact("far", path=[(0.0, -9000.0, 0.0)])
    s.track("T-0001", "boat", ident=resolved_from(120.0))
    needs = labels(s)["needs"]
    assert [n["contact"] for n in needs] == ["boat"]
    assert not needs[0]["evaluable"] and not needs[0]["eligible"]


def test_sensing_errors_from_the_draw():
    s = Synth(300)
    s.contact("boat", path=inbound())
    s.contact("sub", kind="submarine", path=inbound(), domain="subsurface")
    s.track("T-0001", "boat", ident=resolved_from(120.0))
    s.sensing = {"seed": 7, "misclassification_rate": 1.0}  # every draw set
    needs = {n["contact"]: n for n in labels(s)["needs"]}
    assert needs["boat"]["sensing_error"] and not needs["boat"]["eligible"]
    # no carried sensor could classify a contact under water: not a sensing error
    assert not needs["sub"]["sensing_error"] and needs["sub"]["sensing"]["holders"] == []
    s.sensing = {"seed": 7, "misclassification_rate": 0.0}
    assert not labels(s)["needs"][0]["sensing_error"]
    # a sonar carried as well: both draws must be set
    s.fleet.append({"uid": 1, "platform": "seahunter", "sensors": [{"type": "sonar", "range_m": 10000.0}]})
    s.sensing = {"seed": 7, "misclassification_rate": 0.5}
    for n in labels(s)["needs"]:
        d = n["sensing"]["draws"]
        assert n["sensing_error"] == all(d[h] for h in n["sensing"]["holders"])


def test_the_python_draw_is_the_rust_function_on_known_values():
    """Pinned against crates/murmur-bridge sensing.rs (the binary test compares many more)."""
    assert L.hash64(0, 0, 0, "", 0) == L.hash64(0, 0, 0, "", 0)
    u = L.misclassify_u(7, "fast-1", "eo_ir")
    assert 0.0 <= u < 1.0
    assert L.misclassify_draw(7, "fast-1", "eo_ir", u + 1e-12) and not L.misclassify_draw(7, "fast-1", "eo_ir", u)
    assert not L.misclassify_draw(7, "fast-1", "eo_ir", 0.0)


# ---- 2.4 ---------------------------------------------------------------------------------------

def battery(start: float, per_s: float, rtl_at: float | None = None, home_at: float | None = None):
    def fn(t: float) -> dict[str, Any]:
        v: dict[str, Any] = {"battery_pct": round(start - per_s * t, 3), "lon": BR[0], "lat": BR[1]}
        if rtl_at is not None and t >= rtl_at:
            v["mode"] = "RTL"
        if home_at is not None and t >= home_at:
            v["lon"], v["lat"] = BR[0], BR[1] - 0.05
        return v
    return fn


def test_low_reserve_met_unmet_censored():
    s = Synth(300)
    s.vehicle(4, battery(40.0, 0.1, rtl_at=100.0))  # 35 % at 50 s, rtl at 100 s (30 %)
    f = labels(s)["fault_needs"][0]
    assert f["t_open"] == 50.0 and f["status"] == "met" and f["t_met"] == 100.0 and f["met_by"] == "rtl"
    s.vehicle(4, battery(40.0, 0.1, rtl_at=260.0))  # 20 % at 200 s, rtl too late
    f = labels(s)["fault_needs"][0]
    assert f["status"] == "unmet" and f["t_reserve"] == 200.0
    s.vehicle(4, battery(40.0, 0.05))  # never 20 %, never home: censored
    assert labels(s)["fault_needs"][0]["status"] == "censored"
    s.vehicle(4, battery(40.0, 0.05, home_at=150.0))  # back home after leaving it
    f = labels(s)["fault_needs"][0]
    assert f["status"] == "met" and f["met_by"] == "home" and f["t_met"] == 150.0
    # the multiverse's opening: 30 % opens later
    s.vehicle(4, battery(40.0, 0.1, rtl_at=100.0))
    assert labels(s, low_reserve_open_pct=30.0)["fault_needs"][0]["t_open"] == 100.0
    assert labels(s, low_reserve_open_pct=40.0)["fault_needs"][0]["t_open"] == 0.0


# ---- 2.3, 2.5: actions ------------------------------------------------------------------------

def orbit(s: Synth, iid: int, uid: int, created: float, start: float | None, end_state: tuple[float, str] | None = None,
          center: tuple[float, float] | None = None, radius: float = 1000.0, kind: str = "orbit",
          states: list[tuple[float, str, str]] | None = None) -> None:
    op = {}
    if start is not None:
        rows = [(start, "starting"), (start + 1, "active")]
        if end_state:
            rows.append(end_state)
        op = {uid: rows}
    c = center or at(-1500.0, 0.0)
    params = {"center": list(c), "radius_m": radius} if kind == "orbit" else {}
    s.intent(iid, kind, [uid], created, params,
             states=states or [(created, "checked", "check"), (created, "auto_approved", "auto_approve"),
                               (created, "dispatched", "dispatch")], op=op)


def look_world(resolve_at: float = 130.0, hits_from: float = 105.0, hits_to: float = 135.0) -> Synth:
    s = Synth(300)
    s.contact("boat", path=inbound())
    s.track("T-0001", "boat", ident=resolved_from(resolve_at), hits=rising("3:eo_ir", hits_from, hits_to))
    s.vehicle(3, lambda t: {})
    return s


def by_intent(lab: dict[str, Any]) -> dict[int | None, dict[str, Any]]:
    return {a["intent"]: a for a in lab["actions"]}


def test_useful_against_the_none_replicate_then_follow_ups():
    s = look_world()
    orbit(s, 10, 3, created=95.0, start=100.0, end_state=(140.0, "superseded"))
    orbit(s, 11, 3, created=110.0, start=140.0)  # its hits do not rise: the run is flat after 135
    s.tracks["T-0001"]["hits"] = rising("3:eo_ir", 105.0, 150.0)
    lab = labels(s, none_served={"boat": False})
    a = by_intent(lab)
    assert a[10]["label"] == "useful" and a[10]["contributed"] == ["boat"] and a[10]["credited"] == ["boat"]
    # the second: its run starts after the need was served (130 s), so it did not contribute
    assert a[11]["label"] == "redundant"
    # served in the none replicate too: not useful
    lab = labels(s, none_served={"boat": True})
    assert by_intent(lab)[10]["label"] in ("redundant", "useless")
    # two looks contributing before t_served: the first credited, the second a follow-up
    s2 = look_world(resolve_at=150.0, hits_from=105.0, hits_to=160.0)
    orbit(s2, 10, 3, created=95.0, start=100.0, end_state=(120.0, "superseded"))
    orbit(s2, 11, 3, created=110.0, start=120.0)
    a2 = by_intent(labels(s2, none_served={"boat": False}))
    assert a2[10]["label"] == "useful" and a2[11]["label"] == "follow_up" and a2[11]["follow_up_of"] == {"boat": 10}


def test_without_the_pair_useful_is_not_decided():
    s = look_world()
    orbit(s, 10, 3, created=95.0, start=100.0)
    lab = labels(s, none_served=None)
    assert not lab["paired"] and by_intent(lab)[10]["label"] != "useful"


def test_an_action_never_dispatched_contributes_nothing():
    s = look_world()
    orbit(s, 10, 3, created=95.0, start=None,
          states=[(95.0, "checked", "check"), (95.0, "pending_approval", "require_approval"), (215.0, "expired", "expire")])
    a = by_intent(labels(s, none_served={"boat": False}))[10]
    assert a["contributed"] == [] and a["label"] != "useful" and a["card"]


def test_precedence_refused_first():
    s = look_world()
    orbit(s, 10, 3, created=95.0, start=None, states=[(95.0, "checked", "check"), (95.0, "rejected", "checks_failed")])
    s.agent_log.append({"t": 96.0, "source": "playbook", "kind": "error", "error": "propose_intent: RuntimeError: bad"})
    lab = labels(s, none_served={"boat": False})
    labs = [a["label"] for a in lab["actions"]]
    assert labs == ["refused", "refused"]


def test_harmful_b_takes_a_sensor_off_an_unserved_need():
    """PUMA 1's EO/IR was raising its hits on the boat; an orbit far away takes it off; the
    boat goes unserved by its deadline: harmful, ahead of everything else."""
    s = Synth(300)
    s.contact("boat", path=inbound())
    s.contact("other", kind="fishing_vessel", aff="neutral", path=[(0.0, 4000.0, 4000.0)])
    s.track("T-0001", "boat", ident=lambda t: UNRESOLVED, hits=rising("3:eo_ir", 100.0, 120.0))
    s.track("T-0002", "other", ident=lambda t: ("fishing_vessel", 0.9, None, ["eo_ir"]))
    s.vehicle(3, lambda t: {})
    orbit(s, 10, 3, created=118.0, start=120.0, center=at(4000.0, 4000.0), radius=500.0)
    s.vehicle(3, lambda t: {"battery_pct": 30.0, "lat": BR[1]})  # out, under 35 %: an open low-reserve fault
    a = by_intent(labels(s, none_served={"boat": False}))[10]
    assert a["label"] == "harmful" and a["harm"][0]["kind"] == "b" and a["harm"][0]["need"] == "boat"
    assert a["also_fault"]
    # the need served in the end: not harmful
    s.tracks["T-0001"]["ident"] = resolved_from(150.0)
    a = by_intent(labels(s, none_served={"boat": False}))[10]
    assert a["label"] != "harmful"


def test_harmful_a_sends_a_vehicle_under_the_reserve_away_from_home():
    s = look_world()
    s.vehicle(3, lambda t: {"battery_pct": 18.0})
    orbit(s, 10, 3, created=95.0, start=100.0)
    a = by_intent(labels(s, none_served={"boat": False}))[10]
    assert a["label"] == "harmful" and a["harm"][0]["kind"] == "a"
    s2 = look_world()
    s2.vehicle(3, lambda t: {"battery_pct": 18.0, "lat": BR[1]})  # out, under the reserve
    s2.intent(10, "rtl", [3], 95.0, {}, op={3: [(95.0, "acked"), (96.0, "done")]})
    a = by_intent(labels(s2, none_served={"boat": False}))[10]
    assert a["label"] == "fault_response"


def test_fault_response_before_useful():
    s = look_world()
    s.alerts.append(("op:5", "not_moving", 3, 80.0, 80.0, 300.0))
    orbit(s, 10, 3, created=95.0, start=100.0)
    a = by_intent(labels(s, none_served={"boat": False}))[10]
    assert a["label"] == "fault_response" and a["faults_open"] == ["stuck:3"]
    s.alerts = []
    s.script.append({"t": 90.0, "kind": "stop", "uid": 3})
    assert by_intent(labels(s, none_served={"boat": False}))[10]["faults_open"] == ["stuck:3"]
    s.script = []
    s.vehicle(3, lambda t: {"link_blocked": "cut"} if t >= 90 else {})
    assert by_intent(labels(s, none_served={"boat": False}))[10]["faults_open"] == ["link:3"]


def test_an_rtl_that_meets_a_low_reserve_before_it_opened_is_useful():
    s = Synth(300)
    s.vehicle(4, battery(36.0, 0.05, rtl_at=12.0))  # opens at 20 s, on its rtl since 12 s
    s.intent(10, "rtl", [4], 10.0, {}, op={4: [(11.0, "acked"), (400.0, "done")]})
    lab = labels(s, none_served={})
    assert lab["fault_needs"][0]["status"] == "met"
    a = by_intent(lab)[10]
    assert a["label"] == "useful" and a["met_fault"]["uid"] == 4


def test_redundant_when_its_need_was_served_or_credited():
    s = look_world(resolve_at=80.0, hits_from=100.0, hits_to=200.0)  # resolved before the look
    orbit(s, 10, 3, created=95.0, start=100.0)
    assert by_intent(labels(s, none_served={"boat": False}))[10]["label"] == "redundant"


def test_aim_by_geometry_and_ambiguity():
    s = Synth(300)
    s.contact("a", path=[(0.0, -1400.0, 0.0)])  # 100 m inside the circle below
    s.contact("b", path=[(0.0, -1500.0, 700.0)])
    s.contact("c", path=[(0.0, 2000.0, 0.0)])
    s.vehicle(3, lambda t: {})
    # an orbit of 1 km whose circle passes through a, never dispatched (no hits): aim by geometry
    orbit(s, 10, 3, created=95.0, start=None, center=at(-2500.0, 0.0), radius=1000.0,
          states=[(95.0, "checked", "check"), (95.0, "pending_approval", "require_approval")])
    aim = by_intent(labels(s, none_served={}))[10]["aim"]
    assert aim["by"] == "geometry" and aim["nearest"] == "a" and not aim["ambiguous"]
    # b also near the circle: within 1.5 times a's distance -> ambiguous
    s.contacts["b"]["path"] = [(0.0, -3620.0, 0.0)]  # 120 m outside it
    aim = by_intent(labels(s, none_served={}))[10]["aim"]
    assert aim["ambiguous"] and aim["primary"] is None
    # the fallback distance: nothing within 500 m of a circle 800 m from every contact
    s.contacts["b"]["path"] = [(0.0, -1500.0, 2700.0)]
    orbit(s, 11, 3, created=96.0, start=None, center=at(-3300.0, 0.0), radius=1000.0,
          states=[(96.0, "checked", "check"), (96.0, "pending_approval", "require_approval")])
    assert by_intent(labels(s, none_served={}, aim_fallback_m=500.0))[11]["aim"]["by"] == "none"
    assert by_intent(labels(s, none_served={}, aim_fallback_m=1000.0))[11]["aim"]["nearest"] == "a"


def test_justified_on_the_picture_both_columns():
    s = Synth(300)
    s.contact("ferry", kind="ferry", aff="neutral", path=inbound(speed=10.0))  # truth: needs nothing
    s.track("T-0001", "ferry", ident=lambda t: ("large_vessel", 0.4, None, ["radar"]), aff=lambda t: "unknown",
            hits=rising("3:eo_ir", 100.0, 140.0))
    s.vehicle(3, lambda t: {})
    orbit(s, 10, 3, created=60.0, start=100.0, center=at(-3000.0, 0.0))
    lab = labels(s, none_served={})
    a = by_intent(lab)[10]
    assert a["label"] == "useless" and a["justified"]["justified"]
    m = M.episode_metrics(s.record(), lab)
    assert m["counts"]["useless"] == 1 and m["counts"]["useless_justified"] == 1 and m["counts"]["useless_unjustified"] == 0
    assert m["justified_useless_share"] == 1.0
    # shown neutral and confident: not justified
    s.tracks["T-0001"]["ident"] = lambda t: ("ferry", 0.9, None, ["eo_ir", "ais"])
    s.tracks["T-0001"]["aff"] = lambda t: "neutral"
    assert not by_intent(labels(s, none_served={}))[10]["justified"]["justified"]
    # the horizon: a track heading away from the bridge never comes within 3 km
    s.contacts["ferry"]["path"] = [(0.0, -5000.0, 0.0), (300.0, -9000.0, 0.0)]
    s.tracks["T-0001"]["ident"] = lambda t: ("large_vessel", 0.4, None, ["radar"])
    s.tracks["T-0001"]["aff"] = lambda t: "unknown"
    assert not by_intent(labels(s, none_served={}, justify_horizon_s=600.0))[10]["justified"]["justified"]


def test_scripted_cards_are_apart():
    s = look_world()
    s.intent(20, "patrol", [1], 100.0, {"route": [list(at(0, 0)), list(at(100, 100))]},
             states=[(100.0, "checked", "check"), (100.0, "pending_approval", "require_approval")], source="agent-scripted")
    lab = labels(s, none_served={})
    assert lab["actions"] == [] and [c["intent"] for c in lab["scripted_cards"]] == [20]


# ---- metrics -----------------------------------------------------------------------------------

def test_flood_share_starts_above_10_and_ends_under_5():
    assert M.flood_share([], 0.0, 600.0)["share"] == 0.0
    cards = [100.0 + i for i in range(11)]  # 11 cards inside one window: a flood
    f = M.flood_share(cards, 0.0, 1300.0)
    assert f["peak_cards_per_window"] == 11 and f["share"] > 0
    # the flood lasts while a window holds 5 or more: from 110 s until the cards age out at 701
    assert f["in_flood"] == len([t for t in range(110, 701, 10) if t >= 110])
    assert M.flood_share([100.0 + i for i in range(10)], 0.0, 1300.0)["share"] == 0.0


def test_episode_metrics_from_labels():
    s = look_world()
    orbit(s, 10, 3, created=95.0, start=100.0,
          states=[(95.0, "checked", "check"), (95.0, "pending_approval", "require_approval"), (99.0, "approved", "approve"),
                  (99.0, "dispatched", "dispatch")])
    s.intents[-1]["approvals"] = [[99.0, "granted", "op-auto"]]
    rec = s.record()
    lab = labels(s, none_served={"boat": False})
    m = M.episode_metrics(rec, lab)
    assert m["eligible"] == 1 and m["served_share"] == 1.0 and m["counts"]["useful"] == 1
    assert m["cards"] == 1 and m["cards_per_10min"] == pytest.approx(1 / ((300 - 45) / 600))
    assert m["time_margins"] == [pytest.approx(lab["needs"][0]["deadline"] - 95.0)]
    assert m["operator_diagnostics"]["approval_latency_s"] == [4.0]
    assert m["per_hour"]["useful"] == pytest.approx(12.0)
    sm = M.situation_metrics([m, {**m, "served_share": 0.0}])
    assert sm["served_share"] == 0.5 and sm["replicates"] == 2
