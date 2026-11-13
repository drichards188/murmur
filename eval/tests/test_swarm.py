"""Swarms in fleet files (sim/swarm.py, "swarms"; docs/DECISIONS.md, "Swarm units"): a
`[[swarm]]` table expands into its members after the plain units, each manifest entry names
its swarm, and the manifest lists the swarms, while a fleet without swarms writes what it always
did.

    .venv/bin/python -m pytest eval/tests/test_swarm.py
"""

from __future__ import annotations

import json
import math
import tomllib
from types import SimpleNamespace

import pytest

from eval.stack import _swarm

swarm = _swarm()
PLATFORMS = swarm.load_platforms()

FLEET = tomllib.loads(
    """
title = "two swarms and a unit"

[[unit]]
platform = "seahunter"
spawn = [37.82, -122.466, 0, 270]
swarm = "BOATS"

[[swarm]]
id = "BOATS"
name = "Boats"
role = "surface"

[[swarm]]
id = "RING"
name = "A ring"
role = "picket"
color = "#7fb8ff"
platform = "x10"
count = 8
sim = "standin"
cruise_alt_m = 100
layout = "ring"
center = { lat = 37.82, lon = -122.48 }
radius_m = 500
alt_m = [30, 100]

[[swarm]]
id = "BLOCK"
name = "A block"
role = "search"
mix = [{ platform = "puma", count = 3 }, { platform = "x10", count = 3 }]
sim = "standin"
layout = "block"
center = { lat = 37.805, lon = -122.456 }
columns = 3
spacing_m = 10
alt_m = 5

[[swarm]]
id = "LINE"
name = "A line"
role = "relay"
platform = "lta"
count = 5
sim = "standin"
layout = "line"
from = { lat = 37.80, lon = -122.45 }
to = { lat = 37.78, lon = -122.50 }
alt_m = 300
"""
)


def metres(a: list[float], b: list[float]) -> float:
    lat = math.radians((a[0] + b[0]) / 2)
    return math.hypot((a[0] - b[0]) * 111_320.0, (a[1] - b[1]) * 111_320.0 * math.cos(lat))


def test_swarms_expand_after_the_units_in_file_order():
    vs = swarm.plan(FLEET, PLATFORMS)
    assert [v.get("swarm") for v in vs] == ["BOATS"] + ["RING"] * 8 + ["BLOCK"] * 6 + ["LINE"] * 5
    assert [v["platform"] for v in vs[9:15]] == ["puma"] * 3 + ["x10"] * 3
    # callsigns number per platform across the whole fleet, as for units
    assert [v["callsign"] for v in vs[1:3]] == ["SKY 1", "SKY 2"] and vs[12]["callsign"] == "SKY 9"
    assert all(v["sim"] == "standin" and v["cruise_alt_m"] == 100 for v in vs[1:9])


def test_layouts_place_the_members():
    vs = swarm.plan(FLEET, PLATFORMS)
    ring, block, line = vs[1:9], vs[9:15], vs[15:20]
    centre = [37.82, -122.48]
    assert all(abs(metres(v["home"], centre) - 500) < 1 for v in ring)
    assert ring[0]["home"][0] > centre[0] and abs(ring[0]["home"][1] - centre[1]) < 1e-6  # from north
    assert ring[2]["home"][1] > centre[1]  # clockwise: east a quarter round
    assert [v["home"][2] for v in ring] == [30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0]
    # a 3 x 2 block 10 m apart, centred
    lats = sorted({round(v["home"][0], 6) for v in block})
    lons = sorted({round(v["home"][1], 6) for v in block})
    assert len(lats) == 2 and len(lons) == 3
    assert abs(metres([lats[0], lons[0]], [lats[1], lons[0]]) - 10) < 0.1
    mid = [sum(v["home"][0] for v in block) / 6, sum(v["home"][1] for v in block) / 6]
    assert metres(mid, [37.805, -122.456]) < 0.1
    assert line[0]["home"][:2] == [37.80, -122.45] and line[-1]["home"][:2] == [37.78, -122.50]
    assert all(200 < v["home"][3] < 270 for v in line)  # facing south-west along it


def test_the_manifest_names_each_swarm():
    vs = swarm.plan(FLEET, PLATFORMS)
    doc = swarm.manifest_doc(vs, swarm.fleet_swarms(FLEET))
    assert doc["swarms"] == [
        {"id": "BOATS", "name": "Boats", "role": "surface", "color": None},
        {"id": "RING", "name": "A ring", "role": "picket", "color": "#7fb8ff"},
        {"id": "BLOCK", "name": "A block", "role": "search", "color": None},
        {"id": "LINE", "name": "A line", "role": "relay", "color": None},
    ]


def test_a_fleet_without_swarms_writes_what_it_always_did():
    fleet = swarm.load_fleet("hostile-act-engagement")
    vs = swarm.plan(fleet, PLATFORMS)
    assert all("swarm" not in v for v in vs)
    assert json.dumps(swarm.manifest_doc(vs, swarm.fleet_swarms(fleet))) == json.dumps({"vehicles": vs})


@pytest.mark.parametrize(
    "bad",
    [
        {"swarm": [{"id": "A", "role": "navy"}]},
        {"swarm": [{"id": "A"}, {"id": "A"}]},
        {"swarm": [{"id": "A", "color": "blue"}]},
        {"swarm": [{"id": "A", "platform": "x10", "count": 2, "layout": "spiral"}]},
        {"swarm": [{"id": "A", "platform": "x10", "count": 2, "layout": "ring", "center": [37.8, -122.4], "radius_m": 9}]},
        {"swarm": [{"id": "A", "wings": 2}]},
        {"unit": [{"platform": "x10", "spawn": [37.8, -122.4, 0, 0], "swarm": "B"}]},
    ],
)
def test_a_bad_swarm_is_refused(bad: dict):
    with pytest.raises(SystemExit):
        swarm.plan(bad, PLATFORMS)


# ---- starting tasks (decision rules 29) -----------------------------------------------------------


def test_a_swarms_start_moves_with_each_member_and_starts_it_on_station():
    """A ring whose start orbits its centre: each member orbits its own place, starting there in the
    air at its cruise altitude, its home its place in the ring; a unit's patrol moves with each of its
    count as its line abreast does; the manifest names the fleet file only then."""
    fleet = tomllib.loads("""
[[swarm]]
id = "R"
platform = "x10"
count = 4
sim = "standin"
cruise_alt_m = 100
layout = "ring"
center = { lat = 37.82, lon = -122.47 }
radius_m = 300
start = { task = "orbit", center = [-122.47, 37.82], radius_m = 80 }

[[unit]]
platform = "corsair"
sim = "standin"
count = 2
spacing_m = 200
spawn = [37.83, -122.48, 0, 0]
start = { task = "patrol", route = [[-122.48, 37.83], [-122.48, 37.84]] }
""")
    vs = swarm.plan(fleet, PLATFORMS)
    boats, ring = vs[:2], vs[2:]
    for v in ring:
        lat, lon, alt, _ = v["home"]
        assert v["start"]["task"] == "orbit" and v["start"]["radius_m"] == 80
        assert math.dist(v["start"]["center"], [lon, lat]) < 1e-6
        assert v["start_at"][:3] == [round(lat, 7), round(lon, 7), 100.0]
    # the second boat's lane is its first's, 200 m east (line abreast of a heading of 0)
    (a0, a1), (b0, b1) = boats[0]["start"]["route"], boats[1]["start"]["route"]
    assert a0 == [-122.48, 37.83] and abs((b0[0] - a0[0]) * 111_320 * math.cos(math.radians(37.83)) - 200) < 0.5
    assert b0[1] == a0[1] and b1[1] == a1[1]
    assert boats[1]["start_at"] == [b0[1], b0[0], 0.0, 0.0]
    doc = swarm.manifest_doc(vs, swarm.fleet_swarms(fleet), {"name": "f.toml", "sha256": "0" * 64})
    assert doc["fleet"] == {"name": "f.toml", "sha256": "0" * 64}
    assert "fleet" not in swarm.manifest_doc(swarm.plan(swarm.load_fleet("hostile-act-engagement"), PLATFORMS), (), doc["fleet"])


@pytest.mark.parametrize(
    "start, unit",
    [
        ({"task": "engage", "track": "T1"}, {}),
        ({"task": "lay", "route": [[-122.47, 37.82]]}, {}),
        ({"task": "identify", "track": "T1"}, {}),
        ({"task": "collect"}, {}),
        ({"task": "cover"}, {}),
        ({"task": "orbit", "center": [-122.47, 37.82], "alt_m": 120}, {}),
        ({"task": "patrol", "route": [[-122.47, 37.82]]}, {}),
        ({"task": "hold"}, {"sim": "sitl"}),
        ({"task": "hold"}, {"platform": "ugv_logistics"}),
        ({"task": "hold"}, {"platform": "seahunter", "moored": True}),
    ],
)
def test_a_bad_start_is_refused(start: dict, unit: dict):
    u = {"platform": "x10", "sim": "standin", "spawn": [37.82, -122.47, 0, 0], "start": start, **unit}
    with pytest.raises(SystemExit):
        swarm.plan({"unit": [u]}, PLATFORMS)


def test_a_swarms_start_shared_out_gives_each_member_its_own_station():
    """`slots`: a ring's orbit shared out, member k round its own slot k (from north, clockwise)
    inside the ring, at its level; `lanes`: a patrol's lane cut into rows along it and columns
    across it, member k its own stretch; whatever the layout (here a launch block)."""
    fleet = tomllib.loads("""
[[swarm]]
id = "R"
platform = "x10"
count = 8
sim = "standin"
cruise_alt_m = 100
layout = "block"
center = { lat = 37.80, lon = -122.46 }
columns = 4
spacing_m = 8
start = { task = "orbit", center = [-122.47, 37.82], radius_m = 500 }
slots = { radius_m = 100, levels = 3, level_step_m = 15 }

[[swarm]]
id = "L"
platform = "puma"
count = 4
sim = "standin"
cruise_alt_m = 300
layout = "line"
from = { lat = 37.70, lon = -122.60 }
to = { lat = 37.75, lon = -122.60 }
alt_m = 700
start = { task = "patrol", route = [[-122.60, 37.70], [-122.60, 37.72]] }
lanes = { rows = 2, columns = 2, width_m = 200, gap = 0.25, levels = 2, level_step_m = 30 }
""")
    vs = swarm.plan(fleet, PLATFORMS)
    ring, lanes = vs[:8], vs[8:]
    for k, v in enumerate(ring):
        o = v["start"]
        assert o["task"] == "orbit" and o["radius_m"] == 100.0
        lon, lat = o["center"]
        north, east = (lat - 37.82) * 111_320.0, (lon + 122.47) * 111_320.0 * math.cos(math.radians(37.82))
        assert abs(math.hypot(north, east) - 400) < 0.5
        assert abs((math.degrees(math.atan2(east, north)) - 45 * k + 180) % 360 - 180) < 0.1
        assert v["cruise_alt_m"] == 100 + 15 * (k % 3)
        assert v["start_at"][:3] == [lat, lon, v["cruise_alt_m"]]
        assert v["home"][2] == 0  # its home its launch field
    length = 0.02 * 111_320.0
    for k, v in enumerate(lanes):
        r, c = divmod(k, 2)
        (lon_a, lat_a), (lon_b, lat_b) = v["start"]["route"]
        assert lon_a == lon_b and abs((lon_a + 122.60) * 111_320.0 * math.cos(math.radians(37.70)) - (50 if c else -50)) < 0.5
        assert abs((lat_a - 37.70) * 111_320.0 - (r + 0.25) * length / 2) < 0.5
        assert abs((lat_b - 37.70) * 111_320.0 - (r + 0.75) * length / 2) < 0.5
        assert v["cruise_alt_m"] == 300 + 30 * c
    for bad in ({"slots": {"radius_m": 600}}, {"slots": {"radius_m": 100, "rows": 2}}, {"lanes": {"rows": 2, "columns": 2}},
                {"slots": {"radius_m": 100}, "lanes": {"rows": 2, "columns": 4}}):
        f = tomllib.loads(tomllib_dump_swarm(fleet["swarm"][0], bad))
        with pytest.raises(SystemExit):
            swarm.plan(f, PLATFORMS)


def tomllib_dump_swarm(sw: dict, override: dict) -> str:
    """A one-swarm fleet file: `sw` without its slots or lanes, with `override`'s."""
    t = {k: v for k, v in sw.items() if k not in ("slots", "lanes")} | override

    def val(x):
        if isinstance(x, dict):
            return "{ " + ", ".join(f"{k} = {val(y)}" for k, y in x.items()) + " }"
        if isinstance(x, list):
            return "[" + ", ".join(val(y) for y in x) + "]"
        return json.dumps(x)

    return "[[swarm]]\n" + "\n".join(f"{k} = {val(v)}" for k, v in t.items()) + "\n"


# ---- Saturation packed: its fleet and orders (sim/situations/swarm-saturation-packed.toml) -------
# The layout Saturation had until bd murmur-mum5, kept for performance work. Checked on the Golden
# Gate's baked grids by world v3's rules (eval/headless/tests/test_worlds_v3.py), and against the
# geometry its director's notes rely on.

STRIKE = (-122.4950, 37.8120)  # where the shooter holds, [lon, lat] (sim/scenarios/swarm-saturation.toml)
JAM = {"lat": 37.8470, "lon": -122.4720, "radius_m": 1800}  # sim/link-events/swarm-saturation.toml
JAMMED = {"PICKET-1", "PICKET-7", "PICKET-8"}


@pytest.fixture(scope="module")
def saturation():
    from eval import situation as S
    from eval.headless.tests.test_worlds_v3 import CATALOG, Grid, along, draft_m

    sit = S.load_situation("swarm-saturation-packed")
    terrain = S.REPO / "sim" / "headless" / "terrain"
    return SimpleNamespace(
        sit=sit,
        vehicles=S._planned_fleet(sit.fleet),
        surface=Grid(terrain / "golden-gate.grid.gz"),
        depth=Grid(terrain / "golden-gate.water.grid.gz"),
        catalog=CATALOG,
        along=along,
        draft_m=draft_m,
    )


def m_lonlat(a, b) -> float:
    return metres([a[1], a[0]], [b[1], b[0]])


def members(sat, sid: str) -> list[dict]:
    return [v for v in sat.vehicles if v["swarm"] == sid]


def order_points(sat, o) -> list[list[float]]:
    """Where a swarm's opening order puts it, [lon, lat] every 20 m or so: an orbit's ring, a
    patrol's closed lane, a hold's members where they start."""
    p = o.params
    if "center" in p:
        lon, lat = p["center"]
        r = float(p["radius_m"])
        k = 111_320.0 * math.cos(math.radians(lat))
        ring = [[lon + r / k * math.sin(a), lat + r / 111_320.0 * math.cos(a)] for a in (n * math.pi / 36 for n in range(73))]
        return sat.along(ring, 20.0)
    if "route" in p:
        return sat.along([*map(list, p["route"]), list(p["route"][0])], 20.0)
    return [[v["home"][1], v["home"][0]] for v in members(sat, o.vehicles["swarm"])]


def test_saturation_is_37_swarms_of_3602(saturation):
    vs = saturation.vehicles
    swarms = {v["swarm"] for v in vs}
    assert len(vs) == 3602 and len(swarms) == 37 and all(v.get("swarm") for v in vs)
    assert {v["platform"] for v in members(saturation, "RESERVE")} == {"mq9b_armed"}
    # nothing else armed: every other swarm takes a script's opening order
    assert not [v["callsign"] for v in vs if v["swarm"] != "RESERVE" and saturation.catalog[v["platform"]].get("armed")]
    # one opening order per swarm but the reserve, to all its members
    assert sorted(o.vehicles["swarm"] for o in saturation.sit.orders) == sorted(swarms - {"RESERVE"})
    # the act's victim (the contacts' against_vehicle = 3201) is SURFACE-3's first USV
    assert members(saturation, "SURFACE-3")[0]["uid"] == 3201


def test_saturation_spawns_validly(saturation):
    """A boat or a sub in water deeper than its draft, an aircraft on the ground at a launch site
    (land, at the ground's height) or aloft at least 100 m above the surface below it (a fixed
    wing always aloft)."""
    bad = []
    for v in saturation.vehicles:
        lat, lon, alt, _ = v["home"]
        d, s = saturation.depth.at(lat, lon), saturation.surface.at(lat, lon)
        if v["domain"] in ("surface", "subsurface"):
            need = saturation.draft_m(v["platform"]) if v["domain"] == "surface" else 1.0
            if d is None or d < need:
                bad.append((v["callsign"], "water", d))
            continue
        aloft = alt - (s if s is not None else 0.0) >= 100.0
        on_ground = d is not None and d <= 0 and s is not None and abs(alt - s) <= 3.0
        if not (aloft or on_ground) or (saturation.catalog[v["platform"]]["behavior"] == "plane" and not aloft):
            bad.append((v["callsign"], "launch site or aloft", alt, s, d))
    assert not bad, bad[:10]


def test_saturation_orders_keep_to_their_medium_and_clear_of_no_go(saturation):
    areas = json.loads((saturation.sit.areas / "golden-gate.geojson").read_text())
    boxes = []
    for f in areas["features"]:
        if f["properties"]["kind"] == "no_go":
            g = f["geometry"]
            for poly in g["coordinates"] if g["type"] == "MultiPolygon" else [g["coordinates"]]:
                xs, ys = [p[0] for p in poly[0]], [p[1] for p in poly[0]]
                boxes.append((min(xs), min(ys), max(xs), max(ys)))
    bad = []
    for o in saturation.sit.orders:
        first = members(saturation, o.vehicles["swarm"])[0]
        home = [first["home"][1], first["home"][0]]
        points = order_points(saturation, o)
        # the way there and back too (the geofence judges the transit and return legs): from the
        # launch to a point of the ring or lane and back, for every 10th point
        legs = [p for q in points[::10] for p in saturation.along([home, q], 20.0)] if o.task != "hold" else []
        for lon, lat in points + legs:
            for w, s, e, n in boxes:  # 150 m clear of every no-go area (the geofence's, at every height)
                dx = max(w - lon, 0, lon - e) * 111_320.0 * math.cos(math.radians(lat))
                dy = max(s - lat, 0, lat - n) * 111_320.0
                if math.hypot(dx, dy) < 150:
                    bad.append((o.id, "no_go", round(lon, 4), round(lat, 4)))
            if first["domain"] in ("surface", "subsurface"):
                d = saturation.depth.at(lat, lon)
                need = saturation.draft_m(first["platform"]) + 0.5 if first["domain"] == "surface" else 1.0
                if d is None or d < need:
                    bad.append((o.id, "water", round(lon, 4), round(lat, 4), d))
    assert not bad, bad[:10]


def test_saturation_leaves_the_strike_point_clear_and_jams_three_pickets(saturation):
    """Every swarm's station 1.4 km or more from where the shooter holds (an engagement's keep-out
    there can pass); the jammer's circle takes in PICKET-1, -7 and -8's rings whole and no other
    swarm's station at all."""
    jam = (JAM["lon"], JAM["lat"])
    near, jammed = [], set()
    for o in saturation.sit.orders:
        sid = o.vehicles["swarm"]
        points = order_points(saturation, o)
        if min(m_lonlat(p, STRIKE) for p in points) < 1400:
            near.append(sid)
        inside = [m_lonlat(p, jam) <= JAM["radius_m"] for p in points]
        if all(inside):
            jammed.add(sid)
        elif any(inside):
            jammed.add(f"{sid} (partly)")
    assert not near, near
    assert jammed == JAMMED, jammed


def test_saturation_threat_is_500_quadcopters_clear_of_the_shooter_by_11_30(saturation):
    """The contacts (scripts/threat_swarms.py): 500 hostile drones, armed as their kinds are
    (sim/contacts.toml, docs/AUTONOMOUS-NODES.md decision D7): unarmed quadcopters in the recon,
    wave and decoy swarms, one-way attack drones in the two attack swarms (no order sends them in),
    and the shooter, an armed UAS flying with the recon swarm, ordered to attack SURFACE-3's first
    USV from where it hovers, its two missiles a minute apart (no scripted act); by 11:30 every
    other one is 7 km or more from the shooter's holding point (an engagement's keep-out counts any
    air track not declared by an operator anywhere it could reach, rules 14), and the file is what
    the script writes."""
    raw = tomllib.loads(saturation.sit.contacts.read_text())
    drones = [c for c in raw["contact"] if c["domain"] == "air"]
    assert len(drones) == 500 and all(c["affiliation"] == "hostile" and 15 <= c["speed_mps"] <= 28 for c in drones)
    kinds = {(c["id"].rsplit("-", 1)[0], c["kind"]) for c in drones if c["id"] != "recon-shooter"}
    assert kinds == {("recon", "small_drone"), ("wave", "small_drone"), ("decoy", "small_drone"), ("attack-east", "oneway_drone"),
                     ("attack-west", "oneway_drone")} and "act" not in raw
    catalog = tomllib.loads((saturation.sit.contacts.parents[2] / "sim" / "contacts.toml").read_text())
    (sc,) = [c for c in drones if c["id"] == "recon-shooter"]
    assert sc["kind"] == "armed_uas" and catalog["armed_uas"]["looks_like"] == "unknown_uas"
    orders = raw["order"]
    assert all(o["node"] == "recon-shooter" and o["do"] == "attack" and o["target"] == {"vehicle": 3201} and o["stay"] for o in orders)
    (w,) = [w for w in catalog["armed_uas"]["loadout"] if w["name"] == orders[0]["weapon"]]
    assert sum(o["rounds"] for o in orders) <= w["count"] and "surface" in w["targets"]
    shooter = sc["route"][-1]
    quads = drones

    def at(c: dict, t: float) -> list[float]:
        left = max(0.0, t - c.get("start_s", 0)) * c["speed_mps"]
        route = c["route"]
        for a, b in zip(route, route[1:]):
            d = m_lonlat(a, b)
            if left <= d:
                return [a[0] + (b[0] - a[0]) * left / d, a[1] + (b[1] - a[1]) * left / d]
            left -= d
        return route[-1]

    close = [c["id"] for c in quads if c["id"] != "recon-shooter" and m_lonlat(at(c, 690), shooter) < 7000]
    assert not close, close[:10]
    # regenerating writes the same file (same seed)
    import subprocess
    import sys

    out = saturation.sit.contacts.with_name("swarm-saturation.regenerated.toml")
    try:
        subprocess.run([sys.executable, str(saturation.sit.contacts.parents[2] / "scripts" / "threat_swarms.py"), "--layout", "packed", "-o", str(out)],
                       check=True, capture_output=True)
        assert out.read_text() == saturation.sit.contacts.read_text()
    finally:
        out.unlink(missing_ok=True)


# ---- Saturation over the region (sim/situations/swarm-saturation.toml, bd murmur-mum5) ----------
# The same force, threat and timeline as the packed layout, spread from Bodega Bay to Monterey Bay
# among thirteen ground stations. Checked against its links file, its areas and the geometry its
# director's notes rely on, and, where tidewater's cache holds the region's terrain fetched from
# the finest DEM (the live run's own grids; otherwise those tests skip), the spawns and the
# pickets' height over the ground. The line of sight from each station is scripts/layout_reach.py
# --dem's to check (0 of 1,004 sampled points obstructed).

R_STRIKE = (-122.5561, 37.7979)  # where the shooter holds (scripts/threat_swarms.py REGION_SHOOTER_AT)
R_JAM = {"lat": 37.5093, "lon": -122.4997, "radius_m": 5000}  # sim/link-events/swarm-saturation-bay-region.toml
R_JAMMED = {"PICKET-9", "PICKET-10", "PICKET-11"}


@pytest.fixture(scope="module")
def region():
    import sys

    from eval import situation as S

    sys.path.insert(0, str(S.REPO / "scripts"))
    import layout_reach

    sit = S.load_situation("swarm-saturation")
    packed = S.load_situation("swarm-saturation-packed")
    return SimpleNamespace(sit=sit, packed=packed, vehicles=S._planned_fleet(sit.fleet), packed_vehicles=S._planned_fleet(packed.fleet),
                           catalog=tomllib.loads((S.REPO / "sim" / "platforms.toml").read_text()), reach=layout_reach, S=S)


@pytest.fixture(scope="module")
def region_terrain(region):
    """The region's terrain as tidewater serves it to the live run (`bay-region-2026-06`'s grids,
    finest first) and the lowest and highest water its tide stations saw over the window (NAVD 88,
    the terrain's datum), from tidewater's cache; skipped unless fetched from the finest DEM."""
    layers = pytest.importorskip("tidewater.layers")
    from tidewater import fetch
    from tidewater.scenario import CACHE_ROOT, Scenario

    sc = Scenario.load(region.S.REPO / "tidewater" / "scenarios" / "bay-region-2026-06.toml")
    d = CACHE_ROOT / sc.name
    grids = [d / "terrain.npz"] + [d / f"terrain-{name}.npz" for name, _ in sc.terrain_nests]
    if not all(g.exists() for g in grids) or not (d / "tides.json").exists():
        pytest.skip(f"{sc.name} not fetched (tidewater fetch tidewater/scenarios/{sc.name}.toml)")
    if not all(fetch.terrain_sourced(g) for g in grids):
        pytest.skip(f"{sc.name}'s terrain predates the finest-DEM fetch: tidewater fetch fetches it again")
    levels = [v for st in json.loads((d / "tides.json").read_text())["stations"] for _, v in st["series"]]
    return SimpleNamespace(terrain=layers.NestedTerrain([layers.Terrain.load(g) for g in grids]), low=min(levels), high=max(levels))


def r_members(region, sid: str) -> list[dict]:
    return [v for v in region.vehicles if v["swarm"] == sid]


def r_points(v: dict) -> list[list[float]]:
    """Where a vehicle's own starting task keeps it, [lon, lat] every 20 m or so: its orbit's
    circle, its lane there and back, or (a hold) where it starts."""
    from eval.headless.tests.test_worlds_v3 import along

    p = v["start"]
    if p["task"] == "orbit":
        lon, lat = p["center"]
        r = float(p["radius_m"])
        k = 111_320.0 * math.cos(math.radians(lat))
        return along([[lon + r / k * math.sin(a), lat + r / 111_320.0 * math.cos(a)] for a in (n * math.pi / 36 for n in range(73))], 20.0)
    if p["task"] == "patrol":
        return along([*map(list, p["route"]), list(p["route"][0])], 20.0)
    return [[v["start_at"][1], v["start_at"][0]]]


def r_ways(v: dict) -> list[list[float]]:
    """A vehicle's ways from where it starts to its task and from its task home, every 20 m."""
    from eval.headless.tests.test_worlds_v3 import along

    task = r_points(v)
    at, home = [v["start_at"][1], v["start_at"][0]], [v["home"][1], v["home"][0]]
    return along([at, list(task[0])], 20.0) + along([list(task[-1]), home], 20.0)


def test_saturation_region_is_the_packed_force_spread(region):
    """The same 37 swarms (platform, count) as the packed layout, SURFACE-3's first USV still uid
    3201; the order kinds the packed layout's (orbits for the rings, patrols for the lanes) as each
    vehicle's own starting task, the surface groups' a hold where each member stands on its ring
    or lane; the reserve the only armed swarm and without one; no opening order (decision rules 29
    hands the fleet's starting tasks over, bd murmur-by3s)."""
    def shape(vehicles):
        out = {}
        for v in vehicles:
            s = out.setdefault(v["swarm"], {"platform": v["platform"], "n": 0})
            s["n"] += 1
        return {sid: (s["platform"], s["n"]) for sid, s in out.items()}

    assert len(region.vehicles) == 3602
    assert shape(region.vehicles) == shape(region.packed_vehicles)
    assert r_members(region, "SURFACE-3")[0]["uid"] == 3201
    assert not [v["callsign"] for v in region.vehicles if v["swarm"] != "RESERVE" and region.catalog[v["platform"]].get("armed")]
    assert not region.sit.orders
    packed = {o.vehicles["swarm"]: o.task for o in region.packed.orders}
    kinds: dict[str, set[str]] = {}
    for v in region.vehicles:
        kinds.setdefault(v["swarm"], set()).add(v["start"]["task"] if "start" in v else "none")
    assert kinds.pop("RESERVE") == {"none"} and "RESERVE" not in packed
    assert kinds == {sid: {"hold" if sid.startswith("SURFACE") else packed[sid]} for sid in packed}


def test_saturation_region_gives_each_vehicle_its_own_station(region):
    """One order per vehicle, each with its own geometry (bd murmur-xlca): no two vehicles share a
    station; a picket orbits its own slot round its ring, inside it; a Puma patrols its own short
    lane in its group's lane; every vehicle starts on its station (an aircraft in the air at the
    altitude its order flies, a boat or an AUV at the surface), its home its launch field or its
    place in the layout."""
    stations = set()
    for v in region.vehicles:
        if "start" not in v:
            continue
        o = v["start"]
        if o["task"] != "hold":
            key = json.dumps(o, sort_keys=True)
            assert key not in stations, v["callsign"]
            stations.add(key)
        lat, lon, alt, _ = v["start_at"]
        if v["domain"] == "air":
            assert alt == v["cruise_alt_m"], v["callsign"]
        else:
            assert alt == 0.0, v["callsign"]
        if o["task"] == "orbit":
            assert (lon, lat) == tuple(o["center"]), v["callsign"]
        elif o["task"] == "patrol":
            assert [lon, lat] == o["route"][0], v["callsign"]
        else:
            assert (lat, lon) == tuple(v["home"][:2]), v["callsign"]
    assert len(stations) == 3600 - 500  # every orbit and lane its own; the 500 boats hold where they are
    # pickets: inside their group's ring, each round its own slot
    fleet = {s["id"]: s for s in tomllib.loads(region.sit.fleet.read_text())["swarm"]}
    for sid, sw in fleet.items():
        if "slots" not in sw:
            continue
        lon0, lat0 = sw["start"]["center"]
        for v in r_members(region, sid):
            d = m_lonlat(v["start"]["center"], [lon0, lat0])
            assert abs(d + v["start"]["radius_m"] - sw["start"]["radius_m"]) < 1.0, (v["callsign"], d)


def test_saturation_region_neighbouring_orbits_and_lanes_never_share_an_altitude(region):
    """With an order each, nothing stacks the members upward (the task layer's per-order steps):
    the fleet spreads them over a few levels instead, so that any two aircraft whose orbits come
    within 50 m of each other (a picket ring's neighbours, two rings that cross), or whose lanes
    within 100 m (side by side, where a fixed wing turns at its lane's end), fly 10 m or more apart
    in height."""
    shapes = []
    for v in region.vehicles:
        if v["domain"] != "air" or "start" not in v:
            continue
        pts = r_points(v)[::3]
        xs = [p[0] for p in pts]
        ys = [p[1] for p in pts]
        shapes.append((v, pts, (min(xs), min(ys), max(xs), max(ys))))
    cell = 0.02
    grid: dict[tuple[int, int], list[int]] = {}
    for i, (_, _, (w, s, e, n)) in enumerate(shapes):
        for gx in range(int(w // cell), int(e // cell) + 1):
            for gy in range(int(s // cell), int(n // cell) + 1):
                grid.setdefault((gx, gy), []).append(i)
    close = 0
    bad = []
    seen = set()
    for members in grid.values():
        for a in members:
            for b in members:
                if b <= a or (a, b) in seen:
                    continue
                seen.add((a, b))
                (va, pa, ba), (vb, pb, bb) = shapes[a], shapes[b]
                if ba[0] > bb[2] + 0.001 or bb[0] > ba[2] + 0.001 or ba[1] > bb[3] + 0.001 or bb[1] > ba[3] + 0.001:
                    continue
                limit = 100.0 if "route" in va["start"] or "route" in vb["start"] else 50.0
                if min(m_lonlat(p, q) for p in pa for q in pb) > limit:
                    continue
                close += 1
                if abs(va["cruise_alt_m"] - vb["cruise_alt_m"]) < 10.0:
                    bad.append((va["callsign"], vb["callsign"], va["cruise_alt_m"], vb["cruise_alt_m"]))
    assert close > 1000, "the check found no neighbours: it is not looking"
    assert not bad, bad[:10]


def test_saturation_region_links_file_is_sim_links_with_its_stations(region):
    """Its own links file: sim/links.toml table for table but the ground stations (so a profile
    changed there is changed here too) and the line-of-sight class's lost-link timer (60 s here,
    a GUESS from published practice; sim/links.toml keeps ArduPilot's 5 s, on which the headless
    worlds' results are pinned), thirteen stations, the world's links the same file, and the
    runner hands it on as MURMUR_LINKS."""
    links = tomllib.loads(region.sit.links.read_text())
    shipped = tomllib.loads((region.S.REPO / "sim" / "links.toml").read_text())
    assert links["class"]["los"]["lost_link_s"] == 60 and shipped["class"]["los"]["lost_link_s"] == 5
    links["class"]["los"]["lost_link_s"] = shipped["class"]["los"]["lost_link_s"]
    assert {k: v for k, v in links.items() if k != "ground_station"} == {k: v for k, v in shipped.items() if k != "ground_station"}, \
        "sim/links.toml changed: copy the change into sim/situations/links/swarm-saturation-bay-region.toml"
    stations = links["ground_station"]
    assert len(stations) == 13 and len({s["name"] for s in stations}) == 13
    from eval.world import load_world

    assert load_world(region.sit.world).links == region.sit.links
    assert region.packed.links is None, "the packed layout keeps sim/links.toml (Crissy Field alone)"


def test_saturation_region_every_radio_reaches_a_station(region):
    """scripts/layout_reach.py: every point each radio-linked vehicle will be at (its launch or
    spawn, its way out, its ring or lane, its way home) within its class's range_max of a station;
    within range_ok everywhere but the relay chain's middle (16.5 km from Point Reyes and the
    Farallones, a few per cent of loss); the AUVs' acoustic delay a few seconds."""
    r = region.reach.reach(region.sit, quiet=True)
    rows = {row["group"]: row for row in r["rows"]}
    assert r["totals"]["beyond_max"] == 0
    beyond_ok = {g for g, row in rows.items() if row["ok_km"] is not None and row["beyond_ok"]}
    assert beyond_ok == {"RELAY-2"}, beyond_ok
    assert rows["RELAY-2"]["loss"] < 0.05
    assert all(row["worst_km"] <= 5.0 for g, row in rows.items() if g.startswith("PICKET")), "the mesh's clean 5 km"
    assert rows["UNDERSEA-1"]["acoustic_s"] < 5.0
    assert {row["profile"] for g, row in rows.items() if g.startswith("SURFACE") or g == "RESERVE"} <= {"satcom", "satcom_leo"}


def test_saturation_region_spawns_on_the_region_terrain(region, region_terrain):
    """On the terrain the live run reads: a boat in water deeper than its draft and an AUV in 1 m or
    more at the window's lowest water; an aircraft either on the ground at a launch block (land above
    the window's highest water, its spawn 0 to 3 m above the ground under it: never in it) or aloft
    100 m or more over the ground or the highest water (a fixed wing always aloft)."""
    from eval.headless.tests.test_worlds_v3 import draft_m

    t, low, high = region_terrain.terrain, region_terrain.low, region_terrain.high
    bad = []
    for v in region.vehicles:
        lat, lon, alt, _ = v["home"]
        z = t.elevation(lat, lon)
        if z is None:
            bad.append((v["callsign"], "outside the terrain"))
            continue
        if v["domain"] in ("surface", "subsurface"):
            need = draft_m(v["platform"]) if v["domain"] == "surface" else 1.0
            if low - z < need:
                bad.append((v["callsign"], "water", round(low - z, 1)))
            continue
        aloft = alt - max(z, high) >= 100.0
        on_ground = z > high and 0.0 <= alt - z <= 3.0
        if not (aloft or on_ground) or (region.catalog[v["platform"]]["behavior"] == "plane" and not aloft):
            bad.append((v["callsign"], "launch block or aloft", alt, round(z, 1)))
    assert not bad, bad[:10]


def test_saturation_region_pickets_fly_over_the_high_ground(region, region_terrain):
    """Each picket (at its own level: one order each, nothing stacks it) 25 m or more over the
    highest ground under its own orbit and its ways from where it starts and home, every 20 m; each
    AUV's own orbit over 45 m of water or more at the window's lowest (it dives to 40 m)."""
    low = []
    for v in region.vehicles:
        if v["platform"] not in ("x10", "remus"):
            continue
        points = r_points(v) + (r_ways(v) if v["platform"] == "x10" else [])
        zs = region_terrain.terrain.elevation_many([p[1] for p in points], [p[0] for p in points])
        if v["platform"] == "x10":
            top = max(zs)
            if v["cruise_alt_m"] - top < 25.0:
                low.append((v["callsign"], v["swarm"], v["cruise_alt_m"], round(float(top))))
        elif region_terrain.low - max(zs) < 45.0:
            low.append((v["callsign"], v["swarm"], round(float(region_terrain.low - max(zs)), 1)))
    assert not low, low[:10]


def test_saturation_region_is_spread(region):
    """Spread as a planner would: on station the median nearest neighbour three times the packed
    layout's and more, nearly four times the occupied square kilometres (each picket's station its
    own orbit's centre, inside its ring: 512 squares against the group orders' 538 on the rings
    themselves), over a region of 10,000 square km and more."""
    new, old = region.reach.spread(region.sit, quiet=True), region.reach.spread(region.packed, quiet=True)
    assert new["station"]["nn_median_m"] >= 3 * old["station"]["nn_median_m"]
    assert new["station"]["km2"] >= 3.5 * old["station"]["km2"]
    assert new["box_km2"] >= 10_000


def test_saturation_region_orders_keep_clear_of_no_go_and_to_the_reserve(region):
    """Every vehicle's own orbit, lane or hold, and every X10's ways from where it starts to its orbit
    and from there home to its launch field, 150 m clear of every no-go area and inside the
    operating area; every X10's order within the reserve rule with a tenth of its budget to spare
    (rules.rs `reserve`: transit, the lap or 15 minutes, the way home, against endurance less 20 %)."""
    areas = json.loads((region.sit.areas / "bay-region.geojson").read_text())
    boxes, ops = [], []
    for f in areas["features"]:
        g = f["geometry"]
        for poly in g["coordinates"] if g["type"] == "MultiPolygon" else [g["coordinates"]]:
            xs, ys = [p[0] for p in poly[0]], [p[1] for p in poly[0]]
            if f["properties"]["kind"] == "no_go":
                boxes.append((min(xs), min(ys), max(xs), max(ys)))
            elif f["properties"]["kind"] == "approved":
                ops.append((min(xs), min(ys), max(xs), max(ys)))
    bad = []
    _, paths = region.reach.plan(region.sit)
    for v in region.vehicles:
        if "start" not in v:
            continue
        points = r_points(v) + (r_ways(v) if v["platform"] == "x10" else [])
        for lon, lat in points:
            if not any(w <= lon <= e and s <= lat <= n for w, s, e, n in ops):
                bad.append((v["callsign"], "outside the operating area", round(lon, 4), round(lat, 4)))
            for w, s, e, n in boxes:
                dx = max(w - lon, 0, lon - e) * 111_320.0 * math.cos(math.radians(lat))
                dy = max(s - lat, 0, lat - n) * 111_320.0
                if math.hypot(dx, dy) < 150:
                    bad.append((v["callsign"], "no_go", round(lon, 4), round(lat, 4)))
    assert not bad, bad[:10]
    x10 = region.catalog["x10"]
    budget = 0.8 * x10["endurance_h"] * 3600
    for v in region.vehicles:
        pa = paths[v["uid"]]
        if v["platform"] != "x10":
            continue
        lap = sum(region.reach.metres(a, b) for a, b in zip(pa["task"], pa["task"][1:]))
        used = (region.reach.metres(*pa["out"]) + region.reach.metres(*pa["back"])) / x10["cruise_mps"] + max(lap / x10["cruise_mps"], 900)
        assert used <= 0.9 * budget, (v["callsign"], v["swarm"], round(used), round(budget))


def test_saturation_region_leaves_the_strike_point_clear_and_jams_three_pickets(region):
    """Every vehicle's own station 1.4 km or more from where the shooter holds, all of SURFACE-3
    within the missile's band (400 m to 6 km) of it, the hold inside an engagement zone; the
    jammer's circle takes in PICKET-9, -10 and -11 whole and no other swarm's stations."""
    jam = (R_JAM["lon"], R_JAM["lat"])
    near, inside = set(), {}
    for v in region.vehicles:
        if "start" not in v:
            continue
        sid = v["swarm"]
        points = r_points(v)
        d = [m_lonlat(p, R_STRIKE) for p in points]
        if min(d) < 1400:
            near.add(sid)
        if sid == "SURFACE-3":
            assert 400 < min(d) and max(d) < 6000, (v["callsign"], min(d), max(d))
        inside.setdefault(sid, []).extend(m_lonlat(p, jam) <= R_JAM["radius_m"] for p in points)
    assert not near, near
    jammed = {sid for sid, ins in inside.items() if all(ins)} | {f"{sid} (partly)" for sid, ins in inside.items() if any(ins) and not all(ins)}
    assert jammed == R_JAMMED, jammed
    # their launch field is inside it too: the failsafe takes them home into the jam
    assert all(m_lonlat([v["home"][1], v["home"][0]], jam) < R_JAM["radius_m"] for sid in R_JAMMED for v in r_members(region, sid))
    events = tomllib.loads(region.sit.events[0].read_text())["event"]
    assert {(e["at_s"], e["duration_s"], e["circle"]["lat"], e["circle"]["lon"], e["circle"]["radius_m"]) for e in events} == {
        (240, 180, R_JAM["lat"], R_JAM["lon"], R_JAM["radius_m"])}
    zones = [f["geometry"]["coordinates"][0] for f in json.loads((region.sit.areas / "bay-region.geojson").read_text())["features"]
             if f["properties"]["kind"] == "engagement_zone"]
    assert any(min(p[0] for p in z) < R_STRIKE[0] < max(p[0] for p in z) and min(p[1] for p in z) < R_STRIKE[1] < max(p[1] for p in z)
               for z in zones)


def test_saturation_region_threat_clears_the_shooter_by_11_30_and_regenerates(region):
    """The region's contacts (scripts/threat_swarms.py --layout region): 500 hostile drones of the
    packed layout's kinds, the shooter ordered at uid 3201 twice from where it hovers, every other
    drone 7 km or more from the shooter's hold by 11:30, and the file what the script writes."""
    raw = tomllib.loads(region.sit.contacts.read_text())
    drones = [c for c in raw["contact"] if c["domain"] == "air"]
    assert len(drones) == 500 and all(c["affiliation"] == "hostile" and 15 <= c["speed_mps"] <= 28 for c in drones)
    kinds = {(c["id"].rsplit("-", 1)[0], c["kind"]) for c in drones if c["id"] != "recon-shooter"}
    assert kinds == {("recon", "small_drone"), ("wave", "small_drone"), ("decoy", "small_drone"), ("attack-east", "oneway_drone"),
                     ("attack-west", "oneway_drone")} and "act" not in raw
    (sc,) = [c for c in drones if c["id"] == "recon-shooter"]
    assert sc["kind"] == "armed_uas" and m_lonlat(sc["route"][-1], R_STRIKE) < 1.0
    assert [(o["at_s"], o["target"], o["stay"]) for o in raw["order"]] == [(595, {"vehicle": 3201}, True), (655, {"vehicle": 3201}, True)]

    def at(c: dict, t: float) -> list[float]:
        left = max(0.0, t - c.get("start_s", 0)) * c["speed_mps"]
        route = c["route"]
        for a, b in zip(route, route[1:]):
            d = m_lonlat(a, b)
            if left <= d:
                return [a[0] + (b[0] - a[0]) * left / d, a[1] + (b[1] - a[1]) * left / d]
            left -= d
        return route[-1]

    # at 11:30, and a minute either side (the contacts take some 25 s to reach speed)
    close = [c["id"] for c in drones if c["id"] != "recon-shooter" for t in (630, 690, 750) if m_lonlat(at(c, t), R_STRIKE) < 7000]
    assert not close, close[:10]
    import subprocess
    import sys

    out = region.sit.contacts.with_name("swarm-saturation-bay-region.regenerated.toml")
    try:
        subprocess.run([sys.executable, str(region.S.REPO / "scripts" / "threat_swarms.py"), "--layout", "region", "-o", str(out)],
                       check=True, capture_output=True)
        assert out.read_text() == region.sit.contacts.read_text()
    finally:
        out.unlink(missing_ok=True)


def test_a_swarm_order_is_asked_again_only_while_facts_are_stale():
    """eval/situation_run.py `stale_only`: an opening order to a hundred vehicles refused only
    because some report was too old is asked again; any other refusal is routed as before."""
    from eval.situation_run import stale_only

    stale = {"outcome": "unverified", "detail": "cannot evaluate: navigation status is 12.0 s old (limit 10 s)"}
    none_yet = {"outcome": "unverified", "detail": "cannot evaluate: no navigation status reported"}
    geofence = {"outcome": "fail", "detail": "return leg (waypoint 1 to home) crosses alcatraz (no-go: Alcatraz Island)"}
    assert stale_only({"would_reach": "rejected", "checks": [stale, none_yet, {"outcome": "pass"}]})
    assert not stale_only({"would_reach": "rejected", "checks": [stale, geofence]})
    assert not stale_only({"would_reach": "auto_approved", "checks": [stale]})
    assert not stale_only({"would_reach": "rejected", "checks": []})
