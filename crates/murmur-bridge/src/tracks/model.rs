//! A track: what the bridge believes about one contact, built only from detections
//! (`sensing.rs`). Staleness is first class: every reader gets `last_seen_t`, and the
//! position uncertainty grows with age at `sigma_growth_mps`, so a track that nobody has
//! seen for a minute is visibly loose and an engagement check (Phase E) can refuse it.
//!
//! Estimation: a constant-velocity Kalman filter per horizontal axis (position and velocity,
//! white-noise acceleration by domain, [`process_noise`]), every detection a position
//! measurement with its sensor's 1-sigma and an AIS report's course and speed a velocity
//! measurement, so the 1-sigma it reports is the filter's own and honest against the truth
//! (a test checks it in the simulation: `tracks::tests::the_trackers_uncertainty_covers_
//! the_truth`; until rules 9's review it was a scalar update with a velocity smoothed from
//! fixes, too loose holding and too tight after a manoeuvre). Existence confidence climbs
//! with hits and decays in silence; identity confidence grows with dwell per sensor type and
//! combines across independent sensors. Several observers' fixes of one tick are folded one
//! after another, as independent measurements (they are: each sensor's draw is its own). Association is by the simulation's contact id (see `mod.rs`); a dropped track
//! that is seen again gets a new id, as a real tracker would start a new track.

use std::collections::BTreeMap;

use murmur_decide::{Affiliation, Fact, LonLat, Time, TrackFacts};
use serde::Serialize;
use serde_json::{Value, json};

use super::sensing::{AisHeard, Detection, SensorType, family};
use super::truth::Domain;
use crate::clock::round;

/// Existence confidence after one detection: `c += (1 - c) * HIT_GAIN`.
const HIT_GAIN: f64 = 0.25;
/// Existence confidence halves about every 40 s without a detection.
const CONF_DECAY_S: f64 = 60.0;
/// A track is `stale` after this long without a detection (also the console's fade).
pub const STALE_S: f64 = 10.0;
/// AIS identity confidence (cooperative, spoofable).
const AIS_CONF: f64 = 0.85;
/// Dwell credited per detection is capped at this (a 1 Hz sensor gives 1 s a hit).
const MAX_DWELL_PER_HIT_S: f64 = 2.0;
/// A velocity estimate is shown (and given to decisions) once its 1-sigma is below this.
const VEL_KNOWN_SIGMA_MPS: f64 = 3.0;
/// An AIS report's course and speed over ground, 1-sigma per axis, m/s.
const AIS_VEL_SIGMA_MPS: f64 = 0.3;

/// The filter's white-noise acceleration by domain (its spectral density, m^2/s^3): what the
/// contact may do between fixes that the constant-velocity model does not know. Set so the
/// reported 1-sigma covers the truth about as often as a Gaussian says, holding, in transit,
/// turning and circling, in the simulation's motion (see the module doc).
pub fn process_noise(domain: Domain) -> f64 {
    match domain {
        Domain::Air => Q_AIR,
        Domain::Surface => Q_SURFACE,
        Domain::Subsurface => Q_SUBSURFACE,
        Domain::Ground => Q_GROUND,
    }
}

const Q_AIR: f64 = 2.0;
const Q_SURFACE: f64 = 0.05;
const Q_SUBSURFACE: f64 = 0.01;
const Q_GROUND: f64 = 0.3;

/// A fix this far from the prediction (its innovation's normalized square, chi-square with 2
/// degrees of freedom, 99%) widens the prior by the innovation's own size before the update:
/// the contact did something the constant-velocity model did not expect (a stop, a sharp
/// turn), and the fix outranks the prediction.
const NIS_JUMP: f64 = 9.21;

/// A new track's velocity is unknown within this, 1-sigma per axis (about half the domain's
/// top speed).
fn initial_vel_sigma(domain: Domain) -> f64 {
    match domain {
        Domain::Air => 60.0,
        Domain::Surface => 10.0,
        Domain::Subsurface => 5.0,
        Domain::Ground => 15.0,
    }
}

/// The filter's covariance for one horizontal axis (the same for both: they are updated
/// alike): position variance, position-velocity covariance, velocity variance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AxisCov {
    pub pp: f64,
    pub pv: f64,
    pub vv: f64,
}

impl AxisCov {
    /// Carried `dt` seconds forward with white-noise acceleration `q`.
    pub fn predict(self, dt: f64, q: f64) -> AxisCov {
        AxisCov {
            pp: self.pp + 2.0 * dt * self.pv + dt * dt * self.vv + q * dt.powi(3) / 3.0,
            pv: self.pv + dt * self.vv + q * dt * dt / 2.0,
            vv: self.vv + q * dt,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Source {
    pub uid: u64,
    pub sensor: SensorType,
    pub hits: u64,
    pub last_t: f64,
}

#[derive(Debug, Clone, PartialEq)]
struct Classifier {
    label: String,
    dwell_s: f64,
    last_t: f64,
}

/// A declaration in force, as the decision engine's identification ledger holds it (rules 9:
/// the bridge mirrors the ledger here for display; decisions read the ledger itself).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Declared {
    pub affiliation: Affiliation,
    /// An operator, or `rule:hostile_act`.
    pub by: String,
    pub t: f64,
    pub note: Option<String>,
    /// Its number in the run (`D-<id>`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    /// Declared by the engine's hostile-act rule (not an operator).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub rule: bool,
    /// The rule's evidence: the act, whom it was against, which of our sensors saw it, when,
    /// and the attribution against the threshold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<murmur_decide::RuleEvidence>,
    /// The operator who confirmed a rule declaration (none: not confirmed yet).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmed_by: Option<String>,
    /// The basis in a sentence, as the engagement checks and signers read it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub basis: Option<String>,
}

impl Declared {
    /// The ledger's declaration, for display.
    pub fn from_ledger(d: &murmur_decide::Declaration) -> Declared {
        let (note, evidence) = match &d.by {
            murmur_decide::Declarer::Operator { note, .. } => (note.clone(), None),
            murmur_decide::Declarer::Rule { evidence, .. } => (None, Some(evidence.clone())),
        };
        Declared {
            affiliation: d.affiliation,
            by: d.label(),
            t: d.t.0 as f64 / 1000.0,
            note,
            id: Some(d.id),
            rule: d.is_rule(),
            evidence,
            confirmed_by: d.confirmed.as_ref().map(|c| c.by.to_string()),
            basis: Some(d.describe()),
        }
    }
}

/// Rules 9: a review flag in force on a track (what the engine is unsure of), for display.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewMark {
    pub flag: u64,
    pub kind: String,
    pub status: String,
    pub t: f64,
    pub evidence: String,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub id: String,
    pub no: u64,
    pub domain: Domain,
    pub lat: f64,
    pub lon: f64,
    pub alt_m: Option<f64>,
    /// when a detection last carried an altitude (wall s): a sensor that ranges the contact
    /// in height measured it then (rules 6 count it for a 3D keep-out only while recent)
    pub alt_t: Option<f64>,
    /// east, north m/s, once known (its 1-sigma below `VEL_KNOWN_SIGMA_MPS`)
    pub vel: Option<(f64, f64)>,
    /// the filter's velocity estimate, known or not
    vel_est: (f64, f64),
    /// the filter's covariance at `last_seen_t` (per axis)
    pub cov: AxisCov,

    /// 1 sigma at `last_seen_t` (per axis): `sqrt(cov.pp)`
    pub sigma_m: f64,
    pub confidence: f64,
    pub first_seen_t: f64,
    pub last_seen_t: f64,
    pub hits: u64,
    pub sources: BTreeMap<(u64, SensorType), Source>,
    classifiers: BTreeMap<SensorType, Classifier>,
    pub ais: Option<AisHeard>,
    ais_t: Option<f64>,
    pub declared: Option<Declared>,
    /// Rules 9: the review flags in force on it, and the one that raised it to suspect (its
    /// evidence), as the engine's ledger has them (display only).
    pub review: Vec<ReviewMark>,
    pub raised: Option<String>,
}

/// Speed the position uncertainty grows at while unobserved: at least walking pace, or a
/// third of the estimated speed (course changes are the unknown).
pub fn sigma_growth_mps(speed: f64) -> f64 {
    (0.3 * speed).max(1.0)
}

fn family_of(label: &str) -> &'static str {
    family(label)
}

/// AIS ship type to the kind a classifier would use.
fn ais_kind(ship_type: &str) -> &'static str {
    match ship_type {
        "passenger" => "ferry",
        "cargo" => "cargo_ship",
        "tanker" => "tanker",
        "fishing" => "fishing_vessel",
        "pleasure" | "sailing" => "sailboat",
        "law_enforcement" | "military" | "sar" => "patrol_boat",
        "tug" => "tug",
        _ => "vessel",
    }
}

/// Top speed a kind would plausibly make, m/s (a "fishing vessel" at 29 kt is not one).
fn plausible_speed_mps(kind: &str) -> f64 {
    match kind {
        "fishing_vessel" => 8.0,
        "sailboat" => 6.0,
        "cargo_ship" | "tanker" => 13.0,
        "ferry" => 20.0,
        "tug" => 8.0,
        "patrol_boat" => 25.0,
        _ => f64::INFINITY,
    }
}

/// The identity estimate: the best label, its confidence, and the AIS identity.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Identity {
    /// the most specific label a sensor gave (kind, or a radar/ESM family)
    pub kind: Option<String>,
    pub confidence: f64,
    pub ais: Option<AisHeard>,
    /// why the identity is not to be trusted, when it isn't
    pub conflict: Option<String>,
    /// which sensor types support `kind`
    pub supported_by: Vec<SensorType>,
}

impl Track {
    pub fn new(id: String, no: u64, d: &Detection, now: f64) -> Track {
        let mut t = Track {
            id,
            no,
            domain: d.domain,
            lat: d.lat,
            lon: d.lon,
            alt_m: d.alt_m,
            alt_t: None,
            vel: None,
            vel_est: (0.0, 0.0),
            cov: AxisCov {
                pp: d.sigma_m * d.sigma_m,
                pv: 0.0,
                vv: initial_vel_sigma(d.domain).powi(2),
            },
            sigma_m: d.sigma_m,
            confidence: 0.0,
            first_seen_t: now,
            last_seen_t: now,
            hits: 0,
            sources: BTreeMap::new(),
            classifiers: BTreeMap::new(),
            ais: None,
            ais_t: None,
            declared: None,
            review: Vec::new(),
            raised: None,
        };
        // the first fix is the estimate: its own update would count it twice
        t.first_fix(d, now);
        t
    }

    /// The first detection: position as measured, velocity unknown (AIS gives one).
    fn first_fix(&mut self, d: &Detection, now: f64) {
        if let Some((e, n)) = d.velocity_mps {
            self.vel_est = (e, n);
            self.cov.vv = AIS_VEL_SIGMA_MPS * AIS_VEL_SIGMA_MPS;
            self.vel = Some((e, n));
        }
        self.after_update(d, now);
    }

    /// Age in seconds at `now` (never negative).
    pub fn age_s(&self, now: f64) -> f64 {
        (now - self.last_seen_t).max(0.0)
    }

    pub fn speed_mps(&self) -> f64 {
        self.vel.map_or(0.0, |(e, n)| e.hypot(n))
    }

    pub fn course_deg(&self) -> Option<f64> {
        self.vel
            .filter(|(e, n)| e.hypot(*n) > 0.3)
            .map(|(e, n)| e.atan2(n).to_degrees().rem_euclid(360.0))
    }

    /// 1-sigma horizontal uncertainty (per axis) at `now`: the filter's, carried forward.
    pub fn sigma_at(&self, now: f64) -> f64 {
        self.cov.predict(self.age_s(now), process_noise(self.domain)).pp.sqrt()
    }

    /// Where the track is believed to be at `now`: the last estimate carried forward on
    /// its velocity (dead reckoning); the uncertainty says how far to trust it.
    pub fn predicted(&self, now: f64) -> (f64, f64) {
        let Some((e, n)) = self.vel else {
            return (self.lat, self.lon);
        };
        let dt = self.age_s(now);
        (
            self.lat + n * dt / 111_320.0,
            self.lon + e * dt / (111_320.0 * self.lat.to_radians().cos()),
        )
    }

    /// The filter's own prediction to `now` (its velocity estimate, known or not).
    fn predicted_est(&self, now: f64) -> (f64, f64) {
        let (e, n) = self.vel_est;
        let dt = self.age_s(now);
        (
            self.lat + n * dt / 111_320.0,
            self.lon + e * dt / (111_320.0 * self.lat.to_radians().cos()),
        )
    }

    /// Existence confidence at `now`: decays while unobserved.
    pub fn confidence_at(&self, now: f64) -> f64 {
        self.confidence * (-self.age_s(now) / CONF_DECAY_S).exp()
    }

    pub fn is_stale(&self, now: f64) -> bool {
        self.age_s(now) > STALE_S
    }

    /// Fold one detection in: the filter's prediction to `now`, its update with the fix
    /// (and an AIS report's velocity), then everything the fix says besides position.
    pub fn update(&mut self, d: &Detection, now: f64) {
        let q = process_noise(self.domain);
        let dt = self.age_s(now);
        let (plat, plon) = self.predicted_est(now);
        let mut c = self.cov.predict(dt, q);
        // the position measurement, per axis (east, north in metres)
        let r = d.sigma_m * d.sigma_m;
        let m_per_lon = 111_320.0 * plat.to_radians().cos();
        let ie = (d.lon - plon) * m_per_lon;
        let inn = (d.lat - plat) * 111_320.0;
        let nis = (ie * ie + inn * inn) / (c.pp + r);
        if nis > NIS_JUMP {
            let k = nis / 2.0;
            c = AxisCov {
                pp: c.pp * k,
                pv: c.pv * k,
                vv: c.vv * k,
            };
        }
        let s = c.pp + r;
        let (kp, kv) = (c.pp / s, c.pv / s);
        let (mut e, mut n) = (0.0, 0.0);
        let (mut ve, mut vn) = self.vel_est;
        e += kp * ie;
        n += kp * inn;
        ve += kv * ie;
        vn += kv * inn;
        c = AxisCov {
            pp: c.pp - c.pp * c.pp / s,
            pv: c.pv - c.pp * c.pv / s,
            vv: c.vv - c.pv * c.pv / s,
        };
        // AIS: the contact's own course and speed over ground, a velocity measurement
        if let Some((ae, an)) = d.velocity_mps {
            let rv = AIS_VEL_SIGMA_MPS * AIS_VEL_SIGMA_MPS;
            let s = c.vv + rv;
            let (kp, kv) = (c.pv / s, c.vv / s);
            let (ie, inn) = (ae - ve, an - vn);
            e += kp * ie;
            n += kp * inn;
            ve += kv * ie;
            vn += kv * inn;
            c = AxisCov {
                pp: c.pp - c.pv * c.pv / s,
                pv: c.pv - c.pv * c.vv / s,
                vv: c.vv - c.vv * c.vv / s,
            };
        }
        self.lat = plat + n / 111_320.0;
        self.lon = plon + e / m_per_lon;
        self.vel_est = (ve, vn);
        self.cov = c;
        if d.alt_m.is_some() {
            let k = kp;
            self.alt_m = match (self.alt_m, d.alt_m) {
                (Some(a), Some(b)) => Some(a + k * (b - a)),
                (_, b) => b,
            };
            self.alt_t = Some(now);
        }
        self.after_update(d, now);
    }

    /// What a detection says besides position: velocity known, existence, sources, identity.
    fn after_update(&mut self, d: &Detection, now: f64) {
        self.sigma_m = self.cov.pp.sqrt();
        self.vel = (self.cov.vv.sqrt() < VEL_KNOWN_SIGMA_MPS).then_some(self.vel_est);
        if d.alt_m.is_some() && self.alt_m.is_none() {
            self.alt_m = d.alt_m;
            self.alt_t = Some(now);
        }
        // existence
        self.confidence = self.confidence_at(now);
        self.confidence += (1.0 - self.confidence) * HIT_GAIN;
        self.hits += 1;
        if d.domain != self.domain {
            self.domain = d.domain;
        }
        // sources
        let src = self
            .sources
            .entry((d.uid, d.sensor))
            .or_insert(Source {
                uid: d.uid,
                sensor: d.sensor,
                hits: 0,
                last_t: now,
            });
        src.hits += 1;
        src.last_t = now;
        // classification dwell per sensor type
        if let Some(label) = &d.label {
            let c = self.classifiers.entry(d.sensor).or_insert(Classifier {
                label: label.clone(),
                dwell_s: 0.0,
                last_t: now,
            });
            let gap = (now - c.last_t).clamp(0.0, MAX_DWELL_PER_HIT_S);
            c.dwell_s += if c.dwell_s == 0.0 { 1.0 } else { gap };
            c.last_t = now;
            c.label = label.clone();
        }
        if let Some(a) = &d.ais {
            self.ais = Some(a.clone());
            self.ais_t = Some(now);
        }
        self.last_seen_t = now;
    }

    fn classifier_conf(&self, sensor: SensorType, c: &Classifier) -> f64 {
        let Some((max, tau)) = sensor.classifier() else {
            return 0.0;
        };
        max * (1.0 - (-c.dwell_s / tau).exp())
    }

    /// The identity estimate from what the sensors said. Prefers a kind-level label (EO/IR,
    /// sonar) over AIS over a family (radar, ESM); combines the sensors consistent with it.
    pub fn identity(&self) -> Identity {
        let kind_level = [SensorType::EoIr, SensorType::Sonar]
            .into_iter()
            .filter_map(|s| self.classifiers.get(&s).map(|c| (s, c)))
            .max_by(|a, b| {
                self.classifier_conf(a.0, a.1)
                    .total_cmp(&self.classifier_conf(b.0, b.1))
            });
        let ais_kind_label = self.ais.as_ref().map(|a| ais_kind(&a.ship_type).to_string());
        let family_level = [SensorType::Radar, SensorType::Esm]
            .into_iter()
            .filter_map(|s| self.classifiers.get(&s).map(|c| (s, c)))
            .max_by(|a, b| {
                self.classifier_conf(a.0, a.1)
                    .total_cmp(&self.classifier_conf(b.0, b.1))
            });
        let kind = kind_level
            .map(|(_, c)| c.label.clone())
            .or_else(|| ais_kind_label.clone())
            .or_else(|| family_level.map(|(_, c)| c.label.clone()));
        let Some(kind) = kind else {
            return Identity {
                kind: None,
                confidence: 0.0,
                ais: self.ais.clone(),
                conflict: None,
                supported_by: Vec::new(),
            };
        };
        // every sensor whose label agrees (same kind, or the kind's family)
        let fam = family_of(&kind);
        let mut supported_by = Vec::new();
        let mut miss = 1.0;
        for (s, c) in &self.classifiers {
            let agrees = c.label == kind || (c.label == fam && fam != "unknown");
            if agrees {
                supported_by.push(*s);
                miss *= 1.0 - self.classifier_conf(*s, c);
            }
        }
        let mut conflict = None;
        if let Some(a) = &self.ais {
            let ak = ais_kind(&a.ship_type);
            if ak == kind || (family_of(ak) == fam && kind_level.is_none()) {
                supported_by.push(SensorType::Ais);
                miss *= 1.0 - AIS_CONF;
            } else if let Some((s, c)) = kind_level
                && self.classifier_conf(s, c) >= 0.3
            {
                conflict = Some(format!(
                    "AIS says {} ({}) but {} classifies it as {}",
                    a.ship_type,
                    a.name,
                    s.as_str(),
                    c.label
                ));
            }
            let speed = self.speed_mps();
            if conflict.is_none() && speed > plausible_speed_mps(ak) {
                conflict = Some(format!(
                    "AIS says {} ({}) but it is making {:.0} m/s",
                    a.ship_type, a.name, speed
                ));
            }
        }
        // a disagreeing kind-level classifier against the chosen kind
        for (s, c) in &self.classifiers {
            if conflict.is_none()
                && s.classifier().is_some()
                && matches!(s, SensorType::EoIr | SensorType::Sonar)
                && c.label != kind
                && self.classifier_conf(*s, c) >= 0.3
            {
                conflict = Some(format!(
                    "{} calls it {} while the estimate is {}",
                    s.as_str(),
                    c.label,
                    kind
                ));
            }
        }
        Identity {
            kind: Some(kind),
            confidence: 1.0 - miss,
            ais: self.ais.clone(),
            conflict,
            supported_by,
        }
    }

    /// The affiliation shown and why: a declaration in force (an operator's, or the engine's
    /// hostile-act rule's), else a suspect raise by a review flag, else the estimate.
    pub fn affiliation(&self) -> (Affiliation, f64, String) {
        if let Some(d) = &self.declared {
            let why = match (&d.basis, d.rule) {
                (Some(b), true) => b.clone(),
                _ => format!("declared by {}", d.by),
            };
            return (d.affiliation, 1.0, why);
        }
        let (aff, conf, why) = self.estimate();
        if let Some(r) = &self.raised {
            return (Affiliation::Suspect, conf, format!("raised to suspect for review: {r} (its own estimate: {}, {why})", aff.as_str()));
        }
        (aff, conf, why)
    }

    /// The affiliation estimate and why, from the identity alone (what the decision engine is
    /// given; it fills declarations and raises from its own ledger). Hostile is never derived
    /// here: it takes a declaration.
    pub fn estimate(&self) -> (Affiliation, f64, String) {
        let id = self.identity();
        let conf = id.confidence;
        if let Some(c) = &id.conflict {
            return (Affiliation::Suspect, conf.max(0.5), c.clone());
        }
        let Some(kind) = id.kind.as_deref() else {
            return (Affiliation::Unknown, 0.0, "no classification yet".into());
        };
        if let Some(a) = &id.ais {
            let aff = match a.ship_type.as_str() {
                "law_enforcement" | "military" | "sar" => Affiliation::AssumedFriend,
                _ => Affiliation::Neutral,
            };
            return (
                aff,
                conf,
                format!("AIS identity {} ({}), consistent", a.name, a.ship_type),
            );
        }
        if conf < 0.3 {
            return (
                Affiliation::Unknown,
                conf,
                format!("classification {kind} not yet confident"),
            );
        }
        match kind {
            "fishing_vessel" | "sailboat" => (
                Affiliation::Neutral,
                conf * 0.8,
                format!("{kind}, small craft not required to carry AIS"),
            ),
            "light_aircraft" | "helicopter" => (Affiliation::Neutral, conf * 0.8, kind.to_string()),
            "cargo_ship" | "tanker" | "ferry" => (
                Affiliation::Suspect,
                conf * 0.8,
                format!("{kind} not broadcasting AIS"),
            ),
            "small_fast_boat" => (
                Affiliation::Suspect,
                conf,
                "fast small craft, no AIS".into(),
            ),
            "unknown_uas" => (
                Affiliation::Suspect,
                conf,
                "UAS without a transponder".into(),
            ),
            "submarine" | "uuv" => (
                Affiliation::Suspect,
                conf,
                "unidentified submerged contact".into(),
            ),
            _ => (
                Affiliation::Unknown,
                conf * 0.5,
                format!("{kind}: nothing to judge affiliation by"),
            ),
        }
    }

    /// 15-character 2525C SIDC for the symbol: affiliation, battle dimension, function by
    /// the estimated kind, status A (anticipated, dashed) when stale.
    pub fn sidc(&self, now: f64) -> String {
        let (aff, _, _) = self.affiliation();
        let dim = match self.domain {
            Domain::Air => 'A',
            Domain::Surface => 'S',
            Domain::Subsurface => 'U',
            Domain::Ground => 'G',
        };
        let status = if self.is_stale(now) { 'A' } else { 'P' };
        let kind = self.identity().kind.unwrap_or_default();
        let func = match (self.domain, kind.as_str()) {
            (Domain::Surface, "cargo_ship" | "tanker") => "XM----",
            (Domain::Surface, "ferry") => "XMP---",
            (Domain::Surface, "fishing_vessel") => "XF----",
            (Domain::Surface, "sailboat") => "XL----",
            (Domain::Surface, "patrol_boat") => "CP----",
            (Domain::Surface, "small_fast_boat") => "XR----",
            (Domain::Air, "light_aircraft") => "CF----",
            (Domain::Air, "helicopter") => "CH----",
            (Domain::Air, "unknown_uas") => "MFQ---",
            (Domain::Subsurface, "submarine") => "S-----",
            (Domain::Subsurface, "uuv") => "SU----",
            (Domain::Ground, "truck" | "vehicle") => "EV----",
            _ => "------",
        };
        format!("S{}{dim}{status}{func}-----", aff.sidc_char())
    }

    /// The public JSON: everything the console and agents may know. No contact id, no
    /// truth of any kind. The estimate is as of `last_seen_t`; readers carry it forward
    /// themselves (position along `velocity_mps`, `pos_sigma_m` growing by
    /// `sigma_growth_mps` per second, `confidence` decaying), so the message only changes
    /// when the track does. `now` (the bridge's wall clock) sets `state` and the symbol.
    pub fn json(&self, now: f64) -> Value {
        let id = self.identity();
        let (aff, aff_conf, why) = self.affiliation();
        json!({
            "id": self.id,
            "domain": self.domain.as_str(),
            "position": [round(self.lon, 6), round(self.lat, 6)],
            "alt_m": self.alt_m.map(|a| round(a, 1)),
            "velocity_mps": self.vel.map(|(e, n)| [round(e, 2), round(n, 2)]),
            "speed_mps": round(self.speed_mps(), 1),
            "course_deg": self.course_deg().map(|c| round(c, 0)),
            "pos_sigma_m": round(self.sigma_m, 1),
            "sigma_growth_mps": round(sigma_growth_mps(self.speed_mps()), 2),
            "confidence": round(self.confidence, 3),
            "confidence_decay_s": CONF_DECAY_S,
            "affiliation": aff.as_str(),
            "affiliation_confidence": round(aff_conf, 3),
            "affiliation_reason": why,
            "identity": {
                "kind": id.kind,
                "confidence": round(id.confidence, 3),
                "ais": id.ais,
                "conflict": id.conflict,
                "supported_by": id.supported_by,
            },
            "sources": self.sources.values().map(|s| json!({
                "uid": s.uid, "sensor": s.sensor.as_str(), "hits": s.hits, "last_t": round(s.last_t, 2),
            })).collect::<Vec<_>>(),
            "first_seen_t": round(self.first_seen_t, 2),
            "last_seen_t": round(self.last_seen_t, 2),
            "hits": self.hits,
            "state": if self.is_stale(now) { "stale" } else { "active" },
            "declared": self.declared,
            "review": self.review,
            "sidc": self.sidc(now),
        })
    }

    /// The decision layer's view (`WorldSnapshot.tracks`), at engine time `t` (wall ms): the
    /// estimate only. Rules 9: the engine fills declarations, their basis and suspect raises
    /// from its identification ledger (the record); the mirror kept here is for display.
    pub fn facts(&self, now: f64) -> TrackFacts {
        let id = self.identity();
        let (aff, aff_conf, _) = self.estimate();
        let ms = |s: f64| Time((s * 1000.0).round() as i64);
        TrackFacts {
            position: Fact::new(LonLat::new(self.lon, self.lat), ms(self.last_seen_t)),
            alt_m: self.alt_m,
            alt_t: self.alt_t.map(ms),
            velocity_mps: self.vel.map(|(e, n)| [e, n]),
            pos_sigma_m: self.sigma_at(now),
            sigma_growth_mps: Some(sigma_growth_mps(self.speed_mps())),
            confidence: self.confidence_at(now),
            affiliation: Fact::new(aff, ms(self.last_seen_t)),
            affiliation_confidence: aff_conf,
            identity: id.kind,
            identity_confidence: id.confidence,
            sources: self
                .sources
                .values()
                .map(|s| format!("{}:{}", s.uid, s.sensor.as_str()))
                .collect(),
            source_t: self
                .sources
                .values()
                .map(|s| (format!("{}:{}", s.uid, s.sensor.as_str()), ms(s.last_t)))
                .collect(),
            domain: self.domain.decide(),
            last_seen_t: ms(self.last_seen_t),
            // rules 9: the engine fills these from its identification ledger
            declared_by: None,
            declaration: None,
            review_flags: Vec::new(),
        }
    }
}
