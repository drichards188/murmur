"""Families of generated situations (docs/HEADLESS.md, section 8; the split in split.toml,
committed before any result). MODEL RESULTS ONLY: situations from stated distributions, labels
from generated truth, the operator a scripted model.

`generate(family, seed, workdir)` gives one situation (an `EpisodeSpec`) and its contacts file,
from `random.Random(f"{family}:{seed}")`: the same family and seed, the same situation. Every
family keeps Harbor watch's base: its four vehicles and their three opening orders, its watch
policy, its `[playbook]` table, 720 s, the AI at 45 s, the autoplay operator, and its ordinary
traffic (sim/scenarios/harbor-watch.toml without its fast boat) under a contacts seed drawn per
situation, so the traffic's motion varies with the seed. The family adds its element, drawn as
stated below (uniform unless stated; distances from the Golden Gate Bridge, bearings from it).

- **fast-unknown** (tuning; Harbor watch's kind): a small fast boat, no AIS, truth unknown;
  enters at 100 to 300 s, 4.5 to 6 km out on a bearing of 245 to 265 deg, at 10 to 20 m/s, through
  the strait's mouth; holds on the strait's axis 0.8 to 2.4 km from the bridge (p 0.7), or passes
  under it and holds 2.5 to 4 km inside the bay (p 0.3).
- **decoy** (tuning): a small craft without AIS, truth neutral (a fishing boat p 0.6, a sailboat
  p 0.4), from 0 to 200 s, on water 0.5 to 2.8 km from the bridge (p 0.5) or 3.5 to 6 km (p 0.5);
  drifting (p 0.5), on a slow transit at 1 to 5 m/s along a drawn course for 1 to 3 km (p 0.3),
  or loitering in a circle of 100 to 400 m (p 0.2). A fast unknown as above as well (p 0.5).
- **two-at-once** (held-out): two fast unknowns, the second 0 to 60 s after the first: from the
  west at least 15 deg apart (bearings 240 to 275 deg; p 0.5) or from inside the bay, 4 to 5 km
  out on a bearing of 75 to 100 deg, holding 0.8 to 2.4 km east of the bridge (p 0.5). One free
  sensor: PUMA 1 (PUMA 2 stays on its station order).
- **link-cut-mid-look** (tuning; version 2, 2026-10-10): a fast unknown; both directions of the
  link of the vehicle flying the first look of the playbook's to be dispatched cut 5 to 90 s after
  its dispatch, for 8 to 120 s. If no look is dispatched by `LINK_CUT_DEADLINE_S` (600 s), nothing
  is cut and the episode records "no look to cut" (version 1 cut PUMA 1 at a drawn 200 to 400 s
  instead, whatever the look did: most of its episodes cut no look).
- **stuck-vehicle** (held-out): a fast unknown; HUNTER 1 stops making way at 120 to 500 s for
  the rest of the run (its stand-in's cruise and top speed set to zero, a vehicle-side fault).
- **low-battery** (tuning): a fast unknown; one vehicle (PUMA 2 p 0.5, PUMA 1 p 0.3, REAPER 1
  p 0.2) starts with the charge that, drained at its catalog endurance, crosses the low-reserve
  line (the policy's reserve, 20 %, plus 5) at 100 to 600 s.
- **approval-flood** (held-out): a fast unknown; from 60 to 400 s, `agent-scripted` proposes 6
  to 12 routine orders (patrols for HUNTER 1 near its route, orbits for REAPER 1 near its
  station, inside the approved area) evenly over 10 to 60 s; the watch policy sends each to the
  operator.

Labels (labels.py): a contact needs a look when its truth affiliation is unknown, suspect or
hostile and it comes within 3 km of the bridge, by then plus 60 s; faults are labelled by their
windows (`truth.stated`).

**Scoring v2** (`generate(family, seed, workdir, world="v2")`, or a family name prefixed `v2-`
for callers that pass names only, such as batch.py's `--families`; eval/SCORING-V2.md sections 0
and 9.1). The v1 families above stay the default and unchanged, for v1 re-scoring only. The v2
base: Harbor watch's four vehicles, spawn points and three opening orders on the v2 fleet
(sim/fleets/harbor-watch-v2.toml: REAPER 1 and HUNTER 1 carry radar and AIS only, so only the
Pumas' EO/IR identifies), its watch policy, its `[playbook]` table (the same config, labelled
`watch`, so no situation name reaches the playbook's own reads), 720 s, the AI at 45 s, the
autoplay operator, its ordinary traffic under a contacts seed drawn per situation, and the
mission (eval/headless/v2/mission.py) as the brief, verbatim and nothing else. Each family
isolates one test area (`V2_AREAS`; its generator's docstring says what it adds and what it
holds at base), drawing from `random.Random(f"{family}:{seed}")` as v1 does, so a family whose
draws are unchanged (`fast-unknown`, `two-at-once`) has the same contacts in both worlds and a
v1-world run of it shows what the world change did (SCORING-V2.md section 12). Harbor watch on
the v2 base (`harbor-watch`) is one situation: its own contacts file and seed, unchanged. Every
v2 situation states the sensing model's misclassification draw of each contact (`sensing`),
computed with the simulator's own function (`misclassified`, a port checked against it), so a
need no look can resolve is known before the run. New families (`quiet`, `conflict`) are written
from the world and the mission, never from an agent's config.
"""

from __future__ import annotations

import gzip
import json
import math
import random
import struct
import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable

from . import HERE, REPO
from .situations import TERRAIN_GRID, EpisodeSpec, fleet_plan, from_situation
from .v2.mission import MISSION, MISSION_SHA256

GATE = (-122.4783, 37.8199)
#: link-cut-mid-look: no look dispatched by then, no cut (the cut and its window still fit in 720 s)
LINK_CUT_DEADLINE_S = 600.0
MOUTH = (-122.5050, 37.8100)  # the strait's mouth, west of the bridge
BAY_AXIS = (-122.4500, 37.8250)  # east of the bridge, mid-channel
BASE_CONTACTS = REPO / "sim" / "scenarios" / "harbor-watch.toml"
BASE_FAST_BOAT = "fastboat-west"


def split() -> dict[str, str]:
    raw = tomllib.loads((HERE / "split.toml").read_text())
    return {f["name"]: f["split"] for f in raw["family"]}


FAMILIES = tuple(split())


def offset(p: tuple[float, float], bearing_deg: float, dist_m: float) -> tuple[float, float]:
    b = math.radians(bearing_deg)
    lat = p[1] + dist_m * math.cos(b) / 111_320.0
    lon = p[0] + dist_m * math.sin(b) / (111_320.0 * math.cos(math.radians(p[1])))
    return (round(lon, 6), round(lat, 6))


def lerp(a: tuple[float, float], b: tuple[float, float], f: float) -> tuple[float, float]:
    return (round(a[0] + f * (b[0] - a[0]), 6), round(a[1] + f * (b[1] - a[1]), 6))


def dist_m(a: tuple[float, float], b: tuple[float, float]) -> float:
    lat = math.radians((a[1] + b[1]) / 2)
    return math.hypot((a[0] - b[0]) * 111_320.0 * math.cos(lat), (a[1] - b[1]) * 111_320.0)


class Water:
    """Land or water from the baked surface grid: water where the surface is the sea's (within
    1 m of the water level the grid was sampled at)."""

    def __init__(self, path: Path = TERRAIN_GRID):
        raw = gzip.decompress(path.read_bytes())
        nl = raw.index(b"\n")
        self.h = json.loads(raw[:nl])
        n = self.h["nlat"] * self.h["nlon"]
        self.v = struct.unpack(f"<{n}h", raw[nl + 1:nl + 1 + 2 * n])

    def surface(self, lon: float, lat: float) -> float | None:
        i = round((lat - self.h["lat0"]) / self.h["dlat"])
        j = round((lon - self.h["lon0"]) / self.h["dlon"])
        if not (0 <= i < self.h["nlat"] and 0 <= j < self.h["nlon"]):
            return None
        v = self.v[i * self.h["nlon"] + j]
        return None if v == self.h["nodata"] else v * self.h["scale"]

    def wet(self, p: tuple[float, float], margin_m: float = 60.0) -> bool:
        """Water here and `margin_m` around (no point of a craft's path on a beach)."""
        for b in (0, 90, 180, 270, None):
            q = p if b is None else offset(p, b, margin_m)
            s = self.surface(*q)
            if s is None or s > 1.0:
                return False
        return True

    def path_wet(self, pts: list[tuple[float, float]], step_m: float = 50.0) -> bool:
        for a, b in zip(pts, pts[1:]):
            n = max(1, int(dist_m(a, b) / step_m))
            for k in range(n + 1):
                if not self.wet(lerp(a, b, k / n), 20.0):
                    return False
        return True


_WATER: Water | None = None


def water() -> Water:
    global _WATER
    if _WATER is None:
        _WATER = Water()
    return _WATER


# ---- contacts --------------------------------------------------------------------------------

def _toml_value(v: Any) -> str:
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, (int, float)):
        return repr(float(v)) if isinstance(v, float) else str(v)
    if isinstance(v, str):
        return json.dumps(v)
    if isinstance(v, (list, tuple)):
        return "[" + ", ".join(_toml_value(x) for x in v) + "]"
    if isinstance(v, dict):
        return "{ " + ", ".join(f"{k} = {_toml_value(x)}" for k, x in v.items()) + " }"
    raise TypeError(f"no TOML for {type(v)}")


def contacts_toml(title: str, seed: int, sensing: dict[str, Any], contacts: list[dict[str, Any]]) -> str:
    out = [f"title = {json.dumps(title)}", f"seed = {seed}", "", "[sensing]"]
    out += [f"{k} = {_toml_value(v)}" for k, v in sensing.items()]
    for c in contacts:
        out += ["", "[[contact]]"] + [f"{k} = {_toml_value(v)}" for k, v in c.items()]
    return "\n".join(out) + "\n"


def fast_unknown(rng: random.Random, cid: str, start: float | None = None, bearing: float | None = None,
                 from_bay: bool = False) -> tuple[dict[str, Any], dict[str, Any]]:
    """A small fast boat without AIS, truth unknown (Harbor watch's kind). Drawn again while its
    route crosses land (the draws are then conditioned on water)."""
    for _ in range(200 if bearing is None else 20):
        c, how = _fast_unknown(rng, cid, start, bearing, from_bay)
        if water().path_wet([tuple(p) for p in c["route"]]):
            return c, how
    raise RuntimeError(f"{cid}: no route on water in 200 draws")


def _fast_unknown(rng: random.Random, cid: str, start: float | None, bearing: float | None,
                  from_bay: bool) -> tuple[dict[str, Any], dict[str, Any]]:
    speed = round(rng.uniform(10, 20), 2)
    start = round(rng.uniform(100, 300), 1) if start is None else start
    if from_bay:
        b = round(rng.uniform(75, 100), 1) if bearing is None else bearing
        entry = offset(GATE, b, rng.uniform(4000, 5000))
        hold_d = rng.uniform(800, 2400)
        hold = lerp(GATE, BAY_AXIS, hold_d / dist_m(GATE, BAY_AXIS)) if hold_d <= dist_m(GATE, BAY_AXIS) else \
            offset(GATE, 80, hold_d)
        route = [entry, BAY_AXIS, hold] if dist_m(entry, GATE) > dist_m(BAY_AXIS, GATE) + 200 else [entry, hold]
        how = {"from": "bay", "bearing_deg": b, "holds_at_m": round(hold_d)}
    else:
        b = round(rng.uniform(245, 265), 1) if bearing is None else bearing
        entry = offset(GATE, b, rng.uniform(4500, 6000))
        if rng.random() < 0.7:
            hold_d = rng.uniform(800, 2400)
            hold = lerp(GATE, MOUTH, hold_d / dist_m(GATE, MOUTH))
            route = [entry, MOUTH, hold]
            how = {"from": "west", "bearing_deg": b, "holds_at_m": round(hold_d), "passes": False}
        else:
            inside = rng.uniform(2500, 4000)
            end = offset(GATE, 75, inside)
            route = [entry, MOUTH, GATE, BAY_AXIS, end]
            how = {"from": "west", "bearing_deg": b, "passes": True, "holds_inside_m": round(inside)}
    c = {"id": cid, "name": f"Fast boat without AIS ({cid})", "affiliation": "unknown", "domain": "surface",
         "kind": "small_fast_boat", "speed_mps": speed, "motion": "transit", "route": [list(p) for p in route],
         "start_s": start}
    return c, {"contact": cid, "speed_mps": speed, "start_s": start, **how}


def decoy(rng: random.Random) -> tuple[dict[str, Any], dict[str, Any]]:
    w = water()
    kind = "fishing_vessel" if rng.random() < 0.6 else "sailboat"
    inside = rng.random() < 0.5
    start = round(rng.uniform(0, 200), 1)
    for _ in range(500):
        d = rng.uniform(500, 2800) if inside else rng.uniform(3500, 6000)
        p = offset(GATE, rng.uniform(0, 360), d)
        if w.wet(p):
            break
    else:
        raise RuntimeError("decoy: no water found")
    u = rng.random()
    c: dict[str, Any] = {"id": "decoy", "name": f"Small {kind.replace('_', ' ')} without AIS (decoy)",
                         "affiliation": "neutral", "domain": "surface", "kind": kind, "start_s": start}
    if u < 0.5:
        c.update(motion="drift", at=list(p))
        how: dict[str, Any] = {"motion": "drift"}
    elif u < 0.8:
        for _ in range(200):
            course, length = rng.uniform(0, 360), rng.uniform(1000, 3000)
            q = offset(p, course, length)
            if w.path_wet([p, q]):
                break
        else:
            q = p
        c.update(motion="transit", speed_mps=round(rng.uniform(1, 5), 2), route=[list(p), list(q)])
        how = {"motion": "transit", "speed_mps": c["speed_mps"]}
    else:
        c.update(motion="loiter", center=list(p), radius_m=round(rng.uniform(100, 400)), speed_mps=round(rng.uniform(1, 3), 2))
        how = {"motion": "loiter", "radius_m": c["radius_m"]}
    return c, {"contact": "decoy", "kind": kind, "inside_watch": inside, "start_s": start,
               "distance_m": round(dist_m(p, GATE)), **how}


# ---- the families ----------------------------------------------------------------------------

def _base() -> tuple[EpisodeSpec, dict[str, Any]]:
    e = from_situation("harbor-watch")
    raw = tomllib.loads(BASE_CONTACTS.read_text())
    return e, raw


def generate(family: str, seed: int, workdir: Path, world: str = "v1") -> tuple[EpisodeSpec, Path]:
    """One situation of `family` for `seed`: its spec and its contacts file (in `workdir`). `world`
    "v2" (or a family named `v2-<family>`) gives scoring v2's (`generate_v2`); v1, the default, is
    unchanged."""
    if family.startswith(V2_PREFIX):
        family, world = family[len(V2_PREFIX):], "v2"
    if world == "v2":
        return generate_v2(family, seed, workdir)
    if world != "v1":
        raise ValueError(f"unknown world {world!r}: v1 or v2")
    sp = split()
    if family not in sp:
        raise ValueError(f"unknown family {family!r}; families: {', '.join(sp)}")
    rng = random.Random(f"{family}:{seed}")
    e, raw = _base()
    traffic = [c for c in raw.get("contact", []) if c.get("id") != BASE_FAST_BOAT]
    added: list[dict[str, Any]] = []
    stated: dict[str, Any] = {"family": family, "seed": seed, "rule": "labels.py: unknown, suspect or hostile within 3 km of the bridge needs a look within 60 s"}
    script: list[dict[str, Any]] = []
    battery: dict[int, float] = {}

    def boat(cid: str = "fast-1", **kw: Any) -> dict[str, Any]:
        c, how = fast_unknown(rng, cid, **kw)
        added.append(c)
        stated.setdefault("fast_boats", []).append(how)
        return c

    if family == "fast-unknown":
        boat()
    elif family == "decoy":
        c, how = decoy(rng)
        added.append(c)
        stated["decoy"] = how
        if rng.random() < 0.5:
            boat()
    elif family == "two-at-once":
        first = boat("fast-1")
        t2 = round(first["start_s"] + rng.uniform(0, 60), 1)
        if rng.random() < 0.5:
            b1 = stated["fast_boats"][0]["bearing_deg"]
            for _ in range(100):
                b2 = round(rng.uniform(240, 275), 1)
                if abs(b2 - b1) < 15:
                    continue
                try:
                    boat("fast-2", start=t2, bearing=b2)
                    break
                except RuntimeError:
                    continue
            else:
                raise RuntimeError("two-at-once: no second boat 15 deg apart on water")
        else:
            boat("fast-2", start=t2, from_bay=True)
    elif family == "link-cut-mid-look":
        boat()
        item = {"kind": "link_cut", "after_first_look": [5, 90], "delay_s": round(rng.uniform(5, 90), 1),
                "duration_s": round(rng.uniform(8, 120), 1), "deadline_s": LINK_CUT_DEADLINE_S}
        script.append(item)
        stated["link_cut"] = item
        stated["family_version"] = 2
    elif family == "stuck-vehicle":
        boat()
        item = {"kind": "stop", "uid": 1, "at_s": round(rng.uniform(120, 500), 1)}
        script.append(item)
        stated["stuck"] = item
    elif family == "low-battery":
        boat()
        u = rng.random()
        uid, endurance_h = (4, 2.5) if u < 0.5 else ((3, 2.5) if u < 0.8 else (2, 35.0))
        t_cross = rng.uniform(100, 600)
        line = 25.0
        pct = round(line + 100.0 * t_cross / (endurance_h * 3600.0), 2)
        battery[uid] = pct
        stated["low_battery"] = {"uid": uid, "start_pct": pct, "drawn_crossing_s": round(t_cross, 1), "line_pct": line}
    elif family == "approval-flood":
        boat()
        n = rng.randint(6, 12)
        at = round(rng.uniform(60, 400), 1)
        window = round(rng.uniform(10, 60), 1)
        props = []
        for k in range(n):
            if k % 2 == 0:
                dx = rng.uniform(-300, 300)
                a = offset((-122.4720, 37.8135), 90, dx)
                b = offset((-122.4720, 37.8250), 90, dx)
                props.append({"task": "patrol", "uids": [1], "route": [list(a), list(b)],
                              "note": "scripted flood: patrol the channel east of the bridge"})
            else:
                c = offset((-122.5050, 37.8150), rng.uniform(0, 360), rng.uniform(0, 1000))
                props.append({"task": "orbit", "uids": [2], "center": list(c), "radius_m": round(rng.uniform(2000, 2500)),
                              "note": "scripted flood: orbit over the bay entrance"})
        script.append({"kind": "flood", "at_s": at, "window_s": window, "proposals": props})
        stated["flood"] = {"n": n, "at_s": at, "window_s": window}
    contacts_seed = rng.randrange(1, 2**31)
    path = workdir / f"{family}-{seed}.contacts.toml"
    path.write_text(contacts_toml(f"{family} seed {seed}", contacts_seed, raw.get("sensing", {}), traffic + added))
    e.name = f"{family}-{seed}"
    e.family = family
    e.split = sp[family]
    e.situation_seed = seed
    e.seed = seed  # the operator's draws
    e.contacts = path
    e.script = script
    e.battery_pct = battery
    e.truth = stated
    e.config_name = "harbor-watch"
    return e, path


# ---- scoring v2 (eval/SCORING-V2.md, sections 0 and 9.1) -----------------------------------------

#: a family name with this prefix is v2's (for callers that pass names only)
V2_PREFIX = "v2-"
V2_FLEET = REPO / "sim" / "fleets" / "harbor-watch-v2.toml"
#: the playbook's config label in v2: its `[playbook]` table is Harbor watch's, unchanged (same
#: hash); only the label, which the playbook sends at connect and writes into its rationales,
#: names no situation (SCORING-V2.md 4.4)
V2_CONFIG_NAME = "watch"
#: the contacts file's title (truth side only, never served to an agent; neutral all the same)
V2_CONTACTS_TITLE = "Golden Gate traffic"
#: every point of a flood order's ground is at least this far from the bridge: off the
#: approaches, the mission's 3 km circle and half a kilometre more
OFF_APPROACHES_M = 3500.0
#: REAPER 1's flood orbits stay west of this longitude (the bridge's towers are at -122.4797 to
#: -122.4768), so its transit from its opening orbit and its return home never cross the bridge's
#: line, where the towers' no-go boxes are
WEST_OF_TOWERS_LON = -122.482
AREAS_FILE = REPO / "sim" / "areas" / "golden-gate.geojson"

#: crates/murmur-bridge/src/tracks/sensing.rs `SensorType`, in declaration order (`sensor as u64`)
SENSOR_INDEX = {"radar": 0, "eo_ir": 1, "ais": 2, "sonar": 3, "esm": 4}
#: sensing.rs `confusion`: the kind a misclassifying sensor calls a contact of each kind
CONFUSION = {"small_fast_boat": "fishing_vessel", "fishing_vessel": "small_fast_boat", "sailboat": "fishing_vessel",
             "ferry": "cargo_ship", "cargo_ship": "ferry", "tanker": "ferry", "patrol_boat": "fishing_vessel",
             "light_aircraft": "unknown_uas", "helicopter": "light_aircraft", "unknown_uas": "light_aircraft",
             "submarine": "uuv", "uuv": "submarine", "truck": "vehicle"}
#: crates/murmur-bridge/src/tracks/model.rs `plausible_speed_mps` of `ais_kind(ship type)`: the top
#: speed the tracker believes of a vessel broadcasting that AIS ship type, m/s; faster raises an
#: identity conflict ("AIS says ... but it is making N m/s")
AIS_PLAUSIBLE_MPS = {"fishing": 8.0, "pleasure": 6.0}
#: fictional sport fishing boats (name, MMSI), as the scenarios' other fictional identities
HONEST_VESSELS = [("F/V SEA BREEZE", "366999501"), ("F/V LUCKY STRIKE", "366999502"),
                  ("F/V REEL DEAL", "366999503"), ("F/V MARIN DAWN", "366999504")]

_M64 = (1 << 64) - 1


def _hash64(seed: int, tick: int, uid: int, contact: str, salt: int) -> int:
    """sensing.rs `hash64`: FNV-1a over the fields' little-endian bytes and the contact id, then
    splitmix64."""
    h = 0xCBF29CE484222325
    data = b"".join((x & _M64).to_bytes(8, "little") for x in (seed, tick, uid, salt)) + contact.encode() + b"\xff"
    for b in data:
        h = ((h ^ b) * 0x100000001B3) & _M64
    z = (h + 0x9E3779B97F4A7C15) & _M64
    z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & _M64
    z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & _M64
    return z ^ (z >> 31)


def misclassified(seed: int, contact_id: str, sensor: str, rate: float) -> bool:
    """The sensing model's misclassification draw (sensing.rs `misclassify_draw`, called by
    tracks/mod.rs with the truth message's seed, the contacts file's): is this sensor type wrong
    about this contact for the whole run? A port, checked against the simulator itself
    (tests/test_world_v2.py), so a family can choose its contacts seed by it."""
    if rate <= 0.0:
        return False
    u = (_hash64(seed ^ 0xC1A5, -1, 0, contact_id, SENSOR_INDEX[sensor]) >> 11) / float(1 << 53)
    return u < rate


def sensing_draws(seed: int, rate: float, ids: list[str], sensors: tuple[str, ...] = ("eo_ir", "sonar")) -> dict[str, list[str]]:
    """Every contact whose classifying sensors' draw is set: {contact: [sensor type, ...]}."""
    out = {}
    for cid in ids:
        wrong = [s for s in sensors if misclassified(seed, cid, s, rate)]
        if wrong:
            out[cid] = wrong
    return out


class Areas:
    """sim/areas/golden-gate.geojson: the approved area and the no-go areas (by their bounding
    boxes, which is what they are: rectangles)."""

    def __init__(self, path: Path = AREAS_FILE):
        gj = json.loads(path.read_text())
        self.approved: list[tuple[float, float, float, float]] = []
        self.no_go: list[tuple[float, float, float, float]] = []
        for f in gj["features"]:
            kind = f["properties"].get("kind")
            if kind not in ("approved", "no_go"):
                continue
            g = f["geometry"]
            for poly in ([g["coordinates"]] if g["type"] == "Polygon" else g["coordinates"]):
                ring = poly[0]
                box = (min(p[0] for p in ring), max(p[0] for p in ring), min(p[1] for p in ring), max(p[1] for p in ring))
                (self.approved if kind == "approved" else self.no_go).append(box)

    @staticmethod
    def _to_box_m(p: tuple[float, float], box: tuple[float, float, float, float]) -> float:
        dx = max(box[0] - p[0], 0.0, p[0] - box[1]) * 111_320.0 * math.cos(math.radians(p[1]))
        dy = max(box[2] - p[1], 0.0, p[1] - box[3]) * 111_320.0
        return math.hypot(dx, dy)

    def clear(self, pts: list[tuple[float, float]], margin_m: float) -> bool:
        """No point within `margin_m` of a no-go area."""
        return all(self._to_box_m(p, b) >= margin_m for p in pts for b in self.no_go)

    def inside_approved(self, pts: list[tuple[float, float]]) -> bool:
        return all(any(b[0] <= p[0] <= b[1] and b[2] <= p[1] <= b[3] for b in self.approved) for p in pts)


_AREAS: Areas | None = None


def areas() -> Areas:
    global _AREAS
    if _AREAS is None:
        _AREAS = Areas()
    return _AREAS


def along(pts: list[tuple[float, float]], step_m: float = 50.0) -> list[tuple[float, float]]:
    """Points every `step_m` along a polyline."""
    out = [pts[0]]
    for a, b in zip(pts, pts[1:]):
        n = max(1, int(dist_m(a, b) / step_m))
        out += [lerp(a, b, k / n) for k in range(1, n + 1)]
    return out


def ring(center: tuple[float, float], radius_m: float, n: int = 36) -> list[tuple[float, float]]:
    """`n` points on a circle (crates/murmur-decide geo.rs `circle_points`: the first one north,
    an orbit's waypoint 1)."""
    return [offset(center, 360.0 * k / n, radius_m) for k in range(n)]


_V2_VEHICLES: dict[str, dict[str, Any]] | None = None


def v2_vehicles() -> dict[str, dict[str, Any]]:
    """The v2 fleet by callsign, as sim/swarm.py plans it: uid, platform, sensor types, endurance,
    minimum turn radius."""
    global _V2_VEHICLES
    if _V2_VEHICLES is None:
        _, standins = fleet_plan(V2_FLEET)
        _V2_VEHICLES = {s["callsign"]: {"uid": int(s["uid"]), "platform": s["platform"],
                                        "sensors": sorted({x["type"] for x in s["spec"].get("sensors") or []}),
                                        "endurance_h": s["spec"].get("endurance_h"),
                                        "min_turn_radius_m": s.get("min_turn_radius_m")} for s in standins}
    return _V2_VEHICLES


def classifier_uids() -> list[int]:
    """The v2 vehicles that carry a classifying sensor (EO/IR or sonar): the only ones a look can
    be flown by (the Pumas)."""
    return sorted(v["uid"] for v in v2_vehicles().values() if {"eo_ir", "sonar"} & set(v["sensors"]))


def gate_transit(rng: random.Random, inbound: bool) -> list[tuple[float, float]]:
    """A route through the Golden Gate on water: between a point 4 to 6 km out west of the strait
    (bearing 240 to 265 deg from the bridge) and one 3.5 to 5 km into the bay (bearing 65 to 95
    deg), by the strait's mouth, the bridge and the bay's axis; inbound or outbound. Drawn again
    while it crosses land."""
    w = water()
    for _ in range(200):
        out = offset(GATE, rng.uniform(240, 265), rng.uniform(4000, 6000))
        inn = offset(GATE, rng.uniform(65, 95), rng.uniform(3500, 5000))
        mid = [BAY_AXIS] if dist_m(inn, GATE) > dist_m(BAY_AXIS, GATE) + 200 else []
        route = [inn, *mid, GATE, MOUTH, out]
        if inbound:
            route.reverse()
        if w.path_wet(route):
            return route
    raise RuntimeError("no route through the Gate on water in 200 draws")


def decoy_v2(rng: random.Random) -> tuple[dict[str, Any], dict[str, Any]]:
    """S3's decoy: a craft without AIS, truth neutral, that comes within 3 km of the bridge. On
    the v2 picture it is an unknown small craft (radar gives a size class only, never an
    affiliation) until a Puma looks; a recreational speedboat stays suspect after one ("fast
    small craft, no AIS"). A fishing boat (p 0.4), a sailboat (p 0.3) or a speedboat (p 0.3),
    from 0 to 200 s: the speedboat, and the others with p 0.4, transit through the Gate either
    way (speedboat 8 to 14 m/s, fishing boat 3 to 6, sailboat 2 to 4); else they loiter in a
    circle of 100 to 400 m at 1 to 3 m/s (p 0.3) or drift (p 0.3), 0.5 to 2.5 km from the bridge
    on water."""
    w = water()
    u = rng.random()
    kind = "fishing_vessel" if u < 0.4 else ("sailboat" if u < 0.7 else "small_fast_boat")
    start = round(rng.uniform(0, 200), 1)
    m = rng.random()
    label = {"fishing_vessel": "fishing boat", "sailboat": "sailboat", "small_fast_boat": "speedboat"}[kind]
    c: dict[str, Any] = {"id": "craft-1", "name": f"Neutral {label} without AIS", "affiliation": "neutral",
                         "domain": "surface", "kind": kind, "start_s": start}
    if kind == "small_fast_boat" or m < 0.4:
        lo, hi = {"fishing_vessel": (3, 6), "sailboat": (2, 4), "small_fast_boat": (8, 14)}[kind]
        speed = round(rng.uniform(lo, hi), 2)
        inbound = rng.random() < 0.5
        route = gate_transit(rng, inbound)
        c.update(motion="transit", speed_mps=speed, route=[list(p) for p in route])
        how: dict[str, Any] = {"motion": "transit", "inbound": inbound, "speed_mps": speed}
    else:
        for _ in range(500):
            p = offset(GATE, rng.uniform(0, 360), rng.uniform(500, 2500))
            radius = round(rng.uniform(100, 400))
            if w.wet(p) and (m >= 0.7 or all(w.wet(q, 20.0) for q in ring(p, radius, 8))):
                break
        else:
            raise RuntimeError("decoy: no water found")
        if m < 0.7:
            c.update(motion="loiter", center=list(p), radius_m=radius, speed_mps=round(rng.uniform(1, 3), 2))
            how = {"motion": "loiter", "radius_m": radius, "distance_m": round(dist_m(p, GATE))}
        else:
            c.update(motion="drift", at=list(p))
            how = {"motion": "drift", "distance_m": round(dist_m(p, GATE))}
    return c, {"contact": c["id"], "kind": kind, "start_s": start, **how}


def honest_conflict(rng: random.Random) -> tuple[dict[str, Any], dict[str, Any]]:
    """S8's honest vessel: a sport fishing boat (truth neutral) broadcasting its true AIS identity,
    ship type `fishing` (p 0.5) or `pleasure` (p 0.5, the type many sport fishers register as),
    running through the Gate either way from 0 to 300 s at 2 to 5 m/s above the top speed the
    tracker believes of that ship type (`AIS_PLAUSIBLE_MPS`: 10 to 13 m/s as `fishing`, 8 to 11 as
    `pleasure`, a sport fisher's ordinary 16 to 25 knots). The C2 shows it in identity conflict
    from its first speed estimate on radar and AIS alone ("AIS says fishing ... but it is making
    11 m/s"), so suspect, as murmur-8rv's honest cutter was; a Puma's look does not clear it (a
    `pleasure` broadcast then also disagrees with EO/IR's fishing_vessel)."""
    ship_type = "fishing" if rng.random() < 0.5 else "pleasure"
    top = AIS_PLAUSIBLE_MPS[ship_type]
    speed = round(rng.uniform(top + 2.0, top + 5.0), 2)
    inbound = rng.random() < 0.5
    start = round(rng.uniform(0, 300), 1)
    route = gate_transit(rng, inbound)
    name, mmsi = HONEST_VESSELS[rng.randrange(len(HONEST_VESSELS))]
    c = {"id": "vessel-1", "name": f"Sport fishing boat with true AIS ({ship_type}) at speed", "affiliation": "neutral",
         "domain": "surface", "kind": "fishing_vessel", "speed_mps": speed, "motion": "transit",
         "route": [list(p) for p in route], "start_s": start,
         "ais": {"mmsi": mmsi, "name": name, "ship_type": ship_type}}
    return c, {"contact": "vessel-1", "ship_type": ship_type, "speed_mps": speed, "tracker_top_mps": top,
               "inbound": inbound, "start_s": start, "ais_name": name}


def flood_patrol_v2(rng: random.Random) -> list[tuple[float, float]]:
    """A routine patrol for HUNTER 1 off the approaches: an east-west line of 1 to 2 km on the
    water south of Alcatraz (latitude 37.8115 to 37.8175, its west end at longitude -122.437 to
    -122.420), every point at least `OFF_APPROACHES_M` from the bridge, inside the approved area,
    200 m clear of the no-go areas; its transit from the channel east of the bridge and its
    return stay south of Alcatraz."""
    w, ar = water(), areas()
    for _ in range(1000):
        a = (round(rng.uniform(-122.437, -122.420), 6), round(rng.uniform(37.8115, 37.8175), 6))
        b = offset(a, 90.0, rng.uniform(1000, 2000))
        pts = along([a, b])
        if (w.path_wet([a, b]) and all(dist_m(p, GATE) >= OFF_APPROACHES_M for p in pts)
                and ar.inside_approved(pts) and ar.clear(pts, 200.0)):
            return [a, b]
    raise RuntimeError("no flood patrol off the approaches in 1000 draws")


def flood_orbit_v2(rng: random.Random) -> tuple[tuple[float, float], int, str]:
    """A routine orbit for REAPER 1 off the approaches: radius 2 to 2.2 km (the bridge refuses an
    orbit under 1.1 times the platform's minimum turn radius: 1891 m for REAPER 1), centred over
    the Marin Headlands north-west of the entrance (p 0.5) or over Lands End south-west of it,
    every point of its circle at least `OFF_APPROACHES_M` from the bridge and west of the towers
    (`WEST_OF_TOWERS_LON`), 200 m clear of the no-go areas. Off the approaches it reaches past the
    approved area's corner, so the policy routes it to the operator as leaving the approved area
    (a human tier, as every agent orbit is)."""
    ar = areas()
    turn = float(v2_vehicles()["REAPER 1"]["min_turn_radius_m"])
    if 1.1 * turn > 2000.0:
        raise RuntimeError(f"flood orbits: REAPER 1 turns wider ({turn:g} m) than they were drawn for")
    for _ in range(1000):
        north = rng.random() < 0.5
        c = (round(rng.uniform(-122.530, -122.502), 6),
             round(rng.uniform(37.855, 37.878) if north else rng.uniform(37.760, 37.785), 6))
        r = round(rng.uniform(2000, 2200))
        pts = ring(c, r)
        if (all(dist_m(p, GATE) >= OFF_APPROACHES_M for p in pts) and max(p[0] for p in pts) <= WEST_OF_TOWERS_LON
                and ar.clear(pts, 200.0)):
            return c, r, "the Marin Headlands" if north else "Lands End"
    raise RuntimeError("no flood orbit off the approaches in 1000 draws")


@dataclass
class Draft:
    """A v2 situation being drawn: the generator, the misclassification rate of the base
    traffic's sensing, and what the family adds (contacts, stated truth, script, batteries;
    a contacts seed if the family must choose it)."""

    family: str
    seed: int
    rng: random.Random
    rate: float
    stated: dict[str, Any]
    added: list[dict[str, Any]] = field(default_factory=list)
    script: list[dict[str, Any]] = field(default_factory=list)
    battery: dict[int, float] = field(default_factory=dict)
    contacts_seed: int | None = None
    base_contacts: bool = False

    def boat(self, cid: str = "fast-1", **kw: Any) -> dict[str, Any]:
        c, how = fast_unknown(self.rng, cid, **kw)
        self.added.append(c)
        self.stated.setdefault("fast_boats", []).append(how)
        return c


def _v2_fast_unknown(d: Draft) -> None:
    """S1 identify: sensing, tasking and identifying one contact in time. Adds one fast unknown
    boat without AIS from the west (`fast_unknown`, v1's draws: enters at 100 to 300 s, 4.5 to 6
    km out on a bearing of 245 to 265 deg, at 10 to 20 m/s; holds on the strait's axis 0.8 to 2.4
    km from the bridge, p 0.7, or passes under it and holds 2.5 to 4 km inside the bay). Holds at
    base: the traffic, the fleet on its opening orders, the policy, the operator; no fault."""
    d.boat()


def _v2_two_at_once(d: Draft) -> None:
    """S2 allocate: two needs, two Pumas (PUMA 2 on its station order, PUMA 1 in reserve), sensors
    scarce. Adds two fast unknowns, the second 0 to 60 s after the first: from the west at least
    15 deg apart (bearings 240 to 275 deg; p 0.5) or from inside the bay, 4 to 5 km out on a
    bearing of 75 to 100 deg, holding 0.8 to 2.4 km east of the bridge (p 0.5); v1's draws. Holds
    at base: everything else."""
    first = d.boat("fast-1")
    t2 = round(first["start_s"] + d.rng.uniform(0, 60), 1)
    if d.rng.random() < 0.5:
        b1 = d.stated["fast_boats"][0]["bearing_deg"]
        for _ in range(100):
            b2 = round(d.rng.uniform(240, 275), 1)
            if abs(b2 - b1) < 15:
                continue
            try:
                d.boat("fast-2", start=t2, bearing=b2)
                break
            except RuntimeError:
                continue
        else:
            raise RuntimeError("two-at-once: no second boat 15 deg apart on water")
    else:
        d.boat("fast-2", start=t2, from_bay=True)


def _v2_decoy(d: Draft) -> None:
    """S3 restraint: spending looks on what the picture shows but truth does not need. Adds a
    craft without AIS, truth neutral, that comes within 3 km of the bridge (`decoy_v2`: unknown
    on the v2 picture until a Puma looks), among the neutral traffic, and one real need, a fast
    unknown as in S1. Holds at base: everything else."""
    c, how = decoy_v2(d.rng)
    d.added.append(c)
    d.stated["decoy"] = how
    d.boat()


def _v2_quiet(d: Draft) -> None:
    """S4 quiet watch: false alarms and load at baseline. The base traffic only (ferries, the
    shipping lane and a trawler broadcasting AIS, a sport fishing boat without AIS drifting off
    Mile Rock 5 km out, a coast guard cutter): no contact's truth affiliation is unknown, suspect
    or hostile, so nothing needs a look all episode. Holds at base: everything; adds nothing but
    its contacts seed (the traffic's motion)."""
    d.stated["needs"] = "none: no contact of unknown, suspect or hostile truth affiliation"


def _v2_low_battery(d: Draft) -> None:
    """S5 reserve: keeping vehicles above reserve while working. Adds a fast unknown as in S1,
    and one Puma (PUMA 1 or PUMA 2, p 0.5 each) starts with the charge whose drain crosses 20 %
    (the policy's reserve, the mission's "reserve") at a drawn 200 to 600 s from the stand-ins'
    start (a few seconds into the episode). The drain (crates/murmur-standins/src/battery.rs): a
    fixed wing flying at cruise draws a load of 1.0, the draw that empties the pack in its catalog
    endurance (2.5 h, 1 % in 90 s); level flight below cruise draws less (0.7 at a standstill),
    a climb up to 1.5 times; so the start is 20 % plus the endurance's share of the drawn
    crossing, 22.2 to 26.7 %, at or under 35 % from the start (the low-reserve need opens at
    once, SCORING-V2.md 2.4). Its own battery failsafe (BATT_LOW_MAH, a fifth of its pack, for
    10 s) takes it home only after the crossing. REAPER 1 (v1 drew it, p 0.2) is not: its climb
    to its tasking altitude draws 1.5 times its cruise load for minutes, so its crossing could
    not be drawn from its endurance, and it identifies nothing in v2. Holds at base: everything
    else."""
    d.boat()
    puma = "PUMA 1" if d.rng.random() < 0.5 else "PUMA 2"
    v = v2_vehicles()[puma]
    t_cross = d.rng.uniform(200, 600)
    line = 20.0
    pct = round(line + 100.0 * t_cross / (float(v["endurance_h"]) * 3600.0), 2)
    d.battery[v["uid"]] = pct
    d.stated["low_battery"] = {"uid": v["uid"], "callsign": puma, "start_pct": pct, "drawn_crossing_s": round(t_cross, 1),
                               "line_pct": line, "endurance_h": v["endurance_h"], "load": 1.0}


def need_deadline_s(c: dict[str, Any]) -> float | None:
    """When a transiting contact's need falls due, by its route at its speed from its start: the
    first point of its route within 3 km of the bridge, plus the mission's 60 s (its turns and the
    contacts process's start, a few seconds in, left out). None if it never comes that close."""
    route = [tuple(p) for p in c.get("route") or []]
    travelled = 0.0
    for a, b in zip(route, route[1:]):
        n = max(1, int(dist_m(a, b) / 10.0))
        step = dist_m(a, b) / n
        for k in range(n + 1):
            if dist_m(lerp(a, b, k / n), GATE) <= 3000.0:
                return float(c.get("start_s", 0.0)) + (travelled + k * step) / float(c["speed_mps"]) + 60.0
        travelled += dist_m(a, b)
    return None


def _v2_stuck_vehicle(d: Draft) -> None:
    """S6 vehicle failure: re-allocating when a sensor vehicle fails mid-task. Adds a fast
    unknown as in S1, and a Puma stops making way (its stand-in's cruise and top speed set to
    zero, a vehicle-side fault): the one flying the first look of the agent under test (an orbit
    or patrol by a Puma, never an opening or scripted order; script.py `first_look`), a drawn 5 to
    60 s after its dispatch, so on its way or early in its dwell, with the need still open; if no
    such look is dispatched by the boat's need deadline (`need_deadline_s`, from its drawn start,
    route and speed: no look by then is no look in time), PUMA 2 stops then, on its station
    order. A Puma on a task either way; v1 stopped HUNTER 1 at a drawn time, which would change
    nothing in v2 (it identifies nothing). Holds at base: everything else."""
    boat = d.boat()
    due = need_deadline_s(boat)
    item = {"kind": "stop", "after_first_look": [5, 60], "delay_s": round(d.rng.uniform(5, 60), 1),
            "deadline_s": round(due if due is not None else LINK_CUT_DEADLINE_S, 1),
            "fallback_uid": v2_vehicles()["PUMA 2"]["uid"], "look_tasks": ["orbit", "patrol"],
            "look_uids": classifier_uids()}
    d.script.append(item)
    d.stated["stuck"] = item


def _v2_link_cut(d: Draft) -> None:
    """S7 link loss: recovering a look a link cut interrupts, and the C2's handling of the cut
    order. Adds a fast unknown as in S1; both directions of the link of the vehicle flying the
    first look of the agent under test (an orbit or patrol by a Puma, the only vehicles that can
    look in v2; never an opening or scripted order; script.py `first_look`) cut 5 to 90 s after
    its dispatch, for 8 to 120 s (v1 version 2's draws; longer than the Pumas' 5 s lost-link
    timer, so the Puma's own failsafe takes it home); if no look is dispatched by 600 s, nothing
    is cut and the record says "no look to cut". Holds at base: everything else."""
    d.boat()
    item = {"kind": "link_cut", "after_first_look": [5, 90], "delay_s": round(d.rng.uniform(5, 90), 1),
            "duration_s": round(d.rng.uniform(8, 120), 1), "deadline_s": LINK_CUT_DEADLINE_S,
            "look_tasks": ["orbit", "patrol"], "look_uids": classifier_uids()}
    d.script.append(item)
    d.stated["link_cut"] = item


def _v2_conflict(d: Draft) -> None:
    """S8 wrong picture: what the sensing and the display get wrong. Adds (1) an honest vessel
    the C2 shows in identity conflict (`honest_conflict`: true AIS, a speed the tracker deems
    implausible for its ship type; truth neutral, so it needs nothing, while the picture shows it
    suspect within 3 km), and (2) a fast unknown as in S1 that needs a look but whose EO/IR
    misclassification draw is set, so EO/IR calls it a fishing vessel (sensing.rs `confusion`),
    which the C2 shows neutral: no look can resolve it (a sensing error, SCORING-V2.md 2.1). The
    draw is per (contact, sensor type) from the contacts seed, so the contacts seed is the first
    of the generator's draws (`rng.randrange(1, 2**31)`, as every family draws it) for which
    `misclassified(seed, "fast-1", "eo_ir", rate)` holds at the base traffic's rate (0.02:
    about 50 draws); the record states how many it took. No sonar is on any v2 vehicle. Holds at
    base: everything else, the misclassification rate included."""
    c, how = honest_conflict(d.rng)
    d.added.append(c)
    d.stated["honest_conflict"] = how
    need = d.boat()
    if d.rate <= 0.0:
        raise RuntimeError("conflict: the base traffic's misclassification rate is 0; no draw can be set")
    for k in range(1, 100_001):
        seed = d.rng.randrange(1, 2**31)
        if misclassified(seed, need["id"], "eo_ir", d.rate):
            break
    else:
        raise RuntimeError("conflict: no contacts seed sets the draw in 100000 draws")
    d.contacts_seed = seed
    d.stated["misclassified_need"] = {"contact": need["id"], "sensor": "eo_ir", "seed_draws": k,
                                      "shown_as": CONFUSION[need["kind"]]}


def _v2_approval_flood(d: Draft) -> None:
    """S9 load: many cards at once, and whether real needs still get served. Adds a fast unknown
    as in S1; from 60 to 400 s, `agent-scripted` proposes 6 to 12 routine orders evenly over 10 to
    60 s (v1's draws): patrols for HUNTER 1 on the water south of Alcatraz (`flood_patrol_v2`) and
    orbits for REAPER 1 over the headlands north-west or south-west of the entrance
    (`flood_orbit_v2`), every point of their ground at least 3.5 km from the bridge, off the
    approaches (v1's ran over them), and neither vehicle carries a classifying sensor in v2: no
    flood order can identify a contact the mission cares about. The watch policy sends each to
    the operator. Holds at base: everything else."""
    d.boat()
    hunter, reaper = v2_vehicles()["HUNTER 1"]["uid"], v2_vehicles()["REAPER 1"]["uid"]
    n = d.rng.randint(6, 12)
    at = round(d.rng.uniform(60, 400), 1)
    window = round(d.rng.uniform(10, 60), 1)
    props = []
    for k in range(n):
        if k % 2 == 0:
            route = flood_patrol_v2(d.rng)
            props.append({"task": "patrol", "uids": [hunter], "route": [list(p) for p in route],
                          "note": "routine tasking: patrol the water south of Alcatraz"})
        else:
            c, r, where = flood_orbit_v2(d.rng)
            props.append({"task": "orbit", "uids": [reaper], "center": list(c), "radius_m": r,
                          "note": f"routine tasking: orbit wide over {where}"})
    d.script.append({"kind": "flood", "at_s": at, "window_s": window, "proposals": props})
    d.stated["flood"] = {"n": n, "at_s": at, "window_s": window, "off_approaches_m": OFF_APPROACHES_M}


def _v2_harbor_watch(d: Draft) -> None:
    """Harbor watch on the v2 base: one situation, reported alone (SCORING-V2.md 8.1). Its own
    contacts file (sim/scenarios/harbor-watch.toml) and seed, unchanged: the ordinary traffic and
    the fast boat without AIS in from the west at about 150 s at 15 m/s, holding 1.5 km west of
    the bridge. Every situation seed is the same situation; a seed varies only what an episode
    seed varies (and the run id)."""
    d.base_contacts = True


#: the v2 suite (SCORING-V2.md 9.1), one family per test area, and its generators
V2_GENERATORS: dict[str, tuple[str, Callable[[Draft], None]]] = {
    "fast-unknown": ("S1 identify", _v2_fast_unknown),
    "two-at-once": ("S2 allocate", _v2_two_at_once),
    "decoy": ("S3 restraint", _v2_decoy),
    "quiet": ("S4 quiet watch", _v2_quiet),
    "low-battery": ("S5 reserve", _v2_low_battery),
    "stuck-vehicle": ("S6 vehicle failure", _v2_stuck_vehicle),
    "link-cut-mid-look": ("S7 link loss", _v2_link_cut),
    "conflict": ("S8 wrong picture", _v2_conflict),
    "approval-flood": ("S9 load", _v2_approval_flood),
    "harbor-watch": ("Harbor watch", _v2_harbor_watch),
}
V2_AREAS = {f: area for f, (area, _) in V2_GENERATORS.items()}
#: the families of the suite (S1 to S9), and every v2 situation kind (Harbor watch as well)
FAMILIES_V2 = tuple(f for f in V2_GENERATORS if f != "harbor-watch")
SITUATIONS_V2 = tuple(V2_GENERATORS)


def generate_v2(family: str, seed: int, workdir: Path) -> tuple[EpisodeSpec, Path]:
    """One v2 situation of `family` for `seed` (module doc, scoring v2): its spec and its contacts
    file (in `workdir`; Harbor watch's own file for `harbor-watch`)."""
    if family not in V2_GENERATORS:
        raise ValueError(f"unknown v2 family {family!r}; v2: {', '.join(V2_GENERATORS)}")
    area, gen = V2_GENERATORS[family]
    rng = random.Random(f"{family}:{seed}")
    e, raw = _base()
    sensing = dict(raw.get("sensing") or {})
    rate = float(sensing.get("misclassification_rate", 0.0))
    stated: dict[str, Any] = {
        "family": family, "seed": seed, "world": "v2", "area": area, "mission_sha256": MISSION_SHA256,
        "rule": ("the mission (eval/headless/v2/mission.py): a contact of unknown, suspect or hostile truth "
                 "affiliation within 3 km of the bridge is to be identified within 60 s"),
    }
    d = Draft(family=family, seed=seed, rng=rng, rate=rate, stated=stated)
    gen(d)
    if d.base_contacts:
        contacts = list(raw.get("contact", []))
        contacts_seed = int(raw.get("seed", 0))
        path = BASE_CONTACTS
    else:
        traffic = [c for c in raw.get("contact", []) if c.get("id") != BASE_FAST_BOAT]
        contacts = traffic + d.added
        contacts_seed = d.contacts_seed if d.contacts_seed is not None else rng.randrange(1, 2**31)
        path = workdir / f"v2-{family}-{seed}.contacts.toml"
        path.write_text(contacts_toml(V2_CONTACTS_TITLE, contacts_seed, sensing, contacts))
    stated["sensing"] = {"seed": contacts_seed, "misclassification_rate": rate,
                         "misclassified": sensing_draws(contacts_seed, rate, [c["id"] for c in contacts])}
    e.name = f"v2-{family}-{seed}"
    e.fleet = V2_FLEET
    e.family = family
    e.split = "v2"
    e.situation_seed = seed
    e.seed = seed  # the operator's draws
    e.contacts = path
    e.script = d.script
    e.battery_pct = d.battery
    e.truth = stated
    e.config_name = V2_CONFIG_NAME
    e.brief = MISSION
    e.world = "v2"
    return e, path
