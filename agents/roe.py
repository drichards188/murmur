"""The scenario's rules for the watch as an agent reads them (docs/SCENARIO-SETTINGS.md section 3):
pure predicates over a track as MCP `get_tracks` returns it and the scenario's `[watch]` table
(MCP `get_policy`'s `watch`; eval/headless/v3/settings.py `Settings.watch()` is the scoring's
copy). No bridge; the one file it reads is v0's (`Roe.of_policy`). Read since stage S5 by the
officer (agents/watch/picture.py `Poller.approach`: who is in doubt; stakes.py `Watch.entry`: the
horizon, the still speed and the no-velocity speeds; stakes.py `watch_of`: the watch line from
`area`), its case file (casefile.py: `unsettled`, `look_m`, `stale_s`) and the playbook
(playbook/config.py `under`: `stale_s`, the approach horizon, the area where its table has none;
since stage S7 its "settled" is `identified`: playbook/plays/isr.py `look_reasons` and
`custody_held`, playbook/ledger.py `see_classification`).

- `Roe.of_policy(policy)`: the run's settings from MCP `get_policy`'s answer: its top-level
  `watch` (the effective settings the run decides under, from decision rules 33), else the frozen
  v0 settings, sim/policy-v0-watch.toml's `[watch]` (read here and nowhere else in agents/), which
  every run before rules 33 was briefed and graded by. A `watch` that is not a table, or lacks a
  value the agents read, is refused (ValueError), never replaced by v0's.
- `criteria(domain)`: the domain's level, sensors and grace (`[watch.domain.<domain>]`); None for
  a domain the table does not name.
- `identified(track)`: the identity shows a kind at the domain's level or above (`identify`; the
  subsurface's `recognise` takes a class or a kind), one of the domain's sensors is in
  `supported_by` (AIS counts on the surface: the picture cannot tell a true broadcast from a
  spoof, so the scoring's "its own true AIS" is the truth side's), its confidence is at least the
  bar, and it has no conflict. As picture.py `resolved` and the picture side of
  eval/headless/v3/labels.py `resolved_at` judge it today, but for one row: an identity without a
  `supported_by` list is not identified here (the scoring reads it as no support; `resolved` let
  it through). `shortfall(track)` says why not, in words (None when identified).
- `in_doubt(track)`: not identified, or shown with an affiliation other than neutral,
  assumed_friend or friend: a contact leaves doubt only identified and shown clear (section 3).
  As eval/headless/v3/labels.py `in_doubt` holds it (a contact shown unknown, suspect or hostile
  stays in doubt however well identified), with the identification's level and sensor besides.
- `entry(track, area)` / `approaches(...)`: the contact inside the circle now, or brought inside
  within the horizon (the table's `approach_horizon_s` unless given) on its shown velocity; one
  shown without velocity (none, or slower than `still_mps`) is taken straight in at its domain's
  `no_velocity_mps` (the air's: a small drone's speed), or judged where it is in a domain without
  one. The geometry is the officer's (stakes.py `Watch._line` for a circle: haversine inside, the
  course's crossing of the circle on the local plane).

Two intended differences from the officer's code before S5. Its clear list, `("friendly",
"neutral")`, named an affiliation the bridge never sends (`friendly`) and left out one it does
(`assumed_friend`: a Coast Guard cutter on AIS); here neutral, assumed_friend and friend are
clear. And it let a contact go when it was shown neutral or identified, either one; the tracker
calls a fishing boat neutral at 0.3 identity confidence, before the bar (section 1), so here a
contact leaves doubt only when it is both.
"""

from __future__ import annotations

import copy
import functools
import math
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Any

#: the affiliations a contact may be shown with and be out of doubt once identified (the bridge
#: sends neutral and assumed_friend for tracks; friend is ours)
CLEAR = ("neutral", "assumed_friend", "friend")
#: identity levels, least specific first (the track feed's `identity.level`)
LEVELS = ("detect", "recognise", "identify")
#: the bridge's figure for carrying a track along its velocity (MCP `get_tracks`' `predicted`)
M_PER_DEG = 111_320.0
EARTH_R = 6_371_000.0
#: the frozen settings of every run before decision rules 33 (docs/SCENARIO-SETTINGS.md section 3,
#: "Old results stay honest"): what an agent runs under when the bridge's `get_policy` has no `watch`
V0_FILE = Path(__file__).resolve().parents[1] / "sim" / "policy-v0-watch.toml"
#: where a `Roe`'s settings came from (`Roe.source`)
FROM_POLICY = "get_policy"
FROM_V0 = "v0"


@functools.lru_cache(maxsize=1)
def _v0_watch() -> dict[str, Any]:
    return tomllib.loads(V0_FILE.read_text())["watch"]


def v0_watch() -> dict[str, Any]:
    """v0's `[watch]` table (sim/policy-v0-watch.toml), a fresh copy each call."""
    return copy.deepcopy(_v0_watch())


@dataclass(frozen=True)
class Criteria:
    """What identifies a contact of one domain, and by when."""

    level: str
    sensors: tuple[str, ...]
    grace_s: float


def _num(v: Any) -> float | None:
    return float(v) if isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v) else None


def _pair(p: Any) -> tuple[float, float] | None:
    if isinstance(p, (list, tuple)) and len(p) == 2:
        a, b = _num(p[0]), _num(p[1])
        if a is not None and b is not None:
            return a, b
    return None


def haversine_m(a: tuple[float, float], b: tuple[float, float]) -> float:
    p1, p2 = math.radians(a[1]), math.radians(b[1])
    x = math.sin((p2 - p1) / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(math.radians(b[0] - a[0]) / 2) ** 2
    return 2 * EARTH_R * math.asin(math.sqrt(min(1.0, x)))


def enu(p: tuple[float, float], ref: tuple[float, float]) -> tuple[float, float]:
    """`p` in metres east and north of `ref` (a local plane)."""
    return ((p[0] - ref[0]) * M_PER_DEG * math.cos(math.radians(ref[1])), (p[1] - ref[1]) * M_PER_DEG)


def velocity(track: dict[str, Any]) -> tuple[float, float] | None:
    """A track's shown velocity (east, north m/s): `velocity_mps`, else its speed and course;
    None when it shows none (the feed's speed is 0.0 then, with no course)."""
    v = _pair(track.get("velocity_mps"))
    if v is not None:
        return v
    sp, co = _num(track.get("speed_mps")), _num(track.get("course_deg"))
    if sp is None or co is None:
        return None
    return sp * math.sin(math.radians(co)), sp * math.cos(math.radians(co))


def position(track: dict[str, Any], now: float | None = None) -> tuple[float, float] | None:
    """Where a track is: its last fix carried along its shown velocity from `last_seen_t` to `now`
    (the bridge's own carry), or without `now` the `predicted` position `get_tracks` gives (else its
    last fix)."""
    p = _pair(track.get("position"))
    if now is None or p is None:
        return _pair(track.get("predicted")) or p
    v = velocity(track)
    last = _num(track.get("last_seen_t"))
    dt = now - last if last is not None else 0.0
    if v is None or dt == 0:
        return p
    return p[0] + v[0] * dt / (M_PER_DEG * math.cos(math.radians(p[1]))), p[1] + v[1] * dt / M_PER_DEG


def _area(a: Any) -> dict[str, Any] | None:
    """The scenario's circle (`[watch.area]`: name, center [lon, lat], radius_m), or None."""
    if a is None:
        return None
    c = _pair(a.get("center")) if isinstance(a, dict) else None
    r = _num(a.get("radius_m")) if isinstance(a, dict) else None
    if c is None or r is None or r <= 0:
        raise ValueError(f"watch.area needs center = [lon, lat] and radius_m > 0, not {a!r}")
    return {"name": str(a.get("name") or "the watch area"), "center": [c[0], c[1]], "radius_m": r}


class Roe:
    """The predicates over one scenario's `[watch]` table, and the values the agents read from it.
    `source`: where the table came from (`FROM_POLICY`, `FROM_V0`)."""

    def __init__(self, watch: dict[str, Any], source: str = FROM_POLICY):
        if not isinstance(watch, dict):
            raise ValueError(f"the watch settings are a table, not {watch!r}")
        self.watch = watch
        self.source = source
        try:
            self.bar = float(watch["bar"])
            self.horizon_s = float(watch["approach_horizon_s"])
            self.still_mps = float(watch["still_mps"])
            #: an orbit centred this near a contact (or within its radius) is a look on it
            self.look_m = float(watch["look_m"])
            #: the tracker's stale age: a sensor's sighting this recent holds the track now
            self.stale_s = float(watch["stale_s"])
            self.no_velocity_mps = {str(k): float(v) for k, v in (watch.get("no_velocity_mps") or {}).items()}
            self._criteria = {str(d): Criteria(str(c["level"]), tuple(str(s) for s in c["sensors"]),
                                               float(c["grace_s"]))
                              for d, c in (watch.get("domain") or {}).items()}
        except (KeyError, TypeError, ValueError, AttributeError) as e:
            raise ValueError(f"the watch settings lack or garble a value the agents read ({type(e).__name__}: "
                             f"{e})") from e
        #: the scenario's circle, when its policy sets one (`[watch.area]`), else None
        self.area = _area(watch.get("area"))

    @classmethod
    def of_policy(cls, policy: Any) -> "Roe":
        """The run's settings from MCP `get_policy`'s answer: its `watch`, else v0's."""
        watch = policy.get("watch") if isinstance(policy, dict) else None
        return v0() if watch is None else cls(watch, FROM_POLICY)

    def describe(self) -> dict[str, Any]:
        """For a log: where the settings came from, and the scenario's circle."""
        return {"source": ("get_policy's watch" if self.source == FROM_POLICY else
                           f"v0 (sim/{V0_FILE.name}: the bridge's get_policy has no watch)"), "area": self.area}

    def criteria(self, domain: Any) -> Criteria | None:
        return self._criteria.get(str(domain)) if domain is not None else None

    def identified(self, track: dict[str, Any]) -> bool:
        return self.shortfall(track) is None

    def shortfall(self, track: dict[str, Any], conflict: bool = True) -> str | None:
        """Why `track` is not identified under these settings, in words (None: it is): the first
        of its confidence under the bar, a conflict, its level short of its domain's, and none of
        its domain's sensors supporting it, as `identified` judges them. `conflict=False` leaves a
        conflict out (for a reader to which a conflict is a reason of its own: the playbook's look)."""
        ident = track.get("identity") or {}
        conf = _num(ident.get("confidence"))
        if conf is None:
            return "no identity"
        if conf < self.bar:
            return f"{conf:.2f} under the bar {self.bar:.2f}"
        if conflict and ident.get("conflict"):
            return "its identity in conflict"
        c = self.criteria(track.get("domain"))
        if c is None:
            return f"no criteria for its domain ({track.get('domain')})"
        level = ident.get("level")
        if level not in LEVELS or LEVELS.index(level) < LEVELS.index(c.level):
            return f"a {level or 'no'} level, short of {c.level}"
        support = ident.get("supported_by")
        if not (isinstance(support, list) and set(support) & set(c.sensors)):
            by = ", ".join(str(s) for s in support) if isinstance(support, list) and support else "no sensor"
            return f"supported by {by}, not by {' or '.join(c.sensors)}"
        return None

    def in_doubt(self, track: dict[str, Any]) -> bool:
        return not self.identified(track) or track.get("affiliation") not in CLEAR

    def still(self, v: tuple[float, float] | None) -> bool:
        """Shown without velocity: none, or slower than `still_mps`."""
        return v is None or math.hypot(*v) < self.still_mps

    def entry(self, track: dict[str, Any], area: dict[str, Any], horizon_s: float | None = None,
              now: float | None = None) -> tuple[float, str] | None:
        """(seconds until it is inside, what that rests on) for a contact inside the circle `area`
        ({center: [lon, lat], radius_m}) now (0) or brought inside within the horizon; None
        otherwise, or without a position."""
        where = position(track, now)
        if where is None:
            return None
        h = self.horizon_s if horizon_s is None else float(horizon_s)
        c = (float(area["center"][0]), float(area["center"][1]))
        r = float(area["radius_m"])
        d = haversine_m(where, c)
        v = velocity(track)
        if self.still(v):
            speed = self.no_velocity_mps.get(str(track.get("domain")))
            if speed is None:
                return (0.0, "no velocity: judged where it is") if d <= r else None
            t = max(0.0, d - r) / speed
            return (t, f"no velocity: at {speed:g} m/s straight in") if t <= h + 1e-9 else None
        if d <= r:
            return 0.0, "its velocity"
        assert v is not None
        x, y = enu(where, c)
        vx, vy = v
        aa, bb, cc = vx * vx + vy * vy, 2 * (x * vx + y * vy), x * x + y * y - r * r
        disc = bb * bb - 4 * aa * cc
        if disc < 0:
            return None
        t1 = (-bb - math.sqrt(disc)) / (2 * aa)
        return (t1, "its velocity") if 0 <= t1 <= h else None

    def approaches(self, track: dict[str, Any], area: dict[str, Any], horizon_s: float | None = None,
                   now: float | None = None) -> bool:
        return self.entry(track, area, horizon_s, now) is not None


@functools.lru_cache(maxsize=1)
def v0() -> Roe:
    """The predicates over v0's `[watch]` (sim/policy-v0-watch.toml)."""
    return Roe(_v0_watch(), FROM_V0)
