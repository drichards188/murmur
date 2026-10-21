"""The `murmur-headless` process (crates/murmur-headless) and the session adapter an agent gets.

One process per episode: the spec on stdin's first line, then one request per line on stdin
and one answer per line on a pipe of its own (`--reply-fd`), so the bridge's own `println!`
lines go to a log and never into the channel. `Sim` is the driver's handle (it steps the
clock, reads truth, injects faults); `HeadlessSession` is all an agent ever gets: the MCP
tools, answered by the bridge's own `mcp::tools::call`, with `AgentSession`'s surface and
error semantics (agents/policy_agent.py, agents/scripted_agent.py).
"""

from __future__ import annotations

import json
import os
import subprocess
import time
from pathlib import Path
from typing import Any

from . import REPO

BINARY = REPO / "target" / "release" / "murmur-headless"


class SimError(RuntimeError):
    pass


class Sim:
    """A running episode. Requests answer at the simulated instant the clock stands at;
    `step(t)` moves it."""

    def __init__(self, spec: dict[str, Any], env: dict[str, str], binary: Path | None = None,
                 log: Path | None = None):
        binary = Path(binary or os.environ.get("MURMUR_HEADLESS_BIN") or BINARY)
        if not binary.exists():
            raise SimError(f"no {binary}: build it with `cargo build --release -p murmur-headless`")
        r, w = os.pipe()
        out = open(log, "wb") if log else subprocess.DEVNULL
        try:
            self.proc = subprocess.Popen([str(binary), "--reply-fd", str(w)], stdin=subprocess.PIPE,
                                         stdout=out, stderr=subprocess.STDOUT, pass_fds=(w,), env=env,
                                         cwd=str(REPO))
        finally:
            os.close(w)
            if log:
                out.close()  # type: ignore[union-attr]
        self.reply = os.fdopen(r, "rb")
        self.calls = 0
        self.wait_s = 0.0
        self.t = 0.0
        self.pending: float | None = None
        self._send(spec)
        first = self._read()
        if not first.get("ok"):
            raise SimError(f"the episode did not start: {first.get('error')}")
        self.tools = list(first.get("tools") or [])
        self.ended = False

    def _send(self, obj: dict[str, Any]) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.write(json.dumps(obj, separators=(",", ":")).encode() + b"\n")
        self.proc.stdin.flush()

    def _read(self) -> dict[str, Any]:
        line = self.reply.readline()
        if not line:
            code = self.proc.poll()
            raise SimError(f"murmur-headless closed its channel (exit {code})")
        return json.loads(line)

    def rpc(self, req: dict[str, Any]) -> dict[str, Any]:
        if self.pending is not None:
            # the step the driver asked for, carried by the request that follows it
            req = {**req, "at": self.pending}
            self.pending = None
        t0 = time.perf_counter()
        self._send(req)
        out = self._read()
        self.wait_s += time.perf_counter() - t0
        self.calls += 1
        if not out.get("ok"):
            raise SimError(f"{req.get('op')}: {out.get('error')}")
        return out

    def step(self, t: float) -> float:
        """Move the clock to `t`. Lazily: the next request carries it (`at`), so a step that no
        request follows costs nothing, and one that one does costs no round trip of its own;
        the simulation is the same (nothing happens at an instant no request asks about that
        would not happen on the way to the next one)."""
        if t > self.t:
            self.t = float(t)
            self.pending = self.t
        return self.t

    def mcp(self, agent: str, client: tuple[str, str], tool: str, args: dict[str, Any]) -> dict[str, Any]:
        return self.rpc({"op": "mcp", "agent": agent, "client": list(client), "tool": tool, "args": args})

    def api(self, method: str, path: str, body: Any = None, operator: str | None = None) -> tuple[int, Any]:
        out = self.rpc({"op": "api", "method": method, "path": path, "body": body, "operator": operator})
        return int(out["status"]), out["body"]

    def tool_defs(self) -> list[dict[str, Any]]:
        """The MCP tools' definitions as the bridge's `tools/list` gives them (name, description,
        `inputSchema`): what an MCP client hands its model (eval/headless/officer.py)."""
        defs = self.rpc({"op": "tools", "defs": True}).get("defs")
        if not isinstance(defs, list):
            raise SimError("this murmur-headless lists no tool definitions: rebuild it from this tree "
                           "(cargo build --release -p murmur-headless)")
        return defs

    def now(self) -> float:
        """The simulator's own clock, situation seconds (a pending step runs first)."""
        return float(self.rpc({"op": "now"})["t"])

    def truth(self, association: bool = True) -> dict[str, Any]:
        return self.rpc({"op": "truth", "association": association})

    def picture(self, homes: bool = False) -> dict[str, Any]:
        """Scoring v2's picture sample at this instant (evaluation only, never given to an
        agent): every track as the bridge serves it, the tracker's association, the inbox's
        alerts and our vehicles' truth; with `homes`, each vehicle's platform and sensors and
        the sensing model's seed and misclassification rate (eval/headless/v2/record.py)."""
        out = self.rpc({"op": "picture", "homes": bool(homes)})
        if "tracks" not in out:
            raise SimError("this murmur-headless has no picture request: rebuild it from this tree "
                           "(cargo build --release -p murmur-headless)")
        return out

    def misclassify(self, seed: int, rate: float, contacts: list[str]) -> list[dict[str, Any]]:
        """The sensing model's misclassification draws (crates/murmur-bridge tracks/sensing.rs
        `misclassify_draw`) for these contacts: per (contact, sensor type), the uniform the
        draw compares with `rate` and the draw (evaluation only)."""
        return list(self.rpc({"op": "misclassify", "seed": int(seed), "rate": float(rate),
                              "contacts": list(contacts)})["draws"])

    def fault(self, kind: str, **fields: Any) -> dict[str, Any]:
        return self.rpc({"op": "fault", "kind": kind, **fields})

    def end(self, timeout: float = 30.0) -> dict[str, Any]:
        """Close the episode: the bridge anchors its audit log and the process exits."""
        out = self.rpc({"op": "end"})
        self.ended = True
        try:
            self.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
        self.reply.close()
        return out

    def kill(self) -> None:
        if self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait()
        try:
            self.reply.close()
        except OSError:
            pass


class HeadlessSession:
    """What an agent gets: `AgentSession`'s surface over the simulator. `call` answers as
    `agents/scripted_agent.call` does (a tool error raises `RuntimeError("<tool>: <error>")`),
    `propose` as `AgentSession.propose` (a refusal that created an intent comes back with
    `refused`). Only the MCP server's tools exist; nothing else of the simulator is reachable
    from here."""

    def __init__(self, sim: Sim, agent: str, client_version: str = "0.1"):
        self._sim = sim
        self.agent = agent
        self.client_version = client_version
        self.tools = list(sim.tools)

    async def __aenter__(self) -> "HeadlessSession":
        return self

    async def __aexit__(self, *exc: Any) -> None:
        return None

    def _call(self, name: str, args: dict[str, Any]) -> tuple[bool, Any]:
        out = self._sim.mcp(self.agent, (self.agent, self.client_version), name, args)
        if "protocol_error" in out:
            raise RuntimeError(str(out["protocol_error"]))
        return bool(out.get("is_error")), out.get("structured")

    async def call(self, name: str, **args: Any) -> dict[str, Any]:
        is_error, body = self._call(name, args)
        if is_error:
            raise RuntimeError(f"{name}: {body.get('error', body) if isinstance(body, dict) else body}")
        return body

    async def propose(self, rationale: str, **req: Any) -> dict[str, Any]:
        is_error, body = self._call("propose_intent", {"rationale": rationale, **req})
        if is_error:
            intent = body.get("intent") if isinstance(body, dict) else None
            if isinstance(intent, dict) and intent.get("id") is not None:
                return {"id": intent["id"], "state": intent.get("state"), "dispatched": False,
                        "intent": intent, "refused": body.get("error")}
            raise RuntimeError(f"propose_intent: {body.get('error', body) if isinstance(body, dict) else body}")
        return body

    async def withdraw(self, id: int, reason: str) -> dict[str, Any]:
        return await self.call("withdraw_intent", id=id, reason=reason)

    async def explain(self, id: int) -> dict[str, Any]:
        return await self.call("explain_intent", id=id)
