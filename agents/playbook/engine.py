"""The engine: one tick of read, believe, play, arbitrate, preview, veto, propose, follow up.

    1. read: get_fleet (compact), get_tracks, get_inbox, get_intents (mine), get_changes since
       the cursor (and, while a link is lost, that vehicle's full facts and its order's
       parameters); the catalog and the policy once a session
    2. beliefs (beliefs.py)
    3. plays: each returns candidates, what blocks it, what of its own is live
    4. arbiter: the total order, cooldowns and material change, back-offs, claims
    5. preview_intent (at most 3 a tick; the inbox budget gates human-tier cards): a failing one
       backs off
    6. the rule book's veto (eval/evidence.py v2)
    7. propose_intent with the rationale and its evidence record
    8. follow up: withdraw own cards whose withdraw condition holds (link lost, navigation
       suspect on an armed card, the play's own condition), with the reason

Steps 2 to 7 are a pure function of the reads, the preview answers, the config and the ledger
(`Engine.plan` and `Engine.decide`): every collection is sorted, floats are rounded at fixed
precision, and the tick's time is the oldest `t` among its reads. In **shadow** mode (beside
the model watch officer) the engine reads only: no preview, no proposal, no withdrawal; what it
would propose, and what it would withdraw, go to the situation log with their evidence records,
and its would-be cards are held in the ledger as if pending (claiming their vehicles, counted
by the budget as human-tier) until their withdraw condition holds or the policy's approval wait
passes (then as expired unread).
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable

from . import ENGINE_VERSION
from . import rationale as R
from . import rulebook
from .arbiter import Held, arbitrate, hms, statuses
from .beliefs import Beliefs, build
from .config import Config
from .ledger import WAITING, Decision, Ledger, key_of
from .plays import PLAYS, SAFETY_TASKS, Candidate, Result, Withdrawal
from .reads import Reads

WITHDRAW_PLAYS = ("link.withdraw_pending", "nav.suspect_exclude", "hygiene.withdraw_stale")
TIER_MEMORY_S = 300.0
# changes that wake a tick before its time (agents/watch/wake.py's table, cut to what the plays read)
WAKING = ("intent.new", "intent.state", "track.new", "track.affiliation", "track.declared", "track.declaration_revoked",
          "track.identity_conflict", "track.dropped", "vehicle.link_lost", "vehicle.link_restored", "vehicle.nav",
          "vehicle.gps_glitch", "alert.raised", "alert.changed", "feed.reset")


def waking(changes: list[dict[str, Any]]) -> bool:
    for c in changes:
        if not isinstance(c, dict):
            continue
        if c.get("kind") in WAKING and (not str(c.get("kind")).startswith("intent.") or c.get("mine")):
            return True
    return False


@dataclass
class Plan:
    """What the pure part of a tick decided before any preview."""

    t: float
    beliefs: Beliefs
    results: dict[str, Result]
    selected: list[Candidate]
    held: list[Held]
    withdrawals: list[Withdrawal]
    decisions: list[Decision]


@dataclass
class TickReport:
    t: float
    proposed: list[dict[str, Any]] = field(default_factory=list)
    withdrawn: list[dict[str, Any]] = field(default_factory=list)
    would: list[dict[str, Any]] = field(default_factory=list)  # shadow
    statuses: dict[str, str] = field(default_factory=dict)
    compute_ms: float = 0.0
    previews: int = 0


class Engine:
    def __init__(self, cfg: Config, ledger: Ledger | None = None, *, shadow: bool = False,
                 log: Callable[..., Any] | None = None, ledger_path: Path | None = None):
        self.cfg = cfg
        self.led = ledger or Ledger()
        self.shadow = shadow
        self.log = log or (lambda kind, **f: None)
        self.ledger_path = ledger_path
        self.platforms: dict[str, Any] | None = None
        self.policy: dict[str, Any] | None = None
        self.run: str | None = None
        self.pending_changes: list[dict[str, Any]] = []
        self.changes_t: float | None = None
        self.compute: list[float] = []

    # ---- reading ----------------------------------------------------------------------------------

    async def start(self, session: Any) -> dict[str, Any]:
        """A new session: the catalog and the policy, and the change feed's cursor (kept when the
        same bridge answers again after a dropped session)."""
        self.platforms = await session.call("get_platforms")
        self.policy = await session.call("get_policy")
        first = await session.call("get_changes", **({"since": self.led.cursor} if self.led.cursor else {}))
        reset = bool(first.get("reset")) or self.led.cursor is None or (self.run is not None and first.get("run") != self.run)
        if reset:
            self.led.cursor = first.get("cursor")
        else:
            self.pending_changes += list(first.get("changes") or [])
            self.led.cursor = first.get("cursor", self.led.cursor)
        self.run = first.get("run")
        return {"run": self.run, "cursor": self.led.cursor, "reset": reset}

    async def poll_changes(self, session: Any) -> list[dict[str, Any]]:
        """Changes since the cursor, kept for the next tick; returns the new ones."""
        new: list[dict[str, Any]] = []
        while True:
            r = await session.call("get_changes", since=self.led.cursor, limit=500)
            if r.get("reset"):
                new.append({"kind": "feed.reset", "run": r.get("run")})
                self.run = r.get("run")
                self.platforms = None  # a new bridge: the catalog and the policy again
            new += [c for c in r.get("changes") or [] if isinstance(c, dict)]
            self.led.cursor = r.get("cursor", self.led.cursor)
            self.changes_t = r.get("t", self.changes_t)
            if not r.get("more"):
                break
        self.pending_changes += new
        return new

    async def read(self, session: Any) -> Reads:
        if self.platforms is None or self.policy is None:
            self.platforms = await session.call("get_platforms")
            self.policy = await session.call("get_policy")
        fleet = await session.call("get_fleet", compact=True)
        tracks = await session.call("get_tracks")
        inbox = await session.call("get_inbox")
        intents = None if self.shadow else await session.call("get_intents", mine=True, limit=200)
        await self.poll_changes(session)
        changes, self.pending_changes = self.pending_changes, []
        lost = sorted(int(v["uid"]) for v in fleet.get("vehicles") or [] if v.get("link_lost"))
        lost_full: dict[int, dict[str, Any]] = {}
        if lost:
            full = await session.call("get_fleet", uids=lost)
            lost_full = {int(v["uid"]): v for v in full.get("vehicles") or []}
            for v in sorted(fleet.get("vehicles") or [], key=lambda v: int(v["uid"])):
                task = v.get("task") or {}
                iid = task.get("intent_id")
                if not v.get("link_lost") or task.get("task") not in ("orbit", "patrol") or iid is None:
                    continue
                if self.led.entry(int(iid)) is None and str(iid) not in self.led.params:
                    try:
                        ex = await session.call("explain_intent", id=int(iid))
                    except Exception:  # noqa: BLE001 (tried again next tick)
                        continue
                    i = ex.get("intent") or {}
                    src = i.get("source") or {}
                    self.led.params[str(iid)] = {"task": i.get("kind"), "params": dict(i.get("params") or {}),
                                                 "source": f"{src.get('kind')} {src.get('id')}", "play": None}
        return Reads(fleet=fleet, tracks=tracks, inbox=inbox, intents=intents, changes=changes,
                     platforms=self.platforms or {}, policy=self.policy or {}, lost_full=lost_full,
                     changes_t=self.changes_t)

    # ---- the pure part ----------------------------------------------------------------------------

    def plan(self, reads: Reads) -> Plan:
        led, cfg = self.led, self.cfg
        t = reads.tick_t
        led.note_changes(reads.changes)
        decisions = [] if self.shadow else led.sync(
            {int(i["id"]): i for i in (reads.intents or {}).get("intents") or []}, t, cfg)
        b = build(reads, cfg, led)
        for uid, h in sorted(b.health.items()):
            if h.nav_bad:
                led.nav_bad_t[str(uid)] = round(t, 3)
        if self.shadow:
            b.own_pending = sum(1 for s in led.shadow.values() if s.get("task") not in SAFETY_TASKS)
        enabled = cfg["plays"]
        results: dict[str, Result] = {}
        cands: list[Candidate] = []
        for name in sorted(enabled):
            r = PLAYS[name].evaluate(b, led)
            results[name] = r
            if PLAYS[name].proposes:
                cands += r.candidates
        selected, held = arbitrate(cands, b, led)
        waiting = self.waiting(b)
        withdrawals: list[Withdrawal] = []
        seen: set[str] = set()
        for name in WITHDRAW_PLAYS:
            if name not in enabled:
                continue
            for w in PLAYS[name].withdrawals(b, led, waiting):
                ident = w.key or str(w.intent)
                if ident not in seen:
                    seen.add(ident)
                    withdrawals.append(w)
        return Plan(t=t, beliefs=b, results=results, selected=selected, held=held, withdrawals=withdrawals,
                    decisions=decisions)

    def waiting(self, b: Beliefs) -> list[dict[str, Any]]:
        if self.shadow:
            return [{**s, "intent": -1, "key": k} for k, s in sorted(self.led.shadow.items())]
        return [e for e in self.led.open_entries() if e.get("state") in WAITING]

    def budget_block(self, c: Candidate, b: Beliefs, extra_pending: int) -> str | None:
        """Why the inbox budget cannot take a human-tier card for `c` now, or None."""
        if c.task in SAFETY_TASKS:
            return None
        bud = self.cfg.budget
        if b.own_pending + extra_pending >= int(bud["own_pending_max"]):
            return f"inbox budget: {int(bud['own_pending_max'])} cards of ours pending (at most {int(bud['own_pending_max'])})"
        if self.led.tokens(b.t, self.cfg) < 1.0:
            return f"inbox budget: one human-tier card per {bud['human_every_s']:.0f} s after a burst of {int(bud['human_burst'])}"
        return None

    def finish(self, c: Candidate, b: Beliefs, preview: dict[str, Any] | None, veto: list[str]) -> tuple[str, dict[str, Any]]:
        """The rationale and its evidence record (pure)."""
        record = c.record(self.cfg.label, b.t)
        record["engine"] = f"playbook-{ENGINE_VERSION}"
        record["rulebook_v2"] = list(veto)
        if preview is None:
            record["preview"] = {"shadow": True}
        else:
            checks = preview.get("checks") or []
            outcomes = [str(x.get("outcome")) for x in checks]
            summary = ", ".join(f"{outcomes.count(o)} {o}" for o in ("pass", "fail", "unverified", "not_evaluated") if outcomes.count(o))
            pol = preview.get("policy") or {}
            record["preview"] = {"would_reach": preview.get("would_reach"), "tier": pol.get("tier"), "rule": pol.get("rule"),
                                 "checks": f"{len(checks)} checks: {summary or 'none'}"}
        text = PLAYS[c.play].render(record)
        return R.compose(text, record), record

    # ---- one tick ------------------------------------------------------------------------------------

    async def tick(self, session: Any) -> TickReport:
        reads = await self.read(session)
        t0 = time.perf_counter()
        plan = self.plan(reads)
        compute = (time.perf_counter() - t0) * 1000.0
        b, led, cfg = plan.beliefs, self.led, self.cfg
        rep = TickReport(t=plan.t)
        for d in plan.decisions:
            self.log_decision(d)
        held = list(plan.held)
        outcomes: dict[str, str] = {}
        extra_pending = 0
        if self.shadow:
            self.shadow_follow_up(plan)
        cap = int(cfg.budget["previews_per_tick"])
        for c in plan.selected:
            if rep.previews >= cap:
                held.append(Held(c, f"{cap} previews a tick"))
                continue
            block = self.budget_block(c, b, extra_pending)
            if self.shadow:
                if block:
                    held.append(Held(c, block))
                    continue
                t1 = time.perf_counter()
                veto = rulebook.veto(self.armed(c, b), None, c.request, reads.tracks.get("tracks") or [])
                if veto:
                    led.set_backoff(c.key, b.t, "rule book: " + "; ".join(veto), cfg)
                    held.append(Held(c, "rule book: " + "; ".join(veto)))
                    continue
                rationale, record = self.finish(c, b, None, veto)
                compute += (time.perf_counter() - t1) * 1000.0
                rep.previews += 1
                led.shadow[c.key] = {"play": c.play, "target": c.target, "uid": c.uid, "task": c.task, "request": c.request,
                                     "basis": c.basis, "withdraw_if": c.withdraw_if, "t": round(b.t, 3), "armed": self.armed(c, b)}
                if c.task not in SAFETY_TASKS:
                    led.take_token(b.t, cfg)
                    extra_pending += 1
                if c.play == "isr.recentre" and c.uid is not None:
                    led.recentred[str(c.uid)] = round(b.t, 3)
                self.log("shadow", play=c.play, target=c.target, track=c.target, uid=c.uid, request=c.request,
                         rationale=rationale, record=record, playbook="playbook",
                         text=f"the playbook would propose {c.task} on {c.request.get('uids')} for {c.target} ({c.play}; not proposed)")
                rep.would.append({"play": c.play, "target": c.target, "uid": c.uid, "request": c.request, "rationale": rationale})
                outcomes[c.play] = f"active: would propose {c.task} uid {c.uid} on {c.target}"
                continue
            if block:
                seen = led.tier_seen.get(c.key)
                if not (seen and seen.get("would_reach") == "auto_approved" and b.t - seen["t"] < TIER_MEMORY_S):
                    held.append(Held(c, block))
                    continue
            try:
                pv = await session.call("preview_intent", **c.request)
            except Exception as e:  # noqa: BLE001 (a refusal or a lost session: back off)
                led.set_backoff(c.key, b.t, f"preview refused: {str(e)[:120]}", cfg)
                held.append(Held(c, f"preview refused: {str(e)[:80]}"))
                continue
            rep.previews += 1
            t1 = time.perf_counter()
            reach = pv.get("would_reach")
            led.tier_seen[c.key] = {"t": round(b.t, 3), "would_reach": reach}
            if not pv.get("passed"):
                bad = [f"{x.get('rule')} {x.get('outcome')}: {str(x.get('detail'))[:80]}" for x in pv.get("checks") or []
                       if x.get("outcome") != "pass"]
                why = "preview failed: " + ("; ".join(bad[:2]) or str(reach))
                until = led.set_backoff(c.key, b.t, why, cfg)
                held.append(Held(c, f"{why} (again after {hms(until)})"))
                continue
            if reach == "pending_approval":
                block = self.budget_block(c, b, extra_pending)
                if block:
                    held.append(Held(c, block))
                    continue
            veto = rulebook.veto(self.armed(c, b), pv, c.request, reads.tracks.get("tracks") or [])
            if veto:
                until = led.set_backoff(c.key, b.t, "rule book: " + "; ".join(veto), cfg)
                held.append(Held(c, "rule book v2: " + "; ".join(veto)))
                continue
            rationale, record = self.finish(c, b, pv, veto)
            compute += (time.perf_counter() - t1) * 1000.0
            try:
                out = await session.propose(rationale, **c.request)
            except Exception as e:  # noqa: BLE001
                led.set_backoff(c.key, b.t, f"propose refused: {str(e)[:120]}", cfg)
                held.append(Held(c, f"propose refused: {str(e)[:80]}"))
                self.log("error", error=f"propose_intent: {type(e).__name__}: {e}", play=c.play, target=c.target)
                continue
            iid = out.get("id")
            state = str(out.get("state"))
            if iid is not None:
                v = b.vehicles.get(c.uid) if c.uid is not None else None
                led.record(int(iid), play=c.play, version=c.version, target=c.target, uid=c.uid, task=c.task,
                           request=c.request, t=b.t, state=state, basis=c.basis, withdraw_if=c.withdraw_if,
                           prior_task=(v.task if v else None), tier=(pv.get("policy") or {}).get("tier"),
                           armed=self.armed(c, b))
            led.clear_backoff(c.key)
            if reach == "pending_approval":
                led.take_token(b.t, cfg)
                extra_pending += 1
            if c.play == "isr.recentre" and c.uid is not None:
                led.recentred[str(c.uid)] = round(b.t, 3)
            refused = out.get("refused")
            who = (c.vehicle or {}).get("callsign") or c.uid
            self.log("proposal", intent=iid, play=c.play, version=c.version, target=c.target, uid=c.uid, state=state,
                     request=c.request, refused=refused, rationale=rationale,
                     text=f"proposed i{iid} {c.task} {who} on {c.target} ({c.play} v{c.version}): {state}"
                          + (f", refused: {str(refused)[:120]}" if refused else ""))
            rep.proposed.append({"intent": iid, "play": c.play, "target": c.target, "uid": c.uid, "state": state,
                                 "request": c.request, "rationale": rationale})
            outcomes[c.play] = f"active: proposed i{iid} {c.task} {who} on {c.target} ({state})"
        # 8. follow up: withdraw what no longer holds
        if not self.shadow:
            for w in plan.withdrawals:
                try:
                    out = await session.call("withdraw_intent", id=w.intent, reason=f"{w.play}: {w.reason}")
                    state = out.get("state")
                except Exception as e:  # noqa: BLE001 (decided meanwhile, or gone out: the next sync says)
                    self.log("withdraw.refused", intent=w.intent, play=w.play, reason=w.reason, error=str(e)[:200],
                             text=f"could not withdraw i{w.intent}: {str(e)[:120]}")
                    continue
                e = led.entry(w.intent)
                if e is not None:
                    e["withdrawn_by"], e["withdraw_reason"] = w.play, w.reason
                self.log("withdrawal", intent=w.intent, play=w.play, reason=w.reason, state=state,
                         text=f"withdrew i{w.intent} ({w.play}): {w.reason}")
                rep.withdrawn.append({"intent": w.intent, "play": w.play, "reason": w.reason, "state": state})
        rep.statuses = {k: v for k, v in statuses(plan.results, held, outcomes, cfg["plays"]).items() if k in cfg["plays"]}
        self.log_statuses(rep.statuses)
        led.prune(b.t)
        rep.compute_ms = round(compute, 3)
        self.compute.append(compute)
        if self.ledger_path is not None:
            led.save(self.ledger_path)
        return rep

    def armed(self, c: Candidate, b: Beliefs) -> bool:
        if c.armed:
            return True
        return any(bool(b.vehicles[u].armed) for u in c.request.get("uids") or [] if u in b.vehicles)

    def shadow_follow_up(self, plan: Plan) -> None:
        """Shadow mode: would-be cards whose withdraw condition holds are dropped (logged as
        would-withdraw), and those past the policy's approval wait lapse as expired unread."""
        led, b = self.led, plan.beliefs
        for w in plan.withdrawals:
            s = led.shadow.pop(w.key, None)
            if s is None:
                continue
            led._cool({**s, "intent": -1}, "withdrawn", b.t, self.cfg, None, None)
            self.log("shadow.withdraw", play=s["play"], target=s["target"], uid=s.get("uid"), reason=w.reason,
                     text=f"the playbook would withdraw its {s['play']} on {s['target']}: {w.reason}")
        for k, s in sorted(list(led.shadow.items())):
            if b.t - s["t"] >= b.expire_s:
                led.shadow.pop(k)
                led._cool({**s, "intent": -1}, "expired", b.t, self.cfg, None, None)
                self.log("shadow.lapsed", play=s["play"], target=s["target"], uid=s.get("uid"),
                         text=f"the playbook's would-be {s['play']} on {s['target']} lapsed after {b.expire_s:.0f} s (counted as expired)")

    # ---- the situation log ----------------------------------------------------------------------------

    def log_decision(self, d: Decision) -> None:
        bits = [f"i{d.intent} {d.state}"]
        if d.by:
            bits.append(f"by {d.by}")
        if d.note:
            bits.append(f"(note {R.quote(d.note)})")
        cool = f"; {d.play} on {d.target} cools down until {hms(d.cooldown_until)}" if d.cooldown_until else ""
        self.log("decision", intent=d.intent, play=d.play, target=d.target, uid=d.uid, state=d.state, by=d.by, note=d.note,
                 cooldown_until=d.cooldown_until, text=" ".join(bits) + cool)

    def log_statuses(self, sts: dict[str, str]) -> None:
        kind = "shadow.status" if self.shadow else "play.status"
        # a play starts ready (run.start lists them): only a change from that is news
        for name in sorted(sts):
            if self.led.status.get(name, "ready") != sts[name]:
                self.led.status[name] = sts[name]
                self.log(kind, play=name, status=sts[name], text=f"{name}: {sts[name]}")


def key(play: str, target: str, uid: int | None) -> str:
    return key_of(play, target, uid)
