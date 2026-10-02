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

Each ArduPilot firmware (Plane, Copter, Rover, Sub, Blimp, AntennaTracker) numbers its
modes differently and supports different tasks; KINDS below is the capability table.
"""

import asyncio
import itertools
import math
import time

from pymavlink.dialects.v20 import ardupilotmega as mavlink

from .vehicle import AUTOPILOT_COMPID, FLYING_KINDS

FRAME_REL = mavlink.MAV_FRAME_GLOBAL_RELATIVE_ALT_INT
FRAME_MISSION = mavlink.MAV_FRAME_MISSION

# Per kind: which tasks it can do, mode names, mission altitude (base + step per vehicle
# of that kind in the same tasking, for vertical separation), how it gets off the ground,
# how orbits are flown, and how close to home counts as "home".
KINDS = {
    "plane": dict(tasks={"orbit", "patrol", "hold", "rtl"}, auto="AUTO", rtl="RTL", hold="LOITER",
                  alt=(80.0, 15.0), takeoff=mavlink.MAV_CMD_NAV_TAKEOFF, orbit="loiter", home_tol=250.0),
    "vtol": dict(tasks={"orbit", "patrol", "hold", "rtl"}, auto="AUTO", rtl="RTL", hold="LOITER",
                 alt=(70.0, 15.0), takeoff=mavlink.MAV_CMD_NAV_VTOL_TAKEOFF, orbit="loiter", home_tol=250.0),
    "copter": dict(tasks={"orbit", "patrol", "hold", "rtl"}, auto="AUTO", rtl="RTL", hold="LOITER",
                   alt=(30.0, 5.0), takeoff=mavlink.MAV_CMD_NAV_TAKEOFF, orbit="waypoints", home_tol=15.0),
    "blimp": dict(tasks={"orbit", "patrol", "hold", "rtl"}, auto="AUTO", rtl="RTL", hold="LOITER",
                  alt=(20.0, 5.0), takeoff=None, orbit="waypoints", home_tol=30.0),
    "boat": dict(tasks={"orbit", "patrol", "hold", "rtl"}, auto="AUTO", rtl="RTL", hold="HOLD",
                 alt=(0.0, 0.0), takeoff=None, orbit="waypoints", home_tol=15.0),
    # ArduSub has no RTL mode; "rtl" is flown as a one-waypoint mission back to home.
    "sub": dict(tasks={"orbit", "patrol", "hold", "rtl"}, auto="AUTO", rtl=None, hold="POSHOLD",
                alt=(-5.0, -2.0), takeoff=None, orbit="waypoints", home_tol=20.0),
    "tracker": dict(tasks={"track", "hold"}, auto="AUTO", rtl=None, hold="STOP",
                    alt=(0.0, 0.0), takeoff=None, orbit=None, home_tol=0.0),
}
TAKEOFF_ALT = {"plane": 60.0, "vtol": 40.0, "copter": 30.0}

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
        self.slot = slot  # index among same-kind vehicles in this tasking, for altitude/phase spreading
        self.state = "queued"
        self.detail = "waiting for navigation (GPS + EKF)"
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

    @property
    def caps(self):
        return KINDS[self.vehicle.kind]

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
    def __init__(self, kind, params, registry):
        self.id = next(_tasking_ids)
        self.registry = registry  # sysid -> Vehicle, for tasks that involve other vehicles
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


async def set_mode(op, name, patience_s=20):
    """Set a mode by name; returns the mode number to watch for in heartbeats.
    Autopilots refuse modes transiently (e.g. AUTO while the EKF settles), so a
    rejection is retried for a while before the op fails."""
    v = op.vehicle
    try:
        number = v.mode_number(name)
    except KeyError as e:
        raise OpFailed(str(e))
    deadline = time.monotonic() + patience_s
    while True:
        try:
            await command_long(op, mavlink.MAV_CMD_DO_SET_MODE, mavlink.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED, number)
            return number
        except OpFailed as e:
            if time.monotonic() > deadline:
                reason = v.statustext[-1]["text"] if v.statustext else ""
                raise OpFailed(f"{e} ({reason})" if reason else str(e))
            op.set(op.state, f"{name} refused, retrying")
            await asyncio.sleep(2)


async def wait_until(op, cond, detail_fn=None, poll=0.5, guard=None):
    """Poll until cond(). guard() may return an error string to fail the op early."""
    while not cond():
        if guard:
            err = guard()
            if err:
                raise OpFailed(err)
        if detail_fn:
            op.set(op.state, detail_fn())
        await asyncio.sleep(poll)


def with_stall_warning(v, detail_fn, after_s=10.0, min_speed=0.3):
    """Wrap a progress message: if a vehicle that should be moving shows no ground speed
    for after_s, say so. The autopilot can be in AUTO and still not going anywhere
    (aground, fouled, stuck): that's what the operator needs to see."""
    moving_at = time.monotonic()

    def detail():
        nonlocal moving_at
        if (v.groundspeed or 0) >= min_speed:
            moving_at = time.monotonic()
        base = detail_fn() if detail_fn else ""
        stalled = time.monotonic() - moving_at
        if stalled >= after_s:
            return f"NOT MOVING for {stalled:.0f}s (aground or stuck?) · {base}"
        return base

    return detail


def stays_in_mode(v, mode, grace_s=3.0):
    """Guard: fail if the vehicle leaves `mode` (failsafe, pilot, another GCS) for longer
    than grace_s, quoting what the autopilot last said."""
    left_at = None

    def check():
        nonlocal left_at
        if v.custom_mode == mode:
            left_at = None
            return None
        left_at = left_at or time.monotonic()
        if time.monotonic() - left_at < grace_s:
            return None
        said = f": {v.statustext[-1]['text']}" if v.statustext else ""
        return f"vehicle left the task, now in {v.mode}{said}"

    return check


# ---- geometry ---------------------------------------------------------------

def circle_points(lat, lon, radius_m, n=12, phase=0.0):
    pts = []
    for k in range(n):
        a = phase + 2 * math.pi * k / n
        dlat = radius_m * math.cos(a) / 111_320.0
        dlon = radius_m * math.sin(a) / (111_320.0 * math.cos(math.radians(lat)))
        pts.append((lat + dlat, lon + dlon))
    return pts


def bearing_deg(lat1, lon1, lat2, lon2):
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dl = math.radians(lon2 - lon1)
    y = math.sin(dl) * math.cos(p2)
    x = math.cos(p1) * math.sin(p2) - math.sin(p1) * math.cos(p2) * math.cos(dl)
    return math.degrees(math.atan2(y, x)) % 360


# ---- task implementations ---------------------------------------------------

def task_alt(op):
    base, step = op.caps["alt"]
    return base + step * op.slot


def mission_prefix(op, alt):
    """Item 0 is the home slot (ArduPilot overwrites it). Grounded aircraft get a takeoff."""
    v = op.vehicle
    items = [item(0, mavlink.MAV_CMD_NAV_WAYPOINT, v.lat, v.lon, 0)]
    takeoff = op.caps["takeoff"]
    if takeoff is not None and not v.airborne:
        p1 = 15 if v.kind == "plane" else 0  # plane: takeoff pitch
        items.append(item(1, takeoff, alt=min(alt, TAKEOFF_ALT[v.kind]), p1=p1))
    return items


async def run_mission_task(op, items, loop_from, done_cond, done_text, progress_fn):
    v = op.vehicle
    if loop_from is not None:
        items.append(item(len(items), mavlink.MAV_CMD_DO_JUMP, p1=loop_from, p2=-1, frame=FRAME_MISSION))
    op.set("sent", f"uploading {len(items)} mission items")
    await upload_mission(op, items)
    op.set("sent", "starting mission")
    await command_long(op, mavlink.MAV_CMD_DO_SET_MISSION_CURRENT, 1)
    await ensure_armed(op)
    auto = await set_mode(op, op.caps["auto"])
    op.set("acked", "accepted, waiting for vehicle to execute")
    op.progress = None
    await wait_until(op, lambda: v.custom_mode == auto and v.armed, lambda: f"mode {v.mode}")
    op.set("active", "executing")
    await wait_until(op, done_cond, with_stall_warning(v, progress_fn), guard=stays_in_mode(v, auto))
    op.set("done", done_text)


async def task_orbit(op):
    v = op.vehicle
    lon, lat = op.tasking.params["center"]
    radius = float(op.tasking.params.get("radius", 200))
    alt = task_alt(op)
    items = mission_prefix(op, alt)
    if op.caps["orbit"] == "loiter":
        # fixed wing: one loiter-forever item at the center, autopilot flies the circle
        items.append(item(len(items), mavlink.MAV_CMD_NAV_LOITER_UNLIM, lat, lon, alt, p3=radius))
        loop_from = None
        tol = max(60.0, radius * 0.4)
    else:
        # everyone else: a ring of waypoints, looped; same-kind vehicles start spread around it
        same = [o for o in op.tasking.ops if o.vehicle.kind == v.kind]
        phase = 2 * math.pi * op.slot / max(1, len(same))
        loop_from = len(items)
        for plat, plon in circle_points(lat, lon, radius, phase=phase):
            items.append(item(len(items), mavlink.MAV_CMD_NAV_WAYPOINT, plat, plon, alt))
        tol = max(25.0, radius * 0.3)

    def on_station():
        d = v.distance_to(lat, lon)
        return d is not None and abs(d - radius) < tol and (v.kind not in FLYING_KINDS or v.airborne)

    def progress():
        d = v.distance_to(lat, lon)
        return f"{d:.0f} m from center" if d is not None else ""

    await run_mission_task(op, items, loop_from, on_station, "on station", progress)
    op.progress = "orbiting"


async def task_patrol(op):
    v = op.vehicle
    route = op.tasking.params["route"]
    alt = task_alt(op)
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
            return "taking off" if op.caps["takeoff"] is not None and not v.airborne else "en route to first waypoint"
        return f"waypoint {seq - loop_from + 1}/{len(route)}"

    await run_mission_task(op, items, loop_from, watch, "first lap complete, patrolling", progress)
    op.progress = "patrolling"


async def task_rtl(op):
    v = op.vehicle

    def home_dist():
        return None if v.home is None else v.distance_to(*v.home)

    if op.caps["rtl"] is None:
        # no RTL mode (Sub): fly a one-waypoint mission home at the current task depth
        if v.home is None:
            raise OpFailed("home position unknown")
        items = [item(0, mavlink.MAV_CMD_NAV_WAYPOINT, v.lat, v.lon, 0),
                 item(1, mavlink.MAV_CMD_NAV_WAYPOINT, v.home[0], v.home[1], task_alt(op))]
        await run_mission_task(op, items, None, lambda: (home_dist() or 1e9) < op.caps["home_tol"], "home",
                               lambda: f"{home_dist():.0f} m from home" if home_dist() is not None else "")
        return

    op.set("sent", "commanding RTL")
    mode = await set_mode(op, op.caps["rtl"])
    op.set("acked", "accepted")
    await wait_until(op, lambda: v.custom_mode == mode, lambda: f"mode {v.mode}")
    op.set("active", "returning")
    await wait_until(op, lambda: (home_dist() or 1e9) < op.caps["home_tol"],
                     lambda: f"{home_dist():.0f} m from home" if home_dist() is not None else "")
    op.set("done", "home")


async def task_hold(op):
    v = op.vehicle
    op.set("sent", f"commanding {op.caps['hold']}")
    mode = await set_mode(op, op.caps["hold"])
    op.set("acked", "accepted")
    await wait_until(op, lambda: v.custom_mode == mode, lambda: f"mode {v.mode}")
    op.set("done", "holding")


async def task_track(op):
    """Point an antenna tracker at another vehicle. AntennaTracker in AUTO steers toward
    whatever GLOBAL_POSITION_INT it hears from SYSID_TARGET, so the bridge relays the
    target's position messages to the tracker and sets SYSID_TARGET."""
    tracker = op.vehicle
    target = op.tasking.registry.get(op.tasking.params["target"])
    if target is None or target is tracker:
        raise OpFailed("pick one other vehicle to track")
    op.set("sent", f"pointing at sysid {target.sysid}")
    for v in op.tasking.registry.values():
        v.forward_to.discard(tracker.link)
    target.forward_to.add(tracker.link)
    tracker.tracking = target.sysid

    name = b"SYSID_TARGET"
    for _ in range(ACK_RETRIES + 1):
        tracker.send(tracker.mav.param_set_encode(tracker.sysid, AUTOPILOT_COMPID, name, float(target.sysid),
                                                  mavlink.MAV_PARAM_TYPE_REAL32))
        echo = await tracker.wait_for(
            lambda m: m.get_type() == "PARAM_VALUE" and m.param_id == "SYSID_TARGET", ACK_TIMEOUT_S)
        if echo is not None and round(echo.param_value) == target.sysid:
            break
    else:
        raise OpFailed("tracker did not confirm SYSID_TARGET")
    await ensure_armed(op)
    auto = await set_mode(op, op.caps["auto"])
    op.set("acked", "accepted")
    await wait_until(op, lambda: tracker.custom_mode == auto, lambda: f"mode {tracker.mode}")
    op.set("active", "slewing")

    def error():
        if None in (tracker.yaw, tracker.lat, target.lat):
            return None
        want = bearing_deg(tracker.lat, tracker.lon, target.lat, target.lon)
        return abs((tracker.yaw - want + 180) % 360 - 180)

    await wait_until(op, lambda: (error() if error() is not None else 999) < 10,
                     lambda: f"{error():.0f}° off target" if error() is not None else "waiting for attitude")
    op.set("done", "locked on")
    op.progress = "tracking"


TASKS = {"orbit": task_orbit, "patrol": task_patrol, "rtl": task_rtl, "hold": task_hold, "track": task_track}


class TaskManager:
    def __init__(self, registry):
        self.registry = registry
        self.taskings = []
        self.active_by_vehicle = {}

    def submit(self, kind, vehicles, params):
        if kind not in TASKS:
            raise ValueError(f"unknown task {kind}")
        if kind == "orbit" and "center" not in params:
            raise ValueError("orbit needs center [lon, lat]")
        if kind == "patrol" and len(params.get("route", [])) < 2:
            raise ValueError("patrol needs a route of at least 2 points")
        if kind == "track" and params.get("target") not in self.registry:
            raise ValueError("track needs a target vehicle")
        tasking = Tasking(kind, params, self.registry)
        counts = {}
        for v in vehicles:
            tasking.ops.append(Op(tasking, v, counts.get(v.kind, 0)))
            counts[v.kind] = counts.get(v.kind, 0) + 1
        for op in tasking.ops:
            if kind not in KINDS.get(op.vehicle.kind, {}).get("tasks", ()):
                op.set("failed", f"a {op.vehicle.kind} can't {kind}")
                continue
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
