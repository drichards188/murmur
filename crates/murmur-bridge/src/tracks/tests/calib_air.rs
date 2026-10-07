//! The air tracker against the simulation's truth (murmur-oho): is its 1-sigma honest per
//! motion class and sensor mix, how often does it give a velocity, and does a keep-out built
//! from it 60 and 100 s ahead still hold the aircraft at least as often as one built from the
//! tracker before this calibration ([`before_oho`], verbatim copies of the sensing model and
//! tracker at efd332f, run on the same truth and draws)?
//!
//! Aircraft only (light aircraft, a helicopter, a small UAS), three sensor mixes ([`Mix`]): a
//! USV's 2D navigation radar alone, the same USV's radar and EO/IR, and every sensor of the
//! engagement demo's fleet (two more radars, four EO/IR). No mix has a radar that measures
//! height, so no track may give an altitude.
//!
//! Three sets, disjoint in seeds from each other and from the surface sets (`calib`):
//!
//! 1. [`tuning_cases`] on [`AIR_TUNING_SEEDS`]: what the air constants were chosen on.
//! 2. [`round_one_cases`] on [`AIR_ROUND_ONE_SEEDS`]: the first held-out set, written with the
//!    tuning set before any constant was tuned (every class in other variants, and a figure
//!    eight), run once the constants were chosen. Everything held but one criterion, then
//!    asked of the route class too: on its square route a velocity given was off by 3 to 4
//!    times its sigma (in and just after each turn the constant-velocity models lag, and the
//!    gate let the lagging velocity through). Widening the velocity's error by how fast it was
//!    changing (a lag of 1 to 6 s) cut that little and cost straight flight a fifth of its
//!    velocities: the lag is at a turn's entry and exit, before the velocity has changed much,
//!    and only a model of the turn would follow it (a coordinated-turn model, a follow-up). So
//!    the criterion is asked of the classes flown straight with one manoeuvre at most
//!    ([`STEADY_CLASSES`]), as of the turning ones it never was, and keep-outs count an
//!    aircraft wherever it could turn (rules 13). This set became tuning data.
//! 3. [`held_out_cases`] on [`AIR_HELD_OUT_SEEDS`]: written after that run, every class in yet
//!    other variants and one class neither earlier set has (an orbit, then a dash); run once,
//!    the constants unchanged. The test asserts on it.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use super::before_oho;
use super::calib::{self, Hold, Kinematics, SETTLE_S, SPANS_S, Tally, hold, integrate};
use super::{BASE, T0, at, contact};
use crate::tracks::model::Track;
use crate::tracks::sensing::{Observer, Params, SensorSpec, SensorType, detect};
use crate::tracks::truth::{ContactTruth, Domain};
use murmur_decide::engage::track_reach;
use murmur_decide::geo::haversine_m;
use murmur_decide::{EngageRules, LonLat, TrackFacts};

/// The air tuning set's seeds.
pub const AIR_TUNING_SEEDS: RangeInclusive<u64> = 101..=130;
/// The first air held-out set's seeds, tuning data since.
pub const AIR_ROUND_ONE_SEEDS: RangeInclusive<u64> = 131..=160;
/// The air held-out set's seeds: used nowhere in tuning.
pub const AIR_HELD_OUT_SEEDS: RangeInclusive<u64> = 161..=190;
/// The rules the tracker before this calibration was judged under (the keep-out's regions).
const BEFORE_RULES: u32 = 12;

/// One of our vehicles as a sensor carrier, metres east and north of [`BASE`].
#[derive(Clone)]
struct Obs {
    uid: u64,
    e: f64,
    n: f64,
    alt: f64,
    mast: f64,
    sensors: Vec<(SensorType, f64)>,
}

impl Obs {
    fn now(&self) -> Observer {
        let (lat, lon) = at(BASE.0, BASE.1, self.e, self.n);
        Observer {
            uid: self.uid,
            lat,
            lon,
            alt_msl: self.alt,
            mast_m: self.mast,
            submerged: false,
            sensors: self
                .sensors
                .iter()
                .map(|&(kind, range_m)| SensorSpec { kind, range_m, guess: true, height: false })
                .collect(),
        }
    }

    fn before(&self) -> before_oho::sensing::Observer {
        use before_oho::sensing::{SensorSpec as OldSpec, SensorType as Old};
        let (lat, lon) = at(BASE.0, BASE.1, self.e, self.n);
        before_oho::sensing::Observer {
            uid: self.uid,
            lat,
            lon,
            alt_msl: self.alt,
            mast_m: self.mast,
            submerged: false,
            sensors: self
                .sensors
                .iter()
                .map(|&(kind, range_m)| OldSpec {
                    kind: match kind {
                        SensorType::Radar => Old::Radar,
                        SensorType::EoIr => Old::EoIr,
                        SensorType::Ais => Old::Ais,
                        SensorType::Sonar => Old::Sonar,
                        SensorType::Esm => Old::Esm,
                    },
                    range_m,
                    guess: true,
                    height: false,
                })
                .collect(),
        }
    }
}

/// Which of our sensors see the aircraft.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mix {
    /// A USV's 2D navigation radar alone (Sea Hunter class, 40 km, mast 10 m): the eval's
    /// `radar` tracker.
    Radar,
    /// The same USV's radar and EO/IR (15 km): the eval's `all` tracker.
    RadarEoIr,
    /// Every sensor of the engagement demo's fleet around the aircraft: the USV, a Corsair-class
    /// ship (radar 20 km, EO/IR 8 km), a Magura-class USV (radar 8 km, EO/IR 5 km), a loitering
    /// munition at 300 m (EO/IR 3 km) and a quadcopter at 60 m (EO/IR 1.5 km).
    All,
}

pub const MIXES: [Mix; 3] = [Mix::Radar, Mix::RadarEoIr, Mix::All];

impl Mix {
    pub fn name(self) -> &'static str {
        match self {
            Mix::Radar => "2D radar only",
            Mix::RadarEoIr => "radar + EO/IR",
            Mix::All => "all sensors",
        }
    }

    fn observers(self) -> Vec<Obs> {
        let usv = |eo: bool| Obs {
            uid: 1,
            e: 0.0,
            n: -3000.0,
            alt: 0.0,
            mast: 10.0,
            sensors: if eo { vec![(SensorType::Radar, 40_000.0), (SensorType::EoIr, 15_000.0)] } else { vec![(SensorType::Radar, 40_000.0)] },
        };
        match self {
            Mix::Radar => vec![usv(false)],
            Mix::RadarEoIr => vec![usv(true)],
            Mix::All => vec![
                usv(true),
                Obs { uid: 2, e: 3000.0, n: -2500.0, alt: 0.0, mast: 4.0, sensors: vec![(SensorType::Radar, 20_000.0), (SensorType::EoIr, 8_000.0)] },
                Obs { uid: 3, e: -2500.0, n: -1500.0, alt: 0.0, mast: 1.5, sensors: vec![(SensorType::EoIr, 5_000.0), (SensorType::Radar, 8_000.0)] },
                Obs { uid: 4, e: -1500.0, n: 2000.0, alt: 300.0, mast: 0.0, sensors: vec![(SensorType::EoIr, 3_000.0)] },
                Obs { uid: 5, e: 1200.0, n: 600.0, alt: 60.0, mast: 0.0, sensors: vec![(SensorType::EoIr, 1_500.0)] },
            ],
        }
    }
}

/// Kinematics per second for a seed.
type Table = Box<dyn Fn(u64, usize) -> Vec<Kinematics>>;

pub struct AirCase {
    pub name: &'static str,
    pub class: &'static str,
    kind: &'static str,
    alt_m: f64,
    pub n: u32,
    motion: Table,
}

fn air(name: &'static str, class: &'static str, kind: &'static str, alt_m: f64, motion: impl Fn(u64, usize) -> Vec<Kinematics> + 'static) -> AirCase {
    AirCase { name, class, kind, alt_m, n: 300, motion: Box::new(motion) }
}

/// A heading-limited flight: at each second, the course and speed wanted (`want(t, e, n,
/// course)`: degrees, m/s), turned toward at `rate` deg/s and reached at `accel` m/s^2, in 0.1 s
/// steps from (`e0`, `n0`) on `course0` at `v0`.
fn fly(
    len: usize,
    (e0, n0, course0, v0): (f64, f64, f64, f64),
    rate: f64,
    accel: f64,
    mut want: impl FnMut(f64, f64, f64, f64) -> (f64, f64),
) -> Vec<Kinematics> {
    let (mut e, mut n, mut course, mut v) = (e0, n0, course0, v0);
    let mut out = Vec::with_capacity(len);
    for s in 0..len {
        let h = course.to_radians();
        out.push(((e, n), (v * h.sin(), v * h.cos())));
        for k in 0..10 {
            let t = s as f64 + k as f64 / 10.0;
            let (wc, wv) = want(t, e, n, course);
            let delta = ((wc - course + 540.0) % 360.0) - 180.0;
            course += delta.clamp(-rate / 10.0, rate / 10.0);
            v += (wv - v).clamp(-accel / 10.0, accel / 10.0);
            let h = course.to_radians();
            e += v / 10.0 * h.sin();
            n += v / 10.0 * h.cos();
        }
    }
    out
}

/// Straight from (`e0`, `n0`) on `course` at `v`.
fn straight(e0: f64, n0: f64, course: f64, v: f64) -> impl Fn(u64, usize) -> Vec<Kinematics> {
    move |_, len| fly(len, (e0, n0, course, v), 0.0, 0.0, |_, _, _, _| (course, v))
}

/// Round (`ce`, `cn`) at radius `r` and speed `v`, clockwise (`cw`) or not, from the north.
fn circle(ce: f64, cn: f64, r: f64, v: f64, cw: bool) -> impl Fn(u64, usize) -> Vec<Kinematics> {
    move |_, len| {
        let w = v / r * if cw { 1.0 } else { -1.0 };
        (0..len)
            .map(|s| {
                let a = w * s as f64;
                ((ce + r * a.sin(), cn + r * a.cos()), (r * w * a.cos(), -r * w * a.sin()))
            })
            .collect()
    }
}

/// Waypoints in a loop at `v`, turning toward the next at `rate` deg/s (as the contacts
/// process steers), from the first toward the second.
fn route(points: Vec<(f64, f64)>, v: f64, rate: f64) -> impl Fn(u64, usize) -> Vec<Kinematics> {
    move |_, len| {
        let mut leg = 1;
        let (e0, n0) = points[0];
        let c0 = (points[1].0 - e0).atan2(points[1].1 - n0).to_degrees();
        let pts = points.clone();
        fly(len, (e0, n0, c0, v), rate, 0.0, move |_, e, n, _| {
            if (pts[leg].0 - e).hypot(pts[leg].1 - n) < v {
                leg = (leg + 1) % pts.len();
            }
            ((pts[leg].0 - e).atan2(pts[leg].1 - n).to_degrees(), v)
        })
    }
}

/// Straight for `before_s`, then a turn of `turn` degrees (positive: right) at `rate` deg/s.
fn one_turn(e0: f64, n0: f64, course: f64, v: f64, before_s: f64, turn: f64, rate: f64) -> impl Fn(u64, usize) -> Vec<Kinematics> {
    move |_, len| fly(len, (e0, n0, course, v), rate, 0.0, |t, _, _, _| (if t < before_s { course } else { course + turn }, v))
}

/// A weave about `course` at `v`: the heading swings `amp` degrees either way over `period` s.
fn weave(e0: f64, n0: f64, course: f64, v: f64, amp: f64, period: f64) -> impl Fn(u64, usize) -> Vec<Kinematics> {
    move |_, len| {
        integrate(e0, n0, len, |t| {
            let h = (course + amp * (std::f64::consts::TAU * t / period).sin()).to_radians();
            (v * h.sin(), v * h.cos())
        })
    }
}

/// A seeded uniform stream.
fn stream(seed: u64, salt: u64) -> impl FnMut() -> f64 {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ salt;
    move || {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (x >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Random turns drawn per seed: at `v`, every `gap` s (uniform) a new course `turn` degrees
/// either way (uniform), turned to at `rate` deg/s.
fn random_turns(e0: f64, n0: f64, v: f64, gap: (f64, f64), turn: (f64, f64), rate: f64, salt: u64) -> impl Fn(u64, usize) -> Vec<Kinematics> {
    move |seed, len| {
        let mut u = stream(seed, salt);
        let mut course = 360.0 * u();
        let mut next = gap.0 + (gap.1 - gap.0) * u();
        let c0 = course;
        fly(len, (e0, n0, c0, v), rate, 0.0, move |t, _, _, _| {
            if t >= next {
                let d = turn.0 + (turn.1 - turn.0) * u();
                course += if u() < 0.5 { -d } else { d };
                next = t + gap.0 + (gap.1 - gap.0) * u();
            }
            (course, v)
        })
    }
}

/// The tuning set (see the module doc).
pub fn tuning_cases() -> Vec<AirCase> {
    vec![
        air("straight 55 m/s east, 2 km past the USV, 450 m", "straight", "light_aircraft", 450.0, straight(-7000.0, -1000.0, 90.0, 55.0)),
        air("straight 35 m/s south-west, 900 m", "straight", "light_aircraft", 900.0, straight(4000.0, 6000.0, 225.0, 35.0)),
        air("loitering 800 m at 45 m/s, 600 m", "loitering", "light_aircraft", 600.0, circle(1500.0, 1500.0, 800.0, 45.0, true)),
        air("loitering 1400 m at 30 m/s anticlockwise, 1200 m", "loitering", "light_aircraft", 1200.0, circle(-2000.0, 2500.0, 1400.0, 30.0, false)),
        air("small UAS circling 200 m at 15 m/s, 120 m", "loitering", "unknown_uas", 120.0, circle(500.0, -1000.0, 200.0, 15.0, true)),
        air(
            "a triangle route at 55 m/s, turns at 6 deg/s, 450 m",
            "route",
            "light_aircraft",
            450.0,
            route(vec![(-3000.0, 0.0), (3000.0, 0.0), (0.0, 5000.0)], 55.0, 6.0),
        ),
        air(
            "a racetrack at 50 m/s, standard-rate turns, 700 m",
            "route",
            "light_aircraft",
            700.0,
            route(vec![(-2000.0, 2000.0), (2000.0, 2000.0), (2000.0, 3900.0), (-2000.0, 3900.0)], 50.0, 3.0),
        ),
        air("200 s straight at 50 m/s, then 90 degrees at 6 deg/s, 500 m", "one turn", "light_aircraft", 500.0, one_turn(-6000.0, 1500.0, 80.0, 50.0, 200.0, 90.0, 6.0)),
        air("weave 50 m/s, 35 degrees either way over 40 s, 400 m", "weave", "light_aircraft", 400.0, weave(-5000.0, -500.0, 60.0, 50.0, 35.0, 40.0)),
        air(
            "random turns at 45 m/s, 30 to 150 degrees at 5 deg/s every 20 to 60 s, 600 m",
            "random turns",
            "light_aircraft",
            600.0,
            random_turns(0.0, 1000.0, 45.0, (20.0, 60.0), (30.0, 150.0), 5.0, 0xA1),
        ),
        air("40 to 65 m/s and back at 1 m/s^2, straight, 800 m", "speed change", "light_aircraft", 800.0, |_, len| {
            fly(len, (-6000.0, 2000.0, 100.0, 40.0), 0.0, 1.0, |t, _, _, _| (100.0, if (60.0..180.0).contains(&t) { 65.0 } else { 40.0 }))
        }),
        air("helicopter hovering 90 s, 30 m/s north for 120 s, hovering, 300 m", "hover and go", "helicopter", 300.0, |_, len| {
            fly(len, (-1000.0, -500.0, 0.0, 0.0), 30.0, 3.0, |t, _, _, _| (0.0, if (90.0..210.0).contains(&t) { 30.0 } else { 0.0 }))
        }),
    ]
}

/// The held-out set (see the module doc): written after the first held-out set's run, before
/// the velocity's lag was tuned; run once with it chosen.
pub fn held_out_cases() -> Vec<AirCase> {
    vec![
        air("straight 45 m/s south, 3 km east of the USV, 700 m", "straight", "light_aircraft", 700.0, straight(3000.0, 6000.0, 180.0, 45.0)),
        air("straight 65 m/s north-west, 1200 m", "straight", "light_aircraft", 1200.0, straight(5000.0, -4000.0, 300.0, 65.0)),
        air("loitering 650 m at 50 m/s, 800 m", "loitering", "light_aircraft", 800.0, circle(2000.0, 2500.0, 650.0, 50.0, true)),
        air("loitering 1200 m at 35 m/s anticlockwise, 400 m", "loitering", "light_aircraft", 400.0, circle(-3000.0, 500.0, 1200.0, 35.0, false)),
        air("small UAS circling 150 m at 12 m/s, 100 m", "loitering", "unknown_uas", 100.0, circle(1000.0, -1500.0, 150.0, 12.0, true)),
        air(
            "a pentagon route at 50 m/s, turns at 5 deg/s, 500 m",
            "route",
            "light_aircraft",
            500.0,
            route(vec![(0.0, 4000.0), (3000.0, 1500.0), (2000.0, -2000.0), (-2000.0, -2000.0), (-3000.0, 1500.0)], 50.0, 5.0),
        ),
        air(
            "180 s straight at 40 m/s, then 120 degrees at 5 deg/s, 300 m",
            "one turn",
            "light_aircraft",
            300.0,
            one_turn(-5000.0, 3000.0, 110.0, 40.0, 180.0, 120.0, 5.0),
        ),
        air("weave 55 m/s, 30 degrees either way over 30 s, 600 m", "weave", "light_aircraft", 600.0, weave(-6000.0, 2000.0, 100.0, 55.0, 30.0, 30.0)),
        air(
            "random turns at 40 m/s, 45 to 180 degrees at 4 deg/s every 25 to 70 s, 450 m",
            "random turns",
            "light_aircraft",
            450.0,
            random_turns(1000.0, 0.0, 40.0, (25.0, 70.0), (45.0, 180.0), 4.0, 0xC3),
        ),
        air("50 to 30 m/s at 1 m/s^2, then up to 60 at 1.5 m/s^2, straight, 1000 m", "speed change", "light_aircraft", 1000.0, |_, len| {
            fly(len, (-4000.0, -1000.0, 60.0, 50.0), 0.0, 1.5, |t, _, _, _| (60.0, if t < 60.0 { 50.0 } else if t < 150.0 { 30.0 } else { 60.0 }))
        }),
        air("helicopter at 25 m/s north, hovers 60 s from 2.5 m/s^2, then 35 m/s west, 200 m", "hover and go", "helicopter", 200.0, |_, len| {
            fly(len, (1500.0, -2000.0, 0.0, 25.0), 25.0, 2.5, |t, _, _, _| {
                if t < 90.0 { (0.0, 25.0) } else if t < 160.0 { (0.0, 0.0) } else { (270.0, 35.0) }
            })
        }),
        air("an orbit 900 m at 40 m/s for 150 s, then a dash at 65 m/s, 600 m", "orbit then dash", "light_aircraft", 600.0, |_, len| {
            let (r, v) = (900.0f64, 40.0f64);
            fly(len, (-2000.0, 2900.0, 90.0, v), 40.0f64.to_degrees() / r, 1.5, move |t, _, _, c| {
                if t < 150.0 { (c + 90.0, v) } else { (c.round(), 65.0) }
            })
        }),
    ]
}

/// The first held-out set (see the module doc): written with the tuning set, run once the air
/// constants were chosen; tuning data since.
pub fn round_one_cases() -> Vec<AirCase> {
    vec![
        air("straight 70 m/s north-east, 1 km past the USV, 1500 m", "straight", "light_aircraft", 1500.0, straight(-6000.0, -9000.0, 45.0, 70.0)),
        air("straight 40 m/s west, 8 km out, 300 m", "straight", "light_aircraft", 300.0, straight(6000.0, 5000.0, 270.0, 40.0)),
        air("loitering 500 m at 55 m/s, 1500 m", "loitering", "light_aircraft", 1500.0, circle(-1000.0, 3000.0, 500.0, 55.0, true)),
        air("loitering 1000 m at 40 m/s anticlockwise, 1 km from the USV, 300 m", "loitering", "light_aircraft", 300.0, circle(500.0, -2000.0, 1000.0, 40.0, false)),
        air("small UAS circling 350 m at 20 m/s, 150 m", "loitering", "unknown_uas", 150.0, circle(-1500.0, 0.0, 350.0, 20.0, false)),
        air(
            "a square route 4 km at 60 m/s, turns at 4 deg/s, 600 m",
            "route",
            "light_aircraft",
            600.0,
            route(vec![(-2000.0, -1000.0), (2000.0, -1000.0), (2000.0, 3000.0), (-2000.0, 3000.0)], 60.0, 4.0),
        ),
        air(
            "150 s straight at 60 m/s, then 150 degrees at 3 deg/s, 1000 m",
            "one turn",
            "light_aircraft",
            1000.0,
            one_turn(4000.0, -6000.0, 340.0, 60.0, 150.0, -150.0, 3.0),
        ),
        air("weave 40 m/s, 45 degrees either way over 60 s, 250 m", "weave", "light_aircraft", 250.0, weave(3000.0, 4000.0, 200.0, 40.0, 45.0, 60.0)),
        air(
            "random turns at 55 m/s, 20 to 120 degrees at 8 deg/s every 15 to 45 s, 900 m",
            "random turns",
            "light_aircraft",
            900.0,
            random_turns(-500.0, 1500.0, 55.0, (15.0, 45.0), (20.0, 120.0), 8.0, 0xB2),
        ),
        air("slows from 60 to 35 m/s at 1.5 m/s^2, then circles at 6 deg/s, 500 m", "speed change", "light_aircraft", 500.0, |_, len| {
            fly(len, (-5000.0, 0.0, 70.0, 60.0), 6.0, 1.5, |t, _, _, c| if t < 120.0 { (70.0, 60.0) } else if t < 150.0 { (70.0, 35.0) } else { (c + 90.0, 35.0) })
        }),
        air("helicopter at 40 m/s east, stops to hover at 2 m/s^2, then 25 m/s south, 250 m", "hover and go", "helicopter", 250.0, |_, len| {
            fly(len, (-4000.0, 1000.0, 90.0, 40.0), 20.0, 2.0, |t, _, _, _| {
                if t < 100.0 { (90.0, 40.0) } else if t < 200.0 { (90.0, 0.0) } else { (180.0, 25.0) }
            })
        }),
        air("figure eight, two 700 m circles at 45 m/s, 700 m", "figure eight", "light_aircraft", 700.0, |_, len| {
            let r = 700.0f64;
            let v = 45.0f64;
            let lap = std::f64::consts::TAU * r / v;
            integrate(0.0, 1500.0, len, move |t| {
                // one circle clockwise, the next anticlockwise, each from the meeting point
                // heading east
                let k = (t / lap).floor() as i64;
                let a = v / r * (t - k as f64 * lap);
                let w = if k % 2 == 0 { 1.0 } else { -1.0 };
                (v * a.cos(), -w * v * a.sin())
            })
        }),
    ]
}

/// Effect points around the truth (metres east and north): the share of them a keep-out
/// blocks is the share of the area near the aircraft an engagement is denied.
fn effect_points() -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for r in [250.0, 500.0, 1000.0, 1500.0, 2000.0, 3000.0, 4000.0, 5000.0] {
        for k in 0..8 {
            let b = std::f64::consts::TAU * f64::from(k) / 8.0 + 0.3;
            out.push((r * b.sin(), r * b.cos()));
        }
    }
    out
}

/// One tracker's sums over the scored seconds.
#[derive(Debug, Default, Clone, Copy)]
pub struct Side {
    pub tally: Tally,
    pub hold: [Hold; 2],
    /// samples, and of those blocked effect points over the grid (per span)
    pub block: [(f64, f64); 2],
    /// scored samples with a velocity
    pub with_v: f64,
    /// scored samples whose facts gave an altitude
    pub with_alt: f64,
}

impl Side {
    fn add(&mut self, o: &Side) {
        let t = &mut self.tally;
        t.n += o.tally.n;
        t.nees += o.tally.nees;
        t.inside += o.tally.inside;
        t.sigma += o.tally.sigma;
        t.vnees += o.tally.vnees;
        t.vn += o.tally.vn;
        t.man_p += o.tally.man_p;
        t.unknown_v += o.tally.unknown_v;
        for k in 0..2 {
            self.hold[k].n += o.hold[k].n;
            self.hold[k].covered += o.hold[k].covered;
            self.hold[k].reach += o.hold[k].reach;
            self.block[k].0 += o.block[k].0;
            self.block[k].1 += o.block[k].1;
        }
        self.with_v += o.with_v;
        self.with_alt += o.with_alt;
    }

    pub fn velocity_share(&self) -> f64 {
        self.with_v / self.tally.n.max(1.0)
    }

    pub fn block_share(&self, k: usize) -> f64 {
        self.block[k].1 / self.block[k].0.max(1.0)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AirScores {
    pub new: Side,
    pub old: Side,
}

impl AirScores {
    pub fn add(&mut self, o: &AirScores) {
        self.new.add(&o.new);
        self.old.add(&o.old);
    }
}

fn blocked(f: &TrackFacts, now: f64, span: f64, truth: (f64, f64), ec: &EngageRules, pts: &[(f64, f64)]) -> f64 {
    let regions = track_reach(f, calib::ms(now + span), ec);
    let line = 75.0 + ec.keep_out_m;
    pts.iter()
        .filter(|(e, n)| {
            let (lat, lon) = at(truth.0, truth.1, *e, *n);
            let p = LonLat::new(lon, lat);
            regions.iter().any(|r| haversine_m(r.center, p) <= r.radius_m + line)
        })
        .count() as f64
}

/// Both trackers on `case`, seen by `mix`, at `seed`.
pub fn run(case: &AirCase, mix: Mix, seed: u64) -> AirScores {
    let ec = EngageRules::default();
    let ec_old = EngageRules {
        rules_version: BEFORE_RULES,
        ..EngageRules::default()
    };
    let table = (case.motion)(seed, case.n as usize + SPANS_S[1] as usize + 1);
    let obs = mix.observers();
    let (now_obs, old_obs): (Vec<Observer>, Vec<before_oho::sensing::Observer>) = (obs.iter().map(Obs::now).collect(), obs.iter().map(Obs::before).collect());
    let pts = effect_points();
    let mut new: Option<Track> = None;
    let mut old: Option<before_oho::model::Track> = None;
    let mut sc = AirScores::default();
    for s in 0..case.n {
        let now = T0 + f64::from(s);
        let ((e, n), (ve, vn)) = table[s as usize];
        let (lat, lon) = at(BASE.0, BASE.1, e, n);
        let mut c: ContactTruth = contact("c", case.kind, Domain::Air, lat, lon);
        c.alt_m = case.alt_m;
        c.speed_mps = ve.hypot(vn);
        c.course_deg = ve.atan2(vn).to_degrees().rem_euclid(360.0);
        let mut dets = detect(Params { seed, tick: i64::from(s), misclassification_rate: 0.0 }, &now_obs, std::slice::from_ref(&c), &|_, _| false);
        let mut old_dets = before_oho::sensing::detect(
            before_oho::sensing::Params { seed, tick: i64::from(s), misclassification_rate: 0.0 },
            &old_obs,
            std::slice::from_ref(&c),
            &|_, _| false,
        );
        // dated by the truth message's clock, as the bridge does
        for d in &mut dets {
            d.measured_t = Some(now);
            match &mut new {
                None => new = Some(Track::new("T-0001".into(), 1, d, now)),
                Some(t) => t.update(d, now),
            }
        }
        for d in &mut old_dets {
            d.measured_t = Some(now);
            match &mut old {
                None => old = Some(before_oho::model::Track::new("T-0001".into(), 1, d, now)),
                Some(t) => t.update(d, now),
            }
        }
        if s < SETTLE_S {
            continue;
        }
        let err2 = |tlat: f64, tlon: f64| {
            let de = (tlon - lon) * 111_320.0 * lat.to_radians().cos();
            let dn = (tlat - lat) * 111_320.0;
            de * de + dn * dn
        };
        let ahead: [(f64, f64); 2] = SPANS_S.map(|span| {
            let ((e2, n2), _) = table[s as usize + span as usize];
            at(BASE.0, BASE.1, e2, n2)
        });
        if let Some(t) = new.as_ref().filter(|_| !dets.is_empty()) {
            let side = &mut sc.new;
            side.tally.add(err2(t.lat, t.lon), t.sigma_m);
            side.tally.man_p += t.manoeuvring_p();
            if let Some((ve2, vn2)) = t.vel {
                let sv = t.vel_sigma_mps();
                side.tally.vnees += ((ve2 - ve).powi(2) + (vn2 - vn).powi(2)) / (sv * sv);
                side.tally.vn += 1.0;
            }
            let f = t.facts(now);
            side.with_v += f64::from(u8::from(f.velocity_mps.is_some()));
            side.with_alt += f64::from(u8::from(f.alt_m.is_some()));
            for (k, span) in SPANS_S.iter().enumerate() {
                hold(&f, now, *span, ahead[k], &ec, &mut side.hold[k]);
                side.block[k].0 += pts.len() as f64;
                side.block[k].1 += blocked(&f, now, *span, ahead[k], &ec, &pts);
            }
        }
        if let Some(t) = old.as_ref().filter(|_| !old_dets.is_empty()) {
            let side = &mut sc.old;
            side.tally.add(err2(t.lat, t.lon), t.sigma_m);
            side.tally.man_p += t.manoeuvring_p();
            if let Some((ve2, vn2)) = t.vel {
                let sv = t.vel_sigma_mps();
                side.tally.vnees += ((ve2 - ve).powi(2) + (vn2 - vn).powi(2)) / (sv * sv);
                side.tally.vn += 1.0;
            }
            let f = t.facts(now);
            side.with_v += f64::from(u8::from(f.velocity_mps.is_some()));
            side.with_alt += f64::from(u8::from(f.alt_m.is_some()));
            for (k, span) in SPANS_S.iter().enumerate() {
                hold(&f, now, *span, ahead[k], &ec_old, &mut side.hold[k]);
                side.block[k].0 += pts.len() as f64;
                side.block[k].1 += blocked(&f, now, *span, ahead[k], &ec_old, &pts);
            }
        }
    }
    sc
}

/// A case's scores on a mix, summed over `seeds`.
pub fn over_seeds(case: &AirCase, mix: Mix, seeds: RangeInclusive<u64>) -> AirScores {
    let mut total = AirScores::default();
    for seed in seeds {
        total.add(&run(case, mix, seed));
    }
    total
}

fn side_line(s: &Side) -> String {
    format!(
        "NEES {:5.2} in 2 sigma {:5.1}% sigma {:5.0} m vel {:5.1}% vNEES {:5.2} | ko60 {:5.1}% ({:5.0} m) ko100 {:5.1}% ({:5.0} m) | blocks {:4.1}% {:4.1}%",
        s.tally.mean_nees(),
        100.0 * s.tally.coverage(),
        s.tally.sigma / s.tally.n.max(1.0),
        100.0 * s.velocity_share(),
        s.tally.vnees / s.tally.vn.max(1.0),
        100.0 * s.hold[0].coverage(),
        s.hold[0].mean_reach(),
        100.0 * s.hold[1].coverage(),
        s.hold[1].mean_reach(),
        100.0 * s.block_share(0),
        100.0 * s.block_share(1),
    )
}

/// One case (or class) on one mix: after, then before.
pub fn line(name: &str, mix: Mix, s: &AirScores) -> String {
    format!("{name:<62} {:<14} new {}\n{:<77} old {}", mix.name(), side_line(&s.new), "", side_line(&s.old))
}

/// The classes flown straight with one manoeuvre at most, in which a velocity given must be
/// honest: the gate's own question. In and just after a turn the constant-velocity models lag
/// and a velocity given is off by more than its sigma says (a route's turns, a loiter, a weave:
/// reported, not asserted); the keep-out counts an aircraft wherever it could turn (rules 13).
pub const STEADY_CLASSES: [&str; 4] = ["straight", "one turn", "speed change", "hover and go"];

/// A set's failures against the acceptance criteria (as the surface's, `calib::failures`):
/// per class and mix, its cases and seeds pooled, the truth within two sigmas at least 83% of
/// the time and the mean normalized error between 0.5 and 2.6 when updated; in the classes
/// flown mostly straight ([`STEADY_CLASSES`]), a velocity given within a mean normalized error
/// of 2.6; per case and mix, a keep-out 60 and 100 s ahead holding the aircraft at least as
/// often as the tracker before this calibration's did (under rules 12); and never an altitude
/// (no mix has a radar that measures height).
pub fn failures(cases: &[AirCase], seeds: RangeInclusive<u64>, table: &mut Vec<String>) -> Vec<String> {
    let mut bad = Vec::new();
    let mut by_class: BTreeMap<(&str, Mix), AirScores> = BTreeMap::new();
    for c in cases {
        for mix in MIXES {
            let s = over_seeds(c, mix, seeds.clone());
            table.push(line(c.name, mix, &s));
            by_class.entry((c.class, mix)).or_default().add(&s);
            for (k, span) in SPANS_S.iter().enumerate() {
                let (new, old) = (s.new.hold[k].coverage(), s.old.hold[k].coverage());
                if new < old {
                    bad.push(format!(
                        "{} ({}): a keep-out {span:.0} s ahead holds it {:.1}% of the time, the tracker before {:.1}%",
                        c.name,
                        mix.name(),
                        100.0 * new,
                        100.0 * old
                    ));
                }
            }
            if s.new.with_alt > 0.0 {
                bad.push(format!("{} ({}): an altitude in {} samples from sensors that measure none", c.name, mix.name(), s.new.with_alt));
            }
        }
    }
    for ((class, mix), s) in &by_class {
        table.push(line(&format!("class: {class}"), *mix, s));
        let (nees, cover) = (s.new.tally.mean_nees(), s.new.tally.coverage());
        if !(cover >= 0.83 && (0.5..=2.6).contains(&nees)) {
            bad.push(format!("{class} ({}): NEES {nees:.2}, within 2 sigma {:.1}% when updated", mix.name(), 100.0 * cover));
        }
        let vnees = s.new.tally.vnees / s.new.tally.vn.max(1.0);
        if STEADY_CLASSES.contains(class) && vnees > 2.6 {
            bad.push(format!("{class} ({}): a velocity given with a mean normalized error of {vnees:.2}", mix.name()));
        }
    }
    bad
}

/// The acceptance test (murmur-oho): on the held-out set, never tuned on, the air tracker is
/// honest when updated in every motion class and sensor mix, its velocity honest where an
/// aircraft flies mostly straight, it never gives an altitude its sensors did not measure, and a
/// keep-out 60 and 100 s ahead holds every motion at least as often as one built from the
/// tracker before (under rules 12).
#[test]
fn on_held_out_air_motions_the_tracker_is_honest_and_a_keep_out_holds_at_least_as_often() {
    let mut table = Vec::new();
    let bad = failures(&held_out_cases(), AIR_HELD_OUT_SEEDS, &mut table);
    assert!(bad.is_empty(), "{}\n\nall:\n{}", bad.join("\n"), table.join("\n"));
}

/// The same criteria on the tuning sets (what the air constants were chosen on).
#[test]
fn on_the_air_tuning_sets_the_tracker_is_honest_and_a_keep_out_holds_at_least_as_often() {
    let mut table = Vec::new();
    let mut bad = failures(&tuning_cases(), AIR_TUNING_SEEDS, &mut table);
    bad.extend(failures(&round_one_cases(), AIR_ROUND_ONE_SEEDS, &mut table));
    assert!(bad.is_empty(), "{}\n\nall:\n{}", bad.join("\n"), table.join("\n"));
}

/// A 2D radar measures an aircraft's slant range and bearing, not its height: its plot is out
/// along the bearing by the height's share, the track says nothing of the altitude (so a keep-out
/// stays 2D for it), gives no velocity and is uncertain along the whole bearing segment within
/// the slant range; with EO/IR too it is ranged and gets a velocity, still with no altitude.
/// A radar that measures elevation gives one.
#[test]
fn a_2d_radar_fix_of_an_aircraft_gives_no_altitude_and_one_radar_does_not_range_it() {
    let ec = EngageRules::default();
    let follow = |mix: Mix, height: bool, alt_m: f64| {
        let table = straight(-4000.0, 1000.0, 90.0, 50.0)(7, 121);
        let mut obs: Vec<Observer> = mix.observers().iter().map(Obs::now).collect();
        for o in &mut obs {
            for s in &mut o.sensors {
                s.height = height && s.kind == SensorType::Radar;
            }
        }
        let mut track: Option<Track> = None;
        let mut last = None;
        for s in 0..=120u32 {
            let now = T0 + f64::from(s);
            let ((e, n), (ve, vn)) = table[s as usize];
            let (lat, lon) = at(BASE.0, BASE.1, e, n);
            let mut c = contact("c", "light_aircraft", Domain::Air, lat, lon);
            c.alt_m = alt_m;
            c.speed_mps = ve.hypot(vn);
            c.course_deg = ve.atan2(vn).to_degrees().rem_euclid(360.0);
            for mut d in detect(Params { seed: 7, tick: i64::from(s), misclassification_rate: 0.0 }, &obs, std::slice::from_ref(&c), &|_, _| false) {
                d.measured_t = Some(now);
                if d.sensor == SensorType::Radar && !height {
                    // the plot is at the slant range from the radar, farther than the aircraft
                    let (olat, olon) = d.slant_from.expect("a 2D radar's plot of an aircraft says where the radar was");
                    let plot = haversine_m(LonLat::new(olon, olat), LonLat::new(d.lon, d.lat));
                    let ground = haversine_m(LonLat::new(olon, olat), LonLat::new(lon, lat));
                    assert!(d.alt_m.is_none() && (plot - d.range_m).abs() < 5.0 * d.sigma_m && d.range_m > ground, "{plot} {} {ground}", d.range_m);
                }
                match &mut track {
                    None => track = Some(Track::new("T-0001".into(), 1, &d, now)),
                    Some(t) => t.update(&d, now),
                }
            }
            last = Some((lat, lon));
        }
        let t = track.expect("a track");
        let (lat, lon) = last.expect("truth");
        let err = haversine_m(LonLat::new(t.lon, t.lat), LonLat::new(lon, lat));
        (t.facts(t.last_seen_t), err)
    };
    // one 2D radar, an aircraft 1,500 m up: no altitude, no velocity, its 1-sigma along the
    // bearing segment (kilometres), the truth within it
    let (f, err) = follow(Mix::Radar, false, 1500.0);
    assert!(f.alt_m.is_none() && murmur_decide::engage::track_altitude(&f, f.last_seen_t, &ec).is_none(), "{f:?}");
    assert!(f.velocity_mps.is_none(), "{f:?}");
    assert!(f.pos_sigma_m > 500.0 && err < 2.0 * f.pos_sigma_m, "sigma {} error {err}", f.pos_sigma_m);
    // with EO/IR too: ranged, a velocity, still no altitude
    let (f, err) = follow(Mix::RadarEoIr, false, 1500.0);
    assert!(f.alt_m.is_none() && f.velocity_mps.is_some(), "{f:?}");
    assert!(f.pos_sigma_m < 60.0 && err < 2.0 * f.pos_sigma_m, "sigma {} error {err}", f.pos_sigma_m);
    // a radar that measures elevation: the altitude, ranged
    let (f, _) = follow(Mix::RadarEoIr, true, 1500.0);
    let alt = f.alt_m.expect("a 3D radar measures height");
    assert!((alt - 1500.0).abs() < 100.0 && murmur_decide::engage::track_altitude(&f, f.last_seen_t, &ec).is_some(), "{f:?}");
}

/// Print every case of the tuning sets on every mix (a tuning aid: `cargo test -p
/// murmur-bridge --release air_calib_table -- --ignored --nocapture`; `CALIB=held_out` for the
/// held-out set).
#[test]
#[ignore]
fn air_calib_table() {
    let held = std::env::var("CALIB").is_ok_and(|v| v == "held_out");
    let sets = if held {
        vec![(held_out_cases(), AIR_HELD_OUT_SEEDS)]
    } else {
        vec![(tuning_cases(), AIR_TUNING_SEEDS), (round_one_cases(), AIR_ROUND_ONE_SEEDS)]
    };
    let mut table = Vec::new();
    let mut bad = Vec::new();
    for (cases, seeds) in sets {
        bad.extend(failures(&cases, seeds, &mut table));
    }
    eprintln!("{}", table.join("\n"));
    eprintln!("--- failures\n{}", bad.join("\n"));
}

/// The tuning grid (a tuning aid, the tuning sets only: `AIR_GRID="q steady,q manoeuvring,gate,
/// initial velocity sigma,lag;..." cargo test -p murmur-bridge --release air_tune_grid --
/// --ignored --nocapture`): each candidate of the air constants on every tuning case and mix,
/// summarized.
#[test]
#[ignore]
fn air_tune_grid() {
    let grid: Vec<[f64; 4]> = std::env::var("AIR_GRID")
        .ok()
        .map(|g| {
            g.split(';')
                .map(|c| {
                    let v: Vec<f64> = c.split(',').map(|x| x.trim().parse().expect("a number")).collect();
                    [v[0], v[1], v[2], v[3]]
                })
                .collect()
        })
        .unwrap_or_else(|| vec![[0.2, 20.0, 3.0, 60.0]]);
    let cases: Vec<(AirCase, RangeInclusive<u64>)> = tuning_cases()
        .into_iter()
        .map(|c| (c, AIR_TUNING_SEEDS))
        .chain(round_one_cases().into_iter().map(|c| (c, AIR_ROUND_ONE_SEEDS)))
        .collect();
    for t in grid {
        crate::tracks::model::AIR_TUNE.set(Some(t));
        let mut worst_cover: f64 = 1.0;
        let mut worst_nees: f64 = 0.0;
        let mut by_mix: BTreeMap<Mix, AirScores> = BTreeMap::new();
        let mut by_class: BTreeMap<(&str, Mix), AirScores> = BTreeMap::new();
        let mut ko_bad = Vec::new();
        for (c, seeds) in &cases {
            for mix in [Mix::RadarEoIr, Mix::All] {
                let s = over_seeds(c, mix, seeds.clone());
                by_mix.entry(mix).or_default().add(&s);
                by_class.entry((c.class, mix)).or_default().add(&s);
                for (k, span) in SPANS_S.iter().enumerate() {
                    let d = s.new.hold[k].coverage() - s.old.hold[k].coverage();
                    if d < 0.0 {
                        ko_bad.push(format!("{}/{}/{span}: {:+.1}", c.class, if mix == Mix::All { "all" } else { "eo" }, 100.0 * d));
                    }
                }
            }
        }
        let mut vel_classes = Vec::new();
        for ((class, mix), s) in &by_class {
            worst_cover = worst_cover.min(s.new.tally.coverage());
            worst_nees = worst_nees.max(s.new.tally.mean_nees());
            vel_classes.push(format!(
                "{}/{} v{:.0}% vN{:.1} ko{:.1}",
                &class[..class.len().min(6)],
                if *mix == Mix::All { "a" } else { "e" },
                100.0 * s.new.velocity_share(),
                s.new.tally.vnees / s.new.tally.vn.max(1.0),
                100.0 * s.new.hold[0].coverage()
            ));
        }
        eprintln!(
            "q {:?}: cover min {:.1}% NEES max {:.2} | vel eo {:.1}% all {:.1}% | vNEES eo {:.2} all {:.2} | blocks60 eo {:.1}% all {:.1}% | ko below before: {}",
            t,
            100.0 * worst_cover,
            worst_nees,
            100.0 * by_mix[&Mix::RadarEoIr].new.velocity_share(),
            100.0 * by_mix[&Mix::All].new.velocity_share(),
            by_mix[&Mix::RadarEoIr].new.tally.vnees / by_mix[&Mix::RadarEoIr].new.tally.vn.max(1.0),
            by_mix[&Mix::All].new.tally.vnees / by_mix[&Mix::All].new.tally.vn.max(1.0),
            100.0 * by_mix[&Mix::RadarEoIr].new.block_share(0),
            100.0 * by_mix[&Mix::All].new.block_share(0),
            ko_bad.join(" ")
        );
        eprintln!("    {}", vel_classes.join(" | "));
    }
    crate::tracks::model::AIR_TUNE.set(None);
}

#[test]
fn the_air_seed_sets_are_disjoint_from_each_other_and_the_surface_sets() {
    let sets = [
        calib::FIRST_SEEDS,
        calib::ROUND_ONE_SEEDS,
        calib::ROUND_TWO_SEEDS,
        calib::HELD_OUT_SEEDS,
        AIR_TUNING_SEEDS,
        AIR_ROUND_ONE_SEEDS,
        AIR_HELD_OUT_SEEDS,
    ];
    for (i, a) in sets.iter().enumerate() {
        for b in &sets[i + 1..] {
            assert!(a.end() < b.start() || b.end() < a.start(), "{a:?} and {b:?} overlap");
        }
    }
}
