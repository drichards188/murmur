"""A scenario is an area and a time window, plus where its data comes from."""

import datetime as dt
import math
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

CACHE_ROOT = Path(__file__).resolve().parent.parent / "data" / "cache"


def parse_time(s: str) -> dt.datetime:
    t = dt.datetime.fromisoformat(s.replace("Z", "+00:00"))
    return t if t.tzinfo else t.replace(tzinfo=dt.timezone.utc)


@dataclass
class Area:
    west: float
    south: float
    east: float
    north: float
    resolution_m: float

    def contains(self, lat, lon):
        return self.south <= lat <= self.north and self.west <= lon <= self.east

    @property
    def size_px(self):
        """Grid size (width, height) giving roughly resolution_m per cell."""
        mid = math.radians((self.south + self.north) / 2)
        w_m = (self.east - self.west) * 111_320 * math.cos(mid)
        h_m = (self.north - self.south) * 111_320
        return max(2, round(w_m / self.resolution_m)), max(2, round(h_m / self.resolution_m))


@dataclass
class Scenario:
    name: str
    title: str
    area: Area
    start: dt.datetime
    end: dt.datetime
    tide: dict
    currents: dict
    wind: dict
    spawn: dict = field(default_factory=dict)
    path: Path | None = None

    @classmethod
    def load(cls, path) -> "Scenario":
        path = Path(path)
        raw = tomllib.loads(path.read_text())
        return cls(
            name=raw["name"],
            title=raw.get("title", raw["name"]),
            area=Area(**raw["area"]),
            start=parse_time(raw["time"]["start"]),
            end=parse_time(raw["time"]["end"]),
            tide=raw["tide"],
            currents=raw["currents"],
            wind=raw["wind"],
            spawn={k: tuple(v) for k, v in raw.get("spawn", {}).items()},
            path=path,
        )

    @property
    def cache_dir(self) -> Path:
        d = CACHE_ROOT / self.name
        d.mkdir(parents=True, exist_ok=True)
        return d
