"""FastAPI app: UDP MAVLink links in, JSON over WebSocket out, tasking via REST."""

import asyncio
import json
import os
import time
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI, HTTPException, WebSocket, WebSocketDisconnect
from pydantic import BaseModel

from pymavlink.dialects.v20 import ardupilotmega as mavlink

import tomllib

from .tasks import KINDS, TaskManager
from .vehicle import VehicleLink

BASE_PORT = int(os.environ.get("MURMUR_BASE_PORT", 14560))
MAX_VEHICLES = int(os.environ.get("MURMUR_MAX_VEHICLES", 128))
PLATFORMS = Path(__file__).resolve().parents[2] / "sim" / "platforms.toml"
FLEET_MANIFEST = Path(os.environ.get("MURMUR_FLEET_MANIFEST",
                                     Path(__file__).resolve().parents[2] / "sim" / "run" / "fleet.json"))
PUSH_HZ = 4

links = []
vehicles = {}
tasks = TaskManager(vehicles)
clients = set()


identities = {}  # sysid -> identity from the launcher's fleet manifest
_manifest_mtime = None


def load_manifest():
    """Re-read sim/run/fleet.json when it changes (a new fleet was launched)."""
    global _manifest_mtime
    try:
        mtime = FLEET_MANIFEST.stat().st_mtime
    except FileNotFoundError:
        return
    if mtime == _manifest_mtime:
        return
    _manifest_mtime = mtime
    try:
        data = json.loads(FLEET_MANIFEST.read_text())
    except (OSError, ValueError):
        return
    identities.clear()
    for v in data.get("vehicles", []):
        identities[v["sysid"]] = {k: v.get(k) for k in
                                  ("callsign", "platform", "name", "short", "role", "domain", "link", "sidc", "sim",
                                   "cruise_alt_m", "min_turn_radius_m")}
    for sysid, veh in vehicles.items():
        veh.identity = identities.get(sysid)


def on_vehicle(v):
    vehicles[v.sysid] = v
    load_manifest()
    v.identity = identities.get(v.sysid)
    who = v.identity["callsign"] if v.identity else f"sysid {v.sysid}"
    print(f"{who} connected on udp {v.link.port}")


def telemetry_plan(v):
    """(message id, Hz) per vehicle. Position fastest for aircraft; everything else slow.
    Event messages (HEARTBEAT, STATUSTEXT, COMMAND_ACK, mission protocol) aren't streams.
    Was REQUEST_DATA_STREAM ALL at 4 Hz: ~94 msg/s per ArduPilot vehicle."""
    M = mavlink
    air = (v.identity or {}).get("domain") == "air" or v.kind in ("plane", "vtol", "copter", "blimp")
    plan = [
        (M.MAVLINK_MSG_ID_GLOBAL_POSITION_INT, 2 if air else 1),
        (M.MAVLINK_MSG_ID_ATTITUDE, 1),
        (M.MAVLINK_MSG_ID_VFR_HUD, 1),
        (M.MAVLINK_MSG_ID_MISSION_CURRENT, 1),
        (M.MAVLINK_MSG_ID_SYS_STATUS, 0.5),
        (M.MAVLINK_MSG_ID_GPS_RAW_INT, 0.5),
        (M.MAVLINK_MSG_ID_EKF_STATUS_REPORT, 0.5),
    ]
    if v.kind == "tracker":
        plan.append((M.MAVLINK_MSG_ID_NAV_CONTROLLER_OUTPUT, 1))
    return plan


async def housekeeping():
    """GCS heartbeat, telemetry stream requests, and RTT pings."""
    tick = 0
    while True:
        if tick % 5 == 0:
            load_manifest()
        for link in links:
            v = link.vehicle
            if v is None:
                continue
            link.send(link.mav.heartbeat_encode(
                mavlink.MAV_TYPE_GCS, mavlink.MAV_AUTOPILOT_INVALID, 0, 0, mavlink.MAV_STATE_ACTIVE))
            link.send_timesync()
            if v.kind == "plane" and v.is_vtol is None:
                link.send(link.mav.param_request_read_encode(v.sysid, 1, b"Q_ENABLE", -1))
            # telemetry plan: ask for exactly what the console uses, at rates by domain.
            # Re-sent every 30 s (cheap) so a rebooted vehicle picks it up again.
            if tick % 30 == 0 or (v.kind and not getattr(v, "_rates_sent", False)):
                for msg_id, hz in telemetry_plan(v):
                    link.send(link.mav.command_long_encode(
                        v.sysid, 1, mavlink.MAV_CMD_SET_MESSAGE_INTERVAL, 0, msg_id, 1e6 / hz, 0, 0, 0, 0, 0))
                v._rates_sent = True
        tick += 1
        await asyncio.sleep(1)


def state_message():
    """Full state: sent once when a client connects (and served by /api/state)."""
    return {
        "type": "state",
        "t": round(time.time(), 2),
        "vehicles": [vehicles[k].snapshot() for k in sorted(vehicles)],
        "taskings": tasks.snapshot(),
    }


class Client:
    """What one browser already has, so each push sends only what changed."""

    def __init__(self, ws):
        self.ws = ws
        self.sent = {}  # sysid -> json of last sent snapshot (without trail)
        self.trail = {}  # sysid -> trail_count already sent
        self.taskings = None


def delta_for(c, snaps, taskings_json):
    changed = []
    for sysid, (snap, js) in snaps.items():
        v = vehicles[sysid]
        add = v.trail_since(c.trail.get(sysid, 0))
        if js == c.sent.get(sysid) and not add:
            continue
        item = dict(snap)
        item["trail_add"] = add
        changed.append(item)
        c.sent[sysid] = js
        c.trail[sysid] = v.trail_count
    msg = {"type": "delta", "t": round(time.time(), 2), "vehicles": changed}
    if taskings_json != c.taskings:
        msg["taskings"] = json.loads(taskings_json)
        c.taskings = taskings_json
    return msg


async def pusher():
    while True:
        if clients:
            snaps = {}
            for sysid in sorted(vehicles):
                snap = vehicles[sysid].snapshot(trail=False)
                snaps[sysid] = (snap, json.dumps(snap, separators=(",", ":")))
            taskings_json = json.dumps(tasks.snapshot(), separators=(",", ":"))
            for c in list(clients):
                try:
                    await c.ws.send_text(json.dumps(delta_for(c, snaps, taskings_json), separators=(",", ":")))
                except Exception:
                    clients.discard(c)
        await asyncio.sleep(1 / PUSH_HZ)


@asynccontextmanager
async def lifespan(app):
    loop = asyncio.get_running_loop()
    for i in range(MAX_VEHICLES):
        link = VehicleLink(BASE_PORT + i, on_vehicle)
        await loop.create_datagram_endpoint(lambda link=link: link, local_addr=("0.0.0.0", BASE_PORT + i))
        links.append(link)
    print(f"listening for vehicles on udp {BASE_PORT}-{BASE_PORT + MAX_VEHICLES - 1}")
    bg = [asyncio.create_task(housekeeping()), asyncio.create_task(pusher())]
    yield
    for t in bg:
        t.cancel()
    for link in links:
        if link.transport:
            link.transport.close()


app = FastAPI(title="murmur bridge", lifespan=lifespan)


class TaskRequest(BaseModel):
    task: str
    sysids: list[int]
    center: list[float] | None = None  # [lon, lat]
    radius: float | None = None
    route: list[list[float]] | None = None  # [[lon, lat], ...]
    target: int | None = None  # sysid, for "track"


@app.get("/api/state")
async def get_state():
    return state_message()


@app.post("/api/tasks")
async def post_task(req: TaskRequest):
    missing = [s for s in req.sysids if s not in vehicles]
    if missing:
        raise HTTPException(404, f"unknown vehicles {missing}")
    if not req.sysids:
        raise HTTPException(400, "select at least one vehicle")
    params = {k: v for k, v in req.model_dump().items() if k in ("center", "radius", "route", "target") and v is not None}
    try:
        tasking = tasks.submit(req.task, [vehicles[s] for s in req.sysids], params)
    except ValueError as e:
        raise HTTPException(400, str(e))
    return tasking.snapshot()


# which bridge kind (capability table row) each platform runs as
SITL_KIND = {"plane": "plane", "vtol": "vtol", "copter": "copter", "blimp": "blimp", "tracker": "tracker",
             "boat": "boat", "sailboat": "boat", "ugv": "ugv", "sub": "sub"}
STANDIN_KIND = {"plane": "plane", "boat": "boat", "sub": "sub", "drifter": None}


@app.get("/api/platforms")
async def get_platforms():
    """The capability catalog: every platform type, what it can do, and which fleet vehicles
    are that type. Tasks come from the same capability table that drives tasking."""
    load_manifest()
    catalog = tomllib.loads(PLATFORMS.read_text())
    members = {}
    for sysid, ident in identities.items():
        members.setdefault(ident.get("platform"), []).append({"sysid": sysid, "callsign": ident.get("callsign")})
    out = []
    for pid, spec in catalog.items():
        kind = SITL_KIND.get(spec.get("vehicle")) if spec["sim"] == "sitl" else STANDIN_KIND.get(spec.get("behavior"))
        caps = KINDS.get(kind, {}) if kind else {}
        min_r = None
        if spec.get("turn_dps") and spec.get("cruise_mps"):
            min_r = round(spec["cruise_mps"] / (spec["turn_dps"] * 3.141592653589793 / 180))
        out.append({
            "id": pid,
            **{k: spec.get(k) for k in ("name", "short", "role", "domain", "sim", "link", "sidc", "callsign",
                                        "cruise_mps", "max_mps", "endurance_h", "climb_mps", "turn_dps")},
            "alt_m": spec.get("alt_m", spec.get("typical_alt_m")),
            "min_turn_radius_m": min_r,
            "tasks": sorted(caps.get("tasks", [])),
            "in_fleet": sorted(members.get(pid, []), key=lambda m: m["sysid"]),
        })
    return {"platforms": out}


@app.get("/api/stats")
async def get_stats():
    """Per-vehicle packet counters (for load tests; kept out of the UI stream)."""
    return {"t": time.time(), "vehicles": {s: vehicles[s].link.stats.counters() for s in sorted(vehicles)}}


@app.websocket("/ws")
async def ws_endpoint(ws: WebSocket):
    await ws.accept()
    c = Client(ws)
    full = state_message()
    # everything in the full state counts as sent
    for v in full["vehicles"]:
        snap = {k: val for k, val in v.items() if k != "trail"}
        c.sent[v["sysid"]] = json.dumps(snap, separators=(",", ":"))
        c.trail[v["sysid"]] = vehicles[v["sysid"]].trail_count
    c.taskings = json.dumps(full["taskings"], separators=(",", ":"))
    clients.add(c)
    try:
        await ws.send_text(json.dumps(full, separators=(",", ":")))
        while True:
            await ws.receive_text()  # client->server traffic goes over REST; this just detects close
    except WebSocketDisconnect:
        pass
    finally:
        clients.discard(c)
