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

Other options: `--scenario FILE` (extra `[[event]]`s), `--seed N`, `--http ADDR` (default
`127.0.0.1:8766`, `off` disables), `--log-interval SEC`, `--threads N` (default 1; one thread used less CPU than two at these rates),
`--bridge-host`, `--listen-host`.

## Model

Per packet, per direction (the two directions have independent random streams and queues):

1. **Blocked** (no airtime used): the link is down for the vehicle's depth (`down_when`), a
   scripted `cut` is active, or terrain says no line of sight.
2. **Queue**: with a bandwidth cap the link is a virtual transmitter. A packet's airtime is
   `bytes*8/bw`; the backlog is the bytes still ahead of it, and a packet that would push the
   backlog (the one in service included) over `queue_bytes` is dropped (drop-tail). So low
   bandwidth gives real queuing delay, then real loss. A hard cap `max_inflight` (default
   2048 packets per direction) bounds memory.
3. **Channel loss**: a Gilbert-Elliott chain. `loss` is the long-run average and `burst_len`
   the mean burst length in packets; lost packets still used their airtime. Then scripted
   extra loss, jammer loss and range loss, each independent.
4. **Delay**: queue wait + `latency_ms` + uniform `+-jitter_ms` + event latency
   + (slant range to the ground station / `propagation_mps`). Delivery times are made
   monotonic unless `reorder = true`, so jitter does not reorder.
5. **Duplicate**: with probability `duplicate` the packet is delivered twice.

Determinism: a ChaCha8 stream per (seed, uid, direction), exactly seven draws per packet
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
- **Terrain**: `geo::Terrain::line_of_sight(from, to)`; `NoTerrain` is the default. Profiles
  with a range model use it and drop with cause `terrain` when blocked. The tidewater DEM
  implementation is not written.
- **Propagation delay**: `propagation_mps = 1500` for sound.

### Scripted events

`[[event]]` in `sim/links.toml` or a `--scenario` file; times are seconds since the emulator
started. Selectors (all must match): `class`, `profile`, `platform`, `domain`, `uid`, `uids`,
`circle = {lat, lon, radius_m}` (vehicle inside it, by its last known position). Effects:
`loss` (extra, independent), `cut`, `extra_latency_ms`; `dir = both|up|down`; `duration_s`
omitted means until the end. A circle with no `loss` jams (loss 1.0), counted as cause `jam`.

```toml
[[event]]                      # the 30%-loss study
at_s = 300
duration_s = 120
class = "los"
loss = 0.30
```

## Config: `sim/links.toml`

Profiles are `[class.<name>]`, with `extends` for variants. A vehicle uses the profile named
by its platform's `link` class unless `[platform_profile]` maps its platform elsewhere
(`magura = "satcom_leo"` ships on). Shipped profiles: `los`, `mesh`, `satcom` (GEO, 300 ms
each way for the ~600 ms RTT), `satcom_leo`, `satcom_iridium`, `acoustic` (300 bps, 1500 m/s),
`tether`. Each has a comment with where its numbers come from and which are guesses.

## Observability

`GET /stats` returns a summary plus `per_link`: for each direction in/out packets and bytes,
drops by cause (`loss event_loss jam cut range terrain gate queue no_peer socket`), duplicates,
queued packets and bytes (gauges), mean and max added latency; per link the profile, blocked
cause, depth state and last position. `/stats/summary` omits `per_link`; `/stats/<uid>` is one
link; `/health`. The summary also has the process CPU seconds and RSS. A one-line log prints
every `--log-interval` seconds (rates and drops by cause since the last line).

## Tests and tools

`cargo test -p murmur-linkemu`: loss convergence, burst length, determinism, jitter and
reordering, bandwidth queuing and tail drop, duplicates, event windows and selectors, jammer
circle, range loss, depth gating, terrain hook, propagation, MAVLink parsing, and a real
loopback round trip. `src/bin/loadgen.rs` is the synthetic load generator (fake vehicles and
fake bridge ports, writes its own manifest): `loadgen --vehicles 440 --rate 8 --duration 30`.

## Not done

- Terrain line of sight (trait only). No jitter model beyond uniform; no correlated fading
  with range or weather; no per-packet-size loss. Loss does not depend on message type.
- Telemetry priority under low bandwidth (murmur-f6q): the queue is a single FIFO.
- Position comes from uplink only and lags by the link's own delay. No ICMP, MTU or
  fragmentation modelling (datagrams over 2048 bytes are truncated).
- Events use time since emulator start, not scenario or simulation time, and cannot be
  changed while running. No reload of `links.toml`.
- Socket creation is slow in some sandboxes (about 10 ms per socket in this WSL2 harness), so
  1000 links take ~20 s to start there.
