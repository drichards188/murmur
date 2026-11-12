"""Archived surface weather, shared JSON schema with murmur-weather (offline, no forecast).

Nearest requested grid point, linear instantaneous fields, precipitation total for the
preceding hour. Missing fields stay None; no extrapolation or filling missing hours.
"""
import bisect
import datetime as dt
import hashlib
import json
import math

import numpy as np

FIELDS = {
    'visibility': ('visibility_m', 'm'),
    'wind_gusts_10m': ('gust_mps', 'm/s'),
    'precipitation': ('precipitation_mm', 'mm'),
    'cloud_cover': ('cloud_cover_pct', '%'),
    'temperature_2m': ('temperature_c', '°C'),
    'relative_humidity_2m': ('humidity_pct', '%'),
    'surface_pressure': ('pressure_hpa', 'hPa'),
}


def fetch_weather(sc, log=print):
    from .fetch import OPEN_METEO, open_meteo, wind_points
    out = sc.cache_dir / 'weather.json'
    if out.exists():
        Weather.load(out)  # don't silently reuse a corrupt or unsupported artifact
        return out
    a = sc.area
    pts = wind_points(sc)
    source = sc.wind.get('source', 'open-meteo-historical-forecast')
    model = sc.wind.get('model', 'gfs_hrrr')
    params = {
        'start_date': sc.start.strftime('%Y-%m-%d'),
        'end_date': sc.end.strftime('%Y-%m-%d'),
        'hourly': ','.join([*FIELDS, 'wind_speed_10m', 'wind_direction_10m']),
        'wind_speed_unit': 'ms', 'timezone': 'GMT', 'models': model,
    }
    log(f'weather: {len(pts)} points, {model} via Open-Meteo (free noncommercial API; attribution required)')
    responses, raws = open_meteo(OPEN_METEO[source], params, pts, log)
    # one request as it always was; several kept as a JSON array of the responses, in order
    raw = raws[0] if len(raws) == 1 else b'[' + b','.join(raws) + b']'
    params = {'latitude': ','.join(f'{p[0]:.4f}' for p in pts),
              'longitude': ','.join(f'{p[1]:.4f}' for p in pts), **params}
    if len(raws) > 1:
        params['requests'] = len(raws)
    points = []
    for (lat, lon), data in zip(pts, responses):
        hourly, units = data['hourly'], data['hourly_units']
        for name, unit in {**{k: v[1] for k, v in FIELDS.items()},
                           'wind_speed_10m': 'm/s', 'wind_direction_10m': '°'}.items():
            if name in hourly and units.get(name) != unit:
                raise ValueError(f'weather unit for {name}: {units.get(name)!r}, expected {unit!r}')
        rows = []
        for i, stamp in enumerate(hourly['time']):
            def value(name):
                values = hourly.get(name)
                v = values[i] if values is not None else None
                return v if v is not None and math.isfinite(v) else None
            v = {key: value(name) for name, (key, _) in FIELDS.items()}
            speed, direction = value('wind_speed_10m'), value('wind_direction_10m')
            v['wind_east_mps'] = -speed * math.sin(math.radians(direction)) if speed is not None and direction is not None else None
            v['wind_north_mps'] = -speed * math.cos(math.radians(direction)) if speed is not None and direction is not None else None
            rows.append({'t': dt.datetime.fromisoformat(stamp).replace(tzinfo=dt.timezone.utc).timestamp(), 'values': v})
        points.append({'lat': lat, 'lon': lon, 'rows': rows})
    archive = {'schema': 1, 'source': OPEN_METEO[source], 'model': model,
               'fetched_at': dt.datetime.now(dt.timezone.utc).isoformat(),
               'raw_sha256': hashlib.sha256(raw).hexdigest(),
               'bounds': [a.west, a.south, a.east, a.north], 'points': points,
               'request': params, 'attribution': 'Weather data by Open-Meteo.com; NOAA HRRR',
               'sampling': 'nearest requested grid point; linear instantaneous fields; precipitation total and gust maximum of preceding hour'}
    Weather(json.loads(json.dumps(archive)))
    (sc.cache_dir / 'weather-source.json').write_bytes(raw)
    tmp = out.with_suffix('.json.tmp')
    tmp.write_text(json.dumps(archive, allow_nan=False))
    tmp.replace(out)
    return out


class Weather:
    def __init__(self, data):
        if data['schema'] != 1 or not data['points']:
            raise ValueError('unsupported weather schema or no points')
        w, s, e, n = data['bounds']
        if not (-180 <= w < e <= 180 and -90 <= s < n <= 90):
            raise ValueError('invalid weather bounds')
        self.data = data
        self.points = data['points']
        for p in self.points:
            if not (-90 <= p['lat'] <= 90 and -180 <= p['lon'] <= 180):
                raise ValueError('invalid weather point')
            ts = [r['t'] for r in p['rows']]
            if not ts or any(not math.isfinite(t) for t in ts) or any(b <= a for a, b in zip(ts, ts[1:])):
                raise ValueError('weather timestamps must increase')
            for r in p['rows']:
                for k, v in r['values'].items():
                    if v is not None and (not math.isfinite(v) or
                            (k in ('visibility_m', 'gust_mps', 'precipitation_mm', 'pressure_hpa') and v < 0) or
                            (k in ('cloud_cover_pct', 'humidity_pct') and not 0 <= v <= 100)):
                        raise ValueError(f'invalid weather {k}')
            p['_times'] = ts

    @classmethod
    def load(cls, path):
        return cls(json.loads(path.read_text()))

    def at(self, lat, lon, t):
        t = t.timestamp() if isinstance(t, dt.datetime) else t
        w, s, e, n = self.data['bounds']
        if not (s <= lat <= n and w <= lon <= e and math.isfinite(t)):
            return {}
        p = min(self.points, key=lambda p: (p['lat']-lat)**2 + ((p['lon']-lon)*math.cos(math.radians(lat)))**2)
        return self._values(p, t)

    def nearest_many(self, lat, lon):
        """Index of each point's nearest archived point, as `at` picks it (the first of equals);
        -1 outside the bounds."""
        lat = np.asarray(lat, dtype=float)
        lon = np.asarray(lon, dtype=float)
        if not hasattr(self, '_tree'):
            from scipy.spatial import cKDTree
            self._plat = np.array([p['lat'] for p in self.points])
            self._plon = np.array([p['lon'] for p in self.points])
            self._k0 = math.cos(math.radians(float(self._plat.mean())))
            self._tree = cKDTree(np.column_stack([self._plat, self._plon * self._k0]))
        w, s, e, n = self.data['bounds']
        out = np.full(lat.shape, -1, dtype=np.intp)
        inside = np.flatnonzero((s <= lat) & (lat <= n) & (w <= lon) & (lon <= e))
        if len(inside) == 0:
            return out
        la, lo = lat[inside], lon[inside]
        cos = np.cos(np.radians(la))
        # candidates from a tree in a fixed longitude scale, chosen by `at`'s own metric
        k = min(8, len(self.points))
        tdist, cand = self._tree.query(np.column_stack([la, lo * self._k0]), k=k)
        tdist, cand = tdist.reshape(len(la), k), np.sort(cand.reshape(len(la), k), axis=1)  # first of equals
        d = (self._plat[cand] - la[:, None]) ** 2 + ((self._plon[cand] - lo[:, None]) * cos[:, None]) ** 2
        best = np.argmin(d, axis=1)
        out[inside] = cand[np.arange(len(la)), best]
        # a point outside the candidates is at least the k-th's tree distance away, which `at`'s
        # metric shrinks by at most cos(lat) / k0; where that can't rule it out, compare all
        sure = np.sqrt(d[np.arange(len(la)), best]) < tdist[:, -1] * np.minimum(1.0, cos / self._k0)
        if k == len(self.points):
            sure[:] = True
        for c in range(0, int((~sure).sum()), 4096):
            i = inside[~sure][c:c + 4096]
            kc = np.cos(np.radians(lat[i]))[:, None]
            dd = (self._plat[None, :] - lat[i, None]) ** 2 + ((self._plon[None, :] - lon[i, None]) * kc) ** 2
            out[i] = np.argmin(dd, axis=1)
        return out

    def at_many(self, lat, lon, t):
        """`at` for arrays of points at one time: each archived point's values are computed once."""
        t = t.timestamp() if isinstance(t, dt.datetime) else t
        if not math.isfinite(t):
            return [{} for _ in range(len(lat))]
        near = self.nearest_many(lat, lon)
        values = {int(i): self._values(self.points[i], t) for i in np.unique(near) if i >= 0}
        return [values[i].copy() if i >= 0 else {} for i in near.tolist()]

    def _values(self, p, t):
        """One archived point's values at t: linear between its hours, {} where it has none."""
        i = bisect.bisect_right(p['_times'], t)
        if i == 0:
            return {}
        a = p['rows'][i-1]
        if a['t'] == t:
            return a['values'].copy()
        if i == len(p['rows']):
            return {}
        b = p['rows'][i]
        if b['t'] - a['t'] > 3600.01:
            return {}
        f = (t-a['t'])/(b['t']-a['t'])
        return {k: b['values'].get(k) if k in ('precipitation_mm', 'gust_mps') else
                (v + (b['values'][k]-v)*f if v is not None and b['values'].get(k) is not None else None)
                for k, v in a['values'].items()}

    def vector(self, lat, lon, t):
        v = self.at(lat, lon, t)
        e, n = v.get('wind_east_mps'), v.get('wind_north_mps')
        return (e, n) if e is not None and n is not None else (None, None)

    def vector_many(self, lat, lon, t, values=None):
        """Wind (east, north) for arrays of points, NaN where there is none. `values`: at_many's
        answer for the same points and time, when the caller has it."""
        values = self.at_many(lat, lon, t) if values is None else values
        return np.array([(v.get('wind_east_mps'), v.get('wind_north_mps'))
                         if v.get('wind_east_mps') is not None and v.get('wind_north_mps') is not None
                         else (None, None) for v in values], dtype=float).reshape((-1, 2))


def snapshot(source, output, start, end):
    """Pin the rows bracketing a replay window; source metadata and raw checksum survive."""
    from pathlib import Path
    data = json.loads(Path(source).read_text())
    original_sha = hashlib.sha256(Path(source).read_bytes()).hexdigest()
    Weather(json.loads(json.dumps(data)))
    if not math.isfinite(start) or not math.isfinite(end) or end <= start:
        raise ValueError('snapshot needs an increasing time range')
    for p in data['points']:
        p.pop('_times', None)
        times = [r['t'] for r in p['rows']]
        if start < times[0] or end > times[-1]:
            raise ValueError('snapshot window outside archive')
        lo = max(0, bisect.bisect_right(times, start)-1)
        hi = bisect.bisect_left(times, end)+1
        p['rows'] = p['rows'][lo:hi]
    data['parent_sha256'] = original_sha
    data['replay_window'] = [start, end]
    Path(output).parent.mkdir(parents=True, exist_ok=True)
    Path(output).write_text(json.dumps(data, indent=2, allow_nan=False)+'\n')
