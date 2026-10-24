"""Where a line-of-sight or mesh datalink holds, per world (the v3 oracle's; truth about the world,
never given to an agent under test). The link emulator's own terrain check (crates/murmur-linkemu
terrain.rs `path_obstruction_m`), as agents_v2.LinkMap computes it for the Golden Gate, on any
world: the world's surface grid and ground station, the link class's antenna heights and k factor
(sim/links.toml), one answer per 250 m cell at its centre over the world's operating box, as the
emulator caches it. A packet is lost with probability obstruction / 20 m ("ramp"); the vehicle's
own lost-link failsafe follows after `lost_link_s` without one.
"""

from __future__ import annotations

import math
import tomllib
from functools import lru_cache

import numpy as np

from eval.world import load_world

from .families import Grid

M = 111_320.0


class LinkMapV3:
    CELL_M = 250.0
    SPACING_M = 50.0

    def __init__(self, world: str, link_class: str, alt_m: float):
        w = load_world(world)
        links = tomllib.loads(w.links.read_text())
        cls = (links.get("class") or {}).get(link_class) or {}
        gs = w.ground_stations[0] if w.ground_stations else None
        g0 = links.get("ground_station") or {}
        self.gs = (gs.lon, gs.lat) if gs else (float(g0.get("lon", 0.0)), float(g0.get("lat", 0.0)))
        alt0 = gs.alt_m if gs else float(g0.get("alt_m", 0.0) if not isinstance(g0.get("alt_m"), str) else 0.0)
        self.h_a = float(alt0) + float(cls.get("gs_antenna_m", 0.0))
        self.h_b = float(alt_m) + float(cls.get("antenna_m", 0.0))
        self.k = float(cls.get("k_factor", 4.0 / 3.0))
        self.range_ok_m = 1000.0 * float(cls.get("range_ok_km", 1e6))
        assert w.surface_grid is not None
        g = Grid(w.surface_grid)
        self.hdr = g.h
        raw = np.array(g.v, dtype=float).reshape(g.h["nlat"], g.h["nlon"])
        self.nodata = raw == g.h["nodata"]
        self.surface = np.where(self.nodata, 0.0, raw * g.h["scale"])
        self.dlat = self.CELL_M / M
        self.dlon = self.dlat / max(math.cos(math.radians(self.gs[1])), 0.05)
        box = w.operating_box
        self.ci0, self.ci1 = int(math.floor(box["lat_min"] / self.dlat)), int(math.floor(box["lat_max"] / self.dlat))
        self.cj0, self.cj1 = int(math.floor(box["lon_min"] / self.dlon)), int(math.floor(box["lon_max"] / self.dlon))
        lat = (np.arange(self.ci0, self.ci1 + 1) + 0.5) * self.dlat
        lon = (np.arange(self.cj0, self.cj1 + 1) + 0.5) * self.dlon
        LAT, LON = np.meshgrid(lat, lon, indexing="ij")
        self.obs = self._obstruction(LON.ravel(), LAT.ravel()).reshape(LAT.shape)

    def _height(self, lon: np.ndarray, lat: np.ndarray) -> np.ndarray:
        h = self.hdr
        y = (lat - h["lat0"]) / h["dlat"]
        x = (lon - h["lon0"]) / h["dlon"]
        i = np.floor(y).astype(int)
        j = np.floor(x).astype(int)
        ok = (i >= 0) & (j >= 0) & (i + 1 < h["nlat"]) & (j + 1 < h["nlon"])
        i, j = np.clip(i, 0, h["nlat"] - 2), np.clip(j, 0, h["nlon"] - 2)
        fy, fx = y - i, x - j
        s = self.surface
        v = (s[i, j] * (1 - fy) * (1 - fx) + s[i + 1, j] * fy * (1 - fx) + s[i, j + 1] * (1 - fy) * fx
             + s[i + 1, j + 1] * fy * fx)
        bad = self.nodata[i, j] | self.nodata[i + 1, j] | self.nodata[i, j + 1] | self.nodata[i + 1, j + 1]
        return np.where(ok & ~bad, v, 0.0)

    def _obstruction(self, lon: np.ndarray, lat: np.ndarray) -> np.ndarray:
        out = np.empty(len(lon))
        R_E = 6_371_000.0
        for k in range(len(lon)):
            dx = (lon[k] - self.gs[0]) * M * math.cos(math.radians((lat[k] + self.gs[1]) / 2))
            dy = (lat[k] - self.gs[1]) * M
            total = math.hypot(dx, dy)
            n = int(min(max(math.ceil(total / self.SPACING_M), 2), 500))
            f = np.arange(1, n) / n
            s = self._height(self.gs[0] + (lon[k] - self.gs[0]) * f, self.gs[1] + (lat[k] - self.gs[1]) * f)
            d1 = f * total
            bulge = d1 * (total - d1) / (2.0 * self.k * R_E)
            ray = self.h_a + (self.h_b - self.h_a) * f
            obstruction = float(np.max(s + bulge - ray)) if len(s) else -1e4
            out[k] = obstruction if total <= self.range_ok_m else max(obstruction, 20.0 * (total / self.range_ok_m - 1.0))
        return out

    def at_ll(self, lon: np.ndarray, lat: np.ndarray) -> np.ndarray:
        """The obstruction (m; above 0 the link loses packets, 20 m all of them) at points."""
        lon = np.asarray(lon, dtype=float)
        lat = np.asarray(lat, dtype=float)
        ci = np.floor(lat / self.dlat).astype(int) - self.ci0
        cj = np.floor(lon / self.dlon).astype(int) - self.cj0
        ok = (ci >= 0) & (cj >= 0) & (ci < self.obs.shape[0]) & (cj < self.obs.shape[1])
        return np.where(ok, self.obs[np.clip(ci, 0, self.obs.shape[0] - 1), np.clip(cj, 0, self.obs.shape[1] - 1)], 1e4)


@lru_cache(maxsize=None)
def link_map(world: str, link_class: str, alt_m: float) -> LinkMapV3:
    return LinkMapV3(world, link_class, round(float(alt_m), 0))
