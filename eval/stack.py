"""One isolated Murmur stack for a run: link emulator, bridge, stand-ins, contacts (and the
console in study mode), on a port block of its own, with every log in the run directory.

The fleet manifest is planned with `sim/swarm.py`'s own `plan()` (so callsigns, uids and
sysids are exactly what `make up` would give) but written into the run directory, not
`sim/run/`, so stacks never share files. Stand-ins only: a fleet entry that needs SITL is
refused. Every process is started in its own session and stopped by PID (TERM, then KILL),
and the PIDs are written to `pids.json` so a crashed runner's processes can be stopped with
`python -m eval.run kill <run-dir>`.
"""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

from . import REPO
from .bridge import Bridge
from .ports import PortBlock
from .spec import Spec

OPERATORS = ["op-1", "op-planner", "op-auto", "op-study"]
AGENTS = ["agent-planner", "agent-model"]
SUPERVISOR = "op-1"
PROPOSER_OPERATOR = "op-planner"
AUTO_OPERATOR = "op-auto"


class StackError(RuntimeError):
    pass


def _binary(name: str) -> Path:
    p = REPO / "target" / "release" / name
    if not p.exists():
        raise StackError(f"missing {p}: run `make build` first")
    return p


def _swarm():
    """`sim/swarm.py` as a module (its planner), without running its CLI."""
    spec = importlib.util.spec_from_file_location("murmur_swarm", REPO / "sim" / "swarm.py")
    if spec is None or spec.loader is None:
        raise StackError("sim/swarm.py not importable")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def plan_fleet(fleet_path: Path, vehicle_base: int) -> tuple[list[dict], dict]:
    """The vehicles of a fleet file as swarm.py plans them, every one a stand-in."""
    swarm = _swarm()
    platforms = swarm.load_platforms()
    vehicles = swarm.plan(swarm.load_fleet(str(fleet_path)), platforms)
    for i, v in enumerate(vehicles):
        v["uid"] = i + 1
        v["sysid"] = i % 250 + 1
        v["port"] = vehicle_base + i
    sitl = [v["callsign"] for v in vehicles if v["sim"] != "standin"]
    if sitl:
        raise StackError(
            f"{fleet_path}: {', '.join(sitl)} would need SITL; evaluation fleets are stand-ins "
            "only (add `sim = \"standin\"` to the unit)"
        )
    if len(vehicles) > 90:
        raise StackError(f"{fleet_path}: {len(vehicles)} vehicles exceed a port block (90)")
    return vehicles, platforms


class Proc:
    def __init__(self, name: str, popen: subprocess.Popen, log: Path):
        self.name = name
        self.popen = popen
        self.log = log

    @property
    def pid(self) -> int:
        return self.popen.pid

    def alive(self) -> bool:
        return self.popen.poll() is None


class Stack:
    def __init__(
        self,
        spec: Spec,
        seed: int,
        block: PortBlock,
        run_dir: Path,
        run_id: str,
        tidewater_url: str | None = None,
        web: bool = False,
    ):
        self.spec = spec
        self.seed = seed
        self.block = block
        self.run_dir = run_dir
        self.run_id = run_id
        self.tidewater_url = tidewater_url
        self.web = web
        self.procs: list[Proc] = []
        self.vehicles: list[dict] = []
        self.bridge = Bridge(f"http://127.0.0.1:{block.http}")
        self.audit_src = REPO / "logs" / "audit" / f"{run_id}.jsonl"
        self.started_at: float | None = None
        self.notes: list[str] = []

    # ---- files ---------------------------------------------------------------------------

    @property
    def fleet_json(self) -> Path:
        return self.run_dir / "fleet.json"

    @property
    def bridge_fleet_json(self) -> Path:
        return self.run_dir / "bridge-fleet.json"

    @property
    def standins_json(self) -> Path:
        return self.run_dir / "standins.json"

    @property
    def events_file(self) -> Path:
        """The run's own copy of the scripted events (linkemu `--scenario`, stand-ins
        `MURMUR_EVENTS`). Storyline `link` steps append to it while the run goes; the
        emulator re-reads it within a second of a change."""
        return self.run_dir / "events.toml"

    @property
    def emu_url(self) -> str:
        return f"http://127.0.0.1:{self.block.emu}"

    def write_events(self) -> None:
        head = "# The run's scripted events: the spec's events file, then storyline link steps.\n"
        body = self.spec.events.read_text() if self.spec.events else ""
        self.events_file.write_text(head + body + ("\n" if body and not body.endswith("\n") else ""))

    def write_manifests(self) -> None:
        vehicles, platforms = plan_fleet(self.spec.fleet, self.block.vehicle_base)
        self.vehicles = vehicles
        self.fleet_json.write_text(json.dumps({"vehicles": vehicles}, indent=1))
        self.standins_json.write_text(
            json.dumps(
                {
                    "bridge_host": "127.0.0.1",
                    "vehicles": [{**v, "spec": platforms[v["platform"]]} for v in vehicles],
                }
            )
        )

    def _env(self, **extra: str) -> dict[str, str]:
        env = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith("MURMUR_") or k in ("MURMUR_SPACE_CACHE",)
        }
        env.update(
            {
                "PYTHONUNBUFFERED": "1",
                "MURMUR_ROOT": str(REPO),
                "MURMUR_AT": self.spec.at,
                "MURMUR_RATE": str(self.spec.rate),
            }
        )
        if self.tidewater_url:
            env["MURMUR_TIDEWATER_URL"] = self.tidewater_url
        env.update(extra)
        return env

    def _spawn(self, name: str, cmd: list[str], env: dict[str, str], cwd: Path | None = None) -> Proc:
        log = self.run_dir / f"{name}.log"
        f = open(log, "w")
        try:
            p = subprocess.Popen(
                cmd,
                cwd=str(cwd or REPO),
                env=env,
                stdout=f,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
        finally:
            f.close()
        proc = Proc(name, p, log)
        self.procs.append(proc)
        self.write_pids()
        return proc

    def write_pids(self) -> None:
        (self.run_dir / "pids.json").write_text(
            json.dumps({p.name: p.pid for p in self.procs if p.alive()}, indent=1)
        )

    # ---- start ---------------------------------------------------------------------------

    def start(self) -> None:
        self.run_dir.mkdir(parents=True, exist_ok=True)
        self.started_at = time.time()
        self.write_manifests()
        self.write_events()
        b = self.block
        # the emulator first: it binds every vehicle port before any vehicle sends
        emu_cmd = [
            str(_binary("murmur-linkemu")),
            "--fleet", str(self.fleet_json),
            "--config", str(REPO / "sim" / "links.toml"),
            "--vehicle-base", str(b.vehicle_base),
            "--bridge-base", str(b.bridge_base),
            "--manifest-out", str(self.bridge_fleet_json),
            "--http", f"127.0.0.1:{b.emu}",
            "--log-interval", "30",
            "--seed", str(self.seed),
            "--no-space",
            "--scenario", str(self.events_file),
        ]
        emu = self._spawn("linkemu", emu_cmd, self._env())
        self._wait_http(f"http://127.0.0.1:{b.emu}/health", emu, 60)
        if not self.bridge_fleet_json.exists():
            raise StackError("linkemu wrote no bridge manifest")

        bridge_env = {
            "MURMUR_BASE_PORT": str(b.bridge_base),
            "MURMUR_HTTP_PORT": str(b.http),
            "MURMUR_FLEET_MANIFEST": str(self.bridge_fleet_json),
            "MURMUR_LINKEMU_URL": f"http://127.0.0.1:{b.emu}",
            "MURMUR_POLICY": str(self.spec.policy),
            "MURMUR_AREAS": str(self.spec.areas),
            "MURMUR_RUN_ID": self.run_id,
            "MURMUR_OPERATORS": ",".join(OPERATORS),
            "MURMUR_AGENTS": ",".join(AGENTS),
            "MURMUR_EVAL_TRUTH": "1",
            "MURMUR_SENSE_SEED": str(self.seed),
        }
        if self.spec.sense_misclass is not None:
            bridge_env["MURMUR_SENSE_MISCLASS"] = str(self.spec.sense_misclass)
        bridge = self._spawn("bridge", [str(_binary("murmur-bridge"))], self._env(**bridge_env))
        self._wait_http(f"http://127.0.0.1:{b.http}/api/stats", bridge, 60)

        standins_env = {
            "MURMUR_STANDINS_TRUTH": str(self.run_dir / "standins-truth.jsonl"),
            "MURMUR_STANDINS_STATS": "0",
        }
        if self.spec.events:
            # GNSS events are read once at start (storyline link steps are comms only)
            standins_env["MURMUR_EVENTS"] = str(self.events_file)
        self._spawn(
            "standins",
            [str(_binary("murmur-standins")), str(self.standins_json)],
            self._env(**standins_env),
        )

        # the contacts start once the fleet is ready (start_contacts), so the contacts'
        # clock and the storyline's start together in every seed

        if self.web:
            vite = REPO / "web" / "node_modules" / ".bin" / "vite"
            if not vite.exists():
                raise StackError(f"missing {vite}: run `make setup` (npm install) for study mode")
            self._spawn(
                "web",
                [str(vite), "--host", "127.0.0.1", "--port", str(b.web), "--strictPort"],
                self._env(MURMUR_BRIDGE_URL=f"http://127.0.0.1:{b.http}"),
                cwd=REPO / "web",
            )

    def _wait_http(self, url: str, proc: Proc, timeout: float) -> None:
        deadline = time.monotonic() + timeout
        client = Bridge(url.rsplit("/", 1)[0])
        path = "/" + url.rsplit("/", 1)[1]
        while time.monotonic() < deadline:
            if client.try_get(path) is not None:
                return
            if not proc.alive():
                raise StackError(f"{proc.name} exited with {proc.popen.returncode}; see {proc.log}")
            time.sleep(0.3)
        raise StackError(f"{proc.name} did not answer at {url} within {timeout:.0f} s; see {proc.log}")

    def start_contacts(self, timeout: float = 30.0) -> float | None:
        """Start the contacts process and wait until its truth reaches the bridge. Called
        once the fleet is ready, so the scenario's contact times (`start_s`, `end_s`, the
        route timing) count from the same moment as the storyline's `at_s`, whatever the
        fleet's readiness took in this seed. Returns seconds to the first truth, None
        without contacts or if none arrived."""
        if not self.spec.contacts:
            return None
        proc = self._spawn(
            "contacts",
            [str(_binary("murmur-contacts")), str(self.spec.contacts)],
            self._env(
                MURMUR_BRIDGE_URL=f"http://127.0.0.1:{self.block.http}",
                MURMUR_CONTACTS_TRUTH=str(self.run_dir / "contacts-truth.jsonl"),
                MURMUR_CONTACTS_SEED=str(self.seed),
                MURMUR_CONTACTS_HZ="1",
            ),
        )
        t0 = time.monotonic()
        while time.monotonic() - t0 < timeout:
            if (self.bridge.truth() or {}).get("truth"):
                return round(time.monotonic() - t0, 2)
            if not proc.alive():
                raise StackError(f"contacts exited with {proc.popen.returncode}; see {proc.log}")
            time.sleep(0.2)
        return None

    def wait_ready(self, timeout: float) -> dict[str, Any]:
        """Until every vehicle has a position and is navigation-ready. Returns what was and
        wasn't ready; a timeout is not fatal (the run records it)."""
        deadline = time.monotonic() + timeout
        t0 = time.monotonic()
        status: dict[str, Any] = {"ready": False, "vehicles": {}}
        want = {v["uid"] for v in self.vehicles}
        while time.monotonic() < deadline:
            dead = [p.name for p in self.procs if not p.alive()]
            if dead:
                raise StackError(f"{', '.join(dead)} exited during startup; see the run directory logs")
            state = self.bridge.try_get("/api/state") or {}
            ready = {v["uid"] for v in state.get("vehicles", []) if v.get("nav_ready") and v.get("position")}
            status["vehicles"] = {str(u): (u in ready) for u in sorted(want)}
            if want <= ready:
                status["ready"] = True
                break
            time.sleep(1.0)
        status["ready_after_s"] = round(time.monotonic() - t0, 1)
        return status

    # ---- stop ----------------------------------------------------------------------------

    def stop(self) -> dict[str, Any]:
        exits: dict[str, Any] = {}
        for p in reversed(self.procs):
            if p.alive():
                try:
                    os.killpg(os.getpgid(p.pid), signal.SIGTERM)
                except (ProcessLookupError, PermissionError):
                    pass
        deadline = time.monotonic() + 6
        for p in reversed(self.procs):
            while p.alive() and time.monotonic() < deadline:
                time.sleep(0.1)
            if p.alive():
                try:
                    os.killpg(os.getpgid(p.pid), signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    pass
                p.popen.wait(timeout=5)
                exits[p.name] = "killed"
            else:
                exits[p.name] = p.popen.returncode
        self.write_pids()
        # the audit log belongs with the run
        dst = self.run_dir / "audit.jsonl"
        if self.audit_src.exists():
            shutil.move(str(self.audit_src), str(dst))
        return exits


def kill_from_pids(run_dir: Path) -> list[str]:
    """Stop whatever `pids.json` in a run directory still names (crash recovery)."""
    p = run_dir / "pids.json"
    if not p.exists():
        return []
    stopped = []
    for name, pid in json.loads(p.read_text()).items():
        try:
            os.killpg(os.getpgid(int(pid)), signal.SIGTERM)
            stopped.append(f"{name} ({pid})")
        except (ProcessLookupError, PermissionError, ValueError):
            continue
    return stopped


if __name__ == "__main__":
    print("use: python -m eval.run", file=sys.stderr)
