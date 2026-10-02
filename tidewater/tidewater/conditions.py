"""The query API: what is it like at this place, at this time?"""

import math
from dataclasses import asdict, dataclass

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
