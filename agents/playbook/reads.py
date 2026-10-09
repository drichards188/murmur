"""One tick's reads, as the bridge's MCP tools answered them, and the tick's time.

Every tick reads `get_fleet` (compact), `get_tracks`, `get_inbox`, `get_intents` (mine) and
`get_changes` since the last cursor; the platform catalog (`get_platforms`) and the policy
(`get_policy`) are read once a session. Only when a vehicle's link is lost does it also read
that vehicle's full facts (`get_fleet` with `uids`: its link's `lost_after_s`), and, for an
order of someone else's that a gap filler would take over, the order's parameters
(`explain_intent`, kept in the ledger: an intent's parameters never change). The bridge logs
none of these reads.

The tick's time is the oldest `t` among the tick's reads, read once and passed in: every age
and every cooldown in the tick is measured from it.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class Reads:
    fleet: dict[str, Any]
    tracks: dict[str, Any]
    inbox: dict[str, Any]
    intents: dict[str, Any] | None
    changes: list[dict[str, Any]]
    platforms: dict[str, Any]
    policy: dict[str, Any]
    # uid -> the full get_fleet entry of a vehicle whose link is lost (its link's lost_after_s)
    lost_full: dict[int, dict[str, Any]] = field(default_factory=dict)
    changes_t: float | None = None

    @property
    def tick_t(self) -> float:
        ts = [r.get("t") for r in (self.fleet, self.tracks, self.inbox, self.intents) if isinstance(r, dict)]
        if self.changes_t is not None:
            ts.append(self.changes_t)
        ts = [float(t) for t in ts if isinstance(t, (int, float))]
        if not ts:
            raise ValueError("no read carries its time (t)")
        return min(ts)

    def as_dict(self) -> dict[str, Any]:
        """The reads as a fixture records them (agents/tests/fixtures/playbook/)."""
        return {
            "get_fleet": self.fleet, "get_tracks": self.tracks, "get_inbox": self.inbox,
            "get_intents": self.intents, "changes": self.changes, "changes_t": self.changes_t,
            "get_platforms": self.platforms, "get_policy": self.policy,
            "lost_full": {str(k): v for k, v in sorted(self.lost_full.items())},
        }

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "Reads":
        return cls(
            fleet=d["get_fleet"], tracks=d["get_tracks"], inbox=d["get_inbox"], intents=d.get("get_intents"),
            changes=list(d.get("changes") or []), platforms=d.get("get_platforms") or {"platforms": []},
            policy=d.get("get_policy") or {}, lost_full={int(k): v for k, v in (d.get("lost_full") or {}).items()},
            changes_t=d.get("changes_t"),
        )
