#!/usr/bin/env python3
"""The drifting buoy stand-in against the catalog and its Iridium-style expectations.

The buoy has no autopilot to compare with (sim/platforms.toml: protocol "telemetry", "no
commands exist"), so this checks the stand-in against what the catalog promises: it reports
itself as ArduRover's surface boat (MAV_TYPE_SURFACE_BOAT, the only MAVLink identity a
drifter can have), streams nothing until asked and then the Iridium-class plan the bridge
asks for (HIGH_LATENCY2 every 5 s, a few dozen bytes a second), refuses every mission upload
(MAV_MISSION_UNSUPPORTED) and every powered mode (AUTO, RTL, GUIDED, LOITER: "Flight mode
change failed"), takes HOLD and MANUAL, answers parameter reads with the Iridium profile's
lost-link timer (sim/links.toml), and does not move on its own. With `--tidewater` it is
started in the Golden Gate with real conditions and must go where the current goes.

    cargo build --release -p murmur-standins
    .venv/bin/python sim/tests/standins_drifter.py
    .venv/bin/python sim/tests/standins_drifter.py --tidewater http://127.0.0.1:8879 --at 2026-06-14T10:00Z

UDP 18865 (and 18866 with tidewater). Exit 0 = every expectation held.
"""

import argparse
import json
import math
import os
import signal
import subprocess
import sys
import time
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

from pymavlink.dialects.v20 import ardupilotmega as mavlink

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "sim"))
sys.path.insert(0, str(ROOT / "sim" / "tests"))
import swarm  # noqa: E402
from standins_vs_sitl_plane_boat_sub import Gcs, horiz_m  # noqa: E402

M = mavlink
PLATFORM = "buoy"
STILL = (37.8000, -122.5300, 0.0, 0.0)  # west of the Gate (no conditions: it must not move)
GATE = (37.8150, -122.4900, 0.0, 0.0)  # in the Golden Gate: a tidal current
ROVER = dict(MANUAL=0, HOLD=4, LOITER=5, AUTO=10, RTL=11, GUIDED=15)
CUSTOM = M.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED


def start(port, home, workdir, env_extra=None):
    spec = swarm.load_platforms()[PLATFORM]
    params = swarm.lost_link_params(PLATFORM, spec, swarm.STANDIN_FIRMWARE[spec["behavior"]])
    v = {"platform": PLATFORM, "callsign": f"{spec['callsign']} 1", "uid": 1, "sysid": 1, "port": port,
         "home": list(home), "sim": "standin", "name": spec["name"], "spec": spec, "params": params}
    workdir.mkdir(parents=True, exist_ok=True)
    cfg = workdir / "standins.json"
    cfg.write_text(json.dumps({"bridge_host": "127.0.0.1", "vehicles": [v]}))
    env = {k: val for k, val in os.environ.items() if not k.startswith("MURMUR_")}
    env["MURMUR_STANDINS_STATS"] = "0"
    env.update(env_extra or {})
    return subprocess.Popen([str(ROOT / "target" / "release" / "murmur-standins"), str(cfg)], env=env,
                            stdout=open(workdir / "standin.log", "w"), stderr=subprocess.STDOUT, start_new_session=True)


def tw_sample(url, t, lat, lon):
    req = urllib.request.Request(url.rstrip("/") + "/v1/sample", data=json.dumps({"t": t, "points": [[lat, lon, None]]}).encode(),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.loads(r.read())["samples"][0]


def unix(at):
    d = datetime.fromisoformat(at.replace("Z", "+00:00"))
    return (d if d.tzinfo else d.replace(tzinfo=timezone.utc)).timestamp()


class Report:
    def __init__(self):
        self.ok = True
        self.lines = []

    def check(self, cond, what):
        self.ok &= bool(cond)
        self.lines.append(("  ok   " if cond else "  FAIL ") + what)

    def info(self, what):
        self.lines.append("  info " + what)


def catalog(r):
    spec = swarm.load_platforms()[PLATFORM]
    r.lines.append("catalog")
    r.check(spec["behavior"] == "drifter" and spec["sim"] == "standin", f"behavior {spec['behavior']}, sim {spec['sim']}")
    r.check(spec["cruise_mps"] == 0 and spec["max_mps"] == 0, f"no propulsion: cruise {spec['cruise_mps']}, max {spec['max_mps']} m/s")
    r.check(spec["protocol"] == "telemetry", f"protocol {spec['protocol']!r}: {spec.get('protocol_note')}")
    r.check(spec["link"] == "satcom" and spec.get("space_class") == "iridium",
            f"link {spec['link']} on {spec.get('space_class')} (the bridge's Iridium-class plan: HIGH_LATENCY2 every 5 s)")
    return spec


def wire(r, g, timeout_s):
    r.lines.append("on the wire (no conditions)")
    hb = [e for e in g.log if e[1] == "HEARTBEAT"]
    r.check(hb and all(e[2]["type"] == M.MAV_TYPE_SURFACE_BOAT and e[2]["autopilot"] == M.MAV_AUTOPILOT_ARDUPILOTMEGA for e in hb),
            f"HEARTBEAT MAV_TYPE_SURFACE_BOAT / ArduPilotMega: {sorted({(e[2]['type'], e[2]['autopilot']) for e in hb})}")
    r.check(g.mode == ROVER["MANUAL"] and not g.armed, f"boots passive: mode {g.mode}, armed {g.armed}")
    # nothing streams until asked: 10 s of silence but heartbeats
    quiet = {e[1] for e in g.log if e[0] >= g.now() - 10.0}
    r.check(quiet <= {"HEARTBEAT"}, f"streams nothing unasked: {sorted(quiet)}")
    # the bridge's Iridium-class plan
    ack = g.command(M.MAV_CMD_SET_MESSAGE_INTERVAL, M.MAVLINK_MSG_ID_HIGH_LATENCY2, 5e6)
    r.check(ack == M.MAV_RESULT_ACCEPTED, f"SET_MESSAGE_INTERVAL HIGH_LATENCY2 every 5 s: ack {ack}")
    t0, i0 = g.now(), len(g.log)
    g.sleep(31.0)
    window = g.log[i0:]
    hl2 = [e for e in window if e[1] == "HIGH_LATENCY2"]
    r.check(5 <= len(hl2) <= 7, f"HIGH_LATENCY2 {len(hl2)} in 31 s (0.2 Hz)")
    types = {e[1] for e in window}
    r.check(types <= {"HEARTBEAT", "HIGH_LATENCY2", "COMMAND_ACK"}, f"only HEARTBEAT and HIGH_LATENCY2 stream: {sorted(types)}")
    bps = sum(e[3] for e in window) / (g.now() - t0)
    r.check(bps < 60, f"{bps:.0f} bytes/s on the link (Iridium SBD class)")
    if hl2:
        d = hl2[-1][2]
        far = horiz_m((d["latitude"] / 1e7, d["longitude"] / 1e7), STILL[:2])
        r.check(far < 10 and d["type"] == M.MAV_TYPE_SURFACE_BOAT and d["custom_mode"] == ROVER["MANUAL"],
                f"HIGH_LATENCY2 carries the position ({far:.1f} m from the spawn), type {d['type']}, mode {d['custom_mode']}, "
                f"battery {d['battery']} %, failure flags {d['failure_flags']}")
    # the lost-link timer of the Iridium profile, and no speed to set
    p = g.params(["FS_GCS_TIMEOUT", "CRUISE_SPEED", "WP_SPEED", "MIS_TOTAL"])["params"]
    want = float(next(iter(swarm.lost_link_params(PLATFORM, swarm.load_platforms()[PLATFORM], "ardurover").values()), 0))
    r.check(p["FS_GCS_TIMEOUT"] and p["FS_GCS_TIMEOUT"][0] == want, f"FS_GCS_TIMEOUT {p['FS_GCS_TIMEOUT']} (links.toml lost_link_s {want})")
    r.check(p["CRUISE_SPEED"] and p["CRUISE_SPEED"][0] == 0.0 and p["WP_SPEED"][0] == 0.0, f"CRUISE_SPEED {p['CRUISE_SPEED']}, WP_SPEED {p['WP_SPEED']}")
    r.check(p["MIS_TOTAL"] and p["MIS_TOTAL"][0] == 1.0, f"MIS_TOTAL {p['MIS_TOTAL']} (home only)")
    r.lines.append("refusals")
    ack = g.mission([(M.MAV_CMD_NAV_WAYPOINT, M.MAV_FRAME_GLOBAL_RELATIVE_ALT_INT, STILL[0], STILL[1], 0, 0, 0, 0, 0),
                     (M.MAV_CMD_NAV_WAYPOINT, M.MAV_FRAME_GLOBAL_RELATIVE_ALT_INT, STILL[0] + 0.001, STILL[1], 0, 0, 0, 0, 0)])
    r.check(ack == M.MAV_MISSION_UNSUPPORTED, f"mission upload: MISSION_ACK {ack} (MAV_MISSION_UNSUPPORTED {M.MAV_MISSION_UNSUPPORTED})")
    for name in ("AUTO", "RTL", "GUIDED", "LOITER"):
        i = len(g.log)
        ack = g.set_mode(ROVER[name])
        said = [e[2]["text"] for e in g.log[i:] if e[1] == "STATUSTEXT"]
        r.check(ack == M.MAV_RESULT_FAILED and said == ["Flight mode change failed"] and g.mode == ROVER["MANUAL"],
                f"{name}: ack {ack}, said {said}, mode {g.mode}")
    for name in ("HOLD", "MANUAL"):
        ack = g.set_mode(ROVER[name])
        r.check(ack == M.MAV_RESULT_ACCEPTED and g.wait(lambda: g.mode == ROVER[name], 2.0), f"{name}: ack {ack}, mode {g.mode}")
    ack = g.command(M.MAV_CMD_NAV_TAKEOFF)
    r.check(ack == M.MAV_RESULT_UNSUPPORTED, f"NAV_TAKEOFF: ack {ack}")
    ack = g.command(M.MAV_CMD_MISSION_START)
    r.check(ack == M.MAV_RESULT_FAILED, f"MISSION_START: ack {ack}")
    ack = g.command(M.MAV_CMD_NAV_RETURN_TO_LAUNCH)
    r.check(ack == M.MAV_RESULT_FAILED, f"NAV_RETURN_TO_LAUNCH: ack {ack}")
    r.lines.append("movement")
    hl2 = [e for e in g.log if e[1] == "HIGH_LATENCY2"]
    first, last = hl2[0][2], hl2[-1][2]
    moved = horiz_m((first["latitude"] / 1e7, first["longitude"] / 1e7), (last["latitude"] / 1e7, last["longitude"] / 1e7))
    r.check(moved < 5.0, f"without conditions it stays put: {moved:.1f} m over {hl2[-1][0] - hl2[0][0]:.0f} s "
                         f"(wave jitter only), ground speed {last['groundspeed'] / 5:.1f} m/s")


def drift(r, args, workdir):
    r.lines.append(f"drift with tidewater at {args.at} in the Golden Gate")
    t_unix = unix(args.at)
    s0 = tw_sample(args.tidewater, t_unix, GATE[0], GATE[1])
    cur0 = (s0.get("current_east") or 0.0, s0.get("current_north") or 0.0)
    r.info(f"current at the spawn: {math.hypot(*cur0):.2f} m/s toward {math.degrees(math.atan2(cur0[0], cur0[1])) % 360:.0f}, "
           f"water depth {s0.get('water_depth_m')} m")
    g = Gcs(args.port + 1, time.monotonic())
    proc = start(args.port + 1, GATE, workdir / "drift", {"MURMUR_TIDEWATER_URL": args.tidewater, "MURMUR_AT": args.at})
    try:
        if not g.wait(lambda: g.hb is not None, 30):
            raise SystemExit(f"no heartbeat from the drifting buoy; see {workdir / 'drift'}")
        g.command(M.MAV_CMD_SET_MESSAGE_INTERVAL, M.MAVLINK_MSG_ID_HIGH_LATENCY2, 5e6)
        g.sleep(args.drift_s)
    finally:
        os.killpg(proc.pid, signal.SIGTERM)
        proc.wait(10)
        g.sock.close()
    hl2 = [(e[0], e[2]["latitude"] / 1e7, e[2]["longitude"] / 1e7) for e in g.log if e[1] == "HIGH_LATENCY2"]
    if len(hl2) < 3:
        r.check(False, f"only {len(hl2)} HIGH_LATENCY2 reports")
        return
    (t0, lat0, lon0), (t1, lat1, lon1) = hl2[0], hl2[-1]
    s1 = tw_sample(args.tidewater, t_unix + t1, lat1, lon1)
    cur1 = (s1.get("current_east") or 0.0, s1.get("current_north") or 0.0)
    ce, cn = (cur0[0] + cur1[0]) / 2, (cur0[1] + cur1[1]) / 2
    dt = t1 - t0
    dn, de = (lat1 - lat0) * 111_320.0, (lon1 - lon0) * 111_320.0 * math.cos(math.radians(lat0))
    moved, expect = math.hypot(dn, de), math.hypot(ce, cn) * dt
    went = math.degrees(math.atan2(de, dn)) % 360
    toward = math.degrees(math.atan2(ce, cn)) % 360
    err = abs((went - toward + 180) % 360 - 180)
    r.check(expect > 5.0, f"a current to drift on: {math.hypot(ce, cn):.2f} m/s mean over {dt:.0f} s = {expect:.0f} m")
    r.check(0.5 * expect <= moved <= 1.5 * expect, f"drifted {moved:.0f} m (the current would carry it {expect:.0f} m)")
    r.check(err < 45, f"toward {went:.0f}, the current sets toward {toward:.0f} ({err:.0f} deg off)")
    r.check(len({(round(h[1], 6), round(h[2], 6)) for h in hl2}) >= 3, f"{len(hl2)} HIGH_LATENCY2 reports, the position moving in them")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--port", type=int, default=18865)
    ap.add_argument("--workdir", default=str(ROOT / "sim" / "run" / "vs-sitl" / "drifter"))
    ap.add_argument("--tidewater", help="a running `tidewater serve`, e.g. http://127.0.0.1:8879: also check the drift")
    ap.add_argument("--at", default="2026-06-14T10:00Z", help="scenario time for --tidewater")
    ap.add_argument("--drift-s", type=float, default=180.0, help="how long to drift")
    args = ap.parse_args()
    workdir = Path(args.workdir)
    r = Report()
    spec = catalog(r)
    g = Gcs(args.port, time.monotonic())
    proc = start(args.port, STILL, workdir / "still")
    try:
        if not g.wait(lambda: g.hb is not None, 30):
            raise SystemExit(f"no heartbeat; see {workdir / 'still'}")
        g.sleep(10.0)
        wire(r, g, 0)
    finally:
        os.killpg(proc.pid, signal.SIGTERM)
        proc.wait(10)
        g.sock.close()
    if args.tidewater:
        drift(r, args, workdir)
    print(f"{spec['name']} ({PLATFORM}) as a stand-in")
    print("\n".join(r.lines))
    print("OK" if r.ok else "FAILED (see above)")
    sys.exit(0 if r.ok else 1)


if __name__ == "__main__":
    main()
