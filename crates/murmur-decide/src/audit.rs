//! Append-only audit log (Phase C.4): one JSON object per line,
//! `{seq, t, prev, type, ...}`.
//!
//! Decision events (`check.result`, `policy.decision`) carry the exact inputs they read,
//! the world facts with their timestamps, so [`crate::replay`] can recompute them. The
//! area set is logged once (in `run.started`, and as `areas.loaded` whenever it changes)
//! with its SHA-256 content hash; decision inputs reference the hash. Space answers
//! (collect, cover) work the same way: each is logged once as `space.query` (query, answer,
//! `time_map`, segment identity with `cache_meta`) and referenced by hash; the segment in
//! use is logged as `space.loaded` when the engine is given one. The engine writes an
//! event before applying the change it records; if the write fails, the change doesn't
//! happen.
//!
//! ## Hash chain
//!
//! Every event's `prev` is the SHA-256 (hex) of the previous event's line as written (its
//! canonical serialization: compact `serde_json` of [`Event`], without the newline); the
//! first event's `prev` is [`crate::hash::GENESIS`]. Replay recomputes the chain, so
//! deleting, inserting, reordering or editing any event that has a successor breaks it,
//! including deleting a whole intent's events and renumbering `seq`.
//!
//! This makes the log **tamper-evident, not tamper-proof**. Whoever can write the file can
//! recompute every hash after an edit and produce a consistent chain, and truncating the
//! tail (dropping the last events) leaves a valid shorter chain. The fix for both is to
//! anchor the head hash ([`crate::Engine::head_hash`], printed by `murmur-replay`) outside
//! the writer's control. [`crate::anchor`] writes run-time anchors to a separate file and
//! hands them to a command that can post them elsewhere; an anchor kept where the log's
//! writer can also write is no protection against that writer (a separate trust domain
//! is needed: another machine, a signed checkpoint, a transparency log, none of which is
//! here).

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::areas::AreaSet;
use crate::authority::{AuthMethod, Grants, Roster, StepUpRef};
use crate::engage::{
    AssessResult, EngageOrder, EngagePreview, EngageSnapshot, Fallback, ObserveStep, PictureToken,
    PreloadPhase, StopCause,
};
use crate::geo::LonLat;
use crate::ids::{AgentId, IntentId, OperatorId, SessionId, Uid};
use crate::intent::{IntentSpec, State, Task, Transition};
use crate::policy::{DeadlineBasis, Policy, PolicyDecision};
use crate::queue::{Basis, CarriedApproval, Reach, ResumedFrom, Verdict};
use crate::rules::CheckResult;
use crate::space::{ScenarioMap, SegmentId, SpaceAnswer, SpaceOrder};
use crate::time::Time;
use crate::world::LoggedInputs;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub t: Time,
    /// SHA-256 of the previous event's line ([`crate::hash::GENESIS`] for the first).
    pub prev: String,
    #[serde(flatten)]
    pub body: EventBody,
}

/// An area set and its content hash ([`AreaSet::content_hash`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoggedAreas {
    pub sha256: String,
    pub areas: AreaSet,
}

/// When a check ran: at submission, again at approval against the then-current world,
/// (rules 5) when a queued intent's vehicles are reachable again, before it may go, or
/// (rules 11) on a dispatched collect or cover against a later space answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckPhase {
    Submit,
    Approve,
    Dequeue,
    Dispatched,
}

// `run.started` (the policy, the roster, the areas and, rules 12, the carry) is the largest
// variant by far and is written once per run: boxing it would only reshape every match.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum EventBody {
    /// The engine started; the policy in force for every decision that follows, and the
    /// area set if one was given at start.
    #[serde(rename = "run.started")]
    RunStarted {
        crate_version: String,
        /// The decision rules this run decides under (`crate::RULES_VERSION`) and the policy
        /// file schema it was given (`crate::POLICY_SCHEMA`). Absent in logs written before
        /// they existed: version 1 of both. Replay recomputes with these rules.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rules_version: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        policy_schema: Option<u32>,
        /// The build that wrote the log: its commit, and whether the working tree differed
        /// from it. Absent in logs written before it existed (rules 1 and 2).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        build: Option<BuildInfo>,
        /// Who may act as an operator, and with engagement authority (rules 4 on; absent
        /// before). Replay checks every session against it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        roster: Option<Roster>,
        policy: Policy,
        areas: Option<LoggedAreas>,
        /// Rules 12 ([`crate::rules::REVIEW_FOLLOWUPS_SINCE`]): what the bridge carried over at
        /// start from earlier runs' logs, in one line (a crash can't leave half of it): the log
        /// its `.last-run-<port>` pointer named, then any log whose carry failed before and that
        /// nobody wrote off, each with its head and last seq as read and what it still owed
        /// (the platforms' own lost-link values, and the logs it had not carried), or why it
        /// could not be read. The platforms' values seed this run's ledger. Absent when no
        /// pointer named a log (a first start), in a scenario and before rules 12.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        carried_from: Vec<crate::replay::CarriedFrom>,
        /// Rules 15 ([`crate::rules::CARRY_FOLLOWUPS_SINCE`]): the `.last-run` pointer of the
        /// station this run is (its file name, `.last-run-<http port>`): the pointer it read at
        /// start and rewrites to name this log. A later start of the station whose pointer can't
        /// be read falls back to the newest log naming it whose chain and anchors check. Absent
        /// without a pointer (a scenario, a test) and before rules 15.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pointer: Option<String>,
    },

    /// A new area set is in force (the first one, if none was given at start, or a
    /// change). Decision inputs after this reference it by hash.
    #[serde(rename = "areas.loaded")]
    AreasLoaded { sha256: String, areas: AreaSet },

    /// A space segment is in use from here on (`Engine::set_space`): its identity, how
    /// scenario time follows engine time (absent: the same), and the time shift pinned on
    /// the segment for the session (absent: none, the shift is chosen per window).
    #[serde(rename = "space.loaded")]
    SpaceLoaded {
        segment: SegmentId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scenario: Option<ScenarioMap>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pinned_shift_days: Option<i64>,
    },

    /// A space query and its answer (collect, cover), logged once before the first
    /// decision that reads it; decision inputs reference it by `sha256`, the SHA-256 of
    /// the answer's canonical serialization.
    #[serde(rename = "space.query")]
    SpaceQuery {
        sha256: String,
        answer: Box<SpaceAnswer>,
    },

    #[serde(rename = "intent.created")]
    IntentCreated {
        intent: IntentSpec,
        /// Rules 8: a human's intent is made in a live session of theirs (absent for an
        /// agent's, and before rules 8); replay checks it is open and the proposer's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<SessionId>,
    },

    /// Rules 8, a credentials run: a login for an operator on the roster was refused (a wrong
    /// password; `locked_s`: the id is now locked that long). Never the password. A refused
    /// step-up is `operator.action step_up.refused` under its session instead.
    #[serde(rename = "auth.refused")]
    AuthRefused {
        operator: OperatorId,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        locked_s: Option<u64>,
    },

    #[serde(rename = "check.result")]
    CheckResult {
        intent: IntentId,
        phase: CheckPhase,
        inputs: LoggedInputs,
        results: Vec<CheckResult>,
        passed: bool,
        /// Engage: the preview computed from the same inputs (absent when there is none).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preview: Option<Box<EngagePreview>>,
    },

    #[serde(rename = "policy.decision")]
    PolicyDecision {
        intent: IntentId,
        inputs: LoggedInputs,
        decision: PolicyDecision,
    },

    /// Every state change of every intent.
    #[serde(rename = "intent.state")]
    IntentState {
        intent: IntentId,
        transition: Transition,
        from: State,
        to: State,
        reason: String,
    },

    /// A human must decide by `expires_t` (engine time). `until` says when the deadline
    /// is not the plain `expire_s` (a collect's tasking lead time, in scenario time).
    #[serde(rename = "approval.requested")]
    ApprovalRequested {
        intent: IntentId,
        expires_t: Time,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<DeadlineBasis>,
    },

    /// The approval that moves an intent to `approved` (the last signature when the tier
    /// needs more than one).
    #[serde(rename = "approval.granted")]
    ApprovalGranted {
        intent: IntentId,
        by: OperatorId,
        session: SessionId,
        /// The approver is the human who proposed the intent. Allowed where the tier lets
        /// the proposer approve, and recorded so evaluation can measure it; never for an
        /// engagement.
        self_approved: bool,
        note: Option<String>,
        /// Engage: the picture this approval holds (recomputed by replay from the re-check
        /// just before it); its preview is the frozen plan.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapshot: Option<Box<EngageSnapshot>>,
        /// Engage: the picture token the signature quoted (what the signer was shown, as
        /// issued in `picture.issued`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<Box<PictureToken>>,
        /// The engagements in play on this intent's vehicles that its dispatch will stop
        /// (superseded): the approver acknowledged them. Absent when none.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        supersedes: Vec<IntentId>,
        /// Engage: the instant the signature was checked at.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checked_t: Option<Time>,
        /// Engage, rules 6: the signer also accepted one of its lost vehicles (allowed,
        /// recorded like `self_approved`; replay recomputes it).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        self_accepted: bool,
        /// Rules 8: the step-up this signature quoted (the signer's credential re-entered at
        /// signing: the `operator.action step_up` event under the same session). Required in
        /// a credentials run when the decision says `step_up`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_up: Option<StepUpRef>,
    },

    /// A signature that is not yet enough: signature `n` of the `of` distinct approvers the
    /// tier needs (two-person approval). The intent stays `pending_approval`.
    #[serde(rename = "approval.concurred")]
    ApprovalConcurred {
        intent: IntentId,
        by: OperatorId,
        session: SessionId,
        n: u32,
        of: u32,
        note: Option<String>,
        /// Engage: the picture this signature is on, its preview the frozen plan (every
        /// signature is bound to one; replay recomputes it from the re-check just before).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapshot: Option<Box<EngageSnapshot>>,
        /// Engage: the picture token the signature quoted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<Box<PictureToken>>,
        /// As on `approval.granted`: the engagements this approval acknowledged stopping.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        supersedes: Vec<IntentId>,
        /// Engage: the instant the signature was checked at (the token's expiry and the
        /// plan's freeze time are read against it).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checked_t: Option<Time>,
        /// Engage, rules 6: as on `approval.granted`.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        self_accepted: bool,
        /// Rules 8: as on `approval.granted`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_up: Option<StepUpRef>,
    },

    /// Engage: the engine showed a signer the picture (`token`: the plan, the track's
    /// identity, valid until `expires_t`, bound to the signer's session), on the world in
    /// `inputs`. Replay recomputes the plan from them when none was frozen yet.
    #[serde(rename = "picture.issued")]
    PictureIssued {
        token: Box<PictureToken>,
        inputs: LoggedInputs,
    },

    /// Engage: the frozen plan no longer holds (the track left its prediction, was
    /// reclassified or its declaration changed, a keep-out or the platform's reach failed on
    /// it, or the signatures took longer than `max_signing_s`), so every signature so far
    /// (`dropped`) is dropped with the plan and a new plan must be signed. At a signature the
    /// world is that signature's re-check (`inputs` absent); while pending it is the world
    /// then (`inputs` present). Replay recomputes the failed checks.
    #[serde(rename = "approval.reset")]
    ApprovalReset {
        intent: IntentId,
        dropped: Vec<OperatorId>,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inputs: Option<LoggedInputs>,
    },

    /// A human submitted an intent whose policy tier needs a human and lets the submitter
    /// authorize (`submitter_authorizes`): the submission is the human decision. Distinct
    /// from `approval.granted` (an inbox approval) and from `auto_approved` (no human), so
    /// evaluation can tell the three apart. Never for an agent-sourced intent.
    #[serde(rename = "approval.at_submission")]
    ApprovalAtSubmission {
        intent: IntentId,
        by: OperatorId,
        session: SessionId,
        /// The approver is the submitter: always true, recorded like `approval.granted`'s.
        self_approved: bool,
        /// The policy tier that allowed it (`policy.decision`'s `rule`).
        rule: String,
    },

    #[serde(rename = "approval.rejected")]
    ApprovalRejected {
        intent: IntentId,
        by: OperatorId,
        session: SessionId,
        note: Option<String>,
    },

    #[serde(rename = "approval.expired")]
    ApprovalExpired { intent: IntentId, expires_t: Time },

    #[serde(rename = "intent.dispatched")]
    IntentDispatched {
        intent: IntentId,
        task: Task,
        uids: Vec<Uid>,
        /// Collect and cover: the order handed to the bridge (absent for vehicle tasks).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space: Option<SpaceOrder>,
        /// Engage: where the platform is sent (the approval snapshot's effect point).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        engage: Option<EngageOrder>,
        /// Rules 5, a kind the queue holds, dispatched straight from its approval: the
        /// vehicle facts that showed every vehicle reachable (`reach`, recomputed by replay).
        /// Absent when it comes out of the queue (its `queue.recheck` holds them).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inputs: Option<LoggedInputs>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reach: Option<Vec<Reach>>,
    },

    /// Rules 5: an approved intent held for vehicles that can't be reached now (`reach`,
    /// recomputed by replay from `inputs`), until `expires_t` at the latest.
    #[serde(rename = "intent.queued")]
    IntentQueued {
        intent: IntentId,
        inputs: LoggedInputs,
        reach: Vec<Reach>,
        expires_t: Time,
    },

    /// Rules 5: a queued intent's vehicles are reachable again and its checks ran on the
    /// world now (the `check.result`, phase `dequeue`, just before): the policy decision on
    /// those inputs (absent when a check failed) and what follows (`verdict`: dispatch, back
    /// to the inbox, void), with what changed since the approval. Replay recomputes all of it.
    #[serde(rename = "queue.recheck")]
    QueueRecheck {
        intent: IntentId,
        reach: Vec<Reach>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision: Option<PolicyDecision>,
        verdict: Verdict,
        changes: Vec<String>,
    },

    /// Rules 5: a queued intent's deadline passed before its vehicles could be reached. It
    /// expires, or (`inbox`: a human's approval held past `[queue] human_expire_s`) goes back
    /// to the inbox for a new decision (`approval.requested` follows).
    #[serde(rename = "queue.expired")]
    QueueExpired {
        intent: IntentId,
        expires_t: Time,
        reason: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        inbox: bool,
    },

    /// Rules 5: a dispatched intent whose link was lost to every vehicle before any started
    /// it (each vehicle's last `op.state` is `requeued`, none ever started it) goes back to
    /// the queue until `expires_t` (`inbox` as in `queue.expired`), re-checked before it is
    /// sent again.
    #[serde(rename = "intent.requeued")]
    IntentRequeued {
        intent: IntentId,
        expires_t: Time,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        inbox: bool,
    },

    /// Rules 5: an intent the previous run left open (queued, approved, or dispatched and
    /// still running), carried into this one after a bridge restart: its spec and id, where
    /// it comes from (that log, its head and last seq), the state it continues in here
    /// (`queued`: re-checked before it goes; `dispatched`: some vehicle had started it, so it
    /// is followed, never sent again), what its approval rested on, and per vehicle its last
    /// op state there. Replay checks it against the previous log when given it.
    #[serde(rename = "intent.resumed")]
    IntentResumed {
        intent: IntentSpec,
        from: ResumedFrom,
        state: State,
        basis: Basis,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        policy: Option<PolicyDecision>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        approval: Option<CarriedApproval>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_t: Option<Time>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        ops: BTreeMap<Uid, String>,
        /// The vehicles that ever started it, across every run it went through.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        committed: Vec<Uid>,
        /// It was dispatched in some run before: a vehicle may hold it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        dispatched_before: bool,
        /// Each vehicle's platform as the previous run last checked it.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        platforms: BTreeMap<Uid, String>,
        /// The vehicles that may have started it (its start went out, nothing confirmed it).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        maybe_started: Vec<Uid>,
    },

    /// Rules 5: a vehicle rebooted, and lost the mode and armed state (and, on a stand-in,
    /// the mission) its orders had set: its time since boot went back, or (an outage longer
    /// than its uptime before) the boot instant it implies jumped later than the one before
    /// (`booted_before_t`). `booted_t` is the new boot instant the bridge reports from here on.
    #[serde(rename = "vehicle.rebooted")]
    VehicleRebooted {
        uid: Uid,
        boot_ms_before: u64,
        boot_ms: u64,
        booted_t: Time,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        booted_before_t: Option<Time>,
    },

    /// Per-vehicle task progress from the task layer (sent, acked, active, done, failed).
    /// Rules 7: a vehicle task's `starting` is logged ahead of the start it records (`ahead`):
    /// written before the mission mode, the hold or rtl mode, or new items to a vehicle
    /// already flying a mission go out, so a bridge that stops right after still finds that
    /// the order may have started.
    #[serde(rename = "op.state")]
    OpState {
        intent: IntentId,
        uid: Uid,
        state: String,
        detail: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        ahead: bool,
    },

    /// Mode change, failsafe, STATUSTEXT and the like.
    #[serde(rename = "vehicle.event")]
    VehicleEvent {
        uid: Uid,
        kind: String,
        text: String,
    },

    #[serde(rename = "operator.action")]
    OperatorAction {
        operator: OperatorId,
        session: SessionId,
        action: String,
        detail: String,
        /// `session.opened`: what the session may authorize beyond tasking (absent: nothing).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grants: Option<Grants>,
        /// Rules 8, on `session.opened` and `step_up`: how the operator proved who they are
        /// (`password`: checked by the bridge; `asserted`: a name the request gave, dev mode).
        /// `session.closed` (detail: why) ends the session; nothing may follow under it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth: Option<AuthMethod>,
        /// Rules 8: the step-up this act quoted (a declaration the policy marks consequential).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_up: Option<StepUpRef>,
        /// Rules 10, on `session.opened` and `step_up` with `auth: passkey`: the assertion,
        /// what it was for, the nonce and issue time its challenge was derived from, the
        /// credential and its counter (nothing secret). Replay recomputes the challenge and
        /// verifies the signature against the run's logged public key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        passkey: Option<Box<crate::passkey::PasskeyUse>>,
    },

    /// Rules 10, a credentials run: a passkey was registered for `key.operator` while the run
    /// was up (its public key, never private material): through a signed-in session, which
    /// re-entered the operator's credential first (`step_up`, under `session`), or through a
    /// one-time invitation `murmur-auth passkey-invite` issued (`via: invitation`, no
    /// session). Usable from here on.
    #[serde(rename = "passkey.registered")]
    PasskeyRegistered {
        key: crate::passkey::PasskeyKey,
        via: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<SessionId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_up: Option<StepUpRef>,
    },

    /// Rules 10: a passkey was removed from the credential file (`murmur-auth
    /// passkey-remove`) while the run was up; nothing may use it from here on.
    #[serde(rename = "passkey.removed")]
    PasskeyRemoved {
        operator: OperatorId,
        credential: String,
        reason: String,
    },

    #[serde(rename = "agent.action")]
    AgentAction {
        agent: AgentId,
        action: String,
        detail: String,
    },

    /// Engage, after approval: hold the picture. The checks against the approval's
    /// snapshot on these inputs, whether the picture holds, and (dispatched) whether the
    /// effect may be released now. A failed picture is followed by `engage.stopped`.
    #[serde(rename = "engage.picture")]
    EngagePicture {
        intent: IntentId,
        inputs: LoggedInputs,
        results: Vec<CheckResult>,
        holds: bool,
        ready: bool,
    },

    /// Engage: the abstract effect, at the approval's predicted point (released right after
    /// an `engage.picture` that holds and is ready). Not a weapon: a point, a radius, a time.
    #[serde(rename = "engage.effect")]
    EngageEffect {
        intent: IntentId,
        uid: Uid,
        point: LonLat,
        radius_m: f64,
        planned_t: Time,
    },

    /// Engage: stopped before the effect (before dispatch it is voided, after it aborted),
    /// and the fallback commanded.
    #[serde(rename = "engage.stopped")]
    EngageStopped {
        intent: IntentId,
        cause: StopCause,
        reason: String,
        /// The fallback commanded; `null` when none was (stopped before dispatch, or
        /// superseded by another order that stands).
        #[serde(default)]
        fallback: Option<Fallback>,
    },

    /// Engage, rules 6: an operator with engagement authority accepted that one of our
    /// vehicles is lost (its position unknown since its report at `since`; null: it never
    /// reported one) for this engagement, so its friendly keep-out leaves that vehicle out
    /// while it is lost since exactly then. At the nomination or while it waits with no
    /// signature; never automatic, never an agent's (replay checks the session is the
    /// operator's, with grants.engage). Every signer's picture token carries the acceptances;
    /// a reset of the signatures clears them.
    ///
    /// Rules 16: or a lost contact (`contact`, the track id it was dropped under, instead of
    /// `uid`; `since` its last fix; `lost` the contact as the engine's ledger held it, instead
    /// of `vehicle`): its keep-out on tracks leaves that contact out while it is lost since
    /// exactly then. Everything else as for a vehicle.
    #[serde(rename = "engage.accepted")]
    EngageAccepted {
        intent: IntentId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uid: Option<Uid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contact: Option<String>,
        #[serde(default)]
        since: Option<Time>,
        by: OperatorId,
        session: SessionId,
        /// The world it was accepted on: the vehicle's facts then and the world's time (replay
        /// checks it was lost since exactly `since`, by the link-aware freshness rule).
        world_t: Time,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vehicle: Option<Box<crate::world::VehicleFacts>>,
        /// Rules 16: the lost contact as the engine held it (replay checks it against its own
        /// ledger: in force, lost since exactly `since`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lost: Option<Box<crate::lost::LostContact>>,
        /// Rules 8: the step-up the acceptance quoted (`[approval] step_up_acts` lists
        /// `accept_unknown`); every acceptance of one nomination may quote the same one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_up: Option<StepUpRef>,
    },

    /// Engage, rules 6: an operator withdrew an acceptance of a lost vehicle for this
    /// engagement (any operator may: it only restricts). Its checks count the vehicle again.
    /// Rules 16: or of a lost contact (`contact` instead of `uid`).
    #[serde(rename = "engage.acceptance_revoked")]
    EngageAcceptanceRevoked {
        intent: IntentId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uid: Option<Uid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contact: Option<String>,
        by: OperatorId,
        session: SessionId,
    },

    /// Rules 16 ([`crate::lost`]): the bridge's tracker dropped `track` (no detection for its
    /// drop time); its facts then, as the tracker handed them (the estimate, never a
    /// declaration). From here on the engine holds it as a lost contact, which every
    /// engagement's keep-out on tracks counts anywhere it could have reached since its last
    /// fix, until an operator confirms a track as it again (`track.reacquired`). Replay rebuilds
    /// the ledger from these and checks every decision's `lost_contacts` against it, that no
    /// track left a decision's picture without one, that no id is dropped twice and that a drop
    /// carries no older fix than the decisions had seen.
    #[serde(rename = "track.dropped")]
    TrackDropped {
        track: String,
        facts: Box<crate::world::TrackFacts>,
        /// The AIS identity its track carried then, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ais: Option<crate::world::AisIdentity>,
    },

    /// Rules 16: live track `by_track` carries lost contact `track`'s AIS identity: a claim to be
    /// it, raised once for review ([`crate::lost::claims`]), on the picture logged here
    /// (`tracks`, its identification filled, and `track_ais`, at `world_t`), with the evidence
    /// and what keeps it from being confirmed now (`blockers`). It changes nothing: the lost
    /// contact counts until an operator confirms the claim. Replay recomputes it.
    #[serde(rename = "lost.claimed")]
    LostClaimed {
        track: String,
        by_track: String,
        world_t: Time,
        tracks: BTreeMap<String, crate::world::TrackFacts>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        track_ais: BTreeMap<String, crate::world::AisIdentity>,
        evidence: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        blockers: Vec<String>,
    },

    /// Rules 16: an operator (`by`, under `session`; never an agent) confirmed that lost contact
    /// `track` is live track `by_track` again, on the picture logged here (`tracks` at `world_t`,
    /// its identification filled, and `track_ais`), with the evidence shown
    /// ([`crate::lost::claim_of`]: nothing may contradict it): it is retired from the keep-outs
    /// while `by_track` stands, the live track counting in its place. Replay recomputes the claim
    /// from this picture and its own ledger, and checks the session.
    #[serde(rename = "track.reacquired")]
    TrackReacquired {
        track: String,
        by_track: String,
        by: OperatorId,
        session: SessionId,
        world_t: Time,
        tracks: BTreeMap<String, crate::world::TrackFacts>,
        /// The AIS identities heard for that picture's tracks.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        track_ais: BTreeMap<String, crate::world::AisIdentity>,
        evidence: String,
    },

    /// Rules 16: the confirmation of lost contact `track` as `by_track` is void
    /// ([`crate::lost::void_reason`]: the claimant left the picture, turned suspect or hostile,
    /// or was declared), seen at `world_t` with the claimant's facts then (none when it left
    /// the picture): the lost contact counts again, for good. Every decision on a picture that
    /// voids it already counted it; replay checks both.
    #[serde(rename = "lost.reinstated")]
    LostReinstated {
        track: String,
        by_track: String,
        reason: String,
        world_t: Time,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        facts: Option<Box<crate::world::TrackFacts>>,
    },

    /// Rules 6: a platform's own lost-link values owed back outside an engagement's own
    /// restore: carried over at start from an earlier log whose preload was never confirmed
    /// back (`carried`, `from`: that log and engagement), then each attempt to put them back
    /// (`readback`, `error`, `confirmed`: replay recomputes it against the values owed); also
    /// an engagement's restore the engine refused. Replay reports a platform whose values were
    /// still owed when the log ended.
    #[serde(rename = "platform.restore")]
    PlatformRestore {
        uid: Uid,
        values: BTreeMap<String, f64>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        carried: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        readback: BTreeMap<String, f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        confirmed: bool,
        /// `carried`: the vehicle the values were preloaded on (from that log's write-ahead):
        /// they go back only to a vehicle that matches.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vehicle: Option<Box<crate::engage::VehicleIdentity>>,
    },

    /// Rules 12: a debt of the ledger closed without a restore: a platform's own lost-link
    /// values (`uid`, `values`: exactly what was owed) or an earlier log whose carry failed
    /// (`log`: what it owed is unknown), closed because the platform was expended by its own
    /// effect (`expended`: right after the release, its catalog's `expended_by_effect`), or
    /// because an operator wrote it off under their own session, saying why (`written_off`).
    /// Nothing is owed back for it from then on, in this run or a later one.
    #[serde(rename = "restore.closed")]
    RestoreClosed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uid: Option<Uid>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        values: BTreeMap<String, f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        log: Option<String>,
        closed_by: RestoreClosure,
    },

    /// Rules 15 ([`crate::rules::CARRY_FOLLOWUPS_SINCE`], murmur-imu): an operator with
    /// engagement authority un-spends a platform spent by its own effect (`spent_by`, the
    /// engagement that released it; `from`, the earlier log it is in when the mark was carried
    /// over), under their own session, saying why (they believe it survived: a miss), once that
    /// engagement ended, the platform was heard after its end and is not silent now (`vehicle`:
    /// its facts as the world read them at `world_t`), and the vehicle under the uid now is the
    /// one that was spent (`identity`: the bridge's identity of it then; platform, callsign and
    /// system id, never the home). It takes orders and nominations again, and its own lost-link
    /// values (`values`, what the spent mark closed) are owed back again from here on: the
    /// engagement's fallback is still its lost-link action, so a mission waits for the restore.
    /// In a credentials run it quotes a step-up made for it (`step_up`; a passkey's challenge
    /// binds the platform, the engagement and the log of a carried mark).
    #[serde(rename = "platform.unspent")]
    PlatformUnspent {
        uid: Uid,
        spent_by: IntentId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        values: BTreeMap<String, f64>,
        operator: OperatorId,
        session: SessionId,
        note: String,
        world_t: Time,
        vehicle: Box<crate::world::VehicleFacts>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity: Option<crate::engage::VehicleIdentity>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_up: Option<StepUpRef>,
    },

    /// Engage, rules 6: the platform's own lost-link values read and logged before anything is
    /// written (`intent`, the write-ahead: owed back from here on, with the vehicle's
    /// identity), the platform's own lost-link action set as the engagement's fallback
    /// (`set`: its `before` is the intent's, and the order's preload read back), read again
    /// near the release (`verify`),
    /// or its own values put back once the engagement ended (`restore`). `error` is what the
    /// bridge met (no answer); `confirmed` that the read-back shows it in force (replay
    /// recomputes it). A release needs a confirmed `set` and a confirmed read-back at most
    /// 10 s old; an unconfirmed `set` or `verify` stops the engagement with its fallback.
    #[serde(rename = "engage.preload")]
    EngagePreload {
        intent: IntentId,
        uid: Uid,
        phase: PreloadPhase,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        before: BTreeMap<String, f64>,
        readback: BTreeMap<String, f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        confirmed: bool,
        /// `intent`: the vehicle it is about to be written to (the bridge's report).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vehicle: Option<Box<crate::engage::VehicleIdentity>>,
    },

    /// Engage, after the effect: one observation step of the effect point.
    #[serde(rename = "engage.observe")]
    EngageObserve {
        intent: IntentId,
        inputs: LoggedInputs,
        step: ObserveStep,
        covered_s: f64,
    },

    /// Engage: the assessment that finishes the intent.
    #[serde(rename = "engage.assessed")]
    EngageAssessed {
        intent: IntentId,
        result: AssessResult,
        evidence: String,
        covered_s: f64,
    },

    /// Rules 9: an abstract hostile act our sensors observed (the bridge's report: whom it was
    /// against, when, which of our sensors saw it, where its author appeared to be), the track
    /// picture it was attributed on (`inputs`, its identification as the ledger filled it), the
    /// attribution, and the hostile-act rule's verdict: a rule declaration (`ident.declared`)
    /// or a flag (`ident.flagged`) follows at once, else it is only recorded. Replay recomputes
    /// the attribution and the verdict.
    #[serde(rename = "ident.act")]
    IdentAct {
        act: crate::ident::ActReport,
        inputs: crate::ident::IdentInputs,
        candidates: Vec<crate::ident::Candidate>,
        verdict: crate::ident::ActVerdict,
    },

    /// Rules 9: a track's affiliation declared, by an operator (under their session) or by
    /// the hostile-act rule (right after the act whose verdict it carries out, with its
    /// evidence). In force until revoked; a declaration in force is never replaced in place.
    #[serde(rename = "ident.declared")]
    IdentDeclared {
        declaration: Box<crate::ident::Declaration>,
    },

    /// Rules 9: an operator confirmed a rule declaration (any operator may; recorded).
    #[serde(rename = "ident.confirmed")]
    IdentConfirmed {
        track: String,
        declaration: u64,
        by: OperatorId,
        session: SessionId,
        /// The step-up it quoted (`[approval] step_up_acts` lists `confirm_declaration`, a
        /// credentials run; off by default).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_up: Option<crate::authority::StepUpRef>,
    },

    /// Rules 9: an operator revoked the declaration in force on a track (any operator may: it
    /// only restricts). Every engagement in play on the track (`engagements`) is stopped next,
    /// cause `revoked`, before anything else is logged.
    #[serde(rename = "ident.revoked")]
    IdentRevoked {
        track: String,
        declaration: u64,
        by: OperatorId,
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        engagements: Vec<IntentId>,
    },

    /// Rules 9: a review flag, what the engine is unsure of (an act attributed below the
    /// threshold, conflicting evidence, no track, a victim we can't call ours or protected:
    /// right after its `ident.act`; or a track keeping company with a declared hostile, with
    /// the two tracks' facts in `inputs`). Never a declaration.
    #[serde(rename = "ident.flagged")]
    IdentFlagged {
        flag: crate::ident::Flag,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inputs: Option<crate::ident::IdentInputs>,
    },

    /// Rules 9: an operator confirmed or dismissed a review flag.
    #[serde(rename = "ident.flag_resolved")]
    IdentFlagResolved {
        flag: u64,
        resolution: crate::ident::FlagResolution,
        by: OperatorId,
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },

    /// What an operator's console showed for a pending intent (Phase G): when the card was
    /// rendered (`open`), removed (`close`) and when approve or reject was clicked
    /// (`decide`), with what was visible at the time. Additive: replay verifies the chain
    /// through it and makes no decision from it. The evaluation joins it with the approval
    /// events to relate decision latency to the information shown.
    #[serde(rename = "operator.view")]
    OperatorView {
        operator: OperatorId,
        session: SessionId,
        intent: IntentId,
        view: OperatorView,
    },
}

/// One console display event for a pending intent (see [`EventBody::OperatorView`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OperatorView {
    /// `open` (the card was rendered), `close` (it went away undecided), `decide` (approve
    /// or reject was clicked; `action` says which).
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// What the card showed at that moment.
    pub shown: ViewShown,
    /// How long the card had been open, milliseconds (absent for `open`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_ms: Option<u64>,
    /// The client's own clock, milliseconds since the epoch, when it happened (the event's
    /// `t` is the bridge's clock when it was logged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_t: Option<i64>,
}

impl OperatorView {
    pub const EVENTS: [&'static str; 3] = ["open", "close", "decide"];
    pub const ACTIONS: [&'static str; 2] = ["approve", "reject"];
    /// Bounds on what a view may carry: it is instrumentation, not a place to store data.
    pub const MAX_ITEMS: usize = 64;
    pub const MAX_TEXT: usize = 256;

    /// A well-formed view: a known event, an action only on `decide` and then a known one,
    /// at most [`Self::MAX_ITEMS`] checks, warnings and tracks, each at most
    /// [`Self::MAX_TEXT`] bytes.
    pub fn validate(&self) -> Result<(), String> {
        if !Self::EVENTS.contains(&self.event.as_str()) {
            return Err(format!("event must be one of {}", Self::EVENTS.join(", ")));
        }
        for (name, items) in [
            ("checks", &self.shown.checks),
            ("warnings", &self.shown.warnings),
            ("tracks", &self.shown.tracks),
        ] {
            if items.len() > Self::MAX_ITEMS {
                return Err(format!("{name}: at most {} entries", Self::MAX_ITEMS));
            }
            if items.iter().any(|t| t.len() > Self::MAX_TEXT) {
                return Err(format!("{name}: entries of at most {} bytes", Self::MAX_TEXT));
            }
        }
        match (self.event.as_str(), self.action.as_deref()) {
            ("decide", Some(a)) if Self::ACTIONS.contains(&a) => Ok(()),
            ("decide", Some(a)) => Err(format!(
                "action {a:?}: must be one of {}",
                Self::ACTIONS.join(", ")
            )),
            ("decide", None) => Err("a decide event needs an action".into()),
            (_, Some(_)) => Err("only a decide event carries an action".into()),
            (_, None) => Ok(()),
        }
    }
}

/// What was visible on an intent's card: the rule ids of the checks shown, the warning
/// lines ("why this needs a human"), whether the rationale was shown, and the tracks the
/// operator opened while the card was up (by track id).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ViewShown {
    #[serde(default)]
    pub checks: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub rationale: bool,
    #[serde(default)]
    pub tracks: Vec<String>,
}

/// Rules 12: why a debt of the ledger was closed without a restore (`restore.closed`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RestoreClosure {
    /// The platform's engagement released its effect and the platform is spent by it (a
    /// loitering munition: the catalog's `expended_by_effect`): there is nothing to put its
    /// values back on.
    Expended { intent: IntentId },
    /// An operator wrote it off, under their own session, saying why (the vehicle is lost or
    /// destroyed, another vehicle has the uid now, an earlier log is gone for good).
    WrittenOff {
        operator: OperatorId,
        session: SessionId,
        note: String,
    },
}

impl RestoreClosure {
    pub fn as_str(&self) -> &'static str {
        match self {
            RestoreClosure::Expended { .. } => "expended",
            RestoreClosure::WrittenOff { .. } => "written_off",
        }
    }
}

/// The build a log was written by (`run.started`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildInfo {
    /// The commit the binary was built from ("unknown" outside a git checkout).
    pub commit: String,
    /// The working tree had changes to tracked files (None: unknown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
}

impl BuildInfo {
    /// This binary's build (`build.rs`).
    pub fn current() -> BuildInfo {
        BuildInfo {
            commit: env!("MURMUR_BUILD_COMMIT").to_string(),
            dirty: match env!("MURMUR_BUILD_DIRTY") {
                "1" => Some(true),
                "0" => Some(false),
                _ => None,
            },
        }
    }
}

/// Where events go. Implementations must append, never rewrite.
pub trait AuditSink {
    fn append(&mut self, line: &str) -> std::io::Result<()>;
}

/// In-memory log; clones share the buffer, so a test can read what the engine wrote.
#[derive(Clone, Debug, Default)]
pub struct MemorySink(Arc<Mutex<Vec<String>>>);

impl MemorySink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn text(&self) -> String {
        let mut s = self.lines().join("\n");
        s.push('\n');
        s
    }
}

impl AuditSink for MemorySink {
    fn append(&mut self, line: &str) -> std::io::Result<()> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(line.to_string());
        Ok(())
    }
}

/// A JSONL file opened in append mode (e.g. `logs/audit/<run-id>.jsonl`). Each event is
/// written and flushed before the engine acts on it.
#[derive(Debug)]
pub struct JsonlFile(File);

impl JsonlFile {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Ok(JsonlFile(
            OpenOptions::new().create(true).append(true).open(path)?,
        ))
    }
}

impl AuditSink for JsonlFile {
    fn append(&mut self, line: &str) -> std::io::Result<()> {
        let mut buf = Vec::with_capacity(line.len() + 1);
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
        self.0.write_all(&buf)?;
        self.0.flush()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("line {line}: {err}")]
pub struct LogError {
    pub line: usize,
    pub err: serde_json::Error,
}

/// Parse a JSONL log. Blank lines are skipped.
pub fn parse_log(text: &str) -> Result<Vec<Event>, LogError> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| serde_json::from_str(l).map_err(|err| LogError { line: i + 1, err }))
        .collect()
}
