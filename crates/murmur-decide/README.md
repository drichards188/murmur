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
append-only file: after the first event, every `MURMUR_ANCHOR_EVERY` events (100), and at a
clean shutdown (`logs/anchors/<run-id>.anchors`; `MURMUR_ANCHOR_DIR` moves it,
`MURMUR_ANCHORS=0` turns it off). `MURMUR_ANCHOR_CMD`, a shell command, gets each anchor's
JSON on its stdin (and in `MURMUR_ANCHOR`), run in order on a thread of its own, to post it
elsewhere. `murmur-replay --anchors <file>` checks the log against them: the first line and
every anchored event must hash as anchored, the log must reach the last anchor (a cut tail
shows), the file must be whole (the start anchor on `run.started`, one anchor every N events
as each declares in `every`, a shutdown anchor last, so a removed anchor line shows), and
nothing may follow the shutdown anchor: the bridge closes the log behind it (later appends
fail, so nothing is done that isn't logged) and exits with the decision lock held. A run
that crashed has no shutdown anchor and fails the check: its tail can't be told from a cut
one.

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
    [--space sim/space/constellations.toml sim/space/cache]
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

Policy schemas: **1** up to `977854f`; **2** adds `plan_drift_max_m`, `holding_speed_mps`,
`friendly_growth_mps` (optional) and makes `token_ttl_s` optional (default 15) so that
`seen_max_age_s`, an unreleased build's name for it, is still read.

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
5. Every second: `expire_due`. For approved intents: `dispatch` gives a `DispatchOrder` for
   the existing task manager (execute exactly it); report back with `record_op_state` (a
   dispatched intent's own vehicles only, and before its finish) and `finish`; log vehicle
   events; `void` intents the world invalidates. An engagement's fallback is reported as
   op state `fallback` with a detail that starts with the fallback's name (see "What was
   executed is what was approved" above and `src/lib.rs`).
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
