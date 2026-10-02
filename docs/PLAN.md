# Murmur plan

The working plan, in implementation order. Read with [DECISIONS.md](DECISIONS.md) (what was
decided and why). Live task state is in `bd` (`bd ready`); this file is the design those
issues point to. Last updated 2026-10-02.

## What Murmur is

An open-source passion project: a C2 (command and control) testbed for one question. **As one
operator supervises 100 autonomous nodes, does human judgment over consequential decisions stay
meaningful, and how would you know?** Context: the US announced the Autonomous Warfare Command
(AutoWarCom) on 2026-09-30; its public framing is scale, with nothing said about how human
judgment survives scale. This is about C2 decision and command, not weapons.

Why defense first: it is the hardest, highest-stakes setting for supervising autonomy (contested
links, adversaries, irreversible decisions). Practices proven there carry over to easier
commercial settings such as delivery fleets; the reverse doesn't hold.

The pitch is depth and rigor in the open, not novelty: the ideas exist in government labs and
proprietary C2, and several 2026 open projects implement shallow pieces (AMOS, AeroMind, Pharos,
SandTable). The full pressure test is in the "Murmur: Measuring Meaningful Human Control in
Autonomous C2" doc (link in DECISIONS.md).

## Architecture (target)

Three layers. **Agents decide, they don't drive.**

1. **Vehicle autonomy**: ArduPilot SITL (15 platform types) and stand-ins (`sim/standins.py`).
   They fly, drive and avoid. Built.
2. **Deterministic mission layer** (bridge): every action arrives as an **intent**, passes
   **fail-closed checks** (invariants), then a **risk-tier policy** routes it to auto-execute or
   to a human. Every step goes to an append-only **audit log**.
3. **Agent layer**: AI agents (via MCP) and humans use the same intent API. Agents can nominate
   and explain; the authorize step is closed to them in code.

Today the bridge goes straight from a REST task to MAVLink (`bridge/murmur_bridge/tasks.py`,
`TaskManager.submit`). Phase C inserts the intent, check, policy and audit steps in front of it.

## Done

- **Foundation**: bridge (`bridge/murmur_bridge/`), console (`web/`), sim launcher
  (`sim/swarm.py`). Command lifecycle sent -> acked -> active -> done judged from telemetry;
  "left task" and "NOT MOVING" detection; link health per vehicle; 2D/3D map.
- **Tidewater** (sibling repo `~/tidewater`): real tide, current, depth, wind for a place and
  time; ArduPilot patch `SIM_WATER_DEPTH` (boats ground, subs get seabed and currents).
- **Phase A, scale**: 22-type platform catalog (`sim/platforms.toml`), fleets
  (`sim/fleets/*.toml`, default `full-spectrum` = one of each), stand-ins, telemetry plan,
  change-only WebSocket, 100-node load test (`docs/load-test.md`), Platforms panel.

## Phase C: intents, rules, risk tiers, audit log, inbox (next)

Goal: every action is an intent with a recorded decision trail; consequential ones wait for a
human in one inbox. Usable on its own as an open "authorization layer for MAVLink fleets".

**C.1 Intent model and API** (`bridge/murmur_bridge/intents.py`)

```
Intent {
  id, created_t, source: {kind: "human"|"agent", id},
  kind: "orbit"|"patrol"|"hold"|"rtl"|"track"   (E adds "engage")
  sysids: [..], params: {..} (same as today's task params),
  constraints: {..}?  (operator-set, e.g. reserve_pct),
  rationale: str?      (agents must give one)
  state: proposed -> checked -> (auto_approved | pending_approval)
         -> (approved | rejected | expired | voided) -> dispatched -> (done | failed)
  checks: [{rule, passed, detail}], policy: {tier, reason}, approval: {by, t, note}?
}
```

- `POST /api/intents` creates one; `POST /api/intents/{id}/approve|reject` (human only; the
  request carries an operator id); `GET /api/inbox`. Keep `POST /api/tasks` as a thin wrapper
  that creates a human intent, so the current UI keeps working during the change.
- Approved intents dispatch through the existing `TaskManager` (no rewrite of the MAVLink side).
- Pending approvals expire (default 120 s, policy-configurable).

**C.2 Rules layer** (`bridge/murmur_bridge/rules.py`): pure functions
`(intent, world snapshot) -> CheckResult`, fail-closed (a rule that can't evaluate fails).
Initial rules:
- capability: the platform kind supports the task (today's `KINDS` table in `tasks.py`);
- readiness: navigation ready (`Vehicle.has_position`);
- platform limits: orbit radius >= min turn radius (today it silently widens; becomes a check
  with an auto-fix suggestion);
- domain: boat and sub waypoints on water, UGV waypoints on land (tidewater terrain if
  installed, else skip with a recorded "not evaluated" that fails closed only for engage);
- geofence / no-go areas: polygons from a scenario or operator file (`sim/areas/*.geojson`);
- reserves: estimated endurance after the task >= reserve (stand-ins report battery from
  endurance; SITL battery from SYS_STATUS).

**C.3 Risk-tier policy** (`bridge/murmur_bridge/policy.py` + `sim/policy.toml`): a plain rules
file, not OPA/Cedar yet. First match wins:
- `hold`, `rtl`: auto;
- any task on a platform marked `armed = true` in `platforms.toml` (add the flag:
  switchblade, fpv, ugv_armed): human;
- leaving the operator's approved area: human;
- everything else: auto (tunable; approval fatigue is measured in G).
Agent-sourced intents can never reach `approved` without a human for any non-auto tier.

**C.4 Audit log and replay** (`bridge/murmur_bridge/audit.py`, `scripts/replay.py`)
- Append-only JSONL per run in `logs/audit/<run-id>.jsonl`: `{seq, t, type, ...}` with types
  `intent.created`, `check.result`, `policy.decision`, `approval.*`, `intent.dispatched`,
  `op.state`, `vehicle.event` (mode change, failsafe, statustext), `operator.action`.
- Each decision event records its inputs (the world facts the rules read, with their
  timestamps), so decisions can be recomputed.
- **Exit gate meaning:** `scripts/replay.py <log>` recomputes every check and policy decision
  from the logged inputs and must match the logged outputs exactly. Physics is not replayed
  (SITL isn't deterministic); the decision layer is.

**C.5 Inbox UI** (`web/src/InboxPanel.tsx`): one queue for pending approvals and alerts,
ranked by severity. Alerts from what the bridge already detects (lost link > 3 s, NOT MOVING,
left task with the autopilot's message, task failed, ack timeout). Acknowledge / snooze; click
flies the map to the vehicle. Badge count in the top bar.

**C.6 Tests** (`bridge/tests/`, issue murmur-tve): rules and policy unit tests, replay
determinism, LinkStats (gaps, reorder, wrap).

Exit: replay reproduces a run's decisions exactly from its log.

## Phase C2: tracks and sensing

- Scenario contacts that are not ours: hostile, suspect, neutral shipping, civilian, unknown,
  generated per scenario (start in `sim/contacts.py`, run with the stand-ins) with simple
  motion. Keep **truth** (simulator) and **perception** (bridge tracks) separate: evaluation
  needs both.
- Sensing model: platforms get `sensor_range_m` and `sensor_type` in `platforms.toml`;
  detection probability falls with range; identity confidence grows with dwell and number of
  independent sensors; an injectable misclassification rate.
- Tracks in the bridge world model with `confidence`, `sources`, `last_seen_t`, position
  uncertainty that grows with age. Staleness is a first-class fact on everything agents read.
- Map: 2525 affiliation symbols (hostile/neutral/unknown), fading with age.

Exit: tracks visibly age and degrade in the console.

## Phase D: degraded links

- **User-space link emulator** (`sim/linkemu.py`): a UDP proxy between vehicles and bridge.
  Per link class from `platforms.toml` (`los`, `mesh`, `satcom`, `acoustic`, `tether`): loss,
  latency, jitter, bandwidth, outages. Range and terrain line-of-sight from tidewater's DEM.
  Acoustic: tiny bandwidth underwater, SATCOM only when surfaced. Scripted events from the
  scenario ("30% loss on all LOS links from minute 5"). Supersedes the netem plan (murmur-p1f);
  `tc netem` only as a one-off cross-check.
- Bridge behaviors: dead reckoning with a growing uncertainty area for silent vehicles
  (murmur-4y9); intents queued for unreachable vehicles with expiry (murmur-5p4); fallback
  rules per action class; telemetry priority under low bandwidth (murmur-f6q).

Exit: the 30%-loss write-up (murmur-t7d), with numbers.

## Phase E: engagement decision chain

C2 decision study, not weapons: effects are abstract events (a point and a radius). No
autonomous engagement.

1. **Nominate**: pair a track with an armed platform (human or agent).
2. **Check** (fail closed): identity confidence and recency thresholds; inside an engagement
   zone; effect radius clear of the no-strike list; no friendly node within keep-out (all
   domains); civilian contacts in the area; reachable in time.
3. **Preview**: run the platform's path forward (stand-in kinematics) against the track's
   projected motion.
4. **Authorize**: human with engagement authority only; optional two-person.
5. **Hold the picture**: the approval stores a track snapshot; moved beyond tolerance,
   reclassified or stale voids it automatically.
6. **Execute with abort**: abort until the effect; link-loss fallback recorded in the
   approval (abort / return / loiter), never silent continue.
7. **Assess**: a sensor platform confirms; all of it in the audit log.

Exit: every invariant has a scenario that tries to break it.

## Phase F: AI agents via MCP

- MCP server (`bridge/murmur_bridge/mcp_server.py` or a separate process) exposing the world
  model (vehicles, tracks, areas, intents, staleness on every fact) and intent tools
  (`propose_intent`, `explain`). No approve tool exists.
- Agent runner for scenarios; prompts in `agents/`. Read AeroMind's and Pharos's agent
  layers first (DECISIONS.md, open-source neighbors).

Exit: agents never bypass a check in any test run.

## Phase G: evaluation harness

- Headless scenario runner: seeded, stand-ins (and SITL at speedup where needed), batch.
- Adversarial scenarios: misclassified neutral, target moves after approval, link cut
  mid-engagement, friendly sub near target, time pressure.
- Metrics: should-reject catch rate, approvals correctly voided, approval time against
  information shown, override rate (borrow SandTable's definitions), invariant violations
  (must be zero).
- Compare agent+human, human-only, rules-only baseline on the same seeds.
- Human-behavior claims need participants; the harness supports studies, it doesn't replace
  them.

Exit: results published, including what went wrong.

## Alongside

- TAK/CoT export (murmur-dg2): after C2, when there are tracks worth sharing.
- Tidewater: SFBOFS currents (tw-ln7), wind bias correction, upstream the ArduPilot patch.
- Bugs: antenna tracker stall (murmur-9bf), ArduPlane extra streams.
- Polish: map label declutter (murmur-02g).
