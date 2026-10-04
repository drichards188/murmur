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
  (murmur-4y9); intents queued for unreachable vehicles with expiry (murmur-5p4); fallback
  rules per action class; telemetry priority under low bandwidth (murmur-f6q).

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
   (track predicted within tolerance at the plan's point and time, zone, no-strike and
   keep-outs there, the platform still able to make it, the same identity and declaration,
   all signatures within `max_signing_s`); when it stops holding, every signature is dropped
   with it (`approval.reset`) and a new plan must be signed. Compile-fail doctests: no
   forged, cloned or deserialized engagement authority, a plain operator authority is not
   one, and an `AgentPort` has no approve or abort method for engagements.
5. **Hold the picture**: the final approval stores an `EngageSnapshot` (track facts, the
   preview, the platform's link). `Engine::step_engagements` (the bridge's 1 Hz loop)
   re-checks it every second (`engage.picture`): declared, reclassified, recency, deviation
   from the snapshot's prediction, keep-outs at the effect time, the platform's link against
   the approval's assumption, GNSS navigation (`platform_nav`), the window and lateness, and
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

State machine additions: `dispatched -release-> effected -complete-> done`,
`dispatched -abort-> aborted` (terminal). `finish` and `dispatch` refuse engagements.

**Adversarial scenarios** (`sim/scenarios/engage/`, format in its README; runner
`murmur-scenarios`, library `murmur_decide::scenario`): 30 files, one or more per invariant
(agent authorizes, proposer approves, double signature, neutral enters the keep-out,
reclassified, undeclared, target moves, link cut, friendly sub near and closing, stale
track, outside the zone, no-strike, inferred suspect, operator abort, persists, unarmed,
deleted signature, window too short, a policy file without the engagement tier, the target
moving between signatures and stale previews, an agent's order on the engaging platform, a
10 s gap in holding the picture, approvers stripped from the log, the effect point sliding
and the target reversing between signatures, a transient reclassification, a picture taken
before a reclassification, an approval that would stop an engagement, GNSS lost en route).
Each ends in reject, void or abort as designed and replays; the tampered logs (including a
loosened logged policy) are caught. Seeded noise (independent around the truth each step)
turns each into a batch (Phase G); seeds 1, 2, 3, 7, 9 and 42 all pass.

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

Built (first cut; `eval/README.md` says how to run it, `eval/METRICS.md` defines every number):

- **Headless runner** (`eval/run.py`, Python): a TOML spec (`eval/scenarios/*.toml`) bundles a
  stand-in fleet, a contacts scenario, link and GNSS events, a policy variant, the clock, and a
  scripted **storyline** of injected situations (`propose`, `flood`, `declare`, `link` cuts
  appended to the run's own events file, `note`, `engage`). Each run gets an isolated stack on
  its own port block (vehicle and bridge UDP bases in 35000-36999, HTTP 10600-10699; SITL
  never), its own manifests and logs, and is torn down by PID. Batch mode runs scenarios x
  arms x seeds several stacks at a time (`make eval`) and builds a self-contained HTML report
  (`eval/report.py`) with confidence intervals, per-scenario tables, invariants and a
  "what went wrong" section.
- **Arms**: `rules-only` (policy plus approve-all) and `rules-only-reject` (reject-all);
  `human-proxy` (a second operator proposes, a scripted proxy operator decides);
  `agent+human-proxy` (the scripted MCP agent, `agents/policy_agent.py`, proposes);
  `agent+model` (a real Claude model through the MCP server via the Anthropic API, opt-in,
  cost-capped, never in tests); `study` (a participant in the console). The proxy is a
  documented operator model (attention, reading time, diligence decaying with queue length,
  a fixed rule book), labelled a proxy everywhere and never reported as human data.
- **What was shown at decision time**: the console posts card open, close and decide events
  (`web/src/operatorView.ts`) to `POST /api/operator/view`; the bridge logs them as
  `operator.view` (additive; replay verifies the chain through them and decides nothing from
  them).
- **Metrics** from the audit log, the storyline log and truth: should-reject catch rate,
  correct-void rate, false rejects, override rate, discrimination d' and goodput (borrowed from
  SandTable), approval latency against the information shown and the queue, time to effect,
  and invariants that must be zero (an agent authorization, a human-tier intent auto-approved,
  an armed dispatch without a human, a dispatch with a failing check, an engagement without
  two approvers, a replay mismatch, and more).
- **Adversarial scenarios** (Golden Gate, seeded): misclassified neutral near the bridge,
  spoofed GNSS on our own aircraft, link cut during an approval, approval flood under a strict
  policy, stale track, plausible but wrong proposals. Phase E's engagement scenarios (target
  moves after approval, neutral enters the keep-out, friendly sub near the target, proposer
  approves its own nomination) are placeholders in `eval/scenarios/engagement/` behind the
  plug-in point `eval/engage.py`, skipped until the bridge takes engagements.
- First batch (2026-10-04, `docs/eval/phase-g-report.html`): 6 scenarios x 4 arms x 3 seeds,
  72 runs (a full batch takes 39 min on 8 parallel stacks; stale-track was rerun after a
  scenario fix, as the report says), every run ok, zero invariant violations, every log
  replayed. With the proxy deciding, wrong armed orbits were caught where the card and the
  track picture show the error (plausible but wrong 4/6, stale track 4/6) and not where the
  picture itself is wrong (misclassified neutral 0/6, spoofed GNSS 0/6); under the approval
  flood 2/12, with the queue at 12. A cut link voided the pending tasking in 4/6 cases; the
  other two were approved 2.5 and 6.1 s into the cut, inside the 10 s freshness window, and
  dispatched to a silent aircraft (the mission upload then timed out).
  Approve-all caught nothing; reject-all caught everything and rejected every legitimate card
  (d' 0 for both). These are a scripted proxy's numbers, not human data.

Next: wire the engagement scenarios when Phase E lands; a cross-check that can catch a spoofed
position (our own vehicles seen by our own sensors); track-age rules for vehicle tasks aimed
at a track; an approval re-check that refuses a vehicle whose link went silent after the
card was opened (the link-cut window above); study sessions with participants (consent,
anonymous ids: `eval/README.md`).
Human-behavior claims need participants; the harness supports studies, it doesn't replace
them.

Exit: results published, including what went wrong.

## Alongside

- TAK/CoT export (murmur-dg2): after C2, when there are tracks worth sharing.
- Tidewater: SFBOFS currents (tw-ln7), wind bias correction, upstream the ArduPilot patch.
- Bugs: antenna tracker stall (murmur-9bf), ArduPlane extra streams.
- Polish: map label declutter (murmur-02g).
