# Murmur plan

The working plan, in implementation order. Read with [DECISIONS.md](DECISIONS.md) (what was
decided and why). Live task state is in `bd` (`bd ready`); this file is the design those
issues point to. Last updated 2026-10-03.

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

1. **Vehicle autonomy**: ArduPilot SITL (15 platform types) and stand-ins (`crates/murmur-standins`).
   They fly, drive and avoid. Built.
2. **Deterministic mission layer** (bridge): every action arrives as an **intent**, passes
   **fail-closed checks** (invariants), then a **risk-tier policy** routes it to auto-execute or
   to a human. Every step goes to an append-only **audit log**.
3. **Agent layer**: AI agents (via MCP) and humans use the same intent API. Agents can nominate
   and explain; the authorize step is closed to them in code.

Before Phase C the bridge went straight from a REST task to MAVLink (the Python bridge's
`TaskManager.submit`). Phase C inserts the intent, check, policy and audit steps in front of it
(`crates/murmur-bridge/src/decision/`).

## Done

- **Foundation**: bridge (Python at first, Rust since R.4/R.5), console (`web/`), sim launcher
  (`sim/swarm.py`). Command lifecycle sent -> acked -> active -> done judged from telemetry;
  "left task" and "NOT MOVING" detection; link health per vehicle; 2D/3D map.
- **Tidewater** (sibling repo `~/tidewater`): real tide, current, depth, wind for a place and
  time; ArduPilot patch `SIM_WATER_DEPTH` (boats ground, subs get seabed and currents).
- **Phase A, scale**: 22-type platform catalog (`sim/platforms.toml`), fleets
  (`sim/fleets/*.toml`, default `full-spectrum` = one of each), stand-ins, telemetry plan,
  change-only WebSocket, 100-node load test (`docs/load-test.md`), Platforms panel.

## Phase R: Rust core (next, before C)

Decision in DECISIONS.md. Cargo workspace at the repo root, crates in `crates/`.

- **R.1 Stand-in engine** (`crates/murmur-standins`): port `sim/standins.py` (plane, boat,
  drifter, sub) with parity tests against the Python one; same `sim/run/standins.json` input.
  Thousands of nodes in one process. Follows the stand-in contract in `docs/AUTHENTICITY.md`.
- **R.2 tidewater sampling service** (`tidewater serve`, Python): batch HTTP query of currents,
  depth, wind for many points, so the Rust stand-ins get real conditions without
  reimplementing tidewater.
- **R.3 Six new stand-in behaviors**: copter, VTOL, airship, ground vehicle, sailboat, antenna
  tracker. Then any platform can run light; `sim = "standin"|"sitl"` per fleet entry.
- **R.4 Rust bridge** (`crates/murmur-bridge`): same HTTP/WebSocket API, keyed by `uid`;
  telemetry plan, link stats, task lifecycle (port of `bridge/murmur_bridge/`). Parity: the
  same fleet behaves the same against both bridges; the console notices no difference.
- **R.5 Retire the Python bridge and stand-ins**; Makefile, scripts and docs point at Rust.
  Done: `bridge/` and `sim/standins.py` deleted (in git history); `scripts/up.sh` builds and runs
  the Rust bridge and stand-ins and starts `tidewater serve` with a scenario; 300, 1,000 and
  3,000-node stand-in fleets measured in `docs/load-test.md`.
- **Authenticity fields**: real-world protocol per platform in `platforms.toml`, shown in the
  Platforms panel.

Exit: `make goldengate` runs entirely on the Rust bridge and stand-ins, and a 1,000-node
stand-in fleet runs with numbers in `docs/load-test.md`.

## Phase C: intents, rules, risk tiers, audit log, inbox

Goal: every action is an intent with a recorded decision trail; consequential ones wait for a
human in one inbox. Usable on its own as an open "authorization layer for MAVLink fleets".

**C.1 Intent model and API** (in the Rust bridge; module paths below predate Phase R)

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

Built (first cut; see the module docs named here for the figures and the simplifications):

- **Contacts that are not ours** (`crates/murmur-contacts`, `sim/scenarios/golden-gate.toml`):
  ferries on their routes, ships in the lane, fishing boats drifting on the tidewater
  current, a light aircraft, an unknown UAS, a submarine, a suspect fast boat with a spoofed
  AIS identity, a coast guard cutter. Simple motion (route, transit, loiter, station,
  drift), seeded. **Truth** goes to the bridge only on the labelled simulation channel
  (`POST /api/sim/contacts`) and to `logs/contacts-truth.jsonl` for evaluation; the bridge
  serves it back only on `GET /api/sim/truth` with `MURMUR_EVAL_TRUTH=1`.
  `MURMUR_CONTACTS=<scenario>` makes `up.sh` run it; `make goldengate` does by default.
- **Sensing model** (`sim/platforms.toml` `sensors = [{type, range_m, guess}]`, `mast_m`;
  `crates/murmur-bridge/src/tracks/sensing.rs`): detections only from our platforms' radar,
  EO/IR, AIS receiver, sonar and ESM; probability falls with range (0.5 at the nominal range,
  zero past 1.5x) and stops at the radio horizon; subsurface contacts are sonar-only; AIS
  gives a cooperative identity (absent or spoofed for a suspect); EO/IR and sonar classify
  slowly with dwell, radar and ESM give a class only; identity confidence combines across
  independent sensors; a per-scenario misclassification rate; every draw seeded.
- **Tracks** (`crates/murmur-bridge/src/tracks/`): position and velocity estimate, 1-sigma
  uncertainty growing with age, existence confidence that decays in silence, identity and
  affiliation estimates with confidence and the reason, sources, `last_seen_t`; stale after
  10 s, dropped after 180 s. Hostile is never inferred: an operator declares it
  (`POST /api/tracks/{id}/declare`, logged as `operator.action`). Exposed on
  `GET /api/tracks`, the WebSocket `tracks` message, `WorldSnapshot.tracks` (additive; old
  logs replay unchanged), the CoT export with the estimated affiliation as the TAK type,
  and to agents as the read-only MCP tool `get_tracks` and resource `murmur://tracks`
  (each track with `as_of`, `age_s`, `stale` and its uncertainty carried to now).
- **Console**: tracks as 2525 symbols with the estimated affiliation (hostile red, suspect
  red dashed, unknown yellow, neutral green, assumed friend blue) in both views, an
  uncertainty ring that grows and a symbol that fades with age, a Tracks drawer ranked
  hostile/suspect/unknown first, and per-track details (sources, confidence, age,
  identity conflicts, declaration).

Stated simplifications: association by the simulation's contact id (no gating tracker),
no terrain masking (the link emulator's DEM line of sight could feed this), no clutter or
false tracks, sensor figures are "-class" guesses. Exit met: tracks visibly age and degrade
in the console (`sim/fleets/contacts-demo.toml` shows coverage gaps at the Gate).

## Phase D: degraded links

- **User-space link emulator** (`sim/linkemu.py`): a UDP proxy between vehicles and bridge.
  Per link class from `platforms.toml` (`los`, `mesh`, `satcom`, `acoustic`, `tether`): loss,
  latency, jitter, bandwidth, outages. Range and terrain line-of-sight from tidewater's DEM.
  Acoustic: tiny bandwidth underwater, SATCOM only when surfaced. Scripted events from the
  scenario ("30% loss on all LOS links from minute 5"). Supersedes the netem plan (murmur-p1f);
  `tc netem` only as a one-off cross-check.
- Bridge behaviors: dead reckoning with a growing uncertainty area for silent vehicles
  (murmur-4y9, built: `crates/murmur-bridge/src/estimate.rs`, console `estimate.ts`,
  `estimateLayers.ts`); fallback rules per action class; telemetry priority under low
  bandwidth (murmur-f6q).
- **Dead reckoning for silent vehicles** (murmur-4y9). Past its link's stale threshold a
  vehicle's snapshot carries an `estimate`: a motion model frozen at its last report, in
  order of what is known best: its current task (an orbit: somewhere on the ring, the phase
  advancing at the last speed; a patrol: along its route; a hold: near the hold point, a
  fixed wing on its loiter circle; RTL: toward home, a fixed wing then circling home at its
  RTL radius), else the mode, else the last velocity carried on, else stationary. The region
  grows with the silence from the platform's top speed and kind (an aircraft's at tens of
  metres a second, a boat's at a few), never beyond its reach or a cap per domain; an AUV on
  its acoustic cadence is said to be silent by design. The lost-link failsafe is modelled as
  a second, labelled branch: after the profile's `lost_link_s` the vehicle is probably on
  the firmware's action (RTL, surface, land, disarm), drawn in violet; once the timer has
  fired the branch leads (bold, the details card leads with it) and the task model stays
  dimmed as "if it kept its task". Drifters ride the water: a sampler asks tidewater for the
  current and wind under every surface vehicle every 5 s and integrates a silent one's
  track (current plus 2 % windage), the region growing with the current's uncertainty;
  without tidewater, the last reported velocity or a documented 1 m/s bound. A fixed wing
  inside its RTL circle flies out to it before circling, and a joined ring carries the
  phase on. The console evaluates the model every clock tick (region, leader from the last
  known position, faded symbol, "EST on its orbit, silent 47 s"); the details card shows
  last known against the estimate, the basis, the uncertainty and the other hypothesis.
  Display only: the decision core keeps its own conservative friendly-uncertainty model and
  never reads this; MCP `get_fleet` and `get_vehicle` expose it read-only, with `as_of` and
  `age_s` and a note that it is a model, not a fact. `scripts/estimate_eval.py` measures the
  estimate against the emulator's view of the truth (or the stand-ins' truth log for a
  GNSS-denied vehicle); over three 300 s cuts at the Golden Gate the aircraft stayed inside
  their rings 100 % of the time until their timers fired and the RTL branch held them after,
  the boat's branch held it 100 % from the moment its 30 s timer fired.
- **Safe command queue** (murmur-5p4, built; rules 5; `crates/murmur-decide/src/queue.rs`,
  bridge `decision/queue.rs`, `tasks/exec.rs`; `crates/murmur-decide/README.md` "The queue
  for unreachable vehicles"): an approved order for a vehicle the link can't carry it to now
  (silent past its lost-link time, its picture not fresh again yet, or a mission for an AUV
  on its acoustic modem) is queued with a deadline (`[queue]` per kind: 900 s, hold 60 s,
  rtl 120 s), not sent into a dead link; when it can be reached its checks run again on the
  world then and it is sent, sent back to the inbox (a human's picture changed: a check, the
  tier, a reboot, an engagement now in play) or voided; past its deadline it expires. Exactly
  once: a mission is a transaction (a lost final ACK settled by asking the vehicle, a link
  lost mid-way waited out for the kind's wait and then a full mission download decides
  whether to follow, only start, or upload again), a command is retried only when neither its
  ACK nor telemetry showing its effect came back, a vehicle its failsafe sent home is held
  then armed. A restarted bridge carries open intents over from its log (`intent.resumed`;
  pointer `logs/audit/.resume-<port>.json`, `MURMUR_RESUME`): queued ones re-checked, started
  ones followed and never re-sent. Reboots (time since boot going back) are logged, end the
  op, and are a fact (`booted_t`) the re-check compares with the approval. Console: a Queue
  drawer (why each waits, time left, cancel, how recent ones left), queued paths dashed with
  QUEUED, sent orders labelled until the vehicle's telemetry confirms them. Not covered: an
  engagement under way when the bridge restarts (not carried over; its platform keeps its
  last order, nothing commands the fallback), pending approvals across a restart, and a
  stand-in that reboots on command (the fake autopilot in the tests does).

- **Space segment** (`crates/murmur-space`, murmur-0nv.1): satellites as taskable
  infrastructure, not flown nodes. The operator requests a capability over an area and time
  (imagery/SAR collection, SATCOM coverage, GNSS assurance); real orbits (SGP4, public TLEs)
  and sensor constraints give ranked options with timing (collect -> downlink -> product); a
  human picks, through the Phase C intent pipeline (intent kinds `collect`, `cover`). SATCOM
  passes drive the emulator's satcom class; GNSS availability feeds navigation. UI: footprints,
  swaths and link lines on the theater map; an access-window **timeline** as the main space
  view; a linked globe inset for orbits.

Exit: the 30%-loss write-up (murmur-t7d), with numbers.

## Phase E: engagement decision chain

C2 decision study, not weapons: effects are abstract events (a point, a radius, a time). No
autonomous engagement; no munition types, warheads, blast, lethality, terminal guidance or
aim-point optimisation anywhere.

Built (`crates/murmur-decide/src/engage.rs`, `scenario.rs`; bridge `decision/engage.rs`;
contacts `effect.rs`; console `EngagePanel`, `EngageCard`, `engageLayers.ts`):

1. **Nominate**: intent kind `engage` = (perception track id, one armed platform in `uids`,
   `effect_radius_m`, optional window, link-loss `fallback`: `abort` = hold where it is,
   `return` = RTL, `loiter` = orbit where it is). From a human (`POST /api/intents`, the
   console's Engage action on a declared-hostile track or an armed platform) or an agent
   (MCP `propose_intent` with `task: "engage"`, rationale required). A missing window is
   written into the spec as `default_window_s` from creation.
2. **Check**, fail closed, each pass / fail / unverified / not evaluated with a precise
   detail (engage is consequential: unverified and not evaluated block): `target_declared`
   (an operator's declaration; an inferred suspect is not enough), `target_identity`,
   `target_recency`, `target_uncertainty` (1-sigma now within `max_sigma_fraction` of the
   effect radius), `engagement_zone` (new area kind; the whole effect circle inside one),
   `no_strike` (new area kind: the bridge, Fort Point, Kirby Cove, Baker Beach, Crissy
   Field, ferry terminals; the whole circle clear), `keep_out_friendly` (every vehicle of
   ours but the platform, those the fleet manifest names but never heard from included, so
   they fail it; carried along its velocity to the effect time, else how far it could get
   at cruise speed), `keep_out_tracks` (every track but the target and tracks an operator
   declared hostile, with 2-sigma of its predicted uncertainty), `platform_armed`,
   `reach` (catalog cruise speed, intercept inside the window and `max_transit_s`,
   endurance or, with none in the catalog, battery above the reserve and a short transit,
   the effect point on the surface a boat or UGV needs, from a sample within 50 m, logged
   with where, and not evaluated on a shoreline). The generic rules run too (capability,
   readiness, geofence on the transit and return legs). Thresholds in `[rules.engage]` of
   `sim/policy.toml`, each bounded (`EngageRules::validate`).
3. **Preview**: a straight-line intercept at catalog cruise speed after `dispatch_delay_s`
   against the track carried along its estimated velocity; the predicted effect point and
   time; the track's uncertainty at that time as an ellipse along its course (the
   tracker's growth law along, half across). Stored on the intent, logged with every check
   (replay recomputes it), drawn on the map.
4. **Authorize**: tier `engagement` (human, `submitter_authorizes = false`,
   `proposer_may_approve = false`, `approvers = 2`). The engine holds a floor whatever the
   file says (never auto, never at submission, never the proposer, never fewer than two
   approvers; replay holds the same floor). Only an `EngagementAuthority` signs one
   (`Engine::approve_engagement`); it exists only in a session opened with engagement
   authority (`Grants`, logged with the session; the bridge grants it to
   `MURMUR_ENGAGE_OPERATORS`). A signer first asks for the picture (`issue_picture`, logged
   `picture.issued`: a token bound to their session with the plan and the track's identity,
   valid `token_ttl_s`) and the signature quotes it. The first signature freezes the plan
   it was shown (effect point and time, radius, window, platform, fallback, the target's
   identity); every later one signs that exact plan; the first N-1 are
   `approval.concurred`, the Nth `approval.granted`, whose snapshot carries the plan. At
   each later signature and every second while signatures wait the checks run on the plan
   (the track where the plan's track path puts it now and, carried to the plan's time, at
   its point, each within the tolerance plus a bounded allowance for velocity-estimate
   uncertainty; zone, no-strike and keep-outs there; the platform still able to make it;
   the same identity and declaration; all signatures within `max_signing_s`); when it stops
   holding, every signature is dropped with it (`approval.reset`) and a new plan must be
   signed. The effective signing window is `dispatch_delay_s` plus the arrival slack:
   about 85 s for a holding target, 30 to 35 s for a fast mover (DECISIONS.md). A first
   signer whose picture went stale looks again; nothing is voided while the nomination
   holds. One engagement per platform (`platform_free`). Our vehicles count in the keep-out
   with their prediction uncertainty (`friendly_growth_mps`). Compile-fail doctests: no
   forged, cloned or deserialized engagement authority, a plain operator authority is not
   one, and an `AgentPort` has no approve or abort method for engagements.
5. **Hold the picture**: the final approval stores an `EngageSnapshot` (track facts, the
   preview, the platform's link). `Engine::step_engagements` (the bridge's 1 Hz loop)
   re-checks it every second (`engage.picture`): declared, reclassified, recency, deviation
   from the snapshot's prediction, keep-outs at the effect time, the platform's link against
   the approval's assumption, GNSS navigation (`platform_nav`: the filter on an absolute
   position, a 3D receiver fix, no GPS glitch flagged), the window and lateness, and
   first that the last check was at most `max_picture_gap_s` ago (`picture_gap`: a stalled loop aborts, never resumes; the
   terrain fetch runs beside the loop). A failure voids it before dispatch and aborts it
   after (`engage.stopped`, cause `picture`), with the reason. While an engagement is in
   play (pending, approved, dispatched) on a vehicle, an agent's order for that vehicle
   always goes to a human; an operator's own order runs and supersedes it (cause
   `superseded`, by that intent, `fallback: null`), and approving an order that would stop
   one needs an acknowledgement (logged `supersedes`).
6. **Execute with abort**: the platform is sent to loiter at the predicted effect point
   (an orbit through the task layer: no homing). Any operator may abort until the effect
   (`POST /api/intents/{id}/abort`); every stop commands the recorded fallback. The effect
   is released when the picture holds, the predicted time has come, the platform is within
   `arrival_m` and the track is estimated inside the effect radius (`engage.effect`); the
   bridge hands it to the contacts process on the simulation channel, which flags contacts
   inside the radius (`on_effect = "out_of_play"` takes one out of the scenario,
   `"persists"` keeps it) and writes what was inside and any collateral (other contacts, our
   vehicles but the platform; ours with no position as unknown) to the evaluation truth log
   only.
7. **Assess**: our sensors (catalog `sensors`) covering the effect point after the effect
   (`engage.observe` each second): `persists` (the track detected after the effect),
   `ceased` (no detection while covered for `assess_dwell_s`), `unknown` (not covered long
   enough within `assess_window_s`), with the evidence; the intent ends `done`.
8. **Audit and replay**: every step above is an event with its inputs; `murmur-replay`
   recomputes checks, previews, policy, snapshots, picture verdicts (the voids),
   observations and assessments, and checks the two-person rule (distinct signers, never the
   proposer, the count the decision asks for, every signature's picture and every reset)
   and engagement authority from the logged sessions, and the engagements in play logged
   with each decision. Tampering (an approval by the proposer or an agent, a suppressed
   void, an edited snapshot or effect point, a deleted signature, `approvers` stripped from
   the policy and decision, a hidden reset, an agent's order made to look automatic) is
   caught even with a recomputed chain.

Decision rules are versioned (`RULES_VERSION`, recorded in `run.started` with the build):
replay recomputes every log with the rules it was produced under, accepts an older version
only when asked (`--rules N`, `--min-rules N`), and flags a log that carries more than its
claim allows; committed logs of every version replay in the decide crate's tests. The
bridge anchors its log as it grows (`logs/anchors/`, `MURMUR_ANCHOR_CMD` to post anchors
elsewhere; `murmur-replay --anchors`).

State machine additions: `dispatched -release-> effected -complete-> done`,
`dispatched -abort-> aborted` (terminal). `finish` and `dispatch` refuse engagements.

**Adversarial scenarios** (`sim/scenarios/engage/`, format in its README; runner
`murmur-scenarios`, library `murmur_decide::scenario`): 36 files, one or more per invariant
(agent authorizes, proposer approves, double signature, neutral enters the keep-out,
reclassified, undeclared, target moves, link cut, friendly sub near and closing, stale
track, outside the zone, no-strike, inferred suspect, operator abort, persists, unarmed,
deleted signature, window too short, a policy file without the engagement tier, the target
moving between signatures and stale previews, an agent's order on the engaging platform, a
10 s gap in holding the picture, approvers stripped from the log, the effect point sliding
and the target reversing between signatures, a transient reclassification, a picture taken
before a reclassification, an approval that would stop an engagement, GNSS lost en route, a
wobbling velocity estimate, a second engagement on a platform, a GPS glitch en route, a
first signer's stale picture, a target getting under way while a signature waits, a friendly
closing to just outside the keep-out).
Each ends in reject, void or abort as designed and replays; the tampered logs (including a
loosened logged policy) are caught. Seeded noise turns each into a batch (Phase G): position
noise independent around the truth each step (`--noise`), and a velocity-estimate error
correlated over about 10 s (`--vnoise`). Seeds 1 to 10 and 42 pass every scenario at 0, 3,
10 and 20 m with 0, 0.3 and 0.5 m/s (4,320 runs); at 1 m/s per axis some long-lead releases
on moving targets abort late (the plan's point is off by the velocity error times the
lead): the chain fails safe, never releasing off the plan.

Live demo: `sim/fleets/engage-demo.toml` (stand-in switchblade, magura, ugv_armed, corsair,
x10) with `sim/scenarios/golden-gate-engage.toml` (the fast boat holding west of the bridge,
a tour boat circling through the keep-out).

Simplifications: straight-line intercept at cruise speed (no turn radius, no climb), keep-outs
are 2D cylinders (conservative: a fast track's 2-sigma and a friendly without a fresh
velocity grow without a cap), the effect circle is checked against no-strike areas but the
platform's own path only against no-go areas, the assessment trusts our sensor model's
coverage figures, the engagement authority is the same placeholder authentication as
`MURMUR_OPERATORS`, the fallback is commanded after a stop but not preloaded as the
vehicle's lost-link action, and another declared hostile inside the keep-out is exempt like
the target (an open design question, DECISIONS.md).

Exit met: every invariant has a scenario that tries to break it, and each ends as designed.

## Phase F: AI agents via MCP

- **F.1 MCP server in the bridge** (`crates/murmur-bridge/src/mcp/`, done): served at `/mcp`
  on the bridge's HTTP port (streamable HTTP, official Rust SDK `rmcp`), one in-process
  `AgentPort` per agent id (`MURMUR_AGENTS`, header `X-Murmur-Agent` or the MCP
  `clientInfo.name`; placeholder auth like the operator header). Reads with staleness on
  every fact (fleet, platforms, areas, policy, intents, inbox, recent vehicle events,
  satellite passes and tasking options, as tools and as `murmur://` resources); proposing
  with `preview_intent` (a dry run the engine logs as `agent.action intent.preview`; for a
  collect without `chosen`, a tasking query with the options), `propose_intent` (rationale
  required; vehicle tasks, and collect and cover through `decision::space`),
  `explain_intent`, `withdraw_intent` (own only).
  No approve tool exists: `mcp/tests.rs` enumerates the tool list and fails on any name or
  description that implies approval or control, and calls every tool against a pending
  intent. Agent-sourced intents go through the engine: auto tiers dispatch, human tiers
  wait in the inbox (the inbox shows the agent's id and rationale); replay passes.
- **F.2 Agent runner** (`agents/scripted_agent.py`, done): a scripted agent on the official
  Python SDK that reads the fleet, previews and proposes with a rationale, and follows the
  result; `agents/README.md` says how a real model (Claude Code, other MCP clients)
  connects. No model is called from tests.
- Next: tracks in the agent's world view once C2 lands (`get_fleet` carries only our own
  nodes today); prompts and scenario runs for Phase G; real agent authentication. Read
  AeroMind's and Pharos's agent layers (DECISIONS.md, open-source neighbors) before the
  prompts.

Exit: agents never bypass a check in any test run (every MCP test replays its log; the
engine refuses an agent proposal without a rationale, and no tool reaches approval).

## Phase G: evaluation harness

Built (`eval/README.md` says how to run it, `eval/METRICS.md` defines every number). Revised
after an independent review (10-04) found the first report's claims did not hold: the proxy's
rules had been written per scenario, the agent arm was the human arm with another id, labels
came from the bridge's track age and from link state at proposal time, three premises were
contradicted by the runs, the two-signer check passed five forged logs, failed runs were
pooled, and a slow seed shifted every situation against its events. All fixed below; the
numbers further down come from the re-run.

- **Headless runner** (`eval/run.py`, Python): a TOML spec (`eval/scenarios/*.toml`) bundles a
  stand-in fleet, a contacts scenario, link and GNSS events, a policy variant, the clock, and a
  scripted **storyline** of injected situations (`propose`, `flood`, `declare`, `link` cuts
  appended to the run's own events file, `note`, `engage`). Each run gets an isolated stack on
  its own port block (vehicle and bridge UDP bases in 35000-36999, HTTP 10600-10699, reserved
  with lock files; SITL never), its own manifests and logs, a time limit, and is torn down by
  PID. Every clock of a run counts from one origin (45 s after the stand-ins start; a fleet
  not ready by then fails the run). Each run records the commit, a dirty flag and the
  binaries' hashes. Batch mode runs scenarios x arms x seeds several stacks at a time (`make
  eval`) with its own tidewater serve, and builds a self-contained HTML report
  (`eval/report.py`).
- **Arms**: `rules-only` (policy plus approve-all) and `rules-only-reject` (reject-all);
  `human-proxy` (a second operator proposes the storyline, the proxy decides);
  `agent+human-proxy` (an agent reads the picture over MCP and makes its own proposals by a
  fixed policy, the proxy decides); `agent+model` (a real Claude model through the MCP server,
  opt-in, worst-case cost checked before every call, per run and per batch, never in tests);
  `study` (a participant in the console). The proxy is a documented operator model: it reads
  a card with a probability that decays with the queue and, when it reads, applies a generic
  rule book (`eval/evidence.py`) to what the C2 shows. It is a model everywhere in the report,
  never human data.
- **Labels and categories**: derived from truth at the moment each is about (what a proposal
  targets when it is proposed; whether the tasked vehicle's link was cut when it is decided).
  The same rule book sorts every wrong proposal by what the C2 showed when it was made: "the
  C2 showed it" (catchable from the picture) or "picture wrong". The rule book was frozen
  before three held-out scenarios were written and not tuned to them; they were designed
  knowing it, each around one of its rules or a picture-wrong premise, and adjusted after the
  freeze, so their results restate premise times P(read) and are not a generalisation test.
  They are reported apart. Every step states its premise (what the run must show), checked
  against the record.
- **What was shown at decision time**: the console posts card open, close and decide events,
  with the tracks the operator opened while that card was the selected one (`web/src/operatorView.ts`), to
  `POST /api/operator/view`; the bridge logs them as `operator.view` (bounded, rate-limited,
  refused once the intent is decided; replay checks the session and decides nothing from
  them).
- **Metrics** from the audit log, the storyline log and truth: should-reject caught per
  category, correct-void rate, false rejects, override rate, discrimination d' on armed cards
  and on all, goodput (SandTable's definitions), approval latency, time to effect, and
  invariants that must be zero (an agent authorization, a human-tier intent auto-approved, an
  armed dispatch without a human, a dispatch with a failing check, an engagement dispatched or
  released without two signers in sessions they had opened with engagement authority when
  they signed, on one frozen plan, or not as that plan or by another platform, a dispatch
  that is not the approved intent's vehicles, task and parameters, all read from the log
  alone, a replay mismatch, and more).
  Intervals are clustered by seed (Student t over per-seed rates), pooled Wilson second.
- **Scenarios** (Golden Gate, seeded): development: misclassified neutral near the bridge,
  spoofed GNSS on our own aircraft, link cut during an approval, approval flood under a strict
  policy, a track whose contact left the picture, plausible but wrong proposals; held out:
  a friendly boat alongside the target, armed orbits before the picture settles, a neutral
  ferry with its AIS out.
- **Engagement scenarios** (Phase E): every batch runs the 39 scenarios of
  `sim/scenarios/engage/` through `eval/engage.py` (`murmur-scenarios --json` over seeds and
  track-noise levels: the real engine, its replay and the forged-log checks) and the
  harness's own invariants on every audit log. Live engagement steps in a storyline stay
  placeholders until the proxy can sign an engagement (two operators with engagement
  authority quoting picture tokens).
- **Results** (2026-10-04, `docs/eval/phase-g-report.html`, `phase-g-summary.json`): 9
  scenarios (3 held out) x 4 arms x 5 seeds, 180 runs on 54fa0fa, every run scored (none
  failed, timed out or missing), every fleet ready before its origin (1 to 3 s, or 30 s when
  a lost telemetry request waited for the bridge's re-send), zero invariant violations, every
  log replayed, 762 of 765 premises held (listed: two clean orbits decided inside a later link
  cut, one stopped by the geofence on a transit leg past a tower). The same 180 runs on the
  commit before the merge (5cebadf) matched these proposal by proposal in 175 runs; in the
  other 5 the agent took the fast boat and the speedboat in the other order (seed 4 of
  link-cut, spoofed-gnss and stale-track, moving one proposal per scenario between
  categories), and approve-all's misdescribed-ferry orbit was stopped by the reserves rule in
  another seed (2 of 10 either way). 36 runs on 0c7da6b matched these run for run. What they
  are: outputs of the proxy model. On the wrong armed
  orbits the C2 showed something against, it caught about the share of cards it read
  (plausible but wrong 9/10, a contact gone with its track shown stale 8/9, held-out friend
  alongside 8/10, held-out early unknown 7/10; under the flood 7/15, as its reading probability
  fell with the queue); on those the C2 showed nothing against, none (misclassified neutral
  0/10, spoofed GNSS 0/10, held-out ferry with its AIS out 0/10, the flood's speedboat 0/5):
  when it read, its rule book fired on every catchable card and on no picture-wrong one, by
  construction. That says what the picture lets a fixed rule book catch, not what people do;
  on the held-out scenarios it restates their premises, which were built around the rule
  book. Some catches came by another rule than the scenario's: the ferry and the cutter
  move, so the proxy often found no track left at the aim point of the orbits over them. The
  proxy rejected 1 of 10 right stale-track orbits, the one decided after the boat had left
  (right by truth then; 0 of 6 without the 4 such orbits, d' on armed cards 2.50 instead of
  2.13), and 5 of 10 held-out early-unknown ones, still unknown when it read them (the rule
  book's cost).
  Link cut, labelled at the decision: of 10 should-void cards the C2 voided 6, the geofence
  stopped 1, and 3 were approved inside a cut and dispatched to an aircraft that could not hear
  them (one 3.3 s into the cut, before the bridge's 11 s heartbeat limit; two clean orbits
  whose decisions fell inside the second cut). The agent arm ignores the storyline, so its 45
  runs are replicates of two situations (the development picture in six scenarios, the
  held-out picture in three), not nine scenarios; the report shows it that way. Its 43 wrong
  orbits the C2 showed something against were all shown at an affiliation confidence of
  0.31 to 0.48 at proposal, the only evidence, and it had firmed up by the decision (6.5 to
  34.7 s later on the development picture, 17.0 to 54.5 s on the held-out one), so the
  model's rule book fired on none of the 30 it read. Of its right orbits the model rejected
  6 dark-boat orbits decided after the boat had left (right by truth at the decision; without
  them its d' on armed cards is 0.00, not -0.64) and 12 of 15 late-raider orbits still shown
  unknown (the rule book's cost). It never proposes while the police boat is alongside in
  friend-alongside. Its cases are not comparable with the storyline's: what counts as
  catchable depends on the moment, and the report counts the labels that changed between
  proposal and decision (16 of 900) and shows false rejects and d' with and without them.
  Approve-all stopped only what the rules did; reject-all stopped everything, legitimate
  cards included (d' 0 for both).
- Engagement suite on the final code (release binaries as of 0c7da6b, harness checks as of
  this branch's head): 39 scenarios x 10 seeds x track noise 0, 3, 10 and 20 m, 1,560
  scenario runs: every one ended as designed and replayed, 520/520 forged
  logs caught, zero violations of the hardened two-signer and execution-to-approval checks
  (600 effects released).

Next: a cross-check that can catch a spoofed position (our own vehicles seen by our own
sensors); track-age rules for vehicle tasks aimed at a track; an approval re-check that
refuses a vehicle whose link went silent after the card was opened; the bridge confirming a
vehicle's telemetry plan (a lost SET_MESSAGE_INTERVAL waits for the 30 s re-send); study
sessions with participants (consent, anonymous ids: `eval/README.md`). Human-behavior claims
need participants; the harness supports studies, it doesn't replace them.

Exit: results published, including what went wrong.

## Alongside

- TAK/CoT export (murmur-dg2): after C2, when there are tracks worth sharing.
- Tidewater: SFBOFS currents (tw-ln7), wind bias correction, upstream the ArduPilot patch.
- Bugs: antenna tracker stall (murmur-9bf), ArduPlane extra streams.
- Polish: map label declutter (murmur-02g).
