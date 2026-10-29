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

use crate::effect::EffectApplied;
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
    /// An engagement's abstract effect reached it (scenario unix s); it persists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_t: Option<f64>,
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
    /// Effects applied since the last message (evaluation only: what was inside each
    /// radius, the intended contact, collateral).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<EffectApplied>,
    /// Rules 9: hostile acts committed since the last message (truth: the bridge's sensing
    /// model decides which of our sensors observe each), with where the author was.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acts: Vec<ActTruth>,
    /// Opponent weapons (docs/OPPONENT-WEAPONS.md): our vehicles the scenario's weapons may be
    /// aimed at, whose positions the bridge's answer carries (`ours`); absent without weapons.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aim_at: Vec<u64>,
    /// Weapons in flight during the second up to this message (each from its launch to the
    /// message at or after its impact).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub weapons: Vec<WeaponTruth>,
    /// Weapons that struck since the last message, each once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub impacts: Vec<ImpactTruth>,
}

/// One weapon's flight (truth): a straight line at constant speed from its launch point to its
/// lead point, `[lon, lat, metres above the sea]`, from `launch_t` to `impact_t` (scenario unix
/// s). Its position at any instant follows ([`WeaponTruth::at`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeaponTruth {
    pub id: String,
    pub act: String,
    pub by: String,
    pub name: String,
    pub kind: String,
    pub speed_mps: f64,
    pub launch_t: f64,
    pub impact_t: f64,
    pub from: [f64; 3],
    pub to: [f64; 3],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub against_vehicle: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub against_contact: Option<String>,
}

impl WeaponTruth {
    /// Where it is at `t` (held at either end): `[lon, lat, alt]`.
    pub fn at(&self, t: f64) -> [f64; 3] {
        let span = self.impact_t - self.launch_t;
        let f = if span > 0.0 { ((t - self.launch_t) / span).clamp(0.0, 1.0) } else { 1.0 };
        [0, 1, 2].map(|i| self.from[i] + (self.to[i] - self.from[i]) * f)
    }
}

/// A weapon's strike (truth): when and where, on whom. Nothing about the victim changes (no
/// damage is modelled); the bridge reports it as the victim's own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImpactTruth {
    pub weapon: String,
    pub act: String,
    pub t: f64,
    pub lat: f64,
    pub lon: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub against_vehicle: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub against_contact: Option<String>,
}

/// Rules 9: one scripted hostile act as it happens: a time, an author, a victim, and the weapon
/// it launched if its author carries one (or why it held its fire).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActTruth {
    pub id: String,
    /// Scenario seconds and unix seconds it happened at.
    pub s: f64,
    pub t: f64,
    /// The contact that committed it, and where it was.
    pub by: String,
    pub lat: f64,
    pub lon: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub against_vehicle: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub against_contact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The weapon it launched (`WeaponTruth::id`); none: an abstract act.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weapon: Option<String>,
    /// The author held its fire (its victim beyond its weapon's range, or nowhere known): the act
    /// did not happen, and the bridge senses nothing of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<String>,
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
                    effect_t: m.effect_t,
                })
                .collect(),
            effects: Vec::new(),
            acts: Vec::new(),
            aim_at: Vec::new(),
            weapons: Vec::new(),
            impacts: Vec::new(),
        }
    }

    /// Rules 9: the scenario's acts that happen in `(prev_s, s]`, each by a contact that
    /// exists now (one that doesn't commits nothing), with where it is.
    pub fn acts_between(
        acts: &[crate::scenario::ActSpec],
        prev_s: f64,
        s: f64,
        t0: f64,
        movers: &[Mover],
    ) -> Vec<ActTruth> {
        acts.iter()
            .filter(|a| a.at_s > prev_s && a.at_s <= s)
            .filter_map(|a| {
                let m = movers.iter().find(|m| m.contact.id == a.by && m.st.exists)?;
                Some(ActTruth {
                    id: a.id.clone(),
                    s: round(s, 3),
                    t: round(t0 + s, 3),
                    by: a.by.clone(),
                    lat: round(m.st.lat, 7),
                    lon: round(m.st.lon, 7),
                    against_vehicle: a.against_vehicle,
                    against_contact: a.against_contact.clone(),
                    label: a.label.clone(),
                    weapon: None,
                    held: None,
                })
            })
            .collect()
    }
}
