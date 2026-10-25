"""Scoring v3's situation families F7-F13 (docs/WORLD-V3-PLAN.md section 6; eval/SCORING-V3.md
sections 6.1 and 14), each on both of world v3's maps, golden-gate-v3 and san-diego. MODEL RESULTS
ONLY: situations from stated distributions, labels from generated truth, the operator a scripted
model.

Built as F1-F6 are (families.py: the world's watch situation as the base, its fleet on its opening
orders, its traffic, grids, currents and element sets, the scoring v3 mission as the brief, the run
id and the operator's notes naming no situation), from `random.Random("v3:<family>:<world>:<seed>")`,
with the world's conditions drawn by the seed (`random.Random("conditions:<world>:<seed>")`, the same
in every family of a world) and changed only where a family says so (`<what>_by_family` in the
record). The elements are F1-F6's own draws (families.py `_f1_fast_boat` ... `_f6_vehicle`: every
route kept to its medium on the world's baked grids), re-timed where a family needs them at a time,
renamed where a family has more than one; what each family adds:

- **F7 three at once** (air, surface, subsurface; 1,200 s): F4's UUV (version 2, from inside the
  bay), timed to come within the circle 300 to 650 s in; F1's boat and an air element each timed to
  come within the circle 90 s before to 150 s after the UUV: F2's drone (version 3) by day (local
  08:00 to 17:00) with p 0.5, else F3's light aircraft (no camera of either fleet identifies a small
  drone at night, SCORING-V3.md choice 11). Three needs open together, in three domains, served by
  overlapping vehicles (the aircraft's cameras serve the boat and the air contact, Sea Hunter's
  ring the boat and its hull sonar the UUV).
- **F8 decoys and AIS spoofing** (restraint; 900 s, 1,200 s when the need is under water): one need,
  its domain drawn (`F8_NEED_DOMAIN`): on the surface (p 0.4), with p 0.6 a **spoofer** (a small
  fast boat, truth suspect, broadcasting a fishing vessel's AIS identity, `ais.spoofed`, at 4 to
  6.5 m/s, a speed a fishing vessel makes, so the picture shows it a consistent, neutral fishing
  vessel until a camera identifies it: `F8_SPOOFER_SPEED`), else F1's boat; in the air (p 0.2) F2's
  drone by day with p 0.5, else F3's aircraft; under water (p 0.2) F4's UUV or F5's diver (p 0.5
  each); on the ground (p 0.2) F6's vehicle. And five look-alikes of truth neutral affiliation,
  each within the circle 150 s either side of the need:
  on the surface a fishing boat (p 0.5, 3 to 7 m/s) or a sailboat (2 to 4 m/s) without AIS, and a
  boat whose AIS looks wrong: a fishing boat broadcasting a cloned identity (`spoofed`, a cargo
  ship's or a passenger vessel's, p 0.5: the picture shows the conflict once a camera recognises
  it) or a sportfisher whose AIS says fishing at 10 to 14 m/s (the picture shows it suspect at
  once, for its speed); in the air a hobby drone by day (p 0.5) or a light aircraft with its
  transponder off; under water a research UUV or a diver at work (p 0.5 each); on the ground a car
  or a truck on another of the shore roads. AIS exists only on the surface in the model (no
  ADS-B), so the spoofing is the surface's.
- **F9 night and fog** (900 s): one need, F1's boat (p 0.4), an air element (p 0.3: F2's drone or
  F3's aircraft, p 0.5 each) or F6's vehicle (p 0.3); the light drawn toward the hard end: night
  (local 21:00 to 05:00, p 0.45), twilight with the sun 0 to 10 degrees up when the need comes
  within the circle (the thermal crossover, p 0.25; `sun_elevation_deg`) or day (p 0.3); fog
  (visibility log-uniform from the world's lowest to 2 km) always by day and with p 0.5 otherwise.
- **F10 rising sea state** (900 s): the sea state drawn from the world's top three (Golden Gate 3
  to 5, San Diego 2 to 4) and the wind from the upper half of its range; F1's boat with its speed
  capped by the sea (`F10_TOP_SPEED`, [E here]: a planing hull slows in a seaway), and with p 0.5 a
  second small boat, slower (3 to 6 m/s), from the other approach, within the circle 60 s before
  to 240 s after the first. The model's conditions hold for the episode: "rising" is the family's
  draw toward the sea's top, not a change during the episode.
- **F11 link trouble** (900 s; 1,200 s for the AUV): with p 0.6 an **RF jammer** (a circle 1.2 to
  2.5 km around a point of the need's way inside the mission circle, from 60 to 240 s before the
  need comes within the circle for 400 to 800 s: line-of-sight and mesh links jammed, SATCOM losing
  half its packets and LEO terminals falling back to an Iridium-class 700 ms, as
  sim/link-events/golden-gate.toml's jammer and sim/links.toml's satcom_iridium, both GUESS), its
  need F1's boat or an air element (p 0.5 each); else **the AUV must surface**: the acoustic
  channel closed over a circle 2 to 3 km around the AUV's survey line from 200 to 400 s before F4's
  UUV comes within the circle, for 150 to 300 s (the emulator's own `cut` on the `acoustic` class):
  REMUS falls silent, surfaces on its 120 s lost-link timer and reports at the surface over its
  Iridium-class link (acoustic_surface, 700 ms), orders to it waiting in the Queue meanwhile.
- **F12 GNSS jam or spoof** (900 s): one need (F1's boat p 0.4, an air element p 0.3, F6's vehicle
  p 0.3); with p 0.65 a GNSS **jammer** denying the fix in a circle 0.8 to 2 km around a point of the
  need's way inside the mission circle, from 60 to 240 s before the need comes within it to the end
  (A2's `gnss` fault: copters and the airship land, boats and UGVs hold, fixed wings fly on dead
  reckoning, the subs keep their DVL), else a **spoofer** capturing the standing vehicle best placed
  for the need (the nearest whose opening order is a look in the need's domain) 30 to 200 s before
  the need, walking its position off at 1 to 3 m/s on a drawn bearing, up to 300 to 1,000 m.
- **F13 vehicle faults across domains** (900 s; 1,200 s for the UUV): one need (F1's boat p 0.4,
  an air element p 0.3, F6's vehicle p 0.15, F4's UUV p 0.15) and faults in three domains: a
  **breakdown** of the vehicle flying the agent's first look in the need's domain (v2's stuck
  vehicle: stopped 5 to 60 s after its dispatch; none dispatched by the need's deadline, the
  standing vehicle best placed for the need stops then); a **battery** fault on a standing aircraft
  or UGV on its opening order (PUMA 2, KESTREL 1, SKY 2, FERRET 1, and PUMA 3 in San Diego; not
  that vehicle) 120 to 300 s in, a share of its pack gone at once to leave a drawn 22 to 33 % and,
  where its endurance needs it, a heavier draw, so its true battery crosses the 20 % reserve 1.5 to
  3 times its way home later (version 2, `home_s`: a prompt rtl brings it home in time, a late one
  does not; the low-reserve fault need, SCORING-V3.md 2.4); and with p 0.7 a **reboot** (3 to
  8 s silent) of another standing boat on its patrol up to 120 s before the need, which boots
  disarmed and drops its task.
"""

from __future__ import annotations

import datetime as dt
import math
import random
import tomllib
from dataclasses import dataclass, field
from functools import lru_cache
from pathlib import Path
from typing import Any, Callable

from eval.world import load_world

from .. import REPO
from ..situations import EpisodeSpec, at_unix, fleet_plan, from_situation
from . import families as F
from .mission import mission_sha256, mission_text

#: an element's earliest start, s (the AI starts at 45 s; the contacts at 3)
MIN_START_S = 20.0
#: a UUV (F4's) comes within the circle in this window wherever a family times it, so its need's
#: 300 s grace ends inside a 1,200 s episode (F4's own draws bring some in after 1,200 s)
UUV_ENTRY_S = (250.0, 650.0)
#: F7: when the UUV comes within the circle, and the others around it
F7_UUV_ENTRY_S = (300.0, 650.0)
F7_AROUND_S = (-90.0, 150.0)
#: F8: the spoofer's speed (a fishing vessel's, under the tracker's plausible top for one, 8 m/s,
#: by its speed noise: at 8 m/s the tracker shows the AIS's speed conflict now and then)
F8_SPOOFER_SPEED = (4.0, 6.5)
#: F8: the need's domain, and the share of surface needs that are the spoofer (a quarter of all)
F8_NEED_DOMAIN = (("surface", 0.4), ("air", 0.2), ("subsurface", 0.2), ("ground", 0.2))
F8_SPOOFER_SHARE = 0.6
F8_DECOY_AROUND_S = (-150.0, 150.0)
#: F9: the light's shares (night, twilight with the sun 0 to 10 degrees up, day) and fog's
F9_LIGHT = (("night", 0.45), ("twilight", 0.25), ("day", 0.30))
F9_FOG_KM_TOP = 2.0
F9_TWILIGHT_SUN_DEG = (0.0, 10.0)
#: F10: a small boat's top speed in a sea state, m/s ([E here]: a planing hull slows in a seaway;
#: F1 draws 10 to 18 m/s in any sea)
F10_TOP_SPEED = {0: 18.0, 1: 18.0, 2: 18.0, 3: 15.0, 4: 12.0, 5: 10.0}
#: F11: the RF jammer's share; its effects (sim/link-events/golden-gate.toml's jammer: SATCOM half
#: lost, GUESS; a LEO terminal falling back to Iridium: sim/links.toml satcom_iridium's 700 ms)
F11_RF_SHARE = 0.6
F11_SATCOM_LOSS = 0.5
F11_IRIDIUM_MS = 700.0
#: F12: the jammer's share
F12_JAM_SHARE = 0.65
#: F13: the battery fault's vehicles (a standing aircraft or UGV on its opening order)
F13_BATTERY = {"golden-gate-v3": ("PUMA 2", "KESTREL 1", "SKY 2", "FERRET 1"),
               "san-diego": ("PUMA 2", "PUMA 3", "KESTREL 1", "SKY 2", "FERRET 1")}
#: F13 version 2 (after the dev check, seeds 5300-5319: version 1's crossing 150 to 400 s after the
#: fault left 16 of 40 battery faults that no prompt rtl could have met, KESTREL 1 and the Pumas far
#: out): the fault 120 to 300 s in, and the reserve crossed 1.5 to 3 times the vehicle's way home
#: after it (`home_s`), at least 60 s before the end
F13_FAULT_S = (120.0, 300.0)
F13_SLACK = (1.5, 3.0)
F13_END_MARGIN_S = 60.0
#: what landing or stopping at home takes after the way there, s ([E here]: a Puma's AUTOLAND took
#: 70 to 100 s from its arrival over home in the smoke episodes, a copter's RTL descent less)
F13_LAND_S = {"plane": 120.0, "vtol": 90.0, "copter": 60.0, "ugv": 15.0}
F13_REBOOT_SHARE = 0.7
F13_REBOOT = {"golden-gate-v3": ("HUNTER 1", "MARLIN 1", "ALBATROSS 1"),
              "san-diego": ("HUNTER 1", "MARLIN 1", "MARLIN 2", "ALBATROSS 1")}

#: the element generators of F1-F6 and the id each gives its contact
ELEMENTS: dict[str, tuple[Callable[[F.Draft], None], str]] = {
    "boat": (F._f1_fast_boat, "boat-1"),
    "drone": (F._f2_drone, "drone-1"),
    "aircraft": (F._f3_aircraft, "aircraft-1"),
    "uuv": (F._f4_uuv, "uuv-1"),
    "diver": (F._f5_diver, "diver-1"),
    "vehicle": (F._f6_vehicle, "vehicle-1"),
}
DOMAIN_OF = {"boat": "surface", "drone": "air", "aircraft": "air", "uuv": "subsurface", "diver": "subsurface",
             "vehicle": "ground"}


# ---- the sun (for F9's crossover) ---------------------------------------------------------------

def sun_elevation_deg(t_unix: float, lat: float, lon: float) -> float:
    """The sun's elevation, degrees, by the Astronomical Almanac's low-precision formula (as the
    sensing model has it, docs/SENSING.md; refraction ignored)."""
    d = t_unix / 86400.0 + 2440587.5 - 2451545.0
    g = math.radians((357.529 + 0.98560028 * d) % 360.0)
    q = (280.459 + 0.98564736 * d) % 360.0
    lam = math.radians((q + 1.915 * math.sin(g) + 0.020 * math.sin(2.0 * g)) % 360.0)
    eps = math.radians(23.439 - 0.00000036 * d)
    ra = math.atan2(math.cos(eps) * math.sin(lam), math.cos(lam))
    dec = math.asin(math.sin(eps) * math.sin(lam))
    gmst = (18.697374558 + 24.06570982441908 * d) % 24.0
    ha = math.radians((gmst * 15.0 + lon) % 360.0) - ra
    la = math.radians(lat)
    return math.degrees(math.asin(math.sin(la) * math.sin(dec) + math.cos(la) * math.cos(dec) * math.cos(ha)))


# ---- the draft -----------------------------------------------------------------------------------

@dataclass
class DraftX(F.Draft):
    """F1-F6's draft, and what F7-F13 add: the elements' statements, the script (faults), the
    link events, the conditions' changes and the episode's length."""

    elements: list[dict[str, Any]] = field(default_factory=list)
    decoys: list[dict[str, Any]] = field(default_factory=list)
    script: list[dict[str, Any]] = field(default_factory=list)
    events: list[dict[str, Any]] = field(default_factory=list)
    cond: dict[str, Any] = field(default_factory=dict)
    duration_s: float = F.DEFAULT_DURATION_S

    @property
    def is_day(self) -> bool:
        """Whether the drawn start is in the day F2 flies in (local 08:00 to 17:00)."""
        lt = str(self.cond.get("local_time") or "12:00")
        lo, hi = F.LIGHT_HOURS["day"]
        return lo <= int(lt[:2]) < hi


def travel_s(c: dict[str, Any], center: tuple[float, float], radius_m: float) -> float:
    """From a contact's start to its coming within the circle, s (0 when it starts inside)."""
    route = [tuple(p) for p in c["route"]]
    k = F.first_within(route, center, radius_m)
    if k is None:
        raise ValueError(f"{c['id']}: never within the circle")
    return F.time_to(k, float(c["speed_mps"]))


def entry_s(d: DraftX, c: dict[str, Any]) -> float:
    return float(c["start_s"]) + travel_s(c, d.center, d.w.mission.radius_m)


def element(d: DraftX, what: str, cid: str | None = None, entry: float | None = None, decoy: bool = False,
            **change: Any) -> dict[str, Any]:
    """One of F1-F6's elements, drawn by its own generator, renamed `cid`, its fields changed by
    `change` (the affiliation of a look-alike, say), and re-timed so it comes within the circle at
    `entry` (no earlier than `MIN_START_S` from its start)."""
    gen, _ = ELEMENTS[what]
    gen(d)
    c = d.added.pop()
    st = d.stated.pop("element")
    if cid:
        c["id"] = cid
        st["contact"] = cid
    c.update(change)
    st.update({k: v for k, v in change.items() if k in ("affiliation", "kind", "speed_mps")})
    if "speed_mps" in change:
        c["speed_mps"] = round(float(change["speed_mps"]), 2)
        st["speed_mps"] = c["speed_mps"]
        if what == "boat":
            recut(d, c, st, d.rng.uniform(*F.F1_LEAD_S))
    if entry is not None:
        r = d.w.mission.radius_m
        if entry - travel_s(c, d.center, r) < MIN_START_S and entry > MIN_START_S:
            # too far out to come in by then even from the earliest start: it appears nearer
            recut(d, c, st, entry - MIN_START_S)
        c["start_s"] = round(max(MIN_START_S, entry - travel_s(c, d.center, r)), 1)
    st["start_s"] = c["start_s"]
    st["entry_s"] = round(entry_s(d, c), 1)
    st["element"] = what
    st.setdefault("affiliation", c["affiliation"])
    d.added.append(c)
    (d.decoys if decoy else d.elements).append(st)
    return c


def recut(d: DraftX, c: dict[str, Any], st: dict[str, Any], lead: float) -> None:
    """The element appears `lead` seconds before it comes within the circle, at its speed, its
    route cut from its own: a boat given another speed than F1 drew, 15 to 100 s (F1's lead,
    `families.F1_LEAD_S`: no buoy's passive sonar identifies it by its deadline, SCORING-V3.md 2.2
    and choice 5); an element timed to come in earlier than its whole route allows, so it can."""
    route = [tuple(p) for p in c["route"]]
    speed = float(c["speed_mps"])
    k = F.first_within(route, d.center, d.w.mission.radius_m) or 0.0
    lo, hi = 0.0, k
    for _ in range(40):
        mid = 0.5 * (lo + hi)
        if F.time_to(mid, speed) < lead:
            lo = mid
        else:
            hi = mid
    part = F.start_before(route, d.center, d.w.mission.radius_m, lo)
    if part is not None:
        c["route"] = [[round(p[0], 6), round(p[1], 6)] for p in part]
        st["appears_before_circle_m"] = round(lo)
        st["lead_s"] = round(lead, 1)


def air(d: DraftX, cid: str | None = None, entry: float | None = None, decoy: bool = False, any_light: bool = False,
        **change: Any) -> dict[str, Any]:
    """An air element: F2's drone by day with p 0.5, else F3's light aircraft (`any_light`: either
    with p 0.5 whatever the light, F9's)."""
    what = "drone" if ((d.is_day or any_light) and d.rng.random() < 0.5) else "aircraft"
    return element(d, what, cid, entry, decoy, **change)


def pick(rng: random.Random, shares: tuple[tuple[str, float], ...]) -> str:
    u = rng.random()
    acc = 0.0
    for k, p in shares:
        acc += p
        if u < acc:
            return k
    return shares[-1][0]


def one_need(d: DraftX, shares: tuple[tuple[str, float], ...], entry: float | None = None,
             any_light: bool = False) -> dict[str, Any]:
    """One need by its drawn kind: `boat`, `air`, `vehicle`, `uuv` or `diver`."""
    k = pick(d.rng, shares)
    return air(d, entry=entry, any_light=any_light) if k == "air" else element(d, k, entry=entry)


def in_circle_point(d: DraftX, c: dict[str, Any], lo: float = 0.3, hi: float = 0.9) -> tuple[float, float]:
    """A point of a contact's way inside the mission circle, a drawn share along that part."""
    route = [tuple(p) for p in c["route"]]
    r = d.w.mission.radius_m
    k0 = F.first_within(route, d.center, r) or 0.0
    total = F.length(route)
    inside = [s for s in range(int(k0), int(total) + 1, 20) if F.dist_m(F.point_at(route, float(s)), d.center) <= r]
    if not inside:
        return F.point_at(route, k0)
    s = inside[0] + d.rng.uniform(lo, hi) * (inside[-1] - inside[0])
    return F.point_at(route, s)


# ---- the fleet as planned --------------------------------------------------------------------------

def planned(world: str) -> list[dict[str, Any]]:
    """The world's fleet as sim/swarm.py plans it (uid, callsign, platform, spec)."""
    _, standins = fleet_plan(load_world(world).fleet)
    return standins


def by_callsign(world: str) -> dict[str, dict[str, Any]]:
    return {str(s["callsign"]): s for s in planned(world)}


def sensor_uids(world: str, sensor: str) -> list[int]:
    """The vehicles that take an orbit and carry `sensor` (the looks a first-look trigger counts)."""
    out = []
    for s in planned(world):
        types = {x.get("type") for x in s["spec"].get("sensors") or []}
        if sensor in types and s["spec"].get("behavior") not in ("drifter", "tracker"):
            out.append(int(s["uid"]))
    return out


def opening_looks(world: str) -> list[tuple[str, list[tuple[float, float]]]]:
    """The watch situation's opening orders: (callsign, the points of its orbit centre or route)."""
    return [(cs, pts) for cs, pts, _ in opening_orders(world)]


@lru_cache(maxsize=None)
def opening_orders(world: str) -> tuple[tuple[str, list[tuple[float, float]], float], ...]:
    """(callsign, its orbit centre or route, the orbit's radius or 0) of each opening order."""
    sit = tomllib.loads((REPO / "sim" / "situations" / f"{F.WATCH[world]}.toml").read_text())
    out = []
    for o in sit.get("order") or []:
        cs = (o.get("vehicles") or {}).get("callsign")
        pts = [tuple(o["center"])] if o.get("center") else [tuple(p) for p in o.get("route") or []]
        if cs and pts:
            out.append((str(cs), pts, float(o.get("radius_m") or 0.0)))
    return tuple(out)


@lru_cache(maxsize=None)
def home_s(world: str, callsign: str) -> float:
    """How long a standing vehicle needs to come home from the farthest point of its opening order
    at its cruise, and land or stop (`F13_LAND_S`, by its kind)."""
    s = by_callsign(world)[callsign]
    home = (float(s["home"][1]), float(s["home"][0]))
    _, pts, radius = next(o for o in opening_orders(world) if o[0] == callsign)
    far = max(F.dist_m(p, home) for p in pts) + radius
    return far / float(s["spec"]["cruise_mps"]) + F13_LAND_S.get(str(s["spec"].get("behavior")), 60.0)


#: the domains whose vehicles a need of a domain is given to (no UGV ashore for a boat, no boat for
#: a car)
SERVES = {"surface": ("surface", "air"), "air": ("air", "surface", "ground"), "ground": ("ground", "air"),
          "subsurface": ("surface", "subsurface")}


def best_placed(d: DraftX, at: tuple[float, float], domain: str, exclude: tuple[str, ...] = ()) -> str:
    """The standing vehicle whose opening order brings it nearest a point and that carries the
    sensor of a need in `domain` (EO/IR above the water, sonar under it; `SERVES`), high aircraft
    apart."""
    cat = {s["callsign"]: s for s in planned(d.world)}
    sensor = "sonar" if domain == "subsurface" else "eo_ir"
    best: tuple[float, str] | None = None
    for cs, pts in opening_looks(d.world):
        s = cat.get(cs)
        if s is None or cs in exclude:
            continue
        spec = s["spec"]
        if sensor not in {x.get("type") for x in spec.get("sensors") or []}:
            continue
        if spec.get("domain") == "air" and float(spec.get("alt_m") or 0.0) >= 2000.0:
            continue
        if spec.get("domain") not in SERVES[domain]:
            continue
        dist = min(F.dist_m(p, at) for p in pts)
        if best is None or dist < best[0]:
            best = (dist, cs)
    assert best is not None, f"no standing vehicle for a {domain} need in {d.world}"
    return best[1]


def step(at_s: float, sid: str, kind: str, **params: Any) -> dict[str, Any]:
    """A timeline step as situations.py `timeline` gives it (script.py `step`)."""
    return {"kind": "step", "at_s": round(float(at_s), 1), "id": sid, "step": kind, "params": params,
            "proposer": "agent"}


# ---- the families --------------------------------------------------------------------------------

def _f7(d: DraftX) -> None:
    """F7 (module doc)."""
    d.duration_s = 1200.0
    t_u = d.rng.uniform(*F7_UUV_ENTRY_S)
    uuv = element(d, "uuv", entry=t_u)
    t_u = entry_s(d, uuv)
    element(d, "boat", entry=t_u + d.rng.uniform(*F7_AROUND_S))
    air(d, entry=t_u + d.rng.uniform(*F7_AROUND_S))
    d.stated["needs_open_together"] = True


def _f8(d: DraftX) -> None:
    """F8 (module doc)."""
    dom = pick(d.rng, F8_NEED_DOMAIN)
    spoofer = dom == "surface" and d.rng.random() < F8_SPOOFER_SHARE
    if dom == "surface" and spoofer:
        c = element(d, "boat", affiliation="suspect", speed_mps=d.rng.uniform(*F8_SPOOFER_SPEED),
                    ais={"mmsi": f"3669{d.rng.randrange(10 ** 5):05d}", "name": "F/V " + d.rng.choice(
                        ("PACIFIC DAWN", "SEA SPRITE", "MISS KATHY", "NORTHERN LIGHT", "LADY ANN")), "ship_type": "fishing",
                        "spoofed": True})
        d.elements[-1]["spoofer"] = True
    elif dom == "surface":
        c = element(d, "boat")
    elif dom == "air":
        c = air(d)
    elif dom == "subsurface":
        c = element(d, "uuv", entry=d.rng.uniform(*UUV_ENTRY_S)) if d.rng.random() < 0.5 else element(d, "diver")
    else:
        c = element(d, "vehicle")
    if c["domain"] == "subsurface":
        d.duration_s = 1200.0
    t = entry_s(d, c)

    def around() -> float:
        return t + d.rng.uniform(*F8_DECOY_AROUND_S)

    # on the surface: a small craft without AIS, and a boat whose AIS looks wrong
    kind = "fishing_vessel" if d.rng.random() < 0.5 else "sailboat"
    speed = d.rng.uniform(3.0, 7.0) if kind == "fishing_vessel" else d.rng.uniform(2.0, 4.0)
    element(d, "boat", "decoy-surface", around(), True, affiliation="neutral", kind=kind, speed_mps=speed)
    if d.rng.random() < 0.5:
        ship = d.rng.choice(("cargo", "passenger"))
        element(d, "boat", "decoy-ais", around(), True, affiliation="neutral", kind="fishing_vessel",
                speed_mps=d.rng.uniform(4.0, 7.0),
                ais={"mmsi": f"3669{d.rng.randrange(10 ** 5):05d}", "name": d.rng.choice(
                    ("OCEAN STAR", "PACIFIC TRADER", "BAY QUEEN", "GOLDEN WAVE")), "ship_type": ship, "spoofed": True})
        d.decoys[-1]["ais_look"] = f"cloned identity ({ship})"
    else:
        element(d, "boat", "decoy-ais", around(), True, affiliation="neutral", kind="fishing_vessel",
                speed_mps=d.rng.uniform(10.0, 14.0),
                ais={"mmsi": f"3669{d.rng.randrange(10 ** 5):05d}", "name": d.rng.choice(
                    ("REEL DEAL", "FISH TALES", "BLUE FIN", "SEA HAWK")), "ship_type": "fishing"})
        d.decoys[-1]["ais_look"] = "a sportfisher's AIS says fishing at a speed no fishing vessel makes"
    # in the air, under the water, on the ground
    air(d, "decoy-air", around(), True, affiliation="neutral")
    element(d, "uuv" if d.rng.random() < 0.5 else "diver", "decoy-subsurface", around(), True, affiliation="neutral")
    need_road = next((e.get("restricted") for e in d.elements if e["element"] == "vehicle"), None)
    for _ in range(50):
        c = element(d, "vehicle", "decoy-ground", around(), True, affiliation="neutral")
        if d.decoys[-1].get("restricted") != need_road:
            break
        d.added.pop()
        d.decoys.pop()
    d.stated["need_domain"] = dom
    d.stated["spoofer"] = spoofer


def _f9(d: DraftX, base: dict[str, Any]) -> None:
    """F9 (module doc); `base` is the world's draw for the seed."""
    w = d.w
    light = pick(d.rng, F9_LIGHT)
    c = one_need(d, (("boat", 0.4), ("air", 0.3), ("vehicle", 0.3)), any_light=True)
    t_in = entry_s(d, c)
    if light == "twilight":
        a, b = (at_unix(x) for x in w.conditions["at"])
        lon, lat = d.center
        for _ in range(20000):
            t0 = 60.0 * math.floor((a + (b - a) * d.rng.random()) / 60.0)
            el = sun_elevation_deg(t0 + t_in, lat, lon)
            if F9_TWILIGHT_SUN_DEG[0] <= el <= F9_TWILIGHT_SUN_DEG[1]:
                break
        else:
            raise RuntimeError("F9: no twilight in the world's window")
        d.cond["at"] = dt.datetime.fromtimestamp(t0, dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        d.cond["sun_at_need_deg"] = round(el, 1)
    else:
        d.cond["at"] = F.local_at(d.rng, w, light)
    fog = light == "day" or d.rng.random() < 0.5
    if fog:
        lo = float(w.conditions["visibility_km"][0])
        d.cond["visibility_km"] = round(math.exp(d.rng.uniform(math.log(lo), math.log(F9_FOG_KM_TOP))), 2)
        d.cond["visibility_by_family"] = True
    d.cond["light_by_family"] = light
    d.stated["light"] = light
    d.stated["fog"] = fog


def _f10(d: DraftX, base: dict[str, Any]) -> None:
    """F10 (module doc)."""
    w = d.w
    lo, hi = (int(x) for x in w.conditions["sea_state"])
    sea = d.rng.randint(max(lo, hi - 2), hi)
    wlo, whi = (float(x) for x in w.conditions["wind_mps"])
    d.cond["sea_state"] = sea
    d.cond["wind_mps"] = round(d.rng.uniform(0.5 * (wlo + whi), whi), 2)
    d.cond["sea_by_family"] = True
    top = F10_TOP_SPEED[min(sea, 5)]
    first = element(d, "boat", speed_mps=d.rng.uniform(min(8.0, top), top))
    if d.rng.random() < 0.5:
        frm = d.elements[-1].get("from")
        for _ in range(50):
            c = element(d, "boat", "boat-2", entry_s(d, first) + d.rng.uniform(-60.0, 240.0),
                        speed_mps=d.rng.uniform(3.0, 6.0))
            if d.elements[-1].get("from") != frm:
                break
            d.added.pop()
            d.elements.pop()
    d.stated["sea_state"] = sea
    d.stated["boat_top_speed_mps"] = top


def _f11(d: DraftX) -> None:
    """F11 (module doc)."""
    if d.rng.random() < F11_RF_SHARE:
        c = element(d, "boat") if d.rng.random() < 0.5 else air(d)
        t_in = entry_s(d, c)
        at = in_circle_point(d, c)
        r = round(d.rng.uniform(1200.0, 2500.0))
        t0 = max(30.0, t_in - d.rng.uniform(60.0, 240.0))
        dur = round(d.rng.uniform(400.0, 800.0), 1)
        circle = {"lat": round(at[1], 6), "lon": round(at[0], 6), "radius_m": r}
        name = "an RF jammer"
        d.events += [
            {"name": f"{name} (LOS)", "at_s": round(t0, 1), "duration_s": dur, "class": "los", "circle": circle},
            {"name": f"{name} (mesh)", "at_s": round(t0, 1), "duration_s": dur, "class": "mesh", "circle": circle},
            {"name": f"{name} (SATCOM)", "at_s": round(t0, 1), "duration_s": dur, "class": "satcom", "circle": circle,
             "loss": F11_SATCOM_LOSS},
            {"name": f"{name} (LEO on Iridium)", "at_s": round(t0, 1), "duration_s": dur, "profile": "satcom_leo",
             "circle": circle, "extra_latency_ms": F11_IRIDIUM_MS, "loss": 0.0},
        ]
        d.stated["trouble"] = {"kind": "rf_jammer", "circle": circle, "from_s": round(t0, 1), "duration_s": dur,
                               "need_in_s": round(t_in, 1)}
    else:
        d.duration_s = 1200.0
        c = element(d, "uuv", entry=d.rng.uniform(*UUV_ENTRY_S))
        t_in = entry_s(d, c)
        remus = next(pts for cs, pts in opening_looks(d.world) if cs == "REMUS 1")
        mid = F.lerp(remus[0], remus[-1], 0.5)
        r = round(d.rng.uniform(2000.0, 3000.0))
        t0 = max(30.0, t_in - d.rng.uniform(200.0, 400.0))
        dur = round(d.rng.uniform(150.0, 300.0), 1)
        circle = {"lat": round(mid[1], 6), "lon": round(mid[0], 6), "radius_m": r}
        d.events.append({"name": "the acoustic channel closed", "at_s": round(t0, 1), "duration_s": dur,
                         "class": "acoustic", "circle": circle, "cut": True})
        d.stated["trouble"] = {"kind": "acoustic_outage", "circle": circle, "from_s": round(t0, 1), "duration_s": dur,
                               "need_in_s": round(t_in, 1)}


def _f12(d: DraftX) -> None:
    """F12 (module doc)."""
    c = one_need(d, (("boat", 0.4), ("air", 0.3), ("vehicle", 0.3)))
    t_in = entry_s(d, c)
    if d.rng.random() < F12_JAM_SHARE:
        at = in_circle_point(d, c)
        r = round(d.rng.uniform(800.0, 2000.0))
        t0 = max(30.0, t_in - d.rng.uniform(60.0, 240.0))
        circle = {"lat": round(at[1], 6), "lon": round(at[0], 6), "radius_m": r}
        d.script.append(step(t0, "gnss-jammer", "gnss", effect="jam", circle=circle))
        d.stated["gnss"] = {"kind": "jam", "circle": circle, "from_s": round(t0, 1)}
    else:
        route = [tuple(p) for p in c["route"]]
        at = F.point_at(route, F.first_within(route, d.center, d.w.mission.radius_m) or 0.0)
        who = best_placed(d, at, c["domain"])
        t0 = max(30.0, t_in - d.rng.uniform(30.0, 200.0))
        walk = round(d.rng.uniform(1.0, 3.0), 2)
        b = round(d.rng.uniform(0.0, 360.0), 1)
        off = round(d.rng.uniform(300.0, 1000.0))
        d.script.append(step(t0, "gnss-spoofer", "gnss", effect="spoof", vehicles={"callsign": who}, walk_mps=walk,
                             bearing_deg=b, max_offset_m=off))
        d.stated["gnss"] = {"kind": "spoof", "callsign": who, "from_s": round(t0, 1), "walk_mps": walk,
                            "bearing_deg": b, "max_offset_m": off}


def _f13(d: DraftX) -> None:
    """F13 (module doc)."""
    c = one_need(d, (("boat", 0.4), ("air", 0.3), ("vehicle", 0.15), ("uuv", 0.15)))
    if c["domain"] == "subsurface":
        d.duration_s = 1200.0
        r = d.w.mission.radius_m
        entry = d.rng.uniform(*UUV_ENTRY_S)
        if entry - travel_s(c, d.center, r) < MIN_START_S:
            recut(d, c, d.elements[-1], entry - MIN_START_S)  # as `element` does for a timed one
        c["start_s"] = round(max(MIN_START_S, entry - travel_s(c, d.center, r)), 1)
        d.elements[-1]["start_s"] = c["start_s"]
        d.elements[-1]["entry_s"] = round(entry_s(d, c), 1)
    t_in = entry_s(d, c)
    grace = {"surface": 60.0, "air": 30.0, "ground": 60.0, "subsurface": 300.0}[c["domain"]]
    route = [tuple(p) for p in c["route"]]
    at = F.point_at(route, F.first_within(route, d.center, d.w.mission.radius_m) or 0.0)
    cat = by_callsign(d.world)
    # a breakdown: the agent's first look in the need's domain (v2's stuck vehicle)
    sensor = "sonar" if c["domain"] == "subsurface" else "eo_ir"
    fallback = best_placed(d, at, c["domain"])
    d.script.append({"kind": "stop", "after_first_look": [5, 60], "delay_s": round(d.rng.uniform(5.0, 60.0), 1),
                     "deadline_s": round(t_in + grace, 1), "fallback_uid": int(cat[fallback]["uid"]),
                     "look_tasks": ["orbit", "patrol"], "look_uids": sensor_uids(d.world, sensor)})
    faults: list[dict[str, Any]] = [{"kind": "breakdown", "of": "the first look", "fallback": fallback}]
    # a battery fault on a standing aircraft or UGV, timed so a prompt rtl can bring it home in time
    # (version 2: the crossing a drawn 1.5 to 3 times its way home after the fault, `home_s`)
    room = d.duration_s - F13_END_MARGIN_S
    cands = [x for x in F13_BATTERY[d.world] if x != fallback and F13_FAULT_S[0] + F13_SLACK[0] * home_s(d.world, x) <= room]
    who = d.rng.choice(cands)
    spec = cat[who]["spec"]
    back = home_s(d.world, who)
    t_f = d.rng.uniform(F13_FAULT_S[0], min(F13_FAULT_S[1], room - F13_SLACK[0] * back))
    cross = min(back * d.rng.uniform(*F13_SLACK), room - t_f)
    e_s = 3600.0 * float(spec["endurance_h"])
    target = min(33.0, max(22.0, 20.0 + 100.0 * cross / e_s))
    rate = max(1.0, (target - 20.0) / 100.0 * e_s / cross)
    est = 100.0 - 100.0 * max(0.0, t_f - 15.0) / e_s
    drop = round(max(0.0, est - target), 1)
    params: dict[str, Any] = {"vehicles": {"callsign": who}, "drop_pct": drop}
    if rate > 1.0:
        params.update(rate=round(rate, 2), duration_s=round(cross + 120.0, 1))
    d.script.append(step(t_f, "battery-fault", "battery", **params))
    faults.append({"kind": "battery", "callsign": who, "domain": spec.get("domain"), "at_s": round(t_f, 1),
                   "drop_pct": drop, "rate": round(rate, 2), "planned_left_pct": round(target, 1),
                   "planned_crossing_s": round(t_f + cross, 1), "home_s": round(back, 1)})
    d.stated["version"] = 2
    # a reboot of a standing boat on its patrol
    if d.rng.random() < F13_REBOOT_SHARE:
        boat = d.rng.choice([x for x in F13_REBOOT[d.world] if x != fallback])
        t_r = max(60.0, t_in - d.rng.uniform(0.0, 120.0))
        down = round(d.rng.uniform(3.0, 8.0), 1)
        d.script.append(step(t_r, "reboot", "reboot", vehicles={"callsign": boat}, down_s=down))
        faults.append({"kind": "reboot", "callsign": boat, "at_s": round(t_r, 1), "down_s": down})
    d.stated["faults"] = faults


GENERATORS: dict[str, tuple[str, Callable[..., None]]] = {
    "f7-three-at-once": ("F7 three at once (air, surface, subsurface)", _f7),
    "f8-decoys": ("F8 decoys and AIS spoofing", _f8),
    "f9-night-fog": ("F9 night and fog", _f9),
    "f10-sea-state": ("F10 rising sea state", _f10),
    "f11-link-trouble": ("F11 link trouble", _f11),
    "f12-gnss": ("F12 GNSS jam or spoof", _f12),
    "f13-vehicle-faults": ("F13 vehicle faults across domains", _f13),
}
#: the domains of each family's needs (drawn per situation where several)
FAMILY_DOMAIN: dict[str, str] = {
    "f7-three-at-once": "air, surface, subsurface", "f8-decoys": "any", "f9-night-fog": "surface, air, ground",
    "f10-sea-state": "surface", "f11-link-trouble": "surface, air, subsurface", "f12-gnss": "surface, air, ground",
    "f13-vehicle-faults": "any",
}
#: the families whose generator reads the world's conditions draw
WITH_BASE = ("f9-night-fog", "f10-sea-state")


def conditions_for(d: DraftX, base: dict[str, Any]) -> dict[str, Any]:
    """The world's draw for the seed with the family's changes (its start moved: the local time
    follows it)."""
    c = dict(base)
    c.update(d.cond)
    if "at" in d.cond and "utc_offset_h" in d.w.conditions:
        t = dt.datetime.fromisoformat(c["at"].replace("Z", "+00:00")) + dt.timedelta(
            hours=float(d.w.conditions["utc_offset_h"]))
        c["local_time"] = t.strftime("%H:%M")
    return c


def generate(name: str, seed: int, workdir: Path) -> tuple[EpisodeSpec, Path]:
    """One situation of `name` (`v3-<family>-<gg|sd>`, F7-F13) for `seed`: its spec and contacts
    file (families.py `generate`'s, with the family's script, link events, conditions and length)."""
    family, world = F.parse(name)
    label, gen = GENERATORS[family]
    rng = random.Random(f"v3:{family}:{world}:{int(seed)}")
    e = from_situation(F.WATCH[world])
    w = load_world(world)
    base = w.draw_conditions(random.Random(f"conditions:{world}:{int(seed)}"))
    stated: dict[str, Any] = {
        "family": family, "world": world, "seed": int(seed), "scoring": "v3", "area": label,
        "mission_sha256": mission_sha256(world), "domain": FAMILY_DOMAIN[family],
        "rule": "eval/headless/v3/mission.py: unknown, suspect or hostile within 3 km, identified in time per domain",
    }
    d = DraftX(family=family, world=world, seed=int(seed), rng=rng, stated=stated)
    d.cond = {"local_time": base.get("local_time")}
    if family in WITH_BASE:
        gen(d, base)
    else:
        gen(d)
    d.cond.pop("local_time", None)
    cond = conditions_for(d, base)
    contacts_seed = rng.randrange(1, 2 ** 31)
    workdir.mkdir(parents=True, exist_ok=True)
    path = workdir / f"{name}-{int(seed)}.contacts.toml"
    F.contacts_file(world, contacts_seed, d.added, path)
    stated["contacts_seed"] = contacts_seed
    stated["conditions"] = cond
    stated["added"] = [c["id"] for c in d.added]
    # the contacts whose AIS identity is not their own: the record's truth says only that a contact
    # broadcasts (labels.py `mark_spoofed` reads this)
    spoofed = [c["id"] for c in d.added if (c.get("ais") or {}).get("spoofed")]
    if spoofed:
        stated["ais_spoofed"] = spoofed
    stated["needs"] = [x["contact"] for x in d.elements]
    stated["elements"] = d.elements
    if d.decoys:
        stated["decoys"] = d.decoys
    if d.script:
        stated["script"] = d.script
    if d.events:
        stated["link_events"] = d.events
    e.name = f"{name}-{int(seed)}"
    e.family = name
    e.split = "v3"
    e.situation_seed = int(seed)
    e.seed = int(seed)
    e.contacts = path
    e.conditions = cond
    e.at = cond["at"]
    e.duration_s = d.duration_s
    e.truth = stated
    e.config_name = "watch"
    e.brief = mission_text(world)
    e.opaque = True
    e.script = list(e.script) + [dict(x) for x in d.script]
    e.events = list(e.events) + [dict(x) for x in d.events]
    return e, path


__all__ = ["GENERATORS", "FAMILY_DOMAIN", "generate", "sun_elevation_deg"]
