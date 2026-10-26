"""Which of our vehicle types could answer each need (eval/SCORING-V3.md 10.3), asked of the physical
sensing model itself (`murmur-headless --sense`, eval/headless/sensing.py), per need, under the
situation's drawn conditions: the oracle's own reach (oracle.py `reach_of`: the model's
identification range, recognition under water, for the platform at its working altitude, a pointed
sensor's Pd 0.5, in open geometry at the mission's centre, times `REACH_SHARE`), for every type in
the world's fleet that takes orders and carries the need's domain sensor (no drifting buoy, no
tracker; the high aircraft left on their standing orders, as the oracle leaves them; no aircraft for
a need under water), whether or not the oracle ever weighed it. A type can answer when that reach
is at least `CAPABLE_M`. MODEL RESULTS ONLY.

The first dev check read this from the oracle's own log of reaches, which holds only the vehicles
the oracle weighed while the need was open: too few for a need resolved before it opened, a
situation of several needs, or a vehicle out of reach of its link (F11, F12).
"""

from __future__ import annotations

import atexit
from functools import lru_cache
from typing import Any

from eval.world import load_world

from ..situations import at_unix, fleet_plan
from .mission import rule

REACH_SHARE = 0.7
CAPABLE_M = {"eo_ir": 150.0, "sonar": 35.0}
HIGH_ALT_M = 2000.0
NOT_ORDERED = ("drifter", "tracker")

_model: Any = None


def model() -> Any:
    global _model
    if _model is None:
        from ..sensing import SenseModel

        _model = SenseModel()
        atexit.register(_model.close)
    return _model


@lru_cache(maxsize=None)
def movers(world: str) -> tuple[tuple[str, str, float, tuple[str, ...]], ...]:
    """(platform, domain, working altitude, sensor types) of each type of the world's fleet that
    takes orders, the high aircraft apart (one entry a type: the first unit's altitude)."""
    _, standins = fleet_plan(load_world(world).fleet)
    out: dict[str, tuple[str, str, float, tuple[str, ...]]] = {}
    for s in standins:
        spec = s["spec"]
        if spec.get("behavior") in NOT_ORDERED or s["platform"] in out:
            continue
        alt = float(s.get("cruise_alt_m") or spec.get("alt_m") or spec.get("typical_alt_m") or 0.0)
        if spec.get("domain") == "air" and float(spec.get("alt_m") or alt) >= HIGH_ALT_M:
            continue
        sensors = tuple(sorted({str(x.get("type")) for x in spec.get("sensors") or []}))
        out[s["platform"]] = (s["platform"], str(spec.get("domain")), alt, sensors)
    return tuple(out.values())


def conditions(rec: dict[str, Any]) -> dict[str, Any]:
    c = (rec.get("world") or {}).get("conditions") or {}
    out: dict[str, Any] = {}
    if c.get("visibility_km") is not None:
        out["visibility_m"] = 1000.0 * float(c["visibility_km"])
    for k in ("sea_state", "wind_mps"):
        if c.get(k) is not None:
            out[k] = float(c[k])
    return out


def stated_targets(rec: dict[str, Any]) -> dict[str, dict[str, Any]]:
    """Each added contact's altitude (a depth below the water) and speed, as the situation stated
    them (F1-F6's `element`, F7-F13's `elements`)."""
    st = (rec.get("truth") or {}).get("stated") or {}
    out = {}
    for e in ([st["element"]] if st.get("element") else []) + list(st.get("elements") or []):
        alt = e.get("alt_m")
        if alt is None and e.get("depth_m") is not None:
            alt = -float(e["depth_m"])
        out[str(e.get("contact"))] = {"alt_m": float(alt or 0.0), "speed_mps": float(e.get("speed_mps") or 0.0)}
    return out


def able(rec: dict[str, Any], needs: list[dict[str, Any]]) -> dict[str, list[str]]:
    """Per need (its contact), the platform types that could answer it (module doc)."""
    world = str((rec.get("world") or {}).get("name") or "golden-gate-v3")
    w = load_world(world)
    lon, lat = w.mission.center
    at = (rec.get("world") or {}).get("conditions", {}).get("at")
    cond = conditions(rec)
    targets = stated_targets(rec)
    out: dict[str, list[str]] = {}
    for n in needs:
        r = rule(n["domain"])
        sensor = r.sensors[0]
        level = 2 if r.level == "identify" else 1
        tgt = targets.get(n["contact"], {"alt_m": 0.0, "speed_mps": 0.0})
        types = []
        for platform, dom, alt, sensors in movers(world):
            if sensor not in sensors or (n["domain"] == "subsurface" and dom == "air"):
                continue
            req: dict[str, Any] = {"observer": {"platform": platform, "lat": lat - 0.0005, "lon": lon, "alt_m": alt},
                                   "target": {"kind": n["kind"], "domain": n["domain"], "lat": lat, "lon": lon,
                                              "alt_m": tgt["alt_m"], "speed_mps": tgt["speed_mps"]},
                                   "sensor": sensor, "conditions": cond, "ranges": True}
            if at:
                req["t"] = at_unix(at) + float(n["t_need"])
            if w.bathymetry_grid is not None:
                from .. import REPO

                req["bathymetry"] = str(w.bathymetry_grid.relative_to(REPO))
            best = 0.0
            for q in model().ask(req).get("predictions") or []:
                rm = q.get("ranges_m") or [0.0, 0.0, 0.0]
                best = max(best, float(rm[level]), float(rm[2]))
            if REACH_SHARE * best >= CAPABLE_M[sensor]:
                types.append(platform)
        out[n["contact"]] = sorted(types)
    return out
