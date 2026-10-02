# Murmur

Open-source C2 testbed: one operator supervising ~100 autonomous air, ground, surface and
subsurface nodes, built to measure whether human judgment over consequential decisions stays
meaningful. Decision and command, not weapons.

## Start of a session

1. Read `docs/PLAN.md` (the plan, phase by phase) and `docs/DECISIONS.md` (what was decided
   and why; don't relitigate those).
2. `bd ready` for open work (see AGENTS.md for bd usage). Current phase: **C** (intents, rules,
   risk tiers, audit log, inbox).
3. Sibling repo `~/tidewater` (real-world conditions). ArduPilot at `~/ardupilot`, branch
   `tidewater-water-depth`.

## Run and verify

- `make goldengate` runs the default fleet (one of each of 22 platform types) at the Golden
  Gate with tidewater conditions; `make up FLEET=<name>` for other fleets; `make down` stops.
  Console http://127.0.0.1:5173, bridge http://127.0.0.1:8765 (`/api/state`, `/api/tasks`,
  `/api/platforms`, `/api/stats`). Vehicles ready in ~30-60 s.
- `make check` (typecheck, lint, py_compile). Tidewater: `cd ~/tidewater && .venv/bin/pytest`.
- `scripts/measure.py` measures bridge load. Logs in `logs/`, SITL logs in `sim/run/`.
- Ports: vehicles UDP 14560+i -> bridge; SITL SERIAL1 TCP 5762+10*instance (tidewater driver).

## Gotchas (each cost real time)

- **Never `pkill -f <pattern>` with a pattern that appears in your own command line**: it kills
  your shell (exit 144). Use `make down` (PID file `logs/up.pid`).
- Background tasks in this harness stop after 2 hours: restart with `make goldengate`.
- Foreground `sleep` is blocked: wait with `until <check>; do sleep 2; done`.
- Global git `core.autocrlf=true` (WSL): repos rely on `.gitattributes` (LF). A fresh ArduPilot
  clone needs `-c core.autocrlf=false` or its python shebangs break.
- The Playwright browser renders WebGL in **software** (SwiftShader): 3D fps there is
  meaningless (use `?perf` on a real GPU). After many reloads it loses the WebGL context:
  `browser_close` and reopen.
- Dev-only globals for debugging the console: `window.murmurMap`, `murmurBridges`, `murmurPerf`.
- MapLibre is pinned to **v5** (deck.gl 9.4 breaks on v6). Don't upgrade.
- After changing SITL code or its param table, rebuild all firmwares:
  `cd ~/ardupilot && PATH=~/murmur/.venv/bin:$PATH ./waf plane copter rover sub blimp antennatracker`.
- ArduPilot stream-rate params are `MAV1_*` here (not `SR0_*`). ArduPlane still sends ~22 extra
  msg/s (open bug).
- In SITL, stop vehicles with forces the IMU feels, never by zeroing velocity or clamping
  position (the EKF diverges).
- tidewater is installed editable in `.venv`; a scenario needs `tidewater fetch` once.

## Conventions

- bd for all task tracking; the pre-commit hook exports issues to `.beads/issues.jsonl`.
- Commit and push to the local remote (`~/git/murmur.git`) when work lands.
- No AI attribution in commits; no em dashes in prose (global CLAUDE.md).
