# Murmur

Open-source C2 testbed: one operator supervising ~100 autonomous air, ground, surface and
subsurface nodes, built to measure whether human judgment over consequential decisions stays
meaningful. Decision and command, not weapons.

## Start of a session

1. Read `docs/PLAN.md` (the plan, phase by phase) and `docs/DECISIONS.md` (what was decided
   and why; don't relitigate those).
2. `bd ready` for open work (see AGENTS.md for bd usage). Done: R (Rust core), C (intents, rules,
   policy, audit, inbox), C2 (contacts, sensing, tracks), F (MCP for agents). Current: **E**
   (engagement decision chain) and **G** (evaluation harness).
3. Sibling repo `~/tidewater` (real-world conditions). ArduPilot at `~/ardupilot`, branch
   `tidewater-water-depth`.

## Run and verify

- `make goldengate` runs the default fleet (one of each of 22 platform types) at the Golden
  Gate with tidewater conditions; `make up FLEET=<name>` for other fleets; `make down` stops.
  Console http://127.0.0.1:5173, bridge http://127.0.0.1:8765 (`/api/state`, `/api/tasks`,
  `/api/platforms`, `/api/stats`; MCP server for agents at `/mcp`, header `X-Murmur-Agent`,
  ids in `MURMUR_AGENTS`; `agents/README.md`). Vehicles ready in ~30-60 s.
- Live path is Rust: `crates/murmur-bridge`, `crates/murmur-standins`. `scripts/up.sh` builds
  their release binaries (`make build` does too) and, with a scenario, starts `tidewater serve`
  (reused if one answers) and `tidewater drive`, and stops them with the stack.
- `make check` (typecheck, lint, py_compile, `cargo clippy` and `cargo test` for the workspace;
  toolchain in `~/.cargo/bin`). Tidewater: `cd ~/tidewater && .venv/bin/pytest`.
- `scripts/measure.py` measures bridge and stand-in load; `scripts/scale_fleet.py N` writes an
  N-node stand-in fleet. Logs in `logs/`, SITL and stand-in logs in `sim/run/`.
- Links go through `crates/murmur-linkemu` by default (`MURMUR_LINKEMU=0` bypasses):
  profiles `sim/links.toml`, events `make goldengate EVENTS=sim/link-events/golden-gate.toml`,
  log `logs/linkemu.log`, PID `logs/linkemu.pid`; the bridge reads `sim/run/bridge-fleet.json`
  and serves the emulator's view at `/api/links[/{uid}]`.
- Ports: vehicles UDP 14560+i -> linkemu -> bridge UDP 15560+i (`MURMUR_BRIDGE_BASE_PORT`,
  default base + 1000); linkemu stats HTTP :8766 (`MURMUR_LINKEMU_PORT`); bridge :8765; console
  :5173; tidewater serve :8770; SITL SERIAL1 TCP 5762+10*instance (tidewater driver). A second
  stack beside the default one: set `MURMUR_BASE_PORT`, `MURMUR_BRIDGE_BASE_PORT`,
  `MURMUR_LINKEMU_PORT`, `MURMUR_HTTP_PORT`, `MURMUR_WEB_PORT`, `MURMUR_TIDEWATER_PORT`,
  `MURMUR_SITL_INSTANCE_BASE` (header of `scripts/up.sh`).

## Gotchas (each cost real time)

- **Never `pkill -f <pattern>` with a pattern that appears in your own command line**: it kills
  your shell (exit 144). Use `make down` (PID file `logs/up.pid`; it sends TERM, because an
  `up.sh` started in the background ignores SIGINT).
- Background tasks in this harness stop after 2 hours: restart with `make goldengate`, or start it
  detached (`setsid nohup make goldengate > logs/goldengate.out 2>&1 &`) or in your own terminal.
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
- ArduPilot stream-rate params are `MAV1_*` here (not `SR0_*`). Pass SITL one defaults file, not a
  comma list: ArduPilot drops `MAV1_*` from an earlier file in the list (murmur-yjq; `swarm.py` merges).
- In SITL, stop vehicles with forces the IMU feels, never by zeroing velocity or clamping
  position (the EKF diverges).
- tidewater is installed editable in `.venv`; a scenario needs `tidewater fetch` once.
- `murmur-replay` accepts only the current decision-rules version by default: replay older logs with
  `--rules N` / `--min-rules N` (versions and changelog in `crates/murmur-decide/README.md`). The bridge
  writes run-time anchors to `logs/anchors/<run>.anchors`; verify with `murmur-replay <log> --anchors <file>`.
  Engagements: `MURMUR_ENGAGE_OPERATORS` (two distinct signers; each must also be in `MURMUR_OPERATORS`,
  or the bridge refuses to start; both lists are logged as the run's roster), scenarios `murmur-scenarios`
  (`sim/scenarios/engage/`).
- Stand-ins each take one ephemeral UDP port (`net.ipv4.ip_local_port_range`, ~4,000 here,
  shared with everything else on the host): that, not CPU, caps a stand-in fleet near 4,000.
- A stack uses two UDP ranges now (vehicle base + N and bridge base + N): pick both clear of
  every other stack's ranges, including a large load test's (a 3,000-node test spans 3,000
  ports from its base). A bridge that finds a port taken drops it for good.
- Keep fixed port bases (vehicle, bridge) below the ephemeral range: stand-ins and clients draw
  from it, and a fixed base inside it collides at random. It can change between boots: check
  `sysctl net.ipv4.ip_local_port_range` (37795-41890 as of 2026-10-04; was 39160-43255).
- Rules 5 queue: an order for a vehicle the link can't carry it to now waits in the Queue
  (console button; `GET /api/inbox` `queued`), it is not an error; one whose link drops
  before the vehicle started it comes back there (`intent.requeued`). A human's approval is
  held 120 s at most (`[queue] human_expire_s`), then back to the inbox. An order whose start
  went out unconfirmed (op `unconfirmed`) may be running: it is only followed if the vehicle
  runs it, else a human decides; a vehicle that runs one no longer in force raises a
  `may_be_running` alert. Open intents carry into the next run only after a crash (the
  pointer `logs/audit/.resume-<port>.json` that a clean stop, `make down`, deletes; within 15
  min) or with `MURMUR_RESUME=<log>`, onto the same vehicles (platform, callsign, home within
  200 m, from `<log>.fleet.json`) and a log matching its anchors; without those files nothing
  carries unless `MURMUR_RESUME_UNVERIFIED=1`. Replay such a log with its predecessor beside
  it (or `murmur-replay --prev`). `MURMUR_RESUME=0` turns it off.
- The bridge's task-layer timeouts follow each link's measured round trip (`LinkTiming` in
  `crates/murmur-bridge/src/link.rs`; floors 1 s per ACK, 1.5 s per mission item, as before on
  fast links) and its telemetry plan follows the link class (`crates/murmur-bridge/src/
  telemetry.rs`: HIGH_LATENCY2 every 5 s on Iridium-class links). A task to an AUV under water
  still crosses a 300 bps modem: expect tens of seconds per exchange, and ArduPilot's own 8 s
  mission-upload timeout makes uploads there unlikely to finish. `MURMUR_TELEMETRY=uniform`
  restores the one-plan-for-all for A/B runs.

## Conventions

- bd for all task tracking; the pre-commit hook exports issues to `.beads/issues.jsonl`.
- Commit and push to the local remote (`~/git/murmur.git`) when work lands.
- No AI attribution in commits; no em dashes in prose (global CLAUDE.md).
