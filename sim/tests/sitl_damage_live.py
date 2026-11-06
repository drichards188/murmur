#!/usr/bin/env python3
"""Live check (bd murmur-y85j.8): SITL vehicles act out a strike's damage (sim/sitl_damage.py).

Claims: a SITL vehicle hit for mobility loses its propulsion through SITL's own failure injection
and comes down or to a stop by its physics (a plane glides down, a copter falls, a UGV and a boat
stop, a sub rises on its buoyancy) while its autopilot flies on unaware; each says so over its own
link, so the bridge's `damage` fact, event and alert follow as from a stand-in; a firepower hit
only reports; a destroyed vehicle goes silent (the bridge's lost-link alert, no error) and its
stopped process still exits at shutdown.

One run per set: `air` (a Puma: arduplane, plane; an X10: arducopter, quad) or `surface` (a
UGV: ardurover, rover-skid; a Corsair: ardurover, motorboat; a BlueROV: ardusub, vectored),
started by sim/swarm.py with the watcher following its own damage file, and the bridge
(target/release/murmur-bridge, or MURMUR_BRIDGE_BIN) talking to them directly (no link
emulator). The vehicles are driven from their SERIAL1 (TCP 5762 + 10 x instance), which also
records their telemetry; the damage records are appended to the file by hand, as the contacts
process would: mobility on every vehicle, then firepower on the second, then the first
destroyed. Telemetry, the bridge's view and a summary go to logs/stage8/<set>/.

    .venv/bin/python sim/tests/sitl_damage_live.py [--set air|surface]

Ports, clear of the default stack, the situation block and the evaluation's: vehicles UDP
21560+ (the bridge binds 32 from there), bridge HTTP 9865 (console origin 9873, nothing served
there), SITL instances 200+ (TCP 7760+). Needs the ArduPilot SITL binaries and the bridge's
release binary.
"""

import argparse
import json
import os
import signal
import socket
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

from pymavlink import mavutil

REPO = Path(__file__).resolve().parents[2]
BASE_PORT = 21560
HTTP_PORT = 9865
WEB_PORT = 9873
INSTANCE_BASE = 200
GCS_SYSID = 253  # the test's own ground station on SERIAL1 (not the bridge's 255)
RECORD = {"GLOBAL_POSITION_INT", "VFR_HUD", "HEARTBEAT", "STATUSTEXT", "SERVO_OUTPUT_RAW",
          "EKF_STATUS_REPORT", "PARAM_VALUE", "SYS_STATUS"}
M = mavutil.mavlink
FLEETS = {
    "air": """
[[unit]]
platform = "puma"
spawn = [37.8040, -122.4660, 10, 270]

[[unit]]
platform = "x10"
spawn = [37.8030, -122.4600, 10, 0]
""",
    "surface": """
[[unit]]
platform = "ugv_recon"
spawn = [37.8030, -122.4600, 10, 0]

[[unit]]
platform = "corsair"
spawn = [37.8120, -122.4700, 0, 0]

[[unit]]
platform = "bluerov"
spawn = [37.8150, -122.4600, 0, 0]
""",
}


def free(port, kind):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM if kind == "udp" else socket.SOCK_STREAM)
    try:
        s.bind(("127.0.0.1", port))
        return True
    except OSError:
        return False
    finally:
        s.close()


def http_json(path, timeout=3.0):
    with urllib.request.urlopen(f"http://127.0.0.1:{HTTP_PORT}{path}", timeout=timeout) as r:
        return json.loads(r.read())


def offset(lat, lon, north_m, east_m):
    import math
    return lat + north_m / 111_320.0, lon + east_m / (111_320.0 * math.cos(math.radians(lat)))


class Vehicle:
    """One vehicle's SERIAL1: drives it and records its telemetry."""

    def __init__(self, entry, instance, t0, out):
        self.callsign, self.uid, self.platform = entry["callsign"], entry["uid"], entry["platform"]
        self.home = entry["home"]
        self.instance, self.t0 = instance, t0
        self.conn = None
        self.log = open(out / f"telemetry-{self.callsign.replace(' ', '_')}.jsonl", "w")
        self.hb = self.pos = self.hud = None
        self.texts = []

    def connect(self, patience_s=90):
        deadline = time.monotonic() + patience_s
        while True:
            try:
                self.conn = mavutil.mavlink_connection(f"tcp:127.0.0.1:{5762 + 10 * self.instance}",
                                                       source_system=GCS_SYSID, source_component=190, retries=0)
                break
            except OSError:
                if time.monotonic() > deadline:
                    raise
                time.sleep(1)
        if self.conn.wait_heartbeat(timeout=60) is None:
            raise TimeoutError(f"{self.callsign}: no heartbeat")
        self.sysid = self.conn.target_system
        self.conn.mav.request_data_stream_send(self.sysid, 1, M.MAV_DATA_STREAM_ALL, 4, 1)

    def pump(self):
        while True:
            m = self.conn.recv_match(blocking=False)
            if m is None:
                return
            t = m.get_type()
            if t not in RECORD:
                continue
            d = m.to_dict()
            d.pop("mavpackettype", None)
            rec = {"t": round(time.monotonic() - self.t0, 2), "msg": t, "src": [m.get_srcSystem(), m.get_srcComponent()]}
            if t == "HEARTBEAT":
                if m.get_srcSystem() != self.sysid or m.get_srcComponent() != 1:
                    continue
                self.hb = d
            elif t == "GLOBAL_POSITION_INT":
                self.pos = d
            elif t == "VFR_HUD":
                self.hud = d
            elif t == "STATUSTEXT":
                self.texts.append((rec["t"], m.get_srcComponent(), d["text"]))
            rec.update(d)
            self.log.write(json.dumps(rec) + "\n")

    @property
    def armed(self):
        return bool(self.hb and self.hb["base_mode"] & M.MAV_MODE_FLAG_SAFETY_ARMED)

    @property
    def alt(self):
        return self.pos["relative_alt"] / 1000.0 if self.pos else None

    @property
    def speed(self):
        return self.hud["groundspeed"] if self.hud else None

    def set_param(self, name, value):
        self.conn.mav.param_set_send(self.sysid, 1, name.encode(), float(value), M.MAV_PARAM_TYPE_REAL32)

    def command(self, cmd, *p):
        p = list(p) + [0.0] * (7 - len(p))
        self.conn.mav.command_long_send(self.sysid, 1, cmd, 0, *p)

    def mode(self, number):
        self.command(M.MAV_CMD_DO_SET_MODE, M.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED, number)

    def arm(self):
        self.command(M.MAV_CMD_COMPONENT_ARM_DISARM, 1)

    def goto(self, north_m, east_m, alt):
        lat, lon = offset(self.home[0], self.home[1], north_m, east_m)
        self.conn.mav.set_position_target_global_int_send(
            0, self.sysid, 1, M.MAV_FRAME_GLOBAL_RELATIVE_ALT_INT, 0b0000_1111_1111_1000,
            int(lat * 1e7), int(lon * 1e7), alt, 0, 0, 0, 0, 0, 0, 0, 0)

    def state(self):
        hud = self.hud or {}
        return {"alt_m": self.alt, "airspeed": hud.get("airspeed"), "groundspeed": hud.get("groundspeed"),
                "climb": hud.get("climb"), "throttle": hud.get("throttle"), "armed": self.armed,
                "mode": self.hb["custom_mode"] if self.hb else None}


# ---- what each set does before the hit: (mode, after arming, ready?) per platform -------------
# Mode numbers are the firmware's own: Plane TAKEOFF 13, Copter GUIDED 4, Rover GUIDED 15,
# Sub GUIDED 4.
def takeoff_plane(v):
    v.set_param("TKOFF_ALT", 150)
    v.set_param("TKOFF_DIST", 400)


PLAN = {
    "puma": (13, takeoff_plane, None, lambda v: (v.alt or 0) > 140),
    "x10": (4, None, lambda v: v.command(M.MAV_CMD_NAV_TAKEOFF, 0, 0, 0, 0, 0, 0, 40),
            lambda v: (v.alt or 0) > 37),
    "ugv_recon": (15, None, lambda v: v.goto(1500, 0, 0), lambda v: (v.speed or 0) > 3.5),
    "corsair": (15, None, lambda v: v.goto(2500, 0, 0), lambda v: (v.speed or 0) > 6),
    "bluerov": (4, None, lambda v: v.goto(0, 0, -12), lambda v: (v.alt or 0) < -9),
}


def bridge_vehicles():
    try:
        s = http_json("/api/state")
    except Exception as e:  # noqa: BLE001
        return {"error": str(e)}
    keep = ("uid", "callsign", "alt", "damage", "stale", "link", "armed", "mode", "speed", "statustext")
    return {str(v.get("uid")): {k: x for k, x in v.items() if any(w in k for w in keep)} for v in s.get("vehicles", [])}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--set", choices=sorted(FLEETS), default="air")
    args = ap.parse_args()
    out = REPO / "logs" / "stage8" / args.set
    out.mkdir(parents=True, exist_ok=True)
    n = FLEETS[args.set].count("[[unit]]")
    held = [p for p in range(BASE_PORT, BASE_PORT + 32) if not free(p, "udp")]
    held += [p for p in (HTTP_PORT, *[5760 + 10 * (INSTANCE_BASE + i) + k for i in range(n) for k in range(4)])
             if not free(p, "tcp")]
    if held:
        sys.exit(f"ports in use: {held}")
    bridge_bin = os.environ.get("MURMUR_BRIDGE_BIN") or str(REPO / "target" / "release" / "murmur-bridge")
    fleet = out / "fleet.toml"
    fleet.write_text(f'title = "stage 8 live check: {args.set}"\n' + FLEETS[args.set])
    damage = out / "damage.jsonl"
    damage.write_text("")
    env = {**os.environ, "MURMUR_ROOT": str(REPO), "MURMUR_HTTP_PORT": str(HTTP_PORT),
           "MURMUR_BASE_PORT": str(BASE_PORT), "MURMUR_WEB_PORT": str(WEB_PORT),
           "MURMUR_DAMAGE_FILE": str(damage), "MURMUR_SITL_INSTANCE_BASE": str(INSTANCE_BASE),
           "PYTHONUNBUFFERED": "1"}
    py = sys.executable
    swarm_args = [py, str(REPO / "sim" / "swarm.py"), "--fleet", str(fleet), "--base-port", str(BASE_PORT),
                  "--instance-base", str(INSTANCE_BASE)]
    subprocess.run([*swarm_args, "--manifest-only"], env=env, check=True)
    manifest = json.loads((REPO / "sim" / "run" / "fleet.json").read_text())["vehicles"]
    procs = []
    t0 = time.monotonic()
    summary = {"set": args.set, "phases": []}

    def say(*a):
        print(f"[{time.monotonic() - t0:6.1f}]", *a, flush=True)

    def hit(v, state):
        with open(damage, "a") as f:
            f.write(json.dumps({"uid": v.uid, "state": state, "t": time.time(),
                                "cause": f"stage 8 live check ({v.platform})"}) + "\n")
        say(f"damage record: {v.callsign} (uid {v.uid}) {state}")

    try:
        procs.append(subprocess.Popen([bridge_bin], cwd=REPO, stdout=open(out / "bridge.log", "w"),
                                      stderr=subprocess.STDOUT, start_new_session=True,
                                      env={**env, "MURMUR_FLEET_MANIFEST": str(REPO / "sim" / "run" / "fleet.json")}))
        procs.append(subprocess.Popen(swarm_args, cwd=REPO, stdout=open(out / "swarm.log", "w"),
                                      stderr=subprocess.STDOUT, env=env, start_new_session=True))
        fleet_v = [Vehicle(e, INSTANCE_BASE + i, t0, out) for i, e in enumerate(manifest)]
        for v in fleet_v:
            v.connect()
            if PLAN[v.platform][1]:
                PLAN[v.platform][1](v)
        say("connected; arming")
        snaps = open(out / "bridge-state.jsonl", "w")
        last_snap = [0.0]

        def run_for(seconds, cond=None):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                for v in fleet_v:
                    v.pump()
                if time.monotonic() - last_snap[0] >= 2.0:
                    last_snap[0] = time.monotonic()
                    snaps.write(json.dumps({"t": round(time.monotonic() - t0, 1), "vehicles": bridge_vehicles()}) + "\n")
                    snaps.flush()
                if cond and cond():
                    return True
                time.sleep(0.05)
            return False

        # arm and start each, retrying while the EKF settles
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline and not all(v.armed for v in fleet_v):
            for v in fleet_v:
                if not v.armed:
                    v.mode(PLAN[v.platform][0])
                    v.arm()
            run_for(1.0)
            for v in fleet_v:
                if v.armed and PLAN[v.platform][2]:
                    PLAN[v.platform][2](v)
            run_for(2.0)
        say("armed:", {v.callsign: v.armed for v in fleet_v})
        # into its mode (one refused before its position estimate was ready is asked again) and away
        deadline = time.monotonic() + 240
        ok = False
        while time.monotonic() < deadline and not ok:
            for v in fleet_v:
                if v.hb and v.hb["custom_mode"] != PLAN[v.platform][0]:
                    v.mode(PLAN[v.platform][0])
                if PLAN[v.platform][2] and not PLAN[v.platform][3](v):
                    PLAN[v.platform][2](v)
            ok = run_for(3.0, lambda: all(PLAN[v.platform][3](v) for v in fleet_v))
        say(f"under way: {ok}:", {v.callsign: v.state() for v in fleet_v})
        run_for(10)
        before = {"t": round(time.monotonic() - t0, 1), "vehicles": {v.callsign: v.state() for v in fleet_v},
                  "bridge": bridge_vehicles()}
        summary["phases"].append({"before": before})
        say("before:", json.dumps(before["vehicles"]))

        for v in fleet_v:
            hit(v, "mobility")
        hit_t = time.monotonic() - t0
        samples = []
        for _ in range(30):
            run_for(2.0)
            samples.append({"t": round(time.monotonic() - t0, 1), **{v.callsign: v.state() for v in fleet_v}})
        after = {"t": round(time.monotonic() - t0, 1), "vehicles": {v.callsign: v.state() for v in fleet_v},
                 "bridge": bridge_vehicles(), "texts": {v.callsign: v.texts[-6:] for v in fleet_v}}
        summary["phases"].append({"hit_t": round(hit_t, 1), "after_mobility": after, "samples": samples})
        say("after mobility:", json.dumps(after["vehicles"]))

        first, second = fleet_v[0], fleet_v[1]
        hit(second, "firepower")
        run_for(8)
        hit(first, "destroyed")
        destroyed_t = time.monotonic() - t0
        run_for(40)
        first.log.flush()
        last = max((json.loads(line)["t"] for line in open(first.log.name)), default=None)
        end = {"t": round(time.monotonic() - t0, 1), "destroyed": first.callsign, "destroyed_t": round(destroyed_t, 1),
               "destroyed_last_telemetry_t": last, "vehicles": {v.callsign: v.state() for v in fleet_v[1:]},
               "bridge": bridge_vehicles(), "texts": {v.callsign: v.texts[-6:] for v in fleet_v[1:]}}
        try:
            end["inbox_alerts"] = [a for a in http_json("/api/inbox").get("alerts", [])
                                   if a.get("kind") in ("damaged", "lost_link")]
        except Exception as e:  # noqa: BLE001
            end["inbox_alerts"] = str(e)
        summary["phases"].append({"end": end})
        say("end:", json.dumps({k: end[k] for k in ("destroyed", "destroyed_t", "destroyed_last_telemetry_t",
                                                     "vehicles", "inbox_alerts")}))
    finally:
        for p in reversed(procs):
            try:
                os.killpg(p.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        for p in procs:
            try:
                p.wait(timeout=20)
            except subprocess.TimeoutExpired:
                os.killpg(p.pid, signal.SIGKILL)
        # every SITL instance gone, the stopped one included
        time.sleep(3)
        left = [5760 + 10 * (INSTANCE_BASE + i) for i in range(n) if not free(5760 + 10 * (INSTANCE_BASE + i), "tcp")]
        summary["sitl_ports_held_after_stop"] = left
        say("SITL ports held after stop:", left)
        (out / "summary.json").write_text(json.dumps(summary, indent=1))


if __name__ == "__main__":
    main()
