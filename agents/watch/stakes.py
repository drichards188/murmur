"""Whether a moment about a contact has the stakes for the senior consult, and where the contact
stands against the protected line (bd murmur-5xaf; DECISIONS.md 2026-11-14).

The paid smoke of 2026-11-14 sent six of its eight consults to "an identity conflict (possible
AIS spoof)", most of them one sonar call against a ship's AIS, at least four about contacts 18
to 30 km out and not closing. So an identity conflict, and a conflict coming and going
(`condition.flapping` of `identity_conflict`), goes to the stronger model only when its contact
matters to the watch; otherwise it is a routine wake, exactly as before. Every other escalating
trigger is unchanged: the engagement chain, a GPS glitch or an armed platform's lost link (and
their flapping: those are about our own vehicles, which a contact's stakes do not describe).

**Where the watch is**: the situation's `[playbook]` watch areas and protected points, the table
the playbook engine reads (a live situation's runner writes it into the AI's directory as
`playbook.json` and passes it as `--watch-config`; a headless episode hands the officer the
config its playbook would run on). No other source carries the protected area in both: the
bridge's `get_areas` has approved, no-go, engagement and no-strike areas only, and a brief is
free text. With neither a watch area nor a protected point (no table, or one without them),
nothing is known, and the gate lets every escalation through, as before.

**When a contact matters** (`Watch.standing`): the playbook's own relevance geometry
(agents/playbook/beliefs.py `interest_of`, `closing_alert`, `cpa`, on the config's thresholds:
`cpa_alert_m` 457 m, `tcpa_max_s` 600 s, `holding_mps`), as `isr.investigate` reads it: inside a
watch area, or closing on a protected point (its closest approach within 457 m inside 10 minutes,
or already within 457 m); and one thing the playbook does not need: coming into a watch area
within those 10 minutes on its course. The playbook judges every track afresh each tick and sees
a contact once it is inside; the gate judges a change once, when it arrives, so it looks ahead by
the same horizon. A contact with no speed or course is judged where it is, holding (as the
playbook judges one with no velocity).

**The contact's position** comes with the change itself (the change feed's track summary: its
last fix, speed, course and `as_of`), carried along its course to the moment the wake is judged:
the gate reads nothing from the bridge, so the `Waker` stays pure. A change without a position
(or one the gate cannot read) escalates as before: the gate fails toward escalating.
"""

from __future__ import annotations

import math
from dataclasses import dataclass
from typing import Any

from playbook.beliefs import M_PER_DEG, closing_alert, enu, haversine_m, in_polygon, interest_of

#: the escalations the gate judges: a conflict on a contact (and one the driver found has come
#: to matter since: agents/watch/picture.py `Poller.stakes`), and a conflict coming and going
GATED = ("track.identity_conflict", "track.conflict_matters")
GATED_FLAPPING = ("identity_conflict",)
#: how far ahead a contact in doubt is looked for coming into a watch area (the routine wake, bd
#: murmur-1h2r; `Watch.entry`). A drone first seen 4 to 5 km out came inside the 3 km circle 61 to 162 s
#: later (median 108) and its identification had a 30 s air deadline after that: the wake has to
#: leave the time to launch and fly a camera, so it comes this long before the line. Far enough that
#: a drone at first sight (1 to 2 km outside the line, 40 to 80 s at its speed) is already inside the
#: horizon; short enough that a ferry kilometres off, or one passing the area, wakes nothing.
APPROACH_HORIZON_S = 180.0
#: a track with no velocity yet in the air is taken to come straight in at this speed (the
#: evaluation's honest-ceiling rule for a small drone: the threat's own speed, not the tracker's
#: unknown). Other domains with no velocity are judged where they are.
AIR_NO_VELOCITY_MPS = 25.0
#: a polygon area's way in is looked for along the course every this many seconds
STEP_S = 5.0


def gated(c: dict) -> bool:
    """A change whose escalation the gate judges."""
    k = c.get("kind")
    return k in GATED or (k == "condition.flapping" and c.get("condition") in GATED_FLAPPING)


def _num(v: Any) -> float | None:
    return float(v) if isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v) else None


def _point(p: Any) -> tuple[float, float] | None:
    if isinstance(p, (list, tuple)) and len(p) == 2 and all(_num(x) is not None for x in p):
        return float(p[0]), float(p[1])
    return None


def velocity_of(speed: Any, course: Any) -> tuple[float, float] | None:
    """(east, north) m/s from a speed and a course (degrees from north), or None."""
    v, c = _num(speed), _num(course)
    if v is None or c is None:
        return None
    return v * math.sin(math.radians(c)), v * math.cos(math.radians(c))


def carried(p: tuple[float, float], vel: tuple[float, float] | None, dt_s: float) -> tuple[float, float]:
    """`p` carried along `vel` for `dt_s` (the bridge's own figure, as `get_tracks`' `predicted`)."""
    if vel is None or dt_s <= 0:
        return p
    return (p[0] + vel[0] * dt_s / (M_PER_DEG * math.cos(math.radians(p[1]))), p[1] + vel[1] * dt_s / M_PER_DEG)


@dataclass(frozen=True)
class _Where:
    """What `interest_of` reads of a track: where it is and its velocity."""

    where: tuple[float, float] | None
    velocity: tuple[float, float] | None


def _seg_m(p: tuple[float, float], a: tuple[float, float], b: tuple[float, float]) -> float:
    """Metres from `p` to the segment a-b (a local plane round `p`)."""
    ax, ay = enu(a, p)
    bx, by = enu(b, p)
    dx, dy = bx - ax, by - ay
    L = dx * dx + dy * dy
    t = 0.0 if L == 0 else max(0.0, min(1.0, -(ax * dx + ay * dy) / L))
    return math.hypot(ax + t * dx, ay + t * dy)


@dataclass
class Line:
    """A contact against one watch area: inside or not, metres to its edge now (positive
    outside), when it comes in on its course (None: not within `horizon_s`, or holding)."""

    name: str
    inside: bool
    to_edge_m: float
    enters_s: float | None
    horizon_s: float
    radius_m: float | None = None  # a circle's
    center: tuple[float, float] | None = None


@dataclass
class Standing:
    """Where a contact stands against the watch, and whether that is a stake."""

    where: tuple[float, float]
    velocity: tuple[float, float] | None
    lines: list[Line]
    approaches: list[Any]  # playbook.beliefs.Approach, nearest CPA first
    closing: Any  # the protected point it closes on (closing_alert), or None
    matters: bool
    why: str

    def as_dict(self) -> dict[str, Any]:
        return {
            "where": [round(self.where[0], 6), round(self.where[1], 6)], "matters": self.matters, "why": self.why,
            "lines": [{"area": ln.name, "inside": ln.inside, "to_edge_m": round(ln.to_edge_m),
                       "enters_s": None if ln.enters_s is None else round(ln.enters_s)} for ln in self.lines],
            "protected": [{"point": a.name, "distance_m": round(a.distance_m), "cpa_m": round(a.cpa_m),
                           "tcpa_s": None if a.tcpa_s is None else round(a.tcpa_s)} for a in self.approaches],
        }


@dataclass
class Entry:
    """A contact against the watch areas: `stage` inside or approaching, the area, metres to its
    edge (negative inside), seconds to entry, and what the prediction rests on."""

    stage: str
    area: str
    to_edge_m: float
    enters_s: float | None
    basis: str


def km(m: float) -> str:
    return f"{m / 1000.0:.1f} km" if abs(m) >= 1000 else f"{m:.0f} m"


@dataclass
class Watch:
    """The watch areas and protected points of a situation (a playbook `Config`), and the
    playbook's thresholds for judging a contact against them."""

    cfg: Any  # playbook.config.Config
    source: str = "the situation's [playbook] table"

    @property
    def known(self) -> bool:
        return bool(self.cfg["watch_area"] or self.cfg["protected"])

    @property
    def horizon_s(self) -> float:
        return float(self.cfg.t["tcpa_max_s"])

    def describe(self) -> dict[str, Any]:
        """What the run's log says the gate reads."""
        t = self.cfg.t
        return {"source": self.source, "watch_areas": [a["name"] for a in self.cfg["watch_area"]],
                "protected": [p["name"] for p in self.cfg["protected"]], "cpa_alert_m": t["cpa_alert_m"],
                "tcpa_max_s": t["tcpa_max_s"]}

    def _line(self, a: dict[str, Any], where: tuple[float, float], vel: tuple[float, float] | None,
              horizon_s: float | None = None) -> Line:
        h = self.horizon_s if horizon_s is None else float(horizon_s)
        moving = vel is not None and math.hypot(*vel) >= float(self.cfg.t["holding_mps"])
        if "polygon" in a:
            ring = [(float(p[0]), float(p[1])) for p in a["polygon"]]
            inside = in_polygon(where, [list(p) for p in ring])
            edge = min(_seg_m(where, ring[i], ring[(i + 1) % len(ring)]) for i in range(len(ring)))
            enters = 0.0 if inside else None
            if not inside and moving:
                k = 1
                while k * STEP_S <= h + 1e-9:
                    if in_polygon(carried(where, vel, k * STEP_S), [list(p) for p in ring]):
                        enters = k * STEP_S
                        break
                    k += 1
            return Line(a["name"], inside, -edge if inside else edge, enters, h)
        c = (float(a["center"][0]), float(a["center"][1]))
        r = float(a["radius_m"])
        d = haversine_m(where, c)
        inside = d <= r
        enters = 0.0 if inside else None
        if not inside and moving:
            x, y = enu(where, c)
            vx, vy = vel
            aa, bb, cc = vx * vx + vy * vy, 2 * (x * vx + y * vy), x * x + y * y - r * r
            disc = bb * bb - 4 * aa * cc
            if aa > 0 and disc >= 0:
                t1 = (-bb - math.sqrt(disc)) / (2 * aa)
                if 0 <= t1 <= h:
                    enters = t1
        return Line(a["name"], inside, d - r, enters, h, radius_m=r, center=c)

    def standing(self, where: tuple[float, float], vel: tuple[float, float] | None,
                 horizon_s: float | None = None) -> Standing:
        """A contact at `where` with velocity `vel` (east, north m/s) against the watch. Each watch
        area's way in is looked for within `horizon_s` (default the playbook's `tcpa_max_s`, the
        gate's horizon; the case file looks further to say when)."""
        lines = [self._line(a, where, vel, horizon_s) for a in self.cfg["watch_area"]]
        interest = interest_of(_Where(where, vel), self.cfg)  # type: ignore[arg-type]
        closing = closing_alert(interest, self.cfg)
        h = self.horizon_s if horizon_s is None else float(horizon_s)
        alert = float(self.cfg.t["cpa_alert_m"])
        inside = [ln for ln in lines if ln.inside]
        coming = sorted((ln for ln in lines if not ln.inside and ln.enters_s is not None), key=lambda ln: ln.enters_s)
        if inside:
            why = f"inside {inside[0].name}"
        elif closing is not None:
            when = "" if closing.tcpa_s is None or closing.distance_m <= alert else f" in {closing.tcpa_s:.0f} s"
            why = f"closing on {closing.name}: closest approach {km(closing.cpa_m)}{when}"
        elif coming:
            why = f"coming into {coming[0].name} in {coming[0].enters_s:.0f} s on its course"
        else:
            bits = [f"{km(ln.to_edge_m)} outside {ln.name}" for ln in lines]
            near = interest.approaches[0] if interest.approaches else None
            if near is not None:
                bits.append(f"closest approach {km(near.cpa_m)} from {near.name}")
            how = "holding" if vel is None or math.hypot(*vel) < float(self.cfg.t["holding_mps"]) else "on its course"
            why = (", ".join(bits) or "no watch area") + f", not coming within {km(alert)} or into a watch area " \
                f"within {h:.0f} s {how}"
        return Standing(where, vel, lines, interest.approaches, closing, bool(inside or closing or coming), why)

    def of_track(self, t: dict[str, Any]) -> Standing | None:
        """A `get_tracks` entry's standing (its position carried to now, its velocity), or None
        without a position."""
        where = _point(t.get("predicted")) or _point(t.get("position"))
        if where is None:
            return None
        v = t.get("velocity_mps")
        vel = (float(v[0]), float(v[1])) if _point(v) is not None else velocity_of(t.get("speed_mps"), t.get("course_deg"))
        return self.standing(where, vel)

    def of_change(self, c: dict[str, Any], now: float) -> Standing | None:
        """A change's contact (the feed's track summary: its last fix, speed, course and
        `as_of`), carried along its course to `now`; None without a position."""
        p = _point(c.get("position"))
        if p is None:
            return None
        vel = velocity_of(c.get("speed_mps"), c.get("course_deg"))
        as_of = _num(c.get("as_of"))
        return self.standing(carried(p, vel, (now - as_of) if as_of is not None else 0.0), vel)

    def entry(self, where: tuple[float, float], vel: tuple[float, float] | None, domain: Any,
              horizon_s: float = APPROACH_HORIZON_S) -> Entry | None:
        """A contact in doubt against the watch areas alone (bd murmur-1h2r): inside one now, or
        coming into one within `horizon_s` (the earliest), else None. No velocity in the air is
        taken straight in at `AIR_NO_VELOCITY_MPS` (towards the area's centre); no velocity in
        another domain is judged where it is."""
        best: Entry | None = None
        for a in self.cfg["watch_area"]:
            v, basis = vel, "its velocity"
            if v is None and domain == "air":
                if "polygon" in a:
                    ring = a["polygon"]
                    tgt = (sum(float(q[0]) for q in ring) / len(ring), sum(float(q[1]) for q in ring) / len(ring))
                else:
                    tgt = (float(a["center"][0]), float(a["center"][1]))
                x, y = enu(tgt, where)
                n = math.hypot(x, y)
                v = (AIR_NO_VELOCITY_MPS * x / n, AIR_NO_VELOCITY_MPS * y / n) if n > 0 else None
                basis = f"no velocity yet: at {AIR_NO_VELOCITY_MPS:.0f} m/s straight in"
            elif v is None:
                basis = "no velocity yet: judged where it is"
            ln = self._line(a, where, v, horizon_s)
            if ln.inside:
                e = Entry("inside", ln.name, ln.to_edge_m, 0.0, basis)
            elif ln.enters_s is not None:
                e = Entry("approaching", ln.name, ln.to_edge_m, ln.enters_s, basis)
            else:
                continue
            if best is None or (e.stage == "inside" and best.stage != "inside") or (
                    e.stage == best.stage and (e.enters_s or 0.0) < (best.enters_s or 0.0)):
                best = e
        return best

    def entry_of_track(self, t: dict[str, Any]) -> Entry | None:
        """`entry` for a `get_tracks` entry (its position carried to now), None without a position."""
        where = _point(t.get("predicted")) or _point(t.get("position"))
        if where is None:
            return None
        v = t.get("velocity_mps")
        vel = (float(v[0]), float(v[1])) if _point(v) is not None else velocity_of(t.get("speed_mps"), t.get("course_deg"))
        return self.entry(where, vel, t.get("domain"))

    def gate(self, c: dict[str, Any], now: float) -> tuple[bool, str | None]:
        """(escalates, why) for a change the table escalates: True for a change the gate does
        not judge, or whose contact matters, or that it cannot judge (no position); False, with
        why, for a contact that is far and not closing."""
        if not gated(c):
            return True, None
        try:
            s = self.of_change(c, now)
        except Exception:  # noqa: BLE001 (the gate fails toward escalating)
            return True, None
        if s is None:
            return True, None
        if s.matters:
            return True, None
        return False, f"track {c.get('track')}: {s.why}: a routine wake, not the senior consult"


def watch_of(cfg: Any, source: str | None = None) -> Watch | None:
    """The gate for a playbook config, or None when it knows no watch area or protected point
    (every escalation then goes through, as before)."""
    if cfg is None:
        return None
    w = Watch(cfg, source or "the situation's [playbook] table")
    return w if w.known else None
