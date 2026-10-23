"""A situation as an episode: what the simulator runs (the process spec and its environment)
and what the driver does (orders, the operator, the AI, scripted faults), from a situation file
(Harbor watch) or a generated one (families.py).

The fleet manifest and the stand-ins' configs are `sim/swarm.py`'s own (`plan`,
`standin_params`, as `scripts/up.sh` and eval/stack.py use them); the bridge's environment is
the one `eval/situation_run.py` gives a situation's stack (its roster, agents, run id, policy,
scenario clock, simulation token, no resume).

**The world** (eval/world.py, sim/worlds/<name>.toml; world v3's H5) gives what was the Golden
Gate's alone: the terrain grid the link emulator reads, the links file and its ground station,
the operator's areas and, by the situation's `[headless]` choice, the land/water grid the
bridge's domain rule reads (H1) and the water our boats and the drifting contacts move in (H4).
A spec that says nothing of them (`EpisodeSpec`'s defaults: the Golden Gate, no terrain source
for the domain rule, still water) runs exactly as every episode before world v3, and its record
has none of world v3's keys.
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
from eval.world import DEFAULT_WORLD, World, load_world

from . import HERE, REPO

#: the Golden Gate Bridge, mid-span (Harbor watch's protected point): the Golden Gate world's
#: `bridge` point
GATE = load_world(DEFAULT_WORLD).point("bridge")
#: the terrain grid baked from tidewater (sim/headless/terrain/README.md): the Golden Gate
#: world's surface grid
TERRAIN_GRID: Path = load_world(DEFAULT_WORLD).surface_grid  # type: ignore[assignment]
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


def links_for(w: World, workdir: Path) -> Path:
    """The link profiles of an episode in world `w`: MURMUR_LINKS when set (as `links_file`),
    else the world's links file, as it is when its `[ground_station]` is the world's (the Golden
    Gate's: sim/links.toml, byte for byte), else a copy in `workdir` with the world's station in
    it. The link emulator has one ground station, so a world naming more is refused here."""
    if os.environ.get("MURMUR_LINKS"):
        return links_file()
    base = w.links
    if not w.ground_stations:
        return base
    if len(w.ground_stations) > 1:
        raise ValueError(f"{w.path}: {len(w.ground_stations)} ground stations; the link emulator has one "
                         "(crates/murmur-linkemu: [ground_station])")
    gs = w.ground_stations[0]
    text = base.read_text()
    cur = tomllib.loads(text).get("ground_station") or {}
    if (cur.get("lat"), cur.get("lon"), cur.get("alt_m")) == (gs.lat, gs.lon, gs.alt_m):
        return base
    want = {"lat": gs.lat, "lon": gs.lon, "alt_m": gs.alt_m}
    out, inside, done = [], False, set()
    for line in text.splitlines():
        s = line.strip()
        if s.startswith("["):
            if inside:
                out += [f"{k} = {v!r}" for k, v in want.items() if k not in done]
            inside = s == "[ground_station]"
        key = s.split("=", 1)[0].strip() if inside and "=" in s and not s.startswith("#") else None
        if key in want:
            out.append(f"{key} = {want[key]!r}")
            done.add(key)
            continue
        out.append(line)
    if inside:
        out += [f"{k} = {v!r}" for k, v in want.items() if k not in done]
    if "ground_station" not in tomllib.loads(text):
        # a table at the end (one at the top would take the file's top-level keys into it)
        out += ["", "[ground_station]", *[f"{k} = {v!r}" for k, v in want.items()]]
    path = workdir / f"links-{w.name}.toml"
    path.write_text("\n".join(out) + "\n")
    return path


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
    # eval/SCORING-V2.md section 0: families.py `world="v2"`), both on the legacy sensing model;
    # "v3": a situation in one of world v3's worlds (any but the Golden Gate's own file, whose
    # situations, the fault drill among them, stay v1), on the sensing model its contacts name
    world: str = "v1"
    # scoring v2's replicate r of the situation (SCORING-V2.md section 1), set by whoever runs
    # replicates; None: the episode seed tells them apart (`run_id`)
    replicate: int | None = None
    # world v3 (eval/world.py): the world it runs in (sim/worlds/<map>.toml; `world` above is the
    # scoring world, v1 or v2) and what the simulator takes from it. The defaults are every
    # episode's before world v3, so a spec that says nothing runs and records as before:
    # `domain_rule` "not_evaluated" (no terrain source for the bridge's domain rule) or "grid" (the
    # world's land/water grid); `currents` "none" (still water) or "world" (the world's current
    # field moves drifting contacts, and our vehicles that take conditions get their water from
    # the world: depth, current and the drawn wind); `conditions` drawn by the generator
    # (`World.draw_conditions`), recorded, its wind in our vehicles' water.
    map: str = DEFAULT_WORLD
    domain_rule: str = "not_evaluated"
    currents: str = "none"
    conditions: dict[str, Any] | None = None
    # world v3's worlds (sim/worlds/README.md): `conditions_from` "world" draws `conditions` (and
    # the start, `at`) from the world's ranges by the episode seed when the episode runs
    # (`drawn`), "none" leaves them as given; `space_snapshot` is the world's pinned element sets
    # (eval/headless/space.py), which an episode reads unless its options name others
    conditions_from: str = "none"
    space_snapshot: str | None = None

    @property
    def place(self) -> World:
        """The world it runs in."""
        return load_world(self.map)

    def as_before_world_v3(self) -> bool:
        """Nothing of world v3: the Golden Gate, no terrain source, still water, no conditions
        (its record carries no `world` key; every v1 and v2 situation)."""
        return (self.map == DEFAULT_WORLD and self.domain_rule == "not_evaluated" and self.currents == "none"
                and not self.conditions and self.conditions_from == "none" and self.space_snapshot is None)

    def world_record(self) -> dict[str, Any] | None:
        """What a record says of its world (None before world v3: no key, the record unchanged)."""
        if self.as_before_world_v3():
            return None
        out = {"name": self.map, "domain_rule": self.domain_rule, "currents": self.currents,
               "conditions": self.conditions}
        if self.conditions_from != "none":
            out["conditions_from"] = self.conditions_from
        if self.space_snapshot is not None:
            out["space"] = self.space_snapshot
        return out

    def drawn(self, seed: int) -> "EpisodeSpec":
        """This situation with its conditions drawn for an episode seed (`conditions_from`
        "world"): `World.draw_conditions` from `random.Random("conditions:<world>:<seed>")`, so
        the same seed draws the same conditions in every situation of a world (a pair of
        situations on one seed meets the same day or night, fog and sea); the draw's start
        becomes the episode's `at` (the scenario clock, so the sun's place). Anything else: as
        it is."""
        if self.conditions_from != "world" or self.conditions is not None:
            return self
        import dataclasses
        import random

        c = self.place.draw_conditions(random.Random(f"conditions:{self.map}:{int(seed)}"))
        return dataclasses.replace(self, conditions=c, at=c["at"])

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
    """A situation file (sim/situations/), as its runner would run it on autoplay: its world and
    `[headless]` choices, and its timeline (`[[step]]`) as the script's items (`timeline`)."""
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
        events=events, path=sit.path, config_name=sit.name, brief=brief_text(sit), script=timeline(sit),
        map=sit.world, domain_rule=sit.domain_rule, currents=sit.currents, conditions_from=sit.conditions,
        space_snapshot=load_world(sit.world).space if sit.world != DEFAULT_WORLD else None,
        world="v1" if sit.world == DEFAULT_WORLD else "v3",
    )


def timeline(sit: Situation) -> list[dict[str, Any]]:
    """A situation's `[[step]]`s as the script's items (script.py `step`): each at its `at_s`,
    with its id, kind and keys, and the storyline's proposer. A `note` is the panel's (display
    only, as live): the simulator has no panel, so it is left out, and Harbor watch's two notes
    leave its script empty, as before."""
    return [{"kind": "step", "at_s": float(s.at_s), "id": s.id, "step": s.kind, "params": dict(s.params),
             "proposer": sit.storyline_proposer}
            for s in sit.steps if s.kind != "note"]


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
        standins.append({**v, "spec": spec, "params": swarm.standin_params(v["platform"], spec)})
    return vehicles, standins


def space_cache_file(links: Path | None = None) -> Path:
    """Where SATCOM looks are kept between episodes (crates/murmur-headless space_cache.rs):
    one file per set of element sets and link profiles, so a new fetch or new profiles start a
    new one."""
    links = links or links_file()
    h = hashlib.sha256(links.read_bytes())
    cache = REPO / "sim" / "space" / "cache"
    if cache.is_dir():
        for f in sorted(cache.iterdir()):
            h.update(f"{f.name}:{f.stat().st_size}".encode())
    return REPO / "logs" / "headless" / "space-cache" / f"{h.hexdigest()[:16]}.jsonl"


def process_spec(e: EpisodeSpec, seed: int, workdir: Path, calib: Calibration,
                 terrain: bool = True, space: bool = True, anchors: bool = True,
                 contacts_path: Path | None = None) -> tuple[dict[str, Any], dict[str, str]]:
    """The `murmur-headless` spec and environment for one episode, in its world (module doc)."""
    vehicles, standins = fleet_plan(e.fleet)
    contacts = contacts_path or e.contacts
    if contacts and e.conditions:
        contacts = contacts_with_conditions(Path(contacts), e.conditions, workdir)
    w = e.place
    links = links_for(w, workdir)
    grid = w.surface_grid
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
        "links": str(links),
        "linkemu_seed": emulator_seed(seed),
        "events": e.events,
        "terrain_grid": str(grid) if terrain and grid is not None and grid.exists() else None,
        "space": space,
        "space_cache": str(space_cache_file(links)) if space else None,
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
        "MURMUR_LINKS": str(links),
    }
    if e.world in ("v1", "v2"):
        # v1 and v2 are the legacy sensing model's worlds (docs/SENSING.md): pinned here, not in
        # their contacts files, which the v1 generator's digests hold byte for byte
        env["MURMUR_SENSE_MODEL"] = "legacy"
    if not anchors:
        env["MURMUR_ANCHORS"] = "0"
    world_spec(e, w, spec, env)
    return spec, env


def sensing_conditions(c: dict[str, Any]) -> dict[str, float]:
    """What drawn conditions (eval/world.py `draw_conditions`) set in a contacts scenario's
    `[conditions]` (docs/SENSING.md): the Koschmieder visibility, the sea state, the wind (the
    sonar's ambient noise)."""
    out: dict[str, float] = {}
    if c.get("visibility_km") is not None:
        out["visibility_m"] = round(1000.0 * float(c["visibility_km"]), 1)
    if c.get("sea_state") is not None:
        out["sea_state"] = float(c["sea_state"])
    if c.get("wind_mps") is not None:
        out["wind_mps"] = float(c["wind_mps"])
    return out


def contacts_with_conditions(path: Path, c: dict[str, Any], workdir: Path) -> Path:
    """A copy of a contacts scenario in the episode's directory with the drawn conditions in its
    `[conditions]` table (the sensing model's: visibility, sea state, wind), each in place of the
    scenario's own value for it; the scenario's other conditions (shipping, water depth, a fixed
    sun) stay. The sun is the scenario clock's, which the draw's start set."""
    want = sensing_conditions(c)
    if not want:
        return path
    text = path.read_text()
    out, inside, done, seen = [], False, set(), False
    for line in text.splitlines():
        st = line.strip()
        if st.startswith("["):
            if inside:
                out += [f"{k} = {v!r}" for k, v in want.items() if k not in done]
            inside = st == "[conditions]"
            seen = seen or inside
        key = st.split("=", 1)[0].strip() if inside and "=" in st and not st.startswith("#") else None
        if key in want:
            out.append(f"{key} = {want[key]!r}")
            done.add(key)
            continue
        out.append(line)
    if inside:
        out += [f"{k} = {v!r}" for k, v in want.items() if k not in done]
    if not seen:
        out += ["", "# drawn for this episode (eval/headless/situations.py contacts_with_conditions)", "[conditions]",
                *[f"{k} = {v!r}" for k, v in want.items()]]
    workdir.mkdir(parents=True, exist_ok=True)
    dst = workdir / f"contacts-{path.stem}.toml"
    dst.write_text("\n".join(out) + "\n")
    tomllib.loads(dst.read_text())  # a table the scenario's own layout broke fails here, not in the simulator
    return dst


def world_spec(e: EpisodeSpec, w: World, spec: dict[str, Any], env: dict[str, str]) -> None:
    """What the world adds to the spec and the bridge's environment, each only when the episode
    asks for it, so an episode as before world v3 has exactly its spec and environment of then:
    the operator's areas when they are not the bridge's default (`MURMUR_AREAS`); the land/water
    grid for the domain rule (`MURMUR_LAND_WATER_GRID`, read by the bridge's own `Settings`); the
    water (`spec["water"]`: the land/water grid for depth, the current field, the drawn wind)."""
    if w.areas.resolve() != (REPO / "sim" / "areas").resolve():
        env["MURMUR_AREAS"] = str(w.areas)
    if e.domain_rule == "grid":
        if w.land_water_grid is None or not w.land_water_grid.exists():
            raise ValueError(f"{w.path}: domain_rule grid, but no baked land/water grid (eval/headless/bake_world.py)")
        env["MURMUR_LAND_WATER_GRID"] = str(w.land_water_grid)
    elif e.domain_rule != "not_evaluated":
        raise ValueError(f"domain_rule {e.domain_rule!r}: grid or not_evaluated")
    if e.currents == "world":
        if w.currents is None or not w.currents.field.exists():
            raise ValueError(f"{w.path}: currents world, but no baked current field (eval/headless/bake_world.py)")
        water: dict[str, Any] = {"currents": str(w.currents.field)}
        if w.land_water_grid is not None and w.land_water_grid.exists():
            water["land_water"] = str(w.land_water_grid)
        c = e.conditions or {}
        if c.get("wind_mps") is not None and c.get("wind_from_deg") is not None:
            import math

            # the wind blows from wind_from_deg: its velocity points the other way
            to = math.radians(float(c["wind_from_deg"]) + 180.0)
            water["wind_en"] = [round(float(c["wind_mps"]) * math.sin(to), 4), round(float(c["wind_mps"]) * math.cos(to), 4)]
        spec["water"] = water
    elif e.currents != "none":
        raise ValueError(f"currents {e.currents!r}: world or none")
