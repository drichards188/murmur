"""The simulated-time operator against eval/proxy.py's own ProxyOperator: the same cards, the same
seed, the same looks, reads and decisions after the same delays (operator.py's claim)."""

from __future__ import annotations

import asyncio
import re

import pytest

import eval.proxy as proxy
from eval.headless.operator import Operator, operator_model


def _card(iid: int, t_in: float, checks: int = 6, agent: bool = True, source: str = "agent-playbook",
          armed: bool = False) -> dict:
    return {"id": iid, "kind": "orbit", "state": "pending_approval", "t_in": t_in, "expires_t": int((t_in + 120) * 1000),
            "source": {"kind": "agent" if agent else "human", "id": source},
            "checks": [{"rule": "capability", "uid": "3", "outcome": "pass"}] * checks,
            "params": {"center": [-122.5, 37.8], "radius_m": 1000.0}, "policy": {"facts": {"armed": armed}},
            "why": ["needs a human", "a warning"], "rationale": "r"}


CARDS = [_card(4, 5.0), _card(5, 30.0), _card(6, 31.0, 3), _card(7, 32.0, agent=False), _card(8, 33.0),
         _card(9, 200.0), _card(10, 201.0)]
#: a situation's `[autoplay]` with both settings (hostile-act-engagement's): cards from the granted
#: agent among others, one the rule book would reject (armed, nothing at its aim), and rule
#: declarations coming into the review list (D-3 not confirmable, D-4 without a track)
GRANT = {"grant_agents": ["agent-watch"], "confirm_rule_declarations": True}
GRANT_CARDS = [_card(4, 5.0, source="agent-watch"), _card(5, 30.0), _card(6, 31.0, 3, source="agent-watch", armed=True),
               _card(7, 32.0, agent=False), _card(8, 33.0, source="agent-watch"), _card(9, 200.0),
               _card(10, 201.0, source="agent-watch")]
DECLARATIONS = [
    {"id": "D-1", "track": "T-0003", "t_in": 40.0, "actions": ["confirm", "revoke"], "basis": "rule hostile_act"},
    {"id": "D-2", "track": "T-0007", "t_in": 41.0, "actions": ["confirm", "revoke"], "declared_by": "rule"},
    {"id": "D-3", "track": "T-0008", "t_in": 90.0, "actions": ["revoke"]},
    {"id": "D-4", "track": None, "t_in": 90.0, "actions": ["confirm"]},
    {"id": "D-5", "track": "T-0011", "t_in": 230.0, "actions": ["confirm", "revoke"], "basis": "rule hostile_act"},
]


#: rules 30's review items (`node_losses`): HUNTER 1 presumed lost (confirmed), a vehicle silent and
#: flagged by hand (left alone: it asks a judgment), one presumed lost later (confirmed), and HUNTER 1
#: again after it was heard and fell silent anew (a new `since`: confirmed again)
NODE_LOSSES = [
    {"uid": 1, "callsign": "HUNTER 1", "state": "presumed_lost", "since": 125_000, "since_t": 125.0, "t_in": 50.0,
     "actions": ["confirm"], "evidence": [{"kind": "act", "detail": "act A-1 against it, seen by HUNTER 2's eo_ir"},
                                          {"kind": "struck", "detail": "its struck report"}]},
    {"uid": 2, "callsign": "HUNTER 2", "state": "silent", "since": 130_000, "t_in": 60.0, "actions": ["confirm", "dismiss"],
     "evidence": [{"kind": "operator", "detail": "flagged by op-1"}]},
    {"uid": 4, "callsign": "SKY 1", "state": "presumed_lost", "since": None, "t_in": 210.0, "actions": ["confirm"],
     "evidence": [{"kind": "act", "detail": "act A-3 against it"}]},
    {"uid": 1, "callsign": "HUNTER 1", "state": "presumed_lost", "since": 300_000, "since_t": 300.0, "t_in": 320.0,
     "actions": ["confirm"], "evidence": [{"kind": "damage", "detail": "mobility"}]},
]


class World:
    """The inbox over (fake) time: a card is pending from its `t_in` until decided; a rule
    declaration is in the review list from its `t_in` until confirmed; a presumed loss (rules 30)
    from its `t_in` until confirmed on its `since`."""

    def __init__(self, cards: list[dict] | None = None, declarations: list[dict] | None = None,
                 losses: list[dict] | None = None):
        self.now = 0.0
        self.cards = CARDS if cards is None else cards
        self.declarations = declarations or []
        self.losses = losses or []
        self.decided: dict[int, str] = {}
        self.confirmed: set[str] = set()
        self.lost: set[tuple] = set()
        self.calls: list[tuple] = []

    def pending(self) -> list[dict]:
        return [c for c in self.cards if c["t_in"] <= self.now and c["id"] not in self.decided]

    def review(self) -> dict:
        out: dict = {"declarations": [d for d in self.declarations
                                      if d["t_in"] <= self.now and d["track"] not in self.confirmed]}
        if self.losses:
            # the newest silence of each vehicle, until it is confirmed lost on it
            now = {n["uid"]: n for n in self.losses if n["t_in"] <= self.now}
            out["node_losses"] = [n for n in now.values() if (n["uid"], n["since"]) not in self.lost]
        return out

    def lose(self, uid: int, body: dict, operator: str) -> dict:
        assert operator == "op-2" and set(body) == {"since", "note"}, (operator, body)
        self.calls.append(("lost", round(self.now, 6), uid, body["since"], body["note"]))
        self.lost.add((uid, body["since"]))
        return {"lost": {"uid": uid, "since": body["since"], "voided": [], "ended": [7] if uid == 1 else [], "closed": None}}

    def confirm(self, track: str) -> dict:
        self.calls.append(("confirm", round(self.now, 6), track))
        self.confirmed.add(track)
        return {"track": track}

    def decide(self, iid: int, action: str) -> dict:
        self.decided[iid] = action
        return {"intent": {"id": iid, "state": "dispatched" if action == "approve" else "rejected"}}


class FakeBridge:
    """eval/bridge.py's surface for ProxyOperator."""

    def __init__(self, w: World):
        self.w = w

    def try_get(self, path: str):
        if path == "/api/review":
            return self.w.review()
        assert path == "/api/inbox"
        return {"pending": self.w.pending()}

    def post(self, path: str, body, operator):
        m = re.match(r"/api/review/nodes/(\d+)/lost$", path)
        if m:
            return self.w.lose(int(m.group(1)), body, operator)
        m = re.match(r"/api/tracks/([^/]+)/confirm$", path)
        assert m and body == {} and operator == "op-auto", (path, body, operator)
        return self.w.confirm(m.group(1))

    def view(self, iid, event, shown, operator, action=None, open_ms=None):
        self.w.calls.append(("view", round(self.w.now, 6), iid, event, action))

    def tracks(self):
        return []

    def approve(self, iid, note, operator):
        self.w.calls.append(("approve", round(self.w.now, 6), iid, note))
        return self.w.decide(iid, "approve")

    def reject(self, iid, note, operator):
        self.w.calls.append(("reject", round(self.w.now, 6), iid, note))
        return self.w.decide(iid, "reject")


class FakeSim:
    """The simulator's operator API over the same world."""

    def __init__(self, w: World):
        self.w = w

    def api(self, method, path, body=None, operator=None):
        if path == "/api/inbox":
            return 200, {"pending": self.w.pending()}
        if path == "/api/review":
            return 200, self.w.review()
        if path == "/api/tracks":
            return 200, {"tracks": []}
        if path == "/api/operator/view":
            self.w.calls.append(("view", round(self.w.now, 6), body["intent_id"], body["event"], body.get("action")))
            return 200, {}
        m = re.match(r"/api/review/nodes/(\d+)/lost$", path)
        if m:
            assert method == "POST"
            return 200, self.w.lose(int(m.group(1)), body, operator)
        m = re.match(r"/api/tracks/([^/]+)/confirm$", path)
        if m:
            assert method == "POST" and body == {} and operator == "op-auto", (method, body, operator)
            return 200, self.w.confirm(m.group(1))
        m = re.match(r"/api/intents/(\d+)/(approve|reject)", path)
        assert m, path
        iid, action = int(m.group(1)), m.group(2)
        self.w.calls.append((action, round(self.w.now, 6), iid, body["note"]))
        return 200, self.w.decide(iid, action)


def run_proxy(seed: int, preset: str, end: float, extra: dict | None = None, world: World | None = None,
              decisions: list | None = None) -> list[tuple]:
    w = world or World()
    params = proxy.proxy_params({"preset": preset, **(extra or {})})
    op = proxy.ProxyOperator(FakeBridge(w), params, seed, operator="op-auto", note_prefix="np",
                             loss_confirmer="op-2", loss_note_prefix="np2")

    async def fake_sleep(seconds: float) -> None:
        w.now += max(0.0, seconds)

    async def fake__sleep(stop, seconds: float) -> bool:
        w.now += max(0.0, seconds)
        return w.now > end

    op._sleep = fake__sleep  # type: ignore[method-assign]
    real_sleep = proxy.asyncio.sleep
    real_mono = proxy.time.monotonic
    real_time = proxy.time.time
    proxy.asyncio.sleep = fake_sleep  # type: ignore[assignment]
    proxy.time.monotonic = lambda: w.now  # type: ignore[assignment]
    proxy.time.time = lambda: w.now  # type: ignore[assignment]
    try:
        asyncio.run(op.run(asyncio.Event()))
    finally:
        proxy.asyncio.sleep = real_sleep  # type: ignore[assignment]
        proxy.time.monotonic = real_mono  # type: ignore[assignment]
        proxy.time.time = real_time  # type: ignore[assignment]
    if decisions is not None:
        decisions += [d for d in op.decisions if d["t"] <= end]
    return [c for c in w.calls if c[1] <= end]


def run_headless(seed: int, preset: str, end: float, extra: dict | None = None, world: World | None = None,
                 decisions: list | None = None) -> list[tuple]:
    w = world or World()
    op = Operator(FakeSim(w), operator_model({"preset": preset, **(extra or {})}), seed, "op-auto", "np", 0.0,
                  loss_confirmer="op-2", loss_note_prefix="np2")
    t = op.start(0.0)
    while t is not None and t <= end:
        w.now = t
        t = op.wake(t)
    if decisions is not None:
        decisions += op.decisions
    return w.calls


@pytest.mark.parametrize("seed", [1, 2, 3, 7, 101])
@pytest.mark.parametrize("preset", ["default", "fatigued", "diligent", "rubber-stamp"])
def test_same_decisions_after_the_same_delays(seed: int, preset: str):
    a = run_proxy(seed, preset, 400.0)
    b = run_headless(seed, preset, 400.0)
    assert a, "the proxy decided nothing"
    assert a == b


def _comparable(decisions: list[dict]) -> list[dict]:
    """A decision record as both write it: the time to the millisecond; the open duration (the
    live class truncates it, the headless one rounds) and the headless `opened_t` left out."""
    return [{**{k: v for k, v in d.items() if k not in ("open_ms", "opened_t")}, "t": round(d["t"], 3)}
            for d in decisions]


@pytest.mark.parametrize("seed", [1, 2, 3, 7, 101])
@pytest.mark.parametrize("preset", ["default", "rubber-stamp"])
def test_grant_agents_and_confirm_rule_declarations_as_the_live_operator(seed: int, preset: str):
    """murmur-pk8: a situation's `[autoplay] grant_agents` and `confirm_rule_declarations` (eval/proxy.py)
    are honoured: the granted agent's cards read without the draw and approved whatever the rule book
    finds, saying so; after each look's cards, every confirmable rule declaration read and confirmed,
    once. The same calls after the same delays, and the same decision records, as the live class."""
    da: list[dict] = []
    db: list[dict] = []
    a = run_proxy(seed, preset, 400.0, GRANT, World(GRANT_CARDS, DECLARATIONS), da)
    b = run_headless(seed, preset, 400.0, GRANT, World(GRANT_CARDS, DECLARATIONS), db)
    assert a == b
    assert _comparable(da) == _comparable(db)
    # every card of the granted agent approved, read, saying so, the armed one against the rule book
    granted = {d["intent_id"]: d for d in db if d.get("granted")}
    assert set(granted) == {4, 6, 8, 10}
    assert all(d["action"] == "approve" and d["diligent"] and "(grant_agents)" in d["reason"] for d in granted.values())
    assert granted[6]["if_read"] == "reject" and "the rule book alone: reject" in granted[6]["reason"]
    assert all(d["granted"] is False for d in db if d["intent_id"] is not None and d["intent_id"] not in granted)
    # each confirmable declaration confirmed once, read first; D-3 (no confirm) and D-4 (no track) left
    confirms = [c for c in b if c[0] == "confirm"]
    assert [c[2] for c in confirms] == ["T-0003", "T-0007", "T-0011"]
    assert [d["declaration"] for d in db if d["action"] == "confirm"] == ["D-1", "D-2", "D-5"]
    assert all(d["result"] == "confirmed" and "confirm_rule_declarations" in d["reason"]
               for d in db if d["action"] == "confirm")
    d1 = next(c for c in confirms if c[2] == "T-0003")
    assert d1[1] > 40.0, "read after it came into the list"


@pytest.mark.parametrize("seed", [1, 2, 3, 7, 101])
@pytest.mark.parametrize("preset", ["default", "rubber-stamp"])
def test_confirm_node_losses_as_the_live_operator(seed: int, preset: str):
    """Rules 30: `[autoplay] confirm_node_losses` confirms each presumed loss of one of ours once a
    silence, read first, after the look's declarations, as the loss confirmer (op-2), never op-auto,
    on the `since` the item showed, with a note saying it is scripted; a silent vehicle flagged by
    hand is left alone. The same calls after the same delays, and the same records, live and
    headless."""
    extra = {**GRANT, "confirm_node_losses": True}
    da: list[dict] = []
    db: list[dict] = []
    a = run_proxy(seed, preset, 400.0, extra, World(GRANT_CARDS, DECLARATIONS, NODE_LOSSES), da)
    b = run_headless(seed, preset, 400.0, extra, World(GRANT_CARDS, DECLARATIONS, NODE_LOSSES), db)
    assert a == b
    assert _comparable(da) == _comparable(db)
    lost = [c for c in b if c[0] == "lost"]
    assert [(c[2], c[3]) for c in lost] == [(1, 125_000), (4, None), (1, 300_000)]
    assert lost[0][1] > 50.0, "read after it came into the list"
    assert all("np2: confirms every presumed loss" in c[4] and "a script, not a judgment" in c[4] for c in lost)
    assert "HUNTER 1 (uid 1) silent since its report at 125.0" in lost[0][4] and "its struck report" in lost[0][4]
    assert "SKY 1 (uid 4) silent since it never reported a position" in lost[1][4]
    recs = [d for d in db if d["action"] == "confirm_lost"]
    assert [d["by"] for d in recs] == ["op-2"] * 3
    assert recs[0]["result"] == "confirmed lost (ended [7])" and recs[1]["result"] == "confirmed lost"
    # every declaration still confirmed (op-auto), and the declarations before the losses in a look
    assert [d["declaration"] for d in db if d["action"] == "confirm"] == ["D-1", "D-2", "D-5"]


def test_confirm_node_losses_needs_a_confirmer_and_reads_as_before_without_it():
    """A proxy told to confirm losses names who confirms (an operator with engagement authority);
    without the setting the operator's params read as they always did (the headless golden and
    paused digests hold them), and no loss is confirmed."""
    params = proxy.proxy_params({"confirm_node_losses": True})
    with pytest.raises(ValueError, match="loss_confirmer"):
        proxy.ProxyOperator(FakeBridge(World()), params, 1, operator="op-auto")
    with pytest.raises(ValueError, match="loss_confirmer"):
        Operator(FakeSim(World()), operator_model({"confirm_node_losses": True}), 1, "op-auto", "np", 0.0)
    assert "confirm_node_losses" not in proxy.proxy_params({}).as_dict()
    assert proxy.proxy_params({"confirm_node_losses": True}).as_dict()["confirm_node_losses"] is True
    w = World(GRANT_CARDS, DECLARATIONS, NODE_LOSSES)
    run_headless(1, "default", 400.0, GRANT, w, [])
    assert not [c for c in w.calls if c[0] == "lost"]


def test_without_the_settings_nothing_changes():
    """A situation that sets neither: no review read, no `granted` key (a record as before)."""
    db: list[dict] = []
    w = World(GRANT_CARDS, DECLARATIONS)
    run_headless(1, "default", 400.0, None, w, db)
    assert db and not [c for c in w.calls if c[0] == "confirm"] and not any("granted" in d for d in db)


def test_modes_and_parameters_are_checked():
    assert operator_model(None).mode == "proxy"
    assert operator_model({"mode": "rates", "approve": 0.5, "reject": 0.2}).as_dict()["ignore"] == pytest.approx(0.3)
    with pytest.raises(ValueError):
        operator_model({"mode": "rates", "approve": 0.9, "reject": 0.2})
    with pytest.raises(ValueError):
        operator_model({"mode": "telepathic"})
    with pytest.raises(ValueError):
        operator_model({"preset": "default", "no_such_parameter": 1})
