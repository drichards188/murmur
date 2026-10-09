"""The picture plays (P2): a sensor's look at a contact, kept on it, and let go.

- `isr.investigate`: a track shown unknown or suspect (not declared), not stale, existence
  confidence now at least 0.5, inside a watch area or closing on a protected point (CPA within
  457 m inside 10 min, or already inside 457 m), and still worth a look: its classification
  below 0.6, or faster than 8 m/s, or an identity conflict (what was seen does not fit its AIS:
  the tracker's own anomaly flag). Action: an orbit by the free unarmed sensor aircraft (an
  imaging sensor in its catalog) that is the preferred platform if one is configured, then
  nearest by ETA at its cruise speed, then most battery, then lowest uid; centred on the
  track's position carried to now; radius max(1.1 x its minimum turn, a quarter of its imaging
  range). Score: 0.5 plus half the urgency of the closest approach (1 at once, 0 at the 10-minute
  horizon) when closing, 0.25 when only inside a watch area; plus 0.1 for a suspect; times the
  pair's expiry factor. One card or order of ours per track.
- `isr.recentre`: an orbit of ours on a track is running and the track is more than half the
  radius from the centre: the same vehicle, a new orbit on the track now; at most once per
  vehicle per 60 s. Score: the offset over the radius.
- `isr.release`: an orbit of ours is running and its premise is gone (below): a hold (or the
  vehicle's prior order of ours again, when we took it from one: never under the free-vehicle
  definition, which takes idle vehicles only). Score 0.5.
- `isr.armed_overwatch` (off unless a situation enables it; a human decision every time): a
  track shown suspect at affiliation confidence 0.5 or more (the rule book's C_MIN), not
  declared, not stale, closing on a protected point, and no track shown neutral or assumed
  friend within the orbit's radius of it: an orbit by a free armed platform (its navigation not
  suspect). Score as investigate's.

A look's premise is gone (its card is withdrawn, its orbit released) when the track is shown
neutral or assumed friend at affiliation confidence 0.5 or more, unseen for 60 s, dropped, out
of the watch (outside every watch area, no closest approach inside 457 m within 10 min), or its
sensor is no longer fit (navigation suspect, battery under the free line, returning). An armed
overwatch's also when the affiliation confidence falls below 0.5, a protected track enters the
radius, or the track goes stale.

Material change (a re-proposal inside a cooldown): the affiliation shown changed; its
confidence moved by 0.15 or more across 0.5; a new declaration; the closest approach halved, and
by at least half the CPA alert (229 m).
"""

from __future__ import annotations

from typing import Any

from .. import rationale as R
from ..beliefs import (NOT_RUNNING, PROTECTED, RETURNING_MODES, Beliefs, Track, Vehicle, closing_alert, eta_s,
                       haversine_m, orbit_radius, seen_by)
from ..ledger import Ledger
from . import P2, Candidate, Play, Result, register

LOOK_PLAYS = ("isr.investigate", "isr.recentre")  # an orbit of ours on a track
# a look offers the arbiter its first few choices of vehicle, each a little lower in score, so a
# vehicle another look took this tick falls to the next free one rather than holding the look back
CHOICES = 3
RANK_STEP = 0.001
LIVE_WAITING = ("proposed", "checked", "auto_approved", "pending_approval", "approved", "queued")


# ---- shared pieces --------------------------------------------------------------------------------


def sensor_capable(v: Vehicle) -> bool:
    return v.armed is False and v.domain == "air" and "orbit" in v.tasks and bool(v.imaging_range_m)


def armed_capable(v: Vehicle) -> bool:
    return v.armed is True and "orbit" in v.tasks


def runs(e: dict[str, Any], b: Beliefs) -> bool:
    """Whether one of our orders is what its vehicle flies now (an orbit reports `done` once
    established and keeps flying; a failsafe's return is not it)."""
    v = b.vehicles.get(e.get("uid")) if e.get("uid") is not None else None
    if v is None or v.task_intent != int(e["intent"]):
        return False
    return (v.task or {}).get("state") not in NOT_RUNNING and (v.mode or "").upper() not in RETURNING_MODES


def silent(v: Vehicle | None, b: Beliefs) -> bool:
    """A vehicle whose link has been lost for a gap (link.cover_gap's threshold)."""
    if v is None or not v.link_lost:
        return False
    full = b.lost_full.get(v.uid) or {}
    lost_after = ((full.get("link") or {}).get("lost_after_s"))
    if not isinstance(lost_after, (int, float)) or v.link_age_s is None:
        return False
    return v.link_age_s >= b.cfg.t["link_gap_factor"] * lost_after


def live(e: dict[str, Any], b: Beliefs) -> bool:
    """A card of ours waiting, an order of ours gone out (until it ends: done once established,
    failed when superseded), or one its vehicle flies now."""
    return e.get("state") in LIVE_WAITING or e.get("state") == "dispatched" or runs(e, b)


def holding(play_names: tuple[str, ...], target: str, b: Beliefs, led: Ledger) -> dict[str, Any] | None:
    """A card or order of ours on this track that still holds it (a running orbit whose vehicle
    went silent past a gap does not: another sensor may take the track)."""
    for _, e in sorted(led.entries.items(), key=lambda kv: int(kv[0])):
        if e["play"] in play_names and e["target"] == target and live(e, b):
            v = b.vehicles.get(e.get("uid"))
            if runs(e, b) and silent(v, b):
                continue
            return e
    for _, s in sorted(led.shadow.items()):
        if s["play"] in play_names and s["target"] == target:
            return s
    return None


def look_reasons(tr: Track, b: Beliefs) -> tuple[list[str], str | None]:
    """(why the track is worth a look, why not): investigate's trigger."""
    th = b.cfg.t
    if tr.declared is not None:
        return [], f"declared {tr.declared_affiliation}"
    if tr.affiliation not in ("unknown", "suspect"):
        return [], f"shown {tr.affiliation}"
    if tr.stale:
        return [], "stale"
    if tr.existence < th["existence_min"]:
        return [], f"existence confidence {tr.existence:.2f} under {th['existence_min']:.2f}"
    i = b.interest[tr.id]
    where = []
    close = closing_alert(i, b.cfg)
    if i.area:
        where.append(f"inside {i.area}")
    if close is not None:
        where.append(f"closing on {close.name}")
    if not where:
        return [], "outside the watch"
    worth = []
    if tr.kind_conf < th["classification_settled"]:
        worth.append(f"classification {tr.kind_conf:.2f} under {th['classification_settled']:.2f}")
    if tr.speed is not None and tr.speed > th["fast_mps"]:
        worth.append(f"fast: {tr.speed:.1f} m/s over {th['fast_mps']:.0f}")
    if tr.conflict:
        worth.append("identity conflict")
    if not worth:
        return [], "classified, slow and consistent"
    return where + worth, None


def watch_text(b: Beliefs) -> str:
    th = b.cfg.t
    return (f"it leaves the watch (beyond {th['leave_area_factor']:g} x every watch area's radius, its motion known, and no "
            f"closest approach inside {th['leave_cpa_factor'] * th['cpa_alert_m']:.0f} m within "
            f"{R.mins(th['leave_tcpa_factor'] * th['tcpa_max_s'])})")


def still_watched(tr: Track, b: Beliefs) -> bool:
    """Whether a look's track is still in the watch for withdrawing or releasing: the trigger's
    area and closest approach, loosened by the leave factors (hysteresis: a track at the edge does
    not flip a card in and out), and a track whose velocity is not known (the tracker has none
    for a young track at times) never counts as gone: an unknown motion is not a motion away."""
    th, cfg = b.cfg.t, b.cfg
    i = b.interest[tr.id]
    if i.area or tr.where is None or tr.velocity is None:
        return True
    for a in cfg["watch_area"]:
        if "center" in a and haversine_m(tr.where, a["center"]) <= th["leave_area_factor"] * a["radius_m"]:
            return True
    reach, horizon = th["leave_cpa_factor"] * th["cpa_alert_m"], th["leave_tcpa_factor"] * th["tcpa_max_s"]
    return any(a.cpa_m <= reach and (a.distance_m <= reach or (a.tcpa_s is not None and a.tcpa_s <= horizon))
               for a in i.approaches)


def look_withdraw_if(b: Beliefs) -> list[str]:
    th = b.cfg.t
    return [f"it is shown neutral or assumed friend at affiliation confidence {th['affiliation_min']:.1f} or more",
            f"it is unseen for {th['unseen_s']:.0f} s", watch_text(b), "the sensor is no longer fit for the task"]


def sensor_unfit(uid: int | None, b: Beliefs) -> str | None:
    v = b.vehicles.get(uid) if uid is not None else None
    if v is None:
        return None
    if uid in b.nav_suspect:
        return f"{v.callsign}'s navigation is suspect ({b.nav_suspect[uid]})"
    if v.battery_pct is not None and v.battery_pct < b.free_battery_pct:
        return f"{v.callsign}'s battery is {v.battery_pct:.0f}%, under {b.free_battery_pct:.0f}%"
    if (v.mode or "").upper() in RETURNING_MODES:
        return f"{v.callsign} is returning ({v.mode})"
    return None


def premise_gone(target: str, uid: int | None, b: Beliefs) -> str | None:
    """Why a look at `target` by `uid` no longer holds, or None."""
    th = b.cfg.t
    tr = b.tracks.get(target)
    if tr is None:
        return f"{target} was dropped by the tracker"
    if tr.shown in PROTECTED and tr.aff_conf >= th["affiliation_min"]:
        return f"{target} is shown {tr.shown} at {tr.aff_conf:.2f}"
    if tr.age_s >= th["unseen_s"]:
        return f"{target} is unseen for {tr.age_s:.0f} s"
    if not still_watched(tr, b):
        near = b.interest[tr.id].closing
        where = f"{near.distance_m:.0f} m from {near.name}" if near else "no protected point"
        return f"{target} left the watch ({where}, outside every watch area, not closing)"
    return sensor_unfit(uid, b)


def isr_material_change(then: dict[str, Any], now: dict[str, Any], b: Beliefs) -> str | None:
    th, m = b.cfg.t, b.cfg.material
    if then.get("affiliation") != now.get("affiliation"):
        return f"the affiliation shown went from {then.get('affiliation')} to {now.get('affiliation')}"
    a, c = then.get("aff_conf"), now.get("aff_conf")
    if isinstance(a, (int, float)) and isinstance(c, (int, float)):
        gate = th["affiliation_min"]
        if abs(c - a) >= m["confidence_step"] and (a < gate) != (c < gate):
            return f"the affiliation confidence went from {a:.2f} to {c:.2f}, across {gate:.1f}"
    if then.get("declared") != now.get("declared") and now.get("declared") is not None:
        return f"a new declaration ({now.get('declared')})"
    # the closest approach halved, and by at least half the CPA alert: at a few hundred metres the
    # estimate wanders by half with a few degrees of course (live: 192 m to 79 m four seconds after
    # a rejection, the boat on the same heading), which is not news to the operator who rejected
    p, q = then.get("cpa_m"), now.get("cpa_m")
    if (isinstance(p, (int, float)) and isinstance(q, (int, float)) and q <= m["cpa_factor"] * p
            and p - q >= m["cpa_factor"] * th["cpa_alert_m"]):
        return f"the closest approach went from {p:.0f} m to {q:.0f} m"
    return None


def track_basis(tr: Track, b: Beliefs) -> dict[str, Any]:
    close = b.interest[tr.id].closing
    decl = tr.declared.get("id") if isinstance(tr.declared, dict) else None
    return {"affiliation": tr.shown, "aff_conf": R.r3(tr.aff_conf), "declared": decl,
            "cpa_m": R.r0(close.cpa_m) if close else None}


def track_facts(tr: Track, b: Beliefs) -> list[dict[str, Any]]:
    seen = seen_by(tr, b.vehicles, b.t)
    return [
        {"ref": f"track:{tr.id}.affiliation", "value": tr.shown, "conf": R.r2(tr.aff_conf), "reason": tr.aff_reason,
         "declared": tr.declared_affiliation, "age_s": R.r1(tr.age_s)},
        {"ref": f"track:{tr.id}.identity", "value": tr.kind, "conf": R.r2(tr.kind_conf), "ais": tr.ais_name,
         "conflict": tr.conflict, "age_s": R.r1(tr.age_s)},
        {"ref": f"track:{tr.id}.kinematics", "speed_mps": R.r1(tr.speed), "course_deg": R.r0(tr.course),
         "predicted": R.ll(tr.where), "sigma_m": R.r1(tr.sigma_now), "age_s": R.r1(tr.age_s)},
        {"ref": f"track:{tr.id}.existence", "value": R.r2(tr.existence), "age_s": R.r1(tr.age_s)},
        {"ref": f"track:{tr.id}.seen_by", "value": seen[:3]},
    ]


def vehicle_facts(v: Vehicle) -> list[dict[str, Any]]:
    return [
        {"ref": f"vehicle:{v.uid}.position", "value": R.ll(v.pos), "age_s": R.r1(v.position_age_s)},
        {"ref": f"vehicle:{v.uid}.battery_pct", "value": R.r0(v.battery_pct)},
        {"ref": f"vehicle:{v.uid}.navigation", "value": v.nav_source, "ready": v.nav_ready, "gps_glitch": v.gps_glitch},
        {"ref": f"vehicle:{v.uid}.task", "value": v.task},
    ]


def approach_derived(tr: Track, b: Beliefs) -> dict[str, Any]:
    i = b.interest[tr.id]
    close = closing_alert(i, b.cfg)
    near = close or i.closing
    return {
        "area": i.area,
        "protected": near.name if near else None,
        "distance_m": R.r0(near.distance_m) if near else None,
        "cpa_m": R.r0(near.cpa_m) if near else None,
        "tcpa_s": R.r0(near.tcpa_s) if near and near.tcpa_s is not None else None,
        "closing": close is not None,
    }


def urgency(tr: Track, b: Beliefs) -> float:
    close = closing_alert(b.interest[tr.id], b.cfg)
    if close is not None:
        horizon = b.cfg.t["tcpa_max_s"]
        u = 1.0 if close.tcpa_s is None else max(0.0, 1.0 - min(close.tcpa_s, horizon) / horizon)
        base = 0.5 + 0.5 * u
    else:
        base = 0.25
    return base + (0.1 if tr.affiliation == "suspect" else 0.0)


def ranked(b: Beliefs, point: tuple[float, float], ok, preferred: str = "") -> tuple[list[Vehicle], list[tuple[Vehicle, str]]]:
    """(free vehicles that pass `ok`, ranked; capable ones that are not free, with why)."""
    free, busy = [], []
    for uid, v in sorted(b.vehicles.items()):
        if not ok(v):
            continue
        why = b.free_reason(v)
        if why:
            busy.append((v, why))
        else:
            free.append(v)

    def rank(v: Vehicle) -> tuple:
        e = eta_s(v, point)
        return (0 if preferred and v.platform == preferred else 1, float("inf") if e is None else round(e, 3),
                -(v.battery_pct or 0.0), v.uid)

    return sorted(free, key=rank), busy


def alternatives(free: list[Vehicle], busy: list[tuple[Vehicle, str]], point: tuple[float, float]) -> list[dict[str, Any]]:
    out = [{"uid": v.uid, "callsign": v.callsign, "eta_s": R.r0(eta_s(v, point)), "battery_pct": R.r0(v.battery_pct)}
           for v in free[1:4]]
    out += [{"uid": v.uid, "callsign": v.callsign, "not_free": why} for v, why in busy[:3]]
    return out


def look_thresholds(b: Beliefs) -> dict[str, Any]:
    th = b.cfg.t
    return {k: th[k] for k in ("existence_min", "classification_settled", "fast_mps", "cpa_alert_m", "tcpa_max_s",
                               "unseen_s", "affiliation_min", "orbit_range_fraction", "orbit_turn_margin")}


def _fact(record: dict[str, Any], suffix: str) -> dict[str, Any]:
    return next((f for f in record["facts"] if str(f["ref"]).endswith(suffix)), {})


def where_text(d: dict[str, Any]) -> str:
    parts = []
    if d.get("closing") and d.get("protected"):
        if d.get("tcpa_s") is None:
            parts.append(f"{d['distance_m']:.0f} m from {d['protected']}")
        else:
            parts.append(f"closest approach {d['cpa_m']:.0f} m to {d['protected']} in {R.mins(d['tcpa_s'])}")
    elif d.get("protected") and d.get("distance_m") is not None:
        parts.append(f"{d['distance_m']:.0f} m from {d['protected']}")
    if d.get("area"):
        parts.append(f"inside {d['area']}")
    return ", ".join(parts) or "position known"


def track_text(record: dict[str, Any]) -> str:
    tid = record["target"]
    aff, ident, kin = _fact(record, ".affiliation"), _fact(record, ".identity"), _fact(record, ".kinematics")
    seen = _fact(record, ".seen_by").get("value") or []
    kind = f"{ident.get('value') or 'unclassified'} ({ident.get('conf'):.2f})" if ident.get("value") else "unclassified"
    ais = f"AIS {R.quote(ident['ais'], 40)}" if ident.get("ais") else "no AIS"
    conflict = f", identity conflict {R.quote(ident['conflict'])}" if ident.get("conflict") else ""
    sb = ", ".join(f"{s['callsign']}'s {s['sensor']} {s['age_s']:.0f} s ago" for s in seen[:2]) or "no sensor of ours"
    spd = kin.get("speed_mps")
    motion = f"{spd:.1f} m/s course {kin.get('course_deg') or 0:03.0f}" if spd is not None else "no velocity estimate"
    return (f"{tid}: {kind}, {ais}{conflict}, shown {aff.get('value')} {aff.get('conf'):.2f} "
            f"({R.quote(aff.get('reason') or '')}); {motion}; {where_text(record['derived'])}; seen by {sb}")


def sensor_text(record: dict[str, Any], what: str) -> str:
    v = record["vehicle"]
    alts = [a for a in record.get("alternatives") or [] if "eta_s" in a]
    alt = f"; next {alts[0]['callsign']} ({R.mins(alts[0]['eta_s'])})" if alts else ""
    pref = "preferred-platform " if record["derived"].get("preferred") else ""
    passed = record["derived"].get("passed_over") or []
    after = f" after {', '.join(passed)}" if passed else ""
    return (f"{v['callsign']} is the {pref}free {what} nearest by time{after} ({R.mins(record['derived'].get('eta_s'))} away; "
            f"battery {v['battery_pct']:.0f}%){alt}")


def vehicle_ref(v: Vehicle) -> dict[str, Any]:
    return {"uid": v.uid, "callsign": v.callsign, "platform": v.platform, "battery_pct": R.r0(v.battery_pct)}


# ---- the plays ---------------------------------------------------------------------------------------


class Investigate(Play):
    name, klass = "isr.investigate", P2

    def evaluate(self, b: Beliefs, led: Ledger) -> Result:
        res = Result()
        cfg = b.cfg
        if not cfg["watch_area"] and not cfg["protected"]:
            res.blocked.append(("-", "no watch area or protected point is configured"))
            return res
        for tid, tr in sorted(b.tracks.items()):
            why_look, _ = look_reasons(tr, b)
            held = holding(LOOK_PLAYS, tid, b, led)
            if held is not None:
                res.active.append(f"{tid} by uid {held.get('uid')} (i{held.get('intent', '-')} {held.get('state', 'would-be')})")
                continue
            if not why_look:
                continue
            point = tr.where
            free, busy = ranked(b, point, sensor_capable, cfg["sensor_platform"])
            if not free:
                detail = "; ".join(f"{v.callsign}: {why}" for v, why in busy[:3]) or "none in the fleet"
                res.blocked.append((tid, f"no free sensor aircraft ({detail})"))
                continue
            score = urgency(tr, b) * led.score_factor.get(f"{self.name}|{tid}", 1.0)
            for rank, v in enumerate(free[:CHOICES]):
                radius = orbit_radius(v, cfg)
                derived = approach_derived(tr, b)
                derived.update({"eta_s": R.r0(eta_s(v, point)), "radius_m": radius, "rank": rank,
                                "preferred": bool(cfg["sensor_platform"]) and v.platform == cfg["sensor_platform"]})
                res.candidates.append(Candidate(
                    play=self.name, version=self.version, klass=self.klass, score=score - RANK_STEP * rank,
                    target=tid, uid=v.uid, task="orbit",
                    request={"task": "orbit", "uids": [v.uid], "center": R.ll(point), "radius_m": radius},
                    facts=track_facts(tr, b) + vehicle_facts(v), derived=derived, thresholds=look_thresholds(b),
                    withdraw_if=look_withdraw_if(b), basis=track_basis(tr, b), trigger=why_look,
                    alternatives=alternatives(free[rank:], busy, point), claims=(v.uid,), vehicle=vehicle_ref(v),
                ))
        return res

    def withdraw_reason(self, entry: dict[str, Any], b: Beliefs) -> str | None:
        return premise_gone(entry["target"], entry.get("uid"), b)

    def material_change(self, then: dict[str, Any], now: dict[str, Any], b: Beliefs) -> str | None:
        return isr_material_change(then, now, b)

    def render(self, record: dict[str, Any]) -> str:
        return (f"{R.head(record)} Orbit {record['vehicle']['callsign']} over {record['target']} "
                f"({record['request']['radius_m']:.0f} m radius) to keep a sensor on it. {track_text(record)}. "
                f"{sensor_text(record, 'sensor aircraft')}. Why now: {'; '.join(record['trigger'])}. {R.tail(record)}")


class Recentre(Play):
    name, klass = "isr.recentre", P2

    def evaluate(self, b: Beliefs, led: Ledger) -> Result:
        res = Result()
        th = b.cfg.t
        for _, e in sorted(led.entries.items(), key=lambda kv: int(kv[0])):
            if e["play"] not in LOOK_PLAYS or not runs(e, b):
                continue
            tid, uid = e["target"], e.get("uid")
            tr, v = b.tracks.get(tid), b.vehicles.get(uid)
            if tr is None or v is None or tr.where is None or premise_gone(tid, uid, b):
                continue  # isr.release's
            if any(x["play"] == self.name and x["target"] == tid and x.get("state") in LIVE_WAITING
                   for x in led.open_entries()):
                res.active.append(f"{tid}: a recentre waits")
                continue
            centre, radius = e["request"].get("center"), float(e["request"].get("radius_m") or 0.0)
            if not centre or radius <= 0:
                continue
            off = haversine_m(tr.where, centre)
            res.active.append(f"{tid} by {v.callsign} (i{e['intent']})")
            if off <= th["recentre_fraction"] * radius:
                continue
            last = led.recentred.get(str(uid))
            if last is not None and b.t - last < th["recentre_min_gap_s"]:
                res.blocked.append((tid, f"{v.callsign} recentred within the last {th['recentre_min_gap_s']:.0f} s (once per vehicle)"))
                continue
            derived = approach_derived(tr, b)
            derived.update({"offset_m": R.r0(off), "radius_m": radius, "old_center": R.ll(centre),
                            "running": e["intent"], "eta_s": None})
            res.candidates.append(Candidate(
                play=self.name, version=self.version, klass=self.klass, score=off / radius, target=tid, uid=uid,
                task="orbit", request={"task": "orbit", "uids": [uid], "center": R.ll(tr.where), "radius_m": radius},
                facts=track_facts(tr, b) + vehicle_facts(v), derived=derived,
                thresholds={"recentre_fraction": th["recentre_fraction"], "recentre_min_gap_s": th["recentre_min_gap_s"],
                            **look_thresholds(b)},
                withdraw_if=look_withdraw_if(b), basis=track_basis(tr, b),
                trigger=[f"{off:.0f} m off the centre, over {th['recentre_fraction']:.1f} x the {radius:.0f} m radius"],
                claims=(uid,), supersedes=int(e["intent"]), vehicle=vehicle_ref(v),
            ))
        return res

    def withdraw_reason(self, entry: dict[str, Any], b: Beliefs) -> str | None:
        return premise_gone(entry["target"], entry.get("uid"), b)

    def material_change(self, then: dict[str, Any], now: dict[str, Any], b: Beliefs) -> str | None:
        return isr_material_change(then, now, b)

    def render(self, record: dict[str, Any]) -> str:
        d = record["derived"]
        return (f"{R.head(record)} Move {record['vehicle']['callsign']}'s orbit (our i{d['running']}) onto "
                f"{record['target']} now ({record['request']['radius_m']:.0f} m radius): the track is {d['offset_m']:.0f} m "
                f"from the centre. {track_text(record)}. {R.tail(record)}")


class Release(Play):
    name, klass = "isr.release", P2

    def evaluate(self, b: Beliefs, led: Ledger) -> Result:
        res = Result()
        for _, e in sorted(led.entries.items(), key=lambda kv: int(kv[0])):
            if e["play"] not in LOOK_PLAYS or not runs(e, b):
                continue
            tid, uid = e["target"], e.get("uid")
            why = premise_gone(tid, uid, b)
            v = b.vehicles.get(uid)
            if why is None or v is None:
                continue
            if any(x["play"] == self.name and x["target"] == tid and x.get("uid") == uid and live(x, b)
                   for _, x in sorted(led.entries.items())):
                continue
            prior = e.get("prior_task") or {}
            prior_entry = led.entry(int(prior["intent_id"])) if prior.get("intent_id") is not None else None
            if prior_entry is not None and prior_entry.get("request"):
                request = dict(prior_entry["request"])
                action = f"back to its order before ours (i{prior['intent_id']} {prior_entry['task']})"
            else:
                request = {"task": "hold", "uids": [uid]}
                action = "hold where it is"
            res.candidates.append(Candidate(
                play=self.name, version=self.version, klass=self.klass, score=0.5, target=tid, uid=uid,
                task=str(request["task"]), request=request,
                facts=(track_facts(b.tracks[tid], b) if tid in b.tracks else []) + vehicle_facts(v),
                derived={"running": e["intent"], "action": action, "why": why},
                thresholds={k: b.cfg.t[k] for k in ("affiliation_min", "unseen_s", "cpa_alert_m", "tcpa_max_s")},
                withdraw_if=[], basis={"running": e["intent"]}, trigger=[why], claims=(uid,), supersedes=int(e["intent"]),
                vehicle=vehicle_ref(v),
            ))
        return res

    def render(self, record: dict[str, Any]) -> str:
        d = record["derived"]
        return (f"{R.head(record)} Release {record['vehicle']['callsign']} from our orbit i{d['running']} on "
                f"{record['target']}: {d['why']}. It is to {d['action']}. {R.tail(record)}")


class ArmedOverwatch(Play):
    name, klass = "isr.armed_overwatch", P2

    def evaluate(self, b: Beliefs, led: Ledger) -> Result:
        res = Result()
        cfg, th = b.cfg, b.cfg.t
        if not cfg["protected"]:
            res.blocked.append(("-", "no protected point is configured"))
            return res
        for tid, tr in sorted(b.tracks.items()):
            held = holding((self.name,), tid, b, led)
            if held is not None:
                res.active.append(f"{tid} by uid {held.get('uid')}")
                continue
            if tr.declared is not None or tr.affiliation != "suspect" or tr.stale:
                continue
            if tr.existence < th["existence_min"]:
                continue
            close = closing_alert(b.interest[tid], cfg)
            if close is None:
                continue
            if tr.aff_conf < th["affiliation_min"]:
                res.blocked.append((tid, f"suspect at {tr.aff_conf:.2f}, under {th['affiliation_min']:.1f}"))
                continue
            point = tr.where
            free, busy = ranked(b, point, armed_capable, "")
            if not free:
                detail = "; ".join(f"{v.callsign}: {why}" for v, why in busy[:3]) or "none in the fleet"
                res.blocked.append((tid, f"no free armed platform ({detail})"))
                continue
            score = urgency(tr, b) * led.score_factor.get(f"{self.name}|{tid}", 1.0)
            for rank, v in enumerate(free[:CHOICES]):
                why = self.add(res, b, tr, v, rank, score - RANK_STEP * rank, point, close, free[rank:], busy)
                if why:
                    res.blocked.append((tid, why))
                    break
        return res

    def add(self, res: Result, b: Beliefs, tr: Track, v: Vehicle, rank: int, score: float, point, close, free, busy) -> str | None:
        """One choice of platform, or why none fits: a track shown neutral or assumed friend
        inside the orbit's radius of the aim (the radius is the platform's)."""
        th, tid = b.cfg.t, tr.id
        radius = orbit_radius(v, b.cfg)
        near = [o for o in sorted(b.tracks.values(), key=lambda o: o.id)
                if o.id != tid and o.where and o.shown in PROTECTED and haversine_m(o.where, point) <= radius]
        if near:
            o = near[0]
            return f"{o.id} shown {o.shown} {haversine_m(o.where, point):.0f} m from the aim, inside {radius:.0f} m"
        derived = approach_derived(tr, b)
        derived.update({"eta_s": R.r0(eta_s(v, point)), "radius_m": radius, "preferred": False, "rank": rank})
        res.candidates.append(Candidate(
            play=self.name, version=self.version, klass=self.klass, score=score,
            target=tid, uid=v.uid, task="orbit",
            request={"task": "orbit", "uids": [v.uid], "center": R.ll(point), "radius_m": radius},
            facts=track_facts(tr, b) + vehicle_facts(v), derived=derived, thresholds=look_thresholds(b),
            withdraw_if=[f"its affiliation confidence falls under {th['affiliation_min']:.1f}",
                         "a track shown neutral or assumed friend comes within the radius", "it goes stale"]
            + look_withdraw_if(b)[:3],
            basis=track_basis(tr, b), trigger=[f"suspect at {tr.aff_conf:.2f}", f"closing on {close.name}"],
            alternatives=alternatives(free, busy, point), claims=(v.uid,), armed=True, vehicle=vehicle_ref(v),
        ))
        return None

    def withdraw_reason(self, entry: dict[str, Any], b: Beliefs) -> str | None:
        th = b.cfg.t
        tr = b.tracks.get(entry["target"])
        if tr is not None:
            if tr.stale:
                return f"{tr.id} is stale"
            if tr.aff_conf < th["affiliation_min"] and tr.declared is None:
                return f"{tr.id}'s affiliation confidence fell to {tr.aff_conf:.2f}"
            centre, radius = entry["request"].get("center"), float(entry["request"].get("radius_m") or 0.0)
            for o in sorted(b.tracks.values(), key=lambda o: o.id):
                if o.id != tr.id and o.where and centre and o.shown in PROTECTED and haversine_m(o.where, centre) <= radius:
                    return f"{o.id}, shown {o.shown}, is inside the radius"
        return premise_gone(entry["target"], entry.get("uid"), b)

    def material_change(self, then: dict[str, Any], now: dict[str, Any], b: Beliefs) -> str | None:
        return isr_material_change(then, now, b)

    def render(self, record: dict[str, Any]) -> str:
        return (f"{R.head(record)} Armed overwatch: orbit {record['vehicle']['callsign']} (armed) over {record['target']} "
                f"({record['request']['radius_m']:.0f} m radius); a human decides. {track_text(record)}. "
                f"{sensor_text(record, 'armed platform')}. Why now: {'; '.join(record['trigger'])}. {R.tail(record)}")


for _p in (Investigate(), Recentre(), Release(), ArmedOverwatch()):
    register(_p)
