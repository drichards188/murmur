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
"""

from __future__ import annotations

import gzip
import json
import math
import random
import struct
import tomllib
from pathlib import Path
from typing import Any

from . import HERE, REPO
from .situations import TERRAIN_GRID, EpisodeSpec, from_situation

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


def generate(family: str, seed: int, workdir: Path) -> tuple[EpisodeSpec, Path]:
    """One situation of `family` for `seed`: its spec and its contacts file (in `workdir`)."""
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
