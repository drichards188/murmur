"""A situation's scripted faults and proposers (families.py), as a process of the episode.

Each item is `{"at_s": t, "kind": ...}` or waits for a trigger:

- `link_cut` `{uid, duration_s}` at `at_s`, or `{after_first_look: [lo, hi] s, delay_s,
  duration_s, deadline_s}`: a cut of both directions of a vehicle's link (the emulator's own `cut`
  event, as the storyline's `link` step appends one to the run's events file). With
  `after_first_look`, the vehicle is the one flying the first look of the playbook's to be
  dispatched, cut `delay_s` (drawn by the generator) after its dispatch; if none is dispatched by
  `deadline_s`, nothing is cut and the log says "no look to cut". The log records the vehicle's
  mode at the cut (truth) and whether it was already going home on its own failsafe
  (`already_recovering`, since when), in which case no look in progress was cut.
- `stop` `{uid}`: the stand-in stops making way (its cruise and top speed set to zero, as a
  fouled propeller would; nothing goes through the bridge).
- `flood` `{proposals: [{task, uids, center, radius_m | route}], window_s}`: a scripted proposer
  (`agent-scripted`, over MCP, its rationale labelled as the storyline's) puts each in the inbox,
  evenly over the window.

Everything it did goes to `log` (the record's `script`), with truth's view of it.
"""

from __future__ import annotations

import sys
from typing import Any

from eval.agent import _mcp_args
from eval.situation import SCRIPTED_TIMELINE, STORYLINE_AGENT
from eval.situation_run import labelled

from . import REPO
from .sim import HeadlessSession

sys.path.insert(0, str(REPO / "agents"))
from playbook.beliefs import RECOVERING_MODES  # noqa: E402


class Script:
    def __init__(self, ep: Any, items: list[dict[str, Any]]):
        self.ep = ep
        self.items = [dict(i) for i in items]
        self.log: list[dict[str, Any]] = []
        self.queue: list[tuple[float, int, dict[str, Any]]] = []
        self.watch: list[dict[str, Any]] = []
        self.n = 0
        for it in self.items:
            if it.get("kind") == "link_cut" and it.get("after_first_look"):
                self.watch.append(it)
                self._push(float(it["deadline_s"]), {**it, "deadline": True})
            elif it.get("kind") == "flood":
                props = list(it["proposals"])
                step = float(it.get("window_s", 0.0)) / max(1, len(props))
                for k, p in enumerate(props):
                    self._push(float(it["at_s"]) + k * step, {"kind": "propose", "proposal": p, "flood": True})
            else:
                self._push(float(it["at_s"]), it)

    def _push(self, t: float, it: dict[str, Any]) -> None:
        self.n += 1
        self.queue.append((round(float(t), 6), self.n, it))
        self.queue.sort(key=lambda x: (x[0], x[1]))

    def first(self) -> float | None:
        nxt = [self.queue[0][0]] if self.queue else []
        if self.watch:
            nxt.append(1.0)
        return min(nxt) if nxt else None

    def wake(self, now: float) -> float | None:
        self._watch(now)
        while self.queue and self.queue[0][0] <= now + 1e-6:
            _, _, it = self.queue.pop(0)
            self._do(now, it)
        return self.first_after(now)

    def first_after(self, now: float) -> float | None:
        nxt = [self.queue[0][0]] if self.queue else []
        if self.watch:
            nxt.append(now + 1.0)
        return min(nxt) if nxt else None

    def _watch(self, now: float) -> None:
        """A trigger: the first look of the playbook's dispatched (its intent in the operator
        API's list, as the console shows it)."""
        if not self.watch:
            return
        status, body = self.ep.sim.api("GET", "/api/intents")
        if status != 200:
            return
        looks = []
        for i in (body or {}).get("intents", []):
            src = i.get("source") or {}
            if src.get("kind") != "agent" or src.get("id") != "agent-playbook" or i.get("kind") != "orbit":
                continue
            disp = [h for h in i.get("history") or [] if h.get("state") == "dispatched"]
            if disp:
                looks.append((float(disp[0]["t"]) / 1000.0, int(i["id"]), [int(u) for u in i.get("uids") or []]))
        if not looks:
            return
        t_disp, iid, uids = min(looks)
        epoch = self.ep.sim_epoch
        for it in list(self.watch):
            self.watch.remove(it)
            lo, hi = it["after_first_look"]
            delay = float(it.get("delay_s", lo))
            at = (t_disp - epoch) + delay
            self.queue = [q for q in self.queue if not (q[2].get("deadline") and q[2].get("after_first_look") == it.get("after_first_look"))]
            self._push(max(at, now), {"kind": "link_cut", "uid": uids[0], "duration_s": it["duration_s"],
                                      "trigger": {"intent": iid, "dispatched_s": round(t_disp - epoch, 3), "delay_s": delay}})

    def _recovering_since(self, uid: int, now: float) -> float | None:
        """Since when (the episode's samples of our vehicles, every 2 s) `uid` has been in a
        recovering mode without a break, up to `now`."""
        since = None
        for s in getattr(self.ep, "ours_samples", None) or []:
            if s["t"] > now:
                break
            mode = next((o[3] for o in s["ours"] if o[0] == uid), None)
            since = (since if since is not None else s["t"]) if str(mode or "").upper() in RECOVERING_MODES else None
        return since

    def _do(self, now: float, it: dict[str, Any]) -> None:
        sim = self.ep.sim
        kind = it.get("kind")
        rec: dict[str, Any] = {"t": round(now, 3), "kind": kind}
        try:
            if kind == "link_cut" and it.get("deadline"):
                # no look dispatched by the deadline: nothing to cut
                self.watch = [w for w in self.watch if w.get("after_first_look") != it.get("after_first_look")]
                rec.update(skipped=f"no look to cut: none dispatched by {float(it['deadline_s']):g} s")
            elif kind == "link_cut":
                uid = int(it["uid"])
                # what the vehicle was doing when its link was cut, by truth: a look in progress,
                # or already going home on its own failsafe (then no look is being cut)
                mode = next((o.get("mode") for o in (sim.truth(association=False).get("ours") or [])
                             if int(o.get("uid", -1)) == uid), None)
                recovering = str(mode or "").upper() in RECOVERING_MODES
                ev = {"name": f"headless cut uid {uid}", "at_s": round(now, 3), "duration_s": float(it["duration_s"]),
                      "uid": uid, "cut": True}
                sim.fault("link_event", event=ev)
                rec.update(uid=uid, duration_s=float(it["duration_s"]), until_s=round(now + float(it["duration_s"]), 3),
                           trigger=it.get("trigger"), mode_at_cut=mode, already_recovering=recovering,
                           recovering_since_s=self._recovering_since(uid, now) if recovering else None)
            elif kind == "stop":
                sim.fault("stop", uid=int(it["uid"]))
                rec.update(uid=int(it["uid"]))
            elif kind == "propose":
                p = it["proposal"]
                s = HeadlessSession(sim, STORYLINE_AGENT, "0.1")
                text = labelled(SCRIPTED_TIMELINE, p.get("note") or "scripted proposal")
                out = self.ep.run_async(s.propose(text, **_mcp_args(p)))
                rec.update(flood=bool(it.get("flood")), intent=out.get("id"), state=out.get("state"),
                           refused=out.get("refused"), request={k: p[k] for k in p if k != "note"})
            else:
                rec["error"] = f"unknown kind {kind!r}"
        except Exception as e:  # noqa: BLE001 (recorded; the episode goes on)
            rec["error"] = f"{type(e).__name__}: {e}"
        self.log.append(rec)
