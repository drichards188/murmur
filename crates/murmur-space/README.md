# murmur-space

The space segment for Murmur, as a library plus a CLI. Satellites are **taskable
infrastructure, not flown nodes**: the operator asks for a capability over an area and a time
window (imagery or SAR collection, SATCOM coverage, GNSS assurance); this crate computes what is
feasible from real public orbits and approximate sensor constraints, with timing (collect,
downlink, product in hand), and says why everything else was excluded. A human picks.

It is the computational core for the Phase C intent pipeline (`crates/murmur-decide`, future
`collect` / `cover` intents), the SATCOM link class of the link emulator
(`crates/murmur-linkemu`), GNSS availability, and the console's space views (access timeline,
footprints on the map, globe inset).

```
cargo build --release -p murmur-space
target/release/murmur-space fetch                      # once; cached in sim/space/cache/
target/release/murmur-space catalog
target/release/murmur-space passes --area golden-gate --from 2026-06-14T07:00Z
target/release/murmur-space task --area golden-gate --from 2026-06-14T07:00-07:00 \
    --to 2026-06-14T16:00-07:00 --capability sar,optical --rank best-resolution \
    --deadline 2026-06-14T16:00-07:00
target/release/murmur-space link --lat 37.80 --lon -122.60 --from 2026-06-14T07:00Z --utc-offset -7
target/release/murmur-space gnss --lat 37.80 --lon -122.60 --from 2026-06-14T07:00Z --step 3600
target/release/murmur-space positions --class worldview --at 2026-06-14T18:00Z --footprints --json
```

Every command takes `--json`, `--epoch auto|shift|direct` and `--utc-offset <h>`. Times are
RFC 3339 (`2026-06-14T16:00-07:00`, `2026-06-14T23:00Z`, or a bare date).

## Design

| Module | What it does |
|---|---|
| `orbit` | SGP4 via the `sgp4` crate (pure Rust, Vallado 2006 algorithm), from CelesTrak OMM JSON or TLE lines. Improved mode by default (as in Skyfield); AFSPC mode reproduces the verification vectors. Launch-date estimate per object. |
| `geo` | TEME to Earth-fixed by GMST (IAU-82, the TEME convention), WGS84 geodetic, look angles, off-nadir and incidence, sun position (Astronomical Almanac low-precision, ~0.01 deg), field-of-regard geometry. UT1 = UTC and no polar motion (budget below). |
| `catalog` | `sim/space/constellations.toml`: "-class" entries, CelesTrak queries, name filters, sensor, comms and timing models, per-satellite overrides, `guessed` fields, sources. |
| `cache` | `murmur-space fetch`: one file per query in `sim/space/cache/` (gitignored), never refetched inside CelesTrak's 2-hour update cycle (even with `--force`), default refetch after 12 h, sequential with a 2 s pause, stops at the first HTTP error (CelesTrak firewalls IPs with 50 errors in 2 h). A 403 or 429 starts a backoff stored in `sim/space/cache/backoff.json` (2 h for 403, `Retry-After` or 30 min for 429; reruns request nothing until it ends) and prints CelesTrak's own explanation. The User-Agent names the tool and a contact: set `MURMUR_SPACE_CONTACT` to a URL or address (the default is a placeholder). Requests use `FORMAT=JSON` and the catalog's uppercase `GROUP=`, `NAME=`, `CATNR=` forms. `import` stores element sets from elsewhere (e.g. historical data from Space-Track). |
| `timemap` | Scenario time versus propagation time (epoch honesty, below). |
| `access` | Window search. Every constraint (elevation mask, off-nadir limit, incidence band) fits inside a cone of central angle `psi_max` around the target, and the sub-satellite point moves at a bounded angular rate, so far from the cone the search jumps ahead by `(psi - psi_max) / rate` without missing anything, steps at 5 s (shorter for narrow nadir strips) near it, and bisects edges to 0.1 s. Peak by golden-section search. |
| `space` | `Space`: loaded catalog plus satellites, and the queries: imaging access, SATCOM links (windows, coverage, outages, visible count, latency), instantaneous link state, GNSS visibility and DOP, positions and footprints, ground tracks. |
| `tasking` | Ranked options with exclusions, plus a `CloudModel` hook (no cloud model yet). |
| `gnss` | DOP from line-of-sight geometry. |

Tasking: candidate passes are found with a relaxed envelope (the class limit plus 20 deg), each
pass is sampled every second and checked against the sensor's geometric limit (off-nadir for
optical, incidence band for SAR), daylight (optical: sun elevation at the target), the tasking
lead time from the request time, the window, the product deadline and the requested resolution.
The finest feasible sample of a pass becomes an option; a pass with none becomes an exclusion
naming the first constraint that failed (`night`, `off_nadir`, `incidence`, `too_soon`,
`product_too_late`, `resolution_too_coarse`, `not_taskable`). Options are ranked by earliest
product or best resolution; untaskable routine collects (Dove-class) rank after taskable ones and
are labelled. Optical resolution scales the nadir GSD by slant range over altitude and by
1/sqrt(cos incidence), which reproduces WorldView-3's published 0.34 m at 20 deg off-nadir from
0.31 m at nadir. SAR resolution is the class figure. Product time = collect + downlink latency +
processing, all class assumptions (most are marked guessed; there is no ground-station model
yet).

## Catalog

`sim/space/constellations.toml`, approximate public figures, blanks where unknown, `guessed`
lists our estimates, `sources` says where the rest come from.

| Class | Capability | Elements (CelesTrak) | Key figures |
|---|---|---|---|
| Starlink-class | satcom | `GROUP=starlink` | mask 25 deg (20 below 500 km, 10 below 400 km: FCC, April 2026), bent pipe, network latency 8 ms one way (guess), ~100/10 Mbit/s |
| Iridium-class | satcom | `GROUP=iridium-NEXT` | mask 8.2 deg, crosslinks, Certus 700 rates, latency unknown |
| WGS-class | satcom | `NAME=WGS` | GEO, mask 5 deg (guess), rates blank |
| Ku GEO-class (Intelsat) | satcom | `GROUP=intelsat` | GEO, mask 5 deg and network latency 20 ms (guesses), commercial Ku bent pipe, rates blank |
| WorldView-class | optical | `NAME=WORLDVIEW`, `LEGION`, `GEOEYE` | per-satellite GSD 0.30-0.50 m and swath from datasheets; off-nadir 30 deg, sun >= 15 deg, lead 120 min, downlink 45, processing 60 (guesses) |
| SkySat-class | optical | `GROUP=planet`, `SKYSAT*` | 0.5 m, 5.5 km; off-nadir 30 deg (guess) |
| Dove-class | optical | `GROUP=planet`, `FLOCK*` | 3.7 m, 32.5 km nadir strip, not taskable |
| Capella-class | sar | `NAME=CAPELLA` | spotlight 0.5 m, incidence 25-40 deg (standard products) |
| ICEYE-class | sar | `NAME=ICEYE` | spot 1 m ground range, incidence 20-40 deg |
| GPS | gnss | `GROUP=gps-ops` | 5 deg mask (SPS Performance Standard) |
| ISS | reference | `CATNR=25544` | 10 deg, for cross-checks only |

## Epoch honesty

The scenario is Golden Gate, 13-16 June 2026; public element sets are current (fetched
2026-10-02). SGP4 error grows with distance from the element epoch, and LEO satellites
(Starlink especially) manoeuvre. Historical element sets need a Space-Track account.

**Approach (default `--epoch auto`):** if the window is more than 2 days from the elements'
reference time (the median epoch of everything loaded), propagate at **scenario time + N whole
days**, with N chosen so the window's middle lands on the reference. Every output is reported in
scenario time and every report carries the `time_map` (`shift_days`, `reference`). For the June
scenario N = +109 today.

- Whole days keep the UTC (and so local) time of day. Sun-synchronous imagers (WorldView,
  SkySat, Dove, most ICEYE) keep their plane fixed relative to the sun, so they still pass at
  their real local times (WV-3 late morning).
- **Ground illumination uses the scenario time**: sun elevation for optical collects is the
  June sun at the Golden Gate (75 deg at solar noon, long days), not October's.
- **Satellites launched after the scenario start are dropped** (estimated from the designator
  year and the revolution count at epoch): 969 of 11,125 Starlink objects and 4 ICEYE for
  14 June. Satellites retired between June and October cannot be restored.
- **What is not real:** the specific passes are those of the October constellation on the
  shifted day, not the passes that happened on 14 June. Counts, durations, geometry, revisit
  and coverage statistics are representative; a claim like "WV-3 passed at 11:49 on 14 June" is
  not. Non-sun-synchronous planes (Starlink 53/43 deg, Capella and Legion mid-inclination, ISS)
  drift relative to the sun at their own rates, so their local pass times in June were
  effectively different.

**Error budget in shift mode** (one-day window, so propagation within about +-0.5 day of the
reference plus each satellite's own epoch spread, capped at `max_element_age_days = 14`):

| Source | Size | Effect |
|---|---|---|
| SGP4 theory vs reality at epoch | ~1 km LEO (public figure, varies) | pass times ~0.1-0.3 s, elevation << 0.1 deg |
| Growth away from epoch (drag, unmodelled manoeuvres) | ~1-3 km/day LEO, more after a manoeuvre | ~0.2-0.5 s per day of offset |
| This crate vs Skyfield (UT1, polar motion, frame chain) | median 56 m, max 289 m (GEO) | ISS rise/set within 0.19 s |

**Direct mode** (`--epoch direct` on a June window with October elements): 109 days of
propagation. LEO along-track errors of hundreds to thousands of km (drag uncertainty grows
quadratically; Starlink manoeuvres are not in the elements), so pass times are wrong by minutes
or worse and cannot be checked; GEO and GPS degrade more gracefully. Use direct mode only for
windows near the element epoch (the default does this automatically within 2 days) or with
historical element sets loaded via `murmur-space import --query GROUP=starlink file.json`.

## Verification

- `cargo test -p murmur-space`: 23 tests. `cargo clippy --all-targets -- -D warnings` clean.
- **SGP4 verification vectors** (Vallado et al., AIAA 2006-6753, SGP4-VER.TLE, all 33 cases,
  over 500 states plus the expected error cases): positions within 1 mm and velocities within
  1e-9 km/s in AFSPC mode (`orbit::tests::verification_vectors_afspc`).
- GMST against Vallado Example 3-5; sun elevation at the June solstice; geodetic round trips;
  ideal-tetrahedron GDOP (1.58).
- **Independent cross-check, Skyfield 1.55** (`tools/crosscheck_skyfield.py`, same cached
  elements, direct mode): Earth-fixed positions for 1,224 samples across all classes at three
  instants: median 56 m, p99 288 m, max 289 m (GEO; consistent with UT1-UTC and polar motion,
  which this crate ignores). ISS passes above 10 deg over the Golden Gate Bridge, 2-5 October
  2026: 15 of 15 found, rise/set within 0.19 s, peak elevation within 0.002 deg. The first 11
  are a regression test against a fixture (`tests/space.rs::iss_passes_match_skyfield`, 0.5 s
  tolerance).
- The adaptive search agrees with a brute-force 1 s scan for 6 satellites per class over a day
  (window edges within 1.1 s).

## Performance

Release build, WSL2, 11,125 Starlink + 315 other objects loaded:

| Query | Wall time |
|---|---|
| Load catalog and all element sets | 0.07 s |
| Starlink link windows, 1 day, one point, plus 1,440 coverage/latency samples | 1.98 s on 1 thread, 0.43 s on 6, 0.24 s on 28 |
| All SATCOM classes, 3 days | 6.95 s on 1 thread |
| Imaging access, all imaging classes, 3 days | 0.09 s on 1 thread |
| Tasking options, 1 day | 0.08 s on 1 thread |
| GNSS, 1 day at 60 s | 0.08 s on 1 thread |

## Example outputs (Golden Gate, scenario day 14 June 2026)

Element sets fetched 2026-10-02, so every run below is time-shifted +109 days.

### A day's imaging access windows by class

```
$ murmur-space passes --area golden-gate --from 2026-06-14T07:00Z --limit 8
Imaging access over Golden Gate scenario box (tidewater golden-gate-2026-06), 06-14 00:00:00-7 to 06-15 00:00:00-7 (scenario time)
epoch: TIME-SHIFTED +109 d (orbits propagated at 2026-10-01 07:00 to 2026-10-02 07:00 UTC, same time of day; sun at scenario time); elements reference 2026-10-01 22:18Z
target 37.8250,-122.4700  area radius 10.0 km  compute 7 ms

class      cap        sats  stale launched>  windows   night
worldview  optical      10      0         0        7       3
skysat     optical      14      0         0        0       2
dove       optical      92      0         0        1       7
capella    sar           9      0         0       13       0
iceye      sar          53      0         4       63       0

WorldView-class high-resolution optical (7 windows)
  satellite                best (local)        dur s offndr  incid     el  res m   sun
  LEGION 6                 06-14 08:33:27-7       33   28.4   30.8   59.2   0.37    30
  LEGION 5                 06-14 09:54:16-7       90    3.2    3.3   86.7   0.30    46
  WORLDVIEW-3 (WV-3)       06-14 11:49:36-7       45   27.5   30.5   59.5   0.38    67
  WORLDVIEW-2 (WV-2)       06-14 12:06:13-7      131    7.9    8.8   81.2   0.47    70
  LEGION 2                 06-14 12:30:34-7       34   28.0   30.5   59.5   0.37    73
  LEGION 5                 06-14 14:54:26-7       89    7.9    8.4   81.6   0.31    63
  WORLDVIEW-1 (WV-1)       06-14 14:56:36-7       73   15.1   16.4   73.6   0.53    63

Dove-class nadir monitoring optical (1 windows)
  satellite                best (local)        dur s offndr  incid     el  res m   sun
  FLOCK 4H-24              06-14 11:57:12-7        3    1.3    1.4   88.6   3.71    69

Capella-class X-band SAR (13 windows, first 8)
  satellite                best (local)        dur s offndr  incid     el  res m   sun
  CAPELLA-20 (ACADIA-10)   06-14 04:25:24-7       33   22.9   25.0   65.0   0.50   -14
  CAPELLA-20 (ACADIA-10)   06-14 04:26:15-7       33   22.7   25.0   65.0   0.50   -14
  CAPELLA-19 (ACADIA-9)    06-14 06:41:38-7       88   28.4   31.4   58.6   0.50     9
  CAPELLA-18 (ACADIA-8)    06-14 06:55:25-7       29   22.9   25.1   64.9   0.50    11
  CAPELLA-18 (ACADIA-8)    06-14 06:56:39-7       28   22.6   25.1   64.9   0.50    12
  CAPELLA-11 (ACADIA-1)    06-14 08:09:01-7       30   22.6   25.1   64.9   0.50    25
  CAPELLA-11 (ACADIA-1)    06-14 08:10:13-7       30   22.8   25.1   64.9   0.50    25
  CAPELLA-16 (ACADIA-6)    06-14 11:54:47-7       38   22.8   25.0   65.0   0.50    68

ICEYE-class X-band SAR (63 windows, first 8)
  ...
```

A SAR pass that goes nearly overhead appears as two windows (CAPELLA-20 above): the target is
too steep (incidence under 25 deg) in between. Optical windows in darkness are counted in
`night` and hidden unless `--include-night`.

### Tasking: "SAR or optical, best resolution, product before 16:00"

```
$ murmur-space task --area golden-gate --from 2026-06-14T07:00-07:00 --to 2026-06-14T16:00-07:00 \
    --now 2026-06-14T07:00-07:00 --capability sar,optical --rank best-resolution \
    --deadline 2026-06-14T16:00-07:00 --exclusions 10
Tasking request: sar or optical over Golden Gate scenario box (tidewater golden-gate-2026-06); collect 06-14 07:00:00-7 to 06-14 16:00:00-7; requested 06-14 07:00:00-7; product by 06-14 16:00:00-7
epoch: TIME-SHIFTED +109 d (orbits propagated at 2026-10-01 14:00 to 2026-10-01 23:00 UTC, same time of day; sun at scenario time); elements reference 2026-10-01 22:18Z
ranked by BestResolution; cloud model: none (cloud cover not modelled); compute 9 ms

23 feasible options (first 10)
 #  class     satellite              type    collect (local)     offndr  incid  res m product (local)     notes
 1  worldview LEGION 5               optical 06-14 09:54:15-7       3.2    3.3   0.30 06-14 11:39:15-7    timing est., no cloud model
 2  worldview LEGION 2               optical 06-14 12:30:34-7      28.0   30.5   0.37 06-14 14:15:34-7    timing est., no cloud model
 3  worldview WORLDVIEW-3 (WV-3)     optical 06-14 11:49:36-7      27.6   30.5   0.38 06-14 13:34:36-7    timing est., no cloud model, partial area
 4  worldview WORLDVIEW-2 (WV-2)     optical 06-14 12:06:14-7       7.9    8.8   0.47 06-14 13:51:14-7    timing est., no cloud model, partial area
 5  capella   CAPELLA-11 (ACADIA-1)  sar     06-14 08:09:01-7      22.7   25.1   0.50 06-14 09:09:01-7    timing est., partial area
 6  capella   CAPELLA-16 (ACADIA-6)  sar     06-14 11:54:47-7      22.8   25.0   0.50 06-14 12:54:47-7    timing est., partial area
 7  capella   CAPELLA-11 (ACADIA-1)  sar     06-14 14:56:50-7      32.2   35.5   0.50 06-14 15:56:50-7    timing est., partial area
 8  iceye     ICEYE-X2               sar     06-14 10:38:25-7      18.7   20.1   1.00 06-14 12:38:25-7    timing est., partial area
 9  iceye     ICEYE-X46              sar     06-14 11:02:30-7      18.2   20.1   1.00 06-14 13:02:30-7    timing est., partial area
10  iceye     ICEYE-X50              sar     06-14 11:08:48-7      18.7   20.4   1.00 06-14 13:08:48-7    timing est., partial area

43 passes excluded:
  capella    Incidence                 4
  capella    ProductTooLate            1
  dove       OffNadir                 15
  iceye      Incidence                13
  iceye      ProductTooLate            4
  skysat     OffNadir                  2
  worldview  OffNadir                  1
  worldview  TooSoon                   1
  worldview  ProductTooLate            2

examples:
  iceye     ICEYE-X63              06-14 07:20:36-7    Incidence        incidence 41 deg at best, usable 20-40
  capella   CAPELLA-13 (ACADIA-3)  06-14 07:37:48-7    Incidence        incidence 49 deg at best, usable 25-40
  skysat    SKYSAT-C2              06-14 08:07:20-7    OffNadir         off-nadir 34.9 deg at best, limit 30.0
  worldview LEGION 6               06-14 08:33:27-7    TooSoon          pass before earliest collect 16:00Z (lead 120 min)
  dove      FLOCK 4H-12            06-14 11:54:10-7    OffNadir         off-nadir 9.7 deg at best, limit 1.9
  worldview GEOEYE 1               06-14 12:38:52-7    OffNadir         off-nadir 46.7 deg at best, limit 30.0
  iceye     ICEYE-X55              06-14 14:53:44-7    ProductTooLate   collect 21:53Z -> product 23:53Z after deadline 23:00Z
  worldview LEGION 5               06-14 14:54:26-7    ProductTooLate   collect 21:54Z -> product 23:39Z after deadline 23:00Z
  capella   CAPELLA-20 (ACADIA-10) 06-14 15:15:10-7    ProductTooLate   collect 22:15Z -> product 23:15Z after deadline 23:00Z
```

"partial area": the class swath is narrower than the 20 km Golden Gate box, so one scene does
not cover it.

### SATCOM link windows for a vehicle position (USV west of the Gate)

```
$ murmur-space link --lat 37.80 --lon -122.60 --from 2026-06-14T07:00Z --class starlink --class wgs --class iridium --utc-offset -7 --limit 6
SATCOM links from 37.8000,-122.6000 (alt 0 m), 06-14 00:00:00-7 to 06-15 00:00:00-7
epoch: TIME-SHIFTED +109 d (orbits propagated at 2026-10-01 07:00 to 2026-10-02 07:00 UTC, same time of day; sun at scenario time); elements reference 2026-10-01 22:18Z
compute 201 ms

class       sats used   mask  windows  cover %   max gap s       visible       latency ms
starlink  10156/11125   25.0    35016    100.0           0 77-129 (99.7)         10/11/12
wgs             10/10    5.0        4    100.0           0     4-4 (4.0)      249/249/249
iridium         80/80    8.2      325    100.0           0     1-6 (2.0)          unknown
(visible: min-max (mean) sampled every 60 s; latency: one-way min/median/max to the best satellite)

Starlink-class LEO broadband: 35016 windows, first 6
  satellite                  start (local)       end                   dur s max el     az range km   lat ms
  STARLINK-1266              06-14 00:00:00-7    06-14 00:02:48-7        169   68.2    224      361       10
  STARLINK-1810              06-14 00:00:00-7    06-14 00:01:18-7         78   12.6    330      951       14
  STARLINK-2514              06-14 00:00:00-7    06-14 00:01:40-7        100   30.5     35      806       13
  STARLINK-2663              06-14 00:00:00-7    06-14 00:01:21-7         82   25.6    330      968       14
  STARLINK-2687              06-14 00:00:00-7    06-14 00:04:41-7        281   66.1    319      514       11
  STARLINK-3246              06-14 00:00:00-7    06-14 00:01:54-7        114   25.9    328      959       14

WGS-class GEO military SATCOM: 4 windows
  satellite                  start (local)       end                   dur s max el     az range km   lat ms
  WGS F3 (USA 211)           06-14 00:00:00-7    06-15 00:00:00-7      86400   46.5    179    37299      249
  WGS F5 (USA 243)           06-14 00:00:00-7    06-15 00:00:00-7      86400    7.0    102    40905      273
  WGS F6 (USA 244)           06-14 00:00:00-7    06-15 00:00:00-7      86400   44.3    200    37454      250
  WGS F7 (USA 263)           06-14 00:00:00-7    06-15 00:00:00-7      86400   13.1    252    40258      269

Iridium-class LEO voice/data: 325 windows, first 6
  satellite                  start (local)       end                   dur s max el     az range km   lat ms
  IRIDIUM 100                06-14 00:00:00-7    06-14 00:03:01-7        182   27.1    329     1458        -
  ...
```

Starlink visible counts include every catalogued object in the group above its mask (also those
raising or lowering orbit), so they overstate usable beams. Latency: bent pipe counts the user
leg twice (gateway assumed at a similar range) plus the class network allowance; Iridium's is
blank because its network latency is not in the catalog.

### GNSS visibility

```
$ murmur-space gnss --lat 37.80 --lon -122.60 --from 2026-06-14T07:00Z --step 3600 --utc-offset -7
GNSS (gps) at 37.8000,-122.6000, mask 5 deg, 32 satellites (1 stale, 0 launched later: skipped)
epoch: TIME-SHIFTED +109 d (...)
visible 8-12, PDOP mean 1.58 max 2.11; compute 5 ms

time (local)         vis   GDOP   PDOP   HDOP   VDOP
06-14 00:00:00-7      10   1.48   1.33   0.84   1.03
06-14 01:00:00-7      10   1.80   1.60   0.93   1.29
06-14 02:00:00-7      11   1.47   1.34   0.79   1.09
...
06-14 11:00:00-7       8   2.44   2.11   1.06   1.83
...
```

(The stale GPS satellite's element set at CelesTrak dates from 17 September, more than the
14-day limit before the end of the propagation window.)

## Integration contract

All types are `serde` (JSON with RFC 3339 UTC times, degrees, km unless the field name says
otherwise). Load once, query many times:

```rust
let space = Space::load(Path::new("sim/space/constellations.toml"), Path::new("sim/space/cache"))?;
```

`Space` is immutable after load and `Sync`; queries are pure functions of (catalog, element sets,
query), deterministic (parallel results are collected in order, sorts are stable). For
replayable decisions, record the query, the result's `time_map`, and `space.cache_meta` (fetch
times per query) as the decision's inputs.

Times passed in and returned are **scenario time**. Each report carries `time_map` (`mode`,
`shift_days`, `reference`); callers must show it when `shift_days != 0` (the CLI prints
"TIME-SHIFTED").

**murmur-decide (intents `collect`, `cover`).**
`space.tasking_options(&TaskRequest, &dyn CloudModel) -> TaskingResult`. `TaskRequest`:
`capabilities` (`optical`, `sar`), `area` (`{"kind":"polygon","ring":[[lon,lat],..]}` or
`{"kind":"circle","center":[lon,lat],"radius_km":..}`), `from`/`to` (collection window), `now`
(request time; lead time counts from here), `product_deadline`, `max_resolution_m`, `rank`
(`earliest_product` | `best_resolution`), `classes`, `include_untaskable`, `epoch_mode`.
`TaskingResult.options[]` (ranked `TaskOption`: `class_id`, `satellite`, `norad_id`,
`collect_time`, `geometry` {elevation, azimuth, range, off_nadir, incidence, sat_alt},
`resolution_m`, `sun_elevation_deg`, `earliest_collect`, `product_time`, `taskable`,
`cloud_free_probability`, `scene` ring, `covers_area`, `element_age_days`, `notes`) and
`exclusions[]` (`reason` enum and a human `detail`) plus `exclusion_summary`. The intent should
carry the chosen option's `norad_id` + `collect_time`; a policy rule can re-run the query at
approval time and void the approval if that option is gone. Example option:

```json
{"rank": 1, "class_id": "capella", "capability": "sar", "satellite": "CAPELLA-11 (ACADIA-1)",
 "norad_id": 57693, "collect_time": "2026-06-14T15:09:01.597702Z",
 "geometry": {"elevation_deg": 64.89, "azimuth_deg": 206.0, "range_km": 659.4,
              "off_nadir_deg": 22.66, "incidence_deg": 25.11, "sat_alt_km": 602.6},
 "resolution_m": 0.5, "sun_elevation_deg": 25.2, "earliest_collect": "2026-06-14T15:00:00Z",
 "product_time": "2026-06-14T16:09:01.597702Z", "taskable": true,
 "cloud_free_probability": null, "scene": [[-122.4297, 37.8248], ...], "covers_area": false,
 "element_age_days": -0.53, "notes": ["tasking_lead_min is an estimate", ...]}
```

Cloud hook: implement `CloudModel` (`cloud_free_probability(&Geodetic, DateTime) -> Option<f64>`);
nothing filters on it yet (a rule could).

**murmur-linkemu (SATCOM link class).** Two ways:
- Precompute: `space.links(&LinkQuery { point, from, to, classes, sample_step_s, include_samples: true, .. })`
  gives per class `windows[]`, `outages[]` (intervals with no satellite above the mask),
  `samples[]` (`t`, `visible`, `best` {norad, elevation, azimuth, range_km, latency_ms}),
  `user_down_mbps` / `user_up_mbps`, `path`. Good for a fixed or slow vehicle.
- Live: `space.link_at(class_id, Geodetic, t, &time_map) -> LinkSample` at the vehicle's current
  position (Starlink: about 10k SGP4 calls per query, spread over threads; not yet benchmarked
  per vehicle per second). Link down when `visible == 0`; latency from
  `best.latency_ms`; bandwidth from the class.

Platforms with `link = "satcom"` need a class mapping, which is a linkemu (or `platforms.toml`)
decision; suggested: drifting buoy -> `iridium` (SBD messages); Saildrone-class -> `iridium` or
`starlink`; Magura/Corsair/Sea Hunter-class -> `starlink`; RQ-4 / MQ-9B-class -> `wgs` (GEO) or `ku-geo`
(the commercial Ku GEO capacity they also use). Underwater: no SATCOM unless surfaced
(linkemu's rule).

**GNSS availability.** `space.gnss(&GnssQuery { point, from, to, step_s, class: "gps", mask_deg, epoch_mode })`
-> `samples[]` (`visible`, `dop` {gdop, pdop, hdop, vdop, tdop}). Geometry only: jamming, masking
by terrain or buildings and underwater loss belong to the consumer.

**murmur-bridge (HTTP).** Suggested endpoints that wrap the calls above one to one:
`GET /api/space/catalog`, `GET /api/space/passes?area=&from=&to=` (`AccessReport`),
`POST /api/space/task` (`TaskRequest` -> `TaskingResult`),
`GET /api/space/links?lat=&lon=&from=&to=&class=` (`LinkReport`),
`GET /api/space/gnss?...` (`GnssReport`), `GET /api/space/positions?class=&t=&footprints=1`
(`SatPosition[]`). The bridge owns the scenario clock (`MURMUR_AT`) and fetching (`fetch` is
polite enough to run at startup: it does nothing if the cache is fresh).

**Console.** Timeline (primary space view): `AccessReport.windows[]` (`start`, `end`,
`class_id`, `capability`, `best_time`, `daylight_ok`, `resolution_m`) and
`LinkClassReport.outages[]`. Theater map: `TaskOption.scene` rings, `Area.ring()`, footprints
from `positions(.., with_footprint)` (field of regard for imagers, mask circle for comms),
`ground_track(norad, from, to, step)` as `[lon, lat]` lines. Globe inset: `SatPosition.position`
(geodetic, km).

## Not modelled yet

Clouds (hook only), ground stations and real downlink scheduling, satellite agility and
competing tasking demand, multiple collects per pass, Starshield and other non-public
constellations, Galileo/GLONASS/BeiDou, atmospheric refraction, eclipses, beam capacity.
