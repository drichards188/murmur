//! `sim/scenarios/*.toml`: the contacts of a scenario and how they move.
//!
//! ```toml
//! title = "Golden Gate traffic"
//! seed = 7                      # motion jitter and the bridge's sensing draws
//!
//! [sensing]
//! misclassification_rate = 0.05 # injected classifier error, forwarded to the bridge
//!
//! [[contact]]
//! id = "ferry-sausalito"        # unique; the evaluation log keys on it
//! name = "Sausalito ferry"      # truth-side label (the C2 never sees it)
//! affiliation = "neutral"       # hostile | suspect | neutral | unknown | assumed_friend
//! domain = "surface"            # air | surface | subsurface | ground
//! kind = "ferry"                # sim/contacts.toml: the defaults each kind carries
//! speed_mps = 11
//! motion = "route"              # recorded | route | transit | loiter | station | drift
//! route = [[-122.3920, 37.7960], [-122.4790, 37.8560]]   # [lon, lat] (GeoJSON order)
//! reverse = true                # back and forth (default: loop back to the first point)
//! dwell_s = 300                 # wait at each end
//! ais = { mmsi = "366999001", name = "MV SAUSALITO", ship_type = "passenger" }
//! ```
//!
//! The bridge's physical sensing model (WORLD-V3-PLAN section 2, docs/SENSING.md) is chosen in
//! `[sensing]` (`model = "physical"`, with `terrain` and `bathymetry` grids, paths relative to
//! the repository); a top-level `[conditions]` gives `visibility_m`, `sea_state`, `wind_mps`,
//! `shipping`, `water_depth_m` and `sun_elevation_deg` (each optional); and a contact may
//! override its kind's signature with `dc_m`, `height_m`, `rcs_m2`, `thermal_k`, `source_db`,
//! `target_db`, `ais_class` (`a`, `b`, `none`) and `emitter_dbm`. All of it rides in the truth
//! message's `sensing` (absent when the scenario says none of it, so a legacy scenario's
//! messages are as before).
//!
//! Optional per contact: `alt_m` (negative = depth; defaults per domain), `emits` (has a
//! radar or transponder an ESM receiver can hear; default per kind), `signature` (detection
//! range multiplier, the legacy model's; default per kind), `turn_dps`, `start_s` and `end_s` (scenario seconds
//! the contact exists), `ais.spoofed = true` (the AIS identity is false: a suspect boat
//! claiming to be a fishing vessel), `on_effect` (what an engagement's effect does to it:
//! `"out_of_play"`, the default, removes it from the scenario when an abstract effect reaches it,
//! or when the damage model destroys it, so our sensors stop detecting it; `"persists"` keeps it
//! moving, flagged, whatever the draw), `strike_outcome` (pins what every strike aimed at it
//! does, in place of the damage model's hit and damage draws: `"destroyed"`, `"mobility"`,
//! `"firepower"` (an armed contact only), `"undamaged"` (a hit that does nothing) or `"miss"`; a
//! training aid, used by the tutorial's boat only (DECISIONS.md 11-03), refused beside
//! `on_effect = "persists"`; the truth log's record of the strike says `pinned`; an abstract
//! effect, whose weapon lists no figures, and collateral draws ignore it). `transit` moves once
//! from `route[0]` to the last point and holds there; `loiter` needs `center` and `radius_m`; `station` and `drift` need
//! `at`. Drift follows the tidewater current when the contacts process has conditions.
//! `recorded` needs `max_gap_s` and timestamped `samples` (`at_s`, `position = [lon, lat]`,
//! optional `speed_mps` and `course_deg`); no constant speed or route is required. It
//! interpolates only across supported gaps, with one-second bins for isolated/final fixes.
//!
//! A contact may carry a weapon (docs/OPPONENT-WEAPONS.md): `weapon = { name = "ATGM-class
//! missile", kind = "atgm", speed_mps = 250, range_m = 5000 }` (and `fly_m`, its flight height
//! above the sea, default 4; `guidance`, `warhead_kg` and `effects`, its hit and damage figures
//! for the damage model, [`crate::damage`]). Every act by such a contact launches it
//! ([`crate::weapon`]); the acts of any other contact stay abstract. Weapons need the physical sensing model: a file that
//! names the legacy one and a weapon is refused, as is an act against a contact the weapon cannot
//! reach at the act's second.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Affiliation {
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

/// A cooperative identity broadcast on AIS (ships) or, by analogy, a transponder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ais {
    pub mmsi: String,
    pub name: String,
    /// passenger | cargo | fishing | tanker | tug | pleasure | law_enforcement | military | other
    pub ship_type: String,
    /// The broadcast identity is false (truth knows; the C2 has to notice).
    #[serde(default)]
    pub spoofed: bool,
}

/// What a kind of contact is like by default (its radar/EO signature relative to a nominal
/// contact, whether it emits, how fast it turns, its height): its entry in the contact catalog
/// (`sim/contacts.toml`, [`crate::catalog`]), which replaced the `KINDS` table that was here.
pub fn kind_defaults(kind: &str) -> Option<&'static crate::catalog::Kind> {
    crate::catalog::Catalog::shipped().get(kind)
}

/// What an engagement's effect (Phase E) does to a contact inside its radius: a flag and a time,
/// and either out of play (no longer in the truth, so our sensors stop detecting it) or
/// unchanged. An abstract effect (a weapon without hit and damage figures) takes every contact
/// inside the radius out of play unless it `persists`; with figures, only the damage model's
/// `destroyed` does, and `persists` keeps a contact in play whatever the draw ([`crate::damage`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OnEffect {
    #[default]
    OutOfPlay,
    Persists,
}

/// What every strike the damage model makes on a contact does to it when its scenario pins it
/// (`strike_outcome`, a training aid: the tutorial's boat, DECISIONS.md 11-03): a hit with that
/// damage state, or a miss, in place of the hit and damage draws. It pins only a strike aimed at
/// the contact (the effect's target contact); caught in the blast of a strike on another, it
/// draws as any other does ([`crate::effect`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrikeOutcome {
    Destroyed,
    Mobility,
    Firepower,
    Undamaged,
    Miss,
}

impl StrikeOutcome {
    /// The damage state it pins (none: a miss).
    pub fn state(self) -> Option<crate::damage::DamageState> {
        use crate::damage::DamageState;
        match self {
            StrikeOutcome::Destroyed => Some(DamageState::Destroyed),
            StrikeOutcome::Mobility => Some(DamageState::Mobility),
            StrikeOutcome::Firepower => Some(DamageState::Firepower),
            StrikeOutcome::Undamaged => Some(DamageState::Undamaged),
            StrikeOutcome::Miss => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        self.state().map_or("miss", |s| s.as_str())
    }
}

/// The bridge's sensing models (`murmur-bridge` `tracks::truth::SensingModel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensingModel {
    Legacy,
    Physical,
}

/// AIS carriage for a contact's signature override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AisClass {
    A,
    B,
    None,
}

/// `[conditions]`: what the physical sensing model senses in (each optional; the bridge fills
/// the rest with a clear day, sea state 2 and its sonar reference environment).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Conditions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sea_state: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wind_mps: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipping: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub water_depth_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sun_elevation_deg: Option<f64>,
}

/// A contact's changes to its kind's signature in the physical model (the bridge's
/// `tracks::physical::signature::SignatureOverride`).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct SignatureFields {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dc_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rcs_m2: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal_k: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_db: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_db: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ais_class: Option<AisClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emitter_dbm: Option<f64>,
}

impl SignatureFields {
    pub fn is_empty(&self) -> bool {
        *self == SignatureFields::default()
    }
}

/// `[sensing]`, as the truth message carries it to the bridge. `conditions` and `signatures`
/// are filled from the scenario's `[conditions]` and its contacts' signature fields, never
/// written in `[sensing]` itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Sensing {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weather: Option<murmur_weather::Config>,
    /// Probability, per track, that a classifying sensor reports the wrong kind.
    #[serde(default)]
    pub misclassification_rate: f64,
    /// The bridge's sensing model (none: its default, `physical`, unless `MURMUR_SENSE_MODEL`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<SensingModel>,
    /// The physical model's terrain and bathymetry grids (`sim/headless/terrain/*.grid.gz`
    /// format), paths relative to the repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terrain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bathymetry: Option<String>,
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Conditions>,
    #[serde(default, skip_deserializing, skip_serializing_if = "BTreeMap::is_empty")]
    pub signatures: BTreeMap<String, SignatureFields>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Scenario {
    pub title: String,
    #[serde(default)]
    pub seed: u64,
    #[serde(default)]
    pub sensing: Sensing,
    /// The physical sensing model's conditions (into `sensing.conditions` at load).
    #[serde(default)]
    pub conditions: Option<Conditions>,
    #[serde(default, rename = "contact")]
    pub contacts: Vec<ContactSpec>,
    /// Rules 9: scripted hostile acts (`[[act]]`), truth only.
    #[serde(default, rename = "act")]
    pub acts: Vec<ActSpec>,
}

/// Rules 9: a hostile act a contact commits at a scenario second, against one of our vehicles
/// (`against_vehicle`, its fleet uid) or another contact (`against_contact`). By a contact
/// without a weapon it is abstract: a time, an author and a victim. By one with a weapon it
/// launches it (docs/OPPONENT-WEAPONS.md, [`crate::weapon`]). It is truth:
/// the C2 learns of it only through its sensors (the bridge's sensing model decides which of
/// ours observe it, and where they place its author).
///
/// ```toml
/// [[act]]
/// id = "act-1"
/// at_s = 90
/// by = "fast-boat"            # a contact id
/// against_vehicle = 3         # or against_contact = "ferry-sausalito"
/// label = "fires on the patrol USV"   # truth-side note, never seen by the C2
/// ```
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActSpec {
    pub id: String,
    pub at_s: f64,
    pub by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub against_vehicle: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub against_contact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// The weapon kinds the bridge's sensing model has figures for (its `tracks::physical::weapon`).
pub const WEAPON_KINDS: &[&str] = &["atgm"];

/// A contact's weapon (docs/OPPONENT-WEAPONS.md): a straight flight at constant speed from the
/// shooter to a lead point on its victim, no guidance. Rounded public figures, "-class" naming
/// as the catalog's munitions. With `effects` (the catalog's shape: `{ <target kind> = { p_hit,
/// p_kill, p_mobility, p_firepower } }`, our vehicles' kinds by `damage::ours_kind`), its
/// `guidance` and `warhead_kg`, each strike is drawn by the damage model (bd murmur-lbw8.2): a
/// miss, or damage to the victim; without them a strike changes nothing about its victim.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WeaponSpec {
    pub name: String,
    pub kind: String,
    pub speed_mps: f64,
    pub range_m: f64,
    /// Its flight height above the sea, m.
    #[serde(default = "default_fly_m")]
    pub fly_m: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warhead_kg: Option<f64>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub effects: std::collections::BTreeMap<String, crate::damage::Figures>,
}

fn default_fly_m() -> f64 {
    4.0
}

impl WeaponSpec {
    fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("weapon needs a name".into());
        }
        if !WEAPON_KINDS.contains(&self.kind.as_str()) {
            return Err(format!("weapon kind {:?} is none the sensing model knows ({})", self.kind, WEAPON_KINDS.join(", ")));
        }
        if !(self.speed_mps.is_finite() && self.speed_mps > 0.0) {
            return Err("weapon speed_mps must be above 0".into());
        }
        if !(self.range_m.is_finite() && self.range_m > 0.0) {
            return Err("weapon range_m must be above 0".into());
        }
        if !(self.fly_m.is_finite() && (0.0..=100.0).contains(&self.fly_m)) {
            return Err("weapon fly_m must be within 0..100".into());
        }
        if self.warhead_kg.is_some_and(|k| !(k.is_finite() && k >= 0.0)) {
            return Err("weapon warhead_kg must be 0 or more".into());
        }
        self.damage().map_or(Ok(()), |d| d.check())
    }

    /// The weapon as the damage model reads it, when it lists figures (none: a strike does no
    /// damage).
    pub fn damage(&self) -> Option<crate::damage::WeaponDamage> {
        (!self.effects.is_empty()).then(|| crate::damage::WeaponDamage {
            name: self.name.clone(),
            kind: self.kind.clone(),
            guidance: self.guidance.clone().unwrap_or_default(),
            warhead: self.warhead_kg.map(|mass_kg| crate::damage::Warhead { class: String::new(), mass_kg }),
            max_range_m: Some(self.range_m),
            effects: self.effects.clone(),
        })
    }
}

/// One contact as written in the file (flat fields; [`Motion`] is derived and validated).
#[derive(Debug, Clone, Deserialize)]
pub struct ContactSpec {
    #[serde(default, rename = "maneuver")]
    pub maneuvers: Vec<Maneuver>,
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub affiliation: Affiliation,
    pub domain: Domain,
    pub kind: String,
    #[serde(default)]
    pub ais: Option<Ais>,
    #[serde(default)]
    pub emits: Option<bool>,
    #[serde(default)]
    pub signature: Option<f64>,
    #[serde(default)]
    pub speed_mps: f64,
    #[serde(default)]
    pub alt_m: Option<f64>,
    #[serde(default)]
    pub turn_dps: Option<f64>,
    pub motion: String,
    #[serde(default)]
    pub route: Vec<[f64; 2]>,
    /// Timestamped observations for `recorded` motion, in scenario seconds.
    #[serde(default)]
    pub samples: Vec<RecordedPoint>,
    #[serde(default)]
    pub max_gap_s: Option<f64>,
    #[serde(default)]
    pub reverse: bool,
    #[serde(default)]
    pub dwell_s: f64,
    #[serde(default)]
    pub center: Option<[f64; 2]>,
    #[serde(default)]
    pub radius_m: Option<f64>,
    #[serde(default)]
    pub at: Option<[f64; 2]>,
    #[serde(default)]
    pub start_s: f64,
    #[serde(default)]
    pub end_s: Option<f64>,
    #[serde(default)]
    pub on_effect: OnEffect,
    /// Pins what every strike aimed at it does ([`StrikeOutcome`]; none: drawn).
    #[serde(default)]
    pub strike_outcome: Option<StrikeOutcome>,
    /// Its weapon: every act it commits launches it (docs/OPPONENT-WEAPONS.md).
    #[serde(default)]
    pub weapon: Option<WeaponSpec>,
    /// The physical sensing model's per-contact signature (`dc_m`, `rcs_m2`, ...).
    #[serde(flatten)]
    pub physical: SignatureFields,
}

/// An observed fix. Missing speed/course are inferred from a supported segment, or zero
/// for an isolated fix; they are never filled with an AIS unavailable-value sentinel.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedPoint {
    pub at_s: f64,
    pub position: [f64; 2],
    pub speed_mps: Option<f64>,
    pub course_deg: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Motion {
    /// Interpolate only between fixes separated by at most `max_gap_s`. An isolated or
    /// final fix occupies its one-second report bin; no extrapolation or looping.
    Recorded { samples: Vec<RecordedPoint>, max_gap_s: f64 },
    /// Waypoints `[lon, lat]`; back to the first point, or back and forth with `reverse`.
    Route {
        points: Vec<[f64; 2]>,
        reverse: bool,
        dwell_s: f64,
    },
    /// Once along the points, then hold at the last one.
    Transit { points: Vec<[f64; 2]> },
    Loiter { center: [f64; 2], radius_m: f64 },
    Station { at: [f64; 2] },
    /// With the current (tidewater) plus a little jitter; still water without conditions.
    Drift { at: [f64; 2] },
}

/// At a scenario second, steer from the current position to these waypoints at a new speed.
/// This is simulation truth; it is never an order or a reported sensor observation.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Maneuver {
    pub at_s: f64,
    pub speed_mps: f64,
    pub route: Vec<[f64; 2]>,
}

/// A validated contact with every default filled in.
#[derive(Debug, Clone)]
pub struct Contact {
    pub maneuvers: Vec<Maneuver>,
    pub id: String,
    pub name: String,
    pub affiliation: Affiliation,
    pub domain: Domain,
    /// The kind our sensors see it as: its catalog kind's `looks_like`, or the kind itself.
    pub kind: String,
    /// The catalog kind its file names, when it passes for another (`kind` is then the
    /// look-alike; decision D3): truth only.
    pub variant: Option<String>,
    pub ais: Option<Ais>,
    pub emits: bool,
    pub signature: f64,
    pub speed_mps: f64,
    pub alt_m: f64,
    pub turn_dps: f64,
    pub motion: Motion,
    pub start_s: f64,
    pub end_s: Option<f64>,
    pub on_effect: OnEffect,
    pub strike_outcome: Option<StrikeOutcome>,
    pub weapon: Option<WeaponSpec>,
}

impl ContactSpec {
    pub fn validate(&self) -> Result<Contact, String> {
        let bad = |m: String| format!("contact {:?}: {m}", self.id);
        if self.id.trim().is_empty() {
            return Err("a contact has an empty id".into());
        }
        let valid = |p: &[f64; 2]| p[0].abs() <= 180.0 && p[1].abs() <= 90.0;
        let points = |min: usize| -> Result<Vec<[f64; 2]>, String> {
            if self.route.len() < min {
                return Err(bad(format!(
                    "motion {:?} needs at least {min} route points",
                    self.motion
                )));
            }
            if let Some(p) = self.route.iter().find(|p| !valid(p)) {
                return Err(bad(format!("route point {p:?} is not [lon, lat]")));
            }
            Ok(self.route.clone())
        };
        let point = |p: Option<[f64; 2]>, name: &str| -> Result<[f64; 2], String> {
            let p = p.ok_or_else(|| bad(format!("motion {:?} needs `{name}`", self.motion)))?;
            if !valid(&p) {
                return Err(bad(format!("{name} {p:?} is not [lon, lat]")));
            }
            Ok(p)
        };
        let motion = match self.motion.as_str() {
            "recorded" => {
                let gap = self.max_gap_s.filter(|g| g.is_finite() && *g >= 1.0)
                    .ok_or_else(|| bad("recorded needs finite max_gap_s >= 1".into()))?;
                if self.samples.is_empty() || self.samples.iter().any(|p| {
                    !p.at_s.is_finite() || p.at_s < 0.0 || !valid(&p.position)
                        || p.speed_mps.is_some_and(|v| !v.is_finite() || v < 0.0)
                        || p.course_deg.is_some_and(|v| !(0.0..360.0).contains(&v))
                }) || self.samples.windows(2).any(|w| w[0].at_s >= w[1].at_s) {
                    return Err(bad("recorded needs valid fixes in strictly increasing timestamp order".into()));
                }
                Motion::Recorded { samples: self.samples.clone(), max_gap_s: gap }
            }
            "route" => Motion::Route {
                points: points(2)?,
                reverse: self.reverse,
                dwell_s: self.dwell_s.max(0.0),
            },
            "transit" => Motion::Transit { points: points(2)? },
            "loiter" => Motion::Loiter {
                center: point(self.center, "center")?,
                radius_m: self
                    .radius_m
                    .filter(|r| *r > 0.0)
                    .ok_or_else(|| bad("loiter needs radius_m > 0".into()))?,
            },
            "station" => Motion::Station {
                at: point(self.at, "at")?,
            },
            "drift" => Motion::Drift {
                at: point(self.at, "at")?,
            },
            other => {
                return Err(bad(format!(
                    "unknown motion {other:?} (recorded, route, transit, loiter, station, drift)"
                )));
            }
        };
        let moving = !matches!(motion, Motion::Recorded { .. } | Motion::Station { .. } | Motion::Drift { .. });
        if moving && (self.speed_mps <= 0.0 || !self.speed_mps.is_finite()) {
            return Err(bad(format!("motion {:?} needs speed_mps > 0", self.motion)));
        }
        if self.ais.is_some() && self.domain != Domain::Surface {
            return Err(bad("ais is for surface contacts".into()));
        }
        let d = kind_defaults(&self.kind);
        if let Some(d) = d
            && d.domain != self.domain
        {
            return Err(bad(format!(
                "kind {:?} is a {:?} contact, not {:?}",
                self.kind, d.domain, self.domain
            )));
        }
        if let Some(w) = &self.weapon {
            w.validate().map_err(bad)?;
        }
        match self.strike_outcome {
            Some(o) if self.on_effect == OnEffect::Persists => {
                return Err(bad(format!("strike_outcome {:?} contradicts on_effect \"persists\"", o.as_str())));
            }
            Some(StrikeOutcome::Firepower) if self.weapon.is_none() => {
                return Err(bad("strike_outcome \"firepower\" needs a weapon to put out of action".into()));
            }
            _ => {}
        }
        let default_alt = match self.domain {
            Domain::Air => 300.0,
            Domain::Subsurface => -30.0,
            Domain::Surface | Domain::Ground => 0.0,
        };
        let mut last = -1.0;
        for m in &self.maneuvers {
            if !m.at_s.is_finite()
                || m.at_s < self.start_s
                || m.at_s <= last
                || !m.speed_mps.is_finite()
                || m.speed_mps <= 0.0
                || m.route.is_empty()
                || m.route.iter().any(|p| !valid(p))
            {
                return Err(bad(
                    "maneuver needs increasing at_s, positive speed_mps and valid route points"
                        .into(),
                ));
            }
            last = m.at_s;
        }
        Ok(Contact {
            maneuvers: self.maneuvers.clone(),
            id: self.id.clone(),
            name: self.name.clone().unwrap_or_else(|| self.id.clone()),
            affiliation: self.affiliation,
            domain: self.domain,
            kind: d.and_then(|d| d.looks_like.clone()).unwrap_or_else(|| self.kind.clone()),
            variant: d.and_then(|d| d.looks_like.as_ref()).map(|_| self.kind.clone()),
            ais: self.ais.clone(),
            emits: self.emits.unwrap_or(d.is_some_and(|d| d.emits)),
            signature: self
                .signature
                .filter(|s| *s > 0.0)
                .unwrap_or(d.map_or(1.0, |d| d.signature)),
            speed_mps: self.speed_mps.max(0.0),
            alt_m: self.alt_m.unwrap_or(d.map_or(default_alt, |d| d.alt_m)),
            turn_dps: self
                .turn_dps
                .filter(|t| *t > 0.0)
                .unwrap_or(d.map_or(5.0, |d| d.turn_dps)),
            motion,
            start_s: self.start_s.max(0.0),
            end_s: self.end_s,
            on_effect: self.on_effect,
            strike_outcome: self.strike_outcome,
            weapon: self.weapon.clone(),
        })
    }
}

impl Scenario {
    pub fn from_toml(text: &str) -> Result<Scenario, String> {
        let mut s: Scenario = toml::from_str(text).map_err(|e| e.to_string())?;
        if !(0.0..=1.0).contains(&s.sensing.misclassification_rate) {
            return Err("sensing.misclassification_rate must be within 0..1".into());
        }
        if let Some(w) = &s.sensing.weather { w.validate()?; }
        // the physical model's conditions and signatures ride in the sensing section
        s.sensing.conditions = s.conditions;
        s.sensing.signatures = s
            .contacts
            .iter()
            .filter(|c| !c.physical.is_empty())
            .map(|c| (c.id.clone(), c.physical))
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        for c in &s.contacts {
            if !seen.insert(c.id.as_str()) {
                return Err(format!("duplicate contact id {:?}", c.id));
            }
        }
        let mut acts = std::collections::BTreeSet::new();
        for a in &s.acts {
            if a.id.trim().is_empty() || !acts.insert(a.id.as_str()) {
                return Err(format!("act {:?}: an empty or duplicate id", a.id));
            }
            if !seen.contains(a.by.as_str()) {
                return Err(format!("act {:?}: by {:?} is no contact of the scenario", a.id, a.by));
            }
            if a.against_vehicle.is_some() == a.against_contact.is_some() {
                return Err(format!("act {:?}: name one of against_vehicle and against_contact", a.id));
            }
            if let Some(c) = &a.against_contact
                && (!seen.contains(c.as_str()) || *c == a.by)
            {
                return Err(format!("act {:?}: against_contact {c:?} is no other contact of the scenario", a.id));
            }
            if !(a.at_s.is_finite() && a.at_s >= 0.0) {
                return Err(format!("act {:?}: at_s must be a scenario second", a.id));
            }
        }
        if s.contacts.iter().any(|c| c.weapon.is_some()) {
            if s.sensing.model == Some(SensingModel::Legacy) {
                return Err("weapons need the physical sensing model: this file names the legacy one".into());
            }
            s.weapon_ranges()?;
        }
        Ok(s)
    }

    /// Every act by an armed contact against another contact is within its weapon's range at
    /// the act's second (both positions are truth, the motion stepped at 1 Hz without a current
    /// as the contacts process steps it). An act against one of our vehicles is checked at its
    /// second instead (where the vehicle is is known only then: [`crate::weapon`]).
    pub fn weapon_ranges(&self) -> Result<(), String> {
        let armed: Vec<&ActSpec> = self
            .acts
            .iter()
            .filter(|a| a.against_contact.is_some() && self.contacts.iter().any(|c| c.id == a.by && c.weapon.is_some()))
            .collect();
        if armed.is_empty() {
            return Ok(());
        }
        let mut movers: Vec<crate::motion::Mover> = self
            .contacts()?
            .into_iter()
            .enumerate()
            .map(|(i, c)| crate::motion::Mover::new(c, self.seed, i as u64))
            .collect();
        let mut armed = armed;
        armed.sort_by(|a, b| a.at_s.total_cmp(&b.at_s));
        let mut s = 0.0;
        let mut prev = 0.0;
        for a in armed {
            while s < a.at_s {
                s = (s + 1.0).min(a.at_s);
                for m in &mut movers {
                    m.step(s, s - prev, None);
                }
                prev = s;
            }
            let at = |id: &str| movers.iter().find(|m| m.contact.id == id).map(|m| (m.st.lat, m.st.lon));
            let (Some(p), Some(q)) = (at(&a.by), a.against_contact.as_deref().and_then(at)) else { continue };
            let w = self.contacts.iter().find(|c| c.id == a.by).and_then(|c| c.weapon.as_ref()).expect("armed");
            let d = murmur_standins::geo::distance_m(p.0, p.1, q.0, q.1);
            if d > w.range_m {
                return Err(format!(
                    "act {:?}: its victim is {:.1} km from {:?} at {} s, beyond its {}'s {:.1} km",
                    a.id,
                    d / 1000.0,
                    a.by,
                    a.at_s,
                    w.name,
                    w.range_m / 1000.0
                ));
            }
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Scenario, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Scenario::from_toml(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Every contact validated, in file order.
    pub fn contacts(&self) -> Result<Vec<Contact>, String> {
        self.contacts.iter().map(ContactSpec::validate).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_motion_refuses_unusable_samples() {
        let parse = |samples: &str, gap: &str| {
            Scenario::from_toml(&format!(r#"
                title = "recording"
                [[contact]]
                id = "c"
                affiliation = "neutral"
                domain = "surface"
                kind = "ferry"
                motion = "recorded"
                max_gap_s = {gap}
                samples = {samples}
            "#)).and_then(|s| s.contacts())
        };
        let fix = "{at_s=0, position=[0,0]}";
        assert!(parse(&format!("[{fix}]"), "300").is_ok(), "single reports and missing velocity are valid");
        for bad in ["[]", "[{at_s=-1, position=[0,0]}]", "[{at_s=nan, position=[0,0]}]",
                    "[{at_s=0, position=[181,0]}]", "[{at_s=0, position=[0,nan]}]",
                    "[{at_s=0, position=[0,0], speed_mps=-1}]",
                    "[{at_s=0, position=[0,0], course_deg=360}]",
                    "[{at_s=0, position=[0,0], course_deg=nan}]",
                    "[{at_s=1, position=[0,0]}, {at_s=0, position=[0,0]}]"] {
            assert!(parse(bad, "300").is_err(), "{bad}");
        }
        assert!(parse(&format!("[{fix}, {fix}]"), "300").is_err());
        for gap in ["0", "-1", "nan", "inf"] {
            assert!(parse(&format!("[{fix}]"), gap).is_err());
        }
    }

    #[test]
    fn parses_and_fills_defaults() {
        let s = Scenario::from_toml(
            r#"
            title = "t"
            seed = 3
            [sensing]
            misclassification_rate = 0.1
            [[contact]]
            id = "f1"
            affiliation = "neutral"
            domain = "surface"
            kind = "ferry"
            speed_mps = 11
            motion = "route"
            route = [[-122.39, 37.79], [-122.47, 37.85]]
            reverse = true
            dwell_s = 60
            ais = { mmsi = "1", name = "MV X", ship_type = "passenger" }
            [[contact]]
            id = "u1"
            affiliation = "unknown"
            domain = "air"
            kind = "unknown_uas"
            speed_mps = 15
            motion = "loiter"
            center = [-122.48, 37.83]
            radius_m = 400
            "#,
        )
        .expect("parses");
        assert_eq!(s.seed, 3);
        assert_eq!(s.sensing.misclassification_rate, 0.1);
        let cs = s.contacts().expect("valid");
        assert_eq!(cs.len(), 2);
        let f = &cs[0];
        assert_eq!(f.name, "f1");
        assert!(f.emits);
        assert_eq!(f.signature, 1.2);
        assert_eq!(f.turn_dps, 3.0);
        assert!(matches!(&f.motion, Motion::Route { reverse: true, dwell_s, .. } if *dwell_s == 60.0));
        let u = &cs[1];
        assert!(!u.emits);
        assert_eq!(u.alt_m, 120.0);
        assert_eq!(u.signature, 0.15);
    }

    #[test]
    fn refuses_bad_contacts() {
        let base = |body: &str| {
            Scenario::from_toml(&format!(
                "title = \"t\"\n[[contact]]\nid = \"c\"\naffiliation = \"neutral\"\n{body}"
            ))
            .and_then(|s| s.contacts())
        };
        assert!(base("domain = \"surface\"\nkind = \"ferry\"\nmotion = \"route\"\nroute = [[0,0]]\nspeed_mps = 1").is_err());
        assert!(base("domain = \"surface\"\nkind = \"ferry\"\nmotion = \"loiter\"\ncenter = [0,0]\nspeed_mps = 1").is_err());
        assert!(base("domain = \"surface\"\nkind = \"ferry\"\nmotion = \"route\"\nroute = [[0,0],[1,1]]").is_err());
        assert!(base("domain = \"air\"\nkind = \"ferry\"\nmotion = \"station\"\nat = [0,0]").is_err());
        assert!(base("domain = \"air\"\nkind = \"light_aircraft\"\nmotion = \"hover\"\nat = [0,0]").is_err());
        assert!(base("domain = \"surface\"\nkind = \"fishing_vessel\"\nmotion = \"drift\"\nat = [0,0]").is_ok());
        assert!(
            Scenario::from_toml("title = \"t\"\n[[contact]]\nid = \"a\"\naffiliation = \"neutral\"\ndomain = \"surface\"\nkind = \"ferry\"\nmotion = \"station\"\nat=[0,0]\n[[contact]]\nid = \"a\"\naffiliation = \"neutral\"\ndomain = \"surface\"\nkind = \"ferry\"\nmotion = \"station\"\nat=[0,0]\n").is_err()
        );
    }

    /// The physical sensing model's settings ride in the sensing section: the model, the
    /// grids, the conditions, each contact's signature fields by id; a legacy scenario's
    /// section serializes as before (the misclassification rate alone).
    #[test]
    fn the_physical_models_settings_ride_in_the_sensing_section() {
        let s = Scenario::from_toml(
            r#"
            title = "t"
            [sensing]
            misclassification_rate = 0.02
            model = "physical"
            terrain = "sim/headless/terrain/golden-gate.grid.gz"
            [conditions]
            visibility_m = 4000
            sea_state = 4
            [[contact]]
            id = "dark"
            affiliation = "unknown"
            domain = "surface"
            kind = "small_fast_boat"
            motion = "station"
            at = [-122.5, 37.8]
            rcs_m2 = 0.8
            ais_class = "none"
            [[contact]]
            id = "diver"
            affiliation = "unknown"
            domain = "subsurface"
            kind = "diver"
            motion = "station"
            at = [-122.49, 37.8]
            "#,
        )
        .expect("parses");
        assert_eq!(s.sensing.model, Some(SensingModel::Physical));
        assert_eq!(s.sensing.conditions.and_then(|c| c.sea_state), Some(4.0));
        assert_eq!(s.sensing.signatures.len(), 1);
        assert_eq!(s.sensing.signatures["dark"].rcs_m2, Some(0.8));
        assert_eq!(s.sensing.signatures["dark"].ais_class, Some(AisClass::None));
        let cs = s.contacts().expect("valid");
        assert_eq!(cs[1].alt_m, -5.0);
        let json = serde_json::to_value(&s.sensing).expect("json");
        assert_eq!(json["signatures"]["dark"]["rcs_m2"], 0.8);
        assert_eq!(json["conditions"]["visibility_m"], 4000.0);
        let legacy = Scenario::from_toml("title = \"t\"\n[sensing]\nmisclassification_rate = 0.05\n").expect("parses");
        assert_eq!(serde_json::to_string(&legacy.sensing).expect("json"), r#"{"misclassification_rate":0.05}"#);
        assert!(Scenario::from_toml("title = \"t\"\n[sensing]\nmodel = \"optical\"\n").is_err());
        assert!(Scenario::from_toml("title = \"t\"\n[sensing]\nsignatures = {}\n").is_err());
    }

    /// Every contacts scenario that ships loads, its contacts and acts valid.
    #[test]
    fn the_shipped_scenarios_load() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sim/scenarios");
        let mut n = 0;
        for e in std::fs::read_dir(&dir).expect("sim/scenarios") {
            let p = e.expect("entry").path();
            if p.extension().is_some_and(|x| x == "toml") {
                let s = Scenario::load(&p).expect("loads");
                s.contacts().unwrap_or_else(|e| panic!("{}: {e}", p.display()));
                n += 1;
            }
        }
        assert!(n >= 4, "{n} scenarios");
        let ident = Scenario::load(&dir.join("golden-gate-ident.toml")).expect("loads");
        assert_eq!(ident.acts.len(), 3);
    }

    /// Rules 9: scripted acts name an author and exactly one victim among the scenario's
    /// contacts or our vehicles; anything else fails to load.
    #[test]
    fn acts_name_an_author_and_one_victim() {
        let two = "title = \"t\"\n\
            [[contact]]\nid = \"boat\"\naffiliation = \"hostile\"\ndomain = \"surface\"\nkind = \"small_fast_boat\"\nmotion = \"station\"\nat = [-122.5, 37.8]\n\
            [[contact]]\nid = \"ferry\"\naffiliation = \"neutral\"\ndomain = \"surface\"\nkind = \"ferry\"\nmotion = \"station\"\nat = [-122.49, 37.8]\n";
        let with = |act: &str| Scenario::from_toml(&format!("{two}[[act]]\n{act}"));
        let s = with("id = \"a1\"\nat_s = 30\nby = \"boat\"\nagainst_vehicle = 3\nlabel = \"fires on the USV\"").expect("parses");
        assert_eq!(s.acts.len(), 1);
        assert_eq!(s.acts[0].against_vehicle, Some(3));
        assert!(with("id = \"a1\"\nat_s = 30\nby = \"boat\"\nagainst_contact = \"ferry\"").is_ok());
        for bad in [
            "id = \"a1\"\nat_s = 30\nby = \"nobody\"\nagainst_vehicle = 3",
            "id = \"a1\"\nat_s = 30\nby = \"boat\"",
            "id = \"a1\"\nat_s = 30\nby = \"boat\"\nagainst_vehicle = 3\nagainst_contact = \"ferry\"",
            "id = \"a1\"\nat_s = 30\nby = \"boat\"\nagainst_contact = \"boat\"",
            "id = \"a1\"\nat_s = -1\nby = \"boat\"\nagainst_vehicle = 3",
            "id = \"\"\nat_s = 30\nby = \"boat\"\nagainst_vehicle = 3",
            "id = \"a1\"\nat_s = 30\nby = \"boat\"\nagainst_vehicle = 3\nweapon = \"gun\"",
        ] {
            assert!(with(bad).is_err(), "{bad}");
        }
        let dup = format!("{two}[[act]]\nid = \"a\"\nat_s = 1\nby = \"boat\"\nagainst_vehicle = 1\n[[act]]\nid = \"a\"\nat_s = 2\nby = \"boat\"\nagainst_vehicle = 1\n");
        assert!(Scenario::from_toml(&dup).is_err());
    }

    /// `strike_outcome` (a training aid): one of the five outcomes, never beside `persists`, and
    /// `firepower` only on an armed contact; absent, none.
    #[test]
    fn a_pinned_strike_outcome_is_checked_at_load() {
        let boat = |extra: &str| {
            Scenario::from_toml(&format!(
                "title = \"t\"\n[[contact]]\nid = \"boat\"\naffiliation = \"hostile\"\ndomain = \"surface\"\nkind = \"small_fast_boat\"\nmotion = \"station\"\nat = [-122.5, 37.8]\n{extra}"
            ))
            .and_then(|s| s.contacts())
        };
        assert_eq!(boat("").expect("valid")[0].strike_outcome, None);
        for (o, want) in [
            ("destroyed", StrikeOutcome::Destroyed),
            ("mobility", StrikeOutcome::Mobility),
            ("undamaged", StrikeOutcome::Undamaged),
            ("miss", StrikeOutcome::Miss),
        ] {
            let c = boat(&format!("strike_outcome = \"{o}\"\n")).expect("valid");
            assert_eq!((c[0].strike_outcome, want.as_str()), (Some(want), o));
        }
        let armed = "weapon = { name = \"ATGM-class missile\", kind = \"atgm\", speed_mps = 250, range_m = 5000 }\n";
        assert!(boat(&format!("strike_outcome = \"firepower\"\n{armed}")).is_ok());
        for bad in ["strike_outcome = \"firepower\"\n", "strike_outcome = \"sunk\"\n", "strike_outcome = true\n", "strike_outcome = \"destroyed\"\non_effect = \"persists\"\n"] {
            assert!(boat(bad).is_err(), "{bad}");
        }
    }
}
