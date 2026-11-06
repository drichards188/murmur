//! The weapon picture (docs/OPPONENT-WEAPONS.md): each opponent weapon as our sensors know it,
//! kept apart from the track picture, and known by stages as a crew learns it (bd murmur-ju1h):
//!
//! 1. **launch**: a camera saw a launch's flash (or a sonar heard a torpedo's launch transient).
//!    The launch is a time, the shooter's place as the cue fixes it, the bearing from the observer
//!    and, when the sensor told it, what kind of launch it was (`physical::weapon::LaunchLook`: a
//!    missile's, a rocket's, a gun's muzzle flash, a launch transient); no class, no speed, no
//!    target. The shooter is the live track nearest the cue's fix within its gate, when there is
//!    one. A gun's burst is a launch of its own, each under its firing episode's act report.
//! 2. **weapon**: fixes whose kinematics say "weapon": a speed over its domain's threshold
//!    ([`weapon_speed_mps`]: under water when a sonar heard it, in the air when a measured height
//!    puts it over [`AIR_FROM_M`], else at the surface) measured from the fixes (with the launch's
//!    flash, when the fixes associate with one), and either the launch as its origin or a line
//!    closing on one of ours ([`closing`], in three dimensions where a height was measured).
//!    Fixes associate with a weapon track by gating on position and velocity
//!    ([`Picture::plots`]), never by the truth's weapon id. A contact track the tracker holds that
//!    passes the same test (speed over its domain's threshold, closing on one of ours) is taken
//!    for a weapon too ([`Picture::contacts`]): a fast aircraft running at one of ours is, as a
//!    crew's threat warning would take it. Type unknown.
//! 3. **class**: assessed only on evidence, named in the feed ([`Assessed::evidence`]), among the
//!    classes of the threat library (`physical::weapon::threat_library`) that travel where it was
//!    sensed (the water or the air): the launch's kind when told, the measured speed (known within
//!    [`SPEED_EVIDENCE_SHARE`]) and, where measured, height, and the shooter identified as a kind
//!    our side believes may carry the class ([`carried_by`], which may name several). A class is
//!    named only when the evidence leaves exactly one: the shooter alone pins one only if the
//!    launch's kind leaves one of those it may carry, and a speed inside two classes' bands (an
//!    ATGM's and a rocket's overlap) names none. An assessment once made holds.
//! 4. **type** (the exact weapon) is out of reach: nothing we model tells it (no seeker emitter
//!    library, no laser warning, no debris). Not served.
//!
//! The vehicle a weapon heads for and its expected strike are derived too: from its measured line
//! (the one of ours its closest approach passes nearest, within [`AIM_MISS_M`] or its error), or,
//! with a class and no line, the only one of ours of the class's targets within its reach of the
//! launch (at the class's nominal speed); otherwise left out.
//!
//! A weapon track ends **expended** when one of ours reports a strike and the strike is put on it
//! (the track heading for that vehicle, else the one whose last cue could have reached it, else a
//! new track: a weapon nobody saw is in the picture at its strike), or **lost** 10 s after its last
//! cue with none; ended, it is shown for a minute, then forgotten.
//!
//! Never a track: it is not in the decision facts, so it is never a lost contact (rules 16),
//! never counted out of view at an air track's top speed, never a keep-out item, an attribution
//! candidate or an escort, and replay never sees it leave a picture. Nothing the engine decides
//! reads it; the console draws it and the inbox shows each strike.
//!
//! **Still by construction** (docs/OPPONENT-WEAPONS.md section 11 item 3): the plot streams are
//! split by the truth: a weapon's own fixes come to this picture and never to the contact tracker,
//! contacts' detections go to the tracker, whose tracks this picture reads. A strike is reported at
//! the truth tick at or after it, not over the victim's link (bd murmur-3nkc). The truth's weapon
//! `kind` chooses the physics a launch and a flight are sensed by (section 15), and so what our
//! sensors could make of them; nothing here reads it.
//!
//! Times are the bridge's clock (seconds here, engine ms in the feed): a truth instant is
//! placed at the bridge's receipt of its message less its age then.

use std::collections::HashMap;

use serde_json::{Value, json};

use super::physical::weapon::{ClassSig, LaunchLook, threat_library};
use super::truth::Domain;
use crate::geo::{bearing_deg, haversine_m};

/// A weapon track with no cue for this long and no strike ends lost, s.
const LOST_AFTER_S: f64 = 10.0;
/// An ended weapon track is shown this long, then forgotten, s.
const SHOWN_S: f64 = 60.0;
/// The fastest anything is taken to fly when gating a fix on a single earlier point (a launch's
/// flash, a lone fix), m/s: over any weapon a contact here carries, and over a supersonic
/// missile's average [E].
pub const V_MAX_MPS: f64 = 1_200.0;
/// A fix gates on a weapon track's predicted position within this many sigmas of the two
/// errors combined, plus [`GATE_SLACK_M`].
const GATE_SIGMAS: f64 = 3.0;
const GATE_SLACK_M: f64 = 30.0;
/// In the gate, a line's speed is taken as no surer than this share of itself [E].
const GATE_SPEED_SHARE: f64 = 0.25;
/// A line closes on one of ours when its closest approach passes within this (or within twice its
/// own error there, if more) and comes within [`CLOSING_HORIZON_S`], m: a weapon's line passes
/// within a ship's length or two of its target, a guided one's bends to it [E].
pub const AIM_MISS_M: f64 = 200.0;
pub const CLOSING_HORIZON_S: f64 = 60.0;
/// A measured speed is evidence for a class only when known this well (1-sigma over speed): a
/// launch's flash and a camera's fix two seconds later give some 5 %; a launch and one radar
/// plot from 10 km some 35 %, which fits too many speeds to name a class [E].
pub const SPEED_EVIDENCE_SHARE: f64 = 0.25;
/// Two fixes this close in time give no velocity, s.
const MIN_SPAN_S: f64 = 0.3;
/// A weapon whose measured height puts it this far over the sea is judged in the air (its speed
/// against the air's threshold), m [E: over the sea-skimming band and any craft's height].
pub const AIR_FROM_M: f64 = 50.0;
/// A strike is put on a weapon track whose last cue is at most this old, s.
const STRIKE_LINK_S: f64 = 30.0;

/// The speed over which something in a domain is taken for a weapon, m/s [E]: faster than any
/// boat or road vehicle (the fastest craft here make some 25 m/s, racing boats 50), than any
/// submarine (torpedoes run 20 to 30), and, in the air, than helicopters, light aircraft and
/// airliners near the ground (under 130), though not than a jet at speed: a fast jet running at
/// one of ours is taken for a weapon, and a slow one-way drone (40 to 60 m/s) is not. A fix
/// without a height (a 2D radar's plot, a camera's fix) is judged as at the surface, the lower of
/// the two over water, so a low weapon is not missed for want of one.
pub fn weapon_speed_mps(domain: Domain) -> f64 {
    match domain {
        Domain::Air => 150.0,
        Domain::Surface | Domain::Ground => 60.0,
        Domain::Subsurface => 20.0,
    }
}

/// The classes our side believes a contact identified as `kind` may carry: the threat library's
/// order of battle (docs/OPPONENT-WEAPONS.md section 15, [E] on docs/research/opponent-weapons.md's
/// public cases), not what this contact carries (the truth's weapon is never read). An armed
/// craft is identified only as its look-alike (decision D3), so this is a belief about every
/// craft of the kind: small fast craft have fired anti-tank missiles, RPGs, rockets and machine
/// guns at ships [8, 9, 10]; merchant hulls have carried containerised launchers and improvised
/// guns and cannon [7, 16, 20], tankers machine guns against drone boats [18]; coast guard boats
/// carry machine guns [13]; pickups are the technicals' [17]; Group 3 UAS carry light missiles
/// [12]; a submarine (or its periscope) carries torpedoes [15]. A quadcopter's dropped munition
/// and a one-way drone are no launch: none.
pub fn carried_by(kind: &str) -> &'static [&'static str] {
    match kind {
        "small_fast_boat" => &["ATGM-class", "Rocket-class", "Gun-class"],
        "cargo_ship" => &["Rocket-class", "Gun-class"],
        "tanker" | "patrol_boat" | "truck" => &["Gun-class"],
        "unknown_uas" => &["Air missile-class"],
        "submarine" | "periscope" => &["Torpedo-class"],
        _ => &[],
    }
}

/// The launch as our cameras saw it: when the report was made (`t`, the act report's time) and
/// when the flash was (`flash_t`), where the cue placed the shooter, the cameras, and the observer
/// whose fix it is (uid, where it believed it was).
#[derive(Debug, Clone, PartialEq)]
pub struct Launch {
    pub t: f64,
    pub flash_t: f64,
    pub lat: f64,
    pub lon: f64,
    pub sigma_m: f64,
    pub sensors: Vec<String>,
    pub observer: Option<(u64, f64, f64)>,
    /// what kind of launch it was, when the sensor told it
    pub look: Option<LaunchLook>,
}

impl Launch {
    /// Heard by a sonar (a torpedo's launch transient): in the water.
    fn heard(&self) -> bool {
        self.sensors.iter().any(|s| s.ends_with(":sonar"))
    }
}

/// One fix in the weapon channel (no weapon id: it is associated by gating), with the height a
/// height-finding radar measured (none from any other sensor).
#[derive(Debug, Clone, PartialEq)]
pub struct Fix {
    pub t: f64,
    pub lat: f64,
    pub lon: f64,
    pub sigma_m: f64,
    /// `"<uid>:<sensor>"`
    pub sensor: String,
    pub alt_m: Option<f64>,
}

/// A measured line: where (at `t`, with its 1-sigma) and the velocity, m/s east and north, its
/// speed and that speed's 1-sigma; the height when a sensor measured it, and its rate (0 from
/// a single height).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Kin {
    pub t: f64,
    pub lat: f64,
    pub lon: f64,
    pub sigma_m: f64,
    pub ve: f64,
    pub vn: f64,
    pub speed: f64,
    pub speed_sigma: f64,
    pub alt_m: Option<f64>,
    pub vz: f64,
}

/// Why it was taken for a weapon: its measured speed over the threshold of the domain it was
/// judged in, and its origin at a seen launch or its line closing on one of ours.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeaponBasis {
    pub speed_mps: f64,
    pub over_mps: f64,
    pub domain: Domain,
    pub from_launch: bool,
    pub closing_on: Option<u64>,
}

/// One piece of a class's evidence: its basis (`speed`, `height`, `launch`, `shooter`), a short
/// form for a label ("243 m/s", "muzzle flash", "from T-0004") and a sentence.
#[derive(Debug, Clone, PartialEq)]
pub struct Evidence {
    pub basis: &'static str,
    pub short: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assessed {
    pub class: ClassSig,
    pub evidence: Vec<Evidence>,
}

/// The vehicle it heads for and its expected strike, and on what basis (`track`: its measured
/// line; `reach`: the only one of ours of its class's targets within its reach).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aim {
    pub uid: u64,
    pub eta_t: f64,
    pub basis: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Status {
    InFlight,
    Expended,
    Lost,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::InFlight => "in_flight",
            Status::Expended => "expended",
            Status::Lost => "lost",
        }
    }
}

/// How far its identity has come: a lone fix (`plot`, not called a weapon), a seen launch, a
/// weapon by its kinematics, a class on evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Plot,
    Launch,
    Weapon,
    Class,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Plot => "plot",
            Stage::Launch => "launch",
            Stage::Weapon => "weapon",
            Stage::Class => "class",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WeaponTrack {
    pub id: String,
    /// the act report the seen launch became (`A-<n>`)
    pub report: Option<String>,
    pub launch: Option<Launch>,
    pub fixes: Vec<Fix>,
    /// the contact track it is, when a track of the tracker passed the weapon test (a fast
    /// aircraft may)
    pub track: Option<String>,
    /// the contact track the launch was placed on
    pub shooter: Option<String>,
    pub kin: Option<Kin>,
    pub weapon: Option<WeaponBasis>,
    pub class: Option<Assessed>,
    pub aim: Option<Aim>,
    pub status: Status,
    pub ended_t: Option<f64>,
    /// the strike one of ours reported, put on this track: (uid, when)
    pub struck: Option<(u64, f64)>,
}

impl WeaponTrack {
    fn new(id: String) -> WeaponTrack {
        WeaponTrack {
            id,
            report: None,
            launch: None,
            fixes: Vec::new(),
            track: None,
            shooter: None,
            kin: None,
            weapon: None,
            class: None,
            aim: None,
            status: Status::InFlight,
            ended_t: None,
            struck: None,
        }
    }

    pub fn stage(&self) -> Stage {
        if self.class.is_some() {
            Stage::Class
        } else if self.weapon.is_some() {
            Stage::Weapon
        } else if self.launch.is_some() {
            Stage::Launch
        } else {
            Stage::Plot
        }
    }

    /// When it was first known of (the launch seen, else its first fix, else the strike).
    pub fn first_cue_t(&self) -> Option<f64> {
        let a = self.launch.as_ref().map(|l| l.t);
        let b = self.fixes.first().map(|f| f.t);
        match (a, b) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn last_cue_t(&self) -> Option<f64> {
        self.fixes.last().map(|f| f.t).into_iter().chain(self.launch.as_ref().map(|l| l.t)).reduce(f64::max)
    }

    /// The warning our sensors gave: from the first cue to the strike, s.
    pub fn warning_s(&self) -> Option<f64> {
        Some(self.struck?.1 - self.first_cue_t()?)
    }

    /// In the water: its launch heard by a sonar, or a fix of it by one (a contact track taken for
    /// a weapon: its own domain's).
    pub fn in_water(&self) -> bool {
        match (&self.weapon, &self.track) {
            (Some(b), Some(_)) => b.domain == Domain::Subsurface,
            _ => self.launch.as_ref().is_some_and(Launch::heard) || self.fixes.iter().any(|f| f.sensor.ends_with(":sonar")),
        }
    }

    /// The domain its speed is judged in: under water when heard there, in the air when a
    /// measured height puts it over [`AIR_FROM_M`], else at the surface (the lower threshold, so
    /// a low weapon is not missed for want of a height).
    pub fn domain(&self) -> Domain {
        if let (Some(b), Some(_)) = (&self.weapon, &self.track) {
            return b.domain;
        }
        if self.in_water() {
            Domain::Subsurface
        } else if self.kin.and_then(|k| k.alt_m).is_some_and(|a| a >= AIR_FROM_M) {
            Domain::Air
        } else {
            Domain::Surface
        }
    }

    /// Its points in time order: the launch's flash (when the weapon left the shooter), then its
    /// fixes: (t, lat, lon, sigma).
    fn points(&self) -> Vec<(f64, f64, f64, f64)> {
        let mut p: Vec<(f64, f64, f64, f64)> = self.launch.iter().map(|l| (l.flash_t, l.lat, l.lon, l.sigma_m)).collect();
        p.extend(self.fixes.iter().map(|f| (f.t, f.lat, f.lon, f.sigma_m)));
        p.sort_by(|a, b| a.0.total_cmp(&b.0));
        p
    }

    /// The fix's distance from where this track says it would be, over its gate (1 or less
    /// passes); None outside the gate or the track's time.
    fn gate(&self, f: &Fix) -> Option<f64> {
        let (t, lat, lon, sigma, ve, vn, vs) = match &self.kin {
            // the line's speed error no less than a share of its speed: a weapon boosts, and a
            // line from two or three fixes, one a distant radar's, is less sure than its formal
            // error says
            Some(k) => (k.t, k.lat, k.lon, k.sigma_m, k.ve, k.vn, Some(k.speed_sigma.max(GATE_SPEED_SHARE * k.speed))),
            None => {
                let &(t, lat, lon, s) = self.points().last()?;
                (t, lat, lon, s, 0.0, 0.0, None)
            }
        };
        let dt = f.t - t;
        if !(-1.0..=LOST_AFTER_S).contains(&dt) {
            return None;
        }
        let (pe, pn) = (ve * dt, vn * dt);
        let (fe, fn_) = en(lat, lon, f.lat, f.lon);
        let d = (fe - pe).hypot(fn_ - pn);
        let g = match vs {
            Some(vs) => GATE_SIGMAS * (sigma * sigma + (vs * dt).powi(2) + f.sigma_m * f.sigma_m).sqrt() + GATE_SLACK_M,
            None => V_MAX_MPS * dt.abs() + GATE_SIGMAS * sigma.hypot(f.sigma_m) + GATE_SLACK_M,
        };
        let r = d / g;
        (r <= 1.0).then_some(r)
    }
}

/// Metres east and north of (lat0, lon0).
fn en(lat0: f64, lon0: f64, lat: f64, lon: f64) -> (f64, f64) {
    let m = 111_320.0;
    ((lon - lon0) * m * lat0.to_radians().cos(), (lat - lat0) * m)
}

/// (lat, lon) `e`, `n` metres from (lat0, lon0).
fn off(lat0: f64, lon0: f64, e: f64, n: f64) -> (f64, f64) {
    let m = 111_320.0;
    (lat0 + n / m, lon0 + e / (m * lat0.to_radians().cos()))
}

/// The constant-velocity line through `points` (t, lat, lon, sigma), each weighted by its error:
/// where at the last point's time, with its error, and the velocity with its speed's error. None
/// over less than [`MIN_SPAN_S`].
pub fn fit(points: &[(f64, f64, f64, f64)]) -> Option<Kin> {
    let first = points.first()?;
    let last = points.last()?;
    if last.0 - first.0 < MIN_SPAN_S {
        return None;
    }
    let (lat0, lon0) = (first.1, first.2);
    let xs: Vec<(f64, f64, f64, f64)> = points
        .iter()
        .map(|&(t, lat, lon, s)| {
            let (e, n) = en(lat0, lon0, lat, lon);
            (t, e, n, 1.0 / (s * s).max(1.0))
        })
        .collect();
    let w: f64 = xs.iter().map(|x| x.3).sum();
    let tb = xs.iter().map(|x| x.3 * x.0).sum::<f64>() / w;
    let eb = xs.iter().map(|x| x.3 * x.1).sum::<f64>() / w;
    let nb = xs.iter().map(|x| x.3 * x.2).sum::<f64>() / w;
    let stt: f64 = xs.iter().map(|x| x.3 * (x.0 - tb).powi(2)).sum();
    if stt <= 0.0 {
        return None;
    }
    let ve = xs.iter().map(|x| x.3 * (x.0 - tb) * (x.1 - eb)).sum::<f64>() / stt;
    let vn = xs.iter().map(|x| x.3 * (x.0 - tb) * (x.2 - nb)).sum::<f64>() / stt;
    let v_sigma = (1.0 / stt).sqrt();
    let dt = last.0 - tb;
    let (lat, lon) = off(lat0, lon0, eb + ve * dt, nb + vn * dt);
    Some(Kin {
        t: last.0,
        lat,
        lon,
        sigma_m: (1.0 / w + v_sigma * v_sigma * dt * dt).sqrt(),
        ve,
        vn,
        speed: ve.hypot(vn),
        speed_sigma: v_sigma,
        alt_m: None,
        vz: 0.0,
    })
}

/// A line's height from the fixes that measured one: at `k.t`, and its rate (a straight line
/// through two or more over [`MIN_SPAN_S`]; one alone, held).
fn with_height(mut k: Kin, fixes: &[Fix]) -> Kin {
    let h: Vec<(f64, f64)> = fixes.iter().filter_map(|f| f.alt_m.map(|a| (f.t, a))).collect();
    let Some(&(t_last, a_last)) = h.last() else { return k };
    if h.len() < 2 || t_last - h[0].0 < MIN_SPAN_S {
        (k.alt_m, k.vz) = (Some(a_last), 0.0);
        return k;
    }
    let n = h.len() as f64;
    let tb = h.iter().map(|x| x.0).sum::<f64>() / n;
    let ab = h.iter().map(|x| x.1).sum::<f64>() / n;
    let stt: f64 = h.iter().map(|x| (x.0 - tb).powi(2)).sum();
    let vz = h.iter().map(|x| (x.0 - tb) * (x.1 - ab)).sum::<f64>() / stt;
    (k.alt_m, k.vz) = (Some(ab + vz * (k.t - tb)), vz);
    k
}

/// One of ours as this picture knows it: where it believes it is, its height, its velocity from
/// its last two positions, and its domain when known.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ours {
    pub uid: u64,
    pub lat: f64,
    pub lon: f64,
    pub alt_m: f64,
    pub ve: f64,
    pub vn: f64,
    pub domain: Option<Domain>,
}

/// A contact track as the tracker holds it, for the weapon test: its estimate (when last seen),
/// height if measured, velocity and its error, domain, and the sensor that last saw it.
#[derive(Debug, Clone, PartialEq)]
pub struct ContactView {
    pub id: String,
    pub t: f64,
    pub lat: f64,
    pub lon: f64,
    pub alt_m: Option<f64>,
    pub ve: f64,
    pub vn: f64,
    pub vel_sigma: f64,
    pub sigma_m: f64,
    pub domain: Domain,
    pub sensor: String,
}

/// The one of ours a line (`k`) closes on, at `t` (ours' time): the one whose closest approach,
/// ahead and within [`CLOSING_HORIZON_S`], passes nearest relative to its gate (within
/// [`AIM_MISS_M`] or twice the line's error there): (uid, its time to the closest approach, the
/// miss). With the line's height (and its rate), the miss counts the heights at the closest
/// approach; without, it is judged across the ground.
pub fn closing(k: &Kin, ours: &[Ours], t: f64) -> Option<(u64, f64, f64)> {
    let lead = t - k.t;
    let (wlat, wlon) = off(k.lat, k.lon, k.ve * lead, k.vn * lead);
    ours.iter()
        .filter_map(|o| {
            let (re, rn) = en(o.lat, o.lon, wlat, wlon);
            let (ue, un) = (k.ve - o.ve, k.vn - o.vn);
            let vv = ue * ue + un * un;
            if vv < 1.0 {
                return None;
            }
            let tc = -(re * ue + rn * un) / vv;
            if tc <= 0.0 || tc > CLOSING_HORIZON_S {
                return None;
            }
            let miss_h = (re + ue * tc).hypot(rn + un * tc);
            let miss = miss_h.hypot(k.alt_m.map_or(0.0, |a| a + k.vz * (tc + lead) - o.alt_m));
            let sigma = (k.sigma_m.powi(2) + (k.speed_sigma * (tc + lead)).powi(2)).sqrt();
            let gate = AIM_MISS_M.max(2.0 * sigma);
            (miss <= gate).then_some((o.uid, tc, miss, miss / gate))
        })
        .min_by(|a, b| a.3.total_cmp(&b.3))
        .map(|(u, tc, miss, _)| (u, tc, miss))
}

/// Every weapon track, the next number, and ours as last placed.
#[derive(Debug, Default)]
pub struct Picture {
    pub tracks: Vec<WeaponTrack>,
    next_no: u64,
    ours: Vec<Ours>,
    ours_t: f64,
    ours_prev: HashMap<u64, (f64, f64, f64)>,
}

fn ms(t: f64) -> i64 {
    (t * 1000.0).round() as i64
}

fn r1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

impl Picture {
    fn push(&mut self) -> &mut WeaponTrack {
        self.next_no += 1;
        self.tracks.push(WeaponTrack::new(format!("W-{}", self.next_no)));
        self.tracks.last_mut().expect("pushed")
    }

    /// Where ours are at `t`: (uid, lat, lon as each believes, height, domain); each one's
    /// velocity from its last place.
    pub fn set_ours(&mut self, t: f64, ours: &[(u64, f64, f64, f64, Option<Domain>)]) {
        self.ours = ours
            .iter()
            .map(|&(uid, lat, lon, alt_m, domain)| {
                let (ve, vn) = match self.ours_prev.get(&uid) {
                    Some(&(t0, lat0, lon0)) if (0.2..=5.0).contains(&(t - t0)) => {
                        let (e, n) = en(lat0, lon0, lat, lon);
                        (e / (t - t0), n / (t - t0))
                    }
                    _ => self.ours.iter().find(|o| o.uid == uid).map_or((0.0, 0.0), |o| (o.ve, o.vn)),
                };
                Ours { uid, lat, lon, alt_m, ve, vn, domain }
            })
            .collect();
        self.ours_prev = ours.iter().map(|&(uid, lat, lon, _, _)| (uid, (t, lat, lon))).collect();
        self.ours_t = t;
    }

    /// A launch our sensors caught, reported as `report` (its act's report; a gun's later bursts
    /// share their episode's), its shooter the contact track `shooter` when one was placed on it:
    /// a new weapon track at the launch stage.
    pub fn launch_seen(&mut self, launch: Launch, report: Option<String>, shooter: Option<String>) {
        let w = self.push();
        w.launch = Some(launch);
        w.report = report;
        w.shooter = shooter;
    }

    /// Fixes of the weapon channel: each joins the in-flight weapon track it gates best on
    /// (position and velocity), or starts one.
    pub fn plots(&mut self, fixes: &[Fix]) {
        let mut fixes: Vec<&Fix> = fixes.iter().collect();
        fixes.sort_by(|a, b| a.t.total_cmp(&b.t));
        for f in fixes {
            let best = self
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, w)| w.status == Status::InFlight)
                .filter_map(|(i, w)| w.gate(f).map(|r| (i, r)))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i);
            let w = match best {
                Some(i) => &mut self.tracks[i],
                None => self.push(),
            };
            w.fixes.push(Fix { sigma_m: r1(f.sigma_m), ..f.clone() });
            if w.track.is_none() {
                w.kin = fit(&w.points()).map(|k| with_height(k, &w.fixes));
            }
        }
    }

    /// The contact tracks the tracker holds that pass the weapon test now (their speed over their
    /// domain's threshold, their line closing on one of ours): each is a weapon track (`track`),
    /// and gets a fix each time it is seen passing.
    pub fn contacts(&mut self, views: &[ContactView]) {
        for v in views {
            let k = Kin {
                t: v.t,
                lat: v.lat,
                lon: v.lon,
                sigma_m: v.sigma_m,
                ve: v.ve,
                vn: v.vn,
                speed: v.ve.hypot(v.vn),
                speed_sigma: v.vel_sigma,
                alt_m: v.alt_m,
                vz: 0.0,
            };
            let over = weapon_speed_mps(v.domain);
            if k.speed < over {
                continue;
            }
            let Some((uid, _, _)) = closing(&k, &self.ours, self.ours_t) else { continue };
            let i = self.tracks.iter().position(|w| w.status == Status::InFlight && w.track.as_deref() == Some(v.id.as_str()));
            let w = match i {
                Some(i) => &mut self.tracks[i],
                None => self.push(),
            };
            w.track = Some(v.id.clone());
            w.kin = Some(k);
            w.weapon = Some(WeaponBasis { speed_mps: k.speed, over_mps: over, domain: v.domain, from_launch: false, closing_on: Some(uid) });
            if w.fixes.last().is_none_or(|f| f.t < v.t) {
                w.fixes.push(Fix { t: v.t, lat: v.lat, lon: v.lon, sigma_m: r1(v.sigma_m), sensor: v.sensor.clone(), alt_m: v.alt_m });
            }
        }
    }

    /// Each weapon track in flight on what is known now: the weapon test, the vehicle it heads
    /// for, the class. `identified`: a contact track's kind when it is identified (the shooter).
    pub fn assess(&mut self, identified: &dyn Fn(&str) -> Option<String>) {
        let library = threat_library();
        let (ours, t_now) = (self.ours.clone(), self.ours_t);
        for w in self.tracks.iter_mut().filter(|w| w.status == Status::InFlight) {
            let close = w.kin.as_ref().and_then(|k| closing(k, &ours, t_now));
            if w.weapon.is_none()
                && let Some(k) = &w.kin
            {
                let domain = w.domain();
                let over = weapon_speed_mps(domain);
                if k.speed >= over && (w.launch.is_some() || close.is_some()) {
                    w.weapon = Some(WeaponBasis { speed_mps: k.speed, over_mps: over, domain, from_launch: w.launch.is_some(), closing_on: close.map(|c| c.0) });
                }
            }
            if w.weapon.is_some()
                && let Some((uid, tc, _)) = close
            {
                w.aim = Some(Aim { uid, eta_t: t_now + tc, basis: "track" });
            }
            // the class: among the classes that travel where it was sensed (the water or the air)
            // and, when told, launch as it did; the measured speed (and height) against the
            // library and the shooter's kind against what our side believes it may carry
            let water = w.in_water();
            let look = w.launch.as_ref().and_then(|l| l.look);
            let lib: Vec<ClassSig> = library
                .iter()
                .filter(|c| (c.medium == Domain::Subsurface) == water)
                .filter(|c| look.is_none_or(|lk| c.launch == lk))
                .copied()
                .collect();
            let reach = |c: &ClassSig| match (&w.launch, w.aim.and_then(|a| ours.iter().find(|o| o.uid == a.uid))) {
                (Some(l), Some(o)) => haversine_m(l.lat, l.lon, o.lat, o.lon) <= c.range_m,
                _ => true,
            };
            let speed_fit: Option<Vec<ClassSig>> = match (&w.weapon, &w.kin) {
                (Some(_), Some(k)) if k.speed_sigma <= SPEED_EVIDENCE_SHARE * k.speed => Some(
                    lib.iter()
                        .filter(|c| (c.speed_mps.0..=c.speed_mps.1).contains(&k.speed))
                        .filter(|c| k.alt_m.is_none_or(|a| (c.height_m.0..=c.height_m.1).contains(&a)))
                        .filter(|c| reach(c))
                        .copied()
                        .collect(),
                ),
                _ => None,
            };
            let shooter = w.shooter.as_deref().and_then(|s| identified(s).map(|kind| (s.to_string(), kind)));
            let carried: Vec<&str> = shooter.as_ref().map(|(_, kind)| carried_by(kind).to_vec()).unwrap_or_default();
            let mut candidates = lib.clone();
            if let Some(f) = &speed_fit {
                candidates.retain(|c| f.contains(c));
            }
            if !carried.is_empty() {
                candidates.retain(|c| carried.contains(&c.class));
            }
            let evidenced = speed_fit.is_some() || !carried.is_empty() || look.is_some();
            if evidenced
                && let [c] = candidates.as_slice()
                && w.class.as_ref().is_none_or(|a| a.class.class == c.class)
            {
                let mut evidence = Vec::new();
                if let (Some(_), Some(k)) = (&speed_fit, &w.kin) {
                    evidence.push(Evidence {
                        basis: "speed",
                        short: format!("{:.0} m/s", k.speed),
                        text: format!(
                            "measured {:.0} m/s (1-sigma {:.0}), within the {}'s {:.0} to {:.0} m/s",
                            k.speed, k.speed_sigma, c.class, c.speed_mps.0, c.speed_mps.1
                        ),
                    });
                    if let Some(a) = k.alt_m {
                        evidence.push(Evidence {
                            basis: "height",
                            short: format!("{a:.0} m up"),
                            text: format!("flying {a:.0} m up, within its {:.0} to {:.0} m", c.height_m.0, c.height_m.1),
                        });
                    }
                }
                if let Some((lk, l)) = look.zip(w.launch.as_ref()) {
                    evidence.push(Evidence {
                        basis: "launch",
                        short: lk.short().to_string(),
                        text: format!("{} told it as {}", l.sensors.join(", "), lk.text()),
                    });
                }
                if let Some((s, kind)) = shooter.as_ref().filter(|_| !carried.is_empty()) {
                    evidence.push(Evidence {
                        basis: "shooter",
                        short: format!("from {s}"),
                        text: format!("launched from {s}, identified as {kind}, which our side believes may carry the {}", c.class),
                    });
                }
                w.class = Some(Assessed { class: *c, evidence });
            }
            // with a class and no measured line: the only one of ours it could be fired at within
            // its reach of the launch, at its nominal speed
            if w.aim.is_none()
                && let (Some(a), Some(l)) = (&w.class, &w.launch)
            {
                let c = a.class;
                let within: Vec<(u64, f64)> = ours
                    .iter()
                    .filter(|o| o.domain.is_none_or(|d| c.targets.contains(&d)))
                    .map(|o| (o.uid, haversine_m(l.lat, l.lon, o.lat, o.lon)))
                    .filter(|(_, d)| *d <= c.range_m)
                    .collect();
                if let [(uid, d)] = within.as_slice() {
                    w.aim = Some(Aim { uid: *uid, eta_t: l.flash_t + d / c.nominal_mps, basis: "reach" });
                }
            }
        }
    }

    /// One of ours (`uid`) reports it was struck at bridge time `t`: the strike is put on the
    /// weapon track heading for it (the expected strike nearest), else on the one whose last cue
    /// (within [`STRIKE_LINK_S`]) could have reached it, else on a new track (a weapon nobody saw
    /// is in the picture at its strike). That track ends expended. Returns it.
    pub fn struck(&mut self, uid: u64, t: f64) -> WeaponTrack {
        let victim = self.ours.iter().find(|o| o.uid == uid).map(|o| (o.lat, o.lon));
        let open = |w: &WeaponTrack| w.status != Status::Expended && w.struck.is_none();
        let aimed = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, w)| open(w) && w.aim.is_some_and(|a| a.uid == uid))
            .min_by(|a, b| {
                let d = |w: &WeaponTrack| w.aim.map_or(f64::INFINITY, |a| (a.eta_t - t).abs());
                d(a.1).total_cmp(&d(b.1))
            })
            .map(|(i, _)| i);
        let reached = || {
            self.tracks
                .iter()
                .enumerate()
                .filter(|(_, w)| open(w) && w.track.is_none())
                .filter(|(_, w)| w.last_cue_t().is_some_and(|c| c <= t + 1.0 && t - c <= STRIKE_LINK_S))
                .filter(|(_, w)| {
                    let (Some(&(t0, lat, lon, s)), Some((vlat, vlon))) = (w.points().last(), victim) else { return true };
                    haversine_m(lat, lon, vlat, vlon) <= V_MAX_MPS * (t - t0).max(0.0) + GATE_SIGMAS * s + GATE_SLACK_M
                })
                .min_by(|a, b| a.1.first_cue_t().unwrap_or(f64::INFINITY).total_cmp(&b.1.first_cue_t().unwrap_or(f64::INFINITY)))
                .map(|(i, _)| i)
        };
        let i = match aimed.or_else(reached) {
            Some(i) => i,
            None => {
                self.push();
                self.tracks.len() - 1
            }
        };
        let w = &mut self.tracks[i];
        w.struck = Some((uid, t));
        w.status = Status::Expended;
        w.ended_t = Some(t);
        w.clone()
    }

    /// End lost what has had no cue for [`LOST_AFTER_S`]; forget what ended [`SHOWN_S`] ago.
    /// Whether anything changed.
    pub fn age(&mut self, now: f64) -> bool {
        let before = self.tracks.clone();
        for w in &mut self.tracks {
            if w.status == Status::InFlight && w.last_cue_t().is_some_and(|t| now - t > LOST_AFTER_S) {
                w.status = Status::Lost;
                w.ended_t = Some(now);
            }
        }
        self.tracks.retain(|w| w.ended_t.is_none_or(|e| now - e <= SHOWN_S));
        self.tracks != before
    }

    /// The feed's `weapons` (engine ms): each track with what our sensors have of it, at its stage
    /// (docs/OPPONENT-WEAPONS.md section 5 lists the fields).
    pub fn json(&self) -> Vec<Value> {
        self.tracks
            .iter()
            .map(|w| {
                json!({
                    "id": w.id,
                    "report": w.report,
                    "stage": w.stage().as_str(),
                    "class": w.class.as_ref().map(|a| a.class.class),
                    "class_symbol": w.class.as_ref().map(|a| a.class.symbol),
                    "evidence": w.class.as_ref().map(|a| a.evidence.iter().map(|e| json!({
                        "basis": e.basis, "short": e.short, "text": e.text,
                    })).collect::<Vec<_>>()).unwrap_or_default(),
                    "weapon_by": w.weapon.map(|b| json!({
                        "speed_mps": r1(b.speed_mps), "over_mps": b.over_mps, "domain": b.domain.as_str(),
                        "from_launch": b.from_launch, "closing_on": b.closing_on.map(|u| u.to_string()),
                    })),
                    "speed_mps": w.kin.map(|k| r1(k.speed)),
                    "speed_sigma_mps": w.kin.map(|k| r1(k.speed_sigma)),
                    "velocity_mps": w.kin.map(|k| [r1(k.ve), r1(k.vn)]),
                    "kin_t": w.kin.map(|k| ms(k.t)),
                    "kin_point": w.kin.map(|k| [k.lon, k.lat]),
                    "track": w.track,
                    "shooter": w.shooter,
                    "aimed_at": w.aim.map(|a| a.uid.to_string()),
                    "aim_basis": w.aim.map(|a| a.basis),
                    "eta_t": w.aim.map(|a| ms(a.eta_t)),
                    "nominal_mps": w.class.as_ref().map(|a| a.class.nominal_mps),
                    "launch": w.launch.as_ref().map(|l| json!({
                        "t": ms(l.t), "flash_t": ms(l.flash_t), "point": [l.lon, l.lat], "sigma_m": l.sigma_m,
                        "sensors": l.sensors,
                        "observer": l.observer.map(|(u, lat, lon)| json!({ "uid": u.to_string(), "point": [lon, lat] })),
                        "bearing_deg": l.observer.map(|(_, lat, lon)| bearing_deg(lat, lon, l.lat, l.lon).round()),
                        "look": l.look.map(LaunchLook::as_str),
                    })),
                    "fixes": w.fixes.iter().map(|f| json!({
                        "t": ms(f.t), "point": [f.lon, f.lat], "sigma_m": f.sigma_m, "sensor": f.sensor,
                        "alt_m": f.alt_m.map(r1),
                    })).collect::<Vec<_>>(),
                    "status": w.status.as_str(),
                    "ended_t": w.ended_t.map(ms),
                    "struck": w.struck.map(|(u, t)| json!({ "uid": u.to_string(), "t": ms(t) })),
                    "first_cue_t": w.first_cue_t().map(ms),
                    "warning_s": w.warning_s().map(r1),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tutorial's geometry: the boat, HUNTER 1 1 km east of it on the water, REAPER 1 7.6 km
    /// up off to the south-west; an ATGM-class missile at 250 m/s takes 4 s.
    const BOAT: (f64, f64) = (37.812, -122.495);
    const T: f64 = 100.0;

    fn east(m: f64) -> (f64, f64) {
        off(BOAT.0, BOAT.1, m, 0.0)
    }

    fn ours() -> Vec<(u64, f64, f64, f64, Option<Domain>)> {
        let (hlat, hlon) = east(1_000.0);
        let (rlat, rlon) = off(BOAT.0, BOAT.1, -3_000.0, -2_000.0);
        vec![(1, hlat, hlon, 0.0, Some(Domain::Surface)), (2, rlat, rlon, 7_600.0, Some(Domain::Air))]
    }

    fn picture_with(ours: &[(u64, f64, f64, f64, Option<Domain>)]) -> Picture {
        let mut p = Picture::default();
        p.set_ours(T - 1.0, ours);
        p.set_ours(T, ours);
        p
    }

    fn picture() -> Picture {
        picture_with(&ours())
    }

    /// A launch HUNTER 1's camera saw at the boat, told as `look` (None: seen, not told).
    fn launch_told(look: Option<LaunchLook>) -> Launch {
        Launch { t: T + 0.6, flash_t: T + 0.2, lat: BOAT.0, lon: BOAT.1, sigma_m: 20.0, sensors: vec!["1:eo_ir".into()], observer: Some((1, east(1_000.0).0, east(1_000.0).1)), look }
    }

    /// The tutorial's launch: a missile's, told.
    fn launch() -> Launch {
        launch_told(Some(LaunchLook::Missile))
    }

    /// the weapon at `t` (flash at T + 0.2, 250 m/s east), fixed by `sensor` with `sigma`
    fn fix(t: f64, sigma: f64, sensor: &str) -> Fix {
        let (lat, lon) = east(250.0 * (t - T - 0.2));
        Fix { t, lat, lon, sigma_m: sigma, sensor: sensor.into(), alt_m: None }
    }

    /// Something at `speed` m/s east from `x0` m east of the boat at T, fixed by `sensor` each
    /// `every` s from T + 1 to T + `until` with `sigma`, at the height `alt` when measured.
    fn run(speed: f64, x0: f64, every: f64, until: f64, sigma: f64, sensor: &str, alt: Option<f64>) -> Vec<Fix> {
        let mut out = Vec::new();
        let mut dt = 1.0;
        while dt <= until + 1e-9 {
            let (lat, lon) = east(x0 + speed * dt);
            out.push(Fix { t: T + dt, lat, lon, sigma_m: sigma, sensor: sensor.into(), alt_m: alt });
            dt += every;
        }
        out
    }

    const NOBODY: &dyn Fn(&str) -> Option<String> = &|_| None;
    const BOAT_ID: &dyn Fn(&str) -> Option<String> = &|t| (t == "T-0004").then(|| "small_fast_boat".into());

    fn bases(w: &WeaponTrack) -> Vec<&'static str> {
        w.class.as_ref().map(|a| a.evidence.iter().map(|e| e.basis).collect()).unwrap_or_default()
    }

    /// A seen launch is a launch and nothing more (no class, speed or target); a camera's fix two
    /// seconds on joins it by gating, its speed from the flash says weapon, and that speed, known
    /// to some 5 %, with the launch told as a missile's, fits the ATGM-class alone (a rocket's
    /// band holds the speed too, a rocket's launch does not look like this): the class with its
    /// evidence, the line closing on HUNTER 1 (not on REAPER 1 overhead), the strike expected at
    /// the flash plus 4 s; the strike HUNTER 1 reports is put on it.
    #[test]
    fn a_launch_then_fixes_climb_the_ladder() {
        let mut p = picture();
        p.launch_seen(launch(), Some("A-1".into()), None);
        p.assess(NOBODY);
        let w = &p.tracks[0];
        assert_eq!((w.stage(), w.kin, w.aim, w.class.is_some()), (Stage::Launch, None, None, false));
        let j = &p.json()[0];
        assert_eq!((j["class"].clone(), j["aimed_at"].clone(), j["speed_mps"].clone()), (Value::Null, Value::Null, Value::Null));
        assert_eq!(j["launch"]["bearing_deg"], 270.0, "from HUNTER 1, the flash due west");
        assert_eq!(j["launch"]["look"], "missile");
        p.set_ours(T + 2.2, &ours());
        p.plots(&[fix(T + 2.2, 22.0, "1:eo_ir")]);
        p.assess(NOBODY);
        assert_eq!(p.tracks.len(), 1, "the fix joined the launch");
        let w = &p.tracks[0];
        let k = w.kin.expect("a line");
        assert!((k.speed - 250.0).abs() < 1.0 && k.speed_sigma < 20.0, "{k:?}");
        assert_eq!(w.stage(), Stage::Class);
        assert_eq!(w.weapon.map(|b| (b.domain, b.over_mps)), Some((Domain::Surface, 60.0)), "no height measured: judged at the surface");
        let a = w.class.as_ref().expect("class");
        assert_eq!((a.class.class, a.evidence[0].basis, a.evidence[0].short.as_str()), ("ATGM-class", "speed", "250 m/s"));
        assert_eq!(bases(w), vec!["speed", "launch"]);
        let aim = w.aim.expect("aimed");
        assert_eq!((aim.uid, aim.basis), (1, "track"));
        assert!((aim.eta_t - (T + 4.2)).abs() < 0.1, "{aim:?}");
        let s = p.struck(1, T + 4.2);
        assert_eq!((s.id.as_str(), s.status, s.report.as_deref()), ("W-1", Status::Expended, Some("A-1")));
    }

    /// A class only on evidence: one radar plot from afar (150 m) puts the speed at some 35 %, a
    /// weapon but no class (a missile's launch is an ATGM's or an air-launched one's); an
    /// identified shooter our side believes may carry ATGMs (and rockets and guns: of those, only
    /// the ATGM launches like a missile) names it (and, with no line, its reach picks HUNTER 1, the
    /// only one of ours on the water within 5.5 km); a shooter merely detected, or of a kind
    /// believed to carry nothing, names nothing.
    #[test]
    fn a_class_needs_evidence() {
        let mut p = picture();
        p.launch_seen(launch(), Some("A-1".into()), Some("T-0004".into()));
        p.plots(&[fix(T + 2.0, 150.0, "2:radar")]);
        p.assess(NOBODY);
        let w = &p.tracks[0];
        assert_eq!(w.stage(), Stage::Weapon, "{:?}", w.kin);
        assert!(w.class.is_none());
        p.assess(BOAT_ID);
        let a = p.tracks[0].class.as_ref().expect("by its shooter and its launch");
        assert_eq!(a.class.class, "ATGM-class");
        assert_eq!(bases(&p.tracks[0]), vec!["launch", "shooter"]);
        assert_eq!(a.evidence[1].short, "from T-0004");
        // a launch alone from an identified shooter: the class, and the target by reach
        let mut q = picture();
        q.launch_seen(launch(), Some("A-1".into()), Some("T-0004".into()));
        q.assess(BOAT_ID);
        let aim = q.tracks[0].aim.expect("by reach");
        assert_eq!((q.tracks[0].stage(), aim.uid, aim.basis), (Stage::Class, 1, "reach"));
        assert!((aim.eta_t - (T + 4.2)).abs() < 0.1);
        // a shooter not identified (or of a kind believed to carry nothing): no class
        let mut r = picture();
        r.launch_seen(launch(), Some("A-1".into()), Some("T-0004".into()));
        r.assess(&|_| None);
        r.assess(&|_| Some("fishing_vessel".into()));
        assert_eq!((r.tracks[0].stage(), r.tracks[0].aim), (Stage::Launch, None));
    }

    /// With D3 an armed boat is identified only as a small fast boat, which our side believes may
    /// carry ATGMs, rockets and guns: a launch from one not told waits (launch, then a weapon of
    /// unknown type once a camera's fix gives 250 m/s, a speed inside both the ATGM's and the
    /// rocket's bands); told, the launch's kind leaves one: a muzzle flash is the gun at once
    /// (aimed by its reach), a rocket's launch the rocket.
    #[test]
    fn an_identified_small_fast_boat_waits_for_evidence() {
        let mut p = picture();
        p.launch_seen(launch_told(None), Some("A-1".into()), Some("T-0004".into()));
        p.assess(BOAT_ID);
        assert_eq!((p.tracks[0].stage(), p.tracks[0].aim), (Stage::Launch, None), "three classes it may carry, nothing to choose");
        p.plots(&[fix(T + 2.2, 22.0, "1:eo_ir")]);
        p.assess(BOAT_ID);
        let w = &p.tracks[0];
        assert!(w.kin.is_some_and(|k| k.speed_sigma < 0.1 * k.speed), "{:?}", w.kin);
        assert_eq!((w.stage(), w.class.is_none()), (Stage::Weapon, true), "250 m/s: an ATGM's or a rocket's");
        for (look, class) in [(LaunchLook::Muzzle, "Gun-class"), (LaunchLook::Rocket, "Rocket-class"), (LaunchLook::Missile, "ATGM-class")] {
            let mut q = picture();
            q.launch_seen(launch_told(Some(look)), Some("A-1".into()), Some("T-0004".into()));
            q.assess(BOAT_ID);
            let w = &q.tracks[0];
            assert_eq!((w.stage(), w.class.as_ref().map(|a| a.class.class)), (Stage::Class, Some(class)), "{look:?}");
            assert_eq!(bases(w), vec!["launch", "shooter"]);
        }
        // the gun by reach: HUNTER 1 1 km off, its round at the nominal 800 m/s
        let mut g = picture();
        g.launch_seen(launch_told(Some(LaunchLook::Muzzle)), Some("A-1".into()), Some("T-0004".into()));
        g.assess(BOAT_ID);
        let aim = g.tracks[0].aim.expect("by reach");
        assert_eq!((aim.uid, aim.basis), (1, "reach"));
        assert!((aim.eta_t - (T + 0.2 + 1_000.0 / 800.0)).abs() < 0.05, "{aim:?}");
        // a muzzle flash names the gun by itself: nothing else launches so
        let mut m = picture();
        m.launch_seen(launch_told(Some(LaunchLook::Muzzle)), Some("A-1".into()), None);
        m.assess(NOBODY);
        assert_eq!((m.tracks[0].class.as_ref().map(|a| a.class.class), bases(&m.tracks[0])), (Some("Gun-class"), vec!["launch"]));
    }

    /// With its shooter's track not identified, an operator sees each rung in turn: the launch
    /// (bearing, no class or target), then a weapon of unknown type once a distant radar's plot
    /// gives a loose speed, then the class from the speed with the launch's kind once a camera's
    /// fix pins it.
    #[test]
    fn an_unidentified_shooter_leaves_the_class_to_the_speed() {
        let mut p = picture();
        let detected_only: &dyn Fn(&str) -> Option<String> = &|_| None;
        p.launch_seen(launch(), Some("A-1".into()), Some("T-0004".into()));
        p.assess(detected_only);
        assert_eq!((p.tracks[0].stage(), p.tracks[0].aim), (Stage::Launch, None));
        p.plots(&[fix(T + 1.6, 150.0, "2:radar")]);
        p.assess(detected_only);
        assert_eq!((p.tracks[0].stage(), p.tracks[0].class.is_none()), (Stage::Weapon, true));
        p.plots(&[fix(T + 3.0, 20.0, "1:eo_ir")]);
        p.assess(detected_only);
        let w = &p.tracks[0];
        let a = w.class.as_ref().expect("by its speed");
        assert_eq!((p.tracks.len(), w.stage(), a.class.class), (1, Stage::Class, "ATGM-class"));
        assert_eq!(bases(w), vec!["speed", "launch"], "no shooter evidence");
    }

    /// The threat library's bands make a speed fit a choice: with no launch seen and no shooter,
    /// three tight fixes closing on HUNTER 1 at 510 m/s are an air-launched missile, at 380 a
    /// rocket, at 160 an ATGM, at 250 a weapon of unknown type (inside both the ATGM's and the
    /// rocket's bands). A sonar's fixes of something running at 23 m/s are a torpedo: over the
    /// water's threshold (20 m/s), in the torpedo's band, and only the torpedo runs in the water;
    /// the same speed seen above the water is no weapon (under the surface's 60).
    #[test]
    fn a_speed_fit_chooses_between_classes() {
        for (speed, want) in [(510.0, Some("Air missile-class")), (380.0, Some("Rocket-class")), (160.0, Some("ATGM-class")), (250.0, None)] {
            let mut p = picture();
            p.plots(&run(speed, -2_000.0, 1.0, 3.0, 10.0, "1:eo_ir", None));
            p.assess(NOBODY);
            assert_eq!(p.tracks.len(), 1, "{speed}");
            let w = &p.tracks[0];
            assert_eq!(w.weapon.map(|b| b.closing_on), Some(Some(1)), "{speed}: {:?}", w.kin);
            assert_eq!(w.class.as_ref().map(|a| a.class.class), want, "{speed}");
            if want.is_some() {
                assert_eq!(bases(w), vec!["speed"], "{speed}");
            }
        }
        // half a minute of a sonar's fixes, judged when the last came in
        let mut p = picture();
        p.plots(&run(23.0, -500.0, 1.0, 30.0, 150.0, "1:sonar", None));
        p.set_ours(T + 30.0, &ours());
        p.assess(NOBODY);
        let w = &p.tracks[0];
        assert_eq!((p.tracks.len(), w.in_water(), w.weapon.map(|b| (b.domain, b.over_mps))), (1, true, Some((Domain::Subsurface, 20.0))));
        assert_eq!(w.class.as_ref().map(|a| (a.class.class, a.class.symbol)), Some(("Torpedo-class", "torpedo")), "{:?}", w.kin);
        let mut q = picture();
        q.plots(&run(23.0, -500.0, 1.0, 30.0, 150.0, "2:radar", None));
        q.set_ours(T + 30.0, &ours());
        q.assess(NOBODY);
        assert!(q.tracks[0].weapon.is_none(), "23 m/s on the water is a boat's speed");
    }

    /// A missile at an aircraft's height, plotted by a height-finding radar at 3 km up and 500 m/s,
    /// passes the air's weapon test (150 m/s), closes on the aircraft at its height and not on
    /// HUNTER 1 3 km below its line, and its speed and height make it an air-launched missile.
    /// At 120 m/s the same line is no weapon in the air, though without its height it would be
    /// one at the surface's threshold (60 m/s), closing on HUNTER 1 across the ground.
    #[test]
    fn a_missile_at_an_aircraft_height_passes_the_air_weapon_test() {
        let mut ours = ours();
        let (alat, alon) = east(3_000.0);
        ours.push((3, alat, alon, 3_000.0, Some(Domain::Air)));
        let mut p = picture_with(&ours);
        p.plots(&run(500.0, -2_000.0, 1.0, 3.0, 30.0, "4:radar", Some(3_000.0)));
        p.assess(NOBODY);
        let w = &p.tracks[0];
        assert_eq!(w.weapon.map(|b| (b.domain, b.over_mps, b.closing_on)), Some((Domain::Air, 150.0, Some(3))), "{:?}", w.kin);
        assert_eq!(w.aim.map(|a| a.uid), Some(3));
        assert_eq!(w.class.as_ref().map(|a| a.class.class), Some("Air missile-class"));
        assert_eq!(bases(w), vec!["speed", "height"]);
        let mut slow = picture_with(&ours);
        slow.plots(&run(120.0, -2_000.0, 1.0, 3.0, 30.0, "4:radar", Some(3_000.0)));
        slow.assess(NOBODY);
        assert!(slow.tracks[0].weapon.is_none(), "{:?}", slow.tracks[0].weapon);
        let mut flat = picture_with(&ours);
        flat.plots(&run(120.0, -2_000.0, 1.0, 3.0, 30.0, "2:radar", None));
        flat.assess(NOBODY);
        assert_eq!(flat.tracks[0].weapon.map(|b| (b.domain, b.closing_on)), Some((Domain::Surface, Some(1))));
    }

    /// The honest confusion: a jet the tracker holds at 220 m/s with no height (a 2D radar's),
    /// running at HUNTER 1, is taken for a weapon of unknown type (its speed inside both the
    /// ATGM's and the rocket's bands names neither); the same jet 3 km up, or at 90 m/s, is not.
    /// Two fixes of a weapon with no launch seen are a weapon only once they close on one of ours.
    #[test]
    fn a_fast_aircraft_running_at_ours_is_taken_for_a_weapon() {
        let jet = |alt: Option<f64>, speed: f64| {
            let (lat, lon) = east(-2_000.0);
            ContactView { id: "T-0009".into(), t: T, lat, lon, alt_m: alt, ve: speed, vn: 0.0, vel_sigma: 5.0, sigma_m: 60.0, domain: Domain::Air, sensor: "1:radar".into() }
        };
        let mut p = picture();
        p.contacts(&[jet(None, 220.0)]);
        p.assess(NOBODY);
        let w = &p.tracks[0];
        assert_eq!((w.track.as_deref(), w.stage(), w.aim.map(|a| a.uid)), (Some("T-0009"), Stage::Weapon, Some(1)));
        assert!(w.class.is_none(), "heightless, its speed fits two classes: {:?}", w.class);
        let mut q = picture();
        q.contacts(&[jet(Some(3_000.0), 220.0), jet(None, 90.0)]);
        assert!(q.tracks.is_empty(), "{:?}", q.tracks);
        // a lone fix is a plot; a second, 250 m/s on and closing on HUNTER 1, makes a weapon
        let mut r = picture();
        r.plots(&[fix(T + 1.0, 80.0, "2:radar")]);
        r.assess(NOBODY);
        assert_eq!(r.tracks[0].stage(), Stage::Plot);
        r.plots(&[fix(T + 2.0, 80.0, "2:radar")]);
        r.assess(NOBODY);
        let w = &r.tracks[0];
        assert_eq!((r.tracks.len(), w.stage(), w.weapon.and_then(|b| b.closing_on)), (1, Stage::Weapon, Some(1)));
    }
}
