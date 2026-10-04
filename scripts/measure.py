#!/usr/bin/env python3
"""Measure a running stack under load: WebSocket throughput to one browser client, the
telemetry rate from vehicles, and CPU and memory of the Rust bridge and stand-in engine.
Used for the load tests in docs/load-test.md.

    .venv/bin/python scripts/measure.py --seconds 30
    MURMUR_HTTP_PORT=9765 .venv/bin/python scripts/measure.py --seconds 30 --json
    .venv/bin/python scripts/measure.py --seconds 30 --lod --view -122.6,37.6,-122.2,37.9 --zoom 12.6

The processes are found by name (murmur-bridge, murmur-standins) and told apart from other
stacks by the MURMUR_HTTP_PORT in their environment (up.sh exports it to both); --bridge-pid
and --standins-pid override.

Without --lod the client takes the plain feed (/ws: everything, every update, as the console
did before murmur-and). With --lod it connects as the console does now (/ws?lod=1) and sends
the view given by --view (west,south,east,north) and --zoom, plus --pin uids, so the bridge
sends full detail for what that view holds and lite snapshots or clusters for the rest.
"""

import argparse
import asyncio
import json
import os
import re
import time
import urllib.request

import websockets

MSG_TYPE = re.compile(r'\{"type":"(\w+)"')

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


async def ws_sample(seconds, view=None):
    """(initial state bytes, update count, update bytes, bytes by message type). `view` is
    the subscription to send on the level-of-detail feed, None for the plain feed."""
    url = BRIDGE.replace("http", "ws", 1) + "/ws" + ("?lod=1" if view is not None else "")
    n = size = 0
    first = None
    by_type = {}
    async with websockets.connect(url, max_size=None) as ws:
        if view is not None:
            await ws.send(json.dumps({"type": "view", **view}))
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            try:
                msg = await asyncio.wait_for(ws.recv(), timeout=max(0.1, end - time.monotonic()))
            except asyncio.TimeoutError:
                break
            if first is None:
                first = len(msg)  # initial full state
                continue
            m = MSG_TYPE.match(msg)
            kind = m.group(1) if m else "?"
            by_type[kind] = by_type.get(kind, 0) + len(msg)
            if kind != "delta":
                continue  # decisions and tracks are sent whole on change, not per tick
            n += 1
            size += len(msg)
    return first, n, size, by_type


async def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--seconds", type=float, default=20)
    ap.add_argument("--bridge-pid", type=int)
    ap.add_argument("--standins-pid", type=int)
    ap.add_argument("--json", action="store_true", help="also print the numbers as one JSON line")
    ap.add_argument("--lod", action="store_true", help="the level-of-detail feed (/ws?lod=1) with a view")
    ap.add_argument("--view", help="west,south,east,north of the view sent with --lod (default: none, all lite)")
    ap.add_argument("--zoom", type=float, default=12.6, help="zoom sent with --lod (default 12.6, the console's start)")
    ap.add_argument("--pin", default="", help="uids always at full detail, comma-separated (--lod)")
    args = ap.parse_args()
    view = None
    if args.lod:
        view = {"bounds": [float(x) for x in args.view.split(",")] if args.view else None, "zoom": args.zoom,
                "pin": [int(x) for x in args.pin.split(",") if x]}

    procs = {"bridge": args.bridge_pid or find_pid("murmur-bridge"),
             "standins": args.standins_pid or find_pid("murmur-standins")}
    procs = {k: p for k, p in procs.items() if p}
    t0 = time.monotonic()
    c0 = {k: cpu_seconds(p) for k, p in procs.items()}
    rx0 = rx_totals()
    first, n, size, by_type = await ws_sample(args.seconds, view)
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
    out["ws_feed"] = "lod" if view is not None else "plain"
    out["ws_initial_kb"] = round((first or 0) / 1024, 1)
    out["ws_updates_per_s"] = round(n / dt, 1)
    out["ws_kb_per_s"] = round(size / dt / 1024, 1)
    out["ws_other_kb_per_s"] = round(sum(v for k, v in by_type.items() if k != "delta") / dt / 1024, 1)

    print(f"vehicles               {out['vehicles']} ({out['nav_ready']} nav-ready)")
    for k in procs:
        print(f"{k:8s} CPU           {out[f'{k}_cpu_pct']}% of one core, RSS {out[f'{k}_rss_mb']} MB, "
              f"{out[f'{k}_threads']} threads")
    if rates:
        print(f"telemetry rx per veh   median {out['rx_median_per_vehicle']} msg/s, max {out['rx_max_per_vehicle']}, "
              f"total {out['rx_msgs_per_s']} msg/s")
    print(f"initial WS state       {out['ws_initial_kb']} KB ({out['ws_feed']} feed)")
    print(f"WS updates             {out['ws_updates_per_s']}/s, {out['ws_kb_per_s']} KB/s "
          f"({size / max(n, 1) / 1024:.1f} KB each); other messages {out['ws_other_kb_per_s']} KB/s")
    if args.json:
        print(json.dumps(out))


if __name__ == "__main__":
    asyncio.run(main())
