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


def _meter_bins(station):
    """Bin number -> depth below the surface (m) for a current meter."""
    data = json.loads(_get(f"https://api.tidesandcurrents.noaa.gov/mdapi/prod/webapi/stations/{station}/bins.json"))
    return {b["num"]: b["depth"] for b in data["bins"] if b.get("depth") is not None}


def _observed_currents(sc: Scenario, station):
    """Observed current rows (time, speed m/s, direction toward deg) and the bin they come from."""
    out = sc.cache_dir / f"observed_currents_{station}.json"
    if not out.exists():
        begin, end = _days(sc)
        data = _coops({"product": "currents", "station": station, "begin_date": begin, "end_date": end})
        data["bin_depths"] = _meter_bins(station)
        out.write_text(json.dumps(data))
    data = json.loads(out.read_text())
    rows = [(_t(r["t"]), float(r["s"]) / 100.0, float(r["d"]), int(r["b"])) for r in data.get("data", [])
            if r.get("s") not in (None, "") and r.get("d") not in (None, "")]
    rows = [r for r in rows if _in_window(sc, r[0])]
    return data["metadata"], rows, {int(k): v for k, v in data["bin_depths"].items()}


def score_currents(t, ref_e, ref_n, eval_layer, max_lag_min=180, step_min=6):
    """Error statistics of a current layer against reference east/north components.

    eval_layer(shift_s) -> (n, 2) layer values at t + shift_s. Speed RMSE and bias compare
    magnitudes; direction error uses samples where the reference speed exceeds 0.3 m/s
    (direction is noise near slack). Phase lag projects both onto the reference's principal
    (along-channel) axis and finds the time shift of the layer that fits best: positive means
    the layer's tide turns later than the reference."""
    model = eval_layer(0.0)
    ref_s = np.hypot(ref_e, ref_n)
    mod_s = np.hypot(model[:, 0], model[:, 1])
    ref_dir = np.degrees(np.arctan2(ref_e, ref_n)) % 360
    mod_dir = np.degrees(np.arctan2(model[:, 0], model[:, 1])) % 360
    strong = ref_s > 0.3
    dd = _circ_diff(mod_dir[strong], ref_dir[strong])
    vec = np.hypot(model[:, 0] - ref_e, model[:, 1] - ref_n)
    w, vv = np.linalg.eigh(np.cov(np.vstack([ref_e, ref_n])))
    ax = vv[:, np.argmax(w)]
    along_ref = ref_e * ax[0] + ref_n * ax[1]
    best = None
    for lag in range(-max_lag_min, max_lag_min + 1, step_min):
        v = eval_layer(lag * 60.0)
        rms = float(np.sqrt(np.mean((v[:, 0] * ax[0] + v[:, 1] * ax[1] - along_ref) ** 2)))
        if best is None or rms < best[1]:
            best = (lag, rms)
    return {
        "n": len(t), "ref_mean_speed": float(ref_s.mean()), "ref_max_speed": float(ref_s.max()),
        "speed_rmse": float(np.sqrt(np.mean((mod_s - ref_s) ** 2))), "speed_bias": float((mod_s - ref_s).mean()),
        "vector_rmse": float(np.sqrt(np.mean(vec ** 2))),
        "dir_mae": float(np.mean(np.abs(dd))) if strong.any() else None,
        "dir_bias": float(dd.mean()) if strong.any() else None,
        "axis_deg": float(np.degrees(np.arctan2(ax[0], ax[1])) % 180),
        "lag_min": best[0], "along_rmse_at_best_lag": best[1],
    }


def _sfbofs_layer(sc: Scenario):
    path = sc.cache_dir / "sfbofs_currents.npz"
    if not path.exists():
        return None
    from .sfbofs import SfbofsCurrents

    return SfbofsCurrents.load(path)  # no fallback: score the model alone


def validate_currents(sc: Scenario, meters=None):
    """Compare current layers with NOAA current meter observations, per meter.

    - "predicted": the harmonic prediction published for the meter's own station and bin.
    - "idw": station inverse-distance interpolation with that meter's own station left out, i.e.
      what the default layer gives at a place with no station of its own (interpolation error).
    - "sfbofs": the SFBOFS layer (hourly model), evaluated at the meter position.
    Every layer is sampled where the meter reads, at the meter's real-time bin depth."""
    from .layers import Currents

    stations = Currents.load(sc.cache_dir / "currents.json")
    sfb = _sfbofs_layer(sc)
    results = []
    for sid in meters or current_observation_coverage(sc):
        own = [s for s in stations.stations if s.id == sid]
        if not own:
            continue
        try:
            meta, rows, bin_depths = _observed_currents(sc, sid)
        except Exception:
            continue
        if len(rows) < 50:
            continue
        lat, lon = float(meta["lat"]), float(meta["lon"])
        bins = [r[3] for r in rows]
        bin_no = max(set(bins), key=bins.count)
        depth = bin_depths.get(bin_no, 0.0)
        t = np.array([to_epoch(r[0]) for r in rows])
        obs_e = np.array([r[1] * math.sin(math.radians(r[2])) for r in rows])
        obs_n = np.array([r[1] * math.cos(math.radians(r[2])) for r in rows])
        loo = Currents([s for s in stations.stations if s.id != sid])
        evals = {
            "predicted": lambda sh: np.array([own[0].vector(x + sh, depth) for x in t]),
            "idw": lambda sh: np.array([loo.vector(lat, lon, x + sh, depth) for x in t]),
        }
        if sfb is not None:
            evals["sfbofs"] = lambda sh: np.array([sfb.vector(lat, lon, x + sh, depth) for x in t])
        results.append({"id": sid, "name": meta["name"], "lat": lat, "lon": lon, "bin": bin_no, "depth_m": depth,
                        "layers": {k: score_currents(t, obs_e, obs_n, f) for k, f in evals.items()}})
    return results


def validate_vs_predictions(sc: Scenario, step_min=30):
    """SFBOFS against NOAA harmonic predictions at the scenario's prediction stations (every bin
    with a published flood/ebb axis, a 6-minute series and peak speed over 0.3 m/s). Predictions
    are not observations: this shows how far the model and the default layer differ."""
    from .layers import Currents

    sfb = _sfbofs_layer(sc)
    if sfb is None:
        return []
    t = np.arange(sc.start.timestamp(), sc.end.timestamp(), step_min * 60.0)
    out = []
    for st in Currents.load(sc.cache_dir / "currents.json").stations:
        for b in st.bins:
            if float(np.median(np.diff(b[3].t))) > 30 * 60:
                continue  # subordinate station: sparse events, not a series
            ref = np.array([st.vector_bin(b, x) for x in t])
            if np.hypot(ref[:, 0], ref[:, 1]).max() < 0.3:
                continue
            m = score_currents(t, ref[:, 0], ref[:, 1],
                               lambda sh, st=st, b=b: np.array([sfb.vector(st.lat, st.lon, x + sh, b[0]) for x in t]),
                               max_lag_min=120, step_min=10)
            out.append({"id": st.id, "lat": st.lat, "lon": st.lon, "depth_m": b[0], **m})
    return out


def _fmt_layer_row(label, m):
    d = f"{m['dir_mae']:.0f} / {m['dir_bias']:+.0f} deg" if m["dir_mae"] is not None else "n/a"
    return (f"| {label} | {m['speed_rmse']:.2f} m/s | {m['speed_bias']:+.2f} m/s | {m['vector_rmse']:.2f} m/s | "
            f"{d} | {m['lag_min']:+d} min |")


def current_section(results, preds=()):
    """Markdown lines for the current-layer comparisons (no heading)."""
    names = {"predicted": "NOAA prediction at the meter", "idw": "station IDW, meter's own station left out",
             "sfbofs": "SFBOFS model"}
    head = ["| layer | speed RMSE | speed bias | vector RMSE | direction error (MAE / bias) | phase lag |",
            "|---|---|---|---|---|---|"]
    lines = []
    for r in results:
        any_m = next(iter(r["layers"].values()))
        lines += [
            f"### Meter {r['name']} ({r['id']}), bin {r['bin']} at {r['depth_m']:.1f} m",
            "",
            f"Observed {any_m['n']} samples at {r['lat']:.4f}, {r['lon']:.4f}: mean speed "
            f"{any_m['ref_mean_speed']:.2f} m/s, max {any_m['ref_max_speed']:.2f} m/s, principal axis "
            f"{any_m['axis_deg']:.0f} deg.", "", *head,
            *[_fmt_layer_row(names[k], m) for k, m in r["layers"].items()], "",
        ]
    if len(results) > 1:
        lines += ["### Summary over the meters", "", "| layer | speed RMSE (rms over meters) | vector RMSE | mean abs phase lag |",
                  "|---|---|---|---|"]
        for k in names:
            ms = [r["layers"][k] for r in results if k in r["layers"]]
            if ms:
                lines.append(f"| {names[k]} | {np.sqrt(np.mean([m['speed_rmse'] ** 2 for m in ms])):.2f} m/s | "
                             f"{np.sqrt(np.mean([m['vector_rmse'] ** 2 for m in ms])):.2f} m/s | "
                             f"{np.mean([abs(m['lag_min']) for m in ms]):.0f} min |")
        lines.append("")
    if preds:
        lines += [
            "### SFBOFS against NOAA predictions at prediction stations", "",
            "| station | depth | peak predicted | speed RMSE | speed bias | direction error (MAE / bias) | phase lag |",
            "|---|---|---|---|---|---|---|",
        ]
        for m in preds:
            d = f"{m['dir_mae']:.0f} / {m['dir_bias']:+.0f} deg" if m["dir_mae"] is not None else "n/a"
            lines.append(f"| {m['id']} | {m['depth_m']:.0f} m | {m['ref_max_speed']:.2f} m/s | "
                         f"{m['speed_rmse']:.2f} m/s | {m['speed_bias']:+.2f} m/s | {d} | {m['lag_min']:+d} min |")
        rm = np.sqrt(np.mean([m["speed_rmse"] ** 2 for m in preds]))
        lines += ["", f"Across {len(preds)} station bins: speed RMSE {rm:.2f} m/s (rms over bins), "
                      f"median phase lag {int(np.median([m['lag_min'] for m in preds])):+d} min.", ""]
    return lines


CURRENT_INTRO = [
    "No active NOAA current meter (PORTS) sits inside the scenario area, so the current layers are",
    "scored at the three active meters in central San Francisco Bay (scenario `sf-bay-currents-2026-06`,",
    "same window): Oakland Outer Harbor, Oakland Inner Harbor and Southampton Shoal. Each layer is",
    "sampled at the meter's position and real-time bin depth and compared with the 6-minute observations.",
    "",
    "- **NOAA prediction at the meter**: the harmonic prediction for that meter's own station and bin.",
    "  Not available at a place without a station; it is the reference for how good a prediction can be.",
    "- **station IDW, meter's own station left out**: the default layer where no station exists,",
    "  which is the interpolation error the model is meant to remove.",
    "- **SFBOFS model**: the nowcast layer (hourly, mesh interpolation), without the station fallback.",
    "",
    "Speed RMSE and bias compare speed magnitudes. Direction error is the circular difference of",
    "direction toward, over samples where the observed speed exceeds 0.3 m/s (direction is noise near",
    "slack). Phase lag is the time shift of the layer that best fits the observed along-channel",
    "(principal axis) velocity, searched over +-3 h in 6-minute steps; positive means the layer's tide",
    "turns later than observed.",
    "",
]

CURRENT_CAVEATS = [
    "Caveats: three meters in three days is a small sample, and all are in harbors and channels of the",
    "central and north bay, not at the Golden Gate, so Gate-scale accuracy is inferred, not measured.",
    "The Oakland Inner Harbor channel is narrower than the SFBOFS cells there. Observations come from",
    "ADCP bins with their own noise (a few cm/s), and the SFBOFS nowcast is one hourly record per hour,",
    "so the comparison cannot separate model error from sub-hour structure. The prediction-station",
    "table compares two models of the tide, not a model with the truth.",
    "",
]


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
    ]
    current_results, preds = [], []
    if meters:
        current_results = validate_currents(sc, meters)
    elif sc.validation.get("current_meters_scenario"):
        other = Scenario.load(sc.path.parent / sc.validation["current_meters_scenario"])
        current_results = validate_currents(other)
    preds = validate_vs_predictions(sc)
    lines += ["## Currents", ""]
    if current_results:
        lines += CURRENT_INTRO + current_section(current_results, preds) + CURRENT_CAVEATS
    else:
        lines += [
            "No active NOAA current meter (PORTS) inside the area for this window, so currents are "
            "**not validated against observations** here. They are NOAA harmonic predictions, "
            "interpolated between stations.",
            "",
        ]
    return "\n".join(lines), {"tide": tide, "wind": wind, "current_meters": meters}
