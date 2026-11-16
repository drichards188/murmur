"""The settings a record's run was briefed and graded under (docs/SCENARIO-SETTINGS.md section 3):
the watch's values (the bar, each domain's level, sensors and grace, the approach horizon, the
speeds), the scenario's circle, and for a run before decision rules 33 the scoring-only rules the
design retires. Scoring reads each record's own values through `of_record`, never a constant.
Nothing reads this module yet (stage S6 moves the labels, metrics and report to it).

- **v0** (`v0(world)`): every run before rules 33 was briefed and graded by one set of values,
  frozen in `sim/policy-v0-watch.toml` and pinned by its sha256 (`V0_SHA256`: reading an edited
  file raises). Its `[watch]` is in the design's schema; `[v0]` holds the low-reserve fault need's
  35 % and the 20 % battery-now reserve (eval/headless/v2/labels.py); `[area.<world>]` each v3
  world's place name, centre and radius; `[mission_sha256.<world>]` the v0 mission text's sha256
  now (`current`) and before bd murmur-isxg added the circle's centre (`before_isxg`).
- **A logged `[watch]`** (`from_policy`): the bridge logs its whole policy in `run.started` (from
  S2 the merged layers, with `[watch]` and `loosened`). `of_record` reads it from the record's
  audit log when the record still holds it (`audit_text`, the record as the episode builds it), or
  from the audit log kept beside the record (`audit`: batch.py's `keep_audit` writes
  `<name>.audit.jsonl`). A saved record holds neither on its own: the batch drops `audit_text`
  before it writes the record, and the record keeps only the policy file's name
  (`v3.condition.policy`) and the log's digest (`audit`). No run has logged a `[watch]` yet, so
  every existing record is graded under v0.
"""

from __future__ import annotations

import hashlib
import json
import tomllib
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path
from typing import Any

from eval import REPO

V0_PATH = REPO / "sim" / "policy-v0-watch.toml"
#: the frozen file's sha256: `v0` refuses a file that is not this one
V0_SHA256 = "b09c6492674271dd2d8f30686c05cd9ff2b645453068dffae8456196d87a0791"
#: the `[watch]` keys, each required (a key added to the schema is read here on purpose, never
#: passed over); `area` is the scenario's and may be absent
WATCH_KEYS = ("bar", "neutral_after_bar", "approach_horizon_s", "identify_on_station_s", "stale_s", "still_mps",
              "look_m", "no_velocity_mps", "domain")
DOMAIN_KEYS = ("level", "sensors", "grace_s")
AREA_KEYS = ("name", "center", "radius_m")
#: the domains, in the mission's order
DOMAINS = ("surface", "air", "ground", "subsurface")
LEVELS = ("detect", "recognise", "identify")


@dataclass(frozen=True)
class Domain:
    """What identifies a contact of one domain, and by when: its kind (`identify`) or its class
    (`recognise`) or better, supported by one of `sensors`, within `grace_s` of its coming inside
    the circle."""

    level: str
    sensors: tuple[str, ...]
    grace_s: float


@dataclass(frozen=True)
class Area:
    """The scenario's circle: what it is called, its centre ([lon, lat]) and radius."""

    name: str
    center: tuple[float, float]
    radius_m: float


@dataclass(frozen=True)
class V0:
    """The scoring-only rules of v0, and the v0 mission text's sha256 for the world."""

    low_reserve_open_pct: float
    reserve_now_pct: float
    mission_sha256: str
    mission_sha256_before_isxg: str


@dataclass(frozen=True)
class Settings:
    """A run's watch settings: the `[watch]` values, the circle, what was loosened (each key and
    its reason), and `v0` when they are v0's. `source` says where they came from: "v0" or
    "logged"."""

    bar: float
    neutral_after_bar: bool
    approach_horizon_s: float
    identify_on_station_s: float
    stale_s: float
    still_mps: float
    look_m: float
    no_velocity_mps: tuple[tuple[str, float], ...]
    domains: tuple[tuple[str, Domain], ...]
    area: Area | None
    source: str
    v0: V0 | None = None
    loosened: tuple[tuple[str, str], ...] = ()

    @property
    def is_v0(self) -> bool:
        return self.v0 is not None

    def domain(self, name: str) -> Domain | None:
        """A domain's criteria, None for a domain the settings do not name."""
        return dict(self.domains).get(str(name))

    def no_velocity(self, domain: str) -> float | None:
        """The speed a contact of `domain` shown without velocity is taken to come straight in at
        (None: judged where it is)."""
        return dict(self.no_velocity_mps).get(str(domain))

    def watch(self) -> dict[str, Any]:
        """The `[watch]` table in the design's schema (its circle as `area`), as MCP `get_policy`
        serves it: what agents/roe.py reads."""
        out: dict[str, Any] = {
            "bar": self.bar, "neutral_after_bar": self.neutral_after_bar,
            "approach_horizon_s": self.approach_horizon_s, "identify_on_station_s": self.identify_on_station_s,
            "stale_s": self.stale_s, "still_mps": self.still_mps, "look_m": self.look_m,
            "no_velocity_mps": dict(self.no_velocity_mps),
            "domain": {d: {"level": c.level, "sensors": list(c.sensors), "grace_s": c.grace_s}
                       for d, c in self.domains},
        }
        if self.area is not None:
            out["area"] = {"name": self.area.name, "center": list(self.area.center), "radius_m": self.area.radius_m}
        return out

    def as_dict(self) -> dict[str, Any]:
        """Everything, for a record or a report's coverage line."""
        out = {"source": self.source, "watch": self.watch(), "loosened": dict(self.loosened)}
        if self.v0 is not None:
            out["v0"] = {"low_reserve_open_pct": self.v0.low_reserve_open_pct,
                         "reserve_now_pct": self.v0.reserve_now_pct, "mission_sha256": self.v0.mission_sha256,
                         "mission_sha256_before_isxg": self.v0.mission_sha256_before_isxg}
        return out


def _num(v: Any, what: str) -> float:
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        raise ValueError(f"{what}: a number, not {v!r}")
    return float(v)


def _only(t: Any, keys: tuple[str, ...], what: str, optional: tuple[str, ...] = ()) -> dict[str, Any]:
    if not isinstance(t, dict):
        raise ValueError(f"{what}: a table, not {t!r}")
    missing = [k for k in keys if k not in t]
    unknown = sorted(set(t) - set(keys) - set(optional))
    if missing or unknown:
        raise ValueError(f"{what}: missing {missing}, unknown {unknown}")
    return t


def area_of(t: Any, what: str = "watch.area") -> Area:
    a = _only(t, AREA_KEYS, what)
    c = a["center"]
    if not (isinstance(c, list) and len(c) == 2):
        raise ValueError(f"{what}.center: [lon, lat], not {c!r}")
    return Area(str(a["name"]), (_num(c[0], f"{what}.center"), _num(c[1], f"{what}.center")),
                _num(a["radius_m"], f"{what}.radius_m"))


def watch_of(w: Any, source: str, area: Area | None = None, v0: V0 | None = None,
             loosened: dict[str, str] | None = None) -> Settings:
    """Settings from a `[watch]` table (its own `area` unless one is given)."""
    w = _only(w, WATCH_KEYS, "watch", optional=("area",))
    doms = _only(w["domain"], DOMAINS, "watch.domain")
    domains = []
    for d in DOMAINS:
        t = _only(doms[d], DOMAIN_KEYS, f"watch.domain.{d}")
        if t["level"] not in LEVELS:
            raise ValueError(f"watch.domain.{d}.level: one of {LEVELS}, not {t['level']!r}")
        if not isinstance(t["sensors"], list) or not t["sensors"]:
            raise ValueError(f"watch.domain.{d}.sensors: a list of sensor types, not {t['sensors']!r}")
        domains.append((d, Domain(str(t["level"]), tuple(str(s) for s in t["sensors"]),
                                  _num(t["grace_s"], f"watch.domain.{d}.grace_s"))))
    nv = w["no_velocity_mps"]
    if not isinstance(nv, dict) or any(k not in DOMAINS for k in nv):
        raise ValueError(f"watch.no_velocity_mps: speeds by domain, not {nv!r}")
    if not isinstance(w["neutral_after_bar"], bool):
        raise ValueError(f"watch.neutral_after_bar: true or false, not {w['neutral_after_bar']!r}")
    if area is None and w.get("area") is not None:
        area = area_of(w["area"])
    return Settings(
        bar=_num(w["bar"], "watch.bar"), neutral_after_bar=w["neutral_after_bar"],
        approach_horizon_s=_num(w["approach_horizon_s"], "watch.approach_horizon_s"),
        identify_on_station_s=_num(w["identify_on_station_s"], "watch.identify_on_station_s"),
        stale_s=_num(w["stale_s"], "watch.stale_s"), still_mps=_num(w["still_mps"], "watch.still_mps"),
        look_m=_num(w["look_m"], "watch.look_m"),
        no_velocity_mps=tuple((d, _num(nv[d], f"watch.no_velocity_mps.{d}")) for d in DOMAINS if d in nv),
        domains=tuple(domains), area=area, source=source, v0=v0,
        loosened=tuple(sorted((str(k), str(v)) for k, v in (loosened or {}).items())),
    )


def from_policy(policy: dict[str, Any]) -> Settings:
    """A logged (merged) policy's `[watch]` and `loosened`."""
    lo = policy.get("loosened") or {}
    if not isinstance(lo, dict):
        raise ValueError(f"loosened: a table of reasons by key, not {lo!r}")
    return watch_of(policy["watch"], "logged", loosened=lo)


@lru_cache(maxsize=1)
def frozen() -> dict[str, Any]:
    """The frozen v0 file, read once; an edited file raises."""
    raw = V0_PATH.read_bytes()
    got = hashlib.sha256(raw).hexdigest()
    if got != V0_SHA256:
        raise ValueError(f"{V0_PATH} is frozen (sha256 {V0_SHA256}) but reads {got}: v0's values never change")
    return tomllib.loads(raw.decode())


def v0_worlds() -> tuple[str, ...]:
    return tuple(sorted(frozen()["area"]))


def v0(world: str) -> Settings:
    """The values every run before rules 33 in `world` (a v3 world) was briefed and graded by."""
    f = frozen()
    if world not in f["area"]:
        raise ValueError(f"no v0 circle for world {world!r} (v0 has {', '.join(v0_worlds())})")
    x, m = f["v0"], f["mission_sha256"][world]
    extra = V0(_num(x["low_reserve_open_pct"], "v0.low_reserve_open_pct"),
               _num(x["reserve_now_pct"], "v0.reserve_now_pct"), str(m["current"]), str(m["before_isxg"]))
    return watch_of(f["watch"], "v0", area=area_of(f["area"][world], f"area.{world}"), v0=extra)


def run_started(rec: dict[str, Any], audit: Path | None = None) -> dict[str, Any] | None:
    """The run's `run.started` event: from the audit log kept beside the record (`audit`), else
    from the record's own `audit_text`; None when neither holds one."""
    text = Path(audit).read_text() if audit is not None else rec.get("audit_text")
    if not isinstance(text, str):
        return None
    for line in text.splitlines():
        if not line.strip():
            continue
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if isinstance(e, dict) and e.get("type") == "run.started":
            return e
    return None


def logged_policy(rec: dict[str, Any], audit: Path | None = None) -> dict[str, Any] | None:
    """The policy the run logged whole in `run.started`, or None."""
    e = run_started(rec, audit)
    p = (e or {}).get("policy")
    return p if isinstance(p, dict) else None


def of_record(rec: dict[str, Any], audit: Path | None = None) -> Settings:
    """The settings a record's run was graded under: its logged `[watch]` when the run logged one,
    else v0's for the record's world."""
    from .labels import world_of  # here, not at the top: the labels will read this module (S6)

    p = logged_policy(rec, audit)
    if p is not None and p.get("watch") is not None:
        return from_policy(p)
    return v0(world_of(rec))
