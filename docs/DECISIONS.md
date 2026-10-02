# Decisions

What was decided, and why, so a new session doesn't relitigate it. Newest first within each
group. Dates are 2026.

## Direction

- **Open-source passion project exploring C2 at AutoWarCom scale** (10-02). Goal: show how human
  judgment over consequential decisions can stay meaningful, measurably, when one operator
  supervises ~100 nodes. Not commercial.
- **C2 decision and command, not weapons** (10-02). Engagement decisions are in scope as a
  decision-system problem; effects are abstract events. No autonomous engagement: agents
  nominate and explain, the authorize step is closed to them in code. Armed platforms always
  need human approval. Stays inside DoD Directive 3000.09 (appropriate human judgment over use
  of force).
- **Pitch depth and rigor, not "first"** (10-02). Pressure test found the ideas in closed
  work (RESCHU, IMPACT, Aegis doctrine, Lattice, Legion, Palantir AIP, Thunderforge, DARPA
  ASIMOV) and shallow open versions (AMOS, AeroMind, Pharos, SandTable). Cite them. Engage
  SandTable (metric definitions, maybe interop) and the TAK servers rather than compete.
  Full write-up: https://claude.ai/artifact/7CNdGEur8Qs5ns3ZE1naZ3 ("Murmur: Measuring
  Meaningful Human Control in Autonomous C2").
- **Agents decide, they don't drive** (10-02). Intents only, deterministic rules layer,
  risk-tier policy, append-only audit, preview before approve. Open MCP-to-MAVLink servers
  that let a model fly directly are the anti-pattern.
- **Not an "open Lattice"** (10-02). Sensor fusion and vendor integrations are out of reach
  solo; Open-RMF is the wrong base (indoor robots). Focus on the tasking and supervision layer.
- **Stop 3D and Gazebo work here** (10-02). 3D is good enough to understand a scene; it is not
  the differentiator for C2. Gazebo stays a possible tidewater adapter, deferred.

## Fleet and simulation

- **Default fleet: one node of each platform type** (`full-spectrum`, 22 nodes) (10-02). The
  user wants the whole capability list shown, not copies. `scale-100` exists only for load
  tests.
- **Stand-ins for what SITL can't fly** (10-02): HALE/MALE/CCA aircraft, large USV, AUV,
  XLUUV, drifting buoy. MAVLink-speaking kinematic models, ArduPilot mode numbering, labelled
  STAND-IN in the console. ~0.1% CPU each vs ~16% per SITL process.
- **"-class" names, approximate public figures** (10-02): simulated stand-ins tuned to rounded
  public numbers, not models of real systems. Unknown figures are left blank, not invented.
- **Callsign plus model, readable everywhere** (10-02, user requirement): fleet rows show
  callsign, short model badge, full name and role; map symbols show callsign and short model.
- **User-space link emulator instead of tc/netem** (10-02): no sudo, and it can model range,
  terrain masking, acoustic and surfaced-only links. netem only as a cross-check.
- **Area: Golden Gate scenario, widen to Bay + approaches (~100 km) when needed** (10-02).
- **Tidewater is a separate repo** (10-01): a simulator-agnostic conditions engine; Murmur is
  its first consumer. Golden Gate, 13-16 June 2026 spring tides. Past windows replay
  **observed** water level (predictions ran 18 cm low).
- **ArduPilot patch `SIM_WATER_DEPTH`** (10-01), branch `tidewater-water-depth` in
  `~/ardupilot`, patch file in `~/tidewater/patches/ardupilot/`. Lesson: stop vehicles with
  forces the simulated IMU feels; kinematic stops (zeroing velocity, clamping position)
  diverge the EKF.

## Engineering

- **MapLibre pinned to v5** (10-02): deck.gl 9.4's adapter needs `map.transform` and
  `getFreeCameraOptions`, removed in v6 (crashed with terrain on). Revisit when deck.gl
  supports v6.
- **Telemetry plan** (10-02): bridge requests 7 messages with `SET_MESSAGE_INTERVAL`;
  `sim/params/common.parm` zeroes ArduPilot default streams (`MAV1_*`, not `SR0_*` in this
  version). WebSocket sends full state once, then changes only.
- **Readiness trusts the vehicle's EKF flags** (10-02): submerged AUVs have no GPS fix.
- **Tasks use platform cruise altitude and minimum turn radius** from the manifest (10-02).
- **Bridges lifted in 3D** by estimating deck height from the highest abutment along the
  bridge line (10-02). Documented in `web/src/bridges.ts`.

## Repo and process

- **Local git remotes only** (`~/git/*.git`) (10-02). Open: a GitHub remote (user decision),
  and the commit email (global config is a work-looking address; user to decide).
- **bd issues exported to `.beads/issues.jsonl` by the pre-commit hook** (10-02): the Dolt
  database isn't in git.
- **No AI attribution in commits; no em dashes in prose** (user's global CLAUDE.md).
