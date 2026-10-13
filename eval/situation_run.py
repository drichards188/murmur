#!/usr/bin/env python3
"""The situation runner (`sim/situations/README.md`):

    make situation NAME=harbor-watch [OPERATOR=live|autoplay] [AI=claude|codex|playbook|none]
    .venv/bin/python -m eval.situation_run harbor-watch --operator autoplay --ai playbook

1. Reads the situation and checks, before anything starts: a dev run (`MURMUR_AUTH` unset or
   dev), the chosen AI's CLI installed and logged in (`claude auth status`, `codex login
   status`; never a credential), and no stack answering on the situation's ports (its own
   block, `SITUATION_PORTS` in eval/situation.py; each `MURMUR_*` port variable overrides one).
   It never stops another stack.
2. Makes a fresh station: removes the bridge's restore pointer (`logs/audit/.last-run-<http
   port>`) and resume pointer (`.resume-<http port>.json`) and sets `MURMUR_RESUME=0`, saying so.
3. Starts the stack with `scripts/up.sh` in its own session (the situation's fleet, contacts,
   merged events, policy, tidewater scenario, the scenario clock, a roster for the modes, this
   run's id and simulation token), so it outlives the runner: `make down` stops it.
4. Takes the stack's clock (the link emulator's start: "situation time" counts from it, as the
   events files' `at_s` and the contacts' `start_s` do), waits until every vehicle is
   navigation-ready, and issues the opening orders under the scripted id op-setup. Each is
   first an operator's dry run (`POST /api/intents/preview`, nothing logged): an order whose
   submission would be its own approval (a human tier) is proposed instead by the scripted
   agent agent-setup, so it waits for the person (live) or the autoplay operator (op-auto).
   A script never authorizes a human tier, and op-1 is never a script.
5. Starts the AI in an empty directory of its own outside the repository (only `brief.md`;
   agents/README.md, "In a situation: the launch contract"), the autoplay operator
   (`eval/proxy.py`'s `ProxyOperator` as op-auto with `EngagementSigners` op-2 and op-3, all
   scripted) or the scripted cosigner, and the timeline (`eval/storyline.py`; its proposals
   and declarations say they are a script's).
6. Every 2 s posts the status document to the bridge (`POST /api/sim/situation`, display
   only) for the console's Situation panel, and writes everything it did to
   `logs/situations/<run id>/situation.jsonl`.
7. At the end (or Ctrl-C, SIGTERM, SIGHUP) stops its own children (the AI's process group,
   the autoplay), leaves the stack running for inspection, and prints a summary: the audit
   log, the `murmur-replay` command, what happened.
"""

from __future__ import annotations

import argparse
import asyncio
import datetime as dt
import json
import logging
import os
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

from . import PYTHON, REPO
from .agent import AgentProposer, HumanProposer, ProposeError, _mcp_args, _rest_body
from .bridge import Bridge, BridgeError
from .engage import EngageAdapter
from .ports import ephemeral_range
from .proxy import EngagementSigners, ProxyOperator, proxy_params
from .situation import (
    AGENTS,
    AI_LABEL,
    EngagementChain,
    AUTOPLAY_OPERATOR,
    CONSOLE_OPERATOR,
    ENGAGE_OPERATORS,
    OPERATORS,
    SCRIPTED_SETUP,
    SCRIPTED_SIGNERS,
    SCRIPTED_TIMELINE,
    SETUP_AGENT,
    SETUP_OPERATOR,
    STORYLINE_AGENT,
    STORYLINE_OPERATOR,
    AuditTail,
    Situation,
    SituationError,
    agent_activity,
    ai_command,
    ai_preflight,
    brief_text,
    decision_text,
    load_situation,
    playbook_json,
    merge_events,
    pointer_names_situation,
    pointer_problems,
    port_problems,
    resolve_modes,
    situation_env,
    stack_ports,
    status_doc,
    step_text,
)
from .stack import _swarm
from .storyline import Storyline, World, select_vehicles

PUBLISH_S = 2.0
SIM_TOKEN_HEADER = "X-Murmur-Sim-Token"
# the audit events that are a person's (or a script's) decision on an intent
DECISION_EVENTS = ("approval.at_submission", "approval.granted", "approval.concurred", "approval.rejected")


class _SessionCloseAccepted(logging.Filter):
    """The MCP client warns "Session termination failed: 202" when the bridge (rmcp) accepts a
    session's close with 202: not a failure, so not said at each scripted proposal."""

    def filter(self, record: logging.LogRecord) -> bool:
        return record.getMessage() != "Session termination failed: 202"


def utc_stamp() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def say(msg: str) -> None:
    """A line on the terminal; nothing when it is gone (a hung-up terminal raises EIO)."""
    try:
        print(f"situation: {msg}", flush=True)
    except OSError:
        pass


def rel(p: Path) -> str:
    """A path as the repository names it (absolute when outside it)."""
    try:
        return str(p.relative_to(REPO))
    except ValueError:
        return str(p)


def plan_counts(fleet: Path) -> tuple[list[dict[str, Any]], int]:
    """The fleet as `sim/swarm.py` plans it (callsigns, uids in unit order) and how many
    vehicles need SITL."""
    swarm = _swarm()
    vehicles = swarm.plan(swarm.load_fleet(str(fleet)), swarm.load_platforms())
    for i, v in enumerate(vehicles):
        v["uid"] = i + 1
    return vehicles, sum(1 for v in vehicles if v.get("sim") != "standin")


def labelled(label: str, text: str | None) -> str:
    """`text` under a scripted label (once), so the audit log says whose act it is."""
    text = (text or "").strip()
    if text.startswith(label):
        return text
    return f"{label}: {text}" if text else label


def dry_run_summary(preview: dict[str, Any]) -> dict[str, Any]:
    pol = preview.get("policy") or {}
    return {
        "would_reach": preview.get("would_reach"),
        "submission_authorizes": bool(preview.get("submission_authorizes")),
        "tier": pol.get("tier"),
        "rule": pol.get("rule"),
        "reason": pol.get("reason"),
    }


async def operator_dry_run(bridge: Bridge, body: dict[str, Any], operator: str) -> dict[str, Any]:
    """The operator's dry run of a submission (`POST /api/intents/preview`): the checks and the
    policy decision now, and whether the submission would be its own approval. Nothing is
    created or logged. Raises BridgeError."""
    return await asyncio.to_thread(bridge.post, "/api/intents/preview", body, operator)


def submits_itself(preview: dict[str, Any]) -> bool:
    """Whether a scripted operator id may submit this itself: only where its submission can't
    be the approval, now or a moment later. The policy sends it out (`auto_approved`), or it
    waits for someone else (`pending_approval` in a tier whose submitter does not authorize).
    `approved` (its submission would authorize it) and `rejected` (a check fails now, a terrain
    fetch or a fact's age, and may pass a moment later in a human tier) are not: an agent
    proposes those instead, which can never approve."""
    reach = preview.get("would_reach")
    return reach == "auto_approved" or (reach == "pending_approval" and not preview.get("submission_authorizes"))


def self_authorized(out: dict[str, Any], scripted: set[str]) -> dict[str, Any] | None:
    """The approval in a submission's answer when it is a scripted id's own, at submission: what
    the dry run is there to prevent, should a fact have changed between the two."""
    ap = (out.get("intent") or {}).get("approval") or {}
    return ap if ap.get("at_submission") and ap.get("by") in scripted else None


async def undo_self_authorized(bridge: Bridge, intent_id: Any, operator: str, dispatched: bool) -> str:
    """Cancel an intent a scripted id authorized by submitting it, unless it went out already."""
    if dispatched:
        return "it was dispatched already and could not be cancelled"
    try:
        out = await asyncio.to_thread(
            bridge.post, f"/api/intents/{intent_id}/cancel", {"note": "a script's own submission authorized it: cancelled"}, operator
        )
        return f"cancelled ({(out.get('intent') or {}).get('state')})"
    except (BridgeError, OSError, ValueError) as e:
        return f"the cancel failed ({type(e).__name__})"


def proc_stat(pid: int) -> tuple[str, int, int, int] | None:
    """(state, process group, session, start time in clock ticks since boot) from /proc."""
    try:
        rest = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return rest[0], int(rest[2]), int(rest[3]), int(rest[19])
    except (OSError, IndexError, ValueError):
        return None


def session_members(pgid: int, started: int | None) -> list[int]:
    """The live processes of the AI's own session: process group and session both `pgid` (it
    was started in a session of its own), not zombies, started no earlier than the AI (clock
    ticks), and only while the group's number is not another's: a live leader `pgid` must be
    the AI's own (the same start)."""
    if not os.path.isdir("/proc"):
        return []
    leader = proc_stat(pgid)
    if leader is not None and leader[0] != "Z" and started is not None and leader[3] != started:
        return []
    out = []
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        st = proc_stat(int(d))
        if st is None or st[0] == "Z" or st[1] != pgid or st[2] != pgid:
            continue
        if started is not None and st[3] < started:
            continue
        out.append(int(d))
    return out


# The watch officer's log lines a person reads in the panel, by its kind, and the word shown
# (agents/README.md, "The situation log"); its other lines (connecting, starting the CLI, the
# playbook's shadow, a change it set aside) stay in its own log, kept with the run.
PANEL_KINDS = {
    "note": "note", "turn": "note", "wake": "woken", "error": "error", "lockdown.breach": "error",
    "preflight.failed": "error", "backend.crashed": "error", "lockdown.refused": "refused",
    "limit.hit": "limit", "limit.resume": "resumed", "backoff": "waiting", "budget.exhausted": "budget",
    "budget.deferred": "budget", "run.start": "started", "run.stop": "stopped",
    "tool": "tool", "proposal": "proposal",
    # the playbook engine's (AI=playbook): a play's status when it changes, its withdrawals, how
    # the operator answered its cards, what an engagement kept it from doing, an approved card of
    # its own whose premise went (left as it is), a withdrawal the bridge refused because the card
    # was decided meanwhile, and what checking the ledger against the bridge dropped
    "play.status": "play", "withdrawal": "withdrew", "decision": "answer", "withheld": "withheld",
    "left": "left", "withdraw.refused": "kept", "ledger.verified": "note", "ledger.unverified": "waiting",
}


def panel_kind(line: dict[str, Any]) -> str | None:
    """The word the panel shows for one of the AI's log lines, or None to leave it out."""
    kind = str(line.get("kind", "note"))
    if kind == "turn" and not str(line.get("text") or "").strip():
        return None
    return PANEL_KINDS.get(kind)


def ai_env(env: dict[str, str]) -> dict[str, str]:
    """The AI's environment: the runner's minus every `MURMUR_*` variable (no simulation
    token, no agent key from elsewhere, no other stack's ports), so the user's own CLI login
    works as it does in their shell."""
    out = {k: v for k, v in env.items() if not k.startswith("MURMUR_")}
    out["PYTHONUNBUFFERED"] = "1"
    return out


def ai_workdir(run_id: str, forbidden: list[Path]) -> Path:
    """An empty directory of the AI's own for this run, outside the repository and outside
    the run directory: it starts there with only the brief, so it reads nothing the
    situation hides (the director's notes, the timeline, the runner's logs)."""
    d = Path(tempfile.mkdtemp(prefix=f"murmur-{run_id}-ai-")).resolve()
    for f in forbidden:
        if d.is_relative_to(f.resolve()):
            shutil.rmtree(d, ignore_errors=True)
            raise SituationError(f"the AI's directory {d} would be inside {f} (TMPDIR?): it must start outside the repository")
    return d


class FreshAgentProposer(AgentProposer):
    """A scripted agent (the timeline's `agent-scripted`, the opening orders' `agent-setup`),
    connected for each proposal or withdrawal in a session of its own: a situation's steps can
    be minutes apart and the bridge's MCP server closes a session idle for 15 minutes, and
    two steps due together must neither share nor close each other's session. Every rationale
    begins with `label`, so the audit log says it is a script's."""

    def __init__(self, mcp_url: str, agent: str, label: str, key: str | None = None):
        super().__init__(mcp_url, agent=agent, key=key)
        self.label = label

    async def start(self) -> None:
        pass

    async def stop(self) -> None:
        pass

    def _new_session(self) -> Any:
        import policy_agent  # agents/policy_agent.py (looked up at each call)

        return policy_agent.AgentSession(self.mcp_url, self.source_id, self.key)

    async def propose(self, req: dict[str, Any], rationale: str) -> dict[str, Any]:
        try:
            async with self._new_session() as s:
                out = await s.propose(labelled(self.label, rationale), **_mcp_args(req))
        except ProposeError:
            raise
        except Exception as e:  # noqa: BLE001 (a refusal, or the session would not open)
            raise ProposeError(f"{type(e).__name__}: {e}") from e
        rec = {"id": out.get("id"), "state": out.get("state"), "dispatched": bool(out.get("dispatched"))}
        if out.get("refused"):
            rec["refused"] = out["refused"]
        return rec

    async def withdraw(self, id: int, reason: str) -> dict[str, Any] | None:
        try:
            async with self._new_session() as s:
                out = await s.withdraw(id, labelled(self.label, reason))
        except Exception as e:  # noqa: BLE001
            raise ProposeError(f"{type(e).__name__}: {e}") from e
        return {"id": id, "state": out.get("state")}

    async def tracks(self) -> list[dict]:
        async with self._new_session() as s:
            return await s.tracks()


class ScriptedOperatorProposer(HumanProposer):
    """The timeline's operator proposer (`op-planner`, a scripted id): each rationale begins
    with the scripted label, and it submits only what its submission can't authorize
    (`submits_itself` on the operator's dry run, nothing logged); anything else is refused
    before it is sent, in either mode: a human tier is a person's to decide, never a script's.
    An engagement nomination needs no dry run: it is never approved at submission (the engine's
    floor). Every answer is checked all the same; an approval at submission by a scripted id
    (a fact changed between the dry run and the submission) is cancelled if it has not gone
    out, and `alarm` says so."""

    def __init__(self, bridge: Bridge, operator: str, label: str, scripted: set[str], alarm: Any = None):
        super().__init__(bridge, operator)
        self.label = label
        self.scripted = set(scripted)
        self.alarm = alarm or (lambda text, **fields: None)

    async def propose(self, req: dict[str, Any], rationale: str) -> dict[str, Any]:
        text = labelled(self.label, rationale)
        body = _rest_body(req, text)
        if req.get("task") != "engage":
            try:
                preview = await operator_dry_run(self.bridge, body, self.source_id)
            except (BridgeError, OSError, ValueError) as e:
                raise ProposeError(f"not sent: the dry run could not say what {self.source_id}'s submission would do ({type(e).__name__})") from e
            if not submits_itself(preview):
                d = dry_run_summary(preview)
                raise ProposeError(
                    f"not sent: {self.source_id} (scripted) submits only what the policy sends out or what waits for "
                    f"someone else; the dry run said {d['would_reach']} (tier {d['rule']})"
                )
        try:
            out = await asyncio.to_thread(self.bridge.submit, body, self.source_id)
        except BridgeError as e:
            if isinstance(e.body, dict) and e.body.get("intent_id") is not None:
                return {"id": e.body["intent_id"], "state": e.body.get("state"), "dispatched": False, "refused": e.body.get("detail")}
            raise ProposeError(str(e)) from e
        intent = out.get("intent") or {}
        rec = {"id": intent.get("id"), "state": intent.get("state"), "dispatched": bool(out.get("dispatched"))}
        ap = self_authorized(out, self.scripted)
        if ap is not None:
            undone = await undo_self_authorized(self.bridge, intent.get("id"), self.source_id, rec["dispatched"])
            rec["self_authorized"] = undone
            self.alarm(
                f"SCRIPT AUTHORIZED AN ORDER: intent {intent.get('id')} was approved at submission by {ap.get('by')} "
                f"(scripted, tier {ap.get('rule') or '?'}); {undone}",
                intent_id=intent.get("id"),
            )
        return rec

    async def withdraw(self, id: int, reason: str) -> dict[str, Any] | None:
        return await super().withdraw(id, labelled(self.label, reason))


class TimelineBridge(Bridge):
    """The bridge as the timeline uses it: a declaration's note says it is a script's."""

    def declare(self, track_id: str, affiliation: str, note: str | None, operator: str) -> dict:
        return super().declare(track_id, affiliation, labelled(SCRIPTED_TIMELINE, note), operator)


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
    def __init__(
        self,
        sit: Situation,
        modes: dict[str, Any],
        env: dict[str, str],
        preflight: dict[str, Any],
        out_dir: Path | None = None,
    ):
        self.sit = sit
        self.modes = modes
        self.env = situation_env(env)
        self.preflight = preflight
        self.ports = stack_ports(self.env)
        self.run_id = f"situation-{sit.name}-{utc_stamp()}"
        self.out = out_dir or REPO / "logs" / "situations" / self.run_id
        self.out.mkdir(parents=True, exist_ok=True)
        self.log_path = self.out / "situation.jsonl"
        self.audit = REPO / "logs" / "audit" / f"{self.run_id}.jsonl"
        self.anchors = REPO / "logs" / "anchors" / f"{self.run_id}.anchors"
        self.events_file = self.out / "events.toml"
        self.token = secrets.token_hex(24)
        self.bridge_url = f"http://127.0.0.1:{self.ports['http']}"
        self.bridge = Bridge(self.bridge_url)
        self.emu_url = f"http://127.0.0.1:{self.ports['linkemu']}"
        self.linkemu = (self.env.get("MURMUR_LINKEMU") or "1").lower() not in ("0", "off", "false", "no")
        self.steps = {s.id: s for s in sit.steps}
        self.done_steps: set[str] = set()
        self.entries: list[dict[str, Any]] = []
        self.orders: list[dict[str, Any]] = []
        self.problems: list[str] = []
        self.zero: float | None = None
        self.phase = "starting"
        self.phase_detail = "starting the stack"
        self.up: subprocess.Popen | None = None
        # the AI: its process (and process group), its own directory, its notes there
        self.ai_proc: subprocess.Popen | None = None
        self.ai_pgid: int | None = None
        self.ai_dir: Path | None = None
        self.ai_status: dict[str, Any] = {"status": "not started"}
        self.notes_tail: AuditTail | None = None
        self.autoplay_log: list[dict[str, Any]] = []
        self.autoplay_seen = 0
        self.activity: list[dict[str, Any]] = []
        self.ai_notes: list[dict[str, Any]] = []
        self.audit_tail = AuditTail(self.audit)
        # the engagement chain as the audit log has it (acts, declarations, nominations,
        # signatures, the effect, the assessment), for the panel, situation.jsonl and the summary
        planned, _ = plan_counts(sit.fleet) if sit.fleet else ([], 0)
        self.chain = EngagementChain({str(v["uid"]): v["callsign"] for v in planned})
        self.publish_errors = 0
        self.stop = asyncio.Event()
        self.interrupted = False
        self.ready: dict[str, Any] = {}
        # every scripted operator id of the run: none of them may authorize anything by submitting
        self.scripted_ids = set(modes["scripted_operators"]) | {SETUP_OPERATOR, STORYLINE_OPERATOR, AUTOPLAY_OPERATOR, *SCRIPTED_SIGNERS}
        # the AI's start (clock ticks since boot, /proc), to know its process group is its own
        self.ai_started_ticks: int | None = None

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

    def problem(self, text: str, **fields: Any) -> None:
        """Something went wrong in the runner itself: in situation.jsonl, the panel and the
        terminal; the situation goes on."""
        self.problems.append(text)
        self.record("problem", text=text, **fields)
        self.entry("problem", text)
        say(f"warning: {text}")

    def set_phase(self, phase: str, detail: str) -> None:
        self.phase, self.phase_detail = phase, detail
        self.record("phase", phase=phase, detail=detail)

    def watch_task(self, name: str, coro: Any) -> asyncio.Task:
        """A background task whose failure is recorded (not lost until the end)."""
        t = asyncio.create_task(coro, name=name)
        t.add_done_callback(lambda done, name=name: self.task_done(name, done))
        return t

    def task_done(self, name: str, t: asyncio.Task) -> None:
        if t.cancelled():
            return
        e = t.exception()
        if e is not None:
            # the panel gets the runner's own words; what the exception said is in situation.jsonl
            self.problem(f"{name} stopped ({type(e).__name__}); see situation.jsonl", task=name, error=repr(e))

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
            mode = "autoplay" if self.modes["operator"] == "autoplay" else ("cosigner" if self.modes["cosigner"] == "scripted" else "setup")
            autoplay = {
                "operators": self.modes["scripted_operators"],
                "mode": mode,
                "scripted": True,
                "note": "scripted ids, not people: a model or a script (eval/), never evidence",
                "decisions": [
                    {
                        **{k: d.get(k) for k in ("t", "intent_id", "action", "by", "reason", "result")},
                        **{k: d[k] for k in ("declaration", "track", "granted") if k in d},
                    }
                    for d in self.autoplay_log[-10:]
                ],
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
            run={"id": self.run_id, "audit_log": rel(self.audit), "situation_log": rel(self.log_path)},
            problems=self.problems,
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
        own notes, the scripted operators' decisions, the AI process."""
        # the AI's own actions only (no AI, none: the storyline's agent is not the AI)
        events = self.audit_tail.read()
        new = agent_activity(events, self.modes["agent"]) if self.modes["agent"] else []
        for a in new:
            # on the situation's clock, for the panel
            a["t_s"] = round(a["t"] - self.zero, 1) if self.zero is not None else None
            self.record("ai_action", **a)
        self.activity.extend(new)
        for c in self.chain.feed(events):
            # on the situation's clock, for the panel
            c["t_s"] = round(c["t"] - self.zero, 1) if self.zero is not None else None
            self.record("engagement", **c)
            self.entry("engagement", c["text"], what=c["what"], intent=c.get("intent"))
        notes_ok = self.notes_tail is not None and not self.notes_tail.path.is_symlink()
        for n in self.notes_tail.read() if notes_ok and self.notes_tail is not None else []:
            kind = panel_kind(n)
            if kind is None:
                continue  # the watch officer's plumbing: in its own log, kept with the run
            t = n.get("t") if isinstance(n.get("t"), (int, float)) else None
            note = {
                "t": t,
                "t_s": round(t - self.zero, 1) if t is not None and self.zero is not None else None,
                "kind": kind,
                "text": str(n.get("text", ""))[:400],
            }
            self.ai_notes.append(note)
            self.record("ai_note", **note)
        while self.autoplay_seen < len(self.autoplay_log):
            d = self.autoplay_log[self.autoplay_seen]
            self.autoplay_seen += 1
            # the proxy's own decisions name no operator: they are op-auto's
            d.setdefault("by", self.modes.get("autoplay_operator") or "?")
            self.record("autoplay", **d)
            failed = str(d.get("result") or "").startswith("error") or d.get("action") in ("error", "sign_failed")
            if failed:
                # the bridge's status code only; its words and the reason are in situation.jsonl
                code = str(d.get("result") or "").split(":", 1)[0] if str(d.get("result") or "").startswith("error") else "an error"
                what = f"{d.get('declaration')} on {d.get('track')}" if d.get("action") == "confirm" else f"intent {d.get('intent_id')}"
                self.problem(f"{d.get('by')} (scripted): {d.get('action')} on {what} failed ({code}); see situation.jsonl")
                continue
            verb = {"approve": "approved", "reject": "rejected", "sign": "signed", "confirm": "confirmed"}.get(str(d.get("action")))
            if verb == "confirmed":
                self.entry("autoplay", f"{d.get('by')} (scripted) confirmed the rule declaration {d.get('declaration')} on {d.get('track')}: {d.get('result')}")
            elif verb:
                grant = " (it grants every request of the AI)" if d.get("granted") else ""
                self.entry("autoplay", f"{d.get('by')} (scripted) {verb} intent {d.get('intent_id')}{grant}: {d.get('result')}")
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
            if (p.exists() or p.is_symlink()) and pointer_names_situation(p) is not True:
                # another station's (preflight refuses before this): never deleted
                raise SituationError(f"{rel(p)} is not a situation's pointer: not deleted (see pointer_problems)")
            if p.exists() or p.is_symlink():
                try:
                    named = p.read_text().strip()[:300]
                except (OSError, ValueError):
                    named = "(unreadable)"
                p.unlink()
                removed.append(f"{rel(p)} (named {named})")
        self.record("fresh_station", removed=removed, resume="MURMUR_RESUME=0")
        return removed

    def stack_env(self) -> dict[str, str]:
        sit = self.sit
        drop = (
            "MURMUR_SIM_TOKEN", "MURMUR_EVAL_TRUTH", "MURMUR_EVENTS", "MURMUR_STUDY", "MURMUR_STUDY_PASSWORD_STEP_UP",
            "MURMUR_CREDENTIALS", "MURMUR_CONTACTS", "MURMUR_SCENARIO",
            # the situation's own tidewater, on its block's port (up.sh derives the URL from it)
            "MURMUR_TIDEWATER_URL",
        )
        env = {k: v for k, v in self.env.items() if k not in drop}
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
        self.events_file.write_text(merge_events(self.sit.events))
        cmd = [str(REPO / "scripts" / "up.sh"), "--fleet", str(self.sit.fleet)]
        with open(self.out / "up.log", "w") as f:
            self.up = subprocess.Popen(cmd, cwd=str(REPO), env=self.stack_env(), stdout=f, stderr=subprocess.STDOUT, start_new_session=True)
        self.record("stack", command=cmd, pid=self.up.pid, log=rel(self.out / "up.log"), ports=self.ports)
        say(f"stack starting with scripts/up.sh (pid {self.up.pid}; its output: {rel(self.out / 'up.log')})")

    def stack_alive(self) -> bool:
        return self.up is not None and self.up.poll() is None

    async def wait_for(self, what: str, check, timeout: float) -> Any:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline and not self.stop.is_set():
            got = await asyncio.to_thread(check)
            if got:
                return got
            if not self.stack_alive():
                raise SituationError(f"the stack stopped while waiting for {what}; see {rel(self.out / 'up.log')}")
            await asyncio.sleep(1.0)
        if self.stop.is_set():
            raise SituationError("interrupted")
        raise SituationError(f"{what} did not come within {timeout:.0f} s; see {rel(self.out / 'up.log')}")

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

    async def issue_order(self, o: Any, vehicles: list[dict[str, Any]], setup_agent: FreshAgentProposer) -> dict[str, Any]:
        """One opening order under the scripted setup id: an operator's dry run first. op-setup
        submits it only where its submission can't be the approval (`submits_itself`: the
        policy sends it out, or it waits for someone else); otherwise agent-setup proposes it
        and it waits for a decision (the person live, op-auto on autoplay). The answer is
        checked all the same: an approval at submission by a scripted id is a loud problem, and
        the intent is cancelled if it has not gone out."""
        rec: dict[str, Any] = {"id": o.id, "task": o.task}
        try:
            chosen = select_vehicles(o.vehicles, vehicles)
            if not chosen:
                raise SituationError(f"no vehicle matches {o.vehicles!r}")
            rec["vehicles"] = [(v.get("identity") or {}).get("callsign") or str(v["uid"]) for v in chosen]
            req = {"task": o.task, "uids": [v["uid"] for v in chosen], **dict(o.params)}
            note = o.note or f"opening order of situation {self.sit.name}"
            body = _rest_body(req, labelled(SCRIPTED_SETUP, note))
            preview = await operator_dry_run(self.bridge, body, SETUP_OPERATOR)
            dry = dry_run_summary(preview)
            rec["dry_run"] = dry
            if not submits_itself(preview):
                decider = "the person at the console" if self.modes["operator"] == "live" else f"{AUTOPLAY_OPERATOR} (scripted)"
                why = f"a human tier ({dry['rule']})" if dry["would_reach"] == "approved" else f"the dry run said {dry['would_reach']}"
                rec["routed"] = f"{why}: proposed by {SETUP_AGENT} so that {decider} decides it"
                out = await setup_agent.propose(req, note)
                rec.update(by=SETUP_AGENT, intent_id=out.get("id"), state=out.get("state"), dispatched=bool(out.get("dispatched")))
                if out.get("refused"):
                    rec["error"] = str(out["refused"])
            else:
                out = await asyncio.to_thread(self.bridge.submit, body, SETUP_OPERATOR)
                intent = out.get("intent") or {}
                rec.update(by=SETUP_OPERATOR, intent_id=intent.get("id"), state=intent.get("state"), dispatched=bool(out.get("dispatched")))
                ap = self_authorized(out, self.scripted_ids)
                if ap is not None:
                    undone = await undo_self_authorized(self.bridge, intent.get("id"), SETUP_OPERATOR, rec["dispatched"])
                    rec["self_authorized"] = undone
                    self.problem(
                        f"SCRIPT AUTHORIZED AN ORDER: opening order {o.id} (intent {intent.get('id')}) was approved at "
                        f"submission by {ap.get('by')} (scripted, tier {ap.get('rule') or '?'}); {undone}",
                        intent_id=intent.get("id"),
                    )
        except BridgeError as e:
            body_ = e.body if isinstance(e.body, dict) else {}
            rec.update(intent_id=body_.get("intent_id"), state=body_.get("state"), error=str(body_.get("detail") or e))
        except Exception as e:  # noqa: BLE001 (recorded: the situation goes on)
            rec["error"] = str(e)
        return rec

    async def issue_orders(self) -> None:
        if not self.sit.orders:
            return
        self.set_phase("starting", "issuing the opening orders")
        state = await asyncio.to_thread(self.bridge.try_get, "/api/state") or {}
        vehicles = state.get("vehicles", [])
        setup_agent = FreshAgentProposer(f"{self.bridge_url}/mcp", agent=SETUP_AGENT, label=SCRIPTED_SETUP)
        for o in self.sit.orders:
            rec = await self.issue_order(o, vehicles, setup_agent)
            self.orders.append(rec)
            self.record("order", **rec)
            what = f"opening order {o.id}: {o.task} for {', '.join(rec.get('vehicles') or ['?'])}"
            how = f" by {rec['by']} (scripted)" if rec.get("by") else ""
            waits = ""
            if rec.get("routed") and rec.get("state") == "pending_approval":
                waits = "; it waits for your decision in the Inbox" if self.modes["operator"] == "live" else f"; {AUTOPLAY_OPERATOR} decides it"
            err = f" ({rec['error']})" if rec.get("error") else ""
            self.entry("order", f"{what}{how}: intent {rec.get('intent_id')} {rec.get('state') or ''}{waits}{err}")
            say(f"{what}{how}: {rec.get('state') or rec.get('error')}{waits}")

    # ---- the AI and the scripted operators ---------------------------------------------------

    def start_ai(self) -> None:
        ai = self.modes["ai"]
        if ai == "none":
            self.ai_status = {"status": "none"}
            return
        assert self.zero is not None
        until = self.zero + self.sit.duration_s
        # its own empty directory: the brief, and the notes it writes; nothing else
        self.ai_dir = ai_workdir(self.run_id, [REPO, self.out])
        brief = self.ai_dir / "brief.md"
        brief.write_text(brief_text(self.sit))
        # the playbook engine's config (the situation's [playbook] table, nothing else of the
        # file): for AI=playbook, and for the shadow beside a model
        playbook = self.ai_dir / "playbook.json"
        playbook.write_text(playbook_json(self.sit))
        notes = self.ai_dir / "agent.jsonl"
        self.notes_tail = AuditTail(notes)
        cmd = ai_command(
            ai,
            self.sit,
            python=str(PYTHON),
            mcp_url=f"{self.bridge_url}/mcp",
            brief_file=brief,
            agent_log=notes,
            until=until,
            seconds=until - time.time(),
            codex_home=(os.environ.get("MURMUR_CODEX_HOME") or "").strip() or None,
            playbook_file=playbook,
        )
        assert cmd is not None
        with open(self.out / "agent.log", "w") as f:
            self.ai_proc = subprocess.Popen(
                cmd,
                cwd=str(self.ai_dir),
                env=ai_env(dict(os.environ)),
                stdout=f,
                stderr=subprocess.STDOUT,
                stdin=subprocess.DEVNULL,
                start_new_session=True,
            )
        # its own session: the group id is its pid, kept to stop the whole group at the end, with
        # its start, so a reused group number is never signalled
        self.ai_pgid = self.ai_proc.pid
        st = proc_stat(self.ai_proc.pid)
        self.ai_started_ticks = st[3] if st else None
        self.ai_status = {
            "status": "running",
            "pid": self.ai_proc.pid,
            "started_t": round(time.time(), 3),
            "log": rel(self.out / "agent.log"),
            "preflight": {k: v for k, v in self.preflight.items() if k in ("checked", "logged_in", "auth_method", "note")},
        }
        self.record("ai_start", command=cmd, pid=self.ai_proc.pid, cwd=str(self.ai_dir))
        self.entry("ai", f"{AI_LABEL[ai]} started as {self.modes['agent']}")
        say(f"AI: {AI_LABEL[ai]} as {self.modes['agent']} (pid {self.ai_proc.pid}, in {self.ai_dir}; output: {rel(self.out / 'agent.log')})")

    def _group_alive(self) -> bool:
        """Whether the AI's own session still has a live process (checked through /proc: a
        reused group number is not the AI's)."""
        if self.ai_pgid is None:
            return False
        if not os.path.isdir("/proc"):
            # no /proc: only while the leader the runner started is alive is the group surely its
            return self.ai_proc is not None and self.ai_proc.poll() is None
        return bool(session_members(self.ai_pgid, self.ai_started_ticks))

    def _signal_ai(self, sig: int) -> None:
        if self._group_alive():
            try:
                os.killpg(self.ai_pgid, sig)  # type: ignore[arg-type]
            except (ProcessLookupError, PermissionError):
                pass

    def stop_ai(self) -> None:
        """SIGTERM to the AI's whole process group (even when its leader has exited: a CLI it
        started lives on in the group), SIGKILL 8 s later; each only while /proc shows a live
        process of the AI's own session (group and session its number, started after it)."""
        p = self.ai_proc
        if p is None or self.ai_pgid is None:
            return
        was_running = p.poll() is None
        if not was_running and not self._group_alive():
            return
        self._signal_ai(signal.SIGTERM)
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline and (p.poll() is None or self._group_alive()):
            time.sleep(0.2)
        killed = False
        if p.poll() is None or self._group_alive():
            killed = True
            self._signal_ai(signal.SIGKILL)
            try:
                p.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
        if was_running:
            self.ai_status["status"] = f"stopped at the end ({p.poll()})"
        self.record("ai_stop", code=p.poll(), leader_was_running=was_running, killed=killed)

    def finish_ai_dir(self) -> None:
        """Keep the AI's notes with the run (situation.jsonl has them too) and remove its
        directory."""
        d = self.ai_dir
        if d is None:
            return
        # the AI's notes, and the playbook engine's ledgers (AI=playbook, or its shadow)
        # (a ledger of another bridge run, set aside beside it, too: playbook-ledger.<run>.json)
        kept = sorted(f.name for f in d.glob("playbook*-ledger.*.json"))
        for name in ("agent.jsonl", "playbook-ledger.json", "playbook-shadow-ledger.json", *kept):
            f = d / name
            try:
                # a plain file only: a link is not followed
                if f.is_file() and not f.is_symlink():
                    shutil.copyfile(f, self.out / name, follow_symlinks=False)
            except OSError as e:
                self.record("warning", text=f"could not keep the AI's {name}: {e}")
        shutil.rmtree(d, ignore_errors=True)
        self.record("ai_dir_removed", dir=str(d))
        self.ai_dir = None

    def scripted(self) -> Any:
        """The autoplay operator (op-auto, the proxy model, with op-2 and op-3 signing
        engagements) or, live with a scripted cosigner, the cosigner alone; else nothing."""
        label = f"scripted (situation {self.sit.name}, {self.modes['operator']})"
        if self.modes["operator"] == "autoplay":
            signers = EngagementSigners(self.bridge, list(SCRIPTED_SIGNERS), mode="sign", label=label, log=self.autoplay_log)
            proxy = ProxyOperator(
                self.bridge,
                proxy_params(self.sit.autoplay),
                self.sit.seed,
                log=self.autoplay_log,
                operator=AUTOPLAY_OPERATOR,
                signers=signers,
                note_prefix=f"{label}: {AUTOPLAY_OPERATOR} (not a person)",
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
            proposer = ScriptedOperatorProposer(self.bridge, STORYLINE_OPERATOR, SCRIPTED_TIMELINE, self.scripted_ids, alarm=self.problem)
        else:
            proposer = FreshAgentProposer(f"{self.bridge_url}/mcp", agent=STORYLINE_AGENT, label=SCRIPTED_TIMELINE)
        story = Storyline(
            self.sit,  # type: ignore[arg-type]  (a Situation has the spec's steps)
            TimelineBridge(self.bridge_url),
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
        for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            loop.add_signal_handler(sig, self.request_stop, sig)
        vehicles, _ = plan_counts(self.sit.fleet)  # type: ignore[arg-type]
        self.record(
            "start",
            situation=self.sit.name,
            file=rel(self.sit.path),
            modes=self.modes,
            preflight=self.preflight,
            ports=self.ports,
            run_id=self.run_id,
            fleet=[{"uid": v["uid"], "callsign": v["callsign"], "platform": v["platform"]} for v in vehicles],
            evidence_note=self.modes.get("evidence_note"),
        )
        removed = self.fresh_station()
        say("fresh station: " + ("removed " + "; ".join(removed) if removed else "no restore or resume pointer for this port") + "; MURMUR_RESUME=0")
        self.start_stack()
        tasks: list[asyncio.Task] = []
        story_stop = asyncio.Event()
        status = 0
        try:
            await self.take_zero()
            tasks.append(self.watch_task("the status publisher", self.publisher()))
            await self.wait_for("the bridge", self.bridge.alive, 600)
            self.entry("phase", "stack up")
            await self.wait_ready(vehicles)
            await self.issue_orders()
            self.set_phase("running", "the situation runs")
            scripted = self.scripted()
            if scripted is not None:
                name = "the autoplay operator" if self.modes["operator"] == "autoplay" else "the scripted cosigner"
                tasks.append(self.watch_task(name, scripted.run(story_stop)))
            story, _ = self.storyline()
            if story is not None:
                tasks.append(self.watch_task("the timeline", story.run(story_stop)))
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
                    raise SituationError(f"the stack stopped; see {rel(self.out / 'up.log')}")
                if not ai_started and time.time() >= ai_at:
                    ai_started = True
                    try:
                        self.start_ai()
                    except (OSError, SituationError) as e:
                        self.ai_status = {"status": f"did not start: {e}"}
                        self.problem(f"the AI did not start: {e}")
                await asyncio.sleep(1.0)
            if self.interrupted:
                status = 130
                self.set_phase("interrupted", "the runner was stopped; the stack is still running: make down stops it")
            else:
                self.set_phase("ended", "the situation's time is up; the stack is still running for inspection: make down stops it")
        except SituationError as e:
            status = 130 if self.interrupted else 1
            phase = "interrupted" if self.interrupted else "failed"
            self.set_phase(phase, str(e) if not self.interrupted else "the runner was stopped before the situation ran")
            say(f"{phase}: {e}")
        except Exception as e:  # noqa: BLE001 (recorded and summarized; the stack stays up)
            status = 1
            self.set_phase("failed", f"the runner failed ({type(e).__name__}); see situation.jsonl")
            self.record("error", error=repr(e))
            say(f"failed: {e!r}")
        finally:
            # first: the publisher waits on this, so it returns at once
            self.stop.set()
            story_stop.set()
            self.stop_ai()
            if tasks:
                _, pending = await asyncio.wait(tasks, timeout=15)
                for t in pending:
                    self.record("warning", text=f"{t.get_name()} did not stop within 15 s: cancelled")
                    t.cancel()
            self.refresh()
            self.finish_ai_dir()
            if self.bridge.alive():
                await asyncio.to_thread(self.post_doc)
            self.summary()
        return status

    def request_stop(self, sig: int = signal.SIGINT) -> None:
        if not self.stop.is_set():
            # first: a hung-up terminal (SIGHUP) can fail the line below
            self.interrupted = True
            self.stop.set()
            self.record("signal", signal=signal.Signals(sig).name)
            say(f"stopping the runner ({signal.Signals(sig).name}; the stack keeps running: make down stops it)")

    # ---- the summary -------------------------------------------------------------------------

    def summary(self) -> None:
        events = AuditTail(self.audit).read() if self.audit.exists() else []
        intents = {i["id"]: i for i in ((self.bridge.try_get("/api/intents") or {}).get("intents", []))}
        created = [e for e in events if e.get("type") == "intent.created"]
        by_source: dict[str, list[int]] = {}
        for e in created:
            i = e.get("intent") or {}
            by_source.setdefault(f"{(i.get('source') or {}).get('kind')} {(i.get('source') or {}).get('id')}", []).append(i.get("id"))
        decided: dict[str, int] = {}
        for e in events:
            if e.get("type") in DECISION_EVENTS:
                decided[str(e.get("by"))] = decided.get(str(e.get("by")), 0) + 1
        lines = [
            "",
            f"=== {self.sit.title}: {self.phase} ===",
            f"modes: operator {self.modes['operator']}, AI {AI_LABEL[self.modes['ai']]}, cosigner {self.modes['cosigner']}, reveal {self.modes['reveal']}",
        ]
        if self.modes.get("evidence_note"):
            lines.append(f"NOTE: {self.modes['evidence_note']}")
        lines += [
            f"scripted ids (not people): {', '.join(self.modes['scripted_operators']) or 'none'}",
            f"run id: {self.run_id}  (situation time {self.elapsed() or 0:.0f} s of {self.sit.duration_s:.0f})",
            f"situation log: {rel(self.log_path)}",
            f"audit log: {rel(self.audit)}",
            "replay (after `make down`, so the log is closed and its shutdown anchor written):",
            f"  target/release/murmur-replay {rel(self.audit)} --anchors {rel(self.anchors)}",
            "what happened:",
        ]
        for o in self.orders:
            now = intents.get(o.get("intent_id"), {}).get("state")
            by = f" by {o['by']}" if o.get("by") else ""
            routed = f" [{o['routed']}]" if o.get("routed") else ""
            lines.append(f"  opening order {o['id']} ({o['task']} for {', '.join(o.get('vehicles') or ['?'])}){by}: intent {o.get('intent_id')} {now or o.get('state') or o.get('error')}{routed}")
        lines.append(
            "  decisions in the audit log by: "
            + (", ".join(f"{k} {n}" for k, n in sorted(decided.items())) or "nobody")
            + f" ({CONSOLE_OPERATOR} is the person at the console)"
        )
        agent = self.modes["agent"]
        mine = [i for i in intents.values() if (i.get("source") or {}).get("id") == agent] if agent else []
        lines.append(f"  AI {AI_LABEL[self.modes['ai']]}: {self.ai_status.get('status')}; {len(self.activity)} actions in the audit log, {len(mine)} proposals")
        for i in mine:
            ap = i.get("approval") or {}
            who = f", approved by {ap.get('by')}" if ap.get("by") else ""
            lines.append(f"    intent {i['id']} {i.get('kind')} for vehicles {','.join(str(u) for u in i.get('uids') or [])}: {i.get('state')} (tier {(i.get('policy') or {}).get('rule')}{who})")
            if i.get("rationale"):
                lines.append(f"      rationale: {str(i['rationale'])[:220]}")
        if self.modes["operator"] == "autoplay" or self.modes["cosigner"] == "scripted":
            acts = ("approve", "reject", "sign", "confirm")
            ok = [d for d in self.autoplay_log if d.get("action") in acts and not str(d.get("result") or "").startswith("error")]
            failed = [d for d in self.autoplay_log if d not in ok and d.get("action") in (*acts, "sign_failed", "error")]
            lines.append(f"  scripted decisions: {len(ok)}" + (f" (and {len(failed)} failed attempts)" if failed else ""))
            for d in ok[-12:]:
                why = f" ({str(d['reason'])[:120]})" if d.get("reason") else ""
                lines.append(f"    {decision_text(d)}{why}")
        # everything else: the storyline's proposals, an operator's own orders at the console
        known = {o.get("intent_id") for o in self.orders} | {i["id"] for i in mine}
        others = [i for i in intents.values() if i["id"] not in known]
        for i in sorted(others, key=lambda i: i["id"]):
            src = i.get("source") or {}
            lines.append(f"  intent {i['id']} {i.get('kind')} for vehicles {','.join(str(u) for u in i.get('uids') or [])} from {src.get('kind')} {src.get('id')}: {i.get('state')}")
        chain = self.chain.summary_lines()
        if chain:
            lines.append("  the engagement chain (audit log):")
            lines += ["  " + c for c in chain]
        steps = [e for e in self.entries if e["kind"] == "step"]
        lines.append(f"  timeline: {len(steps)} step records, {len(self.done_steps)} of {len(self.sit.steps)} steps run")
        for e in steps:
            s = self.steps.get(str(e.get("step")))
            if s is not None and s.kind == "engage":
                lines.append(f"    step {s.id} at {e['t_s']:.0f} s: {e['text'][:300]}")
        alarms = [p for p in self.problems if p.startswith("SCRIPT AUTHORIZED")]
        if alarms:
            lines.insert(3, f"!!! {len(alarms)} order(s) authorized by a script's own submission: see the problems below and situation.jsonl")
        if self.problems:
            lines.append(f"  problems in the runner: {len(self.problems)}")
            lines += [f"    {p[:200]}" for p in self.problems[-6:]]
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
        # kept first: the terminal may be gone (SIGHUP)
        (self.out / "summary.txt").write_text(text + "\n")
        self.record("end", phase=self.phase, summary=lines)
        try:
            print(text, flush=True)
        except OSError:
            pass


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
    pointers = pointer_problems(ports["http"])
    if pointers:
        raise SituationError("refusing to start: " + "; ".join(pointers) + ".")
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
    logging.getLogger("mcp.client.streamable_http").addFilter(_SessionCloseAccepted())
    try:
        sit = load_situation(args.name)
        if sit.todo:
            raise SituationError(f"{sit.name} is a stub (todo = true): only its title and brief are written yet")
        modes = resolve_modes(sit, args.operator, args.ai, args.cosigner, None if args.reveal is None else args.reveal == "1")
        env = situation_env(dict(os.environ))
        pf = preflight(sit, modes, env)
    except SituationError as e:
        print(f"situation: {e}", file=sys.stderr)
        return 2
    runner = Runner(sit, modes, env, pf)
    p = runner.ports
    cosigner = modes["cosigner"] + (f" ({', '.join(SCRIPTED_SIGNERS)})" if modes["cosigner"] == "scripted" and modes["operator"] == "live" else "")
    say(f"{sit.title}: operator {modes['operator']}, AI {modes['ai_label']}, cosigner {cosigner}, reveal {'on' if modes['reveal'] else 'off'}; run {runner.run_id}")
    say(f"scripted ids (not people): {', '.join(modes['scripted_operators']) or 'none'}; {CONSOLE_OPERATOR} is the person at the console")
    if modes.get("evidence_note"):
        say(f"NOTE: {modes['evidence_note']}")
    say(f"console http://127.0.0.1:{p['web']} (bridge :{p['http']}, link emulator :{p['linkemu']}, tidewater :{p['tidewater']}, vehicles UDP {p['vehicle_base']}+ and {p['bridge_base']}+)")
    if (os.environ.get("MURMUR_TIDEWATER_URL") or "").strip():
        say(f"MURMUR_TIDEWATER_URL is set in this shell: not passed on; the situation's tidewater runs on port {p['tidewater']}")
    if pf.get("checked"):
        say(f"AI preflight: {modes['ai']} logged in ({pf.get('auth_method')}); Murmur only runs your own CLI and never reads its credentials")
    elif pf.get("note"):
        say(f"AI preflight: {pf['note']}")
    return asyncio.run(runner.run())


if __name__ == "__main__":
    sys.exit(main())
