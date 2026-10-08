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
  what jamming and spoofing look like.
- The same subject (a track, a vehicle, an alert, one of its intents in one state) wakes the
  model at most once per `per_subject_s` and escalates at most once per
  `escalate_cooldown_s`: one contact whose conflict keeps reappearing cannot spend the run's
  escalations or the hour's wakes.
- The heartbeat backs off (doubling up to `heartbeat_max_s`) while nothing new arrives:
  autopilot chatter, vehicles joining and repeats of a subject just reported are not news.
  Heartbeats never take the last quarter of the hour's wakes: those are kept for triggers.
- Budgets (`Budgets`) cap wakes per hour, escalations per run and wakes per run.
"""

from __future__ import annotations

from collections import deque
from dataclasses import dataclass, field
from typing import Callable

Change = dict
NEVER = float("-inf")


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
        lambda c: (c.get("to") or {}).get("ready") is False
        or ((c.get("to") or {}).get("source") in ("dead_reckoning", "none")
            and c.get("domain") != "subsurface"),
        never, "a vehicle's navigation is not ready or lost GNSS"),
    Row("vehicle.event", lambda c: c.get("event_kind") == "failsafe", never, "a failsafe"),
    Row("alert.raised", lambda c: c.get("severity") == "high", never, "a high-severity alert"),
    # the same alert, now graver (an order that may be running, say)
    Row("alert.changed",
        lambda c: c.get("severity") == "high" and (c.get("from") or {}).get("severity") != "high",
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


def subject(c: Change) -> tuple:
    """What a change is about, for the per-subject limits: its kind, its track, vehicle,
    alert or intent, and what it says about it (a re-declaration to another affiliation, an
    intent reaching another state, is another subject)."""
    k = c.get("kind")
    if str(k).startswith("intent."):
        return ("intent", c.get("intent"), c.get("to") or c.get("state"))
    for field_ in ("track", "alert", "flag", "uid"):
        if c.get(field_) is not None:
            return (k, field_, c.get(field_), c.get("declared") or c.get("to") or c.get("condition"))
    return (k,)


def _pair(c: Change) -> tuple[tuple, bool] | None:
    """For a condition that can come and go: (its key, whether this change raises it)."""
    k = c.get("kind")
    if k in ("vehicle.link_lost", "vehicle.link_restored"):
        return ("link", c.get("uid")), k == "vehicle.link_lost"
    if k in ("alert.raised", "alert.cleared"):
        return ("alert", c.get("alert")), k == "alert.raised"
    if k == "vehicle.gps_glitch":
        return ("gps_glitch", c.get("uid")), c.get("glitch") is True
    if k in ("nomination.possible", "nomination.closed"):
        return ("nomination", c.get("track")), k == "nomination.possible"
    if k in ("track.identity_conflict", "track.identity_cleared"):
        return ("identity_conflict", c.get("track")), k == "track.identity_conflict"
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


@dataclass
class Waker:
    """Collects changes and says when to wake. Pure logic over an injected clock."""

    budgets: Budgets
    escalation: bool = True  # False: never use the stronger model
    pending: list[Change] = field(default_factory=list)
    first_wake_t: float | None = None  # first waking change not yet woken on
    last_change_t: float | None = None
    last_wake_t: float | None = None
    wakes: int = 0
    escalations: int = 0
    recent: deque = field(default_factory=deque)  # wake times in the last hour
    quiet: int = 0  # heartbeats in a row with nothing changed
    deferred: bool = False  # the hourly cap is holding a wake back (logged once)
    started: bool = False
    woke: dict = field(default_factory=dict)  # subject -> when it last woke the model
    escalated: dict = field(default_factory=dict)  # subject -> when it last escalated
    raised: dict = field(default_factory=dict)  # come-and-go key -> its open raise
    cycles: dict = field(default_factory=dict)  # come-and-go key -> times it cleared again
    flapped: dict = field(default_factory=dict)  # come-and-go key -> when flapping was said

    def wakes_now(self, c: Change, now: float) -> bool:
        return classify(c)[0] and now - self.woke.get(subject(c), NEVER) >= self.budgets.per_subject_s

    def escalates_now(self, c: Change, now: float) -> bool:
        return (classify(c)[1] and self.wakes_now(c, now)
                and now - self.escalated.get(subject(c), NEVER) >= self.budgets.escalate_cooldown_s)

    def _flaps(self, changes: list[Change], now: float) -> list[Change]:
        """`condition.flapping` for a condition that came and went a second time within
        `flap_window_s` (said once a window per condition)."""
        out = []
        window = self.budgets.flap_window_s
        for c in changes:
            p = _pair(c)
            if p is None:
                continue
            key, raises = p
            if raises:
                self.raised[key] = c
                continue
            first = self.raised.pop(key, None)
            if first is None:
                continue
            q = self.cycles.setdefault(key, deque())
            q.append(now)
            while q and q[0] < now - window:
                q.popleft()
            if len(q) >= 2 and now - self.flapped.get(key, NEVER) >= window:
                self.flapped[key] = now
                keep = {k: first.get(k) for k in ("uid", "callsign", "platform", "armed", "track", "alert",
                                                  "alert_kind", "severity") if first.get(k) is not None}
                out.append({"kind": "condition.flapping", "t": c.get("t", now), "condition": key[0],
                            "cycles": len(q), "window_s": window, **keep})
        return out

    def news(self, c: Change, now: float) -> bool:
        """Whether a change is news for the heartbeat's back-off: not autopilot chatter, not
        a vehicle joining, not a repeat of a subject the model was just told about."""
        if c.get("kind") in ("vehicle.event", "vehicle.joined", "vehicle.events_truncated"):
            return False
        return not (classify(c)[0] and not self.wakes_now(c, now))

    def add(self, changes: list[Change], now: float) -> None:
        if not changes:
            return
        changes = list(changes) + self._flaps(changes, now)
        self.pending.extend(changes)
        self.last_change_t = now
        if any(self.news(c, now) for c in changes):
            self.quiet = 0
        if self.first_wake_t is None and any(self.wakes_now(c, now) for c in changes):
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

    def due(self, now: float) -> Wake | None:
        """The wake to run now, if any (and record it)."""
        b = self.budgets
        if self.exhausted():
            return None
        if not self.started:
            return self._take(now, "start")
        if self.last_wake_t is not None and now - self.last_wake_t < b.min_gap_s:
            return None
        waking = [c for c in live(self.pending) if self.wakes_now(c, now)]
        if self.first_wake_t is not None and not waking:
            self.first_wake_t = None  # everything that would wake came and went: notes only
        reason = None
        if self.first_wake_t is not None:
            settled = self.last_change_t is None or now - self.last_change_t >= b.coalesce_s
            if (now - self.first_wake_t >= b.coalesce_s and settled) or now - self.first_wake_t >= b.coalesce_max_s:
                reason = "changes"
        if reason is None and self.last_wake_t is not None and now - self.last_wake_t >= self.heartbeat_every():
            # a heartbeat that finds a waking change coalescing wakes for the change
            reason = "changes" if waking else "heartbeat"
        if reason is None:
            return None
        escalates = self.escalation and self.escalations < b.max_escalations \
            and any(self.escalates_now(c, now) for c in waking)
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
        changes, self.pending = self.pending, []
        why, esc_why = [], []
        for c in live(changes):
            if not self.wakes_now(c, now):
                continue
            text = classify(c)[2]
            if text and text not in why:
                why.append(text)
            if escalates and self.escalates_now(c, now):
                self.escalated[subject(c)] = now
                if text and text not in esc_why:
                    esc_why.append(text)
            self.woke[subject(c)] = now
        self.first_wake_t = None
        self.last_wake_t = now
        self.recent.append(now)
        self.wakes += 1
        self.deferred = False
        if escalates:
            self.escalations += 1
        return Wake(self.wakes, reason, changes, why, escalates, esc_why, now)

    def give_back(self, wake: Wake, now: float) -> None:
        """A wake whose turn failed: its changes come back (to be told again), its subjects
        may wake again, and an escalation it spent is returned."""
        for c in live(wake.changes):
            self.woke.pop(subject(c), None)
            if wake.escalate:
                self.escalated.pop(subject(c), None)
        if wake.escalate:
            self.escalations = max(0, self.escalations - 1)
        self.pending = list(wake.changes) + self.pending
        self.last_change_t = now
        if any(self.wakes_now(c, now) for c in live(self.pending)):
            self.first_wake_t = now

    def next_check(self, now: float) -> float:
        """When `due` could next say yes (the caller polls at least this often)."""
        b = self.budgets
        times = []
        if self.first_wake_t is not None:
            times.append(self.first_wake_t + b.coalesce_s)
        if self.last_wake_t is not None:
            times.append(self.last_wake_t + max(b.min_gap_s, min(self.heartbeat_every(), 1e9)))
        return max(now, min(times)) if times else now + 1.0
