//! The safe command queue (rules 5): orders for vehicles that can't be reached right now.
//!
//! An approved (or auto-approved) vehicle intent is dispatched only to vehicles the link can
//! carry it to now ([`reach`]). Otherwise it is **queued** (`intent.queued`, logged with the
//! vehicle facts the judgement read) with a deadline from `[queue]` in the policy file, per
//! task kind and bounded ([`QueueConfig::validate`]). When every vehicle is reachable again,
//! the engine re-runs the intent's checks and policy against the world at that moment
//! (`check.result` phase `dequeue`, then `queue.recheck`) and compares them with what the
//! approval rested on ([`Basis`]):
//!
//! - a check fails: **void** (fail closed, as an approval re-check would);
//! - an engagement came into play on its vehicles since: back to the **inbox** (dispatching
//!   would stop it unacknowledged);
//! - policy now needs a human and none approved it: **inbox**;
//! - a human approved it and the picture changed (a check's outcome, the policy tier or rule,
//!   a vehicle rebooted since): **inbox**, so a human looks again;
//! - otherwise **dispatch**, exactly the approved order.
//!
//! A queued intent that is still waiting at its deadline ends `expired` (`queue.expired`).
//! Safety commands get a short queue or none (`[queue.kind.hold] expire_s`; 0 = never
//! queued: dispatched at once whatever the link, as before).
//!
//! Everything here is a pure function of logged facts, so replay recomputes it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::catalog::AutopilotKind;
use crate::ids::{IntentId, OperatorId, SessionId, Uid};
use crate::intent::{IntentSpec, State, TaskKind};
use crate::policy::{PolicyDecision, Tier};
use crate::rules::{CheckResult, Outcome};
use crate::time::Time;
use crate::world::WorldSnapshot;

/// A link that reports less often than this (an AUV under water on its acoustic modem, 20 s
/// between reports) can't carry a mission upload: ArduPilot drops an upload after 8 s
/// without the next item, and a 300 bps modem takes tens of seconds per exchange.
pub const MISSION_LINK_MAX_INTERVAL_MS: u32 = 5_000;

/// A vehicle whose estimated boot instant is this long after the approval rebooted since:
/// the estimate (arrival time less the reported time since boot) is late by the link's
/// delay, seconds at most.
pub const BOOT_TOLERANCE_MS: i64 = 30_000;

/// `[queue]` in the policy file (rules 5; absent: these defaults).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QueueConfig {
    /// A queued order waits at most this long for its vehicles, seconds (1..3600).
    pub expire_s: u32,
    /// Reachable: heard from within this many of its link's expected report intervals (3: 3 s
    /// on a streaming link, 60 s on an acoustic one: the bridge's lost-link rule). 1..10.
    pub link_intervals: u32,
    /// Per task kind (`[queue.kind.<kind>]`): another wait, or none.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub kind: BTreeMap<TaskKind, KindQueue>,
}

impl Default for QueueConfig {
    fn default() -> Self {
        QueueConfig {
            expire_s: 900,
            link_intervals: 3,
            kind: BTreeMap::new(),
        }
    }
}

/// `[queue.kind.<kind>]`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KindQueue {
    /// Seconds instead of `[queue] expire_s` (0..3600). 0: never queued, dispatched at once
    /// whatever the link (the task layer then retries on its own).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expire_s: Option<u32>,
}

/// The longest any order may wait in the queue, seconds.
pub const MAX_QUEUE_S: u32 = 3_600;

impl QueueConfig {
    /// Every bound the file must meet (also run on a logged policy by replay).
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_QUEUE_S).contains(&self.expire_s) {
            return Err(format!(
                "queue.expire_s must be within 1..{MAX_QUEUE_S}, got {}",
                self.expire_s
            ));
        }
        if !(1..=10).contains(&self.link_intervals) {
            return Err(format!(
                "queue.link_intervals must be within 1..10, got {}",
                self.link_intervals
            ));
        }
        for (kind, k) in &self.kind {
            if !queueable(*kind) {
                return Err(format!(
                    "queue.kind.{}: only vehicle tasks are queued (orbit, patrol, hold, rtl, track)",
                    kind.as_str()
                ));
            }
            if let Some(s) = k.expire_s
                && s > MAX_QUEUE_S
            {
                return Err(format!(
                    "queue.kind.{}.expire_s must be within 0..{MAX_QUEUE_S}, got {s}",
                    kind.as_str()
                ));
            }
        }
        Ok(())
    }

    /// How long an order of `kind` may wait, seconds (0: never queued).
    pub fn expire_s(&self, kind: TaskKind) -> u32 {
        self.kind
            .get(&kind)
            .and_then(|k| k.expire_s)
            .unwrap_or(self.expire_s)
    }

    /// Whether an order of `kind` is ever held: a vehicle task whose wait is not 0.
    pub fn queues(&self, kind: TaskKind) -> bool {
        queueable(kind) && self.expire_s(kind) > 0
    }
}

/// The kinds the queue holds: vehicle tasks. Satellite requests have no vehicle link, and an
/// engagement holds its own picture (its link is one of its checks).
pub fn queueable(kind: TaskKind) -> bool {
    matches!(
        kind,
        TaskKind::Orbit | TaskKind::Patrol | TaskKind::Hold | TaskKind::Rtl | TaskKind::Track
    )
}

/// Whether the task reaches the vehicle as a mission upload (and so needs a link that can
/// carry one): orbit and patrol, and rtl on a sub (ArduSub has no RTL mode: the task layer
/// flies a mission home). Hold, rtl elsewhere and track are single commands.
pub fn needs_mission(kind: TaskKind, autopilot: Option<AutopilotKind>) -> bool {
    match kind {
        TaskKind::Orbit | TaskKind::Patrol => true,
        TaskKind::Rtl => autopilot == Some(AutopilotKind::Sub),
        _ => false,
    }
}

/// Whether one vehicle can be reached for this order now, and why.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reach {
    pub uid: Uid,
    pub reachable: bool,
    pub detail: String,
}

/// Can each of the intent's vehicles be reached for it now? Heard from within
/// `link_intervals` of its link's report interval (its last heartbeat, the `autopilot`
/// fact's time), and, for a mission upload, on a link that reports at least every
/// [`MISSION_LINK_MAX_INTERVAL_MS`]. Fails closed: no facts, no heartbeat, unreachable.
pub fn reach(spec: &IntentSpec, world: &WorldSnapshot, cfg: &QueueConfig) -> Vec<Reach> {
    spec.uids
        .iter()
        .map(|uid| {
            let out = |reachable: bool, detail: String| Reach {
                uid: uid.clone(),
                reachable,
                detail,
            };
            let Some(v) = world.vehicles.get(uid) else {
                return out(false, "not in the world model".into());
            };
            let Some(hb) = &v.autopilot else {
                return out(false, "never heard from (no heartbeat)".into());
            };
            let every = v.report_interval_ms.unwrap_or(1_000).max(1);
            let limit = i64::from(cfg.link_intervals) * i64::from(every);
            let age = world.t.ms_since(hb.t);
            let every_s = f64::from(every) / 1000.0;
            if age > limit {
                return out(
                    false,
                    format!(
                        "silent for {:.1} s (its link reports every {every_s} s: lost after {:.0} s)",
                        age as f64 / 1000.0,
                        limit as f64 / 1000.0
                    ),
                );
            }
            if needs_mission(spec.kind(), v.autopilot.as_ref().map(|f| f.value))
                && every > MISSION_LINK_MAX_INTERVAL_MS
            {
                return out(
                    false,
                    format!(
                        "on a link that reports every {every_s} s (an AUV on its acoustic modem): a mission upload can't cross it"
                    ),
                );
            }
            out(
                true,
                format!(
                    "heard {:.1} s ago (lost after {:.0} s)",
                    age.max(0) as f64 / 1000.0,
                    limit as f64 / 1000.0
                ),
            )
        })
        .collect()
}

pub fn all_reachable(r: &[Reach]) -> bool {
    !r.is_empty() && r.iter().all(|x| x.reachable)
}

/// The unreachable vehicles, `"3: silent for 12.0 s (...)"`.
pub fn unreachable_summary(r: &[Reach]) -> String {
    r.iter()
        .filter(|x| !x.reachable)
        .map(|x| format!("{}: {}", x.uid, x.detail))
        .collect::<Vec<_>>()
        .join("; ")
}

/// What an approval rested on: the checks it saw, the policy decision, the engagements in
/// play on its vehicles, who approved (a human, or the policy). Taken when the intent became
/// `approved` or `auto_approved`; a dequeue compares the world then with it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Basis {
    /// When the approval was decided (engine time).
    pub t: Time,
    /// A human approved it (in the inbox, or by submitting it in a tier that lets them);
    /// false: the policy's auto tier.
    pub by_human: bool,
    pub checks: Vec<CheckResult>,
    pub policy: Option<PolicyDecision>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub engaged: BTreeMap<Uid, IntentId>,
}

/// What a dequeue does with a queued intent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The checks pass and nothing a human relied on changed: dispatch the approved order.
    Dispatch,
    /// A human must decide again (policy now needs one, or the approved picture changed).
    Inbox,
    /// A check fails on the world now: void (fail closed).
    Void,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Dispatch => "dispatch",
            Verdict::Inbox => "inbox",
            Verdict::Void => "void",
        }
    }
}

/// A dequeue's judgement: the verdict and what changed since the approval.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recheck {
    pub verdict: Verdict,
    /// Each difference from the basis: a check's outcome, the policy tier or rule, an
    /// engagement now in play, a vehicle rebooted since; and the failing checks when void.
    pub changes: Vec<String>,
}

/// The re-check verdict for an intent queued on `basis`, given the checks and policy on the
/// world now (`now`: the dequeue check's inputs). See the module docs for the order.
pub fn assess(
    spec: &IntentSpec,
    basis: &Basis,
    results: &[CheckResult],
    passed: bool,
    decision: Option<&PolicyDecision>,
    now: &WorldSnapshot,
) -> Recheck {
    let mut changes: Vec<String> = Vec::new();
    // each check's outcome against the basis, by rule and vehicle
    for r in results {
        let before = basis
            .checks
            .iter()
            .find(|b| b.rule == r.rule && b.uid == r.uid)
            .map(|b| b.outcome);
        if before != Some(r.outcome) {
            let who = r.uid.as_ref().map(|u| format!(" {u}")).unwrap_or_default();
            changes.push(format!(
                "{}{who}: {} -> {} ({})",
                r.rule.as_str(),
                before.map_or("absent", outcome_name),
                outcome_name(r.outcome),
                r.detail
            ));
        }
    }
    for b in &basis.checks {
        if !results.iter().any(|r| r.rule == b.rule && r.uid == b.uid) {
            let who = b.uid.as_ref().map(|u| format!(" {u}")).unwrap_or_default();
            changes.push(format!("{}{who}: no longer evaluated", b.rule.as_str()));
        }
    }
    if let (Some(was), Some(d)) = (&basis.policy, decision) {
        let key = |p: &PolicyDecision| (p.tier, p.rule.clone(), p.approvers, p.proposer_excluded);
        if key(was) != key(d) {
            changes.push(format!(
                "policy: {} ({}) -> {} ({})",
                was.rule,
                tier_name(was.tier),
                d.rule,
                tier_name(d.tier)
            ));
        }
    }
    let mut engaged_new = false;
    for (u, e) in &now.engaged {
        if basis.engaged.get(u) != Some(e) {
            engaged_new = true;
            changes.push(format!("engagement {e} now in play on vehicle {u}"));
        }
    }
    for u in &spec.uids {
        if let Some(b) = now.vehicles.get(u).and_then(|v| v.booted_t)
            && b.ms_since(basis.t) > BOOT_TOLERANCE_MS
        {
            changes.push(format!(
                "vehicle {u} rebooted after the approval (booted {:.0} s after it)",
                b.ms_since(basis.t) as f64 / 1000.0
            ));
        }
    }
    let verdict = match decision {
        _ if !passed => Verdict::Void,
        None => Verdict::Void,
        Some(_) if engaged_new => Verdict::Inbox,
        Some(d) if d.tier == Tier::Human && !basis.by_human => Verdict::Inbox,
        Some(_) if basis.by_human && !changes.is_empty() => Verdict::Inbox,
        Some(_) => Verdict::Dispatch,
    };
    if verdict == Verdict::Void {
        for r in results.iter().filter(|r| r.outcome == Outcome::Fail) {
            let who = r.uid.as_ref().map(|u| format!(" {u}")).unwrap_or_default();
            let line = format!("{}{who} fails: {}", r.rule.as_str(), r.detail);
            if !changes.contains(&line) {
                changes.push(line);
            }
        }
    }
    Recheck { verdict, changes }
}

fn outcome_name(o: Outcome) -> &'static str {
    match o {
        Outcome::Pass => "pass",
        Outcome::Fail => "fail",
        Outcome::NotEvaluated => "not evaluated",
        Outcome::Unverified => "unverified",
    }
}

fn tier_name(t: Tier) -> &'static str {
    match t {
        Tier::Auto => "auto",
        Tier::Human => "human",
    }
}

/// Where a queued intent stands (on the intent, for the console and agents).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct QueueState {
    /// When it was queued (engine time).
    pub t: Time,
    /// It is dispatched, sent back, voided or expired by then.
    pub expires_t: Time,
    /// Why it was queued: each vehicle's reach then.
    pub reach: Vec<Reach>,
    /// The last dequeue's judgement, once there was one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recheck: Option<RecheckRecord>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RecheckRecord {
    pub t: Time,
    pub verdict: Verdict,
    pub changes: Vec<String>,
}

/// A human approval as carried into the next run's log (`intent.resumed`): who, under which
/// session of that run, when, and how. Shown to the operator and checked against the
/// previous log by replay; it authorizes nothing new (a resumed intent is re-checked before
/// it is dispatched).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CarriedApproval {
    pub by: OperatorId,
    pub session: SessionId,
    pub t: Time,
    pub at_submission: bool,
    pub self_approved: bool,
}

/// Where a resumed intent comes from: the previous run's log (its file name), that log's
/// head hash and last sequence number when it was read, and the state it ended in there.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResumedFrom {
    pub log: String,
    pub head: String,
    pub seq: u64,
    pub was: State,
}

/// An intent a log leaves open for the next run (replay's [`crate::ReplayReport::open`]): a
/// vehicle task queued, approved but not dispatched yet, or dispatched with ops still
/// running.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpenIntent {
    pub spec: IntentSpec,
    /// Its state at the end of the log.
    pub state: State,
    pub basis: Basis,
    /// The policy decision it carries (the basis's, or a re-check's).
    pub policy: Option<PolicyDecision>,
    pub approval: Option<CarriedApproval>,
    /// The queue deadline: queued, its own; approved or not committed, the basis time plus
    /// the kind's wait.
    pub expires_t: Option<Time>,
    /// Dispatched: each vehicle's last op state (`sent`, `acked`, `active`, `done`,
    /// `failed`, ...). A vehicle whose op reached `acked` (it accepted and started the order)
    /// is never sent it again: after a restart it is followed while it still runs it, or
    /// its op ends.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ops: BTreeMap<Uid, String>,
}

impl OpenIntent {
    /// The vehicles that accepted and started it (their last op `acked`, `active` or `done`).
    pub fn committed(&self) -> Vec<Uid> {
        self.ops
            .iter()
            .filter(|(_, s)| committed_op(s))
            .map(|(u, _)| u.clone())
            .collect()
    }

    /// The state it resumes in: dispatched when some vehicle had already started it
    /// (followed, never re-sent), else queued (re-checked, then delivered once).
    pub fn resumes_as(&self) -> State {
        if self.state == State::Dispatched && !self.committed().is_empty() {
            State::Dispatched
        } else {
            State::Queued
        }
    }
}

/// Op states that mean the vehicle accepted and started an order (`acked` and later, but not
/// an op that ended otherwise).
pub fn committed_op(state: &str) -> bool {
    matches!(state, "acked" | "active" | "done")
}

/// Op states an op ends in.
pub fn ended_op(state: &str) -> bool {
    matches!(state, "done" | "failed" | "superseded")
}
