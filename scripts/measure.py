#!/usr/bin/env python3
"""Measure a running stack under load: WebSocket throughput to one browser client, the
telemetry rate from vehicles, and CPU and memory of the Rust bridge and stand-in engine.
Used for the load tests in docs/load-test.md.

    .venv/bin/python scripts/measure.py --seconds 30
    MURMUR_HTTP_PORT=9765 .venv/bin/python scripts/measure.py --seconds 30 --json

The processes are found by name (murmur-bridge, murmur-standins) and told apart from other
stacks by the MURMUR_HTTP_PORT in their environment (up.sh exports it to both); --bridge-pid
and --standins-pid override.
"""

import argparse
import asyncio
import json
import os
import time
import urllib.request

import websockets

PORT = int(os.environ.get("MURMUR_HTTP_PORT", 8765))
BRIDGE = os.environ.get("MURMUR_BRIDGE_URL", f"http://127.0.0.1:{PORT}")


def get(path):
    with urllib.request.urlopen(f"{BRIDGE}{path}", timeout=10) as r:
        return json.loads(r.read())


def find_pid(name):
    """The process called `name` that belongs to this stack (by MURMUR_HTTP_PORT)."""
    for pid in os.listdir("/proc"):
        if not pid.isdigit():
            continue
        try:
            argv0 = open(f"/proc/{pid}/cmdline", "rb").read().split(b"\0")[0].decode()
        except OSError:
            continue
        if os.path.basename(argv0) == name and port_of(int(pid)) in (None, PORT):
            return int(pid)
    return None


def port_of(pid):
    """The HTTP port a process's stack runs on (several stacks can run at once)."""
    try:
        env = open(f"/proc/{pid}/environ", "rb").read().decode(errors="replace").split("\0")
    except OSError:
        return None
    for kv in env:
        if kv.startswith("MURMUR_HTTP_PORT="):
            return int(kv.split("=", 1)[1])
    return 8765


def cpu_seconds(pid):
    f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
    return (int(f[11]) + int(f[12])) / os.sysconf("SC_CLK_TCK")


def rss_mb(pid):
    for line in open(f"/proc/{pid}/status"):
        if line.startswith("VmRSS:"):
            return int(line.split()[1]) / 1024
    return None


def threads(pid):
    return len(os.listdir(f"/proc/{pid}/task"))


def rx_totals():
    return {int(k): v["rx_total"] for k, v in get("/api/stats")["vehicles"].items()}


async def ws_sample(seconds):
    url = BRIDGE.replace("http", "ws", 1) + "/ws"
    n = size = 0
    first = None
    async with websockets.connect(url, max_size=None) as ws:
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            try:
                msg = await asyncio.wait_for(ws.recv(), timeout=max(0.1, end - time.monotonic()))
            except asyncio.TimeoutError:
                break
            if first is None:
                first = len(msg)  # initial full state
                continue
            n += 1
            size += len(msg)
    return first, n, size


async def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--seconds", type=float, default=20)
    ap.add_argument("--bridge-pid", type=int)
    ap.add_argument("--standins-pid", type=int)
    ap.add_argument("--json", action="store_true", help="also print the numbers as one JSON line")
    args = ap.parse_args()

    procs = {"bridge": args.bridge_pid or find_pid("murmur-bridge"),
             "standins": args.standins_pid or find_pid("murmur-standins")}
    procs = {k: p for k, p in procs.items() if p}
    t0 = time.monotonic()
    c0 = {k: cpu_seconds(p) for k, p in procs.items()}
    rx0 = rx_totals()
    first, n, size = await ws_sample(args.seconds)
    rx1 = rx_totals()
    dt = time.monotonic() - t0
    c1 = {k: cpu_seconds(p) for k, p in procs.items()}
    s1 = get("/api/state")
    rates = sorted((rx1[k] - rx0[k]) / dt for k in rx1 if k in rx0)
    out = {
        "vehicles": len(s1["vehicles"]),
        "nav_ready": sum(1 for v in s1["vehicles"] if v.get("nav_ready")),
        "seconds": round(dt, 1),
    }
    for k, p in procs.items():
        out[f"{k}_cpu_pct"] = round(100 * (c1[k] - c0[k]) / dt, 1)
        out[f"{k}_rss_mb"] = round(rss_mb(p), 1)
        out[f"{k}_threads"] = threads(p)
    if rates:
        out["rx_msgs_per_s"] = round(sum(rates))
        out["rx_median_per_vehicle"] = round(rates[len(rates) // 2], 1)
        out["rx_max_per_vehicle"] = round(rates[-1], 1)
    out["ws_initial_kb"] = round((first or 0) / 1024, 1)
    out["ws_updates_per_s"] = round(n / dt, 1)
    out["ws_kb_per_s"] = round(size / dt / 1024, 1)

    print(f"vehicles               {out['vehicles']} ({out['nav_ready']} nav-ready)")
    for k in procs:
        print(f"{k:8s} CPU           {out[f'{k}_cpu_pct']}% of one core, RSS {out[f'{k}_rss_mb']} MB, "
              f"{out[f'{k}_threads']} threads")
    if rates:
        print(f"telemetry rx per veh   median {out['rx_median_per_vehicle']} msg/s, max {out['rx_max_per_vehicle']}, "
              f"total {out['rx_msgs_per_s']} msg/s")
    print(f"initial WS state       {out['ws_initial_kb']} KB")
    print(f"WS updates             {out['ws_updates_per_s']}/s, {out['ws_kb_per_s']} KB/s "
          f"({size / max(n, 1) / 1024:.1f} KB each)")
    if args.json:
        print(json.dumps(out))


if __name__ == "__main__":
    asyncio.run(main())
