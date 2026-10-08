"""The backend interface: one model CLI as the watch officer's engine, and what it reports.

A backend runs the user's own installed CLI (Claude Code or Codex), logged in by the user;
Murmur never stores, reads, proxies or prompts for a model credential. It only removes the
API-key variables that would make the CLI bill an API account instead of the user's plan.

    start()            the session's process (or nothing until the first turn) and its info
    turn(text, ...)    send one update and read events until the turn ends: a Turn
    escalate(text,...) one consult on the stronger model, in a separate short process
    stop()             end the process, whatever it is doing

A turn that hits a usage limit raises LimitHit (`limits.py`); a process that died raises
BackendCrashed (the driver restarts it, resuming the session). Provider specifics live in
`claude.py` and `codex.py`.
"""

from __future__ import annotations

import asyncio
import os
import re
import signal
import time
from dataclasses import dataclass, field
from typing import Any

from .limits import LimitHit


class PreflightError(RuntimeError):
    """The CLI can't run as the watch officer; `fix` says what the user can do."""

    def __init__(self, problem: str, fix: str):
        super().__init__(problem)
        self.problem = problem
        self.fix = fix


class BackendCrashed(RuntimeError):
    def __init__(self, message: str, code: int | None = None, stderr: str = ""):
        super().__init__(message)
        self.code = code
        self.stderr = stderr


class LimitReached(RuntimeError):
    """A turn ended on a usage limit."""

    def __init__(self, hit: LimitHit, turn: "Turn | None" = None):
        super().__init__(hit.text)
        self.hit = hit
        self.turn = turn


@dataclass
class ToolCall:
    name: str
    input: Any = None
    id: str | None = None
    result: str | None = None  # an excerpt
    is_error: bool | None = None

    def as_dict(self) -> dict:
        return {"name": self.name, "input": self.input, "id": self.id,
                "result": self.result, "is_error": self.is_error}


@dataclass
class Turn:
    """One wake's turn, as the backend saw it."""

    model: str | None = None
    text: str = ""  # the model's final message
    tool_calls: list[ToolCall] = field(default_factory=list)
    usage: dict = field(default_factory=dict)
    cost_usd: float | None = None  # the CLI's own estimate (Claude), else None
    duration_s: float = 0.0
    num_turns: int | None = None
    stop: str = "success"  # success | max_turns | interrupted | timeout | error | budget
    error: str | None = None
    session: str | None = None
    events: int = 0
    rate_limit: dict | None = None

    def proposals(self) -> list[int]:
        """Intent ids its propose_intent calls created (read from the tool results)."""
        out = []
        for c in self.tool_calls:
            if c.name.endswith("propose_intent") and c.result:
                m = re.search(r'"id"\s*:\s*(\d+)', c.result)
                if m:
                    out.append(int(m.group(1)))
        return out

    def as_dict(self) -> dict:
        return {
            "model": self.model, "text": self.text, "tool_calls": [c.as_dict() for c in self.tool_calls],
            "usage": self.usage, "cost_usd": self.cost_usd, "duration_s": round(self.duration_s, 2),
            "num_turns": self.num_turns, "stop": self.stop, "error": self.error, "session": self.session,
            "events": self.events, "rate_limit": self.rate_limit, "proposed": self.proposals(),
        }


# Variables that make a CLI bill an API account instead of the user's own login. Removed
# from the child's environment, never read or stored.
SCRUB = {
    "claude": ("ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"),
    "codex": ("CODEX_API_KEY", "OPENAI_API_KEY"),
}


def child_env(provider: str, extra: dict[str, str] | None = None) -> tuple[dict[str, str], list[str]]:
    """The environment for the CLI: ours without the API-key variables (their names are
    returned, never their values), plus `extra`."""
    env = dict(os.environ)
    removed = [k for k in SCRUB[provider] if env.pop(k, None) is not None]
    env.update(extra or {})
    return env, removed


def version_tuple(text: str) -> tuple[int, ...] | None:
    m = re.search(r"(\d+)\.(\d+)\.(\d+)", text or "")
    return tuple(int(x) for x in m.groups()) if m else None


def excerpt(value: Any, n: int = 4000) -> str:
    s = value if isinstance(value, str) else repr(value)
    return s if len(s) <= n else s[:n] + f"... [{len(s) - n} more]"


# Long lines: a tool result can be a whole fleet's JSON.
STREAM_LIMIT = 64 << 20


async def run_quick(argv: list[str], env: dict[str, str], cwd: str | None = None,
                    timeout: float = 30.0, stdin: bytes | None = None) -> tuple[int, str, str]:
    """A short CLI call (version, login status): exit code, stdout, stderr."""
    try:
        p = await asyncio.create_subprocess_exec(
            *argv, stdin=asyncio.subprocess.PIPE if stdin is not None else asyncio.subprocess.DEVNULL,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE, env=env, cwd=cwd)
    except FileNotFoundError:
        return 127, "", f"{argv[0]}: not found"
    try:
        out, err = await asyncio.wait_for(p.communicate(stdin), timeout)
    except asyncio.TimeoutError:
        p.kill()
        await p.wait()
        return 124, "", f"{' '.join(argv[:3])}: no answer in {timeout:.0f} s"
    return p.returncode or 0, out.decode(errors="replace"), err.decode(errors="replace")


async def end_process(p: asyncio.subprocess.Process, grace: float = 5.0) -> int | None:
    """Stop a child: SIGINT (ends a turn), then SIGTERM, then SIGKILL."""
    if p.returncode is not None:
        return p.returncode
    for sig, wait in ((signal.SIGINT, grace), (signal.SIGTERM, grace), (signal.SIGKILL, grace)):
        try:
            p.send_signal(sig)
        except ProcessLookupError:
            break
        try:
            return await asyncio.wait_for(p.wait(), wait)
        except asyncio.TimeoutError:
            continue
    return p.returncode


class StderrTail:
    """Drains a child's stderr, keeping the last lines (a stuck pipe would block it)."""

    def __init__(self, stream: asyncio.StreamReader | None, keep: int = 60):
        self.lines: list[str] = []
        self.keep = keep
        self.task = asyncio.ensure_future(self._drain(stream)) if stream else None

    async def _drain(self, stream: asyncio.StreamReader) -> None:
        while True:
            line = await stream.readline()
            if not line:
                return
            self.lines.append(line.decode(errors="replace").rstrip())
            del self.lines[:-self.keep]

    def text(self) -> str:
        return "\n".join(self.lines)


def now() -> float:
    return time.time()
