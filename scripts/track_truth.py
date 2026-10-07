#!/usr/bin/env python3
"""Live tracks against the simulation's truth, sampled once a second (evaluation only).

Reads `GET /api/sim/truth` (the bridge started with MURMUR_EVAL_TRUTH=1: truth, the tracks and
the private association in one snapshot) and, per track domain (or one contact), reports:
the share of samples whose track gives a velocity (what a keep-out reads: without one, an
aircraft counts anywhere within 100 m/s of where it was), and, on samples where the track was
updated from that truth message, the position error against its reported 1-sigma (NEES, its
mean expected 2; the share within two sigmas, expected 86.5%) and the velocity error. Model
results from the simulation, never field data.

    .venv/bin/python scripts/track_truth.py --port 8865 --settle 60 --seconds 600 --domain air
"""

from __future__ import annotations

import argparse
import json
import math
import sys
import time
import urllib.request
from collections import defaultdict

M_PER_DEG = 111_320.0


def get(url: str) -> dict:
    with urllib.request.urlopen(url, timeout=5) as r:
        return json.load(r)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--port", type=int, default=8765, help="the bridge's HTTP port")
    ap.add_argument("--settle", type=float, default=60.0, help="seconds to wait before sampling")
    ap.add_argument("--seconds", type=float, default=600.0, help="how long to sample")
    ap.add_argument("--domain", default="air", help="only contacts of this domain (default air; 'all' for every one)")
    ap.add_argument("--contact", help="only this contact id")
    ap.add_argument("--out", help="write every sample here (JSON lines)")
    a = ap.parse_args()
    url = f"http://127.0.0.1:{a.port}/api/sim/truth"
    time.sleep(a.settle)
    per: dict[str, dict] = defaultdict(lambda: {"samples": 0, "velocity": 0, "scored": 0, "nees": 0.0, "inside": 0, "verr": [], "sigma": [], "tracks": set()})
    out = open(a.out, "w") if a.out else None
    end = time.monotonic() + a.seconds
    nxt = time.monotonic()
    last_seq = None
    while time.monotonic() < end:
        nxt += 1.0
        try:
            snap = get(url)
        except Exception as e:  # a missed sample is skipped, not fatal
            print(f"sample failed: {e}", file=sys.stderr)
            time.sleep(max(0.0, nxt - time.monotonic()))
            continue
        truth = (snap.get("truth") or {})
        msg = truth.get("message") or {}
        received = truth.get("received_t")
        # one sample per truth message (the contacts process posts about once a second: a poll
        # that finds the same message again is skipped)
        if msg.get("seq") == last_seq:
            time.sleep(max(0.0, nxt - time.monotonic()))
            continue
        last_seq = msg.get("seq")
        contacts = {c["id"]: c for c in msg.get("contacts", [])}
        tracks = {t["id"]: t for t in snap.get("tracks", [])}
        for pair in snap.get("association", []):
            c = contacts.get(pair["contact_id"])
            t = tracks.get(pair["track_id"])
            if not c or not t:
                continue
            if a.contact and c["id"] != a.contact:
                continue
            if a.domain != "all" and c.get("domain") != a.domain:
                continue
            p = per[c["id"]]
            p["samples"] += 1
            p["tracks"].add(t["id"])
            v = t.get("velocity_mps")
            p["velocity"] += int(v is not None)
            row = {"t": snap.get("t"), "seq": last_seq, "contact": c["id"], "track": t["id"], "velocity": v is not None, "sigma": t.get("pos_sigma_m")}
            # scored only when the track was updated from this message (same instant)
            if received is not None and abs(float(t.get("last_seen_t", 0.0)) - float(received)) < 0.05:
                lon, lat = t["position"]
                de = (lon - c["lon"]) * M_PER_DEG * math.cos(math.radians(c["lat"]))
                dn = (lat - c["lat"]) * M_PER_DEG
                d = math.hypot(de, dn)
                s = max(float(t["pos_sigma_m"]), 1e-6)
                p["scored"] += 1
                p["nees"] += (d / s) ** 2
                p["inside"] += int(d <= 2.0 * s)
                p["sigma"].append(s)
                row.update({"err_m": round(d, 1)})
                if v is not None:
                    sp, co = float(c.get("speed_mps", 0.0)), math.radians(float(c.get("course_deg", 0.0)))
                    p["verr"].append(math.hypot(v[0] - sp * math.sin(co), v[1] - sp * math.cos(co)))
            if out:
                out.write(json.dumps(row) + "\n")
        time.sleep(max(0.0, nxt - time.monotonic()))
    if out:
        out.close()
    for cid, p in sorted(per.items()):
        n, k = p["samples"], p["scored"]
        verr = sorted(p["verr"])
        sig = sorted(p["sigma"])
        print(
            f"{cid}: tracks {','.join(sorted(p['tracks']))}; {n} samples, velocity in {p['velocity']} ({100.0 * p['velocity'] / max(n, 1):.0f}%); "
            f"scored {k}: NEES {p['nees'] / max(k, 1):.2f}, within 2 sigma {100.0 * p['inside'] / max(k, 1):.1f}%, "
            f"1-sigma median {sig[len(sig) // 2] if sig else float('nan'):.0f} m; "
            f"velocity error median {verr[len(verr) // 2] if verr else float('nan'):.1f} m/s"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
