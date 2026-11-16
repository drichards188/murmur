"""Scoring v3's labels (eval/SCORING-V3.md section 2), from one v3 episode record (its picture samples
under `v3`) and, for `useful` and `duplicate`, its paired `none` replicate. v2's instruments are
imported where v3 keeps them unchanged (eval/headless/v2/labels.py: the actions from the audit log,
their runs, fault needs, the reserve, the precedence); what changes is written here:

- **The settings** (docs/SCENARIO-SETTINGS.md, stage S6): every value below that was a constant is
  the record's own (settings.py `of_record`: the settings its run logged, rules 33 on, else v0's for
  its world, which hold the values every earlier run was graded by): the circle, each domain's
  grace, sensors and level, the bar, the approach horizon, the no-velocity and still speeds, the
  look distance and the reserve. The multiverse (`Params`) scales the record's own horizon and look
  distance by its value over v2's default (`effective`), as it scales each domain's grace by its
  `late_s`, so a record of v0's is labelled exactly as before.
- **Contact needs per domain** (2.1, 2.2): the settings' circle, the domain's grace, sensor and
  level. A need is **resolved at t** when the track the tracker associates with the contact shows,
  in the picture sample at t, the contact's true kind (or, where the domain's level is recognition,
  its true family) at the bar or more (v0's 0.6), without an identity conflict, and `supported_by`
  holds the domain's sensor (or AIS, for a surface contact that truly broadcasts it).
- **Sensing error** (2.1): every vehicle of the fleet carrying a sensor that could resolve the
  contact has its wrong-call draw set at the deciding level (the physical model's draw: per observer,
  sensor, contact and level, crates/murmur-bridge tracks/physical/mod.rs `label_at`, ported in
  `wrong_call`), so no look could resolve it. Rare by construction (it needs every observer wrong);
  a need **poisoned** by one wrong observer (a conflict at the deciding level) is kept, and named as
  its cause in the dev check (causes.py).
- **Contribution** (2.3): v2's rule, with the need's domain's sensors in place of EO/IR and sonar: the
  action's vehicle raised its hits on the need's track with a sensor that supports the domain, while
  its vehicle flew that order, before the need was identified.
- **Action labels** (2.5) for the moving kinds (orbit, patrol, track, identify, hold, rtl): v2's
  precedence and rules (credit for effect only; none after the need was identified; `duplicate` when
  the `none` replicate also served the need; harm by geometry), with the world's centre, the domain's
  sensors, and a `track` order pointed at the contact its track names. An **identify** (decision
  rules 21; choices 37 to 39) is aimed at the contact its track names, judged on the picture by that
  track, run only while it looks (to the look's end as the record shows it, `look_end`: the vehicle
  then goes back to the order it replaced, under the identify's id), and judged for harm (a) at its
  track's shown position.
- **An order that served no need** (2.5 item 6, the owner's decision of 2026-11-15, choice 40; v2's
  `useless`): **justified** when the picture at its creation supported it (`supported_on_picture`:
  its track in doubt and inside the circle or coming into it within the horizon; an aircraft shown
  with no velocity taken to come straight in at the air threat speed, the honest ceiling's rule) or
  it did something useful (`cleared_doubt`: its vehicle's own sensor of the aimed contact's domain
  raised its hits on the contact's track while it flew the order, and the track, in doubt when the
  order was made, reached the mission's level by the order's end), or, for a hold or an rtl of F14's
  stated run's vehicle ahead of the run's predicted outage, the stated tasking supported it
  (labels_ops.py `tasked_hold`, choice 41); **not_justified** otherwise. A collect or a cover is
  split as labels_ops.py `space_label` says, on the context the agent could see (a stated need's
  area, a predicted outage).
- **Nominations** (`engage`, 2.8) and **review decisions** (2.9), judged against truth: their own
  labels (`nomination_labels`, `review_labels`), and **collect** and **cover** (2.10): built in
  labels_ops.py with F14-F16's own needs (`ops` in the labels, where a situation states them).
- **A spoofed AIS** (2.2, choice 30; F8's spoofer): AIS supports only a contact's own true
  broadcast, and a contact whose AIS is spoofed is resolved when the picture shows its true kind
  from EO/IR at 0.6 or more, the only conflict allowed being the AIS's own (the spoof exposed).
- **Fault needs** (2.4, choices 27 and 28; faults.py): v2's low reserve in every domain, and the
  timeline's faults (stop, reboot, battery, GNSS jam and spoof) as faults open on a vehicle.
"""

from __future__ import annotations

import math
from dataclasses import replace
from typing import Any

from ..labels import Truth, dist_m
from ..v2 import labels as V2
from ..v2.labels import Params, geometry, geometry_dist
from ..v2.labels import instrument as _v2_instrument
from ..v2.record import Picture, carry
from . import labels_ops as OPS
from .faults import fault_needs as fault_needs_v3
from .faults import open_faults
from .settings import Settings, of_record

#: scoring v3 (the labels' format); each instrument carries its own version (`INSTRUMENT_VERSIONS`)
VERSION = "3"
END_MARGIN_S = 10.0
NEED_AFFILIATIONS = ("unknown", "suspect", "hostile")
#: every sensor type that can classify (aim by hits: a rise of any of them names the contact)
CLASSIFYING = ("eo_ir", "sonar")
#: the physical model's families (crates/murmur-bridge tracks/sensing.rs `family`)
FAMILY = {"cargo_ship": "large_vessel", "tanker": "large_vessel", "ferry": "large_vessel", "patrol_boat": "large_vessel",
          "fishing_vessel": "small_craft", "sailboat": "small_craft", "small_fast_boat": "small_craft",
          "light_aircraft": "aircraft", "helicopter": "aircraft", "unknown_uas": "small_uas", "small_drone": "small_uas",
          "submarine": "submerged", "uuv": "submerged", "periscope": "submerged", "vehicle": "ground_vehicle",
          "truck": "ground_vehicle", "diver": "person", "person": "person"}
#: tracks/physical/mod.rs `level_rate`: a recognition is wrong twice as often as an identification
LEVEL_INDEX = {"detect": 0, "recognise": 1, "identify": 2}
SENSOR_INDEX = {"radar": 0, "eo_ir": 1, "ais": 2, "sonar": 3, "esm": 4}
FRIENDLY_OR_NEUTRAL = ("neutral", "friend", "assumed_friend", "assumed friend")
#: the kinds whose order names one track (`params.track`, or `track_id`): the antenna tracker's
#: `track`, and an identify (decision rules 21: one vehicle sent to identify one track)
NAMED_KINDS = ("track", "identify")
#: the kinds an op `done` does not end: v2's (an orbit, a patrol or a track is done once established
#: and flies on), and an identify, whose look only the engine's step ends (rules 21)
CONTINUING_KINDS = V2.CONTINUING_KINDS + ("identify",)
#: the kinds harm (a) judges: v2's orbit and patrol, and an identify, flown as an orbit round its track
SENDING_KINDS = V2.MOVE_KINDS + ("identify",)
#: the bridge's alerts on an identify order whose look ended unidentified (out of time, its track
#: lost) or identified with nothing to go back to (crates/murmur-bridge/src/decision/identify.rs
#: `identify_alerts`): each dated from the op then in force, the one sent at the look's end
IDENTIFY_END_ALERTS = ("identify_timed_out", "identify_lost", "identify_holding")
#: an intent's terminal states once dispatched (crates/murmur-decide/src/intent.rs `is_terminal`)
INTENT_ENDS = ("done", "failed", "aborted", "voided")
INSTRUMENTS = {
    "contact_need": "v3.contact_need", "sensing_error": "v3.sensing_error", "identity_resolved": "v3.identity_resolved",
    "contribution": "v3.contribution", "fault_need": "v2.fault_need", "action": "v3.action_label", "aim": "v3.aim",
    "supported": "v3.supported_on_picture", "cleared": "v3.cleared_doubt", "nomination": "v3.nomination",
    "review": "v3.review_decision",
}
#: an instrument's version where it is not scoring v3's own (`VERSION`): version 4 since 2026-11-15
#: (choice 40): the action labels split an order that served no need into `justified` and
#: `not_justified` (v2's `useless`); the support on the picture (until then `v3.justified_on_picture`,
#: version 3) takes an aircraft shown with no velocity to come straight in at the threat speed; and
#: `cleared_doubt` is new
INSTRUMENT_VERSIONS = {"action": "4", "supported": "4", "cleared": "4"}
#: the labels of an order that served no need (2.5 item 6), and why one is justified (`justified_by`):
#: the picture supported it (`supported_on_picture`), it cleared a doubt (`cleared_doubt`), or the
#: stated tasking supported it (a collect or a cover, labels_ops.py `no_need`; a hold or an rtl of
#: F14's run's vehicle ahead of its predicted outage, `tasked_hold`, choice 41)
NO_NEED_LABELS = ("justified", "not_justified")
JUSTIFIED_BY = ("picture", "cleared", "tasking")


def instrument(name: str) -> dict[str, str]:
    if INSTRUMENTS[name].startswith("v2."):
        return _v2_instrument(name)
    return {"name": INSTRUMENTS[name], "version": INSTRUMENT_VERSIONS.get(name, VERSION)}


def picture(rec: dict[str, Any]) -> Picture:
    v3 = rec.get("v3") or {}
    if not v3.get("picture"):
        raise ValueError("this record has no v3 picture samples (not run with scoring v3)")
    return Picture(v3["picture"])


def effective(p: Params, s: Settings) -> Params:
    """The multiverse's choices on a record's own settings: its horizon and look distance scaled by
    the multiverse's value over v2's default (a record of v0's, whose values are v2's defaults, gets
    `p` exactly); the late share and the low-reserve line as given (faults.py scales the latter)."""
    d = Params()

    def scaled(x: float, value: float, default: float) -> float:
        return x if value == default else x * value / default

    return replace(p, justify_horizon_s=scaled(p.justify_horizon_s, s.approach_horizon_s, d.justify_horizon_s),
                   aim_fallback_m=scaled(p.aim_fallback_m, s.look_m, d.aim_fallback_m))


def as_v2(rec: dict[str, Any]) -> dict[str, Any]:
    """The record as v2's functions read it (their audit digest is under `v2`)."""
    return {**rec, "v2": rec.get("v3") or {}}


def world_of(rec: dict[str, Any]) -> str:
    return str((rec.get("world") or {}).get("name") or ((rec.get("truth") or {}).get("stated") or {}).get("world")
               or "golden-gate-v3")


def _end_s(rec: dict[str, Any]) -> float:
    return float((rec.get("clock") or {}).get("duration_s") or 900.0)


# ---- the physical model's wrong calls (tracks/physical/mod.rs `label_at`) ---------------------

def salt(si: int, sensor: str, what: int) -> int:
    """physical/mod.rs `salt`: the sensor's index and type and what the draw is for."""
    return 0x5000_0000 | (what << 16) | (si << 8) | SENSOR_INDEX[sensor]


def wrong_call(seed: int, rate: float, uid: int, si: int, sensor: str, contact: str, level: str) -> bool:
    """Whether this observer's sensor calls this contact wrong at this level for the run."""
    lr = {"detect": 0.0, "recognise": min(2.0 * rate, 0.5), "identify": rate}[level]
    if lr <= 0.0:
        return False
    u = V2.uniform((int(seed) ^ 0x9E37) & ((1 << 64) - 1), -1, uid, contact, salt(si, sensor, 8 + LEVEL_INDEX[level]))
    return u < lr


def can_hold(sensor: str, domain: str) -> bool:
    """Whether a sensor type can resolve a contact of a domain at all (EO/IR: anything above the
    water; sonar: on or under the water)."""
    if sensor == "eo_ir":
        return domain != "subsurface"
    if sensor == "sonar":
        return domain in ("surface", "subsurface")
    return False


def sensing_errors(pic: Picture, truth: Truth, settings: Settings) -> dict[str, dict[str, Any]]:
    """Per contact: every observer and sensor of the fleet that could resolve it under the domain's
    rule (the settings'), and whether its draw is wrong at the deciding level (identification; for a
    recognition rule, a passive sonar's identification counts too, so either level resolves it)."""
    s = pic.sensing
    seed, rate = s.get("seed"), float(s.get("misclassification_rate") or 0.0)
    out = {}
    for cid, m in sorted(truth.meta.items()):
        domain = str(m.get("domain") or "surface")
        r = settings.rule(domain)
        holders = []
        for v in pic.fleet:
            for si, x in enumerate(v.get("sensors") or []):
                st = str(x.get("type"))
                if st not in r.sensors or not can_hold(st, domain):
                    continue
                levels = ["identify"] if r.level == "identify" else ["recognise", "identify"]
                wrong = {lv: (wrong_call(int(seed), rate, int(v["uid"]), si, st, cid, lv) if seed is not None else None)
                         for lv in levels}
                holders.append({"uid": int(v["uid"]), "si": si, "sensor": st, "wrong": wrong})
        error = bool(holders) and seed is not None and all(all(h["wrong"].values())
                                                          for h in holders)
        out[cid] = {"error": error, "holders": len(holders),
                    "wrong_observers": sorted({h["uid"] for h in holders if any(h["wrong"].values())}),
                    "seed": seed, "rate": rate, "instrument": instrument("sensing_error")}
    return out


# ---- 2.1, 2.2: contact needs ----------------------------------------------------------------------

def shown_ok(kind_shown: str | None, meta: dict[str, Any], s: Settings) -> bool:
    """The shown kind is the contact's true kind, or its true family where the domain's level is
    recognition."""
    true = str(meta.get("kind") or "")
    if kind_shown == true:
        return True
    return s.rule(meta.get("domain")).level == "recognise" and kind_shown is not None and kind_shown == FAMILY.get(true)


def ais_spoofed(meta: dict[str, Any]) -> bool:
    """The contact broadcasts an AIS identity that is not its own (truth's `ais.spoofed`)."""
    a = meta.get("ais")
    return isinstance(a, dict) and bool(a.get("spoofed"))


def mark_spoofed(rec: dict[str, Any], truth: Truth) -> Truth:
    """The record's truth says only whether a contact broadcasts AIS (`ais`: true); the situation
    states which broadcast an identity not their own (families_f7_f13.py `ais_spoofed`): their
    `ais` becomes `{"spoofed": true}` here."""
    for cid in ((rec.get("truth") or {}).get("stated") or {}).get("ais_spoofed") or []:
        if cid in truth.meta:
            truth.meta[cid] = {**truth.meta[cid], "ais": {"spoofed": True}}
    return truth


def ais_true(meta: dict[str, Any]) -> bool:
    """The contact truly broadcasts its own AIS identity."""
    return bool(meta.get("ais")) and not ais_spoofed(meta)


#: how the tracker words a conflict between the AIS identity and the looks (tracks/model.rs)
AIS_CONFLICT = "AIS says "


def supported(idn: dict[str, Any], meta: dict[str, Any], s: Settings) -> bool:
    r = s.rule(meta.get("domain"))
    sb = set(idn.get("supported_by") or [])
    return bool(sb & set(r.sensors)) or (r.ais_counts and "ais" in sb and ais_true(meta))


def resolved_at(pic: Picture, k: int | None, cid: str, meta: dict[str, Any], s: Settings) -> bool:
    """2.2 (v3): the associated track shows the true kind (or family, by the domain's level) at the
    bar or more, without a conflict, supported by the domain's sensor."""
    tid = pic.track_of(cid, k)
    if tid is None:
        return False
    idn = pic.identity(tid, k)
    if not idn or not shown_ok(idn["kind"], meta, s):
        return False
    # a spoofed AIS against the true identification is the spoof exposed, not a doubt (choice 30)
    if idn["conflict"] and not (ais_spoofed(meta) and str(idn["conflict"]).startswith(AIS_CONFLICT)):
        return False
    if idn["confidence"] is None or float(idn["confidence"]) < s.bar:
        return False
    return supported(idn, meta, s)


def circle(s: Settings) -> tuple[tuple[float, float], float]:
    """The settings' circle: its centre ([lon, lat]) and radius."""
    if s.area is None:
        raise ValueError("these settings name no circle ([watch.area])")
    return s.area.center, s.area.radius_m


def contact_needs(rec: dict[str, Any], pic: Picture, truth: Truth, p: Params,
                  s: Settings | None = None) -> list[dict[str, Any]]:
    s = s if s is not None else of_record(rec)
    end = _end_s(rec)
    center, radius = circle(s)
    errs = sensing_errors(pic, truth, s)
    out = []
    for cid, m in sorted(truth.meta.items()):
        aff = str(m.get("affiliation") or "").lower()
        if aff not in NEED_AFFILIATIONS:
            continue
        t_need = truth.first_within(cid, center, radius)
        if t_need is None:
            continue
        domain = str(m.get("domain") or "surface")
        grace = s.rule(domain).grace_s
        D = t_need + grace
        evaluable = D <= end - END_MARGIN_S
        se = errs[cid]
        k_need = pic.index(t_need)
        pre = resolved_at(pic, k_need, cid, m, s)
        t_served = t_need if pre else None
        t_identified = None
        if pre:
            assert k_need is not None
            k = k_need
            while k > 0 and resolved_at(pic, k - 1, cid, m, s):
                k -= 1
            t_identified = pic.time(k)
        else:
            k0 = pic.index_after(t_need)
            if k0 is not None:
                for k in range(k0, len(pic.times)):
                    if resolved_at(pic, k, cid, m, s):
                        t_served = t_identified = pic.time(k)
                        break
        served = t_served is not None and t_served <= D + 1e-9
        late_s = grace * p.late_s / V2.DEADLINE_S  # the multiverse's S' scales each domain's grace
        out.append({
            "contact": cid, "kind": m.get("kind"), "affiliation": aff, "ais": ais_true(m), "domain": domain,
            "ais_spoofed": ais_spoofed(m),
            "grace_s": grace, "t_need": round(t_need, 3), "deadline": round(D, 3), "evaluable": evaluable,
            "sensing_error": se["error"], "sensing": {k: se[k] for k in ("holders", "wrong_observers")},
            "eligible": evaluable and not se["error"], "pre_served": pre,
            "t_served": round(t_served, 3) if t_served is not None else None, "served": served,
            "t_identified": round(t_identified, 3) if t_identified is not None else None,
            "served_late": t_served is not None and t_served <= t_need + late_s + 1e-9,
            "time_to_serve": round(min(t_served - t_need, grace), 3) if served and t_served is not None else None,
            "instrument": instrument("contact_need"), "resolved_by": instrument("identity_resolved"),
        })
    return out


# ---- 2.3, 2.5: aim, contribution, the moving actions' labels ---------------------------------------

def need_sensors(n: dict[str, Any], s: Settings) -> tuple[str, ...]:
    """The sensors whose hits count as a contribution to a need: its domain's (AIS too for a surface
    contact that truly broadcasts it)."""
    r = s.rule(n.get("domain"))
    return tuple(r.sensors) + (("ais",) if r.ais_counts and n.get("ais") else ())


def named_track(a: dict[str, Any]) -> str | None:
    """The track a `track` or `identify` order names (`params.track_id`, or `track`)."""
    tid = (a.get("params") or {}).get("track_id") or (a.get("params") or {}).get("track")
    return str(tid) if tid else None


def track_target(a: dict[str, Any], pic: Picture) -> str | None:
    """The contact a `track` or `identify` order's track names at its creation (the tracker's
    association)."""
    tid = named_track(a)
    if not tid or a.get("created") is None:
        return None
    return pic.contact_of(tid, pic.index(a["created"]))


def look_end(a: dict[str, Any], uid: str, pic: Picture) -> float | None:
    """When an identify order's look on vehicle `uid` ended, as the record shows it (choice 38: the
    audit digest keeps no `identify.step`): the earliest of the bridge's end alert on the order
    (`IDENTIFY_END_ALERTS`, dated from the op sent at the look's end), the first picture sample after
    the order went out at which a look of the vehicle sent on its track holds identification (rules
    23: the look ends on its own identification, at any confidence, any sensor but AIS), and the
    intent's end (it comes once the vehicle has gone back, after the look). None when none shows."""
    t0 = (a["runs"].get(uid) or {}).get("start")
    if t0 is None:
        return None
    ends = [float(x["since_s"]) for x in (pic.enc.get("alerts") or {}).values()
            if x.get("kind") in IDENTIFY_END_ALERTS and x.get("intent") == a["intent"] and str(x.get("uid")) == uid
            and x.get("since_s") is not None and float(x["since_s"]) >= t0]
    tid = named_track(a)
    looks = ((pic.tracks.get(tid) or {}).get("looks") or {}) if tid else {}
    for key, rows in looks.items():
        u, sensor, level = key.split(":")
        if u == uid and level == "identify" and sensor != "ais":
            k = next((r[0] for r in rows if pic.time(r[0]) > t0), None)
            if k is not None:
                ends.append(pic.time(k))
    st = a.get("states") or {}
    ends += [float(st[s]) for s in INTENT_ENDS if st.get(s) is not None and float(st[s]) >= t0]
    return min(ends) if ends else None


def identify_geometry(a: dict[str, Any], pic: Picture) -> dict[str, Any] | None:
    """Where an identify sends its vehicle, for harm (a) (choice 39): its track's shown position when
    it was ordered, the centre of the orbit the engine lays out round it (the standoff is not in the
    record)."""
    tid = named_track(a)
    at = pic.predicted(tid, a["created"]) if tid and a.get("created") is not None else None
    return {"kind": "polyline", "points": [at]} if at is not None else None


def aim_of(a: dict[str, Any], pic: Picture, truth: Truth, needs: dict[str, dict[str, Any]], p: Params) -> dict[str, Any]:
    """v2's aim (by hits of any classifying sensor while the order ran, else by geometry), and for a
    `track` order without hits, the contact its track names. An identify is aimed at the contact its
    track names first (choice 37), and by v2's aim only when its track names none."""
    if a.get("kind") == "identify":
        c = track_target(a, pic)
        if c is not None:
            return {"by": "track", "contacts": [c], "primary": c, "ambiguous": False, "distance_m": None,
                    "instrument": instrument("aim")}
    out = V2.aim_of(a, pic, truth, needs, p)
    if out["by"] == "none" and a.get("kind") == "track":
        c = track_target(a, pic)
        if c is not None:
            out = {"by": "track", "contacts": [c], "primary": c, "ambiguous": False, "distance_m": None,
                   "instrument": instrument("aim")}
    out["instrument"] = instrument("aim")
    return out


def contributed(a: dict[str, Any], pic: Picture, needs: dict[str, dict[str, Any]], s: Settings) -> list[str]:
    """2.3 (v3): v2's, with each need's own domain sensors."""
    out = []
    for c, n in needs.items():
        if not n["served"] or n.get(V2.CONTRIBUTION_UNTIL) is None:
            continue
        sensors = need_sensors(n, s)
        for uid, run in a["runs"].items():
            if not run.get("flew"):
                continue
            t1 = run["own_start"]
            t2 = min(run["end"], n[V2.CONTRIBUTION_UNTIL])
            if t2 > t1 and pic.contact_hits_rose(c, uid, sensors, t1, t2):
                out.append(c)
                break
    return sorted(out, key=lambda c: (needs[c]["t_need"], c))


def in_doubt(tr: dict[str, Any], s: Settings) -> bool:
    """A track the picture shows in doubt (2.5): shown unknown, suspect or hostile, in identity
    conflict, or under the bar's identity confidence (or with none)."""
    idn = tr.get("identity") or {}
    return (tr.get("affiliation") in NEED_AFFILIATIONS or bool(idn.get("conflict"))
            or idn.get("confidence") is None or float(idn["confidence"]) < s.bar)


def approaches(kin: dict[str, Any] | None, domain: str | None, t_a: float, p: Params, center: tuple[float, float],
               radius_m: float, s: Settings) -> tuple[bool, str | None]:
    """Whether a track's shown motion brings it within the circle within the horizon (`p`'s, the
    record's own scaled by the multiverse: `effective`), and how that was judged: v2's rule (its
    shown position carried along its shown velocity from its last fix to `t_a`, then for up to the
    horizon in 5 s steps; a track with no velocity at its position only), except that a contact of a
    domain the settings give a no-velocity speed (`no_velocity_mps`: v0's air at 25 m/s) shown with
    no velocity (none, or under the settings' `still_mps`: the tracker feeds one without as 0.0 m/s)
    is taken to come straight in at that speed (the honest ceiling's rule, planner.py
    `HonestCeiling.motion`): it approaches when that run from its shown position reaches the circle
    within the horizon (choice 40)."""
    if not kin or kin["position"][0] is None:
        return False, None
    v = kin["velocity"]
    p0 = carry(kin["position"], v, t_a - float(kin["last_seen_s"] or t_a))
    speed = s.no_velocity(str(domain)) if domain is not None else None
    if speed is not None and (v is None or math.hypot(*v) < s.still_mps):
        return dist_m(p0, center) - radius_m <= speed * p.justify_horizon_s + 1e-9, "threat_speed"
    steps = int(p.justify_horizon_s // V2.JUSTIFY_STEP_S) if v else 0
    for j in range(steps + 1):
        if dist_m(carry(p0, v, j * V2.JUSTIFY_STEP_S), center) <= radius_m:
            return True, "velocity" if v else "position"
    return False, "velocity" if v else "position"


def supported_on_picture(a: dict[str, Any], aim: dict[str, Any], pic: Picture, truth: Truth, p: Params,
                         center: tuple[float, float], radius_m: float, s: Settings) -> dict[str, Any]:
    """Whether the picture at the order's creation supported it (2.5 item 6; v2's justification on
    the picture, about the world's own centre): the track nearest the aim (an identify's, the track it
    names, when the picture showed it then: choice 37) shown in doubt (`in_doubt`) and inside the
    circle or coming into it within the horizon (`approaches`: an aircraft with no velocity at the
    air threat speed, choice 40)."""
    t_a = a["created"]
    k = pic.index(t_a)
    out: dict[str, Any] = {"supported": False, "track": None, "instrument": instrument("supported"),
                           "horizon_s": p.justify_horizon_s}
    if k is None:
        return out
    tracks = pic.track_ids(k)
    if not tracks:
        return out
    tid = None
    named = named_track(a) if a.get("kind") == "identify" else None
    if named is not None and pic.present(named, k):
        tid = named
    target = aim.get("primary") or aim.get("nearest")
    if tid is None and target is not None:
        tid = pic.track_of(target, k)
        if tid is None:
            at = truth.at(target, t_a)
            if at is not None:
                tid = min(tracks, key=lambda x: dist_m(pic.predicted(x, t_a) or (0.0, 0.0), at))
    if tid is None:
        g = geometry(a["kind"], a["params"])
        if g is None:
            return out
        tid = min(tracks, key=lambda x: geometry_dist(pic.predicted(x, t_a) or (0.0, 0.0), g))
    tr = pic.track(tid, k) or {}
    doubt = in_doubt(tr, s)
    near, how = approaches(pic.kinematics(tid, k), tr.get("domain"), t_a, p, center, radius_m, s)
    out.update(supported=doubt and near, track=tid, shown={"affiliation": tr.get("affiliation"),
                                                           "identity": tr.get("identity") or {}},
               doubt=doubt, approaches=near, approach=how)
    return out


def cleared_doubt(a: dict[str, Any], aim: dict[str, Any], pic: Picture, truth: Truth, s: Settings) -> dict[str, Any]:
    """Whether the order did something useful (2.5 item 6, choice 40): it cleared a doubt the picture
    had, as the brief asks, whether or not its contact was a need. For a contact it was aimed at (an
    identify's or a `track` order's, the contact its track names; by hits, each contact whose hits
    rose while it ran; by geometry, the one it points at when that is not ambiguous): the contact's
    track was shown in doubt (`in_doubt`) and not at the mission's level (`resolved_at`: its true kind
    at 0.6 or more without conflict, supported by the domain's sensor; a class under the water) when
    the order was made; it reached that level at a picture sample after, within a run of the order
    (by the order's end; an identify's, its look's end); and its vehicle's own sensor of the
    contact's domain (2.2's support, as a contribution counts it: EO/IR, sonar under the water, AIS
    for a true broadcaster on the surface) raised its hits on the track from the start of that run
    to that sample."""
    out: dict[str, Any] = {"cleared": False, "contact": None, "instrument": instrument("cleared")}
    t_a = a.get("created")
    runs = [(uid, r) for uid, r in a["runs"].items() if r.get("flew")]
    if t_a is None or not runs:
        return out
    k_a = pic.index(t_a)
    if k_a is None:
        return out
    if aim.get("by") in ("hits", "track"):
        cands = list(aim.get("contacts") or [])
    else:
        cands = [aim["primary"]] if aim.get("primary") else []
    k_end = pic.index(max(float(r["end"]) for _, r in runs))
    for c in cands:
        m = truth.meta.get(c)
        tid = pic.track_of(c, k_a)
        if m is None or tid is None or not in_doubt(pic.track(tid, k_a) or {}, s) or resolved_at(pic, k_a, c, m, s):
            continue
        k_res = next((k for k in range(k_a + 1, (k_end or k_a) + 1) if resolved_at(pic, k, c, m, s)), None)
        if k_res is None:
            continue
        t_res = pic.time(k_res)
        r = s.rule(m.get("domain"))
        sensors = tuple(r.sensors) + (("ais",) if r.ais_counts and ais_true(m) else ())
        for uid, run in runs:
            if run["own_start"] <= t_res <= run["end"] and pic.contact_hits_rose(c, uid, sensors, run["own_start"], t_res):
                out.update(cleared=True, contact=c, track=tid, uid=uid, t_level=round(t_res, 3))
                return out
    return out


def pointed_at(a: dict[str, Any], aim: dict[str, Any], pic: Picture, truth: Truth, p: Params) -> set[str]:
    """v2's harm (b) spares (where the order sends its vehicle), and for a `track` or an `identify`
    order the contact its track names."""
    if a.get("kind") in NAMED_KINDS:
        c = track_target(a, pic)
        return {c} if c else set(aim.get("contacts") or [])
    return V2.pointed_at(a, geometry(a["kind"], a["params"]), aim, truth, p)


def harmful(a: dict[str, Any], aim: dict[str, Any], pic: Picture, needs: dict[str, dict[str, Any]], truth: Truth,
            p: Params, s: Settings) -> list[dict[str, Any]]:
    """v2's harm (a) and (b), with each need's own domain sensors for (b), and (a) for an identify
    too, at its track's shown position (choice 39), a vehicle under the settings' reserve
    (`reserve_now`: v0's 20 % now, a logged run's `reserve_pct`); (c), an engagement of a neutral or
    friend, is the nominations' (`nomination_labels`)."""
    out = []
    if not a["runs"]:
        return out
    g = identify_geometry(a, pic) if a["kind"] == "identify" else geometry(a["kind"], a["params"])
    spared: set[str] | None = None
    for uid, run in a["runs"].items():
        t_d = run["start"]
        v = pic.vehicle(uid, pic.index(t_d))
        if a["kind"] in SENDING_KINDS and v and v.get("battery_pct") is not None and v["battery_pct"] < s.reserve_now() \
                and g is not None and v.get("home") and v["home"][0] is not None \
                and V2.geometry_dist_home((v["home"][0], v["home"][1]), g) > V2.HOME_M:
            out.append({"kind": "a", "uid": uid, "battery_pct": v["battery_pct"]})
        for c, n in needs.items():
            if not n["eligible"] or n["served"] or not (n["t_need"] <= t_d <= n["deadline"]):
                continue
            if pic.contact_hits_rose(c, uid, need_sensors(n, s), t_d - V2.RISE_WINDOW_S, t_d):
                if spared is None:
                    spared = pointed_at(a, aim, pic, truth, p)
                if c not in spared:
                    out.append({"kind": "b", "uid": uid, "need": c})
    return out


#: the kinds labelled as v2 labels its orders (moving a vehicle, or holding or recalling it), and an
#: identify (2026-11-14, bd murmur-1zcp)
MOVING_KINDS = ("orbit", "patrol", "track", "identify", "hold", "rtl")
#: SCORING-V3.md 2.10 (labels_ops.py `space_label`)
SPACE_KINDS = ("collect", "cover")


def label_actions(rec: dict[str, Any], pic: Picture, truth: Truth, needs_list: list[dict[str, Any]],
                  fneeds: list[dict[str, Any]], none_served: dict[str, bool] | None, p: Params,
                  none_cover: dict[str, Any] | None = None, s: Settings | None = None) -> list[dict[str, Any]]:
    """Every action of the agent under test: the moving kinds by v2's precedence and rules (v3's
    domain sensors, centre and `track` aim, and an identify's own aim and run), nominations by
    `nomination_label`, collect and cover by 2.10 (labels_ops.py `space_label`; `none_cover`: the
    paired `none` replicate's coverage of the stated gap needs). `p` as `effective` gives it."""
    s = s if s is not None else of_record(rec)
    center, radius = circle(s)
    needs = {n["contact"]: n for n in needs_list}
    acts = V2.agent_actions(as_v2(rec), pic, continuing=CONTINUING_KINDS)
    for a in acts:
        # choice 38: an identify runs while it looks; after the look its vehicle flies the order the
        # identify replaced (or holds), under the identify's id, and that is not the identify's doing
        if a["kind"] != "identify":
            continue
        for uid, run in a["runs"].items():
            t = look_end(a, uid, pic)
            if t is not None and t < run["end"]:
                run["end"], run["end_why"] = t, "its look ended"
    credited: dict[str, tuple[int | None, float]] = {}
    credited_fault: set[int] = set()
    out = []
    for a in acts:
        empty_aim = {"by": "none", "contacts": [], "primary": None, "ambiguous": False, "distance_m": None,
                     "instrument": instrument("aim")}
        lab: dict[str, Any] = {k: a[k] for k in ("intent", "agent", "kind", "uids", "params", "created", "tier", "refused",
                                                 "card", "card_t", "final", "dispatched", "runs", "expires")}
        lab["approvals"], lab["views"], lab["error"] = a["approvals"], a["views"], a.get("error")
        lab["paired"] = none_served is not None
        lab["instrument"] = instrument("action")
        lab["contribution_instrument"] = instrument("contribution")
        unsupported = {"supported": False}
        if a["refused"] and a["kind"] != "cover":
            lab.update(aim=empty_aim, aimed_need=None, contributed=[], supported=unsupported, faults_open=[],
                       harm=[], label="refused")
            out.append(lab)
            continue
        if a["kind"] == "engage":
            lab.update(aim=empty_aim, aimed_need=None, contributed=[], supported=unsupported,
                       faults_open=open_faults(a, pic, rec, fneeds), harm=[])
            lab.update(nomination_label(a, rec, pic, truth))
            lab["also_fault"] = bool(lab["faults_open"])
            out.append(lab)
            continue
        if a["kind"] in SPACE_KINDS:
            # 2.10 (labels_ops.py), its own labels: a collect or a cover is judged against the stated
            # needs and the predicted outages the agent could see, never as a look at a contact (a cover
            # whose only failing check is coverage answered its question)
            lab.update(aim=empty_aim, aimed_need=None, contributed=[], supported=unsupported,
                       faults_open=open_faults(a, pic, rec, fneeds), harm=[])
            lab.update(OPS.space_label(a, rec, pic, none_cover))
            lab["also_fault"] = bool(lab["faults_open"])
            out.append(lab)
            continue
        if a["kind"] not in MOVING_KINDS:
            lab.update(aim=empty_aim, aimed_need=None, contributed=[], supported=unsupported,
                       faults_open=open_faults(a, pic, rec, fneeds), harm=[], label="unjudged",
                       unjudged_because=f"no v3 label for {a['kind']!r}")
            lab["also_fault"] = bool(lab["faults_open"])
            out.append(lab)
            continue
        aim = aim_of(a, pic, truth, needs, p)
        lab["aim"] = aim
        lab["aimed_need"] = V2.aimed_need(aim, needs)
        lab["contributed"] = contributed(a, pic, needs, s) if a["runs"] else []
        lab["supported"] = supported_on_picture(a, aim, pic, truth, p, center, radius, s)
        lab["faults_open"] = open_faults(a, pic, rec, fneeds)
        harm = harmful(a, aim, pic, needs, truth, p, s)
        lab["harm"] = harm
        lab["also_fault"] = bool(lab["faults_open"])
        if harm:
            label = "harmful"
        else:
            ns = none_served or {}
            paired = none_served is not None
            eligible = [c for c in lab["contributed"] if needs[c]["eligible"]]
            creditable = [c for c in eligible if paired and not ns.get(c, False)]
            dup = [c for c in eligible if paired and ns.get(c, False)]
            fresh = [c for c in creditable if c not in credited]
            met = V2._met_reserve(a, pic, fneeds, credited_fault) if a["kind"] == "rtl" else None
            if fresh or met is not None:
                label = "useful"
                for c in fresh:
                    credited[c] = (a["intent"], a["created"])
                if met is not None:
                    credited_fault.add(id(met))
                    lab["met_fault"] = {"uid": met["uid"], "t_met": met["t_met"]}
                lab["credited"] = fresh
            elif creditable:
                label = "follow_up"
                lab["follow_up_of"] = {c: credited[c][0] for c in creditable if c in credited}
            elif dup:
                label = "duplicate"
                lab["duplicate_of_none"] = dup
            elif lab["faults_open"] and a["kind"] in V2.RECOVERY_KINDS:
                label = "fault_response"
            else:
                c = lab["aimed_need"]
                n = needs.get(c) if c else None
                was_served = bool(n and ((n["t_served"] is not None and n["t_served"] <= (a["created"] or 0.0))
                                         or resolved_at(pic, pic.index(a["created"]), c, truth.meta.get(c) or {}, s)))
                was_credited = bool(c and c in credited and credited[c][1] is not None
                                    and credited[c][1] <= (a["created"] or 0.0))
                if was_served or was_credited:
                    label = "redundant"
                    lab["redundant_because"] = "served" if was_served else "credited"
                else:
                    # it served no need (choice 40): justified when the picture supported it or it
                    # cleared a doubt the picture had, or (a hold or an rtl of F14's stated run's
                    # vehicle ahead of its predicted outage) the stated tasking supported it (choice
                    # 41); not justified when none
                    lab["cleared"] = cleared_doubt(a, aim, pic, truth, s)
                    lab["tasked"] = OPS.tasked_hold(a, rec)
                    by = [w for w, ok in (("picture", lab["supported"]["supported"]),
                                          ("cleared", lab["cleared"]["cleared"]),
                                          ("tasking", lab["tasked"]["tasked"])) if ok]
                    label = "justified" if by else "not_justified"
                    lab["justified_by"] = by
        lab["label"] = label
        out.append(lab)
    return out


# ---- 2.8: nominations (engage), judged against truth -------------------------------------------------

NOMINATION_LABELS = ("refused", "harmful", "effective", "unexecuted", "unwarranted", "missed")


def nomination_label(a: dict[str, Any], rec: dict[str, Any], pic: Picture, truth: Truth) -> dict[str, Any]:
    """An agent's engagement nomination, judged against truth (SCORING-V3.md 2.8), in this
    precedence: `harmful` (its track named a contact of neutral or friend truth affiliation at its
    creation, or an applied effect had one inside its radius); `effective` (an effect was applied and
    took a contact of hostile truth out of play); `missed` (applied on a hostile, without taking it
    out); `unwarranted` (its target's truth affiliation is not hostile: unknown or suspect truth
    gives no ground to engage); `unexecuted` (a hostile target, but no effect was applied: declined,
    expired, aborted, superseded). Collateral in three dimensions, our own vehicles included
    (choices 16 and 17): labels_ops.py `nomination_label`."""
    return OPS.nomination_label(a, rec, pic, truth)


# ---- 2.9: review decisions, judged against truth --------------------------------------------------

def review_labels(rec: dict[str, Any], pic: Picture, truth: Truth) -> list[dict[str, Any]]:
    """Every review decision the record holds (the reviewer's, docs/HEADLESS.md 16.1, review.py), judged
    against truth (SCORING-V3.md 2.9): `right` or `wrong`; `no_effect` when the bridge refused it;
    `unjudged` when the record cannot say. A declaration or a flag: confirming or declaring is right
    when the track's contact is of hostile truth, revoking or dismissing when it is not. An AIS claim
    (`ais_claim`, on a lost track): confirming is right when the claimant's contact is the lost
    track's own (the claimant's track is read from the decision's `by_track`; a decision without
    one is `unjudged`). A lost contact accepted for
    an engagement (`lost_contact`): right when, by truth, its contact had left the world or is of
    hostile truth. An un-spend: right when the platform's effect did not take it out of the world.
    review.py records the claimant's track now (choice 9); a claimant that is the lost contact's
    stated return, a flag naming several tracks and a reviewer's decline or leave are read as
    choices 18-20 say (labels_ops.py `review_labels`)."""
    return OPS.review_labels(rec, pic, truth)


# ---- the whole episode -------------------------------------------------------------------------------

def none_served_of(none_labels: dict[str, Any] | None) -> dict[str, bool] | None:
    if none_labels is None:
        return None
    return {n["contact"]: bool(n["served"]) for n in none_labels["needs"]}


def episode_labels(rec: dict[str, Any], none: dict[str, Any] | None = None, params: Params | None = None,
                   none_labels: dict[str, Any] | None = None) -> dict[str, Any]:
    """Every label of a v3 episode; `none` (or `none_labels`) its paired `none` replicate's. Every
    value is the record's own settings' (`of_record`), the multiverse's `params` scaling them
    (`effective`)."""
    s = of_record(rec)
    p = effective(params or Params(), s)
    pic = picture(rec)
    truth = mark_spoofed(rec, Truth(rec))
    needs = contact_needs(rec, pic, truth, p, s)
    fneeds = fault_needs_v3(rec, pic, p, s)
    if none_labels is None and none is not None:
        none_labels = episode_labels(none, params=p) if none is not rec else None
    ns = none_served_of(none_labels)
    is_none = (rec.get("v3") or {}).get("agent") == "none"
    if is_none and ns is None:
        ns = {n["contact"]: bool(n["served"]) for n in needs}
    # F14's gap needs: when ours covered each area, this episode's and its `none` replicate's
    gap_cover = OPS.gap_covered(rec, pic) if OPS.stated(rec).get("gap_needs") else None
    none_cover = gap_cover if is_none else (none_labels or {}).get("gap_cover")
    actions = label_actions(rec, pic, truth, needs, fneeds, ns, p, none_cover=none_cover, s=s)
    review = review_labels(rec, pic, truth)
    out = {
        "version": VERSION, "world": world_of(rec), "params": p.as_dict(), "needs": needs, "fault_needs": fneeds,
        "actions": actions, "review": review, "scripted_cards": [], "paired": ns is not None,
        "duration_s": _end_s(rec), "ai_start_s": float((rec.get("clock") or {}).get("ai_start_s") or 0.0),
        "stops": [s for s in rec.get("script") or [] if s.get("kind") == "stop"],
        "domains": sorted({n["domain"] for n in needs}),
    }
    # F14-F16's own needs (labels_ops.py), only where the situation states them
    ops = OPS.ops_needs(rec, pic, actions, review)
    if ops:
        out["ops"] = ops
    if gap_cover is not None:
        out["gap_cover"] = gap_cover
    return out


def with_params(p: Params, **kw: Any) -> Params:
    return replace(p, **kw)


__all__ = ["VERSION", "episode_labels", "contact_needs", "label_actions", "nomination_label",
           "review_labels", "resolved_at", "sensing_errors", "wrong_call", "picture", "Params"]
