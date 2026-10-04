"""The storyline: injected situations, executed against a live stack on a wall clock.

Steps (`[[step]]` in a spec; `at_s` counts from the moment the fleet is ready):

    kind = "propose"            a proposal through the arm's proposer
      task = "orbit"            orbit | patrol | hold | rtl | track
      vehicles = {...}          {uids = [..]} | {platform = "cca"} (first of that platform,
                                or all with all = true) | {callsign = "BLADE 1"}
      center = ...              [lon, lat] | {contact = "id", offset_m = [east, north]}
                                (truth position, evaluation only) | {track = {...}} (the
                                track's estimated position) | {vehicle = "self" | uid}
      radius_m = 600
      route = [[lon, lat], ...] or {from = "self", to = <center-like>, legs = 2}
      rationale = "..."         a template: {track} {kind} {affiliation} {reason} {age}
                                {confidence} {callsign} {platform} are filled from the
                                resolved perception (never from truth)
      label = "derived"         legitimate | should_reject | should_void | derived | none
      verify = "..."            contact_affiliation | vehicle_spoofed | track_stale | none
      reason = "..."            why the label (for the report)
      withdraw_after_s = 40     the proposer withdraws it if still waiting (override metric)
      retry_s = 20              keep trying this long if a track or vehicle it names is not
                                there yet (default 20; 0 inside a flood)
    kind = "flood"              count proposals, interval_s apart, cycling templates
    kind = "declare"            an operator declares a track's affiliation (operator.action)
    kind = "link"               a link effect on named vehicles from now (delay_s) for
                                duration_s: cut = true | loss = 0.5 | extra_latency_ms = n,
                                appended to the run's events file for the link emulator
    kind = "note"               a marker in the storyline log
    kind = "engage"             Phase E: nominate an armed platform against a track through
                                eval/engage.py; logged as skipped until the proxy can sign

Track selectors `{track = {...}}` and `declare.track`:
    {id = "T-0001"} | {contact = "id"} (through the evaluation-only association) |
    {select = "most_suspicious" | "nearest" | "stalest", near = [lon, lat], radius_km = 6,
     affiliation = ["suspect", "unknown"], domain = "surface", include_stale = false}

Everything resolved at injection time goes to `storyline.jsonl` (the track, its contact and
both their affiliations, the vehicle's navigation truth), so the metrics can label each
intent without re-reading the heavy logs, and can cross-check against them.
"""

from __future__ import annotations

import asyncio
import json
import math
import time
from pathlib import Path
from typing import Any

from .agent import Proposer, ProposeError
from .bridge import Bridge
from .spec import Spec, Step

SUSPICION = {"hostile": 4, "suspect": 3, "unknown": 2, "neutral": 1, "assumed_friend": 0}
TRUTH_WRONG = ("neutral", "assumed_friend")


def haversine_m(lon1: float, lat1: float, lon2: float, lat2: float) -> float:
    r = 6371000.0
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dp = p2 - p1
    dl = math.radians(lon2 - lon1)
    a = math.sin(dp / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(dl / 2) ** 2
    return 2 * r * math.asin(math.sqrt(a))


def offset(lonlat: list[float], east_m: float, north_m: float) -> list[float]:
    lon, lat = lonlat
    dlat = north_m / 111_320.0
    dlon = east_m / (111_320.0 * math.cos(math.radians(lat)))
    return [round(lon + dlon, 6), round(lat + dlat, 6)]


class StepError(RuntimeError):
    pass


class Log:
    def __init__(self, path: Path):
        self.path = path
        self.path.write_text("")

    def write(self, rec: dict[str, Any]) -> None:
        rec = {"t": round(time.time(), 3), **rec}
        with self.path.open("a") as f:
            f.write(json.dumps(rec) + "\n")


class World:
    """Reads at injection time: perception from the API, truth from the evaluation hook."""

    def __init__(self, bridge: Bridge, run_dir: Path):
        self.bridge = bridge
        self.run_dir = run_dir

    async def vehicles(self) -> list[dict]:
        return await asyncio.to_thread(self.bridge.vehicles)

    async def tracks(self) -> list[dict]:
        return await asyncio.to_thread(self.bridge.tracks)

    async def truth(self) -> dict:
        return (await asyncio.to_thread(self.bridge.truth)) or {}

    async def vehicle_truth(self, uid: int) -> dict | None:
        """The stand-in truth log's latest line for a vehicle (only vehicles that lost GPS
        or were spoofed write lines; absent means healthy)."""
        path = self.run_dir / "standins-truth.jsonl"
        if not path.exists():
            return None

        def read() -> dict | None:
            last = None
            with path.open() as f:
                for line in f:
                    try:
                        rec = json.loads(line)
                    except ValueError:
                        continue
                    if rec.get("uid") == uid:
                        last = rec
            return last

        return await asyncio.to_thread(read)


def select_vehicles(sel: Any, vehicles: list[dict]) -> list[dict]:
    if isinstance(sel, list):
        want = {int(u) for u in sel}
        out = [v for v in vehicles if v.get("uid") in want]
    elif isinstance(sel, dict) and "uids" in sel:
        want = {int(u) for u in sel["uids"]}
        out = [v for v in vehicles if v.get("uid") in want]
    elif isinstance(sel, dict) and "platform" in sel:
        out = [v for v in vehicles if (v.get("identity") or {}).get("platform") == sel["platform"]]
        if not sel.get("all"):
            idx = int(sel.get("index", 0))
            out = out[idx : idx + 1]
    elif isinstance(sel, dict) and "callsign" in sel:
        out = [v for v in vehicles if (v.get("identity") or {}).get("callsign") == sel["callsign"]]
    else:
        raise StepError(f"bad vehicle selector {sel!r}")
    if not out:
        raise StepError(f"no vehicle matches {sel!r}")
    return out


def select_track(sel: Any, tracks: list[dict], association: dict[str, str], now: float) -> dict | None:
    if isinstance(sel, str):
        sel = {"id": sel}
    if "id" in sel:
        return next((t for t in tracks if t["id"] == sel["id"]), None)
    if "contact" in sel:
        tid = association.get(sel["contact"])
        return next((t for t in tracks if t["id"] == tid), None) if tid else None
    how = sel.get("select", "most_suspicious")
    cands = []
    affs = sel.get("affiliation")
    for t in tracks:
        if affs and t.get("affiliation") not in affs:
            continue
        if sel.get("domain") and t.get("domain") != sel["domain"]:
            continue
        if not sel.get("include_stale", False) and t.get("state") == "stale":
            continue
        d = None
        if sel.get("near"):
            near = sel["near"]
            d = haversine_m(t["position"][0], t["position"][1], near[0], near[1])
            if sel.get("radius_km") and d > float(sel["radius_km"]) * 1000:
                continue
        cands.append((t, d if d is not None else 0.0))
    if not cands:
        return None
    if how == "nearest":
        cands.sort(key=lambda c: c[1])
    elif how == "stalest":
        cands.sort(key=lambda c: float(c[0].get("last_seen_t", now)))
    else:  # most_suspicious, nearest among equals
        cands.sort(key=lambda c: (-SUSPICION.get(c[0].get("affiliation"), 0), c[1]))
    return cands[0][0]


def association_map(truth: dict) -> tuple[dict[str, str], dict[str, dict]]:
    """contact id -> track id, and contact id -> truth contact."""
    assoc = {p["contact_id"]: p["track_id"] for p in truth.get("association", []) if "contact_id" in p}
    contacts = {c["id"]: c for c in ((truth.get("truth") or {}).get("message") or {}).get("contacts", [])}
    return assoc, contacts


def target_record(track: dict | None, contact: dict | None, now: float) -> dict[str, Any]:
    rec: dict[str, Any] = {}
    if track:
        rec.update(
            {
                "track_id": track["id"],
                "track_affiliation": track.get("affiliation"),
                "track_reason": track.get("affiliation_reason"),
                "track_state": track.get("state"),
                "track_age_s": round(now - float(track.get("last_seen_t", now)), 1),
                "track_kind": (track.get("identity") or {}).get("kind"),
                "track_confidence": track.get("confidence"),
                "track_position": track.get("position"),
                "track_ais": bool((track.get("identity") or {}).get("ais")),
                "declared": (track.get("declared") or {}).get("affiliation"),
            }
        )
    if contact:
        rec.update(
            {
                "contact_id": contact["id"],
                "truth_affiliation": contact.get("affiliation"),
                "truth_kind": contact.get("kind"),
                "truth_position": [contact.get("lon"), contact.get("lat")],
            }
        )
    return rec


class Storyline:
    """Runs a spec's steps against a live stack. Each step starts at `t_ready + at_s` on its
    own task, so a long step (a flood, a retry) never delays the next one."""

    def __init__(
        self,
        spec: Spec,
        bridge: Bridge,
        proposer: Proposer,
        world: World,
        log: Log,
        t_ready: float,
        declare_operator: str = "op-planner",
        events_file: Path | None = None,
        emu_url: str | None = None,
        engage: Any = None,
    ):
        self.spec = spec
        self.bridge = bridge
        self.proposer = proposer
        self.world = world
        self.log = log
        self.t_ready = t_ready
        self.declare_operator = declare_operator
        self.events_file = events_file
        self.emu_url = emu_url
        self.engage_adapter = engage
        self.tasks: list[asyncio.Task] = []
        self.injected: list[dict[str, Any]] = []
        self._events_lock = asyncio.Lock()

    async def run(self, stop: asyncio.Event) -> None:
        steps = [asyncio.create_task(self._at(step, stop)) for step in self.spec.steps]
        await asyncio.gather(*steps, return_exceptions=True)
        if self.tasks:
            await asyncio.gather(*self.tasks, return_exceptions=True)

    async def _at(self, step: Step, stop: asyncio.Event) -> None:
        due = self.t_ready + step.at_s
        while not stop.is_set():
            left = due - time.time()
            if left <= 0:
                break
            try:
                await asyncio.wait_for(stop.wait(), timeout=min(left, 1.0))
            except asyncio.TimeoutError:
                pass
        if stop.is_set():
            self.log.write({"kind": "skipped", "step": step.id, "reason": "run ended"})
            return
        try:
            await self.execute(step)
        except (StepError, ProposeError) as e:
            self.log.write({"kind": "error", "step": step.id, "t_rel": self.t_rel(), "error": str(e)})
        except Exception as e:  # noqa: BLE001 (a broken step is recorded, the run goes on)
            self.log.write({"kind": "error", "step": step.id, "t_rel": self.t_rel(), "error": repr(e)})

    def t_rel(self, t: float | None = None) -> float:
        return round((t if t is not None else time.time()) - self.t_ready, 1)

    async def execute(self, step: Step) -> None:
        if step.kind == "note":
            self.log.write({"kind": "note", "step": step.id, "t_rel": self.t_rel(), "text": step.params.get("text", "")})
        elif step.kind == "propose":
            await self.propose(step.id, step.params)
        elif step.kind == "flood":
            await self.flood(step)
        elif step.kind == "declare":
            await self.declare(step)
        elif step.kind == "link":
            await self.link(step)
        elif step.kind == "engage":
            await self.engage(step)
        else:
            raise StepError(f"unknown step kind {step.kind}")

    # ---- resolution ----------------------------------------------------------------------

    async def resolve_center(self, c: Any, vehicle: dict | None, now: float) -> tuple[list[float], dict[str, Any]]:
        if isinstance(c, list) and len(c) == 2:
            return [float(c[0]), float(c[1])], {}
        if not isinstance(c, dict):
            raise StepError(f"bad center {c!r}")
        off = c.get("offset_m", [0, 0])
        if "contact" in c:
            truth = await self.world.truth()
            assoc, contacts = association_map(truth)
            contact = contacts.get(c["contact"])
            if contact is None:
                raise StepError(f"contact {c['contact']!r} is not in the truth now")
            tracks = await self.world.tracks()
            track = next((t for t in tracks if t["id"] == assoc.get(c["contact"])), None)
            pos = [contact["lon"], contact["lat"]]
            return offset(pos, off[0], off[1]), target_record(track, contact, now)
        if "track" in c:
            tracks = await self.world.tracks()
            truth = await self.world.truth()
            assoc, contacts = association_map(truth)
            track = select_track(c["track"], tracks, assoc, now)
            if track is None:
                raise StepError(f"no track matches {c['track']!r}")
            by_track = {v: k for k, v in assoc.items()}
            contact = contacts.get(by_track.get(track["id"], ""))
            return offset(list(track["position"]), off[0], off[1]), target_record(track, contact, now)
        if "vehicle" in c:
            if c["vehicle"] == "self":
                if vehicle is None or not vehicle.get("position"):
                    raise StepError("self center: the vehicle has no position")
                pos = list(vehicle["position"])
            else:
                vs = await self.world.vehicles()
                v = next((v for v in vs if v.get("uid") == int(c["vehicle"])), None)
                if v is None or not v.get("position"):
                    raise StepError(f"vehicle {c['vehicle']} has no position")
                pos = list(v["position"])
            return offset(pos, off[0], off[1]), {}
        raise StepError(f"bad center {c!r}")

    async def resolve_route(self, r: Any, vehicle: dict | None, now: float) -> tuple[list[list[float]], dict[str, Any]]:
        if isinstance(r, list):
            return [[float(p[0]), float(p[1])] for p in r], {}
        if isinstance(r, dict) and "to" in r:
            start, _ = await self.resolve_center(r.get("from", {"vehicle": "self"}), vehicle, now)
            end, target = await self.resolve_center(r["to"], vehicle, now)
            legs = max(1, int(r.get("legs", 1)))
            pts = [start]
            for k in range(1, legs + 1):
                f = k / legs
                pts.append([round(start[0] + (end[0] - start[0]) * f, 6), round(start[1] + (end[1] - start[1]) * f, 6)])
            return pts, target
        raise StepError(f"bad route {r!r}")

    def fill(self, template: str, target: dict[str, Any], vehicle: dict) -> str:
        ident = vehicle.get("identity") or {}
        fields = {
            "track": target.get("track_id", "no track"),
            "kind": target.get("track_kind") or "unclassified",
            "affiliation": target.get("track_affiliation") or "unknown",
            "reason": target.get("track_reason") or "",
            "age": f"{target.get('track_age_s', 0):.0f} s",
            "confidence": f"{(target.get('track_confidence') or 0):.2f}",
            "callsign": ident.get("callsign", f"vehicle {vehicle.get('uid')}"),
            "platform": ident.get("platform", "?"),
        }
        try:
            return template.format_map(fields)
        except (KeyError, ValueError) as e:
            raise StepError(f"rationale template: {e}") from e

    async def prepare(self, p: dict[str, Any]) -> tuple[dict[str, Any], str, dict[str, Any], dict]:
        """Resolve a proposal step against the world now: (request, rationale, target, lead
        vehicle). Raises StepError if something it names is not there (yet)."""
        now = time.time()
        vehicles = await self.world.vehicles()
        chosen = select_vehicles(p["vehicles"], vehicles)
        lead = chosen[0]
        req: dict[str, Any] = {"task": p["task"], "uids": [v["uid"] for v in chosen]}
        target: dict[str, Any] = {}
        if p.get("center") is not None:
            req["center"], target = await self.resolve_center(p["center"], lead, now)
        if p.get("radius_m") is not None:
            req["radius_m"] = float(p["radius_m"])
        if p.get("route") is not None:
            req["route"], t2 = await self.resolve_route(p["route"], lead, now)
            target = target or t2
        if p.get("target") is not None:
            req["target"] = int(p["target"])
        for k in ("reserve_pct", "duration_s"):
            if p.get(k) is not None:
                req[k] = p[k]
        rationale = self.fill(str(p.get("rationale", "")), target, lead) or f"{p['task']} on {lead.get('uid')}"
        return req, rationale, target, lead

    async def prepare_with_retry(self, step_id: str, p: dict[str, Any]) -> tuple[dict[str, Any], str, dict[str, Any], dict]:
        """`prepare`, retried every 2 s for `retry_s` (default 20 s): a track the step
        names may not be held yet a few seconds after the fleet is ready."""
        deadline = time.monotonic() + float(p.get("retry_s", 20))
        while True:
            try:
                return await self.prepare(p)
            except StepError as e:
                if time.monotonic() >= deadline:
                    raise
                last = str(e)
            self.log.write({"kind": "retry", "step": step_id, "t_rel": self.t_rel(), "error": last})
            await asyncio.sleep(2.0)

    # ---- steps ---------------------------------------------------------------------------

    async def propose(self, step_id: str, p: dict[str, Any]) -> dict[str, Any]:
        req, rationale, target, lead = await self.prepare_with_retry(step_id, p)
        now = time.time()
        vehicle_truth = await self.world.vehicle_truth(int(lead["uid"]))
        label = p.get("label", "derived")
        rec: dict[str, Any] = {
            "kind": "propose",
            "step": step_id,
            "t_rel": self.t_rel(now),
            "source": {"kind": self.proposer.kind, "id": self.proposer.source_id},
            "request": req,
            "rationale": rationale,
            "label": label,
            "verify": p.get("verify", "contact_affiliation" if label == "derived" else "none"),
            "reason": p.get("reason", ""),
            "target": target,
            "vehicle": {
                "uid": lead.get("uid"),
                "callsign": (lead.get("identity") or {}).get("callsign"),
                "platform": (lead.get("identity") or {}).get("platform"),
                "position": lead.get("position"),
            },
            "vehicle_truth": (
                {k: vehicle_truth.get(k) for k in ("t", "error_m", "spoof_m", "ekf", "fix", "dead_reckoning_s")}
                if vehicle_truth
                else None
            ),
        }
        try:
            out = await self.proposer.propose(req, rationale)
            rec["intent_id"] = out.get("id")
            rec["state"] = out.get("state")
            rec["dispatched"] = out.get("dispatched")
            for k in ("refused", "declined", "model_note"):
                if out.get(k) is not None:
                    rec[k] = out[k]
        except ProposeError as e:
            rec["error"] = str(e)
        self.log.write(rec)
        self.injected.append(rec)
        if rec.get("intent_id") is not None and p.get("withdraw_after_s") is not None:
            self.tasks.append(asyncio.create_task(self.withdraw_later(step_id, int(rec["intent_id"]), float(p["withdraw_after_s"]))))
        return rec

    async def withdraw_later(self, step_id: str, intent_id: int, after_s: float) -> None:
        await asyncio.sleep(after_s)
        try:
            cur = await asyncio.to_thread(self.bridge.intent, intent_id)
        except Exception as e:  # noqa: BLE001
            self.log.write({"kind": "withdraw", "step": step_id, "intent_id": intent_id, "error": str(e)})
            return
        state = (cur.get("intent") or cur).get("state")
        if state not in ("pending_approval", "approved"):
            self.log.write({"kind": "withdraw", "step": step_id, "intent_id": intent_id, "skipped": state})
            return
        try:
            out = await self.proposer.withdraw(intent_id, "the picture changed; the proposer withdraws")
            self.log.write({"kind": "withdraw", "step": step_id, "intent_id": intent_id, "state": (out or {}).get("state")})
        except ProposeError as e:
            self.log.write({"kind": "withdraw", "step": step_id, "intent_id": intent_id, "error": str(e)})

    async def flood(self, step: Step) -> None:
        p = step.params
        count = int(p.get("count", 6))
        interval = float(p.get("interval_s", 8))
        templates = p["templates"]
        for k in range(count):
            t = dict(templates[k % len(templates)])
            t.setdefault("label", p.get("label", "derived"))
            t.setdefault("retry_s", 0)
            try:
                await self.propose(f"{step.id}-{k + 1}", t)
            except (StepError, ProposeError) as e:
                self.log.write({"kind": "error", "step": f"{step.id}-{k + 1}", "t_rel": self.t_rel(), "error": str(e)})
            if k + 1 < count:
                await asyncio.sleep(interval)

    async def declare(self, step: Step) -> None:
        p = step.params
        now = time.time()
        tracks = await self.world.tracks()
        truth = await self.world.truth()
        assoc, contacts = association_map(truth)
        track = select_track(p["track"], tracks, assoc, now)
        if track is None:
            raise StepError(f"declare: no track matches {p['track']!r}")
        by_track = {v: k for k, v in assoc.items()}
        contact = contacts.get(by_track.get(track["id"], ""))
        op = p.get("operator", self.declare_operator)
        out = await asyncio.to_thread(self.bridge.declare, track["id"], p["affiliation"], p.get("note"), op)
        self.log.write(
            {
                "kind": "declare",
                "step": step.id,
                "t_rel": self.t_rel(now),
                "operator": op,
                "affiliation": p["affiliation"],
                "target": target_record(track, contact, now),
                "result": (out.get("declared") or {}).get("affiliation"),
            }
        )

    async def link(self, step: Step) -> None:
        """A scripted link effect on named vehicles, appended to the run's events file as
        an `[[event]]` the link emulator picks up within a second (`at_s` on the emulator's
        own clock, from its `uptime_s`, so the effect starts `delay_s` from now to within
        that second)."""
        p = step.params
        if self.events_file is None or self.emu_url is None:
            raise StepError("link steps need the run's events file and the emulator")
        vehicles = await self.world.vehicles()
        chosen = select_vehicles(p["vehicles"], vehicles)
        uids = [int(v["uid"]) for v in chosen]
        summary = await asyncio.to_thread(Bridge(self.emu_url).try_get, "/stats/summary")
        if not summary or summary.get("uptime_s") is None:
            raise StepError("the link emulator did not answer /stats/summary")
        now = time.time()
        lead = max(1.5, float(p.get("delay_s", 0)))  # the emulator polls the file once a second
        at_s = round(float(summary["uptime_s"]) + lead, 1)
        duration = float(p["duration_s"])
        lines = ["", "[[event]]", f'name = "storyline {step.id}"', f"at_s = {at_s}", f"duration_s = {duration}"]
        lines.append(f"uids = [{', '.join(str(u) for u in uids)}]")
        effect: dict[str, Any] = {}
        if p.get("cut"):
            lines.append("cut = true")
            effect["cut"] = True
        if p.get("loss") is not None:
            lines.append(f"loss = {float(p['loss'])}")
            effect["loss"] = float(p["loss"])
        if p.get("extra_latency_ms") is not None:
            lines.append(f"extra_latency_ms = {float(p['extra_latency_ms'])}")
            effect["extra_latency_ms"] = float(p["extra_latency_ms"])
        async with self._events_lock:
            with self.events_file.open("a") as f:
                f.write("\n".join(lines) + "\n")
        self.log.write(
            {
                "kind": "link",
                "step": step.id,
                "t_rel": self.t_rel(now),
                "uids": uids,
                "callsigns": [(v.get("identity") or {}).get("callsign") for v in chosen],
                "emu_at_s": at_s,
                "starts_t": round(now + lead, 2),
                "ends_t": round(now + lead + duration, 2),
                "effect": effect,
            }
        )

    async def engage(self, step: Step) -> None:
        """Phase E plug-in point (eval/engage.py). Until the bridge takes engagements the
        adapter says so and the step is recorded as skipped, so the report can list the
        scenarios waiting on it."""
        adapter = self.engage_adapter
        if adapter is None or not await adapter.available():
            self.log.write(
                {
                    "kind": "skipped",
                    "step": step.id,
                    "t_rel": self.t_rel(),
                    "reason": "engage is not wired yet (Phase E placeholder, eval/engage.py)",
                    "params": step.params,
                }
            )
            return
        rec = await adapter.run_step(self, step)
        self.log.write(rec)
        self.injected.append(rec)

class TruthSampler:
    """Every `every_s`, truth and perception side by side (evaluation hook) to `truth.jsonl`."""

    def __init__(self, bridge: Bridge, path: Path, every_s: float = 5.0):
        self.bridge = bridge
        self.path = path
        self.every_s = every_s
        self.path.write_text("")

    async def run(self, stop: asyncio.Event) -> None:
        while not stop.is_set():
            snap = await asyncio.to_thread(self.bridge.truth)
            if snap:
                msg = ((snap.get("truth") or {}).get("message")) or {}
                rec = {
                    "t": round(time.time(), 2),
                    "bridge_t": snap.get("t"),
                    "scenario_t": msg.get("t"),
                    "contacts": [
                        {k: c.get(k) for k in ("id", "affiliation", "kind", "lat", "lon", "speed_mps")}
                        for c in msg.get("contacts", [])
                    ],
                    "tracks": [
                        {
                            "id": t["id"],
                            "affiliation": t.get("affiliation"),
                            "state": t.get("state"),
                            "kind": (t.get("identity") or {}).get("kind"),
                            "last_seen_t": t.get("last_seen_t"),
                            "position": t.get("position"),
                            "declared": (t.get("declared") or {}).get("affiliation"),
                        }
                        for t in snap.get("tracks", [])
                    ],
                    "association": snap.get("association", []),
                }
                with self.path.open("a") as f:
                    f.write(json.dumps(rec) + "\n")
            try:
                await asyncio.wait_for(stop.wait(), timeout=self.every_s)
            except asyncio.TimeoutError:
                pass
