#!/usr/bin/env python3
"""End to end: the stand-in-only fleet through the Python bridge, tasked over its REST API.

Starts the bridge and `swarm.py --fleet standins-only` with MURMUR_STANDINS=<engine>, waits
for every vehicle to be navigation-ready, then issues orbit / patrol / hold / rtl tasks
(and one the platform must refuse) and follows each op until it settles. Prints each op's
state history; exit 0 when every op ends as expected.

    .venv/bin/python sim/tests/standins_e2e.py --engine rust
    .venv/bin/python sim/tests/standins_e2e.py --engine python
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
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PY = ROOT / ".venv" / "bin" / "python"


def api(base, path, body=None):
    req = urllib.request.Request(base + path, method="POST" if body is not None else "GET",
                                 data=None if body is None else json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=5) as r:
        return json.loads(r.read())


def offset(lon, lat, north_m, east_m):
    return [lon + east_m / (111_320.0 * math.cos(math.radians(lat))), lat + north_m / 111_320.0]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--engine", choices=("rust", "python"), default="rust")
    ap.add_argument("--base-port", type=int, default=15560)
    ap.add_argument("--http-port", type=int, default=8865)
    ap.add_argument("--timeout", type=float, default=420.0, help="seconds for all ops to settle")
    ap.add_argument("--python", default=str(PY if PY.exists() else sys.executable))
    args = ap.parse_args()
    base = f"http://127.0.0.1:{args.http_port}"
    logs = ROOT / "logs"
    logs.mkdir(exist_ok=True)

    env = {**os.environ, "MURMUR_STANDINS": args.engine, "MURMUR_BASE_PORT": str(args.base_port),
           "MURMUR_HTTP_PORT": str(args.http_port), "PYTHONUNBUFFERED": "1"}
    for k in ("MURMUR_SCENARIO", "MURMUR_AT", "MURMUR_TIDEWATER_URL"):
        env.pop(k, None)
    procs = [
        subprocess.Popen([args.python, "-m", "murmur_bridge"], cwd=ROOT / "bridge", env=env,
                         stdout=open(logs / f"e2e-bridge-{args.engine}.log", "w"), stderr=subprocess.STDOUT,
                         start_new_session=True),
        subprocess.Popen([args.python, str(ROOT / "sim" / "swarm.py"), "--fleet", "standins-only",
                          "--base-port", str(args.base_port)], env=env,
                         stdout=open(logs / f"e2e-swarm-{args.engine}.log", "w"), stderr=subprocess.STDOUT,
                         start_new_session=True),
    ]
    try:
        ok = drive(base, args)
    finally:
        for p in procs:
            try:
                os.killpg(p.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        for p in procs:
            try:
                p.wait(10)
            except subprocess.TimeoutExpired:
                os.killpg(p.pid, signal.SIGKILL)
    sys.exit(0 if ok else 1)


def drive(base, args):
    t0 = time.monotonic()
    by_call = {}
    while time.monotonic() - t0 < 90:
        try:
            st = api(base, "/api/state")
            by_call = {v["identity"]["callsign"]: v for v in st["vehicles"] if v.get("identity")}
            if len(by_call) == 7 and all(v["nav_ready"] for v in by_call.values()):
                break
        except OSError:
            pass
        time.sleep(1)
    else:
        print(f"vehicles not ready: {sorted(by_call)}")
        return False
    print(f"{args.engine}: 7 vehicles ready after {time.monotonic() - t0:.0f} s")
    sid = {c.split()[0]: v["sysid"] for c, v in by_call.items()}
    pos = {c.split()[0]: v["position"] for c, v in by_call.items()}

    # (task body, what each op must end as)
    plan = [
        ({"task": "orbit", "sysids": [sid["HAWK"], sid["REAPER"], sid["FURY"]], "center": [-122.68, 37.78],
          "radius": 5000}, "done"),
        ({"task": "patrol", "sysids": [sid["HUNTER"]],
          "route": [offset(*pos["HUNTER"], 250, 0), offset(*pos["HUNTER"], 250, 250)]}, "done"),
        ({"task": "patrol", "sysids": [sid["REMUS"]],
          "route": [offset(*pos["REMUS"], 60, 0), offset(*pos["REMUS"], 60, 60)]}, "done"),
        ({"task": "hold", "sysids": [sid["DRIFT"]]}, "done"),
        ({"task": "rtl", "sysids": [sid["ORCA"]]}, "done"),
    ]
    taskings = []
    for body, want in plan:
        t = api(base, "/api/tasks", body)
        taskings.append((t["id"], body["task"], want))
    # a drifting buoy has no propulsion: an orbit must be refused by the vehicle itself, after
    # the hold above has finished (a new task supersedes the vehicle's previous one)
    pending_refusal = {"task": "orbit", "sysids": [sid["DRIFT"]], "center": offset(*pos["DRIFT"], 100, 0),
                       "radius": 200}
    rtl_after_patrol = {"task": "rtl", "sysids": [sid["HUNTER"]]}

    deadline = time.monotonic() + args.timeout
    while time.monotonic() < deadline:
        snap = {t["id"]: t for t in api(base, "/api/state")["taskings"]}
        ops = {tid: snap[tid]["ops"] for tid, _, _ in taskings if tid in snap}
        hold = next(t for t in taskings if t[1] == "hold")
        if pending_refusal and all(o["state"] == "done" for o in ops.get(hold[0], [])):
            taskings.append((api(base, "/api/tasks", pending_refusal)["id"], "orbit (buoy)", "failed"))
            pending_refusal = None
        patrol = taskings[1]
        if rtl_after_patrol and all(o["state"] == "done" for o in ops.get(patrol[0], [])):
            taskings.append((api(base, "/api/tasks", rtl_after_patrol)["id"], "rtl (after patrol)", "done"))
            rtl_after_patrol = None
        settled = all(o.get("state") in ("done", "failed") for tid, _, _ in taskings for o in ops.get(tid, [{}]))
        if settled and not pending_refusal and not rtl_after_patrol:
            break
        time.sleep(1)

    snap = {t["id"]: t for t in api(base, "/api/state")["taskings"]}
    names = {v["sysid"]: c for c, v in by_call.items()}
    ok = True
    for tid, task, want in taskings:
        for op in snap[tid]["ops"]:
            # superseded is how an earlier task ends when a later one replaces it on purpose
            good = op["state"] == want or (op["state"] == "superseded" and want == "done")
            ok &= good
            t_first = op["history"][0]["t"]
            hist = " -> ".join(f"{h['state']}@{h['t'] - t_first:.0f}s" for h in op["history"])
            print(f"{'ok  ' if good else 'FAIL'} {task:20s} {names[op['sysid']]:9s} {hist}  ({op['detail']})")
    return ok


if __name__ == "__main__":
    main()
