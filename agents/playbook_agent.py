#!/usr/bin/env python3
"""Murmur's playbook engine as a running agent: the deterministic AI (no model, no account).

    .venv/bin/python agents/playbook_agent.py --url http://127.0.0.1:8765/mcp --agent agent-playbook \\
        --config playbook.json --situation-log logs/playbook.jsonl --minutes 30

It reads the picture through the bridge's MCP tools as one agent every tick (2 s, or sooner when
the change feed shows one of its cards decided, a link lost, a new track), runs its plays,
previews and proposes what the arbiter lets through, withdraws its cards whose premise is gone,
and writes what it did to the situation log (the watch officer's `{t, kind, text}` lines, source
"playbook"): each play's status when it changes, each proposal, withdrawal and decision. Its
ledger (every card and how it fared, the cooldowns) is JSON beside the log, so a restart picks up
where it left off: at every start the ledger is checked against the bridge (a card kept only if
explain_intent shows it is still ours, memory per vehicle only for the same vehicle, memory per
track dropped across a bridge restart), and the ledger as it was is kept beside it when anything
is dropped (playbook-ledger.<run>.json). The config (agents/playbook/defaults.toml under a
situation's [playbook] table) is hashed, and the hash goes to the bridge as the MCP client's
version at connect: the audit log's `agent.action session.connected` names it.

Agents propose; they never approve (no approve tool exists). Nothing here calls a model.
"""

from __future__ import annotations

import argparse
import asyncio
import os
import signal
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from playbook import ENGINE_VERSION  # noqa: E402
from playbook.config import ConfigError, load_file, make  # noqa: E402
from playbook.engine import Engine, waking  # noqa: E402
from playbook.ledger import Ledger  # noqa: E402
from watch.situation import SituationLog  # noqa: E402

POLL_S = 0.5  # between ticks, the change feed is read this often for a waking change


class Agent:
    def __init__(self, args, cfg, log: SituationLog, ledger_path: Path | None):
        self.args = args
        self.cfg = cfg
        self.log = log
        self.ledger_path = ledger_path
        led = Ledger.load(ledger_path) if ledger_path and ledger_path.exists() else Ledger()
        self.engine = Engine(cfg, led, log=log.write, ledger_path=ledger_path)
        self.stop = asyncio.Event()
        self.reason: str | None = None
        self.ticks = 0
        self.errors = 0

    def signal(self, name: str) -> None:
        if not self.stop.is_set():
            self.reason = f"signal {name}"
            self.stop.set()

    async def sleep(self, s: float) -> bool:
        """Sleep, or return True at once on a stop."""
        try:
            await asyncio.wait_for(self.stop.wait(), max(0.0, s))
            return True
        except asyncio.TimeoutError:
            return False

    def session(self):
        from policy_agent import AgentSession

        key = self.args.key or os.environ.get("MURMUR_AGENT_KEY") or None
        return AgentSession(self.args.url, self.args.agent, key, client_version=self.cfg.client_version)

    async def run(self, until: float) -> str:
        delay = 2.0
        while not self.stop.is_set() and time.time() < until:
            try:
                async with self.session() as s:
                    info = await self.engine.start(s)
                    self.log.write("bridge.connected", bridge_run=info["run"], cursor=info["cursor"], reset=info["reset"],
                                   client_version=self.cfg.client_version, tools=list(s.tools),
                                   text=f"connected as {self.args.agent} ({self.cfg.client_version})")
                    delay = 2.0
                    out = await self.loop(s, until)
                    if out:
                        return out
            except Exception as e:  # noqa: BLE001 (the bridge may be starting or restarting)
                self.log.write("bridge.error", error=f"{type(e).__name__}: {e}", retry_in_s=delay,
                               text=f"the bridge did not answer ({type(e).__name__}); again in {delay:.0f} s")
                if await self.sleep(delay):
                    break
                delay = min(delay * 2, 30.0)
        return self.reason or "until"

    async def loop(self, s, until: float) -> str | None:
        misses = 0
        while not self.stop.is_set():
            if time.time() >= until:
                return "until"
            started = time.time()
            try:
                rep = await self.engine.tick(s)
                self.ticks += 1
                misses = 0
                if rep.compute_ms > 50.0:
                    self.log.write("tick.slow", compute_ms=rep.compute_ms, text=f"a tick took {rep.compute_ms:.0f} ms of compute")
                if self.args.once:
                    return "once"
            except Exception as e:  # noqa: BLE001 (three misses in a row: a new session)
                misses += 1
                self.errors += 1
                self.log.write("error", error=f"{type(e).__name__}: {e}", misses=misses,
                               text=f"a tick failed ({type(e).__name__}: {str(e)[:160]})")
                if misses >= 3:
                    return None
            # the next tick at tick_s, or sooner on a waking change
            next_at = started + self.cfg["tick_s"]
            while time.time() < next_at and not self.stop.is_set():
                if await self.sleep(min(POLL_S, next_at - time.time())):
                    return self.reason or "signal"
                try:
                    new = await self.engine.poll_changes(s)
                except Exception:  # noqa: BLE001 (the tick reads again)
                    break
                if waking(new):
                    break
        return self.reason or "signal"


async def main_async(args) -> int:
    run_id = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + f"-{os.getpid()}"
    log = SituationLog(args.situation_log, run_id, echo=not args.quiet, source="playbook")
    try:
        cfg = load_file(args.config, args.config_name) if args.config else make(None, args.config_name or "defaults")
    except (ConfigError, OSError) as e:
        log.write("error", error=f"config: {e}", text=f"the playbook's config was refused: {e}")
        print(f"playbook: {e}", file=sys.stderr)
        return 2
    if args.print_config:
        from playbook.config import canonical

        print(canonical(cfg.data))
        print(cfg.label)
        return 0
    ledger_path = args.ledger or (args.situation_log.parent / "playbook-ledger.json" if args.situation_log else None)
    until = args.until if args.until else time.time() + args.minutes * 60
    agent = Agent(args, cfg, log, ledger_path)
    log.write("run.start", pid=os.getpid(), engine=f"playbook-{ENGINE_VERSION}", agent=args.agent, url=args.url,
              config=cfg.data, config_hash=cfg.hash, config_label=cfg.label, plays=cfg["plays"], until=until,
              ledger=str(ledger_path) if ledger_path else None,
              text=f"playbook started (no model): {len(cfg['plays'])} plays on, config {cfg.label}")
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(sig, agent.signal, sig.name)
    try:
        reason = await agent.run(until)
    except Exception as e:  # noqa: BLE001
        log.write("error", error=f"{type(e).__name__}: {e}", text=f"the playbook stopped on an error: {e}")
        reason = "error"
    led = agent.engine.led
    comp = agent.engine.compute
    log.write("run.stop", reason=reason, ticks=agent.ticks, errors=agent.errors,
              proposed=sorted(int(k) for k in led.entries),
              compute_ms={"mean": round(sum(comp) / len(comp), 3) if comp else None, "max": round(max(comp), 3) if comp else None},
              text=f"playbook stopped ({reason}): {agent.ticks} ticks, proposed {len(led.entries)}")
    log.close()
    return 1 if reason == "error" else 0


def parse_args(argv: list[str] | None = None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--url", default="http://127.0.0.1:8765/mcp", help="the bridge's MCP endpoint")
    ap.add_argument("--agent", default="agent-playbook", help="the agent id (a dev bridge: listed in MURMUR_AGENTS)")
    ap.add_argument("--key", help="the agent's key in a credentials run (default MURMUR_AGENT_KEY); never logged")
    ap.add_argument("--config", type=Path, help="the playbook config: a situation's [playbook] table as JSON or TOML")
    ap.add_argument("--config-name", help="the config's name in its label (default: the file's stem)")
    ap.add_argument("--situation-log", type=Path, help="JSONL log of statuses, proposals, withdrawals, decisions (appended)")
    ap.add_argument("--ledger", type=Path, help="the ledger's JSON (default: playbook-ledger.json beside the log)")
    ap.add_argument("--until", type=float, help="stop at this Unix time")
    ap.add_argument("--minutes", type=float, default=60.0, help="without --until: stop after this long")
    ap.add_argument("--once", action="store_true", help="one tick, then exit")
    ap.add_argument("--print-config", action="store_true", help="print the merged config and its label, then exit")
    ap.add_argument("--quiet", action="store_true", help="no lines on stderr")
    return ap.parse_args(argv)


def _quiet_session_close() -> None:
    """The MCP client warns "Session termination failed: 202" when the bridge accepts a
    session's close with 202: not a failure."""
    import logging

    class Accepted(logging.Filter):
        def filter(self, record: logging.LogRecord) -> bool:
            return record.getMessage() != "Session termination failed: 202"

    logging.getLogger("mcp.client.streamable_http").addFilter(Accepted())


def main() -> None:
    _quiet_session_close()
    sys.exit(asyncio.run(main_async(parse_args())))


if __name__ == "__main__":
    main()
