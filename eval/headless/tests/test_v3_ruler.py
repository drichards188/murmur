"""Scoring v3's ruler (eval/SCORING-V3.md 10 and 13): the ceiling's planning (v3/planner.py), each rung
and perturbation (v3/planner.py, v3/perturb.py), the room test (v3/room.py) and the report's tests
(v3/ruler.py), on stubs: no episode runs here (test_v3_families.py runs the ceiling in one short
episode per family and world)."""

from __future__ import annotations

import math
from types import SimpleNamespace
from typing import Any

import numpy as np
import pytest

from eval.headless.agents_v2 import Straight
from eval.headless.v3 import perturb as PT
from eval.headless.v3 import planner as P
from eval.headless.v3 import room as R
from eval.headless.v3 import ruler as RU


# ---- the look model ----------------------------------------------------------------------------------

def acquire(r60: float) -> np.ndarray:
    """ACQUIRE's curve with its 0.6 point at `r60` (about 0.4 at 1.27 times it)."""
    x = np.minimum(r60 / np.maximum(P.DISTANCES, 1.0) * 1.13, 20.0)
    e = 2.7 + 0.7 * x
    return x ** e / (1 + x ** e)


def quality(r60: float, cue_m: float, gimballed: bool = True, search_m: float | None = None) -> P.Quality:
    """An EO/IR table: identification 0.6 at `r60`; the wide field's detection 0.6 at `search_m`
    (by default where it identifies)."""
    return P.Quality("eo_ir", acquire(r60), cue_m, gimballed, search=acquire(search_m or r60))


def test_a_quality_table_reads_its_reach_and_interpolates():
    q = quality(2000.0, 4000.0)
    assert 1500.0 <= q.reach(0.6) <= 2100.0
    assert q.at(np.array([10.0]))[0] > 0.99 and q.at(np.array([1e6]))[0] == 0.0
    assert q.at(np.array([1000.0]))[0] > q.at(np.array([3000.0]))[0]


def closing(start_m: float, end_m: float, speed: float, dt: float = 2.0, hold_s: float = 60.0):
    n = int((start_m - end_m) / speed / dt) + 1
    r = np.concatenate([np.linspace(start_m, end_m, n), np.full(int(hold_s / dt), end_m)])
    ts = np.arange(len(r)) * dt
    return ts, r, np.zeros_like(r), np.ones(len(r), dtype=bool)


def test_a_camera_that_locks_far_from_its_0_6_range_tends_to_identify_early_and_low():
    # closing slowly: a camera that finds the contact in its wide field far out locks there and
    # identifies on the way in, mostly below 0.6; one that finds it only near identifies above
    ts, r, dc, ex = closing(6000.0, 300.0, 5.0)
    near = P.eo_look(1, quality(2000.0, 8000.0, search_m=800.0), ts, r, dc, 1e9, ex, 2.0)
    far = P.eo_look(1, quality(2000.0, 8000.0, search_m=6000.0), ts, r, dc, 1e9, ex, 2.0)
    assert near.high > 0.6 and near.low < 0.1, near.as_dict()
    assert far.low > near.low and far.high < near.high, far.as_dict()
    assert 0.0 < far.low_c < P.P60


def test_a_cue_that_finds_nothing_keeps_the_camera_off_for_a_minute():
    ts = np.arange(0.0, 200.0, 2.0)
    r = np.full(len(ts), 1000.0)
    ex = np.ones(len(ts), dtype=bool)
    q = P.Quality("eo_ir", np.full(len(P.DISTANCES), 0.9), 4000.0, True, search=np.full(len(P.DISTANCES), 0.3))
    lk = P.eo_look(1, q, ts, r, np.zeros(len(ts)), 1e9, ex, 2.0)
    # three 8 s cues in 200 s at 0.3^2 / 3.4 a second: about half the chance a steady stare gives
    stare = 1.0 - math.exp(-200.0 * 0.09 / 3.4)
    assert 0.2 * stare < lk.high / P.P_EXEC < 0.7 * stare


def test_a_camera_that_looks_all_round_keeps_its_surest_look():
    ts, r, dc, ex = closing(5000.0, 500.0, 10.0)
    lk = P.eo_look(1, quality(1500.0, 8000.0, gimballed=False), ts, r, dc, 1e9, ex, 2.0)
    assert lk.high > 0.7 and lk.low == 0.0
    ts, r, dc, ex = closing(5000.0, 2500.0, 10.0)
    lk = P.eo_look(1, quality(1500.0, 8000.0, gimballed=False), ts, r, dc, 1e9, ex, 2.0)
    assert lk.high == 0.0 and 0.0 < lk.low_c < P.P60


def test_a_camera_cues_only_inside_its_task_area():
    ts, r, _, ex = closing(3000.0, 200.0, 15.0)
    far_from_centre = np.full(len(r), 5000.0)
    assert P.eo_look(1, quality(2000.0, 4000.0), ts, r, far_from_centre, 2200.0, ex, 2.0).high == 0.0


def sonar(active_m: float, passive_m: float = 0.0) -> P.Quality:
    act = (P.DISTANCES <= active_m).astype(float)
    pas = (P.DISTANCES <= passive_m).astype(float)
    return P.Quality("sonar", np.maximum(act, pas), active_m, False, active=act, passive=pas)


def test_sonar_looks_need_their_minute_of_signal_and_two_actives_combine():
    r = np.full(60, 100.0)
    ex = np.ones(60, dtype=bool)
    short = P.sonar_look(1, sonar(300.0), r[:25], ex[:25], 2.0)
    assert short.conf == 0.0, "under 60 s of signal: no recognition"
    one = P.sonar_look(1, sonar(300.0), r, ex, 2.0)
    assert 0.3 < one.conf < 0.6
    assert P.p_resolved([one]) < 0.5
    assert P.p_resolved([one, P.sonar_look(2, sonar(300.0), r, ex, 2.0)]) > 0.5
    listening = P.sonar_look(3, sonar(300.0, passive_m=300.0), np.full(120, 50.0), np.ones(120, dtype=bool), 2.0)
    assert listening.conf > one.conf, "a sonar that also listens reaches the passive mode's 0.8"


def test_two_lower_identifications_combine_to_0_6():
    hi = P.Look(1, "eo", high=0.8)
    lo = P.Look(2, "eo", low=0.9, low_c=0.45)
    assert P.p_resolved([hi]) == pytest.approx(0.8)
    assert P.p_resolved([lo]) == 0.0
    assert P.p_resolved([lo, P.Look(3, "eo", low=0.9, low_c=0.45)]) == pytest.approx(0.81)
    assert P.p_resolved([lo, P.Look(3, "eo", low=0.9, low_c=0.2)]) == 0.0
    assert P.p_resolved([hi, hi]) == pytest.approx(0.96)


# ---- the ceiling's plan on stubs ----------------------------------------------------------------------

class Stub(P.Ceiling):
    """A ceiling with no episode: units and needs set by hand, the model's tables injected, every
    check passing unless a test says otherwise."""

    def __init__(self, units: list[P.UnitV3], needs: list[P.NeedV3], tables: dict[tuple[str, str], P.Quality]):
        self.world_name, self.origin, self.radius, self.at0 = "golden-gate-v3", (-122.4783, 37.8199), 3000.0, 0.0
        self.units = {u.uid: u for u in units}
        self.needs = {n.id: n for n in needs}
        self.tables = tables
        self.quality = {}
        self.cond = {}
        self.lines, self.calls, self.proposals, self.plans, self.errors = [], {}, 0, [], 0
        self.privileged = {"truth_reads": 0, "sense_asks": 0}
        self.now = 0.0
        self.blocked: set[int] = set()
        self.unseen: set[int] = set()
        self.proposed: list[dict[str, Any]] = []

    def clock(self) -> float:
        return self.now

    def quality_of(self, u, n, cond=None, key=""):
        if P.domain_sensor(n.domain) not in u.sensors:
            return None
        if not self.KNOW_SENSORS:
            return super().quality_of(u, n, cond, key)
        return self.tables.get((u.platform, n.id + key))

    def medium_ok(self, u, xy):
        return True

    def link_holds(self, u, centre):
        return (not self.KNOW_LINK) or u.uid not in self.blocked

    def sees_from(self, u, centre, n, t):
        return u.uid not in self.unseen

    def light_at(self, n):
        return "night"

    def call(self, tool, **args):
        return {"passed": True}

    def propose(self, rationale, req, **why):
        self.proposed.append({"req": req, **why})
        return {"id": len(self.proposed), "state": "dispatched"}


def unit(uid: int, platform: str, xy: tuple[float, float], cruise: float = 20.0, sensors=("eo_ir",), link="satcom",
         domain="air", range_m: float = 4000.0) -> P.UnitV3:
    cat = {"sensors": [{"type": s, "range_m": range_m, "slew_dps": 60, "channels": [{"band": "day"}]} for s in sensors],
           "endurance_h": 2.0, "tasks": ["orbit"], "domain": domain}
    u = P.UnitV3(uid, f"U{uid}", platform, cat, 150.0, cruise, 200.0, link, domain)
    u.xy, u.home_xy, u.battery = xy, xy, 95.0
    return u


def boat_need(t_need: float = 300.0) -> P.NeedV3:
    path = Straight("boat-1")
    path.update((-6000.0, 0.0), (15.0, 0.0), 0.0)
    return P.NeedV3("boat-1", "small_fast_boat", "surface", path, t_need, 0.0, 15.0)


def test_the_plan_sends_the_vehicles_that_can_identify_soonest_and_no_more_than_it_needs():
    n = boat_need()
    near, far, blind = unit(1, "puma", (-3000.0, 1000.0)), unit(2, "puma", (8000.0, 8000.0)), unit(3, "x10", (-3000.0, 0.0))
    st = Stub([near, far, blind], [n], {("puma", "boat-1"): quality(2000.0, 1800.0),
                                        ("x10", "boat-1"): P.Quality("eo_ir", np.zeros(len(P.DISTANCES)), 1500.0, True)})
    plan = st.plan(10.0)
    uids = [o["uid"] for o in plan]
    assert uids[0] == 1, "the nearer capable vehicle first"
    assert 3 not in uids, "a vehicle whose sensor cannot reach 0.6 adds nothing"
    assert len(uids) <= P.Ceiling.MAX_LOOKS
    assert all(o["look"].kind == "eo" for o in plan)


def test_the_plan_keeps_off_a_centre_where_the_link_fails_and_the_linkblind_rung_does_not():
    n = boat_need()
    u = unit(1, "puma", (-3000.0, 1000.0), link="los")
    tables = {("puma", "boat-1"): quality(2000.0, 1800.0)}
    st = Stub([u], [n], tables)
    st.blocked = {1}
    assert st.plan(10.0) == []

    class Blind(Stub, P.CeilingLinkBlind):
        pass

    b = Blind([unit(1, "puma", (-3000.0, 1000.0), link="los")], [boat_need()], tables)
    b.blocked = {1}
    assert [o["uid"] for o in b.plan(10.0)] == [1]


def test_the_late_rung_waits_for_the_need_to_open():
    class Late(Stub, P.CeilingLate):
        pass

    n = boat_need()
    st = Late([unit(1, "puma", (-3000.0, 1000.0))], [n], {("puma", "boat-1"): quality(2000.0, 1800.0)})
    assert st.plan(10.0) == []
    n.opened_t = 300.0
    assert [o["uid"] for o in st.plan(300.0)] == [1]


def test_the_sensorblind_rung_believes_the_catalog_range_on_every_kind():
    class SB(Stub, P.CeilingSensorBlind):
        pass

    n = boat_need()
    u = unit(1, "x10", (-3000.0, 0.0), range_m=1500.0)
    st = SB([u], [n], {("x10", "boat-1"): P.Quality("eo_ir", np.zeros(len(P.DISTANCES)), 1500.0, True)})
    q = st.quality_of(u, n)
    assert q.nominal and q.reach(0.6) == pytest.approx(P.DISTANCES[P.DISTANCES <= 1500.0][-1])
    assert [o["uid"] for o in st.plan(10.0)] == [1], "it sends what the model says cannot identify"


def test_feasibility_names_why_no_plan_could_serve_a_need():
    n = boat_need()
    zero = P.Quality("eo_ir", np.zeros(len(P.DISTANCES)), 1500.0, True)

    class F(Stub):
        def window(self, n, now):
            return super().window(n, now)

    tables = {("puma", "boat-1"): zero, ("puma", "boat-1@clear"): quality(2000.0, 1800.0)}
    st = F([unit(1, "puma", (-3000.0, 1000.0))], [n], tables)
    assert st.feasibility(n, 10.0)["verdict"] == "night"
    st = F([unit(1, "puma", (-3000.0, 1000.0))], [boat_need()], {("puma", "boat-1"): zero, ("puma", "boat-1@clear"): zero})
    assert st.feasibility(st.needs["boat-1"], 10.0)["verdict"] == "fleet_gap"
    far = unit(1, "puma", (60000.0, 60000.0), cruise=5.0)
    st = F([far], [boat_need()], {("puma", "boat-1"): quality(2000.0, 1800.0)})
    assert st.feasibility(st.needs["boat-1"], 10.0)["verdict"] == "no_time"
    st = F([unit(1, "puma", (-3000.0, 1000.0))], [boat_need()], {("puma", "boat-1"): quality(2000.0, 1800.0)})
    st.unseen = {1}
    assert st.feasibility(st.needs["boat-1"], 10.0)["verdict"] == "terrain"
    st.unseen = set()
    assert st.feasibility(st.needs["boat-1"], 10.0)["verdict"] == "feasible"


def test_one_active_sonar_in_time_is_not_enough_and_the_plan_sends_two():
    path = Straight("uuv-1")
    path.update((-800.0, 0.0), (1.0, 0.0), 0.0)
    n = P.NeedV3("uuv-1", "uuv", "subsurface", path, 100.0, -10.0, 1.0)
    near = unit(1, "remus", (-700.0, 100.0), cruise=2.0, sensors=("sonar",), domain="subsurface")
    far = unit(2, "remus", (40000.0, 0.0), cruise=2.0, sensors=("sonar",), domain="subsurface")
    tables = {("remus", "uuv-1"): sonar(250.0)}
    st = Stub([near, far], [n], tables)
    f = st.feasibility(n, 10.0)
    assert f["verdict"] == "no_time" and f["timely"] == ["remus"], f
    other = unit(2, "remus", (-600.0, -100.0), cruise=2.0, sensors=("sonar",), domain="subsurface")
    st = Stub([near, other], [n], tables)
    assert st.feasibility(n, 10.0)["verdict"] == "feasible"
    assert sorted(o["uid"] for o in st.plan(10.0)) == [1, 2], "the first look alone resolves nothing; two do"


def test_the_reserve_keeps_a_low_vehicle_out_and_the_reserve_blind_one_does_not_care():
    n = boat_need()
    u = unit(1, "puma", (-3000.0, 1000.0))
    u.battery = 22.0
    st = Stub([u], [n], {("puma", "boat-1"): quality(2000.0, 1800.0)})
    assert not st.reserve_ok(u, (0.0, 0.0), 10.0, n.deadline)
    rb = PT.ReserveBlind.__new__(PT.ReserveBlind)
    rb.RESERVE_AWARE = False
    assert P.Ceiling.reserve_ok(rb, u, (0.0, 0.0), 10.0, n.deadline)


# ---- the honest ceiling and its perturbations on stubs ------------------------------------------------

def honest(cls: type, units: list[P.UnitV3], needs: list[P.NeedV3], tables: dict[tuple[str, str], P.Quality]) -> Any:
    """A perturbation of the honest ceiling on the stub's checks (the stub first in the order)."""
    k = type(f"T{cls.__name__}", (Stub, cls), {})
    obj = k(units, needs, tables)
    obj.shown, obj.shown_before, obj.changed = {}, {}, []
    return obj


def test_spam_proposes_each_held_look_again_every_20_s():
    n = boat_need()
    u = unit(1, "puma", (0.0, 0.0))
    st = honest(PT.Spam, [u], [n], {})
    u.order = {"intent": 7, "need": n.id, "t": 0.0, "centre": [0.0, 0.0], "req": {"task": "orbit"}, "text": "look"}
    st.now = 10.0
    st.after_plan(10.0, [])
    assert st.proposed == []
    st.now = 20.0
    st.after_plan(20.0, [])
    assert len(st.proposed) == 1 and st.proposed[0]["again"] == 7 and u.order["intent"] == 1


def test_behind_moves_the_centre_back_along_the_shown_course():
    n = boat_need()
    u = unit(1, "puma", (0.0, 0.0))
    st = honest(PT.Behind, [u], [n], {("puma", "boat-1"): quality(2000.0, 1800.0)})
    out = st.place(u, n, (1000.0, 0.0), 10.0)
    r = 1.5 * st.quality_of(u, n).reach(0.6)
    assert out[0] == pytest.approx(1000.0 - r) and out[1] == pytest.approx(0.0)


def test_skip_first_hides_the_earliest_need_s_track():
    st = PT.SkipFirst.__new__(PT.SkipFirst)
    st.first, st.hidden, st.changed, st.lines, st.calls, st.now = "boat-1", set(), [], [], {}, 0.0
    st.privileged = {"truth_reads": 0}
    st.ep = SimpleNamespace(sim=SimpleNamespace(truth=lambda association: {"association": [
        {"contact_id": "boat-1", "track_id": "T-1"}, {"contact_id": "ferry", "track_id": "T-2"}]}))
    st.session = None
    st._run = lambda coro: {"tracks": [{"id": "T-1"}, {"id": "T-2"}], "count": 2}
    st.run = st._run
    import eval.headless.agents_v2 as A
    orig = A.SessionAgent.call
    try:
        A.SessionAgent.call = lambda self, tool, **args: {"tracks": [{"id": "T-1"}, {"id": "T-2"}], "count": 2}
        out = st.call("get_tracks")
    finally:
        A.SessionAgent.call = orig
    assert [t["id"] for t in out["tracks"]] == ["T-2"] and st.hidden == {"T-1"}


def test_ferries_look_at_a_neutral_with_ais_and_let_it_go():
    u = unit(1, "puma", (0.0, 0.0))
    st = honest(PT.Ferries, [u], [], {})
    st.looked = set()
    st.shown = {"T-9": {"id": "T-9", "affiliation": "neutral", "identity": {"ais": {"mmsi": "1"}}, "position": [-122.47, 37.82]}}
    st.after_plan(10.0, [])
    assert u.order is not None and u.order["need"] == "ferry:T-9"
    st.now = 101.0
    st.after_plan(101.0, [])
    assert u.order is None and st.proposed[-1]["req"]["task"] == "hold"


def test_pull_off_takes_a_vehicle_off_a_need_its_sensor_is_raising_and_gives_the_need_up():
    n = boat_need()
    n.opened_t = 5.0
    u = unit(1, "puma", (-1000.0, 0.0))
    st = honest(PT.PullOff, [u], [n], {("puma", "boat-1"): quality(2000.0, 1800.0)})
    st.abandoned = set()
    st.shown_before = {n.id: {"sources": [{"uid": 1, "sensor": "eo_ir", "hits": 3}]}}
    st.shown = {n.id: {"sources": [{"uid": 1, "sensor": "eo_ir", "hits": 5}]}}
    st.after_plan(10.0, [])
    assert n.id in st.abandoned and not st.may_look(n, 10.0)
    c = u.order["centre"]
    assert 2400.0 <= math.hypot(c[0] - u.xy[0], c[1] - u.xy[1]) <= 4100.0


def test_wrong_sensor_sends_a_vehicle_that_cannot_identify_the_domain():
    n = boat_need()
    right = unit(1, "puma", (-3000.0, 1000.0))
    wrong = unit(2, "remus", (-2000.0, -500.0), sensors=("sonar",), domain="subsurface", cruise=2.0)
    st = honest(PT.WrongSensor, [right, wrong], [n], {("puma", "boat-1"): quality(2000.0, 1800.0)})
    plan = st.plan(10.0)
    assert plan and all(o["uid"] == 2 for o in plan)
    assert st.wrong(wrong, n) and not st.wrong(right, n)


def test_every_agent_name_is_dispatched():
    from eval.headless.episode import V3_AGENTS
    from eval.headless.v3 import oracle as O

    sim = SimpleNamespace(tools=[], rpc=lambda req: {"predictions": []})
    ep = SimpleNamespace(sim=sim, aloop=SimpleNamespace(run_until_complete=lambda c: None),
                         e=SimpleNamespace(map="golden-gate-v3", at="2026-06-14T10:00:00Z", duration_s=900.0,
                                           conditions={}, contacts=None, fleet=None),
                         contacts_path=None, opts=SimpleNamespace(calibration=SimpleNamespace(contacts_start_s=3.0)))
    for name in V3_AGENTS:
        if name == "oracle-v3-basic":
            continue
        a = O.make(ep, name)
        assert a.name == name
    assert set(P.NAMES) | set(PT.NAMES) | {"oracle-v3-basic"} == set(V3_AGENTS)


# ---- the room test and the report's tests ------------------------------------------------------------

def scored(family: str, rows: dict[str, list[int]]) -> list[dict[str, Any]]:
    out = []
    for agent, served in rows.items():
        for seed, s in enumerate(served):
            out.append({"family": family, "agent": agent, "seed": seed,
                        "needs": [{"contact": "c", "eligible": True, "served": bool(s)}]})
    return out


def test_the_room_test_needs_an_interval_above_zero_and_the_minimum_room():
    big = scored("v3-f1-fast-boat-gg", {"oracle-v3": [1] * 30, "none": [0] * 15 + [1] * 15})
    assert R.room(big, "v3-f1-fast-boat-gg")["verdict"] == "room"
    tie = scored("v3-f1-fast-boat-gg", {"oracle-v3": [1, 0] * 15, "none": [0, 1] * 15})
    assert R.room(tie, "v3-f1-fast-boat-gg")["verdict"] == "none shown"
    small = scored("v3-f1-fast-boat-gg", {"oracle-v3": [1] * 100, "none": [0] * 6 + [1] * 94})
    r = R.room(small, "v3-f1-fast-boat-gg")
    assert r["lo"] > 0 and r["verdict"] == "small", r
    rev = scored("v3-f1-fast-boat-gg", {"oracle-v3": [0] * 30, "none": [1] * 30})
    assert R.room(rev, "v3-f1-fast-boat-gg")["verdict"] == "reversed"


def test_the_one_sided_test_and_the_verdicts():
    assert RU.one_sided_p([-1.0] * 10) == pytest.approx(1 / 1024)
    assert RU.one_sided_p([1.0] * 10) == pytest.approx(1.0)
    assert RU.move_verdict({"mean_difference": -0.1, "lo": -0.2, "hi": -0.05}, "down") == "moved"
    assert RU.move_verdict({"mean_difference": 0.1, "lo": 0.05, "hi": 0.2}, "down") == "wrong way"
    assert RU.move_verdict({"mean_difference": -0.01, "lo": -0.04, "hi": 0.02}, "not worse") == "not worse"
    assert RU.move_verdict({"mean_difference": -0.06, "lo": -0.08, "hi": -0.04}, "not worse") == "not shown not worse"
    assert RU.ladder_verdict({"lo": -0.2, "hi": -0.01}) == "fails"
    assert RU.leans_wrong(-0.01, "up") and RU.leans_wrong(0.01, "down") and not RU.leans_wrong(-0.04, "not worse")


def test_the_audit_lists_the_cells_that_lean_the_wrong_way_in_both_batches():
    def rep(d_room: float, d_move: float) -> dict[str, Any]:
        return {"room": {"v3-f1-fast-boat-gg": {"mean_difference": d_room}},
                "ladder": {}, "moves": {"oracle-v3-honest+behind": [{"metric": "served_share", "want": "down",
                                                                      "mean_difference": d_move, "per_family": {}}]}}
    a = RU.audit(rep(-0.02, 0.01), rep(-0.05, -0.03))
    assert [x["cell"] for x in a["wrong_way_both"]] == ["room v3-f1-fast-boat-gg"]
    assert a["wrong_way_once"] == 1
