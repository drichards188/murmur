//! Risk-tier policy (Phase C.3): `sim/policy.toml`, first match wins.
//!
//! A `[[tier]]` entry matches when every condition in its `when` table holds (an empty
//! `when` always matches). If nothing matches, the decision is `human`: fail closed.
//! Conditions that can't be evaluated resolve in the conservative direction: a vehicle
//! with unknown platform counts as armed, and with no area set loaded (or no fresh
//! position or home) every route counts as leaving the approved area.
//!
//! One floor holds whatever the file says: an intent with an `Unverified` check (a rule
//! that couldn't verify its condition, such as reserves with unknown endurance) is never
//! auto-approved. If the first matching tier is `auto`, the decision becomes `human` with
//! rule `"(unverified)"`. The `unverified = [...]` condition lets the file route such
//! intents explicitly, with its own reason.
//!
//! **Who authorizes a human tier.** A human tier sends an agent's proposal to the inbox,
//! where only a human approves it. A human operator's own submission is already a human
//! decision: when `submitter_authorizes` holds for the matched tier (per tier, else
//! `[approval] submitter_authorizes`, default true), the submission itself is the approval
//! ([`PolicyDecision::submitter_authorizes`]; the engine records it as
//! `approval.at_submission`). Set it false on a tier to make even the submitter's own intent
//! wait in the inbox; `approvers` and `proposer_may_approve` (below) make the inbox approval
//! two-person. It never applies to an agent-sourced intent. In a logged policy without the key (logs written before it
//! existed) it reads as false, which is how those runs behaved.
//!
//! **Two-person approval** (Phase E). A human tier may set `approvers = N` (N distinct
//! operators must sign before the intent is approved) and `proposer_may_approve = false`
//! (the human who proposed it can't be one of them). An engagement has a floor whatever the
//! file says: it is never auto-approved and never approved at submission, its proposer never
//! approves it, and at least one approver signs it (DoD Directive 3000.09: appropriate human
//! judgment over the use of force); only an operator session with engagement authority may
//! sign it (`crate::Engine::approve_engagement`).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::time::Time;

use crate::areas::Area;
use crate::ids::Uid;
use crate::intent::{IntentSpec, Source, Task, TaskKind};
use crate::route::{Segment, route_for};
use crate::rules::{self, Outcome, RuleId, RulesConfig};
use crate::world::WorldSnapshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Approved by policy, no human needed.
    Auto,
    /// Waits in the inbox for a human.
    Human,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Human,
    Agent,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct When {
    /// Task kind is one of these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Vec<TaskKind>>,
    /// Some target is (true) / no target is (false) on an armed platform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub armed: Option<bool>,
    /// The task path leaves (true) / stays inside (false) the approved area.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaves_approved_area: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceKind>,
    /// Some check of one of these rules came out `unverified` (e.g. `["reserves"]`:
    /// endurance unknown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unverified: Option<Vec<RuleId>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TierRule {
    pub name: String,
    #[serde(default)]
    pub when: When,
    pub tier: Tier,
    pub reason: String,
    /// Human tiers: does a human's own submission authorize it (true), or does it wait for
    /// an approval like an agent's proposal (false)? Unset: `[approval]
    /// submitter_authorizes`. Ignored on auto tiers and for agent-sourced intents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitter_authorizes: Option<bool>,
    /// Human tiers: how many distinct operators must approve (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approvers: Option<u32>,
    /// Human tiers: may the human who proposed the intent be one of its approvers (default
    /// true; an engagement is always false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposer_may_approve: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApprovalConfig {
    /// Pending approvals expire after this many seconds.
    pub expire_s: u32,
    /// Default for tiers that don't set `submitter_authorizes`, and for the `(unverified)`
    /// and `(none)` fallbacks. [`Policy::from_toml`] fills in true when the file leaves it
    /// out; a policy deserialized from an old log keeps it unset, which means false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submitter_authorizes: Option<bool>,
    /// Per task kind: `[approval.kind.<kind>]` overrides `expire_s` or sets another
    /// deadline (`until = "tasking_lead"` for collect). Absent in logs written before it
    /// existed, which is how those runs behaved.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub kind: BTreeMap<TaskKind, KindApproval>,
}

impl Default for ApprovalConfig {
    fn default() -> Self {
        ApprovalConfig {
            expire_s: 120,
            submitter_authorizes: None,
            kind: BTreeMap::new(),
        }
    }
}

/// `[approval.kind.<kind>]`: how long a pending intent of that kind waits for a human.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KindApproval {
    /// Seconds, instead of `[approval] expire_s` (also the fallback when `until` can't be
    /// computed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expire_s: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<Until>,
}

/// A deadline set by the task rather than a fixed wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Until {
    /// Collect: the last moment the chosen pass can still be tasked, the collect time minus
    /// the class's tasking lead time ([`crate::space::tasking_deadline`]). An approval
    /// after it would fail the lead-time check anyway; expiring there tells the operator
    /// the real deadline.
    TaskingLead,
    /// Engage: the engagement's window closes (or the fixed wait, if that comes first). An
    /// approval after the window would fail its reach check anyway.
    Window,
}

/// Why a pending intent's deadline is what it is, when it is not a fixed wait (logged in
/// `approval.requested`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeadlineBasis {
    pub until: Until,
    /// The deadline in scenario time (engine `expires_t` is this mapped back through the
    /// segment's scenario map).
    pub scenario_t: Time,
}

/// When an intent sent to the inbox at engine time `now` expires: `[approval.kind.<kind>]`
/// if the policy has one, else `[approval] expire_s`. A collect with `until =
/// "tasking_lead"` and a matched option in `answer` expires at the tasking deadline (scenario
/// time, mapped to engine time through `map`); without one it falls back to the fixed wait.
pub fn approval_deadline(
    policy: &Policy,
    task: &crate::intent::Task,
    answer: Option<&crate::space::SpaceAnswer>,
    map: &crate::space::ScenarioMap,
    now: Time,
) -> (Time, Option<DeadlineBasis>) {
    let k = policy.approval.kind.get(&task.kind());
    let secs = k
        .and_then(|k| k.expire_s)
        .unwrap_or(policy.approval.expire_s);
    let fixed = now.plus_ms(i64::from(secs) * 1000);
    if k.and_then(|k| k.until) == Some(Until::Window)
        && let Task::Engage { window, .. } = task
    {
        // the window the nomination named, or the default from its creation (the engine
        // calls this just after creating it, so `now` stands in for the creation time)
        let until = window.map_or_else(
            || now.plus_ms(i64::from(policy.rules.engage_rules().default_window_s) * 1000),
            |w| w.until,
        );
        if until < fixed {
            return (
                until,
                Some(DeadlineBasis {
                    until: Until::Window,
                    scenario_t: until,
                }),
            );
        }
        return (fixed, None);
    }
    if k.and_then(|k| k.until) == Some(Until::TaskingLead) {
        let tol = policy.rules.space_rules().collect_match_tolerance_s;
        if let Some(s) = answer.and_then(|a| crate::space::tasking_deadline(task, a, tol)) {
            return (
                map.engine(s),
                Some(DeadlineBasis {
                    until: Until::TaskingLead,
                    scenario_t: s,
                }),
            );
        }
    }
    (fixed, None)
}

/// The whole policy file. Logged in full at run start; replay decides with the logged copy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default)]
    pub approval: ApprovalConfig,
    #[serde(default)]
    pub rules: RulesConfig,
    #[serde(rename = "tier")]
    pub tiers: Vec<TierRule>,
}

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("policy: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("policy {0}: {1}")]
    Io(String, std::io::Error),
    #[error("policy: {0}")]
    Invalid(String),
}

impl Policy {
    pub fn from_toml(text: &str) -> Result<Self, PolicyError> {
        let mut p: Policy = toml::from_str(text)?;
        // The file default: a human's own submission authorizes it. Written out explicitly
        // so the logged copy says so (an unset value in a log means false).
        p.approval.submitter_authorizes.get_or_insert(true);
        if p.tiers.is_empty() {
            return Err(PolicyError::Invalid("no [[tier]] rules".into()));
        }
        if p.approval.expire_s == 0 {
            return Err(PolicyError::Invalid("approval.expire_s must be > 0".into()));
        }
        for (kind, k) in &p.approval.kind {
            if k.expire_s == Some(0) {
                return Err(PolicyError::Invalid(format!(
                    "approval.kind.{}.expire_s must be > 0",
                    kind.as_str()
                )));
            }
            if k.until == Some(Until::TaskingLead) && *kind != TaskKind::Collect {
                return Err(PolicyError::Invalid(format!(
                    "approval.kind.{}: until = \"tasking_lead\" applies to collect only",
                    kind.as_str()
                )));
            }
            if k.until == Some(Until::Window) && *kind != TaskKind::Engage {
                return Err(PolicyError::Invalid(format!(
                    "approval.kind.{}: until = \"window\" applies to engage only",
                    kind.as_str()
                )));
            }
        }
        let r = &p.rules;
        if !(r.orbit_radius_margin.is_finite() && r.orbit_radius_margin >= 1.0) {
            return Err(PolicyError::Invalid(
                "rules.orbit_radius_margin must be >= 1".into(),
            ));
        }
        if !(r.reserve_pct.is_finite() && (0.0..=100.0).contains(&r.reserve_pct)) {
            return Err(PolicyError::Invalid(
                "rules.reserve_pct must be 0..100".into(),
            ));
        }
        if let Some(s) = &r.space {
            s.validate().map_err(PolicyError::Invalid)?;
        }
        if let Some(e) = &r.engage {
            e.validate().map_err(PolicyError::Invalid)?;
        }
        for t in &p.tiers {
            if t.approvers == Some(0) {
                return Err(PolicyError::Invalid(format!(
                    "tier {}: approvers must be at least 1",
                    t.name
                )));
            }
        }
        Ok(p)
    }

    pub fn load(path: &Path) -> Result<Self, PolicyError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| PolicyError::Io(path.display().to_string(), e))?;
        Self::from_toml(&text)
    }

    /// Whether a human's own submission authorizes an intent routed to `tier` (`None`: the
    /// `(unverified)` and `(none)` fallbacks, which use the `[approval]` default).
    pub fn submitter_authorizes(&self, tier: Option<&TierRule>) -> bool {
        tier.and_then(|t| t.submitter_authorizes)
            .or(self.approval.submitter_authorizes)
            .unwrap_or(false)
    }
}

/// The facts the policy conditions were evaluated on, kept with the decision so the
/// inbox can show why a human is needed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PolicyFacts {
    /// For collect and cover (no vehicles): `armed` is empty and `leaves_approved_area`
    /// false ("no path"); a satellite collect over any area is routed by `kind`.
    pub kind: TaskKind,
    pub source: SourceKind,
    /// Targets on armed platforms (or with unknown platform, counted as armed).
    pub armed: Vec<Uid>,
    pub leaves_approved_area: bool,
    pub area_detail: String,
    /// Checks a human must judge (outcome `unverified`), as `"rule uid: detail"`. Shown in
    /// the inbox as the reason the intent needs a human.
    pub unverified: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub tier: Tier,
    /// Name of the matching `[[tier]]` entry, or `"(none)"`.
    pub rule: String,
    pub reason: String,
    pub facts: PolicyFacts,
    /// Tier human, a human submitted it, and the tier lets the submitter authorize: the
    /// submission is the approval (`approval.at_submission`), no inbox. Always false for
    /// auto tiers and agent-sourced intents. Absent (false) in logs written before it existed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub submitter_authorizes: bool,
    /// Distinct operators who must approve (two-person approval: 2). Absent (1) in logs
    /// written before it existed.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub approvers: u32,
    /// The proposer may not approve. Always true for an engagement.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub proposer_excluded: bool,
}

fn one() -> u32 {
    1
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_one(n: &u32) -> bool {
    *n == 1
}

/// Does any vehicle's route (transit leg, task path, return leg; see [`crate::route`])
/// leave the approved area, and why. Every point must be inside an approved area and no
/// leg may touch an approved boundary. A vehicle whose route can't be built (no fresh
/// position, no home) counts as leaving.
pub fn leaves_approved_area(
    spec: &IntentSpec,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
) -> (bool, String) {
    let mut routes = Vec::new();
    for uid in &spec.uids {
        let Some(v) = world.vehicles.get(uid) else {
            return (
                true,
                format!("{uid}: not in the world model (counted as leaving)"),
            );
        };
        match route_for(spec, v, world, cfg) {
            Ok(route) if route.is_empty() => {}
            Ok(route) => routes.push((uid, route)),
            Err(e) => return (true, format!("{uid}: {e} (counted as leaving)")),
        }
    }
    if routes.is_empty() {
        return (false, "no path".into());
    }
    let Some(areas) = &world.areas else {
        return (true, "no area set loaded (counted as leaving)".into());
    };
    let approved: Vec<&Area> = areas.approved().collect();
    if approved.is_empty() {
        return (true, "no approved area defined (counted as leaving)".into());
    }
    let inside = |p| approved.iter().any(|a| a.contains(p));
    // Conservative: a leg touching any approved boundary counts as leaving, even if it
    // passes into an adjoining approved area.
    let crosses = |a, b| approved.iter().any(|ar| ar.segment_touches_boundary(a, b));
    // The task path is the same for every vehicle: check it once.
    if let Some(path) = routes.first().and_then(|(_, r)| r.task.as_ref()) {
        for (i, p) in path.points.iter().enumerate() {
            if !inside(*p) {
                return (
                    true,
                    format!("task path: waypoint {} is outside the approved area", i + 1),
                );
            }
        }
        for (i, a, b) in path.legs() {
            if crosses(a, b) {
                return (
                    true,
                    format!(
                        "task path: leg from waypoint {} crosses the approved-area boundary",
                        i + 1
                    ),
                );
            }
        }
    }
    for (uid, route) in &routes {
        for seg in route.segments() {
            if let Some(why) = segment_leaves(seg, &inside, &crosses) {
                return (true, format!("{uid}: {why}"));
            }
        }
    }
    (
        false,
        "inside the approved area (transit, task path, return)".into(),
    )
}

fn segment_leaves(
    seg: &Segment,
    inside: &dyn Fn(crate::geo::LonLat) -> bool,
    crosses: &dyn Fn(crate::geo::LonLat, crate::geo::LonLat) -> bool,
) -> Option<String> {
    let name = seg.name();
    for (p, label) in [(seg.from, &seg.from_label), (seg.to, &seg.to_label)] {
        if !inside(p) {
            return Some(format!("{name}: {label} is outside the approved area"));
        }
    }
    if crosses(seg.from, seg.to) {
        return Some(format!("{name} crosses the approved-area boundary"));
    }
    None
}

pub fn decide(spec: &IntentSpec, world: &WorldSnapshot, policy: &Policy) -> PolicyDecision {
    let d = decide_by_file(spec, world, policy);
    if spec.kind() != TaskKind::Engage {
        return d;
    }
    // The engagement floor: a human always, never at submission, never the proposer, at
    // least one approver; whatever the file says.
    let mut d = d;
    if d.tier == Tier::Auto {
        d.reason = format!(
            "tier {} is auto, but an engagement always needs human approval (DoD Directive 3000.09)",
            d.rule
        );
        d.rule = "(engage)".into();
        d.tier = Tier::Human;
        d.approvers = 1;
    }
    d.submitter_authorizes = false;
    d.proposer_excluded = true;
    d.approvers = d.approvers.max(1);
    d
}

fn decide_by_file(spec: &IntentSpec, world: &WorldSnapshot, policy: &Policy) -> PolicyDecision {
    let armed: Vec<Uid> = spec
        .uids
        .iter()
        .filter(|u| {
            world
                .vehicles
                .get(*u)
                .and_then(|v| v.platform.as_ref())
                .is_none_or(|p| p.armed)
        })
        .cloned()
        .collect();
    let (leaves, area_detail) = leaves_approved_area(spec, world, &policy.rules);
    // The same pure rules the engine ran on the same inputs: which checks need a human.
    let checks = rules::evaluate(spec, world, &policy.rules);
    let facts = PolicyFacts {
        kind: spec.kind(),
        source: match spec.source {
            Source::Human { .. } => SourceKind::Human,
            Source::Agent { .. } => SourceKind::Agent,
        },
        armed,
        leaves_approved_area: leaves,
        area_detail,
        unverified: rules::unverified(&checks),
    };
    let unverified_rules: Vec<RuleId> = checks
        .iter()
        .filter(|c| c.outcome == Outcome::Unverified)
        .map(|c| c.rule)
        .collect();
    let human_source = facts.source == SourceKind::Human;
    for rule in &policy.tiers {
        let w = &rule.when;
        let matches = w.kind.as_ref().is_none_or(|ks| ks.contains(&facts.kind))
            && w.armed.is_none_or(|a| a == !facts.armed.is_empty())
            && w.leaves_approved_area
                .is_none_or(|l| l == facts.leaves_approved_area)
            && w.source.is_none_or(|s| s == facts.source)
            && w.unverified
                .as_ref()
                .is_none_or(|rs| rs.iter().any(|r| unverified_rules.contains(r)));
        if matches {
            if rule.tier == Tier::Auto && !facts.unverified.is_empty() {
                return PolicyDecision {
                    tier: Tier::Human,
                    rule: "(unverified)".into(),
                    reason: format!(
                        "tier {} is auto, but a check could not be verified; a human must judge it",
                        rule.name
                    ),
                    facts,
                    submitter_authorizes: human_source && policy.submitter_authorizes(None),
                    approvers: 1,
                    proposer_excluded: false,
                };
            }
            let human = rule.tier == Tier::Human;
            return PolicyDecision {
                tier: rule.tier,
                rule: rule.name.clone(),
                reason: rule.reason.clone(),
                facts,
                submitter_authorizes: human
                    && human_source
                    && policy.submitter_authorizes(Some(rule)),
                approvers: if human { rule.approvers.unwrap_or(1).max(1) } else { 1 },
                proposer_excluded: human && rule.proposer_may_approve == Some(false),
            };
        }
    }
    PolicyDecision {
        tier: Tier::Human,
        rule: "(none)".into(),
        reason: "no policy rule matched; fail closed to a human".into(),
        facts,
        submitter_authorizes: human_source && policy.submitter_authorizes(None),
        approvers: 1,
        proposer_excluded: false,
    }
}
