"""Download and cache a scenario's data layers.

Every layer lands in data/cache/<scenario>/ as a small JSON or NPZ file, so a scenario
is fetched once and then runs offline.

Sources (all free, no API key):
  terrain   NOAA NCEI DEM mosaic image service: merged topography + bathymetry, metres.
            Land > 0, water < 0. US coastal DEMs in the mosaic use NAVD88. With
            `[[terrain.nest]]`, finer grids inside the area too.
  tide      NOAA CO-OPS water level (observed, or predictions) at one station, metres in the same
            datum; with `[[tide.zones]]`, at every station the zones name.
  currents  NOAA CO-OPS current predictions at every station in the area, every depth bin.
            With `[currents] source = "sfbofs"` also the NOAA SFBOFS model's hourly 3D currents on
            its own mesh (AWS Open Data), subset to the area; see sfbofs.py. With `outside =
            "wcofs"`, NOAA WCOFS's around it; see wcofs.py.
  wind      Open-Meteo: archived HRRR (3 km, US) or reanalysis, 10 m wind on a grid of points
            (and `[[wind.nest]]` grids), in paced requests of 100 points.
"""

import io
import json
import time
import urllib.parse
import urllib.request

import numpy as np
import tifffile

from .scenario import Scenario

UA = {"User-Agent": "tidewater/0.1 (simulation research)"}
COOPS = "https://api.tidesandcurrents.noaa.gov/api/prod/datagetter"
COOPS_META = "https://api.tidesandcurrents.noaa.gov/mdapi/prod/webapi/stations.json"
DEM = "https://gis.ngdc.noaa.gov/arcgis/rest/services/DEM_mosaics/DEM_all/ImageServer/exportImage"
OPEN_METEO = {
    # reanalysis (ERA5 etc.), global but ~25 km cells: smooths out local wind like the Golden Gate gap
    "open-meteo-archive": "https://archive-api.open-meteo.com/v1/archive",
    # archived high-res forecasts; with model gfs_hrrr gives the 3 km US HRRR model
    "open-meteo-historical-forecast": "https://historical-forecast-api.open-meteo.com/v1/forecast",
}
FT = 0.3048


def _get(url, params=None, retries=3):
    if params:
        url = f"{url}?{urllib.parse.urlencode(params)}"
    for attempt in range(retries):
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=120) as r:
                return r.read()
        except Exception:
            if attempt == retries - 1:
                raise
            time.sleep(2 * (attempt + 1))


def _coops(params):
    base = {"time_zone": "gmt", "units": "metric", "format": "json", "application": "tidewater"}
    data = json.loads(_get(COOPS, {**base, **params}))
    if "error" in data:
        raise RuntimeError(f"CO-OPS: {data['error'].get('message')}")
    return data


def _days(sc: Scenario):
    """CO-OPS wants yyyymmdd; include the whole end day."""
    return sc.start.strftime("%Y%m%d"), sc.end.strftime("%Y%m%d")


DEM_TILE_PX = 2000   # the service allows 20,000 a side, but mosaics at most 100 source DEMs a
DEM_TILE_DEG = 0.5   # request: keep each request to a few of NCEI's 0.25-degree coastal tiles


def dem_grid(a, log=print):
    """The NCEI mosaic resampled to `a`'s grid (row 0 the north edge), in tiles whose pixels fall
    exactly on the whole grid's. An area of one tile is one request, as it always was."""
    w, h = a.size_px
    dx = (a.east - a.west) / w
    dy = (a.north - a.south) / h
    tw = max(1, min(DEM_TILE_PX, int(DEM_TILE_DEG / dx)))
    th = max(1, min(DEM_TILE_PX, int(DEM_TILE_DEG / dy)))
    grid = np.empty((h, w), dtype=np.float32)
    tiles = [(r0, c0) for r0 in range(0, h, th) for c0 in range(0, w, tw)]
    for i, (r0, c0) in enumerate(tiles, 1):
        r1, c1 = min(h, r0 + th), min(w, c0 + tw)
        west = a.west if c0 == 0 else a.west + c0 * dx
        east = a.east if c1 == w else a.west + c1 * dx
        north = a.north if r0 == 0 else a.north - r0 * dy
        south = a.south if r1 == h else a.north - r1 * dy
        raw = _get(DEM, {
            "bbox": f"{west},{south},{east},{north}", "bboxSR": 4326, "imageSR": 4326,
            "size": f"{c1 - c0},{r1 - r0}", "format": "tiff", "pixelType": "F32",
            "interpolation": "RSP_BilinearInterpolation", "f": "image",
        })
        tile = tifffile.imread(io.BytesIO(raw)).astype(np.float32)
        if tile.shape != (r1 - r0, c1 - c0):
            raise RuntimeError(f"DEM service returned {tile.shape}, expected {(r1 - r0, c1 - c0)}")
        grid[r0:r1, c0:c1] = tile
        if len(tiles) > 1 and (i % 10 == 0 or i == len(tiles)):
            log(f"  {i}/{len(tiles)} tiles")
    bad = ~np.isfinite(grid) | (grid < -11_500) | (grid > 9_000)
    if bad.any():
        raise RuntimeError(f"DEM service returned {int(bad.sum())} cells without data")
    return grid


def fetch_terrain(sc: Scenario, log=print):
    """terrain.npz: the scenario's area. With `[[terrain.nest]]`, also terrain-<name>.npz for each
    finer grid nested in it."""
    out = sc.cache_dir / "terrain.npz"
    grids = [(out, sc.area)] + [(sc.cache_dir / f"terrain-{name}.npz", a) for name, a in sc.terrain_nests]
    for path, a in grids:
        if path.exists():
            continue
        w, h = a.size_px
        log(f"terrain: {w}x{h} cells (~{a.resolution_m:g} m) from NCEI DEM mosaic"
            + (f" ({path.stem})" if path != out else ""))
        grid = dem_grid(a, log)
        # row 0 is the north edge
        np.savez_compressed(path, elevation=grid, bbox=np.array([a.west, a.south, a.east, a.north]),
                            datum="NAVD88 (NCEI coastal DEMs; verify per area)")
    return out


def _water_level(sc: Scenario, st):
    """One station's water level over the window -> ([[time, metres]], source, gaps filled)."""
    begin, end = _days(sc)
    datum = sc.tide["datum"]
    pred = _coops({"product": "predictions", "station": st, "datum": datum,
                   "interval": 6, "begin_date": begin, "end_date": end})
    series = {p["t"]: float(p["v"]) for p in pred["predictions"]}
    source = sc.tide.get("source", "observed" if sc.end < _now() else "predicted")
    filled = 0
    if source == "observed":
        obs = _coops({"product": "water_level", "station": st, "datum": datum, "begin_date": begin, "end_date": end})
        got = {r["t"]: float(r["v"]) for r in obs.get("data", []) if r.get("v") not in (None, "")}
        filled = sum(1 for t in series if t not in got)
        series.update(got)
    return [[t, v] for t, v in sorted(series.items())], source, filled


def fetch_tide(sc: Scenario, log=print):
    """Water level. source "observed" (default for past windows) replays what was measured,
    which includes weather and river effects that astronomical predictions miss (18 cm
    at the Golden Gate in June 2026); gaps fall back to predictions."""
    out = sc.cache_dir / "tide.json"
    if out.exists():
        return out
    st, datum = sc.tide["station"], sc.tide["datum"]
    series, source, filled = _water_level(sc, st)
    log(f"tide: station {st} {source}, datum {datum}" + (f" ({filled} gaps filled from predictions)" if filled else ""))
    out.write_text(json.dumps({"station": st, "datum": datum, "source": source, "series": series}))
    return out


def tide_stations(sc: Scenario):
    """The stations `[[tide.zones]]` name, the reference station first."""
    ids = [sc.tide["station"]]
    for z in sc.tide.get("zones", []):
        ids += [s for s in z["stations"] if s not in ids]
    return ids


def fetch_tides(sc: Scenario, log=print):
    """tides.json: every zone station's water level, as fetch_tide gets the reference station's,
    with the station's position."""
    out = sc.cache_dir / "tides.json"
    if out.exists():
        return out
    stations = []
    for st in tide_stations(sc):
        meta = json.loads(_get(f"{COOPS_META.rsplit('.json', 1)[0]}/{st}.json"))["stations"][0]
        series, source, filled = _water_level(sc, st)
        log(f"tide: station {st} {meta['name']} {source}" + (f" ({filled} gaps filled from predictions)" if filled else ""))
        stations.append({"id": st, "name": meta["name"], "lat": meta["lat"], "lon": meta["lng"], "source": source,
                         "series": series})
    out.write_text(json.dumps({"datum": sc.tide["datum"], "stations": stations}))
    return out


def _now():
    import datetime as dt

    return dt.datetime.now(dt.timezone.utc)


COOPS_REFUSED_S = 20  # wait before a bin's second try


def fetch_currents(sc: Scenario, log=print):
    out = sc.cache_dir / "currents.json"
    if out.exists():
        return out
    a = sc.area
    meta = json.loads(_get(COOPS_META, {"type": "currentpredictions"}))["stations"]
    inside = [s for s in meta if a.contains(s["lat"], s["lng"])]
    log(f"currents: {len(inside)} station/depth bins in area")
    begin, end = _days(sc)
    stations = {}
    for i, s in enumerate(inside, 1):
        cp = None
        for attempt in range(2):
            try:
                data = _coops({"product": "currents_predictions", "station": s["id"], "bin": s["currbin"],
                               "interval": sc.currents.get("interval_min", 6), "begin_date": begin, "end_date": end})
                cp = data["current_predictions"]["cp"]
                break
            except Exception as e:  # one bad bin shouldn't sink the scenario
                if attempt == 0 and "not available" not in str(e):
                    time.sleep(COOPS_REFUSED_S)  # a long run of requests is refused (HTTP 403) for a while
                    continue
                log(f"  skip {s['id']} bin {s['currbin']}: {e}")
        if not isinstance(cp, list) or not cp or not isinstance(cp[0], dict):
            if cp is not None:  # some bins are answered in words: "Currents are weak and variable"
                log(f"  skip {s['id']} bin {s['currbin']}: {str(cp)[:80]!r}")
            continue
        if cp[0].get("meanFloodDir") is None:
            log(f"  {s['id']} bin {s['currbin']}: no flood/ebb directions published; unusable as vectors")
        st = stations.setdefault(s["id"], {"id": s["id"], "name": s["name"], "lat": s["lat"], "lon": s["lng"], "bins": []})
        st["bins"].append({
            "bin": s["currbin"],
            "depth_m": round((s.get("depth") or 0) * FT, 1),  # metadata depth is in feet
            "flood_dir": cp[0]["meanFloodDir"],
            "ebb_dir": cp[0]["meanEbbDir"],
            # signed speed along the flood/ebb axis, m/s: + flood, - ebb
            "series": [[p["Time"], p["Velocity_Major"] / 100.0] for p in cp],
        })
        if i % 10 == 0:
            log(f"  {i}/{len(inside)}")
    for st in stations.values():
        st["bins"].sort(key=lambda b: b["depth_m"])
    out.write_text(json.dumps({"source": "NOAA CO-OPS current predictions", "stations": list(stations.values())}))
    return out


def fetch_sfbofs(sc: Scenario, log=print):
    out = sc.cache_dir / "sfbofs_currents.npz"
    if out.exists():
        return out
    from .sfbofs import fetch_subset

    return fetch_subset(sc.area, sc.start, sc.end, sc.cache_dir, margin_deg=sc.currents.get("margin_deg", 0.02),
                        log=log)


def fetch_wcofs(sc: Scenario, log=print):
    out = sc.cache_dir / "wcofs_currents.npz"
    if out.exists():
        return out
    from .wcofs import fetch_subset

    return fetch_subset(sc.area, sc.start, sc.end, sc.cache_dir, margin_deg=sc.currents.get("wcofs_margin_deg", 0.1),
                        log=log)


def wind_points(sc):
    """Where wind and weather are fetched: `[wind] grid` = [nx, ny] over the area, then each
    `[[wind.nest]]`'s grid over its own box (denser where the wind changes over a few km), in
    that order, without repeating a point."""
    a = sc.area
    boxes = [((a.west, a.south, a.east, a.north), sc.wind.get("grid", [3, 3]))]
    boxes += [((n["west"], n["south"], n["east"], n["north"]), n["grid"]) for n in sc.wind.get("nest", [])]
    pts, seen = [], set()
    for (w, s, e, n), (nx, ny) in boxes:
        for la in np.linspace(s, n, ny):
            for lo in np.linspace(w, e, nx):
                key = (round(float(la), 4), round(float(lo), 4))  # as requested
                if key not in seen:
                    seen.add(key)
                    pts.append((float(la), float(lo)))
    return pts


OPEN_METEO_CHUNK = 100     # locations per request
OPEN_METEO_PER_MIN = 500   # the free API allows 600 calls a minute, each location a call
OPEN_METEO_LIMITED_S = 65  # wait after HTTP 429 (over the minute's, hour's or day's calls)


def open_meteo(url, params, pts, log=print):
    """GET Open-Meteo for many points, in chunks paced under its per-minute limit.
    -> (one response per point, the raw bytes of each request)."""
    import urllib.error

    responses, raws = [], []
    chunks = [pts[i:i + OPEN_METEO_CHUNK] for i in range(0, len(pts), OPEN_METEO_CHUNK)]
    for i, chunk in enumerate(chunks):
        if i:
            time.sleep(60.0 * len(chunks[i - 1]) / OPEN_METEO_PER_MIN)
        q = {"latitude": ",".join(f"{p[0]:.4f}" for p in chunk),
             "longitude": ",".join(f"{p[1]:.4f}" for p in chunk), **params}
        for attempt in range(5):
            try:
                raw = _get(url, q)
                break
            except urllib.error.HTTPError as e:
                if e.code != 429 or attempt == 4:
                    raise
                log(f"  Open-Meteo: too many requests; waiting {OPEN_METEO_LIMITED_S} s")
                time.sleep(OPEN_METEO_LIMITED_S)
        data = json.loads(raw)
        data = data if isinstance(data, list) else [data]
        if len(data) != len(chunk):
            raise ValueError("Open-Meteo returned the wrong number of locations")
        responses += data
        raws.append(raw)
        if len(chunks) > 1:
            log(f"  {min((i + 1) * OPEN_METEO_CHUNK, len(pts))}/{len(pts)} points")
    return responses, raws


def fetch_wind(sc: Scenario, log=print):
    out = sc.cache_dir / "wind.json"
    if out.exists():
        return out
    pts = wind_points(sc)
    source = sc.wind.get("source", "open-meteo-archive")
    model = sc.wind.get("model")
    log(f"wind: {len(pts)} points from {source}{f' ({model})' if model else ''}")
    params = {
        "start_date": sc.start.strftime("%Y-%m-%d"), "end_date": sc.end.strftime("%Y-%m-%d"),
        "hourly": "wind_speed_10m,wind_direction_10m", "wind_speed_unit": "ms", "timezone": "GMT",
    }
    if model:
        params["models"] = model
    data, _ = open_meteo(OPEN_METEO[source], params, pts, log)
    points = []
    for (lat, lon), d in zip(pts, data):
        hr = d["hourly"]
        points.append({
            "lat": lat, "lon": lon, "model_lat": d["latitude"], "model_lon": d["longitude"],
            # direction the wind blows FROM, degrees true (meteorological convention)
            "series": [[t, s, w] for t, s, w in zip(hr["time"], hr["wind_speed_10m"], hr["wind_direction_10m"])
                       if s is not None and w is not None],
        })
    out.write_text(json.dumps({"source": source, "model": model, "points": points}))
    return out


def fetch_all(sc: Scenario, log=print):
    layers = [fetch_terrain, fetch_tide, fetch_currents]  # station predictions are also the models' fallback
    if sc.tide.get("zones"):
        layers.append(fetch_tides)
    if sc.currents.get("source", "coops-predictions") == "sfbofs":
        layers.append(fetch_sfbofs)
    if sc.currents.get("outside") == "wcofs":
        layers.append(fetch_wcofs)
    if sc.weather.get("enabled", False) and not sc.weather.get("archive"):
        from .weather import fetch_weather
        layers.append(fetch_weather)
    return [f(sc, log) for f in layers + [fetch_wind]]
