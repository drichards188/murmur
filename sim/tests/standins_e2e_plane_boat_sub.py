#!/usr/bin/env python3
"""E2E: the five R.5 stand-ins that imitate SITL platforms (puma, switchblade: plane; magura,
corsair: boat; bluerov: sub) through the Rust bridge, tasked over its REST API, and the same
tasks on the same platforms as ArduPilot SITL, compared loosely.

One run starts the bridge and `swarm.py` with a fleet of the five platforms forced to
`sim = "standin"` (or `"sitl"`), waits for every vehicle to be navigation-ready, then runs a
chain of tasks per vehicle (orbit, patrol, hold, rtl, as each kind can) and records how each
op ended, how long it took, and what the vehicle did meanwhile (its top ground speed against
the catalog's cruise speed, the altitude or depth it held). Tasks on the armed platforms
(switchblade, magura) need a human decision: the operator header makes the submission its
own authorization, or the 202 is approved as op-1 (sim/policy.toml). `--compare` puts the
two recordings side by side: every op must end the same way (done), durations within
DURATION_FACTOR, speeds and depths within tolerance; a difference outside that is a FAIL.

    cargo build --release -p murmur-standins -p murmur-bridge
    .venv/bin/python sim/tests/standins_e2e_plane_boat_sub.py --sim standin
    .venv/bin/python sim/tests/standins_e2e_plane_boat_sub.py --sim sitl
    .venv/bin/python sim/tests/standins_e2e_plane_boat_sub.py --compare

Ports: vehicles UDP 19000+ (stand-ins) or 19010+ (SITL), HTTP 8877; SITL instances 60-64
(TCP 6360+). Recordings go to sim/run/e2e-pbs/<sim>.json.
"""

import argparse
import json
import math
import os
import signal
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = ROOT / "sim" / "run" / "e2e-pbs"
PY = str(ROOT / ".venv" / "bin" / "python") if (ROOT / ".venv").exists() else sys.executable
HTTP = 8877
BASE = f"http://127.0.0.1:{HTTP}"
OPERATOR = {"X-Murmur-Operator": "op-1"}
# a task may take this many times longer (or shorter) on one than on the other
DURATION_FACTOR = 3.0
# top ground speed as a fraction of the catalog cruise, both sims: at least this
SPEED_FLOOR = 0.6
SPEED_CEIL = 1.5

SPAWN = {
    # platform: (callsign prefix, [lat, lon, alt, hdg]); planes at Crissy Field, boats west of
    # the Gate, the ROV west of Alcatraz
    "puma": ("PUMA", [37.8040, -122.4650, 3, 0]),
    "switchblade": ("BLADE", [37.8040, -122.4640, 3, 0]),
    "magura": ("VIPER", [37.8000, -122.5300, 0, 90]),
    "corsair": ("MARLIN", [37.8020, -122.5300, 0, 90]),
    "bluerov": ("EEL", [37.8250, -122.4400, 0, 90]),
}


def fleet_toml(sim):
    out = [f'title = "Plane, boat, sub stand-ins vs SITL (e2e, {sim})"']
    for pid, (_, spawn) in SPAWN.items():
        out.append(f'[[unit]]\nplatform = "{pid}"\nsim = "{sim}"\nspawn = {spawn}')
    return "\n".join(out) + "\n"


def api(path, body=None):
    req = urllib.request.Request(BASE + path, method="POST" if body is not None else "GET",
                                 data=None if body is None else json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", **OPERATOR})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}")


def offset(lon, lat, north_m, east_m):
    return [lon + east_m / (111_320.0 * math.cos(math.radians(lat))), lat + north_m / 111_320.0]


def task(body):
    """Submit; approve if it waits for a human. Returns (tasking id, how it was authorized)."""
    st, r = api("/api/tasks", body)
    if st == 202:
        st, a = api(f"/api/intents/{r['intent_id']}/approve", {"note": "e2e"})
        if st != 200 or not a.get("tasking"):
            raise SystemExit(f"approve {r['intent_id']}: {st} {a}")
        return a["tasking"]["id"], "approved in the inbox"
    if st != 200:
        raise SystemExit(f"task {body}: {st} {r}")
    return r["id"], "authorized at submission" if r.get("authorization") else "auto"


def chains(pos):
    """Per callsign, the tasks in order (each must end done)."""
    def body(t, c, **kw):
        return {"task": t, "callsign": c, **kw}

    p = {c: pos[c] for c in pos}
    return {
        "PUMA": [body("orbit", "PUMA", center=offset(*p["PUMA"], 600, 0), radius=200), body("hold", "PUMA"),
                 body("rtl", "PUMA")],
        "BLADE": [body("orbit", "BLADE", center=offset(*p["BLADE"], 500, 500), radius=150), body("rtl", "BLADE")],
        "VIPER": [body("patrol", "VIPER", route=[offset(*p["VIPER"], 0, 250), offset(*p["VIPER"], 250, 250)]),
                  body("hold", "VIPER"), body("rtl", "VIPER")],
        "MARLIN": [body("orbit", "MARLIN", center=offset(*p["MARLIN"], 150, 0), radius=60), body("hold", "MARLIN"),
                   body("rtl", "MARLIN")],
        "EEL": [body("patrol", "EEL", route=[offset(*p["EEL"], 0, 50), offset(*p["EEL"], 50, 50)]), body("hold", "EEL"),
                body("rtl", "EEL")],
    }


def run(sim, args):
    HERE.mkdir(parents=True, exist_ok=True)
    fleet = HERE / f"fleet-{sim}.toml"
    fleet.write_text(fleet_toml(sim))
    logs = HERE / sim
    logs.mkdir(exist_ok=True)
    base_port = 19000 if sim == "standin" else 19010
    env = {**os.environ, "MURMUR_BASE_PORT": str(base_port), "MURMUR_HTTP_PORT": str(HTTP),
           "MURMUR_ROOT": str(ROOT), "MURMUR_FLEET_MANIFEST": str(ROOT / "sim" / "run" / "fleet.json"),
           "MURMUR_RUN_ID": f"e2e-pbs-{sim}", "MURMUR_STANDINS_STATS": "0", "PYTHONUNBUFFERED": "1",
           "MURMUR_AUTH": "dev",
           "MURMUR_SITL_INSTANCE_BASE": str(args.instance)}
    for k in ("MURMUR_SCENARIO", "MURMUR_AT", "MURMUR_TIDEWATER_URL"):
        env.pop(k, None)
    bridge = ROOT / "target" / "release" / "murmur-bridge"
    for b in (bridge, ROOT / "target" / "release" / "murmur-standins"):
        if not b.exists():
            sys.exit(f"missing {b}; cargo build --release -p murmur-standins -p murmur-bridge")
    procs = [
        subprocess.Popen([str(bridge)], cwd=ROOT, env=env, stdout=open(logs / "bridge.log", "w"),
                         stderr=subprocess.STDOUT, start_new_session=True),
        subprocess.Popen([PY, str(ROOT / "sim" / "swarm.py"), "--fleet", str(fleet), "--base-port", str(base_port)],
                         env=env, stdout=open(logs / "swarm.log", "w"), stderr=subprocess.STDOUT,
                         start_new_session=True),
    ]
    try:
        result = drive(sim, args)
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
    (HERE / f"{sim}.json").write_text(json.dumps(result, indent=1))
    ok = all(t["state"] == "done" for v in result["vehicles"].values() for t in v["tasks"])
    print(f"{sim}: {'all ops done' if ok else 'some ops did not end done'}; recorded in {HERE / f'{sim}.json'}")
    return ok


def drive(sim, args):
    t0 = time.monotonic()
    by_call = {}
    while time.monotonic() - t0 < 360:
        try:
            _, s = api("/api/state")
            by_call = {v["identity"]["callsign"].split()[0]: v for v in s["vehicles"] if v.get("identity")}
            if len(by_call) == len(SPAWN) and all(v["nav_ready"] and v.get("kind") for v in by_call.values()):
                break
        except OSError:
            pass
        time.sleep(1)
    else:
        raise SystemExit(f"vehicles not ready: {sorted(by_call)}")
    ready_s = time.monotonic() - t0
    print(f"{sim}: {len(by_call)} vehicles ready after {ready_s:.0f} s: "
          + ", ".join(f"{c} {v.get('kind')} {v.get('mode')}" for c, v in by_call.items()), flush=True)
    uid = {c: v["uid"] for c, v in by_call.items()}
    pos = {c: v["position"] for c, v in by_call.items()}
    plan = chains(pos)
    for c, bodies in plan.items():
        for b in bodies:
            b["uids"] = [uid[b.pop("callsign")]]
    catalog = tomllib.loads((ROOT / "sim" / "platforms.toml").read_text())
    platform = {c: v["identity"]["platform"] for c, v in by_call.items()}
    tele = {c: {"max_groundspeed": 0.0, "max_alt_rel": -1e9, "min_alt_rel": 1e9} for c in by_call}
    running = {}  # callsign -> (tasking id, task name, submitted at, how authorized)
    results = {c: [] for c in by_call}
    deadline = time.monotonic() + args.timeout
    while time.monotonic() < deadline and (plan or running):
        for c in list(plan):
            if c not in running and plan[c]:
                b = plan[c].pop(0)
                tid, how = task(b)
                running[c] = (tid, b["task"], time.monotonic(), how)
                print(f"  {sim}: {c} {b['task']} submitted ({how})", flush=True)
                if not plan[c]:
                    del plan[c]
        _, s = api("/api/state")
        for v in s["vehicles"]:
            c = v["identity"]["callsign"].split()[0] if v.get("identity") else None
            if c in tele:
                if v.get("groundspeed") is not None:
                    tele[c]["max_groundspeed"] = max(tele[c]["max_groundspeed"], v["groundspeed"])
                if v.get("alt_rel") is not None:
                    tele[c]["max_alt_rel"] = max(tele[c]["max_alt_rel"], v["alt_rel"])
                    tele[c]["min_alt_rel"] = min(tele[c]["min_alt_rel"], v["alt_rel"])
        snap = {t["id"]: t for t in s["taskings"]}
        for c, (tid, name, ts, how) in list(running.items()):
            ops = snap.get(tid, {}).get("ops", [])
            if ops and all(o["state"] in ("done", "failed", "superseded", "cancelled") for o in ops):
                for o in ops:
                    t_first = o["history"][0]["t"]
                    hist = [(h["state"], round(h["t"] - t_first, 1)) for h in o["history"]]
                    results[c].append({"task": name, "state": o["state"], "detail": o["detail"],
                                       "duration_s": round(o["history"][-1]["t"] - t_first, 1), "history": hist,
                                       "authorized": how})
                    print(f"  {sim}: {'ok  ' if o['state'] == 'done' else 'FAIL'} {c:7s} {name:7s} "
                          + " -> ".join(f"{st}@{t:.0f}s" for st, t in hist) + f"  ({o['detail']})", flush=True)
                del running[c]
        time.sleep(1)
    for c, (tid, name, ts, how) in running.items():
        results[c].append({"task": name, "state": "unfinished", "detail": f"still running after {time.monotonic() - ts:.0f} s",
                           "duration_s": None, "history": [], "authorized": how})
        print(f"  {sim}: FAIL {c:7s} {name:7s} unfinished", flush=True)
    return {"sim": sim, "ready_s": round(ready_s), "vehicles": {
        c: {"platform": platform[c], "kind": by_call[c].get("kind"), "link": by_call[c]["identity"].get("link"),
            "cruise_mps": catalog[platform[c]].get("cruise_mps"), "catalog_params": catalog[platform[c]].get("params"),
            "tasks": results[c], "telemetry": {k: round(v, 1) for k, v in tele[c].items()}}
        for c in by_call}}


def compare():
    paths = {sim: HERE / f"{sim}.json" for sim in ("standin", "sitl")}
    missing = [str(p) for p in paths.values() if not p.exists()]
    if missing:
        sys.exit(f"missing recordings: {missing}")
    rec = {sim: json.loads(p.read_text()) for sim, p in paths.items()}
    ok = True
    lines = [f"ready: stand-ins {rec['standin']['ready_s']} s, SITL {rec['sitl']['ready_s']} s"]

    def check(cond, what, soft=False):
        nonlocal ok
        if not cond and not soft:
            ok = False
        lines.append(("  ok   " if cond else ("  note " if soft else "  FAIL ")) + what)

    for c in rec["standin"]["vehicles"]:
        a, b = rec["standin"]["vehicles"][c], rec["sitl"]["vehicles"].get(c)
        lines.append(f"{c} ({a['platform']}, {a['kind']}, {a['link']}; catalog cruise {a['cruise_mps']} m/s, "
                     f"params {a['catalog_params']})")
        if b is None:
            check(False, "missing from the SITL recording")
            continue
        check(a["kind"] == b["kind"], f"kind {a['kind']} vs {b['kind']}")
        ta = {t["task"]: t for t in a["tasks"]}
        tb = {t["task"]: t for t in b["tasks"]}
        for name in ta:
            x, y = ta[name], tb.get(name)
            if y is None:
                check(False, f"{name}: missing from the SITL recording")
                continue
            same = x["state"] == y["state"] == "done"
            check(same, f"{name:7s} stand-in {x['state']} in {x['duration_s']} s ({x['detail']}); "
                        f"SITL {y['state']} in {y['duration_s']} s ({y['detail']})")
            if x["duration_s"] and y["duration_s"] and max(x["duration_s"], y["duration_s"]) > 10:
                r = max(x["duration_s"], y["duration_s"]) / max(min(x["duration_s"], y["duration_s"]), 1.0)
                check(r <= DURATION_FACTOR, f"{name:7s} duration ratio {r:.1f} (factor {DURATION_FACTOR})", soft=True)
            check(x["authorized"] == y["authorized"], f"{name:7s} authorization: {x['authorized']} vs {y['authorized']}")
        cruise = a["cruise_mps"] or 0.0
        va, vb = a["telemetry"]["max_groundspeed"], b["telemetry"]["max_groundspeed"]
        if cruise > 0:
            check(SPEED_FLOOR * cruise <= va <= SPEED_CEIL * cruise and SPEED_FLOOR * cruise <= vb <= SPEED_CEIL * cruise,
                  f"top ground speed: stand-in {va} m/s, SITL {vb} m/s (catalog cruise {cruise} m/s)")
        if a["kind"] == "sub":
            da, db = -a["telemetry"]["min_alt_rel"], -b["telemetry"]["min_alt_rel"]
            check(abs(da - db) <= 2.0, f"deepest: stand-in {da:.1f} m, SITL {db:.1f} m (the task depth)")
        else:
            lines.append(f"  info highest: stand-in {a['telemetry']['max_alt_rel']} m, SITL {b['telemetry']['max_alt_rel']} m")
    print("\n".join(lines))
    print("MATCH" if ok else "DIFFERENCES (see above)")
    return ok


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--sim", choices=["standin", "sitl"], help="record this backend")
    ap.add_argument("--compare", action="store_true", help="compare the two recordings")
    ap.add_argument("--instance", type=int, default=60, help="first SITL instance (60-64 used)")
    ap.add_argument("--timeout", type=float, default=900.0, help="seconds for all ops to settle")
    args = ap.parse_args()
    if not args.sim and not args.compare:
        ap.error("--sim standin|sitl and/or --compare")
    ok = True
    if args.sim:
        ok &= run(args.sim, args)
    if args.compare:
        ok &= compare()
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
