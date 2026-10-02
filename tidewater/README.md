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
 NOAA currents▶ │ currents  per-station, per-depth predictions, interp.  │    • ArduPilot param driver  (done)
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

## Verified so far

- Slack water at the Golden Gate on 14 June 2026 comes out at 00:20 and 06:40 UTC; NOAA's
  published slack times are 00:19 and 06:38.
- An armed boat holding position (motors stopped) off Horseshoe Bay at 10:00 UTC drifted
  **1.78 m/s toward 249°**; tidewater's prediction for that spot was **1.80 m/s toward 250°**.
  That checks the plumbing (data → driver → SITL physics), not the realism of the boat model.

## Limits (v0)

- **Currents between stations** use inverse-distance weighting, which doesn't know where
  channels are. NOAA's SFBOFS hydrodynamic model is the planned replacement.
- **Tide** comes from one station and is applied across the area; real tides lag and
  change size across a bay.
- **Stock SITL** applies current only to the motorboat model, only while armed, and never
  enforces land. Subs ignore current. Fixing both needs the grounding hook (C++).
- **Vertical datums** are assumed consistent (NAVD88 DEM, NAVD tide predictions). True for
  US coastal DEMs in the NCEI mosaic; verify for new areas.
- Predictions, not observations: NOAA current predictions are harmonic forecasts.
  Comparing against PORTS observations is on the roadmap.

See [docs/design.md](docs/design.md) for the architecture and roadmap. Work is tracked
in `bd` (`bd ready`).
