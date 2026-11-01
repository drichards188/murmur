#!/usr/bin/env python3
"""Launch a fleet: ArduPilot SITL vehicles plus lightweight stand-ins.

A fleet file (sim/fleets/*.toml) lists units: a platform from sim/platforms.toml, a count
and a start point (and, for a stand-in boat or ROV at a pier, `moored = true`: the current
leaves it where it is until it first arms), and may group them into swarms (`[[swarm]]`
tables, "swarms" below: a swarm's members laid out as a block, a ring or a line, each vehicle's
manifest entry naming its swarm). Platforms ArduPilot can fly run as real SITL processes; the rest
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
import re
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


# ---- swarms -------------------------------------------------------------------------------
# A fleet file may group its vehicles into swarms (docs/DECISIONS.md, "Swarm units"): a
# `[[swarm]]` table is one swarm and, with a `count` and a layout, its members too, expanded here
# into one unit per member after the file's `[[unit]]` tables (uids follow that order: the
# units first, then each swarm's members in file order). A plain `[[unit]]` may name a declared
# swarm with `swarm = "<id>"`.
#
#   [[swarm]]
#   id = "PICKET-1"                  # unique; what the bridge, the console and orders name
#   name = "Gate north ring"         # what the console shows beside it
#   role = "picket"                  # picket | search | relay | surface | undersea | reserve | other
#   color = "#7fb8ff"                # optional
#   platform = "x10"                 # one platform for every member, with `count`, or
#   # mix = [{ platform = "saildrone", count = 60 }, { platform = "corsair", count = 40 }]
#   count = 100
#   sim = "standin"                  # as a unit's: optional, forces stand-ins
#   cruise_alt_m = 100               # as a unit's: optional
#   moored = true                    # as a unit's: optional (boats tied up until they first arm)
#   layout = "block"                 # where the members start (below)
#
# Layouts, every point an inline table `{ lat, lon }` and every altitude in metres above MSL
# (`alt_m`: one number, or `[low, high]`, a band the members spread across in order):
#   block  `center`, `columns`, `spacing_m`, `heading_deg` (default 0): rows of `columns`
#          members `spacing_m` apart, centred on `center`, rows across `heading_deg`; for a
#          launch site on the ground or a mooring field. Members face `heading_deg`.
#   ring   `center`, `radius_m`: evenly round the circle from north, clockwise, each facing
#          along it (clockwise).
#   line   `from`, `to`: evenly from one end to the other, facing along it.
# Spawns are checked as every fleet's are (boats in water, ground vehicles on land, aircraft on
# the ground at a launch site or aloft): eval/tests/test_swarm.py does it for the swarm fleets.
SWARM_ROLES = ("picket", "search", "relay", "surface", "undersea", "reserve", "other")
SWARM_LAYOUTS = ("block", "ring", "line")
SWARM_KEYS = {
    "id", "name", "role", "color", "platform", "mix", "count", "sim", "cruise_alt_m", "moored", "layout",
    "center", "columns", "spacing_m", "heading_deg", "radius_m", "from", "to", "alt_m",
}
# what a swarm passes on to each member unit
SWARM_UNIT_KEYS = ("sim", "cruise_alt_m", "moored")
_SWARM_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,31}$")
_COLOR = re.compile(r"^#[0-9a-fA-F]{6}$")


def _swarm_point(sw, key):
    p = sw.get(key)
    if not (isinstance(p, dict) and set(p) == {"lat", "lon"} and all(isinstance(p[k], (int, float)) for k in p)):
        sys.exit(f"swarm {sw.get('id')!r}: {key} is {{ lat = ..., lon = ... }}")
    return float(p["lat"]), float(p["lon"])


def _swarm_alts(sw, n):
    alt = sw.get("alt_m", 0)
    if isinstance(alt, (int, float)) and not isinstance(alt, bool):
        return [float(alt)] * n
    if isinstance(alt, list) and len(alt) == 2 and all(isinstance(a, (int, float)) for a in alt):
        lo, hi = float(alt[0]), float(alt[1])
        return [lo + (hi - lo) * (k / (n - 1) if n > 1 else 0.0) for k in range(n)]
    sys.exit(f"swarm {sw.get('id')!r}: alt_m is a number or [low, high]")


def _bearing(a, b):
    """Degrees clockwise from north, from a to b ((lat, lon) each), flat earth."""
    north = (b[0] - a[0]) * 111_320.0
    east = (b[1] - a[1]) * 111_320.0 * math.cos(math.radians((a[0] + b[0]) / 2))
    return math.degrees(math.atan2(east, north)) % 360


def swarm_spawns(sw, n):
    """[lat, lon, alt, heading] for each of a swarm's n members, from its layout."""
    layout = sw.get("layout")
    alts = _swarm_alts(sw, n)
    out = []
    if layout == "block":
        lat0, lon0 = _swarm_point(sw, "center")
        cols = int(sw.get("columns", 10))
        step = float(sw.get("spacing_m", 10.0))
        hdg = float(sw.get("heading_deg", 0.0))
        if cols < 1 or step <= 0:
            sys.exit(f"swarm {sw['id']!r}: a block needs columns >= 1 and spacing_m > 0")
        rows = math.ceil(n / cols)
        h = math.radians(hdg)
        for k in range(n):
            r, c = divmod(k, cols)
            across = (c - (cols - 1) / 2) * step  # along the row, to the right of the heading
            ahead = ((rows - 1) / 2 - r) * step  # rows from the front back
            north = ahead * math.cos(h) - across * math.sin(h)
            east = ahead * math.sin(h) + across * math.cos(h)
            lat, lon = offset(lat0, lon0, north, east)
            out.append([lat, lon, alts[k], hdg])
    elif layout == "ring":
        lat0, lon0 = _swarm_point(sw, "center")
        radius = float(sw.get("radius_m", 0))
        if radius <= 0:
            sys.exit(f"swarm {sw['id']!r}: a ring needs radius_m > 0")
        for k in range(n):
            a = 2 * math.pi * k / n
            lat, lon = offset(lat0, lon0, radius * math.cos(a), radius * math.sin(a))
            out.append([lat, lon, alts[k], (math.degrees(a) + 90) % 360])
    elif layout == "line":
        a, b = _swarm_point(sw, "from"), _swarm_point(sw, "to")
        hdg = _bearing(a, b)
        for k in range(n):
            f = k / (n - 1) if n > 1 else 0.0
            out.append([a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, alts[k], hdg])
    else:
        sys.exit(f"swarm {sw.get('id')!r}: layout is one of {', '.join(SWARM_LAYOUTS)}")
    return [[round(lat, 7), round(lon, 7), round(alt, 1), round(hdg, 1)] for lat, lon, alt, hdg in out]


def fleet_swarms(fleet):
    """The fleet's swarms as the manifest lists them: id, name, role, color (None when not
    given), in file order; checked (unique ids, known roles, a color as #rrggbb)."""
    out = []
    for sw in fleet.get("swarm", []):
        unknown = set(sw) - SWARM_KEYS
        if unknown:
            sys.exit(f"swarm {sw.get('id')!r}: unknown keys {sorted(unknown)}")
        sid = sw.get("id")
        if not isinstance(sid, str) or not _SWARM_ID.match(sid):
            sys.exit(f"swarm id {sid!r}: letters, digits, '-', '_' or '.', up to 32")
        if any(s["id"] == sid for s in out):
            sys.exit(f"swarm {sid!r} is declared twice")
        role = sw.get("role", "other")
        if role not in SWARM_ROLES:
            sys.exit(f"swarm {sid!r}: role {role!r} is one of {', '.join(SWARM_ROLES)}")
        color = sw.get("color")
        if color is not None and not (isinstance(color, str) and _COLOR.match(color)):
            sys.exit(f"swarm {sid!r}: color is \"#rrggbb\"")
        out.append({"id": sid, "name": str(sw.get("name") or sid), "role": role, "color": color})
    return out


def swarm_units(fleet):
    """The fleet's units: its `[[unit]]` tables, then one unit per member of each swarm that
    has a count (its platform, or its mix in order, laid out by its layout)."""
    declared = {s["id"] for s in fleet_swarms(fleet)}
    units = list(fleet.get("unit", []))
    for u in units:
        if "swarm" in u and u["swarm"] not in declared:
            sys.exit(f"a unit names swarm {u['swarm']!r}, which no [[swarm]] table declares")
    for sw in fleet.get("swarm", []):
        if "mix" in sw:
            if "platform" in sw:
                sys.exit(f"swarm {sw['id']!r}: platform or mix, not both")
            mix = sw["mix"]
            if not (isinstance(mix, list) and mix and all(isinstance(m, dict) and set(m) == {"platform", "count"} for m in mix)):
                sys.exit(f"swarm {sw['id']!r}: mix is [{{ platform = ..., count = ... }}, ...]")
            members = [m["platform"] for m in mix for _ in range(int(m["count"]))]
            if "count" in sw and int(sw["count"]) != len(members):
                sys.exit(f"swarm {sw['id']!r}: count {sw['count']} is not its mix's {len(members)}")
        elif "platform" in sw:
            members = [sw["platform"]] * int(sw.get("count", 1))
        else:
            continue  # a declaration only: its members are [[unit]] tables naming it
        if not members:
            continue
        for pid, spawn in zip(members, swarm_spawns(sw, len(members))):
            unit = {"platform": pid, "spawn": spawn, "swarm": sw["id"]}
            unit.update({k: sw[k] for k in SWARM_UNIT_KEYS if k in sw})
            units.append(unit)
    return units


def plan(fleet, platforms):
    """Expand units (and swarms, `swarm_units`) into vehicles with callsign and start pose."""
    vehicles = []
    numbering = {}
    for unit in swarm_units(fleet):
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
                "cruise_alt_m": unit.get("cruise_alt_m", spec.get("alt_m")),
                "min_turn_radius_m": (round(spec["cruise_mps"] / math.radians(spec["turn_dps"]))
                                      if spec.get("turn_dps") and spec.get("cruise_mps") else None),
            })
            if unit.get("moored"):
                # a stand-in tied up at its pier until it first arms (crates/murmur-standins
                # config.rs `moored`): only when the unit says so, so every other entry is as before
                vehicles[-1]["moored"] = True
            if unit.get("swarm"):
                # the swarm it belongs to (fleet_swarms): only when it has one, so a fleet without
                # swarms writes the manifest it always did
                vehicles[-1]["swarm"] = unit["swarm"]
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


def standin_params(pid, spec, links=None):
    """A stand-in's ArduPilot parameters over its firmware's defaults: the catalog's
    `standin_params` for its platform (a Puma's RTL_AUTOLAND and hand launch), then what a SITL
    vehicle's defaults file would add for it (the lost-link timer, `lost_link_params`)."""
    firmware = STANDIN_FIRMWARE.get(spec["behavior"])
    return {**(spec.get("standin_params") or {}), **lost_link_params(pid, spec, firmware, links)}


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


def manifest_doc(vehicles, swarms=()):
    """The manifest: the vehicles, and the fleet's swarms (`fleet_swarms`) when it has any (a
    fleet without swarms writes exactly what it always did)."""
    doc = {"vehicles": vehicles}
    if swarms:
        doc["swarms"] = list(swarms)
    return doc


def write_manifest(vehicles, replace, swarms=()):
    """sim/run/fleet.json, replaced in one step (the bridge re-reads it whenever its mtime
    changes and must never see half of it). `replace`: write it even when it already says
    the same, so its mtime says it is this run's (`--manifest-only`, which up.sh and the
    e2e harnesses run before the bridge; the bridge logs the manifest's age and flags one
    older than a minute as a previous run's); the full launch after it leaves an identical
    file alone, so the bridge doesn't re-read it."""
    manifest = RUN / "fleet.json"
    text = json.dumps(manifest_doc(vehicles, swarms), indent=1)
    if not replace and manifest.exists() and manifest.read_text() == text:
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
    fleet = load_fleet(args.location or args.fleet)
    vehicles = plan(fleet, platforms)
    swarms = fleet_swarms(fleet)
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
    write_manifest(vehicles, replace=args.manifest_only, swarms=swarms)
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
        # params: the catalog's stand-in parameters, then what a SITL vehicle's defaults file
        # would add for it (the lost-link timer)
        cfg.write_text(json.dumps({"bridge_host": args.bridge_host, "vehicles": [
            {**v, "spec": platforms[v["platform"]],
             "params": standin_params(v["platform"], platforms[v["platform"]])}
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
