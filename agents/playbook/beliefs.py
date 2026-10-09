"""Beliefs: pure functions of one tick's reads, the config and the ledger.

Nothing here reads the clock, the network or a file, and nothing depends on set or dict
iteration order: every collection is built sorted (vehicles by uid, tracks by id, areas and
protected points in the config's order). What each belief means:

- **Contacts of interest** (`interest`): per track, whether it is inside a watch area and its
  closest approach to each protected point (CPA, TCPA) along its estimated velocity; the plays
  decide what qualifies.
- **CPA and TCPA** (`cpa`): the track's position carried to now (`predicted`) and velocity,
  against a fixed point, in a local plane. A contact slower than `holding_mps` (or opening)
  has no closest approach ahead: its distance now counts, and its TCPA is None.
- **Coverage** (`seen_by`): which of our vehicles' sensors have detected the track, and how long
  ago, from its `sources`.
- **Vehicle health** (`health`): link up, navigation ready on GNSS with no GPS glitch flagged,
  position fresh, battery known. **Navigation suspect** (`nav_suspect`): navigation not ready,
  off GNSS, or the GPS glitch flagged, now or within `nav_clear_s` (the compact fleet read has
  no satellite count, so fewer than six satellites is not seen).
- **Free** (`free_reason`; None when free): a vehicle is free for a task when its health is
  good, its battery is at least the low-reserve line plus `free_battery_margin_pct`, no card or
  order of ours claims it, no engagement is in play on it, it is not returning (mode RTL, LAND,
  QRTL or SMART_RTL), and it is idle: no current order, or one that is not flying (failed,
  superseded, back in the queue). An order
  that is running (an orbit or a patrol reports `done` once established, and keeps flying) is
  never taken: a vehicle on someone's order is that someone's.
- **Inbox pressure** (`pressure`): pending cards in the inbox, own pending cards, alerts.
- **Engagements in play** (`engaging`): vehicles with an engagement pending in the inbox or, as
  the change feed reported it, approved or under way.
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from typing import Any

from .config import Config
from .reads import Reads

RETURNING_MODES = ("RTL", "LAND", "QRTL", "SMART_RTL", "AUTO_RTL")
# a vehicle's current order in one of these task-layer states is not flying (it failed, another
# order replaced it, or it went back to the queue when the link dropped before it started); an
# orbit or a patrol reports `done` once established and keeps flying, so `done` is not here
NOT_RUNNING = ("failed", "superseded", "requeued")
# a hold of ours that parks a vehicle (letting it go, or stopping a stuck one) leaves it free:
# it carries no mission of ours
IDLE_PLAYS = ("isr.release", "safety.stuck")
IMAGING = ("eo_ir",)
OPEN_STATES = ("proposed", "checked", "auto_approved", "pending_approval", "approved", "queued", "dispatched")
ENGAGE_IN_PLAY = ("proposed", "checked", "auto_approved", "pending_approval", "approved", "dispatched")
PROTECTED = ("neutral", "assumed_friend")
EARTH_R = 6371000.0
M_PER_DEG = 111_320.0  # the bridge's own figure for carrying a track along its velocity


# ---- geometry ----------------------------------------------------------------------------------


def haversine_m(a: list[float] | tuple[float, float], b: list[float] | tuple[float, float]) -> float:
    p1, p2 = math.radians(a[1]), math.radians(b[1])
    x = math.sin((p2 - p1) / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(math.radians(b[0] - a[0]) / 2) ** 2
    return 2 * EARTH_R * math.asin(math.sqrt(min(1.0, x)))


def enu(p: list[float] | tuple[float, float], ref: list[float] | tuple[float, float]) -> tuple[float, float]:
    """`p` in metres east and north of `ref` (a local plane: kilometres, not continents)."""
    return ((p[0] - ref[0]) * M_PER_DEG * math.cos(math.radians(ref[1])), (p[1] - ref[1]) * M_PER_DEG)


def in_polygon(p: list[float] | tuple[float, float], ring: list[list[float]]) -> bool:
    x, y = p
    inside = False
    n = len(ring)
    for i in range(n):
        x1, y1 = ring[i]
        x2, y2 = ring[(i + 1) % n]
        if (y1 > y) != (y2 > y) and x < (x2 - x1) * (y - y1) / (y2 - y1) + x1:
            inside = not inside
    return inside


@dataclass(frozen=True)
class Approach:
    """A track's closest approach to a protected point."""

    name: str
    point: tuple[float, float]
    distance_m: float
    cpa_m: float
    tcpa_s: float | None  # None: holding or opening (no closest approach ahead)


def cpa(pos: list[float], vel: list[float] | None, point: list[float], holding_mps: float) -> tuple[float, float, float | None]:
    """(distance now, CPA, TCPA) of a contact at `pos` with velocity `vel` (east, north m/s)
    against a fixed `point`."""
    x, y = enu(pos, point)
    d = math.hypot(x, y)
    if not vel or len(vel) != 2:
        return d, d, None
    vx, vy = float(vel[0]), float(vel[1])
    v2 = vx * vx + vy * vy
    if v2 < holding_mps * holding_mps:
        return d, d, None
    t = -(x * vx + y * vy) / v2
    if t <= 0:
        return d, d, None
    return d, math.hypot(x + vx * t, y + vy * t), t


# ---- the world as the engine holds it -----------------------------------------------------------


@dataclass(frozen=True)
class Vehicle:
    uid: int
    callsign: str
    platform: str
    domain: str | None
    armed: bool | None
    mode: str | None
    lon: float | None
    lat: float | None
    position_age_s: float | None
    nav_ready: bool | None
    nav_source: str | None
    gps_glitch: bool | None
    battery_pct: float | None
    link_lost: bool
    link_age_s: float | None
    task: dict[str, Any] | None
    stale: tuple[str, ...]
    cruise_mps: float | None
    min_turn_m: float | None
    imaging_range_m: float | None
    tasks: tuple[str, ...]

    @property
    def pos(self) -> tuple[float, float] | None:
        return None if self.lon is None or self.lat is None else (self.lon, self.lat)

    @property
    def task_intent(self) -> int | None:
        t = self.task or {}
        try:
            return int(t["intent_id"]) if t.get("intent_id") is not None else None
        except (TypeError, ValueError):
            return None


@dataclass(frozen=True)
class Track:
    id: str
    domain: str | None
    affiliation: str
    aff_conf: float
    aff_reason: str
    kind: str | None
    kind_conf: float
    ais_name: str | None
    conflict: str | None
    position: tuple[float, float] | None
    predicted: tuple[float, float] | None
    velocity: tuple[float, float] | None
    speed: float | None
    course: float | None
    age_s: float
    stale: bool
    existence: float
    sigma_now: float | None
    declared: dict[str, Any] | None
    sources: tuple[tuple[int, str, float], ...]  # (uid, sensor, last_t)
    review: tuple[str, ...]

    @property
    def where(self) -> tuple[float, float] | None:
        return self.predicted or self.position

    @property
    def declared_affiliation(self) -> str | None:
        d = self.declared
        return d.get("affiliation") if isinstance(d, dict) else (d if isinstance(d, str) else None)

    @property
    def shown(self) -> str:
        """The affiliation the C2 shows: the declaration when there is one."""
        return self.declared_affiliation or self.affiliation


def _f(v: Any) -> float | None:
    return float(v) if isinstance(v, (int, float)) and not isinstance(v, bool) else None


def catalog(platforms: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {str(p.get("id")): p for p in (platforms.get("platforms") or []) if p.get("id") is not None}


def vehicles_of(reads: Reads) -> dict[int, Vehicle]:
    cat = catalog(reads.platforms)
    out: dict[int, Vehicle] = {}
    for v in sorted(reads.fleet.get("vehicles") or [], key=lambda v: int(v["uid"])):
        p = cat.get(str(v.get("platform"))) or {}
        imaging = [float(s.get("range_m") or 0) for s in p.get("sensors") or [] if s.get("type") in IMAGING]
        uid = int(v["uid"])
        out[uid] = Vehicle(
            uid=uid, callsign=str(v.get("callsign") or f"uid {uid}"), platform=str(v.get("platform") or "?"),
            domain=v.get("domain"), armed=v.get("armed"), mode=v.get("mode"),
            lon=_f(v.get("lon")), lat=_f(v.get("lat")), position_age_s=_f(v.get("position_age_s")),
            nav_ready=v.get("nav_ready"), nav_source=v.get("nav_source"), gps_glitch=v.get("gps_glitch"),
            battery_pct=_f(v.get("battery_pct")), link_lost=bool(v.get("link_lost")), link_age_s=_f(v.get("link_age_s")),
            task=v.get("task") if isinstance(v.get("task"), dict) else None,
            stale=tuple(sorted(str(s) for s in v.get("stale") or [])),
            cruise_mps=_f(p.get("cruise_mps")), min_turn_m=_f(p.get("min_turn_radius_m")),
            imaging_range_m=max(imaging) if imaging else None,
            tasks=tuple(sorted(str(t) for t in p.get("tasks") or [])),
        )
    return out


def tracks_of(reads: Reads) -> dict[str, Track]:
    out: dict[str, Track] = {}
    for t in sorted(reads.tracks.get("tracks") or [], key=lambda t: str(t["id"])):
        ident = t.get("identity") or {}
        ais = ident.get("ais") if isinstance(ident.get("ais"), dict) else None
        vel = t.get("velocity_mps")
        out[str(t["id"])] = Track(
            id=str(t["id"]), domain=t.get("domain"), affiliation=str(t.get("affiliation") or "unknown"),
            aff_conf=_f(t.get("affiliation_confidence")) or 0.0, aff_reason=str(t.get("affiliation_reason") or ""),
            kind=ident.get("kind"), kind_conf=_f(ident.get("confidence")) or 0.0,
            ais_name=(str(ais.get("name")) if ais and ais.get("name") else None),
            conflict=(str(ident["conflict"]) if ident.get("conflict") else None),
            position=tuple(t["position"]) if isinstance(t.get("position"), list) and len(t["position"]) == 2 else None,
            predicted=tuple(t["predicted"]) if isinstance(t.get("predicted"), list) and len(t["predicted"]) == 2 else None,
            velocity=tuple(vel) if isinstance(vel, list) and len(vel) == 2 else None,
            speed=_f(t.get("speed_mps")), course=_f(t.get("course_deg")),
            age_s=_f(t.get("age_s")) or 0.0, stale=bool(t.get("stale")),
            existence=_f(t.get("confidence_now")) or 0.0, sigma_now=_f(t.get("pos_sigma_now_m")),
            declared=t.get("declared") if isinstance(t.get("declared"), dict) else None,
            sources=tuple(sorted(((int(s.get("uid")), str(s.get("sensor")), float(s.get("last_t") or 0.0))
                                  for s in t.get("sources") or [] if s.get("uid") is not None),
                                 key=lambda s: (s[0], s[1]))),
            review=tuple(sorted(str(r.get("kind") if isinstance(r, dict) else r) for r in t.get("review") or [])),
        )
    return out


@dataclass
class Health:
    ok: bool
    reasons: list[str]
    nav_bad: bool  # navigation suspect now (not ready, off GNSS, GPS glitch)


def health(v: Vehicle) -> Health:
    reasons: list[str] = []
    nav_bad = False
    if v.link_lost:
        reasons.append("link lost")
    if v.nav_ready is not True:
        reasons.append("navigation not ready")
        nav_bad = True
    if v.nav_source not in (None, "gnss"):
        reasons.append(f"navigation on {v.nav_source}")
        nav_bad = True
    if v.gps_glitch is True:
        reasons.append("GPS glitch flagged")
        nav_bad = True
    if v.pos is None or "position" in v.stale:
        reasons.append("position stale")
    if v.battery_pct is None or "battery" in v.stale:
        reasons.append("battery not known")
    return Health(ok=not reasons, reasons=reasons, nav_bad=nav_bad)


def seen_by(track: Track, vehicles: dict[int, Vehicle], t: float) -> list[dict[str, Any]]:
    """Which of our sensors detected the track and how long ago, freshest first."""
    out = []
    for uid, sensor, last in track.sources:
        v = vehicles.get(uid)
        out.append({"uid": uid, "callsign": v.callsign if v else f"uid {uid}", "sensor": sensor,
                    "age_s": round(max(0.0, t - last), 1)})
    return sorted(out, key=lambda s: (s["age_s"], s["uid"], s["sensor"]))


def orbit_radius(v: Vehicle, cfg: Config) -> float:
    """max(1.1 x the minimum turn radius, a quarter of the imaging sensor's range), rounded up to
    the next 100 m."""
    th = cfg.t
    turn = th["orbit_turn_margin"] * (v.min_turn_m if v.min_turn_m else th["default_min_turn_m"])
    look = th["orbit_range_fraction"] * (v.imaging_range_m or 0.0)
    return float(math.ceil(max(turn, look) / 100.0) * 100.0)


def eta_s(v: Vehicle, point: tuple[float, float]) -> float | None:
    if v.pos is None or not v.cruise_mps:
        return None
    return haversine_m(v.pos, point) / v.cruise_mps


# ---- the beliefs of one tick ---------------------------------------------------------------------


@dataclass
class Interest:
    """Where a track stands against the watch: the watch area it is in (the first, in the
    config's order) and its approach to each protected point, nearest CPA first."""

    area: str | None
    approaches: list[Approach]

    @property
    def closing(self) -> Approach | None:
        return self.approaches[0] if self.approaches else None


@dataclass
class Beliefs:
    t: float
    cfg: Config
    vehicles: dict[int, Vehicle]
    tracks: dict[str, Track]
    health: dict[int, Health]
    nav_suspect: dict[int, str]  # uid -> why (now, or within nav_clear_s)
    engaging: dict[int, str]  # uid -> why an engagement is in play on it
    claims: dict[int, str]  # uid -> the own card or order that claims it
    interest: dict[str, Interest]
    pending: list[dict[str, Any]]
    alerts: list[dict[str, Any]]
    own_open: list[dict[str, Any]]  # own intents in an open state, oldest first
    own_pending: int
    reserve_pct: float
    expire_s: float
    intents: dict[int, dict[str, Any]]  # own intents by id
    lost_full: dict[int, dict[str, Any]] = field(default_factory=dict)
    # our own holds that park a vehicle (isr.release, safety.stuck): a vehicle on one is idle
    own_idle: set[int] = field(default_factory=set)

    @property
    def pressure(self) -> dict[str, int]:
        return {"pending": len(self.pending), "own_pending": self.own_pending, "alerts": len(self.alerts)}

    @property
    def low_reserve_pct(self) -> float:
        return self.reserve_pct + self.cfg.t["low_reserve_margin_pct"]

    @property
    def free_battery_pct(self) -> float:
        return self.low_reserve_pct + self.cfg.t["free_battery_margin_pct"]

    def free_reason(self, v: Vehicle) -> str | None:
        """Why `v` is not free for a task, or None when it is (the module docstring)."""
        h = self.health[v.uid]
        if not h.ok:
            return "; ".join(h.reasons)
        if v.uid in self.nav_suspect:
            return f"navigation suspect ({self.nav_suspect[v.uid]})"
        if v.battery_pct is not None and v.battery_pct < self.free_battery_pct:
            return f"battery {v.battery_pct:.0f}% under {self.free_battery_pct:.0f}%"
        if v.uid in self.claims:
            return f"claimed by {self.claims[v.uid]}"
        if v.uid in self.engaging:
            return f"engagement in play ({self.engaging[v.uid]})"
        if (v.mode or "").upper() in RETURNING_MODES:
            return f"returning ({v.mode})"
        task = v.task or {}
        if task and task.get("state") not in NOT_RUNNING and v.task_intent not in self.own_idle:
            return f"on {task.get('task')} i{task.get('intent_id')} ({task.get('state')})"
        return None

    def alerts_for(self, uid: int) -> list[dict[str, Any]]:
        return [a for a in self.alerts if _uid(a.get("uid")) == uid]


def _uid(v: Any) -> int | None:
    try:
        return int(v)
    except (TypeError, ValueError):
        return None


def policy_value(policy: dict[str, Any], *path: str, default: Any = None) -> Any:
    cur: Any = (policy or {}).get("policy") or {}
    for k in path:
        if not isinstance(cur, dict) or k not in cur:
            return default
        cur = cur[k]
    return cur


def interest_of(track: Track, cfg: Config) -> Interest:
    where = track.where
    if where is None:
        return Interest(area=None, approaches=[])
    area = None
    for a in cfg["watch_area"]:
        if "polygon" in a:
            if in_polygon(where, a["polygon"]):
                area = a["name"]
                break
        elif haversine_m(where, a["center"]) <= a["radius_m"]:
            area = a["name"]
            break
    approaches = []
    for p in cfg["protected"]:
        d, c, tc = cpa(list(where), list(track.velocity) if track.velocity else None, p["point"], cfg.t["holding_mps"])
        approaches.append(Approach(name=p["name"], point=(p["point"][0], p["point"][1]), distance_m=d, cpa_m=c, tcpa_s=tc))
    approaches.sort(key=lambda a: (a.cpa_m, a.name))
    return Interest(area=area, approaches=approaches)


def closing_alert(i: Interest, cfg: Config) -> Approach | None:
    """The protected point the track is closing on inside the CPA alert within the TCPA
    horizon (or already inside the alert distance), nearest CPA first."""
    th = cfg.t
    for a in i.approaches:
        if a.cpa_m <= th["cpa_alert_m"] and (a.distance_m <= th["cpa_alert_m"] or (a.tcpa_s is not None and a.tcpa_s <= th["tcpa_max_s"])):
            return a
    return None


def build(reads: Reads, cfg: Config, ledger: Any) -> Beliefs:
    """The tick's beliefs. `ledger` supplies what the reads alone cannot: the cards and orders
    of ours that claim vehicles, the engagements the change feed reported, and when each
    vehicle's navigation was last suspect."""
    t = reads.tick_t
    vehicles = vehicles_of(reads)
    tracks = tracks_of(reads)
    hl = {uid: health(v) for uid, v in vehicles.items()}
    nav_suspect: dict[int, str] = {}
    for uid, h in hl.items():
        if h.nav_bad:
            nav_suspect[uid] = ", ".join(r for r in h.reasons if r != "link lost" and r != "position stale" and r != "battery not known")
        else:
            last = ledger.nav_bad_t.get(str(uid))
            if last is not None and t - last < cfg.t["nav_clear_s"]:
                nav_suspect[uid] = f"suspect within the last {cfg.t['nav_clear_s']:.0f} s"
    pending = sorted(reads.inbox.get("pending") or [], key=lambda i: int(i.get("id", 0)))
    alerts = sorted(reads.inbox.get("alerts") or [], key=lambda a: (str(a.get("id")), str(a.get("kind"))))
    engaging: dict[int, str] = {}
    for i in pending:
        if i.get("kind") == "engage":
            for u in i.get("uids") or []:
                if _uid(u) is not None:
                    engaging[_uid(u)] = f"nomination i{i.get('id')} pending"  # type: ignore[index]
    for iid, e in sorted(ledger.engaging.items()):
        if e.get("state") in ENGAGE_IN_PLAY:
            for u in e.get("uids") or []:
                if _uid(u) is not None and _uid(u) not in engaging:
                    engaging[_uid(u)] = f"engagement i{iid} {e.get('state')}"  # type: ignore[index]
    own = {}
    if reads.intents is not None:
        for i in reads.intents.get("intents") or []:
            own[int(i["id"])] = i
    own_open = [own[k] for k in sorted(own) if own[k].get("state") in OPEN_STATES]
    own_pending = sum(1 for i in own_open if i.get("state") == "pending_approval")
    claims = ledger.claims(own, vehicles)
    return Beliefs(
        t=t, cfg=cfg, vehicles=vehicles, tracks=tracks, health=hl, nav_suspect=nav_suspect, engaging=engaging,
        claims=claims, interest={tid: interest_of(tr, cfg) for tid, tr in tracks.items()},
        pending=pending, alerts=alerts, own_open=own_open, own_pending=own_pending,
        reserve_pct=float(policy_value(reads.policy, "rules", "reserve_pct", default=20.0) or 20.0),
        expire_s=float(policy_value(reads.policy, "approval", "expire_s", default=120.0) or 120.0),
        intents=own, lost_full=dict(sorted(reads.lost_full.items())),
        own_idle={int(e["intent"]) for e in ledger.entries.values() if e.get("play") in IDLE_PLAYS},
    )
