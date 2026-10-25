"""Scoring v3's labels for F7-F13 (eval/SCORING-V3.md 2.2 and 2.4, choices 14 to 17) on small
synthetic records (tests/test_v2_labels.py `Synth`, moved under `v3` with a world): a contact with a
spoofed AIS identity, the low reserve in every domain, the timeline's faults open on a vehicle, the
vehicles lost by the end (a diagnostic) and the spoofer's cause of loss. No simulator."""

from __future__ import annotations

from typing import Any

import pytest

from eval.headless.tests.test_v2_labels import BR, Synth, at, battery, inbound, orbit, rising, rtl
from eval.headless.tests.test_v3_labels import by_intent, labels, need, v3_record
from eval.headless.v2 import labels as V2L
from eval.headless.v3 import faults as FA
from eval.headless.v3 import labels as L
from eval.headless.v3.causes import cause_of
from eval.headless.labels import Truth

SPOOFED = {"mmsi": "366900001", "name": "F/V PACIFIC DAWN", "ship_type": "fishing", "spoofed": True}
TRUE_AIS = {"mmsi": "366900002", "name": "F/V ANNA", "ship_type": "fishing"}
AIS_CONFLICT = "AIS says fishing (F/V PACIFIC DAWN) but eo_ir identifies it as small_fast_boat"
BELIEVED = ("fishing_vessel", 0.8, None, ["ais"])


def spoofer(after: tuple[Any, ...], t1: float, ais: dict[str, Any] = SPOOFED, dur: float = 600.0) -> Synth:
    """A small fast boat broadcasting `ais`, inbound (inside 3 km at 100 s): its track shows the
    AIS's fishing vessel, then `after` from `t1`."""
    s = Synth(dur)
    s.contact("c", kind="small_fast_boat", domain="surface", aff="suspect", path=inbound())
    s.contacts["c"]["meta"]["ais"] = dict(ais)
    s.track("T-0001", "c", ident=lambda t: after if t >= t1 else BELIEVED)
    return s


# ---- 2.2, choice 17: a spoofed AIS ---------------------------------------------------------------

def test_a_spoofer_is_resolved_by_eo_ir_exposing_the_spoof():
    lab = labels(spoofer(("small_fast_boat", 0.9, AIS_CONFLICT, ["eo_ir"]), 130.0))
    n = need(lab, "c")
    assert n["served"] and n["t_served"] == pytest.approx(130.0, abs=1.0)
    assert n["ais_spoofed"] and n["ais"] is False, "a spoofed AIS is not the contact's own true AIS"


def test_a_spoofer_believed_is_never_resolved():
    assert not need(labels(spoofer(BELIEVED, 0.0)), "c")["served"]


@pytest.mark.parametrize("after", [
    ("small_fast_boat", 0.9, "sonar identifies it as fishing_vessel while the estimate is small_fast_boat", ["eo_ir"]),
    ("small_fast_boat", 0.55, AIS_CONFLICT, ["eo_ir"]),
    ("small_fast_boat", 0.9, AIS_CONFLICT, ["sonar"]),
])
def test_a_spoofer_needs_eo_ir_at_0_6_and_no_other_conflict(after: tuple[Any, ...]):
    assert not need(labels(spoofer(after, 130.0)), "c")["served"]


def test_an_ais_conflict_still_blocks_a_contact_whose_ais_is_its_own():
    """The exemption is the spoof's alone: a true broadcaster in conflict with a look is not resolved
    (2.2 unchanged)."""
    s = spoofer(("small_fast_boat", 0.9, AIS_CONFLICT, ["eo_ir"]), 130.0, ais=TRUE_AIS)
    n = need(labels(s), "c")
    assert not n["served"] and n["ais"] is True and n["ais_spoofed"] is False


def test_a_spoofed_ais_never_supports_its_contact():
    """AIS counts for a surface need only from the contact's own true broadcast (2.2)."""
    shown = ("small_fast_boat", 0.9, None, ["ais"])
    assert not need(labels(spoofer(shown, 130.0)), "c")["served"]
    assert need(labels(spoofer(shown, 130.0, ais=TRUE_AIS)), "c")["served"]


def test_a_spoofed_ais_s_hits_contribute_nothing():
    s = spoofer(("small_fast_boat", 0.9, AIS_CONFLICT, ["eo_ir"]), 130.0)
    s.tracks["T-0001"]["hits"] = rising("3:ais", 105.0, 135.0)
    s.vehicle(3, lambda t: {})
    orbit(s, 10, 3, created=95.0, start=100.0)
    a = by_intent(labels(s, none_served={"c": False}))[10]
    assert a["contributed"] == [] and a["label"] != "useful"


def test_a_spoofer_lost_while_the_picture_believes_it_is_named_so():
    s = spoofer(BELIEVED, 0.0)
    rec = v3_record(s)
    lab = L.episode_labels(rec)
    n = need(lab, "c")
    assert cause_of(rec, L.picture(rec), Truth(rec), n, []) == "spoof_believed@day"


# ---- 2.4, choice 14: the low reserve in every domain ----------------------------------------------

HOME_WATER = (-122.4400, 37.8160)  # HUNTER 1's mooring off the Marina (22 m of water)


def boat(start: float, per_s: float, home_at: float | None = None, stop: bool = True, alt: float = 0.0):
    """A boat out in the channel draining `per_s` a second, back at its mooring from `home_at`
    (stopped there, still armed, as a Rover's rtl ends, unless `stop` is False)."""
    out_at = (-122.4600, 37.8190)

    def fn(t: float) -> dict[str, Any]:
        v: dict[str, Any] = {"battery_pct": round(start - per_s * t, 3), "lon": out_at[0], "lat": out_at[1], "alt_m": alt,
                             "speed_mps": 4.0, "home": list(HOME_WATER), "armed": True, "mode": "AUTO"}
        if home_at is not None and t >= home_at:
            v.update(lon=HOME_WATER[0], lat=HOME_WATER[1], speed_mps=0.0 if stop else 3.0, mode="HOLD")
        return v
    return fn


def with_boat(s: Synth, platform: str = "seahunter") -> Synth:
    s.fleet.append({"uid": 9, "callsign": "HUNTER 1", "platform": platform, "sensors": [{"type": "radar", "range_m": 1e4}]})
    return s


@pytest.mark.parametrize("home_at, stop, status", [
    (150.0, True, "met"),       # stopped at its mooring before 20 % (at 200 s): met, armed as it is
    (250.0, True, "unmet"),     # home after the reserve
    (150.0, False, "unmet"),    # circling at home, never stopped
    (None, True, "unmet"),
])
def test_a_boat_s_low_reserve_is_met_stopped_at_home(home_at: float | None, stop: bool, status: str):
    s = with_boat(Synth(300))
    s.vehicle(9, boat(40.0, 0.1, home_at, stop))  # 35 % at 50 s, 20 % at 200 s
    f = next(f for f in labels(s)["fault_needs"] if f["uid"] == 9)
    assert f["domain"] == "surface" and f["status"] == status
    if status == "met":
        assert f["met_by"] == "stopped at home" and f["t_met"] >= 150.0 + FA.STOPPED_S - 1.0


def test_a_sub_is_at_home_only_at_the_surface():
    for alt, status in ((-10.0, "unmet"), (0.0, "met")):
        s = with_boat(Synth(300), "remus")
        s.vehicle(9, boat(40.0, 0.1, 150.0, True, alt=alt))
        f = next(f for f in labels(s)["fault_needs"] if f["uid"] == 9)
        assert f["domain"] == "subsurface" and f["status"] == status


@pytest.mark.parametrize("land_at", [None, 150.0, 250.0])
def test_an_aircraft_s_low_reserve_is_v2_s_value_for_value(land_at: float | None):
    s = Synth(300)
    s.vehicle(4, battery(40.0, 0.1, rtl_at=100.0, land_at=land_at))
    rec = v3_record(s)
    pic = L.picture(rec)
    ours = FA.fault_needs(rec, pic, V2L.Params())
    v2 = V2L.fault_needs(rec, pic, V2L.Params())
    keys = ("uid", "t_open", "t_reserve", "t_met", "t_landed", "t_closed", "status")
    assert [{k: f[k] for k in keys} for f in ours] == [{k: f[k] for k in keys} for f in v2]


# ---- choice 15: the timeline's faults open on a vehicle -------------------------------------------

def step_log(t: float, sid: str, kind: str, uids: Any, **kw: Any) -> dict[str, Any]:
    return {"t": t, "kind": "step", "step": sid, "step_kind": kind, "uids": uids, **kw}


def hold(s: Synth, iid: int, uid: int, created: float) -> None:
    s.intent(iid, "hold", [uid], created, {}, op={uid: [(created, "starting"), (created + 1, "acked"),
                                                        (created + 2, "done")]})


def faulted(script: list[dict[str, Any]], stated: list[dict[str, Any]] | None = None, where: tuple[float, float] = BR,
            armed_after: float | None = None) -> dict[str, Any]:
    s = Synth(300)
    s.vehicle(3, lambda t: {"lon": where[0], "lat": where[1],
                            "armed": True if armed_after is None or t < 100.0 or t >= armed_after else False})
    s.script = script
    hold(s, 10, 3, 120.0)
    rec = v3_record(s)
    rec["truth"]["stated"] = {"script": stated or []}
    return rec


def test_a_gnss_jammer_is_open_on_every_vehicle_inside_its_circle():
    circle = {"lat": BR[1], "lon": BR[0], "radius_m": 1000.0}
    stated = [{"id": "gnss-jammer", "params": {"effect": "jam", "circle": circle}}]
    log = [step_log(100.0, "gnss-jammer", "gnss", "every vehicle in its circle", effect="jam")]
    a = by_intent(L.episode_labels(faulted(log, stated)))[10]
    assert a["faults_open"] == ["gnss_jam:3"] and a["label"] == "fault_response"
    a = by_intent(L.episode_labels(faulted(log, stated, where=at(3000.0, 0.0))))[10]
    assert a["faults_open"] == [] and a["label"] == "useless"


def test_a_spoof_stop_battery_and_reboot_are_open_on_their_vehicles():
    for kind, extra, want in (("gnss", {"effect": "spoof"}, "gnss_spoof:3"), ("stop", {}, "stuck:3"),
                              ("battery", {}, "battery:3")):
        rec = faulted([step_log(100.0, "f", kind, [3], **extra)])
        assert by_intent(L.episode_labels(rec))[10]["faults_open"] == [want]
    # a reboot on the surface boots disarmed: open until the vehicle is armed again
    rec = faulted([step_log(100.0, "r", "reboot", [3])], armed_after=200.0)
    assert by_intent(L.episode_labels(rec))[10]["faults_open"] == ["reboot:3"]
    rec = faulted([step_log(100.0, "r", "reboot", [3])], armed_after=110.0)
    assert by_intent(L.episode_labels(rec))[10]["faults_open"] == []


def test_a_spoof_ends_with_its_duration():
    stated = [{"id": "s", "params": {"effect": "spoof", "duration_s": 10.0}}]
    rec = faulted([step_log(100.0, "s", "gnss", [3], effect="spoof")], stated)
    assert by_intent(L.episode_labels(rec))[10]["faults_open"] == []


# ---- a diagnostic: the vehicles lost --------------------------------------------------------------

def test_an_aircraft_down_away_from_home_is_lost_and_one_down_at_home_is_not():
    s = Synth(300)
    s.vehicle(3, lambda t: {"armed": t < 200.0, "lon": BR[0], "lat": BR[1]})            # down at the bridge
    s.vehicle(4, lambda t: {"armed": t < 200.0, "lon": BR[0], "lat": BR[1] - 0.05})     # down at home
    rec = v3_record(s)
    lost = FA.lost_vehicles(rec, L.picture(rec))
    assert [(x["uid"], x["why"]) for x in lost] == [(3, "down away from home")]


def test_a_boat_out_of_the_water_is_aground():
    s = with_boat(Synth(300))
    s.vehicle(9, lambda t: {"lon": -122.4500 if t > 100 else HOME_WATER[0], "lat": 37.8040 if t > 100 else HOME_WATER[1],
                            "home": list(HOME_WATER)})  # Crissy Field's lawn
    rec = v3_record(s)
    assert [(x["uid"], x["why"]) for x in FA.lost_vehicles(rec, L.picture(rec))] == [(9, "aground")]


def test_an_rtl_that_brings_a_boat_home_before_the_reserve_is_useful():
    s = with_boat(Synth(300))
    s.vehicle(9, boat(40.0, 0.1, 150.0, True))
    rtl(s, 10, 9, 60.0)
    a = by_intent(labels(s))[10]
    assert a["label"] == "useful" and int(a["met_fault"]["uid"]) == 9
