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


class CurrentStation:
    def __init__(self, sid, lat, lon, bins):
        """bins: list of (depth_m, flood_dir, ebb_dir, TimeSeries of signed speed m/s)."""
        self.id = sid
        self.lat = lat
        self.lon = lon
        self.bins = sorted(bins, key=lambda b: b[0])

    def vector(self, t, depth_m=0.0):
        depth, flood, ebb, ts = min(self.bins, key=lambda b: abs(b[0] - depth_m))
        v = ts.at(t)
        return heading_vector(abs(v), flood if v >= 0 else ebb)


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
