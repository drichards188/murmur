# Murmur

Mixed air and sea swarm command console. Six ArduPilot fixed-wing planes and four motorboats
run in SITL over San Diego Bay. A Python bridge speaks MAVLink to each vehicle over its own
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

Vehicles appear within a few seconds and get a GPS fix about 20 to 30 seconds after start.
Logs go to `./logs`. To run the pieces separately use `make sim`, `make bridge` and `make web`
in three terminals.

## Operating

1. Select vehicles: click rows in the Fleet panel or symbols on the map, or use All / Air / Sea.
2. Pick a task. **Orbit**: click the map for the center and set the radius. **Patrol**: click
   waypoints (the route loops). **Hold** and **RTL** need no geometry.
3. Send. The command log tracks every vehicle through `sent → acked → active → done`.

Planes on the ground take off automatically as part of orbit and patrol. Planes in the same
task are stacked 15 m apart in altitude; boats sharing an orbit spread out around the circle.

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
