"""The ledger: what the engine proposed and how each card fared, as JSON per bridge run.

A card's key is (play, target, uid). Its state is the bridge's (`get_intents` with `mine`,
every tick): proposed, pending_approval, approved, queued, dispatched, then rejected, expired,
voided, done or failed (the bridge has no `withdrawn` state: a withdrawal voids the intent, and
the ledger knows which voids were its own). An order of ours that ends without running, or
fails, is an answer, not a blank: each sets something the next proposal must get past.

- **rejected** by an operator: 10 minutes for (play, target), whatever vehicle. The operator's
  only reason channel is an optional note (there are no structured reject reasons yet), so "not
  this play on this target" is the reading that never sends the same card again with another
  vehicle on it. A proposal the checks refused at submission (state rejected, no operator) is
  not an operator's answer: it backs off like a failing preview instead.
- **expired** unread: 2 minutes for (play, target), and the play's score for that pair halves
  (the operator had no time: do not resend into the same queue).
- **voided** by the system: 30 seconds for the key (another vehicle may be tried at once).
- **withdrawn** by the engine (a void it asked for): 30 seconds for (play, target), every
  vehicle, as a rejection is: a premise that just went is not proposed again on the next
  vehicle in the same breath.
- **failed**: a back-off for the key and for the vehicle (10 s, doubling to 160 s), and the
  rationale of any retry names the failure. An order superseded by another of ours (a recentre,
  a release) is not a failure of ours. One superseded by someone else's order records that
  order (`superseded_by`): the vehicle is that order's while it stands, and `safety.low_reserve`
  reads it (it does not undo a person's re-tasking).

It also keeps what the reads alone do not say: the bridge run it belongs to (`run`: a ledger of
another run, or of no known run, is set aside and a fresh one started), the back-offs, the
inbox budget's token bucket, each vehicle's last recentre and last suspect navigation, the
parameters of others' orders a gap filler took over (`explain_intent`), the engagements the
change feed reported, when the playbook first saw each alert (`safety.stuck` counts from
there), the vehicles parked by `safety.stuck` and the task they were stuck on, the change feed's
cursor, each play's status (logged once it has held for a few ticks), the panel lines already
said, and in shadow mode the would-be cards.

A card proposed by an earlier process of this run (a restart) is adopted from its rationale's
evidence record: the record names its play, target, vehicle and whether it is armed.
"""

from __future__ import annotations

import json
import os
import re
import tempfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from . import rationale as R

NOT_RUNNING = ("failed", "superseded", "requeued")  # beliefs.NOT_RUNNING (no import cycle)

OPEN = ("proposed", "checked", "auto_approved", "pending_approval", "approved", "queued", "dispatched")
WAITING = ("pending_approval",)  # what the playbook withdraws: cards still waiting for the operator
APPROVED = ("approved", "queued")  # a person approved it: never withdrawn by the playbook
ENDED_UNRUN = ("rejected", "expired", "voided")
ENGAGE_KEEP = 200  # engagements remembered from the change feed (the newest)
FAILURE_MEMORY_S = 600.0  # a failure is named in a retry's rationale this long after it


def key_of(play: str, target: str, uid: int | None) -> str:
    return f"{play}|{target}|{'' if uid is None else uid}"


def pair_of(play: str, target: str) -> str:
    return f"{play}|{target}"


def vehicle_key(uid: int) -> str:
    return f"vehicle:{uid}"


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
    detail: str | None = None  # a failure's reason, a refusal's, a supersession


@dataclass
class Ledger:
    entries: dict[str, dict[str, Any]] = field(default_factory=dict)  # intent id -> entry
    cooldowns: list[dict[str, Any]] = field(default_factory=list)
    backoff: dict[str, dict[str, Any]] = field(default_factory=dict)  # key or vehicle:<uid> -> back-off
    score_factor: dict[str, float] = field(default_factory=dict)  # "play|target" -> factor
    recentred: dict[str, float] = field(default_factory=dict)  # uid -> t
    params: dict[str, dict[str, Any]] = field(default_factory=dict)  # intent id -> {task, params, uids}
    engaging: dict[str, dict[str, Any]] = field(default_factory=dict)  # intent id -> {uids, state}
    nav_bad_t: dict[str, float] = field(default_factory=dict)  # uid -> last tick it was suspect
    status: dict[str, str] = field(default_factory=dict)  # play -> last status line logged
    status_seen: dict[str, list[Any]] = field(default_factory=dict)  # play -> [status, ticks held]
    bucket: dict[str, float] = field(default_factory=dict)  # {tokens, t}
    cursor: str | None = None
    run: str | None = None  # the bridge run (its audit log's name) this ledger belongs to
    shadow: dict[str, dict[str, Any]] = field(default_factory=dict)  # key -> would-be card
    notes: dict[str, dict[str, Any]] = field(default_factory=dict)  # intent id -> operator note
    alert_seen: dict[str, float] = field(default_factory=dict)  # "<alert id>|<kind>" -> first tick seen
    stuck_blocks: list[dict[str, Any]] = field(default_factory=list)  # {play, target, uid, alert, until_t}
    noted: dict[str, str] = field(default_factory=dict)  # panel lines said once: ident -> text

    # ---- persistence ---------------------------------------------------------------------------

    FIELDS = ("entries", "backoff", "score_factor", "recentred", "params", "engaging", "nav_bad_t", "status",
              "status_seen", "bucket", "shadow", "notes", "alert_seen", "noted")

    def to_json(self) -> dict[str, Any]:
        out: dict[str, Any] = {"schema": 2, "run": self.run, "cursor": self.cursor, "cooldowns": self.cooldowns,
                               "stuck_blocks": self.stuck_blocks}
        for k in self.FIELDS:
            out[k] = getattr(self, k)
        return out

    @classmethod
    def from_json(cls, d: dict[str, Any]) -> "Ledger":
        led = cls()
        for k in cls.FIELDS:
            setattr(led, k, dict(d.get(k) or {}))
        led.cooldowns = list(d.get("cooldowns") or [])
        led.stuck_blocks = list(d.get("stuck_blocks") or [])
        led.cursor = d.get("cursor")
        led.run = d.get("run")
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
               prior_task: dict[str, Any] | None = None, tier: str | None = None, armed: bool = False,
               refused: str | None = None) -> None:
        self.entries[str(intent)] = {
            "intent": intent, "play": play, "version": version, "target": target, "uid": uid, "task": task,
            "request": request, "proposed_t": round(t, 3), "state": state, "state_t": round(t, 3),
            "basis": basis, "withdraw_if": list(withdraw_if), "prior_task": prior_task, "tier": tier, "armed": armed,
            "refused": refused,
        }

    def entry(self, intent: int) -> dict[str, Any] | None:
        return self.entries.get(str(intent))

    def open_entries(self) -> list[dict[str, Any]]:
        return [e for _, e in sorted(self.entries.items(), key=lambda kv: int(kv[0])) if e.get("state") in OPEN]

    def entries_for(self, play: str, target: str, uid: int | None = None) -> list[dict[str, Any]]:
        """This play's cards on this target (and vehicle), oldest first."""
        return [e for _, e in sorted(self.entries.items(), key=lambda kv: int(kv[0]))
                if e["play"] == play and e["target"] == target and (uid is None or e.get("uid") == uid)]

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
                    withdraw_if=list(rec.get("withdraw_if") or []),
                    armed=bool(rec.get("armed")) or play == "isr.armed_overwatch")
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
        self._trim_engaging()

    def set_engaging(self, nominations: list[dict[str, Any]]) -> None:
        """The engagements as `get_nominations` gives them (after a truncated change feed)."""
        for n in nominations:
            if n.get("id") is None:
                continue
            self.engaging[str(n["id"])] = {"uids": [int(u) for u in n.get("uids") or [] if str(u).isdigit()],
                                           "state": n.get("state")}
        self._trim_engaging()

    def _trim_engaging(self) -> None:
        if len(self.engaging) > ENGAGE_KEEP:
            for k in sorted(self.engaging, key=int)[: len(self.engaging) - ENGAGE_KEEP]:
                del self.engaging[k]

    def sync(self, intents: dict[int, dict[str, Any]], t: float, cfg: Any,
             tasks: dict[int, int | None] | None = None) -> list[Decision]:
        """Take each own card's state from the bridge, and set what each end leaves behind (the
        module docstring). `tasks`: uid -> the intent its vehicle's current order belongs to
        (the compact fleet read), to tell a supersession by someone else's order from one by
        ours. Returns what changed (for the situation log), in intent order."""
        out: list[Decision] = []
        tasks = tasks or {}
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
            hist = i.get("history") or []
            reason = str(hist[-1].get("reason") or "") if hist else ""
            if state == "rejected" and not by:
                by = _by_from(reason)
            detail = None
            until = None
            if state == "failed":
                detail = self._failed(e, reason, t, cfg, tasks)
            elif state == "rejected" and e.get("refused"):
                detail = f"refused by the checks at submission: {e['refused']}"
            else:
                until = self._cool(e, state, t, cfg, by, note)
            e["decided_by"], e["note"] = by, note
            out.append(Decision(intent=iid, play=e["play"], target=e["target"], uid=e.get("uid"), state=state,
                                by=by, note=note, cooldown_until=until, detail=detail))
        return out

    def _failed(self, e: dict[str, Any], reason: str, t: float, cfg: Any, tasks: dict[int, int | None]) -> str:
        uid = e.get("uid")
        mine = {int(k) for k in self.entries}
        if "superseded" in reason and uid is not None:
            by = tasks.get(int(uid))
            if by is not None and by in mine:
                e["superseded_by_ours"] = by
                return f"superseded by our i{by}"
            if by is not None:
                e["superseded_by"] = by
                e["failure"] = f"superseded by i{by}, someone else's order"
                return e["failure"]
        e["failure"] = reason or "failed"
        self.set_backoff(key_of(e["play"], e["target"], uid), t, f"i{e['intent']} failed: {e['failure'][:120]}", cfg)
        if uid is not None:
            self.set_backoff(vehicle_key(int(uid)), t, f"i{e['intent']} failed: {e['failure'][:120]}", cfg)
        return e["failure"]

    def _cool(self, e: dict[str, Any], state: str, t: float, cfg: Any, by: str | None, note: str | None) -> float | None:
        if state not in ENDED_UNRUN:
            return None
        cd = cfg.cooldown
        play, target, uid = e["play"], e["target"], e.get("uid")
        why = state
        if state == "rejected":
            scope, until = pair_of(play, target), t + cd["rejected_s"]
        elif state == "expired":
            scope, until = pair_of(play, target), t + cd["expired_s"]
            pair = pair_of(play, target)
            self.score_factor[pair] = round(self.score_factor.get(pair, 1.0) * cd["expired_score_factor"], 6)
        elif e.get("withdrawn_by"):
            why = "withdrawn"
            scope, until = pair_of(play, target), t + cd["withdrawn_s"]
        else:
            scope, until = key_of(play, target, uid), t + cd["voided_s"]
        self.cooldowns.append({"scope": scope, "until_t": round(until, 3), "why": why, "set_t": round(t, 3),
                               "intent": e["intent"], "by": by, "note": note, "basis": e.get("basis") or {}})
        return until

    def cooldown_for(self, play: str, target: str, uid: int | None, t: float) -> dict[str, Any] | None:
        """The cooldown in force on this key (its pair's or its own), the one ending last."""
        scopes = {pair_of(play, target), key_of(play, target, uid)}
        live = [c for c in self.cooldowns if c["scope"] in scopes and c["until_t"] > t]
        return max(live, key=lambda c: (c["until_t"], c["intent"])) if live else None

    def last_failure(self, play: str, target: str, uid: int | None, t: float) -> dict[str, Any] | None:
        """The latest failure of ours on this key or on this vehicle, within FAILURE_MEMORY_S."""
        hits = [e for e in self.entries.values()
                if e.get("state") == "failed" and e.get("failure") and t - float(e.get("state_t") or 0) <= FAILURE_MEMORY_S
                and ((e["play"], e["target"], e.get("uid")) == (play, target, uid) or (uid is not None and e.get("uid") == uid))]
        if not hits:
            return None
        e = max(hits, key=lambda e: (e["state_t"], e["intent"]))
        return {"intent": e["intent"], "play": e["play"], "target": e["target"], "failure": e["failure"],
                "t": e["state_t"]}

    def prune(self, t: float) -> None:
        self.cooldowns = [c for c in self.cooldowns if c["until_t"] > t - 3600]
        for k in [k for k, b in self.backoff.items() if b["until_t"] < t - 3600]:
            del self.backoff[k]
        self.stuck_blocks = [s for s in self.stuck_blocks if s.get("until_t") is None or s["until_t"] > t]

    # ---- back-offs, the budget ------------------------------------------------------------------------

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
        refill = (t - self.bucket["t"]) / b["human_every_s"]
        return min(float(b["human_burst"]), self.bucket["tokens"] + max(0.0, refill))

    def take_token(self, t: float, cfg: Any) -> None:
        self.bucket = {"tokens": round(self.tokens(t, cfg) - 1.0, 6), "t": round(t, 3)}

    # ---- alerts and stuck vehicles ---------------------------------------------------------------------

    def see_alerts(self, alerts: list[dict[str, Any]], t: float) -> None:
        """Remember when each alert was first seen active (keyed by its id and kind); forget the
        ones gone, so one that comes back counts from then."""
        now = {f"{a.get('id')}|{a.get('kind')}" for a in alerts}
        for k in sorted(now):
            self.alert_seen.setdefault(k, round(t, 3))
        for k in [k for k in self.alert_seen if k not in now]:
            del self.alert_seen[k]

    def block_stuck(self, play: str, target: str, uid: int, alert: str) -> None:
        self.stuck_blocks.append({"play": play, "target": target, "uid": uid, "alert": alert, "until_t": None})

    def update_stuck(self, alerts: list[dict[str, Any]], t: float, cfg: Any) -> None:
        """A stuck block runs while its alert is active, then `stuck_retry_s` more."""
        live = {f"{a.get('id')}|{a.get('kind')}" for a in alerts}
        for s in self.stuck_blocks:
            if s.get("until_t") is None and s["alert"] not in live:
                s["until_t"] = round(t + cfg.t["stuck_retry_s"], 3)

    def stuck_block(self, play: str, target: str, uid: int | None, t: float) -> dict[str, Any] | None:
        for s in self.stuck_blocks:
            if (s["play"], s["target"], s["uid"]) == (play, target, uid) and (s.get("until_t") is None or s["until_t"] > t):
                return s
        return None

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
    m = re.match(r"rejected by (\S+)", reason)
    return m.group(1).rstrip(":,;") if m else None
