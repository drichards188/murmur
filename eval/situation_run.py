#!/usr/bin/env python3
"""The situation runner (`sim/situations/README.md`):

    make situation NAME=harbor-watch [OPERATOR=live|autoplay] [AI=claude|codex|playbook|none]
    .venv/bin/python -m eval.situation_run harbor-watch --operator autoplay --ai playbook

1. Reads the situation and checks, before anything starts: a dev run (`MURMUR_AUTH` unset or
   dev), the chosen AI's CLI installed and logged in (`claude auth status`, `codex login
   status`; never a credential), and no stack answering on the ports the `MURMUR_*`
   variables name (`scripts/up.sh`'s, default ports by default). It never stops another stack.
2. Makes a fresh station: removes the bridge's restore pointer (`logs/audit/.last-run-<http
   port>`) and resume pointer (`.resume-<http port>.json`) and sets `MURMUR_RESUME=0`, saying so.
3. Starts the stack with `scripts/up.sh` in its own session (the situation's fleet, contacts,
   merged events, policy, tidewater scenario, the scenario clock, a roster for the modes, this
   run's id and simulation token), so it outlives the runner: `make down` stops it.
4. Takes the stack's clock (the link emulator's start: "situation time" counts from it, as the
   events files' `at_s` and the contacts' `start_s` do), waits until every vehicle is
   navigation-ready, and issues the opening orders as op-1 over the operator API.
5. Starts the AI (agents/README.md, "Situations: the launch contract"), the autoplay operator
   (`eval/proxy.py`'s `ProxyOperator` as op-1 with `EngagementSigners` op-2 and op-3, all
   scripted) or the scripted cosigner, and the timeline (`eval/storyline.py`).
6. Every 2 s posts the status document to the bridge (`POST /api/sim/situation`, display
   only) for the console's Situation panel, and writes everything it did to
   `logs/situations/<run id>/situation.jsonl`.
7. At the end (or Ctrl-C) stops its own children (the AI, the autoplay), leaves the stack
   running for inspection, and prints a summary: the audit log, the `murmur-replay` command,
   what happened.
"""

from __future__ import annotations

import argparse
import asyncio
import datetime as dt
import json
import os
import secrets
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

from . import PYTHON, REPO
from .agent import AgentProposer, HumanProposer, ProposeError, _rest_body
from .bridge import Bridge, BridgeError
from .engage import EngageAdapter
from .ports import ephemeral_range
from .proxy import EngagementSigners, ProxyOperator, proxy_params
from .situation import (
    AGENTS,
    AI_LABEL,
    CONSOLE_OPERATOR,
    ENGAGE_OPERATORS,
    OPERATORS,
    SCRIPTED_SIGNERS,
    STORYLINE_AGENT,
    STORYLINE_OPERATOR,
    AuditTail,
    Situation,
    SituationError,
    agent_activity,
    ai_command,
    ai_preflight,
    brief_text,
    load_situation,
    merge_events,
    port_problems,
    resolve_modes,
    stack_ports,
    status_doc,
    step_text,
)
from .stack import _swarm
from .storyline import Storyline, World, select_vehicles

PUBLISH_S = 2.0
SIM_TOKEN_HEADER = "X-Murmur-Sim-Token"


def utc_stamp() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def say(msg: str) -> None:
    print(f"situation: {msg}", flush=True)


def plan_counts(fleet: Path) -> tuple[list[dict[str, Any]], int]:
    """The fleet as `sim/swarm.py` plans it (callsigns, uids in unit order) and how many
    vehicles need SITL."""
    swarm = _swarm()
    vehicles = swarm.plan(swarm.load_fleet(str(fleet)), swarm.load_platforms())
    for i, v in enumerate(vehicles):
        v["uid"] = i + 1
    return vehicles, sum(1 for v in vehicles if v.get("sim") != "standin")


class FreshAgentProposer(AgentProposer):
    """The storyline's agent (`agent-scripted`), connected for each proposal or withdrawal:
    a situation's steps can be minutes apart, and the bridge's MCP server (rmcp) closes a
    session idle for 300 s."""

    async def start(self) -> None:
        pass

    async def propose(self, req: dict[str, Any], rationale: str) -> dict[str, Any]:
        await AgentProposer.start(self)
        try:
            return await super().propose(req, rationale)
        finally:
            await AgentProposer.stop(self)

    async def withdraw(self, id: int, reason: str) -> dict[str, Any] | None:
        await AgentProposer.start(self)
        try:
            return await super().withdraw(id, reason)
        finally:
            await AgentProposer.stop(self)


class StepLog:
    """The storyline's log (eval/storyline.py writes through `write`): every record into
    situation.jsonl, and each finished step as a timeline entry for the panel."""

    def __init__(self, runner: "Runner"):
        self.runner = runner

    def write(self, rec: dict[str, Any]) -> None:
        r = self.runner
        r.record("step", **rec)
        if rec.get("kind") in ("retry", "withdraw", "phase"):
            return
        if rec.get("kind") == "skipped" and rec.get("reason") == "run ended":
            # never run: the situation ended (or was stopped) before its time
            return
        sid = str(rec.get("step", ""))
        step = r.steps.get(sid) or r.steps.get(sid.rsplit("-", 1)[0])
        if step is not None:
            r.done_steps.add(step.id)
        r.entry(
            "step",
            step_text(rec, step),
            director=bool(step is not None and step.params.get("director")),
            step=step.id if step else sid,
            kind_detail=rec.get("kind"),
        )


class Runner:
    def __init__(self, sit: Situation, modes: dict[str, Any], env: dict[str, str], preflight: dict[str, Any]):
        self.sit = sit
        self.modes = modes
        self.env = env
        self.preflight = preflight
        self.ports = stack_ports(env)
        self.run_id = f"situation-{sit.name}-{utc_stamp()}"
        self.out = REPO / "logs" / "situations" / self.run_id
        self.out.mkdir(parents=True, exist_ok=True)
        self.log_path = self.out / "situation.jsonl"
        self.audit = REPO / "logs" / "audit" / f"{self.run_id}.jsonl"
        self.anchors = REPO / "logs" / "anchors" / f"{self.run_id}.anchors"
        self.events_file = self.out / "events.toml"
        self.brief_file = self.out / "brief.md"
        self.agent_log = self.out / "agent.jsonl"
        self.token = secrets.token_hex(24)
        self.bridge_url = f"http://127.0.0.1:{self.ports['http']}"
        self.bridge = Bridge(self.bridge_url)
        self.emu_url = f"http://127.0.0.1:{self.ports['linkemu']}"
        self.linkemu = (env.get("MURMUR_LINKEMU") or "1").lower() not in ("0", "off", "false", "no")
        self.steps = {s.id: s for s in sit.steps}
        self.done_steps: set[str] = set()
        self.entries: list[dict[str, Any]] = []
        self.orders: list[dict[str, Any]] = []
        self.zero: float | None = None
        self.phase = "starting"
        self.phase_detail = "starting the stack"
        self.up: subprocess.Popen | None = None
        self.ai_proc: subprocess.Popen | None = None
        self.ai_status: dict[str, Any] = {"status": "not started"}
        self.autoplay_log: list[dict[str, Any]] = []
        self.autoplay_seen = 0
        self.activity: list[dict[str, Any]] = []
        self.ai_notes: list[dict[str, Any]] = []
        self.audit_tail = AuditTail(self.audit)
        self.notes_tail = AuditTail(self.agent_log)
        self.publish_errors = 0
        self.stop = asyncio.Event()
        self.interrupted = False
        self.ready: dict[str, Any] = {}

    # ---- records -----------------------------------------------------------------------------

    def elapsed(self, t: float | None = None) -> float | None:
        if self.zero is None:
            return None
        return (t if t is not None else time.time()) - self.zero

    def record(self, event: str, **fields: Any) -> None:
        rec = {"t": round(time.time(), 3), "t_s": None if self.zero is None else round(self.elapsed() or 0.0, 1), "event": event, **fields}
        with self.log_path.open("a") as f:
            f.write(json.dumps(rec, default=str) + "\n")

    def entry(self, kind: str, text: str, director: bool = False, **extra: Any) -> None:
        e = {"t_s": round(self.elapsed() or 0.0, 1), "kind": kind, "text": text, "director": director, **extra}
        self.entries.append(e)
        if kind != "step":
            self.record("timeline", **e)

    def set_phase(self, phase: str, detail: str) -> None:
        self.phase, self.phase_detail = phase, detail
        self.record("phase", phase=phase, detail=detail)

    # ---- the status document -----------------------------------------------------------------

    def doc(self) -> dict[str, Any]:
        agent = {
            "kind": self.modes["ai"],
            "label": AI_LABEL[self.modes["ai"]],
            "id": self.modes["agent"],
            **self.ai_status,
            "activity": self.activity[-12:],
            "notes": self.ai_notes[-8:],
        }
        autoplay = None
        if self.modes["scripted_operators"]:
            autoplay = {
                "operators": self.modes["scripted_operators"],
                "mode": "autoplay" if self.modes["operator"] == "autoplay" else "cosigner",
                "scripted": True,
                "note": "scripted operators, not people: a model (eval/proxy.py), never evidence",
                "decisions": [{k: d.get(k) for k in ("t", "intent_id", "action", "by", "reason", "result")} for d in self.autoplay_log[-10:]],
            }
        return status_doc(
            self.sit,
            self.modes,
            phase=self.phase,
            phase_detail=self.phase_detail,
            elapsed_s=self.elapsed(),
            now=time.time(),
            entries=self.entries,
            done_steps=self.done_steps,
            orders=self.orders,
            agent=agent,
            autoplay=autoplay,
            run={"id": self.run_id, "audit_log": str(self.audit.relative_to(REPO)), "situation_log": str(self.log_path.relative_to(REPO))},
        )

    def post_doc(self, method: str = "POST") -> bool:
        data = json.dumps(self.doc(), default=str).encode() if method == "POST" else None
        req = urllib.request.Request(
            self.bridge_url + "/api/sim/situation",
            method=method,
            data=data,
            headers={"Content-Type": "application/json", SIM_TOKEN_HEADER: self.token},
        )
        try:
            with urllib.request.urlopen(req, timeout=5) as r:
                r.read()
            return True
        except (urllib.error.URLError, OSError) as e:
            self.publish_errors += 1
            if self.publish_errors == 1:
                self.record("publish_error", error=str(e))
            return False

    def refresh(self) -> None:
        """Fold in what happened since the last look: the AI's actions in the audit log, its
        own notes, the autoplay's decisions, the AI process."""
        # the AI's own actions only (no AI, none: the storyline's agent is not the AI)
        events = self.audit_tail.read()
        new = agent_activity(events, self.modes["agent"]) if self.modes["agent"] else []
        for a in new:
            # on the situation's clock, for the panel
            a["t_s"] = round(a["t"] - self.zero, 1) if self.zero is not None else None
            self.record("ai_action", **a)
        self.activity.extend(new)
        for n in self.notes_tail.read():
            t = n.get("t") if isinstance(n.get("t"), (int, float)) else None
            note = {
                "t": t,
                "t_s": round(t - self.zero, 1) if t is not None and self.zero is not None else None,
                "kind": str(n.get("kind", "note")),
                "text": str(n.get("text", ""))[:400],
            }
            self.ai_notes.append(note)
            self.record("ai_note", **note)
        while self.autoplay_seen < len(self.autoplay_log):
            d = self.autoplay_log[self.autoplay_seen]
            self.autoplay_seen += 1
            self.record("autoplay", **d)
            verb = {"approve": "approved", "reject": "rejected", "sign": "signed"}.get(str(d.get("action")))
            if verb:
                by = d.get("by") or CONSOLE_OPERATOR
                self.entry("autoplay", f"{by} (scripted) {verb} intent {d.get('intent_id')}: {d.get('result')}")
        if self.ai_proc is not None and self.ai_status.get("status") == "running":
            code = self.ai_proc.poll()
            if code is not None:
                self.ai_status["status"] = f"exited {code}"
                self.ai_status["ended_t"] = round(time.time(), 3)
                self.record("ai_exit", code=code)
                self.entry("ai", f"{AI_LABEL[self.modes['ai']]} exited ({code}); the situation goes on")

    async def publisher(self) -> None:
        while True:
            self.refresh()
            if await asyncio.to_thread(self.bridge.alive):
                await asyncio.to_thread(self.post_doc)
            try:
                await asyncio.wait_for(self.stop.wait(), timeout=PUBLISH_S)
                return
            except asyncio.TimeoutError:
                pass

    # ---- the stack ---------------------------------------------------------------------------

    def fresh_station(self) -> list[str]:
        """No restore pointer and no resume pointer for this port (eval/stack.py does the
        same): the station starts as new, owing nothing to an earlier run."""
        removed = []
        audit_dir = REPO / "logs" / "audit"
        for name in (f".last-run-{self.ports['http']}", f".resume-{self.ports['http']}.json"):
            p = audit_dir / name
            if p.exists() or p.is_symlink():
                try:
                    named = p.read_text().strip()[:300]
                except (OSError, ValueError):
                    named = "(unreadable)"
                p.unlink()
                removed.append(f"{p.relative_to(REPO)} (named {named})")
        self.record("fresh_station", removed=removed, resume="MURMUR_RESUME=0")
        return removed

    def stack_env(self) -> dict[str, str]:
        sit = self.sit
        env = {k: v for k, v in self.env.items() if k not in ("MURMUR_SIM_TOKEN", "MURMUR_EVAL_TRUTH", "MURMUR_EVENTS", "MURMUR_STUDY", "MURMUR_STUDY_PASSWORD_STEP_UP", "MURMUR_CREDENTIALS", "MURMUR_CONTACTS", "MURMUR_SCENARIO")}
        env.update(
            {
                "MURMUR_AUTH": "dev",
                "MURMUR_OPERATORS": ",".join(OPERATORS),
                "MURMUR_ENGAGE_OPERATORS": ",".join(ENGAGE_OPERATORS),
                "MURMUR_AGENTS": ",".join(AGENTS),
                "MURMUR_RUN_ID": self.run_id,
                "MURMUR_RESUME": "0",
                "MURMUR_POLICY": str(sit.policy),
                "MURMUR_AT": sit.at,
                "MURMUR_RATE": f"{sit.rate:g}",
                "MURMUR_LINK_EVENTS": str(self.events_file),
                "MURMUR_SIM_TOKEN": self.token,
                "PYTHONUNBUFFERED": "1",
            }
        )
        if sit.contacts:
            env["MURMUR_CONTACTS"] = str(sit.contacts)
        if sit.tidewater:
            env["MURMUR_SCENARIO"] = str(sit.tidewater)
        return env

    def start_stack(self) -> None:
        self.brief_file.write_text(brief_text(self.sit))
        self.events_file.write_text(merge_events(self.sit.events))
        cmd = [str(REPO / "scripts" / "up.sh"), "--fleet", str(self.sit.fleet)]
        with open(self.out / "up.log", "w") as f:
            self.up = subprocess.Popen(cmd, cwd=str(REPO), env=self.stack_env(), stdout=f, stderr=subprocess.STDOUT, start_new_session=True)
        self.record("stack", command=cmd, pid=self.up.pid, log=str((self.out / "up.log").relative_to(REPO)), ports=self.ports)
        say(f"stack starting with scripts/up.sh (pid {self.up.pid}; its output: {(self.out / 'up.log').relative_to(REPO)})")

    def stack_alive(self) -> bool:
        return self.up is not None and self.up.poll() is None

    async def wait_for(self, what: str, check, timeout: float) -> Any:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline and not self.stop.is_set():
            got = await asyncio.to_thread(check)
            if got:
                return got
            if not self.stack_alive():
                raise SituationError(f"the stack stopped while waiting for {what}; see {(self.out / 'up.log').relative_to(REPO)}")
            await asyncio.sleep(1.0)
        if self.stop.is_set():
            raise SituationError("interrupted")
        raise SituationError(f"{what} did not come within {timeout:.0f} s; see {(self.out / 'up.log').relative_to(REPO)}")

    async def take_zero(self) -> None:
        """Situation time 0: the link emulator's start (its uptime), the clock the events
        files count on; without the emulator, when the bridge first answers."""
        if self.linkemu:
            emu = Bridge(self.emu_url)
            # up.sh builds the release binaries first: minutes on a fresh checkout
            summary = await self.wait_for("the link emulator", lambda: emu.try_get("/stats/summary"), 1800)
            self.zero = time.time() - float(summary.get("uptime_s") or 0.0)
        else:
            await self.wait_for("the bridge", self.bridge.alive, 1800)
            self.zero = time.time()
        self.record("zero", zero_unix=round(self.zero, 3), basis="link emulator start" if self.linkemu else "bridge answered")

    def readiness(self, want: set[int]) -> dict[str, Any] | None:
        state = self.bridge.try_get("/api/state") or {}
        ready = {v["uid"] for v in state.get("vehicles", []) if v.get("nav_ready") and v.get("position")}
        self.ready = {"ready": sorted(ready & want), "waiting": sorted(want - ready)}
        return self.ready if want <= ready else None

    async def wait_ready(self, vehicles: list[dict[str, Any]]) -> None:
        want = {int(v["uid"]) for v in vehicles}
        self.set_phase("starting", f"waiting for {len(want)} vehicles to be navigation-ready")
        try:
            await self.wait_for("the fleet's readiness", lambda: self.readiness(want), self.sit.ready_timeout_s)
            self.entry("phase", f"fleet ready: {len(want)} vehicles")
            say(f"fleet ready at T+{self.elapsed():.0f} s")
        except SituationError as e:
            if "interrupted" in str(e) or not self.stack_alive():
                raise
            names = {int(v["uid"]): v["callsign"] for v in vehicles}
            waiting = [names.get(u, str(u)) for u in self.ready.get("waiting", [])]
            self.entry("phase", f"not every vehicle is ready after {self.sit.ready_timeout_s:.0f} s: {', '.join(waiting)}; going on")
            say(f"warning: not ready after {self.sit.ready_timeout_s:.0f} s: {', '.join(waiting)}; going on with the rest")
        self.record("ready", **self.ready)

    async def issue_orders(self) -> None:
        if not self.sit.orders:
            return
        self.set_phase("starting", "issuing the opening orders")
        state = await asyncio.to_thread(self.bridge.try_get, "/api/state") or {}
        vehicles = state.get("vehicles", [])
        for o in self.sit.orders:
            rec: dict[str, Any] = {"id": o.id, "task": o.task}
            try:
                chosen = select_vehicles(o.vehicles, vehicles)
                rec["vehicles"] = [(v.get("identity") or {}).get("callsign") or str(v["uid"]) for v in chosen]
                req = {"task": o.task, "uids": [v["uid"] for v in chosen], **{k: v for k, v in o.params.items()}}
                body = _rest_body(req, o.note or f"opening order of situation {self.sit.name}")
                out = await asyncio.to_thread(self.bridge.submit, body, CONSOLE_OPERATOR)
                intent = out.get("intent") or {}
                rec.update(intent_id=intent.get("id"), state=intent.get("state"), dispatched=bool(out.get("dispatched")))
            except BridgeError as e:
                body_ = e.body if isinstance(e.body, dict) else {}
                rec.update(intent_id=body_.get("intent_id"), state=body_.get("state"), error=str(body_.get("detail") or e))
            except Exception as e:  # noqa: BLE001 (recorded: the situation goes on)
                rec["error"] = str(e)
            self.orders.append(rec)
            self.record("order", **rec)
            what = f"opening order {o.id}: {o.task} for {', '.join(rec.get('vehicles') or ['?'])}"
            self.entry("order", f"{what}: intent {rec.get('intent_id')} {rec.get('state') or ''}{' (' + rec['error'] + ')' if rec.get('error') else ''}")
            say(f"{what}: {rec.get('state') or rec.get('error')}")

    # ---- the AI and the scripted operators ---------------------------------------------------

    def start_ai(self) -> None:
        ai = self.modes["ai"]
        if ai == "none":
            self.ai_status = {"status": "none"}
            return
        assert self.zero is not None
        until = self.zero + self.sit.duration_s
        cmd = ai_command(
            ai,
            self.sit,
            python=str(PYTHON),
            mcp_url=f"{self.bridge_url}/mcp",
            brief_file=self.brief_file,
            agent_log=self.agent_log,
            until=until,
            seconds=until - time.time(),
        )
        assert cmd is not None
        # the AI gets no MURMUR_* variable (no simulation token, no agent key from elsewhere)
        env = {k: v for k, v in os.environ.items() if not k.startswith("MURMUR_")}
        env["PYTHONUNBUFFERED"] = "1"
        with open(self.out / "agent.log", "w") as f:
            self.ai_proc = subprocess.Popen(cmd, cwd=str(REPO), env=env, stdout=f, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True)
        self.ai_status = {
            "status": "running",
            "pid": self.ai_proc.pid,
            "started_t": round(time.time(), 3),
            "log": str((self.out / "agent.log").relative_to(REPO)),
            "preflight": {k: v for k, v in self.preflight.items() if k in ("checked", "logged_in", "auth_method", "note")},
        }
        self.record("ai_start", command=cmd, pid=self.ai_proc.pid)
        self.entry("ai", f"{AI_LABEL[ai]} started as {self.modes['agent']}")
        say(f"AI: {AI_LABEL[ai]} as {self.modes['agent']} (pid {self.ai_proc.pid}; output: {(self.out / 'agent.log').relative_to(REPO)})")

    def stop_ai(self) -> None:
        p = self.ai_proc
        if p is None or p.poll() is not None:
            return
        try:
            os.killpg(os.getpgid(p.pid), signal.SIGTERM)
            p.wait(timeout=8)
        except (ProcessLookupError, PermissionError):
            pass
        except subprocess.TimeoutExpired:
            try:
                os.killpg(os.getpgid(p.pid), signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
        self.ai_status["status"] = f"stopped at the end ({p.poll()})"
        self.record("ai_stop", code=p.poll())

    def scripted(self) -> Any:
        """The autoplay operator (op-1, the proxy model, with op-2 and op-3 signing
        engagements) or, live with a scripted cosigner, the cosigner alone; else nothing."""
        label = f"scripted (situation {self.sit.name}, {self.modes['operator']})"
        if self.modes["operator"] == "autoplay":
            signers = EngagementSigners(self.bridge, list(SCRIPTED_SIGNERS), mode="sign", label=label, log=self.autoplay_log)
            proxy = ProxyOperator(
                self.bridge,
                proxy_params(self.sit.autoplay),
                self.sit.seed,
                log=self.autoplay_log,
                operator=CONSOLE_OPERATOR,
                signers=signers,
                note_prefix=label,
            )
            self.record("autoplay_start", describe=proxy.describe())
            return proxy
        if self.modes["cosigner"] == "scripted":
            signers = EngagementSigners(self.bridge, list(SCRIPTED_SIGNERS), mode="cosign", label=label, log=self.autoplay_log)
            self.record("cosigner_start", describe=signers.describe())
            return signers
        return None

    def storyline(self) -> tuple[Storyline | None, Any]:
        if not self.sit.steps:
            return None, None
        kinds = {s.kind for s in self.sit.steps}
        proposer: Any
        if self.sit.storyline_proposer == "operator":
            proposer = HumanProposer(self.bridge, STORYLINE_OPERATOR)
        else:
            proposer = FreshAgentProposer(f"{self.bridge_url}/mcp", agent=STORYLINE_AGENT)
        story = Storyline(
            self.sit,  # type: ignore[arg-type]  (a Situation has the spec's steps)
            self.bridge,
            proposer,
            World(self.bridge, self.out),
            StepLog(self),  # type: ignore[arg-type]
            self.zero or time.time(),
            declare_operator=STORYLINE_OPERATOR,
            events_file=self.events_file if self.linkemu else None,
            emu_url=self.emu_url if self.linkemu else None,
            # someone signs a nomination: the scripted signers in autoplay, a person live
            engage=EngageAdapter(self.bridge, signers_ready=True),
        )
        needs_proposer = bool(kinds & {"propose", "flood", "engage"})
        return story, (proposer if needs_proposer else None)

    # ---- the run -----------------------------------------------------------------------------

    async def run(self) -> int:
        loop = asyncio.get_running_loop()
        for sig in (signal.SIGINT, signal.SIGTERM):
            loop.add_signal_handler(sig, self.request_stop)
        vehicles, _ = plan_counts(self.sit.fleet)  # type: ignore[arg-type]
        self.record(
            "start",
            situation=self.sit.name,
            file=str(self.sit.path.relative_to(REPO)) if self.sit.path.is_relative_to(REPO) else str(self.sit.path),
            modes=self.modes,
            preflight=self.preflight,
            ports=self.ports,
            run_id=self.run_id,
            fleet=[{"uid": v["uid"], "callsign": v["callsign"], "platform": v["platform"]} for v in vehicles],
        )
        removed = self.fresh_station()
        say("fresh station: " + ("removed " + "; ".join(removed) if removed else "no restore or resume pointer for this port") + "; MURMUR_RESUME=0")
        self.start_stack()
        tasks: list[asyncio.Task] = []
        story_stop = asyncio.Event()
        proposer = None
        status = 0
        try:
            await self.take_zero()
            tasks.append(asyncio.create_task(self.publisher()))
            await self.wait_for("the bridge", self.bridge.alive, 600)
            self.entry("phase", "stack up")
            await self.wait_ready(vehicles)
            await self.issue_orders()
            self.set_phase("running", "the situation runs")
            scripted = self.scripted()
            if scripted is not None:
                tasks.append(asyncio.create_task(scripted.run(story_stop)))
            story, proposer = self.storyline()
            if proposer is not None:
                await proposer.start()
            if story is not None:
                tasks.append(asyncio.create_task(story.run(story_stop)))
            say(f"running: console http://127.0.0.1:{self.ports['web']} (Situation panel), {self.sit.duration_s:.0f} s of situation time from the stack's start")
            assert self.zero is not None
            end = self.zero + self.sit.duration_s
            # the AI comes on watch at ai_start_s (after the opening orders, whichever is
            # later), so it reads a picture the sensors have had time to build
            ai_at = self.zero + self.sit.ai_start_s
            if self.modes["ai"] != "none" and time.time() < ai_at:
                self.ai_status = {"status": f"comes on watch at T+{self.sit.ai_start_s:.0f} s"}
            ai_started = False
            while time.time() < end and not self.stop.is_set():
                if not self.stack_alive():
                    raise SituationError(f"the stack stopped; see {(self.out / 'up.log').relative_to(REPO)}")
                if not ai_started and time.time() >= ai_at:
                    ai_started = True
                    self.start_ai()
                await asyncio.sleep(1.0)
            if self.interrupted:
                self.set_phase("interrupted", "the runner was stopped (Ctrl-C); the stack is still running: make down stops it")
            else:
                self.set_phase("ended", "the situation's time is up; the stack is still running for inspection: make down stops it")
        except SituationError as e:
            status = 130 if self.interrupted else 1
            phase = "interrupted" if self.interrupted else "failed"
            self.set_phase(phase, str(e) if not self.interrupted else "the runner was stopped (Ctrl-C) before the situation ran")
            say(f"{phase}: {e}")
        except Exception as e:  # noqa: BLE001 (recorded and summarized; the stack stays up)
            status = 1
            self.set_phase("failed", f"the runner failed: {e!r}")
            self.record("error", error=repr(e))
            say(f"failed: {e!r}")
        finally:
            story_stop.set()
            self.stop_ai()
            if tasks:
                done, pending = await asyncio.wait([t for t in tasks], timeout=15)
                for t in pending:
                    t.cancel()
            if proposer is not None:
                try:
                    await proposer.stop()
                except Exception as e:  # noqa: BLE001
                    self.record("warning", text=f"storyline proposer stop: {e!r}")
            self.stop.set()
            self.refresh()
            if self.bridge.alive():
                await asyncio.to_thread(self.post_doc)
            self.summary()
        return status

    def request_stop(self) -> None:
        if not self.stop.is_set():
            self.interrupted = True
            say("stopping the runner (the stack keeps running: make down stops it)")
            self.stop.set()

    # ---- the summary -------------------------------------------------------------------------

    def summary(self) -> None:
        events = AuditTail(self.audit).read() if self.audit.exists() else []
        intents = {i["id"]: i for i in ((self.bridge.try_get("/api/intents") or {}).get("intents", []))}
        created = [e for e in events if e.get("type") == "intent.created"]
        by_source: dict[str, list[int]] = {}
        for e in created:
            i = e.get("intent") or {}
            by_source.setdefault(f"{(i.get('source') or {}).get('kind')} {(i.get('source') or {}).get('id')}", []).append(i.get("id"))
        lines = [
            "",
            f"=== {self.sit.title}: {self.phase} ===",
            f"modes: operator {self.modes['operator']}, AI {AI_LABEL[self.modes['ai']]}, cosigner {self.modes['cosigner']}, reveal {self.modes['reveal']}",
            f"run id: {self.run_id}  (situation time {self.elapsed() or 0:.0f} s of {self.sit.duration_s:.0f})",
            f"situation log: {self.log_path.relative_to(REPO)}",
            f"audit log: {self.audit.relative_to(REPO)}",
            "replay (after `make down`, so the log is closed and its shutdown anchor written):",
            f"  target/release/murmur-replay {self.audit.relative_to(REPO)} --anchors {self.anchors.relative_to(REPO)}",
            "what happened:",
        ]
        for o in self.orders:
            now = intents.get(o.get("intent_id"), {}).get("state")
            lines.append(f"  opening order {o['id']} ({o['task']} for {', '.join(o.get('vehicles') or ['?'])}): intent {o.get('intent_id')} {now or o.get('state') or o.get('error')}")
        agent = self.modes["agent"]
        mine = [i for i in intents.values() if (i.get("source") or {}).get("id") == agent] if agent else []
        lines.append(f"  AI {AI_LABEL[self.modes['ai']]}: {self.ai_status.get('status')}; {len(self.activity)} actions in the audit log, {len(mine)} proposals")
        for i in mine:
            ap = i.get("approval") or {}
            who = f", approved by {ap.get('by')}" if ap.get("by") else ""
            lines.append(f"    intent {i['id']} {i.get('kind')} for vehicles {','.join(i.get('uids') or [])}: {i.get('state')} (tier {(i.get('policy') or {}).get('rule')}{who})")
            if i.get("rationale"):
                lines.append(f"      rationale: {str(i['rationale'])[:220]}")
        if self.modes["scripted_operators"]:
            acts = [d for d in self.autoplay_log if d.get("action") in ("approve", "reject", "sign")]
            lines.append(f"  scripted operators ({', '.join(self.modes['scripted_operators'])}): {len(acts)} decisions")
            for d in acts[-8:]:
                why = f" ({str(d['reason'])[:120]})" if d.get("reason") else ""
                lines.append(f"    {d.get('by') or CONSOLE_OPERATOR} {d.get('action')} intent {d.get('intent_id')}: {d.get('result')}{why}")
        # everything else: the storyline's proposals, an operator's own orders at the console
        known = {o.get("intent_id") for o in self.orders} | {i["id"] for i in mine}
        others = [i for i in intents.values() if i["id"] not in known]
        for i in sorted(others, key=lambda i: i["id"]):
            src = i.get("source") or {}
            lines.append(f"  intent {i['id']} {i.get('kind')} for vehicles {','.join(i.get('uids') or [])} from {src.get('kind')} {src.get('id')}: {i.get('state')}")
        steps = [e for e in self.entries if e["kind"] == "step"]
        lines.append(f"  timeline: {len(steps)} step records, {len(self.done_steps)} of {len(self.sit.steps)} steps run")
        tracks = (self.bridge.try_get("/api/tracks") or {}).get("tracks", [])
        if tracks:
            counts: dict[str, int] = {}
            for t in tracks:
                counts[t.get("affiliation", "?")] = counts.get(t.get("affiliation", "?"), 0) + 1
            lines.append("  tracks now: " + ", ".join(f"{n} {a}" for a, n in sorted(counts.items())))
        lines.append("  intents by source: " + ("; ".join(f"{k}: {len(v)}" for k, v in sorted(by_source.items())) or "none"))
        if self.stack_alive() or self.bridge.alive():
            lines.append(f"the stack is still running: console http://127.0.0.1:{self.ports['web']}; `make down` stops it")
        text = "\n".join(lines)
        print(text, flush=True)
        (self.out / "summary.txt").write_text(text + "\n")
        self.record("end", phase=self.phase, summary=lines)


def preflight(sit: Situation, modes: dict[str, Any], env: dict[str, str]) -> dict[str, Any]:
    """Everything that must hold before the stack starts; a SituationError says what to do."""
    if (env.get("MURMUR_AUTH") or "dev") != "dev":
        raise SituationError(
            f"MURMUR_AUTH={env.get('MURMUR_AUTH')}: situations run as dev runs for now (operator ids asserted, "
            "logged as dev, never evidence about people). Unset MURMUR_AUTH for a situation."
        )
    pf = ai_preflight(modes["ai"])
    ports = stack_ports(env)
    vehicles, sitl = plan_counts(sit.fleet)  # type: ignore[arg-type]
    problems = port_problems(ports, len(vehicles), sitl)
    if problems:
        raise SituationError(
            "refusing to start: " + "; ".join(problems) + ". If that is a stack of yours, stop it with `make down` "
            "(from the checkout that started it) and try again, or run this one beside it on other ports (the "
            "MURMUR_* port variables, scripts/up.sh's header). The runner never stops another stack."
        )
    eph = ephemeral_range()
    if eph:
        for key in ("vehicle_base", "bridge_base"):
            lo, hi = ports[key], ports[key] + len(vehicles)
            if lo <= eph[1] and hi >= eph[0]:
                say(f"warning: {key} {lo}-{hi} reaches into the ephemeral port range {eph[0]}-{eph[1]} (collisions at random; CLAUDE.md)")
    if sit.tidewater_wanted != "none" and sit.tidewater is None:
        say(f"warning: tidewater scenario {sit.tidewater_wanted!r} not found (TIDEWATER_HOME): running without real conditions")
    return pf


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="python -m eval.situation_run", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("name", help="a situation in sim/situations (name or path)")
    ap.add_argument("--operator", choices=["live", "autoplay"], help="who decides at the console (default: the situation's)")
    ap.add_argument("--ai", choices=["claude", "codex", "playbook", "none"], help="the AI watch officer (default: the situation's)")
    ap.add_argument("--cosigner", choices=["you", "scripted"], help="live mode: who makes an engagement's second signature")
    ap.add_argument("--reveal", choices=["0", "1"], help="show the director's notes and the timeline to come (default: the situation's)")
    args = ap.parse_args(argv)
    try:
        sit = load_situation(args.name)
        if sit.todo:
            raise SituationError(f"{sit.name} is a stub (todo = true): only its title and brief are written yet")
        modes = resolve_modes(sit, args.operator, args.ai, args.cosigner, None if args.reveal is None else args.reveal == "1")
        env = dict(os.environ)
        pf = preflight(sit, modes, env)
    except SituationError as e:
        print(f"situation: {e}", file=sys.stderr)
        return 2
    runner = Runner(sit, modes, env, pf)
    say(f"{sit.title}: operator {modes['operator']}, AI {modes['ai_label']}, run {runner.run_id}")
    if pf.get("checked"):
        say(f"AI preflight: {modes['ai']} logged in ({pf.get('auth_method')}); Murmur only runs your own CLI and never reads its credentials")
    elif pf.get("note"):
        say(f"AI preflight: {pf['note']}")
    return asyncio.run(runner.run())


if __name__ == "__main__":
    sys.exit(main())
