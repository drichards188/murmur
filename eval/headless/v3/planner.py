"""Scoring v3's ceiling and its ladder (eval/SCORING-V3.md sections 5 and 13; bd murmur-1z6.7). MODEL
RESULTS ONLY: free, deterministic agents on generated situations.

**`oracle-v3-shown`, the ceiling** (the owner's decision of 2026-10-26, SCORING-V3.md 9): the planner
below (`Ceiling`) with no look at a contact before the picture shows it (the tracker associates a
track with it, `CeilingShown`). **`oracle-v3`, the foreknowledge ceiling**: the same planner looking
from the start, so it can order a vehicle to a contact's route minutes before the contact exists
(a median 170 to 250 s in SCORING-V3.md 15), which no agent can; it is reported (what anticipation is
worth), never judged.

**The planner** (privileged, counted and recorded in `agent_detail.privileged`, never
given to an agent under test). It reads the situation's contacts file (every contact's route, speed,
height and start, so each need's crossing of the mission circle and its deadline before it happens),
our vehicles' true state (position, mode, battery, a blocked link), the tracker's association, the
physical sensing model's own predictions under the situation's drawn conditions (`sense`: the
bridge's functions, asked of the running episode), the terrain, and the per-world link map
(linkmap.py, the emulator's own terrain check). It plans what a perfect watch officer would, given
that cameras point themselves (decision W1: it places vehicles, never gimbals):

1. **Which vehicle can identify each contact, and how well.** For every vehicle that takes an orbit
   and carries the need's domain sensor (EO/IR above the water, sonar under it), the model's chance
   along the line of sight at the vehicle's working height, under the drawn visibility, sea, wind
   and the sun at the need's time (`Quality`: EO/IR's identification, or under water each sonar
   mode's detection, whose seconds of signal a classification integrates). What the plan counts
   on is the evidence the picture needs, a confidence of 0.6: an EO/IR look's confidence is the
   identification chance at the second it identifies (tracks/physical/mod.rs `eo`), a gimballed
   camera identifies once per contact in five minutes (`REVISIT_S`), and it cues to a contact as
   soon as it is within the camera's reach (its catalog `range_m`), so a camera whose reach
   outruns its 0.6 range tends to identify early, at a lower confidence (`Look`: the early
   hazard). Two lower looks combine (`1 - prod(1 - c)`), so the plan values sets of looks
   (`p_resolved`). A sonar look's confidence follows the seconds of signal it hears: active
   `0.6 (1 - e^(-s/60))`, passive `0.8 (1 - e^(-s/120))`, each from 60 s on.
2. **Transit, endurance and reserve, and the link.** Each vehicle flies or sails straight at its
   catalog cruise from where it truly is (a vehicle on the ground launches first); its orbit's
   centre must keep to its medium on the world's land/water grid, its true battery must carry it
   there, through the deadline and home above the reserve (20 % and a margin), and for a
   line-of-sight or mesh datalink the orbit and the transit must keep the link (the link map).
3. **Geometry.** Candidate centres along the contact's predicted path (its route at its speed,
   re-anchored on truth every tick) up to the deadline, and its holding point; for each, the
   relative range over time (`profile`) gives the look's value, and the best centre is kept where
   the model sees the contact from it (horizon and terrain).
4. **Sets of looks.** Needs earliest deadline first; for each, vehicles are added while they raise
   the chance it is resolved by its deadline (`MAX_LOOKS` at most, until `P_TARGET`), a vehicle
   serving one need at a time. An order is previewed first; a refused centre gives way to the next
   vehicle. A vehicle that met its contact without resolving it follows it (`chase`). A vehicle that
   stops answering, lands, loses its link or whose order ended is dropped from the plan, as the
   session or truth shows it, never before.

It knows nothing of the situation's sensing draws (which observer calls the contact wrong): a perfect
watch officer could not. **Feasibility** (`feasibility`, for the report of its losses by cause;
eval/SCORING-V3.md 13.2), recorded per need by the ceiling when the picture first shows the contact
(its first chance to act; `not_shown` when that comes only at or after the deadline) and by the
foreknowledge ceiling at its first tick: with every vehicle of the fleet that takes orders, from
where it is then, whether any can raise the need's evidence to 0.6 by its deadline, and if none can,
why: `fleet_gap` (no vehicle's sensor reaches 0.6 on this kind even by a clear day), `night` or `fog`
(one would by a clear day, not in the drawn light or visibility), `no_time` (capable vehicles, none in
reach of its path by the deadline), `terrain` (in reach, never in sight), `link` (only where the
datalink fails), `reserve`.

**The ladder** (SCORING-V3.md 13.3): the ceiling (`oracle-v3-shown`) with one more advantage removed,
nothing else changed (since 2026-10-26; before, each rung was the foreknowledge ceiling with one
removed):

- `oracle-v3-late`: no look before the need opens either (its true position inside the circle);
- `oracle-v3-linkblind`: no link map and no truth of a blocked link (a lost link only as the
  session's `link_lost` shows it);
- `oracle-v3-sensorblind`: no sensing model: every sensor reaches its catalog `range_m` (what
  `get_platforms` shows as its reach) on every kind, by day or night, in any visibility, and sees
  past any terrain;
- `oracle-v3-honest` (`HonestCeiling`): linkblind, reading no truth: the realistic ceiling, built
  from a session alone. It keeps the sensing model (the catalog's figures make it learnable) with
  the sun from the scenario clock, but not the drawn visibility, sea or wind (the C2 does not show
  them: it assumes a clear day's), nor any contact's truth: its needs come from the picture, so it
  too looks at nothing the picture does not show;
- `oracle-v3-honest-linked` (`HonestLinked`): the honest ceiling with the C2's link predictor
  (`get_link_coverage`) where the link map would be: a diagnostic, not a rung (bd murmur-hei).
"""

from __future__ import annotations

import math
import tomllib
from itertools import product
from pathlib import Path
from typing import Any, Callable

import numpy as np

from eval.agent import _mcp_args
from eval.world import load_world

from .. import REPO
from ..agents_v2 import ENDED, SessionAgent, Straight
from ..situations import at_unix, fleet_plan
from .families import grids
from .labels import FAMILY
from .linkmap import link_map
from .mission import RESOLVE_CONF, rule
from .oracle import NEED_AFFILIATIONS, RECOVERING, Path2, to_ll, to_xy

#: the evidence the picture needs (the mission's 0.6)
P60 = RESOLVE_CONF
#: horizontal distances the model's chance is sampled at, m (log-spaced, 10 m to about 60 km)
DISTANCES = np.array([10.0 * 1.3 ** k for k in range(34)])
#: a sonar classification's confidence: (cap, time constant s) per mode, from 60 s of signal on
#: (tracks/physical/mod.rs `sonar`)
SONAR_ACTIVE = (0.6, 60.0)
SONAR_PASSIVE = (0.8, 120.0)
SONAR_RECOGNISE_S = 60.0
#: the share of planned looks that come off as planned (a camera busy with another contact, a cue
#: that finds nothing in its 8 s, a lock that loses its contact): the plan's insurance, not the
#: model's
P_EXEC = 0.8
#: the clear day the feasibility check compares with (docs/SENSING.md "The decision engine's reaches")
CLEAR_DAY = {"visibility_m": 20000.0, "sun_elevation_deg": 45.0}
#: the reference conditions the honest ceiling assumes: the C2 shows no weather
HONEST_CONDITIONS = {"visibility_m": 20000.0, "sea_state": 2.0, "wind_mps": 5.0}
AIR_DEFAULT_ALT_M = 150.0
M = 111_320.0


# ---- what one vehicle's sensor makes of one contact ----------------------------------------------------

class Quality:
    """One platform's domain sensor against one contact under given conditions: its chance at each
    horizontal distance of `DISTANCES` (EO/IR: identification in its best field, `p`, and detection
    in the wide field a cue or a search holds on it, `search`; sonar: each mode's detection, `active`
    and `passive`), its camera's cue reach (catalog `range_m`) and whether it is gimballed (a camera
    that slews, zooms and cues) or looks all round (or along its heading)."""

    def __init__(self, sensor: str, p: np.ndarray, cue_m: float, gimballed: bool,
                 active: np.ndarray | None = None, passive: np.ndarray | None = None, nominal: bool = False,
                 search: np.ndarray | None = None):
        self.sensor = sensor
        self.p = p
        self.search = search if search is not None else p
        self.cue_m = cue_m
        self.gimballed = gimballed
        self.active = active if active is not None else np.zeros_like(p)
        self.passive = passive if passive is not None else np.zeros_like(p)
        self.nominal = nominal

    def at(self, d: np.ndarray, which: str = "p") -> np.ndarray:
        arr = getattr(self, which)
        return np.interp(np.asarray(d, dtype=float), DISTANCES, arr, left=float(arr[0]), right=0.0)

    def reach(self, level: float = P60, which: str = "p") -> float:
        """The farthest sampled distance at which the chance is at least `level` (0: nowhere)."""
        arr = getattr(self, which)
        ok = np.nonzero(arr >= level)[0]
        return float(DISTANCES[ok[-1]]) if len(ok) else 0.0

    def best(self, which: str = "p") -> float:
        return float(np.max(getattr(self, which)))

    def summary(self) -> dict[str, Any]:
        if self.sensor == "sonar":
            return {"sensor": "sonar", "active_08_m": round(self.reach(0.8, "active")), "passive_08_m":
                    round(self.reach(0.8, "passive")), "nominal": self.nominal}
        return {"sensor": self.sensor, "r60_m": round(self.reach()), "search_50_m": round(self.reach(0.5, "search")),
                "best": round(self.best(), 3), "cue_m": self.cue_m, "gimballed": self.gimballed, "nominal": self.nominal}


def nominal_quality(sensor: str, range_m: float, gimballed: bool) -> Quality:
    """The sensor-blind rung's belief: the catalog's `range_m` reaches every kind in any light."""
    inside = (DISTANCES <= range_m).astype(float)
    if sensor == "sonar":
        return Quality("sonar", inside, range_m, False, active=inside, passive=np.zeros_like(inside), nominal=True)
    return Quality(sensor, inside, range_m, gimballed, nominal=True, search=inside)


# ---- one look's value along a planned encounter -----------------------------------------------------------

class Look:
    """What one vehicle's look at one need is planned to give by the deadline: EO/IR, the chance
    of a confident identification (`high`, at 0.6 or more) and of a lower one (`low`, at
    confidence `low_c`); sonar, the confidence its seconds of signal give (`conf`)."""

    def __init__(self, uid: int, kind: str, high: float = 0.0, low: float = 0.0, low_c: float = 0.0, conf: float = 0.0,
                 t_first: float | None = None):
        self.uid, self.kind = uid, kind
        self.high, self.low, self.low_c, self.conf, self.t_first = high, low, low_c, conf, t_first

    def as_dict(self) -> dict[str, Any]:
        return {"uid": self.uid, "kind": self.kind, "high": round(self.high, 3), "low": round(self.low, 3),
                "low_c": round(self.low_c, 3), "conf": round(self.conf, 3),
                "t_first": None if self.t_first is None else round(self.t_first, 1)}


#: the payload manager's constants (tracks/physical/mod.rs): detection's ACQUIRE time constant, a
#: cue's 8 s to find its contact in the wide field, then 60 s off it (RETRY_S) and 10 s of search
#: (SEARCH_MIN_S) before the next cue; identification's hazard `p^2` a second (LEVEL_TAU_S) after 2 s
#: of the narrow field (IDENTIFY_DWELL_S)
TAU_DETECT_S = 3.4
CUE_S = 8.0
CUE_PERIOD_S = CUE_S + 60.0 + 10.0
IDENTIFY_DWELL_S = 2.0


def identification(p_id: np.ndarray, d_lock: np.ndarray, dt: float, dwell_steps: int) -> np.ndarray:
    """The chance of identifying at each step, given the chance the lock (or, for a camera that
    looks all round, the detection) came at each step: identification accrues at `p^2` a second
    from `dwell_steps` after it (P(id in step t) = (1 - e^(-h(t))) e^(-H(t-1)) times the sum over
    l <= t - dwell of P(lock at l) e^(H(l + dwell - 1)), in log space)."""
    n = len(p_id)
    h = p_id ** 2 * dt
    H = np.cumsum(h)
    shift = np.concatenate([np.zeros(dwell_steps), d_lock[: n - dwell_steps]]) if dwell_steps else d_lock
    # the hazard counts from the step after the dwell: weight each lock by e^(H at its start)
    H0 = np.concatenate([[0.0], H[:-1]])
    with np.errstate(divide="ignore"):
        x = np.log(np.maximum(shift, 1e-300)) + H0
    x = np.where(shift > 0.0, x, -np.inf)
    L = np.logaddexp.accumulate(x)
    # in a step the chance is the hazard's own, 1 - e^(-h), after surviving the steps before it
    with np.errstate(over="ignore", invalid="ignore"):
        out = -np.expm1(-h) * np.exp(np.where(np.isfinite(L), L - (H - h), -np.inf))
    return np.nan_to_num(out, nan=0.0)


def eo_look(uid: int, q: Quality, ts: np.ndarray, r: np.ndarray, dist_centre: np.ndarray, area_m: float,
            exists: np.ndarray, dt: float) -> Look:
    """An EO/IR look over an encounter, as the payload manager runs it (tracks/physical/mod.rs
    `steer`, `eo`): `r` the vehicle's distance to the contact at `ts`, `dist_centre` the contact's
    distance from the orbit's centre.

    A gimballed camera cues to a track within its reach and its task area (the orbit and half its
    reach), holds its wide field on it for 8 s while the detection accrues (`search`'s chance at the
    ACQUIRE rate), and after a cue that found nothing is off the contact for 60 s and searches 10 s;
    once detected it locks in the narrow field and, after 2 s, identifies at the hazard `p^2` a
    second, once: the look's confidence is the identification chance at that second (below 0.6, a
    `low` look). A camera that looks all round has the contact in view whenever it is in sight,
    detects and identifies at the same rates, and keeps reporting as the range closes: its
    confidence is the surest the encounter gives after it identifies."""
    p = q.at(r) * exists
    ps = q.at(r, "search") * exists
    dwell = max(1, int(round(IDENTIFY_DWELL_S / dt)))
    if q.gimballed:
        cue = exists & (r <= q.cue_m) & (dist_centre <= area_m)
        if not cue.any():
            return Look(uid, "eo")
        first = int(np.argmax(cue))
        on = cue & (((ts - ts[first]) % CUE_PERIOD_S) < CUE_S)
    else:
        on = exists & (p + ps > 0.0)
        if not on.any():
            return Look(uid, "eo")
        first = int(np.argmax(on))
    hd = np.where(on, ps ** 2 / TAU_DETECT_S * dt, 0.0)
    got = 1.0 - np.exp(-np.cumsum(hd))
    d_lock = np.diff(np.concatenate([[0.0], got]))
    pid = identification(p, d_lock, dt, dwell)
    if q.gimballed:
        conf = p
    else:
        # the surest from each step to the deadline
        conf = np.maximum.accumulate(p[::-1])[::-1]
    hi_mask = conf >= P60
    high = float(np.sum(pid[hi_mask]))
    low = float(np.sum(pid[~hi_mask]))
    low_c = float(np.sum(pid[~hi_mask] * conf[~hi_mask]) / low) if low > 1e-9 else 0.0
    k = np.nonzero(pid > 1e-6)[0]
    t_first = float(ts[k[0]]) if len(k) else float(ts[first])
    return Look(uid, "eo", high=P_EXEC * min(high, 1.0), low=P_EXEC * min(low, 1.0), low_c=min(low_c, P60 - 1e-3),
                t_first=t_first)


def sonar_look(uid: int, q: Quality, r: np.ndarray, exists: np.ndarray, dt: float) -> Look:
    """A sonar look: its seconds of signal (each mode's detection chance a second) by the deadline,
    and the recognition's confidence they give, the better mode's (the tracker keeps one look per
    observer and sensor: an active sonar's listening mode does not add to it)."""
    out = 0.0
    for arr, (cap, tau) in ((q.active, SONAR_ACTIVE), (q.passive, SONAR_PASSIVE)):
        s = float(np.sum(np.interp(r, DISTANCES, arr, left=float(arr[0]), right=0.0) * exists) * dt)
        if s >= SONAR_RECOGNISE_S:
            out = max(out, cap * (1.0 - math.exp(-s / tau)))
    return Look(uid, "sonar", conf=P_EXEC * out if not q.nominal else out)


def p_resolved(looks: list[Look]) -> float:
    """The chance a set of looks resolves the need: EO/IR, any confident identification or lower
    ones that combine to 0.6; sonar, the looks' confidences combined, judged against 0.6 with a
    margin (0.45 to 0.75, linear: the plan's signal seconds are estimates)."""
    eo = [x for x in looks if x.kind == "eo"]
    so = [x for x in looks if x.kind == "sonar"]
    p = 0.0
    if eo:
        for combo in product(range(3), repeat=len(eo)):
            pr = 1.0
            cs = []
            for x, c in zip(eo, combo):
                if c == 0:
                    pr *= x.high
                    cs.append(1.0)
                elif c == 1:
                    pr *= x.low
                    cs.append(x.low_c)
                else:
                    pr *= max(0.0, 1.0 - x.high - x.low)
                    cs.append(0.0)
            if pr <= 0.0:
                continue
            if max(cs) >= 1.0 or 1.0 - float(np.prod([1.0 - c for c in cs])) >= P60:
                p += pr
    if so:
        comb = 1.0 - float(np.prod([1.0 - x.conf for x in so]))
        ps = float(np.clip((comb - 0.45) / 0.3, 0.0, 1.0))
        p = 1.0 - (1.0 - p) * (1.0 - ps)
    return min(p, 1.0)


def utility(looks: list[Look]) -> float:
    """What the plan maximises when it builds a set of looks: the chance the need is resolved, and a
    tenth of the evidence the looks bring short of it (EO/IR the chance of any identification
    weighted by its confidence; sonar the combined confidence), so a first look that only a second
    could complete (two lower identifications, two active sonars) is still worth sending."""
    eo = [x for x in looks if x.kind == "eo"]
    so = [x for x in looks if x.kind == "sonar"]
    soft = 0.0
    if eo:
        soft = max(soft, 1.0 - float(np.prod([1.0 - min(1.0, x.high + x.low * x.low_c) for x in eo])))
    if so:
        soft = max(soft, 1.0 - float(np.prod([1.0 - x.conf for x in so])))
    return p_resolved(looks) + 0.1 * soft


# ---- needs and units ---------------------------------------------------------------------------------

class NeedV3:
    def __init__(self, nid: str, kind: str, domain: str, path: Any, t_need: float, alt: float, speed: float,
                 opened_t: float | None = None):
        self.id, self.kind, self.domain, self.path = nid, kind, domain, path
        self.alt, self.speed = alt, speed
        self.t_need = t_need
        self.rule = rule(domain)
        self.deadline = t_need + self.rule.grace_s
        self.resolved_t: float | None = None
        self.opened_t = opened_t
        #: when the picture first showed the contact (the shown rung's gate)
        self.shown_t: float | None = None
        self.assigned: list[int] = []
        self.feasibility: dict[str, Any] | None = None
        self.hold: tuple[float, float] | None = None


class UnitV3:
    def __init__(self, uid: int, callsign: str, platform: str, cat: dict[str, Any], alt: float, cruise: float,
                 orbit_m: float, link: str, domain: str):
        self.uid, self.callsign, self.platform, self.cat = uid, callsign, platform, cat
        self.alt, self.cruise, self.orbit_m, self.link, self.domain = alt, cruise, orbit_m, link, domain
        self.sensors: dict[str, dict[str, Any]] = {}
        for i, s in enumerate(cat.get("sensors") or []):
            t = str(s.get("type"))
            if t in ("eo_ir", "sonar") and t not in self.sensors:
                self.sensors[t] = {**s, "_index": i}
        self.endurance_s = 3600.0 * float(cat.get("endurance_h") or 1e3)
        self.xy: tuple[float, float] | None = None
        self.home_xy: tuple[float, float] | None = None
        self.mode = ""
        self.down = False
        self.on_ground = False
        self.link_bad = False
        self.battery: float | None = None
        self.order: dict[str, Any] | None = None
        self.flew = False
        self.last_order_t = -1e9
        self.refused_until = 0.0
        self.home_sent = False

    def gimballed(self) -> bool:
        s = self.sensors.get("eo_ir") or {}
        return float(s.get("slew_dps") or 0.0) > 0.0 and not all(c.get("panoramic") for c in s.get("channels") or [])

    def range_m(self, sensor: str) -> float:
        return float((self.sensors.get(sensor) or {}).get("range_m") or 0.0)


def domain_sensor(domain: str) -> str:
    return "sonar" if domain == "subsurface" else "eo_ir"


def qkey(u: UnitV3, n: NeedV3, key: str = "") -> tuple[str, str]:
    """A quality table's key: the platform and its height, the contact's kind, domain and height
    (to 50 m), and the need's 10-minute bucket of the sun (needs of one kind share a table)."""
    return f"{u.platform}@{u.alt:.0f}", f"{n.kind}:{n.domain}@{round(n.alt / 50.0) * 50:.0f}:{int(n.t_need // 600)}{key}"


# ---- the ceiling --------------------------------------------------------------------------------------

class Ceiling(SessionAgent):
    """The v3 planner (module doc), looking from the start: the foreknowledge ceiling. Privileged:
    truth, the contacts file, the sensing model."""

    name = "oracle-v3"
    #: it reads and plans every so often (its orders go out at once)
    TICK_S = 4.0
    #: an order's preview, proposal and approval (approve-all looks every second), and the task's start
    DISPATCH_S = 8.0
    #: a vehicle on the ground launches and climbs first
    LAUNCH_S = 25.0
    STEP_S = 2.0
    #: centres along the contact's path every so many seconds
    CENTRE_EVERY_S = 8.0
    MAX_CENTRES = 40
    #: the plan is made again this often, or at once when an order ended or a need was resolved
    PLAN_EVERY_S = 8.0
    #: what a look needs before the deadline: EO/IR a cue, a lock and the identification's dwell;
    #: sonar its 60 s of signal (already in its look) and a margin
    MARGIN_S = {"surface": 6.0, "air": 4.0, "ground": 6.0, "subsurface": 10.0}
    REPLAN_S = 20.0
    MAX_LOOKS = 3
    P_TARGET = 0.95
    MIN_GAIN = 0.02
    #: a need is feasible for the plan when its best set of looks resolves it with this chance
    FEASIBLE_P = 0.1
    CHASE_M = 400.0
    CHASE_LEAD_S = 20.0
    #: an aircraft that flies this high when tasked (the MQ-9B at 7,600 m, the RQ-4 at 18,000) is
    #: left on its standing order: its one camera cues to every track within tens of kilometres
    #: (its reach), so where it orbits does not decide what it looks at, and the plan cannot place
    #: its attention (decision W1); its looks happen or not whatever the plan does
    HIGH_ALT_M = 2000.0
    TERRAIN_LINKS = ("los", "mesh")
    RING_CLEAR_M = 5.0
    TRANSIT_MAX_M = 15.0
    TRANSIT_SOME_M = 8.0
    TRANSIT_SOME_N = 6
    ORBIT_DEFAULT = {"plane": 200.0, "vtol": 200.0, "copter": 60.0, "blimp": 150.0, "boat": 120.0, "sailboat": 150.0,
                     "sub": 80.0, "ugv": 40.0}
    RESERVE_PCT = 20.0
    RESERVE_MARGIN_PCT = 3.0
    #: a centre on water must be this deep for a boat, a sub's depth and this for a sub
    BOAT_DEPTH_M = 3.0
    SUB_MARGIN_M = 2.0
    #: the ladder's switches (each rung turns one off)
    KNOW_LINK = True
    KNOW_SENSORS = True
    LATE = False
    #: the ceiling's switch (SCORING-V3.md 13.1; a diagnostic rung in section 15, the ceiling since
    #: 2026-10-26): no look before the picture shows the contact (a track the tracker associates
    #: with it). Off, the planner is the foreknowledge ceiling
    SHOWN = False
    RESERVE_AWARE = True
    TRUTH = True

    def __init__(self, ep: Any):
        super().__init__(ep)
        self.end = float(ep.e.duration_s)
        self.world = load_world(ep.e.map)
        self.world_name = self.world.name
        self.origin = self.world.mission.center
        self.radius = self.world.mission.radius_m
        self.at0 = at_unix(ep.e.at)
        self.needs: dict[str, NeedV3] = {}
        self.units: dict[int, UnitV3] = {}
        self.quality: dict[tuple[str, str], Quality] = {}
        self.started = False
        self.errors = 0
        self.plans: list[dict[str, Any]] = []
        self.cond = self.drawn_conditions()
        self.privileged = {"truth_reads": 0, "sense_asks": 0, "contacts_file": None,
                           "note": "reads truth (the contacts' routes and states, our vehicles' true state, the tracker's "
                                   "association) and asks the sensing model's predictions under the drawn conditions: a "
                                   "scoring anchor, never an agent under test"}

    # ---- the sensing model (privileged: the drawn conditions) --------------------------------------------

    def drawn_conditions(self) -> dict[str, Any]:
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

    def rpc_sense(self, req: dict[str, Any]) -> dict[str, Any]:
        self.privileged["sense_asks"] += 1
        return self.ep.sim.rpc({**req, "op": "sense"})

    def sense(self, platform: str, obs_ll: tuple[float, float], obs_alt: float, n: NeedV3, tgt_ll: tuple[float, float],
              cond: dict[str, Any], t_rel: float, sensor: str | None = None, terrain: bool = False) -> list[dict[str, Any]]:
        req: dict[str, Any] = {
            "observer": {"platform": platform, "lat": obs_ll[1], "lon": obs_ll[0], "alt_m": obs_alt},
            "target": {"kind": n.kind, "domain": n.domain, "lat": tgt_ll[1], "lon": tgt_ll[0], "alt_m": n.alt,
                       "speed_mps": n.speed},
            "conditions": cond, "t": self.at0 + t_rel,
        }
        if sensor:
            req["sensor"] = sensor
        if terrain and self.world.surface_grid is not None:
            req["terrain"] = str(self.world.surface_grid.relative_to(REPO))
        if self.world.bathymetry_grid is not None:
            req["bathymetry"] = str(self.world.bathymetry_grid.relative_to(REPO))
        return list(self.rpc_sense(req).get("predictions") or [])

    def conditions_for(self, n: NeedV3) -> dict[str, Any]:
        return dict(self.cond)

    def quality_of(self, u: UnitV3, n: NeedV3, cond: dict[str, Any] | None = None, key: str = "") -> Quality | None:
        """The unit's domain sensor against the need's contact (module doc, step 1), cached per
        platform and need (and `key`, a variant of the conditions)."""
        sensor = domain_sensor(n.domain)
        if sensor not in u.sensors:
            return None
        if n.domain == "subsurface" and u.domain == "air":
            return None
        k = qkey(u, n, key)
        if k in self.quality:
            return self.quality[k]
        gimballed = u.gimballed() if sensor == "eo_ir" else False
        if not self.KNOW_SENSORS:
            q = nominal_quality(sensor, u.range_m(sensor), gimballed)
            self.quality[k] = q
            return q
        cond = self.conditions_for(n) if cond is None else cond
        ll = to_ll(self.origin, 0.0, 0.0)
        lat_scale = M * math.cos(math.radians(ll[1]))
        p = np.zeros(len(DISTANCES))
        srch = np.zeros(len(DISTANCES))
        act = np.zeros(len(DISTANCES))
        pas = np.zeros(len(DISTANCES))
        zeros = 0
        for i, d in enumerate(DISTANCES):
            tgt = (ll[0] + d / lat_scale, ll[1])
            preds = self.sense(u.platform, (ll[0], ll[1]), u.alt, n, tgt, cond, n.t_need, sensor=sensor)
            if not preds:
                break
            pr = preds[0]
            if sensor == "sonar":
                act[i] = float(pr.get("p_detect") or 0.0) if (pr.get("p_identify") or 0.0) < (pr.get("p_detect") or 0.0) \
                    else 0.0
                pas[i] = float(pr.get("p_identify") or 0.0)
                p[i] = max(act[i], pas[i])
            else:
                p[i] = float(pr.get("p_identify") or 0.0)
                srch[i] = float(pr.get("p_search") or pr.get("p_detect") or 0.0)
            zeros = zeros + 1 if max(p[i], srch[i]) <= 0.0 and i > 3 and max(p[: i].max(), srch[: i].max()) > 0.0 else 0
            if zeros >= 3:
                break
        q = Quality(sensor, p, u.range_m(sensor), gimballed, active=act, passive=pas, search=srch)
        self.quality[k] = q
        return q

    def sees_from(self, u: UnitV3, centre: tuple[float, float], n: NeedV3, t: float) -> bool:
        """Whether the model sees the contact's predicted position at `t` from the orbit's centre
        (horizon and terrain); under water, sonar is not blocked by terrain here."""
        if n.domain == "subsurface" or not self.KNOW_SENSORS:
            return True
        pos = n.path.at(np.array([t]))[0]
        if np.isnan(pos[0]):
            return False
        c_ll = to_ll(self.origin, *centre)
        t_ll = to_ll(self.origin, float(pos[0]), float(pos[1]))
        for q in self.sense(u.platform, (c_ll[0], c_ll[1]), u.alt, n, (t_ll[0], t_ll[1]), self.conditions_for(n), t,
                            sensor=domain_sensor(n.domain), terrain=True):
            return bool(q.get("line_of_sight"))
        return False

    # ---- setup and reads (truth: privileged) ----------------------------------------------------------------

    def catalog(self) -> dict[str, dict[str, Any]]:
        return {str(x["id"]): x for x in self.call("get_platforms").get("platforms") or []}

    def make_unit(self, uid: int, callsign: str, pid: str, cat: dict[str, Any], cruise_alt: float | None) -> UnitV3 | None:
        if "orbit" not in (cat.get("tasks") or []):
            return None
        u_dom = str(cat.get("domain") or "")
        behavior = str(cat.get("behavior") or "")
        alt = float(cruise_alt if cruise_alt is not None else (cat.get("alt_m") or cat.get("typical_alt_m") or 0.0))
        if u_dom == "air" and float(cat.get("alt_m") or 0.0) >= self.HIGH_ALT_M:
            return None
        turn = cat.get("min_turn_radius_m")
        orbit = max(1.15 * float(turn), 60.0) if turn else self.ORBIT_DEFAULT.get(behavior, 150.0)
        u = UnitV3(uid, callsign, pid, cat, alt, float(cat.get("cruise_mps") or 5.0), math.ceil(orbit / 10.0) * 10.0,
                   str(cat.get("link") or ""), u_dom)
        return u if u.sensors else None

    def setup(self) -> None:
        ep = self.ep
        path = ep.contacts_path or ep.e.contacts
        self.privileged["contacts_file"] = Path(path).name if path else None
        contacts = list(tomllib.loads(Path(path).read_text()).get("contact") or []) if path else []
        c0 = float(ep.opts.calibration.contacts_start_s)
        for c in contacts:
            if str(c.get("affiliation") or "").lower() not in NEED_AFFILIATIONS or not c.get("route"):
                continue
            p = Path2(c, self.origin, c0)
            t_need = p.first_within(self.radius, p.t0, self.end)
            if t_need is None:
                continue
            dom = str(c.get("domain") or "surface")
            n = NeedV3(p.id, str(c.get("kind")), dom, p, t_need, float(c.get("alt_m") or 0.0), p.speed)
            n.hold = (float(p.xy[-1][0]), float(p.xy[-1][1]))
            self.needs[n.id] = n
            self.write("need", contact=n.id, domain=n.domain, t_need=round(t_need, 1), deadline=round(n.deadline, 1))
        cat = self.catalog()
        _, standins = fleet_plan(ep.e.fleet)
        for s in standins:
            pid = str(s["platform"])
            u = self.make_unit(int(s["uid"]), str(s["callsign"]), pid, cat.get(pid) or {}, s.get("cruise_alt_m"))
            if u is not None:
                self.units[u.uid] = u
        self.write("units", uids=sorted(self.units))

    def read_units(self, now: float) -> None:
        truth = self.ep.sim.truth(association=True)
        self.privileged["truth_reads"] += 1
        self._truth = truth
        for o in truth.get("ours") or []:
            u = self.units.get(int(o["uid"]))
            if u is None:
                continue
            u.xy = to_xy(self.origin, (float(o["lon"]), float(o["lat"])))
            if u.home_xy is None:
                u.home_xy = u.xy
            u.mode = str(o.get("mode") or "")
            air = u.domain == "air"
            if o.get("armed"):
                u.flew = True
            u.on_ground = air and not o.get("armed")
            u.down = u.down or (air and u.flew and o.get("armed") is False) or (air and u.mode.upper() in RECOVERING)
            cap = float(o.get("battery_capacity_mah") or 0.0)
            if cap > 0:
                u.battery = 100.0 * (1.0 - float(o.get("battery_consumed_mah") or 0.0) / cap)
            u.link_bad = bool(o.get("link_blocked")) if self.KNOW_LINK else False

    def read_needs(self, now: float) -> None:
        truth = self._truth
        contacts = {str(c["id"]): c for c in ((truth.get("contacts") or {}).get("contacts") or [])}
        for n in self.needs.values():
            c = contacts.get(n.id)
            if c and c.get("exists"):
                n.path.reanchor(now, n.path.to_xy((float(c["lon"]), float(c["lat"]))))
                if n.opened_t is None and math.hypot(*n.path.to_xy((float(c["lon"]), float(c["lat"])))) <= self.radius:
                    n.opened_t = now
        assoc = {str(a["contact_id"]): str(a["track_id"]) for a in truth.get("association") or []}
        tracks = {str(t["id"]): t for t in self.call("get_tracks").get("tracks") or []}
        self._tracks = tracks
        for n in self.needs.values():
            if n.shown_t is None and assoc.get(n.id) in tracks:
                n.shown_t = now
            if n.resolved_t is not None:
                continue
            tr = tracks.get(assoc.get(n.id, ""))
            idn = (tr or {}).get("identity") or {}
            kind = idn.get("kind")
            ok = kind == n.kind or (n.rule.level == "recognise" and kind == FAMILY.get(n.kind))
            sb = set(idn.get("supported_by") or [])
            if tr and ok and not idn.get("conflict") and float(idn.get("confidence") or 0.0) >= P60 \
                    and sb & set(n.rule.sensors):
                n.resolved_t = now
                self.write("need.resolved", contact=n.id, t=round(now, 1))

    def read_orders(self, now: float, fleet: dict[int, dict[str, Any]] | None = None) -> None:
        if fleet is None:
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

    def read(self, now: float) -> None:
        self.read_units(now)
        self.read_needs(now)
        self.read_orders(now)

    # ---- the plan -----------------------------------------------------------------------------------------

    def until(self, n: NeedV3) -> float:
        """How long the plan works on an unresolved need: the ceiling knows its true deadline."""
        return n.deadline

    def target(self, n: NeedV3, now: float) -> float:
        """The time the plan's looks must come by."""
        return n.deadline

    def usable(self, u: UnitV3, now: float) -> bool:
        return u.xy is not None and not u.down and not u.link_bad and now >= u.refused_until

    def may_look(self, n: NeedV3, now: float) -> bool:
        """The ceiling's switch: no look before the picture shows the contact; the late rung's:
        none before the need opens either."""
        return (not self.LATE or n.opened_t is not None) and (not self.SHOWN or n.shown_t is not None)

    def medium_ok(self, u: UnitV3, xy: tuple[float, float]) -> bool:
        """The centre keeps to the unit's medium on the world's land/water grid."""
        if u.domain == "air":
            return True
        depth_g, _ = grids(self.world_name)
        ll = to_ll(self.origin, *xy)
        d = depth_g.at((ll[0], ll[1]))
        if d is None:
            return False
        if u.domain == "ground":
            return d <= -0.5
        if u.domain == "subsurface":
            return d >= abs(min(u.alt, 0.0)) + self.SUB_MARGIN_M
        return d >= self.BOAT_DEPTH_M

    def reserve_ok(self, u: UnitV3, centre: tuple[float, float], now: float, until: float) -> bool:
        """The unit's battery takes it to the centre, holds it there to `until`, and home above the
        reserve and a margin, at the catalog's endurance (an even draw)."""
        if not self.RESERVE_AWARE or u.battery is None or u.xy is None:
            return True
        home = u.home_xy or u.xy
        out = math.hypot(centre[0] - u.xy[0], centre[1] - u.xy[1]) / max(u.cruise, 0.5)
        back = math.hypot(home[0] - centre[0], home[1] - centre[1]) / max(u.cruise, 0.5)
        need_s = out + max(0.0, until - now - out) + back + 120.0
        spend = 100.0 * need_s / max(u.endurance_s, 1.0)
        return u.battery - spend >= self.RESERVE_PCT + self.RESERVE_MARGIN_PCT

    def link_holds(self, u: UnitV3, centre: tuple[float, float]) -> bool:
        """For a line-of-sight or mesh datalink, the whole orbit and the straight transit keep the
        link (the link map); any other link, or the link-blind rungs: yes."""
        if not self.KNOW_LINK or u.link not in self.TERRAIN_LINKS or u.xy is None:
            return True
        lm = link_map(self.world_name, u.link, max(u.alt, 0.0))
        ring = [(centre[0] + u.orbit_m * math.sin(math.radians(a)), centre[1] + u.orbit_m * math.cos(math.radians(a)))
                for a in range(0, 360, 45)] + [centre]
        ll = np.array([to_ll(self.origin, x, y) for x, y in ring])
        if float(np.max(lm.at_ll(ll[:, 0], ll[:, 1]))) > self.RING_CLEAR_M:
            return False
        f = np.linspace(0.0, 1.0, 30)
        la = np.array([to_ll(self.origin, u.xy[0] + (centre[0] - u.xy[0]) * x, u.xy[1] + (centre[1] - u.xy[1]) * x)
                       for x in f])
        tob = lm.at_ll(la[:, 0], la[:, 1])
        return float(np.max(tob)) <= self.TRANSIT_MAX_M and int(np.sum(tob > self.TRANSIT_SOME_M)) <= self.TRANSIT_SOME_N

    def profile(self, u: UnitV3, n: NeedV3, centre: tuple[float, float], now: float, start_xy: tuple[float, float] | None,
                ts: np.ndarray, pos: np.ndarray, ordered: bool = False) -> tuple[np.ndarray, np.ndarray]:
        """The vehicle's distance to the contact at `ts` and the contact's from the centre: it leaves
        after the dispatch (and a launch, on the ground; none for an order it already flies), flies
        straight at cruise, then circles."""
        sx, sy = start_xy if start_xy is not None else (u.xy or centre)
        delay = 0.0 if ordered else self.DISPATCH_S + (self.LAUNCH_S if u.on_ground else 0.0)
        leg = math.hypot(centre[0] - sx, centre[1] - sy)
        f = np.clip((ts - now - delay) * max(u.cruise, 0.5) / max(leg, 1e-6), 0.0, 1.0)
        vx = sx + (centre[0] - sx) * f
        vy = sy + (centre[1] - sy) * f
        arrived = f >= 1.0
        dx = pos[:, 0] - vx
        dy = pos[:, 1] - vy
        d = np.hypot(dx, dy)
        d = np.where(arrived, np.sqrt(d ** 2 + u.orbit_m ** 2), d)
        dc = np.hypot(pos[:, 0] - centre[0], pos[:, 1] - centre[1])
        return d, dc

    def look_value(self, u: UnitV3, n: NeedV3, q: Quality, centre: tuple[float, float], now: float,
                   start_xy: tuple[float, float] | None, ts: np.ndarray, pos: np.ndarray, exists: np.ndarray,
                   ordered: bool = False) -> Look:
        d, dc = self.profile(u, n, centre, now, start_xy, ts, pos, ordered)
        if q.sensor == "sonar":
            return sonar_look(u.uid, q, d, exists, self.STEP_S)
        area = u.orbit_m + 0.5 * q.cue_m
        return eo_look(u.uid, q, ts, d, dc, area, exists, self.STEP_S)

    def window(self, n: NeedV3, now: float) -> tuple[np.ndarray, np.ndarray, np.ndarray] | None:
        last = self.target(n, now) - self.MARGIN_S[n.domain]
        t_lo = max(now, getattr(n.path, "t0", now))
        if last <= now:
            return None
        ts = np.arange(now, last + 1e-9, self.STEP_S)
        pos = n.path.at(ts)
        exists = ~np.isnan(pos[:, 0]) & (ts >= t_lo)
        pos = np.where(np.isnan(pos), 0.0, pos)
        return ts, pos, exists

    def centres(self, n: NeedV3, ts: np.ndarray, pos: np.ndarray, exists: np.ndarray) -> list[tuple[float, float]]:
        out = []
        step = max(1, int(self.CENTRE_EVERY_S / self.STEP_S), int(math.ceil(len(ts) / self.MAX_CENTRES)))
        for i in range(0, len(ts), step):
            if exists[i]:
                out.append((float(pos[i, 0]), float(pos[i, 1])))
        if exists.any():
            j = int(np.nonzero(exists)[0][-1])
            out.append((float(pos[j, 0]), float(pos[j, 1])))
        if n.hold is not None:
            out.append(n.hold)
        return list(dict.fromkeys(out))

    def option(self, u: UnitV3, n: NeedV3, now: float, win: Any) -> dict[str, Any] | None:
        """The unit's best look at the need (module doc, steps 2 and 3): the centre with the most
        valuable look among those its medium, reserve and link allow, seen from (horizon, terrain)."""
        if not self.usable(u, now) or u.xy is None:
            return None
        q = self.quality_of(u, n)
        if q is None or q.best("p") <= 0.0:
            return None
        ts, pos, exists = win
        ranked = []
        for c in self.centres(n, ts, pos, exists):
            lk = self.look_value(u, n, q, c, now, u.xy, ts, pos, exists)
            v = utility([lk])
            if v <= 1e-4:
                continue
            ranked.append((-round(v, 4), lk.t_first if lk.t_first is not None else 1e9, c, lk))
        ranked.sort(key=lambda x: (x[0], x[1]))
        tried = 0
        for _, _, c, lk in ranked:
            if tried >= 6:
                break
            if not self.medium_ok(u, c) or not self.link_holds(u, c) or not self.reserve_ok(u, c, now, n.deadline):
                continue
            tried += 1
            t_see = lk.t_first if lk.t_first is not None else float(ts[-1])
            if not self.sees_from(u, c, n, t_see):
                continue
            return {"uid": u.uid, "need": n.id, "centre": c, "look": lk, "t": t_see}
        return None

    def current_look(self, u: UnitV3, n: NeedV3, now: float, win: Any) -> Look:
        q = self.quality_of(u, n)
        if q is None or u.order is None:
            return Look(u.uid, "eo" if n.domain != "subsurface" else "sonar")
        ts, pos, exists = win
        return self.look_value(u, n, q, tuple(u.order["centre"]), now, u.xy, ts, pos, exists, ordered=True)

    def plan(self, now: float) -> list[dict[str, Any]]:
        out = []
        taken: set[int] = set()
        for u in self.units.values():
            if u.order is not None and u.order.get("need") in self.needs and \
                    self.needs[u.order["need"]].resolved_t is None and self.until(self.needs[u.order["need"]]) > now:
                taken.add(u.uid)
        for n in sorted(self.needs.values(), key=lambda n: (n.deadline, n.id)):
            if n.resolved_t is not None or self.until(n) <= now or not self.may_look(n, now):
                continue
            win = self.window(n, now)
            if win is None:
                continue
            have = [self.current_look(self.units[uid], n, now, win) for uid in n.assigned
                    if self.units[uid].order is not None]
            p = p_resolved(have)
            if p >= self.P_TARGET or len(have) >= self.MAX_LOOKS:
                continue
            uv = utility(have)
            opts = []
            fresh = {o["uid"] for o in out}
            for u in self.units.values():
                if u.uid in n.assigned or (u.uid in taken and (u.uid in fresh or not self.free_for(u, n, now))):
                    continue
                o = self.option(u, n, now, win)
                if o is not None:
                    opts.append(o)
            chosen: list[dict[str, Any]] = []
            while opts and len(have) + len(chosen) < self.MAX_LOOKS and p < self.P_TARGET:
                gains = [(round(utility(have + [c["look"] for c in chosen] + [o["look"]]) - uv, 4), -o["t"], -o["uid"], i)
                         for i, o in enumerate(opts)]
                g, _, _, i = max(gains)
                if g < self.MIN_GAIN:
                    break
                o = opts.pop(i)
                chosen.append(o)
                uv += g
                p = p_resolved(have + [c["look"] for c in chosen])
            for o in chosen:
                taken.add(o["uid"])
                o["p_after"] = round(p, 3)
                out.append(o)
        return out

    def free_for(self, u: UnitV3, n: NeedV3, now: float) -> bool:
        """Whether a vehicle working on another need may be taken off it for need `n` (the honest
        ceiling's preemption); the ceiling's vehicles serve one need until it is resolved or over."""
        return False

    def chase(self, now: float) -> list[dict[str, Any]]:
        """Orders that follow a contact a vehicle met but has not resolved: when the contact is more
        than `CHASE_M` from the orbit's centre, a new orbit where it will be (orbit, patrol, hold and
        rtl are the movers' tasks: none follows a track)."""
        out = []
        for n in self.needs.values():
            if n.resolved_t is not None or self.until(n) <= now:
                continue
            here = n.path.at(np.array([now]))[0]
            if np.isnan(here[0]):
                continue
            for uid in list(n.assigned):
                u = self.units[uid]
                if u.order is None or u.xy is None or now - u.last_order_t < self.REPLAN_S:
                    continue
                c = u.order["centre"]
                if math.hypot(here[0] - c[0], here[1] - c[1]) <= max(self.CHASE_M, 0.5 * u.orbit_m):
                    continue
                gap = math.hypot(here[0] - u.xy[0], here[1] - u.xy[1])
                ahead = n.path.at(np.array([now + min(self.CHASE_LEAD_S, gap / max(u.cruise, 0.5))]))[0]
                if np.isnan(ahead[0]):
                    continue
                centre = (float(ahead[0]), float(ahead[1]))
                if self.medium_ok(u, centre) and self.link_holds(u, centre):
                    out.append({"uid": uid, "need": n.id, "centre": centre, "look": None, "t": now, "chase": True})
        return out

    def place(self, u: UnitV3, n: NeedV3, centre: tuple[float, float], now: float) -> tuple[float, float]:
        """Where the order puts the orbit's centre (a perturbation moves it)."""
        return centre

    def order(self, o: dict[str, Any], now: float) -> bool:
        u = self.units[o["uid"]]
        n = self.needs[o["need"]]
        if now - u.last_order_t < self.REPLAN_S and not o.get("force"):
            return False
        centre_xy = self.place(u, n, tuple(o["centre"]), now)
        centre = to_ll(self.origin, *centre_xy)
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
        text = (f"Orbit {u.callsign} at {centre[1]:.5f}, {centre[0]:.5f} ({u.orbit_m:.0f} m) to look at a contact to be "
                f"identified (a scoring anchor).")
        lk = o.get("look")
        out = self.propose(text, req, need=n.id, deadline=round(n.deadline, 1), planned_t=round(float(o["t"]), 1),
                           look=lk.as_dict() if lk is not None else None, p_after=o.get("p_after"), chase=o.get("chase"))
        if out and out.get("id") is not None and not out.get("refused"):
            u.order = {"intent": int(out["id"]), "need": n.id, "t": now, "centre": list(centre_xy), "req": req,
                       "text": text}
            u.last_order_t = now
            if u.uid not in n.assigned:
                n.assigned.append(u.uid)
            self.plans.append({"t": round(now, 1), "uid": u.uid, "platform": u.platform, "need": n.id,
                               "planned_t": round(float(o["t"]), 1), "deadline": round(n.deadline, 1),
                               "look": lk.as_dict() if lk is not None else None, "chase": bool(o.get("chase"))})
            return True
        u.refused_until = now + 10.0
        return False

    def homeward(self, now: float) -> None:
        """A vehicle whose true battery, less its way home at cruise, comes to the reserve and a
        margin is sent home (rtl) and planned for no more."""
        if not self.RESERVE_AWARE:
            return
        for u in self.units.values():
            if u.home_sent or u.battery is None or u.xy is None or u.down or u.domain not in ("air",):
                continue
            home = u.home_xy or u.xy
            back = math.hypot(home[0] - u.xy[0], home[1] - u.xy[1]) / max(u.cruise, 0.5) + 120.0
            if u.battery - 100.0 * back / max(u.endurance_s, 1.0) > self.RESERVE_PCT + 1.0:
                continue
            out = self.propose(f"Send {u.callsign} home: its battery comes to the reserve.", {"task": "rtl", "uids": [u.uid]},
                               reserve=True)
            u.home_sent = True
            u.down = True
            self.write("rtl", uid=u.uid, battery=round(u.battery, 1), ok=bool(out and out.get("id") is not None))

    def after_plan(self, now: float, plan: list[dict[str, Any]]) -> None:
        """A perturbation's hook, after the plan's orders went out."""

    def due(self, now: float) -> bool:
        """Whether to plan again: every `PLAN_EVERY_S`, or at once when an order ended, a need was
        resolved or opened, its contact first shown (the ceiling), or a unit was lost since the
        last plan."""
        state = (tuple(sorted((u.uid, u.order["intent"] if u.order else None, u.down, u.link_bad)
                              for u in self.units.values())),
                 tuple(sorted((n.id, n.resolved_t is None, n.opened_t is None, self.SHOWN and n.shown_t is None)
                              for n in self.needs.values())))
        if state != getattr(self, "_plan_state", None) or now - getattr(self, "_plan_t", -1e9) >= self.PLAN_EVERY_S:
            self._plan_state, self._plan_t = state, now
            return True
        return False

    def wake(self, now: float) -> float | None:
        try:
            if not self.started:
                self.setup()
                self.started = True
            self.read(now)
            self.assess(now)
            self.homeward(now)
            plan = self.plan(now) if self.due(now) else []
            for o in plan + self.chase(now):
                self.order(o, now)
            self._plan_state = None if plan else getattr(self, "_plan_state", None)
            self.after_plan(now, plan)
        except Exception as e:  # noqa: BLE001 (the next wake plans again)
            self.errors += 1
            self.write("error.oracle", error=f"{type(e).__name__}: {str(e)[:300]}")
        return now + self.TICK_S

    # ---- feasibility: why a need could not be served (for the report of the ceiling's losses) -------------

    def is_ceiling(self) -> bool:
        """The ceiling or the foreknowledge ceiling: every switch but `SHOWN` at the ceiling's
        value (a rung or the honest ceiling turns one off)."""
        return self.KNOW_LINK and self.KNOW_SENSORS and not self.LATE and self.RESERVE_AWARE and self.TRUTH

    def assess(self, now: float) -> None:
        """Per need, once, with every unit from where it is (module doc): whether any could raise its
        evidence to 0.6 by the deadline, and if none could, the first reason that applies. The
        ceiling judges a need at the tick the picture first shows its contact (its first chance to
        act), `not_shown` when the deadline comes first; the foreknowledge ceiling every need at its
        first tick. The ceilings only (the rungs record none)."""
        if not self.is_ceiling():
            return
        for n in self.needs.values():
            if n.feasibility is not None:
                continue
            if self.SHOWN and n.shown_t is None:
                if now >= n.deadline:
                    n.feasibility = {"verdict": "not_shown", "shown_t": None}
                    self.write("feasibility", contact=n.id, **n.feasibility)
                continue
            if self.SHOWN and n.shown_t is not None and n.shown_t >= n.deadline:
                n.feasibility = {"verdict": "not_shown", "shown_t": round(n.shown_t, 1)}
            else:
                try:
                    n.feasibility = self.feasibility(n, now)
                except Exception as e:  # noqa: BLE001
                    n.feasibility = {"verdict": "unknown", "error": f"{type(e).__name__}: {str(e)[:200]}"}
                if self.SHOWN:
                    n.feasibility["shown_t"] = round(float(n.shown_t), 1)
            self.write("feasibility", contact=n.id, **{k: v for k, v in n.feasibility.items() if k != "units"})

    def feasibility(self, n: NeedV3, now: float) -> dict[str, Any]:
        win = self.window(n, now)
        units = [u for u in self.units.values() if u.xy is not None and not u.down and domain_sensor(n.domain) in u.sensors
                 and not (n.domain == "subsurface" and u.domain == "air")]
        rows: dict[str, Any] = {}
        capable: list[UnitV3] = []
        clear_capable: list[UnitV3] = []
        timely: list[tuple[UnitV3, Look]] = []
        seen: list[tuple[UnitV3, Look]] = []
        linked: list[tuple[UnitV3, Look]] = []
        reserved: list[tuple[UnitV3, Look]] = []
        for u in units:
            q = self.quality_of(u, n)
            row: dict[str, Any] = {"platform": u.platform, **(q.summary() if q else {})}
            rows[str(u.uid)] = row
            if q is None:
                continue
            reach = q.reach(P60) if q.sensor != "sonar" else max(q.reach(0.5, "active"), q.reach(0.5, "passive"))
            if reach > 0.0:
                capable.append(u)
            else:
                cq = self.quality_of(u, n, cond={**self.cond, **CLEAR_DAY}, key="@clear")
                if cq is not None and (cq.reach(P60) if cq.sensor != "sonar" else cq.reach(0.5, "active")) > 0.0:
                    clear_capable.append(u)
                continue
            if win is None:
                continue
            ts, pos, exists = win
            best = None
            for c in self.centres(n, ts, pos, exists):
                lk = self.look_value(u, n, q, c, now, u.xy, ts, pos, exists)
                v = utility([lk])
                if v > 1e-4 and (best is None or v > best[0]):
                    best = (v, c, lk)
            if best is None:
                continue
            timely.append((u, best[2]))
            row["best_alone"] = round(p_resolved([best[2]]), 3)
            if self.sees_from(u, best[1], n, best[2].t_first if best[2].t_first is not None else float(ts[-1])):
                seen.append((u, best[2]))
                if self.link_holds(u, best[1]) and self.medium_ok(u, best[1]):
                    linked.append((u, best[2]))
                    if self.reserve_ok(u, best[1], now, n.deadline):
                        reserved.append((u, best[2]))
        light = self.light_at(n)
        # the best set of looks each stage leaves (up to MAX_LOOKS, greedily), its chance of a 0.6
        best_p = {k: round(self.best_set([lk for _, lk in v]), 3)
                  for k, v in (("timely", timely), ("seen", seen), ("linked", linked), ("reserved", reserved))}
        if not capable:
            if clear_capable:
                verdict = "fog" if light == "fog" else ("night" if light in ("night", "twilight") else "conditions")
            else:
                verdict = "fleet_gap"
        elif best_p["timely"] < self.FEASIBLE_P:
            verdict = "no_time"
        elif best_p["seen"] < self.FEASIBLE_P:
            verdict = "terrain"
        elif best_p["linked"] < self.FEASIBLE_P:
            verdict = "link"
        elif best_p["reserved"] < self.FEASIBLE_P:
            verdict = "reserve"
        else:
            verdict = "feasible"
        return {"verdict": verdict, "light": light, "capable": sorted({u.platform for u in capable}),
                "clear_day_capable": sorted({u.platform for u in clear_capable}),
                "timely": sorted({u.platform for u, _ in timely}), "seen": sorted({u.platform for u, _ in seen}),
                "best_p": best_p, "units": rows}

    def best_set(self, looks: list[Look]) -> float:
        """The chance the best set of these looks (up to `MAX_LOOKS`, chosen greedily) resolves the
        need."""
        chosen: list[Look] = []
        left = list(looks)
        while left and len(chosen) < self.MAX_LOOKS:
            i = max(range(len(left)), key=lambda k: utility(chosen + [left[k]]))
            chosen.append(left.pop(i))
        return p_resolved(chosen)

    def light_at(self, n: NeedV3) -> str:
        """day, night, twilight or fog at the need's time (the drawn visibility under 2 km is fog;
        the sun's elevation from the model)."""
        vis = self.cond.get("visibility_m")
        if vis is not None and vis < 2000.0:
            return "fog"
        try:
            ll = to_ll(self.origin, 0.0, 0.0)
            pr = self.sense("puma", (ll[0], ll[1]), 100.0, n, (ll[0] + 0.001, ll[1]), self.cond, n.t_need, sensor="eo_ir")
            sun = float(pr[0].get("sun_elevation_deg")) if pr else 0.0
        except Exception:  # noqa: BLE001
            return "unknown"
        return "day" if sun >= 0.0 else ("twilight" if sun >= -6.0 else "night")

    def reach_m(self) -> dict[str, float]:
        """Per platform and need, the model's reach the report's capable types read (report.py
        `capable`): EO/IR's 0.6 identification range, a sonar's 0.5 detection range (the better
        mode's), from the quality tables the plan built."""
        out: dict[str, float] = {}
        for n in self.needs.values():
            for u in self.units.values():
                q = self.quality.get(qkey(u, n))
                if q is None:
                    continue
                r = max(q.reach(0.5, "active"), q.reach(0.5, "passive")) if q.sensor == "sonar" else q.reach(P60)
                out[f"{u.platform}:{n.id}"] = max(out.get(f"{u.platform}:{n.id}", 0.0), round(r, 1))
        return out

    def describe(self) -> dict[str, Any]:
        return {**super().describe(),
                "needs": {k: {"t_need": round(n.t_need, 1), "deadline": round(n.deadline, 1), "resolved_t": n.resolved_t,
                              "domain": n.domain, "feasibility": n.feasibility} for k, n in sorted(self.needs.items())},
                "reach_m": self.reach_m(),
                "quality": {f"{k[0]}:{k[1]}": q.summary() for k, q in sorted(self.quality.items())},
                "plans": self.plans, "errors": self.errors,
                "switches": {"know_link": self.KNOW_LINK, "know_sensors": self.KNOW_SENSORS, "late": self.LATE, "shown": self.SHOWN,
                             "reserve_aware": self.RESERVE_AWARE, "truth": self.TRUTH}}


class CeilingShown(Ceiling):
    """The ceiling (SCORING-V3.md 13.1; the owner's decision of 2026-10-26): the planner with no look
    before the picture shows the contact (the tracker associates a track with it), everything else
    the foreknowledge ceiling's. Section 15 found the foreknowledge ceiling's lead over it mostly
    acting before the contact exists, which no agent can, so this is the reachable ceiling: the
    room test's and relative comparison's. Its step under the foreknowledge ceiling is what acting
    before the picture shows a contact is worth (reported); the honest ceiling's step under it,
    what truth is worth once the picture shows it."""

    name = "oracle-v3-shown"
    SHOWN = True


class CeilingLate(CeilingShown):
    """The ceiling, and no look before the need opens either (its true position inside the
    circle)."""

    name = "oracle-v3-late"
    LATE = True


class CeilingLinkBlind(CeilingShown):
    """The ceiling with no link map and no truth of a blocked link: a lost link only as the session
    shows it."""

    name = "oracle-v3-linkblind"
    KNOW_LINK = False


class CeilingSensorBlind(CeilingShown):
    """The ceiling with no sensing model: every sensor reaches its catalog `range_m` on every kind, in
    any light and visibility, past any terrain (the v3 analogue of not knowing your sensors)."""

    name = "oracle-v3-sensorblind"
    KNOW_SENSORS = False


# ---- the honest ceiling: only what the picture shows -----------------------------------------------------

#: a track's kind for the sensing model when the picture names none: by its detection's size class
#: (the physical model's labels), else by its domain. On the ground the class's smaller member
#: (SCORING-V3.md 15): a car and a truck are both `large_ground`, a person `small_ground`
#: (tracks/physical/signature.rs `size_class`); before, a car was planned as a truck and a person as a car
SIZE_KIND = {"small_surface": "small_fast_boat", "large_surface": "cargo_ship", "small_air": "small_drone",
             "large_air": "light_aircraft", "small_ground": "person", "large_ground": "vehicle", "subsurface": "uuv"}
DOMAIN_KIND = {"surface": "small_fast_boat", "air": "small_drone", "ground": "vehicle", "subsurface": "uuv"}
KIND_DOMAIN = {"small_craft": "surface", "large_vessel": "surface", "aircraft": "air", "small_uas": "air",
               "ground_vehicle": "ground", "person": "ground", "submerged": "subsurface"}
#: the kinds a family names (a recognition's family) stands for, for the model
FAMILY_KIND = {"small_craft": "small_fast_boat", "large_vessel": "cargo_ship", "aircraft": "light_aircraft",
               "small_uas": "small_drone", "ground_vehicle": "vehicle", "person": "diver", "submerged": "uuv"}
CLASSIFYING = ("eo_ir", "sonar")
def at_level(shown: str | None, domain: str) -> bool:
    """Whether a shown identity is at the mission's level for the domain (mission.py): a kind (an
    identification), or under the water a class (a recognition's family) or better. A detection's
    size class (`small_air`) or, above the water, a family (`small_uas`) is not, whatever its
    confidence: the picture's identity names them in the same field, at the detection's or the
    recognition's confidence (SCORING-V3.md 15: before this the honest ceiling took 256 of its 533
    resolutions on one, 18 of its 35 losses on F2, F3 and F6 among them)."""
    if shown in FAMILY:
        return True
    return rule(domain).level == "recognise" and shown in set(FAMILY.values())


#: the speed the honest ceiling takes an unranged aircraft to come in at (a small drone's 25 m/s,
#: the top of F2's 12 to 25: the mission's threat for its 30 s grace, mission.py)
THREAT_MPS = {"air": 25.0}


class HonestCeiling(CeilingLinkBlind):
    """The realistic ceiling (module doc): the link-blind ceiling reading no truth, built from a
    session, the loop's runner and the sensing model's functions alone (`sense_fn`, which forwards
    only `sense` requests): never the episode, the simulator's truth, the contacts file, the drawn
    conditions or the link map. The world's map (its mission circle and terrain) is public."""

    name = "oracle-v3-honest"
    TRUTH = False
    #: its needs are the picture's tracks, so it looks at nothing the picture does not show; the
    #: ceiling's gate, which reads the tracker's association with truth, is not its
    SHOWN = False
    #: a track whose shown motion brings it within the circle this soon is a need to plan for
    HORIZON_S = 300.0
    MAX_PLANNED = 6
    #: an unresolved need is worked on this many graces past its own deadline (`until`)
    LAPSE_GRACES = 2.0
    #: the reserve and one more: the session shows the battery in whole percent
    RESERVE_PCT = 21.0
    #: a vehicle on a need due this much later is free for an earlier one (`free_for`)
    PREEMPT_S = 60.0

    def __init__(self, session: Any, run: Callable[[Any], Any], world: str, at: str, end_s: float,
                 sense_fn: Callable[[dict[str, Any]], dict[str, Any]]):
        # not SessionAgent.__init__: no episode is handed over
        self.session = session
        self._run = run
        self.now = 0.0
        self.lines: list[dict[str, Any]] = []
        self.calls: dict[str, int] = {}
        self.proposals = 0
        self.privileged: dict[str, Any] = {}
        self._sense = sense_fn
        self.end = float(end_s)
        self.world = load_world(world)
        self.world_name = self.world.name
        self.origin = self.world.mission.center
        self.radius = self.world.mission.radius_m
        self.at0 = at_unix(at)
        self.needs: dict[str, NeedV3] = {}
        self.units: dict[int, UnitV3] = {}
        self.quality: dict[tuple[str, str], Quality] = {}
        self.started = False
        self.errors = 0
        self.plans: list[dict[str, Any]] = []
        self.cond = dict(HONEST_CONDITIONS)
        self.shown: dict[str, dict[str, Any]] = {}
        self.shown_before: dict[str, dict[str, Any]] = {}
        self.sense_asks = 0

    def clock(self) -> float:
        return self.now

    def run(self, coro: Any) -> Any:
        return self._run(coro)

    def wake(self, now: float) -> float | None:
        self.now = now
        return super().wake(now)

    def rpc_sense(self, req: dict[str, Any]) -> dict[str, Any]:
        self.sense_asks += 1
        return self._sense(req)

    def setup(self) -> None:
        """Its vehicles from the catalog and the fleet as the session shows them."""
        cat = self.catalog()
        compact = sorted(self.call("get_fleet", compact=True).get("vehicles") or [], key=lambda v: int(v["uid"]))
        full = {int(v["uid"]): v for v in self.call("get_fleet", uids=[int(v["uid"]) for v in compact]).get("vehicles") or []}
        for v in compact:
            pid = str(v.get("platform"))
            f = full.get(int(v["uid"])) or {}
            u = self.make_unit(int(v["uid"]), str(v.get("callsign") or v["uid"]), pid, cat.get(pid) or {},
                               (f.get("platform") or {}).get("cruise_alt_m"))
            if u is None:
                continue
            home = f.get("home") or {}
            if home.get("lon") is not None and home.get("lat") is not None:
                u.home_xy = to_xy(self.origin, (float(home["lon"]), float(home["lat"])))
            self.units[u.uid] = u
        self.write("units", uids=sorted(self.units))

    def read(self, now: float) -> None:
        fleet = {int(v["uid"]): v for v in self.call("get_fleet", compact=True).get("vehicles") or []}
        for u in self.units.values():
            v = fleet.get(u.uid) or {}
            stale = set(v.get("stale") or [])
            if v.get("lon") is not None and v.get("lat") is not None:
                u.xy = to_xy(self.origin, (float(v["lon"]), float(v["lat"])))
            if u.home_xy is None and u.xy is not None:
                u.home_xy = u.xy
            u.mode = str(v.get("mode") or "")
            air = u.domain == "air"
            armed = v.get("armed")
            if armed:
                u.flew = True
            if "autopilot" not in stale and armed is False:
                u.on_ground = air
                if air and u.flew:
                    u.down = True
            elif armed:
                u.on_ground = False
            if air and u.mode.upper() in RECOVERING:
                u.down = True
            if v.get("battery_pct") is not None and "battery" not in stale:
                u.battery = float(v["battery_pct"])
            u.link_bad = bool(v.get("link_lost"))
        self.picture(now, self.call("get_tracks").get("tracks") or [])
        self.read_orders(now, fleet)

    def kind_of(self, tr: dict[str, Any]) -> tuple[str, str]:
        """(kind, domain) the model is asked about for a track: its shown kind; else the size class a
        radar or camera detected (an ESM receiver's family names every emitter of a domain alike,
        `aircraft` for a small drone's datalink); else its shown family's stand-in, its size class, its
        domain's smallest threat. A family counts only when a classifying sensor recognised it (an
        ESM receiver's detection shows a family it never recognised: SCORING-V3.md 15)."""
        idn = tr.get("identity") or {}
        kind = str(idn.get("kind") or "")
        dom = str(tr.get("domain") or KIND_DOMAIN.get(kind) or "surface")
        if kind in FAMILY:
            return kind, dom
        looks = idn.get("looks") or []
        for lk in looks:
            if lk.get("level") == "detect" and lk.get("sensor") in ("radar", "eo_ir", "sonar") \
                    and lk.get("label") in SIZE_KIND:
                return SIZE_KIND[str(lk["label"])], dom
        recognised = not looks or any(lk.get("level") == "recognise" and lk.get("sensor") in CLASSIFYING
                                      and lk.get("label") == kind for lk in looks)
        if kind in FAMILY_KIND and recognised:
            return FAMILY_KIND[kind], dom
        if kind in SIZE_KIND:
            return SIZE_KIND[kind], dom
        return DOMAIN_KIND.get(dom, "small_fast_boat"), dom

    def motion(self, xy: tuple[float, float], v: tuple[float, float], dom: str) -> tuple[float, float]:
        """The velocity the plan carries a track at: its shown one; an aircraft the picture gives none
        (a 2D radar's bearings seldom range one) is taken to come straight in at its domain's threat
        speed (`THREAT_MPS`), as a watch officer would."""
        if dom in THREAT_MPS and math.hypot(*v) < 0.5:
            d = math.hypot(*xy)
            if d > 1.0:
                return (-THREAT_MPS[dom] * xy[0] / d, -THREAT_MPS[dom] * xy[1] / d)
        return v

    def due_in(self, route: Any, now: float) -> float | None:
        """When the track's carried motion brings it within the circle (within `HORIZON_S`)."""
        return route.first_within(self.radius, now, now + self.HORIZON_S)

    def picture(self, now: float, tracks: list[dict[str, Any]]) -> None:
        """Needs from the picture (v2's honest oracle's rule): a track in doubt (shown unknown,
        suspect or hostile, in identity conflict, or under 0.6 confidence) whose shown motion
        (carried at constant velocity) brings it within the circle within `HORIZON_S`, or that is
        inside; due its domain's grace after it is first shown inside, after its predicted crossing
        until then; resolved when its identity shows 0.6 or more from EO/IR or sonar without a
        conflict, at the mission's level (`at_level`: a kind, or under the water a class; which kind
        is true, the picture cannot say)."""
        self.shown_before = self.shown
        self.shown = {str(t["id"]): t for t in tracks}
        for tr in sorted(tracks, key=lambda t: str(t["id"])):
            pos = tr.get("predicted") or tr.get("position")
            if not isinstance(pos, list) or len(pos) != 2 or pos[0] is None or pos[1] is None:
                continue
            tid = str(tr["id"])
            xy = to_xy(self.origin, (float(pos[0]), float(pos[1])))
            vel = tr.get("velocity_mps")
            v = (float(vel[0]), float(vel[1])) if isinstance(vel, list) and len(vel) == 2 and None not in vel else (0.0, 0.0)
            idn = tr.get("identity") or {}
            conf = float(idn.get("confidence") or 0.0)
            classified = bool(set(idn.get("supported_by") or []) & set(CLASSIFYING)) and not idn.get("conflict")
            inside = math.hypot(*xy) <= self.radius
            kind, dom = self.kind_of(tr)
            settled = classified and conf >= P60 and at_level(idn.get("kind"), dom)
            alt = float(tr.get("alt_m") or 0.0)
            if dom == "air" and alt <= 0.0:
                alt = AIR_DEFAULT_ALT_M
            n = self.needs.get(tid)
            if n is None:
                doubt = str(tr.get("affiliation") or "") in NEED_AFFILIATIONS or bool(idn.get("conflict")) or conf < P60
                if settled or not doubt:
                    continue
                route = Straight(tid)
                route.update(xy, v if inside else self.motion(xy, v, dom), now)
                t_need = now if inside else self.due_in(route, now)
                if t_need is None:
                    continue
                n = NeedV3(tid, kind, dom, route, t_need, alt, math.hypot(*route.v), opened_t=now if inside else None)
                self.needs[tid] = n
                self.write("need", track=tid, t_need=round(t_need, 1), deadline=round(n.deadline, 1), inside=inside,
                           shown=tr.get("affiliation"), contact_kind=kind, domain=dom, confidence=round(conf, 3))
            else:
                n.path.update(xy, v if inside else self.motion(xy, v, dom), now)
                n.speed = math.hypot(*n.path.v)
                if n.resolved_t is None and n.opened_t is None:
                    if inside:
                        n.opened_t = now
                        n.t_need, n.deadline = now, now + n.rule.grace_s
                    else:
                        t = self.due_in(n.path, now)
                        if t is not None:
                            n.t_need, n.deadline = t, t + n.rule.grace_s
                if kind != n.kind and kind in FAMILY:
                    n.kind = kind
            if n.resolved_t is None and settled:
                n.resolved_t = now
                self.write("need.resolved", track=tid, t=round(now, 1), shown_kind=idn.get("kind"),
                           confidence=round(conf, 3))

    def until(self, n: NeedV3) -> float:
        """The honest ceiling does not know a need's true deadline (its own is from when the picture
        first showed the contact inside, and a 2D radar's plot of an aircraft can show it inside
        before it is): it keeps working on an unresolved need for `LAPSE_GRACES` graces past its own
        deadline, its looks planned to come by then."""
        return n.deadline + self.LAPSE_GRACES * n.rule.grace_s

    def target(self, n: NeedV3, now: float) -> float:
        return self.until(n)

    def plan(self, now: float) -> list[dict[str, Any]]:
        # weigh the earliest open needs only (the picture can show many in doubt)
        open_ = sorted((n for n in self.needs.values() if n.resolved_t is None and self.until(n) > now),
                       key=lambda n: (n.deadline, n.id))
        keep = {n.id for n in open_[: self.MAX_PLANNED]}
        saved = self.needs
        self.needs = {k: v for k, v in saved.items() if k in keep or v.resolved_t is not None or self.until(v) <= now
                      or any(self.units[u].order is not None for u in v.assigned)}
        try:
            return super().plan(now)
        finally:
            self.needs = saved

    def free_for(self, u: UnitV3, n: NeedV3, now: float) -> bool:
        """Earliest deadline first, with preemption: a vehicle working on a need due at least
        `PREEMPT_S` later than `n` is free for `n` (past its `REPLAN_S` since its last order, so the
        order is not refused). The picture shows many tracks in doubt, most of them traffic
        (SCORING-V3.md 15): without it a vehicle sent to a sailboat due in five minutes stayed off
        a drone due in one."""
        m = self.needs.get(str((u.order or {}).get("need") or ""))
        return (m is not None and m.id != n.id and m.deadline > n.deadline + self.PREEMPT_S
                and now - u.last_order_t >= self.REPLAN_S)

    def order(self, o: dict[str, Any], now: float) -> bool:
        ok = super().order(o, now)
        if ok:
            # a vehicle taken off another need no longer counts as that need's look
            for m in self.needs.values():
                if m.id != o["need"] and o["uid"] in m.assigned:
                    m.assigned.remove(o["uid"])
        return ok

    def assess(self, now: float) -> None:
        return

    def describe(self) -> dict[str, Any]:
        out = super().describe()
        out.update(truth="none: built from a session and the sensing model's functions alone (no episode, truth, "
                         "contacts file, drawn conditions or link map)", horizon_s=self.HORIZON_S,
                   sense_asks=self.sense_asks, assumed_conditions=self.cond)
        return out


class HonestLinked(HonestCeiling):
    """The honest ceiling given the bridge's link predictor (docs/LINK-COVERAGE.md, bd murmur-hei):
    where the ceiling's link map reads the world's terrain, it asks the C2's own `get_link_coverage`
    for the orbit it would order (the vehicle's route from where it is, as `preview_intent` shows
    it) and plans only looks whose predicted link holds (the failsafe's chance on the way and five
    minutes on station at most 10 %). Everything else is the honest ceiling's, reading no truth: a
    diagnostic of what the predictor is worth to it, not a rung of the ladder."""

    name = "oracle-v3-honest-linked"
    KNOW_LINK = True

    def link_holds(self, u: UnitV3, centre: tuple[float, float]) -> bool:
        """A line-of-sight or mesh datalink: the predictor's answer for the orbit from where the
        vehicle is (kept per 100 m of its position and 25 m of the centre); any other link, or no
        answer: yes, as the honest ceiling."""
        if u.link not in self.TERRAIN_LINKS or u.xy is None:
            return True
        cache = self.__dict__.setdefault("_links", {})
        key = (u.uid, round(u.xy[0] / 100.0), round(u.xy[1] / 100.0), round(centre[0] / 25.0), round(centre[1] / 25.0))
        if key not in cache:
            lon, lat = to_ll(self.origin, *centre)
            holds = True
            try:
                ans = self.call("get_link_coverage", uid=u.uid, brief=True,
                                order={"task": "orbit", "center": [round(lon, 6), round(lat, 6)], "radius_m": u.orbit_m})
                v = (ans.get("vehicles") or [{}])[0]
                holds = v.get("holds") is not False
            except Exception as e:  # noqa: BLE001 (no answer: as the honest ceiling)
                self.write("error.link_coverage", uid=u.uid, error=f"{type(e).__name__}: {str(e)[:200]}")
            cache[key] = holds
        return cache[key]


def sense_only(sim: Any) -> Callable[[dict[str, Any]], dict[str, Any]]:
    """A callable that asks the simulator the sensing model's question and nothing else (the
    model's functions on the catalog: no episode state)."""

    def ask(req: dict[str, Any]) -> dict[str, Any]:
        return sim.rpc({**{k: v for k, v in req.items() if k != "op"}, "op": "sense"})

    return ask


LADDER = {c.name: c for c in (Ceiling, CeilingShown, CeilingLate, CeilingLinkBlind, CeilingSensorBlind)}
NAMES = tuple(LADDER) + (HonestCeiling.name, HonestLinked.name)


def make(ep: Any, name: str) -> Any:
    """The ceiling or a rung for episode `ep`. The honest one gets a session, the loop's runner,
    the world's public map and clock start, and the sensing model's functions, never the episode."""
    from ..sim import HeadlessSession
    from ..agents_v2 import ANCHOR_AGENT

    if name in LADDER:
        return LADDER[name](ep)
    if name in (HonestCeiling.name, HonestLinked.name):
        cls = HonestLinked if name == HonestLinked.name else HonestCeiling
        return cls(HeadlessSession(ep.sim, ANCHOR_AGENT, f"scoring-v3-{name}/1"), ep.aloop.run_until_complete,
                   str(load_world(ep.e.map).name), str(ep.e.at), float(ep.e.duration_s), sense_only(ep.sim))
    raise ValueError(f"unknown v3 ceiling {name!r}")
