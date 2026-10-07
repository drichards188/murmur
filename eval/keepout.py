"""Keep-outs against engagements, on generated situations (rules 6, 9 and 12): how often a
keep-out blocks the nomination of an engagement on a hostile target, as a function of traffic
density and behaviour, time to effect (the platform's speed and distance), how the tracks are
made and the rules version; whether a block protected someone; and which keep-out component
blocked. MODEL RESULTS FROM SIMULATED GEOMETRY, NEVER HUMAN DATA.

**What runs.** For each variant (`VARIANTS`), seed, density and behaviour, `situation` draws a
world from the stated distributions below (seeded, and paired: the same seed, density and index
give the same target, contact positions, our vehicles and platform placements under every
behaviour, only the motion differs): a hostile target holding in the engagement zone
ez-gate-west, declared hostile by an operator; protected contacts (neutral or unknown, none
declared) over a disc of `FIELD_M` around it, as many as a Poisson draw of the density gives,
uniform or (the lanes layout) mostly in two lanes passing near it; our observing USV; sometimes
one of ours lost; and the engaging platform placed for each nominal time to effect (`TTE_S`)
and at each distance (`DIST_M`). Each contact moves by its behaviour (`MOTION`). What the
engine reads about it comes from one of two trackers:

- `radar`, `all` and `ais`: its last ten minutes played second by second through the bridge's
  own sensing model and IMM tracker (eval/keepout_probe compiles
  crates/murmur-bridge/src/tracks/{model,sensing,truth}.rs unchanged), the observing USV's
  catalog sensors: its radar alone against contacts without AIS (`radar`, the pessimistic
  sensor case), every sensor it carries (radar, EO/IR, sonar) against contacts without AIS
  (`all`), or every sensor against contacts that all broadcast AIS (`ais`, the best case). The target's track is
  the generator's in every variant (a good track, as an engagement needs).
- `model`: a stated model of the tracker's reports with the scenario runner's noise (position
  off by the track's own 1-sigma, velocity off by `vel_sigma` per axis, no velocity in the
  first `VEL_KNOWN_S`, an average velocity once tracked `MEAN_MIN_S`, a manoeuvre when the true
  velocity changed beyond the bridge's threshold within its memory).

`murmur-keepout-probe` (read-only use of murmur-decide) runs every evaluation through the
engine's own nomination checks (`rules::evaluate` on the engine's view of the world) under
rules 6, 9 and 12, and lists each protected track's keep-out regions (`engage::track_reach`)
with how far each reaches into the keep-out.

**Labels (truth).** From the effect point and time the engine predicted, each contact's true
position then. A block is *needed* when a protected contact is truly inside the keep-out (the
effect radius plus the policy's 200 m) at the effect time, the policy's own line; it is
*severe* when one is inside the effect circle. Otherwise it is *conservative*. A *missed*
danger is a pass with a protected contact inside the keep-out (or, severe, the circle). The
same labels over the release window (`late_s`, 60 s: a holding target may be released that
late) count anyone inside at any time from the effect time to 60 s after.

**What it is not.** The situations come from stated distributions, not from the world: open
water with no land or channels, surface contacts (one variant adds a loitering light aircraft),
one observing vehicle, every contact within its sensors' reach (so the bridge's drop of a track
unseen for 180 s never fires, except in the out-of-view variants below; a contact never detected
has no track and is not protected). The checks are
the nomination's only (the plan's re-checks while signatures wait, and the release's own
check, are not run): the nomination check is a screen, not the final outcome. No person
decides anything here.

    python -m eval.run keepout --seeds 1 2 3 4 5 6 7 8 9 10 --situations 50

**Out of view (rules 13, murmur-533).** The `oov` variants send a share of the contacts out of
sensor view (`masked_from_s`: from a uniform time before the nomination nothing detects them,
whatever the range: a radar shadow, clutter, a dark small boat) while they keep moving as their
behaviour says; the bridge's tracker drops a track unseen for 180 s. They compare, on the same
evaluations, what each policy for a dropped contact would decide (`LOST_POLICIES`: today's drop,
rules 13's lost contact held until accepted, the same by a live track's regions alone (along
its course: the first version of rules 13), held only 600 s after its drop, counted only near
the effect point, and every lost contact accepted or cleared), how often a nomination is blocked
and how often only an acceptance would let it pass, and the misses against the truth, those
caused by a dropped contact apart.
"""

from __future__ import annotations

import bisect
import gzip
import json
import math
import os
import random
import shutil
import subprocess
import time
import tomllib
from collections import defaultdict
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path
from typing import Any, Iterable, Iterator

from . import REPO
from .stats import t_interval, wilson

PROBE = REPO / "target" / "release" / "murmur-keepout-probe"
#: what the probe is built from (cargo decides whether it is current; this is for messages)
PROBE_PACKAGE = "murmur-keepout-probe"
NOT_HUMAN = (
    "Model results from simulated geometry: generated situations from stated distributions, "
    "tracks from the bridge's own tracker on simulated sensor fixes (or a stated model of it), "
    "every verdict the decision engine's own nomination checks. Not human data, not field data, "
    "and not reviewed."
)

# ---- the stated distributions -------------------------------------------------------------------

#: protected contacts per km^2 over a disc of FIELD_M around the target (0.03: about the 7
#: tracks of the 10-06 live run over the 8 km around the gate; 1: a busy harbour, a GUESS)
DENSITIES = (0.03, 0.1, 0.3, 1.0)
#: contacts are drawn this far from the target: every keep-out region of a span up to about
#: 310 s fits inside (a young track at 25 m/s, plus the keep-out); only evaluations whose
#: actual time to effect ran longer (a holding target's velocity estimate above the holding
#: speed) can lose a young region beyond it
FIELD_M = 8000.0
BEHAVIOURS = ("stationary", "transiting", "circling", "manoeuvring", "new", "mixed")
#: the mixed traffic's behaviour shares (a stated GUESS for a busy harbour); `new_share`
#: replaces the new share and scales the others
MIX = (("stationary", 0.30), ("transiting", 0.40), ("circling", 0.10), ("manoeuvring", 0.15), ("new", 0.05))
#: how each behaviour moves (truth), all uniform draws: m/s, m, s, degrees
MOTION = {
    "stationary": "holds still",
    "transiting": "a straight course, uniform; 3 to 12 m/s",
    "circling": "a circle of 100 to 600 m radius at 2 to 8 m/s, either way round",
    "manoeuvring": (
        "a zigzag (0.6: legs of 30 to 90 s alternating 30 to 60 degrees either side of a course, 3 to 10 m/s) "
        "or sprints and stops (0.4: 5 to 12 m/s for 30 to 90 s on a course turned up to 90 degrees each time, "
        "then 20 to 60 s stopped)"
    ),
    "new": "tracked for 0 to 45 s only; holds still (0.4) or transits as above (0.6)",
    "lane": "lanes layout: 70% of the contacts in two lanes, each passing 0 to 1.5 km from the target on a uniform course, "
    "100 m wide (1-sigma), transiting along it either way at 3 to 12 m/s; the rest as the behaviour says, uniform",
    "air": "a light aircraft loitering: circling 500 to 1500 m radius at 30 to 55 m/s, 150 to 1500 m up, centred within 4 km of the target "
    "(the air variant adds one to the surface traffic)",
}
#: nominal times to effect (s): the engaging platform starts (TTE - dispatch delay) x cruise away
TTE_S = (30, 45, 60, 90, 120, 180, 240, 300)
#: distances (m) for the platform table: each armed platform at each
DIST_M = (500, 1000, 2000, 3000)
PLATFORMS = ("magura", "switchblade")
RULES = (6, 9, 12)
RADIUS_M = 75.0
#: the policy's dispatch_delay_s, for placing the platform only (the engine uses the policy's own)
DISPATCH_S = 20.0
#: the policy's late_s: a holding target may be released up to this long after the plan's time
LATE_S = 60.0
#: the line the truth labels use: the effect radius plus the shipped policy's keep_out_m, in
#: every variant (a buffer variant changes what the engine checks, not who counts as at risk)
LABEL_BUFFER_M = 200.0
#: the generated model's track 1-sigma (m), uniform; its position estimate is off by that much
SIGMA_P = (8.0, 25.0)
#: the model's velocity error per axis (m/s), the scenario runner's --vnoise (Phase G: 0.5
#: passed the engagement suite)
VEL_SIGMA = 0.5
#: the bridge tracker's rules the model follows (crates/murmur-bridge/src/tracks/model.rs): a
#: velocity once its 1-sigma is below 3 m/s (a GUESS of 7 s of fixes), an average velocity
#: once tracked 45 s, a manoeuvre when the velocity changed by more than 2 m/s and its noise
#: (chi-square 13.8 on two estimates' variances) within 300 s, remembered 300 s
VEL_KNOWN_S = 7.0
MEAN_MIN_S = 45.0
MEAN_WINDOW_S = 60.0
MANOEUVRE_MIN_MPS = 2.0
MANOEUVRE_CHI2 = 13.8
MANOEUVRE_MEMORY_S = 300.0
VEL_SAMPLE_S = 10.0
#: an established track has been followed this long (s), uniform
TRACKED_S = (60.0, 1800.0)
NEW_TRACKED_S = (0.0, 45.0)
#: the real tracker plays at most this much of a contact's history (the manoeuvre memory, 300 s,
#: compares samples up to 300 s apart)
MAX_HISTORY_S = 600.0
#: the chance one of our vehicles is lost (its last report 15 to 300 s old, 2 to 8 km out)
LOST_SHARE = 0.1
#: where the target is: inside ez-gate-west, clear of the no-strike areas (eval/ident.py's box)
CENTRE_BOX = ((-122.520, -122.497), (37.806, 37.819))
#: engine time at the nomination (the scenario runner's T0)
T0_MS = 1_781_000_000_000
M_PER_DEG = 111_320.0
#: how far into the future and past the motions are laid out (s)
HORIZON_S = 900.0

#: rules 13 (murmur-533): in the out-of-view variants each contact goes out of sensor view with
#: this chance (`oov_share`), from a uniform time this long before the nomination (s); the
#: bridge drops a track unseen 180 s, so most are dropped by the nomination, the rest are stale
OOV_SHARE = 0.3
OOV_FROM_S = (30.0, 900.0)
#: the bridge's drop time (MURMUR_TRACK_DROP_S): a contact out of view this long (and a second)
#: before the nomination has been dropped
DROP_S = 180.0
#: what each out-of-view policy does with a contact the tracker dropped (the probe's
#: `lost_variants` under rules 13; `today` is rules 12)
LOST_POLICIES = {
    "today": "rules 12: a dropped track no longer counts in the keep-out",
    "held": "rules 13 as shipped: a lost contact counts from its last fix, no cap, until an operator accepts it: by a live track's regions and also anywhere at its own plausible speed (its speed and 2-sigma of its velocity error, or the fastest it went; without a velocity, its domain's top speed)",
    "heading": "held by a live track's regions alone (along its course and average course; anywhere only if it manoeuvred, was young or had no velocity): the first version of rules 13",
    "held600": "a lost contact counts only until 600 s after its drop",
    "near3km": "a lost contact counts only with its last fix within 3 km of the effect point",
    "accepted": "every lost contact accepted (or cleared by any rule: every one here is within the observer's nominal sensor coverage, so clearing covered water clears them all)",
}

#: the measurement's variants: each the base parameters changed as given. `radar` (one USV's
#: radar alone: the pessimistic sensor case) and `all` (every sensor it carries) are the bridge's
#: own tracker on contacts without AIS; `radar` carries the full tables; the rest are comparisons
#: and sensitivity.
ALL = list(BEHAVIOURS)


def _live(tracker: str, km: float, about: str) -> dict[str, Any]:
    return {
        "tracker": tracker,
        "behaviours": ["mixed"],
        "densities": [0.03, 0.1],
        "near_circler_km": km,
        "tte_s": [],
        "dist_m": [1000, 1500, 2000],
        "platforms": ["magura"],
        "about": about,
    }


VARIANTS: dict[str, dict[str, Any]] = {
    "radar": {"tracker": "radar", "behaviours": ALL, "about": "the bridge's tracker on the observing USV's radar alone, contacts without AIS (the pessimistic sensor case)"},
    "all": {"tracker": "all", "behaviours": ALL, "about": "the bridge's tracker on every sensor the observing USV carries (radar, EO/IR, sonar), contacts without AIS"},
    "model": {"tracker": "model", "behaviours": ALL, "about": "the stated tracker model (the first measurement's)"},
    "ais": {"tracker": "ais", "behaviours": ALL, "about": "the bridge's tracker on every sensor of the observing USV, every contact on AIS (best case)"},
    "lanes": {"tracker": "radar", "layout": "lanes", "behaviours": ["mixed"], "about": "radar tracker; 70% of contacts in two lanes passing 0 to 1.5 km from the target"},
    "lanes-model": {"tracker": "model", "layout": "lanes", "behaviours": ["mixed"], "about": "model tracker; the lanes layout"},
    "new0": {"tracker": "radar", "new_share": 0.0, "behaviours": ["mixed"], "about": "radar tracker; no new tracks in the mix"},
    "new15": {"tracker": "radar", "new_share": 0.15, "behaviours": ["mixed"], "about": "radar tracker; 15% new tracks in the mix"},
    "new25": {"tracker": "radar", "new_share": 0.25, "behaviours": ["mixed"], "about": "radar tracker; 25% new tracks in the mix"},
    "vel1": {"tracker": "model", "vel_sigma": 1.0, "behaviours": ["mixed"], "about": "model tracker; velocity error 1 m/s per axis"},
    "sigma60": {"tracker": "model", "sigma_p": [15.0, 60.0], "behaviours": ["mixed"], "about": "model tracker; track 1-sigma 15 to 60 m"},
    "buffer100": {"tracker": "radar", "keep_out_m": 100.0, "behaviours": ["mixed"], "about": "radar tracker; a copy of the policy with keep_out_m 100 (the shipped 200)"},
    "buffer50": {"tracker": "radar", "keep_out_m": 50.0, "behaviours": ["mixed"], "about": "radar tracker; a copy of the policy with keep_out_m 50"},
    "all-buffer100": {"tracker": "all", "keep_out_m": 100.0, "behaviours": ["mixed"], "about": "all-sensors tracker; keep_out_m 100"},
    "all-buffer50": {"tracker": "all", "keep_out_m": 50.0, "behaviours": ["mixed"], "about": "all-sensors tracker; keep_out_m 50"},
    "air": {"tracker": "radar", "behaviours": ["mixed"], "air_contacts": 1, "about": "radar tracker; mixed traffic plus one light aircraft loitering within 4 km (the worst case: no ADS-B, no height-finding radar)"},
    "air-straight": {"tracker": "radar", "behaviours": ["mixed"], "air_contacts": 1, "air_motion": "straight", "about": "radar tracker; mixed traffic plus one light aircraft flying straight past within 4 km"},
    "air-all": {"tracker": "all", "behaviours": ["mixed"], "air_contacts": 1, "about": "all-sensors tracker; mixed traffic plus one light aircraft loitering within 4 km"},
    "air-all-straight": {"tracker": "all", "behaviours": ["mixed"], "air_contacts": 1, "air_motion": "straight", "about": "all-sensors tracker; one light aircraft flying straight past within 4 km"},
    "live": _live("radar", 1.0, "the live run's question: a tour boat circling within 1 km of the target, mixed traffic around, a magura 1 to 2 km out; radar tracker"),
    "live-all": _live("all", 1.0, "as live, all-sensors tracker"),
    "live-ais": _live("ais", 1.0, "as live, every contact on AIS"),
    "live-model": _live("model", 1.0, "as live, the stated tracker model"),
    "live2": _live("radar", 2.0, "as live, the circling boat within 2 km; radar tracker"),
    "live2-all": _live("all", 2.0, "as live2, all-sensors tracker"),
    "live2-ais": _live("ais", 2.0, "as live2, every contact on AIS"),
    "live2-model": _live("model", 2.0, "as live2, the stated tracker model"),
    # rules 13 (murmur-533): contacts that go out of sensor view
    "oov": {
        "tracker": "radar",
        "behaviours": ["mixed"],
        "densities": [0.03, 0.1, 0.3],
        "oov_share": OOV_SHARE,
        "rules": [12, 13],
        "max_history_s": 1500.0,
        "about": "radar tracker, mixed traffic; 30% of the contacts go out of sensor view 30 to 900 s before the nomination (most dropped by then)",
    },
    "oov-all": {
        "tracker": "all",
        "behaviours": ["mixed"],
        "densities": [0.03, 0.1, 0.3],
        "oov_share": OOV_SHARE,
        "rules": [12, 13],
        "max_history_s": 1500.0,
        "about": "as oov, the all-sensors tracker",
    },
    "oov-near": {
        "tracker": "radar",
        "behaviours": ["mixed"],
        "densities": [0.03, 0.1],
        "near_oov_km": 1.0,
        "rules": [12, 13],
        "max_history_s": 1500.0,
        "about": "the review's case: one contact within 1 km of the target (mixed behaviour) goes out of sensor view 30 to 900 s before the nomination; mixed traffic around, all in view; radar tracker",
    },
}
PRIMARY = "radar"


def default_params() -> dict[str, Any]:
    return {
        "densities": list(DENSITIES),
        "behaviours": list(BEHAVIOURS),
        "tte_s": list(TTE_S),
        "dist_m": list(DIST_M),
        "platforms": list(PLATFORMS),
        "rules": list(RULES),
        "radius_m": RADIUS_M,
        "field_m": FIELD_M,
        "vel_sigma": VEL_SIGMA,
        "lost_share": LOST_SHARE,
        "mix": dict(MIX),
        "new_share": dict(MIX)["new"],
        "motion": MOTION,
        "sigma_p": list(SIGMA_P),
        "tracked_s": list(TRACKED_S),
        "new_tracked_s": list(NEW_TRACKED_S),
        "tracker": "model",
        "layout": "uniform",
        "near_circler_km": None,
        "max_history_s": MAX_HISTORY_S,
        "late_s": LATE_S,
        "stats_per_chunk": 3,
        "air_contacts": 0,
        "air_within_m": 4000.0,
        "air_motion": "loiter",
        "keep_out_m": None,
    }


def variant_params(name: str, overrides: dict[str, Any] | None = None) -> dict[str, Any]:
    v = {k: x for k, x in VARIANTS[name].items() if k != "about"}
    return {**default_params(), **v, **(overrides or {}), "variant": name, "about": VARIANTS[name]["about"]}


def cruise_speeds(root: Path = REPO) -> dict[str, float]:
    cat = tomllib.loads((root / "sim" / "platforms.toml").read_text())
    return {k: float(v["cruise_mps"]) for k, v in cat.items() if isinstance(v, dict) and "cruise_mps" in v}


def mix_for(new_share: float) -> list[tuple[str, float]]:
    """MIX with the new share replaced and the others scaled to fill the rest."""
    base_new = dict(MIX)["new"]
    return [(b, new_share if b == "new" else w * (1.0 - new_share) / (1.0 - base_new)) for b, w in MIX]


# ---- geometry: the engine's flat approximation around the target --------------------------------


def to_lonlat(origin: tuple[float, float], e: float, n: float) -> list[float]:
    """`origin` moved `e` east and `n` north (engage::offset)."""
    lon, lat = origin
    return [lon + e / (M_PER_DEG * math.cos(math.radians(lat))), lat + n / M_PER_DEG]


def to_enu(origin: tuple[float, float], p: list[float] | tuple[float, float]) -> tuple[float, float]:
    """East and north metres of `p` from `origin` (engage::enu)."""
    return ((p[0] - origin[0]) * M_PER_DEG * math.cos(math.radians(origin[1])), (p[1] - origin[1]) * M_PER_DEG)


# ---- motions (truth), in metres from the target at the nomination, t in s from it ----------------
# Each gives the probe its spec (eval/keepout_probe/src/realtrack.rs `Motion`, the same
# formulas), for the real tracker's history.


class Hold:
    def __init__(self, p: tuple[float, float]):
        self.p = p
        self.vmax = 0.0
        self.steady = True

    def pos(self, t: float) -> tuple[float, float]:
        return self.p

    def vel(self, t: float) -> tuple[float, float]:
        return (0.0, 0.0)

    def spec(self) -> dict[str, Any]:
        return {"kind": "hold", "p": list(self.p)}


class Line:
    def __init__(self, p: tuple[float, float], v: tuple[float, float]):
        self.p, self.v = p, v
        self.vmax = math.hypot(*v)
        self.steady = True

    def pos(self, t: float) -> tuple[float, float]:
        return (self.p[0] + self.v[0] * t, self.p[1] + self.v[1] * t)

    def vel(self, t: float) -> tuple[float, float]:
        return self.v

    def spec(self) -> dict[str, Any]:
        return {"kind": "line", "p": list(self.p), "v": list(self.v)}


class Circle:
    """Round `c` at `r` m, `speed` m/s, clockwise for `sense` +1; at bearing `phase` (rad,
    from north) from the centre at t = 0."""

    def __init__(self, c: tuple[float, float], r: float, speed: float, sense: int, phase: float):
        self.c, self.r, self.phase = c, r, phase
        self.w = sense * speed / r
        self.vmax = speed
        self.steady = False

    def pos(self, t: float) -> tuple[float, float]:
        a = self.phase + self.w * t
        return (self.c[0] + self.r * math.sin(a), self.c[1] + self.r * math.cos(a))

    def vel(self, t: float) -> tuple[float, float]:
        a = self.phase + self.w * t
        return (self.r * self.w * math.cos(a), -self.r * self.w * math.sin(a))

    def spec(self) -> dict[str, Any]:
        return {"kind": "circle", "c": list(self.c), "r": self.r, "w": self.w, "phase": self.phase}


class Legs:
    """Piecewise constant velocity: `legs` as (start s, east, north m/s), sorted, each until the
    next; at `p` at t = 0; before the first and after the last, the first's and last's."""

    def __init__(self, p: tuple[float, float], legs: list[tuple[float, float, float]]):
        self.starts = [s for s, _, _ in legs]
        self.v = [(e, n) for _, e, n in legs]
        acc = [(0.0, 0.0)]
        for k in range(1, len(legs)):
            dt = self.starts[k] - self.starts[k - 1]
            acc.append((acc[-1][0] + self.v[k - 1][0] * dt, acc[-1][1] + self.v[k - 1][1] * dt))
        self.acc = acc
        q = self._raw(0.0)
        self.shift = (p[0] - q[0], p[1] - q[1])
        self.vmax = max(math.hypot(*v) for v in self.v)
        self.steady = len({(round(e, 9), round(n, 9)) for e, n in self.v}) <= 1

    def _k(self, t: float) -> int:
        return max(0, bisect.bisect_right(self.starts, t) - 1)

    def _raw(self, t: float) -> tuple[float, float]:
        k = self._k(t)
        dt = t - self.starts[k]
        return (self.acc[k][0] + self.v[k][0] * dt, self.acc[k][1] + self.v[k][1] * dt)

    def pos(self, t: float) -> tuple[float, float]:
        q = self._raw(t)
        return (q[0] + self.shift[0], q[1] + self.shift[1])

    def vel(self, t: float) -> tuple[float, float]:
        return self.v[self._k(t)]

    def spec(self) -> dict[str, Any]:
        return {
            "kind": "legs",
            "starts": self.starts,
            "v": [list(v) for v in self.v],
            "at": [[a[0] + self.shift[0], a[1] + self.shift[1]] for a in self.acc],
        }


def _course(rng: random.Random) -> float:
    return rng.uniform(0.0, 2.0 * math.pi)


def _vec(speed: float, course: float) -> tuple[float, float]:
    return (speed * math.sin(course), speed * math.cos(course))


def motion(behaviour: str, p: tuple[float, float], rng: random.Random) -> tuple[Any, float]:
    """A contact at `p` now, moving as `behaviour` (MOTION), and how long it has been tracked."""
    if behaviour == "new":
        tracked = rng.uniform(*NEW_TRACKED_S)
        if rng.random() < 0.4:
            return Hold(p), tracked
        return Line(p, _vec(rng.uniform(3.0, 12.0), _course(rng))), tracked
    tracked = rng.uniform(*TRACKED_S)
    if behaviour == "stationary":
        return Hold(p), tracked
    if behaviour == "transiting":
        return Line(p, _vec(rng.uniform(3.0, 12.0), _course(rng))), tracked
    if behaviour == "circling":
        r, speed = rng.uniform(100.0, 600.0), rng.uniform(2.0, 8.0)
        sense = 1 if rng.random() < 0.5 else -1
        phase = _course(rng)
        c = (p[0] - r * math.sin(phase), p[1] - r * math.cos(phase))
        return Circle(c, r, speed, sense, phase), tracked
    if behaviour == "manoeuvring":
        legs: list[tuple[float, float, float]] = []
        if rng.random() < 0.6:
            # zigzag about a course
            base, half = _course(rng), math.radians(rng.uniform(30.0, 60.0))
            speed, leg = rng.uniform(3.0, 10.0), rng.uniform(30.0, 90.0)
            t, k = -HORIZON_S - rng.uniform(0.0, 2.0 * leg), 0
            while t < HORIZON_S:
                legs.append((t, *_vec(speed, base + (half if k % 2 == 0 else -half))))
                t, k = t + leg, k + 1
        else:
            # sprints and stops, turning up to 90 degrees each sprint
            course = _course(rng)
            t = -HORIZON_S - rng.uniform(0.0, 150.0)
            while t < HORIZON_S:
                speed = rng.uniform(5.0, 12.0)
                legs.append((t, *_vec(speed, course)))
                t += rng.uniform(30.0, 90.0)
                legs.append((t, 0.0, 0.0))
                t += rng.uniform(20.0, 60.0)
                course += math.radians(rng.uniform(-90.0, 90.0))
        return Legs(p, legs), tracked
    raise ValueError(f"unknown behaviour {behaviour!r}")


# ---- what the tracker reports (the stated model) ------------------------------------------------


def manoeuvre_seen(m: Any, tracked: float, vel_sigma: float) -> tuple[float, float, float] | None:
    """The bridge's manoeuvre evidence at t = 0 from the true velocity sampled every 10 s since
    the track began (at most 600 s back): a sample within the memory whose velocity differs from
    one up to 300 s before it by more than the threshold; (when last, s; the largest change;
    the fastest within the memory plus two sigmas). Noise-driven flags are not modelled."""
    if m.steady or tracked < VEL_SAMPLE_S:
        return None
    thr = max(MANOEUVRE_MIN_MPS, math.sqrt(MANOEUVRE_CHI2 * 2.0) * vel_sigma)
    back = min(tracked, MANOEUVRE_MEMORY_S * 2.0)
    ts = [-VEL_SAMPLE_S * k for k in range(int(back // VEL_SAMPLE_S) + 1)]
    vs = [m.vel(t) for t in ts]
    last, change = None, 0.0
    for i, ti in enumerate(ts):
        if ti < -MANOEUVRE_MEMORY_S:
            break
        for j in range(i + 1, len(ts)):
            if ti - ts[j] > MANOEUVRE_MEMORY_S:
                break
            dv = math.hypot(vs[i][0] - vs[j][0], vs[i][1] - vs[j][1])
            if dv > thr:
                last = ti if last is None else max(last, ti)
                change = max(change, dv)
    if last is None:
        return None
    fastest = max(math.hypot(*v) for t, v in zip(ts, vs) if t >= -MANOEUVRE_MEMORY_S)
    return last, change, fastest + 2.0 * vel_sigma


def facts(
    m: Any,
    tracked: float,
    sigma: float,
    vel_sigma: float,
    rng: random.Random,
    origin: tuple[float, float],
    affiliation: str,
    affiliation_confidence: float,
    domain: str = "surface",
) -> dict[str, Any]:
    """The facts the stated model reports for a contact moving as `m` (murmur-decide's
    TrackFacts)."""
    t_ms = T0_MS
    p = m.pos(0.0)
    err = (rng.gauss(0.0, sigma), rng.gauss(0.0, sigma))
    est = (p[0] + err[0], p[1] + err[1])
    f: dict[str, Any] = {
        "position": {"value": to_lonlat(origin, *est), "t": t_ms},
        "pos_sigma_m": round(sigma, 2),
        "confidence": 0.9,
        "affiliation": {"value": affiliation, "t": t_ms},
        "affiliation_confidence": round(affiliation_confidence, 2),
        "identity_confidence": 0.2,
        "sources": ["usv-1:radar"],
        "domain": domain,
        "last_seen_t": t_ms,
        "first_seen_t": t_ms - int(round(tracked * 1000.0)),
    }
    if tracked < VEL_KNOWN_S:
        f["sigma_growth_mps"] = 1.0
        return f
    v = m.vel(0.0)
    vel = [round(v[0] + rng.gauss(0.0, vel_sigma), 3), round(v[1] + rng.gauss(0.0, vel_sigma), 3)]
    f["velocity_mps"] = vel
    f["sigma_growth_mps"] = round(max(1.0, 0.3 * math.hypot(*vel)), 3)
    if tracked >= MEAN_MIN_S:
        span = min(MEAN_WINDOW_S, tracked)
        q = m.pos(-span)
        past = (q[0] + rng.gauss(0.0, sigma), q[1] + rng.gauss(0.0, sigma))
        f["mean_velocity_mps"] = [round((est[0] - past[0]) / span, 2), round((est[1] - past[1]) / span, 2)]
    seen = manoeuvre_seen(m, tracked, vel_sigma)
    if seen:
        last, change, speed = seen
        f["manoeuvre"] = {"t": t_ms + int(round(last * 1000.0)), "change_mps": round(change, 2), "speed_mps": round(speed, 2)}
    return f


# ---- one situation ----------------------------------------------------------------------------------


def _poisson(rng: random.Random, mean: float) -> int:
    """A Poisson draw (arrivals of a unit-rate process within `mean`)."""
    k, t = 0, rng.expovariate(1.0) if mean > 0 else math.inf
    while t <= mean:
        k += 1
        t += rng.expovariate(1.0)
    return k


def _disc(rng: random.Random, r: float) -> tuple[float, float]:
    d, a = r * math.sqrt(rng.random()), _course(rng)
    return (d * math.sin(a), d * math.cos(a))


def _disc_ring(rng: random.Random, lo: float, hi: float) -> tuple[float, float]:
    d, a = rng.uniform(lo, hi), _course(rng)
    return (d * math.sin(a), d * math.cos(a))


def _lanes(rng: random.Random) -> list[tuple[tuple[float, float], tuple[float, float]]]:
    """Two lanes: each (a point on it nearest the target, its direction), passing 0 to 1.5 km
    from the target on a uniform course."""
    out = []
    for _ in range(2):
        course, off = _course(rng), rng.uniform(0.0, 1500.0)
        d = (math.sin(course), math.cos(course))
        side = 1.0 if rng.random() < 0.5 else -1.0
        out.append(((side * off * d[1], -side * off * d[0]), d))
    return out


def _lane_contact(rng: random.Random, lane: tuple[tuple[float, float], tuple[float, float]], field_m: float) -> tuple[Any, float]:
    (q, d) = lane
    off = math.hypot(*q)
    half = math.sqrt(max(field_m * field_m - off * off, 0.0))
    s, lateral = rng.uniform(-half, half), rng.gauss(0.0, 100.0)
    p = (q[0] + s * d[0] + lateral * d[1], q[1] + s * d[1] - lateral * d[0])
    speed = rng.uniform(3.0, 12.0) * (1.0 if rng.random() < 0.5 else -1.0)
    return Line(p, (speed * d[0], speed * d[1])), rng.uniform(*TRACKED_S)


def situation(seed: int, density: float, index: int, behaviour: str, params: dict[str, Any], cruise: dict[str, float]) -> dict[str, Any]:
    """One situation: the probe's input (`probe`) and the truth behind it (`truth`). Paired:
    everything but the contacts' motion (and the draws that follow it) comes from the field's
    own stream, the same for every behaviour."""
    field = random.Random(f"keepout field {seed} {density:g} {index}")
    move = random.Random(f"keepout motion {seed} {density:g} {index} {behaviour}")
    origin = (field.uniform(*CENTRE_BOX[0]), field.uniform(*CENTRE_BOX[1]))
    vel_sigma = float(params["vel_sigma"])
    field_m = float(params["field_m"])
    sigma_p = tuple(params["sigma_p"])
    tracker = params.get("tracker", "model")
    sid = f"{seed}-{density:g}-{index}-{behaviour}"
    tracks: dict[str, Any] = {}
    # the target: holding, declared hostile by an operator, followed for 15 minutes; its
    # track the generator's in every variant (a good one, as an engagement needs)
    tsig = field.uniform(*SIGMA_P)
    target = facts(Hold((0.0, 0.0)), 900.0, tsig, VEL_SIGMA, field, origin, "hostile", 0.9)
    target.update({"declared_by": "op-1", "identity": "small_fast_boat", "identity_confidence": 0.8, "confidence": 0.95})
    tracks["T-0000"] = target
    truth: dict[str, Any] = {}
    n = _poisson(field, float(density) * math.pi * (field_m / 1000.0) ** 2)
    mix = mix_for(float(params.get("new_share", dict(MIX)["new"])))
    mix_names = [b for b, _ in mix]
    mix_w = [w for _, w in mix]
    lanes = _lanes(field) if params.get("layout") == "lanes" else None
    kinds: dict[str, int] = defaultdict(int)
    motions: dict[str, Any] = {}

    oov_share = float(params.get("oov_share") or 0.0)

    def add(cid: str, kind: str, m: Any, tracked: float, sigma: float, aff: str, aff_c: float, alt_m: float | None = None, masked: float | None = None) -> None:
        kinds[kind] += 1
        if masked is not None:
            # rules 13: tracked a while before it went out of view
            tracked = max(tracked, -masked + move.uniform(60.0, 600.0))
        f = facts(m, tracked, sigma, vel_sigma, move, origin, aff, aff_c, "air" if alt_m is not None else "surface")
        if tracker == "model":
            tracks[cid] = f
        else:
            motions[cid] = {"motion": m.spec(), "tracked_s": round(tracked, 3), "behaviour": kind}
            if alt_m is not None:
                motions[cid].update({"domain": "air", "alt_m": round(alt_m, 1), "kind": "light_aircraft"})
            if masked is not None:
                motions[cid]["masked_from_s"] = round(masked, 3)
        truth[cid] = {"motion": m, "behaviour": kind, "tracked_s": tracked}
        if alt_m is not None:
            truth[cid]["alt_m"] = alt_m
        if masked is not None:
            truth[cid]["masked_from_s"] = masked

    def out_of_view() -> float | None:
        """Rules 13: when this contact goes out of view (s, negative), in the oov variants."""
        if oov_share > 0.0 and move.random() < oov_share:
            return -move.uniform(*OOV_FROM_S)
        return None

    for i in range(n):
        p = _disc(field, field_m)
        sigma = field.uniform(*sigma_p)
        aff = "neutral" if field.random() < 0.6 else "unknown"
        aff_c = field.uniform(0.3, 0.9)
        if lanes and move.random() < 0.7:
            m, tracked = _lane_contact(move, lanes[move.randrange(2)], field_m)
            add(f"C-{i + 1:04d}", "lane", m, tracked, sigma, aff, aff_c)
            continue
        kind = move.choices(mix_names, mix_w)[0] if behaviour == "mixed" else behaviour
        m, tracked = motion(kind, p, move)
        add(f"C-{i + 1:04d}", kind, m, tracked, sigma, aff, aff_c, masked=out_of_view() if kind != "new" else None)
    if params.get("near_circler_km"):
        # the live run's question: a tour boat circling near the target, followed for a while
        p = _disc(move, float(params["near_circler_km"]) * 1000.0)
        m, _ = motion("circling", p, move)
        add("C-9999", "circling (near)", m, move.uniform(*TRACKED_S), move.uniform(*sigma_p), "neutral", 0.8)
    if params.get("near_oov_km"):
        # rules 13, the review's case: a contact near the target goes out of view
        p = _disc(move, float(params["near_oov_km"]) * 1000.0)
        kind = move.choices([b for b in mix_names if b != "new"], [w for b, w in mix if b != "new"])[0]
        m, tracked = motion(kind, p, move)
        add("C-8888", kind, m, tracked, move.uniform(*sigma_p), "neutral", 0.8, masked=-move.uniform(*OOV_FROM_S))
    for k in range(int(params.get("air_contacts") or 0)):
        # a light aircraft loitering near the target (after the surface traffic: that stays the
        # same as without it)
        r, speed = move.uniform(500.0, 1500.0), move.uniform(30.0, 55.0)
        c = _disc(move, float(params.get("air_within_m") or 4000.0))
        sense = 1 if move.random() < 0.5 else -1
        m: Any = Circle(c, r, speed, sense, _course(move))
        if params.get("air_motion") == "straight":
            # flying straight past: at the same point now, on a uniform course
            m = Line(c, _vec(speed, _course(move)))
        add(f"A-{k + 1:04d}", "air", m, move.uniform(*TRACKED_S), move.uniform(*sigma_p), "unknown", 0.5, alt_m=move.uniform(150.0, 1500.0))
    # ours: the observing USV, holding or patrolling; sometimes another one lost
    vehicles = []
    ours: dict[str, Any] = {}
    p = _disc_ring(field, 1000.0, 3000.0)
    v = (0.0, 0.0) if field.random() < 0.5 else _vec(field.uniform(2.0, 6.0), _course(field))
    usv1 = to_lonlat(origin, *p)
    vehicles.append({"uid": "usv-1", "platform": "seahunter", "at": usv1, "velocity": list(v), "report_age_s": 0.0})
    ours["usv-1"] = Line(p, v)
    if field.random() < float(params["lost_share"]):
        p = _disc_ring(field, 2000.0, 8000.0)
        v = (0.0, 0.0) if field.random() < 0.5 else _vec(field.uniform(2.0, 6.0), _course(field))
        age = field.uniform(15.0, 300.0)
        vehicles.append({"uid": "usv-2", "platform": "seahunter", "at": to_lonlat(origin, *p), "velocity": list(v), "report_age_s": round(age, 1)})
        # truth: it carries on as it last reported
        ours["usv-2"] = Line((p[0] + v[0] * age, p[1] + v[1] * age), v)
    evals = []
    for tte in params["tte_s"]:
        plat = field.choice(params["platforms"])
        d = max(0.0, (float(tte) - DISPATCH_S) * cruise[plat])
        b = _course(field)
        evals.append({"key": f"tte-{tte}", "platform": plat, "at": to_lonlat(origin, d * math.sin(b), d * math.cos(b))})
    for dist in params["dist_m"]:
        for plat in params["platforms"]:
            b = _course(field)
            evals.append({"key": f"dist-{dist}-{plat}", "platform": plat, "at": to_lonlat(origin, dist * math.sin(b), dist * math.cos(b))})
    probe: dict[str, Any] = {
        "id": sid,
        "t_ms": T0_MS,
        "target": "T-0000",
        "radius_m": float(params["radius_m"]),
        "fallback": "return",
        "tracks": tracks,
        "vehicles": vehicles,
        "evals": evals,
    }
    if tracker != "model":
        # the observing USV, where it is (it watches from there; a holding or slow patrol moves
        # it little over the history played)
        probe["tracker"] = {
            "mode": tracker,
            "origin": list(origin),
            "seed": int(field.random() * 2**53),
            "observers": [{"uid": 1, "platform": "seahunter", "at": usv1}],
            "contacts": motions,
            "max_history_s": float(params["max_history_s"]),
            "stats": index < int(params.get("stats_per_chunk", 0)),
        }
    return {
        "id": sid,
        "seed": seed,
        "density": density,
        "index": index,
        "behaviour": behaviour,
        "origin": origin,
        "n_contacts": len(truth),
        "kinds": dict(kinds),
        "probe": probe,
        "truth": truth,
        "ours": ours,
    }


# ---- truth at the effect ------------------------------------------------------------------------------


def at_risk(sit: dict[str, Any], effect: list[float], t_s: float, radius: float, keep_out: float, late_s: float = 0.0, step_s: float = 2.0) -> dict[str, Any]:
    """Who is truly where at the effect time: protected contacts inside the effect circle
    (`danger`) and inside the keep-out (`near`, those included), ours inside the effect circle
    (`ours_danger`); and over the release window, from the effect time to `late_s` after (every
    `step_s` and its end): `danger_late`, `near_late`."""
    e = to_enu(sit["origin"], effect)
    danger, near, danger_late, near_late = [], [], [], []
    reach = keep_out + 1.0
    times = [t_s + k * step_s for k in range(int(late_s // step_s) + 1)]
    if late_s > 0 and times[-1] < t_s + late_s:
        times.append(t_s + late_s)
    for cid, tr in sit["truth"].items():
        m = tr["motion"]
        p0 = m.pos(0.0)
        # cheap bound first: it cannot get closer than this by the end of the window
        if math.hypot(p0[0] - e[0], p0[1] - e[1]) - m.vmax * max(t_s + late_s, 0.0) > reach:
            continue
        # an aircraft counts where it is in 3D: above the keep-out's height (the effect radius
        # plus the policy's 200 m over the surface effect) it is outside it, above the radius
        # outside the effect
        alt = abs(tr.get("alt_m") or 0.0)
        if alt > keep_out:
            continue
        for k, t in enumerate(times):
            p = m.pos(t)
            d = math.hypot(p[0] - e[0], p[1] - e[1])
            if alt > radius:
                d = max(d, radius + 1e-6) if d <= keep_out else d
            if k == 0 and d <= keep_out:
                near.append(cid)
                if d <= radius:
                    danger.append(cid)
            if d <= keep_out and cid not in near_late:
                near_late.append(cid)
            if d <= radius:
                danger_late.append(cid)
                break
    ours = []
    for uid, m in sit["ours"].items():
        p = m.pos(t_s)
        if math.hypot(p[0] - e[0], p[1] - e[1]) <= radius:
            ours.append(uid)
    return {"danger": danger, "near": near, "ours_danger": ours, "danger_late": danger_late, "near_late": near_late}


# ---- running ------------------------------------------------------------------------------------------


def cargo() -> str:
    return os.environ.get("CARGO") or shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")


def build_probe() -> Path:
    """Build the probe (a workspace member) into the repository's target directory; a no-op
    when it is current."""
    cmd = [cargo(), "build", "--release", "-p", PROBE_PACKAGE]
    p = subprocess.run(cmd, cwd=str(REPO), capture_output=True, text=True)
    if p.returncode != 0:
        raise RuntimeError(f"building the probe failed: {p.stderr.strip()[-2000:]}")
    return PROBE


def run_probe(inputs: list[dict[str, Any]], rules: Iterable[int] = RULES, probe: Path = PROBE, keep_out_m: float | None = None) -> list[dict[str, Any]]:
    """Every evaluation of `inputs` through the probe: one result per (situation, evaluation),
    and one `tracker` line per situation whose tracks the real tracker made. `keep_out_m`: a copy
    of the policy with that buffer."""
    data = "\n".join(json.dumps(s, separators=(",", ":")) for s in inputs) + "\n"
    extra = ["--keep-out-m", f"{keep_out_m:g}"] if keep_out_m is not None else []
    p = subprocess.run(
        [str(probe), "--root", str(REPO), "--rules", ",".join(str(r) for r in rules), *extra],
        input=data,
        capture_output=True,
        text=True,
        cwd=str(REPO),
        timeout=7200,
    )
    if p.returncode != 0:
        raise RuntimeError(f"murmur-keepout-probe exited {p.returncode}: {p.stderr.strip()[-2000:]}")
    return [json.loads(line) for line in p.stdout.splitlines() if line.strip()]


def motion_check(motions: list[Any], times: list[float], probe: Path = PROBE) -> list[list[list[float]]]:
    """Where the probe's own copy of each motion puts it at `times` (it must be where ours does)."""
    (r,) = run_probe([{"motion_check": {"motions": [m.spec() for m in motions], "times": times}}], probe=probe)
    return r["positions"]


#: reference tracks for the region-size table: (name, velocity, average velocity, tracked s,
#: manoeuvre speed); 15 m 1-sigma, a surface contact
REFERENCE = (
    ("holding, tracked 10 min", (0.0, 0.0), (0.0, 0.0), 600.0, None),
    ("transiting at 8 m/s", (8.0, 0.0), (8.0, 0.0), 600.0, None),
    ("circling at 5 m/s, manoeuvre seen", (0.0, 5.0), (1.0, 0.5), 600.0, 6.0),
    ("zigzagging at 10 m/s, manoeuvre seen", (10.0, 0.0), (7.0, 0.0), 600.0, 11.0),
    ("young: tracked 20 s, holding", (0.0, 0.0), None, 20.0, None),
    ("new: tracked 3 s, no velocity yet", None, None, 3.0, None),
)
REFERENCE_SPANS_S = (30, 60, 90, 120, 180, 240, 300)


def region_sizes(rules: Iterable[int] = RULES, probe: Path = PROBE) -> list[dict[str, Any]]:
    """How large the engine makes each keep-out region of the reference tracks, by span and
    rules version (`engage::track_reach` through the probe)."""
    lines = []
    for name, vel, mean, tracked, man in REFERENCE:
        t: dict[str, Any] = {
            "position": {"value": [CENTRE_BOX[0][0], CENTRE_BOX[1][0]], "t": T0_MS},
            "pos_sigma_m": 15.0,
            "confidence": 0.9,
            "affiliation": {"value": "neutral", "t": T0_MS},
            "affiliation_confidence": 0.5,
            "identity_confidence": 0.2,
            "sources": ["usv-1:radar"],
            "domain": "surface",
            "last_seen_t": T0_MS,
            "first_seen_t": T0_MS - int(tracked * 1000),
        }
        if vel is not None:
            t["velocity_mps"] = list(vel)
        if mean is not None:
            t["mean_velocity_mps"] = list(mean)
        if man is not None:
            t["manoeuvre"] = {"t": T0_MS - 30_000, "change_mps": man, "speed_mps": man}
        lines.append({"regions": {"name": name, "track": t, "spans_s": list(REFERENCE_SPANS_S)}})
    return run_probe(lines, rules, probe)


def _kof_margin(detail: str | None, keep_out: float) -> float | None:
    # "vehicle usv-2 could be 812 m from the effect point at the effect time (...)"
    if not detail or "could be " not in detail:
        return None
    try:
        return float(detail.split("could be ", 1)[1].split(" m ", 1)[0]) - keep_out
    except ValueError:
        return None


def record(sit: dict[str, Any], res: dict[str, Any], params: dict[str, Any]) -> dict[str, Any]:
    """One evaluation, labelled with its truth: what each rules version decided and who was at
    risk."""
    key = res["eval"]
    kind, _, rest = key.partition("-")
    if kind == "tte":
        nominal, platform = int(rest), res.get("platform")
    else:
        d, _, platform = rest.partition("-")
        nominal = int(d)
    out: dict[str, Any] = {
        "id": sit["id"],
        "seed": sit["seed"],
        "density": sit["density"],
        "behaviour": sit["behaviour"],
        "kind": kind,
        "nominal": nominal,
        "platform": platform,
        "n_contacts": sit["n_contacts"],
        "lost_friendly": any(v["report_age_s"] > 0 for v in sit["probe"]["vehicles"]),
    }
    if res.get("error"):
        out["error"] = res["error"]
        return out
    pv = res.get("preview")
    if not pv:
        out["preview"] = False
        return out
    out["preview"] = True
    out["tte_s"] = pv["tte_s"]
    keep_out = float(pv["keep_out_m"])
    line = float(params["radius_m"]) + LABEL_BUFFER_M
    truth = at_risk(sit, pv["effect"], pv["tte_s"], float(params["radius_m"]), line, float(params.get("late_s", LATE_S)))
    out["danger"] = len(truth["danger"])
    out["near"] = len(truth["near"])
    out["danger_late"] = len(truth["danger_late"])
    out["near_late"] = len(truth["near_late"])
    out["ours_danger"] = len(truth["ours_danger"])
    out["near_kinds"] = sorted({sit["truth"][c]["behaviour"] for c in truth["near"]})
    # rules 13: of those, contacts out of view and dropped by the nomination (and still stale)
    def dropped(c: str) -> bool:
        m = sit["truth"][c].get("masked_from_s")
        return m is not None and m <= -(DROP_S + 1.0)

    if any("masked_from_s" in t for t in sit["truth"].values()):
        for k in ("near", "danger", "near_late", "danger_late"):
            out[k + "_dropped"] = sum(1 for c in truth[k] if dropped(c))
    if res.get("preview_differs"):
        out["preview_differs"] = True
    rules: dict[str, Any] = {}
    for v, r in (res.get("rules") or {}).items():
        intr = r.get("intr") or []
        rec = {
            "kot": int(r.get("kot") != "pass"),
            "kof": int(r.get("kof") != "pass"),
            "margin": r.get("margin_m"),
            "comp": r.get("comp") or {},
            "dom": intr[0][1] if intr and r.get("kot") != "pass" else None,
            "dom_track": intr[0][0] if intr and r.get("kot") != "pass" else None,
            "n_intr": r.get("n_intr", 0),
            "n_tracks_intr": r.get("n_tracks_intr", 0),
            "other": [c for c in r.get("failed", []) if c not in ("keep_out_tracks", "keep_out_friendly")],
        }
        if rec["dom_track"]:
            rec["dom_behaviour"] = sit["truth"].get(rec["dom_track"], {}).get("behaviour")
        if rec["kof"]:
            uid = r.get("kof_uid")
            veh = next((x for x in sit["probe"]["vehicles"] if x["uid"] == uid), None)
            rec["kof_lost"] = int(bool(veh and veh["report_age_s"] > 10.0))
            rec["kof_margin"] = _kof_margin(r.get("kof_detail"), keep_out)
        if r.get("mismatch"):
            rec["mismatch"] = r["mismatch"]
        # rules 13: the lost contacts, and the other policies' verdicts on the same evaluation
        if r.get("lost_variants"):
            rec["lost"] = r["lost_variants"]
            rec["n_lost"] = r.get("n_lost", 0)
            rec["n_lost_intr"] = r.get("n_lost_intr", 0)
        rules[v] = rec
    out["rules"] = rules
    return out


def run_chunk(job: tuple[int, float, str, int, dict[str, Any], str]) -> dict[str, Any]:
    """Situations `0..n` of one seed, density and behaviour, through the probe, labelled."""
    seed, density, behaviour, n, params, probe = job
    cruise = cruise_speeds()
    t0 = time.monotonic()
    sits = [situation(seed, density, i, behaviour, params, cruise) for i in range(n)]
    t_gen = time.monotonic() - t0
    results = run_probe([s["probe"] for s in sits], params["rules"], Path(probe), params.get("keep_out_m"))
    t_probe = time.monotonic() - t0 - t_gen
    by_id = {s["id"]: s for s in sits}
    records, quality, untracked = [], [], [0, 0]
    for r in results:
        if "tracker" in r:
            tr = r["tracker"]
            untracked[0] += tr.get("contacts", 0)
            untracked[1] += tr.get("contacts", 0) - tr.get("tracked", 0)
            quality += tr.get("per_contact") or []
            continue
        if "eval" not in r:
            raise RuntimeError(f"murmur-keepout-probe: {json.dumps(r)[:2000]}")
        records.append(record(by_id[r["id"]], r, params))
    return {
        "seed": seed,
        "density": density,
        "behaviour": behaviour,
        "records": records,
        "quality": quality,
        "untracked": untracked,
        "contacts": sum(s["n_contacts"] for s in sits),
        "kinds": _sum_kinds(sits),
        "timing_s": {"generate": round(t_gen, 2), "probe": round(t_probe, 2), "label": round(time.monotonic() - t0 - t_gen - t_probe, 2)},
    }


def _sum_kinds(sits: list[dict[str, Any]]) -> dict[str, int]:
    out: dict[str, int] = defaultdict(int)
    for s in sits:
        for k, v in s["kinds"].items():
            out[k] += v
    return dict(out)


def run(out: Path, seeds: list[int], situations: int, params: dict[str, Any] | None = None, workers: int | None = None, build: bool = True) -> dict[str, Any]:
    """One variant: every seed x density x behaviour chunk into `out`: `records.jsonl.gz` (one
    line per evaluation, every rules version in it) and `summary.json`."""
    params = {**default_params(), **(params or {})}
    cruise = cruise_speeds()
    missing = [p for p in params["platforms"] if p not in cruise]
    if missing:
        raise RuntimeError(f"platforms without a cruise speed in sim/platforms.toml: {missing}")
    params["cruise_mps"] = {p: cruise[p] for p in params["platforms"]}
    params["dispatch_s"] = DISPATCH_S
    out.mkdir(parents=True, exist_ok=True)
    probe = build_probe() if build else PROBE
    if not probe.exists():
        raise RuntimeError(f"missing {probe}: cargo build --release -p {PROBE_PACKAGE}")
    jobs = [(seed, float(d), b, situations, params, str(probe)) for seed in seeds for d in params["densities"] for b in params["behaviours"]]
    workers = workers or max(1, min(len(jobs), (os.cpu_count() or 2) // 2, 12))
    t0 = time.monotonic()
    if workers == 1:
        chunks = [run_chunk(j) for j in jobs]
    else:
        with ProcessPoolExecutor(max_workers=workers) as pool:
            chunks = list(pool.map(run_chunk, jobs))
    chunks.sort(key=lambda c: (c["seed"], c["density"], _bkey(c["behaviour"])))
    path = out / "records.jsonl.gz"
    with gzip.open(path, "wt") as f:
        for c in chunks:
            for r in c["records"]:
                f.write(json.dumps(r, separators=(",", ":")) + "\n")
    kinds: dict[str, int] = defaultdict(int)
    for c in chunks:
        for k, v in c["kinds"].items():
            kinds[k] += v
    meta = {
        "label": NOT_HUMAN,
        "seeds": seeds,
        "situations_per_cell_per_seed": situations,
        "params": params,
        "contacts": sum(c["contacts"] for c in chunks),
        "contact_behaviours": dict(kinds),
        "untracked_contacts": [sum(c["untracked"][0] for c in chunks), sum(c["untracked"][1] for c in chunks)],
        "tracker_quality": tracker_quality([q for c in chunks for q in c["quality"]]),
        "wall_s": round(time.monotonic() - t0, 1),
        "workers": workers,
        "timing_s": {k: round(sum(c["timing_s"][k] for c in chunks), 1) for k in ("generate", "probe", "label")},
    }
    summary = summarize(read_records(path), meta)
    summary["regions"] = region_sizes(params["rules"], probe)
    # rules 13: the out-of-view comparison, where the variant has both versions
    if {12, 13} <= set(params["rules"]):
        summary["lost"] = lost_summary(read_records(path))
    (out / "summary.json").write_text(json.dumps(summary, indent=1, default=str))
    return summary


def run_all(out: Path, seeds: list[int], situations: int, variants: Iterable[str] | None = None, overrides: dict[str, Any] | None = None, workers: int | None = None, build: bool = True) -> dict[str, Any]:
    """Every variant into `out/<variant>/`, and `out/summary.json`: each variant's summary, the
    sensitivity table across them, and which is the primary."""
    names = list(variants or VARIANTS)
    unknown = [v for v in names if v not in VARIANTS]
    if unknown:
        raise RuntimeError(f"unknown variants {unknown}; known: {list(VARIANTS)}")
    if build:
        build_probe()
    out.mkdir(parents=True, exist_ok=True)
    summaries = {}
    t0 = time.monotonic()
    for v in names:
        summaries[v] = run(out / v, seeds, situations, variant_params(v, overrides), workers=workers, build=False)
    combined = {
        "label": NOT_HUMAN,
        "primary": PRIMARY if PRIMARY in summaries else names[0],
        "seeds": seeds,
        "situations_per_cell_per_seed": situations,
        "variants": summaries,
        "sensitivity": sensitivity(summaries),
        "wall_s": round(time.monotonic() - t0, 1),
    }
    (out / "summary.json").write_text(json.dumps(combined, indent=1, default=str))
    return combined


def read_records(path: Path) -> Iterator[dict[str, Any]]:
    with gzip.open(path, "rt") as f:
        for line in f:
            if line.strip():
                yield json.loads(line)


# ---- summaries ---------------------------------------------------------------------------------------

COMPONENTS = ("current", "mean", "manoeuvre", "young", "no_velocity")


def would_pass_without(comp: dict[str, float], drop: str | Iterable[str]) -> bool:
    """Whether the keep-out on tracks would hold with the regions of `drop` left out: every
    other component's nearest region outside the keep-out (2D: the generated tracks carry no
    altitude, so the verdict is every region's). Margins are the probe's, unrounded."""
    drop = {drop} if isinstance(drop, str) else set(drop)
    return all(m >= 0.0 for c, m in comp.items() if c not in drop)


def classify(r: dict[str, Any]) -> str:
    """A block on tracks: `needed` when a protected contact is truly inside the keep-out at the
    effect time (the policy's own line), `severe` when one is inside the effect circle (needed
    too); `conservative` when nobody is inside the keep-out."""
    if r.get("danger"):
        return "severe"
    if r.get("near"):
        return "needed"
    return "conservative"


def _rate(k: int, n: int) -> dict[str, Any]:
    p, lo, hi = wilson(k, n)
    return {"k": k, "n": n, "rate": p, "lo": lo, "hi": hi}


def upper_bound_zero(n: int, level: float = 0.95) -> float | None:
    """The one-sided upper bound on a rate seen 0 times in `n` independent tries."""
    return None if n <= 0 else 1.0 - (1.0 - level) ** (1.0 / n)


def threshold(curve: list[tuple[float, float]], level: float) -> float | None:
    """The longest time to effect at which the pass rate is still at least `level`, linearly
    interpolated between the grid's points (the rate falls with time to effect): None when it
    is below `level` at the first point (shorter than the grid), `inf` when never below it."""
    pts = sorted(curve)
    if not pts or pts[0][1] < level:
        return None
    for (t0, p0), (t1, p1) in zip(pts, pts[1:]):
        if p1 < level <= p0:
            return t0 + (t1 - t0) * (p0 - level) / (p0 - p1) if p0 != p1 else t0
    return math.inf


def _boot_thresholds(rows: list[list[int]], grid: list[float], levels: tuple[float, ...], draws: int, seed: int) -> dict[float, tuple[Any, Any, Any]]:
    """The thresholds on the pooled rows (one per situation: pass per grid point) and a
    percentile bootstrap over situations."""
    out = {}
    n = len(rows)
    if n == 0 or not grid:
        return {lv: (None, None, None) for lv in levels}

    def curve(sample: list[list[int]]) -> list[tuple[float, float]]:
        return [(g, sum(col) / len(sample)) for g, col in zip(grid, zip(*sample))]

    est = {lv: threshold(curve(rows), lv) for lv in levels}
    rng = random.Random(seed)
    boots: dict[float, list[float]] = {lv: [] for lv in levels}
    for _ in range(draws):
        sample = rng.choices(rows, k=n)
        c = curve(sample)
        for lv in levels:
            t = threshold(c, lv)
            boots[lv].append(-1.0 if t is None else t)
    below, above = f"<{grid[0]:g}", f">{grid[-1]:g}"

    def show(t: float | None) -> float | str:
        if t is None or t < 0:
            return below
        if t == math.inf:
            return above
        return round(t, 1)

    for lv in levels:
        b = sorted(boots[lv])
        out[lv] = (show(est[lv]), show(b[int(0.025 * draws)]), show(b[int(0.975 * draws) - 1]))
    return out


def cluster_rate(pairs: list[tuple[int, int]]) -> dict[str, Any]:
    """`sum k / sum n` pooled over clusters `(k_i, n_i)` (a situation's evaluations share its
    contacts), with a Wilson 95% interval on the effective sample size `n / deff`: `deff` the
    ratio's between-cluster variance over the binomial one, at least 1; at 0 or 1, where it
    can't be estimated, one observation per cluster (and the upper bound for none seen)."""
    pairs = [(k, n) for k, n in pairs if n > 0]
    m, k_sum, n_sum = len(pairs), sum(k for k, _ in pairs), sum(n for _, n in pairs)
    if n_sum == 0:
        return {"k": 0, "n": 0, "rate": None, "lo": None, "hi": None, "clusters": 0, "deff": None}
    r = k_sum / n_sum
    if 0 < r < 1 and m > 1:
        v_cl = m / (m - 1) * sum((k - r * n) ** 2 for k, n in pairs) / n_sum**2
        deff = max(1.0, v_cl / (r * (1 - r) / n_sum))
        n_eff = n_sum / deff
    else:
        deff, n_eff = None, float(m)
    _, lo, hi = wilson(r * n_eff, n_eff)  # type: ignore[arg-type]
    out = {"k": k_sum, "n": n_sum, "rate": r, "lo": lo, "hi": hi, "clusters": m, "deff": None if deff is None else round(deff, 2)}
    if k_sum == 0:
        out["upper95_tries"] = upper_bound_zero(n_sum)
        out["upper95_clusters"] = upper_bound_zero(m)
    return out


def _field(sid: str) -> str:
    """The field a situation was drawn on (seed, density, index): the same under every
    behaviour."""
    return sid.rsplit("-", 1)[0]


def _median(xs: list[float]) -> float | None:
    if not xs:
        return None
    s = sorted(xs)
    k = len(s) // 2
    return round(s[k] if len(s) % 2 else (s[k - 1] + s[k]) / 2, 2)


def tracker_quality(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """What the tracker reported against the truth, per contact behaviour: the share it held a
    track of, of those the share with a velocity, an average velocity, a manoeuvre, young; the
    median velocity error, position error and reported 1-sigma."""
    by: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for r in rows:
        by[r.get("behaviour", "?")].append(r)
    out = []
    for b in sorted(by, key=lambda x: (_bkey(x), x)):
        rs = by[b]
        tr = [r for r in rs if r.get("tracked")]
        vel = [r for r in tr if r.get("vel")]
        out.append(
            {
                "behaviour": b,
                "contacts": len(rs),
                "tracked": len(tr),
                "with_velocity": len(vel),
                "with_mean": sum(1 for r in tr if r.get("mean")),
                "manoeuvre": sum(1 for r in tr if r.get("manoeuvre")),
                "young": sum(1 for r in tr if r.get("young")),
                "vel_err_median": _median([r["vel_err"] for r in vel if r.get("vel_err") is not None]),
                "vel_err_p90": _quantile([r["vel_err"] for r in vel if r.get("vel_err") is not None], 0.9),
                "pos_err_median": _median([r["pos_err"] for r in tr if r.get("pos_err") is not None]),
                "sigma_median": _median([r["sigma"] for r in tr if r.get("sigma") is not None]),
            }
        )
    return out


def _quantile(xs: list[float], q: float) -> float | None:
    if not xs:
        return None
    s = sorted(xs)
    return round(s[min(len(s) - 1, int(q * len(s)))], 2)


def summarize(records: Iterable[dict[str, Any]], meta: dict[str, Any] | None = None, draws: int = 1000) -> dict[str, Any]:
    """The tables, from the records alone. Rates per cell (one evaluation per situation) carry a
    Wilson interval; shares pooled over a situation's evaluations a clustered one
    (`cluster_rate`), every density and time to effect weighted as the grid has them (uniform);
    the keep-out on our vehicles counts each field once (it does not depend on the contacts)."""
    meta = meta or {}
    # per (behaviour, density, nominal, rules): n, blocked on tracks, on ours, either, and the
    # truth: someone inside the keep-out (perfect knowledge would block), in the circle, and
    # over the release window
    tte_cells: dict[tuple, dict[str, int]] = defaultdict(lambda: defaultdict(int))
    seed_cells: dict[tuple, list[int]] = defaultdict(lambda: [0, 0])
    dist_cells: dict[tuple, dict[str, int]] = defaultdict(lambda: defaultdict(int))
    per_sit: dict[tuple, dict[str, dict[str, int]]] = defaultdict(lambda: defaultdict(lambda: defaultdict(int)))
    pen: dict[tuple, dict[str, list[float]]] = defaultdict(lambda: defaultdict(list))
    dom_behaviour: dict[tuple, dict[str, int]] = defaultdict(lambda: defaultdict(int))
    by_tte_class: dict[tuple, dict[str, int]] = defaultdict(lambda: defaultdict(int))
    friendly: dict[tuple, dict[str, int]] = defaultdict(lambda: defaultdict(int))
    friendly_seen: set[tuple] = set()
    other: dict[tuple, dict[str, int]] = defaultdict(lambda: defaultdict(int))
    pass_rows: dict[tuple, dict[str, dict[int, int]]] = defaultdict(lambda: defaultdict(dict))
    counts: dict[str, Any] = {"evaluations": 0, "tte_evaluations": 0, "no_preview": 0, "errors": 0, "mismatches": 0, "preview_differs": 0, "tte_off": 0}
    sids: set[str] = set()
    actual_tte: dict[int, list[float]] = defaultdict(list)
    for r in records:
        counts["evaluations"] += 1
        sids.add(r["id"])
        if r.get("error"):
            counts["errors"] += 1
            continue
        if not r.get("preview"):
            counts["no_preview"] += 1
            continue
        if r.get("preview_differs"):
            counts["preview_differs"] += 1
        b, d = r["behaviour"], r["density"]
        if r["kind"] == "tte":
            counts["tte_evaluations"] += 1
            actual_tte[r["nominal"]].append(r["tte_s"])
            if abs(r["tte_s"] - r["nominal"]) > 5.0:
                counts["tte_off"] += 1
        truth = {"near": int(r["near"] > 0), "danger": int(r["danger"] > 0), "near_late": int(r.get("near_late", 0) > 0), "danger_late": int(r.get("danger_late", 0) > 0)}
        for v, x in r["rules"].items():
            v = int(v)
            if x.get("mismatch"):
                counts["mismatches"] += 1
            for c in x.get("other", []):
                other[(r["kind"], v)][c] += 1
            other[(r["kind"], v)]["_n"] += 1
            cell = tte_cells[(b, d, r["nominal"], v)] if r["kind"] == "tte" else dist_cells[(b, d, r["platform"], r["nominal"], v)]
            cell["n"] += 1
            cell["kot"] += x["kot"]
            cell["kof"] += x["kof"]
            cell["any"] += int(x["kot"] or x["kof"])
            for k, t in truth.items():
                cell[k] += t
                cell["missed_" + k] += int(not x["kot"] and t)
            if r["kind"] != "tte":
                continue
            sc = seed_cells[(b, d, r["nominal"], v, r["seed"])]
            sc[0] += 1
            sc[1] += x["kot"]
            pass_rows[(b, d, v)][r["id"]][r["nominal"]] = int(not x["kot"])
            fkey = (_field(r["id"]), r["nominal"], v)
            if fkey not in friendly_seen:
                friendly_seen.add(fkey)
                fr = friendly[(r["nominal"], v)]
                fr["n"] += 1
                fr["blocked"] += x["kof"]
                fr["lost_fields"] += int(r["lost_friendly"])
                if x["kof"]:
                    fr["lost" if x.get("kof_lost") else "predicted"] += 1
                    fr["needed"] += int(r["ours_danger"] > 0)
            ps = per_sit[(b, v)][r["id"]]
            tc = by_tte_class[(b, r["nominal"], v)]
            tc["n"] += 1
            if not x["kot"]:
                ps["passes"] += 1
                for k, t in truth.items():
                    ps["missed_" + k] += t
                continue
            ps["blocks"] += 1
            tc["blocks"] += 1
            for k, t in truth.items():
                ps["blocked_" + k] += t
                tc["blocked_" + k] += t
            if truth["near"]:
                continue
            # conservative: nobody inside the keep-out at the effect time
            ps["cons"] += 1
            comp = x["comp"]
            if x["dom"]:
                ps["dom:" + x["dom"]] += 1
                pen[(b, v)][x["dom"]].append(-(x["margin"] or 0.0))
            if x.get("dom_behaviour"):
                dom_behaviour[(b, v)][x["dom_behaviour"]] += 1
            for c, m in comp.items():
                if m < 0:
                    ps["inside:" + c] += 1
                    if would_pass_without(comp, c):
                        ps["alone:" + c] += 1
            if would_pass_without(comp, ("mean", "manoeuvre", "young")):
                ps["alone:rules 9 and 12 regions"] += 1

    def cellrow(c: dict[str, int]) -> dict[str, Any]:
        n = c["n"]
        return {
            "tracks": _rate(c["kot"], n),
            "friendly": _rate(c["kof"], n),
            "any": _rate(c["any"], n),
            "perfect": _rate(c["near"], n),
            "perfect_circle": _rate(c["danger"], n),
            "perfect_late": _rate(c["near_late"], n),
            "missed": {k: c["missed_" + k] for k in ("near", "danger", "near_late", "danger_late")},
        }

    rows_tte = []
    for (b, d, t, v), c in sorted(tte_cells.items(), key=lambda kv: (_bkey(kv[0][0]), kv[0][1], kv[0][3], kv[0][2])):
        seeds = [k / n_ for (bb, dd, tt, vv, _), (n_, k) in seed_cells.items() if (bb, dd, tt, vv) == (b, d, t, v) and n_]
        m, lo, hi = t_interval(seeds)
        rows_tte.append({"behaviour": b, "density": d, "tte_s": t, "rules": v, **cellrow(c), "tracks_seed_t": {"mean": m, "lo": lo, "hi": hi, "seeds": len(seeds)}})
    rows_dist = []
    for (b, d, plat, dist, v), c in sorted(dist_cells.items(), key=lambda kv: (_bkey(kv[0][0]), kv[0][1], kv[0][2], kv[0][4], kv[0][3])):
        rows_dist.append({"behaviour": b, "density": d, "platform": plat, "dist_m": dist, "rules": v, **cellrow(c)})
    block_rows = []
    for (b, v), sits in sorted(per_sit.items(), key=lambda kv: (_bkey(kv[0][0]), kv[0][1])):
        ss = list(sits.values())

        def share(key: str, of: str) -> dict[str, Any]:
            return cluster_rate([(s.get(key, 0), s.get(of, 0)) for s in ss])

        block_rows.append(
            {
                "behaviour": b,
                "rules": v,
                "blocks": sum(s.get("blocks", 0) for s in ss),
                "passes": sum(s.get("passes", 0) for s in ss),
                "needed": share("blocked_near", "blocks"),
                "severe": share("blocked_danger", "blocks"),
                "needed_late": share("blocked_near_late", "blocks"),
                "severe_late": share("blocked_danger_late", "blocks"),
                "conservative": share("cons", "blocks"),
                "missed": share("missed_near", "passes"),
                "missed_severe": share("missed_danger", "passes"),
                "missed_late": share("missed_near_late", "passes"),
                "missed_severe_late": share("missed_danger_late", "passes"),
                "conservative_by_component": {
                    c: {
                        "dominant": share("dom:" + c, "cons"),
                        "inside": share("inside:" + c, "cons"),
                        "pass_without": share("alone:" + c, "cons"),
                        "penetration_m_median": _median(pen[(b, v)].get(c, [])),
                    }
                    for c in COMPONENTS
                },
                "conservative_pass_without_rules_9_12": share("alone:rules 9 and 12 regions", "cons"),
                "conservative_dominant_behaviour": dict(dom_behaviour[(b, v)]),
            }
        )
    class_rows = []
    for (b, t, v), c in sorted(by_tte_class.items(), key=lambda kv: (_bkey(kv[0][0]), kv[0][2], kv[0][1])):
        class_rows.append({"behaviour": b, "tte_s": t, "rules": v, "n": c["n"], "blocks": c["blocks"], "needed": c["blocked_near"], "severe": c["blocked_danger"], "needed_late": c["blocked_near_late"]})
    thr_rows = []
    grid_all = sorted({t for (_, _, t, _) in tte_cells})
    for (b, d, v), per in sorted(pass_rows.items(), key=lambda kv: (_bkey(kv[0][0]), kv[0][1], kv[0][2])):
        rows = [[row.get(t, 0) for t in grid_all] for row in per.values() if len(row) == len(grid_all)]
        th = _boot_thresholds(rows, [float(g) for g in grid_all], (0.5, 0.9), draws, seed=_stable_seed(b, d, v))
        thr_rows.append({"behaviour": b, "density": d, "rules": v, "situations": len(rows), "t50": th[0.5], "t90": th[0.9]})
    fr_rows = []
    for (t, v), fr in sorted(friendly.items()):
        fr_rows.append({"tte_s": t, "rules": v, "fields": fr["n"], "blocked": _rate(fr["blocked"], fr["n"]), "lost": fr["lost"], "predicted": fr["predicted"], "needed": fr["needed"], "lost_fields": fr["lost_fields"]})
    other_rows = {f"{k}|{v}": dict(c) for (k, v), c in sorted(other.items())}
    return {
        "label": NOT_HUMAN,
        **meta,
        "situations": len(sids),
        "fields": len({_field(s) for s in sids}),
        "counts": counts,
        "actual_tte_s": {str(k): {"median": _median(v), "min": min(v), "max": max(v)} for k, v in sorted(actual_tte.items())},
        "tte": rows_tte,
        "dist": rows_dist,
        "blocks": block_rows,
        "blocks_by_tte": class_rows,
        "thresholds": thr_rows,
        "friendly": fr_rows,
        "other_checks": other_rows,
    }


def lost_policy_blocks(r: dict[str, Any]) -> dict[str, int] | None:
    """Rules 13: whether each of `LOST_POLICIES` blocks this evaluation on tracks (`today`: the
    rules 12 verdict; `held`: rules 13's; the rest the probe's `lost_variants` under rules 13,
    rules 13's own verdict where no contact was lost). None without both versions."""
    a, b = (r.get("rules") or {}).get("12"), (r.get("rules") or {}).get("13")
    if not a or not b:
        return None
    lv = b.get("lost") or {}
    out = {"today": a["kot"], "held": b["kot"]}
    for k in ("heading", "held600", "near3km", "accepted"):
        out[k] = int(lv[k] != "pass") if k in lv else b["kot"]
    return out


def lost_summary(records: Iterable[dict[str, Any]]) -> dict[str, Any]:
    """Rules 13, the out-of-view variants: for each policy of `LOST_POLICIES` (on the same
    evaluations): how often the keep-out on tracks blocks a nomination, how often only an
    acceptance of its lost contacts would let it pass (`needs_acceptance`: it blocks and passes
    with them all accepted), and its misses against the truth (a pass with a protected contact
    inside the keep-out line or the effect circle at the effect time, or over the release
    window), those of a contact out of view and dropped by the nomination apart; by density and
    time to effect, and pooled. Wilson intervals per evaluation (situations are independent;
    an evaluation's evaluations share their contacts, so pooled rates are a little narrower
    than they should be)."""
    cells: dict[tuple, dict[str, int]] = defaultdict(lambda: defaultdict(int))
    pooled: dict[str, dict[str, int]] = defaultdict(lambda: defaultdict(int))
    lost_counts = {"evaluations": 0, "with_lost": 0, "lost_contacts": 0}
    for r in records:
        if r.get("error") or not r.get("preview") or r["kind"] != "tte":
            continue
        pol = lost_policy_blocks(r)
        if pol is None:
            continue
        lost_counts["evaluations"] += 1
        n_lost = (r["rules"]["13"].get("n_lost") or 0)
        lost_counts["with_lost"] += int(n_lost > 0)
        lost_counts["lost_contacts"] += n_lost
        truth = {
            "near": r["near"] > 0,
            "danger": r["danger"] > 0,
            "near_late": r.get("near_late", 0) > 0,
            "danger_late": r.get("danger_late", 0) > 0,
            "near_dropped": r.get("near_dropped", 0) > 0,
            "danger_dropped": r.get("danger_dropped", 0) > 0,
            "danger_late_dropped": r.get("danger_late_dropped", 0) > 0,
        }
        for p, blocked in pol.items():
            for c in (cells[(r["density"], r["nominal"], p)], pooled[p]):
                c["n"] += 1
                c["blocked"] += blocked
                c["needs_acceptance"] += int(bool(blocked) and p in ("held", "heading", "held600", "near3km") and not pol["accepted"])
                c["perfect"] += int(truth["near"])
                if not blocked:
                    c["passes"] += 1
                    for k, t in truth.items():
                        c["missed_" + k] += int(t)

    def row(c: dict[str, int]) -> dict[str, Any]:
        out = {
            "n": c["n"],
            "blocked": _rate(c["blocked"], c["n"]),
            "needs_acceptance": _rate(c["needs_acceptance"], c["n"]),
            "perfect": _rate(c["perfect"], c["n"]),
            "passes": c["passes"],
        }
        for k in ("near", "danger", "near_late", "danger_late", "near_dropped", "danger_dropped", "danger_late_dropped"):
            out["missed_" + k] = {"k": c["missed_" + k], "n": c["passes"], "upper95": upper_bound_zero(c["passes"]) if c["missed_" + k] == 0 else None}
        return out

    return {
        "policies": LOST_POLICIES,
        "counts": lost_counts,
        "pooled": {p: row(c) for p, c in pooled.items()},
        "cells": [{"density": d, "tte_s": t, "policy": p, **row(c)} for (d, t, p), c in sorted(cells.items(), key=lambda kv: (kv[0][0], kv[0][1], list(LOST_POLICIES).index(kv[0][2])))],
    }


def table_lost(s: dict[str, Any]) -> str:
    """The out-of-view comparison as plain text."""
    lines = ["Out of view (rules 13): each policy on the same evaluations; blocked on tracks %, needs an acceptance %, misses (passes with someone truly inside):"]
    c = s["counts"]
    lines.append(f"  {c['evaluations']} evaluations, {c['with_lost']} with a lost contact ({c['lost_contacts']} lost contacts counted)")
    lines.append(f"  {'policy':<10}{'blocked':>14}{'needs acc.':>14}{'passes':>8}{'missed line':>13}{'circle':>8}{'win. circle':>12}{'line, dropped':>15}{'circle, dropped':>17}{'win., dropped':>15}")
    for p in LOST_POLICIES:
        r = s["pooled"].get(p)
        if not r:
            continue
        lines.append(
            f"  {p:<10}{_pct(r['blocked']):>14}{_pct(r['needs_acceptance']):>14}{r['passes']:>8}{r['missed_near']['k']:>13}{r['missed_danger']['k']:>8}"
            f"{r['missed_danger_late']['k']:>12}{r['missed_near_dropped']['k']:>15}{r['missed_danger_dropped']['k']:>17}{r['missed_danger_late_dropped']['k']:>15}"
        )
    tte = sorted({x["tte_s"] for x in s["cells"]})
    dens = sorted({x["density"] for x in s["cells"]})
    cells = {(x["density"], x["tte_s"], x["policy"]): x for x in s["cells"]}
    lines.append("  blocked % by density and time to effect: " + " | ".join(LOST_POLICIES))
    for d in dens:
        for t in tte:
            vals = [cells.get((d, t, p)) for p in LOST_POLICIES]
            lines.append(f"    {d:>5g}/km2 {t:>4}s  " + " | ".join(_pct(v["blocked"], False) if v else "-" for v in vals))
    return "\n".join(lines)


#: the times to effect the sensitivity table shows
SENS_TTE = (30, 60, 120, 180)


def sensitivity(summaries: dict[str, dict[str, Any]], behaviour: str = "mixed") -> list[dict[str, Any]]:
    """Across variants, for `behaviour` traffic: the block rate on tracks at 60, 120 and 180 s
    per density and rules version beside what perfect knowledge would block, the needed share
    of blocks and the passes that missed someone (rules 6 and 12), and the time to effect for a
    50% chance of passing."""
    out = []
    for name, s in summaries.items():
        cells = {(r["density"], r["tte_s"], r["rules"]): r for r in s.get("tte", []) if r["behaviour"] == behaviour}
        blocks = {r["rules"]: r for r in s.get("blocks", []) if r["behaviour"] == behaviour}
        thr = {(r["density"], r["rules"]): r for r in s.get("thresholds", []) if r["behaviour"] == behaviour}
        dens = sorted({d for d, _, _ in cells})
        if not dens:
            continue
        row: dict[str, Any] = {"variant": name, "about": (s.get("params") or {}).get("about"), "by_density": {}}
        for d in dens:
            row["by_density"][f"{d:g}"] = {
                "blocked": {f"{t}|{v}": (cells.get((d, t, v)) or {}).get("tracks") for t in SENS_TTE for v in (6, 9, 12)},
                "perfect": {f"{t}": (cells.get((d, t, 12)) or {}).get("perfect") for t in SENS_TTE},
                "t50": {f"{v}": (thr.get((d, v)) or {}).get("t50") for v in (6, 9, 12)},
                "t90": {f"{v}": (thr.get((d, v)) or {}).get("t90") for v in (6, 9, 12)},
            }
        row["needed"] = {f"{v}": (blocks.get(v) or {}).get("needed") for v in (6, 9, 12)}
        row["missed"] = {f"{v}": (blocks.get(v) or {}).get("missed") for v in (6, 9, 12)}
        row["missed_severe"] = {f"{v}": (blocks.get(v) or {}).get("missed_severe") for v in (6, 9, 12)}
        row["missed_late"] = {f"{v}": (blocks.get(v) or {}).get("missed_late") for v in (6, 9, 12)}
        out.append(row)
    return out


def _stable_seed(*parts: Any) -> int:
    return sum((i + 1) * ord(ch) for i, ch in enumerate(" ".join(str(p) for p in parts))) % 1_000_003


def _bkey(b: str) -> int:
    return BEHAVIOURS.index(b) if b in BEHAVIOURS else len(BEHAVIOURS)


def load_summary(batch: Path) -> dict[str, Any] | None:
    """The combined summary of a batch's keep-out measurement (a single variant's is wrapped)."""
    p = Path(batch) / "keepout" / "summary.json"
    if not p.exists():
        return None
    try:
        s = json.loads(p.read_text())
    except ValueError:
        return None
    if "variants" not in s:
        name = (s.get("params") or {}).get("variant", "model")
        s = {"label": s.get("label"), "primary": name, "variants": {name: s}, "sensitivity": sensitivity({name: s})}
    return s


# ---- plain text ------------------------------------------------------------------------------------


def _pct(r: dict[str, Any] | None, ci: bool = True) -> str:
    if not r or r.get("rate") is None:
        return "-"
    s = f"{100 * r['rate']:.0f}"
    if ci and r.get("lo") is not None:
        s += f" [{100 * r['lo']:.0f}-{100 * r['hi']:.0f}]"
    return s


def _t(x: float | str | None) -> str:
    if x is None:
        return "-"
    if isinstance(x, str):
        return x
    return f"{x:.0f}"


def _miss(r: dict[str, Any] | None) -> str:
    if not r or not r.get("n"):
        return "-"
    if r["k"] == 0 and r.get("upper95_tries") is not None:
        return f"0/{r['n']} (<{100 * r['upper95_tries']:.2f}%)"
    return f"{r['k']}/{r['n']}"


def table_variant(s: dict[str, Any]) -> str:
    """One variant's summary as plain text."""
    p = s.get("params") or {}
    lines = [f"variant {p.get('variant', '?')}: {p.get('about', '')}"]
    c = s["counts"]
    ut = s.get("untracked_contacts") or [0, 0]
    lines.append(
        f"{s['situations']} situations, {c['evaluations']} evaluations x rules {p.get('rules')}; {s.get('contacts', '?')} contacts"
        + (f" ({ut[1]} of {ut[0]} never tracked)" if ut[0] else "")
        + f"; mismatches {c['mismatches']}, errors {c['errors']}, no preview {c['no_preview']}"
    )
    grid = sorted({r["tte_s"] for r in s["tte"]})
    if grid:
        lines.append("Blocked by keep_out_tracks, % [Wilson 95%] (perfect knowledge would block: someone inside the keep-out), by nominal time to effect (s):")
        for v in sorted({r["rules"] for r in s["tte"]}):
            lines.append(f"rules {v}")
            lines.append(f"{'behaviour':<13}{'per km2':>8}  " + "".join(f"{t:>16}" for t in grid))
            cells = {(r["behaviour"], r["density"], r["tte_s"]): r for r in s["tte"] if r["rules"] == v}
            for b in sorted({r["behaviour"] for r in s["tte"]}, key=_bkey):
                for d in sorted({r["density"] for r in s["tte"]}):
                    row = [cells.get((b, d, t)) for t in grid]
                    lines.append(f"{b:<13}{d:>8g}  " + "".join(f"{(_pct(x['tracks']) + ' (' + _pct(x['perfect'], False) + ')') if x else '-':>16}" for x in row))
            lines.append("")
    if s.get("blocks"):
        lines.append("Blocks on tracks: needed (someone inside the keep-out at the effect time), severe (inside the circle), conservative; passes that missed someone:")
        lines.append(f"{'behaviour':<13}{'rules':>6}{'blocks':>8}{'needed %':>16}{'severe %':>14}{'release win. %':>16}{'passes':>8}{'missed':>18}{'severe':>10}{'win.':>10}")
        for r in s["blocks"]:
            lines.append(
                f"{r['behaviour']:<13}{r['rules']:>6}{r['blocks']:>8}{_pct(r['needed']):>16}{_pct(r['severe']):>14}{_pct(r['needed_late']):>16}{r['passes']:>8}"
                f"{_miss(r['missed']):>18}{r['missed_severe']['k']:>10}{r['missed_late']['k']:>10}"
            )
        lines.append("")
        lines.append("Conservative blocks (nobody inside the keep-out): the component reaching in deepest / would pass without it, %:")
        lines.append(f"{'behaviour':<13}{'rules':>6}{'cons.':>7}" + "".join(f"{c:>20}" for c in COMPONENTS) + f"{'w/o rules 9+12':>16}")
        for r in s["blocks"]:
            cbc = r["conservative_by_component"]
            cells = "".join(f"{_pct(cbc[c]['dominant'], False) + ' / ' + _pct(cbc[c]['pass_without'], False):>20}" for c in COMPONENTS)
            lines.append(f"{r['behaviour']:<13}{r['rules']:>6}{r['conservative']['k']:>7}{cells}{_pct(r['conservative_pass_without_rules_9_12'], False):>16}")
        lines.append("")
    if s.get("thresholds"):
        lines.append("Longest time to effect (s) with a pass on tracks at least 50% / 90% of the time [bootstrap 95%]:")
        for r in s["thresholds"]:
            t50, t90 = r["t50"], r["t90"]
            lines.append(f"  {r['behaviour']:<13}{r['density']:>6g}  rules {r['rules']:>2}  {_t(t50[0])} [{_t(t50[1])}-{_t(t50[2])}]   {_t(t90[0])} [{_t(t90[1])}-{_t(t90[2])}]")
        lines.append("")
    if s.get("dist"):
        dists = sorted({r["dist_m"] for r in s["dist"]})
        plats = sorted({r["platform"] for r in s["dist"]})
        versions = sorted({r["rules"] for r in s["dist"]})
        cells = {(r["behaviour"], r["density"], r["platform"], r["dist_m"], r["rules"]): r for r in s["dist"]}
        lines.append("Blocked by keep_out_tracks by platform and distance, %, rules " + " / ".join(map(str, versions)) + " (perfect knowledge):")
        lines.append(f"{'behaviour':<13}{'per km2':>8}  " + "".join(f"{p[:6] + ' ' + str(d):>20}" for p in plats for d in dists))
        for b in sorted({r["behaviour"] for r in s["dist"]}, key=_bkey):
            for d in sorted({r["density"] for r in s["dist"]}):
                row = []
                for p_ in plats:
                    for dist in dists:
                        vals = [cells.get((b, d, p_, dist, v)) for v in versions]
                        pf = (vals[-1] or {}).get("perfect")
                        row.append("/".join(_pct(x["tracks"], False) if x else "-" for x in vals) + f" ({_pct(pf, False)})")
                lines.append(f"{b:<13}{d:>8g}  " + "".join(f"{x:>20}" for x in row))
        lines.append("")
    if s.get("lost"):
        lines.append(table_lost(s["lost"]))
        lines.append("")
    q = s.get("tracker_quality") or []
    if q:
        lines.append("What the tracker reported (sampled contacts): tracked, with velocity, average velocity, manoeuvre, young; velocity error median / p90; position error, 1-sigma (medians):")
        for r in q:
            n, tr = r["contacts"], r["tracked"]
            lines.append(
                f"  {r['behaviour']:<16} {n:>6}  tracked {100 * tr / max(n, 1):5.1f}%  velocity {100 * r['with_velocity'] / max(tr, 1):5.1f}%  mean {100 * r['with_mean'] / max(tr, 1):5.1f}%"
                f"  manoeuvre {100 * r['manoeuvre'] / max(tr, 1):5.1f}%  young {100 * r['young'] / max(tr, 1):5.1f}%  vel err {r['vel_err_median']} / {r['vel_err_p90']} m/s"
                f"  pos err {r['pos_err_median']} m  sigma {r['sigma_median']} m"
            )
        lines.append("")
    return "\n".join(lines)


def table(s: dict[str, Any]) -> str:
    """The combined summary as plain text: the primary variant in full, the sensitivity table."""
    if "variants" not in s:
        return s.get("label", "") + "\n\n" + table_variant(s)
    lines = [s["label"], ""]
    prim = s["variants"].get(s["primary"])
    if prim:
        lines.append(table_variant(prim))
    lines.append("Sensitivity, mixed traffic: blocked at 30 / 60 / 120 / 180 s, rules 6 | 9 | 12 (perfect knowledge at the same times); needed share of blocks and missed passes, rules 6 / 12:")
    for row in s.get("sensitivity", []):
        for d, x in row["by_density"].items():
            b = x["blocked"]
            blocked = " | ".join("/".join(_pct(b.get(f"{t}|{v}"), False) for t in SENS_TTE) for v in (6, 9, 12))
            perfect = "/".join(_pct(x["perfect"].get(f"{t}"), False) for t in SENS_TTE)
            lines.append(
                f"  {row['variant']:<16} {d:>5}/km2  {blocked}  (perfect {perfect})  t50 r12 {_t((x['t50'].get('12') or [None])[0])}  t90 r12 {_t((x['t90'].get('12') or [None])[0])}"
            )
        lines.append(
            f"  {'':<16} needed {_pct(row['needed'].get('6'), False)} / {_pct(row['needed'].get('12'), False)}%  missed {_miss(row['missed'].get('6'))} / {_miss(row['missed'].get('12'))}"
            f"  in the circle {_miss(row['missed_severe'].get('6'))} / {_miss(row['missed_severe'].get('12'))}"
            f"  missed over the release window {_miss(row['missed_late'].get('6'))} / {_miss(row['missed_late'].get('12'))}"
        )
    lines.append("")
    for name, v in s["variants"].items():
        if not v.get("tte") and v.get("dist"):
            lines.append(table_variant(v))
        if v.get("lost") and name != s.get("primary"):
            lines.append(f"variant {name}: {(v.get('params') or {}).get('about', '')}")
            lines.append(table_lost(v["lost"]))
            lines.append("")
    return "\n".join(lines)
