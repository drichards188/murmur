#!/usr/bin/env python3
"""Stand-in vehicles: lightweight MAVLink-speaking kinematic models.

For platforms ArduPilot SITL can't credibly fly (a Global Hawk at 18 km, a torpedo AUV,
a 40 m ship, a drifting buoy) and for scaling to 100+ nodes cheaply (~0.1% CPU each vs
~16% for a SITL process). Each stand-in has its own UDP link to the bridge and imitates an
ArduPilot autopilot at the protocol level: heartbeat with ArduPilot mode numbers for its
MAV_TYPE, telemetry, COMMAND_ACKs, the mission upload protocol, and AUTO / RTL / hold /
loiter behaviour. So the C2 bridge treats it exactly like SITL or a real vehicle.

What they model: position, heading, speed, altitude/depth, turn rate and climb rate
limits from sim/platforms.toml, endurance as battery %, and (if tidewater is installed
and a scenario is set) real water currents, drift and grounding. What they don't: any
aerodynamics or hydrodynamics. They are labelled stand-ins in the console.
"""

import asyncio
import json
import math
import os
import random
import sys
import time

from pymavlink import mavutil
from pymavlink.dialects.v20 import ardupilotmega as mavlink

PHYSICS_HZ = 10
GCS_SYSID, GCS_COMPID = 255, 190

MAV_TYPE = {
    "plane": mavlink.MAV_TYPE_FIXED_WING,
    "boat": mavlink.MAV_TYPE_SURFACE_BOAT,
    "drifter": mavlink.MAV_TYPE_SURFACE_BOAT,
    "sub": mavlink.MAV_TYPE_SUBMARINE,
}
# behaviour -> mode name used for "hold position" and the passive default
HOLD_MODE = {"plane": "LOITER", "boat": "HOLD", "drifter": "HOLD", "sub": "POSHOLD"}
EKF_OK = (mavlink.EKF_ATTITUDE | mavlink.EKF_VELOCITY_HORIZ | mavlink.EKF_VELOCITY_VERT
          | mavlink.EKF_POS_HORIZ_REL | mavlink.EKF_POS_HORIZ_ABS | mavlink.EKF_POS_VERT_ABS)
EARTH_R = 6_371_000.0


def bearing_deg(lat1, lon1, lat2, lon2):
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dl = math.radians(lon2 - lon1)
    y = math.sin(dl) * math.cos(p2)
    x = math.cos(p1) * math.sin(p2) - math.sin(p1) * math.cos(p2) * math.cos(dl)
    return math.degrees(math.atan2(y, x)) % 360


def distance_m(lat1, lon1, lat2, lon2):
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dp, dl = p2 - p1, math.radians(lon2 - lon1)
    a = math.sin(dp / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(dl / 2) ** 2
    return 2 * EARTH_R * math.asin(math.sqrt(a))


def wrap180(a):
    return (a + 180) % 360 - 180


class Conditions:
    """Optional tidewater hookup: real currents, water depth and wind for marine stand-ins."""

    def __init__(self, scenario, at, rate):
        self.cond = None
        if not scenario or not at:
            return
        try:
            from tidewater.conditions import Conditions as TW
            from tidewater.scenario import Scenario, parse_time
        except ImportError:
            print("tidewater not installed in this environment; stand-ins run without currents")
            return
        self.cond = TW.for_scenario(Scenario.load(os.path.expanduser(scenario)))
        self.t0 = parse_time(at).timestamp()
        self.rate = rate
        self.wall0 = time.monotonic()
        print(f"tidewater conditions from {scenario} at {at} x{rate}")

    def at(self, lat, lon, depth):
        if self.cond is None:
            return None
        t = self.t0 + (time.monotonic() - self.wall0) * self.rate
        return self.cond.at(lat, lon, t, depth_m=depth)


class StandIn(asyncio.DatagramProtocol):
    def __init__(self, cfg, conditions):
        self.cfg = cfg
        self.spec = cfg["spec"]
        self.behavior = self.spec["behavior"]
        self.sysid = cfg["sysid"]
        self.callsign = cfg["callsign"]
        self.conditions = conditions
        self.mav_type = MAV_TYPE[self.behavior]
        self.modes = {n: k for k, n in mavutil.mode_mapping_bynumber(self.mav_type).items()}
        self.mav = mavlink.MAVLink(None, srcSystem=self.sysid, srcComponent=1)
        self.rx = mavlink.MAVLink(None)
        self.rx.robust_parsing = True
        self.transport = None

        lat, lon, alt, hdg = cfg["home"]
        airborne_start = self.behavior == "plane" and alt > 50
        # home is the surface/ground at the spawn point; aircraft spawned high start on station
        self.home = (lat, lon, 0.0 if airborne_start else float(alt))
        self.lat, self.lon = lat, lon
        self.alt = float(alt)  # metres above MSL (negative = depth)
        self.heading = float(hdg)
        self.speed = self.spec["cruise_mps"] if airborne_start else 0.0
        self.vz = 0.0
        self.armed = airborne_start
        self.mode = HOLD_MODE[self.behavior] if airborne_start else ("MANUAL" if "MANUAL" in self.modes else HOLD_MODE[self.behavior])
        self.hold_point = (lat, lon)
        self.mission = []
        self.seq = 0
        self.jumps = {}
        self.upload = None  # (count, items, last_request_time)
        self.aground = False
        self.started = time.monotonic()
        self.texts = []

    # ---- link -----------------------------------------------------------------------
    def connection_made(self, transport):
        self.transport = transport

    def send(self, msg):
        if self.transport is None:
            return
        try:
            self.transport.sendto(msg.pack(self.mav))
        except OSError:
            pass  # bridge not up yet
        self.mav.seq = (self.mav.seq + 1) % 256

    def text(self, s, severity=mavlink.MAV_SEVERITY_INFO):
        self.send(self.mav.statustext_encode(severity, s.encode()[:50]))

    def ack(self, command, result=mavlink.MAV_RESULT_ACCEPTED):
        self.send(self.mav.command_ack_encode(command, result))

    # ---- inbound ---------------------------------------------------------------------
    def datagram_received(self, data, addr):
        try:
            msgs = self.rx.parse_buffer(data) or []
        except mavlink.MAVError:
            return
        for m in msgs:
            t = m.get_type()
            if t == "BAD_DATA":
                continue
            target = getattr(m, "target_system", self.sysid)
            if target not in (0, self.sysid):
                continue
            handler = getattr(self, f"on_{t.lower()}", None)
            if handler:
                handler(m)

    def on_timesync(self, m):
        if m.tc1 == 0:
            self.send(self.mav.timesync_encode(time.time_ns(), m.ts1))

    def on_param_request_read(self, m):
        name = m.param_id
        value = 0.0  # e.g. Q_ENABLE: stand-in planes are not QuadPlanes
        self.send(self.mav.param_value_encode(name.encode(), value, mavlink.MAV_PARAM_TYPE_REAL32, 1, 0))

    def on_param_set(self, m):
        self.send(self.mav.param_value_encode(m.param_id.encode(), m.param_value, m.param_type, 1, 0))

    def on_command_long(self, m):
        c = m.command
        if c == mavlink.MAV_CMD_COMPONENT_ARM_DISARM:
            if m.param1 == 0 and self.airborne:
                return self.ack(c, mavlink.MAV_RESULT_DENIED)  # no disarming in flight
            self.armed = m.param1 == 1
            self.text("Arming motors" if self.armed else "Disarming motors")
            return self.ack(c)
        if c == mavlink.MAV_CMD_DO_SET_MODE:
            name = mavutil.mode_mapping_bynumber(self.mav_type).get(int(m.param2))
            if name is None:
                return self.ack(c, mavlink.MAV_RESULT_UNSUPPORTED)
            if self.behavior == "drifter" and name not in ("HOLD", "MANUAL"):
                return self.ack(c, mavlink.MAV_RESULT_UNSUPPORTED)  # no propulsion
            self.set_mode(name)
            return self.ack(c)
        if c == mavlink.MAV_CMD_DO_SET_MISSION_CURRENT:
            self.seq = int(m.param1)
            return self.ack(c)
        if c in (mavlink.MAV_CMD_SET_MESSAGE_INTERVAL, mavlink.MAV_CMD_REQUEST_MESSAGE):
            return self.ack(c)
        self.ack(c, mavlink.MAV_RESULT_UNSUPPORTED)

    def on_mission_set_current(self, m):
        self.seq = m.seq

    def on_mission_count(self, m):
        if m.mission_type != mavlink.MAV_MISSION_TYPE_MISSION:
            return self.send(self.mav.mission_ack_encode(GCS_SYSID, GCS_COMPID, mavlink.MAV_MISSION_UNSUPPORTED, m.mission_type))
        if self.behavior == "drifter":
            return self.send(self.mav.mission_ack_encode(GCS_SYSID, GCS_COMPID, mavlink.MAV_MISSION_UNSUPPORTED, 0))
        self.upload = [m.count, [None] * m.count, 0.0]
        self.request_next()

    def request_next(self):
        count, items, _ = self.upload
        seq = next(i for i, it in enumerate(items) if it is None)
        self.upload[2] = time.monotonic()
        self.send(self.mav.mission_request_int_encode(GCS_SYSID, GCS_COMPID, seq, 0))

    def on_mission_item_int(self, m):
        if not self.upload or m.seq >= self.upload[0]:
            return
        self.upload[1][m.seq] = m
        if all(it is not None for it in self.upload[1]):
            self.mission = self.upload[1]
            self.upload = None
            self.seq = 1 if len(self.mission) > 1 else 0
            self.jumps = {}
            self.send(self.mav.mission_ack_encode(GCS_SYSID, GCS_COMPID, mavlink.MAV_MISSION_ACCEPTED, 0))
            self.text(f"Mission: {len(self.mission) - 1} WP")
        else:
            self.request_next()

    def set_mode(self, name):
        if name != self.mode:
            self.mode = name
            self.hold_point = (self.lat, self.lon)

    # ---- state -----------------------------------------------------------------------
    @property
    def airborne(self):
        return self.behavior == "plane" and self.alt - self.home[2] > 15

    @property
    def depth(self):
        return max(0.0, -self.alt)

    # ---- physics ---------------------------------------------------------------------
    def target(self):
        """(lat, lon, alt, loiter_radius or None, speed) for the current mode/mission."""
        s = self.spec
        cruise = s["cruise_mps"]
        hold_r = s.get("loiter_radius_m", 0) if self.behavior == "plane" else None
        if not self.armed or self.behavior == "drifter":
            return None
        if self.mode == "AUTO" and self.mission:
            while self.seq < len(self.mission):
                it = self.mission[self.seq]
                if self.seq == 0:
                    self.seq = 1
                    continue
                if it.command == mavlink.MAV_CMD_DO_JUMP:
                    left = self.jumps.get(self.seq, int(it.param2))
                    if left == 0:
                        self.seq += 1
                        continue
                    if left > 0:
                        self.jumps[self.seq] = left - 1
                    self.seq = int(it.param1)
                    continue
                lat, lon = it.x / 1e7, it.y / 1e7
                alt = self.home[2] + it.z if it.frame == mavlink.MAV_FRAME_GLOBAL_RELATIVE_ALT_INT else it.z
                if it.command in (mavlink.MAV_CMD_NAV_TAKEOFF, mavlink.MAV_CMD_NAV_VTOL_TAKEOFF):
                    return (self.lat, self.lon, self.home[2] + it.z, None, cruise, "takeoff")
                if it.command == mavlink.MAV_CMD_NAV_LOITER_UNLIM:
                    return (lat, lon, alt, abs(it.param3) or s.get("loiter_radius_m", 200), cruise, "loiter")
                if it.command == mavlink.MAV_CMD_NAV_RETURN_TO_LAUNCH:
                    return (*self.home[:2], self.alt, hold_r, cruise, "rtl")
                return (lat, lon, alt, None, cruise, "waypoint")
            # mission finished: hold where we are
            return (*self.hold_point, self.alt, hold_r, cruise, "hold")
        if self.mode == "RTL":
            alt = self.alt if self.behavior != "sub" else self.alt
            return (*self.home[:2], alt, hold_r, cruise, "rtl")
        if self.behavior == "plane":
            return (*self.hold_point, self.alt, hold_r or 300, cruise, "hold")  # planes can't stop
        return None  # boats/subs in HOLD/POSHOLD/MANUAL: stop

    def step(self, dt):
        s = self.spec
        tgt = self.target()
        want_speed = 0.0
        want_heading = self.heading
        want_alt = self.alt
        if tgt:
            lat, lon, alt, radius, speed, kind = tgt
            want_alt = alt
            want_speed = speed
            dist = distance_m(self.lat, self.lon, lat, lon)
            btc = bearing_deg(self.lat, self.lon, lat, lon)
            if kind == "takeoff":
                want_heading = self.heading
                if self.alt >= alt - 5:
                    self.advance()
            elif radius:  # loiter / orbit, clockwise
                corr = max(-90.0, min(90.0, (dist - radius) / max(radius, 1) * 90.0))
                want_heading = btc - 90 + corr
            else:
                want_heading = btc
                reach = max(20.0, self.speed * 3) if self.behavior == "plane" else 10.0
                if dist < reach and kind == "waypoint":
                    self.reached()
                elif dist < reach and kind == "rtl" and self.behavior != "plane":
                    want_speed = 0.0
        if self.behavior == "plane" and self.airborne:
            want_speed = max(want_speed, s["cruise_mps"] * 0.8)  # fixed wing keeps flying

        # turn, accelerate, climb within platform limits
        turn = s.get("turn_dps", 10) * dt
        self.heading = (self.heading + max(-turn, min(turn, wrap180(want_heading - self.heading)))) % 360
        accel = (3.0 if self.behavior == "plane" else 0.5) * dt
        self.speed += max(-accel, min(accel, want_speed - self.speed))
        climb = s.get("climb_mps", 0)
        if self.behavior == "plane" and not self.airborne and self.speed < s["cruise_mps"] * 0.6:
            climb = 0  # still on the runway
        self.vz = max(-climb, min(climb, (want_alt - self.alt)))
        self.alt += self.vz * dt

        # water: current, depth limits, grounding (needs tidewater)
        drift_e = drift_n = 0.0
        if self.behavior in ("boat", "drifter", "sub"):
            c = self.conditions.at(self.lat, self.lon, self.depth)
            if c is not None and c.in_area:
                drift_e, drift_n = c.current_east, c.current_north
                draft = 0.5 if self.behavior != "sub" else 0.0
                if self.behavior == "sub":
                    floor = -(c.water_depth_m - 1.0)
                    self.alt = max(self.alt, floor)  # rest on the seabed
                else:
                    was = self.aground
                    self.aground = not c.navigable(draft + (2.0 if s["max_mps"] > 10 else 0.0))
                    if self.aground and not was:
                        self.text("Aground", mavlink.MAV_SEVERITY_WARNING)
            if self.behavior == "drifter":
                drift_e += random.gauss(0, 0.02)
                drift_n += random.gauss(0, 0.02)
        if self.aground:
            self.speed = 0.0
            drift_e = drift_n = 0.0

        ve = self.speed * math.sin(math.radians(self.heading)) + drift_e
        vn = self.speed * math.cos(math.radians(self.heading)) + drift_n
        self.lat += vn * dt / 111_320.0
        self.lon += ve * dt / (111_320.0 * math.cos(math.radians(self.lat)))
        self.ve, self.vn = ve, vn

    def advance(self):
        self.seq += 1
        self.send(self.mav.mission_current_encode(self.seq))

    def reached(self):
        self.send(self.mav.mission_item_reached_encode(self.seq))
        self.text(f"Reached waypoint #{self.seq}")
        self.advance()

    # ---- telemetry -------------------------------------------------------------------
    def telemetry(self, tick):
        if tick % PHYSICS_HZ == 0:  # 1 Hz
            base = mavlink.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED | (mavlink.MAV_MODE_FLAG_SAFETY_ARMED if self.armed else 0)
            self.send(self.mav.heartbeat_encode(self.mav_type, mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA, base,
                                                self.modes.get(self.mode, 0), mavlink.MAV_STATE_ACTIVE))
            self.send(self.mav.attitude_encode(0, 0.0, 0.0, math.radians(wrap180(self.heading)), 0, 0, 0))
            gs = math.hypot(getattr(self, "ve", 0), getattr(self, "vn", 0))
            self.send(self.mav.vfr_hud_encode(self.speed, gs, int(self.heading), 50 if self.speed else 0, self.alt, self.vz))
            self.send(self.mav.mission_current_encode(self.seq))
            if self.upload and time.monotonic() - self.upload[2] > 1.5:
                self.request_next()  # lost item: ask again
        if tick % (PHYSICS_HZ // 2) == 0:  # 2 Hz
            ve, vn = getattr(self, "ve", 0.0), getattr(self, "vn", 0.0)
            self.send(self.mav.global_position_int_encode(
                int((time.monotonic() - self.started) * 1000), int(self.lat * 1e7), int(self.lon * 1e7),
                int(self.alt * 1000), int((self.alt - self.home[2]) * 1000),
                int(vn * 100), int(ve * 100), int(-self.vz * 100), int(self.heading * 100)))
        if tick % (PHYSICS_HZ * 2) == 0:  # 0.5 Hz
            hours = (time.monotonic() - self.started) / 3600
            batt = max(0, int(100 - 100 * hours / self.spec.get("endurance_h", 10)))
            self.send(self.mav.sys_status_encode(0, 0, 0, 0, 0, -1, batt, 0, 0, 0, 0, 0, 0))
            submerged = self.behavior == "sub" and self.depth > 1.0
            fix = 0 if submerged else 3  # no GPS underwater: AUVs navigate on INS + DVL
            self.send(self.mav.gps_raw_int_encode(0, fix, int(self.lat * 1e7), int(self.lon * 1e7),
                                                  int(self.alt * 1000), 100, 100, int(self.speed * 100),
                                                  int(self.heading * 100), 0 if submerged else 14))
            self.send(self.mav.ekf_status_report_encode(EKF_OK, 0.1, 0.1, 0.1, 0.1, 0.1))


async def main(path):
    cfg = json.loads(open(path).read())
    conditions = Conditions(os.environ.get("MURMUR_SCENARIO"), os.environ.get("MURMUR_AT"),
                            float(os.environ.get("MURMUR_RATE", "1")))
    loop = asyncio.get_running_loop()
    host = cfg.get("bridge_host", "127.0.0.1")
    fleet = []
    for v in cfg["vehicles"]:
        s = StandIn(v, conditions)
        await loop.create_datagram_endpoint(lambda s=s: s, remote_addr=(host, v["port"]))
        fleet.append(s)
        print(f"{s.callsign:12s} sysid {s.sysid:3d} {v['spec']['name']} -> udp {v['port']}")
    sys.stdout.flush()
    dt = 1.0 / PHYSICS_HZ
    tick = 0
    next_t = time.monotonic()
    while True:
        for s in fleet:
            s.step(dt)
            s.telemetry(tick)
        tick += 1
        next_t += dt
        await asyncio.sleep(max(0.0, next_t - time.monotonic()))


if __name__ == "__main__":
    try:
        asyncio.run(main(sys.argv[1]))
    except KeyboardInterrupt:
        pass
