"""FastAPI app: UDP MAVLink links in, JSON over WebSocket out, tasking via REST."""

import asyncio
import json
import os
import time
from contextlib import asynccontextmanager

from fastapi import FastAPI, HTTPException, WebSocket, WebSocketDisconnect
from pydantic import BaseModel

from pymavlink.dialects.v20 import ardupilotmega as mavlink

from .tasks import TaskManager
from .vehicle import VehicleLink

BASE_PORT = int(os.environ.get("MURMUR_BASE_PORT", 14560))
MAX_VEHICLES = int(os.environ.get("MURMUR_MAX_VEHICLES", 16))
STREAM_HZ = int(os.environ.get("MURMUR_STREAM_HZ", 4))
PUSH_HZ = 5

links = []
vehicles = {}
tasks = TaskManager(vehicles)
clients = set()


def on_vehicle(v):
    vehicles[v.sysid] = v
    print(f"vehicle sysid {v.sysid} connected on udp {v.link.port}")


async def housekeeping():
    """GCS heartbeat, telemetry stream requests, and RTT pings."""
    tick = 0
    while True:
        for link in links:
            v = link.vehicle
            if v is None:
                continue
            link.send(link.mav.heartbeat_encode(
                mavlink.MAV_TYPE_GCS, mavlink.MAV_AUTOPILOT_INVALID, 0, 0, mavlink.MAV_STATE_ACTIVE))
            link.send_timesync()
            if v.kind == "plane" and v.is_vtol is None:
                link.send(link.mav.param_request_read_encode(v.sysid, 1, b"Q_ENABLE", -1))
            # re-request streams every 10s; cheap, and recovers from a vehicle reboot
            if tick % 10 == 0:
                link.send(link.mav.request_data_stream_encode(
                    v.sysid, 1, mavlink.MAV_DATA_STREAM_ALL, STREAM_HZ, 1))
        tick += 1
        await asyncio.sleep(1)


def state_message():
    return {
        "type": "state",
        "t": time.time(),
        "vehicles": [vehicles[k].snapshot() for k in sorted(vehicles)],
        "taskings": tasks.snapshot(),
    }


async def pusher():
    while True:
        if clients:
            payload = json.dumps(state_message())
            dead = []
            for ws in list(clients):
                try:
                    await ws.send_text(payload)
                except Exception:
                    dead.append(ws)
            for ws in dead:
                clients.discard(ws)
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


@app.websocket("/ws")
async def ws_endpoint(ws: WebSocket):
    await ws.accept()
    clients.add(ws)
    try:
        await ws.send_text(json.dumps(state_message()))
        while True:
            await ws.receive_text()  # client->server traffic goes over REST; this just detects close
    except WebSocketDisconnect:
        pass
    finally:
        clients.discard(ws)
