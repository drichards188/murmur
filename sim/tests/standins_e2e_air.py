#!/usr/bin/env python3
"""End to end: copter and VTOL stand-ins through the Rust bridge, tasked over its REST API.

Writes a fleet of the SITL air platforms forced to stand-ins (two Skydio X10-class copters,
an FPV quad, an FVR-90-class QuadPlane), starts `murmur-bridge` and `swarm.py` with the
Rust engine, waits for every vehicle to be navigation-ready, then tasks them: orbit, patrol,
hold, and RTL once the first tasks are done. Tasks that need an operator (the FPV is armed:
sim/policy.toml) are approved as op-1. Prints each op's state history; exit 0 when every op
ends done (or superseded by the next task on purpose).

    cargo build --release -p murmur-standins -p murmur-bridge
    .venv/bin/python sim/tests/standins_e2e_air.py
"""

import argparse
import json
import math
import os
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PY = ROOT / ".venv" / "bin" / "python"
OPERATOR = {"X-Murmur-Operator": "op-1"}
SPAWN = (37.8040, -122.4650)  # Crissy Field, on land

FLEET = f"""title = "Air stand-ins (e2e)"
[[unit]]
platform = "x10"
sim = "standin"
count = 2
spawn = [{SPAWN[0]}, {SPAWN[1]}, 3, 0]
spacing_m = 30
[[unit]]
platform = "fpv"
sim = "standin"
count = 1
spawn = [{SPAWN[0] - 0.001}, {SPAWN[1]}, 3, 0]
[[unit]]
platform = "fvr90"
sim = "standin"
count = 1
spawn = [{SPAWN[0] - 0.002}, {SPAWN[1]}, 3, 0]
"""


def api(base, path, body=None):
    req = urllib.request.Request(base + path, method="POST" if body is not None else "GET",
                                 data=None if body is None else json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", **OPERATOR})
    try:
        with urllib.request.urlopen(req, timeout=5) as r:
            return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}")


def offset(lon, lat, north_m, east_m):
    return [lon + east_m / (111_320.0 * math.cos(math.radians(lat))), lat + north_m / 111_320.0]


def task(base, body):
    """POST /api/tasks; approve if it waits for an operator. Returns the tasking id."""
    status, t = api(base, "/api/tasks", body)
    if status == 202:
        status, a = api(base, f"/api/intents/{t['intent_id']}/approve", {"note": "e2e"})
        if status != 200 or not a.get("tasking"):
            raise SystemExit(f"approve {t['intent_id']}: {status} {a}")
        print(f"  {body['task']} {body['uids']}: needed approval, approved")
        return a["tasking"]["id"]
    if status != 200:
        raise SystemExit(f"task {body}: {status} {t}")
    return t["id"]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--base-port", type=int, default=18900)
    ap.add_argument("--http-port", type=int, default=8875)
    ap.add_argument("--timeout", type=float, default=600.0, help="seconds for all ops to settle")
    ap.add_argument("--python", default=str(PY if PY.exists() else sys.executable))
    args = ap.parse_args()
    base = f"http://127.0.0.1:{args.http_port}"
    run = ROOT / "sim" / "run"
    run.mkdir(exist_ok=True)
    fleet = run / "e2e-air.toml"
    fleet.write_text(FLEET)
    bridge = ROOT / "target" / "release" / "murmur-bridge"
    for b in (bridge, ROOT / "target" / "release" / "murmur-standins"):
        if not b.exists():
            sys.exit(f"missing {b}; cargo build --release -p murmur-standins -p murmur-bridge")

    env = {**os.environ, "MURMUR_BASE_PORT": str(args.base_port),
           "MURMUR_HTTP_PORT": str(args.http_port), "MURMUR_ROOT": str(ROOT),
           "MURMUR_FLEET_MANIFEST": str(run / "fleet.json"), "MURMUR_RUN_ID": "e2e-air",
           "MURMUR_STANDINS_STATS": "0", "PYTHONUNBUFFERED": "1"}
    for k in ("MURMUR_SCENARIO", "MURMUR_AT", "MURMUR_TIDEWATER_URL"):
        env.pop(k, None)
    logs = ROOT / "logs"
    logs.mkdir(exist_ok=True)
    procs = [
        subprocess.Popen([str(bridge)], cwd=ROOT, env=env, stdout=open(logs / "e2e-air-bridge.log", "w"),
                         stderr=subprocess.STDOUT, start_new_session=True),
        subprocess.Popen([args.python, str(ROOT / "sim" / "swarm.py"), "--fleet", str(fleet),
                          "--base-port", str(args.base_port)], env=env,
                         stdout=open(logs / "e2e-air-swarm.log", "w"), stderr=subprocess.STDOUT,
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
            _, st = api(base, "/api/state")
            by_call = {v["identity"]["callsign"]: v for v in st["vehicles"] if v.get("identity")}
            if len(by_call) == 4 and all(v["nav_ready"] and v.get("kind") for v in by_call.values()):
                break
        except OSError:
            pass
        time.sleep(1)
    else:
        print(f"vehicles not ready: {sorted(by_call)}")
        return False
    kinds = {c: v.get("kind") for c, v in by_call.items()}
    print(f"4 vehicles ready after {time.monotonic() - t0:.0f} s: {kinds}")
    uid = {c: v["uid"] for c, v in by_call.items()}
    lon0, lat0 = SPAWN[1], SPAWN[0]

    # (task body, what each op must end as)
    plan = [
        ({"task": "orbit", "uids": [uid["SKY 1"]], "center": offset(lon0, lat0, 150, 0), "radius": 60}, "done"),
        ({"task": "patrol", "uids": [uid["SKY 2"]],
          "route": [offset(lon0, lat0, 0, 150), offset(lon0, lat0, 100, 150)]}, "done"),
        ({"task": "orbit", "uids": [uid["KESTREL 1"]], "center": offset(lon0, lat0, 1500, 0), "radius": 300},
         "done"),
        ({"task": "hold", "uids": [uid["WASP 1"]]}, "done"),
    ]
    taskings = []
    for body, want in plan:
        taskings.append((task(base, body), body["task"], want))
    # once the first tasks are done: RTL for the orbiters, hold for the patroller
    then = {0: ({"task": "rtl", "uids": [uid["SKY 1"]]}, "rtl (copter)"),
            1: ({"task": "hold", "uids": [uid["SKY 2"]]}, "hold (in flight)"),
            2: ({"task": "rtl", "uids": [uid["KESTREL 1"]]}, "rtl (vtol)")}

    deadline = time.monotonic() + args.timeout
    while time.monotonic() < deadline:
        _, st = api(base, "/api/state")
        snap = {t["id"]: t for t in st["taskings"]}
        for i in list(then):
            ops = snap.get(taskings[i][0], {}).get("ops", [])
            if ops and all(o["state"] == "done" for o in ops):
                body, name = then.pop(i)
                taskings.append((task(base, body), name, "done"))
        settled = all(o.get("state") in ("done", "failed") for tid, _, _ in taskings
                      for o in snap.get(tid, {}).get("ops", [{}]))
        if settled and not then:
            break
        time.sleep(1)

    _, st = api(base, "/api/state")
    snap = {t["id"]: t for t in st["taskings"]}
    names = {v["uid"]: c for c, v in by_call.items()}
    ok = True
    for tid, name, want in taskings:
        for op in snap[tid]["ops"]:
            # superseded is how an earlier task ends when a later one replaces it on purpose
            good = op["state"] == want or (op["state"] == "superseded" and want == "done")
            ok &= good
            t_first = op["history"][0]["t"]
            hist = " -> ".join(f"{h['state']}@{h['t'] - t_first:.0f}s" for h in op["history"])
            print(f"{'ok  ' if good else 'FAIL'} {name:18s} {names[op['uid']]:10s} {hist}  ({op['detail']})")
    for c, v in sorted(api(base, "/api/state")[1] and {v["identity"]["callsign"]: v for v in
                                                        api(base, "/api/state")[1]["vehicles"]}.items()):
        print(f"     {c:10s} {v.get('kind')} mode {v.get('mode')} armed {v.get('armed')} "
              f"alt {v.get('alt_rel')} pos {v.get('position')}")
    return ok


if __name__ == "__main__":
    main()
