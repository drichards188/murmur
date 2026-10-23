"""The review work on simulated time (docs/HEADLESS.md, section 16): a scripted operator who
works the identification review list and the review a nomination waits on, through the REST
paths a person's console uses, each act under the operator's own id and logged by the bridge as
the live stack logs it.

**A script with stated rules, not judgment, and never human data.** What it does with each kind
of item is its policy (`ReviewPolicy`, presets in `PRESETS`, a batch's `Options.review`):

| item | where it reads it | what it may do | REST path |
|---|---|---|---|
| a rule declaration awaiting an operator (rules 9) | `GET /api/review` `declarations` | confirm or revoke | `POST /api/tracks/{id}/confirm`, `.../revoke` |
| a review flag in force (rules 9) | `GET /api/review` `open` | confirm, dismiss, or declare the flagged track hostile | `POST /api/review/{flag}/resolve`, `POST /api/tracks/{id}/declare` |
| a lost contact a waiting nomination's keep-out counts (rules 16) | `GET /api/inbox`, a nomination's `engage.lost_open` | accept it for that engagement (engagement authority) | `POST /api/intents/{id}/accept_unknown` `{track, since}` |
| a live track carrying a lost contact's AIS (rules 16) | `GET /api/review` `lost_claims` | confirm it, only when nothing contradicts it | `POST /api/review/lost/{track}/confirm` `{by_track}` |
| a platform spent by its own effect but still heard (rules 15) | `GET /api/inbox` alerts of kind `spent` | un-spend it (engagement authority) | `POST /api/restores/unspend` `{uid, note}` |

Each item is acted on once (the engine refuses what is no longer open, and that answer is
recorded). The operator looks every `attend_s` (lognormal, `attend_sigma`; 0 for a fixed
interval) and reads each item for `read_s` before acting, as eval/proxy.py's
`confirm_rule_declarations` reads a declaration first. Draws from
`random.Random(f"review:{seed}")`.
"""

from __future__ import annotations

import math
import random
from dataclasses import asdict, dataclass, field
from typing import Any

from eval.situation import AUTOPLAY_OPERATOR, SCRIPTED_SIGNERS

DECLARATIONS = ("confirm", "revoke", "leave")
FLAGS = ("confirm", "dismiss", "declare", "leave")
LOST = ("accept", "leave")
CLAIMS = ("confirm", "leave")
UNSPEND = ("unspend", "leave")


@dataclass
class ReviewPolicy:
    # any operator confirms, revokes, resolves and confirms claims; accepting a lost contact
    # and un-spending a platform take engagement authority
    operator: str = AUTOPLAY_OPERATOR
    engage_operator: str = SCRIPTED_SIGNERS[0]
    attend_s: float = 5.0
    attend_sigma: float = 0.0
    read_s: float = 3.0
    declarations: str = "confirm"
    flags: str = "leave"
    lost_contacts: str = "leave"
    ais_claims: str = "leave"
    unspend: str = "leave"
    label: str = "scripted"
    preset: str = "confirm-declarations"

    def as_dict(self) -> dict[str, Any]:
        return {**asdict(self), "scripted": True}


PRESETS: dict[str, dict[str, Any]] = {
    # what a situation's autoplay does with `confirm_rule_declarations` (eval/proxy.py): confirm
    # every rule declaration, leave the rest
    "confirm-declarations": {},
    # yes to everything the review list asks: every declaration and flag confirmed, every lost
    # contact a nomination waits on accepted, every uncontradicted AIS claim confirmed
    "accept-all": {"flags": "confirm", "lost_contacts": "accept", "ais_claims": "confirm"},
    # no to the engine's declarations and flags: revokes every rule declaration, dismisses
    # every flag, accepts nothing
    "skeptic": {"declarations": "revoke", "flags": "dismiss"},
    # nobody reviews
    "absent": {"declarations": "leave"},
}


def review_policy(spec: dict[str, Any] | None = None) -> ReviewPolicy:
    """From a batch's `review` option: `{"preset": ..., ...overrides}`."""
    spec = dict(spec or {})
    name = str(spec.pop("preset", "confirm-declarations"))
    if name not in PRESETS:
        raise ValueError(f"unknown review preset {name!r}; presets: {', '.join(PRESETS)}")
    p = ReviewPolicy(preset=name)
    for k, v in {**PRESETS[name], **spec}.items():
        if not hasattr(p, k) or k == "preset":
            raise ValueError(f"unknown review parameter {k!r}")
        setattr(p, k, float(v) if isinstance(getattr(p, k), float) else v)
    for k, allowed in (("declarations", DECLARATIONS), ("flags", FLAGS), ("lost_contacts", LOST),
                       ("ais_claims", CLAIMS), ("unspend", UNSPEND)):
        if getattr(p, k) not in allowed:
            raise ValueError(f"review {k}: {getattr(p, k)!r} is not one of {', '.join(allowed)}")
    if p.attend_s <= 0 or p.read_s < 0:
        raise ValueError("attend_s is positive and read_s not negative")
    return p


# ---- what the policy does with each item (pure, for tests) -------------------------------------

def declaration_act(p: ReviewPolicy, item: dict[str, Any]) -> dict[str, Any] | None:
    """A rule declaration awaiting an operator (`GET /api/review` `declarations`)."""
    if p.declarations == "leave" or p.declarations not in (item.get("actions") or []) or not item.get("track"):
        return None
    track = str(item["track"])
    why = f"{p.label} reviewer (not a person): {p.declarations}s every rule declaration ({item.get('basis') or item.get('declared_by') or ''})"[:400]
    body = {"note": why} if p.declarations == "revoke" else {}
    return {"key": f"decl:{item.get('id')}", "kind": "declaration", "action": p.declarations, "item": item.get("id"),
            "track": track, "by": p.operator, "path": f"/api/tracks/{track}/{p.declarations}", "body": body}


def flag_act(p: ReviewPolicy, item: dict[str, Any]) -> dict[str, Any] | None:
    """A review flag in force (`GET /api/review` `open`)."""
    if p.flags == "leave" or p.flags not in (item.get("actions") or []):
        return None
    flag = item.get("flag")
    note = f"{p.label} reviewer (not a person): {p.flags} every review flag ({item.get('flag_kind')})"
    if p.flags == "declare":
        tracks = item.get("tracks") or []
        if len(tracks) != 1:
            return None
        return {"key": f"flag:{flag}", "kind": "flag", "action": "declare", "item": item.get("id"), "track": tracks[0],
                "by": p.operator, "path": f"/api/tracks/{tracks[0]}/declare",
                "body": {"affiliation": "hostile", "flag": flag, "note": note}}
    resolution = "confirmed" if p.flags == "confirm" else "dismissed"
    return {"key": f"flag:{flag}", "kind": "flag", "action": p.flags, "item": item.get("id"), "by": p.operator,
            "path": f"/api/review/{flag}/resolve", "body": {"resolution": resolution, "note": note}}


def claim_act(p: ReviewPolicy, claim: dict[str, Any]) -> dict[str, Any] | None:
    """A live track carrying a lost contact's AIS (`GET /api/review` `lost_claims`): confirmed
    only when the engine says nothing contradicts it (`actions` holds confirm: no blockers)."""
    if p.ais_claims != "confirm" or "confirm" not in (claim.get("actions") or []):
        return None
    return {"key": f"claim:{claim.get('id')}", "kind": "ais_claim", "action": "confirm", "item": claim.get("id"),
            "track": claim.get("lost"), "by": p.operator, "path": f"/api/review/lost/{claim.get('lost')}/confirm",
            "body": {"by_track": claim.get("track")}}


def lost_acts(p: ReviewPolicy, card: dict[str, Any]) -> list[dict[str, Any]]:
    """The lost contacts a waiting nomination's keep-out counts that no one has accepted for it
    (`engage.lost_open`, shown while it waits with no signature), each accepted for it."""
    if p.lost_contacts != "accept" or card.get("kind") != "engage" or card.get("state") != "pending_approval":
        return []
    iid = int(card["id"])
    view = card.get("engage") or {}
    # a reset of the signatures clears the acceptances with the plan: given again after one
    reset_t = (view.get("reset") or {}).get("t")
    out = []
    for lost in view.get("lost_open") or []:
        out.append({"key": f"lost:{iid}:{lost.get('track')}:{lost.get('since')}:{reset_t}", "kind": "lost_contact",
                    "action": "accept", "item": iid, "track": lost.get("track"), "by": p.engage_operator,
                    "path": f"/api/intents/{iid}/accept_unknown",
                    "body": {"track": lost.get("track"), "since": lost.get("since")}})
    return out


def unspend_acts(p: ReviewPolicy, alerts: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Platforms spent by their own effect but still heard (inbox alerts of kind `spent`)."""
    if p.unspend != "unspend":
        return []
    out = []
    for a in alerts:
        if a.get("kind") != "spent" or a.get("uid") is None:
            continue
        uid = int(a["uid"])
        out.append({"key": f"unspend:{uid}:{a.get('id')}", "kind": "unspend", "action": "unspend", "item": a.get("id"),
                    "uid": uid, "by": p.engage_operator, "path": "/api/restores/unspend",
                    "body": {"uid": uid, "note": f"{p.label} reviewer (not a person): un-spends every platform spent but still heard"}})
    return out


@dataclass
class Reviewer:
    """The reviewer as a process of the episode (`wake(now)` acts at `now` and says when next)."""

    sim: Any
    policy: ReviewPolicy
    seed: int
    decisions: list[dict[str, Any]] = field(default_factory=list)

    def __post_init__(self) -> None:
        self.rng = random.Random(f"review:{int(self.seed)}")
        self.done: set[str] = set()
        self._todo: list[dict[str, Any]] = []
        self._reading: dict[str, Any] | None = None

    def describe(self) -> dict[str, Any]:
        return self.policy.as_dict()

    def _idle(self) -> bool:
        p = self.policy
        return all(getattr(p, k) == "leave" for k in ("declarations", "flags", "lost_contacts", "ais_claims", "unspend"))

    def start(self, now: float) -> float | None:
        return None if self._idle() else now

    def _interval(self) -> float:
        p = self.policy
        return p.attend_s * math.exp(self.rng.gauss(0.0, p.attend_sigma)) if p.attend_sigma > 0 else p.attend_s

    def items(self) -> list[dict[str, Any]]:
        """Everything the policy would act on now, not yet done, in a fixed order: declarations,
        flags, AIS claims (the review list), then lost contacts and spent platforms (the inbox)."""
        acts: list[dict[str, Any] | None] = []
        status, review = self.sim.api("GET", "/api/review")
        if status == 200 and review:
            acts += [declaration_act(self.policy, d) for d in review.get("declarations") or []]
            acts += [flag_act(self.policy, f) for f in review.get("open") or []]
            acts += [claim_act(self.policy, c) for c in review.get("lost_claims") or []]
        if self.policy.lost_contacts != "leave" or self.policy.unspend != "leave":
            status, inbox = self.sim.api("GET", "/api/inbox")
            if status == 200 and inbox:
                for card in sorted(inbox.get("pending") or [], key=lambda c: int(c.get("id", 0))):
                    acts += lost_acts(self.policy, card)
                acts += unspend_acts(self.policy, inbox.get("alerts") or [])
        seen: set[str] = set()
        out = []
        for a in acts:
            if a is None or a["key"] in self.done or a["key"] in seen:
                continue
            seen.add(a["key"])
            out.append(a)
        return out

    def wake(self, now: float) -> float | None:
        if self._reading is not None:
            act, self._reading = self._reading, None
            self._do(now, act)
        if not self._todo:
            self._todo = self.items()
            if not self._todo:
                return now + self._interval()
        act = self._todo.pop(0)
        self.done.add(act["key"])
        self._reading = {**act, "opened_t": now}
        return now + self.policy.read_s if self.policy.read_s > 0 else now

    def _do(self, now: float, act: dict[str, Any]) -> None:
        status, out = self.sim.api("POST", act["path"], act["body"], act["by"])
        if status // 100 == 2:
            result = "done"
        else:
            result = f"error {status}: {(out or {}).get('detail') if isinstance(out, dict) else out}"[:400]
        rec = {k: act[k] for k in ("kind", "action", "item", "by") if k in act}
        for k in ("track", "uid"):
            if k in act:
                rec[k] = act[k]
        self.decisions.append({"t": round(now, 3), "read_from_t": round(act["opened_t"], 3), **rec, "result": result})
