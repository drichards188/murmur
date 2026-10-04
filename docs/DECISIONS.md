# Decisions

What was decided, and why, so a new session doesn't relitigate it. Newest first within each
group. Dates are 2026.

## Direction

- **Open-source passion project exploring C2 at AutoWarCom scale** (10-02). Goal: show how human
  judgment over consequential decisions can stay meaningful, measurably, when one operator
  supervises ~100 nodes. Not commercial.
- **Why defense first** (10-02, user's reason): defense is the highest-stakes environment for
  autonomy and C2 practices (contested links, adversaries, irreversible consequential
  decisions). Practices that hold up there transfer more easily to commercial uses such as
  delivery fleets; the reverse is not true. Defense is the proving ground, not the end goal.
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
- **The operator's own command is the human decision** (10-02, user direction: "im the only
  one who is clicking the UI ... the ai will interface through an api. those are the ones
  that need human approval"). A console (or any operator-authenticated) submission in a
  human tier is approved at submission (`approval.at_submission`, not `auto_approved`), so
  the operator never waits on themselves; agent proposals in those tiers still wait in the
  inbox. Checks still run first and fail closed. `submitter_authorizes = false` on a tier
  restores the wait. Phase E added the genuinely different second approver
  (`approvers`, `proposer_may_approve` per tier; always for an engagement, see below).
- **Engagement: two operators with engagement authority, never the proposer** (10-03,
  Phase E). An engagement (an abstract effect: a point, a radius, a time) waits for
  `approvers = 2` distinct signatures from sessions opened with engagement authority
  (`MURMUR_ENGAGE_OPERATORS`), and its proposer, human or agent, never signs it. The engine
  holds a floor whatever `sim/policy.toml` says: never auto, never approved at submission,
  never the proposer, never fewer than two approvers (a tier asking for one, or a file with
  no engagement tier at all, is raised to two; replay holds the same floor, so a log whose
  policy or decision was edited to one is caught). This closes the gap left by "the
  operator's own command is the human decision": that rule stands for every other kind; for
  an engagement the submitter's own command is never the approval.
- **Every signature is on the same picture** (10-03, Phase E review). Each signature stores
  its own snapshot (the track's facts and the preview, from that signature's re-check), and
  carries the preview its signer was shown (`seen`: effect point and time, and when the
  bridge computed it; the console sends the one on screen). The engine refuses a signature
  whose preview is older than `seen_max_age_s` (15 s) or whose effect point is beyond the
  deviation tolerance from its own ("look again"). At each later signature, and every
  second while signatures wait, the earlier signatures' pictures are compared with the
  track now: a different track, identity or declaration, or the track beyond the deviation
  tolerance from where an earlier signature's picture predicts it, drops every signature
  (`approval.reset`, with why and the world it read) and the nomination needs its signers
  again. The effect point itself is not compared (it slides along a moving track's path
  while people decide); every signature re-runs the zone, no-strike and keep-out checks on
  the point it would send the platform to. Replay recomputes each signature's snapshot and
  every reset.
- **No agent order slips past an engagement in play** (10-03, Phase E review). An agent's
  order on a vehicle with an engagement pending, approved or under way goes to a human,
  whatever its tier says (`(engagement in play)`; the engine logs the engagements in play
  with each decision and replay recomputes them). An operator's own order (a hold, an RTL)
  still runs at once and supersedes the engagement: stopped with cause `superseded` (the
  intent that superseded it and its source), `fallback: null` (that order stands; nothing
  else is commanded).
- **Hold the picture, then act or stop** (10-03, Phase E). What the approvers saw (the
  track's facts and the preview) is stored with the final approval and re-checked every
  second until the effect; any change beyond tolerance voids it (before dispatch) or aborts
  it (after) and commands the recorded fallback. Never a silent continue: link loss is a
  void like any other (silence past the approval's assumption, or the platform's report
  interval more than doubling). Held every second or not at all: a picture more than
  `max_picture_gap_s` (5 s) after the last one fails `picture_gap` first, so a stalled loop
  aborts rather than resumes; the terrain fetch for pending nominations runs beside the
  loop, never in its way. Replay recomputes the gap from the logged picture times and flags
  an effect not released by a picture of the same step.
- **Space as taskable infrastructure** (10-02, user direction): request coverage and
  capabilities from satellites when needed; don't fly or command satellite buses (no CCSDS).
  One theater map for earthbound nodes, plus a space timeline and a globe inset; one camera
  can't show LEO orbits and a sub at 30 m meaningfully.
- **Not an "open Lattice"** (10-02). Sensor fusion and vendor integrations are out of reach
  solo; Open-RMF is the wrong base (indoor robots). Focus on the tasking and supervision layer.
- **Stop 3D and Gazebo work here** (10-02). 3D is good enough to understand a scene; it is not
  the differentiator for C2. Gazebo stays a possible tidewater adapter, deferred.

## Fleet and simulation

- **Engagement effects are abstract, on the truth side only** (10-03, Phase E). The bridge
  hands the effect (point, radius, time) back to the contacts process on the labelled
  simulation channel (in the answer to its next `POST /api/sim/contacts`). Contacts inside
  the radius are flagged with the time; `on_effect = "out_of_play"` (default) takes one out
  of the scenario (our sensors stop detecting it, which is all the assessment can see),
  `"persists"` keeps it moving. What was inside, whether the intended contact was, and
  collateral (other contacts, our vehicles by their reported positions; one of ours that
  never reported a position is listed as unknown, not clear) go to the evaluation truth log
  only. The engaging platform is named apart and never counted as collateral. No damage,
  blast or lethality model of any kind.
- **A fleet vehicle never heard from is in the world model** (10-03, Phase E review). The
  bridge's snapshot holds every vehicle the fleet manifest names, with its platform and no
  facts until it reports: an engagement's friendly keep-out cannot clear a vehicle with no
  position, so it fails closed (ZEP 1 behind the Marin Headlands blocks every engagement
  until it connects; that is the model).
- **Our vehicles' velocity is the autopilot's** (10-03): GLOBAL_POSITION_INT's vx and vy,
  stamped with the position; HIGH_LATENCY2 has none, so there its heading and ground speed.
- **The Magura-class USV is armed in the catalog** (10-03): its role is "small fast attack
  USV"; with `armed = true` every task on it needs a human, and it can take an engagement.
- **Assessment from perception** (10-03): after the effect, our vehicles whose catalog
  sensors (radar, EO/IR, sonar by domain; not AIS or ESM) reach the effect point at their
  nominal range cover it. Detected after the effect (beyond a grace for the truth side's
  tick): persists; covered `assess_dwell_s` without a detection: ceased; else unknown.
- **Lost-link timers follow the link, failsafes stay on** (10-03, murmur-nb1). One table,
  `lost_link_s` per profile in `sim/links.toml` (5 s LOS and tether, 10 s mesh, 30 s LEO,
  120 s GEO, Iridium and acoustic; sources and guesses marked), written by `sim/swarm.py`
  into every vehicle's parameters, SITL and stand-in alike (FS_LONG_TIMEOUT on Plane,
  FS_GCS_TIMEOUT elsewhere). SITL gets the GCS failsafe on as the stand-ins have it (Blimp
  excepted: 4.8 disarms on it). The GCS heartbeat has its own thread in the bridge, and the
  stand-in engine skips a stall longer than 1 s instead of running its vehicles' clocks
  through it.
- **Truth and perception in separate processes, on a labelled channel** (10-02, Phase C2).
  Contacts that are not ours run in `crates/murmur-contacts`, not in the stand-in engine:
  stand-ins are our vehicles and speak their real protocol; contacts have none, and the
  C2 must learn about them only through the sensing model. The contacts process posts
  truth to `POST /api/sim/contacts`; the bridge keeps it in the `tracks` module, reads it
  only for sensing, and serves it back only on `GET /api/sim/truth` with
  `MURMUR_EVAL_TRUTH=1` (evaluation). A unit test asserts no truth field reaches
  `/api/tracks`, the WebSocket, the decision snapshot or CoT.
- **Hostile is declared, never inferred** (10-02). The tracker derives unknown, suspect,
  neutral and assumed friend from identity evidence (AIS, classification, speed
  plausibility, conflicts); hostile takes an operator's declaration, logged as an
  `operator.action`. The same discipline as "agents don't authorize": the consequential
  label is a human act.
- **Sensing runs in the bridge, association is by contact id** (10-02). One seeded model
  in the bridge (which already loads the catalog and knows our vehicles) rather than in the
  contacts process; detections are associated to tracks by the simulation's contact id
  (private, never serialized) as a stated simplification; a dropped track seen again
  gets a new id. A gating tracker, terrain masking and clutter are future work.

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
- **Golden Gate currents from NOAA's SFBOFS model, not station interpolation** (10-02). Against
  three harbor current meters SFBOFS scored 0.15 m/s speed RMSE vs 0.23 for station IDW (leave
  one out). Needs `tidewater fetch` with the `sfbofs` extra (~730 MB download, 26 MB cache);
  `source = "coops-predictions"` in the scenario reverts. No meter exists at the Gate itself.
- **Tidewater is a separate repo** (10-01): a simulator-agnostic conditions engine; Murmur is
  its first consumer. Golden Gate, 13-16 June 2026 spring tides. Past windows replay
  **observed** water level (predictions ran 18 cm low).
- **ArduPilot patch `SIM_WATER_DEPTH`** (10-01), branch `tidewater-water-depth` in
  `~/ardupilot`, patch file in `~/tidewater/patches/ardupilot/`. Lesson: stop vehicles with
  forces the simulated IMU feels; kinematic stops (zeroing velocity, clamping position)
  diverge the EKF.

## Engineering

- **The engagement chain is in the engine, as pure functions of logged inputs** (10-03,
  Phase E). Checks, preview, snapshot, picture watch, release, observation and assessment
  live in `murmur-decide` (`engage.rs`) and every result is logged with its inputs, so
  `murmur-replay` recomputes the voids and the assessment, not only the checks. The bridge
  feeds the world each second (`Engine::step_engagements`) and carries out what the engine
  releases (an intercept, a fallback, an effect). A picture verdict is logged every second
  while an engagement is in play (`engage.picture`), so a suppressed void is detectable: a
  failed picture must be followed by its stop before anything else happens to it.
- **Engagement authority is a type** (10-03). `EngagementAuthority` exists only inside an
  `OperatorSession` opened with `Grants { engage: true }` (logged with the session);
  `approve_engagement` takes nothing else, and `approve` refuses an engagement. Two-person
  approval is signatures in the engine (`approval.concurred` until the last,
  `approval.granted`), checked again by replay against the logged sessions.
- **No homing, no aim-point optimisation** (10-03). The preview is a straight-line intercept
  at catalog cruise speed against the track's constant-velocity projection; the platform is
  sent to loiter at the predicted point; the effect is released only when the picture holds,
  the predicted time has come, the platform is at the point and the track is estimated
  inside the radius. If not within `late_s`, it aborts.
- **Keep-outs are conservative, in 2D** (10-03). Every vehicle of ours (all domains) but the
  engaging platform, carried along its velocity to the effect time, or, without a fresh
  velocity, wherever it could get at cruise speed; every track but the target and tracks an
  operator declared hostile (`declared_by` set: an affiliation without a declaration is
  protected; neutral, unknown, suspect, assumed friend, stale ones too) with 2-sigma of its
  predicted uncertainty. A vertical cylinder of unlimited height: an aircraft high overhead
  counts. Known costs, left as they are: a fast track's 2-sigma grows with the time to the
  effect, so a fast neutral far away can block (conservative by design); a friendly
  without a fresh velocity grows at cruise speed with no cap, so a long transit can be
  blocked by a vehicle that will never come near. Open design question (not decided here):
  another declared hostile inside the keep-out is exempt like the target, so an effect may
  be released with a second declared hostile inside the radius; whether that should need
  its own signatures is for the study.
- **Whole circle, not the point** (10-03, Phase E review). The effect circle must lie
  entirely inside one engagement zone (its centre at least the radius from the zone's
  edge) and entirely clear of every no-strike area.
- **Engagement thresholds are bounded** (10-03, Phase E review). Each `[rules.engage]`
  value must lie inside a fixed range (`EngageRules::validate`: no zero confidence, no zero
  keep-out, no unbounded age, gap or preview age), so a policy file can tighten the chain but
  not switch a check off; the values in force are logged with the policy.
- **Reclassification is any change of identity** (10-03, Phase E review), unclassified to a
  kind included: the approvers judged the track as it was classified then.
- **An engagement's effect point may take a near terrain sample, logged** (10-03, Phase E
  review). The effect point moves with its track every second, so its land/water answer may
  come from the nearest cached sample within 50 m; the logged sample says where it was taken
  and how far. Samples that near which disagree (a shoreline, a coverage edge) leave it not
  evaluated, which fails closed for an engagement. Every other waypoint is answered exactly.
- **Still placeholders, stated** (10-03). Engagement authority comes from
  `MURMUR_ENGAGE_OPERATORS` and the `X-Murmur-Operator` header, as operators do from
  `MURMUR_OPERATORS`: a stand-in for real authentication. The fallback is commanded by the
  bridge after a stop; it is not preloaded into the vehicle as its lost-link action, so a
  platform that loses its link en route follows its own lost-link failsafe (the per-link
  timers above), not the engagement's fallback, until the link returns.
- **Unknown endurance on an engagement: a weaker check, stated** (10-03). Most armed
  platforms have no endurance figure; engage is consequential (an unverified check would
  block it), so `reach` passes on a fresh battery above the reserve and a transit of at
  most `unknown_endurance_max_transit_s`, and its detail says the task cost was not
  estimated.
- **Adversarial scenarios at the decision layer** (10-03). `sim/scenarios/engage/*.toml`
  (a small world, a timeline, the designed end, optional log tamper) run against the real
  engine by `murmur-scenarios` and `cargo test`; log attacks recompute the hash chain so
  only recomputation can catch them. The Phase G harness loads the same files (seeded track
  noise for batches). The live stack runs the same engine with stand-ins and contacts.
- **Agents nominate engagements through `propose_intent`** (10-03). No new writer tool (the
  only writers stay `propose_intent` and `withdraw_intent`); `get_nominations` is a read.
  The forbidden-name test now also tries engagement names (approve, abort, release,
  declare); nothing an agent can call signs, stops or releases one.
- **The nomination form's dry run is not a decision** (10-03). `POST /api/engage/options`
  runs the engagement checks and preview for every armed platform on the world now, creates
  nothing and logs nothing; the decision is the nomination and its signatures, each
  re-checked by the engine. It lists platforms by uid with no ranking and no recommendation,
  and the form preselects nothing: the operator picks the platform.
- **The MCP server lives in the bridge, on in-process `AgentPort`s** (10-02, Phase F). Not a
  separate process over HTTP: there is no agent HTTP path, so the only way for agent code to
  reach the engine is the typed `AgentPort`, and "no approve tool" is the absence of a method,
  not a route left unregistered. Official Rust SDK (`rmcp`, streamable HTTP at `/mcp`), hand-
  written tool table (one place to audit, one test that enumerates it). Satellite reads reuse
  the bridge's own `/api/space/*` handlers in process rather than a second copy of the space
  API.
- **Agent identity is a header, like the operator's** (10-02). `X-Murmur-Agent` (or the MCP
  `clientInfo.name` inside a session) against `MURMUR_AGENTS`: placeholder authentication,
  stated as such, so that evaluation can attribute proposals now and real auth can replace
  it later without changing the tools.
- **Preview is a logged dry run in the engine** (10-02). `AgentPort::preview` runs the rules
  and policy on the same world view a submission would read and creates nothing; it is
  logged as `agent.action intent.preview` (what the agent considered) but its inputs are
  not, since there is no decision to replay. An agent's `propose` also logs
  `agent.action intent.propose` with the rationale before the intent's own events.
- **Every fact an agent reads carries its age** (10-02). The agent view is built from the
  decision layer's own `WorldSnapshot` (the facts and timestamps the rules read), with
  `as_of`, `age_s` and `stale` against that vehicle's link-aware freshness limit, so an
  agent and the rules never disagree about what is fresh.

- **Rust for the live path, Python where its libraries and audience matter** (10-02). Rust:
  bridge, protocol gateway, stand-in engine, link emulator, the Phase C decision layer, the MCP
  server. Python: tidewater (scientific data, finished), evaluation analysis, small scripts.
  Console stays TypeScript; ArduPilot patch C++. Why: Claude writes and reviews the code and the
  user supervises, so Rust's writing cost mostly disappears while the compiler becomes a second,
  tireless reviewer; "agents cannot authorize" can be enforced by types, not only tests; Rust
  is strongest across MAVLink, DDS (UMAA) and wrapping the C protocol code (JAUS, 4586, JANUS).
  Rust bridge port happens before Phase C so C isn't written twice.
- **Python bridge and stand-ins retired (R.5)** (10-02). The Rust bridge and stand-in engine are
  the only implementations; `bridge/`, `sim/standins.py` and the Python-vs-Rust parity scripts
  are deleted (in git history). Python stays for the fleet launcher (`sim/swarm.py`, which
  still starts the SITL binaries), test and measurement scripts, and tidewater. `up.sh` builds
  the release binaries (a no-op when fresh) and, with a scenario, starts `tidewater serve`
  itself instead of relying on one started by hand. Every platform carries a stand-in
  `behavior`, so any fleet can run without ArduPilot.
- **Vehicle identity: `uid` (fleet-unique) vs `sysid` (MAVLink, per link)** (10-02). A MAVLink
  system ID is one byte; the 300-node test crashed above 255. The API and console key on
  `uid`; `sysid` wraps 1..250 (clear of GCS IDs 255 and 190) and is unique per link.
- **Stand-ins never simplify the interface** (10-02): see `docs/AUTHENTICITY.md` (the stand-in
  contract and, per platform, what maximum authenticity would take).
- **MapLibre pinned to v5** (10-02): deck.gl 9.4's adapter needs `map.transform` and
  `getFreeCameraOptions`, removed in v6 (crashed with terrain on). Revisit when deck.gl
  supports v6.
- **Telemetry plan** (10-02): bridge requests 7 messages with `SET_MESSAGE_INTERVAL`;
  `sim/params/common.parm` zeroes ArduPilot default streams (`MAV1_*`, not `SR0_*` in this
  version). WebSocket sends full state once, then changes only.
- **Readiness trusts the vehicle's EKF flags** (10-02): submerged AUVs have no GPS fix.
  A fixed-wing on dead reckoning (10-03) is `unverified`, not failed: ArduPlane flies on
  without GNSS, so a plane in a jammer can be tasked out of it, with a human judging it.
- **Tasks use platform cruise altitude and minimum turn radius** from the manifest (10-02).
- **Bridges lifted in 3D** by estimating deck height from the highest abutment along the
  bridge line (10-02). Documented in `web/src/bridges.ts`.

## Repo and process

- **Local git remotes only** (`~/git/*.git`) (10-02). Open: a GitHub remote (user decision),
  and the commit email (global config is a work-looking address; user to decide).
- **bd issues exported to `.beads/issues.jsonl` by the pre-commit hook** (10-02): the Dolt
  database isn't in git.
- **No AI attribution in commits; no em dashes in prose** (user's global CLAUDE.md).
