"""Group tasking and the per-vehicle command lifecycle.

A Tasking is what the operator issued ("orbit here" to vehicles 1, 3, 7). It fans out
into one Op per vehicle. Each Op walks through:

    queued   -> waiting for the vehicle to have a position fix
    sent     -> first MAVLink message of the task is on the wire
    acked    -> vehicle accepted everything (mission ACK, arm ACK, mode ACK)
    active   -> telemetry shows the vehicle executing it (mode + mission item)
    done     -> telemetry shows the goal reached (on station, lap complete, home)
    failed / superseded

ACK is not done: a COMMAND_ACK only says the autopilot accepted the command, so
"active" and "done" are judged from telemetry, never from acknowledgements.
"""

import asyncio
import itertools
import math
import time

from pymavlink.dialects.v20 import ardupilotmega as mavlink

from .vehicle import AUTOPILOT_COMPID, haversine_m

MODE_AUTO = 10
MODE_RTL = 11
MODE_PLANE_LOITER = 12
MODE_ROVER_HOLD = 4

FRAME_REL = mavlink.MAV_FRAME_GLOBAL_RELATIVE_ALT_INT
FRAME_MISSION = mavlink.MAV_FRAME_MISSION

PLANE_BASE_ALT = 80.0
PLANE_ALT_STEP = 15.0  # vertical separation between planes sharing a task
TAKEOFF_ALT = 60.0

ACK_TIMEOUT_S = 1.0
ACK_RETRIES = 4
MISSION_TIMEOUT_S = 1.5
MISSION_RETRIES = 5
READY_TIMEOUT_S = 120.0

_op_ids = itertools.count(1)
_tasking_ids = itertools.count(1)


class OpFailed(Exception):
    pass


class Op:
    def __init__(self, tasking, vehicle, slot):
        self.id = next(_op_ids)
        self.tasking = tasking
        self.vehicle = vehicle
        self.slot = slot  # index of this vehicle within the group, for altitude/phase spreading
        self.state = "queued"
        self.detail = "waiting for position fix"
        self.history = [("queued", time.time())]
        self.progress = None
        self.task = None

    def set(self, state, detail=None):
        if state != self.state:
            self.state = state
            self.history.append((state, time.time()))
        if detail is not None:
            self.detail = detail

    @property
    def finished(self):
        return self.state in ("done", "failed", "superseded")

    def snapshot(self):
        return {
            "id": self.id,
            "tasking": self.tasking.id,
            "task": self.tasking.kind,
            "sysid": self.vehicle.sysid,
            "state": self.state,
            "detail": self.detail,
            "progress": self.progress,
            "history": [{"state": s, "t": t} for s, t in self.history],
        }


class Tasking:
    def __init__(self, kind, params):
        self.id = next(_tasking_ids)
        self.kind = kind
        self.params = params
        self.created = time.time()
        self.ops = []

    def snapshot(self):
        return {
            "id": self.id,
            "task": self.kind,
            "params": self.params,
            "created": self.created,
            "ops": [op.snapshot() for op in self.ops],
        }


# ---- MAVLink building blocks -----------------------------------------------

async def command_long(op, command, *params):
    """COMMAND_LONG with retries. The `confirmation` field increments on each retry so
    the autopilot can tell a retransmission from a new command."""
    v = op.vehicle
    params = list(params) + [0.0] * (7 - len(params))
    for confirmation in range(ACK_RETRIES + 1):
        v.send(v.mav.command_long_encode(v.sysid, AUTOPILOT_COMPID, command, confirmation, *params))
        ack = await v.wait_for(lambda m: m.get_type() == "COMMAND_ACK" and m.command == command, ACK_TIMEOUT_S)
        if ack is None:
            continue
        if ack.result == mavlink.MAV_RESULT_ACCEPTED:
            return ack
        if ack.result == mavlink.MAV_RESULT_IN_PROGRESS:
            continue
        name = mavlink.enums["MAV_RESULT"][ack.result].name
        raise OpFailed(f"{mavlink.enums['MAV_CMD'][command].name} rejected: {name}")
    raise OpFailed(f"no ACK for {mavlink.enums['MAV_CMD'][command].name} after {ACK_RETRIES + 1} tries")


def item(seq, command, lat=0.0, lon=0.0, alt=0.0, p1=0.0, p2=0.0, p3=0.0, p4=0.0, frame=FRAME_REL):
    return dict(seq=seq, frame=frame, command=command, current=0, autocontinue=1,
                param1=p1, param2=p2, param3=p3, param4=p4,
                x=int(lat * 1e7), y=int(lon * 1e7), z=alt)


async def upload_mission(op, items):
    """MAVLink mission protocol: COUNT -> (REQUEST_INT seq -> ITEM_INT seq)* -> ACK.
    The vehicle drives the transfer, so a lost item just gets requested again; on
    silence we resend whatever we sent last."""
    v = op.vehicle
    count = len(items)

    def encode(i):
        return v.mav.mission_item_int_encode(v.sysid, AUTOPILOT_COMPID, **items[i])

    last = v.mav.mission_count_encode(v.sysid, AUTOPILOT_COMPID, count)
    v.send(last)
    timeouts = 0
    while True:
        msg = await v.wait_for(
            lambda m: m.get_type() in ("MISSION_REQUEST_INT", "MISSION_REQUEST", "MISSION_ACK"),
            MISSION_TIMEOUT_S,
        )
        if msg is None:
            timeouts += 1
            if timeouts > MISSION_RETRIES:
                raise OpFailed("mission upload timed out")
            v.send(last)
            continue
        timeouts = 0
        if msg.get_type() == "MISSION_ACK":
            if msg.type == mavlink.MAV_MISSION_ACCEPTED:
                return
            raise OpFailed(f"mission rejected: {mavlink.enums['MAV_MISSION_RESULT'][msg.type].name}")
        if msg.seq >= count:
            raise OpFailed(f"vehicle requested item {msg.seq} of {count}")
        op.progress = f"upload {msg.seq + 1}/{count}"
        last = encode(msg.seq)
        v.send(last)


async def ensure_armed(op):
    v = op.vehicle
    if v.armed:
        return
    deadline = time.monotonic() + 30
    while True:
        try:
            await command_long(op, mavlink.MAV_CMD_COMPONENT_ARM_DISARM, 1)
            break
        except OpFailed as e:
            # pre-arm checks can fail for a while after boot (EKF settling)
            if time.monotonic() > deadline:
                raise
            reason = v.statustext[-1]["text"] if v.statustext else str(e)
            op.set(op.state, f"arming: {reason}")
            await asyncio.sleep(2)
    for _ in range(20):
        if v.armed:
            return
        await asyncio.sleep(0.25)
    raise OpFailed("arm accepted but vehicle never reported armed")


async def set_mode(op, custom_mode):
    v = op.vehicle
    await command_long(op, mavlink.MAV_CMD_DO_SET_MODE, mavlink.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED, custom_mode)


async def wait_until(op, cond, detail_fn=None, poll=0.5):
    while not cond():
        if detail_fn:
            op.set(op.state, detail_fn())
        await asyncio.sleep(poll)


# ---- geometry ---------------------------------------------------------------

def circle_points(lat, lon, radius_m, n=12, phase=0.0):
    pts = []
    for k in range(n):
        a = phase + 2 * math.pi * k / n
        dlat = radius_m * math.cos(a) / 111_320.0
        dlon = radius_m * math.sin(a) / (111_320.0 * math.cos(math.radians(lat)))
        pts.append((lat + dlat, lon + dlon))
    return pts


# ---- task implementations ---------------------------------------------------

def mission_prefix(op, alt):
    """Item 0 is the home slot (ArduPilot overwrites it). Planes on the ground get a takeoff."""
    v = op.vehicle
    items = [item(0, mavlink.MAV_CMD_NAV_WAYPOINT, v.lat, v.lon, 0)]
    if v.kind == "plane" and not v.airborne:
        items.append(item(1, mavlink.MAV_CMD_NAV_TAKEOFF, alt=min(alt, TAKEOFF_ALT), p1=15))
    return items


def plane_alt(op):
    return PLANE_BASE_ALT + PLANE_ALT_STEP * op.slot


async def run_mission_task(op, items, loop_from, done_cond, done_text, progress_fn):
    v = op.vehicle
    if loop_from is not None:
        items.append(item(len(items), mavlink.MAV_CMD_DO_JUMP, p1=loop_from, p2=-1, frame=FRAME_MISSION))
    first = 1
    op.set("sent", f"uploading {len(items)} mission items")
    await upload_mission(op, items)
    op.set("sent", "starting mission")
    await command_long(op, mavlink.MAV_CMD_DO_SET_MISSION_CURRENT, first)
    await ensure_armed(op)
    await set_mode(op, MODE_AUTO)
    op.set("acked", "accepted, waiting for vehicle to execute")
    op.progress = None
    await wait_until(op, lambda: v.custom_mode == MODE_AUTO and v.armed, lambda: f"mode {v.mode}")
    op.set("active", "executing")
    await wait_until(op, done_cond, progress_fn)
    op.set("done", done_text)


async def task_orbit(op):
    v = op.vehicle
    lon, lat = op.tasking.params["center"]
    radius = float(op.tasking.params.get("radius", 200))
    if v.kind == "plane":
        alt = plane_alt(op)
        items = mission_prefix(op, alt)
        items.append(item(len(items), mavlink.MAV_CMD_NAV_LOITER_UNLIM, lat, lon, alt, p3=radius))
        loop_from = None
        tol = max(60.0, radius * 0.4)
    else:
        n_boats = sum(1 for o in op.tasking.ops if o.vehicle.kind != "plane")
        boat_idx = sum(1 for o in op.tasking.ops[: op.tasking.ops.index(op)] if o.vehicle.kind != "plane")
        phase = 2 * math.pi * boat_idx / max(1, n_boats)
        items = mission_prefix(op, 0)
        loop_from = len(items)
        for plat, plon in circle_points(lat, lon, radius, phase=phase):
            items.append(item(len(items), mavlink.MAV_CMD_NAV_WAYPOINT, plat, plon, 0))
        tol = max(25.0, radius * 0.3)

    def on_station():
        d = v.distance_to(lat, lon)
        return d is not None and abs(d - radius) < tol and (v.kind != "plane" or v.airborne)

    def progress():
        d = v.distance_to(lat, lon)
        return f"{d:.0f} m from center" if d is not None else ""

    await run_mission_task(op, items, loop_from, on_station, "on station", progress)
    # keep reporting while orbiting
    op.progress = "orbiting"


async def task_patrol(op):
    v = op.vehicle
    route = op.tasking.params["route"]
    alt = plane_alt(op) if v.kind == "plane" else 0.0
    items = mission_prefix(op, alt)
    loop_from = len(items)
    for lon, lat in route:
        items.append(item(len(items), mavlink.MAV_CMD_NAV_WAYPOINT, lat, lon, alt))
    last_wp = len(items) - 1
    reached_last = False

    def watch():
        # lap complete = we were heading to the last waypoint, then DO_JUMP sent us back
        nonlocal reached_last
        seq = v.mission_seq
        if seq == last_wp:
            reached_last = True
        return reached_last and seq is not None and seq < last_wp

    def progress():
        seq = v.mission_seq
        if seq is None or seq < loop_from:
            return "en route to first waypoint" if v.kind != "plane" or v.airborne else "taking off"
        return f"waypoint {seq - loop_from + 1}/{len(route)}"

    await run_mission_task(op, items, loop_from, watch, "first lap complete, patrolling", progress)
    op.progress = "patrolling"


async def task_rtl(op):
    v = op.vehicle
    op.set("sent", "commanding RTL")
    await set_mode(op, MODE_RTL)
    op.set("acked", "accepted")
    await wait_until(op, lambda: v.custom_mode == MODE_RTL, lambda: f"mode {v.mode}")
    op.set("active", "returning")
    tol = 250.0 if v.kind == "plane" else 15.0

    def home_dist():
        if v.home is None:
            return None
        return v.distance_to(*v.home)

    await wait_until(op, lambda: (home_dist() or 1e9) < tol,
                     lambda: f"{home_dist():.0f} m from home" if home_dist() is not None else "")
    op.set("done", "home")


async def task_hold(op):
    v = op.vehicle
    mode = MODE_PLANE_LOITER if v.kind == "plane" else MODE_ROVER_HOLD
    op.set("sent", "commanding hold")
    await set_mode(op, mode)
    op.set("acked", "accepted")
    await wait_until(op, lambda: v.custom_mode == mode, lambda: f"mode {v.mode}")
    op.set("done", "holding")


TASKS = {"orbit": task_orbit, "patrol": task_patrol, "rtl": task_rtl, "hold": task_hold}


class TaskManager:
    def __init__(self):
        self.taskings = []
        self.active_by_vehicle = {}

    def submit(self, kind, vehicles, params):
        if kind not in TASKS:
            raise ValueError(f"unknown task {kind}")
        if kind == "orbit" and "center" not in params:
            raise ValueError("orbit needs center [lon, lat]")
        if kind == "patrol" and len(params.get("route", [])) < 2:
            raise ValueError("patrol needs a route of at least 2 points")
        tasking = Tasking(kind, params)
        planes = [v for v in vehicles if v.kind == "plane"]
        for v in vehicles:
            slot = planes.index(v) if v in planes else 0
            tasking.ops.append(Op(tasking, v, slot))
        for op in tasking.ops:
            prev = self.active_by_vehicle.get(op.vehicle.sysid)
            if prev and not prev.finished:
                prev.task.cancel()
                prev.set("superseded", f"replaced by {kind} #{tasking.id}")
            self.active_by_vehicle[op.vehicle.sysid] = op
            op.task = asyncio.create_task(self._run(op))
        self.taskings.append(tasking)
        del self.taskings[:-30]
        return tasking

    async def _run(self, op):
        v = op.vehicle
        try:
            if not v.has_position:
                deadline = time.monotonic() + READY_TIMEOUT_S
                while not v.has_position:
                    if time.monotonic() > deadline:
                        raise OpFailed("vehicle never got a position fix")
                    await asyncio.sleep(0.5)
            await TASKS[op.tasking.kind](op)
        except asyncio.CancelledError:
            pass
        except OpFailed as e:
            op.set("failed", str(e))
        except Exception as e:  # keep one bad op from killing the bridge
            op.set("failed", f"internal error: {e!r}")

    def snapshot(self):
        return [t.snapshot() for t in reversed(self.taskings)]
