"""The settings a record's run was briefed and graded under (docs/SCENARIO-SETTINGS.md section 3):
the watch's values (the bar, each domain's level, sensors and grace, the approach horizon, the
speeds), the scenario's circle, and for a run before decision rules 33 the scoring-only rules the
design retires. Scoring reads each record's own values through `of_record`, never a constant.
Since stage S6 the labels, the faults, the metrics, the report, the planners and the oracles read
it (each record's own values, `of_record`; a running agent's, `of_answer`).

- **v0** (`v0(world)`): every run before rules 33 was briefed and graded by one set of values,
  frozen in `sim/policy-v0-watch.toml` and pinned by its sha256 (`V0_SHA256`: reading an edited
  file raises). Its `[watch]` is in the design's schema; `[v0]` holds the low-reserve fault need's
  35 % and the 20 % battery-now reserve (eval/headless/v2/labels.py); `[area.<world>]` each v3
  world's place name, centre and radius; `[mission_sha256.<world>]` the v0 mission text's sha256
  now (`current`) and before bd murmur-isxg added the circle's centre (`before_isxg`).
- **A logged `[watch]`** (`from_policy`): the bridge logs its whole policy in `run.started` (from
  rules 33, with `[watch]` and `loosened`, and the files it was built from, `policy_layers`). A
  saved record holds no `run.started` (the batch drops `audit_text` before it writes the record),
  so the v3 recorder (eval/headless/episode.py `v2_part`) copies what it needs into the record as
  `settings` (`record_settings`) when the run logged a `[watch]`, and nothing under older rules (a
  record of theirs is as it was). `of_record` reads `rec["settings"]` first, then the record's
  audit log when it still holds it (`audit_text`, the record as the episode builds it) or the one
  kept beside it (`audit`: batch.py's `keep_audit` writes `<name>.audit.jsonl`), else v0. No run
  before rules 33 became the default (stage S7) logged a `[watch]` outside the staged rules, so
  every result directory before it is graded under v0.
- **A run's settings before it starts** (`for_run`): what a run of a scenario's policy file under
  a rules version will decide under, for its brief (eval/roe_card.py) and its stated truth: v0's
  under rules below 33 (what such a record is graded under), else the policy file's effective
  `[watch]` (eval/policy_file.py `show`, the decision layer's own merge). A running agent reads
  the same from MCP `get_policy`'s answer (`of_answer`: the logged policy's `[watch]`, else v0's).

**Who reads what.** Scoring, the planners and the oracles read a domain's criteria through
`Settings.rule` (its grace, its sensors other than AIS, whether a true AIS counts, its level), the
bar, the horizon, the speeds and the circle from the settings, and the reserve through
`Settings.reserve_now` (v0's battery-now 20 %, or a logged run's `reserve_pct`); the low-reserve
fault need of a logged run (faults.py, version 2) reads `reserve_pct` and `task_duration_s` too.
`sha256` names a settings set in a record's stated truth and a report's coverage line;
`values_sha256` the same without the circle (each world has its own circle).
"""

from __future__ import annotations

import hashlib
import json
import tomllib
from dataclasses import dataclass, replace
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
    #: a logged run's `[rules] reserve_pct` and `task_duration_s` (the engine's reserve: endurance
    #: left after a task of that length) and its engagement's identity bar and level; None for v0
    reserve_pct: float | None = None
    task_duration_s: float | None = None
    engage_bar: float | None = None
    engage_level: str | None = None

    @property
    def is_v0(self) -> bool:
        return self.v0 is not None

    def rule(self, domain: Any) -> Rule:
        """A domain's criteria as the labels read them (`Rule`): its grace, its sensors other than
        AIS, whether a contact's own true AIS counts, its level. A domain the settings do not name
        (or none) is held to the surface's, as the mission held it."""
        c = self.domain(str(domain or "surface")) or self.domain("surface")
        if c is None:
            raise ValueError("the settings name no surface domain")
        return Rule(c.grace_s, tuple(x for x in c.sensors if x != "ais"), "ais" in c.sensors, c.level)

    def reserve_now(self) -> float:
        """The battery a vehicle is held to now (harm (a), the low-reserve need's unmet line, the
        planners' reserve): v0's battery-now reserve, or a logged run's `reserve_pct`."""
        if self.v0 is not None:
            return self.v0.reserve_now_pct
        if self.reserve_pct is None:
            raise ValueError("these settings carry no reserve_pct (a logged run's [rules])")
        return self.reserve_pct

    def values(self, area: bool = True) -> dict[str, Any]:
        """What the settings are, without where they came from (`source`): what `sha256` hashes.
        Without `area`, nothing a world has of its own either: the circle, and v0's mission text
        sha256s (its text names the world's place)."""
        out = self.as_dict()
        out.pop("source", None)
        if not area:
            out["watch"].pop("area", None)
            for k in ("mission_sha256", "mission_sha256_before_isxg"):
                out.get("v0", {}).pop(k, None)
        return out

    def sha256(self) -> str:
        """The sha256 of the settings' values (canonical JSON): a record's stated
        `settings_sha256`, a report's coverage line."""
        return _sha(self.values())

    def values_sha256(self) -> str:
        """The sha256 of the values without what each world has of its own (`values`)."""
        return _sha(self.values(area=False))

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
        for k in ("reserve_pct", "task_duration_s", "engage_bar", "engage_level"):
            if getattr(self, k) is not None:
                out[k] = getattr(self, k)
        return out


@dataclass(frozen=True)
class Rule:
    """A domain's criteria as the labels read them (`Settings.rule`)."""

    grace_s: float
    #: the sensor types whose support counts (the picture's `supported_by`), AIS apart
    sensors: tuple[str, ...]
    #: AIS counts too when the contact truly broadcasts its own
    ais_counts: bool
    #: "identify": the shown kind must be the true kind; "recognise": its true family will do
    level: str


def _sha(v: Any) -> str:
    return hashlib.sha256(json.dumps(v, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def from_dict(d: dict[str, Any], world: str | None = None) -> Settings:
    """`Settings.as_dict`'s inverse (a report's scored row keeps a logged run's settings so). v0's
    are v0's for `world` (`v0`), whatever the dict says beside its source."""
    if d.get("source") == "v0":
        if world is None:
            raise ValueError("v0's settings need their world")
        return v0(world)
    lo = d.get("loosened") or {}
    s = watch_of(d["watch"], str(d.get("source") or "logged"), loosened=lo)
    return _rules_part(s, d.get("reserve_pct"), d.get("task_duration_s"),
                       {"min_identity_confidence": d.get("engage_bar"), "min_identity_level": d.get("engage_level")})


def describe(s: Settings) -> str:
    """One line of a report's coverage: where the settings came from and every value scoring reads."""
    doms = "; ".join(f"{d} {c.level} by {'/'.join(c.sensors)} in {c.grace_s:g} s" for d, c in s.domains)
    nv = ", ".join(f"{d} {v:g} m/s" for d, v in s.no_velocity_mps) or "none"
    if s.v0 is not None:
        src = "v0 (sim/policy-v0-watch.toml: no [watch] logged, a run before decision rules 33)"
        reserve = (f"reserve {s.v0.reserve_now_pct:g} % of battery now, the low-reserve need opening at "
                   f"{s.v0.low_reserve_open_pct:g} %")
    else:
        src = f"logged by the run ({s.source})"
        reserve = (f"reserve {s.reserve_pct:g} % left after a {s.task_duration_s:g} s task"
                   if s.reserve_pct is not None and s.task_duration_s is not None else "reserve not logged")
    area = (f"{s.area.radius_m / 1000.0:g} km around {s.area.name} [{s.area.center[0]:.4f}, {s.area.center[1]:.4f}]"
            if s.area is not None else "no circle")
    out = (f"{src}, sha256 {s.sha256()[:12]}: {area}; bar {s.bar:g}; {doms}; horizon {s.approach_horizon_s:g} s; "
           f"an identify order {s.identify_on_station_s:g} s on station; stale after {s.stale_s:g} s; "
           f"still under {s.still_mps:g} m/s, no velocity taken at {nv}; look {s.look_m:g} m; {reserve}")
    if s.loosened:
        out += "; loosened: " + "; ".join(f"{k} ({why})" for k, why in s.loosened)
    return out + "."


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


def _rules_part(s: Settings, reserve: Any, task_s: Any, engage: Any) -> Settings:
    """`s` with a logged run's reserve and engagement bar (None where the run logged none)."""
    engage = engage if isinstance(engage, dict) else {}
    bar, level = engage.get("min_identity_confidence"), engage.get("min_identity_level")
    return replace(
        s, reserve_pct=None if reserve is None else _num(reserve, "rules.reserve_pct"),
        task_duration_s=None if task_s is None else _num(task_s, "rules.task_duration_s"),
        engage_bar=None if bar is None else _num(bar, "rules.engage.min_identity_confidence"),
        engage_level=None if level is None else str(level))


def from_policy(policy: dict[str, Any], source: str = "logged") -> Settings:
    """A logged (merged) policy's `[watch]`, `loosened`, reserve and engagement bar."""
    lo = policy.get("loosened") or {}
    if not isinstance(lo, dict):
        raise ValueError(f"loosened: a table of reasons by key, not {lo!r}")
    rules = policy.get("rules") or {}
    return _rules_part(watch_of(policy["watch"], source, loosened=lo), rules.get("reserve_pct"),
                       rules.get("task_duration_s"), rules.get("engage"))


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


@lru_cache(maxsize=None)
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


def record_settings(started: dict[str, Any] | None) -> dict[str, Any] | None:
    """What a record keeps of the settings its run logged (`rec["settings"]`), from the run's
    `run.started` event: the `[watch]`, the reserve (`reserve_pct`, `task_duration_s`), the
    engagement's identity bar and level, what was loosened, the files the policy was built from
    (`policy_layers`) and the rules version. None when the run logged no `[watch]` (rules below
    33): such a record carries nothing new and is graded under v0."""
    if not isinstance(started, dict):
        return None
    p = started.get("policy")
    if not isinstance(p, dict) or p.get("watch") is None:
        return None
    rules = p.get("rules") or {}
    engage = rules.get("engage") or {}
    return {
        "rules_version": started.get("rules_version"),
        "watch": p["watch"],
        "reserve_pct": rules.get("reserve_pct"),
        "task_duration_s": rules.get("task_duration_s"),
        "engage": {"min_identity_confidence": engage.get("min_identity_confidence"),
                   "min_identity_level": engage.get("min_identity_level")},
        "loosened": dict(p.get("loosened") or {}),
        "policy_layers": list(started.get("policy_layers") or []),
    }


def of_record(rec: dict[str, Any], audit: Path | None = None) -> Settings:
    """The settings a record's run was graded under: the settings the record kept
    (`rec["settings"]`, `record_settings`: a saved record holds no `run.started`), else its logged
    `[watch]` when the run logged one, else v0's for the record's world."""
    from .labels import world_of  # here, not at the top: the labels will read this module (S6)

    kept = rec.get("settings")
    if isinstance(kept, dict) and kept.get("watch") is not None:
        lo = kept.get("loosened") or {}
        if not isinstance(lo, dict):
            raise ValueError(f"settings.loosened: a table of reasons by key, not {lo!r}")
        return _rules_part(watch_of(kept["watch"], "logged", loosened=lo), kept.get("reserve_pct"),
                           kept.get("task_duration_s"), kept.get("engage"))
    p = logged_policy(rec, audit)
    if p is not None and p.get("watch") is not None:
        return from_policy(p)
    return v0(world_of(rec))


#: the decision rules from which a run decides under its policy's own `[watch]` (crates/murmur-decide
#: rules.rs `SCENARIO_SETTINGS_SINCE`); under older rules it runs under what v0 holds
WATCH_SINCE = 33


def of_answer(answer: Any, world: str) -> Settings:
    """The settings a running agent reads from MCP `get_policy`'s answer: its logged policy's
    `[watch]` (rules 33 on; the answer's top-level `watch` is the same), else v0's for `world`, what
    such a run is graded under (the answer's `watch` then holds the legacy values and the circle)."""
    pol = answer.get("policy") if isinstance(answer, dict) else None
    if isinstance(pol, dict) and pol.get("watch") is not None:
        return from_policy(pol, "get_policy")
    return v0(world)


def for_run(policy: str | Path | None, world: str,
            rules: int | None = None) -> tuple[Settings, dict[str, Any] | None, int]:
    """What a run of `world` under the policy file `policy` and decision rules `rules` (the run's,
    eval/policy_file.py `rules_in_force`, when None) decides under, before it starts: its settings,
    its merged policy as `murmur-policy show` prints it, and the rules. Under rules below 33, v0's
    settings and no policy (a v3 world's record of such a run is graded under v0). A policy file the
    decision layer refuses raises eval/policy_file.py's `PolicyError`."""
    from eval.policy_file import rules_in_force, show

    r = rules_in_force() if rules is None else int(rules)
    if r < WATCH_SINCE or policy is None:
        return v0(world), None, r
    shown = show(policy, r)
    return from_policy(shown["policy"], "policy"), shown["policy"], r
