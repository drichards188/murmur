# Weather and observation

Murmur replays archived surface weather offline. A weather raster is drawn on the tactical map
by default, beneath labels, vehicles, tracks and their uncertainty. The legend switches between
visibility, surface wind (with arrows pointing toward travel), cloud cover and precipitation,
or turns the layer off. Shading updates with the scenario clock; missing cells are hatched and
stale layers are hidden. The legend names the source and labels scenarios with scripted weather. Link
coverage and weather use separate raster views to avoid mixing their colors.

**Weather** in the console samples the selected
vehicle's position, or the map center. `GET /api/weather?lat=37.821&lon=-122.460` and the agent's
`get_weather {lat, lon}` return the same current sample: source, model, artifact checksum, age,
missing values, synthetic status and the inputs the physical sensing model uses. They expose no
future schedule or contact truth. The map uses `GET /api/weather/grid?bbox=west,south,east,north`,
a bounded 32 by 24 raster sampled at one instant. These are simulation conditions, not a forecast or an
operational weather observation.

## Try the fog situation

After `make setup`, choose **Fog watch** in `make start`, or run:

```sh
make situation NAME=fog-watch
```

Open Weather and Tracks. This nine-minute situation needs no AI account and no engagement.
PUMA 1 orbits a small boat looping at 4 m/s. Scripted fog rolls in from 2:30 to 3:00, reducing
visibility to 50 m. At 3:30 the boat accelerates to 12 m/s and heads east. The contact simulator
changes its motion continuously, without teleporting; only a subsequent sensor sighting can
tell the tracker about the turn. The last fix ages, its uncertainty grows, and after 180 seconds
unseen the track drops into the lost-contact set. Visibility recovers from 7:00 to 7:30. The
camera must search and dwell again; a new track is not automatically confirmed as the old one.
Times count from the contacts process, a few seconds after the situation clock.

The fleet has one camera aircraft and no radar or AIS receiver. The boat broadcasts no AIS.
There is no scripted link outage. Ordinary radio geometry and packet loss remain in force.
This isolates an observation problem from a communications problem. Removing the
`[[sensing.weather.timeline]]` blocks gives a comparison with the same boat and archived weather.
The paired headless regression checks that the clear run observes the changed motion while the
fog run does not, that neither loses its radio, and that the camera reacquires after clearing.
This is a demonstration of the implemented model, not a validated finding about real sensors
or human performance. Murmur remains a working draft.

## Sources, cost and replay

The open world (`make goldengate`) uses `tidewater/scenarios/golden-gate-weather.toml`.
`WEATHER=0` restores the original wind interpolation and fixed sensor conditions. Existing
situations retain their previous forcing unless they explicitly opt into weather.

The weather scenario shares the existing Golden Gate terrain, tide and SFBOFS current cache.
Its additional `weather.json` is NOAA HRRR through Open-Meteo's Historical Forecast API, sampled
at a 5 by 4 grid of requested locations. HRRR is about 3 km horizontally and the cached fields
are hourly. This cannot resolve a fog bank moving across individual bridge spans. The fog in
the demonstration is explicitly synthetic, uniformly applied across the scene.

Setup fetches once. For an already installed checkout, fetch the additional layer with:

```sh
.venv/bin/tidewater fetch tidewater/scenarios/golden-gate-weather.toml
```

No API key or paid service is required for the intended noncommercial use. The free endpoint
has usage limits; commercial API use needs a different arrangement. Data attribution remains
required. See [Open-Meteo terms](https://open-meteo.com/en/terms),
[pricing](https://open-meteo.com/en/pricing),
[historical forecasts](https://open-meteo.com/en/docs/historical-forecast-api) and
[HRRR variables](https://open-meteo.com/en/docs/gfs-api).

The raw API response is `weather-source.json`. The normalized artifact records the query,
model, fetch time, bounds and raw response SHA-256; the Rust sampler reports the normalized
artifact's SHA-256. The contacts process pins that digest in its truth messages, and readers
reject a different file at the same path. Units are encoded in the field names: metres, m/s, mm, percent, degrees C,
and hPa. Wind is a velocity toward east/north, converted from the provider's meteorological
FROM bearing. The console converts it back to a FROM direction.

Both samplers use the nearest requested grid point and linear interpolation between hourly
instantaneous samples. They do not interpolate wind bearings across north: they interpolate
components. Missing fields stay null, missing hours are not bridged, and neither time nor
space is extrapolated. Gust maxima and precipitation totals belong to the preceding hour at
each stored timestamp; between timestamps the enclosing hour's value applies. The public
sample's `hourly_interval_end_t` identifies that interval. This is retrospective replay, not
information that would have been available as a forecast at the time.

The fog demo pins a small slice in `sim/headless/weather/fog-watch.json`, with source attribution
in [its README](../sim/headless/weather/README.md). Tidewater and the headless simulator use that
same slice for wind. Recreate a slice explicitly, after fetching, with:

```sh
.venv/bin/tidewater weather-snapshot \
  tidewater/data/cache/golden-gate-2026-06/weather.json \
  sim/headless/weather/fog-watch.json \
  --start 2026-06-14T19:00:00Z --end 2026-06-14T19:10:00Z
```

Changing a committed snapshot is a deliberate change to the replay. Loading it never fetches
new data. Old evaluation worlds do not acquire weather implicitly.

## What changes in the simulation

| Input | Behavior in this release |
| --- | --- |
| Visibility | Existing EO/IR attenuation, at each observer's local weather sample. No camera detection is inserted or suppressed by the situation script. |
| Mean surface wind | Existing radar sea-clutter and sonar ambient-noise inputs; the same archive supplies wind to Tidewater vehicle clients and headless vehicles that take conditions. |
| Gust, precipitation, cloud cover, temperature, humidity, pressure | Display and agent context only. No gust dynamics, rain fade, icing or new sensor physics. |
| Terrain, tide, current | Existing terrain and NOAA tide/SFBOFS layers, independent of this change. |

Mean wind is at 10 m, not wind aloft. Visibility remains the existing model's surface visibility
parameter; there is no vertically resolved cloud/fog layer or integration through a weather
volume. Sampling at the observer approximates conditions along its line of sight. Satellite
optical collection results are **not** newly cloud-masked by this feature. Radar is not turned
off by fog. Weather does not currently modify datalink delivery.

Missing meteorology is shown as unavailable. Sensing uses its declared fixed baseline for a
missing visibility or wind input, and the API names those effective values. Tidewater returns
null wind outside coverage without discarding terrain/current answers. Its SITL driver leaves
the simulator's previous wind in place and reports that status; this is not a measured calm.

A contact scenario opts in with `[sensing.weather] archive = "path/from/repository.json"`.
Optional `[[sensing.weather.timeline]]` entries contain increasing nonnegative `at_s` and a
`values` table using the normalized field names. They interpolate linearly, begin at the first
frame and hold the last. Overrides are labelled synthetic. Mean-wind overrides are refused in
this release: change the shared archive to keep sensing and vehicle forcing consistent.
Without weather, existing `[conditions]` values and defaults remain in force. The legacy
sensing model does not read weather.

## Seeing the effect on sensors

The weather layer shows the conditions; **Sensor reach** shows what they do to our sensors now
(bd murmur-4tq). Its controls sit under the weather legend: rings for the selected vehicles (the
default), for every vehicle, or none, and the domain they show (Auto, or Air, Ground, Surface,
Subsurface for every sensor at once). For each radar, EO/IR and sonar of a vehicle shown, the map
draws, centred on the vehicle and following it:

- a solid ring at the distance it detects a reference contact now,
- a dashed ring at the distance it identifies one now,
- a faint ring at the clear-day reference, so shrinkage is visible at a glance.

Auto shows a sonar's reach under water, a ground vehicle's on the ground and every other sensor's
at the surface (the harbour's boats). A vehicle whose sensor detects at under half its clear-day
reach carries a badge in the fleet list, such as `EO 18% · fog`. The legend gives the sun's state
(day, twilight or night: the sun up, up to 6 degrees down, or lower, as a day camera's light in
the model), and the Weather panel's **Effect on sensors** gives the sampled vehicle's reaches,
their share of a clear day and why.

Agents read the same with `get_sensor_reach {uid?}`; the console and HTTP clients with
`GET /api/sensing/reach` (optional `uid`). Each vehicle's answer gives, per sensor and domain,
`detect_m`, `recognise_m`, `identify_m`, `clear_day_m` (the catalog's `physical_reach_m`),
`ratio`, `limited_by`, `factors` and `horizon_limited`, with the conditions it used (visibility,
sun elevation and light, wind, sea state, and whether the weather is scripted and its source).

How it is computed:

- **The same prediction as the catalog's reach.** `tracks::reach` asks the physical model's
  prediction (`physical::predict`) for the reference contact the catalog's `physical_reach_m`
  generator uses (`tracks/tests/reaches.rs`, which now takes it from `tracks::reach`): a contact
  of no known kind, an aircraft 300 m up, a submarine 30 m down, otherwise at the surface, the
  sensor pointed at it. The detection reach is the generator's: how far out it is detected at
  least half the time all the way. Recognition and identification are where those chances fall to
  one half. On the reference day at the platform's operating height this is the catalog's reach
  for every sensor and domain (a test holds them within 10 m).
- **The conditions where the vehicle is.** The vehicle's own position and height, and the
  conditions the running model would sense in there: the scenario's `[conditions]` with the
  observer's local weather sample (the same merge, `Engine::conditions_at`), the sun from the
  scenario clock (or the scenario's stated sun).
- **`limited_by` from the model's inputs.** The detection reach is computed again with one input
  at its clear-day value: visibility 20 km and the sun 45 degrees up for a camera; sea state 2 and
  5 m/s of wind for a radar against a surface contact; 5 m/s of wind and moderate shipping, or
  50 m of water, for a sonar. Each that gives back at least a tenth more (and 20 m) is a factor;
  the one that gives back the most is `limited_by`: `fog` (visibility under 1 km) or `haze`,
  `night`, `twilight` or `low sun` (the thermal crossover), `sea clutter`, `sea noise`, `shallow
  water`. With none of those, a reach under nine tenths of the reference that ends where the line
  of sight does is `horizon` (the vehicle lower than the catalog's operating height). A submerged
  vehicle's sensors other than sonar are `submerged`.
- **A disc.** No terrain: each ring is a disc, and the answer says so. A hill between a camera
  and a boat still blocks it in the simulation. The sonar's water is the scenario's stated depth,
  not the bathymetry.

It is information only, with the posture of predicted link coverage: never logged, never a
check, and nothing that decides reads it. **The decision engine's assessment still reads the
catalog's clear-day `physical_reach_m`** (no rules change here), so in fog or at night the reach
shown can be much shorter than the one the engine assumes when it judges an engagement's
assessment or a sensor's coverage (bd murmur-ahss). Under the legacy sensing model the
answer is `model: "legacy"` with no reaches: weather and light change no detection there. No
contact truth and no future weather are in it.

Cost: each vehicle's answer is kept and computed again after 5 s, or at once when its height
moves 10 m (or 5 %), its visibility 5 %, its wind 0.5 m/s or the sun half a degree. It is
computed off the bridge's async workers, holding no lock but its own cache's (the physical
model's only to read the weather samples). At 100 vehicles (125 sensors, in fog, rough sea and
twilight so that every comparison is computed) a call that computes every answer took 29 ms and
one from the cache 1.4 ms (release build, this machine;
`cargo test --release -p murmur-bridge --bin murmur-bridge reach_cost -- --ignored --nocapture`).

In Fog watch, select PUMA 1: before the fog its camera's surface ring is the clear-day ring
(14.5 km); from 2:30 the solid ring closes in, and at 50 m visibility it is about 140 m, about the
slant range to the surface below the aircraft, with the badge `EO 1% · fog`; it opens out again
from 7:00.

## Communications and satellite imagery

Clouds can obstruct optical satellite imagery. Radar imagery is much less affected by clouds,
but neither fact guarantees a communications link. Line of sight, terrain, antennas, network
availability and link margins still matter at the Golden Gate. Heavy precipitation can also
interrupt some satellite terminals; see [Starlink's weather guidance](https://starlink.com/bi/support/article/529bf751-3cad-f460-d653-4af162f195da).
The demo deliberately changes optical visibility without inventing a fog-driven radio outage.
