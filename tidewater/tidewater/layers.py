"""Data layers: each answers a question about one place at one time.

Conventions used everywhere in tidewater:
  - positions are (lat, lon) in degrees, WGS84
  - times are UTC datetimes or POSIX seconds
  - vectors are (east, north) in m/s, pointing where the water or air is GOING
  - heights are metres in the scenario's vertical datum (NAVD88 for US coastal DEMs);
    depth below the surface is a positive number
"""

import datetime as dt
import json
import math

import numpy as np

EARTH_M_PER_DEG = 111_320.0


def to_epoch(t):
    if isinstance(t, (int, float)):
        return float(t)
    if t.tzinfo is None:
        t = t.replace(tzinfo=dt.timezone.utc)
    return t.timestamp()


def _parse_series_times(rows, fmt):
    return np.array([dt.datetime.strptime(r[0], fmt).replace(tzinfo=dt.timezone.utc).timestamp() for r in rows])


def heading_vector(speed, toward_deg):
    a = math.radians(toward_deg)
    return speed * math.sin(a), speed * math.cos(a)


def local_xy_m(lat0, lat, lon, lat_ref, lon_ref):
    """Flat-earth metres from a reference point; fine over a ~20 km scenario."""
    k = math.cos(math.radians(lat0))
    return (lon - lon_ref) * EARTH_M_PER_DEG * k, (lat - lat_ref) * EARTH_M_PER_DEG


class Terrain:
    """Merged topography/bathymetry grid. elevation > 0 is land above the datum."""

    def __init__(self, elevation, bbox):
        self.z = np.asarray(elevation, dtype=np.float32)
        self.west, self.south, self.east, self.north = (float(v) for v in bbox)
        self.h, self.w = self.z.shape

    @classmethod
    def load(cls, path):
        d = np.load(path)
        return cls(d["elevation"], d["bbox"])

    def contains(self, lat, lon):
        return self.south <= lat <= self.north and self.west <= lon <= self.east

    def elevation(self, lat, lon):
        """Bilinear sample. Cell centres sit half a cell in from the bbox edges."""
        if not self.contains(lat, lon):
            return None
        fx = (lon - self.west) / (self.east - self.west) * self.w - 0.5
        fy = (self.north - lat) / (self.north - self.south) * self.h - 0.5
        x0 = int(np.clip(math.floor(fx), 0, self.w - 2))
        y0 = int(np.clip(math.floor(fy), 0, self.h - 2))
        tx = float(np.clip(fx - x0, 0, 1))
        ty = float(np.clip(fy - y0, 0, 1))
        z = self.z
        top = z[y0, x0] * (1 - tx) + z[y0, x0 + 1] * tx
        bot = z[y0 + 1, x0] * (1 - tx) + z[y0 + 1, x0 + 1] * tx
        return float(top * (1 - ty) + bot * ty)

    def elevation_many(self, lat, lon):
        """Vectorized `elevation`: NaN outside the area. Same float32 arithmetic as the scalar path."""
        lat = np.asarray(lat, dtype=float)
        lon = np.asarray(lon, dtype=float)
        inside = (self.south <= lat) & (lat <= self.north) & (self.west <= lon) & (lon <= self.east)
        fx = (np.where(inside, lon, self.west) - self.west) / (self.east - self.west) * self.w - 0.5
        fy = (self.north - np.where(inside, lat, self.north)) / (self.north - self.south) * self.h - 0.5
        x0 = np.clip(np.floor(fx), 0, self.w - 2).astype(np.intp)
        y0 = np.clip(np.floor(fy), 0, self.h - 2).astype(np.intp)
        tx = np.clip(fx - x0, 0, 1)
        ty = np.clip(fy - y0, 0, 1)
        f32 = np.float32
        z = self.z
        # the scalar path multiplies float32 samples by Python floats, which numpy keeps in float32
        top = z[y0, x0] * f32(1 - tx) + z[y0, x0 + 1] * f32(tx)
        bot = z[y0 + 1, x0] * f32(1 - tx) + z[y0 + 1, x0 + 1] * f32(tx)
        out = (top * f32(1 - ty) + bot * f32(ty)).astype(float)
        out[~inside] = np.nan
        return out

    @property
    def cell_deg(self):
        """Cell height in degrees of latitude: what orders grids from finest to coarsest."""
        return (self.north - self.south) / self.h


class NestedTerrain:
    """Several `Terrain` grids, each point answered from the finest grid that covers it. The first
    grid is the scenario's area (it decides what is inside); the others are finer grids nested in
    it, so a large area can be coarse where nothing needs more and fine where vehicles work near
    the shore. Where two grids meet, the answer steps from one to the other."""

    def __init__(self, grids):
        grids = list(grids)
        self.base = grids[0]
        self.grids = sorted(grids, key=lambda g: g.cell_deg)  # finest first; stable for equal cells
        self.west, self.south, self.east, self.north = self.base.west, self.base.south, self.base.east, self.base.north

    def contains(self, lat, lon):
        return self.base.contains(lat, lon)

    def elevation(self, lat, lon):
        if not self.contains(lat, lon):
            return None
        for g in self.grids:
            if g.contains(lat, lon):
                return g.elevation(lat, lon)
        return None

    def elevation_many(self, lat, lon):
        lat = np.asarray(lat, dtype=float)
        lon = np.asarray(lon, dtype=float)
        out = np.full(lat.shape, np.nan)
        todo = (self.south <= lat) & (lat <= self.north) & (self.west <= lon) & (lon <= self.east)
        for g in self.grids:
            if not todo.any():
                break
            here = todo & (g.south <= lat) & (lat <= g.north) & (g.west <= lon) & (lon <= g.east)
            if here.any():
                out[here] = g.elevation_many(lat[here], lon[here])
                todo &= ~here
        return out


class TimeSeries:
    """Values over time. `cosine=True` is for sparse tidal events (slack, max flood,
    slack, max ebb...): it follows a half-cosine between events, NOAA's standard
    approximation for currents between published slack and maximum times."""

    def __init__(self, times, values, cosine=False):
        self.t = np.asarray(times, dtype=float)
        self.v = np.asarray(values, dtype=float)
        self.cosine = cosine

    def at(self, t):
        x = to_epoch(t)
        if not self.cosine or x <= self.t[0] or x >= self.t[-1]:
            return float(np.interp(x, self.t, self.v))
        i = int(np.searchsorted(self.t, x)) - 1
        f = (x - self.t[i]) / (self.t[i + 1] - self.t[i])
        return float(self.v[i] + (self.v[i + 1] - self.v[i]) * (1 - math.cos(math.pi * f)) / 2)


class Tide:
    """Water level at the scenario's reference station, applied across the whole area.
    (A single station is the main simplification: real tides lag and grow across a bay.)"""

    def __init__(self, series: TimeSeries, station=None, datum=None):
        self.series = series
        self.station = station
        self.datum = datum

    @classmethod
    def load(cls, path):
        d = json.loads(open(path).read())
        rows = d["series"]
        return cls(TimeSeries(_parse_series_times(rows, "%Y-%m-%d %H:%M"), [r[1] for r in rows]),
                   d.get("station"), d.get("datum"))

    def level(self, t):
        return self.series.at(t)

    def level_at(self, lat, lon, t):
        return self.series.at(t)

    def level_many(self, lat, lon, t):
        """One level for every point: a float, as `level`."""
        return self.series.at(t)


def point_in_polygon(lat, lon, polygon):
    """Even-odd rule for arrays of points. polygon: [[lon, lat], ...], closed implicitly."""
    lat = np.asarray(lat, dtype=float)
    lon = np.asarray(lon, dtype=float)
    inside = np.zeros(lat.shape, dtype=bool)
    poly = np.asarray(polygon, dtype=float)
    x1, y1 = poly[:, 0], poly[:, 1]
    x2, y2 = np.roll(x1, -1), np.roll(y1, -1)
    for ax, ay, bx, by in zip(x1, y1, x2, y2):
        crosses = (ay > lat) != (by > lat)
        with np.errstate(divide="ignore", invalid="ignore"):
            x_at = ax + (lat - ay) * (bx - ax) / (by - ay)
        inside ^= crosses & (lon < x_at)
    return inside


class Tides:
    """Water level from several stations. The area is split into zones (a polygon each, and one
    zone without a polygon for the rest); a point takes the inverse-distance blend of the
    `nearest` stations of its zone. Zones keep a station from reaching across land into another
    water body: Redwood City's South Bay tide never reaches Half Moon Bay, 25 km away over the
    hills. Between stations the blend is an approximation (a real tide's lag and range change
    along the channels, not with straight-line distance)."""

    def __init__(self, stations, zones, reference=None, nearest=2, power=2.0, datum=None):
        """stations: {id: (lat, lon, TimeSeries)}; zones: [(name, [station ids], polygon or None)],
        a polygon [[lon, lat], ...]; the first zone whose polygon holds a point is its zone, else
        the zone without one. reference: the station `level(t)` answers for."""
        self.stations = stations
        self.zones = []
        default = [z for z in zones if z[2] is None]
        if len(default) != 1:
            raise ValueError("tide zones: exactly one zone has no polygon (the rest of the area)")
        for name, ids, polygon in zones:
            missing = [i for i in ids if i not in stations]
            if missing or not ids:
                raise ValueError(f"tide zone {name!r}: no data for stations {missing or ids}")
            self.zones.append((name, list(ids), polygon))
        self.reference = reference or next(iter(stations))
        self.station = self.reference
        self.nearest = nearest
        self.power = power
        self.datum = datum

    @classmethod
    def load(cls, path, zones, reference=None, nearest=2):
        d = json.loads(open(path).read())
        stations = {s["id"]: (s["lat"], s["lon"], TimeSeries(_parse_series_times(s["series"], "%Y-%m-%d %H:%M"),
                                                                [r[1] for r in s["series"]]))
                    for s in d["stations"]}
        return cls(stations, zones, reference, nearest, datum=d.get("datum"))

    def level(self, t):
        """The reference station's level."""
        return self.stations[self.reference][2].at(t)

    def zone_of(self, lat, lon):
        """Index into `zones` for arrays of points."""
        lat = np.asarray(lat, dtype=float)
        lon = np.asarray(lon, dtype=float)
        out = np.full(lat.shape, -1)
        for i, (_, _, polygon) in enumerate(self.zones):
            if polygon is not None:
                out[(out < 0) & point_in_polygon(lat, lon, polygon)] = i
        out[out < 0] = next(i for i, z in enumerate(self.zones) if z[2] is None)
        return out

    def level_many(self, lat, lon, t):
        lat = np.asarray(lat, dtype=float)
        lon = np.asarray(lon, dtype=float)
        out = np.zeros(lat.shape)
        if lat.size == 0:
            return out
        zone = self.zone_of(lat, lon)
        for i, (_, ids, _) in enumerate(self.zones):
            here = zone == i
            if here.any():
                pts = [self.stations[s][:2] for s in ids]
                vals = [(self.stations[s][2].at(t),) for s in ids]
                out[here] = idw_many(pts, vals, lat[here], lon[here], k=self.nearest, power=self.power)[:, 0]
        return out

    def level_at(self, lat, lon, t):
        return float(self.level_many([lat], [lon], t)[0])



class CurrentStation:
    def __init__(self, sid, lat, lon, bins):
        """bins: list of (depth_m, flood_dir, ebb_dir, TimeSeries of signed speed m/s)."""
        self.id = sid
        self.lat = lat
        self.lon = lon
        self.bins = sorted(bins, key=lambda b: b[0])

    def vector(self, t, depth_m=0.0):
        return self.vector_bin(min(self.bins, key=lambda b: abs(b[0] - depth_m)), t)

    @staticmethod
    def vector_bin(bin_, t):
        _, flood, ebb, ts = bin_
        v = ts.at(t)
        return heading_vector(abs(v), flood if v >= 0 else ebb)


def idw_many(points, values, lat, lon, k=4, power=2.0):
    """Vectorized `idw` for arrays of query points. values: (S, C) shared by all queries,
    or (N, S, C) per query. Returns (N, C)."""
    lat = np.asarray(lat, dtype=float)
    lon = np.asarray(lon, dtype=float)
    plat = np.array([p[0] for p in points], dtype=float)
    plon = np.array([p[1] for p in points], dtype=float)
    values = np.asarray(values, dtype=float)
    n, s = len(lat), len(plat)
    if values.ndim == 2:
        values = np.broadcast_to(values, (n,) + values.shape)
    kc = np.cos(np.radians(lat))[:, None]
    dx = (plon[None, :] - lon[:, None]) * EARTH_M_PER_DEG * kc
    dy = (plat[None, :] - lat[:, None]) * EARTH_M_PER_DEG
    d = np.hypot(dx, dy)  # (N, S)
    if s > 64:  # hundreds of points (a regional wind grid, 175 current stations): partition, then order the k
        part = np.argpartition(d, min(k, s) - 1, axis=1)[:, :min(k, s)]
        idx = np.take_along_axis(part, np.argsort(np.take_along_axis(d, part, axis=1), axis=1), axis=1)
    else:
        idx = np.argsort(d, axis=1)[:, :min(k, s)]
    dk = np.take_along_axis(d, idx, axis=1)
    vk = np.take_along_axis(values, idx[:, :, None], axis=1)  # (N, k, C)
    with np.errstate(divide="ignore", invalid="ignore"):  # a point on a station: replaced below
        w = 1.0 / dk ** power
        w = w / w.sum(axis=1, keepdims=True)
        out = (w[:, :, None] * vk).sum(axis=1)
    near = dk[:, 0] < 1.0
    out[near] = vk[near, 0]
    return out


def idw(points, values, lat, lon, k=4, power=2.0):
    """Inverse-distance weighting over the k nearest points. points: [(lat, lon)]."""
    d = np.array([math.hypot(*local_xy_m(lat, plat, plon, lat, lon)) for plat, plon in points])
    idx = np.argsort(d)[:k]
    if d[idx[0]] < 1.0:
        return values[idx[0]]
    w = 1.0 / d[idx] ** power
    w /= w.sum()
    return tuple(float(sum(w[i] * values[j][c] for i, j in enumerate(idx))) for c in range(len(values[0])))


class Currents:
    """Tidal current from NOAA prediction stations, interpolated between stations.

    v0 limitation: inverse-distance weighting doesn't know about channels, so currents
    between stations are approximate. Replacing this layer with NOAA's SFBOFS model
    output (a hydrodynamic mesh) is the planned upgrade."""

    def __init__(self, stations):
        self.stations = stations

    @classmethod
    def load(cls, path):
        d = json.loads(open(path).read())
        stations = []
        for s in d["stations"]:
            bins = []
            for b in s["bins"]:
                if b["flood_dir"] is None or b["ebb_dir"] is None or len(b["series"]) < 2:
                    continue  # no published axis (e.g. Raccoon Strait): can't make a vector
                times = _parse_series_times(b["series"], "%Y-%m-%d %H:%M")
                sparse = float(np.median(np.diff(times))) > 30 * 60  # subordinate stations: events only
                ts = TimeSeries(times, [r[1] for r in b["series"]], cosine=sparse)
                bins.append((b["depth_m"], b["flood_dir"], b["ebb_dir"], ts))
            if bins:
                stations.append(CurrentStation(s["id"], s["lat"], s["lon"], bins))
        return cls(stations)

    def vector(self, lat, lon, t, depth_m=0.0):
        pts = [(s.lat, s.lon) for s in self.stations]
        vals = [s.vector(t, depth_m) for s in self.stations]
        return idw(pts, vals, lat, lon)

    def vector_many(self, lat, lon, t, depth_m):
        """Vectorized `vector` at one time for arrays of points and per-point depths. -> (N, 2).
        Each station's bins are evaluated once; each point then picks its nearest bin."""
        depth_m = np.asarray(depth_m, dtype=float)
        per_station = []
        for st in self.stations:
            bin_depths = np.array([b[0] for b in st.bins])
            vecs = np.array([st.vector_bin(b, t) for b in st.bins])  # (B, 2)
            pick = np.argmin(np.abs(bin_depths[None, :] - depth_m[:, None]), axis=1)  # first nearest, like min()
            per_station.append(vecs[pick])
        values = np.stack(per_station, axis=1)  # (N, S, 2)
        return idw_many([(s.lat, s.lon) for s in self.stations], values, lat, lon)


class Wind:
    """10 m wind from a grid of reanalysis points, interpolated in space and time."""

    def __init__(self, points):
        """points: list of (lat, lon, times, east, north) with east/north the TOWARD vector."""
        self.points = points

    @classmethod
    def load(cls, path):
        d = json.loads(open(path).read())
        pts = []
        for p in d["points"]:
            times = _parse_series_times(p["series"], "%Y-%m-%dT%H:%M")
            # meteorological FROM direction -> TOWARD vector
            east = [heading_vector(s, (frm + 180) % 360)[0] for _, s, frm in p["series"]]
            north = [heading_vector(s, (frm + 180) % 360)[1] for _, s, frm in p["series"]]
            pts.append((p["lat"], p["lon"], TimeSeries(times, east), TimeSeries(times, north)))
        return cls(pts)

    def vector(self, lat, lon, t):
        vals = [(e.at(t), n.at(t)) for _, _, e, n in self.points]
        return idw([(p[0], p[1]) for p in self.points], vals, lat, lon)

    def vector_many(self, lat, lon, t):
        """Vectorized `vector` at one time for arrays of points. -> (N, 2)."""
        vals = [(e.at(t), n.at(t)) for _, _, e, n in self.points]
        return idw_many([(p[0], p[1]) for p in self.points], vals, lat, lon)
