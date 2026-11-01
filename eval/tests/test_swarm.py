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
