#!/usr/bin/env python3
"""Parity test: the Rust stand-in engine against the Python reference (sim/standins.py).

Runs the same small fleet on each engine in turn, plays the same GCS script at both (arm,
modes, mission uploads with a dropped item, the refusals, RTL), records every message, then
compares: MAV_TYPE / autopilot, message types, rates and sizes, COMMAND_ACKs, mission
protocol, STATUSTEXTs, heartbeat mode sequences, and trajectories within a tolerance.

    .venv/bin/python sim/tests/standins_parity.py              # build release first
    .venv/bin/python sim/tests/standins_parity.py --tidewater  # also with real currents

--tidewater runs both engines in the Golden Gate with conditions: Python imports tidewater,
Rust asks `tidewater serve` (started here on --tidewater-port).
Exit status 0 = parity, 1 = a difference outside tolerance.
"""

import argparse
import json
import math
import os
import select
import socket
import subprocess
import sys
import time
import tomllib
from pathlib import Path

from pymavlink.dialects.v20 import ardupilotmega as mavlink

ROOT = Path(__file__).resolve().parents[2]
M = mavlink

# Trajectory tolerance: both engines run the same 10 Hz model, but their ticks are not in
# phase with each other or with the script's commands, so a command lands up to one tick
# (0.1 s) apart. Allow that much travel at the vehicle's speed, plus a small floor.
TICK_SLACK_S = 0.25
FLOOR_M = 3.0
RATE_TOL = 0.06  # stream rates within 6 %
# With conditions, Python samples tidewater every tick at the exact position; Rust batches one
# request a second and uses the latest sample. Allow one poll period at a 2 m/s current.
STALE_M = 2.0


def platforms():
    return tomllib.loads((ROOT / "sim" / "platforms.toml").read_text())


def fleet(base_port, at_golden_gate):
    """(key, platform, home) for the test fleet. Spawns west of the Golden Gate in water."""
    lat0, lon0 = (37.8000, -122.5300) if at_golden_gate else (37.5000, -123.2000)
    units = [
        ("plane_station", "mq9b", [lat0 + 0.05, lon0, 7600, 90]),
        ("plane_ground", "mq9b", [lat0 - 0.05, lon0, 5, 0]),
        ("boat", "seahunter", [lat0, lon0, 0, 90]),
        ("drifter", "buoy", [lat0 + 0.015, lon0 + 0.04, 0, 0]),  # in the Gate when at_golden_gate
        ("sub", "remus", [lat0 + 0.025, lon0 + 0.09, 0, 90]),  # west of Alcatraz
    ]
    cat = platforms()
    vehicles = []
    for i, (key, pid, home) in enumerate(units):
        spec = cat[pid]
        vehicles.append({"key": key, "platform": pid, "callsign": f"{spec['callsign']} {i + 1}", "uid": i + 1,
                         "sysid": i + 1, "port": base_port + i, "home": home, "sim": "standin",
                         "name": spec["name"], "spec": spec})
    return vehicles


def offset(lat, lon, north_m, east_m):
    return lat + north_m / 111_320.0, lon + east_m / (111_320.0 * math.cos(math.radians(lat)))


class Gcs:
    """One vehicle's GCS end: listens on the vehicle's bridge port, logs everything."""

    def __init__(self, v, t0):
        self.v = v
        self.sysid = v["sysid"]
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", v["port"]))
        self.sock.setblocking(False)
        self.remote = None
        self.mav = mavlink.MAVLink(None, srcSystem=255, srcComponent=190)
        self.rx = mavlink.MAVLink(None)
        self.rx.robust_parsing = True
        self.t0 = t0
        self.log = []  # (t, type, fields, size, seq)
        self.upload = None  # items being uploaded
        self.drop = set()  # seqs to ignore once (lost on the link)

    def now(self):
        return time.monotonic() - self.t0

    def send(self, msg):
        if self.remote is None:
            return
        self.sock.sendto(msg.pack(self.mav), self.remote)
        self.mav.seq = (self.mav.seq + 1) % 256

    def receive(self):
        while True:
            try:
                data, addr = self.sock.recvfrom(4096)
            except BlockingIOError:
                return
            self.remote = addr
            for m in self.rx.parse_buffer(data) or []:
                t = m.get_type()
                if t == "BAD_DATA":
                    continue
                d = m.to_dict()
                d.pop("mavpackettype", None)
                self.log.append((round(self.now(), 3), t, d, len(m.get_msgbuf()), m.get_srcSystem(), m.get_srcComponent()))
                if t == "MISSION_REQUEST_INT" and self.upload is not None:
                    if m.seq in self.drop:
                        self.drop.discard(m.seq)
                        continue
                    if m.seq < len(self.upload):
                        self.send(self.upload[m.seq])
                if t == "MISSION_ACK":
                    self.upload = None

    # ---- script actions ----
    def command(self, cmd, *p):
        p = list(p) + [0.0] * (7 - len(p))
        self.send(self.mav.command_long_encode(self.sysid, 1, cmd, 0, *p))

    def mission(self, items, drop=()):
        self.upload = [self.mav.mission_item_int_encode(self.sysid, 1, i, frame, cmd, 0, 1, p1, p2, p3, 0,
                                                        int(lat * 1e7), int(lon * 1e7), alt)
                       for i, (cmd, frame, lat, lon, alt, p1, p2, p3) in enumerate(items)]
        self.drop = set(drop)
        self.send(self.mav.mission_count_encode(self.sysid, 1, len(items)))


REL = M.MAV_FRAME_GLOBAL_RELATIVE_ALT_INT
WP, LOITER, TAKEOFF, JUMP = (M.MAV_CMD_NAV_WAYPOINT, M.MAV_CMD_NAV_LOITER_UNLIM, M.MAV_CMD_NAV_TAKEOFF,
                             M.MAV_CMD_DO_JUMP)


def script(vs):
    """(t seconds after the vehicle's first heartbeat, vehicle key, action(gcs)). Same for both engines."""
    h = {v["key"]: v["home"] for v in vs}
    s = []

    def at(t, key, fn):
        s.append((t, key, fn))

    for key in h:
        at(0.5, key, lambda g: g.send(g.mav.timesync_encode(0, 123456789)))
        at(0.7, key, lambda g: g.send(g.mav.param_request_read_encode(g.sysid, 1, b"Q_ENABLE", -1)))
        at(0.9, key, lambda g: g.send(g.mav.param_set_encode(g.sysid, 1, b"SYSID_TARGET", 7.0, M.MAV_PARAM_TYPE_REAL32)))
        at(1.1, key, lambda g: g.command(M.MAV_CMD_SET_MESSAGE_INTERVAL, M.MAVLINK_MSG_ID_GLOBAL_POSITION_INT, 5e5))
        at(1.3, key, lambda g: g.command(M.MAV_CMD_NAV_LAND))  # not supported by any stand-in
        at(1.5, key, lambda g: g.command(M.MAV_CMD_DO_SET_MODE, 1, 99))  # no such mode
        # addressed to another system: must be ignored
        at(1.7, key, lambda g: g.send(g.mav.command_long_encode(200, 1, M.MAV_CMD_COMPONENT_ARM_DISARM, 0, 1, 0, 0, 0, 0, 0, 0)))

    # airborne plane: no disarm in flight; loiter mission off to the east; later RTL
    lat, lon = h["plane_station"][:2]
    c = offset(lat, lon, 0, 8000)
    at(2, "plane_station", lambda g: g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 0))
    at(3, "plane_station", lambda g: g.mission([(WP, REL, lat, lon, 0, 0, 0, 0), (LOITER, REL, *c, 7600, 0, 0, 3500)]))
    at(5, "plane_station", lambda g: g.command(M.MAV_CMD_DO_SET_MISSION_CURRENT, 1))
    at(5.5, "plane_station", lambda g: g.command(M.MAV_CMD_DO_SET_MODE, 1, 10))
    at(45, "plane_station", lambda g: g.command(M.MAV_CMD_DO_SET_MODE, 1, 11))

    # grounded plane: takeoff mission, arm, AUTO
    lat, lon = h["plane_ground"][:2]
    w = offset(lat, lon, 3000, 0)
    at(2, "plane_ground", lambda g: g.mission([(WP, REL, lat, lon, 0, 0, 0, 0), (TAKEOFF, REL, 0, 0, 60, 15, 0, 0),
                                               (WP, REL, *w, 300, 0, 0, 0)]))
    at(4, "plane_ground", lambda g: g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1))
    at(4.5, "plane_ground", lambda g: g.command(M.MAV_CMD_DO_SET_MODE, 1, 10))
    at(50, "plane_ground", lambda g: g.command(M.MAV_CMD_DO_SET_MODE, 1, 12))

    # boat: patrol loop with a DO_JUMP and an item lost on the link; RTL; disarm at the end
    lat, lon = h["boat"][:2]
    p1, p2, p3 = offset(lat, lon, 0, 120), offset(lat, lon, 120, 120), offset(lat, lon, 120, 0)
    at(2, "boat", lambda g: g.mission([(WP, REL, lat, lon, 0, 0, 0, 0), (WP, REL, *p1, 0, 0, 0, 0),
                                       (WP, REL, *p2, 0, 0, 0, 0), (WP, REL, *p3, 0, 0, 0, 0),
                                       (JUMP, M.MAV_FRAME_MISSION, 0, 0, 0, 1, -1, 0)], drop={2}))
    at(6, "boat", lambda g: g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1))
    at(6.5, "boat", lambda g: g.command(M.MAV_CMD_DO_SET_MODE, 1, 10))
    at(70, "boat", lambda g: g.command(M.MAV_CMD_DO_SET_MODE, 1, 11))
    at(105, "boat", lambda g: g.command(M.MAV_CMD_DO_SET_MODE, 1, 4))
    at(110, "boat", lambda g: g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 0))

    # drifter: refuses missions (any type) and powered modes, accepts HOLD
    at(2, "drifter", lambda g: g.send(g.mav.mission_count_encode(g.sysid, 1, 3)))
    at(2.5, "drifter", lambda g: g.send(g.mav.mission_count_encode(g.sysid, 1, 3, M.MAV_MISSION_TYPE_FENCE)))
    at(3, "drifter", lambda g: g.command(M.MAV_CMD_DO_SET_MODE, 1, 10))
    at(3.5, "drifter", lambda g: g.command(M.MAV_CMD_DO_SET_MODE, 1, 4))
    at(4, "drifter", lambda g: g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1))

    # sub: dive to 40 m on the way to a waypoint; no GPS once below 1 m; POSHOLD
    lat, lon = h["sub"][:2]
    w = offset(lat, lon, 0, 150)
    at(2, "sub", lambda g: g.mission([(WP, REL, lat, lon, 0, 0, 0, 0), (WP, REL, *w, -40, 0, 0, 0)]))
    at(4, "sub", lambda g: g.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1))
    at(4.5, "sub", lambda g: g.command(M.MAV_CMD_DO_SET_MODE, 1, 3))
    at(100, "sub", lambda g: g.command(M.MAV_CMD_DO_SET_MODE, 1, 16))
    return sorted(s, key=lambda x: x[0])


def run_engine(engine, vs, duration, env_extra, workdir):
    cfg = workdir / "standins.json"
    cfg.write_text(json.dumps({"bridge_host": "127.0.0.1", "vehicles": [{k: v[k] for k in v if k != "key"} for v in vs]}))
    if engine == "python":
        cmd = [sys.executable, str(ROOT / "sim" / "standins.py"), str(cfg)]
    else:
        cmd = [str(ROOT / "target" / "release" / "murmur-standins"), str(cfg)]
    env = {k: val for k, val in os.environ.items()
           if k not in ("MURMUR_SCENARIO", "MURMUR_AT", "MURMUR_RATE", "MURMUR_TIDEWATER_URL")}
    env.update(env_extra.get(engine, {}))
    t0 = time.monotonic()
    gcs = {v["key"]: Gcs(v, t0) for v in vs}
    out = open(workdir / f"{engine}.log", "w")
    proc = subprocess.Popen(cmd, stdout=out, stderr=subprocess.STDOUT, env=env)
    try:
        # sync: every vehicle has sent its first heartbeat
        deadline = time.monotonic() + 30
        while not all(any(e[1] == "HEARTBEAT" for e in g.log) for g in gcs.values()):
            if time.monotonic() > deadline or proc.poll() is not None:
                raise SystemExit(f"{engine}: vehicles did not start; see {workdir / (engine + '.log')}")
            select.select([g.sock for g in gcs.values()], [], [], 0.05)
            for g in gcs.values():
                g.receive()
        # each vehicle's clock starts at its first heartbeat (its first tick): the Rust engine
        # staggers vehicles' ticks within 0.1 s, the Python one runs them in lockstep
        sync = {k: next(e[0] for e in g.log if e[1] == "HEARTBEAT") for k, g in gcs.items()}
        pending = sorted(((sync[key] + t, key, fn) for t, key, fn in script(vs)), key=lambda x: x[0])
        end = max(sync.values()) + duration
        while (now := time.monotonic() - t0) < end:
            while pending and pending[0][0] <= now:
                _, key, fn = pending.pop(0)
                fn(gcs[key])
            select.select([g.sock for g in gcs.values()], [], [], 0.01)
            for g in gcs.values():
                g.receive()
    finally:
        proc.terminate()
        try:
            proc.wait(5)
        except subprocess.TimeoutExpired:
            proc.kill()
        for g in gcs.values():
            g.sock.close()
    return {k: [(round(t - sync[k], 3), *rest) for (t, *rest) in g.log] for k, g in gcs.items()}


# ---- comparison ---------------------------------------------------------------------------

EVENTS = ("COMMAND_ACK", "MISSION_ACK", "MISSION_REQUEST_INT", "STATUSTEXT", "PARAM_VALUE", "TIMESYNC",
          "MISSION_ITEM_REACHED")
STREAMS = ("HEARTBEAT", "ATTITUDE", "VFR_HUD", "GLOBAL_POSITION_INT", "SYS_STATUS", "GPS_RAW_INT",
           "EKF_STATUS_REPORT")


def events(log, t):
    return [e for e in log if e[1] == t]


def summary(log, duration):
    out = {}
    for t in STREAMS:
        es = [e for e in events(log, t) if 5 <= e[0] < duration - 1]
        out[t] = len(es) / (duration - 6)
    return out


def constants(log, t):
    es = events(log, t)
    if not es:
        return {}
    first = es[0][2]
    return {k: first[k] for k in first if all(e[2][k] == first[k] for e in es)}


def interp(track, t):
    """track: [(t, lat, lon, alt)] sorted. Linear in time."""
    for a, b in zip(track, track[1:]):
        if a[0] <= t <= b[0]:
            f = 0 if b[0] == a[0] else (t - a[0]) / (b[0] - a[0])
            return tuple(a[i] + f * (b[i] - a[i]) for i in (1, 2, 3))
    return None


def horiz_m(a, b):
    dn = (a[0] - b[0]) * 111_320.0
    de = (a[1] - b[1]) * 111_320.0 * math.cos(math.radians(a[0]))
    return math.hypot(dn, de)


def compare(py, rs, vs, duration, tidewater=False):
    ok = True
    lines = []

    def check(cond, what):
        nonlocal ok
        if not cond:
            ok = False
        lines.append(("  ok   " if cond else "  FAIL ") + what)

    for v in vs:
        k = v["key"]
        a, b = py[k], rs[k]
        lines.append(f"{k} ({v['spec']['name']}, behavior {v['spec']['behavior']})")
        hb_a, hb_b = events(a, "HEARTBEAT"), events(b, "HEARTBEAT")
        ident = lambda hb: sorted({(e[2]["type"], e[2]["autopilot"], e[4], e[5]) for e in hb})  # noqa: E731
        check(ident(hb_a) == ident(hb_b), f"MAV_TYPE/autopilot/sysid/compid {ident(hb_a)} vs {ident(hb_b)}")
        modes = lambda hb: [m for i, m in enumerate([(e[2]["custom_mode"], e[2]["base_mode"]) for e in hb])  # noqa: E731
                            if i == 0 or m != (hb[i - 1][2]["custom_mode"], hb[i - 1][2]["base_mode"])]
        check(modes(hb_a) == modes(hb_b), f"heartbeat mode/armed sequence {modes(hb_a)} vs {modes(hb_b)}")
        check({e[1] for e in a} == {e[1] for e in b},
              f"message types {sorted({e[1] for e in a} ^ {e[1] for e in b}) or 'same'}")
        ra, rb = summary(a, duration), summary(b, duration)
        rate_ok = all(abs(ra[t] - rb[t]) <= RATE_TOL * max(ra[t], 0.1) for t in STREAMS)
        check(rate_ok, "stream rates (Hz) " + ", ".join(f"{t} {ra[t]:.2f}/{rb[t]:.2f}" for t in STREAMS))
        sizes = lambda log, t: sorted({e[3] for e in events(log, t)})  # noqa: E731
        size_diff = {t: (sizes(a, t), sizes(b, t)) for t in STREAMS + EVENTS if sizes(a, t) != sizes(b, t)}
        check(not size_diff, f"wire sizes per type {size_diff or 'identical'}")
        # fields that never change in a run (flags, bitmaps, fixed values) must be the same
        const_diff = {}
        for t in STREAMS + ("MISSION_CURRENT",):
            ca, cb = constants(a, t), constants(b, t)
            if ca != cb:
                const_diff[t] = {f: (ca.get(f), cb.get(f)) for f in set(ca) | set(cb) if ca.get(f) != cb.get(f)}
        check(not const_diff, f"constant fields {const_diff or 'identical'}")
        acks = lambda log: [(e[2]["command"], e[2]["result"]) for e in events(log, "COMMAND_ACK")]  # noqa: E731
        check(acks(a) == acks(b), f"COMMAND_ACKs {acks(a)}" + ("" if acks(a) == acks(b) else f" vs {acks(b)}"))
        macks = lambda log: [(e[2]["type"], e[2]["mission_type"], e[2]["target_system"], e[2]["target_component"])  # noqa: E731
                             for e in events(log, "MISSION_ACK")]
        check(macks(a) == macks(b), f"MISSION_ACKs {macks(a)}" + ("" if macks(a) == macks(b) else f" vs {macks(b)}"))
        reqs = lambda log: [e[2]["seq"] for e in events(log, "MISSION_REQUEST_INT")]  # noqa: E731
        check(reqs(a) == reqs(b), f"MISSION_REQUEST_INT seqs {reqs(a)}" + ("" if reqs(a) == reqs(b) else f" vs {reqs(b)}"))
        texts = lambda log: [(e[2]["severity"], e[2]["text"]) for e in events(log, "STATUSTEXT")]  # noqa: E731
        ta, tb = texts(a), texts(b)
        check(ta == tb, f"STATUSTEXTs {len(ta)}: {ta[:4]}{'...' if len(ta) > 4 else ''}" + ("" if ta == tb else f" vs {tb}"))
        pv = lambda log: [(e[2]["param_id"], e[2]["param_value"], e[2]["param_type"]) for e in events(log, "PARAM_VALUE")]  # noqa: E731
        check(pv(a) == pv(b), f"PARAM_VALUEs {pv(a)}")
        ts = lambda log: [(e[2]["tc1"] > 0, e[2]["ts1"]) for e in events(log, "TIMESYNC")]  # noqa: E731
        check(ts(a) == ts(b), f"TIMESYNC replies {ts(a)}")
        reached = lambda log: [e[2]["seq"] for e in events(log, "MISSION_ITEM_REACHED")]  # noqa: E731
        check(reached(a) == reached(b), f"MISSION_ITEM_REACHED {reached(a)}" + ("" if reached(a) == reached(b) else f" vs {reached(b)}"))
        gps = lambda log: sorted({(e[2]["fix_type"], e[2]["satellites_visible"]) for e in events(log, "GPS_RAW_INT")})  # noqa: E731
        check(gps(a) == gps(b), f"GPS fix/sats seen {gps(a)}")
        batt = lambda log: sorted({e[2]["battery_remaining"] for e in events(log, "SYS_STATUS")})  # noqa: E731
        check(batt(a) == batt(b), f"battery % seen {batt(a)} vs {batt(b)}")

        # trajectory: Rust track interpolated at Python's sample times
        tr = lambda log: [(e[0], e[2]["lat"] / 1e7, e[2]["lon"] / 1e7, e[2]["alt"] / 1000.0)  # noqa: E731
                          for e in events(log, "GLOBAL_POSITION_INT")]
        ta_, tb_ = tr(a), tr(b)
        speed = max(v["spec"].get("cruise_mps", 0), 0.5)
        tol = FLOOR_M + speed * TICK_SLACK_S
        if v["spec"]["behavior"] == "drifter":
            tol = FLOOR_M + 0.02 * math.sqrt(duration * 10) * 0.1 * 6  # random jitter: 6 sigma of a 10 Hz walk
        worst_h = worst_v = 0.0
        n = 0
        for t, lat, lon, alt in ta_:
            p = interp(tb_, t)
            if p is None:
                continue
            n += 1
            worst_h = max(worst_h, horiz_m((lat, lon), p[:2]))
            worst_v = max(worst_v, abs(alt - p[2]))
        if tidewater and v["spec"]["behavior"] in ("boat", "drifter", "sub"):
            tol += STALE_M
        climb = v["spec"].get("climb_mps", 0)
        vtol = 0.5 + climb * TICK_SLACK_S
        span = horiz_m(ta_[0][1:3], ta_[-1][1:3]) if ta_ else 0
        check(n > 0 and worst_h <= tol and worst_v <= vtol,
              f"trajectory over {n} points ({span:.0f} m start to end): max horizontal {worst_h:.2f} m "
              f"(tol {tol:.1f}), max vertical {worst_v:.2f} m (tol {vtol:.2f})")
    return ok, lines


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--base-port", type=int, default=15560)
    ap.add_argument("--duration", type=float, default=120.0, help="seconds per engine after sync")
    ap.add_argument("--tidewater", action="store_true", help="Golden Gate with real currents, depth, grounding")
    ap.add_argument("--tidewater-port", type=int, default=8866)
    ap.add_argument("--scenario", default=str(Path.home() / "tidewater" / "scenarios" / "golden-gate-2026-06.toml"))
    ap.add_argument("--at", default="2026-06-14T10:00Z", help="scenario time for --tidewater")
    ap.add_argument("--workdir", default=str(ROOT / "sim" / "run" / "parity"))
    ap.add_argument("--compare-only", action="store_true", help="re-compare the logs of the last run")
    args = ap.parse_args()

    workdir = Path(args.workdir)
    workdir.mkdir(parents=True, exist_ok=True)
    binary = ROOT / "target" / "release" / "murmur-standins"
    if not binary.exists():
        sys.exit(f"missing {binary}; run: cargo build --release")
    vs = fleet(args.base_port, args.tidewater)
    if args.compare_only:
        logs = {e: json.loads((workdir / f"{e}.json").read_text()) for e in ("python", "rust")}
    else:
        logs = run_both(args, vs, workdir)
    ok, lines = compare(logs["python"], logs["rust"], vs, args.duration, args.tidewater)
    print("\n".join(lines))
    print("PARITY" if ok else "DIFFERENCES (see above)")
    sys.exit(0 if ok else 1)


def run_both(args, vs, workdir):
    env = {}
    serve = None
    if args.tidewater:
        scenario = args.scenario
        env = {"python": {"MURMUR_SCENARIO": scenario, "MURMUR_AT": args.at},
               "rust": {"MURMUR_TIDEWATER_URL": f"http://127.0.0.1:{args.tidewater_port}", "MURMUR_AT": args.at}}
        serve = subprocess.Popen([str(Path(sys.executable).parent / "tidewater"), "serve", scenario,
                                  "--port", str(args.tidewater_port)],
                                 stdout=open(workdir / "tidewater.log", "w"), stderr=subprocess.STDOUT)
        deadline = time.monotonic() + 60
        while True:
            try:
                socket.create_connection(("127.0.0.1", args.tidewater_port), timeout=1).close()
                break
            except OSError:
                if time.monotonic() > deadline or serve.poll() is not None:
                    sys.exit(f"tidewater serve did not start; see {workdir / 'tidewater.log'}")
                time.sleep(0.5)
    try:
        logs = {}
        for engine in ("python", "rust"):
            print(f"running {engine} engine for {args.duration:.0f} s ...", flush=True)
            logs[engine] = run_engine(engine, vs, args.duration, env, workdir)
            (workdir / f"{engine}.json").write_text(json.dumps(logs[engine]))
        return logs
    finally:
        if serve:
            serve.terminate()
            serve.wait(5)


if __name__ == "__main__":
    main()
