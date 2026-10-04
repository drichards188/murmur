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
//!   `engage.stopped`, a release by an `engage.effect`, and the end by an assessment.
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
//! logged inputs were true of the world: it proves the decisions follow from them. The
//! chain is tamper-evident, not tamper-proof (see [`crate::audit`]): a full rewrite or a
//! truncated tail needs the head hash ([`ReplayReport::head_sha256`]) checked against an
//! external anchor.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::areas::AreaSet;
use crate::audit::{Event, EventBody, LogError, parse_log};
use crate::authority::Grants;
use crate::engage::{
    self, EffectRecord, EngageOrder, EngageSnapshot, Fallback, FrozenPlan, ObserveStep,
    PictureToken, StopCause, TrackIdentity,
};
use crate::engine::{
    dispatched_event, engaged_vehicles, is_self_approval, op_state_mismatch, op_state_owner,
    order_for, supersede_mismatch,
};
use crate::ids::{OperatorId, SessionId, Uid};
use crate::hash::{GENESIS, sha256_hex};
use crate::ids::IntentId;
use crate::intent::{IntentSpec, Source, State, Task, TaskKind, Transition, next_state};
use crate::policy::{self, Policy, Tier};
use crate::rules;
use crate::space::{ScenarioMap, SpaceAnswer, SpaceOracle, SpaceOrder};
use crate::time::Time;
use crate::world::{LoggedInputs, LoggedSpace, WorldSnapshot};

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
}

impl ReplayReport {
    /// Every decision reproduced from the logged inputs (including logged space answers).
    pub fn ok(&self) -> bool {
        self.mismatches.is_empty()
    }

    /// Every logged space query was re-run on the same segment and reproduced.
    pub fn space_reproduced(&self) -> bool {
        self.cache_mismatches.is_empty() && self.space_recomputed == self.space_queries
    }
}

struct Tracked {
    spec: IntentSpec,
    state: State,
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
];

/// The first `run.started`'s claims, and everything in the log that contradicts them.
fn check_claims<S: AsRef<str>>(r: &mut ReplayReport, events: &[Event], lines: &[S], opts: &ReplayOptions<'_>) {
    let Some(first) = events.first() else { return };
    let EventBody::RunStarted {
        rules_version,
        policy_schema,
        build,
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
    let mut pinned: Option<i64> = None;
    // operator sessions as opened: who, with what grants
    let mut sessions: BTreeMap<SessionId, (OperatorId, Grants)> = BTreeMap::new();
    // picture tokens issued and not used yet
    let mut issued: BTreeMap<u64, PictureToken> = BTreeMap::new();

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
        prev = Some(e);

        match &e.body {
            EventBody::RunStarted {
                policy: p,
                areas: a,
                rules_version,
                ..
            } => {
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
                scenario,
                pinned_shift_days,
                ..
            } => {
                r.space_segments += 1;
                let map = scenario.unwrap_or(ScenarioMap::IDENTITY);
                if let Err(why) = map.validate() {
                    bad(&mut r, e, None, "invalid scenario map", why, String::new());
                }
                scenario_map = map;
                pinned = *pinned_shift_days;
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
            EventBody::IntentCreated { intent } => {
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
                intents.insert(
                    intent.id,
                    Tracked {
                        spec: intent.clone(),
                        state: State::Proposed,
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
                        dispatch_logged: None,
                        dispatched: false,
                        ready_t: None,
                        stop_fallback: None,
                        platform_events: Vec::new(),
                    },
                );
                if intent.kind() == TaskKind::Engage {
                    r.engagements += 1;
                }
            }
            EventBody::CheckResult {
                intent,
                inputs,
                results,
                passed,
                preview,
                ..
            } => {
                check_engaged(&mut r, e, *intent, &inputs.engaged, &intents);
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
                tr.last_inputs = Some(Box::new(inputs));
                tr.check_window = Some((before_t, e.t));
            }
            EventBody::PolicyDecision {
                intent,
                inputs,
                decision,
            } => {
                check_engaged(&mut r, e, *intent, &inputs.engaged, &intents);
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
                tr.tier = Some(again.tier);
                tr.submitter_authorizes = again.submitter_authorizes;
                tr.approvers = again.approvers.max(1);
                tr.proposer_excluded = again.proposer_excluded;
                tr.rule = Some(again.rule);
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
                check_signed(&mut r, e, *intent, tr, by, *session, snapshot, token, supersedes, *checked_t, &mut issued, policy.as_ref());
                if tr.spec.kind() == TaskKind::Engage {
                    tr.last_picture_t = snapshot.as_ref().map(|s| s.t);
                    tr.snapshot = snapshot.clone();
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
            }
            EventBody::ApprovalAtSubmission {
                intent,
                by,
                self_approved,
                rule,
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
            }
            EventBody::ApprovalRejected { intent, .. } => {
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
                    }
                    Transition::Release => {
                        tr.effect_logged = false;
                        r.effects += 1;
                    }
                    Transition::Abort => {
                        tr.stopped = false;
                        r.aborts += 1;
                    }
                    Transition::Void => tr.stopped = false,
                    _ => {}
                }
                tr.state = *to;
            }
            EventBody::IntentDispatched {
                intent,
                task,
                uids,
                space: order,
                engage: engage_order,
            } => {
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
                if !matches!(tr.state, State::AutoApproved | State::Approved) {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "dispatched without approval",
                        json(&tr.state),
                        "auto_approved|approved".into(),
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
            EventBody::OperatorAction {
                operator,
                session,
                action,
                grants,
                ..
            } => {
                if action == "session.opened" {
                    sessions.insert(*session, (operator.clone(), grants.unwrap_or_default()));
                }
            }
            EventBody::EngagePicture {
                intent,
                inputs,
                results,
                holds,
                ready,
            } => {
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
                let (again, h, rd) =
                    engage::picture(&tr.spec, &snap, &w, &pol.rules, dispatched, prev_t);
                tr.last_picture_t = Some(e.t);
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
                let superseder = match cause {
                    StopCause::Superseded { by, .. } => {
                        intents.get(by).map(|t| (t.spec.clone(), t.dispatched))
                    }
                    _ => None,
                };
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
                        Some((spec, by_dispatched)) => {
                            if let Some(why) = supersede_mismatch(
                                *intent,
                                *by,
                                tr.spec.uids.first(),
                                spec,
                                *by_dispatched,
                            ) {
                                bad(
                                    &mut r,
                                    e,
                                    Some(*intent),
                                    why,
                                    by.to_string(),
                                    json(&(&spec.uids, by_dispatched)),
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
            EventBody::OpState {
                intent,
                uid,
                state,
                detail,
            } => {
                let Some(tr) = intents.get(intent) else {
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
                if let Some(why) = op_state_mismatch(
                    &tr.spec,
                    tr.state,
                    tr.dispatched,
                    tr.stop_fallback,
                    uid,
                    state,
                    detail,
                ) {
                    let reports_under = op_state_owner(&tr.spec, uid).err();
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        why,
                        json(&(uid, state, detail)),
                        json(&(tr.state, tr.dispatched, reports_under, tr.stop_fallback)),
                    );
                }
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
            EventBody::AgentAction { .. } => {}
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
    }
    r.intents = intents.len();
    r
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
        Ok(again) if again == *logged => {}
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
    let want = match order_for(&tr.spec, answer, tr.snapshot.as_deref(), &pol.rules) {
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

/// Decision inputs with the area reference resolved to the set in force and any space
/// answer reference to its logged answer.
fn resolve(
    inputs: &LoggedInputs,
    in_force: &InForce,
    answers: &Answers,
) -> Result<WorldSnapshot, &'static str> {
    if let LoggedSpace::Answered { sha256 } = &inputs.space
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
    match t {
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

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| format!("<unserializable: {e}>"))
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
/// point, and as far as it says.
fn check_terrain(r: &mut ReplayReport, e: &Event, id: IntentId, w: &WorldSnapshot) {
    let crate::world::TerrainInputs::Sampled { samples } = &w.terrain else {
        return;
    };
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
