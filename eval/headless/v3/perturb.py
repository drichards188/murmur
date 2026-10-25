"""The honest v3 ceiling's perturbations (eval/SCORING-V3.md 13.4; bd murmur-1z6.7): `oracle-v3-honest`
with one thing changed, so a known degradation has room to show. MODEL RESULTS ONLY.

Each is built as the honest ceiling is (a session, the loop's runner, the world's public map and
clock start, the sensing model's functions), reads no truth unless it says so (skip-first, which
records what it read), and writes what it changed to its log (`perturbation` lines):

- `oracle-v3-honest+spam`: every look it holds (its own orbit standing for an open need) is proposed
  again, the same request and rationale, every 20 s from the last proposal of it; an accepted copy
  is its look from then on.
- `oracle-v3-honest+behind`: each look's centre moved 1.5 times the vehicle's look range (the model's
  0.6 range for the need's kind, a sonar's 0.8 detection range, else its catalog range) behind the
  contact along its shown course (along the line from the mission's centre through it when slower
  than 0.5 m/s), before the preview; it believes it flies the centre it planned.
- `oracle-v3-honest+skip-first` (privileged): the situation's earliest need (the contacts file:
  the contact of unknown, suspect or hostile truth first to come within the circle) is taken out of
  every track list it reads, by the tracker's association read from truth at each read.
- `oracle-v3-honest+reserve-blind` (harm on purpose, 2.5 harmful a): no rtl ever leaves, and the
  battery takes no vehicle out of its plans (the bridge's reserves check still refuses an order
  that would leave a vehicle under its reserve).
- `oracle-v3-honest+ferries`: a look at every track shown neutral with an AIS identity, once per
  track, by a vehicle with a camera and no order of its own, after every real look; let go (a
  hold) once a classifying sensor settles the track, the track is dropped, or 90 s have passed.
- `oracle-v3-honest+pull-off` (harm on purpose, 2.5 harmful b): the moment any vehicle's domain
  sensor raises its hits on an open need (its track shown inside the circle) not yet resolved,
  that vehicle is re-tasked away (to the station 2.5 or 4 km from it, of 16 bearings, where its own
  model says its look is worth least) and the need is given up.
- `oracle-v3-honest+wrong-sensor` (new to v3): each look the honest ceiling plans goes instead to the
  nearest vehicle that carries no sensor the need's domain is identified by (a sonar-only vehicle,
  REMUS or the ROV, for a contact above the water; a camera-only one for one under it), at the
  same centre, where its medium allows; else the look is dropped.
"""

from __future__ import annotations

import math
import tomllib
from pathlib import Path
from typing import Any

import numpy as np

from eval.agent import _mcp_args

from ..agents_v2 import ANCHOR_AGENT
from .oracle import NEED_AFFILIATIONS, Path2, to_ll
from .planner import CLASSIFYING, P60, HonestCeiling, NeedV3, UnitV3, domain_sensor, p_resolved, sense_only

FERRY_LOOK_S = 90.0


def _hits(tr: dict[str, Any] | None, key: str) -> int:
    """The hits of source `key` ("<uid>:<sensor>") on a track as the session shows it."""
    for s in (tr or {}).get("sources") or []:
        if f"{s.get('uid')}:{s.get('sensor')}" == key:
            return int(s.get("hits") or 0)
    return 0


class Perturbed(HonestCeiling):
    """The honest ceiling with one thing changed (module doc)."""

    name = ""
    PRIVILEGED = False

    def __init__(self, *a: Any, **kw: Any):
        super().__init__(*a, **kw)
        self.changed: list[dict[str, Any]] = []

    def note(self, what: str, **fields: Any) -> None:
        rec = {"t": round(self.now, 3), "perturbation": self.name, "what": what, **fields}
        self.changed.append(rec)
        self.write("perturbation", **{k: v for k, v in rec.items() if k != "t"})

    def describe(self) -> dict[str, Any]:
        out = super().describe()
        out.update(base="oracle-v3-honest", changes=len(self.changed))
        return out


class Spam(Perturbed):
    name = "oracle-v3-honest+spam"
    EVERY_S = 20.0

    def after_plan(self, now: float, plan: list[dict[str, Any]]) -> None:
        for u in sorted(self.units.values(), key=lambda u: u.uid):
            o = u.order
            if o is None or not o.get("req") or now - float(o.get("proposed_t", o["t"])) < self.EVERY_S - 1e-9:
                continue
            n = self.needs.get(str(o.get("need")))
            if n is None or n.resolved_t is not None:
                continue
            req = dict(o["req"])
            out = self.propose(str(o.get("text") or "Orbit again."), req, need=o["need"], again=o["intent"])
            o["proposed_t"] = now
            if out and out.get("id") is not None and not out.get("refused"):
                self.note("re-proposed", uid=u.uid, need=o["need"], of=o["intent"], intent=int(out["id"]))
                o["intent"], o["t"] = int(out["id"]), now
            else:
                self.note("re-proposal refused", uid=u.uid, need=o["need"], of=o["intent"])


class Behind(Perturbed):
    name = "oracle-v3-honest+behind"
    FACTOR = 1.5

    def look_range(self, u: UnitV3, n: NeedV3) -> float:
        q = self.quality_of(u, n)
        if q is not None:
            r = q.reach(0.8, "active") if q.sensor == "sonar" else q.reach(P60)
            if r > 0.0:
                return r
        return u.range_m(domain_sensor(n.domain)) or 1000.0

    def place(self, u: UnitV3, n: NeedV3, centre: tuple[float, float], now: float) -> tuple[float, float]:
        v = getattr(n.path, "v", (0.0, 0.0))
        speed = math.hypot(v[0], v[1])
        if speed >= 0.5:
            e, how = (v[0] / speed, v[1] / speed), "its shown course"
        else:
            d = math.hypot(*centre)
            e, how = ((centre[0] / d, centre[1] / d) if d > 0 else (0.0, 1.0)), "the line from the centre through it"
        r = self.FACTOR * self.look_range(u, n)
        out = (centre[0] - r * e[0], centre[1] - r * e[1])
        self.note("moved behind", uid=u.uid, need=n.id, planned=to_ll(self.origin, *centre),
                  centre=to_ll(self.origin, *out), by_m=round(r), how=how)
        return out


class SkipFirst(Perturbed):
    name = "oracle-v3-honest+skip-first"
    PRIVILEGED = True

    def __init__(self, *a: Any, ep: Any, **kw: Any):
        super().__init__(*a, **kw)
        self.ep = ep
        path = ep.contacts_path or ep.e.contacts
        first, t_first = None, None
        if path:
            c0 = float(ep.opts.calibration.contacts_start_s)
            for c in tomllib.loads(Path(path).read_text()).get("contact") or []:
                if str(c.get("affiliation") or "").lower() not in NEED_AFFILIATIONS or not c.get("route"):
                    continue
                p = Path2(c, self.origin, c0)
                t = p.first_within(self.radius, p.t0, self.end)
                if t is not None and (t_first is None or t < t_first):
                    first, t_first = p.id, t
        self.first = first
        self.privileged = {"earliest_need": first, "t_need": None if t_first is None else round(t_first, 1),
                           "truth_reads": 0, "contacts_file": Path(path).name if path else None,
                           "note": "knows the earliest need from the contacts file and its track from the tracker's "
                                   "association (truth): a perturbation, never an agent under test"}
        self.hidden: set[str] = set()

    def call(self, tool: str, **args: Any) -> dict[str, Any]:
        out = super().call(tool, **args)
        if tool != "get_tracks" or self.first is None or not isinstance(out, dict):
            return out
        truth = self.ep.sim.truth(association=True)
        self.privileged["truth_reads"] += 1
        drop = {str(a["track_id"]) for a in truth.get("association") or [] if str(a["contact_id"]) == self.first}
        kept = [t for t in out.get("tracks") or [] if str(t.get("id")) not in drop]
        for tid in sorted({str(t.get("id")) for t in out.get("tracks") or []} - {str(t.get("id")) for t in kept}):
            if tid not in self.hidden:
                self.hidden.add(tid)
                self.note("hid the earliest need's track", track=tid, contact=self.first)
        return {**out, "tracks": kept, "count": len(kept)}


class ReserveBlind(Perturbed):
    name = "oracle-v3-honest+reserve-blind"
    RESERVE_AWARE = False


class Ferries(Perturbed):
    name = "oracle-v3-honest+ferries"

    def __init__(self, *a: Any, **kw: Any):
        super().__init__(*a, **kw)
        self.looked: set[str] = set()

    def after_plan(self, now: float, plan: list[dict[str, Any]]) -> None:
        for u in sorted(self.units.values(), key=lambda u: u.uid):
            o = u.order
            if o is None or not str(o.get("need") or "").startswith("ferry:"):
                continue
            tid = str(o["need"])[len("ferry:"):]
            tr = self.shown.get(tid)
            idn = (tr or {}).get("identity") or {}
            why = None
            if tr is None:
                why = f"{tid} was dropped"
            elif float(idn.get("confidence") or 0.0) >= P60 and set(idn.get("supported_by") or []) & set(CLASSIFYING):
                why = f"{tid} is classified"
            elif now - float(o["t"]) >= FERRY_LOOK_S:
                why = f"looked at {tid} for {FERRY_LOOK_S:.0f} s"
            if why is None:
                continue
            self.propose(f"Hold {u.callsign}: {why}.", {"task": "hold", "uids": [u.uid]}, ferry=tid)
            self.note("let a ferry look go", uid=u.uid, track=tid, why=why)
            u.order = None
        planned = {o["uid"] for o in plan}
        for tid, tr in sorted(self.shown.items()):
            idn = tr.get("identity") or {}
            if tid in self.looked or str(tr.get("affiliation") or "") != "neutral" or not idn.get("ais") \
                    or str(tr.get("state") or "active") == "stale":
                continue
            pos = tr.get("predicted") or tr.get("position")
            if not isinstance(pos, list) or len(pos) != 2 or None in pos:
                continue
            free = [u for u in sorted(self.units.values(), key=lambda u: u.uid)
                    if u.order is None and u.uid not in planned and self.usable(u, now) and "eo_ir" in u.sensors]
            if not free:
                return
            at = (float(pos[0]), float(pos[1]))
            from .oracle import to_xy
            xy = to_xy(self.origin, at)
            u = min(free, key=lambda u: math.hypot(xy[0] - u.xy[0], xy[1] - u.xy[1]) if u.xy else math.inf)
            if not self.medium_ok(u, xy):
                self.looked.add(tid)
                continue
            req = {"task": "orbit", "uids": [u.uid], "center": [round(at[0], 6), round(at[1], 6)], "radius_m": u.orbit_m}
            try:
                pv = self.call("preview_intent", **_mcp_args(req))
            except Exception:  # noqa: BLE001
                continue
            if not pv.get("passed"):
                continue
            text = f"Orbit {u.callsign} over {tid} ({u.orbit_m:.0f} m) for a look at a vessel shown neutral with AIS."
            out = self.propose(text, req, ferry=tid)
            self.looked.add(tid)
            if out and out.get("id") is not None and not out.get("refused"):
                u.order = {"intent": int(out["id"]), "centre": list(xy), "need": f"ferry:{tid}", "t": now, "req": req,
                           "text": text}
                u.last_order_t = now
                self.note("look at a neutral with AIS", uid=u.uid, track=tid, intent=int(out["id"]))


class PullOff(Perturbed):
    name = "oracle-v3-honest+pull-off"
    AWAY_M = (2500.0, 4000.0)
    AWAY_S = 120.0

    def __init__(self, *a: Any, **kw: Any):
        super().__init__(*a, **kw)
        self.abandoned: set[str] = set()

    def may_look(self, n: NeedV3, now: float) -> bool:
        return n.id not in self.abandoned and super().may_look(n, now)

    def raising(self, u: UnitV3) -> tuple[NeedV3, int, int] | None:
        """The first open, unresolved need (by deadline) shown inside the circle whose track's hits
        from this vehicle's domain sensor rose since the last read."""
        for n in sorted(self.needs.values(), key=lambda n: (n.deadline, n.id)):
            if n.id in self.abandoned or n.resolved_t is not None or n.opened_t is None:
                continue
            key = f"{u.uid}:{domain_sensor(n.domain)}"
            before, after = _hits(self.shown_before.get(n.id), key), _hits(self.shown.get(n.id), key)
            if after > before:
                return n, before, after
        return None

    def after_plan(self, now: float, plan: list[dict[str, Any]]) -> None:
        for u in sorted(self.units.values(), key=lambda u: u.uid):
            hit = self.raising(u) if self.usable(u, now) else None
            if hit is None:
                continue
            n, before, after = hit
            self.abandoned.add(n.id)
            if u.uid in n.assigned:
                n.assigned.remove(u.uid)
            sent = self.station(u, n, now)
            self.note("pulled off", uid=u.uid, need=n.id, hits=[before, after], to=sent)

    def station(self, u: UnitV3, n: NeedV3, now: float) -> str | None:
        """An orbit where the vehicle's look at the contact is worth least: of the points `AWAY_M`
        from it at 16 bearings, by its own model's look value over the next `AWAY_S` (the contact
        carried along its shown motion); the least whose medium allows and whose preview passes."""
        if u.xy is None:
            return None
        q = self.quality_of(u, n)
        ts = np.arange(now, now + self.AWAY_S + 1e-9, self.STEP_S)
        pos = n.path.at(ts)
        exists = np.ones(len(ts), dtype=bool)
        cands = []
        for radius in self.AWAY_M:
            for k in range(16):
                b = k * math.pi / 8.0
                at = (u.xy[0] + radius * math.sin(b), u.xy[1] + radius * math.cos(b))
                v = 0.0
                if q is not None:
                    lk = self.look_value(u, n, q, at, now, u.xy, ts, pos, exists)
                    v = p_resolved([lk]) + lk.low + lk.conf
                cands.append((round(v, 4), radius, k, at))
        cands.sort()
        for _, radius, k, at in cands[:10]:
            if not self.medium_ok(u, at):
                continue
            req = {"task": "orbit", "uids": [u.uid], "center": to_ll(self.origin, *at), "radius_m": u.orbit_m}
            try:
                pv = self.call("preview_intent", **_mcp_args(req))
            except Exception:  # noqa: BLE001
                continue
            if not pv.get("passed"):
                continue
            out = self.propose(f"Orbit {u.callsign} at a station away from {n.id}.", req, away_from=n.id)
            if out and out.get("id") is not None and not out.get("refused"):
                u.order = {"intent": int(out["id"]), "centre": list(at), "need": f"away:{n.id}", "t": now, "req": req,
                           "text": "away"}
                u.last_order_t = now
                return f"a station {radius:.0f} m away ({k * 22.5:.0f} deg)"
        return None


class WrongSensor(Perturbed):
    name = "oracle-v3-honest+wrong-sensor"

    def __init__(self, *a: Any, **kw: Any):
        super().__init__(*a, **kw)
        self.dropped: set[tuple[str, int]] = set()

    def wrong(self, u: UnitV3, n: NeedV3) -> bool:
        """The unit carries no sensor the need's domain is identified by."""
        return domain_sensor(n.domain) not in u.sensors

    def plan(self, now: float) -> list[dict[str, Any]]:
        out = []
        taken = {u.uid for u in self.units.values() if u.order is not None}
        for o in super().plan(now):
            n = self.needs[o["need"]]
            cands = [u for u in self.units.values() if u.uid not in taken and self.usable(u, now) and self.wrong(u, n)
                     and self.medium_ok(u, tuple(o["centre"]))]
            if not cands:
                key = (n.id, o["uid"])
                dropped = self.__dict__.setdefault("dropped", set())
                if key not in dropped:
                    dropped.add(key)
                    self.note("dropped a look: no vehicle with the wrong sensor can go there", need=n.id,
                              instead_of=o["uid"])
                continue
            c = tuple(o["centre"])
            w = min(cands, key=lambda u: (math.hypot(c[0] - u.xy[0], c[1] - u.xy[1]), u.uid))
            taken.add(w.uid)
            self.note("sent the wrong sensor", need=n.id, uid=w.uid, platform=w.platform, instead_of=o["uid"])
            out.append({**o, "uid": w.uid, "look": None})
        return out


PERTURBED = {c.name: c for c in (Spam, Behind, SkipFirst, ReserveBlind, Ferries, PullOff, WrongSensor)}
NAMES = tuple(PERTURBED)


def make(ep: Any, name: str) -> Any:
    """A perturbation for episode `ep`, built as the honest ceiling is; only the privileged one
    (skip-first) is handed the episode, for the truth it says it reads."""
    from eval.world import load_world

    from ..sim import HeadlessSession

    cls = PERTURBED[name]
    args = (HeadlessSession(ep.sim, ANCHOR_AGENT, f"scoring-v3-{name}/1"), ep.aloop.run_until_complete,
            str(load_world(ep.e.map).name), str(ep.e.at), float(ep.e.duration_s), sense_only(ep.sim))
    if cls.PRIVILEGED:
        return cls(*args, ep=ep)
    return cls(*args)
