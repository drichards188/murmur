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
//!   a vehicle rebooted since, a bridge restart since): **inbox**, so a human looks again;
//! - otherwise **dispatch**, exactly the approved order.
//!
//! A queued intent that is still waiting at its deadline ends `expired` (`queue.expired`); a
//! human's approval is held no longer than `[queue] human_expire_s` after it, and then goes
//! back to the inbox instead ([`QueueConfig::bound`]). Safety commands get a short queue or
//! none (`[queue.kind.hold] expire_s`; 0 = never queued: dispatched at once whatever the
//! link, as before). A dispatched order whose link was lost before any vehicle started it
//! comes back to the queue (`intent.requeued`, op state [`REQUEUED_OP`]). One whose start had
//! gone out when its link was lost ([`STARTING_OP`], then [`UNCONFIRMED_OP`]) may be running:
//! it comes back too, and its next delivery only follows it if the vehicle runs it; one the
//! vehicle does not run is never started again on its own, it goes to a human. An older queued
//! order never goes over a newer one for the same vehicle (the engine voids it).
//!
//! Everything here is a pure function of logged facts, so replay recomputes it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::catalog::AutopilotKind;
use crate::ids::{IntentId, OperatorId, SessionId, Uid};
use crate::intent::{IntentSpec, State, TaskKind};
use crate::policy::{PolicyDecision, Tier};
use crate::rules::{CheckResult, Outcome, QUEUE_FOLLOWUPS_SINCE, RulesConfig};
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

/// Why a `vehicle.rebooted` shows no reboot, if it doesn't: the time since boot went back,
/// or the boot instant moved later than the one before by more than [`BOOT_TOLERANCE_MS`].
pub fn reboot_mismatch(
    boot_ms_before: u64,
    boot_ms: u64,
    booted_t: Time,
    booted_before_t: Option<Time>,
) -> Option<&'static str> {
    if boot_ms < boot_ms_before {
        return None;
    }
    match booted_before_t {
        Some(b) if booted_t.ms_since(b) > BOOT_TOLERANCE_MS => None,
        _ => Some("reboot without the time since boot going back or the boot instant jumping later"),
    }
}

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
    /// A human's approval is good in the queue this long, seconds from the approval (1..3600,
    /// default 120, the inbox's own wait): an order a human approved that still waits then goes
    /// back to the inbox for a new look, whatever its kind's wait. Absent: the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub human_expire_s: Option<u32>,
}

impl Default for QueueConfig {
    fn default() -> Self {
        QueueConfig {
            expire_s: 900,
            link_intervals: 3,
            kind: BTreeMap::new(),
            human_expire_s: None,
        }
    }
}

/// [`QueueConfig::human_expire_s`] when the file leaves it out.
pub const HUMAN_EXPIRE_S: u32 = 120;

/// How long an op that lost its link waits for it at most, seconds, once the vehicle had
/// started the order (it follows it; nothing new is sent): the kind's queue wait, capped here.
/// Before the start, a lost link sends the order back to the queue at once (re-checked there).
pub const IN_FLIGHT_PATIENCE_S: u32 = 120;

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
        if let Some(h) = self.human_expire_s
            && !(1..=MAX_QUEUE_S).contains(&h)
        {
            return Err(format!(
                "queue.human_expire_s must be within 1..{MAX_QUEUE_S}, got {h}"
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

    /// How long a human's approval is good in the queue, seconds.
    pub fn human_expire_s(&self) -> u32 {
        self.human_expire_s.unwrap_or(HUMAN_EXPIRE_S)
    }

    /// The end of an order of `kind`'s wait begun at `from`.
    pub fn wait_end(&self, kind: TaskKind, from: Time) -> Time {
        from.plus_ms(i64::from(self.expire_s(kind)) * 1000)
    }

    /// The deadline of an order held in the queue from `now` (queued, requeued or carried over)
    /// whose kind's wait ends at `by_kind`, and what happens at it: past its kind's wait it
    /// expires (`false`); one a human approved is held no longer than `human_expire_s` after
    /// that approval (`basis.t`), and when that comes first it goes back to the inbox instead
    /// (`true`). Never before `now`.
    pub fn bound(&self, by_kind: Time, basis: &Basis, now: Time) -> (Time, bool) {
        if basis.by_human {
            let by_human = basis.t.plus_ms(i64::from(self.human_expire_s()) * 1000);
            // at or before: a deadline carried into the next run stays an inbox one
            if by_human <= by_kind {
                return (by_human.max(now), true);
            }
        }
        (by_kind.max(now), false)
    }

    /// How long an op that lost its link after the vehicle started the order may wait for it.
    pub fn in_flight_patience_s(&self, kind: TaskKind) -> u32 {
        self.expire_s(kind).min(IN_FLIGHT_PATIENCE_S)
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

/// A vehicle task whose start the task layer logs ahead of sending it (rules 7) and which a
/// vehicle owed its own lost-link values waits for: the kinds the queue holds and (rules 21) an
/// identify order, flown as an orbit but never queued (its track moves on) and never carried over
/// a restart.
pub fn vehicle_task(kind: TaskKind) -> bool {
    queueable(kind) || kind == TaskKind::Identify
}

/// Whether the task reaches the vehicle as a mission upload (and so needs a link that can
/// carry one): orbit and patrol, and rtl on a sub (ArduSub has no RTL mode: the task layer
/// flies a mission home). Hold, rtl elsewhere and track are single commands.
pub fn needs_mission(kind: TaskKind, autopilot: Option<AutopilotKind>) -> bool {
    match kind {
        TaskKind::Orbit | TaskKind::Patrol | TaskKind::Identify => true,
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
/// fact's time); for anything but a safety command, its navigation status and position
/// reported again within the freshness limit the checks apply (a link that just came back
/// brings the heartbeat first: the re-check must not read the picture from before the
/// outage); and, for a mission upload, on a link that reports at least every
/// [`MISSION_LINK_MAX_INTERVAL_MS`]. Fails closed: no facts, no heartbeat, unreachable.
pub fn reach(
    spec: &IntentSpec,
    world: &WorldSnapshot,
    cfg: &QueueConfig,
    rules: &RulesConfig,
) -> Vec<Reach> {
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
            // what the re-check reads must be as fresh as the checks need it (hold and rtl
            // are exempt from readiness, as in the rules)
            if !matches!(spec.kind(), TaskKind::Hold | TaskKind::Rtl) {
                let (fresh_ms, _) = rules.fact_age_limit_ms(v.report_interval_ms);
                let facts = [
                    ("navigation status", v.nav_ready.as_ref().map(|f| f.t)),
                    ("position", v.position.as_ref().map(|f| f.t)),
                ];
                for (what, t) in facts {
                    let Some(t) = t else {
                        return out(false, format!("heard, but no {what} reported yet"));
                    };
                    let age = world.t.ms_since(t);
                    if age > fresh_ms {
                        return out(
                            false,
                            format!(
                                "heard again, waiting for its {what} ({:.1} s old, limit {:.0} s)",
                                age as f64 / 1000.0,
                                fresh_ms as f64 / 1000.0
                            ),
                        );
                    }
                }
            }
            // rules 7: an engagement preloaded its own lost-link action as its fallback and
            // it is not confirmed back yet; a mission waits for the restore (a hold or rtl, the
            // safety commands, goes: the restore runs beside it)
            if rules.rules_version >= QUEUE_FOLLOWUPS_SINCE
                && !matches!(spec.kind(), TaskKind::Hold | TaskKind::Rtl)
                && let Some(owed) = world.restore_owed.get(uid).filter(|o| o.holds())
            {
                return out(false, owed.waiting_detail());
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

/// Rules 7 (murmur-85l): a vehicle's own lost-link values an engagement of this run preloaded
/// over (the `engage.preload` write-ahead logged them) and no confirmed restore has put back
/// yet. While they are owed the vehicle carries an engagement's lost-link action (its fallback
/// on a timer of a few seconds), so a mission is not sent to it: [`reach`] holds the order in
/// the queue until a restore confirms (a hold or rtl is never held). A decision's fact the
/// engine fills from its ledger (`restore_owed`) and replay recomputes from the log's.
/// Before rules 12, values carried over from an earlier run's log held no order (the log could
/// not tell which vehicle has the uid now); from rules 12 they do, unless the vehicle with the
/// uid now is known to be another one than they were preloaded on ([`CarriedHold`]: the
/// bridge's identity of it is logged with the fact, and replay recomputes the judgement).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RestoreOwed {
    /// The vehicle's own values, by parameter name.
    pub values: BTreeMap<String, f64>,
    /// The engagements that preloaded them (none: carried over from an earlier run's log).
    pub by: Vec<IntentId>,
    /// Rules 12: values carried over from an earlier run's log (no engagement of this run
    /// preloaded the platform since): where from, and whether the vehicle with the uid now is
    /// the one they were preloaded on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carried: Option<Box<CarriedHold>>,
}

/// Rules 12 (murmur-qjf): values carried over from an earlier run's log, owed to a vehicle of
/// this run's: the log, the vehicle they were preloaded on (that log's write-ahead) and the
/// one with the uid now as the bridge identifies it (its platform, callsign, system id, home;
/// absent: not known). They hold a mission, as an engagement's of this run do, unless `now`
/// is known to be another vehicle (`other_vehicle` says how: then the bridge does not write
/// them to it either, and nothing is held); a vehicle not known yet is held (fail closed).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CarriedHold {
    pub from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preloaded_on: Option<crate::engage::VehicleIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now: Option<crate::engage::VehicleIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_vehicle: Option<String>,
}

/// Rules 15 ([`crate::rules::CARRY_FOLLOWUPS_SINCE`], murmur-zq3): a platform spent by its own
/// effect in an earlier run, carried over at start (`run.started`'s `carried_from`, `spent`): the
/// log whose engagement spent it, that engagement, the vehicle it was (the engagement's
/// write-ahead) and the one with the uid now as the bridge identifies it. It takes no order and
/// no nomination (`readiness` fails), as one spent in this run, unless `now` is known to be
/// another vehicle (`other_vehicle` says how, by the rule of [`CarriedHold`]); a vehicle not
/// known yet is held (fail closed). A decision's fact the engine fills from its ledger
/// (`spent_carried`) and replay recomputes from the log's and the logged identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CarriedSpent {
    pub from: String,
    pub by: IntentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spent_on: Option<crate::engage::VehicleIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now: Option<crate::engage::VehicleIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_vehicle: Option<String>,
}

impl CarriedSpent {
    /// Whether the vehicle with the uid now is held as spent: unless it is known to be another.
    pub fn holds(&self) -> bool {
        self.other_vehicle.is_none()
    }

    /// Why it takes no order, in words.
    pub fn detail(&self) -> String {
        format!(
            "spent by its own effect (engagement {} of {}, carried over): a spent platform takes no order",
            self.by, self.from
        )
    }
}

impl RestoreOwed {
    /// Whether they hold a mission for the vehicle: always for values an engagement of this
    /// run preloaded; for carried ones unless the vehicle is known to be another.
    pub fn holds(&self) -> bool {
        self.carried.as_ref().is_none_or(|c| c.other_vehicle.is_none())
    }

    /// Since when they are owed, in words: the engagements that preloaded them, or the
    /// earlier log they were carried over from.
    pub fn since(&self) -> String {
        match &self.carried {
            Some(c) => format!("they were carried over from {}", c.from),
            None => {
                let by: Vec<String> = self.by.iter().map(ToString::to_string).collect();
                format!("engagement {} preloaded it", by.join(", "))
            }
        }
    }

    /// The values as a reason quotes them; rules 12, none known (an armed platform a failed
    /// carry's fleet named): says so.
    pub fn values_text(&self) -> String {
        match (&self.carried, self.values.is_empty()) {
            (Some(c), true) => format!("values unknown: {}, which may owe them, could not be read at the start", c.from),
            _ => self.values.iter().map(|(n, v)| format!("{n} {v}")).collect::<Vec<_>>().join(", "),
        }
    }

    /// Why an order other than a hold or rtl waits for it.
    pub fn waiting_detail(&self) -> String {
        let values: Vec<String> = self.values.iter().map(|(n, v)| format!("{n} {v}")).collect();
        if let (Some(c), true) = (&self.carried, self.values.is_empty()) {
            return format!(
                "heard, but its own lost-link failsafe may still be an engagement's: {} may owe it values (an armed platform of that run's fleet) and could not be read at the start: a mission waits until that log is read or an operator writes it off (a hold or rtl goes)",
                c.from
            );
        }
        match &self.carried {
            Some(c) => format!(
                "heard, but its own lost-link failsafe ({}) is not confirmed back since it was carried over from {} (an engagement there preloaded it): a mission waits for the restore (a hold or rtl goes; an operator writes the values off if this is not the vehicle they were preloaded on)",
                values.join(", "),
                c.from
            ),
            None => format!(
                "heard, but its own lost-link failsafe ({}) is not confirmed back since {}: a mission waits for the restore (a hold or rtl goes)",
                values.join(", "),
                self.since()
            ),
        }
    }
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

/// What an approval rested on, and what happened to it since: the input of [`assess`].
#[derive(Clone, Copy, Debug)]
pub struct Since<'a> {
    pub basis: &'a Basis,
    /// The basis is from the run before a bridge restart.
    pub carried: bool,
    /// When each vehicle was last logged rebooting in this run (`vehicle.rebooted`): one
    /// logged after the approval rebooted after the facts it rested on.
    pub rebooted: &'a BTreeMap<Uid, Time>,
}

/// The re-check verdict for an intent queued on `since.basis`, given the checks and policy
/// on the world now (`now`: the dequeue check's inputs). See the module docs for the order.
pub fn assess(
    spec: &IntentSpec,
    since: &Since<'_>,
    results: &[CheckResult],
    passed: bool,
    decision: Option<&PolicyDecision>,
    now: &WorldSnapshot,
) -> Recheck {
    let basis = since.basis;
    let mut changes: Vec<String> = Vec::new();
    // a human's look from before a bridge restart: another run, perhaps another picture
    if since.carried && basis.by_human {
        changes.push("approved before a bridge restart: a human looks again".into());
    }
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
        // a reboot logged after the approval (the facts it rested on are from before), or a
        // boot instant later than the approval by more than the estimate's error
        if let Some(t) = since.rebooted.get(u).filter(|t| **t > basis.t) {
            changes.push(format!(
                "vehicle {u} rebooted after the approval (seen {:.0} s after it)",
                t.ms_since(basis.t) as f64 / 1000.0
            ));
        } else if let Some(b) = now.vehicles.get(u).and_then(|v| v.booted_t)
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
    /// At the deadline it goes back to the inbox (a human's approval is held no longer than
    /// `[queue] human_expire_s`), not expired.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub inbox: bool,
    /// Why it was queued: each vehicle's reach then (empty when requeued or carried over).
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
    /// Rules 8: how that run authenticated its operators (its roster's `auth`; none for a
    /// log of earlier rules, whose ids were asserted). Intents are carried only between runs
    /// authenticated alike: an approval made on asserted ids never stands in a credentials
    /// run, nor the reverse; replay checks it against the previous log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<crate::authority::AuthMode>,
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
    /// The vehicles that ever accepted and started it (an op `acked`, `active` or `done`, in
    /// this run or one before): never sent it again, whatever happened after.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub committed: Vec<Uid>,
    /// It was dispatched before (in this run or one before), so a vehicle may hold it: the
    /// next delivery asks the vehicle what it holds first.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dispatched_before: bool,
    /// Each vehicle's platform (catalog id) as its last check saw it: a next run carries it
    /// only onto the same platforms under those uids.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub platforms: BTreeMap<Uid, String>,
    /// The vehicles that may have started it: its start went out (op `starting`) and nothing
    /// confirmed it before the link was lost (or the bridge stopped). Its next delivery there
    /// only follows it if the vehicle runs it; otherwise a human decides.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub maybe_started: Vec<Uid>,
}

impl OpenIntent {
    /// The vehicles that ever accepted and started it (sticky across op states and runs).
    pub fn committed(&self) -> Vec<Uid> {
        self.committed.clone()
    }

    /// The state it resumes in: dispatched when some vehicle had already started it
    /// (followed, never re-sent), else queued (re-checked, then delivered once; checked
    /// against what the vehicle holds first if it was dispatched before).
    pub fn resumes_as(&self) -> State {
        if self.state == State::Dispatched && !self.committed.is_empty() {
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
    matches!(
        state,
        "done" | "failed" | "superseded" | REQUEUED_OP | UNCONFIRMED_OP
    )
}

/// The vehicles a delivery only confirmed (`confirming`: the order may have started there)
/// whose op ended `unconfirmed`: they do not run it, so a human decides.
pub fn confirm_failed(
    confirming: &std::collections::BTreeSet<Uid>,
    ops: &BTreeMap<Uid, String>,
) -> Vec<Uid> {
    confirming
        .iter()
        .filter(|u| ops.get(*u).map(String::as_str) == Some(UNCONFIRMED_OP))
        .cloned()
        .collect()
}

/// Op states that say the order's start went out to the vehicle and nothing confirmed it:
/// the vehicle may have started it.
pub fn maybe_started_op(state: &str) -> bool {
    matches!(state, STARTING_OP | UNCONFIRMED_OP)
}

/// The op state of an op whose start went out (the mission mode, the hold or rtl mode, or a
/// new mission to a vehicle already flying one): from here a lost link means it may have
/// started.
pub const STARTING_OP: &str = "starting";

/// The op state of an op that ended with its start unconfirmed (rules 5): the link was lost
/// after the start went out, or a later delivery found the vehicle not running it. The order
/// goes back to the queue (with the first) or to a human (with the second).
pub const UNCONFIRMED_OP: &str = "unconfirmed";

/// The op state of an op whose link was lost before the vehicle started the order (rules 5):
/// it ends, and when every op of the order ended so, the order goes back to the queue
/// (`intent.requeued`), re-checked before it is sent again.
pub const REQUEUED_OP: &str = "requeued";
