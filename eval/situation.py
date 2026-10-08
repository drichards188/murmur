"""Situations: pre-built, pre-tasked scenarios started with one command (`make situation
NAME=<name>`), to watch the C2 work and an AI respond, with a person at the console or a
scripted operator on autoplay. The format is documented in `sim/situations/README.md`; the
runner is `eval/situation_run.py`. This module is the format and the runner's pure pieces
(the status document, the AI's command line and preflight, the port check, the audit tail),
so they can be tested without a stack.

A situation extends the evaluation's spec (`eval/spec.py`): the same fleet, contacts, events
and policy files, the same `[[step]]` kinds run by the same executor (`eval/storyline.py`),
plus what a watch needs: a brief for the operator and the AI, director's notes shown only when
revealed, the operator and AI modes, opening orders that put the fleet on station, and "what
to watch for" items for the console.

Everything a situation shows is display: the status document goes to the bridge's
`/api/sim/situation` and is never a decision input (crates/murmur-bridge/src/situation.rs).
"""

from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable

from . import REPO
from .spec import Step, _check_step

SITUATIONS = REPO / "sim" / "situations"
OPERATOR_MODES = ("live", "autoplay")
AI_MODES = ("claude", "codex", "playbook", "none")
AI_LABEL = {
    "claude": "Claude",
    "codex": "ChatGPT (Codex)",
    "playbook": "Playbook (deterministic)",
    "none": "None",
}
COSIGNERS = ("you", "scripted")
ORDER_TASKS = ("orbit", "patrol", "hold", "rtl", "track")
STORYLINE_PROPOSERS = ("agent", "operator")

# Who is who in a situation's dev run. op-1 is the console's operator (a person in live mode,
# the scripted operator in autoplay); op-2 and op-3 hold engagement authority with op-1 and
# sign as scripted operators in autoplay (or cosign); op-planner is a second station a
# storyline may propose from. The AI is agent-watch (Claude or Codex) or agent-planner (the
# playbook); storyline proposals made through MCP come from agent-scripted.
CONSOLE_OPERATOR = "op-1"
SCRIPTED_SIGNERS = ["op-2", "op-3"]
STORYLINE_OPERATOR = "op-planner"
OPERATORS = [CONSOLE_OPERATOR, *SCRIPTED_SIGNERS, STORYLINE_OPERATOR]
ENGAGE_OPERATORS = [CONSOLE_OPERATOR, *SCRIPTED_SIGNERS]
WATCH_AGENT = "agent-watch"
PLAYBOOK_AGENT = "agent-planner"
STORYLINE_AGENT = "agent-scripted"
AGENTS = [WATCH_AGENT, PLAYBOOK_AGENT, STORYLINE_AGENT]

TOP_KEYS = {
    "title", "brief", "director", "todo", "fleet", "contacts", "events", "policy", "duration_s",
    "rate", "at", "tidewater", "operator", "ai", "cosigner", "reveal", "seed", "ready_timeout_s", "ai_start_s",
    "order", "step", "watch", "playbook", "storyline", "autoplay",
}
ORDER_KEYS = {"id", "task", "vehicles", "center", "radius_m", "route", "target", "reserve_pct", "duration_s", "note"}
PLAYBOOK_KEYS = {"near", "radius_km", "every_s", "platform"}
TIDEWATER_HOME = Path(os.environ.get("TIDEWATER_HOME", Path.home() / "tidewater"))


class SituationError(ValueError):
    pass


@dataclass
class Order:
    """An opening order: an ordinary operator order (op-1, over the operator API) issued once
    the fleet is ready, so the vehicles start on station."""

    id: str
    task: str
    vehicles: Any
    params: dict[str, Any]
    note: str = ""


@dataclass
class Watch:
    at_s: float
    text: str


@dataclass
class Situation:
    path: Path
    name: str
    title: str
    brief: str
    director: str
    todo: bool
    fleet: Path | None = None
    contacts: Path | None = None
    events: list[Path] = field(default_factory=list)
    policy: Path | None = None
    duration_s: float = 600.0
    rate: float = 1.0
    at: str = "2026-06-14T10:00:00Z"
    tidewater: Path | None = None
    tidewater_wanted: str = "none"
    operator: str = "live"
    ai: str = "claude"
    cosigner: str = "you"
    reveal: bool = False
    seed: int = 1
    ready_timeout_s: float = 180.0
    ai_start_s: float = 30.0
    orders: list[Order] = field(default_factory=list)
    steps: list[Step] = field(default_factory=list)
    watch: list[Watch] = field(default_factory=list)
    playbook: dict[str, Any] = field(default_factory=dict)
    storyline_proposer: str = "agent"
    autoplay: dict[str, Any] = field(default_factory=dict)
    raw: dict[str, Any] = field(default_factory=dict)


def _path(v: Any, base: Path, what: str, sit: Path) -> Path:
    if not isinstance(v, str) or not v.strip():
        raise SituationError(f"{sit}: {what} must be a file path")
    p = Path(v).expanduser()
    if not p.is_absolute():
        p = (REPO / p) if (REPO / p).exists() or not (base / p).exists() else base / p
    if not p.exists():
        raise SituationError(f"{sit}: {what} {v!r} not found")
    return p.resolve()


def find_situation(name: str) -> Path:
    """A situation by name (`sim/situations/<name>.toml`) or path."""
    p = Path(name).expanduser()
    if p.suffix == ".toml" and p.exists():
        return p.resolve()
    cand = SITUATIONS / f"{name}.toml"
    if cand.exists():
        return cand
    known = ", ".join(sorted(f.stem for f in SITUATIONS.glob("*.toml")))
    raise SituationError(f"no situation {name!r}; in {SITUATIONS}: {known}")


def _lonlat(v: Any) -> bool:
    return isinstance(v, list) and len(v) == 2 and all(isinstance(x, (int, float)) for x in v)


def _order(i: int, raw: dict[str, Any], sit: Path) -> Order:
    unknown = set(raw) - ORDER_KEYS
    if unknown:
        raise SituationError(f"{sit}: order {i + 1}: unknown keys {sorted(unknown)}")
    oid = str(raw.get("id", f"order-{i + 1}"))
    task = raw.get("task")
    if task not in ORDER_TASKS:
        raise SituationError(f"{sit}: order {oid!r}: task {task!r} not in {ORDER_TASKS}")
    if "vehicles" not in raw:
        raise SituationError(f"{sit}: order {oid!r}: needs vehicles ({{callsign = ...}}, {{platform = ...}} or {{uids = [...]}})")
    params = {k: raw[k] for k in ("center", "radius_m", "route", "target", "reserve_pct", "duration_s") if k in raw}
    if task == "orbit":
        if not _lonlat(params.get("center")) or not isinstance(params.get("radius_m"), (int, float)) or params["radius_m"] <= 0:
            raise SituationError(f"{sit}: order {oid!r}: an orbit needs center = [lon, lat] and radius_m > 0")
    if task == "patrol":
        route = params.get("route")
        if not isinstance(route, list) or len(route) < 2 or not all(_lonlat(p) for p in route):
            raise SituationError(f"{sit}: order {oid!r}: a patrol needs route = [[lon, lat], ...] (two points or more)")
    if "center" in params and not _lonlat(params["center"]):
        raise SituationError(f"{sit}: order {oid!r}: center is [lon, lat] (opening orders name places, not tracks)")
    return Order(id=oid, task=str(task), vehicles=raw["vehicles"], params=params, note=str(raw.get("note", "")))


def _steps(raw: list[Any], sit: Path) -> list[Step]:
    """The timeline: the storyline's steps (eval/storyline.py), checked as the evaluation
    checks them. A situation proposal carries no truth label, so the evaluation's `reason`
    may be left out; `text` (what the panel says when the step is done) and `director` (shown
    only when revealed) are the situation's own."""
    steps: list[Step] = []
    seen: set[str] = set()
    for i, s in enumerate(raw):
        if not isinstance(s, dict):
            raise SituationError(f"{sit}: step {i + 1} is not a table")
        s = dict(s)
        sid = str(s.pop("id", f"step-{i + 1}"))
        if sid in seen:
            raise SituationError(f"{sit}: duplicate step id {sid!r}")
        seen.add(sid)
        kind = str(s.pop("kind", "note"))
        at_s = float(s.pop("at_s", 0))
        if "director" in s and not isinstance(s["director"], bool):
            raise SituationError(f"{sit}: step {sid!r}: director is true or false")
        check = dict(s)
        if kind == "propose":
            check.setdefault("reason", "a situation step: no truth label")
        if kind == "flood":
            check["templates"] = [{"reason": "a situation step: no truth label", **t} for t in s.get("templates", [])]
        if any("contact" in str(v) for k, v in s.items() if k in ("center", "route", "track")):
            raise SituationError(
                f"{sit}: step {sid!r}: a situation resolves tracks as the C2 sees them ({{id = ...}} or "
                "{select = ...}); contact selectors need the evaluation's truth hook, which a situation leaves off"
            )
        try:
            _check_step(Step(id=sid, kind=kind, at_s=at_s, params=check))
        except ValueError as e:
            raise SituationError(f"{sit}: {e}") from e
        steps.append(Step(id=sid, kind=kind, at_s=at_s, params=s))
    steps.sort(key=lambda s: s.at_s)
    return steps


def tidewater_scenario(v: Any) -> tuple[str, Path | None]:
    """`tidewater = "<scenario>"` (a name under ~/tidewater/scenarios, or a path) or "none":
    what was asked and the file, when it exists here."""
    if v is None or v == "none":
        return "none", None
    name = str(v)
    p = Path(name).expanduser()
    if p.suffix != ".toml":
        p = TIDEWATER_HOME / "scenarios" / f"{name}.toml"
    return name, (p if p.exists() else None)


def load_situation(path_or_name: str | Path) -> Situation:
    path = find_situation(str(path_or_name))
    try:
        raw = tomllib.loads(path.read_text())
    except (OSError, tomllib.TOMLDecodeError) as e:
        raise SituationError(f"{path}: {e}") from e
    unknown = set(raw) - TOP_KEYS
    if unknown:
        raise SituationError(f"{path}: unknown keys {sorted(unknown)} (sim/situations/README.md lists them)")
    title = str(raw.get("title", "")).strip()
    brief = str(raw.get("brief", "")).strip()
    if not title or not brief:
        raise SituationError(f"{path}: needs a title and a brief")
    base = path.parent
    sit = Situation(
        path=path,
        name=path.stem,
        title=title,
        brief=brief,
        director=str(raw.get("director", "")).strip(),
        todo=bool(raw.get("todo", False)),
        raw=raw,
    )
    if sit.todo:
        # a stub: title and brief only, written later; nothing else is read
        return sit
    sit.fleet = _path(raw.get("fleet"), base, "fleet", path)
    if raw.get("contacts"):
        sit.contacts = _path(raw["contacts"], base, "contacts", path)
    events = raw.get("events", [])
    if isinstance(events, str):
        events = [events]
    if not isinstance(events, list):
        raise SituationError(f"{path}: events is a file or a list of files")
    sit.events = [_path(e, base, "events", path) for e in events]
    sit.policy = _path(raw.get("policy", "sim/policy.toml"), base, "policy", path)
    sit.duration_s = float(raw.get("duration_s", 600))
    if not sit.duration_s > 0:
        raise SituationError(f"{path}: duration_s must be > 0")
    sit.rate = float(raw.get("rate", 1))
    if not sit.rate > 0:
        raise SituationError(f"{path}: rate must be > 0")
    sit.at = str(raw.get("at", sit.at))
    sit.tidewater_wanted, sit.tidewater = tidewater_scenario(raw.get("tidewater", "none"))
    sit.operator = str(raw.get("operator", "live"))
    if sit.operator not in OPERATOR_MODES:
        raise SituationError(f"{path}: operator {sit.operator!r} not in {OPERATOR_MODES}")
    sit.ai = str(raw.get("ai", "claude"))
    if sit.ai not in AI_MODES:
        raise SituationError(f"{path}: ai {sit.ai!r} not in {AI_MODES}")
    sit.cosigner = str(raw.get("cosigner", "you"))
    if sit.cosigner not in COSIGNERS:
        raise SituationError(f"{path}: cosigner {sit.cosigner!r} not in {COSIGNERS}")
    if not isinstance(raw.get("reveal", False), bool):
        raise SituationError(f"{path}: reveal is true or false")
    sit.reveal = bool(raw.get("reveal", False))
    sit.seed = int(raw.get("seed", 1))
    sit.ready_timeout_s = float(raw.get("ready_timeout_s", 180))
    sit.ai_start_s = float(raw.get("ai_start_s", 30))
    if sit.ai_start_s < 0 or sit.ai_start_s >= sit.duration_s:
        raise SituationError(f"{path}: ai_start_s must be >= 0 and before the end")
    orders = raw.get("order", [])
    sit.orders = [_order(i, o, path) for i, o in enumerate(orders)]
    ids = [o.id for o in sit.orders]
    if len(set(ids)) != len(ids):
        raise SituationError(f"{path}: duplicate order ids")
    sit.steps = _steps(raw.get("step", []), path)
    for i, w in enumerate(raw.get("watch", [])):
        if not isinstance(w, dict) or not str(w.get("text", "")).strip() or float(w.get("at_s", -1)) < 0:
            raise SituationError(f"{path}: watch {i + 1} needs at_s >= 0 and text")
        sit.watch.append(Watch(at_s=float(w["at_s"]), text=str(w["text"]).strip()))
    sit.watch.sort(key=lambda w: w.at_s)
    pb = dict(raw.get("playbook", {}))
    if set(pb) - PLAYBOOK_KEYS:
        raise SituationError(f"{path}: playbook keys are {sorted(PLAYBOOK_KEYS)}")
    if "near" in pb and not _lonlat(pb["near"]):
        raise SituationError(f"{path}: playbook near is [lon, lat]")
    for k in ("radius_km", "every_s"):
        if k in pb and not float(pb[k]) > 0:
            raise SituationError(f"{path}: playbook {k} must be > 0")
    sit.playbook = pb
    story = dict(raw.get("storyline", {}))
    sit.storyline_proposer = str(story.get("proposer", "agent"))
    if sit.storyline_proposer not in STORYLINE_PROPOSERS or set(story) - {"proposer"}:
        raise SituationError(f"{path}: [storyline] proposer is one of {STORYLINE_PROPOSERS}")
    sit.autoplay = dict(raw.get("autoplay", {}))
    return sit


def resolve_modes(sit: Situation, operator: str | None = None, ai: str | None = None, cosigner: str | None = None, reveal: bool | None = None) -> dict[str, Any]:
    """The run's modes: the file's, with the command line's overrides."""
    op = operator or sit.operator
    a = ai or sit.ai
    co = cosigner or sit.cosigner
    if op not in OPERATOR_MODES:
        raise SituationError(f"operator {op!r}: one of {', '.join(OPERATOR_MODES)}")
    if a not in AI_MODES:
        raise SituationError(f"AI {a!r}: one of {', '.join(AI_MODES)}")
    if co not in COSIGNERS:
        raise SituationError(f"cosigner {co!r}: one of {', '.join(COSIGNERS)}")
    rv = sit.reveal if reveal is None else bool(reveal)
    # who acts as whom: autoplay scripts op-1 and the two signers; live leaves op-1 to the
    # person, and the scripted signers only cosign when asked to
    scripted = [CONSOLE_OPERATOR, *SCRIPTED_SIGNERS] if op == "autoplay" else (list(SCRIPTED_SIGNERS) if co == "scripted" else [])
    return {
        "operator": op,
        "ai": a,
        "ai_label": AI_LABEL[a],
        "cosigner": co,
        "reveal": rv,
        "console_operator": CONSOLE_OPERATOR,
        "scripted_operators": scripted,
        "agent": {"claude": WATCH_AGENT, "codex": WATCH_AGENT, "playbook": PLAYBOOK_AGENT}.get(a),
    }


# ---- the status document ---------------------------------------------------------------------


def step_text(rec: dict[str, Any], step: Step | None) -> str:
    """One line for the panel from a storyline record (eval/storyline.py) and its step."""
    kind = rec.get("kind")
    headline = str((step.params.get("text") if step else "") or "")
    if kind == "note":
        detail = str(rec.get("text", ""))
    elif kind == "propose":
        req = rec.get("request") or {}
        who = (rec.get("vehicle") or {}).get("callsign") or ", ".join(str(u) for u in req.get("uids", []))
        state = f"error: {rec['error']}" if rec.get("error") else rec.get("state")
        detail = f"{req.get('task', '?')} for {who} proposed by {(rec.get('source') or {}).get('id', '?')}: intent {rec.get('intent_id')} {state}"
    elif kind == "link":
        eff = rec.get("effect") or {}
        what = "link cut" if eff.get("cut") else ("loss " + str(eff.get("loss")) if eff.get("loss") is not None else f"+{eff.get('extra_latency_ms')} ms")
        detail = f"{what} on {', '.join(str(c) for c in rec.get('callsigns') or [])}"
    elif kind == "declare":
        detail = f"{rec.get('operator')} declared {(rec.get('target') or {}).get('track_id')} {rec.get('affiliation')}"
    elif kind == "skipped":
        detail = f"step {rec.get('step')} skipped: {rec.get('reason')}"
    elif kind == "error":
        detail = f"step {rec.get('step')} failed: {rec.get('error')}"
    else:
        detail = json.dumps({k: v for k, v in rec.items() if k not in ("t",)})[:200]
    if headline and detail and detail != headline:
        return f"{headline} ({detail})"
    return headline or detail


def status_doc(
    sit: Situation,
    modes: dict[str, Any],
    *,
    phase: str,
    phase_detail: str = "",
    elapsed_s: float | None,
    now: float,
    entries: list[dict[str, Any]],
    done_steps: set[str],
    orders: list[dict[str, Any]],
    agent: dict[str, Any],
    autoplay: dict[str, Any] | None,
    run: dict[str, Any],
) -> dict[str, Any]:
    """The document the runner posts to `/api/sim/situation` (display only). With `reveal`
    false it carries no director's notes, no timeline still to come and no entry a step
    marked `director = true`: nothing a blinded operator should not see."""
    reveal = bool(modes.get("reveal"))
    el = None if elapsed_s is None else round(max(0.0, elapsed_s), 1)
    shown = [e for e in entries if reveal or not e.get("director")]
    doc: dict[str, Any] = {
        "schema": 1,
        "name": sit.name,
        "title": sit.title,
        "brief": sit.brief,
        "reveal": reveal,
        "modes": {k: modes[k] for k in ("operator", "ai", "ai_label", "cosigner", "console_operator", "scripted_operators")},
        "phase": phase,
        "phase_detail": phase_detail,
        "clock": {
            "t": round(now, 3),
            "elapsed_s": el,
            "duration_s": sit.duration_s,
            "remaining_s": None if el is None else round(max(0.0, sit.duration_s - el), 1),
        },
        "orders": orders,
        "timeline": {"done": shown[-40:]},
        "watch": [{"at_s": w.at_s, "text": w.text} for w in sit.watch if el is not None and w.at_s <= el],
        "agent": agent,
        "autoplay": autoplay,
        "run": run,
    }
    if reveal:
        doc["director"] = sit.director
        doc["timeline"]["upcoming"] = [
            {"at_s": s.at_s, "id": s.id, "kind": s.kind, "text": str(s.params.get("text") or s.kind)}
            for s in sit.steps
            if s.id not in done_steps
        ]
        doc["watch_upcoming"] = [{"at_s": w.at_s, "text": w.text} for w in sit.watch if el is None or w.at_s > el]
    return doc


# ---- the AI ----------------------------------------------------------------------------------

FIX_LOGIN = {
    "claude": "run `claude`, then `/login` in it (your own Claude subscription or API key; Murmur never sees it)",
    "codex": "run `codex login` (your own ChatGPT plan or API key; Murmur never sees it)",
}
INSTALL = {
    "claude": "install Claude Code (https://docs.anthropic.com/en/docs/claude-code), then run `claude` and `/login`",
    "codex": "install the Codex CLI (`npm install -g @openai/codex`), then run `codex login`",
}


def ai_preflight(ai: str, run: Callable[..., Any] = subprocess.run, which: Callable[[str], str | None] = shutil.which) -> dict[str, Any]:
    """Before the stack starts: the chosen AI's CLI is installed and logged in, or a clear
    refusal saying how to fix it (and that AI=playbook is the free option). Murmur reads no
    credential: it asks the user's own CLI whether it is logged in and keeps only yes or no
    and the kind of login (never an account, an email or a key)."""
    if ai in ("playbook", "none"):
        return {"ai": ai, "checked": False}
    free = "or run AI=playbook (deterministic, no account needed)"
    exe = which(ai)
    if not exe:
        raise SituationError(f"AI={ai}: the `{ai}` command is not installed: {INSTALL[ai]}; {free}.")
    if ai == "claude":
        try:
            r = run([exe, "auth", "status"], capture_output=True, text=True, timeout=30)
        except (OSError, subprocess.SubprocessError) as e:
            raise SituationError(f"AI=claude: `claude auth status` failed ({e}): {FIX_LOGIN['claude']}; {free}.") from None
        try:
            status = json.loads(r.stdout or "{}")
        except ValueError:
            status = {}
        method = status.get("authMethod")
        if not status.get("loggedIn") or method in (None, "", "none"):
            raise SituationError(f"AI=claude: Claude Code is not logged in: {FIX_LOGIN['claude']}; {free}.")
        return {"ai": ai, "checked": True, "cli": exe, "logged_in": True, "auth_method": str(method)}
    # codex: `codex login status` where the installed CLI has it
    try:
        r = run([exe, "login", "status"], capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.SubprocessError) as e:
        raise SituationError(f"AI=codex: `codex login status` failed ({e}): {FIX_LOGIN['codex']}; {free}.") from None
    out = f"{r.stdout}\n{r.stderr}".lower()
    if "unrecognized subcommand" in out or "unexpected argument" in out:
        return {"ai": ai, "checked": False, "cli": exe, "note": "this codex has no `login status`: the login was not checked"}
    if r.returncode != 0 or "not logged in" in out or "logged in" not in out:
        raise SituationError(f"AI=codex: the Codex CLI is not logged in: {FIX_LOGIN['codex']}; {free}.")
    method = "chatgpt" if "chatgpt" in out else ("api key" if "api key" in out else "logged in")
    return {"ai": ai, "checked": True, "cli": exe, "logged_in": True, "auth_method": method}


def ai_command(
    ai: str,
    sit: Situation,
    *,
    python: str,
    mcp_url: str,
    brief_file: Path,
    agent_log: Path,
    until: float,
    seconds: float,
) -> list[str] | None:
    """The AI's command line (the launch contract in agents/README.md), or None for none."""
    if ai in ("claude", "codex"):
        return [
            python, str(REPO / "agents" / "watch_officer.py"),
            "--backend", ai,
            "--url", mcp_url,
            "--agent", WATCH_AGENT,
            "--brief", str(brief_file),
            "--situation-log", str(agent_log),
            "--until", f"{until:.0f}",
        ]
    if ai == "playbook":
        pb = sit.playbook
        near = pb.get("near", [-122.4783, 37.8199])
        cmd = [
            python, str(REPO / "agents" / "policy_agent.py"),
            "--url", mcp_url,
            "--agent", PLAYBOOK_AGENT,
            "--watch", f"{max(1.0, seconds):.0f}",
            f"--near={near[0]},{near[1]}",  # one token: a longitude starts with a minus
            "--radius-km", f"{float(pb.get('radius_km', 4.0)):g}",
            "--every", f"{float(pb.get('every_s', 10.0)):g}",
        ]
        if pb.get("platform"):
            cmd += ["--platform", str(pb["platform"])]
        return cmd
    return None


def brief_text(sit: Situation) -> str:
    """What the AI is told: the title and the operator's brief, never the director's notes,
    the timeline or the watch items."""
    return f"# {sit.title}\n\n{sit.brief}\n"


# ---- the stack's ports -----------------------------------------------------------------------


def stack_ports(env: dict[str, str]) -> dict[str, int]:
    """The ports `scripts/up.sh` will use, from the same variables and defaults."""
    base = int(env.get("MURMUR_BASE_PORT") or 14560)
    return {
        "vehicle_base": base,
        "bridge_base": int(env.get("MURMUR_BRIDGE_BASE_PORT") or base + 1000),
        "http": int(env.get("MURMUR_HTTP_PORT") or 8765),
        "web": int(env.get("MURMUR_WEB_PORT") or 5173),
        "linkemu": int(env.get("MURMUR_LINKEMU_PORT") or 8766),
        "tidewater": int(env.get("MURMUR_TIDEWATER_PORT") or 8770),
        "sitl_instance_base": int(env.get("MURMUR_SITL_INSTANCE_BASE") or 0),
    }


def tcp_answers(port: int, host: str = "127.0.0.1", timeout: float = 0.5) -> bool:
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except OSError:
        return False


def udp_taken(port: int) -> bool:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.bind(("0.0.0.0", port))
        return False
    except OSError:
        return True
    finally:
        s.close()


def pid_alive(pidfile: Path) -> int | None:
    try:
        pid = int(pidfile.read_text().strip())
    except (OSError, ValueError):
        return None
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return None
    except PermissionError:
        return pid
    return pid


def port_problems(ports: dict[str, int], vehicles: int, sitl: int, root: Path = REPO) -> list[str]:
    """Why a stack can't start on these ports now: one already answers (or a port is held),
    or this checkout's stack is up. Never touches what holds them."""
    out: list[str] = []
    pid = pid_alive(root / "logs" / "up.pid")
    if pid is not None:
        out.append(f"a stack started from this checkout is running (logs/up.pid, pid {pid})")
    for key, var in (("http", "MURMUR_HTTP_PORT"), ("web", "MURMUR_WEB_PORT"), ("linkemu", "MURMUR_LINKEMU_PORT")):
        if tcp_answers(ports[key]):
            out.append(f"something answers on {var}={ports[key]}")
    for key, var in (("vehicle_base", "MURMUR_BASE_PORT"), ("bridge_base", "MURMUR_BRIDGE_BASE_PORT")):
        taken = [p for p in range(ports[key], ports[key] + max(1, vehicles)) if udp_taken(p)]
        if taken:
            out.append(f"UDP {', '.join(str(p) for p in taken[:5])}{' ...' if len(taken) > 5 else ''} in the {var} range ({ports[key]}+{vehicles}) is held")
    for k in range(sitl):
        p = 5760 + 10 * (ports["sitl_instance_base"] + k)
        if tcp_answers(p):
            out.append(f"SITL instance port {p} answers (MURMUR_SITL_INSTANCE_BASE={ports['sitl_instance_base']})")
    return out


# ---- the audit log, read-only ----------------------------------------------------------------


class AuditTail:
    """New complete lines of a growing JSON-lines file (the bridge's audit log, the AI's own
    log), read-only, from where the last read stopped."""

    def __init__(self, path: Path):
        self.path = path
        self.offset = 0
        self.partial = b""

    def read(self) -> list[dict[str, Any]]:
        try:
            with self.path.open("rb") as f:
                f.seek(self.offset)
                data = f.read()
        except OSError:
            return []
        self.offset += len(data)
        data = self.partial + data
        lines = data.split(b"\n")
        self.partial = lines.pop()
        out = []
        for line in lines:
            try:
                rec = json.loads(line)
            except ValueError:
                continue
            if isinstance(rec, dict):
                out.append(rec)
        return out


def agent_activity(events: list[dict[str, Any]], agent: str | None) -> list[dict[str, Any]]:
    """What the AI did, as the audit log has it: its `agent.action` events (connected,
    previewed, proposed, withdrew), each with the bridge's time (s) and the engine's detail."""
    out = []
    for e in events:
        if e.get("type") != "agent.action" or (agent and e.get("agent") != agent):
            continue
        out.append({"t": round(float(e.get("t", 0)) / 1000.0, 3), "seq": e.get("seq"), "action": e.get("action"), "detail": str(e.get("detail", ""))[:400]})
    return out


def merge_events(files: list[Path]) -> str:
    """One events file from several (link and GNSS `[[event]]` tables), for `up.sh`'s single
    `MURMUR_LINK_EVENTS`; storyline `link` steps append to it while the run goes."""
    from .stack import event_toml

    head = "# This situation run's scripted events (eval/situation_run.py): the situation's events files\n"
    head += "# merged, on the stack's clock (at_s from the link emulator's start), then storyline link steps.\n\n"
    body = []
    for f in files:
        raw = tomllib.loads(f.read_text())
        body.append(f"# from {f}\n")
        for e in raw.get("event", []):
            body.append(event_toml(dict(e)))
    return head + "".join(body)
