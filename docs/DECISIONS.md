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
- **A dropped protected contact stays in the keep-out until an operator accepts it** (10-07,
  murmur-533; rules 16, `LOST_CONTACT_SINCE`). The bridge's tracker drops a track unseen for
  180 s, and from then on the keep-out stopped counting it: a civilian boat that went out of
  sensor view near a target stopped protecting anything once its track aged out, and an effect
  could be released where it might still be (review of 2f4f46a). Decided, mirroring the lost
  friendly above: the drop is a logged fact (`track.dropped`, the tracker's facts at the drop,
  its estimate only; the bridge keeps the track in the decision facts until the engine has it,
  so no decision falls between the two) and the engine holds the contact as lost. Every
  engagement's keep-out on tracks counts it anywhere it could have reached since its last fix:
  by the regions a live track gets from those facts (`engage::track_reach`; a track with no
  velocity or no average velocity at its domain's top speed) and, since nothing sees it turn or
  speed up, also anywhere at its domain's top speed (`engage::lost_reach`; its own plausible
  speed until the review below), with no cap: fail closed. It is
  exempt only where a live track would be (the target, a declared hostile by the
  identification ledger). An operator with engagement authority may accept one for one
  engagement, explicitly and logged under their session with its facts and the world's time,
  through the lost friendly's own acceptance (`engage.accepted` naming a `contact` instead of a
  `uid`, rather than a parallel mechanism): at the nomination or before a first signature,
  every check of that engagement reads it, every signer is shown it (the card, the picture
  token: a token from before it is refused), a reset clears it, any operator may revoke it,
  never an agent; it covers that loss only. A track seen again with its AIS identity was first
  retired at once on an unambiguous match (`track.reacquired`); since the review below it is a
  claim an operator confirms (`lost.claimed`, then `track.reacquired` under the operator's
  session), only while nothing contradicts it: the only live track carrying its MMSI and no other
  lost contact carrying it, that track undeclared and not suspect (what our sensors see fits its
  AIS), in the same domain, first seen after the lost contact's last fix, and where it could have
  reached. Association across a drop is by identity, and the only identity here that names one
  contact is its AIS (a kind like "ferry" names many); anything short of a confirmation keeps the
  lost contact beside the new track. The
  decision core gets all of it from logged facts: replay rebuilds the lost contacts from the
  drops, confirmations and reinstatements, checks every engagement decision's (and its accepted
  ones) against them and every other decision's empty, recomputes each claim and confirmation from its own picture, and
  flags a track that leaves a decision's picture without a `track.dropped` (else a track could
  vanish from the record, and from the keep-out, with nothing to show). Shown on the map (a
  faded, anticipated symbol at the last fix, the region counted now, "T-0002 LOST 4 min"), in the
  tracks drawer, in the nomination form's keep-out explanation (which lost contact blocks, since
  when, an Accept for this engagement), on the card; agents read them and cannot accept.
  Accepting one takes the same step-up as accepting a lost vehicle of ours (`accept_unknown` in
  `[approval] step_up_acts`, a credentials run): the password again, or a passkey whose challenge
  names those contacts and their fixes; the bridge refuses the request without it, whatever
  the console sends, and the form asks for it. Measured (`eval/METRICS.md` "Out of view", model
  results, unreviewed; the numbers after the review): with a contact out of view within 1 km of
  the target, the drop let 1,295 of 25,220 nominations through with it inside the keep-out line at
  the effect time and 86 with it inside the effect circle; holding it as shipped blocks every one
  of those nominations (47% before), 51% needing an acceptance; in traffic where 30% of contacts go
  out of view, 95% blocked (51% before), 30% needing an acceptance, and no pass missed a contact
  out of view. None of the cheaper policies is sound against the truth: forgetting it 600 s after
  its drop (145 and 11 such misses near the target), counting it only near the effect point (376
  and 30), or clearing the water our sensors cover with no detection (every lost contact there
  is inside the radar's nominal coverage, so it misses as today does: a track ages out within
  coverage only when the sensors stopped seeing a contact that was there). So none is shipped;
  the acceptance is the way through, and its frequency is the stated cost of failing closed.
  The plausible-speed region (since replaced by the top speed, below) came from the first
  measurement (10-07, the main thread's call, the same rules version since the lost contacts had
  not merged): the first version counted a lost contact by a live track's regions alone, a
  prediction rather than a bound for a contact nobody sees (one last seen steady got only the
  regions along its velocity, with the acceleration-noise uncertainty), and 3 of 716 passes near
  the target had it inside the keep-out line (none in the circle); the design had said its region
  grows at its plausible speed, and the disc took that to 0 of 647 for 3 points of blocks in the
  general traffic (87 to 90%), passes that the review showed were all with the contact not yet
  dropped (no evidence about the disc). Not carried over a bridge restart (a new run starts a
  new tracker): murmur-7qq, open now that rules 15's carry follow-ups are merged.
  Regressions: scenarios 86 to 91.
- **After the review: out of view at the top speed, AIS a claim, the replay gaps closed** (10-07,
  murmur-533's review; rules 16 still, before it merged; the main thread's decisions). An
  independent review found the lost contacts short of their own design in five places:
  - *A lost contact counts at its domain's top speed, not its own.* Its own plausible speed (its
    speed and 2-sigma of its velocity error) is a prediction for a contact that keeps its speed,
    as its course was one for a contact that keeps its course; it also departed from both
    precedents (a track without a velocity counts at its domain's top speed, a lost vehicle of
    ours grows at its class's speed). A contact out of view now counts anywhere it reaches at its
    domain's top speed (25 m/s on the water, 100 in the air with the tracker's 110 m lag, rules
    14), or its own bound when that is faster (`engage::out_of_view_speed`); a lost aircraft also
    keeps rules 14's region. The generated traffic could not show the gap (every behaviour but
    sprint-and-stop kept one speed for good), so a quarter of the contacts that go out of view now
    speed up after their drop; on that traffic the own-speed version missed one (a dropped contact
    that sped up, at the line, 1 of 7,563 passes in `oov-all`) and the top speed none, for 4 to 6
    points of blocks (90 to 95% in `oov`). The first measurement's "0 of 647" passes near the
    target were all with the contact not yet dropped, judged by its course: no evidence about the
    disc (`eval/METRICS.md` "Out of view" says what it can and cannot show).
  - *Out of view before the drop counts the same.* A track unseen 10 to 180 s was judged by its
    course alone: the review's ferry 600 m from the target, steaming away at 5 m/s, let a
    nomination through unseen 170 s that it blocked unseen 182 s, and a short engagement chain
    fits in that window. Now a live track unseen `engage::OUT_OF_VIEW_S` counts the same disc from
    its last fix (`engage::track_standing` with the decision's time). Why 10 s: it is the tracker's
    stale age (the console fades the track then; a test holds the two equal), the sensing model
    draws a detection every second with a probability of at least 0.47 inside a sensor's nominal
    range (0.94 within half of it), so ten misses in a row (at most 0.16%) mean the sensors are not
    seeing it, not a gap between fixes, an AIS report comes every 2 to 10 s underway, and below it
    a contact that turned back unseen is at most about twice its speed times the gap off its
    course (100 m at 5 m/s), which the course regions' acceleration noise already allows for 40 s
    ahead. A shorter threshold would count a track between two radar sweeps; a longer one leaves
    the review's window open. Cost: 1 point of blocks in `oov` (94 to 95%), and those blocks no
    acceptance clears (a live track is not acceptable: it is seen again, or dropped and then
    accepted). Scenario 92.
  - *An AIS match is a claim an operator confirms, never automatic.* The review's probe: a ferry
    with MMSI X dropped 400 m from the target; ten minutes on a "neutral" track carrying X
    appeared 1.9 km away, inside the reach; the match retired the ferry on the first tick, and
    when that track turned suspect a minute later (the tracker finds an AIS spoof suspect only
    once EO/IR has looked, about 16 s, while the match ran every second) nothing reinstated it: a
    nomination at the target passed and replayed. Now a live track carrying a lost contact's MMSI
    raises a review item (`lost.claimed`, once, with the picture, the evidence: names and ship
    types compared, the distance against the reach, what our sensors say of its kind, and what
    keeps it from being confirmed); any operator may confirm it under their own session (the
    review list, `POST /api/review/lost/{track}/confirm`, `track.reacquired` with `by` and
    `session`, replay-checked; agents can't), only while nothing contradicts it, as the automatic
    match required. Until then the lost contact keeps blocking. If the claimant later turns
    suspect or hostile, is declared or drops, the confirmation is void and the lost contact counts
    again: every decision reads it so on its own picture (no race with the loop) and the loop logs
    `lost.reinstated`, for good. No step-up (the review said under their own session; a step-up
    act for it would be a policy schema change: a follow-up). Scenarios 93 (the probe) and 94.
  - *Accepting a lost contact on a waiting nomination took no step-up through the API.* `{track,
    since, password}` was refused as an unknown field and without the password the bridge
    answered 401, so in a run whose policy steps up `accept_unknown` it could never be done: the
    request now carries the password or passkey; the console's card offers it.
  - *Replay gaps.* A second `track.dropped` for an id already lost replayed (the review moved the
    ferry 6 km with a fresh fix): the engine refuses a second drop of an id and replay flags one,
    and a drop's last fix must be no older than the newest the decisions had seen, and the same
    fix when its time is the same (a newer fix was not checked until the next review, below).
    A claimant seen only in a reacquisition's picture was never held to the rest of the log: claim
    and confirmation pictures are now read like a decision's (a track in them cannot vanish
    without a drop).
  And the lesser findings: a drop the log could not take goes back to the tracker instead of being
  lost; the form forgets its ticked acceptances when the target or the platform changes; MCP
  `propose_intent` refuses an acceptance (403) instead of ignoring it; the explanation's grid
  margin is sized for the picture's fastest relative motion (below). A track's AIS identity moved
  out of `TrackFacts` into `WorldSnapshot::track_ais` with the merge of rules 14: the tracker's
  comparison copies (`tracks/tests/before_oho`, never edited) construct `TrackFacts` with its
  fields as they were. The burden, measured as a model (`eval/lost_burden.py`): with no cap and
  acceptance per engagement, the lost contacts of a run pile up and every nomination needs each
  of them accepted, two hours in from one or two in calm water to 140 at the oov variants' loss
  rate in dense traffic; at those numbers a per-contact acceptance is no longer a judgement, and a
  way to retire lost contacts soundly in bulk (evidence of where they are) is the open question
  (follow-up).
- **After the review of the merged branch: a drop held to the pictures and the tracker's age, a
  dropped id never hides its contact** (10-08, rules 16 still, unmerged; the main thread's
  decisions). An independent review of the branch merged with rules 15 found five gaps in the
  lost contacts' own logic:
  - *A drop with a newer fix than a later picture showed replayed.* A ferry last seen at T0 was in
    a picture at T0+175; a drop at T0+180 claiming a fix at T0+170, 10 km away, which that later
    picture never showed, let a nomination through and replayed. Replay now records the world
    time of the latest picture that showed each track and flags a drop whose fix is newer than
    the decisions' when a picture taken at least 1 s after that fix still showed the older one
    (the tracker dates a fix when it takes the detection, and a snapshot reads its time and its
    tracks a moment apart: within that second a picture may honestly lag). The other side, a drop
    too soon: `run.started` logs the tracker's drop age (`track_drop_s`, the run's
    `MURMUR_TRACK_DROP_S`, the same for every domain; v16 only, absent in older logs and in tests
    without a tracker) and replay requires each drop at least that long after the track was last
    seen. The scenario runner logs the bridge's default 180 s and, on a clock that steps back,
    holds a `drop_track` step until the age has passed by its own clock, as the tracker would.
    The review's second pass closed what was left: the age was counted from `last_seen_t` while
    the keep-out reads the fix's own time, and nothing tied them (a fix dated 179 s after a last
    sighting at 0 s, 10 km away, passed); a drop's fix may now be no later than its last sighting
    (the tracker dates it then; none of the 2,704 track facts in the v12 to v16 fixtures has one
    later, and the 7 drops have the two equal) and the age counts from the later of the two. A
    v16 log with drops must log the age (stripped, the check would be off), and a `drop_track`
    step still waiting at the end of a scenario fails the run, naming the step.
  - *A dropped id back in a picture with a fresh fix hid the lost contact.* The keep-out skipped
    a lost contact whose id was in the picture, so a fix 30 km away under the dropped id let a
    nomination through. The tracker never reuses an id within a run, so such a track never stands
    for the contact: the engine judges the lost contact whatever the picture holds (fail closed;
    the honest race, a snapshot taken before the drop was handed over, holds exactly the drop's
    facts and judges the same disc twice), the explanation mirrors it, and replay flags a dropped
    id back in a picture with another fix than its drop's.
  - *An agent's preview read no lost contacts* (MCP `preview_intent`), so it said a nomination
    would wait for signers when its proposal would be rejected. Every decision's checks and every
    dry run of one (the preview, the nomination form's options, the console's live checks of a
    waiting engagement, which also lacked rules 7, 12 and 15's facts) now fill the engine's facts
    with one function.
  - *A claim's confirmation took its picture before the decision lock*, so a tick in between could
    log a newer picture and the confirmation's would read as a track gone without a drop: it is
    taken under the lock, as a nomination's is.
  - *Revoking a dropped hostile's declaration listed no bystander*: the revocation refilled only
    the live tracks of each engagement's last picture; it refills the lost contacts too, so an
    engagement whose keep-out the declaration exempted stops at the revocation, not a tick later.
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
- **The air tracker is calibrated against truth, and a 2D radar's fix of an aircraft is its
  bearing line** (10-07, murmur-oho; the review of the keep-out measurement: a light aircraft
  seen by one USV's radar had a velocity 0 or 1 time in 60, with EO/IR too half the time
  straight and never loitering, so the keep-out held it anywhere within 100 m/s for minutes;
  the air constants had been scaled from the surface's and never checked; then a review of
  this change, the same day). Three changes:
  (1) *A 2D radar's fix of an aircraft*: the radar measures slant range and bearing, not
  height; the sensing model had centred the fix on the aircraft's true ground position, which
  needs the height. Now it plots it at the slant range (out by the height's share: 800 m for
  an aircraft 1,500 m up 1 km away) and the detection says where the radar was
  (`Detection::slant_from`); the tracker reads it as the bearing line the aircraft is on, no
  farther out than the slant range. One radar alone does not range an aircraft (no height
  bound is assumed: one would be an invented altitude). A track is ranged while a ranging fix
  came within 10 s (`RANGE_FRESH_S`, the engine's own limit for a ranged altitude): another
  sensor's position, or another radar's bearing crossing this one by 30 degrees or more (the
  pair triangulates; at 15 degrees, the first choice, two radars 12 km out put an aircraft
  3 km off at 3.8 of its sigmas). Unranged, a track sits at the middle of the segment with a
  1-sigma of a third of its length (its ends 1.5 sigmas out) and no velocity, its velocity
  reset to none (a velocity from bearings at an unknown range is wrong by as much as the
  range). While only bearings come, once no position fix has come for 3 s, both models widen
  along the line as an aircraft turning at 8 m/s^2 would (80 m^2/s^3); a ranged estimate
  beyond the slant range by more than 3 of the radar's sigmas is moved back and its spread
  widened by the move. The review found the first version deciding "ranged" by the
  covariance along the line, which the steady model's small noise kept small once EO/IR lost
  an aircraft, so a track slid kilometres along the bearing on a stale velocity (5.4 km off
  while it said 1.8 km; a keep-out 3 s ahead missed by up to 5.5 km): ranging by time, the
  widening along the line and the 3-sigma constraint answer it, and the velocity reset and
  the 30 degrees were found after (making them, and round three). The track never gives an altitude its
  sensors did not measure (unchanged: only a radar that measures elevation, `height`). EO/IR's
  fix stays a position, as the sensing model has it (a passive sensor without a rangefinder
  is angles only: a follow-up). An act a radar-only aircraft commits is placed as its
  detection is, the segment's middle with its spread (`observe_act`).
  (2) *Full covariance*: a measurement of one direction couples east and north, so the IMM
  keeps the full covariance of the horizontal state; the reported 1-sigma is the least certain
  direction's (keep-outs are circles of it), and the velocity gate reads the velocity's least
  certain direction. On the water nothing else changes: the surface calibration passes
  unchanged, its classes' NEES 1.03 to 1.90 (was 1.05 to 2.02) and within two sigmas 86.7 to
  97.5% (85.2 to 97.3), slightly looser because the models' spread now keeps its east-north
  terms. (3) *Air constants calibrated* as the surface's were (`tracks::tests::calib_air`):
  truth motions of aircraft (straight, loitering, routes, one turn, weaves, random turns,
  speed changes, a helicopter that hovers and goes, a small UAS, an approach and a break
  away), three sensor mixes (one USV's 2D radar; its radar and EO/IR; every sensor of the
  engagement demo), disjoint seeds, and the tracker before this change run on the same truth
  and draws (verbatim copies, `tests::before_oho`). Steady and manoeuvring white-noise
  acceleration 0.2 and 20 m^2/s^3 (were 0.4 and 80): the most velocity with every class
  honest when updated and the velocity honest where an aircraft flies straight between
  manoeuvres. The 3 m/s gate was judged and kept: at 3.5 a route's velocities came with a
  normalized error over 2. *The order of events*, stated because it moved: the tuning set
  (seeds 101-130) and round one (131-160) were written together before any tuning; round one
  passed every criterion but one then asked of routes too (a square route's velocity in its
  turns off by 3 to 4 sigmas); round two (161-190) was written; a widening of the velocity's
  error by its rate of change was tried on the tuning set and round one and removed (it cost
  straight flight a fifth of its velocities: only a coordinated-turn model follows a turn, a
  follow-up), and the velocity criterion narrowed, having seen round one fail it, to the
  classes flown straight between a manoeuvre or two; round two passed on that code (f0c7026,
  a keep-out 60 and 100 s ahead the only spans then asserted). After the review, round three
  (191-220) was written before any fix was run; the fixes were measured on the tuning set and
  rounds one and two, and the criteria widened to a keep-out 3, 10, 30, 60 and 100 s ahead and
  to the tracker alone; round three, run once, failed (the 15-degree triangulation: a keep-out
  3 s ahead missed by up to 1,235 m); the crossing angle went to 30 degrees and the tracker
  alone's bounds at 10 and 30 s were widened for its break away, round three being tuning
  data by then. The held-out set (221-250, written before that change was run) was run once,
  the constants frozen: every class and mix within two sigmas 88.1 to 100% of the time when
  updated, NEES 1.08 to 1.97; a velocity given in the classes flown straight between
  manoeuvres at a normalized error of 1.66 at most; never an altitude; the tracker alone
  within its bounds (a few seconds ahead, missing 0 to 2.1% of samples by up to 236 m, the
  tracker before 0 to 1.7% by up to 221 m); a keep-out 3 to 100 s ahead holding every case
  every time but twice in some 300,000 samples 3 s ahead, by 20 m and 6 m, both an aircraft
  without a velocity whose estimate was 3.5 and 7 sigmas off (one 10 km out ranged by a lone
  EO/IR fix, then given a velocity of 140 m/s by two nearly parallel radars' bearings, the
  truth 75; a route's turn the tracker had not taken in when EO/IR lost it), pinned in the
  test as found (the tracker before, under rules 12: 83.8 to 100%). With radar and EO/IR a
  straight aircraft has a velocity 61 to 71% of the time in the sets flown near the USV and
  32 to 38% where it is far or fast (8 to 10 km out, 75 m/s), the tracker before 14 to 30%;
  with every sensor 69 to 82% (37 to 41%), before 23 to 62%; one radar alone: none (was none,
  on fixes it could not have had). Limits, stated: in a sustained turn (a loiter, a weave,
  random turns) a velocity is seldom given (0 to 27% of the time) and then comes with a mean
  normalized error of 3 to 97 where 2 is expected: the console shows a lagging velocity, and
  the keep-out counts an aircraft wherever it could turn (rules 14, under Engineering). The
  tracker lags a speed-up of 1 to 3 m/s^2 by 7 to 13 s (it has said 2 m/s of a helicopter
  already going 24) and a hard turn with EO/IR lost by up to 300 m, which rules 14's region
  counts. Between a lone position fix and the next, nearly parallel radars' bearings can draw
  a velocity no aircraft here flies (140 m/s) into the estimate (never shown: its error is far
  above the gate), which moves it along the line: bounding the velocity state by the top
  speed is a follow-up.
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

- **Situation status is display only: outside the decision engine and the audit chain** (10-08,
  situations v1; `crates/murmur-bridge/src/situation.rs`, `sim/situations/README.md`). A
  situation (`make situation NAME=<name>`) is a pre-built run to watch: a stack, opening orders,
  an AI, a timeline. Its runner posts one status document to `POST /api/sim/situation` (the
  stack's own simulation token, the same guard as `/api/sim/contacts`, checked before the body
  is read) and the console shows it from `GET /api/situation`. The bridge keeps it in memory and
  nowhere else: no reference to it from the decision engine, the world snapshot, the tracks or
  the MCP tools, and nothing of it in the audit log (a test posts one and finds the log, the
  intents, the state, the tracks and the inbox unchanged). Why: the document may carry truth (the
  director's notes, the timeline still to come, a step that says what a contact really is), and
  truth must not leak into a decision: a decision made on what the sensors never gave could not
  be replayed as the engine's, and an agent that read it would be reading the answer. The audit
  chain is what the engine decided on; a display aid is not that, so the runner keeps its own
  record (`logs/situations/<run>/situation.jsonl`). The token, because only the stack's own
  runner should write what the console presents as the situation. Blinding is the bridge's, not
  only the runner's (10-08 review, B3): unless the document says `reveal: true` (exactly) the
  bridge keeps only the top-level keys a blinded panel shows (an allowlist, round two), of the
  timeline only what is done, and no timeline entry or watch item whose `director` is anything
  but `false`, whatever the runner sent; `phase_detail` and `problems` stay as strings and carry
  only the runner's fixed messages. Watch items without that mark appear as they come due, so
  they say what to look at, never what it is. `GET /api/situation` is open on the
  local port like every other read, so the AI is kept from it, and from the checkout, by how it
  runs: MCP tools only (Claude `--strict-mcp-config`, `--tools ""`, `--allowedTools
  "mcp__murmur__*"`, `--setting-sources project`; Codex a read-only sandbox, approvals never),
  started in an empty directory of its own outside the repository holding only the brief, with
  no `MURMUR_*` variable. A revealed run puts truth on the bridge for anyone who asks, so with a
  model AI it is marked "not evidence about the AI (truth was served on the bridge)" in
  `situation.jsonl`, the panel and the summary.
- **A situation's scripted operators sign as the engine requires, and never count** (10-08,
  situations v1; `eval/proxy.py` `EngagementSigners`). In autoplay the evaluation's proxy model
  decides as `op-auto` and two scripted operators, `op-2` and `op-3`, sign engagement
  nominations: each asks for the picture and quotes its token, reads the nomination again (from
  the inbox, where its engagement view is served) just before it signs, distinct signers, never
  the proposer, only while the engine's live picture of the nomination passes, never a refusal
  (a script, not a judgment). Live, the person signs; with `cosigner = "scripted"` a scripted
  operator adds only a second signature: after a signer who is not one of them, on a plan that
  signer froze (its picture token says `frozen`; a token on no frozen plan, the signatures reset
  meanwhile, would make it the first), at most once per nomination. Situations run as dev
  runs for now (the runner refuses `MURMUR_AUTH=credentials`): a dev run has no `scripted` mark
  on the roster, so the runner records which ids it drove (`situation.jsonl`) and every note they
  leave in the audit log says "scripted ... not a person"; a dev run is never evidence about
  people in any case ("Harness-held credentials are scripted, not people" holds as is). The
  evaluation's arms do not sign yet (their rule book and metrics have no engagement case), so
  `eval/scenarios/engagement/` stay placeholders.
- **In a situation op-1 is only the person, and a script never authorizes a human tier** (10-08
  review, R-H1, R-H2, R-M5; `eval/situation_run.py`, `POST /api/intents/preview`). Situations v1
  issued the opening orders as `op-1` and ran the autoplay model as `op-1`, so a log could show
  the person authorizing what a script did: two Harbor watch orders were approved at submission
  by "op-1" (`endurance-unknown`, a human tier whose submitter authorizes). Now every scripted
  act has its own id (`op-setup` for the opening orders, `op-auto` for the autoplay model,
  `op-2`/`op-3` signing, `op-planner` for the timeline's station, `agent-setup` and
  `agent-scripted` for MCP proposals), listed in the run's record, and its rationale or note says
  it is a script's. Within the current rules a human source's submission in a human tier with
  `submitter_authorizes` is its own approval (replay flags anything else), so the runner asks
  first: an operator's dry run (`Engine::dry_run_for_operator`; read-only, not logged, vehicle
  tasks only). A scripted operator id submits only what the dry run says can't be its own
  approval (`auto_approved`, or `pending_approval` in a tier whose submitter does not
  authorize); anything else, a `rejected` dry run too (round two: a check failing on a flaky
  terrain fetch or a fact's age may pass a moment later in a human tier), is proposed by the
  scripted agent `agent-setup`, and waits for the person (live) or `op-auto` (autoplay). The dry
  run and the submission are two moments, so every scripted submission's answer is checked: an
  approval at submission by a scripted id is a loud problem and is cancelled if it has not gone
  out. An opening order for an armed platform is refused at load. The timeline's operator
  proposer follows the same rule in both modes, and a `declare` step is refused in live mode (a
  script's declaration decides; the person declares). Considered: a policy key making a scripted
  id's submission wait (a rules change for a harness concern; the agent route uses what the
  engine already guarantees, that an agent's proposal in a human tier waits for a human).
  Harbor watch opens in a verified state: its fleet is platforms with an endurance figure
  (`seahunter`, `mq9b`, two `puma`), so its opening orders go out by policy; adding figures for
  `corsair` and `fvr90` is the user's call, and until then the stand-ins say at start that they
  simulate 10 h for a platform without one while the C2 treats its reserves as unverified.
- **Murmur never handles anyone's model credentials; a situation runs the user's own CLI**
  (10-08, user direction: Murmur is open source and people run it with their own Claude or
  ChatGPT subscription). A situation's AI is `claude` (the default), `codex`, `playbook` (the
  deterministic `agents/policy_agent.py`) or `none`. For `claude` and `codex` the runner only
  invokes the CLI the user installed and logged into (`agents/watch_officer.py --backend
  claude|codex`): it never stores, reads, proxies or prompts for a credential.
  Before the stack starts it asks that CLI whether it is logged in (`claude auth status`, its
  `loggedIn` and `authMethod`; `codex login status` where the installed Codex has it, else
  skipped with a note) and keeps only yes or no and the kind of login, never an account, an
  email or a key. A missing or logged-out CLI stops the run there, naming the fix (`claude` then
  `/login`, or `codex login`) and the free option (`AI=playbook`); nothing falls back silently,
  so a run never says it had an AI it did not have. The panel names the AI ("Claude", "ChatGPT
  (Codex)", "Playbook (deterministic)", "None").
- **One clock for a situation: the stack's start** (10-08, situations v1). Every `at_s` of a
  situation (steps, watch items), its `duration_s` and `ai_start_s` count from the link
  emulator's start, which `scripts/up.sh` starts first and the runner reads from its uptime, so
  the existing events files (`sim/link-events/`, `sim/events/`, whose `at_s` count on the
  emulator's clock and from the stand-ins' start) work unchanged; the contacts process starts a
  few seconds later and its `start_s` counts from there. The evaluation instead holds every
  process to an origin 45 s after the stand-ins start and fails a run not ready by then; a
  situation goes through `up.sh`, which starts the contacts and the stand-ins itself, and a demo
  is better started a little late than failed, so the runner waits for readiness, issues the
  opening orders, runs steps that fell due meanwhile at once, and records when everything
  happened.

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

- **The watch officer runs the user's own model CLI, woken by a table, with the stronger
  model as a fresh consult** (10-08, `agents/watch_officer.py`, `agents/README.md`). Murmur
  holds no model credential: it runs `claude -p` (default) or `codex exec` as the user logged
  them in, with the API-key variables removed from the child's environment. The driver, not
  the model, decides when to wake it: a deterministic trigger table over the bridge's
  `get_changes` (and `get_nomination_options` while a track is declared hostile), coalesced,
  with a slow heartbeat and budgets; a condition that came and went in one batch stays a note
  (the first live run woke the model twice for an AUV's 2-second link flaps). Escalation
  (engagement chain, a lost link on an armed platform, a suspected spoof) goes to the
  stronger model in a fresh short process, not a fork or resume of the routine session:
  that would send the whole routine conversation again, uncached, on the stronger model at
  every escalation; the consult reads the picture itself and the routine model is told what
  it did. Claude is kept to the bridge's tools by `--setting-sources project` in an empty
  directory, not `--safe-mode`, which also drops the `--mcp-config` server (checked, 2.1.294).
  The stream-json interrupt ends a turn and keeps the process (SIGINT ends both). After
  review (10-08): a condition that comes and goes a second time within 5 minutes is flapping
  and wakes (escalating for a GPS glitch, a conflict, an armed platform's link), since
  intermittent glitching is what jamming and spoofing look like; one subject wakes at most
  once in 5 minutes and escalates at most once in 30 (one jittering contact spent every
  escalation in the reviewer's probe); free text from outside the driver reaches the model
  quoted and labelled, and the prompt says it is data, since auto-tier proposals run without
  a human; a failed turn gives its wake back. After the second review (10-08): a subject is
  the item and its instance (a declaration by number, an affiliation by its new value), a
  subject that clears is released so its next raise is news at once (unless flapping), and a
  repeat held back inside the 5 minutes becomes a trigger of its own when they end, never
  heartbeat filler (a re-raised armed link stayed silent for minutes in the reviewer's
  probe); a failed turn waits before the next wake, doubling from 30 s to 15 minutes, and
  the run stops only on failures past that ceiling or more than three crashes in ten
  minutes (one overloaded minute used to end a run); a tool the CLI refused is the lockdown
  working (`lockdown.refused`) and one that ran is a breach. After the third review (10-08):
  a subject holds one repeat, the newest, with a count (every repeat stayed pending: three
  wobbling estimates piled up 5,328 changes in an hour and each later wake told the oldest);
  a clear ends a subject's escalation cooldown only on its first come-and-go in the 30
  minutes, since a condition cycling just slower than the flap window spent all six
  escalations in 27 minutes and an adversary can choose that period; Codex fails closed on
  an item type it does not know too, with `--codex-allow-item` as the escape hatch, until a
  live 0.161 run has seen the vocabulary (then log and let through); only the CLI's two
  refusal phrasings count as refused (the bare `<tool_use_error>` tag also wraps a tool that
  ran and failed); the routine model and the consult back off separately, so a failing
  routine model never holds an escalation back; the driver's own nomination poll asks about
  the armed platforms within reach only (the policy's `max_transit_s`, read from
  `get_policy`), and only whether each passes. After the final review (10-08): a repeat is
  held per item, not per subject, and any newer change of the item that is not held back
  (another value, or one that does not wake) turns it into a note told just before that
  change, since a repeat held as a trigger was told minutes after the item had moved on
  (navigation "lost GNSS" at 318 s, GNSS back since 40 s); two of the run's six escalations
  are kept for the engagement chain (hostile declarations, a nomination becoming possible,
  an armed platform's lost link with an engagement in play: the change feed now says
  `engaging`) and one subject escalates at most twice a run, since one contact cycling
  slower than the flap window still escalated every 30 minutes and three spent all six in
  seven minutes (the cooldown alone bounds a rate, not a run); the Codex backend disables
  every feature that runs commands or model-written code or reads files, confirms them off
  with `codex features list` before any turn and refuses to start otherwise, because a
  read-only sandbox does not stop reads and a breach seen in the event stream comes after
  the command ran.
- **The playbook engine: one deterministic agent as the free option, the baseline and the
  shadow** (10-09, murmur-t8m, step 1; `agents/playbook/`, `agents/playbook_agent.py`,
  `agents/README.md` "The playbook engine"; the design is the research report's Part C). A
  library of named, versioned plays, not a behaviour tree or a planner: each play is a trigger
  read from facts, a few checks, one proposal, its withdraw condition, its material change and a
  rationale template, which is what a delegation interface (Playbook, IMPACT) and doctrine-based
  automation (Aegis doctrine statements) look like, and the control flow is shallow (the
  autopilots and the task layer run the actions). One tick (2 s, or sooner on a waking change)
  reads `get_fleet` compact, `get_tracks`, `get_inbox`, `get_intents` mine and `get_changes`
  since its cursor; beliefs, plays, the arbiter, the rule book's veto and the rationale are a
  pure function of the reads, the preview answers, the config and the ledger (every collection
  sorted, floats rounded at fixed precision, the tick's time the oldest `t` among its reads), so
  a fixture of recorded reads gives byte-identical proposals and rationales. The arbiter orders
  every candidate by (class, minus score, play, target, uid) with classes P0 safety, P1
  engagement chain, P2 picture, P3 links and navigation, P4 hygiene; holds back on a cooldown
  without a material change, a back-off, a claimed vehicle (one per vehicle), an engagement in
  play (hold and rtl excepted), a busy inbox (6 pending: only P0 and P1); and the engine spends
  at most 3 previews a tick and a token bucket of human-tier cards (1 per 30 s, burst 2, at most 2
  of its own pending). Every proposal passes `preview_intent` (a failing one backs off, doubling
  from 10 s to 160 s, never dropped for good) and the evaluation's frozen rule book (imported
  from `eval/evidence.py`, so one rule book serves the engine, the proxy and the metrics; it is
  pure and importing it reads no file of the evaluation's). Rationales are rendered by a fixed
  template from an evidence record built only from values the tick read, and the record rides in
  the rationale after a delimiter line until the bridge takes a structured field; the console
  folds it behind a toggle. The ledger (JSON per run) keeps every card by (play, target,
  vehicle) with its state from the bridge, the cooldowns, the back-offs, the budget's bucket and
  each play's last status, and a restart adopts its cards from their records. The config (the
  shipped `defaults.toml`, each threshold with its reason, under a situation's `[playbook]`) is
  hashed and sent as the MCP client's version, so `agent.action session.connected` names it
  (`AgentSession` gained a `client_version`; `policy_agent.py` is otherwise as it was, and the
  evaluation's `AgentPolicy` arm moves to the engine in step 2). `AI=playbook` runs it as its own
  agent id, `agent-playbook` (in the situation roster in place of `agent-planner`, and in the
  bridge's default `MURMUR_AGENTS`), writing the watch officer's `{t, kind, text}` lines with its
  status per play when it changes; beside Claude or Codex it is the shadow, reading through the
  driver's session on its own clock, never previewing, proposing or withdrawing, and logging what
  it would have done. The criteria (`agents/playbook/CRITERIA.md`, the research's C.1 table with
  targets) were committed before its first live run and frozen; additions found against recorded
  reads before that run are a dated section there.
- **Where the playbook departs from the research design, and why** (10-09, murmur-t8m).
  (1) The bridge's `get_changes` and `get_inbox`'s review list stand in for the proposed
  `get_events` and `get_review` (both built since the report). (2) A rejection cools the play on
  that target for every vehicle, not only the rejected key, and so does an expiry: without
  structured reject reasons the operator's note is the only channel, and "not this play on this
  target" never sends them the same card with another vehicle on it; a void and a withdrawal of
  ours cool only the key. (3) The look's 0.5 gate is the track's existence confidence (the
  report's C.2.3); the rule book's 0.5 on affiliation confidence gates the armed play. Gating the
  look on affiliation would keep it off every unknown, 0.0 until a sensor classifies it, which is
  what the look is for. (4) An identity conflict is a reason to look beside an unsettled
  classification and speed: it is the tracker's own anomaly flag and what a close look settles.
  (5) A vehicle whose navigation is suspect is excluded from every tasking play, not only armed
  ones (hold and rtl still go): a sensor whose own position is in doubt places what it sees in
  the wrong place. (6) "Free" means idle (no order, or a failed one): a vehicle on anyone's
  running order is not taken, so `isr.release` is a hold in practice; resuming an earlier order
  of ours is there for when one exists. (7) `link.cover_gap` leaves a silent sensor's orbit on a
  track to `isr.investigate`, which takes the track with another sensor at its position now,
  rather than sending a filler to the old centre. (8) Leaving the watch has hysteresis (1.1 x a
  watch area's radius, 2 x the CPA alert, 1.5 x the TCPA horizon) and a track whose velocity is
  not known never counts as gone: in recorded Harbor watch reads the tracker's velocity for the
  young fast boat alternated between a value and none on every read, so a look was proposed and
  withdrawn on alternate ticks without it. (9) A look offers the arbiter its first three free
  vehicles (0.001 lower in score each), so one claim per vehicle falls to the next free vehicle
  rather than holding a second look back; the rationale names the vehicle that went first.
  (10) A card the budget cannot take is not previewed (previews are logged) unless its key's last
  preview said auto. (11) The shadow holds its would-be cards as if pending for the policy's
  approval wait (then as expired unread), claiming their vehicles and counted by the budget as
  human-tier: a model of an operator who never answers, stated in its log. (12) After the first
  live runs (CRITERIA.md, the dated section): `link.withdraw_pending` withdraws only a card still
  waiting for the operator, never one approved and held by the bridge's queue for the link (that
  would undo a person's decision over a link; the queue already re-checks it and sends a human's
  approval back after 120 s); and the closest approach is a material change only when it halved
  by at least half the CPA alert, since the estimate of a boat a few hundred metres from its
  closest approach wanders by half with a few degrees of course (a re-proposal came 4 s after a
  rejection, citing 192 m to 79 m on the same heading). (13) Not built:
  the bridge additions C.2.6 proposes beyond those that exist (structured reject reasons, a
  structured evidence field on `propose_intent`, a per-agent cap on pending human-tier cards);
  they are follow-ups. The compact fleet read has no satellite count, so fewer than six
  satellites is not seen.
- **`get_changes` is a digest compared between reads, not an event log** (10-08). The bridge
  keeps no history of an affiliation estimate or an alert clearing, so a cursor over the
  timestamps it does keep would miss exactly those; the MCP layer compares a digest of the
  read views (fleet, tracks, inbox, intents, vehicle events) between reads and numbers the
  differences in a bounded ring. Shared by all agents, read-only, not logged, never seen by
  the engine; a condition that came and went between two reads is not reported, and the
  audit log stays the record. `get_fleet` carries the GPS glitch fact the engagement checks
  read (`nav.gps_glitch`). The review queue needed no tool: `get_inbox` already carries it.
  After review (10-08): the digest is lean (`Decision::intent_briefs`, `flag_briefs`,
  `current_alerts`, the world snapshot's facts, the track store; never the full intent or
  fleet JSON, which held the core lock for every intent of the run), at most one new digest a
  second shared by all readers rather than a per-agent refusal (any number of agents then
  costs one digest a second, and nobody is turned away); a declaration is compared by its
  number (an operator's re-declaration revokes and declares in one step, so comparing
  affiliations missed it), an identity conflict by its presence (its text quotes the live
  speed), an alert by its kind and severity too; a cursor names its feed, so a cursor of an
  earlier bridge answers `reset` even where the new feed has passed its number. Operators'
  rejection notes are kept beside the engine (display only, the audit log has them) so the
  agent that proposed an intent can read why it was refused: only that agent reads the note
  (an operator's words to one agent are not every agent's), kept to 1000 characters, and
  looked up per intent. After the second review: a conflict that changes in substance is
  reported with what it was, and vehicle events are compared by their number in the ring,
  not their time (events logged together share one).
- **An agent's MCP session idles 15 minutes, eight to an agent; tools say how large an
  answer may be** (10-08). rmcp drops a session idle for five minutes by default; clients
  before protocol 2026-07-28 hold one (the Python SDK negotiates 2025-11-25), and an agent
  that connected once and waited lost it (the situation framework's storyline agent, live).
  The bridge keeps them 15 minutes (`mcp::IDLE_LIMIT`, `MURMUR_MCP_IDLE_S`): no known client
  is quiet longer than the watch's 5-minute heartbeat ceiling between calls, so three times
  that leaves room while a session a vanished client leaked lives a quarter of an hour (an
  hour, the first choice, let a client that opens a session per retry pile them up). One
  agent id holds at most eight (`mcp::SESSIONS_PER_AGENT`; the one used least recently is
  closed, and sessions their client closed do not count): the watch officer holds one plus
  one per Codex turn, the storyline one per proposal, so eight leaves room for a few clients
  sharing an id. A session counts from its initialize (the `/mcp` service records it as rmcp
  makes it: the handler never sees the new id), and one with a request in flight is never
  closed (a proposal whose answer is lost may be made again by a client that retries); with
  every session busy the cap waits. `AgentSession` closes what it opened, its HTTP client
  included, on a failed start and on a normal exit. Claude Code
  2.1.294 negotiates 2026-07-28, served without a session (checked live with a 20 s limit
  and a 35 s gap), so the watch officer's quiet spells never cost it its tools. Each tool
  carries `_meta["anthropic/maxResultSizeChars"]` (150 000): Claude Code saved larger
  answers than about 50 000 characters to a file a model with no file tools cannot read (the
  nomination dry run, 61 KB, live), so the model saw a preview; other clients ignore the
  key. The figure comes from full reads on 100 stand-ins (23 armed, 14 tracks): `get_fleet`
  131 650 characters, so a full fleet read at the scale the testbed is built for comes
  whole; the dry run over all 23 armed platforms, 213 557, does not, because an answer stays
  in a model's context for the rest of its session. `get_fleet` gained `compact` (32 494 for
  the same fleet) and the prompt steers to it and to `uids`, which both reads already took.
- **MCP list results carry `ttlMs` and `cacheScope`** (10-08). Protocol 2026-07-28 requires
  them; `rmcp` 3.5 leaves them out unless set, and Claude Code 2.1.294 then refused the tool
  list and showed the model no Murmur tool at all (connected, zero tools). Set to five
  minutes and private on the tool, resource and template lists, which never change while
  the bridge runs; older clients ignore the fields.
- **Nothing reads a vehicle under the task manager's lock or the water tracks' lock** (10-08,
  found live by a situation run; no rules change). A silent vehicle's estimate
  (`crate::estimate`, for the console feed and MCP `get_fleet`) holds that vehicle's lock while
  it asks the task manager what the vehicle was ordered (`FrameContext::ordered`) and reads its
  drift track (`water_track`). `TaskManager::orbit_of` (the decision snapshot, every second and
  every decision) and `TaskManager::dispatch` read vehicles under the task manager's lock, and the
  water sampler read vehicles under the water tracks' lock: two lock-order inversions. With a
  drone in a terrain shadow (silent, so estimated four times a second) the bridge deadlocked
  within two minutes: every HTTP worker blocked, the console and agents frozen, TERM ignored.
  Now the task manager and the sampler read vehicles before or after their own lock, never
  under it; the values read are the same.
  `tasks::tests::the_task_manager_never_reads_a_vehicle_under_its_lock` deadlocks on the code
  before (a reader holding the vehicle asks the task manager while `orbit_of` and a dispatch
  read it) and passes after. Then (10-08 review, B4) the vehicle lock became a leaf among the
  state locks (task manager, drift tracks, terrain), so no order among them is needed; it still
  takes the clock's and a relay's link locks, which are leaves. A frame (`push::build_frame`, MCP
  `get_fleet`) reads every link first, and when one vehicle is silent builds its `FrameContext`
  (what each vehicle was ordered, the fresh drift tracks) before any vehicle's lock; the estimate
  under the vehicle lock takes none of those, and the estimator's configuration is loaded at
  startup (`estimate::init`), not first under a vehicle's lock. The same review found the terrain
  cache taking its error lock under its cache lock in one place and the reverse in another (B1);
  each is now taken and released alone. The tests deadlock on the code before and pass after.
  They are bounded: each waits 10 s and fails, and the drift tracks are the world's
  (`World::water`), not a process global, so a failed test's stuck threads hold only its own
  world's locks and the tests after it run (checked by putting each old order back: one test
  fails in 10 s, the other 19 in `estimate::tests` pass, serially too).

- **A space answer is held to when it was computed; a re-check to the instant it judged; the
  pin to the run** (10-07, murmur-der.4, murmur-lsj; rules 13 `SPACE_REPLAY_SINCE`). The
  engine took a collect's answer prepared outside its lock only within 5 s of the check, but
  nothing in the log said when an answer was computed, so replay accepted one of any age.
  Decided: (1) each decision logs when its answer was computed (`computed_t`, beside the
  answer's hash in its inputs), not the `space.query` event, since one answer (by content) can
  be read by several decisions computed at different times; (2) the bound is the `space_query`
  rule's, so replay recomputes it with the check (a decision mismatch, not a separate
  binding), measured against the decision's snapshot time as the engine always measured it;
  (3) the time is bound to the answer itself, because a time alone is a number anyone can
  write: a collect's request time is the scenario time of its computation under the run's
  logged map (a re-check keeps the tasking's request time, so there the time is only logged),
  and the engine takes a prepared collect only if that holds. What stays unbound: the
  snapshot time, which is the bridge's for every decision (replay proves decisions follow from
  the logged inputs, not that the inputs were true). A cover carries its time and no bound,
  its question being independent of when it is asked. A re-check judged "has the pass come"
  on one clock read and logged its check on another, so a clock moving between the two put an
  honest re-check a millisecond past the pass for replay (which judges on the check's time):
  the re-check is now logged at its guard's instant, as a cancel's report already was (one
  clock read per judgement, the 10-06 rule for cancels, applied to every engine whatever its
  rules). The session's time shift is pinned once (the bridge has kept its pin across reloads
  since rules 11), so the engine refuses a segment pinned otherwise instead of taking it as a
  new segment, and replay flags a pin change mid-log; older logs replay as before. Stated, not
  changed: the engine never computing a cover itself (rules 11's review) holds for an engine
  of every rules version, and replay can't see who computed an answer.
- **A scenario rate needs a scenario start** (10-07, murmur-drd; no rules change). With
  `MURMUR_RATE=10` and no `MURMUR_AT`, the space API reported rate 10 on the wall clock while
  the engine's scenario map was the identity (its decisions at 1x, as the space API's answers),
  the console's clock ran at 10x between refreshes (a pass shown come or gone when it was
  not), and the stand-ins' GNSS clock ran at 10x from their own start. Two fixes were on the
  table: apply the rate from a start each process takes, or refuse a rate without a start.
  Refused: every process that reads `MURMUR_RATE` (the bridge, the stand-ins, the contacts
  process, the link emulator; `scripts/up.sh` before any of them starts) stops with a message
  naming `MURMUR_AT`, and without a start the bridge's scenario clock is reported at 1x. Why:
  without `MURMUR_AT` there is no shared origin; each process would start the fast clock at
  its own start (processes started seconds apart disagree by tens of scenario seconds at 10x,
  and a browser's start is anyone's guess), and the engine's map would need an origin nobody
  chose and the others can't know. With `MURMUR_AT`, one stated start names the scenario
  clock, as it already does for every process (`make goldengate` and the evaluation always
  set it); without it every clock is the wall clock, which runs at 1x by definition. The cost
  is one more variable for a fast run without a scenario, which was never consistent. After
  the review (10-07): the check tested whether `MURMUR_AT` was set, not whether it parsed,
  and each process parsed it its own way (chrono, jiff, a hand parser that read an offset as
  UTC): with `MURMUR_AT=garbage MURMUR_RATE=10` the bridge ran 1x while the stand-ins' GNSS
  clock ran 10x. Now one reader in `murmur_space::scenario_clock` (the crate every process
  already depends on) takes `MURMUR_AT` as an RFC 3339 date-time and nothing else, and a rate
  as a positive number; every process refuses anything else at start, and `scripts/up.sh`
  checks the same shape first, so a start one process accepts every process accepts, and
  reads as the same instant. The short form `2026-06-14T10:00Z` is not RFC 3339 and is
  refused; the defaults (`make goldengate`, the evaluation, the test scripts) moved to
  `2026-06-14T10:00:00Z`. Times inside event files keep their own, looser reading.
- **A login's options say nothing of who has a passkey, and a flood can't turn an operator
  away** (10-07, murmur-i58, security review of rules 10; no rules change). The decoy for an
  id without a passkey named one 16-byte credential id, while real ids are what the
  authenticator chose (32 bytes for the virtual authenticator, 64 for a security key), so the
  unauthenticated `POST /api/auth/login/passkey` told by length which ids exist and hold a
  passkey; and its issuance was not limited. Decided: (1) a decoy names as many credentials,
  as long, as real authenticators make, drawn from a fixed table (16 bytes: a version 4 UUID,
  its version and variant bits set, as from the password managers that make one, 15 %, or 16
  random bytes, 10 %, a split the review asked for and a judgment; 20 for iCloud Keychain, 32
  for Windows Hello and virtual authenticators, 64 for a YubiKey 5, 65 for an Android phone as
  a security key; counts: one 70 %, two 25 %, three 5 %; the structure inside a security key's
  longer ids is not imitated), deterministic per id under an HMAC key: repeat
  queries see the same answer, as for a real operator, and nobody without the key predicts
  it. Not drawn from the site's own registrations: those change when someone registers, and
  every decoy would shift at once while real answers did not. The key is kept beside the
  credential file (`<file>.decoy-key`, made once, 0600, not a credential), because a key per
  run would change every decoy at a restart and leave real ids alone, which tells them apart
  to anyone comparing before and after. For the same reason (the review of 10-07) the file is
  made whole before it has its name (written beside it, synced, hard-linked into place, read
  back: a crash or a second bridge can't leave an empty or partial key), and a key file that
  can't be read or made stops the bridge at start, saying to remove it (the decoys then change
  once), rather than falling back to a per-run key. (2) Issuance is limited per id by what is live, not
  by a rate: at most four challenges live per id (120 s each); a request past that is served
  the newest live one again, which costs nothing (no new challenge, no decision lock, no
  state), and nothing live is pushed out. Not a refusal past a rate: a stranger could keep an
  id at its limit for free and so lock its operator out. Not "the oldest gives way", as
  before: four requests then pushed the operator's own challenge out. An id without a passkey
  goes through the same table with the same bounds (its decoy redrawn from the challenge), so
  the limit is no oracle either. The bound on what a stranger can still do: they can be
  served a challenge the operator was also served and answer it wrongly (it is answered
  once), each such answer a failure on the id's login limit, which they could do with
  made-up answers anyway; so the one way to keep an operator from signing in by passkey
  stays the failure lock stated on 10-05 (five failures in 15 minutes: 30 s, doubling to 15
  minutes). A served-again challenge may have less of its 120 s left; once it lapses the next
  request is fresh. The table holds 10,000 ids; past that the id issued to longest ago goes,
  in constant time (ids are kept in the order they were last issued to), whoever it is:
  sparing roster ids would show, by how they are served after a flood, which ids are on the
  roster. So, stated: a sustained flood (10,000 made-up ids within a challenge's lifetime) can
  push out the challenge an operator is answering, and the operator asks again. It no longer
  pushes them toward the failure lock (the review of 10-07 found it did: an operator's
  genuine answer to a challenge pushed out failed and counted, the fifth round locked them
  out): an answer to a challenge that is not live (never issued, answered, lapsed, pushed out)
  can't succeed, so it is no guess, and it is refused in the same words without counting or
  logging it; only an answer to a live challenge that fails counts. (3) A wrong answer to a
  login challenge is refused in the same words whether the id has a passkey, has none, or
  does not exist (the bridge's own log keeps the reason); only a counter that did not count
  up, which takes a valid signature, is named. What still tells, stated (the review): an
  id's options flip from decoy to real when its operator registers a passkey (and back when
  the last is removed), so someone polling ids learns who registered one and when; that is
  inherent in naming the operator's credentials at all, and only sign-in without naming them
  (discoverable credentials, an empty allow list) would remove it. Timing: an operator's
  challenge takes the engine's lock and webauthn-rs's work where a decoy takes an HMAC
  (served again, both cost the same), and a wrong answer to a live challenge of an id on the
  roster writes an `auth.refused` audit event that an id off it does not, so its refusal
  takes measurably longer.
- **A keep-out that blocks is explained, read-only; nothing here relaxes one** (10-06,
  murmur-12e; no rules change). Measured live: with manoeuvring civilian traffic near the
  target, keep-outs blocked 77 of 77 nominations while the platform was 1 to 2 km out (a
  circling tour boat's rules 9 manoeuvre region, a light aircraft with no velocity
  estimate), and the operator saw a rejection naming the first track in id order, not why
  or what would help. Now the nomination form's dry run (`POST /api/engage/options`, and
  agents' `get_nomination_options`) carries `keep_outs` (`murmur_decide::keepout`): for each
  track or vehicle of ours that blocks the keep-out, or nearly does, the region that blocks
  (its course, its average course, a manoeuvre's or a young track's reach, no velocity yet, a
  lost vehicle of ours, no altitude ceiling), its numbers (the region at the effect time, the
  clearance, the shortfall), and what would clear it: a shorter span (the platform closer:
  "within 693 m of the target"), a later nomination if everything keeps behaving as now
  (a young track's average velocity, a manoeuvre leaving the tracker's five-minute memory, a
  region moving off), or why waiting does not help (lost, out of view) and what would (a
  report, a velocity estimate, the existing logged acceptance of a lost friendly). The
  console draws the blocking regions at the effect time and the platform's move-within ring.
  Read-only, and why: (1) the problem measured was understanding, and whether any keep-out
  should be tuned or overridable is the user's decision after the evaluation measures
  blocking rates (murmur-12e); an explanation that tuned or offered an override would make
  that decision by the back door. (2) A second path that could act would sit outside what
  replay binds; kept out of every decision and out of the log (a dry run is not a decision,
  10-03), it leaves replay's guarantee whole: the checks now judge each item through
  `track_standing` and `friendly_standing`, which the explanation reads too, and the 82
  scenarios in ten variants write the same 820 logs as before (the build stamp and the hash
  chain it feeds aside), the fixtures and the audit logs replay as before. (3) It cannot
  disagree with the checks: the same functions on the snapshot the dry run checked, it
  refuses to explain unless every track and vehicle it lists as blocking is one the check
  fails on judged alone, and no other, and every hint is tested by a dry run that tries it
  (`tests/keepout.rs`). (4) Agents may read it, as they read a preview; they still cannot
  authorize anything, and like the console's dry run it is not logged (an agent's dry runs
  are at most one a second). Its limits, stated: hints are conditional (a circling boat's
  wait hint holds only "if it stops manoeuvring", which it never does, and says so);
  closer and farther are on the picture now, which moves (the form refreshes every 4 s);
  waits are the first on a 1 s grid within 10 min; options stay listed by uid, unranked. A
  track out of view gets no wait hint: what happens is stated (the tracker drops a track
  unseen for 180 s, and it then no longer counts in the keep-out), never offered as
  something to wait for; whether a contact should stop protecting once dropped is a
  separate question (filed by the review). After the review of 2f4f46a: the scan of effect
  times runs to the latest a nomination allows, so a farther platform that clears what no
  closer one does is found ("between X and Y m"; it had said no position clears it), the
  dry run reads the engagements in play and the spent platforms as the engine does (both
  were missing: `platform_free` and a spent platform's readiness passed in the form), and
  our vehicles are named by callsign. Live (engage demo, the
  tour boat circling and a light aircraft with no velocity estimate): the Magura-class USV
  2.1 km out was told "within 693 m of the target"; ordered closer, the dry run still
  failed at 836 m (the hint then 816 m, moving with the aircraft) and passed every check at
  810 m, and the nomination went to the inbox with both keep-outs passing; the dry runs
  (about 40) logged nothing, and the log replays with its anchors. Since rules 16 (murmur-533,
  under Direction) a dropped track stays in the keep-out as a lost contact: the explanation lists
  one as an item (since when, what to accept), and says a track out of view stays after its drop.
- **The explanation's effect-time windows hold between grid points** (10-07, murmur-dr6; no
  rules change). A re-check of 0472a71 built two cases, neither seen on a realistic picture: a
  gap narrower than the 0.25 s grid (blocked before +50.05 s and after +50.15 s: the
  explanation said no effect time passes, and the platform at 360.79 m lands at +50.10 s and
  passes), and a dip between grid points (an 80 m/s aircraft grazing the clearance only between
  30.0 and 30.25 s, 274.98 m against 275.05 m at the grid points: "within 300 m", and at 121 m
  the dry run failed by 2 cm). Never unsafe (the engine checks again at submission), but a
  claim the checks contradict. Now a grid point counts as clear only by a margin that bounds the
  dip for anything passing at up to 100 m/s (`keepout::grid_margin_m`: `v^2 h^2 / (8 c)` for the
  grid's step `h` at the clearance `c`, and at least 0.3 m: the dip itself is 0.28 m at the default 275 m), so a
  stretch of passing points holds between them (its review: a head-on aircraft can exceed 100 m/s,
  so the margin is sized for the fastest relative motion the picture holds, the target's speed and
  each track's top speed or own bound with its uncertainty's growth, `keepout::grid_margin_for`,
  and the assumptions say it); and the sentence where no effect time passes,
  and the console's line, say "on a 0.25 s grid": a gap narrower than the grid is possible and
  now stated (finding it would take a continuous search the explanation does not do). The
  explanation reads the checks' own judgement, unchanged; `tests/keepout.rs` checks every point
  outside the windows against the clearance raised by the margin.
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
  note, under the operator's own session, logged and bound like a write-off; rules 15 adds it,
  and carries the mark across a restart, below).
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
- **An aircraft counts anywhere it could turn or speed up to** (10-07, murmur-oho; rules 14,
  `AIRCRAFT_REACH_SINCE`; first numbered 13, renumbered when the space replay took 13). A
  protected aircraft with a velocity also counts in a keep-out anywhere it reaches from its
  speed bound, speeding up to the domain's top speed (100 m/s), whether or not the tracker saw
  it manoeuvre (`engage::aircraft_reach_m`, region `aircraft`). The bound
  (`aircraft_speed_bound`) is its speed, `keep_out_sigma` of its velocity's error in its least
  certain direction (the tracker's own, a new logged fact `velocity_sigma_mps`, or the
  policy's `track_velocity_sigma_mps` when larger or not given), and the tracker's lag: the
  aircraft counts as if it had been speeding up unseen for `AIRCRAFT_LAG_S` (7 s) at
  `AIRCRAFT_SPEEDUP_MPS2` (4.5 m/s^2), its speed that much faster (no faster than the top
  speed for it) and every region of an aircraft, with a velocity or without, 110 m wider
  (`AIRCRAFT_LAG_M`). The speed-up is a bound over every source of aircraft motion here, not
  a sigma: the contacts process speeds an aircraft up at 0.5 m/s^2, the catalog's stand-ins at
  2.5 (a copter) to 4.2 (a VTOL leaving its hover), the air calibration's helicopter at 3; the
  policy's air `track_accel_mps2` (2) is a white-noise sigma. The lag is measured: the
  calibrated tracker takes in a speed-up of 1 to 3 m/s^2 only after 7 to 13 s and falls up to
  300 m behind a hard turn when EO/IR loses an aircraft; on the air tuning sets a keep-out 3
  to 100 s ahead held every aircraft every time from 5 s up (4 s missed one by 2 m), and 7 s is
  that with margin. Why: calibrated against the truth (the air tracker entry, under Fleet and
  simulation), the tracker gives a straight-flying aircraft a velocity most of the time, and a
  keep-out along its course and its average course then missed one that turned (a right angle
  takes 15 to 30 s) or sped up: against the truth on the air tuning sets, under rules 13 (the
  calibrated tracker's own regions) a keep-out 60 s ahead held a first turn 86 to 93% of the
  time, a route 85 to 92%, a helicopter that hovers and goes 89 to 93%, a break away 83 to 86%,
  and even 3 s ahead only 97.5 to 98.6% of samples in those classes. The first
  version of this region (the policy's 2 m/s^2 as if a bound, the policy's velocity error, no
  lag) missed 3 to 20 s ahead (a review of 10-07: a helicopter leaving its hover 1.2 to 2.1% of
  the time, by up to 199 m; a speed change up to 1.3%, by up to 86 m), so its claim that every
  class held was true at 60 and 100 s only. Now every class holds at 3, 10, 30, 60 and 100 s on
  every air tuning set; on the held-out set, run once, every sample with a velocity did, and two
  without one were missed 3 s ahead, by 20 m and 6 m, in some 300,000 (the tracker's tail: its
  estimate 3.5 and 7 sigmas off). The keep-out must stay at least as conservative where the
  truth says so. Options not taken: the white-noise model of rules 6 for aircraft too (it holds
  a boat's course a minute ahead about as well as the old filter did, the surface calibration's
  bar; for aircraft the bar is the top-speed reach, and it misses turns and speed changes, as
  above); the region at the aircraft's own speed alone, or without the lag (missed as above); a
  top speed by the identified kind (a misidentified fast aircraft would escape it: a loosening
  on an unconfirmed identity). Cost, stated: an aircraft with a velocity blocks about as far as
  one without at every span (at 50 m/s with a 15 m 1-sigma: 411 m at 3 s, 3.1 km at 30 s and 6.1
  km at 60 s, where one without a velocity counts 440 m, 3.1 km and 6.1 km): with this lag a
  velocity buys an aircraft almost nothing. In the keep-out measurement (one light aircraft
  within 4 km, no ADS-B, no height-finding radar), an aircraft flying straight past seen by
  radar and EO/IR blocks at 30 / 60 / 120 s 73 / 100 / 100% of nominations at 0.03 contacts per
  km^2 (rules 12 and the tracker before: 41 / 64 / 70; rules 12 and the calibrated tracker: 15 /
  31 / 42, with 2 misses at the keep-out line, a circling boat the aircraft's wider region had
  blocked by chance, where rules 14 has none). What would lift it is altitude (an aircraft
  above the keep-out's ceiling clears it), from a height-finding radar or a transponder, not
  velocity, and the keep-out explanation says so for an aircraft (it no longer offers a
  velocity estimate as the remedy). Surface, ground and subsurface tracks are judged as before.
  Replay recomputes the decisions the region changes and reads the velocity error from the
  logged facts, so a rules 14 log claiming rules 13 is caught on the fact and on the decisions
  (scenario 83's first nomination). Scenario 83; `tests/aircraft_reach.rs`.
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
  not refused (its preload reads what the platform holds now); proposed follow-up (rules 15
  refuses it, and records and compares the fleet file, below).
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

- **The carries' follow-ups are one rules version, gated on a named constant** (10-07, rules
  15, `CARRY_FOLLOWUPS_SINCE`, murmur-zq3 and murmur-imu). The entries below change what is
  logged, decided or checked, so they share one version; every gate reads the constant and
  `RULES_VERSION` is defined from it (first numbered 13; renumbered 15 in one line when the
  space replay and the aircraft reach took 13 and 14). No policy key changes (schema 7 stays).
  Logs of rules 1 to 14 replay unchanged with their own rules; the rules 12 carry tests that
  depend on rules 12's checks are pinned to it.
- **A platform spent by its own effect stays spent across a restart, for the vehicle that was
  spent** (10-07, murmur-zq3 (1); rules 15). The expended closure removed the debt, and nothing
  else carried the mark, so after a bridge-only restart (or with real vehicles) a spent
  platform took orders and nominations again while still carrying the engagement's lost-link
  action, which the next preload would read as its own values. Now a log's debts include the
  platforms it spent and nobody un-spent (the engagement, the values its closure closed, the
  vehicle they were preloaded on, and the log it is in for a mark carried into it), the start
  records them in `run.started` (`carried_from`'s `spent`), and a decision's `spent_carried`
  fact (with the bridge's identity of the vehicle now) fails `readiness`, as a mark of this run
  does, unless the vehicle is known to be another (the rule of the carried values: platform,
  callsign or system id; never the home). Replay checks the marks are exactly what the log
  carried from had spent, and every fact against the ledger. Cost, stated: after a full restack
  with the same fleet (stand-ins respawned under the same identity, or SITL, whose parameters
  persist), a spent munition stays spent until an operator un-spends it; the evaluation starts
  each run fresh (it removes `.last-run`), so it is not affected.
- **An operator with engagement authority may un-spend a platform whose engagement ended and
  that is heard since** (10-07, murmur-imu (2); rules 15; decided by the main thread on the
  user's behalf). A platform marked spent that an operator believes survived (a miss) had no way
  back. Now an operator with engagement authority un-spends it, under their own session, with a
  note: `platform.unspent`, logged with the vehicle facts it was judged on, the bridge's identity
  of the vehicle under the uid, and the values the mark closed. Un-spending reopens what it
  owes: the engagement's fallback is still its lost-link action, so its own values are owed back
  again (a mark of this run as that engagement's, holding missions as rules 7 does; a carried one
  as values carried over, for the vehicle they were preloaded on), a mission waits for the
  restore, and the bridge puts them back when the platform answers. Never an agent's (no
  `AgentPort` method, no MCP tool). The console shows a SPENT, STILL HEARD alert, only when an
  un-spend would be taken, with Un-spend (`POST /api/restores/unspend`). Not taken: requiring the
  platform's own values verified before the un-spend; that verification is the restore the
  un-spend makes due, and a mission waits for it.
  After the review (item 1): "heard after the effect" (a report after this run's mark) was met by
  the munition's own reports in its last seconds: a switchblade silent 2 s after its release was
  un-spent at 170 s on facts from 108 s, and one un-spent at 110 s while its engagement was still
  observing stayed un-spent when it was assessed "ceased" at 122 s; the alert compared last-seen
  times, which never clear, so it showed about a second after every release and stayed after the
  munition went silent. Now, one judgement for the engine, replay and the alert alike
  (`SpentMark::refusal`): the engagement that spent the platform has ended (its terminal
  transition; a carried mark's ended with its run, so it counts from this run's start), the
  platform was heard after that end (its heartbeat, position or navigation status), and it is
  not silent now (heard within the link-aware fact age limit the checks use). And (item 7) the
  vehicle under the uid is the one spent: platform, callsign and system id each known and the
  same where the spent one's identity knows them, never the home (it is set again at every
  arming); before, a carried mark's values could be reopened for another vehicle under the uid.
  Stricter than a carried hold's rule (unknown fields hold a mission there), because an un-spend
  reopens values for a vehicle, so it waits until that vehicle is known to be the one.
  A step-up (decided, not from the review): un-spending makes an armed platform taskable again,
  so in a credentials run it re-enters the credential, like accepting a lost friendly: the
  password or a passkey per the policy's `step_up_factor`, a passkey's challenge derived from the
  act (`StepUpFor::Unspend`: the platform, the engagement, the log of a carried mark). Always in a
  credentials run, not by `step_up_acts`: a new word there would change the policy schema, and
  the un-spend is consequential whatever a policy lists, as an engagement signature is. The API
  takes `password` or `passkey` and answers in order: 403 without engagement authority, 409 when
  the rules refuse the un-spend now (a dry check, nothing logged), then 401 without a valid
  step-up, so a refused un-spend never takes a password, nor counts a wrong one toward the
  session's lock; replay checks the step-up quoted. The write-off stays as it is (it authorizes
  nothing). Replay binds it all: the operator's own session with engagement authority, the
  step-up, the note, the mark named exactly, and the judgement on the logged facts and identity.
  After the second review (item 1): a carried mark counted from the new run's start whatever its
  engagement had reached, so a crash or a restack inside the assessment window (released, still
  observing) skipped the wait for the engagement to be over: carried 3 s after the cut and 4 s
  into the new run, the un-spend was taken and the alert showed at once. A mark now records when
  it was spent (its expended closure, right after the release) and whether its engagement ended
  in the log it was spent in (`spent_t`, `ended` in `carried_from`'s `spent`, checked against
  that log). One that ended counts from this run's start, as before; one that did not counts
  from the latest it would have ended, as unknown: when it was spent plus `assess_window_s`, if
  that is later than the start. Not taken: carrying the engagement itself over (engagements never
  carry: the plan, the picture and the signatures are the run's).
- **A nomination on a platform a failed carry may owe is refused until that log is read or
  written off** (10-07, murmur-imu (1); rules 15). Rules 12 held a mission for an armed platform
  of a failed carry's fleet but took a nomination, whose preload would read what the platform
  holds now (possibly an earlier engagement's fallback) as its own values and put that back
  afterwards. Now an engagement decision's `restore_owed` fact names the hold (values unknown,
  the vehicle it was, the one under the uid now) and `fallback_preload` fails on it, at the
  nomination and on the plan; another vehicle under the uid is not refused. Replay recomputes
  the fact from the ledger and the check from the fact. The mission paths read only missions'
  facts (an engagement's is its check's, never a refusal or a queue hold). `may_owe` was already
  a fact of `run.started` (inside the successor's chain, bound by its start anchor); what lay
  outside the chain was its source, the fleet file: a failed carry now records the file it read
  (`fleet_read`: its SHA-256 and the catalog's armed platforms), replay compares `may_owe` with
  the file when it is at hand (`--prev` by name or beside the log) and the armed list with every
  platform fact the log's decisions read (murmur-zq3 (6)). Residual, stated: without the file at
  hand, `may_owe` is bound by the start anchor only.
  After the review (item 5): replay compared the file only when the start recorded one, so a
  forged start that dropped both `fleet_read` and `may_owe` (and the decisions' may-owe facts)
  replayed clean with the file passed in. Now a file at hand is compared whatever was recorded:
  one that reads, where the start recorded none, is a mismatch (the start claimed it could not
  be read). And (item 8) a pending log tried again whose fleet file is gone too keeps the list
  the log that named it pending recorded (its vehicles stay held, instead of none); replay
  compares it with that log when it is at hand.
- **A pointer that can't be read falls back to the station's newest log that checks** (10-07,
  murmur-zq3 (2), (3), (4); rules 15). A corrupted pointer was recorded unknown and dropped the
  chain behind it (nothing held until written off). Decided: fall back only to a log whose hash
  chain and anchors check (every problem but the shutdown anchor a crash never wrote), record
  that it was used (`pointer_failed`, the first entry of `carried_from`), and stay fail closed
  otherwise. The audit directory is shared by stations (one per HTTP port), so the newest log
  in it may be another station's, whose values carried here would be put on our vehicles: each
  run now names its station's pointer in `run.started` (`pointer`), and only a log naming the
  same pointer is a candidate, the newest by its start; only that one (if it does not check, no
  older log is tried: the newest one's debts would drop unsaid); a log of older rules names no
  pointer and is never taken. With a fallback the pointer's own failure is not kept pending:
  every start rewrites the pointer to its own log, so the station's newest log is the one it
  named, or a later one that carried its debts. Replay checks the fallback is the first entry,
  of a run that names its pointer, and that the log given names the same pointer. A pointer of
  the same line twice names that log (two different lines: unknown, as before). The pointer's
  directory is synced after the rename, so the rename itself survives a power loss.
  After the review (items 3, 4, 6): the newest was ordered by the start's wall-clock time and
  name (either can invert under this host's clock steps), and only logs naming the pointer were
  candidates, so a newer log of the same station from an older build (naming no pointer, the one
  the pointer named, still owing values) was passed over silently for the log before it; and a
  fallback that did not check recorded the pointer with nothing held (no `may_owe`), dropping
  that log's own pending carries. Now the station's logs are those naming its pointer and, by
  the carry chain, every log that carried from one of them (a start carries only from its own
  pointer's log, so such a log is the station's whatever its build); the newest is the one no
  log carried from (two such: the later start, refused if they started within 10 s of each
  other). It is carried from only if it names the pointer, its chain and anchors check, and no
  other log in the directory that names no pointer, or whose first line does not parse, started
  (or was last written) after it less 10 s: that log may be the one the pointer named.
  Otherwise the newest is recorded as a failed carry under its own name, with its armed
  platforms (`may_owe`, `fleet_read`) and why the pointer failed (`pointer_failed`): missions
  and nominations on them are held (fail closed), and it stays pending, so the next start reads
  it and the chain behind it. Replay takes such an entry as the start's refusal, not a dropped
  debt (the log stays pending).
  After the second review (items 2 to 4): a refused log held only its fleet file's armed
  platforms, so without a fleet file (every fallback is refused when anchors are off, and a
  bridge that ends before its first tick, a busy port, writes none) it held nothing: an orbit
  for a munition that log had spent was approved, and replay, which no longer flags a readable
  failed carry here, said ok. Now it holds also every platform the log owed values to, had spent
  or held for a failed carry of its own (read from it), and replay requires that when the log is
  at hand. And only the newest was recorded, so a competing log the refusal named ("it may be
  the log the pointer named") dropped at the next start: now every such log is a failed carry
  too, in the pointer's place: another end of the chain started within 10 s, a newer log that
  names no pointer or does not parse, and a log that is gone while its fleet or anchors file, as
  new, is still here (deleting the newest log otherwise left its predecessor as the chain's end,
  carried from silently). Each stays pending until read or written off; one that may be another
  station's is held all the same (fail closed), and an operator writes it off. Without any log of the station the pointer is a failed carry,
  as before. A directory that can't be synced after the rename is a warning, no longer a failed
  pointer: the rename is done, and the next run finds the log (a power loss before the system
  flushes the directory could bring back the old pointer).
- **A void that says it was superseded names an order the log has** (10-07, murmur-zq3 (5);
  rules 15). Rules 12 accepted any void reason beginning "superseded" in place of a refusal.
  Replay now reads the order and vehicle the engine's and the bridge's reasons name: the order
  must be in the log, newer than the voided one, on that vehicle (an amend's, of the same kind);
  an engagement's stop is checked by its cause, as before. Found doing it: the scenario runner
  voided a newer pending order when an older engagement was dispatched, unlike the bridge (a
  newer order is never voided by an older one's dispatch); the runner now does as the bridge
  (scenario 22's log changed, its outcome did not).
  After the review (item 7): the order named must also be in the state the reason says: one a
  dispatch voided for was dispatched, one a queueing voided for is queued, and a dequeue's newer
  order stands (approved, queued, dispatched, or a person's waiting for approval), as the
  engine's own test of it reads.

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
  shared cap; an unanswered challenge leaves nothing in the log. (Logins revised 10-07,
  murmur-i58: decoys drawn like real credentials, and a login's options limited per id
  without the oldest giving way; see "A login's options say nothing of who has a passkey".)
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
  and the form preselects nothing: the operator picks the platform. From 10-06 each option
  also explains its keep-outs, read-only (murmur-12e, under Engineering).
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
