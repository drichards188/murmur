"""Supervisors: who decides the inbox.

**The proxy operator is not a human and its numbers are not human data.** It is a scripted
model of an operator with explicit parameters, built to exercise the metrics end to end
(latency against information shown, fatigue against queue length, catch rate against
diligence) and to give the rules-only baseline something to be compared with. A claim about
human behaviour needs participants (`eval/README.md`, study mode). Every parameter below is
recorded in the run's `run.json` and shown in the report next to the word "proxy".

Model (`ProxyOperator`), per look at the inbox:

1. **Attention.** The proxy looks at the inbox every `attend_s` seconds (median; lognormal
   with `attend_sigma`). Between looks it does not see new cards.
2. **Reading.** For each pending card, soonest expiry first, it "opens" the card (posting
   an `operator.view open` event with what the card shows: check rule ids, warning lines,
   whether a rationale is on it) and spends `read_s + read_per_check_s x checks` seconds
   (lognormal `read_sigma`) if diligent, else `skim_s`.
3. **Diligence.** With probability `diligence x fatigue x trust` it reads properly:
   `fatigue = 2^(-(queue - 1) / fatigue_queue)` (the queue length when it opened the card;
   `fatigue_queue` cards halve diligence), `trust = trust_agent` for agent-sourced cards
   (automation bias: below 1 the proxy scrutinises agent proposals less, above 1 more).
   Reading time also shrinks under load: multiplied by `1 - hurry x min(1, (queue - 1) /
   fatigue_queue)`.
4. **Judgment, when diligent.** For a task on an unarmed platform it approves (with
   probability `caution` it rejects one with an `unverified` check). For a task on an
   armed platform (the card's "armed platform" warning) it looks up every track the
   rationale names (`T-nnnn`) on `/api/tracks/{id}` (and records them on the `decide` view
   event) and rejects when: the track is gone; the track is stale (or older than
   `stale_s`); its affiliation estimate is neutral or assumed friend; it carries an AIS
   identity and is not declared hostile; or the proposal's centre is more than
   `max(1000 m, 3 sigma)` from the track's position (the proposal is not about the track it
   cites). Otherwise, with probability `caution` it rejects a card that has an
   `unverified` check or names no track at all; else it approves.
5. **Judgment, when not diligent.** It approves (a rubber stamp): the card passed the
   rules, and nothing on it says otherwise.
6. It posts an `operator.view decide` event with the open duration, then approves or
   rejects over the operator API as `op-1` with a note saying which rule fired.

`AutoApprover` is the rules-only baseline: a script that approves (or, in `reject` mode,
rejects) every pending intent on its next look, as `op-auto`, posting no view events. It
stands in for "no human in the loop": policy and rules alone, with the human tier closed by
a constant answer.

`Participant` does nothing: a real person decides in the console (study mode) and the
console posts the view events itself (`web/src/operatorView.ts`).
"""

from __future__ import annotations

import asyncio
import math
import random
import re
import time
from dataclasses import asdict, dataclass, field
from typing import Any

from .bridge import Bridge, BridgeError

TRACK_RE = re.compile(r"\bT-\d{4}\b")


@dataclass
class ProxyParams:
    attend_s: float = 20.0
    attend_sigma: float = 0.4
    read_s: float = 6.0
    read_per_check_s: float = 1.5
    read_sigma: float = 0.3
    skim_s: float = 2.0
    diligence: float = 0.8
    fatigue_queue: float = 6.0
    hurry: float = 0.5
    caution: float = 0.3
    trust_agent: float = 1.0
    stale_s: float = 10.0
    preset: str = "default"

    def as_dict(self) -> dict[str, Any]:
        return asdict(self)


PRESETS: dict[str, dict[str, Any]] = {
    "default": {},
    "diligent": {"diligence": 1.0, "attend_s": 10.0, "caution": 0.5, "fatigue_queue": 12.0},
    "rubber-stamp": {"diligence": 0.1, "attend_s": 10.0, "skim_s": 1.5, "caution": 0.0},
    "fatigued": {"fatigue_queue": 3.0, "hurry": 0.8, "attend_s": 25.0},
    "trusting": {"trust_agent": 0.5},
}


def proxy_params(spec_proxy: dict[str, Any], preset: str | None = None) -> ProxyParams:
    name = preset or str(spec_proxy.get("preset", "default"))
    if name not in PRESETS:
        raise ValueError(f"unknown proxy preset {name!r}; presets: {', '.join(PRESETS)}")
    values: dict[str, Any] = dict(PRESETS[name])
    values.update({k: v for k, v in spec_proxy.items() if k != "preset"})
    p = ProxyParams(preset=name)
    for k, v in values.items():
        if not hasattr(p, k):
            raise ValueError(f"unknown proxy parameter {k!r}")
        setattr(p, k, float(v) if isinstance(getattr(p, k), float) else v)
    return p


def haversine_m(lon1: float, lat1: float, lon2: float, lat2: float) -> float:
    r = 6371000.0
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dp = p2 - p1
    dl = math.radians(lon2 - lon1)
    a = math.sin(dp / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(dl / 2) ** 2
    return 2 * r * math.asin(math.sqrt(a))


def shown_on(intent: dict) -> dict[str, Any]:
    """What the console card shows (mirrors web/src/operatorView.ts `shownOn`)."""
    checks = []
    for c in intent.get("checks", []):
        rule = c.get("rule", "?")
        checks.append(f"{rule}:{c['uid']}" if c.get("uid") is not None else rule)
    return {
        "checks": checks,
        "warnings": list((intent.get("why") or [])[1:]),
        "rationale": bool(intent.get("rationale")),
        "tracks": [],
    }


@dataclass
class Judgment:
    action: str
    reason: str
    tracks: list[str] = field(default_factory=list)


def armed_task(intent: dict) -> bool:
    """The card says an armed platform is involved (policy facts, shown as a warning)."""
    return bool(((intent.get("policy") or {}).get("facts") or {}).get("armed"))


def judge(intent: dict, tracks: dict[str, dict | None], rng: random.Random, p: ProxyParams, now: float) -> Judgment:
    """The diligent proxy's rule book (pure, so tests can drive it)."""
    named = TRACK_RE.findall(intent.get("rationale") or "")
    seen = list(dict.fromkeys(named))
    center = (intent.get("params") or {}).get("center")
    armed = armed_task(intent)
    unverified = any(c.get("outcome") == "unverified" for c in intent.get("checks", []))
    if not armed:
        # a sensor or transport task on an unarmed platform: the picture it cites does not
        # make it harmful; only an unverified check gives pause
        if unverified and rng.random() < p.caution:
            return Judgment("reject", "caution: an unverified check", seen)
        return Judgment("approve", "unarmed task, checks pass", seen)
    for tid in seen:
        t = tracks.get(tid)
        if t is None:
            return Judgment("reject", f"{tid} named in the rationale is no longer tracked", seen)
        if t.get("state") == "stale" or (now - float(t.get("last_seen_t", now))) > p.stale_s:
            return Judgment("reject", f"{tid} is stale (last seen {now - float(t.get('last_seen_t', now)):.0f} s ago)", seen)
        declared = (t.get("declared") or {}).get("affiliation")
        aff = declared or t.get("affiliation")
        if aff in ("neutral", "assumed_friend"):
            return Judgment("reject", f"{tid} is estimated {aff}: {t.get('affiliation_reason', '')}", seen)
        ais = (t.get("identity") or {}).get("ais")
        if ais and aff != "hostile":
            return Judgment("reject", f"{tid} carries an AIS identity ({ais.get('name')}) and is not declared hostile", seen)
        if center and t.get("position"):
            d = haversine_m(center[0], center[1], t["position"][0], t["position"][1])
            if d > max(1000.0, 3.0 * float(t.get("pos_sigma_m", 0.0))):
                return Judgment("reject", f"the proposal's centre is {d / 1000:.1f} km from {tid}", seen)
    if (unverified or not seen) and rng.random() < p.caution:
        why = "an unverified check" if unverified else "no track named for a consequential task"
        return Judgment("reject", f"caution: {why}", seen)
    return Judgment("approve", "checks pass and the cited picture holds" if seen else "checks pass", seen)


class Supervisor:
    name = "none"
    operator = "op-1"

    async def run(self, stop: asyncio.Event) -> None:
        await stop.wait()

    def describe(self) -> dict[str, Any]:
        return {"name": self.name, "operator": self.operator}


class Participant(Supervisor):
    name = "participant"
    operator = "op-study"


class AutoApprover(Supervisor):
    """Rules-only baseline: approve (or reject) everything that waits, at once."""

    name = "auto"
    operator = "op-auto"

    def __init__(self, bridge: Bridge, mode: str = "approve", period_s: float = 1.0):
        if mode not in ("approve", "reject"):
            raise ValueError("mode must be approve or reject")
        self.bridge = bridge
        self.mode = mode
        self.period_s = period_s
        self.decisions: list[dict[str, Any]] = []

    def describe(self) -> dict[str, Any]:
        return {"name": self.name, "operator": self.operator, "mode": self.mode, "period_s": self.period_s}

    async def run(self, stop: asyncio.Event) -> None:
        while not stop.is_set():
            inbox = await asyncio.to_thread(self.bridge.try_get, "/api/inbox")
            for i in (inbox or {}).get("pending", []):
                try:
                    if self.mode == "approve":
                        out = await asyncio.to_thread(self.bridge.approve, i["id"], "rules-only baseline: approve all", self.operator)
                    else:
                        out = await asyncio.to_thread(self.bridge.reject, i["id"], "rules-only baseline: reject all", self.operator)
                    state = (out.get("intent") or {}).get("state")
                except BridgeError as e:
                    state = f"error {e.status}"
                self.decisions.append({"t": time.time(), "intent_id": i["id"], "action": self.mode, "result": state})
            try:
                await asyncio.wait_for(stop.wait(), timeout=self.period_s)
            except asyncio.TimeoutError:
                pass


class ProxyOperator(Supervisor):
    name = "proxy"
    operator = "op-1"

    def __init__(self, bridge: Bridge, params: ProxyParams, seed: int, log: list[dict[str, Any]] | None = None):
        self.bridge = bridge
        self.p = params
        self.rng = random.Random(seed * 7919 + 17)
        self.decisions: list[dict[str, Any]] = log if log is not None else []
        self.handled: set[int] = set()

    def describe(self) -> dict[str, Any]:
        return {"name": self.name, "operator": self.operator, "params": self.p.as_dict(), "proxy": True}

    def _lognormal(self, median: float, sigma: float) -> float:
        return median * math.exp(self.rng.gauss(0.0, sigma))

    async def _sleep(self, stop: asyncio.Event, seconds: float) -> bool:
        try:
            await asyncio.wait_for(stop.wait(), timeout=max(0.0, seconds))
            return True
        except asyncio.TimeoutError:
            return False

    async def run(self, stop: asyncio.Event) -> None:
        while not stop.is_set():
            if await self._sleep(stop, self._lognormal(self.p.attend_s, self.p.attend_sigma)):
                return
            inbox = await asyncio.to_thread(self.bridge.try_get, "/api/inbox")
            pending = sorted((inbox or {}).get("pending", []), key=lambda i: i.get("expires_t") or 0)
            queue = len(pending)
            for i in pending:
                if stop.is_set():
                    return
                if i["id"] in self.handled:
                    continue
                await self._decide(i, queue)
                self.handled.add(i["id"])

    async def _decide(self, intent: dict, queue: int) -> None:
        p = self.p
        iid = int(intent["id"])
        opened = time.monotonic()
        shown = shown_on(intent)
        try:
            await asyncio.to_thread(self.bridge.view, iid, "open", shown, self.operator)
        except (BridgeError, OSError):
            pass
        load = max(0.0, (queue - 1) / p.fatigue_queue) if p.fatigue_queue > 0 else 0.0
        fatigue = 2.0 ** (-load)
        trust = p.trust_agent if (intent.get("source") or {}).get("kind") == "agent" else 1.0
        p_read = min(1.0, max(0.0, p.diligence * fatigue * trust))
        diligent = self.rng.random() < p_read
        hurry = 1.0 - p.hurry * min(1.0, load)
        if diligent:
            n_checks = len(intent.get("checks", []))
            seconds = self._lognormal(p.read_s + p.read_per_check_s * n_checks, p.read_sigma) * hurry
        else:
            seconds = self._lognormal(p.skim_s, p.read_sigma) * hurry
        await asyncio.sleep(seconds)
        looked: dict[str, dict | None] = {}
        if diligent and armed_task(intent):
            for tid in dict.fromkeys(TRACK_RE.findall(intent.get("rationale") or "")):
                looked[tid] = await asyncio.to_thread(self.bridge.track, tid)
            j = judge(intent, looked, self.rng, p, time.time())
        else:
            j = Judgment("approve", "rubber stamp: not read", [])
        shown["tracks"] = j.tracks
        open_ms = int((time.monotonic() - opened) * 1000)
        try:
            await asyncio.to_thread(self.bridge.view, iid, "decide", shown, self.operator, j.action, open_ms)
        except (BridgeError, OSError):
            pass
        note = f"proxy ({p.preset}, {'read' if diligent else 'skimmed'}, queue {queue}): {j.reason}"
        try:
            if j.action == "approve":
                out = await asyncio.to_thread(self.bridge.approve, iid, note, self.operator)
            else:
                out = await asyncio.to_thread(self.bridge.reject, iid, note, self.operator)
            result = (out.get("intent") or {}).get("state")
        except BridgeError as e:
            result = f"error {e.status}: {e.body.get('detail') if isinstance(e.body, dict) else e.body}"
        self.decisions.append(
            {
                "t": time.time(),
                "intent_id": iid,
                "action": j.action,
                "reason": j.reason,
                "diligent": diligent,
                "queue": queue,
                "p_read": round(p_read, 3),
                "open_ms": open_ms,
                "tracks": j.tracks,
                "result": result,
            }
        )


def make_supervisor(kind: str, bridge: Bridge, params: ProxyParams, seed: int) -> Supervisor:
    if kind == "proxy":
        return ProxyOperator(bridge, params, seed)
    if kind == "auto-approve":
        return AutoApprover(bridge, "approve")
    if kind == "auto-reject":
        return AutoApprover(bridge, "reject")
    if kind == "participant":
        return Participant()
    raise ValueError(f"unknown supervisor {kind!r}")
