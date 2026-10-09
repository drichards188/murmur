"""The link and navigation plays (P3).

- `link.cover_gap`: a vehicle on an orbit or a patrol (anyone's order but an orbit of ours on a
  track, which `isr.investigate` takes over with a fresh centre once its sensor is silent) whose
  link has been lost for 2 x its link's `lost_after_s`: the free vehicle of the same domain that
  can fly that task (unarmed, its navigation not suspect), nearest by ETA, then most battery,
  then lowest uid, takes over the order's route (a patrol) or centre (an orbit, the radius
  widened to 1.1 x its own minimum turn when tighter). The order's parameters come from our own
  card or from `explain_intent` (read once per order). Score: 1. It ends when the link is back:
  a filler still waiting is withdrawn; one that runs is kept (no churn: the operator decides).
  Material change: a new loss (the link came back and went again).
- `link.withdraw_pending`: a card of ours waiting for the operator (pending approval) for a vehicle
  whose link is lost: withdrawn, "link lost at <time>". One the operator already approved is left
  to the bridge's queue (rules 5: it goes when the vehicle can be reached, a human's approval back
  to the inbox after 120 s): withdrawing it would undo a person's decision over a link.
- `nav.suspect_exclude`: a vehicle whose navigation is not ready, off GNSS or whose GPS glitch is
  flagged (now, or within 60 s: the flags must clear for that long) is excluded from every
  tasking play (the free-vehicle definition) and from armed ones by name; a card of ours on an
  armed task waiting for such a vehicle is withdrawn. Until murmur-su0 lands (the bridge's
  readiness re-check at dispatch does not read the GPS glitch flag), an approved or queued armed
  card of ours on such a vehicle is withdrawn too, with that state as the precondition, and the
  line says so: "withdrawn (armed, nav suspect, murmur-su0)". An unarmed approved card is left. The compact fleet read has no satellite
  count, so fewer than six satellites is not seen; no navigation-integrity fact exists yet
  (PLAN.md: a cross-check that can catch a spoofed position), so a spoof that keeps every flag
  good is not seen either, by this play or by anyone.
"""

from __future__ import annotations

import datetime as dt
from typing import Any

from .. import rationale as R
from ..beliefs import NOT_RUNNING, Beliefs, Vehicle, eta_s, haversine_m
from ..ledger import Ledger
from . import P3, Candidate, Play, Result, Withdrawal, register
from .isr import CHOICES, LOOK_PLAYS, RANK_STEP, holding, ranked, sensor_unfit, vehicle_facts, vehicle_ref

GAP_TASKS = ("orbit", "patrol")


def hms(t: float) -> str:
    return dt.datetime.fromtimestamp(t, dt.timezone.utc).strftime("%H:%M:%SZ")


def lost_after(b: Beliefs, uid: int) -> float | None:
    v = ((b.lost_full.get(uid) or {}).get("link") or {}).get("lost_after_s")
    return float(v) if isinstance(v, (int, float)) else None


def order_params(v: Vehicle, led: Ledger) -> dict[str, Any] | None:
    """The parameters of the order `v` is on: our card's request, or what explain_intent gave."""
    iid = v.task_intent
    if iid is None:
        return None
    e = led.entry(iid)
    if e is not None and e.get("request"):
        return {"task": e["task"], "params": {k: e["request"][k] for k in ("center", "radius_m", "route") if k in e["request"]},
                "source": "agent (ours)", "play": e["play"]}
    return led.params.get(str(iid))


def gap_since(v: Vehicle, b: Beliefs) -> float | None:
    for a in b.alerts_for(v.uid):
        if a.get("kind") == "lost_link" and isinstance(a.get("since_t"), (int, float)):
            return float(a["since_t"])
    return None if v.link_age_s is None else b.t - v.link_age_s


class CoverGap(Play):
    name, klass = "link.cover_gap", P3

    def gaps(self, b: Beliefs, led: Ledger) -> list[tuple[Vehicle, float, float | None]]:
        """(vehicle, seconds lost, lost_after_s) of every vehicle on an orbit or patrol whose link is lost."""
        out = []
        for uid, v in sorted(b.vehicles.items()):
            task = v.task or {}
            if not v.link_lost or task.get("task") not in GAP_TASKS or task.get("state") in NOT_RUNNING:
                continue
            out.append((v, v.link_age_s or 0.0, lost_after(b, uid)))
        return out

    def evaluate(self, b: Beliefs, led: Ledger) -> Result:
        res = Result()
        factor = b.cfg.t["link_gap_factor"]
        for v, lost_s, after in self.gaps(b, led):
            target = f"gap:{v.uid}"
            held = holding((self.name,), target, b, led)
            if held is not None:
                res.active.append(f"{v.callsign}'s gap: uid {held.get('uid')} (i{held.get('intent', '-')} {held.get('state', 'would-be')})")
                continue
            if after is None:
                res.blocked.append((target, f"{v.callsign}'s link is lost; its lost_after_s is not known yet"))
                continue
            if lost_s < factor * after:
                continue  # a flap, not yet a gap
            params = order_params(v, led)
            if params and params.get("play") in LOOK_PLAYS:
                continue  # our own look: isr.investigate takes the track with another sensor
            if not params:
                res.blocked.append((target, f"{v.callsign}'s order i{v.task_intent}: its parameters are not known yet"))
                continue
            kind = str(params["task"])
            p = params.get("params") or {}
            anchor = p.get("center") or ((p.get("route") or [None])[0])
            if anchor is None:
                res.blocked.append((target, f"{v.callsign}'s order has no centre or route to take over"))
                continue
            free, busy = ranked(b, (anchor[0], anchor[1]),
                                lambda c, v=v, kind=kind: c.uid != v.uid and c.domain == v.domain and kind in c.tasks and c.armed is False)
            if not free:
                detail = "; ".join(f"{c.callsign}: {why}" for c, why in busy[:3]) or "none in the fleet"
                res.blocked.append((target, f"{v.callsign}'s gap: no free capable vehicle ({detail})"))
                continue
            since = gap_since(v, b)
            for rank, f in enumerate(free[:CHOICES]):
                self.add(res, b, v, f, rank, kind, p, params, anchor, lost_s, after, since, free[rank:], busy)
        return res

    def add(self, res: Result, b: Beliefs, v: Vehicle, f: Vehicle, rank: int, kind: str, p: dict[str, Any],
            params: dict[str, Any], anchor: list[float], lost_s: float, after: float, since: float | None,
            free: list[Vehicle], busy: list[tuple[Vehicle, str]]) -> None:
        factor, target = b.cfg.t["link_gap_factor"], f"gap:{v.uid}"
        if kind == "orbit":
            turn = b.cfg.t["orbit_turn_margin"] * (f.min_turn_m or b.cfg.t["default_min_turn_m"])
            request = {"task": "orbit", "uids": [f.uid], "center": R.ll(p["center"]),
                       "radius_m": float(max(float(p.get("radius_m") or 0.0), float(int(-(-turn // 100)) * 100)))}
        else:
            request = {"task": "patrol", "uids": [f.uid], "route": [R.ll(q) for q in p["route"]]}
        res.candidates.append(Candidate(
            play=self.name, version=self.version, klass=self.klass, score=1.0 - RANK_STEP * rank, target=target, uid=f.uid, task=kind,
            request=request,
            facts=[{"ref": f"vehicle:{v.uid}.link", "lost": True, "age_s": R.r1(lost_s), "lost_after_s": R.r1(after),
                    "since_t": R.r1(since)},
                   {"ref": f"vehicle:{v.uid}.task", "value": v.task, "params": p, "source": params.get("source")}]
            + vehicle_facts(f),
            derived={"lost": vehicle_ref(v), "eta_s": R.r0(eta_s(f, (anchor[0], anchor[1]))), "rank": rank,
                     "distance_m": R.r0(haversine_m(f.pos, anchor)) if f.pos else None},
            thresholds={"link_gap_factor": factor},
            withdraw_if=[f"{v.callsign}'s link is back (a filler already running is kept)",
                         f"{f.callsign} is no longer fit for the task"],
            basis={"since_t": R.r0(since)},
            trigger=[f"{v.callsign} silent {lost_s:.0f} s, over {factor:.0f} x its {after:.0f} s lost-link limit"],
            alternatives=[{"uid": c.uid, "callsign": c.callsign, "eta_s": R.r0(eta_s(c, (anchor[0], anchor[1])))} for c in free[1:4]]
            + [{"uid": c.uid, "callsign": c.callsign, "not_free": why} for c, why in busy[:3]],
            claims=(f.uid,), vehicle=vehicle_ref(f),
        ))

    def withdraw_reason(self, entry: dict[str, Any], b: Beliefs) -> str | None:
        uid = int(str(entry["target"]).split(":", 1)[1])
        v = b.vehicles.get(uid)
        if v is not None and not v.link_lost:
            return f"{v.callsign}'s link is back"
        return sensor_unfit(entry.get("uid"), b)

    def material_change(self, then: dict[str, Any], now: dict[str, Any], b: Beliefs) -> str | None:
        a, c = then.get("since_t"), now.get("since_t")
        if isinstance(a, (int, float)) and isinstance(c, (int, float)) and abs(c - a) > 5:
            return f"a new loss of the link (since {hms(c)}, the last since {hms(a)})"
        return None

    def render(self, record: dict[str, Any]) -> str:
        d = record["derived"]
        link = record["facts"][0]
        task = record["facts"][1]
        passed = d.get("passed_over") or []
        after = f" after {', '.join(passed)}" if passed else ""
        what = (f"its orbit ({record['request']['radius_m']:.0f} m radius) at {record['request']['center'][0]:.4f},"
                f"{record['request']['center'][1]:.4f}" if record["task"] == "orbit" else
                f"its patrol ({len(record['request']['route'])} points)")
        return (f"{R.head(record)} Cover {d['lost']['callsign']}'s gap: {record['vehicle']['callsign']} takes over {what}. "
                f"{d['lost']['callsign']} has been silent {link['age_s']:.0f} s (its lost-link limit {link['lost_after_s']:.0f} s), "
                f"on {task['value'].get('task')} i{task['value'].get('intent_id')} ({task.get('source')}). "
                f"{record['vehicle']['callsign']} is the free capable vehicle nearest by time{after} "
                f"({R.mins(d.get('eta_s'))} away; battery {record['vehicle']['battery_pct']:.0f}%). {R.tail(record)}")


class WithdrawPending(Play):
    name, klass, proposes = "link.withdraw_pending", P3, False

    def withdrawals(self, b: Beliefs, led: Ledger, waiting: list[dict[str, Any]]) -> list[Withdrawal]:
        out = []
        for e in waiting:
            if e.get("state") not in ("pending_approval", None):
                continue  # approved or queued: the bridge's queue holds it for the link
            v = b.vehicles.get(e.get("uid")) if e.get("uid") is not None else None
            if v is not None and v.link_lost:
                since = gap_since(v, b)
                when = hms(since) if since is not None else "?"
                out.append(Withdrawal(intent=int(e["intent"]), key=str(e.get("key") or ""), play=self.name,
                                      reason=f"{v.callsign}'s link lost at {when} (silent {v.link_age_s or 0:.0f} s)"))
        return out


class NavSuspect(Play):
    name, klass, proposes = "nav.suspect_exclude", P3, False

    def evaluate(self, b: Beliefs, led: Ledger) -> Result:
        res = Result()
        if b.nav_suspect:
            res.note = "excluding " + "; ".join(f"{b.vehicles[u].callsign} ({why})" for u, why in sorted(b.nav_suspect.items()))
        return res

    def withdrawals(self, b: Beliefs, led: Ledger, waiting: list[dict[str, Any]]) -> list[Withdrawal]:
        out = []
        # interim, until murmur-su0: the bridge's re-check at dispatch does not read the GPS glitch
        # flag, so an approved or queued armed card of ours is withdrawn too (from that state only)
        approved = [e for e in led.open_entries() if e.get("state") in INTERIM_STATES and e.get("armed")]
        for e in list(waiting) + approved:
            state = e.get("state")
            if state not in ("pending_approval", None) + INTERIM_STATES:
                continue
            uid = e.get("uid")
            if uid in b.nav_suspect and e.get("armed"):
                why = f"{b.vehicles[uid].callsign}'s navigation is suspect ({b.nav_suspect[uid]}): excluded from armed tasks"
                if state in INTERIM_STATES:
                    why += "; withdrawn (armed, nav suspect, murmur-su0)"
                out.append(Withdrawal(intent=int(e["intent"]), key=str(e.get("key") or ""), play=self.name, reason=why,
                                      if_state=state or "pending_approval"))
        return out


INTERIM_STATES = ("approved", "queued")  # murmur-su0: remove with it


for _p in (CoverGap(), WithdrawPending(), NavSuspect()):
    register(_p)
