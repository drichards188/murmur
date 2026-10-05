#!/usr/bin/env python3
"""Launch a fleet: ArduPilot SITL vehicles plus lightweight stand-ins.

A fleet file (sim/fleets/*.toml) lists units: a platform from sim/platforms.toml, a count
and a start point. Platforms ArduPilot can fly run as real SITL processes; the rest
(HALE/MALE aircraft, AUVs, large ships, drifters) run as MAVLink-speaking stand-ins in
one process: the Rust engine, crates/murmur-standins (target/release/murmur-standins, built
here if cargo is around; MURMUR_STANDINS_BIN overrides the binary). It reads tidewater
conditions from MURMUR_TIDEWATER_URL, MURMUR_AT and MURMUR_RATE in the environment.

Every vehicle gets its own UDP link to the bridge on base_port + index (so per-vehicle
link stats and link emulation stay honest) and a callsign ("PUMA 1"). The launcher writes
sim/run/fleet.json, the manifest the bridge uses to show each vehicle's identity. Behind the
link emulator (scripts/up.sh, the default) the vehicles send to the emulator on base_port + i
and the bridge reads the emulator's rewritten copy (sim/run/bridge-fleet.json, bridge-side
ports); `--manifest-only` writes the manifest and exits. up.sh runs it first in both modes,
so the emulator binds the vehicle ports before any vehicle starts and the bridge, which
checks a previous run's carried-over orders against this run's fleet, reads this run's
manifest and not the last one's.
"""

import argparse
import json
import math
import os
import shutil
import signal
import subprocess
import sys
import time
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RUN = ROOT / "run"
REPO = ROOT.parent
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
# The firmware a stand-in behavior imitates (the drifting buoy reports itself as ArduRover).
STANDIN_FIRMWARE = {**{kind: v[0] for kind, v in VEHICLES.items()}, "drifter": "ardurover"}
# The parameter that times the GCS (lost-link) failsafe, per ArduPilot 4.8 firmware. Plane's
# GCS failsafe is its long failsafe (FS_LONG_TIMEOUT; 4.8 removed FS_SHORT_TIMEOUT, and
# FS_SHORT_ACTN is the RC failsafe's); AntennaTracker has none. Set from the vehicle's link
# profile, `lost_link_s` in sim/links.toml (see `lost_link_params`).
LOST_LINK_PARAM = {
    "arduplane": "FS_LONG_TIMEOUT",
    "arducopter": "FS_GCS_TIMEOUT",
    "ardurover": "FS_GCS_TIMEOUT",
    "ardusub": "FS_GCS_TIMEOUT",
    "blimp": "FS_GCS_TIMEOUT",
}


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


_links = None


def load_links():
    """The link profiles: MURMUR_LINKS (as scripts/up.sh hands the link emulator) or
    sim/links.toml. Read once."""
    global _links
    if _links is None:
        path = Path(os.environ.get("MURMUR_LINKS") or ROOT / "links.toml")
        if not path.is_absolute():
            path = REPO / path
        _links = tomllib.loads(path.read_text()) if path.exists() else {}
    return _links


def link_profile(pid, spec, links):
    """The profile the link emulator gives a platform (murmur-linkemu's Config::profile_for):
    [platform_profile], else a SATCOM platform's space_class through [space_class_profile],
    else its link class."""
    if pid in links.get("platform_profile", {}):
        return links["platform_profile"][pid]
    if spec.get("link") == "satcom" and spec.get("space_class") in links.get("space_class_profile", {}):
        return links["space_class_profile"][spec["space_class"]]
    return spec.get("link")


def profile_field(links, name, key):
    """A field of a link profile, following `extends` as the emulator does."""
    seen = set()
    while name and name not in seen:
        seen.add(name)
        profile = links.get("class", {}).get(name, {})
        if key in profile:
            return profile[key]
        name = profile.get("extends")
    return None


def lost_link_params(pid, spec, firmware, links=None):
    """The vehicle's lost-link (GCS failsafe) timer from its link profile's `lost_link_s`,
    under its firmware's parameter name: {} when the profile or the firmware has none."""
    links = load_links() if links is None else links
    seconds = profile_field(links, link_profile(pid, spec, links), "lost_link_s")
    name = LOST_LINK_PARAM.get(firmware)
    return {name: seconds} if seconds is not None and name else {}


def parse_parm(text):
    """name -> value from an ArduPilot .parm file (comments and blank lines skipped)."""
    out = {}
    for line in text.splitlines():
        fields = line.split("#", 1)[0].replace(",", " ").split()
        if len(fields) >= 2:
            out[fields[0]] = fields[1]
    return out


def sitl_command(v, spec, args, workdir):
    binary, model, _ = VEHICLES[spec["vehicle"]]
    # One merged defaults file (common, then kind, then the lost-link timer for the link
    # profile, then platform params; later layers win), not a comma list of files: ArduPilot
    # keeps only the last file's "every name resolved" flag
    # (AP_Param::read_param_defaults_file), so when the last file had no MAVn_* params the
    # MAVLink channels, created later in boot, missed common.parm's stream rates and streamed
    # their built-in defaults (ArduPlane: ~22 extra msg/s, murmur-yjq).
    layers = [ROOT / "params" / "common.parm", ROOT / "params" / f"{spec['vehicle']}.parm"]
    merged = {}
    for layer in layers:
        if layer.exists():
            merged.update(parse_parm(layer.read_text()))
    merged.update({k: str(val) for k, val in lost_link_params(v.get("platform"), spec, binary).items()})
    merged.update({k: str(val) for k, val in (spec.get("params") or {}).items()})
    defaults_file = workdir / "defaults.parm"
    defaults_file.write_text("".join(f"{k} {val}\n" for k, val in merged.items()))
    defaults = [str(defaults_file)]
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


def write_manifest(vehicles):
    """sim/run/fleet.json, replaced in one step (the bridge re-reads it whenever its mtime
    changes and must never see half of it) and left alone when it already says the same
    (so the launch after `--manifest-only` doesn't make the bridge re-read it)."""
    manifest = RUN / "fleet.json"
    text = json.dumps({"vehicles": vehicles}, indent=1)
    if manifest.exists() and manifest.read_text() == text:
        return
    tmp = manifest.with_name("fleet.json.tmp")
    tmp.write_text(text)
    os.replace(tmp, manifest)


def standins_engine():
    """The Rust stand-in engine's binary: MURMUR_STANDINS_BIN, else target/release, built (or
    rebuilt when stale) with cargo if cargo is installed; a no-op when it is fresh."""
    if os.environ.get("MURMUR_STANDINS_BIN"):
        return os.environ["MURMUR_STANDINS_BIN"]
    binary = REPO / "target" / "release" / "murmur-standins"
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    if Path(cargo).exists():
        r = subprocess.run([cargo, "build", "--release", "-q", "-p", "murmur-standins"], cwd=REPO)
        if r.returncode != 0:
            sys.exit("building murmur-standins failed")
    if not binary.exists():
        sys.exit(f"missing {binary}; build with: cargo build --release -p murmur-standins")
    return str(binary)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--fleet", default=os.environ.get("MURMUR_FLEET", "sandiego"),
                    help="fleet name in sim/fleets/ or a path to a fleet .toml")
    ap.add_argument("--location", help="deprecated alias for --fleet")
    ap.add_argument("--bridge-host", default="127.0.0.1")
    ap.add_argument("--base-port", type=int, default=int(os.environ.get("MURMUR_BASE_PORT", 14560)),
                    help="vehicle i sends to base_port + i (default MURMUR_BASE_PORT or 14560)")
    ap.add_argument("--instance-base", type=int, default=int(os.environ.get("MURMUR_SITL_INSTANCE_BASE", 0)),
                    help="first SITL instance number (its TCP ports are 5760 + 10*instance); "
                         "a second stack beside the default one needs its own range")
    ap.add_argument("--speedup", type=float, default=1.0)
    ap.add_argument("--keep-eeprom", action="store_true", help="keep params/missions between runs")
    ap.add_argument("--count-only", action="store_true", help="print the number of vehicles and exit")
    ap.add_argument("--manifest-only", action="store_true",
                    help="write sim/run/fleet.json and exit (for the link emulator, before the vehicles start)")
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
        v["instance"] = args.instance_base + inst  # SITL -I: offsets its TCP/sim ports; SERIAL1 = 5762 + 10*inst

    if args.count_only:
        print(len(vehicles))
        return
    if args.sitl_instances:
        print(",".join(str(v["instance"]) for v in sitl))
        return

    RUN.mkdir(exist_ok=True)
    write_manifest(vehicles)
    if args.manifest_only:
        return

    for b in {VEHICLES[platforms[v["platform"]]["vehicle"]][0] for v in sitl}:
        if not (BIN / b).exists():
            sys.exit(f"missing {BIN / b}; build with: make setup")
    engine = standins_engine() if standins else None

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
        # params: what a SITL vehicle's defaults file would add for it (the lost-link timer)
        cfg.write_text(json.dumps({"bridge_host": args.bridge_host, "vehicles": [
            {**v, "spec": platforms[v["platform"]],
             "params": lost_link_params(v["platform"], platforms[v["platform"]],
                                        STANDIN_FIRMWARE.get(platforms[v["platform"]]["behavior"]))}
            for v in standins]}))
        log = open(RUN / "standins.log", "w")
        procs.append(subprocess.Popen([engine, str(cfg)],
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
