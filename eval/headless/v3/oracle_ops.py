"""The basic v3 oracle's few additions for F14-F16 (eval/SCORING-V3.md section 5; families_ops.py): a
feasibility check that each family's needs can be met, not a ceiling (C2 builds the full oracle in its
own files). MODEL RESULTS ONLY. Privileged as the basic oracle is (truth, the contacts file, the
script's truth probes), every read counted under `agent_detail.privileged`; it acts only through the
MCP tools an agent has. In every other family it does exactly what `OracleV3` does.

- **F14** (the stated `gap_needs` and `cover_needs`): at its first wake it asks the ranked tasking
  options over the gap's area until its due time (optical and SAR) and proposes a collect with the
  first option whose product is ready by then and whose collect is at least 10 minutes off; it asks
  a GNSS cover over the run's area and window at the run's level; and, where the script's truth
  probe predicts an outage in the run (fewer satellites than the run needs at a sample), it holds
  the run's vehicle (`hold`, a safety command) 20 s before each outage sample while the run is
  under way, and sends the run again (the same patrol) 20 s after it when the run still has time.
- **F15** (the stated `engagement`): every `ENGAGE_TICK_S`, once the hostile's track (by the
  tracker's association) shows declared hostile and no nomination on it stands, it reads the
  nomination form's dry run for the armed platforms and nominates the first that passes, with an
  effect radius of 75 m, or 40 m when a contact that is not hostile by truth is within 175 m of
  the target; it waits while one is within 140 m (3D), and never nominates a track whose contact
  is not hostile by truth (restraint by truth), at most three times.
- **F16** (the stated `review_items`): each item's contacts become needs for an EO/IR look (the
  evidence the reviewer reads), opened 20 s before the item and due 200 s after: the basic
  oracle's plan sends two vehicles to each, as for an identification.
"""

from __future__ import annotations

import datetime as dt
import math
import tomllib
from pathlib import Path
from typing import Any

from eval.agent import _mcp_args

from ..labels import dist_m
from ..scripted import first_passing, keep_out_blockers, standing_on
from .families import wet
from .oracle import Need, OracleV3, Path2, to_ll, to_xy


def wet_point(world: str, p: tuple[float, float], depth_m: float = 3.0) -> bool:
    return wet(world, [p, p], depth_m)

ENGAGE_TICK_S = 4.0
COLLECT_MIN_LEAD_S = 600.0
HOLD_BEFORE_S = 20.0
RESUME_AFTER_S = 20.0
CLEAR_3D_M = 140.0
SMALL_RADIUS_NEAR_M = 175.0
REVIEW_LEAD_S = 20.0
REVIEW_DUE_S = 200.0


def iso(unix_s: float) -> str:
    return dt.datetime.fromtimestamp(unix_s, dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def unix(t: Any) -> float | None:
    if isinstance(t, (int, float)):
        return float(t) / (1000.0 if t > 1e11 else 1.0)
    if isinstance(t, str):
        try:
            return dt.datetime.fromisoformat(t.replace("Z", "+00:00")).timestamp()
        except ValueError:
            return None
    return None


def attempt_row(o: dict[str, Any]) -> dict[str, Any]:
    """One platform's line of a nomination form: passed, span (s from the preview's time to its
    effect time), failing checks, and each blocking keep-out item [id, subject, region kind,
    domain, shortfall m]."""
    pv = o.get("preview") or {}
    span = None
    t, et = unix(pv.get("t")), unix(pv.get("effect_t"))
    if t is not None and et is not None:
        span = round(et - t, 1)
    items = [[it.get("id"), it.get("subject"), (it.get("region") or {}).get("kind"), it.get("domain"),
              None if it.get("short_m") is None else round(float(it["short_m"]))]
             for it in (o.get("keep_outs") or {}).get("items") or [] if it.get("blocks")]
    return {"uid": o.get("uid"), "callsign": o.get("callsign"), "passed": bool(o.get("passed")), "span_s": span,
            "failing": sorted({str(c.get("rule")) for c in o.get("checks") or [] if c.get("outcome") != "pass"}),
            "blocking": items}


class OracleOps(OracleV3):
    """The basic v3 oracle with F14-F16's additions (module doc)."""

    def __init__(self, ep: Any):
        super().__init__(ep)
        self.st = dict(ep.e.truth or {})
        self.epoch = float(getattr(ep, "sim_epoch", 0.0) or 0.0)
        self.space_done = False
        self.space_options: dict[str, Any] = {}
        self.holds: dict[str, Any] = {"outages": None, "acted": []}
        self.noms: dict[str, int] = {}
        self.next_engage = 0.0
        self.contacts: dict[str, dict[str, Any]] = {}
        self.armed_units: dict[int, Any] = {}
        self.prepositioned: set[int] = set()

    # ---- setup: F16's review looks -----------------------------------------------------------------

    def setup(self) -> None:
        super().setup()
        # F15: the armed platforms are kept for the engagement (none of them is sent to look)
        armed = {int(u) for u in (self.st.get("armed") or {}).values()} if self.st.get("engagement") else set()
        self.armed_units = {u: self.units.pop(u) for u in sorted(armed) if u in self.units}
        path = self.ep.contacts_path or self.ep.e.contacts
        if path:
            self.contacts = {str(c["id"]): c for c in tomllib.loads(Path(path).read_text()).get("contact") or []}
        c0 = float(self.ep.opts.calibration.contacts_start_s)
        for item in self.st.get("review_items") or []:
            ids = [item.get("contact")] + list(item.get("contacts") or []) + [item.get("claimant")]
            for cid in [x for x in ids if x]:
                c = self.contacts.get(cid)
                if c is None or cid in self.needs:
                    continue
                cc = dict(c)
                if not cc.get("route") and cc.get("at"):
                    cc["route"] = [cc["at"], cc["at"]]
                    cc["speed_mps"] = 0.0
                t_need = max(float(item.get("t_s") or 0.0) - REVIEW_LEAD_S, c0 + float(cc.get("start_s") or 0.0))
                n = Need(cc, Path2(cc, self.origin, c0), t_need)
                n.deadline = t_need + REVIEW_DUE_S
                self.needs[n.id] = n
                self.write("need.review", contact=cid, item=item.get("item"), t_need=round(t_need, 1),
                           deadline=round(n.deadline, 1))

    # ---- the wake ------------------------------------------------------------------------------------

    def wake(self, now: float) -> float | None:
        nxt = super().wake(now)
        try:
            if not self.space_done and (self.st.get("gap_needs") or self.st.get("cover_needs")):
                self.space_done = True
                self.collect(now)
                self.cover(now)
            if self.st.get("cover_needs"):
                self.hold_through(now)
            if self.st.get("engagement") and now >= self.next_engage:
                self.next_engage = now + ENGAGE_TICK_S
                self.engage(now)
        except Exception as e:  # noqa: BLE001 (the next wake tries again)
            self.errors += 1
            self.write("error.ops", error=f"{type(e).__name__}: {str(e)[:300]}")
        return nxt

    # ---- F14 -----------------------------------------------------------------------------------------

    def collect(self, now: float) -> None:
        for g in self.st.get("gap_needs") or []:
            t0 = self.epoch + now
            due = self.epoch + float(g["due_s"])
            area = {"kind": "circle", "center": list(g["center"]), "radius_km": round(float(g["radius_m"]) / 1000.0, 3)}
            ans = self.call("get_satellite_tasking_options", area=area, capabilities=["optical", "sar"],
                            rank="earliest_product", **{"from": iso(t0), "to": iso(due)})
            options = ans.get("options") or []
            ok = []
            for o in options:
                ct, pt = unix(o.get("collect_time")), unix(o.get("product_time"))
                if ct is None or pt is None or pt > due or ct < t0 + COLLECT_MIN_LEAD_S:
                    continue
                ok.append((pt, o))
            by_class: dict[str, float] = {}
            for pt, o in ok:
                k = str(o.get("class_id"))
                by_class[k] = min(by_class.get(k, pt), pt)
            self.space_options[g["id"]] = {"options": len(options), "in_time": len(ok),
                                           "classes_in_time": {k: round(v - self.epoch, 1) for k, v in sorted(by_class.items())},
                                           "capabilities_in_time": sorted({str(o.get("capability")) for _, o in ok})}
            if not ok:
                self.write("collect.none_in_time", need=g["id"], options=len(options))
                continue
            pt, o = min(ok, key=lambda x: x[0])
            chosen = {"norad_id": o["norad_id"], "collect_time": o["collect_time"]}
            req = {"task": "collect", "uids": [], "extra": {
                "area": area, "window": {"from": iso(t0), "to": iso(due)},
                "capability": str(o.get("capability") or "any"), "chosen": chosen}}
            self.propose(f"Imagery of {g.get('place')} by {iso(due)}: {o.get('satellite')} (a scoring anchor placed "
                         "from truth).", req, need=g["id"], product_s=round(pt - self.epoch, 1))

    def cover(self, now: float) -> None:
        for n in self.st.get("cover_needs") or []:
            req = {"task": "cover", "uids": [], "extra": {
                "area": n["area"], "service": n["service"], "min_level": float(n["min_level"]),
                "window": {"from": iso(self.epoch + float(n["start_s"])), "to": iso(self.epoch + float(n["end_s"]))}}}
            self.propose(f"Is GNSS good enough for {n['callsign']}'s run (a scoring anchor placed from truth)?", req,
                         need=n["id"])

    def outages(self, n: dict[str, Any]) -> list[float]:
        """The truth probe's outage samples for the run (privileged: the script's log)."""
        log = getattr(self.ep.script, "log", None) or []
        for s in log:
            if s.get("kind") == "probe" and s.get("id") == n["id"]:
                self.privileged["probe_reads"] = self.privileged.get("probe_reads", 0) + 1
                out = []
                for x in (s.get("answer") or {}).get("samples") or []:
                    t = unix(x.get("t"))
                    if t is not None and x.get("visible") is not None and int(x["visible"]) < int(n["min_level"]):
                        out.append(round(t - self.epoch, 3))
                return [t for t in out if float(n["start_s"]) - 1e-6 <= t <= float(n["end_s"]) + 1e-6]
        return []

    def hold_through(self, now: float) -> None:
        for n in self.st.get("cover_needs") or []:
            if self.holds["outages"] is None:
                if not any(s.get("kind") == "probe" for s in getattr(self.ep.script, "log", None) or []):
                    return
                self.holds["outages"] = self.outages(n)
            outs = self.holds["outages"]
            if not outs or now < float(n["start_s"]):
                continue
            uid = int(n["uid"])
            for s in outs:
                key_h, key_r = f"hold:{s}", f"resume:{s}"
                if s - HOLD_BEFORE_S <= now < s and key_h not in self.holds["acted"]:
                    self.holds["acted"].append(key_h)
                    self.propose(f"Hold {n['callsign']} through the predicted GNSS outage at {s:.0f} s (a scoring anchor "
                                 "placed from truth).", {"task": "hold", "uids": [uid]}, need=n["id"])
                if s + RESUME_AFTER_S <= now and key_r not in self.holds["acted"] and key_h in self.holds["acted"]:
                    self.holds["acted"].append(key_r)
                    later = [x for x in outs if x > s]
                    if now < float(n["end_s"]) - 60.0 and not (later and later[0] - now < 2 * HOLD_BEFORE_S):
                        self.propose(f"{n['callsign']}'s run again after the outage (a scoring anchor placed from truth).",
                                     {"task": "patrol", "uids": [uid], "route": n["route"]}, need=n["id"])

    # ---- F15 -----------------------------------------------------------------------------------------

    def preposition(self, now: float, truth: dict[str, Any]) -> None:
        """Before the hostile's arrival at its holding point (truth: the stated point and time),
        each armed platform that loiters (the Switchblade, the Magura) is sent to orbit 700 m off
        it on its own side, so a nomination's span (the dispatch delay and the transit) is short
        and the keep-out's reach regions, which grow with it, stay small."""
        e = self.st["engagement"]
        if now < float(e.get("arrives_s") or 0.0) - 150.0:
            return
        hold = (float(e["hold"][0]), float(e["hold"][1]))
        hx = to_xy(self.origin, hold)
        ours = {int(o["uid"]): o for o in truth.get("ours") or []}
        for uid, u in self.armed_units.items():
            if uid in self.prepositioned or u.platform == "fpv":
                continue
            o = ours.get(uid)
            if o is None:
                continue
            self.prepositioned.add(uid)
            u.xy = to_xy(self.origin, (float(o["lon"]), float(o["lat"])))
            # candidates 700 and 1,000 m off the point, nearest the platform first; the first
            # whose link holds (line-of-sight and mesh links, the link map), on water for a boat,
            # whose dry run passes
            cands = []
            for r in (700.0, 1000.0):
                for b in range(0, 360, 30):
                    c = (hx[0] + r * math.sin(math.radians(b)), hx[1] + r * math.cos(math.radians(b)))
                    cands.append((math.hypot(c[0] - u.xy[0], c[1] - u.xy[1]), c))
            tried = 0
            for _, c in sorted(cands):
                if tried >= 6:
                    break
                ll = to_ll(self.origin, c[0], c[1])
                if u.spec.get("domain") == "surface" and not wet_point(self.world.name, (ll[0], ll[1])):
                    continue
                if not self.link_holds(u, c):
                    continue
                tried += 1
                req = {"task": "orbit", "uids": [uid], "center": ll, "radius_m": u.orbit_m}
                try:
                    pv = self.call("preview_intent", **_mcp_args(req))
                except Exception:  # noqa: BLE001
                    continue
                if not pv.get("passed"):
                    continue
                self.propose(f"Bring {u.callsign} to loiter off the hostile's holding point (a scoring anchor placed "
                             "from truth).", req, need="engage:" + str(e["target"]))
                break
            else:
                self.write("preposition.none", uid=uid, tried=tried)

    def engage(self, now: float) -> None:
        e = self.st["engagement"]
        truth = self.ep.sim.truth(association=True)
        self.privileged["truth_reads"] += 1
        self.preposition(now, truth)
        assoc = {str(a["contact_id"]): str(a["track_id"]) for a in truth.get("association") or []}
        cs = {str(c["id"]): c for c in ((truth.get("contacts") or {}).get("contacts") or []) if c.get("exists")}
        target = e["target"]
        tid = assoc.get(target)
        tc = cs.get(target)
        if tid is None or tc is None:
            return
        tracks = {str(t["id"]): t for t in self.call("get_tracks").get("tracks") or []}
        tr = tracks.get(tid)
        if tr is None or tr.get("affiliation") != "hostile" or self.noms.get(target, 0) >= 3:
            return
        noms = self.call("get_nominations").get("nominations") or []
        if standing_on(noms, tid) is not None:
            return
        # restraint by truth: nothing that is not hostile near the target (3D)
        near = []
        for cid, c in cs.items():
            if cid == target or str(c.get("affiliation") or "").lower() == "hostile":
                continue
            d = dist_m((float(c["lon"]), float(c["lat"])), (float(tc["lon"]), float(tc["lat"])))
            dz = abs(float(c.get("alt_m") or 0.0) - float(tc.get("alt_m") or 0.0))
            near.append((math.hypot(d, dz) if dz > 0 else d, cid))
        closest = min(near, default=(1e9, None))
        if closest[0] <= CLEAR_3D_M:
            self.write("engage.wait", track=tid, near=closest[1], distance_m=round(closest[0], 1))
            return
        radius = 40.0 if closest[0] <= SMALL_RADIUS_NEAR_M else 75.0
        armed = sorted(int(u) for u in (self.st.get("armed") or {}).values())
        args: dict[str, Any] = {"track_id": tid, "effect_radius_m": radius, "window_s": 600, "fallback": "return"}
        if armed:
            args["uids"] = armed
        form = self.call("get_nomination_options", **args)
        opts = form.get("options") or []
        pick = first_passing(opts)
        # every attempt, for the dev check's account of what blocks (SCORING-V3.md 12): per
        # platform, whether it passed, its span (nomination to effect), its failing checks and
        # what blocks its keep-outs (the region the check read, the item's domain, the shortfall)
        self.write("engage.attempt", track=tid, radius_m=radius, passed=pick is not None,
                   options=[attempt_row(o) for o in opts])
        if pick is None:
            self.write("engage.none_passes", track=tid, blocking=keep_out_blockers(opts))
            return
        req = {"task": "engage", "uids": [int(pick["uid"])], "extra": {
            "track_id": tid, "effect_radius_m": radius, "window_s": 600, "fallback": "return"}}
        out = self.propose(f"{tid} is declared hostile and its contact is hostile by truth; {pick.get('callsign')} passes "
                           f"every check (a scoring anchor placed from truth).", req, target=target, radius_m=radius)
        if out is not None:
            self.noms[target] = self.noms.get(target, 0) + 1

    def describe(self) -> dict[str, Any]:
        out = super().describe()
        if self.space_options:
            out["space_options"] = self.space_options
        if self.holds["outages"] is not None:
            out["gnss_holds"] = self.holds
        if self.noms:
            out["nominations"] = self.noms
        return out
