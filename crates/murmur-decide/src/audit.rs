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
//! the writer's control: a second machine, a signed checkpoint, a transparency log. That
//! is future work; nothing here signs or anchors anything.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::areas::AreaSet;
use crate::authority::Grants;
use crate::engage::{
    AssessResult, EngageOrder, EngagePreview, EngageSnapshot, Fallback, ObserveStep, PictureToken,
    StopCause,
};
use crate::geo::LonLat;
use crate::ids::{AgentId, IntentId, OperatorId, SessionId, Uid};
use crate::intent::{IntentSpec, State, Task, Transition};
use crate::policy::{DeadlineBasis, Policy, PolicyDecision};
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

/// When a check ran: at submission, or again at approval against the then-current world.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckPhase {
    Submit,
    Approve,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum EventBody {
    /// The engine started; the policy in force for every decision that follows, and the
    /// area set if one was given at start.
    #[serde(rename = "run.started")]
    RunStarted {
        crate_version: String,
        policy: Policy,
        areas: Option<LoggedAreas>,
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
    IntentCreated { intent: IntentSpec },

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
    },

    /// Per-vehicle task progress from the task layer (sent, acked, active, done, failed).
    #[serde(rename = "op.state")]
    OpState {
        intent: IntentId,
        uid: Uid,
        state: String,
        detail: String,
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

    /// A well-formed view: a known event, an action only on `decide` and then a known one.
    pub fn validate(&self) -> Result<(), String> {
        if !Self::EVENTS.contains(&self.event.as_str()) {
            return Err(format!("event must be one of {}", Self::EVENTS.join(", ")));
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
