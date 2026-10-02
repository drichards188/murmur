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

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Sensing {
    #[serde(default)]
    pub misclassification_rate: f64,
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
