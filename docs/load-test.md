# Load test: one operator console, 100 nodes

Fleet `scale-100` (deliberately many copies; load testing only): 20 ArduPilot SITL vehicles
plus 80 stand-ins, Golden Gate scenario with tidewater conditions. Measured with
`scripts/measure.py --seconds 30` on a 28-core WSL2 machine, 2026-10-02.

| | 30 nodes, before | 30 nodes, after | **100 nodes, after** |
|---|---|---|---|
| Bridge CPU | 12.7% of one core | 3.0% | **5.6%** |
| Telemetry received | 2,235 msg/s (median 94/vehicle) | 483 msg/s (median 9.1) | **1,088 msg/s (median 8.7)** |
| Bridge -> browser | 630 KB/s, full 134 KB state 4.7x/s | 57 KB/s | **167 KB/s** |
| Stand-in process (80 vehicles) | | | 10% of one core total |

What changed:

1. **Telemetry plan.** The bridge used `REQUEST_DATA_STREAM ALL @ 4 Hz`. It now asks for the
   seven messages the console uses with `SET_MESSAGE_INTERVAL` (position 2 Hz for aircraft,
   1 Hz otherwise; the rest at 0.5 to 1 Hz), and `sim/params/common.parm` zeroes ArduPilot's
   default stream groups (`MAV1_*`). ArduPlane still sends ~22 extra msg/s each (open bug).
2. **Change-only updates.** The browser gets the full state once, then only vehicles that
   changed, only new trail points, and the task list only when it changes. Link age is
   computed in the browser from the last-packet time, so idle vehicles don't count as changed.

Browser, 100 nodes, measured in a headless Chromium that renders WebGL in software
(SwiftShader):

- 2D: 57 fps, no long tasks.
- 3D: 2 fps, but terrain alone gives 2-3 fps in that renderer with or without vehicles, so
  this measures the test setup, not Murmur. Check on a real GPU with `?perf` in the URL.

## Rust bridge, and past 255 vehicles (2026-10-02)

Same machine and method (`scripts/measure.py --seconds 30`, one WebSocket client), both bridges
on the same fleet, measured back to back. `scale-300` is 300 stand-ins, so MAVLink sysids wrap
(1..250, then 1 again) and only the uid tells vehicles apart; before uids the bridge crashed
above 255.

| | scale-100, Python | scale-100, Rust | scale-300, Python | scale-300, Rust |
|---|---|---|---|---|
| Bridge CPU | 3.1% of one core | **1.2%** | 6.1% | **2.7%** |
| Telemetry received | 1,087 msg/s | 1,087 msg/s | 2,613 msg/s | 2,618 msg/s |
| Bridge -> browser | 175 KB/s | 149 KB/s | 519 KB/s | 614 KB/s |
| Initial full state | 184 KB | 185 KB | 355 KB | 345 KB |

The Python column for scale-100 reads lower than the 5.6% above: same code path, a quieter
machine that day. Browser bytes vary run to run with how many stand-ins moved between two
pushes (the JSON per vehicle is identical, about 0.5 KB, mostly the identity); at 300 moving
stand-ins nearly every vehicle changes every push. A task to uid 251 (sysid 1) moved only that
vehicle, not uid 1 (also sysid 1).

## Stand-in engine alone: Rust vs Python (R.1, 2026-10-02)

Each vehicle sends to a dummy UDP sink (no bridge); 7 stand-in platforms round robin, planes
airborne and loitering, marine idle. CPU is percent of one core on a shared host (approximate).
`sim/tests/standins_scale.py`.

| Engine | Vehicles | CPU | RSS | Delivered msg/s (expected) |
|---|---|---|---|---|
| Rust (1 worker thread) | 1,000 | 3.8% | 8.3 MB | 7,500 (7,500) |
| Rust (1 worker thread) | 3,000 | 8.4% | 17.3 MB | 22,500 (22,500) |
| Python | 1,000 | 7.1 to 8.2% | 59 MB | 7,500 (7,500) |
| Python | 3,000 | 21.9% | 86 MB | 22,500 (22,500) |

Host limits for "thousands of nodes": `net.ipv4.ip_local_port_range` (here 39160 to 43255,
about 4,000 sockets per address) caps either engine near 4,000 vehicles unless widened; ticks
are staggered across vehicles because lockstep sends lost ~20% of packets on WSL loopback.

## Phase R exit: 1,000 stand-ins, Rust only (R.5, 2026-10-02)

The whole live path in Rust (`murmur-bridge`, `murmur-standins`), started by `scripts/up.sh` on
alternate ports beside another running stack. Fleets from `scripts/scale_fleet.py N`: a fixed
mix of all 22 platform types (per 1,000: 440 air, 140 ground, 280 surface, 140 subsurface),
every unit forced to `sim = "standin"`, in units of 10 (30 at 3,000) spread over the Bay area:
HALE/MALE/CCA on station over the whole area, small aircraft and ground vehicles on land around
the Bay, boats in the Bay and offshore, AUVs offshore. `sim/fleets/scale-1000.toml` is
committed; 22, 300 and 3,000 were generated into `sim/run/`. No tidewater (marine and ground
stand-ins run without currents and land checks), no tasks: every vehicle idle on station or at
home, the bridge's telemetry plan applied to all. Measured with `scripts/measure.py --seconds
30` (one WebSocket client), two windows per size after everything was nav-ready plus 20 s;
ranges are the two windows. CPU is percent of one core on a shared 28-core WSL2 host (other
agents' SITL vehicles running; load average 2 to 10), so treat it as approximate.

| | 22 | 300 | **1,000** | 3,000 |
|---|---|---|---|---|
| All nav-ready after start | not recorded | 13 s | **17 to 35 s** | 159 s |
| Bridge CPU | 0.4% | 3.2 to 3.7% | **9.1 to 13.1%** | 23.4 to 29.2% |
| Bridge RSS | 49 MB | 32 to 34 MB | **62 to 72 MB** | 314 to 327 MB |
| Stand-in engine CPU (one process) | 0.7% | 3.3 to 3.4% | **7.1 to 8.8%** | 13.9 to 17.1% |
| Stand-in engine RSS | 3.7 MB | 5.2 MB | **10.6 MB** | 24.6 MB |
| Telemetry received | 180 msg/s | 2,460 msg/s | **8,100 to 8,180 msg/s** | 21,900 to 24,500 msg/s |
| ... median per vehicle | 7.8 | 7.7 | **7.7 to 8.0** | 7.2 to 7.7 |
| WebSocket to one client | 39 KB/s | 445 KB/s | **1.5 to 1.9 MB/s** | 5.0 to 5.5 MB/s |
| Initial full state | 34 KB | 334 to 489 KB | **1.1 to 1.7 MB** | 7.2 to 8.6 MB |
| WS pushes | 4/s | 4/s | 4/s | 4/s |

Nothing was dropped: the engine logged 0 send errors, the bridge no errors, and the
per-vehicle rate matches the plan (heartbeat plus position at 2 Hz for aircraft, 1 Hz
otherwise, the rest at 0.5 to 1 Hz). The bridge's RSS and the initial state grow while it runs
(trails fill up), which is why the second window reads higher. The slow start at 3,000 came in
steps of a few hundred vehicles every ~30 s; not investigated.

Console, opened at `?perf` in Playwright's headless Chromium (2D view, the default):

| | 22 | 300 | 1,000 | 3,000 |
|---|---|---|---|---|
| 2D frame rate (20 s) | 16 fps | 10.5 fps | 9 fps | crashed |
| Long tasks (share of the 20 s) | 92% | 82% | 86% | |
| Map markers / DOM elements | 22 / 847 | 300 / 8,449 | 1,000 / 27,404 | |
| JS heap | 72 MB | 83 MB | 160 MB | 168 MB |

Caveats: this Chromium renders WebGL in software (SwiftShader), and the 2D basemap is WebGL
too (MapLibre), so the main thread is saturated even at 22 nodes (16 fps, against 57 fps for
100 nodes measured the same way earlier on a quieter host). The fps here mostly measures the
renderer and the host, not the console; the number to trust is a real GPU with `?perf`. What
does scale with the fleet is the DOM: one HTML marker per vehicle and an unvirtualized fleet
list (about 27 elements per vehicle), 27,000 elements at 1,000 nodes. At 3,000 the software
WebGL context was lost after about 90 s and MapView threw (`getProjection` of null in deck.gl's
overlay) with no error boundary, so the whole page went blank. Both are console work for a
real GPU check: a virtualized fleet list and canvas symbols at this scale, and surviving a
lost WebGL context.

### The node ceiling on this host

Each stand-in has its own UDP socket on an ephemeral port, and `net.ipv4.ip_local_port_range`
here is 39160 to 43255: 4,096 ports for every ephemeral socket on the host. With 3,000
stand-ins running, 3,023 of them were in use, leaving about 1,070. The bridge binds one fixed
port per vehicle from `MURMUR_BASE_PORT` (outside that range, so it doesn't compete as long as
`base + N` stays below 39160). So the practical ceiling was about **4,000 stand-ins per host**
without widening the range (not changed here; `sysctl net.ipv4.ip_local_port_range` would, or
stand-ins could bind fixed ports like the bridge does; since murmur-z07i they can, and the
emulator takes no ephemeral port: Saturation, below). CPU and memory are far from limits:
at 3,000 the bridge used under a third of one core and the engine a sixth. The next limit
after ports is the browser feed, 5 MB/s and an 8 MB initial state at 3,000, which would need
viewport filtering or level of detail before 10,000 nodes.

## Console at 1,000 and 3,000 nodes: GPU vehicles, windowed list (murmur-ica, murmur-s8w)

Vehicles are now deck.gl layers in both views (`web/src/vehicleLayers.ts`): one icon atlas of
bare 2525 frames shared by the fleet (one entry per symbol code and stale state, a few dozen
in all), a single rotated heading arrow, the selection ring, and callsign and model text only
for the decluttered labelled set. Layer data is rebuilt when the vehicles, the selection, the
view or the declutter change, never per frame. The fleet list mounts only the rows in view.
A lost WebGL context no longer blanks the page: the overlay is detached, a banner shows, and
on restore the map is rebuilt on a fresh canvas with the same camera (the globe inset too);
every panel and the map sit in their own error boundary.

Measured with `?perf` (`web/src/perf.ts`) on production builds (`vite build`, `vite preview`),
before and after on the same running fleet, 2D, 30 to 40 s windows after 25 to 30 s of
warm-up, headless Chromium with software WebGL, 2026-10-02. "Render" is the main-thread work
one bridge update causes (React render and commit plus the map and list effects); "latency" is
from the message to the end of that work, waits included. Fleets from `scripts/scale_fleet.py`,
stand-ins only, no tidewater.

| | 1,000 before | 1,000 after | 3,000 before | 3,000 after |
|---|---|---|---|---|
| DOM elements | 27,415 | **787** | 81,995 | **787** |
| JS heap (after a forced GC at 3,000) | 220 MB | 165 MB | 133 MB | **67 MB** |
| Merge one update (applyDelta) | 1.1 ms | 0.6 ms | 3.5 ms | 2.8 to 4.0 ms |
| Render per update, mean (p95) | 38.8 ms (64) | **7.6 ms (12.5)** | 112 ms (208) | **18.6 ms (29 to 32)** |
| Latency per update, mean | 262 ms | 163 ms | 423 ms | 73 to 143 ms |
| Updates handled in the window | 44 / 30 s | 93 / 30 s | 26 / 40 s | 32 to 53 / 40 s |

The DOM is now constant in the fleet size (the windowed list keeps about 28 rows mounted).
Render time per update falls about 5x and the console keeps up with the bridge at 1,000.
Frame rate, frame time and long tasks are not in the table: in this renderer they measure
SwiftShader (each frame of map tiles plus 3,000 icons is drawn on the CPU, and several other
consoles shared the same software GPU process during the run), not Murmur. Heap readings
without a forced GC swing by 2x run to run and include other same-site tabs in the process.
On a real GPU, open the console with `?perf`: the overlay shows fps, p95 frame time, DOM size,
heap, merge and render time per update, latency and long tasks; `window.murmurPerfStats()`
returns the same numbers and `murmurPerfReset()` starts a new window.

## Bridge feed by view: level of detail, trail sampling, clusters (murmur-and)

The feed to the console was the whole fleet at full detail four times a second, whatever the
console showed: 5 to 6 MB/s and an initial state of 4 to 9 MB at 3,000 nodes. The console
now connects to `/ws?lod=1` and sends what it looks at (`{"type":"view","bounds":[w,s,e,n],
"zoom":z,"pin":[uid,...]}`, four times a second while the camera moves, and whenever the
pinned set changes); the bridge (`crates/murmur-bridge/src/push.rs`) answers per client:

- **Full detail** for vehicles in the view grown by a quarter on each side, and for the pinned
  ones (selected, followed, named by a pending intent or an alert), at 4 Hz as before, with
  the trail sampled to the zoom: every point at zoom 12.5 and above, every 2nd at 12, 4th at
  11, 8th at 10, 16th below (about a pixel of movement for 20 m/s). Points are chosen by their
  index in the whole trail ever recorded, so the sampling is the same whether a trail goes out
  whole (a vehicle newly in view, or a new step: `trail`) or by increments (`trail_add`).
- **Lite** for the rest, once a second: identity (sent once, then only when it changes),
  position to 4 decimals (11 m), altitude, mode, speed, nav state, and the link as lost or not
  (the bridge's own rule, so a live link is not a change every second), loss in 10% bands and
  the emulator's state; no last-packet time until the link is lost, no round trip (every
  TIMESYNC sample lands somewhere else, which made nearly every lite snapshot a change every
  second), no telemetry plan, no STATUSTEXT. The fleet list shows "live" and "–" where it
  showed the exact age and RTT for a vehicle out of view; its card (full detail) has them.
- Each full update is the part of the snapshot that changes with every report (position,
  attitude, speeds, the link's packet time, loss and round trip, and any field added later),
  the part that seldom does (mode, nav state, home, STATUSTEXT, the link's plan and emulator
  state), or both, diffed apart: a moving vehicle costs about 250 bytes an update instead of
  650, and the console merges the link's two halves.
- **Clusters** when the view holds more than `MURMUR_LOD_MAX_FULL` (800) vehicles: none of
  them is full (the pinned ones still are) and the delta carries counts per Web Mercator tile
  (90 to 180 px wide at the client's zoom) by domain, lost links and track affiliation, with
  each tile's centroid. The console draws one marker per tile (`web/src/clusterLayers.ts`);
  a click zooms in two levels on it. The cells cover the view grown by half on each side (252
  of the 488 tiles the 3,800 fleet spans at a city zoom), so the payload is bounded by the
  screen, and a view change gets them at once.
- Every update is a partial object the console merges over what it has (`useBridge.ts`), so
  the 300 bytes of identity are no longer repeated in every update of every vehicle.

A fleet of `MURMUR_LOD_MIN_FLEET` (100) vehicles or fewer gets the plain feed through either
URL, so a small fleet behaves exactly as before (every vehicle full, every trail point, no
clusters). `/ws` without the parameter is the old feed for any client that does not subscribe
(`scripts/measure.py` without `--lod`; `/api/state` is unchanged). The per-client work is one
pass over the frame with a bounds test and a string compare per vehicle; the frame itself
(every snapshot serialized once per tick, now also as its lite form) is shared.

Measured 2026-10-04 on the same 28-core WSL2 host, fleets from `scripts/scale_fleet.py`,
stand-ins only, **no link emulator** (`MURMUR_LINKEMU=0`, as the R.5 run above: with the
emulator on, most of a Bay-wide fleet is out of radio range of the one ground station, and the
emulator's own sockets would exhaust the ephemeral ports before 3,000; a 1,000-node run with
it on read 1.5 MB/s plain and 110 to 120 KB/s lod). `scripts/measure.py --seconds 30`, one
client; `--lod` sends a view: fleet-wide is the whole Bay at zoom 9.3 (cluster mode), zoomed
is San Francisco at zoom 12.6 (what the console opens at, 38 by 41 km on this screen), none
is a client with no map yet (everything lite). Bridge CPU is percent of one core on a shared
host, approximate.

| | 1,000 plain | **1,000 lod** | 3,000 plain | **3,000 lod** | 3,800 plain | **3,800 lod** |
|---|---|---|---|---|---|---|
| WebSocket to one client, fleet-wide view (clusters) | 1.25 to 2.1 MB/s | **26 KB/s** | 4.8 to 6.4 MB/s | **111 to 207 KB/s** | 7.1 to 8.1 MB/s | **224 to 289 KB/s** |
| ... zoomed to San Francisco (480 full at 1,000, clusters above) | 1.25 to 2.1 MB/s | **390 KB/s** | 4.8 to 6.4 MB/s | **161 to 200 KB/s** | 7.1 to 8.1 MB/s | **298 to 335 KB/s** |
| ... no view yet (all lite) | | 28 KB/s | | 144 to 156 KB/s | | 231 KB/s |
| Initial state | 1.3 to 1.7 MB | **528 KB** | 3.8 to 4.2 MB | **1.6 MB** | 4.7 to 10.5 MB | **2.0 MB** |
| Bridge CPU | 7.4 to 8.7% | 8.1 to 9.2% | 26 to 28% | 22 to 28% | 32 to 35% | 28 to 36% |
| Bridge RSS | 78 to 150 MB | 157 to 177 MB | 314 to 339 MB | 346 to 424 MB | 425 to 548 MB | 415 to 530 MB |
| All nav-ready after start | 18 s | | 43 to 66 s | | 43 s | |

The 3,000 figures are from two runs, before and after the fast/slow split below (which raised
the full-detail cap from 300 to 800: at 300 the zoomed view at 3,000 was still clusters). Run
to run, the lite stream swings with how many loss bands flip (3,000 stand-ins on the WSL
loopback drop packets under load, the fleet rows read 10 to 40% loss).

The initial state is now mostly identity (about 300 bytes a vehicle, once; the plain feed's
grows with the trails, 10.5 MB at 3,800 after a few minutes). A full update is 250 bytes (the
fast part); a lite update is about 250 bytes too, but goes out only for the vehicles whose
lite snapshot changed in the last second (8% of the fleet at 1,000, where the air vehicles
loiter and the rest sit still; more at 3,000 and above, where the host's loopback drops
packets under load and loss bands flip). What the zoomed view costs is the full-detail set it
holds: the San Francisco box (38 by 41 km, grown by a quarter) holds 480 of the 1,000 (the
fleet's land and bay units sit around the city), so 390 KB/s; at 3,000 and 3,800 the same box
holds more than the cap and is clustered, lite plus the cells (the console's own map is
narrower than the box: at 3,000 it drew 330 symbols there, at 3,800 about 400). Bridge CPU is
the frame and the MAVLink decoding, not the per-client diff.

Console, `?perf` on the dev build in Playwright's headless Chromium (software WebGL, 1905 by
2053 px viewport), before (master's console on the plain feed, served against the same
bridge) and after, each at the fleet-wide view the console opens at (the fit to the fleet,
zoom 8.3 to 8.9) and zoomed to San Francisco at 12.6, 30 s windows after 20 to 30 s of
warm-up (the before fleet-wide window ran 118 s). "Render" as above: the main-thread work one
update causes.

| 1,000 nodes | before, fleet-wide | **after, fleet-wide** | before, zoomed | **after, zoomed** |
|---|---|---|---|---|
| Render per update, mean (p95) | 113 ms (248) | **15.7 ms (23)** | 126 ms (224) | **20.9 ms (37)** |
| Merge one update (applyDelta) | 0.8 ms | 0.14 ms | 0.8 ms | 0.26 ms |
| Latency per update, mean | 179 ms | 159 ms | 228 ms | 179 ms |
| JS heap (no forced GC) | 298 MB | 142 MB | 154 MB | 103 MB |
| DOM elements | 792 | 792 | 792 | 792 |

| 3,000 nodes | before, fleet-wide | **after, fleet-wide** | before, zoomed | **after, zoomed** |
|---|---|---|---|---|
| Render per update, mean (p95) | 660 ms (949) | **24.7 ms (42)** | 689 ms (1,050) | **47.3 ms (94)** |
| Merge one update (applyDelta) | 3.8 ms | 0.41 ms | 3.5 ms | 0.82 ms |
| Latency per update, mean | 771 ms | 181 ms | 796 ms | 186 ms |
| JS heap (no forced GC) | 477 MB | 149 MB | 749 MB | 193 MB |

| 3,800 nodes | **after, fleet-wide** | **after, zoomed** |
|---|---|---|
| Render per update, mean (p95) | **28.1 ms (46)** | **57.5 ms (95)** |
| Merge one update (applyDelta) | 0.73 ms | 1.0 ms |
| Latency per update, mean | 136 ms | 185 ms |
| JS heap (no forced GC) | 164 MB | 447 MB |

The 3,000 "before" console (master, on the plain feed at 4.8 to 6.4 MB/s) takes 0.7 s of main
thread per update and falls behind the bridge; the "after" console keeps up at every size
measured. The zoomed figure after is the view's 330 (3,000) or 400 (3,800) symbols with
labels and trails, the fleet-wide one the cluster markers.

What remains per update on the console is React work over the whole vehicle array (the top
bar's domain counts, the fleet list's layout, the task panel's map by uid), which still scales
with the fleet; the map's share (layers, declutter, trails) is now bounded by what is in view.
Screenshots: `.playwright-mcp/and/` (small fleet unchanged; 1,000 and 3,000 as clusters and
zoomed in).

## Console with the camera moving, on a real GPU (2026-10-31)

Windows Chrome 156 (ANGLE on D3D11, an RTX 4070, a 60 Hz display at 150 %), driven over CDP
from WSL; production build of the console; stand-ins only, no link emulator, no tidewater, no
contacts (so no weather layer). One lap per row: the same 32 s path of eases over the Bay (pan
14 km at zoom 12.6, out to the whole Bay and its clusters, back to the Golden Gate, orbit 120
degrees, tilt 55 to 72 to 40), measured from every animation frame. The harness
(`scripts/console_stress.py`) was not yet in the tree when this was written. A trace at 3,000
nodes found the main thread, not the GPU, the limit; on this evening the host ran the
unchanged console faster than in that trace (56 fps, not 45), so "before" is this evening's.

| moving lap | fps | p95 / p99 frame | frames > 50 / > 100 ms | update latency mean (p95) | GPU util mean | GPU process CPU |
|---|---|---|---|---|---|---|
| 3,000, before (two laps) | 55.5 to 56.2 | 16.8 / 83 ms | 20 / 12 to 14 | 9 ms (30) | 35 % | 42 to 44 % |
| 3,000, round 1 | 59.5 | 16.8 / 17 ms | 1 / 1 | 3 ms (10) | 35 % | 21 % |
| 3,600, round 1 | 59.5 | 16.9 / 21 ms | 2 / 0 | 4 ms (13) | 36 % | 22 % |
| no vehicles, round 1 | 59.9 | 16.8 / 17 ms | 1 / 0 | n/a | 44 % | 20 % |
| 3,000, round 2 | 60.0 | 16.8 / 17 ms | 0 / 0 | 3 ms (11) | 35 % | 22 % |
| 3,600, round 2 | 59.4 | 16.8 / 20 ms | 4 / 0 | 3 ms (12) | 36 % | 22 % |
| no vehicles, round 2 | 60.0 | 16.8 / 17 ms | 0 / 0 | n/a | 36 % | 19 % |

Round 1 removed work nothing on screen needed: the bridge decks were re-lifted throughout a
move (each vertex's terrain query recomputed the tiles covering the view, in 100 to 300 ms
chunks: the hitches), the scale bar read the terrain back from the GPU on every move event,
and the labels' four font settings evicted each other from deck.gl's three-entry atlas cache.
Round 2 draws the map at a device pixel ratio of 1 while the camera moves (1166 x 999 instead
of 1749 x 1498 pixels here), back to 1.5 once it is still, and throttles the view feed of a
camera moved by `jumpTo` every frame to the view timer's 4 a second: 36 views in 10 s of such
a camera instead of 304, and the bridge's deltas back to its 7.6 a second from 34. The lap is
now at the display's 60 Hz at every size measured, with no frame over 100 ms; the pitch cap
considered next (85 to 75 degrees) was not needed. At a still camera the bridge decks, labels
and map are the same as before; the scale bar there sits where 500 m and 1 km meet, and shows
either, before and after, as its terrain read-back varies by about 1 %.

## Saturation: 3,602 drones against 507 contacts, link emulator on (murmur-z07i, 2026-11-01)

What was run: Saturation's fleet (`sim/fleets/swarm-saturation.toml`, 37 swarms: 1,700 X10-class
quadcopters, 1,300 Puma-class fixed wings, 200 Saildrones, 300 Corsair-class USVs, 100 REMUS AUVs,
2 armed MQ-9Bs, every one a stand-in) against its threat (`sim/scenarios/swarm-saturation.toml`:
500 `small_drone` contacts in five swarms and 7 harbour contacts, the physical sensing model with
no terrain grid), tidewater conditions, the link emulator with the situation's jammer, in the
situation's port block (vehicles UDP 16000+, bridge 29000+, stand-in sources 10000+, since moved
to 5100+). The same 28-core, 12 GB WSL2 host as above, under mirrored networking, shared with
other agents' work all afternoon (see the last part). Bridge loop figures are the bridge's own
(`/api/stats` `loops`, `crates/murmur-bridge/src/perf.rs`): runs, mean and worst busy time and
the longest gap between two starts, per minute. CPU is percent of one core over 60 s.

**Ports.** Each stand-in had an ephemeral UDP port and each emulator link a second one: 7,200
of the host's 4,096 for 3,600 vehicles. Now (`MURMUR_STANDIN_SRC_BASE`; the emulator answers the
bridge from its listening port) `ss` showed 3,602 stand-in sockets on their fixed ports, 3,602
emulator and 3,602 bridge sockets, and no ephemeral one (one, while Windows held UDP 10010). A
UDP bind takes about 11 ms here (mirrored networking, sequential or not), so the emulator binds
its 3,602 ports in 38 to 85 s and the bridge serves 2.5 to 3.5 minutes after `up.sh` starts.
Stand-in sockets bound to 0.0.0.0 lost every reply past about 1,700 of them; bound to the
loopback address (what connect's autobind takes) all answer. In the run with no orders, 3,542
of 3,602 were navigation-ready 24 s after the bridge served and 3,543 at 113 s; the rest stayed behind terrain
from the ground station (X10s on the ground at their launch fields, low Pumas): the emulator's
terrain rule, not a fault. (Run on tidewater's terrain from before murmur-cbjn's fix, squeezed
north-south toward the box's middle; which vehicles stand behind terrain may differ now.)

**Loopback.** 127.0.0.1 is routed through `loopback0`, a virtual NIC to Windows and back. With
the stack's links there (and no other stack up), its queues dropped about 87,000 of 7.9 million
packets in 6 minutes, TCP retransmitted 3.7 segments a second, a loopback HTTP request stalled up
to 10.5 s in a 30 s probe, and truth messages reached the bridge up to 15 s apart. With
`MURMUR_LINK_HOST=127.0.0.2` (on `lo`): no drops, no retransmits, truth at most 1.15 s apart.
With another 3,600-node stack on 127.0.0.1 beside it, `Udp: SndbufErrors` ran at 18,000 a second.
Every figure below is on 127.0.0.2.

**Emulator.** Its SATCOM refresh propagated each constellation once per terminal: at a Bay-wide
3,600 fleet (1,368 SATCOM terminals) it ran at 190 to 370 % continuously and its stats endpoint
timed out. A round now propagates each constellation once for all its terminals: 22 to 24 %.
At Saturation (502 SATCOM terminals, 44,000 to 48,000 packets a second) it averages 48 to 90 %,
with a burst of about 5 s at 440 % once a minute: each terminal's 30-minute outage outlook,
all due in the same round.

**Bridge, fleet idle on station** (no orders), 13 minutes:

| | no console | one console client (lod feed, Golden Gate view) |
|---|---|---|
| Sensing step (`sense`), per truth message | 99 to 117 ms mean, worst 136 to 176 | 112 ms, worst 151 |
| Truth messages | 60 a minute, gap at most 1.16 s | 60 a minute |
| Tracker fold / tick | 1.5 to 4.3 ms / 0.0 | 17 to 22 ms / 27 to 37 (worst 72) |
| Decision tick | 4.5 ms at start, 47 to 84 at 13 min (alerts grow to 2,000), worst 125 | 52 to 65 ms, worst 112 |
| Push frame | | 36 to 39 ms, worst 73, 217 to 239 a minute |
| To the client | | deltas 4.00 a second (longest gap 0.35 s), 1.8 MB/s |
| Tracks message / decisions message | | 20 to 30 ms (worst 71) / 17 to 19 ms |
| `/api/swarms` | 3 ms to make, 1.5 to 4.6 ms a request, 30 KB for 37 swarms | |
| CPU | emulator 90 %, bridge 35 %, stand-ins 18 %, contacts 0.1 % | bridge 27 % |

Before the console's tracks message was capped (16 sources and 16 looks a track, with the
totals): a swarm's track was seen by up to 1,918 sensors, each message was 5.3 MB, the client
took 7.3 MB/s, its deltas fell to 1.6 a second with 30 s gaps, and the fold, the tracker's tick
and the decision tick averaged 179, 169 and 380 ms (worst 349, 294, 596).

**Memory.** With glibc's default malloc arenas the bridge held 3.1 GB at 7 minutes and 3.4 GB at
13; with two (`MALLOC_ARENA_MAX=2`, now `up.sh`'s default for the bridge) 0.73 GB at 5 and 0.81
GB at 11. The loops ran slower in the two-arena run (the decision tick with a client 204 to 233
ms against 52 to 65), but the host was also busier then; on this host memory, not CPU, is what
ran out (below).

**The tracker against the swarms.** Association is by the simulation's contact id (the stated
simplification in `tracks/mod.rs`), so there were 500 air tracks for 500 quadcopters and 507
for 507 contacts: no track swaps or merges can happen. The density shows as position error: an
active air track was a median 211 to 316 m from the nearest quadcopter, 90 % within 1.47 km, its
own 1-sigma a median 2.65 km; at one moment 28 of the 500 air tracks were stale.

**The situation end to end** (its runner, autoplay with the playbook AI, on a merge of the
situation's branch). The 36 opening orders went out within about two minutes of the fleet being
ready. Some were refused (one member's navigation status 52 s old refuses its swarm's whole
100-vehicle order; the runner retries a stale one) and some failed (one member's failsafe RTL
ends its swarm's order). While they went out the decision tick reached 1.6 to 3.2 s for a minute
or two (an fsynced anchor on every op state; one a tick since murmur-fqid, measured headless in
HEADLESS.md 17.6, not live yet), then averaged 120 ms; with the fleet spread over its rings the sensing step averaged 340
to 490 ms (worst 700) and the truth handler 550 ms (worst 1.2 s). The bridge used 1 to 5 cores:
the loops above are about one; the rest is the 3,602 links' packets (44,000 a second) and 3,600
orders' watchers. The shooter's act at 10:00 was observed by about 40 of SURFACE-3's cameras and
the rule declared its track hostile 0.7 s later. No `vehicle.rebooted` in three runs.

**Where it fell over.** Eight minutes into one run the host went into swap (9.6 GB in use, about
100,000 pages a second in and out; other agents' five Chromium GPU processes at 2 to 3.4 cores
each and node jobs with 6 GB heaps; load 55 with this stack stopped). The bridge's GCS heartbeat
thread, which runs on its own, went 6.8 s between rounds; the vehicles' GCS failsafes fired
(failsafe events from 4,700 to 12,000 in a minute), 23 orders failed, the inbox held 2,217
alerts and the decision tick took 42 s. That is the collapse seen in the situation's own full
run (load about 60). On a host that does not swap, the numbers above hold real time with the
sensing step, at about half a core, the largest single-threaded cost; it is the one to
parallelise next (a shard of state per observer, merged in observer order: done, "The sensing
step on several threads" below).

**Spurious reboots.** None here. The likely mechanism behind the 3,907 `vehicle.rebooted` of
the situation's full run, on the same swapping host: a stand-in engine stalled for more than a second skips the ticks it
missed, so its time since boot fell behind the bridge's clock by every stall, and once that
passed 30 s the bridge read a reboot (`BOOT_JUMP_S`) and ended the vehicle's order. A stand-in's
time since boot now counts the skipped ticks.

## The sensing step on several threads (murmur-9730, 2026-11-10)

**What changed.** The physical sensing step (`crates/murmur-bridge/src/tracks/physical/mod.rs`)
runs each of its two passes (the scanning and listening sensors; then the cameras they cue) over
every vehicle at once on a pool of threads of its own (`physical/par.rs`, rayon) and takes the
detections back in observer order. Each vehicle's state across ticks (its lines of sight, and per
sensor its runs of hits, its sonar evidence, its camera's pointing, looks and history) is its own
(`Own`, one per uid; a uid named twice is one part, its observers in order): the step moves it out
of the engine for the vehicle's part and back. What every vehicle reads (the targets, the terrain
and bathymetry grids, the weather, the cues, the identify orders' contacts) is shared by
reference, never copied: the prototype (0c275bb, never landed) cloned every cue for every observer
each tick, about 1.8 million copies at this size. The cues are still written only between the two
passes, from the detections in observer order; the vehicles' part of forgetting stale state runs
on the pool too. One thread or sixteen, the step gives the same detections in the same order and
leaves the same state; the thread count changes the time only.

**Threads.** `MURMUR_SENSE_THREADS`; without it, a quarter of the host's cores, at most 8, in the
live bridge (7 here) and one in the headless simulator; the bridge says which at start
(`sensing: the physical step on 7 threads (the default; ...)`). Why those: the bridge is one
process of a stack that runs the link emulator (0.9 to 4.4 cores at Saturation), the stand-ins and
its own links' packets on the same host, and the step gains little past 8 below; a headless batch
runs one simulator per worker (half the cores by default), so threads inside each would
oversubscribe the host. One episode on a free host can ask for more (`eval/headless/sim.py`
passes the variable to the simulator). The step's threads are never the async runtime's: the
live bridge runs the truth handler's sensing and fold on a blocking thread (`spawn_blocking`),
which hands the pool its vehicles and waits; the simulator calls it in line on its one thread.

**The same, at any count.** The scale watch (golden-gate-scale-watch, agent `none`, seed 1: 100
of ours, physical model, terrain and bathymetry) gave the same record and audit digests
(`555c7860...`, `2643f0b2...`, 6,225 audit events, `test_episode.digest`) before the change and
on 1, 2, 4 and 8 threads. A verbatim copy of the engine before the change, run beside today's
for the check and not kept, gave the same detections tick by tick as today's on 1, 2, 3 and 8
threads over 200 ticks of a mixed world (every sensor type, contacts of every domain, terrain and
bathymetry grids, navigation errors, identify orders, a uid named twice) in three observer
orders, and on 1 to 16 threads over 60 ticks at Saturation's size (below).
`tracks::tests::sense_par` keeps the first check without the copy: one thread against 2, 3 and 8,
detections and the engine's whole state after (`Engine::state_text`), in three orders.
Since contacts slow at their kind's rate (bd murmur-2of4: this world's ferries now slow onto their
piers, its ships onto their transits' ends) the scale watch gives record `8247b39e...` and audit
`f6a14f3b...` (6,225 events, 2026-11-10); the same code with the contacts' old one-step stops
(`MURMUR_MOTION_MODEL=legacy`) gives the pair above, so the motion alone moved it. Compare a change
to the physical step with the new pair.

**Saturation's size, synthetic** (`tracks::tests::sense_par::sense_par_bench`, release: 3,602 of
ours in Saturation's mix, 1,700 X10s at 100 m, 1,300 Pumas at 800 m, 200 Saildrones, 300
Corsairs, 100 REMUS at 30 m down and 2 armed MQ-9Bs at 7,600 m, four in five on an orbit or a
patrol; 500 small drones in five swarms and 7 other contacts, all moving; no terrain, as
Saturation; spread over a 24 km square, 6,560 detections a tick; 20 ticks' warm-up, then 40
timed). The engine before ran in the same binary, first. Milliseconds a step, mean (median); the
host's load average (one minute) beside each run, on its 28 logical CPUs (an i7-14700K: 8
performance cores with two threads each, 12 efficiency cores):

| threads | load 6 to 7 | speedup | load 6 to 8, before the vehicles' forgetting moved to the pool |
|---|---|---|---|
| the engine before | 247 (243) | | 202 (205) |
| 1 | 216 (220) | 1.0 | 207 (208) |
| 2 | 121 (122) | 1.8 | 112 (115) |
| 4 | 64 (64) | 3.4 | 59 (59) |
| 8 | 39 (39) | 5.6 | 39 (38) |
| 12 | 31 (31) | 7.0 | 32 (32) |
| 16 | 27 (27) | 8.1 | 30 (30) |

On one thread the step costs what it did, within the runs' noise (at loads of 4 to 14 the engine
before took 200 to 248 ms a step over five runs, today's on one thread 207 to 216 over four).
Denser (the same fleet and contacts over a 12 km square, 18,224 detections a tick; load 7 to 9):
before 211, then 228, 141, 76, 48 and 38 ms on 1, 2, 4, 8 and 16 threads. What stays on one
thread at this size, about 5 ms a step (timed part by part on 8 threads): moving each vehicle's
state out and back, the merge in observer order, the cues (6,560 fixes) and the shared part of
forgetting. Under heavy load the times scatter (load 25: before 311 ms; 236, 128, 161, 64, 45
and 35 on 1, 2, 4, 8, 12 and 16 threads).

**The scale watch** (one 30-minute episode, the simulator's per-task timer, `bridge.sense` over
its 1,799 steps; the host loaded to 15 to 29 by other agents' work throughout, which these
numbers carry: run them again on a quiet host before quoting a speedup):

| | series 1 (load 15 to 23) | series 2, reverse order (load 19 to 29) |
|---|---|---|
| before the change | 27.8 s | 20.6 s |
| 1 thread | 28.2 s | 28.9 s |
| 2 threads | 9.4 s | 11.7 s |
| 4 threads | 8.4 s | 13.9 s |
| 8 threads | 7.7 s | 13.7 s |

**Live** (300 stand-ins of `sim/fleets/scale-300.toml` against `golden-gate-scale-traffic.toml`,
the physical model with its terrain and bathymetry grids, no link emulator, its own port block,
the default 7 threads): the bridge said `sensing: the physical step on 7 threads`, 36 tracks
within two minutes (24 surface, 10 ground, 2 air; an AIS identity contradicted by a camera's
identification among them), and its minute line `sense 61x mean 7.8 worst 27.5 gap 1004.0 ms`.
At Saturation the step averaged 99 to 117 ms idle and 340 to 490 spread on one thread; the
synthetic case above suggests about a fifth of that on the default 7.
