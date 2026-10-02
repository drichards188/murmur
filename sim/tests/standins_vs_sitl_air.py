#!/usr/bin/env python3
"""Authenticity test: the Rust copter and VTOL stand-ins against real ArduPilot SITL.

ArduPilot SITL is the reference (docs/AUTHENTICITY.md: a stand-in may simplify how a vehicle
moves, never how it talks or fails). For each kind, the same GCS script is played at an
`arducopter --model quad` (or `arduplane --model quadplane`) SITL started like sim/swarm.py
starts it (common.parm + kind parm + the platform's params), then at a Rust stand-in of the
same platform. The script is event driven (each step waits for the vehicle, not the clock):
parameter reads, refused commands, arming and auto-disarm, GUIDED takeoff, a mission upload
and AUTO flight, mode changes, RTL to landing, and a takeoff-to-land mission from the ground.

Compared, per step: COMMAND_ACK results, the heartbeat mode/armed sequence, MISSION_ACKs and
the mission request sequence, PARAM_VALUEs, the semantic STATUSTEXTs (arming, mode refusals,
takeoff, transitions, landing, mission), and MAV_TYPE / autopilot. Trajectories are compared
loosely: the step must complete in both (within its timeout) and the duration may differ by
DURATION_FACTOR; final positions after landing within LAND_TOL_M of each other.

    .venv/bin/python sim/tests/standins_vs_sitl_air.py               # copter and vtol
    .venv/bin/python sim/tests/standins_vs_sitl_air.py --kind vtol
    .venv/bin/python sim/tests/standins_vs_sitl_air.py --compare-only

SITL uses instance numbers 50+ (TCP 5760+10*I) and UDP ports 18800+; the stand-in UDP 18850+.
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
import tomllib
from pathlib import Path

from pymavlink.dialects.v20 import ardupilotmega as mavlink

ROOT = Path(__file__).resolve().parents[2]
M = mavlink
ARDUPILOT = Path(os.environ.get("ARDUPILOT_HOME", Path.home() / "ardupilot"))
BIN = ARDUPILOT / "build" / "sitl" / "bin"

KINDS = {
    # kind: (platform in sim/platforms.toml, SITL binary, SITL model, swarm.py vehicle name)
    "copter": ("x10", "arducopter", "quad", "copter"),
    "vtol": ("fvr90", "arduplane", "quadplane", "vtol"),
}
HOME = (37.7950, -122.4650, 20.0, 0.0)  # Presidio, on land
# a step may take this many times longer (or shorter) on one than on the other
DURATION_FACTOR = 2.5
LAND_TOL_M = 25.0
# a mode or armed state counts in the run's sequence when held this long
DWELL_S = 2.5
# STATUSTEXTs that are SITL's own housekeeping (sensors, EKF, the simulator), not autopilot
# semantics: the stand-in does not imitate them. Everything else is compared, with numbers
# masked ("VTOL airbrake v=18.5 d=121" carries measured values).
NOISE = re.compile(r"^(EKF|GPS|SIM |Field Elevation|Barometer|Airspeed|AHRS|ArduPilot Ready|Frame|IMU|RCOut|"
                   r"u-blox|Ardu|QuadPlane initialised|Calibrat|.*calibrat)", re.I)
NUMBER = re.compile(r"-?\d+(\.\d+)?")

# Differences that belong to the shared engine (commands, telemetry, mission upload), not to
# the copter / VTOL behaviors: reported as notes, they don't fail this test.
KNOWN_SHARED = {
    "request": "the shared upload asks with MISSION_REQUEST_INT; ArduPilot 4.8 asks with MISSION_REQUEST",
    "upload text": "the shared upload says 'Mission: N WP'; ArduPilot says 'Flight plan received'",
    "param": "the shared PARAM_REQUEST_READ answers every name (0.0); ArduCopter has no Q_ENABLE and stays silent",
}
UPLOAD_TEXT = re.compile(r"^(Flight plan received|Mission: \d+ WP)$")

REL = M.MAV_FRAME_GLOBAL_RELATIVE_ALT_INT
CUSTOM = M.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED
TELEMETRY = [(M.MAVLINK_MSG_ID_GLOBAL_POSITION_INT, 2.0), (M.MAVLINK_MSG_ID_ATTITUDE, 1.0),
             (M.MAVLINK_MSG_ID_VFR_HUD, 1.0), (M.MAVLINK_MSG_ID_MISSION_CURRENT, 1.0),
             (M.MAVLINK_MSG_ID_SYS_STATUS, 0.5), (M.MAVLINK_MSG_ID_GPS_RAW_INT, 0.5),
             (M.MAVLINK_MSG_ID_EKF_STATUS_REPORT, 0.5)]  # the bridge's telemetry plan (air)


def offset(lat, lon, north_m, east_m):
    return lat + north_m / 111_320.0, lon + east_m / (111_320.0 * math.cos(math.radians(lat)))


def horiz_m(a, b):
    dn = (a[0] - b[0]) * 111_320.0
    de = (a[1] - b[1]) * 111_320.0 * math.cos(math.radians(a[0]))
    return math.hypot(dn, de)


class Gcs:
    """The GCS end of one vehicle's link: logs everything, serves mission uploads, tracks state."""

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

    def now(self):
        return time.monotonic() - self.t0

    def send(self, msg):
        if self.remote is None:
            return
        self.sock.sendto(msg.pack(self.mav), self.remote)

    def pump(self, wait=0.02):
        select.select([self.sock], [], [], wait)
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


def item(cmd, lat=0.0, lon=0.0, alt=0.0, p1=0.0, p2=0.0, p3=0.0, p4=0.0, frame=REL):
    return (cmd, frame, lat, lon, alt, p1, p2, p3, p4)


# ---- the scripts ----------------------------------------------------------------------------
# Each step: (name, action(g, rec) -> outcome, timeout for the step). `rec` notes what to
# compare. Mode numbers are the firmware's own.

COPTER = dict(STABILIZE=0, ALT_HOLD=2, AUTO=3, GUIDED=4, LOITER=5, RTL=6, LAND=9, BRAKE=17)
PLANE = dict(MANUAL=0, AUTO=10, RTL=11, LOITER=12, GUIDED=15, QHOVER=18, QLOITER=19, QLAND=20, QRTL=21)


def copter_script(home):
    lat, lon = home[:2]
    a = offset(lat, lon, 120, 0)
    b = offset(lat, lon, 120, 120)
    c = offset(lat, lon, -60, 60)
    s = []

    def step(name, timeout=5.0):
        def deco(fn):
            s.append((name, fn, timeout))
            return fn
        return deco

    @step("param Q_ENABLE")
    def _(g):
        start = len(g.log)
        g.send(g.mav.param_request_read_encode(g.sysid, 1, b"Q_ENABLE", -1))
        g.wait(lambda: False, 1.5)
        return {"param": [(e[2]["param_id"], e[2]["param_value"]) for e in g.log[start:] if e[1] == "PARAM_VALUE"]}

    @step("unknown mode 99")
    def _(g):
        return {"ack": g.set_mode(99)}

    @step("AUTO without a mission")
    def _(g):
        return {"ack": g.set_mode(COPTER["AUTO"]), "mode": g.wait(lambda: g.mode == COPTER["AUTO"], 1.5)}

    @step("takeoff while disarmed")
    def _(g):
        r = g.set_mode(COPTER["GUIDED"])
        return {"guided": r, "ack": g.command(M.MAV_CMD_NAV_TAKEOFF, 0, 0, 0, 0, 0, 0, 10)}

    @step("arm in STABILIZE, auto-disarm", 25)
    def _(g):
        g.set_mode(COPTER["STABILIZE"])
        r = g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1)
        armed = g.wait(lambda: g.armed, 2)
        t = time.monotonic()
        disarmed = g.wait(lambda: not g.armed, 20)
        return {"ack": r, "armed": armed, "auto_disarm": disarmed, "after_s": round(time.monotonic() - t)}

    @step("arm in RTL refused")
    def _(g):
        m = g.set_mode(COPTER["RTL"])
        return {"rtl": m, "ack": g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1)}

    @step("GUIDED takeoff to 20 m", 40)
    def _(g):
        r = [g.set_mode(COPTER["GUIDED"]), g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1)]
        r.append(g.command(M.MAV_CMD_NAV_TAKEOFF, 0, 0, 0, 0, 0, 0, 20))
        ok = g.wait(lambda: g.pos and g.pos[2] > 19, 35)
        return {"acks": r, "reached": ok}

    @step("AUTO without a mission while flying")
    def _(g):
        return {"ack": g.set_mode(COPTER["AUTO"]), "mode": g.mode}

    @step("takeoff again while flying")
    def _(g):
        return {"ack": g.command(M.MAV_CMD_NAV_TAKEOFF, 0, 0, 0, 0, 0, 0, 30)}

    @step("disarm in flight refused")
    def _(g):
        return {"ack": g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 0), "armed": g.armed}

    @step("upload mission, AUTO", 90)
    def _(g):
        r = g.mission([item(M.MAV_CMD_NAV_WAYPOINT, lat, lon, 0), item(M.MAV_CMD_NAV_TAKEOFF, alt=20),
                       item(M.MAV_CMD_NAV_WAYPOINT, *a, 25), item(M.MAV_CMD_NAV_WAYPOINT, *b, 25)])
        acks = [g.command(M.MAV_CMD_DO_SET_MISSION_CURRENT, 1), g.set_mode(COPTER["AUTO"])]
        done = g.wait(lambda: 3 in g.reached, 80)
        g.wait(lambda: False, 3)  # what it does after the last item
        return {"mission": r, "acks": acks, "reached": list(g.reached), "done": done}

    @step("LOITER, ALT_HOLD, BRAKE")
    def _(g):
        return {"acks": [g.set_mode(COPTER["LOITER"]), g.set_mode(COPTER["ALT_HOLD"]),
                         g.set_mode(COPTER["BRAKE"])]}

    @step("RTL to landing", 120)
    def _(g):
        r = g.set_mode(COPTER["RTL"])
        landed = g.wait(lambda: not g.armed, 115)
        return {"ack": r, "disarmed": landed, "home_m": round(horiz_m(g.pos[:2], (lat, lon))) if g.pos else None}

    @step("mission from the ground: takeoff, waypoint, LAND", 120)
    def _(g):
        r = g.mission([item(M.MAV_CMD_NAV_WAYPOINT, lat, lon, 0), item(M.MAV_CMD_NAV_TAKEOFF, alt=15),
                       item(M.MAV_CMD_NAV_WAYPOINT, *c, 15), item(M.MAV_CMD_NAV_LAND, *c, 0)])
        acks = [g.set_mode(COPTER["LOITER"]), g.command(M.MAV_CMD_DO_SET_MISSION_CURRENT, 1),
                g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1), g.set_mode(COPTER["AUTO"])]
        up = g.wait(lambda: g.armed and g.pos and g.pos[2] > 10, 30)
        landed = g.wait(lambda: not g.armed, 90)
        return {"mission": r, "acks": acks, "climbed": up, "disarmed": landed,
                "land_m": round(horiz_m(g.pos[:2], c)) if g.pos else None}

    @step("LAND mode from a hover", 80)
    def _(g):
        r = [g.set_mode(COPTER["GUIDED"]), g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1),
             g.command(M.MAV_CMD_NAV_TAKEOFF, 0, 0, 0, 0, 0, 0, 10)]
        up = g.wait(lambda: g.pos and g.pos[2] > 9, 30)
        r.append(g.set_mode(COPTER["LAND"]))
        landed = g.wait(lambda: not g.armed, 45)
        return {"acks": r, "climbed": up, "disarmed": landed}

    @step("LOITER with the throttle low: descends, lands, auto-disarms", 70)
    def _(g):
        # no RC transmitter in this sim: SITL's throttle stick sits at the bottom, and the
        # pilot-altitude modes (LOITER, ALT_HOLD, POSHOLD...) read that as "descend"
        r = [g.set_mode(COPTER["GUIDED"]), g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1),
             g.command(M.MAV_CMD_NAV_TAKEOFF, 0, 0, 0, 0, 0, 0, 10)]
        up = g.wait(lambda: g.pos and g.pos[2] > 9, 30)
        r.append(g.set_mode(COPTER["LOITER"]))
        p0 = g.pos
        g.wait(lambda: False, 3)
        sink = round((p0[2] - g.pos[2]) / 3)
        landed = g.wait(lambda: g.pos and g.pos[2] < 0.3, 20)
        t = time.monotonic()
        disarmed = g.wait(lambda: not g.armed, 25)
        return {"acks": r, "climbed": up, "sink_ms": sink, "landed": landed, "disarmed": disarmed,
                "after_s": round(time.monotonic() - t)}

    @step("clear mission, AUTO while disarmed")
    def _(g):
        r = g.mission([])
        return {"mission": r, "ack": g.set_mode(COPTER["AUTO"])}

    return s


def vtol_script(home):
    lat, lon = home[:2]
    a = offset(lat, lon, 1500, 0)
    b = offset(lat, lon, 1500, 1500)
    c = offset(lat, lon, 300, 300)
    s = []

    def step(name, timeout=5.0):
        def deco(fn):
            s.append((name, fn, timeout))
            return fn
        return deco

    @step("param Q_ENABLE")
    def _(g):
        start = len(g.log)
        g.send(g.mav.param_request_read_encode(g.sysid, 1, b"Q_ENABLE", -1))
        g.wait(lambda: False, 1.5)
        return {"param": [(e[2]["param_id"], e[2]["param_value"]) for e in g.log[start:] if e[1] == "PARAM_VALUE"]}

    @step("unknown mode 99")
    def _(g):
        return {"ack": g.set_mode(99)}

    @step("AUTO without a mission")
    def _(g):
        r = g.set_mode(PLANE["AUTO"])
        g.wait(lambda: False, 1.5)
        return {"ack": r, "mode": g.mode}

    @step("arm in RTL refused, arm in AUTO without takeoff refused", 10)
    def _(g):
        r = [g.set_mode(PLANE["RTL"]), g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1)]
        r.append(g.mission([item(M.MAV_CMD_NAV_WAYPOINT, lat, lon, 0), item(M.MAV_CMD_NAV_WAYPOINT, *a, 70)]))
        r += [g.command(M.MAV_CMD_DO_SET_MISSION_CURRENT, 1), g.set_mode(PLANE["AUTO"])]
        g.wait(lambda: False, 1)
        r += [g.mode, g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1), g.set_mode(PLANE["QLOITER"])]
        return {"acks": r}

    @step("takeoff while disarmed")
    def _(g):
        r = g.set_mode(PLANE["GUIDED"])
        return {"guided": r, "ack": g.command(M.MAV_CMD_NAV_TAKEOFF, 0, 0, 0, 0, 0, 0, 10),
                "vtol": g.command(M.MAV_CMD_NAV_VTOL_TAKEOFF, 0, 0, 0, 0, 0, 0, 10)}

    @step("arm in QLOITER, auto-disarm", 25)
    def _(g):
        g.set_mode(PLANE["QLOITER"])
        r = g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1)
        armed = g.wait(lambda: g.armed, 2)
        t = time.monotonic()
        disarmed = g.wait(lambda: not g.armed, 20)
        return {"ack": r, "armed": armed, "auto_disarm": disarmed, "after_s": round(time.monotonic() - t)}

    @step("GUIDED takeoff to 30 m", 50)
    def _(g):
        r = [g.set_mode(PLANE["GUIDED"]), g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1)]
        r.append(g.command(M.MAV_CMD_NAV_TAKEOFF, 0, 0, 0, 0, 0, 0, 30))
        ok = g.wait(lambda: g.pos and g.pos[2] > 28, 45)
        g.wait(lambda: False, 3)
        return {"acks": r, "reached": ok}

    @step("disarm in flight refused")
    def _(g):
        return {"ack": g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 0), "armed": g.armed}

    @step("QLOITER with the throttle low", 10)
    def _(g):
        r = g.set_mode(PLANE["QLOITER"])
        p0 = g.pos
        g.wait(lambda: False, 6)
        return {"ack": r, "sink_ms": round((p0[2] - g.pos[2]) / 6)}

    @step("QLAND from a hover", 60)
    def _(g):
        r = g.set_mode(PLANE["QLAND"])
        landed = g.wait(lambda: not g.armed, 55)
        return {"ack": r, "disarmed": landed}

    @step("mission: VTOL takeoff, cruise, VTOL land", 400)
    def _(g):
        r = g.mission([item(M.MAV_CMD_NAV_WAYPOINT, lat, lon, 0), item(M.MAV_CMD_NAV_VTOL_TAKEOFF, alt=40),
                       item(M.MAV_CMD_NAV_WAYPOINT, *a, 70), item(M.MAV_CMD_NAV_WAYPOINT, *b, 70),
                       item(M.MAV_CMD_NAV_VTOL_LAND, *c, 0)])
        acks = [g.set_mode(PLANE["QLOITER"]), g.command(M.MAV_CMD_DO_SET_MISSION_CURRENT, 1),
                g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1), g.set_mode(PLANE["AUTO"])]
        up = g.wait(lambda: g.armed and g.pos and g.pos[2] > 35, 40)
        wp = g.wait(lambda: 3 in g.reached, 150)
        landed = g.wait(lambda: not g.armed, 200)
        return {"mission": r, "acks": acks, "climbed": up, "reached": list(g.reached), "waypoints": wp,
                "disarmed": landed, "land_m": round(horiz_m(g.pos[:2], c)) if g.pos else None}

    @step("mission: VTOL takeoff, loiter; RTL; QRTL", 400)
    def _(g):
        r = g.mission([item(M.MAV_CMD_NAV_WAYPOINT, lat, lon, 0), item(M.MAV_CMD_NAV_VTOL_TAKEOFF, alt=40),
                       item(M.MAV_CMD_NAV_LOITER_UNLIM, *a, 70, p3=150)])
        acks = [g.command(M.MAV_CMD_DO_SET_MISSION_CURRENT, 1), g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1),
                g.set_mode(PLANE["AUTO"])]
        there = g.wait(lambda: g.pos and horiz_m(g.pos[:2], a) < 400, 120)
        acks.append(g.set_mode(PLANE["RTL"]))
        home_ = g.wait(lambda: g.pos and horiz_m(g.pos[:2], (lat, lon)) < 250, 120)
        acks.append(g.set_mode(PLANE["QRTL"]))
        landed = g.wait(lambda: not g.armed, 120)
        return {"mission": r, "acks": acks, "on_station": there, "rtl_home": home_, "disarmed": landed,
                "home_m": round(horiz_m(g.pos[:2], (lat, lon))) if g.pos else None}

    return s


SCRIPTS = {"copter": copter_script, "vtol": vtol_script}


# ---- running a target -----------------------------------------------------------------------

def platform(kind):
    pid = KINDS[kind][0]
    spec = tomllib.loads((ROOT / "sim" / "platforms.toml").read_text())[pid]
    return pid, spec


def start_sitl(kind, port, instance, workdir):
    pid, spec = platform(kind)
    _, binary, model, vehicle = KINDS[kind]
    defaults = [ROOT / "sim" / "params" / "common.parm"]
    kind_parm = ROOT / "sim" / "params" / f"{vehicle}.parm"
    if kind_parm.exists():
        defaults.append(kind_parm)
    if spec.get("params"):
        extra = workdir / "platform.parm"
        extra.write_text("".join(f"{k} {v}\n" for k, v in spec["params"].items()))
        defaults.append(extra)
    cmd = [str(BIN / binary), "--model", model, "--home", ",".join(str(x) for x in HOME),
           "--instance", str(instance), "--sysid", "1", "--speedup", "1",
           "--serial0", f"udpclient:127.0.0.1:{port}", "--defaults", ",".join(str(d) for d in defaults), "--wipe"]
    return subprocess.Popen(cmd, cwd=workdir, stdout=open(workdir / "sitl.log", "w"), stderr=subprocess.STDOUT,
                            start_new_session=True)


def start_standin(kind, port, workdir):
    pid, spec = platform(kind)
    cfg = workdir / "standins.json"
    v = {"platform": pid, "callsign": f"{spec['callsign']} 1", "uid": 1, "sysid": 1, "port": port,
         "home": list(HOME), "sim": "standin", "name": spec["name"], "spec": spec}
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
    # the EKF must be using GPS (modes that need a position estimate), then pre-arm good
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
    try:
        if not g.wait(lambda: g.hb is not None, 60):
            raise SystemExit(f"{target}: no heartbeat; see {workdir}")
        last_plan = 0.0
        deadline = time.monotonic() + 180
        while not ready(g, target):
            if time.monotonic() - last_plan > 2:
                for msg_id, hz in TELEMETRY:
                    g.send(g.mav.command_long_encode(g.sysid, 1, M.MAV_CMD_SET_MESSAGE_INTERVAL, 0, msg_id,
                                                     1e6 / hz, 0, 0, 0, 0, 0))
                last_plan = time.monotonic()
            if time.monotonic() > deadline:
                raise SystemExit(f"{target}: pre-arm checks never passed; see {workdir}")
            g.pump(0.1)
        for msg_id, hz in TELEMETRY:
            g.send(g.mav.command_long_encode(g.sysid, 1, M.MAV_CMD_SET_MESSAGE_INTERVAL, 0, msg_id, 1e6 / hz,
                                             0, 0, 0, 0, 0))
        g.wait(lambda: False, 5 if target == "sitl" else 1)
        for name, fn, timeout in SCRIPTS[kind](HOME):
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
    return {"steps": steps, "log": g.log}


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


def compare(kind, sitl, rs):
    ok = True
    lines = [f"{kind}: SITL {KINDS[kind][1]} --model {KINDS[kind][2]} vs stand-in ({KINDS[kind][0]})"]

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
    if "MISSION_REQUEST_INT" in extra and "MISSION_REQUEST" in missing:
        extra.discard("MISSION_REQUEST_INT")
        missing.discard("MISSION_REQUEST")
        lines.append(f"  note known (shared): {KNOWN_SHARED['request']}")
    check(not extra, f"message types the stand-in sends that SITL does not: {sorted(extra) or 'none'}")
    lines.append(f"  info SITL-only message types (streams the stand-in does not imitate): {sorted(missing)}")
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
        b["out"] = json.loads(json.dumps(b["out"]))  # tuples as lists, as the SITL log was saved
        same = a["out"] == b["out"]
        shared = a["name"].startswith("param") and a["out"]["param"] == [] and \
            [tuple(p) for p in b["out"]["param"]] == [("Q_ENABLE", 0.0)]
        check(same, f"outcome {a['out']}" + ("" if same else f" vs {b['out']}") +
              (f" (known, shared: {KNOWN_SHARED['param']})" if shared else ""),
              soft=shared or _soft_outcome(a["out"], b["out"]))
        ea, eb = tuple(a["end"]), tuple(b["end"])
        check(ea == eb, f"mode/armed after the step {ea}" + ("" if ea == eb else f" vs {eb}"))
        ta, tb = texts(la), texts(lb)
        # the upload's own text is the shared engine's; compare the rest
        ua = [t for t in ta if UPLOAD_TEXT.match(t.replace("#", "0"))]
        ub = [t for t in tb if UPLOAD_TEXT.match(t.replace("#", "0"))]
        ta = [t for t in ta if t not in ua]
        tb = [t for t in tb if t not in ub]
        check(ta == tb, f"STATUSTEXTs {ta}" + ("" if ta == tb else f" vs {tb}"))
        if ua != ub:
            lines.append(f"  note known (shared): {KNOWN_SHARED['upload text']}")
        (pa, ka), (pb, kb) = mission_proto(la), mission_proto(lb)
        if pa or pb:
            check(pa == pb, f"mission protocol {pa}" + ("" if pa == pb else f" vs {pb}"))
            check(ka == kb, f"mission request message {ka}" + ("" if ka == kb else f" vs {kb}"), soft=True)
        da, db = a["t1"] - a["t0"], b["t1"] - b["t0"]
        if max(da, db) > 10:
            r = max(da, db) / max(min(da, db), 0.1)
            check(r <= DURATION_FACTOR, f"duration {da:.0f} s vs {db:.0f} s (factor {DURATION_FACTOR})", soft=True)
    return ok, lines


# outcome fields that are distances or timings: compared loosely
LOOSE = {"home_m": LAND_TOL_M, "land_m": LAND_TOL_M, "after_s": 3, "sink_ms": 1}


def _soft_outcome(a, b):
    if a.keys() != b.keys():
        return False
    for k in a:
        if k in LOOSE and isinstance(a[k], (int, float)) and isinstance(b[k], (int, float)):
            if abs(a[k] - b[k]) > LOOSE[k]:
                return False
        elif a[k] != b[k]:
            return False
    return True


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--kind", choices=[*KINDS, "all"], default="all")
    ap.add_argument("--target", choices=["sitl", "standin", "both"], default="both")
    ap.add_argument("--instance", type=int, default=50, help="SITL instance (50-59)")
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
                port = (18800 if target == "sitl" else 18850) + i
                logs[target] = run_target(kind, target, port, args.instance + i, workdir / kind / target)
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
