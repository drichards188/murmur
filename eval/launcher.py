#!/usr/bin/env python3
"""Murmur's main menu (`make start`), what a person opens first:

    make start                     # then open http://localhost:5173
    .venv/bin/python -m eval.launcher

A small local service. It owns the console's web server for the session (one Vite dev server on
MURMUR_WEB_PORT, 5173), which shows the menu when nothing runs, and it starts and stops one
scenario at a time when the menu asks:

- the open world, the default: what `make goldengate` runs (`make goldengate` itself: the
  full-spectrum fleet at the Golden Gate, tidewater's tide, current and wind, the harbor's
  traffic), on the default stack's ports (8765 and the rest);
- a situation (sim/situations/*.toml): what `make situation NAME=... OPERATOR=... AI=...` runs
  (`eval.situation_run` itself: a fresh station, the opening orders, the AI through the user's
  own CLI, the Situation panel), on the situation's own port block (bridge 9265 and the rest).

Each runs exactly as its command runs from a terminal, with two differences: `scripts/up.sh`
starts no console of its own (MURMUR_WEB=0, MURMUR_WEB_PORT the launcher's, so the bridge
accepts the launcher's console as its origin), and the console reaches whichever bridge runs
through the launcher's bridge relay (a TCP relay on the launcher's port + 1, which Vite proxies
/api and /ws to). The open world and a situation keep their own HTTP ports, so each keeps its own
restore and resume pointers (`logs/audit/.last-run-<port>`): a situation's fresh station never
touches what the open world's runs carry, as from the command line.

Ending a scenario is `make down`'s, in order: the situation runner first (SIGTERM: it stops the
AI's process group and the scripted operators and writes its summary), then the stack (SIGTERM
to up.sh, which stops tidewater, the link emulator, the stand-ins, SITL, the bridge, the
contacts), then the launcher waits until the stack's ports are free. A situation whose time is
up leaves its stack up for inspection, as the runner does, until it is ended. A launch that
fails stops whatever it started and says why, with its log.

The API, on 127.0.0.1 only (the console reaches it through Vite's proxy under /launcher):

    GET  /launcher/api/state   the scenarios, and the current launch with its progress
    POST /launcher/api/start   {"scenario": "open-world" | <situation>, "operator": "live" |
                               "autoplay", "ai": "claude" | "codex" | "playbook" | "none",
                               "replace": true (end the running one first)}
    POST /launcher/api/end     end the running scenario

A POST must come from the console's origin (`Origin`, as a browser sends it) as JSON, and may
name only a scenario of the launcher's own list and the fixed options above: nothing a request
carries reaches a command line but a name from that list.

Logs: logs/launcher/ (this launcher's, the console's web.log, one per launch with its command's
output); a situation's own run directory as always (logs/situations/<run id>/).
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
import tomllib
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

from . import PYTHON, REPO
from .bridge import Bridge
from .situation import (
    AI_LABEL,
    AI_MODES,
    OPERATOR_MODES,
    SITUATIONS,
    Situation,
    SituationError,
    headless_steps,
    load_situation,
    port_problems,
    resolve_modes,
    situation_env,
    stack_ports,
    tcp_answers,
)
from .situation_run import plan_counts

OPEN_WORLD = "open-world"
LAUNCHER_PORT = 8760  # the API (MURMUR_LAUNCHER_PORT); the bridge relay listens on the next port
CONSOLE_PORT = 5173  # MURMUR_WEB_PORT, as up.sh's
# What `make goldengate` runs by default (the Makefile's goldengate target is the command; these
# describe it in the menu, and eval/tests/test_launcher.py holds them to the Makefile)
OPEN_WORLD_FLEET = "full-spectrum"
OPEN_WORLD_CONTACTS = "sim/scenarios/golden-gate.toml"
OPEN_WORLD_TIDEWATER = "golden-gate-2026-06"
OPEN_WORLD_AT = "2026-06-14T10:00:00Z"
# the default stack's ports (scripts/up.sh's defaults; each MURMUR_* variable overrides one)
DEFAULT_PORTS = {
    "MURMUR_BASE_PORT": 14560,
    "MURMUR_HTTP_PORT": 8765,
    "MURMUR_LINKEMU_PORT": 8766,
    "MURMUR_TIDEWATER_PORT": 8770,
    "MURMUR_SITL_INSTANCE_BASE": 0,
}
# make's own variables and the Makefile's settings for goldengate and situation: a shell may
# export one for something else, and it would change what the menu's choice runs
MAKE_VARS = (
    "MAKEFLAGS", "MFLAGS", "MAKELEVEL", "MAKEOVERRIDES",
    "AT", "EVENTS", "CONTACTS", "GG_FLEET", "ARGS", "FLEET", "NAME", "OPERATOR", "AI", "COSIGNER", "REVEAL",
)
START_KEYS = {"scenario", "operator", "ai", "replace"}
DOMAINS = ("air", "ground", "surface", "subsurface")  # sim/platforms.toml's, in the menu's order
NAME_RE = re.compile(r"^[a-z0-9][a-z0-9-]*$")
# how long vehicles usually take to be navigation-ready (the menu says so while it waits)
TYPICAL_SITL_S = (30, 60)
TYPICAL_STANDINS_S = (5, 20)
READY_TIMEOUT_S = 180.0  # the open world goes on with the vehicles that are ready by then
RUNNER_STOP_S = 60.0  # the runner stops the AI (8 s, then SIGKILL) and its tasks (15 s)
STACK_STOP_S = 90.0
PORTS_FREE_S = 30.0
ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")

# a launch's stages, in order; `ended` (a situation's time is up, its stack still up for
# inspection), `stopping`, `stopped` and `failed` come after any of them
STAGES = ("waiting", "starting", "bridge", "ready", "running")
ACTIVE = (*STAGES, "ended", "stopping")
STACK_UP = ("bridge", "ready", "running", "ended")


def say(msg: str) -> None:
    try:
        print(f"launcher: {msg}", flush=True)
    except OSError:
        pass


def utc_stamp() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def rel(p: Path) -> str:
    try:
        return str(p.relative_to(REPO))
    except ValueError:
        return str(p)


class BadRequest(ValueError):
    """A request the launcher refuses (400), saying why."""


class LaunchError(RuntimeError):
    """A launch that can't go on, saying why."""


# ---- the scenarios ------------------------------------------------------------------------------


def _contacts(path: Path | None) -> int:
    if path is None:
        return 0
    try:
        return len(tomllib.loads(path.read_text()).get("contact", []))
    except (OSError, tomllib.TOMLDecodeError):
        return 0


def _fleet(path: Path) -> dict[str, Any]:
    vehicles, sitl = plan_counts(path)
    return {
        "vehicles": len(vehicles),
        "sitl": sitl,
        "platform_types": len({v["platform"] for v in vehicles}),
        # how many of them in each domain (the catalog's), which the menu draws
        "domains": {d: sum(1 for v in vehicles if v["domain"] == d) for d in DOMAINS},
        "typical_s": list(TYPICAL_SITL_S if sitl else TYPICAL_STANDINS_S),
    }


def open_world_entry(root: Path = REPO) -> dict[str, Any]:
    fleet = _fleet(root / "sim" / "fleets" / f"{OPEN_WORLD_FLEET}.toml")
    return {
        "id": OPEN_WORLD,
        "kind": "open_world",
        "default": True,
        "title": "Open world: Golden Gate",
        "brief": (
            f"The full-spectrum fleet at the Golden Gate: one of each of the {fleet['platform_types']} platform types "
            "in the catalog, in the air, on the ground, on and under the water, with real tide, current and wind "
            "from tidewater and the harbor's traffic: ferries, ships in the lane, fishing boats, a yacht, aircraft, "
            "and a few contacts that bear watching.\n\n"
            "You command freely, as with make goldengate from a terminal."
        ),
        "runnable": True,
        "why_not": None,
        **fleet,
        "contacts": _contacts(root / OPEN_WORLD_CONTACTS),
        "tidewater": OPEN_WORLD_TIDEWATER,
        "at": OPEN_WORLD_AT,
        "duration_s": None,
        "operators": [],
        "ais": [],
        "cli": "make goldengate",
    }


def why_not_live(sit: Situation, err: SituationError) -> str:
    """A short reason a situation can't start from the menu."""
    if headless_steps(sit) or sit.conditions == "world":
        return f"headless only: make headless SITUATION={sit.name}"
    return str(err)


def situation_entry(path: Path, loaded: dict[str, Situation] | None = None) -> dict[str, Any] | None:
    """A situation as the menu shows it (never its director's notes), or None for a file name the
    menu does not take (it only ever passes a listed name to a command line). A runnable one is
    kept in `loaded`, to check a start's modes against."""
    name = path.stem
    if not NAME_RE.match(name):
        return None
    entry: dict[str, Any] = {"id": name, "kind": "situation", "default": False, "title": name, "brief": "", "runnable": False}
    try:
        sit = load_situation(path)
    except SituationError as e:
        entry["why_not"] = f"does not load: {e}"
        return entry
    entry.update(title=sit.title, brief=sit.brief, cli=f"make situation NAME={name}")
    if sit.todo:
        entry["why_not"] = "a draft: only its title and brief are written yet"
        return entry
    try:
        resolve_modes(sit)
    except SituationError as e:
        entry["why_not"] = why_not_live(sit, e)
        return entry
    operators = []
    for op in OPERATOR_MODES:
        try:
            resolve_modes(sit, operator=op)
            operators.append(op)
        except SituationError:
            pass
    entry.update(
        runnable=True,
        why_not=None,
        **(_fleet(sit.fleet) if sit.fleet else {}),
        contacts=_contacts(sit.contacts),
        tidewater=sit.tidewater_wanted,
        at=sit.at,
        duration_s=sit.duration_s,
        rate=sit.rate,
        operator=sit.operator,
        ai=sit.ai,
        operators=operators,
        ais=list(AI_MODES),
        orders=len(sit.orders),
        steps=len(sit.steps),
        watch=len(sit.watch),
    )
    if loaded is not None:
        loaded[name] = sit
    return entry


def catalog(root: Path = REPO, situations: Path = SITUATIONS, loaded: dict[str, Situation] | None = None) -> list[dict[str, Any]]:
    """The menu's list: the open world first (the default), then the situations a person can
    start (by name), then the rest with why not (headless only, drafts)."""
    entries = [e for e in (situation_entry(p, loaded) for p in sorted(situations.glob("*.toml"))) if e is not None]
    return [open_world_entry(root), *[e for e in entries if e["runnable"]], *[e for e in entries if not e["runnable"]]]


# ---- a choice, its command and its environment -------------------------------------------------


@dataclass(frozen=True)
class Choice:
    scenario: str
    kind: str  # open_world | situation
    operator: str | None = None
    ai: str | None = None
    replace: bool = False


def validate_start(body: Any, entries: list[dict[str, Any]], situations: dict[str, Situation] | None = None) -> Choice:
    """A start request checked against the menu's own list and the fixed options; BadRequest
    says what is wrong. Nothing else of the body is used."""
    if not isinstance(body, dict):
        raise BadRequest("a JSON object: scenario, and for a situation operator and ai")
    extra = sorted(set(body) - START_KEYS)
    if extra:
        raise BadRequest(f"unknown keys {extra}: only {sorted(START_KEYS)}")
    by_id = {e["id"]: e for e in entries}
    sid = body.get("scenario")
    if not isinstance(sid, str) or sid not in by_id:
        raise BadRequest(f"no scenario {sid!r} in the menu")
    entry = by_id[sid]
    if not entry["runnable"]:
        raise BadRequest(f"{entry['title']} can't be started here: {entry.get('why_not')}")
    replace = body.get("replace", False)
    if not isinstance(replace, bool):
        raise BadRequest("replace is true or false")
    if entry["kind"] == "open_world":
        if body.get("operator") is not None or body.get("ai") is not None:
            raise BadRequest("the open world takes no operator or AI: you command it, nothing is scripted")
        return Choice(sid, "open_world", replace=replace)
    operator = body.get("operator") if body.get("operator") is not None else entry["operator"]
    ai = body.get("ai") if body.get("ai") is not None else entry["ai"]
    if operator not in entry["operators"]:
        raise BadRequest(f"operator {operator!r}: one of {', '.join(entry['operators'])}")
    if ai not in AI_MODES:
        raise BadRequest(f"AI {ai!r}: one of {', '.join(AI_MODES)}")
    sit = (situations or {}).get(sid)
    if sit is not None:
        try:
            resolve_modes(sit, operator=operator, ai=ai)
        except SituationError as e:
            raise BadRequest(str(e)) from None
    return Choice(sid, "situation", operator=operator, ai=ai, replace=replace)


def command(choice: Choice, python: str = str(PYTHON)) -> list[str]:
    """What a choice runs: the command line's own command, never a shell string."""
    if choice.kind == "open_world":
        return ["make", "--no-print-directory", "goldengate"]
    if choice.operator not in OPERATOR_MODES or choice.ai not in AI_MODES or not NAME_RE.match(choice.scenario):
        raise BadRequest(f"not a choice the menu offers: {choice}")
    return [python, "-m", "eval.situation_run", choice.scenario, "--operator", choice.operator, "--ai", choice.ai]


def cli_text(choice: Choice) -> str:
    """The same choice as a person types it."""
    if choice.kind == "open_world":
        return "make goldengate"
    return f"make situation NAME={choice.scenario} OPERATOR={choice.operator} AI={choice.ai}"


def launch_env(base: dict[str, str], console_port: int) -> dict[str, str]:
    """The environment a launch runs in: the launcher's own (so the user's CLI logins and any
    MURMUR_* port variable work as from their shell) without make's variables, with up.sh told
    the console is the launcher's."""
    env = {k: v for k, v in base.items() if k not in MAKE_VARS}
    env.update({"MURMUR_WEB": "0", "MURMUR_WEB_PORT": str(console_port), "PYTHONUNBUFFERED": "1"})
    return env


def stack_ports_for(choice: Choice, env: dict[str, str]) -> dict[str, int]:
    """The ports the choice's stack uses (`stack_ports`' shape): the default stack's for the open
    world, the situation's block for a situation; each MURMUR_* variable overrides one."""
    if choice.kind == "situation":
        return stack_ports(env)

    def port(k: str) -> int:
        return int((env.get(k) or "").strip() or DEFAULT_PORTS[k])

    base = port("MURMUR_BASE_PORT")
    return {
        "vehicle_base": base,
        "bridge_base": int((env.get("MURMUR_BRIDGE_BASE_PORT") or "").strip() or base + 1000),
        "http": port("MURMUR_HTTP_PORT"),
        "web": int(env["MURMUR_WEB_PORT"]),
        "linkemu": port("MURMUR_LINKEMU_PORT"),
        "tidewater": port("MURMUR_TIDEWATER_PORT"),
        "sitl_instance_base": port("MURMUR_SITL_INSTANCE_BASE"),
    }


def refusal(method: str, headers: Any, console_port: int) -> str | None:
    """Why a request is refused: a POST from another origin than the console's (a browser always
    sends Origin with one), or not JSON. A GET only reads the state."""
    if method != "POST":
        return None
    allowed = {f"http://127.0.0.1:{console_port}", f"http://localhost:{console_port}"}
    origin = (headers.get("Origin") or "").rstrip("/")
    if origin not in allowed:
        return f"refused: a request from another origin ({origin or 'no Origin'}); the menu is at http://localhost:{console_port}"
    if not (headers.get("Content-Type") or "").lower().startswith("application/json"):
        return "refused: the body must be JSON (Content-Type: application/json)"
    return None


# ---- the logs -------------------------------------------------------------------------------------


def tail_lines(path: Path | None, size: int = 16384) -> list[str]:
    """The last lines of a log, without colour codes or blank lines."""
    if path is None:
        return []
    try:
        with path.open("rb") as f:
            f.seek(0, os.SEEK_END)
            f.seek(max(0, f.tell() - size))
            text = f.read().decode(errors="replace")
    except OSError:
        return []
    out = []
    for line in text.replace("\r", "\n").splitlines():
        line = ANSI.sub("", line).strip()
        if line:
            out.append(line)
    return out


def _noise(line: str) -> bool:
    """make's own words, a runner's warning or summary heading, the launch log's header."""
    return line.startswith(("make: ***", "make[", "situation: warning", "===", "# "))


def failure_reason(lines: list[str]) -> str:
    """Why a launch's command stopped, from its output: the situation runner's own refusal or
    failure ("situation: ..."), else up.sh's last words."""
    for line in reversed(lines):
        if line.startswith("situation: ") and not _noise(line):
            return line.removeprefix("situation: ")
    for line in reversed(lines):
        if not _noise(line):
            return line
    return "it stopped without saying why"


def progress_line(lines: list[str]) -> str:
    """The latest line of a starting stack's output, short, for the menu's progress."""
    for line in reversed(lines):
        if not _noise(line):
            line = line.removeprefix("situation: ")
            return line if len(line) <= 160 else line[:157] + "..."
    return ""


# ---- processes --------------------------------------------------------------------------------------


def proc_stat(pid: int) -> tuple[str, int, int, int] | None:
    """(state, parent, session, start time in clock ticks since boot) from /proc."""
    try:
        rest = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return rest[0], int(rest[1]), int(rest[3]), int(rest[19])
    except (OSError, IndexError, ValueError):
        return None


def alive(pid: int, start: int | None) -> bool:
    st = proc_stat(pid)
    return st is not None and st[0] != "Z" and (start is None or st[3] == start)


def terminate(proc: subprocess.Popen, grace: float) -> bool:
    """SIGTERM, then SIGKILL after `grace` seconds; whether it stopped on SIGTERM."""
    if proc.poll() is not None:
        return True
    try:
        proc.send_signal(signal.SIGTERM)
        proc.wait(timeout=grace)
        return True
    except subprocess.TimeoutExpired:
        proc.kill()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass
        return False


# ---- the bridge relay -------------------------------------------------------------------------------


def _pipe(src: socket.socket, dst: socket.socket) -> None:
    try:
        while data := src.recv(65536):
            dst.sendall(data)
    except OSError:
        pass
    finally:
        try:
            dst.shutdown(socket.SHUT_WR)
        except OSError:
            pass


class Relay:
    """A TCP relay from a fixed local port to whichever bridge runs (Vite proxies the console's
    /api and /ws here): the open world and a situation keep their own HTTP ports. Bytes only,
    both ways, so HTTP and the WebSocket pass as they are; with no bridge it closes at once."""

    def __init__(self, port: int):
        self.port = port
        self.target: int | None = None
        self.server = socket.create_server(("127.0.0.1", port))

    def serve(self) -> None:
        while True:
            try:
                conn, _ = self.server.accept()
            except OSError:
                return  # closed
            threading.Thread(target=self.handle, args=(conn,), daemon=True).start()

    def handle(self, conn: socket.socket) -> None:
        target = self.target
        try:
            if target is None:
                return
            try:
                up = socket.create_connection(("127.0.0.1", target), timeout=3)
            except OSError:
                return
            up.settimeout(None)
            back = threading.Thread(target=_pipe, args=(up, conn), daemon=True)
            back.start()
            _pipe(conn, up)
            back.join()
            up.close()
        finally:
            conn.close()

    def close(self) -> None:
        self.server.close()


# ---- a launch ---------------------------------------------------------------------------------------


class Launch:
    """One scenario from start to end, run by its own thread: its command, its progress read
    from the bridge (and for a situation the runner's status), and its end."""

    def __init__(self, launcher: "Launcher", choice: Choice, entry: dict[str, Any], previous: "Launch | None"):
        self.launcher = launcher
        self.choice = choice
        self.entry = entry
        self.previous = previous
        self.id = f"{choice.scenario}-{utc_stamp()}"
        self.title = entry["title"]
        self.env = launch_env(dict(os.environ), launcher.console_port)
        if choice.kind == "situation":
            self.env = situation_env(self.env)
        self.ports = stack_ports_for(choice, self.env)
        self.argv = command(choice)
        self.log = launcher.log_dir / f"{self.id}.log"
        self.expected = int(entry.get("vehicles") or 0)
        self.sitl = int(entry.get("sitl") or 0)
        self.stage = "waiting" if previous is not None else "starting"
        self.detail = f"ending {previous.title} first" if previous is not None else "starting"
        self.error: str | None = None
        self.leftovers: list[str] = []
        self.started_t = time.time()
        self.stage_t = self.started_t
        self.history: list[dict[str, Any]] = [{"stage": self.stage, "t": round(self.started_t, 3)}]
        self.ready: tuple[int, int] | None = None
        self.run_info: dict[str, Any] | None = None
        self.proc: subprocess.Popen | None = None
        self.up_pid: tuple[int, int] | None = None  # up.sh: (pid, start ticks), once identified
        self.up_log: Path | None = None  # a situation's stack output (its run directory's up.log)
        self.tidewater_reused = False
        # whether a stack came up at all (up.sh identified, or the bridge answered): only then are
        # its ports waited for at the end
        self.stack_started = False
        self.bridge = Bridge(f"http://127.0.0.1:{self.ports['http']}", timeout=2.0)
        self.end_requested = threading.Event()
        self.thread = threading.Thread(target=self.run, name=f"launch {self.id}", daemon=True)

    # ---- state ------------------------------------------------------------------------------------

    def active(self) -> bool:
        return self.stage in ACTIVE

    def set_stage(self, stage: str, detail: str) -> None:
        with self.launcher.lock:
            if self.end_requested.is_set() and stage not in ("stopping", "stopped", "failed"):
                return  # asked to end: it only goes down from here
            if stage != self.stage:
                self.stage = stage
                self.stage_t = time.time()
                self.history.append({"stage": stage, "t": round(self.stage_t, 3)})
                self.launcher.note(f"{self.id}: {stage}: {detail}")
            self.detail = detail

    def public(self) -> dict[str, Any]:
        c = self.choice
        options = {} if c.kind == "open_world" else {"operator": c.operator, "ai": c.ai, "ai_label": AI_LABEL[c.ai or "none"]}
        return {
            "id": self.id,
            "scenario": c.scenario,
            "kind": c.kind,
            "title": self.title,
            "options": options,
            "cli": cli_text(c),
            "stage": self.stage,
            "detail": self.detail,
            "error": self.error,
            "leftovers": self.leftovers,
            "started_t": round(self.started_t, 3),
            "stage_t": round(self.stage_t, 3),
            "history": self.history,
            "ready": None if self.ready is None else {"ready": self.ready[0], "of": self.ready[1]},
            "typical_s": list(TYPICAL_SITL_S if self.sitl else TYPICAL_STANDINS_S),
            "log": rel(self.log),
            "bridge_port": self.ports["http"],
            "run": self.run_info,
            "stack_up": self.stage in STACK_UP,
        }

    def request_end(self) -> None:
        """End it: its thread stops what it started within a second; the console is told at once
        (it stops asking the bridge)."""
        self.end_requested.set()
        self.set_stage("stopping", "ending: stopping what it started")

    # ---- the run ----------------------------------------------------------------------------------

    def run(self) -> None:
        try:
            if self.previous is not None:
                self.previous.thread.join()
                if self.previous.leftovers:
                    raise LaunchError(f"{self.previous.title} did not stop cleanly ({'; '.join(self.previous.leftovers)}): `make down`, then start again")
            if self.end_requested.is_set():
                self.set_stage("stopped", "ended before it started")
                return
            self.preflight()
            if self.end_requested.is_set():
                self.set_stage("stopped", "ended before it started")
                return
            self.start()
            self.monitor()
        except LaunchError as e:
            self.fail(str(e))
        except Exception as e:  # noqa: BLE001 (said in the menu and the launcher's log)
            self.fail(f"the launcher failed ({type(e).__name__}: {e})")

    def preflight(self) -> None:
        """The open world refuses to start beside a stack on its ports (the situation runner
        checks its own). A tidewater already answering is reused, as up.sh does."""
        self.tidewater_reused = self.choice.kind == "open_world" and tcp_answers(self.ports["tidewater"])
        if self.choice.kind != "open_world":
            return
        self.set_stage("starting", "checking that no other stack holds the open world's ports")
        problems = port_problems(self.ports, self.expected, self.sitl, skip=("web", "tidewater"))
        if problems:
            raise LaunchError("refusing to start: " + "; ".join(problems) + ". If that is a stack of yours, `make down` stops it.")

    def start(self) -> None:
        self.log.parent.mkdir(parents=True, exist_ok=True)
        with self.log.open("w") as f:
            f.write(f"# {cli_text(self.choice)}\n# {' '.join(self.argv)}\n")
            f.flush()
            self.proc = subprocess.Popen(
                self.argv, cwd=str(REPO), env=self.env, stdout=f, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True
            )
        self.launcher.relay.target = self.ports["http"]
        self.launcher.note(f"{self.id}: started {' '.join(self.argv)} (pid {self.proc.pid}; output {rel(self.log)})")
        self.set_stage("starting", "starting the stack")

    def note_up_pid(self) -> None:
        """Identify this launch's up.sh from logs/up.pid (it writes it as it starts): the open
        world's runs in make's session, a situation's is the runner's child. Only that process
        is ever signalled."""
        if self.up_pid is not None or self.proc is None:
            return
        try:
            pid = int((REPO / "logs" / "up.pid").read_text().strip())
        except (OSError, ValueError):
            return
        st = proc_stat(pid)
        if st is None or st[0] == "Z":
            return
        ours = st[2] == self.proc.pid if self.choice.kind == "open_world" else st[1] == self.proc.pid
        if ours:
            self.up_pid = (pid, st[3])
            self.stack_started = True

    def up_alive(self) -> bool:
        return self.up_pid is not None and alive(*self.up_pid)

    def output(self) -> list[str]:
        """The stack's latest output: a situation's run directory's up.log once the runner names
        it, else this launch's own log."""
        lines = tail_lines(self.log)
        if self.choice.kind == "situation" and self.up_log is None:
            for line in lines:
                m = re.search(r"its output: ([^)\s]+up\.log)", line)
                if m:
                    self.up_log = REPO / m.group(1)
        return tail_lines(self.up_log) if self.up_log is not None and self.stage == "starting" else lines

    def monitor(self) -> None:
        bridge_t: float | None = None
        while not self.end_requested.wait(1.0):
            self.note_up_pid()
            assert self.proc is not None
            code = self.proc.poll()
            state = self.bridge.try_get("/api/state")
            sit = (self.bridge.try_get("/api/situation") or {}).get("situation") if state is not None and self.choice.kind == "situation" else None
            if isinstance(sit, dict) and isinstance(sit.get("run"), dict):
                self.run_info = {k: sit["run"].get(k) for k in ("id", "audit_log", "situation_log")}
            if code is not None:
                lines = tail_lines(self.log)
                if self.choice.kind == "situation" and code == 0:
                    # its time is up: the runner leaves the stack up for inspection, as from a
                    # terminal, until it is ended here (or by make down)
                    if self.up_alive():
                        if self.stage != "ended":
                            self.set_stage("ended", "the situation's time is up; its stack stays up for inspection until you end it")
                        continue
                    self.stop_all()
                    self.set_stage("stopped", "its stack was stopped outside the menu (make down)")
                    return
                if self.choice.kind == "open_world" and code == 0 and not any("a process exited" in x for x in lines[-5:]):
                    # up.sh stopped on a request from outside the menu (make down)
                    self.stop_all()
                    self.set_stage("stopped", "the stack was stopped outside the menu (make down)")
                    return
                raise LaunchError(f"{failure_reason(lines)} (exit {code}; see {rel(self.log)})")
            if state is None:
                if self.stage == "starting":
                    self.set_stage("starting", progress_line(self.output()) or "starting the stack")
                elif self.stage in STACK_UP and self.choice.kind == "open_world" and not self.up_alive():
                    raise LaunchError(f"the stack stopped (see {rel(self.log)})")
                continue
            self.stack_started = True
            vehicles = state.get("vehicles") or []
            ready = sum(1 for v in vehicles if v.get("nav_ready") and v.get("position"))
            of = max(self.expected, len(vehicles))
            with self.launcher.lock:
                self.ready = (ready, of)
            if bridge_t is None:
                bridge_t = time.time()
            counted = f"{ready} of {of} vehicles ready"
            if self.choice.kind == "open_world":
                if self.stage in ("starting", "bridge"):
                    if ready >= of:
                        self.set_stage("running", counted)
                    elif time.time() - bridge_t > READY_TIMEOUT_S:
                        self.set_stage("running", f"{counted} after {READY_TIMEOUT_S:.0f} s; the rest may follow")
                    else:
                        self.set_stage("bridge", f"bridge up; {counted}")
                continue
            phase = str((sit or {}).get("phase") or "starting")
            said = str((sit or {}).get("phase_detail") or "")
            if phase == "running" and self.stage != "running":
                self.set_stage("running", counted)
            elif phase == "ended" and self.stage != "ended":
                self.set_stage("ended", "the situation's time is up; its stack stays up for inspection until you end it")
            elif phase == "starting":
                # forward only: a vehicle that drops its fix for a moment does not undo "ready"
                self.set_stage("ready" if ready >= of or self.stage == "ready" else "bridge", f"{said or 'bridge up'}; {counted}")
        self.stop_all()
        self.set_stage("stopped", "ended: everything it started has stopped" if not self.leftovers else "ended, but still held: " + "; ".join(self.leftovers))

    def fail(self, reason: str) -> None:
        with self.launcher.lock:
            self.error = reason
        self.launcher.note(f"{self.id}: failed: {reason}")
        if self.proc is not None:
            self.stop_all()
        self.set_stage("failed", reason if not self.leftovers else f"{reason}; still held: {'; '.join(self.leftovers)}")

    # ---- the end ----------------------------------------------------------------------------------

    def stop_all(self) -> None:
        """`make down`, for this launch only: the situation runner first (it stops the AI and the
        scripted operators and writes its summary), then the stack, then the ports must be free."""
        self.set_stage("stopping", "ending: stopping what it started")
        proc = self.proc
        if proc is not None and self.choice.kind == "situation" and proc.poll() is None:
            self.set_stage("stopping", "ending: the situation runner stops the AI and the scripted operators")
            self.note_up_pid()
            if not terminate(proc, RUNNER_STOP_S):
                self.leftovers.append(f"the situation runner did not stop in {RUNNER_STOP_S:.0f} s (killed)")
        self.note_up_pid()
        if self.up_pid is not None and alive(*self.up_pid):
            self.set_stage("stopping", "ending: stopping the stack (as make down)")
            try:
                os.kill(self.up_pid[0], signal.SIGTERM)
            except ProcessLookupError:
                pass
            deadline = time.monotonic() + STACK_STOP_S
            while alive(*self.up_pid) and time.monotonic() < deadline:
                time.sleep(0.5)
            if alive(*self.up_pid):
                self.leftovers.append(f"up.sh (pid {self.up_pid[0]}) did not stop in {STACK_STOP_S:.0f} s: `make down`")
        if proc is not None and proc.poll() is None:
            # the open world's make, which ends with up.sh
            terminate(proc, 15)
        if self.stack_started:
            # SITL instances and the stand-ins stop a moment after up.sh (swarm.py signals them)
            self.set_stage("stopping", "ending: waiting for the stack's ports to be free")
            skip = ("web", "tidewater") if self.tidewater_reused else ("web",)
            deadline = time.monotonic() + PORTS_FREE_S
            held = port_problems(self.ports, self.expected, self.sitl, skip=skip)
            while held and time.monotonic() < deadline:
                time.sleep(1.0)
                held = port_problems(self.ports, self.expected, self.sitl, skip=skip)
            self.leftovers += held
        with self.launcher.lock:
            if self.launcher.relay.target == self.ports["http"] and self.launcher.current is self:
                self.launcher.relay.target = None


# ---- the launcher -----------------------------------------------------------------------------------


class Launcher:
    def __init__(self, console_port: int, api_port: int, log_dir: Path):
        self.console_port = console_port
        self.api_port = api_port
        self.log_dir = log_dir
        self.lock = threading.RLock()
        self.current: Launch | None = None
        self.relay = Relay(api_port + 1)
        self.situations: dict[str, Situation] = {}
        self.entries = catalog(loaded=self.situations)
        # whether the AI CLIs are installed (the runner checks the login itself, before anything starts)
        self.ai_cli = {ai: shutil.which(ai) is not None for ai in ("claude", "codex")}
        self.notes = log_dir / "launcher.log"

    def note(self, text: str) -> None:
        line = f"{dt.datetime.now(dt.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')} {text}"
        try:
            with self.notes.open("a") as f:
                f.write(line + "\n")
        except OSError:
            pass
        say(text)

    def state(self) -> dict[str, Any]:
        with self.lock:
            return {
                "launcher": {"console_port": self.console_port, "api_port": self.api_port},
                "scenarios": self.entries,
                "ai_cli": self.ai_cli,
                "current": self.current.public() if self.current is not None else None,
                "now": round(time.time(), 3),
            }

    def start(self, body: Any) -> tuple[int, dict[str, Any]]:
        choice = validate_start(body, self.entries, self.situations)
        entry = next(e for e in self.entries if e["id"] == choice.scenario)
        with self.lock:
            cur = self.current
            previous = None
            if cur is not None and cur.active():
                if not choice.replace:
                    return 409, {"detail": f"{cur.title} is running: end it first (or start with replace)", "current": cur.public()}
                cur.request_end()
                previous = cur
            launch = Launch(self, choice, entry, previous)
            self.current = launch
        launch.thread.start()
        return 202, {"current": launch.public()}

    def end(self) -> tuple[int, dict[str, Any]]:
        with self.lock:
            cur = self.current
            if cur is None or not cur.active():
                return 409, {"detail": "nothing is running"}
            cur.request_end()
            return 202, {"current": cur.public()}

    def shutdown(self) -> None:
        cur = self.current
        if cur is not None and cur.active():
            say(f"ending {cur.title} before the launcher stops (as make down)")
            cur.request_end()
            cur.thread.join(timeout=RUNNER_STOP_S + STACK_STOP_S + PORTS_FREE_S + 30)


class Api(BaseHTTPRequestHandler):
    server_version = "murmur-launcher"
    launcher: Launcher  # set on the class by serve()

    def reply(self, code: int, body: dict[str, Any]) -> None:
        data = json.dumps(body, default=str).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self) -> None:  # noqa: N802 (the stdlib's name)
        if self.path.split("?", 1)[0] == "/launcher/api/state":
            self.reply(200, self.launcher.state())
        else:
            self.reply(404, {"detail": "not found"})

    def do_POST(self) -> None:  # noqa: N802
        path = self.path.split("?", 1)[0]
        if path not in ("/launcher/api/start", "/launcher/api/end"):
            return self.reply(404, {"detail": "not found"})
        why = refusal("POST", self.headers, self.launcher.console_port)
        if why:
            return self.reply(403, {"detail": why})
        try:
            length = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            length = -1
        if not 0 <= length <= 4096:
            return self.reply(413 if length > 4096 else 400, {"detail": "a small JSON body"})
        try:
            body = json.loads(self.rfile.read(length) or b"{}")
        except ValueError:
            return self.reply(400, {"detail": "the body is not JSON"})
        try:
            code, out = self.launcher.start(body) if path.endswith("/start") else self.launcher.end()
        except BadRequest as e:
            code, out = 400, {"detail": str(e)}
        except Exception as e:  # noqa: BLE001 (said to the menu, and in the launcher's log)
            self.launcher.note(f"{path} failed: {e!r}")
            code, out = 500, {"detail": f"the launcher failed ({type(e).__name__}); see logs/launcher/launcher.log"}
        self.reply(code, out)

    def log_message(self, format: str, *args: Any) -> None:  # noqa: A002 (the stdlib's name)
        pass


# ---- main -------------------------------------------------------------------------------------------


def start_console(console_port: int, api_port: int, log_dir: Path) -> subprocess.Popen:
    """The console's Vite dev server for the session, its /api and /ws through the bridge relay
    and /launcher to this launcher (web/vite.config.ts). In the launcher's process group, so a
    Ctrl-C in the terminal stops it too."""
    vite = REPO / "web" / "node_modules" / ".bin" / "vite"
    if not vite.exists():
        raise LaunchError(f"no {rel(vite)}: run `make setup` (npm install in web/)")
    env = {
        **{k: v for k, v in os.environ.items() if k not in MAKE_VARS},
        "MURMUR_BRIDGE_URL": f"http://127.0.0.1:{api_port + 1}",
        "MURMUR_LAUNCHER_URL": f"http://127.0.0.1:{api_port}",
    }
    with (log_dir / "web.log").open("w") as f:
        proc = subprocess.Popen(
            [str(vite), "--host", "127.0.0.1", "--port", str(console_port), "--strictPort"],
            cwd=str(REPO / "web"), env=env, stdout=f, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
        )
    deadline = time.monotonic() + 60
    while not tcp_answers(console_port):
        if proc.poll() is not None or time.monotonic() > deadline:
            terminate(proc, 5)
            raise LaunchError(f"the console did not start on :{console_port}; see {rel(log_dir / 'web.log')}")
        time.sleep(0.3)
    return proc


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="python -m eval.launcher", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--port", type=int, default=int(os.environ.get("MURMUR_LAUNCHER_PORT") or LAUNCHER_PORT),
                    help="the launcher's API (default 8760, MURMUR_LAUNCHER_PORT); its bridge relay is the next port")
    args = ap.parse_args(argv)
    console_port = int(os.environ.get("MURMUR_WEB_PORT") or CONSOLE_PORT)
    taken = [f"{what} :{p}" for what, p in (("the console", console_port), ("the launcher", args.port), ("its bridge relay", args.port + 1)) if tcp_answers(p)]
    if taken:
        print(
            f"launcher: something already answers on {', '.join(taken)}: a launcher already running (open "
            f"http://localhost:{console_port}), or a stack started with make up or make goldengate (`make down` stops it)",
            file=sys.stderr,
        )
        return 2
    log_dir = REPO / "logs" / "launcher"
    log_dir.mkdir(parents=True, exist_ok=True)
    say("reading the scenarios")
    launcher = Launcher(console_port, args.port, log_dir)
    stop = threading.Event()

    def on_signal(sig: int, _frame: Any) -> None:
        stop.set()

    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, on_signal)
    Api.launcher = launcher
    api = ThreadingHTTPServer(("127.0.0.1", args.port), Api)
    api.daemon_threads = True
    threading.Thread(target=api.serve_forever, name="api", daemon=True).start()
    threading.Thread(target=launcher.relay.serve, name="relay", daemon=True).start()
    pidfile = REPO / "logs" / "launcher.pid"
    pidfile.write_text(f"{os.getpid()}\n")
    vite: subprocess.Popen | None = None
    try:
        vite = start_console(console_port, args.port, log_dir)
        runnable = [e["title"] for e in launcher.entries if e["runnable"]]
        say(f"{len(runnable)} scenarios: {'; '.join(runnable)}")
        say(f"open http://localhost:{console_port} for the menu (Ctrl-C ends the running scenario and stops the launcher)")
        while not stop.wait(0.5):
            if vite.poll() is not None:
                say(f"the console's server stopped; see {rel(log_dir / 'web.log')}")
                break
    except LaunchError as e:
        print(f"launcher: {e}", file=sys.stderr)
        return 1
    finally:
        launcher.shutdown()
        if vite is not None:
            terminate(vite, 10)
        api.shutdown()
        launcher.relay.close()
        try:
            pidfile.unlink()
        except OSError:
            pass
        say("stopped")
    return 0


if __name__ == "__main__":
    sys.exit(main())
