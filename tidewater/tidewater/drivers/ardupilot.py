"""Param driver: real conditions into stock ArduPilot SITL, no ArduPilot changes.

ArduPilot SITL has world-wide simulation settings for wind (SIM_WIND_SPD/DIR) and
water current (SIM_TIDE_SPEED/DIR). They are uniform across the simulated world, but
every SITL vehicle is its own process, so setting them per vehicle from the conditions
at that vehicle's position gives each one its own local wind and current, updated as
it moves and as the scenario clock runs.

What stock SITL does with them (checked in libraries/SITL, ArduPilot master 2026-10):
  - wind: applied to every vehicle model
  - current: only the sailboat/motorboat model (SIM_Sailboat.cpp), and only while armed.
    Subs and ground rovers ignore it.
  - both directions are "coming FROM"; tidewater vectors point where things are GOING.

With the tidewater SITL patch (patches/ardupilot/, adds SIM_WATER_DEPTH) the driver also
sends the local water depth: boats run aground when it's shallower than their draft, and
subs get a real sea floor plus the current at their depth. Without the patch those
params don't exist and ArduPilot ignores them.

Connects to each instance's SERIAL1 TCP port (5762 + 10 * instance by default), so it
runs alongside a GCS or bridge on SERIAL0 without interfering.
"""

import datetime as dt
import math
import threading
import time

from pymavlink import mavutil

from ..conditions import Conditions
from ..scenario import Scenario, parse_time

BOAT_TYPES = {mavutil.mavlink.MAV_TYPE_SURFACE_BOAT}
SUB_TYPES = {mavutil.mavlink.MAV_TYPE_SUBMARINE}
# change thresholds before re-sending a param (keeps the link quiet)
MIN_SPEED_CHANGE = 0.05
MIN_DIR_CHANGE = 3.0
MIN_DEPTH_CHANGE = 0.05
# keel depth for the AGROUND status; matches the patched SITL motorboat's draft
BOAT_DRAFT_M = 0.3


def from_dir(east, north):
    """(east, north) TOWARD vector -> speed and the direction it comes FROM (ArduPilot convention)."""
    return math.hypot(east, north), (math.degrees(math.atan2(east, north)) + 180) % 360


def parse_instances(spec):
    out = []
    for part in spec.split(","):
        if "-" in part:
            a, b = part.split("-")
            out.extend(range(int(a), int(b) + 1))
        elif part:
            out.append(int(part))
    return out


class Clock:
    """Scenario time: starts at `at`, runs `rate` scenario seconds per wall second."""

    def __init__(self, at: dt.datetime, rate: float):
        self.at = at
        self.rate = rate
        self.wall0 = time.monotonic()

    def now(self) -> dt.datetime:
        return self.at + dt.timedelta(seconds=(time.monotonic() - self.wall0) * self.rate)


class VehicleFeed:
    def __init__(self, instance, url):
        self.instance = instance
        self.url = url
        self.conn = None
        self.mav_type = None
        self.sysid = None
        self.lat = self.lon = None
        self.depth = 0.0
        self.sent = {}  # param -> last value sent
        self.status = "connecting"
        self.last = None  # last Sample

    def connect(self, patience_s=90):
        # SITL may still be booting: retry until its TCP port accepts
        deadline = time.monotonic() + patience_s
        while True:
            try:
                self.conn = mavutil.mavlink_connection(self.url, source_system=254, autoreconnect=True)
                break
            except OSError:
                if time.monotonic() > deadline:
                    raise
                time.sleep(2)
        hb = self.conn.wait_heartbeat(timeout=30)
        if hb is None:
            raise TimeoutError(f"no heartbeat on {self.url}")
        self.sysid = hb.get_srcSystem()
        self.mav_type = hb.type
        self.conn.mav.request_data_stream_send(self.sysid, 1, mavutil.mavlink.MAV_DATA_STREAM_POSITION, 2, 1)
        self.status = "connected"

    def pump(self):
        """Drain incoming messages; keep the latest position."""
        while True:
            msg = self.conn.recv_match(type="GLOBAL_POSITION_INT", blocking=False)
            if msg is None:
                return
            if msg.lat or msg.lon:
                self.lat, self.lon = msg.lat / 1e7, msg.lon / 1e7
                self.depth = max(0.0, -msg.relative_alt / 1000.0)

    def set_param(self, name, value, tolerance):
        prev = self.sent.get(name)
        if prev is not None and abs(value - prev) < tolerance:
            return
        self.conn.mav.param_set_send(self.sysid, 1, name.encode(), float(value),
                                     mavutil.mavlink.MAV_PARAM_TYPE_REAL32)
        self.sent[name] = value

    @property
    def kind(self):
        if self.mav_type in BOAT_TYPES:
            return "boat"
        if self.mav_type in SUB_TYPES:
            return "sub"
        return mavutil.mavlink.enums["MAV_TYPE"][self.mav_type].name.replace("MAV_TYPE_", "").lower()


def apply(feed: VehicleFeed, cond: Conditions, t):
    if feed.lat is None:
        feed.status = "waiting for position"
        return
    s = cond.at(feed.lat, feed.lon, t, depth_m=feed.depth)
    feed.last = s
    if not s.in_area:
        feed.status = "outside scenario area"
        return
    ws, wfrom = from_dir(s.wind_east, s.wind_north)
    feed.set_param("SIM_WIND_SPD", ws, MIN_SPEED_CHANGE)
    feed.set_param("SIM_WIND_DIR", wfrom, MIN_DIR_CHANGE)
    notes = []
    if feed.kind == "boat":
        cs, cfrom = from_dir(s.current_east, s.current_north)
        feed.set_param("SIM_TIDE_SPEED", cs, MIN_SPEED_CHANGE)
        feed.set_param("SIM_TIDE_DIR", cfrom, MIN_DIR_CHANGE)
        if not s.navigable(BOAT_DRAFT_M):
            notes.append("AGROUND")
    elif feed.kind == "sub":
        cs, cfrom = from_dir(s.current_east, s.current_north)  # sampled at the sub's depth
        feed.set_param("SIM_TIDE_SPEED", cs, MIN_SPEED_CHANGE)
        feed.set_param("SIM_TIDE_DIR", cfrom, MIN_DIR_CHANGE)
    if feed.kind in ("boat", "sub"):
        # needs the tidewater SITL patch; stock SITL ignores the unknown param
        feed.set_param("SIM_WATER_DEPTH", max(0.0, s.water_depth_m), MIN_DEPTH_CHANGE)
    feed.status = " ".join(notes) or "ok"


def run(args):
    sc = Scenario.load(args.scenario)
    cond = Conditions.for_scenario(sc)
    clock = Clock(parse_time(args.at), args.rate)
    feeds = [VehicleFeed(i, f"tcp:{args.host}:{args.base_port + 10 * i}") for i in parse_instances(args.instances)]

    def connect(f):
        try:
            f.connect()
        except Exception as e:
            f.status = f"failed: {e}"

    threads = [threading.Thread(target=connect, args=(f,), daemon=True) for f in feeds]
    for th in threads:
        th.start()
    for th in threads:
        th.join()
    live = [f for f in feeds if f.conn and f.sysid]
    print(f"{sc.title}: driving {len(live)}/{len(feeds)} vehicles, clock x{args.rate}")
    for f in feeds:
        if f not in live:
            print(f"  instance {f.instance}: {f.status}")

    last_print = 0.0
    while True:
        t = clock.now()
        for f in live:
            f.pump()
            apply(f, cond, t)
        if time.monotonic() - last_print > 10:
            last_print = time.monotonic()
            print(f"--- scenario time {t:%Y-%m-%d %H:%M:%S}Z  tide {cond.tide.level(t):+.2f} m")
            for f in live:
                s = f.last
                if s is None or not s.in_area:
                    print(f"  sysid {f.sysid:2} {f.kind:12} {f.status}")
                    continue
                cs, cd = s.current
                ws, wd = s.wind
                cur = f"current {cs:4.2f} m/s -> {cd:3.0f}" if s.wet else "on land"
                print(f"  sysid {f.sysid:2} {f.kind:12} {cur} | wind {ws:4.1f} m/s -> {wd:3.0f} | {f.status}")
        time.sleep(args.period)
