"""Worlds (world v3, docs/WORLD-V3-PLAN.md section 4, H5): the data a situation runs in, one
file per map, `sim/worlds/<name>.toml`. Until world v3 these were constants in the code
(the Golden Gate's reference points, its terrain grid, its areas file, its ground station and
the mission's 3 km circle); a world file names them instead, and a situation names its world
(`world = "golden-gate"`, the default: every situation and record before world v3 is the
Golden Gate's).

What a world names (sim/worlds/README.md has the format with the Golden Gate's file):

- `fleet`: the fleet a situation in it takes unless it names its own;
- `areas`: the operator's areas, a directory of `*.geojson` (the bridge's `MURMUR_AREAS`);
- `tidewater`: the tidewater scenario its terrain, tide, currents and wind come from, and `at`,
  the scenario time a situation starts at unless it says;
- `traffic`: the background contacts a situation in it takes unless it names its own (a
  contacts scenario, sim/scenarios; world v3's worlds: every domain, the physical sensing model);
- `space`: the pinned satellite element sets an episode in it reads (eval/headless/space.py:
  `sim/headless/space/<space>/`) unless the batch names others;
- `[terrain]`: the baked grids (eval/headless/bake_world.py): `surface`, the surface heights
  the link emulator's terrain line of sight reads; `land_water`, the water depth the bridge's
  domain rule and the stand-ins' conditions read; `bathymetry`, the seabed's height the
  physical sensing model's sonar reads (named by the traffic's `[sensing]`); the box, spacing
  and time they were sampled on;
- `[currents]`: the baked current field (the same tool) for contacts and drifters;
- `[links]` and `[[ground_station]]`: the link profiles and where the operator's radios are;
- `[points]`: named reference points, `[lon, lat]`;
- `operating_box`: the operating area and a margin (the oracle's link map covers it);
- `[mission]`: the circle a contact must be identified in (its centre a point's name or
  `[lon, lat]`, its radius) and the grace after a contact first comes inside it;
- `[conditions]`: the ranges a situation's conditions are drawn from (`draw_conditions`).

Nothing here is read by the bridge or an agent: a world is the harness's description of the
place. What reaches the simulator goes through the episode's spec (eval/headless/situations.py).
"""

from __future__ import annotations

import datetime as dt
import math
import random
import tomllib
from dataclasses import dataclass, field
from functools import lru_cache
from pathlib import Path
from typing import Any

from . import REPO

WORLDS = REPO / "sim" / "worlds"
#: every situation and record without a world is the Golden Gate's (all of them before world v3)
DEFAULT_WORLD = "golden-gate"

TOP_KEYS = {"name", "title", "fleet", "areas", "tidewater", "at", "terrain", "currents", "links", "ground_station",
            "points", "operating_box", "mission", "conditions", "traffic", "space"}
TERRAIN_KEYS = {"surface", "land_water", "bathymetry", "box", "spacing_m", "sampled_at"}
CURRENTS_KEYS = {"field", "spacing_m", "from", "to", "step_s", "box"}
STATION_KEYS = {"name", "lat", "lon", "alt_m"}
MISSION_KEYS = {"center", "radius_m", "grace_s"}
BOX_KEYS = {"lat_min", "lat_max", "lon_min", "lon_max"}
#: the conditions a situation draws (`draw_conditions`), each a [low, high] range
CONDITION_RANGES = ("visibility_km", "wind_mps", "sea_state")
CONDITIONS_KEYS = {"at", "utc_offset_h", *CONDITION_RANGES}


class WorldError(ValueError):
    pass


@dataclass(frozen=True)
class GroundStation:
    name: str
    lat: float
    lon: float
    alt_m: float


@dataclass(frozen=True)
class Mission:
    """The circle a contact of unknown, suspect or hostile truth affiliation must be identified
    in (the labels' rule), and the grace after it first comes inside."""

    center: tuple[float, float]  # (lon, lat)
    center_name: str | None
    radius_m: float
    grace_s: float


@dataclass(frozen=True)
class Currents:
    field: Path
    spacing_m: float
    start: str
    end: str
    step_s: float
    box: dict[str, float]


@dataclass
class World:
    name: str
    title: str
    path: Path
    fleet: Path
    areas: Path
    tidewater: str
    at: str
    surface_grid: Path | None
    land_water_grid: Path | None
    box: dict[str, float]
    spacing_m: float
    sampled_at: str
    currents: Currents | None
    links: Path
    ground_stations: list[GroundStation]
    points: dict[str, tuple[float, float]]
    operating_box: dict[str, float]
    mission: Mission
    conditions: dict[str, Any] = field(default_factory=dict)
    # world v3's worlds (none in the Golden Gate's: every situation before them names its own
    # contacts and runs without a snapshot unless the batch asks)
    traffic: Path | None = None
    space: str | None = None
    bathymetry_grid: Path | None = None

    def point(self, name: str) -> tuple[float, float]:
        """A named reference point, (lon, lat)."""
        if name not in self.points:
            raise WorldError(f"{self.path}: no point {name!r}; points: {', '.join(sorted(self.points))}")
        return self.points[name]

    def draw_conditions(self, rng: random.Random) -> dict[str, Any]:
        """One situation's conditions, uniform within the world's ranges (`[conditions]`): its
        start (`at`, on the minute, inside the window the world's baked fields and tidewater
        scenario cover), visibility, wind (speed, and the direction it blows from) and sea state
        (Douglas, an integer). The draws come in a fixed order from `rng`, so a family that draws
        them from its own generator gets the same conditions for the same seed. Nothing reads them
        yet but the situation that asks (the sensor model's inputs are world v3's A1)."""
        c = self.conditions
        if not c:
            raise WorldError(f"{self.path}: no [conditions] to draw from")
        a, b = (_unix(x) for x in c["at"])
        t = a + rng.random() * (b - a)
        at = dt.datetime.fromtimestamp(60.0 * math.floor(t / 60.0), dt.timezone.utc)
        out: dict[str, Any] = {"at": at.strftime("%Y-%m-%dT%H:%M:%SZ")}
        lo, hi = c["visibility_km"]
        out["visibility_km"] = round(lo + rng.random() * (hi - lo), 2)
        lo, hi = c["wind_mps"]
        out["wind_mps"] = round(lo + rng.random() * (hi - lo), 2)
        out["wind_from_deg"] = round(360.0 * rng.random(), 1)
        lo, hi = c["sea_state"]
        out["sea_state"] = rng.randint(int(lo), int(hi))
        if "utc_offset_h" in c:
            local = at + dt.timedelta(hours=float(c["utc_offset_h"]))
            out["local_time"] = local.strftime("%H:%M")
        return out


def _unix(at: str) -> float:
    return dt.datetime.fromisoformat(str(at).replace("Z", "+00:00")).timestamp()


def _lonlat(v: Any) -> bool:
    return isinstance(v, list) and len(v) == 2 and all(isinstance(x, (int, float)) and not isinstance(x, bool) for x in v)


def _repo_path(v: Any, what: str, path: Path, must_exist: bool = True) -> Path:
    if not isinstance(v, str) or not v.strip():
        raise WorldError(f"{path}: {what} must be a path")
    p = Path(v).expanduser()
    p = p if p.is_absolute() else REPO / p
    if must_exist and not p.exists():
        raise WorldError(f"{path}: {what} {v!r} not found")
    return p


def _box(v: Any, what: str, path: Path) -> dict[str, float]:
    if not isinstance(v, dict) or set(v) != BOX_KEYS:
        raise WorldError(f"{path}: {what} is {{lat_min, lat_max, lon_min, lon_max}}")
    b = {k: float(v[k]) for k in sorted(BOX_KEYS)}
    if not (b["lat_min"] < b["lat_max"] and b["lon_min"] < b["lon_max"]):
        raise WorldError(f"{path}: {what} is empty")
    return b


def _table(raw: dict[str, Any], key: str, keys: set[str], path: Path) -> dict[str, Any]:
    t = raw.get(key) or {}
    if not isinstance(t, dict):
        raise WorldError(f"{path}: [{key}] is a table")
    unknown = set(t) - keys
    if unknown:
        raise WorldError(f"{path}: [{key}] takes no {sorted(unknown)}")
    return dict(t)


def find_world(name: str | Path) -> Path:
    p = Path(name).expanduser()
    if p.suffix == ".toml" and p.exists():
        return p.resolve()
    cand = WORLDS / f"{name}.toml"
    if cand.exists():
        return cand
    known = ", ".join(world_names())
    raise WorldError(f"no world {str(name)!r}; in {WORLDS}: {known}")


def world_names() -> list[str]:
    return sorted(f.stem for f in WORLDS.glob("*.toml"))


def load_world(name: str | Path | None = None) -> World:
    """A world by name (`sim/worlds/<name>.toml`) or path; None: the default (the Golden
    Gate). Read once per process."""
    return _load(find_world(str(name or DEFAULT_WORLD)))


@lru_cache(maxsize=None)
def _load(path: Path) -> World:
    try:
        raw = tomllib.loads(path.read_text())
    except (OSError, tomllib.TOMLDecodeError) as e:
        raise WorldError(f"{path}: {e}") from e
    unknown = set(raw) - TOP_KEYS
    if unknown:
        raise WorldError(f"{path}: unknown keys {sorted(unknown)} (sim/worlds/README.md lists them)")
    name = str(raw.get("name", path.stem))
    if name != path.stem:
        raise WorldError(f"{path}: name {name!r} is not the file's ({path.stem!r})")
    points: dict[str, tuple[float, float]] = {}
    for k, v in (raw.get("points") or {}).items():
        if not _lonlat(v):
            raise WorldError(f"{path}: point {k!r} is [lon, lat]")
        points[str(k)] = (float(v[0]), float(v[1]))
    terrain = _table(raw, "terrain", TERRAIN_KEYS, path)
    cur = _table(raw, "currents", CURRENTS_KEYS, path)
    currents = None
    if cur:
        missing = {"field", "spacing_m", "from", "to", "step_s", "box"} - set(cur)
        if missing:
            raise WorldError(f"{path}: [currents] needs {sorted(missing)}")
        currents = Currents(field=_repo_path(cur["field"], "[currents] field", path, must_exist=False),
                            spacing_m=float(cur["spacing_m"]), start=str(cur["from"]), end=str(cur["to"]),
                            step_s=float(cur["step_s"]), box=_box(cur["box"], "[currents] box", path))
        if not _unix(currents.start) < _unix(currents.end) or currents.step_s <= 0:
            raise WorldError(f"{path}: [currents] from is before to, step_s > 0")
    links = _table(raw, "links", {"file"}, path)
    stations = []
    for i, g in enumerate(raw.get("ground_station") or []):
        if not isinstance(g, dict) or set(g) - STATION_KEYS or not {"lat", "lon", "alt_m"} <= set(g):
            raise WorldError(f"{path}: ground station {i + 1} is {{name, lat, lon, alt_m}}")
        stations.append(GroundStation(name=str(g.get("name", f"station {i + 1}")), lat=float(g["lat"]),
                                      lon=float(g["lon"]), alt_m=float(g["alt_m"])))
    m = _table(raw, "mission", MISSION_KEYS, path)
    if not {"center", "radius_m"} <= set(m):
        raise WorldError(f"{path}: [mission] needs center and radius_m")
    if isinstance(m["center"], str):
        if m["center"] not in points:
            raise WorldError(f"{path}: [mission] center {m['center']!r} is not a point")
        center, center_name = points[m["center"]], str(m["center"])
    elif _lonlat(m["center"]):
        center, center_name = (float(m["center"][0]), float(m["center"][1])), None
    else:
        raise WorldError(f"{path}: [mission] center is a point's name or [lon, lat]")
    mission = Mission(center=center, center_name=center_name, radius_m=float(m["radius_m"]),
                      grace_s=float(m.get("grace_s", 60.0)))
    cond = _table(raw, "conditions", CONDITIONS_KEYS, path)
    for k in CONDITION_RANGES:
        if k in cond and not (_lonlat(cond[k]) and cond[k][0] <= cond[k][1]):
            raise WorldError(f"{path}: [conditions] {k} is [low, high]")
    if "at" in cond:
        if not (isinstance(cond["at"], list) and len(cond["at"]) == 2):
            raise WorldError(f"{path}: [conditions] at is [from, to] (RFC 3339)")
        if not _unix(cond["at"][0]) <= _unix(cond["at"][1]):
            raise WorldError(f"{path}: [conditions] at: from is not before to")
    if cond and not set(CONDITION_RANGES) | {"at"} <= set(cond):
        raise WorldError(f"{path}: [conditions] needs at and {', '.join(CONDITION_RANGES)}")
    return World(
        name=name,
        title=str(raw.get("title", name)),
        path=path,
        fleet=_repo_path(raw.get("fleet"), "fleet", path),
        areas=_repo_path(raw.get("areas"), "areas", path),
        tidewater=str(raw.get("tidewater", "none")),
        at=str(raw.get("at", "")),
        surface_grid=_repo_path(terrain["surface"], "[terrain] surface", path, False) if "surface" in terrain else None,
        land_water_grid=(_repo_path(terrain["land_water"], "[terrain] land_water", path, False)
                         if "land_water" in terrain else None),
        box=_box(terrain["box"], "[terrain] box", path) if "box" in terrain else {},
        spacing_m=float(terrain.get("spacing_m", 20.0)),
        sampled_at=str(terrain.get("sampled_at", raw.get("at", ""))),
        currents=currents,
        links=_repo_path(links.get("file", "sim/links.toml"), "[links] file", path),
        ground_stations=stations,
        points=points,
        operating_box=_box(raw["operating_box"], "operating_box", path) if "operating_box" in raw else {},
        mission=mission,
        conditions=cond,
        traffic=_repo_path(raw["traffic"], "traffic", path) if "traffic" in raw else None,
        space=_space(raw.get("space"), path),
        bathymetry_grid=(_repo_path(terrain["bathymetry"], "[terrain] bathymetry", path, False)
                         if "bathymetry" in terrain else None),
    )


def _space(v: Any, path: Path) -> str | None:
    """The world's pinned element sets: a snapshot's name under sim/headless/space (checked by
    eval/headless/space.py when an episode unpacks it)."""
    if v is None:
        return None
    if not isinstance(v, str) or not v.strip() or "/" in v:
        raise WorldError(f"{path}: space is a snapshot's name (sim/headless/space/<name>)")
    if not (REPO / "sim" / "headless" / "space" / v / "provenance.json").is_file():
        raise WorldError(f"{path}: space {v!r}: no snapshot sim/headless/space/{v}/ (eval/headless/space.py)")
    return v
