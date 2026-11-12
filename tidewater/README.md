# Tidewater

Real-world conditions for robot simulators. Give it an area and a time window; it builds
the land/water mask, depth, tide, current and wind for that place and time from public
data, and feeds them to simulated vehicles.

Most open simulators use a hand-built world with constant or random conditions. Tidewater
answers "what is it like **here**, **now**": a boat crossing the Golden Gate at max ebb
gets pushed out to sea at 1.8 m/s; the same route at slack water doesn't.

Tidewater is part of [Murmur](../README.md): it gives Murmur's vehicles, contacts and link
emulator the real conditions of the place and time a scenario names. It is a Python package of
its own (`tidewater/` in Murmur's repository, installed editable into Murmur's `.venv` by
`make setup`) and is under the repository's license, the
[GNU Affero General Public License v3.0](../LICENSE) or later; its ArduPilot patches
(`patches/ardupilot/`) modify ArduPilot and are under ArduPilot's license, GPL-3.0.

```
                ┌──────────── data core (simulator-agnostic) ────────────┐
 NOAA DEM  ───▶ │ terrain   land/water + depth (topobathy grid)          │
 NOAA tides ──▶ │ tide      water level → depth changes, grounding       │──▶ adapters
 NOAA currents▶ │ currents  SFBOFS mesh, WCOFS, or station predictions   │    • ArduPilot param driver  (done)
 HRRR wind ───▶ │ wind      3 km model on a grid, interp. in space+time  │    • ArduPilot grounding hook (planned)
                └─────── Conditions.at(lat, lon, time, depth) ───────────┘    • Gazebo world + forcing   (planned)
```

## Quick start

From the root of Murmur's repository, after `make setup` (which installs tidewater into
`.venv` and fetches the Golden Gate scenario):

```bash
.venv/bin/tidewater fetch tidewater/scenarios/golden-gate-2026-06.toml      # ~1 min once, then offline
.venv/bin/tidewater probe tidewater/scenarios/golden-gate-2026-06.toml --at 2026-06-14T10:00Z
.venv/bin/tidewater probe tidewater/scenarios/golden-gate-2026-06.toml --at 2026-06-14T10:00Z --lat 37.8199 --lon -122.4783
.venv/bin/python -m pytest -q tidewater/tests                                # make check runs these
```

To run Murmur's swarm at the same place:

```bash
make goldengate AT=2026-06-14T10:00:00Z
```

That launches the full-spectrum fleet at the Golden Gate (its ArduPilot SITL vehicles and
Murmur's Rust stand-ins) with `tidewater serve` for the bridge, the stand-ins and the link
emulator, and `tidewater drive`, which sets each SITL vehicle's simulated wind and current
from the conditions at its own position, every 2 s, on the scenario clock.

## Model currents (SFBOFS)

By default currents are NOAA's harmonic predictions at the stations in the area, blended by
inverse distance, which can't represent channels or eddies between stations. A scenario can
instead use NOAA's San Francisco Bay Operational Forecast System (SFBOFS), a 3D hydrodynamic
model (FVCOM, 54,000 nodes, 20 layers, 150 to 260 m cells around the Gate) with hourly output:

```toml
[currents]
source = "sfbofs"     # default "coops-predictions"
margin_deg = 0.02     # optional: mesh kept beyond the area box
```

```bash
uv pip install --python .venv/bin/python -e "./tidewater[sfbofs]"      # h5py, only needed to fetch (make setup does)
.venv/bin/tidewater fetch tidewater/scenarios/golden-gate-2026-06.toml  # SFBOFS adds 10 to 20 min for 3 days
```

The fetch reads the hourly nowcast files from the AWS Open Data archive
(`s3://noaa-nos-ofs-pds/sfbofs/netcdf/YYYY/MM/DD/`, public, no key). Each file is 56 MB but is
chunked and uncompressed, so tidewater pulls only the `u`, `v`, `zeta` and wet-cell chunks over
HTTP range requests, about 10 MB per hour, and keeps the mesh elements inside the scenario area:
about 0.7 GB downloaded for the 3-day Golden Gate window and a 26 MB cache file
(`data/cache/<scenario>/sfbofs_currents.npz`). After that the scenario runs offline, and
running it needs no h5py. Current vectors are interpolated on the mesh (barycentric in the
containing triangle), between layer centres in depth and linearly in time. Points outside the
mesh, or times outside the fetched hours, fall back to the station layer, so the stations are
still fetched too. A window the archive doesn't cover stops the fetch with a clear message.
The archive in this layout runs from November 2024 to the present (March to December 2024 is
filed under a different `YYYYMM/` layout that tidewater doesn't read), and a scenario for a
future window can't use it until the hours exist.
Accuracy against meters is in [the validation report](docs/validation-golden-gate-2026-06.md).

## A region: Bodega Bay to Monterey Bay (`bay-region-2026-06`)

`scenarios/bay-region-2026-06.toml` covers about 163 by 200 km (west -123.60, south 36.55, east
-121.75, north 38.35): the whole of San Francisco Bay to Suisun Bay, the coast from Bodega Bay to
Monterey, and the ocean out past the Farallones, about 100 km offshore, over the Golden Gate
scenario's window (13 to 16 June 2026). Situations name it `tidewater = "bay-region-2026-06"`.

```bash
.venv/bin/tidewater fetch tidewater/scenarios/bay-region-2026-06.toml   # about 45 min once
```

The fetch downloads about 2.4 GB (SFBOFS's whole mesh 1.3 GB, WCOFS 1.0 GB, the DEM 52 MB, the
rest a few MB) and keeps 280 MB in `data/cache/bay-region-2026-06/`, plus 180 MB of per-hour
files in `sfbofs/` and `wcofs/` that only an interrupted fetch resumes from (they can be deleted).

It needs four things a 16 km scenario doesn't, each a table any scenario may use:

- **Nested terrain** (`[[terrain.nest]]`): the area at 90 m (1817 x 2226 cells), San Francisco Bay
  at 30 m (2550 x 3154), and the Golden Gate scenario's own 15 m grid, which is fetched with the
  same request and so answers exactly as `golden-gate-2026-06` does there. A point is answered
  from the finest grid that covers it, bilinear as before, so a query costs the same at any
  resolution; what resolution costs is memory and the fetch. The whole region at 15 m would be
  about 145 million cells (580 MB as float32); these three are 13 million (52 MB). 90 m is about
  the offshore DEM's own resolution, and inland it is ample for radio line of sight (the link
  emulator samples its profiles every 50 m): where it is coarse is the open coast, whose
  shoreline is placed to within about 45 m, and the small harbours there. (West of about
  122.6 W and north of 38.25 N the mosaic serves a much coarser source, ETOPO: see "Limits".)
  Where two grids meet the answer steps from one to the other. Large areas are fetched in tiles
  of at most 0.5 degrees (the mosaic merges at most 100 source DEMs a request), each on the whole
  grid's pixels, each placed by its own georeferencing (see "Terrain placement" below).
- **Tide zones** (`[[tide.zones]]`): one station's water level is a poor fit for the whole
  region. Observed over the window, against San Francisco's: Point Reyes 0.19 m rms (0.35 m at
  most; it leads by 36 minutes), Monterey 0.26 m (0.51 m; 48 minutes ahead), Redwood City 0.46 m
  (0.83 m), Port Chicago 0.72 m (1.54 m). So the scenario names eight stations in two zones: the
  Bay (inside the bridge and the ridges between the Bay and the ocean, a polygon) with San
  Francisco, Alameda, Redwood City, Richmond, Martinez and Port Chicago, and the rest with San
  Francisco, Point Reyes and Monterey. A point takes the inverse-distance blend of its zone's two
  nearest stations, so the South Bay's tide never leaks over the hills to Half Moon Bay. Between
  stations this is still an approximation (the tide's lag follows the channels, not straight
  lines), and `water_level_m` now differs from point to point.
- **WCOFS currents outside SFBOFS** (`[currents] outside = "wcofs"`, `wcofs.py`): SFBOFS's mesh
  covers the Bay and the ocean to an arc from Point Reyes to Pacifica, about 45 km off the Gate;
  the rest of the region is NOAA's West Coast OFS, a ROMS model with tides on a 4 km grid, read
  from the same AWS archive by range requests (hourly surface fields, 3D fields every 3 hours).
  At a depth, a WCOFS current is the hourly surface current plus the shear below it, from the 3D
  fields on fixed depth levels, linear between them in time (the surface file is the 3D file's
  top level, so at the 3-hourly records the sum is the 3D field itself); between WCOFS's 4 km
  coastline and the real one (within 6 km of a wet grid point) the nearest grid point answers,
  and past that the station predictions. Inside SFBOFS's open boundary (its edges facing open
  water that WCOFS's cells cover for 10 km out, not its shores, holes or river ends) the two are
  blended over `blend_km` (10): all WCOFS at the boundary, all SFBOFS 10 km in, so the current is
  continuous across the arc. Over the window the two differ by 0.32 m/s on average at the
  boundary (SFBOFS's boundary zone runs at 0.29 m/s against WCOFS's 0.16) and by 0.18 m/s 10 km
  in, where their mean speeds agree: the blend takes WCOFS where SFBOFS is least reliable.
- **Wind and weather nests** (`[[wind.nest]]`): HRRR through Open-Meteo on an 8 km grid over the
  area (21 x 26 points) plus the Golden Gate scenario's own 5 x 4 grid, 566 points. Requests go
  100 points at a time, paced under Open-Meteo's 600 calls a minute (each point is a call), and
  wait out an HTTP 429. Sampling (the nearest archived point) is vectorized, a KD-tree proposing
  candidates and `at`'s own metric choosing among them: 20,000 points find theirs in 13 ms,
  where a Python loop made 11 million comparisons. Murmur's map draws at most 40 weather regions
  (`web/src/weatherField.ts`), so it shows none of this archive's effects until it draws more.

What `tidewater serve` costs, median of repeated requests on this machine (each request a new
connection, as Murmur's clients make them):

| | golden-gate-2026-06 | golden-gate-weather | bay-region-2026-06 |
|---|---|---|---|
| ready / memory | 0.6 s, 110 MB | 0.8 s, 110 MB | 3.6 s, 545 MB |
| one point | 1.2 ms | 1.2 ms | 1.4 ms (in SFBOFS, WCOFS or Monterey Bay alike) |
| a 25 km radio path from Crissy Field, 500 points | 3.9 ms | 4.0 ms | 7.0 ms |
| a 25 km leg every 20 m, 1,250 points | 7.2 ms | 7.4 ms | 13.3 ms |
| 2,000 points in the Gate's box (the bridge's chunk) | 15.4 ms | 18.5 ms | 21.8 ms |
| 20,000 points (the link emulator's batch) | 146 ms | 178 ms | 196 ms in the Gate's box, 232 ms over the region |

The Golden Gate's paths leave its box after about 8 km, where points answer null and cheaply.
The memory is mostly SFBOFS's whole mesh (54,000 nodes, 20 levels, 73 hours as int16); the
extra time over Golden Gate's is mostly that mesh's triangle search, the weather each sample
carries, and its JSON.

## Serving conditions over HTTP

For clients that can't import tidewater (Murmur's Rust engine):

```bash
.venv/bin/tidewater serve tidewater/scenarios/golden-gate-2026-06.toml [--host 127.0.0.1] [--port 8770]
curl -s localhost:8770/v1/info
curl -s -XPOST localhost:8770/v1/sample -d '{"t": 1781431200, "points": [[37.828, -122.470, null], [37.830, -122.469, 20.0]]}'
```

`POST /v1/sample` takes `{"t": <unix seconds>, "points": [[lat, lon, depth_m_or_null], ...]}` and
returns `{"t": ..., "samples": [...]}` in the same order. Each sample has `lat`, `lon`,
`elevation_m` (null outside the area), `water_level_m`, `water_depth_m` (null outside the area),
`current_east`, `current_north`, `wind_east`, `wind_north`, with the same meaning as
`Conditions.at(lat, lon, t, depth_m)`; a null depth means the surface. `GET /v1/info` returns the
scenario name, bounds and valid time range. Bad input or a time outside the scenario gives
status 400 with `{"error": "..."}`. It binds to 127.0.0.1 by default and has no authentication.

The batch path is `Conditions.sample_many`, vectorized over points (time-dependent values are
computed once per request) and tested against `Conditions.at`. About 7 ms for 1,000 points and
60 ms for 10,000, including HTTP and JSON.

## Verified so far

- Slack water at the Golden Gate on 14 June 2026 comes out at 00:20 and 06:40 UTC; NOAA's
  published slack times are 00:19 and 06:38.
- An armed boat holding position (motors stopped) off Horseshoe Bay at 10:00 UTC drifted
  **1.78 m/s toward 249°**; tidewater's prediction for that spot was **1.80 m/s toward 250°**.
  That checks the plumbing (data → driver → SITL physics), not the realism of the boat model.

- `tidewater validate` compares the forcing with NOAA observations for the window
  ([report](docs/validation-golden-gate-2026-06.md)): tide timing is exact but astronomical
  predictions ran 18 cm low, so past windows now replay the **observed** water level. HRRR
  wind averages 1.1 m/s high at the San Francisco station, with peaks 1.1 m/s low and a 22°
  direction offset. No current meter operates inside the Golden Gate area, so currents are
  checked at three Bay meters instead (same report): station IDW vs SFBOFS error is there.

### Terrain placement (murmur-cbjn)

A grid's cells are square in metres, so not in degrees (1/cos(latitude) wider than tall). Asked
for such a box, NCEI's service keeps its pixels square in degrees by widening the box's shorter
side about its middle, and says so only in the image's georeferencing. Until 12 November 2026
tidewater read every image as the box it asked for, so each grid was squeezed north-south toward
its tile's middle: the Golden Gate's and San Diego's features up to 1.3 and 1.5 km at their
boxes' north and south edges (335 m at Fort Point), the bay region's up to 5.8 km in its
0.5-degree tiles. Now each request asks for the box as given (`adjustAspectRatio=false`), and a
tile is placed only if its own GeoTIFF georeferencing puts its edges within 1 % of a cell of
the grid's; `tidewater fetch` fetches again any terrain file fetched before (it lacks the
`placement` key). `tests/test_dem.py` answers as the service does.

Checked against NGS survey marks (NAD 83 positions, NAVD 88 heights; datasheets by PID at
geodesy.noaa.gov), sampling the cached grids before and after:

| mark | grid | before | after |
| --- | --- | --- | --- |
| Mount Livermore, Angel Island (HT3014, 239 m) | Golden Gate, 15 m | 59 m at the mark; summit 868 m S | 236 m; summit (238 m) 56 m SW |
| Fort Point light (HT3036; B 551 beside it 5.7 m) | Golden Gate, 15 m | 52 m (the Presidio's bluff); water 132 m off | 4.8 m; water 51 m off |
| Yerba Buena Island (HT0817, 104.2 m) | bay region, 30 m | -14 m (water); summit 4.4 km N | 101 m; summit 21 m E |
| Mount Tamalpais East Peak (HT3388, 782.4 m) | bay region, 30 m | 273 m; summit 1.6 km N | 775 m; summit (777 m) 16 m W |
| Mount Diablo (HS5120, 1,177.7 m) | bay region, 30 m | 451 m; summit 2.8 km N | 1,165 m; summit (1,169 m) 40 m W |
| Lighthouse Point, Santa Cruz (GU4071) | bay region, 90 m | -38 m (water) | 7 m; water 83 m off |
| Point Loma, LOMA EAST (AC6092, 126.3 m) | San Diego, 15 m | 48 m | 69 m (ETOPO: see "Limits") |
| Point Reyes Head (HT3484, 186.5 m) | bay region, 90 m | -59 m (water) | 81 m (ETOPO: see "Limits") |

"Summit" is the grid's highest point within 300 m of the mark (for "before", of where the old
placement put the mark); the marks' own heights are of the disk, the grid's of 15 to 90 m cells.

### ArduPilot SITL patch (`SIM_WATER_DEPTH`)

59 lines in `libraries/SITL`, default off. With it, the driver also sends local water depth:

- **Boats run aground:** a boat sent at 5 m/s toward the Horseshoe Bay beach stopped where
  tidewater's data says the water runs out (`SIM: aground (water 0.00m, draft 0.30m)`). That
  data was the squeezed terrain (murmur-cbjn, above), whose beach there lay about 170 m south
  of the real one: the check holds for the plumbing, not for where the beach is.
- **Subs drift with the current:** an idle sub drifted 1.78 m/s toward 247° vs a predicted
  1.79 m/s toward 248° (stock SITL has no current model for subs).
- **Subs have a real seabed:** in 3 m of water at full downward thrust a sub holds at
  3.02-3.03 m (the stock limit let it sink ~1 m into the floor and keep creeping).

Lesson from both: the vehicle has to be stopped by forces the simulated IMU feels. Stopping
it kinematically (zeroing velocity, clamping position) made the autopilot's EKF disagree
with GPS: an EKF failsafe for the boat, a diverging depth estimate for the sub.

## Limits (v0)

- **Currents** default to inverse-distance weighting between stations, which doesn't know where
  channels are; `[currents] source = "sfbofs"` uses NOAA's hydrodynamic model instead (see
  above). SFBOFS is hourly, so it has no sub-hour structure, and its nowcast is a model, not
  an observation (see the validation report for its error).
- **Tide** comes from one station and is applied across the area; real tides lag and
  change size across a bay. `[[tide.zones]]` blends several stations instead (the bay region),
  which is better but still an interpolation between them.
- **WCOFS** is a 4 km model: it doesn't resolve the coast's small bays and harbours (Tomales Bay,
  Elkhorn Slough), which fall to the nearest grid point or the station predictions, and like
  SFBOFS it is a nowcast, not an observation. Its 3D fields come every 3 hours.
- **Stock SITL** applies current only to the motorboat model, only while armed, and never
  enforces land; subs ignore current. The patch in `patches/ardupilot/` fixes this (Murmur's
  `make setup` applies it to a fresh ArduPilot clone with Murmur's own patches,
  `scripts/ardupilot-patches.sh`); without it, tidewater still drives wind and boat currents.
  `0002` (from Murmur) centres the SITL antenna tracker's mount on its start heading and applies
  its limits after the rates; it doesn't touch the water model.
- **Vertical datums** are assumed consistent (NAVD88 DEM, NAVD tide predictions). True for
  US coastal DEMs in the NCEI mosaic; verify for new areas. Not true where ETOPO answers (next).
- **Terrain sources.** The mosaic's default rule serves NCEI's 1/9 arc-second tiles where they
  exist and, everywhere else, ETOPO 2022 at 15 arc-seconds (about 450 m cells, mean sea level),
  ahead of NCEI's regional 1/3 arc-second DEMs (NAVD 88) that cover the same ground. That is all
  of the San Diego scenario, and the bay region west of about 122.6 W (Point Reyes, the
  Farallones, the open coast and ocean) and north of 38.25 N; the Golden Gate's box is all 1/9
  arc-second. There a grid is placed right but smooth: Point Loma's ridge at NGS's LOMA EAST
  (126.3 m) reads 69 m, Point Reyes Head (186.5 m) 81 m. The same requests with a `mosaicRule`
  of `{"where": "Name NOT LIKE 'ETOPO%' AND VerticalDatum <> 'MHW'"}` read 125.4 and 181.6 m
  there (the service ignores a client's sort, but honours `where`); not yet used, because past
  the regional DEMs (the open ocean) only ETOPO answers, so the fetch would need to fill from it.
- The station layer is predictions, not observations (harmonic forecasts). The only current
  meters nearby are inside the Bay, not at the Gate, so Gate accuracy is inferred.

See [docs/design.md](docs/design.md) for the architecture and roadmap. Work is tracked
in Murmur's `bd` (`bd ready`).

## Surface weather replay

`scenarios/golden-gate-weather.toml` enables the archived HRRR weather layer in addition to the
existing Golden Gate conditions. `tidewater fetch` caches it once. `weather.json` uses the
same normalized schema as Rust's `murmur-weather`; the raw response is retained separately.
Visibility, wind, gust, precipitation, cloud cover, temperature, humidity and pressure are
available in each sample's optional `weather` object. Wind for vehicle clients comes from
that same archive. Scenarios without `[weather] enabled = true` retain the original wind layer.

[Weather in Murmur](../docs/WEATHER.md) covers attribution, noncommercial API terms, snapshots,
missing data, interpolation and the distinction between simulation forcing and a forecast.
