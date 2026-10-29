# Morning Harbor Watch source data

An offline, recorded-traffic baseline for **14 June 2024, 08:00-09:00 PDT**
(15:00-16:00 UTC), with eight simulated, unarmed observers. Start it from the menu or:

```sh
make situation NAME=morning-harbor-watch
```

It defaults to a live operator and the playbook AI, requiring no model account. The console
is on port 5673 when started directly. `OPERATOR=autoplay` uses the existing scripted operator;
`AI=none` disables the watch officer. After an hour the stack stays up for inspection; use
End in the menu or `make down` to stop it. Murmur is a working draft; this scenario is not a
validated evaluation result or a reconstruction of real surveillance coverage.

## What is recorded

`ais.csv` contains 2,126 NOAA Marine Cadastre reports from 109 MMSIs in the rectangle
west -122.56, south 37.77, east -122.38, north 37.88. The time window includes 15:00 and excludes
16:00 UTC. The source is already downsampled, approximately to one-minute reports. The 109
identities are the union over the hour, not a simultaneous vessel count. AIS is self-reported,
can be incomplete or erroneous, and does not cover every craft on the water.

The clip includes outbound tanker BOW KISO, pilot boat PV GOLDEN GATE, passenger vessels
including SONOMA and SAN FRANCISCO, and many stationary pleasure/sailing vessels. CARNIVAL
MIRACLE reports stationary. High speed, missing reports or proximity to infrastructure do
not establish hostility. Every imported contact has a neutral scenario affiliation; there
are no hostile acts or scenario-directed engagement decisions.

The original NOAA Parquet URL, object metadata, bounding box, UTC window, source checksum
and [CC0 license reference](https://raw.githubusercontent.com/ocm-marinecadastre/ais-vessel-traffic/main/data/ais-broadcast-points-2024-readme.md)
are in `provenance.json`. The source can be queried with DuckDB and its spatial/httpfs
extensions; no DuckDB or network access is required to run the scenario or rebuild its TOML:

```sql
SELECT * EXCLUDE(geometry), ST_X(geometry) AS lon, ST_Y(geometry) AS lat
FROM read_parquet('https://ocmgeodatastor1.blob.core.windows.net/marinecadastre/ais2024/ais-2024-06-14.parquet')
WHERE base_date_time >= TIMESTAMP '2024-06-14 15:00:00'
  AND base_date_time < TIMESTAMP '2024-06-14 16:00:00'
  AND ST_X(geometry) BETWEEN -122.56 AND -122.38
  AND ST_Y(geometry) BETWEEN 37.77 AND 37.88;
```

CSV row order follows the original extraction and is pinned by its SHA-256. A new upstream
export can reorder rows; review changes instead of silently replacing this fixture. AIS
fields are described in NOAA's [data dictionary](https://coast.noaa.gov/data/marinecadastre/ais/data-dictionary.pdf).

## How the replay works

`scripts/import_morning_harbor.py` deterministically derives
`sim/scenarios/morning-harbor-watch.toml` and `weather.json` from the bundled sources:

```sh
python3 scripts/import_morning_harbor.py
python3 scripts/import_morning_harbor.py --check
```

Contacts use `motion = "recorded"` with a timestamped `samples` array. Times are seconds from
the contacts process's 15:00 UTC epoch (the situation panel starts a few seconds earlier with
the link emulator). Positions interpolate linearly between neighboring fixes only when their
gap is at most 300 seconds. Course takes the shortest arc through north; recorded SOG is
converted from knots to m/s and interpolated independently of position. This does not recover
the actual maneuver between reports. Changing simulation rate or step size never changes the
position at a particular replay timestamp. Recorded movement is not additionally displaced by
simulated currents.

No interpolation crosses a longer gap. The final fix before a gap, an isolated fix and the
last fix each occupy a one-second bin, matching the source timestamps' resolution. They then
become unavailable; there is no position extrapolation, invented loiter or looping. At faster
rates a live tick can skip one-second isolated reports. Normal 1x playback samples at 1 Hz.
The tracker's uncertainty and lost-contact handling still operate normally when truth becomes
unavailable. This is deliberately conservative about source coverage, not a claim that the
real vessel disappeared.

Unavailable SOG/COG values remain absent from the samples; the motion model infers velocity
from a supported position segment, or uses zero for an isolated fix. Missing heading is not
turned into a hull orientation. Coarse AIS-type mappings select Murmur sensor-signature
defaults: for example all passenger vessels use the ferry kind, including the cruise ship.
Reported length overrides the signature's characteristic dimension when available. Radar,
thermal and acoustic signatures remain assumptions, not measured properties of these hulls.
AIS names, MMSIs and A/B transceiver classes remain the recorded values.

These paths are **simulation truth inputs**. The bridge and watch officer see only what
Murmur's simulated sensors detect; raw truth is not inserted into the operator's track list.
The AIS data supplies no real sensor performance, intent or threat labels. A future altered
scenario should fork this baseline, use fictional identities for altered actors, and record
the changes separately from the original clip.

## Environment and limits

`wind.json` holds NOAA CO-OPS station 9414290's six-minute wind observations from 15:00 to
16:00 UTC, with the exact API URL. `weather.json` converts mean wind to east/north components
for Murmur's existing weather sampler. Wind is a meteorological FROM bearing, so the velocity
components have the opposite sign. Between reports the components interpolate. One station
is assumed to represent the scene, explicitly named in the weather source/model label; it is
not a measured spatial wind field. The Weather panel and physical sensing use it. Gusts stay
in the source JSON because the current archive schema describes hourly gusts, which these
six-minute observations are not. Visibility, cloud, rain and other absent measurements stay
absent; sensing uses declared assumptions of 10 km visibility and sea state 2. Outside the
wind archive it falls back to a declared 2.7 m/s, not a new observation. Sun elevation follows
the actual historical simulation clock.

`tide-predictions.json` holds NOAA tide **predictions**, falling from 0.934 to 0.736 m above
MLLW over this hour. They are context only and do not force the vehicles or the water surface.
They must not be added directly to NAVD88 terrain. The station's measured water levels for
this period have a [sediment-related warning](https://tidesandcurrents.noaa.gov/waterlevels.html?id=9414290),
which is why this bundle records predictions instead of presenting those measurements as
reliable water levels.

This watch sets `tidewater = "none"` and headless currents to `none`: it never borrows the
2026 current/tide archive for a 2024 replay. Vehicle water dynamics use still water, and live
wind is a sensing/context input rather than a Tidewater vehicle forcing. Live land/water
route checks and radio terrain shadows are not evaluated without a terrain service. Physical
sensing uses the existing baked Golden Gate terrain as static reference geometry (baked for
2026, not a reconstructed 2024 shoreline); headless links also use that reference grid.
There is no recorded radar, camera, bathymetry survey, fog field or actual mission history
behind this watch. Its purpose is to put the operator's simulated work amid observed traffic
timing, with those limits inspectable.
