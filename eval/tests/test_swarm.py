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


# ---- Saturation's fleet and orders (sim/situations/swarm-saturation.toml) ---------------------------
# Checked on the Golden Gate's baked grids by world v3's rules (eval/headless/tests/
# test_worlds_v3.py), and against the geometry its director's notes rely on.

STRIKE = (-122.4950, 37.8120)  # where the shooter holds, [lon, lat] (sim/scenarios/swarm-saturation.toml)
JAM = {"lat": 37.8470, "lon": -122.4720, "radius_m": 1800}  # sim/link-events/swarm-saturation.toml
JAMMED = {"PICKET-1", "PICKET-7", "PICKET-8"}


@pytest.fixture(scope="module")
def saturation():
    from eval import situation as S
    from eval.headless.tests.test_worlds_v3 import CATALOG, Grid, along, draft_m

    sit = S.load_situation("swarm-saturation")
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
    """The contacts (scripts/threat_swarms.py): 500 hostile quadcopters, one of them the shooter,
    whose acts are against SURFACE-3's first USV; by 11:30 every other one is 7 km or more from
    the shooter's holding point (an engagement's keep-out counts any air track not declared by an
    operator anywhere it could reach, rules 14), and the file is what the script writes."""
    raw = tomllib.loads(saturation.sit.contacts.read_text())
    quads = [c for c in raw["contact"] if c.get("kind") == "small_drone"]
    assert len(quads) == 500 and all(c["affiliation"] == "hostile" and 15 <= c["speed_mps"] <= 28 for c in quads)
    assert {a["by"] for a in raw["act"]} == {"recon-shooter"} and {a["against_vehicle"] for a in raw["act"]} == {3201}
    shooter = next(c for c in quads if c["id"] == "recon-shooter")["route"][-1]

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
        subprocess.run([sys.executable, str(saturation.sit.contacts.parents[2] / "scripts" / "threat_swarms.py"), "-o", str(out)], check=True, capture_output=True)
        assert out.read_text() == saturation.sit.contacts.read_text()
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
