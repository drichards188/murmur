"""Truth labels and the truth identity of a proposal's target (docs/HEADLESS.md, section 9;
CRITERIA.md). Read from an episode record, headless or full stack alike.

**Per contact**: what it is (the scenario's affiliation, kind, AIS), whether it needs a look and
by when. The rule (Harbor watch's director's notes, stated once for every family): a contact
whose truth affiliation is unknown, suspect or hostile needs a look by the time it first comes
within `LOOK_RADIUS_M` (3 km) of the protected point (the Golden Gate Bridge), plus
`LOOK_GRACE_S` (60 s); every other contact (neutral, assumed friend, friend), or one that never
comes that close, needs none. The crossing time is the first truth sample within the radius.
The point, the radius and the grace are the record's world's mission (eval/world.py; the Golden
Gate's for a record without a world, every record before world v3).

**A proposal's target**: a live track id follows detection order, so it is matched by truth: the
contact nearest the track's `predicted` position in the evidence record at its `tick_t`, if
within `ASSOC_M` (500 m) and no other contact is within `AMBIGUOUS_RATIO` (1.5) times that
distance; else `ambiguous`. Truth is the truth log's sample at the scenario second nearest
`tick_t` (CRITERIA.md), on both sides: the full stack's contacts-truth.jsonl, the simulator's own
per-step log of the same process.
"""

from __future__ import annotations

import bisect
import math
import sys
from typing import Any

from eval.world import DEFAULT_WORLD, Mission, load_world

from . import REPO

sys.path.insert(0, str(REPO / "agents"))
from playbook.rationale import parse_record  # noqa: E402

#: the Golden Gate world's mission (sim/worlds/golden-gate.toml): the bridge, mid-span (lon,
#: lat), 3 km, 60 s; a record of another world reads its own (`mission_of`)
_GG = load_world(DEFAULT_WORLD).mission
PROTECTED = _GG.center
LOOK_RADIUS_M = _GG.radius_m
LOOK_GRACE_S = _GG.grace_s
ASSOC_M = 500.0
AMBIGUOUS_RATIO = 1.5
NEEDS_LOOK = ("unknown", "suspect", "hostile")
AMBIGUOUS = "ambiguous"


def dist_m(a: tuple[float, float], b: tuple[float, float]) -> float:
    lat = math.radians((a[1] + b[1]) / 2)
    return math.hypot((a[0] - b[0]) * 111_320.0 * math.cos(lat), (a[1] - b[1]) * 111_320.0)


class Truth:
    """Contact positions over time from a record's truth samples: one per contacts step (the
    contacts process's truth log, its scenario seconds placed on the situation's clock by the
    process's start), the same on both sides."""

    def __init__(self, rec: dict[str, Any]):
        tr = rec.get("truth") or {}
        self.meta = {c["id"]: c for c in tr.get("contacts") or []}
        self.samples = sorted(tr.get("samples") or [], key=lambda s: s["t"])
        self.times = [s["t"] for s in self.samples]
        self.where: list[dict[str, tuple[float, float]]] = []
        self.series: dict[str, list[tuple[float, float, float]]] = {}
        for s in self.samples:
            here = {}
            for c in s.get("contacts") or []:
                cid, lon, lat, exists = c[0], c[1], c[2], c[4] if len(c) > 4 else True
                if exists:
                    here[cid] = (lon, lat)
                    self.series.setdefault(cid, []).append((s["t"], lon, lat))
            self.where.append(here)

    def nearest(self, t: float) -> int | None:
        """The sample at the scenario second nearest `t` (the earlier of two equally near), if
        one is within a step and a half."""
        if not self.times:
            return None
        i = bisect.bisect_left(self.times, t)
        cands = [j for j in (i - 1, i) if 0 <= j < len(self.times)]
        j = min(cands, key=lambda k: (abs(self.times[k] - t), k))
        return j if abs(self.times[j] - t) <= 1.5 else None

    def at(self, cid: str, t: float) -> tuple[float, float] | None:
        """Where contact `cid` was at the scenario second nearest `t` (None if it did not exist
        then, or no sample is near)."""
        j = self.nearest(t)
        return None if j is None else self.where[j].get(cid)

    def first_within(self, cid: str, point: tuple[float, float], radius_m: float) -> float | None:
        """The first sample's time with the contact within `radius_m` of `point`."""
        for t, lon, lat in self.series.get(cid) or []:
            if dist_m((lon, lat), point) <= radius_m:
                return t
        return None


def mission_of(rec: dict[str, Any]) -> Mission:
    """The mission geometry of a record's world (`world.name`; a record without one is the
    Golden Gate's, as every record before world v3)."""
    return load_world((rec.get("world") or {}).get("name") or DEFAULT_WORLD).mission


def contact_labels(rec: dict[str, Any], truth: Truth | None = None) -> dict[str, dict[str, Any]]:
    truth = truth or Truth(rec)
    mission = mission_of(rec)
    out = {}
    for cid, m in sorted(truth.meta.items()):
        aff = str(m.get("affiliation") or "").lower()
        within = truth.first_within(cid, mission.center, mission.radius_m)
        needs = aff in NEEDS_LOOK and within is not None
        out[cid] = {
            "affiliation": aff, "kind": m.get("kind"), "ais": bool(m.get("ais")),
            "first_within_s": round(within, 3) if within is not None else None,
            "needs_look": needs,
            "look_by_s": round(within + mission.grace_s, 3) if needs and within is not None else None,
        }
    return out


def associate(truth: Truth, predicted: list[float] | None, t: float) -> str | None:
    """The truth contact a track's position names at situation second `t` (or `ambiguous`;
    None without a position)."""
    if not predicted:
        return None
    p = (float(predicted[0]), float(predicted[1]))
    ds = []
    j = truth.nearest(t)
    for cid in (truth.where[j] if j is not None else {}):
        q = truth.at(cid, t)
        if q is not None:
            ds.append((dist_m(p, q), cid))
    if not ds:
        return AMBIGUOUS
    ds.sort()
    d0, c0 = ds[0]
    if d0 > ASSOC_M:
        return AMBIGUOUS
    if len(ds) > 1 and ds[1][0] <= AMBIGUOUS_RATIO * max(d0, 1e-6):
        return AMBIGUOUS
    return c0


def track_position(record: dict[str, Any], track: str) -> list[float] | None:
    for f in record.get("facts") or []:
        if f.get("ref") == f"track:{track}.kinematics":
            return f.get("predicted")
    return None


def is_track(target: Any) -> bool:
    return isinstance(target, str) and target.startswith("T-")


def proposals(rec: dict[str, Any], to_situation: Any) -> list[dict[str, Any]]:
    """Every proposal of the playbook's log, with its evidence record's time on the situation's
    clock (`to_situation(bridge_t)`), its play, target, truth target and vehicle; and every
    proposal of the model agent's (`source` model, officer.py), with the play category it maps
    to, the time it was proposed (the driver read the picture at that instant), and its
    target's position and picture as read then. The same matching rule for both."""
    truth = Truth(rec)
    assoc = (rec.get("truth") or {}).get("association") or {}
    out = []
    for ln in rec.get("agent_log") or []:
        if ln.get("kind") != "proposal":
            continue
        if ln.get("source") == "model":
            out.append(model_proposal(ln, truth, assoc))
            continue
        ev = parse_record(str(ln.get("rationale") or "")) or {}
        tick = ev.get("tick_t")
        t = to_situation(float(tick)) if isinstance(tick, (int, float)) else ln.get("t")
        target = ln.get("target")
        truth_target = None
        if is_track(target):
            truth_target = associate(truth, track_position(ev, target), t)
        elif target is not None:
            truth_target = str(target)
        out.append({
            "t": round(float(t), 3) if t is not None else None,
            "intent": ln.get("intent"), "play": ln.get("play"), "version": ln.get("version"),
            "target": target, "truth_target": truth_target,
            "tracker_target": assoc.get(target) if is_track(target) else None,
            "uid": ln.get("uid"), "task": (ln.get("request") or {}).get("task"),
            "state": ln.get("state"), "refused": ln.get("refused"),
            "tier": ((ev.get("preview") or {}).get("tier")),
            "rationale": ln.get("rationale"),
            "picture": picture_of(ev, target),
            "source": ln.get("source"),
        })
    return out


def model_proposal(ln: dict[str, Any], truth: Truth, assoc: dict[str, Any]) -> dict[str, Any]:
    """A model proposal (officer.py's `proposal` line) in the playbook's proposal shape. Its
    target's truth identity by the same rule (`associate`, the track's position as the picture
    showed it at the proposal); an aim between two tracks shown (`aim` ambiguous) is ambiguous;
    an unmapped proposal (`unjudged:<task>`) keeps its kind as its play."""
    t = ln.get("t")
    target = ln.get("target")
    truth_target = None
    if ln.get("aim") == AMBIGUOUS:
        truth_target = AMBIGUOUS
    elif is_track(target):
        truth_target = associate(truth, ln.get("predicted"), float(t)) if t is not None else None
    elif target is not None:
        truth_target = str(target)
    return {
        "t": round(float(t), 3) if t is not None else None,
        "intent": ln.get("intent"), "play": ln.get("play"), "version": None,
        "target": target, "truth_target": truth_target,
        "tracker_target": assoc.get(target) if is_track(target) else None,
        "uid": ln.get("uid"), "task": (ln.get("request") or {}).get("task"),
        "state": ln.get("state"), "refused": ln.get("refused"), "tier": ln.get("tier"),
        "rationale": ln.get("rationale"), "picture": ln.get("picture") if is_track(target) else None,
        "source": "model", "lane": ln.get("lane"), "mapping": ln.get("mapping"),
    }


def picture_of(ev: dict[str, Any], target: Any) -> dict[str, Any] | None:
    """What the picture showed of the target when the playbook proposed (its evidence record):
    the affiliation and its confidence, the identity and its confidence."""
    if not is_track(target):
        return None
    aff = ident = None
    for f in ev.get("facts") or []:
        if f.get("ref") == f"track:{target}.affiliation":
            aff = f
        elif f.get("ref") == f"track:{target}.identity":
            ident = f
    return {
        "affiliation": (aff or {}).get("value"), "affiliation_confidence": (aff or {}).get("conf"),
        "declared": (aff or {}).get("declared"),
        "identity": (ident or {}).get("value"), "identity_confidence": (ident or {}).get("conf"),
    }
