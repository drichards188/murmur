"""The operator on simulated time (docs/HEADLESS.md, section 7).

**A model for load and regression testing, not a model of human judgment.** The default
(`mode = "proxy"`) is the full stack's autoplay operator: `op-auto` in `make situation ...
OPERATOR=autoplay`, which is eval/proxy.py's `ProxyOperator` with the situation's `[autoplay]`
preset and seed. Its sequence is re-expressed here as a process of the episode's event loop,
with the same parameters (`ProxyParams`, `proxy_params`, `PRESETS`), the same rule book
(`judge`, eval/evidence.py v2), the same card view (`shown_on`), the same notes, and the same
draws in the same order from the same generator (`random.Random(seed * 7919 + 17)`): a look
interval, then per card whether it reads it and how long it takes, then the next look interval.
`tests/test_operator.py` runs eval/proxy.py's own class against a fake bridge and checks the
two make the same decisions after the same delays.

What it rewards: it approves every card it does not read, and it reads less as the queue grows.
Anything optimized through it learns that a longer queue gets more approved unread, a policy of
flooding the inbox, which is what Murmur exists to detect. Reward from truth only, train with
`approve-all`, never tune the operator-facing parameters (budget, cooldowns, materiality)
against it. Its numbers are outputs of a model with stated parameters, never human data.

Modes:

- `proxy` (default): as above.
- `approve-all`, `reject-all`: eval/proxy.py's `AutoApprover`, a look every second.
- `never`: answers nothing; every human-tier card expires (the shadow's absent operator).
- `rates`: a fixed operator: each new card is approved with probability `approve`, rejected
  with `reject`, else left to expire, after a lognormal latency (`latency_median_s`,
  `latency_sigma`); with `reads`, a card the rule book objects to is rejected before the draw.
"""

from __future__ import annotations

import math
import random
from dataclasses import asdict, dataclass, field
from typing import Any

from eval.proxy import PRESETS, ProxyParams, judge, proxy_params, shown_on

MODES = ("proxy", "approve-all", "reject-all", "never", "rates")


@dataclass
class OperatorModel:
    mode: str = "proxy"
    params: ProxyParams = field(default_factory=ProxyParams)
    # rates mode
    approve: float = 1.0
    reject: float = 0.0
    latency_median_s: float = 20.0
    latency_sigma: float = 0.4
    reads: bool = False

    def as_dict(self) -> dict[str, Any]:
        out: dict[str, Any] = {"mode": self.mode}
        if self.mode == "proxy":
            out["params"] = self.params.as_dict()
        elif self.mode == "rates":
            out.update(approve=self.approve, reject=self.reject, ignore=round(1.0 - self.approve - self.reject, 6),
                       latency_median_s=self.latency_median_s, latency_sigma=self.latency_sigma, reads=self.reads)
        return out


def operator_model(spec: dict[str, Any] | None = None) -> OperatorModel:
    """From a situation's `[autoplay]` table (`preset` and eval/proxy.py's keys) or a batch's
    operator option: `{"mode": ..., ...}`."""
    spec = dict(spec or {})
    mode = str(spec.pop("mode", "proxy"))
    if mode not in MODES:
        raise ValueError(f"unknown operator mode {mode!r}; modes: {', '.join(MODES)}")
    m = OperatorModel(mode=mode)
    if mode == "proxy":
        m.params = proxy_params(spec)
    elif mode == "rates":
        for k in ("approve", "reject", "latency_median_s", "latency_sigma"):
            if k in spec:
                setattr(m, k, float(spec[k]))
        m.reads = bool(spec.get("reads", False))
        if m.approve < 0 or m.reject < 0 or m.approve + m.reject > 1.0 + 1e-9:
            raise ValueError("rates: approve and reject are probabilities that sum to at most 1")
    return m


class Operator:
    """One operator id deciding the inbox, as a process: `wake(now)` acts at `now` and returns
    when it next wants to act (None: never). It reaches the bridge only through the operator
    API (`sim.api`), as the full stack's runner does."""

    def __init__(self, sim: Any, model: OperatorModel, seed: int, operator: str, note_prefix: str,
                 epoch: float):
        self.sim = sim
        self.m = model
        self.p = model.params
        self.rng = random.Random(seed * 7919 + 17)
        self.operator = operator
        self.note_prefix = note_prefix
        self.epoch = epoch
        self.handled: set[int] = set()
        self.decisions: list[dict[str, Any]] = []
        # proxy: the cards of the current look still to decide, and the one being read
        self._cards: list[dict[str, Any]] = []
        self._queue = 0
        self._open: dict[str, Any] | None = None
        # rates: card id -> (decide at, action)
        self._due: dict[int, tuple[float, str, bool]] = {}
        self.started = False

    def describe(self) -> dict[str, Any]:
        return {"operator": self.operator, **self.m.as_dict(), "proxy": self.m.mode == "proxy"}

    def _lognormal(self, median: float, sigma: float) -> float:
        return median * math.exp(self.rng.gauss(0.0, sigma))

    def start(self, now: float) -> float | None:
        self.started = True
        if self.m.mode == "never":
            return None
        if self.m.mode in ("approve-all", "reject-all", "rates"):
            return now
        return now + self._lognormal(self.p.attend_s, self.p.attend_sigma)

    def _inbox(self) -> list[dict[str, Any]]:
        status, body = self.sim.api("GET", "/api/inbox")
        return list((body or {}).get("pending", [])) if status == 200 else []

    def _post(self, method: str, path: str, body: Any) -> tuple[int, Any]:
        return self.sim.api(method, path, body, self.operator)

    def _view(self, iid: int, event: str, shown: dict, now: float, action: str | None = None,
              open_ms: int | None = None) -> None:
        body: dict[str, Any] = {"intent_id": iid, "event": event, "shown": shown,
                                "client_t": int((self.epoch + now) * 1000)}
        if action:
            body["action"] = action
        if open_ms is not None:
            body["open_ms"] = int(open_ms)
        self._post("POST", "/api/operator/view", body)

    def _answer(self, iid: int, action: str, note: str) -> str:
        status, body = self._post("POST", f"/api/intents/{iid}/{action}", {"note": note})
        if status // 100 == 2:
            return str(((body or {}).get("intent") or {}).get("state"))
        detail = body.get("detail") if isinstance(body, dict) else body
        return f"error {status}: {detail}"

    def wake(self, now: float) -> float | None:
        if self.m.mode in ("approve-all", "reject-all"):
            return self._auto(now)
        if self.m.mode == "rates":
            return self._rates(now)
        if self.m.mode == "proxy":
            return self._proxy(now)
        return None

    # ---- eval/proxy.py's AutoApprover --------------------------------------------------------

    def _auto(self, now: float) -> float:
        action = "approve" if self.m.mode == "approve-all" else "reject"
        note = f"rules-only baseline: {action} all"
        for i in self._inbox():
            state = self._answer(int(i["id"]), action, note)
            self.decisions.append({"t": round(now, 3), "intent_id": int(i["id"]), "action": action, "result": state})
        return now + 1.0

    # ---- a fixed operator --------------------------------------------------------------------

    def _rates(self, now: float) -> float:
        pending = {int(i["id"]): i for i in self._inbox() if i.get("kind") != "engage"}
        for iid, card in sorted(pending.items()):
            if iid in self.handled or iid in self._due:
                continue
            objects = False
            if self.m.reads:
                status, tracks = self.sim.api("GET", "/api/tracks")
                found = judge(card, list((tracks or {}).get("tracks", [])), self.p)
                objects = found.action == "reject"
            u = self.rng.random()
            action = "reject" if objects else ("approve" if u < self.m.approve else
                                              ("reject" if u < self.m.approve + self.m.reject else "ignore"))
            lat = self._lognormal(self.m.latency_median_s, self.m.latency_sigma)
            self._due[iid] = (now + lat, action, objects)
        for iid, (at, action, objects) in sorted(self._due.items(), key=lambda kv: (kv[1][0], kv[0])):
            if at > now or iid in self.handled:
                continue
            self.handled.add(iid)
            if action == "ignore" or iid not in pending:
                self.decisions.append({"t": round(now, 3), "intent_id": iid, "action": action,
                                       "result": "ignored" if action == "ignore" else "gone"})
                continue
            why = "the rule book objects" if objects else "fixed rates"
            state = self._answer(iid, action, f"{self.note_prefix} (rates): {why}")
            self.decisions.append({"t": round(now, 3), "intent_id": iid, "action": action, "result": state})
        later = [at for iid, (at, _, _) in self._due.items() if iid not in self.handled and at > now]
        return min([now + 1.0, *later])

    # ---- eval/proxy.py's ProxyOperator ---------------------------------------------------------

    def _proxy(self, now: float) -> float | None:
        if self._open is not None:
            self._decide(now)
            return self._next_card(now)
        # a look: the inbox, soonest expiry first, engagement nominations left to the signers
        pending = sorted(self._inbox(), key=lambda i: i.get("expires_t") or 0)
        pending = [i for i in pending if i.get("kind") != "engage"]
        self._queue = len(pending)
        self._cards = [i for i in pending if int(i["id"]) not in self.handled]
        return self._next_card(now)

    def _next_card(self, now: float) -> float:
        if not self._cards:
            return now + self._lognormal(self.p.attend_s, self.p.attend_sigma)
        card = self._cards.pop(0)
        p = self.p
        iid = int(card["id"])
        shown = shown_on(card)
        self._view(iid, "open", shown, now)
        load = max(0.0, (self._queue - 1) / p.fatigue_queue) if p.fatigue_queue > 0 else 0.0
        fatigue = 2.0 ** (-load)
        trust = p.trust_agent if (card.get("source") or {}).get("kind") == "agent" else 1.0
        p_read = min(1.0, max(0.0, p.diligence * fatigue * trust))
        diligent = self.rng.random() < p_read
        hurry = 1.0 - p.hurry * min(1.0, load)
        if diligent:
            n_checks = len(card.get("checks", []))
            seconds = self._lognormal(p.read_s + p.read_per_check_s * n_checks, p.read_sigma) * hurry
        else:
            seconds = self._lognormal(p.skim_s, p.read_sigma) * hurry
        self._open = {"card": card, "shown": shown, "opened": now, "diligent": diligent, "p_read": p_read,
                      "queue": self._queue}
        return now + seconds

    def _decide(self, now: float) -> None:
        o, self._open = self._open, None
        assert o is not None
        card, shown, p = o["card"], o["shown"], self.p
        iid = int(card["id"])
        status, body = self.sim.api("GET", "/api/tracks")
        tracks = list((body or {}).get("tracks", [])) if status == 200 else []
        found = judge(card, tracks, p)
        j = found if o["diligent"] else type(found)("approve", "rubber stamp: not read", [])
        shown["tracks"] = j.tracks
        open_ms = int(round((now - o["opened"]) * 1000))
        self._view(iid, "decide", shown, now, j.action, open_ms)
        note = f"{self.note_prefix} ({p.preset}, {'read' if o['diligent'] else 'skimmed'}, queue {o['queue']}): {j.reason}"
        result = self._answer(iid, j.action, note)
        self.handled.add(iid)
        self.decisions.append({
            "t": round(now, 3), "intent_id": iid, "action": j.action, "reason": j.reason, "diligent": o["diligent"],
            "if_read": found.action, "queue": o["queue"], "p_read": round(o["p_read"], 3), "open_ms": open_ms,
            "opened_t": round(o["opened"], 3), "tracks": j.tracks, "result": result,
        })


__all__ = ["MODES", "OperatorModel", "Operator", "operator_model", "PRESETS", "asdict"]
