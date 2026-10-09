"""The safety plays (P0). Each proposes a command the policy sends out at once (hold and rtl are
auto in the shipped policy), and each acts once per instance of its condition while its own
command for it is live (waiting, queued for the link, gone out). Neither acts on a vehicle that
is recovering on its own (returning, landing or surfacing: `beliefs.RECOVERING_MODES`, which
covers every such mode of ArduPilot's vehicle types and of the stand-ins' failsafes), nor on a
platform with an engagement in play (the arbiter: no order of any kind there).

- `safety.low_reserve`: battery under the policy's `reserve_pct` plus 5 points, link up, not
  recovering on its own and not on an rtl order: rtl. Score: how far under the line, over the
  line. **A person's later decision wins**: while someone else's order newer than our last rtl
  runs on the vehicle (it superseded our rtl, or came after it), the play does not send rtl
  again. It raises it once more if the battery then falls under the policy's own reserve (the
  hard line, not the reserve plus 5), saying so; and again only on a cause it can name: the
  order that superseded ours no longer stands and no later order of anyone else's runs on it.
- `safety.stuck`: a `not_moving` or `task_failed` alert on a vehicle active in our reads for
  30 s (counted from the first read in which the playbook saw it, keyed by the alert's id and
  kind: the alert's `since_t` is its order's last state change, not when it stopped moving),
  link up, not recovering on its own and not already holding: hold. The rationale quotes the
  bridge's own detail ("NOT MOVING for 12s ...") rather than inventing a duration. A
  `left_task` alert is not stuck: the autopilot left the task on its own (a failsafe, a mode
  switch), and the operator decides. When the stuck order was ours, the vehicle is not given
  that task (play and target) again until the alert has cleared and `stuck_retry_s` more
  (ledger `stuck_blocks`); it stays free for other tasks. Score: how long it has been seen,
  over 300 s, at most 1.
"""

from __future__ import annotations

from typing import Any

from .. import rationale as R
from ..beliefs import NOT_RUNNING, RECOVERING_MODES, Beliefs
from ..ledger import Ledger
from . import P0, Candidate, Play, Result, register
from .isr import holding, vehicle_facts, vehicle_ref

HOLDING_MODES = ("HOLD", "LOITER", "BRAKE", "POSHOLD")
STUCK_KINDS = ("not_moving", "task_failed")
PARKS = ("isr.release", "safety.stuck")


class LowReserve(Play):
    name, klass = "safety.low_reserve", P0

    def evaluate(self, b: Beliefs, led: Ledger) -> Result:
        res = Result()
        line, hard = b.low_reserve_pct, b.reserve_pct
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
            if (v.mode or "").upper() in RECOVERING_MODES or (v.task or {}).get("task") == "rtl":
                res.active.append(f"{v.callsign} is recovering on its own")
                continue
            mine = led.entries_for(self.name, target)
            last = mine[-1] if mine else None
            cause, cause_text = "line", None
            # a person's later decision: someone else's order, newer than our last rtl, running on it
            cur = v.task_intent
            later = (last is not None and cur is not None and led.entry(cur) is None
                     and (cur > int(last["intent"]) or last.get("superseded_by") == cur)
                     and (v.task or {}).get("state") not in NOT_RUNNING)
            if later:
                hard_used = any((e.get("basis") or {}).get("cause") == "hard" for e in mine)
                if v.battery_pct < hard and not hard_used:
                    cause = "hard"
                    cause_text = (f"i{cur}, someone else's order after our rtl i{last['intent']}, still stands, and the battery "
                                  f"is now under the policy's own reserve {hard:.0f}%: raised once more")
                else:
                    more = "" if hard_used else f" (once more if it falls under the policy's reserve {hard:.0f}%)"
                    res.active.append(f"{v.callsign}: i{cur}, someone else's order after our rtl i{last['intent']}: "
                                      f"not sent again while it stands{more}")
                    continue
            elif last is not None and last.get("superseded_by") is not None:
                cause = "superseder_ended"
                cause_text = (f"i{last['superseded_by']}, the order that superseded our rtl i{last['intent']}, no longer stands "
                              f"and no later order of anyone else's runs on it")
            trigger = [f"battery {v.battery_pct:.0f}% under {line:.0f}%"] + ([cause_text] if cause_text else [])
            res.candidates.append(Candidate(
                play=self.name, version=self.version, klass=self.klass, score=(line - v.battery_pct) / line,
                target=target, uid=uid, task="rtl", request={"task": "rtl", "uids": [uid]},
                facts=vehicle_facts(v),
                derived={"line_pct": R.r1(line), "reserve_pct": R.r1(hard), "re_raise": cause_text},
                thresholds={"low_reserve_margin_pct": b.cfg.t["low_reserve_margin_pct"], "reserve_pct": hard},
                withdraw_if=[], basis={"battery_pct": R.r0(v.battery_pct), "cause": cause},
                trigger=trigger, claims=(uid,), vehicle=vehicle_ref(v),
            ))
        return res

    def render(self, record: dict[str, Any]) -> str:
        v, d = record["vehicle"], record["derived"]
        again = f" Raised again: {d['re_raise']}." if d.get("re_raise") else ""
        return (f"{R.head(record)} Return {v['callsign']} home: battery {v['battery_pct']:.0f}%, under "
                f"{d['line_pct']:.0f}% (the policy's reserve {d['reserve_pct']:.0f}% plus "
                f"{record['thresholds']['low_reserve_margin_pct']:.0f} points).{again} {R.tail(record)}")


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
            if v is None:
                continue
            target = f"alert:{a.get('id')}"
            if holding((self.name,), target, b, led) is not None:
                res.active.append(f"{v.callsign}: our hold")
                continue
            key = f"{a.get('id')}|{a.get('kind')}"
            first = led.alert_seen.get(key)
            seen = b.t - first if first is not None else 0.0
            if seen < need:
                continue
            if v.link_lost:
                res.blocked.append((target, f"{v.callsign}: its link is lost"))
                continue
            mode = (v.mode or "").upper()
            if mode in RECOVERING_MODES:
                continue
            if mode in HOLDING_MODES or (v.task or {}).get("task") == "hold":
                res.active.append(f"{v.callsign} holds")
                continue
            stuck_of = None
            e = led.entry(v.task_intent) if v.task_intent is not None else None
            if e is not None and e["play"] not in PARKS:
                stuck_of = {"play": e["play"], "target": e["target"], "intent": e["intent"]}
            res.candidates.append(Candidate(
                play=self.name, version=self.version, klass=self.klass, score=min(1.0, seen / 300.0),
                target=target, uid=uid, task="hold", request={"task": "hold", "uids": [uid]},
                facts=[{"ref": f"alert:{a.get('id')}", "kind": a.get("kind"), "detail": str(a.get("detail") or "")[:200],
                        "first_seen_t": R.r1(first), "seen_s": R.r0(seen)}] + vehicle_facts(v),
                derived={"alert_kind": a.get("kind"), "alert_key": key, "stuck_of": stuck_of},
                thresholds={"stuck_s": need, "stuck_retry_s": b.cfg.t["stuck_retry_s"]}, withdraw_if=[],
                basis={"first_seen_t": R.r0(first)},
                trigger=[f"the bridge's {a.get('kind')} alert active in our reads for {seen:.0f} s"], claims=(uid,),
                vehicle=vehicle_ref(v),
            ))
        return res

    def render(self, record: dict[str, Any]) -> str:
        a, d = record["facts"][0], record["derived"]
        of = d.get("stuck_of")
        later = (f" It is not given our {of['play']} on {of['target']} again until the alert clears and "
                 f"{record['thresholds']['stuck_retry_s']:.0f} s more." if of else "")
        return (f"{R.head(record)} Hold {record['vehicle']['callsign']} where it is: the bridge's {a['kind']} alert has been "
                f"active in our reads for {a['seen_s']:.0f} s (its detail {R.quote(a['detail'])}).{later} {R.tail(record)}")


for _p in (LowReserve(), Stuck()):
    register(_p)
