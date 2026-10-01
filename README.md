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

## Run it

```bash
make setup   # once: .venv, clone + build ArduPilot SITL in ~/ardupilot, npm install (~10 min)
make up      # swarm + bridge + web; open http://127.0.0.1:5173
```

Pick a location with `make up LOCATION=canaveral` (Cape Canaveral Skid Strip + Atlantic) or
the default `sandiego` (NAS North Island + San Diego Bay). Presets live in `sim/swarm.py`;
the map zooms to wherever the swarm is.

Vehicles appear within a few seconds and get a GPS fix about 20 to 30 seconds after start.
Logs go to `./logs`. To run the pieces separately use `make sim`, `make bridge` and `make web`
in three terminals.

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
| tracker | AntennaTracker | track, hold | **Track**: select the tracker + one vehicle; the bridge relays that vehicle's position to it. Known issue `murmur-9bf`: stalls for some bearings. |

Tasks wait in `queued` until the vehicle's EKF has a usable position (a GPS fix arrives seconds
earlier, and autopilots refuse AUTO until the EKF is ready). `make down` stops a running stack.

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
- `web/src/`: `MapView.tsx`, `FleetPanel.tsx`, `TaskPanel.tsx`, `useBridge.ts`.

REST is usable without the UI:

```bash
curl -XPOST localhost:8765/api/tasks -H 'content-type: application/json' \
  -d '{"task":"orbit","sysids":[7,8],"center":[-117.175,32.712],"radius":250}'
```

Work items are tracked in `bd` (`bd ready`).
