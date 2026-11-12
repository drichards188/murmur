"""The WCOFS reader on a small synthetic ROMS grid, and the SFBOFS + WCOFS composite.

The fixture is a 9 x 8 grid of 4 km cells rotated 0.3 rad from east, with a land corner, written
as WCOFS-shaped HDF5 files (hourly surface `2ds`, 3-hourly 3D `fields`) that a local opener hands
to `fetch_subset` instead of S3. The flow is uniform in space, linear in time and in depth, so
every interpolation the layer does should reproduce it to the cache's 1 mm/s."""

import datetime as dt
import math
import re
from types import SimpleNamespace

import numpy as np
import pytest

h5py = pytest.importorskip("h5py")

from tidewater import wcofs  # noqa: E402
from tidewater.layers import Terrain  # noqa: E402
from tidewater.sfbofs import SfbofsCurrents  # noqa: E402

NY, NX, LAYERS = 9, 8, 10
ANGLE = 0.3
LAT0, LON0 = 37.5, -123.0
DEPTH = 100.0
EPOCH = dt.datetime(2016, 1, 1, tzinfo=dt.timezone.utc).timestamp()
T0 = dt.datetime(2026, 6, 14, tzinfo=dt.timezone.utc).timestamp()
SHEAR = 0.002  # east current falls 2 mm/s per metre of depth
FILL = 1e37


def east0(t):
    return 0.1 + 0.2 * (t - T0) / 3600.0


NORTH0 = -0.05


def rho_grid():
    j, i = np.mgrid[0:NY, 0:NX].astype(float)
    xi, eta = i * 4000.0, j * 4000.0
    e = xi * math.cos(ANGLE) - eta * math.sin(ANGLE)
    n = xi * math.sin(ANGLE) + eta * math.cos(ANGLE)
    lat = LAT0 + n / 111_320.0
    lon = LON0 + e / (111_320.0 * math.cos(math.radians(LAT0)))
    mask = np.ones((NY, NX))
    mask[:3, :2] = 0  # a land corner
    return lon + 360.0, lat, mask  # ROMS files may give longitude 0..360


def grid_vars(f):
    lon, lat, mask = rho_grid()
    f["lon_rho"], f["lat_rho"], f["mask_rho"] = lon, lat, mask
    f["mask_u"] = mask[:, :-1] * mask[:, 1:]
    f["mask_v"] = mask[:-1, :] * mask[1:, :]
    f["h"] = np.full((NY, NX), DEPTH)
    f["angle"] = np.full((NY, NX), ANGLE)
    return mask


def xi_eta(east, north):
    """East/north to ROMS's grid-aligned u (along xi) and v (along eta)."""
    return (east * math.cos(ANGLE) + north * math.sin(ANGLE),
            -east * math.sin(ANGLE) + north * math.cos(ANGLE))


def write_record(path, kind, t):
    with h5py.File(path, "w") as f:
        mask = grid_vars(f)
        f["ocean_time"] = np.array([t - EPOCH])
        f["ocean_time"].attrs["units"] = b"seconds since 2016-01-01 00:00:00"
        f["zeta"] = np.zeros((1, NY, NX))
        mu = (mask[:, :-1] * mask[:, 1:]) > 0
        mv = (mask[:-1, :] * mask[1:, :]) > 0
        s = (np.arange(LAYERS) + 0.5) / LAYERS - 1.0  # bottom first; Cs = s: even layers
        depth = -DEPTH * s  # layer centres, 95 .. 5 m
        if kind == "2ds":
            u, v = xi_eta(east0(t) - SHEAR * depth[-1], NORTH0)
            f["u_sur"] = np.where(mu, u, FILL)[None]
            f["v_sur"] = np.where(mv, v, FILL)[None]
        else:
            f["s_rho"], f["Cs_r"] = s, s
            f["hc"], f["Vtransform"] = np.float64(50.0), np.int32(2)
            u, v = xi_eta(east0(t) - SHEAR * depth, NORTH0)  # (L,)
            f["u"] = np.where(mu[None], u[:, None, None], FILL)[None]
            f["v"] = np.where(mv[None], np.broadcast_to(v, (LAYERS,))[:, None, None], FILL)[None]


def local_opener(tmp_path):
    def opener(url):
        m = re.search(r"wcofs\.t03z\.(\d{8})\.(2ds|fields)\.n(\d{3})\.nc$", url)
        cyc = dt.datetime.strptime(m.group(1), "%Y%m%d").replace(hour=3, tzinfo=dt.timezone.utc)
        t = (cyc - dt.timedelta(hours=24 - int(m.group(3)))).timestamp()
        path = tmp_path / url.rsplit("/", 1)[1]
        if not path.exists():
            write_record(path, m.group(2), t)
        return open(path, "rb")
    return opener


@pytest.fixture(scope="module")
def layer(tmp_path_factory):
    tmp = tmp_path_factory.mktemp("wcofs")
    area = SimpleNamespace(west=-123.2, south=37.4, east=-122.6, north=37.9)
    start = dt.datetime.fromtimestamp(T0, dt.timezone.utc)
    path = wcofs.fetch_subset(area, start, start + dt.timedelta(hours=3), tmp, margin_deg=0.5,
                              log=lambda _: None, opener=local_opener(tmp))
    return wcofs.WcofsCurrents.load(path)


def test_cycle_and_urls():
    utc = dt.timezone.utc
    assert wcofs.cycle_for(dt.datetime(2026, 6, 14, 3, tzinfo=utc)) == (dt.datetime(2026, 6, 14, 3, tzinfo=utc), 24)
    assert wcofs.cycle_for(dt.datetime(2026, 6, 14, 0, tzinfo=utc)) == (dt.datetime(2026, 6, 14, 3, tzinfo=utc), 21)
    assert wcofs.cycle_for(dt.datetime(2026, 6, 14, 4, tzinfo=utc)) == (dt.datetime(2026, 6, 15, 3, tzinfo=utc), 1)
    assert wcofs.file_url("fields", dt.datetime(2026, 6, 15, 3, tzinfo=utc), 9).endswith(
        "/wcofs/netcdf/2026/06/15/wcofs.t03z.20260615.fields.n009.nc")
    hours = wcofs.window_hours(dt.datetime(2026, 6, 14, 1, 30, tzinfo=utc), dt.datetime(2026, 6, 14, 7, tzinfo=utc), 3)
    assert [h.hour for h in hours] == [0, 3, 6, 9]


def test_layer_depths_vtransform2():
    with h5py.File("mem", "w", driver="core", backing_store=False) as f:
        f["s_rho"] = np.array([-0.75, -0.25])
        f["Cs_r"] = np.array([-0.6, -0.1])
        f["hc"], f["Vtransform"] = np.float64(50.0), np.int32(2)
        d = wcofs.layer_depths(f, np.array([100.0, 3000.0]), np.array([0.5, -0.2]))
    for (s, cs), row in zip(((-0.75, -0.6), (-0.25, -0.1)), d):
        for h, zeta, got in zip((100.0, 3000.0), (0.5, -0.2), row):
            z = zeta + (zeta + h) * (50 * s + h * cs) / (50 + h)
            assert got == pytest.approx(zeta - z)


def test_grid_keeps_wet_points_and_cells(layer):
    lon, lat, mask = rho_grid()
    inner = mask[1:-1, 1:-1]  # the reader keeps one row and column in from the file's edges
    assert len(layer.lon) == int(inner.sum())
    # every wet cell is two triangles, a cell with one land corner one
    wet = inner > 0
    corners = wet[:-1, :-1].astype(int) + wet[:-1, 1:] + wet[1:, :-1] + wet[1:, 1:]
    assert len(layer.tri) == 2 * int((corners == 4).sum()) + int((corners == 3).sum())
    assert layer.lon.max() < 0  # longitudes back to -180..180


def test_surface_depth_and_time(layer):
    lon, lat, _ = rho_grid()
    plat, plon = lat[5, 4] + 0.003, lon[5, 4] - 360 + 0.004  # inside a wet cell, off the grid points
    for t in (T0, T0 + 1800, T0 + 3 * 3600):
        for d in (0.0, 2.0, 12.0, 40.0, 60.0, 300.0):
            e, n = layer.vector(plat, plon, t, d)
            assert e == pytest.approx(east0(t) - SHEAR * min(max(d, 5.0), 95.0), abs=2e-3), (t, d)
            assert n == pytest.approx(NORTH0, abs=2e-3)


class Const:
    def __init__(self, e, n):
        self.e, self.n = e, n

    def vector_many(self, lat, lon, t, depth):
        return np.tile([self.e, self.n], (len(lat), 1)).astype(float)


def test_reach_and_fallback(layer):
    layer.fallback = Const(9.0, 9.0)
    try:
        lon, lat, _ = rho_grid()
        # off the grid's west side: 3 km from a wet point (the nearest point's current), then 20 km (fallback)
        near = layer.vector_many([lat[4, 1], lat[4, 1]], [lon[4, 1] - 360 - 0.034, lon[4, 1] - 360 - 0.23], T0, [0, 0])
        assert near[0] == pytest.approx([east0(T0) - SHEAR * 5, NORTH0], abs=2e-3)
        assert list(near[1]) == [9.0, 9.0]
        # outside the cached hours: the fallback, unless there is none (then the nearest hour)
        assert list(layer.vector_many([lat[5, 4]], [lon[5, 4] - 360], T0 + 5 * 3600, [0])[0]) == [9.0, 9.0]
        layer.fallback = None
        e, _ = layer.vector(lat[5, 4], lon[5, 4] - 360, T0 + 5 * 3600)
        assert e == pytest.approx(east0(T0 + 3 * 3600) - SHEAR * 5, abs=2e-3)
    finally:
        layer.fallback = None


def square_mesh(west, south, east, north, n=6):
    lon, lat = np.meshgrid(np.linspace(west, east, n), np.linspace(south, north, n))
    tri = []
    for j in range(n - 1):
        for i in range(n - 1):
            a, b, c, d = j * n + i, j * n + i + 1, (j + 1) * n + i, (j + 1) * n + i + 1
            tri += [(a, b, d), (a, d, c)]
    return lon.ravel(), lat.ravel(), np.array(tri)


def test_nested_blends_across_the_open_boundary_only():
    # inner model (SFBOFS's place): current A on a 0.1 degree square; outer (WCOFS's): B on a larger one
    a, b = np.array([1.0, 0.0]), np.array([0.0, 1.0])
    lon, lat, tri = square_mesh(-123.0, 37.5, -122.9, 37.6)
    t = [0.0, 3600.0]
    inner = SfbofsCurrents(lon, lat, np.full(len(lon), 20.0), tri, [0.5], t, np.full((2, 1, len(lon)), a[0]),
                           np.full((2, 1, len(lon)), a[1]), np.zeros((2, len(lon))))
    olon, olat, otri = square_mesh(-123.3, 37.3, -122.6, 37.8, n=15)
    no = len(olon)
    outer = wcofs.WcofsCurrents(olon, olat, np.full(no, 50.0), otri, [0.0, 10.0], t, np.full((2, no), b[0]),
                                np.full((2, no), b[1]), t, np.zeros((2, 2, no)), np.zeros((2, 2, no)),
                                fallback=Const(9.0, 9.0))
    # water everywhere but east of -122.905: the inner square's east side is a shore
    z = np.full((100, 100), -50.0)
    z[:, np.linspace(-123.5, -122.5, 100) > -122.905] = 10.0
    terrain = Terrain(z, [-123.5, 37.0, -122.5, 38.0])
    nested = wcofs.NestedCurrents(inner, outer, terrain, blend_m=2000.0)
    km_lon = 1 / (111.32 * math.cos(math.radians(37.55)))
    pts = {
        "centre": (37.55, -122.95),                  # 4.4 km from the open sides: the inner model
        "west, 1 km in": (37.55, -123.0 + km_lon),   # halfway through the blend
        "east, 0.5 km in": (37.55, -122.9 - 0.5 * km_lon),  # by the shore: the inner model
        "outside, west": (37.55, -123.05),           # the outer model
        "outside both": (36.0, -123.0),              # the stations
    }
    got = {k: nested.vector(la, lo, 1800.0, 0.0) for k, (la, lo) in pts.items()}
    assert got["centre"] == pytest.approx(tuple(a))
    assert got["east, 0.5 km in"] == pytest.approx(tuple(a))
    w = got["west, 1 km in"]
    assert w[0] == pytest.approx(0.5, abs=0.03) and w[0] + w[1] == pytest.approx(1.0)
    assert got["outside, west"] == pytest.approx(tuple(b))
    assert got["outside both"] == (9.0, 9.0)
    # the batch path answers the same as one point at a time
    lat_, lon_ = zip(*pts.values())
    many = nested.vector_many(lat_, lon_, 1800.0, np.zeros(len(pts)))
    assert many == pytest.approx(np.array(list(got.values())))
