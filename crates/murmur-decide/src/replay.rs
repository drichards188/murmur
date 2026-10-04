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
//!
//! **Space recomputation.** [`replay_str_with_space`] also re-runs every logged space query
//! on the segment given. If that segment is the one the query ran on (same
//! [`crate::SegmentId`] hash), the new answer must equal the logged one, else it is a
//! mismatch. If the segment differs (other element sets or catalog), the query is not
//! recomputed and the event goes to [`ReplayReport::cache_mismatches`]: a cache mismatch,
//! which says nothing about the decisions (they were re-decided from the logged answer).
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
use crate::engine::is_self_approval;
use crate::hash::{GENESIS, sha256_hex};
use crate::ids::IntentId;
use crate::intent::{IntentSpec, Source, State, Transition, next_state};
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
    /// Space queries that could not be recomputed because the segment given to replay is
    /// not the one they ran on. Not decision mismatches: [`Self::ok`] ignores them.
    pub cache_mismatches: Vec<Mismatch>,
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
}

/// Replay a JSONL log. The chain is checked over the lines exactly as they are in the
/// text, and each line must be its event's canonical serialization (so an edit that the
/// parser would ignore, such as an extra field, is still caught).
pub fn replay_str(text: &str) -> Result<ReplayReport, LogError> {
    replay_str_with_space(text, None)
}

/// [`replay_str`], and re-run every logged space query on `space` (see the module docs).
pub fn replay_str_with_space(
    text: &str,
    space: Option<&dyn SpaceOracle>,
) -> Result<ReplayReport, LogError> {
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
    let hashes = lines.iter().map(|l| sha256_hex(l.as_bytes())).collect();
    let mut r = replay_inner(&events, hashes, space);
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
    replay_with_space(events, None)
}

/// [`replay`], and re-run every logged space query on `space`.
pub fn replay_with_space(events: &[Event], space: Option<&dyn SpaceOracle>) -> ReplayReport {
    let hashes = events
        .iter()
        .map(|e| match serde_json::to_string(e) {
            Ok(line) => sha256_hex(line.as_bytes()),
            Err(err) => format!("<unserializable: {err}>"),
        })
        .collect();
    replay_inner(events, hashes, space)
}

/// The area set in force: its logged hash, and the set if that hash verified.
type InForce = Option<(String, Option<AreaSet>)>;

/// Logged space answers whose hash verified, by hash.
type Answers = BTreeMap<String, SpaceAnswer>;

fn replay_inner(
    events: &[Event],
    hashes: Vec<String>,
    space: Option<&dyn SpaceOracle>,
) -> ReplayReport {
    let mut r = ReplayReport {
        events: events.len(),
        head_sha256: hashes.last().cloned(),
        ..Default::default()
    };
    let mut policy: Option<Policy> = None;
    let mut areas: InForce = None;
    let mut intents: BTreeMap<IntentId, Tracked> = BTreeMap::new();
    let mut answers: Answers = BTreeMap::new();
    let mut prev: Option<&Event> = None;
    // from the last `space.loaded`
    let mut scenario_map = ScenarioMap::IDENTITY;
    let mut pinned: Option<i64> = None;

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
        prev = Some(e);

        match &e.body {
            EventBody::RunStarted {
                policy: p,
                areas: a,
                ..
            } => {
                policy = Some(p.clone());
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
                    },
                );
            }
            EventBody::CheckResult {
                intent,
                inputs,
                results,
                passed,
                ..
            } => {
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
                tr.last_check_passed = Some(again_passed);
            }
            EventBody::PolicyDecision {
                intent,
                inputs,
                decision,
            } => {
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
            EventBody::ApprovalGranted {
                intent,
                by,
                self_approved,
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
                tr.state = *to;
            }
            EventBody::IntentDispatched {
                intent,
                space: order,
                ..
            } => {
                if let Some(tr) = intents.get(intent) {
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
                    if let Some(why) = order_mismatch(&tr.spec, order.as_ref()) {
                        bad(
                            &mut r,
                            e,
                            Some(*intent),
                            why,
                            json(order),
                            json(&tr.spec.task),
                        );
                    }
                }
            }
            EventBody::OpState { .. }
            | EventBody::VehicleEvent { .. }
            | EventBody::OperatorAction { .. }
            | EventBody::OperatorView { .. }
            | EventBody::AgentAction { .. } => {}
        }
    }
    r.intents = intents.len();
    r
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

/// Why a dispatched order doesn't fit the intent, if it doesn't.
fn order_mismatch(spec: &IntentSpec, order: Option<&SpaceOrder>) -> Option<&'static str> {
    use crate::intent::Task;
    match (&spec.task, order) {
        (Task::Collect { chosen, .. }, Some(SpaceOrder::Collect(o))) => match chosen {
            Some(c) if c.norad_id == o.norad_id && c.collect_time == o.collect_time => None,
            _ => Some("dispatched collect is not the chosen option"),
        },
        (Task::Cover { service, .. }, Some(SpaceOrder::Cover(o))) if *service == o.service => None,
        (Task::Collect { .. } | Task::Cover { .. }, _) => {
            Some("space intent dispatched without its order")
        }
        (_, Some(_)) => Some("vehicle intent dispatched with a space order"),
        (_, None) => None,
    }
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
        Transition::Reject if !tr.rejected_by_human => Some("rejected without a human rejection"),
        Transition::Expire if !tr.expired => Some("expired without an approval.expired"),
        _ => None,
    }
}

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| format!("<unserializable: {e}>"))
}
