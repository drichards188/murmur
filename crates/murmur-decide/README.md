# murmur-decide

Murmur's decision layer (Phase C in `docs/PLAN.md`): intents, fail-closed rules, risk-tier
policy, an append-only audit log, and deterministic replay. A standalone library with one
small binary (`murmur-replay`). It does not depend on the bridge; the bridge calls it.

## The model

Every action, from a human or an AI agent, is an **intent**: what (orbit, patrol, hold, rtl,
track; collect and cover for satellites; engage, Phase E, below), to which vehicles (by
`uid`; none for collect and cover, which request satellites, not fleet vehicles), with what
constraints, why (required for agents), from whom. It moves through one state machine:

```
proposed -> checked -> auto_approved | pending_approval
         -> approved | rejected | expired | voided -> dispatched -> done | failed
checked -> approved   (authorize_at_submission: a human's own intent in a human tier)
dispatched -> effected -> done | failed       (engage: release, then the assessment)
dispatched -> aborted                         (engage: stopped before the effect)
auto_approved | approved -> queued            (rules 5: a vehicle can't be reached now)
queued -> dispatched | pending_approval | voided | expired   (its re-check, or its deadline)
dispatched -> queued                          (rules 5: the link lost before any vehicle started it)
```

The full table is `intent::next_state`; anything else is a typed `IllegalTransition`.

1. **Rules** (`rules.rs`) are pure functions of the intent and a `WorldSnapshot`:
   capability (the KINDS table), readiness (nav ready, fresh), platform limits (orbit radius
   vs minimum turn radius, with a suggested radius instead of silent widening), domain (boats
   and subs on water, UGVs on land, via a `TerrainOracle`), geofence (no-go polygons from
   `sim/areas/*.geojson`), reserves (endurance left after the task). **Fail-closed**: a rule
   that can't evaluate (unknown vehicle, missing or stale fact, no area file, unanswered
   terrain point) fails and says why. Freshness is link-aware: a vehicle's limit is
   `max(max_fact_age_s, fact_age_intervals x its report interval)`, the interval being the
   `report_interval_ms` world fact (20 s for an acoustic AUV, so 60 s); the check detail names
   the limit, and a policy without `fact_age_intervals` keeps the flat limit. Two recorded exceptions:
   - `not_evaluated`: deliberately skipped, blocks only consequential kinds (engage). The
     domain rule with no terrain oracle (PLAN.md), and an rtl whose way home can't be
     checked (no fresh position, no area file): rtl is the safety command and must not be
     blocked by missing facts.
   - `unverified`: can't be verified, so a human must judge it. The reserves rule on a
     platform with no `endurance_h` (fvr90, lta, the four UGVs, magura, corsair): "not
     evaluated: endurance unknown ... needs a human". A battery already below the reserve
     still fails. (`unknown_endurance = "fail"` or `"battery_now"` in `[rules]` restore the
     old behaviors.)
2. **Routes** (`route.rs`). The geofence, the approved-area condition and the reserves
   estimate all look at the same three parts of each vehicle's route: the **transit** leg
   from its current position to waypoint 1, the **task path**, and the **return** leg from
   the task's end to home (what RTL flies). Orbit and patrol have all three (a loop ends at
   waypoint 1); rtl is only the return leg (position to home); hold and track have none.
   Geofence results are per vehicle, and details name the leg: `transit leg (position to
   waypoint 1) crosses alcatraz`, `return leg (waypoint 1 to home): home is inside ...`,
   `task path: leg 1-2 crosses ...`. A leg that starts at the vehicle inside a no-go area
   is leaving it and is allowed, so a vehicle that strayed in can be brought out.
3. **Policy** (`sim/policy.toml`, first match wins): hold/rtl auto; any task on an
   `armed = true` platform (switchblade, fpv, ugv_armed) needs a human; leaving the approved
   area (any leg of any vehicle's route) needs a human; an `unverified` reserves check
   (`when = { unverified = ["reserves"] }`) needs a human; else auto. No match means human.
   Unknown platform counts as armed; no area file, or no fresh position or home, counts as
   leaving. One floor holds whatever the file says: an intent with any `unverified` check is
   never auto-approved (an auto match becomes human, rule `(unverified)`). The decision's
   `facts.unverified` lists those checks, so the inbox can show why a human is needed.
4. **Approval**: a human tier needs a human decision, and who that is depends on where
   the intent came from.
   - **A human's own submission** (`Engine::submit`, `amend_collect`; the console's
     `POST /api/tasks`) is a human decision by construction: it comes with the submitter's
     `HumanAuthority`. If the matched tier has `submitter_authorizes = true` (per tier in
     `sim/policy.toml`, else `[approval] submitter_authorizes`, default true; the
     `(unverified)` and `(none)` fallbacks use the default), the intent goes from `checked`
     straight to `approved` (transition `authorize_at_submission`), logged as its own event
     `approval.at_submission {by, session, self_approved: true, rule}` and recorded on the
     intent as `approval {by, at_submission: true, self_approved: true}`. Not
     `auto_approved` and not `approval.granted`, so evaluation can tell "policy, no human",
     "a human decided by submitting" and "a human approved in the inbox" apart. The checks
     run first and still fail closed: a rejected intent stays rejected. The approval-time
     re-check doesn't apply (the checks just ran); voiding (vehicle lost, superseded,
     cancel) applies afterwards as before. The policy decision records it
     (`submitter_authorizes: true`).
   - **An agent's proposal** (`AgentPort`) in a human tier always waits in the inbox,
     whatever the policy says: the at-submission path needs a `HumanAuthority`, which the
     agent path doesn't have.
   - **`submitter_authorizes = false`** on a tier makes the submitter's own intent wait in
     the inbox too.
   - **Two-person approval**: `approvers = N` on a tier needs N distinct signers (the first
     N-1 logged as `approval.concurred`, the last as `approval.granted`), and
     `proposer_may_approve = false` refuses the proposer (logged `operator.action
     approval.refused`). An engagement always excludes its proposer and needs an
     `EngagementAuthority` (below).
   Pending intents expire (default 120 s; per kind `[approval.kind.<kind>]` with its own
   `expire_s`, or for collect `until = "tasking_lead"`: the chosen pass's collect time minus
   the class's tasking lead time, mapped to engine time through the segment's scenario map,
   logged in `approval.requested` as `until {until, scenario_t}` and recomputed by replay). Approving re-runs the checks against the
   current world; if the picture no longer holds, the intent is voided instead. A human
   may approve an intent they proposed; the approval then records `self_approved: true`
   (on the intent's `approval` and the `approval.granted` event) so evaluation can
   measure it.
5. **Audit**: every step is a JSONL event `{seq, t, prev, type, ...}` written before the
   change it records (a failed write means no change). Decision events carry the exact
   world facts they read, each with its observation time; the policy file is logged at run
   start. The area set is logged once, in `run.started` (with `Engine::with_areas`) or as
   `areas.loaded` the first time a decision reads it and again only when it changes, with
   `sha256` = SHA-256 of its canonical serialization (compact `serde_json`, fields in
   declaration order, no maps). Decision inputs carry `areas_sha256` instead of the
   polygons (about 700 bytes saved per decision event).

## The queue for unreachable vehicles (rules 5)

`queue.rs`, murmur-5p4. Orders to vehicles that can't be reached right now (a silent link, a
satellite outage, a jammed radio, an AUV under water) are neither lost, nor sent into a dead
link to fail, nor sent twice, nor sent stale.

- **Dispatch or queue** (`Engine::dispatch_or_queue`, called for every approved or
  auto-approved intent): `queue::reach` on the world now says whether each vehicle can be
  reached for this order: heard within `link_intervals` of its report interval (its
  heartbeat, the `autopilot` fact: 3 s streaming, 60 s on an acoustic link); for anything
  but a hold or rtl, its navigation status and position fresh again by the checks' own limit
  (a link that comes back brings the heartbeat first); and for a mission upload (orbit,
  patrol, a sub's rtl) a link that reports at least every 5 s
  (`MISSION_LINK_MAX_INTERVAL_MS`: not an AUV on its acoustic modem). All reachable:
  `intent.dispatched` with the inputs and reach that allowed it (`Engine::dispatch(id,
  world)` does the same and refuses an unreachable vehicle; `dispatch_order` refuses a kind
  the queue holds). Otherwise `intent.queued` (the inputs the judgement read, each vehicle's
  reach, the deadline) and the intent is `queued`. Satellite requests and engagements are
  never queued, nor a kind whose wait is 0.
- **Deadline** (`[queue]` in the policy, schema 3): `expire_s` (default 900), per kind
  `[queue.kind.<kind>] expire_s` (the shipped file: hold 60, rtl 120; 0 never queues),
  `human_expire_s` (120: a human's approval is held at most this long after it, then the
  order goes back to the inbox instead of expiring; `QueueConfig::bound`), `link_intervals`
  (3). Bounded (`QueueConfig::validate`: 1..3600 s, 0..3600 per kind, 1..3600 for a human's
  approval, 1..10 intervals, vehicle kinds only); replay validates the logged copy too.
- **Requeue** (`Engine::requeue`): a dispatched order whose link was lost to every vehicle
  before any confirmed it goes back to the queue (`intent.requeued`, its deadline the kind's
  wait from the approval, bounded as above), re-checked like any queued order before it is
  sent again. Each vehicle's last op state is `requeued` (the link went before the start)
  or `unconfirmed` (after it: an op `starting` means the start went out, the mission mode,
  the hold or rtl mode, or new items to a vehicle already flying a mission), and none ever
  `acked`. The bridge then delivers it asking the vehicle what it holds first
  (`Intent::dispatched_before`); where it may have started (`Intent::maybe_started`) the
  delivery only confirms it (`Intent::confirming`): followed if the vehicle runs it, and if
  not (`unconfirmed` again) it is never started again on its own: `intent.requeued` with its
  deadline now and `queue.expired` send it to the inbox, saying what the vehicle does. A
  human's approval there may start it again. An order that ends or goes to the inbox while
  it may be running says so in its reason.
- **Never an older order over a newer one**: a dequeue voids the intent when a newer one for
  one of its vehicles stands (approved, queued, dispatched, or waiting for a human after a
  human submitted it); a human's approved order waiting in the queue, or dispatched with no
  vehicle started on it yet, makes an agent's order on that vehicle a human decision (the
  `waiting` fact, policy rule `(human order waiting)`).
- **Dequeue** (`Engine::dequeue`, the bridge offers every queued intent each second): past
  the deadline it expires (`queue.expired`, with why; or, `inbox`, back to the inbox with a
  new `approval.requested`); while a vehicle can't be reached nothing happens and nothing is
  logged; otherwise the checks run on the world now
  (`check.result`, phase `dequeue`), the policy decides on the same inputs, and
  `queue.recheck` logs the reach, the decision and the verdict (`queue::assess`) against
  what the approval rested on (`Basis`: the checks, the decision and the engagements in
  play when it became `approved` or `auto_approved`, and who approved it):
  - a check fails: **void** (fail closed, as an approval re-check would);
  - an engagement now in play on its vehicles that was not then: **inbox** (its dispatch
    would stop it unacknowledged);
  - the policy now needs a human and none approved it: **inbox**;
  - a human approved it and something changed: a check's outcome, the policy tier, rule or
    approvers, a vehicle that rebooted after the approval (a `vehicle.rebooted` logged after
    it, or `booted_t` later than it by more than 30 s), an approval from the run before a
    bridge restart: **inbox**, where a human approves it again (the old approval is dropped;
    `approval.requested` gives a new deadline);
  - otherwise **dispatch**: `intent.dispatched` exactly as approved, right after the re-check,
    which is judged before the deadline or not at all.
- **Cancel**: an operator cancels a queued intent (`void`), an agent withdraws its own.
- **Restart** (`Engine::resume`, `ReplayReport::open`): the intents a log leaves open (a
  vehicle task queued, approved but not dispatched, or dispatched with ops still running)
  can be carried into the next run's log as `intent.resumed`, keeping their ids (new ones
  number on from `ReplayReport::next_intent`, `Engine::reserve_ids`), with their basis,
  decision, approval (`CarriedApproval`), deadline, each vehicle's last op state, the
  vehicles that ever started it (`committed`: sticky across op states and runs), whether it
  was ever dispatched (`dispatched_before`) and each vehicle's platform as last checked. One
  that some vehicle had started resumes `dispatched`: the bridge follows it there and never
  sends it again; any other resumes `queued` and is re-checked before it goes (a human's
  approval goes back to the inbox: `basis_carried`), and, if it was dispatched before,
  checked against what the vehicle holds first.
- **Reboots**: `Engine::record_reboot` logs `vehicle.rebooted` (the time since boot before
  and after, the new boot instant and, when the time since boot did not go back, the one
  before: a jump of more than 30 s); the bridge ends a running op on a rebooted vehicle.

**Replay.** `intent.queued` is recomputed (reach from its inputs, at least one vehicle
unreachable, the deadline from the policy, approved before, a kind the policy queues);
`queue.recheck` must follow its dequeue check directly, and its reach (all reachable),
decision and verdict are recomputed from that check against the basis replay rebuilt from
the approval; the next transition must carry out the verdict (dispatch, back to the inbox
with a new `approval.requested`, void), and a dispatch from the queue needs a re-check that
says so, logged right after it and before the deadline; a queue expiry needs its deadline
and its kind (expired or back to the inbox). Under rules 5 a dispatch straight from an
approval of a kind the queue holds needs its reach (recomputed: every vehicle reachable). An
`intent.requeued` needs every vehicle's last op state `requeued`, none ever started, and its
deadline from the policy. Each decision's `waiting` fact is recomputed from the queued
intents. `intent.resumed` is checked against the previous log (`ReplayOptions::previous`,
`murmur-replay --prev`, by default the file it names beside the log): that log is the one
named (head, last seq), replays, and leaves the intent open exactly as carried; without it a
resumed intent is a mismatch unless trusted (`--trust-resumed`). One generation: replay each
log with its own predecessor.

## Engagements (Phase E)

`engage.rs`: the engagement decision chain, a C2 decision study with an abstract effect (a
point, a radius, a time); nothing models a weapon, guidance or an aim point. `Task::Engage
{track, effect_radius_m, window, fallback}` names a perception track and one armed platform
(`uids`); a missing window is written into the spec at creation.

- **Checks** (`engage::evaluate`, after the vehicle rules, each pass / fail / unverified /
  not evaluated; engage is consequential so the last two block): `target_declared`,
  `target_identity`, `target_recency`, `target_uncertainty`, `engagement_zone`, `no_strike`
  (area kinds `engagement_zone` and `no_strike` in `sim/areas/*.geojson`),
  `keep_out_friendly`, `keep_out_tracks`, `platform_armed`, `reach`. Thresholds in
  `[rules.engage]`, each bounded (`EngageRules::validate`). The effect circle must lie
  wholly inside a zone and clear of every no-strike area. The effect point's land/water
  answer comes from `TerrainOracle::sample_near` (a sample within `TERRAIN_NEAR_M`, logged
  with where it was taken; not evaluated when the samples that near disagree).
- **Preview** (`engage::preview`): straight-line intercept at catalog cruise speed against
  the track's constant-velocity projection, after `dispatch_delay_s`; the effect point and
  time; the uncertainty ellipse. Logged on every `check.result` (`preview`), recomputed by
  replay.
- **Authorize**: `Engine::approve_engagement(id, &EngagementAuthority, ...)`. The authority
  lives only in a session opened with `Engine::open_operator_session_with(op, Grants {
  engage: true })` (the grants are logged with `session.opened`). `approve` refuses an
  engagement. Policy floor in `policy::decide`: never auto, never at submission, proposer
  excluded, never fewer than two approvers (`ENGAGE_MIN_APPROVERS`; replay holds it too).
  A signer is shown the picture first (`Engine::issue_picture`, a `PictureToken` bound to
  the session, logged `picture.issued`) and the signature quotes it
  (`approve_engagement(id, &auth, note, token, world, terrain)`). The first signature
  freezes the plan it was shown (`FrozenPlan`); every later signature signs that plan, and
  the checks run on it (`WorldSnapshot::plan`); when it stops holding, at a signature or in
  `step_engagements`, every signature is dropped with it (`approval.reset`). The target is
  predicted with `target_velocity` (none below `holding_speed_mps`), and the plan and
  deviation checks allow `sigma_growth_mps` times the span for velocity-estimate wobble,
  at most `plan_drift_max_m`. One engagement per platform (`platform_free`). An agent's order on a vehicle with an engagement in play goes to a
  human (`(engagement in play)`; the engagements in play are logged with each decision as
  `engaged`).
- **Hold the picture**: the final `approval.granted` carries an `EngageSnapshot`;
  `Engine::step_engagements(world)` (once a second) logs `engage.picture` (declared,
  reclassified, recency, deviation, keep-outs, the platform's link, the window and
  lateness; once dispatched also arrival, effect time, alignment and uncertainty) and voids
  (approved) or aborts (dispatched) with `engage.stopped` when it fails, releasing a
  `FallbackOrder`; when it holds and is ready it logs `engage.effect` and returns an
  `EffectOrder`. After the effect it logs `engage.observe` each second and finally
  `engage.assessed` (persists, ceased, unknown).
- **Stop**: `Engine::abort_engagement(id, &HumanAuthority, note)` (any operator),
  `Engine::stop_engagement_system(id, reason)` (the bridge: task failed) and
  `Engine::supersede_engagement(id, by)` (another order dispatched to its platform: cause
  `superseded`, `fallback: null`; refused unless `by` was dispatched to vehicles that
  include the platform). A picture more than `max_picture_gap_s` after the last
  fails `picture_gap`. `step_engagements` returns a `StepOutcome`: the events to act on,
  and each engagement whose step failed, on its own.
  `dispatch` and `finish` refuse engagements; `dispatch_order` returns `Order::Engage`.
- **Lost-link preload** (rules 6, murmur-der.2). A fallback the bridge commands after a
  stop can't arrive over a dead link, so the approval's fallback becomes the platform's own
  lost-link (GCS) failsafe while the engagement is under way. `fallback_preload` checks at
  nomination and on the plan that the platform's firmware has such an action
  (`engage::preload_for`, ArduPilot 4.8):

  | firmware | return | abort | loiter | timer |
  |---|---|---|---|---|
  | ArduPlane (and QuadPlane in fixed-wing modes) | `FS_GCS_ENABL` 1, `FS_LONG_ACTN` 1 (RTL) | none | none | `FS_LONG_TIMEOUT` |
  | ArduCopter | `FS_GCS_ENABLE` 1 (RTL) | 7 (Brake, else Land) | 7 | `FS_GCS_TIMEOUT` |
  | ArduRover (boats, UGVs) | `FS_GCS_ENABLE` 1, `FS_ACTION` 1 (RTL) | `FS_ACTION` 2 (Hold) | `FS_ACTION` 6 (Loiter, else Hold) | `FS_GCS_TIMEOUT` |
  | ArduSub | none | `FS_GCS_ENABLE` 3 (depth hold) | 3 | `FS_GCS_TIMEOUT` |
  | Blimp, AntennaTracker, drifter | none | none | none | |

  ArduPlane's `FS_LONG_ACTN` is continue, RTL, glide, parachute, AUTO or AUTOLAND: nothing
  holds where it is, so a fixed wing can only take fallback `return` (its "continue" would
  fly the intercept on to the effect point and circle there). ArduCopter has no circling
  lost-link action: a multirotor's hold where it is is BRAKE, used for `loiter` too; its
  `FS_OPTIONS` bit 1 (continue a mission in AUTO on GCS loss) is cleared. ArduRover's
  `FS_GCS_ENABLE` 2 would continue in AUTO, so it is set to 1. The timer is at most the
  approval's link-gap limit (`link_gap_s`, or `link_gap_intervals` report intervals), in
  whole seconds and at least 2 s (`PRELOAD_TIMER_MIN_S`, the low end of the documented
  FS_GCS_TIMEOUT range): the platform takes its fallback about when the engine stops the
  engagement for silence, instead of after its link class's own timer (30 s on LEO,
  120 s on GEO and Iridium, `sim/links.toml`). ArduRover acts `FS_TIMEOUT` (1.5 s) after it.
  The engagement order carries the preload (`EngageOrder::preload`, from the final
  signature's world: the platform's autopilot and the snapshot's fallback and link-gap
  limit; replay recomputes it). The bridge reads the platform's own values, sets the
  preload, reads it back and reports it (`Engine::record_preload`, logged `engage.preload`
  with `confirmed` recomputed from the read-back); only then does it send the intercept. A
  `set` that does not confirm (no answer, another value read back, a timer too long) stops
  the engagement at once (cause system, `the platform's lost-link action could not be
  preloaded: ...`) with its fallback; and the release checks it too (`fallback_preload` in
  the release checks: nothing is released without a confirmed `set`, so a preload that is
  never reported ends the engagement late, with its fallback). Near the release it is read
  again: from `PRELOAD_VERIFY_LEAD_S` (30 s) before the release window opens, every
  `PRELOAD_REREAD_MS` (5 s; `engage::preload_verify_due`), each logged as `engage.preload`
  verify. The release needs a confirmed read-back (set or verify) at most
  `PRELOAD_FRESH_MS` (10 s) old, so a value another station (or anything else) changed
  after the set shows before anything is released; a verify that does not confirm stops the
  engagement (`the platform's lost-link action changed: ...`) with its fallback. The bridge
  runs one parameter job at a time on a platform's link (read, write, verify or restore).
  The write is logged ahead: the platform's own values, once read, go into the log as
  `engage.preload` intent (with the vehicle's identity: platform, callsign, system id,
  home) before anything is written, and the set's `before` must be exactly those (the engine
  refuses, and replay flags, a set without its write-ahead or with other values); a bridge
  that stops between the write and its report still finds them owed in its log. An
  engagement that ended before its write-ahead never touches the vehicle. The platform's own
  values are owed back from the write-ahead on: a newer engagement dispatched on it before
  they went back takes them over (its write-ahead logs them and does not read them off the
  still-preloaded vehicle; replay flags one whose values differ from those still owed), and
  they go back only once no engagement holds the platform (none under way or waiting for its
  set): the engine refuses, and replay flags, a restore (`engage.preload` restore or
  `platform.restore`) while another engagement is under way on the platform. The restore
  (`engage.preload` restore by the engagement that preloaded last, or `platform.restore`
  when the engine refuses that or none of this run owns it) is retried with backoff (15 s,
  doubling to 300 s), every attempt logged, with an inbox alert (`restore_pending`) while it
  stays unconfirmed for 30 s. Values still owed when the bridge stops are carried over:
  at its next start it derives them from its previous log (`replay::owed_restores`; the
  file `logs/audit/.last-run-<http port>` names that log; a reused run id carries from the
  part of the same file before this run's `run.started`), logs them as `platform.restore`
  carried with the vehicle's identity, and puts them back only onto a vehicle that matches
  it (platform, callsign, system id, home within 500 m: `VehicleIdentity::matches`; else
  nothing is written and the inbox says why). A pointer to a log that can't be read is an
  inbox alert too, not a silent first start. Replay reports every platform whose values were
  still owed when the log ended (`ReplayReport::unrestored`, printed `UNRESTORED:` by
  `murmur-replay`); it is not a decision mismatch.
- **Keep-out model** (rules 6, murmur-der.3; parameters in `[rules.engage]`, each bounded,
  schema 4):
  - *Tracks* are predicted along their velocity with a 1-sigma of `sqrt(pos_sigma^2 +
    (track_velocity_sigma_mps x span)^2 + q x span^3 / 3)`, `q = track_accel_mps2^2 x 1 s`
    (`track_prediction_sigma`): the tracker's own uncertainty now (already grown for the
    track's age), the velocity estimate's error, and its manoeuvres as white acceleration
    noise (the continuous white-noise acceleration model: Bar-Shalom, Li and Kirubarajan,
    *Estimation with Applications to Tracking and Navigation*, 2001). Before, the growth was
    the tracker's `max(1, 0.3 x speed)` times the span: a 20 m/s ferry 300 s out had a
    2-sigma of 3.63 km, now 1.34 km (and a 6 m/s boat the same 1.34 km: speed is not
    uncertainty). Defaults and basis: `track_velocity_sigma_mps` 1.0 (the Phase G
    velocity-noise sweep passed every scenario at 0.5 m/s per axis and saw late aborts at
    1 m/s per axis; the Phase G live tracks' estimates moved by at most 1.3 m/s from one
    second to the next, AIS ones 0.3); `track_accel_mps2` 0.2 m/s^2 on the surface and under
    water (a ferry or patrol boat in a 300 m turn at 10 m/s pulls 0.33 m/s^2; Bar-Shalom's
    rule of thumb puts the noise between half and all of the largest manoeuvre), 2.0 in the
    air and on the ground (a standard-rate turn at 55 m/s is 2.9 m/s^2). A track without a
    velocity estimate could be anywhere it reaches at its domain's top speed (25 m/s on the
    surface, 100 in the air, 30 on the ground, 15 under water): fail closed. Conservative
    where it matters: the keep-outs are read again every second while the plan waits and
    after approval, and at the release over a span of seconds, where the model is the
    tracker's own uncertainty; a track that turns toward the point shows in its next
    estimate.
  - *Altitude*: where the target's altitude (the effect's) and the other's are both known,
    and a track's is a recent ranged measurement (`engage::track_altitude`: its own time,
    `alt_t`, at most `track_alt_max_age_s`, 10 s, old, and a ranging sensor that hit the
    track that recently in `source_t`: radar for an aircraft, radar or its own AIS report for
    a vessel; never a ground track, whose 0 m from the sensing model is not its height above
    sea level, nor a subsurface one, whose sonar fix is bearing-heavy; never from passive
    sensors alone, EO/IR or ESM: a bearing, no range; and not an altitude a radar measured
    minutes ago while EO/IR keeps the position fresh), the keep-out has a ceiling and a
    floor: `effect_radius_m + keep_out_vertical_m` (default
    `keep_out_m`) above and below the effect, each side's altitude counting with
    `keep_out_sigma` of its error (a track's: 30 m in the air, the sensing model's radar
    height error; 10 m on the surface and ground, 5 m under water; ours: 10 m, GNSS
    vertical) and with how far it could climb or descend by the effect time, a track's
    counted from when its altitude was measured, not from its position's time (ours: the
    largest of `vertical_rate_mps`, 10, the catalog's climb rate and, for a fixed wing, its
    dive at its top speed (`max_mps`, else cruise) down `FIXED_WING_DIVE_DEG`, 25 degrees,
    ArduPlane's default `PTCH_LIM_MIN_DEG` (an MQ-9B at 105 m/s: 44 m/s), either way
    (`friendly_vertical_mps`); aircraft tracks `vertical_rate_mps`; surface tracks 0). An
    unknown, stale or unbacked altitude, either side's, leaves the keep-out 2D (no ceiling),
    as before. A MALE aircraft at 7,000 m over a surface effect two minutes out is clear (it
    can't dive below 1,600 m by then); at 3,000 m it is not; a quadcopter at 120 m is not.
    The bridge's sensing model gives an altitude only where the sensor ranges the contact in
    height: a radar or AIS for a contact on the water (sea level), and for an aircraft only a
    radar that measures elevation (`height = true` in the catalog: the CCA's fire-control
    radar; the USVs' navigation radars and the MALE aircraft's surface-search radars are 2D);
    none from EO/IR, ESM or sonar, none for a ground contact. The tracker carries the
    altitude's time and each source's last hit into the decision facts (`alt_t`,
    `source_t`).
    Our vehicles' altitude is a new world fact (`alt_msl_m`, with the position it came
    with).
  - *Lost friendlies*: one of ours whose position is not fresh still blocks by how far it
    could reach (or, without a position, at all), and the detail says since when (`position
    unknown for 312 s (since t=...)`). An operator with engagement authority may accept
    that, explicitly and logged: "vehicle X position unknown since T: accept"
    (`Engine::submit_accepting` at the nomination, `Engine::accept_unknown_friendly` while
    it waits with no signature; both take only an `EngagementAuthority`; logged as
    `engage.accepted` with the operator, the session, the world's time and the vehicle's
    facts then, so replay recomputes that it was lost since exactly that report by the same
    link-aware freshness rule, `engage::lost_since_report`). The friendly keep-out then
    leaves that vehicle out while it is lost since exactly that report; one that reports
    again counts like any other, and a newer loss needs a new acceptance. Every signer sees
    the acceptances: the picture token carries them (`PictureToken::unknown_accepted`), and a
    signature on a token taken before they changed is refused (look again); the console's
    card lists them. A reset of the signatures (the frozen plan stopped holding) clears them
    with the plan: they are given again, on the new picture. Any operator may withdraw one
    while the engagement is pending, approved or under way (`Engine::revoke_unknown_friendly`,
    `engage.acceptance_revoked`: it only restricts; the held picture counts the vehicle again
    and stops an engagement it now blocks). The acceptor may also be one of the two signers
    (not barred: two people still sign); the signature records it (`self_accepted`, like
    `self_approved`; replay recomputes it), the card says so, and the evaluation counts it. Never automatic: the engine refuses it for a
    vehicle that is not lost or not since that report, and after a signature (the plan was
    signed without it); an agent's port has no such method (a compile-fail doctest) and the
    bridge refuses an agent's request carrying one (403 too for an operator without
    engagement authority). Every engagement decision logs the acceptances it read
    (`unknown_accepted`), which replay checks against the `engage.accepted` and
    `engage.acceptance_revoked` events.
- **Scenarios** (`scenario.rs`, `murmur-scenarios`, `sim/scenarios/engage/`): the
  adversarial set, one or more per invariant, with log tampering; see that directory's
  README for the format.

## Satellite tasking: collect and cover

Satellites are requested, not flown (DECISIONS.md, "Space as taskable infrastructure"). Two
intent kinds use `crates/murmur-space`; both target no vehicle (`Task::targets()` is `space`,
`uids` is empty, and a proposal with uids is refused), and the vehicle rules don't run for
them.

- **collect** `{area, window, capability: optical|sar|any, min_resolution_m?,
  product_deadline?, chosen?: {norad_id, collect_time}}`: imagery or SAR of an area (polygon
  or circle, `[lon, lat]`) in a window (times are scenario time, integer ms).
- **cover** `{area, window, service: {type: satcom, class} | {type: gnss, class}, min_level?}`:
  is SATCOM coverage (fraction of the window, default 0.99) or GNSS visibility (satellites in
  view at every sample, default 4) good enough over the area's centroid.

The flow for a collect:

1. **Query**: propose it without `chosen`. Rules `space_query` (the segment answered this
   intent's question; fail-closed without a segment) and `feasibility` run; `feasibility`
   fails with the first ranked options in its detail, and the intent's `space` field holds
   the whole answer (ranked `options[]` and `exclusions[]` with reasons). The intent ends
   `rejected`: it is a query, not an order.
2. **Choose**: a human (`Engine::amend_collect`, or `submit`) or an agent
   (`AgentPort::amend_collect`, or `propose`, with a rationale) proposes the same collect
   with `chosen` from one option. `feasibility` checks it against a fresh answer: the same
   satellite has a feasible pass within `collect_match_tolerance_s` of the chosen time, the
   pass is still in the future and in the window, the tasking lead time is met, and the
   product meets the deadline (unknown product timing with a deadline is `unverified`).
3. **Approve**: policy sends collects to a human (`satellite-collect` tier). Approval re-runs
   the query at the approval time and voids the intent if the option is gone.
4. **Dispatch**: `Engine::dispatch_order` gives `Order::Collect(CollectOrder)` (see the
   bridge contract below); `Engine::dispatch` refuses space intents, so an empty-uid task
   never reaches the vehicle task manager.

A cover runs on its own (`coverage-query` tier) unless a check is unverified, and dispatches
an `Order::Cover`.

**Replay.** Each space query is logged once as `space.query`: the query, the answer
(`compute_ms` zeroed, the one non-deterministic field), its `time_map`, and the segment
identity (`SegmentId`: SHA-256 over the catalog and every satellite's identity, epoch, mean
elements and SGP4 state at two instants, plus `cache_meta`). Decision inputs reference it by
content hash, like the area set. Rules read only the logged answer, so `murmur-replay`
re-decides every collect and cover without a cache. With `--space <constellations.toml>
<cache-dir>` it also re-runs every query: same segment, the answer must be identical
(else a mismatch, exit 1); a different segment is a **cache mismatch**, listed separately
(exit 3; the decisions still reproduced).

## The hash chain: tamper-evident, not tamper-proof

Every event's `prev` is the SHA-256 of the previous event's line as written (the first
event's is 64 zeros). `murmur-replay` recomputes the chain over the raw lines and checks
that each line is its event's canonical serialization, so it detects deleting, inserting,
reordering or editing any event that has a successor, including deleting a whole intent's
events and renumbering `seq` (which otherwise leaves every decision consistent).

What it doesn't protect against: anyone who can write the file can edit it and recompute
the whole chain, and cutting the tail leaves a valid, shorter chain. Both change the head
hash (`Engine::head_hash`, printed by `murmur-replay`), so the head has to be anchored
outside the log. Tests in `tests/replay.rs` show both sides: naive edits caught by the
chain, a recomputed chain caught only by the head.

**Run-time anchors** (`src/anchor.rs`). The bridge wraps its log's sink so that the hash of
the first line (`run.started`) and the head are written, as the log grows, to a separate
append-only file: after the first event, every `MURMUR_ANCHOR_EVERY` events (100), after
every event a restart carries intents over by (`intent.state`, `intent.resumed`,
`op.state`: "decision"), and at a clean shutdown (`logs/anchors/<run-id>.anchors`;
`MURMUR_ANCHOR_DIR` moves it,
`MURMUR_ANCHORS=0` turns it off). `MURMUR_ANCHOR_CMD`, a shell command, gets each anchor's
JSON on its stdin (and in `MURMUR_ANCHOR`), run in order on a thread of its own, to post it
elsewhere. `murmur-replay --anchors <file>` checks the log against them: the first line and
every anchored event must hash as anchored, the log must reach the last anchor (a cut tail
shows), the file must be whole (the start anchor on `run.started`, a cadence anchor exactly N
events after the anchor before it as each declares in `every`, a decision or shutdown anchor
at most N after it, a shutdown anchor last, so a removed anchor line shows), and
nothing may follow the shutdown anchor: the bridge closes the log behind it (later appends
fail, so nothing is done that isn't logged) and exits with the decision lock held. A run
that crashed has no shutdown anchor and fails the check: its tail can't be told from a cut
one, but everything up to its last decision is anchored (a carry-over checks that much).

**An anchor is only as strong as the separation between where it is kept and whoever can
write the log.** The local anchor file sits in the same account as the log: it catches an
accident, a crash that loses the tail, or someone who edits the log alone, but whoever can
write both files can rewrite both. Real protection needs a separate trust domain:
`MURMUR_ANCHOR_CMD` posting each anchor to another machine, a service or a timestamping
authority the log's writer can't change. Nothing here signs anchors or runs such a
service; that remains future work. `tests/anchors.rs` shows a re-chained rewrite, a cut tail
and an append after shutdown caught by the anchors that replay alone passes.

## The guarantee: agents nominate, only humans authorize

An agent can never authorize. A human's submission is a human decision by construction, so
a human tier can let it stand as the approval; an agent's proposal can't be one.

- Two paths lead to `approved`, and both need a `&HumanAuthority`: `Engine::approve` (the
  inbox), and `Engine::submit` / `Engine::amend_collect` for the submitter's own intent in a
  tier with `submitter_authorizes` (the engine checks that the authority is the intent's
  own human source). `AgentPort::propose` and `amend_collect` pass no authority, so their
  intents in a human tier always go to `pending_approval`.
- A `HumanAuthority` lives only inside an `OperatorSession`, which only
  `Engine::open_operator_session` creates (and logs). Neither type has public fields,
  `Clone`, `Default`, `Deserialize`, or a conversion from an agent id, so no JSON (an MCP tool
  call, say) can become one.
- Agent code gets an `AgentPort`: propose (a non-empty `Rationale` is a required argument;
  logged first as `agent.action intent.propose`), preview (the checks and policy as a dry
  run, no intent created, logged as `agent.action intent.preview`), withdraw its own
  intents, record an action, read. No approve method; its engine reference is private.
- An intent's source is set by the path it came in on, never by the request body.
- `compile_fail` doctests (in `authority.rs` and `engine.rs`) show that forging, cloning or
  deserializing an authority, calling approve through an `AgentPort`, reaching its engine, and
  passing an agent id as an authority all fail to compile. Stable rustdoc only checks that
  they fail, not the error code; each was checked by hand to fail with exactly the code noted.
- Replay checks the same property from the log: no `approved` without either an
  `approval.granted` by an operator before the deadline and a passing re-check, or an
  `approval.at_submission` by the intent's own human source, on passing checks, in a human
  tier whose recomputed decision lets the submitter authorize. An agent-sourced intent with
  an `approval.at_submission` is a mismatch.

**Limits.** Rust privacy is a code-structure guarantee, not a security boundary. It keeps
code that holds only an `AgentPort` (or only JSON) from authorizing, and it leaves one
greppable minting point. It does not stop someone editing this crate, bridge code that holds
`&mut Engine` opening an operator session on an agent's behalf (rule: only the authenticated
operator endpoint calls `open_operator_session`), a compromised process, or a human who
approves without looking. Authenticating operators is the bridge's job. Whether human
approval stays meaningful at scale is what Murmur measures; this crate makes sure there is a
human decision to measure.

## Replay

```
cargo run -p murmur-decide --bin murmur-replay -- logs/audit/<run-id>.jsonl \
    [--rules N | --min-rules N] [--anchors logs/anchors/<run-id>.anchors] \
    [--space sim/space/constellations.toml sim/space/cache] \
    [--prev logs/audit/<previous-run>.jsonl | --trust-resumed]
```

A log is recomputed with the rules it records, but by default only the current rules are
accepted: a log of older rules replays only with `--rules N` (exactly N) or `--min-rules N`
(N or later), so a log can't pass by claiming older, looser rules (see below).

Re-runs every `check.result` and `policy.decision` from its logged inputs and the logged
policy (resolving `areas_sha256` to the logged set in force, whose own hash must match its
content), checks every `intent.state` is legal and backed by its evidence, checks `seq`,
`t`, the hash chain and every `self_approved` flag, checks every at-submission approval
(the approver is the submitting human, the tier allowed it, never an agent's intent, and no
intent whose decision let the submitter authorize was sent to the inbox instead), checks
that every order dispatched, effect released and op state reported is the approved intent's
(below), and prints the approval, self-approval and at-submission counts and the head hash. Logs written
before `submitter_authorizes` existed still replay: the key is additive, and unset in a
logged policy means false (how those runs behaved). Exit 0 if everything reproduces, 1 on mismatch, 2
if the log can't be read, 3 if the decisions reproduce but some space queries ran on a
different space segment (cache mismatch).
Determinism: injected clock, integer-millisecond times, `BTreeMap`s, fixed output order and
fixed-precision detail strings, `serde_json` with `float_roundtrip`, terrain samples keyed at
1e-7 degree. Replay proves decisions follow from the logged inputs; it can't prove the inputs
were true of the world.

**What was executed is what was approved** (sixth review). Recomputing every decision is not
enough: a forger who re-chains the log can leave every decision intact and change what was
done with it (the review's forged logs released an engagement's effect from another vehicle,
dispatched an engagement or an approved armed orbit to another vehicle, and moved that
orbit's centre 2 km, and replay passed them). So replay also binds each executed fact to the
approval, for every rules version (these are audit checks, not decision rules; every
committed fixture, every log in `logs/audit` and every Phase G run log satisfies them), the
roster and the loiter radius from rules 4 (older logs don't carry them):

- `intent.dispatched` is exactly the approved intent's order: the same intent, its task and
  parameters (orbit centre and radius, patrol route, track target, the collect or cover as
  approved) and its vehicles in order, and the space or engagement order recomputed the way
  the engine makes it (`order_for` in `src/engine.rs`, which `Engine::dispatch_order`
  uses): a collect's order is the chosen option as the answer its last check read has it
  (satellite, class, times, resolution, scene, that answer's hash), a cover's the approved
  service, an engagement's the frozen plan (point, time, radius, window) on the nominated
  platform with its track and fallback, and from rules 4 the radius the platform loiters at
  over the point (`engage::loiter_radius`, from its catalog facts on the final signature's
  world). A dispatch transition needs its `intent.dispatched` right before it, and an
  `intent.dispatched` is followed by its dispatch, nothing else.
- `engage.effect` comes from the engagement's platform (the one it was dispatched to), at the
  frozen plan's point and radius (as before), and the picture that released it lies inside
  the plan's time tolerance (`engage::release_window`: from the plan's time, or
  `plan_slack_s` before it for a moving target from rules 2, never before the window opens;
  until `late_s` after it, never after the window closes), the effect never logged before
  it.
- `op.state` is a dispatched intent's, on its own vehicles (a collect's `norad:<id>`, a
  cover's `satcom:<class>` or `gnss:<class>`), for a vehicle or space task only while it
  runs (its finish is final). On a vehicle a newer dispatch took, an older intent reports
  only how its op ended (`done`, `failed`, `superseded`: the task layer replaces an
  unfinished op at the newer dispatch), and `superseded` needs a newer order on the vehicle
  (an engagement's `intercept superseded` excepted, which its own fallback replaces). An
  engagement's `fallback` report follows a stop that released a fallback and names it and
  its task first (`"return commanded (rtl): ..."`, `Fallback::task`).
- `engage.stopped`: a failed picture is stopped for the picture, never under another cause;
  `superseded` names another intent whose vehicles include the platform, the one dispatched
  last and still under way (the bridge supersedes in the same step as that dispatch), with
  that intent's own source; a `system` stop that quotes a vehicle event (`the platform's
  failsafe took over (...)`, `the platform left its intercept (mode ...)`) or the intercept's
  report (`the platform's intercept <state>: <detail>`) follows that `vehicle.event` on the
  engagement's own platform, or that `op.state` of its intercept, since its last picture.
  An engagement never ends with a task's `fail`.
- Sessions (rules 4): every `session.opened` is for an operator on the roster `run.started`
  names, with engagement authority only for one on its engagement list; a rules 4 log always
  names its roster. Rejections and at-submission approvals are under the deciding operator's
  own session, as signatures already were.
- Lost-link preload (rules 6): the order's `preload` is the one the approved fallback and the
  platform's autopilot give (`engage::order_preload` on the final signature's world); every
  `engage.preload` is the dispatched engagement's, on its platform: one write-ahead
  (`intent`, while under way) before one `set` whose `before` is exactly the write-ahead's,
  re-reads (`verify`) only after a confirmed set while under way, and restores only once the
  engagement ended and while no other engagement is under way on the platform, its
  `confirmed` recomputed from the read-back (and, for a restore, against what the `set` read
  first); an `engage.effect` needs a confirmed `set` and its
  latest read-back confirmed (and the release picture recomputes the `fallback_preload`
  check with the read-back's age); a `set` or `verify` that did not confirm while under
  way is followed by its stop, for it, and a stop that says so follows such a report. The
  platforms' own values are a ledger: owed from a write-ahead (or a carried
  `platform.restore`) until a confirmed restore shows them all back; a write-ahead whose
  values differ from those still owed is flagged (a preloaded value read as the platform's
  own); a non-carried `platform.restore` must put back exactly the values owed, never while
  an engagement is under way on the platform, its `confirmed` recomputed; and values still
  owed at the end are reported (`unrestored`), not a mismatch.
- Accepted lost friendlies (rules 6): every `engage.accepted` is under the accepting
  operator's own session with engagement authority, at its submitter's nomination or while
  it waits with no signature, its logged world within 5 s of it and showing the vehicle
  lost since exactly `since` (recomputed); every picture token and signature carries
  exactly the acceptances in force; a reset clears them; an `engage.acceptance_revoked`
  withdraws one in force, under the operator's own session; every decision of that
  engagement logs exactly the acceptances in force; every signature's `self_accepted` says
  whether its signer accepted one of them.
- Track altitudes (rules 6): recomputed from the logged facts, so a decision that counted a
  track's altitude rests on its logged time (`alt_t`) and its sources' last hits
  (`source_t`) within `track_alt_max_age_s`.

The engine refuses the op states, supersedes and sessions these checks reject
(`EngineError::NotItsOrder`, `EngineError::NotOnRoster`, nothing logged), so an honest engine
can't write such a log. What stays unbound: a vehicle event that no decision or stop quotes
(its vehicle id is the bridge's report; replay doesn't know the fleet), whether a vehicle
task's outcome (done, failed) matches its op states (the task layer's report, not a
decision), the reason of a `system` stop that quotes nothing (the bridge's prerogative: a
stop can only make the fleet safer), and what the bridge sends beyond the order when it
commands a fallback: the task is bound through its report, but a loiter fallback's centre
(where the platform is when it is commanded) and radius are not logged. A roster rewritten
together with the sessions passes replay; the roster is in the first line, which the start
anchor covers. `tests/bindings.rs` and the engagement tests hold the reviews' forged logs as
regressions (v1, v2, vE and vE2 byte for byte the reviewers' files, from
`tests/data/audit/v3-armed-orbit-phase-g.jsonl`; N2, N3 and the second review's probes as
the same edits on committed or engine-written logs) and a test per binding.

## Decision rules versions

A rules change that alters a decision (a new check, another threshold or model, another
detail text) must not invalidate the logs released code wrote. `RULES_VERSION`
(`src/rules.rs`) is bumped with such a change, the change is gated on it
(`RulesConfig::rules_version`, copied into `EngageRules`), and `run.started` records the
version a run decides under (`rules_version`; a log without it is version 1), with the
policy schema and, from rules 3, the build (`build`: commit and whether the tree was dirty).
Replay sets the logged version on the logged policy and recomputes every decision with
those rules, so each log is checked against the rules it was produced under; a log of newer
rules than replay knows is a mismatch.

A claim of older rules is a downgrade risk: a log recomputed under rules 1 is judged by
rules 1, however it would fare under today's. So replay accepts only the current rules unless
told otherwise (`ReplayOptions::rules`, `murmur-replay --rules N` / `--min-rules N`), and
checks that the log fits its claim: `run.started` is the first event and the only one (a run
can't start again, with other rules, inside a log), and nothing appears that only later
builds write (`RULES_MARKERS` in `src/replay.rs`: a `checked_t`, a GPS fact, a
`platform_free` check, a build, a circle, a fix from HIGH_LATENCY2, a roster, an order's
loiter radius; `rules_version` and
`policy_schema` themselves come with rules 2). A log of rules 1 carries no build and was
never anchored: accepting one says the verifier trusts where it came from. A log that
carries nothing of a later version and claims an older one replays under the older rules
when they are accepted: that is what the explicit flag is for. `POLICY_SCHEMA` versions the policy file the same way, and its keys
change so that an older log both parses and is still its lines' serialization (replay
requires each line to be its event's serialization, so a serde alias, which parses an old
name but writes the new one back, isn't enough):

- a new key is an `Option`, skipped when absent, read through an accessor with its
  default (`plan_drift_max()`);
- a renamed key keeps its old name as an `Option` field of its own beside the new one,
  read through the new key's accessor, and setting both is an error (`seen_max_age_s`
  beside `token_ttl_s`, read by `token_ttl()`);
- a removed key stays as an `Option` field nothing reads, documented as such.

Replay also checks the schema a log claims against the keys its policy uses
(`EngageRules::schema_keys`: a schema-1 claim with a schema-2 key is a mismatch), and a
schema newer than it knows.

To change a rule: bump `RULES_VERSION`, gate the change on it, add a marker for any field
or check only the new builds write, add a line below, and commit a log of the new version
to `tests/data/audit/` (a live one with its anchors in `tests/data/anchors/`) with an entry
in `tests/versions.rs`, which replays a committed log of every version (each with its
version accepted explicitly), checks the live log against its anchors, and checks the
downgrade cases.

Rules versions:

- **1**: everything up to master `977854f` (logs without `rules_version`), Phase E's first
  frozen-plan chain included.
- **2**: the third Phase E review. A target slower than `holding_speed_mps` is predicted
  where it is; the frozen plan is held against where its track path puts the track now and,
  at its time, against its point, with allowances for position noise and velocity
  uncertainty (`plan_drift_max_m`), the deviation check after approval too; reach slack
  allows a fixed wing's waiting circle; `platform_free` (one engagement per platform);
  `platform_nav` also needs a 3D GPS fix and no GPS glitch; our vehicles count in the
  keep-out with `friendly_growth_mps` of prediction uncertainty; after the plan's time the
  keep-outs are read at the release, and a moving target may be released within the plan's
  time tolerance; signatures log `checked_t`, which replay requires and bounds. Released
  as master `6da8909`.
- **3**: the fourth Phase E review. One of our vehicles known to fly a circle (`circling`:
  an orbit task on station, a fixed wing loitering or circling its RTL point, with its
  source and when its center was reported) is predicted anywhere on its disc, less the
  same growing uncertainty, instead of along its momentary heading (a circling fixed wing
  no longer blocks engagements kilometres away). The circle holds only a vehicle that is
  on it, each report judged on its own freshness: the position fresh by the usual
  link-aware rule and within the radius and 50 m of the center (`CIRCLE_SLACK_M`), the
  center reported at most 12 s ago (`CIRCLE_CENTER_MAX_AGE_MS`); otherwise the vehicle is
  predicted from its position. The disc includes the slack, so it is never farther than
  where the vehicle was reported, and at every span the vehicle is also carried along its
  velocity for at most 5 s (`CIRCLE_CARRY_S`), the nearer of the two counting.
  `platform_nav` says when its 3D fix is a HIGH_LATENCY2 report without its GPS failure
  flag (`gps_fix_from`); `run.started` names the build.
- **4**: the second review of the replay bindings. No check changes; two facts are logged
  so replay can bind them: `run.started` names the operator roster (`roster`: the bridge's
  `MURMUR_OPERATORS`, and `MURMUR_ENGAGE_OPERATORS`, who must be operators too), against
  which replay checks every session, and an engagement order carries the radius the
  platform loiters at over the effect point (`loiter_radius_m`, which the bridge flies and
  replay recomputes). A rules 4 log without a roster is a mismatch.
- **5**: the safe command queue (murmur-5p4, see "The queue for unreachable vehicles"): an
  approved order for a vehicle the link can't carry it to now is queued
  (`intent.queued`) instead of dispatched, re-checked when it can be reached (`check.result`
  phase `dequeue`, `queue.recheck`: dispatch, back to the inbox, void), expired at its
  deadline (`queue.expired`, or back to the inbox for a human's approval held
  `human_expire_s`); a dispatched order whose link was lost before any vehicle started it
  goes back to the queue (`intent.requeued`), and one whose start had gone out (op
  `starting`, then `unconfirmed`) is only confirmed next time, or goes to a human when the
  vehicle does not run it; an older queued order is voided when a newer
  one for its vehicle stands, and an agent's order on a vehicle with a human-approved order
  waiting needs a human (`waiting`); a direct dispatch logs the reach that allowed it; open
  intents are carried over a bridge restart (`intent.resumed`); reboots are logged
  (`vehicle.rebooted`) and the boot instant is a vehicle fact (`booted_t`). Gated in
  `Engine::dispatch_or_queue`, `Engine::dispatch` (rules before 5 dispatch as before),
  `Engine::requeue` and `Engine::resume`, and the `waiting` rule in `policy::decide`;
  markers: those event types, a `dequeue` phase, `booted_t`, `reach`, `waiting` and the
  policy's `[queue]` (policy schema 3).

- **6**, the engagement part (murmur-der.2, murmur-der.3 and their two reviews; gated by
  `EngageRules::v6`, every change on engagement paths only): the approval's fallback
  preloaded as the platform's own lost-link action (the `fallback_preload` check at
  nomination, on the plan and at the release; the order's `preload`; `engage.preload` reports, set, verify near the release
  (the release needs a read-back at most 10 s old) and restore, an unconfirmed set or verify
  stopping the engagement; the platforms' own values owed back as a ledger,
  `platform.restore` for values carried over from an earlier run or restored outside an
  engagement); the keep-outs' prediction model for tracks (constant velocity with
  acceleration noise, not 0.3 x speed; a track without a velocity estimate anywhere it
  reaches); keep-outs in 3D where both altitudes are known and a track's is ranged (never a
  ground track's), our vehicles climbing or diving at the larger of both; a lost friendly
  accepted by an operator with engagement authority (`engage.accepted` with the vehicle's
  facts, carried in every picture token, cleared by a reset, withdrawn by
  `engage.acceptance_revoked`; `unknown_accepted`; a signer who accepted one is recorded,
  `self_accepted`) left out of the friendly keep-out; a preload's write-ahead
  (`engage.preload` intent, with the vehicle's identity) and no restore while another
  engagement holds the platform; a track's altitude only from a recent ranged measurement
  (`track_alt_max_age_s`, policy schema 4). New world facts: our vehicles' `alt_msl_m`, the
  catalog's `climb_mps` and `max_mps`, a track's `alt_t` and `source_t`. Markers (a log
  claiming rules 5 or older can't carry them): a `fallback_preload` check, an order's
  `preload`, `engage.preload`, `engage.accepted`, `engage.acceptance_revoked` and
  `platform.restore` events, `unknown_accepted`, `alt_msl_m`, `climb_mps`, `max_mps`,
  `alt_t`, `source_t`. Fixtures:
  `tests/data/audit/v6-engagement-scenario-01.jsonl`,
  `v6-lost-friendly-accepted-scenario-54.jsonl`, and a live run with its anchors
  (`v6-engagement-live.jsonl`, `tests/data/anchors/v6-engagement-live.anchors`: two
  nominations rejected for a fast ferry by the point, a third clear of a far fast ferry and
  by altitude of our aircraft at 9,000 m (diving at 44 m/s it can't get down by the effect
  time), its USV's own lost-link values logged ahead of the write (with its identity), the
  preload set, read again seven times before the release (the last 3 s before it), the
  effect released, the USV's own values restored; an acceptance by an operator without
  engagement authority refused with a 403, nothing logged).

Policy schemas: **1** up to `977854f`; **2** adds `plan_drift_max_m`, `holding_speed_mps`,
`friendly_growth_mps` (optional) and makes `token_ttl_s` optional (default 15) so that
`seen_max_age_s`, an unreleased build's name for it, is still read; **3** adds `[queue]`
(optional; absent: its defaults); **4** adds `track_velocity_sigma_mps`, `track_accel_mps2`
(a table by domain), `vertical_rate_mps`, `keep_out_vertical_m` and `track_alt_max_age_s`
(optional; rules 6).

## Integration contract for the bridge

The full contract is in the crate docs (`src/lib.rs`). In short:

1. At start: `Policy::load("sim/policy.toml")`, `Catalog::load("sim/platforms.toml")`,
   `AreaSet::load_dir("sim/areas")`, `Roster::new(MURMUR_OPERATORS, MURMUR_ENGAGE_OPERATORS)`
   (refused, and the bridge does not start, when an engagement operator is not an operator),
   `Engine::with_areas(policy, Some(areas), roster, SystemClock, JsonlFile at
   logs/audit/<run-id>.jsonl)` (`Engine::new` works too; the set is then logged at first
   use). Sessions open only for operators on the roster, with engagement authority only for
   those on its engagement list.
2. Per decision call, build a `WorldSnapshot` keyed by `uid`: platform facts from the catalog,
   and timestamped facts from telemetry (autopilot kind, nav ready, position, home, battery).
   Unknown is `None`.
3. Terrain: a `TerrainOracle` over tidewater's `POST /v1/sample` (lat-first points; null
   elevation is outside coverage; `water_depth_m > 0` is water), or `NoTerrain`.
4. Operator endpoints: `submit` (also behind the legacy `POST /api/tasks`), `approve`,
   `reject`, `cancel`, `inbox`. Agent (MCP) side: an `AgentPort` per agent, nothing else.
5. Every second: `expire_due` (pending approvals and, rules 5, queued intents past their
   deadline). For approved intents: `dispatch_or_queue` with the world now gives
   `Routed::Dispatched(Order)` (a vehicle order: a `DispatchOrder` for the existing task
   manager, execute exactly it) or `Routed::Queued`; offer each queued intent to `dequeue`
   with a fresh snapshot every second (`Dequeued::Dispatched` is executed the same way); when
   every op of a dispatched order ended `requeued`, `requeue` it. `dispatch(id, world)` gives
   a `DispatchOrder` directly (refused for an unreachable vehicle under rules 5); report back with `record_op_state` (a
   dispatched intent's own vehicles only, and before its finish) and `finish`; log vehicle
   events; `void` intents the world invalidates. An engagement's fallback is reported as
   op state `fallback` with a detail that starts with the fallback's name (see "What was
   executed is what was approved" above and `src/lib.rs`). Rules 6: an engagement order's
   `preload` is set on the platform before its intercept (its own values read first, the
   preload read back) and reported with `record_preload` (set); send the intercept only
   once it confirms, command the fallback it returns when it doesn't; read it again every
   5 s once `engage::preload_verify_due` says so (`record_preload` verify; command the
   fallback it returns); run one parameter job at a time per platform; put the platform's
   own values back once no engagement holds it (`record_preload` restore by the
   engagement that preloaded last, `record_platform_restore` otherwise or when the engine
   refuses it), never reading a preloaded value as its own; at start, carry over what the
   previous log still owed (`replay::owed_restores`, `record_platform_restore` carried). A
   nomination that accepts lost vehicles goes through `submit_accepting`; one already
   waiting through `accept_unknown_friendly`: both an operator's request with engagement
   authority only; `revoke_unknown_friendly` any operator's.
6. Any `Err` from the engine means nothing changed; never dispatch on an error.
7. The inbox shows `checks` (look for `unverified`), `policy.facts.unverified` and
   `policy.facts.area_detail` as the reason a human is needed, and `approval.self_approved`.
   It holds agent proposals in human tiers, and human intents only from tiers with
   `submitter_authorizes = false`; a human's own intent elsewhere comes back `approved` with
   `approval.at_submission` (dispatch it like any approved intent).
   For a collect, show the intent's `space.result.options` (and `exclusions`) and the
   `space_query` detail (it says `TIME-SHIFTED` when the answer is time-shifted).
8. Space segment: load `murmur_space::Space` (`sim/space/constellations.toml`,
   `sim/space/cache`, after `murmur-space fetch`), wrap it in `SpaceSegment::new(Arc::new(..))`
   (`with_scenario_offset_ms` if the scenario clock is offset from the engine clock,
   `with_scenario_map` if it also runs at another rate), and `Engine::set_space` it. The
   `space.loaded` event logs the scenario map and the segment's pinned time shift;
   `murmur-replay --space` pins the same shift before re-running queries. Dispatch with `dispatch_order`:
   - `Order::Vehicles(DispatchOrder)`: as before.
   - `Order::Collect(CollectOrder)`: a simulated tasking request (no real satellite operator
     API). Schedule an abstract "collection" event at `collect_time` and a "product ready"
     event at `product_time` (scenario time; `None` if the class's timing is unknown). Report
     `record_op_state(id, &order.resource, state, detail)` with `resource` = `norad:<id>` and
     `state` in `tasked`, `collected`, `product_ready` (or `missed`), then `finish(Done)` at
     product ready or `finish(Failed)` if missed. Show `scene` (a `[lon, lat]` ring) on the
     map.
   - `Order::Cover(CoverOrder)`: report outages from the answer as op states under
     `satcom:<class>` or `gnss:<class>`, and `finish` at the window end.

The bridge does all of this (`crates/murmur-bridge/src/decision/space.rs`): collects and
covers come in through `POST /api/intents`, and a simulated executor reports their op states
on the scenario clock.

## Not done yet

- No HTTP endpoints, no tidewater client, no inbox UI (C.5): bridge and web work.
- Routes are straight lines between the points the task commits to. An RTL commanded
  mid-loop flies home from wherever the vehicle is, which sweeps a region the return-leg
  check (from waypoint 1) doesn't cover. A leg starting inside a no-go area is allowed to
  leave it; if a concave area makes that straight leg re-enter it, that isn't caught. The
  domain rule checks waypoints only, not the legs between them. Areas are 2D (no altitude
  bands).
- Reserves assume cruise speed and a fixed time-on-task horizon (`task_duration_s`), because
  orbits and patrols loop until told otherwise; the bridge doesn't end tasks after a duration
  yet, so `duration_s` can only lengthen the estimate.
- The head hash isn't anchored anywhere, so a full rewrite of the log with a recomputed
  chain, or a cut tail, is not detected by replay alone (see the hash chain section).
- Cross-platform replay: trigonometry goes through the platform libm. Same machine is exact;
  across machines a last-bit difference could in principle flip a boundary case.
- Engagements: a straight-line intercept (no turn radius or climb), keep-outs 2D wherever
  an altitude is unknown (and a vertical cylinder, not a volume shaped by any effect model,
  where both are known), the platform's own path checked against no-go areas but not
  no-strike areas, engagement authority on the same placeholder authentication as the
  operator header. An engagement does not survive a bridge restart: the next run puts the
  platform's own lost-link values back (carried over from the last run's log, onto the same
  vehicle only, see the lost-link preload above), but the engagement itself is not resumed.
- Satellite tasking: no cloud model (optical options note it), no sensitive-area or
  cost/priority tiers yet (every collect needs a human), no competing demand or agility
  model, one collect per intent. Cover reads the area's centroid only. The segment identity
  is computed here from public `Satellite` fields and propagated states; a content hash of
  the raw cached files in murmur-space would be stronger.
