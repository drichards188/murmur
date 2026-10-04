//! # murmur-decide: Murmur's decision layer (Phase C)
//!
//! Every action, from a human or an AI agent, becomes an [`Intent`]. It passes fail-closed
//! [rules](rules), then a risk-tier [policy](policy) routes it to automatic approval or to
//! a human. Every step goes to an append-only [audit log](audit) with the exact inputs each
//! decision read, so [`replay`] can recompute every decision from the log alone.
//!
//! ```text
//! proposed -> checked -> auto_approved | pending_approval
//!          -> approved | rejected | expired | voided -> dispatched -> done | failed
//! checked -> approved   (a human's own intent in a human tier that lets the submitter authorize)
//! ```
//!
//! The crate is a library with no I/O beyond the audit sink and file loaders, no network,
//! and no dependency on the bridge. Time comes from an injected [`Clock`]; maps are
//! `BTreeMap`s; times are integer milliseconds; floats round-trip exactly through the log
//! (`serde_json` with `float_roundtrip`).
//!
//! ## The authorization guarantee
//!
//! Murmur's central claim is that agents can nominate and explain but never authorize.
//! A human's own submission is a human decision by construction. Here that is a property
//! of the types:
//!
//! - Both ways into `approved` need a `&`[`HumanAuthority`]: [`Engine::approve`] (an inbox
//!   approval), and [`Engine::submit`] (or [`Engine::amend_collect`]) for the submitter's
//!   own intent when its policy tier has `submitter_authorizes` (logged as
//!   `approval.at_submission`, with approval `{by, at_submission: true, self_approved:
//!   true}`; the engine checks the authority is the intent's own human source).
//!   [`next_state`] is the only transition table; the engine performs `Approve` only inside
//!   `approve` and `AuthorizeAtSubmission` only on the human submit path. An agent's
//!   proposal in a human tier always waits in the inbox.
//! - A `HumanAuthority` exists only inside an [`OperatorSession`], and an `OperatorSession`
//!   comes only from [`Engine::open_operator_session`], which logs the opening. Neither type
//!   has public fields, `Clone`, `Default`, `Deserialize`, or a conversion from an
//!   [`AgentId`]. So no JSON an agent sends (an MCP tool call, say) can become one.
//! - Agent code is given an [`AgentPort`]: propose (a [`Rationale`] is a required argument,
//!   and the type has no empty value), withdraw its own intents, read. It has no approve
//!   method and its engine reference is private.
//! - The source of an intent (human or agent) is set by the path it came in on, never by
//!   the request body ([`Proposal`] has no source field).
//!
//! `compile_fail` doctests on [`HumanAuthority`] (module `authority`) and [`AgentPort`]
//! prove the forbidden programs don't compile. The replay also checks the property from
//! the other side: no `approved` state in a log without an `approval.granted` by an
//! operator before the deadline, or an `approval.at_submission` by the intent's own human
//! source in a tier that allows it (never on an agent's intent). A human approving their
//! own proposal in the inbox is allowed where the tier lets the proposer approve (never for an
//! engagement, which also needs an [`EngagementAuthority`] and N distinct signers) and flagged
//! `self_approved` on the approval.
//!
//! **Limits, stated plainly.** Rust privacy is a code-structure guarantee, not a security
//! boundary. It stops agent-facing code that holds only an `AgentPort` (or only JSON) from
//! authorizing, and it makes the one minting point greppable. It does not stop:
//! someone editing this crate; bridge code that holds `&mut Engine` calling
//! `open_operator_session` on an agent's behalf (review rule: the bridge's authenticated
//! operator endpoint is its only caller); a compromised process; or a human who approves
//! without looking. Operator authentication itself is the bridge's job. Whether approvals
//! stay meaningful at scale is what Murmur measures (Phase G); this crate makes sure there
//! is a human decision to measure.
//!
//! ## Integration contract for the bridge
//!
//! The Rust bridge (`crates/murmur-bridge`) will own one [`Engine`] and call it; this crate
//! never calls the bridge.
//!
//! 1. **Start of run.** Load [`Policy::load`]`("sim/policy.toml")`, [`Catalog::load`]`(
//!    "sim/platforms.toml")`, [`AreaSet::load_dir`]`("sim/areas")`, and the operator
//!    [`Roster`] (`MURMUR_OPERATORS`, and `MURMUR_ENGAGE_OPERATORS`, who must be operators
//!    too: [`Roster::new`] refuses otherwise, and the bridge does not start). Create the
//!    engine with [`Engine::with_areas`] (policy, the area set, the roster, [`SystemClock`],
//!    a [`JsonlFile`] at `logs/audit/<run-id>.jsonl`), so `run.started` carries the area set
//!    and its hash and the roster. Pass the same set in every snapshot; a different one is
//!    logged once as `areas.loaded`. Sessions open only for operators on the roster
//!    ([`EngineError::NotOnRoster`]), and replay checks every logged session against it.
//! 2. **World snapshot.** Before each decision call, build a [`WorldSnapshot`] from the
//!    world model, keyed by `uid`: `t` = now; per vehicle, [`PlatformFacts`] from the catalog
//!    (via the fleet manifest's platform id), and [`Fact`]s stamped with the time the
//!    telemetry carrying them arrived: autopilot kind (heartbeat MAV_TYPE), nav ready
//!    (`has_position`), position, home, battery percent. Unknown is `None`, never a default.
//!    `areas` = the loaded set; leave `terrain` as `NotQueried` (the engine fills it).
//! 3. **Terrain.** Implement [`TerrainOracle`] over tidewater's `POST /v1/sample` (note its
//!    `[lat, lon, depth]` point order; `elevation_m: null` is outside coverage;
//!    `water_depth_m > 0` is water), or pass [`NoTerrain`] when tidewater isn't running.
//!    The oracle is called synchronously; from async code, prefetch the points
//!    (`proposal.task.path()`) and answer from a cache.
//! 4. **Operators.** On an authenticated operator request, use (or open once, with
//!    [`Engine::open_operator_session`]) that operator's [`OperatorSession`]. `POST
//!    /api/intents` and the legacy `POST /api/tasks` call [`Engine::submit`] (in a tier
//!    with `submitter_authorizes`, the intent comes back `approved`: dispatch it);
//!    `.../approve` and `.../reject` call [`Engine::approve`] / [`Engine::reject`];
//!    `GET /api/inbox` is [`Engine::inbox`]. [`Intent`] serializes to JSON for the API.
//! 5. **Agents (Phase F).** The MCP server gets an [`AgentPort`] per agent and nothing
//!    else from this crate: [`AgentPort::propose`] (rationale required),
//!    [`AgentPort::preview`] (the checks and policy as a dry run, no intent),
//!    [`AgentPort::withdraw`] (its own intents), [`AgentPort::record_action`] and the
//!    readers. No approve tool exists.
//! 6. **Timer.** About once a second, [`Engine::expire_due`].
//! 7. **Dispatch.** For each intent in `auto_approved` or `approved`, [`Engine::dispatch`]
//!    returns a [`DispatchOrder`] (task, uids) for the existing task manager: execute
//!    exactly that order (replay requires the logged `intent.dispatched` to be the approved
//!    intent's task and vehicles). Report per-vehicle op states with
//!    [`Engine::record_op_state`] and the end with [`Engine::finish`]: op states only for a
//!    dispatched intent, on its own vehicles, and (a vehicle or space task) before its
//!    finish; the engine refuses anything else ([`EngineError::NotItsOrder`]). Vehicle
//!    events (mode change, failsafe, STATUSTEXT) go to [`Engine::record_vehicle_event`]. If
//!    the world invalidates an intent before dispatch (vehicle lost, superseded), call
//!    [`Engine::void`]. An engagement stopped after dispatch hands back a [`FallbackOrder`]:
//!    report it as op state `fallback` on its platform with a detail that starts with the
//!    fallback's name and task ([`Fallback::task`]: `"return commanded (rtl): ..."`, or
//!    `"return could not be commanded: ..."`), and its later progress as `fallback <state>`;
//!    the intercept's as `intercept <state>`. Fly the intercept at the order's
//!    `loiter_radius_m`. After a newer order takes a vehicle, report an older intent's op
//!    there only as it ended (`done`, `failed`, `superseded`). A stop for the platform's own
//!    failsafe or mode change quotes the `vehicle.event` logged for that platform just
//!    before ([`Engine::stop_engagement_system`] with `the platform's failsafe took over
//!    (<text>)` or `the platform left its intercept (mode <text>)`), one for a failed
//!    intercept its report (`the platform's intercept <state>: <detail>`);
//!    [`Engine::supersede_engagement`] takes the order just dispatched to the platform, in
//!    the same step.
//! 8. **Audit failures fail closed.** Every engine call returns `Err` without changing
//!    state if the log write fails; the bridge must not dispatch on an `Err`.
//! 9. **Head hash.** [`Engine::head_hash`] is the hash of the last event written. The log
//!    is hash-chained (see [`audit`]); publishing the head somewhere the bridge can't
//!    rewrite is what would make a full rewrite or a cut tail detectable (future work).
//! 10. **Space segment (collect, cover).** At start, load `murmur_space::Space::load(
//!     "sim/space/constellations.toml", "sim/space/cache")` (run `murmur-space fetch` first;
//!     it is polite to CelesTrak and does nothing when the cache is fresh), wrap it in
//!     [`SpaceSegment::new`]`(Arc::new(space))` (add [`SpaceSegment::with_scenario_offset_ms`]
//!     if the scenario clock runs at an offset from the engine clock), and pass it to
//!     [`Engine::set_space`] (logged as `space.loaded`). Without it, collect and cover
//!     intents fail their `space_query` check (fail closed). See "Satellite tasking" below.
//!
//! ## Satellite tasking: collect and cover
//!
//! Satellites are requested, not flown ([`space`]). [`Task::Collect`] (imagery or SAR over
//! an area in a window) and [`Task::Cover`] (SATCOM coverage or GNSS assurance) target no
//! vehicle: [`Task::targets`] is [`Targets::Space`], their [`Proposal`] has no uids
//! ([`Proposal::space`]), and the vehicle rules don't run for them.
//!
//! 1. **Query.** Propose a collect without `chosen`. Its `feasibility` check fails with the
//!    ranked options in its detail, and the intent's `space` field (and the `space.query`
//!    event) holds the whole answer: `options[]` (rank, satellite, `norad_id`,
//!    `collect_time`, product time, resolution, geometry, notes) and `exclusions[]` with
//!    reasons. It ends `rejected`: a query, not an order.
//! 2. **Choose.** A human ([`Engine::amend_collect`] or [`Engine::submit`]) or an agent
//!    ([`AgentPort::amend_collect`] or [`AgentPort::propose`], with a rationale) proposes the
//!    collect with `chosen = {norad_id, collect_time}` from one option
//!    ([`ChosenCollect::from_option`]). Policy routes it (default: a human approves).
//! 3. **Approve.** [`Engine::approve`] re-runs the query at the approval time; if the pass
//!    is over, the lead time can no longer be met, or the product would miss the deadline,
//!    the intent is voided ([`EngineError::ChecksFailedAtApproval`]).
//! 4. **Dispatch.** [`Engine::dispatch_order`] returns [`Order::Collect`] with a
//!    [`CollectOrder`]: a simulated tasking request (there is no real satellite operator
//!    API). The bridge schedules a "collection" event at `collect_time` and a "product
//!    ready" event at `product_time` (scenario time) and reports them with
//!    [`Engine::record_op_state`]`(id, &order.resource, state, detail)`, where `resource`
//!    is `norad:<id>` and `state` is `tasked` (request sent), `collected` and
//!    `product_ready` (or `missed`); then [`Engine::finish`] with `Done` at product ready,
//!    or `Failed` if the collect was missed. A cover gives [`Order::Cover`]: report service
//!    outages from the answer as op states under `satcom:<class>` or `gnss:<class>`, and
//!    `finish` at the window end. [`Engine::dispatch`] refuses both (they have no
//!    vehicles), so a bridge that only knows vehicle orders never sends an empty-uid task.
//! 5. **Replay.** The rules read only the logged answer, so `murmur-replay` re-decides
//!    every collect and cover from the log. With `--space` it also re-runs each query on the
//!    loaded segment and compares; a different segment is reported as a cache mismatch,
//!    not a decision mismatch ([`replay::replay_str_with_space`]).
//!
//! ## Example
//!
//! ```
//! use murmur_decide::*;
//!
//! let policy = Policy::from_toml(r#"
//!     [[tier]]
//!     name = "safety"
//!     when = { kind = ["hold", "rtl"] }
//!     tier = "auto"
//!     reason = "hold and rtl reduce risk"
//!     [[tier]]
//!     name = "everything-else"
//!     tier = "human"
//!     reason = "demo: a human approves everything else"
//! "#).unwrap();
//! let clock = ManualClock::new(Time::from_secs(1_000));
//! let log = MemorySink::new();
//! // Who may act as an operator (the bridge's MURMUR_OPERATORS), none with engagement authority.
//! let roster = Roster::new(vec![OperatorId::new("op-1")], Vec::new()).unwrap();
//! let mut engine =
//!     Engine::new(policy, roster, Box::new(clock.clone()), Box::new(log.clone())).unwrap();
//!
//! // A one-vehicle world: a copter, ready, with fresh facts.
//! let t = Time::from_secs(1_000);
//! let mut world = WorldSnapshot::new(t);
//! world.areas = Some(AreaSet::empty("none", t));
//! world.vehicles.insert(Uid::new("sky-1"), VehicleFacts {
//!     platform: Some(PlatformFacts {
//!         id: "x10".into(), domain: Domain::Air, armed: false, cruise_mps: Some(12.0),
//!         min_turn_radius_m: None, endurance_h: Some(0.65), autopilot: Some(AutopilotKind::Copter),
//!         sensors: Vec::new(), climb_mps: None, max_mps: None,
//!     }),
//!     autopilot: Some(Fact::new(AutopilotKind::Copter, t)),
//!     nav_ready: Some(Fact::new(true, t)),
//!     position: Some(Fact::new(LonLat::new(-122.45, 37.81), t)),
//!     home: Some(Fact::new(LonLat::new(-122.45, 37.81), t)),
//!     battery_pct: Some(Fact::new(95.0, t)),
//!     nav_source: None,
//!     report_interval_ms: None,
//!     velocity_mps: None,
//!     gps_fix: None,
//!     gps_glitch: None,
//!     gps_fix_from: None,
//!     circling: None,
//!     alt_msl_m: None,
//! });
//!
//! // An agent proposes; policy sends it to the inbox.
//! let orbit = Proposal::new(
//!     Task::Orbit { center: LonLat::new(-122.46, 37.815), radius_m: 150.0 },
//!     vec![Uid::new("sky-1")],
//! );
//! let id = engine
//!     .agent_port(AgentId::new("planner"))
//!     .propose(orbit, Rationale::new("cover the north tower").unwrap(), &world, &NoTerrain)
//!     .unwrap()
//!     .id();
//! assert_eq!(engine.intent(id).unwrap().state(), State::PendingApproval);
//!
//! // Only an operator session can approve it.
//! let session = engine.open_operator_session(OperatorId::new("op-1")).unwrap();
//! engine.approve(id, session.authority(), None, &world, &NoTerrain).unwrap();
//! let order = engine.dispatch(id).unwrap();
//! assert_eq!(order.uids, vec![Uid::new("sky-1")]);
//!
//! // The log reproduces every decision.
//! assert!(replay::replay_str(&log.text()).unwrap().ok());
//! ```

pub mod anchor;
pub mod areas;
pub mod audit;
pub mod authority;
pub mod catalog;
pub mod engage;
pub mod engine;
pub mod geo;
pub mod hash;
pub mod ids;
pub mod intent;
pub mod policy;
pub mod replay;
pub mod route;
pub mod scenario;
pub mod rules;
pub mod space;
pub mod terrain;
pub mod time;
pub mod world;

pub use areas::{Area, AreaKind, AreaSet};
pub use anchor::{Anchor, AnchorConfig, AnchorReport, AnchoredSink, Anchors};
pub use audit::{
    AuditSink, BuildInfo, Event, EventBody, JsonlFile, LoggedAreas, MemorySink, OperatorView,
    ViewShown, parse_log,
};
pub use authority::{EngagementAuthority, Grants, HumanAuthority, OperatorSession, Roster};
pub use catalog::{AutopilotKind, Catalog, Domain, PlatformFacts, SensorFact};
pub use engage::{
    AssessResult, Assessment, EffectOrder, EffectRecord, EngageOrder, EngagePreview, EngageRules,
    EngageSnapshot, EngageWindow, Engagement, EngagementEvent, Fallback, FallbackOrder,
    AcceptedUnknown, EffectAlt, FrozenPlan, LostLinkPreload, ObserveStep, ParamBits, ParamSetting,
    PictureToken, PictureVerdict, PreloadPhase, PreloadRecord, ResetRecord, SignedPicture, StopCause,
    StopRecord, TrackIdentity, VehicleIdentity,
};
pub use engine::{
    AgentPort, DispatchOrder, Engine, EngineError, Order, PREPARED_SPACE_MAX_AGE_MS, PlatformRestoreReport, PreloadReport,
    PreparedSpace, Preview, StepOutcome, TaskOutcome, prepare_space,
};
pub use geo::{LonLat, Polygon};
pub use ids::{AgentId, IntentId, OperatorId, SessionId, Uid};
pub use intent::{
    Constraints, IllegalTransition, Intent, IntentSpec, Proposal, ProposalError, Rationale, Source,
    State, Targets, Task, TaskKind, Transition, next_state,
};
pub use policy::{DeadlineBasis, KindApproval, Policy, PolicyDecision, Tier, Until};
pub use route::{LegKind, Route, Segment};
pub use rules::{
    CheckResult, Outcome, POLICY_SCHEMA, RULES_VERSION, RuleId, RulesConfig, UnknownEndurance,
};
pub use space::{
    ChosenCollect, CollectCapability, CollectOrder, CoverOrder, CoverService, ScenarioMap,
    SegmentId, SpaceAnswer, SpaceArea, SpaceOracle, SpaceOrder, SpaceQuery, SpaceRules,
    SpaceSegment, Window,
};
pub use terrain::{NearSample, NoTerrain, PolygonTerrain, Surface, TerrainOracle};
pub use time::{Clock, ManualClock, SystemClock, Time};
pub use world::{
    Affiliation, Circle, CircleSource, Fact, GpsFixFrom, LoggedInputs, LoggedSpace, SpaceInputs, TerrainInputs, TrackFacts,
    VehicleFacts, WorldSnapshot,
};
