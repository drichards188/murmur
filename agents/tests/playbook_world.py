"""A small world for the playbook engine's tests: reads shaped as the bridge's MCP tools answer
them (checked against recorded reads in fixtures/playbook/harbor-watch-reads.json), and a fake
session that serves them and records every call. No bridge, no model."""

from __future__ import annotations

import copy
import json
import math
from pathlib import Path
from typing import Any

from playbook.config import make
from playbook.reads import Reads

FIXTURES = Path(__file__).resolve().parent / "fixtures" / "playbook"
GATE = (-122.4783, 37.8199)
T0 = 1_791_556_700.0
M_PER_DEG = 111_320.0

PLATFORMS = {"t": T0, "platforms": [
    {"id": "puma", "domain": "air", "cruise_mps": 20, "min_turn_radius_m": None, "tasks": ["hold", "orbit", "patrol", "rtl"],
     "sensors": [{"type": "eo_ir", "range_m": 4000}]},
    {"id": "mq9b", "domain": "air", "cruise_mps": 90, "min_turn_radius_m": 1719, "tasks": ["hold", "orbit", "patrol", "rtl"],
     "sensors": [{"type": "radar", "range_m": 100000}, {"type": "eo_ir", "range_m": 40000}]},
    {"id": "seahunter", "domain": "surface", "cruise_mps": 6, "min_turn_radius_m": 86, "tasks": ["hold", "orbit", "patrol", "rtl"],
     "sensors": [{"type": "radar", "range_m": 40000}, {"type": "eo_ir", "range_m": 15000}]},
    {"id": "switchblade", "domain": "air", "cruise_mps": 30, "min_turn_radius_m": 90, "tasks": ["hold", "orbit", "rtl"],
     "sensors": [{"type": "eo_ir", "range_m": 3000}]},
]}
ARMED = {"switchblade"}
POLICY = {"policy": {"rules": {"reserve_pct": 20.0, "task_duration_s": 900}, "approval": {"expire_s": 120}}}
WATCH = {
    "sensor_platform": "puma",
    "watch_area": [{"name": "the Golden Gate approaches", "center": list(GATE), "radius_m": 3000}],
    "protected": [{"name": "the Golden Gate Bridge", "point": list(GATE)}],
}


def config(**over: Any):
    table = copy.deepcopy(WATCH)
    table.update(over)
    return make(table, "test")


def offset(east_m: float, north_m: float, ref: tuple[float, float] = GATE) -> list[float]:
    """A [lon, lat] this many metres east and north of `ref`."""
    return [round(ref[0] + east_m / (M_PER_DEG * math.cos(math.radians(ref[1]))), 6), round(ref[1] + north_m / M_PER_DEG, 6)]


def vehicle(uid: int, callsign: str, platform: str = "puma", pos: list[float] | None = None, **over: Any) -> dict[str, Any]:
    domain = next(p["domain"] for p in PLATFORMS["platforms"] if p["id"] == platform)
    pos = pos or offset(-1500, 300)
    v = {"uid": uid, "callsign": callsign, "platform": platform, "domain": domain, "armed": platform in ARMED,
         "mode": "LOITER", "lon": pos[0], "lat": pos[1], "alt_rel_m": 120.0, "position_age_s": 0.3, "nav_ready": True,
         "nav_source": "gnss", "gps_glitch": False, "battery_pct": 90.0, "link_lost": False, "link_age_s": 0.2, "task": None,
         "stale": ["home"]}
    v.update(over)
    return v


def track(tid: str, affiliation: str = "suspect", conf: float = 0.7, pos: list[float] | None = None,
          vel: tuple[float, float] = (12.0, 0.0), **over: Any) -> dict[str, Any]:
    pos = pos or offset(-2000, -300)
    speed = math.hypot(*vel)
    t = {"id": tid, "domain": "surface", "position": pos, "velocity_mps": list(vel), "speed_mps": round(speed, 1),
         "course_deg": round(math.degrees(math.atan2(vel[0], vel[1])) % 360, 0), "pos_sigma_m": 5.0,
         "affiliation": affiliation, "affiliation_confidence": conf,
         "affiliation_reason": over.pop("reason", "fast small craft, no AIS" if affiliation == "suspect" else "AIS identity"),
         "identity": {"kind": over.pop("kind", "small_fast_boat"), "confidence": over.pop("kind_conf", conf),
                      "ais": over.pop("ais", None), "conflict": over.pop("conflict", None), "supported_by": ["radar", "eo_ir"]},
         "sources": over.pop("sources", [{"uid": 1, "sensor": "radar", "hits": 40, "last_t": T0 - 1.0}]),
         "state": "active", "declared": over.pop("declared", None), "review": [], "age_s": 1.0, "stale": False,
         "pos_sigma_now_m": 6.0, "confidence_now": 0.95, "predicted": pos}
    t.update(over)
    if t["stale"]:
        t["state"] = "stale"
    return t


def intent(iid: int, kind: str, uids: list[int], state: str = "pending_approval", **over: Any) -> dict[str, Any]:
    i = {"id": iid, "kind": kind, "uids": [str(u) for u in uids], "state": state, "source": {"kind": "agent", "id": "agent-playbook"},
         "params": over.pop("params", {}), "rationale": over.pop("rationale", ""), "approval": over.pop("approval", None),
         "history": over.pop("history", [{"state": state, "t": T0 * 1000, "reason": state}])}
    i.update(over)
    return i


def reads(t: float = T0, vehicles: list[dict] | None = None, tracks: list[dict] | None = None, pending: list[dict] | None = None,
          alerts: list[dict] | None = None, intents: list[dict] | None = None, changes: list[dict] | None = None,
          lost_full: dict[int, dict] | None = None, policy: dict | None = None) -> Reads:
    return Reads(
        fleet={"t": t, "count": len(vehicles or []), "compact": True, "vehicles": vehicles or []},
        tracks={"t": t, "tracks": tracks or [], "lost": []},
        inbox={"t": t, "pending": pending or [], "alerts": alerts or [], "queued": []},
        intents={"t": t, "intents": intents or []},
        changes=changes or [], platforms=PLATFORMS, policy=policy or POLICY, lost_full=lost_full or {}, changes_t=t,
    )


def recorded() -> dict[str, Any]:
    return json.loads((FIXTURES / "harbor-watch-reads.json").read_text())


def recorded_reads(doc: dict[str, Any], i: int, intents: list[dict] | None = None) -> Reads:
    tick = doc["ticks"][i]
    return Reads(fleet=tick["get_fleet"], tracks=tick["get_tracks"], inbox=tick["get_inbox"],
                 intents={"t": tick["get_fleet"]["t"], "intents": intents or []}, changes=tick["changes"],
                 platforms=doc["get_platforms"], policy=doc["get_policy"], changes_t=tick["changes_t"])


class FakeSession:
    """Serves one Reads per tick (in order), answers previews from `previews` (a callable of the
    request, or a dict; by default a hold or an rtl previews auto tier, anything else human),
    gives proposals ids from 101 (refused at submission where `refuse` names a reason), and
    records every call with its args."""

    agent = "agent-playbook"

    def __init__(self, ticks: list[Reads], previews: Any = None, intents: Any = None, *, refuse: Any = None,
                 run: str = "test", areas: dict | None = None, feed: str = "f"):
        self.ticks = list(ticks)
        self.i = -1
        self.calls: list[tuple[str, dict[str, Any]]] = []
        self.previews = previews
        self.next_id = 101
        self.proposed: list[dict[str, Any]] = []
        self.withdrawn: list[dict[str, Any]] = []
        self.intent_states = intents or {}  # tick index -> {intent id: state}
        self.refuse = refuse  # request -> a refusal's summary, or None
        self.run = run
        self.feed = feed  # the change feed's id: a new one with every bridge process
        self.areas = areas or {"areas": []}
        self.truncate_at: set[int] = set()  # ticks whose change feed says truncated
        self.nominations: list[dict[str, Any]] = []
        self.explained: dict[int, dict[str, Any]] = {}  # id -> intent for explain_intent
        self.reasons: dict[int, str] = {}  # id -> the reason of its last history entry
        self.decided_meanwhile: dict[int, str] = {}  # id -> the state a withdraw finds (a race)
        self.page: dict[int, set[int]] = {}  # tick -> the ids get_intents shows (the newest page)
        self.explain_raise: dict[int, BaseException] = {}  # id -> what explain_intent raises (once each)
        # get_link_coverage: a callable of one query (uid, order) to its vehicle's answer, or None
        # (the default: no answer, so a look ranks as without the tool)
        self.coverage: Any = None

    def advance(self) -> Reads:
        self.i += 1
        return self.ticks[min(self.i, len(self.ticks) - 1)]

    def own_intents(self, t: float) -> dict[str, Any]:
        states = self.intent_states.get(self.i, {})
        if self.i in self.page:
            return {"t": t, "intents": [x for x in self._own(t, states) if x["id"] in self.page[self.i]]}
        return {"t": t, "intents": self._own(t, states)}

    def _own(self, t: float, states: dict[int, str]) -> list[dict[str, Any]]:
        out = []
        for p in self.proposed:
            st = states.get(p["id"], p["state"])
            out.append(intent(p["id"], p["request"]["task"], p["request"]["uids"], state=st, rationale=p["rationale"],
                              params={k: v for k, v in p["request"].items() if k in ("center", "radius_m", "route")},
                              approval=({"by": "op-1", "note": "not now"} if st == "rejected" else None),
                              history=[{"state": st, "t": t * 1000, "reason": self.reasons.get(p["id"], st)}]))
        return out

    async def call(self, name: str, **args: Any) -> dict[str, Any]:
        self.calls.append((name, dict(args)))
        r = self.ticks[max(0, min(self.i, len(self.ticks) - 1))]
        if name == "get_fleet" and args.get("compact"):
            r = self.advance()
            return r.fleet
        if name == "get_fleet":
            return {"t": r.fleet["t"], "vehicles": [r.lost_full.get(int(u), {"uid": u}) for u in args.get("uids") or []]}
        if name == "get_tracks":
            return r.tracks
        if name == "get_inbox":
            return r.inbox
        if name == "get_intents":
            return self.own_intents(r.fleet["t"]) if self.proposed or self.intent_states else (r.intents or {"t": r.fleet["t"], "intents": []})
        if name == "get_changes":
            return {"t": r.changes_t or r.fleet["t"], "run": self.run, "cursor": f"{self.feed}:{self.i}", "more": False,
                    "reset": False, "truncated": bool(args.get("since")) and self.i in self.truncate_at,
                    "changes": r.changes if args.get("since") else []}
        if name == "get_areas":
            return self.areas
        if name == "get_link_coverage":
            qs = args.get("queries") or []
            if self.coverage is None:
                return {"answers": [{} for _ in qs]}
            return {"answers": [{"vehicles": [self.coverage(q)]} for q in qs]}
        if name == "get_nominations":
            return {"nominations": self.nominations}
        if name == "get_platforms":
            return r.platforms
        if name == "get_policy":
            return r.policy
        if name == "explain_intent" and int(args["id"]) in self.explain_raise:
            raise self.explain_raise.pop(int(args["id"]))
        if name == "explain_intent" and int(args["id"]) in self.explained:
            return {"intent": self.explained[int(args["id"])]}
        if name == "explain_intent" and any(p["id"] == int(args["id"]) for p in self.proposed):
            return {"intent": next(x for x in self._own(r.fleet["t"], self.intent_states.get(self.i, {}))
                                   if x["id"] == int(args["id"]))}
        if name == "explain_intent":
            return {"intent": {"kind": "orbit", "params": {"center": offset(500, 900), "radius_m": 300.0},
                               "source": {"kind": "human", "id": "op-setup"}}}
        if name == "preview_intent":
            if self.previews is None:
                pv = passing("auto_approved", "auto", "safety-commands") if args.get("task") in ("hold", "rtl") else passing()
            else:
                pv = self.previews(args) if callable(self.previews) else self.previews
            return copy.deepcopy(pv)
        if name == "withdraw_intent":
            # the bridge voids a withdrawn intent (there is no `withdrawn` state); with if_state,
            # only from that state, else 409 and nothing withdrawn
            p = next((p for p in self.proposed if p["id"] == args["id"]), None)
            st = self.intent_states.get(self.i, {}).get(args["id"], p["state"]) if p else None
            st = self.decided_meanwhile.get(args["id"], st)
            if args.get("if_state") and st != args["if_state"]:
                raise RuntimeError(f"409: {args['id']} is {st}, not {args['if_state']}: not withdrawn (if_state)")
            self.withdrawn.append(dict(args))
            if p is not None:
                p["state"] = "voided"
            for states in self.intent_states.values():
                if states.get(args["id"]) == "pending_approval":
                    states[args["id"]] = "voided"
            return {"id": args["id"], "state": "voided"}
        raise AssertionError(f"unexpected call {name}")

    async def propose(self, rationale: str, **req: Any) -> dict[str, Any]:
        self.calls.append(("propose_intent", {"rationale": rationale, **req}))
        iid = self.next_id
        self.next_id += 1
        refused = self.refuse(req) if self.refuse else None
        state = "rejected" if refused else ("auto_approved" if req["task"] in ("hold", "rtl") else "pending_approval")
        self.proposed.append({"id": iid, "request": dict(req), "rationale": rationale, "state": state})
        out = {"id": iid, "state": state, "dispatched": state == "auto_approved"}
        if refused:
            out["refused"] = refused  # as policy_agent.AgentSession.propose passes the bridge's error
        return out

    def writers(self) -> list[str]:
        return [n for n, _ in self.calls if n in ("preview_intent", "propose_intent", "withdraw_intent")]


def passing(would_reach: str = "pending_approval", tier: str = "human", rule: str = "agent-tasking",
            checks: list[dict] | None = None) -> dict[str, Any]:
    checks = checks if checks is not None else [{"rule": r, "uid": "3", "outcome": "pass", "detail": "ok"} for r in
                                                ("capability", "readiness", "platform_limits", "domain", "geofence", "reserves")]
    passed = all(c["outcome"] != "fail" for c in checks)
    return {"checks": checks, "passed": passed, "policy": {"tier": tier, "rule": rule}, "would_reach": would_reach if passed else "rejected"}
