"""Claude Code (`claude -p`) as the watch officer's backend.

One long-lived process for the routine model, fed one user message per wake over
stream-json; the stronger model is a separate short consult process per escalation (see
`escalate`). The user's own login (a Claude subscription) is what runs it: the child's
environment has ANTHROPIC_API_KEY and ANTHROPIC_AUTH_TOKEN removed (either would bill an API
account), `--bare` is never passed (it ignores the subscription login), and the preflight
refuses an `authMethod` other than `claude.ai` unless told otherwise.

Locked down: only the bridge's MCP server (`--mcp-config`, `--strict-mcp-config`), no
built-in tools (`--tools ""`), only `mcp__murmur__*` allowed, the Agent tool denied,
`--permission-mode dontAsk` with nobody to answer a prompt (`--permission-prompts none`):
anything not allowed is refused, never asked. Never `bypassPermissions`.
`--setting-sources project` with an empty run directory outside any repository keeps the
user's settings out: their CLAUDE.md, hooks and plugins (checked with 2.1.294: no user
CLAUDE.md in context, no hook ran, only built-in plugins). `--safe-mode` would do it too but
also drops the `--mcp-config` server (checked), so it is not used.

What 2.1.294 was seen to do (agents/README.md has the full list): the user message shape
below is accepted; `--max-turns` applies per message and an exceeded one ends that turn
(`error_max_turns`) with the process alive; the stream-json interrupt (a
`control_request` of subtype `interrupt`) ends the turn in flight and keeps the process,
while SIGINT ends the turn and the process; `--max-budget-usd` is checked against the
process's cumulative cost after each turn (`error_max_budget_usd`, then the process exits);
`--resume <id>` brings the conversation back after the process is gone (same session id),
but `total_cost_usd` starts again from zero in the new process, so the spend is carried here.
"""

from __future__ import annotations

import asyncio
import json
import os
import time
import uuid
from dataclasses import dataclass
from pathlib import Path

from .backend import (FALLBACK_HINT, STREAM_LIMIT, BackendCrashed, LimitReached, LockdownBreach, PreflightError, StderrTail,
                      ToolCall, Turn, child_env, end_process, excerpt, run_quick, version_tuple)
from .limits import from_rate_limit_event, parse_claude

PROVIDER = "claude"
# The version these flags and behaviours were checked with; older ones are refused.
MIN_VERSION = (2, 1, 294)
KEY_ENV = "MURMUR_AGENT_KEY"
# How long a turn may run on after the interrupt before the process is restarted.
INTERRUPT_GRACE_S = 20.0


@dataclass
class ClaudeConfig:
    bin: str = "claude"
    model: str = "sonnet"
    effort: str = "medium"
    strong_model: str = "opus"
    strong_effort: str = "high"
    max_budget_usd: float | None = None  # the CLI's own cost estimate, for the whole run
    allow_other_auth: bool = False
    # MCP tools deferred behind ToolSearch would be out of reach with `--tools ""`: off
    tool_search: bool = False


def mcp_config(url: str, agent: str, with_key: bool) -> dict:
    """The bridge as the only MCP server. The key, when there is one, stays in the child's
    environment: the file names the variable (`${MURMUR_AGENT_KEY}`, which the CLI expands;
    checked with an `X-Murmur-Agent` value given the same way)."""
    headers = {"X-Murmur-Agent": agent}
    if with_key:
        headers["Authorization"] = "Bearer ${" + KEY_ENV + "}"
    return {"mcpServers": {"murmur": {"type": "http", "url": url, "headers": headers}}}


MURMUR_PREFIX = "mcp__murmur__"
# What the CLI says when it does not run a tool: no such tool (`--tools ""` leaves none), or a
# permission denied (`dontAsk`). The model asking is the lockdown working; the tool running
# is a breach.
REFUSALS = ("No such tool available", "<tool_use_error>", "has been denied", "Permission to use")


def refused(block: dict) -> bool:
    """A tool_result that says the CLI did not run the tool."""
    return bool(block.get("is_error")) and any(r in _flatten(block.get("content")) for r in REFUSALS)


def context_of(usage) -> int | None:
    """The context one model call read: its input, cache read and cache write tokens."""
    if not isinstance(usage, dict):
        return None
    n = 0
    for k in ("input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"):
        v = usage.get(k)
        if isinstance(v, (int, float)) and not isinstance(v, bool):
            n += int(v)
    return n or None


def user_message(text: str) -> dict:
    return {"type": "user", "message": {"role": "user", "content": text}, "parent_tool_use_id": None}


def interrupt_request() -> dict:
    """End the turn in flight, keeping the process (what the Agent SDKs send)."""
    return {"type": "control_request", "request_id": f"req_{uuid.uuid4().hex[:12]}",
            "request": {"subtype": "interrupt"}}


class Proc:
    """One `claude -p` process: its events as a queue, its stderr's tail."""

    def __init__(self, proc: asyncio.subprocess.Process):
        self.proc = proc
        self.queue: asyncio.Queue = asyncio.Queue()
        self.stderr = StderrTail(proc.stderr)
        self.reader = asyncio.ensure_future(self._read())

    async def _read(self) -> None:
        assert self.proc.stdout is not None
        while True:
            line = await self.proc.stdout.readline()
            if not line:
                await self.queue.put(None)
                return
            try:
                await self.queue.put(json.loads(line))
            except ValueError:
                await self.queue.put({"type": "_text", "text": line.decode(errors="replace").rstrip()})

    @property
    def alive(self) -> bool:
        return self.proc.returncode is None

    async def write(self, obj: dict) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.write((json.dumps(obj) + "\n").encode())
        await self.proc.stdin.drain()

    async def close(self, wait: float = 10.0) -> int | None:
        try:
            if self.proc.stdin is not None and not self.proc.stdin.is_closing():
                self.proc.stdin.close()
        except Exception:  # noqa: BLE001 (closing is best effort)
            pass
        try:
            await asyncio.wait_for(self.proc.wait(), wait)
        except asyncio.TimeoutError:
            await end_process(self.proc)
        self.reader.cancel()
        return self.proc.returncode


class ClaudeBackend:
    provider = PROVIDER

    def __init__(self, cfg: ClaudeConfig, run_dir: Path, url: str, agent: str, key: str | None,
                 prompt_path: Path, max_turns: int = 12):
        self.cfg = cfg
        self.run_dir = run_dir
        self.url = url
        self.agent = agent
        self.key = key
        self.prompt_path = prompt_path
        self.max_turns = max_turns
        self.mcp_path = run_dir / "mcp.json"
        self.session_path = run_dir / "session.json"
        self.session_id: str | None = None
        self.p: Proc | None = None
        self.init: dict | None = None
        self.version: str | None = None
        self.removed_env: list[str] = []
        # the CLI's cost estimate: `total_cost_usd` counts per process, so earlier processes'
        # last figures are kept in `cost_base` (the session's spend is the sum)
        self.cost_base = 0.0
        self.proc_cost = 0.0
        self.consult_cost_usd = 0.0

    # ---- setup -----------------------------------------------------------------------------

    def env(self) -> dict[str, str]:
        # the bridge marks its tools' answers as up to 150 000 characters
        # (`anthropic/maxResultSizeChars`, `mcp::tools::RESULT_MAX_CHARS`); this lets such an
        # answer through untruncated even at two characters a token (an estimate, not
        # measured: this JSON usually runs nearer three)
        extra = {"CLAUDE_CODE_MAX_RETRIES": "2", "MAX_MCP_OUTPUT_TOKENS": "75000"}
        if not self.cfg.tool_search:
            extra["ENABLE_TOOL_SEARCH"] = "false"
        if self.key:
            extra[KEY_ENV] = self.key
        env, self.removed_env = child_env(PROVIDER, extra)
        return env

    def write_files(self) -> None:
        self.mcp_path.write_text(json.dumps(mcp_config(self.url, self.agent, bool(self.key)), indent=1))
        os.chmod(self.mcp_path, 0o600)

    def argv(self, model: str, effort: str, *, resume: str | None = None, consult: bool = False) -> list[str]:
        a = [self.cfg.bin, "-p",
             "--input-format", "stream-json", "--output-format", "stream-json", "--verbose",
             "--replay-user-messages",
             "--mcp-config", str(self.mcp_path), "--strict-mcp-config",
             "--setting-sources", "project",
             "--tools", "",
             "--allowedTools", "mcp__murmur__*",
             "--disallowedTools", "Agent",
             "--permission-mode", "dontAsk", "--permission-prompts", "none",
             "--system-prompt-file", str(self.prompt_path),
             "--model", model, "--effort", effort,
             "--max-turns", str(self.max_turns)]
        if self.cfg.max_budget_usd is not None and not consult:
            # what is left: a new process counts from zero
            left = max(0.01, self.cfg.max_budget_usd - self.cost_base - self.consult_cost_usd)
            a += ["--max-budget-usd", f"{left:.2f}"]
        if consult:
            a.append("--no-session-persistence")
        if resume:
            a += ["--resume", resume]
        return a

    async def preflight(self) -> dict:
        env = self.env()
        code, out, err = await run_quick([self.cfg.bin, "--version"], env)
        if code == 127:
            raise PreflightError(f"{self.cfg.bin} is not installed or not on PATH",
                                 "install Claude Code (https://code.claude.com), run `claude` once and /login "
                                 "with your Claude account, " + FALLBACK_HINT)
        self.version = (out or err).strip()
        v = version_tuple(self.version)
        if v is None or v < MIN_VERSION:
            raise PreflightError(f"claude {self.version or '?'} is older than {'.'.join(map(str, MIN_VERSION))}, "
                                 "the version the watch officer was tested with",
                                 "update Claude Code (`claude update`), " + FALLBACK_HINT)
        code, out, err = await run_quick([self.cfg.bin, "auth", "status"], env)
        try:
            status = json.loads(out)
        except ValueError:
            status = {}
        if code != 0 or not status.get("loggedIn"):
            raise PreflightError("Claude Code is not logged in",
                                 "run `claude` and /login with your Claude account, " + FALLBACK_HINT)
        method = status.get("authMethod")
        if method != "claude.ai" and not self.cfg.allow_other_auth:
            raise PreflightError(
                f"Claude Code is logged in by {method!r}, not with a Claude subscription (claude.ai)",
                "run `claude` and /login with your Claude subscription, or pass --allow-other-auth "
                "to use this login knowingly (it may bill an API account)")
        return {"cli": self.cfg.bin, "version": self.version, "auth_method": method,
                "subscription": status.get("subscriptionType"),
                "api_key_env_removed": self.removed_env}

    # ---- processes -------------------------------------------------------------------------

    async def _spawn(self, argv: list[str]) -> Proc:
        proc = await asyncio.create_subprocess_exec(
            *argv, stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE, env=self.env(), cwd=str(self.run_dir), limit=STREAM_LIMIT,
            start_new_session=True)  # its own process group: a terminal's Ctrl-C reaches the driver only
        return Proc(proc)

    async def start(self, resume: str | None = None) -> dict:
        """The routine process. Its `system/init` (session, MCP status, tools) comes with the
        first turn."""
        self.write_files()
        argv = self.argv(self.cfg.model, self.cfg.effort, resume=resume)
        self.p = await self._spawn(argv)
        return {"pid": self.p.proc.pid, "argv": argv, "resume": resume}

    async def restart(self) -> dict:
        """After a crash or a limit: a new process resuming the session."""
        await self.stop()
        return await self.start(resume=self.session_id)

    def _remember_session(self, sid: str | None) -> None:
        if sid and sid != self.session_id:
            self.session_id = sid
            self.session_path.write_text(json.dumps({"session_id": sid, "t": time.time()}))

    async def turn(self, text: str, deadline: float) -> Turn:
        """Send one update to the routine session and read until the turn's result."""
        if self.p is None or not self.p.alive:
            raise BackendCrashed("the claude process is not running",
                                 self.p.proc.returncode if self.p else None,
                                 self.p.stderr.text() if self.p else "")
        try:
            turn = await self._turn(self.p, text, deadline, self.cfg.model, routine=True)
        except LimitReached as e:
            if e.turn is not None and e.turn.cost_usd is not None:
                self.proc_cost = e.turn.cost_usd
            raise
        if turn.cost_usd is not None:
            self.proc_cost = turn.cost_usd
        return turn

    @property
    def session_cost_usd(self) -> float:
        """The routine session's spend so far, the CLI's estimate, across its processes."""
        return self.cost_base + self.proc_cost

    async def escalate(self, text: str, deadline: float) -> Turn:
        """One consult on the stronger model, in a fresh short process: the system prompt and
        this update only (it reads the picture itself), not the routine conversation. A
        fork of the routine session would re-read that whole conversation, uncached, on the
        stronger model at every escalation; a fresh consult pays for its own reads only,
        and the routine session is told what it did at its next update."""
        self.write_files()
        p = await self._spawn(self.argv(self.cfg.strong_model, self.cfg.strong_effort, consult=True))
        try:
            turn = await self._turn(p, text, deadline, self.cfg.strong_model, routine=False)
            if turn.cost_usd is not None:
                self.consult_cost_usd += turn.cost_usd
            return turn
        except LimitReached as e:
            # a consult that ended on a limit still cost what it reported
            if e.turn is not None and e.turn.cost_usd is not None:
                self.consult_cost_usd += e.turn.cost_usd
            raise
        finally:
            await p.close()

    async def stop(self) -> None:
        """End the routine process: interrupt a turn in flight, close its input, then
        signals if it lingers."""
        if self.p is not None:
            if self.p.alive:
                try:
                    await self.p.write(interrupt_request())
                except (BrokenPipeError, ConnectionResetError, AssertionError):
                    pass
            await self.p.close(wait=5.0)
        self.p = None
        self.cost_base += self.proc_cost
        self.proc_cost = 0.0

    # ---- one turn --------------------------------------------------------------------------

    async def _turn(self, p: Proc, text: str, deadline: float, model: str, routine: bool) -> Turn:
        t0 = time.monotonic()
        turn = Turn(model=model)
        calls: dict[str, ToolCall] = {}
        interrupted: str | None = None
        grace_until: float | None = None
        last_text = ""
        # text the CLI wrote itself (an API error such as a usage limit comes as an assistant
        # message whose model is "<synthetic>"): the only assistant text read for limits
        cli_text = ""
        breach: ToolCall | None = None
        await p.write(user_message(text))
        while True:
            wait = (grace_until or deadline) - time.time()
            try:
                ev = await asyncio.wait_for(p.queue.get(), max(0.1, wait))
            except asyncio.TimeoutError:
                if interrupted is None:
                    interrupted = "timeout"
                    await p.write(interrupt_request())
                    grace_until = time.time() + INTERRUPT_GRACE_S
                    continue
                code = await end_process(p.proc)
                raise BackendCrashed(f"no result {INTERRUPT_GRACE_S:.0f} s after interrupting the turn",
                                     code, p.stderr.text())
            if ev is None:
                code = await p.proc.wait()
                err = p.stderr.text()
                hit = parse_claude(err) or parse_claude(cli_text) or from_rate_limit_event(turn.rate_limit)
                if hit:
                    turn.stop, turn.error = "limit", hit.text
                    raise LimitReached(hit, turn)
                raise BackendCrashed(f"claude exited ({code}) before the turn ended", code, err)
            turn.events += 1
            kind = ev.get("type")
            if kind == "system" and ev.get("subtype") == "init":
                if routine:
                    self.init = {k: ev.get(k) for k in ("session_id", "model", "tools", "mcp_servers",
                                                        "permissionMode", "apiKeySource", "claude_code_version")}
                    self._remember_session(ev.get("session_id"))
                turn.session = ev.get("session_id")
                turn.model = ev.get("model") or turn.model
            elif kind == "assistant":
                msg = ev.get("message") or {}
                turn.model = msg.get("model") or turn.model
                ctx = context_of(msg.get("usage"))
                if ctx is not None:
                    turn.context_tokens = max(turn.context_tokens or 0, ctx)
                for block in msg.get("content") or []:
                    if block.get("type") == "tool_use":
                        c = ToolCall(block.get("name", "?"), block.get("input"), block.get("id"))
                        calls[c.id or str(len(calls))] = c
                        turn.tool_calls.append(c)
                    elif block.get("type") == "text" and block.get("text"):
                        last_text = block["text"]
                        if msg.get("model") == "<synthetic>":
                            cli_text = block["text"]
            elif kind == "user":
                content = (ev.get("message") or {}).get("content")
                if isinstance(content, list):
                    for block in content:
                        if isinstance(block, dict) and block.get("type") == "tool_result":
                            c = calls.get(block.get("tool_use_id"))
                            if c is not None:
                                c.result = excerpt(_flatten(block.get("content")))
                                c.is_error = bool(block.get("is_error"))
                                if not c.name.startswith(MURMUR_PREFIX):
                                    if refused(block):
                                        turn.refused.append({"name": c.name, "input": excerpt(c.input, 300)})
                                    elif breach is None:
                                        # a tool outside the Murmur server ran: stop the turn now
                                        breach = c
                                        await p.write(interrupt_request())
            elif kind == "rate_limit_event":
                turn.rate_limit = ev.get("rate_limit_info")
            elif kind == "result":
                done = self._finish(turn, ev, last_text, cli_text, interrupted, t0, routine)
                if breach is not None:
                    done.stop = "lockdown"
                    done.error = (f"the model used {breach.name}, a tool outside the Murmur server, and "
                                  f"the CLI ran it: {excerpt(breach.result, 300)}")
                    raise LockdownBreach(done.error, done)
                return done

    def _finish(self, turn: Turn, ev: dict, last_text: str, cli_text: str, interrupted: str | None,
                t0: float, routine: bool) -> Turn:
        turn.text = ev.get("result") or last_text
        turn.usage = ev.get("usage") or {}
        if ev.get("modelUsage"):
            turn.usage = {**turn.usage, "model_usage": ev.get("modelUsage")}
        turn.cost_usd = ev.get("total_cost_usd")
        turn.num_turns = ev.get("num_turns")
        turn.session = ev.get("session_id") or turn.session
        if routine:
            self._remember_session(turn.session)
        sub = ev.get("subtype") or ""
        if interrupted:
            turn.stop = interrupted
        elif sub == "success" and not ev.get("is_error"):
            turn.stop = "success"
        elif sub == "error_max_turns":
            turn.stop = "max_turns"
        elif sub == "error_max_budget_usd":
            turn.stop = "budget"
        else:
            turn.stop = "error"
        if ev.get("is_error"):
            turn.error = ev.get("result") or sub or "error"
        turn.duration_s = time.monotonic() - t0
        rejected = (turn.rate_limit or {}).get("status") == "rejected"
        if turn.stop not in ("max_turns", "budget") and (ev.get("is_error") or rejected):
            # the CLI's own words only: its result and its synthetic messages, never the
            # model's prose (a model narrating "limit reached" is not a limit)
            hit = (parse_claude(ev.get("result") or "") or parse_claude(cli_text)
                   or from_rate_limit_event(turn.rate_limit))
            if hit:
                exact = (turn.rate_limit or {}).get("resetsAt")
                if isinstance(exact, (int, float)) and (turn.rate_limit or {}).get("status") == "rejected":
                    hit.reset_at = float(exact)  # the stream's own figure beats the text's
                turn.stop = "limit"
                raise LimitReached(hit, turn)
        return turn

    def describe(self) -> dict:
        return {"provider": PROVIDER, "version": self.version, "model": self.cfg.model, "effort": self.cfg.effort,
                "strong_model": self.cfg.strong_model, "strong_effort": self.cfg.strong_effort,
                "max_turns_per_wake": self.max_turns, "max_budget_usd": self.cfg.max_budget_usd,
                "argv": self.argv(self.cfg.model, self.cfg.effort),
                "consult_argv": self.argv(self.cfg.strong_model, self.cfg.strong_effort, consult=True)}


def _flatten(content) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "\n".join(b.get("text", "") if isinstance(b, dict) else str(b) for b in content)
    return json.dumps(content, default=str)

