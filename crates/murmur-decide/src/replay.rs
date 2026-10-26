//! Replay (Phase C.4 exit gate): recompute every check and policy decision in a log from
//! the inputs logged with it, and verify the log's own story.
//!
//! What is verified:
//! - every `check.result`: rules re-run on the logged inputs give the logged results;
//! - every `policy.decision`: the policy (as logged in `run.started`) re-run on the logged
//!   inputs gives the logged decision;
//! - every `intent.state` is a legal transition from the state replay tracked, and is
//!   backed by its evidence: auto approval by a recomputed `auto` tier and passing checks,
//!   `approved` by an `approval.granted` from an operator before the deadline and a passing
//!   re-check, rejection by failing checks or a human rejection, expiry by the deadline;
//! - `seq` is contiguous and `t` never goes backwards;
//! - the hash chain: each event's `prev` is the SHA-256 of the previous line, and (from
//!   text) each line is its event's canonical serialization;
//! - area sets: each logged set's content hash matches its `sha256`, and every decision
//!   references the set in force;
//! - `self_approved` on every `approval.granted` matches who proposed the intent;
//! - every `approval.at_submission` (a human's submission that is itself the approval):
//!   the intent was just checked and passed, its recomputed policy decision is a human tier
//!   that lets the submitter authorize, the source is a human (never an agent) and the
//!   approver is that human; and an intent whose decision says the submitter authorizes is
//!   not sent to the inbox instead;
//! - every `approval.requested` deadline: `[approval] expire_s`, or the kind's
//!   `[approval.kind.<kind>]` setting; a collect with `until = "tasking_lead"` from the
//!   answer its policy decision read, mapped to engine time through the `space.loaded`
//!   scenario map;
//! - space answers (collect, cover): each `space.query`'s content hash matches its
//!   `sha256`, every decision references a logged answer, and a dispatched collect is the
//!   chosen option. Rules re-decide from the logged answers, so this needs no space cache.
//! - two-person approval: every signature (`approval.concurred`, `approval.granted`) is by
//!   the operator its session was opened for, never the proposer where the policy excludes
//!   the proposer, never twice by the same operator, and `approved` comes only with as many
//!   distinct signatures as the recomputed decision asks;
//! - engagements (Phase E): every signature on an engagement is under a session opened with
//!   engagement authority; the preview logged with each check, the snapshot in the final
//!   `approval.granted`, every `engage.picture` verdict (hold the picture: the voids), every
//!   `engage.observe` step and the `engage.assessed` verdict are recomputed from their
//!   logged inputs; the effect follows a picture that held and was ready, at the snapshot's
//!   point; a failed picture is followed by its `engage.stopped` before anything else
//!   happens to that engagement (a suppressed void is a mismatch); an abort is backed by an
//!   `engage.stopped`, a release by an `engage.effect`, and the end by an assessment;
//! - what was executed is what was approved (an audit check, for every rules version): each
//!   `intent.dispatched` is exactly the approved intent's order (its task and vehicles, and
//!   the space or engagement order [`crate::engine`] derives from the intent, the answer its
//!   last check read and the approval's snapshot, the frozen plan), a dispatch transition
//!   has its `intent.dispatched` right before it and nothing else follows one; an
//!   `engage.effect` comes from the engagement's platform and the picture that released it
//!   lies inside the plan's time tolerance ([`engage::release_window`]), the effect never
//!   before it; an `op.state` is a dispatched intent's, on its own vehicles (a collect's
//!   satellite, a cover's service), a vehicle or space task's only while it runs, after a
//!   newer order took its vehicle only how its op ended, `superseded` only when something
//!   replaced it, and a reported fallback is the one (and the task) its stop released; a
//!   failed picture is stopped for the picture; a superseded stop names the order dispatched
//!   last, still under way, on the platform, with that order's source; a `system` stop that
//!   quotes a failsafe, a mode change or an intercept report follows it on its own platform;
//!   an engagement never fails like a task;
//! - sessions: from rules 4 every session is for an operator on the roster `run.started`
//!   names, with engagement authority only for its engagement operators; rejections and
//!   at-submission approvals are under the decider's own session.
//! - carry-overs from an earlier run: each `intent.resumed` against the log it names
//!   ([`ReplayOptions::previous`]), each carried `platform.restore` against what the log it
//!   names still owed, that log replaying itself ([`ReplayOptions::restores_from`]; the two
//!   can be different logs; either not given is a mismatch unless trusted,
//!   [`ReplayOptions::trust_resumed`], and [`ReplayReport::carried_restores`] says which were
//!   checked); and an op whose start went out (`starting`, or the vehicle started it, a
//!   followed vehicle after a restart included) never ends `requeued` on that delivery.
//! - authentication (rules 8, [`crate::rules::OPERATOR_AUTH_SINCE`]): `run.started` says how
//!   operators were authenticated (the roster's `auth`, [`ReplayReport::auth`]); every
//!   session was opened that way (`password` or a harness-held `scripted` credential in a
//!   credentials run, `asserted` in a dev run; [`ReplayReport::people`] and
//!   [`ReplayReport::scripted`] tell the first two apart); a human's `intent.created` names
//!   the live session of theirs it was made in, an agent's none; every other operator action
//!   is under the operator's own session; nothing acts under a session after its
//!   `session.closed`; a step-up is only in a credentials run, by the credential its session
//!   was opened with; an act quoting one quotes a step-up under its own session, unused, at
//!   most [`crate::STEP_UP_MAX_AGE_MS`] old (one nomination's acceptances of lost vehicles
//!   share theirs), and in a credentials run every signature whose recomputed decision says
//!   `step_up` (every engagement signature), and every act `[approval] step_up_acts` lists,
//!   quotes one; a refused login (`auth.refused`) is only in a credentials run, for an
//!   operator it lists; an intent is carried over only from a run that authenticated alike.
//!   [`ReplayReport::authenticated`] is what evaluation reads before counting a log's human
//!   decisions, with the log's anchors checked and only its people's acts.
//! - passkeys (rules 10, [`crate::rules::PASSKEY_SINCE`]): the roster's relying party and keys
//!   are ones a run could start with (a credentials run, a domain, keys on the curve, a study's
//!   participant with a passkey and a passkey policy unless overridden); every passkey login
//!   and step-up's assertion verifies ([`crate::passkey::verify`]) against the key the run
//!   logged or registered since (not removed, the operator's own), on the challenge recomputed
//!   from the first line's hash, the session, what it was for, the nonce and the issue time,
//!   each nonce once, within [`crate::passkey::CHALLENGE_TTL_MS`], its counter counting up; an
//!   act quoting a passkey step-up is the act it was made for, and where the policy asks a
//!   person for a passkey their quoted step-up is one; a registration through a session quotes
//!   a password step-up. [`ReplayReport::study_evidence`] says whether a study's participant
//!   signed every consequential act by passkey.
//!
//! **Space recomputation.** [`replay_str_with_space`] also re-runs every logged space query
//! on the segment given. If that segment is the one the query ran on (same
//! [`crate::SegmentId`] hash), the new answer must equal the logged one, else it is a
//! mismatch. If the segment differs (other element sets or catalog), the query is not
//! recomputed and the event goes to [`ReplayReport::cache_mismatches`]: a cache mismatch,
//! which says nothing about the decisions (they were re-decided from the logged answer).
//!
//! **Rules versions.** A log is recomputed with the decision rules it records in
//! `run.started` (none: version 1), but only rules the verifier accepts: by default the
//! current ones ([`ReplayOptions::rules`]; `murmur-replay --rules N` or `--min-rules N`
//! accepts older ones explicitly), so a log can't pass by claiming older, looser rules.
//! What the log carries must fit its claim: `run.started` comes first and only once, and no
//! field or check only later builds write (a `checked_t`, a GPS fact, a `platform_free`
//! check, a policy key of a later schema) may appear in a log that claims earlier rules.
//!
//! Physics is not replayed; the decision layer is. What replay cannot know is whether the
//! logged inputs were true of the world: it proves the decisions follow from them, and that
//! the orders logged as executed are the approved ones (not that a vehicle flew them: op
//! states and vehicle events are the bridge's reports, bound to their intent and vehicle but
//! not otherwise checked). The
//! chain is tamper-evident, not tamper-proof (see [`crate::audit`]): a full rewrite or a
//! truncated tail needs the head hash ([`ReplayReport::head_sha256`]) checked against an
//! external anchor.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::areas::AreaSet;
use crate::audit::{CheckPhase, Event, EventBody, LogError, parse_log};
use crate::authority::{AuthMethod, AuthMode, Grants, Roster, STEP_UP_MAX_AGE_MS, StepUpRef, effective_mode};
use crate::engage::{
    self, EffectRecord, EngageOrder, EngageSnapshot, Fallback, FrozenPlan, ObserveStep,
    PictureToken, StopCause, TrackIdentity,
};
use crate::engine::{
    PRELOAD_CHANGED, PRELOAD_FAILED, REFUSED, StartAhead, dispatched_event, engaged_vehicles,
    is_self_approval, op_state_mismatch, op_state_owner, order_for, preload_mismatch,
    preload_verdict, queued_on, refusal_reason, supersede_mismatch, waiting_vehicles,
};
use crate::ids::{OperatorId, SessionId, Uid};
use crate::hash::{GENESIS, sha256_hex};
use crate::ids::IntentId;
use crate::intent::{IntentSpec, Source, State, Task, TaskKind, Transition, next_state};
use crate::policy::{self, Policy, PolicyDecision, Tier};
use crate::queue::{self as queue, Basis, CarriedApproval, OpenIntent, Verdict};
use crate::rules::{
    self, CheckResult, OPERATOR_AUTH_SINCE, PASSKEY_SCHEMA_SINCE, PASSKEY_SINCE, RULE_ID_SCHEMA_SINCE, RULE_ID_SINCE,
    STEP_UP_SCHEMA_SINCE,
};
use crate::space::{self, ScenarioMap, SpaceAnswer, SpaceOracle, SpaceOrder};
use crate::time::Time;
use crate::world::{LoggedInputs, LoggedSpace, SpaceInputs, WorldSnapshot};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Mismatch {
    pub seq: u64,
    pub intent: Option<IntentId>,
    pub what: String,
    pub logged: String,
    pub recomputed: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ReplayReport {
    pub events: usize,
    pub intents: usize,
    pub checks_recomputed: usize,
    pub policy_recomputed: usize,
    pub transitions_verified: usize,
    /// Area sets logged (`run.started` and `areas.loaded`).
    pub area_sets: usize,
    pub approvals: usize,
    /// Approvals where the approver also proposed the intent.
    pub self_approvals: usize,
    /// Human submissions that were themselves the approval (`approval.at_submission`). Not
    /// counted in `approvals` or `self_approvals`, which are inbox approvals.
    pub approvals_at_submission: usize,
    /// SHA-256 of the last event: compare with an external anchor to detect a rewritten
    /// chain or a truncated tail.
    pub head_sha256: Option<String>,
    pub mismatches: Vec<Mismatch>,
    /// Space segments logged (`space.loaded`).
    pub space_segments: usize,
    /// Space answers logged (`space.query`).
    pub space_queries: usize,
    /// Space queries re-run on the given segment and found equal (or not: see mismatches).
    pub space_recomputed: usize,
    /// The decision rules the log was produced under (`run.started`; 1 when absent), which
    /// replay recomputes with.
    pub rules_version: u32,
    /// The build that wrote the log (`run.started`; absent before rules 3).
    pub build: Option<crate::audit::BuildInfo>,
    /// Space queries that could not be recomputed because the segment given to replay is
    /// not the one they ran on. Not decision mismatches: [`Self::ok`] ignores them.
    pub cache_mismatches: Vec<Mismatch>,
    /// Signatures that were not the last one a tier needed (`approval.concurred`).
    pub concurrences: usize,
    /// Engagement intents.
    pub engagements: usize,
    /// Hold-the-picture verdicts recomputed (`engage.picture`), and how many failed (voids).
    pub pictures_recomputed: usize,
    pub picture_voids: usize,
    /// Effects released, engagements aborted after dispatch, assessments made.
    pub effects: usize,
    pub aborts: usize,
    pub assessments: usize,
    /// Rules 6: platforms whose own lost-link values a preload replaced and that were still
    /// owed back when the log ended (no confirmed restore): the next run carries them over
    /// (`platform.restore`), but the operator should know. Not decision mismatches.
    pub unrestored: Vec<String>,
    /// Rules 12: debts of the ledger closed without a restore (`restore.closed`): a platform
    /// expended by its own effect, a platform's values or an earlier log's carry written off by
    /// an operator, each saying which and why.
    pub restores_closed: Vec<String>,
    /// Rules 15: platforms an operator un-spent (`platform.unspent`), each saying who and why.
    pub unspent: Vec<String>,
    /// Rules 5: intents queued for unreachable vehicles, queue re-checks recomputed (and
    /// how many dispatched, went back to the inbox, voided), queue expiries, intents carried
    /// over from a previous run, and vehicle reboots.
    pub queued: usize,
    pub rechecks: usize,
    pub recheck_dispatched: usize,
    pub recheck_inbox: usize,
    pub recheck_voided: usize,
    pub queue_expired: usize,
    pub resumed: usize,
    pub reboots: usize,
    /// Rules 5: dispatched intents sent back to the queue (their link lost before the start).
    pub requeued: usize,
    /// Rules 11: space tasks cancelled after dispatch, re-checked against a later answer and
    /// found to hold, and found no longer feasible (failed).
    pub space_cancelled: usize,
    pub space_rechecked: usize,
    pub space_missed: usize,
    /// Rules 6: the carried `platform.restore` events, by the earlier log they name: how
    /// many, and whether they were checked against what that log still owed (it was given,
    /// [`ReplayOptions::restores_from`]) or not (it was absent).
    pub carried_restores: Vec<CarriedRestores>,
    /// The intents the log leaves open (a vehicle task queued, approved but not dispatched,
    /// or dispatched with ops still running): what a restarted bridge carries over.
    #[serde(skip)]
    pub open: Vec<OpenIntent>,
    /// Rules 9: hostile acts recomputed, declarations by rule and by operators, confirmations,
    /// revocations, flags raised and resolved.
    pub acts: usize,
    pub rule_declarations: usize,
    pub operator_declarations: usize,
    pub confirmations: usize,
    pub revocations: usize,
    pub flags: usize,
    pub flags_resolved: usize,
    /// Rules 16: tracks the tracker dropped (lost contacts recorded).
    pub lost_contacts: usize,
    /// Rules 16: claims raised on lost contacts (`lost.claimed`).
    pub lost_claims: usize,
    /// Rules 16: lost contacts an operator confirmed as a live track (`track.reacquired`).
    pub lost_confirmed: usize,
    /// Rules 16: confirmations void, lost contacts counting again (`lost.reinstated`).
    pub lost_reinstated: usize,
    /// One past the highest intent id the log holds (a restarted bridge numbers on from it).
    pub next_intent: u64,
    /// The last event's seq.
    pub last_seq: u64,
    /// Rules 8: how the run authenticated its operators (the roster's `auth`): `credentials`
    /// (each person's password checked by the bridge) or `dev` (names asserted). None before
    /// rules 8: names asserted, as `dev`.
    pub auth: Option<AuthMode>,
    /// Rules 8: step-ups logged (a credential re-entered to sign), signatures that quoted
    /// one, and sessions ended (`session.closed`).
    pub step_ups: usize,
    pub stepped_signatures: usize,
    pub sessions_closed: usize,
    /// Rules 8: acts other than signatures that quoted a step-up (a hostile declaration, a
    /// nomination's acceptances of lost vehicles, when `[approval] step_up_acts` lists them).
    pub stepped_acts: usize,
    /// Rules 8, a credentials run: logins refused for operators the roster lists
    /// (`auth.refused`: a wrong password or a locked id; nothing secret is logged).
    pub logins_refused: usize,
    /// Rules 8, a credentials run: the operators who logged in with their own password (a
    /// person's), and those whose credential is harness-held (`auth: scripted`: a runner
    /// logs them in; authenticated, but never a person's judgment). Only the first are
    /// evidence about people.
    pub people: BTreeSet<OperatorId>,
    pub scripted: BTreeSet<OperatorId>,
    /// Rules 10: logins by passkey, people's step-ups by passkey and by password, and passkeys
    /// registered and removed while the run was up. Every passkey use was verified against
    /// the public key the run logged.
    pub passkey_logins: usize,
    pub passkey_step_ups: usize,
    pub password_step_ups: usize,
    pub passkeys_registered: usize,
    pub passkeys_removed: usize,
    /// Rules 10: a study run, its participant and how their consequential acts were stepped up.
    pub study: Option<StudyReport>,
}

/// Rules 10: what a study run's log shows of its participant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StudyReport {
    pub participant: OperatorId,
    /// The explicit override that allowed password step-ups, and its reason: a run with it is
    /// never evidence.
    pub password_step_up: Option<String>,
    /// The participant's signatures and other stepped-up acts, by the factor of the step-up
    /// each quoted.
    pub passkey_acts: usize,
    pub password_acts: usize,
}

impl ReplayReport {
    /// Every decision reproduced from the logged inputs (including logged space answers).
    pub fn ok(&self) -> bool {
        self.mismatches.is_empty()
    }

    /// Whether the log's human decisions are bound to authenticated operators: a credentials
    /// run (rules 8 or later) that reproduced. A dev run, and every log of earlier rules,
    /// names operators that requests asserted: never evidence about people, and excluded from
    /// any claim about meaningful human control. Two things this does not say: whether an
    /// operator is a person ([`Self::people`]; a scripted operator's credential is
    /// harness-held), and whether the log is the one the run wrote (a rewritten log replays
    /// too: only its anchors tell, `murmur-replay --anchors`, which a claim needs as well).
    pub fn authenticated(&self) -> bool {
        self.ok() && self.auth == Some(AuthMode::Credentials)
    }

    /// Rules 10: whether a study run's log is evidence about its participant: it is
    /// [`Self::authenticated`], a study, without the password override, its participant
    /// signed in as a person, and every consequential act of theirs quoted a passkey step-up.
    /// (The anchors are still the caller's to check: `murmur-replay --anchors`.)
    pub fn study_evidence(&self) -> bool {
        self.authenticated()
            && self.study.as_ref().is_some_and(|s| {
                s.password_step_up.is_none() && s.password_acts == 0 && self.people.contains(&s.participant)
            })
    }

    /// Every logged space query was re-run on the same segment and reproduced.
    pub fn space_reproduced(&self) -> bool {
        self.cache_mismatches.is_empty() && self.space_recomputed == self.space_queries
    }
}

/// Rules 6: a log's carried `platform.restore` events that name one earlier log.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CarriedRestores {
    /// The earlier log they name (`from`, its file name; empty when they name none).
    pub log: String,
    /// How many carried restores name it.
    pub restores: usize,
    /// Checked against what that log still owed ([`owed_restores`]); false: that log was
    /// not given, so they were not (a mismatch unless [`ReplayOptions::trust_resumed`]).
    pub checked: bool,
    /// The head of that log as given (compare it with that log's anchors).
    pub head: Option<String>,
    /// Rules 12: why its carry failed at the start (it could not be read): what it owed is
    /// unknown, and it stays owed until read or written off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
    /// Rules 15: the `.last-run` pointer could not be read (why), and this log, the newest of the
    /// station by its carry chain, was found in its place: carried from when its chain and
    /// anchors checked, else a failed carry (`failed` says why the start would not).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pointer_failed: Option<String>,
    /// Rules 15: a failed carry's fleet file (`<log stem>.fleet.json`): compared with what the
    /// start recorded (true), or not at hand (false); none when it recorded none and it is not
    /// at hand.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fleet_checked: Option<bool>,
    /// Rules 15: the platforms the start carried as spent from it (uids), still spent here.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub spent: Vec<String>,
}

/// Rules 6: the platforms' own lost-link values owed back to them: from a preload's
/// write-ahead (`engage.preload` intent, logged before anything is written: the platform may
/// hold the preload from then on), or carried over from an earlier run (`platform.restore`),
/// until a restore confirms them. Who owes them (the engagements that preloaded since) and
/// the vehicle they were preloaded on.
#[derive(Clone, Debug, Default)]
pub(crate) struct Restores {
    owed: BTreeMap<Uid, Owed>,
    /// Rules 12: earlier logs whose carry failed (they could not be read) and nobody wrote off,
    /// with why and the armed platforms their fleet named (that may be owed values, unknown):
    /// they stay owed until a later start reads them or an operator writes them off.
    pending: BTreeMap<String, PendingCarry>,
    /// Rules 12: the platforms spent by their own effect in this run (`restore.closed`,
    /// expended), by the engagement that released it: no order or nomination for them is taken
    /// any more. Rules 15: with what the closure closed, the vehicle and when, for an un-spend.
    expended: BTreeMap<Uid, Spent>,
    /// Rules 15: the platforms spent by their own effect in an earlier run, carried over at start
    /// (`run.started`'s `carried_from`, `spent`; `from`: the log whose engagement spent it).
    spent_carried: BTreeMap<Uid, SpentPlatform>,
    /// When the run started (`run.started`): a platform spent in an earlier run and heard since
    /// was heard after its effect.
    started_t: Option<Time>,
}

/// Rules 12: a platform spent by its own effect in this run: the engagement whose release spent
/// it, the values its closure closed (rules 15: owed back again if it is un-spent), the vehicle
/// they were preloaded on, when it was marked and whether that engagement has ended.
#[derive(Clone, Debug)]
struct Spent {
    by: IntentId,
    values: BTreeMap<String, f64>,
    vehicle: Option<engage::VehicleIdentity>,
    /// Rules 15: when it was marked (the closure's time) and whether its engagement ended since.
    t: Option<Time>,
    ended: bool,
}

/// Rules 12: an earlier log whose carry failed: why, and the armed platforms its run's fleet
/// named (from the fleet identity beside it, when that could be read), which may be owed values
/// nobody knows: a mission for one of them waits until the log is read or written off.
#[derive(Clone, Debug, Default)]
struct PendingCarry {
    why: String,
    may_owe: Vec<OwedRestore>,
}

#[derive(Clone, Debug, Default)]
struct Owed {
    values: BTreeMap<String, f64>,
    by: Vec<IntentId>,
    vehicle: Option<engage::VehicleIdentity>,
    /// Rules 12: the earlier log they were carried over from (none: preloaded in this run).
    from: Option<String>,
}

impl Restores {
    /// Every platform's own values owed back, with the engagements that preloaded them.
    pub(crate) fn owed(&self) -> Vec<(Uid, BTreeMap<String, f64>, Vec<IntentId>)> {
        self.owed
            .iter()
            .map(|(u, o)| (u.clone(), o.values.clone(), o.by.clone()))
            .collect()
    }

    /// Rules 7: what `spec`'s vehicles are owed back from engagements of this log. Rules 12:
    /// values carried over from an earlier log too, with the vehicle they were preloaded on and
    /// the one with the uid now (`now`, the bridge's identities as the decision read them):
    /// they hold an order unless that is known to be another vehicle
    /// ([`crate::queue::CarriedHold`]); before rules 12 they hold nothing. Empty before rules 7
    /// and for anything but a vehicle task the queue can hold.
    pub(crate) fn owed_for(
        &self,
        spec: &IntentSpec,
        rules: &crate::rules::RulesConfig,
        now: &BTreeMap<Uid, engage::VehicleIdentity>,
    ) -> BTreeMap<Uid, crate::queue::RestoreOwed> {
        if rules.rules_version < crate::rules::QUEUE_FOLLOWUPS_SINCE {
            return BTreeMap::new();
        }
        // rules 15: an engagement's platform a failed carry may owe values to (refused by its
        // fallback_preload check until that log is read or written off)
        if spec.kind() == TaskKind::Engage && rules.carry_followups() {
            return self.may_owe_for(spec, now);
        }
        if !queue::queueable(spec.kind()) {
            return BTreeMap::new();
        }
        spec.uids
            .iter()
            .filter_map(|u| {
                let Some(o) = self.owed.get(u) else {
                    // rules 12: an armed platform a failed carry's fleet named: its values unknown
                    if !rules.followups() {
                        return None;
                    }
                    let (log, m) = self
                        .pending
                        .iter()
                        .find_map(|(log, p)| Some((log, p.may_owe.iter().find(|m| &m.uid == u)?)))?;
                    let now = now.get(u).cloned();
                    let other_vehicle = match (&m.vehicle, &now) {
                        (Some(was), Some(is)) => was.differs(is),
                        _ => None,
                    };
                    return Some((
                        u.clone(),
                        crate::queue::RestoreOwed {
                            values: BTreeMap::new(),
                            by: Vec::new(),
                            carried: Some(Box::new(crate::queue::CarriedHold {
                                from: log.clone(),
                                preloaded_on: m.vehicle.clone(),
                                now,
                                other_vehicle,
                            })),
                        },
                    ));
                };
                let carried = if o.by.is_empty() {
                    if !rules.followups() {
                        return None;
                    }
                    let now = now.get(u).cloned();
                    let other_vehicle = match (&o.vehicle, &now) {
                        (Some(was), Some(is)) => was.differs(is),
                        _ => None,
                    };
                    Some(Box::new(crate::queue::CarriedHold {
                        from: o.from.clone().unwrap_or_default(),
                        preloaded_on: o.vehicle.clone(),
                        now,
                        other_vehicle,
                    }))
                } else {
                    None
                };
                Some((
                    u.clone(),
                    crate::queue::RestoreOwed {
                        values: o.values.clone(),
                        by: o.by.clone(),
                        carried,
                    },
                ))
            })
            .collect()
    }

    /// Rules 15: `spec`'s vehicles that an earlier log whose carry failed may owe values to (an
    /// armed platform its fleet named; values unknown), with the vehicle it was and the one with
    /// the uid now: they hold unless that is known to be another vehicle.
    fn may_owe_for(
        &self,
        spec: &IntentSpec,
        now: &BTreeMap<Uid, engage::VehicleIdentity>,
    ) -> BTreeMap<Uid, crate::queue::RestoreOwed> {
        spec.uids
            .iter()
            .filter_map(|u| {
                let (log, m) = self
                    .pending
                    .iter()
                    .find_map(|(log, p)| Some((log, p.may_owe.iter().find(|m| &m.uid == u)?)))?;
                let now = now.get(u).cloned();
                let other_vehicle = match (&m.vehicle, &now) {
                    (Some(was), Some(is)) => was.differs(is),
                    _ => None,
                };
                Some((
                    u.clone(),
                    crate::queue::RestoreOwed {
                        values: BTreeMap::new(),
                        by: Vec::new(),
                        carried: Some(Box::new(crate::queue::CarriedHold {
                            from: log.clone(),
                            preloaded_on: m.vehicle.clone(),
                            now,
                            other_vehicle,
                        })),
                    },
                ))
            })
            .collect()
    }

    /// Rules 7: whether any of `spec`'s vehicles is owed values that hold a mission now
    /// ([`Self::owed_for`] on the identities `now`; none known: carried values hold).
    pub(crate) fn holds(
        &self,
        spec: &IntentSpec,
        rules: &crate::rules::RulesConfig,
        now: &BTreeMap<Uid, engage::VehicleIdentity>,
    ) -> bool {
        self.owed_for(spec, rules, now).values().any(crate::queue::RestoreOwed::holds)
    }

    /// Rules 12: seed the ledger with what the bridge carried over at start (`run.started`'s
    /// `carried_from`, newest log first): each log's platforms' values, merged oldest first (a
    /// later run may have read a preload an older one left as the platform's own, so for a
    /// value both name the older log's stands), and the logs whose carry failed, pending (as
    /// is a log one of them names as pending without an entry of its own).
    pub(crate) fn carry(&mut self, from: &[CarriedFrom]) {
        // rules 15: the platforms each log read had spent (its own, or carried into it), the
        // newest log's standing
        for c in from.iter().rev().filter(|c| c.error.is_none()) {
            for sp in &c.spent {
                let mut sp = sp.clone();
                sp.from.get_or_insert_with(|| c.log.clone());
                self.spent_carried.insert(sp.uid.clone(), sp);
            }
        }
        for c in from.iter().rev() {
            if let Some(why) = &c.error {
                self.pending.insert(
                    c.log.clone(),
                    PendingCarry {
                        why: why.clone(),
                        may_owe: c.may_owe.clone(),
                    },
                );
                continue;
            }
            for o in &c.owed {
                let e = self.owed.entry(o.uid.clone()).or_insert_with(|| Owed {
                    values: BTreeMap::new(),
                    by: Vec::new(),
                    vehicle: o.vehicle.clone(),
                    from: Some(c.log.clone()),
                });
                for (n, v) in &o.values {
                    e.values.entry(n.clone()).or_insert(*v);
                }
            }
        }
        for c in from {
            for p in &c.pending {
                if !from.iter().any(|x| &x.log == p) {
                    self.pending.entry(p.clone()).or_insert_with(|| PendingCarry {
                        why: format!("named as not carried by {}, and not tried", c.log),
                        may_owe: Vec::new(),
                    });
                }
            }
        }
    }

    /// [`Self::carry`] at the run's start `t` (`run.started`'s time).
    pub(crate) fn carry_at(&mut self, t: Time, from: &[CarriedFrom]) {
        self.started_t = Some(t);
        self.carry(from);
    }

    /// Rules 12: a debt closed without a restore (`restore.closed`) at `t`: a platform's values
    /// (and, `expended`, the platform spent by that engagement's effect, with what was closed),
    /// or a pending log.
    pub(crate) fn close(&mut self, uid: Option<&Uid>, log: Option<&str>, expended: Option<IntentId>, t: Option<Time>) {
        if let Some(u) = uid {
            let was = self.owed.remove(u);
            if let Some(id) = expended {
                let (values, vehicle) = was.map(|o| (o.values, o.vehicle)).unwrap_or_default();
                self.expended.insert(
                    u.clone(),
                    Spent {
                        by: id,
                        values,
                        vehicle,
                        t,
                        ended: false,
                    },
                );
                // spent again in this run: this run's mark stands
                self.spent_carried.remove(u);
            }
        }
        if let Some(l) = log {
            self.pending.remove(l);
        }
    }

    /// Rules 15: intent `id` reached a terminal state: a platform its effect spent counts its
    /// engagement as ended (a later run's un-spend need not wait out its assessment window).
    pub(crate) fn intent_ended(&mut self, id: IntentId) {
        for s in self.expended.values_mut().filter(|s| s.by == id) {
            s.ended = true;
        }
    }

    /// Rules 12: the engagement whose effect spent `uid` in this run, if one did.
    pub(crate) fn expended_by(&self, uid: &Uid) -> Option<IntentId> {
        self.expended.get(uid).map(|s| s.by)
    }

    /// Rules 15: the spent mark on `uid` an un-spend lifts, this run's or carried over: the
    /// engagement, the earlier log (carried), the values it closed, the vehicle spent and this
    /// run's start ([`SpentMark::refusal`] judges an un-spend on it). None: not spent.
    pub(crate) fn spent_mark(&self, uid: &Uid) -> Option<SpentMark> {
        if let Some(s) = self.expended.get(uid) {
            return Some(SpentMark {
                by: s.by,
                from: None,
                values: s.values.clone(),
                vehicle: s.vehicle.clone(),
                started: self.started_t,
                spent_t: s.t,
                ended: s.ended,
            });
        }
        self.spent_carried.get(uid).map(|s| SpentMark {
            by: s.by,
            from: s.from.clone(),
            values: s.values.clone(),
            vehicle: s.vehicle.clone(),
            started: self.started_t,
            spent_t: s.spent_t,
            ended: s.ended,
        })
    }

    /// Rules 15: an operator un-spent `uid`: no longer spent, and the values its spent mark closed
    /// are owed back again (this run's: as the engagement's that preloaded them; carried: as
    /// carried over from the log it came from, for the vehicle they were preloaded on).
    pub(crate) fn unspend(&mut self, uid: &Uid) {
        let reopened = if let Some(s) = self.expended.remove(uid) {
            Some(Owed {
                values: s.values,
                by: vec![s.by],
                vehicle: s.vehicle,
                from: None,
            })
        } else {
            self.spent_carried.remove(uid).map(|s| Owed {
                values: s.values,
                by: Vec::new(),
                vehicle: s.vehicle,
                from: s.from,
            })
        };
        if let Some(o) = reopened.filter(|o| !o.values.is_empty()) {
            self.owed.insert(uid.clone(), o);
        }
    }

    /// Rules 15: `spec`'s vehicles spent in an earlier run (carried over), with the vehicle with
    /// the uid now (`now`, the bridge's identities as the decision read them): held unless it is
    /// known to be another (a decision's fact, `spent_carried`; empty before rules 15).
    pub(crate) fn spent_carried_for(
        &self,
        spec: &IntentSpec,
        rules: &crate::rules::RulesConfig,
        now: &BTreeMap<Uid, engage::VehicleIdentity>,
    ) -> BTreeMap<Uid, crate::queue::CarriedSpent> {
        if !rules.carry_followups() {
            return BTreeMap::new();
        }
        spec.uids
            .iter()
            .filter_map(|u| {
                let s = self.spent_carried.get(u)?;
                let now = now.get(u).cloned();
                let other_vehicle = match (&s.vehicle, &now) {
                    (Some(was), Some(is)) => was.differs(is),
                    _ => None,
                };
                Some((
                    u.clone(),
                    crate::queue::CarriedSpent {
                        from: s.from.clone().unwrap_or_default(),
                        by: s.by,
                        spent_on: s.vehicle.clone(),
                        now,
                        other_vehicle,
                    },
                ))
            })
            .collect()
    }

    /// Rules 12 and 15: why an order for `spec` is refused as for a spent platform, if it is: a
    /// vehicle spent in this run, or one spent in an earlier run that `now` does not show is
    /// another vehicle (none known: held).
    pub(crate) fn spent_hold(
        &self,
        spec: &IntentSpec,
        rules: &crate::rules::RulesConfig,
        now: &BTreeMap<Uid, engage::VehicleIdentity>,
    ) -> Option<(Uid, String)> {
        if !rules.followups() {
            return None;
        }
        if let Some((u, by)) = spec.uids.iter().find_map(|u| Some((u, self.expended_by(u)?))) {
            return Some((u.clone(), format!("vehicle {u} was spent by its own effect (engagement {by})")));
        }
        self.spent_carried_for(spec, rules, now)
            .into_iter()
            .find(|(_, s)| s.holds())
            .map(|(u, s)| {
                let why = format!(
                    "vehicle {u} was spent by its own effect (engagement {} of {}, carried over)",
                    s.by, s.from
                );
                (u, why)
            })
    }

    /// Rules 15: whether `uid` carries a spent mark of an earlier run (whatever vehicle has it).
    pub(crate) fn spent_carried_on(&self, uid: &Uid) -> bool {
        self.spent_carried.contains_key(uid)
    }

    /// Rules 12 and 15: the platforms spent by their own effect, this run's (`from` none) and
    /// those carried over.
    pub(crate) fn spent(&self) -> Vec<SpentPlatform> {
        self.expended
            .iter()
            .map(|(u, s)| SpentPlatform {
                uid: u.clone(),
                by: s.by,
                from: None,
                values: s.values.clone(),
                vehicle: s.vehicle.clone(),
                spent_t: s.t,
                ended: s.ended,
            })
            .chain(self.spent_carried.values().cloned())
            .collect()
    }

    /// Rules 12: `spec`'s vehicles spent by their own effect, by the engagement that released
    /// it (a decision's fact, `expended`; empty before rules 12).
    pub(crate) fn expended_for(&self, spec: &IntentSpec, rules: &crate::rules::RulesConfig) -> BTreeMap<Uid, IntentId> {
        if !rules.followups() {
            return BTreeMap::new();
        }
        spec.uids
            .iter()
            .filter_map(|u| Some((u.clone(), self.expended_by(u)?)))
            .collect()
    }

    /// Rules 15: what is owed back to `uid`, with the vehicle it was preloaded on, if anything.
    pub(crate) fn owed_restore(&self, uid: &Uid) -> Option<OwedRestore> {
        self.owed.get(uid).map(|o| OwedRestore {
            uid: uid.clone(),
            values: o.values.clone(),
            by: o.by.clone(),
            vehicle: o.vehicle.clone(),
        })
    }

    /// Rules 12: the values owed back to `uid`, if any.
    pub(crate) fn values_owed(&self, uid: &Uid) -> Option<&BTreeMap<String, f64>> {
        self.owed.get(uid).map(|o| &o.values)
    }

    /// Rules 12: whether `log`'s carry failed and nobody wrote it off.
    pub(crate) fn is_pending(&self, log: &str) -> bool {
        self.pending.contains_key(log)
    }

    /// Rules 12: the earlier logs pending (their carry failed), with why and the armed platforms
    /// they may owe values to.
    pub(crate) fn pending(&self) -> Vec<(String, String, Vec<Uid>)> {
        self.pending
            .iter()
            .map(|(l, p)| (l.clone(), p.why.clone(), p.may_owe.iter().map(|m| m.uid.clone()).collect()))
            .collect()
    }

    /// Rules 12: the values owed back that no engagement of this run preloaded (carried
    /// over), with the vehicle they were preloaded on.
    pub(crate) fn carried(&self) -> Vec<OwedRestore> {
        self.owed
            .iter()
            .filter(|(_, o)| o.by.is_empty())
            .map(|(uid, o)| OwedRestore {
                uid: uid.clone(),
                values: o.values.clone(),
                by: Vec::new(),
                vehicle: o.vehicle.clone(),
            })
            .collect()
    }

    /// A preload set on `uid` read `before` as the platform's own values: while earlier
    /// values are owed, those it shares with them must be them (a value read off a preloaded
    /// platform is not its own); the others are owed too from now on. Err: the values owed.
    pub(crate) fn owe(
        &mut self,
        uid: &Uid,
        intent: IntentId,
        before: &BTreeMap<String, f64>,
        vehicle: Option<&engage::VehicleIdentity>,
    ) -> Result<(), String> {
        let o = self.owed.entry(uid.clone()).or_default();
        if !o.by.contains(&intent) {
            o.by.push(intent);
        }
        if let Some(v) = vehicle {
            o.vehicle = Some(v.clone());
        }
        let differs = before.iter().any(|(n, v)| o.values.get(n).is_some_and(|w| w != v));
        let was = json(&o.values);
        for (n, v) in before {
            o.values.entry(n.clone()).or_insert(*v);
        }
        if differs {
            return Err(was);
        }
        Ok(())
    }

    /// A confirmed restore on `uid` read `readback`: paid when it shows every value owed.
    pub(crate) fn paid(&mut self, uid: &Uid, readback: &BTreeMap<String, f64>) {
        if self
            .owed
            .get(uid)
            .is_some_and(|o| engage::restored(&o.values, readback).is_ok())
        {
            self.owed.remove(uid);
        }
    }

    /// The ledger after one logged event, as logged (no recomputation: replay checks it).
    fn step(&mut self, e: &Event) {
        self.step_at(e.t, &e.body);
    }

    /// [`Self::step`] on an event's body (the engine keeps the same ledger as it logs).
    pub(crate) fn step_body(&mut self, body: &EventBody) {
        match body {
            EventBody::EngagePreload {
                intent,
                uid,
                phase,
                before,
                readback,
                confirmed,
                vehicle,
                ..
            } => match phase {
                engage::PreloadPhase::Intent | engage::PreloadPhase::Set if !before.is_empty() => {
                    let _ = self.owe(uid, *intent, before, vehicle.as_deref());
                }
                engage::PreloadPhase::Restore if *confirmed => self.paid(uid, readback),
                _ => {}
            },
            EventBody::PlatformRestore {
                uid,
                values,
                carried,
                from,
                readback,
                confirmed,
                vehicle,
                ..
            } => {
                if *carried {
                    self.owed.entry(uid.clone()).or_insert_with(|| Owed {
                        values: values.clone(),
                        by: Vec::new(),
                        vehicle: vehicle.as_deref().cloned(),
                        from: Some(carried_restore_log(from.as_deref()).to_string()),
                    });
                } else if *confirmed {
                    self.paid(uid, readback);
                }
            }
            EventBody::RunStarted { carried_from, .. } => self.carry(carried_from),
            EventBody::RestoreClosed { uid, log, closed_by, .. } => self.close(
                uid.as_ref(),
                log.as_deref(),
                match closed_by {
                    crate::audit::RestoreClosure::Expended { intent } => Some(*intent),
                    crate::audit::RestoreClosure::WrittenOff { .. } => None,
                },
                None,
            ),
            EventBody::PlatformUnspent { uid, .. } => self.unspend(uid),
            EventBody::IntentState { intent, to, .. } if to.is_terminal() => self.intent_ended(*intent),
            _ => {}
        }
    }

    /// [`Self::step_body`] for an event logged at `t` (a carry keeps the run's start, a spent
    /// mark when it was set).
    pub(crate) fn step_at(&mut self, t: Time, body: &EventBody) {
        match body {
            EventBody::RunStarted { carried_from, .. } => self.carry_at(t, carried_from),
            EventBody::RestoreClosed {
                uid,
                log,
                closed_by: crate::audit::RestoreClosure::Expended { intent },
                ..
            } => self.close(uid.as_ref(), log.as_deref(), Some(*intent), Some(t)),
            _ => self.step_body(body),
        }
    }

    fn report(&self) -> Vec<String> {
        self.owed
            .iter()
            .map(|(uid, o)| {
                let (values, by) = (&o.values, &o.by);
                let by: Vec<String> = by.iter().map(ToString::to_string).collect();
                let by = if by.is_empty() {
                    "carried over from an earlier run".to_string()
                } else {
                    format!("preloaded by {}", by.join(", "))
                };
                format!("{uid}: its own lost-link values {} never confirmed back ({by})", json(values))
            })
            .chain(self.pending.iter().map(|(log, p)| {
                format!(
                    "earlier log {log}: its carry failed ({}): what it still owed is unknown, and nobody wrote it off",
                    p.why
                )
            }))
            .collect()
    }
}

/// Rules 6: the platforms' own lost-link values a log still owed back when it ended (a
/// preload set, or values carried over into it, never confirmed back), and the engagements
/// that preloaded them. The bridge carries them over into its next run's log at start
/// (rules 6 to 10: `platform.restore` carried; rules 12: in `run.started`, `carried_from`)
/// and puts them back. Lines that do not parse (a torn last line) are skipped: this is a
/// ledger, not a replay.
#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize)]
pub struct OwedRestore {
    pub uid: Uid,
    pub values: BTreeMap<String, f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by: Vec<IntentId>,
    /// The vehicle they were preloaded on (from the write-ahead): they go back only to a
    /// vehicle that matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle: Option<engage::VehicleIdentity>,
}

/// Rules 12: one earlier log the bridge carried over from at start, as `run.started` records
/// it (`carried_from`): the log `.last-run-<port>` named, or one whose carry had failed and
/// that nobody wrote off (named `pending` by a log carried before it in the list).
#[derive(Clone, Debug, Default, PartialEq, Serialize, serde::Deserialize)]
pub struct CarriedFrom {
    /// Its file name (the bridge keeps its logs in one directory).
    pub log: String,
    /// The SHA-256 of its last event's line as read (its head) and that event's seq: the log
    /// replay is given must be exactly that one (compare its anchors too).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seq: Option<u64>,
    /// The platforms' own lost-link values it still owed back ([`owed_restores`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owed: Vec<OwedRestore>,
    /// The earlier logs it still owed for (their carry had failed, nobody wrote them off): each
    /// is tried again in the same start, an entry of its own after this one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<String>,
    /// A log that could not be read: the armed platforms its run's fleet named (the fleet
    /// identity beside it, `<log>.fleet.json`, when that could be read), each with the vehicle
    /// it was, values unknown: a mission for one of them waits (fail closed) until the log is
    /// read or written off. Empty when the fleet is unknown too: then nothing can be held.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub may_owe: Vec<OwedRestore>,
    /// Why it could not be read: what it owed is unknown, and it stays owed (pending) until a
    /// later start reads it or an operator writes it off. Nothing else is set then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Rules 15: the platforms it had spent by their own effect and nobody un-spent (its own,
    /// and those carried into it): they stay spent in this run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spent: Vec<SpentPlatform>,
    /// Rules 15: a failed carry's fleet file as read (its hash and the armed platforms that
    /// made `may_owe`); absent when it could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fleet_read: Option<FleetRead>,
    /// Rules 15: the `.last-run` pointer could not be read (why), and this log (the newest of the
    /// station whose chain and anchors checked) was carried from in its place. Only the first
    /// entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer_failed: Option<String>,
}

/// Rules 12: what a log still owes when it ends: the platforms' own lost-link values never
/// confirmed back, and the earlier logs whose carry failed that nobody wrote off. Rules 15: and
/// the platforms spent by their own effect that nobody un-spent (its own, `from` none, and those
/// carried into it).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Debts {
    pub owed: Vec<OwedRestore>,
    pub pending: Vec<String>,
    pub spent: Vec<SpentPlatform>,
    /// Rules 15: for each pending log, the platforms its failed carry was recorded as possibly
    /// owing (`may_owe`): a later start that retries it and can't read its fleet file either
    /// records the same list (the vehicles stay held).
    pub pending_may_owe: BTreeMap<String, Vec<OwedRestore>>,
}

/// Rules 15 ([`crate::rules::CARRY_FOLLOWUPS_SINCE`], murmur-zq3): a platform spent by its own
/// effect, as a log's debts and `run.started`'s `carried_from` (`spent`) carry it: the engagement
/// whose release spent it, the log it is in (none: the log that names it), the values its
/// closure closed (its own lost-link values: owed back again if an operator un-spends it), and
/// the vehicle they were preloaded on (a later run holds only that vehicle as spent).
#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize)]
pub struct SpentPlatform {
    pub uid: Uid,
    pub by: IntentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle: Option<engage::VehicleIdentity>,
    /// When it was marked spent (its engagement's expended `restore.closed`, right after the
    /// release): the engagement's assessment window counts from no earlier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spent_t: Option<Time>,
    /// Whether the engagement that spent it ended in the log it was spent in (a crash, or a
    /// stop, inside its assessment window leaves it unended: a later run's un-spend then waits
    /// out the window, [`SpentMark::refusal`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ended: bool,
}

/// Rules 15: a spent mark as an un-spend reads it ([`Restores::spent_mark`]): the engagement
/// that spent it, the earlier log of a carried mark, the values the mark closed, the vehicle
/// they were preloaded on (the one spent), this run's start, and when it was marked spent and
/// whether its engagement ended in the log it was spent in.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpentMark {
    pub by: IntentId,
    pub from: Option<String>,
    pub values: BTreeMap<String, f64>,
    pub vehicle: Option<engage::VehicleIdentity>,
    pub started: Option<Time>,
    /// When it was marked spent, and whether its engagement ended in the log it was spent in.
    pub spent_t: Option<Time>,
    pub ended: bool,
}

impl SpentMark {
    /// What a passkey made for this un-spend is for (its challenge binds it).
    pub(crate) fn purpose(&self, uid: &Uid) -> crate::passkey::StepUpFor {
        crate::passkey::StepUpFor::Unspend {
            uid: uid.clone(),
            spent_by: self.by,
            from: self.from.clone(),
        }
    }

    /// Rules 15: why the mark can't be lifted on the platform's facts `v` as the world read them
    /// at `t`, and the bridge's identity `now` of the vehicle under the uid, if it can't. The
    /// engagement that spent it must have ended (`ended`: its terminal transition, this run's
    /// mark), the platform heard after that (its heartbeat, position or navigation status
    /// reported later: a munition's own reports in its last seconds, before its assessment, are
    /// not), its link not silent now (heard within the link-aware fact age limit the checks
    /// use), and the vehicle now the one that was spent (platform, callsign and system id, never
    /// the home). A carried mark counts from this run's start when its engagement ended in its
    /// log; when it did not (a crash or a stop inside the assessment window), from the latest it
    /// would have ended, as unknown, if later: when it was marked plus `assess_window_s`. The
    /// engine refuses an un-spend on it, replay recomputes it from the logged facts, and the
    /// bridge's alert offers the un-spend only when it is none.
    pub(crate) fn refusal(
        &self,
        ended: Option<Time>,
        v: Option<&crate::world::VehicleFacts>,
        now: Option<&engage::VehicleIdentity>,
        t: Time,
        cfg: &crate::rules::RulesConfig,
    ) -> Option<String> {
        // since when it must have been heard: its engagement's end, or the latest it would have
        // ended (a carried mark whose engagement did not end in its log)
        let mut since = "engagement {by} ended";
        let ended = match (&self.from, ended, self.started) {
            (None, Some(e), _) => e,
            (None, None, _) => {
                return Some(format!(
                    "engagement {} that spent it has not ended (its effect is still being observed or assessed): un-spend it once the engagement is over",
                    self.by
                ));
            }
            (Some(_), _, Some(s)) if self.ended => s,
            (Some(_), _, Some(s)) => {
                let Some(marked) = self.spent_t else {
                    return Some(format!(
                        "engagement {} that spent it did not end in its log and when it was spent is not known: it can't be told the engagement is over",
                        self.by
                    ));
                };
                let window = (cfg.engage_rules().assess_window_s * 1000.0).round() as i64;
                let bound = marked.plus_ms(window);
                if bound > s {
                    since = "the latest engagement {by} would have ended (its assessment window from when it spent it: it did not end in its log)";
                }
                s.max(bound)
            }
            (Some(_), _, None) => return Some("this run's start is not known".into()),
        };
        let Some(v) = v else {
            return Some("it is not in the world model".into());
        };
        let Some(heard) = v.heard_t().filter(|h| *h > ended) else {
            return Some(format!(
                "it has not been heard since {} (last heard {}): un-spend it once it answers",
                since.replace("{by}", &self.by.to_string()),
                v.heard_t().map_or("never".to_string(), |h| match ended.ms_since(h) {
                    0 => "then".to_string(),
                    d => format!("{:.1} s before that", d as f64 / 1000.0),
                })
            ));
        };
        let (limit, _) = cfg.fact_age_limit_ms(v.report_interval_ms);
        if t.ms_since(heard) > limit {
            return Some(format!(
                "its link is silent: last heard {:.1} s ago (limit {} s)",
                t.ms_since(heard) as f64 / 1000.0,
                limit as f64 / 1000.0
            ));
        }
        let Some(was) = &self.vehicle else {
            return Some("the vehicle that was spent is not known".into());
        };
        was.same_vehicle(now)
            .err()
            .map(|why| format!("the vehicle under the uid now is not shown to be the one spent: {why}"))
    }
}

/// Rules 15 (murmur-zq3): a failed carry's fleet file as the start read it (`<log stem>.fleet.json`,
/// beside the log): the SHA-256 of its bytes, and the catalog's armed platforms the start judged
/// it by. `may_owe` is that fleet's vehicles of those platforms ([`fleet_may_owe`]); replay
/// compares both with the file when it is at hand.
#[derive(Clone, Debug, Default, PartialEq, Serialize, serde::Deserialize)]
pub struct FleetRead {
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub armed: Vec<String>,
}

/// Rules 15: the vehicles of a run's fleet file (`<log stem>.fleet.json` as the bridge writes it:
/// by uid, each vehicle's platform, callsign and home `[lat, lon]`) whose platform is one of
/// `armed`, in uid order, as a failed carry records them (`may_owe`: values unknown). None: the
/// file does not parse.
pub fn fleet_may_owe(bytes: &[u8], armed: &[String]) -> Option<Vec<OwedRestore>> {
    #[derive(serde::Deserialize)]
    struct Entry {
        #[serde(default)]
        platform: Option<String>,
        #[serde(default)]
        callsign: Option<String>,
        #[serde(default)]
        home: Option<[f64; 2]>,
    }
    let fleet: BTreeMap<u64, Entry> = serde_json::from_slice(bytes).ok()?;
    Some(
        fleet
            .into_iter()
            .filter(|(_, v)| v.platform.as_ref().is_some_and(|p| armed.contains(p)))
            .map(|(uid, v)| OwedRestore {
                uid: Uid::new(uid.to_string()),
                values: BTreeMap::new(),
                by: Vec::new(),
                vehicle: Some(engage::VehicleIdentity {
                    platform: v.platform,
                    callsign: v.callsign,
                    sysid: None,
                    home: v.home.map(|[lat, lon]| crate::geo::LonLat::new(lon, lat)),
                }),
            })
            .collect(),
    )
}

/// Rules 15: the fleet file beside a log (`<log stem>.fleet.json`), by its bare file name.
pub fn fleet_file_name(log: &str) -> String {
    match log.strip_suffix(".jsonl") {
        Some(stem) => format!("{stem}.fleet.json"),
        None => format!("{log}.fleet.json"),
    }
}

impl Debts {
    pub fn is_empty(&self) -> bool {
        self.owed.is_empty() && self.pending.is_empty()
    }
}

pub fn owed_restores(text: &str) -> Vec<OwedRestore> {
    owed_debts(text).owed
}

/// Rules 12: [`owed_restores`], and the earlier logs `text` still owed for (pending).
pub fn owed_debts(text: &str) -> Debts {
    let mut restores = Restores::default();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        if let Ok(e) = serde_json::from_str::<Event>(line) {
            restores.step(&e);
        }
    }
    Debts {
        pending: restores.pending.keys().cloned().collect(),
        pending_may_owe: restores
            .pending
            .iter()
            .filter(|(_, p)| !p.may_owe.is_empty())
            .map(|(l, p)| (l.clone(), p.may_owe.clone()))
            .collect(),
        spent: restores.spent(),
        owed: restores
            .owed
            .into_iter()
            .map(|(uid, o)| OwedRestore {
                uid,
                values: o.values,
                by: o.by,
                vehicle: o.vehicle,
            })
            .collect(),
    }
}

/// Rules 12: the head of a log's text as the bridge records it in `carried_from`: the SHA-256
/// of its last line that parses as an event, and that event's seq (a torn last line, from a
/// crash mid-write, is not part of it). None for a log with no event.
pub fn log_head(text: &str) -> Option<(String, u64)> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Event>(l).ok().map(|e| (sha256_hex(l.as_bytes()), e.seq)))
        .next_back()
}

/// Rules 6: the earlier log a carried `platform.restore` names: its `from` less the
/// parenthesis the bridge adds at the end (`<file> (preloaded by i3)`, `<file> (carried
/// over into it)`; a file name may itself hold " ("); the whole `from` when it has neither;
/// empty when it names none.
pub fn carried_restore_log(from: Option<&str>) -> &str {
    let from = from.unwrap_or_default();
    if let Some(file) = from.strip_suffix(" (carried over into it)") {
        return file;
    }
    match from.rsplit_once(" (preloaded by ") {
        Some((file, by)) if by.ends_with(')') => file,
        _ => from,
    }
}

/// Whether `name` is a bare file name, as the bridge names an earlier log (in
/// `intent.resumed` and a carried `platform.restore`): not empty, not `.` or `..`, no path
/// separator. A name that is a path is never followed out of the log's directory.
pub fn is_log_file_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\'])
}

/// Rules 6: the earlier logs a log's carried `platform.restore` events name (normally one:
/// the log `.last-run-<port>` named when the bridge started), in order of first mention.
/// Lines that do not parse are skipped.
pub fn carried_restore_logs(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for line in text.lines().filter(|l| l.contains("\"type\":\"platform.restore\"")) {
        if let Ok(Event {
            body: EventBody::PlatformRestore { carried: true, from, .. },
            ..
        }) = serde_json::from_str::<Event>(line)
        {
            let name = carried_restore_log(from.as_deref());
            if !names.iter().any(|n| n == name) {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// Rules 6: the `from` the bridge logs on values it carries over from `file` (its
/// `carry_from`): the engagements that preloaded them there, or that that log had carried
/// them over itself.
fn carried_from(file: &str, by: &[IntentId]) -> String {
    if by.is_empty() {
        format!("{file} (carried over into it)")
    } else {
        let by: Vec<String> = by.iter().map(ToString::to_string).collect();
        format!("{file} (preloaded by {})", by.join(", "))
    }
}

/// Rules 6: an engagement (other than `except`) under way on vehicle `uid`: it holds the
/// platform's preload, so no restore of its own values may run meanwhile.
fn holding(intents: &BTreeMap<IntentId, Tracked>, uid: &Uid, except: Option<IntentId>) -> Option<IntentId> {
    intents
        .iter()
        .find(|(id, tr)| {
            Some(**id) != except
                && tr.spec.kind() == TaskKind::Engage
                && tr.state == State::Dispatched
                && tr.spec.uids.first() == Some(uid)
        })
        .map(|(id, _)| *id)
}

/// Rules 6: a `platform.restore`: carried values become owed (equal to any already owed);
/// an attempt must put back the values owed, never while an engagement holds the platform,
/// and its `confirmed` is recomputed.
fn platform_restore(
    r: &mut ReplayReport,
    e: &Event,
    restores: &mut Restores,
    intents: &BTreeMap<IntentId, Tracked>,
    followups: bool,
) {
    let EventBody::PlatformRestore {
        uid,
        values,
        carried,
        readback,
        error,
        confirmed,
        ..
    } = &e.body
    else {
        return;
    };
    let owed = restores.owed.get(uid).map(|o| &o.values);
    if *carried {
        // rules 12: what an earlier run owed is carried in run.started, never event by event
        if followups {
            bad(
                r,
                e,
                None,
                "a carried platform.restore under rules that carry in run.started (carried_from)",
                uid.to_string(),
                "run.started's carried_from".into(),
            );
        }
        if owed.is_some_and(|v| v != values) || *confirmed || !readback.is_empty() {
            bad(r, e, None, "carried restore differs from the values owed", json(values), json(&owed));
        }
        let vehicle = match &e.body {
            EventBody::PlatformRestore { vehicle, .. } => vehicle.as_deref().cloned(),
            _ => None,
        };
        let from = match &e.body {
            EventBody::PlatformRestore { from, .. } => Some(carried_restore_log(from.as_deref()).to_string()),
            _ => None,
        };
        restores.owed.entry(uid.clone()).or_insert_with(|| Owed {
            values: values.clone(),
            by: Vec::new(),
            vehicle,
            from,
        });
        return;
    }
    if let Some(other) = holding(intents, uid, None) {
        bad(
            r,
            e,
            Some(other),
            "restore of the platform's own values while an engagement holds it",
            uid.to_string(),
            "no engagement under way on the platform".into(),
        );
    }
    if owed != Some(values) {
        bad(r, e, None, "restore of values not owed back to the platform", json(values), json(&owed));
    }
    let verdict = match error {
        Some(why) => Err(why.clone()),
        None => engage::restored(values, readback),
    };
    if verdict.is_ok() != *confirmed {
        bad(r, e, None, "restore confirmation differs from its read-back", confirmed.to_string(), json(&verdict));
    }
    if *confirmed {
        restores.paid(uid, readback);
    }
}

struct Tracked {
    spec: IntentSpec,
    state: State,
    /// When it reached a terminal state (rules 15: an un-spend comes after the engagement that
    /// spent the platform ended).
    ended_t: Option<Time>,
    last_check_passed: Option<bool>,
    tier: Option<Tier>,
    /// The recomputed decision's rule name and `submitter_authorizes`.
    rule: Option<String>,
    submitter_authorizes: bool,
    expires_t: Option<Time>,
    granted: bool,
    granted_at_submission: bool,
    rejected_by_human: bool,
    expired: bool,
    /// The space answer the policy decision read (a collect's deadline comes from it).
    answer: Option<Box<SpaceAnswer>>,
    /// The recomputed decision's signature count and proposer exclusion.
    approvers: u32,
    proposer_excluded: bool,
    /// Rules 8: the recomputed decision asks every signature for a step-up.
    step_up: bool,
    /// Who signed so far (concurred, then granted).
    signers: Vec<OperatorId>,
    /// The inputs of the last check (an engagement's snapshot is recomputed from them).
    last_inputs: Option<Box<WorldSnapshot>>,
    snapshot: Option<Box<EngageSnapshot>>,
    /// A failed `engage.picture` not yet followed by its `engage.stopped` (seq).
    stop_due: Option<u64>,
    /// The last engage event of this intent was a picture that held and was ready.
    ready: bool,
    /// `engage.effect` logged, waiting for its `release` transition.
    effect_logged: bool,
    effect: Option<EffectRecord>,
    /// `engage.stopped` logged, waiting for its void or abort.
    stopped: bool,
    covered_s: f64,
    last_observe: Option<(Time, bool)>,
    last_step: Option<(Time, ObserveStep)>,
    assessed: bool,
    /// Engage: the plan the first signature froze (dropped on `approval.reset`).
    plan: Option<FrozenPlan>,
    /// The last check.result's time and the time of the event before it: a signature's
    /// `checked_t` lies between them.
    check_window: Option<(Time, Time)>,
    /// Engage: when the picture was last checked (the final approval, then each
    /// `engage.picture`).
    last_picture_t: Option<Time>,
    /// Rules 9: the last `engage.picture`'s world as logged (what a revoked declaration
    /// changes for a bystander).
    last_picture_world: Option<WorldSnapshot>,
    /// An `intent.dispatched` waiting for its dispatch transition (its seq).
    dispatch_logged: Option<u64>,
    /// The dispatch transition happened: op states may refer to the intent from here on.
    dispatched: bool,
    /// Engage: the world time of the last picture, if it held and was ready (the release
    /// must follow it, inside the plan's time tolerance).
    ready_t: Option<Time>,
    /// Engage: the stop, with the fallback it released (`Some(None)`: none).
    stop_fallback: Option<Option<Fallback>>,
    /// Engage: vehicle events its platform reported since its last picture (kind, text), the
    /// ones a stop of this tick may quote.
    platform_events: Vec<(String, String)>,
    /// Engage: its intercept's reports since its last picture (state, detail), the ones a
    /// stop for a failed intercept quotes.
    intercept_reports: Vec<(String, String)>,
    /// Engage: the loiter radius from the final approval's world (its order carries it).
    loiter_radius_m: Option<f64>,
    /// Engage, rules 6: the lost-link preload from the final approval's world (its order
    /// carries it), the platform's `set` report (its own values before, confirmed), whether a
    /// restore confirmed, and an unconfirmed `set` whose stop is due (seq).
    preload: Option<engage::LostLinkPreload>,
    preload_set: Option<(BTreeMap<String, f64>, bool)>,
    restore_confirmed: bool,
    /// Rules 6: the preload's write-ahead (`intent`): the platform's own values it logged.
    preload_intent: Option<BTreeMap<String, f64>>,
    /// Rules 6: the latest `set` or `verify` read-back of the preload: its time, confirmed.
    preload_read: Option<(Time, bool)>,
    /// the unconfirmed preload report a stop must follow: its seq and the stop's reason prefix
    preload_stop_due: Option<(u64, &'static str)>,
    /// Engage, rules 6: our vehicles operators accepted as lost for it (`engage.accepted`).
    accepted: BTreeMap<Uid, engage::AcceptedUnknown>,
    /// Engage, rules 16: the lost contacts operators accepted for it.
    accepted_lost: BTreeMap<String, engage::AcceptedUnknown>,
    /// Engage, rules 19: the live tracks operators accepted for it as behaving now, each with its
    /// facts then (dropped by a revocation, a lapse or a reset).
    accepted_behaving: BTreeMap<String, engage::AcceptedBehaving>,
    /// Collect and cover: the order it was dispatched with (what a cancel's and a re-check's
    /// reports are recomputed against).
    space_order: Option<SpaceOrder>,
    /// Rules 11: an operator's cancel of this dispatched space task (who, seq) waiting for
    /// the engine's cancelled report and the cancel transition; the report came.
    cancel_due: Option<(OperatorId, u64)>,
    cancel_reported: bool,
    /// Rules 11: a dispatched-phase check (passed, the report it calls for) waiting for its
    /// `rechecked` or `missed` report; a `missed` report (its detail) waiting for the failure.
    recheck_due: Option<(bool, String)>,
    recheck_lost: Option<String>,
    /// Rules 11: the answer a space task was dispatched on (its request time, its segment):
    /// a re-check keeps the request time and comes from another segment.
    dispatch_now: Option<Time>,
    last_segment: Option<crate::space::SegmentId>,
    /// The last check's recomputed results, its phase and seq.
    last_results: Vec<CheckResult>,
    last_phase: Option<CheckPhase>,
    last_check_seq: u64,
    /// The last recomputed policy decision (submission, or a queue re-check's).
    decision: Option<PolicyDecision>,
    /// Rules 5: what the approval rested on (taken at approved / auto_approved).
    basis: Option<Basis>,
    /// Who approved it, as a next run would carry it.
    approval_rec: Option<CarriedApproval>,
    /// Rules 5: the queue deadline, an `intent.queued` waiting for its transition, the last
    /// re-check's verdict waiting for its own, a `queue.expired` waiting for the expiry.
    queue_expires: Option<Time>,
    queued_logged: Option<u64>,
    recheck: Option<Verdict>,
    queue_expired: bool,
    /// The deadline sends it back to the inbox (a human's approval held long enough), and
    /// a `queue.expired` that did so waits for its transition.
    queue_inbox: bool,
    queue_expired_inbox: bool,
    /// The last re-check's seq (a dispatch from the queue follows it at once).
    recheck_seq: Option<u64>,
    /// An `intent.requeued` waiting for its transition.
    requeued_logged: Option<u64>,
    /// Each vehicle's last op state (since its last dispatch).
    ops: BTreeMap<Uid, String>,
    /// The vehicles that ever started it (this run or, carried over, one before).
    committed: BTreeSet<Uid>,
    /// Dispatched before (this run or one before).
    dispatched_before: bool,
    /// Its basis is from the run before a bridge restart.
    basis_carried: bool,
    /// Each vehicle's platform as its last check saw it (or as carried over).
    platforms: BTreeMap<Uid, String>,
    /// The vehicles that may have started it (an op `starting` or `unconfirmed`), and those
    /// its current delivery only confirms.
    maybe_started: BTreeSet<Uid>,
    confirming: BTreeSet<Uid>,
    /// Rules 7: the vehicles whose start (or follow) on the current delivery was logged ahead.
    started_ahead: BTreeSet<Uid>,
    /// The vehicles whose op on this delivery (since its dispatch in this log, or its resume:
    /// the vehicles that had started it, followed) reported its start going out (`starting`,
    /// `unconfirmed`, or started): its op never ends `requeued` after that (the bridge ends
    /// it `unconfirmed`: it may be running; a followed one fails).
    start_went_out: BTreeSet<Uid>,
}

impl Tracked {
    fn new(spec: IntentSpec, state: State) -> Tracked {
        Tracked {
            spec,
            state,
            ended_t: None,
            last_check_passed: None,
            tier: None,
            rule: None,
            submitter_authorizes: false,
            expires_t: None,
            granted: false,
            granted_at_submission: false,
            rejected_by_human: false,
            expired: false,
            answer: None,
            approvers: 1,
            proposer_excluded: false,
            step_up: false,
            signers: Vec::new(),
            last_inputs: None,
            snapshot: None,
            stop_due: None,
            ready: false,
            effect_logged: false,
            effect: None,
            stopped: false,
            covered_s: 0.0,
            last_observe: None,
            last_step: None,
            assessed: false,
            plan: None,
            check_window: None,
            last_picture_t: None,
            last_picture_world: None,
            dispatch_logged: None,
            dispatched: false,
            ready_t: None,
            stop_fallback: None,
            platform_events: Vec::new(),
            intercept_reports: Vec::new(),
            loiter_radius_m: None,
            last_results: Vec::new(),
            last_phase: None,
            last_check_seq: 0,
            decision: None,
            basis: None,
            approval_rec: None,
            queue_expires: None,
            queued_logged: None,
            recheck: None,
            queue_expired: false,
            queue_inbox: false,
            queue_expired_inbox: false,
            recheck_seq: None,
            requeued_logged: None,
            ops: BTreeMap::new(),
            committed: BTreeSet::new(),
            dispatched_before: false,
            basis_carried: false,
            platforms: BTreeMap::new(),
            maybe_started: BTreeSet::new(),
            confirming: BTreeSet::new(),
            started_ahead: BTreeSet::new(),
            start_went_out: BTreeSet::new(),
            preload: None,
            preload_set: None,
            restore_confirmed: false,
            preload_intent: None,
            preload_read: None,
            preload_stop_due: None,
            accepted: BTreeMap::new(),
            accepted_lost: BTreeMap::new(),
            accepted_behaving: BTreeMap::new(),
            space_order: None,
            cancel_due: None,
            cancel_reported: false,
            recheck_due: None,
            recheck_lost: None,
            dispatch_now: None,
            last_segment: None,
        }
    }

    /// Back to the inbox from the queue: who approves is decided from scratch.
    fn back_to_inbox(&mut self) {
        self.signers.clear();
        self.granted = false;
        self.granted_at_submission = false;
        self.expires_t = None;
        self.approval_rec = None;
    }

    /// The decision's routing on this intent: tier, rule, approvers, who may approve.
    fn route(&mut self, d: &PolicyDecision) {
        self.tier = Some(d.tier);
        self.submitter_authorizes = d.submitter_authorizes;
        self.approvers = d.approvers.max(1);
        self.proposer_excluded = d.proposer_excluded;
        self.step_up = d.step_up;
        self.rule = Some(d.rule.clone());
        self.decision = Some(d.clone());
    }
}

/// Which decision rules a replay accepts a log under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RulesAccepted {
    /// Exactly this version (`murmur-replay --rules N`).
    Exactly(u32),
    /// This version or a later one (`--min-rules N`; the default is the current version).
    AtLeast(u32),
}

impl Default for RulesAccepted {
    fn default() -> Self {
        RulesAccepted::AtLeast(crate::rules::RULES_VERSION)
    }
}

/// How to replay.
#[derive(Clone, Copy, Default)]
pub struct ReplayOptions<'a> {
    /// The rules versions accepted (default: only the current one).
    pub rules: RulesAccepted,
    /// Re-run every logged space query on this segment (see the module docs).
    pub space: Option<&'a dyn SpaceOracle>,
    /// Rules 5: the previous run's log, which this one's `intent.resumed` events carry
    /// intents from. Each resumed intent must be one that log leaves open, exactly as it
    /// was there, and the log must be the one named (its head and last seq). One generation:
    /// that log's own resumed intents are trusted (replay it with its own predecessor).
    pub previous: Option<&'a str>,
    /// Accept what this log carried over from earlier runs without their logs: its
    /// `intent.resumed` events (without `previous`) and, rules 6, its carried
    /// `platform.restore` values (without `restores_from`), trusting where they came from, as
    /// accepting an older rules version trusts its log. Without this, a carry-over whose
    /// earlier log is not given is a mismatch. A carried restore that names no log by its
    /// file name is a mismatch either way.
    pub trust_resumed: bool,
    /// Rules 6: the earlier log this run's carried `platform.restore` events name, as (its
    /// file name as they name it, its text). It is the log `.last-run-<port>` named when the
    /// bridge started, which need not be `previous` (`.resume-<port>.json`, deleted by a
    /// clean stop, or `MURMUR_RESUME=<log>`, may name another log or none). Each carried
    /// restore must be exactly what that log still owed ([`owed_restores`]), everything it
    /// owed carried, and that log must replay itself (its own carry-overs trusted: one
    /// generation). Carried values from a log not given are a mismatch unless
    /// [`Self::trust_resumed`] ([`ReplayReport::carried_restores`] says which were checked).
    pub restores_from: Option<(&'a str, &'a str)>,
    /// Rules 12: the earlier logs this run's `run.started` says it carried over from
    /// (`carried_from`), as (their file name, their text): each must replay, be the one read
    /// (its head) and still have owed exactly what was carried. [`Self::restores_from`] is
    /// looked at too. A log not given is a mismatch when it carried anything, unless
    /// [`Self::trust_resumed`].
    pub carried_logs: &'a [(&'a str, &'a str)],
    /// Rules 15: the fleet files of the logs whose carry failed (`<log stem>.fleet.json`), as
    /// (their file name, their bytes): each must be the one the start read (its SHA-256), and
    /// the platforms the failed carry may owe values to exactly that fleet's armed ones.
    pub fleets: &'a [(&'a str, &'a [u8])],
}

/// Replay a JSONL log. The chain is checked over the lines exactly as they are in the
/// text, and each line must be its event's canonical serialization (so an edit that the
/// parser would ignore, such as an extra field, is still caught).
pub fn replay_str(text: &str) -> Result<ReplayReport, LogError> {
    replay_str_with(text, &ReplayOptions::default())
}

/// [`replay_str`], and re-run every logged space query on `space` (see the module docs).
pub fn replay_str_with_space(
    text: &str,
    space: Option<&dyn SpaceOracle>,
) -> Result<ReplayReport, LogError> {
    replay_str_with(
        text,
        &ReplayOptions {
            space,
            ..ReplayOptions::default()
        },
    )
}

/// [`replay_str`] with options.
pub fn replay_str_with(text: &str, opts: &ReplayOptions<'_>) -> Result<ReplayReport, LogError> {
    let events = parse_log(text)?;
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut noncanonical = Vec::new();
    for (e, line) in events.iter().zip(&lines) {
        let canon = serde_json::to_string(e).map_err(|err| LogError {
            line: usize::try_from(e.seq).unwrap_or(0),
            err,
        })?;
        if canon != *line {
            noncanonical.push(e.seq);
        }
    }
    let mut r = replay_inner(&events, &lines, opts);
    for seq in noncanonical {
        r.mismatches.push(Mismatch {
            seq,
            intent: None,
            what: "line is not its event's canonical serialization".into(),
            logged: String::new(),
            recomputed: String::new(),
        });
    }
    Ok(r)
}

/// Replay parsed events; the chain is checked over their canonical serialization.
pub fn replay(events: &[Event]) -> ReplayReport {
    replay_with(events, &ReplayOptions::default())
}

/// [`replay`], and re-run every logged space query on `space`.
pub fn replay_with_space(events: &[Event], space: Option<&dyn SpaceOracle>) -> ReplayReport {
    replay_with(
        events,
        &ReplayOptions {
            space,
            ..ReplayOptions::default()
        },
    )
}

/// [`replay`] with options.
pub fn replay_with(events: &[Event], opts: &ReplayOptions<'_>) -> ReplayReport {
    let lines: Vec<String> = events
        .iter()
        .map(|e| serde_json::to_string(e).unwrap_or_else(|err| format!("<unserializable: {err}>")))
        .collect();
    replay_inner(events, &lines, opts)
}

/// Fields and checks only builds of a later rules version write, with that version: a log
/// claiming earlier rules can't carry them (a downgraded claim). Matched on the lines as
/// written (a key in quotes followed by a colon can't occur inside a JSON string value).
const RULES_MARKERS: &[(u32, &str, &str)] = &[
    (2, "\"checked_t\":", "a signature's checked_t"),
    (2, "\"gps_fix\":", "a vehicle's GPS fix fact"),
    (2, "\"gps_glitch\":", "a vehicle's GPS glitch fact"),
    (2, "\"rule\":\"platform_free\"", "a platform_free check"),
    (3, "\"gps_fix_from\":", "a GPS fix inferred from HIGH_LATENCY2"),
    (3, "\"circling\":", "a vehicle's circle"),
    (4, "\"loiter_radius_m\":", "an engagement order's loiter radius"),
    (5, "\"type\":\"intent.queued\"", "a queued intent"),
    (5, "\"type\":\"queue.recheck\"", "a queue re-check"),
    (5, "\"type\":\"queue.expired\"", "a queue expiry"),
    (5, "\"type\":\"intent.resumed\"", "an intent carried over from a previous run"),
    (5, "\"type\":\"vehicle.rebooted\"", "a vehicle reboot"),
    (5, "\"booted_t\":", "a vehicle's boot instant"),
    (5, "\"phase\":\"dequeue\"", "a dequeue check"),
    (5, "\"queue\":{", "a [queue] policy"),
    (5, "\"type\":\"intent.requeued\"", "an intent sent back to the queue"),
    (5, "\"waiting\":", "a human order waiting in the queue (a decision's fact)"),
    (5, "\"reach\":", "a vehicle's reach"),
    (6, "\"rule\":\"fallback_preload\"", "a fallback_preload check"),
    (6, "\"preload\":", "an engagement order's lost-link preload"),
    (6, "\"type\":\"engage.preload\"", "a lost-link preload report"),
    (6, "\"type\":\"engage.accepted\"", "an acceptance of a lost vehicle"),
    (6, "\"type\":\"engage.acceptance_revoked\"", "a revoked acceptance of a lost vehicle"),
    (6, "\"type\":\"platform.restore\"", "a platform's own values owed back"),
    (6, "\"unknown_accepted\":", "an accepted lost vehicle in decision inputs"),
    (6, "\"alt_msl_m\":", "a vehicle's altitude fact"),
    (6, "\"climb_mps\":", "a platform's climb rate"),
    (6, "\"max_mps\":", "a platform's top speed"),
    (6, "\"alt_t\":", "a track altitude's time"),
    (6, "\"source_t\":", "a track source's last hit"),
    (rules::QUEUE_FOLLOWUPS_SINCE, "\"restore_owed\":", "a vehicle's lost-link values owed back (a decision's fact)"),
    (rules::QUEUE_FOLLOWUPS_SINCE, "\"ahead\":", "a vehicle start logged ahead of sending it"),
    (RULE_ID_SINCE, "\"type\":\"ident.act\"", "an observed hostile act"),
    (RULE_ID_SINCE, "\"type\":\"ident.declared\"", "a declaration in the identification ledger"),
    (RULE_ID_SINCE, "\"type\":\"ident.confirmed\"", "a confirmed rule declaration"),
    (RULE_ID_SINCE, "\"type\":\"ident.revoked\"", "a revoked declaration"),
    (RULE_ID_SINCE, "\"type\":\"ident.flagged\"", "a review flag"),
    (RULE_ID_SINCE, "\"type\":\"ident.flag_resolved\"", "a resolved review flag"),
    (RULE_ID_SINCE, "\"declaration\":", "a declaration's basis (a track's facts, a picture token)"),
    (RULE_ID_SINCE, "\"review_flags\":", "a track's review flags"),
    (RULE_ID_SINCE, "\"kind\":\"revoked\"", "an engagement stopped for a revoked declaration"),
    (RULE_ID_SINCE, "\"identification\":", "an [identification] policy"),
    (RULE_ID_SINCE, "\"manoeuvre\":", "a track's manoeuvre (keep-out reach)"),
    (RULE_ID_SINCE, "\"mean_velocity_mps\":", "a track's mean velocity (keep-out reach)"),
    (OPERATOR_AUTH_SINCE, "\"auth\":\"", "how an operator was authenticated"),
    (OPERATOR_AUTH_SINCE, "\"step_up\":", "a step-up (a credential re-entered to sign)"),
    (OPERATOR_AUTH_SINCE, "\"action\":\"session.closed\"", "a session's end"),
    (OPERATOR_AUTH_SINCE, "\"type\":\"auth.refused\"", "a refused login"),
    (OPERATOR_AUTH_SINCE, "\"step_up_acts\":", "acts other than signatures that step up"),
    (PASSKEY_SINCE, "\"auth\":\"passkey\"", "a passkey login or step-up"),
    (PASSKEY_SINCE, "\"passkey\":{", "a passkey assertion"),
    (PASSKEY_SINCE, "\"passkeys\":{", "the run's relying party and passkeys"),
    (PASSKEY_SINCE, "\"study\":{", "a study run's participant"),
    (PASSKEY_SINCE, "\"type\":\"passkey.registered\"", "a passkey registered"),
    (PASSKEY_SINCE, "\"type\":\"passkey.removed\"", "a passkey removed"),
    (PASSKEY_SINCE, "\"step_up_factor\":", "a policy's step-up factor"),
    (PASSKEY_SINCE, "\"login_factor\":", "a policy's login factor"),
    (rules::SPACE_FOLLOWUPS_SINCE, "\"query\":\"cover_links\"", "a SATCOM cover evaluated over the area's sample points"),
    (rules::SPACE_FOLLOWUPS_SINCE, "\"query\":\"cover_gnss\"", "a GNSS cover evaluated over the area's sample points"),
    (rules::SPACE_FOLLOWUPS_SINCE, "\"transition\":\"cancel\"", "a space task cancelled after dispatch"),
    (rules::SPACE_FOLLOWUPS_SINCE, "\"state\":\"cancelled\"", "a space task's cancelled report"),
    (rules::SPACE_FOLLOWUPS_SINCE, "\"phase\":\"dispatched\"", "a space task re-checked after dispatch"),
    (rules::SPACE_FOLLOWUPS_SINCE, "\"state\":\"rechecked\"", "a space task's re-check report"),
    (rules::REVIEW_FOLLOWUPS_SINCE, "\"carried_from\":[", "what a start carried over from earlier runs, in run.started"),
    (rules::REVIEW_FOLLOWUPS_SINCE, "\"type\":\"restore.closed\"", "a restore closed without a restore (expended, written off)"),
    (rules::REVIEW_FOLLOWUPS_SINCE, "\"scripted\":[", "the run's scripted operators on the roster"),
    (rules::REVIEW_FOLLOWUPS_SINCE, "\"first_seen_t\":", "when the tracker started a track (keep-out reach)"),
    (rules::REVIEW_FOLLOWUPS_SINCE, "\"carried\":{", "values carried over holding a vehicle (a decision's fact)"),
    (rules::REVIEW_FOLLOWUPS_SINCE, "\"expended\":{", "a platform spent by its own effect (a decision's fact)"),
    (rules::REVIEW_FOLLOWUPS_SINCE, "\"may_owe\":[", "the platforms a failed carry may owe values to"),
    (rules::SPACE_REPLAY_SINCE, "\"computed_t\":", "when a space answer was computed (a decision's fact)"),
    (rules::AIRCRAFT_REACH_SINCE, "\"velocity_sigma_mps\":", "a track's velocity error (an aircraft's reach)"),
    (rules::CARRY_FOLLOWUPS_SINCE, "\"type\":\"platform.unspent\"", "a platform un-spent by an operator"),
    (rules::CARRY_FOLLOWUPS_SINCE, "\"spent\":[", "the platforms an earlier log had spent, carried over"),
    (rules::CARRY_FOLLOWUPS_SINCE, "\"spent_carried\":{", "a platform spent in an earlier run (a decision's fact)"),
    (rules::CARRY_FOLLOWUPS_SINCE, "\"fleet_read\":{", "the fleet file a failed carry read"),
    (rules::CARRY_FOLLOWUPS_SINCE, "\"pointer_failed\":", "a carry that fell back from a pointer that could not be read"),
    (rules::CARRY_FOLLOWUPS_SINCE, "\"pointer\":\"", "the station's .last-run pointer in run.started"),
    (rules::LOST_CONTACT_SINCE, "\"type\":\"track.dropped\"", "a track the tracker dropped (a lost contact)"),
    (rules::LOST_CONTACT_SINCE, "\"type\":\"track.reacquired\"", "a lost contact confirmed as a live track"),
    (rules::LOST_CONTACT_SINCE, "\"type\":\"lost.claimed\"", "a claim on a lost contact"),
    (rules::LOST_CONTACT_SINCE, "\"type\":\"lost.reinstated\"", "a lost contact counting again"),
    (rules::LOST_CONTACT_SINCE, "\"track_ais\":{", "the AIS identities heard for tracks"),
    (rules::LOST_CONTACT_SINCE, "\"lost_contacts\":{", "lost contacts in decision inputs"),
    (rules::LOST_CONTACT_SINCE, "\"lost_accepted\":{", "an accepted lost contact (decision inputs, a picture token)"),
    (rules::LOST_CONTACT_SINCE, "\"contact\":\"", "an acceptance of a lost contact"),
    (rules::LOST_CONTACT_SINCE, "\"ais\":{", "a lost contact's AIS identity"),
    (rules::LOST_CONTACT_SINCE, "\"track_drop_s\":", "the tracker's drop age in run.started"),
    (rules::ROUTE_SURFACE_SINCE, "\"along\":[", "land/water answers along a route's legs (a decision's terrain)"),
    (rules::ROUTE_SURFACE_SINCE, "\"warning\":\"", "a check's warning (a safety command's way crossing the wrong surface)"),
    (rules::ACCEPT_BEHAVING_SINCE, "\"type\":\"engage.acceptance_lapsed\"", "a lapsed acceptance of a live track as behaving now"),
    (rules::ACCEPT_BEHAVING_SINCE, "\"behaving_accepted\":{", "a live track accepted as behaving now (decision inputs, a picture token)"),
    (rules::ACCEPT_BEHAVING_SINCE, "\"behaving\":{", "an acceptance of a live track as behaving now"),
];

/// The first `run.started`'s claims, and everything in the log that contradicts them.
fn check_claims<S: AsRef<str>>(r: &mut ReplayReport, events: &[Event], lines: &[S], opts: &ReplayOptions<'_>) {
    let Some(first) = events.first() else { return };
    let EventBody::RunStarted {
        rules_version,
        policy_schema,
        build,
        roster,
        policy,
        ..
    } = &first.body
    else {
        let kind = serde_json::to_value(&first.body)
            .ok()
            .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_string))
            .unwrap_or_default();
        bad(r, first, None, "the log does not start with run.started", kind, "run.started".into());
        return;
    };
    let version = rules_version.unwrap_or(1);
    match opts.rules {
        RulesAccepted::Exactly(n) if version != n => bad(
            r,
            first,
            None,
            "log written under other decision rules than this replay was asked to accept",
            version.to_string(),
            n.to_string(),
        ),
        RulesAccepted::AtLeast(n) if version < n => bad(
            r,
            first,
            None,
            "log written under older decision rules than this replay accepts (pass --rules or --min-rules with its version to replay it with them)",
            version.to_string(),
            format!(">= {n}"),
        ),
        _ => {}
    }
    // what only later builds write
    let mut floor: Vec<(u32, u64, &str)> = Vec::new();
    if rules_version.is_some() || policy_schema.is_some() {
        floor.push((2, first.seq, "rules_version and policy_schema in run.started"));
    }
    if rules_version.is_some() != policy_schema.is_some() {
        bad(
            r,
            first,
            None,
            "run.started carries one of rules_version and policy_schema without the other (every build writes both or neither)",
            format!("rules_version {rules_version:?}, policy_schema {policy_schema:?}"),
            String::new(),
        );
    }
    if build.is_some() {
        floor.push((3, first.seq, "the build in run.started"));
    }
    // the operator roster: written from rules 4, and every rules 4 run has one (a log that
    // dropped it would have its sessions go unchecked)
    match roster {
        Some(listed) => {
            floor.push((4, first.seq, "the operator roster in run.started"));
            if let Err(why) = listed.validate() {
                bad(
                    r,
                    first,
                    None,
                    "logged roster is not one a bridge could be configured with",
                    why,
                    String::new(),
                );
            }
        }
        None if version >= 4 => bad(
            r,
            first,
            None,
            "a rules 4 log names its operator roster in run.started",
            "none".into(),
            "roster".into(),
        ),
        None => {}
    }
    // rules 12: a credentials run fixes its scripted operators at the start, always (an empty
    // list when none), so replay can check every session's method against them
    if version >= rules::REVIEW_FOLLOWUPS_SINCE
        && let Some(listed) = roster
        && listed.auth == Some(AuthMode::Credentials)
        && listed.scripted.is_none()
    {
        bad(
            r,
            first,
            None,
            "a credentials log of these rules names its scripted operators on the roster",
            "none".into(),
            "scripted: [...]".into(),
        );
    }
    // how operators were authenticated: written from rules 8 (OPERATOR_AUTH_SINCE), always
    r.auth = roster.as_ref().and_then(|ro| ro.auth);
    if version >= OPERATOR_AUTH_SINCE && r.auth.is_none() {
        bad(
            r,
            first,
            None,
            "a log of these rules says how its operators were authenticated (the roster's auth)",
            "none".into(),
            "credentials or dev".into(),
        );
    }
    for &(since, needle, what) in RULES_MARKERS {
        if since <= version {
            continue;
        }
        if let Some(i) = lines.iter().position(|l| l.as_ref().contains(needle)) {
            floor.push((since, events.get(i).map_or(0, |e| e.seq), what));
        }
    }
    for (since, seq, what) in floor {
        if since > version {
            r.mismatches.push(Mismatch {
                seq,
                intent: None,
                what: format!("{what}: only builds of rules {since} or later write it, the log claims rules {version}"),
                logged: version.to_string(),
                recomputed: format!(">= {since}"),
            });
        }
    }
    // the policy file's schema
    let schema = policy_schema.unwrap_or(1);
    if schema > crate::rules::POLICY_SCHEMA {
        bad(
            r,
            first,
            None,
            "logged policy schema is newer than this replay knows",
            schema.to_string(),
            crate::rules::POLICY_SCHEMA.to_string(),
        );
    }
    if policy.queue.is_some() && schema < 3 {
        bad(
            r,
            first,
            None,
            "logged policy uses a key of a later policy schema than it claims",
            "queue (schema 3)".into(),
            format!("schema {schema}"),
        );
    }
    if let Some(t) = policy.tiers.iter().find(|t| t.step_up.is_some())
        && schema < STEP_UP_SCHEMA_SINCE
    {
        bad(
            r,
            first,
            None,
            "logged policy uses a key of a later policy schema than it claims",
            format!("tier.{}.step_up (schema {STEP_UP_SCHEMA_SINCE})", t.name),
            format!("schema {schema}"),
        );
    }
    if policy.approval.step_up_acts.is_some() && schema < STEP_UP_SCHEMA_SINCE {
        bad(
            r,
            first,
            None,
            "logged policy uses a key of a later policy schema than it claims",
            format!("approval.step_up_acts (schema {STEP_UP_SCHEMA_SINCE})"),
            format!("schema {schema}"),
        );
    }
    // rules 9: `[identification]`, and the step-up act it adds, are schema 6
    if policy.identification.is_some() && schema < RULE_ID_SCHEMA_SINCE {
        bad(
            r,
            first,
            None,
            "logged policy uses a key of a later policy schema than it claims",
            format!("identification (schema {RULE_ID_SCHEMA_SINCE})"),
            format!("schema {schema}"),
        );
    }
    if policy
        .approval
        .step_up_acts
        .as_ref()
        .is_some_and(|a| a.contains(&policy::StepUpAct::ConfirmDeclaration))
        && schema < RULE_ID_SCHEMA_SINCE
    {
        bad(
            r,
            first,
            None,
            "logged policy uses a key of a later policy schema than it claims",
            format!("approval.step_up_acts confirm_declaration (schema {RULE_ID_SCHEMA_SINCE})"),
            format!("schema {schema}"),
        );
    }
    // rules 10: the step-up and login factors are schema 7
    for (set, key) in [
        (policy.approval.step_up_factor.is_some(), "approval.step_up_factor"),
        (policy.approval.login_factor.is_some(), "approval.login_factor"),
    ] {
        if set && schema < PASSKEY_SCHEMA_SINCE {
            bad(
                r,
                first,
                None,
                "logged policy uses a key of a later policy schema than it claims",
                format!("{key} (schema {PASSKEY_SCHEMA_SINCE})"),
                format!("schema {schema}"),
            );
        }
    }
    // rules 10: the passkeys and the study on the roster are ones a run could have started with
    if version >= PASSKEY_SINCE
        && let Some(listed) = roster
    {
        let factor = policy.approval.step_up_factor.unwrap_or(policy::StepUpFactor::Password);
        if let Err(why) = listed.validate_passkeys(factor) {
            bad(r, first, None, "logged passkeys or study could not be a run's", why, String::new());
        }
    }
    if let Some(e) = &policy.rules.engage {
        for (since, key) in e.schema_keys() {
            if since > schema {
                bad(
                    r,
                    first,
                    None,
                    "logged policy uses a key of a later policy schema than it claims",
                    format!("rules.engage.{key} (schema {since})"),
                    format!("schema {schema}"),
                );
            }
        }
    }
}

/// The area set in force: its logged hash, and the set if that hash verified.
type InForce = Option<(String, Option<AreaSet>)>;

/// Logged space answers whose hash verified, by hash.
type Answers = BTreeMap<String, SpaceAnswer>;

fn replay_inner<S: AsRef<str>>(
    events: &[Event],
    lines: &[S],
    opts: &ReplayOptions<'_>,
) -> ReplayReport {
    let space = opts.space;
    let hashes: Vec<String> = lines.iter().map(|l| sha256_hex(l.as_ref().as_bytes())).collect();
    let mut r = ReplayReport {
        events: events.len(),
        head_sha256: hashes.last().cloned(),
        build: match events.first().map(|e| &e.body) {
            Some(EventBody::RunStarted { build, .. }) => build.clone(),
            _ => None,
        },
        ..Default::default()
    };
    check_claims(&mut r, events, lines, opts);
    let mut policy: Option<Policy> = None;
    let mut areas: InForce = None;
    let mut intents: BTreeMap<IntentId, Tracked> = BTreeMap::new();
    let mut answers: Answers = BTreeMap::new();
    let mut prev: Option<&Event> = None;
    // from the last `space.loaded`
    let mut scenario_map = ScenarioMap::IDENTITY;
    // rules 11: the segment in force (the last `space.loaded`), which a re-check must be on
    let mut current_segment: Option<crate::space::SegmentId> = None;
    let mut pinned: Option<i64> = None;
    // operator sessions as opened: who, with what grants
    let mut sessions: BTreeMap<SessionId, (OperatorId, Grants)> = BTreeMap::new();
    // rules 8: sessions ended (nothing may follow under one), and step-ups by their event's
    // seq
    let mut closed: BTreeSet<SessionId> = BTreeSet::new();
    let mut step_ups: BTreeMap<u64, StepUpSeen> = BTreeMap::new();
    // rules 8: how each session was opened (password, scripted, asserted)
    let mut methods: BTreeMap<SessionId, AuthMethod> = BTreeMap::new();
    // rules 6: the platforms' own lost-link values owed back
    let mut restores = Restores::default();
    // picture tokens issued and not used yet
    let mut issued: BTreeMap<u64, PictureToken> = BTreeMap::new();
    // who may open a session, and with engagement authority (run.started, rules 4 on)
    let mut roster: Option<Roster> = None;
    // the intent dispatched last, and per vehicle the one dispatched to it last
    let mut last_dispatch: Option<IntentId> = None;
    // rules 7: the intent queued last (it may supersede an engagement on its vehicles), and
    // the step in progress: the order dispatched or queued in it and the seq of its last event
    let mut last_queued: Option<IntentId> = None;
    let mut step: Option<(IntentId, u64)> = None;
    let mut newest_on: BTreeMap<Uid, IntentId> = BTreeMap::new();
    // intents carried over from a previous run, checked against it after the loop
    let mut resumed: Vec<ResumedEvent> = Vec::new();
    // when each vehicle was last logged rebooting
    let mut rebooted: BTreeMap<Uid, Time> = BTreeMap::new();
    // rules 9: the identification ledger as the log builds it, what an act's verdict makes
    // next, and the engagements a revocation stops next (with whether the current one's stop
    // was logged and waits for its transition)
    let mut ident = crate::ident::Ledger::default();
    let mut ident_due: Option<(u64, IdentDue)> = None;
    let mut revoke_due: Option<RevokeDue> = None;
    // rules 10: the passkeys the log knows (the roster's, registered since), each challenge's
    // nonce once answered, and the hash of the first line every challenge binds to
    let mut passkeys = PasskeysSeen {
        run_hash: hashes.first().cloned().unwrap_or_default(),
        ..PasskeysSeen::default()
    };
    // rules 16: the lost contacts as the log builds them, and the track ids decisions have read
    // and the tracker has dropped (a track leaves the picture only by a drop)
    let mut lost = crate::lost::LostLedger::default();
    let mut pictured = Pictured::default();
    // rules 16: the tracker's drop age the run logged (none: not checked)
    let track_drop_s: Option<f64> = events.first().and_then(|e| match &e.body {
        EventBody::RunStarted { track_drop_s, .. } => *track_drop_s,
        _ => None,
    });

    for (i, e) in events.iter().enumerate() {
        let want_prev = if i == 0 {
            GENESIS
        } else {
            hashes[i - 1].as_str()
        };
        if e.prev != want_prev {
            bad(
                &mut r,
                e,
                None,
                "hash chain broken: prev is not the hash of the previous event",
                e.prev.clone(),
                want_prev.to_string(),
            );
        }
        if let Some(p) = prev {
            if e.seq != p.seq + 1 {
                bad(
                    &mut r,
                    e,
                    None,
                    "seq not contiguous",
                    e.seq.to_string(),
                    (p.seq + 1).to_string(),
                );
            }
            if e.t < p.t {
                bad(
                    &mut r,
                    e,
                    None,
                    "time went backwards",
                    e.t.0.to_string(),
                    format!(">= {}", p.t.0),
                );
            }
        }
        let before_t = prev.map_or(Time(i64::MIN), |p| p.t);
        let prev_event: Option<&Event> = prev;
        prev = Some(e);
        // rules 8: an ended session acts no more (a stale or shared session can't sign, decide
        // or be shown a picture after its logout, expiry or credential change)
        if let Some(s) = acting_session(&e.body)
            && closed.contains(&s)
        {
            bad(&mut r, e, None, "an act under a session that had ended (session.closed)", s.0.to_string(), String::new());
        }
        // rules 9: an act's verdict is carried out by the very next event, and a revocation's
        // engagements are stopped next, before anything else
        if let Some((seq, due)) = ident_due.take() {
            let carried = match (&due, &e.body) {
                (IdentDue::Declare { .. }, EventBody::IdentDeclared { declaration })
                    if declaration.is_rule() =>
                {
                    true
                }
                (IdentDue::Flag { .. }, EventBody::IdentFlagged { inputs: None, .. }) => true,
                _ => false,
            };
            if carried {
                ident_due = Some((seq, due));
            } else {
                bad(
                    &mut r,
                    e,
                    None,
                    "a hostile act's verdict not carried out at once (its rule declaration or flag must follow it)",
                    event_type(&e.body),
                    format!("{} (act at seq {seq})", due.what()),
                );
            }
        }
        let mut revoke_stop = false;
        if let Some(due) = revoke_due.as_mut() {
            match due.step(&e.body) {
                Ok(stop) => revoke_stop = stop,
                Err(why) => {
                    bad(&mut r, e, None, &why, event_type(&e.body), json(&due.remaining));
                    revoke_due = None;
                }
            }
        }
        if revoke_due.as_ref().is_some_and(|d| d.remaining.is_empty() && d.stopping.is_none()) {
            revoke_due = None;
        }

        match &e.body {
            EventBody::RunStarted {
                policy: p,
                areas: a,
                rules_version,
                roster: listed,
                carried_from,
                ..
            } => {
                roster = listed.clone();
                // rules 12: what earlier runs still owed, owed back from here on (checked
                // against those logs after the loop)
                restores.carry_at(e.t, carried_from);
                if i > 0 {
                    bad(
                        &mut r,
                        e,
                        None,
                        "run.started is not the log's first event: a run can't start again inside a log",
                        e.seq.to_string(),
                        "1".into(),
                    );
                }
                let version = rules_version.unwrap_or(1);
                if version > crate::rules::RULES_VERSION {
                    bad(
                        &mut r,
                        e,
                        None,
                        "log written under newer decision rules than this replay knows",
                        version.to_string(),
                        crate::rules::RULES_VERSION.to_string(),
                    );
                }
                r.rules_version = version;
                if let Err(why) = p.validate() {
                    bad(
                        &mut r,
                        e,
                        None,
                        "logged policy is out of bounds (no policy file could say this)",
                        why.to_string(),
                        String::new(),
                    );
                }
                // recompute with the rules the log was produced under
                let mut p = p.clone();
                p.rules.rules_version = version;
                // rules 9: what the rules read from `[identification]`
                p.sync();
                // rules 10: the relying party and the passkeys the run started with
                if p.rules.passkeys()
                    && let Some(pk) = listed.as_ref().and_then(|ro| ro.passkeys.as_ref())
                {
                    passkeys.rp = Some((pk.rp_id.clone(), pk.origins.clone()));
                    for k in &pk.keys {
                        passkeys.keys.insert(k.id.clone(), (k.clone(), false));
                    }
                }
                if let Some(s) = listed.as_ref().and_then(|ro| ro.study.as_ref()) {
                    r.study = Some(StudyReport {
                        participant: s.participant.clone(),
                        password_step_up: s.password_step_up.clone(),
                        passkey_acts: 0,
                        password_acts: 0,
                    });
                }
                policy = Some(p);
                areas = None;
                if let Some(a) = a {
                    areas = Some(load_areas(&mut r, e, &a.sha256, &a.areas));
                }
            }
            EventBody::AreasLoaded { sha256, areas: set } => {
                areas = Some(load_areas(&mut r, e, sha256, set));
            }
            EventBody::SpaceLoaded {
                segment,
                scenario,
                pinned_shift_days,
            } => {
                r.space_segments += 1;
                let map = scenario.unwrap_or(ScenarioMap::IDENTITY);
                if let Err(why) = map.validate() {
                    bad(&mut r, e, None, "invalid scenario map", why, String::new());
                }
                // rules 13: the time shift is pinned once per run (the engine refuses a segment
                // pinned otherwise): the same elements pinned a day apart answer differently.
                // The segment's identity carries the pin too: both say the same, load after load
                if policy.as_ref().is_some_and(|p| p.rules.space_replay()) {
                    if segment.pinned_shift_days != *pinned_shift_days {
                        bad(
                            &mut r,
                            e,
                            None,
                            "a space segment whose identity names another pin than it was loaded with",
                            json(&segment.pinned_shift_days),
                            json(pinned_shift_days),
                        );
                    }
                    // and the scenario clock is the run's (the bridge's map never changes)
                    if current_segment.is_some() && map != scenario_map {
                        bad(
                            &mut r,
                            e,
                            None,
                            "a mid-log scenario clock change: a space segment loaded with another scenario map than the one loaded first",
                            json(&map),
                            json(&scenario_map),
                        );
                    }
                    if let Some(prev) = &current_segment
                        && (*pinned_shift_days != pinned || segment.pinned_shift_days != prev.pinned_shift_days)
                    {
                        bad(
                            &mut r,
                            e,
                            None,
                            "a mid-log pin change: a space segment pinned to another time shift than the one loaded first",
                            json(&(pinned_shift_days, segment.pinned_shift_days)),
                            json(&(pinned, prev.pinned_shift_days)),
                        );
                    }
                }
                scenario_map = map;
                pinned = *pinned_shift_days;
                current_segment = Some(segment.clone());
            }
            EventBody::SpaceQuery { sha256, answer } => {
                r.space_queries += 1;
                match answer.content_hash() {
                    Ok(h) if h == *sha256 => {
                        answers.insert(sha256.clone(), (**answer).clone());
                    }
                    other => {
                        bad(
                            &mut r,
                            e,
                            None,
                            "space answer hash does not match its content",
                            sha256.clone(),
                            other.unwrap_or_else(|err| format!("<unserializable: {err}>")),
                        );
                        continue;
                    }
                }
                if let Some(oracle) = space {
                    recompute_space(&mut r, e, answer, oracle, pinned);
                }
            }
            EventBody::IntentCreated { intent, session } => {
                if intents.contains_key(&intent.id) {
                    bad(
                        &mut r,
                        e,
                        Some(intent.id),
                        "intent created twice",
                        String::new(),
                        String::new(),
                    );
                    continue;
                }
                // rules 8: a human's intent is made in a live session of theirs (an ended one
                // is caught above), an agent's in none
                let bound = policy.as_ref().is_some_and(|p| p.rules.rules_version >= OPERATOR_AUTH_SINCE);
                match (&intent.source, session) {
                    (_, Some(_)) if !bound => bad(
                        &mut r,
                        e,
                        Some(intent.id),
                        "an intent's session: only builds of rules 8 or later write it",
                        json(session),
                        String::new(),
                    ),
                    (Source::Human { id }, Some(s)) => {
                        check_session(&mut r, e, intent.id, id, *s, &sessions, "a human's intent");
                    }
                    (Source::Human { id }, None) if bound => bad(
                        &mut r,
                        e,
                        Some(intent.id),
                        "a human's intent not made in a live session of theirs",
                        id.to_string(),
                        "session".into(),
                    ),
                    (Source::Agent { id }, Some(_)) => bad(
                        &mut r,
                        e,
                        Some(intent.id),
                        "an agent's intent under an operator session",
                        id.to_string(),
                        String::new(),
                    ),
                    _ => {}
                }
                intents.insert(intent.id, Tracked::new(intent.clone(), State::Proposed));
                if intent.kind() == TaskKind::Engage {
                    r.engagements += 1;
                }
            }
            EventBody::CheckResult {
                intent,
                phase,
                inputs,
                results,
                passed,
                preview,
            } => {
                check_engaged(&mut r, e, *intent, &inputs.engaged, &intents);
                check_accepted(&mut r, e, *intent, &inputs.unknown_accepted, &intents);
                check_ident_inputs(&mut r, e, Some(*intent), &inputs.tracks, &ident, policy.as_ref());
                check_lost_inputs(&mut r, e, *intent, inputs, &lost, &ident, &intents, &mut pictured, policy.as_ref());
                check_waiting(&mut r, e, *intent, &inputs.waiting, &intents);
                check_restore_owed(&mut r, e, *intent, &inputs.restore_owed, &intents, &restores, policy.as_ref());
                check_expended(&mut r, e, *intent, &inputs.expended, &intents, &restores, policy.as_ref());
                check_spent_carried(&mut r, e, *intent, &inputs.spent_carried, &intents, &restores, policy.as_ref());
                let (Some(pol), Some(tr)) = (&policy, intents.get_mut(intent)) else {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "check without run.started or intent.created",
                        String::new(),
                        String::new(),
                    );
                    continue;
                };
                let inputs = match resolve(inputs, &areas, &answers) {
                    Ok(w) => w,
                    Err(why) => {
                        tr.last_check_passed = None;
                        bad(
                            &mut r,
                            e,
                            Some(*intent),
                            why,
                            json(&inputs.areas_sha256),
                            json(&areas.as_ref().map(|a| &a.0)),
                        );
                        continue;
                    }
                };
                check_terrain(&mut r, e, *intent, &inputs);
                check_space_computed(&mut r, e, *intent, &inputs, *phase == CheckPhase::Dispatched, &scenario_map, pol);
                if tr.plan.is_some() && inputs.plan.as_deref() != tr.plan.as_ref() {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "checks on a plan other than the frozen one",
                        json(&inputs.plan),
                        json(&tr.plan),
                    );
                }
                let again = rules::evaluate(&tr.spec, &inputs, &pol.rules);
                let again_passed = rules::passed(&again, tr.spec.kind());
                r.checks_recomputed += 1;
                if &again != results || again_passed != *passed {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "check results differ",
                        json(&(results, passed)),
                        json(&(&again, again_passed)),
                    );
                }
                if tr.spec.kind() == TaskKind::Engage {
                    let mut live = inputs.clone();
                    live.plan = None;
                    let pv = engage::preview(&tr.spec, &live, &pol.rules).ok();
                    if pv.as_ref() != preview.as_deref() {
                        bad(
                            &mut r,
                            e,
                            Some(*intent),
                            "engagement preview differs",
                            json(preview),
                            json(&pv),
                        );
                    }
                }
                tr.last_check_passed = Some(again_passed);
                // rules 11: a dispatched-phase check is a running space task's, re-checked
                // against a later answer; its report (rechecked or missed) follows at once
                if tr.cancel_due.is_some() {
                    bad(&mut r, e, Some(*intent), "a check between an operator's cancel of a dispatched space task and its cancelled report", json(phase), String::new());
                }
                if tr.recheck_due.is_some() || tr.recheck_lost.is_some() {
                    bad(&mut r, e, Some(*intent), "a check between a space task's re-check and its report or failure", json(phase), String::new());
                }
                if *phase == CheckPhase::Dispatched {
                    if !(pol.rules.space_followups() && tr.spec.task.is_space() && tr.state == State::Dispatched) {
                        bad(&mut r, e, Some(*intent), "a dispatched-phase check on something but a running space task under rules 11", json(&(tr.spec.kind(), tr.state, pol.rules.rules_version)), String::new());
                    }
                    // a collect whose pass has come was collected: never re-checked
                    if let Some(SpaceOrder::Collect(c)) = &tr.space_order
                        && scenario_map.scenario(e.t) >= c.collect_time
                    {
                        bad(&mut r, e, Some(*intent), "a collect re-checked after its pass had come (it was collected; a later answer can't miss it)", json(&scenario_map.scenario(e.t)), json(&c.collect_time));
                    }
                    if let Some(a) = inputs.space.answer() {
                        // at the request time it was tasked at, on the segment the engine
                        // holds, which is not the one its last answer came from
                        if a.now().is_some() && a.now() != tr.dispatch_now {
                            bad(&mut r, e, Some(*intent), "a re-check at another request time than the task was tasked at", json(&a.now()), json(&tr.dispatch_now));
                        }
                        if current_segment.as_ref() != Some(&a.segment) {
                            bad(&mut r, e, Some(*intent), "a re-check on a segment other than the one loaded (space.loaded) at that point", a.segment.short(), current_segment.as_ref().map_or(String::new(), |s| s.short()));
                        }
                        if tr.last_segment.as_ref() == Some(&a.segment) {
                            bad(&mut r, e, Some(*intent), "a re-check on the segment the task already runs on (the engine re-checks only a reloaded segment)", a.segment.short(), String::new());
                        }
                    }
                    tr.recheck_due = Some((again_passed, space::recheck_detail(&again, again_passed, inputs.space.answer(), scenario_map.scenario(e.t))));
                }
                tr.last_segment = inputs.space.answer().map(|a| a.segment.clone());
                // which platform each of its vehicles was (a next run carries it only onto the
                // same ones)
                for u in &tr.spec.uids {
                    if let Some(p) = inputs.vehicles.get(u).and_then(|v| v.platform.as_ref()) {
                        tr.platforms.insert(u.clone(), p.id.clone());
                    }
                }
                tr.last_inputs = Some(Box::new(inputs));
                tr.check_window = Some((before_t, e.t));
                tr.last_results = again;
                tr.last_phase = Some(*phase);
                tr.last_check_seq = e.seq;
                if *phase == CheckPhase::Dequeue && tr.state != State::Queued {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "dequeue check of an intent that is not queued",
                        json(&tr.state),
                        json(&State::Queued),
                    );
                }
            }
            EventBody::PolicyDecision {
                intent,
                inputs,
                decision,
            } => {
                check_engaged(&mut r, e, *intent, &inputs.engaged, &intents);
                check_accepted(&mut r, e, *intent, &inputs.unknown_accepted, &intents);
                check_ident_inputs(&mut r, e, Some(*intent), &inputs.tracks, &ident, policy.as_ref());
                check_lost_inputs(&mut r, e, *intent, inputs, &lost, &ident, &intents, &mut pictured, policy.as_ref());
                check_waiting(&mut r, e, *intent, &inputs.waiting, &intents);
                check_restore_owed(&mut r, e, *intent, &inputs.restore_owed, &intents, &restores, policy.as_ref());
                check_expended(&mut r, e, *intent, &inputs.expended, &intents, &restores, policy.as_ref());
                check_spent_carried(&mut r, e, *intent, &inputs.spent_carried, &intents, &restores, policy.as_ref());
                let (Some(pol), Some(tr)) = (&policy, intents.get_mut(intent)) else {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "policy decision without run.started or intent.created",
                        String::new(),
                        String::new(),
                    );
                    continue;
                };
                let inputs = match resolve(inputs, &areas, &answers) {
                    Ok(w) => w,
                    Err(why) => {
                        tr.tier = None;
                        bad(
                            &mut r,
                            e,
                            Some(*intent),
                            why,
                            json(&inputs.areas_sha256),
                            json(&areas.as_ref().map(|a| &a.0)),
                        );
                        continue;
                    }
                };
                check_space_computed(&mut r, e, *intent, &inputs, false, &scenario_map, pol);
                let again = policy::decide(&tr.spec, &inputs, pol);
                r.policy_recomputed += 1;
                if &again != decision {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "policy decision differs",
                        json(decision),
                        json(&again),
                    );
                }
                tr.route(&again);
                tr.answer = inputs.space.answer().map(|a| Box::new(a.clone()));
            }
            EventBody::ApprovalRequested {
                intent,
                expires_t,
                until,
            } => {
                let Some(tr) = intents.get_mut(intent) else {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "unknown intent",
                        String::new(),
                        String::new(),
                    );
                    continue;
                };
                if let Some(pol) = &policy {
                    let (want, basis) = policy::approval_deadline(
                        pol,
                        &tr.spec.task,
                        tr.answer.as_deref(),
                        &scenario_map,
                        e.t,
                    );
                    if *expires_t != want || *until != basis {
                        bad(
                            &mut r,
                            e,
                            Some(*intent),
                            "approval deadline differs from policy",
                            json(&(expires_t, until)),
                            json(&(want, basis)),
                        );
                    }
                }
                if tr.state == State::Queued
                    && tr.recheck != Some(Verdict::Inbox)
                    && !tr.queue_expired_inbox
                {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "a queued intent sent for approval without a re-check or a queue expiry that says so",
                        json(&tr.recheck),
                        json(&Verdict::Inbox),
                    );
                }
                tr.expires_t = Some(*expires_t);
            }
            EventBody::ApprovalConcurred {
                intent,
                by,
                session,
                n,
                of,
                snapshot,
                token,
                supersedes,
                checked_t,
                self_accepted,
                step_up,
                ..
            } => {
                let Some(tr) = intents.get_mut(intent) else {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "unknown intent",
                        String::new(),
                        String::new(),
                    );
                    continue;
                };
                check_signature(&mut r, e, *intent, tr, by, *session, &sessions);
                check_step_up(
                    &mut r,
                    e,
                    Some(*intent),
                    tr.step_up,
                    by,
                    *session,
                    *step_up,
                    &mut step_ups,
                    StepUse::Signature,
                    &sign_expected(*intent, token.as_deref()),
                    policy.as_ref(),
                );
                check_self_accepted(&mut r, e, *intent, tr, by, *self_accepted);
                let want = (u32::try_from(tr.signers.len()).unwrap_or(u32::MAX) + 1, tr.approvers);
                if (*n, *of) != want || *n >= *of {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "concurrence count differs",
                        json(&(n, of)),
                        json(&want),
                    );
                }
                check_signed(&mut r, e, *intent, tr, by, *session, snapshot, token, supersedes, *checked_t, &mut issued, policy.as_ref());
                tr.signers.push(by.clone());
                r.concurrences += 1;
            }
            EventBody::PictureIssued { token, inputs } => {
                check_ident_inputs(&mut r, e, Some(token.intent), &inputs.tracks, &ident, policy.as_ref());
                check_lost_inputs(&mut r, e, token.intent, inputs, &lost, &ident, &intents, &mut pictured, policy.as_ref());
                check_issued(&mut r, e, token, inputs, &intents, &sessions, &areas, &answers, policy.as_ref());
                if issued.insert(token.id, (**token).clone()).is_some() {
                    bad(&mut r, e, Some(token.intent), "picture token issued twice", token.id.to_string(), String::new());
                }
            }
            EventBody::ApprovalReset {
                intent,
                dropped,
                inputs,
                ..
            } => {
                if let Some(i) = inputs {
                    check_accepted(&mut r, e, *intent, &i.unknown_accepted, &intents);
                    check_ident_inputs(&mut r, e, Some(*intent), &i.tracks, &ident, policy.as_ref());
                    check_lost_inputs(&mut r, e, *intent, i, &lost, &ident, &intents, &mut pictured, policy.as_ref());
                }
                let Some(tr) = intents.get_mut(intent) else {
                    bad(&mut r, e, Some(*intent), "unknown intent", String::new(), String::new());
                    continue;
                };
                if *dropped != tr.signers {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "reset drops other signatures than the current ones",
                        json(dropped),
                        json(&tr.signers),
                    );
                }
                // the world the plan was checked on: the logged one (the hold while pending),
                // or this signature's re-check (the check.result just before)
                let (world, failed) = match inputs {
                    Some(i) => match (resolve(i, &areas, &answers), &policy) {
                        (Ok(w), Some(pol)) => {
                            let again = engage::evaluate(&tr.spec, &w, &pol.rules);
                            let failed = again.iter().any(|c| c.outcome != rules::Outcome::Pass);
                            (Some(w), failed)
                        }
                        _ => (None, false),
                    },
                    None => (
                        tr.last_inputs.as_deref().cloned(),
                        tr.last_check_passed == Some(false),
                    ),
                };
                let plan_read = world.as_ref().and_then(|w| w.plan.as_deref());
                if tr.plan.is_none() || plan_read != tr.plan.as_ref() {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "reset of a plan that is not the frozen one",
                        json(&plan_read),
                        json(&tr.plan),
                    );
                }
                if !failed {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "signatures dropped although the plan held",
                        json(dropped),
                        "the plan held".into(),
                    );
                }
                tr.signers.clear();
                tr.plan = None;
                // rules 6: the acceptances go with the plan (given again, on the new picture);
                // rules 16: of lost contacts too
                tr.accepted.clear();
                tr.accepted_lost.clear();
                // rules 19: and of live tracks as behaving now
                tr.accepted_behaving.clear();
            }
            EventBody::ApprovalGranted {
                intent,
                by,
                self_approved,
                session,
                snapshot,
                token,
                supersedes,
                checked_t,
                self_accepted,
                step_up,
                ..
            } => {
                let Some(tr) = intents.get_mut(intent) else {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "unknown intent",
                        String::new(),
                        String::new(),
                    );
                    continue;
                };
                if tr.state != State::PendingApproval {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "approval granted outside pending_approval",
                        json(&tr.state),
                        json(&State::PendingApproval),
                    );
                }
                match tr.expires_t {
                    Some(x) if e.t < x => {}
                    other => bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "approval granted at or after its deadline",
                        e.t.0.to_string(),
                        format!("< {:?}", other.map(|t| t.0)),
                    ),
                }
                check_self_accepted(&mut r, e, *intent, tr, by, *self_accepted);
                let want = is_self_approval(&tr.spec.source, by);
                if *self_approved != want {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "self_approved flag differs",
                        self_approved.to_string(),
                        want.to_string(),
                    );
                }
                r.approvals += 1;
                if want {
                    r.self_approvals += 1;
                }
                check_signature(&mut r, e, *intent, tr, by, *session, &sessions);
                check_step_up(
                    &mut r,
                    e,
                    Some(*intent),
                    tr.step_up,
                    by,
                    *session,
                    *step_up,
                    &mut step_ups,
                    StepUse::Signature,
                    &sign_expected(*intent, token.as_deref()),
                    policy.as_ref(),
                );
                check_signed(&mut r, e, *intent, tr, by, *session, snapshot, token, supersedes, *checked_t, &mut issued, policy.as_ref());
                if tr.spec.kind() == TaskKind::Engage {
                    tr.last_picture_t = snapshot.as_ref().map(|s| s.t);
                    tr.snapshot = snapshot.clone();
                    // rules 9: the re-check's world is its picture until its first watch
                    if policy.as_ref().is_some_and(|p| p.rules.rule_id()) {
                        tr.last_picture_world = tr.last_inputs.as_deref().cloned();
                    }
                    // where the platform loiters over the point, from the re-check's world
                    tr.loiter_radius_m = match (&policy, tr.spec.uids.first(), tr.last_inputs.as_deref()) {
                        (Some(pol), Some(u), Some(w)) => Some(engage::loiter_radius(u, w, &pol.rules)),
                        _ => None,
                    };
                    // rules 6: its lost-link action as the fallback, from the same world
                    tr.preload = match (&policy, tr.last_inputs.as_deref(), snapshot.as_deref()) {
                        (Some(pol), Some(w), Some(snap)) => {
                            engage::order_preload(&tr.spec, w, snap, &pol.rules)
                        }
                        _ => None,
                    };
                }
                tr.signers.push(by.clone());
                let needed = usize::try_from(tr.approvers).unwrap_or(usize::MAX);
                if tr.signers.len() != needed {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "approval granted without the signatures the tier needs",
                        tr.signers.len().to_string(),
                        needed.to_string(),
                    );
                }
                tr.granted = true;
                tr.approval_rec = Some(CarriedApproval {
                    by: by.clone(),
                    session: *session,
                    t: e.t,
                    at_submission: false,
                    self_approved: *self_approved,
                });
            }
            EventBody::ApprovalAtSubmission {
                intent,
                by,
                session,
                self_approved,
                rule,
            } => {
                check_session(&mut r, e, *intent, by, *session, &sessions, "approval at submission");
                let Some(tr) = intents.get_mut(intent) else {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "unknown intent",
                        String::new(),
                        String::new(),
                    );
                    continue;
                };
                if tr.state != State::Checked {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "approval at submission outside checked",
                        json(&tr.state),
                        json(&State::Checked),
                    );
                }
                let submitter_ok = match &tr.spec.source {
                    Source::Agent { id } => {
                        bad(
                            &mut r,
                            e,
                            Some(*intent),
                            "agent-sourced intent approved at submission",
                            by.to_string(),
                            format!("agent {id}: only a human approves it, in the inbox"),
                        );
                        false
                    }
                    Source::Human { id } if id != by => {
                        bad(
                            &mut r,
                            e,
                            Some(*intent),
                            "approval at submission by someone other than the submitter",
                            by.to_string(),
                            id.to_string(),
                        );
                        false
                    }
                    Source::Human { .. } => true,
                };
                if !*self_approved {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "self_approved flag differs",
                        self_approved.to_string(),
                        "true".into(),
                    );
                }
                let allowed = tr.tier == Some(Tier::Human) && tr.submitter_authorizes;
                if !allowed {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "approval at submission in a tier that doesn't allow it",
                        rule.clone(),
                        json(&(&tr.tier, &tr.rule, tr.submitter_authorizes)),
                    );
                } else if tr.rule.as_deref() != Some(rule.as_str()) {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "approval at submission names another tier",
                        rule.clone(),
                        json(&tr.rule),
                    );
                }
                r.approvals_at_submission += 1;
                tr.granted_at_submission = submitter_ok && allowed;
                tr.approval_rec = Some(CarriedApproval {
                    by: by.clone(),
                    session: *session,
                    t: e.t,
                    at_submission: true,
                    self_approved: true,
                });
            }
            EventBody::ApprovalRejected {
                intent, by, session, ..
            } => {
                check_session(&mut r, e, *intent, by, *session, &sessions, "rejection");
                if let Some(tr) = intents.get_mut(intent) {
                    tr.rejected_by_human = true;
                }
            }
            EventBody::ApprovalExpired { intent, expires_t } => {
                if let Some(tr) = intents.get_mut(intent) {
                    if e.t < *expires_t || Some(*expires_t) != tr.expires_t {
                        bad(
                            &mut r,
                            e,
                            Some(*intent),
                            "expired before its deadline",
                            e.t.0.to_string(),
                            format!(">= {}", expires_t.0),
                        );
                    }
                    tr.expired = true;
                }
            }
            EventBody::IntentState {
                intent,
                transition,
                from,
                to,
                reason,
            } => {
                // rules 15 (murmur-zq3): a void that says a newer order superseded it names one
                // the log has, newer, on that vehicle (an engagement's stop is checked by its
                // cause, `engage.stopped`)
                if *transition == Transition::Void
                    && policy.as_ref().is_some_and(|p| p.rules.carry_followups())
                    && let Some(tr) = intents.get(intent)
                    && tr.spec.kind() != TaskKind::Engage
                    && let Some(why) = superseded_mismatch(*intent, &tr.spec, reason, &intents)
                {
                    bad(&mut r, e, Some(*intent), why, reason.clone(), String::new());
                }
                // rules 12 (murmur-54l): a mission refused for its platform's own lost-link
                // values (its kind never queued) says exactly why, the orders still queued on
                // its vehicles included
                // rules 12: a void of an approved order for a spent platform says so, and only
                // for a spent one
                if *transition == Transition::Void
                    && policy.as_ref().is_some_and(|p| p.rules.followups())
                    && let Some(tr) = intents.get(intent)
                    && reason.starts_with("system: spent platform:")
                    && !tr
                        .spec
                        .uids
                        .iter()
                        .any(|u| restores.expended_by(u).is_some() || restores.spent_carried_on(u))
                {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "voided as for a spent platform, none of its vehicles spent by its own effect",
                        reason.clone(),
                        json(&tr.spec.uids),
                    );
                }
                // rules 12 (murmur-54l): a mission of a kind never queued, voided while its
                // platform's own values hold it, is the refusal and says exactly why (the
                // restore owed, the engagements it stops, the orders still queued, whether it may
                // be running); a refusal without those facts is no refusal
                let refusal_want = match (policy.as_ref(), intents.get(intent)) {
                    (Some(p), Some(tr))
                        if *transition == Transition::Void
                            && p.rules.followups()
                            && !p.queue().queues(tr.spec.kind())
                            && matches!(tr.state, State::Approved | State::AutoApproved)
                            && !reason.starts_with("system: spent platform:") =>
                    {
                        let owed = restores.owed_for(&tr.spec, &p.rules, &BTreeMap::new());
                        let all = || intents.iter().map(|(i, t)| (*i, &t.spec, t.state));
                        let engaged: BTreeSet<IntentId> = engaged_vehicles(all(), &tr.spec).into_values().collect();
                        let standing = queued_on(all(), &tr.spec);
                        let note = if tr.maybe_started.is_empty() {
                            String::new()
                        } else {
                            let who: Vec<String> = tr.maybe_started.iter().map(ToString::to_string).collect();
                            format!(
                                " (it may be running on vehicle(s) {}: its start went out before the link was lost, and nothing confirmed it since; look at the vehicle)",
                                who.join(", ")
                            )
                        };
                        match refusal_reason(&tr.spec, &owed, &engaged, &standing, &p.rules) {
                            Some(_) if other_void(reason, &tr.spec) => None,
                            Some(w) => Some(format!("system: {w}{note}")),
                            None if reason.starts_with(&format!("system: {REFUSED}")) => Some(String::new()),
                            None => None,
                        }
                    }
                    _ => None,
                };
                if let Some(want) = &refusal_want
                    && (want.is_empty() || reason != want)
                {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "a refusal's reason differs from the one its facts give (rules 12: the restore owed, the engagements it stops, the orders still queued)",
                        reason.clone(),
                        want.clone(),
                    );
                }
                let Some(tr) = intents.get_mut(intent) else {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "unknown intent",
                        String::new(),
                        String::new(),
                    );
                    continue;
                };
                r.transitions_verified += 1;
                if *from != tr.state {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "transition from a state the intent wasn't in",
                        json(from),
                        json(&tr.state),
                    );
                }
                // rules 11: a cancel after dispatch follows the operator's cancel and the
                // engine's report at once, naming the operator; a missed report is followed by
                // the failure for it; nothing of the intent comes between
                let space11 = policy.as_ref().is_some_and(|p| p.rules.space_followups()) && tr.spec.task.is_space();
                if let Some((op, _)) = tr.cancel_due.take() {
                    if *transition != Transition::Cancel {
                        bad(&mut r, e, Some(*intent), "an operator's cancel of a dispatched space task not carried out by the next transition", json(transition), json(&Transition::Cancel));
                    } else if !tr.cancel_reported {
                        bad(&mut r, e, Some(*intent), "cancelled after dispatch without the engine's cancelled report", String::new(), space::CANCELLED_OP.into());
                    } else if !reason.starts_with(&format!("cancelled by {op}: ")) {
                        bad(&mut r, e, Some(*intent), "a cancel after dispatch that does not name the operator who cancelled", reason.clone(), format!("cancelled by {op}: ..."));
                    }
                } else if *transition == Transition::Cancel {
                    bad(&mut r, e, Some(*intent), "cancelled after dispatch without an operator's cancel right before it", reason.clone(), "operator.action intent.cancel".into());
                }
                if *transition == Transition::Cancel && !space11 {
                    bad(&mut r, e, Some(*intent), "a cancel after dispatch on something but a space task under rules 11", json(&tr.spec.kind()), String::new());
                }
                if tr.recheck_due.is_some() {
                    bad(&mut r, e, Some(*intent), "a transition between a space task's re-check and its report", json(transition), "op.state rechecked or missed".into());
                    tr.recheck_due = None;
                }
                if let Some(want) = tr.recheck_lost.take() {
                    let reason_want = space::missed_reason(&tr.spec.task, &want);
                    if *transition != Transition::Fail || *reason != reason_want {
                        bad(&mut r, e, Some(*intent), "a space task's missed report not followed by its failure for it", json(&(transition, reason)), reason_want);
                    }
                } else if *transition == Transition::Fail && space11 && reason.contains(space::NO_LONGER_FEASIBLE) {
                    bad(&mut r, e, Some(*intent), "a space task failed as no longer feasible without the re-check and report that found it", reason.clone(), "check.result phase dispatched, op.state missed".into());
                }
                match next_state(tr.state, *transition) {
                    Ok(s) if s == *to => {}
                    Ok(s) => bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "transition leads elsewhere",
                        json(to),
                        json(&s),
                    ),
                    Err(err) => bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "illegal transition",
                        json(to),
                        err.to_string(),
                    ),
                }
                if let Some(why) = unbacked(tr, *transition) {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        why,
                        json(transition),
                        "no evidence".into(),
                    );
                }
                // a queue re-check's verdict is carried out by the next transition, nothing
                // else (dispatch, back to the inbox, void)
                if tr.state == State::Queued
                    && let Some(v) = tr.recheck
                {
                    let follows = match v {
                        Verdict::Dispatch => Transition::Dispatch,
                        Verdict::Inbox => Transition::RequireApproval,
                        Verdict::Void => Transition::Void,
                    };
                    if *transition != follows {
                        bad(
                            &mut r,
                            e,
                            Some(*intent),
                            "a queue re-check's verdict not carried out",
                            json(transition),
                            json(&follows),
                        );
                    }
                }
                // and a queue expiry by its own (expired, or back to the inbox)
                if tr.state == State::Queued && tr.queue_expired {
                    let follows = if tr.queue_expired_inbox {
                        Transition::RequireApproval
                    } else {
                        Transition::Expire
                    };
                    if *transition != follows {
                        bad(
                            &mut r,
                            e,
                            Some(*intent),
                            "a queue expiry not carried out",
                            json(transition),
                            json(&follows),
                        );
                    }
                }
                // the engine writes an intent.dispatched and its transition back to back
                if let Some(seq) = tr.dispatch_logged
                    && *transition != Transition::Dispatch
                {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "intent.dispatched followed by another transition than its dispatch",
                        json(transition),
                        format!("dispatch (intent.dispatched at seq {seq})"),
                    );
                    tr.dispatch_logged = None;
                }
                match transition {
                    Transition::Dispatch => {
                        tr.dispatch_logged = None;
                        tr.dispatched = true;
                        tr.started_ahead.clear();
                        tr.dispatched_before = true;
                        tr.ops.clear();
                        tr.start_went_out.clear();
                        tr.confirming = tr.maybe_started.clone();
                        last_dispatch = Some(*intent);
                        step = Some((*intent, e.seq));
                        for u in &tr.spec.uids {
                            newest_on.insert(u.clone(), *intent);
                        }
                    }
                    Transition::Release => {
                        tr.effect_logged = false;
                        r.effects += 1;
                    }
                    Transition::Abort => {
                        tr.stopped = false;
                        r.aborts += 1;
                    }
                    // rules 7: a mission refused for its platform's own lost-link values (its
                    // kind never queued) opens a step in which it supersedes the engagements
                    // on its vehicles; no other void does
                    Transition::Void
                        if policy.as_ref().is_some_and(|p| {
                            p.rules.rules_version >= rules::QUEUE_FOLLOWUPS_SINCE
                                && !p.queue().queues(tr.spec.kind())
                                && restores.holds(&tr.spec, &p.rules, &BTreeMap::new())
                        }) && matches!(tr.state, State::Approved | State::AutoApproved)
                            && !matches!(tr.spec.kind(), TaskKind::Hold | TaskKind::Rtl)
                            && reason.starts_with(&format!("system: {REFUSED}")) =>
                    {
                        tr.stopped = false;
                        step = Some((*intent, e.seq));
                    }
                    Transition::Void => tr.stopped = false,
                    Transition::Cancel => {
                        tr.cancel_reported = false;
                        r.space_cancelled += 1;
                    }
                    Transition::Queue => {
                        tr.queued_logged = None;
                        r.queued += 1;
                        last_queued = Some(*intent);
                        step = Some((*intent, e.seq));
                    }
                    Transition::Expire if tr.state == State::Queued => {
                        tr.queue_expired = false;
                        r.queue_expired += 1;
                    }
                    Transition::Requeue => {
                        tr.requeued_logged = None;
                        r.requeued += 1;
                    }
                    // what the approval rests on, as the engine records it: the last check,
                    // the decision that routed it, the engagements in play then, who
                    Transition::AutoApprove
                    | Transition::Approve
                    | Transition::AuthorizeAtSubmission => {
                        tr.basis = Some(Basis {
                            t: e.t,
                            by_human: *transition != Transition::AutoApprove,
                            checks: tr.last_results.clone(),
                            policy: tr.decision.clone(),
                            engaged: tr
                                .last_inputs
                                .as_deref()
                                .map(|w| w.engaged.clone())
                                .unwrap_or_default(),
                        });
                        tr.basis_carried = false;
                    }
                    _ => {}
                }
                if tr.state == State::Queued {
                    tr.recheck = None;
                    tr.recheck_seq = None;
                    tr.queue_expired = false;
                    tr.queue_expired_inbox = false;
                    // back to a human: their next approval decides, starting it again included
                    if *transition == Transition::RequireApproval {
                        tr.maybe_started.clear();
                    }
                }
                tr.state = *to;
                if to.is_terminal() {
                    tr.ended_t.get_or_insert(e.t);
                    restores.intent_ended(*intent);
                }
            }
            EventBody::IntentDispatched {
                intent,
                task,
                uids,
                space: order,
                engage: engage_order,
                inputs: reach_inputs,
                reach,
            } => {
                if let Some(inputs) = reach_inputs {
                    check_restore_owed(&mut r, e, *intent, &inputs.restore_owed, &intents, &restores, policy.as_ref());
                    check_expended(&mut r, e, *intent, &inputs.expended, &intents, &restores, policy.as_ref());
                    check_spent_carried(&mut r, e, *intent, &inputs.spent_carried, &intents, &restores, policy.as_ref());
                    check_ident_inputs(&mut r, e, Some(*intent), &inputs.tracks, &ident, policy.as_ref());
                    check_lost_inputs(&mut r, e, *intent, inputs, &lost, &ident, &intents, &mut pictured, policy.as_ref());
                }
                // rules 7: never a mission to a vehicle still owed its own lost-link values,
                // whether its kind is queued or not (a queued one's reach says so too); rules
                // 12: values carried over hold unless the reach it went on showed another vehicle
                // (a kind never queued logs no reach: they hold it whatever vehicle has the uid)
                let ids = match (reach_inputs, intents.get(intent)) {
                    (Some(i), _) => carried_identities(&i.restore_owed),
                    // from the queue: its re-check read them
                    (None, Some(tr)) if tr.last_phase == Some(CheckPhase::Dequeue) => tr
                        .last_inputs
                        .as_deref()
                        .map(|w| carried_identities(&w.restore_owed))
                        .unwrap_or_default(),
                    _ => BTreeMap::new(),
                };
                // rules 15: nor to one spent in an earlier run, unless what the order went on (its
                // reach, else its last check) showed another vehicle under the uid (none: held
                // whatever vehicle has it)
                let spent_ids = match (reach_inputs, intents.get(intent)) {
                    (Some(i), _) => spent_identities(&i.spent_carried),
                    (None, Some(tr)) => tr
                        .last_inputs
                        .as_deref()
                        .map(|w| spent_identities(&w.spent_carried))
                        .unwrap_or_default(),
                    _ => BTreeMap::new(),
                };
                if let (Some(pol), Some(tr)) = (&policy, intents.get(intent))
                    && pol.rules.carry_followups()
                    && let Some((u, held)) = restores
                        .spent_carried_for(&tr.spec, &pol.rules, &spent_ids)
                        .into_iter()
                        .find(|(_, s)| s.holds())
                {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "an order dispatched to a platform spent by its own effect in an earlier run (rules 15)",
                        u.to_string(),
                        held.detail(),
                    );
                }
                // rules 12: never an order to a platform spent by its own effect
                if let (Some(pol), Some(tr)) = (&policy, intents.get(intent))
                    && pol.rules.followups()
                    && let Some(u) = tr.spec.uids.iter().find(|u| restores.expended_by(u).is_some())
                {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "an order dispatched to a platform spent by its own effect (rules 12)",
                        u.to_string(),
                        format!("spent by engagement {}", restores.expended_by(u).map(|i| i.to_string()).unwrap_or_default()),
                    );
                }
                if let (Some(pol), Some(tr)) = (&policy, intents.get(intent))
                    && !matches!(tr.spec.kind(), TaskKind::Hold | TaskKind::Rtl | TaskKind::Engage)
                    && restores.holds(&tr.spec, &pol.rules, &ids)
                {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "a mission dispatched to a vehicle still owed its own lost-link values (rules 7)",
                        json(&restores.owed_for(&tr.spec, &pol.rules, &ids)),
                        "no values owed back, or a hold or rtl".into(),
                    );
                }
                let Some(tr) = intents.get_mut(intent) else {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "dispatch of an unknown intent",
                        json(&(task, uids)),
                        String::new(),
                    );
                    continue;
                };
                tr.space_order = order.clone();
                if let Some(a) = tr.last_inputs.as_deref().and_then(|w| w.space.answer()) {
                    tr.dispatch_now = a.now();
                }
                if let Some(seq) = tr.dispatch_logged {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "dispatched again before its dispatch transition",
                        seq.to_string(),
                        "intent.state dispatch".into(),
                    );
                }
                tr.dispatch_logged = Some(e.seq);
                if tr.spec.kind() == TaskKind::Engage
                    && let Some(seq) = tr.stop_due
                {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "engagement dispatched after its picture failed (void suppressed)",
                        seq.to_string(),
                        "engage.stopped".into(),
                    );
                }
                let from_queue = tr.state == State::Queued && tr.recheck == Some(Verdict::Dispatch);
                // out of the queue: at once after the re-check that allows it
                if from_queue && tr.recheck_seq.map(|s| s + 1) != Some(e.seq) {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "dispatched from the queue, but not right after its re-check",
                        e.seq.to_string(),
                        json(&tr.recheck_seq.map(|s| s + 1)),
                    );
                }
                // rules 5, straight from the approval: only to vehicles the link reaches now
                if let Some(pol) = &policy
                    && matches!(tr.state, State::AutoApproved | State::Approved)
                    && pol.rules.rules_version >= 5
                    && pol.queue().queues(tr.spec.kind())
                {
                    match (reach_inputs, reach) {
                        (Some(inputs), Some(reach)) => match resolve(inputs, &areas, &answers) {
                            Ok(w) => {
                                let cfg = pol.queue();
                                let again = queue::reach(&tr.spec, &w, &cfg, &pol.rules);
                                if &again != reach {
                                    bad(&mut r, e, Some(*intent), "reach differs", json(reach), json(&again));
                                }
                                if !queue::all_reachable(&again) {
                                    bad(
                                        &mut r,
                                        e,
                                        Some(*intent),
                                        "dispatched to a vehicle the link could not reach (it must be queued)",
                                        json(reach),
                                        json(&again),
                                    );
                                }
                            }
                            Err(why) => bad(&mut r, e, Some(*intent), why, String::new(), String::new()),
                        },
                        _ => bad(
                            &mut r,
                            e,
                            Some(*intent),
                            "dispatched without the reach that allows it (rules 5: its kind is queued when a vehicle can't be reached)",
                            "none".into(),
                            "inputs and reach".into(),
                        ),
                    }
                }
                if !(matches!(tr.state, State::AutoApproved | State::Approved) || from_queue) {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        if tr.state == State::Queued {
                            "dispatched from the queue without a passing re-check"
                        } else {
                            "dispatched without approval"
                        },
                        json(&tr.state),
                        "auto_approved|approved, or queued after a re-check that dispatches".into(),
                    );
                }
                check_dispatch(
                    &mut r,
                    e,
                    tr,
                    Dispatched {
                        task,
                        uids,
                        space: order.as_ref(),
                        engage: engage_order.as_ref(),
                    },
                    policy.as_ref(),
                );
            }
            EventBody::AuthRefused { operator, .. } => {
                // rules 8: a login refused, in a credentials run, for an operator it lists
                // (an unknown id goes to the bridge's own log: the audit log names no one
                // the run does not know)
                if r.auth != Some(AuthMode::Credentials) {
                    let logged = json(&r.auth);
                    bad(&mut r, e, None, "a refused login outside a credentials run", logged, "credentials".into());
                } else if !roster.as_ref().is_some_and(|ro| ro.operators.contains(operator)) {
                    bad(&mut r, e, None, "a refused login for an operator the run does not list", operator.to_string(), String::new());
                }
                r.logins_refused += 1;
            }
            EventBody::PasskeyRegistered { key, via, session, step_up } => {
                // rules 10: a passkey registered while the run was up: an operator it lists, a
                // credential it does not know yet, a P-256 key; through the operator's own
                // session (a person's) quoting the credential they re-entered for it, or
                // through an invitation (no session, no step-up)
                let passkey_rules = policy.as_ref().is_some_and(|p| p.rules.passkeys());
                if !passkey_rules || r.auth != Some(AuthMode::Credentials) || passkeys.rp.is_none() {
                    bad(&mut r, e, None, "a passkey registered outside a credentials run of rules 10 with a relying party", String::new(), String::new());
                }
                if !roster.as_ref().is_some_and(|ro| ro.operators.contains(&key.operator)) {
                    bad(&mut r, e, None, "a passkey registered for an operator the run does not list", key.operator.to_string(), String::new());
                }
                if passkeys.keys.contains_key(&key.id) {
                    bad(&mut r, e, None, "a passkey registered twice", key.id.clone(), String::new());
                }
                if let Err(why) = crate::passkey::check_key(&key.public_key) {
                    bad(&mut r, e, None, "a registered passkey that is not a P-256 key", why, String::new());
                }
                match (via.as_str(), session) {
                    ("session", Some(s)) => {
                        check_operator(&mut r, e, &key.operator, *s, &sessions, "a passkey registration");
                        if !methods.get(s).copied().is_some_and(AuthMethod::is_person) {
                            bad(&mut r, e, None, "a passkey registered through a session that is not a person's", s.0.to_string(), String::new());
                        }
                        check_step_up(
                            &mut r,
                            e,
                            None,
                            true,
                            &key.operator,
                            *s,
                            *step_up,
                            &mut step_ups,
                            StepUse::Registration,
                            &Expected::Registration,
                            policy.as_ref(),
                        );
                    }
                    ("invitation", None) if step_up.is_none() => {}
                    _ => bad(
                        &mut r,
                        e,
                        None,
                        "a registration is through a session (quoting its step-up) or an invitation (neither)",
                        json(&(via, session, step_up)),
                        String::new(),
                    ),
                }
                passkeys.keys.insert(key.id.clone(), (key.clone(), false));
                r.passkeys_registered += 1;
            }
            EventBody::PasskeyRemoved { operator, credential, .. } => {
                match passkeys.keys.get_mut(credential) {
                    Some((k, removed)) if &k.operator == operator && !*removed => *removed = true,
                    _ => bad(
                        &mut r,
                        e,
                        None,
                        "a removal of a passkey the run does not hold for that operator",
                        credential.clone(),
                        operator.to_string(),
                    ),
                }
                r.passkeys_removed += 1;
            }
            EventBody::OperatorAction {
                operator,
                session,
                action,
                detail,
                grants,
                auth,
                step_up,
                passkey: proof,
            } => {
                let authenticated = policy
                    .as_ref()
                    .is_some_and(|p| p.rules.rules_version >= OPERATOR_AUTH_SINCE);
                let passkey_rules = policy.as_ref().is_some_and(|p| p.rules.passkeys());
                let run_auth = r.auth;
                // rules 10: an assertion goes with a passkey login or step-up, and only there
                let passkey_act = matches!(action.as_str(), "session.opened" | "step_up") && *auth == Some(AuthMethod::Passkey);
                // rules 11: an operator's cancel of a dispatched collect or cover: the engine's
                // cancelled report and the cancel transition must follow at once
                if action == "intent.cancel"
                    && policy.as_ref().is_some_and(|p| p.rules.space_followups())
                    && let Some(id) = cancelled_intent(detail)
                    && let Some(tr) = intents.get_mut(&id)
                    && tr.state == State::Dispatched
                    && tr.spec.task.is_space()
                {
                    if tr.recheck_due.is_some() || tr.recheck_lost.is_some() {
                        bad(&mut r, e, Some(id), "a cancel between a space task's re-check and its report", String::new(), String::new());
                    }
                    tr.cancel_due = Some((operator.clone(), e.seq));
                }
                if proof.is_some() != passkey_act {
                    bad(
                        &mut r,
                        e,
                        None,
                        "a passkey assertion logged without a passkey login or step-up, or one without its assertion",
                        json(&(action, auth, proof.is_some())),
                        String::new(),
                    );
                }
                if action == "session.opened" {
                    // rules 10: a passkey login's assertion, verified against the run's key
                    if let Some(u) = proof {
                        if !passkey_rules {
                            bad(&mut r, e, None, "a passkey login under rules that have none", String::new(), String::new());
                        } else if u.purpose != crate::passkey::StepUpFor::Login {
                            bad(&mut r, e, None, "a login's assertion made for another act", u.purpose.describe(), "a login".into());
                        } else if let Err(why) = passkeys.use_once(operator, None, e.t, u) {
                            bad(&mut r, e, None, &format!("passkey login: {why}"), u.credential.clone(), String::new());
                        }
                        r.passkey_logins += 1;
                    }
                    // rules 10: how a person may sign in (`[approval] login_factor`)
                    if passkey_rules
                        && run_auth == Some(AuthMode::Credentials)
                        && let (Some(method), Some(p)) = (*auth, policy.as_ref())
                        && !p.login_factor().admits(method)
                    {
                        bad(
                            &mut r,
                            e,
                            None,
                            "a session opened by a method the policy's login_factor does not admit",
                            method.as_str().into(),
                            p.login_factor().as_str().into(),
                        );
                    }
                    let grants = grants.unwrap_or_default();
                    // only operators the run was configured with, engagement authority only
                    // for its engagement operators (rules 4 on: before, no roster was logged)
                    if let Some(why) = roster.as_ref().and_then(|ro| ro.refuses(operator, grants)) {
                        bad(
                            &mut r,
                            e,
                            None,
                            why,
                            json(&(operator, grants)),
                            json(&roster),
                        );
                    }
                    // rules 8: opened the way the run authenticates (a checked credential in a
                    // credentials run: a person's password, or a harness-held scripted one; an
                    // asserted name in a dev run)
                    // rules 12: a session opens by the method the roster's scripted list gives
                    // its operator (a harness's credential scripted, a person's not)
                    if policy.as_ref().is_some_and(|p| p.rules.followups())
                        && let (Some(method), Some(why)) = (
                            *auth,
                            auth.and_then(|m| roster.as_ref().and_then(|ro| ro.method_refused(operator, m))),
                        )
                    {
                        bad(&mut r, e, None, &format!("session opened by another method than the roster fixes for its operator: {why}"), method.as_str().into(), json(&roster.as_ref().and_then(|ro| ro.scripted.as_ref())));
                    }
                    if authenticated {
                        match (run_auth, *auth) {
                            (Some(mode), Some(method)) if mode.admits(method) => {
                                methods.insert(*session, method);
                                match method {
                                    AuthMethod::Password | AuthMethod::Passkey => r.people.insert(operator.clone()),
                                    AuthMethod::Scripted => r.scripted.insert(operator.clone()),
                                    AuthMethod::Asserted => false,
                                };
                            }
                            _ => bad(
                                &mut r,
                                e,
                                None,
                                "session opened by another method than the run authenticates with",
                                json(auth),
                                json(&run_auth.map(AuthMode::method)),
                            ),
                        }
                    }
                    if sessions
                        .insert(*session, (operator.clone(), grants))
                        .is_some()
                    {
                        bad(
                            &mut r,
                            e,
                            None,
                            "session opened twice",
                            session.0.to_string(),
                            String::new(),
                        );
                    }
                } else if authenticated {
                    // rules 8: every other operator action is under that operator's own session
                    match sessions.get(session) {
                        Some((op, _)) if op == operator => {}
                        other => bad(
                            &mut r,
                            e,
                            None,
                            "operator action under a session that is not the operator's",
                            operator.to_string(),
                            json(&other.map(|o| &o.0)),
                        ),
                    }
                    match action.as_str() {
                        "session.closed" => {
                            closed.insert(*session);
                            r.sessions_closed += 1;
                        }
                        "step_up" => {
                            // a credential re-entered: only a credentials run has one, and it is
                            // the kind the session was opened with (a scripted operator's
                            // step-up is scripted too: never a person's); rules 10: a person
                            // (signed in by password or passkey) re-enters their password or
                            // uses their passkey
                            let opened = methods.get(session).copied();
                            let by_kind = if passkey_rules {
                                match (opened, *auth) {
                                    (Some(AuthMethod::Scripted), Some(AuthMethod::Scripted)) => true,
                                    (Some(o), Some(AuthMethod::Password | AuthMethod::Passkey)) => o.is_person(),
                                    _ => false,
                                }
                            } else {
                                auth.is_some() && *auth == opened && *auth != Some(AuthMethod::Asserted)
                            };
                            if run_auth != Some(AuthMode::Credentials) || !by_kind {
                                bad(
                                    &mut r,
                                    e,
                                    None,
                                    "a step-up outside a credentials run, or not by the credential its session was opened with",
                                    json(&(run_auth, auth)),
                                    json(&(AuthMode::Credentials, opened)),
                                );
                            }
                            // rules 10: a passkey's assertion, verified against the run's key for
                            // this session and what it was made for
                            if let Some(u) = proof {
                                if !passkey_rules {
                                    bad(&mut r, e, None, "a passkey step-up under rules that have none", String::new(), String::new());
                                } else if u.purpose == crate::passkey::StepUpFor::Login {
                                    bad(&mut r, e, None, "a login's assertion logged as a step-up", u.credential.clone(), String::new());
                                } else if let Err(why) = passkeys.use_once(operator, Some(*session), e.t, u) {
                                    bad(&mut r, e, None, &format!("passkey step-up: {why}"), u.credential.clone(), String::new());
                                }
                            }
                            match *auth {
                                Some(AuthMethod::Passkey) => r.passkey_step_ups += 1,
                                Some(AuthMethod::Password) => r.password_step_ups += 1,
                                _ => {}
                            }
                            step_ups.insert(
                                e.seq,
                                StepUpSeen {
                                    session: *session,
                                    operator: operator.clone(),
                                    t: e.t,
                                    used: StepUse::Unused,
                                    factor: *auth,
                                    person: opened.is_some_and(AuthMethod::is_person),
                                    purpose: proof.as_ref().map(|u| u.purpose.clone()),
                                },
                            );
                            r.step_ups += 1;
                        }
                        // rules 9: a declaration is the identification ledger's (ident.declared),
                        // never an action record a decision would not read
                        "track.declare" if policy.as_ref().is_some_and(|p| p.rules.rule_id()) => {
                            bad(
                                &mut r,
                                e,
                                None,
                                "a declaration logged as an operator action under rules that declare through the identification ledger",
                                detail.clone(),
                                "ident.declared".into(),
                            );
                        }
                        "track.declare" => {
                            // a hostile declaration steps up when the policy lists it
                            let hostile = crate::engine::declared_affiliation(detail) == Some("hostile");
                            let needed = hostile
                                && policy.as_ref().is_some_and(|p| p.steps_up(policy::StepUpAct::DeclareHostile));
                            check_step_up(
                                &mut r,
                                e,
                                None,
                                needed,
                                operator,
                                *session,
                                *step_up,
                                &mut step_ups,
                                StepUse::Declaration,
                                &Expected::Unbound,
                                policy.as_ref(),
                            );
                        }
                        _ => {}
                    }
                    if step_up.is_some() && action != "track.declare" {
                        bad(
                            &mut r,
                            e,
                            None,
                            "a step-up quoted by an act that takes none",
                            action.clone(),
                            "track.declare".into(),
                        );
                    }
                }
            }
            EventBody::EngagePicture {
                intent,
                inputs,
                results,
                holds,
                ready,
            } => {
                check_accepted(&mut r, e, *intent, &inputs.unknown_accepted, &intents);
                check_ident_inputs(&mut r, e, Some(*intent), &inputs.tracks, &ident, policy.as_ref());
                check_lost_inputs(&mut r, e, *intent, inputs, &lost, &ident, &intents, &mut pictured, policy.as_ref());
                // rules 19: the engine logs a lapse before the step that would read it, so no
                // held picture reads an acceptance as behaving now that it lapses
                if let Some(pol) = policy.as_ref().filter(|p| p.rules.accept_behaving()) {
                    let ec = pol.rules.engage_rules();
                    for (track, a) in &inputs.behaving_accepted {
                        if let Some(l) = engage::behaving_lapse(track, a, inputs.tracks.get(track), inputs.t, &ec) {
                            bad(
                                &mut r,
                                e,
                                Some(*intent),
                                "a held picture reads an acceptance as behaving now its own picture lapses (no lapse logged first)",
                                track.clone(),
                                l.detail,
                            );
                        }
                    }
                }
                let (Some(pol), Some(tr)) = (&policy, intents.get_mut(intent)) else {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "picture without run.started or intent.created",
                        String::new(),
                        String::new(),
                    );
                    continue;
                };
                if let Some(seq) = tr.stop_due {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "picture failed but the engagement went on (void suppressed)",
                        seq.to_string(),
                        "engage.stopped".into(),
                    );
                }
                if let Some((seq, _)) = tr.preload_stop_due {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "the platform's preload did not confirm but the engagement went on",
                        seq.to_string(),
                        "engage.stopped".into(),
                    );
                }
                if !matches!(tr.state, State::Approved | State::Dispatched) {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "picture watch outside approved or dispatched",
                        json(&tr.state),
                        "approved|dispatched".into(),
                    );
                }
                let Some(snap) = tr.snapshot.clone() else {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "picture watch without an approval snapshot",
                        String::new(),
                        String::new(),
                    );
                    continue;
                };
                let w = match resolve(inputs, &areas, &answers) {
                    Ok(w) => w,
                    Err(why) => {
                        bad(&mut r, e, Some(*intent), why, String::new(), String::new());
                        continue;
                    }
                };
                let dispatched = tr.state == State::Dispatched;
                let prev_t = tr.last_picture_t.unwrap_or(snap.t);
                // rules 6: the release needs the platform's preload confirmed
                let preloaded = tr.preload.as_ref().zip(
                    tr.preload_read
                        .filter(|(_, confirmed)| *confirmed)
                        .map(|(t, _)| t),
                );
                let (again, h, rd) = engage::picture(
                    &tr.spec,
                    &snap,
                    &w,
                    &pol.rules,
                    dispatched,
                    prev_t,
                    preloaded,
                );
                tr.last_picture_t = Some(e.t);
                if pol.rules.rule_id() {
                    tr.last_picture_world = Some(w.clone());
                }
                r.pictures_recomputed += 1;
                if &again != results || h != *holds || rd != *ready {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "picture verdict differs",
                        json(&(results, holds, ready)),
                        json(&(&again, h, rd)),
                    );
                }
                tr.ready = h && rd;
                tr.ready_t = tr.ready.then_some(w.t);
                tr.platform_events.clear();
                tr.intercept_reports.clear();
                if !h {
                    r.picture_voids += 1;
                    tr.stop_due = Some(e.seq);
                }
            }
            EventBody::EngageEffect {
                intent,
                uid,
                point,
                radius_m,
                planned_t,
            } => {
                let Some(tr) = intents.get_mut(intent) else {
                    bad(&mut r, e, Some(*intent), "unknown intent", String::new(), String::new());
                    continue;
                };
                // rules 6: never on a platform whose lost-link action was not confirmed as the
                // fallback (the picture's release check says so; this binds it on its own)
                if policy.as_ref().is_some_and(|p| p.rules.rules_version >= 6)
                    && !(tr.preload_set.as_ref().is_some_and(|(_, c)| *c)
                        && tr.preload_read.is_some_and(|(_, c)| c))
                {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "effect released without the platform's lost-link preload confirmed",
                        json(&(&tr.preload_set, tr.preload_read)),
                        "a confirmed engage.preload set, its latest read-back confirmed".into(),
                    );
                }
                // released by the platform the engagement was dispatched to (its dispatch is
                // bound to the nomination's platform), inside the plan's time tolerance
                let platform = tr.spec.uids.first();
                if platform != Some(uid) {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "effect released by another vehicle than the dispatched engagement's platform",
                        uid.to_string(),
                        json(&platform),
                    );
                }
                // (without a ready picture it is flagged below)
                if let (Some(pol), Some(snap), Some(at)) = (&policy, &tr.snapshot, tr.ready_t) {
                    let (from, until) =
                        engage::release_window(&snap.preview, &pol.rules.engage_rules());
                    if !(from <= at && at <= until && from <= e.t) {
                        bad(
                            &mut r,
                            e,
                            Some(*intent),
                            "effect released outside the plan's time tolerance",
                            json(&(e.t, at)),
                            json(&(from, until)),
                        );
                    }
                }
                if !tr.ready || tr.state != State::Dispatched {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "effect without a picture that held and was ready",
                        json(&tr.state),
                        "dispatched, picture holds and ready".into(),
                    );
                }
                // the picture that released it is this step's, not one from before a gap
                if tr.last_picture_t.is_none_or(|p| e.t.ms_since(p) > 1000) {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "effect without a picture checked in the same step",
                        json(&tr.last_picture_t),
                        json(&e.t),
                    );
                }
                let want = tr
                    .snapshot
                    .as_ref()
                    .map(|s| (s.preview.effect_point, s.preview.effect_radius_m, s.preview.effect_t));
                if want != Some((*point, *radius_m, *planned_t)) {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "effect not at the approval's predicted point",
                        json(&(point, radius_m, planned_t)),
                        json(&want),
                    );
                }
                tr.ready = false;
                tr.ready_t = None;
                tr.effect_logged = true;
                tr.effect = Some(EffectRecord {
                    t: e.t,
                    point: *point,
                    radius_m: *radius_m,
                    planned_t: *planned_t,
                });
            }
            EventBody::EngageStopped {
                intent,
                cause,
                reason,
                fallback,
            } => {
                // the order that superseded it, as replay tracked it: its spec and whether it
                // was dispatched
                let queued_ok = policy
                    .as_ref()
                    .is_some_and(|p| p.rules.rules_version >= rules::QUEUE_FOLLOWUPS_SINCE);
                let superseder = match cause {
                    StopCause::Superseded { by, .. } => intents.get(by).map(|t| {
                        // the order dispatched last (rules 7: or queued last) stands; from
                        // rules 7 only in the step that dispatched or queued it
                        let last = if queued_ok {
                            step == Some((*by, e.seq.saturating_sub(1)))
                        } else if t.state == State::Queued {
                            last_queued == Some(*by)
                        } else {
                            last_dispatch == Some(*by)
                        };
                        (t.spec.clone(), t.state, last)
                    }),
                    _ => None,
                };
                // the step goes on past this stop and its transition (the next event)
                if queued_ok && let StopCause::Superseded { by, .. } = cause {
                    step = Some((*by, e.seq + 1));
                }
                let Some(tr) = intents.get_mut(intent) else {
                    bad(&mut r, e, Some(*intent), "unknown intent", String::new(), String::new());
                    continue;
                };
                match cause {
                    StopCause::Picture if tr.stop_due.is_none() => bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "stopped for the picture, but the picture held",
                        String::new(),
                        String::new(),
                    ),
                    // the engine stops a failed picture at once, for the picture: no other
                    // stop can come between
                    _ if tr.stop_due.is_some() && !matches!(cause, StopCause::Picture) => bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "a failed picture's stop reported with another cause than the picture",
                        json(cause),
                        "picture".into(),
                    ),
                    // rules 9: only in the step of the revocation it names (checked above)
                    StopCause::Revoked { .. } if !revoke_stop => bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "stopped for a revoked declaration outside that revocation's step",
                        json(cause),
                        "right after ident.revoked".into(),
                    ),
                    StopCause::Operator { by, session } => match sessions.get(session) {
                        Some((op, _)) if op == by => {}
                        other => bad(
                            &mut r,
                            e,
                            Some(*intent),
                            "abort under a session that is not the operator's",
                            by.to_string(),
                            json(&other.map(|o| &o.0)),
                        ),
                    },
                    _ => {}
                }
                let superseded = matches!(cause, StopCause::Superseded { .. });
                if let StopCause::Superseded { by, source } = cause {
                    match &superseder {
                        None => bad(
                            &mut r,
                            e,
                            Some(*intent),
                            "superseded by an intent that does not exist",
                            by.to_string(),
                            String::new(),
                        ),
                        Some((spec, by_state, by_last)) => {
                            if let Some(why) = supersede_mismatch(
                                *intent,
                                *by,
                                tr.spec.uids.first(),
                                spec,
                                *by_state,
                                *by_last,
                                queued_ok,
                            ) {
                                bad(
                                    &mut r,
                                    e,
                                    Some(*intent),
                                    why,
                                    by.to_string(),
                                    json(&(&spec.uids, by_state, last_dispatch, last_queued)),
                                );
                            }
                            if *source != spec.source {
                                bad(
                                    &mut r,
                                    e,
                                    Some(*intent),
                                    "superseded with another source than its superseding intent's",
                                    json(source),
                                    json(&spec.source),
                                );
                            }
                        }
                    }
                }
                // a stop for a vehicle event the platform reported (its failsafe took over, it
                // left its mission mode) quotes it: the event must be the platform's own
                if let StopCause::System = cause
                    && let Some(quoted) = quoted_vehicle_event(reason)
                    && !tr
                        .platform_events
                        .iter()
                        .any(|(k, t)| (k.as_str(), t.as_str()) == quoted)
                {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "stop quotes a vehicle event its platform did not report",
                        json(&quoted),
                        json(&tr.platform_events),
                    );
                }
                // rules 6: a preload that did not confirm is stopped at once, for it, and a
                // stop that says so follows such a report
                let for_preload = [PRELOAD_FAILED, PRELOAD_CHANGED]
                    .iter()
                    .any(|p| reason.starts_with(&format!("system: {p}")));
                if let Some((_, want)) = tr.preload_stop_due
                    && !(matches!(cause, StopCause::System) && reason.starts_with(&format!("system: {want}")))
                {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "a preload that did not confirm stopped for another reason",
                        reason.clone(),
                        format!("system: {want}..."),
                    );
                }
                if for_preload && tr.preload_stop_due.is_none() {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "stop quotes a preload failure the platform did not report",
                        reason.clone(),
                        "an unconfirmed engage.preload set or verify".into(),
                    );
                }
                tr.preload_stop_due = None;
                // a stop for a failed intercept quotes the intercept's report (the bridge
                // logs it in the same step, just before)
                if let StopCause::System = cause
                    && let Some(rest) = reason.strip_prefix("system: the platform's intercept ")
                    && !tr
                        .intercept_reports
                        .iter()
                        .any(|(s, d)| rest == format!("{s}: {d}"))
                {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "stop quotes an intercept report the engagement did not make",
                        rest.to_string(),
                        json(&tr.intercept_reports),
                    );
                }
                tr.stop_fallback = Some(*fallback);
                let want = match tr.state {
                    State::Dispatched if !superseded => engage::params(&tr.spec).map(|p| p.3),
                    _ => None,
                };
                if *fallback != want {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "stop's fallback is not the recorded one",
                        json(fallback),
                        json(&want),
                    );
                }
                tr.stop_due = None;
                tr.ready = false;
                tr.stopped = true;
            }
            EventBody::EngageObserve {
                intent,
                inputs,
                step,
                covered_s,
            } => {
                check_accepted(&mut r, e, *intent, &inputs.unknown_accepted, &intents);
                check_ident_inputs(&mut r, e, Some(*intent), &inputs.tracks, &ident, policy.as_ref());
                check_lost_inputs(&mut r, e, *intent, inputs, &lost, &ident, &intents, &mut pictured, policy.as_ref());
                let (Some(pol), Some(tr)) = (&policy, intents.get_mut(intent)) else {
                    bad(&mut r, e, Some(*intent), "unknown intent", String::new(), String::new());
                    continue;
                };
                let (Some(snap), Some(effect)) = (tr.snapshot.clone(), tr.effect.clone()) else {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "observation without an effect",
                        String::new(),
                        String::new(),
                    );
                    continue;
                };
                let w = match resolve(inputs, &areas, &answers) {
                    Ok(w) => w,
                    Err(why) => {
                        bad(&mut r, e, Some(*intent), why, String::new(), String::new());
                        continue;
                    }
                };
                let again = engage::observe(&snap, &effect, &w, &pol.rules);
                let covered = !again.covered_by.is_empty();
                let sum = tr.covered_s + engage::coverage_increment(tr.last_observe, e.t, covered);
                if &again != step || sum != *covered_s {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "observation differs",
                        json(&(step, covered_s)),
                        json(&(&again, sum)),
                    );
                }
                tr.covered_s = sum;
                tr.last_observe = Some((e.t, covered));
                tr.last_step = Some((e.t, again));
            }
            EventBody::EngageAssessed {
                intent,
                result,
                evidence,
                covered_s,
            } => {
                let (Some(pol), Some(tr)) = (&policy, intents.get_mut(intent)) else {
                    bad(&mut r, e, Some(*intent), "unknown intent", String::new(), String::new());
                    continue;
                };
                let again = match (&tr.snapshot, &tr.effect, &tr.last_step) {
                    (Some(snap), Some(effect), Some((t, step))) => {
                        engage::assess(snap, effect, step, tr.covered_s, *t, &pol.rules)
                    }
                    _ => None,
                };
                let logged = Some((*result, evidence.clone()));
                if again != logged || *covered_s != tr.covered_s {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "assessment differs",
                        json(&(result, evidence, covered_s)),
                        json(&(again, tr.covered_s)),
                    );
                }
                tr.assessed = true;
                r.assessments += 1;
            }
            EventBody::EngageAccepted {
                intent,
                uid,
                contact,
                track: live_track,
                since,
                by,
                session,
                world_t,
                vehicle,
                lost: lost_logged,
                behaving,
                step_up,
            } => {
                // a vehicle of ours (rules 6), a lost contact (rules 16) or a live track as
                // behaving now (rules 19), exactly one, with its facts
                let subject = match (uid, contact, live_track, vehicle, lost_logged, behaving) {
                    (Some(u), None, None, Some(v), None, None) => Some(Accepting::Vehicle(u, v)),
                    (None, Some(c), None, None, l, None) => Some(Accepting::Contact(c, l.as_deref())),
                    (None, None, Some(t), None, None, Some(f)) => Some(Accepting::Behaving(t, f)),
                    _ => None,
                };
                let Some(subject) = subject else {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "an acceptance names neither one vehicle with its facts nor one lost contact",
                        json(&(uid, contact)),
                        String::new(),
                    );
                    continue;
                };
                // rules 8: the operator's credential re-entered for it, when the policy lists
                // the act (a nomination's acceptances share one step-up)
                let needed = policy.as_ref().is_some_and(|p| p.steps_up(policy::StepUpAct::AcceptUnknown));
                let expected = match &subject {
                    Accepting::Vehicle(u, _) => Expected::Acceptance {
                        intent: *intent,
                        vehicle: crate::passkey::AcceptedVehicle { uid: (*u).clone(), since: *since },
                    },
                    Accepting::Contact(c, _) => Expected::ContactAcceptance {
                        intent: *intent,
                        contact: crate::passkey::AcceptedContact {
                            track: (*c).clone(),
                            since: since.unwrap_or(Time(i64::MIN)),
                        },
                    },
                    Accepting::Behaving(t, _) => Expected::TrackAcceptance {
                        intent: *intent,
                        track: crate::passkey::AcceptedTrack { track: (*t).clone() },
                    },
                };
                check_step_up(
                    &mut r,
                    e,
                    Some(*intent),
                    needed,
                    by,
                    *session,
                    *step_up,
                    &mut step_ups,
                    StepUse::Acceptances { intent: *intent, last_seq: e.seq },
                    &expected,
                    policy.as_ref(),
                );
                let Some(tr) = intents.get_mut(intent) else {
                    bad(&mut r, e, Some(*intent), "acceptance for an unknown intent", String::new(), String::new());
                    continue;
                };
                // what it accepts, for the words (rules 16: a lost contact)
                let what = match &subject {
                    Accepting::Vehicle(..) => "vehicle",
                    Accepting::Contact(..) => "contact",
                    Accepting::Behaving(..) => "track as behaving now (a live",
                };
                // an operator's own session, with engagement authority: an agent has none
                match sessions.get(session) {
                    Some((op, g)) if op == by && g.engage => {}
                    Some((op, _)) if op == by => bad(
                        &mut r,
                        e,
                        Some(*intent),
                        &format!("acceptance of a lost {what} without engagement authority"),
                        by.to_string(),
                        "a session opened with grants.engage".into(),
                    ),
                    other => bad(
                        &mut r,
                        e,
                        Some(*intent),
                        &format!("acceptance of a lost {what} under a session that is not the operator's"),
                        by.to_string(),
                        json(&other.map(|o| &o.0)),
                    ),
                }
                // the vehicle really was lost since that report, on the world logged with it
                if (e.t.0 - world_t.0).abs() > engage::ACCEPT_WORLD_SKEW_MS {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "acceptance's world is not the one at the acceptance",
                        json(world_t),
                        json(&e.t),
                    );
                }
                match &subject {
                    Accepting::Vehicle(_, vehicle) => {
                        if let Some(pol) = &policy
                            && let Err(why) = engage::lost_since_report(Some(vehicle), *since, *world_t, &pol.rules)
                        {
                            bad(
                                &mut r,
                                e,
                                Some(*intent),
                                "acceptance of a vehicle that was not lost since that report",
                                json(&(since, vehicle.position.as_ref().map(|p| p.t))),
                                why,
                            );
                        }
                    }
                    // rules 16: a contact the log holds as lost, since exactly that fix, with the
                    // facts the ledger holds
                    Accepting::Contact(c, logged_facts) => {
                        if !policy.as_ref().is_some_and(|p| p.rules.lost_contacts()) {
                            bad(&mut r, e, Some(*intent), "an acceptance of a lost contact before rules 16", (*c).clone(), String::new());
                        }
                        if let Err(why) = engage::contact_lost_since(c, lost.get(c), *since) {
                            bad(
                                &mut r,
                                e,
                                Some(*intent),
                                "acceptance of a contact that was not lost since that fix",
                                json(since),
                                why,
                            );
                        }
                        let rule_id = policy.as_ref().is_some_and(|p| p.rules.rule_id());
                        let want = lost.filled(&ident, rule_id).remove(c.as_str());
                        if *logged_facts != want.as_ref() {
                            bad(
                                &mut r,
                                e,
                                Some(*intent),
                                "an acceptance's lost contact differs from the drop the log holds",
                                json(logged_facts),
                                json(&want),
                            );
                        }
                    }
                    // rules 19: a live track that could be accepted then (in the picture, in view,
                    // with a velocity, protected), its estimate's time the one named, and its
                    // identification the ledger's
                    Accepting::Behaving(t, facts) => {
                        if !policy.as_ref().is_some_and(|p| p.rules.accept_behaving()) {
                            bad(&mut r, e, Some(*intent), "an acceptance of a live track as behaving now before rules 19", (*t).clone(), String::new());
                        }
                        if let Some(pol) = &policy {
                            let target = engage::params(&tr.spec).map(|p| p.0.to_string()).unwrap_or_default();
                            if let Err(why) =
                                engage::behaving_acceptable(t, Some(facts), *world_t, &target, &pol.rules.engage_rules())
                            {
                                bad(&mut r, e, Some(*intent), "acceptance of a track that could not be accepted as behaving now", (*t).clone(), why);
                            }
                        }
                        if *since != Some(facts.position.t) {
                            bad(
                                &mut r,
                                e,
                                Some(*intent),
                                "an acceptance as behaving now names another estimate than its facts'",
                                json(since),
                                json(&facts.position.t),
                            );
                        }
                        check_ident_inputs(
                            &mut r,
                            e,
                            Some(*intent),
                            &std::iter::once(((*t).clone(), (**facts).clone())).collect(),
                            &ident,
                            policy.as_ref(),
                        );
                    }
                }
                // at the nomination by its own (human) submitter, or while it waits with no
                // signature
                let at_submission = tr.state == State::Proposed
                    && matches!(&tr.spec.source, Source::Human { id } if id == by);
                let waiting = tr.state == State::PendingApproval && tr.signers.is_empty();
                if tr.spec.kind() != TaskKind::Engage || !(at_submission || waiting) {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        &format!("acceptance of a lost {what} outside an engagement's nomination or its wait for a first signature"),
                        json(&(tr.state, &tr.spec.source, tr.signers.len())),
                        "its submitter's nomination, or pending without signatures".into(),
                    );
                }
                let accepted = engage::AcceptedUnknown {
                    since: *since,
                    by: by.clone(),
                };
                match subject {
                    Accepting::Vehicle(u, _) => {
                        tr.accepted.insert(u.clone(), accepted);
                    }
                    Accepting::Contact(c, _) => {
                        tr.accepted_lost.insert(c.clone(), accepted);
                    }
                    Accepting::Behaving(t, facts) => {
                        // one in force is never given again (the engine refuses it): a second
                        // acceptance means the first was revoked or lapsed in between
                        if tr.accepted_behaving.contains_key(t) {
                            bad(
                                &mut r,
                                e,
                                Some(*intent),
                                "a track accepted as behaving now again while an acceptance of it is in force",
                                t.clone(),
                                "revoked or lapsed first".into(),
                            );
                        }
                        tr.accepted_behaving.insert(
                            t.clone(),
                            engage::AcceptedBehaving {
                                by: by.clone(),
                                t: *world_t,
                                facts: facts.clone(),
                            },
                        );
                    }
                }
            }
            EventBody::EngageAcceptanceRevoked {
                intent,
                uid,
                contact,
                track: live_track,
                by,
                session,
            } => {
                check_session(&mut r, e, *intent, by, *session, &sessions, "revocation of an acceptance");
                let Some(tr) = intents.get_mut(intent) else {
                    bad(&mut r, e, Some(*intent), "revocation for an unknown intent", String::new(), String::new());
                    continue;
                };
                let removed = match (uid, contact, live_track) {
                    (Some(u), None, None) => tr.accepted.remove(u).is_some(),
                    (None, Some(c), None) => tr.accepted_lost.remove(c).is_some(),
                    // rules 19: of a live track as behaving now
                    (None, None, Some(t)) => tr.accepted_behaving.remove(t).is_some(),
                    _ => false,
                };
                if !removed
                    || !matches!(tr.state, State::PendingApproval | State::Approved | State::Dispatched)
                {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "revocation of an acceptance that is not in force",
                        json(&(uid, contact, live_track, tr.state)),
                        "an acceptance of it, the engagement pending or under way".into(),
                    );
                }
            }
            // rules 19: a lapse its own facts bear out, of an acceptance in force
            EventBody::EngageAcceptanceLapsed {
                intent,
                track,
                kind,
                detail,
                world_t,
                facts,
            } => {
                let Some(tr) = intents.get_mut(intent) else {
                    bad(&mut r, e, Some(*intent), "lapse for an unknown intent", String::new(), String::new());
                    continue;
                };
                if !policy.as_ref().is_some_and(|p| p.rules.accept_behaving()) {
                    bad(&mut r, e, Some(*intent), "a lapse of an acceptance as behaving now before rules 19", track.clone(), String::new());
                }
                if (e.t.0 - world_t.0).abs() > engage::ACCEPT_WORLD_SKEW_MS {
                    bad(&mut r, e, Some(*intent), "lapse's world is not the one at the lapse", json(world_t), json(&e.t));
                }
                if !matches!(tr.state, State::PendingApproval | State::Approved | State::Dispatched) {
                    bad(&mut r, e, Some(*intent), "a lapse of an engagement no longer in play", json(&tr.state), String::new());
                }
                let Some(a) = tr.accepted_behaving.remove(track) else {
                    bad(&mut r, e, Some(*intent), "a lapse of an acceptance that is not in force", track.clone(), String::new());
                    continue;
                };
                if let Some(pol) = &policy {
                    let again = engage::behaving_lapse(track, &a, facts.as_deref(), *world_t, &pol.rules.engage_rules());
                    if again.as_ref().map(|l| (l.kind, l.detail.as_str())) != Some((*kind, detail.as_str())) {
                        bad(
                            &mut r,
                            e,
                            Some(*intent),
                            "a lapse its facts do not bear out",
                            json(&(kind, detail)),
                            json(&again),
                        );
                    }
                }
                if let Some(f) = facts {
                    check_ident_inputs(
                        &mut r,
                        e,
                        Some(*intent),
                        &std::iter::once((track.clone(), (**f).clone())).collect(),
                        &ident,
                        policy.as_ref(),
                    );
                }
            }
            EventBody::TrackDropped { track, facts, ais } => {
                if !policy.as_ref().is_some_and(|p| p.rules.lost_contacts()) {
                    // the marker check flags it under older rules
                    continue;
                }
                // the tracker's estimate only: the identification is the ledger's
                if facts.declared_by.is_some() || facts.declaration.is_some() || !facts.review_flags.is_empty() {
                    bad(
                        &mut r,
                        e,
                        None,
                        "a dropped track's facts carry an identification (the ledger fills it)",
                        track.clone(),
                        String::new(),
                    );
                }
                if facts.position.t > e.t {
                    bad(&mut r, e, None, "a track dropped with a fix from after the drop", json(&facts.position.t), json(&e.t));
                }
                // the tracker dates its last fix when it last saw the track (`last_seen_t`): a fix
                // after that is no fix the tracker had (the review of the merged branch's second
                // pass: a fix at 179 s on a track last seen at 0 s passed the drop age, and the
                // keep-out reads the fix)
                if facts.position.t > facts.last_seen_t {
                    bad(
                        &mut r,
                        e,
                        None,
                        "a track dropped with a fix after it was last seen",
                        json(&facts.position.t),
                        json(&facts.last_seen_t),
                    );
                }
                // an id is dropped once (the tracker never reuses one)
                if lost.holds(track) {
                    bad(&mut r, e, None, "a track dropped twice (the tracker drops an id once)", track.clone(), String::new());
                }
                // no older fix, and the same fix, as the decisions last saw it; and no newer fix
                // that a picture taken after it did not show
                if let Some((fix, vel)) = pictured.last.get(track) {
                    let shown_t = pictured.shown_t.get(track).copied();
                    if facts.position.t > fix.t
                        && shown_t.is_some_and(|w| w.ms_since(facts.position.t) >= DROP_FIX_SKEW_MS)
                    {
                        bad(
                            &mut r,
                            e,
                            None,
                            "a track dropped with a fix newer than a later picture showed",
                            json(&facts.position),
                            json(&(fix, shown_t)),
                        );
                    }
                    if facts.position.t < fix.t {
                        bad(
                            &mut r,
                            e,
                            None,
                            "a track dropped with an older fix than the decisions had seen",
                            json(&facts.position),
                            json(fix),
                        );
                    } else if facts.position.t == fix.t && (facts.position.value != fix.value || facts.velocity_mps != *vel) {
                        bad(
                            &mut r,
                            e,
                            None,
                            "a track dropped with its last fix changed from the one the decisions had seen",
                            json(&(&facts.position, facts.velocity_mps)),
                            json(&(fix, vel)),
                        );
                    }
                }
                // rules 16: no sooner than the tracker drops a track (the run's drop age), counted
                // from the later of its last sighting and its fix; a run with drops logs the age
                let seen = facts.last_seen_t.max(facts.position.t);
                match track_drop_s {
                    Some(drop_s) if (e.t.ms_since(seen) as f64) < drop_s * 1000.0 - 1.0 => bad(
                        &mut r,
                        e,
                        None,
                        "a track dropped sooner than the tracker's drop age after it was last seen",
                        json(&(e.t.ms_since(seen) as f64 / 1000.0)),
                        json(&drop_s),
                    ),
                    Some(_) => {}
                    None => bad(
                        &mut r,
                        e,
                        None,
                        "a track dropped in a run whose run.started does not log the tracker's drop age (track_drop_s)",
                        track.clone(),
                        String::new(),
                    ),
                }
                let _ = lost.record(track, (**facts).clone(), ais.clone(), e.t);
                pictured.dropped.insert(track.clone(), (facts.position.clone(), facts.velocity_mps));
                r.lost_contacts += 1;
            }
            EventBody::LostClaimed {
                track,
                by_track,
                world_t,
                tracks,
                track_ais,
                evidence,
                blockers,
            } => {
                let Some(pol) = policy.as_ref().filter(|p| p.rules.lost_contacts()) else {
                    continue;
                };
                check_lost_picture(&mut r, e, "a claim", *world_t, tracks, &ident, &mut pictured);
                if lost.claimed(track, by_track) {
                    bad(&mut r, e, None, "a claim raised twice", format!("{track} by {by_track}"), String::new());
                }
                let again = lost
                    .get(track)
                    .and_then(|l| crate::lost::claim_of(track, l, &lost, by_track, tracks, track_ais, &pol.rules.engage_rules()));
                match again {
                    Some(c) if &c.evidence == evidence && &c.blockers == blockers => {}
                    other => bad(
                        &mut r,
                        e,
                        None,
                        "a claim on a lost contact differs from its recomputation",
                        json(&(evidence, blockers)),
                        json(&other.map(|c| (c.evidence, c.blockers))),
                    ),
                }
                lost.claim(track, by_track);
                r.lost_claims += 1;
            }
            EventBody::TrackReacquired {
                track,
                by_track,
                by,
                session,
                world_t,
                tracks,
                track_ais,
                evidence,
            } => {
                let Some(pol) = policy.as_ref().filter(|p| p.rules.lost_contacts()) else {
                    continue;
                };
                // an operator's act, under their own session (never an agent's)
                check_operator(&mut r, e, by, *session, &sessions, "a confirmation of a lost contact");
                check_lost_picture(&mut r, e, "a confirmation", *world_t, tracks, &ident, &mut pictured);
                let again = match lost.get(track) {
                    Some(l) => crate::lost::claim_of(track, l, &lost, by_track, tracks, track_ais, &pol.rules.engage_rules())
                        .ok_or_else(|| format!("{by_track} does not carry {track}'s MMSI in this picture")),
                    None => Err(format!("{track} is not a lost contact in force")),
                };
                match again {
                    Ok(c) if c.confirmable() && &c.evidence == evidence => {}
                    other => bad(
                        &mut r,
                        e,
                        None,
                        "a lost contact confirmed on a claim that does not hold",
                        json(&(by_track, evidence)),
                        json(&other),
                    ),
                }
                lost.claim(track, by_track);
                let _ = lost.confirm(track, by_track, by, e.t);
                r.lost_confirmed += 1;
            }
            EventBody::LostReinstated {
                track,
                by_track,
                reason,
                world_t,
                facts,
            } => {
                if !policy.as_ref().is_some_and(|p| p.rules.lost_contacts()) {
                    continue;
                }
                match lost.confirmed().get(track) {
                    Some(c) if &c.by_track == by_track => {}
                    other => bad(
                        &mut r,
                        e,
                        None,
                        "a reinstatement of a lost contact not confirmed as that track",
                        format!("{track} as {by_track}"),
                        json(&other.map(|c| &c.by_track)),
                    ),
                }
                if (e.t.0 - world_t.0).abs() > engage::ACCEPT_WORLD_SKEW_MS {
                    bad(&mut r, e, None, "a reinstatement's picture is not the one at the reinstatement", json(world_t), json(&e.t));
                }
                // why, from the claimant's facts then (none: it left the picture, by a drop)
                let picture: BTreeMap<String, crate::world::TrackFacts> =
                    facts.iter().map(|f| (by_track.clone(), (**f).clone())).collect();
                if let Err(why) = ident.check_facts(&picture) {
                    bad(&mut r, e, None, "a reinstatement's facts differ from the identification the log holds", why, String::new());
                }
                if facts.is_none() && !pictured.dropped.contains_key(by_track) {
                    bad(&mut r, e, None, "a reinstatement for a claimant that was never dropped", by_track.clone(), String::new());
                }
                let want = crate::lost::void_reason(by_track, &picture);
                if want.as_ref() != Some(reason) {
                    bad(&mut r, e, None, "a reinstatement's reason does not hold", reason.clone(), json(&want));
                }
                lost.reinstate(track);
                r.lost_reinstated += 1;
            }
            EventBody::PlatformRestore { .. } => {
                let followups = policy.as_ref().is_some_and(|p| p.rules.followups());
                platform_restore(&mut r, e, &mut restores, &intents, followups);
            }
            EventBody::RestoreClosed {
                uid,
                values,
                log,
                closed_by,
            } => {
                restore_closed(
                    &mut r,
                    e,
                    (uid.as_ref(), values, log.as_deref(), closed_by),
                    &restores,
                    &intents,
                    &sessions,
                    policy.as_ref(),
                    prev_event,
                );
                let expended = match closed_by {
                    crate::audit::RestoreClosure::Expended { intent } => Some(*intent),
                    crate::audit::RestoreClosure::WrittenOff { .. } => None,
                };
                restores.close(uid.as_ref(), log.as_deref(), expended, Some(e.t));
            }
            EventBody::PlatformUnspent {
                uid,
                spent_by,
                from,
                values,
                operator,
                session,
                note,
                world_t,
                vehicle,
                identity,
                step_up,
            } => {
                // rules 15: a credentials run's un-spend quotes a step-up made for it
                let expected = crate::passkey::StepUpFor::Unspend {
                    uid: uid.clone(),
                    spent_by: *spent_by,
                    from: from.clone(),
                };
                check_step_up(
                    &mut r,
                    e,
                    None,
                    true,
                    operator,
                    *session,
                    *step_up,
                    &mut step_ups,
                    StepUse::Unspend,
                    &Expected::Is(expected),
                    policy.as_ref(),
                );
                platform_unspent(
                    &mut r,
                    e,
                    (uid, *spent_by, from.as_deref(), values),
                    (operator, *session, note, *world_t, vehicle, identity.as_ref()),
                    &restores,
                    &intents,
                    &sessions,
                    policy.as_ref(),
                );
                restores.unspend(uid);
            }
            EventBody::EngagePreload {
                intent,
                uid,
                phase,
                before,
                readback,
                error,
                confirmed,
                vehicle,
            } => {
                let other_holds = holding(&intents, uid, Some(*intent)).is_some();
                let Some(tr) = intents.get_mut(intent) else {
                    bad(&mut r, e, Some(*intent), "preload for an unknown intent", String::new(), String::new());
                    continue;
                };
                if let Some(why) = preload_mismatch(
                    &tr.spec,
                    tr.state,
                    tr.dispatched,
                    tr.preload.as_ref(),
                    tr.preload_intent.as_ref(),
                    tr.preload_set.as_ref().map(|(_, c)| *c),
                    tr.restore_confirmed,
                    other_holds,
                    *phase,
                    uid,
                    before,
                ) {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        why,
                        json(&(uid, phase)),
                        json(&(tr.state, tr.dispatched, &tr.preload, &tr.preload_set)),
                    );
                }
                let verdict = preload_verdict(
                    *phase,
                    tr.preload.as_ref(),
                    tr.preload_set.as_ref().map(|(b, _)| b),
                    before,
                    readback,
                    error.as_deref(),
                );
                if verdict.is_ok() != *confirmed {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "preload confirmation differs from its read-back",
                        confirmed.to_string(),
                        json(&verdict),
                    );
                }
                match phase {
                    engage::PreloadPhase::Intent => {
                        tr.preload_intent = Some(before.clone());
                        // the platform's own values, owed back from the write-ahead on (a
                        // value read off a platform still preloaded is not its own)
                        if !before.is_empty()
                            && let Err(owed) = restores.owe(uid, *intent, before, vehicle.as_deref())
                        {
                            bad(
                                &mut r,
                                e,
                                Some(*intent),
                                "preload read the platform's values while earlier ones were owed back to it",
                                json(before),
                                owed,
                            );
                        }
                    }
                    engage::PreloadPhase::Set => {
                        tr.preload_set = Some((before.clone(), verdict.is_ok()));
                        tr.preload_read = Some((e.t, verdict.is_ok()));
                        if verdict.is_err() && tr.state == State::Dispatched {
                            tr.preload_stop_due = Some((e.seq, PRELOAD_FAILED));
                        }
                        // (the same values as its write-ahead: already owed)
                        if !before.is_empty()
                            && let Err(owed) = restores.owe(uid, *intent, before, None)
                        {
                            bad(
                                &mut r,
                                e,
                                Some(*intent),
                                "preload read the platform's values while earlier ones were owed back to it",
                                json(before),
                                owed,
                            );
                        }
                    }
                    engage::PreloadPhase::Verify => {
                        tr.preload_read = Some((e.t, verdict.is_ok()));
                        if verdict.is_err() && tr.state == State::Dispatched {
                            tr.preload_stop_due = Some((e.seq, PRELOAD_CHANGED));
                        }
                    }
                    engage::PreloadPhase::Restore => {
                        tr.restore_confirmed |= verdict.is_ok();
                        if verdict.is_ok() {
                            restores.paid(uid, readback);
                        }
                    }
                }
            }
            EventBody::IntentQueued {
                intent,
                inputs,
                reach,
                expires_t,
            } => {
                check_restore_owed(&mut r, e, *intent, &inputs.restore_owed, &intents, &restores, policy.as_ref());
                check_expended(&mut r, e, *intent, &inputs.expended, &intents, &restores, policy.as_ref());
                check_spent_carried(&mut r, e, *intent, &inputs.spent_carried, &intents, &restores, policy.as_ref());
                check_ident_inputs(&mut r, e, Some(*intent), &inputs.tracks, &ident, policy.as_ref());
                check_lost_inputs(&mut r, e, *intent, inputs, &lost, &ident, &intents, &mut pictured, policy.as_ref());
                let (Some(pol), Some(tr)) = (&policy, intents.get_mut(intent)) else {
                    bad(&mut r, e, Some(*intent), "queued without run.started or intent.created", String::new(), String::new());
                    continue;
                };
                if !matches!(tr.state, State::Approved | State::AutoApproved) {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "queued without approval",
                        json(&tr.state),
                        "auto_approved|approved".into(),
                    );
                }
                let cfg = pol.queue();
                if !cfg.queues(tr.spec.kind()) {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "queued although the policy never queues its kind",
                        tr.spec.kind().as_str().into(),
                        json(&cfg.expire_s(tr.spec.kind())),
                    );
                }
                let w = match resolve(inputs, &areas, &answers) {
                    Ok(w) => w,
                    Err(why) => {
                        bad(&mut r, e, Some(*intent), why, String::new(), String::new());
                        continue;
                    }
                };
                let again = queue::reach(&tr.spec, &w, &cfg, &pol.rules);
                if &again != reach {
                    bad(&mut r, e, Some(*intent), "reach differs", json(reach), json(&again));
                }
                if queue::all_reachable(&again) {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "queued although every vehicle could be reached",
                        json(reach),
                        json(&again),
                    );
                }
                let basis = tr
                    .basis
                    .clone()
                    .unwrap_or_else(|| crate::engine::no_basis(e.t));
                let (want, inbox) = cfg.bound(cfg.wait_end(tr.spec.kind(), e.t), &basis, e.t);
                if *expires_t != want {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "queue deadline differs from policy",
                        json(expires_t),
                        json(&want),
                    );
                }
                tr.queued_logged = Some(e.seq);
                tr.queue_expires = Some(want);
                tr.queue_inbox = inbox;
            }
            EventBody::QueueRecheck {
                intent,
                reach,
                decision,
                verdict,
                changes,
            } => {
                let (Some(pol), Some(tr)) = (&policy, intents.get_mut(intent)) else {
                    bad(&mut r, e, Some(*intent), "re-check without run.started or intent.created", String::new(), String::new());
                    continue;
                };
                r.rechecks += 1;
                if tr.state != State::Queued {
                    bad(&mut r, e, Some(*intent), "re-check of an intent that is not queued", json(&tr.state), json(&State::Queued));
                }
                // at its deadline the queue lets go: expired (or back to the inbox), never sent
                if let Some(d) = tr.queue_expires
                    && e.t >= d
                {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "re-checked at or after its queue deadline (it must expire)",
                        json(&e.t),
                        format!("before {}", json(&d)),
                    );
                }
                // the checks it judges are the dequeue check just before it
                let Some(w) = tr
                    .last_inputs
                    .as_deref()
                    .filter(|_| {
                        tr.last_phase == Some(CheckPhase::Dequeue) && tr.last_check_seq + 1 == e.seq
                    })
                    .cloned()
                else {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "re-check without its dequeue check just before",
                        json(&(tr.last_phase, tr.last_check_seq)),
                        json(&(CheckPhase::Dequeue, e.seq.saturating_sub(1))),
                    );
                    continue;
                };
                let cfg = pol.queue();
                let again = queue::reach(&tr.spec, &w, &cfg, &pol.rules);
                if &again != reach {
                    bad(&mut r, e, Some(*intent), "reach differs", json(reach), json(&again));
                }
                if !queue::all_reachable(&again) {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "dequeued while a vehicle could not be reached",
                        json(reach),
                        json(&again),
                    );
                }
                let passed = tr.last_check_passed == Some(true);
                let d = passed.then(|| policy::decide(&tr.spec, &w, pol));
                if d.as_ref() != decision.as_ref() {
                    bad(&mut r, e, Some(*intent), "re-check policy decision differs", json(decision), json(&d));
                }
                let basis = tr
                    .basis
                    .clone()
                    .unwrap_or_else(|| crate::engine::no_basis(w.t));
                let since = queue::Since {
                    basis: &basis,
                    carried: tr.basis_carried,
                    rebooted: &rebooted,
                };
                let rc = queue::assess(&tr.spec, &since, &tr.last_results, passed, d.as_ref(), &w);
                if (rc.verdict, &rc.changes) != (*verdict, changes) {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "re-check verdict differs",
                        json(&(verdict, changes)),
                        json(&(rc.verdict, &rc.changes)),
                    );
                }
                match rc.verdict {
                    Verdict::Dispatch => r.recheck_dispatched += 1,
                    Verdict::Inbox => r.recheck_inbox += 1,
                    Verdict::Void => r.recheck_voided += 1,
                }
                tr.recheck = Some(rc.verdict);
                tr.recheck_seq = Some(e.seq);
                if rc.verdict == Verdict::Inbox
                    && let Some(d) = &d
                {
                    // a new decision: who approves it is this decision's, from scratch
                    tr.route(d);
                    tr.back_to_inbox();
                }
            }
            EventBody::QueueExpired {
                intent,
                expires_t,
                inbox,
                ..
            } => {
                let Some(tr) = intents.get_mut(intent) else {
                    bad(&mut r, e, Some(*intent), "unknown intent", String::new(), String::new());
                    continue;
                };
                if tr.state != State::Queued
                    || Some(*expires_t) != tr.queue_expires
                    || e.t < *expires_t
                {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "expired from the queue before its deadline (or not queued)",
                        json(&(e.t, expires_t, tr.state)),
                        json(&tr.queue_expires),
                    );
                }
                if *inbox != tr.queue_inbox {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "queue expiry differs from its deadline's (expired, or back to the inbox for a human's approval held long enough)",
                        json(inbox),
                        json(&tr.queue_inbox),
                    );
                }
                tr.queue_expired = true;
                tr.queue_expired_inbox = *inbox;
                if *inbox {
                    tr.back_to_inbox();
                }
            }
            EventBody::IntentRequeued {
                intent,
                expires_t,
                inbox,
            } => {
                let (Some(pol), Some(tr)) = (&policy, intents.get_mut(intent)) else {
                    bad(&mut r, e, Some(*intent), "requeued without run.started or intent.created", String::new(), String::new());
                    continue;
                };
                let cfg = pol.queue();
                if tr.state != State::Dispatched
                    || pol.rules.rules_version < 5
                    || !cfg.queues(tr.spec.kind())
                {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "requeued, but it is not a dispatched vehicle task the queue holds",
                        json(&tr.state),
                        json(&State::Dispatched),
                    );
                }
                // evidence: every vehicle's op ended with its link lost before the start, and
                // none ever started the order
                let lost = tr.spec.uids.iter().all(|u| {
                    matches!(
                        tr.ops.get(u).map(String::as_str),
                        Some(queue::REQUEUED_OP | queue::UNCONFIRMED_OP)
                    )
                });
                if !lost || !tr.committed.is_empty() {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "requeued without every vehicle's op ending with its link lost before the start (or after a vehicle started it)",
                        json(&(&tr.ops, &tr.committed)),
                        format!(
                            "every op {} or {}, none started",
                            queue::REQUEUED_OP,
                            queue::UNCONFIRMED_OP
                        ),
                    );
                }
                let basis = tr
                    .basis
                    .clone()
                    .unwrap_or_else(|| crate::engine::no_basis(e.t));
                // a delivery that only confirmed it found a vehicle not running it: to a human
                let (want, want_inbox) = if queue::confirm_failed(&tr.confirming, &tr.ops).is_empty() {
                    cfg.bound(cfg.wait_end(tr.spec.kind(), basis.t), &basis, e.t)
                } else {
                    (e.t, true)
                };
                if (*expires_t, *inbox) != (want, want_inbox) {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "requeue deadline differs from policy",
                        json(&(expires_t, inbox)),
                        json(&(want, want_inbox)),
                    );
                }
                tr.requeued_logged = Some(e.seq);
                tr.queue_expires = Some(want);
                tr.queue_inbox = want_inbox;
            }
            EventBody::IntentResumed {
                intent: spec,
                from,
                state,
                basis,
                policy: carried,
                approval,
                expires_t,
                ops,
                committed,
                dispatched_before,
                platforms,
                maybe_started,
            } => {
                r.resumed += 1;
                if intents.contains_key(&spec.id) {
                    bad(&mut r, e, Some(spec.id), "intent created twice", String::new(), String::new());
                    continue;
                }
                // rules 8: carried only between runs that authenticate alike (an asserted
                // approval is no person's in a credentials run; a log before rules 8 asserted)
                if r.rules_version >= OPERATOR_AUTH_SINCE && effective_mode(from.auth) != effective_mode(r.auth) {
                    let here = json(&effective_mode(r.auth));
                    bad(
                        &mut r,
                        e,
                        Some(spec.id),
                        "carried over from a run that authenticated its operators otherwise",
                        json(&effective_mode(from.auth)),
                        here,
                    );
                } else if r.rules_version < OPERATOR_AUTH_SINCE && from.auth.is_some() {
                    bad(
                        &mut r,
                        e,
                        Some(spec.id),
                        "a carried-over run's auth: only builds of rules 8 or later write it",
                        json(&from.auth),
                        String::new(),
                    );
                }
                let open = OpenIntent {
                    spec: spec.clone(),
                    state: from.was,
                    basis: basis.clone(),
                    policy: carried.clone(),
                    approval: approval.clone(),
                    expires_t: *expires_t,
                    ops: ops.clone(),
                    committed: committed.clone(),
                    dispatched_before: *dispatched_before,
                    platforms: platforms.clone(),
                    maybe_started: maybe_started.clone(),
                };
                if !queue::queueable(spec.kind())
                    || !matches!(state, State::Queued | State::Dispatched)
                    || *state != open.resumes_as()
                    || (*state == State::Queued) != expires_t.is_some()
                {
                    bad(
                        &mut r,
                        e,
                        Some(spec.id),
                        "resumed in a state its kind, op states or deadline don't allow",
                        json(&(state, expires_t)),
                        json(&open.resumes_as()),
                    );
                }
                let mut tr = Tracked::new(spec.clone(), *state);
                if let Some(d) = carried {
                    tr.route(d);
                }
                tr.basis = Some(basis.clone());
                tr.basis_carried = true;
                tr.last_results = basis.checks.clone();
                tr.approval_rec = approval.clone();
                tr.ops = ops.clone();
                tr.committed = committed.iter().cloned().collect();
                tr.dispatched_before = *dispatched_before;
                tr.platforms = platforms.clone();
                tr.maybe_started = maybe_started.iter().cloned().collect();
                // a vehicle that had started it is followed (the bridge's Adopt op), never
                // requeued; only one whose start may have gone out is confirmed and may be
                tr.start_went_out = tr.committed.clone();
                if let (Some(pol), Some(by_kind)) = (&policy, *expires_t) {
                    let cfg = pol.queue();
                    // not queued there: its kind's wait from the approval
                    if from.was != State::Queued && by_kind != cfg.wait_end(spec.kind(), basis.t) {
                        bad(
                            &mut r,
                            e,
                            Some(spec.id),
                            "resumed queue deadline differs from policy",
                            json(&by_kind),
                            json(&cfg.wait_end(spec.kind(), basis.t)),
                        );
                    }
                    let (d, inbox) = cfg.bound(by_kind, basis, e.t);
                    tr.queue_expires = Some(d);
                    tr.queue_inbox = inbox;
                }
                if *state == State::Dispatched {
                    tr.dispatched = true;
                    for u in &spec.uids {
                        newest_on.insert(u.clone(), spec.id);
                    }
                }
                resumed.push((e.seq, from.clone(), open, *state));
                intents.insert(spec.id, tr);
            }
            EventBody::VehicleRebooted {
                uid,
                boot_ms_before,
                boot_ms,
                booted_t,
                booted_before_t,
            } => {
                r.reboots += 1;
                rebooted.insert(uid.clone(), e.t);
                if let Some(why) =
                    queue::reboot_mismatch(*boot_ms_before, *boot_ms, *booted_t, *booted_before_t)
                {
                    bad(
                        &mut r,
                        e,
                        None,
                        why,
                        json(&(uid, boot_ms_before, boot_ms, booted_t, booted_before_t)),
                        String::new(),
                    );
                }
            }
            EventBody::OpState {
                intent,
                uid,
                state,
                detail,
                ahead,
            } => {
                let required = policy
                    .as_ref()
                    .is_some_and(|p| p.rules.rules_version >= rules::QUEUE_FOLLOWUPS_SINCE);
                let Some(tr) = intents.get_mut(intent) else {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "op state for an unknown intent",
                        json(&(uid, state)),
                        String::new(),
                    );
                    continue;
                };
                // a newer order took the vehicle since this intent's dispatch
                let newest = newest_on.get(uid);
                let replaced = newest.is_some_and(|n| n != intent);
                let start = StartAhead {
                    ahead: *ahead,
                    required,
                    started: tr.started_ahead.contains(uid),
                };
                if let Some(why) = op_state_mismatch(
                    &tr.spec,
                    tr.state,
                    tr.dispatched,
                    replaced,
                    tr.stop_fallback,
                    uid,
                    state,
                    detail,
                    start,
                ) {
                    let reports_under = op_state_owner(&tr.spec, uid).err();
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        why,
                        json(&(uid, state, detail)),
                        json(&(tr.state, tr.dispatched, reports_under, tr.stop_fallback, newest)),
                    );
                }
                if let Some(s) = state.strip_prefix("intercept ") {
                    tr.intercept_reports.push((s.to_string(), detail.clone()));
                }
                // a start that may have gone out is never requeued: the bridge ends that op
                // `unconfirmed` (a delivery's op only; a later one, which only confirms it,
                // may lose its link before it asks the vehicle)
                if state == queue::REQUEUED_OP && tr.start_went_out.contains(uid) {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "op requeued after its start went out (it may be running: the bridge ends such an op unconfirmed)",
                        json(&(uid, state, detail)),
                        format!("{}, once its start went out", queue::UNCONFIRMED_OP),
                    );
                }
                if queue::maybe_started_op(state) || queue::committed_op(state) {
                    tr.start_went_out.insert(uid.clone());
                }
                if queue::committed_op(state) {
                    tr.committed.insert(uid.clone());
                }
                if queue::maybe_started_op(state) {
                    tr.maybe_started.insert(uid.clone());
                }
                if *ahead {
                    tr.started_ahead.insert(uid.clone());
                }
                // rules 11: the engine's own reports on a space task (a cancel's, a re-check's)
                // are bound to the cancel or check right before them, their details recomputed
                if policy.as_ref().is_some_and(|p| p.rules.space_followups()) && tr.spec.task.is_space() {
                    if tr.cancel_due.is_some() && state != space::CANCELLED_OP {
                        bad(&mut r, e, Some(*intent), "an op state between an operator's cancel of a dispatched space task and its cancelled report", json(&(uid, state)), space::CANCELLED_OP.into());
                    }
                    let recheck_report = state == space::RECHECKED_OP
                        || (state == space::MISSED_OP && detail.starts_with(space::NO_LONGER_FEASIBLE));
                    if tr.recheck_due.is_some() && !recheck_report {
                        bad(&mut r, e, Some(*intent), "an op state between a space task's re-check and its report", json(&(uid, state)), "rechecked or missed".into());
                    }
                    if tr.recheck_lost.is_some() {
                        bad(&mut r, e, Some(*intent), "an op state between a space task's missed report and its failure", json(&(uid, state)), "intent.state fail".into());
                    }
                    if state == space::CANCELLED_OP {
                        match (&tr.cancel_due, &tr.space_order) {
                            (Some(_), Some(order)) => {
                                let want = space::cancel_detail(order, scenario_map.scenario(e.t));
                                if *detail != want {
                                    bad(&mut r, e, Some(*intent), "cancelled report differs from what the cancel means for the dispatched order at that time", detail.clone(), want);
                                }
                                tr.cancel_reported = true;
                            }
                            _ => bad(&mut r, e, Some(*intent), "a cancelled report without an operator's cancel of the dispatched space task right before it", json(&(uid, detail)), "operator.action intent.cancel".into()),
                        }
                    }
                    if recheck_report {
                        match tr.recheck_due.take() {
                            Some((passed, want)) => {
                                if (state == space::RECHECKED_OP) != passed {
                                    bad(&mut r, e, Some(*intent), "a re-check report that contradicts its check", state.clone(), if passed { space::RECHECKED_OP.into() } else { space::MISSED_OP.into() });
                                }
                                if *detail != want {
                                    bad(&mut r, e, Some(*intent), "re-check report differs from the recomputed check", detail.clone(), want.clone());
                                }
                                if passed {
                                    r.space_rechecked += 1;
                                } else {
                                    r.space_missed += 1;
                                    tr.recheck_lost = Some(want);
                                }
                            }
                            None => bad(&mut r, e, Some(*intent), "a re-check report without its dispatched-phase check right before it", json(&(uid, state, detail)), "check.result phase dispatched".into()),
                        }
                    }
                }
                tr.ops.insert(uid.clone(), state.clone());
            }
            EventBody::VehicleEvent { uid, kind, text } => {
                // what a stop of an approved or dispatched engagement on this vehicle may quote
                for tr in intents.values_mut().filter(|t| {
                    t.spec.kind() == TaskKind::Engage
                        && matches!(t.state, State::Approved | State::Dispatched)
                        && t.spec.uids.first() == Some(uid)
                }) {
                    tr.platform_events.push((kind.clone(), text.clone()));
                }
            }
            EventBody::IdentAct {
                act,
                inputs,
                candidates,
                verdict,
            } => {
                let Some(pol) = policy.as_ref().filter(|p| p.rules.rule_id()) else {
                    // the marker check flags it under older rules
                    continue;
                };
                let cfg = pol.identification();
                r.acts += 1;
                if ident.seen_act(&act.act) {
                    bad(&mut r, e, None, "a hostile act reported twice", act.act.clone(), String::new());
                }
                if !(act.sigma_m.is_finite() && act.sigma_m > 0.0) || act.sensors.is_empty() {
                    bad(
                        &mut r,
                        e,
                        None,
                        "a hostile act without the sensors that saw it or a position error",
                        json(&(&act.sensors, act.sigma_m)),
                        String::new(),
                    );
                }
                if let Err(why) = ident.check_facts(&inputs.tracks) {
                    bad(&mut r, e, None, "an act's track picture differs from the identification the log holds", why, String::new());
                }
                let again = crate::ident::attribute(act, &inputs.tracks, inputs.t, &cfg);
                if &again != candidates {
                    bad(&mut r, e, None, "an act's attribution differs", json(candidates), json(&again));
                }
                // the victim, if one of our vehicles, among the fleet the act's inputs name
                let fleet_needed = matches!(act.against, crate::ident::Against::Vehicle { .. });
                if !fleet_needed && !inputs.vehicles.is_empty() {
                    bad(&mut r, e, None, "an act not against a vehicle of ours logs a fleet", json(&inputs.vehicles), String::new());
                }
                let v = crate::ident::verdict(act, &again, &inputs.tracks, &inputs.vehicles, &ident, inputs.t, &cfg);
                if &v != verdict {
                    bad(&mut r, e, None, "an act's verdict differs", json(verdict), json(&v));
                }
                match v {
                    crate::ident::ActVerdict::Declare {
                        track,
                        attribution,
                        corroborated_by,
                    } => {
                        ident_due = Some((
                            e.seq,
                            IdentDue::Declare {
                                act: act.clone(),
                                track,
                                attribution,
                                corroborated_by,
                            },
                        ));
                    }
                    crate::ident::ActVerdict::Flag { kind, tracks, why } => {
                        ident_due = Some((
                            e.seq,
                            IdentDue::Flag {
                                act: act.clone(),
                                kind,
                                tracks,
                                why,
                            },
                        ));
                    }
                    crate::ident::ActVerdict::Recorded { .. } => {}
                }
                ident.step(e.t, &e.body);
            }
            EventBody::IdentDeclared { declaration } => {
                let Some(pol) = policy.as_ref().filter(|p| p.rules.rule_id()) else {
                    continue;
                };
                let d = &**declaration;
                match &d.by {
                    crate::ident::Declarer::Rule { .. } => {
                        // only right after the act whose recomputed verdict makes it, exactly as
                        // the rule makes it
                        let want = match ident_due.take() {
                            Some((
                                _,
                                IdentDue::Declare {
                                    act,
                                    track,
                                    attribution,
                                    corroborated_by,
                                },
                            )) => Some(crate::ident::rule_declaration(
                                ident.next_declaration(),
                                e.t,
                                &act,
                                &track,
                                attribution,
                                corroborated_by,
                                &pol.identification(),
                            )),
                            _ => None,
                        };
                        if want.as_ref() != Some(d) {
                            bad(
                                &mut r,
                                e,
                                None,
                                "a rule declaration its act does not make (no act, an attribution below the threshold, another track or evidence)",
                                json(d),
                                json(&want),
                            );
                        }
                        r.rule_declarations += 1;
                    }
                    crate::ident::Declarer::Operator {
                        operator,
                        session,
                        flag,
                        step_up,
                        ..
                    } => {
                        check_operator(&mut r, e, operator, *session, &sessions, "a declaration");
                        if d.id != ident.next_declaration() || d.t != e.t || d.confirmed.is_some() {
                            bad(
                                &mut r,
                                e,
                                None,
                                "an operator's declaration out of sequence (its number, time or a confirmation it can't have)",
                                json(&(d.id, d.t, &d.confirmed)),
                                json(&(ident.next_declaration(), e.t)),
                            );
                        }
                        if let Some(f) = flag {
                            let ok = ident.flag(*f).is_some_and(|(fl, s)| {
                                s.in_force() && (fl.tracks.is_empty() || fl.tracks.contains(&d.track))
                            });
                            if !ok {
                                bad(&mut r, e, None, "a declaration answering a flag not in force on its track", format!("F-{f}"), d.track.clone());
                            }
                        }
                        let hostile = d.affiliation == crate::world::Affiliation::Hostile;
                        let needed = hostile && pol.steps_up(policy::StepUpAct::DeclareHostile);
                        let expected = if hostile {
                            Expected::Is(crate::passkey::StepUpFor::DeclareHostile { track: d.track.clone() })
                        } else {
                            Expected::Unbound
                        };
                        check_step_up(
                            &mut r,
                            e,
                            None,
                            needed,
                            operator,
                            *session,
                            *step_up,
                            &mut step_ups,
                            StepUse::Declaration,
                            &expected,
                            Some(pol),
                        );
                        r.operator_declarations += 1;
                    }
                }
                if let Some(cur) = ident.declaration(&d.track) {
                    bad(
                        &mut r,
                        e,
                        None,
                        "a declaration over one in force (it must be revoked first)",
                        d.track.clone(),
                        cur.describe(),
                    );
                }
                ident.step(e.t, &e.body);
            }
            EventBody::IdentConfirmed {
                track,
                declaration,
                by,
                session,
                step_up,
            } => {
                let Some(pol) = policy.as_ref().filter(|p| p.rules.rule_id()) else {
                    continue;
                };
                check_operator(&mut r, e, by, *session, &sessions, "a confirmation");
                match ident.declaration(track) {
                    Some(d) if d.id == *declaration && d.unconfirmed_rule() => {}
                    other => bad(
                        &mut r,
                        e,
                        None,
                        "a confirmation of no rule declaration awaiting one",
                        format!("{track} D-{declaration}"),
                        json(&other),
                    ),
                }
                let needed = pol.steps_up(policy::StepUpAct::ConfirmDeclaration);
                let expected = Expected::Is(crate::passkey::StepUpFor::ConfirmDeclaration {
                    track: track.clone(),
                    declaration: *declaration,
                });
                check_step_up(
                    &mut r,
                    e,
                    None,
                    needed,
                    by,
                    *session,
                    *step_up,
                    &mut step_ups,
                    StepUse::Declaration,
                    &expected,
                    Some(pol),
                );
                r.confirmations += 1;
                ident.step(e.t, &e.body);
            }
            EventBody::IdentRevoked {
                track,
                declaration,
                by,
                session,
                engagements,
                ..
            } => {
                if !policy.as_ref().is_some_and(|p| p.rules.rule_id()) {
                    continue;
                }
                check_operator(&mut r, e, by, *session, &sessions, "a revocation");
                if ident.declaration(track).map(|d| d.id) != Some(*declaration) {
                    bad(
                        &mut r,
                        e,
                        None,
                        "a revocation of a declaration not in force",
                        format!("{track} D-{declaration}"),
                        json(&ident.declaration(track)),
                    );
                }
                let mut want = crate::ident::engagements_on(
                    intents.iter().map(|(id, t)| (*id, &t.spec, t.state)),
                    track,
                );
                // the bystanders: engagements under way whose keep-out the declaration exempted
                if let Some(pol) = policy.as_ref() {
                    let mut after = ident.clone();
                    after.step(e.t, &e.body);
                    want.extend(crate::ident::revocation_bystanders(
                        intents
                            .iter()
                            .filter(|(_, t)| matches!(t.state, State::Approved | State::Dispatched))
                            .filter_map(|(id, t)| {
                                let snap: &engage::EngageSnapshot = t.snapshot.as_deref()?;
                                let w = t.last_picture_world.as_ref()?;
                                Some((*id, snap.track_id.as_str(), snap, w))
                            }),
                        track,
                        &ident,
                        &after,
                        &pol.rules,
                    ));
                    want.sort();
                    want.dedup();
                }
                if *engagements != want {
                    bad(&mut r, e, None, "a revocation names other engagements than those in play on its track", json(engagements), json(&want));
                }
                if !want.is_empty() {
                    revoke_due = Some(RevokeDue {
                        track: track.clone(),
                        declaration: *declaration,
                        by: by.clone(),
                        session: *session,
                        remaining: want,
                        stopping: None,
                    });
                }
                r.revocations += 1;
                ident.step(e.t, &e.body);
            }
            EventBody::IdentFlagged { flag, inputs } => {
                let Some(pol) = policy.as_ref().filter(|p| p.rules.rule_id()) else {
                    continue;
                };
                let cfg = pol.identification();
                match inputs {
                    // a flag an act's verdict raises, as that verdict makes it
                    None => {
                        let want = match ident_due.take() {
                            Some((_, IdentDue::Flag { act, kind, tracks, why })) => Some(
                                crate::ident::act_flag(ident.next_flag(), e.t, &act, kind, &tracks, &why),
                            ),
                            _ => None,
                        };
                        if want.as_ref() != Some(flag) {
                            bad(&mut r, e, None, "a flag its act does not raise", json(flag), json(&want));
                        }
                    }
                    // the weak-evidence rule, recomputed on the two tracks' logged facts
                    Some(i) => {
                        if let Err(why) = ident.check_facts(&i.tracks) {
                            bad(&mut r, e, None, "a flag's track picture differs from the identification the log holds", why, String::new());
                        }
                        let found = crate::ident::escort(&i.tracks, &ident, i.t, &cfg);
                        let ok = flag.kind == crate::ident::FlagKind::Proximity
                            && flag.act.is_none()
                            && flag.id == ident.next_flag()
                            && flag.t == e.t
                            && flag.tracks.len() == 1
                            && found.iter().any(|f| {
                                Some(&f.track) == flag.tracks.first()
                                    && Some(&f.hostile) == flag.related.as_ref()
                                    && f.evidence == flag.evidence
                            });
                        if !ok {
                            bad(&mut r, e, None, "a proximity flag its two tracks' facts do not raise", json(flag), json(&found));
                        }
                    }
                }
                r.flags += 1;
                ident.step(e.t, &e.body);
            }
            EventBody::IdentFlagResolved {
                flag,
                resolution,
                by,
                session,
                ..
            } => {
                if !policy.as_ref().is_some_and(|p| p.rules.rule_id()) {
                    continue;
                }
                check_operator(&mut r, e, by, *session, &sessions, "a flag's resolution");
                let ok = ident.flag(*flag).is_some_and(|(_, s)| match resolution {
                    crate::ident::FlagResolution::Confirmed => matches!(s, crate::ident::FlagStatus::Open),
                    crate::ident::FlagResolution::Dismissed => s.in_force(),
                });
                if !ok {
                    bad(&mut r, e, None, "a flag resolved that is not open (or, dismissed, not in force)", format!("F-{flag}"), json(&ident.flag(*flag).map(|f| &f.1)));
                }
                r.flags_resolved += 1;
                ident.step(e.t, &e.body);
            }
            EventBody::AgentAction { .. } => {}
            // a display event decides nothing; it must be under the operator's own session
            // and about an intent the log holds
            EventBody::OperatorView {
                operator,
                session,
                intent,
                ..
            } => {
                match sessions.get(session) {
                    Some((op, _)) if op == operator => {}
                    other => bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "view under a session that is not the operator's",
                        operator.to_string(),
                        json(&other.map(|o| &o.0)),
                    ),
                }
                if !intents.contains_key(intent) {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "view of an intent the log does not hold",
                        intent.to_string(),
                        String::new(),
                    );
                }
            }
        }
    }
    let last_seq = events.last().map_or(0, |e| e.seq);
    for (id, tr) in &intents {
        if let Some(seq) = tr.dispatch_logged
            && seq < last_seq
        {
            r.mismatches.push(Mismatch {
                seq,
                intent: Some(*id),
                what: "intent.dispatched never followed by its dispatch transition".into(),
                logged: String::new(),
                recomputed: "intent.state dispatch".into(),
            });
        }
        if let Some(seq) = tr.stop_due
            && seq < last_seq
        {
            r.mismatches.push(Mismatch {
                seq,
                intent: Some(*id),
                what: "picture failed but the engagement was never stopped (void suppressed)"
                    .into(),
                logged: String::new(),
                recomputed: "engage.stopped".into(),
            });
        }
        // rules 11: a cancel, a re-check and a missed report each call for what follows them
        if let Some((_, seq)) = &tr.cancel_due {
            r.mismatches.push(Mismatch {
                seq: *seq,
                intent: Some(*id),
                what: "an operator's cancel of a dispatched space task never carried out".into(),
                logged: String::new(),
                recomputed: "op.state cancelled, intent.state cancel".into(),
            });
        }
        if tr.recheck_due.is_some() {
            r.mismatches.push(Mismatch {
                seq: tr.last_check_seq,
                intent: Some(*id),
                what: "a space task's re-check never reported".into(),
                logged: String::new(),
                recomputed: "op.state rechecked or missed".into(),
            });
        }
        if tr.recheck_lost.is_some() {
            r.mismatches.push(Mismatch {
                seq: last_seq,
                intent: Some(*id),
                what: "a space task reported missed but never failed".into(),
                logged: String::new(),
                recomputed: "intent.state fail".into(),
            });
        }
    }
    // rules 9: an act's verdict or a revocation's stops the log ends before
    if let Some((seq, due)) = &ident_due {
        r.mismatches.push(Mismatch {
            seq: *seq,
            intent: None,
            what: "a hostile act's verdict never carried out (the log ends first)".into(),
            logged: String::new(),
            recomputed: due.what(),
        });
    }
    if let Some(due) = &revoke_due
        && (!due.remaining.is_empty() || due.stopping.is_some())
    {
        r.mismatches.push(Mismatch {
            seq: last_seq,
            intent: due.remaining.first().copied().or(due.stopping),
            what: "a revoked declaration's engagements never stopped (the log ends first)".into(),
            logged: String::new(),
            recomputed: json(&due.remaining),
        });
    }
    r.intents = intents.len();
    r.last_seq = last_seq;
    r.next_intent = intents.keys().map(|i| i.0 + 1).max().unwrap_or(1);
    r.open = intents
        .values()
        .filter(|t| {
            queue::queueable(t.spec.kind())
                && matches!(
                    t.state,
                    State::Queued | State::Approved | State::AutoApproved | State::Dispatched
                )
        })
        .filter_map(|t| {
            Some(OpenIntent {
                spec: t.spec.clone(),
                state: t.state,
                basis: t.basis.clone()?,
                policy: t.decision.clone(),
                approval: t.approval_rec.clone(),
                expires_t: if t.state == State::Queued {
                    t.queue_expires
                } else {
                    None
                },
                ops: t.ops.clone(),
                committed: t.committed.iter().cloned().collect(),
                dispatched_before: t.dispatched_before,
                platforms: t.platforms.clone(),
                maybe_started: t.maybe_started.iter().cloned().collect(),
            })
        })
        .collect();
    if !resumed.is_empty() {
        check_resumed(&mut r, &resumed, opts);
    }
    check_carried_restores(&mut r, events, opts);
    check_carried_from(&mut r, events, opts);
    r.unrestored = restores.report();
    r
}

/// Rules 12: a decision's logged `expended` fact (the intent's vehicles spent by their own
/// effect) against the ledger replay keeps.
fn check_expended(
    r: &mut ReplayReport,
    e: &Event,
    id: IntentId,
    logged: &BTreeMap<Uid, IntentId>,
    intents: &BTreeMap<IntentId, Tracked>,
    restores: &Restores,
    policy: Option<&Policy>,
) {
    let (Some(tr), Some(pol)) = (intents.get(&id), policy) else { return };
    let want = restores.expended_for(&tr.spec, &pol.rules);
    if *logged != want {
        bad(r, e, Some(id), "spent platforms differ from the log's ledger", json(logged), json(&want));
    }
}

/// Rules 15: a decision's logged `spent_carried` fact (the intent's vehicles spent in an earlier
/// run) against the ledger replay keeps, with the identity of the vehicle now it logs (the
/// bridge's input; whether it holds is recomputed from it).
fn check_spent_carried(
    r: &mut ReplayReport,
    e: &Event,
    id: IntentId,
    logged: &BTreeMap<Uid, queue::CarriedSpent>,
    intents: &BTreeMap<IntentId, Tracked>,
    restores: &Restores,
    policy: Option<&Policy>,
) {
    let (Some(tr), Some(pol)) = (intents.get(&id), policy) else { return };
    let want = restores.spent_carried_for(&tr.spec, &pol.rules, &spent_identities(logged));
    if *logged != want {
        bad(r, e, Some(id), "spent platforms carried over differ from the log's ledger", json(logged), json(&want));
    }
}

/// Rules 15: the identities of the vehicles now a decision's `spent_carried` fact names.
fn spent_identities(spent: &BTreeMap<Uid, queue::CarriedSpent>) -> BTreeMap<Uid, engage::VehicleIdentity> {
    spent
        .iter()
        .filter_map(|(u, s)| Some((u.clone(), s.now.clone()?)))
        .collect()
}

/// Rules 15: a `platform.unspent`: under these rules only; under the operator's own session,
/// with engagement authority; with a note; for a platform the ledger has spent, naming exactly
/// its mark (the engagement, the log of a carried one, the values it closed: owed back again);
/// at the world's time, which is the event's; and one the rules take on the facts and identity
/// it logs ([`SpentMark::refusal`]: the engagement ended, the platform heard after its end and
/// not silent at the un-spend, the vehicle the one spent).
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn platform_unspent(
    r: &mut ReplayReport,
    e: &Event,
    (uid, spent_by, from, values): (&Uid, IntentId, Option<&str>, &BTreeMap<String, f64>),
    (operator, session, note, world_t, vehicle, identity): (
        &OperatorId,
        SessionId,
        &str,
        Time,
        &crate::world::VehicleFacts,
        Option<&engage::VehicleIdentity>,
    ),
    restores: &Restores,
    intents: &BTreeMap<IntentId, Tracked>,
    sessions: &BTreeMap<SessionId, (OperatorId, Grants)>,
    policy: Option<&Policy>,
) {
    if !policy.is_some_and(|p| p.rules.carry_followups()) {
        bad(r, e, None, "a platform un-spent under rules that have no un-spend (rules 15)", uid.to_string(), String::new());
    }
    match sessions.get(&session) {
        Some((op, g)) if op == operator && g.engage => {}
        Some((op, _)) if op == operator => bad(
            r,
            e,
            None,
            "a platform un-spent without engagement authority",
            operator.to_string(),
            "a session opened with grants.engage".into(),
        ),
        other => bad(
            r,
            e,
            None,
            "a platform un-spent under a session that is not the operator's",
            operator.to_string(),
            json(&other.map(|o| &o.0)),
        ),
    }
    if note.trim().is_empty() {
        bad(r, e, None, "an un-spend without a note saying why", uid.to_string(), "a note".into());
    }
    let Some(mark) = restores.spent_mark(uid) else {
        bad(r, e, None, "an un-spend of a platform that is not spent", uid.to_string(), String::new());
        return;
    };
    if mark.by != spent_by || mark.from.as_deref() != from || mark.values != *values {
        bad(
            r,
            e,
            None,
            "an un-spend that does not name the platform's spent mark (the engagement, its log, the values it closed)",
            json(&(spent_by, from, values)),
            json(&(mark.by, &mark.from, &mark.values)),
        );
    }
    if (e.t.0 - world_t.0).abs() > engage::ACCEPT_WORLD_SKEW_MS {
        bad(r, e, None, "an un-spend's world is not the one at the un-spend", json(&world_t), json(&e.t));
    }
    // the engagement's end: its terminal transition in this log (a carried mark: this run's start)
    let ended = intents.get(&mark.by).and_then(|t| t.ended_t);
    if let Some(pol) = policy
        && let Some(why) = mark.refusal(ended, Some(vehicle), identity, world_t, &pol.rules)
    {
        bad(
            r,
            e,
            None,
            "an un-spend the rules refuse on the facts it logs (rules 15)",
            why,
            json(&(vehicle.heard_t(), ended, identity)),
        );
    }
    r.unspent.push(format!(
        "vehicle {uid}: un-spent by {operator} (spent by engagement {spent_by}{}): {note}",
        from.map(|f| format!(" of {f}")).unwrap_or_default()
    ));
}

/// Rules 12: the identities of the vehicles a decision's `restore_owed` fact names for values
/// carried over (the bridge's input; replay recomputes the hold from them and the ledger).
fn carried_identities(owed: &BTreeMap<Uid, queue::RestoreOwed>) -> BTreeMap<Uid, engage::VehicleIdentity> {
    owed.iter()
        .filter_map(|(u, o)| Some((u.clone(), o.carried.as_ref()?.now.clone()?)))
        .collect()
}

/// Rules 12: a `restore.closed`: under these rules only; one platform's values, exactly what
/// the ledger owed it, or one earlier log whose carry failed; `expended` right after the
/// release of an engagement on that platform whose release picture shows the platform spent
/// by its effect (`expended_by_effect`), never for a log; `written_off` by an operator under
/// their own live session, with a note, never while an engagement is under way on the
/// platform.
#[allow(clippy::too_many_arguments)]
fn restore_closed(
    r: &mut ReplayReport,
    e: &Event,
    (uid, values, log, closed_by): (Option<&Uid>, &BTreeMap<String, f64>, Option<&str>, &crate::audit::RestoreClosure),
    restores: &Restores,
    intents: &BTreeMap<IntentId, Tracked>,
    sessions: &BTreeMap<SessionId, (OperatorId, Grants)>,
    policy: Option<&Policy>,
    before: Option<&Event>,
) {
    if !policy.is_some_and(|p| p.rules.followups()) {
        bad(r, e, None, "a restore closed under rules that close none (rules 12)", String::new(), String::new());
    }
    let what = match (uid, log) {
        (Some(u), _) => format!("vehicle {u}: its own lost-link values {}", json(values)),
        (None, Some(l)) => format!("earlier log {l}: what it owed (its carry had failed)"),
        _ => "nothing named".to_string(),
    };
    r.restores_closed.push(match closed_by {
        crate::audit::RestoreClosure::Expended { intent } => {
            format!("{what}: no longer owed, the platform was expended by its own effect (engagement {intent})")
        }
        crate::audit::RestoreClosure::WrittenOff { operator, note, .. } => {
            format!("{what}: written off by {operator}: {note}")
        }
    });
    match (uid, log) {
        (Some(u), None) => {
            // an expended platform's closure closes what was owed, possibly nothing (it marks the
            // platform spent either way); a write-off closes values owed
            let owed = restores.values_owed(u);
            let fits = match closed_by {
                crate::audit::RestoreClosure::Expended { .. } => owed.cloned().unwrap_or_default() == *values,
                crate::audit::RestoreClosure::WrittenOff { .. } => owed == Some(values),
            };
            if !fits {
                bad(r, e, None, "a restore closed for values not owed back to the platform", json(values), json(&owed));
            }
        }
        (None, Some(l)) => {
            if !restores.is_pending(l) || !values.is_empty() {
                bad(r, e, None, "a carry closed for a log that is not pending (its carry did not fail, or it was closed)", l.to_string(), json(&restores.pending()));
            }
        }
        _ => bad(r, e, None, "a restore.closed names one platform or one earlier log", json(&(uid, log)), String::new()),
    }
    match closed_by {
        crate::audit::RestoreClosure::Expended { intent } => {
            let released = before.is_some_and(|b| {
                matches!(&b.body, EventBody::IntentState { intent: i, transition: Transition::Release, .. } if i == intent)
            });
            let tr = intents.get(intent);
            let on_platform = tr.is_some_and(|t| t.spec.kind() == TaskKind::Engage && t.spec.uids.first() == uid);
            let spent = tr
                .and_then(|t| t.last_picture_world.as_ref())
                .zip(uid)
                .and_then(|(w, u)| w.vehicles.get(u))
                .and_then(|v| v.platform.as_ref())
                .is_some_and(|p| p.expended_by_effect);
            if !released || !on_platform || !spent || log.is_some() {
                bad(
                    r,
                    e,
                    Some(*intent),
                    "a restore closed as expended, not right after the release of an engagement on that platform whose release picture shows it spent by its effect",
                    json(&(uid, intent)),
                    json(&(released, on_platform, spent)),
                );
            }
        }
        crate::audit::RestoreClosure::WrittenOff { operator, session, note } => {
            match sessions.get(session) {
                Some((op, _)) if op == operator => {}
                other => bad(
                    r,
                    e,
                    None,
                    "a write-off under a session that is not the operator's",
                    operator.to_string(),
                    json(&other.map(|o| &o.0)),
                ),
            }
            if note.trim().is_empty() {
                bad(r, e, None, "a write-off without a note saying why", String::new(), "a note".into());
            }
            if let Some(u) = uid
                && let Some(other) = holding(intents, u, None)
            {
                bad(
                    r,
                    e,
                    Some(other),
                    "a write-off while an engagement is under way on the platform (its own restore is still to come)",
                    u.to_string(),
                    String::new(),
                );
            }
        }
    }
}

/// Rules 12: the bare file names of the logs a log's `run.started` says it carried over from
/// (`carried_from`), those it read and those it could not (checked when at hand): what
/// `murmur-replay` looks for, beside the log or given with `--prev`. Empty when the first line
/// is not `run.started` or carried nothing.
pub fn carried_from_logs(text: &str) -> Vec<String> {
    let Some(first) = text.lines().find(|l| !l.trim().is_empty()) else {
        return Vec::new();
    };
    match serde_json::from_str::<Event>(first) {
        Ok(Event {
            body: EventBody::RunStarted { carried_from, .. },
            ..
        }) => carried_from
            .into_iter()
            .map(|c| c.log)
            .filter(|l| !is_pointer_name(l))
            .collect(),
        _ => Vec::new(),
    }
}

/// Rules 15 (murmur-zq3): how a void's reason says a newer order superseded it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Superseding {
    /// `system: superseded: iN is a newer order for vehicle U` (a dequeue): iN stands (approved,
    /// queued, dispatched, or a person's waiting for approval).
    Standing,
    /// `system: superseded: iN was dispatched to vehicle U` (the bridge, at iN's dispatch).
    Dispatched,
    /// `system: superseded: iN was queued for vehicle U` (the bridge, at iN's queueing).
    Queued,
    /// `superseded by iN` or `superseded by iN (agent A)` (an amend).
    Amend,
}

/// Rules 15 (murmur-zq3): the order a void's reason says superseded it, the vehicle it names and
/// how, as the engine and the bridge word it ([`Superseding`]). None: not a superseded reason;
/// `Some((None, ..))`: one that names no order.
fn superseded_by(reason: &str) -> Option<(Option<IntentId>, Option<Uid>, Superseding)> {
    let id = |t: &str| -> Option<IntentId> {
        let digits: String = t.strip_prefix('i')?.chars().take_while(char::is_ascii_digit).collect();
        let rest = &t[1 + digits.len()..];
        if digits.is_empty() || !(rest.is_empty() || rest.starts_with(' ')) {
            return None;
        }
        digits.parse().ok().map(IntentId)
    };
    if let Some(rest) = reason.strip_prefix("system: superseded: ") {
        let (vehicle, how) = [
            ("is a newer order for vehicle ", Superseding::Standing),
            ("was dispatched to vehicle ", Superseding::Dispatched),
            ("was queued for vehicle ", Superseding::Queued),
        ]
        .iter()
        .find_map(|(k, how)| rest.split_once(k).map(|(_, v)| (v.split_whitespace().next().map(Uid::new), *how)))
        .unwrap_or((None, Superseding::Standing));
        return Some((id(rest), vehicle, how));
    }
    let rest = reason.strip_prefix("superseded by ")?;
    Some((id(rest), None, Superseding::Amend))
}

/// Rules 15: why a void's superseded reason does not hold against the log, if it does not: it
/// names no order, one the log does not have, one not newer than the voided, or (naming a
/// vehicle) one not on that vehicle, or one not in the state the reason says (dispatched,
/// queued, or standing as the engine's dequeue judges it), or (an amend) another kind of task.
fn superseded_mismatch(
    id: IntentId,
    spec: &IntentSpec,
    reason: &str,
    intents: &BTreeMap<IntentId, Tracked>,
) -> Option<&'static str> {
    let (named, vehicle, how) = superseded_by(reason)?;
    let Some(by) = named else {
        return Some("a void as superseded that names no order (rules 15)");
    };
    let Some(other) = intents.get(&by) else {
        return Some("a void as superseded by an order the log does not have (rules 15)");
    };
    if by <= id {
        return Some("a void as superseded by an order that is not newer (rules 15)");
    }
    match vehicle {
        Some(u) if !(spec.uids.contains(&u) && other.spec.uids.contains(&u)) => {
            return Some("a void as superseded by an order not on that vehicle (rules 15)");
        }
        None if how == Superseding::Amend && other.spec.kind() != spec.kind() => {
            return Some("a void as superseded by an amend of another kind of task (rules 15)");
        }
        _ => {}
    }
    let in_state = match how {
        Superseding::Dispatched => other.dispatched,
        Superseding::Queued => other.state == State::Queued,
        Superseding::Standing => {
            other.spec.kind() != TaskKind::Engage
                && match other.state {
                    State::Approved | State::AutoApproved | State::Queued | State::Dispatched => true,
                    State::PendingApproval => !other.spec.source.is_agent(),
                    _ => false,
                }
        }
        Superseding::Amend => true,
    };
    (!in_state).then_some("a void as superseded by an order not in the state its reason says (rules 15)")
}

/// Rules 12 (review L6): whether `reason` voids an approved order of a kind never queued for
/// one of the other things that can end it before its dispatch while its platform's own
/// values hold it, as the bridge gives them: its task layer can't take the order, one of its
/// vehicles is lost, a newer order supersedes it. Any other reason there must be the refusal
/// its facts give, exactly. (Which of these held is the bridge's to say: replay can't
/// recompute the task layer or a heartbeat, only that the reason is one of them.)
fn other_void(reason: &str, spec: &IntentSpec) -> bool {
    reason.starts_with("system: the task layer can't take it: ")
        || reason.starts_with("system: superseded: ")
        || reason.starts_with("superseded by ")
        || spec
            .uids
            .iter()
            .any(|u| reason.starts_with(&format!("system: vehicle {u} lost: ")) || reason == format!("system: vehicle {u} lost"))
}

/// Rules 12: whether a carried-over entry names a `.last-run` pointer that could not be read
/// (empty, torn, garbled): the log it named is unknown, so there is no log to look for.
pub fn is_pointer_name(name: &str) -> bool {
    name.starts_with(".last-run")
}

/// Rules 12: `run.started`'s `carried_from` against the logs it names (each found in
/// [`ReplayOptions::carried_logs`], or [`ReplayOptions::restores_from`] by its name): the first
/// is the log the pointer named, every later one a log an earlier one named pending, each
/// named once, and every pending log has its own entry (read again, or failed again); each
/// log read replays itself (one generation, its carry-overs trusted), is the one read (its
/// head and last seq) and still owed exactly what the entry carries (the platforms' values,
/// the vehicles, the engagements, and the logs it named pending). A log not given is a
/// mismatch for an entry that carries anything, unless trusted; one that carries nothing is
/// reported unchecked. Before rules 12 `carried_from` is a marker (a mismatch elsewhere).
fn check_carried_from(r: &mut ReplayReport, events: &[Event], opts: &ReplayOptions<'_>) {
    let Some(first) = events.first() else { return };
    let EventBody::RunStarted {
        carried_from,
        rules_version,
        pointer,
        ..
    } = &first.body
    else {
        return;
    };
    // rules 15: what a start carries also says what each log had spent, the fleet file a failed
    // carry read, and a fallback from a pointer that could not be read
    let rules15 = rules_version.unwrap_or(1) >= rules::CARRY_FOLLOWUPS_SINCE;
    let mut named: Vec<&str> = Vec::new();
    for (i, c) in carried_from.iter().enumerate() {
        if !is_log_file_name(&c.log) {
            bad(r, first, None, "a carried-over log not named by its bare file name", c.log.clone(), String::new());
        }
        if named.contains(&c.log.as_str()) {
            bad(r, first, None, "a log carried over from twice in one start", c.log.clone(), String::new());
        }
        // rules 15: the logs a start found in the pointer's place and would not carry from (each
        // a failed carry naming why the pointer failed) come first, in place of the pointer
        let found_in_place = rules15
            && c.pointer_failed.is_some()
            && carried_from[..=i].iter().all(|p| p.pointer_failed.is_some() && (i == 0 || p.error.is_some()));
        if i > 0 && !found_in_place && !carried_from[..i].iter().any(|p| p.pending.contains(&c.log)) {
            bad(
                r,
                first,
                None,
                "a log carried over from that neither the pointer nor an earlier carried log named (a start carries from the log .last-run names and the ones it still owed for)",
                c.log.clone(),
                json(&carried_from[..i].iter().map(|p| &p.pending).collect::<Vec<_>>()),
            );
        }
        // (rules 15: a log found in place of the pointer that the start would not carry from
        // is a failed carry that also says why the pointer failed)
        if c.error.is_some()
            && (c.head.is_some()
                || !c.owed.is_empty()
                || !c.pending.is_empty()
                || !c.spent.is_empty()
                || (c.pointer_failed.is_some() && !rules15))
        {
            bad(r, first, None, "a carried-over log that could not be read carries what it owed", c.log.clone(), String::new());
        }
        // what a failed carry may owe (its fleet's armed platforms) is only a failed one's; a
        // pointer that could not be read is never read as a log
        if c.error.is_none() && (!c.may_owe.is_empty() || c.fleet_read.is_some() || is_pointer_name(&c.log)) {
            bad(r, first, None, "a carried-over log that was read carries what a failed carry may owe, or is a pointer", c.log.clone(), String::new());
        }
        // rules 15: a fallback stands for the pointer (the first entry) of a station that names
        // its pointer; the platforms a failed carry may owe come from the fleet file it read
        if c.pointer_failed.is_some() && (!found_in_place || pointer.is_none()) {
            bad(
                r,
                first,
                None,
                "a carry from a log found in place of a pointer that could not be read, other than the first (or the failed carries the start found there with it), or by a run that names no pointer (rules 15)",
                c.log.clone(),
                json(pointer),
            );
        }
        // rules 15: a failed carry's may-owe list comes from the fleet file it read, or (a pending
        // log tried again whose fleet file is gone too) from what the log that named it pending
        // recorded for it, compared when that log is at hand
        // (a log found in place of the pointer and not carried from also holds what it owed or
        // had spent, read from it: checked against it below)
        if rules15 && c.error.is_some() && !c.may_owe.is_empty() && c.fleet_read.is_none() && !found_in_place {
            match carried_from[..i].iter().find(|p| p.pending.contains(&c.log)) {
                None => bad(
                    r,
                    first,
                    None,
                    "a failed carry that may owe values without the fleet file it read (rules 15)",
                    c.log.clone(),
                    json(&c.may_owe),
                ),
                Some(p) => {
                    let recorded = carried_text(p, opts).map(|t| owed_debts(&up_to_seq(t, p.last_seq)).pending_may_owe);
                    if let Some(rec) = recorded
                        && rec.get(&c.log) != Some(&c.may_owe)
                    {
                        bad(
                            r,
                            first,
                            None,
                            "a failed carry's may-owe list, without a fleet file, differs from what the log that named it pending recorded (rules 15)",
                            json(&c.may_owe),
                            json(&rec.get(&c.log)),
                        );
                    }
                }
            }
        }
        for p in &c.pending {
            if !carried_from.iter().any(|x| &x.log == p) {
                bad(
                    r,
                    first,
                    None,
                    "a log still owed for (pending) not tried again at the start",
                    p.clone(),
                    c.log.clone(),
                );
            }
        }
        named.push(&c.log);
    }
    for c in carried_from {
        let text = carried_text(c, opts);
        if let Some(why) = &c.error {
            // rules 15: the fleet file the start read, when it is at hand: that one, and the
            // platforms it may owe values to exactly its armed ones; a start that recorded none
            // claimed it could not be read, so one at hand that reads is a mismatch
            let fleet_checked = match &c.fleet_read {
                Some(f) => Some(check_fleet_read(r, first, events, c, f, opts)),
                None => {
                    let name = fleet_file_name(&c.log);
                    let given = opts.fleets.iter().find(|(n, _)| *n == name);
                    if rules15
                        && let Some((_, bytes)) = given
                        && fleet_may_owe(bytes, &[]).is_some()
                    {
                        bad(
                            r,
                            first,
                            None,
                            "a failed carry that recorded no fleet file read, though its fleet file is given and reads (rules 15)",
                            c.log.clone(),
                            name.clone(),
                        );
                    }
                    given.map(|_| true)
                }
            };
            // recorded failed, yet the log is at hand: what it owed was not carried; a log that
            // owed something is a mismatch (its failure can't be told from a forged one), unless
            // trusted. Rules 15: not a log found in place of the pointer that the start would not
            // carry from (its chain or anchors did not check, or another log could be the one the
            // pointer named): it stays pending, and it must hold every platform it owed values
            // to, had spent or held for a failed carry of its own (`may_owe`), so nothing it owed
            // drops unsaid for this run
            let refused_fallback = rules15 && c.pointer_failed.is_some();
            if refused_fallback && let Some(t) = text {
                let d = owed_debts(t);
                let unheld: Vec<&Uid> = d
                    .owed
                    .iter()
                    .map(|o| &o.uid)
                    .chain(d.spent.iter().map(|sp| &sp.uid))
                    .chain(d.pending_may_owe.values().flatten().map(|m| &m.uid))
                    .filter(|u| !c.may_owe.iter().any(|m| &m.uid == *u))
                    .collect();
                if !unheld.is_empty() {
                    bad(
                        r,
                        first,
                        None,
                        &format!(
                            "{} was found in place of the pointer and not carried from, yet does not hold every platform it owed values to, had spent or held (rules 15)",
                            c.log
                        ),
                        json(&c.may_owe.iter().map(|m| &m.uid).collect::<Vec<_>>()),
                        json(&unheld),
                    );
                }
            }
            let readable = text
                .and_then(|t| parse_log(t).ok().filter(|ev| !ev.is_empty()).map(|_| owed_debts(t)))
                .filter(|d| !d.is_empty() || (rules15 && !d.spent.is_empty()));
            if let Some(d) = readable
                && !opts.trust_resumed
                && !refused_fallback
            {
                bad(
                    r,
                    first,
                    None,
                    &format!(
                        "{} is recorded as a failed carry ({why}), yet it reads and still owed: what it owed was not carried",
                        c.log
                    ),
                    json(&(&d.owed, &d.pending)),
                    String::new(),
                );
            }
            r.carried_restores.push(CarriedRestores {
                log: c.log.clone(),
                restores: 0,
                checked: false,
                head: None,
                failed: Some(why.clone()),
                pointer_failed: c.pointer_failed.clone(),
                fleet_checked,
                spent: Vec::new(),
            });
            continue;
        }
        // as the start read it: up to its last complete event (a crash may have torn a line
        // after it)
        let text = text.map(|t| up_to_seq(t, c.last_seq));
        let text = text.as_deref();
        let Some(text) = text else {
            r.carried_restores.push(CarriedRestores {
                log: c.log.clone(),
                restores: c.owed.len(),
                checked: false,
                head: c.head.clone(),
                failed: None,
                pointer_failed: c.pointer_failed.clone(),
                fleet_checked: None,
                spent: c.spent.iter().map(|sp| sp.uid.to_string()).collect(),
            });
            if !opts.trust_resumed && (!c.owed.is_empty() || !c.pending.is_empty() || !c.spent.is_empty()) {
                bad(
                    r,
                    first,
                    None,
                    &format!(
                        "what {} still owed, carried over, not verified: replay with that log (murmur-replay --prev), or trust it (--trust-resumed)",
                        c.log
                    ),
                    json(&(&c.owed, &c.pending, &c.spent)),
                    String::new(),
                );
            }
            continue;
        };
        let prev = replay_str_with(
            text,
            &ReplayOptions {
                rules: RulesAccepted::AtLeast(1),
                trust_resumed: true,
                ..ReplayOptions::default()
            },
        );
        match &prev {
            Err(err) => bad(r, first, None, "a log carried over from can't be read", format!("{}: {err}", c.log), String::new()),
            Ok(p) => {
                if let Some(m) = p.mismatches.first() {
                    bad(
                        r,
                        first,
                        None,
                        "a log carried over from does not replay",
                        format!("{}: {} mismatch(es), first at seq {}: {}", c.log, p.mismatches.len(), m.seq, m.what),
                        String::new(),
                    );
                }
            }
        }
        let read = log_head(text);
        if read.as_ref().map(|(h, _)| h) != c.head.as_ref() || read.as_ref().map(|(_, s)| *s) != c.last_seq {
            bad(
                r,
                first,
                None,
                "the log given is not the one carried over from (its head or last seq differs)",
                json(&(&c.head, c.last_seq)),
                json(&read),
            );
        }
        let debts = owed_debts(text);
        if debts.owed != c.owed {
            bad(
                r,
                first,
                None,
                "carried values differ from what the log they name still owed (each platform's values, the vehicle, the engagements)",
                json(&c.owed),
                json(&debts.owed),
            );
        }
        if debts.pending != c.pending {
            bad(
                r,
                first,
                None,
                "the earlier logs carried as still owed for differ from the ones the log names pending",
                json(&c.pending),
                json(&debts.pending),
            );
        }
        // rules 15: the platforms it had spent and nobody un-spent, every one of them
        if rules15 && debts.spent != c.spent {
            bad(
                r,
                first,
                None,
                "the platforms carried as spent differ from the ones the log had spent (rules 15)",
                json(&c.spent),
                json(&debts.spent),
            );
        }
        // rules 15: a log carried from in place of a pointer is the station's own
        if c.pointer_failed.is_some() && station_pointer(text) != *pointer {
            bad(
                r,
                first,
                None,
                "a log carried from in place of the pointer that is not this station's (its run.started names another pointer)",
                json(&station_pointer(text)),
                json(pointer),
            );
        }
        r.carried_restores.push(CarriedRestores {
            log: c.log.clone(),
            restores: c.owed.len(),
            checked: true,
            head: prev.ok().and_then(|p| p.head_sha256),
            failed: None,
            pointer_failed: c.pointer_failed.clone(),
            fleet_checked: None,
            spent: c.spent.iter().map(|sp| sp.uid.to_string()).collect(),
        });
    }
}

/// Rules 12: the text of a log `run.started` says the start carried from, when it is at hand.
fn carried_text<'a>(c: &CarriedFrom, opts: &ReplayOptions<'a>) -> Option<&'a str> {
    opts.carried_logs
        .iter()
        .find(|(n, _)| *n == c.log)
        .or_else(|| opts.restores_from.as_ref().filter(|(n, _)| *n == c.log))
        .map(|(_, t)| *t)
}

/// Rules 15: why a log's hash chain does not hold, if it does not: a line that does not parse as
/// an event (but a torn last one, a crash mid-write), a seq that is not contiguous from 1, or a
/// `prev` that is not the SHA-256 of the line before. What a start that falls back from a pointer
/// it can't read checks before carrying from a log (with its anchors).
pub fn chain_problem(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut prev = GENESIS.to_string();
    for (i, l) in lines.iter().enumerate() {
        let e = match serde_json::from_str::<Event>(l) {
            Ok(e) => e,
            // a torn last line is not part of the log (a crash cut it short)
            Err(_) if i + 1 == lines.len() && i > 0 => return None,
            Err(err) => return Some(format!("line {} does not parse: {err}", i + 1)),
        };
        if e.seq != i as u64 + 1 {
            return Some(format!("seq {} where {} was due", e.seq, i + 1));
        }
        if e.prev != prev {
            return Some(format!("seq {}: its prev is not the hash of the line before", e.seq));
        }
        prev = sha256_hex(l.as_bytes());
    }
    if lines.is_empty() {
        return Some("no event".into());
    }
    None
}

/// Rules 15: the `.last-run` pointer a log's `run.started` names (its station), if any.
pub fn station_pointer(text: &str) -> Option<String> {
    let first = text.lines().find(|l| !l.trim().is_empty())?;
    match serde_json::from_str::<Event>(first) {
        Ok(Event {
            body: EventBody::RunStarted { pointer, .. },
            ..
        }) => pointer,
        _ => None,
    }
}

/// Rules 15: a failed carry's fleet file against what the start recorded (`fleet_read`): when
/// the file is at hand ([`ReplayOptions::fleets`]), its SHA-256 must be the one read and the
/// platforms the carry may owe values to exactly its vehicles of the armed platforms recorded;
/// and every platform this log's decisions read must agree with that list (an armed platform
/// left out, or one listed that the catalog does not arm, is a mismatch). Returns whether the
/// file was at hand.
fn check_fleet_read(
    r: &mut ReplayReport,
    first: &Event,
    events: &[Event],
    c: &CarriedFrom,
    f: &FleetRead,
    opts: &ReplayOptions<'_>,
) -> bool {
    let mut seen: BTreeMap<String, bool> = BTreeMap::new();
    for e in events {
        if let EventBody::CheckResult { inputs, .. } = &e.body {
            for v in inputs.vehicles.values() {
                if let Some(p) = &v.platform {
                    seen.insert(p.id.clone(), p.armed);
                }
            }
        }
    }
    if let Some((id, armed)) = seen.iter().find(|(id, armed)| **armed != f.armed.contains(*id)) {
        bad(
            r,
            first,
            None,
            "the armed platforms a failed carry judged its fleet by differ from the catalog's as this log's decisions read it",
            json(&f.armed),
            format!("{id}: armed {armed}"),
        );
    }
    let name = fleet_file_name(&c.log);
    let Some((_, bytes)) = opts.fleets.iter().find(|(n, _)| *n == name) else {
        return false;
    };
    let hash = sha256_hex(bytes);
    if hash != f.sha256 {
        bad(r, first, None, "the fleet file given is not the one the failed carry read (its SHA-256 differs)", f.sha256.clone(), hash);
    }
    let want = fleet_may_owe(bytes, &f.armed);
    // rules 15: a log found in place of the pointer and not carried from holds its fleet's armed
    // platforms and also what the log itself owed or had spent (checked against the log)
    let fits = match (&want, c.pointer_failed.is_some()) {
        (Some(w), true) => w.iter().all(|m| c.may_owe.iter().any(|x| x.uid == m.uid)),
        (w, false) => w.as_ref() == Some(&c.may_owe),
        (None, true) => false,
    };
    if !fits {
        bad(
            r,
            first,
            None,
            "the platforms a failed carry may owe values to differ from its fleet file's armed ones",
            json(&c.may_owe),
            json(&want),
        );
    }
    true
}

/// Rules 12: a log's text up to and including its event of seq `last` (the head a start
/// recorded; a crash may have torn a line after it); the whole text when there is no such
/// event, and nothing when `last` is none (a log with no event).
pub fn up_to_seq(text: &str, last: Option<u64>) -> String {
    let Some(last) = last else { return String::new() };
    let mut out = String::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        out.push_str(line);
        out.push('\n');
        if serde_json::from_str::<Event>(line).is_ok_and(|e| e.seq == last) {
            return out;
        }
    }
    text.to_string()
}

/// A resumed intent as its `intent.resumed` event carries it (seq, from, the open intent).
type ResumedEvent = (u64, crate::queue::ResumedFrom, OpenIntent, State);

/// Every `intent.resumed` against the previous log (one generation): that log is the one
/// named (its head and last seq), it replays, and it leaves the intent open exactly as
/// carried (spec, state, basis, policy, approval, op states, deadline), in the state it
/// resumes in here. Without the previous log, a mismatch unless the caller trusts it.
fn check_resumed(r: &mut ReplayReport, resumed: &[ResumedEvent], opts: &ReplayOptions<'_>) {
    let Some(text) = opts.previous else {
        if !opts.trust_resumed {
            for (seq, from, open, _) in resumed {
                r.mismatches.push(Mismatch {
                    seq: *seq,
                    intent: Some(open.spec.id),
                    what: format!(
                        "intent carried over from {} not verified: replay with that log (murmur-replay --prev), or trust it (--trust-resumed)",
                        from.log
                    ),
                    logged: String::new(),
                    recomputed: String::new(),
                });
            }
        }
        return;
    };
    let prev = replay_str_with(
        text,
        &ReplayOptions {
            rules: RulesAccepted::AtLeast(5),
            trust_resumed: true,
            ..ReplayOptions::default()
        },
    );
    let push = |r: &mut ReplayReport, seq: u64, id: IntentId, what: &str, logged: String, want: String| {
        r.mismatches.push(Mismatch {
            seq,
            intent: Some(id),
            what: what.to_string(),
            logged,
            recomputed: want,
        });
    };
    let prev = match prev {
        Ok(p) => p,
        Err(e) => {
            if let Some((seq, _, open, _)) = resumed.first() {
                push(r, *seq, open.spec.id, "the previous log can't be read", e.to_string(), String::new());
            }
            return;
        }
    };
    for (seq, from, open, state) in resumed {
        let id = open.spec.id;
        if !prev.ok() {
            push(r, *seq, id, "the previous log does not replay", format!("{} mismatch(es)", prev.mismatches.len()), String::new());
        }
        if Some(&from.head) != prev.head_sha256.as_ref() || from.seq != prev.last_seq {
            push(
                r,
                *seq,
                id,
                "carried over from another log than the one given (head or last seq differ)",
                json(&(&from.head, from.seq)),
                json(&(&prev.head_sha256, prev.last_seq)),
            );
            continue;
        }
        let Some(was) = prev.open.iter().find(|o| o.spec.id == id) else {
            push(r, *seq, id, "carried over, but the previous log does not leave it open", String::new(), String::new());
            continue;
        };
        if from.was != was.state {
            push(r, *seq, id, "carried over from another state than the previous log ends it in", json(&from.was), json(&was.state));
        }
        // rules 8: the previous run's authentication, as it logged it
        if r.rules_version >= OPERATOR_AUTH_SINCE && from.auth != prev.auth {
            push(r, *seq, id, "carried over claiming another authentication than the previous log's", json(&from.auth), json(&prev.auth));
        }
        if *state != was.resumes_as() {
            push(r, *seq, id, "resumed in another state than its op states allow", json(state), json(&was.resumes_as()));
        }
        let carried = (
            &open.spec,
            &open.basis,
            &open.policy,
            &open.approval,
            &open.ops,
            (&open.committed, open.dispatched_before, &open.platforms),
            &open.maybe_started,
        );
        let there = (
            &was.spec,
            &was.basis,
            &was.policy,
            &was.approval,
            &was.ops,
            (&was.committed, was.dispatched_before, &was.platforms),
            &was.maybe_started,
        );
        if carried != there {
            push(r, *seq, id, "carried over differently from how the previous log leaves it", json(&carried), json(&there));
        }
        if was.state == State::Queued && open.expires_t != was.expires_t {
            push(r, *seq, id, "queue deadline differs from the previous log's", json(&open.expires_t), json(&was.expires_t));
        }
    }
}

/// A carried `platform.restore`: the event, its platform, values, `from` and vehicle.
type Carried<'e> = (
    &'e Event,
    &'e Uid,
    &'e BTreeMap<String, f64>,
    Option<&'e str>,
    Option<&'e engage::VehicleIdentity>,
);

/// Rules 6: every carried `platform.restore` against the earlier log it names
/// ([`ReplayOptions::restores_from`]; one generation, like `intent.resumed`). The bridge
/// derived them from it at start ([`owed_restores`]), so that log must replay (its own
/// carry-overs trusted) and they must be exactly what it still owed: each platform's
/// values, the vehicle they were preloaded on and the engagements named, none carried twice,
/// every platform it owed carried. A carried restore names that log by its file name (no
/// head hash, unlike `intent.resumed`: the head replay reports is to be compared with that
/// log's anchors), and one start carries from the one log its `.last-run-<port>` pointer
/// names. Without that log they are a mismatch unless trusted
/// ([`ReplayOptions::trust_resumed`]); one that names no log by its file name is a mismatch
/// either way.
fn check_carried_restores(r: &mut ReplayReport, events: &[Event], opts: &ReplayOptions<'_>) {
    let carried: Vec<Carried<'_>> = events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::PlatformRestore {
                uid,
                values,
                carried: true,
                from,
                vehicle,
                ..
            } => Some((e, uid, values, from.as_deref(), vehicle.as_deref())),
            _ => None,
        })
        .collect();
    let mut names: Vec<&str> = Vec::new();
    for c in &carried {
        let name = carried_restore_log(c.3);
        if !is_log_file_name(name) {
            bad(
                r,
                c.0,
                None,
                "carried restore names no earlier log by its file name (the bridge names the log it carried it from)",
                json(&c.3),
                "<file> (preloaded by ...) or <file> (carried over into it)".into(),
            );
            continue;
        }
        if names.contains(&name) {
            continue;
        }
        if let Some(first) = names.first() {
            bad(
                r,
                c.0,
                None,
                "carried restores name more than one earlier log (a start carries from the one its .last-run pointer names)",
                name.to_string(),
                (*first).to_string(),
            );
        }
        names.push(name);
    }
    for name in names {
        let these: Vec<&Carried<'_>> = carried.iter().filter(|c| carried_restore_log(c.3) == name).collect();
        let Some((_, text)) = opts.restores_from.filter(|(n, _)| *n == name) else {
            r.carried_restores.push(CarriedRestores {
                log: name.to_string(),
                restores: these.len(),
                checked: false,
                head: None,
                failed: None,
                pointer_failed: None,
                fleet_checked: None,
                spent: Vec::new(),
            });
            if !opts.trust_resumed {
                for c in &these {
                    bad(
                        r,
                        c.0,
                        None,
                        &format!(
                            "lost-link values carried over from {name} not verified: replay with that log (murmur-replay --prev), or trust it (--trust-resumed)"
                        ),
                        c.1.to_string(),
                        String::new(),
                    );
                }
            }
            continue;
        };
        // the log it names replays, its own carry-overs trusted (one generation)
        let first = these[0].0;
        let prev = replay_str_with(
            text,
            &ReplayOptions {
                rules: RulesAccepted::AtLeast(1),
                trust_resumed: true,
                ..ReplayOptions::default()
            },
        );
        let head = match prev {
            Err(err) => {
                bad(r, first, None, "the log the carried values name can't be read", format!("{name}: {err}"), String::new());
                None
            }
            Ok(p) => {
                if let Some(m) = p.mismatches.first() {
                    bad(
                        r,
                        first,
                        None,
                        "the log the carried values name does not replay",
                        format!("{name}: {} mismatch(es), first at seq {}: {}", p.mismatches.len(), m.seq, m.what),
                        String::new(),
                    );
                }
                p.head_sha256
            }
        };
        r.carried_restores.push(CarriedRestores {
            log: name.to_string(),
            restores: these.len(),
            checked: true,
            head,
            failed: None,
            pointer_failed: None,
            fleet_checked: None,
            spent: Vec::new(),
        });
        let owed = owed_restores(text);
        let mut seen: BTreeSet<&Uid> = BTreeSet::new();
        for &&(e, uid, values, from, vehicle) in &these {
            if !seen.insert(uid) {
                bad(r, e, None, "carried over twice from the log it names", uid.to_string(), "once".into());
                continue;
            }
            let Some(o) = owed.iter().find(|o| &o.uid == uid) else {
                bad(
                    r,
                    e,
                    None,
                    "carried restore of values the log it names did not owe",
                    format!("{uid}: {}", json(values)),
                    "nothing owed".into(),
                );
                continue;
            };
            if values != &o.values {
                bad(
                    r,
                    e,
                    None,
                    "carried values differ from what the log it names still owed",
                    json(values),
                    json(&o.values),
                );
            }
            if vehicle != o.vehicle.as_ref() {
                bad(
                    r,
                    e,
                    None,
                    "carried restore names another vehicle than the one the log it names preloaded",
                    json(&vehicle),
                    json(&o.vehicle),
                );
            }
            let want = carried_from(name, &o.by);
            if from != Some(want.as_str()) {
                bad(
                    r,
                    e,
                    None,
                    "carried restore names other engagements than the ones the log it names owed them for",
                    json(&from),
                    want,
                );
            }
        }
        for o in owed.iter().filter(|o| !seen.contains(&o.uid)) {
            bad(
                r,
                first,
                None,
                "the log it names still owed a platform's values this run did not carry over",
                format!("{}: not carried", o.uid),
                json(&o.values),
            );
        }
    }
}

/// Rules 6: a signature's `self_accepted`: the signer accepted one of the engagement's lost
/// vehicles in force.
fn check_self_accepted(r: &mut ReplayReport, e: &Event, id: IntentId, tr: &Tracked, by: &OperatorId, logged: bool) {
    let want = tr.accepted.values().chain(tr.accepted_lost.values()).any(|a| &a.by == by)
        || tr.accepted_behaving.values().any(|a| &a.by == by);
    if logged != want {
        bad(r, e, Some(id), "self_accepted flag differs", logged.to_string(), want.to_string());
    }
}

/// The operator session an event acts under, if it is an operator's act.
fn acting_session(body: &EventBody) -> Option<SessionId> {
    match body {
        EventBody::OperatorAction { session, .. }
        | EventBody::ApprovalGranted { session, .. }
        | EventBody::ApprovalConcurred { session, .. }
        | EventBody::ApprovalAtSubmission { session, .. }
        | EventBody::ApprovalRejected { session, .. }
        | EventBody::EngageAccepted { session, .. }
        | EventBody::EngageAcceptanceRevoked { session, .. }
        | EventBody::PlatformUnspent { session, .. }
        | EventBody::OperatorView { session, .. } => Some(*session),
        EventBody::IntentCreated { session: Some(s), .. } => Some(*s),
        // rules 10: a passkey registered through a session
        EventBody::PasskeyRegistered { session: Some(s), .. } => Some(*s),
        EventBody::PictureIssued { token, .. } => Some(token.session),
        EventBody::EngageStopped {
            cause: StopCause::Operator { session, .. } | StopCause::Revoked { session, .. },
            ..
        } => Some(*session),
        // rules 9: identification acts by operators (a rule declaration has no session)
        EventBody::IdentConfirmed { session, .. }
        | EventBody::IdentRevoked { session, .. }
        | EventBody::IdentFlagResolved { session, .. } => Some(*session),
        // rules 16: a lost contact confirmed as a live track
        EventBody::TrackReacquired { session, .. } => Some(*session),
        EventBody::IdentDeclared { declaration } => match &declaration.by {
            crate::ident::Declarer::Operator { session, .. } => Some(*session),
            crate::ident::Declarer::Rule { .. } => None,
        },
        _ => None,
    }
}

/// Rules 8: a step-up as logged: the session and operator it was entered in, when, and what
/// used it so far; rules 10: its factor (password, passkey, scripted), whether its session was
/// a person's, and a passkey's purpose.
struct StepUpSeen {
    session: SessionId,
    operator: OperatorId,
    t: Time,
    used: StepUse,
    factor: Option<AuthMethod>,
    person: bool,
    purpose: Option<crate::passkey::StepUpFor>,
}

/// What quotes a step-up: one signature, one hostile declaration, one nomination's
/// acceptances of lost vehicles (logged one after the other: they share it), or (rules 10) one
/// passkey registration.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
enum StepUse {
    Unused,
    Signature,
    Declaration,
    Acceptances { intent: IntentId, last_seq: u64 },
    Registration,
    /// Rules 15: an un-spend of a platform spent by its own effect.
    Unspend,
}

impl StepUse {
    fn what(self) -> &'static str {
        match self {
            StepUse::Unused | StepUse::Signature => "a signature",
            StepUse::Declaration => "a hostile declaration",
            StepUse::Acceptances { .. } => "an acceptance of a lost vehicle",
            StepUse::Registration => "a passkey registration",
            StepUse::Unspend => "an un-spend",
        }
    }
}

/// Rules 10: the act quoting a step-up, for a passkey's purpose to be compared with.
enum Expected {
    /// Exactly this purpose (a signature on its intent and picture token, a hostile
    /// declaration of its track, a confirmation of its declaration).
    Is(crate::passkey::StepUpFor),
    /// An acceptance of `vehicle` for `intent`: a passkey made for the acceptances of a
    /// nomination (no intent yet) or for this intent, naming the vehicle.
    Acceptance {
        intent: IntentId,
        vehicle: crate::passkey::AcceptedVehicle,
    },
    /// Rules 16: an acceptance of lost contact `contact` for `intent`, as for a vehicle.
    ContactAcceptance {
        intent: IntentId,
        contact: crate::passkey::AcceptedContact,
    },
    /// Rules 19: an acceptance of live track `track` as behaving now for `intent`, as for a
    /// vehicle.
    TrackAcceptance {
        intent: IntentId,
        track: crate::passkey::AcceptedTrack,
    },
    /// A passkey registration: it re-enters the password, whatever the step-up factor.
    Registration,
    /// An act no passkey assertion is made for (rules 8's `track.declare`, a declaration that
    /// is not hostile).
    Unbound,
}

/// What a signature on `intent` quoting `token` (an engagement's) is for.
fn sign_expected(intent: IntentId, token: Option<&PictureToken>) -> Expected {
    Expected::Is(crate::passkey::StepUpFor::Sign {
        intent,
        token: token.map(|t| t.id),
        picture_sha256: token.map(crate::passkey::picture_sha256),
    })
}

impl Expected {
    /// Why a valid step-up (`seen`) can't stand for this act, if it can't: a passkey's made
    /// for another act, a password where the policy asks a person for a passkey (a
    /// registration excepted), a passkey quoted by a registration.
    fn refuses(&self, seen: &StepUpSeen, policy: Option<&Policy>, auth: Option<AuthMode>) -> Option<String> {
        use crate::passkey::StepUpFor;
        if let Some(made) = &seen.purpose {
            let fits = match self {
                Expected::Is(want) => want == made,
                Expected::Acceptance { intent, vehicle } => matches!(
                    made,
                    StepUpFor::AcceptUnknown { intent: i, vehicles, .. }
                        if (i.is_none() || *i == Some(*intent)) && vehicles.contains(vehicle)
                ),
                Expected::ContactAcceptance { intent, contact } => matches!(
                    made,
                    StepUpFor::AcceptUnknown { intent: i, contacts, .. }
                        if (i.is_none() || *i == Some(*intent)) && contacts.contains(contact)
                ),
                Expected::TrackAcceptance { intent, track } => matches!(
                    made,
                    StepUpFor::AcceptUnknown { intent: i, tracks, .. }
                        if (i.is_none() || *i == Some(*intent)) && tracks.contains(track)
                ),
                Expected::Registration | Expected::Unbound => false,
            };
            if !fits {
                return Some(format!("a passkey assertion made for {} quoted by another act", made.describe()));
            }
        }
        match self {
            Expected::Registration if seen.factor == Some(AuthMethod::Passkey) => {
                Some("a passkey registration quoting a passkey step-up (it re-enters the password)".into())
            }
            Expected::Registration => None,
            _ if auth == Some(AuthMode::Credentials)
                && seen.person
                && policy.is_some_and(|p| p.step_up_factor() == policy::StepUpFactor::Passkey)
                && seen.factor != Some(AuthMethod::Passkey) =>
            {
                Some("a person's step-up by password where the policy requires a passkey (step_up_factor)".into())
            }
            _ => None,
        }
    }
}

/// Rules 10: the passkeys a log knows, as replay follows them.
#[derive(Default)]
struct PasskeysSeen {
    /// The hash of the log's first line: every challenge binds to it.
    run_hash: String,
    /// The relying party id and the origins (`run.started`).
    rp: Option<(String, Vec<String>)>,
    /// By credential id: the key (its counter the last one logged) and whether it was removed.
    keys: BTreeMap<String, (crate::passkey::PasskeyKey, bool)>,
    /// Every challenge's nonce answered so far.
    nonces: BTreeSet<String>,
}

impl PasskeysSeen {
    /// Check one logged passkey use by `operator` (in `session`; none for a login) at `t`
    /// (see [`crate::passkey`]), then record its counter and nonce. Err says why it is not a
    /// use the run could have accepted.
    fn use_once(
        &mut self,
        operator: &OperatorId,
        session: Option<SessionId>,
        t: Time,
        u: &crate::passkey::PasskeyUse,
    ) -> Result<(), String> {
        let (rp_id, origins) = self.rp.clone().ok_or("the run logged no relying party")?;
        if !self.nonces.insert(u.nonce.clone()) {
            return Err("a challenge answered twice (an assertion used again)".into());
        }
        let age = t.ms_since(u.issued_t);
        if !(0..=crate::passkey::CHALLENGE_TTL_MS).contains(&age) {
            return Err(format!("its challenge was {age} ms old (at most {})", crate::passkey::CHALLENGE_TTL_MS));
        }
        let Some((key, removed)) = self.keys.get_mut(&u.credential) else {
            return Err("a credential the run does not know".into());
        };
        if *removed {
            return Err("a passkey used after its removal".into());
        }
        if &key.operator != operator {
            return Err(format!("{}'s passkey claimed by {operator}", key.operator));
        }
        let challenge = crate::passkey::challenge(&self.run_hash, session, operator, &u.purpose, &u.nonce, u.issued_t);
        let v = crate::passkey::verify(key, &rp_id, &origins, &challenge, &u.assertion)?;
        if v.counter != u.counter {
            return Err(format!("the logged counter {} is not the authenticator's {}", u.counter, v.counter));
        }
        if !crate::passkey::counter_ok(key.counter, v.counter) {
            return Err(format!("the counter went from {} to {} (a cloned authenticator)", key.counter, v.counter));
        }
        key.counter = v.counter;
        Ok(())
    }
}

/// Rules 8: an act's step-up (`act`: a signature, a declaration, an acceptance). One it
/// quotes is a step-up logged under the actor's own session, at most
/// [`STEP_UP_MAX_AGE_MS`] before it, used by nothing else, except that one nomination's
/// acceptances share theirs (the same intent, logged one after the other); one the act
/// needs (`needed`: its decision asks for it, or `[approval] step_up_acts` lists the act) in
/// a credentials run must be there. Rules 10: a passkey's was made for this act (`expected`),
/// and the policy's step-up factor holds; a study counts its participant's acts by factor.
#[allow(clippy::too_many_arguments)]
fn check_step_up(
    r: &mut ReplayReport,
    e: &Event,
    id: Option<IntentId>,
    needed: bool,
    by: &OperatorId,
    session: SessionId,
    quoted: Option<StepUpRef>,
    step_ups: &mut BTreeMap<u64, StepUpSeen>,
    act: StepUse,
    expected: &Expected,
    policy: Option<&Policy>,
) {
    let what = act.what();
    let Some(s) = quoted else {
        if needed && r.auth == Some(AuthMode::Credentials) {
            let why = match act {
                StepUse::Unused | StepUse::Signature => {
                    "a consequential signature without a step-up (its decision asks for the signer's credential at signing)".to_string()
                }
                StepUse::Registration => {
                    "a passkey registration without a step-up (through a session, the operator re-enters their password first)".to_string()
                }
                StepUse::Unspend => {
                    "an un-spend without a step-up (rules 15: it makes an armed platform taskable again, so a credentials run asks for the operator's credential)".to_string()
                }
                _ => format!("{what} without a step-up (the policy's step_up_acts asks for the actor's credential)"),
            };
            bad(r, e, id, &why, by.to_string(), "step_up".into());
        }
        return;
    };
    let shares = |used: StepUse| match (used, act) {
        (StepUse::Unused, _) => true,
        (StepUse::Acceptances { intent, last_seq }, StepUse::Acceptances { intent: now, .. }) => {
            intent == now && e.seq == last_seq + 1
        }
        _ => false,
    };
    match step_ups.get_mut(&s.seq) {
        Some(seen)
            if seen.session == session
                && &seen.operator == by
                && shares(seen.used)
                && e.t.ms_since(seen.t) <= STEP_UP_MAX_AGE_MS =>
        {
            seen.used = match act {
                StepUse::Acceptances { intent, .. } => StepUse::Acceptances { intent, last_seq: e.seq },
                other => other,
            };
            match act {
                StepUse::Unused | StepUse::Signature => r.stepped_signatures += 1,
                _ => r.stepped_acts += 1,
            }
            // rules 10: what a passkey was made for, and the policy's factor
            if let Some(why) = expected.refuses(seen, policy, r.auth) {
                bad(r, e, id, &why, json(&(s.seq, &seen.purpose, seen.factor)), what.into());
            }
            // a study: its participant's consequential acts, by factor
            let factor = seen.factor;
            if !matches!(act, StepUse::Registration)
                && let Some(st) = r.study.as_mut().filter(|st| &st.participant == by)
            {
                if factor == Some(AuthMethod::Passkey) {
                    st.passkey_acts += 1;
                } else {
                    st.password_acts += 1;
                }
            }
        }
        other => {
            let why = match act {
                StepUse::Unused | StepUse::Signature => {
                    "a signature quoting a step-up that is not the signer's own, fresh and unused".to_string()
                }
                _ => format!("{what} quoting a step-up that is not the actor's own, fresh and unused"),
            };
            bad(
                r,
                e,
                id,
                &why,
                json(&(s.seq, by, session.0)),
                json(&other.map(|seen| (seen.session.0, seen.operator.clone(), seen.t.0, seen.used))),
            );
        }
    }
}

/// Rules 9: what an act's verdict makes next (its rule declaration, or its flag).
enum IdentDue {
    Declare {
        act: crate::ident::ActReport,
        track: String,
        attribution: f64,
        corroborated_by: Option<crate::ident::Corroborating>,
    },
    Flag {
        act: crate::ident::ActReport,
        kind: crate::ident::FlagKind,
        tracks: Vec<String>,
        why: String,
    },
}

impl IdentDue {
    fn what(&self) -> String {
        match self {
            IdentDue::Declare { track, .. } => format!("ident.declared of {track} by rule"),
            IdentDue::Flag { kind, .. } => format!("ident.flagged ({})", kind.as_str()),
        }
    }
}

/// Rules 9: a revocation's engagements, stopped next in order (cause `revoked`), each stop
/// followed by its transition, before anything else is logged.
struct RevokeDue {
    track: String,
    declaration: u64,
    by: OperatorId,
    session: SessionId,
    remaining: Vec<IntentId>,
    /// Stopped, waiting for its transition.
    stopping: Option<IntentId>,
}

impl RevokeDue {
    /// The next event: `Ok(true)` when it is the next stop, `Ok(false)` when it is the stop's
    /// transition (or nothing is due), `Err` when it is anything else.
    fn step(&mut self, body: &EventBody) -> Result<bool, String> {
        if let Some(id) = self.stopping {
            return match body {
                EventBody::IntentState { intent, .. } if *intent == id => {
                    self.stopping = None;
                    Ok(false)
                }
                _ => Err(format!(
                    "a revoked declaration's engagement {id} was stopped but its transition did not follow at once"
                )),
            };
        }
        let Some(&next) = self.remaining.first() else {
            return Ok(false);
        };
        match body {
            EventBody::EngageStopped {
                intent,
                cause:
                    StopCause::Revoked {
                        by,
                        session,
                        track,
                        declaration,
                    },
                ..
            } if *intent == next
                && *by == self.by
                && *session == self.session
                && *track == self.track
                && *declaration == self.declaration =>
            {
                self.remaining.remove(0);
                self.stopping = Some(next);
                Ok(true)
            }
            _ => Err(format!(
                "a revoked declaration's engagements are not stopped first (next due: {next}, cause revoked, right after the revocation of {} D-{})",
                self.track, self.declaration
            )),
        }
    }
}

/// An event's `type`.
fn event_type(body: &EventBody) -> String {
    serde_json::to_value(body)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_string))
        .unwrap_or_default()
}

/// A human act (`what`) under a session opened for that operator (an act that is no
/// intent's).
fn check_operator(
    r: &mut ReplayReport,
    e: &Event,
    by: &OperatorId,
    session: SessionId,
    sessions: &BTreeMap<SessionId, (OperatorId, Grants)>,
    what: &str,
) {
    match sessions.get(&session) {
        Some((op, _)) if op == by => {}
        other => bad(
            r,
            e,
            None,
            &format!("{what} under a session that is not the operator's"),
            by.to_string(),
            json(&other.map(|o| &o.0)),
        ),
    }
}

/// Rules 9: a decision's logged track picture against the identification ledger at that
/// point (every declaration with its basis, every flag in force, the affiliations they set):
/// a declaration a decision read that nobody made, or one claimed for an operator who did not
/// make it, or a rule declaration shown confirmed when it was not, is caught here.
fn check_ident_inputs(
    r: &mut ReplayReport,
    e: &Event,
    id: Option<IntentId>,
    tracks: &BTreeMap<String, crate::world::TrackFacts>,
    ident: &crate::ident::Ledger,
    policy: Option<&Policy>,
) {
    if !policy.is_some_and(|p| p.rules.rule_id()) {
        return;
    }
    if let Err(why) = ident.check_facts(tracks) {
        bad(
            r,
            e,
            id,
            "a decision's track picture differs from the declarations and flags the log holds",
            why,
            String::new(),
        );
    }
}

/// A human act (`what`) under a session opened earlier for that operator.
fn check_session(
    r: &mut ReplayReport,
    e: &Event,
    id: IntentId,
    by: &OperatorId,
    session: SessionId,
    sessions: &BTreeMap<SessionId, (OperatorId, Grants)>,
    what: &str,
) {
    match sessions.get(&session) {
        Some((op, _)) if op == by => {}
        other => bad(
            r,
            e,
            Some(id),
            &format!("{what} under a session that is not the operator's"),
            by.to_string(),
            json(&other.map(|o| &o.0)),
        ),
    }
}

/// One approval signature: by the operator the session was opened for; with engagement
/// authority on an engagement; never the proposer where excluded; never twice.
fn check_signature(
    r: &mut ReplayReport,
    e: &Event,
    id: IntentId,
    tr: &Tracked,
    by: &OperatorId,
    session: SessionId,
    sessions: &BTreeMap<SessionId, (OperatorId, Grants)>,
) {
    match sessions.get(&session) {
        Some((op, grants)) if op == by => {
            if tr.spec.kind() == TaskKind::Engage && !grants.engage {
                bad(
                    r,
                    e,
                    Some(id),
                    "engagement approved without engagement authority",
                    by.to_string(),
                    "a session opened with grants.engage".into(),
                );
            }
        }
        other => bad(
            r,
            e,
            Some(id),
            "approval under a session that is not the approver's",
            by.to_string(),
            json(&other.map(|o| &o.0)),
        ),
    }
    if tr.proposer_excluded && is_self_approval(&tr.spec.source, by) {
        bad(
            r,
            e,
            Some(id),
            "approved by its proposer (two-person rule)",
            by.to_string(),
            "someone other than the proposer".into(),
        );
    }
    if tr.signers.contains(by) {
        bad(
            r,
            e,
            Some(id),
            "the same operator signed twice",
            by.to_string(),
            json(&tr.signers),
        );
    }
}

/// Verify a logged area set against its hash; the result is the new set in force.
fn load_areas(
    r: &mut ReplayReport,
    e: &Event,
    sha256: &str,
    set: &AreaSet,
) -> (String, Option<AreaSet>) {
    r.area_sets += 1;
    match set.content_hash() {
        Ok(h) if h == sha256 => (sha256.to_string(), Some(set.clone())),
        other => {
            bad(
                r,
                e,
                None,
                "area set hash does not match its content",
                sha256.to_string(),
                other.unwrap_or_else(|err| format!("<unserializable: {err}>")),
            );
            (sha256.to_string(), None)
        }
    }
}

/// Re-run one logged space query on `oracle` (same segment) or record a cache mismatch.
fn recompute_space(
    r: &mut ReplayReport,
    e: &Event,
    logged: &SpaceAnswer,
    oracle: &dyn SpaceOracle,
    pinned: Option<i64>,
) {
    let loaded = oracle.segment();
    if logged.segment.sha256 != loaded.sha256 {
        r.cache_mismatches.push(Mismatch {
            seq: e.seq,
            intent: None,
            what: "cache mismatch: the space segment given to replay is not the one this query ran on; query not recomputed (decisions were re-decided from the logged answer)".into(),
            logged: logged.segment.short(),
            recomputed: loaded.short(),
        });
        return;
    }
    if oracle.pinned_shift_days() != pinned {
        r.cache_mismatches.push(Mismatch {
            seq: e.seq,
            intent: None,
            what: "cache mismatch: the run pinned a different time shift on the segment (space.loaded pinned_shift_days); query not recomputed".into(),
            logged: format!("{pinned:?}"),
            recomputed: format!("{:?}", oracle.pinned_shift_days()),
        });
        return;
    }
    r.space_recomputed += 1;
    match oracle.answer(&logged.query) {
        Ok(again) if again.same_answer(logged) => {}
        Ok(again) => bad(
            r,
            e,
            None,
            "space answer differs on the same segment",
            json(logged),
            json(&again),
        ),
        Err(err) => bad(
            r,
            e,
            None,
            "space query failed on the same segment",
            json(&logged.query),
            err,
        ),
    }
}

/// What an `intent.dispatched` says was handed on.
struct Dispatched<'a> {
    task: &'a Task,
    uids: &'a [Uid],
    space: Option<&'a SpaceOrder>,
    engage: Option<&'a EngageOrder>,
}

/// An `intent.dispatched` is exactly the approved intent's order, whatever the rules version
/// (an audit check, not a decision rule): the task and vehicles as approved, and the space or
/// engagement order that follows from the intent, the space answer its last check read and
/// (engage) the approval's snapshot, the frozen plan ([`order_for`], what the engine
/// dispatches).
fn check_dispatch(
    r: &mut ReplayReport,
    e: &Event,
    tr: &Tracked,
    got: Dispatched<'_>,
    policy: Option<&Policy>,
) {
    let id = Some(tr.spec.id);
    if *got.task != tr.spec.task {
        bad(
            r,
            e,
            id,
            "dispatched task differs from the approved intent's",
            json(got.task),
            json(&tr.spec.task),
        );
    }
    if got.uids != tr.spec.uids.as_slice() {
        bad(
            r,
            e,
            id,
            "dispatched to other vehicles than the approved intent's",
            json(&got.uids),
            json(&tr.spec.uids),
        );
    }
    let Some(pol) = policy else { return };
    let answer = tr.last_inputs.as_deref().and_then(|w| w.space.answer());
    let want = match order_for(
        &tr.spec,
        answer,
        tr.snapshot.as_deref(),
        tr.loiter_radius_m,
        tr.preload.as_ref(),
        &pol.rules,
    ) {
        Ok(o) => dispatched_event(&tr.spec, &o),
        Err(err) => {
            bad(
                r,
                e,
                id,
                "dispatched although the approved intent gives no order",
                json(&(got.space, got.engage)),
                err.to_string(),
            );
            return;
        }
    };
    let EventBody::IntentDispatched {
        space: want_space,
        engage: want_engage,
        ..
    } = want
    else {
        return;
    };
    match (got.engage, &want_engage) {
        (None, None) => {}
        (Some(o), None) => bad(
            r,
            e,
            id,
            "engagement order on an intent that is not an engagement",
            json(o),
            "none".into(),
        ),
        (None, Some(w)) => bad(
            r,
            e,
            id,
            "engagement dispatched without its order",
            "none".into(),
            json(w),
        ),
        (Some(o), Some(w)) => {
            if o.uid != w.uid {
                bad(
                    r,
                    e,
                    id,
                    "engagement order sends another platform than the approved nomination's",
                    o.uid.to_string(),
                    w.uid.to_string(),
                );
            }
            if (o.intent, &o.track) != (w.intent, &w.track) {
                bad(
                    r,
                    e,
                    id,
                    "engagement order names another intent or track than the approved one",
                    json(&(o.intent, &o.track)),
                    json(&(w.intent, &w.track)),
                );
            }
            if (o.effect_point, o.effect_t, o.effect_radius_m, o.fallback)
                != (w.effect_point, w.effect_t, w.effect_radius_m, w.fallback)
            {
                bad(
                    r,
                    e,
                    id,
                    "engagement dispatched to something other than its approval's snapshot",
                    json(o),
                    json(w),
                );
            }
            if o.window_until != w.window_until {
                bad(
                    r,
                    e,
                    id,
                    "engagement order's window differs from the approved plan's",
                    json(&o.window_until),
                    json(&w.window_until),
                );
            }
            if o.loiter_radius_m != w.loiter_radius_m {
                bad(
                    r,
                    e,
                    id,
                    "engagement order's loiter radius differs from the platform's",
                    json(&o.loiter_radius_m),
                    json(&w.loiter_radius_m),
                );
            }
            if o.preload != w.preload {
                bad(
                    r,
                    e,
                    id,
                    "engagement order's lost-link preload is not the approved fallback on that platform",
                    json(&o.preload),
                    json(&w.preload),
                );
            }
        }
    }
    match (got.space, &want_space) {
        (None, None) => {}
        (Some(o), None) => bad(
            r,
            e,
            id,
            "vehicle intent dispatched with a space order",
            json(o),
            "none".into(),
        ),
        (None, Some(w)) => bad(
            r,
            e,
            id,
            "space intent dispatched without its order",
            "none".into(),
            json(w),
        ),
        (Some(o), Some(w)) if o != w => {
            let chosen = matches!(
                (o, w),
                (SpaceOrder::Collect(a), SpaceOrder::Collect(b))
                    if (a.norad_id, a.collect_time) != (b.norad_id, b.collect_time)
            );
            bad(
                r,
                e,
                id,
                if chosen {
                    "dispatched collect is not the chosen option"
                } else {
                    "dispatched space order differs from the one its approved intent and last answer give"
                },
                json(o),
                json(w),
            );
        }
        (Some(_), Some(_)) => {}
    }
}

/// The vehicle event a `system` stop quotes, as (kind, text): the bridge stops an engagement
/// when its platform's failsafe takes over (`the platform's failsafe took over (<text>)`) or
/// it leaves its mission mode (`the platform left its intercept (mode <text>)`), quoting the
/// `vehicle.event` it logged for the platform.
fn quoted_vehicle_event(reason: &str) -> Option<(&'static str, &str)> {
    for (prefix, kind) in [
        ("system: the platform's failsafe took over (", "failsafe"),
        ("system: the platform left its intercept (mode ", "mode"),
    ] {
        if let Some(text) = reason.strip_prefix(prefix).and_then(|t| t.strip_suffix(')')) {
            return Some((kind, text));
        }
    }
    None
}

/// Rules 13 ([`crate::rules::SPACE_REPLAY_SINCE`]): when a decision's space answer was
/// computed. Every answered space input says it (`computed_t`); a collect's answer was asked
/// at the scenario time of that computation (the map of the last `space.loaded`), but in a
/// re-check (`rechecked`), which keeps the request time the task was tasked at. So the time
/// logged is the answer's own, and the `space_query` rule's bound on it (recomputed with the
/// check: a collect's answer within [`crate::rules::PREPARED_SPACE_MAX_AGE_MS`] of its
/// check) holds for the answer the decision read. Neither the answer's computation nor the
/// decision's snapshot is later than the event that logs the decision (the review of rules
/// 13 moved both 4 s past it and the bound alone let it through).
fn check_space_computed(
    r: &mut ReplayReport,
    e: &Event,
    intent: IntentId,
    world: &WorldSnapshot,
    rechecked: bool,
    map: &ScenarioMap,
    pol: &Policy,
) {
    if !pol.rules.space_replay() {
        return;
    }
    let SpaceInputs::Answered { sha256, answer, computed_t } = &world.space else {
        return;
    };
    // the decision is logged no earlier than the snapshot it read (the engine stamps it so)
    if world.t > e.t {
        bad(r, e, Some(intent), "a decision on a space answer logged before the snapshot it read was taken", json(&world.t), format!("<= {}", e.t.0));
    }
    let Some(c) = computed_t else {
        bad(r, e, Some(intent), "a space answer without when it was computed (computed_t)", sha256.clone(), "computed_t".into());
        return;
    };
    // nor before its answer was computed
    if *c > e.t {
        bad(r, e, Some(intent), "a space answer computed after the decision that read it was logged", json(c), format!("<= {}", e.t.0));
    }
    if !rechecked
        && let Some(now) = answer.now()
        && now != map.scenario(*c)
    {
        bad(
            r,
            e,
            Some(intent),
            "a space answer asked at another time than it was computed at (its request time is not the scenario time of its computed_t)",
            json(&now),
            json(&map.scenario(*c)),
        );
    }
}

/// Decision inputs with the area reference resolved to the set in force and any space
/// answer reference to its logged answer.
fn resolve(
    inputs: &LoggedInputs,
    in_force: &InForce,
    answers: &Answers,
) -> Result<WorldSnapshot, &'static str> {
    if let LoggedSpace::Answered { sha256, .. } = &inputs.space
        && !answers.contains_key(sha256)
    {
        return Err("decision references a space answer that was not logged or did not verify");
    }
    let space = |h: &str| answers.get(h);
    let Some(want) = &inputs.areas_sha256 else {
        return Ok(inputs.resolve_with_space(None, space));
    };
    match in_force {
        Some((h, Some(set))) if h == want => {
            Ok(inputs.resolve_with_space(Some(set.clone()), space))
        }
        Some((h, None)) if h == want => {
            Err("decision references an area set whose hash did not verify")
        }
        _ => Err("decision references an area set that is not the one in force"),
    }
}

fn bad(
    r: &mut ReplayReport,
    e: &Event,
    intent: Option<IntentId>,
    what: &str,
    logged: String,
    recomputed: String,
) {
    r.mismatches.push(Mismatch {
        seq: e.seq,
        intent,
        what: what.to_string(),
        logged,
        recomputed,
    });
}

/// Why a transition lacks the evidence it needs, if it does.
fn unbacked(tr: &Tracked, t: Transition) -> Option<&'static str> {
    // rules 5: out of the queue only by a re-check's verdict, or its deadline
    if tr.state == State::Queued {
        return match t {
            Transition::Dispatch if tr.recheck != Some(Verdict::Dispatch) => {
                Some("dispatched from the queue without a passing re-check")
            }
            Transition::Dispatch if tr.dispatch_logged.is_none() => {
                Some("dispatched without its intent.dispatched order")
            }
            Transition::RequireApproval
                if !(tr.recheck == Some(Verdict::Inbox) || tr.queue_expired_inbox)
                    || tr.expires_t.is_none() =>
            {
                Some(
                    "sent back to the inbox without a re-check or a queue expiry that says so and a deadline",
                )
            }
            Transition::Expire if !tr.queue_expired || tr.queue_expired_inbox => {
                Some("expired from the queue without a queue.expired that expires it")
            }
            _ => None,
        };
    }
    match t {
        Transition::Requeue if tr.requeued_logged.is_none() => {
            Some("requeued without an intent.requeued")
        }
        Transition::Queue if tr.queued_logged.is_none() => {
            Some("queued without an intent.queued")
        }
        Transition::Check if tr.last_check_passed.is_none() => {
            Some("checked without a check.result")
        }
        Transition::ChecksFailed if tr.last_check_passed != Some(false) => {
            Some("rejected by rules that passed")
        }
        Transition::AutoApprove
            if tr.last_check_passed != Some(true) || tr.tier != Some(Tier::Auto) =>
        {
            Some("auto-approved without passing checks and an auto policy tier")
        }
        Transition::RequireApproval if tr.tier != Some(Tier::Human) || tr.expires_t.is_none() => {
            Some("sent for approval without a human tier and a deadline")
        }
        Transition::RequireApproval if tr.submitter_authorizes => {
            Some("sent for approval, but the submitter authorizes this tier")
        }
        Transition::AuthorizeAtSubmission
            if !tr.granted_at_submission
                || tr.last_check_passed != Some(true)
                || tr.tier != Some(Tier::Human) =>
        {
            Some(
                "approved at submission without passing checks, a human tier that allows it, and the submitter's approval.at_submission",
            )
        }
        Transition::Approve if !tr.granted || tr.last_check_passed != Some(true) => {
            Some("approved without a human approval and a passing re-check")
        }
        Transition::Approve
            if tr.signers.len() < usize::try_from(tr.approvers).unwrap_or(usize::MAX) =>
        {
            Some("approved without the signatures the tier needs")
        }
        Transition::Dispatch if tr.dispatch_logged.is_none() => {
            Some("dispatched without its intent.dispatched order")
        }
        Transition::Release if !tr.effect_logged => Some("released without an engage.effect"),
        Transition::Abort if !tr.stopped => Some("aborted without an engage.stopped"),
        Transition::Cancel if !tr.cancel_reported => {
            Some("cancelled after dispatch without the operator's cancel and the engine's cancelled report")
        }
        Transition::Complete if tr.spec.kind() == TaskKind::Engage && !tr.assessed => {
            Some("engagement completed without an assessment")
        }
        Transition::Fail if tr.spec.kind() == TaskKind::Engage => {
            Some("engagement failed like a task (it ends by its assessment or an abort)")
        }
        Transition::Reject if !tr.rejected_by_human => Some("rejected without a human rejection"),
        Transition::Expire if !tr.expired => Some("expired without an approval.expired"),
        _ => None,
    }
}

/// The intent an `operator.action intent.cancel` names (`"i12: note"`).
fn cancelled_intent(detail: &str) -> Option<IntentId> {
    let (id, _) = detail.split_once(':')?;
    id.strip_prefix('i')?.parse().ok().map(IntentId)
}

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| format!("<unserializable: {e}>"))
}

/// A decision's logged `waiting` fact (rules 5: the vehicles with a human-approved order
/// waiting in the queue) against the queued intents replay tracks at that point.
fn check_waiting(
    r: &mut ReplayReport,
    e: &Event,
    id: IntentId,
    logged: &BTreeMap<crate::ids::Uid, IntentId>,
    intents: &BTreeMap<IntentId, Tracked>,
) {
    let Some(tr) = intents.get(&id) else { return };
    let want = waiting_vehicles(
        intents.iter().map(|(i, t)| {
            (
                *i,
                &t.spec,
                t.state,
                t.basis.as_ref().is_some_and(|b| b.by_human),
                !t.committed.is_empty(),
            )
        }),
        &tr.spec,
    );
    if *logged != want {
        bad(
            r,
            e,
            Some(id),
            "human-order-waiting facts differ",
            json(logged),
            json(&want),
        );
    }
}

/// Rules 7: a decision's logged `restore_owed` fact (the intent's vehicles still owed their
/// own lost-link values by an engagement of this log) against the ledger replay keeps at
/// that point (none before rules 7, none for anything but a vehicle task).
fn check_restore_owed(
    r: &mut ReplayReport,
    e: &Event,
    id: IntentId,
    logged: &BTreeMap<Uid, queue::RestoreOwed>,
    intents: &BTreeMap<IntentId, Tracked>,
    restores: &Restores,
    policy: Option<&Policy>,
) {
    let (Some(tr), Some(pol)) = (intents.get(&id), policy) else { return };
    // rules 12: a carried entry's identity of the vehicle now is the bridge's input; whether it
    // holds is recomputed from it and the ledger
    let want = restores.owed_for(&tr.spec, &pol.rules, &carried_identities(logged));
    if *logged != want {
        bad(
            r,
            e,
            Some(id),
            "lost-link values owed back differ from the log's ledger",
            json(logged),
            json(&want),
        );
    }
}

/// A decision's logged `engaged` fact (the vehicles with an engagement in play) against the
/// engagements replay tracks at that point.
fn check_engaged(
    r: &mut ReplayReport,
    e: &Event,
    id: IntentId,
    logged: &BTreeMap<crate::ids::Uid, IntentId>,
    intents: &BTreeMap<IntentId, Tracked>,
) {
    let Some(tr) = intents.get(&id) else { return };
    let want = engaged_vehicles(intents.iter().map(|(i, t)| (*i, &t.spec, t.state)), &tr.spec);
    if *logged != want {
        bad(
            r,
            e,
            Some(id),
            "engagement-in-play facts differ",
            json(logged),
            json(&want),
        );
    }
}

/// Rules 6: a decision's logged `unknown_accepted` (the lost vehicles operators accepted for
/// an engagement) against the `engage.accepted` events replay has seen for it (none for any
/// other intent).
fn check_accepted(
    r: &mut ReplayReport,
    e: &Event,
    id: IntentId,
    logged: &BTreeMap<Uid, engage::AcceptedUnknown>,
    intents: &BTreeMap<IntentId, Tracked>,
) {
    let Some(tr) = intents.get(&id) else { return };
    if *logged != tr.accepted {
        bad(
            r,
            e,
            Some(id),
            "accepted lost vehicles differ from the acceptances logged for it",
            json(logged),
            json(&tr.accepted),
        );
    }
}

/// Rules 16: what an `engage.accepted` names.
enum Accepting<'a> {
    Vehicle(&'a Uid, &'a crate::world::VehicleFacts),
    Contact(&'a String, Option<&'a crate::lost::LostContact>),
    /// Rules 19: a live track as behaving now, with its facts then.
    Behaving(&'a String, &'a crate::world::TrackFacts),
}

/// Rules 16: the track ids decisions (and claims and confirmations) have read in their
/// pictures, each with the last fix and velocity they read and the world time of the latest
/// picture that showed it, and those the tracker dropped (`track.dropped`) with the fix each
/// drop carried: a track leaves the picture only by a drop, a drop carries no older fix than the
/// pictures had and no newer one that a picture taken after it did not show, and a dropped id is
/// never back with another fix (the bridge's tracker never reuses an id; a picture snapshotted
/// before the drop was handed over still holds the drop's own fix).
#[derive(Default)]
struct Pictured {
    seen: BTreeSet<String>,
    dropped: BTreeMap<String, (crate::world::Fact<crate::geo::LonLat>, Option<[f64; 2]>)>,
    last: BTreeMap<String, (crate::world::Fact<crate::geo::LonLat>, Option<[f64; 2]>)>,
    shown_t: BTreeMap<String, Time>,
}

/// Rules 16: how much later than a fix a picture must have been taken before replay holds that
/// it should have shown the fix (ms). The tracker dates a fix by its own clock when it takes the
/// detection (`last_seen_t`), and a snapshot reads its world time and its tracks a moment apart,
/// so a picture taken within this of a fix may honestly still show the one before.
const DROP_FIX_SKEW_MS: i64 = 1_000;

impl Pictured {
    /// The tracks of a picture taken at `world_t` and read at `e`: flagged if one read before is
    /// missing without a drop, or a dropped one is back with another fix than its drop's; then
    /// remembered.
    fn read(
        &mut self,
        r: &mut ReplayReport,
        e: &Event,
        id: Option<IntentId>,
        world_t: Time,
        tracks: &BTreeMap<String, crate::world::TrackFacts>,
    ) {
        let gone: Vec<&String> = self
            .seen
            .iter()
            .filter(|t| !tracks.contains_key(*t) && !self.dropped.contains_key(*t))
            .collect();
        if !gone.is_empty() {
            bad(
                r,
                e,
                id,
                "a track left the picture without a drop (track.dropped)",
                json(&gone),
                String::new(),
            );
        }
        for (k, t) in tracks {
            if let Some((fix, vel)) = self.dropped.get(k)
                && (t.position != *fix || t.velocity_mps != *vel)
            {
                bad(
                    r,
                    e,
                    id,
                    "a dropped track back in a picture with another fix than its drop's (the tracker never reuses an id)",
                    json(&(k, &t.position, t.velocity_mps)),
                    json(&(fix, vel)),
                );
            }
            self.seen.insert(k.clone());
            let newer = self.last.get(k).is_none_or(|(fix, _)| t.position.t >= fix.t);
            if newer {
                self.last.insert(k.clone(), (t.position.clone(), t.velocity_mps));
            }
            let shown = self.shown_t.entry(k.clone()).or_insert(world_t);
            *shown = (*shown).max(world_t);
        }
    }
}

/// Rules 16: the picture a claim or a confirmation logged: its identification as the ledger
/// holds it, taken at the event (within the acceptance skew), and read as a decision's is
/// ([`Pictured::read`]).
fn check_lost_picture(
    r: &mut ReplayReport,
    e: &Event,
    what: &str,
    world_t: Time,
    tracks: &BTreeMap<String, crate::world::TrackFacts>,
    ident: &crate::ident::Ledger,
    pictured: &mut Pictured,
) {
    if let Err(why) = ident.check_facts(tracks) {
        bad(r, e, None, &format!("{what}'s picture differs from the identification the log holds"), why, String::new());
    }
    if (e.t.0 - world_t.0).abs() > engage::ACCEPT_WORLD_SKEW_MS {
        bad(r, e, None, &format!("{what}'s picture is not the one at the event"), json(&world_t), json(&e.t));
    }
    pictured.read(r, e, None, world_t, tracks);
}

/// Rules 16: a decision's lost contacts and the acceptances of them against the log: an
/// engagement's are exactly the contacts the log holds as lost (`track.dropped`, less those
/// confirmed as a live track by `track.reacquired` while this picture bears the confirmation
/// out, their identification filled from the ledger) and the acceptances logged for it; any
/// other decision reads none. And no track the decisions read before is missing from this
/// picture without a drop.
#[allow(clippy::too_many_arguments)]
fn check_lost_inputs(
    r: &mut ReplayReport,
    e: &Event,
    id: IntentId,
    inputs: &LoggedInputs,
    lost: &crate::lost::LostLedger,
    ident: &crate::ident::Ledger,
    intents: &BTreeMap<IntentId, Tracked>,
    pictured: &mut Pictured,
    policy: Option<&Policy>,
) {
    let Some(pol) = policy.filter(|p| p.rules.lost_contacts()) else {
        return;
    };
    pictured.read(r, e, Some(id), inputs.t, &inputs.tracks);
    let engage = intents.get(&id).is_some_and(|tr| tr.spec.kind() == TaskKind::Engage);
    let (want, want_accepted) = if engage {
        (
            // in force, and those whose confirmation this picture voids (the claimant gone,
            // suspect or declared in it), logged as reinstated yet or not
            lost.in_force_for(&inputs.tracks, ident, pol.rules.rule_id()),
            intents.get(&id).map(|tr| tr.accepted_lost.clone()).unwrap_or_default(),
        )
    } else {
        (BTreeMap::new(), BTreeMap::new())
    };
    if inputs.lost_contacts != want {
        bad(
            r,
            e,
            Some(id),
            "a decision's lost contacts differ from the drops the log holds",
            json(&inputs.lost_contacts.keys().collect::<Vec<_>>()),
            json(&want.keys().collect::<Vec<_>>()),
        );
    }
    if inputs.lost_accepted != want_accepted {
        bad(
            r,
            e,
            Some(id),
            "a decision's accepted lost contacts differ from the acceptances logged for it",
            json(&inputs.lost_accepted),
            json(&want_accepted),
        );
    }
    // rules 19: and its live tracks accepted as behaving now (none before rules 19, none for
    // anything but an engagement)
    let want_behaving = if engage && pol.rules.accept_behaving() {
        intents.get(&id).map(|tr| tr.accepted_behaving.clone()).unwrap_or_default()
    } else {
        BTreeMap::new()
    };
    if inputs.behaving_accepted != want_behaving {
        bad(
            r,
            e,
            Some(id),
            "a decision's tracks accepted as behaving now differ from the acceptances logged for it",
            json(&inputs.behaving_accepted.keys().collect::<Vec<_>>()),
            json(&want_behaving.keys().collect::<Vec<_>>()),
        );
    }
}

/// A signature's evidence. An engagement's: it quotes a picture token that was issued to this
/// session and not used, unexpired, showing the track as the re-check saw it and the plan in
/// force (the frozen one, or the one this first signature froze), and its snapshot is the
/// re-check's on that plan. Any other intent's: no picture, and `supersedes` names exactly
/// the engagements in play on its vehicles that its re-check read.
#[allow(clippy::too_many_arguments)]
fn check_signed(
    r: &mut ReplayReport,
    e: &Event,
    id: IntentId,
    tr: &mut Tracked,
    by: &OperatorId,
    session: SessionId,
    snapshot: &Option<Box<EngageSnapshot>>,
    token: &Option<Box<PictureToken>>,
    supersedes: &[IntentId],
    checked_t: Option<Time>,
    issued: &mut BTreeMap<u64, PictureToken>,
    policy: Option<&Policy>,
) {
    if tr.last_check_passed != Some(true) {
        bad(
            r,
            e,
            Some(id),
            "signature without a passing re-check",
            json(&tr.last_check_passed),
            "true".into(),
        );
    }
    if tr.spec.kind() != TaskKind::Engage {
        if snapshot.is_some() || token.is_some() || checked_t.is_some() {
            bad(
                r,
                e,
                Some(id),
                "picture on an approval that is not an engagement",
                json(&(snapshot, token)),
                "none".into(),
            );
        }
        let mut want: Vec<IntentId> = tr
            .last_inputs
            .as_deref()
            .map(|w| w.engaged.values().copied().collect())
            .unwrap_or_default();
        want.sort();
        want.dedup();
        if supersedes != want.as_slice() {
            bad(
                r,
                e,
                Some(id),
                "approval stops engagements it did not acknowledge (or names others)",
                json(&supersedes),
                json(&want),
            );
        }
        return;
    }
    if !supersedes.is_empty() {
        bad(r, e, Some(id), "supersedes on an engagement signature", json(&supersedes), "none".into());
    }
    let Some(pol) = policy else { return };
    let Some(tok) = token.as_deref() else {
        bad(r, e, Some(id), "engagement signature without a picture token", "none".into(), "token".into());
        return;
    };
    match issued.remove(&tok.id) {
        Some(t) if t == *tok => {}
        other => bad(
            r,
            e,
            Some(id),
            "signature quotes a picture token that was not issued (or was used)",
            json(tok),
            json(&other),
        ),
    }
    // rules 6: a token taken before the acceptances changed is refused
    if tok.unknown_accepted != tr.accepted {
        bad(
            r,
            e,
            Some(id),
            "signature on a picture token whose accepted lost vehicles have changed since",
            json(&tok.unknown_accepted),
            json(&tr.accepted),
        );
    }
    // rules 16: and of lost contacts
    if tok.lost_accepted != tr.accepted_lost {
        bad(
            r,
            e,
            Some(id),
            "signature on a picture token whose accepted lost contacts have changed since",
            json(&tok.lost_accepted),
            json(&tr.accepted_lost),
        );
    }
    // rules 19: and of live tracks as behaving now (given, revoked or lapsed since)
    if tok.behaving_accepted != tr.accepted_behaving {
        bad(
            r,
            e,
            Some(id),
            "signature on a picture token whose tracks accepted as behaving now have changed since",
            json(&tok.behaving_accepted.keys().collect::<Vec<_>>()),
            json(&tr.accepted_behaving.keys().collect::<Vec<_>>()),
        );
    }
    if tok.intent != id || tok.by != *by || tok.session != session {
        bad(
            r,
            e,
            Some(id),
            "signature quotes another signer's picture token",
            json(&(tok.intent, &tok.by, tok.session)),
            json(&(id, by, session)),
        );
    }
    // the instant the engine read for the signature: after the event before its re-check,
    // not after the re-check itself; the token's expiry and the plan's freeze time are
    // read against it
    // rules 1 logged no instant: the approval's own time stands in for it
    let at = match checked_t {
        Some(at) => at,
        None if pol.rules.rules_version < 2 => e.t,
        None => {
            bad(r, e, Some(id), "engagement signature without the instant it was checked at", "none".into(), String::new());
            return;
        }
    };
    let v2 = pol.rules.rules_version >= 2;
    match tr.check_window {
        _ if !v2 => {}
        Some((lo, hi)) if lo <= at && at <= hi => {}
        other => bad(
            r,
            e,
            Some(id),
            "signature checked at an instant outside its re-check",
            json(&at),
            json(&other),
        ),
    }
    if at > tok.expires_t {
        bad(r, e, Some(id), "signature on an expired picture token", json(&tok.expires_t), json(&at));
    }
    let Some(w) = tr.last_inputs.as_deref() else {
        bad(r, e, Some(id), "engagement signature without its re-check", String::new(), String::new());
        return;
    };
    let track_now = w.tracks.get(&tok.track.id).map(|t| TrackIdentity::of(&tok.track.id, t));
    if track_now.as_ref() != Some(&tok.track) {
        bad(
            r,
            e,
            Some(id),
            "signature on a picture token whose track no longer matches",
            json(&tok.track),
            json(&track_now),
        );
    }
    let Some(plan) = w.plan.as_deref() else {
        bad(r, e, Some(id), "engagement signature re-checked without a plan", "none".into(), String::new());
        return;
    };
    match &tr.plan {
        Some(frozen) => {
            if !tok.frozen || tok.plan != frozen.preview || plan != frozen {
                bad(
                    r,
                    e,
                    Some(id),
                    "signature on another plan than the frozen one",
                    json(&tok.plan),
                    json(&frozen.preview),
                );
            }
        }
        None => {
            if tok.frozen
                || plan.preview != tok.plan
                || plan.by != *by
                || plan.track != tok.track
                || (v2 && plan.t != at)
                || plan.t > e.t
            {
                bad(
                    r,
                    e,
                    Some(id),
                    "first signature freezes another plan than the one it was shown",
                    json(plan),
                    json(tok),
                );
            }
            tr.plan = Some(plan.clone());
        }
    }
    let again = engage::snapshot(&tr.spec, w, &pol.rules).ok();
    match snapshot.as_deref() {
        None => bad(r, e, Some(id), "engagement signature without its picture", "none".into(), json(&again)),
        Some(s) if again.as_ref() != Some(s) => bad(
            r,
            e,
            Some(id),
            "engagement snapshot differs from the signature's re-check",
            json(snapshot),
            json(&again),
        ),
        Some(_) => {}
    }
}

/// A picture token as issued: to the session's operator with engagement authority, who may
/// sign this pending engagement, showing the track as the logged world has it and the plan
/// in force (the frozen one, or the preview recomputed on the logged world), valid
/// `token_ttl_s`.
#[allow(clippy::too_many_arguments)]
fn check_issued(
    r: &mut ReplayReport,
    e: &Event,
    tok: &PictureToken,
    inputs: &LoggedInputs,
    intents: &BTreeMap<IntentId, Tracked>,
    sessions: &BTreeMap<SessionId, (OperatorId, Grants)>,
    areas: &InForce,
    answers: &Answers,
    policy: Option<&Policy>,
) {
    let id = tok.intent;
    match sessions.get(&tok.session) {
        Some((op, g)) if *op == tok.by && g.engage => {}
        other => bad(
            r,
            e,
            Some(id),
            "picture token issued to a session without engagement authority (or another operator's)",
            json(&(&tok.by, tok.session)),
            json(&other.map(|o| &o.0)),
        ),
    }
    let Some(tr) = intents.get(&id) else {
        bad(r, e, Some(id), "picture token for an unknown intent", String::new(), String::new());
        return;
    };
    if tr.spec.kind() != TaskKind::Engage || tr.state != State::PendingApproval {
        bad(
            r,
            e,
            Some(id),
            "picture token for an intent that is not a pending engagement",
            json(&tr.state),
            "pending_approval".into(),
        );
    }
    if (tr.proposer_excluded && is_self_approval(&tr.spec.source, &tok.by)) || tr.signers.contains(&tok.by) {
        bad(r, e, Some(id), "picture token for someone who may not sign it", tok.by.to_string(), String::new());
    }
    let Some(pol) = policy else { return };
    let ttl = (pol.rules.engage_rules().token_ttl() * 1000.0).round() as i64;
    if tok.expires_t != tok.t.plus_ms(ttl) || tok.t > e.t {
        bad(
            r,
            e,
            Some(id),
            "picture token lifetime differs from policy",
            json(&(tok.t, tok.expires_t)),
            json(&tok.t.plus_ms(ttl)),
        );
    }
    let w = match resolve(inputs, areas, answers) {
        Ok(w) => w,
        Err(why) => {
            bad(r, e, Some(id), why, String::new(), String::new());
            return;
        }
    };
    let track = w.tracks.get(&tok.track.id).map(|t| TrackIdentity::of(&tok.track.id, t));
    if track.as_ref() != Some(&tok.track) {
        bad(r, e, Some(id), "picture token's track differs from the world it was issued on", json(&tok.track), json(&track));
    }
    // rules 6: every signer is shown the lost vehicles accepted for it
    if tok.unknown_accepted != tr.accepted {
        bad(
            r,
            e,
            Some(id),
            "picture token's accepted lost vehicles differ from the acceptances logged for it",
            json(&tok.unknown_accepted),
            json(&tr.accepted),
        );
    }
    // rules 16: and the lost contacts
    if tok.lost_accepted != tr.accepted_lost {
        bad(
            r,
            e,
            Some(id),
            "picture token's accepted lost contacts differ from the acceptances logged for it",
            json(&tok.lost_accepted),
            json(&tr.accepted_lost),
        );
    }
    // rules 19: and the live tracks accepted as behaving now
    if tok.behaving_accepted != tr.accepted_behaving {
        bad(
            r,
            e,
            Some(id),
            "picture token's tracks accepted as behaving now differ from the acceptances logged for it",
            json(&tok.behaving_accepted.keys().collect::<Vec<_>>()),
            json(&tr.accepted_behaving.keys().collect::<Vec<_>>()),
        );
    }
    match &tr.plan {
        Some(frozen) => {
            if !tok.frozen || tok.plan != frozen.preview {
                bad(r, e, Some(id), "picture token shows another plan than the frozen one", json(&tok.plan), json(&frozen.preview));
            }
        }
        None => {
            let again = engage::preview(&tr.spec, &w, &pol.rules).ok();
            if tok.frozen || again.as_ref() != Some(&tok.plan) {
                bad(r, e, Some(id), "picture token's plan differs from the preview on its world", json(&tok.plan), json(&again));
            }
        }
    }
}

/// A terrain answer taken from a nearby sample: within [`engage::TERRAIN_NEAR_M`] of the
/// point, and as far as it says; rules 17: the answers along a leg within
/// [`crate::route::LEG_NEAR_M`] of their points.
fn check_terrain(r: &mut ReplayReport, e: &Event, id: IntentId, w: &WorldSnapshot) {
    let crate::world::TerrainInputs::Sampled { samples, along } = &w.terrain else {
        return;
    };
    for l in along {
        if let Some(d) = l.near_m
            && !(0.0..=crate::route::LEG_NEAR_M).contains(&d)
        {
            bad(
                r,
                e,
                Some(id),
                "terrain answers along a leg taken from samples too far from their points",
                format!("{d:.1} m"),
                format!("at most {:.0} m", crate::route::LEG_NEAR_M),
            );
        }
    }
    for s in samples {
        let Some(from) = &s.from else { continue };
        let p = crate::geo::LonLat::new(s.lon_e7 as f64 / 1e7, s.lat_e7 as f64 / 1e7);
        let q = crate::geo::LonLat::new(from.lon_e7 as f64 / 1e7, from.lat_e7 as f64 / 1e7);
        let d = crate::geo::haversine_m(p, q);
        if from.distance_m > engage::TERRAIN_NEAR_M || (d - from.distance_m).abs() > 1.0 {
            bad(
                r,
                e,
                Some(id),
                "terrain answer from a sample too far from the point, or not as far as it says",
                json(from),
                format!("{d:.1} m (at most {:.0} m)", engage::TERRAIN_NEAR_M),
            );
        }
    }
}
