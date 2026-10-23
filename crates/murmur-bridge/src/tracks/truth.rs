//! The simulation channel: what the contacts process (`crates/murmur-contacts`) publishes
//! about contacts that are not ours. This is **truth** and must never reach the console or
//! an agent. It is parsed here, read only by the sensing model (`sensing.rs`) and by the
//! evaluation-only endpoint (`GET /api/sim/truth`, off unless `MURMUR_EVAL_TRUTH=1`), and
//! is not part of the push frame, `/api/state`, `/api/tracks`, the decision snapshot or the
//! CoT export. `tests.rs` checks that none of its fields leak into those.
//!
//! The shape mirrors `murmur-contacts/src/truth.rs`; keep them in step.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TruthAffiliation {
    Hostile,
    Suspect,
    Neutral,
    Unknown,
    AssumedFriend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    Air,
    Surface,
    Subsurface,
    Ground,
}

impl Domain {
    pub fn as_str(self) -> &'static str {
        match self {
            Domain::Air => "air",
            Domain::Surface => "surface",
            Domain::Subsurface => "subsurface",
            Domain::Ground => "ground",
        }
    }

    pub fn decide(self) -> murmur_decide::Domain {
        match self {
            Domain::Air => murmur_decide::Domain::Air,
            Domain::Surface => murmur_decide::Domain::Surface,
            Domain::Subsurface => murmur_decide::Domain::Subsurface,
            Domain::Ground => murmur_decide::Domain::Ground,
        }
    }
}

/// What a cooperative ship broadcasts. `spoofed` is truth-only: the AIS receiver hears the
/// same message either way, so it never leaves this module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ais {
    pub mmsi: String,
    pub name: String,
    pub ship_type: String,
    #[serde(default)]
    pub spoofed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactTruth {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub affiliation: TruthAffiliation,
    pub domain: Domain,
    pub kind: String,
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub alt_m: f64,
    #[serde(default)]
    pub speed_mps: f64,
    #[serde(default)]
    pub course_deg: f64,
    #[serde(default)]
    pub emits: bool,
    #[serde(default = "one")]
    pub signature: f64,
    #[serde(default)]
    pub ais: Option<Ais>,
}

fn one() -> f64 {
    1.0
}

/// Which sensing model turns truth into detections: the legacy one (`sensing.rs`, every record
/// before world v3) or the physical one (`physical/`, WORLD-V3-PLAN section 2), the default since
/// the platform catalog carries every sensor's figures (WORLD-V3-PLAN section 3). The v1 and v2
/// worlds and the Phase G stacks name `legacy` (docs/SENSING.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensingModel {
    Legacy,
    #[default]
    Physical,
}

impl SensingModel {
    pub fn as_str(self) -> &'static str {
        match self {
            SensingModel::Legacy => "legacy",
            SensingModel::Physical => "physical",
        }
    }
}

impl std::str::FromStr for SensingModel {
    type Err = String;
    fn from_str(s: &str) -> Result<SensingModel, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "legacy" => Ok(SensingModel::Legacy),
            "physical" => Ok(SensingModel::Physical),
            other => Err(format!("unknown sensing model {other:?} (legacy, physical)")),
        }
    }
}

/// The scenario's sensing section. Everything but the misclassification rate is the physical
/// model's (and absent from a legacy scenario's messages): the model it names, the terrain
/// and bathymetry grids (paths, relative to the repository), the conditions, and each
/// contact's signature overrides by contact id.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sensing {
    #[serde(default)]
    pub misclassification_rate: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<SensingModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terrain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bathymetry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<super::physical::conditions::Conditions>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub signatures: std::collections::BTreeMap<String, super::physical::signature::SignatureOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TruthMessage {
    /// Scenario time, unix seconds.
    pub t: f64,
    /// Seconds since the scenario started.
    #[serde(default)]
    pub s: f64,
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub scenario: String,
    #[serde(default)]
    pub seed: u64,
    #[serde(default)]
    pub sensing: Sensing,
    pub contacts: Vec<ContactTruth>,
    /// Rules 9: hostile acts committed since the last message (truth: the sensing model
    /// decides which of our sensors observe each, `sensing::observe_act`).
    #[serde(default)]
    pub acts: Vec<ActTruth>,
}

/// Rules 9: one abstract hostile act (a time, an author, a victim; no weapon), as the contacts
/// process scripts it. Truth: never served except on the evaluation endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActTruth {
    pub id: String,
    #[serde(default)]
    pub s: f64,
    #[serde(default)]
    pub t: f64,
    /// The contact that committed it, and where it was.
    pub by: String,
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub against_vehicle: Option<u64>,
    #[serde(default)]
    pub against_contact: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

impl TruthMessage {
    pub fn validate(&self) -> Result<(), String> {
        if !self.t.is_finite() {
            return Err("t must be a finite unix time".into());
        }
        for c in &self.contacts {
            if c.id.is_empty() {
                return Err("a contact has an empty id".into());
            }
            if !(c.lat.is_finite() && c.lon.is_finite() && c.lat.abs() <= 90.0 && c.lon.abs() <= 180.0) {
                return Err(format!("contact {}: bad position", c.id));
            }
        }
        Ok(())
    }
}
