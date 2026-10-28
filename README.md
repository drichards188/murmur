# Murmur

**When one operator supervises a hundred autonomous air, ground, surface and undersea vehicles,
does human judgment over the decisions that matter stay meaningful? And how would you know?**

Murmur is an open-source command-and-control testbed built to answer that with evidence instead
of assurances. AI agents and autonomy are moving into command and control, and the public case
for them is scale. Very little is said, and less is measured, about whether the person nominally
in charge still understands and decides what they approve. Murmur makes that measurable: a
realistic world, a console a person can actually run, AI watch officers that propose orders, and
a decision layer that records who decided what, on which picture, and why.

**Status: a working draft.** The testbed runs end to end. The evaluation that would answer the
question is still being validated, so there are no findings to quote yet. Expect sharp edges.

## What's in it

- **Vehicles.** 22 platform types across air, ground, surface and undersea, from HALE drones to
  AUVs. Those ArduPilot can fly run its real firmware (SITL); the rest are Rust stand-ins that
  speak MAVLink like ArduPilot, light enough for fleets of thousands.
- **A real place and time.** The Golden Gate with real tide, currents, wind, depth and terrain
  from NOAA data ([tidewater](tidewater/README.md)), and the harbor's traffic around you.
- **Links that fail.** Every vehicle's link runs through an emulator: loss, latency, terrain
  line of sight, satellite passes, acoustic modems under water, scripted jamming.
- **Sensing, not truth.** Radar, EO/IR and AIS models feed a calibrated tracker; the operator and
  the AI see tracks, with their uncertainty, never the simulator's ground truth.
- **Agents decide, they don't drive.** Every action, from a person or an AI, is an intent. A
  deterministic engine checks it fail-closed, a policy routes it to go at once or wait for a
  person, and an append-only, replayable audit log records every step. AI agents connect over
  MCP and can propose and explain; the approve step is closed to them in code.
- **The engagement chain.** A hostile declaration, a nomination, then two signatures from
  operators with engagement authority, neither of them its proposer, each re-entering a password
  or passkey at the moment they sign when sign-in is on. Keep-outs protect friendly and neutral
  traffic.
- **AI watch officers.** Claude or ChatGPT (Codex) through your own CLI and subscription (Murmur
  never sees your credentials), or a scripted playbook that needs no account.
- **Situations and evaluation.** Pre-built situations to run live or on autoplay, and a headless
  simulator for running batches of scored episodes.

What it is not: a weapon, or connected to anything real. Every platform is a simulated stand-in
tuned to rounded public figures ("-class"), and the point is to study the person, not the strike.

## Quick start

Linux or WSL2 (developed on WSL2 Ubuntu; macOS is untested). You need git,
[uv](https://docs.astral.sh/uv/), [Rust](https://rustup.rs/), Node 22.18 or later, a C++
toolchain for ArduPilot's simulator, a network connection and several GB of disk.

```bash
git clone https://github.com/drichards188/murmur && cd murmur
make setup   # once, about 30 minutes (see Run it below)
make start   # open http://localhost:5173 and pick a scenario
```

## How it fits together

```
 SITL vehicles  ──UDP 14560+i──▶ murmur-bridge (Rust) ──/ws, /api──▶ web (Vite, React, MapLibre)
 Rust stand-ins                   :8765                               :5173
 tidewater serve :8770 ──conditions──▶ bridge, stand-ins;  tidewater drive ──▶ SITL
```

## Fleets and platforms

`sim/platforms.toml` is the platform catalog: 22 types from RQ-4 Global Hawk-class (HALE, 18 km)
and MQ-9B SkyGuardian-class down to FPV quads, tracked UGVs, Magura V5-class naval drones,
a wind-powered Saildrone-class USV, drifting buoys, BlueROV2-class ROVs, REMUS 600-class
AUVs and Orca XLUUV-class. Names say "-class": they're simulated stand-ins tuned to rounded
public figures, not models of the real systems.

Platforms ArduPilot can fly run as real SITL vehicles. The rest (HALE/MALE/CCA aircraft,
large ships, AUVs, drifters) run as **stand-ins** (`crates/murmur-standins`, one Rust process
for all of them): lightweight kinematic models that speak MAVLink exactly like ArduPilot
(modes, missions, acks), at well under 0.1% CPU each versus ~16% for a SITL process. Every
platform has a stand-in behavior, so a fleet can force any unit to `sim = "standin"` (the
load-test fleets do). Stand-in boats, buoys and subs get real currents, drift and
seabed from tidewater. Stand-ins are badged STAND-IN in the console.

Fleets (`sim/fleets/*.toml`) say which platforms, how many and where they start:
`sandiego`, `canaveral`, `golden-gate`, `full-spectrum` (22 vehicles, one of each type). Every
vehicle gets a callsign (HAWK 1, VIPER 2...) and the console shows callsign, model and role.

```bash
make up FLEET=canaveral
make goldengate GG_FLEET=full-spectrum AT=2026-06-14T10:00:00Z   # with real tide/current/wind
```

## Run it

```bash
make setup   # once: .venv with tidewater and its Golden Gate data, Rust release build, ArduPilot SITL with Murmur's patches in ~/ardupilot, npm install (~30 min, a third of it the Golden Gate's model currents)
make start   # the main menu: open http://localhost:5173, pick a scenario, Start
```

The first `make setup` takes about half an hour and needs a network connection: about 19
minutes of it is downloading the Golden Gate's real conditions (NOAA's bay current model alone
is some 730 MB, kept as a 26 MB cache), the rest the ArduPilot and Rust builds. It is done once:
runs read the cache in `tidewater/data/cache/` and never download. If setup is interrupted, run
it again; it keeps what it already fetched and built.

**The main menu** is how to open Murmur. `make start` runs the launcher (`eval/launcher.py`),
which serves the console and starts one scenario at a time from its menu: the **open world**
first, the default (the Golden Gate with the full-spectrum fleet, real tide, current and wind and
the harbor's traffic: what `make goldengate` runs; you command freely, nothing is scripted or
scored), then each situation in `sim/situations/` with its brief, fleet and duration, and its
operator (you, or autoplay) and AI watch officer (Claude, Codex, Playbook or None) with the
situation's own defaults chosen. The menu shows the launch's progress (vehicles take 30 to 60 s
in the open world, seconds in a stand-in situation) and opens the console on it; the console's
**Menu** button returns to the menu, which can end the scenario (as `make down` does: nothing it
started is left running) or start another (ending the one running first). Ctrl-C in the terminal
ends the running scenario and stops the launcher. The terminal commands below still start a
stack directly; its console then shows no menu.

```bash
make up      # swarm + bridge + web (a dev run); open http://127.0.0.1:5173
```

**Who acts.** By default a run is a dev run: the console's "Acting as" switcher names the
operator in a header the bridge believes, a DEV pill says so, and the audit log records
`auth: dev`, so such a run is never evidence about people (docs/DECISIONS.md). With
`MURMUR_AUTH=credentials` operators sign in: each has a credential of their own (argon2id
hash) in `~/.config/murmur/credentials.json` (`MURMUR_CREDENTIALS` moves it; never in the
repository), managed with `murmur-auth` (`add [--engage] [--scripted]`, `passwd`, `engage`,
`scripted`, `remove`, `list`; agents: `agent-add`, `agent-rotate`, `agent-remove`):

```
target/release/murmur-auth add op-1 --engage   # once: an operator (password prompted, or on stdin)
MURMUR_AUTH=credentials make up                # open the console and sign in
```

An engagement then needs two of them with engagement authority, each re-entering their
password at the moment they sign; the evaluation's study mode always runs this way.

**Real conditions:** `make goldengate AT=2026-06-14T10:00:00Z` starts the swarm at the Golden
Gate from [tidewater](tidewater/README.md)'s scenario (`tidewater/`, installed into `.venv` by
`make setup`, which also fetches the scenario's data). `scripts/up.sh` starts
`tidewater serve` on :8770 (or reuses one already there) for the bridge and the stand-ins, and
`tidewater drive` so each SITL vehicle gets the real tide current and wind at its position for
that date and time. Both stop with the stack.

**Degraded links by default:** every vehicle's link runs through the link emulator
([`crates/murmur-linkemu`](crates/murmur-linkemu/README.md)): vehicle i sends to
`MURMUR_BASE_PORT`+i where the emulator listens, and the emulator forwards to the bridge on
`MURMUR_BRIDGE_BASE_PORT`+i (default base + 1000), degraded per link class from
`sim/links.toml`: loss, latency, bandwidth and queueing; with a scenario also terrain line of
sight to the ground station at Crissy Field (tidewater's DEM); SATCOM following real satellite
passes once `murmur-space fetch` has filled `sim/space/cache`; acoustic links only under
water (a surfaced AUV falls back to an Iridium-like link). `MURMUR_LINKEMU=0` bypasses it.
Scripted events: `make goldengate EVENTS=sim/link-events/golden-gate.toml` (30% extra loss
on every LOS link from minute 5 for two minutes, a jammer at the Gate from minute 8), or
`MURMUR_LINK_EVENTS=<file>` with `scripts/up.sh`; the file is re-read when it changes. The
emulator's stats are on `MURMUR_LINKEMU_PORT` (8766, `/stats`), and through the bridge at
`/api/links` and `/api/links/{uid}`.

Vehicles appear within a few seconds and get a GPS fix about 20 to 30 seconds after start.
Logs go to `./logs`. To run the pieces separately use `make sim`, `make bridge` and `make web`
in three terminals (no emulator then). A second stack can run beside the default one:
`scripts/up.sh` takes its ports from `MURMUR_BASE_PORT`, `MURMUR_BRIDGE_BASE_PORT`,
`MURMUR_LINKEMU_PORT`, `MURMUR_HTTP_PORT`, `MURMUR_WEB_PORT`, `MURMUR_TIDEWATER_PORT` and
`MURMUR_SITL_INSTANCE_BASE` (see the script's header).

## Operating

1. Select vehicles: click rows in the Fleet panel or symbols on the map, or use All / Air / Sea.
2. Pick a task. **Orbit**: click the map for the center and set the radius. **Patrol**: click
   waypoints (the route loops). **Hold** and **RTL** need no geometry.
3. Send. The command log tracks every vehicle through `sent → acked → active → done`.

Aircraft on the ground take off automatically as part of orbit and patrol (runway takeoff for
planes, vertical for the VTOL and copters). Vehicles of one kind sharing a task are stacked in
altitude (or depth, for subs) and spread around orbit circles.

| Kind | Firmware | Tasks | Notes |
|---|---|---|---|
| plane, vtol | ArduPlane | orbit, patrol, hold, RTL | orbit = autopilot loiter circle; VTOL detected via `Q_ENABLE` |
| copter | ArduCopter | orbit, patrol, hold, RTL | `AUTO_OPTIONS=3` so AUTO takes off without an RC stick |
| blimp | Blimp | orbit, patrol, hold, RTL | slow: ~0.3 m/s in SITL |
| boat | ArduRover (motorboat) | orbit, patrol, hold, RTL | |
| sub | ArduSub | orbit, patrol, hold, RTL | 5 m depth; no RTL mode, so RTL is a mission home |
| tracker | AntennaTracker | track, hold | **Track**: select the tracker + one vehicle; the bridge relays that vehicle's position to it. Needs the ArduPilot patches in `sim/patches/ardupilot/` and `tidewater/patches/ardupilot/` (`make setup` applies them to a fresh clone); the SITL mount then turns +/-170 deg about its spawn heading, and a target in the 20 deg gap behind it gets the nearest limit. |

Tasks wait in `queued` until the vehicle's EKF has a usable position (a GPS fix arrives seconds
earlier, and autopilots refuse AUTO until the EKF is ready). `make down` stops a running stack.

## 2D and 3D

The **2D / 3D** toggle on the map switches views; the choice is remembered. 3D tilts the
camera over real terrain and seabed (Terrarium elevation tiles, which include bathymetry) and
draws every vehicle at its true altitude or depth with a stem to sea level and 3D trails. The
water (sea, lakes, ponds and rivers) is tinted on the terrain at whatever height it lies, and the
trails and stems of submerged vehicles are drawn as seen through it. Heights are exaggerated
×2 (terrain and vehicles alike) so 80 m altitudes read at bay scale. Right-drag to orbit the
camera. Tasking works the same in both views; in 3D, clicking a vehicle selects it.

The 3D layers use deck.gl on MapLibre. MapLibre is pinned to v5 because deck.gl 9.4's adapter
relies on map internals (`map.transform`, `getFreeCameraOptions`) that MapLibre v6 removed.

## How the command lifecycle is judged

| State | Meaning | Evidence |
|---|---|---|
| sent | first message of the task is on the wire | bridge |
| acked | autopilot accepted it | `MISSION_ACK`, `COMMAND_ACK` for set-current, arm, mode |
| active | vehicle is executing it | heartbeat shows AUTO/RTL and armed |
| done | goal reached | telemetry: on the orbit ring, first patrol lap closed, near home |

An ACK only means "accepted", so `active` and `done` come from telemetry. Commands retry with
an incrementing MAVLink `confirmation` field; mission uploads follow the vehicle-driven
MAVLink mission protocol, resending on silence. Map geometry is dashed until a vehicle has
acknowledged it, solid after.

## Link health

Each vehicle row shows time since last message, packet loss over the last 10 s (from MAVLink
sequence gaps; reordered packets are not counted as loss) and round-trip time (`TIMESYNC`;
slow classes are pinged less often and waited for longer: acoustic every 30 s, answers
accepted for 5 minutes; SATCOM every 2 s, for a minute). After 3 of its expected intervals of
silence a track is drawn with a dashed frame and a `LOST` tag. When the emulator degrades a
link (recent loss of 20% or more, a block, or a scripted event acting on it) the row says so
with the cause (`degraded: jammer`, `link down: terrain blocks line of sight`). The details
card's Link section shows the emulator's view: profile and its nominal numbers, loss and added
delay over the last 10 to 60 s per direction, the emulated round trip, drops by cause, active
events, terrain clearance, depth, and for SATCOM the satellite, its elevation and latency, and
the next outage or pass.

## Layout

- `sim/swarm.py`: launches SITL binaries directly (one UDP link per vehicle) and the stand-in engine; `sim/params/` layers params on ArduPilot defaults.
- `crates/murmur-bridge/`: the bridge: UDP links and link stats, tasking and lifecycle, the decision layer (rules, policy, audit log, inbox), REST + WebSocket.
- `crates/murmur-standins/`: the stand-in engine, one behavior per imitated ArduPilot autopilot.
- `tidewater/`: real-world conditions (land/water, depth, tide, current, wind) for a place and time, served to the stack and driven into SITL ([its README](tidewater/README.md)).
- `scripts/`: `setup.sh` (once), `up.sh` (the stack), `ardupilot-patches.sh` (the ArduPilot patches, in order), `measure.py` (load numbers), `scale_fleet.py` (load-test fleets).
- `crates/murmur-decide/`: the decision engine (intents, checks, policy, engagements, audit log) and `murmur-replay`, which re-derives every decision from a log.
- `crates/murmur-linkemu/`, `murmur-contacts/`, `murmur-space/`, `murmur-auth/`, `murmur-headless/`: the link emulator, the traffic and sensing world, satellites, operator credentials and passkeys, and the headless simulator.
- `agents/`: the AI watch officers and the scripted playbook, over MCP ([agents/README.md](agents/README.md)).
- `eval/`: the launcher, situations and the evaluation harness ([eval/README.md](eval/README.md)).
- `web/`: the console (React, MapLibre, deck.gl, MIL-STD-2525 symbols).
- `docs/`: the plan ([PLAN.md](docs/PLAN.md)) and what was decided and why ([DECISIONS.md](docs/DECISIONS.md)).

REST is usable without the UI. Vehicles are addressed by `uid` (fleet-unique, from
`sim/run/fleet.json`); the MAVLink `sysid` is unique only per link and wraps above 250.

```bash
curl -XPOST localhost:8765/api/tasks -H 'content-type: application/json' \
  -d '{"task":"orbit","uids":[7,8],"center":[-117.175,32.712],"radius":250}'
```

Work items are tracked in `bd` (`bd ready`).

## License

Murmur is free software under the [GNU Affero General Public License v3.0](LICENSE) or later:
use it, study it and extend it, and anyone who runs a modified version for others, over a
network included, must offer them its source under the same terms. The ArduPilot patches in
`sim/patches/ardupilot/` and `tidewater/patches/ardupilot/` modify ArduPilot and are under its
license, GPL-3.0.
