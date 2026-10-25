"""A situation's scripted faults and proposers (families.py), as a process of the episode.

Each item is `{"at_s": t, "kind": ...}` or waits for a trigger:

- `link_cut` `{uid, duration_s}` at `at_s`, or `{after_first_look: [lo, hi] s, delay_s,
  duration_s, deadline_s}`: a cut of both directions of a vehicle's link (the emulator's own `cut`
  event, as the storyline's `link` step appends one to the run's events file). With
  `after_first_look`, the vehicle is the one flying the first look of the agent under test to be
  dispatched (`first_look`), cut `delay_s` (drawn by the generator) after its dispatch; if none
  is dispatched by `deadline_s`, nothing is cut and the log says "no look to cut". The log
  records the vehicle's mode at the cut (truth) and whether it was already going home on its own
  failsafe (`already_recovering`, since when), in which case no look in progress was cut.
- `stop` `{uid}`: the stand-in stops making way (its cruise and top speed set to zero, as a
  fouled propeller would; nothing goes through the bridge). Or, with `after_first_look` (scoring
  v2's `stuck-vehicle`), the vehicle flying the first look of the agent under test stops
  `delay_s` after its dispatch; if none is dispatched by `deadline_s`, `fallback_uid` stops then.
- `flood` `{proposals: [{task, uids, center, radius_m | route}], window_s}`: a scripted proposer
  (`agent-scripted`, over MCP, its rationale labelled as the storyline's) puts each in the inbox,
  evenly over the window.
- `step` `{id, step, params, proposer}`: one of the situation's `[[step]]`s (world v3, H4: the
  situation timeline, situations.py `timeline`), at its `at_s`, run as the live storyline runs
  it (eval/storyline.py) on the simulated clock: `propose` (its vehicles as the C2 lists them,
  its centre a place, a track as the C2 shows it, `{select = ...}` or `{id = ...}`, or a
  vehicle; tried again every 2 s for `retry_s`, 20 by default, while a track or vehicle it
  names is not there yet; proposed by `agent-scripted` over MCP, its rationale labelled as the
  timeline's), `flood` (`count` proposals `interval_s` apart, cycling its templates), `link`
  (a link effect on its vehicles from `delay_s` on for `duration_s`: the emulator's own event, as
  the live storyline appends to the run's events file), `declare` (op-planner declares a track),
  and the headless faults (eval/situation.py `HEADLESS_STEP_KEYS`; crates/murmur-headless/src/
  sim/faults.rs models each): `gnss`, `battery`, `reboot`, `stop`, `current`, on the vehicles
  its selector names in the fleet as planned (a fault reaches a vehicle the C2 does not hear).
  An `engage` step nominates as the live one does (eval/engage.py `EngageAdapter.run_step`):
  its track and vehicles resolved as a proposal's; with `unless_nominated`, skipped while a
  nomination on the track stands, whoever made it; with `wait_s`, the nomination form's dry run
  (`get_nomination_options`) asked every 3 s until the platform passes every check, skipped with
  the failing checks if it never does; then `agent-scripted` nominates it over MCP. Who signs it
  is the episode's (`Options.signers`, signers.py, docs/HEADLESS.md section 16); without signers
  the nomination waits and expires, as live with no one at the console. A `note` is the panel's
  and never reaches the script.

**The first look** (`first_look`; eval/SCORING-V2.md 4.5): the first intent, by its dispatch
time, that an agent proposed (any agent but the scripted ones, `agent-setup`'s opening orders and
`agent-scripted`'s proposals), of a look task (the item's `look_tasks`; orbit by default, as v1's
items have it; v2's are orbit and patrol), flown by a vehicle in the item's `look_uids` when it
names them (v2: the vehicles that carry a classifying sensor, the Pumas). Until 2026-10-21 it
counted the playbook's orbits only, so a model agent's look never cut anything; for the playbook
nothing changes.

Everything it did goes to `log` (the record's `script`), with truth's view of it.
"""

from __future__ import annotations

import sys
from typing import Any

from eval.agent import _mcp_args
from eval.situation import SCRIPTED_TIMELINE, SETUP_AGENT, STORYLINE_AGENT, STORYLINE_OPERATOR, _select_planned
from eval.situation_run import labelled
from eval.storyline import StepError, offset, select_track, select_vehicles, target_record

from . import REPO
from .sim import HeadlessSession

sys.path.insert(0, str(REPO / "agents"))
from playbook.beliefs import RECOVERING_MODES  # noqa: E402

#: the scripted proposers, never the agent under test: the opening orders and the timeline's
#: proposals (SCORING-V2.md 4.5: a trigger never fires on setup or scripted orders)
SCRIPTED_AGENTS = (SETUP_AGENT, STORYLINE_AGENT)
#: the scripted items that wait for the first look
TRIGGERED = ("link_cut", "stop")


def first_look(intents: list[dict[str, Any]], item: dict[str, Any]) -> tuple[float, int, int] | None:
    """The first dispatched look of the agent under test, as `item` counts one (module doc):
    (its dispatch time, unix s; its intent id; the vehicle flying it), or None. `intents` is the
    operator API's list (`GET /api/intents`), each with its source, kind, uids and history."""
    tasks = tuple(item.get("look_tasks") or ("orbit",))
    only = {int(u) for u in item["look_uids"]} if item.get("look_uids") else None
    looks = []
    for i in intents:
        src = i.get("source") or {}
        if src.get("kind") != "agent" or src.get("id") in SCRIPTED_AGENTS or i.get("kind") not in tasks:
            continue
        uids = [int(u) for u in i.get("uids") or [] if only is None or int(u) in only]
        if not uids:
            continue
        disp = [h for h in i.get("history") or [] if h.get("state") == "dispatched"]
        if disp:
            looks.append((float(disp[0]["t"]) / 1000.0, int(i["id"]), uids[0]))
    return min(looks) if looks else None


class Script:
    def __init__(self, ep: Any, items: list[dict[str, Any]]):
        self.ep = ep
        self.items = [dict(i) for i in items]
        self.log: list[dict[str, Any]] = []
        self.queue: list[tuple[float, int, dict[str, Any]]] = []
        self.watch: list[dict[str, Any]] = []
        self.n = 0
        for it in self.items:
            if it.get("kind") in TRIGGERED and it.get("after_first_look"):
                self.watch.append(it)
                # the deadline names its watch item (by identity) so a trigger can drop it
                self._push(float(it["deadline_s"]), {**it, "deadline": True, "watch": it})
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
        """A trigger: the first look of the agent under test dispatched (`first_look`, its intent
        in the operator API's list, as the console shows it)."""
        if not self.watch:
            return
        status, body = self.ep.sim.api("GET", "/api/intents")
        if status != 200:
            return
        intents = (body or {}).get("intents", [])
        epoch = self.ep.sim_epoch
        for it in list(self.watch):
            look = first_look(intents, it)
            if look is None:
                continue
            t_disp, iid, uid = look
            self.watch.remove(it)
            lo, hi = it["after_first_look"]
            delay = float(it.get("delay_s", lo))
            at = (t_disp - epoch) + delay
            self.queue = [q for q in self.queue if q[2].get("watch") is not it]
            trigger = {"intent": iid, "dispatched_s": round(t_disp - epoch, 3), "delay_s": delay}
            if it["kind"] == "link_cut":
                self._push(max(at, now), {"kind": "link_cut", "uid": uid, "duration_s": it["duration_s"],
                                          "trigger": trigger})
            else:
                self._push(max(at, now), {"kind": "stop", "uid": uid, "trigger": trigger})

    def _mode(self, uid: int) -> Any:
        """The vehicle's mode now, by truth."""
        return next((o.get("mode") for o in (self.ep.sim.truth(association=False).get("ours") or [])
                     if int(o.get("uid", -1)) == uid), None)

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
                self.watch = [w for w in self.watch if w is not it.get("watch")]
                rec.update(skipped=f"no look to cut: none dispatched by {float(it['deadline_s']):g} s")
            elif kind == "stop" and it.get("deadline"):
                # no look dispatched by the deadline: the fallback vehicle stops (on its own task)
                self.watch = [w for w in self.watch if w is not it.get("watch")]
                uid = int(it["fallback_uid"])
                mode = self._mode(uid)
                sim.fault("stop", uid=uid)
                rec.update(uid=uid, mode_at_stop=mode,
                           fallback=f"no look dispatched by {float(it['deadline_s']):g} s: the fallback vehicle stops")
            elif kind == "link_cut":
                uid = int(it["uid"])
                # what the vehicle was doing when its link was cut, by truth: a look in progress,
                # or already going home on its own failsafe (then no look is being cut)
                mode = self._mode(uid)
                recovering = str(mode or "").upper() in RECOVERING_MODES
                ev = {"name": f"headless cut uid {uid}", "at_s": round(now, 3), "duration_s": float(it["duration_s"]),
                      "uid": uid, "cut": True}
                sim.fault("link_event", event=ev)
                rec.update(uid=uid, duration_s=float(it["duration_s"]), until_s=round(now + float(it["duration_s"]), 3),
                           trigger=it.get("trigger"), mode_at_cut=mode, already_recovering=recovering,
                           recovering_since_s=self._recovering_since(uid, now) if recovering else None)
            elif kind == "stop":
                uid = int(it["uid"])
                # a triggered stop (v2) also records the look it hit and the mode at the stop; a
                # timed one (v1) its vehicle only, as before
                extra = {"trigger": it["trigger"], "mode_at_stop": self._mode(uid)} if it.get("trigger") else {}
                sim.fault("stop", uid=uid)
                rec.update(uid=uid, **extra)
            elif kind == "probe":
                # truth for the labels (scoring v3's F14): an operator API read of the space segment
                # (GET, logged nowhere), its answer kept in the script's log only; never an agent's
                status, out = sim.api("GET", str(it["path"]))
                ans = out if isinstance(out, dict) else {}
                rec.update(id=it.get("id"), path=it["path"], status=status,
                           answer={"samples": [{"t": s.get("t"), "visible": s.get("visible")}
                                               for s in ans.get("samples") or []],
                                   "visible_min": ans.get("visible_min"), "visible_max": ans.get("visible_max")})
            elif kind == "propose":
                p = it["proposal"]
                s = HeadlessSession(sim, STORYLINE_AGENT, "0.1")
                text = labelled(SCRIPTED_TIMELINE, p.get("note") or "scripted proposal")
                out = self.ep.run_async(s.propose(text, **_mcp_args(p)))
                rec.update(flood=bool(it.get("flood")), intent=out.get("id"), state=out.get("state"),
                           refused=out.get("refused"), request={k: p[k] for k in p if k != "note"})
            elif kind == "step":
                if self._step(now, it, rec) is False:
                    return  # tried again later (`retry_s`): logged when it is done
            else:
                rec["error"] = f"unknown kind {kind!r}"
        except Exception as e:  # noqa: BLE001 (recorded; the episode goes on)
            rec["error"] = f"{type(e).__name__}: {e}"
        self.log.append(rec)

    # ---- the situation's timeline (`step` items) ---------------------------------------------

    def _planned(self, sel: Any) -> list[dict[str, Any]]:
        """The vehicles a fault step's selector names in the fleet as planned (uid, callsign,
        platform: sim/swarm.py's), whether the C2 hears them or not; none: every vehicle."""
        fleet = list(getattr(self.ep, "planned", None) or [])
        if sel is None:
            return fleet
        chosen = _select_planned(sel, fleet)
        if not chosen:
            raise ValueError(f"no vehicle of the fleet matches {sel!r}")
        return chosen

    def _step(self, now: float, it: dict[str, Any], rec: dict[str, Any]) -> bool | None:
        """One `[[step]]` (module doc): what it did goes into `rec`. False: not yet (a proposal
        whose track or vehicle is not there), tried again in 2 s."""
        sim = self.ep.sim
        kind, p = it["step"], it.get("params") or {}
        rec.update(step=it["id"], step_kind=kind)
        if kind in ("gnss", "battery", "reboot", "stop"):
            vehicles = self._planned(p.get("vehicles"))
            uids = [int(v["uid"]) for v in vehicles]
            # a jammer or spoofer without vehicles acts on whoever is in its circle
            rec["uids"] = uids if p.get("vehicles") is not None else "every vehicle in its circle"
            if kind == "gnss":
                fields = {k: p[k] for k in ("effect", "circle", "power_w", "height_m", "walk_mps", "bearing_deg",
                                            "max_offset_m", "duration_s") if k in p}
                out = sim.fault("gnss", name=f"situation step {it['id']}",
                                uids=uids if p.get("vehicles") is not None else None, **fields)
                rec.update(fault=out, **{k: v for k, v in fields.items() if k != "circle"})
            else:
                outs = []
                for uid in uids:
                    if kind == "stop":
                        outs.append(sim.fault("stop", uid=uid))
                    elif kind == "battery":
                        outs.append(sim.fault("battery", uid=uid, **{k: p[k] for k in ("drop_pct", "rate", "duration_s")
                                                                     if k in p}))
                    else:
                        outs.append(sim.fault("reboot", uid=uid, **({"down_s": p["down_s"]} if "down_s" in p else {})))
                rec["fault"] = outs
        elif kind == "current":
            rec["fault"] = sim.fault("current", **{k: p[k] for k in ("east_mps", "north_mps", "duration_s") if k in p})
        elif kind == "link":
            uids = [int(v["uid"]) for v in self._planned(p["vehicles"])]
            delay = float(p.get("delay_s", 0.0))
            ev: dict[str, Any] = {"name": f"situation step {it['id']}", "at_s": round(now + delay, 3),
                                  "duration_s": float(p["duration_s"]), "uids": uids}
            for k in ("cut", "loss", "extra_latency_ms"):
                if p.get(k) is not None:
                    ev[k] = p[k]
            sim.fault("link_event", event=ev)
            rec.update(uids=uids, starts_s=ev["at_s"], ends_s=round(ev["at_s"] + ev["duration_s"], 3),
                       effect={k: ev[k] for k in ("cut", "loss", "extra_latency_ms") if k in ev})
        elif kind == "flood":
            count, interval = int(p.get("count", 6)), float(p.get("interval_s", 8))
            templates = list(p.get("templates") or [])
            for k in range(count):
                t = {"retry_s": 0, **templates[k % len(templates)]}
                self._push(now + k * interval, {**it, "id": f"{it['id']}-{k + 1}", "step": "propose", "params": t})
            rec.update(count=count, interval_s=interval)
        elif kind == "propose":
            return self._propose(now, it, rec)
        elif kind == "declare":
            status, body = sim.api("GET", "/api/tracks")
            track = select_track(p["track"], (body or {}).get("tracks", []) if status == 200 else [], {},
                                 self.ep.sim_epoch + now)
            if track is None:
                raise ValueError(f"declare: no track matches {p['track']!r}")
            # the note says it is a script's, as the live timeline's bridge labels it
            req = {"affiliation": p["affiliation"], "note": labelled(SCRIPTED_TIMELINE, p.get("note"))}
            status, out = sim.api("POST", f"/api/tracks/{track['id']}/declare", req, STORYLINE_OPERATOR)
            rec.update(operator=STORYLINE_OPERATOR, track=track["id"], affiliation=p["affiliation"], status=status,
                       result=((out or {}).get("declared") or {}).get("affiliation"))
        elif kind == "engage":
            return self._engage(now, it, rec)
        else:
            rec["error"] = f"unknown step kind {kind!r}"
        return None

    def _propose(self, now: float, it: dict[str, Any], rec: dict[str, Any]) -> bool | None:
        """A proposal step as eval/storyline.py `prepare` resolves it, against the C2 now."""
        sim, p = self.ep.sim, it["params"]
        if it.get("proposer", "agent") != "agent":
            rec["error"] = "the operator proposer (op-planner) is not in the headless simulator: [storyline] proposer = agent"
            return None
        first = float(it.get("first_t", now))
        try:
            status, state = sim.api("GET", "/api/state")
            chosen = select_vehicles(p["vehicles"], (state or {}).get("vehicles", []) if status == 200 else [])
            lead = chosen[0]
            req: dict[str, Any] = {"task": p["task"], "uids": [v["uid"] for v in chosen]}
            target: dict[str, Any] = {}
            if p.get("center") is not None:
                req["center"], target = self._center(p["center"], lead, now)
            if p.get("radius_m") is not None:
                req["radius_m"] = float(p["radius_m"])
            if p.get("route") is not None:
                r = p["route"]
                if isinstance(r, dict):
                    start, _ = self._center(r.get("from", {"vehicle": "self"}), lead, now)
                    end, target = self._center(r["to"], lead, now)
                    legs = max(1, int(r.get("legs", 1)))
                    req["route"] = [start] + [[round(start[0] + (end[0] - start[0]) * k / legs, 6),
                                               round(start[1] + (end[1] - start[1]) * k / legs, 6)]
                                              for k in range(1, legs + 1)]
                else:
                    req["route"] = [[float(q[0]), float(q[1])] for q in r]
            for k in ("target", "reserve_pct", "duration_s"):
                if p.get(k) is not None:
                    req[k] = p[k]
        except (StepError, ValueError, KeyError) as e:
            if now + 2.0 <= first + float(p.get("retry_s", 20)):
                self._push(now + 2.0, {**it, "first_t": first, "tries": int(it.get("tries", 0)) + 1})
                return False
            raise ValueError(f"{e} (tried for {now - first:g} s)") from None
        rationale = fill(str(p.get("rationale", "")), target, lead) or f"{p['task']} on {lead.get('uid')}"
        s = HeadlessSession(sim, STORYLINE_AGENT, "0.1")
        out = self.ep.run_async(s.propose(labelled(SCRIPTED_TIMELINE, rationale), **_mcp_args(req)))
        rec.update(intent=out.get("id"), state=out.get("state"), refused=out.get("refused"), request=req,
                   target=target.get("track_id"), tries=int(it.get("tries", 0)) + 1)
        return None

    def _engage(self, now: float, it: dict[str, Any], rec: dict[str, Any]) -> bool | None:
        """An engagement step as eval/engage.py `EngageAdapter.run_step` runs it live (module
        doc), on the simulated clock: False while it waits (its track, or the checks)."""
        from eval.engage import EngageAdapter, failing_checks, standing_nomination

        sim, p = self.ep.sim, it["params"]
        if it.get("proposer", "agent") != "agent":
            rec["error"] = "the operator proposer (op-planner) is not in the headless simulator: [storyline] proposer = agent"
            return None
        first = float(it.get("first_t", now))
        try:
            status, state = sim.api("GET", "/api/state")
            chosen = select_vehicles(p["vehicles"], (state or {}).get("vehicles", []) if status == 200 else [])
            lead = chosen[0]
            _, target = self._center({"track": p["track"]}, lead, now)
        except (StepError, ValueError, KeyError) as e:
            if now + 2.0 <= first + float(p.get("retry_s", 20)):
                self._push(now + 2.0, {**it, "first_t": first, "tries": int(it.get("tries", 0)) + 1})
                return False
            raise ValueError(f"{e} (tried for {now - first:g} s)") from None
        uids = [int(v["uid"]) for v in chosen]
        track = target.get("track_id")
        req = EngageAdapter.request(p, uids, target)
        rec.update(engage=True, track=track, uids=uids)
        if p.get("unless_nominated"):
            status, out = sim.api("GET", "/api/intents")
            there = standing_nomination((out or {}).get("intents", []) if status == 200 else [], track)
            if there is not None:
                src = there.get("source") or {}
                rec.update(skipped=f"a nomination on {track} stands: intent {there.get('id')} by {src.get('kind')} "
                                   f"{src.get('id')} ({there.get('state')})",
                           nomination={"intent_id": there.get("id"), "source": src, "state": there.get("state")})
                return None
        if "wait_s" in p:
            s = HeadlessSession(sim, STORYLINE_AGENT, "0.1")
            x = req["extra"]
            form = self.ep.run_async(s.call("get_nomination_options", track_id=x["track"],
                                            effect_radius_m=x["effect_radius_m"], window_s=x["window_s"],
                                            fallback=x["fallback"], uids=uids))
            want = {str(u) for u in uids}
            failing = failing_checks(next((o for o in form.get("options") or [] if str(o.get("uid")) in want), None))
            if failing:
                since = float(it.get("wait_from", now))
                if now + 3.0 <= since + float(p["wait_s"]):
                    self._push(now + 3.0, {**it, "first_t": now, "wait_from": since, "failing": failing})
                    return False
                callsign = (lead.get("identity") or {}).get("callsign") or uids
                rec.update(skipped=f"{callsign} did not pass the nomination checks within {float(p['wait_s']):g} s: "
                                   f"{'; '.join(failing)}"[:600], failing=failing)
                return None
        rationale = fill(str(p.get("rationale", "")), target, lead) or f"engage {track} with {lead.get('uid')}"
        s = HeadlessSession(sim, STORYLINE_AGENT, "0.1")
        out = self.ep.run_async(s.propose(labelled(SCRIPTED_TIMELINE, rationale), **_mcp_args(req)))
        rec.update(intent=out.get("id"), state=out.get("state"), refused=out.get("refused"), request=req,
                   waited_s=round(now - float(it.get("wait_from", now)), 3))
        return None

    def _center(self, c: Any, lead: dict[str, Any], now: float) -> tuple[list[float], dict[str, Any]]:
        """eval/storyline.py `resolve_center` without truth: a place, a track as the C2 shows it,
        or a vehicle, with `offset_m`."""
        if isinstance(c, list) and len(c) == 2:
            return [float(c[0]), float(c[1])], {}
        if not isinstance(c, dict):
            raise StepError(f"bad center {c!r}")
        off = c.get("offset_m", [0, 0])
        if "track" in c:
            status, body = self.ep.sim.api("GET", "/api/tracks")
            t_now = self.ep.sim_epoch + now
            track = select_track(c["track"], (body or {}).get("tracks", []) if status == 200 else [], {}, t_now)
            if track is None:
                raise StepError(f"no track matches {c['track']!r}")
            return offset(list(track["position"]), off[0], off[1]), target_record(track, None, t_now)
        if "vehicle" in c:
            if c["vehicle"] == "self":
                pos = lead.get("position")
            else:
                status, state = self.ep.sim.api("GET", "/api/state")
                v = next((v for v in (state or {}).get("vehicles", []) if v.get("uid") == int(c["vehicle"])), None)
                pos = (v or {}).get("position")
            if not pos:
                raise StepError(f"vehicle {c['vehicle']} has no position")
            return offset(list(pos), off[0], off[1]), {}
        raise StepError(f"bad center {c!r}")


def fill(template: str, target: dict[str, Any], vehicle: dict[str, Any]) -> str:
    """eval/storyline.py `Storyline.fill`: a rationale template's fields from the perception it
    resolved (never truth)."""
    ident = vehicle.get("identity") or {}
    fields = {
        "track": target.get("track_id", "no track"),
        "kind": target.get("track_kind") or "unclassified",
        "affiliation": target.get("track_affiliation") or "unknown",
        "reason": target.get("track_reason") or "",
        "age": f"{target.get('track_age_s', 0):.0f} s",
        "confidence": f"{(target.get('track_affiliation_confidence') or 0):.2f}",
        "callsign": ident.get("callsign", f"vehicle {vehicle.get('uid')}"),
        "platform": ident.get("platform", "?"),
    }
    try:
        return template.format_map(fields)
    except (KeyError, ValueError) as e:
        raise StepError(f"rationale template: {e}") from e
