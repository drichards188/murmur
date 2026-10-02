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
//! **Operators.** Requests name their operator in the `X-Murmur-Operator` header, checked
//! against `MURMUR_OPERATORS` (comma-separated; default `op-1`). That is a placeholder for
//! real authentication: anyone who can reach the port can claim any listed id. The legacy
//! `POST /api/tasks` (the console, which sends no header) acts as the first listed operator.
//! Each operator gets one engine session, opened (and logged) on first use; nothing else
//! calls `Engine::open_operator_session`. There is no agent path yet (Phase F's MCP server
//! gets an `AgentPort`, never a session).
//!
//! **The loop** ([`Decision::run`], once a second): expire overdue approvals, write vehicle
//! events (mode changes, failsafes, STATUSTEXT) to the audit log, void waiting intents the
//! world invalidated, report each dispatched intent's per-vehicle op states and finish it
//! when every op has ended, then push the inbox to WebSocket clients if it changed.
//!
//! **Voiding.** A waiting intent (pending or approved, not yet dispatched) is voided when a
//! target vehicle goes silent for longer than its freshness limit, `rules.max_fact_age_s` or
//! `rules.fact_age_intervals` expected report intervals of its link if larger (60 s for an
//! acoustic AUV reporting every 20 s; its facts could no
//! longer support the decision), or when another intent is dispatched to one of its vehicles
//! (superseded: approving it later would silently undo the newer order).

mod alerts;
mod civil;
pub mod snapshot;
pub mod space;
mod terrain;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use murmur_decide::{
    AreaSet, AuditSink, Catalog, Clock, Constraints, Engine, EngineError, Intent, IntentId,
    JsonlFile, LonLat, OperatorId, OperatorSession, Order, Policy, Proposal, Rationale,
    ScenarioMap, SpaceSegment, State, Task, TaskOutcome, Time, Uid as DUid, rules,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::watch;

use self::snapshot::{bridge_uid, duid};
use self::terrain::Terrain;
use crate::clock;
use crate::tasks::{
    OpState, OrderError, TaskKind, TaskOrder, TaskParams, TaskRequest, Tasking, TaskingSnapshot,
};
use crate::vehicle::{Uid, VehicleCell};
use crate::world::World;

/// An orbit request without a radius gets this (as the task layer always flew it).
pub const DEFAULT_ORBIT_RADIUS_M: f64 = 200.0;
/// Collect and cover never reach the vehicle task layer.
const SPACE_NOT_VEHICLES: &str = "collect and cover request satellites: they run in the space executor, not the vehicle task layer";
/// The WebSocket inbox message lists this many recent intents.
const PUSH_INTENTS: usize = 50;

/// An order the engine released for an approved (or auto-approved) intent: the only thing
/// [`crate::tasks::TaskManager::dispatch`] accepts. Made only in this module, from the
/// `DispatchOrder` that `Engine::dispatch` returns.
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
    fn new(status: u16, detail: impl Into<String>) -> Self {
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
        | EngineError::ChecksFailedAtApproval(_)
        | EngineError::SpaceIntent(_)
        | EngineError::NotDispatchable { .. }
        | EngineError::NotACollect(_) => 409,
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
    pub operators: Vec<OperatorId>,
    pub tidewater: Option<String>,
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
        Settings {
            policy: var("MURMUR_POLICY")
                .map_or_else(|| root.join("sim/policy.toml"), PathBuf::from),
            catalog: catalog.to_path_buf(),
            areas: var("MURMUR_AREAS").map_or_else(|| root.join("sim/areas"), PathBuf::from),
            audit: root.join("logs/audit").join(format!("{run}.jsonl")),
            operators,
            tidewater: var("MURMUR_TIDEWATER_URL"),
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
    sessions: BTreeMap<OperatorId, OperatorSession>,
    running: BTreeMap<IntentId, Running>,
    /// every tasking an intent produced
    taskings: BTreeMap<IntentId, Arc<Tasking>>,
    /// dispatched collects and covers (running, and the last finished ones)
    space_runs: BTreeMap<IntentId, space::SpaceRun>,
}

pub struct Decision {
    world: Arc<World>,
    clock: SharedClock,
    catalog: Catalog,
    areas: AreaSet,
    operators: Vec<OperatorId>,
    terrain: Terrain,
    rules: murmur_decide::RulesConfig,
    audit_path: PathBuf,
    core: Mutex<Core>,
    updates: watch::Sender<Arc<String>>,
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
        let sink = JsonlFile::open(&s.audit).map_err(|e| format!("{}: {e}", s.audit.display()))?;
        if s.operators.is_empty() {
            return Err("MURMUR_OPERATORS lists no operator".into());
        }
        Decision::from_parts(
            world,
            Parts {
                policy,
                catalog,
                areas,
                sink: Box::new(sink),
                audit_path: s.audit.clone(),
                clock,
                terrain: Terrain::new(s.tidewater.as_deref()),
                operators: s.operators.clone(),
            },
        )
    }

    pub fn from_parts(world: Arc<World>, p: Parts) -> Result<Arc<Decision>, String> {
        let clock = SharedClock(p.clock);
        let rules = p.policy.rules.clone();
        let engine = Engine::with_areas(
            p.policy,
            Some(p.areas.clone()),
            Box::new(clock.clone()),
            p.sink,
        )
        .map_err(|e| e.to_string())?;
        let d = Arc::new(Decision {
            world,
            clock,
            catalog: p.catalog,
            areas: p.areas,
            operators: p.operators,
            terrain: p.terrain,
            rules,
            audit_path: p.audit_path,
            core: Mutex::new(Core {
                engine,
                sessions: BTreeMap::new(),
                running: BTreeMap::new(),
                taskings: BTreeMap::new(),
                space_runs: BTreeMap::new(),
            }),
            updates: watch::channel(Arc::new(String::new())).0,
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
            Ok(s) => s,
            Err(e) => return eprintln!("decision: space segment: {e}"),
        };
        if let Err(e) = self.lock().engine.set_space(Box::new(seg)) {
            eprintln!("decision: space segment: {e}");
        }
        self.publish();
    }

    fn lock(&self) -> MutexGuard<'_, Core> {
        self.core.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn fleet(&self) -> BTreeMap<Uid, Arc<VehicleCell>> {
        self.world
            .vehicles()
            .into_iter()
            .map(|v| (v.uid, v))
            .collect()
    }

    // ---- operators ------------------------------------------------------------------------

    /// The operator a request acts for. `header` is `X-Murmur-Operator`; `legacy` lets a
    /// request without one (the console's `POST /api/tasks`) act as the first listed operator.
    pub fn operator(
        &self,
        header: Option<&str>,
        legacy: bool,
    ) -> Result<OperatorId, DecisionError> {
        match header.map(str::trim).filter(|s| !s.is_empty()) {
            Some(h) => self
                .operators
                .iter()
                .find(|o| o.as_str() == h)
                .cloned()
                .ok_or_else(|| {
                    DecisionError::new(403, format!("operator {h:?} is not in MURMUR_OPERATORS"))
                }),
            None if legacy => self
                .operators
                .first()
                .cloned()
                .ok_or_else(|| DecisionError::new(500, "no operator configured")),
            None => Err(DecisionError::new(
                401,
                "set the X-Murmur-Operator header to an operator id (MURMUR_OPERATORS; a \
                 placeholder for real authentication)",
            )),
        }
    }

    /// That operator's session, opened (and logged) on first use.
    fn session<'a>(
        engine: &mut Engine,
        sessions: &'a mut BTreeMap<OperatorId, OperatorSession>,
        op: &OperatorId,
    ) -> Result<&'a OperatorSession, DecisionError> {
        if !sessions.contains_key(op) {
            let s = engine
                .open_operator_session(op.clone())
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
        by: &OperatorId,
        req: IntentRequest,
    ) -> Result<Outcome, DecisionError> {
        if matches!(req.task.as_str(), "collect" | "cover") {
            return self.submit_space(by, &req);
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
            let session = Self::session(&mut core.engine, &mut core.sessions, by)?;
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

    /// A human's collect or cover. The space query runs under the decision lock (SGP4 over
    /// the cached elements: well under a second for a collect at the scenario area, a few
    /// seconds for a day of Starlink coverage).
    fn submit_space(&self, by: &OperatorId, req: &IntentRequest) -> Result<Outcome, DecisionError> {
        let proposal = space::proposal(
            &req.task,
            !req.uids.is_empty(),
            &req.space,
            req.rationale.as_deref(),
        )?;
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            let world = self.snapshot();
            let session = Self::session(&mut core.engine, &mut core.sessions, by)?;
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
    /// picture no longer holds), then dispatch it.
    pub async fn approve(
        &self,
        by: &OperatorId,
        id: u64,
        note: Option<String>,
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
        self.prefetch(&task, &uids).await;
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            let world = self.snapshot();
            let session = Self::session(&mut core.engine, &mut core.sessions, by)?;
            match core.engine.approve(
                id,
                session.authority(),
                note,
                &world,
                &self.terrain.oracle(),
            ) {
                Ok(_) => self.after_decision(core, id),
                Err(e) => Err(self.with_intent(core, id, engine_err(e))),
            }
        };
        self.publish();
        out
    }

    pub fn reject(
        &self,
        by: &OperatorId,
        id: u64,
        note: Option<String>,
    ) -> Result<Outcome, DecisionError> {
        let id = IntentId(id);
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            let session = Self::session(&mut core.engine, &mut core.sessions, by)?;
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
        by: &OperatorId,
        id: u64,
        note: Option<String>,
    ) -> Result<Outcome, DecisionError> {
        let id = IntentId(id);
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            let session = Self::session(&mut core.engine, &mut core.sessions, by)?;
            let note = note.unwrap_or_else(|| "cancelled".into());
            match core.engine.cancel(id, session.authority(), &note) {
                Ok(_) => Ok(self.outcome(core, id)),
                Err(e) => Err(self.with_intent(core, id, engine_err(e))),
            }
        };
        self.publish();
        out
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
        // the task layer must accept it before the engine marks it dispatched
        if let Err(e) = task_request(&spec.task, &spec.uids)
            .and_then(|r| TaskOrder::new(r, &fleet).map_err(|e| e.to_string()))
        {
            core.engine
                .void(id, &format!("the task layer can't take it: {e}"))
                .map_err(engine_err)?;
            return Ok(());
        }
        let released = core.engine.dispatch(id).map_err(engine_err)?;
        let order = match task_request(&released.task, &released.uids)
            .and_then(|r| TaskOrder::new(r, &fleet).map_err(|e| e.to_string()))
        {
            Ok(o) => o,
            Err(e) => {
                core.engine
                    .finish(
                        id,
                        TaskOutcome::Failed {
                            detail: format!("the task layer refused the order: {e}"),
                        },
                    )
                    .map_err(engine_err)?;
                return Ok(());
            }
        };
        let tasking = self.world.tasks.dispatch(AuthorizedOrder {
            intent: released.intent,
            order,
        });
        core.taskings.insert(id, tasking.clone());
        core.running.insert(
            id,
            Running {
                tasking,
                reported: HashMap::new(),
            },
        );
        // older orders still waiting for these vehicles are superseded
        let took: BTreeSet<&DUid> = released.uids.iter().collect();
        let stale: Vec<(IntentId, DUid)> = core
            .engine
            .intents()
            .filter(|i| {
                i.id() != id && matches!(i.state(), State::PendingApproval | State::Approved)
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
        Ok(())
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
            Order::Vehicles(_) => {
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
        snapshot::build(&self.world, &self.catalog, &self.areas, self.clock.now())
    }

    /// Ask tidewater for the task's waypoints first, if some target must stay on water or
    /// land (the engine's oracle then answers from the cache).
    async fn prefetch(&self, task: &Task, uids: &[DUid]) {
        if !self.terrain.configured() {
            return;
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

    // ---- the loop -----------------------------------------------------------------------

    pub async fn run(self: Arc<Self>) {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            self.tick();
        }
    }

    pub fn tick(&self) {
        {
            let mut guard = self.lock();
            let core = &mut *guard;
            if let Err(e) = core.engine.expire_due() {
                eprintln!("decision: expire: {e}");
            }
            self.log_vehicle_events(core);
            self.void_lost(core);
            self.follow_ops(core);
            self.follow_space(core);
        }
        self.publish();
    }

    fn log_vehicle_events(&self, core: &mut Core) {
        for cell in self.world.vehicles() {
            let events: Vec<_> = cell.write(|v| v.events.drain(..).collect());
            for e in events {
                if let Err(err) = core
                    .engine
                    .record_vehicle_event(&duid(cell.uid), e.kind, &e.text)
                {
                    eprintln!("decision: vehicle event: {err}");
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

    /// Log each op's state changes (and NOT MOVING) as `op.state`; finish the intent when
    /// all its ops have ended: done if every vehicle got there, else failed with why.
    fn follow_ops(&self, core: &mut Core) {
        let Core {
            engine, running, ..
        } = core;
        let mut ended = Vec::new();
        for (id, run) in running.iter_mut() {
            let snap = run.tasking.snapshot();
            for op in &snap.ops {
                let key = (op.state, op.detail.starts_with("NOT MOVING"));
                if run.reported.get(&op.id) == Some(&key) {
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
        for (id, outcome) in ended {
            match engine.finish(id, outcome) {
                Ok(_) => {
                    running.remove(&id);
                }
                Err(e) => eprintln!("decision: finish {id}: {e}"),
            }
        }
    }

    // ---- reading ------------------------------------------------------------------------

    /// `GET /api/intents`: newest first.
    pub fn intents_json(&self) -> Value {
        let core = self.lock();
        let all: Vec<&Intent> = core.engine.intents().collect();
        let intents: Vec<Value> = all
            .into_iter()
            .rev()
            .map(|i| intent_json(i, core.taskings.get(&i.id()).map(|t| t.id), self.tol()))
            .collect();
        json!({
            "intents": intents,
            "audit": { "path": self.audit_path, "head": core.engine.head_hash() },
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
        let pending: Vec<Value> = {
            let core = self.lock();
            core.engine
                .inbox()
                .into_iter()
                .map(|i| {
                    let mut v = intent_json(i, None, self.tol());
                    v["why"] = json!(why_human(i));
                    v
                })
                .collect()
        };
        let alerts = alerts::alerts(
            &self.world.vehicles(),
            &self.world.tasks.current_ops(),
            clock::wall(),
        );
        json!({
            "pending": pending,
            "alerts": alerts,
            "counts": { "pending": pending.len(), "alerts": alerts.len() },
        })
    }

    /// The WebSocket message (`type: "decisions"`): the inbox and the latest intents.
    fn message(&self) -> String {
        let inbox = self.inbox_json();
        let intents: Vec<Value> = {
            let core = self.lock();
            let all: Vec<&Intent> = core.engine.intents().collect();
            all.into_iter()
                .rev()
                .take(PUSH_INTENTS)
                .map(|i| intent_json(i, core.taskings.get(&i.id()).map(|t| t.id), self.tol()))
                .collect()
        };
        let space = self.space_runs_json();
        json!({ "type": "decisions", "inbox": inbox, "intents": intents, "space": space })
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
    };
    Ok(TaskRequest {
        task: name.into(),
        uids,
        params,
    })
}
