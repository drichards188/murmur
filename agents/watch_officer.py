#!/usr/bin/env python3
"""Murmur's AI watch officer: a model as the operator's AI teammate, through the bridge's MCP
server, on the user's own Claude Code (`claude -p`, the default) or Codex CLI (`codex exec`).

    .venv/bin/python agents/watch_officer.py --backend claude --url http://127.0.0.1:8765/mcp \\
        --agent agent-watch --brief brief.md --situation-log logs/situation.jsonl --until <unix time>

It reads the picture as agent `--agent` (its key from --key or MURMUR_AGENT_KEY in a
credentials run), wakes the model when something worth a look changed (the trigger table in
agents/watch/wake.py) or on a slow heartbeat, gives the stronger model the consequential
moments (engagement chain, a lost link on an armed platform, a suspected spoof), enforces
budgets, and writes every turn to the situation log (JSONL; agents/watch/situation.py lists
its lines). The model proposes through the same tools as any agent: it cannot approve or
sign anything, and humans sign every effect.

Murmur never stores, reads, proxies or prompts for model credentials: it runs the CLI you
installed and logged into yourself, with the API-key variables removed from its environment
so it uses your subscription. `--preflight` checks the CLI and its login and exits;
`--print-command` prints the exact locked-down command lines without running a model.

Stops at --until, on SIGTERM or SIGINT, when a budget is spent, or on a usage limit whose
reset comes after --until (with --on-limit wait, the default, it sleeps until an earlier
reset and resumes the session).
"""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import os
import signal
import sys
import tempfile
import time
from dataclasses import asdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from watch.backend import BackendCrashed, LimitReached, LockdownBreach, PreflightError  # noqa: E402
from watch.claude import ClaudeBackend, ClaudeConfig  # noqa: E402
from watch.codex import CodexBackend, CodexConfig  # noqa: E402
from watch.limits import LimitHit  # noqa: E402
from watch.picture import NoChangesTool, Poller, compose, render_mine  # noqa: E402
from watch.situation import SituationLog  # noqa: E402
from watch.wake import Budgets, Waker, Wake  # noqa: E402

PROMPT = HERE / "watch" / "prompt.md"
DEFAULTS = {
    "claude": {"model": "sonnet", "effort": "medium", "strong_model": "opus", "strong_effort": "high"},
    "codex": {"model": "gpt-6-luna", "effort": "high", "strong_model": "gpt-6.1-sol", "strong_effort": "high"},
}
CONSULT_HEAD = (
    "You are consulted as the senior watch officer for one consequential moment: {why}. The "
    "routine watch officer handles everything else and will be told what you did. Read the "
    "picture yourself before you act; your own earlier proposals (agent {agent}) are in "
    "get_intents with mine.\n\n")


class Stopped(Exception):
    pass


def sha256(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()


def build_prompt(brief: str | None) -> str:
    base = PROMPT.read_text()
    return base.rstrip() + "\n\n## Situation brief\n\n" + ((brief or "").strip() or "(no brief given)") + "\n"


class RunDirError(ValueError):
    pass


# What may not be in the run directory or above it: instructions a CLI reads by walking up
# (CLAUDE.md, AGENTS.md), project settings with their hooks (.claude), a repository root
# (.git). The user's own ~/.claude is their configuration directory, which
# `--setting-sources project` keeps out, so it is allowed above a run directory.
FORBIDDEN_ABOVE = ("CLAUDE.md", "AGENTS.md", ".claude", ".git")


def run_dir_problems(d: Path) -> list[str]:
    # both resolved: a home or a run directory reached by a symlink is the same directory
    home = Path.home().resolve()
    d = d.expanduser().resolve()
    found = []
    for p in [d, *d.parents]:
        for name in FORBIDDEN_ABOVE:
            if name == ".claude" and p == home:
                continue
            if (p / name).exists():
                found.append(str(p / name))
    return found


def make_run_dir(given: Path | None) -> Path:
    """An empty directory with nothing at or above it that a CLI would read. A directory the
    driver creates is made private (0700); one given that already exists must be empty, and
    is left as it is."""
    if given is None:
        # 0700, new, empty; resolved, since TMPDIR (or a home) may be reached by a symlink
        d = Path(tempfile.mkdtemp(prefix="murmur-watch-")).resolve()
        problems = run_dir_problems(d)
        if problems:
            d.rmdir()
            raise RunDirError(f"the temporary directory {d.parent} has {', '.join(problems)} at or above it: "
                              "pass --run-dir somewhere clear of them (or set TMPDIR)")
        return d
    d = given.expanduser().resolve()
    problems = run_dir_problems(d)
    if problems:
        raise RunDirError(f"--run-dir {d} has {', '.join(problems)} at or above it: the CLI would read "
                          "them; pick a directory outside any repository or project")
    if d.exists():
        if not d.is_dir() or any(d.iterdir()):
            raise RunDirError(f"--run-dir {d} must be a new or empty directory")
        return d
    d.mkdir(parents=True)
    os.chmod(d, 0o700)
    return d


class Officer:
    def __init__(self, args, backend, log: SituationLog, waker: Waker, poller: Poller, shadow,
                 until: float):
        self.args = args
        self.backend = backend
        self.log = log
        self.waker = waker
        self.poller = poller
        self.shadow = shadow
        self.until = until
        self.stop = asyncio.Event()
        self.stop_reason: str | None = None
        self.consult_notes: list[str] = []
        self.crashes: list[float] = []
        self.strong_blocked_until = 0.0
        self.init_logged = False
        self.poll_errors = 0
        self.turns = 0
        self.proposed: list[int] = []
        self._deferred_logged = False
        self.bridge_restarted = False
        self.limits: list[float] = []  # usage limits hit in the last hour
        self.fail_streak = 0  # failed turns in a row
        self.backoff_s = 0.0  # the last wait after one
        self.backoff_until = 0.0  # no wake before this

    def signal(self, name: str) -> None:
        if not self.stop.is_set():
            self.stop_reason = f"signal {name}"
            self.stop.set()

    async def sleep(self, seconds: float) -> None:
        if seconds <= 0:
            return
        try:
            await asyncio.wait_for(self.stop.wait(), seconds)
        except asyncio.TimeoutError:
            pass

    async def race(self, coro):
        """Run `coro` unless a stop comes first (then the backend is stopped and Stopped raised)."""
        task = asyncio.ensure_future(coro)
        stopper = asyncio.ensure_future(self.stop.wait())
        done, _ = await asyncio.wait({task, stopper}, return_when=asyncio.FIRST_COMPLETED)
        if task in done:
            stopper.cancel()
            return task.result()
        await self.backend.stop()
        task.cancel()
        try:
            await task
        except BaseException:  # noqa: BLE001 (the turn was cut short on purpose)
            pass
        raise Stopped()

    # ---- the bridge ------------------------------------------------------------------------

    async def connect(self) -> dict | None:
        delay = 2.0
        while not self.stop.is_set() and time.time() < self.until:
            try:
                return await self.poller.open()
            except NoChangesTool:
                raise
            except Exception as e:  # noqa: BLE001 (the bridge may still be starting)
                self.log.write("poll.error", error=f"{type(e).__name__}: {e}", retry_in_s=delay)
                await self.sleep(delay)
                delay = min(delay * 2, 30.0)
        return None

    async def poll_loop(self) -> None:
        every = self.args.poll_s
        while not self.stop.is_set():
            try:
                changes = await self.poller.poll()
                changes += await self.poller.nominations()
                self.poll_errors = 0
                if any(isinstance(c, dict) and c.get("kind") == "feed.reset" for c in changes):
                    self.bridge_restarted = True
                if changes:
                    self.waker.add(changes, time.time())
                    self.log_dropped()
            except Exception as e:  # noqa: BLE001 (keep polling; reconnect after a few misses)
                self.poll_errors += 1
                if self.poll_errors in (1, 5) or self.poll_errors % 30 == 0:
                    self.log.write("poll.error", error=f"{type(e).__name__}: {e}", misses=self.poll_errors)
                if self.poll_errors >= 3:
                    try:
                        info = await self.poller.open()
                        self.log.write("bridge.reconnected", cursor=info["cursor"], bridge_run=info["run"],
                                       reset=info["reset"])
                        if info.get("reset"):
                            self.bridge_restarted = True
                            self.waker.add([{"kind": "feed.reset", "t": time.time(), "run": info.get("run")}],
                                           time.time())
                    except Exception:  # noqa: BLE001
                        pass
            await self.sleep(every)

    # ---- the run ---------------------------------------------------------------------------

    async def run(self) -> str:
        try:
            info = await self.connect()
        except NoChangesTool as e:
            self.log.write("error", error=str(e))
            return "error"
        if info is None:
            return self.stop_reason or "until"
        # bridge_run: the bridge's audit-log run (a line's own run is this driver's)
        self.log.write("bridge.connected", cursor=info["cursor"], bridge_run=info["run"], tools=info["tools"])
        if hasattr(self.backend, "murmur_tools"):
            self.backend.murmur_tools = set(info["tools"])
        started = await self.backend.start()
        self.log.write("backend.start", **{k: v for k, v in started.items() if k != "argv"})
        poller = asyncio.ensure_future(self.poll_loop())
        try:
            while True:
                if self.stop.is_set():
                    return self.stop_reason or "signal"
                now = time.time()
                if now >= self.until:
                    return "until"
                if self.waker.exhausted():
                    self.log.write("budget.exhausted", what="wakes per run", max_wakes=self.waker.budgets.max_wakes)
                    return "budget"
                if now < self.backoff_until:
                    # after a failed turn: changes keep arriving, the wake waits
                    await self.sleep(min(self.backoff_until - now, self.args.poll_s))
                    continue
                wake = self.waker.due(now)
                self.log_dropped()
                if wake is None:
                    if self.waker.deferred and not self._deferred_logged:
                        self._deferred_logged = True
                        self.log.write("budget.deferred", what="wakes per hour",
                                       max_wakes_per_hour=self.waker.budgets.max_wakes_per_hour,
                                       pending=len(self.waker.pending))
                    # changes arrive with every poll: look again at least that often
                    nxt = min(self.waker.next_check(now), self.until, now + self.args.poll_s)
                    await self.sleep(max(0.05, nxt - now))
                    continue
                self._deferred_logged = False
                try:
                    outcome = await self.do_wake(wake)
                except Stopped:
                    return self.stop_reason or "signal"
                if outcome:
                    return outcome
        finally:
            poller.cancel()
            try:
                await poller
            except BaseException:  # noqa: BLE001
                pass
            await self.backend.stop()
            await self.poller.close()

    def budget_line(self, wake: Wake) -> str:
        b = self.waker.budgets
        left = max(0, b.max_escalations - self.waker.escalations)
        return (f"Budget: this is wake {wake.n} of at most {b.max_wakes} this run ({b.max_wakes_per_hour} an "
                f"hour); you have up to {b.max_turns_per_wake} tool turns for this update; "
                f"{left} escalations to the senior model left.")

    def failed(self, now: float, crashed: bool) -> str | None:
        """A failed turn (an error result, or a crash): wait before the next wake, doubling with
        each failure in a row (`--error-backoff-s`, up to `--error-backoff-max-s`), so an API
        overload of a minute costs a few waits, not the watch. The run stops only on repeated
        crashes (more than three in ten minutes) or on failures that go on after a wait at
        the ceiling; the reason says which."""
        if crashed:
            self.crashes = [t for t in self.crashes if now - t < 600] + [now]
            if len(self.crashes) > 3:
                return "crashes: the CLI died more than three times in ten minutes"
        ceiling = self.args.error_backoff_max_s
        if self.fail_streak and self.backoff_s >= ceiling:
            return (f"errors: turns kept failing after a {ceiling:.0f} s wait "
                    f"({self.fail_streak + 1} in a row)")
        self.fail_streak += 1
        self.backoff_s = min(self.args.error_backoff_s * 2 ** (self.fail_streak - 1), ceiling)
        self.backoff_until = now + self.backoff_s
        self.log.write("backoff", wait_s=round(self.backoff_s, 1), failures_in_a_row=self.fail_streak,
                       crashed=crashed)
        return None

    def log_dropped(self) -> None:
        for change, error in self.waker.take_dropped():
            self.log.write("change.dropped", error=error, change=str(change)[:500])

    async def do_wake(self, wake: Wake) -> str | None:
        now = time.time()
        if self.bridge_restarted:
            # the bridge restarted: a long-lived CLI holds a dead MCP session; a new process
            # (the same conversation) connects afresh, and its MCP status is checked again
            self.bridge_restarted = False
            info = await self.backend.restart()
            self.init_logged = False
            self.log.write("backend.restart", why="the bridge restarted", resume=info.get("resume"))
        escalate = wake.escalate and now >= self.strong_blocked_until
        if wake.escalate and not escalate:
            # the stronger model is limited: the routine one takes it, and nothing is spent
            self.waker.unescalate(wake)
        try:
            mine = render_mine(await self.poller.mine(), now)
        except Exception:  # noqa: BLE001 (the update goes out without the list)
            mine = ["(could not read your intents: use get_intents with mine)"]
        notes = [] if escalate else list(self.consult_notes)
        text = compose(wake, mine, self.budget_line(wake), notes, now, first=wake.reason == "start")
        if escalate:
            text = CONSULT_HEAD.format(why="; ".join(wake.escalate_why), agent=self.args.agent) + text
        if self.shadow is not None:
            await self.run_shadow(wake)
        cfg = self.backend.cfg
        model = cfg.strong_model if escalate else cfg.model
        effort = cfg.strong_effort if escalate else cfg.effort
        self.log.write("wake", n=wake.n, reason=wake.reason, why=wake.why, escalated=escalate,
                       escalate_why=wake.escalate_why if escalate else [], model=model, effort=effort,
                       update=text, changes=wake.changes, cursor=self.poller.cursor)
        deadline = min(now + self.args.turn_timeout_s, self.until)
        try:
            if escalate:
                turn = await self.race(self.backend.escalate(text, deadline))
            else:
                turn = await self.race(self.backend.turn(text, deadline))
        except LimitReached as e:
            if e.turn is not None:
                self.log.write("turn", n=wake.n, escalated=escalate, **e.turn.as_dict())
            return await self.on_limit(e.hit, escalate, wake)
        except LockdownBreach as e:
            if e.turn is not None:
                self.log.write("turn", n=wake.n, escalated=escalate, **e.turn.as_dict())
            self.log.write("lockdown.breach", n=wake.n, error=str(e))
            self.log.write("error", n=wake.n, error=f"lockdown breach: {e}; the run stops")
            return "lockdown"
        except BackendCrashed as e:
            # a crashed turn reports no cost: what it spent is missing from the budget's
            # count (the CLI's own estimate comes only with a turn's result)
            self.log.write("backend.crashed", n=wake.n, escalated=escalate, error=str(e), code=e.code,
                           stderr=e.stderr[-2000:], cost="unknown: a crashed turn reports none")
            self.waker.give_back(wake, time.time())  # the next wake tells it again
            stop = self.failed(now, crashed=True)
            if stop:
                self.log.write("error", error=stop)
                return stop.split(":")[0]
            if not escalate:
                info = await self.backend.restart()
                self.log.write("backend.restart", why="crashed", resume=info.get("resume"))
            return None
        self.turns += 1
        self.proposed += turn.proposals()
        self.log.write("turn", n=wake.n, escalated=escalate, **turn.as_dict())
        for r in turn.refused:
            self.log.write("lockdown.refused", n=wake.n, tool=r.get("name"), input=r.get("input"),
                           text=f"the model asked for {r.get('name')}, outside the Murmur server; the CLI refused it")
        for u in turn.unknown_items:
            self.log.write("lockdown.unknown_item", n=wake.n, item_type=u.get("type"), item=u.get("item"))
        if turn.stop == "error":
            # an error turn (overloaded after the CLI's retries, say): what it was told
            # comes back for the next wake, an escalation it spent is returned
            self.waker.give_back(wake, time.time())
            stop = self.failed(now, crashed=False)
            if stop:
                self.log.write("error", error=stop)
                return stop.split(":")[0]
            return None
        self.fail_streak = 0
        self.backoff_s = 0.0
        if not escalate and not self.init_logged and getattr(self.backend, "init", None):
            self.init_logged = True
            self.log.write("backend.init", **self.backend.init)
            if not self.mcp_ok():
                self.log.write("error", error="the model cannot reach the bridge's MCP tools",
                               init=self.backend.init)
                return "error"
        if escalate:
            calls = ", ".join(c.name.removeprefix("mcp__murmur__") for c in turn.tool_calls) or "none"
            ids = ", ".join(f"#{i}" for i in turn.proposals()) or "none"
            self.consult_notes.append(
                f"While you were not woken, the senior watch officer ({turn.model or model}) handled update "
                f"{wake.n} ({'; '.join(wake.escalate_why)}). Its tool calls: {calls}. It proposed: {ids}. "
                f"It said: {(turn.text or '').strip()[:800]}")
        else:
            self.consult_notes = []
        if turn.stop == "budget":
            self.log.write("budget.exhausted", what="claude --max-budget-usd (the CLI's estimate)")
            return "budget"
        cap = getattr(cfg, "max_budget_usd", None)
        spent = getattr(self.backend, "session_cost_usd", 0.0) + getattr(self.backend, "consult_cost_usd", 0.0)
        if cap is not None and spent >= cap:
            self.log.write("budget.exhausted", what="max budget (the CLI's estimate)", spent_usd=round(spent, 4),
                           max_budget_usd=cap)
            return "budget"
        return None

    def mcp_ok(self) -> bool:
        init = self.backend.init or {}
        if "mcp_servers" not in init:  # codex: `required = true` fails the turn instead
            return True
        servers = {s.get("name"): s.get("status") for s in init.get("mcp_servers") or []}
        tools = init.get("tools") or []
        return servers.get("murmur") == "connected" and any(t.startswith("mcp__murmur__") for t in tools)

    def retry_at(self, hit: LimitHit, now: float) -> float:
        """When to try again after a limit: its reset (plus the margin), or the retry time when
        it gives none, never sooner than a floor that doubles with each limit in the last hour
        (60 s, 120 s, ... 30 min): a reset already past cannot loop restart, turn, limit."""
        self.limits = [t for t in self.limits if now - t < 3600] + [now]
        floor = now + min(self.args.limit_floor_s * 2 ** (len(self.limits) - 1), 1800.0)
        wanted = hit.reset_at + self.args.limit_margin_s if hit.reset_at else now + self.args.limit_retry_s
        return max(wanted, floor)

    async def on_limit(self, hit: LimitHit, escalated: bool, wake: Wake) -> str | None:
        now = time.time()
        retry = self.retry_at(hit, now)
        if escalated:
            # the stronger model's limit: the routine one carries on, told what it missed
            self.strong_blocked_until = retry
            self.log.write("limit.hit", **hit.as_dict(), escalated=True, action="routine model only",
                           until=retry)
            self.waker.give_back(wake, now)
            return None
        if self.args.on_limit == "wait" and retry < self.until:
            self.log.write("limit.hit", **hit.as_dict(), action="wait", resume_at=retry)
            await self.backend.stop()
            await self.sleep(retry - time.time())
            if self.stop.is_set():
                return self.stop_reason or "signal"
            info = await self.backend.restart()
            self.log.write("limit.resume", resume=info.get("resume"))
            self.waker.give_back(wake, time.time())
            return None
        self.log.write("limit.hit", **hit.as_dict(), action="stop",
                       why="--on-limit stop" if self.args.on_limit == "stop" else "the reset comes after --until")
        return "limit"

    async def run_shadow(self, wake: Wake) -> None:
        try:
            fleet = await self.poller.session.fleet()
            tracks = await self.poller.session.tracks(include_stale=False)
            for s in self.shadow.observe(fleet, tracks):
                self.log.write("shadow", n=wake.n, playbook=self.shadow.name, **s)
        except Exception as e:  # noqa: BLE001 (the shadow never stops the watch)
            self.log.write("shadow.error", n=wake.n, error=f"{type(e).__name__}: {e}")


def make_backend(args, run_dir: Path, prompt_path: Path, prompt_text: str):
    d = DEFAULTS[args.backend]
    model = args.model or d["model"]
    effort = args.effort or d["effort"]
    strong_model = args.strong_model or d["strong_model"]
    strong_effort = args.strong_effort or d["strong_effort"]
    key = args.key or os.environ.get("MURMUR_AGENT_KEY") or None
    if args.backend == "claude":
        cfg = ClaudeConfig(bin=args.claude_bin, model=model, effort=effort, strong_model=strong_model,
                           strong_effort=strong_effort, max_budget_usd=args.max_budget_usd,
                           allow_other_auth=args.allow_other_auth)
        return ClaudeBackend(cfg, run_dir, args.url, args.agent, key, prompt_path,
                             max_turns=args.max_turns_per_wake)
    cfg = CodexConfig(bin=args.codex_bin, model=model, effort=effort, strong_model=strong_model,
                      strong_effort=strong_effort, home=args.codex_home)
    return CodexBackend(cfg, run_dir, args.url, args.agent, key, prompt_text, max_turns=args.max_turns_per_wake)


async def main_async(args) -> int:
    run_id = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + f"-{os.getpid()}"
    log = SituationLog(args.situation_log, run_id, echo=not args.quiet)
    until = args.until if args.until else time.time() + args.minutes * 60
    brief = args.brief.read_text() if args.brief else None
    prompt_text = build_prompt(brief)
    try:
        run_dir = make_run_dir(args.run_dir)
    except RunDirError as e:
        log.write("error", error=str(e))
        print(f"watch officer: {e}", file=sys.stderr)
        return 2
    prompt_path = run_dir / "system-prompt.md"
    prompt_path.write_text(prompt_text)
    try:
        backend = make_backend(args, run_dir, prompt_path, prompt_text)
    except ValueError as e:
        log.write("error", error=str(e))
        return 2
    if args.print_command:
        import json

        print(json.dumps(backend.describe(), indent=1))
        return 0
    try:
        pre = await backend.preflight()
    except PreflightError as e:
        log.write("preflight.failed", backend=args.backend, problem=e.problem, fix=e.fix)
        print(f"watch officer: {e.problem}.\n  fix: {e.fix}", file=sys.stderr)
        return 2
    log.write("preflight", backend=args.backend, **pre)
    if args.preflight:
        return 0
    budgets = Budgets(max_wakes_per_hour=args.max_wakes_per_hour, max_turns_per_wake=args.max_turns_per_wake,
                      max_escalations=args.max_escalations, max_wakes=args.max_wakes,
                      heartbeat_s=args.heartbeat_s, coalesce_s=args.coalesce_s, min_gap_s=args.min_gap_s)
    waker = Waker(budgets, escalation=not args.no_escalation)
    key = args.key or os.environ.get("MURMUR_AGENT_KEY") or None

    def connect():
        from policy_agent import AgentSession

        return AgentSession(args.url, args.agent, key)

    poller = Poller(connect)
    shadow = None
    if args.shadow_playbook:
        from watch.shadow import PolicyShadow

        near = tuple(float(x) for x in args.shadow_near.split(","))
        shadow = PolicyShadow((near[0], near[1]), args.shadow_radius_km)
    log.write("run.start", pid=os.getpid(), backend=backend.describe(), budgets=asdict(budgets), until=until,
              until_iso=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(until)), url=args.url, agent=args.agent,
              auth="agent key" if key else "dev header", run_dir=str(run_dir),
              brief=str(args.brief) if args.brief else None, brief_sha256=sha256(brief) if brief else None,
              prompt_sha256=sha256(prompt_text), escalation=not args.no_escalation,
              on_limit=args.on_limit, shadow=shadow.name if shadow else None)
    officer = Officer(args, backend, log, waker, poller, shadow, until)
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(sig, officer.signal, sig.name)
    try:
        reason = await officer.run()
    except Exception as e:  # noqa: BLE001 (logged, then the run ends)
        log.write("error", error=f"{type(e).__name__}: {e}")
        reason = "error"
    log.write("run.stop", reason=reason, wakes=waker.wakes, escalations=waker.escalations, turns=officer.turns,
              proposed=officer.proposed,
              session=getattr(backend, "session_id", None) or getattr(backend, "thread_id", None),
              cost_usd_estimate=round(getattr(backend, "session_cost_usd", 0.0)
                                      + getattr(backend, "consult_cost_usd", 0.0), 4) if args.backend == "claude" else None,
              run_dir=str(run_dir))
    log.close()
    return 1 if reason in ("error", "crashes", "errors", "lockdown") else 0


def parse_args(argv: list[str] | None = None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--backend", choices=("claude", "codex"), default="claude")
    ap.add_argument("--url", default="http://127.0.0.1:8765/mcp", help="the bridge's MCP endpoint")
    ap.add_argument("--agent", default="agent-watch", help="the agent id (a dev bridge: listed in MURMUR_AGENTS)")
    ap.add_argument("--key", help="the agent's key in a credentials run (default MURMUR_AGENT_KEY); never logged")
    ap.add_argument("--brief", type=Path, help="the situation brief, appended to the system prompt")
    ap.add_argument("--situation-log", type=Path, help="JSONL log of every wake and turn (appended)")
    ap.add_argument("--until", type=float, help="stop at this Unix time")
    ap.add_argument("--minutes", type=float, default=60.0, help="without --until: stop after this long")
    g = ap.add_argument_group("budgets")
    g.add_argument("--max-wakes-per-hour", type=int, default=60)
    g.add_argument("--max-turns-per-wake", type=int, default=12, help="tool turns the model gets per update")
    g.add_argument("--max-escalations", type=int, default=6, help="consults of the stronger model per run")
    g.add_argument("--max-wakes", type=int, default=400, help="wakes per run, all told")
    g.add_argument("--max-budget-usd", type=float, help="claude: stop when the CLI's own cost estimate reaches this")
    g.add_argument("--heartbeat-s", type=float, default=60.0, help="wake this often with no trigger (0: never); "
                   "doubles while nothing changes, up to 300 s")
    g.add_argument("--poll-s", type=float, default=2.0)
    g.add_argument("--coalesce-s", type=float, default=3.0)
    g.add_argument("--min-gap-s", type=float, default=10.0)
    g.add_argument("--turn-timeout-s", type=float, default=300.0)
    g.add_argument("--on-limit", choices=("wait", "stop"), default="wait",
                   help="on a usage limit: wait for the reset if it comes before --until, or stop")
    g.add_argument("--limit-retry-s", type=float, default=900.0, help="a limit with no reset time: retry after this")
    g.add_argument("--limit-margin-s", type=float, default=60.0, help="resume this long after a limit's reset")
    g.add_argument("--limit-floor-s", type=float, default=60.0,
                   help="never retry sooner than this after a limit; doubles with each limit in the last hour")
    g.add_argument("--error-backoff-s", type=float, default=30.0,
                   help="wait this long after a failed turn; doubles with each failure in a row")
    g.add_argument("--error-backoff-max-s", type=float, default=900.0,
                   help="the longest wait; a failure after a wait this long ends the run")
    m = ap.add_argument_group("models")
    m.add_argument("--model", help="routine model (claude: sonnet; codex: gpt-6-luna)")
    m.add_argument("--effort", help="routine effort (claude: medium; codex: high)")
    m.add_argument("--strong-model", help="the escalation model (claude: opus; codex: gpt-6.1-sol)")
    m.add_argument("--strong-effort", help="the escalation effort (high)")
    m.add_argument("--no-escalation", action="store_true", help="never use the stronger model")
    c = ap.add_argument_group("CLIs")
    c.add_argument("--claude-bin", default=os.environ.get("MURMUR_CLAUDE_BIN", "claude"))
    c.add_argument("--codex-bin", default=os.environ.get("MURMUR_CODEX_BIN", "codex"))
    c.add_argument("--codex-home", default=os.environ.get("MURMUR_CODEX_HOME") or None,
                   help="CODEX_HOME for codex (default MURMUR_CODEX_HOME; unset: your normal home)")
    c.add_argument("--allow-other-auth", action="store_true",
                   help="claude: accept a login other than a Claude subscription (it may bill an API account)")
    c.add_argument("--run-dir", type=Path,
                   help="the CLI's working directory: new or empty, with no CLAUDE.md, AGENTS.md, .claude or "
                        ".git at or above it (default: a fresh temporary one)")
    c.add_argument("--preflight", action="store_true", help="check the CLI and its login, then exit")
    c.add_argument("--print-command", action="store_true", help="print the backend's command lines and exit")
    s = ap.add_argument_group("shadow")
    s.add_argument("--shadow-playbook", action="store_true",
                   help="log what the deterministic policy agent would propose at each wake (never proposes)")
    s.add_argument("--shadow-near", default="-122.4780,37.8180", help="its watch point, lon,lat")
    s.add_argument("--shadow-radius-km", type=float, default=4.0)
    ap.add_argument("--quiet", action="store_true", help="no lines on stderr")
    return ap.parse_args(argv)


def main() -> None:
    sys.exit(asyncio.run(main_async(parse_args())))


if __name__ == "__main__":
    main()
