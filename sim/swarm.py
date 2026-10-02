#!/usr/bin/env python3
"""Launch a mixed swarm of ArduPilot SITL vehicles, one or more of every firmware:
Plane (fixed-wing and VTOL QuadPlane), Copter, Blimp, AntennaTracker, Rover (boats), Sub.

Each vehicle is a separate SITL process. Its primary MAVLink port (SERIAL0) is a
UDP client that sends to the bridge on 127.0.0.1:<base_port + instance>, so every
vehicle has its own UDP link. That keeps per-vehicle link stats honest and lets
`tc netem` degrade one vehicle's link without touching the others.

We launch the SITL binaries directly instead of going through sim_vehicle.py
(`--count` / `--auto-sysid`) because sim_vehicle.py only launches one vehicle
type per invocation and ties each instance to a MAVProxy console. The command
line below is what sim_vehicle.py would run under the hood.
"""

import argparse
import math
import os
import signal
import subprocess
import sys
import time
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent
ARDUPILOT = Path(os.environ.get("ARDUPILOT_HOME", Path.home() / "ardupilot"))
BIN = ARDUPILOT / "build" / "sitl" / "bin"

# Start point per vehicle kind at each location: (lat, lon, alt_m, heading_deg).
# Vehicles of one kind line up abreast, perpendicular to their heading.
LOCATIONS = {
    "sandiego": {
        # NAS North Island, runway 29
        "plane": (32.6990, -117.2050, 5.0, 290.0),
        "vtol": (32.7010, -117.2085, 5.0, 290.0),
        "copter": (32.6962, -117.2000, 5.0, 0.0),
        "blimp": (32.6950, -117.2110, 5.0, 0.0),
        "tracker": (32.6975, -117.1985, 5.0, 0.0),
        # San Diego Bay, open water between North Island and downtown
        "boat": (32.7115, -117.1850, 0.0, 0.0),
        "sub": (32.7140, -117.1870, 0.0, 0.0),
    },
    "canaveral": {
        # Cape Canaveral SFS Skid Strip, runway 13
        "plane": (28.4676, -80.5669, 3.0, 130.0),
        "vtol": (28.4700, -80.5640, 3.0, 130.0),
        "copter": (28.4650, -80.5600, 3.0, 0.0),
        "blimp": (28.4720, -80.5560, 3.0, 0.0),
        "tracker": (28.4660, -80.5625, 3.0, 0.0),
        # Atlantic, east of the cape
        "boat": (28.4550, -80.4950, 0.0, 90.0),
        "sub": (28.4500, -80.4900, 0.0, 90.0),
    },
}

# kind -> (binary, SITL model, spacing between vehicles in m)
VEHICLES = {
    "plane": ("arduplane", "plane", 40.0),
    "vtol": ("arduplane", "quadplane", 40.0),
    "copter": ("arducopter", "quad", 20.0),
    "blimp": ("blimp", "blimp", 40.0),
    "tracker": ("antennatracker", "tracker", 20.0),
    "boat": ("ardurover", "motorboat", 150.0),
    "sub": ("ardusub", "vectored", 60.0),
}
DEFAULT_COUNTS = {"plane": 4, "vtol": 1, "copter": 2, "blimp": 1, "tracker": 1, "boat": 3, "sub": 1}


def offset(lat, lon, north_m, east_m):
    dlat = north_m / 111_320.0
    dlon = east_m / (111_320.0 * math.cos(math.radians(lat)))
    return lat + dlat, lon + dlon


def scenario_spawn(path):
    """Start points from a tidewater scenario file's [spawn] table."""
    spawn = tomllib.loads(Path(path).expanduser().read_text()).get("spawn", {})
    missing = sorted(set(VEHICLES) - set(spawn))
    if missing:
        sys.exit(f"scenario {path} has no [spawn] entry for: {', '.join(missing)}")
    return {k: tuple(v) for k, v in spawn.items()}


def vehicle_specs(spawn, counts):
    specs = []
    for kind, n in counts.items():
        binary, model, spacing = VEHICLES[kind]
        lat0, lon0, alt, hdg = spawn[kind]
        beam = math.radians(hdg + 90)
        for i in range(n):
            lat, lon = offset(lat0, lon0, spacing * i * math.cos(beam), spacing * i * math.sin(beam))
            specs.append((kind, binary, model, (lat, lon, alt, hdg)))
    return specs


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--location", choices=sorted(LOCATIONS), default=os.environ.get("MURMUR_LOCATION", "sandiego"))
    ap.add_argument("--scenario", default=os.environ.get("MURMUR_SCENARIO") or None,
                    help="tidewater scenario .toml; its [spawn] table overrides --location")
    for kind, n in DEFAULT_COUNTS.items():
        ap.add_argument(f"--{kind}s", type=int, default=n, help=f"number of {kind}s (default {n})")
    ap.add_argument("--bridge-host", default="127.0.0.1")
    ap.add_argument("--base-port", type=int, default=14560, help="vehicle i sends to base_port + i")
    ap.add_argument("--speedup", type=float, default=1.0)
    ap.add_argument("--count-only", action="store_true", help="print how many vehicles would launch and exit")
    ap.add_argument("--keep-eeprom", action="store_true", help="keep params/missions between runs")
    args = ap.parse_args()

    counts = {kind: getattr(args, f"{kind}s") for kind in DEFAULT_COUNTS}
    if args.count_only:
        print(sum(counts.values()))
        return
    for b in {VEHICLES[k][0] for k, n in counts.items() if n}:
        if not (BIN / b).exists():
            sys.exit(f"missing {BIN / b}; build with: make setup")

    procs = []
    for inst, (kind, binary, model, home) in enumerate(vehicle_specs(scenario_spawn(args.scenario) if args.scenario else LOCATIONS[args.location], counts)):
        sysid = inst + 1
        workdir = ROOT / "run" / f"{sysid:02d}-{kind}"
        workdir.mkdir(parents=True, exist_ok=True)
        cmd = [
            str(BIN / binary),
            "--model", model,
            "--home", ",".join(f"{v:.7f}" for v in home),
            "--instance", str(inst),
            "--sysid", str(sysid),
            "--speedup", str(args.speedup),
            "--serial0", f"udpclient:{args.bridge_host}:{args.base_port + inst}",
            *(["--defaults", str(ROOT / "params" / f"{kind}.parm")] if (ROOT / "params" / f"{kind}.parm").exists() else []),
        ]
        if not args.keep_eeprom:
            cmd.append("--wipe")
        log = open(workdir / "sitl.log", "w")
        procs.append(subprocess.Popen(cmd, cwd=workdir, stdout=log, stderr=subprocess.STDOUT, start_new_session=True))
        print(f"sysid {sysid:2d}  {kind:7s}  udp -> {args.bridge_host}:{args.base_port + inst}  log {workdir / 'sitl.log'}")

    def shutdown(*_):
        for p in procs:
            try:
                os.killpg(p.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        sys.exit(0)

    signal.signal(signal.SIGINT, shutdown)
    signal.signal(signal.SIGTERM, shutdown)
    print(f"{len(procs)} vehicles running. Ctrl-C to stop.")
    while True:
        for i, p in enumerate(procs):
            if p.poll() is not None:
                print(f"vehicle sysid {i + 1} exited with {p.returncode}; see its sitl.log")
                shutdown()
        time.sleep(1)


if __name__ == "__main__":
    main()
