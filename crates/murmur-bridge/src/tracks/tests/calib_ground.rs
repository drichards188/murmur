//! The tracker against the truth of contacts on land (murmur-e3b): people and vehicles as the
//! contacts process moves them (`murmur-contacts` `motion.rs`: a route's waypoints at a kind's
//! speed and turn rate, speeding up at 0.5 m/s^2 from a stop, stopping at once), seen by the
//! physical sensing model as world v3's fleet sees the shore: a MALE aircraft's radar from 7,600
//! m (an MQ-9B-class radar, a plot every 5 s of 1-sigma about 1 degree of the slant range, 150 to
//! 200 m here), a ship offshore with radar and EO/IR, a quadcopter's camera. Until this module
//! the ground models' constants (`model::process_noise`) were scaled from the surface's and never
//! checked against truth, and no calibration set had a ground contact.
//!
//! Two sets, disjoint in seeds from each other and from the surface and air sets, written
//! together before any change to the tracker for murmur-e3b (tracks seen stationary or steady
//! keep a velocity): [`tuning_cases`] on [`GROUND_TUNING_SEEDS`], what the change is developed
//! on, and [`held_out_cases`] on [`GROUND_HELD_OUT_SEEDS`], the same classes in other variants,
//! run once after it (`ground_calib_held_out`). The criteria are the surface's
//! (`calib::failures_with_velocity`), per sensor mix ([`GroundMix`]): per class, the truth within
//! two sigmas at least 83% of the time and the mean normalized error between 0.5 and 2.6 when
//! updated; per case, a keep-out 60 and 100 s ahead holding the contact at least as often as one
//! built from the tracker before rules 9's review; and in [`VELOCITY_CLASSES`] a velocity given
//! within a mean normalized error of 2.6. Each class's share of seconds without a velocity is
//! in the table (`nov`), reported.
//!
//! The order of events: both sets were written and committed (b3e9ecf) before the tracker was
//! changed; the ground constants were chosen on the tuning set ([`ground_tune_grid`],
//! `model::process_noise`); the held-out set was run once after (588fd57): every velocity class
//! honest in every mix and a velocity 90 to 98 % of the time where the contact stands or keeps
//! its course, keep-outs held, but the hairpin road failed position honesty in all three mixes
//! (within two sigmas 78.3 to 81.3 %, NEES 2.44 to 2.74; docs/SENSING.md, "Ground tracks under
//! coarse fixes"). Both sets are spent for that change.

use std::ops::RangeInclusive;

use super::calib::{Case, Kinematics, domain_case, failures_with_velocity, physical, run_by};
use super::{BASE, at, observer};
use crate::tracks::sensing::{Observer, SensorType};
use crate::tracks::truth::Domain;

/// The ground tuning set's seeds.
pub const GROUND_TUNING_SEEDS: RangeInclusive<u64> = 491..=520;
/// The ground held-out set's seeds: used nowhere in tuning.
pub const GROUND_HELD_OUT_SEEDS: RangeInclusive<u64> = 461..=490;
/// Seconds of truth per case: the F15 dev check's no-velocity ground tracks had been tracked for
/// minutes (SCORING-V3.md 12.2).
const N: u32 = 600;

/// The classes in which a velocity given must be honest: a contact holding still or keeping its
/// course and speed, or doing so on either side of one manoeuvre (a stop, a start), as the air
/// calibration's `calib_air::STEADY_CLASSES`. A road's bends and a walk's corners are reported.
pub const VELOCITY_CLASSES: [&str; 4] = ["holding", "transit", "stop after transit", "park and go"];

/// Which of our sensors see the contact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroundMix {
    /// A MALE aircraft's radar alone, 7,600 m up and about 8 km off (the F15 dev check's most
    /// common source of a no-velocity ground track's last fixes).
    Airborne,
    /// The aircraft's radar and a ship 5 km offshore with radar and EO/IR.
    AirborneShip,
    /// Those and a quadcopter's camera 60 m up, 700 m off.
    All,
}

pub const GROUND_MIXES: [GroundMix; 3] = [GroundMix::Airborne, GroundMix::AirborneShip, GroundMix::All];

impl GroundMix {
    pub fn name(self) -> &'static str {
        match self {
            GroundMix::Airborne => "airborne radar",
            GroundMix::AirborneShip => "airborne radar + ship",
            GroundMix::All => "all sensors",
        }
    }

    fn observers(self) -> Vec<Observer> {
        let place = |uid: u64, e: f64, n: f64, alt: f64, sensors: &[(SensorType, f64)]| {
            let (lat, lon) = at(BASE.0, BASE.1, e, n);
            observer(uid, lat, lon, alt, sensors)
        };
        let male = place(7, -6000.0, -5000.0, 7600.0, &[(SensorType::Radar, 80_000.0)]);
        let ship = place(2, 2500.0, -4500.0, 0.0, &[(SensorType::Radar, 20_000.0), (SensorType::EoIr, 8_000.0)]);
        let quad = place(6, 600.0, 400.0, 60.0, &[(SensorType::EoIr, 1_500.0)]);
        match self {
            GroundMix::Airborne => vec![male],
            GroundMix::AirborneShip => vec![male, ship],
            GroundMix::All => vec![male, ship, quad],
        }
    }
}

/// Along `points` (metres east and north of [`BASE`]) at `v`, turning toward the next at
/// `turn_dps` and speeding up at `accel` (from rest, after `wait_s` still at the first point),
/// holding still once at the last, in 0.1 s steps: as the contacts process moves a contact on a
/// route (a waypoint taken within 25 m, a stop at once).
fn drive(points: Vec<(f64, f64)>, v: f64, turn_dps: f64, accel: f64, wait_s: f64) -> impl Fn(u64, usize) -> Vec<Kinematics> {
    move |_, len| {
        let (mut e, mut n) = points[0];
        let mut course = (points[1].0 - e).atan2(points[1].1 - n).to_degrees();
        let (mut speed, mut leg, mut held) = (0.0f64, 1usize, false);
        let mut out = Vec::with_capacity(len);
        for s in 0..len {
            let h = course.to_radians();
            out.push(((e, n), (speed * h.sin(), speed * h.cos())));
            for k in 0..10 {
                let t = s as f64 + f64::from(k) / 10.0;
                if held || t < wait_s {
                    speed = 0.0;
                    continue;
                }
                let (we, wn) = points[leg];
                if (we - e).hypot(wn - n) <= 25f64.max(speed / 10.0) {
                    if leg + 1 == points.len() {
                        held = true;
                        speed = 0.0;
                        continue;
                    }
                    leg += 1;
                }
                let (we, wn) = points[leg];
                let want = (we - e).atan2(wn - n).to_degrees();
                let delta = ((want - course + 540.0) % 360.0) - 180.0;
                course += delta.clamp(-turn_dps / 10.0, turn_dps / 10.0);
                speed = (speed + accel / 10.0).min(v);
                let h = course.to_radians();
                e += speed / 10.0 * h.sin();
                n += speed / 10.0 * h.cos();
            }
        }
        out
    }
}

/// At (`e`, `n`) throughout.
fn still(e: f64, n: f64) -> impl Fn(u64, usize) -> Vec<Kinematics> {
    move |_, len| vec![((e, n), (0.0, 0.0)); len]
}

/// Up to speed at once (a transit already under way).
const AT_SPEED: f64 = 1e9;

fn ground(
    name: &'static str,
    class: &'static str,
    kind: &'static str,
    mix: GroundMix,
    f: impl Fn(u64, usize) -> Vec<Kinematics> + 'static,
) -> Case {
    domain_case(name, class, kind, Domain::Ground, mix.observers(), N, f)
}

/// The tuning set (see the module doc), seen by `mix`.
pub fn tuning_cases(mix: GroundMix) -> Vec<Case> {
    vec![
        ground("a person standing", "holding", "person", mix, still(0.0, 0.0)),
        ground("a parked car", "holding", "vehicle", mix, still(300.0, -200.0)),
        ground("a car at 11 m/s east", "transit", "vehicle", mix, drive(vec![(-3000.0, 0.0), (6000.0, 0.0)], 11.0, 15.0, AT_SPEED, 0.0)),
        ground("a person walking 1.4 m/s north", "transit", "person", mix, drive(vec![(0.0, -500.0), (0.0, 1500.0)], 1.4, 60.0, AT_SPEED, 0.0)),
        ground(
            "a car at 10 m/s parks at about 200 s",
            "stop after transit",
            "vehicle",
            mix,
            drive(vec![(-2000.0, 100.0), (0.0, 100.0)], 10.0, 15.0, AT_SPEED, 0.0),
        ),
        ground(
            "a car at 9 m/s on a winding road, turns at 15 deg/s",
            "road",
            "vehicle",
            mix,
            drive(
                vec![
                    (-2500.0, -500.0),
                    (-1800.0, -200.0),
                    (-1200.0, -600.0),
                    (-500.0, -300.0),
                    (0.0, -700.0),
                    (600.0, -300.0),
                    (1200.0, -700.0),
                    (1800.0, -200.0),
                    (2500.0, -600.0),
                    (3200.0, -300.0),
                    (3900.0, -700.0),
                ],
                9.0,
                15.0,
                AT_SPEED,
                0.0,
            ),
        ),
        ground(
            "a car parked 200 s, then off north at 0.5 m/s^2 to 10 m/s",
            "park and go",
            "vehicle",
            mix,
            drive(vec![(0.0, 0.0), (0.0, 5000.0)], 10.0, 15.0, 0.5, 200.0),
        ),
        ground(
            "a person walking 1.3 m/s on a path with corners, turning at 60 deg/s",
            "walk with turns",
            "person",
            mix,
            drive(
                vec![(0.0, 0.0), (200.0, 0.0), (200.0, 150.0), (400.0, 150.0), (400.0, -100.0), (650.0, -100.0), (650.0, 200.0), (900.0, 200.0)],
                1.3,
                60.0,
                AT_SPEED,
                0.0,
            ),
        ),
    ]
}

/// The held-out set (see the module doc), seen by `mix`: the same classes in other variants,
/// written with the tuning set, run once after the change, never tuned on.
pub fn held_out_cases(mix: GroundMix) -> Vec<Case> {
    vec![
        ground("visitors standing at a viewpoint", "holding", "person", mix, still(-400.0, 600.0)),
        ground("a parked truck", "holding", "truck", mix, still(200.0, 400.0)),
        ground("a truck at 7 m/s north-east", "transit", "truck", mix, drive(vec![(-2000.0, -2000.0), (3000.0, 3000.0)], 7.0, 10.0, AT_SPEED, 0.0)),
        ground("a person walking 1.2 m/s west", "transit", "person", mix, drive(vec![(600.0, 200.0), (-1000.0, 200.0)], 1.2, 60.0, AT_SPEED, 0.0)),
        ground(
            "a car at 11 m/s parks at about 150 s",
            "stop after transit",
            "vehicle",
            mix,
            drive(vec![(-1650.0, -300.0), (0.0, -300.0)], 11.0, 15.0, AT_SPEED, 0.0),
        ),
        ground(
            "a car at 11 m/s up a hairpin road, turns at 15 deg/s",
            "road",
            "vehicle",
            mix,
            drive(
                vec![
                    (0.0, -2000.0),
                    (400.0, -1500.0),
                    (-300.0, -1100.0),
                    (400.0, -700.0),
                    (-300.0, -300.0),
                    (400.0, 100.0),
                    (-300.0, 500.0),
                    (400.0, 900.0),
                    (-300.0, 1300.0),
                    (400.0, 1700.0),
                    (-300.0, 2100.0),
                    (400.0, 2500.0),
                    (-300.0, 2900.0),
                    (400.0, 3300.0),
                ],
                11.0,
                15.0,
                AT_SPEED,
                0.0,
            ),
        ),
        ground(
            "a car parked 150 s, then off south at 0.5 m/s^2 to 9 m/s",
            "park and go",
            "vehicle",
            mix,
            drive(vec![(500.0, 500.0), (500.0, -4500.0)], 9.0, 15.0, 0.5, 150.0),
        ),
        ground(
            "a person walking 1.4 m/s round a block, turning at 60 deg/s",
            "walk with turns",
            "person",
            mix,
            drive(
                vec![(-200.0, -200.0), (-200.0, 200.0), (200.0, 200.0), (200.0, -200.0), (-200.0, -200.0), (-200.0, 200.0), (200.0, 200.0), (200.0, -200.0), (-200.0, -200.0)],
                1.4,
                60.0,
                AT_SPEED,
                0.0,
            ),
        ),
    ]
}

/// A set's failures on every mix, its table appended to `table`.
fn failures(cases: impl Fn(GroundMix) -> Vec<Case>, seeds: RangeInclusive<u64>, table: &mut Vec<String>) -> Vec<String> {
    let mut bad = Vec::new();
    for mix in GROUND_MIXES {
        table.push(format!("--- {}", mix.name()));
        let found = failures_with_velocity(&cases(mix), seeds.clone(), table, &|c, seed| run_by(c, seed, &mut physical(seed)), &VELOCITY_CLASSES);
        bad.extend(found.into_iter().map(|b| format!("({}) {b}", mix.name())));
    }
    bad
}

/// The ground tuning set on the physical sensing model (a tuning aid: `cargo test -p
/// murmur-bridge --release ground_calib_tuning -- --ignored --nocapture`).
#[test]
#[ignore]
fn ground_calib_tuning() {
    let mut table = Vec::new();
    let bad = failures(tuning_cases, GROUND_TUNING_SEEDS, &mut table);
    eprintln!("{}", table.join("\n"));
    eprintln!("--- the criteria on the physical model: {}", if bad.is_empty() { "all hold".to_string() } else { format!("\n{}", bad.join("\n")) });
}

/// The grid the ground models' white-noise acceleration was chosen on (`model::process_noise`):
/// the tuning set's criteria for each (steady, manoeuvring) pair, with each class's share of
/// seconds without a velocity (a tuning aid: `cargo test -p murmur-bridge --release
/// ground_tune_grid -- --ignored --nocapture`).
#[test]
#[ignore]
fn ground_tune_grid() {
    for q in [[0.06, 12.0], [0.06, 2.0], [0.06, 1.0], [0.02, 2.0], [0.02, 1.0], [0.01, 2.0], [0.01, 1.5], [0.01, 1.25], [0.01, 1.0], [0.01, 0.75], [0.01, 0.5], [0.005, 1.0]] {
        crate::tracks::model::GROUND_TUNE.set(Some(q));
        let mut table = Vec::new();
        let bad = failures(tuning_cases, GROUND_TUNING_SEEDS, &mut table);
        eprintln!("=== steady {} manoeuvring {}: {}", q[0], q[1], if bad.is_empty() { "all hold".to_string() } else { bad.join("; ") });
        for l in table.iter().filter(|l| l.starts_with("---") || l.starts_with("class")) {
            eprintln!("{}", &l[..l.len().min(140)]);
        }
    }
    crate::tracks::model::GROUND_TUNE.set(None);
}

/// Deterministic noise for the tests below: standard normal draws from a seed.
fn normals(seed: u64) -> impl FnMut() -> f64 {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x2545_F491_4F6C_DD1D;
    let mut u = move || {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        ((x >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    };
    move || (-2.0 * u().ln()).sqrt() * (std::f64::consts::TAU * u()).cos()
}

/// A ground contact at `v` m/s east fixed every 5 s for 15 minutes, each fix 150 m off its
/// truth (1-sigma, each way), as the F15 dev check's MALE radar fixed the ground tracks whose
/// velocity was withheld (SCORING-V3.md 12.2): after the first two minutes, the share of fixes
/// after which the track has a velocity, and the mean normalized error (two dimensions,
/// expected 2) of the velocities it has.
fn coarse_fixes(v: f64, seed: u64) -> (f64, f64) {
    use crate::tracks::model::Track;
    use crate::tracks::sensing::Detection;
    let mut noise = normals(seed);
    let (sigma, every) = (150.0, 5.0);
    let mut track: Option<Track> = None;
    let (mut n, mut with_v, mut nees) = (0.0, 0.0, 0.0);
    for k in 0..180 {
        let t = super::T0 + every * f64::from(k);
        let (lat, lon) = at(BASE.0, BASE.1, v * every * f64::from(k) + sigma * noise(), sigma * noise());
        let d = Detection {
            contact_id: "c".into(),
            uid: 7,
            sensor: SensorType::Radar,
            lat,
            lon,
            alt_m: None,
            sigma_m: sigma,
            range_m: 10_000.0,
            velocity_mps: None,
            label: Some("small_ground".into()),
            ais: None,
            domain: Domain::Ground,
            measured_t: Some(t),
            slant_from: None,
            look: None,
            scan_s: Some(5.0),
        };
        match &mut track {
            None => track = Some(Track::new("T-0001".into(), 1, &d, t)),
            Some(tr) => tr.update(&d, t),
        }
        let tr = track.as_ref().expect("a track");
        if every * f64::from(k) < 120.0 {
            continue;
        }
        n += 1.0;
        if let Some((e, nn)) = tr.vel {
            let s = tr.vel_sigma_mps();
            with_v += 1.0;
            nees += ((e - v).powi(2) + nn.powi(2)) / (s * s);
        }
    }
    (with_v / n, nees / with_v.max(1.0))
}

/// murmur-e3b: a person standing, fixed only by a coarse radar every 5 s, keeps a velocity (near
/// none, and honest), where the ground models before their calibration withheld it for good.
#[test]
fn a_contact_standing_under_a_coarse_radar_keeps_an_honest_velocity() {
    for seed in [1, 2, 3] {
        let (share, nees) = coarse_fixes(0.0, seed);
        assert!(share >= 0.8 && nees <= 2.6, "seed {seed}: a velocity after {:.0}% of fixes, NEES {nees:.2}", 100.0 * share);
    }
}

/// murmur-e3b: a car keeping its course and speed (10 m/s) under the same radar keeps a
/// velocity too, honest.
#[test]
fn a_contact_moving_steadily_under_a_coarse_radar_keeps_an_honest_velocity() {
    for seed in [1, 2, 3] {
        let (share, nees) = coarse_fixes(10.0, seed);
        assert!(share >= 0.8 && nees <= 2.6, "seed {seed}: a velocity after {:.0}% of fixes, NEES {nees:.2}", 100.0 * share);
    }
}

/// The ground held-out set (murmur-e3b): [`held_out_cases`] on [`GROUND_HELD_OUT_SEEDS`],
/// written before the tracker change, run once after it, every criterion reported (`cargo test
/// -p murmur-bridge --release -- --ignored --exact
/// tracks::tests::calib_ground::ground_calib_held_out --nocapture`). A record, not a target: a
/// later change is judged on a set written for it.
#[test]
#[ignore]
fn ground_calib_held_out() {
    let mut table = Vec::new();
    let bad = failures(held_out_cases, GROUND_HELD_OUT_SEEDS, &mut table);
    eprintln!("{}", table.join("\n"));
    eprintln!("--- the criteria on the physical model: {}", if bad.is_empty() { "all hold".to_string() } else { format!("\n{}", bad.join("\n")) });
}
