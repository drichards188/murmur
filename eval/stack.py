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
import secrets
import shutil
import signal
import subprocess
import sys
import time
import tomllib
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
# the study's participant (eval/proxy.py `Participant`): signs in at the console themselves
PARTICIPANT_OPERATOR = "op-study"

# How a stack's operators prove who they are (rules 7). `dev`: the bridge believes the ids
# requests name (MURMUR_AUTH=dev), logged as dev: the proxy batches, where every decider is a
# script. `credentials`: a run-local credential file, each operator with a fresh random
# passphrase (the participant's printed for them, the scripted ones logged in by the runner),
# each agent with a fresh key; the only mode whose decisions count as people's (study mode).
AUTH_MODES = ("dev", "credentials")


def passphrase() -> str:
    """A fresh random passphrase someone can type: five groups of five letters and digits
    (29 characters, about 129 bits)."""
    alphabet = "abcdefghijkmnpqrstuvwxyz23456789"
    return "-".join("".join(secrets.choice(alphabet) for _ in range(5)) for _ in range(5))


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


def toml_value(v: Any) -> str:
    """A TOML value for the event tables this module writes (strings, numbers, booleans,
    lists, inline tables)."""
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, (int, float)):
        return repr(v)
    if isinstance(v, str):
        return json.dumps(v)
    if isinstance(v, (list, tuple)):
        return "[" + ", ".join(toml_value(x) for x in v) + "]"
    if isinstance(v, dict):
        return "{ " + ", ".join(f"{k} = {toml_value(x)}" for k, x in v.items()) + " }"
    raise TypeError(f"no TOML for {v!r}")


def event_toml(e: dict[str, Any]) -> str:
    return "[[event]]\n" + "".join(f"{k} = {toml_value(v)}\n" for k, v in e.items()) + "\n"


class Stack:
    """The stack's scenario clocks share one origin: `origin = stand-ins start + origin_s`.
    The spec's GNSS events (`at_s`, read once by the stand-in engine at its start) are
    written shifted by `origin_s`; its link events (`at_s` on the emulator's clock) are
    shifted by the emulator's offset to the origin once the stand-ins have started (the
    emulator re-reads the file); the contacts start at the origin; the storyline's `at_s`
    counts from it. The fleet must be ready before the origin, or the run fails."""

    def __init__(
        self,
        spec: Spec,
        seed: int,
        block: PortBlock,
        run_dir: Path,
        run_id: str,
        tidewater_url: str | None = None,
        web: bool = False,
        auth: str = "dev",
    ):
        if auth not in AUTH_MODES:
            raise StackError(f"auth {auth!r}: one of {', '.join(AUTH_MODES)}")
        self.auth = auth
        # credentials run: each operator's passphrase and each agent's key, in memory only
        self.passwords: dict[str, str] = {}
        self.agent_keys: dict[str, str] = {}
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
        self.emu_start: float | None = None
        self.standins_start: float | None = None
        self.origin: float | None = None
        self.contacts_spawned: float | None = None
        self.notes: list[str] = []
        self.spec_events: list[dict[str, Any]] = []

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
    def credentials_file(self) -> Path:
        """A credentials run's own credential file (hashes and key digests only; removed when
        the stack stops)."""
        return self.run_dir / "credentials.json"

    def write_credentials(self) -> None:
        """Every operator with a fresh passphrase and every agent with a fresh key, through
        the `murmur-auth` CLI as a script would (passwords on stdin); kept in memory only."""
        tool = str(_binary("murmur-auth"))
        f = str(self.credentials_file)
        for op in OPERATORS:
            pw = passphrase()
            r = subprocess.run([tool, "--file", f, "add", op], input=pw + "\n", capture_output=True, text=True)
            if r.returncode != 0:
                raise StackError(f"murmur-auth add {op}: {r.stderr.strip()}")
            self.passwords[op] = pw
        for agent in AGENTS:
            r = subprocess.run([tool, "--file", f, "agent-add", agent], capture_output=True, text=True)
            if r.returncode != 0 or not r.stdout.strip():
                raise StackError(f"murmur-auth agent-add {agent}: {r.stderr.strip()}")
            self.agent_keys[agent] = r.stdout.strip()

    @property
    def events_file(self) -> Path:
        """The run's own scripted events (linkemu `--scenario`, stand-ins `MURMUR_EVENTS`),
        on the run's origin. Storyline `link` steps append to it while the run goes; the
        emulator re-reads it within a second of a change."""
        return self.run_dir / "events.toml"

    @property
    def emu_url(self) -> str:
        return f"http://127.0.0.1:{self.block.emu}"

    def load_spec_events(self) -> None:
        if not self.spec.events:
            return
        raw = tomllib.loads(self.spec.events.read_text())
        self.spec_events = [dict(e) for e in raw.get("event", [])]

    def gnss_events(self) -> list[dict[str, Any]]:
        """The spec's GNSS effects, `at_s` shifted onto the origin (the stand-in engine
        counts from its own start, `origin_s` before the origin)."""
        out = []
        for e in self.spec_events:
            if "gnss" not in e:
                continue
            g = {k: v for k, v in e.items() if k not in ("loss", "cut", "extra_latency_ms", "dir")}
            if "at_s" in g:
                g["at_s"] = round(float(g["at_s"]) + self.spec.origin_s, 3)
            out.append(g)
        return out

    def link_events(self) -> list[dict[str, Any]]:
        """The spec's link effects, `at_s` shifted onto the origin on the emulator's clock."""
        assert self.origin is not None and self.emu_start is not None
        out = []
        for e in self.spec_events:
            if not (e.get("cut") or e.get("loss") is not None or e.get("extra_latency_ms") or ("circle" in e and "gnss" not in e)):
                continue
            c = {k: v for k, v in e.items() if k != "gnss"}
            if "at_s" in c:
                c["at_s"] = round(float(c["at_s"]) + (self.origin - self.emu_start), 3)
            out.append(c)
        return out

    def write_events(self, with_links: bool) -> None:
        head = (
            "# The run's scripted events on its origin (eval/stack.py): the spec's GNSS events\n"
            "# shifted for the stand-in engine, its link events for the emulator, then storyline\n"
            "# link steps.\n\n"
        )
        body = "".join(event_toml(e) for e in self.gnss_events())
        if with_links:
            body += "".join(event_toml(e) for e in self.link_events())
        self.events_file.write_text(head + body)

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
        self.load_spec_events()
        self.write_events(with_links=False)
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
        summary = Bridge(self.emu_url).try_get("/stats/summary") or {}
        self.emu_start = time.time() - float(summary.get("uptime_s") or 0.0)
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
            # every run starts clean: never carry another run's open intents over (port blocks
            # are reused, and with them the bridge's resume pointer)
            "MURMUR_RESUME": "0",
            "MURMUR_EVAL_TRUTH": "1",
            "MURMUR_SENSE_SEED": str(self.seed),
            # the console's origin (study mode), the only one a browser may change things from
            "MURMUR_WEB_PORT": str(b.web),
        }
        if self.auth == "credentials":
            self.write_credentials()
            bridge_env["MURMUR_CREDENTIALS"] = str(self.credentials_file)
        else:
            # ids asserted by header, logged as dev: never evidence about people
            bridge_env.update(
                {
                    "MURMUR_AUTH": "dev",
                    "MURMUR_OPERATORS": ",".join(OPERATORS),
                    "MURMUR_AGENTS": ",".join(AGENTS),
                }
            )
        if self.spec.sense_misclass is not None:
            bridge_env["MURMUR_SENSE_MISCLASS"] = str(self.spec.sense_misclass)
        bridge = self._spawn("bridge", [str(_binary("murmur-bridge"))], self._env(**bridge_env))
        self._wait_http(f"http://127.0.0.1:{b.http}/api/stats", bridge, 60)
        if self.auth == "credentials":
            # the scripted operators sign in as anyone would; the participant signs in at the
            # console with the passphrase printed for them
            for op, pw in self.passwords.items():
                if op != PARTICIPANT_OPERATOR:
                    self.bridge.login(op, pw)

        standins_env = {
            "MURMUR_STANDINS_TRUTH": str(self.run_dir / "standins-truth.jsonl"),
            "MURMUR_STANDINS_STATS": "0",
            "MURMUR_EVENTS": str(self.events_file),
        }
        self.standins_start = time.time()
        self._spawn(
            "standins",
            [str(_binary("murmur-standins")), str(self.standins_json)],
            self._env(**standins_env),
        )
        self.origin = self.standins_start + self.spec.origin_s
        # the emulator re-reads the file within a second; the origin is seconds away
        self.write_events(with_links=True)

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

    def wait_ready(self, timeout: float) -> dict[str, Any]:
        """Until every vehicle has a position and is navigation-ready (or `timeout`). The
        result says when, relative to the stand-ins' start, and whether that was before
        the origin."""
        assert self.standins_start is not None and self.origin is not None
        deadline = time.monotonic() + timeout
        status: dict[str, Any] = {"ready": False, "vehicles": {}, "first_ready_s": {}, "first_seen_s": {}}
        want = {v["uid"] for v in self.vehicles}
        while time.monotonic() < deadline:
            dead = [p.name for p in self.procs if not p.alive()]
            if dead:
                raise StackError(f"{', '.join(dead)} exited during startup; see the run directory logs")
            state = self.bridge.try_get("/api/state") or {}
            now_s = round(time.time() - self.standins_start, 2)
            for v in state.get("vehicles", []):
                status["first_seen_s"].setdefault(str(v["uid"]), now_s)
            ready = {v["uid"] for v in state.get("vehicles", []) if v.get("nav_ready") and v.get("position")}
            for u in ready:
                status["first_ready_s"].setdefault(str(u), now_s)
            status["vehicles"] = {str(u): (u in ready) for u in sorted(want)}
            if want <= ready:
                status["ready"] = True
                break
            time.sleep(0.25)
        status["ready_after_s"] = round(time.time() - self.standins_start, 2)
        status["before_origin"] = bool(status["ready"] and time.time() <= self.origin)
        return status

    def start_contacts_at_origin(self, timeout: float = 30.0) -> dict[str, Any]:
        """Wait for the origin, start the contacts process then (its clock is the
        scenario's from there), and wait until its truth reaches the bridge."""
        assert self.origin is not None
        while time.time() < self.origin:
            time.sleep(min(0.05, max(0.0, self.origin - time.time())))
        out: dict[str, Any] = {"spawned_after_origin_s": None, "truth_after_s": None}
        if not self.spec.contacts:
            return out
        self.contacts_spawned = time.time()
        out["spawned_after_origin_s"] = round(self.contacts_spawned - self.origin, 3)
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
                out["truth_after_s"] = round(time.monotonic() - t0, 2)
                return out
            if not proc.alive():
                raise StackError(f"contacts exited with {proc.popen.returncode}; see {proc.log}")
            time.sleep(0.1)
        return out

    def clocks(self) -> dict[str, Any]:
        """Where each clock started relative to the origin (seconds; negative = before)."""
        o = self.origin or 0.0

        def rel(t: float | None) -> float | None:
            return None if t is None else round(t - o, 3)

        return {
            "origin_unix": self.origin,
            "origin_s": self.spec.origin_s,
            "linkemu": rel(self.emu_start),
            "standins": rel(self.standins_start),
            "contacts": rel(self.contacts_spawned),
        }

    # ---- stop ----------------------------------------------------------------------------

    def alive_names(self) -> list[str]:
        return [p.name for p in self.procs if not p.alive()]

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
        # the run's throwaway credentials go with it (hashes only, but no reason to keep them)
        self.credentials_file.unlink(missing_ok=True)
        self.passwords.clear()
        self.agent_keys.clear()
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
