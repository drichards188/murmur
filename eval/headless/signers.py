"""The engagement signers on simulated time (docs/HEADLESS.md, section 16): two or more scripted
operators with engagement authority who decide engagement nominations, as eval/proxy.py's
`EngagementSigners` does on a live stack.

**Scripts, not judgment, and never human data.** Every note they leave says "scripted" and
"not a person". They reach the bridge only through the operator API a person's console uses:
`GET /api/inbox` (the nomination's `engage` view: who signed, who may, the frozen plan, the
engine's live picture), `POST /api/intents/{id}/picture` (a picture token, the look), then
`POST /api/intents/{id}/approve` with `{"token"}` (the signature on that picture) or
`POST /api/intents/{id}/reject` (a decline). The engine decides everything else: two distinct
signers with engagement authority, never the proposer, one frozen plan, its reset when the plan
stops holding (and then they sign again, as the live signers do).

**Who signs next** is `EngagementSigners.next_signer`, the live class's own function (pure);
the sequence is the live loop's: a look at the inbox every `period_s`, then for each nomination
the next signer looks (a token), reads for `read_s`, reads the nomination again and signs only
if the same signer would still sign and the live picture still passes.

**Policies** (`SignerPolicy`, presets in `PRESETS`, a batch's `Options.signers`):

- `mode = "sign"` (the `scripted` preset): EngagementSigners exactly: every nomination whose
  live picture passes is signed. With `decline_if`, a reading signer declines a nomination the
  card shows something against (`accepted_lost`: it rests on a lost contact someone accepted;
  `accepted_unknown`: on one of our vehicles accepted as lost; `changed`: a check is worse
  than at nomination), and says which.
- `mode = "decline"`: the first eligible signer looks, reads and declines every nomination.
- `mode = "never"`: nobody answers; every nomination waits until it expires.
- `mode = "rates"`: each nomination's fate is drawn once, when first seen: signed (as `sign`)
  with probability `p_sign`, declined with `p_decline`, else left to expire; the first look
  comes after a lognormal latency (`latency_median_s`, `latency_sigma`).
- `abort_after_s` (any mode; the `abort` preset): the first signer aborts every engagement still
  dispatched (its effect not released) that long after its dispatch
  (`POST /api/intents/{id}/abort`): any operator may, and the engine then commands the
  platform's fallback and owes its own lost-link values back (rules 6), the path a released
  effect never takes.

Draws come from `random.Random(f"signers:{seed}")`, in the order nominations are first seen,
so an episode stays a function of its situation and seed.
"""

from __future__ import annotations

import math
import random
from dataclasses import asdict, dataclass, field
from typing import Any

from eval.proxy import EngagementSigners
from eval.situation import SCRIPTED_SIGNERS

MODES = ("sign", "decline", "never", "rates")
#: what a reading signer may decline on (`decline_if`)
DECLINE_ON = ("accepted_lost", "accepted_unknown", "changed")


@dataclass
class SignerPolicy:
    mode: str = "sign"
    operators: tuple[str, ...] = tuple(SCRIPTED_SIGNERS)
    # EngagementSigners' defaults: a look at the inbox every 3 s, 3 s reading a picture
    read_s: float = 3.0
    period_s: float = 3.0
    decline_if: tuple[str, ...] = ()
    # rates
    p_sign: float = 1.0
    p_decline: float = 0.0
    latency_median_s: float = 10.0
    latency_sigma: float = 0.4
    abort_after_s: float | None = None
    label: str = "scripted"
    preset: str = "scripted"

    def as_dict(self) -> dict[str, Any]:
        out = asdict(self)
        out["operators"] = list(self.operators)
        out["decline_if"] = list(self.decline_if)
        out["scripted"] = True
        return out


PRESETS: dict[str, dict[str, Any]] = {
    # eval/proxy.py EngagementSigners as the situations run them
    "scripted": {},
    # slow signers: each reads a picture for 20 s and looks every 10 s, so the plan must hold
    # longer between the two signatures
    "deliberate": {"read_s": 20.0, "period_s": 10.0},
    # signs only a nomination that rests on nothing accepted as lost and whose checks are no
    # worse than at nomination
    "cautious": {"decline_if": ("accepted_lost", "accepted_unknown", "changed")},
    "decline": {"mode": "decline"},
    "absent": {"mode": "never"},
    # sign, then abort 20 s after dispatch (the fallback, and the restore of the platform's own
    # lost-link values)
    "abort": {"abort_after_s": 20.0},
}


def signer_policy(spec: dict[str, Any] | None = None) -> SignerPolicy:
    """From a batch's `signers` option: `{"preset": ..., ...overrides}`."""
    spec = dict(spec or {})
    name = str(spec.pop("preset", "scripted"))
    if name not in PRESETS:
        raise ValueError(f"unknown signer preset {name!r}; presets: {', '.join(PRESETS)}")
    values = {**PRESETS[name], **spec}
    p = SignerPolicy(preset=name)
    for k, v in values.items():
        if not hasattr(p, k) or k == "preset":
            raise ValueError(f"unknown signer parameter {k!r}")
        cur = getattr(p, k)
        if k == "abort_after_s":
            setattr(p, k, None if v is None else float(v))
        elif isinstance(cur, tuple):
            setattr(p, k, tuple(str(x) for x in v))
        elif isinstance(cur, float):
            setattr(p, k, float(v))
        else:
            setattr(p, k, v)
    if p.mode not in MODES:
        raise ValueError(f"unknown signer mode {p.mode!r}; modes: {', '.join(MODES)}")
    if len(set(p.operators)) != len(p.operators) or not p.operators:
        raise ValueError("engagement signers are distinct operators")
    if p.mode in ("sign", "rates") and len(p.operators) < 2:
        raise ValueError("signing an engagement takes two distinct operators")
    bad = [d for d in p.decline_if if d not in DECLINE_ON]
    if bad:
        raise ValueError(f"unknown decline_if {bad}; known: {', '.join(DECLINE_ON)}")
    if p.p_sign < 0 or p.p_decline < 0 or p.p_sign + p.p_decline > 1.0 + 1e-9:
        raise ValueError("rates: p_sign and p_decline are probabilities that sum to at most 1")
    if p.read_s <= 0 or p.period_s <= 0:
        raise ValueError("read_s and period_s are positive")
    return p


def objections(policy: SignerPolicy, card: dict[str, Any]) -> list[str]:
    """What a reading signer under `policy` finds against the nomination as the inbox serves it
    (`decline_if`; pure, for tests). Empty: nothing."""
    view = card.get("engage") or {}
    found = []
    if "accepted_lost" in policy.decline_if and view.get("accepted_lost"):
        found.append("it rests on lost contacts accepted for it: "
                     + ", ".join(str(a.get("track")) for a in view["accepted_lost"]))
    if "accepted_unknown" in policy.decline_if and view.get("accepted"):
        found.append("it rests on vehicles of ours accepted as lost: "
                     + ", ".join(str(a.get("uid")) for a in view["accepted"]))
    if "changed" in policy.decline_if:
        worse = [c.get("rule") for c in ((view.get("live") or {}).get("changed") or []) if c.get("worse")]
        if worse:
            found.append("checks worse than at nomination: " + ", ".join(str(r) for r in worse))
    return found


@dataclass
class _Look:
    intent: int
    op: str
    token: int | None
    action: str  # "sign" or "decline"
    t: float
    why: str = ""


@dataclass
class Signers:
    """The signers as a process of the episode (`wake(now)` acts at `now` and says when next)."""

    sim: Any
    policy: SignerPolicy
    seed: int
    # the bridge's clock at the situation's zero, unix s (the intents' times are on it)
    epoch: float = 0.0
    decisions: list[dict[str, Any]] = field(default_factory=list)

    def __post_init__(self) -> None:
        self.rng = random.Random(f"signers:{int(self.seed)}")
        # the live class's own `next_signer` (pure; it never touches its bridge)
        self._es = EngagementSigners(None, list(self.policy.operators),  # type: ignore[arg-type]
                                     mode="sign", label=self.policy.label,
                                     read_s=self.policy.read_s, period_s=self.policy.period_s) \
            if len(self.policy.operators) >= 2 else None
        self._said: dict[int, str] = {}
        # rates: intent -> (fate, first look not before)
        self._fate: dict[int, tuple[str, float]] = {}
        self._round: list[int] = []
        self._look: _Look | None = None
        self._aborted: set[int] = set()

    def describe(self) -> dict[str, Any]:
        return self.policy.as_dict()

    def start(self, now: float) -> float | None:
        return None if self.policy.mode == "never" and self.policy.abort_after_s is None else now

    # ---- reads and writes, as the console makes them ----------------------------------------

    def _inbox(self) -> list[dict[str, Any]]:
        status, body = self.sim.api("GET", "/api/inbox")
        return list((body or {}).get("pending", [])) if status == 200 else []

    def _card(self, iid: int) -> dict[str, Any] | None:
        return next((i for i in self._inbox() if int(i.get("id", -1)) == iid), None)

    def _record(self, now: float, rec: dict[str, Any]) -> None:
        self.decisions.append({"t": round(now, 3), **rec})

    def _wait(self, now: float, iid: int, why: str) -> None:
        if self._said.get(iid) != why:
            self._said[iid] = why
            self._record(now, {"intent_id": iid, "action": "wait", "reason": why})

    # ---- who acts -----------------------------------------------------------------------------

    def _next(self, card: dict[str, Any] | None) -> tuple[str | None, str]:
        """Who signs next on `card` (the inbox's item) and why not, the live picture included:
        EngagementSigners._blocked's rule."""
        if card is None or card.get("state") != "pending_approval":
            return None, "no longer waiting"
        view = card.get("engage") or {}
        if self._es is None:
            return None, "no two signers"
        op, why = self._es.next_signer(view, card.get("source") or {})
        if op is None:
            return None, why
        live = view.get("live") or {}
        if not live.get("passed"):
            failing = [c.get("rule") for c in live.get("checks", []) if c.get("outcome") != "pass"]
            return None, "the engine's live picture does not pass" + (f" ({', '.join(failing)})" if failing else ": none yet")
        return op, ""

    def _decliner(self, card: dict[str, Any]) -> str | None:
        """The first of these operators who may decide this nomination (not its proposer)."""
        view = card.get("engage") or {}
        proposer = view.get("proposer")
        eligible = view.get("eligible") or view.get("engage_operators") or list(self.policy.operators)
        return next((op for op in self.policy.operators if op != proposer and op in eligible), None)

    def _fate_of(self, iid: int, now: float) -> tuple[str, float]:
        if iid not in self._fate:
            p = self.policy
            u = self.rng.random()
            fate = "sign" if u < p.p_sign else ("decline" if u < p.p_sign + p.p_decline else "ignore")
            lat = p.latency_median_s * math.exp(self.rng.gauss(0.0, p.latency_sigma))
            self._fate[iid] = (fate, now + lat)
        return self._fate[iid]

    # ---- the process --------------------------------------------------------------------------

    def wake(self, now: float) -> float | None:
        if self.policy.abort_after_s is not None and self._look is None:
            self._aborts(now)
        if self.policy.mode == "never":
            return None if self.policy.abort_after_s is None else now + self.policy.period_s
        if self._look is not None:
            look, self._look = self._look, None
            self._finish(now, look)
            nxt = self._consider(now, look.intent)
            if nxt is not None:
                return nxt
        else:
            self._round = sorted(int(i["id"]) for i in self._inbox() if i.get("kind") == "engage")
        while self._round:
            nxt = self._consider(now, self._round.pop(0))
            if nxt is not None:
                return nxt
        return now + self.policy.period_s

    def _aborts(self, now: float) -> None:
        """`abort_after_s`: abort every engagement dispatched that long ago and not released."""
        status, body = self.sim.api("GET", "/api/intents")
        if status != 200:
            return
        for i in (body or {}).get("intents", []):
            if i.get("kind") != "engage" or i.get("state") != "dispatched" or int(i["id"]) in self._aborted:
                continue
            disp = [h for h in i.get("history") or [] if h.get("state") == "dispatched"]
            if not disp:
                continue
            since = now - (float(disp[0]["t"]) / 1000.0 - self.epoch)
            if since + 1e-9 < float(self.policy.abort_after_s or 0.0):
                continue
            iid = int(i["id"])
            self._aborted.add(iid)
            op = self.policy.operators[0]
            note = (f"{self.policy.label} signer {op} (not a person) aborts: the policy aborts every engagement "
                    f"{float(self.policy.abort_after_s or 0.0):g} s after its dispatch")
            st, out = self.sim.api("POST", f"/api/intents/{iid}/abort", {"note": note}, op)
            self._record(now, {"intent_id": iid, "action": "abort", "by": op, "dispatched_s": round(since, 3),
                               "result": ((out or {}).get("intent") or {}).get("state") if st // 100 == 2
                               else f"error {st}: {(out or {}).get('detail')}"})

    def _consider(self, now: float, iid: int) -> float | None:
        """Start the next look on nomination `iid` if one is due (returns when it ends)."""
        card = self._card(iid)
        if card is None or card.get("state") != "pending_approval":
            return None
        mode = self.policy.mode
        action = "sign"
        if mode == "rates":
            fate, not_before = self._fate_of(iid, now)
            if fate == "ignore":
                self._wait(now, iid, "rates: drawn to be left waiting (it expires)")
                return None
            if now < not_before:
                return None
            action = fate
        elif mode == "decline":
            action = "decline"
        if action == "decline":
            op = self._decliner(card)
            if op is None:
                self._wait(now, iid, "none of the scripted signers may decide it")
                return None
            why = "declines every nomination (policy decline)" if mode == "decline" else "rates: drawn to decline"
        else:
            op, why = self._next(card)
            if op is None:
                if why != "no longer waiting":
                    self._wait(now, iid, why)
                return None
        status, token = self.sim.api("POST", f"/api/intents/{iid}/picture", {}, op)
        if status // 100 != 2:
            self._record(now, {"intent_id": iid, "action": "look_failed", "by": op,
                               "result": f"error {status}: {(token or {}).get('detail')}"})
            return None
        self._look = _Look(iid, op, (token or {}).get("id"), action, now, why)
        return now + self.policy.read_s

    def _finish(self, now: float, look: _Look) -> None:
        """The end of a look: read the nomination again, then sign or decline (or say why not)."""
        card = self._card(look.intent)
        iid, op = look.intent, look.op
        if look.action == "sign":
            again, why = self._next(card)
            if again != op:
                self._wait(now, iid, f"changed while {op} looked: {why or f'{again} would sign now'}")
                return
            assert card is not None
            found = objections(self.policy, card)
            if found:
                look = _Look(iid, op, look.token, "decline", look.t, "; ".join(found))
            else:
                body = {"token": look.token,
                        "note": f"{self.policy.label} signer {op} (not a person): signed on the engine's picture token {look.token}"}
                status, out = self.sim.api("POST", f"/api/intents/{iid}/approve", body, op)
                if status // 100 != 2:
                    self._record(now, {"intent_id": iid, "action": "sign_failed", "by": op, "token": look.token,
                                       "result": f"error {status}: {(out or {}).get('detail')}"})
                    return
                self._said.pop(iid, None)
                self._record(now, {"intent_id": iid, "action": "sign", "by": op, "token": look.token,
                                   "looked_t": round(look.t, 3),
                                   "result": ((out or {}).get("intent") or {}).get("state"),
                                   "dispatched": bool((out or {}).get("dispatched"))})
                return
        if card is None or card.get("state") != "pending_approval":
            return
        note = f"{self.policy.label} signer {op} (not a person) declines: {look.why}"
        status, out = self.sim.api("POST", f"/api/intents/{iid}/reject", {"note": note}, op)
        self._record(now, {"intent_id": iid, "action": "decline", "by": op, "reason": look.why,
                           "result": ((out or {}).get("intent") or {}).get("state") if status // 100 == 2
                           else f"error {status}: {(out or {}).get('detail')}"})
