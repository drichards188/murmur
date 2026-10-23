"""World v3's three worlds (docs/WORLD-V3-PLAN.md section 5, H6; sim/worlds/README.md): Golden
Gate v3, San Diego and the 100-node scale world, checked against their own baked grids and run
once each on the real `murmur-headless` binary (built here if stale):

- every world loads with its traffic, its pinned element sets (which verify) and one ground
  station; San Diego's grids carry their provenance;
- every vehicle spawns validly: a boat or a sub in water deeper than its draft, a UGV or a
  tracker on land at the ground's height, an aircraft on the ground at a launch site or aloft
  well above the surface;
- every opening order of the world's watch situation keeps a boat's or a sub's way (from its
  spawn, a patrol back to its start) in water deeper than its draft and a UGV's on land;
- the traffic keeps to its medium (surface contacts on water, ground contacts on land, aircraft
  above the ground), and AIS only on surface contacts;
- the conditions draw day and night;
- a short episode per world: the physical model runs, every domain the traffic has by then is
  tracked, no opening order is refused at submission, no boat at a pier runs aground, the
  record names the world, its drawn conditions and its space snapshot.
"""

from __future__ import annotations

import gzip
import json
import math
import random
import shutil
import struct
import subprocess
import tomllib
from pathlib import Path

import pytest

from eval.headless import REPO
from eval.headless.episode import Episode, Options
from eval.headless.situations import (
    Calibration,
    contacts_with_conditions,
    fleet_plan,
    from_situation,
    process_spec,
)
from eval.world import load_world

WORLDS = {"golden-gate-v3": "golden-gate-v3-watch", "san-diego": "san-diego-watch",
          "golden-gate-scale": "golden-gate-scale-watch"}
CATALOG = tomllib.loads((REPO / "sim" / "platforms.toml").read_text())


class Grid:
    """A baked grid (eval/headless/bake_world.py's format), nearest sample; None: no data."""

    def __init__(self, path: Path):
        raw = gzip.decompress(path.read_bytes())
        nl = raw.index(b"\n")
        self.h = json.loads(raw[:nl])
        n = self.h["nlat"] * self.h["nlon"]
        self.v = struct.unpack(f"<{n}h", raw[nl + 1:nl + 1 + 2 * n])

    def at(self, lat: float, lon: float) -> float | None:
        i = round((lat - self.h["lat0"]) / self.h["dlat"])
        j = round((lon - self.h["lon0"]) / self.h["dlon"])
        if not (0 <= i < self.h["nlat"] and 0 <= j < self.h["nlon"]):
            return None
        x = self.v[i * self.h["nlon"] + j]
        return None if x == self.h["nodata"] else x * self.h["scale"]


def grids(world: str) -> tuple[Grid, Grid]:
    w = load_world(world)
    assert w.surface_grid is not None and w.land_water_grid is not None
    return Grid(w.surface_grid), Grid(w.land_water_grid)


def draft_m(platform: str) -> float:
    """crates/murmur-standins marine.rs `draft_m`: fast boats get 2 m of margin."""
    spec = CATALOG[platform]
    top = spec.get("max_mps") or 0.0
    return 0.5 + (2.0 if top > 10.0 else 0.0)


def along(path: list[list[float]], step_m: float = 20.0) -> list[tuple[float, float]]:
    """Points every `step_m` along a [lon, lat] path."""
    out: list[tuple[float, float]] = []
    for a, b in zip(path, path[1:]):
        lat = math.radians((a[1] + b[1]) / 2)
        d = math.hypot((b[0] - a[0]) * 111_320.0 * math.cos(lat), (b[1] - a[1]) * 111_320.0)
        n = max(1, int(d / step_m))
        out += [(a[0] + (b[0] - a[0]) * k / n, a[1] + (b[1] - a[1]) * k / n) for k in range(n)]
    out.append((path[-1][0], path[-1][1]))
    return out


@pytest.fixture(scope="module")
def binary() -> None:
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    subprocess.run([cargo, "build", "--release", "-q", "-p", "murmur-headless"], cwd=REPO, check=True)


# ---- the worlds as data -----------------------------------------------------------------------


@pytest.mark.parametrize("world", sorted(WORLDS))
def test_the_world_loads_with_its_traffic_space_and_station(world: str):
    from eval.headless import space

    w = load_world(world)
    assert w.traffic is not None and w.traffic.exists() and w.space is not None
    assert space.verify(w.space) == [], "the pinned element sets hold"
    assert len(w.ground_stations) == 1, "the link emulator has one station"
    for g in (w.surface_grid, w.land_water_grid, w.bathymetry_grid, w.currents.field if w.currents else None):
        assert g is not None and g.exists(), g
    traffic = tomllib.loads(w.traffic.read_text())
    assert traffic["sensing"]["model"] == "physical"
    assert traffic["sensing"]["terrain"] == str(w.surface_grid.relative_to(REPO))
    assert traffic["sensing"]["bathymetry"] == str(w.bathymetry_grid.relative_to(REPO))  # type: ignore[union-attr]
    e = from_situation(WORLDS[world])
    assert (e.map, e.world, e.contacts, e.space_snapshot, e.conditions_from) == (world, "v3", w.traffic, w.space, "world")


def test_san_diego_is_baked_from_its_own_tidewater_scenario():
    w = load_world("san-diego")
    sc = tomllib.loads((REPO / w.tidewater).read_text())
    assert sc["name"] == "san-diego-2026-06"
    for grid in (w.surface_grid, w.land_water_grid, w.bathymetry_grid, w.currents.field):  # type: ignore[union-attr]
        stem = grid.name.removesuffix(".gz").removesuffix(".grid")
        prov = json.loads((grid.parent / f"{stem}.provenance.json").read_text())
        assert prov["scenario"] == w.tidewater and prov["world"] == "san-diego", prov
        assert prov["tidewater"]["info"]["name"] == "san-diego-2026-06"
        assert prov["tidewater"]["info"]["bounds"] == {"west": -117.32, "south": 32.6, "east": -117.1, "north": 32.77}
    # the areas are approximations, and say so
    areas = json.loads((w.areas / "san-diego.geojson").read_text())
    assert "approximate" in areas["description"]
    assert all(f["properties"]["approximate"] is True for f in areas["features"])
    kinds = {f["properties"]["kind"] for f in areas["features"]}
    assert kinds == {"approved", "no_go", "engagement_zone", "no_strike"}


@pytest.mark.parametrize("world", sorted(WORLDS))
def test_every_vehicle_spawns_validly(world: str):
    """A boat or a sub in water deeper than its draft, a UGV or a tracker on land at the ground's
    height, an aircraft on the ground at a launch site (on land, at the ground's height) or aloft
    at least 100 m above the surface below it."""
    surface, depth = grids(world)
    vehicles, _ = fleet_plan(load_world(world).fleet)
    bad = []
    for v in vehicles:
        lat, lon, alt, _ = v["home"]
        d, s = depth.at(lat, lon), surface.at(lat, lon)
        spec = CATALOG[v["platform"]]
        if v["domain"] in ("surface", "subsurface"):
            need = draft_m(v["platform"]) if v["domain"] == "surface" else 1.0
            if d is None or d < need:
                bad.append((v["callsign"], "water", d, need))
        elif v["domain"] == "ground":
            if d is None or d > 0 or s is None or abs(alt - s) > 3.0:
                bad.append((v["callsign"], "land", d, s, alt))
        else:
            aloft = alt - (s if s is not None else 0.0) >= 100.0
            on_ground = d is not None and d <= 0 and s is not None and abs(alt - s) <= 3.0
            if spec["behavior"] == "plane" and alt > 50 and not aloft:
                bad.append((v["callsign"], "aloft", alt, s))
            elif not (aloft or on_ground):
                bad.append((v["callsign"], "launch site or aloft", alt, s, d))
    assert not bad, bad


@pytest.mark.parametrize("world", sorted(WORLDS))
def test_the_opening_orders_keep_to_their_medium(world: str):
    """From its spawn along its whole way (a patrol loops back to its first waypoint; an orbit's
    circle), a boat or a sub stays in water deeper than its draft and a UGV on land."""
    _, depth = grids(world)
    e = from_situation(WORLDS[world])
    vehicles, _ = fleet_plan(e.fleet)
    bad = []
    for o in e.orders:
        (v,) = [v for v in vehicles if all(v.get(k) == x for k, x in o.vehicles.items())]
        if v["domain"] not in ("surface", "subsurface", "ground"):
            continue
        home = [v["home"][1], v["home"][0]]
        p = o.params
        if "route" in p:
            path = [home, *map(list, p["route"]), list(p["route"][0])]
        elif "center" in p:
            lon, lat = p["center"]
            r = float(p["radius_m"])
            path = [home] + [[lon + r / (111_320.0 * math.cos(math.radians(lat))) * math.cos(a), lat + r / 111_320.0 * math.sin(a)]
                             for a in (k * math.pi / 12 for k in range(25))]
        else:
            path = [home, home]
        need = draft_m(v["platform"]) + 0.5 if v["domain"] == "surface" else 1.0
        for lon, lat in along(path):
            d = depth.at(lat, lon)
            if d is not None and ((v["domain"] == "ground" and d > 0) or (v["domain"] != "ground" and d < need)):
                bad.append((o.id, v["callsign"], round(lon, 4), round(lat, 4), d))
                break
    assert not bad, bad


@pytest.mark.parametrize("world", sorted(WORLDS))
def test_the_traffic_keeps_to_its_medium(world: str):
    """Surface contacts on water (1.5 m at least) and ground contacts on land wherever the grid
    has data; aircraft at least 30 m above the surface along their way; AIS on surface contacts
    only; every domain but the subsurface present (subsurface contacts come with situations)."""
    surface, depth = grids(world)
    traffic = tomllib.loads(load_world(world).traffic.read_text())  # type: ignore[union-attr]
    domains = {c["domain"] for c in traffic["contact"]}
    assert domains == {"surface", "air", "ground"}, domains
    bad = []
    for c in traffic["contact"]:
        if c["motion"] in ("route", "transit"):
            path = c["route"]
        elif c["motion"] == "loiter":
            lon, lat = c["center"]
            r = float(c["radius_m"])
            path = [[lon + r / (111_320.0 * math.cos(math.radians(lat))) * math.cos(a), lat + r / 111_320.0 * math.sin(a)]
                    for a in (k * math.pi / 12 for k in range(25))]
        else:
            path = [c["at"], c["at"]]
        if c.get("ais"):
            assert c["domain"] == "surface", c["id"]
        for lon, lat in along(path):
            d, s = depth.at(lat, lon), surface.at(lat, lon)
            if c["domain"] == "surface" and d is not None and d < 1.5:
                bad.append((c["id"], "water", lon, lat, d))
                break
            if c["domain"] == "ground" and d is not None and d > 0:
                bad.append((c["id"], "land", lon, lat, d))
                break
            if c["domain"] == "air" and s is not None and c.get("alt_m", 300) - s < 30:
                bad.append((c["id"], "clearance", lon, lat, s))
                break
    assert not bad, bad


@pytest.mark.parametrize("world", sorted(WORLDS))
def test_the_conditions_draw_day_and_night(world: str):
    w = load_world(world)
    hours = [int(w.draw_conditions(random.Random(f"conditions:{world}:{k}"))["local_time"][:2]) for k in range(1, 21)]
    assert any(8 <= h < 19 for h in hours) and any(h < 5 or h >= 22 for h in hours), hours


def test_drawn_conditions_reach_the_sensing_model(tmp_path: Path):
    """The episode's drawn visibility, sea state and wind replace the contacts scenario's own in
    a copy beside the episode (the scenario's other conditions stay); a scenario without a
    `[conditions]` table gets one; a v3 situation runs the model its contacts name, a v1 one
    stays pinned to the legacy model."""
    c = {"visibility_km": 2.5, "sea_state": 4, "wind_mps": 6.25}
    src = tmp_path / "s.toml"
    src.write_text('title = "t"\n[sensing]\nmodel = "physical"\n[conditions]\nvisibility_m = 20000\nshipping = 0.7\n'
                   '[[contact]]\nid = "a"\naffiliation = "neutral"\ndomain = "surface"\nkind = "ferry"\nmotion = "station"\n'
                   'at = [0, 0]\n')
    out = tomllib.loads(contacts_with_conditions(src, c, tmp_path / "w").read_text())
    assert out["conditions"] == {"visibility_m": 2500.0, "shipping": 0.7, "sea_state": 4.0, "wind_mps": 6.25}
    assert out["contact"][0]["id"] == "a"
    bare = tmp_path / "b.toml"
    bare.write_text('title = "t"\n')
    assert tomllib.loads(contacts_with_conditions(bare, c, tmp_path / "w").read_text())["conditions"]["sea_state"] == 4.0
    e = from_situation("golden-gate-v3-watch").drawn(3)
    spec, env = process_spec(e, 3, tmp_path / "e", Calibration.load())
    assert "MURMUR_SENSE_MODEL" not in env and e.at == e.conditions["at"]  # type: ignore[index]
    got = tomllib.loads(Path(spec["contacts"]["scenario"]).read_text())["conditions"]
    assert got["visibility_m"] == round(1000 * e.conditions["visibility_km"], 1)  # type: ignore[index]
    _, env = process_spec(from_situation("fault-drill"), 3, tmp_path / "f", Calibration.load())
    assert env["MURMUR_SENSE_MODEL"] == "legacy"


# ---- one short episode per world --------------------------------------------------------------


@pytest.mark.parametrize("world", sorted(WORLDS))
def test_a_short_episode_per_world(world: str, tmp_path: Path, binary: None):
    e = from_situation(WORLDS[world])
    e.duration_s = 240.0
    rec = Episode(e, 1, tmp_path / "w", Options(agent="none", calibration=Calibration.load(),
                                                keep=tmp_path / "keep")).run()
    log = next((tmp_path / "keep").glob("sim.log")).read_text()
    assert "sensing: the physical model (the scenario)" in log
    assert rec["world"]["name"] == world and rec["world"]["space"] == load_world(world).space
    assert rec["clock"]["at"] == rec["world"]["conditions"]["at"]
    assert rec["clock"]["ready_t"] is not None and rec["clock"]["ready_t"] < e.ready_timeout_s
    # no opening order refused at submission or failing in the harness
    assert not [o for o in rec["orders"] if o.get("error") or o.get("state") == "rejected"], rec["orders"]
    # every domain the traffic has by now is tracked
    meta = {c["id"]: c for c in rec["truth"]["contacts"]}
    tracked = {meta[c]["domain"] for c in rec["truth"]["association"].values() if c in meta}
    assert tracked == {"surface", "air", "ground"}, tracked
    # no hull grounds (the drifting buoys may, by design: they drift)
    ground = [json.loads(ln) for ln in rec["audit_text"].splitlines() if "Aground" in ln]
    drifters = {str(v["uid"]) for v in fleet_plan(e.fleet)[0] if v["platform"] == "buoy"}
    assert not [g for g in ground if g.get("type") == "vehicle.event" and g.get("uid") not in drifters], ground
