"""Batch sampling: sample_many and the HTTP service must equal Conditions.at point for point."""

import json
import math
import threading
import urllib.error
import urllib.request

import numpy as np
import pytest

from tidewater.conditions import Conditions
from tidewater.layers import CurrentStation, Currents, Terrain, Tide, TimeSeries, Wind
from tidewater.scenario import CACHE_ROOT, Scenario
from tidewater.serve import Service, make_handler

SCENARIO = Scenario.load(__file__.rsplit("/tests/", 1)[0] + "/scenarios/golden-gate-2026-06.toml")
FIELDS = ("lat", "lon", "elevation_m", "water_level_m", "water_depth_m",
          "current_east", "current_north", "wind_east", "wind_north")


def synthetic():
    rng = np.random.default_rng(1)
    terrain = Terrain(rng.uniform(-20, 8, (30, 40)), [-1, -1, 1, 1])
    tide = Tide(TimeSeries([0, 1000, 2000, 3000], [0.5, 2.0, -0.5, 1.0]))
    stations = []
    for i in range(6):
        bins = []
        for d in (0.0, 3.0, 8.0):
            ts = TimeSeries([0, 500, 1500, 2500, 3000], rng.uniform(-2, 2, 5), cosine=(i % 2 == 0))
            bins.append((d, rng.uniform(0, 360), rng.uniform(0, 360), ts))
        stations.append(CurrentStation(f"S{i}", rng.uniform(-1, 1), rng.uniform(-1, 1), bins))
    wind = Wind([(rng.uniform(-1, 1), rng.uniform(-1, 1),
                  TimeSeries([0, 3000], rng.uniform(-5, 5, 2)), TimeSeries([0, 3000], rng.uniform(-5, 5, 2)))
                 for _ in range(8)])
    return Conditions(terrain, tide, Currents(stations), wind)


def assert_equal_to_at(cond, lat, lon, t, depth):
    batch = cond.sample_many(lat, lon, t, depth)
    rows = batch.to_dicts()
    samples = batch.samples()
    for i in range(len(lat)):
        d = None if np.isnan(depth[i]) else depth[i]
        ref = cond.at(lat[i], lon[i], t, 0.0 if d is None else d).as_dict()
        for k in FIELDS:
            for got in (rows[i][k], getattr(samples[i], k)):
                if ref[k] is None:
                    assert got is None, (i, k)
                else:
                    assert got == pytest.approx(ref[k], abs=1e-9, rel=1e-9), (i, k, t)


def test_sample_many_equals_at_synthetic():
    cond = synthetic()
    rng = np.random.default_rng(2)
    lat = rng.uniform(-1.3, 1.3, 400)  # includes points outside the [-1, 1] area
    lon = rng.uniform(-1.3, 1.3, 400)
    depth = rng.choice([np.nan, 0.0, 1.0, 3.0, 5.0, 12.0, 100.0], 400)
    for t in (-50.0, 0.0, 123.4, 1700.0, 2999.9):
        assert_equal_to_at(cond, lat, lon, t, depth)


def test_sample_many_station_and_edge_cases():
    cond = synthetic()
    st = cond.currents.stations[0]
    # exactly on a station (IDW short-circuit), exactly on the area edge, empty batch
    lat = np.array([st.lat, 1.0, -1.0, 0.0])
    lon = np.array([st.lon, 1.0, -1.0, 1.0])
    assert_equal_to_at(cond, lat, lon, 700.0, np.full(4, 3.0))
    assert len(cond.sample_many([], [], 0.0)) == 0


needs_cache = pytest.mark.skipif(
    not all((CACHE_ROOT / SCENARIO.name / n).exists() for n in ("terrain.npz", "tide.json", "currents.json", "wind.json")),
    reason="golden-gate scenario not fetched")


@needs_cache
def test_sample_many_equals_at_golden_gate():
    cond = Conditions.for_scenario(SCENARIO)
    a = SCENARIO.area
    # grid over and beyond the area: dry land, open water, outside the scenario
    glat, glon = np.meshgrid(np.linspace(a.south - 0.02, a.north + 0.02, 24), np.linspace(a.west - 0.02, a.east + 0.02, 24))
    lat, lon = glat.ravel(), glon.ravel()
    t0 = SCENARIO.start.timestamp()
    span = SCENARIO.end.timestamp() - t0
    for frac, depth in ((0.0, 0.0), (0.1, 5.0), (0.37, 30.0), (0.5, np.nan), (0.83, 2.0), (1.0, 10.0)):
        assert_equal_to_at(cond, lat, lon, t0 + frac * span, np.full(len(lat), depth))
    rng = np.random.default_rng(3)
    assert_equal_to_at(cond, rng.uniform(a.south, a.north, 300), rng.uniform(a.west, a.east, 300),
                       t0 + 0.61 * span, rng.choice([np.nan, 0.0, 4.0, 20.0, 80.0], 300))


@pytest.fixture(scope="module")
def server():
    from http.server import ThreadingHTTPServer
    cond = synthetic()
    sc = Scenario.load(SCENARIO.path)
    import datetime as dt
    sc.start = dt.datetime.fromtimestamp(0, dt.timezone.utc)
    sc.end = dt.datetime.fromtimestamp(3000, dt.timezone.utc)
    srv = ThreadingHTTPServer(("127.0.0.1", 0), make_handler(Service(sc, cond)))
    srv.daemon_threads = True
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    yield f"http://127.0.0.1:{srv.server_address[1]}", cond
    srv.shutdown()
    srv.server_close()


def post(base, body, raw=None):
    data = raw if raw is not None else json.dumps(body).encode()
    req = urllib.request.Request(base + "/v1/sample", data=data, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, json.load(r)
    except urllib.error.HTTPError as e:
        return e.code, json.load(e)


def test_http_sample_matches_at(server):
    base, cond = server
    points = [[0.3, 0.2, None], [0.3, 0.2, 4.0], [5.0, 5.0, None], [-0.9, 0.9, 1], [0.0, 0.0, 0.0]]
    status, out = post(base, {"t": 1234.5, "points": points})
    assert status == 200 and out["t"] == 1234.5 and len(out["samples"]) == len(points)
    for (lat, lon, depth), s in zip(points, out["samples"]):
        ref = cond.at(lat, lon, 1234.5, depth or 0.0).as_dict()
        assert set(s) == set(FIELDS)
        for k in FIELDS:
            assert s[k] == (pytest.approx(ref[k], abs=1e-9) if ref[k] is not None else None)
    assert out["samples"][2]["elevation_m"] is None and out["samples"][2]["water_depth_m"] is None


def test_http_empty_and_info(server):
    base, _ = server
    assert post(base, {"t": 10, "points": []}) == (200, {"t": 10, "samples": []})
    with urllib.request.urlopen(base + "/v1/info") as r:
        info = json.load(r)
    assert info["name"] == SCENARIO.name
    assert set(info["bounds"]) == {"west", "south", "east", "north"}
    assert info["time_range"]["start_unix"] == 0 and info["time_range"]["end_unix"] == 3000


@pytest.mark.parametrize("body", [
    {"points": [[0, 0, None]]},
    {"t": 10},
    {"t": "10", "points": []},
    {"t": True, "points": []},
    {"t": 10, "points": "x"},
    {"t": 10, "points": [[0, 0]]},
    {"t": 10, "points": [[0, "a", None]]},
    {"t": 10, "points": [[0, 0, "deep"]]},
    {"t": 10, "points": [[95, 0, None]]},
    {"t": 10, "points": [[0, 0, None, 1]]},
    [1, 2],
])
def test_http_malformed_is_400(server, body):
    status, out = post(server[0], body)
    assert status == 400 and out["error"]


def test_http_bad_json_and_nan_are_400(server):
    assert post(server[0], None, raw=b"{nope")[0] == 400
    status, out = post(server[0], None, raw=b'{"t": NaN, "points": []}')
    assert status == 400 and "finite" in out["error"]
    assert post(server[0], None, raw=b'{"t": 10, "points": [[Infinity, 0, null]]}')[0] == 400


def test_http_time_out_of_range_names_valid_range(server):
    for t in (-1, 3001):
        status, out = post(server[0], {"t": t, "points": [[0, 0, None]]})
        assert status == 400 and "time range" in out["error"] and "0 to 3000" in out["error"]


def test_http_unknown_path_is_404(server):
    try:
        urllib.request.urlopen(server[0] + "/nope")
    except urllib.error.HTTPError as e:
        assert e.code == 404 and "error" in json.load(e)
