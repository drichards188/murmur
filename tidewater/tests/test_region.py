"""What a regional scenario adds: nested terrain grids, tide zones of several stations, weather and
wind on a grid with nests (paced requests), DEM tiles, and the bay region's own cache."""

import copy
import json
import math
from pathlib import Path
from types import SimpleNamespace

import numpy as np
import pytest

from tidewater import fetch
from tidewater.conditions import Conditions
from tidewater.layers import NestedTerrain, Terrain, Tides, TimeSeries, point_in_polygon
from tidewater.scenario import CACHE_ROOT, Area, Scenario
from tidewater.weather import Weather

HERE = Path(__file__).resolve().parent
SCENARIOS = HERE.parent / "scenarios"
WEATHER_FIXTURE = HERE.parents[1] / "crates/murmur-weather/tests/data/weather.json"


def test_nested_terrain_answers_from_the_finest_grid():
    rng = np.random.default_rng(4)
    base = Terrain(rng.uniform(-50, 50, (20, 30)), [-2, -1, 2, 1])
    fine = Terrain(rng.uniform(-50, 50, (40, 40)), [-0.5, -0.5, 0.5, 0.5])
    finer = Terrain(rng.uniform(-50, 50, (30, 30)), [0.0, 0.0, 0.3, 0.3])
    nested = NestedTerrain([base, finer, fine])
    lat = rng.uniform(-1.2, 1.2, 2000)
    lon = rng.uniform(-2.2, 2.2, 2000)
    many = nested.elevation_many(lat, lon)
    for la, lo, got in zip(lat, lon, many):
        g = finer if finer.contains(la, lo) else fine if fine.contains(la, lo) else base
        ref = g.elevation(la, lo) if base.contains(la, lo) else None
        assert nested.elevation(la, lo) == ref
        assert (math.isnan(got) and ref is None) or got == pytest.approx(ref, abs=1e-12)


def test_terrain_nests_must_lie_inside_the_area(tmp_path):
    p = tmp_path / "s.toml"
    p.write_text((SCENARIOS / "bay-region-2026-06.toml").read_text().replace("west = -122.62", "west = -124.0"))
    with pytest.raises(ValueError, match="inside the scenario's area"):
        Scenario.load(p).terrain_nests
    names = [n for n, _ in Scenario.load(SCENARIOS / "bay-region-2026-06.toml").terrain_nests]
    assert names == ["bay", "golden-gate"]


def test_point_in_polygon():
    square = [[0, 0], [2, 0], [2, 2], [0, 2]]
    assert point_in_polygon([1, 1, 3, -0.1], [1, 3, 1, 1], square).tolist() == [True, False, False, False]
    notch = [[0, 0], [4, 0], [4, 4], [2, 1], [0, 4]]  # concave: the notch above (2, 1) is outside
    assert point_in_polygon([0.5, 3], [2, 2], notch).tolist() == [True, False]


def tides():
    t = [0.0, 3600.0, 7200.0]
    stations = {"A": (0.0, 0.0, TimeSeries(t, [0.0, 1.0, 0.0])), "B": (0.0, 1.0, TimeSeries(t, [1.0, 2.0, 1.0])),
                "C": (1.0, 0.5, TimeSeries(t, [5.0, 5.0, 5.0]))}
    inner = [[-0.5, -0.5], [1.5, -0.5], [1.5, 0.5], [-0.5, 0.5]]  # lat -0.5..0.5
    return Tides(stations, [("inner", ["A", "B"], inner), ("rest", ["A", "C"], None)], reference="A", nearest=2)


def test_tide_zones_blend_their_own_stations():
    td = tides()
    assert td.level(3600) == 1.0 and td.station == "A"
    assert td.level_at(0.0, 0.0, 3600) == 1.0  # on a station: its own level
    assert td.level_at(0.0, 1.0, 3600) == 2.0
    assert td.level_at(0.0, 0.5, 3600) == pytest.approx(1.5)  # halfway between A and B
    # outside the polygon: A and C only, never B (as Redwood City never reaches Half Moon Bay)
    da2 = 0.75 ** 2 + (0.5 * math.cos(math.radians(0.75))) ** 2
    dc2 = 0.25 ** 2
    assert td.level_at(0.75, 0.5, 3600) == pytest.approx((1.0 / da2 + 5.0 / dc2) / (1 / da2 + 1 / dc2))
    lat = np.random.default_rng(5).uniform(-1, 2, 300)
    lon = np.random.default_rng(6).uniform(-1, 2, 300)
    many = td.level_many(lat, lon, 1234.0)
    assert many == pytest.approx([td.level_at(a, b, 1234.0) for a, b in zip(lat, lon)])
    with pytest.raises(ValueError, match="exactly one zone"):
        Tides(td.stations, [("x", ["A"], None), ("y", ["B"], None)])
    with pytest.raises(ValueError, match="no data"):
        Tides(td.stations, [("x", ["Z"], None)])


def test_conditions_with_tide_zones_batch_equals_points():
    from test_serve import assert_equal_to_at, synthetic
    cond = synthetic()
    cond.tide = tides()
    rng = np.random.default_rng(7)
    lat, lon = rng.uniform(-1.2, 1.2, 300), rng.uniform(-1.2, 1.2, 300)
    assert_equal_to_at(cond, lat, lon, 1500.0, rng.choice([np.nan, 0.0, 4.0], 300))
    levels = cond.sample_many(lat, lon, 1500.0).water_level_m
    assert np.ndim(levels) == 1 and len(set(np.round(levels, 6))) > 10


def test_weather_at_many_equals_at():
    w = Weather.load(WEATHER_FIXTURE)
    d = json.loads(WEATHER_FIXTURE.read_text())
    rng = np.random.default_rng(8)
    # a regional archive: 300 points with their own values
    base = d["points"][0]
    d["points"] = []
    for _ in range(300):
        p = copy.deepcopy(base)
        p["lat"], p["lon"] = float(rng.uniform(37, 38)), float(rng.uniform(-123, -122))
        for r in p["rows"]:
            r["values"]["visibility_m"] = float(rng.uniform(100, 9000))
        d["points"].append(p)
    for w in (w, Weather(d)):
        lat, lon = rng.uniform(36.9, 38.1, 500), rng.uniform(-123.1, -121.9, 500)
        for t in (0.0, 1800.0, 3600.0, 7201.0, float("nan")):
            many = w.at_many(lat, lon, t)
            assert many == [w.at(a, b, t) for a, b in zip(lat, lon)]
            assert np.array_equal(w.vector_many(lat, lon, t),
                                  np.array([w.vector(a, b, t) for a, b in zip(lat, lon)], dtype=float), equal_nan=True)


def wind_scenario(grid, nests=()):
    return SimpleNamespace(area=Area(-123.0, 37.0, -122.0, 38.0, 100), wind={"grid": grid, "nest": list(nests)})


def test_wind_points_grid_then_nests_without_repeats():
    pts = fetch.wind_points(wind_scenario([3, 2], [{"west": -123.0, "south": 37.0, "east": -122.5, "north": 37.5,
                                                     "grid": [2, 2]}]))
    assert pts[:3] == [(37.0, -123.0), (37.0, -122.5), (37.0, -122.0)]
    assert pts[6:] == [(37.5, -123.0), (37.5, -122.5)]  # (37.0, -123.0) and (37.0, -122.5) were already there
    assert fetch.wind_points(wind_scenario([5, 4])) == [(float(a), float(b)) for a in np.linspace(37, 38, 4)
                                                      for b in np.linspace(-123, -122, 5)]


def test_open_meteo_chunks_paced_and_retried(monkeypatch):
    import urllib.error
    calls, sleeps = [], []
    failures = [1]

    def get(url, params):
        lats = params["latitude"].split(",")
        if len(calls) == 1 and failures[0]:
            failures[0] -= 1
            raise urllib.error.HTTPError(url, 429, "Too Many Requests", {}, None)
        calls.append(params)
        return json.dumps([{"i": la} for la in lats] if len(lats) > 1 else {"i": lats[0]}).encode()

    monkeypatch.setattr(fetch, "_get", get)
    monkeypatch.setattr(fetch.time, "sleep", sleeps.append)
    pts = [(37.0 + i / 1000, -122.0) for i in range(250)]
    data, raws = fetch.open_meteo("u", {"hourly": "x"}, pts, log=lambda _: None)
    assert [len(c["latitude"].split(",")) for c in calls] == [100, 100, 50] and len(raws) == 3
    assert [d["i"] for d in data] == [f"{p[0]:.4f}" for p in pts]
    assert sleeps == [12.0, fetch.OPEN_METEO_LIMITED_S, 12.0]  # 100 calls at 500 a minute; a 429's wait
    assert list(calls[0]) == ["latitude", "longitude", "hourly"]  # the order one request always had


def test_station_currents_skip_bad_answers_and_retry_refusals(tmp_path, monkeypatch):
    """CO-OPS refuses a long run of requests for a while (HTTP 403), and answers some bins in words
    where the predictions go: a refused bin is tried again, a bin in words is skipped, not fatal."""
    import urllib.error
    from tidewater.scenario import parse_time
    good = {"current_predictions": {"cp": [{"meanFloodDir": 90, "meanEbbDir": 270, "Time": "2026-06-14 00:00",
                                            "Velocity_Major": 50.0}, {"meanFloodDir": 90, "meanEbbDir": 270,
                                            "Time": "2026-06-14 00:06", "Velocity_Major": 60.0}]}}
    answers = {"A": [good], "B": [urllib.error.HTTPError("u", 403, "Forbidden", {}, None), good],
               "C": [{"current_predictions": {"cp": "Currents are weak and variable"}}]}

    def get(url, params=None):
        if url == fetch.COOPS_META:
            return json.dumps({"stations": [{"id": k, "name": k, "lat": 37.5, "lng": -122.5, "currbin": 1,
                                             "depth": 10} for k in "ABC"]}).encode()
        a = answers[params["station"]].pop(0)
        if isinstance(a, Exception):
            raise a
        return json.dumps(a).encode()

    monkeypatch.setattr(fetch, "_get", get)
    monkeypatch.setattr(fetch.time, "sleep", lambda s: None)
    sc = SimpleNamespace(cache_dir=tmp_path, area=Area(-123, 37, -122, 38, 100), currents={},
                         start=parse_time("2026-06-14"), end=parse_time("2026-06-15"))
    logs = []
    d = json.loads(fetch.fetch_currents(sc, logs.append).read_text())
    assert [s["id"] for s in d["stations"]] == ["A", "B"]
    assert any("skip C" in m for m in logs)


def test_dem_tiles_fall_on_the_whole_grid(monkeypatch):
    """Each tile is asked for exactly its share of the grid: stitched, the tiles are the field
    sampled at the whole grid's cell centres. A one-tile area is the request it always was."""
    def field(lat, lon):
        return np.float32(100 * (lat - 37) + (lon + 122))

    requests = []

    def get(url, params):
        requests.append(params)
        w, s, e, n = (float(v) for v in params["bbox"].split(","))
        cw, ch = (int(v) for v in params["size"].split(","))
        lon = w + (np.arange(cw) + 0.5) * (e - w) / cw
        lat = n - (np.arange(ch) + 0.5) * (n - s) / ch
        import io
        import tifffile
        buf = io.BytesIO()
        tifffile.imwrite(buf, field(lat[:, None], lon[None, :]).astype(np.float32))
        return buf.getvalue()

    monkeypatch.setattr(fetch, "_get", get)
    monkeypatch.setattr(fetch, "DEM_TILE_DEG", 0.05)
    a = Area(-122.6, 37.7, -122.4, 37.83, 400)
    w, h = a.size_px
    grid = fetch.dem_grid(a, log=lambda _: None)
    assert len(requests) > 4
    lon = a.west + (np.arange(w) + 0.5) * (a.east - a.west) / w
    lat = a.north - (np.arange(h) + 0.5) * (a.north - a.south) / h
    assert grid == pytest.approx(field(lat[:, None], lon[None, :]), abs=1e-3)
    requests.clear()
    monkeypatch.setattr(fetch, "DEM_TILE_DEG", 0.5)
    fetch.dem_grid(a, log=lambda _: None)
    assert [r["bbox"] for r in requests] == [f"{a.west},{a.south},{a.east},{a.north}"]


# ---- the bay region's own cache, when fetched

BAY = Scenario.load(SCENARIOS / "bay-region-2026-06.toml")
GG = Scenario.load(SCENARIOS / "golden-gate-2026-06.toml")


needs_bay = pytest.mark.skipif(bool(Conditions.missing(BAY)), reason="bay region scenario not fetched")


@pytest.fixture(scope="module")
def bay():
    return Conditions.for_scenario(BAY)


@needs_bay
def test_bay_region_batch_equals_points(bay):
    from test_serve import assert_equal_to_at
    cond = bay
    rng = np.random.default_rng(9)
    a = BAY.area
    t0, t1 = BAY.start.timestamp(), BAY.end.timestamp()
    lat = np.concatenate([rng.uniform(a.south - 0.05, a.north + 0.05, 250), rng.uniform(37.4, 38.1, 250)])
    lon = np.concatenate([rng.uniform(a.west - 0.05, a.east + 0.05, 250), rng.uniform(-123.1, -122.2, 250)])
    for frac, depth in ((0.0, 0.0), (0.42, 5.0), (0.77, np.nan), (1.0, 60.0)):
        assert_equal_to_at(cond, lat, lon, t0 + frac * (t1 - t0), np.full(len(lat), depth))


@needs_bay
@pytest.mark.skipif(not (CACHE_ROOT / GG.name / "terrain.npz").exists(), reason="golden-gate not fetched")
def test_bay_region_terrain_is_the_golden_gate_inside_its_box(bay):
    gg = Terrain.load(CACHE_ROOT / GG.name / "terrain.npz")
    rng = np.random.default_rng(10)
    a = GG.area
    lat, lon = rng.uniform(a.south, a.north, 2000), rng.uniform(a.west, a.east, 2000)
    assert np.array_equal(bay.terrain.elevation_many(lat, lon), gg.elevation_many(lat, lon))
