"""NOAA SFBOFS (San Francisco Bay Operational Forecast System) currents.

SFBOFS is an FVCOM model on an unstructured triangular mesh (about 54,000 nodes, 20 terrain
following layers). NOAA archives its output on AWS Open Data (NODD):

    https://noaa-nos-ofs-pds.s3.amazonaws.com/sfbofs/netcdf/YYYY/MM/DD/
        sfbofs.tHHz.YYYYMMDD.fields.nNNN.nc     hourly 3D fields, nowcast, whole bay (56 MB each)

Four cycles a day (03, 09, 15, 21 UTC); a cycle's nowcast files n000..n006 are the hours
HH-6 .. HH. The files are chunked and uncompressed, so this module reads only the bytes it
needs with HTTP range requests: the mesh elements inside the scenario area, only u and v,
about 9 MB per hour instead of 56 MB. The result (nodes and triangles of the area, hourly
u/v/zeta) is cached as one npz, and the layer then runs offline.

Needs h5py to fetch (`pip install tidewater[sfbofs]`); running a cached layer needs only numpy
and scipy.
"""

import datetime as dt
import io
import math
import re
import threading
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor

import numpy as np
from scipy.spatial import cKDTree

from .layers import EARTH_M_PER_DEG, to_epoch

BUCKET = "https://noaa-nos-ofs-pds.s3.amazonaws.com"
CYCLES = (3, 9, 15, 21)
NOWCAST_HOURS = 6  # a cycle's nowcast spans HH-6 .. HH
VEL_SCALE = 0.001  # cached velocities are int16 millimetres per second
UA = {"User-Agent": "tidewater/0.1 (simulation research)"}


# ---------------------------------------------------------------- the layer


class SfbofsCurrents:
    """`Currents`-compatible layer on the SFBOFS mesh.

    Velocities live on nodes (the cache converts FVCOM's element values to node values),
    are interpolated barycentrically inside the triangle that contains the query point,
    linearly between the layer centres in depth, and linearly in time between hourly
    records. Points outside the mesh, and times outside the cached span, go to `fallback`
    (the station IDW layer) when given; otherwise the nearest valid time is used and points
    outside the mesh get zero current.
    """

    SEARCH_K = 24

    def __init__(self, lon, lat, h, tri, sigma, times, u, v, zeta, fallback=None, info=None):
        """lon/lat/h: (N,) node position and still-water depth; tri: (E, 3) node indices;
        sigma: (L,) layer centres as a fraction of depth below the surface (0 top, 1 bottom);
        times: (T,) POSIX seconds ascending; u/v: (T, L, N) east/north m/s (int16 mm/s or
        float); zeta: (T, N) surface elevation."""
        self.lon = np.asarray(lon, dtype=float)
        self.lat = np.asarray(lat, dtype=float)
        self.h = np.asarray(h, dtype=float)
        self.tri = np.asarray(tri, dtype=np.intp)
        self.sigma = np.asarray(sigma, dtype=float)
        self.times = np.asarray(times, dtype=float)
        self.u, self.v = np.asarray(u), np.asarray(v)
        self.zeta = np.asarray(zeta, dtype=float)
        self.scale = VEL_SCALE if self.u.dtype.kind == "i" else 1.0
        self.fallback = fallback
        self.info = info or {}
        self._lat0 = float(self.lat.mean())
        self._lon0 = float(self.lon.mean())
        self._build_locator()

    # -- loading

    @classmethod
    def load(cls, path, fallback=None):
        d = np.load(path, allow_pickle=False)
        info = {"source": str(d["source"]), "files": int(d["files"])} if "source" in d else {}
        return cls(d["lon"], d["lat"], d["h"], d["tri"], d["sigma"], d["times"], d["u"], d["v"], d["zeta"],
                   fallback=fallback, info=info)

    def save(self, path, source="", files=0):
        def mm(a):  # int16 millimetres per second, as the fetch stores it
            return a if a.dtype.kind == "i" else np.clip(np.rint(a / VEL_SCALE), -32767, 32767).astype(np.int16)

        np.savez_compressed(path, lon=self.lon.astype(np.float32), lat=self.lat.astype(np.float32),
                            h=self.h.astype(np.float32), tri=self.tri.astype(np.int32),
                            sigma=self.sigma.astype(np.float32), times=self.times, u=mm(self.u), v=mm(self.v),
                            zeta=self.zeta.astype(np.float32), source=source, files=files)

    @property
    def time_range(self):
        return float(self.times[0]), float(self.times[-1])

    # -- geometry

    def _xy(self, lat, lon):
        k = math.cos(math.radians(self._lat0))
        return (np.asarray(lon, dtype=float) - self._lon0) * EARTH_M_PER_DEG * k, \
               (np.asarray(lat, dtype=float) - self._lat0) * EARTH_M_PER_DEG

    def _build_locator(self):
        x, y = self._xy(self.lat, self.lon)
        tx, ty = x[self.tri], y[self.tri]  # (E, 3)
        self._x0, self._y0 = tx[:, 0], ty[:, 0]
        a, b = tx[:, 1] - tx[:, 0], tx[:, 2] - tx[:, 0]
        c, d = ty[:, 1] - ty[:, 0], ty[:, 2] - ty[:, 0]
        det = a * d - b * c
        det = np.where(np.abs(det) < 1e-9, np.nan, det)  # degenerate triangles never match
        # (l1, l2) = inv([[a, b], [c, d]]) @ (px - x0, py - y0)
        self._inv = (d / det, -b / det, -c / det, a / det)
        self._tree = cKDTree(np.column_stack([tx.mean(axis=1), ty.mean(axis=1)]))

    def locate(self, lat, lon):
        """Containing triangle and barycentric weights. -> (elem (n,), weights (n, 3), found (n,))."""
        px, py = self._xy(lat, lon)
        k = min(self.SEARCH_K, len(self.tri))
        _, cand = self._tree.query(np.column_stack([px, py]), k=k)
        cand = cand.reshape(len(px), k)
        dx, dy = px[:, None] - self._x0[cand], py[:, None] - self._y0[cand]
        i00, i01, i10, i11 = (m[cand] for m in self._inv)
        l1 = i00 * dx + i01 * dy
        l2 = i10 * dx + i11 * dy
        eps = 1e-9
        inside = (l1 >= -eps) & (l2 >= -eps) & (l1 + l2 <= 1 + eps)  # NaN (degenerate) is False
        found = inside.any(axis=1)
        first = np.argmax(inside, axis=1)
        rows = np.arange(len(px))
        l1, l2 = l1[rows, first], l2[rows, first]
        w = np.stack([1 - l1 - l2, l1, l2], axis=1)
        return cand[rows, first], np.where(found[:, None], w, 0.0), found

    # -- sampling

    def _at_record(self, i, nodes, w, depth):
        """Current at time record i for points with triangle nodes (n, 3), weights (n, 3)."""
        total = (self.h[nodes] + self.zeta[i][nodes]) * w
        total = np.maximum(total.sum(axis=1), 0.1)  # water column depth at the point
        layers = len(self.sigma)
        pos = np.clip(depth / total, 0.0, 1.0)
        # fractional layer index between layer centres, clamped to the top and bottom layers
        f = np.interp(pos, self.sigma, np.arange(layers))
        k0 = np.minimum(np.floor(f).astype(np.intp), max(layers - 2, 0))
        k1 = np.minimum(k0 + 1, layers - 1)
        fr = (f - k0)[:, None]
        out = []
        for arr in (self.u, self.v):
            lo = arr[i][k0[:, None], nodes].astype(float)
            hi = arr[i][k1[:, None], nodes].astype(float)
            out.append((w * (lo * (1 - fr) + hi * fr)).sum(axis=1) * self.scale)
        return np.stack(out, axis=1)

    def _sample(self, nodes, w, t, depth):
        n_t = len(self.times)
        t = min(max(t, self.times[0]), self.times[-1])
        i1 = min(max(int(np.searchsorted(self.times, t)), 1), n_t - 1) if n_t > 1 else 0
        i0 = max(i1 - 1, 0)
        a = 0.0 if i1 == i0 else (t - self.times[i0]) / (self.times[i1] - self.times[i0])
        r0 = self._at_record(i0, nodes, w, depth)
        if a == 0.0:
            return r0
        return r0 * (1 - a) + self._at_record(i1, nodes, w, depth) * a

    def vector_many(self, lat, lon, t, depth_m):
        """Currents (east, north) m/s for arrays of points at one time. -> (N, 2)."""
        lat = np.asarray(lat, dtype=float)
        lon = np.asarray(lon, dtype=float)
        depth = np.broadcast_to(np.asarray(depth_m, dtype=float), lat.shape)
        out = np.zeros((len(lat), 2))
        if len(lat) == 0:
            return out
        tt = to_epoch(t)
        elem, w, found = self.locate(lat, lon)
        covered = self.times[0] <= tt <= self.times[-1]
        use = found & (covered or self.fallback is None)
        if use.any():
            out[use] = self._sample(self.tri[elem[use]], w[use], tt, depth[use])
        rest = ~use
        if rest.any() and self.fallback is not None:
            out[rest] = self.fallback.vector_many(lat[rest], lon[rest], t, depth[rest])
        return out

    def vector(self, lat, lon, t, depth_m=0.0):
        e, n = self.vector_many([lat], [lon], t, [depth_m])[0]
        return float(e), float(n)


# ---------------------------------------------------------------- the archive


def cycle_for(hour: dt.datetime):
    """(cycle datetime, nowcast index n) of the archived nowcast file holding this UTC hour:
    the first cycle at or after it."""
    hour = hour.astimezone(dt.timezone.utc).replace(minute=0, second=0, microsecond=0)
    for c in CYCLES:
        if hour.hour <= c:
            cyc = hour.replace(hour=c)
            break
    else:
        cyc = (hour + dt.timedelta(days=1)).replace(hour=CYCLES[0])
    return cyc, NOWCAST_HOURS - int((cyc - hour).total_seconds() // 3600)


def fields_url(cyc: dt.datetime, n: int):
    return (f"{BUCKET}/sfbofs/netcdf/{cyc:%Y/%m/%d}/sfbofs.t{cyc:%H}z.{cyc:%Y%m%d}.fields.n{n:03d}.nc")


def window_hours(start: dt.datetime, end: dt.datetime):
    """Every whole UTC hour needed to cover [start, end] by interpolation."""
    h = start.astimezone(dt.timezone.utc).replace(minute=0, second=0, microsecond=0)
    last = end.astimezone(dt.timezone.utc)
    out = []
    while h < last or not out:
        out.append(h)
        h += dt.timedelta(hours=1)
    out.append(h)  # the hour at or after the end
    return out


class RangeFile(io.RawIOBase):
    """Read-only file object over HTTP range requests, for h5py. Small reads are served from
    256 KiB blocks; big reads (HDF5 chunks) go straight through. Counts bytes transferred."""

    BLOCK = 256 * 1024

    def __init__(self, url, retries=4):
        self.url = url
        self.retries = retries
        self.pos = 0
        self.bytes = 0
        self._blocks = {}
        self.size = self._head()

    def _open(self, req):
        for attempt in range(self.retries):
            try:
                return urllib.request.urlopen(req, timeout=120)
            except urllib.error.HTTPError as e:
                if e.code in (403, 404):
                    raise FileNotFoundError(f"{self.url}: HTTP {e.code}") from e
                if attempt == self.retries - 1:
                    raise
            except OSError:
                if attempt == self.retries - 1:
                    raise
            time.sleep(1.5 * (attempt + 1))

    def _head(self):
        with self._open(urllib.request.Request(self.url, headers={**UA, "Range": "bytes=0-0"})) as r:
            return int(r.headers["Content-Range"].rsplit("/", 1)[1])

    def _range(self, a, b):
        """Bytes [a, b)."""
        req = urllib.request.Request(self.url, headers={**UA, "Range": f"bytes={a}-{b - 1}"})
        for attempt in range(self.retries):
            try:
                with self._open(req) as r:
                    data = r.read()
                if len(data) == b - a:
                    self.bytes += len(data)
                    return data
            except FileNotFoundError:
                raise
            except OSError:
                if attempt == self.retries - 1:
                    raise
            time.sleep(1.5 * (attempt + 1))
        raise OSError(f"short read from {self.url}")

    def readable(self):
        return True

    def seekable(self):
        return True

    def tell(self):
        return self.pos

    def seek(self, off, whence=io.SEEK_SET):
        self.pos = off if whence == io.SEEK_SET else self.pos + off if whence == io.SEEK_CUR else self.size + off
        return self.pos

    def readinto(self, buf):
        n = min(len(buf), max(self.size - self.pos, 0))
        if n == 0:
            return 0
        if n >= self.BLOCK:
            buf[:n] = self._range(self.pos, self.pos + n)
        else:
            got = 0
            while got < n:
                p = self.pos + got
                bi = p // self.BLOCK
                if bi not in self._blocks:
                    if len(self._blocks) > 64:
                        self._blocks.pop(next(iter(self._blocks)))
                    self._blocks[bi] = self._range(bi * self.BLOCK, min((bi + 1) * self.BLOCK, self.size))
                blk = self._blocks[bi]
                off = p - bi * self.BLOCK
                take = min(n - got, len(blk) - off)
                buf[got:got + take] = blk[off:off + take]
                got += take
        self.pos += n
        return n


def _require_h5py():
    try:
        import h5py
    except ImportError as e:
        raise ImportError("SFBOFS fetching needs h5py: pip install 'tidewater[sfbofs]'") from e
    return h5py


def _lon180(lon):
    lon = np.asarray(lon, dtype=float)
    return np.where(lon > 180, lon - 360, lon)


def _file_time(f):
    """UTC POSIX seconds of the single record in an SFBOFS fields file."""
    s = bytes(f["Times"][0]).decode().strip("\x00 ")
    return dt.datetime.fromisoformat(s).replace(tzinfo=dt.timezone.utc).timestamp()


class Subset:
    """The part of the SFBOFS mesh inside a box: nodes, triangles and the element range to read."""

    def __init__(self, lon, lat, h, tri, elems, sigma):
        self.lon, self.lat, self.h, self.tri, self.elems, self.sigma = lon, lat, h, tri, elems, sigma

    @classmethod
    def from_file(cls, f, west, south, east, north, margin):
        lonc, latc = _lon180(f["lonc"][:]), np.asarray(f["latc"][:], dtype=float)
        sel = np.where((lonc >= west - margin) & (lonc <= east + margin)
                       & (latc >= south - margin) & (latc <= north + margin))[0]
        if len(sel) == 0:
            raise ValueError("no SFBOFS mesh elements inside the scenario area (is it in San Francisco Bay?)")
        nv = f["nv"][:][:, sel].T.astype(np.intp) - 1  # (E, 3) global node ids, 1-based in the file
        nodes, inv = np.unique(nv, return_inverse=True)
        lon = _lon180(f["lon"][:])[nodes]
        lat = np.asarray(f["lat"][:], dtype=float)[nodes]
        h = np.asarray(f["h"][:], dtype=float)[nodes]
        sigma = -np.asarray(f["siglay"][:, 0], dtype=float)  # layer centres, positive down
        return cls(lon, lat, h, inv.reshape(-1, 3), sel, sigma), nodes


def elements_to_nodes(vals, tri, n_nodes, wet=None):
    """Average element values (..., E) onto the nodes of those elements (..., N), over the
    wet elements around each node; nodes with no wet element get 0."""
    e = tri.shape[0]
    wgt = np.ones(e) if wet is None else np.asarray(wet, dtype=float)
    rows = tri.ravel()
    cols = np.repeat(np.arange(e), 3)
    from scipy.sparse import csr_matrix

    a = csr_matrix((wgt[cols], (rows, cols)), shape=(n_nodes, e))  # node <- element weights
    norm = np.asarray(a.sum(axis=1)).ravel()
    flat = np.asarray(vals, dtype=float).reshape(-1, e)
    out = (a @ flat.T).T / np.where(norm > 0, norm, 1.0)
    return out.reshape(vals.shape[:-1] + (n_nodes,))


def read_hour(f, sub: "Subset", nodes_global):
    """One hourly record from an open fields file -> (u, v) int16 (L, N) mm/s, zeta float32 (N,)."""
    e0, e1 = int(sub.elems.min()), int(sub.elems.max()) + 1
    pick = sub.elems - e0
    wet = np.asarray(f["wet_cells"][0, e0:e1])[pick] > 0
    n = len(sub.lon)
    out = []
    for name in ("u", "v"):
        el = np.asarray(f[name][0, :, e0:e1])[:, pick]  # (L, E)
        el = np.where(wet[None, :], el, 0.0)
        node = elements_to_nodes(el, sub.tri, n, wet)
        out.append(np.clip(np.rint(node / VEL_SCALE), -32767, 32767).astype(np.int16))
    zeta = np.asarray(f["zeta"][0])[nodes_global].astype(np.float32)
    return out[0], out[1], zeta


def _missing(e):
    return (f"SFBOFS archive has no data for this window ({e}). The NODD archive covers recent history "
            "only; choose a window it covers, or keep currents.source = 'coops-predictions'.")


def fetch_subset(area, start, end, cache_dir, margin_deg=0.02, workers=4, log=print):
    """Download the SFBOFS hours covering [start, end] for the box, return the cached npz path.

    Per-hour results are kept in cache_dir/sfbofs/ so an interrupted fetch resumes."""
    h5py = _require_h5py()
    work = cache_dir / "sfbofs"
    work.mkdir(parents=True, exist_ok=True)
    hours = window_hours(start, end)
    plan = [(h, *cycle_for(h)) for h in hours]
    log(f"currents: SFBOFS nowcast, {len(hours)} hourly records {hours[0]:%Y-%m-%d %H}Z to {hours[-1]:%Y-%m-%d %H}Z")

    mesh_path = work / "mesh.npz"
    nbytes = [0]
    lock = threading.Lock()
    if not mesh_path.exists():
        try:
            rf = RangeFile(fields_url(plan[0][1], plan[0][2]))
        except FileNotFoundError as e:
            raise RuntimeError(_missing(e)) from e
        with h5py.File(rf, "r") as f:
            sub, nodes = Subset.from_file(f, area.west, area.south, area.east, area.north, margin_deg)
        nbytes[0] += rf.bytes
        np.savez_compressed(mesh_path, lon=sub.lon, lat=sub.lat, h=sub.h, tri=sub.tri, elems=sub.elems,
                            sigma=sub.sigma, nodes=nodes)
    m = np.load(mesh_path)
    sub = Subset(m["lon"], m["lat"], m["h"], m["tri"], m["elems"], m["sigma"])
    nodes = m["nodes"]
    log(f"  mesh subset: {len(sub.lon)} nodes, {len(sub.tri)} triangles, {len(sub.sigma)} layers")

    def one(item):
        hour, cyc, n = item
        out = work / f"h{int(hour.timestamp())}.npz"
        if out.exists():
            return out
        url = fields_url(cyc, n)
        rf = RangeFile(url)
        with h5py.File(rf, "r") as f:
            t = _file_time(f)
            if abs(t - hour.timestamp()) > 1:
                raise RuntimeError(f"{url} holds {dt.datetime.fromtimestamp(t, dt.timezone.utc)}, expected {hour}")
            u, v, z = read_hour(f, sub, nodes)
        np.savez_compressed(out.with_suffix(".tmp.npz"), u=u, v=v, zeta=z, t=t)
        out.with_suffix(".tmp.npz").replace(out)
        with lock:
            nbytes[0] += rf.bytes
            done = sum(1 for p in work.glob("h*.npz"))
            if done % 12 == 0:
                log(f"  {done}/{len(hours)} hours, {nbytes[0] / 1e6:.0f} MB downloaded")
        return out

    try:
        with ThreadPoolExecutor(workers) as ex:
            paths = list(ex.map(one, plan))
    except FileNotFoundError as e:
        raise RuntimeError(_missing(e)) from e
    recs = [np.load(p) for p in paths]
    layer = SfbofsCurrents(sub.lon, sub.lat, sub.h, sub.tri, (np.arange(len(sub.sigma)) + 0.5) / len(sub.sigma),
                           [float(r["t"]) for r in recs], np.stack([r["u"] for r in recs]),
                           np.stack([r["v"] for r in recs]), np.stack([r["zeta"] for r in recs]))
    out = cache_dir / "sfbofs_currents.npz"
    layer.save(out, source=f"NOAA SFBOFS nowcast fields, {BUCKET}/sfbofs/netcdf", files=len(paths))
    log(f"  wrote {out.name}: {out.stat().st_size / 1e6:.1f} MB (downloaded {nbytes[0] / 1e6:.0f} MB)")
    return out
