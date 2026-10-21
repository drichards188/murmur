"""Scoring v2's latency-charged clock (eval/SCORING-V2.md, section 4.2; docs/HEADLESS.md 14.1.1):
the constants that say how much simulated time an agent's thinking takes.

Under `Options.clock = "charged"` a model watch officer's wake is a resumable turn (officer.py):
each model call's answer is applied only after the simulator has advanced by that call's charge

    a_s + b_in_s * input_tokens + b_out_s * output_tokens + c_s * tool_calls

(`input_tokens`: every input token the call billed, fresh, cache write and cache read, the
request's size; `output_tokens`: thinking included; `tool_calls`: the tool calls in its answer,
which run at that time). A call the API failed is charged `a_s` (it took time and answered with an
error); a call never sent (the cost guard, the turn's last model call used) is charged nothing.
The playbook is charged `playbook_tick_s` per tick: it reads the picture when the tick starts and
its previews, proposals and withdrawals reach the bridge that long after.

The values in latency.toml are **placeholders**: they are fitted on dev-build episodes to measured
API wall times (SCORING-V2.md section 11, stage 1 item a) and then frozen; a change is a dated
section of the spec. Every charged episode records the constants it ran with (its provenance).
"""

from __future__ import annotations

import tomllib
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

from . import HERE

#: the episode's clock conditions (SCORING-V2.md section 4): the world waits for the agent, or
#: the agent's thinking costs simulated time
CLOCKS = ("paused", "charged")
LATENCY_FILE = HERE / "latency.toml"


@dataclass(frozen=True)
class Latency:
    a_s: float = 2.0
    b_in_s: float = 2e-5
    b_out_s: float = 0.02
    c_s: float = 0.5
    playbook_tick_s: float = 0.2
    source: str = "placeholders (not fitted)"

    KEYS = ("a_s", "b_in_s", "b_out_s", "c_s", "playbook_tick_s", "source")

    @classmethod
    def load(cls, path: Path | None = None) -> "Latency":
        path = path or LATENCY_FILE
        if not path.exists():
            return cls()
        raw = tomllib.loads(path.read_text())
        out = cls(**{k: raw[k] for k in cls.KEYS if k in raw})
        for k in cls.KEYS[:-1]:
            v = getattr(out, k)
            if not isinstance(v, (int, float)) or isinstance(v, bool) or v < 0:
                raise ValueError(f"{path}: {k} must be a number of seconds, 0 or more (got {v!r})")
        return out

    def call_s(self, tokens: dict[str, Any], tool_calls: int) -> float:
        """One model call's charge, from the tokens it billed (officer.py's per-call `tokens`:
        input, cache_write, cache_read, output) and the tool calls in its answer."""
        n_in = sum(int(tokens.get(k) or 0) for k in ("input", "cache_write", "cache_read"))
        n_out = int(tokens.get("output") or 0)
        return self.a_s + self.b_in_s * n_in + self.b_out_s * n_out + self.c_s * int(tool_calls)

    def as_dict(self) -> dict[str, Any]:
        return asdict(self)
