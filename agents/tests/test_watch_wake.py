"""The trigger table, coalescing and budgets (agents/watch/wake.py), on a made-up clock."""

from __future__ import annotations

from watch.picture import compose, render, render_mine
from watch.wake import TRIGGERS, Budgets, Waker, classify


def test_the_table_wakes_and_escalates_as_documented():
    cases = [
        ({"kind": "track.declared", "track": "T-1", "declared": "hostile"}, (True, True)),
        ({"kind": "track.declared", "track": "T-1", "declared": "neutral"}, (True, False)),
        ({"kind": "nomination.possible", "track": "T-1"}, (True, True)),
        ({"kind": "track.new", "affiliation": "suspect"}, (True, False)),
        ({"kind": "track.new", "affiliation": "neutral"}, (False, False)),
        ({"kind": "track.affiliation", "from": "unknown", "to": "suspect"}, (True, False)),
        ({"kind": "track.affiliation", "from": "neutral", "to": "assumed_friend"}, (False, False)),
        ({"kind": "track.identity_conflict", "conflict": "AIS says tanker"}, (True, True)),
        ({"kind": "vehicle.link_lost", "armed": True}, (True, True)),
        ({"kind": "vehicle.link_lost", "armed": False}, (True, False)),
        ({"kind": "vehicle.gps_glitch", "glitch": True}, (True, True)),
        ({"kind": "vehicle.gps_glitch", "glitch": False}, (False, False)),
        ({"kind": "vehicle.nav", "domain": "air", "to": {"ready": True, "source": "dead_reckoning"}}, (True, False)),
        ({"kind": "vehicle.nav", "domain": "subsurface", "to": {"ready": True, "source": "dead_reckoning"}},
         (False, False)),
        ({"kind": "vehicle.nav", "domain": "subsurface", "to": {"ready": False, "source": "none"}}, (True, False)),
        ({"kind": "intent.state", "mine": True, "to": "rejected"}, (True, False)),
        ({"kind": "intent.state", "mine": False, "to": "rejected"}, (False, False)),
        ({"kind": "intent.new", "mine": True}, (False, False)),
        ({"kind": "alert.raised", "severity": "high"}, (True, False)),
        ({"kind": "alert.raised", "severity": "medium"}, (False, False)),
        ({"kind": "vehicle.event", "event_kind": "failsafe"}, (True, False)),
        ({"kind": "vehicle.event", "event_kind": "mode"}, (False, False)),
        ({"kind": "vehicle.joined"}, (False, False)),
        ({"kind": "feed.reset"}, (True, False)),
    ]
    for change, want in cases:
        wakes, escalates, why = classify(change)
        assert (wakes, escalates) == want, change
        assert (why is not None) == wakes
    assert len({r.kind for r in TRIGGERS}) == len(TRIGGERS), "one row per kind"


def quiet(**kw) -> Budgets:
    base = dict(heartbeat_s=0, coalesce_s=3, coalesce_max_s=10, min_gap_s=10, max_wakes_per_hour=60)
    base.update(kw)
    return Budgets(**base)


def test_the_first_wake_is_the_start_then_nothing_without_a_trigger():
    w = Waker(quiet())
    first = w.due(0)
    assert first.reason == "start" and first.n == 1
    w.add([{"kind": "vehicle.joined"}, {"kind": "track.new", "affiliation": "neutral"}], 20)
    assert w.due(30) is None, "notes alone never wake"
    assert len(w.pending) == 2


def test_a_burst_is_coalesced_into_one_wake_with_the_notes():
    w = Waker(quiet())
    w.due(0)
    w.add([{"kind": "vehicle.joined"}], 20)
    w.add([{"kind": "track.new", "affiliation": "suspect", "track": "T-1"}], 30)
    assert w.due(31) is None, "waits coalesce_s"
    w.add([{"kind": "track.affiliation", "track": "T-1", "from": "suspect", "to": "unknown"}], 32)
    assert w.due(33.5) is None, "still coming in"
    wake = w.due(35.1)
    assert wake.reason == "changes" and len(wake.changes) == 3 and not wake.escalate
    assert wake.why == ["a new contact that is not known to be friendly or neutral",
                        "a contact's affiliation estimate changed"]
    assert w.pending == []


def test_a_steady_stream_still_wakes_after_coalesce_max():
    w = Waker(quiet())
    w.due(0)
    for t in range(20, 32):
        w.add([{"kind": "track.new", "affiliation": "unknown", "track": f"T-{t}"}], t)
        if t < 30:
            assert w.due(t + 0.5) is None
    assert w.due(30.5).reason == "changes"


def test_a_condition_that_came_and_went_in_the_batch_is_a_note():
    w = Waker(quiet())
    w.due(0)
    # an AUV's acoustic link drops for two seconds (as seen live), with its alert
    w.add([{"kind": "vehicle.link_lost", "uid": 6, "armed": False},
           {"kind": "alert.raised", "alert": "link:6", "severity": "high", "uid": 6}], 20)
    w.add([{"kind": "vehicle.link_restored", "uid": 6},
           {"kind": "alert.cleared", "alert": "link:6", "severity": "high", "uid": 6}], 22)
    assert w.due(26) is None and w.first_wake_t is None
    assert len(w.pending) == 4, "kept as notes for the next update"
    # an armed platform's link that stays lost still wakes, and escalates
    w.add([{"kind": "vehicle.link_lost", "uid": 3, "armed": True}], 30)
    wake = w.due(34)
    assert wake.escalate and wake.why == ["a vehicle lost its link"] and len(wake.changes) == 5
    # lost, back, lost again: in force at the end of the batch
    w.add([{"kind": "vehicle.link_lost", "uid": 6}, {"kind": "vehicle.link_restored", "uid": 6},
           {"kind": "vehicle.link_lost", "uid": 6}], 50)
    assert w.due(54).why == ["a vehicle lost its link"]


def test_min_gap_between_wakes():
    w = Waker(quiet())
    w.due(0)
    w.add([{"kind": "review.flag"}], 1)
    assert w.due(5) is None, "10 s since the last wake not yet"
    assert w.due(10.1).reason == "changes"


def test_escalation_goes_to_the_stronger_model_within_its_budget():
    w = Waker(quiet(max_escalations=1))
    w.due(0)
    w.add([{"kind": "track.declared", "track": "T-1", "declared": "hostile", "by": "rule:hostile_act"}], 20)
    wake = w.due(24)
    assert wake.escalate and wake.escalate_why == ["a track was declared"]
    w.add([{"kind": "vehicle.link_lost", "armed": True, "uid": 3}], 40)
    wake = w.due(44)
    assert not wake.escalate, "the run's escalations are spent: the routine model takes it"
    assert w.escalations == 1
    assert not Waker(quiet(), escalation=False).escalation


def test_the_hourly_cap_defers_routine_wakes_but_not_an_escalation():
    w = Waker(quiet(max_wakes_per_hour=2, max_escalations=3))
    w.due(0)
    w.add([{"kind": "review.flag"}], 20)
    assert w.due(24).n == 2
    w.add([{"kind": "review.flag"}], 40)
    assert w.due(44) is None and w.deferred, "two an hour"
    w.add([{"kind": "vehicle.gps_glitch", "glitch": True, "uid": 4}], 50)
    wake = w.due(54)
    assert wake is not None and wake.escalate, "a consequential moment is not held back"
    assert len(wake.changes) == 2, "the deferred change rides along"
    w.add([{"kind": "review.flag"}], 3700)
    assert w.due(3704) is not None, "an hour later there is room again"


def test_the_run_cap():
    w = Waker(quiet(max_wakes=2))
    w.due(0)
    w.add([{"kind": "review.flag"}], 20)
    w.due(24)
    assert w.exhausted()
    w.add([{"kind": "review.flag"}], 40)
    assert w.due(50) is None


def test_the_heartbeat_backs_off_while_nothing_changes():
    w = Waker(quiet(heartbeat_s=60, heartbeat_max_s=300))
    w.due(0)
    assert w.due(59) is None
    assert w.due(60).reason == "heartbeat"
    assert w.heartbeat_every() == 120
    assert w.due(150) is None
    assert w.due(180).reason == "heartbeat"
    assert w.heartbeat_every() == 240
    w.add([{"kind": "vehicle.joined"}], 200)  # a note: the heartbeat comes back to 60 s
    assert w.heartbeat_every() == 60
    wake = w.due(240)
    assert wake.reason == "heartbeat" and len(wake.changes) == 1


def test_the_update_lists_changes_proposals_and_budget():
    w = Waker(quiet())
    w.due(0)
    w.add([{"kind": "track.new", "t": 1791500000, "track": "T-0004", "domain": "surface",
            "identity": "small_fast_boat", "affiliation": "suspect", "affiliation_confidence": 0.62,
            "position": [-122.48, 37.81], "speed_mps": 4.1, "course_deg": 270,
            "sources": [{"uid": 7, "sensor": "radar"}]},
           {"kind": "intent.state", "t": 1791500001, "intent": 12, "task": "orbit", "uids": [5], "mine": True,
            "from": "pending_approval", "to": "rejected", "reason": "operator: not now"}], 20)
    wake = w.due(30)
    mine = render_mine([{"id": 12, "kind": "orbit", "uids": [5], "state": "rejected",
                         "history": [{"t": 1791500001000, "reason": "operator: not now"}]},
                        {"id": 13, "kind": "hold", "uids": [2], "state": "pending_approval", "expires_in_s": 84.0,
                         "history": [{"t": 1791500002000, "reason": "waits"}]}], 1791500010)
    text = compose(wake, mine, "Budget: a line.", ["A consult note."], 1791500010)
    assert "Watch update 2" in text and "T-0004: surface small_fast_boat" in text and "suspect 0.62" in text
    assert "pending_approval -> rejected (operator: not now)" in text
    assert "#13 hold on [2]: pending_approval, expires in 84 s" in text
    assert "A consult note." in text and "Budget: a line." in text
    assert "\u2014" not in text
    assert render({"kind": "vehicle.gps_glitch", "uid": 3, "callsign": "X", "armed": True, "glitch": True,
                   "platform": "fpv"}).endswith("uid 3 X (fpv, ARMED) GPS glitch FLAGGED by its navigation filter")
