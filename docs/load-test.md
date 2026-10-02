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
