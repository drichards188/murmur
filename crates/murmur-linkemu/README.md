# murmur-linkemu

A user-space UDP link emulator for Murmur (Phase D). It sits between the vehicles and the
bridge and degrades each vehicle's link according to its link class, with no root and no
`tc netem`. See `docs/DECISIONS.md` for why.

```
vehicle i  --UDP-->  [emulator listens on A+i]  --UDP-->  bridge listens on B+i
           <--UDP--  [one task per vehicle     ]  <--UDP--
```

Each vehicle gets its own task, vehicle-facing socket (port `A+i`), bridge-facing socket
(its own source port, so the bridge's replies come back to it) and a delay queue per
direction. The vehicle's source address is learned from its first packet; bridge replies go
back to it. Packets are never modified. Nothing crosses between links.

## Wiring

1. Launch the vehicles with a base port the bridge does not use:
   `swarm.py --fleet <f> --base-port A` (writes `sim/run/fleet.json`, each vehicle with its
   `uid`, `sysid`, `port`, `link`, `domain`, `platform`).
2. Start the emulator: `murmur-linkemu --bridge-base B --manifest-out sim/run/bridge-fleet.json`
   (defaults: `--fleet sim/run/fleet.json`, `--config sim/links.toml`, vehicle base = lowest
   port in the manifest).
3. Start the bridge on `B` and point it at the rewritten manifest:
   `MURMUR_BASE_PORT=B MURMUR_FLEET_MANIFEST=sim/run/bridge-fleet.json`.

Step 3's manifest matters: the Python bridge opens exactly the ports its manifest lists, so
reading the launcher's manifest it would bind the vehicle-facing ports and fight the emulator.
`--manifest-out` writes a copy with `port` replaced by the bridge-side port (the original is
kept as `vehicle_port`). The Rust bridge should do the same, or take the port as `B + (uid-1)`.

Terrain and scenario time: `--tidewater URL` (or `MURMUR_TIDEWATER_URL`) turns on terrain line of
sight; `--at ISO` / `--rate X` (or `MURMUR_AT` / `MURMUR_RATE`) set the scenario clock. Other
options: `--scenario FILE` (extra `[[event]]`s), `--seed N`, `--http ADDR` (default
`127.0.0.1:8766`, `off` disables), `--log-interval SEC`, `--threads N` (default 1; one thread used less CPU than two at these rates),
`--bridge-host`, `--listen-host`.

## Model

Per packet, per direction (the two directions have independent random streams and queues):

1. **Blocked** (no airtime used): the link is down for the vehicle's depth (`down_when`), a
   scripted `cut` is active, or terrain blocks the path (`terrain = "block"`).
2. **Queue**: with a bandwidth cap the link is a virtual transmitter. A packet's airtime is
   `bytes*8/bw`; the backlog is the bytes still ahead of it, and a packet that would push the
   backlog (the one in service included) over `queue_bytes` is dropped (drop-tail). So low
   bandwidth gives real queuing delay, then real loss. A hard cap `max_inflight` (default
   2048 packets per direction) bounds memory.
3. **Channel loss**: a Gilbert-Elliott chain. `loss` is the long-run average and `burst_len`
   the mean burst length in packets; lost packets still used their airtime. Then scripted
   extra loss, jammer loss, range loss and terrain loss (`terrain = "ramp"`), each independent.
4. **Delay**: queue wait + `latency_ms` + uniform `+-jitter_ms` + event latency
   + (slant range to the ground station / `propagation_mps`). Delivery times are made
   monotonic unless `reorder = true`, so jitter does not reorder.
5. **Duplicate**: with probability `duplicate` the packet is delivered twice.

Determinism: a ChaCha8 stream per (seed, uid, direction), exactly eight draws per packet
whatever the outcome. The model is a pure function of arrival time and size
(`model.rs`, `link.rs`), tested without sockets. The same seed and input timing give the same
drop decisions. Real runs differ only through socket arrival timing.

### Physics hooks

All driven by `GLOBAL_POSITION_INT` parsed from uplink traffic (mavlink crate, ardupilotmega
dialect; frames are walked by length and only that message is decoded; v1, v2 and
concatenated frames work).

- **Range**: `range_ok_km`/`range_max_km`/`range_exp` give loss from 0 to 1 as distance from
  `[ground_station]` grows.
- **Depth gating**: submerged means depth `-alt` exceeds `submerged_depth_m` (altitude is MSL, negative is
  depth, matching the stand-ins). `down_when = "submerged"` for SATCOM, `"surfaced"` for
  acoustic. Before the first position the link is open.
- **Terrain** (`terrain.rs`, needs `--tidewater URL`): the path from `[ground_station]` to the
  vehicle is sampled from tidewater's DEM and judged against the straight ray between the
  antennas, with Earth curvature at an effective radius of `k_factor` (default 4/3) Earth
  radii. Antenna heights are per profile (`gs_antenna_m` above the station's `alt_m`,
  `antenna_m` above the vehicle's altitude). The result is an obstruction depth: how far the
  highest terrain point (raised by the Earth bulge, and by `fresnel_clear` of the first Fresnel
  radius when `fresnel_mhz` is set) sticks up above the ray. `terrain = "block"` (default for
  profiles with a range model) drops everything while it is positive (cause `terrain`);
  `"ramp"` drops a share that rises from 0 at the ray to 1 at `terrain_ramp_m` deep;
  `"off"` ignores it. The surface is the higher of the DEM and the water level, so a
  vehicle below the sea surface is hidden. Points outside the DEM count as sea level.

  The packet path never waits for HTTP. A link asks a cache keyed by grid cell (`[terrain]
  cell_m`, 250 m), altitude band (`alt_step_m`, 10 m) and the profile's antenna parameters; the
  profile is computed from the cell centre, so every vehicle in a cell gets the same answer. A
  miss queues the cell and the link keeps its previous answer (open, and counted as
  `unknown_pkts`, if it has none yet). Every `interval_s` (1 s) a background task takes the
  queued cells, samples all their profiles (every `sample_spacing_m`, 50 m, at most
  `max_points`) and sends them in as few `POST /v1/sample` requests as fit (20,000 points each).
  Answers are kept for good (terrain does not move); a failed request is counted and retried.
  The request time is the scenario clock, clamped into the scenario's range from `/v1/info`.
  Without a tidewater URL nothing is checked and `los.state` is `off`.
- **Propagation delay**: `propagation_mps = 1500` for sound.

### Scripted events

`[[event]]` in `sim/links.toml` or a `--scenario` file. Start with `at_s` (seconds since the
emulator started) or `at_scenario` (ISO time on the scenario clock: `MURMUR_AT` at emulator
start plus elapsed x `MURMUR_RATE`, the stand-ins' convention; it is an error without
`MURMUR_AT`). `duration_s` is in the start's clock (scenario seconds with `at_scenario`;
`until_scenario` gives an end time instead); omitted means until the end. Start the emulator
with the same `MURMUR_AT` and `MURMUR_RATE` as the stand-ins and shortly after them: the
emulator's clock starts at its own start. Events wholly in the past never fire.
Selectors (all must match): `class`, `profile`, `platform`, `domain`, `uid`, `uids`,
`circle = {lat, lon, radius_m}` (vehicle inside it, by its last known position). Effects:
`loss` (extra, independent), `cut`, `extra_latency_ms`; `dir = both|up|down`. A circle with no
`loss` jams (loss 1.0), counted as cause `jam`.

```toml
[[event]]                      # the 30%-loss study, on the scenario clock
at_scenario = "2026-06-14T10:05:00Z"
duration_s = 120
class = "los"
loss = 0.30
```

Events are reloaded while running, on `SIGHUP` or when the config or scenario file changes
(polled each second). Each link switches to the new schedule at its next packet, so the result
is a function of the file contents and packet arrival order. A file that does not parse (or
a scenario-time event without a scenario clock) is logged and the running events stay.
Profiles, the ground station and `[terrain]` are not reloaded.

## Config: `sim/links.toml`

Profiles are `[class.<name>]`, with `extends` for variants. A vehicle's profile is, in order:
an explicit `[platform_profile]` entry (empty as shipped); for SATCOM vehicles, the profile
that `[space_class_profile]` gives the platform's `space_class` (set in `sim/platforms.toml`,
carried in the fleet manifest, and also what the bridge's space views read); else the
platform's `link` class. Shipped profiles: `los`, `mesh`, `satcom` (GEO, 300 ms
each way for the ~600 ms RTT), `satcom_leo`, `satcom_iridium`, `acoustic` (300 bps, 1500 m/s),
`satcom_ku_geo`, `tether`. Each has a comment with where its numbers come from and which are guesses.

## SATCOM from real satellites (`space_class`)

A profile with `space_class = "starlink" | "iridium" | "wgs" | "ku-geo"` (classes of
`sim/space/constellations.toml`, computed by the `murmur-space` library) follows real satellite
visibility instead of fixed numbers. For each vehicle, from its reported position and the
scenario clock (`MURMUR_AT` + elapsed x `MURMUR_RATE`), a background task refreshes every
`[space].interval_s` scenario seconds (never less than `min_wall_s` of wall time apart; SGP4
runs on a blocking thread) and the packet path reads the latest answer:

- **Up/down**: no satellite above the class's mask means every packet is dropped (cause `no_sat`).
- **Latency**: one-way, from the best (highest elevation) satellite's slant range plus the
  class's network latency (a bent pipe counts the user leg twice). It replaces `latency_ms`;
  jitter still applies. Iridium has no published network latency, so its profile `latency_ms`
  stays the total (`latency_source = "profile"` in `/stats`).
- **Handover**: the best satellite changes while the link stays up. Always counted; with
  `handover_outage_ms > 0` the link also drops that long (cause `handover`, a GUESS in the
  shipped profiles). The outage is wall time, so at a high `MURMUR_RATE` handovers cost
  proportionally more packets than at rate 1. "Best by elevation" changes about as often as,
  or more often than, a real terminal's satellite (Starlink: every 15-20 s here).
- **Time**: public element sets are current, scenarios are dated; `[space].epoch = "auto"` uses
  murmur-space's whole-day time shift (its README, "Epoch honesty"). The map is fixed at start
  and reported in the log and `/stats`.
- **No data**: if `murmur-space fetch` has not been run (no `sim/space/cache`), a class has no
  usable satellites, the source errors, or `--no-space` is given, the profile's static numbers
  apply, stated in the log, in `/stats` (`space.classes.<class>.static_reason`) and per vehicle
  (`satcom.mode = "static"`).
- **Bandwidth** stays the profile's: Iridium (22 kbps) cannot carry the stand-ins' telemetry
  rates and drops most of it. That mismatch is deliberate and visible; link-aware telemetry is
  a separate issue.

`/stats/<uid>` has `satcom`: `class`, `mode` (`pending`, `live`, `static`), `up`, `visible`,
`best_norad_id`, `best_name`, `best_elevation_deg`, `slant_range_km`, `latency_ms` and
`latency_source`, `handovers`, `last_handover_scenario`, and `next_outage_scenario` /
`next_outage_in_s` (link up now) or `next_pass_scenario` / `next_pass_in_s` (link down now),
looked ahead `outlook_s` scenario seconds (null if none in the horizon; GEO has none).
`/stats` also has `space` (per class live or static, element reference, time shift, refresh
round times).

Platform mapping (from `space_class` in `sim/platforms.toml`): buoy to Iridium; magura,
corsair, seahunter and saildrone to Starlink-class; rq4 and mq9b to WGS-class GEO (`satcom`).
`space_class_alt` (saildrone iridium, rq4 and mq9b ku-geo) is not used here: one link per
vehicle. To run the alternative, add a `[platform_profile]` override (`saildrone =
"satcom_iridium"`, `rq4 = "satcom_ku_geo"`).

Ground station altitude: with `--tidewater` the DEM is sampled at `[ground_station]` at
startup; if `alt_m` differs by more than 5 m a warning is logged, and `alt_m = "dem"` uses the
DEM height (the profiles' `gs_antenna_m` mast height is added on top either way).

`src/bin/satprobe.rs` is the live check: fake vehicles at given positions and a fake bridge,
printing the emulator's `/stats` view next to measured delay and loss, e.g.
`satprobe --vehicle magura:37.82:-122.51:0 --vehicle rq4:80:-100:0 --duration 60` (first with
`--write-only` for the manifest, then the emulator with `MURMUR_AT` and `MURMUR_RATE`).

## Observability

`GET /stats` returns a summary plus `per_link`: for each direction in/out packets and bytes,
drops by cause (`loss event_loss jam cut range terrain gate queue no_peer socket no_sat handover`), duplicates,
queued packets and bytes (gauges), mean and max added latency; per link the profile, blocked
cause, depth state, last position and `los`: `state` (`off`, `unknown`, `clear`, `obstructed`),
`obstruction_m` (positive: terrain above the ray; negative: clear by that much),
`unknown_pkts` (decided before any terrain answer) and `blocked_by_terrain_pkts`. `/stats/summary` omits `per_link`; the summary also has `los_links` (links per state),
`terrain_blocked_pkts`, `terrain_unknown_pkts` and `terrain` (the client's cells cached and
pending, requests, errors, points sampled, last request time and last error); `/stats/<uid>` is one
link; `/health`. The summary also has the process CPU seconds and RSS. A one-line log prints
every `--log-interval` seconds (rates and drops by cause since the last line).

## Tests and tools

`cargo test -p murmur-linkemu`: loss convergence, burst length, determinism, jitter and
reordering, bandwidth queuing and tail drop, duplicates, event windows and selectors, jammer
circle, range loss, depth gating, terrain (ridge, curvature, Fresnel, unknown-then-answered, ramp,
batching, an HTTP client against a fake server), scenario time and event reload, propagation, MAVLink parsing, and a real
loopback round trip. `src/bin/losprobe.rs` asks a running
`tidewater serve` whether given positions see a ground station:
`losprobe --gs 37.8068,-122.4460,5 --gs-antenna 10 37.875,-122.405,30 ...`.
`src/bin/loadgen.rs` is the synthetic load generator (fake vehicles and
fake bridge ports, writes its own manifest): `loadgen --vehicles 440 --rate 8 --duration 30`.

## Not done

- Terrain is judged to the ground station only (no relays or mesh hops), from a DEM cell
  centre rather than the exact position, and the DEM may include structures (the Golden Gate
  Bridge reads as 60 to 76 m at Fort Point). `[ground_station].alt_m` must match the ground
  there. No jitter model beyond uniform; no correlated fading
  with range or weather; no per-packet-size loss. Loss does not depend on message type.
- Telemetry priority under low bandwidth (murmur-f6q): the queue is a single FIFO.
- Position comes from uplink only and lags by the link's own delay. No ICMP, MTU or
  fragmentation modelling (datagrams over 2048 bytes are truncated).
- `links.toml` profiles are not reloaded, only events.
- Socket creation is slow in some sandboxes (about 10 ms per socket in this WSL2 harness), so
  1000 links take ~20 s to start there.
