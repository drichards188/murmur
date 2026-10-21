"""Scoring v2's part of an episode record (eval/SCORING-V2.md, sections 2.6 and 7): the picture
sampler's compact encoding and exact accessors over it, the audit log's digest the labels read,
the invariants, and loading a record from disk.

**The picture sampler** (episode.py, v2 episodes only: `Options.scoring = "v2"`) asks the
simulator for the picture every `PICTURE_DT` (1 s) of situation time (`Sim.picture`, a request
that never moves the clock): every track as the bridge serves it (`Track::json`, what `GET
/api/tracks` answers), the tracker's association of tracks to contacts, the inbox's alerts, and
our vehicles' truth; the first sample also carries each vehicle's platform and sensors and the
sensing model's seed and misclassification rate. Nothing of it reaches an agent.

**The encoding** (`murmur-picture/1`) keeps every value the sampler saw, as change points: a row
is written only when a value differs from the sample before, so the state at any sample is the
last row at or before it, exactly. Sample `k` is at situation time `times[k]`.

    times   [t_0, t_1, ...]
    tracks  {track id: {
              domain,
              span   [[k_first, k_last], ...]       the samples it was in the picture
              ident  [[k, kind, confidence, conflict, [supported_by]], ...]
              aff    [[k, affiliation, confidence], ...]
              kin    [[k, lon, lat, v_east, v_north, last_seen_s], ...]
              src    {"<uid>:<sensor>": [[k, hits, last_s] or [k, hits, last_s, n], ...]}}}
    assoc   {track id: [[k, contact id or null], ...]}
    alerts  {"<alert id>@<since_s>": {kind, uid, intent, since_s, span: [[k0, k1], ...]}}
    ours    {uid: [[k, lon, lat, alt_m, speed_mps, mode, armed, battery_pct, link_blocked], ...]}
    homes   {uid: [[k, lon, lat], ...]}
    fleet   [{uid, callsign, platform, sensors: [{type, range_m, height}]}]
    sensing {seed, misclassification_rate, seed_from_env, rate_from_env}

A source row with a fourth element `n` is a run: at samples `k + j` (j < n) the hits were
`hits + j` and `last_s` was `last_s + j` (one detection a second, the contacts' rate); written
only when every sample of the run held exactly that, so it decodes to the same values. Times
(`last_seen_s`, `last_s`, `since_s`) are situation seconds (the bridge's clock minus the epoch).

Size, measured on the v1 Harbor watch base (`fast-unknown`, 720 s, 8 tracks, 4 vehicles, 721
samples): the picture about 0.64 MB of JSON (tracks 0.43 MB, our vehicles 0.2 MB), 0.15 MB
gzipped; the whole v2 record (without the audit text, as the batch keeps it) about 1.1 MB, 0.22 MB
gzipped. The batch writes v2 records gzipped (`<name>.json.gz`). The v2 world's radar-and-AIS
standing sensors give fewer sources per track, so less.
"""

from __future__ import annotations

import bisect
import gzip
import json
from pathlib import Path
from typing import Any

ENCODING = "murmur-picture/1"
PICTURE_DT = 1.0
CLASSIFYING = ("eo_ir", "sonar")
EPS = 1e-6


def sit_s(wall: float | None, epoch: float) -> float | None:
    """A bridge wall-clock time as situation seconds."""
    return None if wall is None else round(float(wall) - epoch, 3)


def _r(x: Any, nd: int) -> Any:
    return None if x is None else round(float(x), nd)


class PictureEncoder:
    """Builds the encoding from successive `Sim.picture` answers (the first with `homes`)."""

    def __init__(self, epoch: float):
        self.epoch = float(epoch)
        self.times: list[float] = []
        self.tracks: dict[str, dict[str, Any]] = {}
        self.assoc: dict[str, list[list[Any]]] = {}
        self.alerts: dict[str, dict[str, Any]] = {}
        self.ours: dict[str, list[list[Any]]] = {}
        self.homes: dict[str, list[list[Any]]] = {}
        self.fleet: list[dict[str, Any]] | None = None
        self.sensing: dict[str, Any] | None = None
        self._last: dict[tuple[str, ...], tuple[Any, ...]] = {}

    def _change(self, rows: list[list[Any]], key: tuple[str, ...], k: int, values: tuple[Any, ...]) -> None:
        if self._last.get(key) != values:
            self._last[key] = values
            rows.append([k, *values])

    @staticmethod
    def _span(spans: list[list[int]], k: int) -> None:
        if spans and spans[-1][1] == k - 1:
            spans[-1][1] = k
        else:
            spans.append([k, k])

    def _source(self, rows: list[list[Any]], key: tuple[str, ...], k: int, hits: int, last_s: float) -> None:
        if self._last.get(key) == (hits, last_s):
            return
        self._last[key] = (hits, last_s)
        if rows:
            r = rows[-1]
            k0, h0, l0 = r[0], r[1], r[2]
            n = r[3] if len(r) > 3 else 1
            if k == k0 + n and hits == h0 + n and last_s == round(l0 + n, 3):
                if len(r) > 3:
                    r[3] = n + 1
                else:
                    r.append(2)
                return
        rows.append([k, hits, last_s])

    def add(self, p: dict[str, Any]) -> None:
        k = len(self.times)
        self.times.append(round(float(p["t"]), 3))
        if p.get("fleet") is not None and self.fleet is None:
            self.fleet = p["fleet"]
        if p.get("sensing") is not None and self.sensing is None:
            self.sensing = p["sensing"]
        for tr in p.get("tracks") or []:
            tid = str(tr["id"])
            T = self.tracks.setdefault(tid, {"domain": tr.get("domain"), "span": [], "ident": [], "aff": [],
                                             "kin": [], "src": {}})
            self._span(T["span"], k)
            idn = tr.get("identity") or {}
            self._change(T["ident"], (tid, "ident"), k,
                         (idn.get("kind"), _r(idn.get("confidence"), 3), idn.get("conflict"),
                          list(idn.get("supported_by") or [])))
            self._change(T["aff"], (tid, "aff"), k, (tr.get("affiliation"), _r(tr.get("affiliation_confidence"), 3)))
            pos = tr.get("position") or [None, None]
            vel = tr.get("velocity_mps") or [None, None]
            self._change(T["kin"], (tid, "kin"), k, (_r(pos[0], 6), _r(pos[1], 6), _r(vel[0], 2), _r(vel[1], 2),
                                                     sit_s(tr.get("last_seen_t"), self.epoch)))
            for s in tr.get("sources") or []:
                skey = f"{s['uid']}:{s['sensor']}"
                rows = T["src"].setdefault(skey, [])
                self._source(rows, (tid, "src", skey), k, int(s["hits"]), sit_s(s.get("last_t"), self.epoch) or 0.0)
        assoc = {str(a[0]): str(a[1]) for a in p.get("association") or []}
        for tid in set(assoc) | set(self.assoc):
            rows = self.assoc.setdefault(tid, [])
            self._change(rows, (tid, "assoc"), k, (assoc.get(tid),))
        for a in p.get("alerts") or []:
            since = sit_s(a.get("since_t"), self.epoch)
            key = f"{a.get('id')}@{since}"
            A = self.alerts.setdefault(key, {"kind": a.get("kind"), "uid": a.get("uid"), "intent": a.get("intent_id"),
                                             "since_s": since, "span": []})
            self._span(A["span"], k)
        for o in p.get("ours") or []:
            uid = str(o["uid"])
            rows = self.ours.setdefault(uid, [])
            self._change(rows, (uid, "ours"), k, (_r(o.get("lon"), 6), _r(o.get("lat"), 6), _r(o.get("alt_m"), 1),
                                                  _r(o.get("speed_mps"), 2), o.get("mode"), bool(o.get("armed")),
                                                  _r(o.get("battery_pct"), 3), o.get("link_blocked")))
            home = o.get("home") or [None, None]
            self._change(self.homes.setdefault(uid, []), (uid, "home"), k, (_r(home[0], 6), _r(home[1], 6)))

    def finish(self) -> dict[str, Any]:
        return {"encoding": ENCODING, "dt": PICTURE_DT, "times": self.times, "tracks": self.tracks,
                "assoc": self.assoc, "alerts": self.alerts, "ours": self.ours, "homes": self.homes,
                "fleet": self.fleet or [], "sensing": self.sensing or {}}


class Picture:
    """Exact reads of an encoded picture: the state at a sample is the last change point at or
    before it. `t` arguments are situation seconds (the sample at or before `t` answers);
    `k` arguments are sample indices."""

    def __init__(self, enc: dict[str, Any]):
        if enc.get("encoding") != ENCODING:
            raise ValueError(f"not a {ENCODING} picture: {enc.get('encoding')!r}")
        self.enc = enc
        self.times: list[float] = list(enc["times"])
        self.tracks: dict[str, dict[str, Any]] = enc["tracks"]
        self._keys: dict[int, list[int]] = {}
        self._assoc_at: list[dict[str, str]] | None = None
        self._rev_at: list[dict[str, str]] | None = None

    # ---- time ----------------------------------------------------------------------------

    def index(self, t: float | None) -> int | None:
        """The sample at or before situation time `t` (None before the first)."""
        if t is None:
            return None
        k = bisect.bisect_right(self.times, float(t) + EPS) - 1
        return k if k >= 0 else None

    def index_after(self, t: float) -> int | None:
        """The first sample at or after `t` (None after the last)."""
        k = bisect.bisect_left(self.times, float(t) - EPS)
        return k if k < len(self.times) else None

    def time(self, k: int) -> float:
        return self.times[k]

    def _row(self, rows: list[list[Any]], k: int | None) -> list[Any] | None:
        if k is None or not rows:
            return None
        ks = self._keys.get(id(rows))
        if ks is None:
            ks = [r[0] for r in rows]
            self._keys[id(rows)] = ks
        i = bisect.bisect_right(ks, k) - 1
        return rows[i] if i >= 0 else None

    # ---- tracks --------------------------------------------------------------------------

    def present(self, tid: str, k: int | None) -> bool:
        T = self.tracks.get(tid)
        if T is None or k is None:
            return False
        return any(a <= k <= b for a, b in T["span"])

    def track_ids(self, k: int | None) -> list[str]:
        return sorted(tid for tid in self.tracks if self.present(tid, k))

    def identity(self, tid: str, k: int | None) -> dict[str, Any] | None:
        if not self.present(tid, k):
            return None
        r = self._row(self.tracks[tid]["ident"], k)
        if r is None:
            return None
        return {"kind": r[1], "confidence": r[2], "conflict": r[3], "supported_by": list(r[4])}

    def affiliation(self, tid: str, k: int | None) -> tuple[str | None, float | None] | None:
        if not self.present(tid, k):
            return None
        r = self._row(self.tracks[tid]["aff"], k)
        return None if r is None else (r[1], r[2])

    def kinematics(self, tid: str, k: int | None) -> dict[str, Any] | None:
        if not self.present(tid, k):
            return None
        r = self._row(self.tracks[tid]["kin"], k)
        if r is None:
            return None
        vel = None if r[3] is None or r[4] is None else (r[3], r[4])
        return {"position": (r[1], r[2]), "velocity": vel, "last_seen_s": r[5]}

    @staticmethod
    def _source_value(r: list[Any] | None, k: int) -> tuple[int, float] | None:
        if r is None:
            return None
        n = r[3] if len(r) > 3 else 1
        j = min(k - r[0], n - 1)
        return (r[1] + j, round(r[2] + j, 3))

    def source(self, tid: str, key: str, k: int | None) -> tuple[int, float] | None:
        """(hits, last_s) of source `key` ("<uid>:<sensor>") on track `tid` at sample `k` (None:
        no such entry yet). The sources map only grows (model.rs), so a track's entry stays."""
        T = self.tracks.get(tid)
        if T is None or k is None:
            return None
        rows = T["src"].get(key)
        if not rows:
            return None
        return self._source_value(self._row(rows, k), k)

    def hits(self, tid: str, key: str, k: int | None) -> int:
        v = self.source(tid, key, k)
        return 0 if v is None else v[0]

    def sources(self, tid: str, k: int | None) -> dict[str, tuple[int, float]]:
        T = self.tracks.get(tid)
        if T is None or not self.present(tid, k):
            return {}
        out = {}
        for key in T["src"]:
            v = self.source(tid, key, k)
            if v is not None:
                out[key] = v
        return out

    def hits_rose(self, tid: str, key: str, t1: float, t2: float) -> bool:
        """Did the hits of `key` on track `tid` rise between the samples at `t1` and `t2`?"""
        k2 = self.index(t2)
        if k2 is None:
            return False
        return self.hits(tid, key, k2) > self.hits(tid, key, self.index(t1))

    def track(self, tid: str, k: int | None) -> dict[str, Any] | None:
        """Everything the picture showed of track `tid` at sample `k`."""
        if not self.present(tid, k):
            return None
        out: dict[str, Any] = {"id": tid, "domain": self.tracks[tid].get("domain"), "identity": self.identity(tid, k)}
        aff = self.affiliation(tid, k)
        out["affiliation"], out["affiliation_confidence"] = aff if aff else (None, None)
        out.update(self.kinematics(tid, k) or {})
        out["sources"] = self.sources(tid, k)
        return out

    def predicted(self, tid: str, t: float) -> tuple[float, float] | None:
        """The track's position as shown at `t`, carried along its shown velocity from its
        `last_seen_s` to `t` (`position + velocity * (t - last_seen_t)`)."""
        kin = self.kinematics(tid, self.index(t))
        if kin is None or kin["position"][0] is None:
            return None
        return carry(kin["position"], kin["velocity"], t - float(kin["last_seen_s"] or t))

    # ---- association ---------------------------------------------------------------------

    def _build_assoc(self) -> None:
        n = len(self.times)
        changes: dict[int, list[tuple[str, str | None]]] = {}
        for tid, rows in (self.enc.get("assoc") or {}).items():
            for r in rows:
                changes.setdefault(r[0], []).append((tid, r[1]))
        at: list[dict[str, str]] = []
        rev: list[dict[str, str]] = []
        cur: dict[str, str] = {}
        cur_rev: dict[str, str] = {}
        for k in range(n):
            if k in changes:
                cur = dict(cur)
                for tid, c in sorted(changes[k], key=lambda x: x[0]):
                    if c is None:
                        cur.pop(tid, None)
                    else:
                        cur[tid] = c
                cur_rev = {}
                for tid in sorted(cur):
                    cur_rev[cur[tid]] = tid
            at.append(cur)
            rev.append(cur_rev)
        self._assoc_at, self._rev_at = at, rev

    def contact_of(self, tid: str, k: int | None) -> str | None:
        if k is None:
            return None
        if self._assoc_at is None:
            self._build_assoc()
        assert self._assoc_at is not None
        return self._assoc_at[k].get(tid) if 0 <= k < len(self._assoc_at) else None

    def track_of(self, contact: str, k: int | None) -> str | None:
        """The track the tracker associated with `contact` at sample `k`."""
        if k is None:
            return None
        if self._rev_at is None:
            self._build_assoc()
        assert self._rev_at is not None
        return self._rev_at[k].get(contact) if 0 <= k < len(self._rev_at) else None

    def contact_hits_rose(self, contact: str, uid: int | str, sensors: tuple[str, ...], t1: float, t2: float) -> bool:
        """Did any of `uid`'s `sensors` raise its hits on the track associated with `contact`
        between `t1` and `t2` (sample by sample, on the track associated at each sample, so a
        contact tracked again under a new id still counts)?"""
        k2 = self.index(t2)
        if k2 is None or t2 < t1:
            return False
        k1 = self.index(t1)
        start = 0 if k1 is None else k1 + 1
        keys = [f"{uid}:{s}" for s in sensors]
        for k in range(start, k2 + 1):
            tid = self.track_of(contact, k)
            if tid is None:
                continue
            for key in keys:
                if self.hits(tid, key, k) > self.hits(tid, key, k - 1 if k > 0 else None):
                    return True
        return False

    def contacts_with_rise(self, uid: int | str, sensors: tuple[str, ...], t1: float, t2: float) -> list[str]:
        """Every contact whose associated track's hits from `uid`'s `sensors` rose between `t1`
        and `t2`."""
        k2 = self.index(t2)
        if k2 is None or t2 < t1:
            return []
        k1 = self.index(t1)
        start = 0 if k1 is None else k1 + 1
        keys = [f"{uid}:{s}" for s in sensors]
        out: set[str] = set()
        for tid, T in self.tracks.items():
            if not any(key in T["src"] for key in keys):
                continue
            for k in range(start, k2 + 1):
                c = self.contact_of(tid, k)
                if c is None or c in out:
                    continue
                if any(self.hits(tid, key, k) > self.hits(tid, key, k - 1 if k > 0 else None) for key in keys):
                    out.add(c)
        return sorted(out)

    # ---- our vehicles, alerts --------------------------------------------------------------

    def vehicle(self, uid: int | str, k: int | None) -> dict[str, Any] | None:
        r = self._row((self.enc.get("ours") or {}).get(str(uid)) or [], k)
        if r is None:
            return None
        h = self._row((self.enc.get("homes") or {}).get(str(uid)) or [], k)
        return {"lon": r[1], "lat": r[2], "alt_m": r[3], "speed_mps": r[4], "mode": r[5], "armed": r[6],
                "battery_pct": r[7], "link_blocked": r[8], "home": (h[1], h[2]) if h else None}

    def vehicle_series(self, uid: int | str) -> list[tuple[float, dict[str, Any]]]:
        """Every sample's truth of `uid`, in order (the state carried between change points)."""
        out = []
        for k in range(len(self.times)):
            v = self.vehicle(uid, k)
            if v is not None:
                out.append((self.times[k], v))
        return out

    def uids(self) -> list[str]:
        return sorted((self.enc.get("ours") or {}), key=lambda u: int(u) if str(u).isdigit() else str(u))

    def alerts_at(self, k: int | None) -> list[dict[str, Any]]:
        if k is None:
            return []
        out = []
        for key, a in sorted((self.enc.get("alerts") or {}).items()):
            if any(x <= k <= y for x, y in a["span"]):
                out.append({"id": key.split("@", 1)[0], **{n: a.get(n) for n in ("kind", "uid", "intent", "since_s")}})
        return out

    @property
    def fleet(self) -> list[dict[str, Any]]:
        return list(self.enc.get("fleet") or [])

    @property
    def sensing(self) -> dict[str, Any]:
        return dict(self.enc.get("sensing") or {})


def carry(pos: tuple[float, float], vel: tuple[float, float] | None, dt: float) -> tuple[float, float]:
    """A position (lon, lat) carried along a velocity (east, north m/s) for `dt` seconds."""
    lon, lat = float(pos[0]), float(pos[1])
    if vel is None or dt == 0:
        return (lon, lat)
    import math

    return (lon + vel[0] * dt / (111_320.0 * math.cos(math.radians(lat))), lat + vel[1] * dt / 111_320.0)


# ---- the audit log's digest ---------------------------------------------------------------

def digest_audit(text: str, epoch: float) -> dict[str, Any]:
    """What scoring v2 reads of the audit log, agent-agnostic: every intent with its source,
    task, vehicles, parameters, the policy's tier, each state it reached and when, each
    vehicle's op states (`op.state`), the approvals and the operator's views. Times are
    situation seconds."""
    intents: dict[int, dict[str, Any]] = {}

    def get(i: Any) -> dict[str, Any] | None:
        try:
            return intents.get(int(i))
        except (TypeError, ValueError):
            return None

    for line in text.splitlines():
        if not line.strip():
            continue
        try:
            e = json.loads(line)
        except ValueError:
            continue
        typ = e.get("type")
        t = sit_s(e["t"] / 1000.0, epoch) if isinstance(e.get("t"), (int, float)) else None
        if typ == "intent.created":
            i = e.get("intent") or {}
            src = i.get("source") or {}
            intents[int(i["id"])] = {
                "id": int(i["id"]), "source": {"kind": src.get("kind"), "id": src.get("id")},
                "kind": i.get("kind"), "uids": [int(u) if str(u).isdigit() else u for u in i.get("uids") or []],
                "params": i.get("params") or {}, "created": t, "tier": None, "rule": None,
                "states": [], "op": {}, "approvals": [], "views": [], "dispatched": None, "expires": None,
            }
            continue
        rec = get(e.get("intent"))
        if rec is None:
            continue
        if typ == "intent.state":
            rec["states"].append([t, e.get("to"), e.get("transition")])
        elif typ == "policy.decision":
            d = e.get("decision") or {}
            if isinstance(d, dict):
                rec["tier"], rec["rule"] = d.get("tier"), d.get("rule")
        elif typ == "intent.dispatched":
            if rec["dispatched"] is None:
                rec["dispatched"] = t
        elif typ == "op.state":
            rec["op"].setdefault(str(e.get("uid")), []).append([t, e.get("state"), str(e.get("detail") or "")[:160]])
        elif typ == "approval.requested":
            rec["expires"] = sit_s(e["expires_t"] / 1000.0, epoch) if isinstance(e.get("expires_t"), (int, float)) else None
        elif typ in ("approval.granted", "approval.rejected", "approval.denied", "approval.concurred"):
            rec["approvals"].append([t, typ.split(".", 1)[1], e.get("by")])
        elif typ == "operator.view":
            v = e.get("view") or {}
            rec["views"].append([t, v.get("event") if isinstance(v, dict) else None, e.get("operator")])
    return {"intents": [intents[i] for i in sorted(intents)]}


def invariant_counts(text: str) -> dict[str, Any]:
    """The floor's invariants on the audit log (eval/metrics.py `invariants`, the storyline
    bench's: each count must be zero), for 3.7."""
    from eval.metrics import invariants, trails
    from eval.situation import OPERATORS

    events = []
    for line in text.splitlines():
        try:
            events.append(json.loads(line))
        except ValueError:
            continue
    inv = invariants(events, trails(events), set(OPERATORS), None)
    return {"counts": inv["counts"], "total": int(sum(inv["counts"].values()))}


# ---- loading ------------------------------------------------------------------------------

def load(path: Path | str) -> dict[str, Any]:
    """A record from `<name>.json` or `<name>.json.gz`."""
    p = Path(path)
    raw = p.read_bytes()
    if p.suffix == ".gz" or raw[:2] == b"\x1f\x8b":
        raw = gzip.decompress(raw)
    return json.loads(raw)


def dump(rec: dict[str, Any], path: Path | str) -> int:
    """Write a record (gzipped when the name ends in .gz); returns the bytes written."""
    p = Path(path)
    data = json.dumps(rec, sort_keys=True).encode()
    if p.suffix == ".gz":
        data = gzip.compress(data, compresslevel=6, mtime=0)
    p.write_bytes(data)
    return len(data)


def picture(rec: dict[str, Any]) -> Picture:
    v2 = rec.get("v2") or {}
    if not v2.get("picture"):
        raise ValueError("this record has no picture samples (a v1 record, or not run with scoring v2)")
    return Picture(v2["picture"])
