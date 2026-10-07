//! A track: what the bridge believes about one contact, built only from detections
//! (`sensing.rs`). Staleness is first class: every reader gets `last_seen_t`, and the
//! position uncertainty grows with age at `sigma_growth_mps`, so a track that nobody has
//! seen for a minute is visibly loose and an engagement check (Phase E) can refuse it.
//!
//! Estimation: an interacting multiple model filter (IMM, Blom and Bar-Shalom) of two
//! constant-velocity Kalman filters of the horizontal state (position and velocity, east and
//! north, with their full covariance), one for a contact keeping its course and speed (little
//! process noise) and one for a manoeuvring contact (much more), switching as a Markov chain
//! ([`process_noise`], [`SOJOURN_S`]). Every detection is a position measurement with its
//! sensor's 1-sigma, an AIS report's course and speed a velocity measurement, and a 2D radar's
//! fix of an aircraft (it measured the slant range and bearing, not the height) the bearing
//! line it lies on, no farther out than the slant range: one radar alone does not range an
//! aircraft, so a track no other fix has ranged within [`RANGE_FRESH_S`] is the middle of that
//! segment with its spread and no velocity ([`Model::reset_to_segment`]), and a ranged one
//! loses its along-line knowledge as an aircraft turning hard would ([`ALONG_Q`]) once only
//! bearings come; the track never invents its height. Each fix weighs
//! the two models by how well each predicted it, and the estimate is their mixture, whose
//! covariance widens while they disagree (a turn, a sprint, a stop). The reported covariance is
//! that widened by the recent fixes' consistency ([`REPORT_NIS`]): fixes farther off than
//! predicted mean the filter has not yet taken in a manoeuvre; the reported 1-sigma is its
//! least certain direction's. The 1-sigma is checked against the simulation's truth on seeds
//! and motions it was not tuned on (`tracks::tests::calib`, and for aircraft
//! `tracks::tests::calib_air`). Until rules 9's review the filter was a scalar update with a
//! velocity smoothed from fixes (too loose holding, too tight after a manoeuvre); the first
//! Kalman filter of that review had one model, honest on the motions it was tuned to and
//! overconfident on a zigzag or a sprint and stop; until murmur-oho each axis had its own
//! covariance and an aircraft's 2D radar fix was a position (the sensing model placed it at
//! the aircraft's true ground range).
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
/// A velocity estimate is shown (and given to decisions) once its 1-sigma, in its least
/// certain direction, is below this. Judged for aircraft too against the truth
/// (`tracks::tests::calib_air`): at 3.5 a route's velocities came with a normalized error over 2.
const VEL_KNOWN_SIGMA_MPS: f64 = 3.0;
/// An AIS report's course and speed over ground, 1-sigma per axis, m/s.
const AIS_VEL_SIGMA_MPS: f64 = 0.3;

/// The two models' white-noise acceleration by domain (spectral density, m^2/s^3): steady
/// (what a contact keeping its course and speed still does that the constant-velocity model
/// does not know) and manoeuvring (a turn, a sprint, a stop). Surface values tuned on the
/// calibration test's tuning sets (`tracks::tests::calib`); air values on the air tuning set
/// (`tracks::tests::calib_air`: they were 0.4 and 80, scaled from the surface's, and gave a
/// straight-flying aircraft seen by radar and EO/IR a velocity 15% of the time; the most
/// velocity with every class honest and a velocity on a route within a normalized error of 2);
/// ground and subsurface scaled from the single-model values by the same factors (a fifth,
/// and forty times), not checked against truth.
pub fn process_noise(domain: Domain) -> [f64; 2] {
    #[cfg(test)]
    if let (Domain::Air, Some(t)) = (domain, AIR_TUNE.get()) {
        return [t[0], t[1]];
    }
    match domain {
        Domain::Air => [0.2, 20.0],
        Domain::Surface => [0.01, 2.0],
        Domain::Subsurface => [0.002, 0.4],
        Domain::Ground => [0.06, 12.0],
    }
}

/// Mean time a contact stays in each model before switching (steady, manoeuvring), s: the
/// Markov chain between them (a manoeuvre every two minutes or so, lasting half a minute).
pub const SOJOURN_S: [f64; 2] = [120.0, 30.0];

#[cfg(test)]
thread_local! {
    /// The air calibration's tuning grid (`tests::calib_air`): the air constants overridden on
    /// this thread (steady and manoeuvring white-noise acceleration, the velocity gate, the
    /// initial velocity sigma).
    pub static AIR_TUNE: std::cell::Cell<Option<[f64; 4]>> = const { std::cell::Cell::new(None) };
}

/// A fix this far from the manoeuvring model's prediction (its innovation's normalized
/// square, chi-square with 2 degrees of freedom, 99%) widens that model's prior by the
/// innovation's own size before the update: the contact did something neither model
/// expected (a stop from speed within a second), and the fix outranks the prediction. The
/// model's weight is judged on its prediction as it was; the steady model keeps its own (an
/// outlier fix does not drag the estimate).
const NIS_JUMP: f64 = 9.21;
/// The same for a measurement of one dimension (a 2D radar's bearing line): chi-square with 1
/// degree of freedom, 99%.
const NIS_JUMP_1: f64 = 6.63;
/// A 2D radar's slant range bounds an aircraft's ground range from above, give or take this
/// many of the radar's own sigmas (the segment an unranged track is on).
const SLANT_SIGMAS: f64 = 2.0;
/// A ranged track's estimate is moved back to the slant range only beyond this many of the
/// radar's own sigmas (about one fix in 700 of a low aircraft, whose slant range is its ground
/// range, crosses 3 sigmas by noise: within it, the bound says nothing a Gaussian would not).
const SLANT_CLAMP_SIGMAS: f64 = 3.0;
/// A track is ranged (its position along a 2D radar's bearing measured) while a ranging fix
/// came within this, s: another sensor's position (EO/IR, a radar that measures elevation,
/// AIS), or another radar's bearing across this one by [`CROSSING_DEG`] or more (the pair
/// ranged it when the earlier of the two came). The engine's own limit for a ranged altitude
/// (`track_alt_max_age_s`, 10 s) and the track's stale limit ([`STALE_S`]); a ranging sensor at
/// the edge of its reach (half its fixes) misses ten in a row once in a thousand. After it the
/// track is the segment again, whatever its covariance says (the review of 10-07: a covariance
/// kept small by the steady model's noise let an unranged track slide kilometres along the
/// line).
const RANGE_FRESH_S: f64 = 10.0;
/// Two radars' bearings range an aircraft when they cross by this much or more, degrees. Where
/// they cross, the error along them is their errors across them over the crossing's sine: 3.9
/// times at 15 degrees, 2 at 30. At 15 (the first choice) two radars 12 km out, each 140 to 160
/// m across, put an aircraft 3 km off its truth at 3.8 of its sigmas, and a keep-out 3 s ahead
/// missed it by up to 1,235 m (`tracks::tests::calib_air`, round three); at 30 and above none
/// did, and the classes' honesty and velocity shares were the same.
const CROSSING_DEG: f64 = 30.0;
/// A 2D radar's bearings cannot see an aircraft manoeuvre along the line, which only a
/// position fix measures: once no position fix has come for this long, s, each bearing's
/// update first widens both models along the line to [`ALONG_Q`] (the models' own noise covers
/// a fix or two missed: 2, 3 and 4 s gave the same holds on the air tuning sets).
const ALONG_GRACE_S: f64 = 3.0;
/// The white-noise acceleration along a 2D radar's bearing line while only bearings come, m^2/s^3:
/// its 2-sigma covers an aircraft turning at 8 m/s^2 (the air sets' hardest turn is 7.7: 8
/// deg/s at 55 m/s; the contacts process turns a light aircraft at 6 deg/s) for the 7 s from
/// [`ALONG_GRACE_S`] to [`RANGE_FRESH_S`], `3 a^2 t / 16`, rounded. Measured on the air tuning
/// sets (`tracks::tests::calib_air`): at the manoeuvring model's 20 a keep-out 3 s ahead missed
/// an aircraft in random turns at 8 deg/s by up to 55 m; at 80 and 200 none.
const ALONG_Q: f64 = 80.0;
/// A track that knows an aircraft's range only from a 2D radar is at the middle of the bearing
/// segment the slant range bounds, with a 1-sigma of the segment's length over this: its ends
/// are 1.5 sigmas out (a uniform spread's own sigma would put them at 1.73, where a low
/// aircraft, whose slant range is about its ground range, always is).
const SEGMENT_SIGMAS: f64 = 3.0;

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
/// (`murmur_decide::keepout::project`), with the stale limit and how long an unseen track is
/// kept (`drop_after_s`, the run's `MURMUR_TRACK_DROP_S`).
pub fn memory(drop_after_s: f64) -> murmur_decide::keepout::TrackerMemory {
    murmur_decide::keepout::TrackerMemory {
        mean_min_s: MEAN_MIN_S,
        mean_window_s: MEAN_WINDOW_S,
        manoeuvre_memory_s: MANOEUVRE_MEMORY_S,
        stale_after_s: STALE_S,
        drop_after_s,
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
    #[cfg(test)]
    if let (Domain::Air, Some(t)) = (domain, AIR_TUNE.get()) {
        return t[3];
    }
    match domain {
        Domain::Air => 60.0,
        Domain::Surface => 10.0,
        Domain::Subsurface => 5.0,
        Domain::Ground => 15.0,
    }
}

/// A velocity estimate is shown (and given to decisions) once its 1-sigma, in its least
/// certain direction, is below this, m/s.
fn vel_known_sigma(domain: Domain) -> f64 {
    #[cfg(test)]
    if let (Domain::Air, Some(t)) = (domain, AIR_TUNE.get()) {
        return t[2];
    }
    let _ = domain;
    VEL_KNOWN_SIGMA_MPS
}

/// The state of one model: east and north of the track's estimate (m), velocity east and
/// north (m/s).
type V4 = [f64; 4];
/// Its covariance. Full, not per axis: a 2D radar's fix of an aircraft measures one direction
/// only (its bearing line), which couples east and north.
type M4 = [[f64; 4]; 4];

fn dot(a: V4, b: V4) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3]
}

fn mat_vec(m: &M4, v: V4) -> V4 {
    [dot(m[0], v), dot(m[1], v), dot(m[2], v), dot(m[3], v)]
}

/// The larger eigenvalue of the symmetric 2x2 [[a, b], [b, c]]: a covariance's variance in its
/// least certain direction (for one that is the same every way, `a`).
fn var_max(a: f64, b: f64, c: f64) -> f64 {
    0.5 * (a + c) + (0.25 * (a - c) * (a - c) + b * b).sqrt()
}

/// A position block's and a velocity block's variance in their least certain direction.
fn pos_var(c: &M4) -> f64 {
    var_max(c[0][0], c[0][1], c[1][1])
}

fn vel_var(c: &M4) -> f64 {
    var_max(c[2][2], c[2][3], c[3][3])
}

/// The covariance carried `dt` seconds forward with white-noise acceleration `q` on each axis
/// (constant velocity: `F P F' + Q`).
fn predict_cov(c: &M4, dt: f64, q: f64) -> M4 {
    let mut fp = *c;
    for i in 0..2 {
        for j in 0..4 {
            fp[i][j] = c[i][j] + dt * c[i + 2][j];
        }
    }
    let mut out = fp;
    for row in &mut out {
        for j in 0..2 {
            row[j] += dt * row[j + 2];
        }
    }
    let (q3, q2, q1) = (q * dt.powi(3) / 3.0, q * dt * dt / 2.0, q * dt);
    for a in 0..2 {
        out[a][a] += q3;
        out[a][a + 2] += q2;
        out[a + 2][a] += q2;
        out[a + 2][a + 2] += q1;
    }
    out
}

/// One linear measurement of the state: one or two rows of `H` (`m`), each with its value and
/// its variance (independent).
#[derive(Debug, Clone, Copy)]
struct Meas {
    h: [V4; 2],
    z: [f64; 2],
    r: [f64; 2],
    m: usize,
}

impl Meas {
    /// A position fix (east, north m) with variance `r` each way.
    fn position(z: [f64; 2], r: f64) -> Meas {
        Meas { h: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]], z, r: [r, r], m: 2 }
    }

    /// A velocity (an AIS report's course and speed over ground) with variance `r` each way.
    fn velocity(z: [f64; 2], r: f64) -> Meas {
        Meas { h: [[0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]], z, r: [r, r], m: 2 }
    }

    /// The contact is on a line: its offset across it (along the unit `u`) is `z`, give or take
    /// `r` (a 2D radar's bearing to an aircraft whose height it did not measure).
    fn line(u: [f64; 2], z: f64, r: f64) -> Meas {
        Meas { h: [[u[0], u[1], 0.0, 0.0], [0.0; 4]], z: [z, 0.0], r: [r, 0.0], m: 1 }
    }
}

/// A measurement against a prediction: its innovation `y`, `S^-1`, `P H'` (by column), the
/// normalized square (NIS) and `ln det S`.
struct Innovation {
    y: [f64; 2],
    sinv: [[f64; 2]; 2],
    ph: [V4; 2],
    nis: f64,
    logdet: f64,
}

fn innovate(x: &V4, c: &M4, me: &Meas) -> Option<Innovation> {
    let m = me.m;
    let (mut y, mut ph, mut s) = ([0.0; 2], [[0.0; 4]; 2], [[0.0; 2]; 2]);
    for (i, h) in me.h.iter().enumerate().take(m) {
        y[i] = me.z[i] - dot(*h, *x);
        ph[i] = mat_vec(c, *h);
    }
    for (i, row) in s.iter_mut().enumerate().take(m) {
        for (j, v) in row.iter_mut().enumerate().take(m) {
            *v = dot(me.h[i], ph[j]);
        }
        row[i] += me.r[i];
    }
    let (sinv, det) = if m == 1 {
        ([[1.0 / s[0][0], 0.0], [0.0, 0.0]], s[0][0])
    } else {
        let det = s[0][0] * s[1][1] - s[0][1] * s[1][0];
        ([[s[1][1] / det, -s[0][1] / det], [-s[1][0] / det, s[0][0] / det]], det)
    };
    if !(det > 0.0 && det.is_finite()) {
        return None;
    }
    let mut nis = 0.0;
    for i in 0..m {
        for j in 0..m {
            nis += y[i] * sinv[i][j] * y[j];
        }
    }
    Some(Innovation { y, sinv, ph, nis, logdet: det.ln() })
}

/// One of the filter's two models: its state (east and north of the track's estimate, m;
/// velocity, m/s), the state's covariance, and the model's probability.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Model {
    x: V4,
    c: M4,
    p: f64,
}

impl Model {
    /// Carried `dt` seconds forward with white-noise acceleration `q`.
    fn predicted(&self, dt: f64, q: f64) -> Model {
        Model {
            x: [self.x[0] + self.x[2] * dt, self.x[1] + self.x[3] * dt, self.x[2], self.x[3]],
            c: predict_cov(&self.c, dt, q),
            p: self.p,
        }
    }

    /// The Kalman update with `me`, and the measurement's log-likelihood under this model's
    /// prediction as it was. `jump`: a measurement outside the model's 99% widens its prior by
    /// the innovation's own size first ([`NIS_JUMP`]).
    fn update(&mut self, me: &Meas, jump: bool) -> f64 {
        let Some(mut inn) = innovate(&self.x, &self.c, me) else {
            return 0.0;
        };
        let m = me.m as f64;
        let loglik = -0.5 * inn.nis - 0.5 * m * (std::f64::consts::TAU).ln() - 0.5 * inn.logdet;
        let limit = if me.m == 1 { NIS_JUMP_1 } else { NIS_JUMP };
        if jump && inn.nis > limit {
            let k = inn.nis / m;
            for row in &mut self.c {
                for v in row.iter_mut() {
                    *v *= k;
                }
            }
            match innovate(&self.x, &self.c, me) {
                Some(again) => inn = again,
                None => return loglik,
            }
        }
        // K = P H' S^-1; x += K y; P -= K H P
        let mut k = [[0.0; 2]; 4];
        for (a, ka) in k.iter_mut().enumerate() {
            for (j, kaj) in ka.iter_mut().enumerate().take(me.m) {
                *kaj = (0..me.m).map(|i| inn.ph[i][a] * inn.sinv[i][j]).sum();
            }
        }
        for ((xa, ca), ka) in self.x.iter_mut().zip(self.c.iter_mut()).zip(k) {
            *xa += (0..me.m).map(|j| ka[j] * inn.y[j]).sum::<f64>();
            for (b, cab) in ca.iter_mut().enumerate() {
                *cab -= (0..me.m).map(|j| ka[j] * inn.ph[j][b]).sum::<f64>();
            }
        }
        for a in 0..4 {
            for b in a + 1..4 {
                let v = 0.5 * (self.c[a][b] + self.c[b][a]);
                self.c[a][b] = v;
                self.c[b][a] = v;
            }
        }
        loglik
    }

    /// One radar's bearing alone does not range an aircraft: the track is on the bearing line
    /// from the radar at `o` (unit direction `w`), at most `hi` metres out (its ground range is
    /// no more than its slant range, whatever its height), at the segment's middle give or take
    /// its spread ([`SEGMENT_SIGMAS`]), and its velocity is unknown again (none, give or take
    /// `vel_sigma` each way, nothing correlated): a velocity from bearings at a range it does
    /// not know would be wrong by as much as the range is (found while fixing the review of
    /// 10-07: one kept from such bearings, 100 m/s off, carried a small UAS's track 1.9 km in
    /// 10 s once EO/IR ranged it again). The
    /// across-line estimate (the bearing's) is kept.
    fn reset_to_segment(&mut self, o: [f64; 2], w: [f64; 2], hi: f64, vel_sigma: f64) {
        let mu = self.x[0] * w[0] + self.x[1] * w[1] - (w[0] * o[0] + w[1] * o[1]);
        let spread = hi / SEGMENT_SIGMAS;
        let dx = 0.5 * hi - mu;
        self.x[0] += w[0] * dx;
        self.x[1] += w[1] * dx;
        let u = [w[1], -w[0]];
        let across = u[0] * u[0] * self.c[0][0] + 2.0 * u[0] * u[1] * self.c[0][1] + u[1] * u[1] * self.c[1][1];
        let along = spread * spread;
        let vv = vel_sigma * vel_sigma;
        self.x[2] = 0.0;
        self.x[3] = 0.0;
        self.c = [
            [along * w[0] * w[0] + across * u[0] * u[0], along * w[0] * w[1] + across * u[0] * u[1], 0.0, 0.0],
            [along * w[0] * w[1] + across * u[0] * u[1], along * w[1] * w[1] + across * u[1] * u[1], 0.0, 0.0],
            [0.0, 0.0, vv, 0.0],
            [0.0, 0.0, 0.0, vv],
        ];
    }

    /// A ranged track (another sensor's fix, or another radar's bearing across this one, within
    /// [`RANGE_FRESH_S`]) kept on the segment the slant range bounds: an estimate more than
    /// [`SLANT_CLAMP_SIGMAS`] of the radar's own error beyond the slant range (or behind the
    /// radar) is moved to that bound and its spread along the line widened by the move, its
    /// velocity untouched (a constraint, not a measurement: it says where the aircraft is not).
    fn keep_on_segment(&mut self, o: [f64; 2], w: [f64; 2], rho: f64, sigma_r: f64) {
        let mu = self.x[0] * w[0] + self.x[1] * w[1] - (w[0] * o[0] + w[1] * o[1]);
        let (lo, hi) = (-SLANT_CLAMP_SIGMAS * sigma_r, rho + SLANT_CLAMP_SIGMAS * sigma_r);
        if (lo..=hi).contains(&mu) {
            return;
        }
        let dx = mu.clamp(lo, hi) - mu;
        self.x[0] += w[0] * dx;
        self.x[1] += w[1] * dx;
        let a = dx * dx;
        self.c[0][0] += a * w[0] * w[0];
        self.c[0][1] += a * w[0] * w[1];
        self.c[1][0] += a * w[0] * w[1];
        self.c[1][1] += a * w[1] * w[1];
    }

    /// `q` more white-noise acceleration along the unit `w` over `dt` seconds: what bearings
    /// cannot see (an aircraft speeding up, slowing or turning toward or away from the radar).
    fn widen_along(&mut self, w: [f64; 2], q: f64, dt: f64) {
        if q <= 0.0 || dt <= 0.0 {
            return;
        }
        let (q3, q2, q1) = (q * dt.powi(3) / 3.0, q * dt * dt / 2.0, q * dt);
        for a in 0..2 {
            for b in 0..2 {
                let ww = w[a] * w[b];
                self.c[a][b] += q3 * ww;
                self.c[a][b + 2] += q2 * ww;
                self.c[a + 2][b] += q2 * ww;
                self.c[a + 2][b + 2] += q1 * ww;
            }
        }
    }
}

/// The models' mixture: mean state and covariance (with the spread of their means).
fn mixture(models: &[Model; 2]) -> (V4, M4) {
    let mut x = [0.0; 4];
    for m in models {
        for (xk, mk) in x.iter_mut().zip(m.x) {
            *xk += m.p * mk;
        }
    }
    let mut c = [[0.0; 4]; 4];
    for m in models {
        let d = [m.x[0] - x[0], m.x[1] - x[1], m.x[2] - x[2], m.x[3] - x[3]];
        for a in 0..4 {
            for b in 0..4 {
                c[a][b] += m.p * (m.c[a][b] + d[a] * d[b]);
            }
        }
    }
    (x, c)
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
        let (x, c) = mixture(&[Model { p: w[0], ..models[0] }, Model { p: w[1], ..models[1] }]);
        out[j] = Model { x, c, p: cbar };
    }
    out
}

/// A 2D radar's fix of an aircraft, in metres east and north of the track's estimate: the
/// radar at `o`, the unit direction `w` of its bearing, its plot `rho` metres out (the slant
/// range).
#[derive(Debug, Clone, Copy)]
struct Slant {
    o: [f64; 2],
    w: [f64; 2],
    rho: f64,
}

impl Slant {
    /// From the fix at `z` (east, north m) and the radar at `o`.
    fn new(o: [f64; 2], z: [f64; 2]) -> Slant {
        let (we, wn) = (z[0] - o[0], z[1] - o[1]);
        let rho = we.hypot(wn);
        let w = if rho > 1e-6 { [we / rho, wn / rho] } else { [0.0, 1.0] };
        Slant { o, w, rho }
    }

    /// The bearing as a measurement: the contact on the line through the radar along `w`, its
    /// offset across it the radar's own error at its plot (at the contact's ground range,
    /// which is shorter, it is smaller).
    fn line(&self, r: f64) -> Meas {
        let u = [self.w[1], -self.w[0]];
        Meas::line(u, u[0] * self.o[0] + u[1] * self.o[1], r)
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
    /// the filter's two models at `last_seen_t`, relative to `lat`, `lon` (their mixture)
    models: [Model; 2],
    /// the velocity every `VEL_SAMPLE_S` over `MANOEUVRE_MEMORY_S`, while known: (wall s,
    /// east, north, its covariance: east, east-north, north)
    vel_hist: VecDeque<(f64, f64, f64, [f64; 3])>,
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
    /// when a fix last ranged it (a position, or another radar's bearing across a 2D radar's),
    /// by the fixes' clock
    ranged_t: Option<f64>,
    /// when a position fix last came (not a 2D radar's bearing), by the fixes' clock
    fixed_t: Option<f64>,
    /// each observer's last 2D radar bearing of it: (uid, when, unit direction east, north)
    bearings: Vec<(u64, f64, [f64; 2])>,

    /// 1 sigma at `last_seen_t`, in its least certain direction
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
        let r = d.sigma_m * d.sigma_m;
        // a 2D radar's fix of an aircraft says only that it is on the bearing line within the
        // slant range: the track starts in the middle of that segment, with its spread
        // (`Model::reset_to_segment`)
        let (lat, lon, pos) = match d.slant_from {
            Some((olat, olon)) => {
                let m_per_lon = 111_320.0 * olat.to_radians().cos();
                let s = Slant::new([0.0, 0.0], [(d.lon - olon) * m_per_lon, (d.lat - olat) * 111_320.0]);
                let hi = s.rho + SLANT_SIGMAS * d.sigma_m;
                let a = (hi / SEGMENT_SIGMAS).powi(2);
                let (u, w) = ([s.w[1], -s.w[0]], s.w);
                (
                    olat + 0.5 * hi * w[1] / 111_320.0,
                    olon + 0.5 * hi * w[0] / m_per_lon,
                    [
                        [a * w[0] * w[0] + r * u[0] * u[0], a * w[0] * w[1] + r * u[0] * u[1]],
                        [a * w[0] * w[1] + r * u[0] * u[1], a * w[1] * w[1] + r * u[1] * u[1]],
                    ],
                )
            }
            None => (d.lat, d.lon, [[r, 0.0], [0.0, r]]),
        };
        let vv = initial_vel_sigma(d.domain).powi(2);
        let mut t = Track {
            id,
            no,
            domain: d.domain,
            lat,
            lon,
            alt_m: d.alt_m,
            alt_t: None,
            vel: None,
            models: [0, 1].map(|j| Model {
                x: [0.0; 4],
                c: [
                    [pos[0][0], pos[0][1], 0.0, 0.0],
                    [pos[1][0], pos[1][1], 0.0, 0.0],
                    [0.0, 0.0, vv, 0.0],
                    [0.0, 0.0, 0.0, vv],
                ],
                p: SOJOURN_S[j] / (SOJOURN_S[0] + SOJOURN_S[1]),
            }),
            vel_hist: VecDeque::new(),
            pos_hist: VecDeque::new(),
            manoeuvre: None,
            nis_avg: 2.0,
            inflate: 1.0,
            fix_t: d.measured_t,
            ranged_t: None,
            fixed_t: None,
            bearings: Vec::new(),
            sigma_m: pos_var(&[
                [pos[0][0], pos[0][1], 0.0, 0.0],
                [pos[1][0], pos[1][1], 0.0, 0.0],
                [0.0; 4],
                [0.0; 4],
            ])
            .sqrt(),
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
        let t_fix = d.measured_t.unwrap_or(now);
        match d.slant_from {
            Some((olat, olon)) => {
                let m_per_lon = 111_320.0 * olat.to_radians().cos();
                let s = Slant::new([0.0, 0.0], [(d.lon - olon) * m_per_lon, (d.lat - olat) * 111_320.0]);
                self.bearings.push((d.uid, t_fix, s.w));
            }
            None => {
                self.ranged_t = Some(t_fix);
                self.fixed_t = Some(t_fix);
            }
        }
        if let Some((e, n)) = d.velocity_mps {
            for m in &mut self.models {
                m.x[2] = e;
                m.x[3] = n;
                m.c[2][2] = AIS_VEL_SIGMA_MPS * AIS_VEL_SIGMA_MPS;
                m.c[3][3] = AIS_VEL_SIGMA_MPS * AIS_VEL_SIGMA_MPS;
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

    /// 1-sigma horizontal uncertainty at `now`, in its least certain direction (the same every
    /// way unless a 2D radar's bearing line made it longer one way than another): the
    /// filter's, each model carried forward with its own process noise, and their mixture.
    pub fn sigma_at(&self, now: f64) -> f64 {
        let dt = self.age_s(now);
        let q = process_noise(self.domain);
        let m = [self.models[0].predicted(dt, q[0]), self.models[1].predicted(dt, q[1])];
        let (_, c) = mixture(&m);
        (pos_var(&c) * self.inflate).sqrt()
    }

    /// The 1-sigma error of the velocity estimate in its least certain direction, m/s, as
    /// reported (widened by the fixes' consistency, as the position's is).
    pub fn vel_sigma_mps(&self) -> f64 {
        let (_, c) = mixture(&self.models);
        (vel_var(&c) * self.inflate).sqrt()
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
        // a position, or, from a 2D radar's fix of an aircraft, its bearing line (the plot is at
        // the slant range, which the unmeasured height lengthens: only the bearing locates it,
        // and the slant range bounds how far out along it)
        let slant = d
            .slant_from
            .map(|(olat, olon)| Slant::new([(olon - self.lon) * m_per_lon, (olat - self.lat) * 111_320.0], z));
        let me = match &slant {
            Some(s) => s.line(r),
            None => Meas::position(z, r),
        };
        // ranged: a position fix now or within RANGE_FRESH_S, or another radar's bearing across
        // this one within it (the two lines cross: the filter's own update triangulates)
        let t_fix = d.measured_t.unwrap_or(now);
        let fresh = |t: f64| (0.0..=RANGE_FRESH_S).contains(&(t_fix - t));
        let ranged = match &slant {
            None => {
                self.ranged_t = Some(t_fix);
                self.fixed_t = Some(t_fix);
                true
            }
            Some(s) => {
                // another radar's bearing across this one ranged it when it came (the filter
                // folded it in then): the pair triangulates
                let crossing = CROSSING_DEG.to_radians().sin();
                let crossed = self
                    .bearings
                    .iter()
                    .filter(|&&(uid, t, w)| uid != d.uid && fresh(t) && (w[0] * s.w[1] - w[1] * s.w[0]).abs() >= crossing)
                    .map(|b| b.1)
                    .fold(None, |a: Option<f64>, t| Some(a.map_or(t, |a| a.max(t))));
                if let Some(t) = crossed {
                    self.ranged_t = Some(self.ranged_t.map_or(t, |r| r.max(t)));
                }
                self.bearings.retain(|b| b.0 != d.uid && fresh(b.1));
                self.bearings.push((d.uid, t_fix, s.w));
                self.ranged_t.is_some_and(fresh)
            }
        };
        // bearings cannot see a manoeuvre along the line: once no position fix has come for
        // ALONG_GRACE_S, both models grow along it as an aircraft turning hard would
        if let (Some(s), true) = (&slant, ranged) {
            let from = (t_fix - dt).max(self.fixed_t.unwrap_or(f64::NEG_INFINITY) + ALONG_GRACE_S);
            for (j, m) in models.iter_mut().enumerate() {
                m.widen_along(s.w, (ALONG_Q - q[j]).max(0.0), t_fix - from);
            }
        }
        let prior_pp: f64 = models.iter().map(|m| m.p * 0.5 * (m.c[0][0] + m.c[1][1])).sum();
        // the fix against the mixture's prediction: the reported covariance's widening
        // (expected 2 a fix: a bearing line, of one dimension, counts double)
        let (x, c) = mixture(&models);
        if let Some(inn) = innovate(&x, &c, &me) {
            let nis = inn.nis * 2.0 / me.m as f64;
            self.nis_avg = (1.0 - REPORT_ALPHA) * self.nis_avg + REPORT_ALPHA * nis;
            self.inflate = (self.nis_avg / REPORT_NIS).max(1.0);
        }
        let mut loglik = [0.0; 2];
        for (j, m) in models.iter_mut().enumerate() {
            loglik[j] = m.update(&me, j == 1);
            // AIS: the contact's own course and speed over ground, a velocity measurement
            if let Some((ae, an)) = d.velocity_mps {
                loglik[j] += m.update(&Meas::velocity([ae, an], AIS_VEL_SIGMA_MPS * AIS_VEL_SIGMA_MPS), false);
            }
            if let Some(s) = &slant {
                if ranged {
                    m.keep_on_segment(s.o, s.w, s.rho, d.sigma_m);
                } else {
                    m.reset_to_segment(s.o, s.w, s.rho + SLANT_SIGMAS * d.sigma_m, initial_vel_sigma(self.domain));
                }
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
        let (x, _) = mixture(&models);
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
        let (x, c) = mixture(&self.models);
        self.sigma_m = (pos_var(&c) * self.inflate).sqrt();
        let known = vel_var(&c).sqrt() < vel_known_sigma(self.domain);
        self.vel = known.then_some((x[2], x[3]));
        if !known {
            return;
        }
        let v = [c[2][2], c[2][3], c[3][3]];
        self.vel_hist.retain(|s| now - s.0 <= MANOEUVRE_MEMORY_S);
        let change = self
            .vel_hist
            .iter()
            .filter_map(|&(_, ve, vn, sv)| {
                let (de, dn) = (x[2] - ve, x[3] - vn);
                let dv = de.hypot(dn);
                // the change against both estimates' errors (chi-square, two dimensions)
                let (a, b, cc) = (v[0] + sv[0], v[1] + sv[1], v[2] + sv[2]);
                let det = a * cc - b * b;
                let d2 = if det > 0.0 { (cc * de * de - 2.0 * b * de * dn + a * dn * dn) / det } else { 0.0 };
                (dv > MANOEUVRE_MIN_MPS && d2 > MANOEUVRE_CHI2).then_some(dv)
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
            self.vel_hist.push_back((now, x[2], x[3], v));
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
        let (x, c) = mixture(&self.models);
        let sv = vel_var(&c).sqrt();
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
            // rules 14: how far off the velocity may be, for an aircraft's reach
            velocity_sigma_mps: self.vel.map(|_| (self.vel_sigma_mps() * 100.0).round() / 100.0),
        }
    }
}
