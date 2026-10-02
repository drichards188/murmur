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
//! Here that is a property of the types:
//!
//! - [`Engine::approve`] takes a `&`[`HumanAuthority`]. There is no other way to move an
//!   intent to `approved` ([`next_state`] is the only transition table, and the engine
//!   performs `Approve` only inside `approve`).
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
//! operator before the deadline. A human approving their own proposal is allowed (two-person
//! approval is Phase E) and flagged `self_approved` on the approval.
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
//!    "sim/platforms.toml")`, [`AreaSet::load_dir`]`("sim/areas")`. Create the engine with
//!    [`Engine::with_areas`] (policy, the area set, [`SystemClock`], a [`JsonlFile`] at
//!    `logs/audit/<run-id>.jsonl`), so `run.started` carries the area set and its hash.
//!    Pass the same set in every snapshot; a different one is logged once as `areas.loaded`.
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
//!    /api/intents` and the legacy `POST /api/tasks` call [`Engine::submit`];
//!    `.../approve` and `.../reject` call [`Engine::approve`] / [`Engine::reject`];
//!    `GET /api/inbox` is [`Engine::inbox`]. [`Intent`] serializes to JSON for the API.
//! 5. **Agents (Phase F).** The MCP server gets an [`AgentPort`] per agent and nothing
//!    else from this crate. No approve tool exists.
//! 6. **Timer.** About once a second, [`Engine::expire_due`].
//! 7. **Dispatch.** For each intent in `auto_approved` or `approved`, [`Engine::dispatch`]
//!    returns a [`DispatchOrder`] (task, uids) for the existing task manager. Report
//!    per-vehicle op states with [`Engine::record_op_state`] and the end with
//!    [`Engine::finish`]. Vehicle events (mode change, failsafe, STATUSTEXT) go to
//!    [`Engine::record_vehicle_event`]. If the world invalidates an intent before dispatch
//!    (vehicle lost, superseded), call [`Engine::void`].
//! 8. **Audit failures fail closed.** Every engine call returns `Err` without changing
//!    state if the log write fails; the bridge must not dispatch on an `Err`.
//! 9. **Head hash.** [`Engine::head_hash`] is the hash of the last event written. The log
//!    is hash-chained (see [`audit`]); publishing the head somewhere the bridge can't
//!    rewrite is what would make a full rewrite or a cut tail detectable (future work).
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
//! let mut engine = Engine::new(policy, Box::new(clock.clone()), Box::new(log.clone())).unwrap();
//!
//! // A one-vehicle world: a copter, ready, with fresh facts.
//! let t = Time::from_secs(1_000);
//! let mut world = WorldSnapshot::new(t);
//! world.areas = Some(AreaSet::empty("none", t));
//! world.vehicles.insert(Uid::new("sky-1"), VehicleFacts {
//!     platform: Some(PlatformFacts {
//!         id: "x10".into(), domain: Domain::Air, armed: false, cruise_mps: Some(12.0),
//!         min_turn_radius_m: None, endurance_h: Some(0.65), autopilot: Some(AutopilotKind::Copter),
//!     }),
//!     autopilot: Some(Fact::new(AutopilotKind::Copter, t)),
//!     nav_ready: Some(Fact::new(true, t)),
//!     position: Some(Fact::new(LonLat::new(-122.45, 37.81), t)),
//!     home: Some(Fact::new(LonLat::new(-122.45, 37.81), t)),
//!     battery_pct: Some(Fact::new(95.0, t)),
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

pub mod areas;
pub mod audit;
pub mod authority;
pub mod catalog;
pub mod engine;
pub mod geo;
pub mod hash;
pub mod ids;
pub mod intent;
pub mod policy;
pub mod replay;
pub mod route;
pub mod rules;
pub mod terrain;
pub mod time;
pub mod world;

pub use areas::{Area, AreaKind, AreaSet};
pub use audit::{AuditSink, Event, EventBody, JsonlFile, LoggedAreas, MemorySink, parse_log};
pub use authority::{HumanAuthority, OperatorSession};
pub use catalog::{AutopilotKind, Catalog, Domain, PlatformFacts};
pub use engine::{AgentPort, DispatchOrder, Engine, EngineError, TaskOutcome};
pub use geo::{LonLat, Polygon};
pub use ids::{AgentId, IntentId, OperatorId, SessionId, Uid};
pub use intent::{
    Constraints, IllegalTransition, Intent, IntentSpec, Proposal, ProposalError, Rationale, Source,
    State, Task, TaskKind, Transition, next_state,
};
pub use policy::{Policy, PolicyDecision, Tier};
pub use route::{LegKind, Route, Segment};
pub use rules::{CheckResult, Outcome, RuleId, RulesConfig, UnknownEndurance};
pub use terrain::{NoTerrain, PolygonTerrain, Surface, TerrainOracle};
pub use time::{Clock, ManualClock, SystemClock, Time};
pub use world::{Fact, LoggedInputs, TerrainInputs, VehicleFacts, WorldSnapshot};
