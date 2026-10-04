#!/usr/bin/env python3
"""Authenticity test: the Rust plane, boat and sub stand-ins against real ArduPilot SITL.

ArduPilot SITL is the reference (docs/AUTHENTICITY.md: a stand-in may simplify how a vehicle
moves, never how it talks or fails). For each kind the same GCS script is played at a SITL
started exactly as sim/swarm.py starts it (`arduplane --model plane`, `ardurover --model
motorboat`, `ardusub --model vectored`; merged defaults: common.parm, the kind's parm, the
lost-link timer of the platform's link profile, the platform's catalog params), then at a
Rust stand-in of the same platform (puma, magura, bluerov) with the same lost-link timer.
The script is event driven (each step waits for the vehicle, not the clock): parameter reads
and sets (a refused read-only one, an unknown one), refused modes and commands, arming and
its refusals, a mission upload with DO_ items (DO_CHANGE_SPEED, DO_JUMP, a hold time) and
AUTO, the mission's end, RTL, the hold mode, the GCS-loss failsafe with the link profile's
timer (the GCS heartbeat stops, then comes back), a mission download, and clearing it.

Both get the bridge's telemetry plan (SET_MESSAGE_INTERVAL). Compared: MAV_TYPE / autopilot;
the mode/armed sequence over the run; per step, the outcome field by field (COMMAND_ACK
results, PARAM_VALUEs, what was reached, the mode/armed state the step leaves), MISSION_
REQUEST seqs and MISSION_ACKs, and the STATUSTEXTs with numbers masked (SITL housekeeping
texts aside). Stream rates after SET_MESSAGE_INTERVAL are compared over the run. Trajectories
are compared loosely: each step must complete in both, its duration within DURATION_FACTOR,
distances within LOOSE. Differences that are the model (documented in docs/AUTHENTICITY.md)
are listed in KNOWN and reported as notes.

    .venv/bin/python sim/tests/standins_vs_sitl_plane_boat_sub.py               # all three
    .venv/bin/python sim/tests/standins_vs_sitl_plane_boat_sub.py --kind boat
    .venv/bin/python sim/tests/standins_vs_sitl_plane_boat_sub.py --compare-only

SITL uses instance numbers 50+ (TCP 5760+10*I) and UDP ports 18810+; the stand-in UDP 18860+.
Needs `cargo build --release -p murmur-standins` and the ArduPilot SITL binaries.
Exit status 0 = the stand-in matches, 1 = a difference outside tolerance.
"""

import argparse
import json
import math
import os
import re
import select
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path
from types import SimpleNamespace

from pymavlink.dialects.v20 import ardupilotmega as mavlink

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "sim"))
import swarm  # noqa: E402  (sitl_command: launch SITL exactly as the fleet launcher does)

M = mavlink
LAND = (37.7950, -122.4650, 20.0, 0.0)  # Presidio, on land, runway heading north
WATER = (37.8000, -122.5300, 0.0, 90.0)  # west of the Golden Gate
KINDS = {
    # kind: (platform in sim/platforms.toml, home)
    "plane": ("puma", LAND),
    "boat": ("magura", WATER),
    "sub": ("bluerov", WATER),
}
# a step may take this many times longer (or shorter) on one than on the other
DURATION_FACTOR = 2.5
# a mode or armed state counts in the run's sequence when held this long
DWELL_S = 2.5
# a stream's rate may differ by this fraction (the stand-in sends on 10 Hz physics ticks)
RATE_TOL = 0.35
# STATUSTEXTs that are SITL's own housekeeping (sensors, EKF, the simulator), not autopilot
# semantics: the stand-in does not imitate them. Everything else is compared, with numbers
# masked ("Reached waypoint #2 dist 61m" carries measured values).
NOISE = re.compile(r"^(EKF|GPS|SIM |Field Elevation|Barometer|Airspeed|AHRS|ArduPilot Ready|Frame|IMU|RCOut|"
                   r"u-blox|Ardu|Calibrat|.*calibrat|Compass|Terrain|Fence|Soaring|Battery \d.*(?:failsafe|monitor)|"
                   r"Not using|Running|Config|Lua|Scripting|No AirSpeed|Flight plan|Mission upload|"
                   r"Warning: Parameter|AP_)", re.I)
NUMBER = re.compile(r"-?\d+(\.\d+)?")

REL = M.MAV_FRAME_GLOBAL_RELATIVE_ALT_INT
CUSTOM = M.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED


def telemetry(kind):
    """The bridge's full telemetry plan for this kind (crates/murmur-bridge/src/telemetry.rs)."""
    air = kind == "plane"
    plan = [(M.MAVLINK_MSG_ID_GLOBAL_POSITION_INT, 2.0 if air else 1.0), (M.MAVLINK_MSG_ID_ATTITUDE, 1.0),
            (M.MAVLINK_MSG_ID_VFR_HUD, 1.0), (M.MAVLINK_MSG_ID_MISSION_CURRENT, 1.0),
            (M.MAVLINK_MSG_ID_SYS_STATUS, 0.5), (M.MAVLINK_MSG_ID_GPS_RAW_INT, 0.5),
            (M.MAVLINK_MSG_ID_EKF_STATUS_REPORT, 0.5)]
    if air:
        plan.append((M.MAVLINK_MSG_ID_NAV_CONTROLLER_OUTPUT, 0.5))
    return plan


def offset(lat, lon, north_m, east_m):
    return lat + north_m / 111_320.0, lon + east_m / (111_320.0 * math.cos(math.radians(lat)))


def horiz_m(a, b):
    dn = (a[0] - b[0]) * 111_320.0
    de = (a[1] - b[1]) * 111_320.0 * math.cos(math.radians(a[0]))
    return math.hypot(dn, de)


class Gcs:
    """The GCS end of one vehicle's link: a 1 Hz heartbeat (so the vehicle's GCS failsafe is
    armed), logs everything, serves mission uploads, tracks state."""

    def __init__(self, port, t0):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", port))
        self.sock.setblocking(False)
        self.remote = None
        self.mav = mavlink.MAVLink(None, srcSystem=255, srcComponent=190)
        self.rx = mavlink.MAVLink(None)
        self.rx.robust_parsing = True
        self.t0 = t0
        self.log = []  # (t, type, fields, size, srcsys, srccomp)
        self.upload = None
        self.sysid = 1
        self.hb = None
        self.pos = None  # (lat, lon, rel_alt)
        self.mission_seq = None
        self.reached = []
        self.silent = False  # the GCS "link" is down: no heartbeats
        self.last_hb = -1.0

    def now(self):
        return time.monotonic() - self.t0

    def send(self, msg):
        if self.remote is None:
            return
        self.sock.sendto(msg.pack(self.mav), self.remote)

    def pump(self, wait=0.02):
        select.select([self.sock], [], [], wait)
        if not self.silent and self.now() - self.last_hb >= 1.0:
            self.last_hb = self.now()
            self.send(self.mav.heartbeat_encode(M.MAV_TYPE_GCS, M.MAV_AUTOPILOT_INVALID, 0, 0, 0))
        while True:
            try:
                data, addr = self.sock.recvfrom(4096)
            except BlockingIOError:
                return
            self.remote = addr
            for m in self.rx.parse_buffer(data) or []:
                t = m.get_type()
                if t == "BAD_DATA" or m.get_srcComponent() != 1:
                    continue
                d = m.to_dict()
                d.pop("mavpackettype", None)
                self.log.append((round(self.now(), 3), t, d, len(m.get_msgbuf()), m.get_srcSystem(),
                                 m.get_srcComponent()))
                if t == "HEARTBEAT":
                    self.hb = d
                    self.sysid = m.get_srcSystem()
                elif t == "GLOBAL_POSITION_INT":
                    self.pos = (d["lat"] / 1e7, d["lon"] / 1e7, d["relative_alt"] / 1000.0)
                elif t == "MISSION_CURRENT":
                    self.mission_seq = d["seq"]
                elif t == "MISSION_ITEM_REACHED":
                    self.reached.append(d["seq"])
                elif t in ("MISSION_REQUEST_INT", "MISSION_REQUEST") and self.upload is not None:
                    if d["seq"] < len(self.upload):
                        self.send(self.upload[d["seq"]])
                elif t == "MISSION_ACK":
                    self.upload = None

    @property
    def armed(self):
        return bool(self.hb and self.hb["base_mode"] & M.MAV_MODE_FLAG_SAFETY_ARMED)

    @property
    def mode(self):
        return self.hb["custom_mode"] if self.hb else None

    def wait(self, cond, timeout):
        """Pump until cond() or timeout; returns whether cond held."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.pump()
            if cond():
                return True
        return False

    def sleep(self, seconds):
        self.wait(lambda: False, seconds)

    def command(self, cmd, *p, timeout=3.0):
        """COMMAND_LONG, wait for its ACK; the result number or None."""
        p = list(p) + [0.0] * (7 - len(p))
        start = len(self.log)
        self.send(self.mav.command_long_encode(self.sysid, 1, cmd, 0, *p))
        found = []

        def acked():
            for e in self.log[start:]:
                if e[1] == "COMMAND_ACK" and e[2]["command"] == cmd:
                    found.append(e[2]["result"])
                    return True
            return False

        self.wait(acked, timeout)
        return found[0] if found else None

    def mission(self, items, timeout=10.0):
        """Upload; returns the MISSION_ACK type or None."""
        self.upload = [self.mav.mission_item_int_encode(self.sysid, 1, i, frame, cmd, 0, 1, p1, p2, p3, p4,
                                                        int(lat * 1e7), int(lon * 1e7), alt)
                       for i, (cmd, frame, lat, lon, alt, p1, p2, p3, p4) in enumerate(items)]
        start = len(self.log)
        self.send(self.mav.mission_count_encode(self.sysid, 1, len(items)))
        acks = []

        def done():
            for e in self.log[start:]:
                if e[1] == "MISSION_ACK":
                    acks.append(e[2]["type"])
                    return True
            return False

        self.wait(done, timeout)
        return acks[0] if acks else None

    def set_mode(self, number):
        return self.command(M.MAV_CMD_DO_SET_MODE, CUSTOM, number)

    def params(self, names, timeout=2.0):
        """PARAM_REQUEST_READ each name; {name: [value, type]} and {name: error} as answered."""
        start = len(self.log)
        for n in names:
            self.send(self.mav.param_request_read_encode(self.sysid, 1, n.encode(), -1))
        got, errors = {}, {}

        def all_in():
            for e in self.log[start:]:
                if e[1] == "PARAM_VALUE":
                    got[e[2]["param_id"]] = [round(e[2]["param_value"], 4), e[2]["param_type"]]
                elif e[1] == "PARAM_ERROR":
                    errors[e[2]["param_id"]] = e[2]["error"]
            return all(n in got or n in errors for n in names)

        self.wait(all_in, timeout)
        return {"params": {n: got.get(n) for n in names}, "errors": {n: errors.get(n) for n in names}}

    def param_set(self, name, value, ptype=M.MAV_PARAM_TYPE_REAL32, timeout=2.0):
        """PARAM_SET; the replies about that name (PARAM_VALUE value, PARAM_ERROR error) in
        order. ArduPilot also announces its own periodic saves (BARO*_GND_PRESS, STAT_*)."""
        start = len(self.log)
        self.send(self.mav.param_set_encode(self.sysid, 1, name.encode(), value, ptype))
        self.sleep(timeout)
        return [("value", e[2]["param_id"], round(e[2]["param_value"], 4)) if e[1] == "PARAM_VALUE"
                else ("error", e[2]["param_id"], e[2]["error"])
                for e in self.log[start:] if e[1] in ("PARAM_VALUE", "PARAM_ERROR") and e[2]["param_id"] == name]

    def download(self, timeout=15.0):
        """MISSION_REQUEST_LIST, then every item with MISSION_REQUEST_INT; the items as
        (seq, command, frame, lat, lon, alt, p1..p4) plus the count."""
        start = len(self.log)
        self.send(self.mav.mission_request_list_encode(self.sysid, 1, 0))
        count = []

        def counted():
            for e in self.log[start:]:
                if e[1] == "MISSION_COUNT":
                    count.append(e[2]["count"])
                    return True
            return False

        if not self.wait(counted, 5.0):
            return {"count": None, "items": []}
        items = []
        for seq in range(count[0]):
            n = len(self.log)
            self.send(self.mav.mission_request_int_encode(self.sysid, 1, seq, 0))
            got = []

            def item_in():
                for e in self.log[n:]:
                    if e[1] in ("MISSION_ITEM_INT", "MISSION_ITEM") and e[2]["seq"] == seq:
                        got.append(e)
                        return True
                return False

            if not self.wait(item_in, 3.0):
                items.append(None)
                continue
            d = got[0][2]
            scale = 1e7 if got[0][1] == "MISSION_ITEM_INT" else 1.0
            items.append([d["seq"], d["command"], d["frame"], round(d["x"] / scale, 6), round(d["y"] / scale, 6),
                          round(d["z"], 2), d["param1"], d["param2"], d["param3"], d["param4"]])
        self.send(self.mav.mission_ack_encode(self.sysid, 1, M.MAV_MISSION_ACCEPTED, 0))
        return {"count": count[0], "items": items}

    def gcs_loss(self, seconds):
        """Stop the GCS heartbeat for `seconds`, then resume: (mode, armed) at the end of the
        silence, and what the vehicle said meanwhile (numbers masked)."""
        start = len(self.log)
        self.silent = True
        self.sleep(seconds)
        during = (self.mode, self.armed)
        # (the periodic "PreArm:" reports are compared over the run, see `texts`)
        said = [NUMBER.sub("#", e[2]["text"]) for e in self.log[start:] if e[1] == "STATUSTEXT"
                and not e[2]["text"].startswith("PreArm:")]
        self.silent = False
        self.sleep(5.0)
        after = (self.mode, self.armed)
        return {"during": list(during), "said_during_loss": said, "after": list(after)}


def item(cmd, lat=0.0, lon=0.0, alt=0.0, p1=0.0, p2=0.0, p3=0.0, p4=0.0, frame=REL):
    return (cmd, frame, lat, lon, alt, p1, p2, p3, p4)


# ---- the scripts ----------------------------------------------------------------------------
# Each step: (name, action(g) -> outcome, timeout for the step). Mode numbers are the
# firmware's own. `fs` is the platform's lost-link timer (seconds) from sim/links.toml.

PLANE = dict(MANUAL=0, FBWA=5, AUTO=10, RTL=11, LOITER=12, GUIDED=15, QLOITER=19)
ROVER = dict(MANUAL=0, LEARNING=2, HOLD=4, LOITER=5, AUTO=10, RTL=11, SMART_RTL=12, GUIDED=15)
SUB = dict(STABILIZE=0, ALT_HOLD=2, AUTO=3, GUIDED=4, CIRCLE=7, SURFACE=9, POSHOLD=16, MANUAL=19)

PLANE_PARAMS = ["AIRSPEED_CRUISE", "WP_LOITER_RAD", "RTL_RADIUS", "RTL_ALTITUDE", "WP_RADIUS", "FS_GCS_ENABL",
                "FS_LONG_ACTN", "FS_LONG_TIMEOUT", "Q_ENABLE", "MIS_TOTAL", "NO_SUCH_PARAM"]
BOAT_PARAMS = ["CRUISE_SPEED", "WP_SPEED", "WP_RADIUS", "FS_GCS_ENABLE", "FS_GCS_TIMEOUT", "FS_ACTION",
               "FS_TIMEOUT", "LOIT_RADIUS", "MIS_DONE_BEHAVE", "FRAME_CLASS", "MIS_TOTAL", "NO_SUCH_PARAM"]
SUB_PARAMS = ["WP_SPD", "WP_RADIUS_M", "WP_SPD_UP", "FS_GCS_ENABLE", "FS_GCS_TIMEOUT", "SURFACE_DEPTH",
              "MIS_TOTAL", "NO_SUCH_PARAM"]


def plane_script(home, fs):
    lat, lon = home[:2]
    a = offset(lat, lon, 700, 0)
    b = offset(lat, lon, 700, 700)
    c = offset(lat, lon, 0, 700)
    d = offset(lat, lon, 800, 0)
    s = []

    def step(name, timeout=5.0):
        def deco(fn):
            s.append((name, fn, timeout))
            return fn
        return deco

    @step("params")
    def _(g):
        return g.params(PLANE_PARAMS)

    @step("param set: WP_RADIUS, read-only MIS_TOTAL, unknown", 10)
    def _(g):
        r = {"set": g.param_set("WP_RADIUS", 60.0), "readonly": g.param_set("MIS_TOTAL", 7.0),
             "unknown": g.param_set("NO_SUCH_PARAM", 1.0)}
        r["back"] = g.params(["WP_RADIUS"])["params"]
        return r

    @step("unknown mode 99")
    def _(g):
        return {"ack": g.set_mode(99)}

    @step("Q mode on a plain plane (QLOITER)")
    def _(g):
        return {"ack": g.set_mode(PLANE["QLOITER"])}

    @step("AUTO without a mission")
    def _(g):
        r = g.set_mode(PLANE["AUTO"])
        g.sleep(2.0)
        return {"ack": r, "mode": g.mode}

    @step("arm in RTL refused")
    def _(g):
        m = g.set_mode(PLANE["RTL"])
        return {"rtl": m, "ack": g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1), "armed": g.armed}

    @step("takeoff command in GUIDED, disarmed")
    def _(g):
        r = g.set_mode(PLANE["GUIDED"])
        return {"guided": r, "ack": g.command(M.MAV_CMD_NAV_TAKEOFF, 15, 0, 0, 0, 0, 0, 30)}

    @step("arm in FBWA on the ground: stays armed, disarm", 25)
    def _(g):
        r = [g.set_mode(PLANE["FBWA"]), g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1)]
        armed = g.wait(lambda: g.armed, 2)
        g.sleep(12.0)
        still = g.armed
        r.append(g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 0))
        return {"acks": r, "armed": armed, "still_armed_12s": still, "disarmed": g.wait(lambda: not g.armed, 2)}

    @step("mission: takeoff, waypoints, change speed, loiter; AUTO; arm", 240)
    def _(g):
        r = g.mission([item(M.MAV_CMD_NAV_WAYPOINT, lat, lon, 0), item(M.MAV_CMD_NAV_TAKEOFF, alt=50, p1=15),
                       item(M.MAV_CMD_NAV_WAYPOINT, *a, 80), item(M.MAV_CMD_DO_CHANGE_SPEED, p1=0, p2=16),
                       item(M.MAV_CMD_NAV_WAYPOINT, *b, 80), item(M.MAV_CMD_NAV_LOITER_UNLIM, *c, 80)])
        acks = [g.set_mode(PLANE["AUTO"]), g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1)]
        up = g.wait(lambda: g.armed and g.pos and g.pos[2] > 45, 60)
        wp = g.wait(lambda: 4 in g.reached, 150)
        near = g.wait(lambda: g.pos and horiz_m(g.pos[:2], c) < 350, 90)
        g.sleep(10.0)
        return {"mission": r, "acks": acks, "climbed": up, "reached": list(g.reached), "waypoints": wp,
                "on_station": near, "max_alt": round(max(e[2]["relative_alt"] / 1000 for e in g.log
                                                         if e[1] == "GLOBAL_POSITION_INT") / 10) * 10}

    @step("LOITER (hold)", 10)
    def _(g):
        r = g.set_mode(PLANE["LOITER"])
        g.sleep(5.0)
        return {"ack": r}

    @step("GCS loss in LOITER: failsafe RTL, then cleared", fs + 20)
    def _(g):
        return g.gcs_loss(fs + 5.0)

    @step("RTL home", 150)
    def _(g):
        r = g.set_mode(PLANE["RTL"])
        near = g.wait(lambda: g.pos and horiz_m(g.pos[:2], (lat, lon)) < 400, 120)
        g.sleep(15.0)
        return {"ack": r, "rtl_home": near, "home_m": round(horiz_m(g.pos[:2], (lat, lon)) / 50) * 50 if g.pos else None}

    @step("mission: approach and land; auto-disarm", 330)
    def _(g):
        r = g.mission([item(M.MAV_CMD_NAV_WAYPOINT, lat, lon, 0), item(M.MAV_CMD_NAV_WAYPOINT, *d, 60),
                       item(M.MAV_CMD_NAV_LAND, lat, lon, 0)])
        acks = [g.command(M.MAV_CMD_DO_SET_MISSION_CURRENT, 1), g.set_mode(PLANE["AUTO"])]
        reached = g.wait(lambda: 1 in g.reached[-3:], 120)
        down = g.wait(lambda: g.pos and g.pos[2] < 1.0, 110)
        t = time.monotonic()
        # ArduPlane disarms LAND_DISARMDELAY after it stops believing it is flying (a ~25 s decay)
        landed = g.wait(lambda: not g.armed, 90)
        return {"mission": r, "acks": acks, "approach_wp": reached, "touched_down": down, "disarmed": landed,
                "disarm_after_s": round((time.monotonic() - t) / 10) * 10 if landed else None,
                "reached": list(g.reached), "land_m": round(horiz_m(g.pos[:2], (lat, lon))) if g.pos else None}

    @step("MANUAL after the landing; GCS loss on the ground", fs + 20)
    def _(g):
        r = g.set_mode(PLANE["MANUAL"])
        g.sleep(1.0)
        out = g.gcs_loss(fs + 5.0)
        out["manual"] = r
        return out

    @step("mission download", 20)
    def _(g):
        return g.download()

    @step("clear mission, AUTO while disarmed")
    def _(g):
        r = g.mission([])
        a = g.set_mode(PLANE["AUTO"])
        g.sleep(2.0)
        return {"mission": r, "ack": a, "mode": g.mode}

    return s


def boat_script(home, fs):
    lat, lon = home[:2]
    p1 = offset(lat, lon, 0, 150)
    p2 = offset(lat, lon, 150, 150)
    p3 = offset(lat, lon, 150, 0)
    s = []

    def step(name, timeout=5.0):
        def deco(fn):
            s.append((name, fn, timeout))
            return fn
        return deco

    @step("params")
    def _(g):
        return g.params(BOAT_PARAMS)

    @step("param set: WP_RADIUS, read-only MIS_TOTAL, unknown", 10)
    def _(g):
        r = {"set": g.param_set("WP_RADIUS", 12.0), "readonly": g.param_set("MIS_TOTAL", 7.0),
             "unknown": g.param_set("NO_SUCH_PARAM", 1.0)}
        r["back"] = g.params(["WP_RADIUS"])["params"]
        return r

    @step("unknown modes 99 and 2 (LEARNING)")
    def _(g):
        return {"acks": [g.set_mode(99), g.set_mode(ROVER["LEARNING"])]}

    @step("AUTO without a mission")
    def _(g):
        r = g.set_mode(ROVER["AUTO"])
        g.sleep(1.5)
        return {"ack": r, "mode": g.mode}

    @step("RTL disarmed, arm refused, HOLD")
    def _(g):
        r = [g.set_mode(ROVER["RTL"]), g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1)]
        armed = g.armed
        r.append(g.set_mode(ROVER["HOLD"]))
        return {"acks": r, "armed": armed}

    @step("arm in HOLD; takeoff command")
    def _(g):
        r = [g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1)]
        armed = g.wait(lambda: g.armed, 2)
        r.append(g.command(M.MAV_CMD_NAV_TAKEOFF))
        return {"acks": r, "armed": armed}

    @step("mission: waypoints, change speed, hold time, DO_JUMP; AUTO", 300)
    def _(g):
        r = g.mission([item(M.MAV_CMD_NAV_WAYPOINT, lat, lon, 0), item(M.MAV_CMD_NAV_WAYPOINT, *p1, 0),
                       item(M.MAV_CMD_DO_CHANGE_SPEED, p1=1, p2=6), item(M.MAV_CMD_NAV_WAYPOINT, *p2, 0),
                       item(M.MAV_CMD_NAV_WAYPOINT, *p3, 0, p1=5), item(M.MAV_CMD_DO_JUMP, p1=1, p2=1)])
        acks = [g.set_mode(ROVER["AUTO"])]
        done = g.wait(lambda: any(e[1] == "STATUSTEXT" and e[2]["text"] == "Mission Complete" for e in g.log), 280)
        g.sleep(8.0)
        spd = [math.hypot(e[2]["vx"], e[2]["vy"]) / 100 for e in g.log if e[1] == "GLOBAL_POSITION_INT"]
        return {"mission": r, "acks": acks, "reached": list(g.reached), "done": done, "mode": g.mode,
                "max_speed": round(max(spd)), "end_m": round(horiz_m(g.pos[:2], p3) / 10) * 10 if g.pos else None}

    @step("LOITER, then HOLD", 15)
    def _(g):
        r = [g.set_mode(ROVER["LOITER"])]
        g.sleep(4.0)
        r.append(g.set_mode(ROVER["HOLD"]))
        g.sleep(4.0)
        return {"acks": r}

    @step("RTL home", 90)
    def _(g):
        r = g.set_mode(ROVER["RTL"])
        there = g.wait(lambda: any(e[1] == "STATUSTEXT" and e[2]["text"] == "Reached destination" for e in g.log), 80)
        g.sleep(5.0)
        return {"ack": r, "reached_destination": there, "home_m": round(horiz_m(g.pos[:2], (lat, lon))) if g.pos else None}

    @step("HOLD at home, then RTL again from there", 15)
    def _(g):
        r = [g.set_mode(ROVER["HOLD"])]
        g.sleep(3.0)
        r.append(g.set_mode(ROVER["RTL"]))
        g.sleep(8.0)
        return {"acks": r}

    @step("GCS loss in LOITER: failsafe, then cleared", fs + 25)
    def _(g):
        r = g.set_mode(ROVER["LOITER"])
        out = g.gcs_loss(fs + 8.0)
        out["loiter"] = r
        return out

    @step("disarm; GCS loss disarmed in MANUAL", fs + 25)
    def _(g):
        r = [g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 0), g.set_mode(ROVER["MANUAL"])]
        out = g.gcs_loss(fs + 8.0)
        out["acks"] = r
        return out

    @step("mission download", 20)
    def _(g):
        return g.download()

    @step("clear mission, AUTO while disarmed")
    def _(g):
        r = g.mission([])
        a = g.set_mode(ROVER["AUTO"])
        g.sleep(1.5)
        return {"mission": r, "ack": a, "mode": g.mode}

    return s


def sub_script(home, fs):
    lat, lon = home[:2]
    s1 = offset(lat, lon, 0, 60)
    s2 = offset(lat, lon, 60, 60)
    s = []

    def step(name, timeout=5.0):
        def deco(fn):
            s.append((name, fn, timeout))
            return fn
        return deco

    @step("params")
    def _(g):
        return g.params(SUB_PARAMS)

    @step("param set: WP_RADIUS_M, read-only MIS_TOTAL, unknown", 10)
    def _(g):
        r = {"set": g.param_set("WP_RADIUS_M", 3.0), "readonly": g.param_set("MIS_TOTAL", 7.0),
             "unknown": g.param_set("NO_SUCH_PARAM", 1.0)}
        r["back"] = g.params(["WP_RADIUS_M"])["params"]
        return r

    @step("unknown modes 99 and 11 (no RTL in ArduSub)")
    def _(g):
        return {"acks": [g.set_mode(99), g.set_mode(11)]}

    @step("AUTO without a mission")
    def _(g):
        r = g.set_mode(SUB["AUTO"])
        g.sleep(1.5)
        return {"ack": r, "mode": g.mode}

    @step("arm in MANUAL; ALT_HOLD")
    def _(g):
        r = [g.set_mode(SUB["MANUAL"]), g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1)]
        armed = g.wait(lambda: g.armed, 2)
        r.append(g.set_mode(SUB["ALT_HOLD"]))
        g.sleep(2.0)
        return {"acks": r, "armed": armed}

    @step("RTL and takeoff commands; POSHOLD, GUIDED at the surface", 12)
    def _(g):
        r = [g.command(M.MAV_CMD_NAV_RETURN_TO_LAUNCH), g.command(M.MAV_CMD_NAV_TAKEOFF),
             g.set_mode(SUB["POSHOLD"])]
        g.sleep(3.0)
        r.append(g.set_mode(SUB["GUIDED"]))
        g.sleep(3.0)
        return {"acks": r}

    @step("mission at 5 m depth; AUTO; mission end", 300)
    def _(g):
        r = g.mission([item(M.MAV_CMD_NAV_WAYPOINT, lat, lon, 0), item(M.MAV_CMD_NAV_WAYPOINT, *s1, -5),
                       item(M.MAV_CMD_NAV_WAYPOINT, *s2, -5)])
        acks = [g.set_mode(SUB["AUTO"])]
        done = g.wait(lambda: 2 in g.reached, 270)
        g.sleep(10.0)
        depth = max(-e[2]["relative_alt"] / 1000 for e in g.log if e[1] == "GLOBAL_POSITION_INT")
        return {"mission": r, "acks": acks, "reached": list(g.reached), "done": done, "mode": g.mode,
                "max_depth_m": round(depth), "end_m": round(horiz_m(g.pos[:2], s2) / 10) * 10 if g.pos else None}

    @step("SURFACE: ascends, then ALT_HOLD at the surface", 90)
    def _(g):
        r = g.set_mode(SUB["SURFACE"])
        up = g.wait(lambda: g.mode == SUB["ALT_HOLD"], 80)
        g.sleep(3.0)
        return {"ack": r, "alt_hold_at_surface": up, "depth_m": round(-g.pos[2]) if g.pos else None}

    @step("GCS loss armed at the surface: failsafe, then cleared", fs + 25)
    def _(g):
        return g.gcs_loss(fs + 6.0)

    @step("mission download", 20)
    def _(g):
        return g.download()

    @step("clear mission, AUTO")
    def _(g):
        r = g.mission([])
        a = g.set_mode(SUB["AUTO"])
        g.sleep(1.5)
        return {"mission": r, "ack": a, "mode": g.mode}

    return s


SCRIPTS = {"plane": plane_script, "boat": boat_script, "sub": sub_script}

# Differences that are the model, not a bug (docs/AUTHENTICITY.md): (kind, step) -> {outcome
# field: why}. They are reported as notes. The field is the flattened key ("params.X").
KNOWN = {
    ("plane", "params"): {
        "params.RTL_ALTITUDE": "Murmur's choice (crates/murmur-standins/src/params.rs): -1 returns at the current "
                               "altitude, so a HALE/MALE stand-in does not descend to ArduPlane's 100 m default",
    },
}


# ---- running a target -----------------------------------------------------------------------

def platform(kind):
    pid = KINDS[kind][0]
    return pid, swarm.load_platforms()[pid]


def lost_link_s(kind):
    pid, spec = platform(kind)
    firmware = swarm.STANDIN_FIRMWARE[spec["behavior"]]
    return float(next(iter(swarm.lost_link_params(pid, spec, firmware).values()), 5.0))


def start_sitl(kind, port, instance, workdir):
    pid, spec = platform(kind)
    v = {"platform": pid, "home": list(KINDS[kind][1]), "instance": instance, "sysid": 1, "port": port}
    args = SimpleNamespace(speedup=1.0, bridge_host="127.0.0.1", keep_eeprom=False)
    cmd = swarm.sitl_command(v, spec, args, workdir)
    return subprocess.Popen(cmd, cwd=workdir, stdout=open(workdir / "sitl.log", "w"), stderr=subprocess.STDOUT,
                            start_new_session=True)


def start_standin(kind, port, workdir):
    pid, spec = platform(kind)
    cfg = workdir / "standins.json"
    params = swarm.lost_link_params(pid, spec, swarm.STANDIN_FIRMWARE[spec["behavior"]])
    v = {"platform": pid, "callsign": f"{spec['callsign']} 1", "uid": 1, "sysid": 1, "port": port,
         "home": list(KINDS[kind][1]), "sim": "standin", "name": spec["name"], "spec": spec, "params": params}
    cfg.write_text(json.dumps({"bridge_host": "127.0.0.1", "vehicles": [v]}))
    binary = ROOT / "target" / "release" / "murmur-standins"
    env = {k: val for k, val in os.environ.items() if not k.startswith("MURMUR_")}
    env["MURMUR_STANDINS_STATS"] = "0"
    return subprocess.Popen([str(binary), str(cfg)], stdout=open(workdir / "standin.log", "w"),
                            stderr=subprocess.STDOUT, env=env, start_new_session=True)


def ready(g, target):
    """SITL: wait for the pre-arm checks to pass (EKF, GPS). A stand-in is ready at once."""
    if target == "standin":
        return True
    if not any(e[1] == "STATUSTEXT" and "is using GPS" in e[2]["text"] for e in g.log):
        return False
    prearm = M.MAV_SYS_STATUS_PREARM_CHECK
    return any(e[1] == "SYS_STATUS" and e[2]["onboard_control_sensors_health"] & prearm
               for e in g.log[-10:])


def run_target(kind, target, port, instance, workdir):
    workdir.mkdir(parents=True, exist_ok=True)
    t0 = time.monotonic()
    g = Gcs(port, t0)
    proc = start_sitl(kind, port, instance, workdir) if target == "sitl" else start_standin(kind, port, workdir)
    steps = []
    plan = telemetry(kind)
    try:
        if not g.wait(lambda: g.hb is not None, 60):
            raise SystemExit(f"{target}: no heartbeat; see {workdir}")
        last_plan = 0.0
        deadline = time.monotonic() + 180
        while not ready(g, target):
            if time.monotonic() - last_plan > 2:
                for msg_id, hz in plan:
                    g.send(g.mav.command_long_encode(g.sysid, 1, M.MAV_CMD_SET_MESSAGE_INTERVAL, 0, msg_id,
                                                     1e6 / hz, 0, 0, 0, 0, 0))
                last_plan = time.monotonic()
            if time.monotonic() > deadline:
                raise SystemExit(f"{target}: pre-arm checks never passed; see {workdir}")
            g.pump(0.1)
        for msg_id, hz in plan:
            g.send(g.mav.command_long_encode(g.sysid, 1, M.MAV_CMD_SET_MESSAGE_INTERVAL, 0, msg_id, 1e6 / hz,
                                             0, 0, 0, 0, 0))
        plan_t = g.now()
        g.sleep(5 if target == "sitl" else 2)
        for name, fn, timeout in SCRIPTS[kind](KINDS[kind][1], lost_link_s(kind)):
            start_t, start_i = g.now(), len(g.log)
            out = fn(g)
            end_t = g.now()
            # the state the step left: the next heartbeat (ArduPilot sends one on a mode change,
            # the stand-in at its 1 Hz)
            n = len(g.log)
            g.wait(lambda: any(e[1] == "HEARTBEAT" for e in g.log[n:]), 1.5)
            end = (g.mode, g.armed)
            said = [e[2]["text"] for e in g.log[start_i:] if e[1] == "STATUSTEXT"]
            print(f"  {target:7} {name}: {out}\n          said {said}", flush=True)
            steps.append({"name": name, "out": out, "t0": start_t, "t1": end_t, "i0": start_i, "i1": len(g.log),
                          "end": end})
    finally:
        try:
            os.killpg(proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        proc.wait(10)
        g.sock.close()
    return {"steps": steps, "log": g.log, "plan_t": plan_t, "plan": plan}


# ---- comparison -----------------------------------------------------------------------------

def mode_seq(log, dwell=DWELL_S):
    """(mode, armed) as the heartbeats tell it, dropping states held under `dwell` seconds
    (ArduPilot sends a heartbeat at once on a mode change, the stand-in at its 1 Hz)."""
    runs = []  # [state, first t, last t]
    for e in log:
        if e[1] == "HEARTBEAT":
            m = (e[2]["custom_mode"], bool(e[2]["base_mode"] & M.MAV_MODE_FLAG_SAFETY_ARMED))
            if runs and runs[-1][0] == m:
                runs[-1][2] = e[0]
            else:
                runs.append([m, e[0], e[0]])
    out = []
    for i, (m, t0, _) in enumerate(runs):
        t1 = runs[i + 1][1] if i + 1 < len(runs) else math.inf
        if t1 - t0 >= dwell and (not out or out[-1] != m):
            out.append(m)
    return out


def texts(log, masked=True):
    """Semantic STATUSTEXTs; the periodic "PreArm:" reports are compared over the run instead
    (they come every 30 s from boot, so which step they land in is a matter of timing)."""
    out = [e[2]["text"] for e in log if e[1] == "STATUSTEXT" and not NOISE.match(e[2]["text"])
           and not e[2]["text"].startswith("PreArm:")]
    return [NUMBER.sub("#", t) for t in out] if masked else out


def mission_proto(log):
    """Request seqs and ACK results; which request message (REQUEST or REQUEST_INT) is apart."""
    seq = [("ack", e[2]["type"]) if e[1] == "MISSION_ACK" else ("req", e[2]["seq"]) for e in log
           if e[1] in ("MISSION_REQUEST_INT", "MISSION_REQUEST", "MISSION_ACK")]
    kinds = sorted({e[1] for e in log if e[1] in ("MISSION_REQUEST_INT", "MISSION_REQUEST")})
    return seq, kinds


def flatten(d, prefix=""):
    """Nested dicts as {"a.b": value}; lists stay whole values."""
    out = {}
    for k, v in d.items():
        key = f"{prefix}{k}"
        if isinstance(v, dict):
            out.update(flatten(v, key + "."))
        else:
            out[key] = v
    return out


def rates(run):
    """Messages per second by type after the telemetry plan was set."""
    log = [e for e in run["log"] if e[0] >= run["plan_t"]]
    span = max(log[-1][0] - run["plan_t"], 1.0) if log else 1.0
    counts = {}
    for e in log:
        counts[e[1]] = counts.get(e[1], 0) + 1
    return {t: n / span for t, n in counts.items()}


def compare(kind, sitl, rs):
    ok = True
    pid, spec = platform(kind)
    lines = [f"{kind}: SITL {swarm.VEHICLES[spec['vehicle']][0]} --model {swarm.VEHICLES[spec['vehicle']][1]} "
             f"vs stand-in ({pid}, behavior {spec['behavior']})"]

    def check(cond, what, soft=False):
        nonlocal ok
        if not cond and not soft:
            ok = False
        tag = "  ok   " if cond else ("  note " if soft else "  FAIL ")
        lines.append(tag + what)

    ident = lambda log: sorted({(e[2]["type"], e[2]["autopilot"]) for e in log if e[1] == "HEARTBEAT"})  # noqa: E731
    check(ident(sitl["log"]) == ident(rs["log"]), f"MAV_TYPE/autopilot {ident(sitl['log'])} vs {ident(rs['log'])}")
    types = lambda log: {e[1] for e in log}  # noqa: E731
    extra, missing = types(rs["log"]) - types(sitl["log"]), types(sitl["log"]) - types(rs["log"])
    check(not extra, f"message types the stand-in sends that SITL does not: {sorted(extra) or 'none'}")
    lines.append(f"  info SITL-only message types (streams the stand-in does not imitate): {sorted(missing)}")
    ra, rb = rates(sitl), rates(rs)
    names = {M.mavlink_map[i].name: hz for i, hz in sitl["plan"]}
    names["HEARTBEAT"] = 1.0
    for name, hz in names.items():
        a, b = ra.get(name, 0.0), rb.get(name, 0.0)
        close = abs(a - b) <= RATE_TOL * max(a, 1e-9)
        check(close, f"rate {name}: asked {hz:.1f} Hz, SITL {a:.2f} Hz, stand-in {b:.2f} Hz")
    ma, mb = mode_seq(sitl["log"]), mode_seq(rs["log"])
    check(ma == mb, f"mode/armed sequence over the run {ma}" + ("" if ma == mb else f" vs {mb}"))
    prearm = lambda log: sorted({e[2]["text"] for e in log if e[1] == "STATUSTEXT"  # noqa: E731
                                 and e[2]["text"].startswith("PreArm:")})
    pa, pb = prearm(sitl["log"]), prearm(rs["log"])
    check(pa == pb, f"periodic pre-arm reports {pa}" + ("" if pa == pb else f" vs {pb}"), soft=True)
    theirs = {b["name"]: b for b in rs["steps"]}
    for a in sitl["steps"]:
        lines.append(f" step: {a['name']}")
        b = theirs.get(a["name"])
        if b is None:
            check(False, "step missing from the stand-in run")
            continue
        la, lb = sitl["log"][a["i0"]:a["i1"]], rs["log"][b["i0"]:b["i1"]]
        # tuples as lists, as a saved log has them
        fa, fb = flatten(json.loads(json.dumps(a["out"]))), flatten(json.loads(json.dumps(b["out"])))
        known = KNOWN.get((kind, a["name"]), {})
        for key in sorted(set(fa) | set(fb)):
            va, vb = fa.get(key), fb.get(key)
            if va == vb:
                continue
            loose = LOOSE.get(key.rsplit(".", 1)[-1])
            if loose is not None and isinstance(va, (int, float)) and isinstance(vb, (int, float)):
                check(abs(va - vb) <= loose, f"outcome {key}: {va} vs {vb} (within {loose})")
            elif key == "items" and isinstance(va, list) and isinstance(vb, list) and va and vb:
                # a downloaded mission: item 0 is home as each autopilot last set it, where the
                # vehicle armed or came to rest disarmed (SITL's GPS puts it a few metres off,
                # its motorboat drifts on a simulated tide before arming, a landing ends
                # metres from the point, its altitude is its own); the rest must be what was
                # uploaded
                h = horiz_m(va[0][3:5], vb[0][3:5]) if va[0] and vb[0] else 1e9
                check(h <= 15.0 and va[1:] == vb[1:], f"outcome items: home {h:.1f} m apart, then {va[1:]}"
                      + ("" if va[1:] == vb[1:] else f" vs {vb[1:]}"))
            elif key in known:
                check(False, f"outcome {key}: {va} vs {vb}: {known[key]}", soft=True)
            else:
                check(False, f"outcome {key}: {va} vs {vb}")
        same = fa == fb
        if same:
            lines.append(f"  ok   outcome {a['out']}")
        ea, eb = tuple(a["end"]), tuple(b["end"])
        check(ea == eb, f"mode/armed after the step {ea}" + ("" if ea == eb else f" vs {eb}"))
        ta, tb = texts(la), texts(lb)
        if ta == tb or sorted(ta) != sorted(tb):
            check(ta == tb, f"STATUSTEXTs {ta}" + ("" if ta == tb else f" vs {tb}"))
        else:
            # the same texts; ArduPilot itself orders texts from one tick differently run to run
            check(False, f"STATUSTEXTs {ta} vs {tb} (same texts, order differs)", soft=True)
        (pa, ka), (pb, kb) = mission_proto(la), mission_proto(lb)
        if pa or pb:
            check(pa == pb, f"mission protocol {pa}" + ("" if pa == pb else f" vs {pb}"))
            check(ka == kb, f"mission request message {ka}" + ("" if ka == kb else f" vs {kb}"), soft=True)
        da, db = a["t1"] - a["t0"], b["t1"] - b["t0"]
        if max(da, db) > 10:
            r = max(da, db) / max(min(da, db), 0.1)
            check(r <= DURATION_FACTOR, f"duration {da:.0f} s vs {db:.0f} s (factor {DURATION_FACTOR})", soft=True)
    return ok, lines


# outcome fields that are distances or timings: compared loosely (by their last key)
LOOSE = {"home_m": 25.0, "land_m": 40.0, "end_m": 20.0, "max_alt": 20.0, "max_depth_m": 2.0, "max_speed": 3.0,
         "depth_m": 1.0}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--kind", choices=[*KINDS, "all"], default="all")
    ap.add_argument("--target", choices=["sitl", "standin", "both"], default="both")
    ap.add_argument("--instance", type=int, default=50, help="first SITL instance (50-79)")
    ap.add_argument("--workdir", default=str(ROOT / "sim" / "run" / "vs-sitl"))
    ap.add_argument("--compare-only", action="store_true", help="re-compare the logs of the last run")
    args = ap.parse_args()
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))  # still stop SITL (finally blocks)
    kinds = list(KINDS) if args.kind == "all" else [args.kind]
    workdir = Path(args.workdir)
    all_ok = True
    for kind in kinds:
        i = list(KINDS).index(kind)  # fixed ports and instance per kind, so kinds can run at once
        logs = {}
        for target in ("sitl", "standin"):
            path = workdir / kind / f"{target}.json"
            if not args.compare_only and args.target in (target, "both"):
                print(f"running {kind} on {target} ...", flush=True)
                port = (18810 if target == "sitl" else 18860) + i
                logs[target] = run_target(kind, target, port, args.instance + i, workdir / kind / target)
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(json.dumps(logs[target]))
            elif path.exists():
                logs[target] = json.loads(path.read_text())
        if len(logs) == 2:
            ok, lines = compare(kind, logs["sitl"], logs["standin"])
            print("\n".join(lines))
            all_ok &= ok
    print("MATCH" if all_ok else "DIFFERENCES (see above)")
    sys.exit(0 if all_ok else 1)


if __name__ == "__main__":
    main()
