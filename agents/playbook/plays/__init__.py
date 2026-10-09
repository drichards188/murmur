"""The plays: each a named, versioned template with a trigger read from facts, a few checks, one
proposal, its withdraw condition, its material change, and its rationale template.

Priority classes (research report C.2.3): **P0** safety, **P1** engagement chain, **P2** picture
(ISR, identification support), **P3** links and navigation, **P4** hygiene. Within a class a
stated score orders candidates (each play's docstring states it).

Step 1's plays are in `isr.py`, `links.py`, `safety.py` and `hygiene.py`. The engagement plays
(`engage.nominate`, `engage.wait_keepout`, `engage.renominate`, `engage.assess_support`),
`ident.review_support` and `isr.reacquire_lost` are step 3: a play is a subclass of `Play`
registered in `PLAYS`, so each lands as a module of its own with its fixtures, and the arbiter
already orders P1 before P2 and lets only P0 and P1 through a busy inbox.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from ..beliefs import Beliefs
    from ..ledger import Ledger

P0, P1, P2, P3, P4 = 0, 1, 2, 3, 4
SAFETY_TASKS = ("hold", "rtl")


@dataclass
class Candidate:
    """What a play would propose now, with everything its evidence record needs."""

    play: str
    version: str
    klass: int
    score: float
    target: str
    uid: int | None
    task: str
    request: dict[str, Any]
    facts: list[dict[str, Any]]
    derived: dict[str, Any]
    thresholds: dict[str, Any]
    withdraw_if: list[str]
    basis: dict[str, Any]
    trigger: list[str] = field(default_factory=list)
    alternatives: list[dict[str, Any]] = field(default_factory=list)
    claims: tuple[int, ...] = ()
    armed: bool = False  # a consequential (armed) task: excluded for a suspect navigation
    # the own card or order this candidate may take a claimed vehicle from (a recentre, a release)
    supersedes: int | None = None
    prior_task: dict[str, Any] | None = None
    material_change: dict[str, Any] | None = None
    vehicle: dict[str, Any] | None = None  # the tasked vehicle as the record names it
    after_failure: dict[str, Any] | None = None  # an order of ours that failed on this key or vehicle

    @property
    def key(self) -> str:
        from ..ledger import key_of

        return key_of(self.play, self.target, self.uid)

    def order(self) -> tuple:
        """The arbiter's total order: class, minus score, play, target, uid."""
        return (self.klass, -round(self.score, 6), self.play, self.target, -1 if self.uid is None else self.uid)

    def record(self, config_label: str, t: float) -> dict[str, Any]:
        """The evidence record before the preview and the rule book (the engine adds those)."""
        return {
            "play": f"{self.play}@{self.version}",
            "config": config_label,
            "tick_t": round(t, 3),
            "target": self.target,
            "uid": self.uid,
            "vehicle": self.vehicle,
            "task": self.task,
            "request": self.request,
            "facts": self.facts,
            "derived": self.derived,
            "trigger": self.trigger,
            "thresholds": self.thresholds,
            "alternatives": self.alternatives,
            "withdraw_if": self.withdraw_if,
            "basis": self.basis,
            "score": round(self.score, 4),
            "material_change": self.material_change,
            "after_failure": self.after_failure,
            "armed": self.armed,
        }


@dataclass
class Withdrawal:
    intent: int
    play: str  # the play that withdraws it (link.withdraw_pending, nav.suspect_exclude, hygiene.withdraw_stale)
    reason: str
    key: str = ""


@dataclass
class Result:
    """A play's look at one tick: its candidates, what blocks it (target, constraint), and what
    of its own is live (a card waiting or an order running)."""

    candidates: list[Candidate] = field(default_factory=list)
    blocked: list[tuple[str, str]] = field(default_factory=list)
    active: list[str] = field(default_factory=list)
    note: str | None = None  # a status for a play that proposes nothing (nav.suspect_exclude)


class Play:
    name = ""
    version = "1"
    klass = P2
    proposes = True  # False: a play that only withdraws or excludes

    def evaluate(self, b: "Beliefs", led: "Ledger") -> Result:
        return Result()

    def withdraw_reason(self, entry: dict[str, Any], b: "Beliefs") -> str | None:
        """For one of this play's own waiting cards: why its premise no longer holds, or None."""
        return None

    def withdrawals(self, b: "Beliefs", led: "Ledger", waiting: list[dict[str, Any]]) -> list[Withdrawal]:
        """Withdraw plays only: the own waiting cards (entries) to withdraw, with the reason."""
        return []

    def material_change(self, then: dict[str, Any], now: dict[str, Any], b: "Beliefs") -> str | None:
        return None

    def render(self, record: dict[str, Any]) -> str:
        raise NotImplementedError


PLAYS: dict[str, Play] = {}


def register(p: Play) -> Play:
    PLAYS[p.name] = p
    return p


from . import hygiene, isr, links, safety  # noqa: E402,F401  (each registers its plays)
