//! Risk-tier policy (Phase C.3): `sim/policy.toml`, first match wins.
//!
//! A `[[tier]]` entry matches when every condition in its `when` table holds (an empty
//! `when` always matches). If nothing matches, the decision is `human`: fail closed.
//! Conditions that can't be evaluated resolve in the conservative direction: a vehicle
//! with unknown platform counts as armed, and with no area set loaded every path counts
//! as leaving the approved area.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::geo::TaskPath;
use crate::ids::Uid;
use crate::intent::{IntentSpec, Source, TaskKind};
use crate::rules::RulesConfig;
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub tier: Tier,
    /// Name of the matching `[[tier]]` entry, or `"(none)"`.
    pub rule: String,
    pub reason: String,
    pub facts: PolicyFacts,
}

/// Does the path leave the approved area, and why.
pub fn leaves_approved_area(path: Option<&TaskPath>, world: &WorldSnapshot) -> (bool, String) {
    let Some(path) = path else {
        return (false, "no waypoints".into());
    };
    let Some(areas) = &world.areas else {
        return (true, "no area set loaded (counted as leaving)".into());
    };
    let approved: Vec<_> = areas.approved().collect();
    if approved.is_empty() {
        return (true, "no approved area defined (counted as leaving)".into());
    }
    for (i, p) in path.points.iter().enumerate() {
        if !approved.iter().any(|a| a.contains(*p)) {
            return (
                true,
                format!("waypoint {} is outside the approved area", i + 1),
            );
        }
    }
    // Conservative: a leg touching any approved boundary counts as leaving, even if it
    // passes into an adjoining approved area.
    for (i, a, b) in path.legs() {
        if approved.iter().any(|ar| ar.segment_touches_boundary(a, b)) {
            return (
                true,
                format!(
                    "leg from waypoint {} crosses the approved-area boundary",
                    i + 1
                ),
            );
        }
    }
    (false, "inside the approved area".into())
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
    let (leaves, area_detail) = leaves_approved_area(spec.task.path().as_ref(), world);
    let facts = PolicyFacts {
        kind: spec.kind(),
        source: match spec.source {
            Source::Human { .. } => SourceKind::Human,
            Source::Agent { .. } => SourceKind::Agent,
        },
        armed,
        leaves_approved_area: leaves,
        area_detail,
    };
    for rule in &policy.tiers {
        let w = &rule.when;
        let matches = w.kind.as_ref().is_none_or(|ks| ks.contains(&facts.kind))
            && w.armed.is_none_or(|a| a == !facts.armed.is_empty())
            && w.leaves_approved_area
                .is_none_or(|l| l == facts.leaves_approved_area)
            && w.source.is_none_or(|s| s == facts.source);
        if matches {
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
