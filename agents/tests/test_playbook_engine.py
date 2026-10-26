"""The engine end to end on a fake session (murmur-t8m): a tick's reads, preview, veto and
proposal; the follow-up withdrawals; shadow mode's reads-only; determinism byte for byte on
recorded reads; compute per tick at 100 vehicles; the evidence record's round trip; the config
hash sent at connect. No bridge, no model."""

from __future__ import annotations

import asyncio
import json
import os
import statistics
import time
from pathlib import Path

import pytest

from playbook import rationale as R
from playbook.config import ConfigError, canonical, config_hash, load_file, make
from playbook.engine import Engine
from playbook.ledger import Ledger
from playbook_world import (FIXTURES, GATE, T0, WATCH, FakeSession, config, intent, offset, passing, reads, recorded,
                            recorded_reads, track, vehicle)

GOLDEN = FIXTURES / "golden-harbor-watch.txt"


def run(ticks, previews=None, cfg=None, intents=None, shadow=False, log=None):
    s = FakeSession(ticks, previews, intents)
    lines: list[dict] = []
    e = Engine(cfg or config(), shadow=shadow, log=log or (lambda kind, **f: lines.append({"kind": kind, **f})))

    async def go():
        await e.start(s)
        return [await e.tick(s) for _ in ticks]

    return s, e, asyncio.run(go()), lines


FLEET = [vehicle(1, "HUNTER 1", "seahunter", offset(1000, -300), task={"intent_id": 1, "task": "patrol", "state": "active"}),
         vehicle(3, "PUMA 1", "puma", offset(-1200, 200))]


def test_a_tick_previews_vetoes_and_proposes_with_the_record_after_the_delimiter():
    s, e, reps, lines = run([reads(vehicles=FLEET, tracks=[track("T-1", "suspect", 0.7)])])
    assert s.writers() == ["preview_intent", "propose_intent"]
    rationale = s.proposed[0]["rationale"]
    text, body = R.split(rationale)
    rec = R.parse_record(rationale)
    assert body is not None and rec["play"] == "isr.investigate@2" and rec["config"] == e.cfg.label
    assert text.startswith("[isr.investigate v2] Orbit PUMA 1 over T-1 (1000 m radius)")
    assert "Rule book v2: nothing against." in text and "tier human (agent-tasking)" in text
    # every number the text cites is in the record (rationale fidelity, by construction)
    assert f"battery {rec['vehicle']['battery_pct']:.0f}%" in text
    assert f"{rec['derived']['cpa_m']:.0f} m to the Golden Gate Bridge" in text
    assert e.led.entries["101"]["play"] == "isr.investigate" and e.led.entries["101"]["state"] == "pending_approval"
    kinds = [x["kind"] for x in lines]
    assert kinds.count("proposal") == 1


def test_a_tick_reads_the_predicted_link_once_and_the_look_goes_where_it_holds():
    """One get_link_coverage call a tick for every orbit a look could offer; the proposal goes to
    the vehicle whose predicted link holds, and its rationale says so. A failing call is said once
    and the look ranks as before."""
    far = vehicle(5, "PUMA 3", "puma", offset(-6000, 0))
    s = FakeSession([reads(vehicles=FLEET + [far], tracks=[track("T-1", "suspect", 0.7)])])
    s.coverage = lambda q: {"holds": q["uid"] != 3, "p_failsafe": 1.0 if q["uid"] == 3 else 0.0,
                            "sentence": f"uid {q['uid']}: predicted."}
    e = Engine(config())

    async def go():
        await e.start(s)
        return await e.tick(s)

    asyncio.run(go())
    asked = [a for n, a in s.calls if n == "get_link_coverage"]
    assert len(asked) == 1 and [q["uid"] for q in asked[0]["queries"]] == [3, 5]
    assert s.proposed[0]["request"]["uids"] == [5]
    assert "Predicted link: uid 5: predicted. Chosen over PUMA 1" in s.proposed[0]["rationale"]

    class Broken(FakeSession):
        async def call(self, name, **args):
            if name == "get_link_coverage":
                raise RuntimeError("unknown tool get_link_coverage")
            return await super().call(name, **args)

    b = Broken([reads(vehicles=FLEET + [far], tracks=[track("T-1", "suspect", 0.7)])] * 2)
    lines: list[dict] = []
    e = Engine(config(), log=lambda kind, **f: lines.append({"kind": kind, **f}))

    async def go2():
        await e.start(b)
        return [await e.tick(b) for _ in range(2)]

    asyncio.run(go2())
    assert b.proposed[0]["request"]["uids"] == [3], "as before"
    assert [x["kind"] for x in lines].count("links.unread") == 1, "said once"


def test_the_rule_book_vetoes_what_a_passing_preview_lets_through():
    """A preview whose reserves check is unverified still passes (a human tier); Murmur's rule
    book objects to it, so the engine never proposes it."""
    unverified = passing(checks=[{"rule": "reserves", "uid": "3", "outcome": "unverified", "detail": "no endurance figure"}])
    s, e, reps, _ = run([reads(vehicles=FLEET, tracks=[track("T-1", "suspect", 0.7)])], previews=unverified)
    assert s.writers() == ["preview_intent"] and not s.proposed
    assert "rule book v2: check reserves:3 unverified" in reps[0].statuses["isr.investigate"]


def test_a_status_goes_to_the_log_once_it_has_held_three_ticks():
    """Debounced (fix round 1): a status is logged when it has held `status_hold_ticks` (3)
    ticks running and differs from the last one logged; the one-tick "proposed" line between
    ready and waiting is never news, and a flicker shorter than three ticks is not logged."""
    tr = track("T-1", "suspect", 0.7)
    ticks = [reads(t=T0 + dt, vehicles=FLEET, tracks=trs) for dt, trs in
             ((0, []), (2, []), (4, [tr]), (6, [tr]), (8, [tr]), (10, [tr]))]
    _, _, reps, lines = run(ticks)
    status = [(x["play"], x["status"]) for x in lines if x["kind"] == "play.status"]
    assert all(v == "ready" for v in reps[0].statuses.values())
    assert reps[2].statuses["isr.investigate"] == "active: proposed i101 orbit PUMA 1 on T-1 (pending_approval)"
    assert status == [("isr.investigate", "active: T-1 by uid 3 (i101 pending_approval)")]
    # a status that flickers (two ticks blocked, back to ready) is not logged
    busy = [FLEET[0], vehicle(3, "PUMA 1", "puma", offset(-1200, 200), mode="RTL")]
    ticks = [reads(t=T0 + dt, vehicles=v, tracks=[tr]) for dt, v in ((0, busy), (2, busy), (4, [FLEET[0]]), (6, [FLEET[0]]))]
    _, _, reps, lines = run(ticks)
    assert reps[0].statuses["isr.investigate"].startswith("not ready")
    assert [x for x in lines if x["kind"] == "play.status"] == []


def test_stale_and_link_lost_cards_are_withdrawn_within_a_tick_with_the_reason():
    tr = track("T-1", "suspect", 0.7)
    lost = [FLEET[0], vehicle(3, "PUMA 1", "puma", offset(-1200, 200), link_lost=True, link_age_s=3.5)]
    alerts = [{"id": "link:3", "kind": "lost_link", "uid": 3, "since_t": T0 + 2 - 3.5}]
    ticks = [reads(t=T0, vehicles=FLEET, tracks=[tr]),
             reads(t=T0 + 2, vehicles=lost, tracks=[tr], alerts=alerts, lost_full={3: {"uid": 3, "link": {"lost_after_s": 3.0}}})]
    s, e, reps, lines = run(ticks)
    assert [w["id"] for w in s.withdrawn] == [101]
    assert s.withdrawn[0]["reason"] == "link.withdraw_pending: PUMA 1's link lost at 14:38:18Z (silent 4 s)"
    neutral = track("T-1", "neutral", 0.8, kind="fishing_vessel")
    s, e, reps, lines = run([reads(t=T0, vehicles=FLEET, tracks=[tr]), reads(t=T0 + 2, vehicles=FLEET, tracks=[neutral])])
    assert s.withdrawn[0]["reason"] == "hygiene.withdraw_stale: isr.investigate: T-1 is shown neutral at 0.80"
    assert [x["text"] for x in lines if x["kind"] == "withdrawal"] == [
        "withdrew i101 (hygiene.withdraw_stale): isr.investigate: T-1 is shown neutral at 0.80"]


def test_a_rejection_with_a_note_is_logged_and_the_play_cools_down():
    tr = track("T-1", "suspect", 0.7)
    ticks = [reads(t=T0 + dt, vehicles=FLEET, tracks=[tr]) for dt in (0, 2, 4, 6)]
    s, e, reps, lines = run(ticks, intents={1: {101: "rejected"}, 2: {101: "rejected"}, 3: {101: "rejected"}})
    assert [len(r.proposed) for r in reps] == [1, 0, 0, 0], "no re-proposal without a material change"
    d = [x for x in lines if x["kind"] == "decision"]
    assert d and d[0]["text"].startswith("i101 rejected by op-1 (note \"not now\"); isr.investigate on T-1 cools down until")
    # stated steadily while it lasts, whether or not the trigger holds at the moment
    assert {r.statuses["isr.investigate"] for r in reps[1:]} == {"ready | cooling down: T-1 until 14:48:22Z (i101 rejected by op-1)"}


# ---- shadow mode ------------------------------------------------------------------------------------------


def test_shadow_mode_reads_only_and_logs_what_it_would_propose_and_withdraw():
    tr = track("T-1", "suspect", 0.7)
    neutral = track("T-1", "neutral", 0.8, kind="fishing_vessel")
    ticks = [reads(t=T0, vehicles=FLEET, tracks=[tr]), reads(t=T0 + 2, vehicles=FLEET, tracks=[tr]),
             reads(t=T0 + 4, vehicles=FLEET, tracks=[neutral])]
    s, e, reps, lines = run(ticks, shadow=True)
    assert s.writers() == [], "no preview, no proposal, no withdrawal"
    assert not any(n == "get_intents" for n, _ in s.calls), "it holds no intents of its own"
    would = [x for x in lines if x["kind"] == "shadow"]
    assert len(would) == 1 and would[0]["track"] == "T-1" and would[0]["request"]["uids"] == [3]
    assert would[0]["record"]["preview"] == {"shadow": True, "tier_estimate": "human", "rule_estimate": None}
    assert "Shadow: not previewed, not proposed. Tier human by the policy, estimated (no tier matched)." in would[0]["rationale"]
    w = [x for x in lines if x["kind"] == "shadow.withdraw"]
    assert len(w) == 1 and w[0]["reason"] == "isr.investigate: T-1 is shown neutral at 0.80"
    assert all(x["kind"] != "play.status" for x in lines)


def test_shadow_statuses_go_to_the_log_as_shadow_status_once_held_three_ticks():
    ticks = [reads(t=T0 + dt, vehicles=[], tracks=[track("T-1", "suspect", 0.7)]) for dt in (0, 2, 4, 6)]
    _, _, reps, lines = run(ticks, shadow=True)
    st = [(x["play"], x["status"]) for x in lines if x["kind"] == "shadow.status"]
    assert st == [("isr.investigate", "not ready: T-1: no free sensor aircraft (none in the fleet)")]


def test_a_would_be_card_lapses_after_the_policys_approval_wait():
    tr = track("T-1", "suspect", 0.7)
    ticks = [reads(t=T0 + dt, vehicles=FLEET, tracks=[tr]) for dt in (0, 60, 121, 180, 242)]
    s, e, reps, lines = run(ticks, shadow=True)
    kinds = [x["kind"] for x in lines if x["kind"].startswith("shadow") and x["kind"] != "shadow.status"]
    assert kinds == ["shadow", "shadow.lapsed", "shadow"], "lapsed as expired: 2 minutes, then again"
    assert s.writers() == []


# ---- determinism, compute, the record ---------------------------------------------------------------------


def recorded_run() -> str:
    doc = recorded()
    cfg = make(WATCH, "harbor-watch")
    ticks = [recorded_reads(doc, i) for i in range(len(doc["ticks"]))]
    pv = doc["preview_example"]
    s, e, reps, lines = run(ticks, previews=pv, cfg=cfg)
    out = [f"config {cfg.label}"]
    for i, r in enumerate(reps):
        out.append(f"--- tick {i} at {doc['ticks'][i]['at_s']} s")
        for p in r.proposed:
            out.append(f"proposed i{p['intent']} {json.dumps(p['request'], sort_keys=True)}")
            out.append(p["rationale"])
        for w in r.withdrawn:
            out.append(f"withdrew i{w['intent']}: {w['reason']}")
        for k, v in sorted(r.statuses.items()):
            out.append(f"status {k}: {v}")
    return "\n".join(out) + "\n"


def test_recorded_reads_give_byte_identical_proposals_and_rationales():
    """The live Harbor watch reads (fixtures/playbook/harbor-watch-reads.json) through the engine
    twice, and against the committed golden output. Regenerate with UPDATE_GOLDEN=1 only for a
    deliberate change of a play, a template or the config, and say so in the commit."""
    a, b = recorded_run(), recorded_run()
    assert a == b
    if os.environ.get("UPDATE_GOLDEN") == "1":
        GOLDEN.write_text(a)
    assert a == GOLDEN.read_text(), "the golden output changed: see the diff, and regenerate only on purpose"
    assert "proposed i101" in a and "[isr.investigate v2] Orbit PUMA 1 over T-0008" in a


def hundred() -> tuple[list[dict], list[dict]]:
    fleet = []
    for u in range(1, 101):
        platform = ("puma", "mq9b", "seahunter", "switchblade")[u % 4]
        task = {"intent_id": 1000 + u, "task": "orbit", "state": "done"} if u % 3 else None
        fleet.append(vehicle(u, f"V{u}", platform, offset(-4000 + 80 * u, -2000 + 40 * (u % 50)), task=task,
                             battery_pct=float(30 + u % 70)))
    tracks = [track(f"T-{i:04d}", ("suspect", "unknown", "neutral", "assumed_friend")[i % 4], 0.3 + (i % 7) / 10,
                    pos=offset(-3000 + 150 * i, -1500 + 100 * (i % 30)), vel=(float(i % 13), float(i % 5) - 2.0)) for i in range(30)]
    return fleet, tracks


def test_compute_per_tick_at_100_vehicles_is_under_50_ms():
    fleet, tracks = hundred()
    alerts = [{"id": f"op:{u}", "kind": "not_moving", "uid": u, "since_t": T0 - 40} for u in range(5, 100, 17)]
    r = reads(vehicles=fleet, tracks=tracks, alerts=alerts)
    e = Engine(config(enable=["isr.armed_overwatch"]))
    times = []
    for _ in range(15):
        t0 = time.perf_counter()
        plan = e.plan(r)
        times.append((time.perf_counter() - t0) * 1000.0)
        for c in plan.selected[:3]:
            e.finish(c, plan.beliefs, passing(), [])
    assert plan.selected, "the world gives the plays work to do"
    assert statistics.median(times) < 50.0, times


def test_the_evidence_record_round_trips():
    rec = {"play": "isr.investigate@1", "facts": [{"ref": "track:T-1.affiliation", "value": "suspect", "conf": 0.41}],
           "derived": {"cpa_m": 140.0, "tcpa_s": None}, "note": "quotes \"and\" unicode é"}
    text = "[isr.investigate v1] Orbit HAWK 2 over T-1."
    r = R.compose(text, rec)
    assert R.split(r) == (text, R.dumps(rec)) and R.parse_record(r) == rec
    assert R.compose(text + "\n\n", rec) == r, "trailing space in the text does not matter"
    assert R.parse_record("an operator's own words") is None
    assert R.parse_record(text + R.SEP + "{not json") is None
    assert R.r1(-0.04) == 0.0 and str(R.r1(-0.04)) == "0.0", "no -0.0 in a record"


def test_the_config_is_canonical_hashed_and_sent_at_connect(tmp_path: Path):
    a, b = make(WATCH, "harbor-watch"), make(json.loads(json.dumps(WATCH)), "harbor-watch")
    assert a.hash == b.hash == config_hash(json.loads(canonical(a.data))) and len(a.hash) == 12
    assert a.client_version == f"playbook-1 config harbor-watch#{a.hash}"
    assert make({**WATCH, "thresholds": {"fast_mps": 9.0}}, "x").hash != a.hash
    # the keys of before still work: near + radius_km, platform, every_s
    legacy = make({"near": list(GATE), "radius_km": 3, "every_s": 10, "platform": "puma"}, "old")
    assert legacy["watch_area"] == [{"name": "watch point", "center": list(GATE), "radius_m": 3000.0}]
    assert legacy["sensor_platform"] == "puma" and legacy["tick_s"] == 10.0
    for bad in ({"near": [1]}, {"radius_km": 2}, {"plays": ["isr.nonsense"]}, {"thresholds": {"fastest": 1}},
                {"watch_area": [{"name": "x", "center": [0, 0]}]}, {"tick_s": 0.1}, {"colour": "red"}):
        with pytest.raises(ConfigError):
            make(bad)
    p = tmp_path / "playbook.json"
    p.write_text(json.dumps(WATCH))
    assert load_file(p, "harbor-watch").hash == a.hash


def test_the_session_sends_the_config_label_as_its_client_version(monkeypatch):
    import policy_agent as P

    seen = {}

    class Session:
        def __init__(self, read, write, client_info=None):
            seen["client"] = client_info

        async def __aenter__(self):
            return self

        async def __aexit__(self, *a):
            pass

        async def initialize(self):
            pass

        async def list_tools(self):
            class R_:
                tools = []

            return R_()

    class Transport:
        async def __aenter__(self):
            return "r", "w"

        async def __aexit__(self, *a):
            pass

    class Http:
        def __init__(self, **_):
            pass

        async def aclose(self):
            pass

    monkeypatch.setattr(P.httpx, "AsyncClient", Http)
    monkeypatch.setattr(P, "streamable_http_client", lambda url, http_client=None: Transport())
    monkeypatch.setattr(P, "ClientSession", Session)
    cfg = make(WATCH, "harbor-watch")

    async def go():
        async with P.AgentSession("http://x/mcp", "agent-playbook", client_version=cfg.client_version):
            pass

    asyncio.run(go())
    assert seen["client"].name == "agent-playbook" and seen["client"].version == cfg.client_version


def test_the_ledger_is_saved_each_tick(tmp_path: Path):
    s = FakeSession([reads(vehicles=FLEET, tracks=[track("T-1", "suspect", 0.7)])])
    p = tmp_path / "playbook-ledger.json"
    e = Engine(config(), ledger_path=p)

    async def go():
        await e.start(s)
        await e.tick(s)

    asyncio.run(go())
    back = Ledger.load(p)
    assert back.entries["101"]["target"] == "T-1" and back.cursor == "f:0"
    assert intent(1, "hold", [1])["state"] == "pending_approval"


def test_the_fidelity_check_passes_a_proposal_and_catches_a_tampered_text(tmp_path: Path):
    from playbook import fidelity

    s, e, reps, _ = run([reads(vehicles=FLEET, tracks=[track("T-1", "suspect", 0.7)])])
    rationale = s.proposed[0]["rationale"]
    assert fidelity.check(rationale) == []
    assert fidelity.check(rationale.replace("battery 90%", "battery 99%")) == [
        "the text is not what the template renders from the record",
        "the number 99% in the text traces to no field of the record"]
    assert fidelity.check("an operator's own words") == ["no evidence record after the delimiter"]
    log = tmp_path / "audit.jsonl"
    created = {"type": "intent.created", "intent": {"id": 101, "source": {"kind": "agent", "id": "agent-playbook"},
                                                    "rationale": rationale}}
    other = {"type": "intent.created", "intent": {"id": 102, "source": {"kind": "agent", "id": "agent-watch"}, "rationale": "x"}}
    log.write_text(json.dumps(created) + "\n" + json.dumps(other) + "\n")
    assert fidelity.main([str(log)]) == 0
