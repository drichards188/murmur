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
from eval.headless.tests.test_v2_labels import EPOCH, Synth, at, inbound
from eval.headless.tests.test_v3_labels import UNRESOLVED, by_intent, v3_record
from eval.headless.v3 import families as F
from eval.headless.v3 import families_ops as FO
from eval.headless.v3 import labels as L
from eval.headless.v3 import labels_ops as O
from eval.headless.v3.mission import mission_text
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


def test_a_collect_is_useful_late_redundant_or_useless():
    lab = collect_world(5000.0)
    assert by_intent(lab)[20]["label"] == "useful" and lab["ops"]["gap"][0]["served"]
    assert lab["ops"]["gap"][0]["by"] == "collect"
    assert by_intent(collect_world(7000.0))[20]["label"] == "late"
    assert not collect_world(7000.0)["ops"]["gap"][0]["served"]
    assert by_intent(collect_world(5000.0, none_cover={"gap-1": 900.0}))[20]["label"] == "redundant"
    assert by_intent(collect_world(5000.0, near_s=50.0))[20]["label"] == "redundant", "ours covered it before"
    assert by_intent(collect_world(5000.0, area_center=[-122.0, 36.0]))[20]["label"] == "useless", "no need's area"
    assert by_intent(collect_world(None))[20]["label"] == "useless", "never tasked"
    assert by_intent(collect_world(5000.0, none_cover=None))[20]["label"] == "unjudged", "no none replicate"
    served_by_ours = collect_world(None, near_s=300.0)["ops"]["gap"][0]
    assert served_by_ours["served"] and served_by_ours["by"] == "ours"


NEED = {"id": "gnss-1", "uid": 18, "callsign": "FERRET 1", "route": [[-122.46, 37.8085], [-122.448, 37.8082]],
        "start_s": 400.0, "end_s": 1300.0, "min_level": 10, "service": {"type": "gnss", "class": "gps"},
        "area": {"kind": "circle", "center": [-122.454, 37.8083], "radius_km": 0.8}}


def cover_world(coverage: str, held_at: float | None, outage_at: float = 600.0, other_fail: bool = False,
                stated: bool = True, created: float = 100.0) -> dict[str, Any]:
    s = Synth(900)
    s.contact("c", aff="neutral", path=inbound())
    s.intent(30, "cover", [], created, {"area": NEED["area"], "service": NEED["service"], "min_level": 10.0},
             states=[(created, "checked", "check"), (created, "rejected" if coverage == "fail" else "dispatched", "x")])
    s.intent(31, "patrol", [18], 400.0, {"route": NEED["route"]}, source="agent-scripted",
             states=[(400.0, "dispatched", "x")], op={18: [(401.0, "acked"), (402.0, "active")]})
    if held_at is not None:
        s.intent(32, "hold", [18], held_at, {}, states=[(held_at, "dispatched", "x")],
                 op={18: [(held_at + 1.0, "acked"), (held_at + 2.0, "done")]})
    rec = v3_record(s)
    rec["truth"]["stated"] = {"cover_needs": [NEED]} if stated else {}
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
    lab = cover_world("fail", held_at=None)
    assert by_intent(lab)[30]["label"] == "useless" and lab["ops"]["cover"][0]["cause"] == "run_during_outage"
    assert by_intent(cover_world("pass", held_at=None))[30]["label"] == "useless", "no outage predicted"
    assert by_intent(cover_world("fail", held_at=None, stated=False))[30]["label"] == "unjudged"
    assert by_intent(cover_world("fail", held_at=None, other_fail=True))[30]["label"] == "refused"
    late = cover_world("fail", held_at=550.0, created=450.0)["ops"]["cover"][0]
    assert not late["served"] and late["cause"] == "not_asked", "asked after the run began"


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


# ---- the evidence reviewer (review.py) ---------------------------------------------------------------

def track(tid: str, kind: str | None, conf: float = 0.9, sb: tuple[str, ...] = ("eo_ir",), conflict: Any = None,
          ais: dict[str, Any] | None = None) -> dict[str, Any]:
    return {"id": tid, "identity": {"kind": kind, "confidence": conf, "supported_by": list(sb), "conflict": conflict,
                                    "ais": ais}}


def test_the_evidence_reviewer_judges_on_what_the_picture_shows():
    j = rv.judge
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
            assert F.dist_m(tuple(gap["center"]), load_world(world).mission.center) > 55_000.0
            assert 2.5 * 3600 <= gap["due_s"] <= 6 * 3600 and cov["min_level"] in (9, 10)
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
    rec, lab = smoke(name, "oracle-v3", 300.0, tmp_path)
    assert rec["episode_class"] == "scored" and rec["agent_detail"]["errors"] == 0, rec["agent_log"][-3:]
    probe = next(s for s in rec["script"] if s["kind"] == "probe")
    assert probe["status"] == 200 and len(probe["answer"]["samples"]) == 4
    kinds = {i["kind"] for i in rec["v3"]["space"]["intents"]}
    in_time = rec["agent_detail"]["space_options"]["gap-1"]["in_time"]
    # a collect when a pass can deliver by the due time (else the oracle says none is in time)
    assert kinds == ({"collect", "cover"} if in_time else {"cover"})
    labels = {a["kind"]: a["label"] for a in lab["actions"] if a["kind"] in ("collect", "cover")}
    assert labels.get("collect", "unjudged") in ("unjudged", "useful", "late") and labels["cover"] in ("useful", "useless")
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
