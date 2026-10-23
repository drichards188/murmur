# Worlds

One file per map: what a situation runs in (world v3, docs/WORLD-V3-PLAN.md, H5). Read by
`eval/world.py`; a situation names its world (`world = "golden-gate"`, the default), and the
headless simulator takes from it the terrain the link emulator reads, the link profiles and the
ground station, the operator's areas, and (by the situation's `[headless]` table) the land/water
grid the bridge's domain rule reads and the water our boats and the drifting contacts move in
(docs/HEADLESS.md, "Worlds"). Labels take the mission's circle from the record's world.

`golden-gate.toml` holds what the code held as constants until world v3, unchanged, so every v1
and v2 situation gives the results it gave before.

| key | what |
| --- | --- |
| `name`, `title` | the file's stem; a display name |
| `fleet` | the fleet a situation takes unless it names its own (sim/fleets) |
| `areas` | the operator's areas: a directory of `*.geojson` (the bridge's `MURMUR_AREAS`; a world's own areas go in a directory of their own, never in `sim/areas`, which every Golden Gate stack loads) |
| `tidewater`, `at` | the tidewater scenario and the scenario time a situation takes unless it names its own |
| `operating_box` | the operating area and a margin, `{lat_min, lat_max, lon_min, lon_max}` (the oracle's link map covers it) |
| `[terrain]` | the baked grids: `surface` (surface heights, the link emulator's line of sight), `land_water` (water depth, the domain rule and our vehicles' depth), and the `box`, `spacing_m` and `sampled_at` they were baked on (without a box: the tidewater scenario's own bounds) |
| `[currents]` | the baked current field: `field`, its `box`, `spacing_m`, and `from`, `to`, `step_s` |
| `[links]` `file` | the link profiles (default sim/links.toml) |
| `[[ground_station]]` | where the operator's radios are: `name`, `lat`, `lon`, `alt_m`. The headless simulator writes the station into a copy of the links file when it differs from the file's; the link emulator has one station, so a world naming more is refused for now |
| `[points]` | named reference points, `name = [lon, lat]` |
| `[mission]` | the labels' circle: `center` (a point's name or `[lon, lat]`), `radius_m`, `grace_s` (the time after a contact first comes inside it by which it needs a look) |
| `[conditions]` | the ranges a situation's conditions are drawn from (`World.draw_conditions`): `at` (the window its start is drawn in, inside the baked fields' and the tidewater scenario's), `utc_offset_h`, `visibility_km`, `wind_mps`, `sea_state` (each `[low, high]`); mark estimates `[E]` and say where the others come from |

To make a world: write its file, fetch its tidewater scenario (`tidewater fetch <scenario>`),
then bake its grids from it (`eval/headless/bake_world.py`, which also prints the wind's
recorded range for `[conditions]`):

```
~/tidewater/.venv/bin/tidewater serve ~/tidewater/scenarios/<scenario>.toml --port 9570 &
.venv/bin/python -m eval.headless.bake_world <world> --url http://127.0.0.1:9570
```
