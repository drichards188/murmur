"""The engine: one tick of read, believe, play, arbitrate, preview, veto, propose, follow up.

    1. read: get_fleet (compact), get_tracks, get_inbox, get_intents (mine: the newest 50, and
       any older card of ours still open, by explain_intent), get_changes since the cursor (and,
       while a link is lost, that vehicle's full facts and its order's parameters; after a
       truncated change feed, get_nominations for the engagements it would have reported); the
       catalog and the policy once a session (and the areas, in shadow mode)
    2. beliefs (beliefs.py)
    3. plays: each returns candidates, what blocks it, what of its own is live
    4. arbiter: engagements in play, the total order, cooldowns and material change, back-offs,
       stuck blocks, claims
    5. preview_intent, at most 3 a tick: a failing one backs off; the preview says the tier, and
       only a human-tier result spends the inbox budget (P0 commands are exempt by class)
    6. the rule book's veto (eval/evidence.py v2)
    7. propose_intent with the rationale and its evidence record (a refusal at submission backs
       off and is logged with its reason)
    8. follow up: withdraw own cards still waiting for the operator whose withdraw condition
       holds (link lost, navigation suspect on an armed card, the play's own condition), with
       the reason and `if_state: pending_approval`, so an approval that came meanwhile is never
       undone; a card a person approved whose premise is gone is left, and said so in the panel

Steps 2 to 7 are a pure function of the reads, the preview answers, the config and the ledger:
every collection is sorted, floats are rounded at fixed precision, and the tick's time is the
oldest `t` among its reads. A ledger of another bridge run (or of no known run) is set aside,
renamed beside the new one, and a fresh one started.

In **shadow** mode (beside the model watch officer) the engine reads only: no preview, no
proposal, no withdrawal. What it would propose, and what it would withdraw or release, go to
the situation log with their evidence records. Each would-be card's tier is estimated from the
policy (`tier.py`): a human-tier one is held as pending (claiming its vehicle, spending the
budget) until its withdraw condition holds or the policy's approval wait passes (then as expired
unread: a model of an operator who never answers); an auto-tier one is held as running until its
premise is gone (then a would-be release).
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable

from . import ENGINE_VERSION
from . import rationale as R
from . import rulebook, tier
from .arbiter import ENGAGED, Held, arbitrate, cooling, hms, statuses
from .beliefs import NOT_RUNNING, RECOVERING_MODES, Beliefs, build, catalog
from .config import Config
from .ledger import APPROVED, OPEN, WAITING, Decision, Ledger, key_of
from .plays import P0, PLAYS, Candidate, Result, Withdrawal
from .reads import Reads

WITHDRAW_PLAYS = ("link.withdraw_pending", "nav.suspect_exclude", "hygiene.withdraw_stale")
INTENTS_PAGE = 50
# changes that wake a tick before its time (agents/watch/wake.py's table, cut to what the plays read)
WAKING = ("intent.new", "intent.state", "track.new", "track.affiliation", "track.declared", "track.declaration_revoked",
          "track.identity_conflict", "track.dropped", "vehicle.link_lost", "vehicle.link_restored", "vehicle.nav",
          "vehicle.gps_glitch", "alert.raised", "alert.changed", "feed.reset", "feed.truncated")


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
    left: list[tuple[dict[str, Any], str]] = field(default_factory=list)  # approved cards whose premise went


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
        self.areas: dict[str, Any] | None = None
        self.run: str | None = None
        self.pending_changes: list[dict[str, Any]] = []
        self.changes_t: float | None = None
        self.truncated = False
        self.compute: list[float] = []

    # ---- the bridge run ---------------------------------------------------------------------------

    def bind_run(self, run: str | None) -> None:
        """This ledger is the bridge run's, or it is set aside (renamed beside the new one) and a
        fresh one started: an intent id of an earlier run names another order in this one."""
        if run is not None and self.led.run == run:
            return
        old = self.led
        if old.run is not None or old.entries or old.cursor:
            if self.ledger_path is not None and self.ledger_path.exists():
                stem = (old.run or "no-run").replace("/", "_")
                dest = self.ledger_path.with_name(f"{self.ledger_path.stem}.{stem}{self.ledger_path.suffix}")
                n = 1
                while dest.exists():
                    dest = self.ledger_path.with_name(f"{self.ledger_path.stem}.{stem}.{n}{self.ledger_path.suffix}")
                    n += 1
                self.ledger_path.rename(dest)
                kept = str(dest)
            else:
                kept = None
            self.log("ledger.reset", was=old.run, run=run, kept=kept,
                     text=f"a new bridge run ({run}): the ledger of {old.run or 'no known run'} is set aside"
                          + (f" as {Path(kept).name}" if kept else ""))
        self.led = Ledger(run=run)

    # ---- reading ----------------------------------------------------------------------------------

    async def start(self, session: Any) -> dict[str, Any]:
        """A new session: the catalog and the policy, the run, and the change feed's cursor (kept
        when the same bridge run answers again after a dropped session)."""
        self.platforms = await session.call("get_platforms")
        self.policy = await session.call("get_policy")
        if self.shadow:
            self.areas = await session.call("get_areas")
        probe = await session.call("get_changes")
        run = probe.get("run")
        self.bind_run(run)
        reset = self.led.cursor is None
        if reset:
            self.led.cursor = probe.get("cursor")
        else:
            r = await session.call("get_changes", since=self.led.cursor, limit=500)
            if r.get("reset"):
                self.led.cursor = r.get("cursor")
                reset = True
            else:
                self.pending_changes += [c for c in r.get("changes") or [] if isinstance(c, dict)]
                self.led.cursor = r.get("cursor", self.led.cursor)
        self.run = run
        return {"run": self.run, "cursor": self.led.cursor, "reset": reset}

    async def poll_changes(self, session: Any) -> list[dict[str, Any]]:
        """Changes since the cursor, kept for the next tick; returns the new ones."""
        new: list[dict[str, Any]] = []
        while True:
            r = await session.call("get_changes", since=self.led.cursor, limit=500)
            if r.get("reset"):
                new.append({"kind": "feed.reset", "run": r.get("run")})
                if r.get("run") != self.run:
                    self.bind_run(r.get("run"))
                self.run = r.get("run")
                self.platforms = None  # a new bridge: the catalog and the policy again
            if r.get("truncated"):
                new.append({"kind": "feed.truncated"})
                self.truncated = True
            new += [c for c in r.get("changes") or [] if isinstance(c, dict)]
            self.led.cursor = r.get("cursor", self.led.cursor)
            self.changes_t = r.get("t", self.changes_t)
            if not r.get("more"):
                break
        self.pending_changes += new
        return new

    async def read_intents(self, session: Any) -> dict[str, Any]:
        """Our intents: the newest page, and every older one the ledger still holds open (by
        explain_intent), so a long run never loses an open card of ours off the page."""
        page = await session.call("get_intents", mine=True, limit=INTENTS_PAGE)
        have = {int(i["id"]) for i in page.get("intents") or []}
        extra = []
        for e in self.led.open_entries():
            if int(e["intent"]) in have:
                continue
            try:
                ex = await session.call("explain_intent", id=int(e["intent"]))
            except Exception:  # noqa: BLE001 (read again next tick)
                continue
            if isinstance(ex.get("intent"), dict):
                extra.append({**ex["intent"], "id": ex["intent"].get("id", e["intent"])})
        return {**page, "intents": list(page.get("intents") or []) + extra}

    async def read(self, session: Any) -> Reads:
        if self.platforms is None or self.policy is None:
            self.platforms = await session.call("get_platforms")
            self.policy = await session.call("get_policy")
            if self.shadow:
                self.areas = await session.call("get_areas")
        fleet = await session.call("get_fleet", compact=True)
        tracks = await session.call("get_tracks")
        inbox = await session.call("get_inbox")
        intents = None if self.shadow else await self.read_intents(session)
        await self.poll_changes(session)
        changes, self.pending_changes = self.pending_changes, []
        if self.truncated:
            # older changes were dropped from the feed: what it would have said that the other
            # reads do not (the engagements; the inbox is read every tick)
            self.truncated = False
            try:
                noms = await session.call("get_nominations")
                self.led.set_engaging(list(noms.get("nominations") or []))
            except Exception:  # noqa: BLE001
                pass
            self.log("feed.truncated", text="older changes were dropped from the change feed: the engagements read again")
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
        alerts = list(reads.inbox.get("alerts") or [])
        led.see_alerts(alerts, t)
        led.update_stuck(alerts, t, cfg)
        tasks = {}
        for v in reads.fleet.get("vehicles") or []:
            task = v.get("task") if isinstance(v.get("task"), dict) else None
            tasks[int(v["uid"])] = int(task["intent_id"]) if task and task.get("intent_id") is not None else None
        decisions = [] if self.shadow else led.sync(
            {int(i["id"]): i for i in (reads.intents or {}).get("intents") or []}, t, cfg, tasks)
        b = build(reads, cfg, led)
        for uid, h in sorted(b.health.items()):
            if h.nav_bad:
                led.nav_bad_t[str(uid)] = round(t, 3)
        if self.shadow:
            b.own_pending = sum(1 for s in led.shadow.values() if s.get("tier") == "human")
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
        left = []
        if not self.shadow:
            for e in led.open_entries():
                if e.get("state") in APPROVED and e["play"] in PLAYS:
                    why = PLAYS[e["play"]].withdraw_reason(e, b)
                    if why:
                        left.append((e, why))
        return Plan(t=t, beliefs=b, results=results, selected=selected, held=held, withdrawals=withdrawals,
                    decisions=decisions, left=left)

    def waiting(self, b: Beliefs) -> list[dict[str, Any]]:
        if self.shadow:
            return [{**s, "intent": -1, "key": k} for k, s in sorted(self.led.shadow.items())]
        return [e for e in self.led.open_entries() if e.get("state") in WAITING]

    def budget_block(self, c: Candidate, b: Beliefs, extra_pending: int) -> str | None:
        """Why the inbox budget cannot take a human-tier card for `c` now, or None. P0 commands
        are exempt by class (a policy that makes a hold human-tier still budgets a release's)."""
        if c.klass == P0:
            return None
        bud = self.cfg.budget
        if b.own_pending + extra_pending >= int(bud["own_pending_max"]):
            return f"inbox budget: {int(bud['own_pending_max'])} cards of ours pending (at most {int(bud['own_pending_max'])})"
        if self.led.tokens(b.t, self.cfg) < 1.0:
            return f"inbox budget: one human-tier card per {bud['human_every_s']:.0f} s after a burst of {int(bud['human_burst'])}"
        return None

    def finish(self, c: Candidate, b: Beliefs, preview: dict[str, Any] | None, veto: list[str],
               estimated: dict[str, Any] | None = None) -> tuple[str, dict[str, Any]]:
        """The rationale and its evidence record (pure)."""
        record = c.record(self.cfg.label, b.t)
        record["engine"] = f"playbook-{ENGINE_VERSION}"
        record["rulebook_v2"] = list(veto)
        if preview is None:
            record["preview"] = {"shadow": True, "tier_estimate": (estimated or {}).get("tier"),
                                 "rule_estimate": (estimated or {}).get("rule")}
        else:
            checks = preview.get("checks") or []
            outcomes = [str(x.get("outcome")) for x in checks]
            summary = ", ".join(f"{outcomes.count(o)} {o}" for o in ("pass", "fail", "unverified", "not_evaluated") if outcomes.count(o))
            pol = preview.get("policy") or {}
            record["preview"] = {"would_reach": preview.get("would_reach"), "tier": pol.get("tier"), "rule": pol.get("rule"),
                                 "checks": f"{len(checks)} checks: {summary or 'none'}"}
        text = PLAYS[c.play].render(record)
        return R.compose(text, record), record

    def prior_task(self, v: Any) -> dict[str, Any] | None:
        """The order of ours a vehicle was flying when ours superseded it (never a failed one)."""
        if v is None or v.task_intent is None or self.led.entry(v.task_intent) is None:
            return None
        if (v.task or {}).get("state") in NOT_RUNNING or (v.mode or "").upper() in RECOVERING_MODES:
            return None
        return {"intent_id": v.task_intent, "task": (v.task or {}).get("task"), "running": True}

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
        notes_now: dict[str, str] = {}
        if self.shadow:
            self.shadow_follow_up(plan)
        cap = int(cfg.budget["previews_per_tick"])
        for c in plan.selected:
            if rep.previews >= cap:
                held.append(Held(c, f"{cap} previews a tick"))
                continue
            if self.shadow:
                self.shadow_one(c, b, reads, rep, held, outcomes)
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
                led.set_backoff(c.key, b.t, "rule book: " + "; ".join(veto), cfg)
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
            # a refusal at submission: the intent exists, rejected by the checks; the session
            # passes the bridge's error as `refused` (policy_agent.AgentSession.propose)
            refused = (str(out.get("refused") or out.get("summary") or "rejected by the checks at submission")[:300]
                       if state == "rejected" or out.get("refused") else None)
            v = b.vehicles.get(c.uid) if c.uid is not None else None
            if iid is not None:
                led.record(int(iid), play=c.play, version=c.version, target=c.target, uid=c.uid, task=c.task,
                           request=c.request, t=b.t, state=state, basis=c.basis, withdraw_if=c.withdraw_if,
                           prior_task=self.prior_task(v), tier=(pv.get("policy") or {}).get("tier"),
                           armed=self.armed(c, b), refused=str(refused) if refused else None)
            who = (c.vehicle or {}).get("callsign") or c.uid
            if refused:
                # refused at submission: an answer of the checks, not of a person; backed off
                until = led.set_backoff(c.key, b.t, f"refused at submission: {str(refused)[:120]}", cfg)
                held.append(Held(c, f"refused at submission (again after {hms(until)}): {str(refused)[:80]}"))
                self.log("proposal", intent=iid, play=c.play, version=c.version, target=c.target, uid=c.uid, state=state,
                         request=c.request, refused=refused, rationale=rationale,
                         text=f"proposed i{iid} {c.task} {who} on {c.target} ({c.play} v{c.version}): refused at submission: "
                              f"{str(refused)[:160]}")
                continue
            led.clear_backoff(c.key)
            if reach == "pending_approval":
                led.take_token(b.t, cfg)
                extra_pending += 1
            if c.play == "isr.recentre" and c.uid is not None:
                led.recentred[str(c.uid)] = round(b.t, 3)
            of = (c.derived or {}).get("stuck_of")
            if c.play == "safety.stuck" and of and c.uid is not None:
                led.block_stuck(of["play"], of["target"], int(c.uid), str(c.derived.get("alert_key")))
            self.log("proposal", intent=iid, play=c.play, version=c.version, target=c.target, uid=c.uid, state=state,
                     request=c.request, rationale=rationale,
                     text=f"proposed i{iid} {c.task} {who} on {c.target} ({c.play} v{c.version}): {state}")
            rep.proposed.append({"intent": iid, "play": c.play, "target": c.target, "uid": c.uid, "state": state,
                                 "request": c.request, "rationale": rationale})
            outcomes[c.play] = f"active: proposed i{iid} {c.task} {who} on {c.target} ({state})"
        # what an engagement in play kept the playbook from doing, said once per key
        for h in held:
            if h.reason.startswith(ENGAGED):
                c = h.candidate
                notes_now[f"withheld|{c.key}"] = (f"would {c.task} {(c.vehicle or {}).get('callsign') or c.uid} for {c.target} "
                                                  f"({c.play}), not sent: {h.reason}")
        # 8. follow up: withdraw what no longer holds (only cards still waiting for the operator)
        if not self.shadow:
            for w in plan.withdrawals:
                try:
                    out = await session.call("withdraw_intent", id=w.intent, reason=f"{w.play}: {w.reason}",
                                             if_state="pending_approval")
                    state = out.get("state")
                except Exception as e:  # noqa: BLE001 (decided meanwhile, or gone out: the next sync says)
                    self.log("withdraw.refused", intent=w.intent, play=w.play, reason=w.reason, error=str(e)[:200],
                             text=f"did not withdraw i{w.intent}: {str(e)[:160]}")
                    continue
                e = led.entry(w.intent)
                if e is not None:
                    e["withdrawn_by"], e["withdraw_reason"] = w.play, w.reason
                self.log("withdrawal", intent=w.intent, play=w.play, reason=w.reason, state=state,
                         text=f"withdrew i{w.intent} ({w.play}): {w.reason}")
                rep.withdrawn.append({"intent": w.intent, "play": w.play, "reason": w.reason, "state": state})
            for e, why in plan.left:
                notes_now[f"left|{e['intent']}"] = (f"i{e['intent']} ({e['play']} on {e['target']}) is {e['state']} and no longer "
                                                    f"holds: {why}. Left as it is: the playbook withdraws only what still "
                                                    f"waits for approval")
        self.say_notes(notes_now)
        sts = statuses(plan.results, held, outcomes, cfg["plays"], cooling(led, b.t))
        rep.statuses = {k: v for k, v in sts.items() if k in cfg["plays"]}
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

    # ---- shadow mode ---------------------------------------------------------------------------------

    def shadow_one(self, c: Candidate, b: Beliefs, reads: Reads, rep: TickReport, held: list[Held],
                   outcomes: dict[str, str]) -> None:
        """One would-be card: its tier estimated from the policy; a human-tier one spends the
        budget as the live engine's would; logged, never proposed."""
        led, cfg = self.led, self.cfg
        v = b.vehicles.get(c.uid) if c.uid is not None else None
        cat = catalog(reads.platforms)
        est = tier.estimate(c.request, armed=self.armed(c, b),
                            endurance_known=bool(v and (cat.get(v.platform) or {}).get("endurance_h")),
                            vehicle_pos=v.pos if v else None, policy=reads.policy, areas=self.areas or {})
        if est["tier"] == "human":
            block = self.budget_block(c, b, 0)
            if block:
                held.append(Held(c, block))
                return
        veto = rulebook.veto(self.armed(c, b), None, c.request, reads.tracks.get("tracks") or [])
        if veto:
            led.set_backoff(c.key, b.t, "rule book: " + "; ".join(veto), cfg)
            held.append(Held(c, "rule book: " + "; ".join(veto)))
            return
        rationale, record = self.finish(c, b, None, veto, est)
        rep.previews += 1
        led.shadow[c.key] = {"play": c.play, "target": c.target, "uid": c.uid, "task": c.task, "request": c.request,
                             "basis": c.basis, "withdraw_if": c.withdraw_if, "t": round(b.t, 3), "armed": self.armed(c, b),
                             "tier": est["tier"]}
        if est["tier"] == "human" and c.klass != P0:
            led.take_token(b.t, cfg)
            b.own_pending += 1
        if c.play == "isr.recentre" and c.uid is not None:
            led.recentred[str(c.uid)] = round(b.t, 3)
        self.log("shadow", play=c.play, target=c.target, track=c.target, uid=c.uid, request=c.request,
                 rationale=rationale, record=record, playbook="playbook", tier_estimate=est,
                 text=f"the playbook would propose {c.task} on {c.request.get('uids')} for {c.target} ({c.play}; "
                      f"{est['tier']} tier by its estimate; not proposed)")
        rep.would.append({"play": c.play, "target": c.target, "uid": c.uid, "request": c.request, "rationale": rationale,
                          "tier": est["tier"]})
        outcomes[c.play] = f"active: would propose {c.task} uid {c.uid} on {c.target}"

    def shadow_follow_up(self, plan: Plan) -> None:
        """Shadow mode: a would-be card whose withdraw condition holds is dropped (logged as a
        would-be withdrawal, or release for one held as running), and a human-tier one past the
        policy's approval wait lapses as expired unread."""
        led, b = self.led, plan.beliefs
        for w in plan.withdrawals:
            s = led.shadow.pop(w.key, None)
            if s is None:
                continue
            led._cool({**s, "intent": -1, "withdrawn_by": "shadow"}, "voided", b.t, self.cfg, None, None)
            what = "release" if s.get("tier") == "auto" else "withdraw"
            self.log("shadow.withdraw", play=s["play"], target=s["target"], uid=s.get("uid"), reason=w.reason,
                     text=f"the playbook would {what} its {s['play']} on {s['target']}: {w.reason}")
        for k, s in sorted(list(led.shadow.items())):
            if s.get("tier") == "human" and b.t - s["t"] >= b.expire_s:
                led.shadow.pop(k)
                led._cool({**s, "intent": -1}, "expired", b.t, self.cfg, None, None)
                self.log("shadow.lapsed", play=s["play"], target=s["target"], uid=s.get("uid"),
                         text=f"the playbook's would-be {s['play']} on {s['target']} lapsed after {b.expire_s:.0f} s (counted as expired)")

    # ---- the situation log ----------------------------------------------------------------------------

    def log_decision(self, d: Decision) -> None:
        bits = [f"i{d.intent} {d.state}"]
        # who decided, and their note, only where the state is their decision (an order that
        # failed or ended was not failed by its approver)
        decided = d.state in ("rejected", "approved", "queued", "dispatched")
        if d.by and decided:
            bits.append(f"by {d.by}")
        if d.note and decided:
            bits.append(f"(note {R.quote(d.note)})")
        if d.detail:
            bits.append(f"({d.detail[:160]})")
        cool = f"; {d.play} on {d.target} cools down until {hms(d.cooldown_until)}" if d.cooldown_until else ""
        self.log("decision", intent=d.intent, play=d.play, target=d.target, uid=d.uid, state=d.state, by=d.by, note=d.note,
                 cooldown_until=d.cooldown_until, detail=d.detail, text=" ".join(bits) + cool)

    def say_notes(self, now: dict[str, str]) -> None:
        """Panel lines said once while they hold (an engagement keeping the playbook off a
        platform: `withheld`; an approved card whose premise went: `left`); said again if they
        come back."""
        for ident in sorted(now):
            if self.led.noted.get(ident) != now[ident]:
                self.led.noted[ident] = now[ident]
                kind = ident.split("|", 1)[0]
                self.log(f"shadow.{kind}" if self.shadow else kind, ident=ident, text=now[ident])
        for ident in [i for i in self.led.noted if i not in now]:
            del self.led.noted[ident]

    def log_statuses(self, sts: dict[str, str]) -> None:
        """A status goes to the log once it has held `status_hold_ticks` ticks (a play starts
        ready: only a change from that is news)."""
        kind = "shadow.status" if self.shadow else "play.status"
        hold = int(self.cfg.t["status_hold_ticks"])
        for name in sorted(sts):
            seen = self.led.status_seen.get(name)
            if seen and seen[0] == sts[name]:
                seen[1] += 1
            else:
                seen = [sts[name], 1]
                self.led.status_seen[name] = seen
            if seen[1] >= hold and self.led.status.get(name, "ready") != sts[name]:
                self.led.status[name] = sts[name]
                self.log(kind, play=name, status=sts[name], text=f"{name}: {sts[name]}")


def key(play: str, target: str, uid: int | None) -> str:
    return key_of(play, target, uid)
