"""Cross-check murmur-space against Skyfield (independent SGP4 wrapper and frame chain).

Usage (any venv with skyfield):
    python crates/murmur-space/tools/crosscheck_skyfield.py [--bin target/release/murmur-space]

Checks, with the same cached element sets:
1. Earth-fixed positions of a sample of satellites from every class at several instants
   (murmur-space: TEME -> GMST rotation, no polar motion, UT1=UTC; Skyfield: TEME -> GCRS ->
   ITRS with its own UT1 and IAU 2000 precession-nutation).
2. ISS pass times (10 deg elevation) over the Golden Gate Bridge for three days.
"""

import argparse
import json
import math
import random
import subprocess
from datetime import datetime, timedelta, timezone
from pathlib import Path

from sgp4 import omm
from sgp4.api import Satrec
from skyfield.api import EarthSatellite, load, wgs84
from skyfield.framelib import itrs

ROOT = Path(__file__).resolve().parents[3]
CACHE = ROOT / "sim/space/cache"
LAT, LON = 37.8199, -122.4783


def run(bin_, *args):
    out = subprocess.run([bin_, *args, "--json", "--epoch", "direct"], check=True, capture_output=True, text=True)
    return json.loads(out.stdout)


def skyfield_sats(ts, files):
    sats = {}
    for f in files:
        for rec in json.loads((CACHE / f).read_text()):
            s = Satrec()
            omm.initialize(s, rec)
            sats[int(rec["NORAD_CAT_ID"])] = EarthSatellite.from_satrec(s, ts)
    return sats


def parse(t):
    return datetime.fromisoformat(t.replace("Z", "+00:00"))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=str(ROOT / "target/release/murmur-space"))
    a = ap.parse_args()
    ts = load.timescale()
    files = [p.name for p in CACHE.glob("*.json") if not p.name.endswith(".meta.json")]
    sk = skyfield_sats(ts, files)

    # 1. positions
    random.seed(1)
    worst = {}
    diffs = []
    for cls in ["starlink", "iridium", "wgs", "worldview", "skysat", "capella", "iceye", "gps", "iss"]:
        for at in ["2026-10-01T22:00:00Z", "2026-10-02T06:00:00Z", "2026-10-03T12:00:00Z"]:
            rep = run(a.bin, "positions", "--class", cls, "--at", at)
            pos = rep["positions"]
            if cls == "starlink":
                pos = random.sample(pos, 200)
            t = ts.from_datetime(parse(at))
            for p in pos:
                s = sk.get(p["norad_id"])
                if s is None:
                    continue
                x, y, z = s.at(t).frame_xyz(itrs).km
                g = p["position"]
                lat, lon, alt = math.radians(g["lat_deg"]), math.radians(g["lon_deg"]), g["alt_km"]
                a_, f = 6378.137, 1 / 298.257223563
                e2 = f * (2 - f)
                n = a_ / math.sqrt(1 - e2 * math.sin(lat) ** 2)
                mx = (n + alt) * math.cos(lat) * math.cos(lon)
                my = (n + alt) * math.cos(lat) * math.sin(lon)
                mz = (n * (1 - e2) + alt) * math.sin(lat)
                d = math.dist((x, y, z), (mx, my, mz))
                diffs.append(d)
                worst[cls] = max(worst.get(cls, 0), d)
    diffs.sort()
    print(f"positions: {len(diffs)} comparisons, median {diffs[len(diffs)//2]*1000:.1f} m, "
          f"p99 {diffs[int(len(diffs)*0.99)]*1000:.1f} m, max {diffs[-1]*1000:.1f} m")
    for k, v in worst.items():
        print(f"  {k:<10} worst {v*1000:7.1f} m")

    # 2. ISS passes
    start = datetime(2026, 10, 2, tzinfo=timezone.utc)
    end = start + timedelta(days=3)
    rep = run(a.bin, "link", "--lat", str(LAT), "--lon", str(LON), "--class", "iss",
              "--from", start.isoformat(), "--to", end.isoformat(), "--limit", "100")
    mine = rep["classes"][0]["windows"]
    iss = sk[25544]
    obs = wgs84.latlon(LAT, LON)
    t, ev = iss.find_events(obs, ts.from_datetime(start), ts.from_datetime(end), altitude_degrees=10.0)
    passes, cur = [], {}
    for ti, e in zip(t, ev):
        if e == 0:
            cur = {"rise": ti.utc_datetime()}
        elif e == 1 and "rise" in cur:
            cur["peak"] = ti.utc_datetime()
            cur["max_el"] = (iss - obs).at(ti).altaz()[0].degrees
        elif e == 2 and "rise" in cur:
            cur["set"] = ti.utc_datetime()
            passes.append(cur)
            cur = {}
    print(f"\nISS passes >= 10 deg over the Golden Gate Bridge, {start:%Y-%m-%d} + 3 days: "
          f"skyfield {len(passes)}, murmur-space {len([w for w in mine if not w['clipped']])}")
    print(f"  {'rise (UTC)':<20} {'d rise s':>8} {'d peak s':>8} {'d set s':>8} {'max el':>7} {'d el':>6}")
    worst_dt = 0.0
    for p in passes:
        m = min(mine, key=lambda w: abs((parse(w["start"]) - p["rise"]).total_seconds()))
        dr = (parse(m["start"]) - p["rise"]).total_seconds()
        dp = (parse(m["peak_time"]) - p["peak"]).total_seconds()
        ds = (parse(m["end"]) - p["set"]).total_seconds()
        de = m["peak_elevation_deg"] - p["max_el"]
        worst_dt = max(worst_dt, abs(dr), abs(ds))
        print(f"  {p['rise']:%m-%d %H:%M:%S}       {dr:8.2f} {dp:8.2f} {ds:8.2f} {p['max_el']:7.2f} {de:6.3f}")
    print(f"  worst rise/set difference {worst_dt:.2f} s")


if __name__ == "__main__":
    main()
