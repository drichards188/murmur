# Design

## Goal

Answer "what are conditions here, at this time?" for any point in a scenario area, from
real data, and feed that into simulators. The core knows nothing about any simulator;
adapters translate.

## Data core

| Layer | Source (v0) | Resolution | Notes |
|---|---|---|---|
| terrain | NOAA NCEI DEM mosaic image service | ~15 m in the Golden Gate scenario | One topobathy grid gives both land mask and depth. NAVD88 for US coastal DEMs. |
| tide | NOAA CO-OPS predictions, one station | 6 min | Datum requested to match the DEM. |
| currents | NOAA CO-OPS current predictions, all stations and depth bins in the area | 6 min (harmonic) or slack/max events (subordinate) | Subordinate events interpolated with a half-cosine. IDW between stations. Stations with no published flood/ebb axis are skipped. |
| wind | Open-Meteo historical forecast API, NOAA HRRR | 3 km, hourly | Reanalysis (~25 km) was tried and badly under-reads the Golden Gate gap wind. |

Everything is cached under `data/cache/<scenario>/` so a scenario runs offline after one
fetch. `Conditions.at(lat, lon, t, depth)` returns a `Sample` with elevation, water level,
water depth, current and wind vectors (pointing where things go, m/s, east/north).

## Adapters

### ArduPilot, level 1: param driver (done)

`tidewater drive` connects to each SITL instance's SERIAL1 TCP port and sets
`SIM_WIND_SPD/DIR` and (boats) `SIM_TIDE_SPEED/DIR` from the conditions at that vehicle.
Works on stock ArduPilot because each SITL vehicle is its own process. Findings from the
SITL source that shape this:

- `SIM_TIDE_*` is only applied in `SIM_Sailboat.cpp` (motorboat inherits it), only while
  armed. Direction is "coming from".
- `SIM_Submarine` has no current model. Subs need level 2.
- Nothing in SITL knows about land.

### ArduPilot, level 2: grounding and current (done, `patches/ardupilot/`)

One new param, `SIM_WATER_DEPTH`, that the driver updates per vehicle every 0.5 s (a 5 m/s
boat moves ~2.5 m between samples). Boats ground below their draft; the sub's existing
`calculate_sea_floor_depth()` TODO returns it; the sub's drag uses the water current. Kept
generic so it can be offered upstream. A param is the smallest possible interface; a
higher-rate alternative (a depth grid loaded by SITL, or the JSON backend) is only worth it
if 2 Hz sampling proves too coarse.

### Gazebo (planned)

World generator (terrain and seabed meshes from the DEM, coastline) plus forcing. To check
first: recent Gazebo versions may load environmental data that varies in space and time
from a file and feed it to the hydrodynamics system, which would make this mostly an
exporter.

## Validation

1. Plumbing: drift of an unpowered boat matches the forcing (done, 1.78 vs 1.80 m/s).
2. Forcing vs reality: compare predictions to NOAA PORTS observed currents and NDBC/PORTS
   wind for the same window.
3. Vehicle response: needs a real boat (Tampa Bay idea). Until then, claim realistic
   forcing, not realistic vehicle behaviour.
