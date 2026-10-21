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
    brief_text,
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
    full-stack measurements, and only from calibration seeds (CRITERIA.md): the contacts
    process's and the stand-in engine's starts, the bridge's (its decision layer and loops, the
    first anchor), each a median; the playbook's first tick after `ai_start_s`, a range: each
    episode draws its own from the seed (`ai_first_tick`), as the full stack's agent process
    starts and connects in a time that varies run to run. Defaults before calibration: what
    scripts/up.sh's order of starts suggests."""

    contacts_start_s: float = 3.0
    standins_start_s: float = 3.0
    bridge_start_s: float = 0.0
    ai_first_tick_lo_s: float = 2.0
    ai_first_tick_hi_s: float = 2.0
    source: str = "defaults (not calibrated)"

    KEYS = ("contacts_start_s", "standins_start_s", "bridge_start_s", "ai_first_tick_lo_s", "ai_first_tick_hi_s",
            "source")

    @classmethod
    def load(cls, path: Path | None = None) -> "Calibration":
        path = path or HERE / "calibration.toml"
        if not path.exists():
            return cls()
        raw = tomllib.loads(path.read_text())
        return cls(**{k: raw[k] for k in cls.KEYS if k in raw})

    def ai_first_tick(self, seed: int) -> float:
        """The playbook's first tick after `ai_start_s` for an episode seed: uniform over the
        calibrated range, from a generator of its own (nothing else draws from it)."""
        import random

        lo, hi = self.ai_first_tick_lo_s, self.ai_first_tick_hi_s
        return round(lo + (hi - lo) * random.Random(f"ai-first-tick:{int(seed)}").random(), 3)

    def as_dict(self) -> dict[str, Any]:
        return {k: getattr(self, k) for k in self.KEYS}


def emulator_seed(seed: int) -> int:
    """The link emulator's seed for an episode: the episode seed, so every episode of a batch
    meets its own draws (fix round 2: a Harbor watch batch had them all on the situation file's
    seed 1, the same draws every time). Anything compared with the full stack passes the same
    value to it as MURMUR_LINKEMU_SEED (eval/headless/fullstack.py `stack_env`); G1's copies set
    the situation's `seed` to the episode seed k, and the full stack runs them with k too."""
    return int(seed)


def links_file() -> Path:
    """The link profiles both the emulator and the bridge read (MURMUR_LINKS, else sim/links.toml)."""
    return Path(os.environ.get("MURMUR_LINKS") or REPO / "sim" / "links.toml")


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
    # what a model AI is told (eval/situation.py `brief_text`: the title and the operator's
    # brief, never the director's notes); a v1 family keeps its base situation's, a v2 one is
    # the mission (eval/headless/v2/mission.py) and nothing else
    brief: str = ""
    # the world it runs in: "v1" (Harbor watch's fleet, the default) or "v2" (scoring v2's base,
    # eval/SCORING-V2.md section 0: families.py `world="v2"`)
    world: str = "v1"
    # scoring v2's replicate r of the situation (SCORING-V2.md section 1), set by whoever runs
    # replicates; None: the episode seed tells them apart (`run_id`)
    replicate: int | None = None

    def operator_note(self, operator: str) -> str:
        """How the scripted operator signs its decisions' notes. v1 names the situation, as it
        always has (the golden episode holds it); v2 never does (SCORING-V2.md 4.4: the operator
        model's notes carry no situation name)."""
        if self.world == "v2":
            return f"scripted (autoplay): {operator} (not a person)"
        return f"scripted (situation {self.name}, autoplay): {operator} (not a person)"


def opaque_run_id(family: str, situation_seed: int | None, replicate: int | str | None) -> str:
    """Scoring v2's run id (SCORING-V2.md 4.4): `ep-` and 12 hex characters of a SHA-256 over
    (family, situation seed, replicate), so nothing an agent reads names the family or a seed.
    Domain-separated from the replicate seed's hash (same inputs, `run-id:` prefix)."""
    h = hashlib.sha256(f"run-id:{family}:{situation_seed}:{replicate}".encode()).hexdigest()
    return f"ep-{h[:12]}"


def run_id(e: EpisodeSpec, seed: int) -> str:
    """The bridge's run id (MURMUR_RUN_ID: the audit log's name, served to agents at connect).
    v1: `headless-<situation>-s<seed>`, unchanged (the golden episode's audit log holds it). v2:
    opaque (`opaque_run_id`) over the family (the situation's name for a situation file), the
    situation seed and the replicate; without a replicate number the episode seed stands in
    for it, so two replicates of one situation never share a run id."""
    if e.world != "v2":
        return f"headless-{e.name}-s{seed}"
    rep: int | str = e.replicate if e.replicate is not None else f"seed{int(seed)}"
    return opaque_run_id(e.family or e.name, e.situation_seed, rep)


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
        events=events, path=sit.path, config_name=sit.name, brief=brief_text(sit),
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
    links = links_file()
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
        "links": str(links_file()),
        "linkemu_seed": emulator_seed(seed),
        "events": e.events,
        "terrain_grid": str(TERRAIN_GRID) if terrain and TERRAIN_GRID.exists() else None,
        "space": space,
        "space_cache": str(space_cache_file()) if space else None,
        "contacts":({"scenario": str(contacts), "start_s": calib.contacts_start_s, "seed": e.contacts_seed}
                     if contacts else None),
        "standins_start_s": calib.standins_start_s,
        "bridge_start_s": calib.bridge_start_s,
        "battery_pct": {str(k): v for k, v in e.battery_pct.items()},
    }
    rid = run_id(e, seed)
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": os.environ.get("HOME", "/tmp"),
        "MURMUR_AUTH": "dev",
        "MURMUR_OPERATORS": ",".join(OPERATORS),
        "MURMUR_ENGAGE_OPERATORS": ",".join(ENGAGE_OPERATORS),
        "MURMUR_AGENTS": ",".join(AGENTS),
        "MURMUR_RUN_ID": rid,
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
        # the bridge's lost-link timers (estimate.rs) from the same profiles as the emulator's
        "MURMUR_LINKS": str(links_file()),
    }
    if not anchors:
        env["MURMUR_ANCHORS"] = "0"
    return spec, env
