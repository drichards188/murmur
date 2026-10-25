"""Scoring v3's labels (eval/SCORING-V3.md section 2), from one v3 episode record (its picture samples
under `v3`) and, for `useful` and `duplicate`, its paired `none` replicate. v2's instruments are
imported where v3 keeps them unchanged (eval/headless/v2/labels.py: the actions from the audit log,
their runs, fault needs, the reserve, the precedence); what changes is written here:

- **Contact needs per domain** (2.1, 2.2): the world's mission circle (the record's world), the
  domain's grace, sensor and level (mission.py `DOMAINS`). A need is **resolved at t** when the track
  the tracker associates with the contact shows, in the picture sample at t, the contact's true kind
  (or, where the domain's level is recognition, its true family) at a confidence of 0.6 or more,
  without an identity conflict, and `supported_by` holds the domain's sensor (or AIS, for a surface
  contact that truly broadcasts it).
- **Sensing error** (2.1): every vehicle of the fleet carrying a sensor that could resolve the
  contact has its wrong-call draw set at the deciding level (the physical model's draw: per observer,
  sensor, contact and level, crates/murmur-bridge tracks/physical/mod.rs `label_at`, ported in
  `wrong_call`), so no look could resolve it. Rare by construction (it needs every observer wrong);
  a need **poisoned** by one wrong observer (a conflict at the deciding level) is kept, and named as
  its cause in the dev check (causes.py).
- **Contribution** (2.3): v2's rule, with the need's domain's sensors in place of EO/IR and sonar: the
  action's vehicle raised its hits on the need's track with a sensor that supports the domain, while
  its vehicle flew that order, before the need was identified.
- **Action labels** (2.5) for the moving kinds (orbit, patrol, track, hold, rtl): v2's precedence and
  rules (credit for effect only; none after the need was identified; `duplicate` when the `none`
  replicate also served the need; harm by geometry), with the world's centre, the domain's sensors,
  and a `track` order pointed at the contact its track names.
- **Nominations** (`engage`, 2.8) and **review decisions** (2.9), judged against truth: their own
  labels (`nomination_labels`, `review_labels`). **Collect** and **cover** (2.10): defined in
  SCORING-V3.md, labelled `unjudged` here until F14 builds them.
"""

from __future__ import annotations

from dataclasses import replace
from typing import Any

from eval.world import load_world

from ..labels import Truth, dist_m
from ..v2 import labels as V2
from ..v2.labels import Params, fault_needs, geometry, geometry_dist
from ..v2.labels import instrument as _v2_instrument
from ..v2.record import Picture, carry
from . import labels_ops as OPS
from .mission import DOMAINS, RESOLVE_CONF, rule

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
INSTRUMENTS = {
    "contact_need": "v3.contact_need", "sensing_error": "v3.sensing_error", "identity_resolved": "v3.identity_resolved",
    "contribution": "v3.contribution", "fault_need": "v2.fault_need", "action": "v3.action_label", "aim": "v3.aim",
    "justified": "v3.justified_on_picture", "nomination": "v3.nomination", "review": "v3.review_decision",
}


def instrument(name: str) -> dict[str, str]:
    if INSTRUMENTS[name].startswith("v2."):
        return _v2_instrument(name)
    return {"name": INSTRUMENTS[name], "version": VERSION}


def picture(rec: dict[str, Any]) -> Picture:
    v3 = rec.get("v3") or {}
    if not v3.get("picture"):
        raise ValueError("this record has no v3 picture samples (not run with scoring v3)")
    return Picture(v3["picture"])


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


def sensing_errors(pic: Picture, truth: Truth) -> dict[str, dict[str, Any]]:
    """Per contact: every observer and sensor of the fleet that could resolve it under the domain's
    rule, and whether its draw is wrong at the deciding level (identification; for a recognition
    rule, a passive sonar's identification counts too, so either level resolves it)."""
    s = pic.sensing
    seed, rate = s.get("seed"), float(s.get("misclassification_rate") or 0.0)
    out = {}
    for cid, m in sorted(truth.meta.items()):
        domain = str(m.get("domain") or "surface")
        r = rule(domain)
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

def shown_ok(kind_shown: str | None, meta: dict[str, Any]) -> bool:
    """The shown kind is the contact's true kind, or its true family where the domain's level is
    recognition."""
    true = str(meta.get("kind") or "")
    if kind_shown == true:
        return True
    return rule(meta.get("domain")).level == "recognise" and kind_shown is not None and kind_shown == FAMILY.get(true)


def supported(idn: dict[str, Any], meta: dict[str, Any]) -> bool:
    r = rule(meta.get("domain"))
    sb = set(idn.get("supported_by") or [])
    return bool(sb & set(r.sensors)) or (r.ais_counts and "ais" in sb and bool(meta.get("ais")))


def resolved_at(pic: Picture, k: int | None, cid: str, meta: dict[str, Any]) -> bool:
    """2.2 (v3): the associated track shows the true kind (or family, by the domain's level) at
    0.6 or more, without a conflict, supported by the domain's sensor."""
    tid = pic.track_of(cid, k)
    if tid is None:
        return False
    idn = pic.identity(tid, k)
    if not idn or idn["conflict"] or not shown_ok(idn["kind"], meta):
        return False
    if idn["confidence"] is None or float(idn["confidence"]) < RESOLVE_CONF:
        return False
    return supported(idn, meta)


def contact_needs(rec: dict[str, Any], pic: Picture, truth: Truth, p: Params) -> list[dict[str, Any]]:
    end = _end_s(rec)
    m_ = load_world(world_of(rec)).mission
    errs = sensing_errors(pic, truth)
    out = []
    for cid, m in sorted(truth.meta.items()):
        aff = str(m.get("affiliation") or "").lower()
        if aff not in NEED_AFFILIATIONS:
            continue
        t_need = truth.first_within(cid, m_.center, m_.radius_m)
        if t_need is None:
            continue
        domain = str(m.get("domain") or "surface")
        grace = rule(domain).grace_s
        D = t_need + grace
        evaluable = D <= end - END_MARGIN_S
        se = errs[cid]
        k_need = pic.index(t_need)
        pre = resolved_at(pic, k_need, cid, m)
        t_served = t_need if pre else None
        t_identified = None
        if pre:
            assert k_need is not None
            k = k_need
            while k > 0 and resolved_at(pic, k - 1, cid, m):
                k -= 1
            t_identified = pic.time(k)
        else:
            k0 = pic.index_after(t_need)
            if k0 is not None:
                for k in range(k0, len(pic.times)):
                    if resolved_at(pic, k, cid, m):
                        t_served = t_identified = pic.time(k)
                        break
        served = t_served is not None and t_served <= D + 1e-9
        late_s = grace * p.late_s / V2.DEADLINE_S  # the multiverse's S' scales each domain's grace
        out.append({
            "contact": cid, "kind": m.get("kind"), "affiliation": aff, "ais": bool(m.get("ais")), "domain": domain,
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

def need_sensors(n: dict[str, Any]) -> tuple[str, ...]:
    """The sensors whose hits count as a contribution to a need: its domain's (AIS too for a surface
    contact that truly broadcasts it)."""
    r = rule(n.get("domain"))
    return tuple(r.sensors) + (("ais",) if r.ais_counts and n.get("ais") else ())


def track_target(a: dict[str, Any], pic: Picture) -> str | None:
    """The contact a `track` order's track names at its creation (the tracker's association)."""
    tid = (a.get("params") or {}).get("track_id") or (a.get("params") or {}).get("track")
    if not tid or a.get("created") is None:
        return None
    return pic.contact_of(str(tid), pic.index(a["created"]))


def aim_of(a: dict[str, Any], pic: Picture, truth: Truth, needs: dict[str, dict[str, Any]], p: Params) -> dict[str, Any]:
    """v2's aim (by hits of any classifying sensor while the order ran, else by geometry), and for a
    `track` order without hits, the contact its track names."""
    out = V2.aim_of(a, pic, truth, needs, p)
    if out["by"] == "none" and a.get("kind") == "track":
        c = track_target(a, pic)
        if c is not None:
            out = {"by": "track", "contacts": [c], "primary": c, "ambiguous": False, "distance_m": None,
                   "instrument": instrument("aim")}
    out["instrument"] = instrument("aim")
    return out


def contributed(a: dict[str, Any], pic: Picture, needs: dict[str, dict[str, Any]]) -> list[str]:
    """2.3 (v3): v2's, with each need's own domain sensors."""
    out = []
    for c, n in needs.items():
        if not n["served"] or n.get(V2.CONTRIBUTION_UNTIL) is None:
            continue
        sensors = need_sensors(n)
        for uid, run in a["runs"].items():
            if not run.get("flew"):
                continue
            t1 = run["own_start"]
            t2 = min(run["end"], n[V2.CONTRIBUTION_UNTIL])
            if t2 > t1 and pic.contact_hits_rose(c, uid, sensors, t1, t2):
                out.append(c)
                break
    return sorted(out, key=lambda c: (needs[c]["t_need"], c))


def justified(a: dict[str, Any], aim: dict[str, Any], pic: Picture, truth: Truth, p: Params,
              center: tuple[float, float], radius_m: float) -> dict[str, Any]:
    """v2's justification on the picture, about the world's own centre."""
    t_a = a["created"]
    k = pic.index(t_a)
    out: dict[str, Any] = {"justified": False, "track": None, "instrument": instrument("justified"),
                           "horizon_s": p.justify_horizon_s}
    if k is None:
        return out
    tracks = pic.track_ids(k)
    if not tracks:
        return out
    tid = None
    target = aim.get("primary") or aim.get("nearest")
    if target is not None:
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
    idn = tr.get("identity") or {}
    shown_aff = tr.get("affiliation")
    doubt = (shown_aff in NEED_AFFILIATIONS or bool(idn.get("conflict"))
             or idn.get("confidence") is None or float(idn["confidence"]) < RESOLVE_CONF)
    kin = pic.kinematics(tid, k)
    near = False
    if kin and kin["position"][0] is not None:
        p0 = carry(kin["position"], kin["velocity"], t_a - float(kin["last_seen_s"] or t_a))
        steps = int(p.justify_horizon_s // V2.JUSTIFY_STEP_S) if kin["velocity"] else 0
        for j in range(steps + 1):
            if dist_m(carry(p0, kin["velocity"], j * V2.JUSTIFY_STEP_S), center) <= radius_m:
                near = True
                break
    out.update(justified=doubt and near, track=tid, shown={"affiliation": shown_aff, "identity": idn}, doubt=doubt,
               approaches=near)
    return out


def pointed_at(a: dict[str, Any], aim: dict[str, Any], pic: Picture, truth: Truth, p: Params) -> set[str]:
    """v2's harm (b) spares (where the order sends its vehicle), and for a `track` order the contact
    its track names."""
    if a.get("kind") == "track":
        c = track_target(a, pic)
        return {c} if c else set(aim.get("contacts") or [])
    return V2.pointed_at(a, geometry(a["kind"], a["params"]), aim, truth, p)


def harmful(a: dict[str, Any], aim: dict[str, Any], pic: Picture, needs: dict[str, dict[str, Any]], truth: Truth,
            p: Params) -> list[dict[str, Any]]:
    """v2's harm (a) and (b), with each need's own domain sensors for (b); (c), an engagement of a
    neutral or friend, is the nominations' (`nomination_labels`)."""
    out = []
    if not a["runs"]:
        return out
    g = geometry(a["kind"], a["params"])
    spared: set[str] | None = None
    for uid, run in a["runs"].items():
        t_d = run["start"]
        v = pic.vehicle(uid, pic.index(t_d))
        if a["kind"] in V2.MOVE_KINDS and v and v.get("battery_pct") is not None and v["battery_pct"] < V2.RESERVE_PCT \
                and g is not None and v.get("home") and v["home"][0] is not None \
                and V2.geometry_dist_home((v["home"][0], v["home"][1]), g) > V2.HOME_M:
            out.append({"kind": "a", "uid": uid, "battery_pct": v["battery_pct"]})
        for c, n in needs.items():
            if not n["eligible"] or n["served"] or not (n["t_need"] <= t_d <= n["deadline"]):
                continue
            if pic.contact_hits_rose(c, uid, need_sensors(n), t_d - V2.RISE_WINDOW_S, t_d):
                if spared is None:
                    spared = pointed_at(a, aim, pic, truth, p)
                if c not in spared:
                    out.append({"kind": "b", "uid": uid, "need": c})
    return out


#: the kinds labelled as v2 labels its orders (moving a vehicle, or holding or recalling it)
MOVING_KINDS = ("orbit", "patrol", "track", "hold", "rtl")
#: defined in SCORING-V3.md 2.10, built with F14: labelled `unjudged` until then
SPACE_KINDS = ("collect", "cover")


def label_actions(rec: dict[str, Any], pic: Picture, truth: Truth, needs_list: list[dict[str, Any]],
                  fneeds: list[dict[str, Any]], none_served: dict[str, bool] | None, p: Params,
                  none_cover: dict[str, Any] | None = None) -> list[dict[str, Any]]:
    """Every action of the agent under test: the moving kinds by v2's precedence and rules (v3's
    domain sensors, centre and `track` aim), nominations by `nomination_label`, collect and cover
    by 2.10 (labels_ops.py `space_label`; `none_cover`: the paired `none` replicate's coverage of
    the stated gap needs)."""
    w = load_world(world_of(rec))
    center, radius = w.mission.center, w.mission.radius_m
    needs = {n["contact"]: n for n in needs_list}
    acts = V2.agent_actions(as_v2(rec), pic)
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
        if a["refused"] and a["kind"] != "cover":
            lab.update(aim=empty_aim, aimed_need=None, contributed=[], justified={"justified": False}, faults_open=[],
                       harm=[], label="refused")
            out.append(lab)
            continue
        if a["kind"] == "engage":
            lab.update(aim=empty_aim, aimed_need=None, contributed=[], justified={"justified": False},
                       faults_open=V2._open_faults(a, pic, rec, fneeds), harm=[])
            lab.update(nomination_label(a, rec, pic, truth))
            lab["also_fault"] = bool(lab["faults_open"])
            out.append(lab)
            continue
        if a["kind"] in SPACE_KINDS:
            # 2.10 (labels_ops.py): a cover whose only failing check is coverage answered its question
            lab.update(aim=empty_aim, aimed_need=None, contributed=[], justified={"justified": False},
                       faults_open=V2._open_faults(a, pic, rec, fneeds), harm=[])
            lab.update(OPS.space_label(a, rec, pic, none_cover))
            lab["also_fault"] = bool(lab["faults_open"])
            out.append(lab)
            continue
        if a["kind"] not in MOVING_KINDS:
            lab.update(aim=empty_aim, aimed_need=None, contributed=[], justified={"justified": False},
                       faults_open=V2._open_faults(a, pic, rec, fneeds), harm=[], label="unjudged",
                       unjudged_because=f"no v3 label for {a['kind']!r}")
            lab["also_fault"] = bool(lab["faults_open"])
            out.append(lab)
            continue
        aim = aim_of(a, pic, truth, needs, p)
        lab["aim"] = aim
        lab["aimed_need"] = V2.aimed_need(aim, needs)
        lab["contributed"] = contributed(a, pic, needs) if a["runs"] else []
        lab["justified"] = justified(a, aim, pic, truth, p, center, radius)
        lab["faults_open"] = V2._open_faults(a, pic, rec, fneeds)
        harm = harmful(a, aim, pic, needs, truth, p)
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
                                         or resolved_at(pic, pic.index(a["created"]), c, truth.meta.get(c) or {})))
                was_credited = bool(c and c in credited and credited[c][1] is not None
                                    and credited[c][1] <= (a["created"] or 0.0))
                label = "redundant" if (was_served or was_credited) else "useless"
                if label == "redundant":
                    lab["redundant_because"] = "served" if was_served else "credited"
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
    track's own (the claimant's track is read from the decision's `by_track` when it carries one;
    review.py's decisions do not yet, so it is `unjudged` until they do). A lost contact accepted for
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
    """Every label of a v3 episode; `none` (or `none_labels`) its paired `none` replicate's."""
    p = params or Params()
    pic = picture(rec)
    truth = Truth(rec)
    needs = contact_needs(rec, pic, truth, p)
    fneeds = fault_needs(rec, pic, p)
    if none_labels is None and none is not None:
        none_labels = episode_labels(none, params=p) if none is not rec else None
    ns = none_served_of(none_labels)
    is_none = (rec.get("v3") or {}).get("agent") == "none"
    if is_none and ns is None:
        ns = {n["contact"]: bool(n["served"]) for n in needs}
    # F14's gap needs: when ours covered each area, this episode's and its `none` replicate's
    gap_cover = OPS.gap_covered(rec, pic) if OPS.stated(rec).get("gap_needs") else None
    none_cover = gap_cover if is_none else (none_labels or {}).get("gap_cover")
    actions = label_actions(rec, pic, truth, needs, fneeds, ns, p, none_cover=none_cover)
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


__all__ = ["VERSION", "DOMAINS", "episode_labels", "contact_needs", "label_actions", "nomination_label",
           "review_labels", "resolved_at", "sensing_errors", "wrong_call", "picture", "Params"]
