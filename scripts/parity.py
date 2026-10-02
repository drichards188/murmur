#!/usr/bin/env python3
"""Parity between the Python and the Rust bridge on a live fleet.

For each bridge in turn: launch the fleet (fresh), start the bridge, wait until the
vehicles are ready, capture /api/state, /api/platforms, /api/stats rates and a WebSocket
sample, then run the same tasks on several platform kinds to completion. Then compare.

    .venv/bin/python scripts/parity.py run                 # full-spectrum, both bridges
    .venv/bin/python scripts/parity.py compare A.json B.json

Runs on side ports by default (vehicles 16560+, HTTP 8965) so it can sit beside a normal
stack; SITL instances still use TCP 5760+10*i, so only one SITL fleet at a time.
"""

import argparse
import asyncio
import json
import os
import signal
import statistics
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

import websockets

ROOT = Path(__file__).resolve().parents[1]

# (phase, task, callsign, params): phase 2 starts when phase 1 has finished
TASKS = [
    (1, "orbit", "PUMA 1", {"center": [-122.4400, 37.8150], "radius": 300}),  # SITL plane
    (1, "patrol", "SKY 1", {"route": [[-122.4430, 37.8060], [-122.4430, 37.8080]]}),  # SITL copter
    (1, "patrol", "VIPER 1", {"route": [[-122.4700, 37.8260], [-122.4670, 37.8260]]}),  # SITL boat
    (1, "patrol", "EEL 1", {"route": [[-122.4690, 37.8292], [-122.4678, 37.8292]]}),  # SITL sub
    (1, "orbit", "REAPER 1", {"center": [-122.5800, 37.8800], "radius": 500}),  # stand-in plane
    (1, "orbit", "REMUS 1", {"center": [-122.4400, 37.8270], "radius": 150}),  # stand-in sub
    (1, "track", "TRK 1", {"target": "PUMA 1"}),  # antenna tracker
    (2, "rtl", "SKY 1", {}),
    (2, "hold", "VIPER 1", {}),
    (2, "hold", "EEL 1", {}),
    (2, "rtl", "MULE 1", {}),  # SITL UGV, already home
    (2, "rtl", "HUNTER 1", {}),  # stand-in boat, already home
    (2, "orbit", "DRIFT 1", {"center": [-122.49, 37.815]}),  # a drifter can't: fails at once
]
PHASE_TIMEOUT_S = 300


def http(base, path, body=None):
    req = urllib.request.Request(base + path, method="POST" if body is not None else "GET",
                                 data=None if body is None else json.dumps(body).encode(),
                                 headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")


def launch(cmd, log, env=None, cwd=ROOT):
    return subprocess.Popen(cmd, cwd=cwd, env={**os.environ, **(env or {})}, stdout=open(log, "w"),
                            stderr=subprocess.STDOUT, start_new_session=True)


def stop(p):
    try:
        os.killpg(p.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        p.wait(15)
    except subprocess.TimeoutExpired:
        os.killpg(p.pid, signal.SIGKILL)
        p.wait()


def wait_ready(base, expected, needed, timeout=240):
    end = time.time() + timeout
    s, ready = {"vehicles": []}, []
    while time.time() < end:
        try:
            _, s = http(base, "/api/state")
        except OSError:
            time.sleep(2)
            continue
        by_cs = {(v["identity"] or {}).get("callsign"): v for v in s["vehicles"]}
        ready = [cs for cs in needed if by_cs.get(cs, {}).get("nav_ready")]
        if len(s["vehicles"]) >= expected and len(ready) == len(needed):
            return s
        time.sleep(2)
    raise SystemExit(f"fleet not ready after {timeout}s: {len(s['vehicles'])}/{expected} vehicles, "
                     f"not ready: {sorted(set(needed) - set(ready))}")


async def ws_sample(base, seconds):
    out = {"first": None, "deltas": 0, "bytes": 0, "top_keys": set(), "vehicle_keys": set(), "with_taskings": 0}
    async with websockets.connect(base.replace("http", "ws") + "/ws", max_size=None) as ws:
        first = json.loads(await ws.recv())
        out["first"] = {"type": first["type"], "keys": sorted(first), "bytes": len(json.dumps(first)),
                        "vehicle_keys": sorted(first["vehicles"][0]) if first["vehicles"] else []}
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            try:
                raw = await asyncio.wait_for(ws.recv(), timeout=max(0.1, end - time.monotonic()))
            except asyncio.TimeoutError:
                break
            msg = json.loads(raw)
            out["deltas"] += 1
            out["bytes"] += len(raw)
            out["top_keys"] |= set(msg)
            out["with_taskings"] += "taskings" in msg
            for v in msg["vehicles"]:
                out["vehicle_keys"] |= set(v)
    out["top_keys"], out["vehicle_keys"] = sorted(out["top_keys"]), sorted(out["vehicle_keys"])
    out["deltas_per_s"] = out["deltas"] / seconds
    out["kb_per_s"] = out["bytes"] / seconds / 1024
    return out


def capture(base, seconds):
    _, state = http(base, "/api/state")
    _, platforms = http(base, "/api/platforms")
    _, s0 = http(base, "/api/stats")
    t0 = time.monotonic()
    ws = asyncio.run(ws_sample(base, seconds))
    _, s1 = http(base, "/api/stats")
    dt = time.monotonic() - t0
    rates = {}
    for uid, c1 in s1["vehicles"].items():
        c0 = s0["vehicles"].get(uid)
        if not c0:
            continue
        rates[uid] = {"rx": (c1["rx_total"] - c0["rx_total"]) / dt, "tx": (c1["tx_total"] - c0["tx_total"]) / dt,
                      "by_type": {k: round((n - c0["by_type"].get(k, 0)) / dt, 2) for k, n in c1["by_type"].items()
                                  if n - c0["by_type"].get(k, 0) > 0}}
    return {"state": state, "platforms": platforms, "stats_keys": sorted(s1), "rates": rates, "ws": ws}


def run_tasks(base, uid_of):
    results = []
    for phase in sorted({t[0] for t in TASKS}):
        pending = []
        for _, task, cs, params in [t for t in TASKS if t[0] == phase]:
            body = {"task": task, "uids": [uid_of[cs]], **params}
            if "target" in params:
                body["target"] = uid_of[params["target"]]
            code, resp = http(base, "/api/tasks", body)
            if code != 200:
                results.append({"task": task, "callsign": cs, "http": code, "error": resp})
                continue
            pending.append((task, cs, resp["id"], time.time()))
        end = time.time() + PHASE_TIMEOUT_S
        while pending:
            _, state = http(base, "/api/state")
            ops = {(t["id"], o["uid"]): (t, o) for t in state["taskings"] for o in t["ops"]}
            for item in list(pending):
                task, cs, tid, t0 = item
                tasking, op = ops[(tid, uid_of[cs])]
                finished = op["state"] in ("done", "failed", "superseded")
                if finished or time.time() > end:
                    hist = [h["state"] for h in op["history"]]
                    t_end = op["history"][-1]["t"] if finished else time.time()
                    results.append({"task": task, "callsign": cs, "http": 200,
                                    "state": op["state"] if finished else f"{op['state']} (timed out)",
                                    "detail": op["detail"], "progress": op["progress"], "history": hist,
                                    "seconds": round(t_end - tasking["created"], 1),
                                    "op_keys": sorted(op), "tasking_keys": sorted(tasking),
                                    "params": tasking["params"]})
                    pending.remove(item)
            time.sleep(1)
    return results


def run_one(bridge, args, out_dir):
    manifest_count = int(subprocess.check_output(
        [sys.executable, "sim/swarm.py", "--fleet", args.fleet, "--count-only"], cwd=ROOT))
    env = {"MURMUR_BASE_PORT": str(args.base_port), "MURMUR_HTTP_PORT": str(args.http_port)}
    base = f"http://127.0.0.1:{args.http_port}"
    print(f"[{bridge}] launching {args.fleet} ({manifest_count} vehicles) on udp {args.base_port}+", flush=True)
    sim = launch([sys.executable, "sim/swarm.py", "--fleet", args.fleet, "--base-port", str(args.base_port)],
                 out_dir / f"{bridge}-sim.log")
    if bridge == "rust":
        cmd, cwd = [str(ROOT / "target/release/murmur-bridge")], ROOT
    else:
        cmd, cwd = [sys.executable, "-m", "murmur_bridge"], ROOT / "bridge"
    br = launch(cmd, out_dir / f"{bridge}-bridge.log", env=env, cwd=cwd)
    try:
        needed = sorted({t[2] for t in TASKS} | {t[3]["target"] for t in TASKS if "target" in t[3]})
        needed.remove("DRIFT 1")
        state = wait_ready(base, manifest_count, needed)
        print(f"[{bridge}] ready; settling", flush=True)
        time.sleep(20)  # telemetry plan applied, Q_ENABLE read, link stats warmed up
        cap = capture(base, args.seconds)
        uid_of = {(v["identity"] or {}).get("callsign"): v["uid"] for v in state["vehicles"]}
        print(f"[{bridge}] running {len(TASKS)} tasks", flush=True)
        cap["tasks"] = run_tasks(base, uid_of)
        cap["bridge"] = bridge
        for r in cap["tasks"]:
            print(f"[{bridge}]   {r['task']:6s} {r['callsign']:9s} {r.get('state', r.get('http'))}: "
                  f"{r.get('detail', r.get('error'))} ({r.get('seconds')} s)", flush=True)
        return cap
    finally:
        stop(sim)
        stop(br)


# ---- comparison --------------------------------------------------------------------------

def shape(x):
    """Type skeleton: dict keys with value types (merged over list items)."""
    if isinstance(x, dict):
        return {k: shape(v) for k, v in x.items()}
    if isinstance(x, list):
        merged = {}
        for item in x:
            s = shape(item)
            if isinstance(s, dict) and isinstance(merged, dict):
                for k, v in s.items():
                    if v != "null" or k not in merged:
                        merged[k] = v
            else:
                merged = s
        return [merged] if x else []
    if isinstance(x, bool):
        return "bool"
    if isinstance(x, int):
        return "number"  # JSON has one number type; 5 and 5.0 mean the same to the console
    if isinstance(x, float):
        return "number"
    return "null" if x is None else type(x).__name__


def diff_shape(a, b, path=""):
    out = []
    if isinstance(a, dict) and isinstance(b, dict):
        for k in sorted(set(a) | set(b)):
            if k not in b:
                out.append(f"{path}.{k}: only in A")
            elif k not in a:
                out.append(f"{path}.{k}: only in B")
            else:
                out += diff_shape(a[k], b[k], f"{path}.{k}")
    elif isinstance(a, list) and isinstance(b, list):
        if a and b:
            out += diff_shape(a[0], b[0], path + "[]")
    elif a != b and "null" not in (a, b):
        out.append(f"{path}: {a} vs {b}")
    return out


def compare(a, b):
    na, nb = a.get("bridge", "A"), b.get("bridge", "B")
    lines = [f"# Bridge parity: {na} (A) vs {nb} (B)", ""]

    lines.append("## /api/state shape")
    d = diff_shape(shape(a["state"]), shape(b["state"]))
    lines += [f"- {x}" for x in d] or ["- identical (keys and value types, all vehicles merged)"]

    lines += ["", "## /api/state values per vehicle (at ready)"]
    va = {v["uid"]: v for v in a["state"]["vehicles"]}
    vb = {v["uid"]: v for v in b["state"]["vehicles"]}
    lines.append(f"- vehicles: {len(va)} vs {len(vb)}; same uids: {sorted(va) == sorted(vb)}")
    fields = ("sysid", "identity", "kind", "mode", "armed", "nav_ready", "gps_fix")
    same = {f: 0 for f in fields}
    diffs = []
    for uid in sorted(set(va) & set(vb)):
        for f in fields:
            if va[uid][f] == vb[uid][f]:
                same[f] += 1
            else:
                diffs.append(f"uid {uid} {(va[uid]['identity'] or {}).get('callsign')}: {f} {va[uid][f]!r} vs {vb[uid][f]!r}")
    n = len(set(va) & set(vb))
    lines.append("- equal: " + ", ".join(f"{f} {same[f]}/{n}" for f in fields))
    lines += [f"  - {x}" for x in diffs]
    homes = []
    for uid in sorted(set(va) & set(vb)):
        ha, hb = va[uid]["home"], vb[uid]["home"]
        if ha and hb:
            homes.append(abs(ha[0] - hb[0]) * 88_000 + abs(ha[1] - hb[1]) * 111_000)
    if homes:
        lines.append(f"- home position, run to run: median {statistics.median(homes):.1f} m, max {max(homes):.1f} m")
    lr = [(v["link"]["loss_pct"], v["link"]["rtt_ms"]) for v in vb.values()]
    lines.append(f"- B link: loss_pct reported for {sum(x[0] is not None for x in lr)}/{len(lr)}, "
                 f"rtt_ms for {sum(x[1] is not None for x in lr)}/{len(lr)}")

    lines += ["", "## /api/platforms"]
    pa = {p["id"]: p for p in a["platforms"]["platforms"]}
    pb = {p["id"]: p for p in b["platforms"]["platforms"]}
    lines.append(f"- platforms: {len(pa)} vs {len(pb)}, same ids and order: {list(pa) == list(pb)}")
    mism, extra = [], set()
    for pid in pa:
        for k, v in pa[pid].items():
            if pb.get(pid, {}).get(k) != v:
                mism.append(f"{pid}.{k}: {v!r} vs {pb.get(pid, {}).get(k)!r}")
        extra |= set(pb.get(pid, {})) - set(pa[pid])
    lines += [f"- every field of A equal in B: {not mism}"] + [f"  - {m}" for m in mism]
    if extra:
        lines.append(f"- fields only in B (passed through from platforms.toml): {', '.join(sorted(extra))}")

    lines += ["", "## /api/stats (telemetry received, msg/s)"]
    for name, cap in ((na, a), (nb, b)):
        rx = sorted(r["rx"] for r in cap["rates"].values())
        tx = sorted(r["tx"] for r in cap["rates"].values())
        lines.append(f"- {name}: total {sum(rx):.0f} msg/s, median {statistics.median(rx):.1f}/vehicle, "
                     f"max {max(rx):.1f}; to vehicles {sum(tx):.1f} msg/s")
    by_kind = {}
    for cap, col in ((a, 0), (b, 1)):
        for uid, r in cap["rates"].items():
            v = {x["uid"]: x for x in cap["state"]["vehicles"]}.get(int(uid))
            if not v:
                continue
            key = f"{v['kind']} ({(v['identity'] or {}).get('sim')})"
            by_kind.setdefault(key, [[], []])[col].append(r["rx"])
    lines.append("- median rx per vehicle by kind: " + "; ".join(
        f"{k} {statistics.median(x[0]):.1f} vs {statistics.median(x[1]):.1f}" for k, x in sorted(by_kind.items())
        if x[0] and x[1]))
    types_a, types_b = {}, {}
    for cap, acc in ((a, types_a), (b, types_b)):
        for r in cap["rates"].values():
            for t, hz in r["by_type"].items():
                acc[t] = acc.get(t, 0) + hz
    top = sorted(set(types_a) | set(types_b), key=lambda t: -(types_a.get(t, 0) + types_b.get(t, 0)))[:12]
    lines.append("- by message type, fleet total: " + "; ".join(
        f"{t} {types_a.get(t, 0):.1f} vs {types_b.get(t, 0):.1f}" for t in top))

    lines += ["", "## WebSocket"]
    for name, cap in ((na, a), (nb, b)):
        w = cap["ws"]
        lines.append(f"- {name}: full state {w['first']['bytes'] / 1024:.1f} KB, then {w['deltas_per_s']:.2f} deltas/s, "
                     f"{w['kb_per_s']:.1f} KB/s")
    for key in ("top_keys", "vehicle_keys"):
        ka, kb = set(a["ws"][key]), set(b["ws"][key])
        lines.append(f"- delta {key}: {'same' if ka == kb else f'A only {sorted(ka - kb)}, B only {sorted(kb - ka)}'}")
    fa, fb = a["ws"]["first"], b["ws"]["first"]
    lines.append(f"- first message keys: {'same' if fa['keys'] == fb['keys'] else (fa['keys'], fb['keys'])}; "
                 f"vehicle keys: {'same' if fa['vehicle_keys'] == fb['vehicle_keys'] else (fa['vehicle_keys'], fb['vehicle_keys'])}")

    lines += ["", "## Tasks", "", f"| task | vehicle | {na} | {nb} |", "|---|---|---|---|"]
    tb = {(r["task"], r["callsign"]): r for r in b["tasks"]}
    agree = 0
    for r in a["tasks"]:
        s = tb.get((r["task"], r["callsign"]), {})
        fmt = lambda x: (f"{x.get('state', x.get('http'))}: {x.get('detail', x.get('error'))} "  # noqa: E731
                         f"({x.get('seconds')} s; {'>'.join(h[:3] for h in x.get('history', []))})")
        lines.append(f"| {r['task']} | {r['callsign']} | {fmt(r)} | {fmt(s)} |")
        agree += r.get("state") == s.get("state") and r.get("history") == s.get("history")
    lines.append(f"\nsame final state and lifecycle: {agree}/{len(a['tasks'])}")
    keys_a = {k for r in a["tasks"] for k in r.get("op_keys", [])}
    keys_b = {k for r in b["tasks"] for k in r.get("op_keys", [])}
    lines.append(f"op keys: {'same' if keys_a == keys_b else (sorted(keys_a), sorted(keys_b))}")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--fleet", default="full-spectrum")
    r.add_argument("--bridges", default="python,rust")
    r.add_argument("--base-port", type=int, default=16560)
    r.add_argument("--http-port", type=int, default=8965)
    r.add_argument("--seconds", type=float, default=20, help="stats and WebSocket sample length")
    r.add_argument("--out", default="logs/parity")
    c = sub.add_parser("compare")
    c.add_argument("a")
    c.add_argument("b")
    args = ap.parse_args()

    if args.cmd == "compare":
        print(compare(json.loads(Path(args.a).read_text()), json.loads(Path(args.b).read_text())))
        return
    out_dir = ROOT / args.out
    out_dir.mkdir(parents=True, exist_ok=True)
    bridges = args.bridges.split(",")
    if "rust" in bridges:
        subprocess.check_call(["cargo", "build", "--release", "-p", "murmur-bridge"], cwd=ROOT,
                              env={**os.environ, "PATH": f"{Path.home() / '.cargo/bin'}:{os.environ['PATH']}"})
    caps = []
    for bridge in bridges:
        cap = run_one(bridge, args, out_dir)
        (out_dir / f"{bridge}.json").write_text(json.dumps(cap, indent=1))
        caps.append(cap)
    if len(caps) == 2:
        report = compare(*caps)
        (out_dir / "report.md").write_text(report + "\n")
        print(report)


if __name__ == "__main__":
    main()
