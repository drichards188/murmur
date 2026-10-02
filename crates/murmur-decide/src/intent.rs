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
    // Phase E adds Engage. When it lands, `is_consequential` returns true for it, which makes
    // "not evaluated" rule outcomes blocking.
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Orbit => "orbit",
            TaskKind::Patrol => "patrol",
            TaskKind::Hold => "hold",
            TaskKind::Rtl => "rtl",
            TaskKind::Track => "track",
        }
    }

    /// Kinds for which every rule must actually evaluate (a "not evaluated" outcome blocks).
    pub fn is_consequential(self) -> bool {
        false
    }
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
}

impl Task {
    pub fn kind(&self) -> TaskKind {
        match self {
            Task::Orbit { .. } => TaskKind::Orbit,
            Task::Patrol { .. } => TaskKind::Patrol,
            Task::Hold {} => TaskKind::Hold,
            Task::Rtl {} => TaskKind::Rtl,
            Task::Track { .. } => TaskKind::Track,
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
            Task::Hold {} | Task::Rtl {} | Task::Track { .. } => None,
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

    pub fn validate(&self) -> Result<(), ProposalError> {
        if self.uids.is_empty() {
            return Err(ProposalError::NoVehicles);
        }
        let mut seen = self.uids.clone();
        seen.sort();
        seen.dedup();
        if seen.len() != self.uids.len() {
            return Err(ProposalError::DuplicateVehicle);
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
    Done,
    Failed,
}

impl State {
    pub const ALL: [State; 11] = [
        State::Proposed,
        State::Checked,
        State::AutoApproved,
        State::PendingApproval,
        State::Approved,
        State::Rejected,
        State::Expired,
        State::Voided,
        State::Dispatched,
        State::Done,
        State::Failed,
    ];

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            State::Rejected | State::Expired | State::Voided | State::Done | State::Failed
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
    /// A human rejected.
    Reject,
    /// Nobody decided in time.
    Expire,
    /// Withdrawn, cancelled, or invalidated by a changed world.
    Void,
    /// Handed to the task layer.
    Dispatch,
    Complete,
    Fail,
}

impl Transition {
    pub const ALL: [Transition; 11] = [
        Transition::Check,
        Transition::ChecksFailed,
        Transition::AutoApprove,
        Transition::RequireApproval,
        Transition::Approve,
        Transition::Reject,
        Transition::Expire,
        Transition::Void,
        Transition::Dispatch,
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
/// pending_approval -approve-> approved | -reject-> rejected | -expire-> expired
/// pending_approval | auto_approved | approved -void-> voided
/// auto_approved | approved -dispatch-> dispatched -complete-> done | -fail-> failed
/// ```
///
/// Approved is reachable only through `Approve`, which the engine performs only with a
/// `HumanAuthority`.
pub fn next_state(from: State, transition: Transition) -> Result<State, IllegalTransition> {
    use State as S;
    use Transition as T;
    let to = match (from, transition) {
        (S::Proposed, T::Check) => S::Checked,
        (S::Checked, T::ChecksFailed) => S::Rejected,
        (S::Checked, T::AutoApprove) => S::AutoApproved,
        (S::Checked, T::RequireApproval) => S::PendingApproval,
        (S::PendingApproval, T::Approve) => S::Approved,
        (S::PendingApproval, T::Reject) => S::Rejected,
        (S::PendingApproval, T::Expire) => S::Expired,
        (S::PendingApproval | S::AutoApproved | S::Approved, T::Void) => S::Voided,
        (S::AutoApproved | S::Approved, T::Dispatch) => S::Dispatched,
        (S::Dispatched, T::Complete) => S::Done,
        (S::Dispatched, T::Fail) => S::Failed,
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
    pub(crate) fn set_expires(&mut self, t: Time) {
        self.expires_t = Some(t);
    }
}
