//! The engine: owns the intents, applies rules and policy, enforces the state machine, and
//! writes every step to the audit log before taking it.

use std::collections::{BTreeMap, BTreeSet};

use crate::areas::AreaSet;
use crate::audit::{AuditSink, CheckPhase, Event, EventBody, LoggedAreas, OperatorView};
use crate::authority::{EngagementAuthority, Grants, HumanAuthority, OperatorSession};
use crate::engage::{
    self, EffectOrder, EffectRecord, EngageOrder, EngageWindow, EngagementEvent, FallbackOrder,
    FrozenPlan, PictureToken, PictureVerdict, StopCause, StopRecord, TrackIdentity,
};
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
    #[error("intent {id} is {state:?}: an open or decide view is only taken while it waits for a decision")]
    ViewNotPending { id: IntentId, state: State },
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
    #[error("intent {0} is an engagement: dispatch it with dispatch_order")]
    EngageIntent(IntentId),
    #[error("intent {0} is an engagement: only an operator with engagement authority may approve it")]
    EngagementNeedsAuthority(IntentId),
    #[error("intent {0} is not an engagement")]
    NotAnEngagement(IntentId),
    #[error("intent {id}: {by} proposed it and may not approve it (two-person rule)")]
    ProposerCannotApprove { id: IntentId, by: OperatorId },
    #[error("intent {id}: {by} has already signed it; another operator must")]
    AlreadySigned { id: IntentId, by: OperatorId },
    #[error("intent {id}: {why}")]
    EngageLifecycle { id: IntentId, why: String },
    #[error("intent {id}: signature refused: {why}")]
    StalePicture { id: IntentId, why: String },
    #[error("intent {id}: {why}; every signature was dropped: sign the new plan")]
    PlanChanged { id: IntentId, why: String },
    #[error("intent {id}: no picture to show: {why}")]
    NoPicture { id: IntentId, why: String },
    #[error("intent {id}: approving it would stop engagement(s) {list} on its vehicles: acknowledge that to approve it")]
    SupersedesEngagement { id: IntentId, list: String },
}

/// A space answer computed outside the engine's lock (the bridge runs the SGP4 work first,
/// then takes the lock): the next check of the same task uses it if it was computed within
/// [`PREPARED_SPACE_MAX_AGE_MS`] of that check.
#[derive(Clone, Debug)]
pub struct PreparedSpace {
    pub task: Task,
    pub t: Time,
    pub inputs: SpaceInputs,
}

/// How old a prepared space answer may be when the check that uses it runs, ms.
pub const PREPARED_SPACE_MAX_AGE_MS: i64 = 5_000;

/// Ask `oracle` the task's space question at engine time `t` (what [`Engine`] does inside
/// its checks; the bridge calls this outside its lock and hands the result to
/// [`Engine::use_prepared_space`]).
pub fn prepare_space(
    oracle: &dyn SpaceOracle,
    task: &Task,
    t: Time,
    rules: &crate::rules::RulesConfig,
) -> Result<PreparedSpace, EngineError> {
    Ok(PreparedSpace {
        task: task.clone(),
        t,
        inputs: space_inputs(oracle, task, t, rules)?,
    })
}

fn space_inputs(
    oracle: &dyn SpaceOracle,
    task: &Task,
    t: Time,
    rules: &crate::rules::RulesConfig,
) -> Result<SpaceInputs, EngineError> {
    let now = oracle.scenario_time(t);
    let query = match query_for(task, now, &rules.space_rules()) {
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

/// What [`Engine::step_engagements`] did: what the bridge must act on, and the engagements
/// whose step failed (an audit write, say), each on its own.
#[derive(Debug, Default)]
pub struct StepOutcome {
    pub events: Vec<EngagementEvent>,
    pub errors: Vec<(IntentId, EngineError)>,
}

/// The engagements in play (pending, approved, dispatched) on `spec`'s vehicles, other than
/// `spec` itself, by vehicle (the lowest engagement id when there are several). The engine
/// logs this with each decision (`engaged`); replay recomputes it from its own intents.
pub(crate) fn engaged_vehicles<'a>(
    intents: impl Iterator<Item = (IntentId, &'a IntentSpec, State)>,
    spec: &IntentSpec,
) -> BTreeMap<Uid, IntentId> {
    let mut out: BTreeMap<Uid, IntentId> = BTreeMap::new();
    for (id, s, state) in intents {
        if id == spec.id
            || s.kind() != TaskKind::Engage
            || !matches!(
                state,
                State::PendingApproval | State::Approved | State::Dispatched
            )
        {
            continue;
        }
        for u in spec.uids.iter().filter(|u| s.uids.contains(u)) {
            out.entry(u.clone()).or_insert(id);
        }
    }
    out
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
    /// An approved engagement: send the platform to loiter at the effect point.
    Engage(EngageOrder),
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
    /// A space answer computed outside the lock for the next check ([`PreparedSpace`]).
    prepared_space: Option<PreparedSpace>,
    /// Picture tokens issued and not yet used or expired, by id.
    tokens: BTreeMap<u64, PictureToken>,
    next_token: u64,
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
            prepared_space: None,
            tokens: BTreeMap::new(),
            next_token: 1,
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
        self.open_operator_session_with(operator, Grants::default())
    }

    /// [`Engine::open_operator_session`] with grants beyond tasking: with `engage`, the
    /// session carries an [`EngagementAuthority`] (the only thing that approves an
    /// engagement). The grants are logged with the session (`grants` on
    /// `operator.action session.opened`), so replay knows who could approve what.
    pub fn open_operator_session_with(
        &mut self,
        operator: OperatorId,
        grants: Grants,
    ) -> Result<OperatorSession, EngineError> {
        let session = SessionId(self.next_session);
        self.emit(EventBody::OperatorAction {
            operator: operator.clone(),
            session,
            action: "session.opened".into(),
            detail: if grants.engage {
                "engagement authority".into()
            } else {
                String::new()
            },
            grants: (grants != Grants::default()).then_some(grants),
        })?;
        self.next_session += 1;
        Ok(OperatorSession::mint(operator, session, grants))
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
    ///
    /// When the tier needs more than one approver (`approvers = N`), each call is one
    /// signature: the first N-1 are logged as `approval.concurred` and leave the intent
    /// pending; the Nth is `approval.granted` and approves it. Each signer must be a
    /// different operator, and where the tier excludes the proposer
    /// (`proposer_may_approve = false`, always for an engagement) the proposer's attempt is
    /// refused and logged (`operator.action approval.refused`). An engagement can't be
    /// approved here at all: it needs [`Engine::approve_engagement`].
    ///
    /// If dispatching it would stop an engagement in play on its vehicles (the newest order
    /// stands), the approval is refused (`EngineError::SupersedesEngagement`, logged) unless
    /// it is made with [`Engine::approve_acknowledging`].
    pub fn approve(
        &mut self,
        id: IntentId,
        by: &HumanAuthority,
        note: Option<String>,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        self.approve_inner(id, by, note, false, world, terrain)
    }

    /// [`Engine::approve`], acknowledging that its dispatch stops the engagements in play on
    /// its vehicles (logged as `supersedes` on the approval).
    pub fn approve_acknowledging(
        &mut self,
        id: IntentId,
        by: &HumanAuthority,
        note: Option<String>,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        self.approve_inner(id, by, note, true, world, terrain)
    }

    fn approve_inner(
        &mut self,
        id: IntentId,
        by: &HumanAuthority,
        note: Option<String>,
        acknowledge: bool,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        if self.get(id)?.spec().kind() == TaskKind::Engage {
            self.refuse(
                by.operator(),
                by.session(),
                id,
                "no engagement authority in this session",
            )?;
            return Err(EngineError::EngagementNeedsAuthority(id));
        }
        self.sign(id, by.operator(), by.session(), note, acknowledge, world, terrain)
    }

    /// Show a signer the picture they would sign ([`PictureToken`], logged as
    /// `picture.issued` with the world it was computed on): the frozen plan, or (no signature
    /// yet) the preview on the world now that their signature would freeze, and the track's
    /// identity, affiliation and declaration, valid `token_ttl_s`. Only for an operator who
    /// may sign it (engagement authority by type, not the proposer, not signed yet).
    pub fn issue_picture(
        &mut self,
        id: IntentId,
        by: &EngagementAuthority,
        world: &WorldSnapshot,
    ) -> Result<PictureToken, EngineError> {
        let intent = self.get(id)?;
        if intent.spec().kind() != TaskKind::Engage {
            return Err(EngineError::NotAnEngagement(id));
        }
        next_state(intent.state(), Transition::Approve)
            .map_err(|err| EngineError::Illegal { id, err })?;
        let spec = intent.spec().clone();
        let operator = by.operator().clone();
        if intent.policy().is_some_and(|p| p.proposer_excluded) && is_self_approval(&spec.source, &operator) {
            return Err(EngineError::ProposerCannotApprove { id, by: operator });
        }
        if intent.concurrences().iter().any(|a| a.by == operator) {
            return Err(EngineError::AlreadySigned { id, by: operator });
        }
        let frozen = intent.engagement().and_then(|e| e.plan.clone());
        let track_id = engage::params(&spec).map(|p| p.0.to_string()).unwrap_or_default();
        let inputs = world.subset_for(&spec);
        let plan = match &frozen {
            Some(p) => p.preview.clone(),
            None => engage::preview(&spec, &inputs, &self.policy.rules)
                .map_err(|why| EngineError::NoPicture { id, why })?,
        };
        let track = inputs.tracks.get(&track_id).ok_or_else(|| EngineError::NoPicture {
            id,
            why: format!("{track_id} is not in the picture"),
        })?;
        let now = self.now();
        let ttl = self.policy.rules.engage_rules().token_ttl_s;
        let token = PictureToken {
            id: self.next_token,
            intent: id,
            by: operator,
            session: by.session(),
            t: now,
            expires_t: now.plus_ms((ttl * 1000.0).round() as i64),
            plan,
            frozen: frozen.is_some(),
            track: TrackIdentity::of(&track_id, track),
        };
        let logged = self.log_inputs(&inputs)?;
        self.emit(EventBody::PictureIssued {
            token: Box::new(token.clone()),
            inputs: logged,
        })?;
        self.next_token += 1;
        self.tokens.retain(|_, t| t.expires_t >= now);
        self.tokens.insert(token.id, token.clone());
        Ok(token)
    }

    /// A picture token issued and not yet used or expired (the bridge reads its plan to
    /// fetch the terrain the signature's checks will ask for).
    pub fn picture_token(&self, token: u64) -> Option<&PictureToken> {
        self.tokens.get(&token)
    }

    /// Approve (sign) an engagement. Needs an [`EngagementAuthority`] (a session opened with
    /// engagement authority), is never the proposer's, and needs as many distinct signers as
    /// the tier asks (at least two). The signature quotes the picture token its signer was
    /// shown ([`Engine::issue_picture`]); it is refused (logged) if the token expired, is
    /// another session's, or its plan or the track's identity, affiliation or declaration
    /// no longer match. The first signature freezes the token's plan; every later one signs
    /// that exact plan. Each signature re-runs the checks on the plan against the world now:
    /// before a plan is frozen a failure voids the nomination, after it the plan no longer
    /// holds, so every signature is dropped with it (`approval.reset`) and this one refused.
    /// The same plan checks run every second while signatures wait
    /// ([`Engine::step_engagements`]). The last signature's picture (its snapshot carries the
    /// plan) is held against every step after, and the release executes the plan.
    pub fn approve_engagement(
        &mut self,
        id: IntentId,
        by: &EngagementAuthority,
        note: Option<String>,
        token: u64,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        if self.get(id)?.spec().kind() != TaskKind::Engage {
            return Err(EngineError::NotAnEngagement(id));
        }
        self.sign_engagement(id, by.operator(), by.session(), note, token, world, terrain)
    }

    /// Log a refused approval attempt (nothing else changes).
    fn refuse(
        &mut self,
        by: &OperatorId,
        session: SessionId,
        id: IntentId,
        why: &str,
    ) -> Result<(), EngineError> {
        self.emit(EventBody::OperatorAction {
            operator: by.clone(),
            session,
            action: "approval.refused".into(),
            detail: format!("{id}: {why}"),
            grants: None,
        })?;
        Ok(())
    }

    /// One approval signature on an intent that is not an engagement (see
    /// [`Engine::approve`]).
    #[allow(clippy::too_many_arguments)]
    fn sign(
        &mut self,
        id: IntentId,
        by: &OperatorId,
        session: SessionId,
        note: Option<String>,
        acknowledge: bool,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        let intent = self.get(id)?;
        next_state(intent.state(), Transition::Approve)
            .map_err(|err| EngineError::Illegal { id, err })?;
        let spec = intent.spec().clone();
        let expires = intent.expires_t();
        let (of, proposer_excluded) = intent
            .policy()
            .map_or((1, false), |p| (p.approvers.max(1), p.proposer_excluded));
        let signed_before = intent.concurrences().iter().any(|a| a.by == *by);
        let now = self.now();
        if expires.is_some_and(|e| now >= e) {
            self.expire(id)?;
            return Err(EngineError::Expired(id));
        }
        let self_approved = is_self_approval(&spec.source, by);
        if proposer_excluded && self_approved {
            self.refuse(by, session, id, "the proposer may not approve it (two-person rule)")?;
            return Err(EngineError::ProposerCannotApprove { id, by: by.clone() });
        }
        if signed_before {
            self.refuse(by, session, id, "already signed: another operator must")?;
            return Err(EngineError::AlreadySigned { id, by: by.clone() });
        }
        let (inputs, _, results, ok) =
            self.run_checks_plan(&spec, CheckPhase::Approve, world, terrain, None)?;
        if !ok {
            let reason = format!(
                "checks failed at approval: {}",
                summarize_failures(&results)
            );
            self.transition(id, Transition::Void, reason)?;
            return Err(EngineError::ChecksFailedAtApproval(id));
        }
        // dispatching it stops the engagements in play on its vehicles: only if the approver
        // says so
        let mut supersedes: Vec<IntentId> = inputs.engaged.values().copied().collect();
        supersedes.sort();
        supersedes.dedup();
        if !supersedes.is_empty() && !acknowledge {
            let list = supersedes
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            self.refuse(
                by,
                session,
                id,
                &format!("would stop engagement(s) {list} on its vehicles; not acknowledged"),
            )?;
            return Err(EngineError::SupersedesEngagement { id, list });
        }
        let n = u32::try_from(self.get(id)?.concurrences().len()).unwrap_or(u32::MAX) + 1;
        if n < of {
            let t = self.emit(EventBody::ApprovalConcurred {
                intent: id,
                by: by.clone(),
                session,
                n,
                of,
                note: note.clone(),
                snapshot: None,
                token: None,
                supersedes,
            })?;
            self.get_mut(id)?.push_concurrence(Approval {
                by: by.clone(),
                session,
                t,
                self_approved,
                at_submission: false,
                note,
            });
            return self.get(id);
        }
        let t = self.emit(EventBody::ApprovalGranted {
            intent: id,
            by: by.clone(),
            session,
            self_approved,
            note: note.clone(),
            snapshot: None,
            token: None,
            supersedes,
        })?;
        self.get_mut(id)?.set_approval(Approval {
            by: by.clone(),
            session,
            t,
            self_approved,
            at_submission: false,
            note,
        });
        let signers = if of > 1 {
            format!(" (signature {n} of {of})")
        } else {
            String::new()
        };
        let reason = if self_approved {
            format!("approved by {by} (self-approved: also the proposer){signers}")
        } else {
            format!("approved by {by}{signers}")
        };
        self.transition(id, Transition::Approve, reason)?;
        self.get(id)
    }

    /// One engagement signature (see [`Engine::approve_engagement`]).
    #[allow(clippy::too_many_arguments)]
    fn sign_engagement(
        &mut self,
        id: IntentId,
        by: &OperatorId,
        session: SessionId,
        note: Option<String>,
        token: u64,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        let intent = self.get(id)?;
        next_state(intent.state(), Transition::Approve)
            .map_err(|err| EngineError::Illegal { id, err })?;
        let spec = intent.spec().clone();
        let expires = intent.expires_t();
        let (of, proposer_excluded) = intent
            .policy()
            .map_or((1, false), |p| (p.approvers.max(1), p.proposer_excluded));
        let signed_before = intent.concurrences().iter().any(|a| a.by == *by);
        let frozen = intent.engagement().and_then(|e| e.plan.clone());
        let now = self.now();
        if expires.is_some_and(|e| now >= e) {
            self.expire(id)?;
            return Err(EngineError::Expired(id));
        }
        let self_approved = is_self_approval(&spec.source, by);
        if proposer_excluded && self_approved {
            self.refuse(by, session, id, "the proposer may not approve it (two-person rule)")?;
            return Err(EngineError::ProposerCannotApprove { id, by: by.clone() });
        }
        if signed_before {
            self.refuse(by, session, id, "already signed: another operator must")?;
            return Err(EngineError::AlreadySigned { id, by: by.clone() });
        }
        // the picture this signer was shown
        let track_id = engage::params(&spec).map(|p| p.0.to_string()).unwrap_or_default();
        let shown = match self.tokens.get(&token) {
            Some(t) => t.clone(),
            None => {
                let why = format!("no picture token {token} was issued (or it was used): look again");
                self.refuse(by, session, id, &why)?;
                return Err(EngineError::StalePicture { id, why });
            }
        };
        if let Err(why) = engage::check_token(
            &shown,
            id,
            by,
            session,
            now,
            world.tracks.get(&track_id),
            frozen.as_ref(),
        ) {
            self.refuse(by, session, id, &why)?;
            return Err(EngineError::StalePicture { id, why });
        }
        // the plan this signature is on: the frozen one, or the one this signature freezes
        let plan = frozen.clone().unwrap_or_else(|| FrozenPlan {
            t: now,
            by: by.clone(),
            preview: shown.plan.clone(),
            track: shown.track.clone(),
        });
        let (inputs, _, results, ok) =
            self.run_checks_plan(&spec, CheckPhase::Approve, world, terrain, Some(&plan))?;
        if !ok {
            let failed = summarize_failures(&results);
            if frozen.is_some() {
                let why = format!("the plan no longer holds: {failed}");
                self.reset_signatures(id, why.clone(), None)?;
                self.refuse(by, session, id, &why)?;
                return Err(EngineError::PlanChanged { id, why });
            }
            self.transition(id, Transition::Void, format!("checks failed at approval: {failed}"))?;
            return Err(EngineError::ChecksFailedAtApproval(id));
        }
        let snapshot = match engage::snapshot(&spec, &inputs, &self.policy.rules) {
            Ok(s) => s,
            Err(why) => {
                self.transition(
                    id,
                    Transition::Void,
                    format!("checks failed at approval: cannot hold the picture: {why}"),
                )?;
                return Err(EngineError::ChecksFailedAtApproval(id));
            }
        };
        self.tokens.remove(&token);
        let n = u32::try_from(self.get(id)?.concurrences().len()).unwrap_or(u32::MAX) + 1;
        if n < of {
            let t = self.emit(EventBody::ApprovalConcurred {
                intent: id,
                by: by.clone(),
                session,
                n,
                of,
                note: note.clone(),
                snapshot: Some(Box::new(snapshot.clone())),
                token: Some(Box::new(shown)),
                supersedes: Vec::new(),
            })?;
            let intent = self.get_mut(id)?;
            intent.push_concurrence(Approval {
                by: by.clone(),
                session,
                t,
                self_approved,
                at_submission: false,
                note,
            });
            let e = intent.engagement_mut();
            e.signed.push(engage::SignedPicture {
                by: by.clone(),
                t,
                snapshot,
            });
            if e.plan.is_none() {
                e.plan = Some(plan);
            }
            return self.get(id);
        }
        let t = self.emit(EventBody::ApprovalGranted {
            intent: id,
            by: by.clone(),
            session,
            self_approved,
            note: note.clone(),
            snapshot: Some(Box::new(snapshot.clone())),
            token: Some(Box::new(shown)),
            supersedes: Vec::new(),
        })?;
        let intent = self.get_mut(id)?;
        intent.set_approval(Approval {
            by: by.clone(),
            session,
            t,
            self_approved,
            at_submission: false,
            note,
        });
        let e = intent.engagement_mut();
        e.signed.push(engage::SignedPicture {
            by: by.clone(),
            t,
            snapshot: snapshot.clone(),
        });
        if e.plan.is_none() {
            e.plan = Some(plan);
        }
        e.snapshot = Some(snapshot);
        let reason = format!("approved by {by} (signature {n} of {of}) on the plan frozen by the first signature");
        self.transition(id, Transition::Approve, reason)?;
        self.get(id)
    }

    /// Drop an engagement's signatures so far and its frozen plan: the plan no longer holds.
    /// `inputs`: the world the checks read, when it is not a signature's own re-check.
    fn reset_signatures(
        &mut self,
        id: IntentId,
        reason: String,
        inputs: Option<LoggedInputs>,
    ) -> Result<(), EngineError> {
        let dropped: Vec<OperatorId> = self
            .get(id)?
            .concurrences()
            .iter()
            .map(|a| a.by.clone())
            .collect();
        let t = self.emit(EventBody::ApprovalReset {
            intent: id,
            dropped: dropped.clone(),
            reason: reason.clone(),
            inputs,
        })?;
        let intent = self.get_mut(id)?;
        intent.clear_concurrences();
        let e = intent.engagement_mut();
        e.signed.clear();
        e.plan = None;
        e.reset = Some(engage::ResetRecord { t, dropped, reason });
        Ok(())
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
            grants: None,
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
            grants: None,
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
            grants: None,
        })?;
        Ok(())
    }

    /// Log what an operator's console showed for an intent (`operator.view`, Phase G):
    /// the card opened, closed, or a decision button was clicked, with what was visible.
    /// Decides nothing; replay makes no decision from it (it checks the session is the
    /// operator's). The intent must exist, the view be well-formed
    /// ([`OperatorView::validate`]), and an `open` or `decide` view is taken only while the
    /// intent waits for a decision (`close` any time: a card can go away after its intent
    /// ended), so a late decide view cannot follow the decision it describes.
    pub fn record_operator_view(
        &mut self,
        by: &HumanAuthority,
        intent: IntentId,
        view: OperatorView,
    ) -> Result<(), EngineError> {
        let state = self.get(intent)?.state();
        view.validate()
            .map_err(|m| EngineError::Proposal(ProposalError::BadParams(m)))?;
        if view.event != "close" && state != State::PendingApproval {
            return Err(EngineError::ViewNotPending { id: intent, state });
        }
        self.emit(EventBody::OperatorView {
            operator: by.operator().clone(),
            session: by.session(),
            intent,
            view,
        })?;
        Ok(())
    }

    // ---- engagements --------------------------------------------------------------------

    /// Any operator stops an approved or dispatched engagement before its effect (no
    /// engagement authority needed: anyone may stop an action). Before dispatch it is
    /// voided; after, aborted, and the recorded fallback is returned for the bridge to
    /// command. Logged as `engage.stopped` with the operator.
    pub fn abort_engagement(
        &mut self,
        id: IntentId,
        by: &HumanAuthority,
        note: &str,
    ) -> Result<Option<FallbackOrder>, EngineError> {
        let cause = StopCause::Operator {
            by: by.operator().clone(),
            session: by.session(),
        };
        let reason = format!("aborted by {}: {note}", by.operator());
        self.stop_engagement(id, cause, reason, true)
    }

    /// The bridge stops an engagement for a reason outside the picture (its task failed, it
    /// was lost). As [`Engine::abort_engagement`].
    pub fn stop_engagement_system(
        &mut self,
        id: IntentId,
        reason: &str,
    ) -> Result<Option<FallbackOrder>, EngineError> {
        self.stop_engagement(id, StopCause::System, format!("system: {reason}"), true)
    }

    /// Another intent (`by`) was dispatched to an engagement's platform: that order stands,
    /// so the engagement (pending, approved or dispatched) is stopped with no fallback
    /// commanded. Logged as `engage.stopped`, cause `superseded` with `by` and its source.
    pub fn supersede_engagement(&mut self, id: IntentId, by: IntentId) -> Result<(), EngineError> {
        let source = self.get(by)?.spec().source.clone();
        let who = match &source {
            Source::Human { id } => format!("human {id}"),
            Source::Agent { id } => format!("agent {id}"),
        };
        let reason = format!("superseded by {by} from {who}: that order stands; no fallback commanded");
        self.stop_engagement(id, StopCause::Superseded { by, source }, reason, false)?;
        Ok(())
    }

    fn stop_engagement(
        &mut self,
        id: IntentId,
        cause: StopCause,
        reason: String,
        command_fallback: bool,
    ) -> Result<Option<FallbackOrder>, EngineError> {
        let intent = self.get(id)?;
        if intent.spec().kind() != TaskKind::Engage {
            return Err(EngineError::NotAnEngagement(id));
        }
        let state = intent.state();
        let superseded = matches!(cause, StopCause::Superseded { .. });
        let (transition, fallback) = match state {
            State::PendingApproval if superseded => (Transition::Void, None),
            State::Approved => (Transition::Void, None),
            State::Dispatched => (
                Transition::Abort,
                engage::params(intent.spec())
                    .map(|p| p.3)
                    .filter(|_| command_fallback),
            ),
            _ => {
                let err = next_state(state, Transition::Abort)
                    .err()
                    .unwrap_or(crate::intent::IllegalTransition {
                        from: state,
                        transition: Transition::Abort,
                    });
                return Err(EngineError::Illegal { id, err });
            }
        };
        let uid = intent.spec().uids.first().cloned();
        let t = self.emit(EventBody::EngageStopped {
            intent: id,
            cause: cause.clone(),
            reason: reason.clone(),
            fallback,
        })?;
        self.get_mut(id)?.engagement_mut().stop = Some(StopRecord {
            t,
            cause,
            reason: reason.clone(),
            fallback,
        });
        let reason = match fallback {
            Some(f) => format!("{reason}; fallback {}", f.as_str()),
            None => reason,
        };
        self.transition(id, transition, reason.clone())?;
        Ok(match (fallback, uid) {
            (Some(fallback), Some(uid)) => Some(FallbackOrder {
                intent: id,
                uid,
                fallback,
                reason,
            }),
            _ => None,
        })
    }

    /// Once a second (the bridge's loop): for every engagement waiting with signatures,
    /// check that the track still is what they signed (else drop them: `approval.reset`);
    /// for every approved or dispatched engagement, hold the picture against its snapshot
    /// (logged as `engage.picture`), void it (before dispatch) or abort it with its fallback
    /// (after) when the picture no longer holds, release the effect when it is ready
    /// (`engage.effect`); and for every effected engagement, observe the effect point
    /// (`engage.observe`) until the assessment (`engage.assessed`) finishes it. Each
    /// engagement is stepped on its own: an error on one (reported in `errors`) does not
    /// keep the others from being held, released or stopped.
    pub fn step_engagements(
        &mut self,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> StepOutcome {
        let ids: Vec<IntentId> = self
            .intents
            .values()
            .filter(|i| {
                i.spec().kind() == TaskKind::Engage
                    && (matches!(
                        i.state(),
                        State::Approved | State::Dispatched | State::Effected
                    ) || (i.state() == State::PendingApproval
                        && i.engagement().is_some_and(|e| e.plan.is_some())))
            })
            .map(Intent::id)
            .collect();
        let mut out = StepOutcome::default();
        for id in ids {
            match self.step_one(id, world, terrain) {
                Ok(Some(ev)) => out.events.push(ev),
                Ok(None) => {}
                Err(e) => out.errors.push((id, e)),
            }
        }
        out
    }

    fn step_one(
        &mut self,
        id: IntentId,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<Option<EngagementEvent>, EngineError> {
        let intent = self.get(id)?;
        let spec = intent.spec().clone();
        let state = intent.state();
        let Some(eng) = intent.engagement().cloned() else {
            return Ok(None);
        };
        if state == State::PendingApproval {
            // hold the plan the signers signed: the engagement checks on it, every second
            let Some(plan) = eng.plan.clone() else {
                return Ok(None);
            };
            let inputs = world.for_intent_plan(&spec, terrain, &self.policy.rules, Some(&plan));
            let results = engage::evaluate(&spec, &inputs, &self.policy.rules);
            if results.iter().any(|c| c.outcome != Outcome::Pass) {
                let logged = self.log_inputs(&inputs)?;
                let why = format!("the plan no longer holds: {}", summarize_failures(&results));
                self.reset_signatures(id, why, Some(logged))?;
            }
            return Ok(None);
        }
        let inputs = world.subset_for(&spec);
        let Some(snap) = eng.snapshot.clone() else {
            return Ok(None);
        };
        if state == State::Effected {
            let Some(effect) = eng.effect.clone() else {
                return Ok(None);
            };
            let prev = eng.last_observe;
            let step = engage::observe(&snap, &effect, &inputs, &self.policy.rules);
            let covered = !step.covered_by.is_empty();
            let logged = self.log_inputs(&inputs)?;
            let t = self.now();
            let covered_s = eng.covered_s + engage::coverage_increment(prev, t, covered);
            let t = self.emit_at(
                t,
                EventBody::EngageObserve {
                    intent: id,
                    inputs: logged,
                    step: step.clone(),
                    covered_s,
                },
            )?;
            let verdict = engage::assess(&snap, &effect, &step, covered_s, t, &self.policy.rules);
            {
                let e = self.get_mut(id)?.engagement_mut();
                e.observe = Some(step);
                e.covered_s = covered_s;
                e.last_observe = Some((t, covered));
            }
            let Some((result, evidence)) = verdict else {
                return Ok(None);
            };
            let t = self.emit(EventBody::EngageAssessed {
                intent: id,
                result,
                evidence: evidence.clone(),
                covered_s,
            })?;
            self.get_mut(id)?.engagement_mut().assessment = Some(engage::Assessment {
                t,
                result,
                evidence: evidence.clone(),
                covered_s,
            });
            self.transition(
                id,
                Transition::Complete,
                format!("assessed {}: {evidence}", result.as_str()),
            )?;
            return Ok(Some(EngagementEvent::Assessed { intent: id, result }));
        }
        let dispatched = state == State::Dispatched;
        let prev_t = eng.watch.as_ref().map_or(snap.t, |w| w.t);
        let (results, holds, ready) = engage::picture(
            &spec,
            &snap,
            &inputs,
            &self.policy.rules,
            dispatched,
            prev_t,
        );
        let logged = self.log_inputs(&inputs)?;
        let t = self.emit_at(
            inputs.t.max(self.last_t),
            EventBody::EngagePicture {
                intent: id,
                inputs: logged,
                results: results.clone(),
                holds,
                ready,
            },
        )?;
        let why = summarize_failures(&results);
        self.get_mut(id)?.engagement_mut().watch = Some(PictureVerdict {
            t,
            holds,
            ready,
            results,
        });
        if !holds {
            let reason = format!("picture changed: {why}");
            return Ok(Some(
                match self.stop_engagement(id, StopCause::Picture, reason.clone(), true)? {
                    Some(order) => EngagementEvent::Stopped(order),
                    None => EngagementEvent::Voided { intent: id, reason },
                },
            ));
        }
        if !(dispatched && ready) {
            return Ok(None);
        }
        let p = &snap.preview;
        let Some(uid) = spec.uids.first().cloned() else {
            return Ok(None);
        };
        let t = self.emit(EventBody::EngageEffect {
            intent: id,
            uid: uid.clone(),
            point: p.effect_point,
            radius_m: p.effect_radius_m,
            planned_t: p.effect_t,
        })?;
        self.get_mut(id)?.engagement_mut().effect = Some(EffectRecord {
            t,
            point: p.effect_point,
            radius_m: p.effect_radius_m,
            planned_t: p.effect_t,
        });
        self.transition(
            id,
            Transition::Release,
            format!(
                "effect released at ({:.5}, {:.5}), radius {:.0} m",
                p.effect_point.lon, p.effect_point.lat, p.effect_radius_m
            ),
        )?;
        Ok(Some(EngagementEvent::Effect(EffectOrder {
            intent: id,
            uid,
            track: snap.track_id.clone(),
            point: p.effect_point,
            radius_m: p.effect_radius_m,
            t,
        })))
    }

    /// The engagements in play (pending, approved, dispatched) on `spec`'s vehicles, other
    /// than `spec` itself: a fact the engine logs with each decision (`engaged`).
    fn engaged_for(&self, spec: &IntentSpec) -> BTreeMap<Uid, IntentId> {
        engaged_vehicles(
            self.intents.values().map(|i| (i.id(), i.spec(), i.state())),
            spec,
        )
    }

    /// For the console (not a decision, not logged): an engagement's checks and preview on
    /// the world now, to show the operator what changed since it was nominated.
    pub fn engage_live(
        &self,
        id: IntentId,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Option<(Vec<CheckResult>, Option<engage::EngagePreview>)> {
        let intent = self.intents.get(&id)?;
        let spec = intent.spec();
        if spec.kind() != TaskKind::Engage {
            return None;
        }
        // with a frozen plan: the checks on it, and it is what a signer signs
        let plan = intent.engagement().and_then(|e| e.plan.as_ref());
        let inputs = world.for_intent_plan(spec, terrain, &self.policy.rules, plan);
        let checks = rules::evaluate(spec, &inputs, &self.policy.rules);
        let pv = match plan {
            Some(p) => Some(p.preview.clone()),
            None => engage::preview(spec, &inputs, &self.policy.rules).ok(),
        };
        Some((checks, pv))
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
        let spec = self.get(id)?.spec();
        if spec.task.is_space() {
            return Err(EngineError::SpaceIntent(id));
        }
        if spec.kind() == TaskKind::Engage {
            return Err(EngineError::EngageIntent(id));
        }
        match self.dispatch_order(id)? {
            Order::Vehicles(o) => Ok(o),
            Order::Collect(_) | Order::Cover(_) => Err(EngineError::SpaceIntent(id)),
            Order::Engage(_) => Err(EngineError::EngageIntent(id)),
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
            Task::Engage { track, .. } => {
                let not = |why: &str| EngineError::NotDispatchable {
                    id,
                    why: why.to_string(),
                };
                let snap = intent
                    .engagement()
                    .and_then(|e| e.snapshot.as_ref())
                    .ok_or_else(|| not("no approval snapshot"))?;
                let uid = spec
                    .uids
                    .first()
                    .cloned()
                    .ok_or_else(|| not("no platform"))?;
                Order::Engage(EngageOrder {
                    intent: id,
                    uid,
                    track: track.clone(),
                    effect_point: snap.preview.effect_point,
                    effect_t: snap.preview.effect_t,
                    effect_radius_m: snap.preview.effect_radius_m,
                    fallback: snap.fallback,
                    window_until: snap.preview.window.until,
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
            Order::Vehicles(_) | Order::Engage(_) => None,
            Order::Collect(c) => Some(SpaceOrder::Collect(c.clone())),
            Order::Cover(c) => Some(SpaceOrder::Cover(c.clone())),
        };
        let engage_order = match &order {
            Order::Engage(o) => Some(o.clone()),
            _ => None,
        };
        let reason = match &order {
            Order::Vehicles(_) => "dispatched to the task layer",
            Order::Collect(_) => "dispatched: satellite tasking request",
            Order::Cover(_) => "dispatched: coverage watch",
            Order::Engage(_) => "dispatched: platform sent to the effect point",
        };
        self.emit(EventBody::IntentDispatched {
            intent: id,
            task,
            uids,
            space,
            engage: engage_order,
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
        if self.get(id)?.spec().kind() == TaskKind::Engage {
            return Err(EngineError::EngageLifecycle {
                id,
                why: "an engagement ends by its effect and assessment, or by an abort".into(),
            });
        }
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
        self.run_checks_plan(spec, phase, world, terrain, None)
    }

    /// [`Engine::run_checks`] on an engagement plan (a signature's).
    fn run_checks_plan(
        &mut self,
        spec: &IntentSpec,
        phase: CheckPhase,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
        plan: Option<&FrozenPlan>,
    ) -> Result<(WorldSnapshot, LoggedInputs, Vec<CheckResult>, bool), EngineError> {
        let mut inputs = world.for_intent_plan(spec, terrain, &self.policy.rules, plan);
        if spec.task.is_space() {
            inputs.space = self.ask_space(&spec.task, inputs.t)?;
        }
        inputs.engaged = self.engaged_for(spec);
        let results = rules::evaluate(spec, &inputs, &self.policy.rules);
        let passed = rules::passed(&results, spec.kind());
        let preview = (spec.kind() == TaskKind::Engage)
            .then(|| {
                let mut live = inputs.clone();
                live.plan = None;
                engage::preview(spec, &live, &self.policy.rules).ok()
            })
            .flatten();
        let logged = self.log_inputs(&inputs)?;
        self.emit(EventBody::CheckResult {
            intent: spec.id,
            phase,
            inputs: logged.clone(),
            results: results.clone(),
            passed,
            preview: preview.clone().map(Box::new),
        })?;
        let intent = self.get_mut(spec.id)?;
        intent.set_checks(results.clone());
        if spec.task.is_space() {
            intent.set_space(inputs.space.answer().map(|a| Box::new(a.clone())));
        }
        if spec.kind() == TaskKind::Engage {
            intent.engagement_mut().preview = preview;
        }
        Ok((inputs, logged, results, passed))
    }

    /// Ask the space segment this task's question at snapshot time `t`. Failures become
    /// `Unavailable` (the space rule then fails, saying why); only a hashing error is an
    /// engine error.
    fn ask_space(&mut self, task: &Task, t: Time) -> Result<SpaceInputs, EngineError> {
        // computed outside the lock for this very question, a moment ago
        if let Some(p) = self.prepared_space.take()
            && p.task == *task
            && (t.ms_since(p.t)).abs() <= PREPARED_SPACE_MAX_AGE_MS
        {
            return Ok(p.inputs);
        }
        let Some(oracle) = &self.space else {
            return Ok(SpaceInputs::Unavailable {
                reason: "no space segment loaded".into(),
            });
        };
        space_inputs(oracle.as_ref(), task, t, &self.policy.rules)
    }

    /// Use `prepared` (computed outside the engine, see [`prepare_space`]) for the next check
    /// of the same task, if it is still fresh then; otherwise the engine asks again.
    pub fn use_prepared_space(&mut self, prepared: PreparedSpace) {
        self.prepared_space = Some(prepared);
    }

    /// An engagement without a window gets the default one, written into its spec (so the
    /// log, the deadline and every check read the same window). Other tasks as given.
    fn normalized(&self, task: Task, created_t: Time) -> Task {
        match task {
            Task::Engage {
                track,
                effect_radius_m,
                window: None,
                fallback,
            } => Task::Engage {
                track,
                effect_radius_m,
                window: Some(EngageWindow {
                    from: None,
                    until: created_t.plus_ms(
                        i64::from(self.policy.rules.engage_rules().default_window_s) * 1000,
                    ),
                }),
                fallback,
            },
            t => t,
        }
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
        let created_t = self.now();
        let spec = IntentSpec {
            id,
            created_t,
            source,
            task: self.normalized(proposal.task, created_t),
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
    /// Engage: the intercept and the predicted effect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engage: Option<Box<engage::EngagePreview>>,
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
/// ```compile_fail,E0599
/// use murmur_decide::{AgentPort, IntentId};
/// fn agent_code(port: &mut AgentPort<'_>) {
///     port.approve_engagement(IntentId(1)); // no such method either (Phase E)
/// }
/// ```
///
/// ```compile_fail,E0599
/// use murmur_decide::{AgentPort, IntentId};
/// fn agent_code(port: &mut AgentPort<'_>) {
///     port.abort_engagement(IntentId(1), "stop"); // an agent can't stop an engagement
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
        let created_t = self.engine.now();
        let spec = IntentSpec {
            id: IntentId(0),
            created_t,
            source: Source::Agent {
                id: self.agent.clone(),
            },
            task: self.engine.normalized(proposal.task, created_t),
            uids: proposal.uids,
            constraints: proposal.constraints,
            rationale: proposal.rationale,
        };
        let mut inputs = world.for_intent_with(&spec, terrain, &self.engine.policy.rules);
        if spec.task.is_space() {
            inputs.space = self.engine.ask_space(&spec.task, inputs.t)?;
        }
        inputs.engaged = self.engine.engaged_for(&spec);
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
        let engage = (spec.kind() == TaskKind::Engage)
            .then(|| engage::preview(&spec, &inputs, &self.engine.policy.rules).ok())
            .flatten()
            .map(Box::new);
        Ok(Preview {
            kind: spec.kind(),
            uids: spec.uids,
            checks,
            passed,
            policy,
            would_reach,
            space: inputs.space.answer().map(|a| Box::new(a.clone())),
            engage,
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
