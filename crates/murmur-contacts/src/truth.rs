//! The truth message: what the contacts process publishes to the bridge's simulation channel
//! (`POST /api/sim/contacts`) and writes to the evaluation log, one JSON object per tick.
//! The bridge parses the same shape (`murmur-bridge/src/tracks/truth.rs`); keep them in step.
//!
//! ```json
//! {"t": 1781431200.0, "s": 12.0, "seq": 12, "scenario": "Golden Gate traffic", "seed": 7,
//!  "sensing": {"misclassification_rate": 0.05},
//!  "contacts": [{"id": "ferry-sausalito", "name": "Sausalito ferry", "affiliation": "neutral",
//!    "domain": "surface", "kind": "ferry", "lat": 37.81, "lon": -122.43, "alt_m": 0.0,
//!    "speed_mps": 11.0, "course_deg": 300.0, "emits": true, "signature": 1.2,
//!    "ais": {"mmsi": "366999001", "name": "MV SAUSALITO", "ship_type": "passenger", "spoofed": false}}]}
//! ```
//!
//! `t` is scenario time as unix seconds (`MURMUR_AT` plus elapsed x `MURMUR_RATE`), `s` the
//! seconds since the scenario started. Contacts that don't exist yet (or any more) are left
//! out, so the bridge can't see them either.

use serde::{Deserialize, Serialize};

use crate::motion::Mover;
use crate::scenario::{Affiliation, Ais, Domain, Sensing};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactTruth {
    pub id: String,
    pub name: String,
    pub affiliation: Affiliation,
    pub domain: Domain,
    pub kind: String,
    pub lat: f64,
    pub lon: f64,
    pub alt_m: f64,
    pub speed_mps: f64,
    pub course_deg: f64,
    pub emits: bool,
    pub signature: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ais: Option<Ais>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TruthMessage {
    pub t: f64,
    pub s: f64,
    pub seq: u64,
    pub scenario: String,
    pub seed: u64,
    pub sensing: Sensing,
    pub contacts: Vec<ContactTruth>,
}

fn round(x: f64, digits: i32) -> f64 {
    let k = 10f64.powi(digits);
    (x * k).round() / k
}

impl TruthMessage {
    pub fn build(
        t: f64,
        s: f64,
        seq: u64,
        scenario: &str,
        seed: u64,
        sensing: Sensing,
        movers: &[Mover],
    ) -> TruthMessage {
        TruthMessage {
            t: round(t, 3),
            s: round(s, 3),
            seq,
            scenario: scenario.into(),
            seed,
            sensing,
            contacts: movers
                .iter()
                .filter(|m| m.st.exists)
                .map(|m| ContactTruth {
                    id: m.contact.id.clone(),
                    name: m.contact.name.clone(),
                    affiliation: m.contact.affiliation,
                    domain: m.contact.domain,
                    kind: m.contact.kind.clone(),
                    lat: round(m.st.lat, 7),
                    lon: round(m.st.lon, 7),
                    alt_m: round(m.st.alt_m, 1),
                    speed_mps: round(m.st.speed_mps, 2),
                    course_deg: round(m.st.course_deg, 1),
                    emits: m.contact.emits,
                    signature: m.contact.signature,
                    ais: m.contact.ais.clone(),
                })
                .collect(),
        }
    }
}
