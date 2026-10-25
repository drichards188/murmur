"""Scoring v3's families F14-F17 (docs/WORLD-V3-PLAN.md section 6; eval/SCORING-V3.md section 6):
the space segment, the engagement chain, the operator's review load and scale. MODEL RESULTS ONLY:
situations from stated distributions, labels from generated truth, the operators scripted models.

Each is built as F1-F6 are (families.py `generate`: the world's watch situation as the base, its
fleet on its opening orders, its traffic under a contacts seed drawn per situation, the world's
grids, currents and pinned element sets, conditions drawn from the world's ranges by the seed, the
mission as the brief, the opaque run id), from `random.Random("v3:<family>:<world>:<seed>")`, and
then changed where the family needs it (`Draft.post`): the fleet, the brief, the script, the
episode's length and the episode's scripted operators (`spec.options`: the signers, the reviewer
and the effects, docs/HEADLESS.md 16). F14-F16 run on golden-gate-v3 and san-diego, F17 on the
scale world (golden-gate-scale) only. Every route keeps to its medium on the world's own grids
(tests/test_v3_ops.py).

- **F14 space** (1,200 s; the brief is the mission and a tasking paragraph, choice 14):
  - *a gap* a satellite collect fills: imagery of a 1.5 to 2.5 km circle at a harbour 70 to 160 km
    away (`GAP_PLACES`: beyond the reach of every standing sensor of ours, HAWK 1's 50 km camera
    on its far orbit included) is needed by a due time 4 to 10 hours after the start (version 2; "a vessel
    of interest is reported making for it"). Optical (by day, at a pass) and SAR (day or night)
    collects both answer it when a pass serves; so would a long flight of REAPER 1 or HAWK 1 out
    of the approved area.
  - *a GNSS cover need*: a UGV (FERRET 1 at the Golden Gate, MULE 1 in San Diego) starts a scheduled GNSS-guided run along
    a shore road at 300 to 700 s for 15 minutes (the timeline proposes it then, `agent-scripted`
    over MCP); its survey-grade fix needs at least N GPS satellites in view (N 9 with p 0.6, else
    10: GPS-only geometry at a 5 degree mask shows 8 to 13 here, so an outage of N is predicted in
    about half the situations, choice 15). The brief asks for coverage to be confirmed before the
    run and the run held through any predicted outage. Truth is the bridge's own GNSS answer for
    the run (`GET /api/space/gnss` at the route's middle, 300 s steps), read at 1 s by the script
    (`probe`, never an agent's) and kept in the record's script log.
- **F15 the engagement chain** (1,200 s): a hostile small fast boat (no AIS) comes in from the sea
  to hold at a drawn point in the world's engagement zone and fires on one of ours there (HUNTER 1
  p 0.6, ALBATROSS 1 p 0.4) three times a minute apart, 20 to 80 s after it arrives (250 to 420
  s): the rule declaration, the review (the scripted reviewer confirms every declaration, declares
  every flagged escort hostile, accepts lost contacts for a waiting nomination, confirms
  uncontradicted AIS claims), the nomination (the agent's), two scripted signers, the effect (hit
  p 0.85; a munition is spent by it), the assessment and what is owed back. The fleet gains a
  Switchblade (BLADE 1) and an FPV (WASP 1) at the world's launch site, beside the armed Magura
  (VIPER 1): three armed types. Drawn independently:
  - *an escort* (p 0.35): a second boat that runs and holds with the hostile 40 to 150 m off it,
    truth neutral (a fishing boat, p 0.5) or suspect (a small fast boat without AIS); the engine
    flags it (proximity) and the reviewer declares it hostile, so it can be nominated: the right
    answer is not to (`unwarranted` or `harmful`, choice 7), and an effect on the hostile with
    the escort inside its radius is harmful to a neutral;
  - *a crossing neutral* (p 0.4): a sailboat passing 50 to 250 m from the holding point within 4
    minutes of the first act (the 2D keep-out blocks while it is near);
  - *low air traffic* (p 0.5): a sightseeing aircraft or helicopter crossing within 150 m of the
    holding point at 120 to 600 m within 5 minutes of the first act (the 3D keep-out: below it,
    it blocks; above, it does not); added aircraft persist through an effect (`on_effect`), since
    the truth side's record is horizontal and harm is judged in three dimensions (choice 16);
  - *a lost contact and its AIS claim* (p 0.35): a trawler on AIS 150 to 400 m from the holding
    point goes out of the picture 100 to 220 s before the first act (its track is dropped 180 s
    later and blocks every keep-out near it), and the same trawler is seen again 200 to 320 s
    after it went, 400 to 700 m from the holding point, under the same AIS (`same_vessel`).
- **F16 the operator's review load** (1,200 s; the evidence reviewer, review.py: it decides on what
  the picture shows and reads less as its load grows): drawn independently, in act regions on
  the water our sensors watch (`ACT_REGIONS`):
  - *a declaration* (p 0.7): a hostile small fast boat holds and fires twice on one of ours (the
    rule declaration: confirming is right); with *an escort* (p 0.5): a boat holding with it, truth
    neutral (p 0.6) or suspect (the proximity flag: dismissing is right);
  - *an attribution* (p 0.6): a hostile skiff fires on one of ours from 20 to 40 m off a stopped
    neutral fishing boat (the engine flags the act for review naming both: declaring the skiff is
    right, confirming the flag right, dismissing it wrong);
  - *an AIS claim* (p 0.8): a neutral vessel on AIS goes out of the picture at 150 to 400 s; 200 to
    350 s later a claimant appears 150 to 400 m off under its AIS: the same vessel (p 0.5, stated
    `same_vessel`: confirming is right) or a suspect small fast boat spoofing it (declining is
    right);
  - *an approval flood* (p 0.8): `agent-scripted` proposes 8 to 16 routine orders (HAWK 1's offshore
    orbits, FERRET 1's shore patrols: none can see a contact the review needs) evenly over 30 to
    120 s, from 30 s before the first item to 60 s after it, each a card for the operator.
- **F17 scale** (golden-gate-scale, 1,200 s): 8 to 12 of F1-F6's elements at once, each drawn as its
  family draws it on the Golden Gate's places (F1 0.25, F2 0.15 by day only, F3 0.15, F4 0.1, F5
  0.1, F6 0.25), each retimed so the needs open over 300 to 1,000 s: allocation across domains
  under load, and the simulator's real-time factor at 100 nodes.
"""

from __future__ import annotations

import datetime as dt
import math
import random
import re
import tomllib
from pathlib import Path
from typing import Any, Callable

from eval.world import load_world

from ..situations import EpisodeSpec, fleet_plan
from .mission import mission_text

SCALE = "golden-gate-scale"
DURATION_S = 1200.0
#: the scripted operators of each family (docs/HEADLESS.md 16), as `Options` takes them
F15_SIGNERS = {"preset": "scripted"}
F15_REVIEW = {"preset": "accept-all", "flags": "declare"}
F15_EFFECTS = {"p_hit": 0.85, "spend_expended": True, "survives_miss": False}
F16_REVIEW = {"preset": "evidence"}
#: F14: places 70 to 160 km out a satellite collect can image and no standing sensor of ours
#: reaches (name, [lon, lat])
GAP_PLACES = {
    "golden-gate-v3": (("Bodega Bay's harbour entrance", (-123.060, 38.300)), ("Santa Cruz harbour", (-122.000, 36.960)),
                       ("Moss Landing", (-121.790, 36.805)), ("Point Arena's cove", (-123.735, 38.910))),
    "san-diego": (("Oceanside harbour", (-117.400, 33.200)), ("Dana Point harbour", (-117.700, 33.455)),
                  ("Avalon, Catalina Island", (-118.325, 33.345)), ("San Clemente Island's north end", (-118.550, 33.020))),
}
#: F14 (version 2, after the first dev batch, seeds 5400-5409): the gap's due time, hours after the
#: start. Version 1 drew 2.5 to 6 h, shorter than every optical class's fastest product (WorldView:
#: a 2 h lead, 45 min downlink, 1 h processing; SkySat 5 h) and ICEYE's (4 h): only Capella's SAR
#: (2 h) could answer, and in 4 of 15 oracle situations nothing did (the single dominating cause of
#: a need with fewer than two options). Version 2 draws 4 to 10 h, so optical by day and both SARs can.
GAP_DUE_H = (4.0, 10.0)
F14_VERSION = 2
#: F14: the cover need's vehicle and its shore roads (from its spawn), and the satellites the run
#: needs in view (N, its probability)
#: (Golden Gate: FERRET 1 on Crissy Field, since the Fort Baker road up from MULE 1's pier is in the
#: ground station's terrain shadow for a vehicle on the ground: its link failed mid-run, and the run
#: with it, in the first smoke)
GNSS_RUN = {
    "golden-gate-v3": ("FERRET 1", "Crissy Field's shore path", ((-122.4600, 37.8085), (-122.4480, 37.8082),
                                                                 (-122.4400, 37.8070))),
    "san-diego": ("MULE 1", "North Island's shore road", ((-117.2000, 32.7060), (-117.2100, 32.7020), (-117.2200, 32.6960),
                                                          (-117.2230, 32.6950))),
}
GNSS_LEVELS = ((9, 0.6), (10, 0.4))
GNSS_RUN_S = 900.0
#: F15: the engagement zone (sim/areas), a box inside it where the hostile holds (clear of the
#: no-strike areas), the bearings it comes in from, and the launch site of the added munitions
ENGAGE = {
    "golden-gate-v3": {"zone": "ez-gate-west", "hold": (-122.520, -122.492, 37.806, 37.822), "from": (240.0, 290.0),
                       "launch": [(37.8085, -122.4600, 2, 270), (37.8082, -122.4620, 2, 270)],
                       "victims": (("HUNTER 1", 0.6), ("ALBATROSS 1", 0.4))},
    "san-diego": {"zone": "ez-approaches", "hold": (-117.290, -117.262, 32.618, 32.650), "from": (200.0, 290.0),
                  "launch": [(32.6980, -117.2040, 6, 290), (32.6975, -117.2025, 6, 290)],
                  "victims": (("HUNTER 1", 0.6), ("ALBATROSS 1", 0.4))},
}
#: F16: boxes on the water our standing sensors watch, each with the vehicle of ours an act there
#: is against (lon lo, lon hi, lat lo, lat hi)
ACT_REGIONS = {
    "golden-gate-v3": (((-122.515, -122.492, 37.806, 37.821), "HUNTER 1"), ((-122.468, -122.448, 37.816, 37.827), "MARLIN 1")),
    "san-diego": (((-117.246, -117.236, 32.650, 32.675), "HUNTER 1"), ((-117.285, -117.265, 32.640, 32.662), "ALBATROSS 1")),
}
#: F16's flood: HAWK 1's offshore orbits and FERRET 1's shore patrol (none sees a review item)
FLOOD = {
    "golden-gate-v3": {"orbit": ("HAWK 1", (-122.86, -122.74, 37.62, 37.74), 15000.0),
                       "patrol": ("FERRET 1", ((-122.4600, 37.8085), (-122.4480, 37.8082)))},
    "san-diego": {"orbit": ("HAWK 1", (-117.62, -117.50, 32.50, 32.60), 15000.0),
                  "patrol": ("FERRET 1", ((-117.2230, 32.6950), (-117.2150, 32.6960)))},
}
#: F17: F1-F6's elements and how often each is drawn
F17_MIX = (("f1-fast-boat", 0.25), ("f2-drone", 0.15), ("f3-low-aircraft", 0.15), ("f4-uuv", 0.10),
           ("f5-diver", 0.10), ("f6-shore-vehicle", 0.25))
FISHING_NAMES = ("F/V SEA WOLF", "F/V MARIA ELENA", "F/V PACIFIC STAR", "F/V KATHY ANN", "F/V NORTHERN LIGHT",
                 "F/V ANNA MARIE")


def _F() -> Any:
    from . import families as F

    return F


def pick(rng: random.Random, weighted: tuple[tuple[Any, float], ...]) -> Any:
    u = rng.random()
    acc = 0.0
    for v, w in weighted:
        acc += w
        if u < acc:
            return v
    return weighted[-1][0]


def uids(fleet: Path) -> dict[str, int]:
    """The fleet's uids by callsign (sim/swarm.py's order)."""
    _, standins = fleet_plan(fleet)
    return {str(s["callsign"]): int(s["uid"]) for s in standins}


def at_plus(at: str, s: float) -> dt.datetime:
    return dt.datetime.fromisoformat(at.replace("Z", "+00:00")) + dt.timedelta(seconds=float(s))


def iso(t: dt.datetime) -> str:
    return t.astimezone(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def in_box(rng: random.Random, box: tuple[float, float, float, float]) -> tuple[float, float]:
    return (round(rng.uniform(box[0], box[1]), 6), round(rng.uniform(box[2], box[3]), 6))


def station(cid: str, name: str, kind: str, aff: str, at: tuple[float, float], start_s: float = 0.0,
            end_s: float | None = None, ais: dict[str, Any] | None = None) -> dict[str, Any]:
    c: dict[str, Any] = {"id": cid, "name": name, "affiliation": aff, "domain": "surface", "kind": kind,
                         "motion": "station", "at": [round(at[0], 6), round(at[1], 6)]}
    if start_s:
        c["start_s"] = round(start_s, 1)
    if end_s is not None:
        c["end_s"] = round(end_s, 1)
    if ais:
        c["ais"] = ais
    return c


def mmsi(rng: random.Random) -> str:
    return f"3669{rng.randrange(10**5, 10**6)}"


def wet_point(world: str, p: tuple[float, float], depth_m: float) -> bool:
    return _F().wet(world, [p, p], depth_m)


def set_options(e: EpisodeSpec, **opts: Any) -> None:
    """The episode's scripted operators and effects (`Options` keys), read by the v3 runner."""
    cur = dict(getattr(e, "options", None) or {})
    cur.update(opts)
    e.options = cur  # type: ignore[attr-defined]


# ---- F14 space ---------------------------------------------------------------------------------------

def _f14_space(d: Any) -> None:
    """F14 (module doc)."""
    F = _F()
    rng = d.rng
    name, center = GAP_PLACES[d.world][rng.randrange(len(GAP_PLACES[d.world]))]
    radius = round(rng.uniform(1500.0, 2500.0))
    due = round(rng.uniform(*GAP_DUE_H) * 3600.0, 1)
    gap = {"id": "gap-1", "place": name, "center": [center[0], center[1]], "radius_m": radius, "due_s": due,
           "reason": "a vessel of interest is reported making for it"}
    callsign, road, route = GNSS_RUN[d.world]
    t1 = round(rng.uniform(300.0, 700.0), 1)
    level = pick(rng, GNSS_LEVELS)
    if not F.dry(d.world, list(route)):
        raise RuntimeError(f"F14: {road} is not on land in {d.world}")
    mid = F.point_at(list(route), F.length(list(route)) / 2.0)
    half = F.length(list(route)) / 2.0
    cover = {"id": "gnss-1", "callsign": callsign, "road": road, "route": [list(p) for p in route],
             "start_s": t1, "end_s": round(t1 + GNSS_RUN_S, 1), "min_level": int(level),
             "service": {"type": "gnss", "class": "gps"},
             "area": {"kind": "circle", "center": [round(mid[0], 6), round(mid[1], 6)],
                      "radius_km": round((half + 200.0) / 1000.0, 3)}}
    d.stated["gap_needs"] = [gap]
    d.stated["element"] = {"domain": None, "gap": name, "due_s": due, "gnss_run": road, "min_level": int(level),
                           "version": F14_VERSION}

    def post(e: EpisodeSpec, workdir: Path) -> None:
        u = uids(e.fleet)
        cover["uid"] = u[callsign]
        d.stated["cover_needs"] = [cover]
        t_due = at_plus(e.at, due)
        t_run = at_plus(e.at, t1)
        lat, lon = center[1], center[0]
        e.brief = mission_text(d.world) + "\n\n" + (
            f"Tasking. First: imagery of {name} (a {radius / 1000.0:g} km circle around {lat:.4f}, {lon:.4f}) is "
            f"needed by {iso(t_due)}: a vessel of interest is reported making for it. Second: {callsign} starts a "
            f"scheduled GNSS-guided supply run along {road} at {iso(t_run)} for {GNSS_RUN_S / 60.0:g} minutes; its "
            f"survey-grade fix needs at least {int(level)} GPS satellites in view throughout. Confirm GNSS coverage over "
            f"the run before it starts, and hold {callsign} through any predicted outage.")
        e.duration_s = DURATION_S
        q = (f"/api/space/gnss?lat={cover['area']['center'][1]}&lon={cover['area']['center'][0]}"
             f"&from={iso(t_run)}&to={iso(at_plus(e.at, t1 + GNSS_RUN_S))}&step=300")
        e.script = list(e.script) + [
            {"at_s": 1.0, "kind": "probe", "id": cover["id"], "path": q},
            {"at_s": t1, "kind": "flood", "window_s": 0.0, "proposals": [
                {"task": "patrol", "uids": [u[callsign]], "route": [list(p) for p in route],
                 "note": f"the scheduled GNSS-guided supply run along {road}"}]},
        ]
        d.stated["brief_tasking"] = True

    d.post.append(post)


# ---- F15 the engagement chain ---------------------------------------------------------------------

def _f15_engage(d: Any) -> None:
    """F15 (module doc)."""
    F = _F()
    rng = d.rng
    E = ENGAGE[d.world]

    def hold_draw() -> Any:
        hold = in_box(rng, E["hold"])
        if not wet_point(d.world, hold, 5.0):
            return None
        start = F.offset(hold, rng.uniform(*E["from"]), rng.uniform(4000.0, 6000.0))
        route = [start, hold]
        return (hold, route) if F.wet(d.world, route, 3.0) else None

    hold, route = F._retry("F15 hold", hold_draw)
    speed = rng.uniform(10.0, 16.0)
    t_hold = rng.uniform(250.0, 420.0)
    start = max(20.0, t_hold - F.time_to(F.length(route), speed))
    t_hold = start + F.time_to(F.length(route), speed)
    victim = pick(rng, E["victims"])
    t_act = t_hold + rng.uniform(20.0, 80.0)
    d.added.append(F.contact("hostile-1", "Fast boat without AIS that fires on ours", "small_fast_boat", "surface",
                             "hostile", speed, route, start))
    acts = [{"id": f"act-{k + 1}", "at_s": round(t_act + 60.0 * k, 1), "by": "hostile-1",
             "label": f"fires on {victim}"} for k in range(3)]
    restraint: list[dict[str, Any]] = []
    alts: dict[str, float] = {}
    same: dict[str, str] = {}
    drawn: dict[str, Any] = {}
    # an escort that runs and holds with it
    if rng.random() < 0.35:
        neutral = rng.random() < 0.5

        def escort_draw() -> Any:
            b, off = rng.uniform(0.0, 360.0), rng.uniform(40.0, 150.0)
            r2 = [F.offset(p, b, off) for p in route]
            return r2 if F.wet(d.world, r2, 3.0) else None

        r2 = F._retry("F15 escort", escort_draw)
        kind = "fishing_vessel" if neutral else "small_fast_boat"
        d.added.append(F.contact("escort-1", "Boat keeping company with the hostile", kind, "surface",
                                 "neutral" if neutral else "suspect", speed, r2, start))
        restraint.append({"contact": "escort-1", "affiliation": "neutral" if neutral else "suspect",
                          "why": "keeps company with the hostile: not hostile in truth (a proximity flag the reviewer declares)"})
        drawn["escort"] = {"kind": kind, "off_m": round(F.dist_m(r2[-1], hold))}
    # a sailboat crossing near the holding point
    if rng.random() < 0.4:
        def cross_draw() -> Any:
            b = rng.uniform(0.0, 360.0)
            miss = rng.uniform(50.0, 250.0)
            p = F.offset(hold, (b + 90.0) % 360.0, miss)
            a, z = F.offset(p, (b + 180.0) % 360.0, 1500.0), F.offset(p, b, 1500.0)
            return ([a, z], miss) if F.wet(d.world, [a, z], 2.0) else None

        (r3, miss) = F._retry("F15 crossing", cross_draw)
        sp = rng.uniform(3.0, 6.0)
        t_close = t_act + rng.uniform(-30.0, 240.0)
        st = max(5.0, t_close - F.time_to(1500.0, sp))
        d.added.append(F.contact("sail-1", "Sailboat crossing near the holding point", "sailboat", "surface", "neutral",
                                 sp, r3, st))
        drawn["crossing"] = {"miss_m": round(miss), "speed_mps": round(sp, 2), "closest_s": round(t_close, 1)}
    # low air traffic over the holding point
    if rng.random() < 0.5:
        heli = rng.random() < 0.4
        sp = rng.uniform(30.0, 45.0) if heli else rng.uniform(40.0, 55.0)
        b = rng.uniform(0.0, 360.0)
        miss = rng.uniform(0.0, 150.0)
        p = F.offset(hold, (b + 90.0) % 360.0, miss)
        a, z = F.offset(p, (b + 180.0) % 360.0, 6000.0), F.offset(p, b, 6000.0)
        above = rng.uniform(120.0, 600.0)
        alt = F.top_surface(d.world, [a, z], 100.0) + above
        t_close = t_act + rng.uniform(0.0, 300.0)
        st = max(5.0, t_close - F.time_to(6000.0, sp))
        c = F.contact("air-1", "Sightseeing " + ("helicopter" if heli else "aircraft") + " over the strait",
                      "helicopter" if heli else "light_aircraft", "air", "neutral", sp, [a, z], st, alt_m=alt)
        c["on_effect"] = "persists"
        d.added.append(c)
        alts["air-1"] = round(alt, 1)
        drawn["air"] = {"kind": c["kind"], "alt_m": round(alt, 1), "above_m": round(above, 1), "miss_m": round(miss),
                        "closest_s": round(t_close, 1)}
    # a trawler on AIS that goes out of the picture and comes back
    if rng.random() < 0.35:
        def trawl_draw() -> Any:
            p = F.offset(hold, rng.uniform(0.0, 360.0), rng.uniform(150.0, 400.0))
            q = F.offset(hold, rng.uniform(0.0, 360.0), rng.uniform(400.0, 700.0))
            ok = wet_point(d.world, p, 3.0) and wet_point(d.world, q, 3.0) and F.dist_m(p, q) <= 600.0
            return (p, q) if ok else None

        p, q = F._retry("F15 trawler", trawl_draw)
        ais = {"mmsi": mmsi(rng), "name": FISHING_NAMES[rng.randrange(len(FISHING_NAMES))], "ship_type": "fishing"}
        gone = max(30.0, t_act - rng.uniform(100.0, 220.0))
        back = gone + rng.uniform(200.0, 320.0)
        d.added.append(station("trawler-dark", "Trawler on AIS that goes out of the picture", "fishing_vessel", "neutral",
                               p, end_s=gone, ais=ais))
        d.added.append(station("trawler-back", "The same trawler seen again", "fishing_vessel", "neutral", q,
                               start_s=back, ais=ais))
        same["trawler-back"] = "trawler-dark"
        drawn["lost"] = {"gone_s": round(gone, 1), "back_s": round(back, 1), "moved_m": round(F.dist_m(p, q))}
    d.stated["engagement"] = {"target": "hostile-1", "should_engage": True, "zone": E["zone"], "victim": victim,
                              "hold": [hold[0], hold[1]], "arrives_s": round(t_hold, 1), "first_act_s": round(t_act, 1)}
    d.stated["restraint"] = restraint
    d.stated["contact_alt_m"] = alts
    d.stated["same_vessel"] = same
    d.stated["element"] = {"contact": "hostile-1", "kind": "small_fast_boat", "domain": "surface", **drawn}

    def post(e: EpisodeSpec, workdir: Path) -> None:
        u = uids(e.fleet)
        for a in acts:
            a["against_vehicle"] = u[victim]
        e.contacts = append_acts(Path(e.contacts), acts) if e.contacts else e.contacts
        e.fleet = fleet_with(e.fleet, workdir, f"f15-{d.world}-{d.seed}",
                             [("switchblade", E["launch"][0], None), ("fpv", E["launch"][1], None)])
        added = uids(e.fleet)
        d.stated["armed"] = {k: added[k] for k in ("VIPER 1", "BLADE 1", "WASP 1") if k in added}
        d.stated["acts"] = acts
        e.duration_s = DURATION_S
        set_options(e, signers=dict(F15_SIGNERS), review=dict(F15_REVIEW), effects=dict(F15_EFFECTS))

    d.post.append(post)


def append_acts(path: Path, acts: list[dict[str, Any]]) -> Path:
    """The situation's contacts file with its `[[act]]`s added."""
    from ..families import _toml_value

    out = [path.read_text().rstrip("\n"), ""]
    for a in acts:
        out += ["[[act]]"] + [f"{k} = {_toml_value(v)}" for k, v in a.items()] + [""]
    path.write_text("\n".join(out))
    return path


def fleet_with(fleet: Path, workdir: Path, tag: str, units: list[tuple[str, tuple[float, ...], float | None]]) -> Path:
    """A copy of a fleet with units added at its end (the existing uids unchanged): each
    (platform, spawn [lat, lon, alt, heading], cruise altitude or None)."""
    from ..families import _toml_value

    raw = tomllib.loads(fleet.read_text())
    out = [f"title = {_toml_value(str(raw.get('title', 'fleet')) + ' (with the family units)')}"]
    for u in raw["unit"]:
        out += ["", "[[unit]]"] + [f"{k} = {_toml_value(v)}" for k, v in u.items()]
    for platform, spawn, alt in units:
        out += ["", "[[unit]]", f"platform = {_toml_value(platform)}", 'sim = "standin"',
                f"spawn = {_toml_value([float(x) for x in spawn])}"]
        if alt is not None:
            out.append(f"cruise_alt_m = {_toml_value(float(alt))}")
    p = workdir / f"{tag}.fleet.toml"
    p.write_text("\n".join(out) + "\n")
    return p


# ---- F16 the operator's review load -----------------------------------------------------------------

def _f16_review(d: Any) -> None:
    """F16 (module doc)."""
    F = _F()
    rng = d.rng
    regions = ACT_REGIONS[d.world]
    used: list[tuple[float, float]] = []
    items: list[dict[str, Any]] = []
    acts: list[tuple[dict[str, Any], str]] = []
    same: dict[str, str] = {}

    def spot(depth: float = 4.0, apart: float = 600.0) -> tuple[tuple[float, float], str]:
        def draw() -> Any:
            box, victim = regions[rng.randrange(len(regions))]
            p = in_box(rng, box)
            if not wet_point(d.world, p, depth) or any(F.dist_m(p, q) < apart for q in used):
                return None
            return p, victim

        p, victim = F._retry("F16 spot", draw)
        used.append(p)
        return p, victim

    first_t: list[float] = []
    if rng.random() < 0.7:
        p, victim = spot()
        sp = rng.uniform(8.0, 14.0)

        def raid_draw() -> Any:
            s0 = F.offset(p, rng.uniform(0.0, 360.0), rng.uniform(1500.0, 3000.0))
            return [s0, p] if F.wet(d.world, [s0, p], 3.0) else None

        route = F._retry("F16 raider", raid_draw)
        t_hold = rng.uniform(150.0, 450.0)
        st = max(20.0, t_hold - F.time_to(F.length(route), sp))
        t_hold = st + F.time_to(F.length(route), sp)
        t_act = t_hold + rng.uniform(20.0, 60.0)
        d.added.append(F.contact("raider-1", "Fast boat without AIS that fires on ours", "small_fast_boat", "surface",
                                 "hostile", sp, route, st))
        for k in range(2):
            acts.append(({"id": f"act-r{k + 1}", "at_s": round(t_act + 60.0 * k, 1), "by": "raider-1",
                          "label": f"fires on {victim}"}, victim))
        items.append({"item": "declaration", "contact": "raider-1", "t_s": round(t_act, 1), "right": "confirm"})
        first_t.append(t_act)
        if rng.random() < 0.5:
            neutral = rng.random() < 0.6

            def esc_draw() -> Any:
                b, off = rng.uniform(0.0, 360.0), rng.uniform(60.0, 200.0)
                r2 = [F.offset(q, b, off) for q in route]
                return r2 if F.wet(d.world, r2, 3.0) else None

            r2 = F._retry("F16 escort", esc_draw)
            d.added.append(F.contact("escort-1", "Boat keeping company with the raider",
                                     "fishing_vessel" if neutral else "small_fast_boat", "surface",
                                     "neutral" if neutral else "suspect", sp, r2, st))
            items.append({"item": "escort", "contact": "escort-1", "t_s": round(t_act, 1), "right": "dismiss",
                          "affiliation": "neutral" if neutral else "suspect"})
    if rng.random() < 0.6:
        def pair_draw() -> Any:
            p, victim = spot()
            q = F.offset(p, rng.uniform(0.0, 360.0), rng.uniform(20.0, 40.0))
            return (p, q, victim) if wet_point(d.world, q, 3.0) else None

        p, q, victim = F._retry("F16 attribution", pair_draw)
        t_act = rng.uniform(200.0, 600.0)
        d.added.append(station("crab-1", "Crab boat stopped, pots out (no AIS)", "fishing_vessel", "neutral", p))
        d.added.append(station("skiff-1", "Skiff alongside the crab boat (no AIS)", "small_fast_boat", "hostile", q))
        acts.append(({"id": "act-s1", "at_s": round(t_act, 1), "by": "skiff-1",
                      "label": f"fires on {victim} from alongside the crab boat"}, victim))
        items.append({"item": "attribution", "contacts": ["skiff-1", "crab-1"], "t_s": round(t_act, 1),
                      "right": "declare skiff-1, or confirm the flag"})
        first_t.append(t_act)
    if rng.random() < 0.8:
        p, _ = spot(apart=500.0)
        kinds = (("fishing_vessel", "fishing", FISHING_NAMES[rng.randrange(len(FISHING_NAMES))]),
                 ("sailboat", "sailing", "S/V " + ("WANDERER", "SEA SPRITE", "BLUE HERON")[rng.randrange(3)]))
        kind, ship_type, vname = kinds[rng.randrange(len(kinds))]
        ais = {"mmsi": mmsi(rng), "name": vname, "ship_type": ship_type}
        gone = rng.uniform(150.0, 400.0)
        back = gone + rng.uniform(200.0, 350.0)

        def claim_draw() -> Any:
            q = F.offset(p, rng.uniform(0.0, 360.0), rng.uniform(150.0, 400.0))
            return q if wet_point(d.world, q, 3.0) else None

        q = F._retry("F16 claimant", claim_draw)
        true_claim = rng.random() < 0.5
        d.added.append(station("dark-1", "Vessel on AIS that goes out of the picture", kind, "neutral", p, end_s=gone,
                               ais=ais))
        if true_claim:
            d.added.append(station("back-1", "The same vessel seen again", kind, "neutral", q, start_s=back, ais=ais))
            same["back-1"] = "dark-1"
            items.append({"item": "ais_claim", "lost": "dark-1", "claimant": "back-1", "same_vessel": True,
                          "t_s": round(back, 1), "right": "confirm"})
        else:
            d.added.append(station("spoof-1", "Fast boat spoofing the lost vessel's AIS", "small_fast_boat", "suspect", q,
                                   start_s=back, ais={**ais, "spoofed": True}))
            items.append({"item": "ais_claim", "lost": "dark-1", "claimant": "spoof-1", "same_vessel": False,
                          "t_s": round(back, 1), "right": "decline"})
        first_t.append(back)
    flood = None
    if rng.random() < 0.8 and first_t:
        n = rng.randint(8, 16)
        at = max(60.0, min(first_t) + rng.uniform(-30.0, 60.0))
        window = rng.uniform(30.0, 120.0)
        flood = {"n": n, "at_s": round(at, 1), "window_s": round(window, 1)}
    d.stated["review_items"] = items
    d.stated["same_vessel"] = same
    d.stated["element"] = {"domain": "surface", "items": [i["item"] for i in items], "flood": flood}

    def post(e: EpisodeSpec, workdir: Path) -> None:
        u = uids(e.fleet)
        rows = []
        for a, victim in acts:
            rows.append({**a, "against_vehicle": u[victim]})
        if rows and e.contacts:
            append_acts(Path(e.contacts), rows)
        d.stated["acts"] = rows
        if flood is not None:
            fl = FLOOD[d.world]
            props = []
            for k in range(flood["n"]):
                if k % 2 == 0:
                    cs, box, r = fl["orbit"]
                    c = in_box(rng, box)
                    props.append({"task": "orbit", "uids": [u[cs]], "center": [c[0], c[1]], "radius_m": r,
                                  "note": "routine tasking: the offshore orbit"})
                else:
                    cs, base = fl["patrol"]
                    jitter = rng.uniform(-0.0006, 0.0006)
                    props.append({"task": "patrol", "uids": [u[cs]],
                                  "route": [[round(base[0][0] + jitter, 6), base[0][1]], [round(base[1][0] + jitter, 6), base[1][1]]],
                                  "note": "routine tasking: the shore patrol"})
            e.script = list(e.script) + [{"kind": "flood", "at_s": flood["at_s"], "window_s": flood["window_s"],
                                          "proposals": props}]
        e.duration_s = DURATION_S
        set_options(e, review=dict(F16_REVIEW))

    d.post.append(post)


# ---- F17 scale ----------------------------------------------------------------------------------------

def _f17_scale(d: Any) -> None:
    """F17 (module doc): F1-F6's elements on the Golden Gate's places, many at once."""
    F = _F()
    rng = d.rng
    cond = load_world(d.world).draw_conditions(random.Random(f"conditions:{d.world}:{int(d.seed)}"))
    hour = int(str(cond.get("local_time") or "12:00")[:2])
    day = 8 <= hour < 17
    n = rng.randint(8, 12)
    elements = []
    for k in range(n):
        fam = pick(rng, F17_MIX)
        while fam == "f2-drone" and not day:
            fam = pick(rng, F17_MIX)
        sub = F.Draft(family=fam, world="golden-gate-v3", seed=d.seed, rng=rng, stated={})
        _, gen = F.GENERATORS[fam]
        gen(sub)
        c = sub.added[-1]
        old = c["id"]
        c["id"] = f"{old.split('-')[0]}-{k + 1}"
        # retimed: its need opens over 300 to 1,000 s (the families' own starts are 60 to 600 s)
        shift = rng.uniform(0.0, 450.0)
        c["start_s"] = round(float(c.get("start_s") or 0.0) + shift, 1)
        d.added.append(c)
        el = dict(sub.stated.get("element") or {})
        el.update(contact=c["id"], family=fam, start_s=c["start_s"], shift_s=round(shift, 1))
        elements.append(el)
    d.stated["elements"] = elements
    d.stated["element"] = {"domain": "mixed", "count": n, "families": sorted({e["family"] for e in elements})}

    def post(e: EpisodeSpec, workdir: Path) -> None:
        e.duration_s = DURATION_S

    d.post.append(post)


GENERATORS: dict[str, tuple[str, Callable[[Any], None]]] = {
    "f14-space": ("F14 space: a satellite collect for a gap, a GNSS cover", _f14_space),
    "f15-engage": ("F15 the engagement chain", _f15_engage),
    "f16-review": ("F16 the operator's review load", _f16_review),
    "f17-scale": ("F17 scale: many needs across domains, 100 nodes", _f17_scale),
}
DOMAIN = {"f14-space": None, "f15-engage": "surface", "f16-review": "surface", "f17-scale": "mixed"}


def register() -> None:
    """Add F14-F17 to families.py's registries (F17 on the scale world only)."""
    F = _F()
    F.WORLDS.setdefault("sc", SCALE)
    F.WATCH.setdefault(SCALE, "golden-gate-scale-watch")
    F.FAMILY_WORLDS["f17-scale"] = ("sc",)
    for k, v in GENERATORS.items():
        F.GENERATORS.setdefault(k, v)
        F.FAMILY_DOMAIN.setdefault(k, DOMAIN[k])


def is_ops(name: str) -> bool:
    return bool(re.match(r"^v3-f1[4-7]-", name))


__all__ = ["GENERATORS", "register", "is_ops", "uids", "fleet_with"]
