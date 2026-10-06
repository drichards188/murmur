"""Keep-outs against engagements, on generated situations (rules 6, 9 and 12): how often a
keep-out blocks the nomination of an engagement on a hostile target, as a function of traffic
density and behaviour, time to effect (the platform's speed and distance) and the rules
version; whether a block protected someone or nobody was at risk; and which keep-out component
blocked. MODEL RESULTS FROM SIMULATED GEOMETRY, NEVER HUMAN DATA.

**What runs.** For each seed, density and behaviour, `situation` draws a world from the stated
distributions below (seeded, and paired: the same seed, density and index give the same
target, contact positions, our vehicles and platform placements under every behaviour, only
the motion differs): a hostile target holding in the engagement zone ez-gate-west, declared
hostile by an operator; protected contacts (neutral or unknown, none declared) uniform over a
disc of `FIELD_M` around it, as many as a Poisson draw of the density gives; our observing USV;
sometimes one of ours lost; and the engaging platform placed for each nominal time to effect
(`TTE_S`) and at each distance (`DIST_M`). Each contact moves by its behaviour (`MOTION`), and
the generator writes the facts a tracker would report for it at the nomination, with the
scenario runner's noise model (position error of the track's own 1-sigma, velocity error
`vel_sigma` per axis) and the bridge tracker's rules for what it reports (no velocity in its
first `VEL_KNOWN_S`, an average velocity over the last minute once tracked `MEAN_MIN_S`, a
manoeuvre when its velocity changed by more than the bridge's threshold within its memory,
`first_seen_t`). `murmur-keepout-probe` (eval/keepout_probe, read-only use of murmur-decide)
runs every evaluation through the engine's own nomination checks (`rules::evaluate` on the
engine's view of the world) under rules 6, 9 and 12, and lists each protected track's
keep-out regions (`engage::track_reach`) with how far each reaches into the keep-out.

**Labels (truth).** From the effect point and time the engine predicted, each contact's true
position then: a block is *needed* when a protected contact is truly inside the effect circle
(the effect radius) at the effect time, else *conservative* (nobody at risk; *buffer* when
someone is inside the keep-out but outside the effect circle). A *missed* danger is a pass
with a protected contact truly inside the effect circle.

**What it is not.** The situations come from stated distributions, not from the world: open
water with no land or channels, uniform traffic, surface contacts only, our tracker's
behaviour replaced by a stated model of its reports (no noise-driven manoeuvre flags, no
filter lag, no gaps or re-initialized tracks). The checks are the nomination's only (the
plan's re-checks while signatures wait and after approval are not run). No person decides
anything here.

    python -m eval.run keepout --seeds 1 2 3 4 5 6 7 8 9 10 --situations 100
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

PROBE_DIR = REPO / "eval" / "keepout_probe"
PROBE = REPO / "target" / "release" / "murmur-keepout-probe"
NOT_HUMAN = (
    "Model results from simulated geometry: generated situations from stated distributions, "
    "tracker reports from a stated model, every verdict the decision engine's own nomination "
    "checks. Not human data, not field data, and not reviewed."
)

# ---- the stated distributions -------------------------------------------------------------------

#: protected contacts per km^2, uniform over a disc of FIELD_M around the target (0.03: about
#: the 7 tracks of the 10-06 live run over the 8 km around the gate; 1: a busy harbour, a GUESS)
DENSITIES = (0.03, 0.1, 0.3, 1.0)
#: contacts are drawn this far from the target: beyond the farthest any keep-out region reaches
#: at the longest nominal time to effect (a young track at 25 m/s for 300 s, plus the keep-out)
FIELD_M = 8000.0
BEHAVIOURS = ("stationary", "transiting", "circling", "manoeuvring", "new", "mixed")
#: the mixed traffic's behaviour shares (a stated GUESS for a busy harbour)
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
#: a track's reported 1-sigma (m), uniform; its position estimate is off by that much per axis
SIGMA_P = (8.0, 25.0)
#: the velocity estimate's error per axis (m/s), the scenario runner's --vnoise; Phase G: 0.5
#: passed the engagement suite
VEL_SIGMA = 0.5
#: the bridge tracker (crates/murmur-bridge/src/tracks/model.rs): a velocity once its 1-sigma is
#: below 3 m/s (about 7 radar fixes at 15 m: a GUESS of 7 s), an average velocity over the last
#: minute once tracked 45 s, a manoeuvre when the velocity changed by more than 2 m/s and its
#: noise (chi-square 13.8 on two estimates' variances) within 300 s, remembered 300 s, its
#: speed the fastest in the memory plus two sigmas
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
#: the chance one of our vehicles is lost (its last report 15 to 300 s old, 2 to 8 km out)
LOST_SHARE = 0.1
#: where the target is: inside ez-gate-west, clear of the no-strike areas (eval/ident.py's box)
CENTRE_BOX = ((-122.520, -122.497), (37.806, 37.819))
#: engine time at the nomination (the scenario runner's T0)
T0_MS = 1_781_000_000_000
M_PER_DEG = 111_320.0
#: how far into the future and past the motions are laid out (s)
HORIZON_S = 900.0


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
        "motion": MOTION,
        "sigma_p": list(SIGMA_P),
        "tracked_s": list(TRACKED_S),
        "new_tracked_s": list(NEW_TRACKED_S),
    }


def cruise_speeds(root: Path = REPO) -> dict[str, float]:
    cat = tomllib.loads((root / "sim" / "platforms.toml").read_text())
    return {k: float(v["cruise_mps"]) for k, v in cat.items() if isinstance(v, dict) and "cruise_mps" in v}


# ---- geometry: the engine's flat approximation around the target --------------------------------


def to_lonlat(origin: tuple[float, float], e: float, n: float) -> list[float]:
    """`origin` moved `e` east and `n` north (engage::offset)."""
    lon, lat = origin
    return [lon + e / (M_PER_DEG * math.cos(math.radians(lat))), lat + n / M_PER_DEG]


def to_enu(origin: tuple[float, float], p: list[float] | tuple[float, float]) -> tuple[float, float]:
    """East and north metres of `p` from `origin` (engage::enu)."""
    return ((p[0] - origin[0]) * M_PER_DEG * math.cos(math.radians(origin[1])), (p[1] - origin[1]) * M_PER_DEG)


# ---- motions (truth), in metres from the target at the nomination, t in s from it ----------------


class Hold:
    def __init__(self, p: tuple[float, float]):
        self.p = p
        self.vmax = 0.0
        self.steady = True

    def pos(self, t: float) -> tuple[float, float]:
        return self.p

    def vel(self, t: float) -> tuple[float, float]:
        return (0.0, 0.0)


class Line:
    def __init__(self, p: tuple[float, float], v: tuple[float, float]):
        self.p, self.v = p, v
        self.vmax = math.hypot(*v)
        self.steady = True

    def pos(self, t: float) -> tuple[float, float]:
        return (self.p[0] + self.v[0] * t, self.p[1] + self.v[1] * t)

    def vel(self, t: float) -> tuple[float, float]:
        return self.v


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


# ---- what the tracker reports ---------------------------------------------------------------------


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
) -> dict[str, Any]:
    """The facts the tracker reports for a contact moving as `m` (murmur-decide's TrackFacts)."""
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
        "domain": "surface",
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


def situation(seed: int, density: float, index: int, behaviour: str, params: dict[str, Any], cruise: dict[str, float]) -> dict[str, Any]:
    """One situation: the probe's input (`probe`) and the truth behind it (`truth`). Paired:
    everything but the contacts' motion (and the draws that follow it) comes from the field's
    own stream, the same for every behaviour."""
    field = random.Random(f"keepout field {seed} {density:g} {index}")
    move = random.Random(f"keepout motion {seed} {density:g} {index} {behaviour}")
    origin = (field.uniform(*CENTRE_BOX[0]), field.uniform(*CENTRE_BOX[1]))
    vel_sigma = float(params["vel_sigma"])
    field_m = float(params["field_m"])
    sid = f"{seed}-{density:g}-{index}-{behaviour}"
    tracks: dict[str, Any] = {}
    # the target: holding, declared hostile by an operator, followed for 15 minutes
    tsig = field.uniform(*SIGMA_P)
    target = facts(Hold((0.0, 0.0)), 900.0, tsig, vel_sigma, field, origin, "hostile", 0.9)
    target.update({"declared_by": "op-1", "identity": "small_fast_boat", "identity_confidence": 0.8, "confidence": 0.95})
    tracks["T-0000"] = target
    truth: dict[str, Any] = {}
    n = _poisson(field, float(density) * math.pi * (field_m / 1000.0) ** 2)
    mix_names = [b for b, _ in MIX]
    mix_w = [w for _, w in MIX]
    kinds: dict[str, int] = defaultdict(int)
    for i in range(n):
        p = _disc(field, field_m)
        sigma = field.uniform(*SIGMA_P)
        aff = "neutral" if field.random() < 0.6 else "unknown"
        aff_c = field.uniform(0.3, 0.9)
        kind = move.choices(mix_names, mix_w)[0] if behaviour == "mixed" else behaviour
        kinds[kind] += 1
        m, tracked = motion(kind, p, move)
        cid = f"C-{i + 1:04d}"
        tracks[cid] = facts(m, tracked, sigma, vel_sigma, move, origin, aff, aff_c)
        truth[cid] = {"motion": m, "behaviour": kind, "tracked_s": tracked}
    # ours: the observing USV, holding or patrolling; sometimes another one lost
    vehicles = []
    ours: dict[str, Any] = {}
    p = _disc_ring(field, 1000.0, 3000.0)
    v = (0.0, 0.0) if field.random() < 0.5 else _vec(field.uniform(2.0, 6.0), _course(field))
    vehicles.append({"uid": "usv-1", "platform": "seahunter", "at": to_lonlat(origin, *p), "velocity": list(v), "report_age_s": 0.0})
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
    probe = {
        "id": sid,
        "t_ms": T0_MS,
        "target": "T-0000",
        "radius_m": float(params["radius_m"]),
        "fallback": "return",
        "tracks": tracks,
        "vehicles": vehicles,
        "evals": evals,
    }
    return {
        "id": sid,
        "seed": seed,
        "density": density,
        "index": index,
        "behaviour": behaviour,
        "origin": origin,
        "n_contacts": n,
        "kinds": dict(kinds),
        "probe": probe,
        "truth": truth,
        "ours": ours,
    }


def _disc_ring(rng: random.Random, lo: float, hi: float) -> tuple[float, float]:
    d, a = rng.uniform(lo, hi), _course(rng)
    return (d * math.sin(a), d * math.cos(a))


# ---- truth at the effect ------------------------------------------------------------------------------


def at_risk(sit: dict[str, Any], effect: list[float], t_s: float, radius: float, keep_out: float) -> dict[str, Any]:
    """Who is truly where at the effect time: protected contacts inside the effect circle
    (`danger`) and inside the keep-out (`near`, those included), ours inside the effect circle
    (`ours_danger`)."""
    e = to_enu(sit["origin"], effect)
    danger, near = [], []
    reach = keep_out + 1.0
    for cid, tr in sit["truth"].items():
        m = tr["motion"]
        p0 = m.pos(0.0)
        # cheap bound first: it cannot get closer than this by then
        if math.hypot(p0[0] - e[0], p0[1] - e[1]) - m.vmax * max(t_s, 0.0) > reach:
            continue
        p = m.pos(t_s)
        d = math.hypot(p[0] - e[0], p[1] - e[1])
        if d <= keep_out:
            near.append(cid)
            if d <= radius:
                danger.append(cid)
    ours = []
    for uid, m in sit["ours"].items():
        p = m.pos(t_s)
        if math.hypot(p[0] - e[0], p[1] - e[1]) <= radius:
            ours.append(uid)
    return {"danger": danger, "near": near, "ours_danger": ours}


# ---- running ------------------------------------------------------------------------------------------


def cargo() -> str:
    return os.environ.get("CARGO") or shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")


def build_probe() -> Path:
    """Build the probe into the repository's target directory (a no-op when it is current)."""
    cmd = [cargo(), "build", "--release", "--locked", "--manifest-path", str(PROBE_DIR / "Cargo.toml"), "--target-dir", str(REPO / "target")]
    p = subprocess.run(cmd, cwd=str(REPO), capture_output=True, text=True)
    if p.returncode != 0:
        raise RuntimeError(f"building the probe failed: {p.stderr.strip()[-2000:]}")
    return PROBE


def run_probe(inputs: list[dict[str, Any]], rules: Iterable[int] = RULES, probe: Path = PROBE) -> list[dict[str, Any]]:
    """Every evaluation of `inputs` through the probe: one result per (situation, evaluation)."""
    data = "\n".join(json.dumps(s, separators=(",", ":")) for s in inputs) + "\n"
    p = subprocess.run(
        [str(probe), "--root", str(REPO), "--rules", ",".join(str(r) for r in rules)],
        input=data,
        capture_output=True,
        text=True,
        cwd=str(REPO),
        timeout=3600,
    )
    if p.returncode != 0:
        raise RuntimeError(f"murmur-keepout-probe exited {p.returncode}: {p.stderr.strip()[-2000:]}")
    return [json.loads(line) for line in p.stdout.splitlines() if line.strip()]


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
    truth = at_risk(sit, pv["effect"], pv["tte_s"], float(params["radius_m"]), keep_out)
    out["danger"] = len(truth["danger"])
    out["near"] = len(truth["near"])
    out["ours_danger"] = len(truth["ours_danger"])
    out["danger_kinds"] = sorted({sit["truth"][c]["behaviour"] for c in truth["danger"]})
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
    results = run_probe([s["probe"] for s in sits], params["rules"], Path(probe))
    t_probe = time.monotonic() - t0 - t_gen
    by_id = {s["id"]: s for s in sits}
    records = [record(by_id[r["id"]], r, params) for r in results]
    return {
        "seed": seed,
        "density": density,
        "behaviour": behaviour,
        "records": records,
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
    """Every seed x density x behaviour chunk into `out`: `records.jsonl.gz` (one line per
    evaluation, every rules version in it) and `summary.json`."""
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
        raise RuntimeError(f"missing {probe}: build eval/keepout_probe")
    jobs = [(seed, float(d), b, situations, params, str(probe)) for seed in seeds for d in params["densities"] for b in params["behaviours"]]
    workers = workers or max(1, min(len(jobs), (os.cpu_count() or 2) // 2, 12))
    t0 = time.monotonic()
    chunks: list[dict[str, Any]] = []
    if workers == 1:
        chunks = [run_chunk(j) for j in jobs]
    else:
        with ProcessPoolExecutor(max_workers=workers) as pool:
            chunks = list(pool.map(run_chunk, jobs))
    chunks.sort(key=lambda c: (c["seed"], c["density"], BEHAVIOURS.index(c["behaviour"]) if c["behaviour"] in BEHAVIOURS else 99))
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
        "wall_s": round(time.monotonic() - t0, 1),
        "workers": workers,
        "timing_s": {k: round(sum(c["timing_s"][k] for c in chunks), 1) for k in ("generate", "probe", "label")},
    }
    summary = summarize(read_records(path), meta)
    summary["regions"] = region_sizes(params["rules"], probe)
    (out / "summary.json").write_text(json.dumps(summary, indent=1, default=str))
    return summary


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
    altitude, so the verdict is every region's)."""
    drop = {drop} if isinstance(drop, str) else set(drop)
    return all(m >= 0.0 for c, m in comp.items() if c not in drop)


def classify(r: dict[str, Any]) -> str:
    """A block on tracks: `needed` (a protected contact truly inside the effect circle at the
    effect time), `buffer` (inside the keep-out, outside the circle) or `clear` (nobody inside
    the keep-out). `buffer` and `clear` are the conservative blocks."""
    if r.get("danger"):
        return "needed"
    if r.get("near"):
        return "buffer"
    return "clear"


def _rate(k: int, n: int) -> dict[str, Any]:
    p, lo, hi = wilson(k, n)
    return {"k": k, "n": n, "rate": p, "lo": lo, "hi": hi}


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


def _boot_thresholds(rows: list[list[int]], grid: list[float], levels: tuple[float, ...], draws: int, seed: int) -> dict[float, tuple[float | None, float | None, float | None]]:
    """The thresholds on the pooled rows (one per situation: pass per grid point) and a
    percentile bootstrap over situations."""
    out = {}
    n = len(rows)
    if n == 0:
        return {lv: (None, None, None) for lv in levels}

    def curve(sample: list[list[int]]) -> list[tuple[float, float]]:
        return [(g, sum(r[i] for r in sample) / len(sample)) for i, g in enumerate(grid)]

    est = {lv: threshold(curve(rows), lv) for lv in levels}
    rng = random.Random(seed)
    boots: dict[float, list[float]] = {lv: [] for lv in levels}
    for _ in range(draws):
        sample = [rows[rng.randrange(n)] for _ in range(n)]
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
    can't be estimated, one observation per cluster."""
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
    return {"k": k_sum, "n": n_sum, "rate": r, "lo": lo, "hi": hi, "clusters": m, "deff": None if deff is None else round(deff, 2)}


def _field(sid: str) -> str:
    """The field a situation was drawn on (seed, density, index): the same under every
    behaviour."""
    return sid.rsplit("-", 1)[0]


def summarize(records: Iterable[dict[str, Any]], meta: dict[str, Any] | None = None, draws: int = 1000) -> dict[str, Any]:
    """The tables, from the records alone. Rates per cell (one evaluation per situation) carry a
    Wilson interval; shares pooled over a situation's evaluations a clustered one
    (`cluster_rate`); the keep-out on our vehicles counts each field once (it does not depend on
    the contacts' behaviour)."""
    meta = meta or {}
    tte_cells: dict[tuple, list[int]] = defaultdict(lambda: [0, 0, 0, 0, 0])  # n, kot, kof, any, missed
    seed_cells: dict[tuple, list[int]] = defaultdict(lambda: [0, 0])  # n, kot per seed
    dist_cells: dict[tuple, list[int]] = defaultdict(lambda: [0, 0, 0, 0])  # n, kot, kof, any
    # per (behaviour, rules), per situation: what its evaluations did
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
        for v, x in r["rules"].items():
            v = int(v)
            if x.get("mismatch"):
                counts["mismatches"] += 1
            for c in x.get("other", []):
                other[(r["kind"], v)][c] += 1
            other[(r["kind"], v)]["_n"] += 1
            if r["kind"] != "tte":
                cell = dist_cells[(b, d, r["platform"], r["nominal"], v)]
                cell[0] += 1
                cell[1] += x["kot"]
                cell[2] += x["kof"]
                cell[3] += int(x["kot"] or x["kof"])
                continue
            cell = tte_cells[(b, d, r["nominal"], v)]
            cell[0] += 1
            cell[1] += x["kot"]
            cell[2] += x["kof"]
            cell[3] += int(x["kot"] or x["kof"])
            cell[4] += int(not x["kot"] and r["danger"] > 0)
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
            # blocks on tracks: needed or conservative, and what reached in
            ps = per_sit[(b, v)][r["id"]]
            tc = by_tte_class[(b, r["nominal"], v)]
            tc["n"] += 1
            if not x["kot"]:
                ps["passes"] += 1
                ps["missed"] += int(r["danger"] > 0)
                continue
            cls = classify(r)
            ps["blocks"] += 1
            ps[cls] += 1
            tc["blocks"] += 1
            tc[cls] += 1
            if cls == "needed":
                continue
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
    rows_tte = []
    for (b, d, t, v), (n, kot, kof, anyb, missed) in sorted(tte_cells.items(), key=lambda kv: (_bkey(kv[0][0]), kv[0][1], kv[0][3], kv[0][2])):
        seeds = [k / n_ for (bb, dd, tt, vv, _), (n_, k) in seed_cells.items() if (bb, dd, tt, vv) == (b, d, t, v) and n_]
        m, lo, hi = t_interval(seeds)
        rows_tte.append(
            {
                "behaviour": b,
                "density": d,
                "tte_s": t,
                "rules": v,
                "tracks": _rate(kot, n),
                "tracks_seed_t": {"mean": m, "lo": lo, "hi": hi, "seeds": len(seeds)},
                "friendly": _rate(kof, n),
                "any": _rate(anyb, n),
                "missed": missed,
            }
        )
    rows_dist = []
    for (b, d, plat, dist, v), (n, kot, kof, anyb) in sorted(dist_cells.items(), key=lambda kv: (_bkey(kv[0][0]), kv[0][1], kv[0][2], kv[0][4], kv[0][3])):
        rows_dist.append({"behaviour": b, "density": d, "platform": plat, "dist_m": dist, "rules": v, "tracks": _rate(kot, n), "friendly": _rate(kof, n), "any": _rate(anyb, n)})
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
                "needed": share("needed", "blocks"),
                "conservative": share("cons", "blocks"),
                "buffer": sum(s.get("buffer", 0) for s in ss),
                "clear": sum(s.get("clear", 0) for s in ss),
                "passes": sum(s.get("passes", 0) for s in ss),
                "missed": share("missed", "passes"),
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
        class_rows.append({"behaviour": b, "tte_s": t, "rules": v, "n": c["n"], "blocks": c["blocks"], "needed": c["needed"], "buffer": c["buffer"], "clear": c["clear"]})
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


def _stable_seed(*parts: Any) -> int:
    return sum((i + 1) * ord(ch) for i, ch in enumerate(" ".join(str(p) for p in parts))) % 1_000_003


def _bkey(b: str) -> int:
    return BEHAVIOURS.index(b) if b in BEHAVIOURS else len(BEHAVIOURS)


def _median(xs: list[float]) -> float | None:
    if not xs:
        return None
    s = sorted(xs)
    k = len(s) // 2
    return round(s[k] if len(s) % 2 else (s[k - 1] + s[k]) / 2, 1)


def load_summary(batch: Path) -> dict[str, Any] | None:
    p = Path(batch) / "keepout" / "summary.json"
    if not p.exists():
        return None
    try:
        return json.loads(p.read_text())
    except ValueError:
        return None


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


def table(s: dict[str, Any]) -> str:
    """The summary as plain text."""
    lines = [s["label"], ""]
    grid = sorted({r["tte_s"] for r in s["tte"]})
    lines.append(f"{s['situations']} situations, {s['counts']['evaluations']} evaluations x rules {s.get('params', {}).get('rules')}; "
                 f"{s.get('contacts', '?')} contacts; mismatches {s['counts']['mismatches']}, errors {s['counts']['errors']}, no preview {s['counts']['no_preview']}")
    lines.append("")
    lines.append("Blocked by keep_out_tracks, % [Wilson 95%], by nominal time to effect (s):")
    for v in sorted({r["rules"] for r in s["tte"]}):
        lines.append(f"rules {v}")
        lines.append(f"{'behaviour':<13}{'per km2':>8}  " + "".join(f"{t:>12}" for t in grid))
        cells = {(r["behaviour"], r["density"], r["tte_s"]): r for r in s["tte"] if r["rules"] == v}
        for b in sorted({r["behaviour"] for r in s["tte"]}, key=_bkey):
            for d in sorted({r["density"] for r in s["tte"]}):
                row = [cells.get((b, d, t)) for t in grid]
                lines.append(f"{b:<13}{d:>8g}  " + "".join(f"{_pct(c['tracks'] if c else None):>12}" for c in row))
        lines.append("")
    lines.append("Blocks on tracks: needed (someone truly inside the effect circle) or conservative; passes that missed someone:")
    lines.append(f"{'behaviour':<13}{'rules':>6}{'blocks':>8}{'needed %':>16}{'conserv. %':>16}{'buffer':>8}{'clear':>8}{'passes':>8}{'missed':>8}")
    for r in s["blocks"]:
        lines.append(f"{r['behaviour']:<13}{r['rules']:>6}{r['blocks']:>8}{_pct(r['needed']):>16}{_pct(r['conservative']):>16}{r['buffer']:>8}{r['clear']:>8}{r['passes']:>8}{r['missed']['k']:>8}")
    lines.append("")
    lines.append("Conservative blocks: the component reaching in deepest (dominant) and would pass without it, %:")
    lines.append(f"{'behaviour':<13}{'rules':>6}{'cons.':>7}" + "".join(f"{c:>22}" for c in COMPONENTS) + f"{'w/o rules 9+12':>16}")
    for r in s["blocks"]:
        cons = r["conservative"]["k"]
        cbc = r["conservative_by_component"]
        cells = "".join(f"{_pct(cbc[c]['dominant'], False) + ' / ' + _pct(cbc[c]['pass_without'], False):>22}" for c in COMPONENTS)
        lines.append(f"{r['behaviour']:<13}{r['rules']:>6}{cons:>7}{cells}{_pct(r['conservative_pass_without_rules_9_12'], False):>16}")
    lines.append("")
    lines.append("Longest time to effect (s) with a pass on tracks at least 50% / 90% of the time [bootstrap 95%]:")
    lines.append(f"{'behaviour':<13}{'per km2':>8}{'rules':>6}{'50%':>20}{'90%':>20}")
    for r in s["thresholds"]:
        t50, t90 = r["t50"], r["t90"]
        lines.append(f"{r['behaviour']:<13}{r['density']:>8g}{r['rules']:>6}{_t(t50[0]) + ' [' + _t(t50[1]) + '-' + _t(t50[2]) + ']':>20}{_t(t90[0]) + ' [' + _t(t90[1]) + '-' + _t(t90[2]) + ']':>20}")
    lines.append("")
    if s.get("dist"):
        lines.append("Blocked by keep_out_tracks by platform and distance, %, rules " + " / ".join(str(v) for v in sorted({r["rules"] for r in s["dist"]})) + ":")
        dists = sorted({r["dist_m"] for r in s["dist"]})
        plats = sorted({r["platform"] for r in s["dist"]})
        cells = {(r["behaviour"], r["density"], r["platform"], r["dist_m"], r["rules"]): r for r in s["dist"]}
        versions = sorted({r["rules"] for r in s["dist"]})
        lines.append(f"{'behaviour':<13}{'per km2':>8}  " + "".join(f"{p[:6] + ' ' + str(d):>16}" for p in plats for d in dists))
        for b in sorted({r["behaviour"] for r in s["dist"]}, key=_bkey):
            for d in sorted({r["density"] for r in s["dist"]}):
                row = []
                for p in plats:
                    for dist in dists:
                        vals = [cells.get((b, d, p, dist, v)) for v in versions]
                        row.append("/".join(_pct(c["tracks"], False) if c else "-" for c in vals))
                lines.append(f"{b:<13}{d:>8g}  " + "".join(f"{x:>16}" for x in row))
        lines.append("")
    if s.get("regions"):
        lines.append("Keep-out regions the engine counts (radius m, keep_out_sigma included; centre offset m), rules 12, by span (s):")
        for q in s["regions"]:
            spans = q["rules"].get(str(max(int(k) for k in q["rules"])), [])
            lines.append(f"  {q['regions']}")
            comps = [c for c in COMPONENTS if any(c in x["regions"] for x in spans)]
            lines.append(f"    {'region':<12}" + "".join(f"{x['span_s']:>8g}" for x in spans))
            for c in comps:
                lines.append(f"    {c:<12}" + "".join(f"{x['regions'][c]['radius_m']:>8.0f}" if c in x["regions"] else f"{'-':>8}" for x in spans))
        lines.append("")
    return "\n".join(lines)
