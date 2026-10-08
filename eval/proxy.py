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
   (lognormal `read_sigma`) if it reads the card, else `skim_s`.
3. **Whether it reads.** With probability `P(read) = diligence x fatigue x trust` it reads
   the card: `fatigue = 2^(-(queue - 1) / fatigue_queue)` (the queue length when it looked;
   `fatigue_queue` cards halve it), `trust = trust_agent` for agent-sourced cards. Reading
   time also shrinks under load: multiplied by `1 - hurry x min(1, (queue - 1) /
   fatigue_queue)`.
4. **When it reads**, it applies the generic rule book (eval/evidence.py, version
   `RULEBOOK_VERSION`) to the card and to the tracks the C2 shows now (`/api/tracks`): it
   rejects when anything the C2 shows speaks against the task (a failed or unverified
   check; for an armed task aimed at a point: no track within its radius, a track there
   shown neutral or assumed friend, the nearest shown stale or with an affiliation
   confidence below `c_min`), else it approves. The tracks it looked at go on the `decide`
   view event.
5. **When it skims**, it approves (a rubber stamp).
6. It posts an `operator.view decide` event with the open duration, then approves or
   rejects over the operator API as `op-1` with a note saying which rule fired.

So, by construction, the proxy catches a wrong card exactly when it reads it and the C2
shows something against it: its catch rate on such cards is P(read), a parameter, and on
cards the C2 shows nothing against it is zero. The report presents its numbers that way
("caught if read; P(read) = ..."), as outputs of this model and never as estimates of what
people do.

`AutoApprover` is the rules-only baseline: a script that approves (or, in `reject` mode,
rejects) every pending intent on its next look, as `op-auto`, posting no view events. It
stands in for "no human in the loop": policy and rules alone, with the human tier closed by
a constant answer.

`Participant` does nothing: a real person decides in the console (study mode) and the
console posts the view events itself (`web/src/operatorView.ts`).

`EngagementSigners` signs engagement nominations as two (or more) distinct scripted operators
with engagement authority, the way the engine requires it: each asks for the picture first
and quotes its token, never the proposer, never twice, only while the engine's live picture
of the nomination passes its checks. It never refuses: a script, not a judgment, and every
note it leaves says so. A `ProxyOperator` given signers leaves engagement cards to them (a
plain approve is refused for an engagement anyway). Situations (`sim/situations/`) run them
in autoplay, or as the cosigner of a person at the console (`mode = "cosign"`: never the first
signature); the evaluation's arms do not use them yet (`eval/engage.py`).
"""

from __future__ import annotations

import asyncio
import math
import random
import time
from dataclasses import asdict, dataclass, field
from typing import Any

from .bridge import Bridge, BridgeError
from .evidence import C_MIN, RULEBOOK_VERSION, evidence, near_aim

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
    trust_agent: float = 1.0
    c_min: float = C_MIN
    preset: str = "default"

    def as_dict(self) -> dict[str, Any]:
        return {**asdict(self), "rulebook": RULEBOOK_VERSION}


PRESETS: dict[str, dict[str, Any]] = {
    "default": {},
    "diligent": {"diligence": 1.0, "attend_s": 10.0, "fatigue_queue": 12.0},
    "rubber-stamp": {"diligence": 0.1, "attend_s": 10.0, "skim_s": 1.5},
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


def judge(intent: dict, tracks: list[dict], p: ProxyParams) -> Judgment:
    """The rule book applied to a card and the tracks the C2 shows (pure, for tests)."""
    params = intent.get("params") or {}
    center = params.get("center")
    # the intent's params as the API serves them (`radius_m`; `radius` in older logs)
    radius = params.get("radius_m", params.get("radius"))
    armed = armed_task(intent)
    reasons = evidence(armed, intent.get("checks", []), center, radius, tracks, p.c_min)
    looked = [t.get("id") for t in near_aim(center, radius, tracks)] if armed else []
    if reasons:
        return Judgment("reject", "; ".join(reasons), looked)
    return Judgment("approve", "nothing the C2 shows speaks against it", looked)


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

    def __init__(
        self,
        bridge: Bridge,
        params: ProxyParams,
        seed: int,
        log: list[dict[str, Any]] | None = None,
        operator: str | None = None,
        signers: "EngagementSigners | None" = None,
        note_prefix: str = "proxy",
    ):
        self.bridge = bridge
        self.p = params
        self.rng = random.Random(seed * 7919 + 17)
        self.decisions: list[dict[str, Any]] = log if log is not None else []
        self.handled: set[int] = set()
        if operator:
            self.operator = operator
        # engagement nominations go to these scripted signers (two distinct operators with
        # engagement authority, picture tokens); without them the proxy decides only
        # ordinary cards, as before
        self.signers = signers
        # how its notes begin in the audit log ("proxy" in the evaluation; a situation's
        # autoplay says "scripted")
        self.note_prefix = note_prefix

    def describe(self) -> dict[str, Any]:
        out = {"name": self.name, "operator": self.operator, "params": self.p.as_dict(), "proxy": True}
        if self.signers is not None:
            out["engagement_signers"] = self.signers.describe()
        return out

    def _lognormal(self, median: float, sigma: float) -> float:
        return median * math.exp(self.rng.gauss(0.0, sigma))

    async def _sleep(self, stop: asyncio.Event, seconds: float) -> bool:
        try:
            await asyncio.wait_for(stop.wait(), timeout=max(0.0, seconds))
            return True
        except asyncio.TimeoutError:
            return False

    async def run(self, stop: asyncio.Event) -> None:
        signing = asyncio.create_task(self.signers.run(stop)) if self.signers is not None else None
        try:
            await self._looks(stop)
        finally:
            if signing is not None:
                await asyncio.gather(signing, return_exceptions=True)

    async def _looks(self, stop: asyncio.Event) -> None:
        while not stop.is_set():
            if await self._sleep(stop, self._lognormal(self.p.attend_s, self.p.attend_sigma)):
                return
            inbox = await asyncio.to_thread(self.bridge.try_get, "/api/inbox")
            pending = sorted((inbox or {}).get("pending", []), key=lambda i: i.get("expires_t") or 0)
            if self.signers is not None:
                # engagement nominations are the signers' (picture tokens, two operators)
                pending = [i for i in pending if i.get("kind") != "engage"]
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
        # what the rule book finds on the card and the picture now (recorded either way, so
        # the report can say what reading would have done)
        try:
            tracks = await asyncio.to_thread(self.bridge.tracks)
        except (BridgeError, OSError, ValueError):
            tracks = []
        found = judge(intent, tracks, p)
        j = found if diligent else Judgment("approve", "rubber stamp: not read", [])
        shown["tracks"] = j.tracks
        open_ms = int((time.monotonic() - opened) * 1000)
        try:
            await asyncio.to_thread(self.bridge.view, iid, "decide", shown, self.operator, j.action, open_ms)
        except (BridgeError, OSError):
            pass
        note = f"{self.note_prefix} ({p.preset}, {'read' if diligent else 'skimmed'}, queue {queue}): {j.reason}"
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
                "if_read": found.action,
                "queue": queue,
                "p_read": round(p_read, 3),
                "open_ms": open_ms,
                "tracks": j.tracks,
                "result": result,
            }
        )


class EngagementSigners:
    """Scripted engagement signers (see the module docstring). `operators` are distinct ids
    with engagement authority, tried in order. `mode = "sign"` supplies every signature the
    tier needs; `mode = "cosign"` signs only after someone who is not one of them signed
    first, so a person at the console always makes the first signature. On a bridge that
    authenticates each operator must be logged in on `bridge` and its step-up password given
    in `passwords`; on a dev bridge the header names it."""

    def __init__(
        self,
        bridge: Bridge,
        operators: list[str],
        mode: str = "sign",
        label: str = "scripted",
        read_s: float = 3.0,
        period_s: float = 3.0,
        passwords: dict[str, str] | None = None,
        log: list[dict[str, Any]] | None = None,
    ):
        if mode not in ("sign", "cosign"):
            raise ValueError("mode must be sign or cosign")
        if not operators or len(set(operators)) != len(operators):
            raise ValueError("engagement signers are distinct operators")
        if mode == "sign" and len(operators) < 2:
            raise ValueError("signing an engagement alone takes two distinct operators")
        self.bridge = bridge
        self.operators = list(operators)
        self.mode = mode
        self.label = label
        self.read_s = read_s
        self.period_s = period_s
        self.passwords = dict(passwords or {})
        self.decisions: list[dict[str, Any]] = log if log is not None else []
        # intent -> what was said last, so a waiting nomination is recorded once per reason
        self._said: dict[int, str] = {}

    def describe(self) -> dict[str, Any]:
        return {"operators": self.operators, "mode": self.mode, "label": self.label, "read_s": self.read_s, "scripted": True}

    def _record(self, rec: dict[str, Any]) -> None:
        self.decisions.append({"t": time.time(), **rec})

    def _wait(self, iid: int, why: str) -> None:
        if self._said.get(iid) != why:
            self._said[iid] = why
            self._record({"intent_id": iid, "action": "wait", "reason": why})

    async def run(self, stop: asyncio.Event) -> None:
        while not stop.is_set():
            inbox = await asyncio.to_thread(self.bridge.try_get, "/api/inbox")
            for i in (inbox or {}).get("pending", []):
                if stop.is_set():
                    return
                if i.get("kind") == "engage":
                    try:
                        await self.consider(i)
                    except (BridgeError, OSError, ValueError) as e:
                        self._record({"intent_id": i.get("id"), "action": "error", "reason": str(e)})
            try:
                await asyncio.wait_for(stop.wait(), timeout=self.period_s)
            except asyncio.TimeoutError:
                pass

    def next_signer(self, view: dict[str, Any], source: dict[str, Any]) -> tuple[str | None, str]:
        """Who of these operators signs next on this nomination (the engine's `engage` view),
        or None and why not (pure, for tests)."""
        needed = int(view.get("approvers_needed") or 2)
        signed = list(view.get("signed") or [])
        if len(signed) >= needed:
            return None, "every signature it needs is given"
        if self.mode == "cosign" and not any(s not in self.operators for s in signed):
            return None, "waiting for a person to sign first (the scripted cosigner never makes the first signature)"
        proposer = view.get("proposer") or (source.get("id") if source.get("kind") == "human" else None)
        eligible = view.get("eligible") or view.get("engage_operators")
        for op in self.operators:
            if op in signed or op == proposer:
                continue
            if eligible and op not in eligible:
                continue
            return op, ""
        return None, "none of the scripted signers may sign it (signed already, the proposer, or no engagement authority)"

    async def consider(self, intent: dict[str, Any]) -> list[dict[str, Any]]:
        """Sign what can be signed on this nomination now; returns the signatures made."""
        iid = int(intent["id"])
        made: list[dict[str, Any]] = []
        cur = intent
        for _ in range(len(self.operators)):
            view = cur.get("engage")
            if view is None:
                full = await asyncio.to_thread(self.bridge.intent, iid)
                cur = full.get("intent") or full
                view = cur.get("engage") or {}
            if cur.get("state") != "pending_approval":
                return made
            op, why = self.next_signer(view, cur.get("source") or {})
            if op is None:
                self._wait(iid, why)
                return made
            live = view.get("live") or {}
            if not live.get("passed"):
                failing = [c.get("rule") for c in live.get("checks", []) if c.get("outcome") != "pass"]
                self._wait(iid, "the engine's live picture does not pass" + (f" ({', '.join(failing)})" if failing else ": none yet"))
                return made
            # look first: the engine shows this operator exactly what they would sign
            token = await asyncio.to_thread(self.bridge.post, f"/api/intents/{iid}/picture", {}, op)
            await asyncio.sleep(self.read_s)
            body: dict[str, Any] = {
                "token": token.get("id"),
                "note": f"{self.label} signer {op} (not a person): signed on the engine's picture token {token.get('id')}",
            }
            if op in self.passwords:
                body["password"] = self.passwords[op]
            try:
                out = await asyncio.to_thread(self.bridge.post, f"/api/intents/{iid}/approve", body, op)
            except BridgeError as e:
                detail = e.body.get("detail") if isinstance(e.body, dict) else e.body
                self._record({"intent_id": iid, "action": "sign", "by": op, "token": token.get("id"), "result": f"error {e.status}: {detail}"})
                return made
            cur = out.get("intent") or {}
            rec = {"intent_id": iid, "action": "sign", "by": op, "token": token.get("id"), "result": cur.get("state"), "dispatched": bool(out.get("dispatched"))}
            self._record(rec)
            made.append(rec)
            self._said.pop(iid, None)
            # the view after a signature (the plan it froze, who signed) comes with the intent;
            # fetch it whole when the answer does not carry it
            if "engage" not in cur:
                full = await asyncio.to_thread(self.bridge.intent, iid)
                cur = full.get("intent") or full
        return made


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
