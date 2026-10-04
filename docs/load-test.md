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
`base + N` stays below 39160). So the practical ceiling is about **4,000 stand-ins per host**
without widening the range (not changed here; `sysctl net.ipv4.ip_local_port_range` would, or
stand-ins could bind fixed ports like the bridge does). CPU and memory are far from limits:
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
