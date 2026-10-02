//! The engine: owns the intents, applies rules and policy, enforces the state machine, and
//! writes every step to the audit log before taking it.

use std::collections::BTreeMap;

use crate::areas::AreaSet;
use crate::audit::{AuditSink, CheckPhase, Event, EventBody, LoggedAreas};
use crate::authority::{HumanAuthority, OperatorSession};
use crate::hash::{GENESIS, sha256_hex};
use crate::ids::{AgentId, IntentId, OperatorId, SessionId, Uid};
use crate::intent::{
    Approval, IllegalTransition, Intent, IntentSpec, Proposal, ProposalError, Rationale, Source,
    State, Task, Transition, next_state,
};
use crate::policy::{self, Policy, Tier};
use crate::rules::{self, CheckResult, Outcome};
use crate::terrain::TerrainOracle;
use crate::time::{Clock, Time};
use crate::world::{LoggedInputs, WorldSnapshot};

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
}

/// What the task layer should execute for an approved intent.
#[derive(Clone, Debug, PartialEq)]
pub struct DispatchOrder {
    pub intent: IntentId,
    pub task: Task,
    pub uids: Vec<Uid>,
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

    /// A human's intent (also what `POST /api/tasks` becomes).
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
        let id = self.submit_inner(source, proposal, world, terrain)?;
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

    /// Hand an approved (or auto-approved) intent to the task layer.
    pub fn dispatch(&mut self, id: IntentId) -> Result<DispatchOrder, EngineError> {
        let intent = self.get(id)?;
        next_state(intent.state(), Transition::Dispatch)
            .map_err(|err| EngineError::Illegal { id, err })?;
        let order = DispatchOrder {
            intent: id,
            task: intent.spec().task.clone(),
            uids: intent.spec().uids.clone(),
        };
        self.emit(EventBody::IntentDispatched {
            intent: id,
            task: order.task.clone(),
            uids: order.uids.clone(),
        })?;
        self.transition(
            id,
            Transition::Dispatch,
            "dispatched to the task layer".into(),
        )?;
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

    /// The decision inputs as logged: the area set replaced by its hash. Logs the set as
    /// `areas.loaded` first if it isn't the one last logged.
    fn log_inputs(&mut self, inputs: &WorldSnapshot) -> Result<LoggedInputs, EngineError> {
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
        let inputs = world.for_intent(spec, terrain);
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
        self.get_mut(spec.id)?.set_checks(results.clone());
        Ok((inputs, logged, results, passed))
    }

    fn submit_inner(
        &mut self,
        source: Source,
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
        self.get_mut(id)?.set_policy(decision);
        match tier {
            Tier::Auto => {
                self.transition(id, Transition::AutoApprove, reason)?;
            }
            Tier::Human => {
                let expires_t = self
                    .now()
                    .plus_ms(i64::from(self.policy.approval.expire_s) * 1000);
                self.emit(EventBody::ApprovalRequested {
                    intent: id,
                    expires_t,
                })?;
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

/// What agent code is given (Phase F's MCP server holds one per agent). It can propose
/// (with a mandatory [`Rationale`]), withdraw its own intents, and read. It has no method
/// that approves, rejects on a human's behalf, or yields an operator session, and its
/// engine reference is private.
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
    pub fn propose(
        &mut self,
        mut proposal: Proposal,
        rationale: Rationale,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        proposal.rationale = Some(rationale);
        let source = Source::Agent {
            id: self.agent.clone(),
        };
        let id = self.engine.submit_inner(source, proposal, world, terrain)?;
        self.engine.get(id)
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

    pub fn intent(&self, id: IntentId) -> Option<&Intent> {
        self.engine.intent(id)
    }

    pub fn inbox(&self) -> Vec<&Intent> {
        self.engine.inbox()
    }
}
