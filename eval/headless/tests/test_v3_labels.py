"""Scoring v3's labels on small synthetic records (eval/SCORING-V3.md section 2): each domain's sensor,
level and grace, the world's own mission circle, contributions and harm by the need's own sensors,
a `track` order's aim, collect and cover left unjudged, nominations and review decisions judged
against truth, the physical model's wrong-call draw and sensing errors, and the metrics per domain.
No simulator: the records are v2's synthetic ones (tests/test_v2_labels.py `Synth`) moved under
`v3` with a world."""

from __future__ import annotations

import random
from typing import Any

import pytest

from eval.headless.tests.test_v2_labels import BR, Synth, at, inbound, orbit, rising
from eval.headless.v2.labels import Params
from eval.headless.v3 import labels as L
from eval.headless.v3 import metrics as M
from eval.headless.v3.mission import DOMAINS, mission_text

UNRESOLVED = ("small_craft", 0.4, None, ["radar"])


def v3_record(s: Synth, agent: str = "playbook", world: str = "golden-gate-v3", effects: list[dict[str, Any]] | None = None,
              review: list[dict[str, Any]] | None = None) -> dict[str, Any]:
    rec = s.record(agent)
    rec["v3"] = rec.pop("v2")
    rec["scoring"] = "v3"
    rec["world"] = {"name": world, "conditions": {"local_time": "12:00", "visibility_km": 20.0}}
    if effects is not None:
        rec["effects"] = {"applied": effects}
    if review is not None:
        rec["review"] = {"decisions": review}
    return rec


def labels(s: Synth, none_served: dict[str, bool] | None = None, **kw: Any) -> dict[str, Any]:
    nl = {"needs": [{"contact": c, "served": v} for c, v in (none_served or {}).items()]} if none_served is not None else None
    return L.episode_labels(v3_record(s, **kw), none_labels=nl)


def need(lab: dict[str, Any], cid: str) -> dict[str, Any]:
    return next(n for n in lab["needs"] if n["contact"] == cid)


def by_intent(lab: dict[str, Any]) -> dict[int | None, dict[str, Any]]:
    return {a["intent"]: a for a in lab["actions"]}


def one(kind: str, domain: str, ident: tuple[Any, ...], t1: float, ais: bool = False, dur: float = 600.0) -> Synth:
    """A contact of `kind` in `domain` inbound from the west (inside 3 km at 100 s), its track showing
    `ident` from `t1`."""
    s = Synth(dur)
    s.contact("c", kind=kind, domain=domain, ais=ais, path=inbound())
    s.track("T-0001", "c", ident=lambda t: ident if t >= t1 else UNRESOLVED)
    return s


# ---- the mission ---------------------------------------------------------------------------------

def test_the_mission_text_names_every_domain_s_grace_and_sensor():
    text = mission_text("san-diego")
    assert "bay entrance off Ballast Point" in text and "3 km" in text
    for d in DOMAINS.values():
        assert f"{d.grace_s:.0f} seconds" in text
    assert "EO/IR" in text and "sonar" in text and "AIS" in text
    assert mission_text("golden-gate-v3") != text


@pytest.mark.parametrize("supported_by, ais, served", [
    (["eo_ir"], False, True),
    (["radar", "eo_ir", "sonar"], False, True),
    (["sonar"], False, False),        # a buoy's passive sonar alone: not the surface's sensor
    (["radar"], False, False),
    (["ais"], True, True),            # its own true AIS
    (["ais"], False, False),
])
def test_a_surface_need_is_resolved_by_eo_ir_or_its_own_true_ais(supported_by: list[str], ais: bool, served: bool):
    s = one("small_fast_boat", "surface", ("small_fast_boat", 0.9, None, supported_by), 120.0, ais=ais)
    n = need(labels(s), "c")
    assert n["domain"] == "surface" and n["grace_s"] == 60.0
    assert n["served"] is served


@pytest.mark.parametrize("domain, kind, shown, sensors, at_s, served", [
    ("air", "small_drone", "small_drone", ["eo_ir"], 125.0, True),
    ("air", "small_drone", "small_drone", ["eo_ir"], 135.0, False),      # 30 s grace
    ("air", "small_drone", "small_uas", ["eo_ir"], 110.0, False),        # its family is not enough in the air
    ("ground", "vehicle", "vehicle", ["eo_ir"], 150.0, True),
    ("ground", "vehicle", "vehicle", ["eo_ir"], 165.0, False),           # 60 s grace
    ("subsurface", "uuv", "submerged", ["sonar"], 380.0, True),          # recognition is enough, 300 s grace
    ("subsurface", "uuv", "uuv", ["sonar"], 200.0, True),
    ("subsurface", "uuv", "submerged", ["sonar"], 410.0, False),
    ("subsurface", "uuv", "submerged", ["eo_ir"], 200.0, False),         # sonar's call only
    ("subsurface", "diver", "person", ["sonar"], 200.0, True),           # a diver's family is a person
])
def test_each_domain_has_its_sensor_level_and_grace(domain: str, kind: str, shown: str, sensors: list[str], at_s: float,
                                                    served: bool):
    s = one(kind, domain, (shown, 0.7, None, sensors), at_s)
    n = need(labels(s), "c")
    assert n["domain"] == domain and n["grace_s"] == DOMAINS[domain].grace_s
    assert n["served"] is served, n


def test_a_conflict_or_a_low_confidence_never_resolves():
    s = one("small_fast_boat", "surface", ("small_fast_boat", 0.9, "AIS says fishing", ["eo_ir"]), 110.0)
    assert not need(labels(s), "c")["served"]
    s = one("small_fast_boat", "surface", ("small_fast_boat", 0.55, None, ["eo_ir"]), 110.0)
    assert not need(labels(s), "c")["served"]


def test_the_mission_circle_is_the_record_s_world_s():
    """The Synth's contact comes within 3 km of the Golden Gate Bridge: a need there, none in San
    Diego's world (its circle is around the bay entrance off Ballast Point)."""
    s = one("small_fast_boat", "surface", ("small_fast_boat", 0.9, None, ["eo_ir"]), 120.0)
    assert len(labels(s)["needs"]) == 1
    assert labels(s, world="san-diego")["needs"] == []


# ---- 2.3, 2.5: contributions, harm, aim --------------------------------------------------------------

def boat_with_look(sensor: str) -> Synth:
    s = one("small_fast_boat", "surface", ("small_fast_boat", 0.9, None, ["eo_ir"]), 130.0)
    s.tracks["T-0001"]["hits"] = rising(f"3:{sensor}", 105.0, 135.0)
    s.vehicle(3, lambda t: {})
    orbit(s, 10, 3, created=95.0, start=100.0)
    return s


def test_a_contribution_counts_only_the_need_s_own_sensors():
    a = by_intent(labels(boat_with_look("eo_ir"), none_served={"c": False}))[10]
    assert a["contributed"] == ["c"] and a["label"] == "useful"
    a = by_intent(labels(boat_with_look("sonar"), none_served={"c": False}))[10]
    assert a["contributed"] == [] and a["label"] != "useful"


def test_useful_follow_up_and_duplicate_carry_over():
    lab = labels(boat_with_look("eo_ir"), none_served={"c": True})
    a = by_intent(lab)[10]
    assert a["label"] == "duplicate" and a["duplicate_of_none"] == ["c"]


def test_harm_b_counts_the_need_s_own_sensors():
    """A vehicle whose EO/IR was raising its hits on an unserved surface need, sent away: harmful;
    whose sonar was (not the surface's sensor): not."""
    for sensor, harmful in (("eo_ir", True), ("sonar", False)):
        s = Synth(300)
        s.contact("c", path=inbound())
        s.track("T-0001", "c", ident=lambda t: UNRESOLVED, hits=rising(f"3:{sensor}", 100.0, 120.0))
        s.vehicle(3, lambda t: {})
        orbit(s, 10, 3, created=118.0, start=120.0, center=at(4000.0, 4000.0), radius=500.0)
        a = by_intent(labels(s, none_served={"c": False}))[10]
        assert (a["label"] == "harmful") is harmful, (sensor, a["label"], a["harm"])


def test_a_track_order_is_pointed_at_the_contact_its_track_names():
    s = Synth(300)
    s.contact("c", path=inbound())
    s.track("T-0001", "c", ident=lambda t: UNRESOLVED)
    s.vehicle(20, lambda t: {})
    s.intent(10, "track", [20], 110.0, {"track_id": "T-0001"},
             states=[(110.0, "checked", "check"), (110.0, "dispatched", "dispatch")],
             op={20: [(111.0, "acked"), (112.0, "active")]})
    a = by_intent(labels(s, none_served={"c": False}))[10]
    assert a["aim"]["by"] == "track" and a["aim"]["primary"] == "c" and a["aimed_need"] == "c"


def test_collect_and_cover_are_left_unjudged():
    s = Synth(300)
    s.contact("c", path=inbound())
    s.track("T-0001", "c", ident=lambda t: UNRESOLVED)
    for iid, kind in ((10, "collect"), (11, "cover")):
        s.intent(iid, kind, [], 100.0, {"area": "x"}, states=[(100.0, "checked", "check")])
    lab = labels(s, none_served={"c": False})
    assert {a["label"] for a in lab["actions"]} == {"unjudged"}
    m = M.episode_metrics(v3_record(s), lab)
    assert m["unjudged_actions"] == 2 and m["counts"]["useless"] == 0


# ---- 2.8: nominations ----------------------------------------------------------------------------------

def nomination_world(aff: str, effects: list[dict[str, Any]] | None = None) -> tuple[Synth, list[dict[str, Any]] | None]:
    s = Synth(400)
    s.contact("c", aff=aff, path=inbound())
    s.contact("n", kind="fishing_vessel", aff="neutral", path=[(0.0, -1000.0, 50.0)])
    s.track("T-0001", "c", ident=lambda t: UNRESOLVED)
    s.track("T-0002", "n", ident=lambda t: ("fishing_vessel", 0.9, None, ["eo_ir"]))
    s.intent(10, "engage", [7], 150.0, {"track_id": "T-0001", "effect_radius_m": 75.0},
             states=[(150.0, "checked", "check"), (150.0, "pending_approval", "require_approval")])
    return s, effects


@pytest.mark.parametrize("aff, effects, want", [
    ("hostile", [{"intent": 10, "hit": True, "applied": {"inside": [["c", 5.0]], "out_of_play": ["c"]}}], "effective"),
    ("hostile", [{"intent": 10, "hit": False, "applied": {"inside": [], "out_of_play": []}}], "missed"),
    ("hostile", [], "unexecuted"),
    ("hostile", [{"intent": 10, "hit": True, "applied": {"inside": [["c", 5.0], ["n", 60.0]], "out_of_play": ["c", "n"]}}],
     "harmful"),                                    # a neutral inside the radius
    ("neutral", [], "harmful"),                     # nominated a neutral
    ("unknown", [], "unwarranted"),                 # nothing in truth made it a target
])
def test_a_nomination_is_judged_against_truth(aff: str, effects: list[dict[str, Any]], want: str):
    s, fx = nomination_world(aff, effects)
    a = by_intent(L.episode_labels(v3_record(s, effects=fx)))[10]
    assert a["label"] == want, a["nomination"]
    assert a["nomination"]["target"] == "c" and a["nomination"]["instrument"]["name"] == "v3.nomination"


# ---- 2.9: review decisions -------------------------------------------------------------------------------

def test_review_decisions_are_judged_against_truth():
    s = Synth(400)
    s.contact("h", aff="hostile", path=inbound())
    s.contact("n", kind="fishing_vessel", aff="neutral", path=[(0.0, -1000.0, 50.0)])
    s.track("T-0001", "h", ident=lambda t: UNRESOLVED)
    s.track("T-0002", "n", ident=lambda t: UNRESOLVED)
    review = [
        {"t": 200.0, "kind": "declaration", "action": "confirm", "track": "T-0001", "result": "done"},
        {"t": 200.0, "kind": "declaration", "action": "confirm", "track": "T-0002", "result": "done"},
        {"t": 200.0, "kind": "declaration", "action": "revoke", "track": "T-0002", "result": "done"},
        {"t": 200.0, "kind": "flag", "action": "dismiss", "track": "T-0001", "result": "done"},
        {"t": 200.0, "kind": "ais_claim", "action": "confirm", "track": "T-0002", "by_track": "T-0002", "result": "done"},
        {"t": 200.0, "kind": "ais_claim", "action": "confirm", "track": "T-0001", "by_track": "T-0002", "result": "done"},
        {"t": 200.0, "kind": "ais_claim", "action": "confirm", "track": "T-0001", "result": "done"},
        {"t": 200.0, "kind": "lost_contact", "action": "accept", "track": "T-0002", "result": "done"},
        {"t": 200.0, "kind": "lost_contact", "action": "accept", "track": "T-0001", "result": "done"},
        {"t": 200.0, "kind": "unspend", "action": "unspend", "uid": 7, "result": "done"},
        {"t": 210.0, "kind": "unspend", "action": "unspend", "uid": 8, "result": "done"},
        {"t": 200.0, "kind": "declaration", "action": "confirm", "track": "T-0001", "result": "error 409: gone"},
    ]
    fx = [{"intent": 10, "platform": "8", "platform_removed": True, "applied": {}}]
    got = [r["label"] for r in L.episode_labels(v3_record(s, review=review, effects=fx))["review"]]
    assert got == ["right", "wrong", "right", "wrong", "right", "wrong", "unjudged", "wrong", "right", "right", "wrong",
                   "no_effect"]


# ---- the physical model's wrong calls and sensing errors -------------------------------------------------

def test_the_wrong_call_draw_is_per_observer_sensor_and_level_at_the_level_s_rate():
    ids = [f"c-{k}" for k in range(4000)]
    ident = sum(L.wrong_call(31, 0.05, 3, 0, "eo_ir", c, "identify") for c in ids) / len(ids)
    rec = sum(L.wrong_call(31, 0.05, 3, 0, "eo_ir", c, "recognise") for c in ids) / len(ids)
    assert 0.04 < ident < 0.06 and 0.085 < rec < 0.115
    assert not any(L.wrong_call(31, 0.05, 3, 0, "eo_ir", c, "detect") for c in ids[:200])
    # another observer, another draw
    a = [L.wrong_call(31, 0.05, 3, 0, "eo_ir", c, "identify") for c in ids]
    b = [L.wrong_call(31, 0.05, 4, 0, "eo_ir", c, "identify") for c in ids]
    assert a != b
    assert a == [L.wrong_call(31, 0.05, 3, 0, "eo_ir", c, "identify") for c in ids], "deterministic"


def test_a_sensing_error_needs_every_holder_wrong():
    s = Synth(300)
    s.fleet = [{"uid": 3, "callsign": "PUMA 1", "platform": "puma", "sensors": [{"type": "eo_ir", "range_m": 4000.0}]}]
    s.sensing = {"seed": 31, "misclassification_rate": 0.05}
    wrong = next(c for c in (f"c-{k}" for k in range(2000)) if L.wrong_call(31, 0.05, 3, 0, "eo_ir", c, "identify"))
    right = next(c for c in (f"c-{k}" for k in range(2000)) if not L.wrong_call(31, 0.05, 3, 0, "eo_ir", c, "identify"))
    for cid in (wrong, right):
        s.contact(cid, path=inbound())
    lab = labels(s)
    assert need(lab, wrong)["sensing_error"] and not need(lab, wrong)["eligible"]
    assert not need(lab, right)["sensing_error"]
    # a second holder whose draw is right: no longer an error
    s.fleet.append({"uid": 4, "callsign": "PUMA 2", "platform": "puma", "sensors": [{"type": "eo_ir", "range_m": 4000.0}]})
    if not L.wrong_call(31, 0.05, 4, 0, "eo_ir", wrong, "identify"):
        assert not need(labels(s), wrong)["sensing_error"]


# ---- the metrics -----------------------------------------------------------------------------------------

def test_need_served_per_domain_and_time_to_serve_censored_at_each_grace():
    s = Synth(700)
    s.contact("boat", kind="small_fast_boat", path=inbound())
    s.contact("uuv", kind="uuv", domain="subsurface", path=inbound(speed=10.0))
    s.track("T-0001", "boat", ident=lambda t: ("small_fast_boat", 0.9, None, ["eo_ir"]) if t >= 120.0 else UNRESOLVED)
    s.track("T-0002", "uuv", ident=lambda t: UNRESOLVED)
    rec = v3_record(s)
    lab = L.episode_labels(rec)
    m = M.episode_metrics(rec, lab)
    assert m["by_domain"]["surface"]["served_share"] == 1.0 and m["by_domain"]["subsurface"]["served_share"] == 0.0
    tts = dict(zip([n["contact"] for n in lab["needs"] if n["eligible"]], m["time_to_serve"]))
    assert tts["uuv"] == DOMAINS["subsurface"].grace_s and tts["boat"] < 60.0
    assert m["rmst"] is None  # two domains: per domain only


def test_the_labels_say_their_instrument_and_version():
    lab = labels(one("small_fast_boat", "surface", ("small_fast_boat", 0.9, None, ["eo_ir"]), 120.0))
    assert lab["version"] == "3" and lab["world"] == "golden-gate-v3"
    assert lab["needs"][0]["instrument"] == {"name": "v3.contact_need", "version": "3"}
    assert lab["fault_needs"] == [] or lab["fault_needs"][0]["instrument"]["name"] == "v2.fault_need"


def test_v2_s_labels_are_untouched():
    """v3 builds beside v2: v2's labels still say version 2 and the Golden Gate's bridge."""
    from eval.headless.v2 import labels as V2

    assert V2.VERSION == "2" and V2.BRIDGE == BR and V2.DEADLINE_S == 60.0


def test_params_scale_the_late_tolerance_per_domain():
    s = one("uuv", "subsurface", ("submerged", 0.7, None, ["sonar"]), 450.0)
    n = need(L.episode_labels(v3_record(s), params=Params(late_s=120.0)), "c")
    assert not n["served"] and n["served_late"]  # 600 s at twice the grace
    rng = random.Random(1)
    assert rng.random() < 1.0
