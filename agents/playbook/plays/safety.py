"""The safety plays (P0). Each proposes a command the policy sends out at once (hold and rtl are
auto in the shipped policy, and an engagement in play on a vehicle still lets them through),
and each acts once per instance of its condition.

- `safety.low_reserve`: battery under the policy's `reserve_pct` plus 5 points, link up, not
  already returning (mode RTL, LAND, QRTL, SMART_RTL, or an rtl order): rtl. Score: how far under
  the line, over the line.
- `safety.stuck`: a `not_moving` or `task_failed` alert on a vehicle for 30 s, link up, not on
  its failsafe's return (a vehicle the lost-link failsafe sent home is not stuck, and a hold
  would fight it) and not already holding: hold. A `left_task` alert is not stuck either: the
  autopilot left the task on its own (a failsafe, a mode switch), and the operator decides. Once
  our hold runs the vehicle counts as idle (free again when healthy): the hold parks it.

Either acts once per instance: while its own command for that alert or battery is live (waiting,
queued for the link, gone out), it proposes nothing more there, and a P0 command never takes a
vehicle from another safety command.
  Score: the alert's age over 300 s, at most 1.
"""

from __future__ import annotations

from typing import Any

from .. import rationale as R
from ..beliefs import RETURNING_MODES, Beliefs
from ..ledger import Ledger
from . import P0, Candidate, Play, Result, register
from .isr import holding, vehicle_facts, vehicle_ref

HOLDING_MODES = ("HOLD", "LOITER", "BRAKE", "POSHOLD")
STUCK_KINDS = ("not_moving", "task_failed")


class LowReserve(Play):
    name, klass = "safety.low_reserve", P0

    def evaluate(self, b: Beliefs, led: Ledger) -> Result:
        res = Result()
        line = b.low_reserve_pct
        for uid, v in sorted(b.vehicles.items()):
            if v.battery_pct is None or v.battery_pct >= line:
                continue
            target = f"battery:{uid}"
            if holding((self.name,), target, b, led) is not None:
                res.active.append(f"{v.callsign}: our rtl")
                continue
            if v.link_lost:
                res.blocked.append((target, f"{v.callsign} is under the line: its link is lost"))
                continue
            if (v.mode or "").upper() in RETURNING_MODES or (v.task or {}).get("task") == "rtl":
                res.active.append(f"{v.callsign} is returning")
                continue
            res.candidates.append(Candidate(
                play=self.name, version=self.version, klass=self.klass, score=(line - v.battery_pct) / line,
                target=target, uid=uid, task="rtl", request={"task": "rtl", "uids": [uid]},
                facts=vehicle_facts(v), derived={"line_pct": R.r1(line), "reserve_pct": R.r1(b.reserve_pct)},
                thresholds={"low_reserve_margin_pct": b.cfg.t["low_reserve_margin_pct"], "reserve_pct": b.reserve_pct},
                withdraw_if=[], basis={"battery_pct": R.r0(v.battery_pct)},
                trigger=[f"battery {v.battery_pct:.0f}% under {line:.0f}%"], claims=(uid,), vehicle=vehicle_ref(v),
            ))
        return res

    def render(self, record: dict[str, Any]) -> str:
        v = record["vehicle"]
        return (f"{R.head(record)} Return {v['callsign']} home: battery {v['battery_pct']:.0f}%, under "
                f"{record['derived']['line_pct']:.0f}% (the policy's reserve {record['derived']['reserve_pct']:.0f}% plus "
                f"{record['thresholds']['low_reserve_margin_pct']:.0f} points). {R.tail(record)}")


class Stuck(Play):
    name, klass = "safety.stuck", P0

    def evaluate(self, b: Beliefs, led: Ledger) -> Result:
        res = Result()
        need = b.cfg.t["stuck_s"]
        for a in b.alerts:
            if a.get("kind") not in STUCK_KINDS:
                continue
            try:
                uid = int(a.get("uid"))
            except (TypeError, ValueError):
                continue
            v = b.vehicles.get(uid)
            since = a.get("since_t")
            if v is None or not isinstance(since, (int, float)):
                continue
            target = f"alert:{a.get('id')}"
            if holding((self.name,), target, b, led) is not None:
                res.active.append(f"{v.callsign}: our hold")
                continue
            age = b.t - float(since)
            if age < need:
                continue
            if v.link_lost:
                res.blocked.append((target, f"{v.callsign}: its link is lost"))
                continue
            mode = (v.mode or "").upper()
            if mode in RETURNING_MODES:
                continue
            if mode in HOLDING_MODES or (v.task or {}).get("task") == "hold":
                res.active.append(f"{v.callsign} holds")
                continue
            res.candidates.append(Candidate(
                play=self.name, version=self.version, klass=self.klass, score=min(1.0, age / 300.0),
                target=target, uid=uid, task="hold", request={"task": "hold", "uids": [uid]},
                facts=[{"ref": f"alert:{a.get('id')}", "kind": a.get("kind"), "detail": str(a.get("detail") or "")[:200],
                        "since_t": R.r1(float(since)), "age_s": R.r0(age)}] + vehicle_facts(v),
                derived={"alert_kind": a.get("kind")}, thresholds={"stuck_s": need}, withdraw_if=[],
                basis={"since_t": R.r0(float(since))}, trigger=[f"{a.get('kind')} for {age:.0f} s"], claims=(uid,),
                vehicle=vehicle_ref(v),
            ))
        return res

    def render(self, record: dict[str, Any]) -> str:
        a = record["facts"][0]
        return (f"{R.head(record)} Hold {record['vehicle']['callsign']} where it is: {a['kind']} for {a['age_s']:.0f} s "
                f"(the bridge's detail {R.quote(a['detail'])}). {R.tail(record)}")


for _p in (LowReserve(), Stuck()):
    register(_p)
