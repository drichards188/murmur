# Baked terrain and water for the headless simulator

The Golden Gate world's grids (sim/worlds/golden-gate.toml), sampled once from `tidewater serve`
so `make check` and every episode run without tidewater. Each has a provenance file beside it
(tidewater's commit and `/v1/info`, the scenario, the box, the spacing, the time sampled, the
date, the command, the file's SHA-256), which `eval/headless/tests/test_labels_families.py`
checks against the file.

| file | what | read by |
| --- | --- | --- |
| `golden-gate.grid.gz` | surface height, `max(elevation, water level)`, on a 20 m grid at the scenario's start | the link emulator's terrain line of sight (`crates/murmur-headless/src/sim/terrain.rs`); the families' water test |
| `golden-gate.water.grid.gz` | water depth (water level minus elevation: above 0 water, else land) on the same points at the same time | the bridge's domain rule without tidewater (`MURMUR_LAND_WATER_GRID`, `crates/murmur-bridge/src/decision/terrain.rs`); our vehicles' depth |
| `golden-gate.currents.gz` | surface current (east, north) on a 250 m grid over tidewater's area, hourly over its window (13-16 June 2026) | drifting contacts, our boats and drifters (`crates/murmur-headless/src/sim/water.rs`) |

Grid format: gzip of one JSON header line (`lat0`, `lon0` of the first sample, `dlat`, `dlon`,
`nlat`, `nlon`, `scale` metres per unit, `nodata`), then `nlat x nlon` little-endian i16 row by
row from the south-west; -32768 where tidewater has no data (outside its scenario's bounds, which
the emulator also reads as no data). The current field's header adds `t0`, `dt` and `nt` (unix
seconds) and its body is `nt x nlat x nlon` pairs (east, north) in cm/s.

The surface grid was baked on 2026-10-09 (eval/headless/bake_terrain.py, since folded into
bake_world.py); bake_world.py rebuilt it on 2026-10-23 from the same tidewater commit
(3a531c9) byte for byte (SHA-256 9153e1a4...), so its provenance file is the original one. To
bake a world's grids (a new tidewater DEM, another place or spacing):

```
~/tidewater/.venv/bin/tidewater serve ~/tidewater/scenarios/golden-gate-2026-06.toml --port 9570 &
.venv/bin/python -m eval.headless.bake_world golden-gate --url http://127.0.0.1:9570
.venv/bin/python -m eval.headless.bake_world golden-gate --out /tmp/rebake    # elsewhere, to compare
```
