"""Download and cache a scenario's data layers.

Every layer lands in data/cache/<scenario>/ as a small JSON or NPZ file, so a scenario
is fetched once and then runs offline.

Sources (all free, no API key):
  terrain   NOAA NCEI DEM mosaic image service: merged topography + bathymetry, metres.
            Land > 0, water < 0. US coastal DEMs in the mosaic use NAVD88.
  tide      NOAA CO-OPS water level predictions at one station, metres in the same datum.
  currents  NOAA CO-OPS current predictions at every station in the area, every depth bin.
            With `[currents] source = "sfbofs"` also the NOAA SFBOFS model's hourly 3D currents on
            its own mesh (AWS Open Data), subset to the area; see sfbofs.py.
  wind      Open-Meteo: archived HRRR (3 km, US) or reanalysis, 10 m wind on a grid of points.
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


def fetch_terrain(sc: Scenario, log=print):
    out = sc.cache_dir / "terrain.npz"
    if out.exists():
        return out
    a = sc.area
    w, h = a.size_px
    log(f"terrain: {w}x{h} cells (~{a.resolution_m:g} m) from NCEI DEM mosaic")
    raw = _get(DEM, {
        "bbox": f"{a.west},{a.south},{a.east},{a.north}", "bboxSR": 4326, "imageSR": 4326,
        "size": f"{w},{h}", "format": "tiff", "pixelType": "F32",
        "interpolation": "RSP_BilinearInterpolation", "f": "image",
    })
    grid = tifffile.imread(io.BytesIO(raw)).astype(np.float32)
    if grid.shape != (h, w):
        raise RuntimeError(f"DEM service returned {grid.shape}, expected {(h, w)}")
    # row 0 is the north edge
    np.savez_compressed(out, elevation=grid, bbox=np.array([a.west, a.south, a.east, a.north]),
                        datum="NAVD88 (NCEI coastal DEMs; verify per area)")
    return out


def fetch_tide(sc: Scenario, log=print):
    """Water level. source "observed" (default for past windows) replays what was measured,
    which includes weather and river effects that astronomical predictions miss (18 cm
    at the Golden Gate in June 2026); gaps fall back to predictions."""
    out = sc.cache_dir / "tide.json"
    if out.exists():
        return out
    begin, end = _days(sc)
    st, datum = sc.tide["station"], sc.tide["datum"]
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
    log(f"tide: station {st} {source}, datum {datum}" + (f" ({filled} gaps filled from predictions)" if filled else ""))
    out.write_text(json.dumps({"station": st, "datum": datum, "source": source,
                               "series": [[t, v] for t, v in sorted(series.items())]}))
    return out


def _now():
    import datetime as dt

    return dt.datetime.now(dt.timezone.utc)


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
        try:
            data = _coops({"product": "currents_predictions", "station": s["id"], "bin": s["currbin"],
                           "interval": sc.currents.get("interval_min", 6), "begin_date": begin, "end_date": end})
        except Exception as e:  # one bad bin shouldn't sink the scenario
            log(f"  skip {s['id']} bin {s['currbin']}: {e}")
            continue
        cp = data["current_predictions"]["cp"]
        if cp and cp[0].get("meanFloodDir") is None:
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


def fetch_wind(sc: Scenario, log=print):
    out = sc.cache_dir / "wind.json"
    if out.exists():
        return out
    a = sc.area
    nx, ny = sc.wind.get("grid", [3, 3])
    lats = np.linspace(a.south, a.north, ny)
    lons = np.linspace(a.west, a.east, nx)
    pts = [(float(la), float(lo)) for la in lats for lo in lons]
    source = sc.wind.get("source", "open-meteo-archive")
    model = sc.wind.get("model")
    log(f"wind: {len(pts)} points from {source}{f' ({model})' if model else ''}")
    params = {
        "latitude": ",".join(f"{p[0]:.4f}" for p in pts),
        "longitude": ",".join(f"{p[1]:.4f}" for p in pts),
        "start_date": sc.start.strftime("%Y-%m-%d"), "end_date": sc.end.strftime("%Y-%m-%d"),
        "hourly": "wind_speed_10m,wind_direction_10m", "wind_speed_unit": "ms", "timezone": "GMT",
    }
    if model:
        params["models"] = model
    data = json.loads(_get(OPEN_METEO[source], params))
    data = data if isinstance(data, list) else [data]
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
    layers = [fetch_terrain, fetch_tide, fetch_currents]  # station predictions are also SFBOFS's fallback
    if sc.currents.get("source", "coops-predictions") == "sfbofs":
        layers.append(fetch_sfbofs)
    return [f(sc, log) for f in layers + [fetch_wind]]
