"""Scoring v3's fault needs and open faults (eval/SCORING-V3.md 2.4, extended for F11-F13 by
choices 27 and 28): v2's low reserve, in every domain, and the faults the v3 families script (a
timeline's `step`s: GNSS jam and spoof, battery, reboot, stop) as faults open on a vehicle, so a
hold or an rtl on it is a fault response (v2's label) and every action on it carries `also_fault`.
And a diagnostic of no need: the vehicles lost by the end (`lost_vehicles`).

- **Low reserve** (v2's rule, version 2: eval/headless/v2/labels.py `fault_needs`), for a record
  graded under v0 (settings.py `of_record`: every run before decision rules 33): opens when the
  vehicle's true battery is at or under 35 % (v0's `low_reserve_open_pct`, scaled by the
  multiverse's `Params.low_reserve_open_pct` over its default); met when the vehicle is **safe at
  home** at a sample from the opening and before its battery reaches the reserve (v0's battery-now
  20 %); unmet when the reserve comes first; censored when neither by the end.
- **Low reserve, version 2** (stage S6, a record whose run logged its settings: rules 33 on): the
  need opens when the engine would refuse the vehicle a further task for reserves (crates/
  murmur-decide rules.rs `reserves`: endurance left after the task under `reserve_pct`), taken at
  the vehicle's own spot: its true battery under `reserve_pct` plus one `task_duration_s` at its
  catalog endurance (sim/platforms.toml `endurance_h`), or, with no endurance figure, under
  `reserve_pct` itself (the engine's battery-now rule); the multiverse scales that line as v0's.
  It is unmet when the battery reaches `reserve_pct` away from home, met and censored as v0's.
  Safe at home (choice 27): an aircraft as v2 has it, on the
  ground (disarmed) within 150 m of the home it had at its last armed sample, after having flown; a
  boat, a sub or a UGV within 150 m of its home and stopped there (disarmed, or under 0.5 m/s for
  `STOPPED_S`), a sub at the surface (above 1 m deep), after having moved (armed) since the episode
  began. A Rover's rtl ends holding at home, armed: v2's "on the ground" would never meet a boat's.
- **Open faults** (v2's `_open_faults`: a stuck alert or the script's stop, a low reserve open, a
  link cut or lost; and choice 28): a timeline `stop` from its time on; a `reboot` from its time
  until the vehicle is armed again (on the surface it boots disarmed and drops its task) or 60 s
  for one that comes back armed; a `battery` fault from its time until the vehicle is safe at home;
  a GNSS `spoof` on its vehicles from its time for its duration; a GNSS `jam` on every vehicle
  truly inside its circle while it is on.
"""

from __future__ import annotations

from functools import lru_cache
from typing import Any

from eval.world import load_world

from ..labels import dist_m
from ..v2 import labels as V2
from ..v2.labels import Params, instrument, landed_at_home
from ..v2.record import Picture
from .settings import Settings

#: a vehicle that is not an aircraft is at rest when slower than this for this long
STOPPED_MPS = 0.5
STOPPED_S = 10.0
#: a sub is at the surface above this depth, m
SURFACE_M = -1.0
#: a reboot that comes back armed (an aircraft's watchdog restore) is a fault for this long
REBOOT_OPEN_S = 60.0
#: the low-reserve need of a record graded under its logged settings (module doc)
LOW_RESERVE_V2 = {"name": "v3.low_reserve", "version": "2"}


@lru_cache(maxsize=1)
def _catalog_domains() -> dict[str, str]:
    import tomllib

    from .. import REPO

    cat = tomllib.loads((REPO / "sim" / "platforms.toml").read_text())
    return {k: str(v.get("domain") or "air") for k, v in cat.items() if isinstance(v, dict)}


@lru_cache(maxsize=1)
def _catalog_endurance_s() -> dict[str, float]:
    """Each platform's catalog endurance at cruise, s (sim/platforms.toml `endurance_h`; absent or
    not positive: none, as the engine's reserves check reads it)."""
    import tomllib

    from .. import REPO

    cat = tomllib.loads((REPO / "sim" / "platforms.toml").read_text())
    return {k: 3600.0 * float(v["endurance_h"]) for k, v in cat.items()
            if isinstance(v, dict) and isinstance(v.get("endurance_h"), (int, float)) and v["endurance_h"] > 0}


def platform_domains(pic: Picture) -> dict[str, str]:
    """Each vehicle's domain, from the catalog by its platform (sim/platforms.toml)."""
    cat = _catalog_domains()
    return {str(v["uid"]): cat.get(str(v.get("platform")), "air") for v in pic.fleet}


def safe_at_home(series: list[tuple[float, dict[str, Any]]], t_from: float, domain: str) -> float | None:
    """The first sample at or after `t_from` with the vehicle safe at home (module doc)."""
    if domain == "air":
        return landed_at_home(series, t_from)
    moved = False
    since: float | None = None
    for t, v in series:
        if v.get("armed"):
            moved = True
        h = v.get("home")
        here = (moved and h and h[0] is not None and v.get("lon") is not None
                and dist_m((v["lon"], v["lat"]), (h[0], h[1])) <= V2.HOME_M
                and (domain != "subsurface" or float(v.get("alt_m") or 0.0) > SURFACE_M))
        still = v.get("armed") is False or float(v.get("speed_mps") or 0.0) < STOPPED_MPS
        if not (here and still):
            since = None
            continue
        since = t if since is None else since
        if v.get("armed") is False or t - since >= STOPPED_S:
            if t >= t_from:
                return t
    return None


def fault_needs(rec: dict[str, Any], pic: Picture, p: Params, s: Settings) -> list[dict[str, Any]]:
    """Low reserve per vehicle in every domain (module doc), on the record's settings `s`: v0's
    rule with the domain's safe at home (an aircraft's need is v2's, value for value), or version 2
    for a record graded under its logged settings."""
    doms = platform_domains(pic)
    plats = {str(v["uid"]): str(v.get("platform")) for v in pic.fleet}
    scale = p.low_reserve_open_pct / Params().low_reserve_open_pct
    reserve = s.reserve_now()
    out = []
    for uid in pic.uids():
        dom = doms.get(str(uid), "air")
        series = pic.vehicle_series(uid)
        if s.v0 is not None:
            open_pct = p.low_reserve_open_pct * (s.v0.low_reserve_open_pct / Params().low_reserve_open_pct)
            t_open = next((t for t, v in series if v.get("battery_pct") is not None
                           and v["battery_pct"] <= open_pct), None)
        else:
            endurance = _catalog_endurance_s().get(plats.get(str(uid), ""))
            task_s = float(s.task_duration_s or 0.0)
            open_pct = scale * (reserve + (100.0 * task_s / endurance if endurance else 0.0))
            t_open = next((t for t, v in series if v.get("battery_pct") is not None
                           and v["battery_pct"] < open_pct), None)
        if t_open is None:
            continue
        t_reserve = next((t for t, v in series if t >= t_open and v.get("battery_pct") is not None
                          and v["battery_pct"] <= reserve), None)
        t_home = safe_at_home(series, t_open, dom)
        met = t_home is not None and (t_reserve is None or t_home < t_reserve)
        status = "met" if met else ("unmet" if t_reserve is not None else "censored")
        if s.v0 is not None:
            inst = instrument("fault_need") if dom == "air" else {"name": "v3.fault_need", "version": "3"}
        else:
            inst = dict(LOW_RESERVE_V2)
        row = {"kind": "low_reserve", "uid": int(uid) if str(uid).isdigit() else uid, "domain": dom,
               "t_open": t_open, "t_reserve": t_reserve, "t_met": t_home if met else None,
               "met_by": ("on the ground at home" if dom == "air" else "stopped at home") if met else None,
               "t_landed": t_home, "t_closed": t_home, "status": status, "open_pct": open_pct,
               "instrument": inst}
        if s.v0 is None:
            row["reserve_pct"] = reserve
        out.append(row)
    return out


# ---- open faults ----------------------------------------------------------------------------------

def steps(rec: dict[str, Any]) -> list[dict[str, Any]]:
    """The timeline's fault steps the script ran (script.py's log: `step_kind`, `uids`, `t`)."""
    return [s for s in rec.get("script") or [] if s.get("kind") == "step" and s.get("step_kind")
            in ("stop", "reboot", "battery", "gnss") and not s.get("error")]


def stated_step(rec: dict[str, Any], sid: str | None) -> dict[str, Any]:
    """A step's parameters as the situation stated them (its circle, its duration)."""
    for s in ((rec.get("truth") or {}).get("stated") or {}).get("script") or []:
        if s.get("id") == sid:
            return s.get("params") or {}
    return {}


def _step_uids(s: dict[str, Any]) -> list[str]:
    u = s.get("uids")
    return [str(x) for x in u] if isinstance(u, list) else []


def step_faults(rec: dict[str, Any], pic: Picture, uid: str, t: float) -> list[str]:
    """The scripted faults open on vehicle `uid` at `t` (module doc)."""
    out = []
    k = pic.index(t)
    for s in steps(rec):
        t0 = float(s.get("t") or 0.0)
        if t < t0:
            continue
        kind = s["step_kind"]
        if kind in ("stop", "battery") and uid in _step_uids(s):
            if kind == "battery":
                home = safe_at_home([(tt, v) for tt, v in pic.vehicle_series(uid) if tt >= t0], t0,
                                    platform_domains(pic).get(uid, "air"))
                if home is not None and home <= t:
                    continue
            out.append(f"{'stuck' if kind == 'stop' else 'battery'}:{uid}")
        elif kind == "reboot" and uid in _step_uids(s):
            back = next((tt for tt, v in pic.vehicle_series(uid) if tt > t0 + 1.0 and v.get("armed")), None)
            first = next((v for tt, v in pic.vehicle_series(uid) if tt > t0), None)
            end = back if (first is not None and not first.get("armed")) else t0 + REBOOT_OPEN_S
            if end is None or t < end:
                out.append(f"reboot:{uid}")
        elif kind == "gnss":
            params = stated_step(rec, str(s.get("step") or ""))
            dur = params.get("duration_s")
            if dur is not None and t >= t0 + float(dur):
                continue
            if s.get("effect") == "spoof" and uid in _step_uids(s):
                out.append(f"gnss_spoof:{uid}")
            elif s.get("effect") == "jam":
                c = params.get("circle") or {}
                v = pic.vehicle(uid, k)
                if c and v and v.get("lon") is not None and \
                        dist_m((v["lon"], v["lat"]), (float(c["lon"]), float(c["lat"]))) <= float(c["radius_m"]):
                    out.append(f"gnss_jam:{uid}")
    return out


def open_faults(a: dict[str, Any], pic: Picture, rec: dict[str, Any], fneeds: list[dict[str, Any]]) -> list[str]:
    """v2's open faults on an action's vehicles at its creation, and the timeline's (module doc)."""
    out = list(V2._open_faults(a, pic, rec, fneeds))
    t_a = a.get("created")
    if t_a is None:
        return out
    for uid in a["uids"]:
        for f in step_faults(rec, pic, str(uid), float(t_a)):
            if f not in out:
                out.append(f)
    return out


# ---- a diagnostic: the vehicles lost --------------------------------------------------------------

def lost_vehicles(rec: dict[str, Any], pic: Picture) -> list[dict[str, Any]]:
    """Our vehicles lost by the episode's end (no need: the mission names only the reserve,
    choice 29): an aircraft on the ground or the water (disarmed) more than 150 m from its home after
    having flown; a boat or a sub whose true position at the end is out of the water (the world's
    land/water grid: under 0.5 m, aground)."""
    from .families import grids

    doms = platform_domains(pic)
    plat = {str(v["uid"]): str(v.get("platform")) for v in pic.fleet}
    world = str((rec.get("world") or {}).get("name") or "golden-gate-v3")
    try:
        depth, _ = grids(world)
        load_world(world)
    except Exception:  # noqa: BLE001 (a world without grids: no aground check)
        depth = None
    out = []
    for uid in pic.uids():
        series = pic.vehicle_series(uid)
        if not series:
            continue
        flew = any(v.get("armed") for _, v in series)
        t, v = series[-1]
        dom = doms.get(str(uid), "air")
        h = v.get("home")
        if dom == "air" and flew and v.get("armed") is False and h and h[0] is not None and v.get("lon") is not None \
                and dist_m((v["lon"], v["lat"]), (h[0], h[1])) > V2.HOME_M:
            out.append({"uid": int(uid), "platform": plat.get(str(uid)), "why": "down away from home",
                        "t": t, "from_home_m": round(dist_m((v["lon"], v["lat"]), (h[0], h[1])))})
        elif dom in ("surface", "subsurface") and depth is not None and v.get("lon") is not None \
                and plat.get(str(uid)) != "buoy":
            d = depth.at((v["lon"], v["lat"]))
            if d is not None and d < 0.5:
                out.append({"uid": int(uid), "platform": plat.get(str(uid)), "why": "aground", "t": t,
                            "depth_m": round(d, 1)})
    return out
