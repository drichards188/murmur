"""The ledger: what the engine proposed and how each card fared, as JSON per run.

A card's key is (play, target, uid). Its state is the bridge's (`get_intents` with `mine`,
every tick): proposed, pending_approval, approved, queued, dispatched, then rejected, expired,
voided, withdrawn, done or failed. When one of ours ends without running, the ledger sets a
cooldown, and a play may propose inside it only on a material change (the play defines it;
the rationale names it):

- **rejected**: 10 minutes for (play, target), whatever vehicle. The operator's only reason
  channel is an optional note (there are no structured reject reasons yet), so "not this
  play on this target" is the reading that never sends the operator the same card again with
  another vehicle on it.
- **expired** unread: 2 minutes for (play, target), and the play's score for that pair halves
  (the operator had no time: do not resend into the same queue).
- **voided** by the system: 30 seconds for the key (another vehicle may be tried at once).
- **withdrawn** by the engine: 30 seconds for the key (a premise that comes back at once is
  not proposed again in the same breath).

It also keeps what the reads alone do not say: a failing preview's back-off per key (10 s,
doubling to 160 s), the inbox budget's token bucket, the tier a preview gave for a key (so a
card the budget cannot take is not previewed again every tick), each vehicle's last recentre
and last suspect navigation, the parameters of others' orders a gap filler took over
(`explain_intent`), the engagements the change feed reported, the change feed's cursor, each
play's last status, and in shadow mode the would-be cards.

A card proposed by an earlier process of this run (a restart) is adopted from its rationale's
evidence record: the record names its play, target and vehicle.
"""

from __future__ import annotations

import json
import os
import tempfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from . import rationale as R

NOT_RUNNING = ("failed", "superseded", "requeued")  # beliefs.NOT_RUNNING (no import cycle)

OPEN = ("proposed", "checked", "auto_approved", "pending_approval", "approved", "queued", "dispatched")
WAITING = ("pending_approval", "approved", "queued")  # withdrawable (approved: until it goes out)
ENDED_UNRUN = ("rejected", "expired", "voided", "withdrawn")
ENGAGE_KEEP = 200  # engagements remembered from the change feed (the newest)


def key_of(play: str, target: str, uid: int | None) -> str:
    return f"{play}|{target}|{'' if uid is None else uid}"


def pair_of(play: str, target: str) -> str:
    return f"{play}|{target}"


@dataclass
class Decision:
    """How one of our cards ended or moved, for the situation log."""

    intent: int
    play: str
    target: str
    uid: int | None
    state: str
    by: str | None
    note: str | None
    cooldown_until: float | None


@dataclass
class Ledger:
    entries: dict[str, dict[str, Any]] = field(default_factory=dict)  # intent id -> entry
    cooldowns: list[dict[str, Any]] = field(default_factory=list)
    backoff: dict[str, dict[str, Any]] = field(default_factory=dict)
    score_factor: dict[str, float] = field(default_factory=dict)  # "play|target" -> factor
    recentred: dict[str, float] = field(default_factory=dict)  # uid -> t
    tier_seen: dict[str, dict[str, Any]] = field(default_factory=dict)  # key -> {t, would_reach}
    params: dict[str, dict[str, Any]] = field(default_factory=dict)  # intent id -> {task, params, uids}
    engaging: dict[str, dict[str, Any]] = field(default_factory=dict)  # intent id -> {uids, state}
    nav_bad_t: dict[str, float] = field(default_factory=dict)  # uid -> last tick it was suspect
    status: dict[str, str] = field(default_factory=dict)  # play -> last status line
    bucket: dict[str, float] = field(default_factory=dict)  # {tokens, t}
    cursor: str | None = None
    shadow: dict[str, dict[str, Any]] = field(default_factory=dict)  # key -> would-be card
    notes: dict[str, dict[str, Any]] = field(default_factory=dict)  # intent id -> operator note

    # ---- persistence ---------------------------------------------------------------------------

    def to_json(self) -> dict[str, Any]:
        return {
            "schema": 1, "entries": self.entries, "cooldowns": self.cooldowns, "backoff": self.backoff,
            "score_factor": self.score_factor, "recentred": self.recentred, "tier_seen": self.tier_seen,
            "params": self.params, "engaging": self.engaging, "nav_bad_t": self.nav_bad_t, "status": self.status,
            "bucket": self.bucket, "cursor": self.cursor, "shadow": self.shadow, "notes": self.notes,
        }

    @classmethod
    def from_json(cls, d: dict[str, Any]) -> "Ledger":
        led = cls()
        for k in ("entries", "backoff", "score_factor", "recentred", "tier_seen", "params", "engaging",
                  "nav_bad_t", "status", "bucket", "shadow", "notes"):
            setattr(led, k, dict(d.get(k) or {}))
        led.cooldowns = list(d.get("cooldowns") or [])
        led.cursor = d.get("cursor")
        return led

    def save(self, path: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        fd, tmp = tempfile.mkstemp(dir=str(path.parent), prefix=".ledger-", suffix=".json")
        try:
            with os.fdopen(fd, "w") as f:
                json.dump(self.to_json(), f, sort_keys=True, indent=1)
            os.replace(tmp, path)
        except BaseException:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise

    @classmethod
    def load(cls, path: Path) -> "Ledger":
        try:
            return cls.from_json(json.loads(path.read_text()))
        except (OSError, ValueError):
            return cls()

    # ---- own cards --------------------------------------------------------------------------------

    def record(self, intent: int, *, play: str, version: str, target: str, uid: int | None, task: str,
               request: dict[str, Any], t: float, state: str, basis: dict[str, Any], withdraw_if: list[str],
               prior_task: dict[str, Any] | None = None, tier: str | None = None, armed: bool = False) -> None:
        self.entries[str(intent)] = {
            "intent": intent, "play": play, "version": version, "target": target, "uid": uid, "task": task,
            "request": request, "proposed_t": round(t, 3), "state": state, "state_t": round(t, 3),
            "basis": basis, "withdraw_if": list(withdraw_if), "prior_task": prior_task, "tier": tier, "armed": armed,
        }

    def entry(self, intent: int) -> dict[str, Any] | None:
        return self.entries.get(str(intent))

    def open_entries(self) -> list[dict[str, Any]]:
        return [e for _, e in sorted(self.entries.items(), key=lambda kv: int(kv[0])) if e.get("state") in OPEN]

    def adopt(self, intent: dict[str, Any], t: float) -> dict[str, Any] | None:
        """An intent of ours this ledger does not hold (a restart): its key from the evidence
        record in its rationale."""
        rec = R.parse_record(str(intent.get("rationale") or ""))
        if not rec or not isinstance(rec.get("play"), str):
            return None
        play, _, version = rec["play"].partition("@")
        uids = [int(u) for u in intent.get("uids") or []]
        self.record(int(intent["id"]), play=play, version=version, target=str(rec.get("target")),
                    uid=rec.get("uid") if rec.get("uid") is not None else (uids[0] if uids else None),
                    task=str(intent.get("kind")), request=dict(rec.get("request") or {}), t=t,
                    state=str(intent.get("state")), basis=dict(rec.get("basis") or {}),
                    withdraw_if=list(rec.get("withdraw_if") or []))
        e = self.entries[str(intent["id"])]
        e["adopted"] = True
        return e

    def note_changes(self, changes: list[dict[str, Any]]) -> None:
        """What only the change feed says: an operator's note on a rejection of ours, and the
        engagements in play (any proposer's)."""
        for c in changes:
            if not isinstance(c, dict):
                continue
            kind = c.get("kind")
            if kind in ("intent.new", "intent.state") and c.get("intent") is not None:
                iid = str(c["intent"])
                note = c.get("operator_note")
                if c.get("mine") and isinstance(note, dict) and note.get("note"):
                    self.notes[iid] = {"by": note.get("by"), "note": str(note.get("note"))[:1000]}
                if c.get("task") == "engage":
                    self.engaging[iid] = {"uids": [int(u) for u in c.get("uids") or [] if str(u).isdigit()],
                                          "state": c.get("to") or c.get("state")}
        if len(self.engaging) > ENGAGE_KEEP:
            for k in sorted(self.engaging, key=int)[: len(self.engaging) - ENGAGE_KEEP]:
                del self.engaging[k]

    def sync(self, intents: dict[int, dict[str, Any]], t: float, cfg: Any) -> list[Decision]:
        """Take each own card's state from the bridge; on an end without a run, set its cooldown.
        Returns what changed (for the situation log), in intent order."""
        out: list[Decision] = []
        for iid in sorted(intents):
            i = intents[iid]
            e = self.entries.get(str(iid)) or self.adopt(i, t)
            if e is None:
                continue
            state = str(i.get("state"))
            if state == e.get("state"):
                continue
            e["state"], e["state_t"] = state, round(t, 3)
            ap = i.get("approval") or {}
            by = ap.get("by") if isinstance(ap, dict) else None
            note = (self.notes.get(str(iid)) or {}).get("note")
            if note is None and isinstance(ap, dict) and ap.get("note"):
                note = str(ap.get("note"))
            if state == "rejected" and not by:
                hist = i.get("history") or []
                by = _by_from(hist[-1].get("reason") if hist else None)
            until = self._cool(e, state, t, cfg, by, note)
            e["decided_by"], e["note"] = by, note
            out.append(Decision(intent=iid, play=e["play"], target=e["target"], uid=e.get("uid"), state=state,
                                by=by, note=note, cooldown_until=until))
        return out

    def _cool(self, e: dict[str, Any], state: str, t: float, cfg: Any, by: str | None, note: str | None) -> float | None:
        if state not in ENDED_UNRUN:
            return None
        cd = cfg.cooldown
        play, target, uid = e["play"], e["target"], e.get("uid")
        if state == "rejected":
            scope, until = pair_of(play, target), t + cd["rejected_s"]
        elif state == "expired":
            scope, until = pair_of(play, target), t + cd["expired_s"]
            pair = pair_of(play, target)
            self.score_factor[pair] = round(self.score_factor.get(pair, 1.0) * cd["expired_score_factor"], 6)
        elif state == "voided":
            scope, until = key_of(play, target, uid), t + cd["voided_s"]
        else:
            scope, until = key_of(play, target, uid), t + cd["withdrawn_s"]
        self.cooldowns.append({"scope": scope, "until_t": round(until, 3), "why": state, "set_t": round(t, 3),
                               "intent": e["intent"], "by": by, "note": note, "basis": e.get("basis") or {}})
        return until

    def cooldown_for(self, play: str, target: str, uid: int | None, t: float) -> dict[str, Any] | None:
        """The cooldown in force on this key (its pair's or its own), the one ending last."""
        scopes = {pair_of(play, target), key_of(play, target, uid)}
        live = [c for c in self.cooldowns if c["scope"] in scopes and c["until_t"] > t]
        return max(live, key=lambda c: (c["until_t"], c["intent"])) if live else None

    def prune(self, t: float) -> None:
        self.cooldowns = [c for c in self.cooldowns if c["until_t"] > t - 3600]
        for k in [k for k, b in self.backoff.items() if b["until_t"] < t - 3600]:
            del self.backoff[k]

    # ---- back-offs, tiers, the budget --------------------------------------------------------------

    def set_backoff(self, key: str, t: float, why: str, cfg: Any) -> float:
        b = self.backoff.get(key) or {"fails": 0}
        fails = int(b.get("fails", 0)) + 1
        wait = min(cfg.cooldown["preview_backoff_s"] * 2 ** (fails - 1), cfg.cooldown["preview_backoff_max_s"])
        self.backoff[key] = {"fails": fails, "until_t": round(t + wait, 3), "why": why}
        return t + wait

    def backoff_for(self, key: str, t: float) -> dict[str, Any] | None:
        b = self.backoff.get(key)
        return b if b and b["until_t"] > t else None

    def clear_backoff(self, key: str) -> None:
        self.backoff.pop(key, None)

    def tokens(self, t: float, cfg: Any) -> float:
        b = cfg.budget
        if not self.bucket:
            return float(b["human_burst"])
        refill = (t - self.bucket["t"]) / b["human_every_s"] if b["human_every_s"] > 0 else float("inf")
        return min(float(b["human_burst"]), self.bucket["tokens"] + max(0.0, refill))

    def take_token(self, t: float, cfg: Any) -> None:
        self.bucket = {"tokens": round(self.tokens(t, cfg) - 1.0, 6), "t": round(t, 3)}

    # ---- claims -------------------------------------------------------------------------------------

    def claims(self, own: dict[int, dict[str, Any]], vehicles: dict[int, Any]) -> dict[int, str]:
        """Vehicles held by a card or order of ours: one waiting or going out, or running (the
        vehicle's current order is ours), and in shadow mode a would-be card."""
        out: dict[int, str] = {}
        for iid in sorted(own):
            i = own[iid]
            if i.get("state") in OPEN:
                for u in i.get("uids") or []:
                    out.setdefault(int(u), f"our i{iid} ({i.get('state')})")
        # a hold of ours that parks a vehicle (isr.release, safety.stuck) leaves it free once it runs
        parks = ("isr.release", "safety.stuck")
        mine = {int(k) for k, e in self.entries.items() if e.get("play") not in parks}
        mine |= {iid for iid in own if (self.entries.get(str(iid)) or {}).get("play") not in parks}
        for uid, v in sorted(vehicles.items()):
            if v.task_intent is not None and v.task_intent in mine and (v.task or {}).get("state") not in NOT_RUNNING:
                out.setdefault(uid, f"our i{v.task_intent} (running)")
        for _, s in sorted(self.shadow.items()):
            if s.get("uid") is not None:
                out.setdefault(int(s["uid"]), f"would-be {s['play']} on {s['target']}")
        return out


def _by_from(reason: Any) -> str | None:
    """`rejected by op-1...` in an intent's history: the operator."""
    if not isinstance(reason, str):
        return None
    words = reason.split()
    if len(words) >= 3 and words[0] == "rejected" and words[1] == "by":
        return words[2].rstrip(":,;")
    return None
