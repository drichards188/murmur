//! The engine: owns the intents, applies rules and policy, enforces the state machine, and
//! writes every step to the audit log before taking it.

use std::collections::{BTreeMap, BTreeSet};

use crate::areas::AreaSet;
use crate::audit::{AuditSink, CheckPhase, Event, EventBody, LoggedAreas, OperatorView};
use crate::authority::{HumanAuthority, OperatorSession};
use crate::hash::{GENESIS, sha256_hex};
use crate::ids::{AgentId, IntentId, OperatorId, SessionId, Uid};
use crate::intent::{
    Approval, IllegalTransition, Intent, IntentSpec, Proposal, ProposalError, Rationale, Source,
    State, Task, TaskKind, Transition, next_state,
};
use crate::policy::{self, Policy, PolicyDecision, Tier};
use crate::rules::{self, CheckResult, Outcome};
use serde::Serialize;
use crate::space::{
    ChosenCollect, CollectOrder, CoverOrder, ScenarioMap, SegmentId, SpaceOracle, SpaceOrder,
    from_utc, matched_option, query_for,
};
use crate::terrain::TerrainOracle;
use crate::time::{Clock, Time};
use crate::world::{LoggedInputs, SpaceInputs, WorldSnapshot};

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("unknown intent {0}")]
    UnknownIntent(IntentId),
    #[error(transparent)]
    Proposal(#[from] ProposalError),
    #[error("intent {id}: {err}")]
    Illegal {
        id: IntentId,
        err: IllegalTransition,
    },
    #[error("intent {0} expired before it was approved")]
    Expired(IntentId),
    #[error("intent {0} failed its checks against the current world at approval and was voided")]
    ChecksFailedAtApproval(IntentId),
    #[error("agent {agent} can't withdraw intent {id}: not its own")]
    NotOwner { agent: AgentId, id: IntentId },
    #[error("audit log write failed, nothing changed: {0}")]
    Audit(#[from] std::io::Error),
    #[error("audit event failed to serialize: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("intent {0} requests satellites, not vehicles: dispatch it with dispatch_order")]
    SpaceIntent(IntentId),
    #[error("intent {id} can't be dispatched: {why}")]
    NotDispatchable { id: IntentId, why: String },
    #[error("intent {0} is not a collect")]
    NotACollect(IntentId),
}

/// What the task layer should execute for an approved intent.
#[derive(Clone, Debug, PartialEq)]
pub struct DispatchOrder {
    pub intent: IntentId,
    pub task: Task,
    pub uids: Vec<Uid>,
}

/// What [`Engine::dispatch_order`] hands the bridge: vehicle work for the task manager, or
/// a simulated satellite request (collect) or service watch (cover).
#[derive(Clone, Debug, PartialEq)]
pub enum Order {
    Vehicles(DispatchOrder),
    Collect(CollectOrder),
    Cover(CoverOrder),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskOutcome {
    Done,
    Failed { detail: String },
}

pub struct Engine {
    policy: Policy,
    clock: Box<dyn Clock + Send>,
    sink: Box<dyn AuditSink + Send>,
    seq: u64,
    last_t: Time,
    /// SHA-256 of the last line written: the next event's `prev`.
    head: String,
    /// The area set last logged (in `run.started` or `areas.loaded`) and its hash.
    areas: Option<LoggedAreas>,
    next_intent: u64,
    next_session: u64,
    intents: BTreeMap<IntentId, Intent>,
    /// The space segment collect and cover intents query (none: they fail closed).
    space: Option<Box<dyn SpaceOracle + Send>>,
    /// Space answers already logged as `space.query`, by hash.
    logged_space: BTreeSet<String>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("seq", &self.seq)
            .field("intents", &self.intents.len())
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Start a run: logs `run.started` with the full policy. The area set is logged (as
    /// `areas.loaded`) the first time a decision reads one; [`Engine::with_areas`] logs it
    /// in `run.started` instead.
    pub fn new(
        policy: Policy,
        clock: Box<dyn Clock + Send>,
        sink: Box<dyn AuditSink + Send>,
    ) -> Result<Self, EngineError> {
        Self::with_areas(policy, None, clock, sink)
    }

    /// Start a run with the area set known up front: `run.started` carries the policy and
    /// the area set with its content hash. Decisions reference the hash; if a snapshot
    /// later brings a different set, it is logged once as `areas.loaded`.
    pub fn with_areas(
        policy: Policy,
        areas: Option<AreaSet>,
        clock: Box<dyn Clock + Send>,
        sink: Box<dyn AuditSink + Send>,
    ) -> Result<Self, EngineError> {
        let areas = match areas {
            Some(a) => Some(LoggedAreas {
                sha256: a.content_hash()?,
                areas: a,
            }),
            None => None,
        };
        let mut e = Engine {
            policy,
            clock,
            sink,
            seq: 0,
            last_t: Time(i64::MIN),
            head: GENESIS.to_string(),
            areas: None,
            next_intent: 1,
            next_session: 1,
            intents: BTreeMap::new(),
            space: None,
            logged_space: BTreeSet::new(),
        };
        let policy = e.policy.clone();
        e.emit(EventBody::RunStarted {
            crate_version: env!("CARGO_PKG_VERSION").to_string(),
            policy,
            areas: areas.clone(),
        })?;
        e.areas = areas;
        Ok(e)
    }

    /// SHA-256 of the last event written: the head of the log's hash chain. Publishing it
    /// somewhere the log writer can't rewrite (future work) is what turns the chain from
    /// tamper-evident into evidence against a full rewrite or a truncated tail.
    pub fn head_hash(&self) -> &str {
        &self.head
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Give the engine a space segment for collect and cover intents (logged as
    /// `space.loaded` with its identity). Without one, their space check fails closed.
    pub fn set_space(&mut self, oracle: Box<dyn SpaceOracle + Send>) -> Result<(), EngineError> {
        let map = oracle.scenario_map();
        self.emit(EventBody::SpaceLoaded {
            segment: oracle.segment().clone(),
            scenario: (!map.is_identity()).then_some(map),
            pinned_shift_days: oracle.pinned_shift_days(),
        })?;
        self.space = Some(oracle);
        Ok(())
    }

    /// The loaded space segment's identity, if any.
    pub fn space_segment(&self) -> Option<&SegmentId> {
        self.space.as_ref().map(|o| o.segment())
    }

    /// How scenario time follows engine time (identity without a space segment).
    pub fn scenario_map(&self) -> ScenarioMap {
        self.space
            .as_ref()
            .map_or(ScenarioMap::IDENTITY, |o| o.scenario_map())
    }

    pub fn intent(&self, id: IntentId) -> Option<&Intent> {
        self.intents.get(&id)
    }

    pub fn intents(&self) -> impl Iterator<Item = &Intent> {
        self.intents.values()
    }

    /// Pending approvals, soonest expiry first.
    pub fn inbox(&self) -> Vec<&Intent> {
        let mut v: Vec<_> = self
            .intents
            .values()
            .filter(|i| i.state() == State::PendingApproval)
            .collect();
        v.sort_by_key(|i| (i.expires_t(), i.id()));
        v
    }

    // ---- operator side --------------------------------------------------------------

    /// The one place a [`HumanAuthority`] comes from. The bridge calls this from its
    /// authenticated operator endpoint only. Logged as `operator.action session.opened`.
    pub fn open_operator_session(
        &mut self,
        operator: OperatorId,
    ) -> Result<OperatorSession, EngineError> {
        let session = SessionId(self.next_session);
        self.emit(EventBody::OperatorAction {
            operator: operator.clone(),
            session,
            action: "session.opened".into(),
            detail: String::new(),
        })?;
        self.next_session += 1;
        Ok(OperatorSession::mint(operator, session))
    }

    /// A human's intent (also what `POST /api/tasks` becomes). If its checks pass and policy
    /// routes it to a human tier that lets the submitter authorize
    /// ([`crate::PolicyDecision::submitter_authorizes`]), the submission is the human
    /// decision: it goes straight to `approved` (`approval.at_submission`, approval
    /// `{by: this operator, at_submission: true, self_approved: true}`). Otherwise it waits
    /// in the inbox like any other.
    pub fn submit(
        &mut self,
        by: &HumanAuthority,
        proposal: Proposal,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        let source = Source::Human {
            id: by.operator().clone(),
        };
        let id = self.submit_inner(source, Some(by), proposal, world, terrain)?;
        self.get(id)
    }

    /// Approve a pending intent. Re-runs the checks against the current world first: an
    /// approval of a picture that no longer holds is void.
    pub fn approve(
        &mut self,
        id: IntentId,
        by: &HumanAuthority,
        note: Option<String>,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        let intent = self.get(id)?;
        next_state(intent.state(), Transition::Approve)
            .map_err(|err| EngineError::Illegal { id, err })?;
        let spec = intent.spec().clone();
        let expires = intent.expires_t();
        let now = self.now();
        if expires.is_some_and(|e| now >= e) {
            self.expire(id)?;
            return Err(EngineError::Expired(id));
        }
        let (_, _, results, ok) = self.run_checks(&spec, CheckPhase::Approve, world, terrain)?;
        if !ok {
            let reason = format!(
                "checks failed at approval: {}",
                summarize_failures(&results)
            );
            self.transition(id, Transition::Void, reason)?;
            return Err(EngineError::ChecksFailedAtApproval(id));
        }
        let self_approved = is_self_approval(&spec.source, by.operator());
        let t = self.emit_at(
            now,
            EventBody::ApprovalGranted {
                intent: id,
                by: by.operator().clone(),
                session: by.session(),
                self_approved,
                note: note.clone(),
            },
        )?;
        self.get_mut(id)?.set_approval(Approval {
            by: by.operator().clone(),
            session: by.session(),
            t,
            self_approved,
            at_submission: false,
            note,
        });
        let reason = if self_approved {
            format!(
                "approved by {} (self-approved: also the proposer)",
                by.operator()
            )
        } else {
            format!("approved by {}", by.operator())
        };
        self.transition(id, Transition::Approve, reason)?;
        self.get(id)
    }

    pub fn reject(
        &mut self,
        id: IntentId,
        by: &HumanAuthority,
        note: Option<String>,
    ) -> Result<&Intent, EngineError> {
        next_state(self.get(id)?.state(), Transition::Reject)
            .map_err(|err| EngineError::Illegal { id, err })?;
        self.emit(EventBody::ApprovalRejected {
            intent: id,
            by: by.operator().clone(),
            session: by.session(),
            note,
        })?;
        self.transition(
            id,
            Transition::Reject,
            format!("rejected by {}", by.operator()),
        )?;
        self.get(id)
    }

    /// Commit a collect to one of its options: a new collect intent with the same request
    /// and `chosen`, proposed by this operator and checked and routed like any other. The
    /// old intent is voided if it is still open (a query intent, the usual case, is
    /// already terminal: rejected by its feasibility check). Logged as
    /// `operator.action intent.amend`.
    pub fn amend_collect(
        &mut self,
        by: &HumanAuthority,
        id: IntentId,
        chosen: ChosenCollect,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        let (proposal, old_state) = self.amended(id, chosen)?;
        let new_id = IntentId(self.next_intent);
        self.emit(EventBody::OperatorAction {
            operator: by.operator().clone(),
            session: by.session(),
            action: "intent.amend".into(),
            detail: amend_detail(id, new_id, &chosen),
        })?;
        let source = Source::Human {
            id: by.operator().clone(),
        };
        let new_id = self.submit_inner(source, Some(by), proposal, world, terrain)?;
        if next_state(old_state, Transition::Void).is_ok() {
            self.transition(id, Transition::Void, format!("superseded by {new_id}"))?;
        }
        self.get(new_id)
    }

    /// An operator withdraws an intent before dispatch (pending or approved).
    pub fn cancel(
        &mut self,
        id: IntentId,
        by: &HumanAuthority,
        note: &str,
    ) -> Result<&Intent, EngineError> {
        next_state(self.get(id)?.state(), Transition::Void)
            .map_err(|err| EngineError::Illegal { id, err })?;
        self.emit(EventBody::OperatorAction {
            operator: by.operator().clone(),
            session: by.session(),
            action: "intent.cancel".into(),
            detail: format!("{id}: {note}"),
        })?;
        self.transition(
            id,
            Transition::Void,
            format!("cancelled by {}: {note}", by.operator()),
        )?;
        self.get(id)
    }

    /// Log something an operator did that isn't an intent decision (acknowledge, snooze).
    pub fn record_operator_action(
        &mut self,
        by: &HumanAuthority,
        action: &str,
        detail: &str,
    ) -> Result<(), EngineError> {
        self.emit(EventBody::OperatorAction {
            operator: by.operator().clone(),
            session: by.session(),
            action: action.into(),
            detail: detail.into(),
        })?;
        Ok(())
    }

    /// Log what an operator's console showed for an intent (`operator.view`, Phase G):
    /// the card opened, closed, or a decision button was clicked, with what was visible.
    /// Decides nothing; replay ignores it. The intent must exist and the view be
    /// well-formed ([`OperatorView::validate`]).
    pub fn record_operator_view(
        &mut self,
        by: &HumanAuthority,
        intent: IntentId,
        view: OperatorView,
    ) -> Result<(), EngineError> {
        self.get(intent)?;
        view.validate()
            .map_err(|m| EngineError::Proposal(ProposalError::BadParams(m)))?;
        self.emit(EventBody::OperatorView {
            operator: by.operator().clone(),
            session: by.session(),
            intent,
            view,
        })?;
        Ok(())
    }

    // ---- agent side -----------------------------------------------------------------

    /// The agent-facing surface: propose, withdraw its own, read. No path to approval.
    pub fn agent_port(&mut self, agent: AgentId) -> AgentPort<'_> {
        AgentPort {
            engine: self,
            agent,
        }
    }

    // ---- system side ----------------------------------------------------------------

    /// The bridge voids an intent whose basis no longer holds (vehicle lost, superseded).
    /// Anyone can stop an action; only a human can authorize one.
    pub fn void(&mut self, id: IntentId, reason: &str) -> Result<&Intent, EngineError> {
        self.transition(id, Transition::Void, format!("system: {reason}"))?;
        self.get(id)
    }

    /// Expire every pending approval past its deadline. Call periodically.
    pub fn expire_due(&mut self) -> Result<Vec<IntentId>, EngineError> {
        let now = self.now();
        let due: Vec<IntentId> = self
            .intents
            .values()
            .filter(|i| {
                i.state() == State::PendingApproval && i.expires_t().is_some_and(|e| now >= e)
            })
            .map(Intent::id)
            .collect();
        for id in &due {
            self.expire(*id)?;
        }
        Ok(due)
    }

    /// Hand an approved (or auto-approved) vehicle intent to the task layer. A collect or
    /// cover is refused ([`EngineError::SpaceIntent`], nothing changes): it has no vehicles,
    /// so use [`Engine::dispatch_order`].
    pub fn dispatch(&mut self, id: IntentId) -> Result<DispatchOrder, EngineError> {
        if self.get(id)?.spec().task.is_space() {
            return Err(EngineError::SpaceIntent(id));
        }
        match self.dispatch_order(id)? {
            Order::Vehicles(o) => Ok(o),
            Order::Collect(_) | Order::Cover(_) => Err(EngineError::SpaceIntent(id)),
        }
    }

    /// Hand any approved (or auto-approved) intent on: a [`DispatchOrder`] for vehicle
    /// tasks, a [`CollectOrder`] (the chosen option, as the last check found it) or a
    /// [`CoverOrder`] for space tasks. Logged as `intent.dispatched` (with `space` for
    /// collect and cover).
    pub fn dispatch_order(&mut self, id: IntentId) -> Result<Order, EngineError> {
        let intent = self.get(id)?;
        next_state(intent.state(), Transition::Dispatch)
            .map_err(|err| EngineError::Illegal { id, err })?;
        let spec = intent.spec();
        let order = match &spec.task {
            Task::Collect { area, chosen, .. } => {
                let not = |why: &str| EngineError::NotDispatchable {
                    id,
                    why: why.to_string(),
                };
                let chosen = chosen.ok_or_else(|| not("no option chosen"))?;
                let answer = intent
                    .space_answer()
                    .ok_or_else(|| not("no space answer recorded"))?;
                let result = answer
                    .tasking()
                    .ok_or_else(|| not("the space answer is not a tasking answer"))?;
                let tol = self.policy.rules.space_rules().collect_match_tolerance_s;
                let opt = matched_option(result, &chosen, tol)
                    .ok_or_else(|| not("the chosen option is not in the last answer"))?;
                let product_time = opt.product_time.map(|p| {
                    chosen
                        .collect_time
                        .plus_ms(from_utc(p).ms_since(from_utc(opt.collect_time)))
                });
                Order::Collect(CollectOrder {
                    intent: id,
                    resource: Uid::new(format!("norad:{}", chosen.norad_id)),
                    norad_id: chosen.norad_id,
                    satellite: opt.satellite.clone(),
                    class_id: opt.class_id.clone(),
                    capability: opt.capability,
                    collect_time: chosen.collect_time,
                    product_time,
                    resolution_m: opt.resolution_m,
                    area: area.clone(),
                    scene: opt
                        .scene
                        .as_ref()
                        .map(|r| r.iter().map(|p| (*p).into()).collect()),
                    answer_sha256: answer.content_hash()?,
                })
            }
            Task::Cover {
                area,
                window,
                service,
                ..
            } => {
                let answer = intent
                    .space_answer()
                    .ok_or_else(|| EngineError::NotDispatchable {
                        id,
                        why: "no space answer recorded".into(),
                    })?;
                Order::Cover(CoverOrder {
                    intent: id,
                    resource: service.resource(),
                    service: service.clone(),
                    area: area.clone(),
                    window: *window,
                    answer_sha256: answer.content_hash()?,
                })
            }
            _ => Order::Vehicles(DispatchOrder {
                intent: id,
                task: spec.task.clone(),
                uids: spec.uids.clone(),
            }),
        };
        let (task, uids) = (spec.task.clone(), spec.uids.clone());
        let space = match &order {
            Order::Vehicles(_) => None,
            Order::Collect(c) => Some(SpaceOrder::Collect(c.clone())),
            Order::Cover(c) => Some(SpaceOrder::Cover(c.clone())),
        };
        let reason = match &order {
            Order::Vehicles(_) => "dispatched to the task layer",
            Order::Collect(_) => "dispatched: satellite tasking request",
            Order::Cover(_) => "dispatched: coverage watch",
        };
        self.emit(EventBody::IntentDispatched {
            intent: id,
            task,
            uids,
            space,
        })?;
        self.transition(id, Transition::Dispatch, reason.into())?;
        Ok(order)
    }

    pub fn record_op_state(
        &mut self,
        id: IntentId,
        uid: &Uid,
        state: &str,
        detail: &str,
    ) -> Result<(), EngineError> {
        self.get(id)?;
        self.emit(EventBody::OpState {
            intent: id,
            uid: uid.clone(),
            state: state.into(),
            detail: detail.into(),
        })?;
        Ok(())
    }

    pub fn finish(&mut self, id: IntentId, outcome: TaskOutcome) -> Result<&Intent, EngineError> {
        let (tr, reason) = match outcome {
            TaskOutcome::Done => (Transition::Complete, "done".to_string()),
            TaskOutcome::Failed { detail } => (Transition::Fail, detail),
        };
        self.transition(id, tr, reason)?;
        self.get(id)
    }

    pub fn record_vehicle_event(
        &mut self,
        uid: &Uid,
        kind: &str,
        text: &str,
    ) -> Result<(), EngineError> {
        self.emit(EventBody::VehicleEvent {
            uid: uid.clone(),
            kind: kind.into(),
            text: text.into(),
        })?;
        Ok(())
    }

    // ---- internals ------------------------------------------------------------------

    fn get(&self, id: IntentId) -> Result<&Intent, EngineError> {
        self.intents.get(&id).ok_or(EngineError::UnknownIntent(id))
    }

    fn get_mut(&mut self, id: IntentId) -> Result<&mut Intent, EngineError> {
        self.intents
            .get_mut(&id)
            .ok_or(EngineError::UnknownIntent(id))
    }

    /// Clock time, never earlier than the last logged event.
    fn now(&self) -> Time {
        self.clock.now().max(self.last_t)
    }

    fn emit(&mut self, body: EventBody) -> Result<Time, EngineError> {
        let now = self.now();
        self.emit_at(now, body)
    }

    fn emit_at(&mut self, t: Time, body: EventBody) -> Result<Time, EngineError> {
        let t = t.max(self.last_t);
        let event = Event {
            seq: self.seq + 1,
            t,
            prev: self.head.clone(),
            body,
        };
        let line = serde_json::to_string(&event)?;
        self.sink.append(&line)?;
        self.seq += 1;
        self.last_t = t;
        self.head = sha256_hex(line.as_bytes());
        Ok(t)
    }

    /// The decision inputs as logged: the area set and any space answer replaced by their
    /// hashes. Logs the answer as `space.query`, and the set as `areas.loaded`, first if
    /// they haven't been logged yet.
    fn log_inputs(&mut self, inputs: &WorldSnapshot) -> Result<LoggedInputs, EngineError> {
        if let SpaceInputs::Answered { sha256, answer } = &inputs.space
            && !self.logged_space.contains(sha256)
        {
            self.emit(EventBody::SpaceQuery {
                sha256: sha256.clone(),
                answer: answer.clone(),
            })?;
            self.logged_space.insert(sha256.clone());
        }
        let Some(set) = &inputs.areas else {
            return Ok(LoggedInputs::from_world(inputs, None));
        };
        if let Some(cur) = &self.areas
            && cur.areas == *set
        {
            return Ok(LoggedInputs::from_world(inputs, Some(cur.sha256.clone())));
        }
        let sha256 = set.content_hash()?;
        self.emit(EventBody::AreasLoaded {
            sha256: sha256.clone(),
            areas: set.clone(),
        })?;
        self.areas = Some(LoggedAreas {
            sha256: sha256.clone(),
            areas: set.clone(),
        });
        Ok(LoggedInputs::from_world(inputs, Some(sha256)))
    }

    fn transition(
        &mut self,
        id: IntentId,
        transition: Transition,
        reason: String,
    ) -> Result<State, EngineError> {
        let from = self.get(id)?.state();
        let to = next_state(from, transition).map_err(|err| EngineError::Illegal { id, err })?;
        let t = self.emit(EventBody::IntentState {
            intent: id,
            transition,
            from,
            to,
            reason: reason.clone(),
        })?;
        self.get_mut(id)?
            .apply(transition, t, reason)
            .map_err(|err| EngineError::Illegal { id, err })
    }

    fn expire(&mut self, id: IntentId) -> Result<(), EngineError> {
        let expires_t = self.get(id)?.expires_t().unwrap_or(Time(0));
        next_state(self.get(id)?.state(), Transition::Expire)
            .map_err(|err| EngineError::Illegal { id, err })?;
        self.emit(EventBody::ApprovalExpired {
            intent: id,
            expires_t,
        })?;
        self.transition(
            id,
            Transition::Expire,
            "no decision before the deadline".into(),
        )?;
        Ok(())
    }

    fn run_checks(
        &mut self,
        spec: &IntentSpec,
        phase: CheckPhase,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<(WorldSnapshot, LoggedInputs, Vec<CheckResult>, bool), EngineError> {
        let mut inputs = world.for_intent(spec, terrain);
        if spec.task.is_space() {
            inputs.space = self.ask_space(&spec.task, inputs.t)?;
        }
        let results = rules::evaluate(spec, &inputs, &self.policy.rules);
        let passed = rules::passed(&results, spec.kind());
        let logged = self.log_inputs(&inputs)?;
        self.emit(EventBody::CheckResult {
            intent: spec.id,
            phase,
            inputs: logged.clone(),
            results: results.clone(),
            passed,
        })?;
        let intent = self.get_mut(spec.id)?;
        intent.set_checks(results.clone());
        if spec.task.is_space() {
            intent.set_space(inputs.space.answer().map(|a| Box::new(a.clone())));
        }
        Ok((inputs, logged, results, passed))
    }

    /// Ask the space segment this task's question at snapshot time `t`. Failures become
    /// `Unavailable` (the space rule then fails, saying why); only a hashing error is an
    /// engine error.
    fn ask_space(&self, task: &Task, t: Time) -> Result<SpaceInputs, EngineError> {
        let Some(oracle) = &self.space else {
            return Ok(SpaceInputs::Unavailable {
                reason: "no space segment loaded".into(),
            });
        };
        let now = oracle.scenario_time(t);
        let query = match query_for(task, now, &self.policy.rules.space_rules()) {
            Some(Ok(q)) => q,
            Some(Err(reason)) => return Ok(SpaceInputs::Unavailable { reason }),
            None => return Ok(SpaceInputs::NotQueried),
        };
        Ok(match oracle.answer(&query) {
            Ok(answer) => SpaceInputs::Answered {
                sha256: answer.content_hash()?,
                answer: Box::new(answer),
            },
            Err(reason) => SpaceInputs::Unavailable { reason },
        })
    }

    /// The proposal that commits collect `id` to `chosen`, and `id`'s current state.
    fn amended(
        &self,
        id: IntentId,
        chosen: ChosenCollect,
    ) -> Result<(Proposal, State), EngineError> {
        let old = self.get(id)?;
        let task = old
            .spec()
            .task
            .with_chosen(chosen)
            .ok_or(EngineError::NotACollect(id))?;
        let proposal = Proposal {
            task,
            uids: Vec::new(),
            constraints: old.spec().constraints.clone(),
            rationale: old.spec().rationale.clone(),
        };
        proposal.validate()?;
        Ok((proposal, old.state()))
    }

    /// Create, check and route an intent. `submitter` is the submitting human's authority on
    /// the human paths and `None` on the agent paths: an at-submission approval needs it (and
    /// needs it to be the intent's own source), so an agent's proposal can never get one.
    fn submit_inner(
        &mut self,
        source: Source,
        submitter: Option<&HumanAuthority>,
        proposal: Proposal,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<IntentId, EngineError> {
        proposal.validate()?;
        let id = IntentId(self.next_intent);
        let spec = IntentSpec {
            id,
            created_t: self.now(),
            source,
            task: proposal.task,
            uids: proposal.uids,
            constraints: proposal.constraints,
            rationale: proposal.rationale,
        };
        self.emit(EventBody::IntentCreated {
            intent: spec.clone(),
        })?;
        self.next_intent += 1;
        self.intents.insert(id, Intent::new(spec.clone()));

        let (inputs, logged, results, ok) =
            self.run_checks(&spec, CheckPhase::Submit, world, terrain)?;
        self.transition(id, Transition::Check, "rules evaluated".into())?;
        if !ok {
            self.transition(id, Transition::ChecksFailed, summarize_failures(&results))?;
            return Ok(id);
        }

        // Policy decides on the same inputs the rules saw (one terrain query, one picture).
        let decision = policy::decide(&spec, &inputs, &self.policy);
        self.emit(EventBody::PolicyDecision {
            intent: id,
            inputs: logged,
            decision: decision.clone(),
        })?;
        let tier = decision.tier;
        let reason = format!("policy {}: {}", decision.rule, decision.reason);
        // The submission is the human decision only for the submitter's own intent, with
        // the submitter's authority in hand (never on an agent path, where it is None).
        let at_submission = (decision.submitter_authorizes && tier == Tier::Human)
            .then_some(submitter)
            .flatten()
            .filter(|by| is_self_approval(&spec.source, by.operator()));
        let rule = decision.rule.clone();
        self.get_mut(id)?.set_policy(decision);
        match (tier, at_submission) {
            (Tier::Auto, _) => {
                self.transition(id, Transition::AutoApprove, reason)?;
            }
            (Tier::Human, Some(by)) => {
                let t = self.emit(EventBody::ApprovalAtSubmission {
                    intent: id,
                    by: by.operator().clone(),
                    session: by.session(),
                    self_approved: true,
                    rule,
                })?;
                self.get_mut(id)?.set_approval(Approval {
                    by: by.operator().clone(),
                    session: by.session(),
                    t,
                    self_approved: true,
                    at_submission: true,
                    note: None,
                });
                self.transition(
                    id,
                    Transition::AuthorizeAtSubmission,
                    format!("{reason}; authorized at submission by {}", by.operator()),
                )?;
            }
            (Tier::Human, None) => {
                // one instant for the deadline and the event, so replay recomputes it from e.t
                let now = self.now();
                let (expires_t, until) = policy::approval_deadline(
                    &self.policy,
                    &spec.task,
                    inputs.space.answer(),
                    &self.scenario_map(),
                    now,
                );
                self.emit_at(
                    now,
                    EventBody::ApprovalRequested {
                        intent: id,
                        expires_t,
                        until,
                    },
                )?;
                self.get_mut(id)?.set_expires(expires_t);
                self.transition(id, Transition::RequireApproval, reason)?;
            }
        }
        Ok(id)
    }
}

/// The approving operator is the human who proposed the intent.
pub(crate) fn is_self_approval(source: &Source, approver: &OperatorId) -> bool {
    matches!(source, Source::Human { id } if id == approver)
}

fn amend_detail(old: IntentId, new: IntentId, chosen: &ChosenCollect) -> String {
    format!(
        "{old} -> {new}: NORAD {} at {}",
        chosen.norad_id,
        crate::space::fmt_time(chosen.collect_time)
    )
}

fn summarize_failures(results: &[CheckResult]) -> String {
    results
        .iter()
        .filter(|r| r.outcome == Outcome::Fail)
        .map(|r| match &r.uid {
            Some(u) => format!("{} {u}: {}", r.rule.as_str(), r.detail),
            None => format!("{}: {}", r.rule.as_str(), r.detail),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// What a dry run of a proposal would decide ([`AgentPort::preview`]): the checks, the
/// policy decision and the state the intent would reach if proposed now. Nothing is
/// created; the engine logs the preview as `agent.action intent.preview`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Preview {
    pub kind: TaskKind,
    pub uids: Vec<Uid>,
    pub checks: Vec<CheckResult>,
    pub passed: bool,
    /// Absent when the checks fail (policy never runs on a rejected intent).
    pub policy: Option<PolicyDecision>,
    /// `rejected`, `auto_approved` or `pending_approval`: never `approved`, because an
    /// agent's proposal in a human tier waits for a human.
    pub would_reach: State,
    /// Collect and cover: the space segment's answer the checks read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space: Option<Box<crate::space::SpaceAnswer>>,
}

/// What agent code is given (Phase F's MCP server holds one per agent). It can propose
/// (with a mandatory [`Rationale`]), preview a proposal (the checks and policy without an
/// intent), withdraw its own intents, note what it did, and read. It has no method that
/// approves, rejects on a human's behalf, or yields an operator session, and its engine
/// reference is private.
///
/// ```compile_fail,E0599
/// use murmur_decide::{AgentPort, IntentId};
/// fn agent_code(port: &mut AgentPort<'_>) {
///     port.approve(IntentId(1)); // no such method
/// }
/// ```
///
/// ```compile_fail,E0616
/// use murmur_decide::{AgentPort, OperatorId};
/// fn agent_code(port: &mut AgentPort<'_>) {
///     // reaching through to the engine to open an operator session: field is private
///     let _ = port.engine.open_operator_session(OperatorId::new("me"));
/// }
/// ```
///
/// ```compile_fail,E0308
/// use murmur_decide::{AgentId, Engine, IntentId, NoTerrain, WorldSnapshot};
/// fn agent_code(engine: &mut Engine, me: &AgentId, world: &WorldSnapshot) {
///     // an agent identity is not an authority
///     let _ = engine.approve(IntentId(1), me, None, world, &NoTerrain);
/// }
/// ```
pub struct AgentPort<'a> {
    engine: &'a mut Engine,
    agent: AgentId,
}

impl AgentPort<'_> {
    pub fn agent(&self) -> &AgentId {
        &self.agent
    }

    /// Nominate an action. Policy decides whether it runs on its own or waits for a human.
    /// Logged as `agent.action intent.propose` (the kind, the vehicles and the rationale)
    /// before the intent's own events.
    pub fn propose(
        &mut self,
        mut proposal: Proposal,
        rationale: Rationale,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        proposal.validate()?;
        self.engine.emit(EventBody::AgentAction {
            agent: self.agent.clone(),
            action: "intent.propose".into(),
            detail: format!(
                "{}: {}",
                describe(&proposal.task, &proposal.uids),
                rationale.as_str()
            ),
        })?;
        proposal.rationale = Some(rationale);
        let source = Source::Agent {
            id: self.agent.clone(),
        };
        let id = self
            .engine
            .submit_inner(source, None, proposal, world, terrain)?;
        self.engine.get(id)
    }

    /// A dry run: the checks and the policy decision a proposal would get right now,
    /// without creating an intent (nothing to withdraw, nothing in the inbox). The same
    /// rules and policy run on the same world view the real submission would read, so the
    /// answer is what [`AgentPort::propose`] would do a moment later, as long as the world
    /// holds still. Logged as `agent.action intent.preview`; the decision inputs are not
    /// logged (there is no decision to replay).
    pub fn preview(
        &mut self,
        proposal: Proposal,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<Preview, EngineError> {
        proposal.validate()?;
        let spec = IntentSpec {
            id: IntentId(0),
            created_t: self.engine.now(),
            source: Source::Agent {
                id: self.agent.clone(),
            },
            task: proposal.task,
            uids: proposal.uids,
            constraints: proposal.constraints,
            rationale: proposal.rationale,
        };
        let mut inputs = world.for_intent(&spec, terrain);
        if spec.task.is_space() {
            inputs.space = self.engine.ask_space(&spec.task, inputs.t)?;
        }
        let checks = rules::evaluate(&spec, &inputs, &self.engine.policy.rules);
        let passed = rules::passed(&checks, spec.kind());
        let policy = passed.then(|| policy::decide(&spec, &inputs, &self.engine.policy));
        let would_reach = match &policy {
            None => State::Rejected,
            Some(d) if d.tier == Tier::Auto => State::AutoApproved,
            Some(_) => State::PendingApproval,
        };
        let verdict = match &policy {
            None => format!("checks fail ({})", summarize_failures(&checks)),
            Some(d) => format!("checks pass; policy {}: {}", d.rule, d.reason),
        };
        self.engine.emit(EventBody::AgentAction {
            agent: self.agent.clone(),
            action: "intent.preview".into(),
            detail: format!(
                "{}: {verdict}; would reach {}",
                describe(&spec.task, &spec.uids),
                state_name(would_reach)
            ),
        })?;
        Ok(Preview {
            kind: spec.kind(),
            uids: spec.uids,
            checks,
            passed,
            policy,
            would_reach,
            space: inputs.space.answer().map(|a| Box::new(a.clone())),
        })
    }

    /// Log something this agent did that isn't a proposal (connected, read the inbox).
    /// Logged as `agent.action <action>`, like [`Engine::record_operator_action`] for
    /// operators.
    pub fn record_action(&mut self, action: &str, detail: &str) -> Result<(), EngineError> {
        self.engine.emit(EventBody::AgentAction {
            agent: self.agent.clone(),
            action: action.into(),
            detail: detail.into(),
        })?;
        Ok(())
    }

    /// Withdraw one of this agent's own intents before dispatch.
    pub fn withdraw(&mut self, id: IntentId, reason: &str) -> Result<&Intent, EngineError> {
        let mine = matches!(&self.engine.get(id)?.spec().source, Source::Agent { id: a } if *a == self.agent);
        if !mine {
            return Err(EngineError::NotOwner {
                agent: self.agent.clone(),
                id,
            });
        }
        next_state(self.engine.get(id)?.state(), Transition::Void)
            .map_err(|err| EngineError::Illegal { id, err })?;
        self.engine.emit(EventBody::AgentAction {
            agent: self.agent.clone(),
            action: "intent.withdraw".into(),
            detail: format!("{id}: {reason}"),
        })?;
        self.engine.transition(
            id,
            Transition::Void,
            format!("withdrawn by agent {}: {reason}", self.agent),
        )?;
        self.engine.get(id)
    }

    /// Commit a collect (any source's) to one of its options: a new collect proposed by
    /// this agent, with a mandatory rationale, routed by policy (collect: a human
    /// approves). The old intent is voided only if it is this agent's own and still open.
    /// Logged as `agent.action intent.amend`.
    pub fn amend_collect(
        &mut self,
        id: IntentId,
        chosen: ChosenCollect,
        rationale: Rationale,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        let (mut proposal, old_state) = self.engine.amended(id, chosen)?;
        proposal.rationale = Some(rationale);
        let mine = matches!(&self.engine.get(id)?.spec().source, Source::Agent { id: a } if *a == self.agent);
        let new_id = IntentId(self.engine.next_intent);
        self.engine.emit(EventBody::AgentAction {
            agent: self.agent.clone(),
            action: "intent.amend".into(),
            detail: amend_detail(id, new_id, &chosen),
        })?;
        let source = Source::Agent {
            id: self.agent.clone(),
        };
        let new_id = self
            .engine
            .submit_inner(source, None, proposal, world, terrain)?;
        if mine && next_state(old_state, Transition::Void).is_ok() {
            self.engine.transition(
                id,
                Transition::Void,
                format!("superseded by {new_id} (agent {})", self.agent),
            )?;
        }
        self.engine.get(new_id)
    }

    pub fn intent(&self, id: IntentId) -> Option<&Intent> {
        self.engine.intent(id)
    }

    /// Every intent, any source, in id order (read-only).
    pub fn intents(&self) -> impl Iterator<Item = &Intent> {
        self.engine.intents()
    }

    pub fn inbox(&self) -> Vec<&Intent> {
        self.engine.inbox()
    }
}

/// `orbit on [sky-1, sky-2]`, for agent action details.
fn describe(task: &Task, uids: &[Uid]) -> String {
    let uids: Vec<&str> = uids.iter().map(Uid::as_str).collect();
    format!("{} on [{}]", task.kind().as_str(), uids.join(", "))
}

/// The state's name as logged (`pending_approval`).
fn state_name(state: State) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{state:?}"))
}
