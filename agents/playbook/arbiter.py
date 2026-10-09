"""The arbiter: one order over every play's candidates, and what holds each one back.

The total order is (class, minus score, play, target, uid): P0 safety first, then the
engagement chain, the picture, links and navigation, hygiene; within a class the higher score;
ties broken by name, target and uid, so the same candidates always come out the same way.
Walking that order, a candidate is held back (with the reason, for the play's status) when:

- an engagement is in play on a vehicle it needs (a nomination pending, approved, under way or
  in its assessment): the playbook gives such a platform no order of any kind, a hold or an rtl
  included (a dispatched one would supersede the engagement, rules 7); the engine says in the
  panel what it would have done (`withheld`), and the bridge's own alerts reach the signers;
- the inbox holds `inbox_busy` (6) or more pending cards and it is not P0 or P1;
- a cooldown is in force on its key and its play finds no material change since;
- its key, or its vehicle, is backing off after a failing preview, a rule-book veto, a refusal
  at submission or an order of ours that failed;
- it would send a vehicle back to the task `safety.stuck` stopped it on (until the alert has
  cleared and `stuck_retry_s` more);
- a vehicle it needs is claimed: by a higher candidate this tick (one claim per vehicle), by a
  card or order of ours it does not supersede, or by another proposer's card waiting in the
  inbox (a P0 command passes those: a waiting card is not a decision);
- a higher candidate of the same play already took its target (one card per play and target).

A selected candidate whose key or vehicle saw an order of ours fail in the last 10 minutes
carries that failure (`after_failure`), and its rationale names it. The inbox budget and the cap
of three previews a tick are the engine's, applied in this order as it previews (a preview says
the tier, and only a human-tier card spends the budget).
"""

from __future__ import annotations

import datetime as dt
from dataclasses import dataclass
from typing import Any

from .beliefs import Beliefs
from .ledger import Ledger, vehicle_key
from .plays import P1, PLAYS, Candidate

ENGAGED = "engagement in play"  # the start of every hold-back an engagement causes (engine: `withheld`)


@dataclass
class Held:
    candidate: Candidate
    reason: str


def hms(t: float) -> str:
    return dt.datetime.fromtimestamp(t, dt.timezone.utc).strftime("%H:%M:%SZ")


def order(cands: list[Candidate]) -> list[Candidate]:
    return sorted(cands, key=lambda c: c.order())


def arbitrate(cands: list[Candidate], b: Beliefs, led: Ledger) -> tuple[list[Candidate], list[Held]]:
    selected: list[Candidate] = []
    held: list[Held] = []
    taken: dict[int, str] = {}
    pairs: set[tuple[str, str]] = set()
    # a look's better choices of vehicle that another candidate took this tick (said in its rationale)
    passed: dict[tuple[str, str], list[str]] = {}
    busy = len(b.pending) >= int(b.cfg.budget["inbox_busy"])
    for c in order(cands):
        engaged = [u for u in c.claims if u in b.engaging]
        if engaged:
            u = engaged[0]
            held.append(Held(c, f"{ENGAGED} on uid {u} ({b.engaging[u]}): the playbook gives it no order"))
            continue
        if busy and c.klass > P1:
            held.append(Held(c, f"the inbox holds {len(b.pending)} pending cards: only P0 and P1 propose"))
            continue
        cd = led.cooldown_for(c.play, c.target, c.uid, b.t)
        if cd is not None:
            change = PLAYS[c.play].material_change(cd.get("basis") or {}, c.basis, b)
            if not change:
                who = f" by {cd['by']}" if cd.get("by") else ""
                note = f" ('{cd['note'][:60]}')" if cd.get("note") else ""
                held.append(Held(c, f"cooling down until {hms(cd['until_t'])}: i{cd['intent']} {cd['why']}{who}{note}"))
                continue
            c.material_change = {"cooldown": f"i{cd['intent']} {cd['why']}{' by ' + cd['by'] if cd.get('by') else ''} at "
                                              f"{hms(cd['set_t'])}", "change": change}
        bo = led.backoff_for(c.key, b.t) or next(
            (led.backoff_for(vehicle_key(u), b.t) for u in c.claims if led.backoff_for(vehicle_key(u), b.t)), None)
        if bo is not None:
            held.append(Held(c, f"backing off until {hms(bo['until_t'])}: {bo['why']}"))
            continue
        sb = led.stuck_block(c.play, c.target, c.uid, b.t)
        if sb is not None:
            when = "the alert clears" if sb.get("until_t") is None else hms(sb["until_t"])
            held.append(Held(c, f"uid {c.uid} was stuck on this task (alert {sb['alert'].split('|')[0]}): not before {when}"))
            continue
        clash = None
        for u in c.claims:
            if u in taken:
                clash = f"uid {u} is taken by {taken[u]} this tick"
                v = b.vehicles.get(u)
                passed.setdefault((c.play, c.target), []).append(f"{v.callsign if v else u} (taken for {taken[u].split(' on ')[-1]})")
                break
            if u in b.claims and not supersedes(c, b.claims[u], led):
                clash = f"uid {u} is {b.claims[u]}"
                break
        if clash:
            held.append(Held(c, clash))
            continue
        if (c.play, c.target) in pairs:
            held.append(Held(c, f"{c.target} already has a {c.play} candidate this tick"))
            continue
        if (c.play, c.target) in passed:
            c.derived = {**c.derived, "passed_over": list(passed[(c.play, c.target)])}
        c.after_failure = led.last_failure(c.play, c.target, c.uid, b.t)
        selected.append(c)
        pairs.add((c.play, c.target))
        for u in c.claims:
            taken[u] = f"{c.play} on {c.target}"
    # a target that got a vehicle is not held back: its other choices are not news
    held = [h for h in held if (h.candidate.play, h.candidate.target) not in pairs]
    return selected, held


def supersedes(c: Candidate, claim: str, led: Ledger) -> bool:
    """Whether `c` may take a vehicle from the claim on it: its own predecessor (a recentre or
    a release of our running order), or, for a P0 command, a card or order of ours that is not
    itself a P0 command (a hold or an rtl reduces risk whatever else of ours waits on the
    vehicle; it never replaces another safety command, its own included)."""
    if c.supersedes is not None and claim.startswith(f"our i{c.supersedes} "):
        return True
    if c.klass == 0 and claim.startswith("pending i"):
        return True  # another proposer's card waiting: not a decision yet
    if c.klass != 0 or not claim.startswith("our i"):
        return False
    try:
        e = led.entry(int(claim.split()[1][1:]))
    except (IndexError, ValueError):
        return False
    return e is None or PLAYS.get(e["play"]) is None or PLAYS[e["play"]].klass != 0


def cooling(led: Ledger, t: float) -> dict[str, list[str]]:
    """Per play, the operator's answers it is cooling down after (a rejection, an expiry), for
    its status: stated while they last, whether or not the trigger holds at the moment, so the
    status does not flip with a trigger that comes and goes."""
    out: dict[str, list[str]] = {}
    for c in sorted(led.cooldowns, key=lambda c: (c["scope"], c["until_t"])):
        if c["until_t"] <= t or c["why"] not in ("rejected", "expired"):
            continue
        play, target = c["scope"].split("|")[:2]
        who = f" by {c['by']}" if c.get("by") else ""
        out.setdefault(play, []).append(f"{target} until {hms(c['until_t'])} (i{c['intent']} {c['why']}{who})")
    return out


def statuses(results: dict[str, Any], held: list[Held], outcomes: dict[str, str], enabled: list[str],
             cools: dict[str, list[str]] | None = None) -> dict[str, str]:
    """One status line per play: active (something of its own is live), ready (on, nothing to
    do), not ready (the blocking constraint), or off; and the cooldowns after an operator's
    answer, while they last."""
    out: dict[str, str] = {}
    held_by: dict[str, list[Held]] = {}
    cools = cools or {}
    for h in held:
        if h.reason.startswith("cooling down") and h.candidate.play in cools:
            continue  # said once, steadily, below
        held_by.setdefault(h.candidate.play, []).append(h)
    for name in sorted(PLAYS):
        if name not in enabled:
            out[name] = "off"
            continue
        r = results.get(name)
        parts = []
        if name in outcomes:
            parts.append(outcomes[name])
        elif r is not None and r.active:
            parts.append("active: " + "; ".join(r.active[:3]))
        elif r is not None and r.note:
            parts.append(f"active: {r.note}")
        if name in held_by:
            h = held_by[name][0]
            parts.append(f"not ready: {h.candidate.target}: {h.reason}")
        elif r is not None and r.blocked:
            t, why = r.blocked[0]
            parts.append(f"not ready: {t}: {why}" if t != "-" else f"not ready: {why}")
        if name in cools:
            parts.append("cooling down: " + "; ".join(cools[name][:3]))
        if not parts or (len(parts) == 1 and name in cools):
            parts.insert(0, "ready")
        out[name] = " | ".join(parts)
    return out
