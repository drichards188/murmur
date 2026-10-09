"""Truth labels and the truth identity of a proposal's target (docs/HEADLESS.md, section 9;
CRITERIA.md). Read from an episode record, headless or full stack alike.

**Per contact**: what it is (the scenario's affiliation, kind, AIS), whether it needs a look and
by when. The rule (Harbor watch's director's notes, stated once for every family): a contact
whose truth affiliation is unknown, suspect or hostile needs a look by the time it first comes
within `LOOK_RADIUS_M` (3 km) of the protected point (the Golden Gate Bridge), plus
`LOOK_GRACE_S` (60 s); every other contact (neutral, assumed friend, friend), or one that never
comes that close, needs none. The crossing time is interpolated between truth samples.

**A proposal's target**: a live track id follows detection order, so it is matched by truth: the
contact nearest the track's `predicted` position in the evidence record at its `tick_t`, if
within `ASSOC_M` (500 m) and no other contact is within `AMBIGUOUS_RATIO` (1.5) times that
distance; else `ambiguous`. Truth positions are interpolated to `tick_t`.
"""

from __future__ import annotations

import bisect
import math
import sys
from typing import Any

from . import REPO

sys.path.insert(0, str(REPO / "agents"))
from playbook.rationale import parse_record  # noqa: E402

PROTECTED = (-122.4783, 37.8199)  # the Golden Gate Bridge, mid-span (lon, lat)
LOOK_RADIUS_M = 3000.0
LOOK_GRACE_S = 60.0
ASSOC_M = 500.0
AMBIGUOUS_RATIO = 1.5
NEEDS_LOOK = ("unknown", "suspect", "hostile")
AMBIGUOUS = "ambiguous"


def dist_m(a: tuple[float, float], b: tuple[float, float]) -> float:
    lat = math.radians((a[1] + b[1]) / 2)
    return math.hypot((a[0] - b[0]) * 111_320.0 * math.cos(lat), (a[1] - b[1]) * 111_320.0)


class Truth:
    """Contact positions over time from a record's truth samples."""

    def __init__(self, rec: dict[str, Any]):
        tr = rec.get("truth") or {}
        self.meta = {c["id"]: c for c in tr.get("contacts") or []}
        self.samples = sorted(tr.get("samples") or [], key=lambda s: s["t"])
        self.times = [s["t"] for s in self.samples]
        self.series: dict[str, list[tuple[float, float, float]]] = {}
        for s in self.samples:
            for c in s.get("contacts") or []:
                cid, lon, lat, exists = c[0], c[1], c[2], c[4] if len(c) > 4 else True
                if exists:
                    self.series.setdefault(cid, []).append((s["t"], lon, lat))

    def at(self, cid: str, t: float) -> tuple[float, float] | None:
        """Where contact `cid` was at `t` (linear between samples; None outside its span)."""
        ser = self.series.get(cid)
        if not ser:
            return None
        ts = [x[0] for x in ser]
        i = bisect.bisect_left(ts, t)
        if i < len(ser) and abs(ser[i][0] - t) < 1e-9:
            return (ser[i][1], ser[i][2])
        if i == 0 or i >= len(ser):
            # within one sample interval of the ends: the end sample
            j = 0 if i == 0 else len(ser) - 1
            return (ser[j][1], ser[j][2]) if abs(ser[j][0] - t) <= 2.5 else None
        (t0, lo0, la0), (t1, lo1, la1) = ser[i - 1], ser[i]
        if t1 - t0 > 5.0:
            return None
        f = (t - t0) / (t1 - t0)
        return (lo0 + f * (lo1 - lo0), la0 + f * (la1 - la0))

    def first_within(self, cid: str, point: tuple[float, float], radius_m: float) -> float | None:
        ser = self.series.get(cid) or []
        prev = None
        for t, lon, lat in ser:
            d = dist_m((lon, lat), point)
            if d <= radius_m:
                if prev is None:
                    return t
                tp, dp = prev
                f = (dp - radius_m) / (dp - d) if dp != d else 1.0
                return tp + f * (t - tp)
            prev = (t, d)
        return None


def contact_labels(rec: dict[str, Any], truth: Truth | None = None) -> dict[str, dict[str, Any]]:
    truth = truth or Truth(rec)
    out = {}
    for cid, m in sorted(truth.meta.items()):
        aff = str(m.get("affiliation") or "").lower()
        within = truth.first_within(cid, PROTECTED, LOOK_RADIUS_M)
        needs = aff in NEEDS_LOOK and within is not None
        out[cid] = {
            "affiliation": aff, "kind": m.get("kind"), "ais": bool(m.get("ais")),
            "first_within_s": round(within, 3) if within is not None else None,
            "needs_look": needs,
            "look_by_s": round(within + LOOK_GRACE_S, 3) if needs and within is not None else None,
        }
    return out


def associate(truth: Truth, predicted: list[float] | None, t: float) -> str | None:
    """The truth contact a track's position names at situation second `t` (or `ambiguous`;
    None without a position)."""
    if not predicted:
        return None
    p = (float(predicted[0]), float(predicted[1]))
    ds = []
    for cid in truth.series:
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
    clock (`to_situation(bridge_t)`), its play, target, truth target and vehicle."""
    truth = Truth(rec)
    assoc = (rec.get("truth") or {}).get("association") or {}
    out = []
    for ln in rec.get("agent_log") or []:
        if ln.get("kind") != "proposal":
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
        })
    return out


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
