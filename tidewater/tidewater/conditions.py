"""The query API: what is it like at this place, at this time?"""

import math
from dataclasses import asdict, dataclass

import numpy as np

from .layers import Currents, Terrain, Tide, Wind
from .scenario import Scenario
from .weather import Weather


def nullable(values):
    return [None if not math.isfinite(v) else v for v in values.tolist()]


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
    wind_east: float | None
    wind_north: float | None
    weather: dict | None = None

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
        return speed_dir(self.wind_east, self.wind_north) if self.wind_east is not None and self.wind_north is not None else (None, None)

    def as_dict(self):
        d = asdict(self)
        if self.weather is None:
            d.pop("weather")
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
        self.weather = None

    @classmethod
    def for_scenario(cls, sc: Scenario):
        c = sc.cache_dir
        needed = ["terrain.npz", "tide.json", "currents.json", "wind.json"]
        if sc.currents.get("source", "coops-predictions") == "sfbofs":
            needed.append("sfbofs_currents.npz")
        if sc.weather.get("enabled", False) and not sc.weather.get("archive"):
            needed.append("weather.json")
        missing = [n for n in needed if not (c / n).exists()]
        if missing:
            raise FileNotFoundError(f"scenario data not fetched ({', '.join(missing)}); run: tidewater fetch {sc.path}")
        weather_path = sc.path.parent / sc.weather["archive"] if sc.weather.get("archive") else c / "weather.json"
        weather = Weather.load(weather_path) if sc.weather.get("enabled", False) else None
        result = cls(Terrain.load(c / "terrain.npz"), Tide.load(c / "tide.json"),
                     cls._currents(sc), weather or Wind.load(c / "wind.json"))
        result.weather = weather
        return result

    @staticmethod
    def _currents(sc: Scenario, source=None):
        """The scenario's current layer. Station IDW is the default; "sfbofs" uses the model mesh
        and falls back to the stations outside the mesh and its time span."""
        stations = Currents.load(sc.cache_dir / "currents.json")
        source = source or sc.currents.get("source", "coops-predictions")
        if source == "coops-predictions":
            return stations
        if source != "sfbofs":
            raise ValueError(f"unknown currents source {source!r} (use 'coops-predictions' or 'sfbofs')")
        path = sc.cache_dir / "sfbofs_currents.npz"
        if not path.exists():
            raise FileNotFoundError(f"SFBOFS currents not fetched; run: tidewater fetch {sc.path}")
        from .sfbofs import SfbofsCurrents

        return SfbofsCurrents.load(path, fallback=stations)

    def at(self, lat, lon, t, depth_m=0.0) -> Sample:
        elev = self.terrain.elevation(lat, lon)
        level = self.tide.level(t)
        water_depth = None if elev is None else level - elev
        if water_depth is not None and water_depth > 0:
            ce, cn = self.currents.vector(lat, lon, t, min(depth_m, water_depth))
        else:
            ce = cn = 0.0  # no current on dry land or outside the area
        we, wn = self.wind.vector(lat, lon, t)
        return Sample(lat, lon, elev, level, water_depth, ce, cn, we, wn,
                      self.weather.at(lat, lon, t) if self.weather else None)

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
        return SampleBatch(lat, lon, elev, level, water_depth, current[:, 0], current[:, 1], wind[:, 0], wind[:, 1],
                           [self.weather.at(la, lo, t) for la, lo in zip(lat, lon)] if self.weather else None)


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
    weather: list | None = None

    def __len__(self):
        return len(self.lat)

    def samples(self):
        """List of `Sample`, as `Conditions.at` would return."""
        elev = [None if np.isnan(v) else v for v in self.elevation_m.tolist()]
        wd = [None if np.isnan(v) else v for v in self.water_depth_m.tolist()]
        return [Sample(*row) for row in zip(self.lat.tolist(), self.lon.tolist(), elev,
                                            [self.water_level_m] * len(self), wd,
                                            self.current_east.tolist(), self.current_north.tolist(),
                                            nullable(self.wind_east), nullable(self.wind_north),
                                            self.weather or [None] * len(self))]

    def to_dicts(self):
        """Plain dicts (JSON-ready): the nine `Sample` fields, None outside the area."""
        keys = ("lat", "lon", "elevation_m", "water_level_m", "water_depth_m",
                "current_east", "current_north", "wind_east", "wind_north", "weather")
        elev = [None if np.isnan(v) else v for v in self.elevation_m.tolist()]
        wd = [None if np.isnan(v) else v for v in self.water_depth_m.tolist()]
        cols = (self.lat.tolist(), self.lon.tolist(), elev, [self.water_level_m] * len(self), wd,
                self.current_east.tolist(), self.current_north.tolist(),
                nullable(self.wind_east), nullable(self.wind_north), self.weather or [None] * len(self))
        rows = [dict(zip(keys, row)) for row in zip(*cols)]
        if self.weather is None:
            for row in rows:
                row.pop("weather")
        return rows
