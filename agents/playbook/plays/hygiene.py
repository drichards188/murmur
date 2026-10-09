"""Hygiene (P4).

- `hygiene.withdraw_stale`: every tick, each card of ours still waiting for the operator
  (pending approval) whose play's withdraw condition holds is withdrawn, with the reason the
  play gives, and only if it is still pending at that moment (`if_state`). Its cooldown follows
  the cause: the pair when the premise about the target went, that vehicle for that target when
  the reason is the sensor's (battery, navigation, recovering). A card carried over a bridge
  restart whose target is a track id from before it is withdrawn, "track ids restarted with the
  bridge" (that id may name another contact now), with no cooldown. A card a person
  approved (approved, queued) is never withdrawn by the playbook: the engine says in the panel
  that its premise went and leaves it to the operator and to the bridge's own re-check at
  dispatch. A card proposed by a play that is not on in this config is judged by its play all
  the same: what it asked for is still the play's.
"""

from __future__ import annotations

from typing import Any

from ..beliefs import Beliefs
from ..ledger import Ledger
from . import P4, PLAYS, Play, Withdrawal, register
from .isr import sensor_unfit

RESTARTED = "track ids restarted with the bridge"


class WithdrawStale(Play):
    name, klass, proposes = "hygiene.withdraw_stale", P4, False

    def withdrawals(self, b: Beliefs, led: Ledger, waiting: list[dict[str, Any]]) -> list[Withdrawal]:
        out = []
        for e in waiting:
            if e.get("state") not in ("pending_approval", None):  # None: a would-be card (shadow)
                continue  # a person approved it: the playbook never takes that back
            if e.get("stale_target"):
                # carried over a bridge restart: its track id may name another contact now
                out.append(Withdrawal(intent=int(e["intent"]), key=str(e.get("key") or ""), play=self.name,
                                      reason=f"{e['play']}: {RESTARTED}", cause="restart"))
                continue
            play = PLAYS.get(e["play"])
            if play is None:
                continue
            why = play.withdraw_reason(e, b)
            if why:
                # the sensor's sake (battery, navigation, recovering) or the target's (the premise)
                cause = "vehicle" if e.get("uid") is not None and why == sensor_unfit(e.get("uid"), b) else "premise"
                out.append(Withdrawal(intent=int(e["intent"]), key=str(e.get("key") or ""), play=self.name,
                                      reason=f"{e['play']}: {why}", cause=cause))
        return out


register(WithdrawStale())
