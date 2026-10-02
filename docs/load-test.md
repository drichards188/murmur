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
