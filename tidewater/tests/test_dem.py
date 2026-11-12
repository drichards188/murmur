"""The terrain fetch: tiles of NCEI's DEM mosaic placed where their own georeferencing says they
lie, each cell from the finest DEM that covers it, against a fake service that answers as the real
one does (murmur-cbjn: asked for a box whose pixels are not square in degrees, it widens the box
unless told not to; asked with a mosaicRule's `where`, it mosaics only those DEMs and writes the
noData value where none of them has data)."""

import io
import json
import math
from pathlib import Path
from types import SimpleNamespace

import numpy as np
import pytest
import tifffile

from tidewater import fetch
from tidewater.layers import NestedTerrain, Terrain
from tidewater.scenario import CACHE_ROOT, Area, Scenario

GEOGRAPHIC_WGS84_AREA = ((1024, 2), (1025, 1), (2048, 4326))  # GTModelType, GTRasterType (PixelIsArea), GCS
SCENARIOS = Path(__file__).resolve().parent.parent / "scenarios"


def field(lat, lon):
    """A field whose every row and column differ: a tile misplaced by a fraction of a cell shows."""
    return np.float32(100 * (lat - 37) + (lon + 122))


def geotiff(z, west, north, px, py, keys=GEOGRAPHIC_WGS84_AREA):
    directory = [1, 1, 0, len(keys)] + [v for key, value in keys for v in (key, 0, 1, value)]
    buf = io.BytesIO()
    tifffile.imwrite(buf, z.astype(np.float32), extratags=[
        (33550, "d", 3, (px, py, 0.0)),                     # ModelPixelScale
        (33922, "d", 6, (0.0, 0.0, 0.0, west, north, 0.0)),  # ModelTiepoint: pixel (0, 0)'s corner
        (34735, "H", len(directory), directory),            # GeoKeyDirectory
    ])
    return buf.getvalue()


def everywhere(lat, lon):
    return np.ones(np.broadcast(lat, lon).shape, dtype=bool)


class DemService:
    """exportImage as ArcGIS answers it: `field` at the centres of the pixels it returns, and
    GeoTIFF tags saying where they are. Unless asked `adjustAspectRatio=false` (ArcGIS 11.1 and
    later; `honours=False` is a server that ignores it), it keeps its pixels square in degrees by
    widening the box's shorter side about its middle, as NCEI's does. `cover` says where each of
    fetch.DEM_SOURCES has data, by its index: {index: (covers(lat, lon), metres added)}; by default
    NCEI's 1/9 arc-second tiles cover everything (the Golden Gate's box). A request whose
    mosaicRule names a source gets noData wherever that source has none."""

    def __init__(self, honours=True, cover=None):
        self.honours = honours
        self.cover = cover if cover is not None else {0: (everywhere, 0.0)}
        self.requests = []

    def __call__(self, url, params):
        self.requests.append(params)
        w, s, e, n = (float(v) for v in params["bbox"].split(","))
        cw, ch = (int(v) for v in params["size"].split(","))
        px, py = (e - w) / cw, (n - s) / ch
        if not (self.honours and params.get("adjustAspectRatio") == "false"):
            px = py = max(px, py)
            cx, cy = (w + e) / 2, (s + n) / 2
            w, n = cx - cw * px / 2, cy + ch * py / 2
        lon = w + (np.arange(cw) + 0.5) * px
        lat = n - (np.arange(ch) + 0.5) * py
        z = field(lat[:, None], lon[None, :])
        if "mosaicRule" in params:
            where = json.loads(params["mosaicRule"])["where"]
            k = [w for _, w in fetch.DEM_SOURCES].index(where)
            covers, add = self.cover.get(k, (lambda la, lo: ~everywhere(la, lo), 0.0))
            z = np.where(covers(lat[:, None], lon[None, :]), z + np.float32(add), np.float32(params["noData"]))
        return geotiff(z, w, n, px, py)


def cell_centres(a):
    w, h = a.size_px
    lon = a.west + (np.arange(w) + 0.5) * (a.east - a.west) / w
    lat = a.north - (np.arange(h) + 0.5) * (a.north - a.south) / h
    return lat[:, None], lon[None, :]


def test_dem_tiles_fall_on_the_whole_grid(monkeypatch):
    """Each tile is asked for exactly its share of the grid, as given: stitched, the tiles are the
    field sampled at the whole grid's cell centres. A one-tile area is the request it always was.
    Before murmur-cbjn the service widened every tile's box, and rows here were read up to 0.0056
    degrees of latitude (620 m) from where they lie."""
    dem = DemService()
    monkeypatch.setattr(fetch, "_get", dem)
    monkeypatch.setattr(fetch, "DEM_TILE_DEG", 0.05)
    a = Area(-122.6, 37.7, -122.4, 37.83, 400)
    grid, source, datum = fetch.dem_grid(a, log=lambda _: None)
    assert len(dem.requests) > 4
    assert grid == pytest.approx(field(*cell_centres(a)), abs=1e-3)
    assert (source == 0).all() and datum.startswith("NAVD88")
    assert {r.get("adjustAspectRatio") for r in dem.requests} == {"false"}
    dem.requests.clear()
    monkeypatch.setattr(fetch, "DEM_TILE_DEG", 0.5)
    fetch.dem_grid(a, log=lambda _: None)
    assert [r["bbox"] for r in dem.requests] == [f"{a.west},{a.south},{a.east},{a.north}"]


def test_a_landmark_lands_where_it_is(monkeypatch):
    """A point read back through `Terrain` answers the field there, in the middle of the box and at
    its edges, where the widened box put the Golden Gate's features up to 1.3 km off."""
    monkeypatch.setattr(fetch, "_get", DemService())
    a = Area(-122.56, 37.77, -122.38, 37.88, 60)
    t = Terrain(fetch.dem_grid(a, log=lambda _: None)[0], [a.west, a.south, a.east, a.north])
    rng = np.random.default_rng(3)
    lat = np.concatenate([rng.uniform(a.south + 0.001, a.north - 0.001, 500), [37.8106, 37.8617, 37.7712, 37.8788]])
    lon = np.concatenate([rng.uniform(a.west + 0.001, a.east - 0.001, 500), [-122.4773, -122.4304, -122.47, -122.47]])
    assert t.elevation_many(lat, lon) == pytest.approx(field(lat, lon), abs=1e-3)


def test_a_widened_tile_is_refused(monkeypatch):
    """A server that keeps square degree pixels whatever it is asked (as tidewater met it before
    murmur-cbjn) returns a tile whose georeferencing spans more latitude than the grid's: refused,
    not stretched over the grid's rows."""
    monkeypatch.setattr(fetch, "_get", DemService(honours=False))
    with pytest.raises(RuntimeError, match="cells off"):
        fetch.dem_grid(Area(-122.56, 37.77, -122.38, 37.88, 60), log=lambda _: None)


@pytest.mark.parametrize("keys", [(), ((1024, 2), (1025, 2), (2048, 4326)), ((1024, 2), (1025, 1), (2048, 4269))])
def test_a_tile_not_georeferenced_as_asked_is_refused(monkeypatch, keys):
    """No GeoKeys at all, pixels as points, or another datum: refused rather than guessed at."""
    def get(url, params):
        cw, ch = (int(v) for v in params["size"].split(","))
        return geotiff(np.zeros((ch, cw)), -122.56, 37.88, 0.18 / cw, 0.11 / ch, keys)

    monkeypatch.setattr(fetch, "_get", get)
    with pytest.raises(RuntimeError, match="georeferencing"):
        fetch.dem_grid(Area(-122.56, 37.77, -122.38, 37.88, 60), log=lambda _: None)


def test_terrain_fetched_before_the_fix_is_fetched_again(tmp_path, monkeypatch):
    """A cached grid without `placement` (fetched with its rows squeezed) is fetched again and
    replaced whole; one with it is left alone."""
    dem = DemService()
    monkeypatch.setattr(fetch, "_get", dem)
    a = Area(-122.56, 37.77, -122.38, 37.88, 120)
    sc = SimpleNamespace(cache_dir=tmp_path, area=a, terrain_nests=[], terrain={})
    path = tmp_path / "terrain.npz"
    np.savez_compressed(path, elevation=np.zeros((3, 3), np.float32), bbox=np.array([a.west, a.south, a.east, a.north]))
    assert not fetch.terrain_placed(path)
    logs = []
    fetch.fetch_terrain(sc, logs.append)
    assert any("fetching it again" in m for m in logs)
    assert fetch.terrain_placed(path) and fetch.terrain_sourced(path) and len(dem.requests) == 1
    assert sorted(p.name for p in tmp_path.iterdir()) == ["terrain.npz"]
    with np.load(path) as d:
        assert d["elevation"] == pytest.approx(field(*cell_centres(a)), abs=1e-3)
    fetch.fetch_terrain(sc, logs.append)
    assert len(dem.requests) == 1


def test_terrain_fetched_with_the_default_order_is_fetched_again(tmp_path, monkeypatch):
    """A grid placed right but fetched with the mosaic's default order (ETOPO ahead of the regional
    DEMs: a `placement` key and no `source`) is fetched again, finest DEM first."""
    dem = DemService()
    monkeypatch.setattr(fetch, "_get", dem)
    a = Area(-122.56, 37.77, -122.38, 37.88, 120)
    path = tmp_path / "terrain.npz"
    np.savez_compressed(path, elevation=np.zeros((3, 3), np.float32), bbox=np.array([a.west, a.south, a.east, a.north]),
                        placement=fetch.PLACEMENT)
    assert fetch.terrain_placed(path) and not fetch.terrain_sourced(path)
    logs = []
    fetch.fetch_terrain(SimpleNamespace(cache_dir=tmp_path, area=a, terrain_nests=[], terrain={}), logs.append)
    assert any("default order" in m for m in logs) and len(dem.requests) == 1
    with np.load(path) as d:
        assert (d["source"] == 0).all() and list(d["sources"]) == [name for name, _ in fetch.DEM_SOURCES]


# where each source has data in the next tests: the 1/9 arc-second tiles east of 122.5 W, a 1/3
# arc-second DEM north of 37.78 N, no 1 arc-second DEM, ETOPO everywhere and 0.8 m low (EGM2008's
# zero lies 0.8 m above NAVD 88's)
SPLIT = {0: (lambda lat, lon: lon > -122.5 + 0 * lat, 0.0), 1: (lambda lat, lon: lat > 37.78 + 0 * lon, 0.0),
         3: (everywhere, -0.8)}


def test_each_cell_comes_from_the_finest_dem_that_covers_it(monkeypatch):
    """1/9 arc-second where it has data, else 1/3, else ETOPO raised to NAVD 88; a tile the 1/9
    arc-second tiles fill is one request, as before; the grid says which source each cell took."""
    dem = DemService(cover=SPLIT)
    monkeypatch.setattr(fetch, "_get", dem)
    monkeypatch.setattr(fetch, "DEM_TILE_DEG", 0.05)
    a = Area(-122.6, 37.7, -122.4, 37.83, 400)
    grid, source, datum = fetch.dem_grid(a, log=lambda _: None, etopo_to_navd88_m=0.8)
    lat, lon = cell_centres(a)
    want = np.where(lon > -122.5, 0, np.where(lat > 37.78, 1, fetch.DEM_FILL))
    assert (source == want).all()
    assert grid == pytest.approx(field(lat, lon), abs=1e-3)
    assert "+0.8 m" in datum and datum.startswith("NAVD88")
    asked = {}
    for r in dem.requests:
        assert r["noData"] == "-99999" and r["adjustAspectRatio"] == "false"
        asked.setdefault(r["bbox"], []).append(json.loads(r["mosaicRule"])["where"])
    wheres = [w for _, w in fetch.DEM_SOURCES]
    for bbox, rules in asked.items():
        w = float(bbox.split(",")[0])
        assert rules == wheres[:1] if w >= -122.5 else rules[0] == wheres[0], (bbox, rules)


def test_etopo_beside_navd88_needs_its_offset(monkeypatch):
    """A grid that needs ETOPO beside NCEI's DEMs on NAVD 88 is refused unless the scenario says how
    far EGM2008's zero lies from NAVD 88's there; a grid all ETOPO stays on EGM2008."""
    monkeypatch.setattr(fetch, "_get", DemService(cover=SPLIT))
    a = Area(-122.6, 37.7, -122.4, 37.83, 400)
    with pytest.raises(RuntimeError, match="etopo_to_navd88_m"):
        fetch.dem_grid(a, log=lambda _: None)
    monkeypatch.setattr(fetch, "_get", DemService(cover={3: (everywhere, -0.8)}))
    grid, source, datum = fetch.dem_grid(a, log=lambda _: None)
    assert (source == fetch.DEM_FILL).all() and datum.startswith("EGM2008")
    assert grid == pytest.approx(field(*cell_centres(a)) - 0.8, abs=1e-3)


# ---- the cached grids against NGS survey marks, when fetched (where NCEI's 1/9 arc-second tiles
# answer: in ETOPO's 450 m cells a summit can't be placed to 75 m). Positions NAD 83, heights
# NAVD 88, from NGS datasheets by PID. Before murmur-cbjn the summits lay 0.9 to 4.4 km off.

SUMMITS = {  # scenario: [(PID, lat, lon, height m)]
    "golden-gate-2026-06": [("HT3014", 37.86171264, -122.43036211, 239.0)],  # Mount Livermore, Angel Island
    "bay-region-2026-06": [
        ("HT3014", 37.86171264, -122.43036211, 239.0),
        ("HT0817", 37.80982846, -122.36619552, 104.15),  # Yerba Buena Island
        ("HT3388", 37.92918798, -122.57765865, 782.4),  # Mount Tamalpais, East Peak
        ("HS5120", 37.88169778, -121.91415499, 1177.7),  # Mount Diablo
    ],
}
FORT_POINT = (37.81053463, -122.47727779)  # HT3036, the light on the fort at the point's tip


def cached(name):
    sc = Scenario.load(SCENARIOS / f"{name}.toml")
    paths = [sc.cache_dir / "terrain.npz"] + [sc.cache_dir / f"terrain-{n}.npz" for n, _ in sc.terrain_nests]
    if not all(p.exists() for p in paths):
        pytest.skip(f"{name} not fetched")
    grids = [Terrain.load(p) for p in paths]
    return NestedTerrain(grids) if len(grids) > 1 else grids[0]


def around(lat, lon, r_m, step_m=5.0):
    k = np.arange(-r_m, r_m + step_m / 2, step_m)
    dn, de = np.meshgrid(k, k, indexing="ij")
    keep = np.hypot(dn, de) <= r_m
    return (lat + dn[keep] / 111_320.0, lon + de[keep] / (111_320.0 * math.cos(math.radians(lat))),
            np.hypot(dn[keep], de[keep]))


@pytest.mark.parametrize("name", SUMMITS)
def test_cached_grids_meet_ngs_marks(name):
    """Each summit within 75 m and 15 m of its mark; Fort Point on the shore, not on the bluff."""
    t = cached(name)
    for pid, lat, lon, height in SUMMITS[name]:
        la, lo, d = around(lat, lon, 300)
        z = t.elevation_many(la, lo)
        top = int(np.nanargmax(z))
        assert d[top] < 75 and abs(z[top] - height) < 15, (pid, d[top], z[top], height)
    la, lo, d = around(*FORT_POINT, 300)
    z = t.elevation_many(la, lo)
    assert t.elevation(*FORT_POINT) < 10 and d[z < 0].min() < 80


# ---- where the mosaic's default order served ETOPO 2022's 450 m cells (found under murmur-cbjn),
# the finest DEM now answers: NCEI's 1/3 arc-second DEMs at these marks. The bay region's 90 m grid
# still smooths a narrow headland or an island's summit (Point Reyes Head reads 161 m, the
# Farallones' Lighthouse Hill 48 m), so each mark has its grid's tolerance; the ETOPO grids missed
# every one by more (106, 32, 77 and 58 m). Positions NAD 83, heights NAVD 88, NGS datasheets by PID.

FINEST = {  # scenario: [(PID, lat, lon, height m, tolerance m)]
    "bay-region-2026-06": [  # its 90 m grid
        ("HT3484", 37.9963980750, -123.0148880028, 186.5, 30.0),  # Point Reyes Head
        ("JT2832", 38.3077791667, -123.0633921500, 73.7, 15.0),  # Bodega Head
        ("HT3492", 37.6992269861, -123.0018878694, 88.6, 45.0),  # Southeast Farallon, Lighthouse Hill
    ],
    "san-diego-2026-06": [  # 15 m (Murmur's sim/worlds/san-diego.tidewater.toml)
        ("AC6092", 32.6705561361, -117.2410425917, 126.331, 5.0),  # Point Loma's ridge, LOMA EAST
    ],
}


def cached_by_source(name):
    """A scenario's cached grids (terrain.npz and its nests), each with the source of its cells;
    skipped unless fetched, and fetched from the finest DEM."""
    d = CACHE_ROOT / name
    # terrain.npz (the scenario's area, which decides what is inside) and terrain-<nest>.npz, not
    # the replaced grids kept beside them (terrain.squeezed.npz, terrain.etopo.npz)
    paths = [p for p in d.glob("terrain*.npz") if p.name.count(".") == 1] if d.is_dir() else []
    paths.sort(key=lambda p: (p.name != "terrain.npz", p.name))
    if not paths:
        pytest.skip(f"{name} not fetched")
    if not all(fetch.terrain_sourced(p) for p in paths):
        pytest.skip(f"{name}'s terrain predates the finest-DEM fetch (tidewater fetch fetches it again)")
    out = []
    for p in paths:
        with np.load(p) as d:
            out.append((Terrain(d["elevation"], d["bbox"]), d["source"]))
    return out


@pytest.mark.parametrize("name", FINEST)
def test_cached_grids_take_the_finest_dem_at_ngs_marks(name):
    """At each mark the grid that answers (the finest that covers it) took an NCEI DEM on NAVD 88,
    not ETOPO, and reads the mark's height to its tolerance."""
    grids = cached_by_source(name)
    t = NestedTerrain([g for g, _ in grids]) if len(grids) > 1 else grids[0][0]
    for pid, lat, lon, height, tol in FINEST[name]:
        g, source = min(((g, s) for g, s in grids if g.contains(lat, lon)), key=lambda gs: gs[0].cell_deg)
        r = int((g.north - lat) / (g.north - g.south) * g.h)
        c = int((lon - g.west) / (g.east - g.west) * g.w)
        assert source[r, c] < fetch.DEM_FILL, (pid, fetch.DEM_SOURCES[source[r, c]][0])
        assert abs(t.elevation(lat, lon) - height) < tol, (pid, t.elevation(lat, lon), height)
