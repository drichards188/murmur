"""The playbook engine's fix round 2 (a second review of murmur-t8m): mostly side effects of round
1. Each item the reviewer ran is a test here that failed before its fix."""

from __future__ import annotations

import asyncio

from playbook import rationale as R
from playbook.arbiter import arbitrate
from playbook.beliefs import CLASSIFYING, build
from playbook.engine import Engine
from playbook.ledger import Ledger, vehicle_key
from playbook.plays import PLAYS
from playbook import tier
from playbook_world import (PLATFORMS, FakeSession, T0, config, intent, offset, passing, reads, track, vehicle)

AUTO = passing("auto_approved", "auto", "agent-auto")
HUMAN = passing()


def run(ticks, previews=None, cfg=None, intents=None, ledger=None, session=None, ledger_path=None, shadow=False):
    s = session or FakeSession(ticks, previews, intents)
    lines: list[dict] = []
    if ledger is not None and ledger.run is None:
        ledger.run = s.run
    e = Engine(cfg or config(), ledger, shadow=shadow, ledger_path=ledger_path,
               log=lambda kind, **f: lines.append({"kind": kind, **f}))

    async def go():
        await e.start(s)
        return [await e.tick(s) for _ in ticks]

    return s, e, asyncio.run(go()), lines


def text(rationale: str) -> str:
    return R.split(rationale)[0]


def states(n: int, **by_tick):
    out, now = {}, {}
    for i in range(n):
        now.update(by_tick.get(f"t{i}", {}))
        out[i] = dict(now)
    return out


# ---- 1: a P0 command on an engaging platform goes to a person ------------------------------------------------


def test_r2_1_low_reserve_on_an_engaging_platform_goes_out_as_a_human_card_naming_the_engagement():
    """The bridge raises no battery alert of its own (decision/alerts.rs), so a withheld rtl
    reached nobody. Now it is proposed; the bridge routes an agent's order on an engaging platform
    to a person, who must acknowledge it stops the engagement (policy.rs, engagement in play)."""
    eng = [intent(50, "engage", [7], source={"kind": "agent", "id": "agent-watch"})]
    v7 = vehicle(7, "BLADE 1", "switchblade", offset(100, 100), battery_pct=12.0, mode="AUTO")
    ticks = [reads(t=T0, vehicles=[v7], pending=eng)]
    s, e, reps, lines = run(ticks, passing("pending_approval", "human", "(engagement in play)"))
    assert [(p["request"]["task"], p["request"]["uids"]) for p in s.proposed] == [("rtl", [7])]
    assert "An engagement is in play on this platform (uid 7: nomination i50 pending)" in text(s.proposed[0]["rationale"])
    assert e.led.bucket == {}, "P0: no token, even as a human card"


# ---- 2: a card the budget holds does not fill the preview cap ------------------------------------------------


def test_r2_2_a_budget_held_card_is_not_previewed_every_tick_and_an_auto_filler_still_goes():
    """The reviewer's sim P: five human-tier looks the budget held were previewed every tick, filled
    the cap of three, and the auto-tier gap filler was never proposed (90 previews in 30 ticks)."""
    station = offset(500, 900)
    tracks = [track(f"T-{i}", "suspect", 0.7, pos=offset(-1500 - 100 * i, -200 * i), vel=(10.0, 0.0)) for i in range(1, 6)]
    pumas = [vehicle(u, f"PUMA {u}", "puma", offset(-1000 - 100 * u, 300)) for u in (3, 5, 6, 7, 8, 9)]
    lost = vehicle(4, "PUMA 2", "puma", station, mode="AUTO", link_lost=True, link_age_s=30.0,
                   task={"intent_id": 3, "task": "orbit", "state": "done"})
    alerts = [{"id": "link:4", "kind": "lost_link", "uid": 4, "since_t": T0 - 30, "detail": "no telemetry"}]
    lf = {4: {"uid": 4, "link": {"lost_after_s": 3.0}}}
    ticks = [reads(t=T0 + 2 * k, vehicles=[lost] + pumas, tracks=tracks, alerts=alerts, lost_full=lf) for k in range(30)]
    led = Ledger()
    led.params["3"] = {"task": "orbit", "params": {"center": station, "radius_m": 300.0}, "source": "human op-setup", "play": None}

    def by_center(req):
        c = req.get("center")
        return AUTO if c and abs(c[0] - station[0]) < 1e-6 else HUMAN

    s, e, reps, lines = run(ticks, by_center, ledger=led)
    filled = [i for i, r in enumerate(reps) if any(p["request"].get("center") == station for p in r.proposed)]
    assert filled and filled[0] <= 4, filled
    previews = sum(1 for n, _ in s.calls if n == "preview_intent")
    assert previews <= 15, previews
    assert "previewed again when i101 or i102 is decided" in reps[10].statuses["isr.investigate"]


def test_r2_2_a_card_held_for_a_token_is_previewed_again_when_the_token_is_due():
    tracks = [track(f"T-{i}", "suspect", 0.7, pos=offset(-1500 - 100 * i, -200 * i), vel=(10.0, 0.0)) for i in (1, 2, 3)]
    pumas = [vehicle(u, f"PUMA {u}", "puma", offset(-1000 - 100 * u, 300)) for u in (3, 5, 6, 7)]
    ticks = [reads(t=T0 + dt, vehicles=pumas, tracks=tracks) for dt in (0, 2, 20, 31)]
    s, e, reps, _ = run(ticks, cfg=config(budget={"own_pending_max": 5}))
    assert [len(r.proposed) for r in reps] == [2, 0, 0, 1]
    assert [r.previews for r in reps] == [3, 0, 0, 1], "held until the next token (30 s after the burst)"


# ---- 3: a failed order's vehicle back-off never holds back P0 -------------------------------------------------


def test_r2_3_a_vehicle_back_off_from_a_failed_look_does_not_hold_back_a_p0_rtl():
    """The reviewer's sim Q: PUMA 1's look failed, and its battery under the line waited out the
    vehicle's back-off. A P0 command's own key still backs off when that command fails."""
    def puma(batt, task=None):
        return [vehicle(3, "PUMA 1", "puma", offset(-1200, 200), battery_pct=batt, task=task)]

    failed = {"intent_id": 101, "task": "orbit", "state": "failed"}
    ticks = [reads(t=T0, vehicles=puma(90.0), tracks=[track("T-1", "suspect", 0.7)]),
             reads(t=T0 + 2, vehicles=puma(22.0, failed)), reads(t=T0 + 4, vehicles=puma(21.0, failed))]
    s = FakeSession(ticks, None, states(3, t1={101: "failed"}))
    s.reasons = {101: "vehicle 3: failed (mission upload unconfirmed)"}
    s, e, reps, _ = run(ticks, session=s)
    assert [(p["id"], p["request"]["task"]) for p in s.proposed] == [(101, "orbit"), (102, "rtl")]
    assert e.led.backoff_for(vehicle_key(3), T0 + 3) is not None, "the vehicle still backs off for other plays"
    led = Ledger()
    b = build(reads(vehicles=puma(22.0)), config(), led)
    c = PLAYS["safety.low_reserve"].evaluate(b, led).candidates
    led.backoff[c[0].key] = {"fails": 1, "until_t": T0 + 10, "why": "i102 failed: x"}
    sel, held = arbitrate(c, b, led)
    assert sel == [] and held[0].reason.startswith("backing off"), "its own failed command does back off"


# ---- 4: a withdrawal for the vehicle's sake cools only that vehicle ---------------------------------------------


def test_r2_4_a_link_lost_withdrawal_cools_that_vehicle_not_the_look_on_every_vehicle():
    """The reviewer's sim R: PUMA 3's link dropped while our look waited for it; the withdrawal
    then held the look off PUMA 1 too for 30 s. A withdrawal because the premise went (hygiene)
    still cools the pair (round 1, item 9)."""
    fleet0 = [vehicle(3, "PUMA 1", "puma", offset(-1200, 200)), vehicle(5, "PUMA 3", "puma", offset(-1400, 300))]
    fleet1 = [vehicle(3, "PUMA 1", "puma", offset(-1200, 200)),
              vehicle(5, "PUMA 3", "puma", offset(-1400, 300), link_lost=True, link_age_s=4.0)]
    al = [{"id": "link:5", "kind": "lost_link", "uid": 5, "since_t": T0 - 2}]
    sus = track("T-1", "suspect", 0.7)
    ticks = [reads(t=T0, vehicles=fleet0, tracks=[sus])] + [
        reads(t=T0 + 2 * k, vehicles=fleet1, tracks=[sus], alerts=al, lost_full={5: {"uid": 5, "link": {"lost_after_s": 3.0}}})
        for k in range(1, 6)]
    s, e, reps, _ = run(ticks)
    assert [w["id"] for w in s.withdrawn] == [101]
    assert [(p["id"], p["request"]["uids"]) for p in s.proposed] == [(101, [5]), (102, [3])]
    assert [i for i, r in enumerate(reps) if r.proposed] == [0, 2]
    cd = e.led.cooldown_for("isr.investigate", "T-1", 5, T0 + 5)
    assert cd and cd["scope"] == "isr.investigate|T-1|5" and cd["why"] == "withdrawn"


# ---- 5: the ledger verified against the bridge, not discarded by its run --------------------------------------


def ours(iid, kind, uids):
    return {"id": iid, "kind": kind, "uids": [str(u) for u in uids], "source": {"kind": "agent", "id": "agent-playbook"}}


def old_ledger(run, cursor):
    led = Ledger(run=run)
    led.record(5, play="link.cover_gap", version="1", target="gap:4", uid=3, task="orbit",
               request={"task": "orbit", "uids": [3], "center": offset(500, 900), "radius_m": 300.0}, t=T0 - 60,
               state="dispatched", basis={}, withdraw_if=[])
    led.cooldowns.append({"scope": "isr.investigate|T-0008", "until_t": T0 + 500, "why": "rejected", "set_t": T0 - 100,
                          "intent": 4, "by": "op-1", "note": None, "basis": {}})
    led.cursor = cursor
    led.vehicles = {"3": {"platform": "puma", "callsign": "PUMA 1"}}
    led.backoff[vehicle_key(3)] = {"fails": 1, "until_t": T0 + 100, "why": "i9 failed: x"}
    return led


def test_r2_5_a_resumed_run_keeps_its_verified_cards_and_drops_its_track_cooldowns(tmp_path):
    """MURMUR_RESUME: a new run (another `run`) where intent ids stay unique: i5 is still ours, so
    it is kept (round 1 discarded the whole ledger); the bridge restarted, so the cooldown on
    T-0008 is dropped (a new run's T-0008 is another contact), said in the panel."""
    p = tmp_path / "playbook-ledger.json"
    v3 = vehicle(3, "PUMA 1", "puma", offset(500, 900), task={"intent_id": 5, "task": "orbit", "state": "done"})
    ticks = [reads(t=T0, vehicles=[v3]), reads(t=T0 + 2, vehicles=[v3])]
    s = FakeSession(ticks, run="run-B", feed="feedB")
    s.explained = {5: ours(5, "orbit", [3])}
    s, e, reps, lines = run(ticks, ledger=old_ledger("run-A", "feedA:40"), session=s, ledger_path=p)
    assert "5" in e.led.entries, "verified: the same intent, ours, the same task and vehicle"
    assert e.led.cooldowns == [] and e.led.backoff_for(vehicle_key(3), T0 + 50) is not None
    v = [x["text"] for x in lines if x["kind"] == "ledger.verified"]
    assert len(v) == 1 and "1 track cooldown(s) and back-off(s) dropped" in v[0] and "this run's T-0008" in v[0]
    assert (tmp_path / "playbook-ledger.run-A.json").exists(), "the ledger as it was, beside"


def test_r2_5_a_fixed_run_id_over_a_restart_does_not_keep_cards_whose_ids_were_reused():
    """MURMUR_RUN_ID fixed, a restart without resume: `run` is the same, the ids start again. The
    feed id says it restarted, and explain_intent says i5 is someone else's now."""
    v3 = vehicle(3, "PUMA 1", "puma", offset(500, 900))
    ticks = [reads(t=T0, vehicles=[v3]), reads(t=T0 + 2, vehicles=[v3])]
    s = FakeSession(ticks, run="fixed", feed="feed2")
    s.explained = {5: {"id": 5, "kind": "patrol", "uids": ["1"], "source": {"kind": "human", "id": "op-setup"}}}
    s, e, reps, lines = run(ticks, ledger=old_ledger("fixed", "feed1:40"), session=s)
    assert e.led.entries == {} and e.led.cooldowns == []
    assert "i5" in [x["text"] for x in lines if x["kind"] == "ledger.verified"][0]


def test_r2_5_memory_per_vehicle_is_kept_only_for_the_same_vehicle_under_the_uid():
    led = old_ledger("run-A", "f:0")
    led.stuck_blocks.append({"play": "link.cover_gap", "target": "gap:4", "uid": 3, "alert": "op:7|not_moving", "until_t": None})
    other = vehicle(3, "HUNTER 9", "seahunter", offset(500, 900))
    ticks = [reads(t=T0, vehicles=[other])] * 3  # verify reads the fleet once, then two ticks
    s = FakeSession(ticks, run="run-A", feed="f")
    s.explained = {5: ours(5, "orbit", [3])}
    s, e, reps, lines = run(ticks, ledger=led, session=s)
    assert e.led.backoff_for(vehicle_key(3), T0 + 50) is None and e.led.stuck_blocks == []
    assert "5" not in e.led.entries, "a card on another vehicle under the uid is not ours to remember"
    assert "memory of uid 3 dropped (another vehicle under the uid)" in [x["text"] for x in lines if x["kind"] == "ledger.verified"][0]
    assert e.led.cooldowns, "the same bridge (same feed): the track cooldown stays"


# ---- 6: a later order worded by its origin; a null task is unknown ----------------------------------------------


def test_r2_6_a_later_order_is_worded_by_its_origin():
    def sea(task, batt=22.0, mode="AUTO"):
        return vehicle(5, "SEA 1", "seahunter", offset(500, 500), battery_pct=batt, mode=mode, task=task)

    for origin, want in ((
            {"source": {"kind": "human", "id": "op-1"}}, "an operator's order (op-1)"),
            ({"source": {"kind": "agent", "id": "agent-watch"}, "policy": {"tier": "auto"}},
             "another agent's order (agent-watch), approved by the policy"),
            ({"source": {"kind": "agent", "id": "agent-watch"}, "approval": {"by": "op-2"}, "policy": {"tier": "human"}},
             "another agent's order (agent-watch), approved by op-2")):
        ticks = [reads(t=T0, vehicles=[sea(None)]),
                 reads(t=T0 + 2, vehicles=[sea({"intent_id": 101, "task": "rtl", "state": "done"}, mode="RTL")]),
                 reads(t=T0 + 4, vehicles=[sea({"intent_id": 201, "task": "patrol", "state": "active"})]),
                 reads(t=T0 + 6, vehicles=[sea({"intent_id": 201, "task": "patrol", "state": "active"})])]
        s = FakeSession(ticks, None, states(4, t1={101: "done"}))
        s.explained = {201: {"id": 201, "kind": "patrol", "uids": ["5"], **origin}}
        s, e, reps, _ = run(ticks, session=s)
        assert [len(r.proposed) for r in reps] == [1, 0, 0, 0]
        assert reps[3].statuses["safety.low_reserve"].startswith(f"active: SEA 1: i201, {want}, given after our rtl i101"), want


def test_r2_6_a_null_task_after_our_rtl_is_unknown_and_never_re_raises():
    """The reviewer's sim T2: re-tasked, then the vehicle's task read null, and rtl went again at
    once, its rationale calling the supersession a failure."""
    def sea(task):
        return vehicle(5, "SEA 1", "seahunter", offset(500, 500), battery_pct=22.0, mode="AUTO", task=task)

    ticks = [reads(t=T0, vehicles=[sea(None)]), reads(t=T0 + 2, vehicles=[sea({"intent_id": 201, "task": "patrol", "state": "active"})]),
             reads(t=T0 + 4, vehicles=[sea(None)]), reads(t=T0 + 6, vehicles=[sea(None)])]
    s = FakeSession(ticks, None, states(4, t1={101: "failed"}))
    s.reasons = {101: "vehicle 5: superseded (replaced by patrol #2)"}
    s, e, reps, _ = run(ticks, session=s)
    assert [len(r.proposed) for r in reps] == [1, 0, 0, 0]
    assert reps[3].statuses["safety.low_reserve"] == ("active: SEA 1: its order reads null after our rtl i101: not raised "
                                                      "again until a definite read")
    assert e.led.last_failure("safety.low_reserve", "battery:5", 5, T0 + 6) is None, "a supersession is not a failure"


# ---- 7: sensor coverage under intermittent sonar ------------------------------------------------------------------


def sonar_track(conf, last_t, **kw):
    return track("T-1", "unknown", 0.0, pos=offset(-1500, 0), vel=(0.0, 0.0), kind="small_craft", kind_conf=conf,
                 sources=[{"uid": 1, "sensor": "sonar", "hits": 3, "last_t": last_t}], **kw)


def look_after(history, now_t, now_track):
    led, cfg = Ledger(), config()
    for t, tr in history:
        led.see_classification([tr], t, cfg, CLASSIFYING)
    led.see_classification([now_track], now_t, cfg, CLASSIFYING)
    b = build(reads(t=now_t, vehicles=[vehicle(3, "PUMA 1", "puma", offset(-1200, 200))], tracks=[now_track]), cfg, led)
    return PLAYS["isr.investigate"].evaluate(b, led)


def test_r2_7_slow_intermittent_sonar_does_not_suppress_a_look():
    """The reviewer's case: sonar on the contact every 20 s, 2 s of dwell each (tau 120 s, at most
    0.8): the classification creeps up about 0.008 a detection. Round 1 suppressed the look for as
    long as any sonar detection was 10 s fresh: 14 minutes to reach 0.6 on sonar alone."""
    hist = [(T0 - 40 + 20 * k, sonar_track(0.30 + 0.008 * k, T0 - 40 + 20 * k)) for k in range(2)]
    res = look_after(hist, T0, sonar_track(0.316, T0 - 1))
    assert res.candidates and res.candidates[0].trigger[-1] == (
        "classification 0.32 under 0.60: our sonar on it, not raising it within 30 s")


def test_r2_7_suppression_is_capped_per_track_and_a_label_conflict_always_counts():
    # rising fast all along, but held unsettled for more than 120 s: a reason again
    hist = [(T0 - 130 + 10 * k, sonar_track(0.05 + 0.03 * k, T0 - 130 + 10 * k)) for k in range(13)]
    res = look_after(hist, T0, sonar_track(0.45, T0 - 1))
    assert res.candidates and res.candidates[0].trigger[-1] == (
        "classification 0.45 under 0.60: our sonar on it 130 s without settling it")
    # rising within the window and under the cap: suppressed, unless the sensors disagree
    hist = [(T0 - 10, sonar_track(0.30, T0 - 10))]
    assert look_after(hist, T0, sonar_track(0.35, T0 - 1)).candidates == []
    res = look_after(hist, T0, sonar_track(0.35, T0 - 1, conflict="sonar calls it submarine while the estimate is small_craft"))
    assert res.candidates and "identity conflict" in res.candidates[0].trigger


# ---- 8: a resumed order inherits the premise of the play that first made it ---------------------------------------


def test_r2_8_release_follows_a_resumed_order_back_to_its_origin_premise():
    """The reviewer's sim S: an earlier release copied the gap filler's orbit; a later look took the
    vehicle; its release resumed the copy although PUMA 2's link was back (the gap closed): a
    release entry has no premise of its own."""
    station = offset(500, 900)
    gap = {"task": "orbit", "uids": [3], "center": station, "radius_m": 300.0}

    def ledger(resumed):
        led = Ledger(run="test")
        led.record(7, play="link.cover_gap", version="1", target="gap:4", uid=3, task="orbit", request=gap, t=T0 - 600,
                   state="failed", basis={}, withdraw_if=[])
        led.record(9, play="isr.release", version="1", target="T-1", uid=3, task="orbit", request=gap, t=T0 - 300,
                   state="done", basis={"running": 8, "resumed": resumed}, withdraw_if=[])
        led.record(10, play="isr.investigate", version="1", target="T-2", uid=3, task="orbit",
                   request={"task": "orbit", "uids": [3], "center": offset(-1800, 600), "radius_m": 1000.0}, t=T0 - 60,
                   state="done", basis={}, withdraw_if=[], prior_task={"intent_id": 9, "task": "orbit", "running": True})
        return led

    me = vehicle(3, "PUMA 1", "puma", offset(-1800, 1400), mode="AUTO", task={"intent_id": 10, "task": "orbit", "state": "done"})
    t2n = track("T-2", "neutral", 0.85, kind="fishing_vessel", pos=offset(-1800, 600))
    back = vehicle(4, "PUMA 2", "puma", station, task={"intent_id": 3, "task": "orbit", "state": "done"})
    for resumed in (None, {"intent": 7, "play": "link.cover_gap", "target": "gap:4"}):
        led = ledger(resumed)
        b = build(reads(vehicles=[back, me], tracks=[t2n]), config(), led)
        assert PLAYS["isr.release"].evaluate(b, led).candidates[0].request == {"task": "hold", "uids": [3]}, resumed
    # the gap still open: the copy goes on, naming its origin
    lost = vehicle(4, "PUMA 2", "puma", station, link_lost=True, link_age_s=60.0, task={"intent_id": 3, "task": "orbit", "state": "done"})
    led = ledger({"intent": 7, "play": "link.cover_gap", "target": "gap:4"})
    r = reads(vehicles=[lost, me], tracks=[t2n], alerts=[{"id": "link:4", "kind": "lost_link", "uid": 4, "since_t": T0 - 60}],
              lost_full={4: {"uid": 4, "link": {"lost_after_s": 3.0}}})
    b = build(r, config(), led)
    c = PLAYS["isr.release"].evaluate(b, led).candidates[0]
    assert c.request == gap and c.basis["resumed"] == {"intent": 7, "play": "link.cover_gap", "target": "gap:4"}
    assert "(our i9, orbit on gap:4, first link.cover_gap's i7), whose premise still holds" in c.derived["action"]


# ---- 9: until murmur-su0, an approved armed card on a suspect vehicle is withdrawn ---------------------------------


def test_r2_9_an_approved_armed_card_on_a_suspect_vehicle_is_withdrawn_from_its_state_until_murmur_su0():
    led = Ledger()
    led.record(22, play="isr.armed_overwatch", version="1", target="T-1", uid=9, task="orbit",
               request={"task": "orbit", "uids": [9]}, t=T0 - 10, state="approved", basis={}, withdraw_if=[], armed=True)
    led.record(23, play="isr.investigate", version="1", target="T-2", uid=3, task="orbit",
               request={"task": "orbit", "uids": [3]}, t=T0 - 10, state="queued", basis={}, withdraw_if=[])
    led.cursor = "f:-1"
    fleet = [vehicle(9, "BLADE 1", "switchblade", gps_glitch=True), vehicle(3, "PUMA 1", "puma", gps_glitch=True)]
    ticks = [reads(t=T0, vehicles=fleet, tracks=[track("T-1"), track("T-2")],
                   intents=[intent(22, "orbit", [9], "approved"), intent(23, "orbit", [3], "queued")])]
    s = FakeSession(ticks)
    s.explained = {22: ours(22, "orbit", [9]), 23: ours(23, "orbit", [3])}
    s.proposed = [{"id": 22, "request": {"task": "orbit", "uids": [9]}, "rationale": "", "state": "approved"},
                  {"id": 23, "request": {"task": "orbit", "uids": [3]}, "rationale": "", "state": "queued"}]
    s, e, reps, lines = run(ticks, ledger=led, session=s)
    calls = [a for n, a in s.calls if n == "withdraw_intent"]
    assert [(a["id"], a["if_state"]) for a in calls] == [(22, "approved")], "the unarmed queued card is left"
    assert calls[0]["reason"].endswith("withdrawn (armed, nav suspect, murmur-su0)")


# ---- nits: the shadow's tier floors -----------------------------------------------------------------------------


def test_r2_nit_the_shadow_estimate_applies_the_bridges_floors():
    policy = {"policy": {"tier": [{"name": "inside", "tier": "auto", "when": {"kind": ["orbit"]}}]}}
    areas = {"areas": [{"kind": "approved", "polygons": [{"exterior": [offset(-6000, -6000), offset(6000, -6000),
                                                                       offset(6000, 6000), offset(-6000, 6000)]}]}]}
    req = {"task": "orbit", "uids": [3], "center": offset(0, 0), "radius_m": 500.0}

    def est(**kw):
        args = dict(armed=False, endurance_known=True, vehicle_pos=tuple(offset(0, 0)), policy=policy, areas=areas)
        args.update(kw)
        return tier.estimate(req, **args)

    assert (est()["tier"], est()["rule"]) == ("auto", "inside")
    assert est(endurance_known=False)["rule"] == "(unverified)"
    assert est(engaged={3})["rule"] == "(engagement in play)"
    assert est(waiting=tier.waiting_uids({"queued": [{"uids": ["3"], "approval": {"by": "op-1"}}]}))["rule"] == \
        "(human order waiting)"
    assert PLATFORMS["platforms"][0]["id"] == "puma"
