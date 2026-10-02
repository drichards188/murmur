# Murmur

Multi-domain swarm command console. Thirteen simulated vehicles covering every ArduPilot
firmware run in SITL: 4 fixed-wing planes and 1 VTOL QuadPlane (Plane), 2 quadcopters (Copter),
1 airship (Blimp), 3 motorboats (Rover), 1 ROV/sub (Sub) and 1 antenna tracker
(AntennaTracker). A Python bridge speaks MAVLink to each vehicle over its own
UDP link and streams JSON over a WebSocket to a React + MapLibre console with MIL-STD-2525
symbols (milsymbol).

```
 SITL x10 ──UDP 14560..14569──▶ bridge (FastAPI, pymavlink) ──/ws, /api──▶ web (Vite, React, MapLibre)
 arduplane / ardurover            :8765                                   :5173
```

## Fleets and platforms

`sim/platforms.toml` is the platform catalog: 22 types from RQ-4 Global Hawk-class (HALE, 18 km)
and MQ-9B SkyGuardian-class down to FPV quads, tracked UGVs, Magura V5-class naval drones,
a wind-powered Saildrone-class USV, drifting buoys, BlueROV2-class ROVs, REMUS 600-class
AUVs and Orca XLUUV-class. Names say "-class": they're simulated stand-ins tuned to rounded
public figures, not models of the real systems.

Platforms ArduPilot can fly run as real SITL vehicles. The rest (HALE/MALE/CCA aircraft,
large ships, AUVs, drifters) run as **stand-ins** (`sim/standins.py`): lightweight kinematic
models that speak MAVLink exactly like ArduPilot (modes, missions, acks), at ~0.1% CPU each
versus ~16% for a SITL process. Stand-in boats, buoys and subs get real currents, drift and
seabed from tidewater. Stand-ins are badged STAND-IN in the console.

Fleets (`sim/fleets/*.toml`) say which platforms, how many and where they start:
`sandiego`, `canaveral`, `golden-gate`, `full-spectrum` (30 vehicles, every type). Every
vehicle gets a callsign (HAWK 1, VIPER 2...) and the console shows callsign, model and role.

```bash
make up FLEET=canaveral
make goldengate GG_FLEET=full-spectrum AT=2026-06-14T10:00Z   # with real tide/current/wind
```

## Run it

```bash
make setup   # once: .venv, clone + build ArduPilot SITL in ~/ardupilot, npm install (~10 min)
make up      # swarm + bridge + web; open http://127.0.0.1:5173
```

**Real conditions:** `make goldengate AT=2026-06-14T10:00Z` starts the swarm at the Golden
Gate from the [tidewater](../tidewater) sibling repo's scenario, and runs its driver so each
vehicle gets the real tide current and wind at its position for that date and time (run
`tidewater fetch` there first).

Vehicles appear within a few seconds and get a GPS fix about 20 to 30 seconds after start.
Logs go to `./logs`. To run the pieces separately use `make sim`, `make bridge` and `make web`
in three terminals. `MURMUR_BRIDGE=rust make up` (or `make bridge-rs`) runs the Rust port of the
bridge instead; same API, same console.

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
| tracker | AntennaTracker | track, hold | **Track**: select the tracker + one vehicle; the bridge relays that vehicle's position to it. Needs the ArduPilot patches in `sim/patches/ardupilot/` and `~/tidewater/patches/ardupilot/`; the SITL mount then turns +/-170 deg about its spawn heading, and a target in the 20 deg gap behind it gets the nearest limit. |

Tasks wait in `queued` until the vehicle's EKF has a usable position (a GPS fix arrives seconds
earlier, and autopilots refuse AUTO until the EKF is ready). `make down` stops a running stack.

## 2D and 3D

The **2D / 3D** toggle on the map switches views; the choice is remembered. 3D tilts the
camera over real terrain and seabed (Terrarium elevation tiles, which include bathymetry) and
draws every vehicle at its true altitude or depth with a stem to sea level, 3D trails and a
translucent sea surface, so submerged vehicles show through the water. Heights are exaggerated
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
sequence gaps; reordered packets are not counted as loss) and round-trip time (`TIMESYNC`).
After 3 s of silence a track is drawn with a dashed frame and a `LOST` tag.

## Layout

- `sim/swarm.py`: launches SITL binaries directly (one UDP link per vehicle); `sim/params/` layers params on ArduPilot defaults.
- `bridge/murmur_bridge/`: `vehicle.py` (UDP link, state, link stats), `tasks.py` (tasking and lifecycle), `server.py` (REST + WebSocket).
- `crates/murmur-bridge/`: the Rust port of the bridge (same API); `scripts/parity.py` compares the two on a live fleet.
- `web/src/`: `MapView.tsx`, `FleetPanel.tsx`, `TaskPanel.tsx`, `useBridge.ts`.

REST is usable without the UI. Vehicles are addressed by `uid` (fleet-unique, from
`sim/run/fleet.json`); the MAVLink `sysid` is unique only per link and wraps above 250.

```bash
curl -XPOST localhost:8765/api/tasks -H 'content-type: application/json' \
  -d '{"task":"orbit","uids":[7,8],"center":[-117.175,32.712],"radius":250}'
```

Work items are tracked in `bd` (`bd ready`).
