"""The query API: what is it like at this place, at this time?"""

import math
from dataclasses import asdict, dataclass

import numpy as np

from .layers import Currents, Terrain, Tide, Wind
from .scenario import Scenario


def speed_dir(east, north):
    """(east, north) vector -> (speed, direction it points TOWARD, degrees true)."""
    return math.hypot(east, north), math.degrees(math.atan2(east, north)) % 360


@dataclass
class Sample:
    lat: float
    lon: float
    elevation_m: float | None  # ground/seabed height above datum; None outside the area
    water_level_m: float  # tide height above datum
    water_depth_m: float | None  # water_level - elevation; <= 0 means dry land
    current_east: float
    current_north: float
    wind_east: float
    wind_north: float

    @property
    def in_area(self):
        return self.elevation_m is not None

    @property
    def wet(self):
        return self.in_area and self.water_depth_m > 0

    def navigable(self, draft_m):
        """Enough water under the keel at this tide."""
        return self.in_area and self.water_depth_m > draft_m

    @property
    def current(self):
        return speed_dir(self.current_east, self.current_north)

    @property
    def wind(self):
        return speed_dir(self.wind_east, self.wind_north)

    def as_dict(self):
        d = asdict(self)
        cs, cd = self.current
        ws, wd = self.wind
        d.update(current_speed=cs, current_toward_deg=cd, wind_speed=ws, wind_toward_deg=wd, wet=self.wet)
        return d


class Conditions:
    def __init__(self, terrain: Terrain, tide: Tide, currents: Currents, wind: Wind):
        self.terrain = terrain
        self.tide = tide
        self.currents = currents
        self.wind = wind

    @classmethod
    def for_scenario(cls, sc: Scenario):
        c = sc.cache_dir
        missing = [n for n in ("terrain.npz", "tide.json", "currents.json", "wind.json") if not (c / n).exists()]
        if missing:
            raise FileNotFoundError(f"scenario data not fetched ({', '.join(missing)}); run: tidewater fetch {sc.path}")
        return cls(Terrain.load(c / "terrain.npz"), Tide.load(c / "tide.json"),
                   Currents.load(c / "currents.json"), Wind.load(c / "wind.json"))

    def at(self, lat, lon, t, depth_m=0.0) -> Sample:
        elev = self.terrain.elevation(lat, lon)
        level = self.tide.level(t)
        water_depth = None if elev is None else level - elev
        if water_depth is not None and water_depth > 0:
            ce, cn = self.currents.vector(lat, lon, t, min(depth_m, water_depth))
        else:
            ce = cn = 0.0  # no current on dry land or outside the area
        we, wn = self.wind.vector(lat, lon, t)
        return Sample(lat, lon, elev, level, water_depth, ce, cn, we, wn)

    def sample_many(self, lat, lon, t, depth_m=None) -> "SampleBatch":
        """`at` for many points at one time, vectorized over points. depth_m is None (surface),
        a scalar, or an array with NaN meaning surface. Time-dependent quantities are computed
        once. Matches `at` point for point."""
        lat = np.asarray(lat, dtype=float)
        lon = np.asarray(lon, dtype=float)
        depth = np.zeros(lat.shape) if depth_m is None else np.broadcast_to(np.asarray(depth_m, dtype=float), lat.shape)
        depth = np.where(np.isnan(depth), 0.0, depth)
        elev = self.terrain.elevation_many(lat, lon)
        level = self.tide.level(t)
        water_depth = level - elev  # NaN where outside the area
        wet = water_depth > 0  # False for NaN
        current = np.zeros((len(lat), 2))
        if wet.any():
            current[wet] = self.currents.vector_many(lat[wet], lon[wet], t, np.minimum(depth[wet], water_depth[wet]))
        wind = self.wind.vector_many(lat, lon, t)
        return SampleBatch(lat, lon, elev, level, water_depth, current[:, 0], current[:, 1], wind[:, 0], wind[:, 1])


@dataclass
class SampleBatch:
    """Array form of `Sample`: elevation_m and water_depth_m are NaN outside the area."""
    lat: np.ndarray
    lon: np.ndarray
    elevation_m: np.ndarray
    water_level_m: float
    water_depth_m: np.ndarray
    current_east: np.ndarray
    current_north: np.ndarray
    wind_east: np.ndarray
    wind_north: np.ndarray

    def __len__(self):
        return len(self.lat)

    def samples(self):
        """List of `Sample`, as `Conditions.at` would return."""
        elev = [None if np.isnan(v) else v for v in self.elevation_m.tolist()]
        wd = [None if np.isnan(v) else v for v in self.water_depth_m.tolist()]
        return [Sample(*row) for row in zip(self.lat.tolist(), self.lon.tolist(), elev,
                                            [self.water_level_m] * len(self), wd,
                                            self.current_east.tolist(), self.current_north.tolist(),
                                            self.wind_east.tolist(), self.wind_north.tolist())]

    def to_dicts(self):
        """Plain dicts (JSON-ready): the nine `Sample` fields, None outside the area."""
        keys = ("lat", "lon", "elevation_m", "water_level_m", "water_depth_m",
                "current_east", "current_north", "wind_east", "wind_north")
        elev = [None if np.isnan(v) else v for v in self.elevation_m.tolist()]
        wd = [None if np.isnan(v) else v for v in self.water_depth_m.tolist()]
        cols = (self.lat.tolist(), self.lon.tolist(), elev, [self.water_level_m] * len(self), wd,
                self.current_east.tolist(), self.current_north.tolist(),
                self.wind_east.tolist(), self.wind_north.tolist())
        return [dict(zip(keys, row)) for row in zip(*cols)]
