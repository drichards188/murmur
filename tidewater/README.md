# Tidewater

Real-world conditions for robot simulators. Give it an area and a time window; it builds
the land/water mask, depth, tide, current and wind for that place and time from public
data, and feeds them to simulated vehicles.

Most open simulators use a hand-built world with constant or random conditions. Tidewater
answers "what is it like **here**, **now**": a boat crossing the Golden Gate at max ebb
gets pushed out to sea at 1.8 m/s; the same route at slack water doesn't.

```
                ┌──────────── data core (simulator-agnostic) ────────────┐
 NOAA DEM  ───▶ │ terrain   land/water + depth (topobathy grid)          │
 NOAA tides ──▶ │ tide      water level → depth changes, grounding       │──▶ adapters
 NOAA currents▶ │ currents  SFBOFS model mesh, or station predictions    │    • ArduPilot param driver  (done)
 HRRR wind ───▶ │ wind      3 km model on a grid, interp. in space+time  │    • ArduPilot grounding hook (planned)
                └─────── Conditions.at(lat, lon, time, depth) ───────────┘    • Gazebo world + forcing   (planned)
```

## Quick start

```bash
uv venv .venv --python 3.12 && uv pip install --python .venv/bin/python -e ".[dev]"
.venv/bin/tidewater fetch scenarios/golden-gate-2026-06.toml      # ~1 min, then offline
.venv/bin/tidewater probe scenarios/golden-gate-2026-06.toml --at 2026-06-14T10:00Z
.venv/bin/tidewater probe scenarios/golden-gate-2026-06.toml --at 2026-06-14T10:00Z --lat 37.8199 --lon -122.4783
```

With [Murmur](../murmur) (sibling repo) running a swarm at the same place:

```bash
cd ../murmur && make goldengate AT=2026-06-14T10:00Z
```

That launches 13 ArduPilot SITL vehicles from the scenario's `[spawn]` points and runs
`tidewater drive`, which sets each vehicle's simulated wind and current from the
conditions at its own position, every 2 s, on the scenario clock.

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
uv pip install --python .venv/bin/python -e ".[sfbofs]"      # h5py, only needed to fetch
.venv/bin/tidewater fetch scenarios/golden-gate-2026-06.toml  # SFBOFS adds 10 to 20 min for 3 days
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

## Serving conditions over HTTP

For clients that can't import tidewater (Murmur's Rust engine):

```bash
.venv/bin/tidewater serve scenarios/golden-gate-2026-06.toml [--host 127.0.0.1] [--port 8770]
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

### ArduPilot SITL patch (`SIM_WATER_DEPTH`)

59 lines in `libraries/SITL`, default off. With it, the driver also sends local water depth:

- **Boats run aground:** a boat sent at 5 m/s toward the Horseshoe Bay beach stopped where
  tidewater's data says the water runs out (`SIM: aground (water 0.00m, draft 0.30m)`).
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
  change size across a bay.
- **Stock SITL** applies current only to the motorboat model, only while armed, and never
  enforces land; subs ignore current. The patch in `patches/ardupilot/` fixes this (apply with
  `scripts/patch-ardupilot.sh`); without it, tidewater still drives wind and boat currents.
  `0002` (from Murmur) centres the SITL antenna tracker's mount on its start heading and applies
  its limits after the rates; it doesn't touch the water model.
- **Vertical datums** are assumed consistent (NAVD88 DEM, NAVD tide predictions). True for
  US coastal DEMs in the NCEI mosaic; verify for new areas.
- The station layer is predictions, not observations (harmonic forecasts). The only current
  meters nearby are inside the Bay, not at the Gate, so Gate accuracy is inferred.

See [docs/design.md](docs/design.md) for the architecture and roadmap. Work is tracked
in `bd` (`bd ready`).
