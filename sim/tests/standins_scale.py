#!/usr/bin/env python3
"""Load test of a stand-in engine alone: N vehicles sending to a dummy UDP sink.

Measures the engine's CPU (user+system, % of one core) and resident memory over a window
after warm-up, and the message rate the sink receives (should be 7.5 msg/s per vehicle). The
sink listens on --sink-ports ports (vehicle i sends to port + i % sink_ports), so one socket's
receive buffer does not overflow when every vehicle's telemetry lands in the same tick.

Like ArduPilot with Murmur's common.parm, a Rust stand-in sends only its heartbeat until a
GCS asks for more, so the sink answers each new vehicle with the bridge's telemetry plan
(SET_MESSAGE_INTERVAL, position at 2 Hz: 7.5 msg/s with the heartbeat).

    .venv/bin/python sim/tests/standins_scale.py --engine rust --vehicles 1000
"""

import argparse
import json
import math
import multiprocessing as mp
import os
import selectors
import socket
import subprocess
import sys
import time
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
STANDINS = ("rq4", "mq9b", "cca", "seahunter", "buoy", "remus", "orca")


def fleet(n, port, ports):
    cat = tomllib.loads((ROOT / "sim" / "platforms.toml").read_text())
    vs = []
    for i in range(n):
        pid = STANDINS[i % len(STANDINS)]
        spec = cat[pid]
        lat = 37.0 + (i // 60) * 0.01
        lon = -123.5 + (i % 60) * 0.01
        alt = spec["alt_m"] if spec["behavior"] == "plane" else 0
        vs.append({"uid": i + 1, "sysid": i % 250 + 1, "callsign": f"{spec['callsign']} {i + 1}", "port": port + i % ports,
                   "home": [lat, lon, alt, (i * 37) % 360], "spec": spec, "platform": pid})
    return {"bridge_host": "127.0.0.1", "vehicles": vs}


def telemetry_plan():
    """The bridge's plan for an aircraft (world.rs telemetry_plan), as COMMAND_LONG frames to
    any system (target 0)."""
    from pymavlink.dialects.v20 import ardupilotmega as mavlink
    mav = mavlink.MAVLink(None, srcSystem=255, srcComponent=190)
    rates = [(33, 2.0), (30, 1.0), (74, 1.0), (42, 1.0), (1, 0.5), (24, 0.5), (193, 0.5)]
    return [mav.command_long_encode(0, 0, mavlink.MAV_CMD_SET_MESSAGE_INTERVAL, 0, msg_id, 1e6 / hz,
                                    0, 0, 0, 0, 0).pack(mav) for msg_id, hz in rates]


def sink(ports, counter, ready, stop):
    plan = telemetry_plan()
    asked = set()
    sel = selectors.DefaultSelector()
    for p in ports:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 8 << 20)
        s.bind(("127.0.0.1", p))
        s.setblocking(False)
        sel.register(s, selectors.EVENT_READ)
    ready.set()
    n = 0
    while not stop.is_set():
        for key, _ in sel.select(0.2):
            while True:
                try:
                    _, addr = key.fileobj.recvfrom(4096)
                    n += 1
                except BlockingIOError:
                    break
                if addr not in asked:
                    asked.add(addr)
                    for frame in plan:
                        key.fileobj.sendto(frame, addr)
        counter.value = n


def cpu_seconds(pid):
    f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
    return (int(f[11]) + int(f[12])) / os.sysconf("SC_CLK_TCK")


def rss_mb(pid):
    for line in open(f"/proc/{pid}/status"):
        if line.startswith("VmRSS:"):
            return int(line.split()[1]) / 1024
    return math.nan


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--engine", choices=("rust", "python"), default="rust")
    ap.add_argument("--vehicles", type=int, default=1000)
    ap.add_argument("--port", type=int, default=15560, help="the sink's first UDP port")
    ap.add_argument("--sink-ports", type=int, default=100)
    ap.add_argument("--sink-procs", type=int, default=4)
    ap.add_argument("--warmup", type=float, default=15)
    ap.add_argument("--window", type=float, default=30)
    args = ap.parse_args()

    cfg = ROOT / "sim" / "run" / f"scale-{args.engine}-{args.vehicles}.json"
    cfg.parent.mkdir(exist_ok=True)
    cfg.write_text(json.dumps(fleet(args.vehicles, args.port, args.sink_ports)))
    # the sink runs in several processes so it keeps up; each counts into a shared counter
    ports = list(range(args.port, args.port + args.sink_ports))
    counters = [mp.Value("q", 0, lock=False) for _ in range(args.sink_procs)]
    ready = mp.Event()
    stop = mp.Event()
    workers = [mp.Process(target=sink, args=(ports[k::args.sink_procs], counters[k], ready, stop), daemon=True)
               for k in range(args.sink_procs)]
    for w in workers:
        w.start()
    ready.wait(10)
    cmd = ([str(ROOT / "target" / "release" / "murmur-standins")] if args.engine == "rust"
           else [sys.executable, str(ROOT / "sim" / "standins.py")])
    env = {k: v for k, v in os.environ.items() if k not in ("MURMUR_SCENARIO", "MURMUR_TIDEWATER_URL")}
    env["MURMUR_STANDINS_STATS"] = "10"  # the Rust engine logs what it sent
    log = open(cfg.with_suffix(".log"), "w")
    proc = subprocess.Popen([*cmd, str(cfg)], stdout=log, stderr=subprocess.STDOUT, env=env)
    try:
        time.sleep(args.warmup)
        c0, t0, n0 = cpu_seconds(proc.pid), time.monotonic(), sum(c.value for c in counters)
        time.sleep(args.window)
        c1, t1, n1 = cpu_seconds(proc.pid), time.monotonic(), sum(c.value for c in counters)
        received = n1 - n0
        result = {
            "engine": args.engine,
            "vehicles": args.vehicles,
            "cpu_pct_of_one_core": round(100 * (c1 - c0) / (t1 - t0), 1),
            "rss_mb": round(rss_mb(proc.pid), 1),
            "sink_msgs_per_s": round(received / (t1 - t0)),
            "expected_msgs_per_s": 7.5 * args.vehicles,
        }
        print(json.dumps(result))
    finally:
        proc.terminate()
        proc.wait(10)
        stop.set()
        for w in workers:
            w.join(5)


if __name__ == "__main__":
    main()
