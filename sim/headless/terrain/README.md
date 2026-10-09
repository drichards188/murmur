# Baked terrain for the headless simulator

`golden-gate.grid.gz` is the surface height the link emulator's terrain line of sight reads in
the headless simulator (docs/HEADLESS.md; `crates/murmur-headless/src/sim/terrain.rs`): what
`tidewater serve`'s `/v1/sample` answers the emulator, `max(elevation, water level)`, sampled
once on a 20 m grid over the Golden Gate at the scenario's start, so `make check` and every
episode run without tidewater. `golden-gate.provenance.json` says where it came from (tidewater's
commit, the scenario, the box, the spacing, the time sampled, the date, the command) and holds its
SHA-256, which `eval/headless/tests/test_labels_families.py` checks.

Format: gzip of one JSON header line (`lat0`, `lon0` of the first sample, `dlat`, `dlon`, `nlat`,
`nlon`, `scale` metres per unit, `nodata`), then `nlat x nlon` little-endian i16 heights row by
row from the south-west; -32768 where tidewater has no data (outside its scenario's bounds, which
the emulator also reads as no data).

To bake it again (a new tidewater DEM, another place or spacing):

```
~/tidewater/.venv/bin/tidewater serve ~/tidewater/scenarios/golden-gate-2026-06.toml --port 9570 &
.venv/bin/python -m eval.headless.bake_terrain --url http://127.0.0.1:9570
```
