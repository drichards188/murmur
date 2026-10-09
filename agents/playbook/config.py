"""The playbook's configuration: the shipped defaults (`defaults.toml`, beside this file) with a
situation's `[playbook]` table on top, checked, canonical and hashed.

A situation's table (sim/situations/README.md, "The playbook") may set:

    plays = [...]               the plays on (replaces the default list)
    enable = [...]              plays turned on besides the default list (isr.armed_overwatch)
    disable = [...]             plays turned off
    sensor_platform = "puma"    a preferred sensor platform id
    tick_s = 2
    [[playbook.watch_area]]     name, and center = [lon, lat] with radius_m, or polygon = [[lon, lat], ...]
    [[playbook.protected]]      name, point = [lon, lat]
    [playbook.thresholds]       any key of the defaults' [thresholds]
    [playbook.budget]           any key of the defaults' [budget]
    [playbook.cooldown]         any key of the defaults' [cooldown]
    [playbook.material]         any key of the defaults' [material]

and the keys the deterministic agent of before took, kept working: `near` = [lon, lat] with
`radius_km` (a watch area named "watch point", 4 km by default), `platform` (the preferred
sensor platform) and `every_s` (the tick).

The canonical form is the merged config as JSON with sorted keys; its SHA-256, cut to 12 hex
digits, is the config hash, and `<name>#<hash>` the label every evidence record carries.
"""

from __future__ import annotations

import copy
import hashlib
import json
import math
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from . import ENGINE_VERSION

DEFAULTS = Path(__file__).resolve().parent / "defaults.toml"
LEGACY_KEYS = {"near", "radius_km", "every_s", "platform"}
TABLE_KEYS = {"plays", "enable", "disable", "sensor_platform", "tick_s", "watch_area", "protected",
              "thresholds", "budget", "cooldown", "material"} | LEGACY_KEYS
SECTIONS = ("thresholds", "budget", "cooldown", "material")
LEGACY_RADIUS_KM = 4.0  # the old agent's default radius


class ConfigError(ValueError):
    pass


def _lonlat(v: Any) -> bool:
    return (isinstance(v, (list, tuple)) and len(v) == 2
            and all(isinstance(x, (int, float)) and not isinstance(x, bool) and math.isfinite(x) for x in v)
            and -180 <= v[0] <= 180 and -90 <= v[1] <= 90)


def _num(v: Any, what: str) -> float:
    if isinstance(v, bool) or not isinstance(v, (int, float)) or not math.isfinite(v):
        raise ConfigError(f"{what} must be a number, not {v!r}")
    return float(v)


def load_defaults() -> dict[str, Any]:
    return tomllib.loads(DEFAULTS.read_text())


def known_plays() -> list[str]:
    from .plays import PLAYS

    return sorted(PLAYS)


def _areas(raw: Any) -> list[dict[str, Any]]:
    if not isinstance(raw, list):
        raise ConfigError("watch_area is a list of tables ([[playbook.watch_area]])")
    out = []
    for i, a in enumerate(raw):
        if not isinstance(a, dict) or set(a) - {"name", "center", "radius_m", "polygon"}:
            raise ConfigError(f"watch_area {i + 1}: a table of name, center and radius_m, or name and polygon")
        name = str(a.get("name") or f"watch area {i + 1}")
        if "polygon" in a:
            ring = a["polygon"]
            if "center" in a or "radius_m" in a or not isinstance(ring, list) or len(ring) < 3 or not all(_lonlat(p) for p in ring):
                raise ConfigError(f"watch_area {name!r}: polygon = [[lon, lat], ...] (three points or more), without center")
            out.append({"name": name, "polygon": [[float(p[0]), float(p[1])] for p in ring]})
        else:
            if not _lonlat(a.get("center")):
                raise ConfigError(f"watch_area {name!r}: center = [lon, lat]")
            r = _num(a.get("radius_m"), f"watch_area {name!r}: radius_m")
            if r <= 0:
                raise ConfigError(f"watch_area {name!r}: radius_m must be > 0")
            out.append({"name": name, "center": [float(a["center"][0]), float(a["center"][1])], "radius_m": r})
    return out


def _protected(raw: Any) -> list[dict[str, Any]]:
    if not isinstance(raw, list):
        raise ConfigError("protected is a list of tables ([[playbook.protected]])")
    out = []
    for i, p in enumerate(raw):
        if not isinstance(p, dict) or set(p) - {"name", "point"} or not _lonlat(p.get("point")):
            raise ConfigError(f"protected {i + 1}: a table of name and point = [lon, lat]")
        out.append({"name": str(p.get("name") or f"protected point {i + 1}"), "point": [float(p["point"][0]), float(p["point"][1])]})
    return out


def merge(table: dict[str, Any] | None) -> dict[str, Any]:
    """The defaults with a situation's `[playbook]` table on top, checked. Raises ConfigError."""
    table = dict(table or {})
    unknown = set(table) - TABLE_KEYS
    if unknown:
        raise ConfigError(f"unknown playbook keys {sorted(unknown)} (known: {sorted(TABLE_KEYS)})")
    cfg = load_defaults()
    names = set(known_plays())
    # the keys of before
    areas: list[dict[str, Any]] = []
    if "near" in table:
        if not _lonlat(table["near"]):
            raise ConfigError("near is [lon, lat]")
        rk = _num(table.get("radius_km", LEGACY_RADIUS_KM), "radius_km")
        if rk <= 0:
            raise ConfigError("radius_km must be > 0")
        areas.append({"name": "watch point", "center": [float(table["near"][0]), float(table["near"][1])], "radius_m": rk * 1000.0})
    elif "radius_km" in table:
        raise ConfigError("radius_km goes with near = [lon, lat]")
    if "every_s" in table and "tick_s" not in table:
        table["tick_s"] = table["every_s"]
    if "platform" in table and "sensor_platform" not in table:
        table["sensor_platform"] = table["platform"]
    cfg["tick_s"] = _num(table.get("tick_s", cfg["tick_s"]), "tick_s")
    if not 0.5 <= cfg["tick_s"] <= 60:
        raise ConfigError("tick_s must be from 0.5 to 60")
    sp = table.get("sensor_platform", cfg.get("sensor_platform", ""))
    if not isinstance(sp, str):
        raise ConfigError("sensor_platform is a platform id (a string)")
    cfg["sensor_platform"] = sp
    plays = list(cfg["plays"])
    if "plays" in table:
        if not isinstance(table["plays"], list) or not all(isinstance(p, str) for p in table["plays"]):
            raise ConfigError("plays is a list of play names")
        plays = list(table["plays"])
    for key in ("enable", "disable"):
        v = table.get(key, [])
        if not isinstance(v, list) or not all(isinstance(p, str) for p in v):
            raise ConfigError(f"{key} is a list of play names")
    plays = [p for p in plays if p not in table.get("disable", [])] + [p for p in table.get("enable", []) if p not in plays]
    bad = sorted(set(plays) - names)
    if bad:
        raise ConfigError(f"unknown plays {bad} (known: {sorted(names)})")
    cfg["plays"] = sorted(set(plays))
    cfg["watch_area"] = areas + _areas(table.get("watch_area", []))
    cfg["protected"] = _protected(table.get("protected", []))
    for sec in SECTIONS:
        over = table.get(sec, {})
        if not isinstance(over, dict):
            raise ConfigError(f"[playbook.{sec}] is a table")
        extra = set(over) - set(cfg[sec])
        if extra:
            raise ConfigError(f"[playbook.{sec}]: unknown keys {sorted(extra)} (known: {sorted(cfg[sec])})")
        for k, v in over.items():
            cfg[sec][k] = _num(v, f"{sec}.{k}")
        for k, v in cfg[sec].items():
            cfg[sec][k] = float(v) if not isinstance(v, bool) else v
    t, b = cfg["thresholds"], cfg["budget"]
    for k in ("existence_min", "affiliation_min", "classification_settled", "orbit_range_fraction", "recentre_fraction"):
        if not 0 < t[k] <= 1:
            raise ConfigError(f"thresholds.{k} must be in (0, 1]")
    if b["previews_per_tick"] < 1 or b["own_pending_max"] < 0 or b["human_burst"] < 1:
        raise ConfigError("budget: previews_per_tick and human_burst at least 1, own_pending_max at least 0")
    return cfg


def canonical(cfg: dict[str, Any]) -> str:
    return json.dumps(cfg, sort_keys=True, separators=(",", ":"), ensure_ascii=True)


def config_hash(cfg: dict[str, Any]) -> str:
    return hashlib.sha256(canonical(cfg).encode()).hexdigest()[:12]


@dataclass(frozen=True)
class Config:
    """The merged config, its hash and its label (`<name>#<hash>`)."""

    data: dict[str, Any]
    name: str
    hash: str

    @property
    def label(self) -> str:
        return f"{self.name}#{self.hash}"

    @property
    def client_version(self) -> str:
        """What the engine sends as the MCP client's version at connect (the bridge logs it in
        `agent.action session.connected`)."""
        return f"playbook-{ENGINE_VERSION} config {self.label}"

    def __getitem__(self, k: str) -> Any:
        return self.data[k]

    @property
    def t(self) -> dict[str, float]:
        return self.data["thresholds"]

    @property
    def budget(self) -> dict[str, float]:
        return self.data["budget"]

    @property
    def cooldown(self) -> dict[str, float]:
        return self.data["cooldown"]

    @property
    def material(self) -> dict[str, float]:
        return self.data["material"]

    def enabled(self, play: str) -> bool:
        return play in self.data["plays"]


def make(table: dict[str, Any] | None, name: str = "defaults") -> Config:
    data = merge(table)
    return Config(data=data, name=name, hash=config_hash(data))


def load_file(path: Path, name: str | None = None) -> Config:
    """A config file: JSON or TOML, either the `[playbook]` table itself or a document holding
    one under `playbook`."""
    text = path.read_text()
    try:
        raw = json.loads(text) if path.suffix == ".json" else tomllib.loads(text)
    except (ValueError, tomllib.TOMLDecodeError) as e:
        raise ConfigError(f"{path}: {e}") from e
    if not isinstance(raw, dict):
        raise ConfigError(f"{path}: not a table")
    label = name or str(raw.pop("name", "")) or path.stem
    table = raw.get("playbook", raw) if isinstance(raw.get("playbook"), dict) else raw
    table = {k: v for k, v in table.items() if k != "name"}
    return make(copy.deepcopy(table), label)
