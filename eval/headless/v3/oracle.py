"""The basic v3 oracle (agent `oracle-v3-basic`, the first dev check's `oracle-v3`; eval/SCORING-V3.md
section 5): a feasibility check for the v3 families, kept to compare with that check. The ceiling of
the ladder, `oracle-v3`, is planner.py's (C2); its rungs and the honest ceiling's perturbations are
planner.py's and perturb.py's, dispatched by `make` here. MODEL RESULTS ONLY.

**Privileged, counted and recorded** (`agent_detail.privileged`), never given to an agent under test:
it reads the situation's contacts file (every contact's route, speed, depth or altitude and start:
so each need's crossing of the mission circle and its deadline before it happens), our vehicles'
true state (`Sim.truth`), the tracker's association, and the physical sensing model's own
predictions (`sense`, crates/murmur-bridge tracks/physical/predict.rs, asked of the running
episode): which of our platforms can identify (or, for a subsurface need, recognise) each contact,
at what range, under the situation's drawn conditions (visibility, sea state, wind, the sun at the
scenario clock), and whether terrain hides the contact from a chosen spot.

**What it does**, every `TICK_S` from its first tick, for each open need (not yet resolved, deadline
ahead), earliest deadline first:

1. Candidates: every vehicle that takes an orbit and carries the need's domain sensor (EO/IR for a
   surface, air or ground need; sonar for a subsurface one), alive, not landed, its link up. Its
   reach for the need is the model's identification range (recognition for a subsurface need) for
   its platform at its working altitude, a pointed sensor's Pd 0.5, times `REACH_SHARE`.
2. For each candidate, the earliest moment the contact's predicted position (its route at its speed,
   re-anchored on truth every tick) comes within that reach of where the vehicle can be by then
   (straight at cruise from where it truly is, after `DISPATCH_S`), not later than the deadline less
   `MARGIN_S` (the camera's cue, lock and identification dwell; a sonar's 60 s to recognise); the
   orbit centre is the contact's predicted position then, kept only if the model says the vehicle
   would see it from the centre (horizon and terrain).
3. Each need gets the two candidates with the earliest such moments (`WANT`): under water one active
   sonar's recognition never reaches 0.6 alone, and two looks combine; above it, a camera's failed
   cue keeps every camera off that contact for a minute (the payload manager's retry is per contact,
   docs/SENSING.md), so the second look is the ceiling's insurance. A vehicle serves one need at a
   time. An order is sent only when it changes the plan, at most every
   `REPLAN_S` per vehicle; its preview must pass, else the next candidate is tried.

4. A vehicle that met its contact without identifying it follows it (`chase`): a new orbit where the
   contact will be, once it is 400 m from the orbit's centre (no mover's task follows a track).

It answers what happens as it happens (a vehicle that stops answering, lands or loses its link is
dropped from the plan), never before. It does not plan around the link (no link map: a basic oracle)
nor the reserve beyond the checks' own refusals.
"""

from __future__ import annotations

import math
import tomllib
from pathlib import Path
from typing import Any

import numpy as np

from eval.agent import _mcp_args
from eval.world import load_world

from .. import REPO
from ..agents_v2 import ENDED, SessionAgent
from ..situations import at_unix, fleet_plan
from .labels import FAMILY
from .linkmap import link_map
from .mission import rule

M = 111_320.0
CONTACT_ACCEL_MPS2 = 0.5
NEED_AFFILIATIONS = ("unknown", "suspect", "hostile")
RECOVERING = ("RTL", "QRTL", "SMART_RTL", "AUTO_RTL", "LAND", "QLAND", "AUTOLAND", "SURFACE", "DOCK")


def to_xy(origin: tuple[float, float], p: tuple[float, float]) -> tuple[float, float]:
    """(east, north) metres of a point from an origin (lon, lat)."""
    return ((p[0] - origin[0]) * M * math.cos(math.radians(origin[1])), (p[1] - origin[1]) * M)


def to_ll(origin: tuple[float, float], x: float, y: float) -> list[float]:
    return [round(origin[0] + x / (M * math.cos(math.radians(origin[1]))), 6), round(origin[1] + y / M, 6)]


class Path2:
    """A transiting contact's path on the situation's clock in metres east and north of a world's
    centre, as murmur-contacts moves it (speeds up at 0.5 m/s^2 from its start, then holds its
    speed, then holds at the end), re-anchored on truth."""

    def __init__(self, contact: dict[str, Any], origin: tuple[float, float], contacts_start_s: float):
        self.id = str(contact["id"])
        self.origin = origin
        self.xy = np.array([self.to_xy((float(p[0]), float(p[1]))) for p in contact["route"]], dtype=float)
        seg = np.hypot(np.diff(self.xy[:, 0]), np.diff(self.xy[:, 1]))
        self.cum = np.concatenate([[0.0], np.cumsum(seg)])
        self.length = float(self.cum[-1])
        self.speed = float(contact.get("speed_mps") or 0.0)
        self.t0 = float(contacts_start_s) + float(contact.get("start_s") or 0.0)
        self.alt = float(contact.get("alt_m") or 0.0)
        self.shift = 0.0

    def to_xy(self, p: tuple[float, float]) -> tuple[float, float]:
        return to_xy(self.origin, p)

    def to_ll(self, x: float, y: float) -> list[float]:
        return to_ll(self.origin, x, y)

    def along(self, t: np.ndarray) -> np.ndarray:
        T = np.asarray(t, dtype=float) - self.t0
        v, a = self.speed, CONTACT_ACCEL_MPS2
        ramp = v / a if v > 0 else 0.0
        d = np.where(T <= ramp, 0.5 * a * np.maximum(T, 0.0) ** 2, 0.5 * a * ramp ** 2 + v * (T - ramp))
        return np.where(T < 0, np.nan, np.clip(d + self.shift, 0.0, self.length))

    def at(self, t: np.ndarray) -> np.ndarray:
        d = self.along(t)
        x = np.interp(d, self.cum, self.xy[:, 0])
        y = np.interp(d, self.cum, self.xy[:, 1])
        out = np.stack([x, y], axis=-1)
        out[np.isnan(d)] = np.nan
        return out

    def project(self, p: tuple[float, float]) -> float:
        best, best_d = 0.0, float("inf")
        for i in range(len(self.xy) - 1):
            a, b = self.xy[i], self.xy[i + 1]
            ab = b - a
            L = float(ab @ ab)
            u = 0.0 if L == 0 else max(0.0, min(1.0, float((np.array(p) - a) @ ab) / L))
            q = a + u * ab
            dd = math.hypot(p[0] - q[0], p[1] - q[1])
            if dd < best_d:
                best, best_d = float(self.cum[i] + u * math.sqrt(L)), dd
        return best

    def reanchor(self, t: float, p: tuple[float, float]) -> None:
        self.shift = 0.0
        pred = float(self.along(np.array([t]))[0])
        if not math.isnan(pred):
            self.shift = self.project(p) - pred

    def first_within(self, radius_m: float, t_from: float, t_to: float, step: float = 1.0) -> float | None:
        ts = np.arange(t_from, t_to + step, step)
        xy = self.at(ts)
        r = np.hypot(xy[:, 0], xy[:, 1])
        hit = np.nonzero(r <= radius_m)[0]
        return float(ts[hit[0]]) if len(hit) else None


class Need:
    def __init__(self, c: dict[str, Any], path: Path2, t_need: float):
        self.id = path.id
        self.kind = str(c.get("kind"))
        self.domain = str(c.get("domain") or "surface")
        self.path = path
        self.t_need = t_need
        self.rule = rule(self.domain)
        self.deadline = t_need + self.rule.grace_s
        self.resolved_t: float | None = None
        self.assigned: list[int] = []


class Unit:
    def __init__(self, uid: int, callsign: str, platform: str, spec: dict[str, Any], alt: float, cruise: float,
                 orbit_m: float, sensors: list[str], link: str = ""):
        self.uid, self.callsign, self.platform, self.spec, self.link = uid, callsign, platform, spec, link
        self.alt, self.cruise, self.orbit_m, self.sensors = alt, cruise, orbit_m, sensors
        self.xy: tuple[float, float] | None = None
        self.ll: tuple[float, float] | None = None
        self.depth = 0.0
        self.mode = ""
        self.down = False
        self.link_bad = False
        self.order: dict[str, Any] | None = None
        self.flew = False
        self.last_order_t = -1e9
        self.refused_until = 0.0


class OracleV3(SessionAgent):
    """The basic v3 oracle (module doc). Privileged: truth, the contacts file, the sensing model."""

    name = "oracle-v3-basic"
    TICK_S = 2.0
    DISPATCH_S = 8.0
    #: a share of the model's Pd 0.5 range the plan counts on (the model's range is a pointed
    #: sensor's; the camera has still to cue, slew and lock)
    REACH_SHARE = 0.7
    #: what an identification takes once in reach: a camera's cue, lock and dwell; a sonar's 60 s
    #: recognition (and a second for each look to combine)
    MARGIN_S = {"surface": 20.0, "air": 15.0, "ground": 20.0, "subsurface": 75.0}
    REPLAN_S = 20.0
    STEP_S = 2.0
    #: a vehicle's orbit follows its unidentified contact once it is this far from the centre, to
    #: where it will be this much later (`chase`)
    CHASE_M = 400.0
    CHASE_LEAD_S = 20.0
    #: vehicles per need: two everywhere (under water, one active sonar's recognition never reaches
    #: 0.6 alone; above it, a camera's failed cue keeps every camera off that contact for a minute,
    #: docs/SENSING.md "Pointing", so a second look from elsewhere is the ceiling's insurance)
    WANT = {"surface": 2, "air": 2, "ground": 2, "subsurface": 2}
    #: an aircraft that flies this high when tasked (the MQ-9B at 7,600 m, the RQ-4 at 18,000) is
    #: left on its standing order: its one camera is shared by every track in tens of kilometres,
    #: and an orbit takes it to its catalog altitude (docs: SCORING-V3.md 5)
    HIGH_ALT_M = 2000.0
    #: the link classes whose datalink the terrain can block (sim/links.toml: ramp), and the
    #: oracle's tolerance (as agents_v2.Oracle's): every point of the orbit at most 5 m obstructed,
    #: the transit's worst point 15 m and at most 6 of its 30 samples over 8 m
    TERRAIN_LINKS = ("los", "mesh")
    RING_CLEAR_M = 5.0
    TRANSIT_MAX_M = 15.0
    TRANSIT_SOME_M = 8.0
    TRANSIT_SOME_N = 6
    #: orbit radii when a platform names no turn (by its kind), m
    ORBIT_DEFAULT = {"plane": 200.0, "vtol": 200.0, "copter": 60.0, "blimp": 150.0, "boat": 120.0, "sailboat": 150.0,
                     "sub": 80.0, "ugv": 40.0}

    def __init__(self, ep: Any):
        super().__init__(ep)
        self.end = float(ep.e.duration_s)
        self.world = load_world(ep.e.map)
        self.origin = self.world.mission.center
        self.needs: dict[str, Need] = {}
        self.units: dict[int, Unit] = {}
        self.reach: dict[tuple[str, str], float] = {}
        self.started = False
        self.errors = 0
        self.plans: list[dict[str, Any]] = []
        self.privileged = {"truth_reads": 0, "sense_asks": 0, "contacts_file": None,
                           "note": "reads truth (the contacts' routes and states, our vehicles' true state, the "
                                   "tracker's association) and asks the sensing model's predictions: a scoring anchor, "
                                   "never an agent under test"}

    # ---- the sensing model's predictions (privileged) ---------------------------------------------

    def conditions(self) -> dict[str, Any]:
        c = self.ep.e.conditions or {}
        out: dict[str, Any] = {}
        if c.get("visibility_km") is not None:
            out["visibility_m"] = 1000.0 * float(c["visibility_km"])
        for k in ("sea_state", "wind_mps"):
            if c.get(k) is not None:
                out[k] = float(c[k])
        path = self.ep.contacts_path or self.ep.e.contacts
        if path:
            tc = tomllib.loads(Path(path).read_text()).get("conditions") or {}
            if tc.get("shipping") is not None:
                out["shipping"] = float(tc["shipping"])
        return out

    def sense(self, u: Unit, observer_ll: tuple[float, float], obs_alt: float, n: Need, target_ll: tuple[float, float],
              ranges: bool = False, terrain: bool = True) -> list[dict[str, Any]]:
        self.privileged["sense_asks"] += 1
        w = self.world
        req: dict[str, Any] = {
            "op": "sense",
            "observer": {"platform": u.platform, "lat": observer_ll[1], "lon": observer_ll[0], "alt_m": obs_alt},
            "target": {"kind": n.kind, "domain": n.domain, "lat": target_ll[1], "lon": target_ll[0],
                       "alt_m": n.path.alt, "speed_mps": n.path.speed},
            "conditions": self.conditions(), "t": at_unix(self.ep.e.at) + self.clock(), "ranges": ranges,
        }
        if terrain and w.surface_grid is not None:
            req["terrain"] = str(w.surface_grid.relative_to(REPO))
        if w.bathymetry_grid is not None:
            req["bathymetry"] = str(w.bathymetry_grid.relative_to(REPO))
        return list(self.ep.sim.rpc(req).get("predictions") or [])

    def reach_of(self, u: Unit, n: Need) -> float:
        """The unit's range for the need (identification; recognition under water), Pd 0.5, in
        open geometry at its working altitude, times REACH_SHARE; 0 if it cannot."""
        key = (u.platform, n.id)
        if key not in self.reach:
            sensor = n.rule.sensors[0]
            ll = n.path.to_ll(0.0, 0.0)
            preds = self.sense(u, (ll[0], ll[1] - 0.0005), u.alt, n, (ll[0], ll[1]), ranges=True, terrain=False)
            r = 0.0
            for q in preds:
                if q.get("sensor") != sensor:
                    continue
                rm = q.get("ranges_m") or [0.0, 0.0, 0.0]
                level = 2 if n.rule.level == "identify" else 1
                r = max(r, float(rm[level]), float(rm[2]))
            self.reach[key] = self.REACH_SHARE * r
        return self.reach[key]

    def sees_from(self, u: Unit, centre: tuple[float, float], n: Need, t: float) -> bool:
        """Whether the model says the unit at an orbit centre sees the contact's predicted position
        at `t` (horizon and terrain; sonar: in the water and in range)."""
        if n.domain == "subsurface":
            return True
        pos = n.path.at(np.array([t]))[0]
        if np.isnan(pos[0]):
            return False
        c_ll = n.path.to_ll(*centre)
        t_ll = n.path.to_ll(float(pos[0]), float(pos[1]))
        for q in self.sense(u, (c_ll[0], c_ll[1]), u.alt, n, (t_ll[0], t_ll[1])):
            if q.get("sensor") == n.rule.sensors[0]:
                return bool(q.get("line_of_sight"))
        return False

    # ---- setup and reads ------------------------------------------------------------------------------

    def setup(self) -> None:
        ep = self.ep
        path = ep.contacts_path or ep.e.contacts
        self.privileged["contacts_file"] = Path(path).name if path else None
        contacts = list(tomllib.loads(Path(path).read_text()).get("contact") or []) if path else []
        c0 = float(ep.opts.calibration.contacts_start_s)
        r = self.world.mission.radius_m
        for c in contacts:
            if str(c.get("affiliation") or "").lower() not in NEED_AFFILIATIONS or not c.get("route"):
                continue
            p = Path2(c, self.origin, c0)
            t_need = p.first_within(r, p.t0, self.end)
            if t_need is None:
                continue
            n = Need(c, p, t_need)
            self.needs[n.id] = n
            self.write("need", contact=n.id, domain=n.domain, t_need=round(t_need, 1), deadline=round(n.deadline, 1))
        catalog = {str(x["id"]): x for x in self.call("get_platforms").get("platforms") or []}
        _, standins = fleet_plan(ep.e.fleet)
        for s in standins:
            pid = s["platform"]
            spec = s["spec"]
            cat = catalog.get(pid) or {}
            if "orbit" not in (cat.get("tasks") or []):
                continue
            sensors = sorted({x.get("type") for x in spec.get("sensors") or []} & {"eo_ir", "sonar"})
            if not sensors:
                continue
            behavior = str(spec.get("behavior") or "")
            alt = float(s.get("cruise_alt_m") or spec.get("alt_m") or spec.get("typical_alt_m") or 0.0)
            if spec.get("domain") == "air" and float(spec.get("alt_m") or alt) >= self.HIGH_ALT_M:
                continue
            turn = cat.get("min_turn_radius_m")
            orbit = max(1.15 * float(turn), 60.0) if turn else self.ORBIT_DEFAULT.get(behavior, 150.0)
            self.units[int(s["uid"])] = Unit(int(s["uid"]), s["callsign"], pid, spec, alt,
                                             float(spec.get("cruise_mps") or 5.0), math.ceil(orbit / 10.0) * 10.0, sensors,
                                             str(spec.get("link") or ""))
        self.write("units", uids=sorted(self.units))

    def read(self, now: float) -> None:
        truth = self.ep.sim.truth(association=True)
        self.privileged["truth_reads"] += 1
        for o in truth.get("ours") or []:
            u = self.units.get(int(o["uid"]))
            if u is None:
                continue
            u.ll = (float(o["lon"]), float(o["lat"]))
            u.xy = to_xy(self.origin, u.ll)
            u.mode = str(o.get("mode") or "")
            air = u.spec.get("domain") == "air"
            if o.get("armed"):
                u.flew = True
            # an aircraft down again after it flew (a Puma that lands is never relaunched), or
            # returning on its own
            u.down = u.down or (air and u.flew and o.get("armed") is False) or (air and u.mode.upper() in RECOVERING)
            u.link_bad = bool(o.get("link_blocked"))
        contacts = {str(c["id"]): c for c in ((truth.get("contacts") or {}).get("contacts") or [])}
        for n in self.needs.values():
            c = contacts.get(n.id)
            if c and c.get("exists"):
                n.path.reanchor(now, n.path.to_xy((float(c["lon"]), float(c["lat"]))))
        assoc = {str(a["contact_id"]): str(a["track_id"]) for a in truth.get("association") or []}
        tracks = {str(t["id"]): t for t in self.call("get_tracks").get("tracks") or []}
        for n in self.needs.values():
            if n.resolved_t is not None:
                continue
            tr = tracks.get(assoc.get(n.id, ""))
            idn = (tr or {}).get("identity") or {}
            kind = idn.get("kind")
            ok = kind == n.kind or (n.rule.level == "recognise" and kind == FAMILY.get(n.kind))
            sb = set(idn.get("supported_by") or [])
            if tr and ok and not idn.get("conflict") and float(idn.get("confidence") or 0.0) >= 0.6 \
                    and sb & set(n.rule.sensors):
                n.resolved_t = now
                self.write("need.resolved", contact=n.id, t=round(now, 1))
        fleet = {int(v["uid"]): v for v in self.call("get_fleet", compact=True).get("vehicles") or []}
        for u in self.units.values():
            v = fleet.get(u.uid) or {}
            u.link_bad = u.link_bad or bool(v.get("link_lost"))
            if u.order is None:
                continue
            task = v.get("task") if isinstance(v.get("task"), dict) else None
            mine = task is not None and task.get("intent_id") == u.order["intent"]
            if (mine and str(task.get("state")) in ENDED) or (not mine and now - u.order["t"] > 20.0) \
                    or u.mode.upper() in RECOVERING:
                self.write("order.ended", uid=u.uid, intent=u.order["intent"], mode=u.mode, task=task)
                n = self.needs.get(u.order.get("need") or "")
                if n is not None and u.uid in n.assigned:
                    n.assigned.remove(u.uid)
                u.order = None

    # ---- the plan ---------------------------------------------------------------------------------------

    def usable(self, u: Unit, now: float) -> bool:
        return u.xy is not None and not u.down and not u.link_bad and now >= u.refused_until

    def option(self, u: Unit, n: Need, now: float) -> dict[str, Any] | None:
        """The unit's earliest look at the need (module doc, step 2), or None."""
        if not (set(n.rule.sensors) & set(u.sensors)):
            return None
        if n.domain == "subsurface" and u.spec.get("domain") == "air":
            return None
        reach = self.reach_of(u, n)
        if reach <= 0.0:
            return None
        last = n.deadline - self.MARGIN_S[n.domain]
        t_lo = max(now, n.path.t0)
        if last <= t_lo:
            return None
        ts = np.arange(t_lo, last + 1e-9, self.STEP_S)
        pos = n.path.at(ts)
        ok = ~np.isnan(pos[:, 0])
        if not ok.any():
            return None
        assert u.xy is not None
        d = np.hypot(pos[:, 0] - u.xy[0], pos[:, 1] - u.xy[1])
        horiz = math.sqrt(max(reach ** 2 - (u.alt - n.path.alt) ** 2, 0.0)) if n.domain != "subsurface" else reach
        speed = max(u.cruise, 0.5)
        # in reach of the contact's position by then (straight at cruise after the dispatch)
        ready = ok & (now + self.DISPATCH_S + np.maximum(d - horiz, 0.0) / speed <= ts)
        idx = np.nonzero(ready)[0]
        if len(idx) == 0:
            return None
        i = int(idx[0])
        # the orbit centre: on the contact's way, where the vehicle meets it (it can be there by
        # then), so the contact is the nearest track its camera cues to; a sonar listens from reach
        j = i
        if n.domain != "subsurface":
            meet = ok & (now + self.DISPATCH_S + d / speed <= ts)
            mj = np.nonzero(meet)[0]
            j = int(mj[0]) if len(mj) else min(i + int(30.0 / self.STEP_S), len(ts) - 1)
        for jj in range(j, len(ts), max(1, int(6.0 / self.STEP_S))):
            if not ok[jj]:
                continue
            centre = (float(pos[jj, 0]), float(pos[jj, 1]))
            if not self.link_holds(u, centre):
                continue
            if not self.sees_from(u, centre, n, float(ts[jj])):
                continue
            return {"uid": u.uid, "need": n.id, "t": float(max(ts[i], ts[jj] - (ts[j] - ts[i]))),
                    "meet_t": float(ts[jj]), "centre": centre, "reach": round(reach, 1)}
        return None

    def link_holds(self, u: Unit, centre: tuple[float, float]) -> bool:
        """For a line-of-sight or mesh datalink, whether the whole orbit and the straight transit to
        it keep the link (the link map, linkmap.py); any other link: yes."""
        if u.link not in self.TERRAIN_LINKS or u.xy is None:
            return True
        lm = link_map(self.world.name, u.link, u.alt)
        ring = [(centre[0] + u.orbit_m * math.sin(math.radians(a)), centre[1] + u.orbit_m * math.cos(math.radians(a)))
                for a in range(0, 360, 45)] + [centre]
        ll = np.array([to_ll(self.origin, x, y) for x, y in ring])
        if float(np.max(lm.at_ll(ll[:, 0], ll[:, 1]))) > self.RING_CLEAR_M:
            return False
        f = np.linspace(0.0, 1.0, 30)
        line = [to_ll(self.origin, u.xy[0] + (centre[0] - u.xy[0]) * x, u.xy[1] + (centre[1] - u.xy[1]) * x) for x in f]
        la = np.array(line)
        tob = lm.at_ll(la[:, 0], la[:, 1])
        return float(np.max(tob)) <= self.TRANSIT_MAX_M and int(np.sum(tob > self.TRANSIT_SOME_M)) <= self.TRANSIT_SOME_N

    def plan(self, now: float) -> list[dict[str, Any]]:
        out = []
        taken: set[int] = set()
        for u in self.units.values():
            if u.order is not None and u.order.get("need") in self.needs and \
                    self.needs[u.order["need"]].resolved_t is None:
                taken.add(u.uid)
        for n in sorted(self.needs.values(), key=lambda n: (n.deadline, n.id)):
            if n.resolved_t is not None or n.deadline <= now:
                continue
            want = self.WANT.get(n.domain, 1)
            have = [uid for uid in n.assigned if self.units[uid].order is not None]
            if len(have) >= want:
                continue
            opts = []
            for u in self.units.values():
                if u.uid in taken or not self.usable(u, now):
                    continue
                o = self.option(u, n, now)
                if o is not None:
                    opts.append(o)
            opts.sort(key=lambda o: (o["t"], o["uid"]))
            for o in opts[: want - len(have)]:
                taken.add(o["uid"])
                out.append(o)
        return out

    def chase(self, now: float) -> list[dict[str, Any]]:
        """Orders that follow a contact a vehicle met but has not yet identified: when the contact
        is more than `CHASE_M` from the orbit's centre, a new orbit where it will be `CHASE_LEAD_S`
        on (the movers' tasks are orbit, patrol, hold and rtl: none follows a track)."""
        out = []
        for n in self.needs.values():
            if n.resolved_t is not None or n.deadline <= now:
                continue
            here = n.path.at(np.array([now]))[0]
            if np.isnan(here[0]):
                continue
            for uid in list(n.assigned):
                u = self.units[uid]
                if u.order is None or u.xy is None or now - u.last_order_t < self.REPLAN_S:
                    continue
                c = u.order["centre"]
                if math.hypot(here[0] - c[0], here[1] - c[1]) <= self.CHASE_M:
                    continue
                gap = math.hypot(here[0] - u.xy[0], here[1] - u.xy[1])
                ahead = n.path.at(np.array([now + min(self.CHASE_LEAD_S, gap / max(u.cruise, 0.5))]))[0]
                centre = (float(ahead[0]), float(ahead[1]))
                if self.link_holds(u, centre):
                    out.append({"uid": uid, "need": n.id, "t": now, "meet_t": now, "centre": centre,
                                "reach": round(self.reach_of(u, n), 1), "chase": True})
        return out

    def order(self, o: dict[str, Any], now: float) -> bool:
        u = self.units[o["uid"]]
        n = self.needs[o["need"]]
        if now - u.last_order_t < self.REPLAN_S:
            return False
        centre = n.path.to_ll(*o["centre"])
        req = {"task": "orbit", "uids": [u.uid], "center": centre, "radius_m": u.orbit_m}
        try:
            pv = self.call("preview_intent", **_mcp_args(req))
        except Exception as e:  # noqa: BLE001
            self.write("preview.refused", uid=u.uid, request=req, error=str(e)[:200])
            u.refused_until = now + 10.0
            return False
        if not pv.get("passed"):
            bad = [x for x in pv.get("checks") or [] if x.get("outcome") != "pass"]
            self.write("preview.failed", uid=u.uid, request=req, checks=[f"{x.get('rule')} {x.get('outcome')}" for x in bad])
            u.refused_until = now + 10.0
            return False
        text = (f"Orbit {u.callsign} at {centre[1]:.5f}, {centre[0]:.5f} ({u.orbit_m:.0f} m) where a contact to be "
                f"identified will pass (a scoring anchor placed from truth).")
        out = self.propose(text, req, need=n.id, deadline=round(n.deadline, 1), planned_t=round(o["t"], 1),
                           reach_m=o["reach"])
        if out and out.get("id") is not None and not out.get("refused"):
            u.order = {"intent": int(out["id"]), "need": n.id, "t": now, "centre": o["centre"]}
            u.last_order_t = now
            if u.uid not in n.assigned:
                n.assigned.append(u.uid)
            self.plans.append({"t": round(now, 1), "uid": u.uid, "need": n.id, "planned_t": round(o["t"], 1),
                               "deadline": round(n.deadline, 1)})
            return True
        u.refused_until = now + 10.0
        return False

    def wake(self, now: float) -> float | None:
        try:
            if not self.started:
                self.setup()
                self.started = True
            self.read(now)
            for o in self.plan(now) + self.chase(now):
                self.order(o, now)
        except Exception as e:  # noqa: BLE001 (the next wake plans again)
            self.errors += 1
            self.write("error.oracle", error=f"{type(e).__name__}: {str(e)[:300]}")
        return now + self.TICK_S

    def describe(self) -> dict[str, Any]:
        return {**super().describe(), "needs": {k: {"t_need": round(n.t_need, 1), "deadline": round(n.deadline, 1),
                                                    "resolved_t": n.resolved_t, "domain": n.domain}
                                                for k, n in sorted(self.needs.items())},
                "reach_m": {f"{k[0]}:{k[1]}": round(v, 1) for k, v in sorted(self.reach.items())},
                "plans": self.plans, "errors": self.errors}


def make(ep: Any, name: str) -> Any:
    """Scoring v3's agent `name` for episode `ep`: the basic oracle here, the ceiling and its rungs
    (planner.py), the honest ceiling's perturbations (perturb.py)."""
    if name == "oracle-v3-basic":
        return OracleV3(ep)
    from . import perturb, planner

    if name in planner.NAMES:
        return planner.make(ep, name)
    if name in perturb.NAMES:
        return perturb.make(ep, name)
    raise ValueError(f"unknown v3 agent {name!r}")
