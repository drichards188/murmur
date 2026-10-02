"""SFBOFS currents layer and archive reader: offline, on small synthetic meshes and a local server."""

import datetime as dt
import http.server
import io
import re
import threading

import numpy as np
import pytest

from tidewater import sfbofs
from tidewater.conditions import Conditions
from tidewater.layers import CurrentStation, Currents, Terrain, Tide, TimeSeries, Wind
from tidewater.sfbofs import SfbofsCurrents

UTC = dt.timezone.utc
LAT0, LON0 = 37.8, -122.5
NX, NY, LAYERS = 6, 5, 4
SIGMA = (np.arange(LAYERS) + 0.5) / LAYERS


def grid_mesh():
    """A 6x5 node grid, ~1 km spacing, two triangles per cell. Returns lon, lat, h, tri."""
    ii, jj = np.meshgrid(np.arange(NX), np.arange(NY))
    lon = LON0 + ii.ravel() * 0.0113
    lat = LAT0 + jj.ravel() * 0.009
    h = 10.0 + 4.0 * ii.ravel()  # deepens eastward: 10 .. 30 m
    tri = []
    for j in range(NY - 1):
        for i in range(NX - 1):
            a, b, c, d = j * NX + i, j * NX + i + 1, (j + 1) * NX + i, (j + 1) * NX + i + 1
            tri += [(a, b, d), (a, d, c)]
    return lon, lat, h, np.array(tri)


def layer(times=(0.0, 3600.0), fallback=None):
    """u = linear in position (so barycentric interpolation is exact), scaled by layer and time;
    v = -u/2. zeta is 1 m at every node and time."""
    lon, lat, h, tri = grid_mesh()
    lay = SfbofsCurrents(lon, lat, h, tri, SIGMA, list(times), np.zeros((len(times), LAYERS, len(lon))),
                         np.zeros((len(times), LAYERS, len(lon))), np.ones((len(times), len(lon))), fallback=fallback)
    x, y = lay._xy(lat, lon)
    base = 1.0 + 1e-4 * x + 2e-4 * y
    gain = (1.0 + np.arange(LAYERS))[None, :, None] * (1.0 + np.arange(len(times)))[:, None, None]
    lay.u = gain * base[None, None, :]
    lay.v = -0.5 * lay.u
    return lay, base


def expected_u(lay, lat, lon, k_gain, t_gain):
    x, y = lay._xy(lat, lon)
    return k_gain * t_gain * (1.0 + 1e-4 * x + 2e-4 * y)


def test_linear_field_is_reproduced_inside_the_mesh():
    lay, _ = layer()
    lat, lon = LAT0 + 0.0123, LON0 + 0.0271
    # at layer centre 0 (depth = sigma[0] * water column) the layer gain is 1; t=0 is time gain 1
    column = (10.0 + 4.0 * ((lon - LON0) / 0.0113)) + 1.0
    e, n = lay.vector(lat, lon, 0.0, SIGMA[0] * column)
    assert e == pytest.approx(expected_u(lay, lat, lon, 1.0, 1.0), rel=1e-3)
    assert n == pytest.approx(-0.5 * e, rel=1e-9)


def test_vertical_and_time_interpolation():
    lay, _ = layer()
    lat, lon = LAT0 + 0.0123, LON0 + 0.0271
    column = 10.0 + 4.0 * ((lon - LON0) / 0.0113) + 1.0
    u1 = lay.vector(lat, lon, 0.0, SIGMA[0] * column)[0]
    # halfway between layer centres 0 and 1: gains 1 and 2 -> 1.5
    mid = lay.vector(lat, lon, 0.0, (SIGMA[0] + SIGMA[1]) / 2 * column)[0]
    assert mid == pytest.approx(1.5 * u1, rel=1e-3)
    # above the top layer centre and below the bottom one: clamped, not extrapolated
    assert lay.vector(lat, lon, 0.0, 0.0)[0] == pytest.approx(u1, rel=1e-9)
    assert lay.vector(lat, lon, 0.0, 500.0)[0] == pytest.approx(4.0 * u1, rel=1e-3)
    # halfway through the hour: time gains 1 and 2
    half = lay.vector(lat, lon, 1800.0, SIGMA[0] * column)[0]
    assert half == pytest.approx(1.5 * u1, rel=1e-3)
    assert lay.vector(lat, lon, 3600.0, SIGMA[0] * column)[0] == pytest.approx(2.0 * u1, rel=1e-3)


def test_scalar_equals_vector_many_and_int16_cache_roundtrip(tmp_path):
    lay, _ = layer()
    rng = np.random.default_rng(5)
    lat = rng.uniform(LAT0, LAT0 + 4 * 0.009, 200)
    lon = rng.uniform(LON0, LON0 + 5 * 0.0113, 200)
    depth = rng.uniform(0, 40, 200)
    many = lay.vector_many(lat, lon, 1234.0, depth)
    for i in range(0, 200, 7):
        assert lay.vector(lat[i], lon[i], 1234.0, depth[i]) == pytest.approx(tuple(many[i]), abs=1e-12)
    path = tmp_path / "s.npz"
    lay.save(path, source="test", files=2)
    back = SfbofsCurrents.load(path)
    assert back.u.dtype == np.int16 and back.info["files"] == 2
    again = back.vector_many(lat, lon, 1234.0, depth)
    assert np.abs(again - many).max() < 0.004  # millimetre quantisation of velocities up to ~8 m/s


def test_outside_mesh_and_outside_time_use_fallback():
    flat = TimeSeries([0, 1e10], [1.0, 1.0])
    stations = Currents([CurrentStation("S", LAT0, LON0, [(0.0, 90, 270, flat)])])  # 1 m/s toward east
    lay, _ = layer(fallback=stations)
    inside = (LAT0 + 0.012, LON0 + 0.03)
    outside = (LAT0 + 0.5, LON0)
    assert lay.vector(*outside, 0.0)[0] == pytest.approx(1.0)  # station IDW
    assert lay.vector(*inside, 0.0)[0] != pytest.approx(1.0)  # the mesh
    assert lay.vector(*inside, 7200.0) == pytest.approx((1.0, 0.0), abs=1e-9)  # past the cached hours
    plain, _ = layer()
    assert plain.vector(*outside, 0.0) == (0.0, 0.0)
    # without a fallback an uncovered time clamps to the nearest record
    assert plain.vector(*inside, 7200.0) == pytest.approx(plain.vector(*inside, 3600.0))
    assert len(lay.vector_many([], [], 0.0, [])) == 0


def test_conditions_sample_many_equals_at_with_sfbofs():
    lay, _ = layer()
    terrain = Terrain(np.full((20, 20), -15.0), [LON0 - 0.1, LAT0 - 0.1, LON0 + 0.2, LAT0 + 0.2])
    flat = TimeSeries([0, 1e10], [0.0, 0.0])
    cond = Conditions(terrain, Tide(TimeSeries([0, 1e10], [1.0, 1.0])), lay, Wind([(LAT0, LON0, flat, flat)]))
    rng = np.random.default_rng(1)
    lat = rng.uniform(LAT0 - 0.05, LAT0 + 0.15, 100)  # partly outside the mesh
    lon = rng.uniform(LON0 - 0.05, LON0 + 0.12, 100)
    depth = rng.choice([np.nan, 0.0, 3.0, 12.0], 100)
    batch = cond.sample_many(lat, lon, 900.0, depth)
    for i in range(100):
        ref = cond.at(lat[i], lon[i], 900.0, 0.0 if np.isnan(depth[i]) else depth[i])
        assert batch.current_east[i] == pytest.approx(ref.current_east, abs=1e-9)
        assert batch.current_north[i] == pytest.approx(ref.current_north, abs=1e-9)


def test_elements_to_nodes_averages_wet_elements_only():
    tri = np.array([[0, 1, 2], [1, 2, 3]])
    vals = np.array([[2.0, 4.0]])
    assert elements_to_nodes(vals, tri, 4, None).tolist() == [[2.0, 3.0, 3.0, 4.0]]
    # element 1 dry: its value is ignored, node 3 (only touched by it) is dry -> 0
    assert elements_to_nodes(vals, tri, 4, [1, 0]).tolist() == [[2.0, 2.0, 2.0, 0.0]]


elements_to_nodes = sfbofs.elements_to_nodes


def test_cycles_hours_and_urls():
    cyc, n = sfbofs.cycle_for(dt.datetime(2026, 6, 14, 5, 0, tzinfo=UTC))
    assert (cyc.hour, n) == (9, 2)  # t09z nowcast covers 03..09; 05Z is n002
    cyc, n = sfbofs.cycle_for(dt.datetime(2026, 6, 14, 9, 0, tzinfo=UTC))
    assert (cyc.hour, n) == (9, 6)
    cyc, n = sfbofs.cycle_for(dt.datetime(2026, 6, 14, 22, 0, tzinfo=UTC))
    assert (cyc.day, cyc.hour, n) == (15, 3, 1)  # rolls into the next day's t03z
    assert sfbofs.fields_url(cyc, n).endswith("/sfbofs/netcdf/2026/06/15/sfbofs.t03z.20260615.fields.n001.nc")
    hours = sfbofs.window_hours(dt.datetime(2026, 6, 13, tzinfo=UTC), dt.datetime(2026, 6, 14, 0, 30, tzinfo=UTC))
    assert hours[0].hour == 0 and hours[-1] == dt.datetime(2026, 6, 14, 1, tzinfo=UTC) and len(hours) == 26
    # every hour maps to a distinct (cycle, n) holding exactly that hour
    for h in hours:
        cyc, n = sfbofs.cycle_for(h)
        assert cyc - dt.timedelta(hours=6 - n) == h


# ---- an HTTP server with range support and fake FVCOM files built on the fly

try:
    import h5py
except ImportError:  # the optional extra; the fetch tests need it
    h5py = None
needs_h5py = pytest.mark.skipif(h5py is None, reason="h5py not installed (pip install .[sfbofs])")
FILE_RE = re.compile(r"t(\d\d)z\.(\d{8})\.fields\.n(\d{3})\.nc")


def fake_fvcom(when: dt.datetime):
    """A tiny FVCOM-like fields file: the grid mesh as elements, uniform u = hour of day (cm/s) * k."""
    lon, lat, h, tri = grid_mesh()
    ne = len(tri)
    buf = io.BytesIO()
    with h5py.File(buf, "w") as f:
        f["lon"] = (lon % 360).astype("f4")
        f["lat"] = lat.astype("f4")
        f["h"] = h.astype("f4")
        f["nv"] = (tri + 1).T.astype("i4")
        f["lonc"] = (lon[tri].mean(axis=1) % 360).astype("f4")
        f["latc"] = lat[tri].mean(axis=1).astype("f4")
        f["siglay"] = np.tile(-SIGMA[:, None], (1, len(lon))).astype("f4")
        s = when.strftime("%Y-%m-%dT%H:%M:%S.000000").encode().ljust(26, b"\0")
        f["Times"] = np.frombuffer(s, dtype="S1").reshape(1, 26)
        gain = np.arange(1, LAYERS + 1)[:, None] * 0.01 * when.hour
        f["u"] = np.broadcast_to(gain, (LAYERS, ne)).astype("f4")[None]
        f["v"] = (-0.5 * np.broadcast_to(gain, (LAYERS, ne))).astype("f4")[None]
        wet = np.ones(ne, dtype="i4")
        wet[0] = 0  # one dry element: must not drag its nodes toward zero
        f["u"][0, :, 0] = 0.0
        f["wet_cells"] = wet[None]
        f["zeta"] = np.full((1, len(lon)), 0.5, dtype="f4")
    return buf.getvalue()


class Handler(http.server.BaseHTTPRequestHandler):
    hits = []
    files = {}  # (cycle, n) -> bytes, built in the main thread: h5py holds a global lock while reading

    def log_message(self, *a):
        pass

    def do_GET(self):
        m = FILE_RE.search(self.path)
        if not m or "missing" in self.path:
            self.send_error(404)
            return
        cyc = dt.datetime.strptime(m.group(2) + m.group(1), "%Y%m%d%H").replace(tzinfo=UTC)
        body = Handler.files[(cyc, int(m.group(3)))]
        a, b = re.match(r"bytes=(\d+)-(\d+)", self.headers["Range"]).groups()
        a, b = int(a), min(int(b), len(body) - 1)
        Handler.hits.append((a, b))
        self.send_response(206)
        self.send_header("Content-Range", f"bytes {a}-{b}/{len(body)}")
        self.send_header("Content-Length", str(b - a + 1))
        self.end_headers()
        self.wfile.write(body[a:b + 1])


@pytest.fixture()
def archive(monkeypatch):
    for day in (13, 14, 15):
        for c in sfbofs.CYCLES:
            cyc = dt.datetime(2026, 6, day, c, tzinfo=UTC)
            for n in range(7):
                Handler.files[(cyc, n)] = fake_fvcom(cyc - dt.timedelta(hours=6 - n))
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    srv.daemon_threads = True
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    monkeypatch.setattr(sfbofs, "BUCKET", f"http://127.0.0.1:{srv.server_address[1]}")
    yield srv
    srv.shutdown()
    srv.server_close()


@needs_h5py
def test_range_file_reads_like_a_file(archive):
    cyc = dt.datetime(2026, 6, 14, 9, tzinfo=UTC)
    rf = sfbofs.RangeFile(sfbofs.fields_url(cyc, 6))
    truth = fake_fvcom(cyc)
    assert rf.size == len(truth)
    rf.seek(100)
    assert rf.read(5000) == truth[100:5100]
    rf.seek(-50, io.SEEK_END)
    assert rf.read() == truth[-50:]
    assert rf.read(10) == b""
    with pytest.raises(FileNotFoundError):
        sfbofs.RangeFile(sfbofs.fields_url(cyc, 6).replace("fields", "missing.fields"))


@needs_h5py
def test_fetch_subset_end_to_end(archive, tmp_path):
    from tidewater.scenario import Area

    # an area covering the whole fake mesh; 03Z..06Z on 14 June is t09z n000..n003
    area = Area(LON0 - 0.001, LAT0 - 0.001, LON0 + 0.06, LAT0 + 0.04, 100)
    start = dt.datetime(2026, 6, 14, 3, tzinfo=UTC)
    end = dt.datetime(2026, 6, 14, 5, 30, tzinfo=UTC)
    out = sfbofs.fetch_subset(area, start, end, tmp_path, margin_deg=0.0, workers=2, log=lambda *a: None)
    lay = SfbofsCurrents.load(out)
    assert len(lay.times) == 4 and lay.times[0] == start.timestamp() and np.all(np.diff(lay.times) == 3600)
    assert lay.lon.min() == pytest.approx(LON0, abs=1e-4) and lay.lon.max() < 0  # converted from 0..360
    # u at hour H, layer k is 0.01 * H * (k + 1): sample at 04:30Z, top layer centre
    t = dt.datetime(2026, 6, 14, 4, 30, tzinfo=UTC).timestamp()
    e, n = lay.vector(LAT0 + 0.02, LON0 + 0.03, t, SIGMA[0] * 11.0)
    assert e == pytest.approx(0.01 * 4.5, rel=2e-2) and n == pytest.approx(-0.5 * e, rel=1e-6)
    # a second call is served from the per-hour cache without touching the server
    n_hits = len(Handler.hits)
    sfbofs.fetch_subset(area, start, end, tmp_path, margin_deg=0.0, log=lambda *a: None)
    assert len(Handler.hits) == n_hits


@needs_h5py
def test_missing_archive_window_is_a_clear_error(archive, tmp_path, monkeypatch):
    from tidewater.scenario import Area

    monkeypatch.setattr(sfbofs, "fields_url", lambda c, n: f"{sfbofs.BUCKET}/missing/{c:%Y%m%d}.t{c:%H}z.20200101.fields.n{n:03d}.nc")
    area = Area(LON0, LAT0, LON0 + 0.05, LAT0 + 0.03, 100)
    with pytest.raises(RuntimeError, match="no data for this window"):
        sfbofs.fetch_subset(area, dt.datetime(2020, 1, 1, tzinfo=UTC), dt.datetime(2020, 1, 1, 3, tzinfo=UTC),
                            tmp_path, log=lambda *a: None)


# ---- a real excerpt: SFBOFS nowcast for 09-11Z on 14 June 2026 (max ebb), Golden Gate triangles only

EXCERPT = __file__.rsplit("/", 1)[0] + "/data/sfbofs_gate_excerpt.npz"


def test_real_gate_excerpt_shows_the_ebb_and_its_structure():
    lay = SfbofsCurrents.load(EXCERPT)
    t = dt.datetime(2026, 6, 14, 10, tzinfo=UTC).timestamp()
    assert lay.time_range == (t - 3600, t + 3600)
    mid = (37.8199, -122.4783)  # mid-channel under the Gate
    e, n = lay.vector(*mid, t, 0.0)
    speed, toward = np.hypot(e, n), np.degrees(np.arctan2(e, n)) % 360
    assert 1.6 < speed < 2.1 and 260 < toward < 310  # max ebb: out through the Gate, westward
    # the water column turns with depth (the layer station IDW can only pick by bin)
    assert lay.vector(*mid, t, 40.0)[1] < lay.vector(*mid, t, 0.0)[1] - 0.5
    # half an hour earlier it is a bit different, not a jump
    e2, n2 = lay.vector(*mid, t - 1800, 0.0)
    assert 0 < np.hypot(e2 - e, n2 - n) < 0.5
    # mesh geometry: point locations inside the excerpt are found, one far away is not
    _, _, found = lay.locate([37.8199, 37.8199, 36.0], [-122.4783, -122.47, -122.4783])
    assert found.tolist() == [True, True, False]
