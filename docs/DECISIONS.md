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
- **One frozen plan for every signature** (10-03, second Phase E review; replaces comparing
  each signature's own picture). The first signature freezes the plan it was shown: the
  effect point and time, radius, window, platform, fallback, and the target as identified
  then (`FrozenPlan`). Every later signer is shown that plan and signs it exactly; the
  approval's snapshot carries it and the release executes it, never a recomputed one (two
  signers can no longer approve points 2 km and a minute apart). At each later signature
  and every second while signatures wait, the engagement checks run on the frozen plan:
  the track still identified and declared as when it was frozen and predicted within the
  deviation tolerance of the plan's point at the plan's time, zone, no-strike and keep-outs
  on that point, the platform still able to be there by then (plus a slack: `late_s` for a
  holding track, the time to leave the tolerance for a moving one), and every signature
  within `max_signing_s` (120 s) of the first. When the plan stops holding every signature
  is dropped with it (`approval.reset`) and a new plan must be signed; a signature that
  finds it so is refused. Replay recomputes every plan check, reset and snapshot.
- **The frozen plan survives a wobbling velocity estimate, not a real turn** (10-04, third
  review). A tracker's velocity estimate moves by half a metre per second or more within
  seconds; carried a minute or two to the plan's time that moved the prediction tens of
  metres and dropped plans for nothing. Now: a target slower than `holding_speed_mps`
  (1.5 m/s) is predicted where it is; the track is held against where the plan's track path
  puts it now (as after approval), and carried to the plan's time against the plan's point,
  each tolerance growing with the track's uncertainty growth over the span
  (`sigma_growth_mps` times the time, at most `plan_drift_max_m`, 300 m) and with the two
  estimates' position noise (twice the combined `pos_sigma_m`, bounded by the uncertainty
  check); the detail logs every part. A turn, a reversal or getting under way still drops the plan at once; a
  small real drift inside the allowance is left to the release, which needs the track
  inside the effect radius of the plan's point (and otherwise aborts late with the
  fallback). The same allowance applies to the deviation check after approval.
- **Friendlies count with their prediction uncertainty; the effect has a time tolerance**
  (10-04, Phase G harness: scenario 10 at seed 8 and 20 m noise released with our AUV
  predicted 7 m outside the keep-out and still closing). One of our vehicles predicted
  along its velocity now counts with `keep_out_sigma` times an uncertainty growing at
  `friendly_growth_mps` (1 m/s) over the prediction span (it may turn or be retasked),
  failing closed (not 0.3 times its speed as for tracks: a fixed wing circling while it
  waits would then block every engagement within kilometres); after the plan's time the keep-outs are read at the release
  itself. A moving target may be released up to `plan_slack_s` before the plan's time when
  it is at the point (the same time tolerance as for arriving late), with the keep-outs
  read at that instant; a holding one waits for the time. Regression:
  `36-friendly-just-outside-keep-out`.
- **The platform's own failsafe stops its engagement** (10-04, live run). When the
  engaging platform reports a failsafe taking over (a GCS failsafe switching it to RTL,
  say), the bridge stops the engagement at once (cause `system`, the fallback commanded)
  instead of waiting for the picture to time out. A dispatched platform that leaves its
  mission mode (`AUTO -> RTL`) is stopped the same way: a later live run lost the failsafe's
  own message on the link that failed, only `GCS Failsafe Cleared` and the mode change
  arrived, and the task layer stops watching the mode once an orbit is on station.
  Only failsafe texts that say control changed stop it (fourth review): a switch to a mode
  other than AUTO, a disarm, a landing (`decision/failsafe.rs`, ArduPilot's texts per
  firmware); "Failsafe - Continuing Auto Mode", "RC Short Failsafe On" without an action,
  "Throttle failsafe off" and every "Cleared" do not, and texts that don't say what was done
  are left to the mode leaving AUTO.
- **A circling friendly is predicted on its circle** (10-04, fourth review; rules 3). One
  of our vehicles known to fly a circle (an orbit task on station and still in AUTO, a fixed
  wing in LOITER around the point NAV_CONTROLLER_OUTPUT names, or circling home in RTL) is
  reported with that circle (center, radius plus the tolerance it is flown to), and the
  friendly keep-out counts it anywhere on the disc, less the same growing uncertainty as
  any of ours. Carried along its momentary heading, a circling fixed wing pointed somewhere
  every lap, and the live demo's loitering BLADE blocked engagements kilometres away. A
  vehicle not known to circle is still carried along its velocity. Regression:
  `37-friendly-circling-near-target`. The engine trusts a circle only while it holds the
  vehicle (fifth review): the position, fresh by the usual link-aware rule, within the radius
  and 50 m of the center, and the center at most 12 s old (each judged on its own, so a
  reduced link's slower navigation target doesn't drop the circle); else the vehicle is
  predicted from its position (regression `38-friendly-off-its-reported-circle`). The disc
  includes the 50 m slack, so it is never farther than where the vehicle was reported, and
  at every span the vehicle is also carried 5 s along its velocity, the nearer counting: no
  cliff at a span boundary (regression `39-friendly-on-its-circles-slack-band`). The bridge takes a LOITER or RTL center from
  NAV_CONTROLLER_OUTPUT reported in that mode and at most 12 s old, stamps the circle with
  the older of the center and the position, and logs its source and center time.
- **A HIGH_LATENCY2 report without its GPS failure flag is a 3D fix** (10-04, fourth
  review). HIGH_LATENCY2 carries no fix type, so a platform on that link alone never had a
  fresh fix and could never release. The bridge now counts each one without
  HL_FAILURE_FLAG_GPS as a fresh 3D fix and logs where it came from (`gps_fix_from`), which
  the navigation check states (rules 3).
- **The effective signing window, by target speed** (10-04, third review). The plan gives
  the signatures `dispatch_delay_s` (20 s) before the platform was to leave, plus the slack
  it may arrive late (`late_s`, 60 s, for a holding target; the deviation tolerance over the
  target's speed for a moving one), plus, for a fixed wing that circles while it waits, the
  time to cross its circle (about 5 s for a Switchblade-class platform). With the defaults
  and a 75 m radius: about 85 s for a holding target, 34 s at 8 m/s, 30 s at 15 m/s
  (`max_signing_s` caps it). Past it the plan is dropped and signed again; a longer
  `dispatch_delay_s` buys time at the cost of a later, less certain effect point.
- **A first signer's stale picture asks them to look again** (10-04, third review). If the
  plan a first signer was shown no longer holds when they sign, but the nomination does on
  the world now, the signature is refused ("look again") and nothing is voided; only when
  the nomination itself fails its checks is it void, as for any approval.
- **One engagement per platform** (10-04, third review). A nomination on a platform with
  another engagement in play is rejected (`platform_free`), since its dispatch would stop
  the first without anyone acknowledging it; the scenario runner models the bridge's
  superseding too, and flags any engagement found in play at a dispatch.
- **A signer signs what the engine showed them** (10-03, second review). Before signing,
  the console asks for the picture (`POST /api/intents/{id}/picture`): the engine issues a
  token bound to the operator's session, with the plan (the frozen one, or the preview the
  signature would freeze), the track's identity, affiliation and declaration then, and an
  expiry (`token_ttl_s`, 15 s), logged as `picture.issued` with the world it was computed
  on. The signature quotes the token, once; it is refused if the token expired, is another
  session's, or its plan or track facts no longer match ("look again"). Replay checks each
  signature against the issued token and recomputes an unfrozen token's plan. This replaces
  a self-attested "seen" preview. The instant the engine checked the token at is logged
  (`checked_t`), and replay checks it lies within the signature's re-check and compares the
  expiry against it.
- **No agent order slips past an engagement in play** (10-03, Phase E review). An agent's
  order on a vehicle with an engagement pending, approved or under way goes to a human,
  whatever its tier says (`(engagement in play)`; the engine logs the engagements in play
  with each decision and replay recomputes them). An operator's own order (a hold, an RTL)
  still runs at once and supersedes the engagement: stopped with cause `superseded` (the
  intent that superseded it and its source), `fallback: null` (that order stands; nothing
  else is commanded).
- **An approval that stops an engagement says so** (10-03, second review). Approving an
  order (an agent's, or an older one) whose dispatch would stop an engagement in play on its
  vehicles is refused unless the approver acknowledges it; the acknowledged engagements are
  logged as `supersedes` on the approval and replay checks them against the engagements in
  play its re-check read. The console's card says which engagement it stops. An operator's
  own new order still runs at once (it is the newest human command, and the console shows
  the engagement on that vehicle).
- **Hold the picture, then act or stop** (10-03, Phase E). What the approvers saw (the
  track's facts and the preview) is stored with the final approval and re-checked every
  second until the effect; any change beyond tolerance voids it (before dispatch) or aborts
  it (after) and commands the recorded fallback. Never a silent continue: link loss is a
  void like any other (silence past the approval's assumption, or the platform's report
  interval more than doubling). Held every second or not at all: a picture more than
  `max_picture_gap_s` (5 s) after the last one fails `picture_gap` first, so a stalled loop
  aborts rather than resumes; the terrain fetch for pending nominations runs beside the
  loop, never in its way, and space queries (a week of Starlink coverage takes over a
  second) are answered before the decision lock is taken and used if still fresh (5 s), so
  no agent can starve the tick. Replay recomputes the gap from the logged picture times and
  flags an effect not released by a picture of the same step. After approval the platform
  must navigate on GNSS: dead reckoning (or no solution) fails `platform_nav` and stops it
  with its fallback, consistent with an unverified position blocking an engagement. Since
  ArduPilot keeps its absolute-position flag for seconds after GPS loss and while it rejects
  a glitching fix (a spoofed one is fused afterwards), the receiver's own fix
  (GPS_RAW_INT, 3D) and the filter's EKF_GPS_GLITCHING flag (HIGH_LATENCY2: its GPS
  failure flag) are logged facts of the vehicle and both must hold; unknown fails.
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
- **A silent vehicle's estimate is situational awareness, not a decision input** (10-04,
  murmur-4y9). The bridge's `estimate` module projects a silent vehicle from its last report
  (task, mode, velocity, else stationary) with a region that grows with the silence, and
  models the lost-link failsafe as a labelled second branch. It is sent to the console and
  the API only; the decision core keeps its own conservative friendly-uncertainty model
  (`keep_out_friendly`: along the velocity, else as far as cruise speed reaches), so a
  display guess can never tighten an engagement check. The snapshot carries the *model*,
  frozen at the last report and evaluated by the console every tick (as tracks are), so the
  change-only push sends a silent vehicle once per `NOW_STEP_S`, not per frame: a thousand
  silent nodes cost the bridge nothing per frame. Growth rates are stated GUESSes in the
  module header (about two sigma); `scripts/estimate_eval.py` measures containment.
  Agents read it through MCP as a labelled model with its age, never as a position fact.
- **Once the lost-link timer has fired, the failsafe branch leads** (10-04, murmur-4y9
  follow-up). A cut link is cut both ways in every case measured: the vehicle lost the GCS
  too and did what its firmware does (the boat turned home 30 s in, the aircraft 120 s
  in), so from the timer on the branch is drawn as the estimate and the task model is
  kept, dimmed, as the other hypothesis. Both stay on the map: the operator, not the
  bridge, decides which to believe when the cut was one way.
- **Drifters are carried along tidewater's current** (10-04, murmur-4y9 follow-up). A
  buoy's own motion is the water's, so its estimate integrates the sampled current (plus
  2 % windage, a GUESS) from its last known position rather than freezing (its catalog top
  speed is 0); without tidewater the bound is the documented 1 m/s.
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

## Evaluation (Phase G)

- **One isolated stand-in stack per run, Python runner** (10-03). `eval/run.py` starts the
  link emulator, bridge, stand-ins and contacts on a port block of their own (UDP 35000-36999,
  HTTP 10600-10699), with manifests, events and logs in the run directory, and stops them by
  PID; batches run several at once. Stand-ins only (SITL is neither deterministic nor cheap),
  and the same release binaries as `make goldengate`, so the evaluation measures the shipped
  decision layer, not a model of it.
- **Labels from truth, at the moment each is about; metrics from logs only** (10-03,
  revised 10-04). The storyline records what the C2 showed and what truth had when it
  injected a situation (the track as displayed, the contact behind it, what was within the
  task's radius of the aim point on both sides, the vehicle's real navigation offset).
  Should-reject comes from truth at proposal (the contact gone, neutral or friendly, the aim
  off the contact, a protected contact inside the radius, the vehicle's position off by over
  50 m), never from what the C2 showed (a stale track is not the label; the contact having
  left is); should-void from truth at the decision (the tasked vehicle's link cut then).
  Metrics read the records plus the audit log and the replay, never the runner's memory.
  Every step says why its label holds and may state a premise (what the run must show), which
  is checked and listed when it fails. Revised after an independent review found labels taken
  from bridge track age and link cuts judged at proposal time.
- **One generic rule book, frozen before held-out scenarios** (10-04). `eval/evidence.py`
  reads only what the C2 shows (failed or unverified checks; for an armed task, the tracks
  within its radius of the aim point as displayed: none there, any neutral or friendly, the
  nearest stale or below 0.5 affiliation confidence unless declared). The proxy applies it
  when it reads a card and the metrics apply it to sort wrong proposals into "the C2 showed
  it" and "picture wrong", reported apart and never pooled. It was frozen before three
  held-out scenarios were written and not tuned to them, but they were designed knowing it
  and adjusted after the freeze, each around one of its rules or a picture-wrong premise:
  their results restate premise times P(read) and are not a generalisation test. Revised
  after the review found the proxy's earlier rules written per scenario, so its catch rates
  echoed the scenarios' design.
- **The proxy operator is a documented model, never human data** (10-03, revised 10-04).
  Attention, reading time, a probability of reading that decays with queue length, and the
  rule book above; all parameters recorded per run and printed in the report. Its rates are
  presented as model outputs ("read r of n, rule fired on f"), and their intervals as Monte
  Carlo variation of the model across seeds, never as estimates about people; the queue and
  latency tables are labelled as its parameters coming back. Human claims need study sessions.
- **The agent arm proposes on its own** (10-04). `agent+human-proxy` used to pass the
  storyline's proposals through the MCP server, which made it the human-proxy arm with
  another source id. Its agent now reads the picture over MCP and chooses its own armed
  orbits by a fixed policy (`eval/agent.py` `AgentPolicy`, the same in every scenario); its
  proposals are labelled by truth like any other. It ignores the storyline, so the report
  shows it as replicates of two situations (one per picture), with each case's affiliation
  confidence at proposal and at the decision, and not alongside the storyline arms: its
  "C2 showed it" cases were low confidence at proposal that had firmed up by the decision,
  unlike the storyline's, whose evidence persists (second review, 10-04).
- **Intervals clustered by seed; one origin per run** (10-04). Proposals within a run share a
  stack, a picture and a decider, so the primary interval is over per-seed rates (Student t,
  exact quantile), the pooled Wilson interval second; study sessions cluster by participant.
  Every clock in a run counts from one origin, 45 s after the stand-ins start, and a fleet not
  ready by then fails the run: before this, readiness started the storyline while events and
  contacts ran on their own clocks, so a seed that started slowly (35 s, an acoustic AUV)
  shifted every situation against the events. The AUV left the evaluation fleets, and 45 s
  covers the bridge's 30 s re-send of a lost telemetry request.
- **Only finished runs count, and the code that ran is on the report** (10-04). A run that
  failed, timed out or left no `run.json` (checked against the batch's job list) is excluded
  and listed; each run records the commit, a dirty flag and binary hashes, and the report
  shows them. Port blocks and the batch's tidewater serve are reserved with lock files.
- **What the console showed is an audit event** (10-03). `POST /api/operator/view` logs
  `operator.view` (card open, close, decide; checks, warnings, rationale, tracks opened while
  the card was up, open time) under the operator's session. Additive: replay verifies the
  chain through it and decides nothing from it. Bounded, rate-limited per operator, refused
  for an intent no longer pending (so a late decide can't overwrite the one before the
  decision), and replay checks the view's session belongs to its operator (10-04). The
  console side is its own module (`web/src/operatorView.ts`).
- **One evaluation policy for every arm** (10-03). `eval/policy/base.toml` is `sim/policy.toml`
  with `submitter_authorizes = false`, so a second human's consequential proposals wait in the
  inbox as an agent's do; the strict variant routes every vehicle task to a human with no
  `source` condition. Arms differ only in who proposes and who decides. Everything else,
  engagement rules and tier included, is the shipped policy's, and a test holds them together
  (10-04).
- **Borrow SandTable's discrimination and goodput; define override ourselves** (10-03). The
  report's d' is SandTable's `trust_dprime` (log-linear corrected, 0 at the reject-all and
  approve-all corners) and its goodput is SandTable's `operator_goodput`; SandTable lists
  override rates but defines no formula, so Murmur's is its own (METRICS.md). Catch rate alone
  rewards rejecting everything; d' does not.
- **A wrong picture and a wrong proposal are reported apart** (10-03, made exact 10-04). Some
  should-rejects cannot be caught from what the C2 shows (a neutral that looks exactly like
  the hostile, a spoofed position with good flags); every should-reject number is split by
  the rule book above into catchable and picture-wrong, case by case, and no tile pools
  scenarios.
- **Engagements are measured by the scenario suite, checked from the log alone** (10-04).
  The 30 Phase E scenarios run deterministically through `murmur-scenarios` in every batch,
  over seeds and track noise. The harness does not trust the engine's verdicts: it reads each
  audit log and counts an engagement as a violation unless, at the dispatch and at the
  release, two distinct signers, neither the proposer, each in a session they opened earlier
  with engagement authority, signed one frozen plan before the dispatch, and the dispatched
  order and the released effect are that plan's point, time and radius; an engagement
  without its nomination in the log is a violation too. Hardened after the review forged
  five logs that the first version passed. Live engagement steps wait until the proxy can
  sign through picture tokens.
- **The real-model arm is opt-in and capped** (10-03, revised 10-04). `MURMUR_EVAL_MODEL=1`
  or the runner refuses; before every call its worst case (input plus the whole `max_tokens`)
  must fit under the run's cap (`MURMUR_EVAL_MAX_USD`, default $2) and the batch's
  (`MURMUR_EVAL_BATCH_MAX_USD`, default $20, a shared ledger), and what was billed is priced at
  the model that served each iteration (a fallback can bill another). The Anthropic SDK is not
  in `requirements.txt`; no test calls an external API.

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
- **Engagement thresholds are bounded** (10-03, Phase E reviews). Each `[rules.engage]`
  value must lie inside a fixed range (`EngageRules::validate`: no zero confidence, no zero
  keep-out, no unbounded age, gap, token lifetime or signing time), and so must the
  freshness every rule trusts (`max_fact_age_s` 1..120, `fact_age_intervals` 1..10) and the
  other rules thresholds, so a policy file can tighten the chain but not switch a check off.
  Replay runs the same validation on the policy logged in `run.started`: a log whose policy
  no file could carry is a mismatch.
- **Reclassification is any change of identity** (10-03, Phase E review), unclassified to a
  kind included: the approvers judged the track as it was classified then.
- **An engagement's effect point may take a near terrain sample, logged** (10-03, Phase E
  review). The effect point moves with its track every second, so its land/water answer may
  come from the nearest cached sample within 50 m; the logged sample says where it was taken
  and how far. Samples that near which disagree (a shoreline, a coverage edge) leave it not
  evaluated, which fails closed for an engagement. Every other waypoint is answered exactly.
- **Decision rules are versioned; old logs replay with their own rules** (10-04,
  coordinator). A rule change that alters decisions bumps `RULES_VERSION` and is gated on
  it; `run.started` records the version (none: 1) and the policy schema, and replay
  recomputes each log with the rules it was produced under. New policy keys are optional
  (absent from older logs, which re-serialize byte for byte); a renamed or removed key
  stays an optional field under its old name, not a serde alias: an alias parses the old
  name but writes the new one back, and replay requires each line to be its event's
  serialization. Committed logs of every version replay in `tests/versions.rs`; the
  changelog is in `crates/murmur-decide/README.md`.
- **An older rules version is accepted only when asked for** (10-04, fourth review). A log
  that claims rules 1 is judged by rules 1, so the claim is a downgrade path. Replay accepts
  only the current rules by default; `murmur-replay --rules N` or `--min-rules N` accepts
  older ones explicitly. It also flags a `run.started` that is not the first event, a log
  that carries what only later builds write (a `checked_t`, a GPS fact, a `platform_free`
  check, a build, a circle) under an older claim, and a policy with keys of a later schema
  than it claims. From rules 3, `run.started` names the build (commit, dirty flag). A
  rules-1 log carries no build and no anchors: accepting one is trusting its provenance.
- **Replay binds what was executed to what was approved** (10-04, sixth review). Forged,
  re-chained logs passed replay with every decision intact and the execution changed: an
  engagement's effect released by another vehicle, an engagement or an approved armed orbit
  dispatched to another vehicle, the orbit's centre moved 2 km. Recomputing decisions is not
  the claim; the claim is that what was done is what two signers (or the tier's human)
  approved. So every `intent.dispatched` must be exactly the approved intent's order (task,
  parameters, vehicles, and the space or engagement order recomputed as the engine makes it:
  the chosen option as the last answer has it, the frozen plan on the nominated platform);
  every `engage.effect` comes from that platform, at the plan's point and radius, released
  inside the plan's time tolerance; every `op.state` is a dispatched intent's, on its own
  vehicles, and a reported fallback is the one the stop released; a superseding order was
  dispatched to the platform, under its own source; a stop that quotes a failsafe follows
  that vehicle event on its own platform. These are audit checks, not decision rules: they
  apply to every rules version (every committed fixture and every log in `logs/audit`
  satisfies them, nothing needed gating), and the engine refuses the op states and
  supersedes they would reject. Not bound, stated: a vehicle event nothing quotes (replay
  doesn't know the fleet) and a task's done or failed against its op states (the task
  layer's report). The forged logs are regressions in `tests/bindings.rs`. A second review
  forged around the first bindings, and they were tightened (10-04): a superseding order is
  the one dispatched last and still under way (the bridge supersedes in the same step as
  that dispatch; an engagement aborted long before no longer qualifies); a failed picture is
  stopped for the picture, never relabelled as a system stop, and a system stop that quotes
  an intercept failure follows that report; after a newer order takes a vehicle an older
  intent reports only how its op ended, and `superseded` needs something that replaced it
  (op states can't be moved between orders on one vehicle); a fallback report names its
  task; rejections and at-submission approvals are under the decider's own session. The
  intercept's loiter radius, which the bridge chose and nothing logged, is now part of the
  engagement order (rules 4) and recomputed; a loiter fallback's centre and radius are still
  the bridge's and not logged.
- **Orders for unreachable vehicles are queued, re-checked, delivered once** (10-04,
  murmur-5p4; rules 5). An approved order is dispatched only when the link can carry it to
  every vehicle; otherwise it waits in the queue with a bounded, per-kind deadline (safety
  commands short: a hold or rtl delivered long after it was given may no longer be wanted).
  It leaves the queue only through a re-check on the world when its vehicles can be reached
  (logged, recomputed by replay): sent exactly as approved, back to the inbox when a human
  approved a picture that has since changed (an auto-approved order is sent if the policy
  still says auto), voided when a check fails. A human's approval is not stretched over a
  changed picture; the policy's is re-run. Gated on rules 5; older logs replay unchanged.
- **Reachable means heard and the picture fresh again** (10-04, found live). A link that comes
  back brings the heartbeat a tick before the position and navigation reports; a re-check
  then read the pre-outage picture, failed closed on its age and voided the order. For
  anything but a hold or rtl, a vehicle is reachable only once its navigation status and
  position are fresh by the checks' own limit. A mission also needs a link reporting at least
  every 5 s: an AUV on its acoustic modem can't take an upload (ArduPilot drops one after 8 s
  without the next item), so its order waits until it surfaces.
- **Exactly once is the bridge's job, by telemetry** (10-04). ArduPilot does not use
  COMMAND_LONG's confirmation field to drop duplicates, so a retry is sent only when neither
  the ACK nor telemetry showing the effect (a heartbeat in the mode, armed, MISSION_CURRENT
  on the first item) came back. A mission upload is a transaction whose commit is the start
  (current item, arm, mode): a lost final ACK is settled by asking the vehicle; after a link
  lost mid-way (waited out for the kind's queue wait) or a bridge restart, a full download
  compared item by item decides between following, starting only, and uploading again; a
  vehicle that had started an order is never sent it again.
- **A restarted bridge carries open intents over from its own log** (10-04). The log is the
  record; a small pointer file (`logs/audit/.resume-<http port>.json`: the log, the open ids,
  rewritten when they change) says where to look, and a bridge starting within 15 minutes
  replays that log and carries every open vehicle intent over as `intent.resumed`, keeping
  its id. Replay checks each against the previous log. The evaluation harness turns it off
  (`MURMUR_RESUME=0`): its stacks reuse port blocks.
- **A reboot ends the op; nothing is re-sent behind the operator** (10-04). A vehicle whose
  time since boot went back lost the mode and armed state its order set: its running op
  fails saying so and the intent ends failed, logged as `vehicle.rebooted`. A queued order
  approved by a human before the reboot goes back to the inbox.
- **The queue after its review: a lost link goes back through the queue, never around it**
  (10-04, review of d3ccb5d). An order sent whose link drops before the vehicle started it
  no longer waits in the executor for up to the kind's wait and then resumes unchecked: its
  op ends `requeued` and the order goes back to the queue (`intent.requeued`, replayed
  against the op states), where it is re-checked like any other, and the next delivery asks
  the vehicle what it holds first. An executor waits for a lost link only to follow an order
  its vehicle had started (after a restart), 120 s at most. The newest order for a vehicle
  stands: an older queued one is voided, never a newer one, and a newer queued order stops
  an older one's op that had not started. A human's approval is held 120 s at most
  (`[queue] human_expire_s`) and does not survive a bridge restart unseen; an agent's order
  on a vehicle with a human's order waiting needs a human. Carry-over happens only after a
  crash (a clean stop deletes the pointer, whose writer must also be gone) or by naming the
  log, onto the same platforms under the same uids, from a log that matches its anchors.
  "Started" is sticky across op states and restarts. A mission is ours only if command,
  frame, position, altitude and parameters match. A reboot during an outage longer than the
  uptime shows as the boot instant jumping later (more than 30 s, plus 5 % of the silence for
  a simulator that runs slow): residual, a vehicle up less than about 30 s before such an
  outage. Copters arm in GUIDED (not BRAKE). A command answered IN_PROGRESS is never sent
  again. Replay rejects a re-check at or after its deadline, a dispatch not right after its
  re-check, and (rules 5) a direct dispatch without the reach that allowed it.
- **The operator roster is logged and checked** (10-04, second review of the bindings; rules
  4). A log could credit a session, with engagement authority, to an operator the bridge was
  never configured with (`MURMUR_OPERATORS` and `MURMUR_ENGAGE_OPERATORS` were not logged).
  `run.started` now names the roster; the engine opens sessions only for operators on it,
  with engagement authority only for its engagement operators, and replay checks every
  session against it (a rules 4 log without a roster is a mismatch; the roster is a rules 4
  marker, so an older claim can't carry it). Every engagement operator must be an operator
  too, or the bridge does not start (before, an engagement operator outside
  `MURMUR_OPERATORS` was admitted anyway). The roster is fixed for the run. Rules 1 to 3
  logs carry no roster: accepting one with `--rules N` trusts its sessions. A roster
  rewritten together with the sessions is caught only by the anchors (it is in the first
  line).
- **The hash chain is tamper-evident, not tamper-proof** (10-03, restated by the second
  review). A forger who removes a self-consistent set of events (a transient reset, the
  signature it dropped and its picture token) and recomputes the chain leaves a log replay
  accepts; only comparing the head hash with an external anchor shows the rewrite. Since
  the fourth review the bridge writes run-time anchors (the first line's hash and the head,
  every 100 events and at shutdown) to `logs/anchors/<run-id>.anchors` and hands each to
  `MURMUR_ANCHOR_CMD`; `murmur-replay --anchors` checks a log against them, and the anchor
  file's own completeness (start, the declared cadence, a shutdown anchor last; the log is
  closed behind the shutdown anchor). A local anchor
  file is only as strong as the separation between it and whoever can write the log:
  protection against that writer needs a separate trust domain the command posts to, which
  is not part of Murmur (`murmur-decide/README.md`).
- **Still placeholders, stated** (10-03). Engagement authority comes from
  `MURMUR_ENGAGE_OPERATORS` and the `X-Murmur-Operator` header, as operators do from
  `MURMUR_OPERATORS`: a stand-in for real authentication (the lists are logged as the run's
  roster from rules 4, which bounds who a log can name, not who sent a header). The fallback is commanded by the
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
