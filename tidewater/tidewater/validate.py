"""Compare a scenario's forcing to what was actually measured.

Predictions (tide, current) and model output (wind) are what tidewater feeds simulators;
this checks them against NOAA CO-OPS observations for the same window and writes a
report. It validates the forcing, not any vehicle's response to it.
"""

import datetime as dt
import json
import math

import numpy as np

from .conditions import Conditions
from .fetch import COOPS_META, _coops, _days, _get
from .layers import to_epoch
from .scenario import Scenario


def _obs(sc: Scenario, product, station, extra=None):
    out = sc.cache_dir / f"observed_{product}_{station}.json"
    if not out.exists():
        begin, end = _days(sc)
        data = _coops({"product": product, "station": station, "begin_date": begin, "end_date": end, **(extra or {})})
        out.write_text(json.dumps(data))
    return json.loads(out.read_text())


def _t(s):
    return dt.datetime.strptime(s, "%Y-%m-%d %H:%M").replace(tzinfo=dt.timezone.utc)


def _in_window(sc, t):
    return sc.start <= t < sc.end


def _circ_diff(a, b):
    return (a - b + 180) % 360 - 180


def validate_tide(sc: Scenario, cond: Conditions):
    """Astronomical predictions vs measured water level at the reference station. (The
    scenario itself replays the observations for past windows; this shows why.)"""
    st = sc.tide["station"]
    data = _obs(sc, "water_level", st, {"datum": sc.tide["datum"]})
    preds = _obs(sc, "predictions", st, {"datum": sc.tide["datum"], "interval": 6})
    rows = [(_t(r["t"]), float(r["v"])) for r in data["data"] if r.get("v") not in (None, "")]
    rows = [(t, v) for t, v in rows if _in_window(sc, t)]
    obs = np.array([v for _, v in rows])
    pts = [(_t(p["t"]), float(p["v"])) for p in preds["predictions"]]
    pt = np.array([to_epoch(t) for t, _ in pts])
    pv = np.array([v for _, v in pts])

    def pred_at(x):
        return float(np.interp(x, pt, pv))

    pred = np.array([pred_at(to_epoch(t)) for t, _ in rows])
    err = pred - obs
    # timing: shift predictions in 6-min steps and find the best match
    times = np.array([to_epoch(t) for t, _ in rows])
    lags = range(-60, 61, 6)
    rms_by_lag = {lag: float(np.sqrt(np.mean((np.array([pred_at(x + lag * 60) for x in times]) - obs) ** 2)))
                  for lag in lags}
    best = min(rms_by_lag, key=rms_by_lag.get)
    return {
        "station": st, "name": data["metadata"]["name"], "n": len(rows),
        "rmse_m": float(np.sqrt(np.mean(err ** 2))), "bias_m": float(err.mean()), "max_abs_m": float(np.abs(err).max()),
        "obs_range_m": float(obs.max() - obs.min()),
        "best_lag_min": best, "rmse_at_best_lag_m": rms_by_lag[best],
    }


def validate_wind(sc: Scenario, cond: Conditions, station=None):
    st = station or sc.tide["station"]
    data = _obs(sc, "wind", st)
    lat, lon = float(data["metadata"]["lat"]), float(data["metadata"]["lon"])
    rows = []
    for r in data["data"]:
        if r.get("s") in (None, "") or r.get("d") in (None, ""):
            continue
        t = _t(r["t"])
        if _in_window(sc, t):
            rows.append((t, float(r["s"]), float(r["d"])))
    # hourly means of the observations, to compare like with like (model is hourly)
    by_hour = {}
    for t, s, d in rows:
        h = t.replace(minute=0)
        a = math.radians(d)
        acc = by_hour.setdefault(h, [0.0, 0.0, 0.0, 0])
        acc[0] += s
        acc[1] += s * math.sin(a)
        acc[2] += s * math.cos(a)
        acc[3] += 1
    spd_err, dir_err, obs_spd, mod_spd = [], [], [], []
    for h, (ssum, e, n, k) in sorted(by_hour.items()):
        o_s = ssum / k
        o_from = math.degrees(math.atan2(e, n)) % 360
        sample = cond.at(lat, lon, h)
        m_s, m_toward = sample.wind
        m_from = (m_toward + 180) % 360
        obs_spd.append(o_s)
        mod_spd.append(m_s)
        spd_err.append(m_s - o_s)
        if o_s > 2.0:  # direction is meaningless in near-calm
            dir_err.append(_circ_diff(m_from, o_from))
    spd_err = np.array(spd_err)
    return {
        "station": st, "name": data["metadata"]["name"], "hours": len(by_hour),
        "obs_mean_ms": float(np.mean(obs_spd)), "model_mean_ms": float(np.mean(mod_spd)),
        "obs_max_hourly_ms": float(np.max(obs_spd)), "model_max_ms": float(np.max(mod_spd)),
        "speed_rmse_ms": float(np.sqrt(np.mean(spd_err ** 2))), "speed_bias_ms": float(spd_err.mean()),
        "dir_mae_deg": float(np.mean(np.abs(dir_err))) if dir_err else None,
        "dir_bias_deg": float(np.mean(dir_err)) if dir_err else None,
    }


def current_observation_coverage(sc: Scenario):
    """Active NOAA current meters inside the area (PORTS etc.). Often none."""
    meta = json.loads(_get(COOPS_META, {"type": "currents"}))["stations"]
    a = sc.area
    return [s["id"] for s in meta if a.contains(float(s["lat"]), float(s["lng"]))]


def report(sc: Scenario):
    cond = Conditions.for_scenario(sc)
    tide = validate_tide(sc, cond)
    wind = validate_wind(sc, cond)
    meters = current_observation_coverage(sc)
    lines = [
        f"# Validation: {sc.title}",
        "",
        f"Window {sc.start:%Y-%m-%d %H:%M}Z to {sc.end:%Y-%m-%d %H:%M}Z. Generated by `tidewater validate`.",
        "",
        f"## Tide: predictions vs observed water level ({tide['name']}, {tide['station']})",
        "",
        "The scenario replays observed water level for past windows; this table shows what using",
        "astronomical predictions alone would have cost.",
        "",
        "| metric | value |", "|---|---|",
        f"| samples (6 min) | {tide['n']} |",
        f"| observed range | {tide['obs_range_m']:.2f} m |",
        f"| RMSE | {tide['rmse_m']:.3f} m |",
        f"| bias (pred - obs) | {tide['bias_m']:+.3f} m |",
        f"| max abs error | {tide['max_abs_m']:.3f} m |",
        f"| best timing shift | {tide['best_lag_min']:+d} min (RMSE {tide['rmse_at_best_lag_m']:.3f} m) |",
        "",
        f"## Wind vs observed ({wind['name']}, {wind['station']}), hourly means",
        "",
        "| metric | value |", "|---|---|",
        f"| hours | {wind['hours']} |",
        f"| mean speed obs / model | {wind['obs_mean_ms']:.1f} / {wind['model_mean_ms']:.1f} m/s |",
        f"| max hourly speed obs / model | {wind['obs_max_hourly_ms']:.1f} / {wind['model_max_ms']:.1f} m/s |",
        f"| speed RMSE | {wind['speed_rmse_ms']:.2f} m/s |",
        f"| speed bias (model - obs) | {wind['speed_bias_ms']:+.2f} m/s |",
        f"| direction mean abs error (obs > 2 m/s) | {wind['dir_mae_deg']:.0f} deg |",
        f"| direction bias | {wind['dir_bias_deg']:+.0f} deg |",
        "",
        "Caveat: station anemometers are not at the model's 10 m height or exposure.",
        "",
        "## Currents",
        "",
        (f"Active NOAA current meters in the area: {', '.join(meters)}." if meters else
         "No active NOAA current meter (PORTS) inside the area for this window, so currents are "
         "**not validated against observations** here. They are NOAA harmonic predictions, "
         "interpolated between stations."),
        "",
    ]
    return "\n".join(lines), {"tide": tide, "wind": wind, "current_meters": meters}
