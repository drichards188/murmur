"""OpenAI Codex CLI (`codex exec`) as the watch officer's backend.

One process per wake: the first runs `codex exec`, the later ones `codex exec resume
<thread_id>`, each reading its update on stdin and printing JSONL events (`thread.started`,
`item.completed` with an `agent_message` or an `mcp_tool_call`, `turn.completed` with usage,
`turn.failed` with the error; `error` events may be retried inside the CLI, only
`turn.failed` is final). The stronger model is a fresh `codex exec` thread per escalation
(see `escalate`).

The user's own ChatGPT login is what runs it: CODEX_API_KEY and OPENAI_API_KEY are removed
from the child's environment (an API key there bills the API instead), and the preflight
wants `codex login status` to say "Logged in using ChatGPT". MURMUR_CODEX_HOME, when set,
becomes the child's CODEX_HOME: a home of its own keeps the user's global AGENTS.md out
(log in there once: `CODEX_HOME=<dir> codex login`); without it the user's normal home is
used and its global AGENTS.md is read.

Before any turn the backend checks `codex features list`, disables active features
matching RISKY_FEATURES, and requires shell_tool to be present and off. Removed flags are
historical entries. unified_exec ignores user opt-outs in 0.164; shell_tool gates command
access. code_mode_host must remain available for MCP calls (even with code_mode off).
The CLI may expose its JavaScript tool orchestrator; disabling its host breaks MCP calls.
Shell, file-viewing, web, apps, plugins and sub-agent capabilities remain disabled, with a
read-only sandbox and the bridge as the only configured MCP server. The JSONL event check
stops on foreign tool calls or unknown item types; it is not a pre-turn tool inventory.

Live verified with Codex 0.164.0 and a ChatGPT login on 2026-10-27: fleet reads, routine
thread resume, a fresh stronger-model consult, two driver wakes and SIGTERM shutdown.
See agents/README.md for scope and remaining unverified behavior. --strict-config rejects
unknown configuration keys rather than silently ignoring them.
"""

from __future__ import annotations

import asyncio
import json
import os
import re
import signal
import time
from dataclasses import dataclass
from pathlib import Path

from .backend import (FALLBACK_HINT, STREAM_LIMIT, BackendCrashed, LimitReached, LockdownBreach, PreflightError, StderrTail,
                      ToolCall, Turn, child_env, end_process, excerpt, run_quick, version_tuple)
from .limits import parse_codex

PROVIDER = "codex"
MIN_VERSION = (0, 161, 0)
KEY_ENV = "MURMUR_AGENT_KEY"


@dataclass
class CodexConfig:
    bin: str = "codex"
    model: str = "gpt-6-luna"
    effort: str = "high"
    strong_model: str = "gpt-6.1-sol"
    strong_effort: str = "high"
    home: str | None = None  # MURMUR_CODEX_HOME: the child's CODEX_HOME
    # item types the user allows besides ALLOWED_ITEMS (`--codex-allow-item`): an escape hatch
    # while the vocabulary is unconfirmed; never one that acts (FORBIDDEN_ITEMS, a tool call)
    allow_items: tuple[str, ...] = ()


def toml_str(s: str) -> str:
    """A TOML basic string (JSON's escapes are TOML's; non-ASCII stays as it is)."""
    return json.dumps(s, ensure_ascii=False)


# "brief" marks the original 0.161 documentation research; "inferred" marks its
# inferred keys. The remaining keys were accepted by --strict-config in live 0.164 runs.
# Match capability names conservatively, with explicit infrastructure exceptions below.
# Sandbox features are deliberately not matched: disabling one could weaken isolation.
RISKY_FEATURES = re.compile(r"shell|exec|repl|code_mode|patch|agent|apps|plugin|browser|computer|hook|skill|"
                            r"memor|image|artifact|git|undo|fanout|permission|tool_suggest|search|realtime|voice|"
                            r"goal")
# These must be in the list and off, or the backend does not start (a renamed shell feature
# cannot be confirmed off).
REQUIRED_OFF = ("shell_tool",)


def parse_features(text: str) -> dict[str, bool]:
    """`codex features list`: one feature a line, `<name> <stage...> <true|false>`."""
    out: dict[str, bool] = {}
    for line in text.splitlines():
        parts = line.split()
        # Removed flags are immutable historical entries, not available capabilities.
        if len(parts) >= 3 and parts[1] == "removed":
            continue
        if len(parts) >= 3 and parts[-1] in ("true", "false") and re.fullmatch(r"[a-z0-9_]+", parts[0]):
            out[parts[0]] = parts[-1] == "true"
    return out


# Everything off that the watch officer does not need (`-c key=value`, TOML values).
LOCKDOWN: tuple[tuple[str, str], ...] = (
    ("features.shell_tool", "false"),  # brief
    ("features.unified_exec", "false"),  # brief
    ("web_search", '"disabled"'),  # brief
    ("features.apps", "false"),  # brief
    ("features.plugins", "false"),  # brief
    ("agents.enabled", "false"),  # brief
    ("features.multi_agent", "false"),  # brief
    ("features.browser_use", "false"),  # brief
    ("features.computer_use", "false"),  # brief
    ("features.image_generation", "false"),  # brief
    ("features.goals", "false"),  # brief
    ("features.memories", "false"),  # brief
    ("features.hooks", "false"),  # brief
    ("features.tool_suggest", "false"),  # brief
    ("skills.include_instructions", "false"),  # brief
    ("skills.bundled.enabled", "false"),  # brief
    ("project_doc_max_bytes", "0"),  # brief
    ("include_environment_context", "false"),  # brief
    ("include_permissions_instructions", "false"),  # brief
    ("include_collaboration_mode_instructions", "false"),  # brief
    ("include_apps_instructions", "false"),  # brief
    # nothing may wait for an approval prompt nobody answers; the Murmur tools are approved
    # by their own server setting
    ("approval_policy", '"never"'),  # inferred (the long-standing approval key)
    # Disable image viewing. The read-only sandbox blocks writes; file-change events stop the run.
    ("features.view_image", "false"),  # verified with Codex 0.164
    ("features.apply_patch_freeform", "false"),  # inferred
)
# What a turn's items may be. The event stream carries no list of the tools a session offers,
# so this checks what the model used, after the fact; it cannot see a tool that was offered
# and not used. Fail closed on the items that act or read outside Murmur (the documented
# command, file change and web search items, and a call to another MCP server): the run
# stops. The documented harmless items pass. An item type this module does not know stops the
# run too, with the raw item in the error: future CLI versions may add item types.
# `--codex-allow-item TYPE` lets a reviewed type through (recorded in `unknown_items`,
# logged as lockdown.unknown_item). Unknown types remain refused by default.
ALLOWED_ITEMS = ("agent_message", "reasoning", "todo_list", "error", "plan")
FORBIDDEN_ITEMS = ("command_execution", "file_change", "web_search", "patch_apply", "image_view")
MURMUR_SERVERS = ("murmur", "mcp__murmur")
# Never: `ultra` effort spawns sub-agents.
REFUSED_EFFORTS = ("ultra",)


class CodexBackend:
    provider = PROVIDER

    def __init__(self, cfg: CodexConfig, run_dir: Path, url: str, agent: str, key: str | None,
                 instructions: str, max_turns: int = 12):
        if cfg.effort in REFUSED_EFFORTS or cfg.strong_effort in REFUSED_EFFORTS:
            raise ValueError("codex effort 'ultra' spawns sub-agents: not allowed for the watch officer")
        bad = [t for t in cfg.allow_items if t in FORBIDDEN_ITEMS or t == "mcp_tool_call"]
        if bad:
            raise ValueError(f"--codex-allow-item cannot allow {', '.join(bad)}: those act or call tools, "
                             "and the lockdown stops them")
        self.cfg = cfg
        self.run_dir = run_dir
        self.url = url
        self.agent = agent
        self.key = key
        self.instructions = instructions
        self.max_turns = max_turns
        self.thread_id: str | None = None
        self.thread_path = run_dir / "thread.json"
        self.version: str | None = None
        self.removed_env: list[str] = []
        self.init: dict | None = None
        self.current: asyncio.subprocess.Process | None = None
        # the bridge's tool names, when the driver knows them: a call whose server field is
        # missing passes only when it names one of these
        self.murmur_tools: set[str] | None = None
        # the features disabled on every exec, once confirmed off (None: not yet checked)
        self.disabled: list[str] | None = None

    def judge_item(self, item: dict) -> str:
        """'ok', 'breach', 'unknown' (a type the user let through with --codex-allow-item:
        recorded) or 'unlisted' (a type nobody listed: a breach) for one item of a turn."""
        itype = item.get("type")
        if itype in ALLOWED_ITEMS:
            return "ok"
        if itype == "mcp_tool_call":
            server = item.get("server")
            tool = str(item.get("tool") or "")
            if server in MURMUR_SERVERS:
                return "ok"
            if server in (None, "") and (tool.startswith("mcp__murmur__") or self.murmur_tools is None
                                         or tool in self.murmur_tools):
                return "ok"
            return "breach"  # another server, or a tool that is not Murmur's
        if itype in FORBIDDEN_ITEMS:
            return "breach"
        return "unknown" if itype in self.cfg.allow_items else "unlisted"

    def env(self) -> dict[str, str]:
        extra: dict[str, str] = {}
        if self.cfg.home:
            extra["CODEX_HOME"] = self.cfg.home
        if self.key:
            extra[KEY_ENV] = self.key
        env, self.removed_env = child_env(PROVIDER, extra)
        return env

    def config_args(self, model: str, effort: str) -> list[str]:
        cfg = [
            ("model_reasoning_effort", toml_str(effort)),  # brief
            ("mcp_servers.murmur.url", toml_str(self.url)),  # brief
            ("mcp_servers.murmur.http_headers", "{" + f'"X-Murmur-Agent"={toml_str(self.agent)}' + "}"),  # brief
            ("mcp_servers.murmur.required", "true"),  # brief
            ("mcp_servers.murmur.default_tools_approval_mode", '"approve"'),  # brief
        ]
        if self.key:  # the key stays in the environment; the config names the variable
            cfg.append(("mcp_servers.murmur.bearer_token_env_var", toml_str(KEY_ENV)))  # inferred
        cfg += list(LOCKDOWN)
        cfg.append(("developer_instructions", toml_str(self.instructions)))
        out: list[str] = []
        for k, v in cfg:
            out += ["-c", f"{k}={v}"]
        return out

    def disable_args(self) -> list[str]:
        """`--disable <feature>` for each confirmed off (the spelling `codex exec --help` and
        `codex exec resume --help` give on 0.116)."""
        return [a for name in self.disabled or () for a in ("--disable", name)]

    def argv_new(self, model: str, effort: str) -> list[str]:
        return [self.cfg.bin, "exec", "--json", "--skip-git-repo-check", "--ignore-user-config",
                "--ignore-rules", "--strict-config", "-C", str(self.run_dir), "-s", "read-only",
                "-m", model, *self.disable_args(), *self.config_args(model, effort), "-"]

    def argv_resume(self, thread: str, model: str, effort: str) -> list[str]:
        # resume takes no -C or -s: the sandbox by configuration, the directory as cwd
        return [self.cfg.bin, "exec", "resume", thread, "--json", "--skip-git-repo-check",
                "--ignore-user-config", "--ignore-rules", "--strict-config", "-m", model,
                *self.disable_args(), *self.config_args(model, effort), "-c", 'sandbox_mode="read-only"', "-"]

    async def _features(self, extra: list[str]) -> dict[str, bool]:
        argv = [self.cfg.bin, *extra, *self.config_args(self.cfg.model, self.cfg.effort), "features", "list"]
        code, out, err = await run_quick(argv, self.env(), cwd=str(self.run_dir), timeout=60)
        listed = parse_features(out)
        if code != 0 or not listed:
            last = (err.strip().splitlines() or ["no output"])[-1]
            raise PreflightError(
                f"`codex features list` failed ({code}: {last}), so the shell and exec features cannot be "
                "confirmed off; the watch officer does not run with them possibly on",
                "check that this Codex has the features command (codex features list), " + FALLBACK_HINT)
        return listed

    async def confirm_features(self) -> dict:
        """Disable every risky feature the CLI lists and confirm it off, or refuse to start."""
        listed = await self._features([])
        missing = [n for n in REQUIRED_OFF if n not in listed]
        if missing:
            raise PreflightError(
                f"`codex features list` does not list {', '.join(missing)}, so they cannot be confirmed off "
                "(renamed in this Codex?)",
                "add the new names to REQUIRED_OFF and RISKY_FEATURES in agents/watch/codex.py after checking "
                "them, " + FALLBACK_HINT)
        # Current Codex ignores unified_exec opt-outs: shell_tool gates all command
        # execution; unified_exec only selects the resumable implementation.
        # https://developers.openai.com/codex/config-reference
        # code_mode_host also hosts direct MCP calls; disabling it makes every tool
        # return "tool execution host is unavailable". code_mode itself stays off.
        risky = sorted(n for n in listed if RISKY_FEATURES.search(n)
                       and n not in ("unified_exec", "code_mode_host"))
        after = await self._features([a for n in risky for a in ("--disable", n)])
        still = [n for n in risky if after.get(n, True)]
        if still:
            raise PreflightError(
                f"Codex still shows {', '.join(still)} on after --disable: the watch officer does not run with "
                "a feature that runs commands or reads files on",
                "check `codex --disable <name> features list`, " + FALLBACK_HINT)
        self.disabled = risky
        return {"features_disabled": risky, "features_on": sorted(n for n, on in after.items() if on)}

    async def preflight(self) -> dict:
        env = self.env()
        code, out, err = await run_quick([self.cfg.bin, "--version"], env)
        if code == 127:
            raise PreflightError(f"{self.cfg.bin} is not installed or not on PATH",
                                 "install the Codex CLI (npm install -g @openai/codex), run `codex login` "
                                 "with your ChatGPT account, " + FALLBACK_HINT)
        self.version = (out or err).strip()
        v = version_tuple(self.version)
        if v is None or v < MIN_VERSION:
            raise PreflightError(
                f"codex {self.version or '?'} is older than {'.'.join(map(str, MIN_VERSION))}, the minimum supported version "
                "(older versions lack configuration keys the watch officer's lockdown needs)",
                "update the Codex CLI (npm install -g @openai/codex@latest, or your package manager), "
                + FALLBACK_HINT)
        code, out, err = await run_quick([self.cfg.bin, "login", "status"], env)
        said = (out + err).strip()
        if code != 0 or "Logged in using ChatGPT" not in said:
            home = f"CODEX_HOME={self.cfg.home} " if self.cfg.home else ""
            raise PreflightError(f"Codex is not logged in with ChatGPT ({said or 'no answer'})",
                                 f"run `{home}codex login` with your ChatGPT account, " + FALLBACK_HINT)
        features = await self.confirm_features()
        return {"cli": self.cfg.bin, "version": self.version, "login": said, "codex_home": self.cfg.home,
                "api_key_env_removed": self.removed_env, **features}

    async def start(self, resume: str | None = None) -> dict:
        """Nothing runs between wakes; a later run resumes a recorded thread."""
        if resume:
            self.thread_id = resume
        return {"thread_id": self.thread_id, "argv": self.argv_new(self.cfg.model, self.cfg.effort)}

    async def restart(self) -> dict:
        return await self.start(resume=self.thread_id)

    def _remember_thread(self, tid: str | None) -> None:
        if tid and tid != self.thread_id:
            self.thread_id = tid
            self.thread_path.write_text(json.dumps({"thread_id": tid, "t": time.time()}))

    async def turn(self, text: str, deadline: float) -> Turn:
        if self.disabled is None:
            await self.confirm_features()  # never a turn with the shell possibly on
        if self.thread_id:
            argv = self.argv_resume(self.thread_id, self.cfg.model, self.cfg.effort)
        else:
            argv = self.argv_new(self.cfg.model, self.cfg.effort)
        return await self._run(argv, text, deadline, self.cfg.model, routine=True)

    async def escalate(self, text: str, deadline: float) -> Turn:
        """One consult on the stronger model in a fresh thread: the instructions and this
        update only, not the routine thread. Resuming the routine thread on the stronger
        model would send that whole thread, uncached, at every escalation; a fresh thread
        pays for its own reads, and the routine thread is told what it did."""
        if self.disabled is None:
            await self.confirm_features()
        return await self._run(self.argv_new(self.cfg.strong_model, self.cfg.strong_effort), text, deadline,
                               self.cfg.strong_model, routine=False)

    async def stop(self) -> None:
        if self.current is not None and self.current.returncode is None:
            await end_process(self.current)
        self.current = None

    async def _run(self, argv: list[str], text: str, deadline: float, model: str, routine: bool) -> Turn:
        t0 = time.monotonic()
        turn = Turn(model=model)
        calls: dict[str, ToolCall] = {}
        failed: str | None = None
        stopped: str | None = None
        last_error: str | None = None
        foreign: list[str] = []  # items outside the lockdown (see ALLOWED_ITEMS)
        proc = await asyncio.create_subprocess_exec(
            *argv, stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE, env=self.env(), cwd=str(self.run_dir), limit=STREAM_LIMIT,
            start_new_session=True)
        self.current = proc
        stderr = StderrTail(proc.stderr)
        assert proc.stdin is not None and proc.stdout is not None
        proc.stdin.write(text.encode())
        await proc.stdin.drain()
        proc.stdin.close()
        try:
            while True:
                try:
                    line = await asyncio.wait_for(proc.stdout.readline(), max(0.1, deadline - time.time()))
                except asyncio.TimeoutError:
                    stopped = "timeout"
                    await end_process(proc)  # SIGINT first: the CLI ends its turn
                    break
                if not line:
                    break
                try:
                    ev = json.loads(line)
                except ValueError:
                    continue
                turn.events += 1
                kind = ev.get("type")
                if kind == "thread.started":
                    turn.session = ev.get("thread_id")
                    if routine:
                        self._remember_thread(turn.session)
                        self.init = {"thread_id": turn.session}
                elif kind in ("item.started", "item.completed"):
                    item = ev.get("item") or {}
                    itype = item.get("type")
                    verdict = self.judge_item(item)
                    if verdict in ("breach", "unlisted"):
                        raw = excerpt(json.dumps(item, default=str), 500)
                        if verdict == "unlisted":
                            raw = (f"an item type the backend does not know ({itype}; if it is harmless, "
                                   f"--codex-allow-item {itype}): {raw}")
                        if raw not in foreign:
                            foreign.append(raw)
                        if stopped is None:
                            stopped = "lockdown"
                            proc.send_signal(signal.SIGINT)
                        continue
                    if verdict == "unknown":
                        iid = item.get("id")
                        if iid is None or all(u.get("id") != iid for u in turn.unknown_items):
                            turn.unknown_items.append({"id": iid, "type": itype,
                                                       "item": excerpt(json.dumps(item, default=str), 500)})
                        continue
                    if itype == "mcp_tool_call":
                        iid = item.get("id") or str(len(calls))
                        c = calls.get(iid)
                        if c is None:
                            c = ToolCall(f"mcp__{item.get('server') or 'murmur'}__{item.get('tool', '?')}",
                                         item.get("arguments"), iid)
                            calls[iid] = c
                            turn.tool_calls.append(c)
                            if len(turn.tool_calls) > self.max_turns and stopped is None:
                                stopped = "max_turns"
                                proc.send_signal(signal.SIGINT)
                        if kind == "item.completed":
                            err = item.get("error")
                            c.is_error = bool(err) or item.get("status") == "failed"
                            c.result = excerpt(err if err else item.get("result"))
                    elif itype == "agent_message" and kind == "item.completed":
                        turn.text = item.get("text") or turn.text  # the last one is the answer
                elif kind == "turn.completed":
                    turn.usage = ev.get("usage") or {}
                    # the turn's input tokens (cached included): its calls' sum, an upper bound
                    # on the context one call read (Codex reports no per-call usage)
                    n = turn.usage.get("input_tokens")
                    turn.context_tokens = int(n) if isinstance(n, (int, float)) else None
                elif kind == "turn.failed":
                    failed = ((ev.get("error") or {}).get("message")) or "turn failed"
                elif kind == "error":
                    last_error = ev.get("message")  # may be retried inside the CLI
            code = await asyncio.wait_for(proc.wait(), 15)
        except asyncio.TimeoutError:
            code = await end_process(proc)
        finally:
            self.current = None
        turn.duration_s = time.monotonic() - t0
        if foreign:
            turn.stop = "lockdown"
            turn.error = ("the model used tools outside the Murmur server (the items as Codex sent them): "
                          + " | ".join(foreign))
            raise LockdownBreach(turn.error, turn)
        if failed:
            hit = parse_codex(failed)
            turn.stop, turn.error = ("limit" if hit else "error"), failed
            if hit:
                raise LimitReached(hit, turn)
            return turn
        if stopped:
            turn.stop = stopped
            return turn
        if code not in (0, None):
            text = "\n".join(x for x in (last_error, stderr.text()) if x)
            hit = parse_codex(text)
            if hit:
                turn.stop, turn.error = "limit", hit.text
                raise LimitReached(hit, turn)
            raise BackendCrashed(f"codex exited ({code})", code, text)
        return turn

    def describe(self) -> dict:
        redacted = [a if not a.startswith("developer_instructions=") else "developer_instructions=<prompt>"
                    for a in self.argv_new(self.cfg.model, self.cfg.effort)]
        return {"provider": PROVIDER, "version": self.version, "model": self.cfg.model, "effort": self.cfg.effort,
                "strong_model": self.cfg.strong_model, "strong_effort": self.cfg.strong_effort,
                "codex_home": self.cfg.home or os.environ.get("CODEX_HOME") or "~/.codex (the user's own)",
                "allow_items": list(self.cfg.allow_items),
                # None until `codex features list` confirmed them off (the preflight does)
                "features_disabled": self.disabled,
                "max_turns_per_wake": self.max_turns, "argv": redacted}
