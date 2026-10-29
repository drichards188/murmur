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
    from .fetch import OPEN_METEO, _get
    out = sc.cache_dir / 'weather.json'
    if out.exists():
        Weather.load(out)  # don't silently reuse a corrupt or unsupported artifact
        return out
    a = sc.area
    nx, ny = sc.wind.get('grid', [3, 3])
    pts = [(float(la), float(lo)) for la in np.linspace(a.south, a.north, ny)
           for lo in np.linspace(a.west, a.east, nx)]
    source = sc.wind.get('source', 'open-meteo-historical-forecast')
    model = sc.wind.get('model', 'gfs_hrrr')
    params = {
        'latitude': ','.join(f'{p[0]:.4f}' for p in pts),
        'longitude': ','.join(f'{p[1]:.4f}' for p in pts),
        'start_date': sc.start.strftime('%Y-%m-%d'),
        'end_date': sc.end.strftime('%Y-%m-%d'),
        'hourly': ','.join([*FIELDS, 'wind_speed_10m', 'wind_direction_10m']),
        'wind_speed_unit': 'ms', 'timezone': 'GMT', 'models': model,
    }
    log(f'weather: {len(pts)} points, {model} via Open-Meteo (free noncommercial API; attribution required)')
    raw = _get(OPEN_METEO[source], params)
    responses = json.loads(raw)
    responses = responses if isinstance(responses, list) else [responses]
    if len(responses) != len(pts):
        raise ValueError('weather API returned the wrong number of locations')
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

    def vector_many(self, lat, lon, t):
        return np.array([self.vector(la, lo, t) for la, lo in zip(lat, lon)], dtype=float).reshape((-1, 2))


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
