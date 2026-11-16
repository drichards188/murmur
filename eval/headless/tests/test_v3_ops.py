"""Scoring v3's F14-F17 (eval/headless/v3/families_ops.py) and the labels they need (labels_ops.py:
SCORING-V3.md 2.8 nominations, 2.9 review decisions, 2.10 collect and cover, and the families' own
needs), and review.py's evidence reviewer. Labels on small synthetic records (test_v2_labels.py's
`Synth`, moved under `v3`); the families' draws on the worlds' own grids; and one smoke episode per
family and world on the real `murmur-headless` binary (built here if stale)."""

from __future__ import annotations

import datetime as dt
import json
import shutil
import subprocess
import tomllib
from pathlib import Path
from typing import Any

import pytest

from eval.headless import REPO
from eval.headless import review as rv
from eval.headless.tests.test_engage_space import FakeSim
from eval.headless.tests.test_v2_labels import EPOCH, Synth, inbound
from eval.headless.tests.test_v3_labels import UNRESOLVED, by_intent, v3_record
from eval.headless.v3 import families as F
from eval.headless.v3 import families_ops as FO
from eval.headless.v3 import labels as L
from eval.headless.v3 import labels_ops as O
from eval.headless.v3.mission import mission_text
from eval.headless.v3.settings import v0
from eval.world import load_world

SEEDS = range(5400, 5412)
OPS_NAMES = [n for n in F.all_names() if FO.is_ops(n)]


def iso(t: float) -> str:
    return dt.datetime.fromtimestamp(EPOCH + t, dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


# ---- 2.8: nominations in three dimensions, our own vehicles included ------------------------------

def nomination(inside: list[list[Any]], alts: dict[str, float], ours: list[str] | None = None,
               our_alt: float = 2.0) -> dict[str, Any]:
    s = Synth(400)
    s.contact("c", aff="hostile", path=inbound())
    s.contact("air", kind="light_aircraft", domain="air", aff="neutral", path=[(0.0, -1000.0, 0.0)])
    s.contact("sus", aff="suspect", path=[(0.0, -1000.0, 30.0)])
    s.track("T-0001", "c", ident=lambda t: UNRESOLVED)
    s.vehicle(21, lambda t: {"alt_m": our_alt})
    s.intent(10, "engage", [7], 150.0, {"track_id": "T-0001", "effect_radius_m": 75.0},
             states=[(150.0, "checked", "check"), (150.0, "pending_approval", "require_approval")])
    fx = [{"intent": 10, "hit": True, "applied": {"radius_m": 75.0, "inside": inside, "out_of_play": ["c"],
                                                  "applied_t": EPOCH + 300.0, "collateral": {"ours": ours or []}}}]
    rec = v3_record(s, effects=fx)
    rec["truth"]["stated"] = {"contact_alt_m": alts}
    return by_intent(L.episode_labels(rec))[10]


@pytest.mark.parametrize("inside, alts, ours, our_alt, want", [
    ([["c", 4.0], ["air", 20.0]], {"air": 420.0}, [], 2.0, "effective"),   # an aircraft high over it
    ([["c", 4.0], ["air", 20.0]], {"air": 60.0}, [], 2.0, "harmful"),      # a low one inside
    ([["c", 4.0]], {}, ["21"], 2.0, "harmful"),                            # one of ours inside
    ([["c", 4.0]], {}, ["21"], 500.0, "effective"),                        # one of ours far above it
    ([["c", 4.0], ["sus", 30.0]], {}, [], 2.0, "effective"),               # a suspect: recorded, not harm
])
def test_a_nomination_s_collateral_is_three_dimensional_and_counts_ours(inside: list[list[Any]], alts: dict[str, float],
                                                                         ours: list[str], our_alt: float, want: str):
    a = nomination(inside, alts, ours, our_alt)
    assert a["label"] == want, a["nomination"]
    n = a["nomination"]
    if want == "effective" and alts.get("air", 0.0) > 75.0:
        assert n["inside_2d_only"][0]["contact"] == "air", "the record's horizontal inside, kept apart"
    if any(c == "sus" for c, _ in inside):
        assert n["collateral_doubtful"][0]["contact"] == "sus"


# ---- 2.9: review decisions -------------------------------------------------------------------------

def review_world(decisions: list[dict[str, Any]], same: dict[str, str] | None = None) -> list[str]:
    s = Synth(400)
    s.contact("h", aff="hostile", path=inbound())
    s.contact("n", kind="fishing_vessel", aff="neutral", path=[(0.0, -1000.0, 50.0)])
    s.contact("back", kind="fishing_vessel", aff="neutral", path=[(0.0, -1200.0, 50.0)])
    s.contact("spoof", aff="suspect", path=[(0.0, -1300.0, 50.0)])
    for tid, c in (("T-0001", "h"), ("T-0002", "n"), ("T-0003", "back"), ("T-0004", "spoof")):
        s.track(tid, c, ident=lambda t: UNRESOLVED)
    rec = v3_record(s, review=decisions)
    rec["truth"]["stated"] = {"same_vessel": same or {}}
    return [r["label"] for r in L.episode_labels(rec)["review"]]


def test_claims_flags_declines_and_leaves_are_judged_against_truth():
    d = [
        # the claimant is the lost vessel's stated return: confirming is right, declining wrong
        {"t": 200.0, "kind": "ais_claim", "action": "confirm", "track": "T-0002", "by_track": "T-0003", "result": "done"},
        {"t": 200.0, "kind": "ais_claim", "action": "decline", "track": "T-0002", "by_track": "T-0003", "result": "done (no request)"},
        # a spoofer: confirming is wrong, declining right, leaving unjudged
        {"t": 200.0, "kind": "ais_claim", "action": "confirm", "track": "T-0002", "by_track": "T-0004", "result": "done"},
        {"t": 200.0, "kind": "ais_claim", "action": "decline", "track": "T-0002", "by_track": "T-0004", "result": "done (no request)"},
        {"t": 200.0, "kind": "ais_claim", "action": "leave", "track": "T-0002", "by_track": "T-0004", "result": "done (no request)"},
        # a flag naming a hostile and a neutral: confirming is right, dismissing wrong
        {"t": 200.0, "kind": "flag", "action": "confirm", "tracks": ["T-0002", "T-0001"], "result": "done"},
        {"t": 200.0, "kind": "flag", "action": "dismiss", "tracks": ["T-0002", "T-0001"], "result": "done"},
        # a flag on a neutral alone: dismissing is right; declaring it wrong; declaring the hostile right
        {"t": 200.0, "kind": "flag", "action": "dismiss", "tracks": ["T-0002"], "result": "done"},
        {"t": 200.0, "kind": "flag", "action": "declare", "track": "T-0002", "tracks": ["T-0002"], "result": "done"},
        {"t": 200.0, "kind": "flag", "action": "declare", "track": "T-0001", "tracks": ["T-0001"], "result": "done"},
    ]
    assert review_world(d, {"back": "n"}) == ["right", "wrong", "wrong", "right", "unjudged", "right", "wrong", "right",
                                              "wrong", "right"]
    # without the statement, the return is another contact: a confirmation is wrong
    assert review_world(d[:1]) == ["wrong"]


# ---- 2.10: collect and cover ------------------------------------------------------------------------

GAP = {"id": "gap-1", "place": "x", "center": [-123.06, 38.30], "radius_m": 2000.0, "due_s": 6000.0}


def collect_world(product_s: float | None, area_center: list[float] | None = None, near_s: float | None = None,
                  none_cover: Any = "unset") -> dict[str, Any]:
    s = Synth(400)
    s.contact("c", aff="neutral", path=inbound())
    if near_s is not None:
        s.vehicle(3, lambda t: {"lon": GAP["center"][0], "lat": GAP["center"][1], "alt_m": 100.0} if t >= near_s else {})
    area = {"kind": "circle", "center": area_center or GAP["center"], "radius_km": 2.0}
    s.intent(20, "collect", [], 100.0, {"area": area},
             states=[(100.0, "checked", "check"), (100.0, "pending_approval", "x"), (101.0, "dispatched", "x")])
    rec = v3_record(s)
    rec["truth"]["stated"] = {"gap_needs": [GAP]}
    order = {"class_id": "capella", "product_s": product_s} if product_s is not None else None
    rec["v3"]["space"] = {"intents": [{"id": 20, "kind": "collect", "checks": [{"rule": "feasibility", "outcome": "pass"}],
                                       "order": order, "op": []}]}
    nl = {"needs": [], "gap_cover": {"gap-1": None}} if none_cover == "unset" else (
        None if none_cover is None else {"needs": [], "gap_cover": none_cover})
    return L.episode_labels(rec, none_labels=nl)


def test_a_collect_is_useful_late_redundant_justified_or_not():
    lab = collect_world(5000.0)
    assert by_intent(lab)[20]["label"] == "useful" and lab["ops"]["gap"][0]["served"]
    assert lab["ops"]["gap"][0]["by"] == "collect"
    assert by_intent(collect_world(7000.0))[20]["label"] == "late"
    assert not collect_world(7000.0)["ops"]["gap"][0]["served"]
    assert by_intent(collect_world(5000.0, none_cover={"gap-1": 900.0}))[20]["label"] == "redundant"
    assert by_intent(collect_world(5000.0, near_s=50.0))[20]["label"] == "redundant", "ours covered it before"
    # served no need (choice 40): over no stated need's area not justified; over one, never tasked,
    # justified (the brief's tasking supported asking)
    assert by_intent(collect_world(5000.0, area_center=[-122.0, 36.0]))[20]["label"] == "not_justified", "no need's area"
    a = by_intent(collect_world(None))[20]
    assert a["label"] == "justified" and a["justified_by"] == ["tasking"], "never tasked"
    assert a["space"]["instrument"] == {"name": "v3.collect", "version": "4"}
    assert by_intent(collect_world(5000.0, none_cover=None))[20]["label"] == "unjudged", "no none replicate"
    served_by_ours = collect_world(None, near_s=300.0)["ops"]["gap"][0]
    assert served_by_ours["served"] and served_by_ours["by"] == "ours"


NEED = {"id": "gnss-1", "uid": 18, "callsign": "FERRET 1", "route": [[-122.46, 37.8085], [-122.448, 37.8082]],
        "start_s": 400.0, "end_s": 1300.0, "min_level": 10, "service": {"type": "gnss", "class": "gps"},
        "area": {"kind": "circle", "center": [-122.454, 37.8083], "radius_km": 0.8}}


def cover_world(coverage: str, held_at: float | None, outage_at: float = 600.0, other_fail: bool = False,
                stated: bool = True, created: float = 100.0, area: dict[str, Any] | None = None,
                end_s: float | None = None) -> dict[str, Any]:
    s = Synth(900)
    s.contact("c", aff="neutral", path=inbound())
    s.intent(30, "cover", [], created, {"area": area or NEED["area"], "service": NEED["service"], "min_level": 10.0},
             states=[(created, "checked", "check"), (created, "rejected" if coverage == "fail" else "dispatched", "x")])
    s.intent(31, "patrol", [18], 400.0, {"route": NEED["route"]}, source="agent-scripted",
             states=[(400.0, "dispatched", "x")], op={18: [(401.0, "acked"), (402.0, "active")]})
    if held_at is not None:
        s.intent(32, "hold", [18], held_at, {}, states=[(held_at, "dispatched", "x")],
                 op={18: [(held_at + 1.0, "acked"), (held_at + 2.0, "done")]})
    rec = v3_record(s)
    rec["truth"]["stated"] = {"cover_needs": [{**NEED, **({"end_s": end_s} if end_s is not None else {})}]} if stated else {}
    checks = [{"rule": "space_query", "outcome": "fail" if other_fail else "pass"}, {"rule": "coverage", "outcome": coverage}]
    rec["v3"]["space"] = {"intents": [{"id": 30, "kind": "cover", "checks": checks, "order": None, "op": []}]}
    rec["script"] = [{"t": 1.0, "kind": "probe", "id": "gnss-1", "answer": {"samples": [
        {"t": iso(400.0), "visible": 10}, {"t": iso(outage_at), "visible": 9}, {"t": iso(1000.0), "visible": 11}]}}]
    return L.episode_labels(rec)


def test_a_cover_is_useful_when_the_predicted_outage_was_avoided():
    lab = cover_world("fail", held_at=550.0)
    a = by_intent(lab)[30]
    assert a["label"] == "useful", a["space"]
    assert lab["ops"]["cover"][0]["served"] and lab["ops"]["cover"][0]["outage"]
    # served no need (choice 40), on the context the agent could see: the outage predicted over the
    # stated run's area, which then went through it, is justified; so is a cover of that area asked
    # before its run's end that predicted no outage (the service confirmed, as tasked); one of no
    # stated need's area that predicted none, or of that area after its run, is not
    lab = cover_world("fail", held_at=None)
    assert by_intent(lab)[30]["label"] == "justified" and lab["ops"]["cover"][0]["cause"] == "run_during_outage"
    a = by_intent(cover_world("pass", held_at=None))[30]
    assert a["label"] == "justified" and a["justified_by"] == ["tasking"], "no outage predicted, as tasked"
    assert a["space"]["instrument"] == {"name": "v3.cover", "version": "4"}
    assert by_intent(cover_world("pass", held_at=None, stated=False))[30]["label"] == "not_justified"
    assert by_intent(cover_world("pass", held_at=None, created=500.0, end_s=450.0))[30]["label"] == "not_justified"
    assert by_intent(cover_world("fail", held_at=None, outage_at=420.0, created=500.0, end_s=450.0))[30]["label"] \
        == "not_justified"
    assert by_intent(cover_world("fail", held_at=None, stated=False))[30]["label"] == "unjudged"
    assert by_intent(cover_world("fail", held_at=None, other_fail=True))[30]["label"] == "refused"
    late = cover_world("fail", held_at=550.0, created=450.0)["ops"]["cover"][0]
    assert not late["served"] and late["cause"] == "not_asked", "asked after the run began"


def test_a_hold_of_the_stated_run_s_vehicle_ahead_of_its_outage_is_justified():
    """A hold or an rtl that served no need is justified by the stated tasking when it names F14's run
    vehicle before the run's end with a predicted outage of the run still to come (choice 41): taking
    the vehicle off its run ahead of the outage is what the tasking asks. After the outage, or with no
    stated run, it is judged on the picture alone (here none: not justified)."""
    a = by_intent(cover_world("fail", held_at=550.0))[32]
    assert a["label"] == "justified" and a["justified_by"] == ["tasking"], (a["label"], a.get("tasked"))
    assert a["tasked"] == {"tasked": True, "need": "gnss-1", "outage_s": 600.0,
                           "instrument": {"name": "v3.tasked_hold", "version": "4"}}
    assert by_intent(cover_world("fail", held_at=650.0))[32]["label"] == "not_justified", "after the outage"
    assert by_intent(cover_world("fail", held_at=550.0, stated=False))[32]["label"] == "not_justified"
    assert by_intent(cover_world("fail", held_at=550.0, end_s=500.0, outage_at=480.0))[32]["label"] == "not_justified"


def box(west: float, east: float, south: float = 37.806, north: float = 37.81) -> dict[str, Any]:
    return {"kind": "polygon", "ring": [[west, south], [east, south], [east, north], [west, north]]}


def test_a_polygon_area_answers_the_stated_need_its_circle_overlaps():
    """A cover or a collect over a polygon (the decision engine's other kind of area) answers the
    stated need whose circle it overlaps, as a circle does (2.10). Until 2026-11-15 only a circle was
    matched, so the model pilot's one polygon cover (F14 at the Golden Gate, seed 6601, the run's
    route boxed) counted as no need's and its need as not asked."""
    assert O.cover_need_of(box(-122.46, -122.448), [NEED]) is NEED, "the need's centre inside"
    assert O.cover_need_of(box(-122.446, -122.44), [NEED]) is NEED, "an edge 700 m from its centre, within 800"
    assert O.cover_need_of(box(-122.443, -122.44), [NEED]) is None, "960 m off"
    assert O.cover_need_of({"kind": "polygon", "ring": [[-122.45, 37.8]]}, [NEED]) is None, "not a ring"
    g = GAP["center"]
    assert O.gap_need_of(box(g[0] - 0.01, g[0] + 0.01, g[1] - 0.01, g[1] + 0.01), [GAP]) is GAP
    lab = cover_world("fail", held_at=550.0, area=box(-122.46, -122.448))
    assert by_intent(lab)[30]["space"]["need"] == "gnss-1" and by_intent(lab)[30]["label"] == "useful"
    assert lab["ops"]["cover"][0]["served"] and lab["ops"]["cover"][0]["asked"] == [30]


def test_an_outage_at_the_run_s_own_start_is_the_scheduler_s():
    rec = {"clock": {"epoch": EPOCH}, "script": [{"kind": "probe", "id": "gnss-1", "answer": {"samples": [
        {"t": iso(400.0), "visible": 8}, {"t": iso(700.0), "visible": 11}, {"t": iso(1000.0), "visible": 9}]}}]}
    assert O.outage_samples(rec, NEED) == [1000.0]


def test_without_the_space_digest_collect_and_cover_stay_unjudged():
    s = Synth(300)
    s.contact("c", path=inbound())
    s.intent(10, "collect", [], 100.0, {"area": "x"}, states=[(100.0, "checked", "check")])
    s.intent(11, "cover", [], 100.0, {"area": "x"}, states=[(100.0, "checked", "check")])
    assert {a["label"] for a in L.episode_labels(v3_record(s))["actions"]} == {"unjudged"}


def test_the_space_digest_reads_a_collect_and_a_cover_from_the_audit_log():
    ms = lambda t: int((EPOCH + t) * 1000)  # noqa: E731
    lines = [
        {"type": "intent.created", "t": ms(50), "intent": {"id": 7, "kind": "collect", "source": {"kind": "agent", "id": "a"},
                                                         "params": {"area": {"kind": "circle"}, "window": {"from": ms(50), "to": ms(9000)}}}},
        {"type": "space.query", "t": ms(50), "answer": {"result": {"result": "tasking", "options": [{"class_id": "capella"}]}}},
        {"type": "check.result", "t": ms(50), "intent": 7, "phase": "submit", "passed": True,
         "results": [{"rule": "feasibility", "outcome": "pass", "detail": "ok"}]},
        {"type": "intent.dispatched", "t": ms(51), "intent": 7, "space": {"class_id": "capella", "collect_time": ms(4000),
                                                                           "product_time": ms(7000)}},
        {"type": "op.state", "t": ms(51), "intent": 7, "uid": "norad:1", "state": "tasked", "detail": "x"},
        {"type": "intent.created", "t": ms(60), "intent": {"id": 8, "kind": "orbit", "params": {}}},
    ]
    d = O.digest_space("\n".join(json.dumps(x) for x in lines), EPOCH)
    (c,) = d["intents"]
    assert c["id"] == 7 and c["params"]["window_s"] == [50.0, 9000.0] and c["answer"]["classes"] == ["capella"]
    assert c["order"]["product_s"] == 7000.0 and c["op"][0][2] == "tasked" and c["checks"][0]["rule"] == "feasibility"
    assert O.digest_space(json.dumps(lines[-1]), EPOCH) is None


def test_the_engagement_need_is_right_only_when_effective_and_restrained():
    def world(extra: list[dict[str, Any]]) -> dict[str, Any]:
        s = Synth(400)
        s.contact("h", aff="hostile", path=inbound())
        s.contact("n", kind="fishing_vessel", aff="neutral", path=[(0.0, -1000.0, 50.0)])
        s.track("T-0001", "h", ident=lambda t: UNRESOLVED)
        s.track("T-0002", "n", ident=lambda t: UNRESOLVED)
        s.intent(10, "engage", [7], 150.0, {"track_id": "T-0001", "effect_radius_m": 75.0},
                 states=[(150.0, "checked", "check"), (150.0, "pending_approval", "x")])
        for i, tid in enumerate(extra):
            s.intent(11 + i, "engage", [7], 160.0, {"track_id": tid, "effect_radius_m": 75.0},
                     states=[(160.0, "checked", "check"), (160.0, "pending_approval", "x")])
        rec = v3_record(s, effects=[{"intent": 10, "hit": True, "applied": {"radius_m": 75.0, "inside": [["h", 3.0]],
                                                                             "out_of_play": ["h"]}}])
        rec["truth"]["stated"] = {"engagement": {"target": "h", "should_engage": True}}
        return L.episode_labels(rec)["ops"]["engagement"][0]

    assert world([])["right"] and world([])["effective"]
    e = world(["T-0002"])
    assert e["effective"] and not e["right"] and e["harmful"] == 1 and not e["restraint_ok"]


def test_an_adopted_nomination_is_judged_apart_from_the_agent_s_own():
    """F15 version 2 (bd murmur-e3b): the agent's nomination refused, the adopter's of it (op-4) took
    effect. `right` stays the agent's own (false, its cause `refused`); `right_with_adopted` reads
    the adopter's nomination as the agent's are read; an adopted one on a neutral is harmful."""
    def world(adopted_track: str) -> dict[str, Any]:
        s = Synth(400)
        s.contact("h", aff="hostile", path=inbound())
        s.contact("n", kind="fishing_vessel", aff="neutral", path=[(0.0, -1000.0, 50.0)])
        s.track("T-0001", "h", ident=lambda t: UNRESOLVED)
        s.track("T-0002", "n", ident=lambda t: UNRESOLVED)
        s.intent(10, "engage", [7], 150.0, {"track_id": "T-0001", "effect_radius_m": 75.0},
                 states=[(150.0, "rejected", "check")], source="agent-watch")
        s.intent(11, "engage", [7], 156.0, {"track": adopted_track, "effect_radius_m": 75.0},
                 states=[(156.0, "checked", "check"), (156.0, "pending_approval", "x"), (170.0, "effected", "release")],
                 source="op-4")
        target = "h" if adopted_track == "T-0001" else "n"
        rec = v3_record(s, effects=[{"intent": 11, "hit": True, "applied": {"radius_m": 75.0, "inside": [[target, 3.0]],
                                                                             "out_of_play": [target]}}],
                        review=[{"t": 156.0, "kind": "adopt", "action": "adopt", "item": 10, "intent": 11, "by": "op-4",
                                 "result": "done", "accepted": {"accept_lost": [], "accept_behaving": [{"track": "T-0009"}]}}])
        rec["truth"]["stated"] = {"engagement": {"target": "h", "should_engage": True}}
        return L.episode_labels(rec)["ops"]["engagement"][0]

    e = world("T-0001")
    assert not e["right"] and e["cause"] == "refused" and e["nominations"] == 1
    assert e["right_with_adopted"] and e["adopted"] == 1 and e["adopted_executed"] == 1 and e["adopted_effective"] == 1
    assert e["adopted_finals"] == ["effected"]
    bad = world("T-0002")
    assert not bad["right_with_adopted"] and bad["adopted_harmful"] == 1 and not bad["restraint_ok_with_adopted"]


def test_the_engagement_digest_and_the_form_s_acceptances():
    """The record's engagement digest (labels_ops `digest_engage`): nominations with their failing
    checks, acceptances, lapses, effects, stops; none without a nomination. The oracle's attempt rows
    (oracle_ops `attempt_row`) say what an operator could accept of each blocker."""
    from eval.headless.v3.oracle_ops import attempt_row

    ms = lambda s: (EPOCH + s) * 1000.0  # noqa: E731
    lines = [
        {"type": "intent.created", "t": ms(10), "intent": {"id": 4, "kind": "engage", "source": {"kind": "agent", "id": "agent-watch"}}},
        {"type": "check.result", "t": ms(10), "intent": 4, "phase": "submit",
         "results": [{"rule": "keep_out_tracks", "outcome": "fail", "detail": "track T-0010 ..."}, {"rule": "reach", "outcome": "pass"}]},
        {"type": "intent.created", "t": ms(14), "intent": {"id": 5, "kind": "engage", "source": {"kind": "human", "id": "op-4"}}},
        {"type": "engage.accepted", "t": ms(14), "intent": 5, "track": "T-0010", "by": "op-4"},
        {"type": "engage.accepted", "t": ms(14), "intent": 5, "contact": "T-0028", "by": "op-4"},
        {"type": "engage.acceptance_lapsed", "t": ms(19), "intent": 5, "track": "T-0010", "kind": "velocity", "detail": "d"},
        {"type": "engage.effect", "t": ms(60), "intent": 5, "uid": 21, "point": [-122.5, 37.8], "radius_m": 75.0},
        {"type": "engage.stopped", "t": ms(61), "intent": 9, "cause": {"kind": "picture"}, "reason": "not ours"},
    ]
    dg = O.digest_engage("\n".join(json.dumps(x) for x in lines), EPOCH)
    assert [(n["id"], n["source"]["kind"], [f["rule"] for f in n["failing"]]) for n in dg["nominations"]] == [
        (4, "agent", ["keep_out_tracks"]), (5, "human", [])]
    assert [(a["what"], a["id"]) for a in dg["accepted"]] == [("track", "T-0010"), ("contact", "T-0028")]
    assert dg["lapsed"][0]["kind"] == "velocity" and dg["lapsed"][0]["t"] == 19.0
    assert dg["effects"][0]["uid"] == 21 and dg["stopped"] == []
    assert O.digest_engage(json.dumps(lines[-1]), EPOCH) is None
    row = attempt_row({"uid": 21, "callsign": "BLADE 1", "passed": False, "checks": [{"rule": "keep_out_tracks", "outcome": "fail"}],
                       "keep_outs": {"blocking": 14, "items": [
                           {"id": "T-0028", "subject": "lost_contact", "blocks": True, "domain": "air", "short_m": 900.4,
                            "region": {"kind": "aircraft"}, "lost": {"track": "T-0028", "since": 5}, "behaving": None},
                           {"id": "T-0010", "subject": "track", "blocks": True, "domain": "air", "short_m": 12.0,
                            "region": {"kind": "aircraft"}, "behaving": {"acceptable": True, "clears": False}},
                           {"id": "1", "subject": "vehicle", "blocks": True, "region": {"kind": "circling"}}]}})
    assert [b[5] for b in row["blocking"]] == [{"lost": True, "since": 5}, {"acceptable": True, "clears": False}, None]
    assert row["blocking_n"] == 14 and row["blocking"][0][:5] == ["T-0028", "lost_contact", "aircraft", "air", 900]


# ---- F15 version 3 (bd murmur-yu6s): our drifters, the escort's departure, the oracle's restraint ----

def field_file(path: Path, steps: list[list[tuple[int, int]]], nodata: int = -32768) -> Path:
    """A 2 x 2 current field in bake_world.py's format over [0, 0.01] degrees of latitude and
    longitude, one step an hour from t 0; each step's four (east, north) pairs in cm/s, row by row
    from the south-west."""
    import gzip
    import struct

    h = {"lat0": 0.0, "lon0": 0.0, "dlat": 0.01, "dlon": 0.01, "nlat": 2, "nlon": 2, "scale": 0.01, "nodata": nodata,
         "t0": 0.0, "dt": 3600.0, "nt": len(steps)}
    body = b"".join(struct.pack("<2h", e, n) for step in steps for e, n in step)
    path.write_bytes(gzip.compress(json.dumps(h).encode() + b"\n" + body))
    return path


def test_the_current_field_is_read_as_the_simulator_reads_it(tmp_path: Path):
    """families_ops `CurrentField` (crates/murmur-headless sim/water.rs): bilinear in space, linear in
    time between steps, held at the first and last step, None outside the box or next to a point
    without data."""
    f = FO.CurrentField(field_file(tmp_path / "c.gz", [[(100, 0), (100, 0), (100, 0), (300, 0)],
                                                       [(0, 200), (0, 200), (0, 200), (0, 200)]]))
    e, n = f.at(0.005, 0.005, 0.0)
    assert e == pytest.approx(1.5) and n == pytest.approx(0.0)
    e, n = f.at(0.005, 0.005, 1800.0)
    assert e == pytest.approx(0.75) and n == pytest.approx(1.0)
    assert f.at(0.005, 0.005, -500.0) == f.at(0.005, 0.005, 0.0) and f.at(0.0, 0.0, 9000.0) == pytest.approx((0.0, 2.0))
    assert f.at(0.02, 0.005, 0.0) is None and f.at(-0.001, 0.005, 0.0) is None
    holed = FO.CurrentField(field_file(tmp_path / "h.gz", [[(100, 0), (100, 0), (100, 0), (-32768, -32768)]]))
    assert holed.at(0.005, 0.005, 0.0) is None


def test_a_drifter_goes_with_the_current_from_its_first_sample_and_stays_aground():
    """families_ops `drift_path` (crates/murmur-standins behavior/drifter.rs): still until its first
    water sample, a second after the stand-ins start; then the current where it is; once the water
    under it is no deeper than its draft it moves no more; outside the field it is still."""
    class East:
        def at(self, lat: float, lon: float, t: float) -> tuple[float, float] | None:
            return (0.5, 0.0) if lon < 0.01 else None

    shore = 30.0 / 111_320.0  # land 30 m east of the start
    p = FO.drift_path("golden-gate-v3", (0.0, 0.0), 0.0, 100.0, start_s=1.0, field=East(), depth=lambda la, lo: 10.0)
    assert p[2, 1] == 0.0 and p[3, 1] > 0.0  # its first move is at 2 s
    assert p[100, 1] * 111_320.0 == pytest.approx(0.5 * 98, abs=1e-6)
    g = FO.drift_path("golden-gate-v3", (0.0, 0.0), 0.0, 100.0, start_s=1.0, field=East(),
                      depth=lambda la, lo: 10.0 if lo <= shore else 0.3)
    assert g[100, 1] * 111_320.0 == pytest.approx(30.5, abs=1e-6) and g[100, 1] == g[80, 1]
    out = FO.drift_path("golden-gate-v3", (0.02, 0.0), 0.0, 100.0, start_s=1.0, field=East(), depth=lambda la, lo: 10.0)
    assert out[100, 1] == 0.02


@pytest.mark.parametrize("name", ["v3-f15-engage-gg", "v3-f15-engage-sd"])
def test_f15_holds_clear_of_our_drifters_and_the_escort_peels_away(name: str, tmp_path: Path):
    """F15 version 3: no drifter of ours comes within `DRIFT_CLEAR_M` of the holding point at any
    second of the episode, on its predicted drift; an escort leaves 30 to 110 s after the first act,
    before the third, on water, within 105 degrees of straight away from the hostile, 1.5 to 3 km, at
    its kind's speed."""
    _, world = F.parse(name)
    escorts = 0
    for seed in SEEDS:
        e, path = F.generate(name, seed, tmp_path)
        el, eng, acts = e.truth["element"], e.truth["engagement"], e.truth["acts"]
        assert el["version"] == FO.F15_VERSION == 3 and eng["should_engage"]
        hold = tuple(eng["hold"])
        near = FO.nearest_drifter(hold, FO.drifter_paths(world, e.at, FO.DURATION_S))
        assert near[0] > FO.DRIFT_CLEAR_M and el["drifters"]["nearest_m"] == round(near[0]), (seed, near)
        cs, _ = contacts(path)
        esc = next((c for c in cs if c["id"] == "escort-1"), None)
        if esc is None:
            assert "escort" not in el
            continue
        escorts += 1
        (m,) = esc["maneuver"]
        here, far = tuple(esc["route"][-1]), tuple(m["route"][-1])
        assert acts[0]["at_s"] + 30.0 <= m["at_s"] <= acts[0]["at_s"] + 110.0 < acts[-1]["at_s"], seed
        lo, hi = FO.ESCORT_SPEED_MPS[esc["kind"]]
        assert lo <= m["speed_mps"] <= hi and F.wet(world, [here, far], 3.0)
        assert 1500.0 - 1.0 <= F.dist_m(here, far) <= 3000.0 + 1.0
        off = abs((F.bearing(hold, here) - F.bearing(here, far) + 180.0) % 360.0 - 180.0)
        assert off <= FO.ESCORT_AWAY_DEG[1] + 0.5, (seed, off)
        assert el["escort"]["leaves_s"] == m["at_s"] and m["at_s"] < el["escort"]["clear_s"] < FO.DURATION_S - 300.0
    assert escorts, "the seeds draw at least one escort"


def test_the_oracle_spares_a_contact_by_the_labels_geometry():
    """oracle_ops `unspared` (F15 version 3): a plan spares a contact not hostile by truth when it
    stays beyond the radius plus twice the track's 1-sigma from the plan's point while the effect
    may come (along its course, between braking at once at its kind's deceleration and keeping its
    speed to `LATE_S` after the effect time), or more than the radius above or below the target."""
    from eval.headless.v3.oracle_ops import unspared

    p = (-122.5, 37.81)
    t0 = dt.datetime(2026, 6, 14, 10, 0, tzinfo=dt.timezone.utc)
    stamp = lambda s: (t0 + dt.timedelta(seconds=s)).strftime("%Y-%m-%dT%H:%M:%SZ")  # noqa: E731
    opt = {"uid": 21, "preview": {"t": stamp(0), "effect_t": stamp(60), "effect_point": list(p), "track_sigma_m": 5.0}}

    def boat(bearing: float, dist: float, aff: str = "neutral", kind: str = "fishing_vessel", speed: float = 0.0,
             course: float = 0.0, alt: float = 0.0, domain: str = "surface") -> dict[str, Any]:
        q = F.offset(p, bearing, dist)
        return {"lon": q[0], "lat": q[1], "alt_m": alt, "affiliation": aff, "kind": kind, "domain": domain,
                "speed_mps": speed, "course_deg": course}

    def worst(r: float, c: dict[str, Any]) -> dict[str, Any] | None:
        return unspared(opt, r, {"hostile-1": boat(0.0, 0.0, aff="hostile"), "c": c}, "hostile-1", aim_sigma=2.0,
                        late_s=60.0)

    holding = boat(0.0, 60.0)  # an escort holding 60 m off the point
    w = worst(75.0, holding)
    assert w is not None and w["contact"] == "c" and w["clear_m"] == 85.0 and w["distance_m"] == pytest.approx(60.0, abs=0.5)
    assert worst(40.0, holding) is None, "a 40 m effect spares it: 60 m > 40 + 2 x 5"
    assert worst(75.0, boat(0.0, 60.0, aff="hostile")) is None and worst(75.0, boat(0.0, 60.0, aff="suspect")) is not None
    # in the air: more than the radius (and 10 m) above it is clear, as the labels judge
    assert worst(75.0, boat(0.0, 20.0, kind="light_aircraft", alt=120.0, domain="air")) is None
    assert worst(75.0, boat(0.0, 20.0, kind="light_aircraft", alt=50.0, domain="air")) is not None
    # a sailboat 300 m south: heading for the point it crosses it within 120 s; heading away it never comes near
    assert worst(40.0, boat(180.0, 300.0, kind="sailboat", speed=4.0, course=0.0)) is not None
    assert worst(40.0, boat(180.0, 300.0, kind="sailboat", speed=4.0, course=180.0)) is None
    # a fast boat 30 m north running off north at 10 m/s: braking at once (1.5 m/s^2) it is still 63 m off
    off = boat(0.0, 30.0, kind="small_fast_boat", speed=10.0, course=0.0)
    assert worst(40.0, off) is None and worst(75.0, off)["distance_m"] == pytest.approx(63.3, abs=0.5)
    # a fishing boat 133 m south slowing to its station (0.1 m/s^2 from 3.7 m/s: 68 m on) may stop at
    # the point: version 3's first smoke carried it on at its speed and judged it 89 m off
    assert worst(75.0, boat(180.0, 133.0, speed=3.7, course=0.0))["distance_m"] == pytest.approx(0.0, abs=0.5)
    assert unspared({"uid": 21, "preview": None}, 75.0, {"c": holding}, "hostile-1", aim_sigma=2.0, late_s=60.0) is None


# ---- the evidence reviewer (review.py) ---------------------------------------------------------------

def track(tid: str, kind: str | None, conf: float = 0.9, sb: tuple[str, ...] = ("eo_ir",), conflict: Any = None,
          ais: dict[str, Any] | None = None) -> dict[str, Any]:
    return {"id": tid, "identity": {"kind": kind, "confidence": conf, "supported_by": list(sb), "conflict": conflict,
                                    "ais": ais}}


def test_the_evidence_reviewer_judges_on_what_the_picture_shows():
    def j(*args: Any) -> tuple[str, str]:
        return rv.judge(*args, bar=0.6)

    # the bar is the run's: a 0.65 identification is evidence at 0.6, not at 0.77
    assert rv.eo_kind(track("T-1", "small_fast_boat", conf=0.65), 0.6) == "small_fast_boat"
    assert rv.eo_kind(track("T-1", "small_fast_boat", conf=0.65), 0.77) is None
    decl = {"kind": "declaration", "track": "T-1"}
    assert j(decl, {"T-1": track("T-1", "fishing_vessel")}, None, 5.0, 180.0)[0] == "revoke"
    assert j(decl, {"T-1": track("T-1", "small_fast_boat")}, None, 5.0, 180.0)[0] == "confirm"
    assert j(decl, {"T-1": track("T-1", "small_fast_boat", sb=("radar",))}, None, 5.0, 180.0)[0] == "wait"
    assert j(decl, {}, None, 200.0, 180.0)[0] == "confirm", "the act's evidence after the wait"
    attr = {"kind": "flag", "flag_kind": "attribution", "tracks": ["T-1", "T-2"]}
    both = {"T-1": track("T-1", "fishing_vessel"), "T-2": track("T-2", "small_fast_boat")}
    assert j(attr, both, None, 5.0, 180.0)[0] == "declare:T-2"
    assert j(attr, {"T-1": track("T-1", "fishing_vessel"), "T-2": track("T-2", "sailboat")}, None, 5.0, 180.0)[0] == "dismiss"
    assert j(attr, {}, None, 5.0, 180.0)[0] == "wait" and j(attr, {}, None, 181.0, 180.0)[0] == "dismiss"
    prox = {"kind": "flag", "flag_kind": "proximity", "tracks": ["T-2"]}
    assert j(prox, both, None, 0.0, 180.0)[0] == "dismiss", "proximity alone never declares"
    claim = {"kind": "ais_claim", "track": "T-9", "by_track": "T-3"}
    ok = {"actions": ["confirm"], "blockers": []}
    fishing = {"ship_type": "fishing", "mmsi": "1"}
    assert j(claim, {"T-3": track("T-3", "fishing_vessel", ais=fishing)}, ok, 5.0, 180.0)[0] == "confirm"
    assert j(claim, {"T-3": track("T-3", "sailboat", ais=fishing)}, ok, 5.0, 180.0)[0] == "decline"
    assert j(claim, {"T-3": track("T-3", "fishing_vessel", ais=fishing)}, {"actions": [], "blockers": ["suspect"]},
             5.0, 180.0)[0] == "decline"
    assert j(claim, {"T-3": track("T-3", None, ais=fishing)}, ok, 5.0, 180.0)[0] == "wait"
    assert j(claim, {}, ok, 200.0, 180.0)[0] == "leave"


def test_the_evidence_reviewer_reads_less_under_load_and_waits_for_evidence():
    review = {"declarations": [{"id": "D-1", "track": "T-1", "actions": ["confirm", "revoke"]}], "flags": [],
              "lost_claims": [], "counts": {"declarations": 1, "flags": 0, "lost_claims": 0}}
    state = {"kind": None}
    intents: list[dict[str, Any]] = []

    def handler(method: str, path: str, body: Any, op: Any) -> tuple[int, Any]:
        if path == "/api/review":
            return 200, review
        if path == "/api/policy":
            return 200, {"watch": {"bar": 0.6}}
        if path == "/api/tracks":
            return 200, {"tracks": [track("T-1", state["kind"])] if state["kind"] else []}
        if path == "/api/intents":
            return 200, {"intents": intents}
        return 200, {}

    p = rv.review_policy({"preset": "evidence", "diligence": 1.0, "max_wait_s": 30.0})
    r = rv.Reviewer(FakeSim(handler), p, seed=1, epoch=EPOCH)
    t = r.start(0.0)
    for _ in range(4):  # no evidence: it reads, waits for its next look, reads again
        t = r.wake(t)
    assert not r.decisions and t <= 30.0
    state["kind"] = "fishing_vessel"
    while not r.decisions and t is not None and t < 100.0:
        t = r.wake(t)
    (d,) = r.decisions
    assert (d["action"], d["diligent"], d["result"]) == ("revoke", True, "done")
    assert "EO/IR shows a fishing_vessel" in d["why"]
    # under a flood of cards it skims: yes to what the engine asks
    intents[:] = [{"history": [{"state": "pending_approval", "t": (EPOCH + 1.0) * 1000}]} for _ in range(60)]
    p2 = rv.review_policy({"preset": "evidence", "diligence": 0.9, "fatigue_load": 4.0})
    r2 = rv.Reviewer(FakeSim(handler), p2, seed=3, epoch=EPOCH)
    t = r2.start(0.0)
    while not r2.decisions and t is not None and t < 60.0:
        t = r2.wake(t)
    assert r2.decisions[0]["load"] == 61 and r2.decisions[0]["diligent"] is False
    assert r2.decisions[0]["action"] == "confirm"
    assert "diligence" not in rv.review_policy({}).as_dict(), "a policy without evidence records as before"


# ---- the families' draws --------------------------------------------------------------------------

def test_f17_runs_on_the_scale_world_only_and_the_rest_on_both_maps():
    assert OPS_NAMES == ["v3-f14-space-gg", "v3-f14-space-sd", "v3-f15-engage-gg", "v3-f15-engage-sd",
                         "v3-f16-review-gg", "v3-f16-review-sd", "v3-f17-scale-sc"]
    for n in OPS_NAMES:
        fam, world = F.parse(n)
        assert F.family_name(fam, world) == n


def contacts(path: Path) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    raw = tomllib.loads(path.read_text())
    return raw["contact"], raw.get("act") or []


def in_medium(world: str, c: dict[str, Any]) -> bool:
    pts = [tuple(p) for p in c.get("route") or [c["at"], c["at"]]]
    if c["domain"] == "surface":
        return F.wet(world, pts, 2.0)
    if c["domain"] == "subsurface":
        return F.wet(world, pts, -float(c["alt_m"]) + 1.0)
    if c["domain"] == "ground":
        return F.dry(world, pts)
    return float(c["alt_m"]) >= F.top_surface(world, pts, 100.0) + 50.0


@pytest.mark.parametrize("name", OPS_NAMES)
def test_each_family_draws_its_elements_in_their_medium(name: str, tmp_path: Path):
    fam, world = F.parse(name)
    for seed in SEEDS:
        e, path = F.generate(name, seed, tmp_path)
        assert e.opaque and e.map == world and e.duration_s == FO.DURATION_S
        cs, acts = contacts(path)
        mine = [c for c in cs if c["id"] in e.truth["added"]]
        assert len(mine) == len(e.truth["added"])
        for c in mine:
            assert in_medium(world if world != FO.SCALE else "golden-gate-v3", c), (seed, c["id"])
        opts = getattr(e, "options", None) or {}
        if fam == "f14-space":
            assert e.brief.startswith(mission_text(world)) and "Tasking." in e.brief
            (gap,) = e.truth["gap_needs"]
            (cov,) = e.truth["cover_needs"]
            assert F.dist_m(tuple(gap["center"]), v0(world).area.center) > 55_000.0
            assert FO.GAP_DUE_H[0] * 3600 <= gap["due_s"] <= FO.GAP_DUE_H[1] * 3600 and cov["min_level"] in (9, 10)
            assert F.dry(world, [tuple(p) for p in cov["route"]])
            kinds = [s["kind"] for s in e.script]
            assert "probe" in kinds and "flood" in kinds and not opts
        elif fam == "f15-engage":
            assert e.brief == mission_text(world)
            eng = e.truth["engagement"]
            assert eng["target"] == "hostile-1" and len(acts) == 3
            u = FO.uids(e.fleet)
            assert all(a["by"] == "hostile-1" and a["against_vehicle"] == u[eng["victim"]] for a in acts)
            assert {"VIPER 1", "BLADE 1", "WASP 1"} <= set(u) and set(e.truth["armed"]) == {"VIPER 1", "BLADE 1", "WASP 1"}
            base = FO.uids(load_world(world).fleet)
            assert all(u[k] == v for k, v in base.items()), "the world's uids unchanged"
            zone = [f for f in json.loads(next(load_world(world).areas.glob("*.geojson")).read_text())["features"]
                    if f["properties"].get("id") == eng["zone"]][0]["geometry"]["coordinates"][0]
            lon, lat = eng["hold"]
            assert min(p[0] for p in zone) < lon < max(p[0] for p in zone) and min(p[1] for p in zone) < lat < max(p[1] for p in zone)
            assert opts == {"signers": FO.F15_SIGNERS, "review": FO.F15_REVIEW, "effects": FO.F15_EFFECTS}
            if "escort-1" in e.truth["added"]:
                assert e.truth["restraint"][0]["affiliation"] in ("neutral", "suspect")
            if "trawler-back" in e.truth["added"]:
                assert e.truth["same_vessel"] == {"trawler-back": "trawler-dark"}
        elif fam == "f16-review":
            assert opts == {"review": FO.F16_REVIEW}
            for it in e.truth["review_items"]:
                if it["item"] == "ais_claim":
                    claimant = next(c for c in cs if c["id"] == it["claimant"])
                    assert claimant["ais"]["mmsi"] == next(c for c in cs if c["id"] == it["lost"])["ais"]["mmsi"]
                    assert bool(claimant["ais"].get("spoofed")) is not it["same_vessel"]
            assert {a["by"] for a in acts} <= {"raider-1", "skiff-1"}
        else:
            assert 8 <= len(mine) <= 12 and world == FO.SCALE
            hour = int(e.conditions["local_time"][:2])
            if not 8 <= hour < 17:
                assert not any(c["kind"] == "small_drone" for c in mine), "F2's drone by day only"


# ---- one smoke episode per family and world ---------------------------------------------------------

@pytest.fixture(scope="module")
def binary() -> None:
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    subprocess.run([cargo, "build", "--release", "-q", "-p", "murmur-headless"], cwd=REPO, check=True)


def smoke(name: str, agent: str, duration: float, tmp_path: Path) -> tuple[dict[str, Any], Any]:
    from eval.headless.batch import replicate_seed
    from eval.headless.situations import Calibration
    from eval.headless.v3.run import APPROVE_ALL, _episode

    job = {"family": name, "situation_seed": 5400, "r": 0, "seed": replicate_seed(name, 5400, 0), "agent": agent,
           "operator": APPROVE_ALL, "calibration": Calibration.load().as_dict()}
    orig = FO.DURATION_S
    FO.DURATION_S = duration
    try:
        rec = _episode(job, tmp_path)
    finally:
        FO.DURATION_S = orig
    return rec, L.episode_labels(rec)


@pytest.mark.parametrize("name", ["v3-f14-space-gg", "v3-f14-space-sd"])
def test_f14_smoke_a_collect_and_a_cover(name: str, tmp_path: Path, binary: None):
    rec, lab = smoke(name, "oracle-v3-basic", 300.0, tmp_path)
    assert rec["episode_class"] == "scored" and rec["agent_detail"]["errors"] == 0, rec["agent_log"][-3:]
    probe = next(s for s in rec["script"] if s["kind"] == "probe")
    assert probe["status"] == 200 and len(probe["answer"]["samples"]) == 4
    kinds = {i["kind"] for i in rec["v3"]["space"]["intents"]}
    in_time = rec["agent_detail"]["space_options"]["gap-1"]["in_time"]
    # a collect when a pass can deliver by the due time (else the oracle says none is in time)
    assert kinds == ({"collect", "cover"} if in_time else {"cover"})
    labels = {a["kind"]: a["label"] for a in lab["actions"] if a["kind"] in ("collect", "cover")}
    assert labels.get("collect", "unjudged") in ("unjudged", "useful", "late")
    assert labels["cover"] in ("useful", "justified", "not_justified")
    assert lab["ops"]["gap"][0]["need"] == "gap-1" and lab["ops"]["cover"][0]["asked"]


@pytest.mark.parametrize("name", ["v3-f15-engage-gg", "v3-f15-engage-sd"])
def test_f15_smoke_the_act_the_declaration_and_its_review(name: str, tmp_path: Path, binary: None):
    rec, lab = smoke(name, "none", 700.0, tmp_path)
    evs = [json.loads(x) for x in rec.get("audit_text", "").splitlines() if x.strip()]
    assert any(e["type"] == "ident.act" for e in evs), "our sensors observed an act"
    assert rec["review"]["policy"]["flags"] == "declare" and rec["signers"]["policy"]
    assert rec["effects"]["config"]["p_hit"] == FO.F15_EFFECTS["p_hit"]
    (e,) = lab["ops"]["engagement"]
    assert e["target"] == "hostile-1" and not e["effective"] and e["cause"] == "no_nomination" and e["restraint_ok"]


@pytest.mark.parametrize("name", ["v3-f16-review-gg", "v3-f16-review-sd"])
def test_f16_smoke_the_evidence_reviewer_decides(name: str, tmp_path: Path, binary: None):
    rec, lab = smoke(name, "none", 900.0, tmp_path)
    assert rec["review"]["policy"]["preset"] == "evidence"
    assert rec["review"]["decisions"], "the review list had items to decide"
    assert {r["label"] for r in lab["review"]} <= {"right", "wrong", "no_effect", "unjudged"}
    assert lab["ops"]["review"][0]["items"] == rec["truth"]["stated"]["review_items"]


def test_f17_smoke_a_hundred_nodes(tmp_path: Path, binary: None):
    rec, lab = smoke("v3-f17-scale-sc", "none", 240.0, tmp_path)
    assert rec["world"]["name"] == FO.SCALE and len(rec["v3"]["picture"]["fleet"]) == 100
    assert rec["episode_class"] == "scored"
