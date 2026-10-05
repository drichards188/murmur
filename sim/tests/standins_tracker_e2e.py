#!/usr/bin/env python3
"""End to end: the Rust bridge's track task on a stand-in antenna tracker.

Runs `sim/fleets/tracker-standin.toml` (a tracker forced to the Rust stand-in engine and a
fixed-wing stand-in to follow) behind the Rust bridge, issues the `track` task, and checks that
the op reaches its tracking state (SYSID_TARGET confirmed, AUTO, "locked on") and that the
tracker then points at the target: yaw within a few degrees of the bearing to it, sampled for
20 s while the aircraft flies its circuit.

    cargo build --release -p murmur-standins -p murmur-bridge
    .venv/bin/python sim/tests/standins_tracker_e2e.py

Uses UDP 19300+ and HTTP 8885. Exit 0 = locked on and pointing.
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
OPERATOR = "op-1"


def api(base, path, body=None):
    req = urllib.request.Request(base + path, method="POST" if body is not None else "GET",
                                 data=None if body is None else json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", "X-Murmur-Operator": OPERATOR})
    with urllib.request.urlopen(req, timeout=5) as r:
        return json.loads(r.read())


def bearing(lon1, lat1, lon2, lat2):
    dn = (lat2 - lat1) * 111_320.0
    de = (lon2 - lon1) * 111_320.0 * math.cos(math.radians(lat1))
    return math.degrees(math.atan2(de, dn)) % 360.0


def wrap180(a):
    return (a + 180.0) % 360.0 - 180.0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--base-port", type=int, default=19300)
    ap.add_argument("--http-port", type=int, default=8885)
    ap.add_argument("--python", default=str(PY if PY.exists() else sys.executable))
    args = ap.parse_args()
    base = f"http://127.0.0.1:{args.http_port}"
    logs = ROOT / "logs"
    logs.mkdir(exist_ok=True)
    env = {**os.environ, "MURMUR_BASE_PORT": str(args.base_port),
           "MURMUR_HTTP_PORT": str(args.http_port), "MURMUR_ROOT": str(ROOT), "PYTHONUNBUFFERED": "1",
           "MURMUR_AUTH": "dev"}
    for k in ("MURMUR_SCENARIO", "MURMUR_AT", "MURMUR_TIDEWATER_URL"):
        env.pop(k, None)
    # this run's manifest before the bridge starts (as scripts/up.sh): started beside
    # swarm.py the bridge could read the previous run's sim/run/fleet.json
    subprocess.run([args.python, str(ROOT / "sim" / "swarm.py"), "--fleet", "tracker-standin",
                    "--base-port", str(args.base_port), "--manifest-only"], env=env, check=True)
    procs = [
        subprocess.Popen([str(ROOT / "target" / "release" / "murmur-bridge")], cwd=ROOT, env=env,
                         stdout=open(logs / "e2e-tracker-bridge.log", "w"), stderr=subprocess.STDOUT,
                         start_new_session=True),
        subprocess.Popen([args.python, str(ROOT / "sim" / "swarm.py"), "--fleet", "tracker-standin",
                          "--base-port", str(args.base_port)], env=env,
                         stdout=open(logs / "e2e-tracker-swarm.log", "w"), stderr=subprocess.STDOUT,
                         start_new_session=True),
    ]
    try:
        ok = drive(base)
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


def drive(base):
    t0 = time.monotonic()
    by_call = {}
    while time.monotonic() - t0 < 90:
        try:
            st = api(base, "/api/state")
            by_call = {v["identity"]["callsign"]: v for v in st["vehicles"] if v.get("identity")}
            if len(by_call) == 2 and all(v["nav_ready"] for v in by_call.values()):
                break
        except OSError:
            pass
        time.sleep(1)
    else:
        print(f"vehicles not ready: {sorted(by_call)}")
        return False
    trk, tgt = by_call["TRK 1"], by_call["REAPER 1"]
    print(f"2 vehicles ready after {time.monotonic() - t0:.0f} s; tracker kind {trk['kind']}, "
          f"mode {trk['mode']}, armed {trk['armed']}")
    try:
        t = api(base, "/api/tasks", {"task": "track", "uids": [trk["uid"]], "target": tgt["uid"]})
    except urllib.error.HTTPError as e:
        print(f"bridge refused the task: {e.code} {e.read()}")
        return False
    if t.get("id") is None and t.get("intent_id") is not None:
        if t.get("state") == "pending_approval":
            t = api(base, f"/api/intents/{t['intent_id']}/approve", {"note": "e2e"})["tasking"]
        else:
            # rules 5: approved but queued until the link can carry it; its tasking then
            # appears on the intent (approving it again is a 409)
            iid = t["intent_id"]
            deadline = time.monotonic() + 120
            while time.monotonic() < deadline and not t.get("tasking"):
                time.sleep(1)
                t = api(base, f"/api/intents/{iid}")
            if not t.get("tasking"):
                print(f"intent {iid} not dispatched within 120 s")
                return False
            t = t["tasking"]
    tid = t["id"]

    deadline = time.monotonic() + 120
    op = None
    while time.monotonic() < deadline:
        snap = {x["id"]: x for x in api(base, "/api/state")["taskings"]}
        op = snap[tid]["ops"][0]
        if op["state"] in ("done", "failed", "superseded"):
            break
        time.sleep(1)
    t_first = op["history"][0]["t"]
    print("track op: " + " -> ".join(f"{h['state']}@{h['t'] - t_first:.0f}s" for h in op["history"])
          + f"  ({op['detail']})")
    if op["state"] != "done":
        return False

    errs = []
    for _ in range(20):
        time.sleep(1)
        vs = {v["identity"]["callsign"]: v for v in api(base, "/api/state")["vehicles"] if v.get("identity")}
        a, b = vs["TRK 1"], vs["REAPER 1"]
        want = bearing(*a["position"], *b["position"])
        errs.append(abs(wrap180(a["yaw"] - want)))
    print(f"tracker yaw vs bearing to the target over 20 s: mean {sum(errs) / len(errs):.1f} deg, "
          f"max {max(errs):.1f} deg; tracker mode {a['mode']}, tracking uid {a['tracking']}")
    good = max(errs) < 5.0 and a["tracking"] == tgt["uid"]
    print("ok: locked on and pointing" if good else "FAIL: not pointing at the target")
    return good


if __name__ == "__main__":
    main()
