//! A track: what the bridge believes about one contact, built only from detections
//! (`sensing.rs`). Staleness is first class: every reader gets `last_seen_t`, and the
//! position uncertainty grows with age at `sigma_growth_mps`, so a track that nobody has
//! seen for a minute is visibly loose and an engagement check (Phase E) can refuse it.
//!
//! Estimation: an interacting multiple model filter (IMM, Blom and Bar-Shalom) of two
//! constant-velocity Kalman filters per horizontal axis, one for a contact keeping its course
//! and speed (little process noise) and one for a manoeuvring contact (much more), switching
//! as a Markov chain ([`process_noise`], [`SOJOURN_S`]). Every detection is a position
//! measurement with its sensor's 1-sigma and an AIS report's course and speed a velocity
//! measurement; each fix weighs the two models by how well each predicted it, and the
//! estimate is their mixture, whose covariance widens while they disagree (a turn, a sprint,
//! a stop). The reported covariance is that widened by the recent fixes' consistency
//! ([`REPORT_NIS`]): fixes farther off than predicted mean the filter has not yet taken in a
//! manoeuvre. The 1-sigma is checked against the simulation's truth on seeds and motions it
//! was not tuned on (`tracks::tests::calib`). Until rules 9's review the filter was a scalar
//! update with a velocity smoothed from fixes (too loose holding, too tight after a
//! manoeuvre); the first Kalman filter of that review had one model, honest on the motions it
//! was tuned to and overconfident on a zigzag or a sprint and stop.
//!
//! Each fix is dated by the clock of the truth it was drawn from (`Detection::measured_t`),
//! not by its arrival: on this host the bridge's wall clock steps back about a second every
//! half minute, and a filter that trusts its velocity to 0.3 m/s cannot take a second lost.
//!
//! A minute ahead the filter's velocity is the current leg's, which a contact that zigzags,
//! weaves or sprints and stops will not keep: the track also gives its average velocity over
//! the last minute (`TrackFacts::mean_velocity_mps`), and remembers whether its velocity
//! changed beyond its noise within the last [`MANOEUVRE_MEMORY_S`] and the fastest it went
//! (`TrackFacts::manoeuvre`); a keep-out counts it along both velocities and, manoeuvring,
//! anywhere it could reach (rules 9).
//!
//! Existence confidence climbs with hits and decays in silence; identity confidence grows
//! with dwell per sensor type and combines across independent sensors. Several observers'
//! fixes of one tick are folded one after another, as independent measurements (they are:
//! each sensor's draw is its own). Association is by the simulation's contact id (see
//! `mod.rs`); a dropped track that is seen again gets a new id, as a real tracker would start
//! a new track.

use std::collections::{BTreeMap, VecDeque};

use murmur_decide::{Affiliation, Fact, LonLat, Manoeuvre, Time, TrackFacts};
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

/// The two models' white-noise acceleration by domain (spectral density, m^2/s^3): steady
/// (what a contact keeping its course and speed still does that the constant-velocity model
/// does not know) and manoeuvring (a turn, a sprint, a stop). Surface values tuned on the
/// calibration test's tuning sets (`tracks::tests::calib`); air, ground and subsurface scaled
/// from the single-model values by the same factors (a fifth, and forty times), not checked
/// against truth.
pub fn process_noise(domain: Domain) -> [f64; 2] {
    match domain {
        Domain::Air => [0.4, 80.0],
        Domain::Surface => [0.01, 2.0],
        Domain::Subsurface => [0.002, 0.4],
        Domain::Ground => [0.06, 12.0],
    }
}

/// Mean time a contact stays in each model before switching (steady, manoeuvring), s: the
/// Markov chain between them (a manoeuvre every two minutes or so, lasting half a minute).
pub const SOJOURN_S: [f64; 2] = [120.0, 30.0];

/// A fix this far from the manoeuvring model's prediction (its innovation's normalized
/// square, chi-square with 2 degrees of freedom, 99%) widens that model's prior by the
/// innovation's own size before the update: the contact did something neither model
/// expected (a stop from speed within a second), and the fix outranks the prediction. The
/// model's weight is judged on its prediction as it was; the steady model keeps its own (an
/// outlier fix does not drag the estimate).
const NIS_JUMP: f64 = 9.21;

/// The reported covariance is the filter's widened by the recent fixes' consistency: their
/// normalized distance from the mixture's prediction (expected 2), averaged with this weight
/// per fix (about the last three fixes), over [`REPORT_NIS`] when above it. [`REPORT_NIS`]
/// is below the expectation, so a consistent filter is widened too (about 1.33 times its
/// variance on average: steady tracks report loose, NEES about 1.1, by design); a run of
/// fixes farther off than predicted, a filter behind a manoeuvre it has not yet taken in,
/// widens it more. The estimate itself is unchanged.
const REPORT_ALPHA: f64 = 0.3;
/// Tuned: at 2 (the expectation) a sprint and stop and a weave stayed overconfident.
const REPORT_NIS: f64 = 1.5;

/// The average velocity a keep-out also predicts along is over about this long, s (the
/// displacement since the position sample nearest this long ago, at least
/// [`MEAN_MIN_S`] ago).
pub const MEAN_WINDOW_S: f64 = 60.0;
pub const MEAN_MIN_S: f64 = 45.0;

/// The memories above, as the keep-out explanation ages a picture with them
/// (`murmur_decide::keepout::project`).
pub fn memory() -> murmur_decide::keepout::TrackerMemory {
    murmur_decide::keepout::TrackerMemory {
        mean_min_s: MEAN_MIN_S,
        mean_window_s: MEAN_WINDOW_S,
        manoeuvre_memory_s: MANOEUVRE_MEMORY_S,
    }
}

/// The manoeuvring model's probability above which the track counts as manoeuvring for the
/// keep-out (as well as a velocity change beyond its noise).
const MANOEUVRE_P: f64 = 0.8;

/// The track remembers its velocity every this many seconds, for the manoeuvre evidence.
const VEL_SAMPLE_S: f64 = 10.0;
/// How long a manoeuvre is remembered, and over what the velocity is compared, s.
pub const MANOEUVRE_MEMORY_S: f64 = 300.0;
/// A change of velocity counts as a manoeuvre above this, m/s, and only when unlikely as
/// noise: its square above [`MANOEUVRE_CHI2`] times the two estimates' variances.
const MANOEUVRE_MIN_MPS: f64 = 2.0;
/// Chi-square with 2 degrees of freedom, 99.9%.
const MANOEUVRE_CHI2: f64 = 13.8;

/// The filter predicts over the interval between two fixes' own times (`Detection::measured_t`)
/// when it is between 0 and this, seconds; otherwise (no time, a clock that jumped) over the
/// interval between their arrivals.
const MAX_FIX_INTERVAL_S: f64 = 120.0;

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

    fn scaled(self, k: f64) -> AxisCov {
        AxisCov {
            pp: self.pp * k,
            pv: self.pv * k,
            vv: self.vv * k,
        }
    }

    /// Plus the outer product of a (position, velocity) difference: a mixture's spread.
    fn spread(self, w: f64, dp: f64, dv: f64) -> AxisCov {
        AxisCov {
            pp: self.pp + w * dp * dp,
            pv: self.pv + w * dp * dv,
            vv: self.vv + w * dv * dv,
        }
    }
}

/// One of the filter's two models: position east and north of the track's estimate (m),
/// velocity east and north (m/s), the covariance per axis, and the model's probability.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Model {
    x: [f64; 4],
    ce: AxisCov,
    cn: AxisCov,
    p: f64,
}

impl Model {
    /// Carried `dt` seconds forward with white-noise acceleration `q`.
    fn predicted(&self, dt: f64, q: f64) -> Model {
        Model {
            x: [self.x[0] + self.x[2] * dt, self.x[1] + self.x[3] * dt, self.x[2], self.x[3]],
            ce: self.ce.predict(dt, q),
            cn: self.cn.predict(dt, q),
            p: self.p,
        }
    }

    /// A position measurement `z` (east, north m) with variance `r` per axis: the update, and
    /// the measurement's log-likelihood under this model's prediction as it was.
    fn update_position(&mut self, z: [f64; 2], r: f64, jump: bool) -> f64 {
        let (ie, inn) = (z[0] - self.x[0], z[1] - self.x[1]);
        let (se, sn) = (self.ce.pp + r, self.cn.pp + r);
        let nis = ie * ie / se + inn * inn / sn;
        let loglik = -0.5 * nis - (2.0 * std::f64::consts::PI).ln() - 0.5 * (se * sn).ln();
        if jump && nis > NIS_JUMP {
            let k = nis / 2.0;
            self.ce = self.ce.scaled(k);
            self.cn = self.cn.scaled(k);
        }
        let (pe, ve) = kalman_position(&mut self.ce, ie, r);
        let (pn, vn) = kalman_position(&mut self.cn, inn, r);
        self.x = [self.x[0] + pe, self.x[1] + pn, self.x[2] + ve, self.x[3] + vn];
        loglik
    }

    /// A velocity measurement (an AIS report's course and speed over ground): the update and
    /// its log-likelihood.
    fn update_velocity(&mut self, z: [f64; 2], r: f64) -> f64 {
        let (ie, inn) = (z[0] - self.x[2], z[1] - self.x[3]);
        let (se, sn) = (self.ce.vv + r, self.cn.vv + r);
        let loglik = -0.5 * (ie * ie / se + inn * inn / sn)
            - (2.0 * std::f64::consts::PI).ln()
            - 0.5 * (se * sn).ln();
        let (pe, ve) = kalman_velocity(&mut self.ce, ie, r);
        let (pn, vn) = kalman_velocity(&mut self.cn, inn, r);
        self.x = [self.x[0] + pe, self.x[1] + pn, self.x[2] + ve, self.x[3] + vn];
        loglik
    }
}

/// One axis's Kalman update with a position innovation `i` of variance `r`: the corrections
/// to position and velocity; the covariance updated in place.
fn kalman_position(c: &mut AxisCov, i: f64, r: f64) -> (f64, f64) {
    let s = c.pp + r;
    let (kp, kv) = (c.pp / s, c.pv / s);
    *c = AxisCov {
        pp: c.pp - c.pp * c.pp / s,
        pv: c.pv - c.pp * c.pv / s,
        vv: c.vv - c.pv * c.pv / s,
    };
    (kp * i, kv * i)
}

/// One axis's Kalman update with a velocity innovation `i` of variance `r`.
fn kalman_velocity(c: &mut AxisCov, i: f64, r: f64) -> (f64, f64) {
    let s = c.vv + r;
    let (kp, kv) = (c.pv / s, c.vv / s);
    *c = AxisCov {
        pp: c.pp - c.pv * c.pv / s,
        pv: c.pv - c.pv * c.vv / s,
        vv: c.vv - c.vv * c.vv / s,
    };
    (kp * i, kv * i)
}

/// The models' mixture: mean state and covariance per axis (with the spread of their means).
fn mixture(models: &[Model; 2]) -> ([f64; 4], AxisCov, AxisCov) {
    let mut x = [0.0; 4];
    for m in models {
        for (xk, mk) in x.iter_mut().zip(m.x) {
            *xk += m.p * mk;
        }
    }
    let zero = AxisCov { pp: 0.0, pv: 0.0, vv: 0.0 };
    let (mut ce, mut cn) = (zero, zero);
    for m in models {
        ce = AxisCov {
            pp: ce.pp + m.p * m.ce.pp,
            pv: ce.pv + m.p * m.ce.pv,
            vv: ce.vv + m.p * m.ce.vv,
        }
        .spread(m.p, m.x[0] - x[0], m.x[2] - x[2]);
        cn = AxisCov {
            pp: cn.pp + m.p * m.cn.pp,
            pv: cn.pv + m.p * m.cn.pv,
            vv: cn.vv + m.p * m.cn.vv,
        }
        .spread(m.p, m.x[1] - x[1], m.x[3] - x[3]);
    }
    (x, ce, cn)
}

/// The IMM's interaction over `dt` seconds: each model's prior is the mixture of both,
/// weighted by the chance the contact switched between them ([`SOJOURN_S`]); the models'
/// probabilities become the predicted ones.
fn interact(models: &[Model; 2], dt: f64) -> [Model; 2] {
    let leave = [1.0 - (-dt / SOJOURN_S[0]).exp(), 1.0 - (-dt / SOJOURN_S[1]).exp()];
    // trans[i][j]: from model i to model j
    let trans = [[1.0 - leave[0], leave[0]], [leave[1], 1.0 - leave[1]]];
    let mut out = *models;
    for j in 0..2 {
        let cbar: f64 = (0..2).map(|i| trans[i][j] * models[i].p).sum();
        if cbar <= 0.0 {
            continue;
        }
        let w = [trans[0][j] * models[0].p / cbar, trans[1][j] * models[1].p / cbar];
        let mixed = mixture(&[
            Model { p: w[0], ..models[0] },
            Model { p: w[1], ..models[1] },
        ]);
        out[j] = Model {
            x: mixed.0,
            ce: mixed.1,
            cn: mixed.2,
            p: cbar,
        };
    }
    out
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
    /// the filter's two models at `last_seen_t`, relative to `lat`, `lon` (their mixture)
    models: [Model; 2],
    /// the velocity every `VEL_SAMPLE_S` over `MANOEUVRE_MEMORY_S`, while known: (wall s,
    /// east, north, its variance per axis)
    vel_hist: VecDeque<(f64, f64, f64, f64)>,
    /// the estimate's position every `VEL_SAMPLE_S`: (wall s, lat, lon), for the average
    /// velocity over `MEAN_WINDOW_S`
    pos_hist: VecDeque<(f64, f64, f64)>,
    /// the last manoeuvre seen: when (wall s) and the largest change of velocity within
    /// the memory, m/s
    manoeuvre: Option<(f64, f64)>,
    /// the fixes' normalized distance from the mixture's prediction, averaged (expected 2)
    nis_avg: f64,
    /// the last fix's own time (`Detection::measured_t`), when it had one
    fix_t: Option<f64>,
    /// the reported covariance's factor over the filter's (1 or more, `REPORT_NIS`)
    inflate: f64,

    /// 1 sigma at `last_seen_t` (per axis, the root mean of the two axes' variances)
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
            models: [0, 1].map(|j| {
                let c = AxisCov {
                    pp: d.sigma_m * d.sigma_m,
                    pv: 0.0,
                    vv: initial_vel_sigma(d.domain).powi(2),
                };
                Model {
                    x: [0.0; 4],
                    ce: c,
                    cn: c,
                    p: SOJOURN_S[j] / (SOJOURN_S[0] + SOJOURN_S[1]),
                }
            }),
            vel_hist: VecDeque::new(),
            pos_hist: VecDeque::new(),
            manoeuvre: None,
            nis_avg: 2.0,
            inflate: 1.0,
            fix_t: d.measured_t,
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
            for m in &mut self.models {
                m.x[2] = e;
                m.x[3] = n;
                m.ce.vv = AIS_VEL_SIGMA_MPS * AIS_VEL_SIGMA_MPS;
                m.cn.vv = AIS_VEL_SIGMA_MPS * AIS_VEL_SIGMA_MPS;
            }
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

    /// 1-sigma horizontal uncertainty (per axis) at `now`: the filter's, each model carried
    /// forward with its own process noise, and their mixture.
    pub fn sigma_at(&self, now: f64) -> f64 {
        let dt = self.age_s(now);
        let q = process_noise(self.domain);
        let m = [self.models[0].predicted(dt, q[0]), self.models[1].predicted(dt, q[1])];
        let (_, ce, cn) = mixture(&m);
        (0.5 * (ce.pp + cn.pp) * self.inflate).sqrt()
    }

    /// The 1-sigma error of the velocity estimate per axis, m/s (the calibration test's).
    #[cfg(test)]
    pub fn vel_sigma_mps(&self) -> f64 {
        let (_, ce, cn) = mixture(&self.models);
        (0.5 * (ce.vv + cn.vv) * self.inflate).sqrt()
    }

    /// The manoeuvring model's probability now (0..1), for the calibration test.
    #[cfg(test)]
    pub fn manoeuvring_p(&self) -> f64 {
        self.models[1].p
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

    /// Existence confidence at `now`: decays while unobserved.
    pub fn confidence_at(&self, now: f64) -> f64 {
        self.confidence * (-self.age_s(now) / CONF_DECAY_S).exp()
    }

    pub fn is_stale(&self, now: f64) -> bool {
        self.age_s(now) > STALE_S
    }

    /// Fold one detection in: the models' interaction and prediction to `now`, each one's
    /// update with the fix (and an AIS report's velocity), their probabilities by how well
    /// each predicted it, then everything the fix says besides position.
    pub fn update(&mut self, d: &Detection, now: f64) {
        let q = process_noise(self.domain);
        // the interval between the fixes' own times, when both have one and it is sane; else
        // between their arrivals
        let dt = match (d.measured_t, self.fix_t) {
            (Some(m), Some(f)) if (0.0..=MAX_FIX_INTERVAL_S).contains(&(m - f)) => m - f,
            _ => self.age_s(now),
        };
        if d.measured_t.is_some() {
            self.fix_t = d.measured_t;
        }
        // the fix in metres east and north of the estimate (the models' origin)
        let m_per_lon = 111_320.0 * self.lat.to_radians().cos();
        let z = [(d.lon - self.lon) * m_per_lon, (d.lat - self.lat) * 111_320.0];
        let mut models = if dt > 0.0 { interact(&self.models, dt) } else { self.models };
        for (j, m) in models.iter_mut().enumerate() {
            *m = m.predicted(dt, q[j]);
        }
        let r = d.sigma_m * d.sigma_m;
        let prior_pp: f64 = models.iter().map(|m| m.p * 0.5 * (m.ce.pp + m.cn.pp)).sum();
        // the fix against the mixture's prediction: the reported covariance's widening
        let (x, ce, cn) = mixture(&models);
        let nis = (z[0] - x[0]).powi(2) / (ce.pp + r) + (z[1] - x[1]).powi(2) / (cn.pp + r);
        self.nis_avg = (1.0 - REPORT_ALPHA) * self.nis_avg + REPORT_ALPHA * nis;
        self.inflate = (self.nis_avg / REPORT_NIS).max(1.0);
        let mut loglik = [0.0; 2];
        for (j, m) in models.iter_mut().enumerate() {
            loglik[j] = m.update_position(z, r, j == 1);
            // AIS: the contact's own course and speed over ground, a velocity measurement
            if let Some((ae, an)) = d.velocity_mps {
                loglik[j] += m.update_velocity([ae, an], AIS_VEL_SIGMA_MPS * AIS_VEL_SIGMA_MPS);
            }
        }
        // each model's probability by how well it predicted the fix
        let top = loglik[0].max(loglik[1]);
        let w = [0, 1].map(|j| models[j].p * (loglik[j] - top).exp());
        let total = w[0] + w[1];
        if total > 0.0 && total.is_finite() {
            for j in 0..2 {
                models[j].p = (w[j] / total).clamp(1e-6, 1.0 - 1e-6);
            }
        }
        // the mixture is the estimate; the models re-centred on it
        let (x, _, _) = mixture(&models);
        for m in &mut models {
            m.x[0] -= x[0];
            m.x[1] -= x[1];
        }
        self.lat += x[1] / 111_320.0;
        self.lon += x[0] / m_per_lon;
        self.models = models;
        if d.alt_m.is_some() {
            let k = prior_pp / (prior_pp + r);
            self.alt_m = match (self.alt_m, d.alt_m) {
                (Some(a), Some(b)) => Some(a + k * (b - a)),
                (_, b) => b,
            };
            self.alt_t = Some(now);
        }
        self.after_update(d, now);
    }

    /// The models' mixture now: the 1-sigma, the velocity (known once its 1-sigma is below
    /// `VEL_KNOWN_SIGMA_MPS`), and the manoeuvre evidence: a velocity that changed within
    /// the memory by more than `MANOEUVRE_MIN_MPS` and more than its noise explains.
    fn after_estimate(&mut self, now: f64) {
        if self.pos_hist.back().is_none_or(|s| now - s.0 >= VEL_SAMPLE_S) {
            self.pos_hist.push_back((now, self.lat, self.lon));
        }
        while self.pos_hist.front().is_some_and(|s| now - s.0 > MEAN_WINDOW_S + VEL_SAMPLE_S) {
            self.pos_hist.pop_front();
        }
        let (x, ce, cn) = mixture(&self.models);
        self.sigma_m = (0.5 * (ce.pp + cn.pp) * self.inflate).sqrt();
        let vv = 0.5 * (ce.vv + cn.vv);
        let known = vv.sqrt() < VEL_KNOWN_SIGMA_MPS;
        self.vel = known.then_some((x[2], x[3]));
        if !known {
            return;
        }
        self.vel_hist.retain(|s| now - s.0 <= MANOEUVRE_MEMORY_S);
        let change = self
            .vel_hist
            .iter()
            .filter_map(|&(_, ve, vn, svv)| {
                let dv = (x[2] - ve).hypot(x[3] - vn);
                (dv > MANOEUVRE_MIN_MPS && dv * dv > MANOEUVRE_CHI2 * (vv + svv)).then_some(dv)
            })
            .fold(None, |a: Option<f64>, dv| Some(a.map_or(dv, |a| a.max(dv))));
        // or the manoeuvring model all but certain: the largest change it made, whatever it is
        let change = change.or_else(|| {
            (self.models[1].p > MANOEUVRE_P).then(|| {
                self.vel_hist
                    .iter()
                    .map(|&(_, ve, vn, _)| (x[2] - ve).hypot(x[3] - vn))
                    .fold(0.0, f64::max)
            })
        });
        if let Some(dv) = change {
            let before = self
                .manoeuvre
                .filter(|(t, _)| now - t <= MANOEUVRE_MEMORY_S)
                .map_or(0.0, |(_, c)| c);
            self.manoeuvre = Some((now, dv.max(before)));
        }
        if self.vel_hist.back().is_none_or(|s| now - s.0 >= VEL_SAMPLE_S) {
            self.vel_hist.push_back((now, x[2], x[3], vv));
        }
    }

    /// The average velocity over about the last minute (east, north m/s): the displacement
    /// since the position sample nearest `MEAN_WINDOW_S` before the estimate, if one is at
    /// least `MEAN_MIN_S` old.
    pub fn mean_velocity(&self) -> Option<(f64, f64)> {
        let t = self.last_seen_t;
        let &(t0, lat0, lon0) = self
            .pos_hist
            .iter()
            .filter(|s| t - s.0 >= MEAN_MIN_S)
            .min_by(|a, b| ((t - a.0) - MEAN_WINDOW_S).abs().total_cmp(&((t - b.0) - MEAN_WINDOW_S).abs()))?;
        let dt = t - t0;
        Some((
            (self.lon - lon0) * 111_320.0 * self.lat.to_radians().cos() / dt,
            (self.lat - lat0) * 111_320.0 / dt,
        ))
    }

    /// The manoeuvre evidence at `t` (wall s): the last manoeuvre within the memory, the
    /// largest change, and the fastest the contact went within the memory with two sigmas
    /// of its velocity error.
    pub fn manoeuvre_at(&self, t: f64) -> Option<(f64, f64, f64)> {
        let (mt, change) = self.manoeuvre.filter(|(mt, _)| t - mt <= MANOEUVRE_MEMORY_S)?;
        let (x, ce, cn) = mixture(&self.models);
        let sv = (0.5 * (ce.vv + cn.vv)).sqrt();
        let fastest = self
            .vel_hist
            .iter()
            .filter(|s| t - s.0 <= MANOEUVRE_MEMORY_S)
            .map(|s| s.1.hypot(s.2))
            .fold(x[2].hypot(x[3]), f64::max);
        Some((mt, change, fastest + 2.0 * sv))
    }

    /// What a detection says besides position: velocity known, existence, sources, identity.
    fn after_update(&mut self, d: &Detection, now: f64) {
        self.after_estimate(now);
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
            manoeuvre: self.manoeuvre_at(self.last_seen_t).map(|(t, change, speed)| Manoeuvre {
                t: ms(t),
                change_mps: (change * 100.0).round() / 100.0,
                speed_mps: (speed * 100.0).round() / 100.0,
            }),
            mean_velocity_mps: self
                .vel
                .and(self.mean_velocity())
                .map(|(e, n)| [(e * 100.0).round() / 100.0, (n * 100.0).round() / 100.0]),
            // rules 12: when this track started (with no average velocity yet, it is young: the
            // engine of older rules drops it)
            first_seen_t: Some(ms(self.first_seen_t)),
        }
    }
}
