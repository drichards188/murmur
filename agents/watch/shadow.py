"""The shadow: the playbook engine run beside the model watch officer, reading only, its would-be
proposals logged for comparison and never proposed.

    class Shadow(Protocol):
        name: str
        tick_s: float
        async def tick(self, session) -> list[dict]

`tick` gets the driver's MCP session (the model's agent id) and reads the picture through it
(`get_fleet` compact, `get_tracks`, `get_inbox`, `get_changes` on a cursor of its own; reads
are not logged by the bridge). It never previews (a preview is logged under the model's agent
id), never proposes and never withdraws. What it would propose, with its rationale and evidence
record, goes to the watch officer's situation log as a `shadow` line; what it would withdraw as
`shadow.withdraw`; a would-be card past the policy's approval wait as `shadow.lapsed`; each
play's status, when it changes, as `shadow.status`. The driver ticks it on its own clock (the
engine's 2 s), not at the model's wakes, so the two can be compared on timeliness as well as on
what they propose (agents/playbook/engine.py, "shadow").
"""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Any, Callable, Protocol

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from playbook.config import Config  # noqa: E402
from playbook.engine import Engine  # noqa: E402
from playbook.ledger import Ledger  # noqa: E402


class Shadow(Protocol):
    name: str
    tick_s: float

    async def tick(self, session: Any) -> list[dict]: ...


class PlaybookShadow:
    name = "playbook"

    def __init__(self, cfg: Config, log: Callable[..., Any], ledger_path: Path | None = None):
        self.cfg = cfg
        self.tick_s = float(cfg["tick_s"])
        self.engine = Engine(cfg, Ledger(), shadow=True, log=log, ledger_path=ledger_path)
        self.session: Any = None

    async def tick(self, session: Any) -> list[dict]:
        if session is not self.session:
            # a new session (the driver reconnected): the catalog, the policy and a cursor again
            await self.engine.start(session)
            self.session = session
        rep = await self.engine.tick(session)
        return rep.would
