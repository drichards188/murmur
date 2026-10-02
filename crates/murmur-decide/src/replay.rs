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
//! - `seq` is contiguous and `t` never goes backwards.
//!
//! Physics is not replayed; the decision layer is. What replay cannot know is whether the
//! logged inputs were true of the world: it proves the decisions follow from them.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::audit::{Event, EventBody, LogError, parse_log};
use crate::ids::IntentId;
use crate::intent::{IntentSpec, State, Transition, next_state};
use crate::policy::{self, Policy, Tier};
use crate::rules;
use crate::time::Time;

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
    pub mismatches: Vec<Mismatch>,
}

impl ReplayReport {
    pub fn ok(&self) -> bool {
        self.mismatches.is_empty()
    }
}

struct Tracked {
    spec: IntentSpec,
    state: State,
    last_check_passed: Option<bool>,
    tier: Option<Tier>,
    expires_t: Option<Time>,
    granted: bool,
    rejected_by_human: bool,
    expired: bool,
}

pub fn replay_str(text: &str) -> Result<ReplayReport, LogError> {
    Ok(replay(&parse_log(text)?))
}

pub fn replay(events: &[Event]) -> ReplayReport {
    let mut r = ReplayReport {
        events: events.len(),
        ..Default::default()
    };
    let mut policy: Option<Policy> = None;
    let mut intents: BTreeMap<IntentId, Tracked> = BTreeMap::new();
    let mut prev: Option<&Event> = None;

    for e in events {
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
            EventBody::RunStarted { policy: p, .. } => policy = Some(p.clone()),
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
                        expires_t: None,
                        granted: false,
                        rejected_by_human: false,
                        expired: false,
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
                let again = rules::evaluate(&tr.spec, inputs, &pol.rules);
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
                let again = policy::decide(&tr.spec, inputs, pol);
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
            }
            EventBody::ApprovalRequested { intent, expires_t } => {
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
                    let want = e.t.plus_ms(i64::from(pol.approval.expire_s) * 1000);
                    if *expires_t != want {
                        bad(
                            &mut r,
                            e,
                            Some(*intent),
                            "approval deadline differs from policy",
                            expires_t.0.to_string(),
                            want.0.to_string(),
                        );
                    }
                }
                tr.expires_t = Some(*expires_t);
            }
            EventBody::ApprovalGranted { intent, .. } => {
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
                tr.granted = true;
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
            EventBody::IntentDispatched { intent, .. } => {
                if let Some(tr) = intents.get(intent)
                    && !matches!(tr.state, State::AutoApproved | State::Approved)
                {
                    bad(
                        &mut r,
                        e,
                        Some(*intent),
                        "dispatched without approval",
                        json(&tr.state),
                        "auto_approved|approved".into(),
                    );
                }
            }
            EventBody::OpState { .. }
            | EventBody::VehicleEvent { .. }
            | EventBody::OperatorAction { .. }
            | EventBody::AgentAction { .. } => {}
        }
    }
    r.intents = intents.len();
    r
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
