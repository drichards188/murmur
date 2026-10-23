# Worlds

One file per map: what a situation runs in (world v3, docs/WORLD-V3-PLAN.md, H5). Read by
`eval/world.py`; a situation names its world (`world = "golden-gate"`, the default), and the
headless simulator takes from it the terrain the link emulator reads, the link profiles and the
ground station, the operator's areas, and (by the situation's `[headless]` table) the land/water
grid the bridge's domain rule reads, the water our boats and the drifting contacts move in and
the conditions each episode draws (docs/HEADLESS.md, "Worlds"). Labels take the mission's circle
from the record's world.

| world | what |
| --- | --- |
| `golden-gate.toml` | The Golden Gate as the code held it until world v3, unchanged, so every v1 and v2 situation (and the fault drill) gives the results it gave before: Harbor watch's four stand-ins, no traffic of its own, the legacy sensing model. |
| `golden-gate-v3.toml` | World v3's Golden Gate: the same map, grids, currents, ground station (Crissy Field) and conditions, with 20 stand-ins in every domain (`sim/fleets/golden-gate-v3.toml`), traffic in every domain under the physical sensing model (`sim/scenarios/golden-gate-v3-traffic.toml`), the seabed for sonar and the Golden Gate's pinned element sets. |
| `san-diego.toml` | San Diego: the bay entrance, NAS North Island, the bay to Naval Base San Diego and the approaches off Point Loma, from its own tidewater scenario (`san-diego.tidewater.toml`), 24 stand-ins (`sim/fleets/san-diego-v3.toml`), approximate areas (`sim/areas/san-diego/`), one ground station on Point Loma, traffic in every domain (`sim/scenarios/san-diego-traffic.toml`) and its pinned element sets. |
| `golden-gate-scale.toml` | The scale world (H6): Golden Gate v3 with 100 stand-ins in the same domain mix (`sim/fleets/golden-gate-scale.toml`) and Golden Gate v3's traffic plus a heavier day (`sim/scenarios/golden-gate-scale-traffic.toml`). |

Each of world v3's worlds has a watch situation, nothing scripted, its conditions drawn per
episode: `sim/situations/golden-gate-v3-watch.toml`, `san-diego-watch.toml`,
`golden-gate-scale-watch.toml` (headless only; their smoke and the checks below:
`eval/headless/tests/test_worlds_v3.py`).

## The format

| key | what |
| --- | --- |
| `name`, `title` | the file's stem; a display name |
| `fleet` | the fleet a situation takes unless it names its own (sim/fleets) |
| `areas` | the operator's areas: a directory of `*.geojson` (the bridge's `MURMUR_AREAS`; a world's own areas go in a directory of their own, never in `sim/areas` itself, which every Golden Gate stack loads: San Diego's are `sim/areas/san-diego/`) |
| `traffic` | the background contacts a situation takes unless it names its own (`contacts`; `contacts = ""` for none): a contacts scenario (sim/scenarios) whose `[sensing]` names the model and the world's grids |
| `space` | the pinned satellite element sets an episode reads (`sim/headless/space/<space>/`, eval/headless/space.py) unless the batch names others (`--space-snapshot`) |
| `tidewater`, `at` | the tidewater scenario (a name under ~/tidewater/scenarios, or the path of its file in this repository) and the scenario time a situation takes unless it names its own |
| `operating_box` | the operating area and a margin, `{lat_min, lat_max, lon_min, lon_max}` (the oracle's link map covers it) |
| `[terrain]` | the baked grids: `surface` (surface heights, the link emulator's line of sight and the sensing model's terrain), `land_water` (water depth, the domain rule and our vehicles' depth), `bathymetry` (optional: the seabed's height, the sonar's water depth), and the `box`, `spacing_m` and `sampled_at` they were baked on (without a box: the tidewater scenario's own bounds) |
| `[currents]` | the baked current field: `field`, its `box`, `spacing_m`, and `from`, `to`, `step_s` |
| `[links]` `file` | the link profiles (default sim/links.toml) |
| `[[ground_station]]` | where the operator's radios are: `name`, `lat`, `lon`, `alt_m`. The headless simulator writes the station into a copy of the links file when it differs from the file's; the link emulator has one station, so a world naming more is refused for now |
| `[points]` | named reference points, `name = [lon, lat]` |
| `[mission]` | the labels' circle: `center` (a point's name or `[lon, lat]`), `radius_m`, `grace_s` (the time after a contact first comes inside it by which it needs a look) |
| `[conditions]` | the ranges a situation's conditions are drawn from (`World.draw_conditions`): `at` (the window its start is drawn in, inside the baked fields' and the tidewater scenario's), `utc_offset_h`, `visibility_km`, `wind_mps`, `sea_state` (each `[low, high]`); mark estimates `[E]` and say where the others come from |

A situation with `[headless] conditions = "world"` draws its episode's conditions from
`random.Random("conditions:<world>:<seed>")` (the same seed meets the same day or night, fog and
sea in every situation of a world): the drawn start is the episode's `at` (the scenario clock, so
the sun), and the drawn visibility, sea state and wind go into a copy of the contacts scenario's
`[conditions]` beside the episode, which the physical sensing model reads (docs/SENSING.md); the
wind also blows on our vehicles' water. The record's `world` carries what was drawn.

## How each world was made

**Spawns, orders and traffic** are drawn on the world's own baked grids and checked there
(`eval/headless/tests/test_worlds_v3.py`): every boat and sub spawns in water deeper than its
draft (the stand-ins' rule: 0.5 m, 2.5 m for a boat faster than 10 m/s), every UGV and tracker
on land at the ground's height, every aircraft on the ground at a launch site or aloft at least
100 m above the surface; every opening order keeps a boat's or a sub's way (from its spawn; a
patrol loops back to its first waypoint, so a patrol along a channel retraces its way) in water
half a metre deeper than its draft and a UGV's on land; surface traffic stays on water, ground
traffic on land, aircraft at least 30 m above the surface. A boat or an ROV at a pier is tied up
(`moored = true` in its fleet unit, sim/swarm.py): the current leaves it there until it first
arms, as a hull with its motors stopped and its lines on would stay. The small aircraft fly
their missions at a fleet unit's `cruise_alt_m` (Pumas and the FVR-90 at 150 m, X10s at 100 m),
the MQ-9B and the RQ-4 at their catalog altitudes.

**Golden Gate v3.** The plan's list exactly, 20 stand-ins: air REAPER 1 (MQ-9B, on station),
HAWK 1 (RQ-4, a far orbit 30 km offshore), PUMA 1 and 2, SKY 1 and 2, KESTREL 1 (FVR-90) on the
ground at Crissy Field and Fort Baker, ZEP 1 (the airship) aloft over the Marin Headlands;
surface HUNTER 1 (Sea Hunter) at a mooring off the Marina, MARLIN 1 (Corsair) and VIPER 1
(Magura, armed) at the Coast Guard station's pier in Horseshoe Bay, ALBATROSS 1 (Saildrone) at
sea off Point Bonita; subsurface REMUS 1 at the surface in the channel, EEL 1 (BlueROV) at the
Horseshoe Bay pier, three buoys in the channel (51 to 112 m of water); ground FERRET 1 (recon
UGV) on Crissy Field, MULE 1 (logistics UGV) at Fort Baker; TRK 1 beside the ground station.
Traffic (34 contacts; sources in the file's header): the Sausalito, Larkspur, Angel Island and
Tiburon, and Alcatraz ferries and a bay cruise, ships in the main channel and the southern lane,
a tanker outbound, fishing out of the Wharf and Sausalito, sailing off the Cityfront and Crissy
Field, the Coast Guard, the bar pilot, offshore boats 12 to 30 km out; sightseeing aircraft and
tour helicopters at 300 to 600 m, a hobby drone off Baker Beach; cars, a truck and walkers on
Mason Street, Marine Drive, Lincoln Boulevard, Conzelman Road and at Battery Spencer. AIS by
class (ferries, ships, the Coast Guard, the trawler class A; the charter boat and a daysailer
class B; the rest none).

**San Diego.** Its tidewater scenario (`san-diego.tidewater.toml`, fetched 2026-10-23):
west -117.32, south 32.60, east -117.10, north 32.77 at 15 m, 13-16 June 2026; terrain from
NOAA NCEI's DEM mosaic (topography and bathymetry, NAVD88: -116 to 122 m); the water level
observed at CO-OPS 9410170 (San Diego, Quarantine Station), -0.57 to 2.30 m NAVD88; currents
from CO-OPS harmonic predictions at the bay's 16 current stations with data (Point Loma Light
to National City; two Pacific stations publish none), blended by inverse distance (no
operational model resolves the bay); wind from HRRR (3 km) via Open-Meteo on a 5 x 4 grid. The
grids, baked at 17:00Z on 14 June (water level 1.2 m) on the scenario's bounds every 20 m
(948 x 1032), and the current field every 250 m hourly over the window (fastest 1.41 m/s), each
with its provenance file beside it (sim/headless/terrain/san-diego.*). The wind at the bay
entrance over the window, 1.12 to 5.39 m/s, is the world's `wind_mps`. Its 24 stand-ins adapt
sim/fleets/sandiego.toml's units (four Pumas, two X10s, the FVR-90 and the airship at NAS North
Island; three Corsairs at the North Island carrier piers; the BlueROV) and add REAPER, HAWK, a
Sea Hunter alongside at Naval Base Point Loma, a Magura off the Naval Amphibious Base, a
Saildrone, REMUS, three buoys in the entrance channel and two UGVs on North Island. The ground
station is a mast on the Point Loma ridge (91 m), which sees the bay, the entrance and the
approaches within 12 km: one station is enough, so the link emulator's single station stays.
Its areas (`sim/areas/san-diego/san-diego.geojson`, every feature marked `approximate`): the
operating area over the bay and the approaches; no-go: San Diego International's runway and
the final approach to runway 27 over downtown, Naval Base Point Loma's submarine piers, Naval
Base San Diego's piers; an engagement zone in open water south-west of Point Loma; no-strike:
the Hotel del Coronado and its beach, Cabrillo National Monument, Shelter Island, the Coronado
ferry landing, the Embarcadero's piers and the USS Midway Museum, the Coronado Bridge, Ocean
Beach pier. The mission's circle: 3 km around the entrance channel off Ballast Point. Traffic
(39 contacts): the Coronado ferry, a harbor cruise, Navy ships in and out of the channel (AIS
`military`, signatures a warship's), a car carrier to National City, a tanker at anchor,
sportfishing to the kelp beds and the Coronado Islands' banks and a commercial boat to Tuna
Harbor, sailing and a powerboat, the Coast Guard and the Harbor Police, a ship in the coastal
lane and a troller 25 km offshore (beyond the horizon of our boats' masts); airliners on the
final approach to runway 27 and departing over Point Loma (`light_aircraft` contacts with an
airliner's size, cross-section and heat; one altitude each, so each flies its segment and
leaves), a Navy MH-60, a coastal sightseeing flight, a banner tow, a hobby drone; cars on Harbor
Drive, Rosecrans Street, Cabrillo Memorial Drive and Orange Avenue, a truck on Pacific Highway,
a Navy truck on North Island, walkers on the Embarcadero and Coronado Beach, visitors at
Cabrillo. Its element sets are the Golden Gate snapshot's (CelesTrak, imported 2026-10-02),
pinned for San Diego with its named areas (`san-diego`, `sd-bay-entrance` in
sim/space/areas.toml).

**The scale world.** 100 stand-ins laid out on the grids: 43 air (four MQ-9Bs and two RQ-4s on
station, four airships aloft, twelve Pumas, fifteen X10s and six FVR-90s on the ground at Crissy
Field, Fort Baker and Fort Mason), 20 surface (four Sea Hunters at moorings off the Marina, eight
Corsairs and four Maguras tied up at Horseshoe Bay and off the Fort Mason piers, four
Saildrones at sea), 25 subsurface (six REMUS in the channel, four BlueROVs at the piers, fifteen
buoys across the strait), ten UGVs and two trackers ashore; 54 opening orders. Traffic: Golden
Gate v3's 34 contacts unchanged and 32 more (second boats on the ferry runs, the Oakland and
Vallejo ferries, more ships, a crab fleet, a yacht race off the Cityfront, powerboats, a Coast
Guard cutter, more aircraft and helicopters, a news helicopter over the bridge, a second hobby
drone, more cars and walkers).

## To make a world

Write its file, fetch its tidewater scenario (`tidewater fetch <scenario>`), then bake its grids
from it (`eval/headless/bake_world.py`, which also prints the wind's recorded range for
`[conditions]`), and pin its element sets:

```
~/tidewater/.venv/bin/tidewater serve ~/tidewater/scenarios/<scenario>.toml --port 9570 &
.venv/bin/python -m eval.headless.bake_world <world> --url http://127.0.0.1:9570
.venv/bin/python -m eval.headless.space snapshot --world <world> --cache sim/space/cache --note "<source>"
```

San Diego's: `~/tidewater/.venv/bin/tidewater fetch sim/worlds/san-diego.tidewater.toml`, then
the same with `serve sim/worlds/san-diego.tidewater.toml`.
