#!/usr/bin/env python3
"""Launch a mixed swarm of ArduPilot SITL vehicles (fixed-wing planes + motorboats).

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
from pathlib import Path

ROOT = Path(__file__).resolve().parent
ARDUPILOT = Path(os.environ.get("ARDUPILOT_HOME", Path.home() / "ardupilot"))
BIN = ARDUPILOT / "build" / "sitl" / "bin"

# NAS North Island, runway 29. Planes line up abreast on the field.
PLANE_HOME = (32.6990, -117.2050, 5.0, 290.0)
# San Diego Bay, open water between North Island and downtown. Boats start in a line.
BOAT_HOME = (32.7115, -117.1850, 0.0, 0.0)


def offset(lat, lon, north_m, east_m):
    dlat = north_m / 111_320.0
    dlon = east_m / (111_320.0 * math.cos(math.radians(lat)))
    return lat + dlat, lon + dlon


def vehicle_specs(planes, boats):
    specs = []
    for i in range(planes):
        lat, lon = offset(PLANE_HOME[0], PLANE_HOME[1], -40.0 * i, 0.0)
        specs.append(("plane", "arduplane", "plane", (lat, lon, PLANE_HOME[2], PLANE_HOME[3])))
    for i in range(boats):
        lat, lon = offset(BOAT_HOME[0], BOAT_HOME[1], 0.0, 150.0 * i)
        specs.append(("boat", "ardurover", "motorboat", (lat, lon, BOAT_HOME[2], BOAT_HOME[3])))
    return specs


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--planes", type=int, default=6)
    ap.add_argument("--boats", type=int, default=4)
    ap.add_argument("--bridge-host", default="127.0.0.1")
    ap.add_argument("--base-port", type=int, default=14560, help="vehicle i sends to base_port + i")
    ap.add_argument("--speedup", type=float, default=1.0)
    ap.add_argument("--keep-eeprom", action="store_true", help="keep params/missions between runs")
    args = ap.parse_args()

    for b in ("arduplane", "ardurover"):
        if not (BIN / b).exists():
            sys.exit(f"missing {BIN / b}; build with: cd {ARDUPILOT} && ./waf configure --board sitl && ./waf plane rover")

    procs = []
    for inst, (kind, binary, model, home) in enumerate(vehicle_specs(args.planes, args.boats)):
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
            "--defaults", str(ROOT / "params" / f"{kind}.parm"),
        ]
        if not args.keep_eeprom:
            cmd.append("--wipe")
        log = open(workdir / "sitl.log", "w")
        procs.append(subprocess.Popen(cmd, cwd=workdir, stdout=log, stderr=subprocess.STDOUT, start_new_session=True))
        print(f"sysid {sysid:2d}  {kind:5s}  udp -> {args.bridge_host}:{args.base_port + inst}  log {workdir / 'sitl.log'}")

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
