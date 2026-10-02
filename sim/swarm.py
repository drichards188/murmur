#!/usr/bin/env python3
"""Launch a fleet: ArduPilot SITL vehicles plus lightweight stand-ins.

A fleet file (sim/fleets/*.toml) lists units: a platform from sim/platforms.toml, a count
and a start point. Platforms ArduPilot can fly run as real SITL processes; the rest
(HALE/MALE aircraft, AUVs, large ships, drifters) run as MAVLink-speaking stand-ins in
one process (sim/standins.py).

Every vehicle gets its own UDP link to the bridge on base_port + index (so per-vehicle
link stats and link emulation stay honest) and a callsign ("PUMA 1"). The launcher writes
sim/run/fleet.json, the manifest the bridge uses to show each vehicle's identity.
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
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RUN = ROOT / "run"
ARDUPILOT = Path(os.environ.get("ARDUPILOT_HOME", Path.home() / "ardupilot"))
BIN = ARDUPILOT / "build" / "sitl" / "bin"

# SITL vehicle -> (binary, SITL model, default spacing between vehicles of a unit in m)
VEHICLES = {
    "plane": ("arduplane", "plane", 40.0),
    "vtol": ("arduplane", "quadplane", 40.0),
    "copter": ("arducopter", "quad", 20.0),
    "blimp": ("blimp", "blimp", 40.0),
    "tracker": ("antennatracker", "tracker", 20.0),
    "boat": ("ardurover", "motorboat", 150.0),
    "sailboat": ("ardurover", "sailboat", 150.0),
    "ugv": ("ardurover", "rover-skid", 30.0),
    "sub": ("ardusub", "vectored", 60.0),
}
STANDIN_SPACING_M = 500.0


def offset(lat, lon, north_m, east_m):
    dlat = north_m / 111_320.0
    dlon = east_m / (111_320.0 * math.cos(math.radians(lat)))
    return lat + dlat, lon + dlon


def load_platforms():
    return tomllib.loads((ROOT / "platforms.toml").read_text())


def load_fleet(name_or_path):
    p = Path(name_or_path).expanduser()
    if not p.suffix:
        p = ROOT / "fleets" / f"{name_or_path}.toml"
    if not p.exists():
        sys.exit(f"no fleet {p}; available: {', '.join(f.stem for f in (ROOT / 'fleets').glob('*.toml'))}")
    return tomllib.loads(p.read_text())


def plan(fleet, platforms):
    """Expand units into vehicles with callsign and start pose."""
    vehicles = []
    numbering = {}
    for unit in fleet["unit"]:
        pid = unit["platform"]
        if pid not in platforms:
            sys.exit(f"unknown platform {pid!r}; see sim/platforms.toml")
        spec = platforms[pid]
        sim = unit.get("sim", spec["sim"])  # a fleet may force stand-ins (load tests)
        if sim == "sitl" and "vehicle" not in spec:
            sys.exit(f"platform {pid!r} has no SITL vehicle; it can only run as a stand-in")
        lat0, lon0, alt, hdg = unit["spawn"]
        spacing = unit.get("spacing_m", VEHICLES[spec["vehicle"]][2] if sim == "sitl" else STANDIN_SPACING_M)
        beam = math.radians(hdg + 90)  # line abreast, perpendicular to heading
        for i in range(unit.get("count", 1)):
            lat, lon = offset(lat0, lon0, spacing * i * math.cos(beam), spacing * i * math.sin(beam))
            prefix = spec["callsign"]
            numbering[prefix] = numbering.get(prefix, 0) + 1
            vehicles.append({
                "platform": pid,
                "callsign": f"{prefix} {numbering[prefix]}",
                "sim": sim,
                "home": [round(lat, 7), round(lon, 7), alt, hdg],
                **{k: spec[k] for k in ("name", "short", "role", "domain", "link", "sidc")},
                # what a C2 needs to task it sensibly (stand-ins; SITL platforms use defaults)
                # SATCOM class(es) from the catalog, for the link emulator (None off satcom)
                "space_class": spec.get("space_class"),
                "space_class_alt": spec.get("space_class_alt"),
                "armed": bool(spec.get("armed", False)),  # carries an effect (policy), not MAVLink arm state
                "cruise_alt_m": spec.get("alt_m"),
                "min_turn_radius_m": (round(spec["cruise_mps"] / math.radians(spec["turn_dps"]))
                                      if spec.get("turn_dps") and spec.get("cruise_mps") else None),
            })
    return vehicles


def sitl_command(v, spec, args, workdir):
    binary, model, _ = VEHICLES[spec["vehicle"]]
    defaults = [str(ROOT / "params" / "common.parm")]
    kind_parm = ROOT / "params" / f"{spec['vehicle']}.parm"
    if kind_parm.exists():
        defaults.append(str(kind_parm))
    if spec.get("params"):
        extra = workdir / "platform.parm"
        extra.write_text("".join(f"{k} {val}\n" for k, val in spec["params"].items()))
        defaults.append(str(extra))
    lat, lon, alt, hdg = v["home"]
    cmd = [
        str(BIN / binary),
        "--model", model,
        "--home", f"{lat:.7f},{lon:.7f},{alt},{hdg}",
        "--instance", str(v["instance"]),
        "--sysid", str(v["sysid"]),
        "--speedup", str(args.speedup),
        "--serial0", f"udpclient:{args.bridge_host}:{v['port']}",
    ]
    if defaults:
        cmd += ["--defaults", ",".join(defaults)]
    if not args.keep_eeprom:
        cmd.append("--wipe")
    return cmd


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--fleet", default=os.environ.get("MURMUR_FLEET", "sandiego"),
                    help="fleet name in sim/fleets/ or a path to a fleet .toml")
    ap.add_argument("--location", help="deprecated alias for --fleet")
    ap.add_argument("--bridge-host", default="127.0.0.1")
    ap.add_argument("--base-port", type=int, default=14560, help="vehicle i sends to base_port + i")
    ap.add_argument("--speedup", type=float, default=1.0)
    ap.add_argument("--keep-eeprom", action="store_true", help="keep params/missions between runs")
    ap.add_argument("--count-only", action="store_true", help="print the number of vehicles and exit")
    ap.add_argument("--sitl-instances", action="store_true",
                    help="print SITL instance numbers (comma list) and exit; for tidewater drive")
    args = ap.parse_args()

    platforms = load_platforms()
    vehicles = plan(load_fleet(args.location or args.fleet), platforms)
    for i, v in enumerate(vehicles):
        # uid: unique across the fleet, what the API and console key on. sysid: the MAVLink system
        # ID, one byte and unique only per link (each vehicle has its own port), so it wraps.
        # 1..250 keeps clear of the bridge's GCS IDs (255, 190).
        v["uid"] = i + 1
        v["sysid"] = i % 250 + 1
        v["port"] = args.base_port + i
    sitl = [v for v in vehicles if v["sim"] == "sitl"]
    standins = [v for v in vehicles if v["sim"] == "standin"]
    for inst, v in enumerate(sitl):
        v["instance"] = inst  # SITL -I: offsets its TCP/sim ports; SERIAL1 = 5762 + 10*inst

    if args.count_only:
        print(len(vehicles))
        return
    if args.sitl_instances:
        print(",".join(str(v["instance"]) for v in sitl))
        return

    for b in {VEHICLES[platforms[v["platform"]]["vehicle"]][0] for v in sitl}:
        if not (BIN / b).exists():
            sys.exit(f"missing {BIN / b}; build with: make setup")
    # MURMUR_STANDINS=rust runs the Rust engine (conditions via MURMUR_TIDEWATER_URL)
    engine = [sys.executable, str(ROOT / "standins.py")]
    if os.environ.get("MURMUR_STANDINS", "python") == "rust":
        engine = [str(ROOT.parent / "target" / "release" / "murmur-standins")]
        if standins and not Path(engine[0]).exists():
            sys.exit(f"missing {engine[0]}; build with: cargo build --release")

    RUN.mkdir(exist_ok=True)
    (RUN / "fleet.json").write_text(json.dumps({"vehicles": vehicles}, indent=1))

    procs = []
    for v in sitl:
        spec = platforms[v["platform"]]
        workdir = RUN / f"{v['sysid']:03d}-{v['platform']}"
        workdir.mkdir(parents=True, exist_ok=True)
        log = open(workdir / "sitl.log", "w")
        procs.append(subprocess.Popen(sitl_command(v, spec, args, workdir), cwd=workdir, stdout=log,
                                      stderr=subprocess.STDOUT, start_new_session=True))
        print(f"sysid {v['sysid']:3d}  {v['callsign']:12s} {v['name']}  (SITL, udp {v['port']})")

    if standins:
        cfg = RUN / "standins.json"
        cfg.write_text(json.dumps({"bridge_host": args.bridge_host,
                                   "vehicles": [{**v, "spec": platforms[v["platform"]]} for v in standins]}))
        log = open(RUN / "standins.log", "w")
        procs.append(subprocess.Popen([*engine, str(cfg)],
                                      stdout=log, stderr=subprocess.STDOUT, start_new_session=True))
        for v in standins:
            print(f"sysid {v['sysid']:3d}  {v['callsign']:12s} {v['name']}  (stand-in, udp {v['port']})")

    def shutdown(*_):
        for p in procs:
            try:
                os.killpg(p.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        sys.exit(0)

    signal.signal(signal.SIGINT, shutdown)
    signal.signal(signal.SIGTERM, shutdown)
    print(f"{len(vehicles)} vehicles ({len(sitl)} SITL, {len(standins)} stand-in). Ctrl-C to stop.")
    while True:
        for p in procs:
            if p.poll() is not None:
                print(f"a simulator exited with {p.returncode}; see sim/run/*/sitl.log or sim/run/standins.log")
                shutdown()
        time.sleep(1)


if __name__ == "__main__":
    main()
