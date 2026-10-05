#!/usr/bin/env python3
"""E2E: the blimp, UGV and sailboat stand-ins through the Rust bridge, tasked over its REST API.

Starts the Rust bridge and `swarm.py` with a fleet of lta, the four ugv_* platforms and
saildrone forced to `sim = "standin"`, waits for them to be ready, then
runs a chain of tasks per vehicle (patrol, rtl, orbit, hold); a task that waits for a human
(HTTP 202) is approved as op-1. Each op must end "done". Ports: vehicles UDP 19100+,
HTTP 8877, so it runs beside `make goldengate`.

    cargo build --release -p murmur-standins -p murmur-bridge
    .venv/bin/python sim/tests/standins_e2e_blimp_ugv_sailboat.py [--tidewater]

--tidewater gives the stand-ins (and the bridge) real conditions from a running
`tidewater serve` on port 8876 (blimp tasks may then not complete: the wind can beat it).
"""
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
HERE = ROOT / "sim" / "run" / "e2e-r3"
PY = str(ROOT / ".venv" / "bin" / "python") if (ROOT / ".venv").exists() else sys.executable
BASE_PORT = 19100
HTTP = 8877
BASE = f"http://127.0.0.1:{HTTP}"
TW = "--tidewater" in sys.argv

FLEET = """title = "R.3 blimp, ugv, sailboat stand-ins"
[[unit]]
platform = "lta"
sim = "standin"
spawn = [37.8330, -122.4830, 45, 0]
[[unit]]
platform = "ugv_logistics"
sim = "standin"
spawn = [37.8035, -122.4540, 14, 90]
[[unit]]
platform = "ugv_armed"
sim = "standin"
spawn = [37.8045, -122.4540, 9, 90]
[[unit]]
platform = "ugv_mines"
sim = "standin"
spawn = [37.8050, -122.4500, 15, 90]
[[unit]]
platform = "ugv_recon"
sim = "standin"
spawn = [37.8055, -122.4480, 7, 90]
[[unit]]
platform = "saildrone"
sim = "standin"
spawn = [37.8000, -122.5300, 0, 0]
"""


def api(path, body=None):
    req = urllib.request.Request(BASE + path, method="POST" if body is not None else "GET",
                                 data=None if body is None else json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", "X-Murmur-Operator": "op-1"})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}")


def offset(lon, lat, north_m, east_m):
    return [lon + east_m / (111_320.0 * math.cos(math.radians(lat))), lat + north_m / 111_320.0]


def task(body):
    """Submit; approve if it waits for a human. Returns the tasking id."""
    st, r = api("/api/tasks", body)
    if st == 202:
        print(f"  {body["task"]} on {body["uids"]}: 202, approving intent {r['intent_id']}: {r['detail'][:90]}")
        st, r = api(f"/api/intents/{r['intent_id']}/approve", {"note": "e2e"})
        if st != 200:
            raise SystemExit(f"approve failed {st}: {r}")
        return r["tasking"]["id"]
    if st != 200:
        raise SystemExit(f"task {body} refused {st}: {r}")
    return r["id"]


def main():
    HERE.mkdir(parents=True, exist_ok=True)
    fleet = HERE / "fleet.toml"
    fleet.write_text(FLEET)
    logs = HERE / "e2e"
    logs.mkdir(exist_ok=True)
    env = {**os.environ, "MURMUR_BASE_PORT": str(BASE_PORT),
           "MURMUR_HTTP_PORT": str(HTTP), "PYTHONUNBUFFERED": "1",
           "MURMUR_AUTH": "dev", "CARGO_BUILD_JOBS": "4"}
    for k in ("MURMUR_SCENARIO", "MURMUR_AT", "MURMUR_TIDEWATER_URL"):
        env.pop(k, None)
    if TW:
        env.update({"MURMUR_TIDEWATER_URL": "http://127.0.0.1:8876", "MURMUR_AT": "2026-06-14T10:00Z"})
    procs = [
        subprocess.Popen([str(ROOT / "target" / "release" / "murmur-bridge")], cwd=ROOT, env=env, stdout=open(logs / "bridge.log", "w"), stderr=subprocess.STDOUT,
                         start_new_session=True),
        subprocess.Popen([PY, str(ROOT / "sim" / "swarm.py"), "--fleet", str(fleet), "--base-port", str(BASE_PORT)],
                         env=env, stdout=open(logs / "swarm.log", "w"), stderr=subprocess.STDOUT,
                         start_new_session=True),
    ]
    try:
        ok = drive()
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


def drive():
    t0 = time.monotonic()
    by_call = {}
    while time.monotonic() - t0 < 300:
        try:
            st, s = api("/api/state")
            by_call = {v["identity"]["callsign"].split()[0]: v for v in s["vehicles"] if v.get("identity")}
            if len(by_call) == 6 and all(v["nav_ready"] for v in by_call.values()):
                break
        except OSError:
            pass
        time.sleep(1)
    else:
        print(f"vehicles not ready: {sorted(by_call)}")
        return False
    print(f"6 vehicles ready after {time.monotonic() - t0:.0f} s: "
          + ", ".join(f"{c} {v.get('kind')} mode {v.get('mode')}" for c, v in by_call.items()))
    sid = {c: v["sysid"] for c, v in by_call.items()}
    uid = {c: v.get("uid") for c, v in by_call.items()}
    pos = {c: v["position"] for c, v in by_call.items()}
    key = lambda c: uid[c] if uid[c] is not None else sid[c]  # noqa: E731

    def body(t, c, **kw):
        return {"task": t, "uids": [key(c)], **kw}

    # chains: each vehicle's tasks in order, each must end as "done"
    chains = {
        "ZEP": [body("patrol", "ZEP", route=[offset(*pos["ZEP"], 0, 120), offset(*pos["ZEP"], 100, 120)]),
                body("rtl", "ZEP"), body("orbit", "ZEP", center=offset(*pos["ZEP"], 0, 0), radius=80),
                body("hold", "ZEP")],
        "MULE": [body("patrol", "MULE", route=[offset(*pos["MULE"], 0, 80), offset(*pos["MULE"], -60, 80)]),
                 body("rtl", "MULE")],
        "BULL": [body("hold", "BULL")],
        "MOLE": [body("orbit", "MOLE", center=offset(*pos["MOLE"], 0, 0), radius=50), body("hold", "MOLE")],
        "FERRET": [body("rtl", "FERRET")],
        "ALBATROSS": [body("patrol", "ALBATROSS", route=[offset(*pos["ALBATROSS"], -150, 0),
                                                         offset(*pos["ALBATROSS"], -150, 150)]),
                      body("rtl", "ALBATROSS")],
    }
    running = {}  # callsign -> (tasking id, task name, t submitted)
    results = []
    deadline = time.monotonic() + 1500
    while time.monotonic() < deadline and (chains or running):
        for c in list(chains):
            if c not in running and chains[c]:
                b = chains[c].pop(0)
                running[c] = (task(b), b["task"], time.monotonic())
                if not chains[c]:
                    del chains[c]
        _, s = api("/api/state")
        snap = {t["id"]: t for t in s["taskings"]}
        for c, (tid, name, ts) in list(running.items()):
            ops = snap.get(tid, {}).get("ops", [])
            if ops and all(o["state"] in ("done", "failed", "superseded", "cancelled") for o in ops):
                for o in ops:
                    hist = " -> ".join(f"{h['state']}@{h['t'] - o['history'][0]['t']:.0f}s" for h in o["history"])
                    good = o["state"] == "done"
                    results.append(good)
                    print(f"{'ok  ' if good else 'FAIL'} {c:9s} {name:7s} {hist}  ({o['detail']})", flush=True)
                del running[c]
        time.sleep(1)
    for c, (tid, name, ts) in running.items():
        _, s = api("/api/state")
        t = next((t for t in s["taskings"] if t["id"] == tid), {})
        for o in t.get("ops", []):
            print(f"FAIL {c:9s} {name:7s} still {o['state']} after {time.monotonic() - ts:.0f} s ({o['detail']})")
        results.append(False)
    print("E2E OK" if all(results) and results else "E2E FAILED")
    return all(results)


if __name__ == "__main__":
    main()
