//! The decision layer in the bridge (Phase C): every action, from the console, the API or
//! (Phase F) an agent, goes intent -> checks -> policy -> (auto | human) -> dispatch.
//!
//! **Who the human is.** A human tier needs a human decision. An operator's own request is
//! one: when the tier has `submitter_authorizes = true` (the default in `sim/policy.toml`),
//! the engine approves it at submission (`approval.at_submission`) and it is dispatched at
//! once, with [`Outcome::authorization`] saying which tier the operator authorized. With
//! `submitter_authorizes = false` it waits in the inbox (HTTP 202). An agent's proposal in a
//! human tier always waits in the inbox for an operator.
//!
//! The bridge owns one [`murmur_decide::Engine`] (policy `sim/policy.toml`, catalog
//! `sim/platforms.toml`, areas `sim/areas/`, audit log `logs/audit/<run-id>.jsonl`, the real
//! clock) behind one lock. This module is the only code that holds it.
//!
//! **One path to the vehicles.** [`crate::tasks::TaskManager::dispatch`] takes only an
//! [`AuthorizedOrder`]. Its fields are private to this module and it has no public
//! constructor, so the only way to make one is [`Decision`]'s dispatch step, which first
//! gets a `DispatchOrder` from `Engine::dispatch` (legal only for an intent in
//! `auto_approved` or `approved`, and logged as `intent.dispatched`) and builds the order
//! from it. (A test-only constructor exists under `#[cfg(test)]` for the task layer's own
//! tests.)
//!
//! **Operators** (rules 8; `crate::auth`). Every request acts for an [`Actor`]. In a
//! credentials run (the default) that is an operator who logged in with their own password:
//! each login opens one engine session ([`Decision::open_login`], `session.opened` with
//! `auth: password`), its end (logout, expiry, a credential removed or changed) closes it
//! ([`Decision::close_login`], `session.closed`), and a consequential signature (every
//! engagement signature, any tier with `step_up`) re-checks the password first
//! ([`crate::auth::StepUpChecked`], then `Engine::step_up`). The roster is the credential
//! file's operators when the run starts. In a dev run (`MURMUR_AUTH=dev`, logged as such)
//! requests name their operator in the `X-Murmur-Operator` header against
//! `MURMUR_OPERATORS` and `MURMUR_ENGAGE_OPERATORS` (who must be operators too, or the bridge
//! does not start), each operator gets one session opened on first use, and the legacy
//! `POST /api/tasks` and track declarations without a header act as the first listed
//! operator. Only `crate::auth` and this module open engine sessions. The agent path (Phase
//! F's MCP server, `crate::mcp`) is [`agent`]: it gets an `AgentPort` per agent, never a
//! session.
//!
//! **The loop** ([`Decision::run`], once a second): expire overdue approvals, write vehicle
//! events (mode changes, failsafes, STATUSTEXT) to the audit log, void waiting intents the
//! world invalidated, report each dispatched intent's per-vehicle op states and finish it
//! when every op has ended, then push the inbox to WebSocket clients if it changed.
//!
//! **Engagements** (Phase E, [`engage`]). `task = "engage"` nominates an abstract effect on
//! a declared-hostile track with one armed platform. The engine checks it, holds it for two
//! operators with engagement authority (`MURMUR_ENGAGE_OPERATORS`; never its proposer),
//! stores the picture they approved, and every second the loop lets it hold that picture,
//! release the effect or stop it with its recorded fallback; `POST
//! /api/intents/{id}/abort` lets any operator stop it before the effect.
//!
//! **The queue** (rules 5, [`queue`]). An approved order for a vehicle the link can't carry
//! it to now is held, not sent into a dead link; each tick it is offered again on the world
//! then, re-checked before it goes, and it survives a bridge restart.
//!
//! **Voiding.** A waiting intent (pending or approved, not yet dispatched) is voided when a
//! target vehicle goes silent for longer than its freshness limit, `rules.max_fact_age_s` or
//! `rules.fact_age_intervals` expected report intervals of its link if larger (60 s for an
//! acoustic AUV reporting every 20 s; its facts could no
//! longer support the decision), or when another intent is dispatched to one of its vehicles
//! (superseded: approving it later would silently undo the newer order).

pub mod agent;
mod alerts;
mod civil;
pub mod engage;
mod failsafe;
mod preload;
pub mod queue;
pub mod snapshot;
pub mod space;
pub(crate) mod terrain;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use murmur_decide::{
    AnchorConfig, Anchors, AreaSet, AuditSink, AuthMethod, AuthMode, Catalog, Clock, Constraints, StepUpAct,
    Engine, EngineError, Intent, IntentId, JsonlFile, LonLat, OperatorId, OperatorSession,
    OperatorView, Order, Policy, Proposal, Rationale, ScenarioMap, SessionId, SpaceSegment, State,
    Task, TaskOutcome, Time, Uid as DUid, rules,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::watch;

use self::snapshot::{bridge_uid, duid};
pub use self::terrain::Terrain;
use crate::clock;
use crate::tasks::{
    OpState, OrderError, TaskKind, TaskOrder, TaskParams, TaskRequest, Tasking, TaskingSnapshot,
};
use crate::tracks::Tracks;
use crate::vehicle::{Uid, VehicleCell};
use crate::world::World;

/// An orbit request without a radius gets this (as the task layer always flew it).
pub const DEFAULT_ORBIT_RADIUS_M: f64 = 200.0;
/// Collect and cover never reach the vehicle task layer.
const SPACE_NOT_VEHICLES: &str = "collect and cover request satellites: they run in the space executor, not the vehicle task layer";
/// The WebSocket inbox message lists this many recent intents.
const PUSH_INTENTS: usize = 50;

/// Who a request acts for: an operator and, in a credentials run, the login session it came
/// in on (`crate::auth` makes those, after checking the password; the session must still be
/// open). An operator named by the request itself ([`Actor::asserted`]) acts only in a dev
/// run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Actor {
    operator: OperatorId,
    login: Option<SessionId>,
}

impl Actor {
    /// An operator the request names (dev mode, tests); refused in a credentials run.
    pub fn asserted(operator: OperatorId) -> Actor {
        Actor {
            operator,
            login: None,
        }
    }

    /// An operator on their login session (`crate::auth` only, for a session
    /// [`Decision::open_login`] opened).
    pub(crate) fn logged_in(operator: OperatorId, session: SessionId) -> Actor {
        Actor {
            operator,
            login: Some(session),
        }
    }

    pub fn operator(&self) -> &OperatorId {
        &self.operator
    }

    /// The login session (credentials run), if any.
    pub fn login(&self) -> Option<SessionId> {
        self.login
    }
}

impl std::fmt::Display for Actor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.operator.fmt(f)
    }
}

/// An order the engine released for an approved (or auto-approved) intent: the only thing
/// [`crate::tasks::TaskManager::dispatch`] accepts. Made only in this module, from the
/// `DispatchOrder` that `Engine::dispatch` returns, or (engagements, [`engage`]) from the
/// engine's `EngageOrder` and `FallbackOrder`.
pub struct AuthorizedOrder {
    intent: IntentId,
    order: TaskOrder,
}

impl AuthorizedOrder {
    /// For the task layer: the intent's number and the order to run.
    pub(crate) fn into_parts(self) -> (u64, TaskOrder) {
        (self.intent.0, self.order)
    }

    /// Task-layer tests only (no engine in front); intent 0.
    #[cfg(test)]
    pub(crate) fn unchecked(order: TaskOrder) -> AuthorizedOrder {
        AuthorizedOrder {
            intent: IntentId(0),
            order,
        }
    }
}

/// `POST /api/intents`: the console's task request plus the intent fields.
#[derive(Debug, Clone, Deserialize)]
pub struct IntentRequest {
    pub task: String,
    /// Vehicle tasks: the vehicles. Collect and cover: none.
    #[serde(default)]
    pub uids: Vec<Uid>,
    #[serde(flatten)]
    pub params: TaskParams,
    /// Collect and cover: area, window, capability or service, ... (see [`space`]).
    #[serde(flatten)]
    pub space: space::SpaceParams,
    /// Engage: track, effect radius, window, fallback (see [`engage`]).
    #[serde(flatten)]
    pub engage: engage::EngageParams,
    #[serde(default)]
    pub constraints: Constraints,
    #[serde(default)]
    pub rationale: Option<String>,
}

impl From<TaskRequest> for IntentRequest {
    fn from(r: TaskRequest) -> Self {
        IntentRequest {
            task: r.task,
            uids: r.uids,
            params: r.params,
            space: space::SpaceParams::default(),
            engage: engage::EngageParams::default(),
            constraints: Constraints::default(),
            rationale: None,
        }
    }
}

/// An API error: HTTP status, a sentence for the operator, and the intent if one exists.
#[derive(Debug, Clone)]
pub struct DecisionError {
    pub status: u16,
    pub detail: String,
    pub intent: Option<Value>,
}

impl DecisionError {
    pub(crate) fn new(status: u16, detail: impl Into<String>) -> Self {
        DecisionError {
            status,
            detail: detail.into(),
            intent: None,
        }
    }
}

fn engine_err(e: EngineError) -> DecisionError {
    let status = match &e {
        EngineError::UnknownIntent(_) => 404,
        EngineError::Proposal(_) => 400,
        EngineError::NotOwner { .. } => 403,
        EngineError::Illegal { .. }
        | EngineError::Expired(_)
        | EngineError::ViewNotPending { .. }
        | EngineError::ChecksFailedAtApproval(_)
        | EngineError::SpaceIntent(_)
        | EngineError::NotDispatchable { .. }
        | EngineError::NotACollect(_)
        | EngineError::EngageIntent(_)
        | EngineError::NotAnEngagement(_)
        | EngineError::AlreadySigned { .. }
        | EngineError::EngageLifecycle { .. }
        | EngineError::StalePicture { .. }
        | EngineError::PlanChanged { .. }
        | EngineError::NoPicture { .. }
        | EngineError::SupersedesEngagement { .. }
        | EngineError::NotAccepted { .. } => 409,
        EngineError::EngagementNeedsAuthority(_)
        | EngineError::ProposerCannotApprove { .. }
        | EngineError::NotOnRoster { .. } => 403,
        // rules 8: the signer must (re-)enter their credential
        EngineError::NotAuthenticated { .. } | EngineError::StepUpRequired { .. } => 401,
        _ => 500,
    };
    DecisionError::new(status, e.to_string())
}

/// What a decision call produced.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub id: IntentId,
    pub state: State,
    pub intent: Value,
    /// Set once the intent was dispatched.
    pub tasking: Option<TaskingSnapshot>,
    /// The submitting operator's request was itself the human decision a human tier asks
    /// for: `{by, at_submission, tier, reason, why}` (see [`authorization_json`]).
    pub authorization: Option<Value>,
    /// Collect and cover, once dispatched: the simulated run (see [`space::SpaceRun::json`]).
    pub space: Option<Value>,
}

impl Outcome {
    /// Handed on: to the vehicle task layer, or to the space executor.
    pub fn dispatched(&self) -> bool {
        self.tasking.is_some() || self.space.is_some()
    }
}

/// Where things are, from the environment (see `main.rs`).
pub struct Settings {
    pub policy: PathBuf,
    pub catalog: PathBuf,
    pub areas: PathBuf,
    pub audit: PathBuf,
    /// The run's operators: the credential file's (credentials run, set by `crate::auth`), or
    /// `MURMUR_OPERATORS` (dev run).
    pub operators: Vec<OperatorId>,
    /// Those whose sessions carry engagement authority: the credential file's flag, or
    /// `MURMUR_ENGAGE_OPERATORS` (dev run).
    pub engage_operators: Vec<OperatorId>,
    /// How operators are authenticated (`MURMUR_AUTH`: credentials unless `dev`).
    pub auth: AuthMode,
    pub tidewater: Option<String>,
    /// Run-time anchors of the audit log (`logs/anchors/<run-id>.anchors`, or
    /// `MURMUR_ANCHOR_DIR`; every `MURMUR_ANCHOR_EVERY` events, 100; `MURMUR_ANCHOR_CMD` gets
    /// each one). `MURMUR_ANCHORS=0` turns them off.
    pub anchors: Option<AnchorConfig>,
    /// Rules 6: the file naming this station's last audit log (`logs/audit/.last-run-<http
    /// port>`, kept across clean stops too): lost-link values it still owed back are carried
    /// over at start.
    pub last_run: Option<PathBuf>,
    /// Where the previous run's open intents come from (`MURMUR_RESUME`, see [`queue`]).
    pub resume: queue::ResumeFrom,
    /// The pointer to this run's log and the intents it leaves open
    /// (`logs/audit/.resume-<MURMUR_HTTP_PORT>.json`; none with `MURMUR_RESUME=0`).
    pub pointer: Option<PathBuf>,
}

impl Settings {
    pub fn from_env(root: &Path, catalog: &Path) -> Settings {
        let var = |k: &str| std::env::var(k).ok().filter(|s| !s.trim().is_empty());
        let run = var("MURMUR_RUN_ID").unwrap_or_else(|| {
            let id = civil::compact_utc(clock::wall() as i64);
            format!("{id}-{}", std::process::id())
        });
        let operators: Vec<OperatorId> = var("MURMUR_OPERATORS")
            .unwrap_or_else(|| "op-1".into())
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(OperatorId::new)
            .collect();
        let engage_operators: Vec<OperatorId> = var("MURMUR_ENGAGE_OPERATORS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(OperatorId::new)
            .collect();
        Settings {
            policy: var("MURMUR_POLICY")
                .map_or_else(|| root.join("sim/policy.toml"), PathBuf::from),
            catalog: catalog.to_path_buf(),
            areas: var("MURMUR_AREAS").map_or_else(|| root.join("sim/areas"), PathBuf::from),
            audit: root.join("logs/audit").join(format!("{run}.jsonl")),
            operators,
            engage_operators,
            // `main` sets it, and in a credentials run the roster, from `crate::auth::Settings`
            // (a dev run unless MURMUR_AUTH=credentials)
            auth: AuthMode::Dev,
            tidewater: var("MURMUR_TIDEWATER_URL"),
            last_run: Some(root.join("logs/audit").join(format!(
                ".last-run-{}",
                var("MURMUR_HTTP_PORT").and_then(|p| p.parse::<u16>().ok()).unwrap_or(8765)
            ))),
            anchors: (var("MURMUR_ANCHORS").as_deref() != Some("0")).then(|| AnchorConfig {
                path: var("MURMUR_ANCHOR_DIR")
                    .map_or_else(|| root.join("logs/anchors"), PathBuf::from)
                    .join(format!("{run}.anchors")),
                every: var("MURMUR_ANCHOR_EVERY")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(100),
                cmd: var("MURMUR_ANCHOR_CMD"),
            }),
            resume: match var("MURMUR_RESUME").as_deref() {
                Some("0") => queue::ResumeFrom::Off,
                Some("1") | None => queue::ResumeFrom::Pointer {
                    window_s: var("MURMUR_RESUME_WINDOW_S")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(queue::RESUME_WINDOW_S),
                },
                Some(path) => queue::ResumeFrom::Log(PathBuf::from(path)),
            },
            pointer: (var("MURMUR_RESUME").as_deref() != Some("0")).then(|| {
                let port = var("MURMUR_HTTP_PORT").unwrap_or_else(|| "8765".into());
                root.join("logs/audit").join(format!(".resume-{}.json", port.trim()))
            }),
        }
    }
}

/// A clock both the engine and the snapshot builder read.
#[derive(Clone)]
struct SharedClock(Arc<dyn Clock + Send + Sync>);

impl Clock for SharedClock {
    fn now(&self) -> Time {
        self.0.now()
    }
}

/// A dispatched intent the loop follows until its ops end.
struct Running {
    tasking: Arc<Tasking>,
    /// op id -> (state, not moving) last written to the log
    reported: HashMap<u64, (OpState, bool)>,
}

struct Core {
    engine: Engine,
    /// Dev run: one session per asserted operator, opened on first use.
    sessions: BTreeMap<OperatorId, OperatorSession>,
    /// Credentials run: one session per login, by its engine session id.
    logins: BTreeMap<SessionId, OperatorSession>,
    running: BTreeMap<IntentId, Running>,
    /// every tasking an intent produced
    taskings: BTreeMap<IntentId, Arc<Tasking>>,
    /// dispatched collects and covers (running, and the last finished ones)
    space_runs: BTreeMap<IntentId, space::SpaceRun>,
    /// dispatched engagements: the platform's intercept, then any fallback
    engagements: BTreeMap<IntentId, engage::EngageRun>,
    /// rules 6: per platform, its one lost-link parameter job and its own values owed back
    /// (outlives the engagements' runs)
    params: BTreeMap<Uid, engage::ParamLane>,
    /// rules 6: why the start could not carry over what the last run owed (and when)
    carry_problem: Option<(String, f64)>,
    /// `operator.view` budget per operator: (tokens, monotonic seconds when last filled)
    view_budget: BTreeMap<OperatorId, (f64, f64)>,
    /// resumed intents a vehicle had started, waiting for their vehicles to connect
    follow: BTreeMap<IntentId, queue::PendingFollow>,
    /// the open intents last written to the pointer file, and when
    pointer_written: Option<(Vec<u64>, f64)>,
    /// the fleet identity last written beside the log, and when it was last looked at
    fleet_written: Option<queue::FleetIdentity>,
    fleet_looked: Option<f64>,
}

/// `operator.view` events an operator may log: a burst of this many, refilled at
/// [`VIEW_RATE_PER_S`] (a console posts a handful per card; more is not instrumentation).
const VIEW_BURST: f64 = 40.0;
const VIEW_RATE_PER_S: f64 = 10.0;

pub struct Decision {
    world: Arc<World>,
    clock: SharedClock,
    catalog: Catalog,
    areas: AreaSet,
    operators: Vec<OperatorId>,
    engage_operators: Vec<OperatorId>,
    /// How operators are authenticated (logged in `run.started`).
    auth: AuthMode,
    terrain: Terrain,
    rules: murmur_decide::RulesConfig,
    /// The track store (tests inject one; the bridge uses the process-wide one).
    tracks: Option<Arc<Tracks>>,
    audit_path: PathBuf,
    /// The audit log's run-time anchors, when on (set once by [`Decision::load`]).
    anchors: std::sync::OnceLock<Anchors>,
    core: Mutex<Core>,
    updates: watch::Sender<Arc<String>>,
    /// Vehicle events the loop logged, for agents (`agent::RecentEvents`).
    recent: agent::RecentEvents,
    /// The space segment the engine has, shared: collect and cover queries are answered
    /// with it before the decision lock is taken ([`Decision::prepare_space`]).
    space: std::sync::RwLock<Option<Arc<SpaceSegment>>>,
    /// `[queue]` of the policy (rules 5).
    queue_cfg: murmur_decide::QueueConfig,
    /// The pointer file this run keeps ([`queue`]), if any (set once by [`Decision::load`]).
    pointer: std::sync::OnceLock<PathBuf>,
    /// The fleet identity file beside the audit log (set once by [`Decision::load`]).
    fleet_file: std::sync::OnceLock<PathBuf>,
    /// Itself, for the task layer's write-ahead of a vehicle start (rules 7: the op logs it
    /// through the decision lock before sending it).
    me: std::sync::Weak<Decision>,
}

/// Everything [`Decision::from_parts`] needs (tests pass a memory sink and a manual clock).
pub struct Parts {
    pub policy: Policy,
    pub catalog: Catalog,
    pub areas: AreaSet,
    pub sink: Box<dyn AuditSink + Send>,
    pub audit_path: PathBuf,
    pub clock: Arc<dyn Clock + Send + Sync>,
    pub terrain: Terrain,
    pub operators: Vec<OperatorId>,
    pub engage_operators: Vec<OperatorId>,
    pub auth: AuthMode,
    pub tracks: Option<Arc<Tracks>>,
}

impl Decision {
    /// Load policy, catalog and areas, open the audit log and start the engine. Any failure
    /// stops the bridge: without the decision layer nothing may reach a vehicle.
    pub fn load(world: Arc<World>, s: &Settings) -> Result<Arc<Decision>, String> {
        let policy = Policy::load(&s.policy).map_err(|e| e.to_string())?;
        let catalog = Catalog::load(&s.catalog).map_err(|e| e.to_string())?;
        let clock: Arc<dyn Clock + Send + Sync> = Arc::new(murmur_decide::SystemClock);
        let areas = AreaSet::load_dir(&s.areas, clock.now()).map_err(|e| e.to_string())?;
        if let Some(dir) = s.audit.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        if s.operators.is_empty() {
            return Err(match s.auth {
                AuthMode::Dev => "MURMUR_OPERATORS lists no operator".into(),
                AuthMode::Credentials => "the credential file holds no operator".into(),
            });
        }
        let file = JsonlFile::open(&s.audit).map_err(|e| format!("{}: {e}", s.audit.display()))?;
        // anchored from its first event on: an anchor file that can't be opened stops the
        // bridge like an audit log that can't
        let anchors = match &s.anchors {
            Some(cfg) => Some(
                Anchors::open(cfg, &s.audit)
                    .map_err(|e| format!("anchors {}: {e}", cfg.path.display()))?,
            ),
            None => None,
        };
        let sink: Box<dyn AuditSink + Send> = match &anchors {
            Some(a) => Box::new(a.sink(file)),
            None => Box::new(file),
        };
        let d = Decision::from_parts(
            world,
            Parts {
                policy,
                catalog,
                areas,
                sink,
                audit_path: s.audit.clone(),
                clock,
                terrain: Terrain::new(s.tidewater.as_deref()),
                operators: s.operators.clone(),
                engage_operators: s.engage_operators.clone(),
                auth: s.auth,
                tracks: None,
            },
        )?;
        if let Some(a) = anchors {
            let _ = d.anchors.set(a);
        }
        if let Some(p) = &s.pointer {
            let _ = d.pointer.set(p.clone());
        }
        let _ = d.fleet_file.set(queue::fleet_file(&s.audit));
        // the previous run's open intents (queued, approved, under way), carried over
        if let Some(prev) = queue::previous_log(&s.resume, s.pointer.as_deref(), &s.audit) {
            let name = prev
                .file_name()
                .map_or_else(|| prev.display().to_string(), |n| n.to_string_lossy().into_owned());
            // its anchors, where this run keeps its own (`<dir>/<log stem>.anchors`), and the
            // fleet it had (`<log>.fleet.json`): without either, nothing is carried unless
            // MURMUR_RESUME_UNVERIFIED=1 says to go without
            let anchors = s.anchors.as_ref().and_then(|cfg| {
                let stem = prev.file_stem()?.to_string_lossy().into_owned();
                let file = cfg.path.parent()?.join(format!("{stem}.anchors"));
                std::fs::read_to_string(file).ok()
            });
            let fleet = std::fs::read(queue::fleet_file(&prev))
                .ok()
                .and_then(|b| serde_json::from_slice::<queue::FleetIdentity>(&b).ok());
            let unverified = std::env::var("MURMUR_RESUME_UNVERIFIED").as_deref() == Ok("1");
            let missing: Vec<&str> = [
                anchors.is_none().then_some("its anchor file"),
                fleet.is_none().then_some("its fleet identity"),
            ]
            .into_iter()
            .flatten()
            .collect();
            let check = queue::CarryCheck { anchors, fleet };
            let run = if missing.is_empty() || unverified {
                if !missing.is_empty() {
                    eprintln!(
                        "decision: {name}: carried over without {} (MURMUR_RESUME_UNVERIFIED=1)",
                        missing.join(" or ")
                    );
                }
                std::fs::read_to_string(&prev)
                    .map_err(|e| format!("{}: {e}", prev.display()))
                    .and_then(|text| d.carry_over(&name, &text, &check))
            } else {
                Err(format!(
                    "{name}: {} can't be read, so it can't be checked: nothing carried over (MURMUR_RESUME_UNVERIFIED=1 carries it over unchecked)",
                    missing.join(" and ")
                ))
            };
            match run {
                Ok(ids) if ids.is_empty() => {}
                Ok(ids) => println!(
                    "decision: carried {} open intent(s) over from {name}: {}",
                    ids.len(),
                    ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
                ),
                Err(e) => eprintln!("decision: nothing carried over: {e}"),
            }
        }
        // rules 6: lost-link values the last run on this port still owed back
        if let Some(pointer) = &s.last_run {
            d.carry_restores(pointer);
        }
        Ok(d)
    }

    /// The anchor file, when the log is anchored.
    pub fn anchor_path(&self) -> Option<PathBuf> {
        self.anchors.get().map(Anchors::path)
    }

    /// A clean shutdown: under the decision lock (no event being written, none after),
    /// anchor the log's head and close it behind the anchor, let the anchor command finish,
    /// and exit with the lock still held.
    pub fn shutdown_and_exit(&self) -> ! {
        let mut core = self.lock();
        // rules 8: every login's session ends with the run, on the record
        let logins = std::mem::take(&mut core.logins);
        for (_, s) in logins {
            if let Err(e) = core.engine.close_operator_session(s, "bridge stopped") {
                eprintln!("decision: closing a session at shutdown: {e}");
            }
        }
        if let Some(a) = self.anchors.get() {
            match a.close() {
                Ok(_) => a.flush_hook(),
                Err(e) => eprintln!("anchor {}: {e}", a.path().display()),
            }
        }
        // a clean stop leaves nothing to carry over: the next run starts afresh (only a
        // crash, or `MURMUR_RESUME=<log>`, carries open intents into a new run)
        if let Some(p) = self.pointer.get() {
            queue::drop_pointer(p, &self.audit_path);
        }
        std::process::exit(0)
    }

    pub fn from_parts(world: Arc<World>, p: Parts) -> Result<Arc<Decision>, String> {
        let clock = SharedClock(p.clock);
        let rules = p.policy.rules.clone();
        let queue_cfg = p.policy.queue();
        // the operators this run admits, logged in run.started: every engagement operator
        // must be an operator too, or the bridge does not start
        let roster = murmur_decide::Roster::new(p.operators.clone(), p.engage_operators.clone())
            .map_err(|e| match p.auth {
                AuthMode::Dev => format!(
                    "{e} (MURMUR_OPERATORS: {:?}, MURMUR_ENGAGE_OPERATORS: {:?})",
                    p.operators, p.engage_operators
                ),
                AuthMode::Credentials => format!("{e} (the credential file's operators)"),
            })?
            .with_auth(p.auth);
        let engine = Engine::with_areas(
            p.policy,
            Some(p.areas.clone()),
            roster,
            Box::new(clock.clone()),
            p.sink,
        )
        .map_err(|e| e.to_string())?;
        let d = Arc::new_cyclic(|me| Decision {
            me: me.clone(),
            world,
            clock,
            catalog: p.catalog,
            areas: p.areas,
            operators: p.operators,
            engage_operators: p.engage_operators,
            auth: p.auth,
            terrain: p.terrain,
            rules,
            tracks: p.tracks,
            audit_path: p.audit_path,
            anchors: std::sync::OnceLock::new(),
            core: Mutex::new(Core {
                engine,
                sessions: BTreeMap::new(),
                logins: BTreeMap::new(),
                running: BTreeMap::new(),
                taskings: BTreeMap::new(),
                space_runs: BTreeMap::new(),
                engagements: BTreeMap::new(),
                params: BTreeMap::new(),
                carry_problem: None,
                view_budget: BTreeMap::new(),
                follow: BTreeMap::new(),
                pointer_written: None,
                fleet_written: None,
                fleet_looked: None,
            }),
            updates: watch::channel(Arc::new(String::new())).0,
            recent: agent::RecentEvents::default(),
            space: std::sync::RwLock::new(None),
            queue_cfg,
            pointer: std::sync::OnceLock::new(),
            fleet_file: std::sync::OnceLock::new(),
        });
        d.publish();
        Ok(d)
    }

    pub fn audit_path(&self) -> &Path {
        &self.audit_path
    }

    pub fn terrain_configured(&self) -> bool {
        self.terrain.configured()
    }

    /// Give the engine the space segment (once `crate::space` has loaded it), with the
    /// scenario clock as a map from the engine clock. Until then collects and covers fail
    /// closed ("no space segment loaded").
    pub fn install_space(&self, space: Arc<murmur_space::Space>, map: ScenarioMap) {
        let seg = match SpaceSegment::new(space).and_then(|s| s.with_scenario_map(map)) {
            Ok(s) => Arc::new(s),
            Err(e) => return eprintln!("decision: space segment: {e}"),
        };
        if let Err(e) = self.lock().engine.set_space(Box::new(seg.clone())) {
            return eprintln!("decision: space segment: {e}");
        }
        *self.space.write().unwrap_or_else(|e| e.into_inner()) = Some(seg);
        self.publish();
    }

    /// A collect's or cover's space question answered now, outside the decision lock (a
    /// week of Starlink coverage takes over a second of SGP4): the engine uses the answer for
    /// the check that follows if it is still fresh then ([`murmur_decide::PreparedSpace`]),
    /// so agents' and operators' space queries never hold the lock the picture tick needs.
    async fn prepare_space(&self, task: &Task) -> Option<murmur_decide::PreparedSpace> {
        if !task.is_space() {
            return None;
        }
        let seg = self.space.read().unwrap_or_else(|e| e.into_inner()).clone()?;
        let task = task.clone();
        let t = self.clock.now();
        let rules = self.rules.clone();
        tokio::task::spawn_blocking(move || {
            murmur_decide::prepare_space(&seg, &task, t, &rules).ok()
        })
        .await
        .ok()
        .flatten()
    }

    fn lock(&self) -> MutexGuard<'_, Core> {
        self.core.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The track store: the injected one, else the process-wide one (`tracks::init`).
    pub(crate) fn tracks(&self) -> Option<Arc<Tracks>> {
        self.tracks.clone().or_else(crate::tracks::store)
    }

    /// The operators and who of them has engagement authority (for the console), how they
    /// are authenticated (`credentials` or `dev`), and the acts besides signatures that
    /// re-enter the credential in this run (`[approval] step_up_acts`; none in a dev run).
    pub fn operators_json(&self) -> Value {
        let acts: Vec<StepUpAct> = [StepUpAct::DeclareHostile, StepUpAct::AcceptUnknown]
            .into_iter()
            .filter(|a| self.steps_up(*a))
            .collect();
        json!({
            "operators": self.all_operators(),
            "engage": self.engage_operators,
            "auth": self.auth.as_str(),
            "step_up_acts": acts,
        })
    }

    /// Credentials run: whether `act` re-enters the operator's credential (the policy's
    /// `[approval] step_up_acts`).
    pub fn steps_up(&self, act: StepUpAct) -> bool {
        self.auth == AuthMode::Credentials && self.lock().engine.policy().steps_up(act)
    }

    /// Credentials run: a refused login for `op` (on the roster), logged as `auth.refused`
    /// with why and the lock it set. Nothing secret; a failure to log is reported, not fatal.
    pub fn record_login_refused(&self, op: &OperatorId, reason: &str, locked_s: Option<u64>) {
        if let Err(e) = self.lock().engine.record_login_refused(op, reason, locked_s) {
            eprintln!("decision: logging a refused login for {op}: {e}");
        }
    }

    /// Whether `op` is on the run's roster, and with engagement authority.
    pub fn roster_entry(&self, op: &OperatorId) -> Option<bool> {
        self.operators
            .contains(op)
            .then(|| self.engage_operators.contains(op))
    }

    /// Credentials run: open the engine session for a login whose password `crate::auth`
    /// just checked (`session.opened`, `auth: password`; `auth: scripted` when the credential
    /// is harness-held), with engagement authority if `engage` (the credential holds it now)
    /// and the roster gave it at start.
    pub fn open_login(&self, op: &OperatorId, engage: bool, scripted: bool) -> Result<SessionId, DecisionError> {
        if self.auth != AuthMode::Credentials {
            return Err(DecisionError::new(409, "this run asserts operator ids (MURMUR_AUTH=dev): there is no login"));
        }
        let grants = murmur_decide::Grants {
            engage: engage && self.engage_operators.contains(op),
        };
        let out = {
            let mut core = self.lock();
            let s = core
                .engine
                .open_authenticated_session(
                    op.clone(),
                    grants,
                    if scripted { AuthMethod::Scripted } else { AuthMethod::Password },
                )
                .map_err(engine_err)?;
            let id = s.authority().session();
            core.logins.insert(id, s);
            id
        };
        Ok(out)
    }

    /// Credentials run: end a login's engine session (`session.closed`, with why: logout,
    /// expiry, a credential removed or changed). Its authorities and picture tokens go with
    /// it. Nothing happens for a session already ended.
    pub fn close_login(&self, session: SessionId, reason: &str) {
        let mut core = self.lock();
        if let Some(s) = core.logins.remove(&session)
            && let Err(e) = core.engine.close_operator_session(s, reason)
        {
            eprintln!("decision: closing session {}: {e}", session.0);
        }
    }

    /// Credentials run: whether approving intent `id` re-enters the approver's credential
    /// (its decision says `step_up`: every engagement, any tier marked so).
    pub fn needs_step_up(&self, id: u64) -> bool {
        self.auth == AuthMode::Credentials
            && self
                .lock()
                .engine
                .intent(IntentId(id))
                .and_then(Intent::policy)
                .is_some_and(|p| p.step_up)
    }

    /// Rules 8: the step-up `crate::auth` checked for `by`'s login, logged under `session`
    /// (`operator.action step_up`) for the act about to quote it. None when none was checked.
    fn mint_step(
        engine: &mut Engine,
        session: &OperatorSession,
        by: &Actor,
        step: Option<crate::auth::StepUpChecked>,
    ) -> Result<Option<murmur_decide::StepUp>, DecisionError> {
        match step {
            Some(c) if c.session() == by.login() && c.operator() == by.operator() => {
                Ok(Some(engine.step_up(session).map_err(engine_err)?))
            }
            Some(_) => Err(DecisionError::new(401, "the credential re-entered is not this session's")),
            None => Ok(None),
        }
    }

    /// An operator declares a track's affiliation (the tracks module has applied it):
    /// `operator.action track.declare`, quoting the step-up when the policy marks a hostile
    /// declaration consequential.
    pub fn record_declaration(
        &self,
        by: &Actor,
        step: Option<crate::auth::StepUpChecked>,
        track: &str,
        affiliation: &str,
        note: Option<&str>,
    ) -> Result<(), DecisionError> {
        let mut guard = self.lock();
        let core = &mut *guard;
        let session = Self::session(&mut core.engine, &mut core.sessions, &core.logins, by, &self.engage_operators, self.auth)?;
        let step = Self::mint_step(&mut core.engine, session, by, step)?;
        core.engine
            .record_declaration(session, step, track, affiliation, note)
            .map_err(engine_err)
    }

    fn all_operators(&self) -> Vec<OperatorId> {
        let mut v = self.operators.clone();
        for o in &self.engage_operators {
            if !v.contains(o) {
                v.push(o.clone());
            }
        }
        v
    }

    fn fleet(&self) -> BTreeMap<Uid, Arc<VehicleCell>> {
        self.world
            .vehicles()
            .into_iter()
            .map(|v| (v.uid, v))
            .collect()
    }

    // ---- operators ------------------------------------------------------------------------

    /// Dev run: the operator a request names. `header` is `X-Murmur-Operator`; `legacy` lets
    /// a request without one (the console's `POST /api/tasks`, a track declaration) act as
    /// the first listed operator. A credentials run never names its operator this way
    /// (`crate::auth` resolves the login instead): 401.
    pub fn operator(
        &self,
        header: Option<&str>,
        legacy: bool,
    ) -> Result<OperatorId, DecisionError> {
        if self.auth == AuthMode::Credentials {
            return Err(DecisionError::new(
                401,
                "sign in: this bridge authenticates operators with their own password (MURMUR_AUTH=dev asserts ids by header instead)",
            ));
        }
        match header.map(str::trim).filter(|s| !s.is_empty()) {
            Some(h) => self
                .all_operators()
                .into_iter()
                .find(|o| o.as_str() == h)
                .ok_or_else(|| {
                    DecisionError::new(
                        403,
                        format!("operator {h:?} is not in MURMUR_OPERATORS or MURMUR_ENGAGE_OPERATORS"),
                    )
                }),
            None if legacy => self
                .operators
                .first()
                .cloned()
                .ok_or_else(|| DecisionError::new(500, "no operator configured")),
            None => Err(DecisionError::new(
                401,
                "set the X-Murmur-Operator header to an operator id (MURMUR_OPERATORS; a dev run: \
                 the id is asserted, not authenticated)",
            )),
        }
    }

    /// The session an actor acts in: their login's (credentials run; 401 once it has ended),
    /// or (dev run) the asserted operator's, opened (and logged) on first use with engagement
    /// authority if the operator is in `MURMUR_ENGAGE_OPERATORS`.
    fn session<'a>(
        engine: &mut Engine,
        sessions: &'a mut BTreeMap<OperatorId, OperatorSession>,
        logins: &'a BTreeMap<SessionId, OperatorSession>,
        by: &Actor,
        engage_operators: &[OperatorId],
        auth: AuthMode,
    ) -> Result<&'a OperatorSession, DecisionError> {
        if let Some(id) = by.login {
            return logins
                .get(&id)
                .filter(|s| s.operator() == &by.operator)
                .ok_or_else(|| DecisionError::new(401, "your session has ended: sign in again"));
        }
        if auth == AuthMode::Credentials {
            return Err(DecisionError::new(
                401,
                "sign in: this bridge authenticates operators with their own password",
            ));
        }
        let op = &by.operator;
        if !sessions.contains_key(op) {
            let grants = murmur_decide::Grants {
                engage: engage_operators.contains(op),
            };
            let s = engine
                .open_operator_session_with(op.clone(), grants)
                .map_err(engine_err)?;
            sessions.insert(op.clone(), s);
        }
        sessions
            .get(op)
            .ok_or_else(|| DecisionError::new(500, "operator session missing"))
    }

    // ---- requests -------------------------------------------------------------------------

    /// A human's intent: `POST /api/intents`, and the legacy `POST /api/tasks`. Malformed
    /// requests (unknown task or vehicle, missing parameters) are refused before an intent
    /// exists, as before. Auto-approved intents are dispatched at once.
    pub async fn submit(
        &self,
        by: &Actor,
        req: IntentRequest,
    ) -> Result<Outcome, DecisionError> {
        self.submit_stepped(by, req, None).await
    }

    /// [`Decision::submit`] with the step-up `crate::auth` checked for it: a nomination that
    /// accepts lost vehicles quotes it when the policy lists `accept_unknown`.
    pub async fn submit_stepped(
        &self,
        by: &Actor,
        req: IntentRequest,
        step: Option<crate::auth::StepUpChecked>,
    ) -> Result<Outcome, DecisionError> {
        if matches!(req.task.as_str(), "collect" | "cover") {
            return self.submit_space(by, &req).await;
        }
        if req.task == "engage" {
            return self.submit_engage(by, &req, step).await;
        }
        let fleet = self.fleet();
        let order = TaskRequest {
            task: req.task.clone(),
            uids: req.uids.clone(),
            params: req.params.clone(),
        };
        match TaskOrder::new(order, &fleet) {
            Ok(_) => {}
            Err(e @ OrderError::UnknownVehicles(_)) => {
                return Err(DecisionError::new(404, e.to_string()));
            }
            Err(e) => return Err(DecisionError::new(400, e.to_string())),
        }
        let proposal = proposal(&req)?;
        self.prefetch(&proposal.task, &proposal.uids).await;
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            let world = self.snapshot();
            let session = Self::session(&mut core.engine, &mut core.sessions, &core.logins, by, &self.engage_operators, self.auth)?;
            let id = core
                .engine
                .submit(
                    session.authority(),
                    proposal,
                    &world,
                    &self.terrain.oracle(),
                )
                .map_err(engine_err)?
                .id();
            self.after_decision(core, id)
        };
        self.publish();
        out
    }

    /// A human's engagement nomination: never approved at submission (the engine's floor);
    /// it waits in the inbox for operators with engagement authority.
    async fn submit_engage(
        &self,
        by: &Actor,
        req: &IntentRequest,
        step: Option<crate::auth::StepUpChecked>,
    ) -> Result<Outcome, DecisionError> {
        self.check_vehicles(&req.uids)?;
        let proposal = engage::proposal(req, self.clock.now())?;
        self.prefetch(&proposal.task, &proposal.uids).await;
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            let world = self.snapshot();
            let session =
                Self::session(&mut core.engine, &mut core.sessions, &core.logins, by, &self.engage_operators, self.auth)?;
            // rules 6: the operator's acceptances of lost vehicles, as the check showed them
            // (only with engagement authority: 403 otherwise)
            let accept: Vec<(DUid, Option<Time>)> =
                req.engage.accept_unknown.iter().map(engage::AcceptUnknown::engine).collect();
            let submitted = if accept.is_empty() {
                core.engine
                    .submit(session.authority(), proposal, &world, &self.terrain.oracle())
            } else {
                let Some(auth) = session.engagement_authority() else {
                    return Err(DecisionError::new(
                        403,
                        format!("{by} has no engagement authority: only an operator with it accepts a lost vehicle"),
                    ));
                };
                match Self::mint_step(&mut core.engine, session, by, step)? {
                    Some(s) => core.engine.submit_accepting_stepped(
                        auth,
                        s,
                        proposal,
                        &accept,
                        &world,
                        &self.terrain.oracle(),
                    ),
                    None => core
                        .engine
                        .submit_accepting(auth, proposal, &accept, &world, &self.terrain.oracle()),
                }
            };
            let id = submitted.map_err(engine_err)?.id();
            self.after_decision(core, id)
        };
        self.publish();
        out
    }

    /// Every named vehicle is in the fleet (404 otherwise).
    fn check_vehicles(&self, uids: &[Uid]) -> Result<(), DecisionError> {
        let fleet = self.fleet();
        let missing: Vec<String> = uids
            .iter()
            .filter(|u| !fleet.contains_key(u))
            .map(ToString::to_string)
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(DecisionError::new(
                404,
                format!("unknown vehicles [{}]", missing.join(", ")),
            ))
        }
    }

    /// A human's collect or cover. The space query runs before the decision lock is taken
    /// ([`Self::prepare_space`]; SGP4 over the cached elements: well under a second for a
    /// collect at the scenario area, over a second for a week of Starlink coverage), and the
    /// engine uses that answer if it is still fresh when the checks run.
    async fn submit_space(&self, by: &Actor, req: &IntentRequest) -> Result<Outcome, DecisionError> {
        let proposal = space::proposal(
            &req.task,
            !req.uids.is_empty(),
            &req.space,
            req.rationale.as_deref(),
        )?;
        let prepared = self.prepare_space(&proposal.task).await;
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            let world = self.snapshot();
            let session = Self::session(&mut core.engine, &mut core.sessions, &core.logins, by, &self.engage_operators, self.auth)?;
            if let Some(p) = prepared {
                core.engine.use_prepared_space(p);
            }
            let id = core
                .engine
                .submit(
                    session.authority(),
                    proposal,
                    &world,
                    &self.terrain.oracle(),
                )
                .map_err(engine_err)?
                .id();
            self.after_decision(core, id)
        };
        self.publish();
        out
    }

    /// Approve a pending intent (re-checked against the current world; voided if the
    /// picture no longer holds), then dispatch it. Tests' shorthand for
    /// [`Self::approve_seen`] without a preview (which an engagement needs).
    #[cfg(test)]
    pub async fn approve(
        &self,
        by: &Actor,
        id: u64,
        note: Option<String>,
    ) -> Result<Outcome, DecisionError> {
        self.approve_with(by, id, note, None, false).await
    }

    /// `POST /api/intents/{id}/picture`: what an operator with engagement authority would
    /// sign (the engine's picture token: the plan, the track's identity, valid
    /// `token_ttl_s`, bound to their session). The console shows it and the signature quotes
    /// its `id`.
    pub fn issue_picture(&self, by: &Actor, id: u64) -> Result<Value, DecisionError> {
        let id = IntentId(id);
        let mut guard = self.lock();
        let core = &mut *guard;
        let world = self.snapshot();
        let session = Self::session(&mut core.engine, &mut core.sessions, &core.logins, by, &self.engage_operators, self.auth)?;
        let Some(auth) = session.engagement_authority() else {
            return Err(DecisionError::new(
                403,
                match self.auth {
                    AuthMode::Credentials => format!("{by} has no engagement authority (their credential does not hold it): nothing to sign"),
                    AuthMode::Dev => format!("{by} has no engagement authority (MURMUR_ENGAGE_OPERATORS): nothing to sign"),
                },
            ));
        };
        let token = core
            .engine
            .issue_picture(id, auth, &world)
            .map_err(|e| self.with_intent(core, id, engine_err(e)))?;
        Ok(serde_json::to_value(&token).unwrap_or(Value::Null))
    }

    /// [`Self::approve`] with what the signature needs: an engagement's quotes the picture
    /// token its signer was shown (`token`, from [`Self::issue_picture`]); the engine refuses
    /// it when the token expired, is another session's, or its plan or track facts no longer
    /// hold. Any other intent whose dispatch would stop an engagement in play on its vehicles
    /// needs `acknowledge` (the engine refuses it otherwise, and logs what was acknowledged).
    #[cfg(test)]
    pub async fn approve_with(
        &self,
        by: &Actor,
        id: u64,
        note: Option<String>,
        token: Option<u64>,
        acknowledge: bool,
    ) -> Result<Outcome, DecisionError> {
        self.approve_stepped(by, id, note, token, acknowledge, None).await
    }

    /// [`Self::approve_with`] after a step-up: `step` says `crate::auth` just checked the
    /// approver's password again, for this login. The engine logs the step-up under the
    /// session (`operator.action step_up`) and the signature quotes it; a consequential
    /// signature (every engagement's, any tier with `step_up`) is refused without one in a
    /// credentials run (401).
    pub async fn approve_stepped(
        &self,
        by: &Actor,
        id: u64,
        note: Option<String>,
        token: Option<u64>,
        acknowledge: bool,
        step: Option<crate::auth::StepUpChecked>,
    ) -> Result<Outcome, DecisionError> {
        let id = IntentId(id);
        let (task, uids) = {
            let core = self.lock();
            let i = core
                .engine
                .intent(id)
                .ok_or_else(|| DecisionError::new(404, format!("unknown intent {id}")))?;
            (i.spec().task.clone(), i.spec().uids.clone())
        };
        if matches!(task, Task::Engage { .. }) {
            // the plan this signature would be on: the frozen one, or the token's
            let plan = {
                let core = self.lock();
                core.engine
                    .intent(id)
                    .and_then(|i| i.engagement())
                    .and_then(|e| e.plan.as_ref())
                    .map(|p| p.preview.effect_point)
                    .or_else(|| {
                        token
                            .and_then(|t| core.engine.picture_token(t))
                            .map(|t| t.plan.effect_point)
                    })
            };
            self.prefetch_engage(&task, &uids, plan).await;
        } else {
            self.prefetch(&task, &uids).await;
        }
        let prepared = self.prepare_space(&task).await;
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            let world = self.snapshot();
            let session = Self::session(&mut core.engine, &mut core.sessions, &core.logins, by, &self.engage_operators, self.auth)?;
            if let Some(p) = prepared {
                core.engine.use_prepared_space(p);
            }
            // rules 8: the password was just checked again for this login: log the step-up
            // under its session, for this signature to quote
            let step = Self::mint_step(&mut core.engine, session, by, step)?;
            let oracle = self.terrain.oracle();
            let r = if matches!(task, Task::Engage { .. }) {
                match (session.engagement_authority(), token, step) {
                    (Some(auth), Some(token), Some(s)) => core
                        .engine
                        .approve_engagement_stepped(id, auth, s, note, token, &world, &oracle)
                        .map(|_| ()),
                    (Some(auth), Some(token), None) => core
                        .engine
                        .approve_engagement(id, auth, note, token, &world, &oracle)
                        .map(|_| ()),
                    (Some(_), None, _) => {
                        return Err(DecisionError::new(
                            400,
                            "an engagement signature quotes the picture you were shown: ask for it first (POST /api/intents/{id}/picture) and send its token",
                        ));
                    }
                    (None, _, _) => core
                        .engine
                        .approve(id, session.authority(), note, &world, &oracle)
                        .map(|_| ()),
                }
            } else if let Some(s) = step {
                core.engine
                    .approve_stepped(id, session.authority(), s, note, acknowledge, &world, &oracle)
                    .map(|_| ())
            } else if acknowledge {
                core.engine
                    .approve_acknowledging(id, session.authority(), note, &world, &oracle)
                    .map(|_| ())
            } else {
                core.engine
                    .approve(id, session.authority(), note, &world, &oracle)
                    .map(|_| ())
            };
            match r {
                Ok(()) => self.after_decision(core, id),
                Err(e) => Err(self.with_intent(core, id, engine_err(e))),
            }
        };
        self.publish();
        out
    }

    pub fn reject(
        &self,
        by: &Actor,
        id: u64,
        note: Option<String>,
    ) -> Result<Outcome, DecisionError> {
        let id = IntentId(id);
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            let session = Self::session(&mut core.engine, &mut core.sessions, &core.logins, by, &self.engage_operators, self.auth)?;
            match core.engine.reject(id, session.authority(), note) {
                Ok(_) => Ok(self.outcome(core, id)),
                Err(e) => Err(self.with_intent(core, id, engine_err(e))),
            }
        };
        self.publish();
        out
    }

    /// Withdraw an intent before dispatch.
    pub fn cancel(
        &self,
        by: &Actor,
        id: u64,
        note: Option<String>,
    ) -> Result<Outcome, DecisionError> {
        let id = IntentId(id);
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            let session = Self::session(&mut core.engine, &mut core.sessions, &core.logins, by, &self.engage_operators, self.auth)?;
            let note = note.unwrap_or_else(|| "cancelled".into());
            match core.engine.cancel(id, session.authority(), &note) {
                Ok(_) => Ok(self.outcome(core, id)),
                Err(e) => Err(self.with_intent(core, id, engine_err(e))),
            }
        };
        self.publish();
        out
    }

    /// Log something an operator did outside the intent flow (a track declaration, an
    /// acknowledgement) as `operator.action`, under their session.
    pub fn record_action(
        &self,
        by: &Actor,
        action: &str,
        detail: &str,
    ) -> Result<(), DecisionError> {
        let mut guard = self.lock();
        let core = &mut *guard;
        let session = Self::session(&mut core.engine, &mut core.sessions, &core.logins, by, &self.engage_operators, self.auth)?;
        core.engine
            .record_operator_action(session.authority(), action, detail)
            .map_err(engine_err)
    }

    /// Log what an operator's console showed for a pending intent (`operator.view`, Phase
    /// G): `POST /api/operator/view`. Unknown intent 404, malformed view 422, an `open` or
    /// `decide` view of an intent no longer waiting for a decision 409, more than
    /// [`VIEW_BURST`] views at once (refilled at [`VIEW_RATE_PER_S`] a second) 429.
    pub fn record_view(
        &self,
        by: &Actor,
        id: u64,
        view: OperatorView,
    ) -> Result<(), DecisionError> {
        let mut guard = self.lock();
        let core = &mut *guard;
        let mono = clock::mono();
        let budget = core.view_budget.entry(by.operator().clone()).or_insert((VIEW_BURST, mono));
        budget.0 = (budget.0 + (mono - budget.1).max(0.0) * VIEW_RATE_PER_S).min(VIEW_BURST);
        budget.1 = mono;
        if budget.0 < 1.0 {
            return Err(DecisionError::new(
                429,
                "too many view events: this is instrumentation, a few per card",
            ));
        }
        budget.0 -= 1.0;
        let session = Self::session(&mut core.engine, &mut core.sessions, &core.logins, by, &self.engage_operators, self.auth)?;
        core.engine
            .record_operator_view(session.authority(), IntentId(id), view)
            .map_err(|e| match e {
                EngineError::Proposal(p) => DecisionError::new(422, p.to_string()),
                e => engine_err(e),
            })
    }

    fn with_intent(&self, core: &Core, id: IntentId, mut e: DecisionError) -> DecisionError {
        if let Some(i) = core.engine.intent(id) {
            if let Some(last) = i.history().last()
                && !e.detail.contains(&last.reason)
            {
                e.detail = format!("{}: {}", e.detail, last.reason);
            }
            e.intent = serde_json::to_value(i).ok();
        }
        e
    }

    fn outcome(&self, core: &Core, id: IntentId) -> Outcome {
        let intent = core.engine.intent(id);
        Outcome {
            id,
            state: intent.map_or(State::Proposed, Intent::state),
            intent: intent.map_or(Value::Null, |i| intent_json(i, None, self.tol())),
            tasking: core.taskings.get(&id).map(|t| t.snapshot()),
            authorization: intent.and_then(authorization_json),
            space: core.space_runs.get(&id).map(space::SpaceRun::json),
        }
    }

    /// After submit or approve: dispatch what may run, report the rest as it stands.
    fn after_decision(&self, core: &mut Core, id: IntentId) -> Result<Outcome, DecisionError> {
        let state = core.engine.intent(id).map(Intent::state);
        if matches!(state, Some(State::AutoApproved | State::Approved)) {
            self.dispatch(core, id)?;
        }
        Ok(self.outcome(core, id))
    }

    /// The one place an [`AuthorizedOrder`] is made: from the engine's `DispatchOrder`.
    fn dispatch(&self, core: &mut Core, id: IntentId) -> Result<(), DecisionError> {
        let fleet = self.fleet();
        let spec = core
            .engine
            .intent(id)
            .ok_or_else(|| DecisionError::new(404, format!("unknown intent {id}")))?
            .spec()
            .clone();
        if spec.task.is_space() {
            return self.dispatch_space(core, id);
        }
        if matches!(spec.task, Task::Engage { .. }) {
            return self.dispatch_engage(core, id);
        }
        // the task layer must accept it before the engine marks it dispatched
        if let Err(e) = task_request(&spec.task, &spec.uids)
            .and_then(|r| TaskOrder::new(r, &fleet).map_err(|e| e.to_string()))
        {
            core.engine
                .void(id, &format!("the task layer can't take it: {e}"))
                .map_err(engine_err)?;
            return Ok(());
        }
        // rules 5: dispatched when the link can carry it to every vehicle, else queued
        self.dispatch_vehicles(core, id)
    }

    /// Older orders still waiting for these vehicles are superseded (voided), and an
    /// engagement in play on one of them (pending, approved or under way) is stopped with
    /// that cause: the newest order stands. A newer order is never voided by an older one's
    /// dispatch (rules 5: an older queued order that comes out of the queue after it).
    fn supersede(&self, core: &mut Core, id: IntentId, uids: &[DUid]) {
        // rules 7: the engagements first, right after the dispatch (the engine and replay take
        // a superseding order only in the step that dispatched it, nothing logged between)
        let rules7 = self.rules.rules_version >= rules::QUEUE_FOLLOWUPS_SINCE;
        if rules7 {
            self.stop_superseded_engagements(core, id, uids);
        }
        let took: BTreeSet<&DUid> = uids.iter().collect();
        let stale: Vec<(IntentId, DUid)> = core
            .engine
            .intents()
            .filter(|i| {
                i.id() < id
                    && !matches!(i.spec().task, Task::Engage { .. })
                    && matches!(
                        i.state(),
                        State::PendingApproval | State::Approved | State::Queued
                    )
            })
            .filter_map(|i| {
                let u = i.spec().uids.iter().find(|u| took.contains(u))?;
                Some((i.id(), u.clone()))
            })
            .collect();
        for (other, uid) in stale {
            let why = format!("superseded: {id} was dispatched to vehicle {uid}");
            if let Err(e) = core.engine.void(other, &why) {
                eprintln!("decision: void {other}: {e}");
            }
        }
        if !rules7 {
            self.stop_superseded_engagements(core, id, uids);
        }
    }

    /// A collect or cover: the engine releases its order (`intent.dispatched`), the space
    /// executor reports its first state (`tasked` or `watching`) at once.
    fn dispatch_space(&self, core: &mut Core, id: IntentId) -> Result<(), DecisionError> {
        let answer = core
            .engine
            .intent(id)
            .and_then(|i| i.space_answer().cloned());
        let run = match core.engine.dispatch_order(id).map_err(engine_err)? {
            Order::Collect(o) => space::SpaceRun::collect(o),
            Order::Cover(o) => space::SpaceRun::cover(o, answer.as_ref()),
            Order::Vehicles(_) | Order::Engage(_) => {
                return Err(DecisionError::new(500, SPACE_NOT_VEHICLES));
            }
        };
        core.space_runs.insert(id, run);
        self.step_space(core, &[id]);
        Ok(())
    }

    /// Advance space runs to the current scenario time: log what they report as op
    /// states, finish the intents whose run ended, and keep the last finished ones.
    fn step_space(&self, core: &mut Core, ids: &[IntentId]) {
        let now = self.clock.now();
        let map = core.engine.scenario_map();
        let Core {
            engine, space_runs, ..
        } = core;
        for id in ids {
            let Some(run) = space_runs.get_mut(id) else {
                continue;
            };
            if run.finished() {
                continue;
            }
            let resource = run.resource().clone();
            for r in run.step(now, &map) {
                if let Err(e) = engine.record_op_state(*id, &resource, r.state, &r.detail) {
                    eprintln!("decision: op state: {e}");
                }
            }
            if let Some(outcome) = run.outcome().cloned()
                && let Err(e) = engine.finish(*id, outcome)
            {
                eprintln!("decision: finish {id}: {e}");
            }
        }
        let finished: Vec<IntentId> = space_runs
            .iter()
            .filter(|(_, r)| r.finished())
            .map(|(id, _)| *id)
            .collect();
        for id in finished
            .iter()
            .take(finished.len().saturating_sub(space::KEEP_FINISHED))
        {
            space_runs.remove(id);
        }
    }

    fn follow_space(&self, core: &mut Core) {
        let ids: Vec<IntentId> = core
            .space_runs
            .iter()
            .filter(|(_, r)| !r.finished())
            .map(|(id, _)| *id)
            .collect();
        if !ids.is_empty() {
            self.step_space(core, &ids);
        }
    }

    /// The space part of the console's state: the scenario clock as a map from the
    /// bridge clock, and every collect and cover run (newest first).
    fn space_json(&self, core: &Core) -> Value {
        json!({
            "clock": core.engine.scenario_map(),
            "loaded": core.engine.space_segment().is_some(),
            "runs": core.space_runs.values().rev().map(space::SpaceRun::json).collect::<Vec<_>>(),
        })
    }

    /// `GET /api/space/runs`.
    pub fn space_runs_json(&self) -> Value {
        let core = self.lock();
        self.space_json(&core)
    }

    // ---- world --------------------------------------------------------------------------

    fn snapshot(&self) -> murmur_decide::WorldSnapshot {
        snapshot::build(
            &self.world,
            &self.catalog,
            &self.areas,
            self.clock.now(),
            self.tracks().as_deref(),
        )
    }

    /// Ask tidewater for the task's waypoints first, if some target must stay on water or
    /// land (the engine's oracle then answers from the cache).
    async fn prefetch(&self, task: &Task, uids: &[DUid]) {
        if !self.terrain.configured() {
            return;
        }
        if matches!(task, Task::Engage { .. }) {
            return self.prefetch_engage(task, uids, None).await;
        }
        let Some(path) = task.path() else { return };
        let fleet = self.fleet();
        let needs = uids.iter().filter_map(bridge_uid).any(|u| {
            fleet
                .get(&u)
                .and_then(|c| {
                    c.read(|v| {
                        v.identity
                            .as_ref()
                            .and_then(|i| i.str("platform"))
                            .map(str::to_string)
                    })
                })
                .and_then(|p| self.catalog.get(&p).cloned())
                .is_some_and(|p| rules::required_surface(p.domain, p.autopilot).is_some())
        });
        if needs {
            self.terrain.prefetch(&path.points).await;
        }
    }

    /// An engagement's effect point, for a boat or ground platform: `plan` (a frozen or
    /// token plan's point), or the preview on the world now.
    async fn prefetch_engage(&self, task: &Task, uids: &[DUid], plan: Option<LonLat>) {
        if !self.terrain.configured() {
            return;
        }
        let spec = murmur_decide::IntentSpec {
            id: IntentId(0),
            created_t: self.clock.now(),
            source: murmur_decide::Source::Human {
                id: OperatorId::new("prefetch"),
            },
            task: task.clone(),
            uids: uids.to_vec(),
            constraints: Constraints::default(),
            rationale: None,
        };
        let world = self.snapshot().subset_for(&spec);
        if !murmur_decide::engage::surface_needed(&spec, &world) {
            return;
        }
        let point = plan.or_else(|| murmur_decide::engage::terrain_point(&spec, &world, &self.rules));
        if let Some(p) = point {
            self.terrain.prefetch_near(&[p]).await;
        }
    }

    // ---- the loop -----------------------------------------------------------------------

    /// The loop: once a second, [`Self::tick`] (which holds every engagement's picture);
    /// beside it, its own task fetches terrain for pending engagements, so a slow or
    /// failing terrain service never delays the tick (a gap in holding the picture aborts an
    /// engagement: `picture_gap`).
    pub async fn run(self: Arc<Self>) {
        let prefetcher = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                prefetcher.prefetch_engagements().await;
                // queued intents' waypoints: their dequeue re-check reads them from the cache
                if !prefetcher.terrain.backing_off() {
                    for (task, uids) in prefetcher.queued_paths() {
                        prefetcher.prefetch(&task, &uids).await;
                    }
                }
            }
        });
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            self.tick();
        }
    }

    /// Pending engagements' predicted effect points (they move with their tracks), for the
    /// console's live checks and the signatures (the reach check asks a boat's or UGV's
    /// effect point for land or water). Approved and dispatched engagements are held on
    /// their approval snapshot, which needs no terrain. Skipped for a few seconds after
    /// tidewater fails.
    async fn prefetch_engagements(&self) {
        if !self.terrain.configured() || self.terrain.backing_off() {
            return;
        }
        // each pending engagement with its frozen plan's point, if it has one
        let specs: Vec<(murmur_decide::IntentSpec, Option<LonLat>)> = self
            .lock()
            .engine
            .intents()
            .filter(|i| {
                matches!(i.spec().task, Task::Engage { .. }) && i.state() == State::PendingApproval
            })
            .map(|i| {
                let plan = i
                    .engagement()
                    .and_then(|e| e.plan.as_ref())
                    .map(|p| p.preview.effect_point);
                (i.spec().clone(), plan)
            })
            .collect();
        if specs.is_empty() {
            return;
        }
        let world = self.snapshot();
        let points: Vec<LonLat> = specs
            .iter()
            .filter_map(|(s, plan)| {
                let w = world.subset_for(s);
                if !murmur_decide::engage::surface_needed(s, &w) {
                    return None;
                }
                plan.or_else(|| murmur_decide::engage::terrain_point(s, &w, &self.rules))
            })
            .collect();
        if !points.is_empty() {
            self.terrain.prefetch_near(&points).await;
        }
    }

    pub fn tick(&self) {
        {
            let mut guard = self.lock();
            let core = &mut *guard;
            if let Err(e) = core.engine.expire_due() {
                eprintln!("decision: expire: {e}");
            }
            self.log_reboots(core);
            self.log_vehicle_events(core);
            self.void_lost(core);
            self.follow_queue(core);
            self.follow_resumed(core);
            self.follow_ops(core);
            self.follow_space(core);
            self.step_engagements(core);
            self.write_pointer(core);
            self.write_fleet(core);
        }
        self.publish();
    }

    fn log_vehicle_events(&self, core: &mut Core) {
        let mut failsafes: Vec<(DUid, String)> = Vec::new();
        let mut left: Vec<(DUid, String)> = Vec::new();
        for cell in self.world.vehicles() {
            let (events, mission_mode) = cell.write(|v| {
                let events: Vec<_> = v.events.drain(..).collect();
                (events, v.kind.and_then(crate::tasks::caps).map(|c| c.auto))
            });
            for e in &events {
                if let Err(err) = core
                    .engine
                    .record_vehicle_event(&duid(cell.uid), e.kind, &e.text)
                {
                    eprintln!("decision: vehicle event: {err}");
                }
                // a failsafe that took control from the mission (a mode switch, a disarm, a
                // landing: failsafe.rs); one that says nothing of what it did is left to
                // the mode change below
                if e.kind == "failsafe" && failsafe::failsafe_took_over(&e.text) {
                    failsafes.push((duid(cell.uid), e.text.clone()));
                }
                // the platform left the mission mode an intercept flies in: its failsafe's
                // own message may have been lost on the link that failed, and the task layer
                // stops watching the mode once the platform is on station (an orbit keeps
                // orbiting)
                if e.kind == "mode"
                    && let Some(auto) = mission_mode
                    && let Some((from, to)) = e.text.split_once(" -> ")
                    && from.trim() == auto
                    && to.trim() != auto
                {
                    left.push((duid(cell.uid), e.text.clone()));
                }
            }
            self.remember_events(&cell, &events);
        }
        for (uid, text) in left {
            let hit: Vec<IntentId> = core
                .engine
                .intents()
                .filter(|i| {
                    matches!(i.spec().task, Task::Engage { .. })
                        && i.state() == State::Dispatched
                        && i.spec().uids.contains(&uid)
                })
                .map(Intent::id)
                .collect();
            for id in hit {
                let why = format!("the platform left its intercept (mode {text})");
                match core.engine.stop_engagement_system(id, &why) {
                    Ok(Some(f)) => self.command_fallback(core, f),
                    Ok(None) => {}
                    Err(e) => eprintln!("decision: stop {id}: {e}"),
                }
            }
        }
        // an engagement whose platform's own failsafe took over is stopped: the platform is
        // no longer flying the intercept, so nothing must be released on it
        for (uid, text) in failsafes {
            let hit: Vec<IntentId> = core
                .engine
                .intents()
                .filter(|i| {
                    matches!(i.spec().task, Task::Engage { .. })
                        && matches!(i.state(), State::Approved | State::Dispatched)
                        && i.spec().uids.contains(&uid)
                })
                .map(Intent::id)
                .collect();
            for id in hit {
                match core
                    .engine
                    .stop_engagement_system(id, &format!("the platform's failsafe took over ({text})"))
                {
                    Ok(Some(f)) => self.command_fallback(core, f),
                    Ok(None) => {}
                    Err(e) => eprintln!("decision: stop {id}: {e}"),
                }
            }
        }
    }

    /// Void waiting intents whose vehicle has gone silent (its facts are past the policy's
    /// freshness limit, link-aware: `max_fact_age_s` or `fact_age_intervals` report intervals,
    /// so the picture the decision rests on is gone).
    fn void_lost(&self, core: &mut Core) {
        let fleet = self.fleet();
        let now = clock::wall();
        let mono = clock::mono();
        let lost: Vec<(IntentId, String)> = core
            .engine
            .intents()
            .filter(|i| matches!(i.state(), State::PendingApproval | State::Approved))
            .filter_map(|i| {
                i.spec().uids.iter().find_map(|u| {
                    let cell = fleet.get(&bridge_uid(u)?)?;
                    let seen = cell.read(|v| v.seen.heartbeat);
                    let interval = cell.link_snapshot(mono).expected_interval_ms;
                    let (limit_ms, _) = self.rules.fact_age_limit_ms(Some(interval));
                    match seen {
                        Some(t) if now - t <= limit_ms as f64 / 1000.0 => None,
                        Some(t) => Some((
                            i.id(),
                            format!("vehicle {u} lost: no heartbeat for {:.0} s", now - t),
                        )),
                        None => Some((i.id(), format!("vehicle {u} lost: no heartbeat"))),
                    }
                })
            })
            .collect();
        for (id, why) in lost {
            if let Err(e) = core.engine.void(id, &why) {
                eprintln!("decision: void {id}: {e}");
            }
        }
    }

    /// Log each op's state changes (and NOT MOVING) as `op.state`; when all its ops have
    /// ended, finish the intent: done if every vehicle got there, else failed with why; or
    /// (rules 5), when every op ended `requeued` (the link lost before any vehicle started
    /// it), send it back to the queue.
    fn follow_ops(&self, core: &mut Core) {
        let ahead = self.writes_ahead();
        let Core {
            engine, running, ..
        } = core;
        let mut ended = Vec::new();
        let mut requeued = Vec::new();
        for (id, run) in running.iter_mut() {
            let snap = run.tasking.snapshot();
            for op in &snap.ops {
                let key = (op.state, op.detail.starts_with("NOT MOVING"));
                if run.reported.get(&op.id) == Some(&key) {
                    continue;
                }
                // rules 7: a start is logged by the op itself, ahead of sending it
                // (`write_ahead`), never after it here
                if ahead && op.state == OpState::Starting {
                    continue;
                }
                let state = serde_json::to_value(op.state)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default();
                if let Err(e) = engine.record_op_state(*id, &duid(op.uid), &state, &op.detail) {
                    eprintln!("decision: op state: {e}");
                    continue;
                }
                run.reported.insert(op.id, key);
            }
            if snap.ops.iter().all(|o| o.state.is_terminal()) {
                if !snap.ops.is_empty()
                    && snap
                        .ops
                        .iter()
                        .all(|o| matches!(o.state, OpState::Requeued | OpState::Unconfirmed))
                {
                    requeued.push(*id);
                    continue;
                }
                let bad: Vec<String> = snap
                    .ops
                    .iter()
                    .filter(|o| o.state != OpState::Done)
                    .map(|o| {
                        let s = serde_json::to_value(o.state).unwrap_or(Value::Null);
                        format!(
                            "vehicle {}: {} ({})",
                            o.uid,
                            s.as_str().unwrap_or("?"),
                            o.detail
                        )
                    })
                    .collect();
                let outcome = if bad.is_empty() {
                    TaskOutcome::Done
                } else {
                    TaskOutcome::Failed {
                        detail: bad.join("; "),
                    }
                };
                ended.push((*id, outcome));
            }
        }
        // back to the queue: re-checked on the world when the link is back, then delivered
        // asking the vehicle what it holds first; if the engine refuses (a vehicle had
        // started it), the order failed
        for id in requeued {
            match engine.requeue(id) {
                Ok(_) => {
                    running.remove(&id);
                }
                Err(e) => {
                    eprintln!("decision: requeue {id}: {e}");
                    let detail = format!("the link was lost before the order started: {e}");
                    match engine.finish(id, TaskOutcome::Failed { detail }) {
                        Ok(_) => {
                            running.remove(&id);
                        }
                        Err(e) => eprintln!("decision: finish {id}: {e}"),
                    }
                }
            }
        }
        for (id, outcome) in ended {
            match engine.finish(id, outcome) {
                Ok(_) => {
                    running.remove(&id);
                }
                Err(e) => eprintln!("decision: finish {id}: {e}"),
            }
        }
    }

    /// Rules 7: whether vehicle starts are logged ahead of sending them.
    fn writes_ahead(&self) -> bool {
        self.rules.rules_version >= rules::QUEUE_FOLLOWUPS_SINCE
    }

    /// Rules 7: the task layer's write-ahead of a vehicle start, for an order to the vehicles
    /// (none under earlier rules): an op logs its start through it, and the start goes out
    /// only once it is in the audit log.
    pub(super) fn write_ahead_hook(&self) -> Option<crate::tasks::WriteAhead> {
        if !self.writes_ahead() {
            return None;
        }
        let me = self.me.clone();
        Some(Arc::new(move |op: &crate::tasks::OpCell, detail: &str| {
            match me.upgrade() {
                Some(d) => d.write_ahead(op.intent_id, op.id, op.uid, detail),
                None => Err("the decision layer is gone".into()),
            }
        }))
    }

    /// Rules 7: `op.state` starting for op `op` (intent `intent`, vehicle `uid`), logged ahead
    /// of the start it records (`Engine::record_op_state_ahead`), under the decision lock: it
    /// waits for any decision in progress, so it comes after the dispatch that sent the order.
    /// `Err`: not logged (the order ended or a newer one took the vehicle meanwhile, or the
    /// log refused it), and the op sends nothing.
    fn write_ahead(&self, intent: u64, op: u64, uid: Uid, detail: &str) -> Result<(), String> {
        let mut guard = self.lock();
        let Core {
            engine, running, ..
        } = &mut *guard;
        let id = IntentId(intent);
        let Some(run) = running.get_mut(&id) else {
            return Err(format!("{id} is no longer followed"));
        };
        if run.reported.get(&op).is_some_and(|(s, _)| *s == OpState::Starting) {
            return Ok(());
        }
        engine
            .record_op_state_ahead(id, &duid(uid), detail)
            .map_err(|e| e.to_string())?;
        run.reported.insert(op, (OpState::Starting, false));
        Ok(())
    }

    // ---- reading ------------------------------------------------------------------------

    /// `GET /api/intents`: newest first.
    pub fn intents_json(&self) -> Value {
        let core = self.lock();
        let all: Vec<&Intent> = core.engine.intents().collect();
        let intents: Vec<Value> = all
            .into_iter()
            .rev()
            .map(|i| {
                let mut v = intent_json(i, core.taskings.get(&i.id()).map(|t| t.id), self.tol());
                if core.follow.contains_key(&i.id()) {
                    v["follow_pending"] = json!(true);
                }
                v
            })
            .collect();
        json!({
            "intents": intents,
            "audit": {
                "path": self.audit_path,
                "head": core.engine.head_hash(),
                "anchors": self.anchor_path(),
            },
        })
    }

    /// `GET /api/intents/{id}`: the intent (with a collect's or cover's whole space answer)
    /// and, once dispatched, its tasking with every op or its space run.
    pub fn intent_json(&self, id: u64) -> Option<Value> {
        let core = self.lock();
        let id = IntentId(id);
        let i = core.engine.intent(id)?;
        let tasking = core.taskings.get(&id).map(|t| t.snapshot());
        let run = core.space_runs.get(&id).map(space::SpaceRun::json);
        Some(json!({ "intent": intent_json(i, None, None), "tasking": tasking, "space": run }))
    }

    /// The collect match tolerance, for compact space answers.
    fn tol(&self) -> Option<u32> {
        Some(self.rules.space_rules().collect_match_tolerance_s)
    }

    /// `GET /api/inbox`: pending approvals (soonest expiry first, with why each needs a
    /// human) and alerts (most severe first).
    pub fn inbox_json(&self) -> Value {
        // the world only when an engagement waits (its live picture); a snapshot of a large
        // fleet is not free
        let world = self.engagements_in_view().then(|| self.snapshot());
        let pending: Vec<Value> = {
            let core = self.lock();
            core.engine
                .inbox()
                .into_iter()
                .map(|i| {
                    let mut v = intent_json(i, None, self.tol());
                    v["why"] = json!(why_human(i));
                    if let Some(w) = &world
                        && let Some(e) = self.engage_view(&core, i, w)
                    {
                        v["engage"] = e;
                    }
                    v
                })
                .collect()
        };
        let mut alerts = alerts::alerts(
            &self.world.vehicles(),
            &self.world.tasks.current_ops(),
            clock::wall(),
        );
        // rules 6: platforms whose own lost-link failsafe has not been confirmed back
        {
            let core = self.lock();
            alerts.extend(self.restore_alerts(&core, clock::wall()));
            alerts.extend(self.carry_alert(&core, clock::wall()));
        }
        alerts.sort_by(|a, b| a.severity.cmp(&b.severity).then(a.since_t.total_cmp(&b.since_t)));
        // rules 5: orders held for vehicles the link can't carry them to now
        let queued = {
            let core = self.lock();
            self.queued_json(&core)
        };
        json!({
            "pending": pending,
            "alerts": alerts,
            "queued": queued,
            "counts": { "pending": pending.len(), "alerts": alerts.len(), "queued": queued.len() },
        })
    }

    /// The WebSocket message (`type: "decisions"`): the inbox and the latest intents.
    /// Whether any engagement is among the intents the console sees.
    fn engagements_in_view(&self) -> bool {
        let core = self.lock();
        let all: Vec<&Intent> = core.engine.intents().collect();
        all.into_iter()
            .rev()
            .take(PUSH_INTENTS)
            .any(|i| matches!(i.spec().task, Task::Engage { .. }))
            || core
                .engine
                .inbox()
                .iter()
                .any(|i| matches!(i.spec().task, Task::Engage { .. }))
    }

    fn message(&self) -> String {
        let inbox = self.inbox_json();
        let world = self.engagements_in_view().then(|| self.snapshot());
        let intents: Vec<Value> = {
            let core = self.lock();
            let all: Vec<&Intent> = core.engine.intents().collect();
            all.into_iter()
                .rev()
                .take(PUSH_INTENTS)
                .map(|i| {
                    let mut v =
                        intent_json(i, core.taskings.get(&i.id()).map(|t| t.id), self.tol());
                    // resumed under way, waiting for its vehicles to connect to be followed
                    if core.follow.contains_key(&i.id()) {
                        v["follow_pending"] = json!(true);
                    }
                    if i.state() != State::PendingApproval
                        && let Some(w) = &world
                        && let Some(e) = self.engage_view(&core, i, w)
                    {
                        v["engage"] = e;
                    }
                    v
                })
                .collect()
        };
        let space = self.space_runs_json();
        json!({
            "type": "decisions",
            "inbox": inbox,
            "intents": intents,
            "space": space,
            "operators": self.operators_json(),
        })
        .to_string()
    }

    /// Push the inbox to WebSocket clients if it changed.
    pub fn publish(&self) {
        let msg = self.message();
        self.updates.send_if_modified(|cur| {
            if **cur == msg {
                false
            } else {
                *cur = Arc::new(msg);
                true
            }
        });
    }

    pub fn subscribe(&self) -> watch::Receiver<Arc<String>> {
        self.updates.subscribe()
    }
}

/// An intent for lists, the inbox and the WebSocket: a collect's or cover's space answer
/// (thousands of exclusions, possibly) is cut down to [`space::compact`] when
/// `compact_space` gives the match tolerance.
fn intent_json(i: &Intent, tasking_id: Option<u64>, compact_space: Option<u32>) -> Value {
    let mut v = serde_json::to_value(i).unwrap_or(Value::Null);
    if let Some(t) = tasking_id {
        v["tasking_id"] = json!(t);
    }
    if let Some(tol) = compact_space
        && let Some(s) = space::compact(i, tol)
    {
        v["space"] = s;
    }
    v
}

/// For an intent its submitter authorized at submission: who, which policy tier asked for
/// a human, and why (the inbox's reasons). The console notes it in the command log
/// ("authorized by you (armed platform)").
pub fn authorization_json(i: &Intent) -> Option<Value> {
    let a = i.approval().filter(|a| a.at_submission)?;
    let p = i.policy()?;
    Some(json!({
        "by": a.by,
        "at_submission": true,
        "self_approved": a.self_approved,
        "tier": p.rule,
        "reason": p.reason,
        "why": why_human(i),
    }))
}

/// Why an intent needs a human, for the inbox (and an at-submission authorization).
fn why_human(i: &Intent) -> Vec<String> {
    let mut why = Vec::new();
    if let Some(p) = i.policy() {
        why.push(format!("policy {}: {}", p.rule, p.reason));
        if matches!(i.spec().task, Task::Engage { .. }) {
            let who = match &i.spec().source {
                murmur_decide::Source::Human { id } => format!("not the proposer ({id})"),
                murmur_decide::Source::Agent { id } => format!("proposed by agent {id}"),
            };
            why.push(format!(
                "engagement: {} distinct approver(s) with engagement authority, {who}",
                p.approvers
            ));
        } else if p.approvers > 1 {
            why.push(format!("{} distinct approvers", p.approvers));
        }
        if !p.facts.armed.is_empty() {
            let armed: Vec<&str> = p.facts.armed.iter().map(DUid::as_str).collect();
            why.push(format!("armed platform: vehicle {}", armed.join(", ")));
        }
        if p.facts.leaves_approved_area {
            why.push(p.facts.area_detail.clone());
        }
        why.extend(p.facts.unverified.iter().cloned());
    }
    why
}

/// The engine's proposal for a request.
fn proposal(req: &IntentRequest) -> Result<Proposal, DecisionError> {
    let bad = |m: String| DecisionError::new(400, m);
    let kind: TaskKind = req.task.parse().map_err(bad)?;
    let p = &req.params;
    let ll = |a: [f64; 2]| LonLat::new(a[0], a[1]);
    let task = match kind {
        TaskKind::Orbit => Task::Orbit {
            center: ll(p
                .center
                .ok_or_else(|| bad("orbit needs center [lon, lat]".into()))?),
            radius_m: p.radius.unwrap_or(DEFAULT_ORBIT_RADIUS_M),
        },
        TaskKind::Patrol => Task::Patrol {
            route: p.route.iter().flatten().copied().map(ll).collect(),
        },
        TaskKind::Hold => Task::Hold {},
        TaskKind::Rtl => Task::Rtl {},
        TaskKind::Track => Task::Track {
            target: duid(
                p.target
                    .ok_or_else(|| bad("track needs a target vehicle".into()))?,
            ),
        },
    };
    let mut proposal = Proposal::new(task, req.uids.iter().map(|u| duid(*u)).collect());
    proposal.constraints = req.constraints.clone();
    proposal.rationale = req
        .rationale
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(Rationale::new)
        .transpose()
        .map_err(|e| bad(e.to_string()))?;
    proposal.validate().map_err(|e| bad(e.to_string()))?;
    Ok(proposal)
}

/// The task layer's request for an engine task (the inverse of [`proposal`]).
fn task_request(task: &Task, uids: &[DUid]) -> Result<TaskRequest, String> {
    let uid = |u: &DUid| bridge_uid(u).ok_or_else(|| format!("vehicle {u} has no bridge uid"));
    let uids = uids.iter().map(uid).collect::<Result<Vec<_>, _>>()?;
    let mut params = TaskParams::default();
    let name = match task {
        Task::Orbit { center, radius_m } => {
            params.center = Some([center.lon, center.lat]);
            params.radius = Some(*radius_m);
            "orbit"
        }
        Task::Patrol { route } => {
            params.route = Some(route.iter().map(|p| [p.lon, p.lat]).collect());
            "patrol"
        }
        Task::Hold {} => "hold",
        Task::Rtl {} => "rtl",
        Task::Track { target } => {
            params.target = Some(uid(target)?);
            "track"
        }
        Task::Collect { .. } | Task::Cover { .. } => return Err(SPACE_NOT_VEHICLES.into()),
        Task::Engage { .. } => {
            return Err("an engagement runs through its executor (decision::engage)".into());
        }
    };
    Ok(TaskRequest {
        task: name.into(),
        uids,
        params,
    })
}
