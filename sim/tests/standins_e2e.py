#!/usr/bin/env python3
"""End to end: the stand-in-only fleet through the bridge, tasked over its REST API.

Starts the Rust bridge and `swarm.py --fleet standins-only` (the Rust stand-in engine; both
release binaries must be built), waits for every vehicle to be navigation-ready, then issues
orbit / patrol / rtl tasks (and a hold and an orbit the drifting buoy must refuse) and
follows each op until it settles. Tasks the decision layer holds for approval (202) are
approved as operator op-1. The surface vessel and the AUV are sent home after their patrols
(the AUV submerged). --plane-rtl also sends a fixed wing home once its orbit is on station.
Also checks that the submerged AUV's position keeps updating (HIGH_LATENCY2) and that no
acoustic vehicle raises a lost-link alert. Prints each op's state history; exit 0 when
everything ends as expected.

    .venv/bin/python sim/tests/standins_e2e.py
    .venv/bin/python sim/tests/standins_e2e.py --plane-rtl
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


def task(base, body):
    """POST a task; approve it if the decision layer holds it. Returns the tasking id, or None
    when the bridge refused it before dispatch (printed)."""
    try:
        t = api(base, "/api/tasks", body)
    except urllib.error.HTTPError as e:
        print(f"bridge refused {body['task']}: {e.code} {json.loads(e.read()).get('detail')}")
        return None
    if t.get("id") is None and t.get("intent_id") is not None:
        t = api(base, f"/api/intents/{t['intent_id']}/approve", {"note": "e2e"})["tasking"]
    return t["id"]


def offset(lon, lat, north_m, east_m):
    return [lon + east_m / (111_320.0 * math.cos(math.radians(lat))), lat + north_m / 111_320.0]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--plane-rtl", action="store_true", help="also RTL a fixed wing after its orbit")
    ap.add_argument("--base-port", type=int, default=15560)
    ap.add_argument("--http-port", type=int, default=8865)
    ap.add_argument("--timeout", type=float, default=420.0, help="seconds for all ops to settle")
    ap.add_argument("--python", default=str(PY if PY.exists() else sys.executable))
    args = ap.parse_args()
    base = f"http://127.0.0.1:{args.http_port}"
    logs = ROOT / "logs"
    logs.mkdir(exist_ok=True)

    env = {**os.environ, "MURMUR_BASE_PORT": str(args.base_port),
           "MURMUR_HTTP_PORT": str(args.http_port), "PYTHONUNBUFFERED": "1"}
    for k in ("MURMUR_SCENARIO", "MURMUR_AT", "MURMUR_TIDEWATER_URL"):
        env.pop(k, None)
    bridge = subprocess.Popen([str(ROOT / "target" / "release" / "murmur-bridge")], cwd=ROOT,
                              env={**env, "MURMUR_ROOT": str(ROOT)},
                              stdout=open(logs / "e2e-bridge.log", "w"),
                              stderr=subprocess.STDOUT, start_new_session=True)
    procs = [
        bridge,
        subprocess.Popen([args.python, str(ROOT / "sim" / "swarm.py"), "--fleet", "standins-only",
                          "--base-port", str(args.base_port)], env=env,
                         stdout=open(logs / "e2e-swarm.log", "w"), stderr=subprocess.STDOUT,
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
    print(f"7 vehicles ready after {time.monotonic() - t0:.0f} s")
    # the bridge keys vehicles by uid (a sysid is only unique per link)
    key = "uid"
    sid = {c.split()[0]: v[key] for c, v in by_call.items()}
    pos = {c.split()[0]: v["position"] for c, v in by_call.items()}

    # (task body, what each op must end as). "refused": the decision layer refuses it before
    # dispatch, or the vehicle rejects it.
    # A drifting buoy has no propulsion: it can't hold position, let alone orbit.
    plan = [
        ({"task": "orbit", f"{key}s": [sid["HAWK"], sid["REAPER"], sid["FURY"]], "center": [-122.68, 37.78],
          "radius": 5000}, "done"),
        ({"task": "patrol", f"{key}s": [sid["HUNTER"]],
          "route": [offset(*pos["HUNTER"], 250, 0), offset(*pos["HUNTER"], 250, 250)]}, "done"),
        ({"task": "patrol", f"{key}s": [sid["REMUS"]],
          "route": [offset(*pos["REMUS"], 60, 0), offset(*pos["REMUS"], 60, 60)]}, "done"),
        ({"task": "hold", f"{key}s": [sid["DRIFT"]]}, "refused"),
        ({"task": "orbit", f"{key}s": [sid["DRIFT"]], "center": offset(*pos["DRIFT"], 100, 0), "radius": 200},
         "refused"),
        ({"task": "rtl", f"{key}s": [sid["ORCA"]]}, "done"),
    ]
    taskings = []
    for body, want in plan:
        taskings.append((task(base, body), body["task"], want))
    # sent home once their patrols complete: HUNTER on the surface, REMUS submerged (its
    # RTL must complete on dead reckoning, reported by HIGH_LATENCY2 over its acoustic link)
    after_patrol = {1: {"task": "rtl", f"{key}s": [sid["HUNTER"]]},
                    2: {"task": "patrol", f"{key}s": [sid["REMUS"]],
                        "route": [offset(*pos["REMUS"], 40, 40), offset(*pos["REMUS"], 40, 80)]}}
    # a patrol sent while REMUS is submerged (reports every 20 s) must pass the link-aware
    # freshness rules; its RTL follows when that patrol completes
    rtl_remus = {"task": "rtl", f"{key}s": [sid["REMUS"]]}
    submerged_now = False
    plane_rtl = {"task": "rtl", f"{key}s": [sid["REAPER"]]} if args.plane_rtl else None
    acoustic = {sid["REMUS"], sid["ORCA"]}
    # REMUS positions while it reports by HIGH_LATENCY2, and lost-link alerts on acoustic vehicles
    submerged_fixes, lost_alerts = set(), []

    deadline = time.monotonic() + args.timeout
    while time.monotonic() < deadline:
        state = api(base, "/api/state")
        for v in state["vehicles"]:
            if v[key] == sid["REMUS"]:
                submerged_now = v["link"].get("expected_interval_ms", 0) > 1000
            if v[key] == sid["REMUS"] and v["link"].get("expected_interval_ms", 0) > 1000 and v["position"]:
                submerged_fixes.add(tuple(v["position"]))
        lost_alerts += [a for a in api(base, "/api/inbox").get("alerts", [])
                        if a["kind"] == "lost_link" and a["uid"] in acoustic]
        snap = {t["id"]: t for t in state["taskings"]}
        ops = {tid: snap[tid]["ops"] for tid, _, _ in taskings if tid in snap}
        reaper = [o for o in ops.get(taskings[0][0], []) if o[key] == sid["REAPER"]]
        if plane_rtl and reaper and reaper[0]["state"] == "done":
            taskings.append((task(base, plane_rtl), "rtl (plane)", "done"))
            plane_rtl = None
        for i in list(after_patrol):
            patrol = taskings[i]
            if all(o.get("state") == "done" for o in ops.get(patrol[0], [{}])):
                if i == 2 and not submerged_now:
                    continue
                body = after_patrol.pop(i)
                taskings.append((task(base, body), f"{body['task']} (after {patrol[1]})", "done"))
                if i == 2:
                    after_patrol[len(taskings) - 1] = rtl_remus
        settled = all(o.get("state") in ("done", "failed", "superseded")
                      for tid, _, _ in taskings if tid is not None for o in ops.get(tid, [{}]))
        if settled and not after_patrol and not plane_rtl:
            break
        time.sleep(1)

    snap = {t["id"]: t for t in api(base, "/api/state")["taskings"]}
    names = {v[key]: c for c, v in by_call.items()}
    ok = True
    good = len(submerged_fixes) >= 3
    ok &= good
    print(f"{'ok  ' if good else 'FAIL'} REMUS submerged: {len(submerged_fixes)} distinct positions "
          "reported by HIGH_LATENCY2")
    good = not lost_alerts
    ok &= good
    print(f"{'ok  ' if good else 'FAIL'} acoustic vehicles: {len(lost_alerts)} lost-link alert samples"
          + (f" ({lost_alerts[0]['detail']})" if lost_alerts else ""))
    for tid, what, want in taskings:
        if tid is None:
            # refused by the bridge's decision layer before reaching the vehicle
            good = want == "refused"
            ok &= good
            print(f"{'ok  ' if good else 'FAIL'} {what:20s} refused by the bridge before dispatch")
            continue
        if want == "refused":
            want = "failed"  # passed the decision layer, so the vehicle must refuse it
        for op in snap[tid]["ops"]:
            # superseded is how an earlier task ends when a later one replaces it on purpose
            good = op["state"] == want or (op["state"] == "superseded" and want == "done")
            ok &= good
            t_first = op["history"][0]["t"]
            hist = " -> ".join(f"{h['state']}@{h['t'] - t_first:.0f}s" for h in op["history"])
            print(f"{'ok  ' if good else 'FAIL'} {what:20s} {names[op[key]]:9s} {hist}  ({op['detail']})")
    return ok


if __name__ == "__main__":
    main()
