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
  `step_engagements`, every signature is dropped with it (`approval.reset`). An agent's order on a vehicle with an engagement in play goes to a
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
  `superseded`, `fallback: null`). A picture more than `max_picture_gap_s` after the last
  fails `picture_gap`. `step_engagements` returns a `StepOutcome`: the events to act on,
  and each engagement whose step failed, on its own.
  `dispatch` and `finish` refuse engagements; `dispatch_order` returns `Order::Engage`.
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
hash (`Engine::head_hash`, printed by `murmur-replay`), so the fix is to anchor the head
outside the writer's reach (a second machine, signed checkpoints, a transparency log).
That is future work; nothing here signs or anchors anything. Tests in `tests/replay.rs`
show both sides: naive edits caught by the chain, a recomputed chain caught only by the
head.

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
    [--space sim/space/constellations.toml sim/space/cache]
```

Re-runs every `check.result` and `policy.decision` from its logged inputs and the logged
policy (resolving `areas_sha256` to the logged set in force, whose own hash must match its
content), checks every `intent.state` is legal and backed by its evidence, checks `seq`,
`t`, the hash chain and every `self_approved` flag, checks every at-submission approval
(the approver is the submitting human, the tier allowed it, never an agent's intent, and no
intent whose decision let the submitter authorize was sent to the inbox instead), and
prints the approval, self-approval and at-submission counts and the head hash. Logs written
before `submitter_authorizes` existed still replay: the key is additive, and unset in a
logged policy means false (how those runs behaved). Exit 0 if everything reproduces, 1 on mismatch, 2
if the log can't be read, 3 if the decisions reproduce but some space queries ran on a
different space segment (cache mismatch).
Determinism: injected clock, integer-millisecond times, `BTreeMap`s, fixed output order and
fixed-precision detail strings, `serde_json` with `float_roundtrip`, terrain samples keyed at
1e-7 degree. Replay proves decisions follow from the logged inputs; it can't prove the inputs
were true of the world.

## Integration contract for the bridge

The full contract is in the crate docs (`src/lib.rs`). In short:

1. At start: `Policy::load("sim/policy.toml")`, `Catalog::load("sim/platforms.toml")`,
   `AreaSet::load_dir("sim/areas")`, `Engine::with_areas(policy, Some(areas), SystemClock,
   JsonlFile at logs/audit/<run-id>.jsonl)` (`Engine::new` works too; the set is then
   logged at first use).
2. Per decision call, build a `WorldSnapshot` keyed by `uid`: platform facts from the catalog,
   and timestamped facts from telemetry (autopilot kind, nav ready, position, home, battery).
   Unknown is `None`.
3. Terrain: a `TerrainOracle` over tidewater's `POST /v1/sample` (lat-first points; null
   elevation is outside coverage; `water_depth_m > 0` is water), or `NoTerrain`.
4. Operator endpoints: `submit` (also behind the legacy `POST /api/tasks`), `approve`,
   `reject`, `cancel`, `inbox`. Agent (MCP) side: an `AgentPort` per agent, nothing else.
5. Every second: `expire_due`. For approved intents: `dispatch` gives a `DispatchOrder` for
   the existing task manager; report back with `record_op_state` and `finish`; log vehicle
   events; `void` intents the world invalidates.
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
- Engagements: a straight-line intercept (no turn radius or climb), 2D keep-outs, the
  platform's own path checked against no-go areas but not no-strike areas, engagement
  authority on the same placeholder authentication as the operator header.
- Satellite tasking: no cloud model (optical options note it), no sensitive-area or
  cost/priority tiers yet (every collect needs a human), no competing demand or agility
  model, one collect per intent. Cover reads the area's centroid only. The segment identity
  is computed here from public `Satellite` fields and propagated states; a content hash of
  the raw cached files in murmur-space would be stronger.
