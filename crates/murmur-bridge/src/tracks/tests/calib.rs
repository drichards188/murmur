//! The tracker against the simulation's truth (rules 9's review): is its 1-sigma honest when
//! it is updated, and does a keep-out built from it a minute or more ahead still hold the
//! contact, compared with the tracker before the review ([`OldTrack`], a replica of its
//! arithmetic)?
//!
//! Four sets, disjoint in seeds, each run on both filters with the same detections.
//!
//! 1. [`first_cases`] on [`FIRST_SEEDS`]: what the single-model filter was tuned on.
//! 2. [`round_one_cases`] on [`ROUND_ONE_SEEDS`]: the first held-out set, the review's classes
//!    and motions the constants had never seen (a zigzag, a sprint and stop, an acceleration,
//!    a weave, a sharper turn, a tighter circle, random legs drawn per seed). The first
//!    two-model constants were overconfident on it when updated (zigzag, sprint and stop,
//!    weave, random legs), so it became tuning data.
//! 3. [`round_two_cases`] on [`ROUND_TWO_SEEDS`]: the second held-out set, written before the
//!    constants were tuned again: the same classes in other variants and random turns at a
//!    physical turn rate. The tuned filter was honest on every class of it when updated, but
//!    a keep-out a minute ahead held a 60 degree zigzag less often than the old filter's (the
//!    new filter's velocity is the current leg's, the old one's an average of the last minute
//!    or so), and a weave about as often. The keep-out gained the average velocity's
//!    prediction (`TrackFacts::mean_velocity_mps`); this set became tuning data.
//! 4. [`held_out_cases`] on [`HELD_OUT_SEEDS`]: written before that change was run on
//!    anything, run once with it: every class in yet other variants, a patrol box, on seeds
//!    used nowhere else. The test asserts on it.

use super::{BASE, T0, at, contact, observer};
use crate::tracks::model::Track;
use crate::tracks::sensing::{Detection, Observer, Params, SensorType, detect};
use crate::tracks::truth::{Ais, Domain};
use murmur_decide::engage::track_reach;
use murmur_decide::geo::haversine_m;
use murmur_decide::{Affiliation, EngageRules, Fact, LonLat, Time, TrackFacts};

/// The first tuning set's seeds.
pub const FIRST_SEEDS: std::ops::RangeInclusive<u64> = 1..=5;
/// The first held-out set's seeds, tuning data since (6-25 are the ones the review ran).
pub const ROUND_ONE_SEEDS: std::ops::RangeInclusive<u64> = 6..=35;
/// The second held-out set's seeds, tuning data since.
pub const ROUND_TWO_SEEDS: std::ops::RangeInclusive<u64> = 36..=65;
/// The held-out set's seeds: used nowhere in tuning.
pub const HELD_OUT_SEEDS: std::ops::RangeInclusive<u64> = 66..=95;
/// Seconds before anything is scored (a new track settles).
pub(super) const SETTLE_S: u32 = 30;
/// How far ahead a keep-out is checked, s.
pub const SPANS_S: [f64; 2] = [60.0, 100.0];

/// Position (east, north m from BASE) and velocity (east, north m/s) per second.
pub(super) type Kinematics = ((f64, f64), (f64, f64));

pub struct Case {
    pub name: &'static str,
    pub class: &'static str,
    kind: &'static str,
    domain: Domain,
    ais: bool,
    observers: Vec<Observer>,
    pub n: u32,
    motion: Motion,
}

enum Motion {
    /// Kinematics at second s.
    At(Box<dyn Fn(f64) -> Kinematics>),
    /// Kinematics per second for a seed (motions drawn per seed).
    Drawn(Box<dyn Fn(u64, usize) -> Vec<Kinematics>>),
}

impl Case {
    fn table(&self, seed: u64, len: usize) -> Vec<Kinematics> {
        match &self.motion {
            Motion::At(f) => (0..len).map(|s| f(s as f64)).collect(),
            Motion::Drawn(f) => f(seed, len),
        }
    }
}

fn case(
    name: &'static str,
    class: &'static str,
    kind: &'static str,
    ais: bool,
    observers: Vec<Observer>,
    n: u32,
    f: impl Fn(f64) -> Kinematics + 'static,
) -> Case {
    Case {
        name,
        class,
        kind,
        domain: Domain::Surface,
        ais,
        observers,
        n,
        motion: Motion::At(Box::new(f)),
    }
}

struct Observers {
    near: Observer,
    far: Observer,
    near2: Observer,
    blade: Observer,
    quad: Observer,
    ais_rx: Observer,
}

fn observers() -> Observers {
    Observers {
        near: observer(1, BASE.0, BASE.1 - 0.027, 0.0, &[(SensorType::Radar, 8_000.0), (SensorType::EoIr, 5_000.0)]),
        far: observer(2, BASE.0 + 0.05, BASE.1, 0.0, &[(SensorType::Radar, 12_000.0)]),
        near2: observer(4, BASE.0 + 0.025, BASE.1 + 0.002, 0.0, &[(SensorType::Radar, 20_000.0), (SensorType::EoIr, 8_000.0)]),
        blade: observer(5, BASE.0 - 0.01, BASE.1 + 0.03, 80.0, &[(SensorType::EoIr, 3_000.0)]),
        quad: observer(6, BASE.0 - 0.005, BASE.1 + 0.012, 60.0, &[(SensorType::EoIr, 1_500.0), (SensorType::Radar, 5_000.0)]),
        ais_rx: observer(3, BASE.0, BASE.1 + 0.03, 0.0, &[(SensorType::Ais, 30_000.0), (SensorType::Radar, 8_000.0)]),
    }
}

fn holding(_: f64) -> Kinematics {
    ((0.0, 0.0), (0.0, 0.0))
}

fn transit(x0: f64, y: f64, v: f64) -> impl Fn(f64) -> Kinematics {
    move |s| ((x0 + v * s, y), (v, 0.0))
}

fn stop_after_transit(s: f64) -> Kinematics {
    if s < 120.0 { ((-1440.0 + 12.0 * s, 0.0), (12.0, 0.0)) } else { ((0.0, 0.0), (0.0, 0.0)) }
}

/// East at `v` for 150 s, a turn to north at `rate` deg/s, then north.
fn turn(v: f64, rate_dps: f64) -> impl Fn(f64) -> Kinematics {
    move |s| {
        let w = rate_dps.to_radians();
        let rt = v / w;
        let t_turn = std::f64::consts::FRAC_PI_2 / w;
        if s < 150.0 {
            ((-1200.0 + v * s, 0.0), (v, 0.0))
        } else if s < 150.0 + t_turn {
            let a = (s - 150.0) * w;
            ((-1200.0 + v * 150.0 + rt * a.sin(), rt * (1.0 - a.cos())), (v * a.cos(), v * a.sin()))
        } else {
            ((-1200.0 + v * 150.0 + rt, rt + v * (s - 150.0 - t_turn)), (0.0, v))
        }
    }
}

/// Clockwise around BASE at radius `r`, speed `v`.
fn circling(r: f64, v: f64) -> impl Fn(f64) -> Kinematics {
    move |s| {
        let a = v / r * s;
        ((r * a.sin(), r * a.cos()), (v * a.cos(), -v * a.sin()))
    }
}

/// The review's zigzag: 8 m/s, a 90 degree course change every 40 s.
fn zigzag(s: f64) -> Kinematics {
    let leg = (s / 40.0).floor();
    let r = s - leg * 40.0;
    let half = (leg / 2.0).floor();
    let (e, n) = (-1000.0 + half * 320.0, half * 320.0);
    if (leg as i64) % 2 == 0 { ((e + 8.0 * r, n), (8.0, 0.0)) } else { ((e + 320.0, n + 8.0 * r), (0.0, 8.0)) }
}

/// The review's sprint and stop: 12 m/s for 25 s, still for 25 s.
fn sprint_stop(s: f64) -> Kinematics {
    let c = (s / 50.0).floor();
    let r = s - c * 50.0;
    let base = -1500.0 + c * 300.0;
    if r < 25.0 { ((base + 12.0 * r, 0.0), (12.0, 0.0)) } else { ((base + 300.0, 0.0), (0.0, 0.0)) }
}

/// The review's acceleration: from still to 15 m/s over 60 s, then steady.
fn accelerating(s: f64) -> Kinematics {
    if s < 60.0 { ((0.125 * s * s, 0.0), (0.25 * s, 0.0)) } else { ((450.0 + 15.0 * (s - 60.0), 0.0), (15.0, 0.0)) }
}

/// Integrate a velocity law v(t) from rest at (x0, y0), 0.1 s steps, sampled each second.
pub(super) fn integrate(x0: f64, y0: f64, len: usize, v: impl Fn(f64) -> (f64, f64)) -> Vec<Kinematics> {
    let mut out = Vec::with_capacity(len);
    let (mut e, mut n) = (x0, y0);
    for s in 0..len {
        let t = s as f64;
        out.push(((e, n), v(t)));
        for k in 0..10 {
            let (ve, vn) = v(t + (k as f64 + 0.5) / 10.0);
            e += ve / 10.0;
            n += vn / 10.0;
        }
    }
    out
}

/// A weave: 7 m/s, course 45 degrees swinging 40 degrees either way over a 60 s period.
fn weave(_: u64, len: usize) -> Vec<Kinematics> {
    integrate(-900.0, -900.0, len, |t| {
        let h = (45.0 + 40.0 * (std::f64::consts::TAU * t / 60.0).sin()).to_radians();
        (7.0 * h.sin(), 7.0 * h.cos())
    })
}

/// Random legs, drawn per seed: each 20 to 80 s, at 0 to 12 m/s on any course, changing at
/// once (as the simulation's contacts stop at a waypoint).
fn random_legs(seed: u64, len: usize) -> Vec<Kinematics> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(0x5851_F42D_4C95_7F2D);
    let mut u = move || {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (x >> 11) as f64 / (1u64 << 53) as f64
    };
    let mut legs = Vec::new();
    let mut t = 0.0;
    while t < len as f64 + 1.0 {
        let d = 20.0 + 60.0 * u();
        let v = 12.0 * u();
        let c = std::f64::consts::TAU * u();
        legs.push((t, t + d, v * c.sin(), v * c.cos()));
        t += d;
    }
    integrate(0.0, 0.0, len, |t| {
        legs.iter()
            .find(|l| t >= l.0 && t < l.1)
            .map_or((0.0, 0.0), |l| (l.2, l.3))
    })
}

/// The motions the single-model filter was tuned on (tuning data for the two-model one too).
pub fn first_cases() -> Vec<Case> {
    let o = observers();
    vec![
        case("holding, one observer", "holding", "small_fast_boat", false, vec![o.near.clone()], 300, holding),
        case("holding, two observers", "holding", "small_fast_boat", false, vec![o.near.clone(), o.far.clone()], 300, holding),
        // the live run's geometry: two ships 2.4 and 2.8 km off, each with radar and EO/IR
        case("holding, four sensors (live)", "holding", "small_fast_boat", false, vec![o.near.clone(), o.near2.clone()], 300, holding),
        case("transit 12 m/s, four sensors", "transit", "small_fast_boat", false, vec![o.near.clone(), o.near2.clone()], 300, transit(-1500.0, 0.0, 12.0)),
        case(
            "stops after a transit, then holds 15 min, seven sensors",
            "stop after transit",
            "small_fast_boat",
            false,
            vec![o.near.clone(), o.near2.clone(), o.blade.clone(), o.quad.clone()],
            1020,
            stop_after_transit,
        ),
        case("stops after a transit, four sensors", "stop after transit", "small_fast_boat", false, vec![o.near.clone(), o.near2.clone()], 300, stop_after_transit),
        case("transit 10 m/s", "transit", "fishing_vessel", false, vec![o.near.clone(), o.far.clone()], 300, transit(-1500.0, 0.0, 10.0)),
        case("a 90 degree turn at 3 deg/s", "90 degree turn", "fishing_vessel", false, vec![o.near.clone(), o.far.clone()], 300, turn(8.0, 3.0)),
        case("circling 400 m at 6 m/s", "circling", "ferry", false, vec![o.near.clone()], 300, circling(400.0, 6.0)),
        case("ferry with AIS, 10 m/s", "transit", "ferry", true, vec![o.ais_rx.clone()], 300, transit(-1500.0, 300.0, 10.0)),
        // the live run's coast guard cutter: AIS, circling 1.2 km at 6 m/s
        case("patrol boat with AIS circling 1.2 km at 6 m/s", "circling", "ferry", true, vec![o.ais_rx.clone()], 300, circling(1200.0, 6.0)),
        case("radar only, 6 km", "transit", "cargo_ship", false, vec![o.far.clone()], 300, |s| ((0.0, -500.0 + 4.0 * s), (0.0, 4.0))),
    ]
}

/// The first held-out set (see the module doc): the review's classes on seeds the first
/// constants never saw, and motions they never saw. Tuning data since it failed them.
pub fn round_one_cases() -> Vec<Case> {
    let o = observers();
    let two = || vec![o.near.clone(), o.near2.clone()];
    vec![
        case("holding, one observer", "holding", "small_fast_boat", false, vec![o.near.clone()], 300, holding),
        case("holding, four sensors", "holding", "small_fast_boat", false, two(), 300, holding),
        case("transit 12 m/s, four sensors", "transit", "small_fast_boat", false, two(), 300, transit(-1500.0, 0.0, 12.0)),
        case("ferry with AIS, 10 m/s", "transit", "ferry", true, vec![o.ais_rx.clone()], 300, transit(-1500.0, 300.0, 10.0)),
        case("radar only, 6 km", "transit", "cargo_ship", false, vec![o.far.clone()], 300, |s| ((0.0, -500.0 + 4.0 * s), (0.0, 4.0))),
        case("stops after a transit, four sensors", "stop after transit", "small_fast_boat", false, two(), 300, stop_after_transit),
        case("a 90 degree turn at 3 deg/s", "90 degree turn", "fishing_vessel", false, vec![o.near.clone(), o.far.clone()], 300, turn(8.0, 3.0)),
        case("a 90 degree turn at 10 deg/s, 10 m/s (unseen)", "90 degree turn", "small_fast_boat", false, two(), 300, turn(10.0, 10.0)),
        case("circling 400 m at 6 m/s", "circling", "ferry", false, vec![o.near.clone()], 300, circling(400.0, 6.0)),
        case("AIS circling 1.2 km at 6 m/s", "circling", "ferry", true, vec![o.ais_rx.clone()], 300, circling(1200.0, 6.0)),
        case("circling 250 m at 4 m/s, four sensors (unseen)", "circling", "fishing_vessel", false, two(), 300, circling(250.0, 4.0)),
        case("zigzag 8 m/s, 90 degrees every 40 s (unseen)", "zigzag", "small_fast_boat", false, two(), 300, zigzag),
        case("sprint 12 m/s 25 s, stop 25 s (unseen)", "sprint and stop", "small_fast_boat", false, two(), 300, sprint_stop),
        case("accelerating 0 to 15 m/s (unseen)", "accelerating", "small_fast_boat", false, two(), 300, accelerating),
        Case {
            name: "weave 7 m/s, 40 degrees either way (unseen)",
            class: "weave",
            kind: "fishing_vessel",
            domain: Domain::Surface,
            ais: false,
            observers: two(),
            n: 300,
            motion: Motion::Drawn(Box::new(weave)),
        },
        Case {
            name: "random legs, drawn per seed (unseen)",
            class: "random legs",
            kind: "small_fast_boat",
            domain: Domain::Surface,
            ais: false,
            observers: two(),
            n: 300,
            motion: Motion::Drawn(Box::new(random_legs)),
        },
    ]
}

/// A zigzag of 60 degree course changes: 6 m/s, course 15 then 75 degrees, 30 s each.
fn zigzag_60(_: u64, len: usize) -> Vec<Kinematics> {
    integrate(-900.0, -900.0, len, |t| {
        let h = if ((t / 30.0).floor() as i64) % 2 == 0 { 15f64 } else { 75f64 }.to_radians();
        (6.0 * h.sin(), 6.0 * h.cos())
    })
}

/// Sprint and stop: 10 m/s for 20 s, still for 30 s.
fn sprint_stop_10(s: f64) -> Kinematics {
    let c = (s / 50.0).floor();
    let r = s - c * 50.0;
    let base = -1200.0 + c * 200.0;
    if r < 20.0 { ((base + 10.0 * r, 0.0), (10.0, 0.0)) } else { ((base + 200.0, 0.0), (0.0, 0.0)) }
}

/// A weave: 5 m/s, course 45 degrees swinging 30 degrees either way over 45 s.
fn weave_30(_: u64, len: usize) -> Vec<Kinematics> {
    integrate(-700.0, -700.0, len, |t| {
        let h = (45.0 + 30.0 * (std::f64::consts::TAU * t / 45.0).sin()).to_radians();
        (5.0 * h.sin(), 5.0 * h.cos())
    })
}

/// Random turns, drawn per seed: 8 m/s, every 15 to 60 s a new course 30 to 120 degrees
/// either way, turned to at 20 degrees a second.
fn random_turns(seed: u64, len: usize) -> Vec<Kinematics> {
    let mut x = seed.wrapping_mul(0xD1B5_4A32_D192_ED03).wrapping_add(0x2545_F491_4F6C_DD1D);
    let mut u = move || {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (x >> 11) as f64 / (1u64 << 53) as f64
    };
    let (mut e, mut n, mut course, mut want) = (-600.0f64, -600.0f64, 45f64, 45f64);
    let mut next = 15.0 + 45.0 * u();
    let mut out = Vec::with_capacity(len);
    for s in 0..len {
        let t = s as f64;
        if t >= next {
            let turn = 30.0 + 90.0 * u();
            want = course + if u() < 0.5 { -turn } else { turn };
            next = t + 15.0 + 45.0 * u();
        }
        let h = course.to_radians();
        out.push(((e, n), (8.0 * h.sin(), 8.0 * h.cos())));
        for _ in 0..10 {
            course += (want - course).clamp(-2.0, 2.0);
            let h = course.to_radians();
            e += 0.8 * h.sin();
            n += 0.8 * h.cos();
        }
    }
    out
}

fn drawn(
    name: &'static str,
    class: &'static str,
    kind: &'static str,
    observers: Vec<Observer>,
    f: fn(u64, usize) -> Vec<Kinematics>,
) -> Case {
    Case {
        name,
        class,
        kind,
        domain: Domain::Surface,
        ais: false,
        observers,
        n: 300,
        motion: Motion::Drawn(Box::new(f)),
    }
}

/// The second held-out set (see the module doc), tuning data since.
pub fn round_two_cases() -> Vec<Case> {
    let o = observers();
    let two = || vec![o.near.clone(), o.near2.clone()];
    vec![
        case("holding, four sensors", "holding", "small_fast_boat", false, two(), 300, holding),
        case("holding, radar only 6 km", "holding", "cargo_ship", false, vec![o.far.clone()], 300, holding),
        case("transit 8 m/s, two observers", "transit", "fishing_vessel", false, vec![o.near.clone(), o.far.clone()], 300, transit(-1200.0, 0.0, 8.0)),
        case("AIS ferry 12 m/s north", "transit", "ferry", true, vec![o.ais_rx.clone()], 300, |s| ((300.0, -1500.0 + 12.0 * s), (0.0, 12.0))),
        case(
            "stops after a 10 m/s transit at 100 s",
            "stop after transit",
            "fishing_vessel",
            false,
            vec![o.near.clone(), o.far.clone()],
            300,
            |s| if s < 100.0 { ((-1000.0 + 10.0 * s, 0.0), (10.0, 0.0)) } else { ((0.0, 0.0), (0.0, 0.0)) },
        ),
        case("a 90 degree turn at 5 deg/s, 6 m/s", "90 degree turn", "small_fast_boat", false, two(), 300, turn(6.0, 5.0)),
        case("circling 600 m at 8 m/s", "circling", "ferry", false, vec![o.near.clone()], 300, circling(600.0, 8.0)),
        drawn("zigzag 6 m/s, 60 degrees every 30 s", "zigzag", "small_fast_boat", two(), zigzag_60),
        case("sprint 10 m/s 20 s, stop 30 s", "sprint and stop", "small_fast_boat", false, two(), 300, sprint_stop_10),
        drawn("weave 5 m/s, 30 degrees either way over 45 s", "weave", "fishing_vessel", two(), weave_30),
        drawn("random legs, drawn per seed", "random legs", "small_fast_boat", two(), random_legs),
        drawn("random turns at 20 deg/s, 8 m/s", "random turns", "small_fast_boat", two(), random_turns),
    ]
}

/// A zigzag of 45 degree course changes: 9 m/s, course 7.5 then 52.5 degrees, 50 s each.
fn zigzag_45(_: u64, len: usize) -> Vec<Kinematics> {
    integrate(-1500.0, -1500.0, len, |t| {
        let h = if ((t / 50.0).floor() as i64) % 2 == 0 { 7.5f64 } else { 52.5f64 }.to_radians();
        (9.0 * h.sin(), 9.0 * h.cos())
    })
}

/// Sprint and stop: 8 m/s for 30 s, still for 20 s.
fn sprint_stop_8(s: f64) -> Kinematics {
    let c = (s / 50.0).floor();
    let r = s - c * 50.0;
    let base = -1200.0 + c * 240.0;
    if r < 30.0 { ((0.0, base + 8.0 * r), (0.0, 8.0)) } else { ((0.0, base + 240.0), (0.0, 0.0)) }
}

/// A weave: 9 m/s, course 120 degrees swinging 50 degrees either way over 90 s.
fn weave_50(_: u64, len: usize) -> Vec<Kinematics> {
    integrate(-1500.0, 800.0, len, |t| {
        let h = (120.0 + 50.0 * (std::f64::consts::TAU * t / 90.0).sin()).to_radians();
        (9.0 * h.sin(), 9.0 * h.cos())
    })
}

/// Random turns, drawn per seed: 6 m/s, every 20 to 90 s a new course 30 to 150 degrees
/// either way, turned to at 10 degrees a second.
fn random_turns_slow(seed: u64, len: usize) -> Vec<Kinematics> {
    let mut x = seed.wrapping_mul(0xA076_1D64_78BD_642F).wrapping_add(0xE703_7ED1_A0B4_28DB);
    let mut u = move || {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (x >> 11) as f64 / (1u64 << 53) as f64
    };
    let (mut e, mut n, mut course, mut want) = (-400.0f64, -400.0f64, 30f64, 30f64);
    let mut next = 20.0 + 70.0 * u();
    let mut out = Vec::with_capacity(len);
    for s in 0..len {
        let t = s as f64;
        if t >= next {
            let turn = 30.0 + 120.0 * u();
            want = course + if u() < 0.5 { -turn } else { turn };
            next = t + 20.0 + 70.0 * u();
        }
        let h = course.to_radians();
        out.push(((e, n), (6.0 * h.sin(), 6.0 * h.cos())));
        for _ in 0..10 {
            course += (want - course).clamp(-1.0, 1.0);
            let h = course.to_radians();
            e += 0.6 * h.sin();
            n += 0.6 * h.cos();
        }
    }
    out
}

/// A patrol box: 800 m sides at 7 m/s, clockwise, turning 90 degrees at 15 degrees a second
/// at each corner (as the simulation's contacts follow a route).
fn patrol_box(_: u64, len: usize) -> Vec<Kinematics> {
    let corners = [(-400.0f64, -400.0f64), (-400.0, 400.0), (400.0, 400.0), (400.0, -400.0)];
    let (mut e, mut n) = (-400.0f64, -400.0f64);
    let mut course = 0f64;
    let mut leg = 1;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        let h = course.to_radians();
        out.push(((e, n), (7.0 * h.sin(), 7.0 * h.cos())));
        for _ in 0..10 {
            let (we, wn) = corners[leg];
            if (we - e).hypot(wn - n) < 10.0 {
                leg = (leg + 1) % 4;
            }
            let (we, wn) = corners[leg];
            let want = (we - e).atan2(wn - n).to_degrees();
            let delta = ((want - course + 540.0) % 360.0) - 180.0;
            course += delta.clamp(-1.5, 1.5);
            let h = course.to_radians();
            e += 0.7 * h.sin();
            n += 0.7 * h.cos();
        }
    }
    out
}

/// The held-out set (see the module doc): written before the average velocity's prediction
/// was run on anything, run once with it; every class in yet other variants, on seeds used
/// nowhere else.
pub fn held_out_cases() -> Vec<Case> {
    let o = observers();
    let two = || vec![o.near.clone(), o.near2.clone()];
    vec![
        case("holding, two observers", "holding", "fishing_vessel", false, vec![o.near.clone(), o.far.clone()], 300, holding),
        case("transit 6 m/s, four sensors", "transit", "fishing_vessel", false, two(), 300, transit(-900.0, 200.0, 6.0)),
        case("AIS ferry 8 m/s east", "transit", "ferry", true, vec![o.ais_rx.clone()], 300, transit(-1200.0, -200.0, 8.0)),
        case(
            "stops after a 12 m/s transit at 150 s, four sensors",
            "stop after transit",
            "small_fast_boat",
            false,
            two(),
            300,
            |s| if s < 150.0 { ((-1800.0 + 12.0 * s, 0.0), (12.0, 0.0)) } else { ((0.0, 0.0), (0.0, 0.0)) },
        ),
        case("a 90 degree turn at 2 deg/s, 10 m/s", "90 degree turn", "fishing_vessel", false, vec![o.near.clone(), o.far.clone()], 300, turn(10.0, 2.0)),
        case("circling 800 m at 5 m/s, four sensors", "circling", "ferry", false, two(), 300, circling(800.0, 5.0)),
        drawn("zigzag 9 m/s, 45 degrees every 50 s", "zigzag", "small_fast_boat", two(), zigzag_45),
        case("sprint 8 m/s 30 s, stop 20 s", "sprint and stop", "small_fast_boat", false, two(), 300, sprint_stop_8),
        drawn("weave 9 m/s, 50 degrees either way over 90 s", "weave", "small_fast_boat", two(), weave_50),
        drawn("random legs, drawn per seed", "random legs", "small_fast_boat", two(), random_legs),
        drawn("random turns at 10 deg/s, 6 m/s", "random turns", "fishing_vessel", two(), random_turns_slow),
        drawn("patrol box 800 m at 7 m/s", "patrol box", "patrol_boat", two(), patrol_box),
    ]
}

/// The tracker before rules 9's review, as it estimated position, velocity and 1-sigma (its
/// arithmetic, replicated for the comparison: a scalar update, a velocity from fixes 10 s
/// apart smoothed 0.8/0.2 or AIS's averaged, a 1-sigma growing at a third of the speed).
struct OldTrack {
    lat: f64,
    lon: f64,
    sigma_m: f64,
    vel: Option<(f64, f64)>,
    vel_anchor: (f64, f64, f64),
    last_seen_t: f64,
}

impl OldTrack {
    fn new(d: &Detection, now: f64) -> OldTrack {
        let mut t = OldTrack {
            lat: d.lat,
            lon: d.lon,
            sigma_m: d.sigma_m,
            vel: None,
            vel_anchor: (d.lat, d.lon, now),
            last_seen_t: now,
        };
        t.update(d, now);
        t
    }

    fn speed(&self) -> f64 {
        self.vel.map_or(0.0, |(e, n)| e.hypot(n))
    }

    fn sigma_at(&self, now: f64) -> f64 {
        self.sigma_m + (0.3 * self.speed()).max(1.0) * (now - self.last_seen_t).max(0.0)
    }

    fn predicted(&self, now: f64) -> (f64, f64) {
        let Some((e, n)) = self.vel else {
            return (self.lat, self.lon);
        };
        let dt = (now - self.last_seen_t).max(0.0);
        (self.lat + n * dt / 111_320.0, self.lon + e * dt / (111_320.0 * self.lat.to_radians().cos()))
    }

    fn update(&mut self, d: &Detection, now: f64) {
        let (plat, plon) = self.predicted(now);
        let sig_t = self.sigma_at(now);
        let k = sig_t * sig_t / (sig_t * sig_t + d.sigma_m * d.sigma_m);
        self.lat = plat + k * (d.lat - plat);
        self.lon = plon + k * (d.lon - plon);
        self.sigma_m = (1.0 / (1.0 / (sig_t * sig_t) + 1.0 / (d.sigma_m * d.sigma_m))).sqrt();
        if let Some((e, n)) = d.velocity_mps {
            self.vel = Some(match self.vel {
                Some((ve, vn)) => (0.5 * ve + 0.5 * e, 0.5 * vn + 0.5 * n),
                None => (e, n),
            });
            self.vel_anchor = (self.lat, self.lon, now);
        } else {
            let (alat, alon, at) = self.vel_anchor;
            let dt = now - at;
            if dt >= 10.0 {
                let n = (self.lat - alat) * 111_320.0 / dt;
                let e = (self.lon - alon) * 111_320.0 * self.lat.to_radians().cos() / dt;
                self.vel = Some(match self.vel {
                    Some((ve, vn)) => (0.8 * ve + 0.2 * e, 0.8 * vn + 0.2 * n),
                    None => (e, n),
                });
                self.vel_anchor = (self.lat, self.lon, now);
            }
        }
        self.last_seen_t = now;
    }

    fn facts(&self, now: f64, domain: Domain) -> TrackFacts {
        let t = ms(self.last_seen_t);
        TrackFacts {
            position: Fact::new(LonLat::new(self.lon, self.lat), t),
            alt_m: None,
            alt_t: None,
            velocity_mps: self.vel.map(|(e, n)| [e, n]),
            pos_sigma_m: self.sigma_at(now),
            sigma_growth_mps: Some((0.3 * self.speed()).max(1.0)),
            confidence: 0.9,
            affiliation: Fact::new(Affiliation::Neutral, t),
            affiliation_confidence: 0.8,
            identity: None,
            identity_confidence: 0.0,
            sources: Vec::new(),
            source_t: Default::default(),
            domain: domain.decide(),
            last_seen_t: t,
            declared_by: None,
            declaration: None,
            review_flags: Vec::new(),
            manoeuvre: None,
            mean_velocity_mps: None,
            first_seen_t: None,
        }
    }
}

pub(super) fn ms(s: f64) -> Time {
    Time((s * 1000.0).round() as i64)
}

/// Sums over the scored seconds.
#[derive(Debug, Default, Clone, Copy)]
pub struct Tally {
    pub n: f64,
    /// squared error over the per-axis 1-sigma squared (NEES; expected 2)
    pub nees: f64,
    /// the truth within two sigmas (expected 0.865)
    pub inside: f64,
    pub sigma: f64,
    /// the velocity's squared error over its per-axis 1-sigma squared, while known
    pub vnees: f64,
    pub vn: f64,
    /// the manoeuvring model's probability
    pub man_p: f64,
    /// seconds without a velocity estimate
    pub unknown_v: f64,
}

impl Tally {
    pub(super) fn add(&mut self, d2: f64, sigma: f64) {
        self.n += 1.0;
        self.nees += d2 / (sigma * sigma);
        self.inside += f64::from(u8::from(d2.sqrt() <= 2.0 * sigma));
        self.sigma += sigma;
    }
    pub fn mean_nees(&self) -> f64 {
        self.nees / self.n.max(1.0)
    }
    pub fn coverage(&self) -> f64 {
        self.inside / self.n.max(1.0)
    }
}

/// A keep-out's hold on the truth `span` seconds ahead.
#[derive(Debug, Default, Clone, Copy)]
pub struct Hold {
    pub n: f64,
    /// the truth inside a region the keep-out counts
    pub covered: f64,
    /// the largest region's radius, m (how much the keep-out holds clear)
    pub reach: f64,
}

impl Hold {
    pub fn coverage(&self) -> f64 {
        self.covered / self.n.max(1.0)
    }
    pub fn mean_reach(&self) -> f64 {
        self.reach / self.n.max(1.0)
    }
}

/// Both filters' scores on one case and seed.
#[derive(Debug, Default, Clone, Copy)]
pub struct Scores {
    pub new: Tally,
    pub old: Tally,
    pub new_hold: [Hold; 2],
    pub old_hold: [Hold; 2],
}

impl Scores {
    pub fn add(&mut self, o: &Scores) {
        let t = |a: &mut Tally, b: &Tally| {
            a.n += b.n;
            a.nees += b.nees;
            a.inside += b.inside;
            a.sigma += b.sigma;
            a.vnees += b.vnees;
            a.vn += b.vn;
            a.man_p += b.man_p;
            a.unknown_v += b.unknown_v;
        };
        let h = |a: &mut Hold, b: &Hold| {
            a.n += b.n;
            a.covered += b.covered;
            a.reach += b.reach;
        };
        t(&mut self.new, &o.new);
        t(&mut self.old, &o.old);
        for k in 0..2 {
            h(&mut self.new_hold[k], &o.new_hold[k]);
            h(&mut self.old_hold[k], &o.old_hold[k]);
        }
    }
}

pub(super) fn hold(f: &TrackFacts, now: f64, span: f64, truth: (f64, f64), ec: &EngageRules, h: &mut Hold) {
    let regions = track_reach(f, ms(now + span), ec);
    let p = LonLat::new(truth.1, truth.0);
    h.n += 1.0;
    h.covered += f64::from(u8::from(regions.iter().any(|r| haversine_m(r.center, p) <= r.radius_m)));
    h.reach += regions.iter().map(|r| r.radius_m).fold(0.0, f64::max);
}

/// Run both filters on `case` at `seed`.
pub fn run(case: &Case, seed: u64) -> Scores {
    let ec = EngageRules::default();
    let table = case.table(seed, case.n as usize + SPANS_S[1] as usize + 1);
    let mut new: Option<Track> = None;
    let mut old: Option<OldTrack> = None;
    let mut sc = Scores::default();
    for s in 0..case.n {
        let now = T0 + f64::from(s);
        let ((e, n), (ve, vn)) = table[s as usize];
        let (lat, lon) = at(BASE.0, BASE.1, e, n);
        let mut c = contact("c", case.kind, case.domain, lat, lon);
        c.speed_mps = ve.hypot(vn);
        c.course_deg = ve.atan2(vn).to_degrees().rem_euclid(360.0);
        if case.ais {
            c.ais = Some(Ais { mmsi: "366000001".into(), name: "MV TEST".into(), ship_type: "passenger".into(), spoofed: false });
            c.emits = true;
        }
        let params = Params { seed, tick: i64::from(s), misclassification_rate: 0.0 };
        let dets = detect(params, &case.observers, &[c], &|_, _| false);
        for d in &dets {
            match &mut new {
                None => new = Some(Track::new("T-0001".into(), 1, d, now)),
                Some(t) => t.update(d, now),
            }
            match &mut old {
                None => old = Some(OldTrack::new(d, now)),
                Some(t) => t.update(d, now),
            }
        }
        let (Some(nt), Some(ot)) = (&new, &old) else { continue };
        if s < SETTLE_S || dets.is_empty() {
            continue;
        }
        let err2 = |tlat: f64, tlon: f64| {
            let de = (tlon - lon) * 111_320.0 * lat.to_radians().cos();
            let dn = (tlat - lat) * 111_320.0;
            de * de + dn * dn
        };
        sc.new.add(err2(nt.lat, nt.lon), nt.sigma_m);
        sc.new.man_p += nt.manoeuvring_p();
        if let Some((e, n)) = nt.vel {
            let sv = nt.vel_sigma_mps();
            sc.new.vnees += ((e - ve).powi(2) + (n - vn).powi(2)) / (sv * sv);
            sc.new.vn += 1.0;
        }
        sc.old.add(err2(ot.lat, ot.lon), ot.sigma_m);
        let (nf, of) = (nt.facts(now), ot.facts(now, case.domain));
        if nf.velocity_mps.is_none() {
            sc.new.unknown_v += 1.0;
        }
        for (k, span) in SPANS_S.iter().enumerate() {
            let ((e2, n2), _) = table[s as usize + *span as usize];
            let ahead = at(BASE.0, BASE.1, e2, n2);
            hold(&nf, now, *span, ahead, &ec, &mut sc.new_hold[k]);
            hold(&of, now, *span, ahead, &ec, &mut sc.old_hold[k]);
        }
    }
    sc
}

/// A case's scores summed over `seeds`, and one line per seed.
pub fn over_seeds(case: &Case, seeds: impl Iterator<Item = u64>) -> (Scores, Vec<(u64, Scores)>) {
    let mut total = Scores::default();
    let mut each = Vec::new();
    for seed in seeds {
        let s = run(case, seed);
        total.add(&s);
        each.push((seed, s));
    }
    (total, each)
}

/// One line of the table.
pub fn line(name: &str, s: &Scores) -> String {
    format!(
        "{name:<52} new NEES {:5.2} within 2 sigma {:5.1}% vNEES {:5.2} pman {:4.2} nov {:4.2} | old {:5.2} {:5.1}% | keep-out 60 s new {:5.1}% (reach {:4.0} m) old {:5.1}% ({:4.0} m) | 100 s new {:5.1}% ({:4.0} m) old {:5.1}% ({:4.0} m)",
        s.new.mean_nees(),
        100.0 * s.new.coverage(),
        s.new.vnees / s.new.vn.max(1.0),
        s.new.man_p / s.new.n.max(1.0),
        s.new.unknown_v / s.new.n.max(1.0),
        s.old.mean_nees(),
        100.0 * s.old.coverage(),
        100.0 * s.new_hold[0].coverage(),
        s.new_hold[0].mean_reach(),
        100.0 * s.old_hold[0].coverage(),
        s.old_hold[0].mean_reach(),
        100.0 * s.new_hold[1].coverage(),
        s.new_hold[1].mean_reach(),
        100.0 * s.old_hold[1].coverage(),
        s.old_hold[1].mean_reach(),
    )
}

/// Print every case of the tuning sets over their seeds (a tuning aid: `cargo test -p
/// murmur-bridge --release calib_table -- --ignored --nocapture`; `CALIB=held_out` for the
/// held-out set).
#[test]
#[ignore]
fn calib_table() {
    let held = std::env::var("CALIB").is_ok_and(|v| v == "held_out");
    let sets = if held {
        vec![(held_out_cases(), HELD_OUT_SEEDS)]
    } else {
        vec![
            (first_cases(), FIRST_SEEDS),
            (round_one_cases(), ROUND_ONE_SEEDS),
            (round_two_cases(), ROUND_TWO_SEEDS),
        ]
    };
    let mut by_class: std::collections::BTreeMap<&str, Scores> = Default::default();
    for (cases, seeds) in &sets {
        for c in cases {
            let (s, _) = over_seeds(c, seeds.clone());
            eprintln!("{}", line(c.name, &s));
            by_class.entry(c.class).or_default().add(&s);
        }
    }
    eprintln!("--- by class");
    for (class, s) in &by_class {
        eprintln!("{}", line(class, s));
    }
}

/// A set's failures against the acceptance criteria. When updated, per class (its cases and
/// seeds pooled): the truth within two sigmas at least 83% of the time (a 2D Gaussian gives
/// 86.5%) and the mean normalized error at most 2.6 (expected 2), so no class is
/// overconfident; and at least 0.5, so none is uselessly loose. A keep-out 60 and 100 s
/// ahead, per case: the truth inside it at least as often as with the old filter.
fn failures(cases: &[Case], seeds: std::ops::RangeInclusive<u64>, table: &mut Vec<String>) -> Vec<String> {
    let mut bad = Vec::new();
    let mut by_class: std::collections::BTreeMap<&str, Scores> = Default::default();
    for c in cases {
        let (s, _) = over_seeds(c, seeds.clone());
        table.push(line(c.name, &s));
        by_class.entry(c.class).or_default().add(&s);
        for (k, span) in SPANS_S.iter().enumerate() {
            let (new, old) = (s.new_hold[k].coverage(), s.old_hold[k].coverage());
            if new < old {
                bad.push(format!(
                    "{}: a keep-out {span:.0} s ahead holds it {:.1}% of the time, the old filter's {:.1}%",
                    c.name,
                    100.0 * new,
                    100.0 * old
                ));
            }
        }
    }
    for (class, s) in &by_class {
        let (nees, cover) = (s.new.mean_nees(), s.new.coverage());
        if !(cover >= 0.83 && (0.5..=2.6).contains(&nees)) {
            bad.push(format!("{class}: NEES {nees:.2}, within 2 sigma {:.1}% when updated", 100.0 * cover));
        }
    }
    bad
}

#[test]
fn the_seed_sets_are_disjoint() {
    let sets = [FIRST_SEEDS, ROUND_ONE_SEEDS, ROUND_TWO_SEEDS, HELD_OUT_SEEDS];
    for (i, a) in sets.iter().enumerate() {
        for b in &sets[i + 1..] {
            assert!(a.end() < b.start() || b.end() < a.start(), "{a:?} and {b:?} overlap");
        }
    }
}

/// The acceptance test (review of rules 9): on the held-out set, never tuned on, the tracker
/// is honest when updated in every motion class, and a keep-out built from it 60 and 100 s
/// ahead holds every motion at least as often as one built from the tracker before the
/// review.
#[test]
fn on_held_out_seeds_and_motions_the_tracker_is_honest_and_a_keep_out_holds_at_least_as_often() {
    let mut table = Vec::new();
    let bad = failures(&held_out_cases(), HELD_OUT_SEEDS, &mut table);
    assert!(bad.is_empty(), "{}\n\nall:\n{}", bad.join("\n"), table.join("\n"));
}

/// The same criteria on the tuning sets (what the constants were chosen on), so a change that
/// breaks them shows here first.
#[test]
fn on_the_tuning_sets_the_tracker_is_honest_and_a_keep_out_holds_at_least_as_often() {
    let mut table = Vec::new();
    let mut bad = failures(&first_cases(), FIRST_SEEDS, &mut table);
    bad.extend(failures(&round_one_cases(), ROUND_ONE_SEEDS, &mut table));
    bad.extend(failures(&round_two_cases(), ROUND_TWO_SEEDS, &mut table));
    assert!(bad.is_empty(), "{}\n\nall:\n{}", bad.join("\n"), table.join("\n"));
}

/// What the tracker tells a keep-out: a zigzagging boat's facts say it manoeuvred, with the
/// fastest it went, and give its average velocity of the last minute (north-east, as the
/// zigzag goes, not the leg it is on); a boat holding still shows no manoeuvre.
#[test]
fn a_track_that_manoeuvred_says_so_in_its_facts() {
    let facts_at = |case: &Case, at_s: u32| {
        let table = case.table(7, case.n as usize + 1);
        let mut track: Option<Track> = None;
        for s in 0..=at_s {
            let now = T0 + f64::from(s);
            let ((e, n), (ve, vn)) = table[s as usize];
            let (lat, lon) = at(BASE.0, BASE.1, e, n);
            let mut c = contact("c", case.kind, case.domain, lat, lon);
            c.speed_mps = ve.hypot(vn);
            c.course_deg = ve.atan2(vn).to_degrees().rem_euclid(360.0);
            let dets = detect(Params { seed: 7, tick: i64::from(s), misclassification_rate: 0.0 }, &case.observers, &[c], &|_, _| false);
            for d in &dets {
                match &mut track {
                    None => track = Some(Track::new("T-0001".into(), 1, d, now)),
                    Some(t) => t.update(d, now),
                }
            }
        }
        let t = track.expect("a track");
        t.facts(t.last_seen_t)
    };
    let cases = round_one_cases();
    let zigzag = cases.iter().find(|c| c.class == "zigzag").expect("zigzag");
    let f = facts_at(zigzag, 200);
    let m = f.manoeuvre.expect("a zigzag is a manoeuvre");
    assert!(m.speed_mps >= 8.0, "{m:?}");
    let [e, n] = f.mean_velocity_mps.expect("an average velocity");
    // the zigzag makes good 320 m east and 320 m north every 80 s: 4 m/s each way
    assert!((e - 4.0).abs() < 2.5 && (n - 4.0).abs() < 2.5, "{e:.1}, {n:.1}");
    let holding = cases.iter().find(|c| c.class == "holding").expect("holding");
    let f = facts_at(holding, 200);
    assert!(f.manoeuvre.is_none(), "{:?}", f.manoeuvre);
    let [e, n] = f.mean_velocity_mps.unwrap_or([0.0, 0.0]);
    assert!(e.hypot(n) < 1.0, "{e:.2}, {n:.2}");
    // rules 12 (murmur-9m0): the facts say when the tracker started the track; young (a
    // velocity, no average velocity yet: its history too short to show a manoeuvre) for its
    // first 45 s or so, so a keep-out counts it as possibly manoeuvring until then
    let first = facts_at(holding, 0).position.t;
    let young = facts_at(zigzag, 20);
    assert_eq!(young.first_seen_t, Some(first), "{young:?}");
    assert!(young.velocity_mps.is_some() && young.mean_velocity_mps.is_none(), "{young:?}");
    let grown = facts_at(zigzag, 60);
    assert_eq!(grown.first_seen_t, Some(first));
    assert!(grown.mean_velocity_mps.is_some(), "{grown:?}");
}

/// The live run's clock (review of 10-05, the coast guard cutter's 79%): on this host the
/// bridge's wall clock steps back about 0.94 s every 32 s while the contacts process's does
/// not, so a fix arriving 0.06 s after the last by the wall clock was taken a second after it.
/// Dated by its arrival, an AIS ferry at 10 m/s, its velocity measured to 0.3 m/s, is
/// predicted 9 m short at every step and stays off by metres while its sigma says one or two.
/// Dated by the truth message's own clock, it is honest.
#[test]
fn a_wall_clock_that_steps_back_does_not_bend_a_track() {
    use super::super::{Settings, Tracks};
    use super::{catalog, settings, truth};
    let o = observers();
    let (mut nees, mut inside, mut n) = (0.0, 0usize, 0usize);
    for seed in [3u64, 4, 5] {
        let tracks = Tracks::new(catalog(), Settings { seed: Some(seed), ..settings() });
        for s in 0..300u32 {
            let sf = f64::from(s);
            let ((e, nn), (ve, vn)): Kinematics = ((-1500.0 + 10.0 * sf, 300.0), (10.0, 0.0));
            let (lat, lon) = at(BASE.0, BASE.1, e, nn);
            let mut c = contact("c", "ferry", Domain::Surface, lat, lon);
            c.speed_mps = ve.hypot(vn);
            c.course_deg = 90.0;
            c.ais = Some(Ais { mmsi: "366000001".into(), name: "MV TEST".into(), ship_type: "passenger".into(), spoofed: false });
            c.emits = true;
            // the message is dated by the contacts process; the bridge's wall clock stepped
            // back 0.94 s every 32 s
            let wall = T0 + sf - 0.94 * (sf / 32.0).floor();
            tracks.on_truth_with(truth(sf, vec![c]), std::slice::from_ref(&o.ais_rx), wall).expect("ok");
            if s < 30 {
                continue;
            }
            let Some(t) = tracks.track("T-0001") else { continue };
            let de = (t.lon - lon) * 111_320.0 * lat.to_radians().cos();
            let dn = (t.lat - lat) * 111_320.0;
            let d2 = de * de + dn * dn;
            nees += d2 / (t.sigma_m * t.sigma_m);
            inside += usize::from(d2.sqrt() <= 2.0 * t.sigma_m);
            n += 1;
        }
    }
    let (nees, cover) = (nees / n as f64, inside as f64 / n as f64);
    assert!(cover >= 0.83 && nees <= 2.6, "NEES {nees:.2}, within 2 sigma {:.1}%", 100.0 * cover);
}
