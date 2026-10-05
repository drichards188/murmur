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
  (`MURMUR_ENGAGE_OPERATORS`; from rules 8 the credential's engagement flag, each signer
  re-entering their own password at signing), and its proposer, human or agent, never signs
  it. The engine holds a floor whatever `sim/policy.toml` says: never auto, never approved
  at submission, never the proposer, never fewer than two approvers (a tier asking for one,
  or a file with no engagement tier at all, is raised to two; replay holds the same floor,
  so a log whose policy or decision was edited to one is caught). This closes the gap left
  by "the operator's own command is the human decision": that rule stands for every other
  kind; for an engagement the submitter's own command is never the approval.
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
- **The approval's fallback is the platform's own lost-link action; no release without it**
  (10-04, murmur-der.2; rules 6). A fallback commanded over a dead link can't arrive, so the
  engagement order carries a preload: the platform's GCS failsafe set to the fallback
  (ArduPilot 4.8: Plane `FS_LONG_ACTN` 1 RTL; Copter `FS_GCS_ENABLE` 1 RTL or 7 Brake;
  Rover `FS_ACTION` 1 RTL, 2 Hold, 6 Loiter; Sub `FS_GCS_ENABLE` 3 depth hold), whatever
  would override it cleared (Copter `FS_OPTIONS` bit 1, Rover `FS_GCS_ENABLE` 2), its timer
  no longer than the approval's link-gap limit (5 s on a streaming link instead of 30 s on
  LEO or 120 s on GEO). The bridge reads the platform's own values, sets the preload, reads
  it back, and sends the intercept only once that confirms; one that doesn't stops the
  engagement with its fallback, and the release needs a confirmed read-back at most 10 s old
  (fail closed: a lost report means no effect): from 30 s before the release window opens
  the bridge reads it again every 5 s (`engage.preload` verify), and one that changed stops
  the engagement. One parameter job at a time runs on a platform's link. The platform's own
  values are owed back from the set on, and go back only when no engagement holds it: a newer
  engagement dispatched on it first takes them over (its set never reads the preloaded
  values as the platform's own), so a restore can't undo its preload. Restores are retried
  with backoff, every attempt logged (`platform.restore` when no engagement of the run owns
  them or the engine refuses one), with an inbox alert after 30 s; values still owed when
  the bridge stops are derived from its log at the next start, logged as carried and put
  back; replay reports any still owed at a log's end (review of 3318f25, findings 3 and 4).
  The write is logged ahead (`engage.preload` intent: the platform's own values and the
  vehicle's identity, before anything is written), so a crash between the write and its
  report still leaves them owed; carried values go back only to a vehicle that matches the
  one preloaded (platform, callsign, system id, home within 500 m), else nothing is written
  and the inbox says why; a start that can't read the last run's log says so too; and
  neither the engine nor replay allows a restore while another engagement holds the
  platform (second review, f389832). A
  fallback the firmware has no lost-link action for fails the nomination
  (`fallback_preload`): ArduPlane has none that holds where it is (continue, RTL, glide,
  parachute, AUTO, AUTOLAND), so a fixed wing takes `return` only (its "continue" would fly
  on to the effect point and circle there); ArduSub has no return. The stand-in copter now
  honours `FS_GCS_ENABLE` and `FS_OPTIONS` as ArduCopter does. Regressions: scenarios 08 (the
  USV holds where it is on its own after its link is cut), 40 to 44, 62 (changed before the
  release).
- **Keep-outs predict a track with acceleration noise, not with its speed** (10-04,
  murmur-der.3; rules 6). The tracker's growth law, `max(1, 0.3 x speed)` times the span at
  2-sigma, made a 20 m/s ferry 300 s out block 3.6 km around its predicted position while a
  slow boat that can turn on a dime counted as tighter. A protected track is now predicted
  along its velocity with `sqrt(tracker sigma^2 + (1 m/s x span)^2 + q span^3 / 3)`, q the
  domain's manoeuvre (0.2 m/s^2 on the water, 2 in the air) squared over the 1 s update:
  the continuous white-noise acceleration model (Bar-Shalom et al. 2001). The ferry's 2-sigma
  is 1.34 km, the same as a 6 m/s boat's. Basis: the Phase G velocity-noise sweep (0.5 m/s
  per axis passes, 1 m/s per axis aborts late), the Phase G live tracks (estimates moved at
  most 1.3 m/s second to second), a ferry's 300 m turn (0.33 m/s^2), a standard-rate turn
  at 55 m/s (2.9 m/s^2). Still conservative where it binds: the keep-outs are read every
  second, and at the release over seconds, where only the tracker's own uncertainty counts;
  a track without a velocity estimate is anywhere it reaches at its domain's top speed (it
  was held where it was). Regressions: 45 (a distant fast ferry), 46 (a near one still
  blocks), 47.
- **Keep-outs in 3D where both altitudes are known and ranged** (10-04, murmur-der.3; rules
  5). The keep-out reaches the effect radius plus `keep_out_vertical_m` (default
  `keep_out_m`, 200 m) above and below the effect's altitude (the target's estimate), each
  side counted with 2-sigma of its altitude error and how far it could climb or descend by
  then (ours: the largest of 10 m/s, the catalog's climb rate and, for a fixed wing, a 25
  degree dive at cruise speed, ArduPlane's default pitch-down limit: an MQ-9B's 38 m/s,
  either way; aircraft tracks 10 m/s; surface tracks none). A track's altitude counts only
  when a sensor that ranges it backs it (radar; a vessel's own AIS; sonar): passive sensors
  give none (the sensing model no longer hands EO/IR an air contact's exact altitude), and a
  ground track's 0 m is not its height above sea level, so ground targets stay 2D until a
  terrain-referenced altitude with a logged basis exists. An unknown, stale or unbacked
  altitude on either side leaves it 2D, as before. A track's altitude counts only while it is
  recent: the tracker carries its time and each source's last hit, a ranging sensor must have
  hit the track within `track_alt_max_age_s` (10 s, about ten radar revisits in the sensing
  model) and the vertical margin grows from the altitude's time, not the position's (second
  review: a radar altitude frozen while EO/IR kept the position fresh cleared an aircraft
  that could have descended to the effect). Only sensors that range a contact in height give
  one: a radar or AIS for a contact on the water, a 3D radar for an aircraft (our USVs'
  navigation radars are 2D), never sonar. Our fixed wings dive at their top speed. A MALE
  aircraft at 7 km over a surface effect no longer blocks; at 3 km, or a quadcopter at 120
  m, it still does. Our vehicles' altitude (MSL) is a world fact now. Regressions: 48 to 52,
  61 (a passive-only aircraft altitude), 63 (a ground target), 64 (a stale radar altitude).
- **A lost friendly blocks until an operator accepts it, explicitly and logged** (10-04,
  murmur-der.3; rules 6). Its reachable circle still grows without a cap (fail closed), but
  the check says since when its position is unknown, and an operator may accept it for one
  engagement ("vehicle X position unknown since T: accept"): at the nomination or while it
  waits with no signature (after one, the plan was signed without it), logged as
  `engage.accepted` under the operator's session with the vehicle's facts and the world's
  time (replay recomputes that it was lost since then), read by every check of that
  engagement and replay-checked. It covers that loss only: a vehicle that reports again
  counts like any other, a newer loss needs a new acceptance. It is an engagement decision,
  so it needs engagement authority (review of 3318f25, finding 1); every signer is shown
  it, on the card and in the picture token, and a signature on a token from before it is
  refused; a reset of the signatures clears it with the plan it was given for (given again
  on the new picture, not carried over silently); any operator may revoke it, which stops
  an engagement it now blocks. Never automatic and never an agent's: the engine takes it
  only with an `EngagementAuthority`, an agent's port has no such method and the bridge
  refuses an agent's request carrying one. The acceptor may also sign (two people still
  sign; barring it would need a third operator with authority on duty): the signature
  records it (`self_accepted`), the card shows it and the evaluation counts it. Regressions:
  53 to 60, 65.
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

- **The CCA's C2 link is SATCOM, as the other large aircraft's** (10-04, murmur-3zj). The
  stock fleets spawn it 21.5 km from Crissy Field at 11 km altitude, 24.2 km of slant range
  (the emulator's range is the slant range to the ground station), where the `los` class
  (0 up to 15 km, 1 at 25 km, from Puma's 20 km figure, a GUESS) already loses about 84 %
  of packets and the 5 s lost-link timer fires within minutes. Moving the spawn in would only defer it: at
  230 m/s the jet crosses the class's 25 km in under two minutes, so any sortie on a
  100 km theatre would RTL at the edge. A jet combat UAS tasked from a ground station tens
  of km away is on a BLOS link; the catalog maps it to WGS GEO (`space_class = "wgs"`,
  alternative ku-geo) like the RQ-4 and MQ-9B, with the GEO lost-link timer (120 s, a
  GUESS) and the reduced telemetry tier. Stated as a GUESS in the catalog: CCA datalinks
  are not public, and the control path in a fight, an air-to-air tactical link to a crewed
  lead, has no node in the fleet to carry it. Links no longer hinge on where a fleet file
  happens to put the aircraft. Scripted link events select by class, so the populations of
  `sim/link-events/golden-gate.toml` changed with it: the CCA leaves the `class = "los"`
  events (the 30 % loss study, murmur-t7d, and the LOS jammer at the Gate) and falls under
  the SATCOM jammer (half loss) instead; link-event runs before and after this change are
  not comparable on the CCA.
- **The Blimp stand-in's GCS failsafe is the firmware's: off, and a disarm when on**
  (10-04, murmur-3zj; supersedes the Blimp exception under "Lost-link timers follow the
  link"). ArduPilot Blimp 4.8's `failsafe_gcs_check` (Blimp/events.cpp) raises the
  failsafe flag and calls `arming.disarm`, armed or not, whatever `FS_GCS_ENABLE` says (the
  parameter documents "5: Land"; no `failsafe_gcs_on_event` exists to do it), and ships it
  off. The SITL Blimp has run it off for that reason while the stand-in announced "GCS
  Failsafe" and went to RTL or LAND, a safer airship than the firmware has. The gap is
  closed on the stand-in's side, since that is the honest model and a divergence would have
  to be re-documented at every firmware change: it ships `FS_GCS_ENABLE 0` too and, when
  the parameter is set, raises the flag (system status CRITICAL) and disarms ("Disarming
  motors"; the envelope then drifts with the wind), with no failsafe text, no mode change,
  and a silent clear. The bridge's estimate draws no lost-link branch for an airship, and
  the engagement preload refuses one for the same reason. Regression:
  `blimp_gcs_failsafe_is_off_and_only_disarms_when_on` (stand-in contract tests).
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
- **Every run starts as a fresh station: no restore pointer between runs** (10-05,
  murmur-kvb). Rules 6 has the bridge name its audit log in `logs/audit/.last-run-<http
  port>` and, at its next start on that port, carry over the lost-link values that log still
  owed back, or raise a High `restore:carry` inbox alert, held for the whole run, when it
  can't read the log. The runner moves each run's log into its run directory, so from the
  second run on a port block every run started with that alert (shown live: two runs of
  stale-track on one block), and a run whose runner died before its teardown would have had
  what it owed carried onto the next run's new stand-ins (nothing has been owed yet: only an
  engagement's preload is owed back, and live engagement steps are placeholders). The runner
  now removes the port's pointer before the bridge starts and after it stops, and records
  what it found in `run.json` (`restore_carry`). Chosen over a pointer location per run or a
  bridge setting that turns the carry off (each a bridge change, the second also a
  `run.started` field): the bridge runs as shipped and sees what any station sees on its
  first start on a port, and runs outside the harness are untouched. No published result was
  affected: the runs behind `docs/eval/` and PLAN.md (54fa0fa, 5cebadf, 0c7da6b) predate the
  pointer (52f928e); neither the scripted deciders (they read `pending`) nor the metrics (the
  audit log, where the alert never appears) read alerts, and the same seed with and without
  the alert gave the same decisions and metrics. Who could have seen it: a real model
  (`agent+model`, through `get_inbox`) and a study participant (the console's inbox); none
  of their runs is published.
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
- **Keep-outs are conservative, in 2D** (10-03; revised 10-04, rules 6: see the three
  keep-out entries under Direction). Every vehicle of ours (all domains) but the
  engaging platform, carried along its velocity to the effect time, or, without a fresh
  velocity, wherever it could get at cruise speed; every track but the target and tracks an
  operator declared hostile (`declared_by` set: an affiliation without a declaration is
  protected; neutral, unknown, suspect, assumed friend, stale ones too) with 2-sigma of its
  predicted uncertainty. Until rules 6 a vertical cylinder of unlimited height (an aircraft
  high overhead counted), a fast track's 2-sigma growing with its speed, and a lost friendly
  blocking with no way past; rules 6 read altitude where both sides know it, predict tracks
  with acceleration noise, and let an operator accept a lost friendly, each failing closed
  on missing data. Open design question (not decided here):
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
- **A start that went out unconfirmed may be running; it is never started again on its own**
  (10-04, second review of the queue). An ACK lost after the vehicle took the mission mode
  left the op looking unstarted, so a later delivery restarted a mission the vehicle's
  failsafe had stopped. The op now marks the moment its start goes out (`starting`: the
  mission mode, a hold or rtl mode, new items to a vehicle already flying a mission); a link
  lost after it ends the op `unconfirmed`, the order goes back to the queue, and its next
  delivery only follows it if the vehicle runs it; if it does not, a human decides (the
  inbox, saying the vehicle's mode). Every path that delivers an order sent before asks the
  vehicle first, a human's re-approval included. An order that ends while it may be
  running says so, keeps its tasking on the map, and a vehicle running it raises an alert.
  A carried-over order needs the same vehicle, not only the same platform (callsign and
  home within 200 m, from a fleet identity written beside the log), and a log whose anchors
  and fleet identity can be read (or `MURMUR_RESUME_UNVERIFIED=1`); every decision is
  anchored as it is written, so a crashed log is covered to its last decision. A reboot is
  judged at the vehicle clock's own rate (a slow simulator is not a reboot). An agent may
  not replace a human's order that no vehicle has started yet.
- **A start is logged before it goes out** (10-04, murmur-dma; rules 7, provisional
  number). The queue's `starting` was logged at the next decision tick, so a bridge that
  crashed within about a second of a start lost the record that it may have gone out, and a
  review probe restarted a mission over a failsafe RTL after such a crash. The op now logs its
  start itself, through the decision lock, before the mission mode, the hold or rtl mode, or
  new items to a vehicle flying a mission go out, and sends nothing if that write fails; a
  carry-over then always treats the order as may-have-started (confirmed only, never started
  again on its own). The other fix the issue offered, treating "held, not running, in a
  failsafe mode" as may-have-started on carry-over, was not taken: it guesses from the
  vehicle's state what the log can record exactly. Replay accepts the write-ahead and binds
  it: a vehicle task's start is logged ahead and nothing else is, and on an order's first
  delivery a vehicle reports taking it only after its start was logged, so a forged log that
  drops the write-ahead is caught, on every delivery (review of 1a3cadb: a redelivery after a
  requeue was not held to it), and a follow of an order the vehicle already runs is logged
  ahead the same way (from then on it runs it). The write waits for the decision lock off the
  runtime's worker (`block_in_place`), so it stalls no other vehicle's receive loop. The engine
  also refuses an op ending `requeued` after its
  start went out on that delivery (it may be running: it ends `unconfirmed`), which replay
  rejects in every log. Residual: a carry-over from a rules 5 or 6 log keeps the old window.
- **A mission waits for its platform's own lost-link values; a hold or rtl never does**
  (10-04, murmur-85l; rules 7). After an engagement ended its platform carried the
  engagement's lost-link action (its fallback on a few seconds' timer) until the restore
  confirmed, and the restore backed off through an outage (a probe: six failed attempts, back
  220 s after the link was) while the queue sent waiting orders on the first tick the link
  was back; a second short gap then braked, landed or returned the platform instead of its own
  30 to 120 s failsafe. Decided: the restore first, then the order. The bridge tries no restore
  into a dead link and tries one the moment the vehicle is heard again, whatever the backoff
  said; the queue holds an order other than a hold or rtl for a vehicle still owed values an
  engagement of the run preloaded, and sends it the tick after the restore confirms. Missions
  only: a hold or rtl is the safety command (a short queue, exempt from position freshness),
  holding it for a parameter exchange that may not finish could drop it, and the lost-link
  action it might meet in the seconds the restore takes is itself a safety action (brake,
  hold, loiter, return). The hold is bounded as any queued order's wait: a human's approval
  goes back to the inbox after 120 s saying why it was queued, an automatic order expires at its
  kind's wait, and the inbox's restore alert names the platform, its values, the attempts and
  the missions waiting. Nothing sends a mission over the engagement's lost-link action; an
  operator who wants the platform elsewhere meanwhile gives a hold or rtl. The hold is not the
  queue's to waive (review of 1a3cadb): a mission of a kind the policy never queues has nowhere
  to wait, so it is voided saying why and given again once the restore confirmed, rather than
  sent at once; every dispatch path refuses it, and replay flags a mission dispatched to a
  vehicle its ledger shows still owed, whatever the kind's queue. An operator's own
  mission for a platform whose engagement is still under way waits the same way (queued, it
  supersedes the engagement, below; the restore runs; then it goes, seconds later on a working
  link); a hold or rtl still runs at once. The fact
  (`restore_owed`) comes from the log's own ledger and replay recomputes it, so a forged log
  can't strip it. Values carried over from an earlier run's log hold no order (stated
  limitation): the bridge writes those back only onto the vehicle they were preloaded on,
  which the log alone can't tell, and another vehicle under the uid would otherwise be held
  for the whole run; they still go back at once when heard, alerted until they do.
- **An order queued for an engagement's platform supersedes it, as a dispatched one does**
  (10-04, murmur-636; rules 7). A newer order queued for the platform abandoned the
  engagement's unstarted intercept, the engagement read that as an intercept failure, stopped
  with cause `system` and commanded its fallback, which flew when the link returned while the
  operator's order still waited; dispatched, the same order stopped it as `superseded` with
  no fallback. Made consistent: the newest order stands, the stop says `superseded` (Phase G
  counts stop causes), no fallback is commanded, in the step that queues the order: replay
  takes the intent queued last as it takes the one dispatched last, and from rules 7, for
  both, only right after that order's transition with nothing else logged between (review of
  1a3cadb: a stop moved seconds later passed, so a picture or system stop could have been
  relabelled `superseded`); the bridge stops the engagements first in that step. No safety reason
  against it was found: a stop never releases anything and the chain's guarantees are
  untouched (two operators with engagement authority on one frozen plan, never an agent, a
  platform that leaves its intercept stopped before any release); only an operator's own
  order, or one a human approved acknowledging the engagement it stops, can be queued for its
  platform (an agent's goes to a human); and a platform cut off meanwhile still takes its
  preloaded fallback as its own lost-link action. What changes is the label and the bridge no
  longer commanding a fallback over the operator's order. The platform keeps its last order
  (or its failsafe's) until the queued one reaches it, possibly still loitering on the
  intercept path at the effect point with no engagement (nothing can be released); if the
  restore never confirms, the restore alert says so, and a hold or rtl takes it off; if the
  queued order expires instead, nothing is commanded. The same holds for a mission refused
  rather than queued (re-check of c577d5d): under a policy that never queues its kind it is
  voided while the platform is owed its own values, and it still stops an engagement in play
  there, `superseded` in the refusal's step, so the restore can run (before, it was voided
  and the engagement went on, while the same order dispatched or queued stopped it). Its
  reason says so, or that the restore is pending if the engagement had already ended, and
  that a hold or rtl goes at once.
- **A rules change is gated on a named constant** (10-04). Rules 7 is gated on
  `QUEUE_FOLLOWUPS_SINCE` (and `RULES_VERSION` is defined from it), never on a literal 7,
  because another branch (operator authentication) may take the next number first: the branch
  that merges second renumbers in one line and has its fixtures written again by the merged
  build (their `run.started` names the number).
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
  line). From rules 8 a credentials run's roster is the credential file when the bridge
  starts (an operator added later can log in only after a restart; one removed or changed
  can't act from then on), and it says how its operators are authenticated.
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
- **Operators are authenticated people; a consequential signature re-enters the credential**
  (10-04, murmur-der.1; rules 8, `OPERATOR_AUTH_SINCE`). Before, one person could declare a
  track, nominate it and supply both engagement signatures by switching the console's
  "Acting as" (a header the bridge believed), and a headerless request acted as the first
  operator, so no log could say a person decided anything. Now, in a credentials run
  (`MURMUR_AUTH=credentials`; every study), each operator
  has a credential (argon2id hash, OWASP parameters pinned: 19 MiB, 2 passes; at least 15
  characters, NIST SP 800-63B-4 for single-factor) in a file outside the repository
  (`MURMUR_CREDENTIALS`, else `~/.config/murmur/credentials.json`, mode 0600, written
  atomically; `crates/murmur-auth`, managed with the `murmur-auth` CLI, password on stdin),
  and the file's operators and their engagement flag are the run's roster. Login opens one
  engine session per login (`session.opened`, `auth: password`) behind an HttpOnly,
  SameSite=Strict cookie whose token the bridge keeps only as a digest; logout, 30 minutes
  without an action, 8 hours, and a removed or changed credential (each entry carries a
  stamp renewed on every change; the bridge re-reads the file by content every 2 s) end it,
  logged as `session.closed`, and replay refuses anything under an ended session. Every
  engagement signature, and any human tier the policy marks `step_up = true` (policy schema
  5), carries the signer's password again: the bridge checks it for the session's own
  operator, then `Engine::step_up` logs it and mints a `StepUp` (private fields, no Clone or
  Deserialize, compile-fail doctests) that the one signature consumes, at most 10 s old;
  without it the engine refuses the signature in a credentials run, and such a tier is
  never approved at submission. Why a step-up and not the session: a session can be stale
  (left open) or shared (someone else at the keyboard); the signature should be made with
  the credential at the moment it matters, as a hardware-token PIN is. Two people still
  means two credentials: the engine refuses a second signature by the same operator however
  they log in, and the proposer never signs. Five failed logins lock an id's next login (30
  s, doubling to 15 minutes; an attempt is reserved before its password is checked, so
  concurrent attempts can't outrun the lock), and each failed password for an operator on
  the roster is logged (`auth.refused`: the id, why, the lock; never a secret; attempts
  refused unchecked while locked, and ids not on the roster, go to stdout only, so no one can
  flood the audit log). Step-ups count against a limit of the session's own: five wrong in a
  row end that session, and only failures made under a session can end it (a stranger
  guessing at the login form signs no one out). An unknown id costs the same as a known one.
  A request carrying two cookies of the session's name is refused (another page on the host
  can plant one with another path; never guess which is meant). A browser's state-changing
  request or WebSocket from another origin
  is refused (Origin, else Sec-Fetch-Site): SameSite does not separate two ports of one
  host. Agents present their own key (`Authorization: Bearer`, SHA-256 kept, compared in
  constant time; `murmur-auth agent-add`) on every `/mcp` request, checked before an MCP
  session exists; they still have no path to an approval. `run.started` records the mode
  (`roster.auth`). A human's `intent.created` names the live session it was made in (an
  agent's none), so replay refuses an intent attributed to someone after their logout. The
  console's display reports (`/api/operator/view`) only peek at the session: instrumentation
  does not keep an idle session alive. Reads (`/api/state`, the WebSocket feed, tracks) stay
  open on the local port: they are the picture, not authority.
- **The lockout leaves a denial of service on the next login, stated** (10-05, review of
  rules 8). Anyone who can reach the port can type five wrong passwords for an operator's id
  and lock that id's next login for 30 s (doubling, up to 15 minutes). We accept it: the
  alternative (no lock, or a lock per source address on a single-host testbed where every
  client is 127.0.0.1) gives an online guesser unlimited tries. What the lock no longer does
  is reach a live session: step-ups have their own per-session limit, so a signed-in signer
  keeps signing. The failed logins are in the audit log, so a run where it happened says so.
- **Harness-held credentials are scripted, not people** (10-05, review of rules 8). The
  evaluation's runner logs its scripted operators in with credentials it generated; those
  sessions authenticate a script, not a person. A credential can be marked `scripted`
  (`murmur-auth add <id> --scripted`, or `scripted <id> on`): its logins are `auth:
  scripted`, admitted in a credentials run, step-ups included, and replay lists their
  operators apart (`ReplayReport::people` vs `scripted`). A study marks every operator but
  its participant scripted, and its evidence is the participant's own acts only, signed in
  with their own password; the scripted operators' acts never count.
- **Approvals carry over only between runs that authenticate alike** (10-05, review of rules
  7). A crash-resumed run carries open intents with their approvals. An approval made on
  asserted ids (a dev run, or any log before rules 8) is no person's in a credentials run, and
  a person's has no place in a dev run's counts. So the bridge, `Engine::resume` and replay
  refuse a carry-over whose previous run authenticated otherwise (its `ResumedFrom.auth`
  against this run's mode; a log before rules 8 counts as dev), in both directions; nothing
  is carried, and the operators decide again.
- **Evidence about people needs the log's anchors too** (10-05, review of rules 8). A
  rewritten log replays as well as the original. The evaluation keeps each run's anchors
  with its log (`audit.anchors`), replays with `--anchors`, and counts a study run only when
  they hold (`anchors: verified`); `murmur-replay` without `--anchors` says on a credentials
  log that its anchors were not checked.
- **What authentication can't see, stated** (10-05). One person holding two credentials (two
  roster ids) signs as two people: the two-person rule is two credentials, and who holds
  them is the study's procedure (one participant, one id, the others scripted and marked).
  Two sessions in one browser are possible through separate cookie jars (127.0.0.1 and
  localhost are different hosts to the browser), and from the bridge they look like two
  logins. Neither is detectable from the log.
- **A dev mode stays, explicit and excluded from claims; it is the default** (10-04,
  murmur-der.1; default 10-05). A run is dev unless `MURMUR_AUTH=credentials`: `make up` and
  `make goldengate` need no credential file. Dev keeps the header identities (and the
  console's switcher, in the top bar beside a "DEV: IDS NOT AUTHENTICATED" pill) for
  development, scripted tests, the e2e scripts and the evaluation's proxy arms, whose deciders
  are scripts anyway; the evaluation's study mode always runs with credentials. Why dev by
  default: most runs are development, a run that needs a credential file first would push
  people to share one, and a dev run can never be mistaken for evidence (it says so in the
  log). The bridge says so in a banner
  at start, `run.started` logs `auth: dev` and every session `auth: asserted`, replay prints
  "DEV ... exclude from any human-control claim" (`ReplayReport::authenticated` is false),
  and the evaluation's metrics carry the mode: a study run counts only when its participant
  signed in with their own credential, its log replayed and its anchors held. Logs of earlier
  rules record no mode and count as asserted. Engine-only runs (scenarios, tests) log `dev`.
- **Second factor: password step-up now, passkeys before real participants** (10-05). A
  consequential act re-enters the password today. Passkeys (WebAuthn: a key bound to the
  device and the origin, not phishable, not shareable by telling) come later and are required
  before a study with real participants; until then a study run is a pilot (study mode says
  so at start).
- **Which acts step up besides signatures is a policy switch** (10-05; the scope is not yet
  decided). Every engagement signature steps up, and any human tier marked `step_up = true`
  (the armed-platform tier is one line away). `[approval] step_up_acts` (policy schema 5)
  adds others: `declare_hostile` (declaring a track hostile, what makes it a target) and
  `accept_unknown` (accepting a lost vehicle for an engagement's keep-out; a nomination's
  acceptances share one step-up). Off by default, as built; replay requires the step-up on
  each listed act in a credentials run, and the console asks for the password where it
  applies.
- **Still placeholders, stated** (10-03; revised 10-04 by rules 8). Who holds engagement
  authority is now each person's credential, not a header (above); what is still not here:
  a second factor (a password is single-factor; passkeys are planned, above), a blocklist
  of common passwords, TLS (the bridge listens on 127.0.0.1; `MURMUR_COOKIE_SECURE=1` marks
  the cookie Secure behind an HTTPS proxy), signed or remotely anchored logs (the hash chain
  and anchors are tamper-evident only), and per-person identity on the reads. Until rules
  5 the fallback was only commanded by the bridge after a stop, so a platform that lost its
  link en route followed its own lost-link failsafe; from rules 6 it is preloaded as that
  failsafe (see above). A bridge restarted mid-engagement puts the platform's own failsafe
  back from its last log's ledger (`.last-run-<port>` names that log); the engagement itself
  does not survive the restart.
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
- **Agent identity is a header, like the operator's** (10-02; replaced 10-04 by per-agent
  keys, rules 8). `X-Murmur-Agent` (or the MCP `clientInfo.name` inside a session) against
  `MURMUR_AGENTS`: placeholder authentication, stated as such, so that evaluation could
  attribute proposals then and real auth could replace it without changing the tools, as it
  did: a credentials run takes `Authorization: Bearer <key>` (above), a dev run the header.
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
