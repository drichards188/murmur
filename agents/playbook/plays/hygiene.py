"""Hygiene (P4).

- `hygiene.withdraw_stale`: every tick, each card of ours still waiting (pending, approved but
  not gone out, queued) whose play's withdraw condition holds is withdrawn, with the reason the
  play gives. A card proposed by a play that is not on in this config is judged by its play
  all the same: what it asked for is still the play's.
"""

from __future__ import annotations

from typing import Any

from ..beliefs import Beliefs
from ..ledger import Ledger
from . import P4, PLAYS, Play, Withdrawal, register


class WithdrawStale(Play):
    name, klass, proposes = "hygiene.withdraw_stale", P4, False

    def withdrawals(self, b: Beliefs, led: Ledger, waiting: list[dict[str, Any]]) -> list[Withdrawal]:
        out = []
        for e in waiting:
            play = PLAYS.get(e["play"])
            if play is None:
                continue
            why = play.withdraw_reason(e, b)
            if why:
                out.append(Withdrawal(intent=int(e["intent"]), key=str(e.get("key") or ""), play=self.name, reason=f"{e['play']}: {why}"))
        return out


register(WithdrawStale())
