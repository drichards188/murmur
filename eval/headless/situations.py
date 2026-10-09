"""A situation as an episode: what the simulator runs (the process spec and its environment)
and what the driver does (orders, the operator, the AI, scripted faults), from a situation file
(Harbor watch) or a generated one (families.py).

The fleet manifest and the stand-ins' configs are `sim/swarm.py`'s own (`plan`,
`lost_link_params`, as `scripts/up.sh` and eval/stack.py use them); the bridge's environment is
the one `eval/situation_run.py` gives a situation's stack (its roster, agents, run id, policy,
scenario clock, simulation token, no resume).
"""

from __future__ import annotations

import hashlib
import os
import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from eval.situation import (
    AGENTS,
    ENGAGE_OPERATORS,
    OPERATORS,
    Order,
    Situation,
    load_situation,
)

from . import HERE, REPO

#: the Golden Gate Bridge, mid-span (Harbor watch's protected point)
GATE = (-122.4783, 37.8199)
#: the terrain grid baked from tidewater (sim/headless/terrain/README.md)
TERRAIN_GRID = REPO / "sim" / "headless" / "terrain" / "golden-gate.grid.gz"
#: the station's ports in a headless run: identifiers only, nothing is bound
HTTP_PORT = 9565
VEHICLE_BASE = 29000
BRIDGE_BASE = 30000


@dataclass
class Calibration:
    """Process start offsets on the situation's clock, the only simulator parameters set from
    full-stack measurements, and only from calibration seeds (CRITERIA.md). Defaults before
    calibration: what scripts/up.sh's order of starts suggests."""

    contacts_start_s: float = 3.0
    standins_start_s: float = 3.0
    ai_first_tick_s: float = 2.0
    source: str = "defaults (not calibrated)"

    @classmethod
    def load(cls, path: Path | None = None) -> "Calibration":
        path = path or HERE / "calibration.toml"
        if not path.exists():
            return cls()
        raw = tomllib.loads(path.read_text())
        return cls(**{k: raw[k] for k in ("contacts_start_s", "standins_start_s", "ai_first_tick_s", "source") if k in raw})

    def as_dict(self) -> dict[str, Any]:
        return {"contacts_start_s": self.contacts_start_s, "standins_start_s": self.standins_start_s,
                "ai_first_tick_s": self.ai_first_tick_s, "source": self.source}


@dataclass
class EpisodeSpec:
    """One situation as the driver runs it."""

    name: str
    fleet: Path
    contacts: Path | None
    policy: Path
    at: str
    duration_s: float
    ai_start_s: float
    ready_timeout_s: float
    orders: list[Order]
    playbook: dict[str, Any]
    autoplay: dict[str, Any]
    seed: int  # the situation's `seed`: the operator's draws
    events: list[dict[str, Any]] = field(default_factory=list)
    contacts_seed: int | None = None
    family: str | None = None
    split: str | None = None
    situation_seed: int | None = None
    battery_pct: dict[int, float] = field(default_factory=dict)
    # scripted faults and proposers: [{"at_s", "kind", ...}] and dynamic ones (episode.py)
    script: list[dict[str, Any]] = field(default_factory=list)
    # truth the generator states (families.py); Harbor watch's comes from labels.py's rule
    truth: dict[str, Any] = field(default_factory=dict)
    path: Path | None = None
    config_name: str | None = None


def from_situation(path_or_name: str | Path) -> EpisodeSpec:
    """A situation file (sim/situations/), as its runner would run it on autoplay."""
    sit: Situation = load_situation(path_or_name)
    if sit.todo:
        raise ValueError(f"{sit.path}: a stub, not runnable")
    events: list[dict[str, Any]] = []
    for f in sit.events:
        events += list(tomllib.loads(Path(f).read_text()).get("event", []))
    assert sit.fleet is not None and sit.policy is not None
    return EpisodeSpec(
        name=sit.name, fleet=sit.fleet, contacts=sit.contacts, policy=sit.policy, at=sit.at,
        duration_s=sit.duration_s, ai_start_s=sit.ai_start_s, ready_timeout_s=sit.ready_timeout_s,
        orders=list(sit.orders), playbook=dict(sit.playbook), autoplay=dict(sit.autoplay), seed=sit.seed,
        events=events, path=sit.path, config_name=sit.name,
    )


def _swarm() -> Any:
    from eval.stack import _swarm as swarm

    return swarm()


def at_unix(at: str) -> float:
    import datetime as dt

    return dt.datetime.fromisoformat(at.replace("Z", "+00:00")).timestamp()


def fleet_plan(fleet: Path) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """(manifest entries, stand-in configs) as `sim/swarm.py` writes them for this fleet."""
    swarm = _swarm()
    platforms = swarm.load_platforms()
    vehicles = swarm.plan(swarm.load_fleet(str(fleet)), platforms)
    for i, v in enumerate(vehicles):
        v["uid"] = i + 1
        v["sysid"] = i % 250 + 1
        v["port"] = VEHICLE_BASE + i
    standins = []
    for v in vehicles:
        if v["sim"] != "standin":
            raise ValueError(f"{fleet}: {v['callsign']} is SITL: the simulator runs stand-ins only")
        spec = platforms[v["platform"]]
        standins.append({**v, "spec": spec,
                         "params": swarm.lost_link_params(v["platform"], spec,
                                                          swarm.STANDIN_FIRMWARE.get(spec["behavior"]))})
    return vehicles, standins


def space_cache_file() -> Path:
    """Where SATCOM looks are kept between episodes (crates/murmur-headless space_cache.rs):
    one file per set of element sets and link profiles, so a new fetch or new profiles start a
    new one."""
    links = Path(os.environ.get("MURMUR_LINKS") or REPO / "sim" / "links.toml")
    h = hashlib.sha256(links.read_bytes())
    cache = REPO / "sim" / "space" / "cache"
    if cache.is_dir():
        for f in sorted(cache.iterdir()):
            h.update(f"{f.name}:{f.stat().st_size}".encode())
    return REPO / "logs" / "headless" / "space-cache" / f"{h.hexdigest()[:16]}.jsonl"


def process_spec(e: EpisodeSpec, seed: int, workdir: Path, calib: Calibration,
                 terrain: bool = True, space: bool = True, anchors: bool = True,
                 contacts_path: Path | None = None) -> tuple[dict[str, Any], dict[str, str]]:
    """The `murmur-headless` spec and environment for one episode."""
    vehicles, standins = fleet_plan(e.fleet)
    contacts = contacts_path or e.contacts
    spec: dict[str, Any] = {
        "seed": int(seed),
        "epoch": at_unix(e.at),
        "root": str(REPO),
        "workdir": str(workdir),
        "http_port": HTTP_PORT,
        "vehicle_base": VEHICLE_BASE,
        "bridge_base": BRIDGE_BASE,
        "fleet": vehicles,
        "standins": standins,
        "links": str(Path(os.environ.get("MURMUR_LINKS") or REPO / "sim" / "links.toml")),
        "events": e.events,
        "terrain_grid": str(TERRAIN_GRID) if terrain and TERRAIN_GRID.exists() else None,
        "space": space,
        "space_cache": str(space_cache_file()) if space else None,
        "contacts":({"scenario": str(contacts), "start_s": calib.contacts_start_s, "seed": e.contacts_seed}
                     if contacts else None),
        "standins_start_s": calib.standins_start_s,
        "battery_pct": {str(k): v for k, v in e.battery_pct.items()},
    }
    run_id = f"headless-{e.name}-s{seed}"
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": os.environ.get("HOME", "/tmp"),
        "MURMUR_AUTH": "dev",
        "MURMUR_OPERATORS": ",".join(OPERATORS),
        "MURMUR_ENGAGE_OPERATORS": ",".join(ENGAGE_OPERATORS),
        "MURMUR_AGENTS": ",".join(AGENTS),
        "MURMUR_RUN_ID": run_id,
        "MURMUR_RESUME": "0",
        "MURMUR_POLICY": str(e.policy),
        "MURMUR_AT": e.at,
        "MURMUR_RATE": "1",
        "MURMUR_SIM_TOKEN": "headless",
        "MURMUR_HTTP_PORT": str(HTTP_PORT),
        # the space segment's loader runs on a thread on real time (crate::space::router): the
        # simulator gives it no element sets, so it loads nothing (docs/HEADLESS.md, the
        # determinism audit); the emulator's SATCOM uses them, loaded once at the start
        "MURMUR_ROOT": str(workdir / "no-space-root"),
        # the bridge serves the emulator's view (/api/links) as with an emulator; nothing polls
        "MURMUR_LINKEMU_URL": "http://headless.invalid:1",
    }
    if not anchors:
        env["MURMUR_ANCHORS"] = "0"
    return spec, env
