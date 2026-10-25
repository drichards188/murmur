"""Scoring v3's first situation families (docs/WORLD-V3-PLAN.md section 6; eval/SCORING-V3.md
section 6): F1-F6 and F18, each on both of world v3's maps, golden-gate-v3 and san-diego. MODEL
RESULTS ONLY: situations from stated distributions, labels from generated truth, the operator a
scripted model.

`generate(name, seed, workdir)` gives one situation (an `EpisodeSpec`) and its contacts file, from
`random.Random("v3:<family>:<world>:<seed>")`; `name` is `v3-<family>-<gg|sd>` (`family_name`). The
base is the world's watch situation (sim/situations/golden-gate-v3-watch.toml, san-diego-watch.toml):
its fleet in every domain on its opening orders, its watch policy, the world's background traffic in
every domain (a contacts seed drawn per situation, so the traffic's motion varies) under the
physical sensing model, the world's land/water grid, currents and pinned element sets, the AI at
45 s, and the scoring v3 mission (mission.py) as the brief, verbatim and nothing else. The run id and
the operator's notes name no situation (`EpisodeSpec.opaque`).

**Conditions**: drawn from the world's ranges (`World.draw_conditions`) from
`random.Random("conditions:<world>:<seed>")`, so one seed meets the same day or night, fog, wind and
sea in every family of a world; F5 alone moves the start into the night (p 0.7, `night`).

**The families** (each adds one element to the base; every route is drawn again until it keeps to
its medium on the world's baked land/water grid: a boat in water deeper than its draft and margin,
a UUV and a diver deeper than their depth plus 2 m, a vehicle on land, an aircraft above the ground):

- **F1 fast boat, no AIS** (surface): a small fast boat, truth unknown, 10 to 18 m/s, entering the
  3 km circle from the sea (p 0.6) or from inside the bay (p 0.4) along the world's lanes; it holds
  0.8 to 2.4 km from the centre (p 0.7) or runs on through the circle (p 0.3). It sets out so that it
  reaches the circle 15 to 100 s after it appears (`F1_LEAD_S`), so a buoy's passive sonar, which hears
  a fast boat tens of kilometres off and identifies after 180 s of contact, cannot identify it by its
  deadline: the need is the cameras' (and a surface need's sensor is EO/IR, SCORING-V3.md 2.2).
- **F2 unknown drone inbound** (air; version 2): a small drone, truth unknown, no transponder (its
  datalink emits), 12 to 25 m/s at 60 to 150 m above the surface it crosses, from 3.8 to 5.5 km out to
  a target inside the circle (our boats' pier, the protected point, a shore post), where it hovers.
  By day only (local 08:00 to 17:00, `F2_DAY_ONLY`): version 1 drew the world's day and night, and no
  camera identified a drone at night (night is F9's). Version 3 brings it from bearings over the
  strait or the bay (Golden Gate: 250 to 275 deg or 60 to 110; San Diego: over Coronado, 100 to 150,
  or the sea, 170 to 230): version 2's drones from over the Marin Headlands, Lands End or Point
  Loma's ridge flew 130 to 440 m above the sea, and our small aircraft, whose orders cannot name an
  altitude, fly theirs at 150 m: the oracle could send nothing in 24 of those 28 situations and served
  1. A route of version 3 to a target behind a shore (the Horseshoe Bay pier, Fort Point) may still
  cross the headlands' or the Presidio's edge, up to 270 m (16 of its 60 dev situations).
- **F3 low aircraft, transponder off** (air): a light aircraft, truth unknown, no emitter, 45 to 60
  m/s at 150 to 400 m above the highest ground it crosses, on a straight line through the circle
  passing within 1.5 km of the centre, from 9 km out to 9 km out on the far side.
- **F4 UUV in the channel** (subsurface; version 2): a UUV, truth unknown, 1 to 2.5 m/s, 5 to 15 m
  deep (2 m above the shallowest seabed of its route at least), down the bay's lane toward the
  channel from 0.2 to 1.2 km outside the circle; it holds 0.5 to 2 km from the centre. Version 1
  brought 60 % in from the sea, past the channel's buoys and HUNTER 1's patrol, which recognised it
  before its need opened (`F4_SEA_SHARE`).
- **F5 diver at a pier** (subsurface): a diver, truth unknown, 0.3 to 0.6 m/s, 2 to 4 m deep, from
  200 to 450 m off a pier where our boats lie (Golden Gate: Horseshoe Bay; San Diego: HUNTER 1's
  berth or the submarine piers at Point Loma) to 20 to 40 m off it, where it stays; at night p 0.7.
- **F6 vehicle at a restricted shore** (ground): a car or a truck, truth unknown or suspect, 6 to 12
  m/s, along a shore road to a restricted point inside the circle (Golden Gate: Fort Point, the
  Horseshoe Bay pier, Battery Spencer; San Diego: the submarine base's gate, North Island's west
  shore), where it parks.
- **F18 quiet**: the world's traffic only (no contact of unknown, suspect or hostile truth
  affiliation): the cost of acting.
"""

from __future__ import annotations

import datetime as dt
import gzip
import json
import math
import random
import re
import struct
from dataclasses import dataclass, field
from functools import lru_cache
from pathlib import Path
from typing import Any, Callable

from eval.world import World, load_world

from ..situations import EpisodeSpec, from_situation
from .mission import DOMAINS, mission_sha256, mission_text

PREFIX = "v3-"
#: the worlds, by the short tag a family name carries
WORLDS = {"gg": "golden-gate-v3", "sd": "san-diego"}
WATCH = {"golden-gate-v3": "golden-gate-v3-watch", "san-diego": "san-diego-watch"}
#: F1: the boat reaches the circle this long after it appears, at most (a buoy's passive sonar
#: identifies after 180 s of contact; with the 60 s grace, 100 + 60 < 180)
F1_LEAD_S = (15.0, 100.0)
#: F4 (version 2, after the first dev pilot): the share of UUVs that come in from the sea. From the
#: sea a UUV passes the channel's buoys and HUNTER 1's patrol, whose passive and active looks
#: combine to recognise it before its need opens (5 of 6 `none` episodes on seeds 5000-5002), so
#: nothing is left for an agent; from inside the bay none of them hears it in its first minutes
F4_SEA_SHARE = 0.0
#: F2 (version 2, after the first dev check, seeds 5000-5029): by day only. At night no camera of
#: either fleet identifies a small drone unless it passes within about 200 m (the Puma's LWIR; the
#: FVR-90's 390 m, the airship's 580 m): no agent and no oracle identified one at night (0 of 25
#: situations), the single largest cause of its losses (`no_look@night`). Night is
#: F9's to test.
F2_DAY_ONLY = True
#: the episodes' length: the subsurface families need their 300 s grace and the time to come in
DURATION_S = {"f4-uuv": 1200.0, "f5-diver": 1200.0}
DEFAULT_DURATION_S = 900.0
CONTACT_ACCEL_MPS2 = 0.5  # murmur-contacts motion.rs: a contact speeds up from its spawn so
CONTACTS_START_S = 3.0  # the contacts process's start (situations.Calibration)
M = 111_320.0


# ---- geometry on a world ----------------------------------------------------------------------

def offset(p: tuple[float, float], bearing_deg: float, dist_m: float) -> tuple[float, float]:
    b = math.radians(bearing_deg)
    return (round(p[0] + dist_m * math.sin(b) / (M * math.cos(math.radians(p[1]))), 6),
            round(p[1] + dist_m * math.cos(b) / M, 6))


def dist_m(a: tuple[float, float], b: tuple[float, float]) -> float:
    lat = math.radians((a[1] + b[1]) / 2)
    return math.hypot((a[0] - b[0]) * M * math.cos(lat), (a[1] - b[1]) * M)


def bearing(a: tuple[float, float], b: tuple[float, float]) -> float:
    return math.degrees(math.atan2((b[0] - a[0]) * M * math.cos(math.radians(a[1])), (b[1] - a[1]) * M)) % 360.0


def lerp(a: tuple[float, float], b: tuple[float, float], f: float) -> tuple[float, float]:
    return (round(a[0] + f * (b[0] - a[0]), 6), round(a[1] + f * (b[1] - a[1]), 6))


def along(pts: list[tuple[float, float]], step_m: float = 20.0) -> list[tuple[float, float]]:
    out = [pts[0]]
    for a, b in zip(pts, pts[1:]):
        n = max(1, int(dist_m(a, b) / step_m))
        out += [lerp(a, b, k / n) for k in range(1, n + 1)]
    return out


def length(pts: list[tuple[float, float]]) -> float:
    return sum(dist_m(a, b) for a, b in zip(pts, pts[1:]))


def point_at(pts: list[tuple[float, float]], d: float) -> tuple[float, float]:
    """The point `d` metres along a polyline (clamped to its ends)."""
    for a, b in zip(pts, pts[1:]):
        L = dist_m(a, b)
        if d <= L:
            return lerp(a, b, d / L if L > 0 else 0.0)
        d -= L
    return pts[-1]


def cut(pts: list[tuple[float, float]], d0: float, d1: float | None = None) -> list[tuple[float, float]]:
    """The part of a polyline from `d0` to `d1` metres along it."""
    total = length(pts)
    d1 = total if d1 is None else min(d1, total)
    out = [point_at(pts, d0)]
    acc = 0.0
    for a, b in zip(pts, pts[1:]):
        acc += dist_m(a, b)
        if d0 < acc < d1:
            out.append(b)
    out.append(point_at(pts, d1))
    return [p for i, p in enumerate(out) if i == 0 or dist_m(p, out[i - 1]) > 1.0]


def first_within(pts: list[tuple[float, float]], center: tuple[float, float], radius_m: float,
                 step_m: float = 10.0) -> float | None:
    """How far along a polyline it first comes within `radius_m` of `center`."""
    d = 0.0
    for a, b in zip(pts, pts[1:]):
        L = dist_m(a, b)
        n = max(1, int(L / step_m))
        for k in range(n + 1):
            if dist_m(lerp(a, b, k / n), center) <= radius_m:
                return d + L * k / n
        d += L
    return None


def time_to(d: float, speed: float) -> float:
    """Seconds to cover `d` metres from rest at the contacts' acceleration, then at `speed`."""
    ramp = speed / CONTACT_ACCEL_MPS2
    ramp_d = 0.5 * CONTACT_ACCEL_MPS2 * ramp ** 2
    if d <= ramp_d:
        return math.sqrt(2.0 * d / CONTACT_ACCEL_MPS2)
    return ramp + (d - ramp_d) / speed


class Grid:
    """A baked grid (eval/headless/bake_world.py's format), bilinear-free: the nearest sample."""

    def __init__(self, path: Path):
        raw = gzip.decompress(path.read_bytes())
        nl = raw.index(b"\n")
        self.h = json.loads(raw[:nl])
        n = self.h["nlat"] * self.h["nlon"]
        self.v = struct.unpack(f"<{n}h", raw[nl + 1:nl + 1 + 2 * n])

    def at(self, p: tuple[float, float]) -> float | None:
        i = round((p[1] - self.h["lat0"]) / self.h["dlat"])
        j = round((p[0] - self.h["lon0"]) / self.h["dlon"])
        if not (0 <= i < self.h["nlat"] and 0 <= j < self.h["nlon"]):
            return None
        x = self.v[i * self.h["nlon"] + j]
        return None if x == self.h["nodata"] else x * self.h["scale"]


@lru_cache(maxsize=None)
def grids(world: str) -> tuple[Grid, Grid]:
    """(water depth: positive in water, the land/water grid; surface height) of a world."""
    w = load_world(world)
    assert w.land_water_grid is not None and w.surface_grid is not None
    return Grid(w.land_water_grid), Grid(w.surface_grid)


def wet(world: str, pts: list[tuple[float, float]], min_depth_m: float, step_m: float = 20.0) -> bool:
    depth, _ = grids(world)
    for p in along(pts, step_m):
        d = depth.at(p)
        if d is None or d < min_depth_m:
            return False
    return True


def min_depth(world: str, pts: list[tuple[float, float]], step_m: float = 20.0) -> float:
    depth, _ = grids(world)
    return min((depth.at(p) or -1e9) for p in along(pts, step_m))


def dry(world: str, pts: list[tuple[float, float]], step_m: float = 20.0) -> bool:
    depth, _ = grids(world)
    for p in along(pts, step_m):
        d = depth.at(p)
        if d is None or d > -0.5:
            return False
    return True


def top_surface(world: str, pts: list[tuple[float, float]], step_m: float = 50.0) -> float:
    _, surface = grids(world)
    return max((surface.at(p) or 0.0) for p in along(pts, step_m))


# ---- the worlds' places ----------------------------------------------------------------------------

@dataclass(frozen=True)
class Place:
    """One world's places for the families (every point [lon, lat]; checked on its grids by the
    tests): water lanes from outside the circle to its centre, boats' targets, piers, shore roads,
    drone targets and launch sectors."""

    sea_entry: tuple[float, float, float, float]  # bearing lo, hi, distance lo, hi from the centre
    sea_via: tuple[tuple[float, float], ...]  # from the sea entry to the centre
    bay_entry: tuple[tuple[float, float], ...]  # candidate starts inside the bay
    bay_via: tuple[tuple[float, float], ...]  # from inside the bay to the centre
    through: tuple[tuple[float, float], ...]  # past the centre, inbound from the sea, on through
    piers: tuple[tuple[str, tuple[float, float], tuple[float, float]], ...]  # name, pier, open-water bearings
    roads: tuple[tuple[str, tuple[tuple[float, float], ...]], ...]  # name, route ending at the restricted point
    drone_targets: tuple[tuple[str, tuple[float, float]], ...]
    drone_sectors: tuple[tuple[float, float], ...]  # bearings from the centre a drone comes from (F2 v3)


PLACES: dict[str, Place] = {
    "golden-gate-v3": Place(
        sea_entry=(245.0, 270.0, 4500.0, 6500.0),
        sea_via=((-122.5050, 37.8100),),  # the strait's mouth
        bay_entry=((-122.4060, 37.8230), (-122.4100, 37.8320), (-122.4150, 37.8150)),
        bay_via=((-122.4500, 37.8250),),  # the bay's axis, mid-channel
        through=((-122.4500, 37.8250), (-122.4180, 37.8300)),
        piers=(("the Horseshoe Bay pier", (-122.4760, 37.8305), (140.0, 230.0)),),
        roads=(
            ("Fort Point", ((-122.4440, 37.8060), (-122.4600, 37.8070), (-122.4680, 37.8085), (-122.4740, 37.8095),
                            (-122.4765, 37.8102))),
            ("the Horseshoe Bay pier", ((-122.4840, 37.8450), (-122.4800, 37.8380), (-122.4790, 37.8360),
                                        (-122.4775, 37.8340), (-122.4768, 37.8322))),
            ("Battery Spencer", ((-122.4840, 37.8450), (-122.4800, 37.8380), (-122.4795, 37.8320),
                                 (-122.4825, 37.8285))),
        ),
        drone_targets=(("the Horseshoe Bay pier", (-122.4760, 37.8305)), ("the bridge", (-122.4783, 37.8199)),
                       ("Fort Point", (-122.4765, 37.8102))),
        drone_sectors=((250.0, 275.0), (60.0, 110.0)),  # F2 v3: over the strait or the bay
    ),
    "san-diego": Place(
        sea_entry=(190.0, 235.0, 4500.0, 6500.0),
        sea_via=((-117.2330, 32.6650),),  # where the entrance channel meets the sea
        bay_entry=((-117.1800, 32.7150), (-117.1900, 32.7140), (-117.1850, 32.7120)),
        bay_via=((-117.2050, 32.7130), (-117.2220, 32.7090), (-117.2290, 32.7000)),
        through=((-117.2290, 32.7000), (-117.2220, 32.7090), (-117.2050, 32.7130)),
        piers=(("HUNTER 1's berth at Point Loma", (-117.2352, 32.6950), (60.0, 150.0)),
               ("the submarine piers at Point Loma", (-117.2370, 32.6900), (60.0, 140.0))),
        roads=(
            ("the submarine base's gate", ((-117.2455, 32.7100), (-117.2450, 32.6950), (-117.2420, 32.6920),
                                           (-117.2395, 32.6905))),
            ("North Island's west shore", ((-117.2000, 32.7060), (-117.2100, 32.7020), (-117.2200, 32.6960),
                                           (-117.2230, 32.6950))),
        ),
        drone_targets=(("HUNTER 1's berth", (-117.2352, 32.6950)), ("the bay entrance", (-117.2320, 32.6860)),
                       ("North Island's west shore", (-117.2230, 32.6950))),
        drone_sectors=((100.0, 150.0), (170.0, 230.0)),  # F2 v3: over Coronado or the sea
    ),
}


# ---- the draft --------------------------------------------------------------------------------------

@dataclass
class Draft:
    family: str
    world: str
    seed: int
    rng: random.Random
    stated: dict[str, Any]
    added: list[dict[str, Any]] = field(default_factory=list)
    night: bool = False
    # what a family changes in the spec once it is built (families_ops.py: the fleet, the brief,
    # the script, the duration, the episode's options): each called with (spec, workdir)
    post: list[Callable[[Any, Path], None]] = field(default_factory=list)

    @property
    def w(self) -> World:
        return load_world(self.world)

    @property
    def center(self) -> tuple[float, float]:
        return self.w.mission.center

    @property
    def place(self) -> Place:
        return PLACES[self.world]


def _retry(what: str, fn: Callable[[], Any], tries: int = 400) -> Any:
    for _ in range(tries):
        out = fn()
        if out is not None:
            return out
    raise RuntimeError(f"{what}: nothing valid in {tries} draws")


def lane_in(d: Draft, from_sea: bool, min_depth_m: float) -> list[tuple[float, float]] | None:
    """A route on water from outside the circle to the centre: from the sea (a drawn point in the
    world's sea sector, then its lane) or from inside the bay (a drawn start, then its lane)."""
    p = d.place
    c = d.center
    if from_sea:
        lo, hi, r0, r1 = p.sea_entry
        start = offset(c, d.rng.uniform(lo, hi), d.rng.uniform(r0, r1))
        route = [start, *p.sea_via, c]
    else:
        start = offset(p.bay_entry[d.rng.randrange(len(p.bay_entry))], d.rng.uniform(0, 360), d.rng.uniform(0, 300))
        route = [start, *p.bay_via, c]
    return route if wet(d.world, route, min_depth_m) else None


def hold_or_through(d: Draft, route: list[tuple[float, float]], from_sea: bool, hold_lo: float, hold_hi: float,
                    min_depth_m: float, p_hold: float = 0.7) -> tuple[list[tuple[float, float]], dict[str, Any]] | None:
    """The route's end: it holds `hold_lo` to `hold_hi` from the centre on its way in, or (from the
    sea, 1 - p_hold) runs on through the circle along the world's `through` lane."""
    c = d.center
    if d.rng.random() < p_hold or not from_sea:
        hold = d.rng.uniform(hold_lo, hold_hi)
        total = length(route)
        # the last point of the route `hold` metres from the centre, walking back from it
        k = None
        for s in range(int(total), 0, -10):
            if dist_m(point_at(route, s), c) >= hold:
                k = s
                break
        if k is None:
            return None
        out = cut(route, 0.0, float(k))
        return (out, {"holds_at_m": round(hold)}) if wet(d.world, out, min_depth_m) else None
    out = [*route, *d.place.through]
    return (out, {"runs_through": True}) if wet(d.world, out, min_depth_m) else None


def start_before(route: list[tuple[float, float]], center: tuple[float, float], radius_m: float,
                 before_m: float) -> list[tuple[float, float]] | None:
    """The route from `before_m` metres before its first point within the circle."""
    k = first_within(route, center, radius_m)
    if k is None or k < before_m:
        return None
    return cut(route, k - before_m)


def contact(cid: str, name: str, kind: str, domain: str, aff: str, speed: float, route: list[tuple[float, float]],
            start_s: float, alt_m: float = 0.0, emits: bool = False) -> dict[str, Any]:
    c: dict[str, Any] = {"id": cid, "name": name, "affiliation": aff, "domain": domain, "kind": kind,
                         "speed_mps": round(speed, 2), "motion": "transit",
                         "route": [[round(p[0], 6), round(p[1], 6)] for p in route], "start_s": round(start_s, 1)}
    if alt_m:
        c["alt_m"] = round(alt_m, 1)
    if emits:
        c["emits"] = True
    return c


# ---- the families -----------------------------------------------------------------------------------

def _f1_fast_boat(d: Draft) -> None:
    """F1 (module doc)."""
    speed = d.rng.uniform(10.0, 18.0)
    from_sea = d.rng.random() < 0.6
    lead = d.rng.uniform(*F1_LEAD_S)
    r = d.w.mission.radius_m

    def draw() -> Any:
        route = lane_in(d, from_sea, 3.0)
        if route is None:
            return None
        ended = hold_or_through(d, route, from_sea, 800.0, 2400.0, 3.0)
        if ended is None:
            return None
        full, how = ended
        # it appears `before` metres before the circle, so it reaches the circle `lead` s after
        lo, hi = 0.0, 3000.0
        for _ in range(40):
            mid = 0.5 * (lo + hi)
            if time_to(mid, speed) < lead:
                lo = mid
            else:
                hi = mid
        part = start_before(full, d.center, r, lo)
        return None if part is None else (part, how, lo)

    route, how, before = _retry("F1 boat", draw)
    start = d.rng.uniform(150.0, 400.0)
    d.added.append(contact("boat-1", "Fast boat without AIS", "small_fast_boat", "surface", "unknown", speed, route, start))
    d.stated["element"] = {"contact": "boat-1", "kind": "small_fast_boat", "domain": "surface",
                           "from": "sea" if from_sea else "bay", "speed_mps": round(speed, 2), "start_s": round(start, 1),
                           "appears_before_circle_m": round(before), "lead_s": round(lead, 1), **how}


def _f2_drone(d: Draft) -> None:
    """F2 (module doc)."""
    p = d.place
    speed = d.rng.uniform(12.0, 25.0)
    name, target = p.drone_targets[d.rng.randrange(len(p.drone_targets))]
    lo, hi = p.drone_sectors[d.rng.randrange(len(p.drone_sectors))]
    start_pt = offset(d.center, d.rng.uniform(lo, hi), d.rng.uniform(3800.0, 5500.0))
    route = [start_pt, offset(target, d.rng.uniform(0, 360), d.rng.uniform(0, 80))]
    above = d.rng.uniform(60.0, 150.0)
    alt = top_surface(d.world, route) + above
    start = d.rng.uniform(150.0, 400.0)
    d.added.append(contact("drone-1", "Small drone without a transponder", "small_drone", "air", "unknown", speed,
                           route, start, alt_m=alt, emits=True))
    d.stated["element"] = {"contact": "drone-1", "kind": "small_drone", "domain": "air", "target": name,
                           "speed_mps": round(speed, 2), "alt_m": round(alt, 1), "above_m": round(above, 1),
                           "start_s": round(start, 1), "from_bearing_deg": round(bearing(d.center, start_pt), 1),
                           "from_m": round(dist_m(d.center, start_pt))}


def _f3_aircraft(d: Draft) -> None:
    """F3 (module doc)."""
    speed = d.rng.uniform(45.0, 60.0)
    b = d.rng.uniform(0.0, 360.0)
    miss = d.rng.uniform(-1500.0, 1500.0)
    mid = offset(d.center, (b + 90.0) % 360.0, miss)
    a = offset(mid, (b + 180.0) % 360.0, 9000.0)
    z = offset(mid, b, 9000.0)
    route = [a, z]
    above = d.rng.uniform(150.0, 400.0)
    alt = top_surface(d.world, route, 100.0) + above
    k = first_within(route, d.center, d.w.mission.radius_m) or 0.0
    t_in = d.rng.uniform(250.0, 600.0)
    start = max(60.0, t_in - time_to(k, speed))
    d.added.append(contact("aircraft-1", "Light aircraft, transponder off", "light_aircraft", "air", "unknown", speed,
                           route, start, alt_m=alt))
    d.stated["element"] = {"contact": "aircraft-1", "kind": "light_aircraft", "domain": "air",
                           "heading_deg": round(b, 1), "passes_within_m": round(abs(miss)), "speed_mps": round(speed, 2),
                           "alt_m": round(alt, 1), "above_m": round(above, 1), "start_s": round(start, 1)}


def _f4_uuv(d: Draft) -> None:
    """F4 (module doc)."""
    speed = d.rng.uniform(1.0, 2.5)
    from_sea = d.rng.random() < F4_SEA_SHARE
    r = d.w.mission.radius_m
    want_depth = d.rng.uniform(5.0, 15.0)

    def draw() -> Any:
        route = lane_in(d, from_sea, 6.0)
        if route is None:
            return None
        ended = hold_or_through(d, route, from_sea, 500.0, 2000.0, 6.0, p_hold=1.0)
        if ended is None:
            return None
        full, how = ended
        part = start_before(full, d.center, r, d.rng.uniform(200.0, 1200.0))
        if part is None:
            return None
        floor = min_depth(d.world, part)
        depth = min(want_depth, floor - 2.0)
        return None if depth < 3.0 else (part, how, depth)

    route, how, depth = _retry("F4 UUV", draw)
    start = d.rng.uniform(60.0, 200.0)
    d.added.append(contact("uuv-1", "Unmanned underwater vehicle", "uuv", "subsurface", "unknown", speed, route, start,
                           alt_m=-depth))
    d.stated["element"] = {"contact": "uuv-1", "kind": "uuv", "domain": "subsurface", "from": "sea" if from_sea else "bay",
                           "speed_mps": round(speed, 2), "depth_m": round(depth, 1), "start_s": round(start, 1), **how}


def _f5_diver(d: Draft) -> None:
    """F5 (module doc)."""
    p = d.place
    speed = d.rng.uniform(0.3, 0.6)
    name, pier, (b0, b1) = p.piers[d.rng.randrange(len(p.piers))]

    def draw() -> Any:
        b = d.rng.uniform(b0, b1)
        end = offset(pier, b, d.rng.uniform(20.0, 40.0))
        start_pt = offset(pier, b + d.rng.uniform(-25.0, 25.0), d.rng.uniform(200.0, 450.0))
        route = [start_pt, end]
        if not wet(d.world, route, 1.0):
            return None
        floor = min_depth(d.world, route)
        depth = min(d.rng.uniform(2.0, 4.0), floor - 1.0)
        return None if depth < 1.0 else (route, depth, b)

    route, depth, b = _retry("F5 diver", draw)
    start = d.rng.uniform(200.0, 500.0)
    d.added.append(contact("diver-1", "Diver", "diver", "subsurface", "unknown", speed, route, start, alt_m=-depth))
    d.stated["element"] = {"contact": "diver-1", "kind": "diver", "domain": "subsurface", "pier": name,
                           "speed_mps": round(speed, 2), "depth_m": round(depth, 1), "start_s": round(start, 1),
                           "from_m": round(dist_m(route[0], pier)), "night": d.night}


def _f6_vehicle(d: Draft) -> None:
    """F6 (module doc)."""
    p = d.place
    name, road = p.roads[d.rng.randrange(len(p.roads))]
    kind = "vehicle" if d.rng.random() < 0.7 else "truck"
    aff = "unknown" if d.rng.random() < 0.6 else "suspect"
    speed = d.rng.uniform(6.0, 12.0)

    def draw() -> Any:
        route = [tuple(q) for q in road]
        route = [route[0], *[(q[0], q[1]) for q in route[1:-1]], offset(route[-1], d.rng.uniform(0, 360), d.rng.uniform(0, 30))]
        return route if dry(d.world, route) else None

    route = _retry("F6 road", draw)
    start = d.rng.uniform(150.0, 400.0)
    d.added.append(contact("vehicle-1", f"{'Car' if kind == 'vehicle' else 'Truck'} at a restricted shore", kind,
                           "ground", aff, speed, route, start))
    d.stated["element"] = {"contact": "vehicle-1", "kind": kind, "domain": "ground", "affiliation": aff,
                           "restricted": name, "speed_mps": round(speed, 2), "start_s": round(start, 1)}


def _f18_quiet(d: Draft) -> None:
    """F18 (module doc)."""
    d.stated["needs"] = "none: no contact of unknown, suspect or hostile truth affiliation"


GENERATORS: dict[str, tuple[str, Callable[[Draft], None]]] = {
    "f1-fast-boat": ("F1 fast boat, no AIS", _f1_fast_boat),
    "f2-drone": ("F2 unknown drone inbound", _f2_drone),
    "f3-low-aircraft": ("F3 low aircraft, transponder off", _f3_aircraft),
    "f4-uuv": ("F4 UUV in the channel", _f4_uuv),
    "f5-diver": ("F5 diver at a pier", _f5_diver),
    "f6-shore-vehicle": ("F6 vehicle at a restricted shore", _f6_vehicle),
    "f18-quiet": ("F18 quiet", _f18_quiet),
}
FAMILIES = tuple(GENERATORS)
#: the domain each family's need is in (F18 has none)
FAMILY_DOMAIN = {"f1-fast-boat": "surface", "f2-drone": "air", "f3-low-aircraft": "air", "f4-uuv": "subsurface",
                 "f5-diver": "subsurface", "f6-shore-vehicle": "ground", "f18-quiet": None}


#: the worlds a family runs on, by tag, when not both maps (F17: the scale world only)
FAMILY_WORLDS: dict[str, tuple[str, ...]] = {}


def family_name(family: str, world: str) -> str:
    tag = next(k for k, v in WORLDS.items() if v == world)
    return f"{PREFIX}{family}-{tag}"


def parse(name: str) -> tuple[str, str]:
    """(family, world) of `v3-<family>-<gg|sd>`."""
    if not name.startswith(PREFIX):
        raise ValueError(f"not a v3 family name: {name!r}")
    body = name[len(PREFIX):]
    fam, _, tag = body.rpartition("-")
    if fam not in GENERATORS or tag not in WORLDS:
        raise ValueError(f"unknown v3 family {name!r}: v3-<{'|'.join(GENERATORS)}>-<{'|'.join(WORLDS)}>")
    return fam, WORLDS[tag]


def all_names() -> list[str]:
    return [f"{PREFIX}{f}-{tag}" for f in GENERATORS for tag in FAMILY_WORLDS.get(f, ("gg", "sd"))]


#: the local hours a family's start may be moved into: night 21:00 to 05:00 (F5), day 08:00 to
#: 17:00 (F2 version 2)
LIGHT_HOURS = {"night": (21, 5), "day": (8, 17)}


def local_at(rng: random.Random, w: World, light: str) -> str:
    """A start in the world's conditions window whose local hour is in `LIGHT_HOURS[light]`."""
    lo, hi = LIGHT_HOURS[light]
    a, b = (dt.datetime.fromisoformat(x.replace("Z", "+00:00")) for x in w.conditions["at"])
    off = dt.timedelta(hours=float(w.conditions.get("utc_offset_h", 0)))
    for _ in range(1000):
        t = a + (b - a) * rng.random()
        h = (t + off).hour
        if (lo <= h < hi) if lo < hi else (h >= lo or h < hi):
            return dt.datetime.fromtimestamp(60.0 * math.floor(t.timestamp() / 60.0), dt.timezone.utc).strftime(
                "%Y-%m-%dT%H:%M:%SZ")
    raise RuntimeError(f"no {light} in the world's window")


def night_at(rng: random.Random, w: World) -> str:
    """A start in the world's conditions window whose local time is between 21:00 and 05:00."""
    return local_at(rng, w, "night")


def conditions(world: str, seed: int, light: str | None, rng: random.Random) -> dict[str, Any]:
    """The situation's conditions: the world's draw for this seed (the same in every family of the
    world), its start moved into the night or the day when the family asks."""
    w = load_world(world)
    c = w.draw_conditions(random.Random(f"conditions:{world}:{int(seed)}"))
    if light:
        c["at"] = local_at(rng, w, light)
        if "utc_offset_h" in w.conditions:
            t = dt.datetime.fromisoformat(c["at"].replace("Z", "+00:00")) + dt.timedelta(
                hours=float(w.conditions["utc_offset_h"]))
            c["local_time"] = t.strftime("%H:%M")
        c[f"{light}_by_family"] = True
    return c


def contacts_file(world: str, seed: int, added: list[dict[str, Any]], path: Path) -> None:
    """The world's traffic file with this situation's contacts seed and its added contacts."""
    from ..families import _toml_value

    w = load_world(world)
    assert w.traffic is not None
    text = w.traffic.read_text()
    text = re.sub(r"(?m)^seed = \d+$", f"seed = {int(seed)}", text, count=1)
    text = re.sub(r'(?m)^title = ".*"$', 'title = "traffic"', text, count=1)
    out = [text.rstrip("\n"), ""]
    for c in added:
        out += ["[[contact]]"] + [f"{k} = {_toml_value(v)}" for k, v in c.items()] + [""]
    path.write_text("\n".join(out))


def generate(name: str, seed: int, workdir: Path) -> tuple[EpisodeSpec, Path]:
    """One situation of `name` (`v3-<family>-<gg|sd>`) for `seed`: its spec and contacts file."""
    family, world = parse(name)
    label, gen = GENERATORS[family]
    rng = random.Random(f"v3:{family}:{world}:{int(seed)}")
    e = from_situation(WATCH[world])
    stated: dict[str, Any] = {
        "family": family, "world": world, "seed": int(seed), "scoring": "v3", "area": label,
        "mission_sha256": mission_sha256(world), "domain": FAMILY_DOMAIN[family],
        "rule": "eval/headless/v3/mission.py: unknown, suspect or hostile within 3 km, identified in time per domain",
    }
    d = Draft(family=family, world=world, seed=int(seed), rng=rng, stated=stated)
    if family == "f5-diver":
        d.night = rng.random() < 0.7
    gen(d)
    light = "night" if d.night else ("day" if family == "f2-drone" and F2_DAY_ONLY else None)
    cond = conditions(world, int(seed), light, rng)
    contacts_seed = rng.randrange(1, 2 ** 31)
    workdir.mkdir(parents=True, exist_ok=True)
    path = workdir / f"{name}-{int(seed)}.contacts.toml"
    contacts_file(world, contacts_seed, d.added, path)
    stated["contacts_seed"] = contacts_seed
    stated["conditions"] = cond
    stated["added"] = [c["id"] for c in d.added]
    e.name = f"{name}-{int(seed)}"
    e.family = name
    e.split = "v3"
    e.situation_seed = int(seed)
    e.seed = int(seed)
    e.contacts = path
    e.conditions = cond
    e.at = cond["at"]
    e.duration_s = DURATION_S.get(family, DEFAULT_DURATION_S)
    e.truth = stated
    e.config_name = "watch"
    e.brief = mission_text(world)
    e.opaque = True
    for f in d.post:
        f(e, workdir)
    return e, path


def needs_of(e: EpisodeSpec) -> list[dict[str, Any]]:
    """The added contacts a situation states, with the domain rule each is held to."""
    out = []
    for c in (e.truth or {}).get("added") or []:
        dom = (e.truth.get("element") or {}).get("domain")
        out.append({"contact": c, "domain": dom, "grace_s": DOMAINS[dom].grace_s if dom in DOMAINS else None})
    return out


# F14-F17 (families_ops.py) add themselves to the registries above
from . import families_ops as _ops  # noqa: E402

_ops.register()
