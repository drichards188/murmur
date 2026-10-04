//! Intents (Phase C.1): every action, from a human or an agent, as a record with a
//! decision trail.
//!
//! An [`IntentSpec`] is the immutable request (what, to whom, why, from whom). An
//! [`Intent`] adds where it stands: state, check results, policy decision, approval. Its
//! fields are private and change only through the [`crate::Engine`], which logs every change
//! before making it. The legal state changes are the table in [`next_state`].

use serde::{Deserialize, Serialize};

use crate::geo::{LonLat, TaskPath, circle_points};
use crate::ids::{AgentId, IntentId, OperatorId, SessionId, Uid};
use crate::policy::PolicyDecision;
use crate::rules::CheckResult;
use crate::engage::{EngageWindow, Engagement, Fallback};
use crate::space::{
    ChosenCollect, CollectCapability, CoverService, SpaceAnswer, SpaceArea, Window,
};
use crate::time::Time;

/// Waypoints in an orbit ring for non-fixed-wing vehicles (as the Python bridge flies it).
pub const ORBIT_RING_POINTS: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Orbit,
    Patrol,
    Hold,
    Rtl,
    Track,
    /// Satellite collection (imagery or SAR) over an area in a window. No fleet vehicle.
    Collect,
    /// SATCOM coverage or GNSS assurance over an area for a window. No fleet vehicle.
    Cover,
    /// Phase E: pair a declared-hostile track with one armed platform for an abstract effect
    /// (a point, a radius, a time). Consequential: every rule must evaluate.
    Engage,
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Orbit => "orbit",
            TaskKind::Patrol => "patrol",
            TaskKind::Hold => "hold",
            TaskKind::Rtl => "rtl",
            TaskKind::Track => "track",
            TaskKind::Collect => "collect",
            TaskKind::Cover => "cover",
            TaskKind::Engage => "engage",
        }
    }

    /// What the kind acts on: fleet vehicles (by uid) or the space segment.
    pub fn targets(self) -> Targets {
        match self {
            TaskKind::Collect | TaskKind::Cover => Targets::Space,
            _ => Targets::Vehicles,
        }
    }

    /// Kinds for which every rule must actually evaluate (a "not evaluated" or "unverified"
    /// outcome blocks): engage.
    pub fn is_consequential(self) -> bool {
        matches!(self, TaskKind::Engage)
    }
}

/// What an intent acts on. Vehicle kinds name their vehicles in `uids` (at least one);
/// space kinds (collect, cover) request a capability from satellites, which are not fleet
/// vehicles, and must name none. Rules, policy facts and dispatch all branch on this.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Targets {
    Vehicles,
    Space,
}

/// The task and its parameters (today's task params). The kind is the variant, so a kind
/// with the wrong params can't be built. Serialized as `{"kind": ..., "params": {...}}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "params", rename_all = "snake_case")]
pub enum Task {
    Orbit {
        center: LonLat,
        #[serde(alias = "radius")]
        radius_m: f64,
    },
    /// Flown as a loop back to the first point.
    Patrol {
        route: Vec<LonLat>,
    },
    Hold {},
    Rtl {},
    /// Point an antenna tracker at another vehicle.
    Track {
        target: Uid,
    },
    /// Task a satellite collection: optical or SAR imagery of `area` within `window`.
    /// Without `chosen`, the intent is a query: its checks list the ranked options and
    /// why other passes were excluded, and it cannot be approved. With `chosen` (an option
    /// from such a query), approval re-checks that the option is still feasible.
    Collect {
        area: SpaceArea,
        window: Window,
        capability: CollectCapability,
        /// Coarsest acceptable ground resolution, metres (smaller is finer).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_resolution_m: Option<f64>,
        /// The product must be in hand by then (scenario time).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        product_deadline: Option<Time>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chosen: Option<ChosenCollect>,
    },
    /// Ask for SATCOM coverage or GNSS assurance over `area` for `window`.
    Cover {
        area: SpaceArea,
        window: Window,
        service: CoverService,
        /// SATCOM: minimum fraction of the window covered (0..1). GNSS: minimum satellites
        /// in view at every sample. Default from `[rules.space]`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_level: Option<f64>,
    },
    /// Nominate an engagement (Phase E): the track `track` (a perception track id, which an
    /// operator must have declared hostile), the one armed platform in `uids`, an effect
    /// radius, an optional time window, and what the platform does if the engagement stops
    /// before the effect (operator abort, a void, link loss). The effect is an abstract event
    /// at a point and time the engine predicts; nothing here models a weapon.
    Engage {
        track: String,
        effect_radius_m: f64,
        /// When the effect may happen (engine time). Absent: from now until
        /// `[rules.engage] default_window_s` after the intent was created.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<EngageWindow>,
        fallback: Fallback,
    },
}

impl Task {
    pub fn kind(&self) -> TaskKind {
        match self {
            Task::Orbit { .. } => TaskKind::Orbit,
            Task::Patrol { .. } => TaskKind::Patrol,
            Task::Hold {} => TaskKind::Hold,
            Task::Rtl {} => TaskKind::Rtl,
            Task::Track { .. } => TaskKind::Track,
            Task::Collect { .. } => TaskKind::Collect,
            Task::Cover { .. } => TaskKind::Cover,
            Task::Engage { .. } => TaskKind::Engage,
        }
    }

    /// Fleet vehicles or the space segment.
    pub fn targets(&self) -> Targets {
        self.kind().targets()
    }

    pub fn is_space(&self) -> bool {
        self.targets() == Targets::Space
    }

    /// For a collect: the same request committed to `chosen` (how a query intent is
    /// amended to one of its options). None for other kinds.
    pub fn with_chosen(&self, chosen: ChosenCollect) -> Option<Task> {
        match self {
            Task::Collect {
                area,
                window,
                capability,
                min_resolution_m,
                product_deadline,
                ..
            } => Some(Task::Collect {
                area: area.clone(),
                window: *window,
                capability: *capability,
                min_resolution_m: *min_resolution_m,
                product_deadline: *product_deadline,
                chosen: Some(chosen),
            }),
            _ => None,
        }
    }

    /// The waypoints the task commits a vehicle to; None for tasks flown in place.
    /// The transit leg to them and the return leg home are added by [`crate::route`].
    pub fn path(&self) -> Option<TaskPath> {
        match self {
            Task::Orbit { center, radius_m } => Some(TaskPath {
                points: circle_points(*center, *radius_m, ORBIT_RING_POINTS),
                closed: true,
            }),
            Task::Patrol { route } => Some(TaskPath {
                points: route.clone(),
                closed: true,
            }),
            // engage: the point is predicted from the world (crate::engage::preview), not
            // committed to by the spec
            Task::Hold {}
            | Task::Rtl {}
            | Task::Track { .. }
            | Task::Collect { .. }
            | Task::Cover { .. }
            | Task::Engage { .. } => None,
        }
    }

    fn validate(&self) -> Result<(), ProposalError> {
        let bad = |m: &str| Err(ProposalError::BadParams(m.to_string()));
        match self {
            Task::Orbit { center, radius_m } => {
                if !center.is_valid() {
                    return bad("orbit center out of range");
                }
                if !(radius_m.is_finite() && *radius_m > 0.0) {
                    return bad("orbit radius must be a positive number of metres");
                }
            }
            Task::Patrol { route } => {
                if route.len() < 2 {
                    return bad("patrol needs a route of at least 2 points");
                }
                if !route.iter().all(LonLat::is_valid) {
                    return bad("patrol point out of range");
                }
            }
            Task::Track { target } if target.as_str().is_empty() => {
                return bad("track needs a target uid");
            }
            Task::Collect {
                area,
                window,
                min_resolution_m,
                product_deadline,
                chosen,
                ..
            } => {
                area.validate().or_else(|e| bad(&e))?;
                window.validate().or_else(|e| bad(&e))?;
                if let Some(r) = min_resolution_m
                    && !(r.is_finite() && *r > 0.0)
                {
                    return bad("min_resolution_m must be a positive number of metres");
                }
                if let Some(d) = product_deadline
                    && *d <= window.from
                {
                    return bad("product_deadline must be after the window start");
                }
                if let Some(c) = chosen
                    && !window.contains(c.collect_time)
                {
                    return bad("chosen collect_time is outside the window");
                }
            }
            Task::Cover {
                area,
                window,
                service,
                min_level,
            } => {
                area.validate().or_else(|e| bad(&e))?;
                window.validate().or_else(|e| bad(&e))?;
                if service.class().trim().is_empty() {
                    return bad("cover needs a service class");
                }
                match (service, min_level) {
                    (_, None) => {}
                    (CoverService::Satcom { .. }, Some(l))
                        if !(l.is_finite() && (0.0..=1.0).contains(l)) =>
                    {
                        return bad("satcom min_level is a fraction of the window, 0..1");
                    }
                    (CoverService::Gnss { .. }, Some(l))
                        if !(l.is_finite() && *l >= 0.0 && l.fract() == 0.0) =>
                    {
                        return bad("gnss min_level is a whole number of satellites");
                    }
                    _ => {}
                }
            }
            Task::Engage {
                track,
                effect_radius_m,
                window,
                ..
            } => {
                if track.trim().is_empty() {
                    return bad("engage needs a track id");
                }
                if !(effect_radius_m.is_finite() && *effect_radius_m > 0.0) {
                    return bad("effect_radius_m must be a positive number of metres");
                }
                if let Some(w) = window
                    && let Some(from) = w.from
                    && from >= w.until
                {
                    return bad("engage window must end after it starts");
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// Operator-set limits. They can only tighten what the policy file sets, never loosen it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Constraints {
    /// Endurance reserve after the task, percent of full endurance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserve_pct: Option<f64>,
    /// Time on task for the reserves estimate, seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<u32>,
}

/// Why an intent is wanted. Required for agents (the type has no empty value), optional for
/// humans.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Rationale(String);

impl Rationale {
    pub fn new(text: impl Into<String>) -> Result<Self, ProposalError> {
        let s: String = text.into();
        if s.trim().is_empty() {
            Err(ProposalError::EmptyRationale)
        } else {
            Ok(Rationale(s))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Rationale {
    type Error = ProposalError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Rationale::new(s)
    }
}

impl From<Rationale> for String {
    fn from(r: Rationale) -> String {
        r.0
    }
}

/// Who asked. Set by the engine from the path the request came in on (an operator's
/// authority or an agent port), never from the request body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    Human { id: OperatorId },
    Agent { id: AgentId },
}

impl Source {
    pub fn is_agent(&self) -> bool {
        matches!(self, Source::Agent { .. })
    }
}

/// A request to act, before the engine gives it an id. Deliberately has no `source` field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    #[serde(flatten)]
    pub task: Task,
    /// The vehicles, for vehicle kinds. Space kinds (collect, cover) take none; the field
    /// may then be omitted.
    #[serde(default)]
    pub uids: Vec<Uid>,
    #[serde(default)]
    pub constraints: Constraints,
    #[serde(default)]
    pub rationale: Option<Rationale>,
}

impl Proposal {
    pub fn new(task: Task, uids: Vec<Uid>) -> Self {
        Proposal {
            task,
            uids,
            constraints: Constraints::default(),
            rationale: None,
        }
    }

    /// A collect or cover: no vehicles.
    pub fn space(task: Task) -> Self {
        Proposal::new(task, Vec::new())
    }

    pub fn validate(&self) -> Result<(), ProposalError> {
        match self.task.targets() {
            Targets::Space if !self.uids.is_empty() => {
                return Err(ProposalError::VehiclesOnSpaceTask(self.task.kind()));
            }
            Targets::Space => return self.task.validate(),
            Targets::Vehicles if self.uids.is_empty() => {
                return Err(ProposalError::NoVehicles);
            }
            Targets::Vehicles => {}
        }
        let mut seen = self.uids.clone();
        seen.sort();
        seen.dedup();
        if seen.len() != self.uids.len() {
            return Err(ProposalError::DuplicateVehicle);
        }
        if matches!(self.task, Task::Engage { .. }) && self.uids.len() != 1 {
            return Err(ProposalError::BadParams(
                "engage names exactly one armed platform".into(),
            ));
        }
        if let Task::Track { target } = &self.task
            && self.uids.contains(target)
        {
            return Err(ProposalError::BadParams(
                "a tracker can't track itself".into(),
            ));
        }
        if let Some(r) = self.constraints.reserve_pct
            && !(r.is_finite() && (0.0..=100.0).contains(&r))
        {
            return Err(ProposalError::BadParams(
                "reserve_pct must be 0..100".into(),
            ));
        }
        self.task.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProposalError {
    #[error("an intent needs at least one vehicle uid")]
    NoVehicles,
    #[error("a vehicle uid appears twice")]
    DuplicateVehicle,
    #[error("rationale must not be empty")]
    EmptyRationale,
    #[error("bad params: {0}")]
    BadParams(String),
    #[error("{} requests satellites, not fleet vehicles: uids must be empty", .0.as_str())]
    VehiclesOnSpaceTask(TaskKind),
}

/// The immutable request, as logged in `intent.created`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IntentSpec {
    pub id: IntentId,
    pub created_t: Time,
    pub source: Source,
    #[serde(flatten)]
    pub task: Task,
    pub uids: Vec<Uid>,
    pub constraints: Constraints,
    pub rationale: Option<Rationale>,
}

impl IntentSpec {
    pub fn kind(&self) -> TaskKind {
        self.task.kind()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Proposed,
    Checked,
    AutoApproved,
    PendingApproval,
    Approved,
    Rejected,
    Expired,
    Voided,
    Dispatched,
    /// Engage only: the abstract effect was released; the assessment follows.
    Effected,
    /// Engage only: stopped after dispatch and before the effect (an operator's abort, a
    /// void when the picture changed, link loss, the window closing); the platform was sent
    /// its recorded fallback.
    Aborted,
    Done,
    Failed,
}

impl State {
    pub const ALL: [State; 13] = [
        State::Proposed,
        State::Checked,
        State::AutoApproved,
        State::PendingApproval,
        State::Approved,
        State::Rejected,
        State::Expired,
        State::Voided,
        State::Dispatched,
        State::Effected,
        State::Aborted,
        State::Done,
        State::Failed,
    ];

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            State::Rejected
                | State::Expired
                | State::Voided
                | State::Aborted
                | State::Done
                | State::Failed
        )
    }
}

/// The events that move an intent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transition {
    /// Rules evaluated.
    Check,
    /// A rule failed: rejected by the rules, before policy.
    ChecksFailed,
    /// Policy tier auto.
    AutoApprove,
    /// Policy tier human: into the inbox.
    RequireApproval,
    /// A human approved (needs a `HumanAuthority`).
    Approve,
    /// Policy tier human, and the human who submitted it is the authorizing human: the
    /// submission itself is the approval (needs the submitter's `HumanAuthority`; never
    /// for an agent's proposal).
    AuthorizeAtSubmission,
    /// A human rejected.
    Reject,
    /// Nobody decided in time.
    Expire,
    /// Withdrawn, cancelled, or invalidated by a changed world.
    Void,
    /// Handed to the task layer.
    Dispatch,
    /// Engage: the abstract effect is released (picture held, platform at the point).
    Release,
    /// Engage: stopped after dispatch, before the effect; the recorded fallback applies.
    Abort,
    Complete,
    Fail,
}

impl Transition {
    pub const ALL: [Transition; 14] = [
        Transition::Check,
        Transition::ChecksFailed,
        Transition::AutoApprove,
        Transition::RequireApproval,
        Transition::Approve,
        Transition::AuthorizeAtSubmission,
        Transition::Reject,
        Transition::Expire,
        Transition::Void,
        Transition::Dispatch,
        Transition::Release,
        Transition::Abort,
        Transition::Complete,
        Transition::Fail,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("illegal transition {transition:?} from state {from:?}")]
pub struct IllegalTransition {
    pub from: State,
    pub transition: Transition,
}

/// The state machine:
///
/// ```text
/// proposed -check-> checked -checks_failed-> rejected
///                           -auto_approve-> auto_approved
///                           -require_approval-> pending_approval
///                           -authorize_at_submission-> approved
/// pending_approval -approve-> approved | -reject-> rejected | -expire-> expired
/// pending_approval | auto_approved | approved -void-> voided
/// auto_approved | approved -dispatch-> dispatched -complete-> done | -fail-> failed
/// dispatched -release-> effected -complete-> done | -fail-> failed       (engage)
/// dispatched -abort-> aborted                                            (engage)
/// ```
///
/// Approved is reachable only through `Approve` and `AuthorizeAtSubmission`, which the
/// engine performs only with a `HumanAuthority` (the second only for the submitter's own
/// intent, on the human submit path).
pub fn next_state(from: State, transition: Transition) -> Result<State, IllegalTransition> {
    use State as S;
    use Transition as T;
    let to = match (from, transition) {
        (S::Proposed, T::Check) => S::Checked,
        (S::Checked, T::ChecksFailed) => S::Rejected,
        (S::Checked, T::AutoApprove) => S::AutoApproved,
        (S::Checked, T::RequireApproval) => S::PendingApproval,
        (S::Checked, T::AuthorizeAtSubmission) => S::Approved,
        (S::PendingApproval, T::Approve) => S::Approved,
        (S::PendingApproval, T::Reject) => S::Rejected,
        (S::PendingApproval, T::Expire) => S::Expired,
        (S::PendingApproval | S::AutoApproved | S::Approved, T::Void) => S::Voided,
        (S::AutoApproved | S::Approved, T::Dispatch) => S::Dispatched,
        (S::Dispatched, T::Complete) => S::Done,
        (S::Dispatched, T::Fail) => S::Failed,
        (S::Dispatched, T::Release) => S::Effected,
        (S::Dispatched, T::Abort) => S::Aborted,
        (S::Effected, T::Complete) => S::Done,
        (S::Effected, T::Fail) => S::Failed,
        _ => return Err(IllegalTransition { from, transition }),
    };
    Ok(to)
}

/// A recorded human approval. Serialize-only: it can't be fabricated from JSON.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Approval {
    pub by: OperatorId,
    pub session: SessionId,
    pub t: Time,
    /// The approver proposed the intent too (no second person looked at it).
    pub self_approved: bool,
    /// The approval is the submission itself: a human submitted the intent and its policy
    /// tier lets the submitter authorize (`approval.at_submission`). Always self-approved.
    pub at_submission: bool,
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StateChange {
    pub state: State,
    pub t: Time,
    pub reason: String,
}

/// An intent and its decision trail. Serialize-only, read-only outside the engine.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Intent {
    #[serde(flatten)]
    spec: IntentSpec,
    state: State,
    checks: Vec<CheckResult>,
    policy: Option<PolicyDecision>,
    approval: Option<Approval>,
    /// For pending approvals: when it expires.
    expires_t: Option<Time>,
    history: Vec<StateChange>,
    /// Collect and cover: the space answer the last check read (ranked options and
    /// exclusions for a collect; coverage for a cover). Also in the log as `space.query`.
    #[serde(skip_serializing_if = "Option::is_none")]
    space: Option<Box<SpaceAnswer>>,
    /// Signatures recorded before the last one, when the tier needs more than one approver
    /// (two-person approval): each a distinct operator, none the proposer where the tier
    /// excludes the proposer. The last signature is `approval`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    concurrences: Vec<Approval>,
    /// Engage: the preview, the picture held at approval, the last watch verdict, the
    /// effect, a stop, and the assessment.
    #[serde(skip_serializing_if = "Option::is_none")]
    engagement: Option<Box<Engagement>>,
}

impl Intent {
    pub(crate) fn new(spec: IntentSpec) -> Self {
        let t = spec.created_t;
        Intent {
            spec,
            state: State::Proposed,
            checks: Vec::new(),
            policy: None,
            approval: None,
            expires_t: None,
            history: vec![StateChange {
                state: State::Proposed,
                t,
                reason: "created".into(),
            }],
            space: None,
            concurrences: Vec::new(),
            engagement: None,
        }
    }

    pub fn id(&self) -> IntentId {
        self.spec.id
    }
    pub fn spec(&self) -> &IntentSpec {
        &self.spec
    }
    pub fn state(&self) -> State {
        self.state
    }
    pub fn checks(&self) -> &[CheckResult] {
        &self.checks
    }
    pub fn policy(&self) -> Option<&PolicyDecision> {
        self.policy.as_ref()
    }
    pub fn approval(&self) -> Option<&Approval> {
        self.approval.as_ref()
    }
    pub fn expires_t(&self) -> Option<Time> {
        self.expires_t
    }
    pub fn history(&self) -> &[StateChange] {
        &self.history
    }
    /// Collect and cover: the space answer the last check read.
    pub fn space_answer(&self) -> Option<&SpaceAnswer> {
        self.space.as_deref()
    }
    /// Signatures before the last (see the field).
    pub fn concurrences(&self) -> &[Approval] {
        &self.concurrences
    }
    /// Engage: where the engagement stands.
    pub fn engagement(&self) -> Option<&Engagement> {
        self.engagement.as_deref()
    }

    pub(crate) fn apply(
        &mut self,
        transition: Transition,
        t: Time,
        reason: String,
    ) -> Result<State, IllegalTransition> {
        let to = next_state(self.state, transition)?;
        self.state = to;
        self.history.push(StateChange {
            state: to,
            t,
            reason,
        });
        Ok(to)
    }

    pub(crate) fn set_checks(&mut self, checks: Vec<CheckResult>) {
        self.checks = checks;
    }
    pub(crate) fn set_policy(&mut self, p: PolicyDecision) {
        self.policy = Some(p);
    }
    pub(crate) fn set_approval(&mut self, a: Approval) {
        self.approval = Some(a);
    }
    pub(crate) fn set_space(&mut self, a: Option<Box<SpaceAnswer>>) {
        self.space = a;
    }
    pub(crate) fn set_expires(&mut self, t: Time) {
        self.expires_t = Some(t);
    }
    pub(crate) fn push_concurrence(&mut self, a: Approval) {
        self.concurrences.push(a);
    }
    /// Drop the signatures so far (an engagement whose picture changed); returns who signed.
    pub(crate) fn clear_concurrences(&mut self) -> Vec<OperatorId> {
        std::mem::take(&mut self.concurrences)
            .into_iter()
            .map(|a| a.by)
            .collect()
    }
    pub(crate) fn engagement_mut(&mut self) -> &mut Engagement {
        self.engagement.get_or_insert_with(Default::default)
    }
}
