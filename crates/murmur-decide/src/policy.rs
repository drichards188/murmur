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

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::areas::Area;
use crate::ids::Uid;
use crate::intent::{IntentSpec, Source, TaskKind};
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApprovalConfig {
    /// Pending approvals expire after this many seconds.
    pub expire_s: u32,
}

impl Default for ApprovalConfig {
    fn default() -> Self {
        ApprovalConfig { expire_s: 120 }
    }
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
        let p: Policy = toml::from_str(text)?;
        if p.tiers.is_empty() {
            return Err(PolicyError::Invalid("no [[tier]] rules".into()));
        }
        if p.approval.expire_s == 0 {
            return Err(PolicyError::Invalid("approval.expire_s must be > 0".into()));
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
        Ok(p)
    }

    pub fn load(path: &Path) -> Result<Self, PolicyError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| PolicyError::Io(path.display().to_string(), e))?;
        Self::from_toml(&text)
    }
}

/// The facts the policy conditions were evaluated on, kept with the decision so the
/// inbox can show why a human is needed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PolicyFacts {
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
                };
            }
            return PolicyDecision {
                tier: rule.tier,
                rule: rule.name.clone(),
                reason: rule.reason.clone(),
                facts,
            };
        }
    }
    PolicyDecision {
        tier: Tier::Human,
        rule: "(none)".into(),
        reason: "no policy rule matched; fail closed to a human".into(),
        facts,
    }
}
