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
BASE_NAMES = [n for n in F.all_names() if not FO.is_ops(n)]


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
    opts = Options(agent="oracle-v3", operator={"mode": "approve-all"}, calibration=Calibration.load(), scoring="v3",
                   replicate={"r": 0, "seed": 5000, "situation_seed": 5000})
    rec = Episode(e, 5000, tmp_path / "ep", opts, contacts_path=path).run()
    assert rec["scoring"] == "v3" and rec["episode_class"] == "scored" and "v2" not in rec
    assert rec["world"]["name"] == world and rec["world"]["conditions"]["at"] == e.at
    assert rec["v3"]["picture_samples"] > 100
    lab = episode_labels(rec)
    assert lab["version"] == "3" and lab["world"] == world
    detail = rec["agent_detail"]
    assert detail["agent"] == "oracle-v3" and detail["errors"] == 0, rec["agent_log"][-3:]
    if fam != "f18-quiet":
        cid = e.truth["added"][0]
        assert any(row[0] == cid for s in rec["truth"]["samples"] for row in s["contacts"]), "the element moves"
        assert cid in detail["needs"], "the oracle knows the need"
    else:
        assert detail["needs"] == {} and detail["proposals"] == 0, "nothing to do in a quiet watch"
