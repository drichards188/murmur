"""The simulated-time operator against eval/proxy.py's own ProxyOperator: the same cards, the same
seed, the same looks, reads and decisions after the same delays (operator.py's claim)."""

from __future__ import annotations

import asyncio
import re

import pytest

import eval.proxy as proxy
from eval.headless.operator import Operator, operator_model


def _card(iid: int, t_in: float, checks: int = 6, agent: bool = True) -> dict:
    return {"id": iid, "kind": "orbit", "state": "pending_approval", "t_in": t_in, "expires_t": int((t_in + 120) * 1000),
            "source": {"kind": "agent" if agent else "human", "id": "agent-playbook"},
            "checks": [{"rule": "capability", "uid": "3", "outcome": "pass"}] * checks,
            "params": {"center": [-122.5, 37.8], "radius_m": 1000.0}, "policy": {"facts": {"armed": False}},
            "why": ["needs a human", "a warning"], "rationale": "r"}


CARDS = [_card(4, 5.0), _card(5, 30.0), _card(6, 31.0, 3), _card(7, 32.0, agent=False), _card(8, 33.0),
         _card(9, 200.0), _card(10, 201.0)]


class World:
    """The inbox over (fake) time: a card is pending from its `t_in` until decided."""

    def __init__(self):
        self.now = 0.0
        self.decided: dict[int, str] = {}
        self.calls: list[tuple] = []

    def pending(self) -> list[dict]:
        return [c for c in CARDS if c["t_in"] <= self.now and c["id"] not in self.decided]

    def decide(self, iid: int, action: str) -> dict:
        self.decided[iid] = action
        return {"intent": {"id": iid, "state": "dispatched" if action == "approve" else "rejected"}}


class FakeBridge:
    """eval/bridge.py's surface for ProxyOperator."""

    def __init__(self, w: World):
        self.w = w

    def try_get(self, path: str):
        assert path == "/api/inbox"
        return {"pending": self.w.pending()}

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
        if path == "/api/tracks":
            return 200, {"tracks": []}
        if path == "/api/operator/view":
            self.w.calls.append(("view", round(self.w.now, 6), body["intent_id"], body["event"], body.get("action")))
            return 200, {}
        m = re.match(r"/api/intents/(\d+)/(approve|reject)", path)
        assert m, path
        iid, action = int(m.group(1)), m.group(2)
        self.w.calls.append((action, round(self.w.now, 6), iid, body["note"]))
        return 200, self.w.decide(iid, action)


def run_proxy(seed: int, preset: str, end: float) -> list[tuple]:
    w = World()
    params = proxy.proxy_params({"preset": preset})
    op = proxy.ProxyOperator(FakeBridge(w), params, seed, operator="op-auto", note_prefix="np")

    async def fake_sleep(seconds: float) -> None:
        w.now += max(0.0, seconds)

    async def fake__sleep(stop, seconds: float) -> bool:
        w.now += max(0.0, seconds)
        return w.now > end

    op._sleep = fake__sleep  # type: ignore[method-assign]
    real_sleep = proxy.asyncio.sleep
    real_mono = proxy.time.monotonic
    proxy.asyncio.sleep = fake_sleep  # type: ignore[assignment]
    proxy.time.monotonic = lambda: w.now  # type: ignore[assignment]
    try:
        asyncio.run(op.run(asyncio.Event()))
    finally:
        proxy.asyncio.sleep = real_sleep  # type: ignore[assignment]
        proxy.time.monotonic = real_mono  # type: ignore[assignment]
    return [c for c in w.calls if c[1] <= end]


def run_headless(seed: int, preset: str, end: float) -> list[tuple]:
    w = World()
    op = Operator(FakeSim(w), operator_model({"preset": preset}), seed, "op-auto", "np", 0.0)
    t = op.start(0.0)
    while t is not None and t <= end:
        w.now = t
        t = op.wake(t)
    return w.calls


@pytest.mark.parametrize("seed", [1, 2, 3, 7, 101])
@pytest.mark.parametrize("preset", ["default", "fatigued", "diligent", "rubber-stamp"])
def test_same_decisions_after_the_same_delays(seed: int, preset: str):
    a = run_proxy(seed, preset, 400.0)
    b = run_headless(seed, preset, 400.0)
    assert a, "the proxy decided nothing"
    assert a == b


def test_modes_and_parameters_are_checked():
    assert operator_model(None).mode == "proxy"
    assert operator_model({"mode": "rates", "approve": 0.5, "reject": 0.2}).as_dict()["ignore"] == pytest.approx(0.3)
    with pytest.raises(ValueError):
        operator_model({"mode": "rates", "approve": 0.9, "reject": 0.2})
    with pytest.raises(ValueError):
        operator_model({"mode": "telepathic"})
    with pytest.raises(ValueError):
        operator_model({"preset": "default", "no_such_parameter": 1})
