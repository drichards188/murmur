#!/usr/bin/env python3
"""Stand-ins against real ArduPilot SITL: blimp (Blimp), ugv (Rover skid-steer), sailboat (Rover).

For each kind, runs the SITL firmware exactly as sim/swarm.py launches it (same binary, model,
defaults) and the Rust stand-in side by side, plays the same GCS script at both (refused mode,
AUTO without a mission, mission upload with a dropped item, arm, AUTO, RTL, hold, disarm,
commands the autopilot refuses), records every message, then compares what a C2 sees:
MAV_TYPE / autopilot, the heartbeat mode and armed sequence, COMMAND_ACKs, the mission
protocol (requests, acks, items reached), the STATUSTEXTs an operator acts on, which message
types arrive, and the trajectory loosely (same waypoints reached, end point).

    cargo build --release -p murmur-standins
    .venv/bin/python sim/tests/standins_sitl_blimp_ugv_sailboat.py               # all three
    .venv/bin/python sim/tests/standins_sitl_blimp_ugv_sailboat.py --kind ugv

SITL instances 60.. (TCP 5760+10*I) and UDP ports 19000.. so it runs beside `make goldengate`.
SITL boots for ~20-40 s first; each side's script clock starts when it is ready (SITL: GPS fix
and pre-arm checks passing; stand-in: first heartbeat). Stand-ins run without tidewater here;
the sailboat stand-in then uses ArduPilot SITL's default wind (3 m/s from 180), as SITL does.
Exit status 0 = the protocol matches, 1 = a difference (listed).
"""

import argparse
import json
import os
import math
import re
import select
import socket
import subprocess
import sys
import time
import urllib.request
from pathlib import Path
from types import SimpleNamespace

from pymavlink.dialects.v20 import ardupilotmega as mavlink

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "sim"))
import swarm  # noqa: E402  (sitl_command: launch SITL exactly as the fleet launcher does)

M = mavlink
REL = M.MAV_FRAME_GLOBAL_RELATIVE_ALT_INT
WP, LOITER, TAKEOFF, JUMP, RTL_CMD = (M.MAV_CMD_NAV_WAYPOINT, M.MAV_CMD_NAV_LOITER_UNLIM, M.MAV_CMD_NAV_TAKEOFF,
                                      M.MAV_CMD_DO_JUMP, M.MAV_CMD_NAV_RETURN_TO_LAUNCH)

# What the stand-in sends (crates/murmur-standins/src/telemetry.rs); SITL is asked for the same.
STREAMS = {"HEARTBEAT": 1, "ATTITUDE": 1, "VFR_HUD": 1, "MISSION_CURRENT": 1, "GLOBAL_POSITION_INT": 2,
           "SYS_STATUS": 0.5, "GPS_RAW_INT": 0.5, "EKF_STATUS_REPORT": 0.5}
PREARM_OK = 0x10000000  # MAV_SYS_STATUS_PREARM_CHECK in SYS_STATUS health

# STATUSTEXTs an operator or the bridge acts on; boot and sensor chatter is left out.
SEMANTIC = re.compile(r"arm|mission|reached|mode|command|tack|destination|aground|shore|obstacle|skipp", re.I)

# kind -> (platform in sim/platforms.toml, home [lat, lon, alt, hdg])
KINDS = {
    "blimp": ("lta", [37.8000, -122.5300, 0, 90]),
    "ugv": ("ugv_logistics", [37.7990, -122.4600, 0, 90]),
    "sailboat": ("saildrone", [37.8000, -122.5300, 0, 90]),
}


def offset(lat, lon, north_m, east_m):
    return lat + north_m / 111_320.0, lon + east_m / (111_320.0 * math.cos(math.radians(lat)))


class Gcs:
    """One vehicle's GCS end: listens on the vehicle's port, logs everything, serves uploads."""

    def __init__(self, port, t0):
        self.sysid = 1
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", port))
        self.sock.setblocking(False)
        self.remote = None
        self.mav = mavlink.MAVLink(None, srcSystem=255, srcComponent=190)
        self.rx = mavlink.MAVLink(None)
        self.rx.robust_parsing = True
        self.t0 = t0
        self.log = []  # (t, type, fields, srcSystem, srcComponent)
        self.upload = None
        self.drop = set()

    def now(self):
        return time.monotonic() - self.t0

    def send(self, msg):
        if self.remote is not None:
            self.sock.sendto(msg.pack(self.mav), self.remote)

    def receive(self):
        while True:
            try:
                data, addr = self.sock.recvfrom(4096)
            except BlockingIOError:
                return
            self.remote = addr
            for m in self.rx.parse_buffer(data) or []:
                t = m.get_type()
                if t == "BAD_DATA" or m.get_srcSystem() != self.sysid:
                    continue
                d = m.to_dict()
                d.pop("mavpackettype", None)
                self.log.append((round(self.now(), 3), t, d, m.get_srcSystem(), m.get_srcComponent()))
                if t in ("MISSION_REQUEST_INT", "MISSION_REQUEST") and self.upload is not None:
                    if m.seq in self.drop:
                        self.drop.discard(m.seq)  # lost on the link once: the vehicle must ask again
                        continue
                    if m.seq < len(self.upload):
                        self.send(self.upload[m.seq])
                if t == "MISSION_ACK":
                    self.upload = None

    def command(self, cmd, *p):
        p = list(p) + [0.0] * (7 - len(p))
        self.send(self.mav.command_long_encode(self.sysid, 1, cmd, 0, *p))

    def mode(self, number):
        self.command(M.MAV_CMD_DO_SET_MODE, 1, number)

    def mission(self, items, drop=()):
        self.upload = [self.mav.mission_item_int_encode(self.sysid, 1, i, frame, cmd, 0, 1, p1, p2, p3, 0,
                                                        int(lat * 1e7), int(lon * 1e7), alt)
                       for i, (cmd, frame, lat, lon, alt, p1, p2, p3) in enumerate(items)]
        self.drop = set(drop)
        self.send(self.mav.mission_count_encode(self.sysid, 1, len(items)))

    def streams(self):
        for name, hz in STREAMS.items():
            self.command(M.MAV_CMD_SET_MESSAGE_INTERVAL, getattr(M, f"MAVLINK_MSG_ID_{name}"), 1e6 / hz)


def script(kind, home):
    """[(t after ready, action(gcs))]. Same at SITL and the stand-in."""
    lat, lon = home[:2]
    s = []

    def at(t, fn):
        s.append((t, fn))

    at(0.0, lambda g: g.mode(99))  # no such mode
    if kind == "blimp":
        # Blimp: modes LAND 0, MANUAL 1, VELOCITY 2, LOITER 3, RTL 4, AUTO 5, HOLD 6. AUTO takes any
        # mission (or none) and runs only NAV_WAYPOINT (TAKEOFF, LOITER_UNLIM: "Command not
        # supported. Skipped."). Its SITL flies at 0.25 m/s, hence the long script.
        w1, w2 = offset(lat, lon, 0, 25), offset(lat, lon, 20, 25)
        at(1.0, lambda g: g.command(M.MAV_CMD_NAV_LAND))
        at(1.5, lambda g: g.mode(5))  # no mission: accepted, "Mission complete."
        at(2.5, lambda g: g.mode(3))
        at(3.0, lambda g: g.mission([(WP, REL, lat, lon, 0, 0, 0, 0), (TAKEOFF, REL, 0, 0, 10, 0, 0, 0),
                                     (WP, REL, *w1, 10, 0, 0, 0), (WP, REL, *w2, 10, 0, 0, 0),
                                     (LOITER, REL, *w2, 10, 0, 0, 0)], drop={3}))
        at(7.0, lambda g: g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1))
        at(8.0, lambda g: g.mode(5))
        at(205.0, lambda g: g.command(M.MAV_CMD_NAV_TAKEOFF))
        at(210.0, lambda g: g.mode(4))  # RTL: back to home, and down to it
        at(330.0, lambda g: g.mode(2))
        at(333.0, lambda g: g.mode(6))
        at(336.0, lambda g: g.mode(1))
        at(339.0, lambda g: g.mode(0))
        at(390.0, lambda g: g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 0))
        return s, 400.0
    if kind == "ugv":
        # Rover: AUTO refused without a mission; square patrol with a turn back (skid steer turns
        # in place), Mission Complete, RTL (Reached destination), HOLD, LOITER, disarm
        p1, p2, p3 = offset(lat, lon, 0, 60), offset(lat, lon, 40, 60), offset(lat, lon, 40, 0)
        at(0.5, lambda g: g.mode(2))  # LEARNING is gone from ArduRover: "No such mode 2"
        at(1.0, lambda g: g.mode(10))
        at(2.0, lambda g: g.mission([(WP, REL, lat, lon, 0, 0, 0, 0), (WP, REL, *p1, 0, 0, 0, 0),
                                     (WP, REL, *p2, 0, 0, 0, 0), (WP, REL, *p3, 0, 0, 0, 0)], drop={2}))
        at(3.5, lambda g: g.mode(11))  # RTL, disarmed: arming is refused here ("Mode not armable")
        at(4.0, lambda g: g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1))
        at(4.5, lambda g: g.mode(4))
        at(6.0, lambda g: g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1))
        at(7.0, lambda g: g.mode(10))
        at(80.0, lambda g: g.mode(11))
        at(110.0, lambda g: g.mode(4))
        at(112.0, lambda g: g.mode(5))
        at(114.0, lambda g: g.command(M.MAV_CMD_NAV_TAKEOFF))
        at(116.0, lambda g: g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 0))
        return s, 120.0
    # sailboat: wind from the south (SITL default 3 m/s from 180): the first waypoint is dead
    # upwind (no-go zone: tacks), the second a beam reach
    up, beam = offset(lat, lon, -80, 0), offset(lat, lon, -80, 80)
    at(1.0, lambda g: g.mode(10))
    at(2.0, lambda g: g.mission([(WP, REL, lat, lon, 0, 0, 0, 0), (WP, REL, *up, 0, 0, 0, 0),
                                 (WP, REL, *beam, 0, 0, 0, 0)]))
    at(6.0, lambda g: g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1))
    at(7.0, lambda g: g.mode(10))
    at(300.0, lambda g: g.mode(11))
    at(420.0, lambda g: g.mode(4))
    at(425.0, lambda g: g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 0))
    return s, 430.0


def start_sitl(kind, platform, home, inst, port, workdir):
    spec = swarm.load_platforms()[platform]
    v = {"home": home, "instance": inst, "sysid": 1, "port": port}
    args = SimpleNamespace(speedup=1.0, bridge_host="127.0.0.1", keep_eeprom=False)
    wd = workdir / f"sitl-{kind}"
    wd.mkdir(parents=True, exist_ok=True)
    cmd = swarm.sitl_command(v, spec, args, wd)
    return subprocess.Popen(cmd, cwd=wd, stdout=open(wd / "sitl.log", "w"), stderr=subprocess.STDOUT)


def start_standins(units, base_port, workdir, env_extra=None):
    """units: [(platform, home)]. One engine, one vehicle per port from base_port."""
    cat = swarm.load_platforms()
    vehicles = []
    for i, (platform, home) in enumerate(units):
        spec = cat[platform]
        # the same lost-link timer the SITL side gets from swarm.sitl_command
        params = swarm.lost_link_params(platform, spec, swarm.STANDIN_FIRMWARE.get(spec["behavior"]))
        vehicles.append({"uid": i + 1, "sysid": 1, "callsign": f"{spec['callsign']} {i + 1}", "port": base_port + i,
                         "home": home, "sim": "standin", "name": spec["name"], "spec": spec, "params": params})
    cfg = workdir / "standins.json"
    cfg.write_text(json.dumps({"bridge_host": "127.0.0.1", "vehicles": vehicles}))
    env = {k: v for k, v in os.environ.items()
           if k not in ("MURMUR_TIDEWATER_URL", "MURMUR_AT", "MURMUR_RATE", "MURMUR_SCENARIO")}
    env.update(env_extra or {})
    return subprocess.Popen([str(ROOT / "target" / "release" / "murmur-standins"), str(cfg)], env=env,
                            stdout=open(workdir / "standins.log", "w"), stderr=subprocess.STDOUT)


class Side:
    """One vehicle under test (SITL or stand-in) and its script."""

    def __init__(self, name, gcs, kind, sitl, plan=None):
        self.name, self.gcs, self.kind, self.sitl = name, gcs, kind, sitl
        # plan() -> (actions, duration), made when the vehicle is ready
        self.plan = plan or (lambda: script(kind, KINDS[kind][1]))
        self.first_hb = None
        self.ready = None
        self.pending = None
        self.end = None

    def poll(self, now):
        g = self.gcs
        g.receive()
        if self.first_hb is None:
            if any(e[1] == "HEARTBEAT" for e in g.log):
                self.first_hb = now
                g.streams()
            return
        if self.ready is None:
            if not self.sitl:
                self.ready = self.first_hb + 2.0
            else:
                st = [e for e in g.log if e[1] == "SYS_STATUS"]
                gps = [e for e in g.log if e[1] == "GPS_RAW_INT"]
                ekf = [e for e in g.log if e[1] == "EKF_STATUS_REPORT"]
                # pre-arm passing is not enough: modes that need a position fail until the
                # EKF has an absolute horizontal position (not constant-position mode)
                ok = (st and st[-1][2]["onboard_control_sensors_health"] & PREARM_OK
                      and gps and gps[-1][2]["fix_type"] >= 3
                      and ekf and ekf[-1][2]["flags"] & 16 and not ekf[-1][2]["flags"] & 128)
                if ok:
                    self.ready = now + 3.0
                elif now - self.first_hb > 25 and not st:
                    g.streams()  # first request lost while booting
                    self.first_hb = now - 10
            if self.ready is not None:
                actions, duration = self.plan()
                self.pending = sorted(((self.ready + t, fn) for t, fn in actions), key=lambda x: x[0])
                self.end = self.ready + duration
            return
        while self.pending and self.pending[0][0] <= now:
            self.pending.pop(0)[1](g)

    def done(self, now):
        return self.end is not None and now >= self.end

    def events(self):
        """Log from ready to the end of the script, times relative to ready."""
        return [(round(t - self.ready, 2), *rest) for (t, *rest) in self.gcs.log
                if self.ready - 0.5 <= t <= self.end]


def run(kinds, args, workdir):
    t0 = time.monotonic()
    sides = []
    procs = []
    try:
        if not args.standin_only:
            for i, kind in enumerate(kinds):
                platform, home = KINDS[kind]
                port = args.base_port + i
                g = Gcs(port, t0)
                procs.append(start_sitl(kind, platform, home, args.instance + i, port, workdir))
                sides.append(Side("sitl", g, kind, True))
        if not args.sitl_only:
            sport = args.base_port + 10
            gs = [Gcs(sport + i, t0) for i in range(len(kinds))]
            procs.append(start_standins([KINDS[k] for k in kinds], sport, workdir))
            sides += [Side("standin", g, kind, False) for g, kind in zip(gs, kinds)]
        deadline = time.monotonic() + 600 + max(script(k, KINDS[k][1])[1] for k in kinds)
        while not all(s.done(time.monotonic() - t0) for s in sides):
            now = time.monotonic() - t0
            if time.monotonic() > deadline:
                raise SystemExit("timed out; see " + str(workdir))
            for s in sides:
                if s.first_hb is None and now > 90:
                    raise SystemExit(f"{s.name} {s.kind}: no heartbeat; see {workdir}")
                if s.ready is None and s.first_hb is not None and now - s.first_hb > 150:
                    raise SystemExit(f"{s.name} {s.kind}: never ready; see {workdir}")
                s.poll(now)
            select.select([s.gcs.sock for s in sides], [], [], 0.02)
    finally:
        for p in procs:
            p.terminate()
        for p in procs:
            try:
                p.wait(5)
            except subprocess.TimeoutExpired:
                p.kill()
    out = {}
    for s in sides:
        out.setdefault(s.kind, {})[s.name] = s.events()
        s.gcs.sock.close()
    return out


# ---- comparison ---------------------------------------------------------------------------

def ev(log, t):
    return [e for e in log if e[1] == t]


def horiz_m(a, b):
    dn = (a[0] - b[0]) * 111_320.0
    de = (a[1] - b[1]) * 111_320.0 * math.cos(math.radians(a[0]))
    return math.hypot(dn, de)


def compare(kind, a, b):
    """a = SITL, b = stand-in. Returns (ok, lines)."""
    ok = True
    lines = [f"{kind} ({KINDS[kind][0]})"]

    def check(cond, what, info=False):
        nonlocal ok
        if cond:
            tag = "  ok     "
        elif info:
            tag = "  note   "
        else:
            tag = "  FAIL   "
            ok = False
        lines.append(tag + what)

    ident = lambda log: sorted({(e[2]["type"], e[2]["autopilot"]) for e in ev(log, "HEARTBEAT")})  # noqa: E731
    check(ident(a) == ident(b), f"MAV_TYPE/autopilot {ident(a)} vs {ident(b)}")

    def modes(log):
        seq = []
        for e in ev(log, "HEARTBEAT"):
            m = (e[2]["custom_mode"], bool(e[2]["base_mode"] & M.MAV_MODE_FLAG_SAFETY_ARMED))
            if not seq or seq[-1] != m:
                seq.append(m)
        return seq
    check(modes(a) == modes(b), f"mode/armed sequence {modes(a)}" + ("" if modes(a) == modes(b) else f" vs {modes(b)}"))
    acks = lambda log: [(e[2]["command"], e[2]["result"]) for e in ev(log, "COMMAND_ACK")  # noqa: E731
                        if e[2]["command"] != M.MAV_CMD_SET_MESSAGE_INTERVAL]
    aa, ab = acks(a), acks(b)
    check(aa == ab, f"COMMAND_ACKs {aa}" + ("" if aa == ab else f" vs {ab}"))
    macks = lambda log: [(e[2]["type"], e[2]["mission_type"]) for e in ev(log, "MISSION_ACK")]  # noqa: E731
    check(macks(a) == macks(b), f"MISSION_ACKs {macks(a)}" + ("" if macks(a) == macks(b) else f" vs {macks(b)}"))
    reqs = lambda log: [(e[1], e[2]["seq"]) for e in log if e[1] in ("MISSION_REQUEST_INT", "MISSION_REQUEST")]  # noqa: E731
    check(reqs(a) == reqs(b), f"mission requests {reqs(a)}" + ("" if reqs(a) == reqs(b) else f" vs {reqs(b)}"))
    reached = lambda log: [e[2]["seq"] for e in ev(log, "MISSION_ITEM_REACHED")]  # noqa: E731
    check(reached(a) == reached(b), f"MISSION_ITEM_REACHED {reached(a)}" + ("" if reached(a) == reached(b) else f" vs {reached(b)}"))
    cur = lambda log: [x[2]["seq"] for x in ev(log, "MISSION_CURRENT")]  # noqa: E731
    dedup = lambda xs: [x for i, x in enumerate(xs) if i == 0 or x != xs[i - 1]]  # noqa: E731
    check(dedup(cur(a)) == dedup(cur(b)), f"MISSION_CURRENT sequence {dedup(cur(a))}"
          + ("" if dedup(cur(a)) == dedup(cur(b)) else f" vs {dedup(cur(b))}"))

    def texts(log):
        """[(severity, text)] for the texts an operator acts on."""
        return [(e[2]["severity"], e[2]["text"]) for e in ev(log, "STATUSTEXT") if SEMANTIC.search(e[2]["text"])]
    # Tacking is a navigation event whose count depends on the trajectory: compare the rest exactly
    pa = [t for t in texts(a) if "Tacking" not in t[1]]
    pb = [t for t in texts(b) if "Tacking" not in t[1]]
    check(pa == pb, f"STATUSTEXTs {pa}" + ("" if pa == pb else f"\n           vs {pb}"))
    tack = lambda log: sum("Tacking" in e[2]["text"] for e in ev(log, "STATUSTEXT"))  # noqa: E731
    if kind == "sailboat":
        check(tack(a) > 0 and tack(b) > 0, f"tacked upwind: SITL {tack(a)} tacks, stand-in {tack(b)}")
    types_a, types_b = {e[1] for e in a}, {e[1] for e in b}
    check(types_b <= types_a, f"stand-in message types all sent by SITL too: {sorted(types_b - types_a) or 'yes'}")
    track = lambda log: [(e[0], e[2]["lat"] / 1e7, e[2]["lon"] / 1e7, e[2]["relative_alt"] / 1000) for e in ev(log, "GLOBAL_POSITION_INT")]  # noqa: E731
    tra, trb = track(a), track(b)
    if tra and trb:
        end = horiz_m(tra[-1][1:3], trb[-1][1:3])
        far = lambda tr: max(horiz_m(tr[0][1:3], p[1:3]) for p in tr)  # noqa: E731
        check(end < 30.0, f"end points {end:.1f} m apart; furthest from start SITL {far(tra):.0f} m, "
              f"stand-in {far(trb):.0f} m; max alt SITL {max(p[3] for p in tra):.1f} m, "
              f"stand-in {max(p[3] for p in trb):.1f} m", info=True)
        for lbl, tr in (("SITL", tra), ("stand-in", trb)):
            vs = [math.hypot(e[2]["vx"], e[2]["vy"]) / 100 for e in ev(a if lbl == "SITL" else b, "GLOBAL_POSITION_INT")]
            lines.append(f"  info {lbl} ground speed max {max(vs):.2f} m/s, mean while moving "
                         f"{(sum(v for v in vs if v > 0.2) / max(1, sum(v > 0.2 for v in vs))):.2f} m/s")
    return ok, lines


# ---- with tidewater: land, water and wind (stand-ins only) ---------------------------------

# Golden Gate: the UGV drives north off Crissy Field toward the Bay (shoreline about 37.8102 at
# this longitude); the sailboat starts west of the Gate; the blimp on the Marin Headlands.
TW_UNITS = {
    "ugv": ("ugv_logistics", [37.8080, -122.4550, 6, 0]),
    "sailboat": ("saildrone", [37.8000, -122.5300, 0, 0]),
    "blimp": ("lta", [37.8330, -122.4830, 45, 0]),
}


def tw_sample(url, t, points):
    req = urllib.request.Request(url + "/v1/sample", data=json.dumps({"t": t, "points": points}).encode(),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.loads(r.read())["samples"]


def tw_plan(kind, home, wind_from):
    lat, lon = home[:2]
    s = []

    def at(t, fn):
        s.append((t, fn))

    if kind == "ugv":
        water = (37.8125, lon)  # in the Bay, straight ahead
        at(1, lambda g: g.mission([(WP, REL, lat, lon, 0, 0, 0, 0), (WP, REL, *water, 0, 0, 0, 0)]))
        at(4, lambda g: g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1))
        at(5, lambda g: g.mode(10))
        at(130, lambda g: g.mode(11))  # it can still drive away from the water
        return s, 220.0
    if kind == "sailboat":
        b = math.radians(wind_from)  # waypoint 250 m dead upwind
        up = offset(lat, lon, 250 * math.cos(b), 250 * math.sin(b))
        at(1, lambda g: g.mission([(WP, REL, lat, lon, 0, 0, 0, 0), (WP, REL, *up, 0, 0, 0, 0)]))
        at(4, lambda g: g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1))
        at(5, lambda g: g.mode(10))
        return s, 420.0
    # blimp: climb to 20 m over the spawn point and try to stay there
    at(1, lambda g: g.mission([(WP, REL, lat, lon, 0, 0, 0, 0), (WP, REL, lat, lon, 20, 0, 0, 0)]))
    at(4, lambda g: g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1))
    at(5, lambda g: g.mode(5))
    return s, 150.0


def run_tidewater(kinds, args, workdir):
    url = args.tidewater_url.rstrip("/")
    at_unix = swarm_time(args.at)
    homes = {k: TW_UNITS[k][1] for k in kinds}
    samples = dict(zip(kinds, tw_sample(url, at_unix, [[h[0], h[1], None] for h in homes.values()])))
    wind = {k: (math.hypot(s["wind_east"], s["wind_north"]),
                math.degrees(math.atan2(-s["wind_east"], -s["wind_north"])) % 360) for k, s in samples.items()}
    t0 = time.monotonic()
    port = args.base_port + 20
    gs = [Gcs(port + i, t0) for i in range(len(kinds))]
    proc = start_standins([TW_UNITS[k] for k in kinds], port, workdir,
                          {"MURMUR_TIDEWATER_URL": url, "MURMUR_AT": args.at})
    sides = [Side("standin", g, k, False, plan=lambda k=k: tw_plan(k, homes[k], wind[k][1]))
             for g, k in zip(gs, kinds)]
    try:
        while not all(s.done(time.monotonic() - t0) for s in sides):
            now = time.monotonic() - t0
            if now > 60 and any(s.first_hb is None for s in sides):
                raise SystemExit(f"no heartbeat; see {workdir / 'standins.log'}")
            for s in sides:
                s.poll(now)
            select.select([g.sock for g in gs], [], [], 0.02)
    finally:
        proc.terminate()
        proc.wait(5)
    ok = True
    lines = []

    def check(cond, what):
        nonlocal ok
        ok &= bool(cond)
        lines.append(("  ok   " if cond else "  FAIL ") + what)

    for s in sides:
        log = s.events()
        k = s.kind
        texts = [e[2]["text"] for e in ev(log, "STATUSTEXT")]
        track = [(e[0], e[2]["lat"] / 1e7, e[2]["lon"] / 1e7, e[2]["relative_alt"] / 1000)
                 for e in ev(log, "GLOBAL_POSITION_INT")]
        spd, frm = wind[k]
        lines.append(f"{k} with tidewater at {args.at} (wind {spd:.1f} m/s from {frm:.0f})")
        if k == "ugv":
            before = [p for p in track if p[0] < 130]
            north = max(p[1] for p in before)
            stop = before[-1]
            # the shoreline moves with the tide: ask about the time the vehicle was there
            got = tw_sample(url, at_unix + stop[0], [[stop[1], stop[2], None], [north, stop[2], None]])
            check(any(t.startswith("Shoreline") for t in texts), f"stopped at the shoreline: {texts}")
            check(got[0]["water_depth_m"] <= 0.1 and got[1]["water_depth_m"] <= 0.5,
                  f"stayed on land: stopped at {stop[1]:.5f} (ground {got[0]['elevation_m']:.1f} m), "
                  f"furthest north {north:.5f} (water depth {got[1]['water_depth_m']:.1f} m)")
            check("Mission Complete" not in texts and not ev(log, "MISSION_ITEM_REACHED"),
                  "the mission did not complete")
            check("Reached destination" in texts, "drove away from the water in RTL and got home")
            alts = [p[3] for p in track]
            lines.append(f"  info altitude above home followed the DEM from {min(alts):.1f} to {max(alts):.1f} m")
        elif k == "sailboat":
            tacks = sum("Tacking" in t for t in texts)
            check(tacks > 0, f"tacked toward the upwind waypoint: {tacks} tacks")
            check("Reached waypoint #1" in texts, "reached the upwind waypoint" +
                  (f" after {next(e[0] for e in ev(log, 'MISSION_ITEM_REACHED')):.0f} s" if ev(log, "MISSION_ITEM_REACHED") else ""))
            hd = sorted({round(e[2]["hdg"] / 100 / 10) * 10 % 360 for e in ev(log, "GLOBAL_POSITION_INT")
                         if math.hypot(e[2]["vx"], e[2]["vy"]) > 50})
            lines.append(f"  info headings sailed (10 deg bins): {hd}")
        else:
            high = [p for p in track if p[3] > 18]
            check(high, "climbed to 20 m")
            if high:
                drift = horiz_m(high[0][1:3], track[-1][1:3])
                b = math.degrees(math.atan2((track[-1][2] - high[0][2]) * math.cos(math.radians(high[0][1])),
                                            track[-1][1] - high[0][1])) % 360
                dt = track[-1][0] - high[0][0]
                downwind = (frm + 180) % 360
                check(abs((b - downwind + 180) % 360 - 180) < 30 and drift > 10,
                      f"could not hold station: drifted {drift:.0f} m toward {b:.0f} in {dt:.0f} s "
                      f"(downwind is {downwind:.0f}; air speed max {s_max(log):.1f} m/s)")
    return ok, lines


def s_max(log):
    return max((e[2]["airspeed"] for e in ev(log, "VFR_HUD")), default=0.0)


def swarm_time(at):
    """ISO time (tidewater's format) to unix seconds."""
    from datetime import datetime, timezone
    d = datetime.fromisoformat(at.replace("Z", "+00:00"))
    return (d if d.tzinfo else d.replace(tzinfo=timezone.utc)).timestamp()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--kind", choices=sorted(KINDS), action="append", help="default: all")
    ap.add_argument("--base-port", type=int, default=19000)
    ap.add_argument("--instance", type=int, default=60)
    ap.add_argument("--workdir", default=str(ROOT / "sim" / "run" / "sitl-compare"))
    ap.add_argument("--sitl-only", action="store_true", help="record SITL only (exploring)")
    ap.add_argument("--standin-only", action="store_true")
    ap.add_argument("--compare-only", action="store_true", help="re-compare the logs of the last run")
    ap.add_argument("--tidewater", action="store_true",
                    help="instead: stand-ins in the Golden Gate with tidewater (shoreline, tacking, wind drift)")
    ap.add_argument("--tidewater-url", default="http://127.0.0.1:8876", help="a running `tidewater serve`")
    ap.add_argument("--at", default="2026-06-14T10:00Z", help="scenario time for --tidewater")
    args = ap.parse_args()
    kinds = args.kind or sorted(KINDS)
    workdir = Path(args.workdir)
    workdir.mkdir(parents=True, exist_ok=True)
    if args.tidewater:
        ok, lines = run_tidewater(kinds, args, workdir)
        print("\n".join(lines))
        print("OK" if ok else "FAILED (see above)")
        sys.exit(0 if ok else 1)
    if args.compare_only:
        logs = json.loads((workdir / "logs.json").read_text())
    else:
        if not args.sitl_only and not (ROOT / "target" / "release" / "murmur-standins").exists():
            sys.exit("missing target/release/murmur-standins; run: cargo build --release -p murmur-standins")
        logs = run(kinds, args, workdir)
        old = json.loads((workdir / "logs.json").read_text()) if (workdir / "logs.json").exists() else {}
        for k, sides in logs.items():
            old.setdefault(k, {}).update(sides)
        logs = old
        (workdir / "logs.json").write_text(json.dumps(logs))
    if args.sitl_only or args.standin_only:
        print(f"recorded in {workdir / 'logs.json'}")
        return
    all_ok = True
    for kind in kinds:
        sides = logs.get(kind, {})
        if "sitl" not in sides or "standin" not in sides:
            print(f"{kind}: missing a side in {workdir / 'logs.json'}")
            all_ok = False
            continue
        ok, lines = compare(kind, sides["sitl"], sides["standin"])
        all_ok &= ok
        print("\n".join(lines))
    print("MATCH" if all_ok else "DIFFERENCES (see above)")
    sys.exit(0 if all_ok else 1)


if __name__ == "__main__":
    main()
