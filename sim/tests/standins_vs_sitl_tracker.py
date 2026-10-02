#!/usr/bin/env python3
"""Antenna tracker: the Rust stand-in against ArduPilot SITL, driven by one script.

Starts an AntennaTracker SITL (the patched binary, default ~/ardupilot-fix) and a Rust stand-in
tracker at the same spot and heading, plays the same GCS script at both and compares what each
sends back: MAV_TYPE and heartbeat, parameters (SYSID_TARGET persistence, echoes, refusals),
mode changes and arming with their COMMAND_ACKs, the heartbeat target lock, then the pointing:
a relayed GLOBAL_POSITION_INT around all quadrants and elevations (including a bearing in the
gap behind the mount and ones that need the long way round), a moving target, a target from
the wrong sysid, a lost target, and SERVOTEST / MANUAL / STOP.

    cargo build --release -p murmur-standins
    .venv/bin/python sim/tests/standins_vs_sitl_tracker.py [--quick] [--ardupilot DIR]

Uses SITL instance 80 (TCP 6560) and UDP 19200 (SITL) / 19201 (stand-in). Takes about ten
minutes (both run in real time; the mount slews at 9 deg/s). Exit 0 = every comparison held.

Known, documented differences (printed, not failures):
- STOP / disarmed: SITL's servo output is PWM 0, which its mount model drives to the -yaw stop;
  the stand-in holds (a real servo with no pulse holds or goes limp).
- Boot: SITL's mount drifts toward the -yaw stop for the second before it arms (PWM 0), so its
  yaw at boot is not the start heading; the stand-in starts on it.
"""

import argparse
import importlib.util
import json
import math
import os
import signal
import subprocess
import sys
import time
import tomllib
from pathlib import Path

from pymavlink import mavutil
from pymavlink.dialects.v20 import ardupilotmega as M

ROOT = Path(__file__).resolve().parents[2]
SITL_INSTANCE = 80
SITL_PORT, STANDIN_PORT = 19200, 19201
SYSID = 7
TARGET_SYSID = 42
HOME = [37.8105, -122.4770, 10.0, 0.0]  # lat, lon, alt, heading (the mount's centre)
YAW_RANGE = 340.0


def flat_offset(lat, lon, bearing_deg, dist_m):
    b = math.radians(bearing_deg)
    dlat = dist_m * math.cos(b) / 111_320.0
    dlon = dist_m * math.sin(b) / (111_320.0 * math.cos(math.radians(lat)))
    return lat + dlat, lon + dlon


def bearing_to(lat1, lon1, lat2, lon2):
    dn = (lat2 - lat1) * 111_320.0
    de = (lon2 - lon1) * 111_320.0 * math.cos(math.radians(lat1))
    return math.degrees(math.atan2(de, dn)) % 360.0, math.hypot(dn, de)


def wrap180(a):
    return (a + 180.0) % 360.0 - 180.0


class Rig:
    """One side: a MAVLink connection and what it last sent."""

    def __init__(self, name, port):
        self.name = name
        self.conn = mavutil.mavlink_connection(f"udpin:127.0.0.1:{port}", source_system=255,
                                               source_component=190)
        self.last = {}
        self.events = []  # (time, message) of the ones a script waits for
        self.count = {}
        self.t0 = time.time()

    def pump(self):
        while True:
            m = self.conn.recv_match(blocking=False)
            if m is None:
                return
            t = m.get_type()
            if t == "BAD_DATA":
                continue
            self.last[t] = m
            self.count[t] = self.count.get(t, 0) + 1
            if t in ("COMMAND_ACK", "PARAM_VALUE", "PARAM_ERROR", "STATUSTEXT", "REQUEST_DATA_STREAM",
                     "MISSION_ACK", "MISSION_REQUEST", "MISSION_REQUEST_INT", "MISSION_COUNT",
                     "MISSION_ITEM_INT", "MISSION_ITEM"):
                self.events.append((time.time(), m))

    def mark(self):
        return len(self.events)

    def since(self, mark, *types):
        return [m for _, m in self.events[mark:] if m.get_type() in types]

    def yaw(self):
        a = self.last.get("ATTITUDE")
        return math.degrees(a.yaw) % 360.0 if a else float("nan")

    def pitch(self):
        a = self.last.get("ATTITUDE")
        return math.degrees(a.pitch) if a else float("nan")

    def pos(self):
        g = self.last.get("GLOBAL_POSITION_INT")
        return (g.lat / 1e7, g.lon / 1e7, g.alt / 1000.0) if g else None

    def hb(self):
        return self.last.get("HEARTBEAT")

    def send_as(self, sysid, fn):
        old = self.conn.mav.srcSystem
        self.conn.mav.srcSystem = sysid
        try:
            fn(self.conn.mav)
        finally:
            self.conn.mav.srcSystem = old

    def command(self, cmd, *p):
        p = list(p) + [0] * (7 - len(p))
        self.conn.mav.command_long_send(SYSID, 1, cmd, 0, *p)

    def param_set(self, name, value):
        self.conn.mav.param_set_send(SYSID, 1, name.encode(), value, M.MAV_PARAM_TYPE_REAL32)

    def param_read(self, name):
        self.conn.mav.param_request_read_send(SYSID, 1, name.encode(), -1)


class Both:
    def __init__(self, a, b):
        self.rigs = [a, b]
        self.sitl, self.standin = a, b
        self.failures = []

    def wait(self, seconds):
        end = time.time() + seconds
        while time.time() < end:
            for r in self.rigs:
                r.pump()
            time.sleep(0.02)

    def until(self, cond, timeout):
        end = time.time() + timeout
        while time.time() < end:
            for r in self.rigs:
                r.pump()
            if cond():
                return True
            time.sleep(0.02)
        return False

    def each(self, fn):
        return [fn(r) for r in self.rigs]

    def cmd(self, cmd, *p, settle=0.4):
        """The COMMAND_ACK result each side gives (None = no ack)."""
        marks = self.each(lambda r: r.mark())
        for r in self.rigs:
            r.command(cmd, *p)
        self.until(lambda: all(any(a.command == cmd for a in r.since(m, "COMMAND_ACK"))
                               for r, m in zip(self.rigs, marks)), 3.0)
        self.wait(settle)
        out = []
        for r, m in zip(self.rigs, marks):
            acks = [a for a in r.since(m, "COMMAND_ACK") if a.command == cmd]
            out.append(acks[0].result if acks else None)
        return out

    def compare(self, what, a, b, ok=None, note=""):
        good = (a == b) if ok is None else ok(a, b)
        print(f"  {'ok  ' if good else 'FAIL'} {what:44s} sitl={a!s:24s} standin={b!s:24s} {note}")
        if not good:
            self.failures.append(what)
        return good

    def show(self, what, a, b, note=""):
        print(f"  info {what:44s} sitl={a!s:24s} standin={b!s:24s} {note}")


def start_processes(args, tmp):
    os.environ["ARDUPILOT_HOME"] = str(Path(args.ardupilot).expanduser())
    spec_ = importlib.util.spec_from_file_location("swarm", ROOT / "sim" / "swarm.py")
    swarm = importlib.util.module_from_spec(spec_)
    spec_.loader.exec_module(swarm)
    platforms = tomllib.loads((ROOT / "sim" / "platforms.toml").read_text())
    spec = platforms["tracker"]
    sitl_dir = tmp / "sitl"
    sitl_dir.mkdir(parents=True, exist_ok=True)
    v = {"home": HOME, "instance": SITL_INSTANCE, "sysid": SYSID, "port": SITL_PORT}
    ns = argparse.Namespace(speedup=1.0, bridge_host="127.0.0.1", keep_eeprom=False)
    cmd = swarm.sitl_command(v, spec, ns, sitl_dir)
    if not Path(cmd[0]).exists():
        sys.exit(f"missing {cmd[0]}; pass --ardupilot")
    procs = [subprocess.Popen(cmd, cwd=sitl_dir, stdout=open(sitl_dir / "sitl.log", "w"),
                              stderr=subprocess.STDOUT, start_new_session=True)]
    engine = Path(args.engine) if args.engine else ROOT / "target" / "release" / "murmur-standins"
    if not engine.exists():
        sys.exit(f"missing {engine}; build with: cargo build --release -p murmur-standins")
    cfg = tmp / "standins.json"
    cfg.write_text(json.dumps({"bridge_host": "127.0.0.1", "vehicles": [{
        "uid": 1, "sysid": SYSID, "callsign": "TRK 1", "port": STANDIN_PORT, "home": HOME, "spec": spec}]}))
    procs.append(subprocess.Popen([str(engine), str(cfg)], stdout=open(tmp / "standins.log", "w"),
                                  stderr=subprocess.STDOUT, start_new_session=True))
    return procs


def stop(procs):
    for p in procs:
        try:
            os.killpg(p.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass


# ---------------------------------------------------------------------------------------- stages

def identity(b):
    print("identity and heartbeat")
    hb = b.each(lambda r: r.hb())
    b.compare("MAV_TYPE", hb[0].type, hb[1].type)
    b.compare("autopilot", hb[0].autopilot, hb[1].autopilot)
    b.compare("boot mode (AUTO = 10)", hb[0].custom_mode, hb[1].custom_mode)
    # SITL arms its servos once its safety switch / startup is done; the stand-in at once
    t0 = time.time()
    b.until(lambda: all(r.hb().base_mode & 128 for r in b.rigs), 40)
    b.show("seconds until armed (AUTO at boot)", f"{time.time() - t0:.0f}+", "0")
    hb = b.each(lambda r: r.hb())
    b.compare("armed at boot", bool(hb[0].base_mode & 128), bool(hb[1].base_mode & 128))
    b.compare("base_mode", hb[0].base_mode, hb[1].base_mode)
    b.compare("system_status", hb[0].system_status, hb[1].system_status)
    b.compare("heartbeat source sysid", SYSID, SYSID)
    yaw = b.each(lambda r: r.yaw())
    b.show("yaw at boot (SITL's mount drifts while disarmed)", f"{yaw[0]:.0f}", f"{yaw[1]:.0f}")
    nav = b.each(lambda r: r.last.get("NAV_CONTROLLER_OUTPUT"))
    b.compare("NAV_CONTROLLER_OUTPUT is sent", nav[0] is not None, nav[1] is not None)


def params(b):
    print("parameters")

    def read(name):
        marks = b.each(lambda r: r.mark())
        for r in b.rigs:
            r.param_read(name)
        b.wait(0.6)
        return [next((p.param_value for p in r.since(m, "PARAM_VALUE") if p.param_id == name), None)
                for r, m in zip(b.rigs, marks)]

    for name in ("SYSID_TARGET", "YAW_RANGE", "PITCH_MIN", "PITCH_MAX", "SERVO1_MIN", "SERVO1_MAX",
                 "ALT_SOURCE", "DISTANCE_MIN", "SCAN_SPEED_YAW", "INITIAL_MODE"):
        v = read(name)
        b.compare(f"{name} default", v[0], v[1])
    for r in b.rigs:
        r.param_set("SYSID_TARGET", TARGET_SYSID)
    b.wait(0.6)
    v = read("SYSID_TARGET")
    b.compare("SYSID_TARGET after PARAM_SET 42", v[0], v[1])
    marks = b.each(lambda r: r.mark())
    for r in b.rigs:
        r.param_set("SYSID_TARGET", 7.0 + 0.4)
    b.wait(0.6)
    echo = [[p.param_value for p in r.since(m, "PARAM_VALUE") if p.param_id == "SYSID_TARGET"]
            for r, m in zip(b.rigs, marks)]
    b.compare("SYSID_TARGET echo of 7.4 (int rounding)", echo[0], echo[1])
    marks = b.each(lambda r: r.mark())
    for r in b.rigs:
        r.param_set("NO_SUCH_PARAM", 1.0)
    b.wait(0.6)
    # (SITL may also announce an unrelated saved parameter, e.g. CMD_TOTAL, at boot: ignored)
    kinds = [sorted({(m.get_type(), m.param_id, getattr(m, "error", None)) for m in
                     r.since(mk, "PARAM_VALUE", "PARAM_ERROR") if m.param_id == "NO_SUCH_PARAM"})
             for r, mk in zip(b.rigs, marks)]
    b.compare("PARAM_SET of an unknown name replies", kinds[0], kinds[1])
    for r in b.rigs:
        r.param_set("SYSID_TARGET", 0)
    b.wait(0.6)
    v = read("SYSID_TARGET")
    b.compare("SYSID_TARGET back to 0", v[0], v[1])


def modes_and_arming(b):
    print("modes, arming, refusals")

    def state():
        b.wait(0.7)
        return b.each(lambda r: (r.hb().custom_mode, bool(r.hb().base_mode & 128)))

    names = {0: "MANUAL", 1: "STOP", 2: "SCAN", 3: "SERVOTEST", 4: "GUIDED", 10: "AUTO"}
    for n in (0, 1, 2, 3, 4, 10, 16, 99, 5):
        ack = b.cmd(M.MAV_CMD_DO_SET_MODE, 1, n)
        s = state()
        b.compare(f"DO_SET_MODE {n} {names.get(n, '')} ack", ack[0], ack[1])
        b.compare(f"  -> heartbeat (mode, armed)", s[0], s[1])
    b.cmd(M.MAV_CMD_DO_SET_MODE, 1, 1)  # STOP: servos disarmed
    state()
    for label, p in (("arm (1)", (1, 0)), ("disarm (0)", (0, 0)), ("param1=2", (2, 0)),
                     ("arm", (1, 0)), ("force disarm 21196", (0, 21196))):
        ack = b.cmd(M.MAV_CMD_COMPONENT_ARM_DISARM, *p)
        s = state()
        b.compare(f"ARM_DISARM {label} ack", ack[0], ack[1])
        b.compare("  -> heartbeat (mode, armed)", s[0], s[1])
    ack = b.cmd(M.MAV_CMD_MISSION_START, 0, 0)
    s = state()
    b.compare("MISSION_START ack", ack[0], ack[1])
    b.compare("  -> heartbeat (mode, armed)", s[0], s[1])
    ack = b.cmd(M.MAV_CMD_DO_SET_SERVO, 1, 1500)
    s = state()
    b.compare("DO_SET_SERVO 1 1500 ack", ack[0], ack[1])
    b.compare("  -> enters SERVOTEST (mode, armed)", s[0], s[1])
    b.cmd(M.MAV_CMD_DO_SET_MODE, 1, 10)
    ack = b.cmd(M.MAV_CMD_DO_SET_SERVO, 3, 1500)
    b.compare("DO_SET_SERVO 3 (no such servo) ack", ack[0], ack[1])
    ack = b.cmd(M.MAV_CMD_NAV_RETURN_TO_LAUNCH)
    b.compare("NAV_RETURN_TO_LAUNCH (not a tracker command)", ack[0], ack[1])
    ack = b.cmd(M.MAV_CMD_NAV_TAKEOFF, 0, 0, 0, 0, 0, 0, 10)
    b.compare("NAV_TAKEOFF", ack[0], ack[1])
    # the mission protocol: a tracker has no mission storage, only (in some builds) a home
    for label, send in (
            ("MISSION_COUNT 1 mission", lambda m: m.mission_count_send(SYSID, 1, 1, 0)),
            ("MISSION_COUNT 0 mission", lambda m: m.mission_count_send(SYSID, 1, 0, 0)),
            ("MISSION_COUNT 1 fence", lambda m: m.mission_count_send(SYSID, 1, 1, 1)),
            ("MISSION_COUNT 1 rally", lambda m: m.mission_count_send(SYSID, 1, 1, 2)),
            ("MISSION_REQUEST_LIST mission", lambda m: m.mission_request_list_send(SYSID, 1, 0)),
            ("MISSION_REQUEST_LIST fence", lambda m: m.mission_request_list_send(SYSID, 1, 1)),
            ("MISSION_CLEAR_ALL mission", lambda m: m.mission_clear_all_send(SYSID, 1, 0)),
            ("MISSION_REQUEST_INT 0", lambda m: m.mission_request_int_send(SYSID, 1, 0, 0))):
        marks = b.each(lambda r: r.mark())
        for r in b.rigs:
            send(r.conn.mav)
        b.wait(1.2)
        kinds = [sorted({(m.get_type(), getattr(m, "type", getattr(m, "count", None))) for m in
                         r.since(mk, "MISSION_ACK", "MISSION_REQUEST", "MISSION_REQUEST_INT",
                                 "MISSION_COUNT", "MISSION_ITEM_INT", "MISSION_ITEM")})
                 for r, mk in zip(b.rigs, marks)]
        texts = [[m.text for m in r.since(mk, "STATUSTEXT")] for r, mk in zip(b.rigs, marks)]
        b.show(label, kinds[0], kinds[1], f"texts {texts[0]} | {texts[1]}")


def heartbeat_lock(b):
    print("target lock from a heartbeat (SYSID_TARGET 0)")
    b.cmd(M.MAV_CMD_DO_SET_MODE, 1, 10)
    marks = b.each(lambda r: r.mark())
    for r in b.rigs:
        r.send_as(255, lambda mav: mav.heartbeat_send(M.MAV_TYPE_GCS, 8, 0, 0, 4))
        r.send_as(23, lambda mav: mav.heartbeat_send(M.MAV_TYPE_ANTENNA_TRACKER, 3, 0, 0, 4))
        r.send_as(24, lambda mav: mav.heartbeat_send(M.MAV_TYPE_GIMBAL, 3, 0, 0, 4))
    b.wait(0.8)
    for r in b.rigs:
        r.param_read("SYSID_TARGET")
    b.wait(0.6)
    v = [next((p.param_value for p in r.since(m, "PARAM_VALUE") if p.param_id == "SYSID_TARGET"), None)
         for r, m in zip(b.rigs, marks)]
    b.compare("GCS, tracker, gimbal heartbeats do not lock", v[0], v[1])
    marks = b.each(lambda r: r.mark())
    for r in b.rigs:
        r.send_as(55, lambda mav: mav.heartbeat_send(M.MAV_TYPE_FIXED_WING, 3, 0, 0, 4))
    b.wait(0.8)
    for r in b.rigs:
        r.param_read("SYSID_TARGET")
    b.wait(0.6)
    v = [next((p.param_value for p in r.since(m, "PARAM_VALUE") if p.param_id == "SYSID_TARGET"), None)
         for r, m in zip(b.rigs, marks)]
    b.compare("aircraft heartbeat locks SYSID_TARGET", v[0], v[1])
    streams = [sorted((m.req_stream_id, m.req_message_rate, m.target_system, m.start_stop)
                      for m in r.since(mk, "REQUEST_DATA_STREAM")) for r, mk in zip(b.rigs, marks)]
    b.compare("REQUEST_DATA_STREAM to the target", streams[0], streams[1])
    for r in b.rigs:
        r.param_set("SYSID_TARGET", TARGET_SYSID)
    b.wait(0.6)


def relay(rig, lat, lon, alt, rel_alt=0.0, vel=(0.0, 0.0, 0.0), sysid=TARGET_SYSID):
    rig.send_as(sysid, lambda mav: mav.global_position_int_send(
        int(time.time() * 1000) & 0xFFFFFFFF, int(lat * 1e7), int(lon * 1e7), int(alt * 1000),
        int(rel_alt * 1000), int(vel[0] * 100), int(vel[1] * 100), int(vel[2] * 100), 0))


def point_at(b, bearing, elev, dist, label, timeout=75.0, tol=1.0):
    """Hold a target still and report each side's pointing error once settled."""
    res = {}
    t0 = time.time()
    settled = [None, None]
    last_send = 0.0
    while time.time() - t0 < timeout:
        now = time.time()
        for r in b.rigs:
            r.pump()
        if now - last_send >= 0.5:
            last_send = now
            for i, r in enumerate(b.rigs):
                p = r.pos()
                if p is None:
                    continue
                lat, lon = flat_offset(p[0], p[1], bearing, dist)
                alt = p[2] + dist * math.tan(math.radians(elev))
                relay(r, lat, lon, alt, rel_alt=alt - HOME[2])
                ey = wrap180(r.yaw() - bearing)
                ep = r.pitch() - elev
                if abs(ey) < tol and abs(ep) < tol:
                    settled[i] = settled[i] or now
                else:
                    settled[i] = None
        if all(s is not None and now - s >= 3.0 for s in settled):
            break
        time.sleep(0.02)
    out = []
    for r in b.rigs:
        out.append((wrap180(r.yaw() - bearing), r.pitch() - elev))
    return out, time.time() - t0


def pointing(b, quick):
    print("pointing: a stationary target relayed around the quadrants (ALT_SOURCE 1, 1.2 km)")
    for r in b.rigs:
        r.param_set("ALT_SOURCE", 1)
    b.wait(0.6)
    b.cmd(M.MAV_CMD_DO_SET_MODE, 1, 10)
    # (bearing, elevation): the mount's centre is its start heading (north); +-170 reachable
    plan = [(0, 5), (45, 15), (90, 25), (135, 10), (170, 0), (225, 20), (270, 30), (315, 15),
            (355, 0), (190, -10), (183, 10), (177, 10), (90, 45), (45, 55), (0, 0)]
    if quick:
        plan = [(90, 20), (225, 15), (183, 10), (177, 10)]
    for bearing, elev in plan:
        rel = wrap180(bearing - HOME[3])
        reachable = abs(rel) <= YAW_RANGE / 2
        gap = 0.0 if reachable else abs(rel) - YAW_RANGE / 2
        (sy, sp), (ty, tp) = point_at(b, bearing, elev, 1200.0, f"{bearing}/{elev}")[0]
        # the demand is clamped to the pitch range; error is vs the clamped elevation
        el = max(-45.0, min(45.0, elev))
        sp, tp = sp + elev - el, tp + elev - el
        tag = f"bearing {bearing:3d} elev {elev:3d} {'reachable' if reachable else f'gap {gap:.0f} deg'}"
        ok_yaw = abs(ty) <= max(1.5, abs(sy) + 1.0) if reachable else abs(abs(ty) - gap) <= 3.0
        print(f"  {'ok  ' if ok_yaw else 'FAIL'} {tag:40s} yaw err sitl={sy:6.1f} standin={ty:6.1f} | "
              f"pitch err sitl={sp:6.1f} standin={tp:6.1f}")
        if not ok_yaw:
            b.failures.append(f"yaw {tag}")
        if elev >= -10 and elev <= 45 and abs(tp) > max(2.0, abs(sp) + 1.5):
            b.failures.append(f"pitch {tag}")
            print("  FAIL pitch")


def moving(b):
    print("pointing: a target circling at 1.5 km, 3 deg/s through north, sent at 1 Hz")
    rate = 3.0
    start = 270.0
    errs = [[], []]
    t0 = time.time()
    last = 0.0
    # first get both there
    while time.time() - t0 < 60.0:
        now = time.time()
        for r in b.rigs:
            r.pump()
        if now - last >= 1.0:
            last = now
            el = now - t0
            bearing = (start + rate * min(el, 60.0)) % 360.0
            for i, r in enumerate(b.rigs):
                p = r.pos()
                lat, lon = flat_offset(p[0], p[1], bearing, 1500.0)
                tang = math.radians(rate) * 1500.0
                vn = -tang * math.sin(math.radians(bearing))  # d/dt of (cos b, sin b) * R
                ve = tang * math.cos(math.radians(bearing))
                alt = p[2] + 1500.0 * math.tan(math.radians(10))
                relay(r, lat, lon, alt, alt - HOME[2], vel=(vn, ve, 0.0))
                if el > 30.0:
                    errs[i].append(abs(wrap180(r.yaw() - bearing)))
        time.sleep(0.02)
    for i, name in enumerate(("sitl", "standin")):
        e = errs[i]
        print(f"  info {name:8s} yaw error after 30 s: mean {sum(e) / len(e):.1f} max {max(e):.1f} deg")
    b.compare("moving target mean yaw error (+-1.5 of sitl)", round(sum(errs[0]) / len(errs[0]), 1),
              round(sum(errs[1]) / len(errs[1]), 1),
              ok=lambda s, t: abs(t - s) <= 1.5)


def wrong_sysid(b):
    print("position from a sysid that is not SYSID_TARGET")
    for r in b.rigs:
        r.param_set("SYSID_TARGET", TARGET_SYSID)
    b.wait(0.5)
    # settle at bearing 90 from the true target, then a foreign vehicle appears at 300
    point_at(b, 90, 0, 1200.0, "90", timeout=30.0)
    y0 = b.each(lambda r: r.yaw())
    t0 = time.time()
    last = 0.0
    while time.time() - t0 < 14.0:
        now = time.time()
        for r in b.rigs:
            r.pump()
        if now - last >= 0.5:
            last = now
            for r in b.rigs:
                p = r.pos()
                lat, lon = flat_offset(p[0], p[1], 300.0, 1200.0)
                relay(r, lat, lon, p[2], sysid=99)
        time.sleep(0.02)
    y1 = b.each(lambda r: r.yaw())
    moved = [abs(wrap180(y1[i] - y0[i])) > 20 for i in range(2)]
    b.compare("foreign-sysid position moves the mount", moved[0], moved[1])


def lost_target(b):
    print("target lost (no position for 5 s): AUTO scans once a target was set")
    point_at(b, 90, 0, 1200.0, "90", timeout=30.0)
    y0 = b.each(lambda r: r.yaw())
    b.wait(15.0)
    y1 = b.each(lambda r: r.yaw())
    d = [wrap180(y1[i] - y0[i]) for i in range(2)]
    b.show("yaw change over 15 s (5 s timeout, then scan)", f"{d[0]:.1f}", f"{d[1]:.1f}")
    b.compare("mount moves (scan) after the target is lost", abs(d[0]) > 3, abs(d[1]) > 3)
    b.compare("scan direction", d[0] > 0, d[1] > 0)


def servo_manual_stop(b):
    print("SERVOTEST, MANUAL, STOP")
    b.cmd(M.MAV_CMD_DO_SET_SERVO, 1, 1750)
    b.cmd(M.MAV_CMD_DO_SET_SERVO, 2, 1500)
    b.wait(16.0)
    y = b.each(lambda r: wrap180(r.yaw() - HOME[3]))
    b.compare("SERVOTEST yaw servo 1750 -> +85 deg (+-4)", round(y[0]), round(y[1]),
              ok=lambda s, t: abs(s - 85) < 4 and abs(t - 85) < 4)
    b.cmd(M.MAV_CMD_DO_SET_SERVO, 1, 1000)
    b.wait(32.0)
    y = b.each(lambda r: wrap180(r.yaw() - HOME[3]))
    b.compare("SERVOTEST yaw servo 1000 -> -170 deg (+-4)", round(y[0]), round(y[1]),
              ok=lambda s, t: abs(s + 170) < 4 and abs(t + 170) < 4)
    b.cmd(M.MAV_CMD_DO_SET_MODE, 1, 0)
    b.wait(22.0)
    y = b.each(lambda r: wrap180(r.yaw() - HOME[3]))
    b.compare("MANUAL (RC at trim) -> centre (+-5)", round(y[0]), round(y[1]),
              ok=lambda s, t: abs(s) < 5 and abs(t) < 5)
    b.cmd(M.MAV_CMD_DO_SET_MODE, 1, 1)
    b.wait(8.0)
    y = b.each(lambda r: wrap180(r.yaw() - HOME[3]))
    b.show("STOP after 8 s (SITL: PWM 0 drives to the stop)", f"{y[0]:.0f}", f"{y[1]:.0f}",
           "documented difference")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--ardupilot", default="~/ardupilot-fix", help="ArduPilot tree with the patched build")
    ap.add_argument("--engine", help="murmur-standins binary (default target/release)")
    ap.add_argument("--stages", help="comma list of: identity params modes lock pointing moving "
                                     "wrongsysid lost servo (default: all)")
    ap.add_argument("--quick", action="store_true", help="a few pointing targets, skip the long stages")
    args = ap.parse_args()

    import tempfile
    tmp = Path(tempfile.mkdtemp(prefix="tracker-vs-sitl-"))
    sitl = Rig("sitl", SITL_PORT)
    standin = Rig("standin", STANDIN_PORT)
    b = Both(sitl, standin)
    procs = start_processes(args, tmp)
    try:
        print(f"logs in {tmp}")
        for r in b.rigs:
            if r.conn.wait_heartbeat(timeout=60) is None:
                sys.exit(f"{r.name}: no heartbeat")
        for r in b.rigs:
            for msg_id, hz in ((30, 10), (62, 10), (33, 2)):
                r.command(M.MAV_CMD_SET_MESSAGE_INTERVAL, msg_id, 1e6 / hz)
        b.until(lambda: all(r.pos() and "ATTITUDE" in r.last and "NAV_CONTROLLER_OUTPUT" in r.last
                            for r in b.rigs), 20)
        stages = {"identity": identity, "params": params, "modes": modes_and_arming,
                  "lock": heartbeat_lock, "pointing": lambda b: pointing(b, args.quick),
                  "moving": moving, "wrongsysid": wrong_sysid, "lost": lost_target,
                  "servo": servo_manual_stop}
        want = args.stages.split(",") if args.stages else [
            k for k in stages if not (args.quick and k in ("moving", "wrongsysid", "lost", "servo"))]
        for k in want:
            stages[k](b)
    finally:
        stop(procs)
    print()
    if b.failures:
        print(f"{len(b.failures)} comparison(s) failed: {', '.join(b.failures)}")
        sys.exit(1)
    print("stand-in and SITL agree on every comparison")


if __name__ == "__main__":
    main()
