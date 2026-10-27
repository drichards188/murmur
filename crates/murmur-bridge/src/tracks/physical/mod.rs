//! The physical sensing model (WORLD-V3-PLAN section 2, docs/SENSING.md): per-sensor physics
//! in place of the legacy model's range curve, chosen per contacts scenario (`[sensing] model =
//! "physical"`) or by `MURMUR_SENSE_MODEL`. It turns the same truth into the same
//! [`Detection`]s for the same tracker, so the decision engine, replay and the tracker's facts
//! are untouched; what changes is which detections come, how often, how good, and the
//! identification evidence each carries ([`Detection::look`]).
//!
//! - **Geometry** ([`geometry`]): optical and radio horizons with hull-down, terrain line of
//!   sight from the scenario's grid (earth curvature alone without one, said once), cached per
//!   observer and contact and refreshed when either end moves 50 m or after 5 s.
//! - **EO/IR** ([`eo`]): each channel's pixels on target through the ACQUIRE curve, the
//!   atmosphere, the light and the thermal crossover; a contact is seen only inside the
//!   field of view the payload manager points ([`Gimbal`]: search the task area in the wide
//!   field, cue to the nearest unidentified radar, AIS or EO track there, then the stalest, an
//!   aircraft whose height no sensor measured searched for in elevation, lock on in the narrow
//!   field to identify while it stays the manager's choice, the image tracker following it,
//!   back to search; each camera its own history). Each level of a look accrues as a hazard
//!   while the contact is in the field, ACQUIRE's initial rate at this second's range and light
//!   (tau 3.4 / P s to detect); identification needs the narrow field on it for 2 to 3 s. A
//!   detection gives the size class, a recognition the family, an identification the kind; a
//!   wrong call is likelier at recognition.
//! - **Radar** ([`radar`]): one Swerling 1 draw per scan as the beam passes the contact, the
//!   fourth-root law about the radar's reference, the two-ray model for low targets, sea
//!   clutter in the radar's range cell by grazing angle, sea state, band and wind (a radar with a
//!   Doppler mode sees a contact moving clear of it); 30 m or 1 % of range, and 1 degree in
//!   bearing.
//! - **AIS** and **ESM** ([`rf`]): reports at the transponder class's interval within the VHF
//!   horizon, decoded by slot load; emitters heard over the receiver's sensitivity in radio
//!   line of sight.
//! - **Sonar** ([`sonar`]): the sonar equation with each sensor's figure of merit per kind from
//!   its reference ranges; passive classifies over minutes of signal heard, active localises;
//!   an active sonar may listen too (its passive mode).
//! - **Navigation** (murmur-k8i): every sensor sees from where its vehicle is ([`Observer`]'s
//!   `lat`, `lon`); the vehicle lays its radar's plots, its cameras', sonars' and ESM's fixes off
//!   from where it believes it is ([`Observer::geolocate`]: its navigation error, a GNSS
//!   spoofer's walk or a dead reckoning's drift, rides on every fix but AIS's, the contact's own
//!   GPS), and its payload manager points at a cue or its search from there; a lock follows its
//!   contact in the image, where the vehicle's navigation plays no part.
//!
//! Deterministic as the legacy model: every draw is a hash of the seed, a tick or a scan or a
//! look's start, the vehicle, the contact and the sensor; state (pointing, looks, scans) moves
//! only with the scenario seconds of the truth messages.

pub mod conditions;
pub mod eo;
pub mod geometry;
// the headless simulator serves it to Python (`sense`); the bridge itself does not call it
#[allow(dead_code)]
pub mod predict;
pub mod radar;
pub mod rf;
pub mod signature;
pub mod sonar;
pub mod spec;

use std::collections::{BTreeSet, BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use self::conditions::Conditions;
use self::geometry::{Grid, K_OPTICAL, K_RADIO, M_PER_DEG, en, visible_height_m};
use self::signature::{AisClass, Signature, family_confusion, signature, size_class};
use self::spec::{EoSensor, EsmSensor, RadarSensor, Resolved, SonarMode, SonarSensor, resolve};
use super::sensing::{
    ActFix, ActObservation, AisHeard, Detection, Level, Look, Observer, SearchArea, SensorType, confusion, family, normal, uniform,
};
use super::truth::{ContactTruth, Domain, Sensing};
use crate::geo::haversine_m;

/// A line of sight is recomputed when either end has moved this far, m, or after
/// [`LOS_MAX_AGE_S`].
const LOS_MOVE_M: f64 = 50.0;
const LOS_MAX_AGE_S: f64 = 5.0;
/// A gimbal settles this long after a slew or a zoom, s (research [E]: about 1 s).
const SETTLE_S: f64 = 1.0;
/// A search step dwells this long on one aim point, s [E]: about the time to detect a clear
/// contact (tau 3.4 s) after the settle.
const SEARCH_DWELL_S: f64 = 5.0;
/// The payload manager searches at least this long between two cues, s [E].
const SEARCH_MIN_S: f64 = 10.0;
/// A cue that finds nothing in the wide field gives up after this long, s [E]; a lock gives up
/// this long after the identification it waits for last got likelier (the contact closing, the
/// light rising), s [E: the lock's longest before, now its patience without progress].
const CUE_MAX_S: f64 = 8.0;
const LOCK_MAX_S: f64 = 10.0;
/// A cue is a track fixed within this long, s (the tracker's stale age).
const CUE_FRESH_S: f64 = 10.0;
/// A cued camera holds its aim while the cue stays within this share of its field's width of
/// the boresight (the contact stays well inside the field), and slews (and settles) only when
/// the cue leaves it: it does not chase a track's every update [E] (murmur-7vp).
const CUE_DEADBAND: f64 = 0.25;
/// The band of heights an aircraft whose height no sensor measured may fly at, m: a camera cued
/// to it searches in elevation from just above the surface to the ceiling, or to the height its
/// 2D radar plot's slant range allows if lower [E here: the ceiling is above the service
/// ceilings of the light aircraft and helicopters the air kinds stand for, 4 to 6 km, and the
/// 7,000 m the engagement scenarios' highest aircraft fly] (murmur-7vp).
const AIR_FLOOR_M: f64 = 10.0;
const AIR_CEILING_M: f64 = 8_000.0;
/// Each bar of a cue's elevation search is stared at this long once the gimbal has settled, s
/// [E]: about the time to detect a clear contact (tau 3.4 s), as a search step's dwell.
const BAR_STARE_S: f64 = 4.0;
/// An identified contact is not cued again for this long, s; one looked at and not identified
/// for [`RETRY_S`]; with nothing unidentified to cue, the stalest look older than
/// [`STALE_LOOK_S`] is revisited [E].
const REVISIT_S: f64 = 300.0;
const RETRY_S: f64 = 60.0;
const STALE_LOOK_S: f64 = 120.0;
/// A recognition needs the contact detected this long first, s [E].
const RECOGNISE_AFTER_S: f64 = 1.0;
/// Identification needs the narrow field on the contact for 2 to 3 s (research [E]): at least
/// 2 s, then the level's hazard ([`LEVEL_TAU_S`]).
const IDENTIFY_DWELL_S: f64 = 2.0;
/// Each level of an EO/IR look accrues as a hazard while the contact is in the field (a
/// look's evidence grows with the seconds it holds the contact, at the current range and
/// light): ACQUIRE's time law's initial rate, `P / tau = P^2 / T` a second with `tau = T / P`
/// [12], so a short look is ACQUIRE's `P (1 - exp(-t / tau))` and a longer one keeps
/// accruing where one draw a look stopped at `P`. Detection's `T` is the NVL constant
/// ([`eo::TAU_K_S`], 3.4 s); recognition's and identification's this, once their own dwell
/// is met [E here: at P = 1 the level comes in the first second its dwell allows two times in
/// three, so a recognition about a second after the detection and an identification in 2 to
/// 3 s of the narrow field, as the research's [E] times].
const LEVEL_TAU_S: f64 = 1.0;
/// A sensor's run of hits on a contact ends after this gap, s (its dwell starts again).
const DWELL_GAP_S: f64 = 30.0;
/// A passive sonar recognises after this many seconds of signal heard, and identifies after
/// [`PASSIVE_IDENTIFY_S`] (research: passive spectra separate ship, submarine and UUV after
/// minutes [E]); an active sonar recognises by the track [27] and never identifies. Seconds
/// heard, not of contact: a classifier integrates the signal it receives, so a contact heard
/// one second in twenty takes twenty times as long (each detection counts the draw interval it
/// closes, a second or a scan; a run's first counts a second).
const PASSIVE_RECOGNISE_S: f64 = 60.0;
const PASSIVE_IDENTIFY_S: f64 = 180.0;
const ACTIVE_RECOGNISE_S: f64 = 60.0;

/// What a look at the given level calls a contact with a wrong call's rate: a recognition is
/// wrong twice as often as an identification, which is wrong at the scenario's
/// misclassification rate (WORLD-V3-PLAN 2.3) [E].
fn level_rate(rate: f64, level: Level) -> f64 {
    match level {
        Level::Detect => 0.0,
        Level::Recognise => (2.0 * rate).min(0.5),
        Level::Identify => rate,
    }
}

/// A draw's salt in the physical model: the sensor's index and type, and what it is for
/// (apart from every salt the legacy model uses).
fn salt(si: usize, kind: SensorType, what: u64) -> u64 {
    0x5000_0000 | (what << 16) | ((si as u64) << 8) | kind as u64
}

/// One step's settings: the truth message's seed, scenario second (its tick) and clock, the
/// misclassification rate and the scenario's sensing section.
pub struct Step<'a> {
    pub seed: u64,
    pub tick: i64,
    /// scenario seconds since the start (durations)
    pub s: f64,
    /// unix seconds of the scenario clock (the sun)
    pub t: f64,
    pub misclassification_rate: f64,
    pub sensing: &'a Sensing,
}

/// A contact as the sensors see it this tick.
pub struct Target<'a> {
    pub c: &'a ContactTruth,
    pub sig: Signature,
    /// its top above the sea (an aircraft's altitude; a ground contact's ground plus its
    /// height; a periscope's mast), m, and the height that counts for hull-down
    pub top_msl: f64,
    pub vis_height_m: f64,
    /// something of it stands above the water or the ground (radar, EO/IR, ESM can see it)
    pub above: bool,
    pub size: &'static str,
}

impl<'a> Target<'a> {
    pub fn new(c: &'a ContactTruth, over: Option<&signature::SignatureOverride>, terrain: Option<&Grid>) -> Target<'a> {
        let sig = signature(&c.kind, c.domain, c.speed_mps, over);
        let (top_msl, vis_height_m, above) = match c.domain {
            Domain::Air => (c.alt_m.max(1.0), sig.height_m.max(0.5), true),
            Domain::Surface => (sig.height_m, sig.height_m, sig.dc_m > 0.0 || sig.rcs_m2 > 0.0),
            Domain::Ground => {
                let ground = terrain.and_then(|g| g.height(c.lat, c.lon)).unwrap_or(0.0);
                (ground + sig.height_m, sig.height_m, true)
            }
            // a periscope's mast above the water; anything else wholly under it
            Domain::Subsurface if sig.dc_m > 0.0 => (sig.height_m, sig.height_m, true),
            Domain::Subsurface => (c.alt_m, 0.0, false),
        };
        Target { c, sig, top_msl, vis_height_m, above, size: size_class(&sig, c.domain) }
    }
}

/// An observer's sensor height above the sea, m: an aircraft's altitude, else its own height
/// (a hill's for a ground vehicle) plus its mast (as the legacy model's horizon).
pub fn sensor_height_m(o: &Observer) -> f64 {
    if o.alt_msl > 20.0 { o.alt_msl } else { o.alt_msl.max(0.0) + o.mast_m }
}

/// The geometry of one observer and one contact.
#[derive(Debug, Clone, Copy)]
pub struct Pair {
    pub ground_m: f64,
    pub slant_m: f64,
    /// bearing from the observer, radians from north
    pub bearing: f64,
    /// elevation of the contact's top from the sensor, radians
    pub el: f64,
}

pub fn pair(o: &Observer, h_s: f64, t: &Target) -> Pair {
    let ground = haversine_m(o.lat, o.lon, t.c.lat, t.c.lon);
    let (de, dn) = en(o.lat, o.lon, t.c.lat, t.c.lon);
    let dz = t.top_msl - h_s;
    Pair { ground_m: ground, slant_m: ground.hypot(dz), bearing: de.atan2(dn), el: dz.atan2(ground.max(1.0)) }
}

/// What stands above the horizon of radius factor `k` (hull-down) and clear of the terrain:
/// the visible height, or None.
pub fn sight(h_s: f64, t: &Target, p: &Pair, k: f64, terrain_obstruction: Option<f64>) -> Option<f64> {
    if !t.above {
        return None;
    }
    let vis = visible_height_m(t.top_msl, t.vis_height_m, p.ground_m, h_s, k);
    if vis <= 0.0 || terrain_obstruction.is_some_and(|x| x > 0.0) {
        return None;
    }
    Some(vis)
}

/// A cached terrain line of sight: both ends when it was computed, when, and how far the
/// terrain stood over the ray at k = 7/6 and 4/3.
struct Los {
    a: (f64, f64, f64),
    b: (f64, f64, f64),
    s: f64,
    obstruction: [f64; 2],
}

fn moved_m(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
    let (de, dn) = en(a.0, a.1, b.0, b.1);
    de.hypot(dn).hypot(a.2 - b.2)
}

/// What a payload manager is doing with its camera.
#[derive(Debug, Clone, PartialEq)]
enum Mode {
    Search,
    /// slewing the wide field to a track's position, for a contact
    Cue(String),
    /// the narrow field locked on a contact (the image tracker keeps it centred)
    Lock(String),
}

/// One EO/IR sensor's simulated payload manager (decision W1: it points by itself; orders do
/// not name a look target).
#[derive(Debug, Clone)]
pub struct Gimbal {
    az: f64,
    el: f64,
    narrow: bool,
    busy_until: f64,
    mode: Mode,
    since: f64,
    step: usize,
    step_s: f64,
    seen_s: f64,
    /// a cue's search pattern (one aim at the contact, or an aircraft's elevation bars), held
    /// while the cue stays within it; the bar it is on, and when its stare began (the gimbal
    /// settled)
    pattern: Vec<(f64, f64)>,
    bar: usize,
    bar_s: f64,
}

/// Where a camera looks this tick.
#[derive(Debug, Clone, Copy)]
struct Boresight {
    az: f64,
    el: f64,
    narrow: bool,
    /// slewing or settling: it sees nothing
    blind: bool,
    /// fixed to the vehicle (no gimbal): its narrow field is its wide one
    fixed: bool,
}

/// One EO/IR look at a contact: from when it entered the field, when it was detected (and how
/// likely that was then), since when the narrow field has held it, each level's hazard accrued
/// ([`LEVEL_TAU_S`]) and whether it reached recognition and identification, and the best
/// identification chance it has had and when that last rose (a lock's progress).
#[derive(Debug, Clone)]
struct EoLook {
    start_tick: i64,
    detected_s: Option<f64>,
    detected_p: f64,
    narrow_since: Option<f64>,
    accrued: [f64; 3],
    recognised: bool,
    identified: bool,
    id_best: f64,
    id_best_s: f64,
    seen_s: f64,
}

/// A run of hits by a scanning or listening sensor: its first and last.
#[derive(Debug, Clone, Copy)]
struct Dwell {
    first_s: f64,
    last_s: f64,
}

/// A sonar's run of detections of a contact: its last, and the seconds of signal the run has
/// heard (the evidence its classification integrates).
#[derive(Debug, Clone, Copy)]
struct Heard {
    last_s: f64,
    signal_s: f64,
}

/// A track position a camera may be cued to (a radar's, AIS's or a camera's own fix).
#[derive(Debug, Clone, Copy)]
struct Cue {
    lat: f64,
    lon: f64,
    /// its height, m, and when it was measured: a surface or ground contact's (the surface), a
    /// height-finding radar's of an aircraft (kept while fresh); None: an aircraft no sensor
    /// gave a height
    alt: Option<(f64, f64)>,
    /// a 2D radar's plot of an aircraft: the radar's position and height above the sea, the
    /// plot being at the slant range on its bearing
    slant: Option<(f64, f64, f64)>,
    s: f64,
}

impl Cue {
    /// Where the contact would be at height `h`: on a 2D radar plot's slant-range arc (over the
    /// radar beyond it), else over the fix.
    fn at_height(&self, h: f64) -> (f64, f64) {
        let Some((rlat, rlon, rh)) = self.slant else {
            return (self.lat, self.lon);
        };
        let (de, dn) = en(rlat, rlon, self.lat, self.lon);
        let r = de.hypot(dn);
        let dz = h - rh;
        let f = (r * r - dz * dz).max(0.0).sqrt() / r.max(1.0);
        (rlat + f * dn / M_PER_DEG, rlon + f * de / (M_PER_DEG * rlat.to_radians().cos()))
    }

    /// The aims (azimuth, elevation, radians) a camera at `o`, `h_s` above the sea, with a wide
    /// field `wide` across, steps through for this cue: one, at the contact, where its height is
    /// known (a surface contact's, a height-finding radar's); for an aircraft no sensor gave a
    /// height, the bars of an elevation search over the heights it may fly at ([`AIR_FLOOR_M`]
    /// to [`AIR_CEILING_M`], or what the slant range allows), lowest first, a field (as wide as
    /// it is, three quarters of that high) a bar, stepping four fifths of its height
    /// (murmur-7vp).
    fn aims(&self, o: &Observer, h_s: f64, wide: f64) -> Vec<(f64, f64)> {
        let lines = self.sightlines(o, h_s);
        if self.alt.is_some() {
            return lines;
        }
        let high = 0.75 * wide;
        let mut bars: Vec<(f64, f64)> = Vec::new();
        for (az, el) in lines {
            let covered = bars.iter().any(|&(bar_az, bar_el)| {
                let daz = (az - bar_az + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI;
                daz.abs() * el.cos() <= 0.45 * wide && (el - bar_el).abs() <= 0.45 * high
            });
            if !covered {
                // a new bar with this height near its foot
                bars.push((az, (el + 0.35 * high).min(std::f64::consts::FRAC_PI_2)));
            }
        }
        bars
    }

    /// Where the contact may be, as lines of sight (azimuth, elevation, radians) from a camera at
    /// `o`, `h_s` above the sea: the one at the contact where its height is known, else one at
    /// each of the heights sampled along the band it may fly at, lowest first ([`Cue::aims`]'s
    /// bars cover them). A cue is a place: the payload manager aims at it from where its vehicle
    /// believes it is ([`Observer::believed`]).
    fn sightlines(&self, o: &Observer, h_s: f64) -> Vec<(f64, f64)> {
        /// The heights sampled along the band (each covered by a bar).
        const SAMPLES: usize = 160;
        let (olat, olon) = o.believed();
        let aim = |(lat, lon): (f64, f64), alt: f64| {
            let (de, dn) = en(olat, olon, lat, lon);
            (de.atan2(dn), (alt - h_s).atan2(de.hypot(dn).max(1.0)))
        };
        if let Some((h, _)) = self.alt {
            return vec![aim(self.at_height(h), h)];
        }
        let top = match self.slant {
            Some((rlat, rlon, rh)) => {
                let (de, dn) = en(rlat, rlon, self.lat, self.lon);
                (rh + de.hypot(dn)).min(AIR_CEILING_M)
            }
            None => AIR_CEILING_M,
        };
        (0..=SAMPLES)
            .map(|k| {
                let h = AIR_FLOOR_M + (top - AIR_FLOOR_M).max(0.0) * k as f64 / SAMPLES as f64;
                aim(self.at_height(h), h)
            })
            .collect()
    }
}

/// A grid the scenario names: its path and what loading it gave.
struct GridSlot {
    path: Option<String>,
    grid: Option<Arc<Grid>>,
}

/// The physical model's state across ticks.
pub struct Engine {
    root: Option<PathBuf>,
    terrain: GridSlot,
    bathymetry: GridSlot,
    noticed_no_terrain: bool,
    los: HashMap<u64, HashMap<String, Los>>,
    gimbals: HashMap<(u64, usize), Gimbal>,
    looks: HashMap<(u64, usize), HashMap<String, EoLook>>,
    dwell: HashMap<(u64, usize), HashMap<String, Dwell>>,
    /// each sonar's (and an active sonar's passive mode's: the third key) evidence per contact
    heard: HashMap<(u64, usize, u8), HashMap<String, Heard>>,
    cues: HashMap<String, Cue>,
    /// each camera's own history (a payload manager on its vehicle knows what it did, not what
    /// another camera did): when it identified each contact, and when it last looked at one in
    /// vain
    identified: HashMap<(u64, usize), HashMap<String, f64>>,
    looked: HashMap<(u64, usize), HashMap<String, f64>>,
    /// Rules 21: per vehicle, the contacts behind the track an identify order sends it to,
    /// which its cameras cue first (set before each step, [`Engine::set_firsts`])
    firsts: HashMap<u64, BTreeSet<String>>,
}

impl Engine {
    /// `root`: what relative grid paths are relative to (the repository).
    pub fn new(root: Option<PathBuf>) -> Engine {
        Engine {
            root,
            terrain: GridSlot { path: None, grid: None },
            bathymetry: GridSlot { path: None, grid: None },
            noticed_no_terrain: false,
            los: HashMap::new(),
            gimbals: HashMap::new(),
            looks: HashMap::new(),
            dwell: HashMap::new(),
            heard: HashMap::new(),
            cues: HashMap::new(),
            identified: HashMap::new(),
            looked: HashMap::new(),
            firsts: HashMap::new(),
        }
    }

    /// Rules 21: the contacts each vehicle's cameras cue first from now on (the contacts behind
    /// the track its identify order names; none: the payload manager's own order of cues).
    pub fn set_firsts(&mut self, firsts: HashMap<u64, BTreeSet<String>>) {
        self.firsts = firsts;
    }

    fn resolve_path(&self, p: &str) -> PathBuf {
        let path = Path::new(p);
        match &self.root {
            Some(r) if path.is_relative() => r.join(path),
            _ => path.to_path_buf(),
        }
    }

    /// The grid `want` names, loaded once per path (a grid that fails to load is said once and
    /// counts as none).
    fn load(&self, slot: &mut GridSlot, want: Option<&str>, what: &str) {
        if slot.path.as_deref() == want {
            return;
        }
        slot.path = want.map(str::to_string);
        slot.grid = want.and_then(|p| match Grid::load(&self.resolve_path(p)) {
            Ok(g) => {
                println!("sensing: physical model {what} grid {p} ({} x {} samples)", g.h.nlat, g.h.nlon);
                Some(Arc::new(g))
            }
            Err(e) => {
                println!("sensing: physical model {what} grid {p} not loaded ({e}): earth curvature only");
                None
            }
        });
    }

    /// Read the scenario's grids (on a change of path), and say once that there is no terrain.
    pub fn configure(&mut self, sensing: &Sensing) {
        let mut t = std::mem::replace(&mut self.terrain, GridSlot { path: None, grid: None });
        let before = t.path.clone();
        self.load(&mut t, sensing.terrain.as_deref(), "terrain");
        if t.path != before {
            self.los.clear();
        }
        self.terrain = t;
        let mut b = std::mem::replace(&mut self.bathymetry, GridSlot { path: None, grid: None });
        self.load(&mut b, sensing.bathymetry.as_deref(), "bathymetry");
        self.bathymetry = b;
        if self.terrain.grid.is_none() && !self.noticed_no_terrain {
            self.noticed_no_terrain = true;
            println!("sensing: physical model without a terrain grid: line of sight by earth curvature only");
        }
    }

    /// A terrain grid given directly (tests).
    #[cfg(test)]
    pub fn set_terrain(&mut self, g: Grid) {
        self.terrain = GridSlot { path: Some("test".into()), grid: Some(Arc::new(g)) };
        self.los.clear();
    }

    /// What camera `si` of `uid` is doing (diagnostics): its azimuth and elevation, radians, its
    /// narrow field, slewing or settling until when, and its mode (`search`, `cue`, `lock`).
    #[cfg(test)]
    pub fn gimbal_of(&self, uid: u64, si: usize) -> Option<(f64, f64, bool, f64, &'static str)> {
        self.gimbals.get(&(uid, si)).map(|g| {
            let mode = match g.mode {
                Mode::Search => "search",
                Mode::Cue(_) => "cue",
                Mode::Lock(_) => "lock",
            };
            (g.az, g.el, g.narrow, g.busy_until, mode)
        })
    }

    /// The terrain's obstruction of the ray from `o` to `t` at k = 7/6 and 4/3, cached per pair
    /// (refreshed when an end moves [`LOS_MOVE_M`] or after [`LOS_MAX_AGE_S`]); None: no grid.
    fn terrain_los(&mut self, o: &Observer, h_s: f64, t: &Target, s: f64) -> Option<[f64; 2]> {
        let grid = self.terrain.grid.clone()?;
        let a = (o.lat, o.lon, h_s);
        let b = (t.c.lat, t.c.lon, t.top_msl);
        let per = self.los.entry(o.uid).or_default();
        if let Some(e) = per.get(&t.c.id)
            && s - e.s < LOS_MAX_AGE_S
            && s >= e.s
            && moved_m(e.a, a) < LOS_MOVE_M
            && moved_m(e.b, b) < LOS_MOVE_M
        {
            return Some(e.obstruction);
        }
        let obstruction = grid.obstruction_m(a, b, [K_OPTICAL, K_RADIO]);
        per.insert(t.c.id.clone(), Los { a, b, s, obstruction });
        Some(obstruction)
    }

    /// One tick: every observer's every sensor against every contact.
    pub fn step(&mut self, st: &Step, observers: &[Observer], contacts: &[ContactTruth]) -> Vec<Detection> {
        let cond = st.sensing.conditions.unwrap_or_default().sane();
        let terrain = self.terrain.grid.clone();
        let targets: Vec<Target> =
            contacts.iter().map(|c| Target::new(c, st.sensing.signatures.get(&c.id), terrain.as_deref())).collect();
        let (lat0, lon0) = observers
            .first()
            .map(|o| (o.lat, o.lon))
            .or_else(|| contacts.first().map(|c| (c.lat, c.lon)))
            .unwrap_or((0.0, 0.0));
        let sun = cond.sun_at(st.t, lat0, lon0);
        let ctx = Ctx { st, cond, sun };
        let mut out = Vec::new();
        // the scanning and listening sensors first: their fixes cue this tick's cameras
        for o in observers {
            let h_s = sensor_height_m(o);
            for (si, s) in o.sensors.iter().enumerate() {
                if o.submerged && s.kind != SensorType::Sonar {
                    continue; // nothing but sonar works under water
                }
                match resolve(s, h_s > 20.0) {
                    Resolved::Radar(r) => self.radar(&ctx, o, h_s, si, &r, &targets, &mut out),
                    Resolved::Ais => self.ais(&ctx, o, h_s, si, &targets, &mut out),
                    Resolved::Sonar(sn) => {
                        self.sonar(&ctx, o, si, 0, &sn, &targets, &mut out);
                        // an active sonar whose array also listens: its passive mode
                        if let Some(p) = sn.passive.as_deref() {
                            self.sonar(&ctx, o, si, 1, p, &targets, &mut out);
                        }
                    }
                    Resolved::Esm(e) => self.esm(&ctx, o, h_s, si, &e, &targets, &mut out),
                    Resolved::EoIr(_) => {}
                }
            }
        }
        let heights: HashMap<u64, f64> = observers.iter().map(|o| (o.uid, sensor_height_m(o))).collect();
        self.cue_from(&out, st.s, &heights);
        let first_eo = out.len();
        for o in observers {
            if o.submerged {
                continue;
            }
            let h_s = sensor_height_m(o);
            for (si, s) in o.sensors.iter().enumerate() {
                if let Resolved::EoIr(eo) = resolve(s, h_s > 20.0) {
                    self.eo(&ctx, o, h_s, si, s.range_m, &eo, &targets, &mut out);
                }
            }
        }
        self.cue_from(&out[first_eo..], st.s, &heights);
        self.forget(st.s, contacts);
        out
    }

    /// Track positions to cue cameras to: every fix this tick, the contact's last. An aircraft's
    /// height is what a height-finding radar measured within [`CUE_FRESH_S`], else unknown (a 2D
    /// radar's, a camera's and ESM's fixes give none); a 2D radar's plot of it keeps the radar
    /// (`heights`: each observer's sensor height), its slant range being on the plot's bearing.
    fn cue_from(&mut self, dets: &[Detection], s: f64, heights: &HashMap<u64, f64>) {
        for d in dets {
            let alt = match d.alt_m {
                Some(a) => Some((a, s)),
                None if d.domain == Domain::Air => {
                    self.cues.get(&d.contact_id).and_then(|c| c.alt).filter(|(_, t)| s - t <= CUE_FRESH_S)
                }
                None => Some((0.0, s)),
            };
            let slant = d.slant_from.map(|(lat, lon)| (lat, lon, heights.get(&d.uid).copied().unwrap_or(0.0)));
            self.cues.insert(d.contact_id.clone(), Cue { lat: d.lat, lon: d.lon, alt, slant, s });
        }
    }

    /// What no longer matters: contacts gone, stale cues, cameras unheard of for a minute.
    fn forget(&mut self, s: f64, contacts: &[ContactTruth]) {
        let live: std::collections::HashSet<&str> = contacts.iter().map(|c| c.id.as_str()).collect();
        self.cues.retain(|k, c| live.contains(k.as_str()) && s - c.s <= 60.0);
        for per in self.looks.values_mut() {
            per.retain(|_, l| l.seen_s >= s);
        }
        for per in self.dwell.values_mut() {
            per.retain(|k, d| live.contains(k.as_str()) && s - d.last_s <= DWELL_GAP_S);
        }
        for per in self.heard.values_mut() {
            per.retain(|k, h| live.contains(k.as_str()) && s - h.last_s <= DWELL_GAP_S);
        }
        for per in self.los.values_mut() {
            per.retain(|k, l| live.contains(k.as_str()) && s - l.s <= 60.0);
        }
        self.gimbals.retain(|_, g| s - g.seen_s <= 60.0);
        for per in self.identified.values_mut() {
            per.retain(|k, t| live.contains(k.as_str()) && s - *t <= REVISIT_S);
        }
        self.identified.retain(|_, per| !per.is_empty());
        for per in self.looked.values_mut() {
            per.retain(|k, t| live.contains(k.as_str()) && s - *t <= 3600.0);
        }
        self.looked.retain(|_, per| !per.is_empty());
    }

    /// A run of hits by sensor `si` of `uid` on `contact`: its dwell now, s.
    fn hit(&mut self, uid: u64, si: usize, contact: &str, s: f64) -> f64 {
        let per = self.dwell.entry((uid, si)).or_default();
        let d = per.entry(contact.to_string()).or_insert(Dwell { first_s: s, last_s: s });
        if s - d.last_s > DWELL_GAP_S {
            d.first_s = s;
        }
        d.last_s = s;
        s - d.first_s + 1.0
    }

    /// A sonar's detection of `contact` (`key`: the observer, the sensor and its mode): the
    /// seconds of signal its run has now heard. Each detection counts the draw interval it
    /// closes (`revisit_s`: a second, or a scanning sonar's turn), no more than the time since
    /// the last; a run's first counts a second, and a gap of [`DWELL_GAP_S`] ends the run. At
    /// one detection a draw this is the run's length, as the dwell before it; heard one draw in
    /// twenty, it grows twenty times slower.
    fn heard(&mut self, key: (u64, usize, u8), contact: &str, s: f64, revisit_s: f64) -> f64 {
        let per = self.heard.entry(key).or_default();
        match per.get_mut(contact) {
            Some(h) if s - h.last_s <= DWELL_GAP_S => {
                h.signal_s += (s - h.last_s).clamp(0.0, revisit_s);
                h.last_s = s;
                h.signal_s
            }
            _ => {
                per.insert(contact.to_string(), Heard { last_s: s, signal_s: 1.0 });
                1.0
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn radar(&mut self, cx: &Ctx, o: &Observer, h_s: f64, si: usize, r: &RadarSensor, targets: &[Target], out: &mut Vec<Detection>) {
        let st = cx.st;
        let phase = uniform(st.seed, -2, o.uid, "radar", salt(si, SensorType::Radar, 0)) * r.scan_s;
        for t in targets {
            if !t.above || t.sig.rcs_m2 <= 0.0 {
                continue;
            }
            let p = pair(o, h_s, t);
            // the scan's last pass over the contact's bearing: one draw per pass
            let pass = phase + (p.bearing.rem_euclid(std::f64::consts::TAU) / std::f64::consts::TAU) * r.scan_s;
            let k = ((st.s - pass) / r.scan_s).floor();
            if pass + k * r.scan_s <= st.s - 1.0 {
                continue;
            }
            let obstruction = self.terrain_los(o, h_s, t, st.s).map(|x| x[1]);
            let Some(_) = sight(h_s, t, &p, K_RADIO, obstruction) else {
                continue;
            };
            // the two-ray model's target height: what stands above the horizon line (an
            // aircraft's whole altitude above it)
            let h_t = if t.c.domain == Domain::Air {
                visible_height_m(t.top_msl, t.top_msl, p.ground_m, h_s, K_RADIO)
            } else {
                visible_height_m(t.top_msl, t.vis_height_m, p.ground_m, h_s, K_RADIO)
            };
            let sea = (t.c.domain == Domain::Surface || t.c.domain == Domain::Subsurface).then(|| sea_for(&cx.cond, t.c, &p));
            let pd = radar::pd(r, t.sig.rcs_m2, p.slant_m, p.ground_m, h_s, h_t, sea);
            let scan = k as i64;
            if pd <= 0.0 || uniform(st.seed, scan, o.uid, &t.c.id, salt(si, SensorType::Radar, 1)) >= pd {
                continue;
            }
            let dwell = self.hit(o.uid, si, &t.c.id, st.s);
            // 30 m or 1 % of range, and 1 degree in bearing (IMO minimum [19])
            let sig_r = (0.01 * p.slant_m).max(30.0);
            let sig_x = p.slant_m * 1f64.to_radians();
            let (n_r, n_x) = (
                normal(st.seed, st.tick, o.uid, &t.c.id, salt(si, SensorType::Radar, 2)),
                normal(st.seed, st.tick, o.uid, &t.c.id, salt(si, SensorType::Radar, 3)),
            );
            // a 2D radar measures an aircraft's slant range, not its height: the plot is at the
            // slant range on the bearing (as the legacy model)
            let slant = t.c.domain == Domain::Air && !r.height;
            let range = if slant { p.slant_m } else { p.ground_m } + sig_r * n_r;
            let bearing = p.bearing + sig_x * n_x / p.slant_m.max(1.0);
            let m_per_lon = M_PER_DEG * o.lat.to_radians().cos();
            let (lat, lon) = (o.lat + range.max(0.0) * bearing.cos() / M_PER_DEG, o.lon + range.max(0.0) * bearing.sin() / m_per_lon);
            // the range and bearing laid off from where the vehicle believes it is
            let (lat, lon) = o.geolocate(lat, lon);
            let alt_m = match t.c.domain {
                Domain::Surface => Some(t.c.alt_m),
                Domain::Air if r.height => {
                    Some(t.c.alt_m + 30.0 * normal(st.seed, st.tick, o.uid, &t.c.id, salt(si, SensorType::Radar, 4)))
                }
                _ => None,
            };
            out.push(Detection {
                contact_id: t.c.id.clone(),
                uid: o.uid,
                sensor: SensorType::Radar,
                lat,
                lon,
                alt_m,
                sigma_m: sig_r.max(sig_x),
                range_m: p.slant_m,
                velocity_mps: None,
                label: Some(t.size.to_string()),
                ais: None,
                domain: t.c.domain,
                measured_t: None,
                slant_from: slant.then(|| o.believed()),
                look: Some(Look { level: Level::Detect, confidence: 0.5 * (1.0 - (-dwell / 90.0).exp()) }),
                scan_s: Some(r.scan_s),
            });
        }
    }

    fn ais(&mut self, cx: &Ctx, o: &Observer, h_s: f64, si: usize, targets: &[Target], out: &mut Vec<Detection>) {
        let st = cx.st;
        let class = |t: &Target| if t.sig.ais_class == AisClass::None { AisClass::B } else { t.sig.ais_class };
        let reaches = |t: &Target| {
            let ground = haversine_m(o.lat, o.lon, t.c.lat, t.c.lon);
            ground <= rf::vhf_range_m(h_s, t.sig.height_m.max(1.0))
        };
        // the slot load: every report a minute this receiver could hear
        let load: f64 = targets
            .iter()
            .filter(|t| t.c.ais.is_some() && t.c.domain == Domain::Surface && reaches(t))
            .filter_map(|t| rf::ais_interval_s(class(t), t.c.speed_mps))
            .map(|i| 60.0 / i)
            .sum();
        let decode = rf::ais_decode(load);
        for t in targets {
            let Some(a) = t.c.ais.as_ref().filter(|_| t.c.domain == Domain::Surface) else {
                continue;
            };
            let Some(interval) = rf::ais_interval_s(class(t), t.c.speed_mps) else {
                continue;
            };
            // its reports, at its own phase: one in this second?
            let phase = uniform(st.seed, -3, 0, &t.c.id, 0xA15) * interval;
            let k = ((st.s - phase) / interval).floor();
            if phase + k * interval <= st.s - 1.0 || !reaches(t) {
                continue;
            }
            let p = pair(o, h_s, t);
            if self.terrain_los(o, h_s, t, st.s).is_some_and(|x| x[1] > 0.0) {
                continue;
            }
            if uniform(st.seed, k as i64, o.uid, &t.c.id, salt(si, SensorType::Ais, 1)) >= decode {
                continue;
            }
            let (ne, nn) = (
                normal(st.seed, st.tick, o.uid, &t.c.id, salt(si, SensorType::Ais, 2)),
                normal(st.seed, st.tick, o.uid, &t.c.id, salt(si, SensorType::Ais, 3)),
            );
            let sigma = 10.0;
            let course = t.c.course_deg.to_radians();
            let v = t.c.speed_mps + 0.2 * normal(st.seed, st.tick, o.uid, &t.c.id, salt(si, SensorType::Ais, 4));
            out.push(Detection {
                contact_id: t.c.id.clone(),
                uid: o.uid,
                sensor: SensorType::Ais,
                lat: t.c.lat + sigma * nn / M_PER_DEG,
                lon: t.c.lon + sigma * ne / (M_PER_DEG * t.c.lat.to_radians().cos()),
                alt_m: Some(t.c.alt_m),
                sigma_m: sigma,
                range_m: p.slant_m,
                velocity_mps: Some((v * course.sin(), v * course.cos())),
                label: None,
                ais: Some(AisHeard { mmsi: a.mmsi.clone(), name: a.name.clone(), ship_type: a.ship_type.clone() }),
                domain: t.c.domain,
                measured_t: None,
                slant_from: None,
                look: None,
                scan_s: None,
            });
        }
    }

    /// Sonar `si` of `o` in one of its modes (`part` 0, or 1 for an active sonar's passive mode:
    /// its own draws and evidence, the same wrong calls, being the same sensor's).
    #[allow(clippy::too_many_arguments)]
    fn sonar(&mut self, cx: &Ctx, o: &Observer, si: usize, part: u8, sn: &SonarSensor, targets: &[Target], out: &mut Vec<Detection>) {
        let st = cx.st;
        let draw_salt = |what: u64| salt(si, SensorType::Sonar, what + 16 * u64::from(part));
        let dnl = sonar::noise_db(sn.freq_khz, cx.cond.wind_mps, cx.cond.shipping)
            - sonar::noise_db(sn.freq_khz, sonar::REF_WIND_MPS, sonar::REF_SHIPPING);
        let depth_o = o.alt_msl.min(0.0);
        let phase = sn.scan_s.map(|sc| uniform(st.seed, -2, o.uid, "sonar", draw_salt(0)) * sc);
        for t in targets {
            if !matches!(t.c.domain, Domain::Surface | Domain::Subsurface) {
                continue;
            }
            let Some(fom) = sonar::fom_db(sn, &t.c.kind, &t.sig) else {
                continue;
            };
            let ground = haversine_m(o.lat, o.lon, t.c.lat, t.c.lon);
            let depth_t = if t.c.domain == Domain::Subsurface { t.c.alt_m.min(0.0) } else { 0.0 };
            let r = ground.hypot(depth_t - depth_o);
            if r > sn.max_m {
                continue;
            }
            let water = self.water_depth_m(o, t.c, &cx.cond);
            let pd = sonar::pd(sn, fom, r, water, dnl);
            // a scanning sonar draws once per pass over the bearing, any other every second
            let draw = match (sn.scan_s, phase) {
                (Some(sc), Some(ph)) => {
                    let (de, dn) = en(o.lat, o.lon, t.c.lat, t.c.lon);
                    let pass = ph + (de.atan2(dn).rem_euclid(std::f64::consts::TAU) / std::f64::consts::TAU) * sc;
                    let k = ((st.s - pass) / sc).floor();
                    if pass + k * sc <= st.s - 1.0 {
                        continue;
                    }
                    k as i64
                }
                _ => st.tick,
            };
            if pd <= 0.0 || uniform(st.seed, draw, o.uid, &t.c.id, draw_salt(1)) >= pd {
                continue;
            }
            // the evidence: the seconds of signal this run has heard, not its length
            let heard = self.heard((o.uid, si, part), &t.c.id, st.s, sn.scan_s.unwrap_or(1.0));
            // passive classifies over minutes; active localises, and recognises by the track
            let (level, conf, sigma) = match sn.mode {
                SonarMode::Passive => {
                    let level = if heard >= PASSIVE_IDENTIFY_S {
                        Level::Identify
                    } else if heard >= PASSIVE_RECOGNISE_S {
                        Level::Recognise
                    } else {
                        Level::Detect
                    };
                    (level, 0.8 * (1.0 - (-heard / 120.0).exp()), 50.0 + 0.10 * r)
                }
                SonarMode::Active => {
                    let level = if heard >= ACTIVE_RECOGNISE_S { Level::Recognise } else { Level::Detect };
                    (level, 0.6 * (1.0 - (-heard / 60.0).exp()), 5.0 + 0.01 * r)
                }
            };
            let (ne, nn) = (normal(st.seed, st.tick, o.uid, &t.c.id, draw_salt(2)), normal(st.seed, st.tick, o.uid, &t.c.id, draw_salt(3)));
            let (lat, lon) = o.geolocate(t.c.lat + sigma * nn / M_PER_DEG, t.c.lon + sigma * ne / (M_PER_DEG * t.c.lat.to_radians().cos()));
            out.push(Detection {
                contact_id: t.c.id.clone(),
                uid: o.uid,
                sensor: SensorType::Sonar,
                lat,
                lon,
                alt_m: None,
                sigma_m: sigma,
                range_m: r,
                velocity_mps: None,
                label: Some(label_at(st, o.uid, si, SensorType::Sonar, t, level)),
                ais: None,
                domain: t.c.domain,
                measured_t: None,
                slant_from: None,
                look: Some(Look { level, confidence: conf }),
                scan_s: None,
            });
        }
    }

    /// The water's depth between an observer and a contact: the bathymetry's at the midpoint
    /// (a grid of surface heights below the sea), else the conditions'.
    fn water_depth_m(&self, o: &Observer, c: &ContactTruth, cond: &Conditions) -> f64 {
        self.bathymetry
            .grid
            .as_ref()
            .and_then(|g| g.height(0.5 * (o.lat + c.lat), 0.5 * (o.lon + c.lon)))
            .filter(|h| *h < -1.0)
            .map_or(cond.water_depth_m, |h| -h)
    }

    #[allow(clippy::too_many_arguments)]
    fn esm(&mut self, cx: &Ctx, o: &Observer, h_s: f64, si: usize, e: &EsmSensor, targets: &[Target], out: &mut Vec<Detection>) {
        let st = cx.st;
        for t in targets {
            let Some(em) = t.sig.emitter.filter(|_| t.c.emits && t.above) else {
                continue;
            };
            let p = pair(o, h_s, t);
            let obstruction = self.terrain_los(o, h_s, t, st.s).map(|x| x[1]);
            if sight(h_s, t, &p, K_RADIO, obstruction).is_none() {
                continue;
            }
            let pr = rf::received_dbm(&em, p.slant_m);
            let prob = 0.95 * sonar::phi((pr - e.s_min_dbm) / 3.0);
            if prob < 1e-6 || uniform(st.seed, st.tick, o.uid, &t.c.id, salt(si, SensorType::Esm, 1)) >= prob {
                continue;
            }
            let dwell = self.hit(o.uid, si, &t.c.id, st.s);
            // a bearing: a position along it with the error the bearing implies (as today)
            let sigma = (p.slant_m * e.bearing_deg.to_radians().tan()).max(200.0);
            let (ne, nn) = (
                normal(st.seed, st.tick, o.uid, &t.c.id, salt(si, SensorType::Esm, 2)),
                normal(st.seed, st.tick, o.uid, &t.c.id, salt(si, SensorType::Esm, 3)),
            );
            let label = match t.c.domain {
                Domain::Air => "aircraft",
                Domain::Surface => "vessel",
                Domain::Ground => "ground_vehicle",
                Domain::Subsurface => "submerged",
            };
            let (lat, lon) = o.geolocate(t.c.lat + sigma * nn / M_PER_DEG, t.c.lon + sigma * ne / (M_PER_DEG * t.c.lat.to_radians().cos()));
            out.push(Detection {
                contact_id: t.c.id.clone(),
                uid: o.uid,
                sensor: SensorType::Esm,
                lat,
                lon,
                alt_m: None,
                sigma_m: sigma,
                range_m: p.slant_m,
                velocity_mps: None,
                label: Some(label.to_string()),
                ais: None,
                domain: t.c.domain,
                measured_t: None,
                slant_from: None,
                look: Some(Look { level: Level::Detect, confidence: 0.6 * (1.0 - (-dwell / 60.0).exp()) }),
                scan_s: None,
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn eo(&mut self, cx: &Ctx, o: &Observer, h_s: f64, si: usize, reach_m: f64, eo: &EoSensor, targets: &[Target], out: &mut Vec<Detection>) {
        let st = cx.st;
        let key = (o.uid, si);
        let bore = self.steer(cx, o, h_s, si, reach_m, eo, targets);
        if bore.blind {
            // slewing or zooming: the gimbal sees nothing, but a look in progress survives it
            // (the image tracker keeps its target through a zoom)
            for l in self.looks.entry(key).or_default().values_mut() {
                l.seen_s = st.s;
            }
        }
        let mut identified_now: Vec<String> = Vec::new();
        for t in targets {
            if !t.above || t.sig.dc_m <= 0.0 {
                continue;
            }
            let p = pair(o, h_s, t);
            let obstruction = self.terrain_los(o, h_s, t, st.s).map(|x| x[0]);
            let Some(vis) = sight(h_s, t, &p, K_OPTICAL, obstruction) else {
                continue;
            };
            // hull-down: the critical dimension is sqrt(width x height), the height cut
            let dc = if t.c.domain == Domain::Air || t.vis_height_m <= 0.0 {
                t.sig.dc_m
            } else {
                t.sig.dc_m * (vis / t.vis_height_m).clamp(0.0, 1.0).sqrt()
            };
            // every channel that has it in its field now; identification counts only a field
            // that is narrow (or a camera that does not zoom)
            let (mut pd, mut id_p, mut in_view, mut id_view) = ([0.0f64; 3], 0.0f64, false, false);
            for ch in &eo.channels {
                let narrow = !ch.panoramic && !bore.fixed && bore.narrow;
                let seen = ch.panoramic || (!bore.blind && inside(&bore, &p, if narrow { ch.narrow_rad } else { ch.wide_rad }));
                if !seen {
                    continue;
                }
                in_view = true;
                let pc = eo::channel_p(ch, narrow, p.slant_m, [h_s, t.top_msl], dc, t.sig.thermal_k, &cx.cond, cx.sun);
                for (a, b) in pd.iter_mut().zip(pc) {
                    *a = a.max(b);
                }
                if narrow || !ch.zooms() {
                    id_view = true;
                    id_p = id_p.max(pc[2]);
                }
            }
            if !in_view {
                continue;
            }
            let look = self.looks.entry(key).or_default().entry(t.c.id.clone()).or_insert(EoLook {
                start_tick: st.tick,
                detected_s: None,
                detected_p: 0.0,
                narrow_since: None,
                accrued: [0.0; 3],
                recognised: false,
                identified: false,
                id_best: 0.0,
                id_best_s: st.s,
                seen_s: st.s,
            });
            look.seen_s = st.s;
            look.narrow_since = if id_view { Some(look.narrow_since.unwrap_or(st.s)) } else { None };
            if id_p > look.id_best {
                (look.id_best, look.id_best_s) = (id_p, st.s);
            }
            // each level's evidence accrues while the contact is in the field, at this second's
            // probability (LEVEL_TAU_S), and is reached when it passes the look's own draw of
            // what it takes (an exponential: one per look and level)
            let start = look.start_tick;
            let draw = |what: u64| uniform(st.seed, start, o.uid, &t.c.id, salt(si, SensorType::EoIr, what));
            let need = |what: u64| -(1.0 - draw(what)).ln();
            if look.detected_s.is_none() {
                // the second this tick closes, at ACQUIRE's rate (tau 3.4 / P)
                look.accrued[0] += pd[0] * pd[0] / eo::TAU_K_S;
                if look.accrued[0] >= need(1) {
                    (look.detected_s, look.detected_p) = (Some(st.s), pd[0]);
                }
            }
            let Some(det_s) = look.detected_s else {
                continue;
            };
            // a contact found stays held while it stays about as findable as when it was found
            // (below a share of that drawn once a look, it fades out of the image)
            if pd[0] < look.detected_p * draw(6) {
                continue;
            }
            if !look.recognised && st.s - det_s >= RECOGNISE_AFTER_S {
                look.accrued[1] += pd[1] * pd[1] / LEVEL_TAU_S;
                look.recognised = look.accrued[1] >= need(2);
            }
            if !look.identified && look.narrow_since.is_some_and(|n| st.s - n + 1.0 >= IDENTIFY_DWELL_S) {
                look.accrued[2] += id_p * id_p / LEVEL_TAU_S;
                look.identified = look.accrued[2] >= need(3);
            }
            let (mut level, mut conf) = (Level::Detect, pd[0]);
            if look.recognised {
                (level, conf) = (Level::Recognise, pd[1]);
            }
            if look.identified && id_view {
                (level, conf) = (Level::Identify, id_p);
                identified_now.push(t.c.id.clone());
            }
            // EO/IR's fix as the legacy model's: angles and a position, 15 m + 0.5 % of range
            let sigma = 15.0 + 0.005 * p.slant_m;
            let (ne, nn) = (
                normal(st.seed, st.tick, o.uid, &t.c.id, salt(si, SensorType::EoIr, 4)),
                normal(st.seed, st.tick, o.uid, &t.c.id, salt(si, SensorType::EoIr, 5)),
            );
            let (lat, lon) = o.geolocate(t.c.lat + sigma * nn / M_PER_DEG, t.c.lon + sigma * ne / (M_PER_DEG * t.c.lat.to_radians().cos()));
            out.push(Detection {
                contact_id: t.c.id.clone(),
                uid: o.uid,
                sensor: SensorType::EoIr,
                lat,
                lon,
                alt_m: None,
                sigma_m: sigma,
                range_m: p.slant_m,
                velocity_mps: None,
                label: Some(label_at(st, o.uid, si, SensorType::EoIr, t, level)),
                ais: None,
                domain: t.c.domain,
                measured_t: None,
                slant_from: None,
                look: Some(Look { level, confidence: conf }),
                scan_s: None,
            });
        }
        if !identified_now.is_empty() {
            let mine = self.identified.entry(key).or_default();
            for c in identified_now {
                mine.insert(c, st.s);
            }
        }
    }

    /// The payload manager's turn (WORLD-V3-PLAN 2.4, decision W1): search the task area in the
    /// wide field; after [`SEARCH_MIN_S`], cue to a track there (the nearest unidentified,
    /// else the stalest look); once the wide field has the contact, lock on in the narrow
    /// field until it is identified, while it stays a candidate (fresh, in reach and the task
    /// area) and no cue of a higher class has come (one not identified, against a revisit: cued
    /// at once), and while its identification keeps getting likelier (or did within
    /// [`LOCK_MAX_S`]); back to search. Each camera keeps its own history of what it
    /// identified and looked at in vain: one camera's failed cue does not keep another off the
    /// contact. A cue to an aircraft whose height no sensor measured (a 2D radar's plot, a
    /// camera's or ESM's fix) searches the elevations it may fly at, bar by bar
    /// ([`Cue::aims`]); a cued camera holds its pattern while each new fix keeps the contact
    /// within it and plans it again only when a fix puts the contact outside it (one aim: the
    /// middle of its field, [`CUE_DEADBAND`]; sensing round 3: the bars rebuilt from every fix
    /// left a camera whose cue alternated between two sensors' fixes slewing and settling at
    /// nearly every one), and a locked one follows its contact in closed loop (murmur-7vp:
    /// slewing and settling again at each track update and each second of a lock left the
    /// cameras blind 93 to 98 % of the time they were cued or locked). A slew, a zoom, a search
    /// step and a new bar settle ([`SETTLE_S`]). A camera that cannot slew looks along the
    /// heading; one that is all panoramic does not point.
    #[allow(clippy::too_many_arguments)]
    fn steer(&mut self, cx: &Ctx, o: &Observer, h_s: f64, si: usize, reach_m: f64, eo: &EoSensor, targets: &[Target]) -> Boresight {
        let s = cx.st.s;
        let heading = o.heading_deg.unwrap_or(0.0).to_radians();
        if eo.slew_dps <= 0.0 || eo.channels.iter().all(|c| c.panoramic) {
            return Boresight { az: heading, el: -(5f64.to_radians()), narrow: false, blind: false, fixed: true };
        }
        let wide = eo.channels.iter().filter(|c| !c.panoramic).map(|c| c.wide_rad).fold(0.0, f64::max);
        let slew = eo.slew_dps.to_radians();
        let key = (o.uid, si);
        let mut g = self.gimbals.remove(&key).unwrap_or(Gimbal {
            az: heading,
            el: -(10f64.to_radians()),
            narrow: false,
            busy_until: f64::NEG_INFINITY,
            mode: Mode::Search,
            since: s,
            step: 0,
            step_s: f64::NEG_INFINITY,
            seen_s: s,
            pattern: Vec::new(),
            bar: 0,
            bar_s: f64::NEG_INFINITY,
        });
        g.seen_s = s;
        // the image tracker's line of sight to a contact it has in view: from where the vehicle is
        let aim = |lat: f64, lon: f64, alt: f64| {
            let (de, dn) = en(o.lat, o.lon, lat, lon);
            (de.atan2(dn), (alt - h_s).atan2(de.hypot(dn).max(1.0)))
        };
        // the payload manager's to a place (a search point; a cue's, `Cue::sightlines`): from where
        // the vehicle believes it is, so a navigation error turns it off the place
        let (blat, blon) = o.believed();
        let aim_at_place = |lat: f64, lon: f64, alt: f64| {
            let (de, dn) = en(blat, blon, lat, lon);
            (de.atan2(dn), (alt - h_s).atan2(de.hypot(dn).max(1.0)))
        };
        let point = |g: &mut Gimbal, az: f64, el: f64, narrow: bool| {
            let moved = angle(g.az, g.el, az, el);
            if moved > 1e-3 || narrow != g.narrow {
                g.busy_until = g.busy_until.max(s) + moved.to_degrees() / slew.to_degrees() + SETTLE_S;
            }
            (g.az, g.el, g.narrow) = (az, el, narrow);
        };
        let target = |id: &str| targets.iter().find(|t| t.c.id == id);
        let mut cue_to: Option<String> = None;
        match g.mode.clone() {
            Mode::Lock(c) => {
                // held until this camera identifies it, while it stays a candidate the manager
                // would keep (a cue of a higher class takes the camera at once), and while the
                // identification keeps getting likelier or did within LOCK_MAX_S
                let identified = self.identified.get(&key).and_then(|m| m.get(&c)).is_some_and(|ti| *ti >= g.since);
                let progress = self.looks.get(&key).and_then(|l| l.get(&c)).map_or(g.since, |l| l.id_best_s.max(g.since));
                let choice = if identified || s - progress >= LOCK_MAX_S { None } else { self.pick_cue(key, o, reach_m, s, Some(&c)) };
                match target(&c).filter(|_| choice.as_deref() == Some(c.as_str())) {
                    Some(t) => {
                        // the image tracker keeps it centred: the gimbal follows it in closed
                        // loop, with nothing to settle, while its line of sight turns no faster
                        // than the gimbal slews (murmur-7vp)
                        let (az, el) = aim(t.c.lat, t.c.lon, t.top_msl);
                        if g.narrow && angle(g.az, g.el, az, el) <= slew {
                            (g.az, g.el) = (az, el);
                        } else {
                            point(&mut g, az, el, true);
                        }
                    }
                    None => {
                        // a cue of a higher class (`pick_cue`) takes the camera at once
                        cue_to = choice.filter(|x| *x != c);
                        if !identified {
                            self.looked.entry(key).or_default().insert(c, s);
                        }
                        g.mode = Mode::Search;
                        g.since = s;
                        g.step_s = f64::NEG_INFINITY;
                    }
                }
            }
            Mode::Cue(c) => {
                let acquired = s >= g.busy_until
                    && self.looks.get(&key).and_then(|l| l.get(&c)).is_some_and(|l| l.detected_s.is_some());
                // the cue's pattern, one aim at the contact or an aircraft's elevation search, is
                // held while the latest fix keeps the contact within it, and planned again from
                // that fix only when it does not (a fix from another sensor, a 2D radar's or a
                // camera's, is another arc through the same aircraft: it does not restart the
                // search). Held: one aim while the fix stays in the field's middle
                // (CUE_DEADBAND, as before); a known height while it lies in the current bar's
                // field; a fix with no height while some place it allows lies in one of the
                // pattern's bars' fields.
                let replan = self.cues.get(&c).and_then(|cue| {
                    let fresh = cue.aims(o, h_s, wide);
                    let held = match (g.pattern.as_slice(), fresh.as_slice()) {
                        (_, []) => true,
                        ([], _) => false,
                        ([one], [new]) => angle(one.0, one.1, new.0, new.1) <= CUE_DEADBAND * wide,
                        (bars, [new]) if cue.alt.is_some() => bars.get(g.bar).is_some_and(|b| in_field(*b, *new, wide)),
                        (bars, _) => cue.sightlines(o, h_s).iter().any(|l| bars.iter().any(|b| in_field(*b, *l, wide))),
                    };
                    (!held).then_some(fresh)
                });
                // each bar stared at for BAR_STARE_S once settled, then the next, the cue ending
                // with the last
                let scan = g.pattern.len() > 1;
                let bar_over = scan && s >= g.busy_until && s - g.bar_s >= BAR_STARE_S;
                // one aim gives up CUE_MAX_S after the cue began (or after it replaced a search)
                let began = if g.bar > 0 { g.bar_s } else { g.since };
                if acquired {
                    g.mode = Mode::Lock(c.clone());
                    g.since = s;
                    if let Some(t) = target(&c) {
                        let (az, el) = aim(t.c.lat, t.c.lon, t.top_msl);
                        point(&mut g, az, el, true);
                    }
                } else if target(&c).is_none()
                    || (!scan && s - began >= CUE_MAX_S)
                    || (bar_over && g.bar + 1 >= g.pattern.len())
                {
                    self.looked.entry(key).or_default().insert(c, s);
                    g.mode = Mode::Search;
                    g.since = s;
                    g.step_s = f64::NEG_INFINITY;
                } else if let Some(fresh) = replan {
                    // a new plan from the fix: it keeps the bar the search had reached and stares
                    // at it afresh
                    if g.pattern.len() > 1 && fresh.len() == 1 {
                        g.since = s;
                    }
                    g.bar = g.bar.min(fresh.len() - 1);
                    g.pattern = fresh;
                    let (az, el) = g.pattern[g.bar];
                    point(&mut g, az, el, false);
                    g.bar_s = g.busy_until.max(s);
                } else if bar_over {
                    g.bar += 1;
                    let (az, el) = g.pattern[g.bar];
                    point(&mut g, az, el, false);
                    g.bar_s = g.busy_until.max(s);
                }
            }
            Mode::Search => {}
        }
        if g.mode == Mode::Search {
            let pick = cue_to.or_else(|| (s - g.since >= SEARCH_MIN_S).then(|| self.pick_cue(key, o, reach_m, s, None)).flatten());
            if let Some(c) = pick {
                g.pattern = self.cues[&c].aims(o, h_s, wide);
                g.mode = Mode::Cue(c);
                g.since = s;
                g.bar = 0;
                if let Some(&(az, el)) = g.pattern.first() {
                    point(&mut g, az, el, false);
                }
                g.bar_s = g.busy_until.max(s);
            } else if s - g.step_s >= SEARCH_DWELL_S {
                let pts = search_points(o, reach_m, wide);
                let (lat, lon) = pts[g.step % pts.len()];
                g.step += 1;
                g.step_s = s;
                let (az, el) = aim_at_place(lat, lon, 0.0);
                point(&mut g, az, el, false);
            } else if g.narrow {
                let (az, el) = (g.az, g.el);
                point(&mut g, az, el, false);
            }
        }
        let b = Boresight { az: g.az, el: g.el, narrow: g.narrow, blind: s < g.busy_until, fixed: false };
        self.gimbals.insert(key, g);
        b
    }

    /// The track camera `key` cues to (WORLD-V3-PLAN 2.4): within its reach and the task area,
    /// fixed within [`CUE_FRESH_S`], not looked at in vain by it within [`RETRY_S`]; the nearest
    /// it has not identified within [`REVISIT_S`], else the one it looked at longest ago (beyond
    /// [`STALE_LOOK_S`]). A locked camera asks it each second, `holding` its contact: it keeps
    /// the contact while it is a candidate, unless the contact is a revisit and a contact the
    /// camera has not identified is one (a higher class); a nearer contact of the same class
    /// does not preempt a lock (it would thrash between two contacts and finish neither).
    fn pick_cue(&self, key: (u64, usize), o: &Observer, reach_m: f64, s: f64, holding: Option<&str>) -> Option<String> {
        let none = HashMap::new();
        let identified = self.identified.get(&key).unwrap_or(&none);
        let looked = self.looked.get(&key).unwrap_or(&none);
        // how far a cue is, as the payload manager reckons it: from where its vehicle believes it is
        let (olat, olon) = o.believed();
        let mut cands: Vec<(&String, f64, f64)> = self
            .cues
            .iter()
            .filter(|(c, cue)| s - cue.s <= CUE_FRESH_S && looked.get(*c).is_none_or(|t| s - t >= RETRY_S))
            .filter_map(|(c, cue)| {
                let d = haversine_m(olat, olon, cue.lat, cue.lon);
                let in_area = o.area.as_ref().is_none_or(|a| area_distance_m(a, cue.lat, cue.lon) <= 0.5 * reach_m);
                let last = identified.get(c).copied().into_iter().chain(looked.get(c).copied()).fold(f64::NEG_INFINITY, f64::max);
                (d <= reach_m && in_area).then_some((c, d, last))
            })
            .collect();
        cands.sort_by(|a, b| a.0.cmp(b.0));
        let unidentified_ = |c: &str| identified.get(c).is_none_or(|t| s - t > REVISIT_S);
        // rules 21: the track an identify order sends the vehicle to, first, whenever it is a
        // candidate this camera has not identified (ahead of a lock on another contact)
        if let Some(first) = self.firsts.get(&key.0)
            && let Some((c, _, _)) = cands
                .iter()
                .filter(|(c, _, _)| first.contains(c.as_str()) && unidentified_(c))
                .min_by(|a, b| a.1.total_cmp(&b.1))
        {
            return Some((*c).clone());
        }
        let unidentified = cands.iter().filter(|(c, _, _)| unidentified_(c)).min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some(h) = holding {
            // a lock keeps its contact while it is still a candidate, unless a cue of a higher
            // class (one not identified, against a revisit) has come; a nearer one of its own
            // class does not take the camera from it
            return match cands.iter().find(|(c, _, _)| c.as_str() == h) {
                None => None,
                Some(_) if unidentified_(h) || unidentified.is_none() => Some(h.to_string()),
                Some(_) => unidentified.map(|(c, _, _)| (*c).clone()),
            };
        }
        if let Some((c, _, _)) = unidentified {
            return Some((*c).clone());
        }
        cands
            .iter()
            .filter(|(_, _, last)| s - last > STALE_LOOK_S)
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .map(|(c, _, _)| (*c).clone())
    }
}

/// The sea a surface contact is seen against by a radar at the pair's end (sea clutter): the sea
/// state and the wind, and the contact's own speed along the line of sight (its course and speed
/// over the ground, the line of sight's depression taken into account; a Doppler mode's).
pub fn sea_for(cond: &Conditions, c: &ContactTruth, p: &Pair) -> radar::Sea {
    let radial = c.speed_mps * (c.course_deg.to_radians() - p.bearing).cos() * p.el.cos();
    radar::Sea { state: cond.sea_state, wind_mps: cond.wind_mps, radial_mps: radial }
}

/// What one step reads besides the state.
struct Ctx<'a> {
    st: &'a Step<'a>,
    cond: Conditions,
    sun: f64,
}

/// The angle between two pointing directions (azimuth, elevation), radians.
fn angle(az1: f64, el1: f64, az2: f64, el2: f64) -> f64 {
    let c = el1.sin() * el2.sin() + el1.cos() * el2.cos() * (az1 - az2).cos();
    c.clamp(-1.0, 1.0).acos()
}

/// Whether a contact at `p` is inside a field `hfov` wide (and three quarters as high) about
/// the boresight.
fn inside(b: &Boresight, p: &Pair, hfov: f64) -> bool {
    in_field((b.az, b.el), (p.bearing, p.el), hfov)
}

/// Whether the line of sight `p` (azimuth, elevation, radians) is inside a field `hfov` wide
/// (and three quarters as high) aimed at `aim`.
fn in_field(aim: (f64, f64), p: (f64, f64), hfov: f64) -> bool {
    let daz = (p.0 - aim.0 + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI;
    daz.abs() * p.1.cos() <= 0.5 * hfov && (p.1 - aim.1).abs() <= 0.375 * hfov
}

/// How far a point is outside a task area, m (0 inside an orbit's circle or on a route).
fn area_distance_m(a: &SearchArea, lat: f64, lon: f64) -> f64 {
    match a {
        SearchArea::Orbit { center, radius_m } => (haversine_m(center.0, center.1, lat, lon) - radius_m).max(0.0),
        SearchArea::Route { points } => points
            .windows(2)
            .map(|w| {
                let (ax, ay) = en(lat, lon, w[0].0, w[0].1);
                let (bx, by) = en(lat, lon, w[1].0, w[1].1);
                let (dx, dy) = (bx - ax, by - ay);
                let l2 = dx * dx + dy * dy;
                let f = if l2 > 0.0 { (-(ax * dx + ay * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
                (ax + f * dx).hypot(ay + f * dy)
            })
            .fold(f64::INFINITY, f64::min),
    }
}

/// The ground points a camera's search steps through (WORLD-V3-PLAN 2.4): an orbit's centre
/// and six points halfway out; a route's points every quarter of the reach, those within it;
/// around the vehicle at half its reach, one wide field (less an overlap) a step. Places the
/// payload manager reckons from where its vehicle believes it is.
fn search_points(o: &Observer, reach_m: f64, wide_rad: f64) -> Vec<(f64, f64)> {
    let (olat, olon) = o.believed();
    let at = |lat: f64, lon: f64, bearing: f64, d: f64| {
        (lat + d * bearing.cos() / M_PER_DEG, lon + d * bearing.sin() / (M_PER_DEG * lat.to_radians().cos()))
    };
    match &o.area {
        Some(SearchArea::Orbit { center, radius_m }) => std::iter::once(*center)
            .chain((0..6).map(|k| at(center.0, center.1, f64::from(k) * std::f64::consts::FRAC_PI_3, 0.5 * radius_m)))
            .collect(),
        Some(SearchArea::Route { points }) if points.len() >= 2 => {
            let step = (0.25 * reach_m).max(200.0);
            let mut pts = Vec::new();
            for w in points.windows(2) {
                let (de, dn) = en(w[0].0, w[0].1, w[1].0, w[1].1);
                let len = de.hypot(dn);
                let n = (len / step).ceil().max(1.0) as usize;
                for i in 0..n {
                    let f = i as f64 / n as f64;
                    let p = (w[0].0 + f * (w[1].0 - w[0].0), w[0].1 + f * (w[1].1 - w[0].1));
                    if haversine_m(olat, olon, p.0, p.1) <= reach_m {
                        pts.push(p);
                    }
                }
            }
            if pts.is_empty() { around(o, reach_m, wide_rad, at) } else { pts }
        }
        _ => around(o, reach_m, wide_rad, at),
    }
}

fn around(o: &Observer, reach_m: f64, wide_rad: f64, at: impl Fn(f64, f64, f64, f64) -> (f64, f64)) -> Vec<(f64, f64)> {
    let heading = o.heading_deg.unwrap_or(0.0).to_radians();
    let n = (std::f64::consts::TAU / (0.8 * wide_rad.max(1e-3))).ceil().clamp(1.0, 36.0) as usize;
    let (olat, olon) = o.believed();
    (0..n).map(|k| at(olat, olon, heading + std::f64::consts::TAU * k as f64 / n as f64, 0.5 * reach_m)).collect()
}

/// What a look at `level` calls the contact: its size class (a detection), its family (a
/// recognition) or its kind (an identification), wrong at the level's rate, decided once per
/// observer, sensor, contact and level so a wrong call persists, and a closer look can correct
/// it.
fn label_at(st: &Step, uid: u64, si: usize, sensor: SensorType, t: &Target, level: Level) -> String {
    let rate = level_rate(st.misclassification_rate, level);
    let wrong = rate > 0.0 && uniform(st.seed ^ 0x9E37, -1, uid, &t.c.id, salt(si, sensor, 8 + level as u64)) < rate;
    match level {
        Level::Detect => t.size.to_string(),
        Level::Recognise => {
            let f = family(&t.c.kind);
            if wrong { family_confusion(f) } else { f }.to_string()
        }
        Level::Identify => if wrong { confusion(&t.c.kind) } else { t.c.kind.as_str() }.to_string(),
    }
}

/// Rules 9 under the physical model: an act is observed by the sensors that detected its
/// author this tick (radar, EO/IR, sonar; not AIS nor ESM), each observer's best fix placing it
/// (a 2D radar's bearing segment read at its middle, as the legacy `observe_act`); the observer
/// with the smallest error places the act. None: nobody saw it.
pub fn observe_act(detections: &[Detection], actor: &str) -> Option<ActObservation> {
    /// An observer's best fix (lat, lon, its 1-sigma) and its sensors that saw the act.
    type ObserverFix = ((f64, f64, f64), Vec<String>);
    let mut sensors: Vec<String> = Vec::new();
    let mut per: BTreeMap<u64, ObserverFix> = BTreeMap::new();
    for d in detections {
        if d.contact_id != actor || !matches!(d.sensor, SensorType::Radar | SensorType::EoIr | SensorType::Sonar) {
            continue;
        }
        let name = format!("{}:{}", d.uid, d.sensor.as_str());
        let fix = match d.slant_from {
            Some((olat, olon)) => {
                let (de, dn) = en(olat, olon, d.lat, d.lon);
                let hi = de.hypot(dn) + 2.0 * d.sigma_m;
                let b = de.atan2(dn);
                let m_per_lon = M_PER_DEG * olat.to_radians().cos();
                (olat + 0.5 * hi * b.cos() / M_PER_DEG, olon + 0.5 * hi * b.sin() / m_per_lon, (hi / 3.0).max(d.sigma_m))
            }
            None => (d.lat, d.lon, d.sigma_m),
        };
        if !sensors.contains(&name) {
            sensors.push(name.clone());
        }
        let e = per.entry(d.uid).or_insert((fix, Vec::new()));
        if fix.2 < e.0.2 {
            e.0 = fix;
        }
        if !e.1.contains(&name) {
            e.1.push(name);
        }
    }
    let (&uid, &((lat, lon, sigma), _)) = per.iter().min_by(|a, b| a.1.0.2.total_cmp(&b.1.0.2))?;
    let fixes = per
        .into_iter()
        .filter(|(o, _)| *o != uid)
        .map(|(o, ((lat, lon, s), ss))| ActFix { observer: o, sensors: ss, lat, lon, sigma_m: (s * 10.0).round() / 10.0 })
        .collect();
    Some(ActObservation { sensors, lat, lon, sigma_m: (sigma * 10.0).round() / 10.0, fixes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracks::sensing::SensorSpec;
    use crate::tracks::truth::TruthAffiliation;

    /// (`lat`, `lon`) moved `east` and `north` metres.
    fn at(lat: f64, lon: f64, east: f64, north: f64) -> (f64, f64) {
        (lat + north / M_PER_DEG, lon + east / (M_PER_DEG * lat.to_radians().cos()))
    }

    /// A cued camera holds its elevation search while the cue stays within it (sensing round 3;
    /// SCORING-V3.md 11, F2 San Diego seed 5108: the search's bars were rebuilt from every new fix,
    /// so when a 2D radar's plot and another camera's fix of one aircraft took turns as the cue,
    /// the bar the camera was on jumped between two patterns and it slewed and settled at nearly
    /// every fix, blind most of the cue). A camera 1.5 km from where an aircraft holds 1,200 m up
    /// is cued, its fixes alternating each second between a radar 400 m from it (a plot at the
    /// slant range on the radar's bearing) and a camera's fix (a position, no height), both
    /// consistent with it: the camera steps its bars no faster than a stare allows, each bar once,
    /// to the pattern's end. The aircraft itself is out of the camera's sight (behind it), so the
    /// cue runs its course.
    #[test]
    fn a_cued_camera_holds_its_search_while_the_cue_stays_within_it() {
        let p = at(37.8, -122.48, 0.0, 6_000.0);
        let cam: SensorSpec = toml::from_str::<toml::Table>(
            "s = { type = \"eo_ir\", range_m = 4000, slew_dps = 60, channels = [{ band = \"day\", wide_deg = 30, narrow_deg = 3, px = 1920 }] }",
        )
        .expect("toml")["s"]
            .clone()
            .try_into()
            .expect("spec");
        let (clat, clon) = at(p.0, p.1, 1_500.0, 0.0);
        let o = Observer { uid: 5, lat: clat, lon: clon, alt_msl: 120.0, mast_m: 0.0, sensors: vec![cam], heading_deg: Some(270.0), ..Observer::default() };
        let (tlat, tlon) = at(p.0, p.1, 30_000.0, 0.0);
        let heli = ContactTruth {
            id: "heli".into(),
            name: "heli".into(),
            affiliation: TruthAffiliation::Unknown,
            domain: Domain::Air,
            kind: "helicopter".into(),
            lat: tlat,
            lon: tlon,
            alt_m: 1_200.0,
            speed_mps: 0.0,
            course_deg: 0.0,
            emits: false,
            signature: 1.0,
            ais: None,
        };
        let (rlat, rlon) = at(p.0, p.1, 0.0, -400.0);
        let slant = 400f64.hypot(1_200.0 - 10.0);
        let plot = at(rlat, rlon, 0.0, slant);
        let radar = |s: f64| Cue { lat: plot.0, lon: plot.1, alt: None, slant: Some((rlat, rlon, 10.0)), s };
        let camera = |s: f64| Cue { lat: p.0, lon: p.1, alt: None, slant: None, s };
        let sensing = Sensing { conditions: Some(Conditions { sun_elevation_deg: Some(45.0), ..Conditions::default() }), ..Sensing::default() };
        let mut e = Engine::new(None);
        let (mut cued, mut ended) = (None, None);
        let mut aims: Vec<(i64, f64, f64)> = Vec::new();
        for s in 0..120i64 {
            let st = Step { seed: 7, tick: s, s: s as f64, t: 1_781_467_800.0 + s as f64, misclassification_rate: 0.0, sensing: &sensing };
            e.cues.insert("heli".into(), if s % 2 == 0 { radar(s as f64) } else { camera(s as f64) });
            e.step(&st, std::slice::from_ref(&o), std::slice::from_ref(&heli));
            let g = &e.gimbals[&(5, 0)];
            match (&g.mode, cued) {
                (Mode::Cue(_), None) => cued = Some(s),
                (Mode::Cue(_), Some(_)) => {}
                (_, Some(_)) => {
                    ended = Some(s);
                    break;
                }
                _ => continue,
            }
            if aims.last().is_none_or(|a| (a.1 - g.az).abs() + (a.2 - g.el).abs() > 1e-9) {
                aims.push((s, g.az, g.el));
            }
        }
        let cued = cued.expect("cued");
        let at_s: Vec<i64> = aims.iter().map(|a| a.0).collect();
        assert!(aims.len() >= 2, "an elevation search of one bar: {at_s:?}");
        assert!(aims.windows(2).all(|w| w[1].0 - w[0].0 > BAR_STARE_S as i64), "cued at {cued} s, the aim changed at {at_s:?}");
        assert!(ended.is_some_and(|t| t - cued <= aims.len() as i64 * 7), "cued at {cued} s, aims at {at_s:?}, ended at {ended:?}");
    }
}
