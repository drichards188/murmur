"""The playbook engine's fix round 3 (a third review of murmur-t8m): the ledger's verification
must not take an error for an answer, and four cheap fixes. Each failed before its fix."""

from __future__ import annotations

import asyncio

from playbook.beliefs import build
from playbook.engine import Engine
from playbook.ledger import FAILURE_MEMORY_S, Ledger
from playbook.plays import PLAYS
from playbook_world import (FakeSession, T0, config, intent, offset, passing, reads, track, vehicle)


def ours(iid, kind, uids, state="pending_approval"):
    return {"id": iid, "kind": kind, "uids": [str(u) for u in uids], "state": state,
            "source": {"kind": "agent", "id": "agent-playbook"}}


def ledger_with_cards(cursor="f:-1"):
    """A look of ours op-1 rejected a minute ago (and its cooldown), and one still waiting."""
    led = Ledger(run="test")
    led.cursor = cursor
    for iid, target, uid, state in ((101, "T-1", 3, "rejected"), (102, "T-2", 5, "pending_approval")):
        led.record(iid, play="isr.investigate", version="1", target=target, uid=uid, task="orbit",
                   request={"task": "orbit", "uids": [uid], "center": offset(-2000, -300), "radius_m": 1000.0},
                   t=T0 - 60, state=state, basis={"affiliation": "suspect", "aff_conf": 0.7, "declared": None, "cpa_m": None},
                   withdraw_if=[])
    led.cooldowns.append({"scope": "isr.investigate|T-1", "until_t": T0 + 540, "why": "rejected", "set_t": T0 - 60,
                          "intent": 101, "by": "op-1", "note": "not now", "basis": led.entries["101"]["basis"]})
    return led


FLEET = [vehicle(3, "PUMA 1", "puma", offset(-1200, 200)), vehicle(5, "PUMA 3", "puma", offset(-1300, 300)),
         vehicle(6, "PUMA 4", "puma", offset(-1400, 300))]


def engine(led, lines, path=None):
    return Engine(config(), led, ledger_path=path, log=lambda kind, **f: lines.append({"kind": kind, **f}))


# ---- 1: an error is not an answer ------------------------------------------------------------------------------


class Flaky(FakeSession):
    """explain_intent times out while `down` (a slow bridge, a dropped connection)."""

    down = True

    async def call(self, name, **args):
        if name == "explain_intent" and self.down:
            raise TimeoutError("explain_intent timed out")
        return await super().call(name, **args)


def test_r3_1_a_timeout_mid_verify_keeps_every_card_and_its_cooldown_and_the_engine_waits(tmp_path):
    """The reviewer's sim U: explain_intent timed out during the check (the playbook restarted on
    the same bridge), the waiting card was taken for "not ours" and dropped, the panel said so,
    and its cooldown went with it. Now nothing is dropped, nothing acts, and the next tick tries
    again; once the check completes, op-1's rejection still holds T-1 off."""
    p = tmp_path / "playbook-ledger.json"
    led = ledger_with_cards()
    led.save(p)
    on_disk = p.read_text()
    tracks = [track("T-1", "suspect", 0.7), track("T-2", "suspect", 0.7, pos=offset(-1800, 600))]
    ticks = [reads(t=T0 + dt, vehicles=FLEET, tracks=tracks) for dt in (0, 2, 4)]
    s = Flaky(ticks)
    s.explained = {102: ours(102, "orbit", [5])}
    s.proposed = [{"id": 102, "request": {"task": "orbit", "uids": [5]}, "rationale": "", "state": "pending_approval"}]
    s.next_id = 103
    lines: list[dict] = []
    e = engine(Ledger.load(p), lines, p)

    disk = []

    async def go():
        info = await e.start(s)
        first = await e.tick(s)
        disk.append(p.read_text())
        s.down = False
        return info, first, await e.tick(s)

    info, first, second = asyncio.run(go())
    assert disk == [on_disk], "the ledger on disk as it was while unverified"
    assert info["verified"] is False
    assert first.proposed == [] and first.statuses == {}, "no tick on an unverified ledger"
    u = [x["text"] for x in lines if x["kind"] == "ledger.unverified"]
    assert len(u) == 2 and "TimeoutError: explain_intent timed out" in u[0]
    assert not [x for x in lines if x["kind"] == "ledger.verified"], "no false 'no longer ours'"
    assert e.verified is True and set(e.led.entries) >= {"101", "102"} and len(e.led.cooldowns) == 1
    assert [p["target"] for p in second.proposed] == [], "T-1 cools after op-1's rejection; T-2 has our card"
    assert Ledger.load(p).entries.keys() >= {"101", "102"}


def test_r3_1_a_session_dropped_mid_verify_keeps_everything_and_the_next_start_completes():
    led = ledger_with_cards()
    tracks = [track("T-1", "suspect", 0.7)]
    ticks = [reads(t=T0 + dt, vehicles=FLEET, tracks=tracks) for dt in (0, 2, 4)]
    s1 = FakeSession(ticks)
    s1.explain_raise = {102: ConnectionError("session dropped")}
    lines: list[dict] = []
    e = engine(led, lines)
    asyncio.run(e.start(s1))
    assert e.verified is False and set(e.led.entries) == {"101", "102"} and len(e.led.cooldowns) == 1
    s2 = FakeSession(ticks)  # the reconnect, to the same bridge
    s2.explained = {102: ours(102, "orbit", [5])}

    async def go():
        await e.start(s2)
        return await e.tick(s2)

    rep = asyncio.run(go())
    assert e.verified is True and set(e.led.entries) == {"101", "102"} and len(e.led.cooldowns) == 1
    assert [p["target"] for p in rep.proposed] == [], "the rejected look does not come back"


def test_r3_1_only_an_unknown_intent_drops_its_card():
    led = ledger_with_cards()
    led.entries["102"]["state"] = "pending_approval"
    led.record(103, play="isr.investigate", version="1", target="T-3", uid=6, task="orbit",
               request={"task": "orbit", "uids": [6]}, t=T0 - 30, state="pending_approval", basis={}, withdraw_if=[])
    ticks = [reads(t=T0, vehicles=FLEET, tracks=[track("T-1", "suspect", 0.7)])]
    s = FakeSession(ticks)
    s.explained = {102: ours(102, "orbit", [5])}
    s.explain_raise = {103: RuntimeError("explain_intent: unknown intent 103")}
    lines: list[dict] = []
    e = engine(led, lines)
    asyncio.run(e.start(s))
    assert e.verified is True and set(e.led.entries) == {"101", "102"}, "101 ended on this bridge: not asked"
    assert len(e.led.cooldowns) == 1
    assert "(i103) dropped" in [x["text"] for x in lines if x["kind"] == "ledger.verified"][0]
    assert [a["id"] for n, a in s.calls if n == "explain_intent"] == [102, 103], "ended cards are not checked"


# ---- 2: a hygiene withdrawal for the sensor's sake cools that vehicle only ----------------------------------------


def test_r3_2_a_hygiene_withdrawal_whose_reason_is_the_sensor_cools_that_vehicle_not_the_pair():
    led = Ledger()
    led.record(21, play="isr.investigate", version="1", target="T-1", uid=3, task="orbit", request={}, t=T0,
               state="pending_approval", basis={}, withdraw_if=[])
    low = [vehicle(3, "PUMA 1", battery_pct=30.0)]  # under the free line: the sensor, not the track
    b = build(reads(vehicles=low, tracks=[track("T-1")]), config(), led)
    w = PLAYS["hygiene.withdraw_stale"].withdrawals(b, led, led.open_entries())
    assert [(x.intent, x.cause) for x in w] == [(21, "vehicle")]
    neutral = track("T-1", "neutral", 0.8, kind="fishing_vessel")
    b = build(reads(vehicles=[vehicle(3, "PUMA 1")], tracks=[neutral]), config(), led)
    assert [x.cause for x in PLAYS["hygiene.withdraw_stale"].withdrawals(b, led, led.open_entries())] == ["premise"]
    led.entries["21"].update(withdrawn_by="hygiene.withdraw_stale", withdraw_cause="vehicle")
    led.sync({21: intent(21, "orbit", [3], "voided")}, T0 + 2, config())
    assert led.cooldown_for("isr.investigate", "T-1", 5, T0 + 3) is None, "another vehicle may take T-1 at once"
    assert led.cooldown_for("isr.investigate", "T-1", 3, T0 + 3)["scope"] == "isr.investigate|T-1|3"


# ---- 3: ended cards, not checked on the same bridge, pruned once nothing uses them -----------------------------


def test_r3_3_ended_cards_are_pruned_once_nothing_can_use_them():
    led = Ledger()
    old = T0 - FAILURE_MEMORY_S - 100

    def card(iid, play="isr.investigate", state="voided", **kw):
        led.record(iid, play=play, version="1", target=f"T-{iid}", uid=3, task="orbit", request={}, t=old, state=state,
                   basis=kw.pop("basis", {}), withdraw_if=[], **kw)

    card(1)                                   # nothing refers to it: pruned
    card(2)                                   # a cooldown in force refers to it: kept
    led.cooldowns.append({"scope": "isr.investigate|T-2", "until_t": T0 + 100, "why": "rejected", "intent": 2})
    card(3, state="done")                     # an orbit a vehicle still flies: kept
    card(4, state="done")                     # the order an open card would resume: kept
    card(5, state="pending_approval", prior_task={"intent_id": 4, "running": True})
    card(6, play="safety.low_reserve", state="done")  # low_reserve's memory: kept
    card(7, state="failed")                   # a back-off in force names it: kept
    led.backoff["vehicle:3"] = {"fails": 1, "until_t": T0 + 50, "why": "i7 failed", "intent": 7}
    led.record(8, play="isr.investigate", version="1", target="T-8", uid=3, task="orbit", request={}, t=T0 - 30,
               state="voided", basis={}, withdraw_if=[])  # ended 30 s ago: a retry may still name it
    assert led.prunable(T0, running={3}) == ["1"]
    led.prune(T0, running={3})
    assert sorted(led.entries, key=int) == ["2", "3", "4", "5", "6", "7", "8"]


# ---- 4: the budget wait's backstop, and our own cards adopted without a record ---------------------------------


def test_r3_4_a_budget_wait_ends_by_the_approval_wait_plus_a_margin_and_bare_cards_are_adopted():
    led = Ledger()
    led.set_budget_wait("isr.investigate|T-3", T0, None, [201])
    assert led.budget_wait_for("isr.investigate|T-3", T0 + 100, {201}, 150.0) is not None
    assert led.budget_wait_for("isr.investigate|T-3", T0 + 150, {201}, 150.0) is None, "ends even if i201 reads stale"
    # an intent of ours the ledger does not know (no evidence record): adopted when the counts disagree
    ds = led.sync({201: intent(201, "orbit", [4], "pending_approval", rationale="an older engine's words")}, T0, config())
    assert led.entries["201"]["play"] == "adopted" and led.entries["201"]["uid"] == 4 and ds == []
    led2 = Ledger()
    led2.record(301, play="isr.investigate", version="1", target="T-1", uid=3, task="orbit", request={}, t=T0,
                state="pending_approval", basis={}, withdraw_if=[])
    led2.sync({301: intent(301, "orbit", [3], "pending_approval"), 302: intent(302, "hold", [9], "done", rationale="x")},
              T0, config())
    assert "302" not in led2.entries, "counts agree: nothing to adopt"


# ---- 5: carried cards whose target is a track id from before a restart ------------------------------------------


def test_r3_5_a_carried_card_on_a_track_id_from_before_a_restart_is_withdrawn_pending_and_left_running():
    led = Ledger(run="run-A")
    led.cursor = "old:5"
    led.record(5, play="isr.investigate", version="1", target="T-0008", uid=3, task="orbit",
               request={"task": "orbit", "uids": [3]}, t=T0 - 60, state="pending_approval", basis={}, withdraw_if=[])
    led.record(6, play="isr.investigate", version="1", target="T-0003", uid=4, task="orbit",
               request={"task": "orbit", "uids": [4], "center": offset(-2000, 0), "radius_m": 1000.0}, t=T0 - 60,
               state="dispatched", basis={}, withdraw_if=[])
    running = vehicle(4, "PUMA 2", "puma", offset(-2000, 900), mode="AUTO", task={"intent_id": 6, "task": "orbit", "state": "done"})
    # the new tracker's T-0003 is another contact: neutral, and far from the old orbit's centre
    new_tracks = [track("T-0003", "neutral", 0.9, kind="ferry", pos=offset(1500, 0)), track("T-0008", "suspect", 0.7)]
    ticks = [reads(t=T0 + dt, vehicles=[vehicle(3, "PUMA 1", "puma", offset(-1200, 200)), running], tracks=new_tracks,
                   intents=[intent(5, "orbit", [3]), intent(6, "orbit", [4], "dispatched")]) for dt in (0, 2, 4)]
    s = FakeSession(ticks, run="run-B", feed="new")
    s.explained = {5: ours(5, "orbit", [3]), 6: ours(6, "orbit", [4], "dispatched")}
    s.proposed = [{"id": 5, "request": {"task": "orbit", "uids": [3]}, "rationale": "", "state": "pending_approval"},
                  {"id": 6, "request": {"task": "orbit", "uids": [4]}, "rationale": "", "state": "dispatched"}]
    lines: list[dict] = []
    e = engine(led, lines)

    async def go():
        await e.start(s)
        return [await e.tick(s) for _ in ticks]

    reps = asyncio.run(go())
    w = [a for n, a in s.calls if n == "withdraw_intent"]
    assert [(a["id"], a["if_state"]) for a in w] == [(5, "pending_approval")]
    assert w[0]["reason"] == "hygiene.withdraw_stale: isr.investigate: track ids restarted with the bridge"
    assert not [p for p in s.proposed if p["id"] > 6 and p["request"]["uids"] == [4]], "no release or recentre of i6"
    assert e.led.cooldowns == [] or all(c["scope"] != "isr.investigate|T-0008" for c in e.led.cooldowns), \
        "no cooldown on the new run's T-0008"
    v = [x["text"] for x in lines if x["kind"] == "ledger.verified"][0]
    assert "card(s) i5, i6 kept with an unknown target (track ids restarted with the bridge)" in v
    # the new run's T-0008 gets a look of its own once i5 is gone (no cooldown held it)
    assert [(p["request"]["task"], p["request"]["uids"]) for p in s.proposed if p["id"] > 6] == [("orbit", [3])]
    assert e.led.entries["6"].get("stale_target") is True
