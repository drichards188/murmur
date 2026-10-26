//! The engine: owns the intents, applies rules and policy, enforces the state machine, and
//! writes every step to the audit log before taking it.

use std::collections::{BTreeMap, BTreeSet};

use crate::areas::AreaSet;
use crate::audit::{AuditSink, CheckPhase, Event, EventBody, LoggedAreas, OperatorView};
use crate::authority::{
    AuthMethod, AuthMode, EngagementAuthority, Grants, HumanAuthority, OperatorSession, Roster,
    STEP_UP_MAX_AGE_MS, StepUp, StepUpRef,
};
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
use crate::queue::{
    self as queue, Basis, CarriedApproval, OpenIntent, QueueState, Reach, RecheckRecord,
    ResumedFrom, Verdict,
};
use crate::rules::{self, CheckResult, OPERATOR_AUTH_SINCE, Outcome};
use serde::Serialize;
use crate::space::{
    self, ChosenCollect, CollectOrder, CoverOrder, ScenarioMap, SegmentId, SpaceAnswer,
    SpaceOracle, SpaceOrder, from_utc, matched_option, query_for,
};
use crate::terrain::{NoTerrain, TerrainOracle};
use crate::time::{Clock, Time};
use crate::world::{AisIdentity, LoggedInputs, SpaceInputs, TrackFacts, WorldSnapshot};

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
    /// A report or stop that does not fit what the intent was dispatched with (replay would
    /// reject the log): nothing was logged.
    #[error("intent {id}: {why}")]
    NotItsOrder { id: IntentId, why: String },
    /// The roster a run was started with is not one a bridge could be configured with.
    #[error("operator roster: {0}")]
    Roster(String),
    /// A session for an operator the run's roster does not list, or with a grant it does not
    /// give them: nothing was logged.
    #[error("operator {operator}: {why}")]
    NotOnRoster { operator: OperatorId, why: String },
    /// Rules 6: an acceptance of a lost friendly the engine can't take (the vehicle is not
    /// lost, or not since the report it names; the engagement already has signatures):
    /// nothing was logged.
    #[error("vehicle {uid}: {why}")]
    NotAccepted { uid: Uid, why: String },
    /// Rules 16: an acceptance of a lost contact the engine does not take (not lost since that
    /// fix, not an engagement, after a signature): nothing was logged.
    #[error("lost contact {track}: {why}")]
    NotAcceptedContact { track: String, why: String },
    /// Rules 19: an acceptance of a live track as behaving now the engine refuses (not in the
    /// picture, out of view, no velocity, not protected, not an engagement, after a signature,
    /// before rules 19), or a revocation of one not in force: nothing was logged.
    #[error("track {track}: {why}")]
    NotAcceptedTrack { track: String, why: String },
    /// Rules 8: a session or step-up the run's authentication does not allow (an asserted
    /// session in a credentials run, a step-up in a dev run, a step-up for another session
    /// or too old): nothing was logged.
    #[error("operator {operator}: {why}")]
    NotAuthenticated { operator: OperatorId, why: String },
    /// Rules 8, a credentials run: the decision marks this approval consequential, and the
    /// signer did not re-enter their credential for it (logged as `approval.refused`).
    #[error("intent {id}: {by} must re-enter their credential to sign this (step-up)")]
    StepUpRequired { id: IntentId, by: OperatorId },
    /// Rules 9: an identification the engine can't take (no such track, no declaration in
    /// force to confirm or revoke, a flag not in force, an act reported twice, rules before 9):
    /// nothing was logged.
    #[error("identification: {0}")]
    Identification(String),
    /// Rules 12: a write-off of a restore the engine can't take (nothing owed, no note, an
    /// engagement under way on the platform, rules before 12): nothing was logged.
    #[error("write-off: {0}")]
    NotWrittenOff(String),
    /// Rules 15: an un-spend the engine can't take (no note, a platform not spent, one not heard
    /// since its effect, rules before 15): nothing was logged.
    #[error("un-spend: {0}")]
    NotUnspent(String),
    /// Rules 12: what a start carried over can't go into `run.started` (a log not named by its
    /// bare file name, rules before 12): the engine did not start.
    #[error("carry-over: {0}")]
    Carry(String),
    /// Rules 13: a space segment the engine can't take (pinned to another time shift than
    /// the one loaded first): nothing was logged.
    #[error("space segment: {0}")]
    SpaceSegment(String),
}

/// Rules 12: what a start carries over from earlier runs' logs ([`Engine::start`]): the entries of
/// `run.started`'s `carried_from`, and (rules 15) the station's `.last-run` pointer, by its file
/// name, logged as `run.started`'s `pointer`; and what else the start records of the bridge it
/// runs in (rules 16: its tracker's drop age). A list of entries alone is a carry without a
/// pointer (a test).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StartCarry {
    pub pointer: Option<String>,
    pub from: Vec<crate::replay::CarriedFrom>,
    /// Rules 16: the bridge's tracker's drop age (s), logged as `run.started`'s
    /// `track_drop_s`; none without a tracker that drops (a test).
    pub track_drop_s: Option<f64>,
}

impl From<Vec<crate::replay::CarriedFrom>> for StartCarry {
    fn from(from: Vec<crate::replay::CarriedFrom>) -> Self {
        StartCarry { pointer: None, from, track_drop_s: None }
    }
}

/// Rules 12: what an operator writes off ([`Engine::write_off_restore`]): the lost-link values
/// owed back to one platform, or an earlier log whose carry failed (by its file name).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteOff {
    Platform(Uid),
    Log(String),
}

/// Rules 9: what [`Engine::record_act`] did with an observed act: the verdict, and the rule
/// declaration or flag it made.
#[derive(Clone, Debug, PartialEq)]
pub struct ActOutcome {
    pub verdict: crate::ident::ActVerdict,
    pub candidates: Vec<crate::ident::Candidate>,
    pub declaration: Option<crate::ident::Declaration>,
    pub flag: Option<crate::ident::Flag>,
}

/// Rules 9: what an operator's identification action did: the declaration it made or
/// changed, and the engagements a revocation stopped (the fallbacks the bridge commands,
/// the ones voided before dispatch).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IdentOutcome {
    pub declaration: Option<crate::ident::Declaration>,
    pub revoked: Option<u64>,
    pub fallbacks: Vec<FallbackOrder>,
    pub voided: Vec<IntentId>,
}

/// A space answer computed outside the engine's lock (the bridge runs the SGP4 work first,
/// then takes the lock): the next check of the same task uses it if it was computed within
/// [`PREPARED_SPACE_MAX_AGE_MS`] of that check.
#[derive(Clone, Debug)]
pub struct PreparedSpace {
    pub task: Task,
    /// When it was computed (engine time): logged with the answer from rules 13.
    pub t: Time,
    pub inputs: SpaceInputs,
}

/// How old a prepared space answer may be when the check that uses it runs, ms.
pub use crate::rules::PREPARED_SPACE_MAX_AGE_MS;

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

/// [`prepare_space`] with the query's request time given (scenario time) instead of taken
/// from `t`: a dispatched collect is re-checked at the request time of the answer it was
/// tasked on ([`Engine::recheck_space`]), so a later answer differs only where the segment
/// does, not by the lead time spent since.
pub fn prepare_space_at(
    oracle: &dyn SpaceOracle,
    task: &Task,
    t: Time,
    now: Time,
    rules: &crate::rules::RulesConfig,
) -> Result<PreparedSpace, EngineError> {
    Ok(PreparedSpace {
        task: task.clone(),
        t,
        inputs: space_inputs_at(oracle, task, now, t, rules)?,
    })
}

fn space_inputs(
    oracle: &dyn SpaceOracle,
    task: &Task,
    t: Time,
    rules: &crate::rules::RulesConfig,
) -> Result<SpaceInputs, EngineError> {
    space_inputs_at(oracle, task, oracle.scenario_time(t), t, rules)
}

/// The task's space question asked at scenario time `now`, computed at engine time `t`
/// (logged with the answer from rules 13).
fn space_inputs_at(
    oracle: &dyn SpaceOracle,
    task: &Task,
    now: Time,
    t: Time,
    rules: &crate::rules::RulesConfig,
) -> Result<SpaceInputs, EngineError> {
    let query = match query_for(task, now, &rules.space_rules()) {
        Some(Ok(q)) => q,
        Some(Err(reason)) => return Ok(SpaceInputs::Unavailable { reason }),
        None => return Ok(SpaceInputs::NotQueried),
    };
    Ok(match oracle.answer(&query) {
        Ok(answer) => SpaceInputs::Answered {
            sha256: answer.content_hash()?,
            answer: Box::new(answer),
            computed_t: rules.space_replay().then_some(t),
        },
        Err(reason) => SpaceInputs::Unavailable { reason },
    })
}

/// Rules 11: what [`Engine::recheck_space`] found a dispatched collect or cover to be
/// against a later space answer.
#[derive(Clone, Debug, PartialEq)]
pub enum SpaceRecheck {
    /// The later answer is the one the task runs on: nothing logged.
    Unchanged,
    /// A new answer, and the task can still be met: `check.result` (phase `dispatched`) and
    /// `op.state rechecked`; a cover's watch takes the new answer's outages.
    Holds { detail: String },
    /// A new answer, and the task can no longer be met: `check.result`, `op.state missed`
    /// (`no longer feasible: ...`, a collect's with the fresh options a human may choose
    /// from) and the intent failed.
    Lost { detail: String },
    /// The later answer could not be computed (the query failed on the segment): nothing is
    /// known and nothing is logged; the task runs on, and the operator should hear of it.
    Unknown { reason: String },
}

/// Who reports an op state: the bridge (the task layer, the space executor), the bridge
/// ahead of a vehicle start (rules 7), or the engine itself (rules 11: a space task's cancel
/// and re-check).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpReport {
    Bridge,
    Ahead,
    Engine,
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

/// Rules 5: the orders a human approved that wait on `spec`'s vehicles, other than `spec`
/// itself, by vehicle (the lowest id when there are several): queued, or dispatched with no
/// vehicle having started them yet (an agent's order there would replace them before they
/// run). The engine logs this with each decision (`waiting`); replay recomputes it from its
/// own intents. `intents`: (id, spec, state, approved by a human, some vehicle started it).
pub(crate) fn waiting_vehicles<'a>(
    intents: impl Iterator<Item = (IntentId, &'a IntentSpec, State, bool, bool)>,
    spec: &IntentSpec,
) -> BTreeMap<Uid, IntentId> {
    let mut out: BTreeMap<Uid, IntentId> = BTreeMap::new();
    for (id, s, state, by_human, started) in intents {
        let waits = match state {
            State::Queued => true,
            State::Dispatched => !started,
            _ => false,
        };
        if id == spec.id || !waits || !by_human || s.kind() == TaskKind::Engage {
            continue;
        }
        for u in spec.uids.iter().filter(|u| s.uids.contains(u)) {
            out.entry(u.clone()).or_insert(id);
        }
    }
    out
}

/// Rules 12: the orders queued on `spec`'s vehicles, other than `spec` itself, by id, with
/// their kind and the vehicles they share with it: they still stand (each goes when its
/// vehicles can be reached, unless voided or cancelled). Replay recomputes it from its own
/// intents.
pub(crate) fn queued_on<'a>(
    intents: impl Iterator<Item = (IntentId, &'a IntentSpec, State)>,
    spec: &IntentSpec,
) -> Vec<(IntentId, TaskKind, Vec<Uid>)> {
    intents
        .filter(|(id, _, state)| *id != spec.id && *state == State::Queued)
        .filter_map(|(id, s, _)| {
            let shared: Vec<Uid> = s.uids.iter().filter(|u| spec.uids.contains(u)).cloned().collect();
            (!shared.is_empty()).then(|| (id, s.kind(), shared))
        })
        .collect()
}

/// Rules 7: the reason a mission of a kind the policy never queues is refused (voided)
/// instead of sent while a vehicle of its is owed its own lost-link values that hold it
/// (`owed`, the ledger's on no identity: values carried over hold whatever vehicle has the
/// uid), if it is: whether the engagements in play on its vehicles (`engaged`) are stopped by
/// it or the restore is pending, and that a hold or rtl goes at once. Rules 12 (murmur-54l):
/// it also names the orders still queued on its vehicles (`standing`, [`queued_on`]): the
/// refusal leaves them in place, and an operator who meant this order to replace one cancels
/// it. Replay recomputes it. None for a hold or rtl, or with nothing owed that holds.
pub(crate) fn refusal_reason(
    spec: &IntentSpec,
    owed: &BTreeMap<Uid, queue::RestoreOwed>,
    engaged: &BTreeSet<IntentId>,
    standing: &[(IntentId, TaskKind, Vec<Uid>)],
    rules: &rules::RulesConfig,
) -> Option<String> {
    // rules 15: an engagement's own fact (a failed carry that may owe its platform values) is
    // its fallback_preload check's, never a mission's refusal
    if matches!(spec.kind(), TaskKind::Hold | TaskKind::Rtl | TaskKind::Engage) {
        return None;
    }
    let (uid, o) = owed.iter().find(|(_, o)| o.holds())?;
    let values: Vec<String> = o.values.iter().map(|(n, v)| format!("{n} {v}")).collect();
    let by: Vec<String> = o.by.iter().map(ToString::to_string).collect();
    let engaged: Vec<String> = engaged.iter().map(ToString::to_string).collect();
    let kind = spec.kind().as_str();
    let mut out = match (&o.carried, engaged.is_empty()) {
        (Some(c), true) => format!(
            "{REFUSED}vehicle {uid}'s own lost-link failsafe ({}) is not confirmed back since it was carried over from {}, and the policy queues no {kind} order to wait for the restore: give it again once the restore confirms, or have an operator write the values off if this is not the vehicle they were preloaded on (a hold or rtl goes at once)",
            o.values_text(),
            c.from
        ),
        (Some(c), false) => format!(
            "{REFUSED}vehicle {uid}'s own lost-link failsafe ({}) is not confirmed back since it was carried over from {}; this order stops engagement {} (superseded, no fallback), but the policy queues no {kind} order to wait for the restore: give it again once the restore confirms (a hold or rtl goes at once)",
            o.values_text(),
            c.from,
            engaged.join(", ")
        ),
        (None, true) => format!(
            "{REFUSED}vehicle {uid}'s own lost-link failsafe ({}) is not confirmed back since engagement {} preloaded it, and the policy queues no {kind} order to wait for the restore: give it again once the restore confirms (a hold or rtl goes at once)",
            values.join(", "),
            by.join(", ")
        ),
        (None, false) => format!(
            "{REFUSED}vehicle {uid} carries engagement {}'s lost-link action (its own failsafe {} owed back); this order stops engagement {} (superseded, no fallback) and the restore of its own values starts now (as soon as the platform is heard), but the policy queues no {kind} order to wait for it: give it again once the restore confirms (a hold or rtl goes at once)",
            by.join(", "),
            values.join(", "),
            engaged.join(", ")
        ),
    };
    // rules 12: the refusal replaces nothing: an older order queued on its vehicles still goes
    if rules.followups() && !standing.is_empty() {
        let list: Vec<String> = standing
            .iter()
            .map(|(id, kind, uids)| {
                let uids: Vec<String> = uids.iter().map(ToString::to_string).collect();
                format!("{id} ({}, vehicle {})", kind.as_str(), uids.join(", "))
            })
            .collect();
        let (each, one) = if standing.len() == 1 { ("it", "it") } else { ("each", "one") };
        out.push_str(&format!(
            "; this refusal replaces no order: still standing, queued on its vehicles: {} ({each} goes when its vehicles can be reached; cancel {one} in the Queue if this order was to replace it)",
            list.join(", ")
        ));
    }
    Some(out)
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

/// What [`Engine::dispatch_or_queue`] did with an approved intent.
#[derive(Clone, Debug, PartialEq)]
pub enum Routed {
    /// Every vehicle could be reached: handed on now.
    Dispatched(Order),
    /// Some vehicle can't be reached now: held (`intent.queued`) until `expires_t`.
    Queued { expires_t: Time, reach: Vec<Reach> },
    /// Rules 7: a mission of a kind the policy never queues, for a vehicle still owed its own
    /// lost-link values after an engagement: it has nowhere to wait, so it is voided (with
    /// `reason`) rather than sent over the engagement's lost-link action.
    Voided { reason: String },
}

/// What [`Engine::dequeue`] did with a queued intent.
#[derive(Clone, Debug, PartialEq)]
pub enum Dequeued {
    /// A vehicle still can't be reached: nothing logged, still queued.
    Waiting(Vec<Reach>),
    /// Re-checked and dispatched: execute the order.
    Dispatched(Order),
    /// Re-checked: a human must decide again; it waits in the inbox.
    Inbox,
    /// Re-checked: a check failed on the world now; voided.
    Voided,
    /// Its deadline passed first; expired.
    Expired,
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
    /// Who may open an operator session, and with engagement authority (`run.started`).
    roster: Roster,
    /// Rules 8: how this run's operators are authenticated (the roster's logged `auth`);
    /// None before rules 8.
    auth: Option<AuthMode>,
    /// Rules 8: sessions ended (`session.closed`): nothing more is logged under them.
    closed: BTreeSet<SessionId>,
    /// The intent dispatched last: the only one that may supersede an engagement.
    last_dispatched: Option<IntentId>,
    /// Per vehicle, the intent dispatched to it last: an older one's order there was replaced.
    newest_on: BTreeMap<Uid, IntentId>,
    /// Rules 5: when each vehicle was last logged rebooting (`vehicle.rebooted`).
    rebooted: BTreeMap<Uid, Time>,
    /// Rules 6: the platforms' own lost-link values owed back, as the log has them (the
    /// ledger replay keeps): rules 7 holds a mission for a vehicle still owed them.
    restores: crate::replay::Restores,
    /// Rules 7: the intent queued last: like the one dispatched last, it may supersede an
    /// engagement on its vehicles (the bridge does it in the same step as the queueing).
    last_queued: Option<IntentId>,
    /// Rules 7: the order whose dispatch or queueing is the step in progress, and the seq of
    /// that step's last event (its transition, or the last engagement it stopped): it
    /// supersedes engagements only right there, nothing else logged in between.
    step: Option<(IntentId, u64)>,
    /// Rules 9: the identification ledger as the log has it (declarations in force, flags,
    /// acts seen): every decision's track picture is filled from it.
    ident: crate::ident::Ledger,
    /// Rules 9: each engagement in play's last picture (its world as logged), for what a
    /// revoked declaration changes.
    last_pictures: BTreeMap<IntentId, WorldSnapshot>,
    /// Rules 10: the SHA-256 of the `run.started` line, which every passkey challenge binds
    /// to (an assertion from another run does not verify here).
    run_hash: String,
    /// Rules 10: the passkeys this run knows (its roster's, and those registered since), by
    /// credential id.
    passkeys: BTreeMap<String, KeySeen>,
    /// Rules 10: the nonce of every passkey use logged: each challenge is answered once.
    used_nonces: BTreeSet<String>,
    /// Rules 16: the contacts the tracker dropped that the engine holds as lost
    /// (`track.dropped`, less `track.reacquired`): every engagement's keep-out counts them.
    lost: crate::lost::LostLedger,
}

/// Rules 10: a passkey as the engine follows it: whose, its public key, the last counter
/// logged for it, and whether it was removed.
#[derive(Clone, Debug)]
struct KeySeen {
    key: crate::passkey::PasskeyKey,
    removed: bool,
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
    /// Start a run: logs `run.started` with the full policy and the operator roster (only
    /// operators on it can open a session, with engagement authority only those on its
    /// engagement list). The area set is logged (as `areas.loaded`) the first time a
    /// decision reads one; [`Engine::with_areas`] logs it in `run.started` instead.
    pub fn new(
        policy: Policy,
        roster: Roster,
        clock: Box<dyn Clock + Send>,
        sink: Box<dyn AuditSink + Send>,
    ) -> Result<Self, EngineError> {
        Self::with_areas(policy, None, roster, clock, sink)
    }

    /// Start a run with the area set known up front: `run.started` carries the policy, the
    /// roster and the area set with its content hash. Decisions reference the hash; if a
    /// snapshot later brings a different set, it is logged once as `areas.loaded`.
    pub fn with_areas(
        policy: Policy,
        areas: Option<AreaSet>,
        roster: Roster,
        clock: Box<dyn Clock + Send>,
        sink: Box<dyn AuditSink + Send>,
    ) -> Result<Self, EngineError> {
        Self::start(policy, areas, roster, Vec::new(), clock, sink)
    }

    /// [`Engine::with_areas`] for a bridge that carried over what earlier runs' logs still owed
    /// (rules 12, [`crate::rules::REVIEW_FOLLOWUPS_SINCE`]): `carried_from` goes into
    /// `run.started` (the log `.last-run-<port>` named first, then any log whose carry had
    /// failed, each with its head and what it owed), in that one line, and seeds the ledger of
    /// the platforms' own lost-link values owed back. Refused before rules 12 (those carry with
    /// `platform.restore` events, [`Engine::record_platform_restore`]) and for an entry that
    /// does not name a log by its bare file name.
    pub fn start(
        policy: Policy,
        areas: Option<AreaSet>,
        roster: Roster,
        carry: impl Into<StartCarry>,
        clock: Box<dyn Clock + Send>,
        sink: Box<dyn AuditSink + Send>,
    ) -> Result<Self, EngineError> {
        let StartCarry {
            pointer,
            from: mut carried_from,
            track_drop_s,
        } = carry.into();
        // rules 16: the tracker's drop age (no build of older rules logged it)
        let track_drop_s = track_drop_s.filter(|_| policy.rules.lost_contacts());
        // rules 15: the station's pointer, the spent platforms carried, a failed carry's fleet
        // file and a fallback from a pointer that could not be read (no build of older rules
        // logged them)
        let pointer = pointer.filter(|_| policy.rules.carry_followups());
        if !policy.rules.carry_followups() {
            for c in &mut carried_from {
                c.spent.clear();
                c.fleet_read = None;
                c.pointer_failed = None;
            }
        }
        if let Some(p) = &pointer
            && !crate::replay::is_pointer_name(p)
        {
            return Err(EngineError::Carry(format!("the station's pointer is a .last-run file name, not {p:?}")));
        }
        if !carried_from.is_empty() && !policy.rules.followups() {
            return Err(EngineError::Carry(
                "these decision rules carry earlier runs' debts with platform.restore events, not in run.started".into(),
            ));
        }
        if let Some(c) = carried_from.iter().find(|c| !crate::replay::is_log_file_name(&c.log)) {
            return Err(EngineError::Carry(format!(
                "a carried-over log is named by its bare file name, not {:?}",
                c.log
            )));
        }
        roster.validate().map_err(EngineError::Roster)?;
        // rules 8: the roster says how its operators are authenticated (dev when the caller
        // said nothing: names asserted, never evidence about people); older rules never
        // logged it
        let mut roster = roster;
        roster.auth = (policy.rules.rules_version >= OPERATOR_AUTH_SINCE)
            .then(|| roster.auth.unwrap_or(AuthMode::Dev));
        // rules 10: passkeys and a study, only from rules 10 (older rules never logged them),
        // checked against the policy (a study requires passkey step-ups unless overridden)
        if policy.rules.passkeys() {
            roster
                .validate_passkeys(policy.step_up_factor())
                .map_err(EngineError::Roster)?;
        } else {
            roster.passkeys = None;
            roster.study = None;
        }
        // rules 12: a credentials run fixes its scripted (harness-held) operators at the start,
        // an empty list when none is; never before rules 12, never in a dev run
        if policy.rules.followups() && roster.auth == Some(AuthMode::Credentials) {
            roster.scripted.get_or_insert_with(Vec::new);
        } else {
            roster.scripted = None;
        }
        // what the rules read from other sections (rules 9: `[identification]`, which older
        // rules don't have: no build of theirs logged one)
        let mut policy = policy;
        if !policy.rules.rule_id() {
            policy.identification = None;
        }
        // rules 10: the step-up and login factors (no build of older rules logged them)
        if !policy.rules.passkeys() {
            policy.approval.step_up_factor = None;
            policy.approval.login_factor = None;
        }
        policy.sync();
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
            auth: roster.auth,
            roster: roster.clone(),
            closed: BTreeSet::new(),
            last_dispatched: None,
            newest_on: BTreeMap::new(),
            rebooted: BTreeMap::new(),
            restores: crate::replay::Restores::default(),
            last_queued: None,
            step: None,
            ident: crate::ident::Ledger::default(),
            last_pictures: BTreeMap::new(),
            run_hash: String::new(),
            passkeys: BTreeMap::new(),
            used_nonces: BTreeSet::new(),
            lost: crate::lost::LostLedger::default(),
        };
        let policy = e.policy.clone();
        let keys = roster.passkeys.as_ref().map(|p| p.keys.clone()).unwrap_or_default();
        let started = e.emit(EventBody::RunStarted {
            crate_version: env!("CARGO_PKG_VERSION").to_string(),
            rules_version: Some(policy.rules.rules_version),
            policy_schema: Some(crate::rules::POLICY_SCHEMA),
            build: Some(crate::audit::BuildInfo::current()),
            roster: Some(roster),
            policy,
            areas: areas.clone(),
            carried_from: carried_from.clone(),
            pointer,
            track_drop_s,
        })?;
        // rules 12: what earlier runs still owed, owed back from here on (rules 15: and the
        // platforms they had spent stay spent)
        e.restores.carry_at(started, &carried_from);
        e.areas = areas;
        e.run_hash = e.head.clone();
        for k in keys {
            e.passkeys.insert(k.id.clone(), KeySeen { key: k, removed: false });
        }
        Ok(e)
    }

    /// Rules 10: the SHA-256 of this run's `run.started` line (every passkey challenge binds
    /// to it).
    pub fn run_hash(&self) -> &str {
        &self.run_hash
    }

    /// Rules 10: the run's relying party and the passkeys it knows now (its roster's and those
    /// registered since, not removed), public parts only.
    pub fn passkeys(&self) -> Option<crate::passkey::PasskeyRoster> {
        let p = self.roster.passkeys.as_ref()?;
        Some(crate::passkey::PasskeyRoster {
            rp_id: p.rp_id.clone(),
            origins: p.origins.clone(),
            keys: self.passkeys.values().filter(|k| !k.removed).map(|k| k.key.clone()).collect(),
        })
    }

    /// Rules 10: the study this run is, if it is one.
    pub fn study(&self) -> Option<&crate::passkey::Study> {
        self.roster.study.as_deref()
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
    /// `space.loaded` with its identity). Without one, their space check fails closed. Rules
    /// 13: a segment given again keeps the time shift pinned on the first
    /// ([`SpaceOracle::pinned_shift_days`]) and its scenario map: one pinned or mapped
    /// otherwise is refused, nothing logged (the bridge pins once per session and its map is
    /// fixed at its start; replay flags either changing between two `space.loaded`), as is one
    /// whose identity names another pin than it is pinned to.
    pub fn set_space(&mut self, oracle: Box<dyn SpaceOracle + Send>) -> Result<(), EngineError> {
        let days = |d: Option<i64>| d.map_or("none".to_string(), |d| format!("{d:+} d"));
        if self.policy.rules.space_replay() && oracle.segment().pinned_shift_days != oracle.pinned_shift_days() {
            return Err(EngineError::SpaceSegment(format!(
                "its identity names another pin ({}) than it is pinned to ({})",
                days(oracle.segment().pinned_shift_days),
                days(oracle.pinned_shift_days())
            )));
        }
        if self.policy.rules.space_replay()
            && let Some(held) = &self.space
            && held.pinned_shift_days() != oracle.pinned_shift_days()
        {
            return Err(EngineError::SpaceSegment(format!(
                "its pinned time shift can't change within a run (pinned {} at the first load, {} given)",
                days(held.pinned_shift_days()),
                days(oracle.pinned_shift_days())
            )));
        }
        // nor its scenario clock (the bridge's map is fixed at its start)
        if self.policy.rules.space_replay()
            && let Some(held) = &self.space
            && held.scenario_map() != oracle.scenario_map()
        {
            return Err(EngineError::SpaceSegment(format!(
                "its scenario clock can't change within a run ({:?} at the first load, {:?} given)",
                held.scenario_map(),
                oracle.scenario_map()
            )));
        }
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
    ///
    /// Rules 8: the session is logged as opened on an asserted name (`auth: asserted`), so
    /// only a dev run (the roster's `auth`) opens one this way; a credentials run refuses
    /// it ([`EngineError::NotAuthenticated`]) and opens sessions with
    /// [`Engine::open_authenticated_session`] after the bridge checked the password.
    pub fn open_operator_session_with(
        &mut self,
        operator: OperatorId,
        grants: Grants,
    ) -> Result<OperatorSession, EngineError> {
        self.open_session(operator, grants, AuthMethod::Asserted, None)
    }

    /// Rules 8: open a session for an operator whose identity the bridge established by
    /// `method` (a credentials run: `Password`, a person's credential checked at login, or
    /// `Scripted`, a harness-held one checked the same way but never a person's; a dev run:
    /// `Asserted`). Logged as `operator.action session.opened` with
    /// `auth`. The only way into a session in a credentials run; refused when the method is
    /// not the run's ([`EngineError::NotAuthenticated`], nothing logged).
    pub fn open_authenticated_session(
        &mut self,
        operator: OperatorId,
        grants: Grants,
        method: AuthMethod,
    ) -> Result<OperatorSession, EngineError> {
        if self.auth.is_none() {
            return Err(EngineError::NotAuthenticated {
                operator,
                why: "these decision rules record no authentication: open the session with open_operator_session_with".into(),
            });
        }
        if method == AuthMethod::Passkey {
            return Err(EngineError::NotAuthenticated {
                operator,
                why: "a passkey login carries its assertion: open_passkey_session".into(),
            });
        }
        self.open_session(operator, grants, method, None)
    }

    /// Rules 10: open a session for an operator who signed in with a passkey: `proof` is the
    /// assertion the bridge checked, made for a login ([`crate::StepUpFor::Login`]) on a
    /// challenge [`Engine::passkey_challenge`] issued. The engine verifies it again against the
    /// public key the run logged (the credential must be the operator's, its counter must
    /// count up, the challenge unanswered and fresh) and logs it with the session
    /// (`session.opened`, `auth: passkey`, `passkey`). Refused, nothing logged, otherwise, and
    /// when the policy's `login_factor` is `password`.
    pub fn open_passkey_session(
        &mut self,
        operator: OperatorId,
        grants: Grants,
        proof: crate::passkey::PasskeyUse,
    ) -> Result<OperatorSession, EngineError> {
        if proof.purpose != crate::passkey::StepUpFor::Login {
            return Err(EngineError::NotAuthenticated {
                operator,
                why: format!("the passkey assertion was made for {}, not a login", proof.purpose.describe()),
            });
        }
        self.check_passkey_use(&operator, None, &proof)
            .map_err(|why| EngineError::NotAuthenticated { operator: operator.clone(), why })?;
        self.open_session(operator, grants, AuthMethod::Passkey, Some(proof))
    }

    fn open_session(
        &mut self,
        operator: OperatorId,
        grants: Grants,
        method: AuthMethod,
        proof: Option<crate::passkey::PasskeyUse>,
    ) -> Result<OperatorSession, EngineError> {
        if let Some(why) = self.roster.refuses(&operator, grants) {
            return Err(EngineError::NotOnRoster {
                operator,
                why: why.into(),
            });
        }
        if let Some(mode) = self.auth
            && !mode.admits(method)
        {
            return Err(EngineError::NotAuthenticated {
                operator,
                why: format!(
                    "a {} run opens sessions only on {} identities",
                    mode.as_str(),
                    match mode {
                        AuthMode::Credentials => "password-checked, passkey-checked or scripted",
                        AuthMode::Dev => "asserted",
                    }
                ),
            });
        }
        if method == AuthMethod::Passkey && !self.policy.rules.passkeys() {
            return Err(EngineError::NotAuthenticated {
                operator,
                why: "these decision rules have no passkeys".into(),
            });
        }
        // rules 12: a scripted credential opens scripted sessions, a person's a person's, as
        // the roster fixed them at the run's start
        if self.policy.rules.followups()
            && let Some(why) = self.roster.method_refused(&operator, method)
        {
            return Err(EngineError::NotAuthenticated { operator, why });
        }
        // rules 10: how a person may sign in (`[approval] login_factor`)
        if self.auth == Some(AuthMode::Credentials) && !self.policy.login_factor().admits(method) {
            return Err(EngineError::NotAuthenticated {
                operator,
                why: format!(
                    "this run's policy signs people in by {} only ([approval] login_factor)",
                    self.policy.login_factor().as_str()
                ),
            });
        }
        let session = SessionId(self.next_session);
        let counter = proof.as_ref().map(|p| (p.credential.clone(), p.counter, p.nonce.clone()));
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
            auth: self.auth.map(|_| method),
            step_up: None,
            passkey: proof.map(Box::new),
        })?;
        if let Some((id, n, nonce)) = counter {
            self.passkey_used(&id, n, nonce);
        }
        self.next_session += 1;
        Ok(OperatorSession::mint(operator, session, grants, self.auth.map(|_| method)))
    }

    // ---- passkeys (rules 10) ---------------------------------------------------------

    /// Rules 10: the challenge a passkey use for `purpose` by `operator` signs (in `session`;
    /// none for a login), with the bridge's random `nonce`, issued now. Not logged: the use
    /// logs it, and an unanswered challenge leaves no trace; but its time is a floor for every
    /// later event's (a host clock that steps back can't log the use before its challenge).
    /// Refused outside a credentials run of rules 10 that logged a relying party, for an
    /// operator not on the roster, or a session that is not theirs or has ended.
    pub fn passkey_challenge(
        &mut self,
        operator: &OperatorId,
        session: Option<&OperatorSession>,
        purpose: crate::passkey::StepUpFor,
        nonce: String,
    ) -> Result<crate::passkey::Issued, EngineError> {
        let refuse = |why: &str| EngineError::NotAuthenticated { operator: operator.clone(), why: why.into() };
        if !self.policy.rules.passkeys() || self.auth != Some(AuthMode::Credentials) || self.roster.passkeys.is_none() {
            return Err(refuse("this run takes no passkey (a credentials run of rules 10 does)"));
        }
        if self.roster.refuses(operator, Grants::default()).is_some() {
            return Err(refuse("not on the run's roster"));
        }
        let sid = match session {
            Some(s) if s.operator() != operator => return Err(refuse("not this operator's session")),
            Some(s) if self.closed.contains(&s.authority().session()) => return Err(refuse("the session has ended")),
            Some(s) => Some(s.authority().session()),
            None => None,
        };
        if (purpose == crate::passkey::StepUpFor::Login) != sid.is_none() {
            return Err(refuse("a login's challenge has no session; every other one has its session"));
        }
        let issued_t = self.now();
        self.last_t = issued_t;
        let challenge = crate::passkey::challenge(&self.run_hash, sid, operator, &purpose, &nonce, issued_t);
        Ok(crate::passkey::Issued {
            purpose,
            nonce,
            issued_t,
            challenge,
        })
    }

    /// Rules 10: what a passkey signature on intent `id` is for: an engagement's names the
    /// picture token `token` and the SHA-256 of that token (its plan, the track's identity);
    /// any other intent's names the intent.
    pub fn sign_purpose(&self, id: IntentId, token: Option<u64>) -> Result<crate::passkey::StepUpFor, EngineError> {
        let intent = self.get(id)?;
        if intent.spec().kind() != TaskKind::Engage {
            return Ok(crate::passkey::StepUpFor::Sign { intent: id, token: None, picture_sha256: None });
        }
        let Some(t) = token else {
            return Err(EngineError::StalePicture {
                id,
                why: "an engagement signature quotes the picture you were shown: ask for it first".into(),
            });
        };
        let shown = self.tokens.get(&t).ok_or_else(|| EngineError::StalePicture {
            id,
            why: format!("no picture token {t} was issued (or it was used): look again"),
        })?;
        Ok(crate::passkey::StepUpFor::Sign {
            intent: id,
            token: Some(t),
            picture_sha256: Some(crate::passkey::picture_sha256(shown)),
        })
    }

    /// Rules 10: what a passkey confirmation of the rule declaration in force on `track` is
    /// for (the declaration's number).
    pub fn confirm_purpose(&self, track: &str) -> Result<crate::passkey::StepUpFor, EngineError> {
        let d = self
            .ident
            .declaration(track)
            .ok_or_else(|| EngineError::Identification(format!("no declaration in force on track {track}")))?;
        Ok(crate::passkey::StepUpFor::ConfirmDeclaration {
            track: track.to_string(),
            declaration: d.id,
        })
    }

    /// Why a passkey use by `operator` (in `session`; none for a login) can't be logged, if it
    /// can't: see [`crate::passkey`]. Ok with the counter it reported.
    fn check_passkey_use(
        &self,
        operator: &OperatorId,
        session: Option<SessionId>,
        u: &crate::passkey::PasskeyUse,
    ) -> Result<u32, String> {
        if !self.policy.rules.passkeys() || self.auth != Some(AuthMode::Credentials) {
            return Err("only a credentials run of rules 10 takes a passkey".into());
        }
        let Some(rp) = &self.roster.passkeys else {
            return Err("this run logged no relying party".into());
        };
        if self.used_nonces.contains(&u.nonce) {
            return Err("this challenge was answered already (a passkey assertion is used once)".into());
        }
        let age = self.now().ms_since(u.issued_t);
        if !(0..=crate::passkey::CHALLENGE_TTL_MS).contains(&age) {
            return Err(format!(
                "the challenge is {age} ms old (at most {} ms): ask for a new one",
                crate::passkey::CHALLENGE_TTL_MS
            ));
        }
        let Some(seen) = self.passkeys.get(&u.credential) else {
            return Err("not a passkey this run knows".into());
        };
        if seen.removed {
            return Err("this passkey was removed".into());
        }
        if &seen.key.operator != operator {
            return Err("another operator's passkey".into());
        }
        let challenge = crate::passkey::challenge(&self.run_hash, session, operator, &u.purpose, &u.nonce, u.issued_t);
        let v = crate::passkey::verify(&seen.key, &rp.rp_id, &rp.origins, &challenge, &u.assertion)?;
        if v.counter != u.counter {
            return Err(format!("the counter given ({}) is not the authenticator's ({})", u.counter, v.counter));
        }
        if !crate::passkey::counter_ok(seen.key.counter, v.counter) {
            return Err(format!(
                "the authenticator's counter went from {} to {}: it may have been cloned, refused",
                seen.key.counter, v.counter
            ));
        }
        Ok(v.counter)
    }

    /// A passkey use was logged: its counter and nonce.
    fn passkey_used(&mut self, credential: &str, counter: u32, nonce: String) {
        if let Some(k) = self.passkeys.get_mut(credential) {
            k.key.counter = counter;
        }
        self.used_nonces.insert(nonce);
    }

    /// Rules 10, a credentials run: the operator of `session` stepped up with a passkey: the
    /// bridge checked the assertion (made on a challenge [`Engine::passkey_challenge`] issued
    /// for this session and act), the engine verifies it again against the run's logged
    /// public key and logs it (`operator.action step_up`, `auth: passkey`, `passkey`: the
    /// assertion and what it is for). The [`StepUp`] returned stands only for that act (the act
    /// compares its own purpose), once, within [`STEP_UP_MAX_AGE_MS`]. Refused, nothing
    /// logged, for a scripted session (a harness's credential holds no passkey), another
    /// operator's credential, a counter that does not count up, a challenge answered before or
    /// older than [`crate::CHALLENGE_TTL_MS`], or a login's assertion.
    pub fn step_up_passkey(
        &mut self,
        session: &OperatorSession,
        proof: crate::passkey::PasskeyUse,
    ) -> Result<StepUp, EngineError> {
        let operator = session.operator().clone();
        let id = session.authority().session();
        let refuse = |why: String| EngineError::NotAuthenticated { operator: operator.clone(), why };
        if self.closed.contains(&id) {
            return Err(refuse(format!("session {} has ended", id.0)));
        }
        if !session.method().is_some_and(AuthMethod::is_person) {
            return Err(refuse("a passkey is a person's: this session is not (scripted or asserted)".into()));
        }
        if proof.purpose == crate::passkey::StepUpFor::Login {
            return Err(refuse("a login's assertion is not a step-up".into()));
        }
        let counter = self.check_passkey_use(&operator, Some(id), &proof).map_err(refuse)?;
        let purpose = proof.purpose.clone();
        let nonce = proof.nonce.clone();
        let credential = proof.credential.clone();
        let t = self.emit(EventBody::OperatorAction {
            operator: operator.clone(),
            session: id,
            action: "step_up".into(),
            detail: format!("passkey for {}", purpose.describe()),
            grants: None,
            auth: Some(AuthMethod::Passkey),
            step_up: None,
            passkey: Some(Box::new(proof)),
        })?;
        self.passkey_used(&credential, counter, nonce);
        Ok(StepUp::mint(operator, id, self.seq, t, AuthMethod::Passkey, session.method(), Some(purpose)))
    }

    /// Rules 10, a credentials run: a passkey was registered for `key.operator` while the run
    /// is up (`passkey.registered`, public parts only): through `by`, the operator's own
    /// session, quoting the step-up they just made (their credential re-entered: a session
    /// left open does not add a credential to someone's account), or (`by` None) through a
    /// one-time invitation the bridge checked. Usable from here on. Refused, nothing logged,
    /// for a credential id the run knows already, a key that is not a P-256 point, an operator
    /// not on the roster or not the session's, or a stale or missing step-up.
    pub fn record_passkey_registered(
        &mut self,
        key: crate::passkey::PasskeyKey,
        by: Option<(&OperatorSession, StepUp)>,
    ) -> Result<(), EngineError> {
        let operator = key.operator.clone();
        let refuse = |why: String| EngineError::NotAuthenticated { operator: operator.clone(), why };
        if !self.policy.rules.passkeys() || self.auth != Some(AuthMode::Credentials) || self.roster.passkeys.is_none() {
            return Err(refuse("only a credentials run of rules 10 takes a passkey".into()));
        }
        if self.roster.refuses(&operator, Grants::default()).is_some() {
            return Err(refuse("not on the run's roster".into()));
        }
        if self.passkeys.contains_key(&key.id) {
            return Err(refuse("this credential is registered already".into()));
        }
        crate::passkey::check_key(&key.public_key).map_err(refuse)?;
        let (session, step_up) = match by {
            Some((s, step)) => {
                if s.operator() != &operator {
                    return Err(refuse("a passkey is registered for the session's own operator".into()));
                }
                let sid = s.authority().session();
                if self.closed.contains(&sid) {
                    return Err(refuse(format!("session {} has ended", sid.0)));
                }
                if !s.method().is_some_and(AuthMethod::is_person) {
                    return Err(refuse("a passkey is a person's: this session is not (scripted or asserted)".into()));
                }
                if step.factor() != AuthMethod::Password {
                    return Err(refuse("a registration re-enters the password (a passkey assertion is made for another act)".into()));
                }
                let r = self.fresh_step(&operator, sid, &step).map_err(refuse)?;
                (Some(sid), Some(r))
            }
            None => (None, None),
        };
        self.emit(EventBody::PasskeyRegistered {
            key: key.clone(),
            via: if session.is_some() { "session".into() } else { "invitation".into() },
            session,
            step_up,
        })?;
        self.passkeys.insert(key.id.clone(), KeySeen { key, removed: false });
        Ok(())
    }

    /// Rules 10: a passkey was removed from the credential file while the run is up
    /// (`passkey.removed`); nothing may use it from here on. Nothing happens for one the run
    /// does not know or already removed.
    pub fn record_passkey_removed(&mut self, operator: &OperatorId, credential: &str, reason: &str) -> Result<(), EngineError> {
        match self.passkeys.get(credential) {
            Some(k) if !k.removed && &k.key.operator == operator => {}
            _ => return Ok(()),
        }
        self.emit(EventBody::PasskeyRemoved {
            operator: operator.clone(),
            credential: credential.to_string(),
            reason: reason.to_string(),
        })?;
        if let Some(k) = self.passkeys.get_mut(credential) {
            k.removed = true;
        }
        Ok(())
    }

    /// How this run authenticates its operators (rules 8; None before).
    pub fn auth_mode(&self) -> Option<AuthMode> {
        self.auth
    }

    /// Rules 8: end an operator session (logout, expiry, a credential removed or changed:
    /// `reason`), logged as `operator.action session.closed`. Consumes the session and the
    /// authorities it holds; picture tokens issued to it are dropped. Nothing may be logged
    /// under it afterwards (replay checks). Before rules 8 sessions are not closed: the
    /// session is dropped and nothing is logged.
    pub fn close_operator_session(
        &mut self,
        session: OperatorSession,
        reason: &str,
    ) -> Result<(), EngineError> {
        let id = session.authority().session();
        if self.auth.is_none() || self.closed.contains(&id) {
            return Ok(());
        }
        self.emit(EventBody::OperatorAction {
            operator: session.operator().clone(),
            session: id,
            action: "session.closed".into(),
            detail: reason.to_string(),
            grants: None,
            auth: None,
            step_up: None,
            passkey: None,
        })?;
        self.closed.insert(id);
        self.tokens.retain(|_, t| t.session != id);
        Ok(())
    }

    /// Rules 8, a credentials run: the operator of `session` re-entered their credential just
    /// now, and the bridge checked it (its step-up endpoint is the only caller, right before
    /// the signature it is for). Logged as `operator.action step_up` (`auth: password`) under
    /// the session; the [`StepUp`] returned is what a consequential signature quotes
    /// ([`Engine::approve_stepped`], [`Engine::approve_engagement_stepped`]), once, within
    /// [`STEP_UP_MAX_AGE_MS`]. Refused (nothing logged) in a run that does not authenticate
    /// with credentials: a step-up there would prove nothing.
    pub fn step_up(&mut self, session: &OperatorSession) -> Result<StepUp, EngineError> {
        let operator = session.operator().clone();
        let id = session.authority().session();
        if self.auth != Some(AuthMode::Credentials) {
            return Err(EngineError::NotAuthenticated {
                operator,
                why: "a step-up re-enters a credential: only a credentials run has one".into(),
            });
        }
        if self.closed.contains(&id) {
            return Err(EngineError::NotAuthenticated {
                operator,
                why: format!("session {} has ended", id.0),
            });
        }
        // the password re-entered (a person's, whether their session opened on a password or,
        // rules 10, a passkey), or a machine-held credential (the harness's scripted operators)
        let factor = match session.method() {
            Some(AuthMethod::Scripted) => AuthMethod::Scripted,
            _ => AuthMethod::Password,
        };
        let t = self.emit(EventBody::OperatorAction {
            operator: operator.clone(),
            session: id,
            action: "step_up".into(),
            detail: "credential re-entered to sign".into(),
            grants: None,
            auth: Some(factor),
            step_up: None,
            passkey: None,
        })?;
        Ok(StepUp::mint(operator, id, self.seq, t, factor, session.method(), None))
    }

    /// Why `step` can't stand for an act by `by` in `session` now (another session's or
    /// operator's, older than [`STEP_UP_MAX_AGE_MS`]; rules 10: a passkey's made for another
    /// act than `expected`, or a password where the policy asks a person for a passkey), else
    /// its reference.
    fn valid_step(
        &self,
        by: &OperatorId,
        session: SessionId,
        step: &StepUp,
        expected: Option<&crate::passkey::StepUpFor>,
    ) -> Result<StepUpRef, String> {
        let r = self.fresh_step(by, session, step)?;
        if let Some(made) = step.purpose()
            && Some(made) != expected
        {
            return Err(format!(
                "the passkey assertion was made for {}, not for {}",
                made.describe(),
                expected.map_or_else(|| "this act".to_string(), crate::passkey::StepUpFor::describe)
            ));
        }
        if let Some(why) = self.factor_refusal(step) {
            return Err(why);
        }
        Ok(r)
    }

    /// Why `step` can't stand for an act by `by` in `session` now (another session's or
    /// operator's, or older than [`STEP_UP_MAX_AGE_MS`]), else its reference.
    fn fresh_step(&self, by: &OperatorId, session: SessionId, step: &StepUp) -> Result<StepUpRef, String> {
        let now = self.now();
        if step.session() != session || step.operator() != by {
            return Err(format!("the step-up was made in session {} by {}, not this signer's", step.session().0, step.operator()));
        }
        if now.ms_since(step.t()) > STEP_UP_MAX_AGE_MS {
            return Err(format!("the step-up is {} ms old (at most {STEP_UP_MAX_AGE_MS})", now.ms_since(step.t())));
        }
        Ok(StepUpRef { seq: step.seq() })
    }

    /// Rules 10: why a person's step-up does not meet the policy's `step_up_factor`, if it
    /// doesn't (a scripted credential is a harness's: it steps up as it is held).
    fn factor_refusal(&self, step: &StepUp) -> Option<String> {
        let person = step.session_method().is_some_and(AuthMethod::is_person);
        (self.auth == Some(AuthMode::Credentials)
            && person
            && self.policy.step_up_factor() == crate::policy::StepUpFactor::Passkey
            && step.factor() != AuthMethod::Passkey)
            .then(|| {
                "this run's policy asks for your passkey at this step-up, not your password ([approval] step_up_factor = \"passkey\")".to_string()
            })
    }

    /// Rules 8: the step-up an act other than an approval quotes (`[approval]
    /// step_up_acts`): required for `act` in a credentials run when the policy lists it,
    /// checked when given (rules 10: a passkey's must have been made for `expected`, this
    /// act). Refused, nothing logged.
    fn act_step_up(
        &self,
        act: policy::StepUpAct,
        by: &OperatorId,
        session: SessionId,
        step: Option<StepUp>,
        expected: &crate::passkey::StepUpFor,
    ) -> Result<Option<StepUpRef>, EngineError> {
        let needed = self.auth == Some(AuthMode::Credentials) && self.policy.steps_up(act);
        match step {
            None if needed => Err(EngineError::NotAuthenticated {
                operator: by.clone(),
                why: "the policy marks this act consequential: re-enter your credential (a step-up)".into(),
            }),
            None => Ok(None),
            Some(s) => self
                .valid_step(by, session, &s, Some(expected))
                .map(Some)
                .map_err(|why| EngineError::NotAuthenticated { operator: by.clone(), why }),
        }
    }

    /// Rules 8, a credentials run: a login for `operator` (on the roster) was refused; logged
    /// as `auth.refused` with why and, when it locked the id, for how long. Never the
    /// password. Refused for an operator not on the roster (nothing logged: made-up ids would
    /// flood the log) and outside a credentials run.
    pub fn record_login_refused(
        &mut self,
        operator: &OperatorId,
        reason: &str,
        locked_s: Option<u64>,
    ) -> Result<(), EngineError> {
        if self.auth != Some(AuthMode::Credentials) {
            return Err(EngineError::NotAuthenticated {
                operator: operator.clone(),
                why: "only a credentials run has logins".into(),
            });
        }
        if self.roster.refuses(operator, Grants::default()).is_some() {
            return Err(EngineError::NotOnRoster {
                operator: operator.clone(),
                why: "not on the run's roster".into(),
            });
        }
        self.emit(EventBody::AuthRefused {
            operator: operator.clone(),
            reason: reason.to_string(),
            locked_s,
        })?;
        Ok(())
    }

    /// An operator declares a track's affiliation (logged as `operator.action track.declare`,
    /// detail `track <id> declared <affiliation>[: <note>]`). Rules 8: declaring one hostile
    /// quotes a step-up when `[approval] step_up_acts` lists `declare_hostile` (a credentials
    /// run); the event records it. Rules 9: refused (a declaration goes through the
    /// identification ledger, [`Engine::declare_track`]).
    pub fn record_declaration(
        &mut self,
        by: &OperatorSession,
        step: Option<StepUp>,
        track: &str,
        affiliation: &str,
        note: Option<&str>,
    ) -> Result<(), EngineError> {
        if self.policy.rules.rule_id() {
            return Err(EngineError::Identification(
                "under these rules a declaration goes through the identification ledger (declare_track)".into(),
            ));
        }
        let a = by.authority();
        let hostile = crate::passkey::StepUpFor::DeclareHostile { track: track.to_string() };
        let step_up = if affiliation == "hostile" {
            self.act_step_up(policy::StepUpAct::DeclareHostile, a.operator(), a.session(), step, &hostile)?
        } else {
            match step {
                Some(s) => Some(
                    self.valid_step(a.operator(), a.session(), &s, None)
                        .map_err(|why| EngineError::NotAuthenticated { operator: a.operator().clone(), why })?,
                ),
                None => None,
            }
        };
        self.emit(EventBody::OperatorAction {
            operator: a.operator().clone(),
            session: a.session(),
            action: "track.declare".into(),
            detail: format!("track {track} declared {affiliation}{}", note.map(|n| format!(": {n}")).unwrap_or_default()),
            grants: None,
            auth: None,
            step_up,
            passkey: None,
        })?;
        Ok(())
    }

    /// Whether a signature on `id` by `by` in `session` may go ahead as far as step-up goes,
    /// and the reference it records. Refused (logged as `approval.refused`) when the decision
    /// asks for a step-up in a credentials run and none was given; a step-up for another
    /// session, or too old, is refused too.
    fn check_step_up(
        &mut self,
        id: IntentId,
        by: &OperatorId,
        session: SessionId,
        step: Option<StepUp>,
        expected: &crate::passkey::StepUpFor,
    ) -> Result<Option<StepUpRef>, EngineError> {
        let needed = self.auth == Some(AuthMode::Credentials)
            && self.get(id)?.policy().is_some_and(|p| p.step_up);
        let Some(s) = step else {
            if needed {
                self.refuse(by, session, id, "step-up required: the signer must re-enter their credential to sign this")?;
                return Err(EngineError::StepUpRequired { id, by: by.clone() });
            }
            return Ok(None);
        };
        match self.valid_step(by, session, &s, Some(expected)) {
            Ok(r) => Ok(Some(r)),
            Err(why) => {
                self.refuse(by, session, id, &why)?;
                Err(EngineError::NotAuthenticated { operator: by.clone(), why })
            }
        }
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
        let id = self.submit_inner(source, Some(by), proposal, None, world, terrain)?;
        self.get(id)
    }

    /// Rules 6: [`Engine::submit`] for an engagement nomination whose submitter, an operator
    /// with engagement authority, also accepts, as their explicit decision, that some of our
    /// vehicles are lost: each `(uid, since)` names a vehicle whose position is unknown since
    /// its report at `since` (none: it never reported one). Logged as `engage.accepted`, with
    /// the vehicle's facts and the world's time, before the checks; the friendly keep-out then
    /// leaves each out while it is lost since exactly that report. Refused, with nothing
    /// logged, for another kind of intent, a vehicle that is not lost (its position fresh by
    /// the link-aware rule) or not since that report. Only engagement authority accepts: an
    /// operator without it, and an agent's port, have no such path. The submitter is the
    /// proposer, so the two signers are others, and they are shown the acceptances.
    pub fn submit_accepting(
        &mut self,
        by: &EngagementAuthority,
        proposal: Proposal,
        accept: &[(Uid, Option<Time>)],
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        self.submit_accepting_inner(by, None, proposal, accept, world, terrain)
    }

    /// Rules 8: [`Engine::submit_accepting`] quoting a step-up, which every acceptance of
    /// the nomination records; required in a credentials run when `[approval]
    /// step_up_acts` lists `accept_unknown`.
    pub fn submit_accepting_stepped(
        &mut self,
        by: &EngagementAuthority,
        step: StepUp,
        proposal: Proposal,
        accept: &[(Uid, Option<Time>)],
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        self.submit_accepting_inner(by, Some(step), proposal, accept, world, terrain)
    }

    fn submit_accepting_inner(
        &mut self,
        by: &EngagementAuthority,
        step: Option<StepUp>,
        proposal: Proposal,
        accept: &[(Uid, Option<Time>)],
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        self.submit_accepting_lost_inner(by, step, proposal, accept, &[], &[], world, terrain)
    }

    /// Rules 16: [`Engine::submit_accepting`] that also accepts lost contacts, each `(track,
    /// since)` naming a contact the tracker dropped and the last fix it is out of view since,
    /// as the keep-out check names it (see [`Engine::accept_lost_contact`]). Logged as
    /// `engage.accepted` naming the contact, before the checks. Refused, nothing logged, for a
    /// contact that is not lost since exactly that fix.
    #[allow(clippy::too_many_arguments)]
    pub fn submit_accepting_lost(
        &mut self,
        by: &EngagementAuthority,
        step: Option<StepUp>,
        proposal: Proposal,
        vehicles: &[(Uid, Option<Time>)],
        contacts: &[(String, Time)],
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        self.submit_accepting_lost_inner(by, step, proposal, vehicles, contacts, &[], world, terrain)
    }

    /// Rules 19: [`Engine::submit_accepting_lost`] that also accepts live tracks as they are
    /// behaving now (`tracks`, by id: each with its facts on `world`, see
    /// [`Engine::accept_live_track`]). Logged as `engage.accepted` naming each track with its
    /// facts, before the checks. Refused, nothing logged, for a track that may not be accepted
    /// ([`engage::behaving_acceptable`]: not in the picture, out of view, no velocity, the target
    /// or a declared hostile), or before rules 19.
    #[allow(clippy::too_many_arguments)]
    pub fn submit_accepting_all(
        &mut self,
        by: &EngagementAuthority,
        step: Option<StepUp>,
        proposal: Proposal,
        vehicles: &[(Uid, Option<Time>)],
        contacts: &[(String, Time)],
        tracks: &[String],
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        self.submit_accepting_lost_inner(by, step, proposal, vehicles, contacts, tracks, world, terrain)
    }

    #[allow(clippy::too_many_arguments)]
    fn submit_accepting_lost_inner(
        &mut self,
        by: &EngagementAuthority,
        step: Option<StepUp>,
        proposal: Proposal,
        accept: &[(Uid, Option<Time>)],
        contacts: &[(String, Time)],
        tracks: &[String],
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        // rules 19: the live tracks accepted as behaving now, each with its facts as decisions
        // read them (identification filled)
        let behaving: Vec<(String, TrackFacts)> = if tracks.is_empty() {
            Vec::new()
        } else {
            let target = match &proposal.task {
                Task::Engage { track, .. } => track.clone(),
                _ => {
                    return Err(EngineError::NotAcceptedTrack {
                        track: tracks[0].clone(),
                        why: "an acceptance of a live track as behaving now is for an engagement's keep-out only".into(),
                    });
                }
            };
            let filled = self.identified(world);
            let mut out = Vec::new();
            for track in tracks {
                self.acceptable_track(track, &target, &filled)?;
                if let Some(t) = filled.tracks.get(track) {
                    out.push((track.clone(), t.clone()));
                }
            }
            out
        };
        if let Some((uid, _)) = accept.first()
            && !matches!(proposal.task, Task::Engage { .. })
        {
            return Err(EngineError::NotAccepted {
                uid: uid.clone(),
                why: "an acceptance of a lost vehicle is for an engagement's keep-out only".into(),
            });
        }
        if let Some((track, _)) = contacts.first()
            && !matches!(proposal.task, Task::Engage { .. })
        {
            return Err(EngineError::NotAcceptedContact {
                track: track.clone(),
                why: "an acceptance of a lost contact is for an engagement's keep-out only".into(),
            });
        }
        for (uid, since) in accept {
            self.acceptable(uid, *since, world)?;
        }
        for (track, since) in contacts {
            self.acceptable_contact(track, *since)?;
        }
        // rules 8: accepting a lost vehicle may be marked consequential (rules 19: and a live
        // track as behaving now, the same act)
        let step_up = if accept.is_empty() && contacts.is_empty() && behaving.is_empty() {
            None
        } else {
            let named: Vec<String> = behaving.iter().map(|(id, _)| id.clone()).collect();
            let expected = if named.is_empty() {
                crate::passkey::StepUpFor::accepting_lost(None, accept, contacts)
            } else {
                crate::passkey::StepUpFor::accepting_all(None, accept, contacts, &named)
            };
            self.act_step_up(policy::StepUpAct::AcceptUnknown, by.operator(), by.session(), step, &expected)?
        };
        let source = Source::Human {
            id: by.operator().clone(),
        };
        let acceptor = Acceptor {
            by: by.operator(),
            session: by.session(),
            list: accept,
            contacts,
            tracks: &behaving,
            step_up,
        };
        let human = by.human();
        let id = self.submit_inner(source, Some(&human), proposal, Some(acceptor), world, terrain)?;
        self.get(id)
    }

    /// Rules 6: an operator with engagement authority accepts, for engagement `id` while it
    /// waits with no signature yet, that vehicle `uid` is lost since its report at `since`
    /// (see [`Engine::submit_accepting`]). Logged as `engage.accepted` with the vehicle's facts
    /// and the world's time; the next check reads it, and every signer's picture token
    /// carries the acceptances (a token taken before a change is refused: look again).
    /// Refused, nothing logged, after a signature (the plan it froze was signed without it:
    /// the plan must stop holding and be signed again), or for a vehicle that is not lost since
    /// that report. A reset of the signatures clears the acceptances: they are given again,
    /// on the new picture.
    pub fn accept_unknown_friendly(
        &mut self,
        id: IntentId,
        by: &EngagementAuthority,
        uid: Uid,
        since: Option<Time>,
        world: &WorldSnapshot,
    ) -> Result<&Intent, EngineError> {
        self.accept_unknown_inner(id, by, None, uid, since, world)
    }

    /// Rules 8: [`Engine::accept_unknown_friendly`] quoting a step-up (recorded on
    /// `engage.accepted`); required in a credentials run when `[approval] step_up_acts` lists
    /// `accept_unknown`.
    pub fn accept_unknown_friendly_stepped(
        &mut self,
        id: IntentId,
        by: &EngagementAuthority,
        step: StepUp,
        uid: Uid,
        since: Option<Time>,
        world: &WorldSnapshot,
    ) -> Result<&Intent, EngineError> {
        self.accept_unknown_inner(id, by, Some(step), uid, since, world)
    }

    fn accept_unknown_inner(
        &mut self,
        id: IntentId,
        by: &EngagementAuthority,
        step: Option<StepUp>,
        uid: Uid,
        since: Option<Time>,
        world: &WorldSnapshot,
    ) -> Result<&Intent, EngineError> {
        let intent = self.get(id)?;
        if intent.spec().kind() != TaskKind::Engage {
            return Err(EngineError::NotAnEngagement(id));
        }
        if intent.state() != State::PendingApproval {
            return Err(EngineError::NotAccepted {
                uid,
                why: format!("{id} is not waiting for signatures: nominate it again to accept a lost vehicle"),
            });
        }
        if !intent.concurrences().is_empty() {
            return Err(EngineError::NotAccepted {
                uid,
                why: format!("{id} already has signatures on a plan made without this acceptance: they must be dropped first"),
            });
        }
        self.acceptable(&uid, since, world)?;
        let expected = crate::passkey::StepUpFor::accepting(Some(id), &[(uid.clone(), since)]);
        let step_up = self.act_step_up(policy::StepUpAct::AcceptUnknown, by.operator(), by.session(), step, &expected)?;
        let vehicle = Box::new(world.vehicles.get(&uid).cloned().unwrap_or_default());
        self.emit(EventBody::EngageAccepted {
            intent: id,
            uid: Some(uid.clone()),
            contact: None,
            track: None,
            since,
            by: by.operator().clone(),
            session: by.session(),
            world_t: world.t,
            vehicle: Some(vehicle),
            lost: None,
            behaving: None,
            step_up,
        })?;
        self.get_mut(id)?.engagement_mut().accepted.insert(
            uid,
            engage::AcceptedUnknown {
                since,
                by: by.operator().clone(),
            },
        );
        self.get(id)
    }

    /// Rules 6: any operator withdraws an acceptance of a lost vehicle for engagement `id`
    /// (pending, approved or under way: it only restricts, like an abort). Logged as
    /// `engage.acceptance_revoked`; its checks, and the picture held after approval, count
    /// the vehicle again (which stops an engagement it now blocks).
    pub fn revoke_unknown_friendly(
        &mut self,
        id: IntentId,
        by: &HumanAuthority,
        uid: Uid,
    ) -> Result<&Intent, EngineError> {
        let intent = self.get(id)?;
        if intent.spec().kind() != TaskKind::Engage {
            return Err(EngineError::NotAnEngagement(id));
        }
        if !matches!(
            intent.state(),
            State::PendingApproval | State::Approved | State::Dispatched
        ) || !intent.engagement().is_some_and(|e| e.accepted.contains_key(&uid))
        {
            return Err(EngineError::NotAccepted {
                uid,
                why: format!("no acceptance of it is in force on {id}"),
            });
        }
        self.emit(EventBody::EngageAcceptanceRevoked {
            intent: id,
            uid: Some(uid.clone()),
            contact: None,
            track: None,
            by: by.operator().clone(),
            session: by.session(),
        })?;
        self.get_mut(id)?.engagement_mut().accepted.remove(&uid);
        self.get(id)
    }

    /// Rules 16: an operator with engagement authority accepts, for engagement `id` while it
    /// waits with no signature yet, that lost contact `track` (a track the tracker dropped,
    /// [`crate::lost`]) is out of view since its last fix at `since`, as the keep-out check
    /// names it: the keep-out on tracks of this engagement then leaves that contact out while it
    /// is lost since exactly then. As [`Engine::accept_unknown_friendly`]: logged as
    /// `engage.accepted` naming the contact, with its facts and the world's time; every check of
    /// the engagement reads it, every signer's picture token carries it (a token from before is
    /// refused), a reset of the signatures clears it, any operator may revoke it. Refused,
    /// nothing logged, after a signature, before rules 16, or for a contact that is not lost since
    /// exactly that fix (seen again, or another loss).
    pub fn accept_lost_contact(
        &mut self,
        id: IntentId,
        by: &EngagementAuthority,
        step: Option<StepUp>,
        track: &str,
        since: Time,
        world: &WorldSnapshot,
    ) -> Result<&Intent, EngineError> {
        let no = |why: String| EngineError::NotAcceptedContact {
            track: track.to_string(),
            why,
        };
        let intent = self.get(id)?;
        if intent.spec().kind() != TaskKind::Engage {
            return Err(EngineError::NotAnEngagement(id));
        }
        if intent.state() != State::PendingApproval {
            return Err(no(format!(
                "{id} is not waiting for signatures: nominate it again to accept a lost contact"
            )));
        }
        if !intent.concurrences().is_empty() {
            return Err(no(format!(
                "{id} already has signatures on a plan made without this acceptance: they must be dropped first"
            )));
        }
        self.acceptable_contact(track, since)?;
        let expected = crate::passkey::StepUpFor::accepting_lost(Some(id), &[], &[(track.to_string(), since)]);
        let step_up = self.act_step_up(policy::StepUpAct::AcceptUnknown, by.operator(), by.session(), step, &expected)?;
        let lost = self.lost_filled().remove(track).map(Box::new);
        self.emit(EventBody::EngageAccepted {
            intent: id,
            uid: None,
            contact: Some(track.to_string()),
            track: None,
            since: Some(since),
            by: by.operator().clone(),
            session: by.session(),
            world_t: world.t,
            vehicle: None,
            lost,
            behaving: None,
            step_up,
        })?;
        self.get_mut(id)?.engagement_mut().accepted_lost.insert(
            track.to_string(),
            engage::AcceptedUnknown {
                since: Some(since),
                by: by.operator().clone(),
            },
        );
        self.get(id)
    }

    /// Rules 16: any operator withdraws an acceptance of a lost contact for engagement `id`
    /// (pending, approved or under way: it only restricts). Logged as
    /// `engage.acceptance_revoked` naming the contact; the keep-out counts it again (which stops
    /// an engagement it now blocks).
    pub fn revoke_lost_contact(
        &mut self,
        id: IntentId,
        by: &HumanAuthority,
        track: &str,
    ) -> Result<&Intent, EngineError> {
        let intent = self.get(id)?;
        if intent.spec().kind() != TaskKind::Engage {
            return Err(EngineError::NotAnEngagement(id));
        }
        if !matches!(
            intent.state(),
            State::PendingApproval | State::Approved | State::Dispatched
        ) || !intent.engagement().is_some_and(|e| e.accepted_lost.contains_key(track))
        {
            return Err(EngineError::NotAcceptedContact {
                track: track.to_string(),
                why: format!("no acceptance of it is in force on {id}"),
            });
        }
        self.emit(EventBody::EngageAcceptanceRevoked {
            intent: id,
            uid: None,
            contact: Some(track.to_string()),
            track: None,
            by: by.operator().clone(),
            session: by.session(),
        })?;
        self.get_mut(id)?.engagement_mut().accepted_lost.remove(track);
        self.get(id)
    }

    /// Rules 16: whether `track` is a lost contact since exactly `since`.
    fn acceptable_contact(&self, track: &str, since: Time) -> Result<(), EngineError> {
        let no = |why: String| EngineError::NotAcceptedContact {
            track: track.to_string(),
            why,
        };
        if !self.policy.rules.lost_contacts() {
            return Err(no("these decision rules have no lost contacts".into()));
        }
        engage::contact_lost_since(track, self.lost.get(track), Some(since)).map_err(no)
    }

    // ---- live tracks accepted as behaving now (rules 19) --------------------------------

    /// Rules 19 (bd murmur-j0x): an operator with engagement authority accepts, for engagement
    /// `id` while it waits with no signature yet, live track `track` as it is behaving now: its
    /// facts on `world` (as decisions read them) are logged with `engage.accepted` naming the
    /// track and the world's time, and from then on the keep-out on tracks of this engagement
    /// counts it along the path accepted for it and its course now
    /// ([`engage::behaving_regions`]) instead of anywhere it could turn or speed up to, while the
    /// acceptance holds: it lapses on its own when the track departs from the accepted prediction
    /// beyond its uncertainty or goes out of view ([`engage::behaving_lapse`];
    /// `engage.acceptance_lapsed`, at the next step or decision), and a new one needs a person
    /// again. As the other acceptances: every signer's picture token carries it (a token from
    /// before is refused), a reset of the signatures clears it, any operator may revoke it, never
    /// an agent's (only an [`EngagementAuthority`] takes it). Refused, nothing logged, after a
    /// signature, before rules 19, or for a track that may not be accepted
    /// ([`engage::behaving_acceptable`]).
    pub fn accept_live_track(
        &mut self,
        id: IntentId,
        by: &EngagementAuthority,
        step: Option<StepUp>,
        track: &str,
        world: &WorldSnapshot,
    ) -> Result<&Intent, EngineError> {
        let no = |why: String| EngineError::NotAcceptedTrack {
            track: track.to_string(),
            why,
        };
        let intent = self.get(id)?;
        if intent.spec().kind() != TaskKind::Engage {
            return Err(EngineError::NotAnEngagement(id));
        }
        if intent.state() != State::PendingApproval {
            return Err(no(format!(
                "{id} is not waiting for signatures: nominate it again to accept a live track as behaving now"
            )));
        }
        if !intent.concurrences().is_empty() {
            return Err(no(format!(
                "{id} already has signatures on a plan made without this acceptance: they must be dropped first"
            )));
        }
        let target = engage::params(intent.spec()).map(|p| p.0.to_string()).unwrap_or_default();
        // an acceptance this picture lapses goes first; one that holds is not given twice
        self.lapse_behaving(id, world)?;
        if self.get(id)?.engagement().is_some_and(|e| e.accepted_behaving.contains_key(track)) {
            return Err(no(format!(
                "already accepted as behaving now for {id}: revoke that acceptance to accept it anew"
            )));
        }
        let filled = self.identified(world);
        self.acceptable_track(track, &target, &filled)?;
        let Some(facts) = filled.tracks.get(track).cloned() else {
            return Err(no(format!("{track} is not in the picture")));
        };
        let expected = crate::passkey::StepUpFor::accepting_all(Some(id), &[], &[], &[track.to_string()]);
        let step_up = self.act_step_up(policy::StepUpAct::AcceptUnknown, by.operator(), by.session(), step, &expected)?;
        self.record_behaving(id, by.operator(), by.session(), step_up, track, &facts, world.t)?;
        self.get(id)
    }

    /// Rules 19: any operator withdraws an acceptance of a live track as behaving now for
    /// engagement `id` (pending, approved or under way: it only restricts). Logged as
    /// `engage.acceptance_revoked` naming the track; the keep-out counts it again by all its
    /// regions (which stops an engagement it now blocks).
    pub fn revoke_live_track(&mut self, id: IntentId, by: &HumanAuthority, track: &str) -> Result<&Intent, EngineError> {
        let intent = self.get(id)?;
        if intent.spec().kind() != TaskKind::Engage {
            return Err(EngineError::NotAnEngagement(id));
        }
        if !matches!(
            intent.state(),
            State::PendingApproval | State::Approved | State::Dispatched
        ) || !intent.engagement().is_some_and(|e| e.accepted_behaving.contains_key(track))
        {
            return Err(EngineError::NotAcceptedTrack {
                track: track.to_string(),
                why: format!("no acceptance of it as behaving now is in force on {id}"),
            });
        }
        self.emit(EventBody::EngageAcceptanceRevoked {
            intent: id,
            uid: None,
            contact: None,
            track: Some(track.to_string()),
            by: by.operator().clone(),
            session: by.session(),
        })?;
        self.get_mut(id)?.engagement_mut().accepted_behaving.remove(track);
        self.get(id)
    }

    /// Rules 19: whether live track `track` may be accepted as behaving now for an engagement on
    /// `target`, on `world` (identification filled).
    fn acceptable_track(&self, track: &str, target: &str, world: &WorldSnapshot) -> Result<(), EngineError> {
        let no = |why: String| EngineError::NotAcceptedTrack {
            track: track.to_string(),
            why,
        };
        if !self.policy.rules.accept_behaving() {
            return Err(no("these decision rules have no acceptance of a live track as behaving now".into()));
        }
        let ec = self.policy.rules.engage_rules();
        engage::behaving_acceptable(track, world.tracks.get(track), world.t, target, &ec).map_err(no)
    }

    /// Rules 19: log an acceptance of live track `track` as behaving now for engagement `id`
    /// (`engage.accepted` with its facts and the world's time) and hold it.
    #[allow(clippy::too_many_arguments)]
    fn record_behaving(
        &mut self,
        id: IntentId,
        by: &OperatorId,
        session: SessionId,
        step_up: Option<StepUpRef>,
        track: &str,
        facts: &TrackFacts,
        world_t: Time,
    ) -> Result<(), EngineError> {
        self.emit(EventBody::EngageAccepted {
            intent: id,
            uid: None,
            contact: None,
            track: Some(track.to_string()),
            since: Some(facts.position.t),
            by: by.clone(),
            session,
            world_t,
            vehicle: None,
            lost: None,
            behaving: Some(Box::new(facts.clone())),
            step_up,
        })?;
        self.get_mut(id)?.engagement_mut().accepted_behaving.insert(
            track.to_string(),
            engage::AcceptedBehaving {
                by: by.clone(),
                t: world_t,
                facts: facts.clone(),
            },
        );
        Ok(())
    }

    /// Rules 19: the live tracks accepted as behaving now for engagement `id`.
    fn accepted_behaving_for(&self, id: IntentId) -> BTreeMap<String, engage::AcceptedBehaving> {
        self.intents
            .get(&id)
            .and_then(Intent::engagement)
            .map(|e| e.accepted_behaving.clone())
            .unwrap_or_default()
    }

    /// Rules 19: engagement `id`'s acceptances as behaving now that the picture (`world`, its
    /// identification filled here) lapses ([`engage::behaving_lapse`]): each is logged
    /// (`engage.acceptance_lapsed`, with why, the world's time and the track's facts then, none
    /// when it is not in the picture), dropped for good and recorded on the engagement for the
    /// card. Run before every step of the engagement and before a look or a signature, so a
    /// decision reads only acceptances its own picture bears out. Nothing before rules 19.
    pub fn lapse_behaving(&mut self, id: IntentId, world: &WorldSnapshot) -> Result<Vec<engage::LapseRecord>, EngineError> {
        if !self.policy.rules.accept_behaving() {
            return Ok(Vec::new());
        }
        let accepted = self.accepted_behaving_for(id);
        if accepted.is_empty() {
            return Ok(Vec::new());
        }
        let filled = self.identified(world);
        let ec = self.policy.rules.engage_rules();
        let mut out = Vec::new();
        for (track, a) in accepted {
            let live = filled.tracks.get(&track);
            let Some(lapse) = engage::behaving_lapse(&track, &a, live, filled.t, &ec) else {
                continue;
            };
            let t = self.emit(EventBody::EngageAcceptanceLapsed {
                intent: id,
                track: track.clone(),
                kind: lapse.kind,
                detail: lapse.detail.clone(),
                world_t: filled.t,
                facts: live.cloned().map(Box::new),
            })?;
            let record = engage::LapseRecord {
                track: track.clone(),
                t,
                kind: lapse.kind,
                detail: lapse.detail,
                accepted_by: a.by.clone(),
            };
            let e = self.get_mut(id)?.engagement_mut();
            e.accepted_behaving.remove(&track);
            e.lapsed.push(record.clone());
            out.push(record);
        }
        Ok(out)
    }

    // ---- lost contacts (rules 16) -----------------------------------------------------

    /// Rules 16: the bridge's tracker dropped these tracks (each with its facts at the drop, as
    /// it hands them to decisions: the estimate, never a declaration, and the AIS identity it
    /// carried). Each is logged as `track.dropped` and held from here on as a lost contact
    /// ([`crate::lost`]), which every engagement's keep-out on tracks counts anywhere it could
    /// have reached since its last fix, until an operator confirms a track as it again
    /// ([`Engine::confirm_lost_claim`]). A drop of a track a confirmed lost contact was
    /// confirmed as voids that confirmation (`lost.reinstated`). The bridge keeps a dropped track
    /// in its picture until this returns, so no decision falls between the two. Before rules 16
    /// nothing is logged (a dropped track is simply gone, as then). A track id the ledger holds
    /// already is refused (the tracker drops an id once), and so is everything after it: on an
    /// error, the drops from the first not recorded on were not. Returns how many were recorded.
    pub fn record_track_drops(
        &mut self,
        drops: Vec<(String, TrackFacts, Option<AisIdentity>)>,
    ) -> Result<usize, EngineError> {
        if !self.policy.rules.lost_contacts() {
            return Ok(0);
        }
        let mut n = 0;
        for (track, mut facts, ais) in drops {
            if self.lost.holds(&track) {
                return Err(EngineError::NotAcceptedContact {
                    track,
                    why: "dropped already: the tracker drops a track id once".into(),
                });
            }
            // the estimate only: the ledger fills the identification for every decision
            facts.declared_by = None;
            facts.declaration = None;
            facts.review_flags = Vec::new();
            let t = self.emit(EventBody::TrackDropped {
                track: track.clone(),
                facts: Box::new(facts.clone()),
                ais: ais.clone(),
            })?;
            self.lost
                .record(&track, facts, ais, t)
                .map_err(|why| EngineError::NotAcceptedContact { track: track.clone(), why })?;
            n += 1;
            // a lost contact confirmed as this track counts again
            let voided: Vec<String> = self
                .lost
                .confirmed()
                .iter()
                .filter(|(_, c)| c.by_track == track)
                .map(|(id, _)| id.clone())
                .collect();
            for id in voided {
                self.reinstate_lost(&id, format!("{track} is no longer in the picture (dropped)"), t, None)?;
            }
        }
        Ok(n)
    }

    /// Rules 16, every second (the bridge's loop; the scenario runner's): on `world`, its
    /// identification filled, void every confirmation the picture no longer bears out (the
    /// claimant gone, suspect or declared: `lost.reinstated`, the lost contact counting again
    /// for good), then raise every new claim, a live track carrying a lost contact's AIS
    /// identity (`lost.claimed`, once per pair, with the picture and the evidence; it changes
    /// nothing until an operator confirms it). Never retires anything.
    pub fn step_lost(&mut self, world: &WorldSnapshot) -> Result<LostStep, EngineError> {
        let mut out = LostStep::default();
        if !self.policy.rules.lost_contacts() || self.lost.is_empty() {
            return Ok(out);
        }
        let mut tracks = world.tracks.clone();
        if self.policy.rules.rule_id() {
            self.ident.fill(&mut tracks);
        }
        for (id, by_track, why) in self.lost.void_in(&tracks) {
            let facts = tracks.get(&by_track).cloned().map(Box::new);
            self.reinstate_lost(&id, why.clone(), world.t, facts)?;
            out.reinstated.push((id, by_track, why));
        }
        let ec = self.policy.rules.engage_rules();
        for c in crate::lost::claims(&self.lost, &tracks, &world.track_ais, &ec) {
            if self.lost.claimed(&c.lost, &c.track) {
                continue;
            }
            self.emit(EventBody::LostClaimed {
                track: c.lost.clone(),
                by_track: c.track.clone(),
                world_t: world.t,
                tracks: tracks.clone(),
                track_ais: world.track_ais.clone(),
                evidence: c.evidence.clone(),
                blockers: c.blockers.clone(),
            })?;
            self.lost.claim(&c.lost, &c.track);
            out.claimed.push(c);
        }
        Ok(out)
    }

    fn reinstate_lost(&mut self, id: &str, reason: String, world_t: Time, facts: Option<Box<TrackFacts>>) -> Result<(), EngineError> {
        let Some(by_track) = self.lost.confirmed().get(id).map(|c| c.by_track.clone()) else {
            return Ok(());
        };
        self.emit(EventBody::LostReinstated {
            track: id.to_string(),
            by_track,
            reason,
            world_t,
            facts,
        })?;
        self.lost.reinstate(id);
        Ok(())
    }

    /// Rules 16: an operator (any, under their own session; an agent's port has no such
    /// method) confirms the claim of live track `by_track` of `world` to be lost contact `track`
    /// ([`crate::lost::claim_of`] on the picture with its identification filled): taken only
    /// while nothing contradicts it (its blockers empty: the only track carrying the MMSI,
    /// undeclared, not suspect, the same domain, first seen after the lost contact's last fix,
    /// where it could have reached). Logged as `track.reacquired` with the picture and the
    /// evidence; the lost contact is retired from the keep-outs while `by_track` stands. Refused,
    /// nothing logged, otherwise.
    pub fn confirm_lost_claim(
        &mut self,
        by: &HumanAuthority,
        track: &str,
        by_track: &str,
        world: &WorldSnapshot,
    ) -> Result<crate::lost::Claim, EngineError> {
        let no = |why: String| EngineError::NotAcceptedContact {
            track: track.to_string(),
            why,
        };
        if !self.policy.rules.lost_contacts() {
            return Err(no("these decision rules have no lost contacts".into()));
        }
        let Some(lost) = self.lost.get(track).cloned() else {
            return Err(no(match self.lost.confirmed().get(track) {
                Some(c) => format!("already confirmed as {} by {}", c.by_track, c.by),
                None => format!("{track} is not a lost contact"),
            }));
        };
        if lost.ais.is_none() {
            return Err(no(format!("{track} carried no AIS identity: no track can claim to be it")));
        }
        let mut tracks = world.tracks.clone();
        if self.policy.rules.rule_id() {
            self.ident.fill(&mut tracks);
        }
        let ec = self.policy.rules.engage_rules();
        let Some(claim) = crate::lost::claim_of(track, &lost, &self.lost, by_track, &tracks, &world.track_ais, &ec) else {
            return Err(no(format!("{by_track} is not in the picture carrying {track}'s MMSI")));
        };
        if !claim.confirmable() {
            return Err(no(format!("{by_track} may not be confirmed as {track}: {}", claim.blockers.join("; "))));
        }
        let t = self.emit(EventBody::TrackReacquired {
            track: track.to_string(),
            by_track: by_track.to_string(),
            by: by.operator().clone(),
            session: by.session(),
            world_t: world.t,
            tracks,
            track_ais: world.track_ais.clone(),
            evidence: claim.evidence.clone(),
        })?;
        self.lost.claim(track, by_track);
        self.lost.confirm(track, by_track, by.operator(), t).map_err(no)?;
        Ok(claim)
    }

    /// Rules 16: the claims on the lost contacts in force on `world` now (for review): each live
    /// track carrying a lost contact's MMSI, with the evidence and what keeps it from being
    /// confirmed ([`crate::lost::claims`]).
    pub fn lost_claims(&self, world: &WorldSnapshot) -> Vec<crate::lost::Claim> {
        if !self.policy.rules.lost_contacts() {
            return Vec::new();
        }
        let mut tracks = world.tracks.clone();
        if self.policy.rules.rule_id() {
            self.ident.fill(&mut tracks);
        }
        crate::lost::claims(&self.lost, &tracks, &world.track_ais, &self.policy.rules.engage_rules())
    }

    /// Rules 16: the lost contacts in force, as decisions read them (identification filled).
    pub fn lost_contacts(&self) -> BTreeMap<String, crate::lost::LostContact> {
        self.lost_filled()
    }

    /// Rules 16: the lost contacts an operator confirmed as a live track (retired while it
    /// stands).
    pub fn lost_confirmed(&self) -> &BTreeMap<String, crate::lost::Confirmed> {
        self.lost.confirmed()
    }

    fn lost_filled(&self) -> BTreeMap<String, crate::lost::LostContact> {
        self.lost.filled(&self.ident, self.policy.rules.rule_id())
    }

    /// Rules 16: an engagement decision's lost contacts and the acceptances of them for it,
    /// filled into its inputs (nothing for another kind, or before rules 16): those in force
    /// and those whose confirmation its picture (`w.tracks`, identification filled) voids.
    fn fill_lost(&self, spec: &IntentSpec, w: &mut WorldSnapshot) {
        if spec.kind() != TaskKind::Engage || !self.policy.rules.lost_contacts() {
            return;
        }
        w.lost_contacts = self.lost.in_force_for(&w.tracks, &self.ident, self.policy.rules.rule_id());
        w.lost_accepted = self.accepted_lost_for(spec.id);
        // rules 19: and its live tracks accepted as behaving now (every engagement decision reads
        // them where it reads its lost contacts)
        if self.policy.rules.accept_behaving() {
            w.behaving_accepted = self.accepted_behaving_for(spec.id);
        }
    }

    /// Rules 16: the lost contacts an operator accepted for engagement `id`.
    fn accepted_lost_for(&self, id: IntentId) -> BTreeMap<String, engage::AcceptedUnknown> {
        self.intents
            .get(&id)
            .and_then(Intent::engagement)
            .map(|e| e.accepted_lost.clone())
            .unwrap_or_default()
    }

    /// Whether `uid` is lost since exactly `since` on `world` (rules 6 acceptances).
    fn acceptable(&self, uid: &Uid, since: Option<Time>, world: &WorldSnapshot) -> Result<(), EngineError> {
        let no = |why: String| EngineError::NotAccepted {
            uid: uid.clone(),
            why,
        };
        if self.policy.rules.rules_version < 6 {
            return Err(no("these decision rules have no acceptance of a lost vehicle".into()));
        }
        engage::lost_since_report(world.vehicles.get(uid), since, world.t, &self.policy.rules).map_err(no)
    }

    /// Rules 6: the platform's report of its lost-link preload for dispatched engagement
    /// `id` (`set`: its own values read first, then the order's preload set and read back;
    /// `restore`: its own values put back once the engagement ended). Logged as
    /// `engage.preload` with whether the read-back confirms it (replay recomputes that). A
    /// `set` that does not confirm while the engagement is under way stops it (cause
    /// system: the platform's lost-link action could not be preloaded) and returns its
    /// fallback for the bridge to command; nothing is released before a confirmed `set`
    /// ([`engage::picture`]). Refused ([`EngineError::NotItsOrder`], nothing logged) for an
    /// intent that was never dispatched, another vehicle than its platform, an order without
    /// a preload, a second `set`, or a `restore` before the engagement ended or without a
    /// `set`.
    pub fn record_preload(
        &mut self,
        id: IntentId,
        report: PreloadReport,
    ) -> Result<Option<FallbackOrder>, EngineError> {
        let intent = self.get(id)?;
        if intent.spec().kind() != TaskKind::Engage {
            return Err(EngineError::NotAnEngagement(id));
        }
        let dispatched = intent.history().iter().any(|h| h.state == State::Dispatched);
        let eng = intent.engagement().cloned().unwrap_or_default();
        let other_holds = self.engaged_elsewhere(id, &report.uid);
        if let Some(why) = preload_mismatch(
            intent.spec(),
            intent.state(),
            dispatched,
            eng.preload.as_ref(),
            eng.intended.as_ref().map(|r| &r.before),
            eng.preloaded.as_ref().map(|r| r.confirmed),
            eng.restored.as_ref().is_some_and(|r| r.confirmed),
            other_holds,
            report.phase,
            &report.uid,
            &report.before,
        ) {
            return Err(EngineError::NotItsOrder { id, why: why.into() });
        }
        let verdict = preload_verdict(
            report.phase,
            eng.preload.as_ref(),
            eng.preloaded.as_ref().map(|r| &r.before),
            &report.before,
            &report.readback,
            report.error.as_deref(),
        );
        let confirmed = verdict.is_ok();
        let body = EventBody::EngagePreload {
            intent: id,
            uid: report.uid.clone(),
            phase: report.phase,
            before: report.before.clone(),
            readback: report.readback.clone(),
            error: report.error.clone(),
            confirmed,
            vehicle: report.vehicle.clone().map(Box::new),
        };
        let t = self.emit(body.clone())?;
        // the platform's own values owed back, as replay keeps them
        self.restores.step_body(&body);
        let record = engage::PreloadRecord {
            t,
            phase: report.phase,
            before: report.before,
            readback: report.readback,
            confirmed,
            detail: verdict.clone().err().unwrap_or_default(),
        };
        let state = self.get(id)?.state();
        let e = self.get_mut(id)?.engagement_mut();
        match report.phase {
            engage::PreloadPhase::Intent => e.intended = Some(record),
            engage::PreloadPhase::Set => e.preloaded = Some(record),
            engage::PreloadPhase::Verify => e.verified = Some(record),
            engage::PreloadPhase::Restore => e.restored = Some(record),
        }
        match (report.phase, verdict, state) {
            (engage::PreloadPhase::Set, Err(why), State::Dispatched) => self.stop_engagement(
                id,
                StopCause::System,
                format!("system: {PRELOAD_FAILED}{why}"),
                true,
            ),
            (engage::PreloadPhase::Verify, Err(why), State::Dispatched) => self.stop_engagement(
                id,
                StopCause::System,
                format!("system: {PRELOAD_CHANGED}{why}"),
                true,
            ),
            _ => Ok(None),
        }
    }

    /// Rules 6: a platform's own lost-link values owed back outside an engagement's own
    /// restore, logged as `platform.restore`: carried over at start from an earlier log whose
    /// preload was never confirmed back (`carried`), each attempt to put them back, and an
    /// engagement's restore that [`Engine::record_preload`] refused. Returns whether the
    /// attempt confirmed (the read-back shows every value back, no error; replay recomputes
    /// it, and reports a platform whose values were still owed when the log ended).
    pub fn record_platform_restore(&mut self, report: PlatformRestoreReport) -> Result<bool, EngineError> {
        // rules 12: what an earlier run owed is carried in run.started, in one line
        // (Engine::start), never event by event
        if report.carried && self.policy.rules.followups() {
            return Err(EngineError::NotItsOrder {
                id: IntentId(0),
                why: "values carried over from an earlier run go into run.started under these rules (Engine::start)".into(),
            });
        }
        // never while an engagement holds the platform: that would undo its preload
        if !report.carried
            && let Some(other) = self.engagement_holding(&report.uid, None)
        {
            return Err(EngineError::NotItsOrder {
                id: other,
                why: "a restore of the platform's own values while an engagement holds it".into(),
            });
        }
        let confirmed = !report.carried
            && match &report.error {
                Some(_) => false,
                None => engage::restored(&report.values, &report.readback).is_ok(),
            };
        let body = EventBody::PlatformRestore {
            uid: report.uid,
            values: report.values,
            carried: report.carried,
            from: report.from,
            readback: report.readback,
            error: report.error,
            confirmed,
            vehicle: report.vehicle.map(Box::new),
        };
        self.emit(body.clone())?;
        self.restores.step_body(&body);
        Ok(confirmed)
    }

    /// Rules 12 (murmur-q05, murmur-cq3): an operator writes off a debt of the ledger, under
    /// their own session, saying why: the platform's own lost-link values owed back to `uid`
    /// (the vehicle is lost or destroyed, spent, or another vehicle has the uid now), or an
    /// earlier log whose carry failed (`log`: gone for good). Logged as `restore.closed`
    /// (`written_off`, with the values closed); nothing is owed back for it from then on, in
    /// this run or a later one, and the bridge stops trying. Any operator may (it records a
    /// judgement about the fleet, it authorizes nothing); an agent can't ([`AgentPort`] has no
    /// such method). Refused before rules 12, without a note, for nothing owed, and while an
    /// engagement is under way on the platform (its own restore is still to come).
    pub fn write_off_restore(
        &mut self,
        by: &HumanAuthority,
        what: WriteOff,
        note: &str,
    ) -> Result<(), EngineError> {
        let refuse = |why: String| Err(EngineError::NotWrittenOff(why));
        if !self.policy.rules.followups() {
            return refuse("these decision rules have no write-off of a restore".into());
        }
        let note = note.trim();
        if note.is_empty() {
            return refuse("say why it is written off (a note)".into());
        }
        let (uid, values, log) = match what {
            WriteOff::Platform(uid) => {
                let Some(values) = self.restores.values_owed(&uid).cloned() else {
                    return refuse(format!("nothing is owed back to vehicle {uid}"));
                };
                if let Some(other) = self.engagement_holding(&uid, None) {
                    return refuse(format!(
                        "engagement {other} is under way on vehicle {uid}: its own restore comes when it ends"
                    ));
                }
                (Some(uid), values, None)
            }
            WriteOff::Log(log) => {
                if !self.restores.is_pending(&log) {
                    return refuse(format!("{log} is not an earlier log whose carry failed"));
                }
                (None, BTreeMap::new(), Some(log))
            }
        };
        let body = EventBody::RestoreClosed {
            uid,
            values,
            log,
            closed_by: crate::audit::RestoreClosure::WrittenOff {
                operator: by.operator().clone(),
                session: by.session(),
                note: note.to_string(),
            },
        };
        self.emit(body.clone())?;
        self.restores.step_body(&body);
        Ok(())
    }

    /// Rules 12: the earlier logs whose carry failed, as the ledger has them, with why and the
    /// armed platforms their fleet named (a mission for one waits until the log is read or
    /// written off): for the inbox, what an operator could write off.
    pub fn pending_carries(&self) -> Vec<(String, String, Vec<Uid>)> {
        self.restores.pending()
    }

    /// Rules 12: the engagement whose effect spent `uid`, if one did: no order or nomination
    /// for it is taken any more.
    pub fn expended_by(&self, uid: &Uid) -> Option<IntentId> {
        self.restores.expended_by(uid)
    }

    /// Rules 15: what the ledger owes back to `uid` (its own lost-link values and the vehicle they
    /// were preloaded on), if anything: after an un-spend, what the bridge puts back.
    pub fn owed_to(&self, uid: &Uid) -> Option<crate::replay::OwedRestore> {
        self.restores.owed_restore(uid)
    }

    /// Rules 12: whether the ledger still owes `uid` its own lost-link values.
    pub fn restore_owed_to(&self, uid: &Uid) -> bool {
        self.restores.values_owed(uid).is_some()
    }

    /// Rules 12: the values the ledger owes back that no engagement of this run preloaded
    /// (carried over at the start), with the vehicle they were preloaded on: what the bridge
    /// puts back once a vehicle that matches answers.
    pub fn carried_owed(&self) -> Vec<crate::replay::OwedRestore> {
        self.restores.carried()
    }

    /// Rules 12 (murmur-q05): right after an engagement's release, when its platform is spent
    /// by its own effect (its catalog's `expended_by_effect`, as the release picture `world`
    /// has it), close what is owed back to it (possibly nothing) and mark it spent:
    /// `restore.closed`, `expended`. From then on it takes no order and no nomination (the
    /// `readiness` check fails for it, every dispatch path refuses it, replay flags one sent):
    /// it carries the engagement's lost-link action, and a new preload would read that as its
    /// own values. Nothing to do otherwise.
    fn close_expended(&mut self, id: IntentId, uid: &Uid, world: &WorldSnapshot) -> Result<(), EngineError> {
        if !self.policy.rules.followups() {
            return Ok(());
        }
        let spent = world
            .vehicles
            .get(uid)
            .and_then(|v| v.platform.as_ref())
            .is_some_and(|p| p.expended_by_effect);
        if !spent {
            return Ok(());
        }
        let values = self.restores.values_owed(uid).cloned().unwrap_or_default();
        let body = EventBody::RestoreClosed {
            uid: Some(uid.clone()),
            values,
            log: None,
            closed_by: crate::audit::RestoreClosure::Expended { intent: id },
        };
        let t = self.emit(body.clone())?;
        self.restores.step_at(t, &body);
        Ok(())
    }

    /// Rules 15 (murmur-imu): an operator with engagement authority un-spends a platform spent by
    /// its own effect (this run's mark, or one carried over from an earlier run) that they
    /// believe survived (a miss), under their own session, saying why. Only once the engagement
    /// that spent it ended, the platform was heard after that and is not silent now, and the
    /// vehicle under the uid is the one spent (platform, callsign and system id; never the
    /// home): [`Engine::unspend_refusal`] on `world`. Logged as `platform.unspent` with the
    /// vehicle's facts, its identity and the values the spent mark closed: they are owed back
    /// again from here on (the engagement's fallback is still the platform's lost-link action),
    /// so a mission waits for the restore as under rules 7, and the bridge puts them back when
    /// it is heard. In a credentials run it quotes a step-up (`step`; a passkey's made for this
    /// un-spend, [`Engine::unspend_purpose`], password or passkey per the policy's
    /// `step_up_factor`): it makes an armed platform taskable again. Refused, nothing logged,
    /// before rules 15, without a note, for a platform not spent, one the rules refuse, and
    /// without the step-up a credentials run asks for. An agent can't ([`AgentPort`] has no such
    /// method).
    pub fn unspend_platform(
        &mut self,
        by: &EngagementAuthority,
        uid: Uid,
        note: &str,
        world: &WorldSnapshot,
        step: Option<StepUp>,
    ) -> Result<(), EngineError> {
        let refuse = |why: String| Err(EngineError::NotUnspent(why));
        if !self.policy.rules.carry_followups() {
            return refuse("these decision rules have no un-spend of a platform".into());
        }
        let note = note.trim();
        if note.is_empty() {
            return refuse("say why it is un-spent (a note: what shows it survived)".into());
        }
        let Some(mark) = self.restores.spent_mark(&uid) else {
            return refuse(format!("vehicle {uid} is not spent by its own effect"));
        };
        if let Some(why) = self.unspend_refusal(&uid, world.vehicles.get(&uid), world.identities.get(&uid), world.t) {
            return refuse(format!("vehicle {uid}: {why}"));
        }
        // rules 15: a credentials run re-enters the credential for it (a passkey's made for it)
        let needed = self.auth == Some(AuthMode::Credentials);
        let step_up = match step {
            None if needed => {
                return Err(EngineError::NotAuthenticated {
                    operator: by.operator().clone(),
                    why: "an un-spend makes an armed platform taskable again: re-enter your credential (a step-up)".into(),
                });
            }
            None => None,
            Some(s) => Some(
                self.valid_step(by.operator(), by.session(), &s, Some(&mark.purpose(&uid)))
                    .map_err(|why| EngineError::NotAuthenticated {
                        operator: by.operator().clone(),
                        why,
                    })?,
            ),
        };
        let body = EventBody::PlatformUnspent {
            uid: uid.clone(),
            spent_by: mark.by,
            from: mark.from,
            values: mark.values,
            operator: by.operator().clone(),
            session: by.session(),
            note: note.to_string(),
            world_t: world.t,
            vehicle: Box::new(world.vehicles.get(&uid).cloned().unwrap_or_default()),
            identity: world.identities.get(&uid).cloned(),
            step_up,
        };
        self.emit(body.clone())?;
        self.restores.step_body(&body);
        Ok(())
    }

    /// Rules 15: why `uid` can't be un-spent now, if it can't, on its facts `v` as the world read
    /// them at `t` and the bridge's identity `now` of the vehicle under the uid: not spent; the
    /// engagement that spent it not ended (this run's mark: its terminal transition); not heard
    /// after that engagement's end (a carried mark: since this run started); its link silent
    /// now (not heard within the link-aware fact age limit); or not shown to be the vehicle that
    /// was spent ([`crate::VehicleIdentity::same_vehicle`]). None: an un-spend would be taken (the
    /// bridge's SPENT, STILL HEARD alert offers it then, and only then).
    pub fn unspend_refusal(
        &self,
        uid: &Uid,
        v: Option<&crate::world::VehicleFacts>,
        now: Option<&crate::engage::VehicleIdentity>,
        t: Time,
    ) -> Option<String> {
        let Some(mark) = self.restores.spent_mark(uid) else {
            return Some("not spent by its own effect".into());
        };
        let ended = (mark.from.is_none())
            .then(|| self.intents.get(&mark.by))
            .flatten()
            .filter(|i| i.state().is_terminal())
            .and_then(|i| i.history().last().map(|h| h.t));
        mark.refusal(ended, v, now, t, &self.policy.rules)
    }

    /// Rules 15: what a passkey step-up for un-spending `uid` is for (its challenge binds the
    /// platform, the engagement that spent it and the log of a carried mark).
    pub fn unspend_purpose(&self, uid: &Uid) -> Result<crate::passkey::StepUpFor, EngineError> {
        self.restores
            .spent_mark(uid)
            .map(|m| m.purpose(uid))
            .ok_or_else(|| EngineError::NotUnspent(format!("vehicle {uid} is not spent by its own effect")))
    }

    /// Rules 12 and 15: the platforms spent by their own effect, this run's (`from` none) and
    /// those carried over from earlier runs: for the bridge's inbox.
    pub fn spent_platforms(&self) -> Vec<crate::replay::SpentPlatform> {
        self.restores.spent()
    }

    /// Rules 6: another engagement than `id` under way on vehicle `uid`.
    fn engaged_elsewhere(&self, id: IntentId, uid: &Uid) -> bool {
        self.engagement_holding(uid, Some(id)).is_some()
    }

    /// Rules 6: an engagement (other than `except`) under way on vehicle `uid`: it holds the
    /// platform's lost-link preload, so nothing restores the platform's own values meanwhile.
    fn engagement_holding(&self, uid: &Uid, except: Option<IntentId>) -> Option<IntentId> {
        self.intents()
            .find(|i| {
                Some(i.id()) != except
                    && i.spec().kind() == TaskKind::Engage
                    && i.state() == State::Dispatched
                    && i.spec().uids.first() == Some(uid)
            })
            .map(Intent::id)
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
        self.approve_inner(id, by, note, false, None, world, terrain)
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
        self.approve_inner(id, by, note, true, None, world, terrain)
    }

    /// Rules 8: [`Engine::approve`] (with `acknowledge` as in
    /// [`Engine::approve_acknowledging`]) quoting a step-up, the approver's credential
    /// re-entered just now ([`Engine::step_up`]). A decision marked `step_up` needs one in a
    /// credentials run; the signature records it (`step_up: {seq}`).
    #[allow(clippy::too_many_arguments)]
    pub fn approve_stepped(
        &mut self,
        id: IntentId,
        by: &HumanAuthority,
        step: StepUp,
        note: Option<String>,
        acknowledge: bool,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        self.approve_inner(id, by, note, acknowledge, Some(step), world, terrain)
    }

    #[allow(clippy::too_many_arguments)]
    fn approve_inner(
        &mut self,
        id: IntentId,
        by: &HumanAuthority,
        note: Option<String>,
        acknowledge: bool,
        step: Option<StepUp>,
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
        self.sign(id, by.operator(), by.session(), note, acknowledge, step, world, terrain)
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
        // rules 19: an acceptance as behaving now this picture lapses is logged first, so the
        // signer is shown the acceptances that hold
        self.lapse_behaving(id, world)?;
        let intent = self.get(id)?;
        let frozen = intent.engagement().and_then(|e| e.plan.clone());
        let track_id = engage::params(&spec).map(|p| p.0.to_string()).unwrap_or_default();
        let mut inputs = world.subset_for(&spec);
        self.identify(&mut inputs);
        self.fill_lost(&spec, &mut inputs);
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
        let ttl = self.policy.rules.engage_rules().token_ttl();
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
            unknown_accepted: self.accepted_for(id),
            lost_accepted: self.accepted_lost_for(id),
            behaving_accepted: self.accepted_behaving_for(id),
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
        self.sign_engagement(id, by.operator(), by.session(), note, token, None, world, terrain)
    }

    /// Rules 8: [`Engine::approve_engagement`] quoting a step-up, the signer's credential
    /// re-entered just now ([`Engine::step_up`]): what every engagement signature needs in a
    /// credentials run. The signature records it (`step_up: {seq}`).
    #[allow(clippy::too_many_arguments)]
    pub fn approve_engagement_stepped(
        &mut self,
        id: IntentId,
        by: &EngagementAuthority,
        step: StepUp,
        note: Option<String>,
        token: u64,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        if self.get(id)?.spec().kind() != TaskKind::Engage {
            return Err(EngineError::NotAnEngagement(id));
        }
        self.sign_engagement(id, by.operator(), by.session(), note, token, Some(step), world, terrain)
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
            auth: None,
            step_up: None,
            passkey: None,
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
        step: Option<StepUp>,
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
        // rules 8: a consequential approval quotes the approver's step-up (rules 10: a
        // passkey's, made for a signature on this intent)
        let expected = crate::passkey::StepUpFor::Sign { intent: id, token: None, picture_sha256: None };
        let step_up = self.check_step_up(id, by, session, step, &expected)?;
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
                checked_t: None,
                self_accepted: false,
                step_up,
            })?;
            self.get_mut(id)?.push_concurrence(Approval {
                by: by.clone(),
                session,
                t,
                self_approved,
                self_accepted: false,
                at_submission: false,
                note,
                step_up: step_up.is_some(),
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
            checked_t: None,
            self_accepted: false,
            step_up,
        })?;
        self.get_mut(id)?.set_approval(Approval {
            by: by.clone(),
            session,
            t,
            self_approved,
            self_accepted: false,
            at_submission: false,
            note,
            step_up: step_up.is_some(),
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
        let t = self.transition_at(id, Transition::Approve, reason)?;
        // what this approval rests on: its re-check, the decision that asked for it
        let policy = self.get(id)?.policy().cloned();
        self.get_mut(id)?.set_basis(Basis {
            t,
            by_human: true,
            checks: results,
            policy,
            engaged: inputs.engaged.clone(),
        });
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
        step: Option<StepUp>,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<&Intent, EngineError> {
        // rules 9: the track as decisions read it, its declaration filled from the ledger (the
        // token's track identity is compared with it)
        let filled = self.identified(world);
        let world: &WorldSnapshot = &filled;
        next_state(self.get(id)?.state(), Transition::Approve)
            .map_err(|err| EngineError::Illegal { id, err })?;
        // rules 19: an acceptance as behaving now this picture lapses is logged before the
        // signature reads the acceptances (a token from before the lapse is then refused)
        self.lapse_behaving(id, world)?;
        let intent = self.get(id)?;
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
        // rules 8: every engagement signature quotes the signer's step-up (credentials run);
        // rules 10: a passkey's was made for this intent and this very picture token (its
        // plan and the track's identity, by their hash)
        let expected = crate::passkey::StepUpFor::Sign {
            intent: id,
            token: Some(token),
            picture_sha256: self.tokens.get(&token).map(crate::passkey::picture_sha256),
        };
        let step_up = self.check_step_up(id, by, session, step, &expected)?;
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
        let accepted = self.accepted_for(id);
        let accepted_lost = self.accepted_lost_for(id);
        let accepted_behaving = self.accepted_behaving_for(id);
        if let Err(why) = engage::check_token(
            &shown,
            id,
            by,
            session,
            now,
            world.tracks.get(&track_id),
            frozen.as_ref(),
            &accepted,
            &accepted_lost,
            &accepted_behaving,
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
            // the plan this first signer was shown no longer holds; if the nomination does
            // on the world now (a fresh look would show a plan that passes), they look again;
            // if not, it is void like any approval whose re-check fails
            let (_, _, live, live_ok) =
                self.run_checks_plan(&spec, CheckPhase::Approve, world, terrain, None)?;
            if live_ok {
                let why = format!("the plan you were shown no longer holds ({failed}): look again");
                self.refuse(by, session, id, &why)?;
                return Err(EngineError::StalePicture { id, why });
            }
            self.transition(
                id,
                Transition::Void,
                format!("checks failed at approval: {}", summarize_failures(&live)),
            )?;
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
        // rules 6: the signer also accepted one of its lost vehicles (allowed, recorded)
        let self_accepted = self
            .get(id)?
            .engagement()
            .is_some_and(|e| {
                e.accepted.values().chain(e.accepted_lost.values()).any(|a| &a.by == by)
                    || e.accepted_behaving.values().any(|a| &a.by == by)
            });
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
                checked_t: Some(now),
                self_accepted,
                step_up,
            })?;
            let intent = self.get_mut(id)?;
            intent.push_concurrence(Approval {
                by: by.clone(),
                session,
                t,
                self_approved,
                self_accepted,
                at_submission: false,
                note,
                step_up: step_up.is_some(),
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
        // where the platform will loiter over the point, from its facts on this world
        let loiter = spec
            .uids
            .first()
            .map(|u| engage::loiter_radius(u, &inputs, &self.policy.rules));
        // rules 6: its lost-link action as the fallback (the checks just passed say there is one)
        let preload = engage::order_preload(&spec, &inputs, &snapshot, &self.policy.rules);
        let t = self.emit(EventBody::ApprovalGranted {
            intent: id,
            by: by.clone(),
            session,
            self_approved,
            note: note.clone(),
            snapshot: Some(Box::new(snapshot.clone())),
            token: Some(Box::new(shown)),
            supersedes: Vec::new(),
            checked_t: Some(now),
            self_accepted,
            step_up,
        })?;
        let intent = self.get_mut(id)?;
        intent.set_approval(Approval {
            by: by.clone(),
            session,
            t,
            self_approved,
            self_accepted,
            at_submission: false,
            note,
            step_up: step_up.is_some(),
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
        e.loiter_radius_m = loiter;
        e.preload = preload;
        // rules 9: the picture it holds until its first watch (a revocation before then still
        // finds it, as a bystander or on its target)
        if self.policy.rules.rule_id() {
            self.last_pictures.insert(id, inputs.clone());
        }
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
        // rules 6: the acceptances go with the plan they were given for (given again, on the
        // new picture); rules 16: of lost contacts too
        e.accepted.clear();
        e.accepted_lost.clear();
        // rules 19: and of live tracks as behaving now
        e.accepted_behaving.clear();
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
            auth: None,
            step_up: None,
            passkey: None,
        })?;
        let source = Source::Human {
            id: by.operator().clone(),
        };
        let new_id = self.submit_inner(source, Some(by), proposal, None, world, terrain)?;
        if next_state(old_state, Transition::Void).is_ok() {
            self.transition(id, Transition::Void, format!("superseded by {new_id}"))?;
        }
        self.get(new_id)
    }

    /// An operator withdraws an intent before dispatch (pending, approved or queued:
    /// `voided`), or, rules 11 ([`rules::SPACE_FOLLOWUPS_SINCE`]), a dispatched collect or
    /// cover: the engine then reports what the cancel means for the order under its
    /// resource (`op.state cancelled`, [`space::cancel_detail`]: a tasking request
    /// withdrawn before its pass; a pass already under way, which cannot be recalled, its
    /// product no longer awaited; a watch ended) and the intent is `aborted` (transition
    /// `cancel`). A dispatched vehicle task or engagement is refused as before (a newer
    /// order supersedes the one, an abort stops the other), and an agent withdraws nothing
    /// after dispatch.
    pub fn cancel(
        &mut self,
        id: IntentId,
        by: &HumanAuthority,
        note: &str,
    ) -> Result<&Intent, EngineError> {
        let intent = self.get(id)?;
        let after_dispatch = intent.state() == State::Dispatched
            && intent.spec().task.is_space()
            && self.policy.rules.space_followups();
        let transition = if after_dispatch {
            Transition::Cancel
        } else {
            Transition::Void
        };
        next_state(intent.state(), transition).map_err(|err| EngineError::Illegal { id, err })?;
        let order = intent.space_order().cloned();
        if after_dispatch && order.is_none() {
            return Err(EngineError::NotDispatchable {
                id,
                why: "no space order recorded for its dispatch".into(),
            });
        }
        self.emit(EventBody::OperatorAction {
            operator: by.operator().clone(),
            session: by.session(),
            action: "intent.cancel".into(),
            detail: format!("{id}: {note}"),
            grants: None,
            auth: None,
            step_up: None,
            passkey: None,
        })?;
        if let (true, Some(order)) = (after_dispatch, order) {
            // one clock read: the report is logged at the instant its detail was computed for
            let t = self.now();
            let scen = self.scenario_map().scenario(t);
            let detail = space::cancel_detail(&order, scen);
            self.op_state(id, order.resource(), space::CANCELLED_OP, &detail, OpReport::Engine, Some(t))?;
        }
        self.transition(
            id,
            transition,
            format!("cancelled by {}: {note}", by.operator()),
        )?;
        self.get(id)
    }

    /// Rules 11 ([`rules::SPACE_FOLLOWUPS_SINCE`]): a dispatched collect or cover checked
    /// again against a later space answer (`prepared`: the bridge's, computed outside its
    /// lock on the segment it reloaded, at the request time of the answer the task was
    /// tasked on, [`prepare_space_at`]). An answer with the hash the task already runs on
    /// changes nothing and logs nothing ([`SpaceRecheck::Unchanged`]). A new answer is
    /// checked as a submission is (`check.result`, phase `dispatched`, the rules reading
    /// only the logged answer): the task still holds (`op.state rechecked`,
    /// [`SpaceRecheck::Holds`]: a collect keeps its approved pass, a fresh pass within the
    /// tolerance being the same opportunity; a cover's watch takes the new outages), or it
    /// can no longer be met (`op.state missed`, `no longer feasible: ...`, with a collect's
    /// fresh options for a human to choose from, and the intent failed,
    /// [`SpaceRecheck::Lost`]). Refused for anything but a dispatched space task, a
    /// `prepared` for another task, request time or segment than the engine's, and under
    /// older rules; nothing is logged then.
    pub fn recheck_space(
        &mut self,
        id: IntentId,
        prepared: PreparedSpace,
    ) -> Result<SpaceRecheck, EngineError> {
        let refuse = |why: &str| EngineError::NotDispatchable {
            id,
            why: why.to_string(),
        };
        if !self.policy.rules.space_followups() {
            return Err(refuse("these decision rules re-check no space task after dispatch"));
        }
        let intent = self.get(id)?;
        let spec = intent.spec().clone();
        if !spec.task.is_space() {
            return Err(refuse("not a collect or cover"));
        }
        if intent.state() != State::Dispatched {
            return Err(refuse("not dispatched: a space task is re-checked only while it runs"));
        }
        if prepared.task != spec.task {
            return Err(refuse("the prepared answer is for another task"));
        }
        let current = intent.space_answer();
        let fresh = prepared.inputs.answer();
        if let (Some(a), Some(b)) = (current.and_then(SpaceAnswer::now), fresh.and_then(SpaceAnswer::now))
            && a != b
        {
            return Err(refuse(
                "the prepared answer is for another request time than the answer the task runs on",
            ));
        }
        match (self.space.as_ref().map(|o| o.segment()), fresh) {
            (Some(seg), Some(a)) if a.segment != *seg => {
                return Err(refuse("the prepared answer is from another space segment (or pin) than the engine's"));
            }
            (None, Some(_)) => return Err(refuse("no space segment loaded")),
            _ => {}
        }
        let order = intent
            .space_order()
            .cloned()
            .ok_or_else(|| refuse("no space order recorded for its dispatch"))?;
        // a collect whose pass has come was collected (simulated): nothing to re-check, and
        // a later answer must not fail it. One clock read: the check below is logged at the
        // instant this judged (replay judges on the check's time)
        let guard_t = self.now();
        if let SpaceOrder::Collect(c) = &order
            && self.scenario_map().scenario(guard_t) >= c.collect_time
        {
            return Err(refuse("its pass has come: the scene was collected, there is nothing to re-check"));
        }
        // rules 13: the re-check is logged at the guard's instant, so its answer was computed
        // by then
        if self.policy.rules.space_replay() && prepared.t > guard_t {
            return Err(refuse("the prepared answer was computed after the re-check's instant"));
        }
        // a query that failed on the segment is not a later answer: nothing is known
        if let SpaceInputs::Unavailable { reason } = &prepared.inputs {
            return Ok(SpaceRecheck::Unknown {
                reason: reason.clone(),
            });
        }
        let old_hash = current.map(SpaceAnswer::content_hash).transpose()?;
        if let SpaceInputs::Answered { sha256, .. } = &prepared.inputs
            && Some(sha256) == old_hash.as_ref()
        {
            return Ok(SpaceRecheck::Unchanged);
        }
        let t = prepared.t;
        self.prepared_space = Some(prepared);
        let world = WorldSnapshot::new(t);
        let (inputs, _, results, passed) = self.run_checks_inner(
            &spec,
            CheckPhase::Dispatched,
            &world,
            &NoTerrain,
            None,
            Some(guard_t),
        )?;
        // the fresh options are ranked from the check's own scenario time
        let checked_at = self.scenario_map().scenario(self.last_t);
        let detail = space::recheck_detail(&results, passed, inputs.space.answer(), checked_at);
        if passed {
            self.op_state(id, order.resource(), space::RECHECKED_OP, &detail, OpReport::Engine, None)?;
            Ok(SpaceRecheck::Holds { detail })
        } else {
            self.op_state(id, order.resource(), space::MISSED_OP, &detail, OpReport::Engine, None)?;
            self.transition(id, Transition::Fail, space::missed_reason(&spec.task, &detail))?;
            Ok(SpaceRecheck::Lost { detail })
        }
    }

    /// Log something an operator did that isn't an intent decision (acknowledge, snooze).
    /// Rules 8: the session's own events (`session.opened`, `session.closed`, `step_up`) are
    /// the engine's to log, refused here; a hostile declaration the policy marks
    /// consequential goes through [`Engine::record_declaration`] with its step-up.
    pub fn record_operator_action(
        &mut self,
        by: &HumanAuthority,
        action: &str,
        detail: &str,
    ) -> Result<(), EngineError> {
        if self.auth.is_some() && matches!(action, "session.opened" | "session.closed" | "step_up") {
            return Err(EngineError::NotAuthenticated {
                operator: by.operator().clone(),
                why: format!("{action} is logged by the session itself"),
            });
        }
        if action == "track.declare" && self.policy.rules.rule_id() {
            return Err(EngineError::Identification(
                "under these rules a declaration goes through the identification ledger (declare_track)".into(),
            ));
        }
        if action == "track.declare" && declared_affiliation(detail) == Some("hostile") {
            let expected = crate::passkey::StepUpFor::DeclareHostile { track: String::new() };
            self.act_step_up(policy::StepUpAct::DeclareHostile, by.operator(), by.session(), None, &expected)?;
        }
        self.emit(EventBody::OperatorAction {
            operator: by.operator().clone(),
            session: by.session(),
            action: action.into(),
            detail: detail.into(),
            grants: None,
            auth: None,
            step_up: None,
            passkey: None,
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
    /// Refused ([`EngineError::NotItsOrder`]) unless `by` is another intent, the one
    /// dispatched last and still under way, to vehicles that include the engagement's
    /// platform (the bridge supersedes in the same step as that dispatch): replay checks the
    /// same.
    pub fn supersede_engagement(&mut self, id: IntentId, by: IntentId) -> Result<(), EngineError> {
        let platform = self.get(id)?.spec().uids.first().cloned();
        let other = self.get(by)?;
        let queued = other.state() == State::Queued;
        let rules7 = self.policy.rules.rules_version >= rules::QUEUE_FOLLOWUPS_SINCE;
        // the order dispatched last (rules 7: or queued last) stands; from rules 7 only in the
        // step that dispatched or queued it, nothing else logged in between
        let last = if rules7 {
            self.step == Some((by, self.seq))
        } else if queued {
            self.last_queued == Some(by)
        } else {
            self.last_dispatched == Some(by)
        };
        let queued_ok = rules7;
        if let Some(why) = supersede_mismatch(
            id,
            by,
            platform.as_ref(),
            other.spec(),
            other.state(),
            last,
            queued_ok,
        ) {
            return Err(EngineError::NotItsOrder { id, why: why.into() });
        }
        let source = other.spec().source.clone();
        let who = match &source {
            Source::Human { id } => format!("human {id}"),
            Source::Agent { id } => format!("agent {id}"),
        };
        let how = match other.state() {
            State::Queued => " (queued until its vehicles can be reached)",
            State::Voided => " (not sent: its platform's own lost-link values go back first)",
            _ => "",
        };
        let reason = format!("superseded by {by}{how} from {who}: that order stands; no fallback commanded");
        self.stop_engagement(id, StopCause::Superseded { by, source }, reason, false)?;
        if rules7 {
            // the step goes on: another engagement on its vehicles may be stopped next
            self.step = Some((by, self.seq));
        }
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
        // rules 9: a revoked declaration stops a nomination still waiting for signatures too
        let revoked = matches!(cause, StopCause::Revoked { .. });
        let (transition, fallback) = match state {
            State::PendingApproval if superseded || revoked => (Transition::Void, None),
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
                        && i.engagement().is_some_and(|e| e.plan.is_some() || !e.accepted_behaving.is_empty())))
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
        // rules 19: the acceptances as behaving now this picture lapses go first (logged, dropped
        // for good), so the step reads only those that hold; a pending nomination with one is
        // stepped for this alone until a signature freezes a plan
        if matches!(
            self.get(id)?.state(),
            State::PendingApproval | State::Approved | State::Dispatched
        ) {
            self.lapse_behaving(id, world)?;
        }
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
            let mut inputs = world.for_intent_plan(&spec, terrain, &self.policy.rules, Some(&plan));
            self.identify(&mut inputs);
            inputs.unknown_accepted = eng.accepted.clone();
            self.fill_lost(&spec, &mut inputs);
            let results = engage::evaluate(&spec, &inputs, &self.policy.rules);
            if results.iter().any(|c| c.outcome != Outcome::Pass) {
                let logged = self.log_inputs(&inputs)?;
                let why = format!("the plan no longer holds: {}", summarize_failures(&results));
                self.reset_signatures(id, why, Some(logged))?;
            }
            return Ok(None);
        }
        let mut inputs = world.subset_for(&spec);
        self.identify(&mut inputs);
        inputs.unknown_accepted = eng.accepted.clone();
        self.fill_lost(&spec, &mut inputs);
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
        // rules 6: the release needs the platform's lost-link action read back as the
        // fallback while it was dispatched
        let preloaded = eng
            .preload
            .as_ref()
            .zip(engage::preload_confirmed_at(&eng));
        let (results, holds, ready) = engage::picture(
            &spec,
            &snap,
            &inputs,
            &self.policy.rules,
            dispatched,
            prev_t,
            preloaded,
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
        if self.policy.rules.rule_id() {
            self.last_pictures.insert(id, inputs.clone());
        }
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
        // rules 12: a platform spent by its own effect is owed nothing back from now on
        self.close_expended(id, &uid, &inputs)?;
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

    /// Rules 6: the vehicles an operator accepted as lost for engagement `id` (a fact the
    /// engine logs with each of its decisions, `unknown_accepted`; replay tracks the
    /// `engage.accepted` events and compares).
    fn accepted_for(&self, id: IntentId) -> BTreeMap<Uid, engage::AcceptedUnknown> {
        self.intents
            .get(&id)
            .and_then(Intent::engagement)
            .map(|e| e.accepted.clone())
            .unwrap_or_default()
    }

    /// Rules 5: the human-approved orders waiting in the queue on `spec`'s vehicles, other
    /// than `spec` itself: a fact the engine logs with each decision (`waiting`).
    fn waiting_for(&self, spec: &IntentSpec) -> BTreeMap<Uid, IntentId> {
        waiting_vehicles(
            self.intents.values().map(|i| {
                (
                    i.id(),
                    i.spec(),
                    i.state(),
                    i.basis().is_some_and(|b| b.by_human),
                    !i.committed().is_empty(),
                )
            }),
            spec,
        )
    }

    /// Rules 7: `spec`'s vehicles still owed their own lost-link values by an engagement of
    /// this run (a fact the engine logs with each decision, `restore_owed`; replay recomputes
    /// it from the log's ledger). Empty before rules 7 and for anything but a vehicle task.
    fn restore_owed_for(
        &self,
        spec: &IntentSpec,
        identities: &BTreeMap<Uid, engage::VehicleIdentity>,
    ) -> BTreeMap<Uid, queue::RestoreOwed> {
        self.restores.owed_for(spec, &self.policy.rules, identities)
    }

    /// Rules 7: why `spec` (a vehicle task other than a hold or rtl) can't go now: a vehicle
    /// still owed its own lost-link values by an engagement of this run (rules 12: or carried
    /// over from an earlier run's, unless `owed` shows the vehicle is another). `owed`: the
    /// fact its reach was judged on; none (a kind the policy never queues, no reach logged):
    /// the ledger's, carried values holding whatever vehicle has the uid. None otherwise.
    fn restore_hold(
        &self,
        spec: &IntentSpec,
        owed: Option<&BTreeMap<Uid, queue::RestoreOwed>>,
    ) -> Option<String> {
        // rules 15: an engagement's (a failed carry that may owe its platform values) is refused
        // at nomination, by its fallback_preload check
        if matches!(spec.kind(), TaskKind::Hold | TaskKind::Rtl | TaskKind::Engage) {
            return None;
        }
        let ledger;
        let owed = match owed {
            Some(o) => o,
            None => {
                ledger = self.restore_owed_for(spec, &BTreeMap::new());
                &ledger
            }
        };
        let (uid, o) = owed.iter().find(|(_, o)| o.holds())?;
        Some(format!("vehicle {uid}: {}", o.waiting_detail()))
    }

    /// Rules 7: why a mission of a kind the policy never queues is refused (voided) instead of
    /// sent, if it is: a vehicle still owed its own lost-link values (no reach is logged for
    /// such a kind, so values carried over from an earlier run hold whatever vehicle has the
    /// uid). Says whether an engagement in play there is stopped by this order (the bridge
    /// supersedes it in the same step; its restore then goes) or the restore is pending, that a
    /// hold or rtl goes, and (rules 12) the orders still queued on its vehicles.
    fn refusal(&self, spec: &IntentSpec) -> Option<String> {
        let owed = self.restore_owed_for(spec, &BTreeMap::new());
        let engaged: BTreeSet<IntentId> = self.engaged_for(spec).into_values().collect();
        let standing = queued_on(
            self.intents.values().map(|i| (i.id(), i.spec(), i.state())),
            spec,
        );
        refusal_reason(spec, &owed, &engaged, &standing, &self.policy.rules)
    }

    /// What [`queue::reach`] reads for `spec`: its part of `world`, and (rules 7) its
    /// vehicles' lost-link values still owed back (rules 12: with the bridge's identity of a
    /// vehicle owed values carried over).
    fn reach_inputs(&self, spec: &IntentSpec, world: &WorldSnapshot) -> WorldSnapshot {
        let mut inputs = world.subset_for(spec);
        self.identify(&mut inputs);
        inputs.restore_owed = self.restore_owed_for(spec, &inputs.identities);
        inputs.expended = self.restores.expended_for(spec, &self.policy.rules);
        inputs.spent_carried = self.restores.spent_carried_for(spec, &self.policy.rules, &inputs.identities);
        inputs
    }

    /// For the console (not a decision, not logged): whether each of a queued intent's
    /// vehicles can be reached for it on `world`, and why not, as its dequeue would judge it.
    pub fn reach_now(&self, id: IntentId, world: &WorldSnapshot) -> Option<Vec<Reach>> {
        let spec = self.intents.get(&id)?.spec();
        Some(queue::reach(
            spec,
            &self.reach_inputs(spec, world),
            &self.policy.queue(),
            &self.policy.rules,
        ))
    }

    /// Rules 6: the platforms' own lost-link values owed back as this run's log has them
    /// (from a preload's write-ahead, or carried over), and the engagements that preloaded
    /// them (none: carried over from an earlier run).
    pub fn owed_restores(&self) -> Vec<(Uid, BTreeMap<String, f64>, Vec<IntentId>)> {
        self.restores.owed()
    }

    /// Rules 5: a newer order still standing for one of the intent's vehicles, and which
    /// vehicle (see [`Engine::dequeue`]): an older order is neither sent nor followed over it.
    pub fn newer_order(&self, id: IntentId) -> Option<(IntentId, Uid)> {
        self.newer_on(self.intents.get(&id)?.spec())
    }

    /// Rules 5: a newer order still standing for one of `spec`'s vehicles (approved, queued,
    /// dispatched, or waiting for a human's approval after a human submitted it), and which
    /// vehicle. An older queued order never goes over it: the newest order stands.
    fn newer_on(&self, spec: &IntentSpec) -> Option<(IntentId, Uid)> {
        self.intents.values().find_map(|i| {
            let standing = match i.state() {
                State::Approved | State::AutoApproved | State::Queued | State::Dispatched => true,
                State::PendingApproval => !i.spec().source.is_agent(),
                _ => false,
            };
            if i.id() <= spec.id || i.spec().kind() == TaskKind::Engage || !standing {
                return None;
            }
            let u = spec.uids.iter().find(|u| i.spec().uids.contains(u))?;
            Some((i.id(), u.clone()))
        })
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
        let mut inputs = world.for_intent_plan(spec, terrain, &self.policy.rules, plan);
        self.identify(&mut inputs);
        self.fill_engine_facts(spec, &mut inputs);
        let checks = rules::evaluate(spec, &inputs, &self.policy.rules);
        let pv = match plan {
            Some(p) => Some(p.preview.clone()),
            None => engage::preview(spec, &inputs, &self.policy.rules).ok(),
        };
        Some((checks, pv))
    }

    /// Rules 19: for the console's card of engagement `id` (pending, approved or under way), each
    /// live track that blocks its keep-out on `world` now and what accepting it as behaving now
    /// would do, and each track accepted for it as the acceptance has it now (its plan's effect
    /// point once a signature froze one), read as the checks read them ([`crate::keepout::
    /// behaving_options`]). Read-only, nothing logged; empty before rules 19 or when there is
    /// nothing to judge.
    pub fn behaving_options(
        &self,
        id: IntentId,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Vec<crate::keepout::BehavingInfo> {
        let Some(intent) = self.intents.get(&id) else {
            return Vec::new();
        };
        let spec = intent.spec();
        if spec.kind() != TaskKind::Engage || !self.policy.rules.accept_behaving() {
            return Vec::new();
        }
        let mut inputs = world.for_intent_with(spec, terrain, &self.policy.rules);
        self.identify(&mut inputs);
        self.fill_engine_facts(spec, &mut inputs);
        // a frozen plan's (or the approval's) effect point and time: what the signers signed
        let plan = intent
            .engagement()
            .and_then(|e| e.snapshot.as_ref().map(|s| &s.preview).or(e.plan.as_ref().map(|p| &p.preview)));
        match plan {
            Some(p) => crate::keepout::behaving_at(spec, &inputs, &self.policy.rules, p).unwrap_or_default(),
            None => crate::keepout::behaving_options(spec, &inputs, &self.policy.rules).unwrap_or_default(),
        }
    }

    // ---- identification (rules 9) ----------------------------------------------------

    /// Rules 9: fill the picture's identification from the ledger (declarations with their
    /// basis, flags in force, suspects raised), as every decision reads and logs it. Before
    /// rules 9 the picture is the bridge's own.
    fn identify(&self, w: &mut WorldSnapshot) {
        if self.policy.rules.rule_id() {
            self.ident.fill(&mut w.tracks);
        }
        self.drop_later_facts(w);
    }

    /// Rules 12: when the tracker started a track (`first_seen_t`) is a fact only rules 12
    /// read; rules 14: its velocity's 1-sigma (`velocity_sigma_mps`) one only rules 14 read: an
    /// engine of older rules never logs them (a log claiming them can't carry them).
    fn drop_later_facts(&self, w: &mut WorldSnapshot) {
        if !self.policy.rules.followups() {
            for t in w.tracks.values_mut() {
                t.first_seen_t = None;
            }
        }
        if self.policy.rules.rules_version < crate::rules::AIRCRAFT_REACH_SINCE {
            for t in w.tracks.values_mut() {
                t.velocity_sigma_mps = None;
            }
        }
        // rules 16: a track's AIS identity is a fact only rules 16 read
        if !self.policy.rules.lost_contacts() {
            w.track_ais.clear();
        }
    }

    /// The world as decisions read it (rules 9: its identification filled from the ledger),
    /// for readers outside the engine (the console, agents) and the engine's own checks.
    pub fn identified<'w>(&self, world: &'w WorldSnapshot) -> std::borrow::Cow<'w, WorldSnapshot> {
        // always a copy: rules 9 fill the identification, and older rules drop rules 12's facts
        let mut w = world.clone();
        self.identify(&mut w);
        std::borrow::Cow::Owned(w)
    }

    /// The inputs a decision on `spec` would read on `world` now, filled as the engine fills
    /// them before its checks (rules 9 identification, then [`Engine::fill_engine_facts`]), for
    /// a dry run that is not a decision (the nomination form's, `POST /api/engage/options`, and
    /// agents' `get_nomination_options`): nothing is logged or changed. A space task's answer is
    /// not asked for.
    pub fn dry_run_inputs(&self, spec: &IntentSpec, world: &WorldSnapshot, terrain: &dyn TerrainOracle) -> WorldSnapshot {
        let mut inputs = world.for_intent_with(spec, terrain, &self.policy.rules);
        self.identify(&mut inputs);
        // (a new nomination has no acceptances yet)
        self.fill_engine_facts(spec, &mut inputs);
        inputs
    }

    /// What the engine fills into a decision's inputs from its own state, after the world's part,
    /// the identification and a space task's answer: the engagements in play and the approved
    /// orders waiting on its vehicles, their own lost-link values still owed back (rules 7 and
    /// 12), the platforms spent by their own effect (rules 12) and carried over as spent (rules
    /// 15), an engagement's acceptances of lost vehicles (rules 6) and its lost contacts with the
    /// acceptances of them (rules 16). Every decision's checks read them so, and so does every dry
    /// run that says what a decision would do (an agent's preview, the nomination form's options,
    /// the console's live checks of a waiting engagement): one fill, so none drifts from the
    /// decision (the review of the merged branch: the agent's preview had no lost contacts).
    fn fill_engine_facts(&self, spec: &IntentSpec, inputs: &mut WorldSnapshot) {
        inputs.engaged = self.engaged_for(spec);
        inputs.waiting = self.waiting_for(spec);
        inputs.restore_owed = self.restore_owed_for(spec, &inputs.identities);
        inputs.expended = self.restores.expended_for(spec, &self.policy.rules);
        inputs.spent_carried = self.restores.spent_carried_for(spec, &self.policy.rules, &inputs.identities);
        if spec.kind() == TaskKind::Engage {
            inputs.unknown_accepted = self.accepted_for(spec.id);
        }
        self.fill_lost(spec, inputs);
    }

    /// Rules 9: the identification ledger (declarations in force, flags, acts seen).
    pub fn identification(&self) -> &crate::ident::Ledger {
        &self.ident
    }

    fn rule_id_only(&self, what: &str) -> Result<(), EngineError> {
        if self.policy.rules.rule_id() {
            Ok(())
        } else {
            Err(EngineError::Identification(format!(
                "these decision rules (version {}) keep no identification ledger: {what} needs rules {}",
                self.policy.rules.rules_version,
                rules::RULE_ID_SINCE
            )))
        }
    }

    /// Log an identification event at the engine's time and step the ledger with it.
    fn emit_ident(&mut self, body: EventBody) -> Result<Time, EngineError> {
        let t = self.now();
        self.emit_ident_at(t, body)
    }

    /// [`Self::emit_ident`] at `t`, a time already read for the event's own fields (a
    /// declaration's or flag's `t`), so the two never differ by a clock tick between reads.
    fn emit_ident_at(&mut self, t: Time, body: EventBody) -> Result<Time, EngineError> {
        let t = self.emit_at(t, body.clone())?;
        self.ident.step(t, &body);
        Ok(t)
    }

    /// Rules 9: the bridge's sensing model observed an abstract hostile act (`report`). The
    /// engine attributes it to the tracks in `world` (their identification filled from the
    /// ledger), logs the report, that picture, the attribution and the hostile-act rule's
    /// verdict (`ident.act`), and carries the verdict out at once: a rule declaration
    /// (`ident.declared`, the act's evidence with it) when the best track reaches the
    /// policy's attribution threshold and nothing conflicts, else a review flag
    /// (`ident.flagged`), or nothing more (the rule is off, the track is already declared
    /// hostile). Refused, nothing logged, for an act reported twice or before rules 9.
    pub fn record_act(
        &mut self,
        report: crate::ident::ActReport,
        world: &WorldSnapshot,
    ) -> Result<ActOutcome, EngineError> {
        self.rule_id_only("an observed hostile act")?;
        if self.ident.seen_act(&report.act) {
            return Err(EngineError::Identification(format!(
                "act {} was reported already",
                report.act
            )));
        }
        if !(report.sigma_m.is_finite() && report.sigma_m > 0.0) || report.sensors.is_empty() {
            return Err(EngineError::Identification(format!(
                "act {}: an observation needs the sensors that saw it and a position error above zero",
                report.act
            )));
        }
        let cfg = self.policy.identification();
        let mut tracks = world.tracks.clone();
        self.ident.fill(&mut tracks);
        // our fleet, for an act against one of our vehicles
        let vehicles: std::collections::BTreeSet<Uid> = match &report.against {
            crate::ident::Against::Vehicle { .. } => world.vehicles.keys().cloned().collect(),
            _ => std::collections::BTreeSet::new(),
        };
        let candidates = crate::ident::attribute(&report, &tracks, world.t, &cfg);
        let verdict =
            crate::ident::verdict(&report, &candidates, &tracks, &vehicles, &self.ident, world.t, &cfg);
        self.emit_ident(EventBody::IdentAct {
            act: report.clone(),
            inputs: crate::ident::IdentInputs {
                t: world.t,
                tracks,
                vehicles,
            },
            candidates: candidates.clone(),
            verdict: verdict.clone(),
        })?;
        let mut out = ActOutcome {
            verdict: verdict.clone(),
            candidates,
            declaration: None,
            flag: None,
        };
        match verdict {
            crate::ident::ActVerdict::Declare {
                track,
                attribution,
                corroborated_by,
            } => {
                let t = self.now();
                let d = crate::ident::rule_declaration(
                    self.ident.next_declaration(),
                    t,
                    &report,
                    &track,
                    attribution,
                    corroborated_by,
                    &cfg,
                );
                self.emit_ident_at(
                    t,
                    EventBody::IdentDeclared {
                        declaration: Box::new(d.clone()),
                    },
                )?;
                out.declaration = Some(d);
            }
            crate::ident::ActVerdict::Flag { kind, tracks, why } => {
                let t = self.now();
                let f = crate::ident::act_flag(self.ident.next_flag(), t, &report, kind, &tracks, &why);
                self.emit_ident_at(
                    t,
                    EventBody::IdentFlagged {
                        flag: f.clone(),
                        inputs: None,
                    },
                )?;
                out.flag = Some(f);
            }
            crate::ident::ActVerdict::Recorded { .. } => {}
        }
        Ok(out)
    }

    /// Rules 9: an operator declares a track's affiliation (any operator may, as before rules
    /// 9; logged as `ident.declared` under their session). A hostile declaration quotes a
    /// step-up when `[approval] step_up_acts` lists `declare_hostile` (a credentials run, as
    /// rules 8's `track.declare`). `flag` names the review flag it answers (in force, naming
    /// this track or none). Over a declaration in force: the same affiliation again is refused
    /// (a rule declaration not confirmed yet is confirmed instead,
    /// [`Engine::confirm_declaration`]); another affiliation first revokes the one in force
    /// (`ident.revoked`, stopping the engagements in play on the track, as
    /// [`Engine::revoke_declaration`]) and then declares. Refused, nothing logged, for a track
    /// not in `world`'s picture.
    #[allow(clippy::too_many_arguments)]
    pub fn declare_track(
        &mut self,
        by: &HumanAuthority,
        step: Option<StepUp>,
        track: &str,
        affiliation: crate::world::Affiliation,
        note: Option<String>,
        flag: Option<u64>,
        world: &WorldSnapshot,
    ) -> Result<IdentOutcome, EngineError> {
        self.rule_id_only("a declaration through the engine")?;
        if !world.tracks.contains_key(track) {
            return Err(EngineError::Identification(format!("track {track} is not in the picture")));
        }
        // rules 8: declaring a track hostile may be marked consequential (a step-up; rules 10:
        // a passkey's made for declaring this track hostile)
        let step_up = if affiliation == crate::world::Affiliation::Hostile {
            let expected = crate::passkey::StepUpFor::DeclareHostile { track: track.to_string() };
            self.act_step_up(policy::StepUpAct::DeclareHostile, by.operator(), by.session(), step, &expected)?
        } else {
            match step {
                Some(s) => Some(
                    self.valid_step(by.operator(), by.session(), &s, None)
                        .map_err(|why| EngineError::NotAuthenticated { operator: by.operator().clone(), why })?,
                ),
                None => None,
            }
        };
        if let Some(f) = flag {
            match self.ident.flag(f) {
                Some((fl, status)) if status.in_force() && (fl.tracks.is_empty() || fl.tracks.iter().any(|t| t == track)) => {}
                Some(_) => {
                    return Err(EngineError::Identification(format!(
                        "flag F-{f} is not in force on track {track}"
                    )));
                }
                None => return Err(EngineError::Identification(format!("no flag F-{f}"))),
            }
        }
        let mut out = IdentOutcome::default();
        if let Some(d) = self.ident.declaration(track).cloned() {
            if d.affiliation == affiliation {
                let hint = if d.unconfirmed_rule() { ": confirm it instead" } else { "" };
                return Err(EngineError::Identification(format!(
                    "{track} is already {}{hint}",
                    d.describe()
                )));
            }
            out = self.revoke_inner(
                by,
                track,
                Some(format!("replaced by {}'s declaration {}", by.operator(), affiliation.as_str())),
            )?;
        }
        let t = self.now();
        let d = crate::ident::Declaration {
            id: self.ident.next_declaration(),
            track: track.to_string(),
            affiliation,
            t,
            by: crate::ident::Declarer::Operator {
                operator: by.operator().clone(),
                session: by.session(),
                note,
                flag,
                step_up,
            },
            confirmed: None,
        };
        self.emit_ident_at(
            t,
            EventBody::IdentDeclared {
                declaration: Box::new(d.clone()),
            },
        )?;
        out.declaration = Some(d);
        Ok(out)
    }

    /// Rules 9: any operator confirms a rule declaration in force and not confirmed yet
    /// (`ident.confirmed`, recorded under their session; a step-up when `[approval]
    /// step_up_acts` lists `confirm_declaration`, off by default). A confirmed rule
    /// declaration is exempt in another engagement's keep-out, and may be nominated whatever
    /// `nominate_unconfirmed` says.
    pub fn confirm_declaration(
        &mut self,
        by: &HumanAuthority,
        step: Option<StepUp>,
        track: &str,
    ) -> Result<IdentOutcome, EngineError> {
        self.rule_id_only("a confirmation")?;
        let Some(d) = self.ident.declaration(track).cloned() else {
            return Err(EngineError::Identification(format!("no declaration in force on track {track}")));
        };
        if !d.unconfirmed_rule() {
            return Err(EngineError::Identification(format!(
                "{track} is {}: only a rule declaration not yet confirmed is confirmed",
                d.describe()
            )));
        }
        let expected = crate::passkey::StepUpFor::ConfirmDeclaration { track: track.to_string(), declaration: d.id };
        let step_up = self.act_step_up(policy::StepUpAct::ConfirmDeclaration, by.operator(), by.session(), step, &expected)?;
        self.emit_ident(EventBody::IdentConfirmed {
            track: track.to_string(),
            declaration: d.id,
            by: by.operator().clone(),
            session: by.session(),
            step_up,
        })?;
        Ok(IdentOutcome {
            declaration: self.ident.declaration(track).cloned(),
            ..IdentOutcome::default()
        })
    }

    /// Rules 9: any operator revokes the declaration in force on `track` (an operator's or a
    /// rule's; it only restricts, like an abort; `note` optional, logged). Logged as
    /// `ident.revoked` with the engagements it stops: those in play on the track, and those
    /// under way whose last picture held the track exempt from their keep-out and would fail
    /// it without the declaration (a bystander). They are stopped next, before anything else
    /// (cause `revoked`): voided before dispatch, aborted with their fallback after it
    /// (returned for the bridge to command). The track goes back to its estimate; a later act
    /// attributed to it is flagged, not declared (an operator's judgment outranks the rule,
    /// until an operator declares it again).
    pub fn revoke_declaration(
        &mut self,
        by: &HumanAuthority,
        track: &str,
        note: Option<String>,
    ) -> Result<IdentOutcome, EngineError> {
        self.rule_id_only("a revocation")?;
        self.revoke_inner(by, track, note)
    }

    fn revoke_inner(
        &mut self,
        by: &HumanAuthority,
        track: &str,
        note: Option<String>,
    ) -> Result<IdentOutcome, EngineError> {
        let Some(d) = self.ident.declaration(track).cloned() else {
            return Err(EngineError::Identification(format!("no declaration in force on track {track}")));
        };
        let on_track = crate::ident::engagements_on(
            self.intents.values().map(|i| (i.id(), i.spec(), i.state())),
            track,
        );
        let t = self.now();
        let body = EventBody::IdentRevoked {
            track: track.to_string(),
            declaration: d.id,
            by: by.operator().clone(),
            session: by.session(),
            note: note.clone(),
            engagements: Vec::new(),
        };
        // the bystanders: engagements under way whose keep-out this declaration exempted
        let mut after = self.ident.clone();
        after.step(t, &body);
        let bystanders = crate::ident::revocation_bystanders(
            self.intents
                .values()
                .filter(|i| matches!(i.state(), State::Approved | State::Dispatched))
                .filter_map(|i| {
                    let snap = i.engagement()?.snapshot.as_ref()?;
                    let w = self.last_pictures.get(&i.id())?;
                    Some((i.id(), snap.track_id.as_str(), snap, w))
                }),
            track,
            &self.ident,
            &after,
            &self.policy.rules,
        );
        let mut engagements = on_track.clone();
        engagements.extend(bystanders.iter().copied());
        engagements.sort();
        engagements.dedup();
        self.emit_ident_at(
            t,
            EventBody::IdentRevoked {
                track: track.to_string(),
                declaration: d.id,
                by: by.operator().clone(),
                session: by.session(),
                note: note.clone(),
                engagements: engagements.clone(),
            },
        )?;
        let mut out = IdentOutcome {
            revoked: Some(d.id),
            ..IdentOutcome::default()
        };
        let why = note.map(|n| format!(": {n}")).unwrap_or_default();
        for id in engagements {
            let cause = StopCause::Revoked {
                by: by.operator().clone(),
                session: by.session(),
                track: track.to_string(),
                declaration: d.id,
            };
            let reason = if bystanders.contains(&id) && !on_track.contains(&id) {
                format!(
                    "keep_out_tracks: the declaration of {track}, which exempted it from this engagement's keep-out, was revoked by {}{why} (it was {})",
                    by.operator(),
                    d.describe()
                )
            } else {
                format!(
                    "target_declared: the declaration of {track} was revoked by {}{why} (it was {})",
                    by.operator(),
                    d.describe()
                )
            };
            match self.stop_engagement(id, cause, reason, true)? {
                Some(f) => out.fallbacks.push(f),
                None => out.voided.push(id),
            }
        }
        Ok(out)
    }

    /// Rules 9: any operator resolves a review flag: confirms one that is open (a proximity
    /// flag then keeps its track suspect), or dismisses one that is open or confirmed (it no
    /// longer raises its track). Logged as `ident.flag_resolved`. An operator's declaration
    /// that names the flag resolves it too ([`Engine::declare_track`]).
    pub fn resolve_flag(
        &mut self,
        by: &HumanAuthority,
        flag: u64,
        resolution: crate::ident::FlagResolution,
        note: Option<String>,
    ) -> Result<(), EngineError> {
        self.rule_id_only("a review flag")?;
        let Some((_, status)) = self.ident.flag(flag) else {
            return Err(EngineError::Identification(format!("no flag F-{flag}")));
        };
        let ok = match resolution {
            crate::ident::FlagResolution::Confirmed => matches!(status, crate::ident::FlagStatus::Open),
            crate::ident::FlagResolution::Dismissed => status.in_force(),
        };
        if !ok {
            return Err(EngineError::Identification(format!(
                "flag F-{flag} is {}: it can't be {}",
                status.name(),
                match resolution {
                    crate::ident::FlagResolution::Confirmed => "confirmed",
                    crate::ident::FlagResolution::Dismissed => "dismissed",
                }
            )));
        }
        self.emit_ident(EventBody::IdentFlagResolved {
            flag,
            resolution,
            by: by.operator().clone(),
            session: by.session(),
            note,
        })?;
        Ok(())
    }

    /// Rules 9, once a second (the bridge's loop): the weak-evidence rule. Every undeclared
    /// track keeping company with a declared hostile ([`crate::ident::escort`]) is flagged
    /// (`ident.flagged`, kind proximity, with the two tracks' facts) and so raised to suspect,
    /// each pair once. Never a declaration. Returns the new flags' numbers. Nothing before
    /// rules 9 or with rule declaration off.
    pub fn step_identification(&mut self, world: &WorldSnapshot) -> Result<Vec<u64>, EngineError> {
        let cfg = self.policy.identification();
        if !self.policy.rules.rule_id() || !cfg.rule_declaration {
            return Ok(Vec::new());
        }
        let mut new = Vec::new();
        // one at a time: each flag changes the picture the next one reads (a raised suspect)
        loop {
            let mut tracks = world.tracks.clone();
            self.ident.fill(&mut tracks);
            let Some(found) = crate::ident::escort(&tracks, &self.ident, world.t, &cfg).into_iter().next() else {
                break;
            };
            let (track, hostile) = (found.track.clone(), found.hostile.clone());
            let t = self.now();
            let flag = crate::ident::Flag {
                id: self.ident.next_flag(),
                t,
                kind: crate::ident::FlagKind::Proximity,
                tracks: vec![track.clone()],
                act: None,
                related: Some(hostile.clone()),
                evidence: found.evidence,
            };
            self.emit_ident_at(
                t,
                EventBody::IdentFlagged {
                    flag: flag.clone(),
                    inputs: Some(crate::ident::IdentInputs {
                        t: world.t,
                        tracks: crate::ident::escort_inputs(&tracks, &track, &hostile),
                        vehicles: std::collections::BTreeSet::new(),
                    }),
                },
            )?;
            new.push(flag.id);
        }
        Ok(new)
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
        let note = self.running_note(id);
        self.transition(id, Transition::Void, format!("system: {reason}{note}"))?;
        self.get(id)
    }

    /// Rules 5: what a vehicle may still be doing with an intent that ends or goes to a human
    /// unconfirmed: running it (its start went out and nothing confirmed it), or holding it
    /// (it was sent before its link was lost). Empty otherwise.
    fn running_note(&self, id: IntentId) -> String {
        let Some(i) = self.intents.get(&id) else {
            return String::new();
        };
        if !i.maybe_started().is_empty() {
            let who: Vec<String> = i.maybe_started().iter().map(ToString::to_string).collect();
            return format!(
                " (it may be running on vehicle(s) {}: its start went out before the link was lost, and nothing confirmed it since; look at the vehicle)",
                who.join(", ")
            );
        }
        if i.dispatched_before() && i.state() == State::Queued {
            return " (it was sent before its link was lost: the vehicle may hold it; look at the vehicle)".into();
        }
        String::new()
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
        // rules 5: queued intents whose vehicles were not reachable before their deadline
        let queued: Vec<IntentId> = self
            .intents
            .values()
            .filter(|i| {
                i.state() == State::Queued && i.queue().is_some_and(|q| now >= q.expires_t)
            })
            .map(Intent::id)
            .collect();
        for id in &queued {
            self.expire_queued(*id)?;
        }
        Ok(due.into_iter().chain(queued).collect())
    }

    /// Hand an approved (or auto-approved) vehicle intent to the task layer. A collect or
    /// cover is refused ([`EngineError::SpaceIntent`], nothing changes): it has no vehicles,
    /// so use [`Engine::dispatch_order`]. Rules 5, a kind the queue holds: only if the link
    /// reaches every vehicle on `world` ([`queue::reach`], logged with the dispatch); else
    /// refused, nothing logged ([`Engine::dispatch_or_queue`] queues it instead).
    pub fn dispatch(
        &mut self,
        id: IntentId,
        world: &WorldSnapshot,
    ) -> Result<DispatchOrder, EngineError> {
        let spec = self.get(id)?.spec().clone();
        if spec.task.is_space() {
            return Err(EngineError::SpaceIntent(id));
        }
        if spec.kind() == TaskKind::Engage {
            return Err(EngineError::EngageIntent(id));
        }
        let order = if self.reach_needed(&spec) {
            next_state(self.get(id)?.state(), Transition::Dispatch)
                .map_err(|err| EngineError::Illegal { id, err })?;
            self.refuse_queued(id)?;
            let cfg = self.policy.queue();
            let inputs = self.reach_inputs(&spec, world);
            let reach = queue::reach(&spec, &inputs, &cfg, &self.policy.rules);
            if !queue::all_reachable(&reach) {
                return Err(EngineError::NotDispatchable {
                    id,
                    why: format!(
                        "the link can't reach its vehicles now ({}): queue it (dispatch_or_queue)",
                        queue::unreachable_summary(&reach)
                    ),
                });
            }
            let logged = self.log_inputs(&inputs)?;
            self.dispatch_inner(id, Some((logged, reach)), None)?
        } else {
            self.dispatch_order(id)?
        };
        match order {
            Order::Vehicles(o) => Ok(o),
            Order::Collect(_) | Order::Cover(_) => Err(EngineError::SpaceIntent(id)),
            Order::Engage(_) => Err(EngineError::EngageIntent(id)),
        }
    }

    /// Rules 5: whether dispatching `spec` straight from its approval needs the link's reach
    /// logged with it (a kind the policy's queue holds).
    fn reach_needed(&self, spec: &IntentSpec) -> bool {
        self.policy.rules.rules_version >= 5 && self.policy.queue().queues(spec.kind())
    }

    /// A queued intent leaves the queue only through [`Engine::dequeue`] (its re-check).
    fn refuse_queued(&self, id: IntentId) -> Result<(), EngineError> {
        if self.get(id)?.state() == State::Queued {
            return Err(EngineError::NotDispatchable {
                id,
                why: "it is queued: it leaves the queue through its re-check (dequeue)".into(),
            });
        }
        Ok(())
    }

    /// Hand any approved (or auto-approved) intent on: a [`DispatchOrder`] for vehicle
    /// tasks, a [`CollectOrder`] (the chosen option, as the last check found it) or a
    /// [`CoverOrder`] for space tasks. Logged as `intent.dispatched` (with `space` for
    /// collect and cover). Rules 5: a vehicle task the queue holds goes through
    /// [`Engine::dispatch`] or [`Engine::dispatch_or_queue`] (the link's reach is logged with
    /// it), and a queued intent through [`Engine::dequeue`]: refused here.
    pub fn dispatch_order(&mut self, id: IntentId) -> Result<Order, EngineError> {
        let spec = self.get(id)?.spec().clone();
        if self.reach_needed(&spec) {
            return Err(EngineError::NotDispatchable {
                id,
                why: "under rules 5 a vehicle task is dispatched with its vehicles' reach (dispatch, dispatch_or_queue)".into(),
            });
        }
        self.refuse_queued(id)?;
        self.dispatch_inner(id, None, None)
    }

    /// The dispatch itself: `intent.dispatched` (with the reach that allowed it, when one was
    /// judged), then the transition. `rechecked`: the lost-link values owed back as a dequeue's
    /// re-check read them (rules 12: with the identity of a vehicle owed carried values).
    fn dispatch_inner(
        &mut self,
        id: IntentId,
        reached: Option<(LoggedInputs, Vec<Reach>)>,
        rechecked: Option<&BTreeMap<Uid, queue::RestoreOwed>>,
    ) -> Result<Order, EngineError> {
        let intent = self.get(id)?;
        next_state(intent.state(), Transition::Dispatch)
            .map_err(|err| EngineError::Illegal { id, err })?;
        // rules 7: whatever the path (and whether the kind is queued or not), never a mission
        // to a vehicle still owed its own lost-link values (replay checks every dispatch); rules
        // 12: the reach it was judged on says whether carried values hold its vehicle
        // rules 12: never an order to a platform spent by its own effect
        if let Some((uid, by)) = intent
            .spec()
            .uids
            .iter()
            .find_map(|u| Some((u, self.restores.expended_by(u).filter(|_| self.policy.rules.followups())?)))
        {
            return Err(EngineError::NotDispatchable {
                id,
                why: format!("vehicle {uid} was spent by its own effect (engagement {by}): a spent platform takes no order"),
            });
        }
        let judged = reached.as_ref().map(|(i, _)| &i.restore_owed).or(rechecked);
        if let Some(why) = self.restore_hold(intent.spec(), judged) {
            return Err(EngineError::NotDispatchable {
                id,
                why: format!("{why}: a mission is not sent over an engagement's lost-link action"),
            });
        }
        let intent = self.get(id)?;
        let order = order_for(
            intent.spec(),
            intent.space_answer(),
            intent.engagement().and_then(|e| e.snapshot.as_ref()),
            intent.engagement().and_then(|e| e.loiter_radius_m),
            intent.engagement().and_then(|e| e.preload.as_ref()),
            &self.policy.rules,
        )?;
        let reason = match &order {
            Order::Vehicles(_) => "dispatched to the task layer",
            Order::Collect(_) => "dispatched: satellite tasking request",
            Order::Cover(_) => "dispatched: coverage watch",
            Order::Engage(_) => "dispatched: platform sent to the effect point",
        };
        let uids = intent.spec().uids.clone();
        let mut event = dispatched_event(intent.spec(), &order);
        if let (
            EventBody::IntentDispatched {
                inputs: logged,
                reach: logged_reach,
                ..
            },
            Some((inputs, reach)),
        ) = (&mut event, reached)
        {
            *logged = Some(inputs);
            *logged_reach = Some(reach);
        }
        self.emit(event)?;
        self.transition(id, Transition::Dispatch, reason.into())?;
        self.get_mut(id)?.mark_dispatched();
        match &order {
            Order::Collect(c) => self.get_mut(id)?.set_space_order(SpaceOrder::Collect(c.clone())),
            Order::Cover(c) => self.get_mut(id)?.set_space_order(SpaceOrder::Cover(c.clone())),
            Order::Vehicles(_) | Order::Engage(_) => {}
        }
        self.last_dispatched = Some(id);
        self.step = Some((id, self.seq));
        for u in uids {
            self.newest_on.insert(u, id);
        }
        Ok(order)
    }

    /// Rules 5: dispatch an approved (or auto-approved) vehicle intent if the link can carry
    /// it to every one of its vehicles now ([`queue::reach`] on `world`), else hold it in the
    /// queue (`intent.queued`, with the vehicle facts that judgement read and its deadline,
    /// `[queue]` per kind). Space intents, engagements, kinds the policy never queues and
    /// rules before 5 are dispatched as [`Engine::dispatch_order`] does.
    pub fn dispatch_or_queue(
        &mut self,
        id: IntentId,
        world: &WorldSnapshot,
    ) -> Result<Routed, EngineError> {
        let intent = self.get(id)?;
        let state = intent.state();
        if !matches!(state, State::Approved | State::AutoApproved) {
            let err = next_state(state, Transition::Queue)
                .err()
                .unwrap_or(IllegalTransition {
                    from: state,
                    transition: Transition::Queue,
                });
            return Err(EngineError::Illegal { id, err });
        }
        let spec = intent.spec().clone();
        // rules 12: approved before its platform was spent by its own effect: voided, saying why
        // (it takes no order); rules 15: or spent in an earlier run, the same vehicle unless the
        // world shows another under the uid
        if let Some((_, why)) = self.restores.spent_hold(&spec, &self.policy.rules, &world.identities) {
            let reason = format!("spent platform: {why} and takes no order");
            self.void(id, &reason)?;
            return Ok(Routed::Voided { reason });
        }
        let cfg = self.policy.queue();
        if !self.reach_needed(&spec) {
            // rules 7: no queue for this kind, but still never a mission over an engagement's
            // lost-link action: refused, saying why. The newest order stands all the same: an
            // engagement in play on its vehicles is stopped in this step (superseded, as a
            // dispatch or a queueing would), so the platform's own values can go back
            if let Some(reason) = self.refusal(&spec) {
                self.void(id, &reason)?;
                self.step = Some((id, self.seq));
                return Ok(Routed::Voided { reason });
            }
            return Ok(Routed::Dispatched(self.dispatch_order(id)?));
        }
        let inputs = self.reach_inputs(&spec, world);
        let reach = queue::reach(&spec, &inputs, &cfg, &self.policy.rules);
        let logged = self.log_inputs(&inputs)?;
        if queue::all_reachable(&reach) {
            return Ok(Routed::Dispatched(
                self.dispatch_inner(id, Some((logged, reach)), None)?,
            ));
        }
        // one instant for the deadline and the event, so replay recomputes it from e.t
        let now = self.now();
        let basis = self
            .get(id)?
            .basis()
            .cloned()
            .unwrap_or_else(|| no_basis(now));
        let (expires_t, inbox) = cfg.bound(cfg.wait_end(spec.kind(), now), &basis, now);
        self.emit_at(
            now,
            EventBody::IntentQueued {
                intent: id,
                inputs: logged,
                reach: reach.clone(),
                expires_t,
            },
        )?;
        self.get_mut(id)?.set_queue(Some(QueueState {
            t: now,
            expires_t,
            inbox,
            reach: reach.clone(),
            recheck: None,
        }));
        let why = queue::unreachable_summary(&reach);
        self.transition(
            id,
            Transition::Queue,
            format!(
                "queued for at most {} s: {why}",
                expires_t.ms_since(now) / 1000
            ),
        )?;
        self.last_queued = Some(id);
        self.step = Some((id, self.seq));
        Ok(Routed::Queued { expires_t, reach })
    }

    /// Rules 5: try a queued intent again. Past its deadline it expires. While some vehicle
    /// still can't be reached it stays queued and nothing is logged. Otherwise its checks run
    /// on the world now (`check.result`, phase `dequeue`), the policy decides on the same
    /// inputs, and the verdict ([`queue::assess`] against what the approval rested on) is
    /// logged as `queue.recheck`: dispatch the approved order, back to the inbox (a human
    /// must decide again), or void.
    pub fn dequeue(
        &mut self,
        id: IntentId,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<Dequeued, EngineError> {
        let intent = self.get(id)?;
        if intent.state() != State::Queued {
            let err = next_state(intent.state(), Transition::Dispatch)
                .err()
                .unwrap_or(IllegalTransition {
                    from: intent.state(),
                    transition: Transition::Dispatch,
                });
            return Err(EngineError::Illegal { id, err });
        }
        let spec = intent.spec().clone();
        let deadline = intent.queue().map(|q| q.expires_t);
        if deadline.is_some_and(|d| self.now() >= d) {
            return self.expire_queued(id);
        }
        // never an older order over a newer one for the same vehicle: the newest stands
        if let Some((newer, uid)) = self.newer_on(&spec) {
            let note = self.running_note(id);
            self.transition(
                id,
                Transition::Void,
                format!(
                    "system: superseded: {newer} is a newer order for vehicle {uid} (the newest order stands){note}"
                ),
            )?;
            return Ok(Dequeued::Voided);
        }
        let cfg = self.policy.queue();
        let reach = queue::reach(&spec, &self.reach_inputs(&spec, world), &cfg, &self.policy.rules);
        if !queue::all_reachable(&reach) {
            return Ok(Dequeued::Waiting(reach));
        }
        let (inputs, _, results, passed) =
            self.run_checks(&spec, CheckPhase::Dequeue, world, terrain)?;
        // the re-check is judged before the deadline, or not at all
        let now = self.now();
        if deadline.is_some_and(|d| now >= d) {
            return self.expire_queued(id);
        }
        let decision = passed.then(|| policy::decide(&spec, &inputs, &self.policy));
        // without a basis (none was recorded), a human decides: nothing to compare with
        let intent = self.get(id)?;
        let carried = intent.basis_carried();
        let basis = intent
            .basis()
            .cloned()
            .unwrap_or_else(|| no_basis(inputs.t));
        let since = queue::Since {
            basis: &basis,
            carried,
            rebooted: &self.rebooted,
        };
        let recheck = queue::assess(&spec, &since, &results, passed, decision.as_ref(), &inputs);
        let t = self.emit_at(now, EventBody::QueueRecheck {
            intent: id,
            reach,
            decision: decision.clone(),
            verdict: recheck.verdict,
            changes: recheck.changes.clone(),
        })?;
        if let Some(q) = self.get_mut(id)?.queue_mut() {
            q.recheck = Some(RecheckRecord {
                t,
                verdict: recheck.verdict,
                changes: recheck.changes.clone(),
            });
        }
        match (recheck.verdict, decision) {
            // the order goes out at once, right after the re-check that allows it
            (Verdict::Dispatch, _) => Ok(Dequeued::Dispatched(self.dispatch_inner(
                id,
                None,
                Some(&inputs.restore_owed),
            )?)),
            (Verdict::Inbox, Some(decision)) => {
                self.back_to_inbox(
                    id,
                    Some(decision),
                    format!(
                        "back to the inbox from the queue: {}",
                        recheck.changes.join("; ")
                    ),
                )?;
                Ok(Dequeued::Inbox)
            }
            _ => {
                let note = self.running_note(id);
                self.transition(
                    id,
                    Transition::Void,
                    format!(
                        "re-check failed at dequeue: {}{note}",
                        recheck.changes.join("; ")
                    ),
                )?;
                Ok(Dequeued::Voided)
            }
        }
    }

    /// From the queue back to the inbox: a new decision is needed (`decision`, a re-check's;
    /// none: the one it has), so the old approval and its signatures are dropped, and it
    /// waits for a human under the policy's approval deadline (`approval.requested`).
    fn back_to_inbox(
        &mut self,
        id: IntentId,
        decision: Option<PolicyDecision>,
        reason: String,
    ) -> Result<(), EngineError> {
        let task = self.get(id)?.spec().task.clone();
        let reason = format!("{reason}{}", self.running_note(id));
        let intent = self.get_mut(id)?;
        if let Some(d) = decision {
            intent.set_policy(d);
        }
        intent.clear_approval();
        // the human's next approval decides, starting it again included
        intent.clear_maybe_started();
        let now = self.now();
        let (expires_t, until) =
            policy::approval_deadline(&self.policy, &task, None, &self.scenario_map(), now);
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
        Ok(())
    }

    /// A queued intent's deadline passed: `queue.expired`, then expired; or, for a human's
    /// approval held as long as `[queue] human_expire_s` allows, back to the inbox.
    fn expire_queued(&mut self, id: IntentId) -> Result<Dequeued, EngineError> {
        let intent = self.get(id)?;
        let (expires_t, since, why, inbox) = intent.queue().map_or(
            (Time(0), Time(0), String::new(), false),
            |q| (q.expires_t, q.t, queue::unreachable_summary(&q.reach), q.inbox),
        );
        let tr = if inbox {
            Transition::RequireApproval
        } else {
            Transition::Expire
        };
        next_state(intent.state(), tr).map_err(|err| EngineError::Illegal { id, err })?;
        let waited = expires_t.ms_since(since) / 1000;
        let reason = if inbox {
            // rules 7: and why it was queued (the reach `intent.queued` logged; what holds it
            // now is the queue's live view, not logged while it waits)
            let held = if self.policy.rules.rules_version >= rules::QUEUE_FOLLOWUPS_SINCE && !why.is_empty() {
                format!(" (queued because its vehicles could not be reached: {why})")
            } else {
                String::new()
            };
            format!(
                "a human's approval waits in the queue at most {} s: back to the inbox for a new decision{held}",
                self.policy.queue().human_expire_s()
            )
        } else if why.is_empty() {
            format!(
                "not dispatched within {waited} s: its vehicles could not be reached{}",
                self.running_note(id)
            )
        } else {
            format!(
                "not dispatched within {waited} s: its vehicles could not be reached ({why}){}",
                self.running_note(id)
            )
        };
        self.emit(EventBody::QueueExpired {
            intent: id,
            expires_t,
            reason: reason.clone(),
            inbox,
        })?;
        if inbox {
            self.back_to_inbox(id, None, reason)?;
            return Ok(Dequeued::Inbox);
        }
        self.transition(id, Transition::Expire, reason)?;
        Ok(Dequeued::Expired)
    }

    /// Rules 5: a dispatched intent whose link was lost to every one of its vehicles before
    /// any of them confirmed it (each vehicle's last op state `requeued`, or `unconfirmed`
    /// when its start had gone out; none ever committed) goes back to the queue
    /// (`intent.requeued`): re-checked on the world before it is sent again (then asking each
    /// vehicle what it holds first, and only following it where it may have started), until
    /// its kind's wait from the approval at the latest. When a delivery that only confirmed
    /// an order that may have started found a vehicle not running it (`unconfirmed` there),
    /// it is not started again on its own: it goes back to a human at once (`intent.requeued`
    /// with its deadline now, then `queue.expired` to the inbox). Refused, nothing logged,
    /// otherwise.
    pub fn requeue(&mut self, id: IntentId) -> Result<Time, EngineError> {
        let intent = self.get(id)?;
        let refuse = |why: &str| EngineError::NotDispatchable {
            id,
            why: why.to_string(),
        };
        next_state(intent.state(), Transition::Requeue)
            .map_err(|err| EngineError::Illegal { id, err })?;
        let spec = intent.spec().clone();
        if !self.reach_needed(&spec) {
            return Err(refuse("only a vehicle task the queue holds is requeued (rules 5)"));
        }
        if !intent.committed().is_empty() {
            return Err(refuse("a vehicle had started it: it is followed, never sent again"));
        }
        let lost: Vec<String> = spec
            .uids
            .iter()
            .filter(|u| {
                matches!(
                    intent.ops().get(*u).map(String::as_str),
                    Some(queue::REQUEUED_OP | queue::UNCONFIRMED_OP)
                )
            })
            .map(|u| u.to_string())
            .collect();
        if lost.len() != spec.uids.len() {
            return Err(refuse(
                "requeued only when every vehicle's op ended with its link lost before it was confirmed",
            ));
        }
        // a delivery that only confirmed it, to a vehicle that does not run it
        let not_running: Vec<String> = queue::confirm_failed(intent.confirming(), intent.ops())
            .iter()
            .map(|u| {
                let detail = intent.op_details().get(u).cloned().unwrap_or_default();
                format!("vehicle {u}: {detail}")
            })
            .collect();
        let cfg = self.policy.queue();
        let now = self.now();
        let basis = intent.basis().cloned().unwrap_or_else(|| no_basis(now));
        let (expires_t, inbox) = if not_running.is_empty() {
            cfg.bound(cfg.wait_end(spec.kind(), basis.t), &basis, now)
        } else {
            (now, true)
        };
        self.emit_at(
            now,
            EventBody::IntentRequeued {
                intent: id,
                expires_t,
                inbox,
            },
        )?;
        self.get_mut(id)?.set_queue(Some(QueueState {
            t: now,
            expires_t,
            inbox,
            reach: Vec::new(),
            recheck: None,
        }));
        if !not_running.is_empty() {
            self.transition(
                id,
                Transition::Requeue,
                "back to the queue, on its way to a human: it may have started, and does not run now".into(),
            )?;
            let reason = format!(
                "it may have started before its link was lost, and does not run now: not started again on its own; a human decides ({})",
                not_running.join("; ")
            );
            self.emit(EventBody::QueueExpired {
                intent: id,
                expires_t,
                reason: reason.clone(),
                inbox: true,
            })?;
            // it is not running there: the reason says so, not "may be running"
            self.get_mut(id)?.clear_maybe_started();
            self.back_to_inbox(id, None, reason)?;
            return Ok(expires_t);
        }
        let started: Vec<String> = spec
            .uids
            .iter()
            .filter(|u| intent_ops_unconfirmed(self.get(id).ok(), u))
            .map(ToString::to_string)
            .collect();
        let how = if started.is_empty() {
            "before the order started".to_string()
        } else {
            format!(
                "after its start went out to {} (it may be running: its next delivery only follows it there)",
                started.join(", ")
            )
        };
        self.transition(
            id,
            Transition::Requeue,
            format!(
                "back to the queue: the link to vehicle(s) {} was lost {how}; re-checked before it is sent again (for {} s at most)",
                lost.join(", "),
                expires_t.ms_since(now) / 1000
            ),
        )?;
        Ok(expires_t)
    }

    /// Rules 5: carry an intent the previous run left open into this one (`intent.resumed`),
    /// keeping its id. It continues queued (re-checked on the world before it goes, delivered
    /// once) unless some vehicle had already started it, when it continues dispatched: the
    /// bridge follows it there and never sends it again. Ids after the previous run's are
    /// reserved with [`Engine::reserve_ids`].
    pub fn resume(
        &mut self,
        open: &OpenIntent,
        from: ResumedFrom,
    ) -> Result<IntentId, EngineError> {
        let id = open.spec.id;
        let refuse = |why: &str| EngineError::NotDispatchable {
            id,
            why: why.to_string(),
        };
        if self.policy.rules.rules_version < 5 {
            return Err(refuse("intents are carried over from rules 5 on"));
        }
        if self.intents.contains_key(&id) {
            return Err(refuse("an intent with that id already exists in this run"));
        }
        if !queue::queueable(open.spec.kind()) {
            return Err(refuse("only vehicle tasks are carried over"));
        }
        // rules 8: only between runs that authenticated their operators alike (an approval on
        // asserted ids never stands in a credentials run, nor the reverse)
        let mut from = from;
        if self.auth.is_some() {
            let (was, now) = (
                crate::authority::effective_mode(from.auth),
                crate::authority::effective_mode(self.auth),
            );
            if was != now {
                return Err(refuse(&format!(
                    "carried from a {} run into a {} run: intents are carried only between runs that authenticate operators alike",
                    was.as_str(),
                    now.as_str()
                )));
            }
        } else {
            from.auth = None;
        }
        let state = open.resumes_as();
        let cfg = self.policy.queue();
        // the deadline it carries: its own if it was queued, else its kind's wait from the
        // approval (bounded below for a human's approval, as at any queueing)
        let expires_t = (state == State::Queued).then(|| {
            open.expires_t
                .unwrap_or_else(|| cfg.wait_end(open.spec.kind(), open.basis.t))
        });
        let t = self.emit(EventBody::IntentResumed {
            intent: open.spec.clone(),
            from: from.clone(),
            state,
            basis: open.basis.clone(),
            policy: open.policy.clone(),
            approval: open.approval.clone(),
            expires_t,
            ops: open.ops.clone(),
            committed: open.committed.clone(),
            dispatched_before: open.dispatched_before,
            platforms: open.platforms.clone(),
            maybe_started: open.maybe_started.clone(),
        })?;
        let mut intent = Intent::new(open.spec.clone());
        intent.set_checks(open.basis.checks.clone());
        if let Some(p) = &open.policy {
            intent.set_policy(p.clone());
        }
        if let Some(a) = &open.approval {
            intent.set_approval(carried_approval(a));
        }
        intent.set_basis(open.basis.clone());
        intent.set_carried(
            open.committed.iter().cloned().collect(),
            open.dispatched_before,
            true,
            open.maybe_started.iter().cloned().collect(),
        );
        intent.set_resumed(crate::intent::Resumed {
            from: from.clone(),
            ops: open.ops.clone(),
        });
        if let Some(by_kind) = expires_t {
            let (expires_t, inbox) = cfg.bound(by_kind, &open.basis, t);
            intent.set_queue(Some(QueueState {
                t,
                expires_t,
                inbox,
                reach: Vec::new(),
                recheck: None,
            }));
        }
        let reason = match state {
            State::Dispatched => format!(
                "resumed after a restart from {} ({}): started on {}; followed, never sent again",
                from.log,
                state_name(from.was),
                open.committed()
                    .iter()
                    .map(Uid::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            _ => format!(
                "resumed after a restart from {} ({}): queued until it can be re-checked and sent",
                from.log,
                state_name(from.was)
            ),
        };
        intent.resume_in(state, t, reason);
        if state == State::Dispatched {
            for u in &open.spec.uids {
                self.newest_on.insert(u.clone(), id);
            }
        }
        self.intents.insert(id, intent);
        self.next_intent = self.next_intent.max(id.0 + 1);
        Ok(id)
    }

    /// New intents get ids from `next` on (after a restart: past every id of the previous
    /// run, so an id names one intent across the carried-over logs).
    pub fn reserve_ids(&mut self, next: u64) {
        self.next_intent = self.next_intent.max(next);
    }

    /// Rules 5: a vehicle rebooted (its time since boot went back, or its boot instant
    /// jumped later than `booted_before_t` by more than [`queue::BOOT_TOLERANCE_MS`]): logged
    /// as `vehicle.rebooted` with both readings and the new boot instant. Refused, nothing
    /// logged, when it shows neither.
    pub fn record_reboot(
        &mut self,
        uid: &Uid,
        boot_ms_before: u64,
        boot_ms: u64,
        booted_t: Time,
        booted_before_t: Option<Time>,
    ) -> Result<(), EngineError> {
        if let Some(why) = queue::reboot_mismatch(boot_ms_before, boot_ms, booted_t, booted_before_t)
        {
            return Err(EngineError::NotDispatchable {
                id: IntentId(0),
                why: format!("vehicle {uid}: {why}"),
            });
        }
        let t = self.emit(EventBody::VehicleRebooted {
            uid: uid.clone(),
            boot_ms_before,
            boot_ms,
            booted_t,
            booted_before_t,
        })?;
        self.rebooted.insert(uid.clone(), t);
        Ok(())
    }

    /// Report a dispatched intent's progress on one of its own vehicles (for a collect or
    /// cover, under the resource its order names: `norad:<id>`, `satcom:<class>`,
    /// `gnss:<class>`). Refused ([`EngineError::NotItsOrder`], nothing logged) for an intent
    /// that was never dispatched, a vehicle or resource it was not dispatched to, a vehicle or
    /// space task that already ended (its outcome is final), and an engagement's `fallback`
    /// report without a stop that released that fallback: replay rejects each of these.
    pub fn record_op_state(
        &mut self,
        id: IntentId,
        uid: &Uid,
        state: &str,
        detail: &str,
    ) -> Result<(), EngineError> {
        self.op_state(id, uid, state, detail, OpReport::Bridge, None)
    }

    /// Rules 7 ([`rules::QUEUE_FOLLOWUPS_SINCE`]): a vehicle task's start, logged ahead of
    /// sending it (`op.state` starting, `ahead`): the bridge calls this before the mission
    /// mode, the hold or rtl mode, or new items to a vehicle already flying a mission go out,
    /// and sends nothing if it fails. A bridge that stops right after still finds in its log
    /// that the order may have started, so a carry-over never starts it again on its own.
    /// Refused, nothing logged, under rules before 7, for anything but a vehicle task, and
    /// where [`Engine::record_op_state`] refuses.
    pub fn record_op_state_ahead(&mut self, id: IntentId, uid: &Uid, detail: &str) -> Result<(), EngineError> {
        if self.policy.rules.rules_version < rules::QUEUE_FOLLOWUPS_SINCE {
            return Err(EngineError::NotItsOrder {
                id,
                why: "these decision rules log no start ahead of sending it".into(),
            });
        }
        self.op_state(id, uid, queue::STARTING_OP, detail, OpReport::Ahead, None)
    }

    /// An op state, from `by`; logged at `at` when given (a report whose detail was
    /// computed for that instant), else now.
    fn op_state(
        &mut self,
        id: IntentId,
        uid: &Uid,
        state: &str,
        detail: &str,
        by: OpReport,
        at: Option<Time>,
    ) -> Result<(), EngineError> {
        let ahead = by == OpReport::Ahead;
        let intent = self.get(id)?;
        // rules 11: a space task's cancel and re-check are reported by the engine itself,
        // bound to the operator's cancel or the check before them; the bridge is refused them
        if by != OpReport::Engine
            && self.policy.rules.space_followups()
            && intent.spec().task.is_space()
            && space::engine_reported(state, detail)
        {
            return Err(EngineError::NotItsOrder {
                id,
                why: format!("op state {state} on a space task is the engine's own report (cancel, recheck_space)"),
            });
        }
        let dispatched = intent.history().iter().any(|h| h.state == State::Dispatched);
        let fallback = intent
            .engagement()
            .and_then(|e| e.stop.as_ref())
            .map(|s| s.fallback);
        let replaced = self.newest_on.get(uid).is_some_and(|n| *n != id);
        let start = StartAhead {
            ahead,
            required: self.policy.rules.rules_version >= rules::QUEUE_FOLLOWUPS_SINCE,
            started: intent.started_ahead().contains(uid),
        };
        if let Some(why) = op_state_mismatch(
            intent.spec(),
            intent.state(),
            dispatched,
            replaced,
            fallback,
            uid,
            state,
            detail,
            start,
        ) {
            return Err(EngineError::NotItsOrder { id, why: why.into() });
        }
        // rules 7: what replay rejects in every log, the engine refuses too: an op whose start
        // went out on this delivery may be running, so it ends `unconfirmed`, never `requeued`
        if start.required && state == queue::REQUEUED_OP && intent.start_went_out().contains(uid) {
            return Err(EngineError::NotItsOrder {
                id,
                why: "op requeued after its start went out (it may be running: it ends unconfirmed)".into(),
            });
        }
        let at = at.unwrap_or_else(|| self.now());
        self.emit_at(
            at,
            EventBody::OpState {
                intent: id,
                uid: uid.clone(),
                state: state.into(),
                detail: detail.into(),
                ahead,
            },
        )?;
        self.get_mut(id)?
            .record_op(uid, state, detail, queue::committed_op(state), ahead);
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
        self.log_inputs_at(inputs, None)
    }

    /// [`Self::log_inputs`], its events logged at `at` when given (else at the clock).
    fn log_inputs_at(&mut self, inputs: &WorldSnapshot, at: Option<Time>) -> Result<LoggedInputs, EngineError> {
        if let SpaceInputs::Answered { sha256, answer, .. } = &inputs.space
            && !self.logged_space.contains(sha256)
        {
            let t = at.unwrap_or_else(|| self.now());
            self.emit_at(
                t,
                EventBody::SpaceQuery {
                    sha256: sha256.clone(),
                    answer: answer.clone(),
                },
            )?;
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
        let t = at.unwrap_or_else(|| self.now());
        self.emit_at(
            t,
            EventBody::AreasLoaded {
                sha256: sha256.clone(),
                areas: set.clone(),
            },
        )?;
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
        // rules 15: an engagement that spent a platform has ended (a later run need not wait out
        // its assessment window before an un-spend)
        if to.is_terminal() {
            self.restores.intent_ended(id);
        }
        self.get_mut(id)?
            .apply(transition, t, reason)
            .map_err(|err| EngineError::Illegal { id, err })
    }

    /// [`Self::transition`], returning the time the state change was logged at.
    fn transition_at(
        &mut self,
        id: IntentId,
        transition: Transition,
        reason: String,
    ) -> Result<Time, EngineError> {
        self.transition(id, transition, reason)?;
        Ok(self.last_t)
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
        self.run_checks_inner(spec, phase, world, terrain, plan, None)
    }

    /// The checks, logged at `at` when given (an instant read before, which a guard judged
    /// on: a re-check's), else at the clock.
    fn run_checks_inner(
        &mut self,
        spec: &IntentSpec,
        phase: CheckPhase,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
        plan: Option<&FrozenPlan>,
        at: Option<Time>,
    ) -> Result<(WorldSnapshot, LoggedInputs, Vec<CheckResult>, bool), EngineError> {
        let mut inputs = world.for_intent_plan(spec, terrain, &self.policy.rules, plan);
        self.identify(&mut inputs);
        if spec.task.is_space() {
            inputs.space = self.ask_space(&spec.task, inputs.t, phase)?;
        }
        self.fill_engine_facts(spec, &mut inputs);
        let results = rules::evaluate(spec, &inputs, &self.policy.rules);
        let passed = rules::passed(&results, spec.kind());
        let preview = (spec.kind() == TaskKind::Engage)
            .then(|| {
                let mut live = inputs.clone();
                live.plan = None;
                engage::preview(spec, &live, &self.policy.rules).ok()
            })
            .flatten();
        // rules 13: a check on a space answer is logged no earlier than its snapshot and its
        // answer's computation (replay holds both to it; in the bridge both are read before)
        let at = match (&inputs.space, self.policy.rules.space_replay()) {
            (SpaceInputs::Answered { computed_t, .. }, true) => {
                let t = at.unwrap_or_else(|| self.now()).max(inputs.t);
                Some(computed_t.map_or(t, |c| t.max(c)))
            }
            _ => at,
        };
        let logged = self.log_inputs_at(&inputs, at)?;
        let t = at.unwrap_or_else(|| self.now());
        self.emit_at(
            t,
            EventBody::CheckResult {
                intent: spec.id,
                phase,
                inputs: logged.clone(),
                results: results.clone(),
                passed,
                preview: preview.clone().map(Box::new),
            },
        )?;
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
    /// engine error. An answer prepared outside the lock ([`prepare_space`]) is used when
    /// nothing it depended on changed: the same task, answered by the segment (and pin) the
    /// engine holds now (one reloaded in between answers again:
    /// [`Engine::prepared_space_fits`]), and, for a collect, computed within
    /// [`PREPARED_SPACE_MAX_AGE_MS`] of `t` (its query carries the request time; a cover's
    /// does not depend on when it was asked, so its age is no concern). Without one, a
    /// collect's tasking query (milliseconds) is asked here, under whatever lock the caller
    /// holds; a cover's (seconds of SGP4 over nine points) never is: it is `Unavailable`
    /// (the check fails closed, saying the bridge prepares it outside the lock). Rules 13:
    /// the answer carries when it was computed (`computed_t`, logged), and a prepared
    /// collect's (but a re-check's, `phase` dispatched, which keeps the tasking's request
    /// time) is used only if it was asked at the engine's scenario time of that computation,
    /// as replay checks.
    fn ask_space(&mut self, task: &Task, t: Time, phase: CheckPhase) -> Result<SpaceInputs, EngineError> {
        let cover = matches!(task, Task::Cover { .. });
        let rules13 = self.policy.rules.space_replay();
        let asked_when_computed = |p: &PreparedSpace, map: ScenarioMap| {
            cover
                || phase == CheckPhase::Dispatched
                || p.inputs
                    .answer()
                    .and_then(SpaceAnswer::now)
                    .is_none_or(|now| now == map.scenario(p.t))
        };
        // computed outside the lock for this very question
        if let Some(mut p) = self.prepared_space.take()
            && p.task == *task
            && (cover || (t.ms_since(p.t)).abs() <= PREPARED_SPACE_MAX_AGE_MS)
            && self.prepared_space_fits(&p)
            && (!rules13 || asked_when_computed(&p, self.scenario_map()))
        {
            if let SpaceInputs::Answered { computed_t, .. } = &mut p.inputs {
                *computed_t = rules13.then_some(p.t);
            }
            return Ok(p.inputs);
        }
        let Some(oracle) = &self.space else {
            return Ok(SpaceInputs::Unavailable {
                reason: "no space segment loaded".into(),
            });
        };
        if cover {
            return Ok(SpaceInputs::Unavailable {
                reason: "no cover answer prepared on the segment the engine holds (a cover is computed outside the decision lock, prepare_space)".into(),
            });
        }
        space_inputs(oracle.as_ref(), task, t, &self.policy.rules)
    }

    /// Use `prepared` (computed outside the engine, see [`prepare_space`]) for the next check
    /// of the same task, if it is still fresh then; otherwise the engine asks again.
    pub fn use_prepared_space(&mut self, prepared: PreparedSpace) {
        self.prepared_space = Some(prepared);
    }

    /// Whether a prepared answer was given by the segment (and pin) the engine holds now: an
    /// answer prepared on a segment since replaced is not what the engine would answer. The
    /// bridge asks this under its lock before deciding on it and prepares again outside the
    /// lock if not.
    pub fn prepared_space_fits(&self, p: &PreparedSpace) -> bool {
        match (self.space.as_ref(), p.inputs.answer()) {
            (Some(o), Some(a)) => a.segment == *o.segment(),
            (None, Some(_)) => false,
            (_, None) => true,
        }
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
        accept: Option<Acceptor<'_>>,
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
            // rules 8: a human's intent is made in a live session of theirs
            session: if self.auth.is_some() { submitter.map(HumanAuthority::session) } else { None },
        })?;
        self.next_intent += 1;
        self.intents.insert(id, Intent::new(spec.clone()));
        // rules 6: the submitter's acceptances of lost vehicles, before the checks read them
        // (only an engagement operator's submission carries any: the others pass none)
        if let Some(a) = accept {
            for (uid, since) in a.list {
                self.emit(EventBody::EngageAccepted {
                    intent: id,
                    uid: Some(uid.clone()),
                    contact: None,
                    track: None,
                    since: *since,
                    by: a.by.clone(),
                    session: a.session,
                    world_t: world.t,
                    vehicle: Some(Box::new(world.vehicles.get(uid).cloned().unwrap_or_default())),
                    lost: None,
                    behaving: None,
                    step_up: a.step_up,
                })?;
                self.get_mut(id)?.engagement_mut().accepted.insert(
                    uid.clone(),
                    engage::AcceptedUnknown {
                        since: *since,
                        by: a.by.clone(),
                    },
                );
            }
            // rules 16: and the lost contacts it accepts
            let filled = if a.contacts.is_empty() { BTreeMap::new() } else { self.lost_filled() };
            for (track, since) in a.contacts {
                self.emit(EventBody::EngageAccepted {
                    intent: id,
                    uid: None,
                    contact: Some(track.clone()),
                    track: None,
                    since: Some(*since),
                    by: a.by.clone(),
                    session: a.session,
                    world_t: world.t,
                    vehicle: None,
                    lost: filled.get(track).cloned().map(Box::new),
                    behaving: None,
                    step_up: a.step_up,
                })?;
                self.get_mut(id)?.engagement_mut().accepted_lost.insert(
                    track.clone(),
                    engage::AcceptedUnknown {
                        since: Some(*since),
                        by: a.by.clone(),
                    },
                );
            }
            // rules 19: and the live tracks it accepts as behaving now, with their facts
            for (track, facts) in a.tracks {
                self.record_behaving(id, a.by, a.session, a.step_up, track, facts, world.t)?;
            }
        }

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
        // what an approval here rests on (auto, or the submitter's own): these checks and
        // this decision, on these inputs
        let basis = Basis {
            t: inputs.t,
            by_human: tier == Tier::Human && at_submission.is_some(),
            checks: results.clone(),
            policy: Some(decision.clone()),
            engaged: inputs.engaged.clone(),
        };
        self.get_mut(id)?.set_policy(decision);
        match (tier, at_submission) {
            (Tier::Auto, _) => {
                let t = self.transition_at(id, Transition::AutoApprove, reason)?;
                self.get_mut(id)?.set_basis(Basis { t, ..basis });
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
                    self_accepted: false,
                    at_submission: true,
                    note: None,
                    step_up: false,
                });
                let t = self.transition_at(
                    id,
                    Transition::AuthorizeAtSubmission,
                    format!("{reason}; authorized at submission by {}", by.operator()),
                )?;
                self.get_mut(id)?.set_basis(Basis { t, ..basis });
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

/// The order an approved intent is dispatched with, from what the log holds: its spec (the
/// task and vehicles that were approved), the space answer its last check read (a collect's
/// chosen option as that answer has it, a cover's service) and the approval's snapshot (an
/// engagement: the frozen plan's point, time, radius and window, the nomination's platform,
/// track and fallback). [`Engine::dispatch_order`] logs it as `intent.dispatched`; replay
/// recomputes it from the same logged facts and requires the logged order to be exactly it.
pub(crate) fn order_for(
    spec: &IntentSpec,
    answer: Option<&crate::space::SpaceAnswer>,
    snapshot: Option<&engage::EngageSnapshot>,
    loiter_radius_m: Option<f64>,
    preload: Option<&engage::LostLinkPreload>,
    rules: &crate::rules::RulesConfig,
) -> Result<Order, EngineError> {
    let id = spec.id;
    let not = |why: &str| EngineError::NotDispatchable {
        id,
        why: why.to_string(),
    };
    Ok(match &spec.task {
        Task::Collect { area, chosen, .. } => {
            let chosen = chosen.ok_or_else(|| not("no option chosen"))?;
            let answer = answer.ok_or_else(|| not("no space answer recorded"))?;
            let result = answer
                .tasking()
                .ok_or_else(|| not("the space answer is not a tasking answer"))?;
            let tol = rules.space_rules().collect_match_tolerance_s;
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
            let answer = answer.ok_or_else(|| not("no space answer recorded"))?;
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
            let snap = snapshot.ok_or_else(|| not("no approval snapshot"))?;
            let uid = spec.uids.first().cloned().ok_or_else(|| not("no platform"))?;
            Order::Engage(EngageOrder {
                intent: id,
                uid,
                track: track.clone(),
                effect_point: snap.preview.effect_point,
                effect_t: snap.preview.effect_t,
                effect_radius_m: snap.preview.effect_radius_m,
                fallback: snap.fallback,
                window_until: snap.preview.window.until,
                // rules 4 on: the loiter radius is part of the order (before, the bridge
                // chose it and nothing logged it)
                loiter_radius_m: if rules.rules_version >= 4 {
                    Some(loiter_radius_m.ok_or_else(|| not("no loiter radius from the approval"))?)
                } else {
                    None
                },
                // rules 6 on: the platform's lost-link action as the fallback
                preload: if rules.rules_version >= 6 {
                    Some(Box::new(
                        preload
                            .cloned()
                            .ok_or_else(|| not("no lost-link preload from the approval"))?,
                    ))
                } else {
                    None
                },
            })
        }
        _ => Order::Vehicles(DispatchOrder {
            intent: id,
            task: spec.task.clone(),
            uids: spec.uids.clone(),
        }),
    })
}

/// The `intent.dispatched` event for `order`: the approved task and vehicles, and the space
/// or engagement order handed on.
pub(crate) fn dispatched_event(spec: &IntentSpec, order: &Order) -> EventBody {
    EventBody::IntentDispatched {
        intent: spec.id,
        task: spec.task.clone(),
        uids: spec.uids.clone(),
        space: match order {
            Order::Vehicles(_) | Order::Engage(_) => None,
            Order::Collect(c) => Some(SpaceOrder::Collect(c.clone())),
            Order::Cover(c) => Some(SpaceOrder::Cover(c.clone())),
        },
        engage: match order {
            Order::Engage(o) => Some(o.clone()),
            _ => None,
        },
        inputs: None,
        reach: None,
    }
}

/// Why intent `by` can't be what superseded engagement `id`, if it can't: it must be another
/// intent, to vehicles that include the engagement's platform, dispatched and still under way
/// (`by_state`), and the intent dispatched last (`by_last`): the bridge stops an engagement
/// in the same step as the dispatch that takes its platform, so no other dispatch comes
/// between (and the engagement's own dispatch, if any, came before it). From rules 7
/// (`queued_ok`) an order queued for the platform supersedes it the same way, in the step
/// that queues it: still queued, and the intent queued last (`by_last` then says so).
pub(crate) fn supersede_mismatch(
    id: IntentId,
    by: IntentId,
    platform: Option<&Uid>,
    by_spec: &IntentSpec,
    by_state: State,
    by_last: bool,
    queued_ok: bool,
) -> Option<&'static str> {
    if by == id {
        return Some("an engagement can't supersede itself");
    }
    if !platform.is_some_and(|p| by_spec.uids.contains(p)) {
        return Some("superseded by an order that does not take the engagement's platform");
    }
    if queued_ok && by_state == State::Queued {
        if !by_last {
            return Some("superseded by a queued order outside the step that queued it");
        }
        return None;
    }
    // rules 7: a mission refused for its platform's own lost-link values (no queue for its
    // kind) stands over an engagement too, in the step that refused it (`by_last` is that
    // step, which only such a refusal opens)
    if queued_ok && by_state == State::Voided {
        if !by_last {
            return Some("superseded by a voided order outside the step that refused it");
        }
        return None;
    }
    if !matches!(
        by_state,
        State::Dispatched | State::Effected | State::Aborted | State::Done | State::Failed
    ) {
        return Some("superseded by an order that was not dispatched");
    }
    if by_state != State::Dispatched {
        return Some("superseded by an order no longer under way");
    }
    if !by_last {
        return Some(if queued_ok {
            "superseded by an order outside the step that dispatched it"
        } else {
            "superseded by an order other than the one dispatched last"
        });
    }
    None
}

/// Whether `uid` is one a dispatched intent reports op states under: a vehicle task's or an
/// engagement's own vehicles, a collect's satellite (`norad:<id>`), a cover's service
/// (`satcom:<class>`, `gnss:<class>`). `Err` lists the ones it does report under.
pub(crate) fn op_state_owner(spec: &IntentSpec, uid: &Uid) -> Result<(), String> {
    let own = match &spec.task {
        Task::Collect {
            chosen: Some(c), ..
        } => vec![Uid::new(format!("norad:{}", c.norad_id))],
        Task::Collect { chosen: None, .. } => Vec::new(),
        Task::Cover { service, .. } => vec![service.resource()],
        _ => spec.uids.clone(),
    };
    if own.contains(uid) {
        Ok(())
    } else {
        Err(own.iter().map(Uid::as_str).collect::<Vec<_>>().join(", "))
    }
}

/// Rules 6: who accepts lost vehicles at a nomination, and which.
/// Rules 16: what [`Engine::step_lost`] did: the claims it raised and the confirmations it
/// voided (lost contact, claimant, why).
#[derive(Clone, Debug, Default)]
pub struct LostStep {
    pub claimed: Vec<crate::lost::Claim>,
    pub reinstated: Vec<(String, String, String)>,
}

struct Acceptor<'a> {
    by: &'a OperatorId,
    session: SessionId,
    list: &'a [(Uid, Option<Time>)],
    /// Rules 16: the lost contacts accepted, each since its last fix.
    contacts: &'a [(String, Time)],
    /// Rules 19: the live tracks accepted as behaving now, each with its facts then.
    tracks: &'a [(String, TrackFacts)],
    /// Rules 8: the step-up the acceptances quote (`[approval] step_up_acts`).
    step_up: Option<StepUpRef>,
}

/// Rules 6: what the bridge reports of an engagement's lost-link preload
/// ([`Engine::record_preload`]).
#[derive(Clone, Debug, PartialEq)]
pub struct PreloadReport {
    /// The engagement's platform.
    pub uid: Uid,
    pub phase: engage::PreloadPhase,
    /// `set`: the platform's own values of every parameter the preload touches, read first.
    pub before: BTreeMap<String, f64>,
    /// What the platform reported after setting (or putting back) the values.
    pub readback: BTreeMap<String, f64>,
    /// What went wrong on the way (no answer from the vehicle, a refused write).
    pub error: Option<String>,
    /// `intent`: the vehicle the values are about to be written to.
    pub vehicle: Option<engage::VehicleIdentity>,
}

/// Rules 6: a platform's own lost-link values owed back outside an engagement's restore
/// ([`Engine::record_platform_restore`]).
#[derive(Clone, Debug, PartialEq)]
pub struct PlatformRestoreReport {
    pub uid: Uid,
    /// The platform's own values, owed back.
    pub values: BTreeMap<String, f64>,
    /// Taken over at start from an earlier log (`from`: which log and engagement); no attempt.
    pub carried: bool,
    pub from: Option<String>,
    /// An attempt: what the platform reported after putting the values back, and what went
    /// wrong on the way.
    pub readback: BTreeMap<String, f64>,
    pub error: Option<String>,
    /// `carried`: the vehicle they were preloaded on.
    pub vehicle: Option<engage::VehicleIdentity>,
}

/// Rules 7: how the reason of a mission refused for its platform's own lost-link values
/// begins (after `system: `): the refusal opens a step in which it supersedes the engagements
/// in play on its vehicles, as a dispatch or a queueing does.
pub(crate) const REFUSED: &str = "not sent: ";

/// Rules 6: how a stop for a preload that did not confirm begins (after `system: `); replay
/// requires such a stop to follow an unconfirmed `engage.preload` set.
pub(crate) const PRELOAD_FAILED: &str = "the platform's lost-link action could not be preloaded: ";

/// Rules 6: how a stop for a re-read that no longer shows the preload begins; replay requires
/// it to follow an unconfirmed `engage.preload` verify.
pub(crate) const PRELOAD_CHANGED: &str = "the platform's lost-link action changed: ";

/// Why a preload report can't be this engagement's, if it can't: never dispatched; another
/// vehicle than its platform; an order without a preload (before rules 6); a second `set`;
/// a `restore` without a `set`, while the engagement is under way, or after one confirmed.
/// The engine refuses these; replay flags them.
#[allow(clippy::too_many_arguments)]
pub(crate) fn preload_mismatch(
    spec: &IntentSpec,
    state: State,
    dispatched: bool,
    preload: Option<&engage::LostLinkPreload>,
    intent: Option<&BTreeMap<String, f64>>,
    set: Option<bool>,
    restore_confirmed: bool,
    other_holds: bool,
    phase: engage::PreloadPhase,
    uid: &Uid,
    before: &BTreeMap<String, f64>,
) -> Option<&'static str> {
    let set_logged = set.is_some();
    if !dispatched {
        return Some("preload reported for an engagement that was never dispatched");
    }
    if spec.uids.first() != Some(uid) {
        return Some("preload reported on another vehicle than the engagement's platform");
    }
    if preload.is_none() {
        return Some("preload reported for an engagement order that carries none");
    }
    match phase {
        engage::PreloadPhase::Intent if intent.is_some() || set_logged => {
            Some("a second write-ahead of the preload, or one after its set")
        }
        engage::PreloadPhase::Intent if state != State::Dispatched => {
            Some("a write-ahead of the preload for an engagement not under way")
        }
        engage::PreloadPhase::Set if set_logged => Some("a second preload set for one engagement"),
        // the values it put aside are the ones the write-ahead logged (none read: nothing written)
        engage::PreloadPhase::Set if !before.is_empty() && intent != Some(before) => {
            Some("a preload set whose own values are not its write-ahead's (or that has none)")
        }
        engage::PreloadPhase::Verify if set != Some(true) || state != State::Dispatched => {
            Some("a preload re-read without a confirmed set, or of an engagement not under way")
        }
        engage::PreloadPhase::Restore if !set_logged => Some("a restore without a preload set"),
        engage::PreloadPhase::Restore if state == State::Dispatched => {
            Some("a restore while the engagement is under way")
        }
        engage::PreloadPhase::Restore if restore_confirmed => {
            Some("a restore after the platform's values were already put back")
        }
        engage::PreloadPhase::Restore if other_holds => {
            Some("a restore while another engagement holds the platform")
        }
        _ => None,
    }
}

/// Whether a preload report confirms: no error on the way, and the read-back shows the
/// order's preload in force (`set`) or the platform's own values from the `set` back
/// (`restore`). `Err` says why not.
pub(crate) fn preload_verdict(
    phase: engage::PreloadPhase,
    preload: Option<&engage::LostLinkPreload>,
    before: Option<&BTreeMap<String, f64>>,
    own: &BTreeMap<String, f64>,
    readback: &BTreeMap<String, f64>,
    error: Option<&str>,
) -> Result<(), String> {
    if let Some(e) = error {
        return Err(e.to_string());
    }
    match phase {
        engage::PreloadPhase::Intent if own.is_empty() => Err("no values of its own were read".into()),
        engage::PreloadPhase::Intent => Ok(()),
        engage::PreloadPhase::Set | engage::PreloadPhase::Verify => preload
            .ok_or_else(|| "no preload in the order".to_string())?
            .holds(readback),
        engage::PreloadPhase::Restore => engage::restored(
            before.ok_or_else(|| "no preload set before".to_string())?,
            readback,
        ),
    }
}

/// The op states the task layer ends an op with: after a newer order took the vehicle, an
/// older one's op can only be reported in one of these (it was replaced, or had ended).
const TERMINAL_OPS: [&str; 3] = ["done", "failed", "superseded"];

/// Rules 7: what an op state report says about a vehicle start logged ahead of sending it,
/// and what its intent's delivery needs ([`op_state_mismatch`]).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct StartAhead {
    /// The report is a start logged ahead of sending it.
    pub ahead: bool,
    /// The log's rules write every vehicle start ahead (rules 7 on).
    pub required: bool,
    /// This vehicle's start (or follow) on the current delivery was logged ahead.
    pub started: bool,
}

/// Why an op state can't be this intent's, if it can't: never dispatched; not one of its
/// vehicles (or its order's resource); a vehicle or space task that already ended; on a
/// vehicle a newer order took (`replaced`), anything but how its op ended; `superseded` with
/// no newer order on the vehicle (an engagement's intercept excepted, which its own fallback
/// replaces); an engagement's `fallback` report without a stop that released a fallback, or
/// naming another fallback or task than that stop's (`"<fallback> commanded (<task>): ..."`,
/// `"<fallback> could not be commanded ..."`). `fallback` is the engagement's stop, if any:
/// `Some(None)` when it stopped without commanding one. Rules 7 (`start`): a vehicle task's
/// `starting` is logged ahead of the start (and only it is), and on every delivery a vehicle
/// reports taking the order (`acked`, `active`, `done`) only after its start was logged
/// ahead: every way to it goes through the start, or through a follow of an order the vehicle
/// already runs, which the bridge logs ahead the same way (from then on the vehicle runs it).
/// The engine refuses these; replay flags them.
#[allow(clippy::too_many_arguments)]
pub(crate) fn op_state_mismatch(
    spec: &IntentSpec,
    state: State,
    dispatched: bool,
    replaced: bool,
    fallback: Option<Option<engage::Fallback>>,
    uid: &Uid,
    op: &str,
    detail: &str,
    start: StartAhead,
) -> Option<&'static str> {
    if !dispatched {
        return Some("op state for an intent that was never dispatched");
    }
    if op_state_owner(spec, uid).is_err() {
        return Some("op state for a vehicle or resource the intent was not dispatched to");
    }
    let vehicle_task = queue::queueable(spec.kind());
    if start.ahead && (!vehicle_task || op != queue::STARTING_OP) {
        return Some("op state logged ahead that is not a vehicle task's start");
    }
    if start.required && vehicle_task {
        if op == queue::STARTING_OP && !start.ahead {
            return Some("a vehicle start not logged ahead of sending it (from rules 7 every start is)");
        }
        if queue::committed_op(op) && !start.started {
            return Some("a vehicle took an order whose start was not logged ahead of it on this delivery (rules 7)");
        }
    }
    let engagement = spec.kind() == TaskKind::Engage;
    if !engagement && state != State::Dispatched {
        // a vehicle or space task reports while it runs; its finish is final
        return Some("op state for a task that already ended");
    }
    if !spec.task.is_space() {
        // the op state proper: an engagement reports its intercept and its fallback by name
        let base = if engagement {
            op.strip_prefix("intercept ").or_else(|| op.strip_prefix("fallback "))
        } else {
            Some(op)
        };
        if let Some(base) = base {
            if replaced && !TERMINAL_OPS.contains(&base) {
                return Some("op state for an order a newer dispatch replaced on that vehicle");
            }
            let own_fallback = op == "intercept superseded" && matches!(fallback, Some(Some(_)));
            if base == "superseded" && !replaced && !own_fallback {
                return Some("op state superseded with no newer order on that vehicle");
            }
        }
    }
    if engagement && (op == "fallback" || op.starts_with("fallback ")) {
        let Some(Some(f)) = fallback else {
            return Some("fallback reported for an engagement no stop released a fallback for");
        };
        let commanded = format!("{} commanded ({}): ", f.as_str(), f.task());
        let refused = format!("{} could not be commanded", f.as_str());
        if op == "fallback" && !detail.starts_with(&commanded) && !detail.starts_with(&refused) {
            return Some("fallback reported is not the one its engagement's stop released");
        }
    }
    None
}

/// Whether `uid`'s last op on the intent ended `unconfirmed` (its start went out).
fn intent_ops_unconfirmed(intent: Option<&Intent>, uid: &Uid) -> bool {
    intent.is_some_and(|i| i.ops().get(uid).map(String::as_str) == Some(queue::UNCONFIRMED_OP))
}

/// A basis that forces a human: no checks to compare with (every result is a change), by a
/// human. Used for a queued intent with no recorded basis (never written by this engine).
pub(crate) fn no_basis(t: Time) -> Basis {
    Basis {
        t,
        by_human: true,
        checks: Vec::new(),
        policy: None,
        engaged: BTreeMap::new(),
    }
}

/// The approval a resumed intent carries, as the engine records approvals.
fn carried_approval(a: &CarriedApproval) -> Approval {
    Approval {
        by: a.by.clone(),
        session: a.session,
        t: a.t,
        self_approved: a.self_approved,
        // carried intents are vehicle tasks: no engagement, nothing accepted
        self_accepted: false,
        at_submission: a.at_submission,
        note: None,
        step_up: false,
    }
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

impl Engine {
    /// A dry run for an operator: the checks and the policy decision a vehicle task from
    /// `operator` would get right now. Nothing is created and nothing is logged: it decides
    /// nothing, as the nomination form's dry run decides nothing. Its `would_reach` is
    /// `approved` where the submission itself would be the approval (a human tier the
    /// submitter authorizes), so a caller that is not a person (a situation's runner placing
    /// opening orders under a scripted id) sees that before it submits and can send such an
    /// order to a person instead. Vehicle tasks only: collects, covers and engagements have
    /// their own dry runs (the tasking query, the nomination form's options).
    pub fn dry_run_for_operator(
        &self,
        operator: &OperatorId,
        proposal: Proposal,
        world: &WorldSnapshot,
        terrain: &dyn TerrainOracle,
    ) -> Result<Preview, EngineError> {
        proposal.validate()?;
        if proposal.task.is_space() || proposal.task.kind() == TaskKind::Engage {
            return Err(EngineError::Proposal(ProposalError::BadParams(format!(
                "an operator's dry run takes vehicle tasks only, not {}",
                proposal.task.kind().as_str()
            ))));
        }
        let created_t = self.now();
        let spec = IntentSpec {
            id: IntentId(0),
            created_t,
            source: Source::Human { id: operator.clone() },
            task: self.normalized(proposal.task, created_t),
            uids: proposal.uids,
            constraints: proposal.constraints,
            rationale: proposal.rationale,
        };
        let mut inputs = world.for_intent_with(&spec, terrain, &self.policy.rules);
        self.identify(&mut inputs);
        self.fill_engine_facts(&spec, &mut inputs);
        let checks = rules::evaluate(&spec, &inputs, &self.policy.rules);
        let passed = rules::passed(&checks, spec.kind());
        let policy = passed.then(|| policy::decide(&spec, &inputs, &self.policy));
        let would_reach = match &policy {
            None => State::Rejected,
            Some(d) if d.tier == Tier::Auto => State::AutoApproved,
            Some(d) if d.submitter_authorizes => State::Approved,
            Some(_) => State::PendingApproval,
        };
        Ok(Preview {
            kind: spec.kind(),
            uids: spec.uids,
            checks,
            passed,
            policy,
            would_reach,
            space: None,
            engage: None,
        })
    }
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
    /// `rejected`, `auto_approved` or `pending_approval`: never `approved` for an agent's
    /// proposal, because it waits for a human in a human tier. An operator's dry run
    /// ([`Engine::dry_run_for_operator`]) reaches `approved` where the submission itself
    /// would be the approval.
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
/// ```compile_fail,E0599
/// use murmur_decide::{AgentPort, IntentId, Uid, WorldSnapshot};
/// fn agent_code(port: &mut AgentPort<'_>, world: &WorldSnapshot) {
///     // rules 6: accepting a lost friendly's unknown position is an operator's decision
///     port.accept_unknown_friendly(IntentId(1), Uid::new("drone-1"), None, world);
/// }
/// ```
///
/// ```compile_fail,E0308
/// use murmur_decide::{AgentId, Engine, NoTerrain, Proposal, Uid, WorldSnapshot};
/// fn agent_code(engine: &mut Engine, me: &AgentId, p: Proposal, world: &WorldSnapshot) {
///     // nor does an agent identity stand in for the operator who accepts one at nomination
///     let accept = [(Uid::new("drone-1"), None)];
///     let _ = engine.submit_accepting(me, p, &accept, world, &NoTerrain);
/// }
/// ```
///
/// Rules 9: identification is an operator's or the engine's rule's, never an agent's. An
/// agent's port can't declare a track, confirm or revoke a declaration, resolve a review flag,
/// or report an act for the rule to judge:
///
/// ```compile_fail,E0599
/// use murmur_decide::{AgentPort, Affiliation, WorldSnapshot};
/// fn agent_code(port: &mut AgentPort<'_>, world: &WorldSnapshot) {
///     port.declare_track(None, "T-0001", Affiliation::Hostile, None, None, world);
/// }
/// ```
///
/// ```compile_fail,E0599
/// use murmur_decide::AgentPort;
/// fn agent_code(port: &mut AgentPort<'_>) {
///     port.confirm_declaration(None, "T-0001");
/// }
/// ```
///
/// ```compile_fail,E0599
/// use murmur_decide::AgentPort;
/// fn agent_code(port: &mut AgentPort<'_>) {
///     port.revoke_declaration("T-0001", None);
/// }
/// ```
///
/// ```compile_fail,E0599
/// use murmur_decide::{AgentPort, FlagResolution};
/// fn agent_code(port: &mut AgentPort<'_>) {
///     port.resolve_flag(1, FlagResolution::Dismissed, None);
/// }
/// ```
///
/// ```compile_fail,E0599
/// use murmur_decide::{ActReport, AgentPort, WorldSnapshot};
/// fn agent_code(port: &mut AgentPort<'_>, report: ActReport, world: &WorldSnapshot) {
///     port.record_act(report, world);
/// }
/// ```
///
/// ```compile_fail,E0308
/// use murmur_decide::{AgentId, Affiliation, Engine, WorldSnapshot};
/// fn agent_code(engine: &mut Engine, me: &AgentId, world: &WorldSnapshot) {
///     // an agent identity is not the operator who declares
///     let _ = engine.declare_track(me, None, "T-0001", Affiliation::Hostile, None, None, world);
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
            .submit_inner(source, None, proposal, None, world, terrain)?;
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
        self.engine.identify(&mut inputs);
        if spec.task.is_space() {
            inputs.space = self.engine.ask_space(&spec.task, inputs.t, CheckPhase::Submit)?;
        }
        // what the proposal's decision would read, lost contacts included (rules 16)
        self.engine.fill_engine_facts(&spec, &mut inputs);
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
            .submit_inner(source, None, proposal, None, world, terrain)?;
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

/// The affiliation a `track.declare` detail names (`track <id> declared <affiliation>[:
/// <note>]`, as [`Engine::record_declaration`] writes it).
pub(crate) fn declared_affiliation(detail: &str) -> Option<&str> {
    let (_, rest) = detail.split_once(" declared ")?;
    let aff = rest.split(':').next()?.trim();
    (!aff.is_empty()).then_some(aff)
}
