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
dispatched -> aborted                         (engage: stopped before the effect; rules 11: a
                                               collect or cover cancelled after dispatch)
auto_approved | approved -> queued            (rules 5: a vehicle can't be reached now)
queued -> dispatched | pending_approval | voided | expired   (its re-check, or its deadline)
dispatched -> queued                          (rules 5: the link lost before any vehicle started it)
```

The full table is `intent::next_state`; anything else is a typed `IllegalTransition`.

1. **Rules** (`rules.rs`) are pure functions of the intent and a `WorldSnapshot`:
   capability (the KINDS table), readiness (nav ready, fresh), platform limits (orbit radius
   vs minimum turn radius, with a suggested radius instead of silent widening), domain (boats
   and subs on water, UGVs on land, via a `TerrainOracle`: the waypoints, and from rules 17
   every leg of the route, below), geofence (no-go polygons from
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
   is leaving it and is allowed, so a vehicle that strayed in can be brought out. From rules
   17 the domain rule judges the same legs for land and water (`route::Route::legs`): a point
   every 20 m along each (`route::along`, measured from the leg's far end), besides each
   waypoint answered exactly; an engagement's legs too (position to effect point, effect point
   to home), and an rtl's way home. Details name the leg and the point: `transit leg
   (position to waypoint 1) crosses land at (-122.46012, 37.83968), 1067 m along it; magura
   needs water`.
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
- **Rules 7: a start is logged ahead of sending it** (murmur-dma). The bridge logged an op's
  `starting` at its next tick, so a bridge that crashed within a second of a start left no
  record that it may have gone out, and the next run delivered the order as never started
  (a review probe restarted a mission over a failsafe RTL). Now the op logs it itself before
  the mission mode, the hold or rtl mode, or new items to a vehicle already flying a mission
  go out (`Engine::record_op_state_ahead`: `op.state` starting with `ahead: true`), under the
  decision lock, and sends nothing if the write fails; the tick no longer logs a `starting`.
  A carry-over then always finds it (`maybe_started`): only confirmed, never started again on
  its own. A follow of an order the vehicle already runs (after a restart, or a redelivery
  that finds it running it, or in the hold or rtl mode) is logged ahead the same way, saying
  it was followed: from then on the vehicle runs it. The engine refuses, and replay flags, a
  vehicle task's `starting` not logged ahead, an `ahead` on anything else, and, on every
  delivery, a vehicle taking the order (`acked`, `active`, `done`) before its start or follow
  on that delivery was logged ahead (`Intent::started_ahead`, cleared at each dispatch), so a
  forged log that drops a write-ahead is caught, a redelivery's too. The write waits for the
  decision lock off the runtime's worker (`block_in_place` on the bridge's multi-threaded
  runtime), so a lock held for seconds stalls no other vehicle's receive loop.
- **Rules 7: a mission waits for its platform's own lost-link values** (murmur-85l). After an
  engagement ended, its platform still carried the engagement's lost-link action (its
  fallback on a timer of a few seconds) until the restore confirmed, which backed off while
  the link was down, while the queue sent waiting orders on the first tick the link was back:
  a second short gap then braked, landed or returned the vehicle instead of its own failsafe.
  The engine keeps the platforms' own values owed back as the log has them (the ledger replay
  keeps, `replay::Restores`), and `queue::reach` holds an order other than a hold or rtl for
  a vehicle still owed values an engagement of this run preloaded (`restore_owed`, a
  decision's fact logged with the inputs and recomputed by replay from its ledger; under rules
  7 to 10 values only carried over from an earlier run's log hold nothing, since the bridge
  puts those back only onto the vehicle they were preloaded on, which the log can't tell;
  from rules 12 they hold too, unless the bridge's identity of the vehicle under the uid,
  logged with the fact, shows another one: see the changelog). Its reach says why
  (`heard, but its own lost-link failsafe (...) is not confirmed back since engagement ...`),
  it goes the tick after the restore confirms, and its wait is bounded as any queued order's:
  a human's approval back to the inbox after `human_expire_s` (the reason now quotes why it
  was queued, the reach `intent.queued` logged), else expired at its kind's wait. A hold or
  rtl, the safety commands, is never held: the restore runs beside it. A mission for a
  platform whose engagement is still under way waits the same way (queued, it supersedes the
  engagement, below; the restore runs; then it goes). The hold is not the queue's to waive:
  for a kind the policy never queues (`[queue.kind.<kind>] expire_s = 0`) the mission has
  nowhere to wait, so it is voided, saying why (`Routed::Voided`, reason `system: not sent:
  ...`), and given again once the restore confirmed; every dispatch path refuses one
  (`dispatch_inner`), and replay flags any mission dispatched to a vehicle its ledger shows
  still owed, whatever the kind's queue. Refused, it still stands over an engagement on its
  vehicles, as a dispatched or queued order does: the refusal opens a step in which the
  bridge stops the engagement, `superseded` by it, no fallback (the reason says so: `this
  order stops engagement ... and the restore of its own values starts now`), and the restore
  runs; refused once the engagement had ended, the reason says the restore is pending. Either
  way it says a hold or rtl goes at once.
  The bridge no longer tries a restore into a dead link and tries it at once when the vehicle
  is heard again, whatever the backoff of attempts the outage cost; the inbox's restore alert
  lists the missions waiting for it.
- **Rules 7: a queued order supersedes an engagement** (murmur-636). An order queued for an
  engagement's platform stops the engagement in the step that queues it, as its dispatch
  would: cause `superseded` by that order, no fallback commanded (`Engine::supersede_engagement`
  takes the intent queued last as it takes the one dispatched last, and from rules 7 only in
  the step that dispatched or queued it: right after its transition, with nothing logged in
  between but the other engagements that step stops; replay checks the same, so a later
  picture or system stop can't be relabelled `superseded`). The bridge stops the engagements
  first in that step, before it voids older orders. The platform's intercept is not stopped
  (nothing can be sent before the queued order goes): it may keep loitering at the effect
  point until then, with no engagement and nothing to release, and the restore alert says so.
  Before, the queue abandoned the engagement's unstarted intercept, the engagement read that
  as its intercept failing, stopped as `system` and commanded its fallback, which flew when the
  link returned while the operator's newer order still waited.

**Replay.** `intent.queued` is recomputed (reach from its inputs, at least one vehicle
unreachable, the deadline from the policy, approved before, a kind the policy queues);
`queue.recheck` must follow its dequeue check directly, and its reach (all reachable),
decision and verdict are recomputed from that check against the basis replay rebuilt from
the approval; the next transition must carry out the verdict (dispatch, back to the inbox
with a new `approval.requested`, void), and a dispatch from the queue needs a re-check that
says so, logged right after it and before the deadline; a queue expiry needs its deadline
and its kind (expired or back to the inbox). Under rules 5 a dispatch straight from an
approval of a kind the queue holds needs its reach (recomputed: every vehicle reachable). An
`intent.requeued` needs every vehicle's last op state `requeued` or `unconfirmed`, none ever
started, and its deadline from the policy; an op that reported its start going out
(`starting`, `unconfirmed`, or the vehicle started it) never ends `requeued` on that
delivery: the bridge ends it `unconfirmed`, it may be running (murmur-rcf). A later delivery
that only confirms it may lose its link before it asks the vehicle what it holds, or while it
asks (a mission download cut short), and end `requeued`; after a restart, a vehicle that had
started the order is followed and never ends `requeued`, one whose start may have gone out
is only confirmed and may.
Each decision's `waiting` fact is recomputed from the queued intents. `intent.resumed` is
checked against the previous log (`ReplayOptions::previous`, `murmur-replay --prev`, by
default the file it names beside the log): that log is the one named (head, last seq),
replays, and leaves the intent open exactly as carried; without it a resumed intent is a
mismatch unless trusted (`--trust-resumed`). One generation: replay each log with its own
predecessor (the lost-link values a run carries can come from another log: see the
lost-link preload below).

## Engagements (Phase E)

`engage.rs`: the engagement decision chain, a C2 decision study with an abstract effect (a
point, a radius, a time); nothing models a weapon, guidance or an aim point. `Task::Engage
{track, effect_radius_m, window, fallback}` names a perception track and one armed platform
(`uids`); a missing window is written into the spec at creation.

- **Checks** (`engage::evaluate`, after the vehicle rules, each pass / fail / unverified /
  not evaluated; engage is consequential so the last two block): `target_declared` (an
  operator's declaration, or from rules 9 the hostile-act rule's, confirmed unless
  `nominate_unconfirmed`),
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
  `engage.assessed` (persists, ceased, unknown). A vehicle of ours covers the effect point
  when a sensor that sees the target's domain reaches it (`engage::covering_sensor`): its
  `range_m`, or in a run on the physical sensing model the catalog's `physical_reach_m` for
  the domain, derived from that model (bd murmur-1u4, docs/SENSING.md "The decision engine's
  reaches"). The facts carry whichever the run is on (`Catalog::reaching`) and are logged with
  each observation, so replay needs neither the catalog nor the model; a legacy run's facts
  carry `range_m` alone, as before. `murmur-scenarios --reaches physical` runs the scenarios
  on the physical reaches.
- **Stop**: `Engine::abort_engagement(id, &HumanAuthority, note)` (any operator),
  `Engine::stop_engagement_system(id, reason)` (the bridge: task failed) and
  `Engine::supersede_engagement(id, by)` (another order dispatched to its platform: cause
  `superseded`, `fallback: null`; refused unless `by` was dispatched to vehicles that
  include the platform, or from rules 7 queued for them, or refused for their own lost-link
  values, in that order's step). A picture more than `max_picture_gap_s` after the last
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
  stays unconfirmed for 30 s. From rules 7 no attempt goes into a dead link, the first comes
  the moment the vehicle is heard again, and a mission for it waits in the queue until one
  confirms (see the queue's rules 7 entries). Values still owed when the bridge stops are carried over:
  at its next start it derives them from its previous log (`replay::owed_restores`; the
  file `logs/audit/.last-run-<http port>` names that log; a reused run id carries from the
  part of the same file before this run's `run.started`), logs them as `platform.restore`
  carried with the vehicle's identity, and puts them back only onto a vehicle that matches
  it (platform, callsign, system id, home within 500 m: `VehicleIdentity::matches`; else
  nothing is written and the inbox says why). A pointer to a log that can't be read is an
  inbox alert too, not a silent first start. Replay reports every platform whose values were
  still owed when the log ended (`ReplayReport::unrestored`, printed `UNRESTORED:` by
  `murmur-replay`); it is not a decision mismatch. Carried values are checked against the
  log they name (murmur-aca; `ReplayOptions::restores_from`, `murmur-replay --prev` or the
  file of that name beside the log): that log must replay (its own carry-overs trusted, one
  generation), and the values must be exactly what it still owed (`owed_restores`: each
  platform's values, the vehicle they were preloaded on, the engagements named), none twice,
  every platform it owed carried, all from one log. That log is the one `.last-run-<port>`
  named, which can be another than the one the carried intents name (`.resume-<port>.json`
  is deleted by a clean stop, `MURMUR_RESUME=<log>` names any): `murmur-replay` takes one
  `--prev` for each and matches them by file name, and looks beside the log only for a bare
  file name (never a path, for either). Fail closed, as for carried intents: carried values
  whose log is not given or found are a mismatch unless trusted (`--trust-resumed`, which
  covers both carry-overs: it is the same trust decision about where the log came from, and
  the bridge's own carry-over replay, which passes it, needs no change; the output still
  names each unchecked one, `UNCHECKED:`), and a carried restore that names no log by its
  file name is a mismatch, trusted or not. A carried restore names its log by file only (no
  head hash, unlike `intent.resumed`): replay prints the head of the log it was checked
  against, and that log's own anchors are what bind its content. From rules 12 the carry is
  not events at all but one line: the bridge reads the logs before this run's log is opened
  and the engine records them in `run.started` (`carried_from`: the log `.last-run` named and
  every log it still owed for whose carry had failed, tried again, each with its head and last
  seq as read and what it still owed, or why it could not be read), seeding the ledger from
  it; replay checks each against the log it names (`ReplayOptions::carried_logs`), its head
  included, so a successor with the carry emptied, partial or naming another log is caught,
  and a failed carry stays owed in the chain until read or written off (`restore.closed`).
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
    second to the next, AIS ones 0.3; from rules 18 a floor under the error the tracker
    reports for the track, `velocity_sigma_mps`, which counts when larger:
    `engage::track_velocity_error`); `track_accel_mps2` 0.2 m/s^2 on the surface and under
    water (a ferry or patrol boat in a 300 m turn at 10 m/s pulls 0.33 m/s^2; Bar-Shalom's
    rule of thumb puts the noise between half and all of the largest manoeuvre), 2.0 in the
    air and on the ground (a standard-rate turn at 55 m/s is 2.9 m/s^2). A track without a
    velocity estimate could be anywhere it reaches at its domain's top speed (25 m/s on the
    surface, 100 in the air, 30 on the ground, 15 under water): fail closed. Conservative
    where it matters: the keep-outs are read again every second while the plan waits and
    after approval, and at the release over a span of seconds, where the model is the
    tracker's own uncertainty; a track that turns toward the point shows in its next
    estimate. From rules 14 an aircraft with a velocity also counts anywhere it reaches from
    its speed bound (its speed, `keep_out_sigma` of its velocity's error, and the tracker's
    lag behind a speed-up), speeding up at a stated bound to the top speed, and every region
    of an aircraft adds how far the tracker may lag behind it (`engage::aircraft_reach_m`,
    `aircraft_speed_bound`, `AIRCRAFT_LAG_M`): an aircraft turns through a right angle in 15
    to 30 s, whenever it likes, and the white-noise model does not hold a turned one even
    seconds ahead.
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
    radar; the USVs' navigation radars and the MALE aircraft's surface-search radars are 2D:
    such a radar plots an aircraft at its slant range, and the tracker reads the plot as the
    bearing line it is on, no farther out, never as a height or a position, murmur-oho);
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
  - *Lost contacts* (rules 16, `lost.rs`, murmur-533 and its review): a contact the bridge's
    tracker drops (180 s without a detection) is not forgotten. The bridge hands the engine
    each drop with the track's facts then and the AIS identity it carried
    (`Engine::record_track_drops`, logged `track.dropped`; the tracker keeps the track in the
    decision facts until the engine has it, and one the log could not take goes back to it for
    the next tick); an id is dropped once (a second drop is refused and replay flags one; replay
    also holds a drop's fix to the pictures and the drop to the run's tracker drop age, see the
    changelog's 16). Every engagement's keep-out on tracks counts it as a lost contact, whether
    or not its id is in the picture (the tracker never reuses an id) (`WorldSnapshot::lost_contacts`,
    filled by the engine and logged with the decision): anywhere it could have reached since
    its last fix (`engage::lost_reach`): by the regions a live track gets from those facts
    (`engage::track_reach`) and, since nothing sees it turn or speed up, anywhere at its
    domain's top speed (region `out_of_view`, `engage::out_of_view_speed`: 25 m/s on the water,
    100 in the air with the tracker's 110 m lag, or its own bound if faster; a region at its
    last fix that already reaches as far stands for it), growing with no cap; exempt where a
    live track would be (a declared hostile, from the identification ledger). A live track
    unseen `engage::OUT_OF_VIEW_S` (10 s, the tracker's stale age) counts the same disc from its
    last fix (`engage::track_standing`, `track_reach_at` with the decision's time): its course
    is a prediction only while the tracker sees it. The check names a lost contact, since when
    and that it may be accepted (`lost contact T-0002 (neutral, fishing_vessel) could be 400 m
    ...: T-0002 out of view since t=... (184 s), dropped by the tracker at t=...; an operator
    with engagement authority may accept it for this engagement`), a live one `track T-0002
    ... out of view (no detection for 169 s) could be ...`. An operator with engagement
    authority may accept a lost contact for one engagement exactly as a lost vehicle of ours
    (`Engine::submit_accepting_lost` at the nomination, `Engine::accept_lost_contact` while it
    waits with no signature, logged as `engage.accepted` naming the `contact` and its last fix
    `since`, with the contact as the engine held it, `lost`; `Engine::revoke_lost_contact`
    withdraws it): picture tokens carry them (`lost_accepted`), a reset clears them, every
    decision logs them, the signer who accepted one is `self_accepted`. It covers that loss
    only. It takes the lost vehicle's step-up (`StepUpAct::AcceptUnknown`; a passkey's
    challenge names the contacts and their fixes, `StepUpFor::AcceptUnknown { contacts }`). A
    live track carrying a lost contact's MMSI (`WorldSnapshot::track_ais`, kept apart from
    `TrackFacts`, whose shape the tracker's comparison copies fix) is only a claim
    (`lost::claims`, `Engine::step_lost`, logged once as `lost.claimed` with the picture, the
    evidence and its blockers): never a retirement, since a contact may broadcast another's
    MMSI and the tracker finds a spoof suspect only once EO/IR has looked. Any operator confirms
    one under their own session (`Engine::confirm_lost_claim`, logged `track.reacquired` with
    `by`, `session` and the picture; an agent's port has no such method), and only while
    nothing contradicts it (`lost::claim_of` blockers: the only track carrying the MMSI and no
    other lost contact carrying it, undeclared, not suspect, the same domain, first seen after
    the lost contact's last fix, where it could have reached). The evidence compares the names
    and ship types, the distance against the reach, and what our sensors say of its kind. A
    confirmed contact is retired while its claimant stands (`LostLedger::confirmed`): once the
    claimant turns suspect or hostile, is declared or is dropped, every decision on that
    picture counts the lost contact again (`LostLedger::in_force_for`) and the loop logs it
    reinstated for good (`lost.reinstated`, `lost::void_reason`). A contact without AIS is
    never claimed (association across a drop is by identity, and nothing else names one
    contact).
  - *Live tracks accepted as behaving now* (rules 19, bd murmur-j0x, the owner's decision B of
    2026-10-26): an operator with engagement authority may accept one live track that blocks a
    waiting nomination's keep-out as it is behaving now, for that engagement only, at the
    nomination (`Engine::submit_accepting_all`) or before a first signature
    (`Engine::accept_live_track`): logged as `engage.accepted` naming the `track`, with its facts
    then (`behaving`) and the world's time; only a track in the picture, in view, with a
    velocity, protected (`engage::behaving_acceptable`), never twice while one is in force. For
    that engagement the keep-out on tracks counts it along the path accepted for it (its accepted
    velocity from its accepted estimate, `keep_out_sigma` of `track_prediction_sigma` of the
    accepted facts over the span since, rules 18's velocity error and the domain's white-noise
    acceleration included, and an aircraft's tracker lag) and along its course now (its live
    velocity and average velocity regions), not anywhere it could turn or speed up to
    (`engage::behaving_regions`, region kind `accepted`); every other track counts as before. The
    acceptance lapses on its own (`engage::behaving_lapse`) when the track leaves the picture,
    goes out of view (`OUT_OF_VIEW_S`), or its estimate departs from the accepted prediction by
    more than `keep_out_sigma` times their combined 1-sigma, in position (the prediction's and
    the estimate's) or, while the tracker shows a velocity, in velocity (each velocity's error and
    the white-noise acceleration's `q t`). The engine logs a lapse (`engage.acceptance_lapsed`,
    with its kind, its words, the world's time and the track's facts) before every step, look,
    signature and acceptance of the engagement (a waiting nomination with one is stepped for
    it), drops it for good and records it on the engagement (`Engagement::lapsed`, the card's); a
    decision whose own picture lapses one judges the track as if none had been given. The rest as
    the other acceptances: every signer's picture token carries them (`behaving_accepted`; a
    token from before a change is refused), a reset clears them, any operator revokes one
    (`Engine::revoke_live_track`), the `accept_unknown` step-up (a passkey's purpose names the
    track, `StepUpFor::AcceptUnknown { tracks }`), the signer who accepted is `self_accepted`,
    and only an `EngagementAuthority` takes one (an agent's port has no such method).
- **Keep-out explanation** (`keepout.rs`, murmur-12e; read-only, no rules change). The
  checks judge each track and vehicle of ours through `engage::track_standing` and
  `engage::friendly_standing` (the region the check reads, its kind, centre and radius,
  with the same numbers and words as before: 820 scenario logs unchanged), and
  `keepout::explain(spec, world, cfg, options)` reads the same functions on the dry run's
  snapshot (`options`: the tracker's memories, our vehicles' callsigns for its words). For
  every track or vehicle that blocks the keep-out, or nearly does (its nearest region's edge
  within twice the clearance; at most 12 listed, blocking first, the rest counted), an
  `Item`: the region that blocks (along its velocity or average velocity, a manoeuvre's, a
  young track's or (rules 14) an aircraft's reach, no velocity; for ours a course, a circle, a
  cruise reach, lost), that
  region at the effect time, the clearance (effect radius plus `keep_out_m`), the shortfall,
  how altitude counted (for aircraft), and what would clear it: `clear_windows` (same
  picture: the stretches of effect times, scanned every 0.25 s from the earliest any
  position allows, the dispatch delay or the window's opening, to the latest a nomination
  allows, the window's close or the longest transit, at which it is clear; `fits_until_t`
  when the first starts at the earliest), `wait` (the first nomination time, on a 1 s grid
  within 10 min, at which a dry run would find it clear on the picture carried forward by
  `keepout::project`: tracks in view, seen within the tracker's 10 s stale limit, hold course
  and speed and are seen as now, a young track gets its velocity as its average after
  `mean_min_s`, a manoeuvre leaves the memory after `manoeuvre_memory_s`; our vehicles keep
  doing what they do, carried as the check predicts them, a circle that holds one keeping it
  on it; the platform holds), or what happens instead (a track out of view: its region grows
  while unseen, and the tracker drops a track unseen for `drop_after_s`, after which it no
  longer counts, stated, not offered as a hint; one of ours lost: its reach grows), and
  `other` (a report, a velocity estimate once the track settles, an operator's logged
  acceptance of a lost friendly). For the platform, `MoveWithin`: every window of effect
  times at which every keep-out holds, as distances from the target when it holds where it
  is ("within X m" when the first starts at the earliest, "between X and Y m" farther), or
  that no position clears it, naming what blocks the earliest effect. It refuses (Err) for an
  effect radius above the policy's maximum, and unless every track and vehicle it lists as
  blocking is one the check fails on, judged alone (a picture with only it), and no other.
  The tracker's memories come from the bridge (`Tracks::memory()`); the defaults are the same.
  `tests/keepout.rs` checks each claim against `rules::evaluate`: on seeded random pictures
  (700 in the suite, from rules 18 most tracks' velocities with an error of their own;
  `MURMUR_KO_SEED`, `MURMUR_KO_WORLDS` for more), a dry run at the stated
  nomination time on the projected picture passes that item's keep-out and one second
  earlier fails; the platform placed within the stated distance passes and one step farther
  fails; placed so the effect lands exactly at times inside a window (a moving target's
  effect time set by placing the platform ahead of it on its course) it passes, at grid
  times outside every window it fails with the clearance raised by the grid's margin; and the
  explanation's checks are the dry run's. murmur-dr6: a stretch of passing grid points claims
  every effect time between them, so a grid point counts as clear only by a margin that bounds
  how far a region's edge can dip between two points (`keepout::grid_margin_m`: 0.3 m at the
  default 275 m clearance, `v^2 h^2 / (8 c)`), and "no effect time passes" says "on a 0.25 s
  grid"; after its review the margin is sized for the fastest relative motion the picture
  holds (`keepout::grid_margin_for`, `MoveWithin::grid_margin_m` and `grid_speed_mps`: the
  target's speed and each track's top speed or own bound with its uncertainty's growth, each
  vehicle's speed or cruise speed; over 100 m/s with an aircraft about) and the assumptions
  say both. Rules 16: a lost contact is an item (`Subject::LostContact`, with `lost`: what to
  accept, since when, when dropped), never given a wait (its reach only grows), with what
  would clear it: an acceptance for this engagement, or an operator confirming a track
  carrying its AIS as it; a live track out of view names its `out_of_view` region and that
  seeing it again would clear it, and is said to stay in the keep-out after its drop.
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
  view at every sample, default 4) good enough over the area. From rules 11
  (`SPACE_FOLLOWUPS_SINCE`) "over the area" means at every one of its sample points
  (`space::cover_points`: the centroid, then the boundary: eight bearings around a circle;
  a polygon's vertices, a closed ring's first vertex counted once, thinned to eight by
  geometry when there are more: the vertex farthest from the centroid first, then each time
  the one farthest from every vertex chosen so far, so the outline's far tips are always
  sampled): the query is one query over the points (`SpaceQuery::CoverLinks`, `CoverGnss`,
  one time map for all), answered with every satellite propagated once for all of them
  (`Space::links_area`, `gnss_area`: one window search per satellite driven by the centroid's
  cone widened by the area, one propagation per sample judged for every point, so nine points
  cost about one), the logged answer holds each point's figures and nothing aggregated, and
  the `coverage` rule decides on the least covered point (the fewest in view for GNSS) and
  names it, with the mean and the outages merged over the area in its detail. The boundary
  and the centroid bound a convex area for a field that varies on the scale of a satellite
  footprint (hundreds of km, against an area of at most 200 km radius), which assumes the
  field is close to linear across the area; the interior of a concave polygon is not
  sampled. Before rules 11 the service was read at the centroid alone (`Links`, `Gnss`),
  which older logs still replay with.

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

**After dispatch** (rules 11, every gate on `SPACE_FOLLOWUPS_SINCE`):

- **Cancel.** An operator may cancel a dispatched collect or cover (`Engine::cancel`, the
  same call as before dispatch; `POST /api/intents/{id}/cancel`, the Cancel button on the
  run's card). The engine logs the operator's `intent.cancel`, then its own `op.state
  cancelled` under the order's resource saying what the cancel means
  (`space::cancel_detail`, recomputed by replay from the dispatched order and the event's
  scenario time, the report logged at the very instant its detail was computed for), then
  the transition `cancel` to `aborted`: a collect before its pass has
  its simulated tasking request withdrawn and collects nothing; a collect whose pass has
  come cannot be recalled (a pass under way is a pass under way): the scene counts as
  collected and its product is no longer awaited, none is delivered; a cover's watch ends
  where it is, nothing having been commanded. The bridge is refused the `cancelled` report
  (it is the engine's), a dispatched vehicle task or engagement is refused the cancel as
  before (a newer order supersedes the one, an abort stops the other), and an agent
  withdraws nothing after dispatch, as before.
- **Re-check.** A dispatched collect or cover is checked again when a later space answer
  exists (`Engine::recheck_space`, with an answer the bridge prepares outside its lock): the
  bridge's space loader watches `sim/space/cache` (every 30 s) and reloads the segment when
  the element sets change on disk (a `murmur-space fetch` while the bridge runs), keeping
  the time shift pinned at the first load (the same elements pinned a day apart are another
  segment: `SegmentId` carries the pin; from rules 13 the engine refuses a segment pinned
  otherwise than the first, nothing logged, and replay flags a pin change between two
  `space.loaded`); the engine logs the new segment (`space.loaded`) and every running space
  task is re-checked against it, but never a collect whose pass has come: it was collected
  (simulated), a later answer has nothing to say about it, and replay refuses a re-check
  written after its pass (judged on the check's time: the engine logs the re-check at the
  instant its guard read the clock, so a clock moving between reads can't put an honest one
  past the pass).
  A collect is re-queried at the request time of the answer it was tasked on
  (`prepare_space_at`), so only the segment can change the answer, not the lead time spent
  since. An answer with the hash the task already runs on changes nothing and logs nothing;
  one that could not be computed (the query failed on the segment) is no answer: nothing is
  logged, the task runs on, and the inbox says so (`space_recheck_unknown`); a new one is
  checked as a submission is (`check.result` with phase `dispatched`, the rules reading only
  the logged answer), then the engine reports under the order's resource: `rechecked`
  (`still feasible: ...`) when the task holds (a collect keeps its approved pass, a fresh
  pass within `collect_match_tolerance_s` being the same opportunity; a cover's watch takes
  the new answer's outages), or `missed` (`no longer feasible: <every failing check>; <a
  collect's fresh ranked options still taskable at the check's own time: collect time at or
  after then plus the class's lead>`) and the intent failed (`collection missed: ...`,
  `coverage missed: ...`) when it cannot be met, with an inbox alert (`space_missed`) that
  carries the report. The engine never re-chooses a pass: the fresh options go to a human,
  who may propose a new collect. The bridge's clock-based `missed` (a pass over at dispatch,
  a jump past it) is unchanged and never starts with `no longer feasible`.
- **The lock.** The space query (milliseconds for a collect, seconds for a week of Starlink
  over nine points) runs before the decision lock is taken, on the shared segment
  (`prepare_space`, `prepare_space_at`); the engine uses the prepared answer under the lock
  only if nothing it depended on changed: the same task, answered by the segment and pin the
  engine holds now (`Engine::prepared_space_fits`; one reloaded in between answers again)
  and, for a collect, prepared within 5 s (`PREPARED_SPACE_MAX_AGE_MS`) of the check (its
  query carries the request time; a cover's does not depend on when it was asked, so its age
  is no concern). Without one, a collect's query is asked under the lock (milliseconds); a
  cover is never computed by the engine: its check fails closed, saying so, and the bridge
  prepares again outside the lock when the engine would not take what it had
  (`Decision::lock_with_space`, a few attempts). So no submission, approval or agent's
  preview holds the lock for the query, whatever the cover's size: what stays under it is the
  decision on the answer and its audit write; `tests/space_tasking.rs` measures the longest
  wait for the lock while a week of Starlink over a 50 km circle is submitted and previewed
  (bounded at half a second, a tenth of the picture-gap limit; tens of milliseconds on the
  fixture, under 25 ms live). The logged answer is the one the decision used. From rules 13
  (`SPACE_REPLAY_SINCE`) the decision also logs when its answer was computed (`computed_t`,
  engine time, beside the answer's hash in its inputs), the 5 s bound is the `space_query`
  rule's (a collect's answer computed farther from its check fails it, so replay recomputes
  the bound), and the engine takes a prepared collect only if it was asked at the scenario
  time of that computation, which replay checks too.

**Replay.** Each space query is logged once as `space.query`: the query, the answer
(`compute_ms` zeroed, the one non-deterministic field), its `time_map`, and the segment
identity (`SegmentId`: SHA-256 over the catalog and every satellite's identity, epoch, mean
elements and SGP4 state at two instants, plus `cache_meta`). Decision inputs reference it by
content hash, like the area set. Rules read only the logged answer, so `murmur-replay`
re-decides every collect and cover without a cache. With `--space <constellations.toml>
<cache-dir>` it also re-runs every query: same segment, the answer must be identical
(else a mismatch, exit 1); a different segment is a **cache mismatch**, listed separately
(exit 3; the decisions still reproduced). A log whose segment was reloaded carries two
`space.loaded` events, and the queries of the segment not given are cache mismatches.
Rules 11 bindings: a cover's verdict is recomputed from the logged per-point answers, the
query is rebuilt from the intent (an answer whose query points are not the area's is not
this intent's question) and the answer's result points must be the query's, in order, of
the query's class (`space::points_match`: a result point dropped, moved or copied over is
caught); the engine's reports are bound to what they follow: `cancelled` to an
`operator.action intent.cancel` of that dispatched space task right before it, its detail
recomputed from the dispatched order at the event's time, and followed at once by the
`cancel` transition naming that operator; `rechecked` or `missed` (`no longer feasible`) to a
`dispatched`-phase check right before it, the report recomputed from the recomputed results
(the fresh options from the check's own scenario time), a `missed` followed at once by the
failure quoting it, and a failed check never left unreported; a `dispatched`-phase check is
at the request time the task was tasked at, on the segment the last `space.loaded` put in
force and not the one the task's last answer came from, and never on a collect after its
pass has come; nothing of the intent between any of these.

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
`op.state`: "decision"), and at a clean shutdown (`logs/anchors/<run-id>.anchors`, each
timed on the clock the writer gives, the bridge's own, never earlier than the anchor before:
the verifier refuses anchors out of order, murmur-dup; and with the wall clock beside it,
`utc`, so any time in the log maps to UTC;
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

- Rules 8: the human behind each act is an authenticated person in a credentials run. A
  session opens only on a login whose password the bridge checked
  (`open_authenticated_session`), ends visibly (`session.closed`; nothing may follow under
  it), and a consequential signature (every engagement signature) needs a `StepUp`, which
  only `Engine::step_up` mints after the bridge checked the password again, for that session
  and that signature only. A dev run records that its ids were asserted, and replay and the
  evaluation keep it out of any claim about people.
- Rules 9: the engine may declare a track hostile by its hostile-act rule, a deterministic
  function of logged facts (`ident.rs`), and an operator may declare, confirm, revoke and
  resolve flags; an agent may do none of it. `Engine::declare_track`,
  `confirm_declaration`, `revoke_declaration`, `resolve_flag` and `record_act` take a
  `&HumanAuthority` or are the engine's own, `AgentPort` has none of them (compile-fail
  doctests on `AgentPort`, and one passing an agent id to `declare_track`). A rule
  declaration authorizes nothing: the engagement floor (two signers with engagement
  authority on one frozen plan, never the proposer) is unchanged.

**Limits.** Rust privacy is a code-structure guarantee, not a security boundary. It keeps
code that holds only an `AgentPort` (or only JSON) from authorizing, and it leaves one
greppable minting point. It does not stop someone editing this crate, bridge code that holds
`&mut Engine` opening an operator session on an agent's behalf (rule: only the bridge's
login endpoint opens one in a credentials run, and only `crate::auth` checks passwords before
a step-up), a compromised process, or a human who approves without looking. Authenticating
operators is the bridge's job (rules 8: `crates/murmur-auth` and the bridge's `auth`
module); this crate records how it was done and refuses a consequential signature without a
step-up. Whether human
approval stays meaningful at scale is what Murmur measures; this crate makes sure there is a
human decision to measure.

## Replay

```
cargo run -p murmur-decide --bin murmur-replay -- logs/audit/<run-id>.jsonl \
    [--rules N | --min-rules N] [--anchors logs/anchors/<run-id>.anchors] \
    [--space sim/space/constellations.toml sim/space/cache] \
    [--prev logs/audit/<previous-run>.jsonl]... [--trust-resumed]
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
  last and still under way (the bridge supersedes in the same step as that dispatch), or
  from rules 7 the one queued last and still queued, or a mission just refused for its
  platform's own lost-link values (its kind never queued), each only in that order's step,
  with that intent's own source; a `system` stop that quotes a vehicle event (`the platform's
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
- Starts logged ahead (rules 7): a vehicle task's `op.state` starting carries `ahead`, nothing
  else does, and on an order's first delivery a vehicle reports taking it only after its
  start was logged ahead.
- Lost-link values owed back (rules 7): every decision's `restore_owed` is the ledger's (the
  values an engagement of the log preloaded and no confirmed restore put back), so a forged log
  can't strip it to justify a mission's dispatch, and the reach recomputed from it holds.
- Space tasks after dispatch (rules 11): a `cancelled` report follows an operator's cancel of
  that dispatched collect or cover and says what the cancel meant for the dispatched order at
  that time (recomputed), the `cancel` transition follows it at once naming the operator; a
  `rechecked` or `missed` (`no longer feasible`) report follows a `dispatched`-phase check
  of that running space task, says what the recomputed results say, and a `missed` is
  followed at once by the failure quoting it; a cancel, a re-check or a missed report that
  nothing carries out is flagged; a cover's verdict is the one its logged sample points give,
  which are its query's points; a re-check keeps the task's request time, follows a reload
  and never comes after a collect's pass.
- Space answers' times (rules 13): every answered space input says when its answer was
  computed (`computed_t`); a collect's answer outside a re-check was asked at the scenario
  time of that computation (the last `space.loaded` map), and the `space_query` rule,
  recomputed, holds it within 5 s of its check; the pinned time shift (both where a load
  states it) and the scenario map are the same in every `space.loaded` of the log.

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
loiter radius, the rules 6 and 7 facts listed below; `rules_version` and
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
downgrade cases. Gate on a named constant, never a literal (rules 7 is
`QUEUE_FOLLOWUPS_SINCE`, and `RULES_VERSION` is defined from it): when two branches each
take the next number, the one that merges second renumbers in one line (its fixtures, whose
`run.started` names the number, are written again by the merged build).

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
  engagement authority refused with a 403, nothing logged); and
  `v6-airship-nomination-scenario-66.jsonl` (master `981cdf3`), the small airship nominated
  and refused by `fallback_preload`. Its detail still says "Blimp 4.8's only lost-link
  action is Land", which misreads the firmware (Blimp 4.8's GCS failsafe can only disarm;
  the documented Land is not implemented, murmur-3zj); a detail is compared by replay, so
  the wording is kept and rules 7 corrects it (murmur-a9h).
- **7** (provisional number: `QUEUE_FOLLOWUPS_SINCE`), the queue's follow-ups to rules 5 and
  6 (murmur-dma, murmur-85l, murmur-636; see "The queue for unreachable vehicles"): a vehicle
  task's start logged ahead of sending it (`op.state` starting with `ahead`; the engine and
  replay refuse a start logged afterwards, and, on an order's first delivery, a vehicle taking
  it before its start was logged), so a carry-over after a crash never starts it again on its
  own; an order other than a hold or rtl held in the queue for a vehicle still owed its own
  lost-link values by an engagement of the run (`restore_owed`, a decision's fact the engine
  fills from its ledger and replay recomputes from the log's; the reach says why, and a
  human's approval that goes back to the inbox from the queue says why it was queued); an order
  queued for an engagement's platform supersedes the engagement in the step that queues it,
  as a dispatch does (cause `superseded`, no fallback; from rules 7 a supersede, queued or
  dispatched, only in the step of that order, nothing logged between); a mission for a kind
  the policy never queues is voided while its platform is owed its values (never sent), and
  that refusal still supersedes an engagement in play on its vehicles, in its step;
  every delivery logs its start, or its follow of an order already running, ahead; the engine
  refuses an op that ends `requeued` after its start went out on that delivery (replay
  rejects that in every log; before rules 7 the engine still wrote it); and an airship's
  `fallback_preload` refusal says its GCS failsafe only disarms (it said Land, which Blimp 4.8
  doesn't have: murmur-a9h). Gated in
  `queue::reach`, `Engine::record_op_state_ahead` and `op_state_mismatch`,
  `Engine::supersede_engagement` and `supersede_mismatch`, and `expire_queued`'s reason; the bridge's write-ahead, its restores (none into a dead link, one
  at once when the vehicle is heard again) and its supersede on queueing are gated on the same
  constant. Markers (a log claiming rules 6 or older can't carry them): `restore_owed` in a
  decision's inputs, `ahead` on an op state; and replay refuses an engagement superseded by a
  queued order under older rules. Fixtures: `tests/data/audit/v7-engagement-scenario-01.jsonl`,
  `v7-queued-order-supersedes-scenario-67.jsonl`,
  `v7-mission-waits-for-restore-scenario-68.jsonl`,
  `v7-refused-mission-supersedes-scenario-71.jsonl`, and a live run with its anchors
  (`v7-queue-followups-live.jsonl`, `tests/data/anchors/v7-queue-followups-live.anchors`,
  stand-ins, clean build 6da2fe4, the scenario logs too: thirteen nominations rejected while
  a ferry, then a light aircraft, was predicted inside the keep-out; an operator's orbit for
  the engaging USV queued for its own lost-link values, superseding the engagement with no
  fallback, sent the second after the restore confirmed, its start logged ahead; a second engagement aborted while the USV's link
  was cut, its values back the second the link returned; a third the same, with an orbit
  queued in the outage that went out only after the restore).

- **8** (murmur-der.1; every gate reads `OPERATOR_AUTH_SINCE`, so another change taking the
  number first renumbers it in one line): operators are authenticated people, and a
  consequential signature re-enters the signer's credential. `run.started`'s roster says how
  operators prove who they are (`auth`: `credentials`, each person's password checked by the
  bridge against an argon2id hash, the roster being the credential file; or `dev`, ids a
  request asserts, never evidence about people; an engine given no mode logs `dev`, so
  scenarios and tests do). Every `session.opened` says how it was opened (`auth`: `password`
  or `scripted` in a credentials run, a person's credential or one a harness holds, never a
  person's; `asserted` in a dev run: the run's own methods only, a credentials run opening
  sessions only through `Engine::open_authenticated_session`, one per login); a session's end
  is logged (`operator.action session.closed`, detail: logout, expiry, a credential removed
  or changed; `Engine::close_operator_session` consumes the session and drops its picture
  tokens) and nothing may follow under it; a human's `intent.created` names the live session
  it was made in (an agent's none); every other operator action is under that operator's own
  session. A decision the policy marks consequential (`policy.decision`'s `step_up`: every
  engagement, whatever the file says, and any human tier with `step_up = true`, policy
  schema 5) is never approved at submission, and in a credentials run each of its signatures
  quotes a step-up: `operator.action step_up` (`auth` the session's method), logged by
  `Engine::step_up` after the bridge checked the password again, consumed by one signature
  (`step_up: {seq}` on `approval.concurred` and `approval.granted`) under the same session,
  at most `STEP_UP_MAX_AGE_MS` (10 s) old; the engine refuses a signature without it
  (`approval.refused`, `StepUpRequired`). `[approval] step_up_acts` (policy schema 5, off by
  default) adds a hostile declaration (`Engine::record_declaration`) and an acceptance of a
  lost vehicle (a nomination's acceptances share one step-up). A refused login for an
  operator on the roster is logged (`auth.refused`: who, why, the lock it set; never a
  secret), only in a credentials run. An intent is carried over only from a run that
  authenticated alike (`ResumedFrom.auth`; a log of earlier rules counts as dev).
  `StepUp` has private fields and no `Clone` or `Deserialize` (compile-fail doctests in
  `authority.rs`). Replay checks all of it and says whether a log is evidence about people
  (`ReplayReport::authenticated`: a credentials run that reproduced; `people` apart from
  `scripted` operators; a claim also needs the log's anchors, which `murmur-replay` says
  when they were not checked). Markers (a log claiming rules 7 or older can't carry them):
  an `auth` mode or method, a `step_up` (a tier's key, a decision's, a signature's, an act's,
  the event), a `session.closed`, an `auth.refused`, `step_up_acts`. Fixtures:
  `tests/data/audit/v8-engagement-scenario-01.jsonl` (the scenario runner: `auth: dev`) and
  a live run with its anchors (`v8-auth-live.jsonl`, `tests/data/anchors/v8-auth-live.anchors`,
  from a clean build of the merged head: op-3 signed in at the console first; a wrong
  password for op-2 logged as `auth.refused`, an unknown operator (nothing logged), a bare
  header, an agent's key on an operator endpoint, a request from another origin and one
  carrying two session cookies refused; op-1 and op-2 logged in over the API; a stranger's
  five wrong passwords for op-3, each logged, the fifth locking op-3's next login; an agent
  on its own key; a declaration and a nomination by an operator without engagement
  authority, who could not sign it; a signature refused without a password, with a wrong one
  and with another person's (each `step_up.refused` under the signer's session), then made
  with their own; the same person again over a second login, stepping up, refused by the
  engine (already signed); the second signature from the console, stepped up, on the session
  the lock never touched; the effect released and assessed; every session closed at
  logout). The bridge also logs a refused step-up (`operator.action step_up.refused`, never
  the password) and closes every login at a clean stop (`bridge stopped`).

- **9** (rule-based identification with human review; every gate reads `RULE_ID_SINCE`,
  policy schema 6 `RULE_ID_SCHEMA_SINCE`): the engine keeps an identification ledger
  (`ident::Ledger`: declarations in force, flags and their standing, acts seen) and fills
  every decision's tracks from it (`declared_by`, `declaration`, `review_flags`, and the
  affiliation), so a declaration is no longer a field the bridge's picture supplies. An act
  our sensors observed (`Engine::record_act`, `ident.act` with the picture it was attributed
  in, its candidates and its verdict) is attributed to the tracks (a Gaussian likelihood
  against an untracked density, rounded) and, with `[identification] rule_declaration = true`,
  declares its author hostile by rule (`ident.declared`, `by: {kind: rule, rule: hostile_act,
  evidence}`) at `attribution_threshold` or more with nothing conflicting, or is flagged
  (`ident.flagged`: attribution, conflict, untracked, victim). `Engine::step_identification`
  flags a track keeping company with a declared hostile (proximity: suspect, never a
  declaration). Operators act under their own sessions: `declare_track` (`ident.declared`, `by:
  {kind: operator, ...}`; a hostile one steps up where `step_up_acts` lists
  `declare_hostile`), `confirm_declaration` (`ident.confirmed`; steps up only where
  `confirm_declaration` is listed), `revoke_declaration` (`ident.revoked`, naming the
  engagements in play on the track, each stopped next, before anything else, with cause
  `revoked`: voided while pending or approved, aborted with its fallback once dispatched), and
  `resolve_flag` (`ident.flag_resolved`). `target_declared` passes on an operator's
  declaration or a rule's, a rule's only once confirmed unless `nominate_unconfirmed`; a
  rule declaration nobody confirmed stays protected in other engagements' keep-outs; the
  picture token and frozen plan carry the declaration (`TrackIdentity.declaration`), so a
  confirmation mid-signing resets the plan. After the review: decisions read an undeclared
  track's own affiliation (a proximity flag shows it suspect to readers only); a victim is
  one of our fleet (the act's inputs log it: `vehicles`) or a tracked contact not declared
  hostile, and never a candidate for its own author; `corroboration = "second_observation"`
  wants another observer's fix (`ActReport.fixes`) or an earlier act attributed to the same
  track at the threshold that passed every check but corroboration (a rule declaration, or an
  `uncorroborated` flag; never one flagged for its victim or for conflicting evidence),
  observed at most `corroboration_window_s` before (flag kind `uncorroborated` until then;
  `RuleEvidence.corroborated_by` names it); an act names its channel (`sim:contacts`,
  `scenario`); a declaration's or flag's time and its event's are one clock read; a revocation
  also stops an engagement under way whose last picture held the track exempt from its
  keep-out (`ident::revocation_bystanders`, recomputed by replay from the last logged pictures;
  an approved engagement not yet watched counts its approval's re-check as its picture). After
  the second review the keep-out reads two more facts the tracker gives (rules 9, absent from
  older logs): a track's average velocity over the last minute (`mean_velocity_mps`: a region
  along it) and, for a track that manoeuvred within the tracker's memory, the fastest it went
  (`manoeuvre`: a region anywhere it reaches at that speed); `engage::track_reach` lists the
  regions and the keep-out holds only if the effect point clears every one. Rule declaration off (the default when the
  section is absent), the engine decides exactly as rules 8 did. `Engine::record_declaration`
  and the `track.declare` operator action are refused under rules 9. Replay recomputes every
  attribution, verdict, declaration and flag from the logged inputs, binds every logged
  picture's identification to the ledger, and catches a declaration without its act, above
  its recomputed attribution, claimed for an operator who did not make it, a confirmation or
  revocation of something not in force, and a revocation's stop cut from the log. Markers (a
  log claiming rules 8 or older can't carry them): the `ident.*` events, a track's
  `declaration` or `review_flags`, a stop's `revoked` cause, a policy's `[identification]`
  (and its schema-6 claim, `confirm_declaration` in `step_up_acts`). Fixtures:
  `tests/data/audit/v9-engagement-scenario-01.jsonl` (scenario 1, its declaration through the
  ledger), the five rules 9 scenarios (`v9-hostile-act-declared-scenario-72.jsonl`,
  `v9-ambiguous-attribution-flagged-scenario-73.jsonl`,
  `v9-neutral-alongside-flagged-scenario-74.jsonl`,
  `v9-revoked-mid-engagement-scenario-75.jsonl`, `v9-bystander-protected-scenario-76.jsonl`,
  `v9-revoked-bystander-scenario-77.jsonl`, `v9-revoked-before-picture-scenario-78.jsonl`),
  and a live run with its anchors (`v9-identification-live.jsonl`,
  `tests/data/anchors/v9-identification-live.anchors`; see `tests/versions.rs`).

- **11** (murmur-0nv.1.7; every gate reads `SPACE_FOLLOWUPS_SINCE`, so another change taking
  the number first renumbers it in one line; no policy schema change): satellite tasks
  followed up after dispatch, and covers over the area (see "After dispatch" and "cover"
  under "Satellite tasking"). An operator may cancel a dispatched collect or cover
  (`operator.action intent.cancel`, the engine's own `op.state cancelled` saying what the
  cancel means for the pass, transition `cancel` to `aborted`); a later space answer (the
  element sets reloaded) re-checks every running space task (`check.result` phase
  `dispatched`, then the engine's `op.state rechecked`, or `missed` with `no longer
  feasible: ...` and the intent failed; a collect re-queried at the request time it was
  tasked at, `prepare_space_at`); a cover is evaluated at sample points over its area (the
  centroid and the boundary, `space::cover_points`; `SpaceQuery::CoverLinks`, `CoverGnss`;
  the coverage rule deciding on the least covered point, every satellite propagated once for
  all of them); and a prepared space answer is used under the lock only if the segment and
  pin that answered it are the engine's, a cover never being computed by the engine itself.
  After the review of its first cut: a collect whose pass has come is never re-checked, a
  re-check keeps the task's request time and follows a reload (replay holds both), an
  answer that could not be computed is unknown, not missed, the fresh options are those still
  taskable at the check's time, `SegmentId` carries the pin (a new optional field, absent
  from older logs), and a cancel's report is logged at the instant its detail was computed
  for. Gated in `SpaceRules::area_cover` (`query_for`), `Engine::cancel`,
  `Engine::recheck_space`, the engine's refusal of its own reports from the bridge
  (`op_state`), the bridge's `recheck_space_runs` and its cancel of a run. Markers (a log
  claiming rules 10 or older can't carry them): a `cover_links` or `cover_gnss` query, a
  `cancel` transition, a `cancelled` or `rechecked` op state, a `dispatched` check phase.
  Fixtures: `tests/data/audit/v11-engagement-scenario-01.jsonl` (the scenario runner),
  `v11-space-followups-engine.jsonl` (the engine on the element-set fixture, written by
  `tests/space.rs` with `MURMUR_WRITE_FIXTURE`: an area cover, a collect cancelled before
  its pass, one re-checked on a reloaded segment and missed), and a live run with its
  anchors (`v11-space-followups-live.jsonl`, `tests/data/anchors/v11-space-followups-live.anchors`;
  see `tests/versions.rs`).

- **10** (murmur-437, passkeys; every gate reads `PASSKEY_SINCE`, policy schema 7
  `PASSKEY_SCHEMA_SINCE`, so another change taking the number first renumbers it in one line):
  a login or a step-up may be a passkey's (`crate::passkey`). A credentials run's
  `run.started` roster names the WebAuthn relying party and every operator's passkeys as
  public keys (`passkeys`: `rp_id`, the console `origins`, per key the operator, credential id,
  ES256 public key as a SEC1 point, and its counter); a dev run takes none. A passkey use is
  logged with its assertion (`passkey` on `session.opened` and `operator.action step_up`, `auth:
  passkey`: the credential, its counter, what it is for, `for`, the server's nonce and issue
  time, and the authenticator data, client data and signature, none of them secret); its
  challenge is derived (`passkey::challenge`) from the hash of the log's first line, the
  session (none for a login), the operator, `for`, the nonce and the issue time, so the
  signature binds the person's authenticator to that act in that run. The engine
  (`Engine::passkey_challenge`, `step_up_passkey`, `open_passkey_session`) verifies each use
  with its own code (`passkey::verify`: the client data a `webauthn.get` for exactly that
  challenge from a listed origin, the authenticator data for the relying party with the user
  present and verified, the ECDSA P-256 signature under the logged key), refuses another
  operator's credential, a removed one, a counter that does not count up (a cloned
  authenticator; 0 then 0 is a passkey that never counts), a challenge answered twice or older
  than `CHALLENGE_TTL_MS` (120 s), a login's assertion as a step-up and a scripted session's
  passkey; and an act quoting a passkey step-up must be the act it was made for (`StepUpFor`:
  a signature's intent and, for an engagement, the picture token it quotes and the SHA-256 of
  that token; a hostile declaration's track; a confirmation's declaration; acceptances' lost
  vehicles), else it is refused (`approval.refused` for a signature). `[approval]
  step_up_factor` (`password`, the default, takes either; `passkey` refuses a person's password
  step-up) and `login_factor` (`either`, `password`, `passkey`) are policy schema 7; a scripted
  credential steps up as it is held. Passkeys registered or removed while the run is up are
  logged (`passkey.registered`: the key, `via` a session quoting its password step-up, or an
  invitation; `passkey.removed`). A study run names its participant on the roster (`study`,
  with `password_step_up`, the reason of the explicit override, when one allowed passwords);
  the engine refuses to start one without the participant's passkey or with a policy that would
  take a password, unless overridden. Replay re-verifies every assertion against the roster's
  keys and those registered since (challenge, origin, relying party, flags, signature, counter,
  nonce used once, issue time within the lifetime), checks every quoted passkey step-up was made
  for the act quoting it and the policy's factor (a registration's step-up is a password by
  design), and reports passkey logins, step-ups by factor, registrations, removals and the
  study (`StudyReport`; `ReplayReport::study_evidence`: authenticated, a study, no override,
  every consequential act of the participant's by passkey). Markers (a log claiming rules 9 or
  older can't carry them): `auth: passkey`, a `passkey` assertion, the roster's `passkeys` and
  `study`, `passkey.registered`, `passkey.removed`, `step_up_factor`, `login_factor`. An
  engine of older rules drops a roster's passkeys and study and a policy's factors. Fixtures:
  `tests/data/audit/v10-engagement-scenario-01.jsonl` (the scenario runner: `auth: dev`, no
  passkeys) and a live run with its anchors (`v10-passkeys-live.jsonl`,
  `tests/data/anchors/v10-passkeys-live.anchors`; see `tests/versions.rs`).

- **12** (the reviews' follow-ups; every gate reads `REVIEW_FOLLOWUPS_SINCE`, so another
  change taking the number first renumbers it in one line; no policy key changes, schema 7):
  - *A debt closed without a restore* (murmur-q05): `restore.closed` closes a platform's own
    lost-link values (or an earlier log whose carry failed) for good. Right after the release
    of an engagement whose platform the catalog says is spent by its own effect
    (`expended_by_effect`: switchblade, fpv; a world fact of the platform, read off the release
    picture), the engine closes what is owed to it (`expended`, naming the engagement): there
    is no platform left to put its values back on, and before they stayed owed for good (the
    passkeys live run's switchblade: UNRESTORED, alerted, carried into the next run). An
    operator writes off a platform's values (lost, spent, another vehicle under its uid) or a
    failed carry, under their own session with a note (`Engine::write_off_restore`,
    `written_off`; the bridge's `POST /api/restores/write_off`, the console's Write off on a
    restore alert); never while an engagement is under way on the platform; an agent can't.
    Replay checks each: the values exactly what was owed, an expended closure right after that
    engagement's release with its release picture showing the platform spent, a write-off
    under the operator's own session with a note; it lists them (`restores_closed`,
    `CLOSED:`). An expended closure also marks the platform spent: from then on it takes no
    order and no nomination (`readiness` fails on the decision's `expended` fact, which replay
    checks against the ledger; every dispatch path refuses it; an order approved before the
    effect is voided, `spent platform: ...`; replay flags any order sent to it).
  - *A young track counts as possibly manoeuvring* (murmur-9m0): the tracker reports when it
    started a track (`first_seen_t`); a track with a velocity but no average velocity yet (its
    history too short to show a manoeuvre: its first 45 s or so, or after a gap) also counts in
    a keep-out anywhere it reaches at its domain's top speed (25 m/s on the surface), or at its
    own speed plus `keep_out_sigma` of its velocity error if that is faster. Before, it fell
    back to the one region along its velocity. Cost: larger keep-outs for a new track's first
    minute (a surface track 2 km away blocks an effect 80 s out). The scenario runner gives a
    track the file lists 600 s of history (`tracked_s` overrides it) and one a step makes none:
    it appears then.
  - *Scripted operators fixed at the start* (murmur-jfi): a credentials run's roster lists the
    harness-held credentials (`scripted`, always, empty when none); a session opens by the
    method it gives its operator (the engine refuses, and replay flags, a scripted session for
    a person or a person's for a scripted credential), so a credential re-marked mid-run can't
    move an operator between people and scripts in the log, and a relabelled session (forgery
    14) is caught. The bridge's step-up 429 says "for this session". A login refused for it is
    logged (`auth.refused`), and the browser's own session ends only once a new one opened.
  - *What a start carried, in one line* (murmur-cq3): `run.started` records `carried_from`:
    the log `.last-run-<port>` named and, tried again, every log it still owed for whose carry
    had failed, each with its head and last seq as read and what it still owed (the
    platforms' values, the vehicles, the engagements, the logs pending), or why it could not be
    read. The ledger is seeded from it (no carried `platform.restore` any more: the engine
    refuses one and replay flags it), so a crash can't leave half a carry. Replay checks each
    named log (`ReplayOptions::carried_logs`, `murmur-replay` beside the log or `--prev`): it
    replays, it is the one read, it still owed exactly that; a log carried that nobody named,
    or a pending one not tried again, is a mismatch. A failed carry stays owed (pending) in
    the log that recorded it until a later start reads it or an operator writes it off, so the
    pointer's rewrite no longer drops it from the chain. A pointer that is empty, garbled, names
    no log or can't be read is a failed carry under its own name (`.last-run-<port>`: the log it
    named is unknown), never a failed start; the bridge rewrites it at every start, whole
    (temporary file and rename). Replay checks a log carried from up to the last seq recorded
    (a torn last line is not part of it), and a log recorded as failed that is at hand and
    reads, still owing, is a mismatch unless trusted. A failed carry records the armed
    platforms its run's fleet file names (`may_owe`: identity, values unknown); a mission for
    one waits in the Queue until the log is read or written off.
  - *Carried values hold a mission* (murmur-qjf): values carried over from an earlier run hold
    an order other than a hold or rtl for the vehicle they were preloaded on, as an
    engagement's of this run do; the decision's `restore_owed` fact carries the bridge's
    identity of the vehicle under the uid now (`carried`: `from`, `preloaded_on`, `now`,
    `other_vehicle`), and replay recomputes from it whether they hold (another vehicle: they
    don't; unknown: they do). A kind the policy never queues logs no reach, so there they hold
    whatever vehicle has the uid. Only the platform, the callsign or the system id says another
    vehicle; the home never does (ArduPilot sets it again at every arming).
  - *A refusal names the orders still queued* (murmur-54l): a mission refused for its
    platform's own values replaces no older queued order; its reason now names those still
    standing on its vehicles, and replay recomputes the reason and compares it whole (any other
    void of such an order must be one the bridge gives for something else: its task layer, a
    vehicle lost, a newer order).

  Markers (a log claiming rules 11 or older can't carry them): `carried_from`,
  `restore.closed`, the roster's `scripted`, a track's `first_seen_t`, a `restore_owed`
  entry's `carried`, a decision's or a closure's `expended`, a failed carry's `may_owe`. Older
  carry tests are pinned to rules 10 (`tests/carried.rs`,
  `tests/rules6.rs`). Fixtures (a clean build of `69c1c24`, master's rules 11 merged):
  `tests/data/audit/v12-engagement-scenario-01.jsonl` (the switchblade owed nothing after its
  effect, as scenario 80), `v12-young-track-blocks-scenario-79.jsonl`,
  `v12-refusal-names-queued-order-scenario-81.jsonl`, `v12-restore-written-off-scenario-82.jsonl`,
  and three live logs with their anchors, stand-ins at the Gate (`v12-carry-live-crash-1.jsonl`,
  `v12-carry-live-crash-2.jsonl`, `v12-carry-live-after.jsonl`, `tests/data/anchors/` the same
  names): the first started on an empty `.last-run` pointer (recorded as a failed carry,
  alerted, the pointer rewritten); the bridge killed (SIGKILL) twice a second after the USV's
  lost-link preload confirmed, each next start recording the crashed log, its head and the
  USV's values owed in `run.started` and putting them back on the same vehicle (op-1 wrote the
  pointer's unknown debt off in the second); in the last, the USV spawned 1.2 km away (its
  home about 980 m from the one preloaded, as after arming elsewhere) and heard but its
  downlink cut for 150 s, an orbit for it held in the queue for the values carried over and
  sent the second the restore confirmed, then a clean stop owing nothing (see
  `tests/versions.rs`).

- **13** (murmur-der.4, murmur-lsj; every gate reads `SPACE_REPLAY_SINCE`, so another change
  taking the number first renumbers it in one line; no policy key changes, schema 7): replay
  holds a space answer to when it was computed, and a re-check to the instant it judged.
  - *When an answer was computed* (murmur-der.4): the engine took a collect's answer prepared
    outside its lock only within 5 s of the check (`PREPARED_SPACE_MAX_AGE_MS`), and replay
    accepted any. Now every answered space input logs the engine time its answer was
    computed at (`computed_t`, beside its hash in the decision's inputs: the bridge's
    preparation, or the engine's own query at the check), and the `space_query` rule fails a
    collect whose answer was computed more than 5 s from its check (`cannot evaluate: the
    space answer was computed 12.000 s before the check, more than 5 s`) or does not say when,
    so replay recomputes the bound with the check. Replay binds the time to the answer: outside
    a re-check (which keeps the tasking's request time) a collect's request time is the
    scenario time of `computed_t` under the last `space.loaded` map, and every answered input
    carries one; the engine takes a prepared collect only if it was asked at that time. A
    cover's answer carries its time and no bound (its question does not depend on when it was
    asked). Neither the answer's computation nor the decision's snapshot may be later than the
    event that logs the decision (the review of 10-07 moved both 4 s past it, within the
    bound, and it replayed): the engine logs such a check no earlier than either (in the
    bridge both are read before it), and refuses a re-check whose answer was computed after
    its guard's instant.
  - *A re-check at its guard's instant* (murmur-lsj): `Engine::recheck_space` judged "has the
    pass come" on one clock read and logged its check on a later one, while replay judges on
    the check's time: a clock moving a millisecond per read flagged an honest re-check one or
    two milliseconds before the pass. The re-check (its answer and its check) is now logged at
    the guard's instant, as a cancel's report is; an engine fix in every rules version (no
    decision changes, only the timestamps).
  - *The pin is the run's* (murmur-lsj): the same elements pinned otherwise are another
    segment (`SegmentId` carries the pin), and the engine took one as a reload. The bridge pins
    once per session, so the engine now refuses a segment pinned to another shift than the
    one loaded first (`EngineError::SpaceSegment`, nothing logged; a reload of the elements on
    the same pin is taken) and replay flags a pin change between two `space.loaded`. A load
    says its pin twice (`pinned_shift_days`, and the segment identity's): both must agree, at
    every load (the review of 10-07 forged the identity's alone and it replayed), and the
    engine takes no segment whose two disagree. The scenario clock is held the same way (the
    review): the engine refuses a segment whose scenario map is not the first load's (the
    bridge's is fixed at its start) and replay flags a map change between two `space.loaded`.
  - Stated, not changed: since the review of rules 11 the engine never computes a cover itself
    (a cover's check fails closed without an answer prepared outside the lock), under every
    rules version, 10 and earlier included; replay reads only logged answers and can't tell
    who computed one, so their logs replay as before.

  Gated in `rules::checked_answer` (the bound), `Engine::ask_space` (the request time of a
  prepared collect) and `Engine::set_space` (the pin); the answer's time is logged from the
  same constant. Markers (a log claiming rules 12 or older can't carry them): `computed_t`.
  Fixtures (a clean build of `caaa210`, the review's fixes in): `tests/data/audit/v13-engagement-scenario-01.jsonl`
  (the scenario runner) and
  `v13-space-replay-engine.jsonl` (the engine on the element-set fixture, written by
  `tests/space.rs` with `MURMUR_WRITE_FIXTURE`: an area cover, a collect cancelled before its
  pass, one re-checked on a reloaded segment and missed, every answer with its time).

- **14** (murmur-oho, the air tracker calibrated against truth; every gate reads
  `AIRCRAFT_REACH_SINCE`, so another change taking the number first renumbers it in one line;
  no policy key changes, schema 7): *an aircraft counts anywhere it could turn or speed up to*.
  A protected aircraft with a velocity also counts in a keep-out anywhere it reaches from its
  speed bound, speeding up to the domain's top speed (100 m/s), whether or not the tracker saw
  it manoeuvre (`engage::aircraft_reach_m`, region `aircraft`). The bound
  (`aircraft_speed_bound`) is its speed, `keep_out_sigma` of its velocity's error in its least
  certain direction (the tracker's own, `TrackFacts::velocity_sigma_mps`, or the policy's
  `track_velocity_sigma_mps` when larger or not given), and the tracker's lag: as if the
  aircraft had been speeding up for `AIRCRAFT_LAG_S` (7 s) unseen at `AIRCRAFT_SPEEDUP_MPS2`
  (4.5 m/s^2, a bound over every source of aircraft motion here: the contacts process's 0.5,
  the catalog's stand-ins' 2.5 to 4.2, the air calibration's 3; not the policy's air
  `track_accel_mps2`, a sigma), no faster than the top speed for the lag. Every region of an
  aircraft, with a velocity or without, adds where the lag may have taken it,
  `AIRCRAFT_LAG_M` (110 m). The keep-out explanation names the region ("An aircraft may turn
  or speed up at any moment") and, for an aircraft, does not offer a velocity estimate as the
  remedy (what clears it: a ranged altitude above the ceiling, the effect sooner, the aircraft
  farther than it could fly by then). Before, a straight-flying aircraft with a velocity
  counted only along its course and its average course: calibrated against the truth, the
  bridge's air tracker gives one a velocity most of the time, and a keep-out then missed an
  aircraft that turned or sped up, even seconds ahead (the bridge's `tracks::tests::calib_air`:
  with this region a keep-out 3 to 100 s ahead held every aircraft of the tuning sets every
  time, and the held-out set's but twice, by 20 m and 6 m, both without a velocity). Cost: an
  aircraft with a velocity blocks a keep-out about as far as one without at every span (an
  aircraft at 50 m/s, 15 m 1-sigma: 411 m at 3 s and 3.1 km at 30 s, where one without a
  velocity counts 440 m and 3.1 km). Surface, ground and subsurface tracks are judged as
  before. New logged fact: a track's velocity error (`velocity_sigma_mps`), which a log
  claiming rules 13 or older can't carry (an engine on older rules leaves it out); a rules
  14 log claiming rules 13 differs too where an aircraft's region decided, and replay
  recomputes those decisions. Scenario 83 (an aircraft flying away 2 km from the effect point: rejected; 12 km
  out, pending); `tests/aircraft_reach.rs`. Fixtures (a clean build of `f097a0a`):
  `tests/data/audit/v14-engagement-scenario-01.jsonl` and
  `v14-aircraft-may-turn-back-scenario-83.jsonl` (see `tests/versions.rs`).
- **15** (the follow-ups of rules 12's carries, murmur-zq3 and murmur-imu; every gate reads
  `CARRY_FOLLOWUPS_SINCE`, first numbered 13 and renumbered in one line when the space replay and
  the aircraft reach took 13 and 14; no policy key changes, schema 7):
  - *A spent platform stays spent across a restart* (zq3 (1)): a log's debts include the
    platforms it spent and nobody un-spent (`Debts::spent`: the engagement, the values its
    closure closed, the vehicle they were preloaded on, the log it is in when carried into it);
    the start records them in `run.started` (`carried_from`'s `spent`), the ledger keeps them,
    and a decision's `spent_carried` fact (`queue::CarriedSpent`, with the bridge's identity of
    the vehicle now) fails `readiness` unless the vehicle is known to be another (platform,
    callsign or system id; never the home). Every dispatch path's void of an approved order
    for a spent platform covers them (`Restores::spent_hold`). Replay checks the marks against
    the log carried from (`Debts::spent`, compared from rules 15 only), the fact against the
    ledger with the identity it logs, and flags an order sent to a platform a carried mark holds.
  - *An un-spend* (imu (2) and the review): `Engine::unspend_platform` (an
    `EngagementAuthority`, a note, the world, a step-up): only for a platform spent (this run's
    mark or a carried one) whose engagement ended (its terminal transition; a carried mark counts
    from the run's start if its engagement ended in its log, `SpentPlatform::ended`, else from
    when it was spent, `spent_t`, plus `assess_window_s` if later), heard after that end
    (`VehicleFacts::heard_t`) and not silent now
    (the link-aware fact age limit), the vehicle under the uid the one spent
    (`VehicleIdentity::same_vehicle`: platform, callsign and system id, never the home): one
    judgement, `SpentMark::refusal`, which `Engine::unspend_refusal` gives the bridge's alert.
    In a credentials run it quotes a step-up (password or passkey per `step_up_factor`; a
    passkey's made for it, `StepUpFor::Unspend`, `Engine::unspend_purpose`). Logged as
    `platform.unspent` with the vehicle facts, the identity (`identity`), the step-up and the
    values the mark closed, which are owed back again (this run's as the engagement's, holding
    missions as rules 7 does; a carried one's as carried values, for the vehicle they were
    preloaded on). No `AgentPort` method. Replay checks the session (the operator's own, with
    engagement authority), the step-up (`StepUse::Unspend`), the note, the mark named exactly,
    the judgement on the logged facts and identity (the engagement's end from the log), the
    world's time within 5 s; it lists them (`ReplayReport::unspent`, `UNSPENT:`).
    `Engine::spent_platforms` for the bridge's inbox.
  - *A nomination a failed carry may owe is refused* (imu (1)): an engagement decision's
    `restore_owed` fact names the hold of a pending failed carry's `may_owe` (values unknown,
    the vehicle it was and the one now; another vehicle: not held) and `fallback_preload`
    fails on it, at the nomination and on the plan. A mission's refusal and queue hold never
    read an engagement's fact (`refusal_reason` and `restore_hold` skip engagements).
  - *A failed carry's fleet file* (zq3 (6) and the review): `carried_from`'s `fleet_read` (the
    SHA-256 of `<log stem>.fleet.json` as read and the catalog's armed platforms);
    `replay::fleet_may_owe` computes `may_owe` from such a file. Replay compares both with the
    file when at hand (`ReplayOptions::fleets`, `murmur-replay` by name or beside the log;
    `CarriedRestores::fleet_checked`), and a file at hand that reads where the start recorded
    none is a mismatch; it requires `fleet_read` beside a non-empty `may_owe` unless the entry
    inherits the list the log that named it pending recorded (`Debts::pending_may_owe`, compared
    with that log when at hand), and checks the armed list against every platform fact the
    log's decisions read.
  - *A pointer that can't be read* (zq3 (2), (3), (4) and the review): `run.started` names the
    station's pointer (`pointer`, its `.last-run-<port>` file name). The bridge looks for the
    station's newest log by the carry chain (the logs naming the pointer and those that carried
    from one; the newest is the one nobody carried from), carries from it when it names the
    pointer, its chain (`replay::chain_problem`) and anchors check and no log that names no
    pointer or does not parse could be newer, and no fleet or anchors file of a log that is gone
    is as new (`pointer_failed` on that entry); else records it, and every such log, as failed
    carries under their own names with `pointer_failed`, each holding its fleet's armed
    platforms and what it owed, had spent or held (`refused_holds`; pending: the next start
    reads them); with no log of the station, the pointer as a failed carry as before. Replay
    checks a fallback is the first entry of a run naming its pointer (or the failed entries
    found in its place come first) and that the log given names the same pointer
    (`replay::station_pointer`), takes a refused fallback as the start's refusal, not a dropped
    debt, and requires it to hold what its log owed or had spent when the log is at hand. (The bridge also accepts a pointer of the same
    line twice and syncs the pointer's directory after the rename, warning when only that sync
    fails: no rules change.)
  - *A superseded void names its order* (zq3 (5) and the review): replay reads the order,
    vehicle and form in the engine's and bridge's "superseded" void reasons and checks the
    order is in the log, newer than the voided one, on that vehicle (an amend: of the same kind),
    and in the state the reason says (dispatched, queued, or standing as the engine's dequeue
    reads it).

  Markers (a log claiming rules 14 or older can't carry them): `platform.unspent`,
  `carried_from`'s `spent`, `fleet_read` and `pointer_failed`, `run.started`'s `pointer`, a
  decision's `spent_carried`. The rules 12 tests that depend on rules 12's checks are pinned to
  it. Scenarios 84 (the un-spend) and 85 (the nomination refused); the scenario runner's
  `unspend` step, `write_off` of a `log`, `[[carried]]` failed carries, `expect_unspent`, and
  its vehicles' identities as their preloads name them. Fixtures (a clean build of `3f5f7bb`):
  `tests/data/audit/v15-engagement-scenario-01.jsonl`,
  `v15-spent-munition-unspent-scenario-84.jsonl`,
  `v15-nomination-refused-while-a-carry-may-owe-scenario-85.jsonl`; and four live logs with
  their anchors (stand-ins at the Gate, the fast boat alone): `v15-carry-live-crash-1.jsonl`
  (the switchblade released and spent; no alert and the un-spend refused while its engagement
  observed; the bridge killed after it was assessed), `v15-carry-live-crash-2.jsonl` (the mark
  carried, an orbit refused, op-2's un-spend, an orbit queued for the restore and sent after
  it, spent again, killed inside the assessment window), `v15-carry-live-clean.jsonl` (the new
  mark carried as not ended, an orbit and a nomination refused, the un-spend refused until the
  window is past, a clean stop) and `v15-carry-live-fallback.jsonl` (started on a torn pointer:
  the clean run's log carried in its place, the mark on, the window past and op-2's un-spend
  taken); see `tests/versions.rs`.
- **16** (murmur-533, lost contacts, and its review; every gate reads `LOST_CONTACT_SINCE`, so
  another change taking the number first renumbers it in one line; no policy key changes, schema
  7; a v16 log carries every rules 15 behaviour too): a contact the tracker drops stays
  in every engagement's keep-out as a lost contact until an operator with engagement authority
  accepts it for one engagement or an operator confirms a track carrying its AIS as it
  (`lost.rs`, *Lost contacts* above), counted by a live track's regions and anywhere at its
  domain's top speed (`engage::lost_reach`, region kind `out_of_view`); a live track unseen
  `engage::OUT_OF_VIEW_S` counts the same disc. The disc's speed was first the contact's own
  plausible speed (the review: a prediction, not a bound, and a departure from a track
  without a velocity and a lost vehicle of ours), and the stale track's regions its course
  alone (the review: unseen 170 s it passed what unseen 182 s blocked). New events
  `track.dropped` (the tracker's facts at the drop, never an identification, and its `ais`),
  `lost.claimed` (a live track carrying a lost contact's MMSI: the picture, the evidence, the
  blockers), `track.reacquired` (an operator's confirmation of a claim: `by`, `session`, the
  picture, the evidence) and `lost.reinstated` (a confirmation void: the claimant gone,
  suspect or declared); `engage.accepted` and `engage.acceptance_revoked` may name a `contact`
  instead of a `uid` (with `lost`, the contact as the engine held it, instead of `vehicle`);
  an engagement decision's inputs carry `lost_contacts` and `lost_accepted`, a picture token
  `lost_accepted`; every decision's inputs the AIS identities heard (`track_ais`). Replay
  rebuilds the ledger from the drops, confirmations and reinstatements and checks every
  engagement decision's lost contacts (and accepted ones) against it, every other decision's
  empty; a confirmation its decision's picture voids counts as reinstated there, logged yet or
  not; each acceptance of a contact (rules 16 only, the operator's own session with
  engagement authority, at the nomination or before a first signature, the contact lost since
  exactly that fix, its facts the ledger's); each claim and confirmation, recomputed from its
  own picture (an operator's session, nothing contradicting it), each picture read like a
  decision's; each reinstatement's reason; that no id is dropped twice; that a drop's fix is no
  older than the newest the decisions had seen, the same fix when its time is the same, and not
  newer than one a picture taken at least 1 s after it still showed (`DROP_FIX_SKEW_MS`), nor
  later than when the tracker last saw the track (`last_seen_t`); that a drop comes no sooner
  after the later of the two than the run's tracker drops one (`run.started`'s `track_drop_s`,
  which a v16 log with a drop must carry); that a dropped id is back in no
  picture with another fix than its drop's; and that no track leaves a picture without a
  `track.dropped` (a track that vanished would have stopped protecting with nothing in the log).
  After the review of the merged branch (10-08): a lost contact is judged whether or not its id
  is in the picture (the tracker never reuses an id; before, a track under the dropped id hid
  it); every decision's checks and every dry run of one (an agent's preview, which read no lost
  contacts, the nomination form's options, the console's live checks of a waiting engagement)
  fill the engine's facts with one function (`Engine::fill_engine_facts`); a revocation's
  bystanders are judged with their pictures' lost contacts refilled too; the bridge takes a
  claim's confirmation picture under the decision lock. The keep-out explanation's grid margin
  and words (murmur-dr6 and its review) change no decision. Markers (a log claiming rules 15 or
  older can't carry them): `track.dropped`, `track.reacquired`, `lost.claimed`,
  `lost.reinstated`, `lost_contacts`, `lost_accepted`, `track_ais`, an acceptance's `contact`, a
  lost contact's `ais`, `run.started`'s `track_drop_s`. Scenarios 86 to 94 (84 and 85
  are rules 15's). Fixtures (a clean build of `75c505b`, rules 15 merged, after the second review):
  `tests/data/audit/v16-engagement-scenario-01.jsonl` and scenarios 86 to 88, 90 and 93
  (`v16-lost-contact-*`), and a live log with its anchors (`v16-lost-contacts-live.jsonl`,
  stand-ins at the Gate on `sim/scenarios/golden-gate-lost.toml`: two contacts out of view block
  a nomination before their drop and one after it, both accepted with a third that two signers
  approve, the tour boat back as a claim, the effect released and assessed, then op-1 confirms
  the claim while the spent switchblade's alert stands beside it; see `tests/versions.rs`).
  The measurement of what it costs is in `eval/METRICS.md` ("Out of view").

- **17** (murmur-k8i item 2, the F7-F13 dev check's groundings; every gate reads
  `ROUTE_SURFACE_SINCE`, so another change taking the number first renumbers it in one line; no
  policy key changes, schema 7; a v17 log carries every rules 16 behaviour too): the domain rule
  judges the way a boat, sub or ground vehicle goes, not only its waypoints. Up to rules 16 it
  sampled an orbit's 12 ring points or a patrol's route points and nothing between them, nothing
  of the transit from the vehicle's position or the return home, and nothing of an engagement's
  or an rtl's way; the stand-ins and ArduPilot's AUTO drive straight to waypoint 1, so
  `random-valid`'s orbits west of the Gate ran the Magura ashore at Cavallo Point in 34 to 39 of
  each F7-F13 family's 40 episodes. From rules 17 the engine samples land or water every
  `route::LEG_STEP_M` (20 m, the baked grid's spacing) along each straight leg of each vehicle's
  route (`route::surface_legs`: the transit from where it is, the task path's legs with a loop's
  closing leg, the return from the task's end to home; an engagement's transit to its predicted
  effect point and return; an rtl's way home), in one oracle query beside the waypoints
  (`TerrainOracle::sample_along`), and logs the answers one letter a point per leg
  (`TerrainInputs::Sampled`'s `along`, `world::LegSurfaces`: `w` water deeper than 0, `d` water
  no deeper, `l` land, `o` outside coverage; `near_m` when a source answered some from a sample
  near them, at most `route::LEG_NEAR_M`, 25 m: the bridge's tidewater cache while the vehicle
  moves; replay bounds it). The waypoints are judged first, exactly as before, then the legs in
  travel order; the first point on the wrong surface refuses a mission or a nomination, naming
  the leg, the point and how far along it is (`transit leg (position to effect point) crosses
  land at (-122.46012, 37.83758), 1213 m along it; magura needs water`). A leg from the vehicle
  may leave the wrong surface it starts on, and a leg to home may reach it there, within
  `route::LEG_ENDS_M` (40 m: a pier, a slipway, a grid edge that reads a berth ashore), only
  before the right surface is reached (after it is left); a leg longer than `route::LEG_MAX_M`
  (200 km) can't be judged. A hold or an rtl, the safety commands, are never refused for their
  way (the coordinator, 10-26; DECISIONS.md): an rtl whose way home crosses the wrong surface
  passes with a `warning` on its check (`CheckResult::warning`, the marker; the bridge raises an
  inbox alert `path_crosses_land` from it while the rtl runs), and one whose way can't be worked
  out is `not_evaluated`. A source that can't answer every point leaves the terrain unavailable,
  as for a waypoint (not evaluated). Not covered: a multi-vehicle orbit's other vehicles start
  at other ring points than waypoint 1 (the bridge spreads them by slot; the transit checked is
  to waypoint 1, as the geofence's), an RTL commanded mid-loop, and anything narrower than the
  20 m step. The bridge fetches the points from where the vehicles are just before each decision
  (`route::along_points`) and every second for queued intents and pending engagements, and
  answers a point a moving vehicle shifted from the nearest cached sample within 25 m. Markers
  (a log claiming rules 16 or older can't carry them): `along`, a check's `warning`. The rules 9
  test that runs every scenario under rules 8 skips 95 and 96. Scenarios 95 (an orbit and a
  nomination from Richardson Bay refused for a transit across the Marin shore; `--rules 16`:
  the orbit dispatched and the nomination waiting for its signers) and 96 (an orbit across the
  shore's corner refused, a patrol round the point accepted, a nomination on a clear way
  waiting for its signers); the scenario runner's `task` step takes `patrol` with a `route`.
  Fixtures (a clean build of this branch): `tests/data/audit/v17-engagement-scenario-01.jsonl`,
  `v17-transit-crosses-land-scenario-95.jsonl`, `v17-route-round-the-point-scenario-96.jsonl`.
- **18** (murmur-vni, the owner's decision A of 2026-10-26; every gate reads
  `VELOCITY_SIGMA_SINCE`, so another change taking the number first renumbers it in one line; no
  policy key changes, schema 7; a v18 log carries every rules 17 behaviour too): *a protected
  track is predicted with its own velocity error*. Up to rules 17 the region along a track's
  velocity (and along its average velocity) grew with the policy's `track_velocity_sigma_mps`
  (1 m/s) whatever the tracker said, and a young track's speed bound added `keep_out_sigma` of
  the same: every shown velocity counted as good to 1 m/s. That held only while the tracker
  showed none worse than its gate allowed (3 m/s, so up to three times the error assumed), and
  it kept the gate there: aircraft fixed by ESM positions and 2D radars' bearings, whose honest
  velocity 1-sigma is 5 to 9 m/s, showed no velocity at all (docs/SENSING.md, "Aircraft velocity
  under the physical model"). From rules 18 those regions read the error the tracker reports in
  its least certain direction (`TrackFacts::velocity_sigma_mps`, a logged fact since rules 14),
  or the policy's when that is larger or none is given (`engage::track_velocity_error`: the
  policy's figure is a floor, as in rules 14's speed bound), so a velocity known to 3 m/s widens
  the prediction three times as fast as one known to 1 m/s. The region's detail names the error
  it was predicted with (`velocity 300 m at 3.0 m/s`), and the keep-out explanation says it
  (`its velocity known to 3.0 m/s`); the explanation's scan bounds its regions' growth with it
  too. Every other region is as before: rules 14's aircraft reach, rules 16's out of view and a
  manoeuvre's or a track without a velocity's. A track whose velocity the tracker holds better
  than the policy's 1 m/s, or one with no error given (every scenario before 97), is judged as
  before: the 96 scenarios before it end as they did under rules 17 (the same states, failed
  rules, assessments, fallbacks and refusals; two final details, 46's and 73's, now say `at 1.0
  m/s`). No new marker: replay reads the error from the logged facts, and a rules 18 log
  claiming rules 17 is caught where a decision read an error above the policy's (scenarios 97
  and 98's). Scenarios 97 (a fishing boat whose course passes 700 m from the effect point, fixed
  by a radar only, its velocity known to 3 m/s: rejected, then waiting for its signers once the
  boat is 1.4 km off by the effect; `--rules 17` lets the first through) and 98 (a ferry 850 m
  off, its velocity known to 0.5 m/s from its AIS, signed and dispatched; its AIS quiet, the
  tracker reports 4 m/s and the engagement aborts; `--rules 17` goes ahead); a scenario's track
  takes `velocity_sigma_mps`, in the file and in a `track` step. `tests/rules18.rs`. Fixtures (a
  clean build of this branch): `tests/data/audit/v18-engagement-scenario-01.jsonl`,
  `v18-velocity-known-to-3-mps-scenario-97.jsonl`,
  `v18-velocity-error-grows-under-way-scenario-98.jsonl`.
- **19** (bd murmur-j0x, the owner's decision B of 2026-10-26, option 2 of
  docs/ENGAGEMENT-IN-TRAFFIC.md; every gate reads `ACCEPT_BEHAVING_SINCE`, so another change taking
  the number first renumbers it in one line; no policy key changes, schema 7; a v19 log carries
  every rules 18 behaviour too): *an operator may accept a live track as behaving now*. In traffic
  the keep-out refused nearly every engagement (F15: 1,186 of 1,191 attempts at Golden Gate v3, an
  aircraft among the blockers in 97 %), mostly for aircraft counted anywhere they could turn or
  speed up to (rules 14), and no person could make the call. From rules 19 an operator with
  engagement authority may accept one such live track for one engagement (*Live tracks accepted
  as behaving now* above): the keep-out then counts it along its accepted path and its course
  now, and the acceptance lapses on its own on a departure beyond the prediction's uncertainty or
  out of view. A first build also lapsed one whenever the tracker withheld the track's velocity;
  on the legacy sensing model that ended an acceptance of a steady aircraft every 10 to 20 s in a
  headless episode, and the owner's decision names the two tests alone, so a velocity withheld
  for a moment leaves the position test running. New events and facts (markers: a log claiming
  rules 18 or older can't carry them): `engage.acceptance_lapsed`, an `engage.accepted` naming a
  `track` with `behaving`, an `engage.acceptance_revoked` naming a `track`, decision inputs' and
  picture tokens' `behaving_accepted`. Replay recomputes each acceptance (rules 19, the operator's
  own session with engagement authority, the nomination or before a first signature, the track
  acceptable on its logged facts, its estimate's time the one named, never twice while one is in
  force), each lapse from its logged facts and the acceptance's (the same kind and words),
  every decision's and token's acceptances against its ledger (accepted less revoked, lapsed and
  reset), the self-acceptance of each signature, and flags a held picture (`engage.picture`) that
  reads an acceptance its own picture lapses (the engine logs the lapse first). The keep-out
  explanation says, for each live blocker, what accepting it as behaving now would do
  (`keepout::Item::behaving`, `keepout::BehavingInfo`: acceptable, clears, the accepted path's
  region and why), and `keepout::behaving_options` says it without the scans for a waiting
  nomination's card. The 98 scenarios before it end exactly as under rules 18 (state, failed
  rules, assessment, refusals, fallback and final reason). Scenarios 99 (83's aircraft accepted at
  the nomination, held to the effect; `--rules 18`: the nomination refused), 100 (a jump off the
  accepted path lapses it while no plan is frozen, a token from before refused, accepted again),
  101 (out of view: lapsed, the signature dropped, voided), 102 (revoked under way: aborted), 103
  (reset with the plan), 104 (an operator without engagement authority and an agent refused), 105
  (a turn back under way: lapsed by velocity at the next step, aborted then, nothing released);
  the scenario runner's steps take `accept_behaving` (nominate) and `behaving` (accept, revoke,
  agent_accept), and `expect_lapsed`. `tests/rules19.rs`. Fixtures (a clean build of `502e0da`):
  `tests/data/audit/v19-engagement-scenario-01.jsonl`,
  `v19-aircraft-accepted-as-behaving-scenario-99.jsonl`,
  `v19-behaving-acceptance-lapses-on-a-jump-scenario-100.jsonl`,
  `v19-behaving-acceptance-lapses-under-way-scenario-105.jsonl`.
- **20** (bd murmur-64z, the owner's direction of 2026-10-26, DECISIONS.md "Munitions shown in
  flight and strikes animated"; every gate reads `MUNITION_FLIGHT_SINCE`, so another change taking
  the number first renumbers it in one line; no policy key changes, schema 7; a v20 log carries
  every rules 19 behaviour too): *a standoff munition is released its time of flight early and
  strikes at its impact*. Up to rules 19 the effect applied at the release, which needed the
  platform within `arrival_m` (at most 2,000 m) of the point: an armed MQ-9B circling no nearer
  than about 1.75 km could release only under a policy stretched to the limit, and a missile
  released when the target is at the point lands some 18 s late. From rules 20 a platform whose
  catalog entry has `munition = { name, speed_mps, range_m }` (`catalog::MunitionFacts`, a
  platform fact logged with every decision; first `[mq9b_armed]`'s Hellfire-class missile, 425
  m/s, 8,000 m) launches it at the release along a straight slant line from its reported position
  and altitude to the plan's effect point at the effect's altitude (the target's when the picture
  ranges it, else the sea surface), at a constant speed (`engage::shot`): the time of flight is
  the slant over the speed. The release checks judge the strike, now plus that flight: `arrival`
  is the slant within the munition's range instead of `arrival_m` (its detail names the slant,
  the ground distance and height, the range and the flight); `effect_time` is the strike inside
  the plan's tolerance (`release_window`); the keep-outs at a release before the plan's time are
  judged at the strike; `alignment` is the track predicted at the strike. The picture judges its
  keep-outs and `engage_window` at the soonest the munition could strike (`Shot::lead_ms`: its
  flight from its range when it is farther), so a platform that can't get the munition there
  before `late_s` stops then. `engage.effect` carries the flight (`munition`: name, speed, launch
  time, point and altitude, impact time, point and altitude, time of flight;
  `engage::MunitionFlight`), and so do the engagement's record (`engagement.munition`, the
  console's) and the effect order (the bridge hands the contacts process the impact on its own
  clock, `impact_t`, and it applies the effect then). The assessment observes from the impact on
  (`engage::struck`). A platform without a munition (a loitering munition is its own) decides
  exactly as under rules 19: scenario 01's log is the same event for event, and the 105
  scenarios before it end as they did. Replay recomputes the flight from the releasing picture's
  inputs (`engage::release_shot`; a flight logged before rules 20, or on a platform without one,
  is caught too), bounds the strike, not the release, by the plan's tolerance, and flags an
  observation before the impact. Scenarios 106 (a boat running west at 5 m/s, the MQ-9B at 7,600
  m: released some 33 s before the plan's time, struck 18 s later inside its tolerance, ceased;
  the impact moved 10 s in the log is caught) and 107 (the MQ-9B at 9,000 m, beyond its missile's
  slant range even overhead: never released, aborted late by `engage_window`; `--rules 19`
  releases both over the point); the scenario runner's target leaves the picture at the strike;
  tamper `edit_munition`. `tests/rules20.rs`. Fixtures (a clean build of `c788c83`):
  `tests/data/audit/v20-engagement-scenario-01.jsonl`,
  `v20-standoff-missile-released-early-scenario-106.jsonl` (relabelled rules 19, its flight is
  caught).

- **21** (bd murmur-bia, the owner's direction of 2026-10-27, DECISIONS.md "W1 follow-up: an
  identify order"; every gate reads `IDENTIFY_SINCE`; no policy key changes, schema 7; a v21 log
  carries every rules 20 behaviour too): *a vehicle may be ordered to identify a track*. A new
  intent kind, `identify` (`Task::Identify { track, standoff_m }`, one vehicle), laid out by the
  engine as an orbit around the track's position now (`identify::plan`): its standoff is 70 % of
  the vehicle's EO/IR identification reach for the track's domain as slant range, that reach an
  eighth of the sensor's detection reach (`IDENTIFY_PX_SHARE`, the physical model's pixels for
  detection over identification), never inside the platform's tightest orbit; an operator's
  `standoff_m` replaces it. Its checks are a new rule, `identify` (the track live, not a lost
  contact, not identified yet; an EO/IR that reaches its domain; the plan), then the vehicle
  rules on that orbit (`identify::orbit_view`: readiness, platform limits, the domain rule on its
  route, the geofence, reserves; the terrain and the route read the orbit too). The world gains
  `track_levels` (each track's identity level and the looks that identified it, the tracker's;
  logged with every decision from rules 21, never before), and an identify decision reads its
  lost contacts as an engagement does. It is dispatched as that orbit; `intent.dispatched`
  carries `identify` (the plan, the deadline: the dispatch plus the time to get there plus
  `ON_STATION_S`, and `resumes`: the vehicle's newest order then, of a kind it can go back to,
  under way or done). `Engine::step_identify`, each second, ends the look when the track is
  identified, lost or past the deadline (`identify.step` ended: the outcome, the level, who
  identified it, and the order sent afterwards, the replaced one re-checked on `return_inputs`
  or a hold) or moves the orbit with the track (`recentred`, with the domain and geofence checks
  on it, or `held` once for each reason they fail), each logged before the bridge sends anything
  (`IdentifyEvent`, under the identify's own id). `Engine::finish` completes it once the order
  sent afterwards ended, only if its look identified the track (else it fails), and refuses a
  done report while it looks. An identify order is a vehicle task for its starts logged ahead
  and for the values owed back (`queue::vehicle_task`), but never queued and never carried over.
  Replay recomputes the plan at every check, the dispatch's `identify`, every step from its
  inputs, and refuses a completion without an identified end. Scenarios 108 (a holding boat
  identified; a radar-only MQ-9B refused), 109 (the orbit follows a boat at 10 m/s), 110 (the
  track dropped: failed), 111 (the deadline: failed), 112 (back to the patrol it replaced); the
  runner steps identify orders and takes the order sent afterwards at once. `tests/rules21.rs`.
  Fixtures (a clean build of `31b3a28`): `tests/data/audit/v21-engagement-scenario-01.jsonl`,
  `v21-identify-follows-a-moving-boat-scenario-109.jsonl` (relabelled rules 20, its checks and
  steps are caught), `v21-identify-returns-to-its-patrol-scenario-112.jsonl`.

- **22** (bd murmur-n6a, the owner's order of 2026-10-27 refused at the Gate, DECISIONS.md
  "Route planning before a refusal" and "A second look at an identified contact"; the gates read
  `ROUTE_PLAN_SINCE` and `IDENTIFY_SECOND_LOOK_SINCE`, both 22; no policy key changes, schema 7;
  a v22 log carries every rules 21 behaviour too): *a leg the domain rule or the geofence would
  refuse is routed around what it meets first*, and *any live contact may be sent a look*. Up to
  rules 21 an orbit, patrol or identify order whose straight transit touched the wrong surface or
  a no-go area was refused: HUNTER 1's patrol west of the bridge ran into the south tower's pier
  though the main span is 1.3 km of open water. From rules 22 the transit to the task, and each
  of a patrol's legs (a two-point patrol's way back included, `route::Route::legs`), that a boat's
  or sub's need of water, a UGV's of land, or any vehicle's no-go areas would refuse is planned
  around it first (`plan`): a grid over the leg's box widened by `plan::MARGIN_M` (1.5 km), cells
  of `plan::CELL_M` (40 m; coarser past `plan::MAX_CELLS`, 40,000), each cell answered by the
  same land/water source the domain rule reads (none asked for an aircraft), free on the right
  surface `plan::SURFACE_CLEARANCE_M` (60 m) from the wrong one and `plan::NOGO_CLEARANCE_M`
  (100 m) from a no-go area's edge (waived near the leg's own ends); an A* over the free cells
  (eight neighbours, integer costs, a fixed order: deterministic), pulled straight to at most
  `plan::MAX_WAYPOINTS` (12) waypoints; all [E]. The decision's inputs log each planned leg
  (`route_plans`: the leg, what it met, the surface it keeps to, its waypoints or why there is no
  route, the metres it adds and its grid, run-length coded), the route then reads it
  (`Route::transit_via`, `path_via`), and the routed legs are judged by every rule as any leg is
  (the domain rule's points along each part, the geofence, reserves on the longer way). A passing
  domain or geofence check says what it routed (`...; the transit leg (position to waypoint 1)
  routed around land at (-122.47572, 37.81350): 1 waypoint, +40 m`); a leg with no route within
  the bounds refuses the order saying so (`no route around land at (...) for the transit leg
  (...): no way through water ... within 4.3 x 3.1 km around the leg (the planner's bounds)`).
  `intent.dispatched` carries `via` (each vehicle's transit waypoints) and `path` (a patrol's
  route with its routed waypoints), flown in that order; an identify order's moved orbit and the
  order it goes back to carry the plan their own checks made. The return home, an rtl and an
  engagement's way stay straight: the first two are the autopilot's own way home (rules 17's
  `path_crosses_land` warning stands), the last is a frozen plan's; an order whose straight
  return crosses land is refused for that leg as before (scenarios 95 and 96 now are). Replay
  plans every logged leg again on its own grid and the no-go areas the decision read (the same
  waypoints, or no route for the same reason), refuses a plan under older rules, and compares
  the dispatch's `via` and `path`. Second, the identify rule (rules 21) no longer refuses an
  identified track: any live one may be sent a look, one out of view (`engage::OUT_OF_VIEW_S`)
  is refused. `track_levels` gains `own_t` (when one of our own sensors, not AIS, last
  identified it) and `conflict` (the tracker's identity conflict on it); the dispatch's
  `identify` gains `sent_t` and `before` (the track's kind and level then); and the look ends
  identified only on an identification by our own sensors after it went out, its outcome saying
  whether it agrees with the identity before, differs from it, or contradicts it (the tracker's
  conflict, which raises its review flag as fusion does), or what it was before. Scenarios 113
  (the owner's order: routed through the main span), 114 (a UGV round the water), 115 (an
  aircraft round a no-go box), 116 (no route into an enclosed pool: refused, saying why), 117 (a
  second look at an AIS-identified boat that contradicts its AIS; `--rules 21`: refused); the
  runner's `land` polygons, a vehicle's `home`, `expect_routed` and a track's `level_conflict`.
  `tests/rules.rs` (the geofence's routed legs), `tests/rules21.rs` (the second look).

Policy schemas: **1** up to `977854f`; **2** adds `plan_drift_max_m`, `holding_speed_mps`,
`friendly_growth_mps` (optional) and makes `token_ttl_s` optional (default 15) so that
`seen_max_age_s`, an unreleased build's name for it, is still read; **3** adds `[queue]`
(optional; absent: its defaults); **4** adds `track_velocity_sigma_mps`, `track_accel_mps2`
(a table by domain), `vertical_rate_mps`, `keep_out_vertical_m` and `track_alt_max_age_s`
(optional; rules 6); **5** adds a tier's `step_up` (optional, human tiers only) and
`[approval] step_up_acts` (optional: `declare_hostile`, `accept_unknown`;
`STEP_UP_SCHEMA_SINCE`, rules 8); **6** adds `[identification]` (every key optional, unknown
keys refused: `rule_declaration`, `attribution_threshold`, `nominate_unconfirmed`,
`conflict_confidence`, `untracked_per_km2`, `escort_m`, `escort_speed_mps`, each bounded,
`corroboration`, `corroboration_window_s`)
and `confirm_declaration` in `step_up_acts` (`RULE_ID_SCHEMA_SINCE`, rules 9); **7** adds
`[approval] step_up_factor` (`password`, `passkey`) and `login_factor` (`either`, `password`,
`passkey`), both optional (`PASSKEY_SCHEMA_SINCE`, rules 10).

## Integration contract for the bridge

The full contract is in the crate docs (`src/lib.rs`). In short:

1. At start: `Policy::load("sim/policy.toml")`, `Catalog::load("sim/platforms.toml")`,
   `AreaSet::load_dir("sim/areas")`, the roster (a credentials run: the credential file's
   operators and engagement flags, `Roster::new(..).with_auth(AuthMode::Credentials)`; a dev
   run: `Roster::new(MURMUR_OPERATORS, MURMUR_ENGAGE_OPERATORS)`, refused, and the bridge
   does not start, when an engagement operator is not an operator),
   `Engine::with_areas(policy, Some(areas), roster, clock, JsonlFile at
   logs/audit/<run-id>.jsonl)` (`Engine::new` works too; the set is then logged at first
   use). Sessions open only for operators on the roster, with engagement authority only for
   those on its engagement list. The clock is one that never steps and that the facts are
   stamped on too (murmur-dup): the bridge gives its own (`clock::BridgeClock`: the
   monotonic rate, steered within about 2 s of the wall clock at 5 % at most, never back),
   since every age, expiry and window the
   engine decides on is a difference of its times and the facts', and replay sees only those.
   `SystemClock` reads the wall clock, which steps (this WSL host's back about 0.65 s every
   32 s; NTP or a resumed virtual machine either way). The engine never logs a time earlier
   than the last one, so even a clock that steps back leaves the log in order and replayed
   (`tests/clock.rs` runs every scenario on two such clocks), but the durations it decided on
   would have moved with each step.
2. Per decision call, build a `WorldSnapshot` keyed by `uid`: platform facts from the catalog
   (its sensors' reaches those of the sensing model the run is on: `Catalog::reaching`, the
   bridge's `Decision::facts_catalog`), and timestamped facts from telemetry (autopilot kind,
   nav ready, position, home, battery). Unknown is `None`.
3. Terrain: a `TerrainOracle` over tidewater's `POST /v1/sample` (lat-first points; null
   elevation is outside coverage; `water_depth_m > 0` is water), or `NoTerrain`.
4. Operator endpoints: `submit` (also behind the legacy `POST /api/tasks`), `approve`,
   `reject`, `cancel`, `inbox`, each under the operator's session: a credentials run opens
   one per login (`open_authenticated_session` with `AuthMethod::Password`, after checking
   the password) and closes it at logout, expiry or a credential change
   (`close_operator_session`); a consequential approval (`PolicyDecision::step_up`) checks
   the password again, then `step_up` and `approve_stepped` or
   `approve_engagement_stepped`. Rules 10: log the relying party and the operators' passkeys
   on the roster (`Roster::with_passkeys`), and a study (`with_study`); for a passkey use, ask
   `passkey_challenge` for the act (`sign_purpose` for a signature, `confirm_purpose`), have
   the browser sign it, check it (webauthn-rs in the bridge), then `step_up_passkey` (or
   `open_passkey_session` for a login) with the assertion; log registrations and removals
   (`record_passkey_registered`, `record_passkey_removed`). Agent (MCP) side: an `AgentPort`
   per agent, nothing else.
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
   previous log still owed (`replay::owed_restores`, `record_platform_restore` carried; rules
   12: `replay::owed_debts` and `log_head` of each log, before this run's log is opened, into
   `Engine::start`'s `carried_from`; give each snapshot the fleet's identities,
   `WorldSnapshot::identities`; take an operator's `write_off_restore`; restore nothing the
   ledger no longer owes, `Engine::restore_owed_to`). A
   nomination that accepts lost vehicles goes through `submit_accepting`; one already
   waiting through `accept_unknown_friendly`: both an operator's request with engagement
   authority only; `revoke_unknown_friendly` any operator's. Rules 7: report a vehicle
   task's start with `record_op_state_ahead` before it goes out (and send nothing if that
   fails), never as a `starting` op state afterwards; stop an engagement in play on the
   vehicles of an order the engine queued with `supersede_engagement` in the same step; show a
   queued order's reach with `reach_now` (it holds a mission for a vehicle still owed its own
   lost-link values); try a restore when the vehicle is heard again, not into a dead link.
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
     `satcom:<class>` or `gnss:<class>` (rules 11: merged over the area's sample points), and
     `finish` at the window end.
   - Rules 11: `cancel` works on a dispatched collect or cover too (the engine reports
     `cancelled` itself and aborts it: stop following the run); when the segment is
     reloaded (keep the session's pin), `set_space` it again and re-check every running
     space task but a collect whose pass has come, with `recheck_space` on an answer
     prepared outside the lock (`prepare_space_at` with the task's answer's `now` for a
     collect): `Unchanged` (nothing to do), `Holds` (a cover's watch takes the intent's new
     answer), `Lost` (the engine reported `missed` and failed it: stop following the run,
     alert the operator), `Unknown` (the query failed: the task runs on, alert the
     operator). Never report `cancelled`, `rechecked` or a `missed` that starts with `no
     longer feasible` yourself: the engine refuses them. Never ask the engine for a cover
     without an answer prepared outside the lock (`prepare_space`, checked with
     `prepared_space_fits` under the lock and prepared again if the segment moved): the
     engine computes none and fails the check closed.

The bridge does all of this (`crates/murmur-bridge/src/decision/space.rs`): collects and
covers come in through `POST /api/intents`, and a simulated executor reports their op states
on the scenario clock.

## Not done yet

- Carry-overs, defended only by anchors (`murmur-replay --anchors`), not by replay: a log
  that deletes an op's `starting` and turns its `unconfirmed` into `requeued` reads as a
  link lost before the start; a confirm-only delivery's "does not run it" (`unconfirmed`, a
  human decides) rewritten as a lost link (`requeued`, back to the queue) reads as the
  genuine one. (Rules 6 to 10: a log that removes every carried `platform.restore` names no
  earlier log to check against, so what that log owed goes unchecked; rules 12 record the
  carry in `run.started`, and only removing that record from the first line, which the start
  anchor covers, reads as a first start.)
- No HTTP endpoints, no tidewater client, no inbox UI (C.5): bridge and web work.
- Routes are straight lines between the points the task commits to. An RTL commanded
  mid-loop flies home from wherever the vehicle is, which sweeps a region the return-leg
  check (from waypoint 1) doesn't cover. A leg starting inside a no-go area is allowed to
  leave it; if a concave area makes that straight leg re-enter it, that isn't caught. The
  domain rule checks the legs from rules 17, every 20 m, so a spit or breakwater narrower than
  that can fall between two points, and a multi-vehicle orbit's other vehicles are checked to
  waypoint 1, not the ring point the bridge sends each to first. Areas are 2D (no altitude
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
  no-strike areas, signers' passkeys required only where the policy (or a study) asks
  (rules 10; the shipped policy takes the password or a passkey), no attestation of what
  kind of authenticator holds a passkey. An
  engagement does not survive a bridge restart: the next run puts the platform's own
  lost-link values back (carried over from the last run's log, onto the same
  vehicle only, see the lost-link preload above), but the engagement itself is not resumed.
- Lost contacts (rules 16): the ledger is a run's: a bridge restart starts a new tracker and
  a new log, and the contacts lost before it are not carried (nor is anything else of the
  picture). A contact out of view counts at its domain's top speed (25 m/s on the water): one
  faster than that (a fast boat out of view goes 25 m/s and more) is outside its region unless
  the tracker had seen it go so. Only an operator's confirmation of a claim on its AIS retires
  one; a contact the tracker sees again without AIS stays lost beside its new track. A live
  track out of view (unseen 10 s to 180 s) blocks like a lost one but cannot be accepted:
  only seeing it again, or its drop and then an acceptance, lets a nomination through. With no
  cap, the lost contacts of a run pile up and every nomination needs each accepted
  (`eval/lost_burden.py`, `eval/METRICS.md` "Out of view").
- Satellite tasking: no cloud model (optical options note it), no sensitive-area or
  cost/priority tiers yet (every collect needs a human), no competing demand or agility
  model, one collect per intent. A cover samples the centroid and the boundary (the interior
  of a concave polygon is not sampled). A re-check after dispatch comes only from a reloaded
  segment (the one later answer this system has), and a lost pass is never re-planned by the
  engine: the fresh options go to a human. The segment identity is computed here from public
  `Satellite` fields and propagated states; a content hash of the raw cached files in
  murmur-space would be stronger.
