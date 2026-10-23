"""What the physical sensing model costs headless (WORLD-V3-PLAN 2.8 item 4; evaluation only).

A synthetic world at the Golden Gate, 30 stand-ins of ten platform types and 40 contacts of every
domain, run on the real `murmur-headless` binary under each sensing model for the same simulated
time with nothing else happening, and timed: the factor is simulated seconds per wall second. The
plan's bar: at least 20 times real time under the physical model.

    .venv/bin/python -m eval.headless.sense_perf [--seconds 600] [--models legacy physical]

Its files go to a temporary directory, removed afterwards.
"""

from __future__ import annotations

import argparse
import json
import random
import tempfile
import time
from pathlib import Path

from .sim import Sim
from .situations import Calibration, from_situation, process_spec

# (platform, how many, altitude m): stand-ins of ten types, 30 in all
FLEET = [("mq9b", 2, 3000), ("rq4", 1, 15000), ("puma", 6, 120), ("x10", 3, 60), ("fvr90", 2, 300),
         ("seahunter", 4, 0), ("corsair", 3, 0), ("saildrone", 3, 0), ("buoy", 4, 0), ("remus", 2, -30)]
# (kind, domain, how many, speed m/s, alt m, ais): 40 contacts
CONTACTS = [("ferry", "surface", 4, 10, 0, True), ("cargo_ship", "surface", 4, 7, 0, True),
            ("fishing_vessel", "surface", 6, 4, 0, True), ("sailboat", "surface", 6, 3, 0, False),
            ("small_fast_boat", "surface", 8, 14, 0, False), ("light_aircraft", "air", 3, 50, 600, False),
            ("small_drone", "air", 3, 15, 80, False), ("submarine", "subsurface", 2, 3, -40, False),
            ("diver", "subsurface", 2, 1, -5, False), ("periscope", "subsurface", 2, 2, -15, False)]
# open water west of the bridge, out to the approaches (lat, lon)
WATER = ((37.775, 37.815), (-122.62, -122.53))


def toml_value(v: object) -> str:
    return json.dumps(v)


def write_world(d: Path, model: str, rng: random.Random) -> tuple[Path, Path]:
    fleet = [f"title = {toml_value('sense perf: 30 stand-ins')}"]
    for platform, n, alt in FLEET:
        for _ in range(n):
            lat, lon = rng.uniform(*WATER[0]), rng.uniform(*WATER[1])
            fleet += ["", "[[unit]]", f"platform = {toml_value(platform)}", 'sim = "standin"',
                      f"spawn = [{lat:.5f}, {lon:.5f}, {alt}, {rng.randrange(360)}]"]
    contacts = ["title = \"sense perf: 40 contacts\"", "seed = 5", "", "[sensing]", f"model = {toml_value(model)}",
                'terrain = "sim/headless/terrain/golden-gate.grid.gz"', "", "[conditions]", "sun_elevation_deg = 40"]
    n = 0
    for kind, domain, count, speed, alt, ais in CONTACTS:
        for _ in range(count):
            n += 1
            lat, lon = rng.uniform(*WATER[0]), rng.uniform(*WATER[1])
            contacts += ["", "[[contact]]", f'id = "c{n}"', 'affiliation = "unknown"', f"domain = {toml_value(domain)}",
                         f"kind = {toml_value(kind)}", f"speed_mps = {speed}", f"alt_m = {alt}", 'motion = "loiter"',
                         f"center = [{lon:.5f}, {lat:.5f}]", f"radius_m = {rng.randint(300, 1500)}"]
            if ais:
                contacts.append(f'ais = {{ mmsi = "36690{n:04d}", name = "MV C{n}", ship_type = "cargo" }}')
    fp, cp = d / "fleet.toml", d / "contacts.toml"
    fp.write_text("\n".join(fleet) + "\n")
    cp.write_text("\n".join(contacts) + "\n")
    return fp, cp


def measure(model: str, seconds: float, seed: int = 1) -> dict[str, float]:
    with tempfile.TemporaryDirectory(prefix="sense-perf-") as td:
        d = Path(td)
        fleet, contacts = write_world(d, model, random.Random(seed))
        e = from_situation("harbor-watch")
        e.fleet, e.contacts, e.orders, e.script = fleet, contacts, [], []
        spec, env = process_spec(e, seed, d / "work", Calibration.load(), anchors=False)
        sim = Sim(spec, env, log=d / "headless.log")
        try:
            t0 = time.perf_counter()
            for t in range(10, int(seconds) + 1, 10):
                sim.step(float(t))
                sim.now()
            wall = time.perf_counter() - t0
            tracks = sim.picture()["tracks"]
        finally:
            sim.end()
        log = (d / "headless.log").read_text(errors="replace")
        said = next((ln for ln in log.splitlines() if ln.startswith("sensing: the ")), "sensing: the legacy model (default)")
        return {"simulated_s": seconds, "wall_s": round(wall, 2), "factor": round(seconds / wall, 1), "tracks": len(tracks),
                "bridge": said}


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--seconds", type=float, default=600.0)
    ap.add_argument("--models", nargs="+", default=["legacy", "physical"], choices=["legacy", "physical"])
    a = ap.parse_args()
    for m in a.models:
        print(json.dumps({"model": m, **measure(m, a.seconds)}), flush=True)


if __name__ == "__main__":
    main()
