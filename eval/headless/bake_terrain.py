"""Bake the surface-height grid the headless simulator's link emulator reads for its terrain
line of sight (docs/HEADLESS.md; crates/murmur-headless/src/sim/terrain.rs), from a running
`tidewater serve`: what its `/v1/sample` answers the emulator, `max(elevation, water level)`,
at the scenario's start, on a regular grid.

    ~/tidewater/.venv/bin/tidewater serve ~/tidewater/scenarios/golden-gate-2026-06.toml --port 9570 &
    .venv/bin/python -m eval.headless.bake_terrain --url http://127.0.0.1:9570

Writes `sim/headless/terrain/golden-gate.grid.gz` (gzip: one JSON header line, then i16
decimetres row by row from the south-west, -32768 where tidewater has no data) and
`golden-gate.provenance.json` beside it (tidewater's commit and version, the scenario, the box,
the spacing, the time sampled, the date, this command, the grid's SHA-256).
"""

from __future__ import annotations

import argparse
import datetime as dt
import gzip
import hashlib
import json
import math
import struct
import subprocess
import sys
import urllib.request
from pathlib import Path

from . import REPO

OUT = REPO / "sim" / "headless" / "terrain"
#: the Golden Gate: every family's vehicles and the ground station at Crissy Field, with margin
BOX = {"lat_min": 37.74, "lat_max": 37.90, "lon_min": -122.62, "lon_max": -122.36}
SPACING_M = 20.0
AT = "2026-06-14T10:00:00Z"
NODATA = -32768


def post(url: str, body: dict, timeout: float = 120.0) -> dict:
    req = urllib.request.Request(url, data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read())


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--url", default="http://127.0.0.1:9570")
    ap.add_argument("--name", default="golden-gate")
    ap.add_argument("--scenario", default="golden-gate-2026-06")
    ap.add_argument("--spacing-m", type=float, default=SPACING_M)
    ap.add_argument("--batch", type=int, default=20_000)
    args = ap.parse_args(argv)
    t = dt.datetime.fromisoformat(AT.replace("Z", "+00:00")).timestamp()
    lat0, lon0 = BOX["lat_min"], BOX["lon_min"]
    dlat = args.spacing_m / 111_320.0
    dlon = args.spacing_m / (111_320.0 * math.cos(math.radians((BOX["lat_min"] + BOX["lat_max"]) / 2)))
    nlat = int(math.ceil((BOX["lat_max"] - lat0) / dlat)) + 1
    nlon = int(math.ceil((BOX["lon_max"] - lon0) / dlon)) + 1
    print(f"{nlat} x {nlon} = {nlat * nlon} points at {args.spacing_m:g} m", file=sys.stderr)
    info = json.loads(urllib.request.urlopen(f"{args.url}/v1/info", timeout=30).read())
    points = [(round(lat0 + i * dlat, 7), round(lon0 + j * dlon, 7)) for i in range(nlat) for j in range(nlon)]
    heights: list[int] = []
    for k in range(0, len(points), args.batch):
        chunk = points[k:k + args.batch]
        out = post(f"{args.url}/v1/sample", {"t": t, "points": [[la, lo, None] for la, lo in chunk]})
        for s in out["samples"]:
            e = s.get("elevation_m")
            if e is None:
                heights.append(NODATA)
                continue
            w = s.get("water_level_m")
            h = max(float(e), float(w)) if w is not None else float(e)
            heights.append(max(-32767, min(32767, int(round(h * 10.0)))))
        print(f"  {min(k + args.batch, len(points))}/{len(points)}", file=sys.stderr)
    header = {"lat0": lat0, "lon0": lon0, "dlat": dlat, "dlon": dlon, "nlat": nlat, "nlon": nlon, "scale": 0.1,
              "nodata": NODATA}
    raw = json.dumps(header, sort_keys=True).encode() + b"\n" + struct.pack(f"<{len(heights)}h", *heights)
    OUT.mkdir(parents=True, exist_ok=True)
    grid = OUT / f"{args.name}.grid.gz"
    # mtime 0: the same samples give the same bytes
    grid.write_bytes(gzip.compress(raw, compresslevel=9, mtime=0))
    tw = Path.home() / "tidewater"
    commit = subprocess.run(["git", "-C", str(tw), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    prov = {
        "file": grid.name,
        "sha256": hashlib.sha256(grid.read_bytes()).hexdigest(),
        "what": "surface height (max of elevation and water level), metres x 10, as tidewater's /v1/sample answers the link emulator",
        "tidewater": {"commit": commit or None, "info": info},
        "scenario": args.scenario,
        "sampled_at": AT,
        "box": BOX,
        "spacing_m": args.spacing_m,
        "dims": {k: header[k] for k in ("nlat", "nlon")},
        "nodata_points": sum(1 for h in heights if h == NODATA),
        "baked": dt.datetime.now(dt.timezone.utc).date().isoformat(),
        "command": f".venv/bin/python -m eval.headless.bake_terrain --url {args.url} --name {args.name} "
                   f"--scenario {args.scenario} --spacing-m {args.spacing_m:g}",
    }
    (OUT / f"{args.name}.provenance.json").write_text(json.dumps(prov, indent=1, sort_keys=True) + "\n")
    print(f"{grid} ({grid.stat().st_size} bytes), {prov['nodata_points']} points without data", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
