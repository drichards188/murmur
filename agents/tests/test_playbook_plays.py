"""Each play fires on its trigger and stays silent in its negative cases (murmur-t8m): a neutral
ferry, an assumed friend alongside, a stale track, a suspect below 0.5 for armed plays. Pure:
beliefs and plays on synthetic and recorded reads, no session."""

from __future__ import annotations

import pytest

from playbook.beliefs import CLASSIFYING, build
from playbook.ledger import Ledger
from playbook.plays import PLAYS
from playbook_world import (GATE, T0, config, intent, offset, reads, recorded, recorded_reads, track, vehicle)


def look(r, cfg=None, led=None, play="isr.investigate"):
    cfg = cfg or config()
    led = led or Ledger()
    b = build(r, cfg, led)
    return PLAYS[play].evaluate(b, led), b


FLEET = [vehicle(1, "HUNTER 1", "seahunter", offset(1000, -300), task={"intent_id": 1, "task": "patrol", "state": "active"}),
         vehicle(3, "PUMA 1", "puma", offset(-1200, 200)),
         vehicle(4, "PUMA 2", "puma", offset(800, 900), task={"intent_id": 3, "task": "orbit", "state": "done"})]


# ---- isr.investigate ---------------------------------------------------------------------------------


def test_investigate_fires_on_the_recorded_fast_boat_closing_on_the_bridge():
    """The live Harbor watch read at the reader's 176 s: the unknown boat, now suspect at 0.51,
    8.4 m/s toward the bridge (CPA 124 m in 9.4 min). PUMA 1, the free Puma, takes it."""
    doc = recorded()
    led, cfg = Ledger(), config()
    for i in (0, 1):  # the engine keeps each track's recent classification (ledger.see_classification)
        r = recorded_reads(doc, i)
        led.see_classification(r.tracks["tracks"], r.tick_t, cfg, CLASSIFYING)
    res, _ = look(recorded_reads(doc, 1), led=led)
    assert [(c.target, c.uid, c.request["radius_m"]) for c in res.candidates] == [("T-0008", 3, 1000.0)]
    c = res.candidates[0]
    assert c.request["task"] == "orbit" and c.claims == (3,)
    # classification 0.51 is no reason of its own here: REAPER 1's EO/IR and HUNTER 1's sonar have
    # it and it rose from 0.44 five seconds before (sensor coverage, murmur-t8m fix rounds 1 and
    # 2); fast and closing on the bridge are
    assert c.trigger == ["closing on the Golden Gate Bridge", "fast: 8.4 m/s over 8"]
    assert c.derived["closing"] and c.derived["cpa_m"] < 457 and c.derived["tcpa_s"] < 600
    assert [a["callsign"] for a in c.alternatives] == ["REAPER 1", "PUMA 2"], "busy, with why"
    assert all("not_free" in a for a in c.alternatives)
    # five seconds before, the same boat was not yet worth it (no closest approach inside the alert)
    assert look(recorded_reads(doc, 0))[0].candidates == []


def test_investigate_takes_an_unknown_with_no_classification_inside_the_area():
    """An unknown at affiliation confidence 0.0 (no classification yet) is exactly what a look is
    for: the 0.5 gate is the track's existence confidence, not its affiliation's."""
    t = track("T-1", "unknown", 0.0, pos=offset(-1500, 0), vel=(0.0, 0.0), kind=None, kind_conf=0.0)
    res, _ = look(reads(vehicles=FLEET, tracks=[t]))
    assert [c.target for c in res.candidates] == ["T-1"]
    assert res.candidates[0].trigger == ["inside the Golden Gate approaches",
                                         "classification 0.00 under 0.60, no EO/IR or sonar of ours on it"]


def test_investigate_does_not_look_for_classification_at_a_track_our_eo_ir_already_has():
    """Sensor coverage (fix round 1, item 13): a slow unknown inside the area that an EO/IR or a
    sonar of ours detected in the last 10 s is being classified already; one seen only by radar,
    or by EO/IR longer ago, is not. Fast and closing are reasons whatever sees the track."""
    def unknown(sources, conf=0.05):
        return track("T-1", "unknown", 0.0, pos=offset(-1500, 0), vel=(0.0, 0.0), kind=None, kind_conf=conf, sources=sources)

    def candidates(sources, fast=False):
        led, cfg = Ledger(), config()
        led.see_classification([unknown(sources, 0.0)], T0 - 10, cfg, CLASSIFYING)  # rising: 0.0, then 0.05
        tr = track("T-1", "suspect", 0.7, pos=offset(-1500, 0), vel=(9.0, 0.0), sources=sources) if fast else unknown(sources)
        led.see_classification([tr], T0, cfg, CLASSIFYING)
        return look(reads(vehicles=FLEET, tracks=[tr]), led=led)[0].candidates

    eo_now = [{"uid": 1, "sensor": "eo_ir", "hits": 12, "last_t": T0 - 3.0}]
    eo_old = [{"uid": 1, "sensor": "eo_ir", "hits": 12, "last_t": T0 - 30.0}]
    sonar_now = [{"uid": 1, "sensor": "sonar", "hits": 2, "last_t": T0 - 1.0}]
    radar = [{"uid": 1, "sensor": "radar", "hits": 40, "last_t": T0 - 1.0}]
    assert candidates(eo_now) == []
    assert candidates(sonar_now) == []
    for src in (eo_old, radar):
        assert [c.target for c in candidates(src)] == ["T-1"]
    c = candidates(eo_now, fast=True)
    assert c and "fast: 9.0 m/s over 8" in c[0].trigger


@pytest.mark.parametrize("case,t", [
    ("neutral ferry", track("T-2", "neutral", 0.9, kind="ferry", ais={"name": "MV GOLDEN BEAR"}, vel=(10.0, 0.0))),
    ("assumed friend", track("T-3", "assumed_friend", 0.9, kind="patrol_boat", ais={"name": "CGC TERN"})),
    ("stale track", track("T-4", "suspect", 0.8, stale=True, age_s=25.0)),
    ("barely there", track("T-5", "suspect", 0.8, confidence_now=0.3)),
    ("outside the watch", track("T-6", "suspect", 0.8, pos=offset(-9000, 0), vel=(0.0, 5.0))),
    ("classified, slow, consistent", track("T-7", "suspect", 0.9, kind_conf=0.9, vel=(2.0, 0.0))),
    ("declared", track("T-8", "suspect", 0.9, declared={"id": 1, "affiliation": "hostile", "by": "op-1"})),
])
def test_investigate_stays_silent(case, t):
    res, _ = look(reads(vehicles=FLEET, tracks=[t]))
    assert res.candidates == [], case


def test_investigate_says_what_blocks_it_when_no_sensor_is_free():
    busy = [dict(v) for v in FLEET]
    busy[1] = vehicle(3, "PUMA 1", "puma", offset(-1200, 200), mode="RTL")
    res, _ = look(reads(vehicles=busy, tracks=[track("T-1", "suspect", 0.7)]))
    assert res.candidates == []
    assert res.blocked == [("T-1", "no free sensor aircraft (PUMA 1: recovering on its own (RTL); PUMA 2: on orbit i3 (done))")]


def test_investigate_prefers_the_configured_platform_then_eta_battery_uid():
    far_puma = vehicle(5, "PUMA 3", "puma", offset(-6000, 0))
    near_reaper = vehicle(2, "REAPER 1", "mq9b", offset(-1800, -300))
    res, _ = look(reads(vehicles=[far_puma, near_reaper], tracks=[track("T-1", "suspect", 0.7)]))
    assert res.candidates[0].uid == 5, "the preferred platform first"
    res, _ = look(reads(vehicles=[far_puma, near_reaper], tracks=[track("T-1", "suspect", 0.7)]), cfg=config(sensor_platform=""))
    assert res.candidates[0].uid == 2 and res.candidates[0].request["radius_m"] == 10000.0, "nearest by ETA; a quarter of 40 km"
    twins = [vehicle(7, "PUMA 7", "puma", offset(-1200, 0), battery_pct=60.0), vehicle(6, "PUMA 6", "puma", offset(-1200, 0), battery_pct=60.0)]
    res, _ = look(reads(vehicles=twins, tracks=[track("T-1", "suspect", 0.7)]))
    assert res.candidates[0].uid == 6, "a tie by ETA and battery goes to the lower uid"


def test_investigate_prefers_a_vehicle_whose_predicted_link_holds_and_says_why():
    """Version 2: the predicted link of each choice's orbit (reads.links, get_link_coverage) ranks
    first; the rationale says the link and whom the look was chosen over for theirs. A link not
    known counts as holding; when none holds the look still goes, saying so."""
    near, far = vehicle(3, "PUMA 1", "puma", offset(-1200, 200)), vehicle(5, "PUMA 3", "puma", offset(-6000, 0))
    r = reads(vehicles=[near, far], tracks=[track("T-1", "suspect", 0.7)])
    res, _ = look(r)
    assert [c.uid for c in res.candidates] == [3, 5], "without a prediction: by ETA, as version 1"
    assert "link" not in res.candidates[0].derived
    r.links = {"3|T-1": {"holds": False, "p_failsafe": 1.0,
                         "sentence": "PUMA 1 would lose its link 2.0 km along the transit leg (behind terrain from the "
                                     "ground station), for about 50 s; its failsafe trips after 5 s."},
               "5|T-1": {"holds": True, "p_failsafe": 0.0,
                         "sentence": "PUMA 3's link is predicted to hold on the transit leg and the task path (loss at most 1 %)."}}
    res, _ = look(r)
    assert [c.uid for c in res.candidates] == [5, 3], "the one whose link holds first"
    first = res.candidates[0]
    assert first.derived["link"]["holds"] is True and first.derived["link_passed_over"] == ["PUMA 1"]
    assert first.alternatives[0]["uid"] == 3 and first.alternatives[0]["link_holds"] is False
    text = PLAYS["isr.investigate"].render(first.record("test", T0))
    assert "Predicted link: PUMA 3's link is predicted to hold" in text and "Chosen over PUMA 1, whose predicted link would not hold." in text
    # none holds: the look still goes, by the order before, and says so
    r.links["5|T-1"] = dict(r.links["5|T-1"], holds=False, sentence="PUMA 3 would lose its link on the task path.")
    res, _ = look(r)
    assert res.candidates[0].uid == 3 and res.candidates[0].derived["link_passed_over"] == []
    text = PLAYS["isr.investigate"].render(res.candidates[0].record("test", T0))
    assert "No free sensor aircraft's predicted link holds there." in text
    # one not known counts as holding
    del r.links["5|T-1"]
    res, _ = look(r)
    assert res.candidates[0].uid == 5 and "link" not in res.candidates[0].derived


def test_link_queries_ask_for_every_free_sensor_aircraft_on_each_look():
    from playbook.plays.isr import link_queries

    near, far = vehicle(3, "PUMA 1", "puma", offset(-1200, 200)), vehicle(5, "PUMA 3", "puma", offset(-6000, 0))
    r = reads(vehicles=[near, far, FLEET[0]], tracks=[track("T-1", "suspect", 0.7), track("T-2", "neutral", 0.9)])
    b = build(r, config(), Ledger())
    q = link_queries(b, Ledger())
    assert [k for k, _ in q] == ["3|T-1", "5|T-1"], "a neutral is not worth a look; the boat is no sensor aircraft"
    assert q[0][1] == {"uid": 3, "brief": True,
                       "order": {"task": "orbit", "center": track("T-1")["predicted"], "radius_m": 1000.0}}


def test_investigate_needs_a_watch_area_or_a_protected_point():
    res, _ = look(reads(vehicles=FLEET, tracks=[track("T-1")]), cfg=config(watch_area=[], protected=[]))
    assert res.candidates == [] and res.blocked == [("-", "no watch area or protected point is configured")]


# ---- isr.armed_overwatch -------------------------------------------------------------------------------

ARMED_FLEET = FLEET + [vehicle(9, "BLADE 1", "switchblade", offset(-2500, 500))]


def overwatch(tracks, **cfg_over):
    return look(reads(vehicles=ARMED_FLEET, tracks=tracks), cfg=config(enable=["isr.armed_overwatch"], **cfg_over),
                play="isr.armed_overwatch")


def test_armed_overwatch_is_off_unless_enabled():
    assert "isr.armed_overwatch" not in config()["plays"]
    assert "isr.armed_overwatch" in config(enable=["isr.armed_overwatch"])["plays"]


def test_armed_overwatch_fires_on_a_suspect_at_half_or_more_closing_on_the_bridge():
    res, _ = overwatch([track("T-1", "suspect", 0.62, pos=offset(-2000, 0), vel=(10.0, 0.0))])
    assert [(c.target, c.uid, c.armed) for c in res.candidates] == [("T-1", 9, True)]


@pytest.mark.parametrize("case,tracks,why", [
    ("suspect below 0.5", [track("T-1", "suspect", 0.45, pos=offset(-2000, 0), vel=(10.0, 0.0))], "suspect at 0.45, under 0.5"),
    ("assumed friend alongside",
     [track("T-1", "suspect", 0.8, pos=offset(-2000, 0), vel=(10.0, 0.0)),
      track("T-9", "assumed_friend", 0.9, pos=offset(-1950, 40), vel=(10.0, 0.0), kind="patrol_boat")],
     "T-9 shown assumed_friend"),
    ("neutral ferry", [track("T-2", "neutral", 0.9, pos=offset(-2000, 0), vel=(10.0, 0.0), kind="ferry")], None),
    ("stale track", [track("T-4", "suspect", 0.9, pos=offset(-2000, 0), vel=(10.0, 0.0), stale=True, age_s=20.0)], None),
    ("not closing", [track("T-5", "suspect", 0.9, pos=offset(-2000, 0), vel=(0.0, 10.0))], None),
])
def test_armed_overwatch_stays_silent(case, tracks, why):
    res, _ = overwatch(tracks)
    assert res.candidates == [], case
    if why:
        assert why in res.blocked[0][1], res.blocked


def test_armed_overwatch_never_takes_a_platform_whose_navigation_is_suspect():
    glitchy = ARMED_FLEET[:-1] + [vehicle(9, "BLADE 1", "switchblade", offset(-2500, 500), gps_glitch=True)]
    res, _ = look(reads(vehicles=glitchy, tracks=[track("T-1", "suspect", 0.8, pos=offset(-2000, 0), vel=(10.0, 0.0))]),
                  cfg=config(enable=["isr.armed_overwatch"]), play="isr.armed_overwatch")
    assert res.candidates == [] and "GPS glitch flagged" in res.blocked[0][1]


# ---- isr.recentre and isr.release -------------------------------------------------------------------------


def running(led: Ledger, iid: int, tid: str, uid: int, center: list[float], radius: float = 1000.0, play="isr.investigate"):
    led.record(iid, play=play, version="1", target=tid, uid=uid, task="orbit",
               request={"task": "orbit", "uids": [uid], "center": center, "radius_m": radius}, t=T0 - 60, state="done",
               basis={"affiliation": "suspect", "aff_conf": 0.7, "declared": None, "cpa_m": 100.0}, withdraw_if=[])


def test_recentre_moves_a_running_orbit_once_the_track_is_half_a_radius_off_and_not_again_within_60_s():
    led = Ledger()
    running(led, 7, "T-1", 3, offset(-2600, 0))
    fleet = [vehicle(3, "PUMA 1", "puma", offset(-2600, 1000), mode="AUTO", task={"intent_id": 7, "task": "orbit", "state": "done"})]
    t = track("T-1", "suspect", 0.8, pos=offset(-2000, 0), vel=(10.0, 0.0))
    res, _ = look(reads(vehicles=fleet, tracks=[t]), led=led, play="isr.recentre")
    assert [(c.uid, c.supersedes) for c in res.candidates] == [(3, 7)]
    assert res.candidates[0].request["center"] == t["predicted"]
    # within half a radius: nothing
    near = track("T-1", "suspect", 0.8, pos=offset(-2300, 0), vel=(10.0, 0.0))
    assert look(reads(vehicles=fleet, tracks=[near]), led=led, play="isr.recentre")[0].candidates == []
    # recentred 30 s ago: held back by the hysteresis
    led.recentred["3"] = T0 - 30
    res, _ = look(reads(vehicles=fleet, tracks=[t]), led=led, play="isr.recentre")
    assert res.candidates == [] and "within the last 60 s" in res.blocked[0][1]


def test_release_holds_a_vehicle_whose_track_is_shown_neutral_and_leaves_it_free():
    led = Ledger()
    running(led, 7, "T-1", 3, offset(-2000, 0))
    fleet = [vehicle(3, "PUMA 1", "puma", offset(-2000, 900), mode="AUTO", task={"intent_id": 7, "task": "orbit", "state": "done"})]
    neutral = track("T-1", "neutral", 0.82, kind="fishing_vessel", vel=(0.5, 0.0), reason="sport fishing boat")
    res, _ = look(reads(vehicles=fleet, tracks=[neutral]), led=led, play="isr.release")
    assert [(c.task, c.uid, c.supersedes) for c in res.candidates] == [("hold", 3, 7)]
    assert "shown neutral at 0.82" in res.candidates[0].trigger[0]
    # the track still suspect: no release
    assert look(reads(vehicles=fleet, tracks=[track("T-1")]), led=led, play="isr.release")[0].candidates == []
    # once our hold runs, the vehicle is free again for a new look
    led.record(8, play="isr.release", version="1", target="T-1", uid=3, task="hold", request={"task": "hold", "uids": [3]},
               t=T0, state="done", basis={}, withdraw_if=[])
    held = [vehicle(3, "PUMA 1", "puma", offset(-2000, 900), task={"intent_id": 8, "task": "hold", "state": "done"})]
    b = build(reads(vehicles=held, tracks=[]), config(), led)
    assert b.free_reason(b.vehicles[3]) is None


# ---- links and navigation ---------------------------------------------------------------------------------


def gap_world(lost_s: float, lost_after: float = 3.0):
    station = offset(500, 900)
    fleet = [vehicle(4, "PUMA 2", "puma", station, mode="AUTO", link_lost=True, link_age_s=lost_s,
                     task={"intent_id": 3, "task": "orbit", "state": "done"}),
             vehicle(3, "PUMA 1", "puma", offset(-1200, 200))]
    alerts = [{"id": "link:4", "kind": "lost_link", "uid": 4, "severity": "high", "since_t": T0 - lost_s, "detail": "no telemetry"}]
    led = Ledger()
    led.params["3"] = {"task": "orbit", "params": {"center": station, "radius_m": 300.0}, "source": "human op-setup", "play": None}
    r = reads(vehicles=fleet, alerts=alerts, lost_full={4: {"uid": 4, "link": {"lost_after_s": lost_after}}})
    return r, led, station


def test_cover_gap_puts_a_free_vehicle_on_a_silent_vehicles_orbit():
    r, led, station = gap_world(7.0)
    res, _ = look(r, led=led, play="link.cover_gap")
    assert [(c.target, c.uid, c.request) for c in res.candidates] == [
        ("gap:4", 3, {"task": "orbit", "uids": [3], "center": station, "radius_m": 300.0})]
    assert res.candidates[0].withdraw_if[0] == "PUMA 2's link is back (a filler already running is kept)"


def test_cover_gap_waits_out_a_flap_and_leaves_our_own_looks_to_investigate():
    r, led, _ = gap_world(5.0)  # under 2 x 3 s
    assert look(r, led=led, play="link.cover_gap")[0].candidates == []
    r, led, _ = gap_world(30.0)
    del led.params["3"]
    running(led, 3, "T-1", 4, offset(500, 900))
    assert look(r, led=led, play="link.cover_gap")[0].candidates == [], "our orbit on a track: investigate's"


def test_cover_gap_says_so_when_no_vehicle_can_fill():
    r, led, _ = gap_world(30.0)
    r.fleet["vehicles"][1]["mode"] = "RTL"
    res, _ = look(r, led=led, play="link.cover_gap")
    assert res.candidates == [] and "no free capable vehicle (PUMA 1: recovering on its own (RTL))" in res.blocked[0][1]


def test_withdraw_pending_and_nav_suspect_exclude():
    led = Ledger()
    led.record(11, play="isr.investigate", version="1", target="T-1", uid=3, task="orbit", request={}, t=T0, state="pending_approval",
               basis={}, withdraw_if=[])
    led.record(12, play="isr.armed_overwatch", version="1", target="T-1", uid=9, task="orbit", request={}, t=T0,
               state="pending_approval", basis={}, withdraw_if=[], armed=True)
    fleet = [vehicle(3, "PUMA 1", link_lost=True, link_age_s=4.0), vehicle(9, "BLADE 1", "switchblade", nav_source="dead_reckoning")]
    alerts = [{"id": "link:3", "kind": "lost_link", "uid": 3, "since_t": T0 - 4.0}]
    b = build(reads(vehicles=fleet, alerts=alerts, tracks=[track("T-1")]), config(), led)
    waiting = led.open_entries()
    w = PLAYS["link.withdraw_pending"].withdrawals(b, led, waiting)
    assert [(x.intent, x.reason) for x in w] == [(11, "PUMA 1's link lost at 14:38:16Z (silent 4 s)")]
    n = PLAYS["nav.suspect_exclude"].withdrawals(b, led, waiting)
    assert [(x.intent, x.reason) for x in n] == [(12, "BLADE 1's navigation is suspect (navigation on dead_reckoning): excluded from armed tasks")]
    assert b.free_reason(b.vehicles[9]).startswith("navigation on dead_reckoning")
    # a card the operator already approved, queued for the link (rules 5), is the queue's: left
    led.entries["11"]["state"] = "queued"
    assert PLAYS["link.withdraw_pending"].withdrawals(b, led, led.open_entries()) == []


def test_an_order_back_in_the_queue_is_not_flying_and_claims_nothing():
    """Live, Harbor watch: our recentre went out, PUMA 1's link dropped before it started, the
    bridge put it back in the queue; PUMA 1's current order then read `requeued`."""
    led = Ledger()
    running(led, 5, "T-1", 3, offset(-2000, 0), play="isr.recentre")
    fleet = [vehicle(3, "PUMA 1", "puma", offset(-2000, 900), mode="LOITER", task={"intent_id": 5, "task": "orbit", "state": "requeued"})]
    b = build(reads(vehicles=fleet, tracks=[track("T-1")]), config(), led)
    assert b.claims == {} and b.free_reason(b.vehicles[3]) is None


def test_nav_suspect_clears_only_after_60_s_of_good_navigation():
    led = Ledger()
    cfg = config()
    glitch = reads(vehicles=[vehicle(3, "PUMA 1", gps_glitch=True)], tracks=[track("T-1")])
    b = build(glitch, cfg, led)
    assert 3 in b.nav_suspect
    led.nav_bad_t["3"] = T0
    later = reads(t=T0 + 30, vehicles=[vehicle(3, "PUMA 1")], tracks=[track("T-1")])
    b = build(later, cfg, led)
    assert b.nav_suspect[3] == "suspect within the last 60 s"
    res = PLAYS["isr.investigate"].evaluate(b, led)
    assert res.candidates == [] and "navigation suspect" in res.blocked[0][1]
    clear = reads(t=T0 + 61, vehicles=[vehicle(3, "PUMA 1")], tracks=[track("T-1")])
    assert 3 not in build(clear, cfg, led).nav_suspect


# ---- safety ------------------------------------------------------------------------------------------------


def test_low_reserve_returns_a_vehicle_under_the_line_once():
    res, _ = look(reads(vehicles=[vehicle(3, "PUMA 1", battery_pct=22.0)]), play="safety.low_reserve")
    assert [(c.task, c.uid, c.klass) for c in res.candidates] == [("rtl", 3, 0)]
    for v in (vehicle(3, "PUMA 1", battery_pct=30.0), vehicle(3, "PUMA 1", battery_pct=22.0, mode="RTL"),
              vehicle(3, "PUMA 1", battery_pct=22.0, link_lost=True)):
        assert look(reads(vehicles=[v]), play="safety.low_reserve")[0].candidates == []


def stuck_world(seen_s, since_age=900.0, **over):
    """A not_moving alert the playbook first saw `seen_s` ago; its since_t is its order's last
    state change (`since_age` ago), which says nothing about when the vehicle stopped."""
    a = [{"id": "op:9", "kind": "not_moving", "uid": 4, "since_t": T0 - since_age,
          "detail": "NOT MOVING for 12s (aground or stuck?) · orbit"}]
    kw = {"mode": "AUTO", **over}
    led = Ledger()
    led.see_alerts(a, T0 - seen_s)
    return reads(vehicles=[vehicle(4, "PUMA 2", task={"intent_id": 3, "task": "orbit", "state": "active"}, **kw)], alerts=a), led


def test_stuck_holds_after_30_s_in_our_reads_but_never_a_vehicle_on_its_failsafe_return():
    r, led = stuck_world(31.0)
    res, _ = look(r, led=led, play="safety.stuck")
    assert [(c.task, c.uid, c.target) for c in res.candidates] == [("hold", 4, "alert:op:9")]
    text = PLAYS["safety.stuck"].render(res.candidates[0].record("test", T0))
    assert "active in our reads for 31 s (its detail \"NOT MOVING for 12s (aground or stuck?) · orbit\")" in text
    r, led = stuck_world(20.0)
    assert look(r, led=led, play="safety.stuck")[0].candidates == []
    for mode in ("RTL", "LOITER"):
        r, led = stuck_world(40.0, mode=mode)
        assert look(r, led=led, play="safety.stuck")[0].candidates == [], mode


def test_stuck_counts_from_the_first_read_that_showed_the_alert_not_from_since_t():
    """Fix round 1, item 4: since_t is when the stuck order last changed state (minutes before,
    for an order that ran a while). A hold on the first read would undercut the 30 s rule."""
    r, led = stuck_world(0.0, since_age=900.0)
    assert look(r, led=led, play="safety.stuck")[0].candidates == [], "first seen now: not yet"
    r, led = stuck_world(29.0, since_age=900.0)
    assert look(r, led=led, play="safety.stuck")[0].candidates == []
    r, led = stuck_world(30.0, since_age=30.0)
    assert len(look(r, led=led, play="safety.stuck")[0].candidates) == 1
    # an alert that goes away and comes back counts again from its return
    led = Ledger()
    a = [{"id": "op:9", "kind": "not_moving", "uid": 4}]
    led.see_alerts(a, T0 - 100)
    led.see_alerts([], T0 - 50)
    led.see_alerts(a, T0 - 10)
    assert led.alert_seen == {"op:9|not_moving": T0 - 10}


def test_stuck_acts_once_per_alert_while_its_hold_is_live():
    """Live, Harbor watch: PUMA 1's orbit upload failed on a lossy link, the hold went to the
    queue for the link, and the next tick proposed a second hold (a P0 command took the vehicle
    from our own queued hold). Now the play holds back while its own hold is live."""
    from playbook.arbiter import arbitrate

    led = Ledger()
    led.record(6, play="safety.stuck", version="1", target="alert:op:9", uid=4, task="hold", request={"task": "hold", "uids": [4]},
               t=T0, state="queued", basis={}, withdraw_if=[])
    alerts = [{"id": "op:9", "kind": "task_failed", "uid": 4, "since_t": T0 - 40, "detail": "mission upload unconfirmed"}]
    r = reads(vehicles=[vehicle(4, "PUMA 2", mode="AUTO", task={"intent_id": 3, "task": "orbit", "state": "failed"})], alerts=alerts,
              intents=[intent(6, "hold", [4], "queued")])
    res, b = look(r, led=led, play="safety.stuck")
    assert res.candidates == [] and res.active == ["PUMA 2: our hold"]
    # and a P0 command never takes a vehicle from another safety command of ours
    low = PLAYS["safety.low_reserve"].evaluate(build(reads(vehicles=[vehicle(4, "PUMA 2", battery_pct=20.0)], alerts=alerts,
                                                           intents=[intent(6, "hold", [4], "queued")]), config(), led), led)
    sel, held = arbitrate(low.candidates, b, led)
    assert sel == [] and held[0].reason == "uid 4 is our i6 (queued)"


# ---- hygiene --------------------------------------------------------------------------------------------------


def test_withdraw_stale_takes_back_a_card_whose_track_turned_neutral_or_went_unseen():
    led = Ledger()
    led.record(21, play="isr.investigate", version="1", target="T-1", uid=3, task="orbit", request={}, t=T0, state="pending_approval",
               basis={}, withdraw_if=[])
    fleet = [vehicle(3, "PUMA 1")]
    for t, why in ((track("T-1", "neutral", 0.8, kind="fishing_vessel"), "T-1 is shown neutral at 0.80"),
                   (track("T-1", "suspect", 0.8, age_s=70.0, stale=True), "T-1 is unseen for 70 s"),
                   (None, "T-1 was dropped by the tracker")):
        b = build(reads(vehicles=fleet, tracks=[t] if t else []), config(), led)
        w = PLAYS["hygiene.withdraw_stale"].withdrawals(b, led, led.open_entries())
        assert [(x.intent, x.reason) for x in w] == [(21, f"isr.investigate: {why}")]
    b = build(reads(vehicles=fleet, tracks=[track("T-1")]), config(), led)
    assert PLAYS["hygiene.withdraw_stale"].withdrawals(b, led, led.open_entries()) == []


def test_every_play_of_step_one_is_registered_with_its_class():
    want = {"isr.investigate": 2, "isr.recentre": 2, "isr.release": 2, "isr.armed_overwatch": 2, "link.cover_gap": 3,
            "link.withdraw_pending": 3, "nav.suspect_exclude": 3, "safety.low_reserve": 0, "safety.stuck": 0,
            "hygiene.withdraw_stale": 4}
    # the playbook's own plays: eval/headless/agents_v2.py registers its look-at-ferries perturbation's
    # play when imported (inert unless a config enables it), so a run of every suite may see it here
    own = {n: p.klass for n, p in PLAYS.items() if type(p).__module__.startswith("playbook.")}
    assert own == want
    assert intent(1, "orbit", [3])["uids"] == ["3"], "the bridge serves an intent's uids as strings"
    assert GATE == (-122.4783, 37.8199)


def test_every_play_renders_its_rationale_from_its_record():
    """Each proposing play's template on a real candidate: the play and version named, the
    numbers the record holds, the record after the delimiter."""
    from playbook import rationale as R
    from playbook.engine import Engine

    r, led, _ = gap_world(7.0)
    res, b = look(r, led=led, play="link.cover_gap")
    text, rec = Engine(config()).finish(res.candidates[0], b, None, [])
    assert text.startswith("[link.cover_gap v1] Cover PUMA 2's gap: PUMA 1 takes over its orbit (300 m radius)")
    assert "PUMA 2 has been silent 7 s (its lost-link limit 3 s), on orbit i3 (human op-setup)" in text
    assert R.parse_record(text) == rec
    res, b = look(reads(vehicles=[vehicle(3, "PUMA 1", battery_pct=22.0)]), play="safety.low_reserve")
    text, _ = Engine(config()).finish(res.candidates[0], b, None, [])
    assert R.split(text)[0].startswith("[safety.low_reserve v1] Return PUMA 1 home: battery 22%, under 25%")
