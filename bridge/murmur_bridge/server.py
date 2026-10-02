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
# No fixed vehicle cap: the bridge opens exactly the ports the fleet manifest lists, and more
# when a bigger fleet is launched. MURMUR_MAX_VEHICLES (optional) pre-opens a fixed range.
DEFAULT_PORTS = int(os.environ.get("MURMUR_MAX_VEHICLES", 32))
PLATFORMS = Path(__file__).resolve().parents[2] / "sim" / "platforms.toml"
FLEET_MANIFEST = Path(os.environ.get("MURMUR_FLEET_MANIFEST",
                                     Path(__file__).resolve().parents[2] / "sim" / "run" / "fleet.json"))
PUSH_HZ = 4

links = {}  # udp port -> VehicleLink
vehicles = {}  # uid -> Vehicle
tasks = TaskManager(vehicles)
clients = set()


identities = {}  # uid -> identity from the launcher's fleet manifest
port_uids = {}  # udp port -> (uid, sysid) from the manifest
# A vehicle the manifest doesn't list (a stray MAVLink source on one of our ports) still shows
# up, under a uid that can't collide with the launcher's 1..N.
SYNTHETIC_UID_BASE = 1_000_000
_manifest_mtime = None
_wanted_ports = set()  # ports the current manifest uses; housekeeping opens any missing


async def ensure_ports(ports):
    """Open a UDP link for every port not yet open (one per vehicle, so per-vehicle link
    stats and link emulation stay honest)."""
    loop = asyncio.get_running_loop()
    new = sorted(p for p in ports if p not in links)
    for port in new:
        link = VehicleLink(port, on_vehicle)
        try:
            await loop.create_datagram_endpoint(lambda link=link: link, local_addr=("0.0.0.0", port))
        except OSError as e:
            print(f"cannot listen on udp {port}: {e}")
            continue
        links[port] = link
    if new:
        print(f"listening on {len(new)} more udp ports ({new[0]}-{new[-1]}); {len(links)} total")


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
    port_uids.clear()
    _wanted_ports.update(v["port"] for v in data.get("vehicles", []) if "port" in v)
    for v in data.get("vehicles", []):
        uid = v.get("uid", v["sysid"])  # manifests from before uid existed: sysid was unique
        identities[uid] = {k: v.get(k) for k in
                           ("callsign", "platform", "name", "short", "role", "domain", "link", "sidc", "sim", "armed",
                            "cruise_alt_m", "min_turn_radius_m")}
        if "port" in v:
            port_uids[v["port"]] = (uid, v["sysid"])
    for uid, veh in vehicles.items():
        veh.identity = identities.get(uid)


def resolve_uid(port, sysid):
    """The manifest entry for the port a vehicle talks on, if its sysid matches; else a
    synthetic uid (stable per port)."""
    entry = port_uids.get(port)
    if entry and entry[1] == sysid:
        return entry[0]
    return SYNTHETIC_UID_BASE + port


def on_vehicle(v):
    load_manifest()
    v.uid = resolve_uid(v.link.port, v.sysid)
    vehicles[v.uid] = v
    v.identity = identities.get(v.uid)
    who = v.identity["callsign"] if v.identity else f"sysid {v.sysid} (uid {v.uid})"
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
        if _wanted_ports - set(links):
            await ensure_ports(_wanted_ports)
        for link in list(links.values()):
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
        self.sent = {}  # uid -> json of last sent snapshot (without trail)
        self.trail = {}  # uid -> trail_count already sent
        self.taskings = None


def delta_for(c, snaps, taskings_json):
    changed = []
    for uid, (snap, js) in snaps.items():
        v = vehicles[uid]
        add = v.trail_since(c.trail.get(uid, 0))
        if js == c.sent.get(uid) and not add:
            continue
        item = dict(snap)
        item["trail_add"] = add
        changed.append(item)
        c.sent[uid] = js
        c.trail[uid] = v.trail_count
    msg = {"type": "delta", "t": round(time.time(), 2), "vehicles": changed}
    if taskings_json != c.taskings:
        msg["taskings"] = json.loads(taskings_json)
        c.taskings = taskings_json
    return msg


async def pusher():
    while True:
        if clients:
            snaps = {}
            for uid in sorted(vehicles):
                snap = vehicles[uid].snapshot(trail=False)
                snaps[uid] = (snap, json.dumps(snap, separators=(",", ":")))
            taskings_json = json.dumps(tasks.snapshot(), separators=(",", ":"))
            for c in list(clients):
                try:
                    await c.ws.send_text(json.dumps(delta_for(c, snaps, taskings_json), separators=(",", ":")))
                except Exception:
                    clients.discard(c)
        await asyncio.sleep(1 / PUSH_HZ)


@asynccontextmanager
async def lifespan(app):
    _wanted_ports.update(range(BASE_PORT, BASE_PORT + DEFAULT_PORTS))
    load_manifest()
    await ensure_ports(_wanted_ports)
    bg = [asyncio.create_task(housekeeping()), asyncio.create_task(pusher())]
    yield
    for t in bg:
        t.cancel()
    for link in links.values():
        if link.transport:
            link.transport.close()


app = FastAPI(title="murmur bridge", lifespan=lifespan)


class TaskRequest(BaseModel):
    task: str
    uids: list[int]
    center: list[float] | None = None  # [lon, lat]
    radius: float | None = None
    route: list[list[float]] | None = None  # [[lon, lat], ...]
    target: int | None = None  # uid, for "track"


@app.get("/api/state")
async def get_state():
    return state_message()


@app.post("/api/tasks")
async def post_task(req: TaskRequest):
    missing = [u for u in req.uids if u not in vehicles]
    if missing:
        raise HTTPException(404, f"unknown vehicles {missing}")
    if not req.uids:
        raise HTTPException(400, "select at least one vehicle")
    params = {k: v for k, v in req.model_dump().items() if k in ("center", "radius", "route", "target") and v is not None}
    try:
        tasking = tasks.submit(req.task, [vehicles[u] for u in req.uids], params)
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
    sysids = {uid: sysid for uid, sysid in port_uids.values()}
    for uid, ident in identities.items():
        members.setdefault(ident.get("platform"), []).append(
            {"uid": uid, "sysid": sysids.get(uid), "callsign": ident.get("callsign")})
    out = []
    for pid, spec in catalog.items():
        kind = SITL_KIND.get(spec.get("vehicle")) if spec["sim"] == "sitl" else STANDIN_KIND.get(spec.get("behavior"))
        caps = KINDS.get(kind, {}) if kind else {}
        min_r = None
        if spec.get("turn_dps") and spec.get("cruise_mps"):
            min_r = round(spec["cruise_mps"] / (spec["turn_dps"] * 3.141592653589793 / 180))
        out.append({
            "id": pid,
            # numeric fields the panel formats read as null when absent
            **{k: None for k in ("cruise_mps", "max_mps", "endurance_h", "climb_mps", "turn_dps")},
            **spec,  # every catalog field passes through (protocol, authenticity, ...)
            "alt_m": spec.get("alt_m", spec.get("typical_alt_m")),
            "min_turn_radius_m": min_r,
            "tasks": sorted(caps.get("tasks", [])),
            "in_fleet": sorted(members.get(pid, []), key=lambda m: m["uid"]),
        })
    return {"platforms": out}


@app.get("/api/stats")
async def get_stats():
    """Per-vehicle packet counters (for load tests; kept out of the UI stream)."""
    return {"t": time.time(), "vehicles": {u: vehicles[u].link.stats.counters() for u in sorted(vehicles)}}


@app.websocket("/ws")
async def ws_endpoint(ws: WebSocket):
    await ws.accept()
    c = Client(ws)
    full = state_message()
    # everything in the full state counts as sent
    for v in full["vehicles"]:
        snap = {k: val for k, val in v.items() if k != "trail"}
        c.sent[v["uid"]] = json.dumps(snap, separators=(",", ":"))
        c.trail[v["uid"]] = vehicles[v["uid"]].trail_count
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
