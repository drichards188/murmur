#!/usr/bin/env python3
"""Measure the bridge under load: WebSocket throughput to the browser, telemetry rate from
vehicles, and bridge CPU. Used for the 100-node load test.

    .venv/bin/python scripts/measure.py --seconds 20
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


def state():
    with urllib.request.urlopen(f"{BRIDGE}/api/state", timeout=10) as r:
        return json.loads(r.read())


def bridge_pid():
    for pid in os.listdir("/proc"):
        if not pid.isdigit():
            continue
        try:
            argv = open(f"/proc/{pid}/cmdline", "rb").read().decode().split("\0")
        except OSError:
            continue
        cmd = " ".join(argv)
        rust = os.path.basename(argv[0]) == "murmur-bridge"
        if rust or ("-m murmur_bridge" in cmd and "python" in cmd):
            if port_of(int(pid)) in (None, PORT):
                return int(pid)
    return None


def port_of(pid):
    """The HTTP port a bridge process was started with (several stacks can run at once)."""
    try:
        env = open(f"/proc/{pid}/environ", "rb").read().decode(errors="replace").split("\0")
    except OSError:
        return None
    for kv in env:
        if kv.startswith("MURMUR_HTTP_PORT="):
            return int(kv.split("=", 1)[1])
    return 8765


def cpu_seconds(pid):
    f = open(f"/proc/{pid}/stat").read().split()
    return (int(f[13]) + int(f[14])) / os.sysconf("SC_CLK_TCK")


def rx_totals(_state=None):
    with urllib.request.urlopen(f"{BRIDGE}/api/stats", timeout=10) as r:
        stats = json.loads(r.read())
    return {int(k): v["rx_total"] for k, v in stats["vehicles"].items()}


async def ws_sample(seconds):
    url = BRIDGE.replace("http", "ws") + "/ws"
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
    ap = argparse.ArgumentParser()
    ap.add_argument("--seconds", type=float, default=20)
    args = ap.parse_args()

    s0 = state()
    pid = bridge_pid()
    c0, t0 = (cpu_seconds(pid), time.monotonic()) if pid else (None, None)
    rx0 = rx_totals(s0)
    first, n, size = await ws_sample(args.seconds)
    s1 = state()
    dt = time.monotonic() - t0 if t0 else args.seconds
    rx1 = rx_totals(s1)
    rates = sorted((rx1[k] - rx0[k]) / dt for k in rx1 if k in rx0)

    print(f"vehicles               {len(s1['vehicles'])}")
    if pid:
        print(f"bridge CPU             {100 * (cpu_seconds(pid) - c0) / dt:.1f}% of one core")
    if rates:
        print(f"telemetry rx per veh   median {rates[len(rates) // 2]:.1f} msg/s, max {rates[-1]:.1f}, "
              f"total {sum(rates):.0f} msg/s")
    print(f"initial WS state       {first / 1024:.1f} KB")
    print(f"WS updates             {n / args.seconds:.1f}/s, {size / args.seconds / 1024:.1f} KB/s "
          f"({size / max(n, 1) / 1024:.1f} KB each)")


if __name__ == "__main__":
    asyncio.run(main())
