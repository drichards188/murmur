"""The arbiter's total order and claims, and the engine's inbox budget and preview cap
(murmur-t8m). The budget is exercised through ticks on a fake session: it counts what was
previewed and proposed."""

from __future__ import annotations

import asyncio

from playbook.arbiter import arbitrate, order
from playbook.beliefs import build
from playbook.engine import Engine
from playbook.ledger import Ledger
from playbook.plays import Candidate
from playbook_world import T0, FakeSession, config, offset, passing, reads, track, vehicle


def cand(play: str, klass: int, score: float, target: str, uid: int | None, task: str = "orbit", **kw) -> Candidate:
    return Candidate(play=play, version="1", klass=klass, score=score, target=target, uid=uid, task=task,
                     request={"task": task, "uids": [uid] if uid is not None else []}, facts=[], derived={}, thresholds={},
                     withdraw_if=[], basis={}, claims=(uid,) if uid is not None else (), **kw)


def beliefs(led=None, pending=None, vehicles=None):
    led = led or Ledger()
    vs = vehicles or [vehicle(u, f"V{u}") for u in (1, 2, 3, 4)]
    return build(reads(vehicles=vs, pending=pending or []), config(), led), led


def test_the_total_order_is_class_then_score_then_play_target_uid():
    cs = [cand("isr.investigate", 2, 0.6, "T-2", 3), cand("link.cover_gap", 3, 1.0, "gap:9", 4),
          cand("safety.stuck", 0, 0.1, "alert:op:1", 1), cand("isr.investigate", 2, 0.6, "T-1", 2),
          cand("isr.armed_overwatch", 2, 0.6, "T-1", 2), cand("isr.investigate", 2, 0.9, "T-3", 1)]
    got = [(c.play, c.target, c.uid) for c in order(cs)]
    assert got == [("safety.stuck", "alert:op:1", 1), ("isr.investigate", "T-3", 1), ("isr.armed_overwatch", "T-1", 2),
                   ("isr.investigate", "T-1", 2), ("isr.investigate", "T-2", 3), ("link.cover_gap", "gap:9", 4)]
    assert [c.key for c in order(list(reversed(cs)))] == [c.key for c in order(cs)], "the input order does not matter"


def test_one_claim_per_vehicle_and_one_card_per_play_and_target():
    b, led = beliefs()
    sel, held = arbitrate([cand("isr.investigate", 2, 0.9, "T-1", 3), cand("isr.investigate", 2, 0.5, "T-2", 3),
                           cand("isr.investigate", 2, 0.4, "T-1", 4)], b, led)
    assert [(c.target, c.uid) for c in sel] == [("T-1", 3)]
    # T-1's other choice is not news (it got a vehicle); T-2 is held, and says why
    assert [(h.candidate.target, h.reason) for h in held] == [("T-2", "uid 3 is taken by isr.investigate on T-1 this tick")]


def test_a_vehicle_taken_by_a_higher_look_falls_to_the_next_choice_and_the_rationale_says_so():
    b, led = beliefs()
    first = [cand("isr.investigate", 2, 0.9, "T-1", 3), cand("isr.investigate", 2, 0.899, "T-1", 4)]
    second = [cand("isr.investigate", 2, 0.8, "T-2", 3), cand("isr.investigate", 2, 0.799, "T-2", 4)]
    sel, held = arbitrate(first + second, b, led)
    assert [(c.target, c.uid) for c in sel] == [("T-1", 3), ("T-2", 4)] and held == []
    assert sel[1].derived["passed_over"] == ["V3 (taken for T-1)"]


def test_a_claim_by_a_card_of_ours_holds_except_for_its_successor_and_p0():
    led = Ledger()
    led.record(7, play="isr.investigate", version="1", target="T-1", uid=3, task="orbit", request={}, t=T0, state="pending_approval",
               basis={}, withdraw_if=[])
    b = build(reads(vehicles=[vehicle(3, "V3")], intents=[{"id": 7, "uids": ["3"], "state": "pending_approval", "kind": "orbit"}]),
              config(), led)
    assert b.claims == {3: "our i7 (pending_approval)"}
    sel, held = arbitrate([cand("isr.investigate", 2, 0.9, "T-2", 3)], b, led)
    assert sel == [] and held[0].reason == "uid 3 is our i7 (pending_approval)"
    sel, _ = arbitrate([cand("isr.recentre", 2, 0.9, "T-1", 3, supersedes=7)], b, led)
    assert len(sel) == 1, "a recentre takes over its own orbit's vehicle"
    sel, _ = arbitrate([cand("safety.low_reserve", 0, 0.5, "battery:3", 3, task="rtl")], b, led)
    assert len(sel) == 1, "a P0 command is never held back by a card of ours"


def test_an_engagement_in_play_blocks_every_play_but_hold_and_rtl():
    pending = [{"id": 40, "kind": "engage", "uids": ["4"], "state": "pending_approval"}]
    b, led = beliefs(pending=pending)
    sel, held = arbitrate([cand("isr.investigate", 2, 0.9, "T-1", 4), cand("isr.release", 2, 0.5, "T-1", 4, task="hold")], b, led)
    assert [(c.play, c.task) for c in sel] == [("isr.release", "hold")]
    assert held[0].reason == "uid 4: nomination i40 pending (only hold and rtl)"


def test_a_busy_inbox_lets_only_p0_and_p1_through():
    pending = [{"id": i, "kind": "orbit", "uids": ["1"], "state": "pending_approval"} for i in range(1, 7)]
    b, led = beliefs(pending=pending)
    sel, held = arbitrate([cand("isr.investigate", 2, 0.9, "T-1", 3), cand("safety.low_reserve", 0, 0.2, "battery:4", 4, task="rtl")],
                          b, led)
    assert [c.play for c in sel] == ["safety.low_reserve"]
    assert held[0].reason == "the inbox holds 6 pending cards: only P0 and P1 propose"


# ---- the budget, through ticks ----------------------------------------------------------------------------

THREE = [track(f"T-{i}", "suspect", 0.7, pos=offset(-1500 - 100 * i, -200 * i), vel=(10.0, 0.0)) for i in (1, 2, 3)]
PUMAS = [vehicle(u, f"PUMA {u}", "puma", offset(-1000 - 100 * u, 300)) for u in (3, 5, 6, 7)]


def run(ticks, previews=None, cfg=None, intents=None):
    s = FakeSession(ticks, previews, intents)
    e = Engine(cfg or config())

    async def go():
        await e.start(s)
        return [await e.tick(s) for _ in ticks]

    return s, e, asyncio.run(go())


def test_the_budget_takes_a_burst_of_two_then_one_card_per_30_s():
    ticks = [reads(t=T0 + dt, vehicles=PUMAS, tracks=THREE) for dt in (0, 2, 4, 31)]
    s, e, reps = run(ticks)
    assert [len(r.proposed) for r in reps] == [2, 0, 0, 0], "a burst of two, and at most two of ours pending"
    assert s.writers() == ["preview_intent", "propose_intent", "preview_intent", "propose_intent"]
    assert reps[1].statuses["isr.investigate"].startswith("active")
    # the operator decides one of them: a card per 30 s again, the third track gets its turn
    ticks = [reads(t=T0 + dt, vehicles=PUMAS, tracks=THREE) for dt in (0, 2, 31, 33)]
    s, e, reps = run(ticks, intents={2: {101: "approved"}, 3: {101: "approved"}})
    assert [len(r.proposed) for r in reps] == [2, 0, 1, 0]
    held = [h for h in reps[1].statuses.values() if "inbox budget" in h]
    assert held, reps[1].statuses


def test_no_preview_is_spent_on_a_card_the_budget_cannot_take():
    ticks = [reads(t=T0 + dt, vehicles=PUMAS, tracks=THREE) for dt in (0, 2, 4, 6)]
    s, _, _ = run(ticks)
    assert sum(1 for n in s.writers() if n == "preview_intent") == 2, "only the two it could propose"


def test_at_most_three_previews_a_tick():
    many = [track(f"T-{i}", "suspect", 0.7, pos=offset(-1500 - 50 * i, -100 * i), vel=(10.0, 0.0)) for i in range(1, 6)]
    fleet = [vehicle(u, f"PUMA {u}", "puma", offset(-1000 - 50 * u, 300)) for u in range(3, 9)]
    failing = passing(checks=[{"rule": "geofence", "uid": "3", "outcome": "fail", "detail": "enters a no-go area"}])
    s, e, reps = run([reads(vehicles=fleet, tracks=many)], previews=failing)
    assert reps[0].previews == 3 and sum(1 for n in s.writers() if n == "preview_intent") == 3
    assert len(e.led.backoff) == 3, "each failing preview backs off its key"
