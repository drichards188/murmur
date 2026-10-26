"""Scoring v3's families (eval/headless/v3/families.py; eval/SCORING-V3.md section 6) on both worlds:
their draws keep each element to its medium on the world's own baked grids, bring it into the
mission's circle, stay inside the world's conditions and state what they drew; and one short episode
per family and world on the real `murmur-headless` binary (built here if stale) with the basic v3
oracle, labelled with labels v3."""

from __future__ import annotations

import shutil
import subprocess
import tomllib
from pathlib import Path

import pytest

from eval.headless import REPO
from eval.headless.episode import Episode, Options
from eval.headless.situations import Calibration, run_id
from eval.headless.v3 import families as F
from eval.headless.v3 import families_ops as FO
from eval.headless.v3.labels import episode_labels
from eval.headless.v3.mission import mission_text
from eval.world import load_world

SEEDS = range(5000, 5012)
#: F1-F6 and F18 (F14-F17, families_ops.py, have their own tests: test_v3_ops.py)
#: F1-F6 and F18 (F7-F13 and F14-F17 have their own tests)
BASE_NAMES = [n for n in F.all_names() if not FO.is_ops(n) and F.extension(F.parse(n)[0]) is None]


def added(path: Path) -> list[dict]:
    raw = tomllib.loads(path.read_text())
    return [c for c in raw["contact"] if c["id"] in ("boat-1", "drone-1", "aircraft-1", "uuv-1", "diver-1", "vehicle-1")]


@pytest.mark.parametrize("name", BASE_NAMES)
def test_every_family_draws_its_element_in_its_medium_and_into_the_circle(name: str, tmp_path: Path):
    fam, world = F.parse(name)
    w = load_world(world)
    for seed in SEEDS:
        e, path = F.generate(name, seed, tmp_path)
        assert e.brief == mission_text(world) and e.opaque and e.map == world and e.world == "v3"
        assert run_id(e, 123).startswith("ep-"), "the run id names no family"
        assert e.family == name and e.situation_seed == seed and e.contacts == path
        cond = e.conditions or {}
        lo, hi = w.conditions["visibility_km"]
        assert lo <= cond["visibility_km"] <= hi and e.at == cond["at"]
        assert e.truth["mission_sha256"] and e.truth["contacts_seed"] == tomllib.loads(path.read_text())["seed"]
        cs = added(path)
        if fam == "f18-quiet":
            assert cs == [] and e.truth["added"] == []
            continue
        assert len(cs) == 1
        c = cs[0]
        route = [tuple(p) for p in c["route"]]
        assert c["affiliation"] in ("unknown", "suspect") and c["domain"] == F.FAMILY_DOMAIN[fam]
        assert F.first_within(route, w.mission.center, w.mission.radius_m) is not None, "it comes into the circle"
        if c["domain"] == "surface":
            assert F.wet(world, route, 3.0)
        elif c["domain"] == "subsurface":
            assert F.wet(world, route, -float(c["alt_m"]) + 1.0), "deeper than its depth"
        elif c["domain"] == "ground":
            assert F.dry(world, route)
        else:
            assert float(c["alt_m"]) >= F.top_surface(world, route, 100.0) + 50.0
        if fam == "f1-fast-boat":
            k = F.first_within(route, w.mission.center, w.mission.radius_m)
            assert F.time_to(k, float(c["speed_mps"])) + 60.0 < 180.0, "no buoy can identify it by its deadline"
        if fam == "f4-uuv":
            assert e.truth["element"]["from"] == "bay"


def test_the_families_are_seeded_and_share_conditions_by_seed(tmp_path: Path):
    a, _ = F.generate("v3-f1-fast-boat-gg", 5003, tmp_path / "a")
    b, _ = F.generate("v3-f1-fast-boat-gg", 5003, tmp_path / "b")
    c, _ = F.generate("v3-f3-low-aircraft-gg", 5003, tmp_path / "c")
    assert a.truth == b.truth and a.conditions == b.conditions
    assert c.conditions == a.conditions, "one seed, the same day or night in every family of a world"


def test_f3_s_aircraft_has_its_transponder_off_so_no_esm_hears_it(tmp_path: Path, binary: None):
    """F3 version 2 (SCORING-V3.md 15 (g)): the contact states `emits = false`, where version 1 left
    the field to the contacts process, whose default for a light aircraft is its transponder on; the
    sensing model's ESM hears the aircraft with it on and not with it off. The families that draw
    F3's aircraft as an element (F7-F13, F8's look-alike, F17) carry it off too."""
    from eval.headless.sensing import SenseModel

    for name in ("v3-f3-low-aircraft-gg", "v3-f3-low-aircraft-sd"):
        for seed in SEEDS:
            e, path = F.generate(name, seed, tmp_path)
            (c,) = added(path)
            assert c["kind"] == "light_aircraft" and c["emits"] is False
            assert e.truth["element"]["version"] == F.F3_VERSION == 2
    drawn = 0
    for name in ("v3-f7-three-at-once-gg", "v3-f8-decoys-sd", "v3-f9-night-fog-gg", "v3-f17-scale-sc"):
        for seed in range(5000, 5006):
            _, path = F.generate(name, seed, tmp_path)
            for x in tomllib.loads(path.read_text())["contact"]:
                if x["id"].startswith(("aircraft-", "decoy-air")) and x["kind"] == "light_aircraft":
                    assert x["emits"] is False, (name, seed, x["id"])
                    drawn += 1
    assert drawn > 5, "F3's aircraft drawn as an element"
    _, path = F.generate("v3-f3-low-aircraft-gg", 5000, tmp_path)
    (c,) = added(path)
    lon, lat = c["route"][0]
    target = {"kind": "light_aircraft", "domain": "air", "lat": lat, "lon": lon, "alt_m": c["alt_m"],
              "speed_mps": c["speed_mps"]}
    observer = {"platform": "corsair", "lat": lat - 0.02, "lon": lon, "alt_m": 0.0}
    clear = {"visibility_m": 20000.0, "sea_state": 2, "sun_elevation_deg": 30.0}
    with SenseModel() as m:
        on = m.predict(observer, {**target, "emits": True}, sensor="esm", conditions=clear)
        off = m.predict(observer, {**target, "emits": c["emits"]}, sensor="esm", conditions=clear)
    assert on[0]["p_detect"] > 0.5, "a transponder on is heard (version 1)"
    assert off[0]["p_detect"] == 0.0, "version 2 emits nothing an ESM receiver can hear"


def test_f6_s_vehicle_appears_outside_the_circle_and_comes_in(tmp_path: Path):
    """F6 version 2 (SCORING-V3.md 15 (h)): the vehicle appears on its road outside the circle and
    comes within it its drawn lead later (F1's 15 to 100 s), on every road of both worlds; version 1
    put it inside the circle on three roads of five."""
    lo, hi = F.F6_LEAD_S
    for name in ("v3-f6-shore-vehicle-gg", "v3-f6-shore-vehicle-sd"):
        _, world = F.parse(name)
        w = load_world(world)
        centre, r = w.mission.center, w.mission.radius_m
        for road, pts in F.PLACES[world].roads:
            k = F.first_within(list(pts), centre, r)
            assert k is not None and k >= F.lead_m(hi, 12.0), f"{road}: long enough outside for the longest lead"
        roads = set()
        for seed in range(5000, 5040):
            e, path = F.generate(name, seed, tmp_path)
            (c,) = added(path)
            el = e.truth["element"]
            route = [tuple(q) for q in c["route"]]
            assert F.dist_m(route[0], centre) > r, (name, seed, "it appears outside the circle")
            k = F.first_within(route, centre, r)
            assert k is not None and F.dry(world, route)
            assert lo <= el["lead_s"] <= hi and abs(F.time_to(k, float(c["speed_mps"])) - el["lead_s"]) < 2.0, \
                "it comes within the circle its drawn lead after it appears"
            assert el["version"] == F.F6_VERSION == 2
            roads.add(el["restricted"])
        assert roads == {n for n, _ in F.PLACES[world].roads}, "every road is drawn"


def test_f5_is_mostly_at_night(tmp_path: Path):
    nights = [F.generate("v3-f5-diver-sd", s, tmp_path)[0].conditions.get("night_by_family", False) for s in range(5000, 5040)]
    assert 0.5 < sum(nights) / len(nights) < 0.9


def test_family_names_round_trip():
    for n in F.all_names():
        fam, world = F.parse(n)
        assert F.family_name(fam, world) == n
    with pytest.raises(ValueError):
        F.parse("v3-f9-night-gg")


# ---- one short episode per family and world -------------------------------------------------------

@pytest.fixture(scope="module")
def binary() -> None:
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    subprocess.run([cargo, "build", "--release", "-q", "-p", "murmur-headless"], cwd=REPO, check=True)


@pytest.mark.parametrize("name", BASE_NAMES)
def test_a_short_episode_per_family_and_world(name: str, tmp_path: Path, binary: None):
    fam, world = F.parse(name)
    e, path = F.generate(name, 5000, tmp_path)
    # long enough for the element to come into the circle, and a minute more
    end = 200.0
    for c in added(path):
        k = F.first_within([tuple(p) for p in c["route"]], load_world(world).mission.center, 3000.0) or 0.0
        end = float(c["start_s"]) + F.time_to(k, float(c["speed_mps"])) + 60.0
    e.duration_s = min(e.duration_s, max(end, 200.0))
    # the ceiling (SCORING-V3.md 13.1: `oracle-v3-shown` since 2026-10-26)
    opts = Options(agent="oracle-v3-shown", operator={"mode": "approve-all"}, calibration=Calibration.load(),
                   scoring="v3", replicate={"r": 0, "seed": 5000, "situation_seed": 5000})
    rec = Episode(e, 5000, tmp_path / "ep", opts, contacts_path=path).run()
    assert rec["scoring"] == "v3" and rec["episode_class"] == "scored" and "v2" not in rec
    assert rec["world"]["name"] == world and rec["world"]["conditions"]["at"] == e.at
    assert rec["v3"]["picture_samples"] > 100
    lab = episode_labels(rec)
    assert lab["version"] == "3" and lab["world"] == world
    detail = rec["agent_detail"]
    assert detail["agent"] == "oracle-v3-shown" and detail["errors"] == 0, rec["agent_log"][-3:]
    if fam != "f18-quiet":
        cid = e.truth["added"][0]
        assert any(row[0] == cid for s in rec["truth"]["samples"] for row in s["contacts"]), "the element moves"
        assert cid in detail["needs"], "the oracle knows the need"
        feas = detail["needs"][cid]["feasibility"]
        assert feas is None or feas["verdict"] in ("not_shown", "fleet_gap", "night", "fog", "conditions", "no_time",
                                                   "terrain", "link", "reserve", "feasible", "unknown"), feas
    else:
        assert detail["needs"] == {} and detail["proposals"] == 0, "nothing to do in a quiet watch"
