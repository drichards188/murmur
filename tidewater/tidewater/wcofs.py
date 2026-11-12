"""NOAA WCOFS (West Coast Operational Forecast System) currents, for the ocean SFBOFS doesn't reach.

WCOFS is a ROMS model of the US West Coast with tides, on a curvilinear grid of about 4 km
(1016 x 348 points, 40 terrain-following levels), run once a day at 03 UTC. NOAA archives it on
AWS Open Data (NODD), public, no key:

    https://noaa-nos-ofs-pds.s3.amazonaws.com/wcofs/netcdf/YYYY/MM/DD/
        wcofs.t03z.YYYYMMDD.2ds.nNNN.nc      hourly surface fields, n001..n024 (74 MB each)
        wcofs.t03z.YYYYMMDD.fields.nNNN.nc   3D fields every 3 hours, n003..n024 (354 MB each)

A cycle's nowcast hour n is n hours after 03 UTC the day before (n024 is the cycle's own 03 UTC).
Both files are chunked and uncompressed, so this module reads only the grid rows over the
scenario area with HTTP range requests (sfbofs.RangeFile): about 3 MB an hour of surface
fields and 28 MB a 3D record (whole HDF5 chunks), instead of 74 and 354 MB.

The layer keeps the wet grid points over the area as nodes, and the model's own wet cells as
triangles (two per wet cell, one where a corner is land), so it never interpolates across land.
Velocities are moved from ROMS's staggered u and v points to the grid points and rotated to
east and north. The current at a depth is the hourly surface current plus the shear below it,
which comes from the 3D fields every 3 hours on fixed depth levels, linear in time: the tide's
hourly turn is kept at every depth, and the 3D structure (the wind-driven surface layer, the
undercurrent) changes slowly. Between WCOFS's 4 km coastline and the real one, a point within
`reach_m` of a wet grid point takes that point's current; past it, the fallback answers.

Needs h5py to fetch (`pip install tidewater[sfbofs]`); a cached layer needs only numpy and scipy.
"""

import datetime as dt
import re
import threading
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import numpy as np
from scipy.spatial import cKDTree

from .layers import to_epoch
from .sfbofs import BUCKET, RangeFile, TriangleLocator, _require_h5py

CYCLE_HOUR = 3  # one cycle a day
PROFILE_STEP_H = 3  # 3D fields every 3 hours
VEL_SCALE = 0.001  # cached velocities are int16 millimetres per second
# depth levels (m below the surface) the 3D shear is kept on; a level below a grid point's seabed
# holds its bottom layer's value
LEVELS = np.array([0, 2, 5, 10, 15, 20, 30, 40, 50, 75, 100, 125, 150, 200, 250, 300, 400, 500, 750, 1000.0])
REACH_M = 6000.0  # about one and a half cells: the coastal strip WCOFS's grid stops short of


# ---------------------------------------------------------------- the layer


def _bracket(times, t):
    """(i0, i1, fraction) of t between two records, clamped to the ends."""
    n = len(times)
    t = min(max(t, times[0]), times[-1])
    i1 = min(max(int(np.searchsorted(times, t)), 1), n - 1) if n > 1 else 0
    i0 = max(i1 - 1, 0)
    a = 0.0 if i1 == i0 else (t - times[i0]) / (times[i1] - times[i0])
    return i0, i1, a


class WcofsCurrents:
    """`Currents`-compatible layer on WCOFS's grid: barycentric inside the wet cells, the nearest
    wet grid point within `reach_m` outside them, linear in time and between depth levels.
    Points beyond reach, and times outside the cached span, go to `fallback` (the station layer)
    when given; otherwise the nearest valid time is used and points beyond reach get zero."""

    def __init__(self, lon, lat, h, tri, levels, t_surf, u_surf, v_surf, t_prof, u_shear, v_shear,
                 fallback=None, reach_m=REACH_M, info=None):
        """lon/lat/h: (N,) grid points and their depth; tri: (E, 3) node indices; levels: (K,) depths;
        t_surf: (T,) POSIX s; u_surf/v_surf: (T, N) east/north m/s at the surface (int16 mm/s or
        float); t_prof: (P,); u_shear/v_shear: (P, K, N) current at each level minus the surface's."""
        self.lon = np.asarray(lon, dtype=float)
        self.lat = np.asarray(lat, dtype=float)
        self.h = np.asarray(h, dtype=float)
        self.tri = np.asarray(tri, dtype=np.intp)
        self.levels = np.asarray(levels, dtype=float)
        self.t_surf = np.asarray(t_surf, dtype=float)
        self.t_prof = np.asarray(t_prof, dtype=float)
        self.u_surf, self.v_surf = np.asarray(u_surf), np.asarray(v_surf)
        self.u_shear, self.v_shear = np.asarray(u_shear), np.asarray(v_shear)
        self.scale = VEL_SCALE if self.u_surf.dtype.kind == "i" else 1.0
        self.fallback = fallback
        self.reach_m = reach_m
        self.info = info or {}
        self.locator = TriangleLocator(self.lon, self.lat, self.tri)
        self._nodes = cKDTree(np.column_stack(self.locator.xy(self.lat, self.lon)))

    @classmethod
    def load(cls, path, fallback=None):
        d = np.load(path, allow_pickle=False)
        return cls(d["lon"], d["lat"], d["h"], d["tri"], d["levels"], d["t_surf"], d["u_surf"], d["v_surf"],
                   d["t_prof"], d["u_shear"], d["v_shear"], fallback=fallback,
                   info={"source": str(d["source"]), "files": int(d["files"])})

    def save(self, path, source="", files=0):
        def mm(a):
            return a if a.dtype.kind == "i" else np.clip(np.rint(a / VEL_SCALE), -32767, 32767).astype(np.int16)

        path = Path(path)
        tmp = path.with_name(path.stem + ".partial.npz")  # an interrupted write must not look like a cache
        np.savez_compressed(tmp, lon=self.lon.astype(np.float32), lat=self.lat.astype(np.float32),
                            h=self.h.astype(np.float32), tri=self.tri.astype(np.int32),
                            levels=self.levels.astype(np.float32), t_surf=self.t_surf, u_surf=mm(self.u_surf),
                            v_surf=mm(self.v_surf), t_prof=self.t_prof, u_shear=mm(self.u_shear),
                            v_shear=mm(self.v_shear), source=source, files=files)
        tmp.replace(path)

    @property
    def time_range(self):
        return max(self.t_surf[0], self.t_prof[0]), min(self.t_surf[-1], self.t_prof[-1])

    def covers(self, lat, lon):
        """Points the layer answers (inside a wet cell, or within reach of a wet grid point)."""
        return self._where(lat, lon)[2]

    def _where(self, lat, lon):
        """-> (nodes (n, 3), weights (n, 3), answered (n,)): a triangle's corners, or the nearest
        node three times with a third of the weight each."""
        elem, w, found = self.locator.locate(lat, lon)
        nodes = self.tri[elem]
        if not found.all():
            px, py = self.locator.xy(np.asarray(lat, dtype=float)[~found], np.asarray(lon, dtype=float)[~found])
            dist, near = self._nodes.query(np.column_stack([px, py]))
            ok = dist <= self.reach_m
            idx = np.flatnonzero(~found)
            nodes[idx] = near[:, None]
            w[idx] = np.where(ok[:, None], 1.0 / 3.0, 0.0)
            found = found.copy()
            found[idx] = ok
        return nodes, w, found

    def _sample(self, nodes, w, t, depth):
        i0, i1, a = _bracket(self.t_surf, t)
        j0, j1, b = _bracket(self.t_prof, t)
        k = len(self.levels)
        f = np.interp(depth, self.levels, np.arange(k))
        k0 = np.minimum(np.floor(f).astype(np.intp), max(k - 2, 0))
        k1 = np.minimum(k0 + 1, k - 1)
        fr = (f - k0)[:, None]
        out = []
        for surf, shear in ((self.u_surf, self.u_shear), (self.v_surf, self.v_shear)):
            s = surf[i0][nodes].astype(float) * (1 - a) + surf[i1][nodes].astype(float) * a
            sh = 0.0
            for j, wt in ((j0, 1 - b), (j1, b)):
                if wt:
                    lo = shear[j][k0[:, None], nodes].astype(float)
                    hi = shear[j][k1[:, None], nodes].astype(float)
                    sh = sh + wt * (lo * (1 - fr) + hi * fr)
            out.append((w * (s + sh)).sum(axis=1) * self.scale)
        return np.stack(out, axis=1)

    def sample_found(self, lat, lon, t, depth_m, strict=True):
        """Currents where the layer answers; when `strict`, only inside its hours (else the nearest
        hour). -> ((N, 2) with zeros elsewhere, answered (N,))."""
        lat = np.asarray(lat, dtype=float)
        lon = np.asarray(lon, dtype=float)
        depth = np.broadcast_to(np.asarray(depth_m, dtype=float), lat.shape)
        out = np.zeros((len(lat), 2))
        if len(lat) == 0:
            return out, np.zeros(0, dtype=bool)
        tt = to_epoch(t)
        lo, hi = self.time_range
        nodes, w, found = self._where(lat, lon)
        use = found & (lo <= tt <= hi or not strict)
        if use.any():
            out[use] = self._sample(nodes[use], w[use], tt, depth[use])
        return out, use

    def vector_many(self, lat, lon, t, depth_m):
        """Currents (east, north) m/s for arrays of points at one time. -> (N, 2)."""
        lat = np.asarray(lat, dtype=float)
        lon = np.asarray(lon, dtype=float)
        depth = np.broadcast_to(np.asarray(depth_m, dtype=float), lat.shape)
        out, use = self.sample_found(lat, lon, t, depth, strict=self.fallback is not None)
        rest = ~use
        if rest.any() and self.fallback is not None:
            out[rest] = self.fallback.vector_many(lat[rest], lon[rest], t, depth[rest])
        return out

    def vector(self, lat, lon, t, depth_m=0.0):
        e, n = self.vector_many([lat], [lon], t, [depth_m])[0]
        return float(e), float(n)


class NestedCurrents:
    """An inner model where its mesh reaches (SFBOFS), an outer one elsewhere (WCOFS), and the
    outer layer's own fallback (the stations) past both. Inside the inner model's open boundary
    (its edges that face open water the outer model covers, not a shore or a river's end) the
    two are blended over `blend_m`: all outer at the boundary, all inner `blend_m` in, so the
    current is continuous where they meet and the inner model's boundary zone, forced from
    outside, gives way to the outer model."""

    PROBES_M = (500.0, 2000.0, 5000.0, 10_000.0)

    def __init__(self, inner, outer, terrain=None, blend_m=10_000.0, probes_m=PROBES_M, step_m=250.0):
        self.inner = inner
        self.outer = outer
        self.blend_m = float(blend_m)
        self.fallback = outer
        self._open = self._open_boundary(terrain, probes_m, step_m)
        self._tree = cKDTree(self._open) if len(self._open) else None

    def _open_boundary(self, terrain, probes_m, step_m):
        """Points every `step_m` along the inner mesh's open edges, in the inner locator's metres.
        An edge is open when it faces open water: probes `probes_m` beyond it are all outside the
        inner mesh, on water (the terrain, when given) and inside the outer model's own cells. A
        shore fails (land beyond it), and so do a river's end (the outer model's cells don't
        reach it) and the rim of a hole in the mesh or a bay shore facing salt ponds or marsh
        (land, or the mesh again, within 10 km)."""
        loc = self.inner.locator
        edges = loc.boundary_edges()
        if len(edges) == 0:
            return np.zeros((0, 2))
        x, y = loc.xy(loc.lat, loc.lon)
        pa = np.column_stack([x[edges[:, 0]], y[edges[:, 0]]])
        pb = np.column_stack([x[edges[:, 1]], y[edges[:, 1]]])
        pc = np.column_stack([x[edges[:, 2]], y[edges[:, 2]]])
        mid = (pa + pb) / 2
        d = pb - pa
        length = np.hypot(d[:, 0], d[:, 1])
        normal = np.column_stack([d[:, 1], -d[:, 0]]) / np.maximum(length, 1e-9)[:, None]
        inward = ((pc - mid) * normal).sum(axis=1) > 0
        normal[inward] *= -1
        is_open = np.ones(len(edges), dtype=bool)
        for probe_m in probes_m:
            probe = mid + normal * probe_m
            plat, plon = loc.latlon(probe[:, 0], probe[:, 1])
            is_open &= ~loc.locate(plat, plon)[2] & self.outer.locator.locate(plat, plon)[2]
            if terrain is not None:
                with np.errstate(invalid="ignore"):
                    is_open &= terrain.elevation_many(plat, plon) < 0  # NaN (outside the terrain) is not water
        pts = []
        for a, b, n in zip(pa[is_open], pb[is_open], np.ceil(length[is_open] / step_m).astype(int)):
            f = np.linspace(0.0, 1.0, max(n, 1) + 1)[:, None]
            pts.append(a + (b - a) * f)
        return np.concatenate(pts) if pts else np.zeros((0, 2))

    def vector_many(self, lat, lon, t, depth_m):
        lat = np.asarray(lat, dtype=float)
        lon = np.asarray(lon, dtype=float)
        depth = np.broadcast_to(np.asarray(depth_m, dtype=float), lat.shape)
        if len(lat) == 0:
            return np.zeros((0, 2))
        out, use = self.inner.sample_found(lat, lon, t, depth)
        if self._tree is not None and self.blend_m > 0 and use.any():
            idx = np.flatnonzero(use)
            px, py = self.inner.locator.xy(lat[idx], lon[idx])
            dist, _ = self._tree.query(np.column_stack([px, py]))
            near = dist < self.blend_m
            if near.any():
                idx, wi = idx[near], dist[near] / self.blend_m
                o, ok = self.outer.sample_found(lat[idx], lon[idx], t, depth[idx])
                idx, wi = idx[ok], wi[ok][:, None]
                out[idx] = wi * out[idx] + (1 - wi) * o[ok]
        rest = ~use
        if rest.any():
            out[rest] = self.outer.vector_many(lat[rest], lon[rest], t, depth[rest])
        return out

    def vector(self, lat, lon, t, depth_m=0.0):
        e, n = self.vector_many([lat], [lon], t, [depth_m])[0]
        return float(e), float(n)


# ---------------------------------------------------------------- the archive


def cycle_for(hour: dt.datetime):
    """(cycle datetime, nowcast index n) of the archived nowcast holding this UTC hour: the first
    03 UTC cycle at or after it, n hours after the cycle before."""
    hour = hour.astimezone(dt.timezone.utc).replace(minute=0, second=0, microsecond=0)
    cyc = hour.replace(hour=CYCLE_HOUR)
    if hour.hour > CYCLE_HOUR:
        cyc += dt.timedelta(days=1)
    return cyc, 24 - int((cyc - hour).total_seconds() // 3600)


def file_url(kind: str, cyc: dt.datetime, n: int):
    """kind: "2ds" (hourly surface) or "fields" (3D, every 3 hours)."""
    return f"{BUCKET}/wcofs/netcdf/{cyc:%Y/%m/%d}/wcofs.t{cyc:%H}z.{cyc:%Y%m%d}.{kind}.n{n:03d}.nc"


def window_hours(start: dt.datetime, end: dt.datetime, step_h=1):
    """Every UTC hour divisible by step_h needed to cover [start, end] by interpolation."""
    h = start.astimezone(dt.timezone.utc).replace(minute=0, second=0, microsecond=0)
    h -= dt.timedelta(hours=h.hour % step_h)
    last = end.astimezone(dt.timezone.utc)
    out = [h]
    while out[-1] < last:
        out.append(out[-1] + dt.timedelta(hours=step_h))
    return out


def _epoch(f):
    """POSIX seconds of the file's single record (ocean_time, "seconds since ...")."""
    units = f["ocean_time"].attrs["units"]
    units = units.decode() if isinstance(units, bytes) else str(units)
    m = re.match(r"seconds since (\d{4}-\d\d-\d\d)[ T]?(\d\d:\d\d:\d\d)?", units)
    if not m:
        raise ValueError(f"WCOFS ocean_time units {units!r}")
    t0 = dt.datetime.fromisoformat(f"{m.group(1)}T{m.group(2) or '00:00:00'}").replace(tzinfo=dt.timezone.utc)
    return t0.timestamp() + float(f["ocean_time"][0])


def _lon180(lon):
    lon = np.asarray(lon, dtype=float)
    return np.where(lon > 180, lon - 360, lon)


class Grid:
    """The part of WCOFS's grid over a box: rows r0..r1 and columns c0..c1 of grid (rho) points,
    the wet ones among them as nodes, and the wet cells as triangles."""

    def __init__(self, r0, r1, c0, c1, lon, lat, h, angle, mask, mask_u, mask_v):
        self.r0, self.r1, self.c0, self.c1 = r0, r1, c0, c1
        self.lon, self.lat, self.h, self.angle = lon, lat, h, angle  # (R, C) over the box
        self.mask_u, self.mask_v = mask_u, mask_v  # (R, C + 1), (R + 1, C): the u and v points around
        self.wet = mask > 0
        self.node = np.full(self.wet.shape, -1)
        self.node[self.wet] = np.arange(int(self.wet.sum()))
        tri = []
        n = self.node
        for j in range(n.shape[0] - 1):
            for i in range(n.shape[1] - 1):
                a, b, c, d = n[j, i], n[j, i + 1], n[j + 1, i], n[j + 1, i + 1]
                corners = [v for v in (a, b, d, c) if v >= 0]
                if len(corners) == 4:
                    tri += [(a, b, d), (a, d, c)]
                elif len(corners) == 3:
                    tri.append(tuple(corners))
        self.tri = np.array(tri, dtype=np.intp).reshape(-1, 3)

    @classmethod
    def from_file(cls, f, west, south, east, north, margin):
        lon = _lon180(f["lon_rho"][:])
        lat = np.asarray(f["lat_rho"][:], dtype=float)
        inside = (lon >= west - margin) & (lon <= east + margin) & (lat >= south - margin) & (lat <= north + margin)
        rows, cols = np.where(inside)
        if len(rows) == 0:
            raise ValueError("no WCOFS grid points inside the scenario area (is it on the US West Coast?)")
        ny, nx = lon.shape
        r0, r1 = max(int(rows.min()), 1), min(int(rows.max()), ny - 2)
        c0, c1 = max(int(cols.min()), 1), min(int(cols.max()), nx - 2)
        box = (slice(r0, r1 + 1), slice(c0, c1 + 1))
        return cls(r0, r1, c0, c1, lon[box], lat[box], np.asarray(f["h"][box], dtype=float),
                   np.asarray(f["angle"][box], dtype=float), np.asarray(f["mask_rho"][box]),
                   np.asarray(f["mask_u"][r0:r1 + 1, c0 - 1:c1 + 1]), np.asarray(f["mask_v"][r0 - 1:r1 + 1, c0:c1 + 1]))

    def to_nodes(self, u, v):
        """u (..., R, C + 1) on u points and v (..., R + 1, C) on v points, land at any value ->
        east and north (..., N) at the wet grid points."""
        mu, mv = self.mask_u > 0, self.mask_v > 0
        u = np.where(mu, u, 0.0)
        v = np.where(mv, v, 0.0)
        ur = (u[..., :, :-1] + u[..., :, 1:]) / np.maximum(mu[:, :-1].astype(float) + mu[:, 1:], 1.0)
        vr = (v[..., :-1, :] + v[..., 1:, :]) / np.maximum(mv[:-1, :].astype(float) + mv[1:, :], 1.0)
        cos, sin = np.cos(self.angle), np.sin(self.angle)
        east = ur * cos - vr * sin
        north = ur * sin + vr * cos
        return east[..., self.wet], north[..., self.wet]

    def read_surface(self, f):
        """An hourly surface record -> (u, v) (N,) m/s east and north."""
        rows = slice(self.r0, self.r1 + 1)
        u = np.asarray(f["u_sur"][0, rows, self.c0 - 1:self.c1 + 1], dtype=float)
        v = np.asarray(f["v_sur"][0, self.r0 - 1:self.r1 + 1, self.c0:self.c1 + 1], dtype=float)
        return self.to_nodes(u, v)

    def read_shear(self, f, levels=LEVELS):
        """A 3D record -> (u, v) shear (K, N) m/s: the current at each level minus the top layer's."""
        rows = slice(self.r0, self.r1 + 1)
        u = np.asarray(f["u"][0, :, rows, self.c0 - 1:self.c1 + 1], dtype=float)  # (L, R, C + 1), bottom first
        v = np.asarray(f["v"][0, :, self.r0 - 1:self.r1 + 1, self.c0:self.c1 + 1], dtype=float)
        east, north = self.to_nodes(u, v)  # (L, N)
        zeta = np.asarray(f["zeta"][0, rows, self.c0:self.c1 + 1], dtype=float)[self.wet]
        zeta = np.where(np.abs(zeta) < 100, zeta, 0.0)
        depth = layer_depths(f, self.h[self.wet], zeta)  # (L, N), deepest first
        out = []
        for comp in (east, north):
            prof = np.empty((len(levels), comp.shape[1]))
            for n in range(comp.shape[1]):
                prof[:, n] = np.interp(levels, depth[::-1, n], comp[::-1, n])  # beyond the ends: the end layers
            out.append(prof - prof[0])
        return out[0], out[1]


def layer_depths(f, h, zeta):
    """Depth below the surface (m, positive) of each s-level's centre at points of depth h and
    surface zeta. -> (L, N), the bottom level first, as ROMS stores them."""
    def scalar(name):
        return float(np.asarray(f[name][()]).ravel()[0])

    s = np.asarray(f["s_rho"][:], dtype=float)[:, None]
    cs = np.asarray(f["Cs_r"][:], dtype=float)[:, None]
    hc = scalar("hc")
    vt = int(scalar("Vtransform"))
    if vt == 2:
        z = zeta + (zeta + h) * (hc * s + h * cs) / (hc + h)
    elif vt == 1:
        z0 = hc * s + (h - hc) * cs
        z = z0 + zeta * (1 + z0 / h)
    else:
        raise ValueError(f"WCOFS Vtransform {vt}")
    return zeta - z


def _missing(e):
    return (f"WCOFS archive has no data for this window ({e}). The NODD archive covers recent history only; "
            "choose a window it covers, or set currents.outside to nothing (station predictions outside SFBOFS).")


def fetch_subset(area, start, end, cache_dir, margin_deg=0.1, workers=4, log=print, opener=RangeFile):
    """Download the WCOFS records covering [start, end] for the box, return the cached npz path.

    Per-record results are kept in cache_dir/wcofs/ so an interrupted fetch resumes. `opener`
    turns a URL into a file object h5py can read (tests pass local files)."""
    h5py = _require_h5py()
    work = cache_dir / "wcofs"
    work.mkdir(parents=True, exist_ok=True)
    hours = window_hours(start, end)
    phours = window_hours(start, end, PROFILE_STEP_H)
    log(f"currents: WCOFS nowcast, {len(hours)} hourly surface and {len(phours)} 3-hourly 3D records "
        f"{hours[0]:%Y-%m-%d %H}Z to {hours[-1]:%Y-%m-%d %H}Z")
    nbytes = [0]
    lock = threading.Lock()

    def open_(url):
        try:
            return opener(url)
        except FileNotFoundError as e:
            raise RuntimeError(_missing(e)) from e

    grid_path = work / "grid.npz"
    if not grid_path.exists():
        rf = open_(file_url("2ds", *cycle_for(hours[0])))
        with h5py.File(rf, "r") as f:
            g = Grid.from_file(f, area.west, area.south, area.east, area.north, margin_deg)
        nbytes[0] += getattr(rf, "bytes", 0)
        np.savez_compressed(grid_path, box=[g.r0, g.r1, g.c0, g.c1], lon=g.lon, lat=g.lat, h=g.h, angle=g.angle,
                            mask=g.wet.astype(np.int8), mask_u=g.mask_u, mask_v=g.mask_v)
    m = np.load(grid_path)
    g = Grid(*(int(v) for v in m["box"]), m["lon"], m["lat"], m["h"], m["angle"], m["mask"], m["mask_u"], m["mask_v"])
    log(f"  grid subset: {int(g.wet.sum())} wet points, {len(g.tri)} triangles")

    def one(item):
        kind, hour = item
        out = work / f"{kind}{int(hour.timestamp())}.npz"
        if out.exists():
            return out
        url = file_url(kind, *cycle_for(hour))
        rf = open_(url)
        # whole chunks in one request each (the 3D fields' chunks are about 2 MB)
        with h5py.File(rf, "r", rdcc_nbytes=64 * 1024 * 1024) as f:
            t = _epoch(f)
            if abs(t - hour.timestamp()) > 1:
                raise RuntimeError(f"{url} holds {dt.datetime.fromtimestamp(t, dt.timezone.utc)}, expected {hour}")
            u, v = g.read_surface(f) if kind == "2ds" else g.read_shear(f)
        np.savez_compressed(out.with_suffix(".tmp.npz"), u=u.astype(np.float32), v=v.astype(np.float32), t=t)
        out.with_suffix(".tmp.npz").replace(out)
        with lock:
            nbytes[0] += getattr(rf, "bytes", 0)
            done = sum(1 for _ in work.glob("*[0-9].npz"))
            if done % 12 == 0:
                log(f"  {done}/{len(hours) + len(phours)} records, {nbytes[0] / 1e6:.0f} MB downloaded")
        return out

    plan = [("2ds", h) for h in hours] + [("fields", h) for h in phours]
    with ThreadPoolExecutor(workers) as ex:
        paths = list(ex.map(one, plan))
    recs = [np.load(p) for p in paths]
    surf, prof = recs[:len(hours)], recs[len(hours):]
    lon, lat = g.lon[g.wet], g.lat[g.wet]
    layer = WcofsCurrents(lon, lat, g.h[g.wet], g.tri, LEVELS,
                          [float(r["t"]) for r in surf], np.stack([r["u"] for r in surf]),
                          np.stack([r["v"] for r in surf]), [float(r["t"]) for r in prof],
                          np.stack([r["u"] for r in prof]), np.stack([r["v"] for r in prof]))
    out = cache_dir / "wcofs_currents.npz"
    layer.save(out, source=f"NOAA WCOFS nowcast (2ds hourly, fields 3-hourly), {BUCKET}/wcofs/netcdf", files=len(paths))
    log(f"  wrote {out.name}: {out.stat().st_size / 1e6:.1f} MB (downloaded {nbytes[0] / 1e6:.0f} MB)")
    return out
