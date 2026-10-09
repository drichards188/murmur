"""The playbook engine's fix round 1 (murmur-t8m review): each finding the reviewer ran, as a test
that failed before its fix. The principle: a person's later decision wins over the playbook, and
nothing the playbook does may repeat without a cause it can name. An order of ours that ended
failed, was superseded by a human, or was refused at submission is an answer, not a blank."""

from __future__ import annotations

import asyncio
import json
import re
from pathlib import Path

import pytest

from playbook import fidelity
from playbook import rationale as R
from playbook.arbiter import arbitrate
from playbook.beliefs import RECOVERING_MODES, build
from playbook.config import ConfigError
from playbook.engine import Engine
from playbook.ledger import Ledger, vehicle_key
from playbook.plays import PLAYS
from playbook_world import (PLATFORMS, FakeSession, T0, config, intent, offset, passing, reads, track, vehicle)

ROOT = Path(__file__).resolve().parents[2]
AUTO = passing("auto_approved", "auto", "agent-auto")


def run(ticks, previews=None, cfg=None, intents=None, ledger=None, session=None, ledger_path=None, shadow=False):
    s = session or FakeSession(ticks, previews, intents)
    lines: list[dict] = []
    if ledger is not None and ledger.run is None:
        ledger.run = s.run  # a ledger of this bridge run (a ledger of no known run is set aside)
    e = Engine(cfg or config(), ledger, shadow=shadow, ledger_path=ledger_path,
               log=lambda kind, **f: lines.append({"kind": kind, **f}))

    async def go():
        await e.start(s)
        return [await e.tick(s) for _ in ticks]

    return s, e, asyncio.run(go()), lines


def text(rationale: str) -> str:
    return R.split(rationale)[0]


def states(n: int, **by_tick):
    """{tick: {intent: state}} with each state kept from the tick it is first given."""
    out, now = {}, {}
    for i in range(n):
        now.update(by_tick.get(f"t{i}", {}))
        out[i] = dict(now)
    return out


# ---- 1 (blocker): safety.low_reserve and an operator who re-tasks ------------------------------------------


def test_1_low_reserve_leaves_an_operators_retasking_alone_and_raises_once_under_the_hard_reserve():
    """The reviewer's sim A: our rtl i101 went out at 22% (the line: reserve 20% plus 5); op-1
    sent SEA 1 on a new patrol (i2), which superseded our rtl. Before the fix the next tick sent
    rtl again, undoing the operator. Now: nothing while op-1's order stands; once more when the
    battery falls under the policy's own reserve (20%), saying so; then nothing again."""
    def sea(battery, task):
        return vehicle(5, "SEA 1", "seahunter", offset(500, 500), battery_pct=battery, mode="AUTO", task=task)

    patrol1 = {"intent_id": 1, "task": "patrol", "state": "active"}
    op_patrol = {"intent_id": 201, "task": "patrol", "state": "active"}  # op-1's, after our rtl i101
    op_again = {"intent_id": 202, "task": "patrol", "state": "active"}
    ticks = [reads(t=T0, vehicles=[sea(22.0, patrol1)]),
             reads(t=T0 + 2, vehicles=[sea(22.0, op_patrol)]),
             reads(t=T0 + 4, vehicles=[sea(21.0, op_patrol)]),
             reads(t=T0 + 60, vehicles=[sea(19.5, op_patrol)]),
             reads(t=T0 + 62, vehicles=[sea(19.4, op_again)]),
             reads(t=T0 + 64, vehicles=[sea(19.0, op_again)])]
    s = FakeSession(ticks, intents=states(6, t1={101: "failed"}, t4={102: "failed"}))
    s.reasons = {101: "vehicle 5: superseded (replaced by patrol #2)", 102: "vehicle 5: superseded (replaced by patrol #3)"}
    s.explained = {201: {"id": 201, "kind": "patrol", "uids": ["5"], "source": {"kind": "human", "id": "op-1"}},
                   202: {"id": 202, "kind": "patrol", "uids": ["5"], "source": {"kind": "human", "id": "op-1"}}}
    s, e, reps, lines = run(ticks, session=s)
    assert [len(r.proposed) for r in reps] == [1, 0, 0, 1, 0, 0]
    assert e.led.entries["101"]["superseded_by"] == 201, "the order that superseded ours, by the vehicle's current order"
    assert reps[1].statuses["safety.low_reserve"] == (
        "active: SEA 1: i201, an operator's order (op-1), given after our rtl i101: not sent again while it stands "
        "(once more if it falls under the policy's reserve 20%)")
    raised = text(reps[3].proposed[0]["rationale"])
    assert "Raised again: i201, an operator's order (op-1), given after our rtl i101, still stands, and the battery is now " \
           "under the policy's own reserve 20%: raised once more." in raised
    assert reps[5].statuses["safety.low_reserve"] == (
        "active: SEA 1: i202, an operator's order (op-1), given after our rtl i102: not sent again while it stands")
    d = [x["text"] for x in lines if x["kind"] == "decision"]
    assert d[0].startswith("i101 failed (superseded by i201, someone else's order)")


def test_1_low_reserve_raises_again_only_once_the_superseding_order_no_longer_stands():
    def sea(task):
        return vehicle(5, "SEA 1", "seahunter", offset(500, 500), battery_pct=22.0, mode="AUTO", task=task)

    ticks = [reads(t=T0, vehicles=[sea(None)]),
             reads(t=T0 + 2, vehicles=[sea({"intent_id": 201, "task": "patrol", "state": "active"})]),
             reads(t=T0 + 4, vehicles=[sea({"intent_id": 201, "task": "patrol", "state": "failed"})])]
    s = FakeSession(ticks, intents=states(3, t1={101: "failed"}))
    s.reasons = {101: "vehicle 5: superseded (replaced by patrol #2)"}
    s.explained = {201: {"id": 201, "kind": "patrol", "uids": ["5"], "source": {"kind": "agent", "id": "agent-watch"},
                         "approval": {"by": "op-1"}}}
    s, e, reps, _ = run(ticks, session=s)
    assert [len(r.proposed) for r in reps] == [1, 0, 1]
    raised = text(reps[2].proposed[0]["rationale"])
    assert ("Raised again: i201 (another agent's order (agent-watch), approved by op-1), the order that superseded our rtl "
            "i101, no longer stands and no later order of anyone else's runs on it.") in raised
    assert "failed" not in raised, "a supersession is not a failure (fix round 2)"


# ---- 2 (blocker): every landing, surfacing and return mode -------------------------------------------------


# written out, not RECOVERING_MODES: a test that iterates the constant it checks proves nothing
# about that constant (review of fix round 1); the mav.rs scan below is the test of the list
EVERY_RECOVERING_MODE = ("RTL", "QRTL", "SMART_RTL", "AUTO_RTL", "DOCK", "LAND", "QLAND", "LOITERALTQLAND", "AUTOLAND",
                         "AUTOROTATE", "SURFACE")


@pytest.mark.parametrize("mode", EVERY_RECOVERING_MODE)
def test_2_a_vehicle_recovering_on_its_own_is_left_alone_in_every_mode(mode):
    """QLAND, LOITERALTQLAND, AUTOLAND (plane), SURFACE (sub) were missing: the safety plays then
    commanded over a vehicle's own failsafe, and the looks counted it free."""
    low = reads(vehicles=[vehicle(5, "V5", "seahunter", battery_pct=10.0, mode=mode)])
    led = Ledger()
    b = build(low, config(), led)
    assert PLAYS["safety.low_reserve"].evaluate(b, led).candidates == [], mode
    ok = build(reads(vehicles=[vehicle(5, "V5", "seahunter", mode=mode)]), config(), Ledger())
    assert ok.free_reason(ok.vehicles[5]) == f"recovering on its own ({mode})"
    a = [{"id": "op:9", "kind": "not_moving", "uid": 5, "detail": "NOT MOVING for 12s"}]
    led = Ledger()
    led.see_alerts(a, T0 - 60)
    stuck = reads(vehicles=[vehicle(5, "V5", "seahunter", mode=mode, task={"intent_id": 1, "task": "patrol", "state": "active"})],
                  alerts=a)
    b = build(stuck, config(), led)
    assert PLAYS["safety.stuck"].evaluate(b, led).candidates == [], mode


def test_2_every_landing_return_and_surfacing_mode_the_bridge_names_is_covered():
    """Scans the bridge's mode tables (crates/murmur-bridge/src/mav.rs): every mode whose name
    says it lands, returns, docks, surfaces or autorotates is in RECOVERING_MODES. get_fleet has
    no failsafe field, so the mode is all the playbook has."""
    src = (ROOT / "crates/murmur-bridge/src/mav.rs").read_text()
    modes = set(re.findall(r'\(\d+, "([A-Z_]+)"\)', src))
    named = {m for m in modes if re.search(r"(?:^|_|Q)RTL$|LAND$|^DOCK$|^SURFACE$|^AUTOROTATE$", m)}
    assert {"QLAND", "LOITERALTQLAND", "AUTOLAND", "SURFACE", "QRTL", "SMART_RTL"} <= named, "the scan finds them"
    assert named and named <= set(RECOVERING_MODES), named - set(RECOVERING_MODES)


# ---- 3: a ledger survives a new bridge run -------------------------------------------------------------------


def test_3_a_card_another_run_reused_the_id_of_is_dropped_and_the_old_ledger_kept_beside(tmp_path):
    """The reviewer's sim E: the ledger of an earlier run held our orbit i5; in the new run i5 is
    op-setup's patrol on the same vehicle. Before the fix the playbook took it for its own. Since
    round 2 the ledger is verified card by card (explain_intent), not discarded by its run."""
    p = tmp_path / "playbook-ledger.json"
    old = Ledger(run="20261009T100000-1")
    old.record(5, play="isr.investigate", version="1", target="T-0008", uid=3, task="orbit",
               request={"task": "orbit", "uids": [3], "center": offset(0, 0), "radius_m": 1000.0},
               t=T0 - 3600, state="dispatched", basis={}, withdraw_if=[])
    old.cursor = "oldfeed:77"
    old.save(p)
    v3 = vehicle(3, "PUMA 1", "puma", offset(-1200, 200), mode="AUTO", task={"intent_id": 5, "task": "patrol", "state": "active"})
    ticks = [reads(t=T0, vehicles=[v3], tracks=[])]
    s = FakeSession(ticks, run="20261009T120000-2", feed="newfeed")
    s.explained = {5: {"id": 5, "kind": "patrol", "uids": ["3"], "source": {"kind": "human", "id": "op-setup"}}}
    e = Engine(config(), Ledger.load(p), ledger_path=p, log=lambda kind, **f: lines.append({"kind": kind, **f}))
    lines: list[dict] = []

    async def go():
        await e.start(s)
        return await e.tick(s)

    rep = asyncio.run(go())
    assert rep.proposed == [] and e.led.entries == {} and e.led.run == "20261009T120000-2"
    kept = tmp_path / "playbook-ledger.20261009T100000-1.json"
    assert kept.exists() and Ledger.load(kept).entries["5"]["target"] == "T-0008", "kept beside, renamed"
    assert Ledger.load(p).run == "20261009T120000-2"
    v = [x["text"] for x in lines if x["kind"] == "ledger.verified"]
    assert len(v) == 1 and "1 card(s) no longer ours under their ids (i5) dropped" in v[0]


# ---- 4: safety.stuck and since_t -----------------------------------------------------------------------------


def test_4_stuck_counts_30_s_from_the_first_read_that_showed_the_alert():
    """The alert's since_t was 15 minutes back (its order's last state change): before the fix
    the hold went out on the first read. Now 30 s of our own reads first."""
    alerts = [{"id": "op:9", "kind": "not_moving", "uid": 4, "since_t": T0 - 900, "detail": "NOT MOVING for 12s (aground or stuck?) · orbit"}]
    v4 = vehicle(4, "PUMA 2", "puma", mode="AUTO", task={"intent_id": 3, "task": "orbit", "state": "active"})
    ticks = [reads(t=T0 + dt, vehicles=[v4], alerts=alerts) for dt in (0, 10, 29, 31)]
    s, e, reps, _ = run(ticks)
    assert [len(r.proposed) for r in reps] == [0, 0, 0, 1]
    t = text(reps[3].proposed[0]["rationale"])
    assert "active in our reads for 31 s (its detail \"NOT MOVING for 12s (aground or stuck?) · orbit\")" in t


# ---- 5: a failed order of ours, proposed again every tick ----------------------------------------------------


def gap_ticks(filler_tasks, dts):
    station = offset(500, 900)

    def fleet(task):
        return [vehicle(4, "PUMA 2", "puma", station, mode="AUTO", link_lost=True, link_age_s=30.0,
                        task={"intent_id": 3, "task": "orbit", "state": "done"}),
                vehicle(3, "PUMA 1", "puma", offset(-1200, 200), task=task)]

    alerts = [{"id": "link:4", "kind": "lost_link", "uid": 4, "since_t": T0 - 30, "detail": "no telemetry"}]
    lf = {4: {"uid": 4, "link": {"lost_after_s": 3.0}}}
    led = Ledger()
    led.params["3"] = {"task": "orbit", "params": {"center": station, "radius_m": 300.0}, "source": "human op-setup", "play": None}
    return [reads(t=T0 + dt, vehicles=fleet(task), alerts=alerts, lost_full=lf) for dt, task in zip(dts, filler_tasks)], led


def test_5_a_failed_order_of_ours_backs_off_its_key_and_its_vehicle_doubling_and_the_retry_names_it():
    """The reviewer's sim I: a gap filler's orbit failed and the next tick proposed it again,
    and again. Now 10 s, then 20 s, and the retry's rationale names the failure."""
    failed = {"intent_id": 101, "task": "orbit", "state": "failed"}
    failed2 = {"intent_id": 102, "task": "orbit", "state": "failed"}
    ticks, led = gap_ticks([None, failed, failed, failed, failed2, failed2, failed2],
                           [0, 2, 4, 13, 15, 30, 36])
    s = FakeSession(ticks, AUTO, states(7, t1={101: "failed"}, t4={102: "failed"}))
    s.reasons = {101: "vehicle 3: failed (mission upload unconfirmed)", 102: "vehicle 3: failed (mission upload unconfirmed)"}
    s, e, reps, _ = run(ticks, ledger=led, session=s)
    assert [len(r.proposed) for r in reps] == [1, 0, 0, 1, 0, 0, 1]
    retry = text(reps[3].proposed[0]["rationale"])
    assert "Again after our i101 (link.cover_gap on gap:4) failed: \"vehicle 3: failed (mission upload unconfirmed)\"." in retry
    assert "backing off until 14:38:55Z" in reps[5].statuses["link.cover_gap"], "20 s after the second failure"
    assert e.led.backoff[vehicle_key(3)]["fails"] == 2
    # the vehicle backs off too: no play takes PUMA 1 in the meantime
    ticks, led = gap_ticks([None], [0])
    b = build(ticks[0], config(), led)
    cand = PLAYS["link.cover_gap"].evaluate(b, led).candidates
    led.backoff[vehicle_key(3)] = {"fails": 1, "until_t": T0 + 100, "why": "i99 failed: x"}
    sel, held = arbitrate(cand, b, led)
    assert sel == [] and held[0].reason.startswith("backing off until 14:40:00Z: i99 failed")


# ---- 6: refused at submission ------------------------------------------------------------------------------------


def test_6_a_refusal_at_submission_backs_off_and_is_logged_with_its_reason():
    """The reviewer's sim D: the checks refused the proposal at submission (state rejected, no
    operator): no cooldown, no back-off, and the same card again every tick. Now it backs off
    like a failing preview, the back-off is not cleared, and the log says why."""
    fleet = [vehicle(3, "PUMA 1", "puma", offset(-1200, 200))]
    ticks = [reads(t=T0 + dt, vehicles=fleet, tracks=[track("T-1", "suspect", 0.7)]) for dt in (0, 2, 4, 11, 13)]
    why = "Preview only... orbit on [3]: 1 fail (geofence: enters a no-go area)."
    s = FakeSession(ticks, refuse=lambda req: why)
    s, e, reps, lines = run(ticks, session=s)
    assert [len(s.proposed[:i]) for i in range(1, 6)] and [x["id"] for x in s.proposed] == [101, 102]
    assert [r.proposed for r in reps] == [[], [], [], [], []], "never reported as a live proposal"
    assert e.led.backoff["isr.investigate|T-1|3"]["fails"] == 2
    assert e.led.cooldowns == []
    refused = [x for x in lines if x["kind"] == "proposal" and x.get("refused")]
    assert len(refused) == 2 and "refused at submission: Preview only... orbit on [3]: 1 fail (geofence" in refused[0]["text"]
    assert e.led.entries["101"]["refused"] == why


# ---- 7: a vehicle parked by safety.stuck, re-tasked at once --------------------------------------------------------


def test_7_a_vehicle_stuck_on_our_task_is_not_given_it_again_until_the_alert_clears_and_300_s():
    """The reviewer's sim L: SEA 1 filled SEA 2's gap, stopped moving, our hold parked it, the
    alert cleared (our hold replaced the order), and the next tick gave SEA 1 the gap again."""
    station = offset(500, 900)

    def fleet(task, mode="AUTO"):
        return [vehicle(4, "SEA 2", "seahunter", station, mode="AUTO", link_lost=True, link_age_s=30.0,
                        task={"intent_id": 3, "task": "patrol", "state": "active"}),
                vehicle(3, "SEA 1", "seahunter", offset(-1200, 200), task=task, mode=mode)]

    lost = {"id": "link:4", "kind": "lost_link", "uid": 4, "since_t": T0 - 30, "detail": "no telemetry"}
    lf = {4: {"uid": 4, "link": {"lost_after_s": 3.0}}}
    stuck = {"id": "op:7", "kind": "not_moving", "uid": 3, "since_t": T0 - 60, "detail": "NOT MOVING for 12s (aground or stuck?)"}
    running = {"intent_id": 101, "task": "patrol", "state": "active"}
    held = {"intent_id": 102, "task": "hold", "state": "done"}
    ticks = [reads(t=T0, vehicles=fleet(None), alerts=[lost], lost_full=lf),
             reads(t=T0 + 40, vehicles=fleet(running), alerts=[lost, stuck], lost_full=lf),
             reads(t=T0 + 71, vehicles=fleet(running), alerts=[lost, stuck], lost_full=lf),
             reads(t=T0 + 73, vehicles=fleet(held, "HOLD"), alerts=[lost], lost_full=lf),
             reads(t=T0 + 200, vehicles=fleet(held, "HOLD"), alerts=[lost], lost_full=lf),
             reads(t=T0 + 374, vehicles=fleet(held, "HOLD"), alerts=[lost], lost_full=lf)]
    led = Ledger()
    led.params["3"] = {"task": "patrol", "params": {"route": [station, offset(900, 1200)]}, "source": "human op-setup", "play": None}
    s = FakeSession(ticks, AUTO, states(6, t1={101: "dispatched"}, t3={101: "failed", 102: "done"}))
    s.reasons = {101: "vehicle 3: superseded (replaced by hold #4)"}
    s, e, reps, _ = run(ticks, ledger=led, session=s)
    got = [(p["play"], p["target"]) for r in reps for p in r.proposed]
    assert got == [("link.cover_gap", "gap:4"), ("safety.stuck", "alert:op:7"), ("link.cover_gap", "gap:4")]
    assert [len(r.proposed) for r in reps] == [1, 0, 1, 0, 0, 1], "not before the alert cleared (T0+73) + 300 s"
    assert "SEA 1" in reps[4].statuses["link.cover_gap"] or "stuck" in reps[4].statuses["link.cover_gap"]
    hold = text(reps[2].proposed[0]["rationale"])
    assert "It is not given our link.cover_gap on gap:4 again until the alert clears and 300 s more." in hold
    # free for another task meanwhile
    assert e.led.stuck_block("isr.investigate", "T-1", 3, T0 + 200) is None


# ---- 8: isr.release resumes a failed order -------------------------------------------------------------------------


def test_8_release_never_resumes_a_failed_order_and_holds_instead():
    """The reviewer's sim K: our look i101 failed; our next look i102 on the same vehicle
    recorded i101 as its prior task, and the release sent PUMA 1 back to it."""
    t1 = track("T-1", "suspect", 0.7, pos=offset(-2000, -300))
    t2 = track("T-2", "suspect", 0.7, pos=offset(-1800, 600))
    t2n = track("T-2", "neutral", 0.8, kind="fishing_vessel", pos=offset(-1800, 600))

    def puma(task):
        return [vehicle(3, "PUMA 1", "puma", offset(-1200, 200), task=task)]

    ticks = [reads(t=T0, vehicles=puma(None), tracks=[t1]),
             reads(t=T0 + 12, vehicles=puma({"intent_id": 101, "task": "orbit", "state": "failed"}), tracks=[t2]),
             reads(t=T0 + 23, vehicles=puma({"intent_id": 101, "task": "orbit", "state": "failed"}), tracks=[t2]),
             reads(t=T0 + 25, vehicles=puma({"intent_id": 102, "task": "orbit", "state": "done"}), tracks=[t2]),
             reads(t=T0 + 27, vehicles=puma({"intent_id": 102, "task": "orbit", "state": "done"}), tracks=[t2n])]
    s = FakeSession(ticks, AUTO, states(5, t1={101: "failed"}, t3={102: "done"}))
    s.reasons = {101: "vehicle 3: failed (mission upload unconfirmed)"}
    s, e, reps, _ = run(ticks, session=s)
    assert e.led.entries["102"]["prior_task"] is None, "i101 had failed: no prior task"
    assert [len(r.proposed) for r in reps] == [1, 0, 1, 0, 1], "PUMA 1 backs off 10 s after i101 failed"
    rel = reps[4].proposed[0]
    assert rel["play"] == "isr.release" and rel["request"] == {"task": "hold", "uids": [3]}
    assert "T-2 is shown neutral at 0.80. It holds where it is." in text(rel["rationale"])


def test_8_release_resumes_only_a_running_order_of_ours_whose_premise_still_holds():
    led = Ledger(run="test")
    station = offset(500, 900)
    gap = {"task": "orbit", "uids": [3], "center": station, "radius_m": 300.0}
    led.record(7, play="link.cover_gap", version="1", target="gap:4", uid=3, task="orbit", request=gap, t=T0 - 100,
               state="failed", basis={}, withdraw_if=[])
    led.entries["7"]["superseded_by_ours"] = 8
    led.record(8, play="isr.investigate", version="1", target="T-1", uid=3, task="orbit",
               request={"task": "orbit", "uids": [3], "center": offset(-2000, 0), "radius_m": 1000.0}, t=T0 - 50,
               state="dispatched", basis={}, withdraw_if=[], prior_task={"intent_id": 7, "task": "orbit", "running": True})
    neutral = track("T-1", "neutral", 0.82, kind="fishing_vessel")
    lost = vehicle(4, "PUMA 2", "puma", station, link_lost=True, link_age_s=60.0, task={"intent_id": 3, "task": "orbit", "state": "done"})
    me = vehicle(3, "PUMA 1", "puma", offset(-2000, 900), mode="AUTO", task={"intent_id": 8, "task": "orbit", "state": "done"})
    alerts = [{"id": "link:4", "kind": "lost_link", "uid": 4, "since_t": T0 - 60}]
    r = reads(vehicles=[lost, me], tracks=[neutral], alerts=alerts, lost_full={4: {"uid": 4, "link": {"lost_after_s": 3.0}}})
    b = build(r, config(), led)
    c = PLAYS["isr.release"].evaluate(b, led).candidates
    assert c[0].request == gap
    assert "It goes back to its order before ours (our i7, orbit on gap:4), whose premise still holds" in \
        PLAYS["isr.release"].render(c[0].record("test", T0))
    # PUMA 2's link back: the gap filler's premise is gone, so a hold
    back = vehicle(4, "PUMA 2", "puma", station, task={"intent_id": 3, "task": "orbit", "state": "done"})
    b = build(reads(vehicles=[back, me], tracks=[neutral]), config(), led)
    assert PLAYS["isr.release"].evaluate(b, led).candidates[0].request == {"task": "hold", "uids": [3]}


# ---- 9: an own withdrawal cools (play, target) on every vehicle -------------------------------------------------


def test_9_a_withdrawn_look_does_not_come_straight_back_on_the_next_vehicle():
    """The reviewer's sim B: T-1 turned neutral, we withdrew i101 on PUMA 1, T-1 turned suspect
    again and the next tick proposed it on PUMA 2."""
    fleet = [vehicle(3, "PUMA 1", "puma", offset(-1200, 200)), vehicle(4, "PUMA 2", "puma", offset(-1300, 300))]
    sus, neu = track("T-1", "suspect", 0.7), track("T-1", "neutral", 0.8, kind="fishing_vessel")
    ticks = [reads(t=T0 + dt, vehicles=fleet, tracks=[tr]) for dt, tr in ((0, sus), (2, neu), (4, sus), (20, sus), (35, sus))]
    s, e, reps, lines = run(ticks)
    assert [w["id"] for w in s.withdrawn] == [101]
    assert [len(r.proposed) for r in reps] == [1, 0, 0, 0, 1]
    assert reps[2].statuses["isr.investigate"] == "not ready: T-1: cooling down until 14:38:54Z: i101 withdrawn"


# ---- 10: withdrawals only of what still waits; never an approval that came first --------------------------------


def test_10a_every_withdrawal_asks_for_pending_approval_and_an_approval_that_came_first_stands():
    fleet = [vehicle(3, "PUMA 1", "puma", offset(-1200, 200))]
    sus, neu = track("T-1", "suspect", 0.7), track("T-1", "neutral", 0.8, kind="fishing_vessel")
    ticks = [reads(t=T0, vehicles=fleet, tracks=[sus]), reads(t=T0 + 2, vehicles=fleet, tracks=[neu]),
             reads(t=T0 + 4, vehicles=fleet, tracks=[neu])]
    s = FakeSession(ticks)
    s.decided_meanwhile = {101: "approved"}  # op-1 approved between our read and our withdraw
    s.intent_states = {2: {101: "approved"}}
    s, e, reps, lines = run(ticks, session=s)
    calls = [a for n, a in s.calls if n == "withdraw_intent"]
    assert calls and all(a["if_state"] == "pending_approval" for a in calls)
    assert s.withdrawn == [] and e.led.cooldowns == [], "nothing withdrawn, nothing cooled"
    assert [x["kind"] for x in lines if x["kind"] in ("withdraw.refused", "left")] == ["withdraw.refused", "left"]
    left = next(x["text"] for x in lines if x["kind"] == "left")
    assert left == ("i101 (isr.investigate on T-1) is approved and no longer holds: T-1 is shown neutral at 0.80. "
                    "Left as it is: the playbook withdraws only what still waits for approval")


def test_10b_an_approved_or_queued_card_is_never_withdrawn_by_hygiene_and_only_armed_by_nav_suspect():
    """Round 1 left every approved or queued card alone. Round 2: until murmur-su0 lands (the
    bridge's re-check at dispatch does not read the GPS glitch flag), nav.suspect_exclude still
    withdraws an approved or queued armed card of ours on a vehicle whose navigation is suspect,
    with that card's state as the precondition. Unarmed approved cards stay alone."""
    led = Ledger()
    led.record(21, play="isr.investigate", version="1", target="T-1", uid=3, task="orbit", request={}, t=T0, state="queued",
               basis={}, withdraw_if=[])
    led.record(22, play="isr.armed_overwatch", version="1", target="T-1", uid=9, task="orbit", request={}, t=T0,
               state="approved", basis={}, withdraw_if=[], armed=True)
    fleet = [vehicle(3, "PUMA 1", battery_pct=30.0), vehicle(9, "BLADE 1", "switchblade", nav_source="dead_reckoning")]
    b = build(reads(vehicles=fleet, tracks=[track("T-1", "neutral", 0.8, kind="fishing_vessel")]), config(), led)
    for name in ("hygiene.withdraw_stale", "link.withdraw_pending"):
        assert PLAYS[name].withdrawals(b, led, led.open_entries()) == [], name
    w = PLAYS["nav.suspect_exclude"].withdrawals(b, led, [e for e in led.open_entries() if e["state"] == "pending_approval"])
    assert [(x.intent, x.if_state) for x in w] == [(22, "approved")]
    assert w[0].reason.endswith("withdrawn (armed, nav suspect, murmur-su0)")
    led.entries["22"]["armed"] = False
    assert PLAYS["nav.suspect_exclude"].withdrawals(b, led, []) == [], "an unarmed approved card stays alone"


# ---- 11 is in test_playbook_arbiter.py (preview first; P0 by class); the shadow's tier estimate here ------------


def test_11_the_shadow_estimates_each_would_be_cards_tier_from_the_policy_and_budgets_only_human_ones():
    policy = {"policy": {"rules": {"reserve_pct": 20.0, "task_duration_s": 900}, "approval": {"expire_s": 120},
                         "tier": [{"name": "safety", "tier": "auto", "when": {"kind": ["hold", "rtl"]}},
                                  {"name": "armed", "tier": "human", "when": {"armed": True}},
                                  {"name": "leaves", "tier": "human", "when": {"leaves_approved_area": True}},
                                  {"name": "inside", "tier": "auto", "when": {"kind": ["orbit", "patrol"]}}]}}
    inside = {"areas": [{"kind": "approved", "polygons": [{"exterior": [offset(-6000, -6000), offset(6000, -6000),
                                                                        offset(6000, 6000), offset(-6000, 6000)]}]}]}
    fleet = [vehicle(3, "PUMA 1", "puma", offset(-1200, 200))]
    ticks = [reads(t=T0, vehicles=fleet, tracks=[track("T-1", "suspect", 0.7)], policy=policy)]
    ticks[0].platforms = {**PLATFORMS, "platforms": [{**p, "endurance_h": 2.5} for p in PLATFORMS["platforms"]]}
    s = FakeSession(ticks, areas=inside)
    s, e, reps, lines = run(ticks, session=s, shadow=True)
    would = [x for x in lines if x["kind"] == "shadow"]
    assert would[0]["tier_estimate"]["tier"] == "auto" and would[0]["tier_estimate"]["rule"] == "inside"
    assert e.led.bucket == {}, "an auto-tier would-be card spends no budget"
    assert s.writers() == [] and not any(n == "preview_intent" for n, _ in s.calls)
    s = FakeSession(ticks, areas={"areas": []})
    s, e, reps, lines = run(ticks, session=s, shadow=True)
    assert [x for x in lines if x["kind"] == "shadow"][0]["tier_estimate"]["rule"] == "leaves"
    assert e.led.bucket["tokens"] == 1.0


# ---- 12: velocity unknown, and the numbers of the text ----------------------------------------------------------


def test_12_a_track_with_no_velocity_says_velocity_unknown_and_every_number_traces_to_the_record():
    """The reviewer's sim J: velocity null rendered as "0.0 m/s course 000"."""
    tr = track("T-1", "unknown", 0.0, kind=None, kind_conf=0.0, vel=(0.0, 0.0))
    tr["velocity_mps"], tr["speed_mps"], tr["course_deg"] = None, 0.0, None
    tr["position"] = tr["predicted"] = offset(-1500, -300)
    s, e, reps, _ = run([reads(t=T0, vehicles=[vehicle(3, "PUMA 1", "puma", offset(-1200, 200))], tracks=[tr])])
    rat = s.proposed[0]["rationale"]
    assert "velocity unknown" in text(rat) and "m/s" not in text(rat) and "course" not in text(rat)
    assert fidelity.check(rat) == []
    # the check catches a number with no source: a course the record does not hold
    rec = R.parse_record(rat)
    bad = text(rat).replace("velocity unknown", "0.0 m/s course 000")
    assert fidelity.untraced(bad, rec) == ["the number 0.0 m/s in the text traces to no field of the record",
                                           "the number course 000 in the text traces to no field of the record"]


def test_12_every_plays_rationale_passes_the_number_trace():
    from test_playbook_plays import gap_world, look, running

    eng = Engine(config(enable=["isr.armed_overwatch"]))
    rats = []
    r, led, _ = gap_world(7.0)
    res, b = look(r, led=led, play="link.cover_gap")
    rats.append(eng.finish(res.candidates[0], b, passing(), [])[0])
    res, b = look(reads(vehicles=[vehicle(3, "PUMA 1", battery_pct=22.0)]), play="safety.low_reserve")
    rats.append(eng.finish(res.candidates[0], b, AUTO, [])[0])
    led = Ledger()
    a = [{"id": "op:9", "kind": "not_moving", "uid": 4, "detail": "NOT MOVING for 12s"}]
    led.see_alerts(a, T0 - 40)
    stuck = reads(vehicles=[vehicle(4, "PUMA 2", mode="AUTO", task={"intent_id": 3, "task": "orbit", "state": "active"})], alerts=a)
    res, b = look(stuck, led=led, play="safety.stuck")
    rats.append(eng.finish(res.candidates[0], b, AUTO, [])[0])
    fleet = [vehicle(3, "PUMA 1", "puma", offset(-1200, 200)), vehicle(9, "BLADE 1", "switchblade", offset(-1000, 100))]
    res, b = look(reads(vehicles=fleet, tracks=[track("T-1", "suspect", 0.7)]), play="isr.investigate")
    rats.append(eng.finish(res.candidates[0], b, passing(), [])[0])
    closing = track("T-1", "suspect", 0.8, pos=offset(-2000, -100), vel=(8.0, 0.0))
    res, b = look(reads(vehicles=fleet, tracks=[closing]), cfg=config(enable=["isr.armed_overwatch"]), play="isr.armed_overwatch")
    rats.append(eng.finish(res.candidates[0], b, passing(), [])[0])
    led = Ledger()
    running(led, 7, "T-1", 3, offset(-2000, 0))
    on = [vehicle(3, "PUMA 1", "puma", offset(-2000, 900), mode="AUTO", task={"intent_id": 7, "task": "orbit", "state": "done"})]
    res, b = look(reads(vehicles=on, tracks=[track("T-1", "neutral", 0.82, kind="fishing_vessel")]), led=led, play="isr.release")
    rats.append(eng.finish(res.candidates[0], b, AUTO, [])[0])
    res, b = look(reads(vehicles=on, tracks=[track("T-1", pos=offset(-1200, 0))]), led=led, play="isr.recentre")
    rats.append(eng.finish(res.candidates[0], b, passing(), [])[0])
    plays = [R.parse_record(x)["play"] for x in rats]
    assert len(set(plays)) == 7, plays
    for x in rats:
        assert fidelity.check(x) == [], text(x)


# ---- 14: a running armed overwatch is released too --------------------------------------------------------------


def test_14_release_covers_a_running_armed_overwatch():
    led = Ledger()
    led.record(9, play="isr.armed_overwatch", version="1", target="T-1", uid=9, task="orbit",
               request={"task": "orbit", "uids": [9], "center": offset(-2000, 0), "radius_m": 500.0}, t=T0 - 60,
               state="dispatched", basis={}, withdraw_if=[], armed=True)
    blade = vehicle(9, "BLADE 1", "switchblade", offset(-2000, 500), mode="AUTO", task={"intent_id": 9, "task": "orbit", "state": "done"})
    b = build(reads(vehicles=[blade], tracks=[track("T-1", "neutral", 0.85, kind="fishing_vessel")]),
              config(enable=["isr.armed_overwatch"]), led)
    c = PLAYS["isr.release"].evaluate(b, led).candidates
    assert [(x.task, x.uid, x.supersedes) for x in c] == [("hold", 9, 9)]
    assert "BLADE 1 from our orbit i9 on T-1" in PLAYS["isr.release"].render(c[0].record("test", T0))


# ---- the nits -----------------------------------------------------------------------------------------------------


def test_an_adopted_armed_card_keeps_its_armed_flag_and_nav_suspect_withdraws_it():
    """The reviewer's sim N: after a restart, an armed card adopted from its record lost `armed`,
    so suspect navigation never withdrew it."""
    rec = {"play": "isr.armed_overwatch@1", "target": "T-1", "uid": 9, "request": {"task": "orbit", "uids": [9]},
           "basis": {}, "withdraw_if": [], "armed": True}
    led = Ledger()
    led.sync({12: intent(12, "orbit", [9], "pending_approval", rationale=R.compose("[isr.armed_overwatch v1] x", rec))},
             T0, config())
    assert led.entries["12"]["armed"] is True
    b = build(reads(vehicles=[vehicle(9, "BLADE 1", "switchblade", nav_source="dead_reckoning")], tracks=[track("T-1")]),
              config(), led)
    assert [w.intent for w in PLAYS["nav.suspect_exclude"].withdrawals(b, led, led.open_entries())] == [12]


def test_an_engagement_in_play_withholds_isr_and_lets_a_p0_card_go_to_a_person():
    """The reviewer's sim H, decided again in round 2: a P0 command on a platform with a
    nomination pending goes on, and the bridge makes it a human card (an agent's order there
    needs a person, who acknowledges it stops the engagement); its rationale says so. Our
    release of the same platform (P2) stays withheld, said once in the panel."""
    eng = [intent(50, "engage", [4], source={"kind": "agent", "id": "agent-watch"})]
    led = Ledger()
    led.record(7, play="isr.investigate", version="1", target="T-1", uid=4, task="orbit",
               request={"task": "orbit", "uids": [4], "center": offset(-2000, -300), "radius_m": 1000.0}, t=T0 - 100,
               state="dispatched", basis={}, withdraw_if=[])
    led.cursor = "f:-1"  # this bridge's feed: a restart of the playbook, not of the bridge
    v4 = vehicle(4, "PUMA 2", "puma", offset(-1200, 200), mode="AUTO", task={"intent_id": 7, "task": "orbit", "state": "done"})
    alerts = [{"id": "op:9", "kind": "not_moving", "uid": 4, "since_t": T0 - 400, "detail": "NOT MOVING for 10s"}]
    neutral = track("T-1", "neutral", 0.85, kind="fishing_vessel")
    human = passing("pending_approval", "human", "(engagement in play)")
    ticks = [reads(t=T0 + dt, vehicles=[v4], pending=eng, alerts=alerts, tracks=[neutral]) for dt in (0, 31, 33)]
    s = FakeSession(ticks, human)
    s.explained = {7: {"id": 7, "kind": "orbit", "uids": ["4"], "source": {"kind": "agent", "id": "agent-playbook"}}}
    s, e, reps, lines = run(ticks, ledger=led, session=s)
    assert [(p["request"]["task"], p["request"]["uids"]) for p in s.proposed] == [("hold", [4])]
    t = text(s.proposed[0]["rationale"])
    assert t.startswith("[safety.stuck v1]")
    assert "An engagement is in play on this platform (uid 4: nomination i50 pending): the bridge sends this card to a " \
           "person, and approving it stops that engagement." in t
    w = [x["text"] for x in lines if x["kind"] == "withheld"]
    assert w == ["would hold PUMA 2 for T-1 (isr.release), not sent: engagement in play on uid 4 (nomination i50 "
                 "pending): the playbook gives it no ISR, link or hygiene order"]


def test_a_truncated_change_feed_reads_the_engagements_again():
    v3 = vehicle(3, "PUMA 1", "puma", offset(-1200, 200))
    ticks = [reads(t=T0 + dt, vehicles=[v3], tracks=[track("T-1", "suspect", 0.7)]) for dt in (0, 2)]
    s = FakeSession(ticks)
    s.truncate_at = {0}
    s.nominations = [{"id": 61, "uids": ["3"], "state": "approved"}]
    s, e, reps, lines = run(ticks, session=s)
    assert ("get_nominations", {}) in s.calls and e.led.engaging["61"] == {"uids": [3], "state": "approved"}
    assert any(x["kind"] == "feed.truncated" for x in lines)
    assert s.writers() == [], "the engagement the feed dropped still keeps the look off PUMA 1"


def test_an_open_card_of_ours_older_than_the_newest_page_is_read_by_explain_intent():
    fleet = [vehicle(3, "PUMA 1", "puma", offset(-1200, 200))]
    sus, neu = track("T-1", "suspect", 0.7), track("T-1", "neutral", 0.8, kind="fishing_vessel")
    ticks = [reads(t=T0, vehicles=fleet, tracks=[sus]), reads(t=T0 + 2, vehicles=fleet, tracks=[neu])]
    s = FakeSession(ticks)
    s.page = {1: set()}  # i101 is off the newest page at tick 1
    s, e, reps, _ = run(ticks, session=s)
    assert ("explain_intent", {"id": 101}) in s.calls
    assert [w["id"] for w in s.withdrawn] == [101], "still followed up"


def test_a_polygon_watch_area_keeps_a_buffer_before_a_track_has_left_it():
    """Hysteresis for a polygon area: a track counts as still watched until it is beyond the
    area's edge by (leave factor - 1) x the polygon's mean radius, as a circle's 1.1 x radius."""
    from playbook.plays.isr import still_watched

    square = [offset(-1000, -1000), offset(1000, -1000), offset(1000, 1000), offset(-1000, 1000)]
    cfg = config(watch_area=[{"name": "the box", "polygon": square}], protected=[])
    # the square's area is a circle of 1128 m radius: the buffer is 0.1 x 1128 = 113 m past its edge
    for east, want in ((900, True), (1080, True), (1400, False)):
        tr = track("T-1", "suspect", 0.7, pos=offset(east, 0), vel=(0.0, 1.0))
        b = build(reads(tracks=[tr]), cfg, Ledger())
        assert still_watched(b.tracks["T-1"], b) is want, east


def test_config_values_that_divide_or_gate_must_be_positive():
    for bad in ({"thresholds": {"tcpa_max_s": 0}}, {"budget": {"human_every_s": 0}}, {"thresholds": {"fast_mps": -1}},
                {"thresholds": {"leave_cpa_factor": 0.5}}, {"cooldown": {"preview_backoff_s": 0}}):
        with pytest.raises(ConfigError):
            config(**bad)
    assert json.loads(json.dumps(config().data))["thresholds"]["stuck_retry_s"] == 300.0
