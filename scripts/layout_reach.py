#!/usr/bin/env python3
"""Check a situation's layout against its ground stations, and measure how spread it is.

    .venv/bin/python scripts/layout_reach.py                              # swarm-saturation
    .venv/bin/python scripts/layout_reach.py swarm-saturation-packed
    .venv/bin/python scripts/layout_reach.py --compare swarm-saturation-packed
    .venv/bin/python scripts/layout_reach.py --dem <terrain.npz> [--dem ...]   # and line of sight

Reach. For each group (a swarm, or a unit outside one), every member's link profile as the link
emulator picks it (the links file's `[platform_profile]`, a SATCOM platform's space class through
`[space_class_profile]`, else its catalog link class; `extends` followed): the situation's
`links` file, else MURMUR_LINKS, else sim/links.toml. For a profile with a range model
(`range_ok_km`, `range_max_km`), every point the member will be at (where it starts, the way to
its opening order's ring or lane, every 200 m round the ring or along the lane and back, the way
home), each against its nearest station; the worst point's distance, against the profile's clean
and maximum range, and the loss the emulator's range curve gives there (none to range_ok, rising
as ((d - ok)/(max - ok))^exp to all at range_max). An acoustic link: its nearest station and the
propagation delay there. SATCOM and tethered links use no station. Range is horizontal distance:
the heights here (under 2.5 km) change it by less than 0.2 % at the ranges that matter.

Line of sight (`--dem`, tidewater terrain .npz files, finest first, as tidewater serve answers):
for each ring or lane point at the order's lowest altitude (the task layer stacks members upward
from it), the path to the station the range curve prefers (the nearest), the terrain raised by
the Earth's bulge (k 4/3) against the ray between the antennas (each site's height from the DEM
where its alt_m is "dem", plus the profile's mast and the vehicle's antenna): how far the worst
obstruction reaches past the ray (points obstructed, and the worst). Without --dem it is not
checked (the live run's emulator does it with tidewater). Use terrain fetched after bd
murmur-cbjn and from the finest DEM (a grid with a `source` key): grids fetched before are
squeezed north-south about their middle, or ETOPO's 450 m cells on the outer coast. The cached
`bay-region-2026-06`, finest first:

    .venv/bin/python scripts/layout_reach.py --dem tidewater/data/cache/bay-region-2026-06/terrain-golden-gate.npz \\
        --dem tidewater/data/cache/bay-region-2026-06/terrain-bay.npz --dem tidewater/data/cache/bay-region-2026-06/terrain.npz

Spread. Every vehicle at its spawn and on station (an orbit's member at its own place round the
ring, as the task layer spreads them; anyone else where it starts): the nearest neighbour's
distance (median and quartiles) and the area occupied, in 1 km squares holding at least one.
"""

from __future__ import annotations

import argparse
import math
import os
import sys
import tomllib
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from eval import situation as S  # noqa: E402
from eval.stack import _swarm  # noqa: E402

M_PER_DEG = 111_320.0
EARTH_R_M = 6_371_000.0
STEP_M = 200.0


def metres(a: tuple[float, float], b: tuple[float, float]) -> float:
    """(lat, lon) to (lat, lon), flat earth at their mean latitude."""
    lat = math.radians((a[0] + b[0]) / 2)
    return math.hypot((a[0] - b[0]) * M_PER_DEG, (a[1] - b[1]) * M_PER_DEG * math.cos(lat))


def along(path: list[tuple[float, float]], step: float = STEP_M) -> list[tuple[float, float]]:
    out: list[tuple[float, float]] = []
    for a, b in zip(path, path[1:]):
        n = max(1, int(metres(a, b) / step))
        out += [(a[0] + (b[0] - a[0]) * k / n, a[1] + (b[1] - a[1]) * k / n) for k in range(n)]
    out.append(path[-1])
    return out


def circle(lat: float, lon: float, r: float, n: int = 12, phase: float = 0.0) -> list[tuple[float, float]]:
    """crates/murmur-bridge geo.rs `circle_points`: from north, clockwise."""
    k = M_PER_DEG * math.cos(math.radians(lat))
    return [(lat + r * math.cos(a) / M_PER_DEG, lon + r * math.sin(a) / k)
            for a in (phase + 2 * math.pi * i / n for i in range(n))]


# ---- the links file -------------------------------------------------------------------------


class Links:
    def __init__(self, path: Path):
        self.path = path
        self.raw = tomllib.loads(path.read_text())
        gs = self.raw.get("ground_station")
        sites = gs if isinstance(gs, list) else [gs] if gs else []
        self.stations = [(s.get("name", f"station {i + 1}"), float(s["lat"]), float(s["lon"]), s.get("alt_m", 0.0))
                         for i, s in enumerate(sites)]

    def profile_name(self, pid: str, spec: dict[str, Any]) -> str:
        swarm = _swarm()
        return swarm.link_profile(pid, spec, self.raw)

    def field(self, profile: str, key: str) -> Any:
        swarm = _swarm()
        return swarm.profile_field(self.raw, profile, key)

    def nearest(self, p: tuple[float, float]) -> tuple[str, float, tuple[float, float, Any]]:
        best = min(self.stations, key=lambda s: metres(p, (s[1], s[2])))
        return best[0], metres(p, (best[1], best[2])), (best[1], best[2], best[3])


def links_path(sit: Any) -> Path:
    if getattr(sit, "links", None):
        return Path(sit.links)
    env = os.environ.get("MURMUR_LINKS")
    return Path(env) if env else ROOT / "sim" / "links.toml"


# ---- terrain (optional) ---------------------------------------------------------------------


class Dem:
    """tidewater's terrain .npz files (elevation, bbox west/south/east/north, row 0 north), the
    finest first; the surface a radio sees over water is the sea's."""

    def __init__(self, paths: list[Path]):
        import numpy as np

        self.np = np
        self.grids = []
        for p in paths:
            d = np.load(p)
            z = d["elevation"].astype(np.float32)
            w, s, e, n = (float(v) for v in d["bbox"])
            self.grids.append((z, w, s, e, n))

    def at(self, lat: float, lon: float) -> float | None:
        for z, w, s, e, n in self.grids:
            if s <= lat <= n and w <= lon <= e:
                h, wd = z.shape
                fx = (lon - w) / (e - w) * wd - 0.5
                fy = (n - lat) / (n - s) * h - 0.5
                x0 = min(max(int(math.floor(fx)), 0), wd - 2)
                y0 = min(max(int(math.floor(fy)), 0), h - 2)
                tx = min(max(fx - x0, 0.0), 1.0)
                ty = min(max(fy - y0, 0.0), 1.0)
                top = z[y0, x0] * (1 - tx) + z[y0, x0 + 1] * tx
                bot = z[y0 + 1, x0] * (1 - tx) + z[y0 + 1, x0 + 1] * tx
                return float(top * (1 - ty) + bot * ty)
        return None

    def obstruction(self, a: tuple[float, float], ha: float, b: tuple[float, float], hb: float, spacing: float = 50.0) -> float | None:
        """crates/murmur-linkemu terrain.rs `path_obstruction_m`: metres the surface (raised by the
        bulge, k 4/3) reaches above the ray; None where the DEM has no answer."""
        total = metres(a, b)
        n = max(2, min(500, math.ceil(total / spacing)))
        worst = -1e9
        for i in range(1, n):
            f = i / n
            g = self.at(a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f)
            if g is None:
                return None
            g = max(g, 0.0)
            d1, d2 = f * total, (1 - f) * total
            worst = max(worst, g + d1 * d2 / (2 * (4 / 3) * EARTH_R_M) - (ha + (hb - ha) * f))
        return worst


# ---- where each vehicle will be -------------------------------------------------------------


def start_path(v: dict[str, Any]) -> dict[str, Any]:
    """A vehicle's own starting task (the fleet's `start`, decision rules 29) as `plan` gives an
    order's: from where it starts (`start_at`, on its station) to its orbit or lane, round it, and
    home; its place on station (an orbit's centre, a patrol's first point, a hold where it is)."""
    st = v["start"]
    home = (v["home"][0], v["home"][1])
    at = (v["start_at"][0], v["start_at"][1])
    if st["task"] == "orbit":
        lon, lat = st["center"]
        ring = circle(lat, lon, float(st.get("radius_m", 100.0)), 12)
        task, station = ring + [ring[0]], (lat, lon)
    elif st["task"] == "patrol":
        task = [(lat, lon) for lon, lat in st["route"]]
        task, station = task + [task[0]], task[0]
    else:
        task, station = [at], at
    return {"out": [at, task[0]], "task": task, "back": [task[-1], home], "station": station,
            "alt": v.get("cruise_alt_m"), "order": "start"}


def plan(sit: Any) -> tuple[list[dict[str, Any]], dict[int, dict[str, Any]]]:
    """The fleet as sim/swarm.py plans it, and each vehicle's starting task's (its own, the
    fleet's `start`) or opening order's geometry: its way out, its ring or lane, its way home, its
    place on station, the order's lowest altitude."""
    vehicles = S._planned_fleet(sit.fleet)
    by_uid = {v["uid"]: v for v in vehicles}
    paths: dict[int, dict[str, Any]] = {v["uid"]: start_path(v) for v in vehicles if "start" in v}
    for o in sit.orders:
        chosen = S._select_planned(o.vehicles, vehicles)
        p = o.params
        for slot, v in enumerate(chosen):
            home = (v["home"][0], v["home"][1])
            alt = v.get("cruise_alt_m")
            if o.task == "orbit":
                lon, lat = p["center"]
                ring = circle(lat, lon, float(p["radius_m"]), 12, 2 * math.pi * slot / len(chosen))
                task = ring + [ring[0]]
                station = ring[0]
            elif o.task == "patrol":
                task = [(lat, lon) for lon, lat in p["route"]]
                task = task + [task[0]]
                station = home
            else:
                task, station = [home], home
            paths[v["uid"]] = {"out": [home, task[0]], "task": task, "back": [task[-1], home], "station": station,
                               "alt": alt, "order": o.id}
    for uid, v in by_uid.items():
        paths.setdefault(uid, {"out": [], "task": [(v["home"][0], v["home"][1])], "back": [],
                               "station": (v["home"][0], v["home"][1]), "alt": v["home"][2], "order": None})
    return vehicles, paths


def groups(vehicles: list[dict[str, Any]]) -> dict[str, list[dict[str, Any]]]:
    out: dict[str, list[dict[str, Any]]] = {}
    for v in vehicles:
        out.setdefault(v.get("swarm") or v["callsign"], []).append(v)
    return out


# ---- the report -----------------------------------------------------------------------------


def reach(sit: Any, dem: Dem | None = None, quiet: bool = False) -> dict[str, Any]:
    links = Links(links_path(sit))
    swarm = _swarm()
    catalog = swarm.load_platforms()
    vehicles, paths = plan(sit)
    rows = []
    totals = {"vehicles": len(vehicles), "beyond_ok": 0, "beyond_max": 0, "los_blocked": 0, "los_checked": 0}
    for gid, members in groups(vehicles).items():
        v0 = members[0]
        pid = v0["platform"]
        spec = catalog[pid]
        prof = links.profile_name(pid, spec)
        ok, mx = links.field(prof, "range_ok_km"), links.field(prof, "range_max_km")
        exp = links.field(prof, "range_exp") or 1.0
        prop = links.field(prof, "propagation_mps")
        worst_d, worst_name, beyond_ok, beyond_max = 0.0, "", 0, 0
        # the group's middle: its members' places on station (every member's ring or lane is the
        # first's when they share an order)
        centre = [paths[v["uid"]]["station"] for v in members]
        mid = (sum(p[0] for p in centre) / len(centre), sum(p[1] for p in centre) / len(centre))
        near_name, near_d, _ = links.nearest(mid)
        los = None
        for v in members:
            pa = paths[v["uid"]]
            pts = along(pa["out"]) + along(pa["task"]) + along(pa["back"]) if pa["out"] else along(pa["task"])
            dv = max(links.nearest(p)[1] for p in pts)
            if dv > worst_d:
                worst_d, worst_name = dv, links.nearest(max(pts, key=lambda p: links.nearest(p)[1]))[0]
            if ok is not None and dv > ok * 1000:
                beyond_ok += 1
            if mx is not None and dv > mx * 1000:
                beyond_max += 1
        if dem is not None and ok is not None:
            los = los_check(links, prof, members, paths, dem)
            totals["los_checked"] += los["checked"]
            totals["los_blocked"] += los["blocked"]
        loss = None
        if ok is not None and mx is not None:
            d = worst_d / 1000
            loss = 0.0 if d <= ok else 1.0 if d >= mx else ((d - ok) / (mx - ok)) ** exp
            totals["beyond_ok"] += beyond_ok
            totals["beyond_max"] += beyond_max
        rows.append({
            "group": gid, "name": (sit_swarm_name(sit, gid) or v0["callsign"]), "platform": pid, "count": len(members),
            "profile": prof, "ok_km": ok, "max_km": mx, "near": near_name, "near_km": near_d / 1000,
            "worst_km": worst_d / 1000, "worst_station": worst_name, "beyond_ok": beyond_ok, "beyond_max": beyond_max,
            "loss": loss, "acoustic_s": (worst_d / prop if prop else None), "los": los,
        })
    if not quiet:
        print(f"{sit.name}: {len(vehicles)} vehicles, links {links.path.relative_to(ROOT) if links.path.is_relative_to(ROOT) else links.path} "
              f"({len(links.stations)} ground station{'s' if len(links.stations) != 1 else ''})")
        for s in links.stations:
            print(f"  station {s[0]}: {s[1]:.4f}, {s[2]:.4f}, alt_m {s[3]}")
        print()
        hdr = f"{'group':<11} {'n':>4} {'platform':<10} {'profile':<11} {'nearest station (centre)':<28} {'worst':>7} {'ok/max':>8} {'>ok':>4} {'>max':>4} {'loss':>5}"
        print(hdr)
        for r in rows:
            okmax = f"{r['ok_km']:.0f}/{r['max_km']:.0f}" if r["ok_km"] is not None else "-"
            worst = f"{r['worst_km']:.1f}" if r["ok_km"] is not None or r["acoustic_s"] is not None else "-"
            loss = f"{r['loss']:.2f}" if r["loss"] is not None else ("-" if r["acoustic_s"] is None else f"{r['acoustic_s']:.1f}s")
            near = f"{r['near']} {r['near_km']:.1f} km" if (r["ok_km"] is not None or r["acoustic_s"] is not None) else "none (no station)"
            print(f"{r['group']:<11} {r['count']:>4} {r['platform']:<10} {r['profile']:<11} {near:<28} {worst:>7} {okmax:>8} "
                  f"{r['beyond_ok'] if r['ok_km'] is not None else '-':>4} {r['beyond_max'] if r['ok_km'] is not None else '-':>4} {loss:>5}"
                  + (f"  LOS: {r['los']['blocked']}/{r['los']['checked']} points obstructed, worst {r['los']['worst']}"
                     if r["los"] else ""))
        print()
        print(f"vehicles with a range model: every point within range_ok: {sum(r['count'] - r['beyond_ok'] for r in rows if r['ok_km'] is not None)}; "
              f"some point beyond range_ok: {totals['beyond_ok']}; beyond range_max: {totals['beyond_max']}")
        if dem is not None:
            print(f"line of sight: {totals['los_blocked']} of {totals['los_checked']} sampled points obstructed past the ramp's start")
    return {"rows": rows, "totals": totals, "stations": links.stations}


def sit_swarm_name(sit: Any, gid: str) -> str | None:
    swarm = _swarm()
    for s in swarm.fleet_swarms(swarm.load_fleet(str(sit.fleet))):
        if s["id"] == gid:
            return s.get("name")
    return None


def los_check(links: Links, prof: str, members: list[dict[str, Any]], paths: dict[int, dict[str, Any]], dem: Dem) -> dict[str, Any]:
    """The group's rings or lanes every 500 m, at the lowest altitude each is flown (a group order's
    is the first member's, every member sharing it; with starting tasks, every member's own), to
    its nearest station."""
    shared = all(paths[v["uid"]]["order"] != "start" for v in members)
    pts_alt: list[tuple[tuple[float, float], float]] = []
    for v in members[:1] if shared else members:
        pa = paths[v["uid"]]
        alt = pa["alt"] if pa["alt"] is not None else v["home"][2]
        pts_alt += [(p, float(alt)) for p in along(pa["task"], 500.0)]
    gs_ant = links.field(prof, "gs_antenna_m") or 0.0
    ant = links.field(prof, "antenna_m") or 0.0
    checked = blocked = 0
    worst = None
    for p, alt in pts_alt:
        _, _, (slat, slon, salt) = links.nearest(p)
        h_site = dem.at(slat, slon) if salt == "dem" else float(salt)
        if h_site is None:
            continue
        o = dem.obstruction((slat, slon), h_site + gs_ant, p, float(alt) + ant)
        if o is None:
            continue
        checked += 1
        worst = o if worst is None else max(worst, o)
        if o > 0:
            blocked += 1
    return {"checked": checked, "blocked": blocked, "worst": None if worst is None else round(worst, 1)}


def spread(sit: Any, quiet: bool = False) -> dict[str, Any]:
    vehicles, paths = plan(sit)
    lat0 = sum(v["home"][0] for v in vehicles) / len(vehicles)
    k = M_PER_DEG * math.cos(math.radians(lat0))

    def stats(points: list[tuple[float, float]]) -> dict[str, Any]:
        xy = [(p[1] * k, p[0] * M_PER_DEG) for p in points]
        cell = 500.0
        grid: dict[tuple[int, int], list[int]] = {}
        for i, (x, y) in enumerate(xy):
            grid.setdefault((int(x // cell), int(y // cell)), []).append(i)
        nn = []
        for i, (x, y) in enumerate(xy):
            cx, cy = int(x // cell), int(y // cell)
            best = math.inf
            r = 1
            while True:
                for gx in range(cx - r, cx + r + 1):
                    for gy in range(cy - r, cy + r + 1):
                        for j in grid.get((gx, gy), ()):
                            if j != i:
                                best = min(best, math.hypot(xy[j][0] - x, xy[j][1] - y))
                if best <= r * cell or r > 400:
                    break
                r *= 2
            nn.append(best)
        nn.sort()
        q = lambda f: nn[min(len(nn) - 1, int(f * len(nn)))]  # noqa: E731
        km = {(int(x // 1000), int(y // 1000)) for x, y in xy}
        return {"nn_median_m": q(0.5), "nn_q1_m": q(0.25), "nn_q3_m": q(0.75), "km2": len(km)}

    spawn = stats([(v["home"][0], v["home"][1]) for v in vehicles])
    station = stats([paths[v["uid"]]["station"] for v in vehicles])
    lats = [v["home"][0] for v in vehicles]
    lons = [v["home"][1] for v in vehicles]
    box_km2 = (max(lats) - min(lats)) * M_PER_DEG / 1000 * (max(lons) - min(lons)) * k / 1000
    out = {"spawn": spawn, "station": station, "box_km2": box_km2}
    if not quiet:
        print(f"{sit.name}: spread of {len(vehicles)} vehicles")
        for what, s in (("at spawn", spawn), ("on station", station)):
            print(f"  {what:<11} nearest neighbour median {s['nn_median_m']:.0f} m (quartiles {s['nn_q1_m']:.0f} to "
                  f"{s['nn_q3_m']:.0f} m); occupied 1 km squares: {s['km2']}")
        print(f"  the box round every spawn: {box_km2:,.0f} square km")
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("situation", nargs="?", default="swarm-saturation")
    ap.add_argument("--compare", help="another situation's spread beside it (e.g. swarm-saturation-packed)")
    ap.add_argument("--dem", type=Path, action="append", help="tidewater terrain .npz, finest first (repeatable)")
    args = ap.parse_args()
    sit = S.load_situation(args.situation)
    dem = Dem(args.dem) if args.dem else None
    r = reach(sit, dem)
    print()
    spread(sit)
    if args.compare:
        print()
        spread(S.load_situation(args.compare))
    return 1 if r["totals"]["beyond_max"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
