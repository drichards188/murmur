"""The physical sensing model's predictions through `murmur-headless --sense` (eval/headless/sensing.py):
the client answers with the bridge's own functions, which fall with range, end at the horizon and
behind terrain, lose a day camera at night and a small boat to sea clutter, and give D/R/I ranges in
order; a bad request is an error, not a crash."""

from __future__ import annotations

import math
import shutil
import subprocess
from pathlib import Path

import pytest

from eval.headless import REPO
from eval.headless.sensing import SenseError, SenseModel
from eval.headless.sim import BINARY

BASE = (37.82, -122.48)
DAY = {"sun_elevation_deg": 45}
NIGHT = {"sun_elevation_deg": -30}


@pytest.fixture(scope="module")
def model() -> SenseModel:
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    subprocess.run([cargo, "build", "--release", "-q", "-p", "murmur-headless"], cwd=REPO, check=True)
    m = SenseModel(BINARY)
    yield m
    m.close()


def east(m: float, lat: float = BASE[0], lon: float = BASE[1]) -> tuple[float, float]:
    return lat, lon + m / (111_320.0 * math.cos(math.radians(lat)))


def boat(m: float, kind: str = "small_fast_boat") -> dict:
    lat, lon = east(m)
    return {"kind": kind, "domain": "surface", "lat": lat, "lon": lon, "speed_mps": 8}


PUMA = {"platform": "puma", "lat": BASE[0], "lon": BASE[1], "alt_m": 120}
HUNTER = {"platform": "seahunter", "lat": BASE[0], "lon": BASE[1], "alt_m": 0}


def test_identification_falls_with_range_and_the_ranges_are_in_order(model: SenseModel):
    p = [model.predict(PUMA, boat(d), sensor="eo_ir", conditions=DAY)[0] for d in (500, 2000, 6000, 15000)]
    ids = [x["p_identify"] for x in p]
    assert ids == sorted(ids, reverse=True) and ids[0] > 0.9 and ids[-1] < 0.5, ids
    assert all(x["line_of_sight"] and not x["placeholder"] for x in p)
    (r,) = model.predict(PUMA, boat(1000), sensor="eo_ir", conditions=DAY, ranges=True)
    d, rec, i = r["ranges_m"]
    assert d > rec > i > 1000, r["ranges_m"]


def test_night_takes_the_day_camera_and_leaves_the_thermal(model: SenseModel):
    (day,) = model.predict(PUMA, boat(1500), sensor="eo_ir", conditions=DAY)
    (night,) = model.predict(PUMA, boat(1500), sensor="eo_ir", conditions=NIGHT)
    assert day["channel"] == "day" and night["channel"] == "lwir", (day, night)
    assert night["p_identify"] < day["p_identify"]


def test_the_horizon_ends_a_radar_and_sea_state_5_loses_a_small_boat(model: SenseModel):
    (far,) = model.predict(HUNTER, boat(20_000), sensor="radar", conditions=DAY)
    assert not far["line_of_sight"] and far["p_detect"] == 0.0, far
    (calm,) = model.predict(HUNTER, boat(5_000), sensor="radar", conditions={**DAY, "sea_state": 3})
    (rough,) = model.predict(HUNTER, boat(5_000), sensor="radar", conditions={**DAY, "sea_state": 5})
    (ferry,) = model.predict(HUNTER, boat(5_000, "ferry"), sensor="radar", conditions={**DAY, "sea_state": 5})
    assert calm["p_detect"] > 0.5 and rough["p_detect"] == 0.0 and ferry["p_detect"] > 0.99, (calm, rough, ferry)
    assert calm["revisit_s"] <= 5


def test_terrain_masks_with_a_grid(model: SenseModel):
    # from the Golden Gate's south shore to a vessel in the lee of Angel Island's far side: the
    # grid masks what the bare horizon would not
    hunter = {"platform": "seahunter", "lat": 37.808, "lon": -122.477, "alt_m": 0}
    target = {"kind": "ferry", "domain": "surface", "lat": 37.875, "lon": -122.410}
    (bare,) = model.predict(hunter, target, sensor="radar", conditions=DAY)
    (grid,) = model.predict(hunter, target, sensor="radar", conditions=DAY,
                            terrain="sim/headless/terrain/golden-gate.grid.gz")
    assert bare["line_of_sight"] and "terrain_clear" not in bare
    assert grid["terrain_clear"] is False and not grid["line_of_sight"] and grid["p_detect"] == 0.0, grid


def test_a_bad_request_is_an_error(model: SenseModel):
    with pytest.raises(SenseError, match="no platform"):
        model.predict({"platform": "nope", "lat": 0, "lon": 0}, boat(100), conditions=DAY)
    with pytest.raises(SenseError, match="sun_elevation_deg"):
        model.predict(PUMA, boat(100))
    # the process goes on
    assert model.predict(PUMA, boat(500), sensor="eo_ir", conditions=DAY)[0]["p_detect"] > 0.9
