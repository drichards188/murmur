#!/usr/bin/env python3
"""Write Saturation's contacts (sim/scenarios/swarm-saturation.toml): the harbor's ordinary traffic
and 500 hostile quadcopters in five swarms, each with its own part on the timeline, and the one
quadcopter whose act the engine may declare on. The contacts format has no swarm of its own, so
each quadcopter is one contact here: the swarm's path with the member's own place in the
formation, a seeded jitter on every waypoint, its own speed, height and start, so a swarm moves
like a swarm and not a column.

    .venv/bin/python scripts/threat_swarms.py            # rewrites sim/scenarios/swarm-saturation.toml
    .venv/bin/python scripts/threat_swarms.py -o FILE    # elsewhere

Same seed, same file. Figures and where they come from:
- Speeds 18 to 25 m/s: small quadcopters. The catalog brackets them (sim/platforms.toml: the X10
  class cruises at 12 m/s, the FPV strike quadcopter at 25); DJI publishes 21 m/s for the Mavic 3
  in its fastest mode. Each member's speed is the swarm's +/- 8 % (a GUESS at how much a swarm's
  members differ).
- Heights 20 to 120 m: low, under the 120 m (400 ft) that small drones ordinarily fly to (the
  FAA's Part 107 ceiling); a hostile one need not keep to it, so the bands are a GUESS.
- Spacing 30 to 80 m between members: a GUESS (swarm flights shown in public fly a few metres to
  tens of metres apart; tens keep them separate tracks for our sensors, which a real swarm
  closing to a few metres would not).
- Kind `small_drone`: the physical sensing model's existing signatures (crates/murmur-contacts
  scenario.rs KINDS; the bridge's tracks/physical/signature.rs).

The geometry the situation relies on (sim/situations/swarm-saturation.toml's director's notes):
the keep-out of an engagement counts every air track not declared hostile by an operator
anywhere it could reach at up to 100 m/s (rules 14; crates/murmur-decide engage.rs
`aircraft_reach_m`, 4.5 m/s^2 to 100 m/s) and a lost one anywhere (rules 16). So the one
quadcopter that fires (`recon-shooter`) holds alone in the engagement zone west of the bridge, and
every swarm has moved on by 11:30 to at least 7 km from it, over our pickets and surface
groups, where our sensors keep seeing it: an engagement there is possible, never certain.
"""

import argparse
import math
import random
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "sim" / "scenarios" / "swarm-saturation.toml"
SEED = 23

# where the shooter holds and fires from, [lon, lat]: inside ez-gate-west, 1.5 km west of the
# bridge (eval/tests/test_swarm.py keeps every station of ours 1.4 km or more from it)
SHOOTER_AT = (-122.4950, 37.8120)
VICTIM_UID = 3201  # SURFACE-3's first USV (sim/fleets/swarm-saturation.toml)

# Each swarm: its path ([lon, lat], start to hold), how many, their speed (m/s), heights (m), the
# spacing of its formation (m) and its columns, the start (s on the contacts' clock), and the spread
# of its members over the holding area at the end (m between them).
SWARMS = [
    {
        "id": "recon",
        "name": "Recon swarm",
        "what": "comes in past Point Bonita, probes the Gate's picket rings (PICKET-1 over Lime Point, "
        "PICKET-9 in the strait), runs on past the jammed north approach and holds over Belvedere and Tiburon",
        "n": 59,
        "speed": 25.0,
        "alt": (60, 110),
        "spacing": 70.0,
        "cols": 10,
        "start_s": 30,
        "path": [(-122.5850, 37.8050), (-122.5400, 37.8220), (-122.4950, 37.8150), (-122.4720, 37.8400), (-122.4580, 37.8760)],
        "hold_spacing": 60.0,
    },
    {
        "id": "wave",
        "name": "Saturation swarm",
        "what": "comes in from the sea in one wave, crosses the bridge at about 6:00 and runs on into the "
        "bay to hold over the Bay Bridge's west approach",
        "n": 180,
        "speed": 25.0,
        "alt": (30, 120),
        "spacing": 40.0,
        "cols": 30,
        "start_s": 32,
        "path": [(-122.5700, 37.8070), (-122.5100, 37.8110), (-122.4780, 37.8180), (-122.4400, 37.8260), (-122.3850, 37.8180)],
        "hold_spacing": 45.0,
    },
    {
        "id": "decoy",
        "name": "Decoy swarm",
        "what": "circles over the jammed north approach (Fort Baker, Sausalito) while the wave crosses the "
        "bridge, then holds over Richardson Bay's north shore",
        "n": 80,
        "speed": 24.0,
        "alt": (40, 100),
        "spacing": 50.0,
        "cols": 10,
        "start_s": 50,
        "path": [(-122.5300, 37.8360), (-122.4870, 37.8420), (-122.4720, 37.8540), (-122.4600, 37.8470),
                 (-122.4720, 37.8400), (-122.4840, 37.8470), (-122.5050, 37.8870)],
        "hold_spacing": 50.0,
    },
    {
        "id": "attack-east",
        "name": "Attack swarm (east)",
        "what": "launched from the East Bay shore, comes in low over the USVs' lane north of Alcatraz and "
        "holds over its east end",
        "n": 90,
        "speed": 20.0,
        "alt": (20, 50),
        "spacing": 35.0,
        "cols": 15,
        "start_s": 120,
        "path": [(-122.3800, 37.9000), (-122.3950, 37.8650), (-122.4050, 37.8380)],
        "hold_spacing": 35.0,
    },
    {
        "id": "attack-west",
        "name": "Attack swarm (west)",
        "what": "comes in low from the sea to the south-west and holds off the Saildrones' ring off Ocean "
        "Beach",
        "n": 90,
        "speed": 20.0,
        "alt": (20, 50),
        "spacing": 35.0,
        "cols": 15,
        "start_s": 50,
        "path": [(-122.6200, 37.7400), (-122.5800, 37.7550), (-122.5530, 37.7620)],
        "hold_spacing": 35.0,
    },
]

JITTER_M = 25.0  # each waypoint's own scatter per member, 1-sigma (a GUESS)


def off(p, east, north):
    lon, lat = p
    return (lon + east / (111_320.0 * math.cos(math.radians(lat))), lat + north / 111_320.0)


def metres(a, b):
    lat = math.radians((a[1] + b[1]) / 2)
    return math.hypot((b[0] - a[0]) * 111_320.0 * math.cos(lat), (b[1] - a[1]) * 111_320.0)


def bearing(a, b):
    lat = math.radians((a[1] + b[1]) / 2)
    return math.atan2((b[0] - a[0]) * math.cos(lat), b[1] - a[1])  # radians clockwise from north


def formation(k, n, cols, spacing, rng):
    """(across, along) of member k in a block `cols` wide, centred, rows behind, jittered."""
    rows = math.ceil(n / cols)
    r, c = divmod(k, cols)
    across = (c - (cols - 1) / 2) * spacing + rng.gauss(0, spacing * 0.25)
    along = ((rows - 1) / 2 - r) * spacing + rng.gauss(0, spacing * 0.25)
    return across, along


def member_route(sw, k, rng):
    path = sw["path"]
    across, along = formation(k, sw["n"], sw["cols"], sw["spacing"], rng)
    out = []
    for i, p in enumerate(path):
        h = bearing(path[max(0, i - 1)], path[i]) if i else bearing(path[0], path[1])
        if i == len(path) - 1:
            # spread over the holding area: a loose grid, hold_spacing apart
            cols = math.ceil(math.sqrt(sw["n"]))
            r, c = divmod(k, cols)
            east = (c - (cols - 1) / 2) * sw["hold_spacing"] + rng.gauss(0, sw["hold_spacing"] * 0.3)
            north = ((cols - 1) / 2 - r) * sw["hold_spacing"] + rng.gauss(0, sw["hold_spacing"] * 0.3)
        else:
            east = across * math.cos(h) + along * math.sin(h) + rng.gauss(0, JITTER_M)
            north = -across * math.sin(h) + along * math.cos(h) + rng.gauss(0, JITTER_M)
        out.append(off(p, east, north))
    return out


def contact(cid, name, speed, alt, route, start_s):
    pts = ", ".join(f"[{lon:.5f}, {lat:.5f}]" for lon, lat in route)
    return (f'[[contact]]\nid = "{cid}"\nname = "{name}"\naffiliation = "hostile"\ndomain = "air"\n'
            f'kind = "small_drone"\nspeed_mps = {speed:.1f}\nalt_m = {alt}\nmotion = "transit"\n'
            f"route = [{pts}]\nstart_s = {start_s}\n")


def arrivals(sw):
    """When the swarm (at its own speed) passes each waypoint, s on the contacts' clock."""
    t, out = sw["start_s"], [sw["start_s"]]
    for a, b in zip(sw["path"], sw["path"][1:]):
        t += metres(a, b) / sw["speed"]
        out.append(round(t))
    return out


TRAFFIC = '''
# ---------------------------------------------------------------- ferries (AIS, neutral)
[[contact]]
id = "ferry-sausalito"
name = "Sausalito ferry (Ferry Building to Sausalito)"
affiliation = "neutral"
domain = "surface"
kind = "ferry"
speed_mps = 10
motion = "route"
route = [[-122.3905, 37.7960], [-122.4120, 37.8130], [-122.4400, 37.8220], [-122.4620, 37.8390], [-122.4770, 37.8560]]
reverse = true
dwell_s = 600
ais = { mmsi = "366999101", name = "MV GOLDEN BEAR", ship_type = "passenger" }

[[contact]]
id = "ferry-larkspur"
name = "Larkspur ferry (Ferry Building to Larkspur via Raccoon Strait)"
affiliation = "neutral"
domain = "surface"
kind = "ferry"
speed_mps = 12
motion = "route"
route = [[-122.3905, 37.7960], [-122.4120, 37.8180], [-122.4170, 37.8380], [-122.4500, 37.8600], [-122.4480, 37.8740], [-122.4700, 37.8880], [-122.4950, 37.9300], [-122.5070, 37.9440]]
reverse = true
dwell_s = 600
ais = { mmsi = "366999102", name = "MV MARIN CREST", ship_type = "passenger" }

# ---------------------------------------------------------------- shipping lane (AIS, neutral)
# The inbound container ship starts five minutes later than in hostile-act-engagement, so it is
# still 3 km off the shooter's holding point at 12:00.
[[contact]]
id = "cargo-inbound"
name = "Container ship inbound to Oakland"
affiliation = "neutral"
domain = "surface"
kind = "cargo_ship"
speed_mps = 7
motion = "transit"
route = [[-122.5600, 37.7900], [-122.5100, 37.8060], [-122.4790, 37.8180], [-122.4300, 37.8360], [-122.4000, 37.8220], [-122.3830, 37.8030], [-122.3550, 37.8080], [-122.3420, 37.8100]]
start_s = 300
ais = { mmsi = "366999201", name = "MSC AURELIA", ship_type = "cargo" }

[[contact]]
id = "cargo-outbound"
name = "Bulk carrier outbound from Richmond"
affiliation = "neutral"
domain = "surface"
kind = "cargo_ship"
speed_mps = 6
motion = "transit"
route = [[-122.4200, 37.8450], [-122.4600, 37.8260], [-122.4790, 37.8170], [-122.5500, 37.7950], [-122.6800, 37.7500]]
ais = { mmsi = "366999202", name = "PACIFIC HARVESTER", ship_type = "cargo" }

# ---------------------------------------------------------------- fishing and leisure
[[contact]]
id = "fishing-wharf"
name = "Trawler from Fisherman's Wharf to the grounds (AIS)"
affiliation = "neutral"
domain = "surface"
kind = "fishing_vessel"
speed_mps = 5
motion = "route"
route = [[-122.4150, 37.8120], [-122.4600, 37.8230], [-122.4790, 37.8190], [-122.5500, 37.7800]]
reverse = true
dwell_s = 900
ais = { mmsi = "366999301", name = "F/V ANNA MARIE", ship_type = "fishing" }

[[contact]]
id = "fishing-milerock"
name = "Sport fishing boat drifting off Mile Rock (no AIS)"
affiliation = "neutral"
domain = "surface"
kind = "fishing_vessel"
motion = "drift"
at = [-122.5200, 37.7850]

[[contact]]
id = "yacht-landsend"
name = "Sailing yacht tacking off Lands End (no AIS)"
affiliation = "neutral"
domain = "surface"
kind = "sailboat"
speed_mps = 3
motion = "loiter"
center = [-122.5350, 37.7900]
radius_m = 250
'''


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("-o", "--out", type=Path, default=OUT)
    args = ap.parse_args()
    rng = random.Random(SEED)
    total = sum(sw["n"] for sw in SWARMS) + 1
    lines = [
        "# Saturation's contacts (sim/situations/swarm-saturation.toml): the harbor's ordinary traffic and",
        f"# {total} hostile quadcopters in five swarms. GENERATED by scripts/threat_swarms.py (seed {SEED}): change",
        "# the script and run `.venv/bin/python scripts/threat_swarms.py` rather than editing this file. The",
        "# script's docstring says where the speeds, heights and spacing come from. Run by",
        "# crates/murmur-contacts; points are [lon, lat]; everything here is truth (the console only",
        "# ever sees what our sensors make of it).",
        "#",
        "# Times (`start_s`, an act's `at_s`) count from the contacts process's start, a few seconds",
        "# after the situation's clock (the link emulator's). The swarms, and when each passes its",
        "# waypoints at its own speed (m:ss on the contacts' clock):",
    ]
    for sw in SWARMS:
        when = ", ".join(f"{t // 60}:{t % 60:02d}" for t in arrivals(sw))
        lines.append(f"# - {sw['id']} ({sw['n']}, {sw['speed']:.0f} m/s, {sw['alt'][0]} to {sw['alt'][1]} m): {sw['what']}; {when}.")
    lines += [
        "# - recon-shooter: one of the recon swarm that stops in the engagement zone 1.5 km west of the",
        f"#   bridge, at {SHOOTER_AT[0]}, {SHOOTER_AT[1]}, 40 m up, and fires on SURFACE-3's first USV (uid {VICTIM_UID},",
        "#   on its ring 2 km east) at about 10:00, 11:00 and 12:00 (abstract acts: no weapon modelled).",
        "#",
        "# By 11:30 every swarm is 7 km or more from the shooter, over our pickets and surface groups",
        "# where our sensors keep seeing it, so an engagement on the shooter can pass its keep-out (the",
        "# script's docstring says why that distance).",
        'title = "Saturation: 500 hostile quadcopters in five swarms, and the harbor\'s traffic"',
        f"seed = {SEED}",
        "",
        "[sensing]",
        "misclassification_rate = 0.0     # deterministic identities: the situation is about the load",
        "",
        "# ---------------------------------------------------------------- the shooter and its acts",
    ]
    recon = SWARMS[0]
    shooter_route = [recon["path"][0], recon["path"][1], SHOOTER_AT]
    lines.append(contact("recon-shooter", "Recon swarm member that stops west of the bridge and fires (no transponder)",
                         recon["speed"], 40, shooter_route, recon["start_s"]))
    for i, (at_s, label) in enumerate([(600, "fires on a USV of SURFACE-3 in the strait"), (660, "fires on the same USV again"),
                                       (720, "fires on it a third time")]):
        lines.append(f'[[act]]\nid = "act-{i + 1}"\nat_s = {at_s}\nby = "recon-shooter"\nagainst_vehicle = {VICTIM_UID}\nlabel = "{label}"\n')
    for sw in SWARMS:
        lines.append(f"# ---------------------------------------------------------------- {sw['id']}: {sw['name']} ({sw['n']})")
        for k in range(sw["n"]):
            speed = sw["speed"] * (1 + rng.uniform(-0.08, 0.08))
            alt = rng.randint(*sw["alt"])
            start = sw["start_s"] + rng.randint(0, 6)
            lines.append(contact(f"{sw['id']}-{k + 1:03d}", f"{sw['name']}, member {k + 1} of {sw['n']} (no transponder)", speed, alt,
                                 member_route(sw, k, rng), start))
    text = "\n".join(lines) + TRAFFIC
    args.out.write_text(text)
    print(f"wrote {args.out} ({total} quadcopters)")


if __name__ == "__main__":
    main()
