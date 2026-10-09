"""The ledger: cooldowns and their material-change override, adoption after a restart, the
token bucket, persistence (murmur-t8m)."""

from __future__ import annotations

import asyncio

from playbook import rationale as R
from playbook.arbiter import arbitrate
from playbook.beliefs import build
from playbook.engine import Engine
from playbook.ledger import Ledger
from playbook.plays import PLAYS
from playbook_world import T0, FakeSession, config, intent, offset, reads, track, vehicle

FLEET = [vehicle(3, "PUMA 1", "puma", offset(-1200, 200)), vehicle(5, "PUMA 3", "puma", offset(-1300, 250))]


def boat(**over):
    aff, conf = over.pop("affiliation", "suspect"), over.pop("conf", 0.7)
    kw = {"pos": offset(-2000, -300), "vel": (12.0, 0.0)}
    kw.update(over)
    return track("T-1", aff, conf, **kw)


def candidates(led, t=T0, tr=None):
    b = build(reads(t=t, vehicles=FLEET, tracks=[tr or boat()]), config(), led)
    return arbitrate(PLAYS["isr.investigate"].evaluate(b, led).candidates, b, led), b


def proposed(led, iid=101, uid=3, state="pending_approval", cpa=300.0):
    led.record(iid, play="isr.investigate", version="1", target="T-1", uid=uid, task="orbit",
               request={"task": "orbit", "uids": [uid], "center": [0, 0], "radius_m": 1000.0}, t=T0, state=state,
               basis={"affiliation": "suspect", "aff_conf": 0.7, "declared": None, "cpa_m": cpa}, withdraw_if=[])


def test_a_rejection_cools_the_play_on_that_target_for_10_minutes_whatever_the_vehicle():
    led = Ledger()
    proposed(led)
    ds = led.sync({101: intent(101, "orbit", [3], "rejected", approval={"by": "op-1", "note": "not now"})}, T0 + 5, config())
    assert [(d.state, d.by, d.note) for d in ds] == [("rejected", "op-1", "not now")]
    assert ds[0].cooldown_until == T0 + 5 + 600
    (sel, held), _ = candidates(led, T0 + 10)
    assert sel == [], "not with PUMA 1, and not with PUMA 3 either"
    assert {h.reason for h in held} == {"cooling down until 14:48:25Z: i101 rejected by op-1 ('not now')"}
    (sel, _), _ = candidates(led, T0 + 606)
    assert [c.uid for c in sel] == [5], "after 10 minutes it may propose again (PUMA 3 is the nearer)"


def test_inside_a_cooldown_only_a_material_change_lets_it_propose_and_the_rationale_names_it():
    led = Ledger()
    proposed(led)
    led.sync({101: intent(101, "orbit", [3], "rejected", approval={"by": "op-1", "note": "not now"})}, T0 + 5, config())
    # the confidence moved by 0.15 but stayed on the same side of 0.5: not material
    (sel, _), _ = candidates(led, T0 + 20, boat(conf=0.86))
    assert sel == []
    # its closest approach went from 300 m to about 100 m: halved, but by less than half the alert
    # (the live case: 192 m to 79 m, the same heading): not material
    (sel, _), b = candidates(led, T0 + 20, boat(pos=offset(-2000, -100)))
    assert sel == []
    # from 700 m (outside the alert) to about 100 m: material
    led = Ledger()
    proposed(led, cpa=700.0)
    led.sync({101: intent(101, "orbit", [3], "rejected", approval={"by": "op-1", "note": "not now"})}, T0 + 5, config())
    (sel, _), b = candidates(led, T0 + 20, boat(pos=offset(-2000, -100)))
    assert len(sel) == 1
    mc = sel[0].material_change
    assert mc["cooldown"] == "i101 rejected by op-1 at 14:38:25Z" and mc["change"].startswith("the closest approach went from 700 m to")
    eng = Engine(config())
    text, rec = eng.finish(sel[0], b, None, [])
    assert "Proposed again inside a cooldown (i101 rejected by op-1 at 14:38:25Z): material change: the closest approach" in text
    assert rec["material_change"] == mc



def test_only_a_rise_in_suspicion_is_a_material_change():
    """Suspect to unknown (the tracker flips them at classification 0.3) is a fall: not news to
    the operator who said no. Unknown to suspect, or a suspect held with confidence that rose
    across 0.5, is."""
    def rejected(aff, conf):
        led = Ledger()
        proposed(led)
        led.entries["101"]["basis"].update({"affiliation": aff, "aff_conf": conf})
        led.sync({101: intent(101, "orbit", [3], "rejected", approval={"by": "op-1", "note": "not now"})}, T0 + 5, config())
        return led

    fell = track("T-1", "unknown", 0.2, pos=offset(-2000, -300), vel=(12.0, 0.0), kind_conf=0.2)
    (sel, held), _ = candidates(rejected("suspect", 0.7), T0 + 20, fell)
    assert sel == [], "suspect to unknown at 0.20: a fall, not material"
    (sel, _), _ = candidates(rejected("suspect", 0.7), T0 + 20, boat(conf=0.3))
    assert sel == [], "a suspect's confidence falling across 0.5: not material"
    (sel, _), _ = candidates(rejected("unknown", 0.4), T0 + 20, boat(conf=0.7))
    assert sel and sel[0].material_change["change"] == "the affiliation shown rose from unknown to suspect"
    (sel, _), _ = candidates(rejected("suspect", 0.4), T0 + 20, boat(conf=0.7))
    assert sel and sel[0].material_change["change"] == "the affiliation confidence rose from 0.40 to 0.70, across 0.5"


def test_an_expiry_cools_for_2_minutes_and_halves_the_score_a_void_cools_only_its_key():
    led = Ledger()
    proposed(led)
    led.sync({101: intent(101, "orbit", [3], "expired")}, T0 + 120, config())
    assert led.score_factor["isr.investigate|T-1"] == 0.5
    (sel, _), _ = candidates(led, T0 + 200)
    assert sel == []
    (sel, _), _ = candidates(led, T0 + 241)
    assert len(sel) == 1 and abs(sel[0].score - 0.5 * (0.5 + 0.5 * (1 - sel[0].derived["tcpa_s"] / 600) + 0.1)) < 0.01
    led2 = Ledger()
    proposed(led2)
    led2.sync({101: intent(101, "orbit", [3], "voided")}, T0 + 10, config())
    (sel, held), _ = candidates(led2, T0 + 15)
    assert [c.uid for c in sel] == [5], "a void cools its key: another vehicle may go at once"


def test_a_withdrawal_of_ours_cools_its_play_and_target_for_30_s_on_every_vehicle():
    """The bridge voids a withdrawn intent (it has no `withdrawn` state): the ledger knows the
    void was its own and cools (play, target) for every vehicle, as a rejection does; a void by
    the system cools only its key."""
    led = Ledger()
    proposed(led)
    led.entries["101"]["withdrawn_by"] = "hygiene.withdraw_stale"
    led.sync({101: intent(101, "orbit", [3], "voided")}, T0 + 10, config())
    assert led.cooldown_for("isr.investigate", "T-1", 3, T0 + 39)["why"] == "withdrawn"
    assert led.cooldown_for("isr.investigate", "T-1", 5, T0 + 20)["why"] == "withdrawn", "PUMA 3 too, not only PUMA 1"
    assert led.cooldown_for("isr.investigate", "T-1", 5, T0 + 41) is None
    (sel, held), _ = candidates(led, T0 + 15)
    assert sel == [] and all(h.reason.startswith("cooling down until 14:39:00Z: i101 withdrawn") for h in held), held


def test_a_restart_adopts_its_cards_from_their_evidence_records():
    rec = {"play": "isr.investigate@1", "target": "T-1", "uid": 3, "request": {"task": "orbit", "uids": [3]},
           "basis": {"affiliation": "suspect"}, "withdraw_if": ["it is unseen for 60 s"]}
    rationale = R.compose("[isr.investigate v1] Orbit PUMA 1 over T-1.", rec)
    led = Ledger()
    led.sync({7: intent(7, "orbit", [3], "pending_approval", rationale=rationale),
              8: intent(8, "hold", [5], "done", rationale="someone else's words")}, T0, config())
    assert set(led.entries) == {"7"} and led.entries["7"]["adopted"]
    assert (led.entries["7"]["play"], led.entries["7"]["target"], led.entries["7"]["uid"]) == ("isr.investigate", "T-1", 3)


def test_the_ledger_round_trips_through_json(tmp_path):
    led = Ledger()
    proposed(led)
    led.sync({101: intent(101, "orbit", [3], "rejected", approval={"by": "op-1"})}, T0 + 5, config())
    led.take_token(T0, config())
    led.set_backoff("k", T0, "preview failed", config())
    p = tmp_path / "ledger.json"
    led.save(p)
    back = Ledger.load(p)
    assert back.to_json() == led.to_json()
    assert Ledger.load(tmp_path / "missing.json").entries == {}


def test_the_token_bucket_refills_one_card_per_30_s_up_to_two():
    led, cfg = Ledger(), config()
    assert led.tokens(T0, cfg) == 2.0
    led.take_token(T0, cfg)
    led.take_token(T0, cfg)
    assert led.tokens(T0 + 15, cfg) == 0.5 and led.tokens(T0 + 30, cfg) == 1.0 and led.tokens(T0 + 600, cfg) == 2.0


def test_a_preview_that_fails_backs_off_doubling_and_is_retried_never_dropped():
    failing = {"checks": [{"rule": "geofence", "uid": "3", "outcome": "fail", "detail": "enters the south tower's no-go box"}],
               "passed": False, "policy": None, "would_reach": "rejected"}
    ticks = [reads(t=T0 + dt, vehicles=FLEET[:1], tracks=[boat()]) for dt in (0, 2, 11, 12, 31)]
    s = FakeSession(ticks, failing)
    e = Engine(config())

    async def go():
        await e.start(s)
        return [await e.tick(s) for _ in ticks]

    reps = asyncio.run(go())
    assert [r.previews for r in reps] == [1, 0, 1, 0, 1], "again after 10 s, then after 20 s more"
    assert e.led.backoff["isr.investigate|T-1|3"]["fails"] == 3
    assert "preview failed: geofence fail: enters the south tower's no-go box" in reps[1].statuses["isr.investigate"]
