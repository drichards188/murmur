"""One UDP MAVLink link per vehicle, plus the vehicle state decoded from it."""

import asyncio
import math
import time
from collections import deque

from pymavlink import mavutil
from pymavlink.dialects.v20 import ardupilotmega as mavlink

GCS_SYSID = 255
GCS_COMPID = 190
AUTOPILOT_COMPID = 1

KIND_BY_MAV_TYPE = {
    mavlink.MAV_TYPE_FIXED_WING: "plane",
    mavlink.MAV_TYPE_VTOL_DUOROTOR: "vtol",
    mavlink.MAV_TYPE_VTOL_QUADROTOR: "vtol",
    mavlink.MAV_TYPE_VTOL_TILTROTOR: "vtol",
    mavlink.MAV_TYPE_QUADROTOR: "copter",
    mavlink.MAV_TYPE_HEXAROTOR: "copter",
    mavlink.MAV_TYPE_OCTOROTOR: "copter",
    mavlink.MAV_TYPE_TRICOPTER: "copter",
    mavlink.MAV_TYPE_COAXIAL: "copter",
    mavlink.MAV_TYPE_HELICOPTER: "copter",
    mavlink.MAV_TYPE_AIRSHIP: "blimp",
    mavlink.MAV_TYPE_SURFACE_BOAT: "boat",
    mavlink.MAV_TYPE_GROUND_ROVER: "ugv",
    mavlink.MAV_TYPE_SUBMARINE: "sub",
    mavlink.MAV_TYPE_ANTENNA_TRACKER: "tracker",
}
FLYING_KINDS = {"plane", "vtol", "copter", "blimp"}
# height above home that counts as "in the air"
AIRBORNE_ALT = {"plane": 15.0, "vtol": 10.0, "copter": 5.0, "blimp": 3.0}

LOSS_WINDOW_S = 10


def haversine_m(lat1, lon1, lat2, lon2):
    r = 6_371_000.0
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dp, dl = p2 - p1, math.radians(lon2 - lon1)
    a = math.sin(dp / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(dl / 2) ** 2
    return 2 * r * math.asin(math.sqrt(a))


class LinkStats:
    """Receive-side link health: silence, packet loss from MAVLink sequence gaps, RTT."""

    def __init__(self):
        self.last_rx = None
        self.last_seq = None
        self.buckets = deque()  # (second, received, expected)
        self.rtt_ms = None
        self.rx_total = 0
        self.tx_total = 0

    def on_packet(self, seq):
        now = time.monotonic()
        self.last_rx = now
        self.rx_total += 1
        if self.last_seq is None:
            expected = 1
        else:
            gap = (seq - self.last_seq) % 256
            if gap == 0 or gap > 128:
                # duplicate or reordered packet (a jump "backwards"), not loss.
                # Counting it as a 250-packet gap would wreck the estimate.
                return
            expected = gap
        self.last_seq = seq
        sec = int(now)
        if self.buckets and self.buckets[-1][0] == sec:
            s, r, e = self.buckets[-1]
            self.buckets[-1] = (s, r + 1, e + expected)
        else:
            self.buckets.append((sec, 1, expected))
        while self.buckets and self.buckets[0][0] <= sec - LOSS_WINDOW_S:
            self.buckets.popleft()

    def snapshot(self):
        now = time.monotonic()
        received = sum(b[1] for b in self.buckets if b[0] > now - LOSS_WINDOW_S)
        expected = sum(b[2] for b in self.buckets if b[0] > now - LOSS_WINDOW_S)
        return {
            "since_last_ms": None if self.last_rx is None else round((now - self.last_rx) * 1000),
            "loss_pct": None if expected == 0 else round(100.0 * (expected - received) / expected, 1),
            "rtt_ms": None if self.rtt_ms is None else round(self.rtt_ms, 1),
            "rx_total": self.rx_total,
            "tx_total": self.tx_total,
        }


class Vehicle:
    def __init__(self, sysid, link):
        self.sysid = sysid
        self.link = link
        self.kind = None
        self.mav_type = None
        self.mode = None
        self.custom_mode = None
        self.armed = False
        self.lat = self.lon = None
        self.alt_rel = self.alt_msl = None
        self.heading = None
        self.yaw = None
        self.roll = None
        self.pitch = None
        self.groundspeed = None
        self.airspeed = None
        self.battery_pct = None
        self.gps_fix = 0
        self.satellites = 0
        self.mission_seq = None
        self.home = None
        self.home_alt = None
        self.statustext = deque(maxlen=8)
        self.trail = deque(maxlen=120)
        self._last_trail = 0.0
        self._waiters = []
        # links that get a copy of this vehicle's position (antenna trackers pointed at it)
        self.forward_to = set()
        self.tracking = None  # for trackers: sysid being tracked
        self.identity = None  # from the fleet manifest: callsign, platform, role, domain, link...
        self.is_vtol = None  # planes only: None until Q_ENABLE is read
        self.ekf_flags = None
        self.nav_bearing = None
        self.target_dist = None
        self._fix_since = None

    # ---- inbound -------------------------------------------------------
    def handle(self, msg):
        t = msg.get_type()
        if t == "HEARTBEAT":
            if msg.autopilot == mavlink.MAV_AUTOPILOT_INVALID:
                return
            self.mav_type = msg.type
            self.kind = KIND_BY_MAV_TYPE.get(msg.type, "unknown")
            # ArduPlane reports FIXED_WING even as a QuadPlane; Q_ENABLE tells them apart
            if self.kind == "plane" and self.is_vtol:
                self.kind = "vtol"
            self.custom_mode = msg.custom_mode
            self.mode = mavutil.mode_mapping_bynumber(msg.type).get(msg.custom_mode, str(msg.custom_mode))
            self.armed = bool(msg.base_mode & mavlink.MAV_MODE_FLAG_SAFETY_ARMED)
        elif t == "GLOBAL_POSITION_INT":
            if msg.lat == 0 and msg.lon == 0:
                return
            self.lat, self.lon = msg.lat / 1e7, msg.lon / 1e7
            self.alt_rel = msg.relative_alt / 1000.0
            self.alt_msl = msg.alt / 1000.0
            if msg.hdg != 65535:
                self.heading = msg.hdg / 100.0
            if self.home is None:
                self.home = (self.lat, self.lon)
                self.home_alt = self.alt_msl - self.alt_rel
            now = time.monotonic()
            if now - self._last_trail > 1.0:
                self._last_trail = now
                # 3D trail: altitude above mean sea level, for the 3D map
                self.trail.append((round(self.lon, 6), round(self.lat, 6), round(self.alt_msl, 1)))
            for link in self.forward_to:
                link.send_raw(msg.get_msgbuf())
        elif t == "PARAM_VALUE" and msg.param_id == "Q_ENABLE":
            self.is_vtol = msg.param_value > 0
            if self.kind == "plane" and self.is_vtol:
                self.kind = "vtol"
        elif t == "NAV_CONTROLLER_OUTPUT":
            self.nav_bearing = msg.nav_bearing
            self.target_dist = msg.wp_dist
        elif t == "ATTITUDE":
            self.yaw = math.degrees(msg.yaw) % 360
            self.roll = math.degrees(msg.roll)
            self.pitch = math.degrees(msg.pitch)
        elif t == "HOME_POSITION":
            self.home = (msg.latitude / 1e7, msg.longitude / 1e7)
        elif t == "VFR_HUD":
            self.groundspeed = msg.groundspeed
            self.airspeed = msg.airspeed
        elif t == "SYS_STATUS":
            self.battery_pct = None if msg.battery_remaining < 0 else msg.battery_remaining
        elif t == "GPS_RAW_INT":
            self.gps_fix = msg.fix_type
            self.satellites = msg.satellites_visible
            if self.gps_fix >= 3 and self._fix_since is None:
                self._fix_since = time.monotonic()
        elif t == "EKF_STATUS_REPORT":
            self.ekf_flags = msg.flags
        elif t == "MISSION_CURRENT":
            self.mission_seq = msg.seq
        elif t == "STATUSTEXT":
            self.statustext.append({"t": time.time(), "severity": msg.severity, "text": msg.text})
        elif t == "TIMESYNC":
            if msg.tc1 != 0 and msg.ts1 in self.link.timesync_sent:
                sent = self.link.timesync_sent.pop(msg.ts1)
                self.link.stats.rtt_ms = (time.monotonic() - sent) * 1000.0

        for pred, fut in list(self._waiters):
            if not fut.done() and pred(msg):
                fut.set_result(msg)

    async def wait_for(self, pred, timeout):
        """Wait for a message matching pred. Returns None on timeout."""
        fut = asyncio.get_running_loop().create_future()
        entry = (pred, fut)
        self._waiters.append(entry)
        try:
            return await asyncio.wait_for(fut, timeout)
        except asyncio.TimeoutError:
            return None
        finally:
            self._waiters.remove(entry)

    # ---- derived -------------------------------------------------------
    @property
    def has_position(self):
        """Ready to navigate: GPS fix AND the autopilot's EKF trusts its absolute position.
        The fix arrives seconds before the EKF sets its origin; AUTO is refused until then."""
        if self.lat is None:
            return False
        if self.ekf_flags is not None and self.kind != "tracker":
            # the vehicle's own navigation filter is the authority: a submerged AUV has no GPS
            # fix but navigates fine on inertial + DVL
            f = self.ekf_flags
            return bool(f & mavlink.EKF_POS_HORIZ_ABS) and not f & mavlink.EKF_CONST_POS_MODE
        if self.gps_fix < 3:
            return False
        if self.kind == "tracker":
            return True  # trackers don't navigate; a GPS fix is enough to know where they stand
        if self._fix_since is None:
            return False
        if self.ekf_flags is None:
            # firmware that doesn't report EKF status: give it a grace period after the fix
            return time.monotonic() - self._fix_since > 15
        f = self.ekf_flags
        return bool(f & mavlink.EKF_POS_HORIZ_ABS) and not f & mavlink.EKF_CONST_POS_MODE

    @property
    def airborne(self):
        return self.kind in FLYING_KINDS and self.alt_rel is not None and self.alt_rel > AIRBORNE_ALT[self.kind]

    def mode_number(self, name):
        for num, n in mavutil.mode_mapping_bynumber(self.mav_type).items():
            if n == name:
                return num
        raise KeyError(f"{self.kind} has no {name} mode")

    def distance_to(self, lat, lon):
        if self.lat is None:
            return None
        return haversine_m(self.lat, self.lon, lat, lon)

    def send(self, msg):
        self.link.send(msg)

    @property
    def mav(self):
        return self.link.mav

    def snapshot(self):
        return {
            "sysid": self.sysid,
            "identity": self.identity,
            "kind": self.kind,
            "mode": self.mode,
            "armed": self.armed,
            "position": None if self.lat is None else [self.lon, self.lat],
            "alt_rel": self.alt_rel,
            "alt_msl": self.alt_msl,
            "roll": None if self.roll is None else round(self.roll, 1),
            "pitch": None if self.pitch is None else round(self.pitch, 1),
            "heading": self.heading,
            "yaw": self.yaw,
            "tracking": self.tracking,
            "nav_bearing": self.nav_bearing,
            "groundspeed": self.groundspeed,
            "airspeed": self.airspeed,
            "battery_pct": self.battery_pct,
            "gps_fix": self.gps_fix,
            "nav_ready": self.has_position,
            "satellites": self.satellites,
            "mission_seq": self.mission_seq,
            "home": None if self.home is None else [self.home[1], self.home[0]],
            "link": self.link.stats.snapshot(),
            "trail": list(self.trail),
            "statustext": list(self.statustext)[-3:],
        }


class VehicleLink(asyncio.DatagramProtocol):
    """UDP endpoint for one vehicle. SITL sends to us; we reply to its source address."""

    def __init__(self, port, on_vehicle):
        self.port = port
        self.on_vehicle = on_vehicle
        self.transport = None
        self.remote = None
        self.vehicle = None
        self.stats = LinkStats()
        self.timesync_sent = {}
        self.mav = mavlink.MAVLink(None, srcSystem=GCS_SYSID, srcComponent=GCS_COMPID)
        self.mav.robust_parsing = True

    def connection_made(self, transport):
        self.transport = transport

    def datagram_received(self, data, addr):
        self.remote = addr
        try:
            msgs = self.mav.parse_buffer(data) or []
        except mavlink.MAVError:
            return
        for msg in msgs:
            if msg.get_type() == "BAD_DATA":
                continue
            sysid, compid = msg.get_srcSystem(), msg.get_srcComponent()
            if compid == AUTOPILOT_COMPID:
                self.stats.on_packet(msg.get_seq())
            if self.vehicle is None:
                if msg.get_type() != "HEARTBEAT" or compid != AUTOPILOT_COMPID:
                    continue
                self.vehicle = Vehicle(sysid, self)
                self.on_vehicle(self.vehicle)
            if sysid == self.vehicle.sysid:
                self.vehicle.handle(msg)

    def send(self, msg):
        if self.transport is None or self.remote is None:
            return
        buf = msg.pack(self.mav)
        self.mav.seq = (self.mav.seq + 1) % 256
        self.transport.sendto(buf, self.remote)
        self.stats.tx_total += 1

    def send_raw(self, buf):
        """Relay bytes as-is (keeps the original sender's sysid), e.g. positions to a tracker."""
        if self.transport is not None and self.remote is not None:
            self.transport.sendto(bytes(buf), self.remote)
            self.stats.tx_total += 1

    def send_timesync(self):
        ts1 = time.time_ns()
        self.timesync_sent[ts1] = time.monotonic()
        # forget pings that never came back so the dict can't grow forever
        cutoff = time.monotonic() - 10
        for k in [k for k, v in self.timesync_sent.items() if v < cutoff]:
            del self.timesync_sent[k]
        self.send(self.mav.timesync_encode(0, ts1))
