"""When to wake the model, and when to wake the stronger one. Deterministic: the model is
woken by the trigger table below (or the slow heartbeat), never by its own choice, and never
for nothing.

The bridge's `get_changes` feed (crates/murmur-bridge/src/mcp/changes.rs) says what changed
since a cursor; the driver adds `nomination.*` changes from `get_nomination_options`, and
this module adds `condition.flapping`. Each change kind has a row: whether it wakes the
model, whether it escalates to the stronger model, and why. A change that does neither is a
note: it rides along with the next update.

- Bursts are coalesced: a wake fires `coalesce_s` after the first waking change, later if
  more keep coming (up to `coalesce_max_s`), and never sooner than `min_gap_s` after the last
  wake. A heartbeat that comes due while a waking change coalesces wakes for the change (and
  escalates if it does).
- A condition that came and went once inside one batch (a link lost and back, an alert
  raised and cleared, a GPS glitch flagged and cleared; the first live run woke the model
  twice for an AUV's 2-second acoustic link flaps) stays a note. One that does it a second
  time within `flap_window_s` becomes a waking `condition.flapping` change (escalating for a
  GPS glitch, an identity conflict, or an armed platform's link): intermittent glitching is
  what jamming and spoofing look like. While a condition is flapping its own raises are
  covered by the flapping change (notes).
- A subject is what a change is about: its kind, its track, vehicle, alert or flag, and the
  instance where there is one (a declaration's number, the affiliation it moved to, an
  intent's new state). One subject wakes the model at most once per `per_subject_s` and
  escalates at most once per `escalate_cooldown_s`, so one contact whose conflict keeps
  reappearing cannot spend the run's escalations or the hour's wakes. A condition that clears
  ends its subject's interval (unless it is flapping): it raised again is news, and wakes
  (and escalates) as the first time did. A change held back as a repeat stays pending, is
  not used as filler by another wake, and becomes a trigger of its own when its interval
  ends; the wake says it was held back.
- The heartbeat backs off (doubling up to `heartbeat_max_s`) while nothing new arrives:
  autopilot chatter, vehicles joining and repeats of a subject just reported are not news.
  Heartbeats never take the last quarter of the hour's wakes: those are kept for triggers.
- A change that cannot be judged (malformed) is dropped and listed in `dropped`, for the
  driver to log: one bad change never stops the watch.
- Budgets (`Budgets`) cap wakes per hour, escalations per run and wakes per run.
"""

from __future__ import annotations

import json
from collections import deque
from dataclasses import dataclass, field
from typing import Any, Callable

Change = dict
NEVER = float("-inf")


def _d(v: Any) -> dict:
    return v if isinstance(v, dict) else {}


def _armed(c: Change) -> bool:
    return c.get("armed") is True


def _mine(c: Change) -> bool:
    return c.get("mine") is True


SUSPICIOUS = ("hostile", "suspect", "unknown")
# States of its own proposals that someone or something decided, worth its attention. Not
# `done`: a task that finished as asked needs no decision (a note).
DECIDED = ("approved", "rejected", "voided", "expired", "failed", "aborted", "queued", "dispatched",
           "effected")


@dataclass(frozen=True)
class Row:
    kind: str
    wake: Callable[[Change], bool]
    escalate: Callable[[Change], bool]
    why: str


def always(_: Change) -> bool:
    return True


def never(_: Change) -> bool:
    return False


def _flap_escalates(c: Change) -> bool:
    cond = c.get("condition")
    return cond in ("gps_glitch", "identity_conflict") or (cond == "link" and _armed(c))


# The trigger table. `wake`: the change wakes the model. `escalate`: that wake goes to the
# stronger model (an engagement-chain moment, a lost link on an armed platform, a suspected
# spoof). Kinds not listed are notes.
TRIGGERS: tuple[Row, ...] = (
    # the engagement chain: a hostile declared (by an operator or the engine's hostile-act
    # rule, or re-declared) makes a nomination possible; a platform passing every check
    # opens the window
    Row("track.declared", always, lambda c: c.get("declared") == "hostile",
        "a track was declared"),
    Row("nomination.possible", always, always,
        "an armed platform passes every nomination check on a hostile track"),
    Row("track.declaration_revoked", always, never, "a declaration was revoked"),
    # a contact worth a look: new and not friendly or neutral, or its estimate moved
    # toward (or away from) suspicion
    Row("track.new", lambda c: c.get("affiliation") in SUSPICIOUS, never,
        "a new contact that is not known to be friendly or neutral"),
    Row("track.affiliation",
        lambda c: c.get("to") in SUSPICIOUS or c.get("from") in ("hostile", "suspect"),
        never, "a contact's affiliation estimate changed"),
    # an AIS identity that does not fit what our sensors saw: a possible spoof
    Row("track.identity_conflict", always, always, "an identity conflict (possible AIS spoof)"),
    Row("review.flag", always, never, "the engine flagged something for review"),
    # our own vehicles: link and navigation
    Row("vehicle.link_lost", always, _armed, "a vehicle lost its link"),
    Row("vehicle.gps_glitch", lambda c: c.get("glitch") is True, lambda c: c.get("glitch") is True,
        "a vehicle's navigation filter flags its GPS as glitching (possible spoof or jamming)"),
    # navigation not ready, or off GNSS for a vehicle that is not a submarine (an AUV
    # dives onto dead reckoning as a matter of course)
    Row("vehicle.nav",
        lambda c: _d(c.get("to")).get("ready") is False
        or (_d(c.get("to")).get("source") in ("dead_reckoning", "none")
            and c.get("domain") != "subsurface"),
        never, "a vehicle's navigation is not ready or lost GNSS"),
    Row("vehicle.event", lambda c: c.get("event_kind") == "failsafe", never, "a failsafe"),
    Row("alert.raised", lambda c: c.get("severity") == "high", never, "a high-severity alert"),
    # the same alert, now graver (an order that may be running, say)
    Row("alert.changed",
        lambda c: c.get("severity") == "high" and _d(c.get("from")).get("severity") != "high",
        never, "an alert became high-severity"),
    # a condition coming and going again and again (see the module's notes)
    Row("condition.flapping", always, _flap_escalates, "a condition keeps coming and going"),
    # the model's own proposals: the operator or the rules decided; a proposal decided before
    # the driver saw it pending arrives as new in its decided state
    Row("intent.state", lambda c: _mine(c) and c.get("to") in DECIDED, never,
        "one of your proposals changed state"),
    Row("intent.new", lambda c: _mine(c) and c.get("state") in DECIDED, never,
        "one of your proposals changed state"),
    # the bridge restarted under us (a cursor from an earlier bridge)
    Row("feed.reset", always, never, "the bridge restarted: read the picture again"),
)
ROWS = {r.kind: r for r in TRIGGERS}


def classify(c: Change) -> tuple[bool, bool, str | None]:
    """(wakes, escalates, why) for one change, by the table alone."""
    row = ROWS.get(str(c.get("kind")))
    if row is None:
        return False, False, None
    wakes = bool(row.wake(c))
    return wakes, wakes and bool(row.escalate(c)), row.why if wakes else None


def _h(v: Any) -> Any:
    """A hashable form of any value (a dict or a list as its sorted JSON)."""
    if v is None or isinstance(v, (str, int, float, bool)):
        return v
    try:
        return json.dumps(v, sort_keys=True, default=str)
    except (TypeError, ValueError):
        return repr(v)


def subject(c: Change) -> tuple:
    """What a change is about, for the per-subject limits: its kind, the item (track, alert,
    flag, vehicle or intent) and, where there is one, the instance: a declaration's number
    (else the affiliation declared), the affiliation an estimate moved to, a flapping
    condition, an intent's new state. Hashable whatever the change holds."""
    k = _h(c.get("kind"))
    if str(k).startswith("intent."):
        return ("intent", _h(c.get("intent")), _h(c.get("to") or c.get("state")))
    item: tuple = ()
    for name in ("track", "alert", "flag", "uid"):
        if c.get(name) is not None:
            item = (name, _h(c.get(name)))
            break
    instance = None
    if k == "track.declared":
        instance = _h(c.get("declaration") or c.get("declared"))
    elif k == "track.affiliation":
        instance = _h(c.get("to"))
    elif k == "condition.flapping":
        instance = _h(c.get("condition"))
    return (k, *item, instance)


def _pair(c: Change) -> tuple[tuple, bool] | None:
    """For a condition that can come and go: (its key, whether this change raises it)."""
    k = c.get("kind")
    if k in ("vehicle.link_lost", "vehicle.link_restored"):
        return ("link", _h(c.get("uid"))), k == "vehicle.link_lost"
    if k in ("alert.raised", "alert.cleared"):
        return ("alert", _h(c.get("alert"))), k == "alert.raised"
    if k == "vehicle.gps_glitch":
        return ("gps_glitch", _h(c.get("uid"))), c.get("glitch") is True
    if k in ("nomination.possible", "nomination.closed"):
        return ("nomination", _h(c.get("track"))), k == "nomination.possible"
    if k in ("track.identity_conflict", "track.identity_cleared"):
        return ("identity_conflict", _h(c.get("track"))), k == "track.identity_conflict"
    if k in ("track.declared", "track.declaration_revoked"):
        return ("declaration", _h(c.get("track"))), k == "track.declared"
    return None


def came_and_went(changes: list[Change]) -> set[int]:
    """Indices of changes whose condition was raised and cleared again within the batch (a
    2-second acoustic link flap, an alert that cleared itself): notes, not a reason to wake."""
    open_: dict[tuple, int] = {}
    gone: set[int] = set()
    for i, c in enumerate(changes):
        p = _pair(c)
        if p is None:
            continue
        key, raises = p
        if raises:
            open_[key] = i
        elif key in open_:
            gone.update((open_.pop(key), i))
    return gone


def live(changes: list[Change]) -> list[Change]:
    """The changes still in force at the end of the batch."""
    gone = came_and_went(changes)
    return [c for i, c in enumerate(changes) if i not in gone]


@dataclass
class Budgets:
    max_wakes_per_hour: int = 60
    max_turns_per_wake: int = 12
    max_escalations: int = 6
    max_wakes: int = 400  # total for the run
    heartbeat_s: float = 60.0  # 0: off
    heartbeat_max_s: float = 300.0  # quiet heartbeats back off to this
    coalesce_s: float = 3.0
    coalesce_max_s: float = 10.0
    min_gap_s: float = 10.0
    per_subject_s: float = 300.0  # one subject wakes the model at most this often
    escalate_cooldown_s: float = 1800.0  # and escalates at most this often
    flap_window_s: float = 300.0  # a second come-and-go within this is flapping


@dataclass
class Wake:
    n: int
    reason: str  # start | changes | heartbeat
    changes: list[Change]
    why: list[str]
    escalate: bool
    escalate_why: list[str]
    t: float
    # repeats held back (they stay pending and wake on their own when their interval ends)
    held: list[str] = field(default_factory=list)
    # the subjects this wake woke and escalated on (to give them back if the turn fails)
    woke_subjects: list = field(default_factory=list)
    escalated_subjects: list = field(default_factory=list)


@dataclass
class Judged:
    wakes: bool  # wakes the model now
    escalates: bool  # and escalates now
    why: str | None
    since: float  # since when it may wake (NEVER: at once)
    held: bool  # a waking kind held back as a repeat (not yet eligible)


@dataclass
class Waker:
    """Collects changes and says when to wake. Pure logic over an injected clock."""

    budgets: Budgets
    escalation: bool = True  # False: never use the stronger model
    pending: list[Change] = field(default_factory=list)
    arrived: list[float] = field(default_factory=list)  # when each pending change came
    first_wake_t: float | None = None  # when the first waking change may wake
    last_change_t: float | None = None
    last_wake_t: float | None = None
    wakes: int = 0
    escalations: int = 0
    recent: deque = field(default_factory=deque)  # wake times in the last hour
    quiet: int = 0  # heartbeats in a row with nothing new
    deferred: bool = False  # the hourly cap is holding a wake back (logged once)
    started: bool = False
    woke: dict = field(default_factory=dict)  # subject -> when it last woke the model
    escalated: dict = field(default_factory=dict)  # subject -> when it last escalated
    raised: dict = field(default_factory=dict)  # come-and-go key -> its open raise
    cycles: dict = field(default_factory=dict)  # come-and-go key -> times it cleared again
    flapped: dict = field(default_factory=dict)  # come-and-go key -> when flapping was said
    dropped: list = field(default_factory=list)  # (change, error) that could not be judged

    # ---- judging one change ---------------------------------------------------------------

    def _covered(self, c: Change, now: float) -> bool:
        """A raise of a condition that is flapping now: the flapping change covers it."""
        p = _pair(c)
        return bool(p and p[1] and now - self.flapped.get(p[0], NEVER) < self.budgets.flap_window_s)

    def judge(self, c: Change, now: float) -> Judged:
        wakes, esc, why = classify(c)
        if not wakes or self._covered(c, now):
            return Judged(False, False, None, NEVER, False)
        subj = subject(c)
        last = self.woke.get(subj)
        since = NEVER if last is None else last + self.budgets.per_subject_s
        if now < since:
            return Judged(False, False, why, since, True)
        esc_now = esc and now - self.escalated.get(subj, NEVER) >= self.budgets.escalate_cooldown_s
        return Judged(True, esc_now, why, since, False)

    def wakes_now(self, c: Change, now: float) -> bool:
        return self.judge(c, now).wakes

    def escalates_now(self, c: Change, now: float) -> bool:
        return self.judge(c, now).escalates

    def _drop_unjudgeable(self, now: float) -> None:
        keep_c, keep_t = [], []
        for c, t in zip(self.pending, self.arrived):
            try:
                self.judge(c, now)
                _pair(c)
            except Exception as e:  # noqa: BLE001 (one bad change never stops the watch)
                self.dropped.append((c, f"{type(e).__name__}: {e}"))
                continue
            keep_c.append(c)
            keep_t.append(t)
        self.pending, self.arrived = keep_c, keep_t

    # ---- conditions that come and go -------------------------------------------------------

    def _pairs(self, c: Change, now: float) -> list[Change]:
        """Track one change's condition: a clear ends its raise's interval (it raised again is
        news) unless the condition is flapping; a second come-and-go within `flap_window_s`
        is a `condition.flapping` change (said once a window per condition)."""
        p = _pair(c)
        if p is None:
            return []
        key, raises = p
        if raises:
            self.raised[key] = c
            return []
        first = self.raised.pop(key, None)
        if first is None:
            return []
        window = self.budgets.flap_window_s
        q = self.cycles.setdefault(key, deque())
        q.append(now)
        while q and q[0] < now - window:
            q.popleft()
        if len(q) < 2:
            # not flapping: the raise's interval ends with it, so a raise again is news
            subj = subject(first)
            self.woke.pop(subj, None)
            self.escalated.pop(subj, None)
            return []
        if now - self.flapped.get(key, NEVER) < window:
            return []
        self.flapped[key] = now
        keep = {k: first.get(k) for k in ("uid", "callsign", "platform", "armed", "track", "alert",
                                          "alert_kind", "severity") if first.get(k) is not None}
        return [{"kind": "condition.flapping", "t": c.get("t", now), "condition": key[0],
                 "cycles": len(q), "window_s": window, **keep}]

    # ---- the clock -------------------------------------------------------------------------

    def news(self, c: Change, now: float) -> bool:
        """Whether a change is news for the heartbeat's back-off: not autopilot chatter, not
        a vehicle joining, not a repeat of a subject the model was just told about."""
        if c.get("kind") in ("vehicle.event", "vehicle.joined", "vehicle.events_truncated"):
            return False
        try:
            return not self.judge(c, now).held
        except Exception:  # noqa: BLE001
            return False

    def add(self, changes: list[Change], now: float) -> None:
        if not changes:
            return
        accepted: list[Change] = []
        for c in changes:
            try:
                if not isinstance(c, dict):
                    raise TypeError(f"a change is a dict, not {type(c).__name__}")
                extra = self._pairs(c, now)
                self.judge(c, now)
            except Exception as e:  # noqa: BLE001 (one bad change never stops the watch)
                self.dropped.append((c, f"{type(e).__name__}: {e}"))
                continue
            accepted.append(c)
            accepted.extend(extra)
        if not accepted:
            return
        self.pending.extend(accepted)
        self.arrived.extend([now] * len(accepted))
        self.last_change_t = now
        if any(self.news(c, now) for c in accepted):
            self.quiet = 0
        if self.first_wake_t is None and any(self.wakes_now(c, now) for c in accepted):
            self.first_wake_t = now

    def heartbeat_every(self) -> float:
        b = self.budgets
        if b.heartbeat_s <= 0:
            return float("inf")
        return min(b.heartbeat_s * (2 ** self.quiet), max(b.heartbeat_max_s, b.heartbeat_s))

    def hourly_left(self, now: float) -> int:
        while self.recent and self.recent[0] <= now - 3600:
            self.recent.popleft()
        return self.budgets.max_wakes_per_hour - len(self.recent)

    def exhausted(self) -> bool:
        return self.wakes >= self.budgets.max_wakes

    def _live_indices(self) -> set[int]:
        gone = came_and_went(self.pending)
        return {i for i in range(len(self.pending)) if i not in gone}

    def due(self, now: float) -> Wake | None:
        """The wake to run now, if any (and record it)."""
        b = self.budgets
        if self.exhausted():
            return None
        self._drop_unjudgeable(now)
        if not self.started:
            return self._take(now, "start")
        live_i = self._live_indices()
        # each waking change from when it may wake (a held-back repeat: when its interval
        # ended), so a repeat becomes a trigger of its own
        waking = [(i, j) for i in sorted(live_i) if (j := self.judge(self.pending[i], now)).wakes]
        eligible = [max(self.arrived[i], j.since) for i, j in waking]
        self.first_wake_t = min(eligible) if eligible else None
        held_next = [j.since for i in live_i if (j := self.judge(self.pending[i], now)).held]
        if held_next:
            # when the next held-back repeat becomes a trigger (for next_check)
            self.first_wake_t = min([self.first_wake_t or float("inf"), min(held_next)])
        if self.last_wake_t is not None and now - self.last_wake_t < b.min_gap_s:
            return None
        reason = None
        if eligible:
            first = min(eligible)
            settled = self.last_change_t is None or now - self.last_change_t >= b.coalesce_s \
                or self.last_change_t <= first
            if (now - first >= b.coalesce_s and settled) or now - first >= b.coalesce_max_s:
                reason = "changes"
        if reason is None and self.last_wake_t is not None and now - self.last_wake_t >= self.heartbeat_every():
            # a heartbeat that finds a waking change coalescing wakes for the change
            reason = "changes" if waking else "heartbeat"
        if reason is None:
            return None
        escalates = self.escalation and self.escalations < b.max_escalations and any(j.escalates for _, j in waking)
        if self.hourly_left(now) <= 0 and not escalates:
            self.deferred = True
            return None
        if reason == "heartbeat" and self.hourly_left(now) <= b.max_wakes_per_hour // 4:
            return None  # the last quarter of the hour's wakes is kept for triggers
        if reason == "heartbeat" and not any(self.news(c, now) for c in self.pending):
            self.quiet += 1
        return self._take(now, reason, escalates)

    def _take(self, now: float, reason: str, escalates: bool = False) -> Wake:
        self.started = True
        live_i = self._live_indices()
        deliver: list[Change] = []
        keep_c: list[Change] = []
        keep_t: list[float] = []
        why: list[str] = []
        esc_why: list[str] = []
        held: list[str] = []
        woke_s: list = []
        esc_s: list = []
        for i, (c, at) in enumerate(zip(self.pending, self.arrived)):
            if i not in live_i:
                deliver.append(c)  # came and went: a note
                continue
            j = self.judge(c, now)
            if j.held:
                # a repeat inside its interval: stays pending, a trigger of its own later
                keep_c.append(c)
                keep_t.append(at)
                if j.why and j.why not in held:
                    held.append(j.why)
                continue
            deliver.append(c)
            if not j.wakes:
                continue
            text = j.why or ""
            if at < j.since:
                text += f" (held back as a repeat for {j.since - at:.0f} s)"
            if text and text not in why:
                why.append(text)
            subj = subject(c)
            if escalates and j.escalates:
                self.escalated[subj] = now
                esc_s.append(subj)
                if j.why and j.why not in esc_why:
                    esc_why.append(j.why)
            self.woke[subj] = now
            woke_s.append(subj)
        self.pending, self.arrived = keep_c, keep_t
        self.first_wake_t = None
        self.last_wake_t = now
        self.recent.append(now)
        self.wakes += 1
        self.deferred = False
        if escalates:
            self.escalations += 1
        return Wake(self.wakes, reason, deliver, why, escalates, esc_why, now, held, woke_s, esc_s)

    def give_back(self, wake: Wake, now: float) -> None:
        """A wake whose turn failed: its changes come back (to be told again), its subjects
        may wake again, and an escalation it spent is returned."""
        for s in wake.woke_subjects:
            self.woke.pop(s, None)
        self.unescalate(wake)
        if wake.reason == "start":
            self.started = False  # the opening update is told again, as the opening
        self.pending = list(wake.changes) + self.pending
        self.arrived = [now] * len(wake.changes) + self.arrived
        self.last_change_t = now
        if any(self.wakes_now(c, now) for c in live(self.pending)):
            self.first_wake_t = now

    def unescalate(self, wake: Wake) -> None:
        """A wake that did not go to the stronger model after all (its turn failed, or the
        stronger model was limited): its escalation and the subjects' cooldowns come back."""
        for s in wake.escalated_subjects:
            self.escalated.pop(s, None)
        if wake.escalate:
            self.escalations = max(0, self.escalations - 1)
        wake.escalate = False
        wake.escalated_subjects = []

    def take_dropped(self) -> list:
        out, self.dropped = self.dropped, []
        return out

    def next_check(self, now: float) -> float:
        """When `due` could next say yes (the caller polls at least this often)."""
        b = self.budgets
        times = []
        if self.first_wake_t is not None:
            times.append(self.first_wake_t + b.coalesce_s)
        if self.last_wake_t is not None:
            times.append(self.last_wake_t + max(b.min_gap_s, min(self.heartbeat_every(), 1e9)))
        return max(now, min(times)) if times else now + 1.0
