"""Scoring v2's anchors and perturbations (eval/SCORING-V2.md section 5) as agents of the headless
episode (`Options.agent`, `batch.py --agent`). MODEL RESULTS ONLY: free, deterministic agents on
generated situations.

Every one of them reaches the bridge only through a session (sim.py `HeadlessSession`: the MCP
tools, as the playbook and the model watch officer do), wakes on the episode's loop from the AI's
first tick (`ai_start_s` plus the replicate's first tick, as every agent), and writes what it did
to the record's `agent_log`. The oracle and two perturbations also read truth, through the
simulator's own reads that never move the clock (`Sim.truth`) and the situation's contacts file:
**privileged**, counted and recorded (`agent_detail.privileged` in the record), never given to an
agent under test.

**Anchors** (agent id `agent-watch`, the AI's place on the roster; `ANCHORS`):

- `null`: connects and reads as the playbook does (the catalog and the policy once; every 2 s the
  fleet, the tracks, the inbox, its own intents; the change feed every 0.5 s), never acts. Its
  episode must be `none`'s, bar the bridge's `agent.action` events for its session (4.5).
- `random-valid`: every 30 s of situation time from its first tick, one action drawn uniformly
  (from `random.Random("random-valid:<replicate seed>")`) among the actions whose
  `preview_intent` passes. The actions are an orbit of each **idle** vehicle over each shown
  track: idle as the C2's own design has it (docs/DECISIONS.md, the playbook's fix rounds: "free
  means idle (no order, or a failed one)"): no order, or its order failed, was superseded,
  requeued or cancelled; not returning on its own (a failsafe's RTL); its link up. The orbit is
  centred on the track's shown position carried to now (`predicted`, else `position`) at the
  vehicle's look radius (the playbook's: max(1.1 x its minimum turn, or 66 m without one, and a
  quarter of its imaging range), up to the next 100 m). No idle vehicle or no passing preview:
  nothing that time. On the v2 base only PUMA 1 starts idle (the others fly opening orders), so
  it acts rarely: it is the floor's random neighbour, not a busy random agent.
- `oracle` (privileged): knows every contact's route, speed and start (the contacts file: so
  each need's future crossing of the 3 km circle and its deadline), which needs no look can
  resolve (the situation's stated misclassification draws), every vehicle's true state, the
  tracker's association, and where a Puma keeps its datalink (the link emulator's own terrain
  check over the baked grid, `LinkMap`). Every 1 s it plans, for each open need and each EO/IR
  Puma: orbit centres (300 m, a radius the Puma flies) on the contact's predicted path and 400,
  800 and 1200 m around it, kept where the orbit and the straight transit to it keep the link;
  for each, when the EO/IR dwell (the sensing model's detection probability at the Puma's mean
  range, and the tracker's at-most-2-s a detection) would reach 45 s (0.6) and 15 s more, given
  the Puma's transit from where it truly is at its cruise speed. The needs are shared between the
  Pumas (every assignment tried, each Puma's needs one after the other): most needs done by their
  deadline, then the most dwell on the others, then the soonest, then the fewest new orders. A
  Puma gets a new order only when that changes (a need resolved, a Puma lost to a link cut, a stop,
  its battery or the checks, its order ended), 15 s apart at least, re-tasking any Puma whatever
  it flies (PUMA 2's station included), and keeps its order while that still gets the dwell done
  in time. An orbit is previewed first; a centre the checks refuse gives way to the next best,
  and a Puma none of whose centres pass is left out for 10 s (for good when only its reserves
  failed and its true battery is low). A Puma whose true battery will reach the reserve (20 %)
  within a minute is sent home (rtl) and not tasked again; one whose truth shows it stopped looks
  from where it is; one whose link is blocked or lost waits. It dispatches as soon as it can
  (from its first tick), not just in time: an early look is never worse for it. It knows nothing
  of the situation's scripted faults before they happen (a stop or a link cut on the first look):
  it answers them as they come, as an agent would, so a fault family keeps its test. It is the
  ceiling: as strong as truth and these orders allow.

**Perturbations** (agent id `agent-playbook`; `PERTURBATIONS`): the playbook (episode.py
`PlaybookAgent`, its engine and config unchanged but where stated), with one thing changed:

- `playbook+spam` (follow-up spam): every look the playbook holds on a track (a card waiting or
  an orbit running, `isr.holding`) is proposed again, the same request and rationale, every 20 s
  from the last proposal on that track; each goes into the engine's own ledger as its own card
  would (so its later reads treat the copy as its look).
- `playbook+behind` (behind the target): each look's orbit centre is moved 1.5 times the Puma's
  EO/IR range behind the contact along its shown course (along the line from the nearest
  protected point through it when its course is not known or it is slower than 0.5 m/s), before
  the preview.
- `playbook+skip-first` (privileged): the situation's earliest need (the contact of unknown,
  suspect or hostile truth affiliation first to come within 3 km, from the contacts file) is
  taken out of every track list the playbook reads (by the tracker's association, read from
  truth at each read), so it never looks at it, nor holds a vehicle for it.
- `playbook+reserve-blind`: no rtl ever leaves (an rtl the plays select is dropped before its
  preview).
- `playbook+ferries` (look at ferries): a play of its own (`perturb.look_at_ferries`, enabled in
  this agent's config only) adds a look at every track shown neutral with an AIS identity, once
  per track, by a free sensor aircraft as `isr.investigate` chooses one, ranked below every real
  look (score 0.2); the look is let go (a hold, as `isr.release` does, so the vehicle is free
  again) once the track's classification is settled by a classifying sensor, the track is
  dropped, or it has run 90 s.
"""

from __future__ import annotations

import bisect
import json
import math
import random
import tomllib
from itertools import product
from pathlib import Path
from typing import Any

import numpy as np

from eval.agent import _mcp_args
from eval.situation import playbook_json

from .episode import PLAYBOOK_AGENT, POLL_S, PlaybookAgent
from .sim import HeadlessSession
from .situations import GATE, fleet_plan

import playbook.rationale as R  # noqa: E402  (agents/ is on sys.path: episode.py)
from playbook.beliefs import CLASSIFYING, RECOVERING_MODES, haversine_m, orbit_radius  # noqa: E402
from playbook.config import make as make_config  # noqa: E402
from playbook.plays import P2, PLAYS, Candidate, Play, Result, register  # noqa: E402
from playbook.plays.isr import (CHOICES, LOOK_PLAYS, RANK_STEP, alternatives, holding, live, ranked,  # noqa: E402
                                runs, sensor_capable, track_facts, vehicle_facts, vehicle_ref)

ANCHOR_AGENT = "agent-watch"
ANCHORS = ("null", "random-valid", "oracle")
PERTURBATIONS = ("playbook+spam", "playbook+behind", "playbook+skip-first", "playbook+reserve-blind",
                 "playbook+ferries")
NAMES = ANCHORS + PERTURBATIONS
#: SCORING-V2.md 2.1: who needs a look, where, by when
NEED_AFFILIATIONS = ("unknown", "suspect", "hostile")
NEED_RADIUS_M = 3000.0
DEADLINE_S = 60.0
#: murmur-contacts motion.rs: a contact speeds up from its spawn at this rate
CONTACT_ACCEL_MPS2 = 0.5
#: the task layer's states after which a vehicle's order no longer runs (its op on the vehicle)
ENDED = ("failed", "superseded", "requeued", "cancelled")
M_PER_DEG = 111_320.0


def _xy(p: tuple[float, float]) -> tuple[float, float]:
    """(east, north) metres from the bridge reference point."""
    return ((p[0] - GATE[0]) * M_PER_DEG * math.cos(math.radians(GATE[1])), (p[1] - GATE[1]) * M_PER_DEG)


def _ll(x: float, y: float) -> list[float]:
    return [round(GATE[0] + x / (M_PER_DEG * math.cos(math.radians(GATE[1]))), 6), round(GATE[1] + y / M_PER_DEG, 6)]


def offset(p: tuple[float, float], bearing_deg: float, dist_m: float) -> list[float]:
    b = math.radians(bearing_deg)
    return [round(p[0] + dist_m * math.sin(b) / (M_PER_DEG * math.cos(math.radians(p[1]))), 6),
            round(p[1] + dist_m * math.cos(b) / M_PER_DEG, 6)]


def bearing(a: tuple[float, float], b: tuple[float, float]) -> float:
    dx = (b[0] - a[0]) * M_PER_DEG * math.cos(math.radians(a[1]))
    dy = (b[1] - a[1]) * M_PER_DEG
    return math.degrees(math.atan2(dx, dy)) % 360.0


def look_radius(platform: dict[str, Any]) -> float:
    """The playbook's look radius for a platform (beliefs.orbit_radius with the shipped defaults):
    max(1.1 x the minimum turn, 66 m without one, a quarter of the imaging range), up to 100 m."""
    turn = 1.1 * float(platform.get("min_turn_radius_m") or 60.0)
    imaging = [float(s.get("range_m") or 0.0) for s in platform.get("sensors") or [] if s.get("type") in ("eo_ir", "sonar")]
    look = 0.25 * (max(imaging) if imaging else 0.0)
    return float(math.ceil(max(turn, look) / 100.0) * 100.0)


# ---- truth: the contacts' routes (privileged) --------------------------------------------------

class Route:
    """A transiting contact's path on the situation's clock, as murmur-contacts moves it: from its
    start it speeds up at 0.5 m/s^2 to its speed along its route and holds at the end (motion.rs;
    its turns, a few seconds, left out). `shift_m` re-anchors it on the contact's true position."""

    def __init__(self, contact: dict[str, Any], contacts_start_s: float):
        self.id = str(contact["id"])
        pts = [_xy((float(p[0]), float(p[1]))) for p in contact["route"]]
        self.xy = np.array(pts, dtype=float)
        seg = np.hypot(np.diff(self.xy[:, 0]), np.diff(self.xy[:, 1]))
        self.cum = np.concatenate([[0.0], np.cumsum(seg)])
        self.length = float(self.cum[-1])
        self.speed = float(contact.get("speed_mps") or 0.0)
        self.t0 = float(contacts_start_s) + float(contact.get("start_s") or 0.0)
        self.shift_m = 0.0

    def along(self, t: np.ndarray) -> np.ndarray:
        """Distance travelled along the route at situation times `t` (NaN before the start)."""
        T = np.asarray(t, dtype=float) - self.t0
        v, a = self.speed, CONTACT_ACCEL_MPS2
        ramp = v / a if v > 0 else 0.0
        d = np.where(T <= ramp, 0.5 * a * T ** 2, 0.5 * a * ramp ** 2 + v * (T - ramp))
        d = np.clip(d + self.shift_m, 0.0, self.length)
        return np.where(T < 0, np.nan, d)

    def at_along(self, d: np.ndarray) -> np.ndarray:
        d = np.asarray(d, dtype=float)
        x = np.interp(d, self.cum, self.xy[:, 0])
        y = np.interp(d, self.cum, self.xy[:, 1])
        out = np.stack([x, y], axis=-1)
        out[np.isnan(d)] = np.nan
        return out

    def at(self, t: np.ndarray) -> np.ndarray:
        """(east, north) metres at situation times `t` (NaN rows before the start)."""
        return self.at_along(self.along(t))

    def project(self, p: tuple[float, float]) -> float:
        """The distance along the route of the point nearest `p` (metres east, north)."""
        best, best_d = 0.0, float("inf")
        for i in range(len(self.xy) - 1):
            a, b = self.xy[i], self.xy[i + 1]
            ab = b - a
            L = float(ab @ ab)
            u = 0.0 if L == 0 else max(0.0, min(1.0, float((np.array(p) - a) @ ab) / L))
            q = a + u * ab
            dd = math.hypot(p[0] - q[0], p[1] - q[1])
            if dd < best_d:
                best, best_d = float(self.cum[i] + u * math.sqrt(L)), dd
        return best

    def reanchor(self, t: float, p: tuple[float, float]) -> None:
        """Shift the path so the contact is where truth has it at `t` (its turns and the contacts
        process's one-second steps leave the model a little off)."""
        self.shift_m = 0.0
        pred = float(self.along(np.array([t]))[0])
        if math.isnan(pred):
            return
        self.shift_m = self.project(p) - pred

    def first_within(self, radius_m: float, t_from: float, t_to: float, step: float = 1.0) -> float | None:
        ts = np.arange(t_from, t_to + step, step)
        xy = self.at(ts)
        r = np.hypot(xy[:, 0], xy[:, 1])
        hit = np.nonzero(r <= radius_m)[0]
        return float(ts[hit[0]]) if len(hit) else None


def read_contacts(path: Path | str | None) -> list[dict[str, Any]]:
    if not path:
        return []
    return list(tomllib.loads(Path(path).read_text()).get("contact") or [])


def need_routes(ep: Any, end_s: float) -> list[tuple[Route, float]]:
    """Every transiting contact of a need affiliation that comes within 3 km, with its predicted
    `t_need` (privileged: the contacts file), earliest first."""
    c0 = float(ep.opts.calibration.contacts_start_s)
    out = []
    for c in read_contacts(ep.contacts_path or ep.e.contacts):
        if str(c.get("affiliation") or "").lower() not in NEED_AFFILIATIONS or not c.get("route"):
            continue
        r = Route(c, c0)
        t_need = r.first_within(NEED_RADIUS_M, r.t0, end_s)
        if t_need is not None:
            out.append((r, t_need))
    return sorted(out, key=lambda x: (x[1], x[0].id))


# ---- the common part ------------------------------------------------------------------------------

class SessionAgent:
    """An agent with only a session (the MCP tools), on the episode's loop."""

    name = ""
    agent_id = ANCHOR_AGENT

    def __init__(self, ep: Any):
        self.ep = ep
        self.session = HeadlessSession(ep.sim, self.agent_id, f"scoring-v2-{self.name}/1")
        self.lines: list[dict[str, Any]] = []
        self.calls: dict[str, int] = {}
        self.proposals = 0
        self.privileged: dict[str, Any] = {}

    def write(self, kind: str, **fields: Any) -> dict[str, Any]:
        rec = {"t": round(self.ep.sim.t, 3), "source": self.name, "kind": kind}
        for k, v in fields.items():
            rec[f"field_{k}" if k in rec else k] = v
        self.lines.append(rec)
        return rec

    def call(self, tool: str, **args: Any) -> dict[str, Any]:
        self.calls[tool] = self.calls.get(tool, 0) + 1
        return self.ep.run_async(self.session.call(tool, **args))

    def propose(self, rationale: str, req: dict[str, Any], **why: Any) -> dict[str, Any] | None:
        """One proposal; a refusal without an intent is written as the playbook writes one (an
        `error` line naming propose_intent), so the labels count it as refused (labels.py)."""
        self.calls["propose_intent"] = self.calls.get("propose_intent", 0) + 1
        self.proposals += 1
        try:
            out = self.ep.run_async(self.session.propose(rationale, **_mcp_args(req)))
        except Exception as e:  # noqa: BLE001
            self.write("error", error=f"propose_intent: {type(e).__name__}: {e}", request=req, **why)
            return None
        self.write("proposal", intent=out.get("id"), state=out.get("state"), refused=out.get("refused"), request=req, **why)
        return out

    def describe(self) -> dict[str, Any]:
        out: dict[str, Any] = {"agent": self.name, "id": self.agent_id, "calls": dict(sorted(self.calls.items())),
                               "proposals": self.proposals}
        if self.privileged:
            out["privileged"] = self.privileged
        return out


# ---- null ---------------------------------------------------------------------------------------

class NullAgent(SessionAgent):
    """Connects, reads as the playbook does, never acts (module doc)."""

    name = "null"
    TICK_S = 2.0

    def __init__(self, ep: Any):
        super().__init__(ep)
        self.cursor: Any = None
        self.next_tick: float | None = None
        self.errors = 0

    def wake(self, now: float) -> float | None:
        try:
            if self.next_tick is None:
                self.call("get_platforms")
                self.call("get_policy")
                r = self.call("get_changes")
                self.cursor = r.get("cursor")
                self.write("connected", run=r.get("run"))
                self.next_tick = now
            if now >= self.next_tick - 1e-9:
                self.call("get_fleet", compact=True)
                self.call("get_tracks")
                self.call("get_inbox")
                self.call("get_intents", mine=True, limit=50)
                self.next_tick = now + self.TICK_S
            r = self.call("get_changes", since=self.cursor, limit=500)
            self.cursor = r.get("cursor", self.cursor)
        except Exception as e:  # noqa: BLE001 (a read that failed: read again next time; never an act)
            self.errors += 1
            self.write("read.error", error=f"{type(e).__name__}: {str(e)[:200]}")
        return now + POLL_S


# ---- random-valid -----------------------------------------------------------------------------------

def idle(v: dict[str, Any]) -> bool:
    """A vehicle with no order running (module doc): none, or one that ended; not returning on its
    own; its link up."""
    if v.get("link_lost") or str(v.get("mode") or "").upper() in RECOVERING_MODES:
        return False
    task = v.get("task") if isinstance(v.get("task"), dict) else None
    return task is None or str(task.get("state")) in ENDED


class RandomValid(SessionAgent):
    """Every 30 s, one action uniform over those whose preview passes (module doc)."""

    name = "random-valid"
    PERIOD_S = 30.0

    def __init__(self, ep: Any):
        super().__init__(ep)
        self.rng = random.Random(f"random-valid:{int(ep.seed)}")
        self.catalog: dict[str, dict[str, Any]] | None = None
        self.draws: list[dict[str, Any]] = []

    def actions(self) -> list[dict[str, Any]]:
        fleet = self.call("get_fleet", compact=True)
        tracks = self.call("get_tracks")
        out = []
        for v in sorted(fleet.get("vehicles") or [], key=lambda v: int(v["uid"])):
            p = (self.catalog or {}).get(str(v.get("platform"))) or {}
            if not idle(v) or "orbit" not in (p.get("tasks") or []):
                continue
            radius = look_radius(p)
            for tr in sorted(tracks.get("tracks") or [], key=lambda t: str(t["id"])):
                pos = tr.get("predicted") or tr.get("position")
                if not isinstance(pos, list) or len(pos) != 2:
                    continue
                out.append({"task": "orbit", "uids": [int(v["uid"])], "center": [round(float(pos[0]), 6),
                            round(float(pos[1]), 6)], "radius_m": radius, "_track": str(tr["id"]),
                            "_callsign": v.get("callsign")})
        return out

    def wake(self, now: float) -> float | None:
        try:
            if self.catalog is None:
                self.catalog = {str(p["id"]): p for p in self.call("get_platforms").get("platforms") or []}
            acts = self.actions()
            valid = []
            for a in acts:
                req = {k: v for k, v in a.items() if not k.startswith("_")}
                try:
                    pv = self.call("preview_intent", **_mcp_args(req))
                except Exception:  # noqa: BLE001 (a preview the bridge refused: not a valid action)
                    continue
                if pv.get("passed"):
                    valid.append(a)
            if not valid:
                self.write("none.valid", actions=len(acts))
                return now + self.PERIOD_S
            k = self.rng.randrange(len(valid))
            a = valid[k]
            req = {kk: v for kk, v in a.items() if not kk.startswith("_")}
            self.draws.append({"t": round(now, 3), "of": len(valid), "actions": len(acts), "drawn": k})
            text = (f"Random valid action: orbit {a['_callsign']} over {a['_track']} ({req['radius_m']:.0f} m), drawn "
                    f"uniformly among the {len(valid)} actions whose preview passed.")
            self.propose(text, req, track=a["_track"], valid=len(valid), actions=len(acts))
        except Exception as e:  # noqa: BLE001 (the next turn reads again)
            self.write("read.error", error=f"{type(e).__name__}: {str(e)[:200]}")
        return now + self.PERIOD_S

    def describe(self) -> dict[str, Any]:
        return {**super().describe(), "seed": f"random-valid:{int(self.ep.seed)}", "draws": self.draws}


# ---- oracle -------------------------------------------------------------------------------------------

class LinkMap:
    """Where a small UAS keeps its line-of-sight link to the ground station: the link emulator's
    own terrain check (crates/murmur-linkemu terrain.rs `path_obstruction_m` over the baked grid,
    sim/headless/terrain: the ground station and its mast from the links file, the vehicle's
    altitude, the Earth bulge at k = 4/3, one answer per 250 m cell at its centre, as the
    emulator caches it), computed once per process for the operating area. Truth about the world
    (the oracle's), never given to an agent under test."""

    CELL_M = 250.0
    SPACING_M = 50.0

    def __init__(self, alt_m: float):
        from .families import water
        from .situations import links_file

        links = tomllib.loads(links_file().read_text())
        gs = links.get("ground_station") or {}
        los = (links.get("class") or {}).get("los") or {}
        self.gs = (float(gs.get("lon", -122.446)), float(gs.get("lat", 37.8068)))
        self.h_a = float(gs.get("alt_m", 5.0) if not isinstance(gs.get("alt_m"), str) else 5.0) \
            + float(los.get("gs_antenna_m", 0.0))
        self.h_b = float(alt_m) + float(los.get("antenna_m", 0.0))
        self.k = float(los.get("k_factor", 4.0 / 3.0))
        w = water()
        self.hdr = w.h
        raw = np.array(w.v, dtype=float).reshape(w.h["nlat"], w.h["nlon"])
        self.nodata = raw == w.h["nodata"]
        self.surface = np.where(self.nodata, 0.0, raw * w.h["scale"])
        self.dlat = self.CELL_M / M_PER_DEG
        self.dlon = self.dlat / max(math.cos(math.radians(self.gs[1])), 0.05)
        # the cells of the operating area and a margin
        self.ci0, self.ci1 = int(math.floor(37.74 / self.dlat)), int(math.floor(37.90 / self.dlat))
        self.cj0, self.cj1 = int(math.floor(-122.57 / self.dlon)), int(math.floor(-122.35 / self.dlon))
        ci = np.arange(self.ci0, self.ci1 + 1)
        cj = np.arange(self.cj0, self.cj1 + 1)
        lat = (ci + 0.5) * self.dlat
        lon = (cj + 0.5) * self.dlon
        LAT, LON = np.meshgrid(lat, lon, indexing="ij")
        self.obs = self._obstruction(LON.ravel(), LAT.ravel()).reshape(LAT.shape)

    def _height(self, lon: np.ndarray, lat: np.ndarray) -> np.ndarray:
        """Bilinear surface height (the headless terrain's `Grid::height`); 0 outside it."""
        h = self.hdr
        y = (lat - h["lat0"]) / h["dlat"]
        x = (lon - h["lon0"]) / h["dlon"]
        i = np.floor(y).astype(int)
        j = np.floor(x).astype(int)
        ok = (i >= 0) & (j >= 0) & (i + 1 < h["nlat"]) & (j + 1 < h["nlon"])
        i, j = np.clip(i, 0, h["nlat"] - 2), np.clip(j, 0, h["nlon"] - 2)
        fy, fx = y - i, x - j
        s = self.surface
        v = (s[i, j] * (1 - fy) * (1 - fx) + s[i + 1, j] * fy * (1 - fx) + s[i, j + 1] * (1 - fy) * fx
             + s[i + 1, j + 1] * fy * fx)
        bad = self.nodata[i, j] | self.nodata[i + 1, j] | self.nodata[i, j + 1] | self.nodata[i + 1, j + 1]
        return np.where(ok & ~bad, v, 0.0)

    def _obstruction(self, lon: np.ndarray, lat: np.ndarray) -> np.ndarray:
        out = np.empty(len(lon))
        R_E = 6_371_000.0
        for k in range(len(lon)):
            dx = (lon[k] - self.gs[0]) * M_PER_DEG * math.cos(math.radians((lat[k] + self.gs[1]) / 2))
            dy = (lat[k] - self.gs[1]) * M_PER_DEG
            total = math.hypot(dx, dy)
            n = int(min(max(math.ceil(total / self.SPACING_M), 2), 500))
            f = np.arange(1, n) / n
            s = self._height(self.gs[0] + (lon[k] - self.gs[0]) * f, self.gs[1] + (lat[k] - self.gs[1]) * f)
            d1 = f * total
            bulge = d1 * (total - d1) / (2.0 * self.k * R_E)
            ray = self.h_a + (self.h_b - self.h_a) * f
            out[k] = float(np.max(s + bulge - ray)) if len(s) else -1e4
        return out

    def at(self, xy: np.ndarray) -> np.ndarray:
        """The obstruction (metres; above 0 the link loses packets, 20 m loses them all) at
        points given as (east, north) metres from the bridge."""
        xy = np.asarray(xy, dtype=float).reshape(-1, 2)
        lon = GATE[0] + xy[:, 0] / (M_PER_DEG * math.cos(math.radians(GATE[1])))
        lat = GATE[1] + xy[:, 1] / M_PER_DEG
        ci = np.floor(lat / self.dlat).astype(int) - self.ci0
        cj = np.floor(lon / self.dlon).astype(int) - self.cj0
        ok = (ci >= 0) & (cj >= 0) & (ci < self.obs.shape[0]) & (cj < self.obs.shape[1])
        return np.where(ok, self.obs[np.clip(ci, 0, self.obs.shape[0] - 1), np.clip(cj, 0, self.obs.shape[1] - 1)],
                        1e4)


_LINKMAPS: dict[float, LinkMap] = {}


def link_map(alt_m: float) -> LinkMap:
    if alt_m not in _LINKMAPS:
        _LINKMAPS[alt_m] = LinkMap(alt_m)
    return _LINKMAPS[alt_m]


#: murmur-contacts scenario.rs `KindDefaults`: each kind's radar and EO signature (a contact's own
#: `signature` in its file overrides it)
SIGNATURE = {"cargo_ship": 1.6, "tanker": 1.6, "ferry": 1.2, "patrol_boat": 0.9, "fishing_vessel": 0.7, "sailboat": 0.5,
             "small_fast_boat": 0.35, "light_aircraft": 0.6, "helicopter": 0.7}


class Need:
    def __init__(self, route: Route, t_need: float, contact: dict[str, Any]):
        self.id = route.id
        self.route = route
        self.t_need = t_need
        self.deadline = t_need + DEADLINE_S
        self.kind = contact.get("kind")
        self.signature = float(contact.get("signature") or SIGNATURE.get(str(self.kind), 0.5))
        self.resolved_t: float | None = None
        self.conf = 0.0  # its track's identity confidence from a classifying sensor, its true kind


#: one planned centre: (when its dwell is done, inf if not by the deadline; the centre (east,
#: north m); the dwell by the deadline)
Centre = tuple[float, tuple[float, float], float]


class Puma:
    def __init__(self, uid: int, callsign: str, cruise: float, alt_m: float, eo_range_m: float):
        self.uid = uid
        self.callsign = callsign
        self.cruise = cruise
        self.alt_m = alt_m
        self.eo_range_m = eo_range_m
        self.xy: tuple[float, float] | None = None
        self.mode = ""
        self.link_blocked = False
        self.link_lost = False
        self.speed = 0.0
        self.slow = 0  # consecutive reads at under 2 m/s
        self.battery: list[tuple[float, float]] = []
        self.sent_rtl = False
        self.order: dict[str, Any] | None = None  # our orbit: intent, centre (xy), need, t
        self.last_order_t: float | None = None
        self.refused_until = 0.0  # none of its centres passed the checks: not planned for until then

    @property
    def stopped(self) -> bool:
        return self.slow >= 3

    def battery_now(self) -> float | None:
        return self.battery[-1][1] if self.battery else None

    def to_reserve_s(self, reserve: float) -> float:
        """Seconds until the true battery reaches `reserve`, at the drain of the last 30 s (a
        Puma at cruise: 1 % in 90 s, used until there are 20 s of reads)."""
        if not self.battery:
            return float("inf")
        t1, b1 = self.battery[-1]
        i = bisect.bisect_left([t for t, _ in self.battery], t1 - 30.0)
        t0, b0 = self.battery[i]
        rate = (b0 - b1) / (t1 - t0) if t1 - t0 >= 20.0 and b0 > b1 else 1.0 / 90.0
        return max(0.0, (b1 - reserve) / rate)


class Oracle(SessionAgent):
    """The ceiling (module doc). Privileged: truth and the contacts file."""

    name = "oracle"
    TICK_S = 1.0
    DWELL_S = 45.0  # EO/IR to 0.6: 0.95 x (1 - exp(-d/45)) >= 0.6 at d = 45 s (tracks/model.rs)
    DWELL_MARGIN_S = 15.0
    ORBIT_M = 300.0  # PUMA 2's station orbit: a radius the Puma flies
    TASK_ALT_M = 80.0  # a tasked Puma flies at its catalog altitude (typical_alt_m)
    DISPATCH_S = 6.0  # approve-all looks every 1 s; the upload over the Puma's datalink
    STEP_S = 2.0
    CENTRE_STEP_S = 4.0
    #: candidate centres: the contact's predicted positions and these offsets from them (m)
    OFFSETS = [(0.0, 0.0)] + [(r * math.sin(math.radians(a)), r * math.cos(math.radians(a)))
                              for r in (400.0, 800.0, 1200.0) for a in range(0, 360, 45)]
    # the link (sim/links.toml, class los): terrain "ramp", a packet lost with probability
    # obstruction / 20 m; the lost-link failsafe after 5 s without one. Every point of the orbit at
    # most 5 m obstructed (25 % loss), the transit's worst point 15 m and at most 6 of its 30
    # samples over 8 m: the link holds
    RING_CLEAR_M = 5.0
    TRANSIT_MAX_M = 15.0
    TRANSIT_SOME_M = 8.0
    TRANSIT_SOME_N = 6
    RESERVE_PCT = 20.0
    RTL_LEAD_S = 60.0
    REPLAN_GAP_S = 15.0  # at least this between two orders to one Puma (each is a card)
    REFUSED_S = 10.0  # a Puma none of whose centres passed the checks: left out of the plan so long
    LOW_BATTERY_S = 1800.0  # a refusal for its reserves holds for good when its battery truly has less

    def __init__(self, ep: Any):
        super().__init__(ep)
        self.end = float(ep.e.duration_s)
        self.needs: dict[str, Need] = {}
        self.pumas: dict[int, Puma] = {}
        self.plans: list[dict[str, Any]] = []
        self.errors = 0
        self.started = False
        self.privileged = {"truth_reads": 0, "contacts_file": None, "sensing_draws": None,
                           "note": "reads truth (the contacts' routes and states, our vehicles' true state, the "
                                   "tracker's association, the terrain between the ground station and a point): an "
                                   "anchor, never an agent under test"}

    # ---- setup -------------------------------------------------------------------------------------

    def setup(self) -> None:
        ep = self.ep
        path = ep.contacts_path or ep.e.contacts
        self.privileged["contacts_file"] = Path(path).name if path else None
        sensing = (ep.e.truth or {}).get("sensing") or {}
        wrong = sensing.get("misclassified") or {}
        self.privileged["sensing_draws"] = {k: list(v) for k, v in sorted(wrong.items())}
        _, standins = fleet_plan(ep.e.fleet)
        carried: set[str] = set()
        for s in standins:
            sensors = s["spec"].get("sensors") or []
            types = {x.get("type") for x in sensors}
            if "eo_ir" in types:
                carried.add("eo_ir")
                alt = float(s["spec"].get("typical_alt_m") or self.TASK_ALT_M)
                eo = max(float(x.get("range_m") or 0.0) for x in sensors if x.get("type") == "eo_ir")
                self.pumas[int(s["uid"])] = Puma(int(s["uid"]), s["callsign"], float(s["spec"].get("cruise_mps") or 20.0),
                                                 alt, eo)
            carried |= types & set(CLASSIFYING)
        meta = {str(c["id"]): c for c in read_contacts(path)}
        for route, t_need in need_routes(ep, self.end):
            c = meta.get(route.id) or {}
            domain = str(c.get("domain") or "")
            holders = [x for x in ("eo_ir", "sonar") if x in carried
                       and ((x == "eo_ir" and domain != "subsurface") or (x == "sonar" and domain in ("surface", "subsurface")))]
            if holders and all(x in (wrong.get(route.id) or []) for x in holders):
                self.write("need.unresolvable", contact=route.id, t_need=round(t_need, 1),
                           why="its classifying sensors' misclassification draws are set")
                continue
            self.needs[route.id] = Need(route, t_need, c)
            self.write("need", contact=route.id, t_need=round(t_need, 1), deadline=round(t_need + DEADLINE_S, 1))

    # ---- one wake ----------------------------------------------------------------------------------

    def read(self, now: float) -> None:
        truth = self.ep.sim.truth(association=True)
        self.privileged["truth_reads"] += 1
        for o in truth.get("ours") or []:
            p = self.pumas.get(int(o["uid"]))
            if p is None:
                continue
            p.xy = _xy((float(o["lon"]), float(o["lat"])))
            p.mode = str(o.get("mode") or "")
            p.link_blocked = bool(o.get("link_blocked"))
            p.speed = float(o.get("speed_mps") or 0.0)
            p.slow = p.slow + 1 if p.speed < 2.0 else 0
            if o.get("battery_capacity_mah"):
                p.battery.append((now, 100.0 * (1.0 - float(o["battery_consumed_mah"]) / float(o["battery_capacity_mah"]))))
                p.battery = p.battery[-60:]
        contacts = {str(c["id"]): c for c in ((truth.get("contacts") or {}).get("contacts") or [])}
        for n in self.needs.values():
            c = contacts.get(n.id)
            if c and c.get("exists"):
                n.route.reanchor(now, _xy((float(c["lon"]), float(c["lat"]))))
        assoc = {str(a["contact_id"]): str(a["track_id"]) for a in truth.get("association") or []}
        tracks = {str(t["id"]): t for t in self.call("get_tracks").get("tracks") or []}
        fleet = {int(v["uid"]): v for v in self.call("get_fleet", compact=True).get("vehicles") or []}
        for n in self.needs.values():
            tr = tracks.get(assoc.get(n.id, ""))
            ident = (tr or {}).get("identity") or {}
            n.conf = 0.0
            if tr and ident.get("kind") == n.kind and not ident.get("conflict") \
                    and set(ident.get("supported_by") or []) & set(CLASSIFYING):
                n.conf = float(ident.get("confidence") or 0.0)
            if n.resolved_t is None and n.conf >= 0.6:
                n.resolved_t = now
                self.write("need.resolved", contact=n.id, t=round(now, 1))
        for p in self.pumas.values():
            v = fleet.get(p.uid) or {}
            p.link_lost = bool(v.get("link_lost"))
            if p.order is None:
                continue
            task = v.get("task") if isinstance(v.get("task"), dict) else None
            mine = task is not None and task.get("intent_id") == p.order["intent"]
            young = now - p.order["t"] < 15.0
            if (mine and str(task.get("state")) in ENDED) or (not mine and not young) \
                    or p.mode.upper() in RECOVERING_MODES:
                self.write("order.ended", uid=p.uid, intent=p.order["intent"], mode=p.mode,
                           task=task, link_blocked=p.link_blocked, link_lost=p.link_lost)
                p.order = None

    def dwell_needed(self, n: Need) -> float:
        """The EO/IR dwell still to come for the track to show 0.6 (from its confidence now), and a
        margin."""
        c = min(n.conf, 0.94)
        done = -45.0 * math.log(1.0 - c / 0.95) if c > 0 else 0.0
        return max(0.0, self.DWELL_S - done) + self.DWELL_MARGIN_S

    def centres(self, n: Need, now: float, p: Puma, start_xy: tuple[float, float]) -> np.ndarray:
        """Candidate orbit centres for `n`: its predicted positions from now to its deadline and
        points 400 and 800 m around each, kept where the whole orbit has the Puma's link and the
        straight transit to it from `start_xy` keeps it too."""
        c_ts = np.arange(max(now, n.route.t0), n.deadline + 1e-9, self.CENTRE_STEP_S)
        if len(c_ts) == 0:
            return np.zeros((0, 2))
        base = n.route.at(c_ts)
        base = base[~np.isnan(base[:, 0])]
        if len(base) == 0:
            return np.zeros((0, 2))
        cands = (base[:, None, :] + np.array(self.OFFSETS)[None, :, :]).reshape(-1, 2)
        cands = np.unique(np.round(cands / 50.0) * 50.0, axis=0)
        lm = link_map(p.alt_m)
        ring = np.array([(self.ORBIT_M * math.sin(math.radians(a)), self.ORBIT_M * math.cos(math.radians(a)))
                         for a in range(0, 360, 45)] + [(0.0, 0.0)])
        obs = lm.at((cands[:, None, :] + ring[None, :, :]).reshape(-1, 2)).reshape(len(cands), len(ring))
        cands = cands[(obs <= self.RING_CLEAR_M).all(axis=1)]
        if len(cands) == 0:
            return cands
        f = np.linspace(0.0, 1.0, 30)[None, :, None]
        line = np.array(start_xy)[None, None, :] + (cands[:, None, :] - np.array(start_xy)[None, None, :]) * f
        tob = lm.at(line.reshape(-1, 2)).reshape(len(cands), -1)
        ok = (tob.max(axis=1) <= self.TRANSIT_MAX_M) & ((tob > self.TRANSIT_SOME_M).sum(axis=1) <= self.TRANSIT_SOME_N)
        return cands[ok]

    def best(self, p: Puma, n: Need, start_t: float, start_xy: tuple[float, float], now: float,
             keep: tuple[float, float] | None = None) -> tuple[list[Centre], Centre | None]:
        """For Puma `p` free at `start_t` at `start_xy`: the candidate centres (`centres`), each with
        the time its dwell is done (inf if not by the deadline) and the dwell by the deadline,
        best first; and the same for `keep`, its current centre, flown without a new order (no
        dispatch delay). A stopped Puma: its own position, whatever it was told."""
        need_s = self.dwell_needed(n)
        t_lo = max(now, start_t)
        if t_lo >= n.deadline:
            return [], None
        ts = np.arange(t_lo, n.deadline + 1e-9, self.STEP_S)
        boat = n.route.at(ts)
        if p.stopped:
            cands = np.array([start_xy], dtype=float)
            arrive = np.array([t_lo])
            keep = None
        else:
            cands = self.centres(n, now, p, start_xy)
            d = np.hypot(cands[:, 0] - start_xy[0], cands[:, 1] - start_xy[1])
            arrive = start_t + self.DISPATCH_S + (d + self.ORBIT_M) / max(p.cruise, 1.0)
            if keep is not None:
                cands = np.vstack([cands, np.array([keep], dtype=float)])
                arrive = np.append(arrive, start_t + math.hypot(keep[0] - start_xy[0], keep[1] - start_xy[1])
                                   / max(p.cruise, 1.0))
        if len(cands) == 0:
            return [], None
        dist = np.hypot(boat[:, None, 0] - cands[None, :, 0], boat[:, None, 1] - cands[None, :, 1])
        # the sensing model (sensing.rs): pd = 0.95 / (1 + x^6) at x = range / (R x signature), none
        # past 1.5; the Puma circles the centre, so its range is about sqrt(d^2 + r^2). A detection
        # adds the time since the last one, at most 2 s (tracks/model.rs), so the dwell grows at
        # about pd (2 - pd) a second
        x = np.sqrt(np.nan_to_num(dist, nan=1e9) ** 2 + self.ORBIT_M ** 2) / (p.eo_range_m * n.signature)
        pd = np.where(x <= 1.5, 0.95 / (1.0 + x ** 6), 0.0)
        rate = pd * (2.0 - pd) * (ts[:, None] >= arrive[None, :])
        cum = np.cumsum(rate, axis=0) * self.STEP_S
        done = cum >= need_s
        first = np.where(done.any(axis=0), np.argmax(done, axis=0), -1)
        tdone = np.where(first >= 0, ts[np.clip(first, 0, None)], np.inf)
        total = cum[-1]
        rows = [(float(tdone[i]), (float(cands[i, 0]), float(cands[i, 1])), float(total[i])) for i in range(len(cands))]
        kept = rows.pop() if keep is not None else None
        rows.sort(key=lambda r: (r[0], -r[2], r[1]))
        return rows[:12], kept

    def first_choice(self, ranked_c: list[Centre], kept: Centre | None, n: Need) -> tuple[Centre | None, bool]:
        """What a Puma flies for need `n`: its current centre while that gets the dwell done by the
        deadline (no new card), else the best new centre; and whether that is a new order."""
        if kept is not None and kept[0] <= n.deadline - 5.0:
            return kept, False
        if ranked_c and (kept is None or ranked_c[0][0] < kept[0] - 5.0):
            return ranked_c[0], True
        if kept is not None:
            return kept, False
        return (ranked_c[0], True) if ranked_c else (None, False)

    def usable(self, p: Puma, now: float) -> bool:
        return (p.xy is not None and not p.sent_rtl and not p.link_blocked and not p.link_lost
                and now >= p.refused_until)

    def plan(self, now: float) -> dict[int, dict[str, Any]]:
        """Needs to Pumas: every assignment of the open needs (each to one Puma or to none, each
        Puma's taken in deadline order, one after the other) is tried; the most needs done by
        their deadline wins, then the least time to get them done, then the fewest new orders.
        Returns each tasked Puma's first need: the centre it flies, whether that is a new order,
        and the ranked new centres (the next ones if a preview refuses the first)."""
        open_needs = [n for n in sorted(self.needs.values(), key=lambda n: (n.deadline, n.id))
                      if n.resolved_t is None and n.deadline > now + 2.0]
        pumas = [p for p in sorted(self.pumas.values(), key=lambda p: p.uid) if self.usable(p, now)]
        if not open_needs or not pumas:
            return {}
        cache: dict[tuple[int, str, float, tuple[float, float], bool], tuple[list[Centre], Centre | None]] = {}
        best_key: tuple | None = None
        best_out: dict[int, dict[str, Any]] = {}
        for choice in product(range(len(pumas) + 1), repeat=len(open_needs)):
            served, partial, spent, new = 0, 0.0, 0.0, 0
            out: dict[int, dict[str, Any]] = {}
            ok = True
            for i, p in enumerate(pumas):
                mine = [n for n, c in zip(open_needs, choice) if c == i + 1]
                if not mine:
                    continue
                if p.to_reserve_s(self.RESERVE_PCT) < (max(n.deadline for n in mine) - now) + self.RTL_LEAD_S:
                    ok = False
                    break
                t, xy = now, p.xy
                assert xy is not None
                for k, n in enumerate(mine):
                    keep = p.order["xy"] if k == 0 and p.order is not None and p.order.get("need") == n.id else None
                    ck = (p.uid, n.id, round(t, 3), (round(xy[0], 1), round(xy[1], 1)), keep is not None)
                    if ck not in cache:
                        cache[ck] = self.best(p, n, t, xy, now, keep)
                    ranked_c, kept = cache[ck]
                    pick, is_new = self.first_choice(ranked_c, kept, n)
                    if pick is None:
                        break
                    if k == 0:
                        out[p.uid] = {"need": n, "pick": pick, "new": is_new, "ranked": ranked_c}
                        new += int(is_new)
                    if not math.isfinite(pick[0]):
                        # not done by its deadline on the model: the most dwell by then (the
                        # sensing may still be kinder than the model's mean)
                        partial += min(1.0, pick[2] / max(self.dwell_needed(n), 1.0))
                        break
                    served += 1
                    spent += pick[0] - now
                    t, xy = pick[0], pick[1]
            if not ok:
                continue
            key = (-served, -round(partial, 3), round(spent, 3), new)
            if best_key is None or key < best_key:
                best_key, best_out = key, out
        return best_out

    def order(self, p: Puma, n: Need, ranked_c: list[Centre], now: float) -> bool:
        """The best centre whose preview passes, proposed. If none passes, the Puma is left out of
        the plans for a while (`REFUSED_S`), or for good when the checks refused only its reserves
        and its true battery is low, and the next plan does without it."""
        refusals: set[str] = set()
        for tdone, cxy, dwell in ranked_c[:6]:
            req = {"task": "orbit", "uids": [p.uid], "center": _ll(*cxy), "radius_m": self.ORBIT_M}
            try:
                pv = self.call("preview_intent", **_mcp_args(req))
            except Exception as e:  # noqa: BLE001
                self.write("preview.refused", uid=p.uid, request=req, error=str(e)[:200])
                continue
            if not pv.get("passed"):
                bad = [x for x in pv.get("checks") or [] if x.get("outcome") != "pass"]
                refusals |= {str(x.get("rule")) for x in bad}
                self.write("preview.failed", uid=p.uid, request=req, checks=[f"{x.get('rule')} {x.get('outcome')}" for x in bad])
                continue
            text = (f"Orbit {p.callsign} at {req['center'][1]:.5f}, {req['center'][0]:.5f} ({self.ORBIT_M:.0f} m) "
                    f"where a contact to be identified will pass (a scoring anchor placed from truth).")
            out = self.propose(text, req, need=n.id, deadline=round(n.deadline, 1),
                               planned_done=None if not math.isfinite(tdone) else round(tdone, 1), dwell=round(dwell, 1))
            if out and out.get("id") is not None and not out.get("refused"):
                p.order = {"intent": int(out["id"]), "xy": cxy, "need": n.id, "t": now}
                p.last_order_t = now
                return True
        # its battery (truly low: the policy's assumed task would take it under the reserve) is for
        # good; anything else, such as its facts stale just after its link came back
        # (`readiness`), is tried again shortly
        low = refusals == {"reserves"} and p.to_reserve_s(self.RESERVE_PCT) < self.LOW_BATTERY_S
        p.refused_until = float("inf") if low else now + self.REFUSED_S
        self.write("no.order", uid=p.uid, need=n.id, refused=sorted(refusals),
                   until=None if math.isinf(p.refused_until) else round(p.refused_until, 1))
        return False

    def wake(self, now: float) -> float | None:
        try:
            if not self.started:
                self.setup()
                self.started = True
            self.read(now)
            for p in sorted(self.pumas.values(), key=lambda p: p.uid):
                if not p.sent_rtl and p.battery and p.to_reserve_s(self.RESERVE_PCT) <= self.RTL_LEAD_S:
                    out = self.propose(f"Return {p.callsign} home before its battery reaches the reserve.",
                                       {"task": "rtl", "uids": [p.uid]}, battery_pct=round(p.battery_now() or 0.0, 2))
                    p.sent_rtl = True
                    p.order = None
                    if out is None:
                        self.write("rtl.refused", uid=p.uid)
            plan = self.plan(now)
            for uid, x in sorted(plan.items()):
                p, n = self.pumas[uid], x["need"]
                if p.stopped or not x["new"]:
                    continue  # a stopped Puma looks from where it is; a kept order needs no card
                if p.last_order_t is not None and now - p.last_order_t < self.REPLAN_GAP_S:
                    continue
                tdone = x["pick"][0]
                self.plans.append({"t": round(now, 1), "uid": uid, "need": n.id, "done": None if not math.isfinite(tdone)
                                   else round(tdone, 1), "deadline": round(n.deadline, 1)})
                self.order(p, n, x["ranked"], now)
        except Exception as e:  # noqa: BLE001 (the next wake plans again)
            self.errors += 1
            self.write("error.oracle", error=f"{type(e).__name__}: {str(e)[:300]}")
        return now + self.TICK_S

    def describe(self) -> dict[str, Any]:
        return {**super().describe(), "needs": {k: {"t_need": round(n.t_need, 1), "deadline": round(n.deadline, 1),
                                                    "resolved_t": n.resolved_t} for k, n in sorted(self.needs.items())},
                "plans": self.plans, "errors": self.errors}


# ---- perturbations -----------------------------------------------------------------------------------

class SessionProxy:
    """The playbook's session with a hook on each read and proposal (the perturbations')."""

    def __init__(self, inner: HeadlessSession, owner: "Perturbed"):
        self._inner = inner
        self._owner = owner
        self.agent = inner.agent
        self.client_version = inner.client_version
        self.tools = inner.tools

    async def __aenter__(self) -> "SessionProxy":
        return self

    async def __aexit__(self, *exc: Any) -> None:
        return None

    async def call(self, name: str, **args: Any) -> dict[str, Any]:
        out = await self._inner.call(name, **args)
        return self._owner.on_read(name, args, out)

    async def propose(self, rationale: str, **req: Any) -> dict[str, Any]:
        out = await self._inner.propose(rationale, **req)
        self._owner.on_propose(rationale, req, out)
        return out

    async def withdraw(self, id: int, reason: str) -> dict[str, Any]:
        return await self.call("withdraw_intent", id=id, reason=reason)

    async def explain(self, id: int) -> dict[str, Any]:
        return await self.call("explain_intent", id=id)


class Perturbed(PlaybookAgent):
    """The playbook with one thing changed (module doc): hooks on its plan, its reads and its
    proposals."""

    name = ""

    def __init__(self, ep: Any, cfg: Any):
        super().__init__(ep, cfg)
        self.session = SessionProxy(self.session, self)  # type: ignore[assignment]
        self.last_plan: Any = None
        self.changed: list[dict[str, Any]] = []
        self.privileged: dict[str, Any] = {}
        plan = self.engine.plan

        def perturbed(reads: Any) -> Any:
            p = plan(reads)
            self.last_plan = p
            self.perturb(p)
            return p

        self.engine.plan = perturbed  # type: ignore[method-assign]

    def perturb(self, plan: Any) -> None:
        return None

    def on_read(self, name: str, args: dict[str, Any], out: dict[str, Any]) -> dict[str, Any]:
        return out

    def on_propose(self, rationale: str, req: dict[str, Any], out: dict[str, Any]) -> None:
        return None

    def note(self, what: str, **fields: Any) -> None:
        rec = {"t": round(self.ep.sim.t, 3), "perturbation": self.name, "what": what, **fields}
        self.changed.append(rec)
        self.write("perturbation", **{k: v for k, v in rec.items() if k != "t"})

    def describe(self) -> dict[str, Any]:
        out: dict[str, Any] = {"agent": self.name, "id": PLAYBOOK_AGENT, "changes": len(self.changed),
                               "config_label": self.cfg.label}
        if self.privileged:
            out["privileged"] = self.privileged
        return out


class Spam(Perturbed):
    """Follow-up spam: each held look proposed again every 20 s (module doc)."""

    name = "playbook+spam"
    EVERY_S = 20.0

    def __init__(self, ep: Any, cfg: Any):
        super().__init__(ep, cfg)
        self.last: dict[str, float] = {}  # target -> situation time of the last look proposed on it
        self.rationale: dict[str, str] = {}  # target -> the rationale of the newest look on it

    def on_propose(self, rationale: str, req: dict[str, Any], out: dict[str, Any]) -> None:
        rec = R.parse_record(rationale) or {}
        play = str(rec.get("play") or "").partition("@")[0]
        if play in LOOK_PLAYS and rec.get("target"):
            self.last[str(rec["target"])] = self.ep.sim.t
            self.rationale[str(rec["target"])] = rationale

    def wake(self, now: float) -> float | None:
        nxt = super().wake(now)
        if self.started and self.last_plan is not None:
            self.spam(now)
        return nxt

    def spam(self, now: float) -> None:
        b, led = self.last_plan.beliefs, self.engine.led
        for target in sorted(self.last):
            if now - self.last[target] < self.EVERY_S - 1e-9:
                continue
            e = holding(LOOK_PLAYS, target, b, led)
            if e is None or e.get("intent") is None:
                continue
            req = dict(e["request"])
            try:
                out = self.ep.run_async(self.session._inner.propose(self.rationale[target], **req))
            except Exception as ex:  # noqa: BLE001 (refused without an intent: as the engine logs one)
                self.write("error", error=f"propose_intent: {type(ex).__name__}: {ex}", play=e["play"], target=target)
                self.last[target] = now
                continue
            iid, state = out.get("id"), str(out.get("state"))
            refused = out.get("refused")
            if iid is not None:
                # into the engine's own ledger, as its own card (engine.tick does the same for its own)
                led.record(int(iid), play=e["play"], version=e.get("version", "1"), target=target, uid=e.get("uid"),
                           task=str(e.get("task")), request=req, t=self.ep.sim_epoch + now, state=state,
                           basis=dict(e.get("basis") or {}), withdraw_if=list(e.get("withdraw_if") or []),
                           tier=e.get("tier"), refused=str(refused)[:300] if refused else None)
            self.last[target] = now
            self.note("re-proposed", target=target, intent=iid, state=state, of=e.get("intent"), request=req)


class Behind(Perturbed):
    """Behind the target: each look's centre 1.5 x the Puma's EO/IR range behind the contact."""

    name = "playbook+behind"
    FACTOR = 1.5

    def perturb(self, plan: Any) -> None:
        b = plan.beliefs
        for c in plan.selected:
            if c.play not in LOOK_PLAYS or c.task != "orbit" or not c.request.get("center"):
                continue
            v = b.vehicles.get(c.uid)
            rng = float(v.imaging_range_m or 0.0) if v else 0.0
            if rng <= 0:
                continue
            tr = b.tracks.get(c.target)
            at = (float(c.request["center"][0]), float(c.request["center"][1]))
            if tr is not None and tr.course is not None and (tr.speed or 0.0) >= 0.5:
                course, how = float(tr.course), "its shown course"
            else:
                prot = min(b.cfg["protected"] or [{"point": list(GATE)}], key=lambda x: haversine_m(at, tuple(x["point"])))
                course, how = bearing(tuple(prot["point"]), at), "the line from the protected point through it"
            centre = offset(at, (course + 180.0) % 360.0, self.FACTOR * rng)
            old = list(c.request["center"])
            c.request = {**c.request, "center": centre}
            c.derived = {**c.derived, "perturbation": f"centre moved {self.FACTOR:g} x {rng:.0f} m behind ({how})"}
            self.note("moved behind", target=c.target, uid=c.uid, old=old, new=centre, course=round(course, 1), how=how)


class SkipFirst(Perturbed):
    """Skip-first (privileged): the earliest need's track never reaches the playbook's reads."""

    name = "playbook+skip-first"

    def __init__(self, ep: Any, cfg: Any):
        super().__init__(ep, cfg)
        routes = need_routes(ep, float(ep.e.duration_s))
        self.first = routes[0][0].id if routes else None
        self.privileged = {"earliest_need": self.first, "t_need": round(routes[0][1], 1) if routes else None,
                           "truth_reads": 0, "contacts_file": Path(ep.contacts_path or ep.e.contacts).name
                           if (ep.contacts_path or ep.e.contacts) else None,
                           "note": "knows the earliest need from the contacts file and its track from the tracker's "
                                   "association (truth): a perturbation, never an agent under test"}
        self.hidden: set[str] = set()

    def on_read(self, name: str, args: dict[str, Any], out: dict[str, Any]) -> dict[str, Any]:
        if name != "get_tracks" or self.first is None or not isinstance(out, dict):
            return out
        truth = self.ep.sim.truth(association=True)
        self.privileged["truth_reads"] += 1
        drop = {str(a["track_id"]) for a in truth.get("association") or [] if str(a["contact_id"]) == self.first}
        kept = [t for t in out.get("tracks") or [] if str(t.get("id")) not in drop]
        gone = sorted({str(t.get("id")) for t in out.get("tracks") or []} - {str(t.get("id")) for t in kept})
        for tid in gone:
            if tid not in self.hidden:
                self.hidden.add(tid)
                self.note("hid the earliest need's track", track=tid, contact=self.first)
        return {**out, "tracks": kept, "count": len(kept)}


class ReserveBlind(Perturbed):
    """Reserve-blind: no rtl ever leaves."""

    name = "playbook+reserve-blind"

    def perturb(self, plan: Any) -> None:
        dropped = [c for c in plan.selected if c.task == "rtl"]
        if dropped:
            plan.selected = [c for c in plan.selected if c.task != "rtl"]
            for c in dropped:
                self.note("dropped an rtl", play=c.play, target=c.target, uid=c.uid)


#: look-at-ferries: a look lasts at most this long
FERRY_LOOK_S = 90.0
FERRY_PLAY = "perturb.look_at_ferries"


class LookAtFerries(Play):
    """A look at every track shown neutral with an AIS identity, once per track, ranked below every
    real look; let go once the classification is settled, the track is dropped, or after 90 s."""

    name, klass = FERRY_PLAY, P2
    SCORE = 0.2

    def evaluate(self, b: Any, led: Any) -> Result:
        res = Result()
        th = b.cfg.t
        for _, e in sorted(led.entries.items(), key=lambda kv: int(kv[0])):
            if e["play"] != self.name or not runs(e, b):
                continue
            tid, uid = e["target"], e.get("uid")
            v = b.vehicles.get(uid)
            if v is None:
                continue
            tr = b.tracks.get(tid)
            why = None
            if tr is None:
                why = f"{tid} was dropped by the tracker"
            elif tr.kind_conf >= th["classification_settled"] and any(s in CLASSIFYING for _, s, _ in tr.sources):
                why = f"{tid} is classified ({tr.kind} {tr.kind_conf:.2f})"
            elif b.t - float(e.get("state_t") or e.get("proposed_t") or b.t) >= FERRY_LOOK_S:
                why = f"looked at {tid} for {FERRY_LOOK_S:.0f} s"
            if why is None:
                continue
            if any(x["play"] == "isr.release" and x["target"] == tid and x.get("uid") == uid and live(x, b)
                   for _, x in sorted(led.entries.items())):
                continue
            res.candidates.append(Candidate(
                play="isr.release", version=PLAYS["isr.release"].version, klass=P2, score=0.5, target=tid, uid=uid,
                task="hold", request={"task": "hold", "uids": [uid]},
                facts=(track_facts(tr, b) if tr is not None else []) + vehicle_facts(v),
                derived={"running": e["intent"], "action": "It holds where it is", "why": why}, thresholds={},
                withdraw_if=[], basis={"running": e["intent"], "resumed": None}, trigger=[why], claims=(uid,),
                supersedes=int(e["intent"]), vehicle=vehicle_ref(v)))
        looked = {e["target"] for e in led.entries.values() if e["play"] == self.name}
        for tid, tr in sorted(b.tracks.items()):
            if tr.shown != "neutral" or not tr.ais_name or tid in looked or tr.stale or tr.where is None:
                continue
            if holding((self.name,), tid, b, led) is not None:
                continue
            point = tr.where
            free, busy = ranked(b, point, sensor_capable, b.cfg["sensor_platform"])
            if not free:
                res.blocked.append((tid, "no free sensor aircraft"))
                continue
            for rank, v in enumerate(free[:CHOICES]):
                radius = orbit_radius(v, b.cfg)
                res.candidates.append(Candidate(
                    play=self.name, version=self.version, klass=self.klass, score=self.SCORE - RANK_STEP * rank,
                    target=tid, uid=v.uid, task="orbit",
                    request={"task": "orbit", "uids": [v.uid], "center": R.ll(point), "radius_m": radius},
                    facts=track_facts(tr, b) + vehicle_facts(v), derived={"radius_m": radius, "rank": rank}, thresholds={},
                    withdraw_if=[], basis={"affiliation": tr.shown, "ais": tr.ais_name}, trigger=["shown neutral with AIS"],
                    alternatives=alternatives(free[rank:], busy, point), claims=(v.uid,), vehicle=vehicle_ref(v)))
        return res

    def render(self, record: dict[str, Any]) -> str:
        return (f"{R.head(record)} Orbit {record['vehicle']['callsign']} over {record['target']} "
                f"({record['request']['radius_m']:.0f} m radius) for a look at a vessel shown neutral with AIS. {R.tail(record)}")


if FERRY_PLAY not in PLAYS:
    register(LookAtFerries())


class Ferries(Perturbed):
    """Look at ferries: the playbook with `perturb.look_at_ferries` enabled."""

    name = "playbook+ferries"

    def on_propose(self, rationale: str, req: dict[str, Any], out: dict[str, Any]) -> None:
        rec = R.parse_record(rationale) or {}
        if str(rec.get("play") or "").startswith(FERRY_PLAY):
            self.note("look at a neutral with AIS", target=rec.get("target"), intent=out.get("id"), request=req)


PERTURBED = {c.name: c for c in (Spam, Behind, SkipFirst, ReserveBlind, Ferries)}


def make(ep: Any, name: str, cfg: Any) -> Any:
    """The agent `name` for episode `ep`; `cfg` the situation's playbook config (a perturbation
    runs on it; look-at-ferries on it with its play enabled)."""
    if name == "null":
        return NullAgent(ep)
    if name == "random-valid":
        return RandomValid(ep)
    if name == "oracle":
        return Oracle(ep)
    if name == "playbook+ferries":
        table = json.loads(playbook_json(ep.e))  # type: ignore[arg-type]
        table["enable"] = sorted(set(table.get("enable") or []) | {FERRY_PLAY})
        cfg = make_config(table, ep.e.config_name or ep.e.name)
    if name in PERTURBED:
        return PERTURBED[name](ep, cfg)
    raise ValueError(f"unknown agent {name!r}: {', '.join(NAMES)}")
