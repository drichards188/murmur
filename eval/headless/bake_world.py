"""Bake a world's grids from a running `tidewater serve` (world v3, H1 and H5): what the headless
simulator reads instead of tidewater, so `make check` and every episode run without it.

    ~/tidewater/.venv/bin/tidewater serve ~/tidewater/scenarios/golden-gate-2026-06.toml --port 9570 &
    .venv/bin/python -m eval.headless.bake_world golden-gate --url http://127.0.0.1:9570
    .venv/bin/python -m eval.headless.bake_world golden-gate --out /tmp/rebake   # elsewhere, to compare

Three files, each with a provenance file beside it (`<stem>.provenance.json`: tidewater's commit
and `/v1/info`, the scenario, the box, the spacing, the time sampled, the date, this command,
the file's SHA-256), at the paths the world file names (sim/worlds/<name>.toml):

- `[terrain] surface`: the surface height the link emulator's terrain line of sight reads,
  `max(elevation, water level)` as tidewater's `/v1/sample` answers the emulator, at the
  world's `sampled_at`, on the `[terrain]` box every `spacing_m` (the format of
  crates/murmur-headless/src/sim/terrain.rs: gzip of one JSON header line, `lat0`, `lon0`,
  `dlat`, `dlon`, `nlat`, `nlon`, `scale` metres per unit, `nodata`; then `nlat x nlon`
  little-endian i16 row by row from the south-west, decimetres, -32768 where tidewater has no
  data). Before world v3 this was eval/headless/bake_terrain.py's one output; its points, order
  and arithmetic are unchanged, so the same tidewater gives the same bytes.
- `[terrain] land_water`: the water depth on the same points at the same time, tidewater's
  `water_depth_m` (water level minus elevation: above 0 water, else land), in the same format:
  the bridge's domain rule reads it when no tidewater is configured
  (crates/murmur-bridge/src/decision/terrain.rs), and the stand-ins' conditions their depth.
- `[currents] field`: tidewater's current at the surface on the `[currents]` box every
  `spacing_m`, from `from` to `to` every `step_s`: a JSON header line (the grid's keys and `t0`,
  `dt`, `nt` unix seconds), then `nt x nlat x nlon` pairs of little-endian i16 (east, north)
  in cm/s, -32768 where tidewater has no data. Drifting contacts and our drifters and boats
  read it (crates/murmur-headless/src/sim/water.rs).

Without a `[terrain] box` the grids cover the tidewater scenario's own bounds (`/v1/info`).
The tool also prints the wind's range at the mission's centre, hourly over the currents'
window (the world's `[conditions] wind_mps`), and records it in the currents' provenance.
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
from typing import Any

from eval.world import World, load_world

NODATA = -32768


def post(url: str, body: dict, timeout: float = 120.0) -> dict:
    req = urllib.request.Request(url, data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read())


def unix(at: str) -> float:
    return dt.datetime.fromisoformat(at.replace("Z", "+00:00")).timestamp()


def lattice(box: dict[str, float], spacing_m: float) -> dict[str, Any]:
    """The grid's header over a box (bake_terrain.py's arithmetic, unchanged): the first sample at
    the south-west corner, `spacing_m` apart, east-west spacing at the box's middle latitude."""
    lat0, lon0 = box["lat_min"], box["lon_min"]
    dlat = spacing_m / 111_320.0
    dlon = spacing_m / (111_320.0 * math.cos(math.radians((box["lat_min"] + box["lat_max"]) / 2)))
    nlat = int(math.ceil((box["lat_max"] - lat0) / dlat)) + 1
    nlon = int(math.ceil((box["lon_max"] - lon0) / dlon)) + 1
    return {"lat0": lat0, "lon0": lon0, "dlat": dlat, "dlon": dlon, "nlat": nlat, "nlon": nlon}


def points(h: dict[str, Any]) -> list[tuple[float, float]]:
    return [(round(h["lat0"] + i * h["dlat"], 7), round(h["lon0"] + j * h["dlon"], 7))
            for i in range(h["nlat"]) for j in range(h["nlon"])]


def sample(url: str, t: float, pts: list[tuple[float, float]], batch: int, label: str = "") -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    for k in range(0, len(pts), batch):
        chunk = pts[k:k + batch]
        out += post(f"{url}/v1/sample", {"t": t, "points": [[la, lo, None] for la, lo in chunk]})["samples"]
        if label:
            print(f"  {label} {min(k + batch, len(pts))}/{len(pts)}", file=sys.stderr)
    return out


def i16(x: float) -> int:
    return max(-32767, min(32767, int(round(x))))


def write_grid(path: Path, header: dict[str, Any], values: list[int]) -> str:
    raw = json.dumps(header, sort_keys=True).encode() + b"\n" + struct.pack(f"<{len(values)}h", *values)
    path.parent.mkdir(parents=True, exist_ok=True)
    # mtime 0: the same samples give the same bytes
    path.write_bytes(gzip.compress(raw, compresslevel=9, mtime=0))
    return hashlib.sha256(path.read_bytes()).hexdigest()


def provenance_path(grid: Path) -> Path:
    """`golden-gate.grid.gz` -> `golden-gate.provenance.json` (the name bake_terrain.py gave it);
    `golden-gate.water.grid.gz` -> `golden-gate.water.provenance.json`; `x.currents.gz` ->
    `x.currents.provenance.json`."""
    name = grid.name
    for suffix in (".grid.gz", ".gz"):
        if name.endswith(suffix):
            return grid.with_name(name[: -len(suffix)] + ".provenance.json")
    return grid.with_name(name + ".provenance.json")


def tidewater_commit() -> str | None:
    tw = Path.home() / "tidewater"
    out = subprocess.run(["git", "-C", str(tw), "rev-parse", "HEAD"], capture_output=True, text=True)
    return out.stdout.strip() or None


def target(world_path: Path | None, out: Path | None) -> Path | None:
    if world_path is None:
        return None
    return out / world_path.name if out is not None else world_path


def bake(w: World, url: str, what: set[str], out: Path | None, batch: int, command: str) -> dict[str, Any]:
    info = json.loads(urllib.request.urlopen(f"{url}/v1/info", timeout=30).read())
    if info.get("name") != w.tidewater:
        raise SystemExit(f"tidewater serves {info.get('name')!r}, the world names {w.tidewater!r}")
    b = info["bounds"]
    scenario_box = {"lat_min": b["south"], "lat_max": b["north"], "lon_min": b["west"], "lon_max": b["east"]}
    common = {"tidewater": {"commit": tidewater_commit(), "info": info}, "scenario": w.tidewater, "world": w.name,
              "baked": dt.datetime.now(dt.timezone.utc).date().isoformat(), "command": command}
    done: dict[str, Any] = {}
    if what & {"surface", "land_water"}:
        box = w.box or scenario_box
        h = lattice(box, w.spacing_m)
        pts = points(h)
        print(f"terrain: {h['nlat']} x {h['nlon']} = {len(pts)} points at {w.spacing_m:g} m", file=sys.stderr)
        samples = sample(url, unix(w.sampled_at), pts, batch, "terrain")
        heights: list[int] = []
        depths: list[int] = []
        for s in samples:
            e = s.get("elevation_m")
            if e is None:
                heights.append(NODATA)
                depths.append(NODATA)
                continue
            lvl = s.get("water_level_m")
            heights.append(i16((max(float(e), float(lvl)) if lvl is not None else float(e)) * 10.0))
            d = s.get("water_depth_m")
            depths.append(i16(float(d) * 10.0) if d is not None else NODATA)
        header = {**h, "scale": 0.1, "nodata": NODATA}
        for kind, values, wpath, desc in (
            ("surface", heights, w.surface_grid,
             "surface height (max of elevation and water level), metres x 10, as tidewater's /v1/sample answers the link emulator"),
            ("land_water", depths, w.land_water_grid,
             "water depth (water level minus elevation: above 0 water, else land), metres x 10, tidewater's water_depth_m"),
        ):
            p = target(wpath, out)
            if kind not in what or p is None:
                continue
            sha = write_grid(p, header, values)
            prov = {**common, "file": p.name, "sha256": sha, "what": desc, "sampled_at": w.sampled_at, "box": box,
                    "spacing_m": w.spacing_m, "dims": {k: header[k] for k in ("nlat", "nlon")},
                    "nodata_points": sum(1 for v in values if v == NODATA)}
            if kind == "land_water":
                prov["water_points"] = sum(1 for v in values if v != NODATA and v > 0)
            provenance_path(p).write_text(json.dumps(prov, indent=1, sort_keys=True) + "\n")
            done[kind] = {"file": str(p), "sha256": sha, "bytes": p.stat().st_size}
            print(f"{p} ({p.stat().st_size} bytes)", file=sys.stderr)
    if "currents" in what and w.currents is not None:
        c = w.currents
        p = target(c.field, out)
        assert p is not None
        h = lattice(c.box, c.spacing_m)
        pts = points(h)
        t0, t1 = unix(c.start), unix(c.end)
        nt = int(math.floor((t1 - t0) / c.step_s + 1e-9)) + 1
        print(f"currents: {h['nlat']} x {h['nlon']} = {len(pts)} points, {nt} times", file=sys.stderr)
        values: list[int] = []
        top = 0.0
        for k in range(nt):
            for s in sample(url, t0 + k * c.step_s, pts, batch):
                if s.get("elevation_m") is None:
                    values += [NODATA, NODATA]
                    continue
                ce, cn = float(s.get("current_east") or 0.0), float(s.get("current_north") or 0.0)
                top = max(top, math.hypot(ce, cn))
                values += [i16(ce * 100.0), i16(cn * 100.0)]
        header = {**h, "scale": 0.01, "nodata": NODATA, "t0": t0, "dt": c.step_s, "nt": nt}
        sha = write_grid(p, header, values)
        # the wind at the mission's centre over the same window, hourly: the world's conditions
        lon, lat = w.mission.center
        winds = []
        for k in range(int((t1 - t0) // 3600) + 1):
            s = post(f"{url}/v1/sample", {"t": t0 + 3600.0 * k, "points": [[lat, lon, None]]})["samples"][0]
            if s.get("wind_east") is not None:
                winds.append(math.hypot(float(s["wind_east"]), float(s["wind_north"])))
        wind = [round(min(winds), 2), round(max(winds), 2)] if winds else None
        prov = {**common, "file": p.name, "sha256": sha,
                "what": "surface current (east, north), m/s x 100, tidewater's current_east and current_north",
                "box": c.box, "spacing_m": c.spacing_m, "from": c.start, "to": c.end, "step_s": c.step_s,
                "dims": {"nlat": h["nlat"], "nlon": h["nlon"], "nt": nt}, "max_speed_mps": round(top, 3),
                "wind_mps_at_mission_center": wind}
        provenance_path(p).write_text(json.dumps(prov, indent=1, sort_keys=True) + "\n")
        done["currents"] = {"file": str(p), "sha256": sha, "bytes": p.stat().st_size, "wind_mps": wind}
        print(f"{p} ({p.stat().st_size} bytes); fastest current {top:.2f} m/s; wind at the mission's centre {wind} m/s",
              file=sys.stderr)
    return done


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("world", help="a world in sim/worlds (its name) or a world file")
    ap.add_argument("--url", default="http://127.0.0.1:9570", help="tidewater serve, running the world's scenario")
    ap.add_argument("--only", nargs="+", choices=("surface", "land_water", "currents"),
                    default=["surface", "land_water", "currents"])
    ap.add_argument("--out", type=Path, help="write into this directory instead of the world's paths")
    ap.add_argument("--batch", type=int, default=20_000)
    args = ap.parse_args(argv)
    w = load_world(args.world)
    command = (f".venv/bin/python -m eval.headless.bake_world {w.name} --url {args.url} --only {' '.join(args.only)}"
               + (f" --out {args.out}" if args.out else ""))
    done = bake(w, args.url.rstrip("/"), set(args.only), args.out, args.batch, command)
    print(json.dumps(done, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
