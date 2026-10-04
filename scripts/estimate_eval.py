#!/usr/bin/env python3
"""Measure the bridge's dead-reckoning estimates for silent vehicles (murmur-4y9) against the
truth the link emulator sees.

The emulator reads every position a vehicle sends before it drops the packet (a scripted
`cut`), so `/stats/<uid>` keeps the true position of a vehicle the bridge no longer hears.
Every second, for each uid: the bridge's `estimate` (from `/api/state`) is evaluated at the
state's time with the same sums as the console (`web/src/estimate.ts`) and the bridge
(`crates/murmur-bridge/src/estimate.rs`), and compared with the emulator's position: the
error in metres and whether the truth lies inside the region. The lost-link branch is
measured the same way once its timer has fired. At the end, a summary per uid by silence
bucket. Lines go to `--out` as JSON.

    scripts/estimate_eval.py --bridge http://127.0.0.1:8765 --emu http://127.0.0.1:8766 \\
        --uids 2,17 --duration 180 --out logs/estimate-eval.jsonl
"""

import argparse
import json
import math
import sys
import time
import urllib.request

DEG = math.pi / 180


def m_per_deg_lon(lat):
    return 111_320 * math.cos(lat * DEG)


def enu(origin, p):
    return (p[0] - origin[0]) * m_per_deg_lon(origin[1]), (p[1] - origin[1]) * 111_320


def dist(a, b):
    e, n = enu(a, b)
    return math.hypot(e, n)


def bearing(a, b):
    e, n = enu(a, b)
    return (math.degrees(math.atan2(e, n))) % 360


def step(p, bearing_deg, d):
    a = bearing_deg * DEG
    return [p[0] + d * math.sin(a) / m_per_deg_lon(p[1]), p[1] + d * math.cos(a) / 111_320]


def along_route(route, seg, frac):
    a = route[seg % len(route)]
    b = route[(seg + 1) % len(route)]
    br = bearing(a, b)
    return step(a, br, frac * dist(a, b)), br


def evaluate(m, dt, reach, cap):
    """(position, region) as the bridge's `eval` and the console's `evalMotion`."""
    dt = max(0.0, dt)

    def bound(x, s0):
        return min(x, cap, reach * dt + s0)

    k = m["kind"]
    if k == "ring":
        angle = (m["angle0_deg"] + m["rate_dps"] * dt) % 360
        pos = step(m["center"], angle, m["radius_m"])
        reach_deg = math.degrees(reach * dt / m["radius_m"]) if m["radius_m"] > 0 else 180
        spread = min(m["spread0_deg"] + m["spread_dps"] * dt, m["spread0_deg"] + reach_deg, 180)
        return pos, {"kind": "arc", "center": m["center"], "radius_m": m["radius_m"], "width_m": m["width_m"],
                     "angle_deg": angle, "spread_deg": spread}
    if k == "route":
        route = m["route"]
        segs = [dist(route[i], route[(i + 1) % len(route)]) for i in range(len(route))]
        loop = sum(segs)
        s = m["speed_mps"] * dt
        seg, frac = m["seg0"], m["frac0"]
        pos, br = None, 0.0
        if m.get("lead"):
            lead_m = dist(m["lead"], route[0])
            b = bearing(m["lead"], route[0])
            if s < lead_m:
                pos, br = step(m["lead"], b, s), b
            else:
                s -= lead_m
                seg, frac = 0, 0.0
        if pos is None:
            if loop <= 0:
                pos = route[0]
            else:
                rem = (s % loop) + frac * segs[seg % len(segs)]
                i = seg % len(segs)
                while True:
                    ln = segs[i]
                    if rem <= ln or ln <= 0:
                        pos, br = along_route(route, i, rem / ln if ln > 0 else 0)
                        break
                    rem -= ln
                    i = (i + 1) % len(segs)
        along = bound(m["along0_m"] + m["along_mps"] * dt, m["along0_m"])
        across = bound(m["across0_m"] + m["across_mps"] * dt, m["across0_m"])
        if not m.get("lead") and loop > 0 and along >= loop / 2:
            return pos, {"kind": "band", "route": route, "width_m": 2 * across}
        return pos, {"kind": "ellipse", "center": pos, "semi_major_m": along, "semi_minor_m": across,
                     "bearing_deg": br}
    if k == "line":
        d = m["speed_mps"] * dt
        along = bound(m["along0_m"] + m["along_mps"] * dt, m["along0_m"])
        across = bound(m["across0_m"] + m["across_mps"] * dt, m["across0_m"])
        # arrived only once even a vehicle lagging by the along-track uncertainty would be there
        arrived = False
        if m.get("stop_m") is not None and d >= m["stop_m"]:
            arrived = d >= m["stop_m"] + along
            d = m["stop_m"]
        pos = step(m["from"], m["bearing_deg"], d)
        a = m.get("arrive")
        if a and arrived and a["ring"]:
            return pos, {"kind": "arc", "center": a["center"], "radius_m": a["radius_m"],
                         "width_m": a["width_m"], "angle_deg": bearing(a["center"], pos),
                         "spread_deg": 180}
        if a and arrived:
            return pos, {"kind": "circle", "center": a["center"], "radius_m": a["radius_m"] + m["along0_m"]}
        return pos, {"kind": "ellipse", "center": pos, "semi_major_m": along, "semi_minor_m": across,
                     "bearing_deg": m["bearing_deg"]}
    if k == "point":
        return m["at"], {"kind": "circle", "center": m["at"],
                         "radius_m": bound(m["radius0_m"] + m["rate_mps"] * dt, m["radius0_m"])}
    raise ValueError(k)


def point_to_segment_m(p, a, b):
    ax, ay = 0.0, 0.0
    bx, by = enu(a, b)
    px, py = enu(a, p)
    dx, dy = bx - ax, by - ay
    len2 = dx * dx + dy * dy
    t = max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / len2)) if len2 > 0 else 0.0
    return math.hypot(px - (ax + t * dx), py - (ay + t * dy))


def contains(region, p):
    k = region["kind"]
    if k == "circle":
        return dist(region["center"], p) <= region["radius_m"]
    if k == "ellipse":
        e, n = enu(region["center"], p)
        b = region["bearing_deg"] * DEG
        along = e * math.sin(b) + n * math.cos(b)
        across = e * math.cos(b) - n * math.sin(b)
        return (along / region["semi_major_m"]) ** 2 + (across / region["semi_minor_m"]) ** 2 <= 1
    if k == "arc":
        r = dist(region["center"], p)
        if abs(r - region["radius_m"]) > region["width_m"] / 2:
            return False
        off = (bearing(region["center"], p) - region["angle_deg"] + 180) % 360 - 180
        return abs(off) <= region["spread_deg"]
    if k == "band":
        route = region["route"]
        return min(point_to_segment_m(p, route[i], route[(i + 1) % len(route)]) for i in range(len(route))) <= region["width_m"] / 2
    raise ValueError(k)


def extent(region):
    k = region["kind"]
    if k == "circle":
        return region["radius_m"]
    if k == "ellipse":
        return region["semi_major_m"]
    if k == "arc":
        return region["spread_deg"]
    return region["width_m"] / 2


def get(url):
    with urllib.request.urlopen(url, timeout=5) as r:
        return json.load(r)


BUCKETS = [(0, 15), (15, 30), (30, 60), (60, 120), (120, 300), (300, 10 ** 9)]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bridge", default="http://127.0.0.1:8765")
    ap.add_argument("--emu", default="http://127.0.0.1:8766")
    ap.add_argument("--uids", required=True, help="comma-separated uids to follow")
    ap.add_argument("--duration", type=float, default=180.0, help="seconds to sample")
    ap.add_argument("--out", default=None, help="JSON lines of every sample")
    args = ap.parse_args()
    uids = [int(u) for u in args.uids.split(",") if u]
    out = open(args.out, "w") if args.out else None
    samples = {u: [] for u in uids}
    t_end = time.time() + args.duration
    while time.time() < t_end:
        try:
            state = get(f"{args.bridge}/api/state")
        except Exception as e:  # noqa: BLE001
            print(f"bridge: {e}", file=sys.stderr)
            time.sleep(1)
            continue
        t = state["t"]
        by_uid = {v["uid"]: v for v in state["vehicles"]}
        for u in uids:
            v = by_uid.get(u)
            if not v:
                continue
            est = v.get("estimate")
            if not est:
                continue
            try:
                stats = get(f"{args.emu}/stats/{u}")
            except Exception as e:  # noqa: BLE001
                print(f"emu {u}: {e}", file=sys.stderr)
                continue
            pos = stats.get("pos") or {}
            if pos.get("lat") is None or pos.get("lon") is None:
                continue
            truth = [pos["lon"], pos["lat"]]
            silent = t - (v["link"]["last_rx_t"] or t)
            p, region = evaluate(est["motion"], t - est["t0"], est["reach_mps"], est["cap_m"])
            rec = {
                "t": round(t, 1), "uid": u, "silent_s": round(silent, 1), "basis": est["basis"],
                "error_m": round(dist(p, truth), 1), "contained": contains(region, truth),
                "region": region["kind"], "extent": round(extent(region), 1),
                "last_known_error_m": round(dist(v["position"], truth), 1) if v.get("position") else None,
                "truth": [round(truth[0], 6), round(truth[1], 6)], "est": [round(p[0], 6), round(p[1], 6)],
            }
            fs = est.get("failsafe")
            if fs and t >= fs["fires_t"]:
                bp, breg = evaluate(fs["motion"], t - fs["fires_t"], est["reach_mps"], est["cap_m"])
                rec["branch"] = {
                    "action": fs["action"], "since_s": round(t - fs["fires_t"], 1),
                    "error_m": round(dist(bp, truth), 1), "contained": contains(breg, truth),
                    "region": breg["kind"], "extent": round(extent(breg), 1),
                }
            samples[u].append(rec)
            if out:
                out.write(json.dumps(rec) + "\n")
                out.flush()
            b = rec.get("branch")
            btxt = f" | branch {b['action']} +{b['since_s']}s err {b['error_m']} m {'IN' if b['contained'] else 'OUT'}" if b else ""
            print(f"t+{silent:5.0f}s uid {u} {est['basis']:10s} err {rec['error_m']:7.1f} m "
                  f"{'IN ' if rec['contained'] else 'OUT'} {region['kind']} {rec['extent']} "
                  f"(last known off by {rec['last_known_error_m']} m){btxt}")
        time.sleep(1)
    print("\nsummary (containment = truth inside the drawn region; error = estimate to truth)")
    for u in uids:
        recs = samples[u]
        if not recs:
            print(f"uid {u}: no samples (never silent, or no estimate)")
            continue
        print(f"uid {u}: {len(recs)} samples, basis {sorted(set(r['basis'] for r in recs))}")
        for lo, hi in BUCKETS:
            rs = [r for r in recs if lo <= r["silent_s"] < hi]
            if not rs:
                continue
            inside = sum(r["contained"] for r in rs)
            errs = [r["error_m"] for r in rs]
            lk = [r["last_known_error_m"] for r in rs if r["last_known_error_m"] is not None]
            line = (f"  silent {lo:>3}-{hi if hi < 10**9 else '':>3}s: n={len(rs):3d} contained {inside / len(rs) * 100:5.1f}% "
                    f"err mean {sum(errs) / len(errs):7.1f} max {max(errs):7.1f} m"
                    f" | last known err mean {sum(lk) / len(lk) if lk else 0:7.1f} m")
            br = [r["branch"] for r in rs if r.get("branch")]
            if br:
                line += f" | branch n={len(br)} contained {sum(b['contained'] for b in br) / len(br) * 100:5.1f}% err mean {sum(b['error_m'] for b in br) / len(br):7.1f} m"
            print(line)
    if out:
        out.close()


if __name__ == "__main__":
    main()
