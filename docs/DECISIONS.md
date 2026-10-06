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
- **The engine may declare hostile by rule, and flags what it is unsure of** (10-05, user
  direction: minimize manual approvals; the system decides where the evidence is clear,
  flags for human review where it is unsure, and evaluation runs show what really needs a
  person; rules 9, `RULE_ID_SINCE`; supersedes "Hostile is declared, never inferred", 10-02,
  under Fleet and simulation). The engine declares a track hostile by its hostile-act rule
  when one of our sensors observed a hostile act against one of our vehicles or a track we
  protect, attributed it to that track at the policy's threshold or more (0.9 shipped), and
  nothing conflicts (the track is not already declared otherwise, an operator has not revoked
  a declaration of it, its own estimate is not neutral or assumed friend with confidence).
  The declaration is the engine's, logged as such (`ident.declared`, `by: rule`, with the
  act, the observing sensors, the time and the attribution), never as an operator's, and it
  is a non-blocking review item: any operator confirms it or revokes it, under their own
  session. What the engine is unsure of it flags and never declares: an attribution below the
  threshold (two tracks close together, a loose fix), conflicting evidence, an author we hold
  no track of, a victim that is not ours, one observation where the policy wants two, and the
  weak evidence of keeping company with a declared hostile (shown suspect to readers, never
  further, and never hiding the track's own evidence from a decision). Any operator may still
  declare any track; any operator may revoke any declaration (user decision, 10-05), and its
  declarer is told when someone else did, with the optional note. Why it is acceptable now: the declaration is a deterministic rule over
  logged facts that replay recomputes (an agent's judgment would not be), it is shown as the
  rule's everywhere it travels (the picture token, the frozen plan, the engagement card, MCP
  reads), and the floor below does not move. **What stays human, in code and by type**:
  releasing an effect needs two distinct operators with engagement authority signing one
  frozen plan, never the proposer, never an agent; accepting a lost friendly is an
  engagement-authority operator's act; agents cannot declare, confirm, revoke, resolve a
  flag, sign, stop or release (the methods are absent from `AgentPort`, compile-fail
  doctests). With `[identification] rule_declaration = false` the engine decides exactly as
  rules 8 did (a test runs every earlier scenario both ways).
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
- **A tasked satellite is followed after dispatch; a cover is judged over its area** (10-06,
  murmur-0nv.1.7, rules 11 `SPACE_FOLLOWUPS_SINCE`; from the collections review). Before,
  a dispatched collect or cover could not be cancelled, the only "missed" was the clock's
  (a pass over at dispatch or jumped past), and a cover was read at the area's centroid.
  Decided: (1) an operator cancels a dispatched collect or cover like any waiting intent,
  and the engine itself states what the cancel means for the pass (`op.state cancelled`,
  recomputed by replay): a request withdrawn before its pass collects nothing; a pass that
  has come cannot be recalled (nothing here or in a real tasking system takes an exposure
  back), so the scene counts as collected and only its product is dropped; a cover's watch
  ends. The intent ends `aborted` (transition `cancel`, kept apart from an engagement's
  `abort`), and an agent withdraws nothing after dispatch, as under the existing policy (an
  agent may withdraw its own proposal while it waits; after dispatch only an operator
  stops anything). (2) A later space answer re-checks every running task, and the only
  later answer this system has is a reloaded segment (new element sets on disk: the bridge
  watches the cache and reloads), because the answer is deterministic in the segment and
  the query; a collect is re-queried at the request time it was tasked at, since a fresh
  "now" would only show the lead time spent and call every tasked pass too soon. A pass
  still there within the match tolerance is the same opportunity (the approval stands); a
  pass gone is `missed` and the intent failed, with the fresh options still taskable at the
  check's own time in the report for a human: the engine never re-chooses, as agents
  nominate and humans authorize. A collect whose pass has come is never re-checked: it was
  collected, and a later answer cannot un-collect it. An answer that could not be computed
  is no answer: the task runs on and the operator hears of it. The session's time shift
  stays pinned across a reload, and the pin is part of the segment's identity (a re-pin is
  a new segment). (3) A cover is evaluated at the centroid and the boundary (eight bearings
  around a circle; a polygon's vertices, thinned to eight by geometry when there are more:
  the farthest from the centroid first, then each time the one farthest from those chosen,
  so the far tips are kept; a closed ring's first vertex counts once) and holds only where
  it holds at every point: SATCOM and GNSS coverage vary on the scale of a footprint
  (hundreds of km), so for an area of at most 200 km radius the extremes of a convex area lie
  on its boundary, which assumes the field is near linear across the area. Every satellite is
  propagated once for all the points (one window search driven by the centroid's cone
  widened by the area, one propagation per sample), so nine points cost about one; a grid
  fine enough to add anything would cost more and tell no more. The logged answer holds each
  point's figures and no aggregate, so replay recomputes the verdict; a query whose points
  are not the area's is not the intent's question, and an answer whose result points are not
  the query's (one dropped, moved or copied over) is not the answer. (4) The space query
  stays outside the decision lock (the 10-03 decision above): a cover is never computed by
  the engine (a week of Starlink over nine points held the lock 13 to 14 s in the review,
  which an agent's dry run could repeat; the check fails closed without a prepared answer),
  a prepared answer is used only if the segment and pin that answered it are the engine's
  (its age is no concern for a cover, whose query does not depend on when it was asked), and
  the bridge prepares again outside the lock when the segment moved in between.
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
- **Hostile is declared, never inferred** (10-02; superseded 10-05 by "The engine may
  declare hostile by rule", under Direction). The tracker derives unknown, suspect,
  neutral and assumed friend from identity evidence (AIS, classification, speed
  plausibility, conflicts); hostile took an operator's declaration, logged as an
  `operator.action`. What still holds: the tracker never infers hostile from identity
  evidence. From rules 9 hostile is an operator's declaration or the engine's rule on an
  observed hostile act, both in the engine's identification ledger (`ident.declared`).
- **A hostile act is an abstract event the sensors observe** (10-05, rules 9). A contacts
  scenario scripts an act (`[[act]]`: a time, an author, a victim, one of ours or another
  contact); nothing models a weapon or damage. It is truth: the bridge's sensing model
  decides which of our sensors observed it, with the same range, horizon, domain and
  detection probability as a detection (radar, EO/IR and sonar; not AIS, a broadcast, nor
  ESM, a bearing), one seeded draw per act and sensor so a run replays, and the observer with
  the smallest position error places the act's author with that error drawn as for a
  detection; every other observer gives its own fix (its best sensor, its own draw), for
  corroboration. Simplifications, stated: the fixes are not fused (the best one is the act's
  location; fusing several observers' fixes is later work), the victim known exactly when it
  is ours, an act against a contact named by the track the association gives it (the
  contact-id association of 10-02), and an act nobody observed simply absent from the C2's
  picture (the evaluation view lists it).
- **The simulation channel takes the stack's own token** (10-05, review of rules 9; scope
  corrected at the second review). Truth reaches the sensing model, and through it acts
  reach the identification rule, so `POST /api/sim/contacts` is no longer open: it carries
  `X-Murmur-Sim-Token`, compared as digests in constant time with the bridge's
  `MURMUR_SIM_TOKEN`, checked on the headers before the body is read; without it the request
  is refused (401); with no token set the channel is closed. `up.sh` (from the caller's
  `MURMUR_SIM_TOKEN`, or a fresh one per stack) and the evaluation's stacks give it to the
  bridge and the contacts process only: `up.sh` takes it out of the environment every other
  process inherits (the first build exported it, so the stand-ins, the emulator, `swarm.py`
  and vite had it too), and it is never printed or written to a run directory. What it stops:
  a client that can only reach the bridge over HTTP (the review had injected a fishing boat
  and an act with only an agent's key and got a rule declaration). What it does not stop: a
  process running as the same user, which can read it from the bridge's or the contacts
  process's `/proc/<pid>/environ`. Each act logs the channel it came in on (`sim:contacts`).
- **The tracker is a two-model IMM, honest on motions it was not tuned on, and a keep-out
  counts where a manoeuvring contact could go** (10-05, user decision after the review of
  rules 9: a live track reported 8 m while about 20 m off; reworked after the second review
  found the first Kalman filter honest only on the motions its noise was tuned to, and
  keep-outs a minute ahead weaker than before on manoeuvring contacts). Each track is an
  interacting multiple model filter (Blom and Bar-Shalom): two constant-velocity Kalman
  filters per horizontal axis, steady and manoeuvring (white-noise acceleration, m^2/s^3:
  surface 0.01 and 2, air 0.4 and 80, subsurface 0.002 and 0.4, ground 0.06 and 12; air,
  ground and subsurface scaled from the surface values, not checked), switching as a Markov
  chain (a manoeuvre every two minutes or so, lasting half a minute); every detection a
  position measurement with its sensor's 1-sigma, an AIS report's course and speed a
  velocity measurement. Each fix weighs the two models by how well each predicted it, and
  the estimate and covariance are their mixture, which widens while they disagree. A fix
  outside the manoeuvring model's 99% widens that model's prior by its own size (a stop from
  speed within a second); the steady model keeps its own, so an outlier does not drag the
  estimate. The reported covariance is the mixture's widened by the recent fixes'
  consistency (their normalized distance from the prediction, averaged over about three
  fixes, over 1.5 where 2 is expected): a run of fixes farther off than predicted means the
  filter is behind a manoeuvre it has not taken in yet. Chosen over adaptive process noise
  alone because the IMM's mode probability is a likelihood ratio, standard for manoeuvring
  targets, and gives the manoeuvre evidence the keep-out needs; the consistency widening
  was added because the two models alone stayed overconfident in the seconds after an
  instant stop or turn. A minute ahead the filter's velocity is the current leg's, which a
  zigzag or a weave does not keep (the old filter's velocity, an average of the last minute
  or so, predicted those better), so the track also gives the engine (rules 9,
  `TrackFacts`) its average velocity over the last minute (`mean_velocity_mps`) and, when its
  velocity changed beyond its noise (2 m/s and a 99.9% chi-square) or the manoeuvring model
  was all but certain (0.8) within the last 5 minutes, the fastest it went
  (`manoeuvre.speed_mps`); a keep-out holds only if the effect point clears every region: along
  the velocity (rules 6), along the average velocity, and, for a manoeuvring track, anywhere
  it reaches at that speed. Acceptance (`tracks::tests::calib`), on four sets disjoint in
  seeds: the review's classes (holding, transit, a stop after a transit, a 90 degree turn,
  circling, a zigzag, a sprint and stop) and more (acceleration, weave, random legs, random
  turns, a patrol box), each run on the new filter and on a replica of the one before the
  review with the same detections; criteria per class when updated: the truth within two
  sigmas at least 83% of the time and the mean normalized error at most 2.6 (expected 2);
  a keep-out 60 and 100 s ahead holding every case at least as often as the old filter's.
  History, stated: the first two-model constants failed the first held-out set (zigzag,
  sprint and stop, weave and random legs overconfident), which became tuning data; the tuned
  filter passed the second held-out set when updated, but its keep-out held a 60 degree
  zigzag 70% of the time against the old filter's 92% (the average-velocity prediction was
  added for that), and it became tuning data too; the third held-out set, written before
  that change was run, passed on its first run: every class 85% or more within two sigmas,
  NEES 1.0 to 2.0; keep-outs at least as often as before in every case (zigzag 89% against
  52% at 60 s, sprint and stop 96% against 87%, weave 93% against 39%, holding and transit
  100% against 91 to 97%). The first Kalman filter of this review fails the same test
  (zigzag 72% when updated, NEES 3.7; sprint and stop 62%, 4.2; weave 59%, 5.5; its keep-out
  held the 45 degree zigzag 25% of the time at 60 s against the old filter's 52%). Cost,
  stated: a steady track's keep-out is about the same size (168 m against 163 m at 60 s,
  2-sigma), a manoeuvring one's much larger (a zigzagging boat's 870 m), and steady tracks'
  reported sigma is loose (NEES about 1.1: a sigma 1.3 times the error), conservative by
  design. The live run's coast guard cutter (AIS, circling 1.2 km), 79% within two sigmas
  with the first Kalman filter and 4% with the IMM at first, was the clock: on this host
  (WSL) the bridge's wall clock steps back about 0.94 s every 32 s while the contacts
  process's does not (measured: 9 steps in 5 minutes, 8.2 s in all), and the tracker dated
  each fix by its arrival, so at every step it predicted 0.06 s of motion where the contact
  had moved a second; with AIS velocity known to 0.3 m/s the stiff steady model then stayed
  metres off for many seconds while its sigma said 1.5 m. Fixes are now dated by the truth
  message's own clock (`Detection::measured_t`; staleness reads the bridge's clock, which no
  longer steps either: murmur-dup, under Engineering); a test
  reproduces the steps (dated by arrival: NEES 21.7, 5% within two sigmas; by the message: honest).
  Live with it (dev run, 7 minutes after a minute's settling, 2099 samples): every contact 93
  to 99% within two sigmas, NEES 0.84 to 1.55 (the cutter 92.9%, 1.55; the AIS ferry 99.3%).
  Keep-outs and attribution read this 1-sigma. Fusing several observers' fixes of one act,
  and a gating tracker, are later work.
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

- **Rule against manual identification is measured on generated situations, and what it
  can and cannot say is stated** (10-05, rules 9; rebuilt after the first review found the
  first comparison circular, and its wording corrected after the second). `python -m eval.run
  ident` generates situations per seed from stated distributions (a hostile author, tracked or
  not, the untracked share a stated dimension run at 0.2 and 0.5; none to three contacts
  near it, neutral, unclassified, friendly or hostile, each holding or under way as stated;
  one or two observers) that name no policy, and plays each under five arms, the shipped
  policy changed only by flags: manual (rule off), rule, rule with nomination before
  confirmation, and both with corroboration by a second observation. The same scripted
  operator reviews in every arm (`scenario::ReviewProxy`: a look every 20 s, 6 s to read,
  reads 80%, slips 5%); a nominator nominates every declared track as soon as the policy lets
  it, and scripted signers sign whatever is nominated. What it cannot say, stated with the
  numbers: the proxy's procedure is close to the rule's own (the best fix, two combined
  sigmas, the victim excluded, a track shown neutral or friendly at 0.5 left alone), so their
  agreement is built in and the manual arm says nothing about how accurately a person
  identifies; with confirmation required each rule declaration still costs a person one
  decision (by design, the shipped policy does not reduce approvals); the rule's latency
  advantage and what nomination before confirmation saves are the proxy's look interval and
  reading time; a proxy that reads half its items makes the manual arm worse while the rule
  arms hold, because a skimmed card is confirmed unread (a model assumption); and the signers
  never refuse, so a declaration the C2 should not have made is carried to an effect. Workload
  is the proxy's own tally, counted alike in every arm (one decision per item, whatever it
  decided). Its numbers are model results from scripted proxies on generated situations (dev
  runs, asserted ids), never human data; scenarios 72-78 illustrate the rule and are not part
  of it.

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
  the alert gave the same decisions and counts (latencies within run-to-run jitter). Who
  could have seen it: a real model
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

- **The reviews' follow-ups are one rules version, gated on a named constant** (10-06,
  rules 12, `REVIEW_FOLLOWUPS_SINCE`). The six follow-ups below change what is logged or
  decided, so they share one version; every gate reads the constant (and `RULES_VERSION` is
  defined from it), never a literal. The space tasks' follow-ups took 11 first (merged
  10-06), so these are 12: one line renumbered, the fixtures written again by the merged
  build. No policy key changes (schema 7 stays). Logs of rules 1 to 11 replay unchanged with
  their own rules; the tests of the rules 6 to 10 carry are pinned to rules 10.
- **A platform spent by its own effect is owed nothing back; an operator may write off what
  never comes back** (10-06, murmur-q05; rules 12). A loitering munition's own lost-link
  values were owed back for good after its effect (the passkeys live run: the switchblade
  never answered, replay said UNRESTORED, the inbox alerted for the rest of the run, and the
  next run carried it over), since only a confirmed restore closed the debt. Decided, both:
  the catalog marks a platform spent by its effect (`expended_by_effect`: switchblade and fpv;
  not magura, whose variants are not all one-way), and the engine closes what is owed to it
  right after its engagement's release (`restore.closed`, `expended`, naming the engagement),
  automatically, since there is nothing to put the values on (minimal approval: no person is
  asked to confirm the obvious); and any operator may write off a platform's values (lost,
  destroyed, another vehicle under the uid) or a failed carry, under their own session with a
  note (`written_off`, `POST /api/restores/write_off`, the console's Write off), never while an
  engagement is under way on the platform, never an agent. Replay binds both (exactly the
  values owed; an expended closure only right after that engagement's release, with its
  release picture showing the platform spent; a write-off under the operator's own session).
  In the simulation the munition flies on with the preloaded failsafe (a safety action); the
  bridge stops trying to restore it. A write-off records a judgement about the fleet and
  authorizes nothing, so it needs no step-up.
  After the review (M1): an expended closure marks the platform spent, a logged state, and a
  spent platform takes no order and no nomination: `readiness` fails for it (the decision's
  `expended` fact, checked against the ledger by replay), every dispatch path refuses it, an
  order approved before the effect is voided saying so, and replay flags any order sent to it.
  Without that, the closure left a live, taskable platform carrying the engagement's lost-link
  action: an orbit went at once with no restore, and a second engagement's preload would have
  read the first one's fallback as the platform's own values, past the rules 6 guard. The
  other option, closing only once the platform is no longer heard, was not taken: a one-way
  munition in the simulation (and a real one whose link outlives it a while) stays heard, so
  the debt would stay owed and the inbox alert with it, which is what q05 set out to end; and
  "no longer heard" is a link fact, not the platform's state. Spent includes the safety
  commands (a hold or an rtl): a platform spent by its own effect has nothing left to hold or
  return, and a command to it would only look as if it did. An operator who believes the
  platform survived has no override in this version (proposed follow-up: an un-spend with a
  note, under the operator's own session, logged and bound like a write-off).
- **A young track counts as possibly manoeuvring until the tracker has the facts** (10-06,
  murmur-9m0; rules 12). Rules 9 read a track's average velocity (after about 45 s of history)
  and its manoeuvre memory; a track too young for either fell back to the single region along
  its velocity: fail open. Now the tracker reports when it started the track (`first_seen_t`)
  and a track with a velocity but no average velocity yet also counts anywhere it reaches at
  its domain's plausible top speed (25 m/s surface, 100 air, 30 ground, 15 under water: the
  figures that already bound a track without a velocity), or at its own speed plus
  `keep_out_sigma` of its velocity error when that is faster. Why the top speed rather than
  the speed seen so far: a few seconds of fixes say nothing about a weave, a zigzag or a
  sprint, and the cost is bounded in time. The cost, stated: larger keep-outs during a track's
  first minute (and after a gap that cleared its history): a surface contact first detected
  2 km from an effect 80 s out blocks the nomination, and one detected near an engagement
  under way aborts it, until the tracker has followed it about 45 s. A scenario or caller that
  gives no `first_seen_t` is judged as before.
- **The scripted operators are fixed at the run's start** (10-06, murmur-jfi; rules 12).
  Whether a credential is harness-held was read from the file at each login and not logged,
  so turning it off mid-run logged the next login as a person's (the operator joined the
  people in replay), and a session's `password` relabelled `scripted` or back passed replay
  (forgery 14). Now a credentials run's roster lists them (`scripted`, an empty list when
  none); the engine opens a session only by the method that gives its operator, and replay
  checks every session against it. A credential re-marked while the run is up ends its
  sessions (its stamp changes, as before) and its next login is refused, saying why, until the
  bridge restarts: the roster is fixed for the run, as for operators added or removed. The
  step-up 429 now says the limit is the session's.
- **What a start carries over is one line of `run.started`, and a failed carry stays owed**
  (10-06, murmur-cq3; rules 12). Replay could not tell a successor log with every carried
  `platform.restore` removed (forgery C3: nothing in the log said a carry was due), a crash
  between two carried events left half a carry, and `.last-run` was rewritten even when the
  log it named could not be read, dropping that log's debt from the chain with only an inbox
  alert. Decided: the bridge reads what the earlier logs still owe before this run's log is
  opened, and the engine records it in `run.started` (`carried_from`: the log the pointer
  named and, tried again, every log it still owed for whose carry had failed, each with its
  head and last seq as read and what it still owed, or why it could not be read). One line, so
  atomic, and covered by the start anchor; the ledger is seeded from it (carried
  `platform.restore` events are refused from rules 12). Replay checks each named log: it
  replays, it is the one read (its head), it still owed exactly that, and every pending log
  was tried again. The pointer always names the newest log; the chain lives in the logs: a log
  whose carry failed is pending in the log that recorded it, so the next start reads it again,
  until it is read or an operator writes it off (`restore.closed`, `log`). The other option,
  a pointer listing every log still owed, was not taken: the pointer is not logged, so replay
  could not check that a debt was kept. Residual, stated: removing the record from the first
  line reads as a first start, caught only by the start anchor (like the roster).
  After the review (M2, L4, L5, L7): what the last run left behind never stops a start. A
  pointer that is empty, garbled, names no log (a crash mid-write, a stray edit) or can't be
  read is a failed carry like a log that can't be read, recorded under the pointer's own name
  (the log it named is unknown), alerted, owed until an operator writes it off; before, the
  bridge refused to start until someone edited the file, leaving an empty log at each try.
  The pointer is written whole or not at all (a temporary file, synced, renamed over it), and
  rewritten at every start, whatever the carry found; a start that fails after opening its log
  leaves no empty log behind. The start records the head and last seq of the last complete
  event it read, and replay checks the log carried from up to there (a torn last line is not
  part of what was read). A log recorded as failed that is at hand and reads, still owing, is a
  mismatch (unless trusted): its failure can't be told from a forged one.
- **Values carried over hold a mission for the vehicle they were preloaded on** (10-06,
  murmur-qjf; rules 12). Rules 7 held a mission only for values an engagement of this run
  preloaded, because the log could not tell which vehicle has the uid after a restart; so in
  exactly the crash case a carried mission could fly over the preloaded fallback. Now each
  snapshot carries the bridge's identity of every vehicle (platform, callsign, system id,
  home), and a decision's `restore_owed` fact for carried values logs it (`carried`: the log,
  the vehicle preloaded, the vehicle now, `other_vehicle` when they differ); they hold unless
  the vehicle is known to be another (unknown fields hold: fail closed), and replay recomputes
  the hold from the logged identity. A kind the policy never queues logs no reach, so carried
  values refuse it whatever vehicle has the uid (an operator writes them off if it is another).
  The identity is the bridge's report, as any fact is: replay proves the hold follows from it,
  not that it was true.
  After the review (M3): the rule is that only the platform, the callsign or the system id,
  where both identities know it, says another vehicle; the home never does, alone or with
  others. ArduPilot and the stand-ins set home again at every arming, so the very vehicle a
  preload was set on reports another home once it armed elsewhere: live after a crash, the USV
  under uid 2 (same platform, VIPER 1, system id 2) was judged another vehicle on a home
  1215 m away, its orbit went at once and its restore was never tried. A home mismatch now
  holds the mission and the restore is tried; a different vehicle that shares platform,
  callsign and system id under the uid is not told apart (an operator writes the values off).
  After the review (L9): what a failed carry owed is unknown, but not which vehicles it could
  owe it to: only an armed platform is ever preloaded, and the run's fleet file beside the log
  (`<log stem>.fleet.json`, written as the run went) names its platforms. The start records
  them with the failed carry (`may_owe`: uid and identity, values unknown), and a mission for
  one waits in the Queue, as for values known, until the log is read or written off (fail
  closed; a hold or rtl goes at once; another vehicle under the uid is not held). Without a
  readable fleet file (or for a pointer that could not be read) no mission is held, and the
  pending carry's alert says so; it names the vehicles otherwise. `may_owe` comes from a file
  outside the hash chain: replay checks it is only on a failed carry, not that it is complete,
  and only the start anchor binds it. Not covered, stated: a nomination for such a platform is
  not refused (its preload reads what the platform holds now); proposed follow-up.
- **A refused mission says which older orders still stand** (10-06, murmur-54l; rules 12). A
  mission refused for its platform's own values (its kind never queued) replaces nothing, so
  an older queued hold or rtl still goes when the link allows (safer: a safety command), but
  the refusal did not say so. Its reason now names every order still queued on its vehicles,
  with the advice to cancel one meant to be replaced, and replay recomputes the reason from the
  ledger, the engagements in play and the queue.
  After the review (L6): replay compares the whole reason, not a prefix, and not only for a
  reason that begins as a refusal does: any other void of such an order while its platform's
  values hold it must be one the bridge gives for something else (its task layer can't take
  the order, a vehicle lost, a newer order); replay can't recompute which of those held, only
  that the reason is one of them.

- **The bridge keeps one clock that never steps, steered to within about 2 s of the wall
  clock; the wall clock itself only dates what another clock reads** (10-05, murmur-dup;
  bounded after the review of 9ab8089, 10-06). On this host (WSL), early after boot, the
  monotonic clock ran fast against the Windows clock (2.1 % in the first minutes: 90.2 s
  against 88.4 s) and time sync pulled the wall clock back with a step every 32 s (0.64 to
  0.67 s at first, 0.94 s on an earlier boot, 0.46 to 0.81 s an hour and a half in); it was
  transient: at 2 h 52 min after boot the monotonic clock ran +0.011 % idle and 0.000 % under
  load against Windows and the wall clock made no step in 300 s (the review), and at 3 h 10
  min none in 130 s and 0.9999 against Windows over 120 s. NTP or a resumed virtual machine
  can step a wall clock either way. The bridge stamped every fact with the wall clock and gave
  the engine the wall clock, so every age, expiry and window decided on moved with a step, and
  tests that step the wall clock showed it: an hour forward voided a waiting intent ("no
  heartbeat for 3600 s"); 120 s forward rejected an order (its navigation status "120.0 s
  old"); 30 s forward expired a signer's picture token and aborted an engagement on
  `picture_gap`; 300 s forward dropped every track; 0.94 s back, 0.1 s after a track's fix,
  left a vehicle silent past its limit unvoided and put the picture's time before the fix,
  so a 20 m/s boat was predicted back along its course; and anchor files, timed by the wall
  clock, failed their own order check ("anchors out of order") when two anchors less than a
  step apart straddled one, so a genuine log could fail `murmur-replay --anchors`. The first
  fix (9ab8089) ran the bridge's clock at the monotonic rate from the wall clock at start; the
  review found that, in the drift, it ran 75 s an hour ahead of UTC, so a restarted run,
  starting at the wall clock, found every deadline it carried that far late (a human's
  approval, held 120 s, 30 s old at the restart after two hours, went back to the inbox 242
  s later instead of 90), the reboot-after-approval check blind for as long, and the
  evaluation's truth samples (Python's wall clock) matched to the wrong second. Decided:
  `clock::wall` runs at the monotonic rate while within 1 s of `clock::utc`
  (`DEAD_BAND_S`: more than the largest step, so the sawtooth alone never moves it) and at
  that rate times 0.95 or 1.05 outside it (`SLEW`: more than twice the worst drift measured,
  so the bound holds while drifting; any duration it measures is within 5 % of the monotonic
  one), never going back and never past the wall clock; the engine reads it
  (`clock::BridgeClock`). Bound: within about 2 s of the wall clock (1 s plus one step; a
  simulation of three hours of 2.9 % drift and 0.94 s steps stays within 1.94 s, where the
  monotonic clock alone ends 317 s ahead). A large jump of the wall clock (an NTP correction,
  a virtual machine resumed after its monotonic clock paused while the host slept) is
  slewed, never stepped, since a step would bring back the spurious voids and picture gaps
  above: a gap of `g` seconds takes `g / 0.05` (20 `g`) to catch up, a minute's gap 20
  minutes, an hour's 20 hours, and meanwhile the log's times trail UTC by what is left. Every
  anchor records the wall clock beside the bridge's time (`Anchor::utc`), so any time in a
  log maps to UTC. Durations and timeouts stay on the monotonic clock (as they were: TIMESYNC
  round trips, the GCS heartbeat thread, the decision tick, sessions and locks, restore
  backoff, telemetry re-sends); the wall clock itself dates only what another clock reads:
  CoT events (a TAK client compares them with its own), inbound CoT stale times, the resume
  pointer the next run reads, the run id. Anchors are timed on the log's clock and never
  earlier than the one before. Without `MURMUR_AT` the link emulator's scenario clock is the
  wall clock read when its elapsed time starts plus elapsed seconds (it read the wall clock
  for every satellite sample). The evaluation matches a decision's time to truth on each
  sample's `bridge_t` and maps the stand-ins' navigation log and the emulator's cut windows
  through the samples' (wall, bridge) pairs, so its labels do not depend on the bound.
  Not a rules change: no rule changed and replay recomputes the same decisions from the
  logged times whichever clock wrote them (the review regenerated 390 scenario logs
  byte-identical to master's); the engine never logs a time earlier than the last one, so
  even a clock stepping back leaves a log in order and replayed (every scenario, on clocks
  stepping back 1.5 s every 32 s and 3 s every 5 s), though each step would have moved the
  waits it decided on (scenario 69's human approval held about 6 s longer).
- **Tests do not depend on when they run or how loaded the host is** (10-05, murmur-ek3).
  The bridge's rigs start at a fixed instant (2026-06-14T10:00:00Z) and stamp the vehicles'
  facts on the engine's clock, the test's manual one or the bridge's own pinned to that start
  in a paused runtime (`clock::test_clock`, per thread); before, facts were stamped by the
  wall clock while the engine ran on the test's clock, and the sensing model's noise followed
  the second a test started in. The stand-ins' tests draw from a fixed seed
  (`Vehicle::seeded`; the AUV drift test drew under 1 m of error in 7 of 400 runs seeded from
  the wall clock), read what a stepped vehicle sent from its link (`Link::capture`), not a
  loopback socket, and their GCS reads a running vehicle's frames in its MAVLink sequence
  order (a loaded host's loopback delivered a refusal's text after its ACK), a frame
  waiting at most 5 s for an earlier one and only within 32 sequence numbers of it (the
  number is one byte: a longer wait let a burst wrap onto frames still held), and a
  window's held frames counted with it (a lost frame no longer holds a rate count back).
  What no harness fixes, stated: a datagram the kernel drops outright still fails a
  real-time test, as it would a ground station. Tests run the bridge's clock per thread
  (`clock::test_clock`): a `multi_thread` runtime's workers and `spawn_blocking` threads do
  not see it.
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
  for the whole run; they still go back at once when heard, alerted until they do. (Lifted
  by rules 12: they hold for the vehicle they were preloaded on, judged on the identity logged
  with the fact; see "Values carried over hold a mission" above.)
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
  so at start). Built as rules 10 (below).
- **Passkeys bind a step-up to its act, and replay verifies the signature itself** (10-05,
  murmur-437; rules 10, `PASSKEY_SINCE`). A step-up (or a login) may be a passkey's: a WebAuthn
  assertion with user verification (the authenticator's PIN, biometric or unlock). Its
  challenge is not random: the engine derives it (`passkey::challenge`, SHA-256 of canonical
  JSON) from the hash of the log's first line, the session, the operator, what it is for (an
  engagement signature: the intent, the picture token quoted and the SHA-256 of that token,
  which carries the plan and the track's identity; a hostile declaration: the track; a
  confirmation: the declaration; an acceptance: the lost vehicles), a server nonce and the
  issue time. The bridge checks the assertion with webauthn-rs, keeps its counter, and the
  engine verifies it again with its own code (ES256, the `p256` crate) against the public key
  the run logged before logging it (`operator.action step_up`, `auth: passkey`, `passkey`: the
  assertion, nothing secret); the act that quotes the step-up must be the act it was made for.
  Replay recomputes every challenge from the log and verifies every signature against the
  roster's keys (`run.started` lists every operator's passkeys as public keys; one registered
  during the run is `passkey.registered`), so a forger without the private key can't relabel a
  password step-up as a passkey's, move an assertion to another intent, token, session or run,
  claim another operator's credential, or answer a challenge twice; rewriting the roster in the
  first line changes every challenge (and is what the anchors catch). Why derive the challenge
  rather than keep webauthn-rs's random one bound in the bridge's memory: then the signed
  client data itself says what was authorized, and the log is evidence of it, not the bridge's
  word. webauthn-rs (kanidm, maintained) does registration (attestation parsing) and the live
  check; the derived challenge goes into its ceremony state through its
  `danger-allow-state-serialisation` feature (the state never leaves the bridge's memory).
  ES256 only: every current platform authenticator and security key offers it, and replay
  verifies one algorithm with its own code.
- **The relying party is `localhost`; 127.0.0.1 can't use a passkey** (10-05, rules 10).
  WebAuthn takes a domain as relying party id, never an IP address: the browser refuses
  passkeys on `http://127.0.0.1:<port>`. The bridge's relying party is `MURMUR_PASSKEY_RP_ID`
  (`localhost`) and an assertion may come only from the allowed console origins on that host
  (`http://localhost:<MURMUR_WEB_PORT>` and the bridge's own port; logged on the roster and
  checked by replay). A passkey registered for `localhost` works on any port of it; another
  stack's origin must be listed. The console at 127.0.0.1 says so and links to the same console
  on localhost (cookies are per host: sign in there); `make up` prints both addresses in a
  credentials run, and the evaluation's study prints the localhost one. Behind an HTTPS proxy
  with a real name, set `MURMUR_PASSKEY_RP_ID` and `MURMUR_ALLOWED_ORIGINS`. A passkey
  sign-in names the operator first; its options have one shape for every id (an id with no
  passkey, or no such operator, gets a made-up credential id derived from a bridge secret:
  no authenticator answers), except that an operator with several passkeys gets several ids,
  stated. Challenges waiting for an answer are bounded per operator on the roster (logins,
  four) and per session (step-ups, eight), the oldest giving way, so no stranger can fill a
  shared cap; an unanswered challenge leaves nothing in the log.
- **What a step-up must be is policy; a study requires a passkey** (10-05, user decision;
  policy schema 7, `PASSKEY_SCHEMA_SINCE`). `[approval] step_up_factor = "password"` (shipped,
  the default for normal credentials runs) takes the password again or a passkey (at least as
  strong); `"passkey"` takes only a passkey, and the bridge refuses a password there unchecked.
  `[approval] login_factor` (`either`, the default; `password`; `passkey`) says how people sign
  in. A scripted (harness-held) credential steps up as it is held and holds no passkey: its acts
  are never evidence anyway. A study run (`MURMUR_STUDY=<participant>`, set by the evaluation's
  study mode) logs its participant on the roster (`study`), runs with `step_up_factor =
  "passkey"` whatever the file says, and does not start (the bridge, the engine and the
  evaluation's runner each refuse) unless the participant is a person on the credential file
  with a passkey, or `MURMUR_STUDY_PASSWORD_STEP_UP=<reason>` gives the explicit override: it
  is logged in `run.started` (`password_step_up`), and such a run is a pilot, never evidence
  (`ReplayReport::study_evidence`, the evaluation's `auth_of`). The evaluation counts a study
  run only when the log names the study, without the override, and every consequential act of
  the participant's quoted a passkey step-up.
- **Registering a passkey re-enters the password, or takes a one-time invitation** (10-05,
  rules 10). A session left open must not add a credential to someone's account, so the
  console's registration (the operator menu) sends the password with its finish (a step-up,
  counted against the session's limit, always a password: the passkey being registered can't
  vouch for itself), and `passkey.registered` quotes it. `murmur-auth passkey-invite <op>`
  prints a one-time link (`#enroll=<op>.<code>`, the code's SHA-256 kept, 30 minutes by
  default, used once): whoever opens it registers a passkey for that operator, so it is handed
  over as a password would be; the evaluation's `enroll` uses it for a participant before
  their first session. The file keeps public keys and counters (and webauthn-rs's record),
  never private material; `murmur-auth passkeys` lists them, `passkey-remove` removes one and
  renews the operator's stamp (their sessions end: a lost authenticator), and a running bridge
  logs the removal (`passkey.removed`). A passkey registered elsewhere while a run is up (another
  bridge on the same file) is usable after a restart, as an operator added mid-run is.
- **A counter that does not count up is refused, and kept before the act goes ahead** (10-05,
  rules 10). WebAuthn's signature counter must increase for an authenticator that counts; a
  repeat or a decrease means a cloned key, and the bridge, the engine and replay refuse it. The
  bridge writes the new counter to the credential file (under its lock) before the act it
  authorizes; a file it can't write refuses the step-up. Synced passkeys (a platform's
  password manager) report 0 every time: that says nothing either way, and a passkey synced to
  another device of the same person is the same credential, by design.
- **Which acts step up besides signatures is a policy switch** (10-05; the scope is not yet
  decided). Every engagement signature steps up, and any human tier marked `step_up = true`
  (the armed-platform tier is one line away). `[approval] step_up_acts` (policy schema 5)
  adds others: `declare_hostile` (declaring a track hostile, what makes it a target) and
  `accept_unknown` (accepting a lost vehicle for an engagement's keep-out; a nomination's
  acceptances share one step-up). Off by default, as built; replay requires the step-up on
  each listed act in a credentials run, and the console asks for the password where it
  applies.
- **Attribution is a likelihood against the tracks and an untracked alternative** (10-05,
  rules 9). Each track is predicted to the act's time along its velocity, its uncertainty
  grown to then; its likelihood is the bivariate Gaussian density of the observed location
  under the combined uncertainty, against a uniform density of contacts we hold no track of
  (`untracked_per_km2`, 1 shipped). A track's attribution is its likelihood over the sum: two
  tracks close together share it, and a lone track far from the fix loses it to the untracked
  alternative. Candidates within five sigmas, the five most likely, rounded (1e-4, 0.1 m) so
  a replay on another machine computes the same verdict. Why this and not nearest-track: a
  nearest-track rule declares the nearer of two boats 15 m apart on a 60 m fix, which is a
  coin toss, and declares a lone boat 2 km from the fix.
- **Proximity is weak evidence, and never masks evidence** (10-05, rules 9; corrected after
  the review). A track in the same domain within 300 m of a declared hostile, velocity
  within 1.5 m/s of its, both velocities known, is flagged once per pair and shown suspect to
  readers (the console, agents); never declared. Same domain because an airliner over a boat
  is not its escort (scenarios 61 and 64 flagged one before the rule required it). The first
  build also put the raise into the decision picture and weighed a later act against the
  estimate frozen when the first proximity flag was raised; the review showed a ferry, flagged
  then dismissed, later reading neutral at 0.95, declared hostile at 0.997. Now decisions
  read the track's own evidence as the tracker has it now and its flags; a dismissed flag
  leaves nothing behind.
- **Who can be a victim, and who can't be its author** (10-05, review of rules 9). A victim
  is one of our vehicles (in our fleet: the world's vehicles, logged with the act; a uid that
  is not one is flagged, victim) or a track we protect: any tracked contact not declared
  hostile, a suspect included (an act against an undeclared contact is an act against
  someone we protect). The victim is never a candidate for its own author (before, an
  untracked author 20 m from a tracked fishing boat it attacked declared the boat at 0.995).
- **What "nothing conflicts" means, stated** (10-05, review of rules 9). Missing identity
  data does not conflict: an unclassified track is declarable when the act is attributed to
  it, because the act is the evidence. A track shown neutral or assumed friend at
  `conflict_confidence` or more conflicts; shipped 0.5 (first 0.7): the tracker gives a
  fishing boat without AIS 0.8 of its classification, so 0.7 protected one only at 0.875
  classified, and at 0.5 it is protected from 0.625. An operator's revocation makes later
  acts attributed to the track conflict flags until an operator declares it again: a
  person's judgment outranks the rule until a person changes it.
- **Corroboration is a policy switch, measured both ways** (10-05, user decision; the
  second act's eligibility and window after the second review). `[identification]
  corroboration = "none"` (shipped) declares on one observation; `"second_observation"` waits
  for a second independent one, another observer's fix of the same act or a second act
  attributed to the same track at the threshold, and flags the act (uncorroborated) until
  then. A second act counts only if it would itself have counted (its verdict a rule
  declaration, or a flag only for want of a second observation: never one flagged because its
  victim was not ours or the track's evidence conflicted) and was observed at most
  `corroboration_window_s` before (10..3600, shipped 600). The declaration names what
  corroborated it; replay recomputes it from the act's fixes and the ledger's earlier acts.
  The evaluation runs both.
- **A rule declaration nobody confirmed stays protected in another engagement's keep-out**
  (10-05, rules 9). A track declared hostile by an operator, or by the rule and confirmed by
  one, is exempt from the keep-out of engagements on other targets, as before; one the rule
  declared and nobody confirmed counts as protected unless it is the engagement's own target.
  Conservative on purpose: a false rule declaration (an untracked author beside a fishing
  boat, scenario 75) must not also lower the protection of everything near it. Whether a rule
  declaration may be nominated before a confirmation is a policy switch
  (`nominate_unconfirmed`, off shipped, on in the evaluation's minimal profile).
- **A revocation stops what it changes, first** (10-05, rules 9; bystanders added after the
  review). Any operator may revoke any declaration (an operator's or the rule's): it only
  restricts. Before anything else in that step every engagement in play on the track stops
  (cause `revoked`, naming the operator, the track and the declaration): one waiting for
  signatures is voided, one under way aborted with its fallback per the existing stop rules;
  so does every engagement under way whose last picture held the track exempt from its
  keep-out and would fail it without the declaration (a bystander: before, it was aborted in
  the same tick but logged as a picture failure; scenario 77). An engagement approved but
  not yet watched holds the world its approval was checked on as its picture, so a
  revocation between the approval and its first picture names it too (before, it was not
  named and a later picture stopped it, cause `picture`; scenario 78). The declarer, when someone
  else revoked their declaration, gets an inbox notice with the note. A confirmation while
  an engagement is being signed changes the target's identity in its plan, so the signatures
  are dropped and taken again (conservative, as any picture change).
- **Confirming is never the easy act in the console** (10-05, second review of rules 9). A
  rule declaration's card opens its evidence first (the act, whom it was against, which
  sensors saw it, the attribution against the threshold) and confirm sits there, a plain
  button as revoke is; revoking (the brake) takes no more clicks than confirming, two each,
  in the inbox and in the tracks drawer. The first build had a prominent one-click confirm and
  a two-click revoke.
- **Confirming a rule declaration does not step up by default** (10-05, user's minimal
  approval). `confirm_declaration` is a name `[approval] step_up_acts` accepts (policy schema
  6, `RULE_ID_SCHEMA_SINCE`), off unless listed; an unknown name fails to load. A rule
  declaration carries no session and no step-up: it is the engine's. Revoking and resolving a
  flag never step up (they restrict or record).
- **The shipped policy turns rule declaration on, nomination before confirmation off**
  (10-05). `sim/policy.toml` (and the evaluation's base and strict policies, which mirror it)
  has `rule_declaration = true`, `attribution_threshold = 0.9`, `nominate_unconfirmed =
  false`: the engine declares what is clear and a person confirms before it becomes a target;
  `eval/policy/minimal.toml` drops that confirmation and every optional step-up, to measure
  what minimal approval costs.
- **Still placeholders, stated** (10-03; revised 10-04 by rules 8, 10-05 by rules 10). Who
  holds engagement authority is now each person's credential, not a header (above), and a
  consequential act can require that person's passkey (rules 10; a study always does). What is
  still not here: attestation (any authenticator is accepted, a software one included: a
  passkey proves the person holds a key they registered and unlocked it, not what kind of
  device it lives in), which person unlocked the authenticator (one person holding two
  credentials, or lending their device and PIN, still signs as two people), a password
  required with a passkey login (`login_factor = "either"` lets a passkey alone sign in: it is
  itself two factors), a blocklist of common passwords, TLS (the bridge listens on 127.0.0.1;
  `MURMUR_COOKIE_SECURE=1` marks the cookie Secure behind an HTTPS proxy), signed or remotely
  anchored logs (the hash chain and anchors are tamper-evident only), and per-person identity
  on the reads. Until rules
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
