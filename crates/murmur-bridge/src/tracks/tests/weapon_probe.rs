//! Opponent weapons, the design's measurement (docs/OPPONENT-WEAPONS.md, "What our sensors make
//! of it"): what the physical model's own functions give against an ATGM-class missile fired from
//! the tutorial's fast boat at HUNTER 1, by day. Ignored (a probe, not a check):
//! `cargo test -p murmur-bridge weapon_probe -- --ignored --nocapture`.
//!
//! - Radar: every scan pass during the flight, one Swerling 1 draw each at the missile's range
//!   then (`radar::pd`, the catalog's figures), the scan's phase spread evenly over the period;
//!   as an air target (no sea clutter, as the model treats `Domain::Air`) and as a target on the
//!   sea's surface (the clutter in its cell, a Doppler radar seeing it clear of the band).
//! - EO/IR in flight: each level accrues as the engine's looks do (`P^2 / tau` a second while in
//!   the field), the panoramic ring always in the field.
//! - The launch: the flash and plume as a steady-state look (a transient, no search time), for
//!   each channel with the boat in its field.
//! - REAPER 1's camera: the share of seconds its payload manager has the boat in its field (wide or
//!   narrow), from the engine itself stepped over ten minutes with the boat and HUNTER 1's radar
//!   cueing it.

use super::super::physical::conditions::Conditions;
use super::super::physical::eo;
use super::super::physical::geometry::{K_RADIO, en, visible_height_m};
use super::super::physical::spec::{EoChannel, RadarSensor, Resolved, resolve};
use super::super::physical::{Engine, Step, Target, pair, radar, sensor_height_m};
use super::super::sensing::{Observer, SensorType};
use super::super::truth::{ContactTruth, Domain, Sensing, TruthAffiliation};
use super::{at, catalog};

/// The tutorial's holding point (sim/scenarios/tutorial.toml) and REAPER 1's spawn.
const BOAT: (f64, f64) = (37.8120, -122.4950);
const REAPER: (f64, f64) = (37.8000, -122.5250);
/// The missile: speed, flight height above the sea, radar cross-section, and its critical
/// dimension and contrast head-on (seen by its victim) and side-on (by anyone else).
const SPEED: f64 = 250.0;
const FLY_M: f64 = 4.0;
const RCS: f64 = 0.01;
const HEAD_ON: (f64, f64) = (0.15, 30.0);
const SIDE_ON: (f64, f64) = (0.4, 100.0);
/// The launch: flash, plume and smoke as one target.
const PLUME: (f64, f64) = (2.0, 300.0);

fn radar_of(platform: &str, airborne: bool) -> RadarSensor {
    let s = catalog().get(platform).expect(platform).sensors.iter().find(|s| s.kind == SensorType::Radar).cloned().expect("a radar");
    match resolve(&s, airborne) {
        Resolved::Radar(r) => r,
        other => panic!("{other:?}"),
    }
}

fn eo_of(platform: &str, airborne: bool) -> Vec<EoChannel> {
    let s = catalog().get(platform).expect(platform).sensors.iter().find(|s| s.kind == SensorType::EoIr).cloned().expect("a camera");
    match resolve(&s, airborne) {
        Resolved::EoIr(e) => e.channels,
        other => panic!("{other:?}"),
    }
}

/// Where the missile is `t` s after the launch, flying from the boat to `to` (east, north of the
/// boat), m: (east, north).
fn missile(to: (f64, f64), t: f64) -> (f64, f64) {
    let d = to.0.hypot(to.1);
    let f = (SPEED * t / d).min(1.0);
    (to.0 * f, to.1 * f)
}

/// The radar's plots of one flight: the expected number and the chance of at least one, the scan
/// phase spread over `n` values; `sea`: as a target on the sea's surface.
fn radar_plots(r: &RadarSensor, sensor: (f64, f64, f64), to: (f64, f64), sea: bool) -> (f64, f64, f64) {
    let tof = to.0.hypot(to.1) / SPEED;
    let n = 200;
    let (mut plots, mut any, mut most) = (0.0, 0.0, 0.0f64);
    for i in 0..n {
        let phase = (i as f64 + 0.5) / n as f64 * r.scan_s;
        let (mut e, mut miss) = (0.0, 1.0);
        let mut k = 0.0;
        loop {
            let t = phase + k * r.scan_s;
            if t >= tof {
                break;
            }
            k += 1.0;
            let (x, y) = missile(to, t);
            let ground = (x - sensor.0).hypot(y - sensor.1);
            let slant = ground.hypot(FLY_M - sensor.2);
            let h_t = visible_height_m(FLY_M, FLY_M, ground, sensor.2, K_RADIO);
            // its speed along the line of sight from the radar
            let (vx, vy) = (to.0 / to.0.hypot(to.1) * SPEED, to.1 / to.0.hypot(to.1) * SPEED);
            let radial = ((x - sensor.0) * vx + (y - sensor.1) * vy) / ground.max(1.0) * (ground / slant);
            let s = sea.then_some(radar::Sea { state: 2.0, wind_mps: Conditions::default().wind_mps, radial_mps: radial });
            let p = radar::pd(r, RCS, slant, ground, sensor.2, h_t, s);
            e += p;
            miss *= 1.0 - p;
        }
        plots += e;
        any += 1.0 - miss;
        most = most.max(k);
    }
    (plots / n as f64, any / n as f64, most)
}

/// A camera's chance of detecting the missile in flight, and of recognising it, given the whole
/// flight in its field (the engine's accrual: `P^2 / tau` a second; a recognition only a second
/// after the detection, at its own tau of 1 s).
fn eo_flight(chs: &[EoChannel], narrow: bool, sensor: (f64, f64, f64), to: (f64, f64), look: (f64, f64)) -> (f64, f64, f64) {
    let tof = to.0.hypot(to.1) / SPEED;
    let dt = 0.05;
    let (mut acc, mut acc_r, mut t, mut det_at) = (0.0, 0.0, 0.0, None::<f64>);
    let mut p_det = 0.0;
    let mut early = 0.0;
    let cond = Conditions::default();
    while t < tof {
        let (x, y) = missile(to, t);
        let slant = (x - sensor.0).hypot(y - sensor.1).hypot(FLY_M - sensor.2);
        let p = chs
            .iter()
            .map(|ch| eo::channel_p(ch, narrow, slant, [sensor.2, FLY_M], look.0, look.1, &cond, 45.0))
            .fold([0.0f64; 3], |a, b| [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])]);
        acc += p[0] * p[0] / eo::TAU_K_S * dt;
        if det_at.is_none() && 1.0 - (-acc).exp() >= 0.5 {
            det_at = Some(t);
        }
        if det_at.is_some_and(|d| t - d >= 1.0) {
            acc_r += p[1] * p[1] * dt;
        }
        p_det = 1.0 - (-acc).exp();
        if t <= tof - 1.0 {
            early = p_det;
        }
        t += dt;
    }
    (p_det, p_det * (1.0 - (-acc_r).exp()), early)
}

/// The launch's chance of being seen by a channel set with the boat in its field: the plume as a
/// steady-state look (`P` itself, no search time).
fn flash(chs: &[EoChannel], narrow: bool, slant: f64, h_s: f64) -> f64 {
    let cond = Conditions::default();
    chs.iter().map(|ch| eo::channel_p(ch, narrow, slant, [h_s, 2.0], PLUME.0, PLUME.1, &cond, 45.0)[0]).fold(0.0, f64::max)
}

fn in_field(aim: (f64, f64), p: (f64, f64), hfov: f64) -> bool {
    let daz = (p.0 - aim.0 + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI;
    daz.abs() * p.1.cos() <= 0.5 * hfov && (p.1 - aim.1).abs() <= 0.375 * hfov
}

#[test]
#[ignore = "a probe: prints the design's measurement"]
fn weapon_probe() {
    let hunter_radar = radar_of("seahunter", false);
    let reaper_radar = radar_of("mq9b_armed", true);
    let hunter_eo = eo_of("seahunter", false);
    let reaper_eo = eo_of("mq9b_armed", true);
    println!("ATGM-class: {SPEED} m/s, {FLY_M} m up, RCS {RCS} m^2; head-on {HEAD_ON:?}, side-on {SIDE_ON:?}; plume {PLUME:?}; by day");
    println!("HUNTER 1 radar scan {} s; REAPER 1 radar scan {} s", hunter_radar.scan_s, reaper_radar.scan_s);
    for d in [900.0, 1000.0, 1200.0] {
        let to = (d, 0.0);
        let tof = d / SPEED;
        let (e_air, any_air, most) = radar_plots(&hunter_radar, (d, 0.0, 10.0), to, false);
        let (e_sea, any_sea, _) = radar_plots(&hunter_radar, (d, 0.0, 10.0), to, true);
        let (eo_det, eo_rec, eo_early) = eo_flight(&hunter_eo, false, (d, 0.0, 10.0), to, HEAD_ON);
        let clutter = radar::clutter_m2(&hunter_radar, d / 2.0, 10.0, 2.0, Conditions::default().wind_mps);
        let fl = flash(&hunter_eo, false, d, 10.0);
        println!(
            "HUNTER 1 at {d:.0} m (flight {tof:.1} s, at most {most} passes): radar as air {e_air:.2} plots, P(>=1) {any_air:.2}; on the sea's surface {e_sea:.2}, P(>=1) {any_sea:.2}; ring camera detects in flight {eo_det:.2} ({eo_early:.2} a second or more before impact), recognises {eo_rec:.2}; sees the launch {fl:.2}; clutter in its cell half way {clutter:.3} m^2"
        );
    }
    // REAPER 1 at 7,600 m, on a 3,500 m loiter about its spawn, at eight points of the circle
    let (re, rn) = en(BOAT.0, BOAT.1, REAPER.0, REAPER.1);
    let to = (1000.0, 0.0);
    for k in 0..8 {
        let a = std::f64::consts::TAU * k as f64 / 8.0;
        let s = (re + 3500.0 * a.sin(), rn + 3500.0 * a.cos(), 7600.0);
        let ground = s.0.hypot(s.1);
        let slant = ground.hypot(7600.0);
        let (e_air, any_air, _) = radar_plots(&reaper_radar, s, to, false);
        let (e_sea, any_sea, _) = radar_plots(&reaper_radar, s, to, true);
        let (w_det, _, _) = eo_flight(&reaper_eo, false, s, to, SIDE_ON);
        let (n_det, _, _) = eo_flight(&reaper_eo, true, s, to, SIDE_ON);
        println!(
            "REAPER 1 {ground:.0} m out ({slant:.0} m slant): radar as air {e_air:.2} plots, P(>=1) {any_air:.2}; on the surface {e_sea:.2}, P(>=1) {any_sea:.2}; camera on it the whole flight: wide {w_det:.2}, narrow {n_det:.2}; launch seen wide {:.2}, narrow {:.2}",
            flash(&reaper_eo, false, slant, 7600.0),
            flash(&reaper_eo, true, slant, 7600.0)
        );
    }

    // REAPER 1's payload manager: how often has its camera the boat in its field?
    let cat = catalog();
    let sensing = Sensing { conditions: Some(Conditions { sun_elevation_deg: Some(45.0), ..Conditions::default() }), ..Sensing::default() };
    let boat = ContactTruth {
        id: "boat".into(),
        name: "boat".into(),
        affiliation: TruthAffiliation::Hostile,
        domain: Domain::Surface,
        kind: "small_fast_boat".into(),
        lat: BOAT.0,
        lon: BOAT.1,
        alt_m: 0.0,
        speed_mps: 0.0,
        course_deg: 0.0,
        emits: false,
        signature: 0.35,
        ais: None,
    };
    let (hlat, hlon) = at(BOAT.0, BOAT.1, 1000.0, 0.0);
    let hunter = Observer {
        uid: 1,
        lat: hlat,
        lon: hlon,
        alt_msl: 0.0,
        mast_m: cat.get("seahunter").expect("seahunter").mast_m,
        sensors: cat.get("seahunter").expect("seahunter").sensors.clone(),
        heading_deg: Some(0.0),
        ..Observer::default()
    };
    let reaper_si = cat.get("mq9b_armed").expect("mq9b").sensors.iter().position(|s| s.kind == SensorType::EoIr).expect("eo");
    let wide = reaper_eo.iter().map(|c| c.wide_rad).fold(0.0, f64::max);
    let narrow = reaper_eo.iter().map(|c| c.narrow_rad).fold(0.0, f64::max);
    let (mut in_wide, mut in_narrow, mut n) = (0usize, 0usize, 0usize);
    for seed in 1..=10u64 {
        let mut e = Engine::new(None);
        for s in 0..600i64 {
            // REAPER 1 flies its loiter: 90 m/s on 3,500 m, a turn in about four minutes
            let a = 90.0 / 3500.0 * s as f64 + seed as f64;
            let (rlat, rlon) = at(REAPER.0, REAPER.1, 3500.0 * a.sin(), 3500.0 * a.cos());
            let reaper = Observer {
                uid: 2,
                lat: rlat,
                lon: rlon,
                alt_msl: 7600.0,
                mast_m: 0.0,
                sensors: cat.get("mq9b_armed").expect("mq9b").sensors.clone(),
                heading_deg: Some((a.to_degrees() + 90.0).rem_euclid(360.0)),
                ..Observer::default()
            };
            let obs = [hunter.clone(), reaper.clone()];
            let st = Step { seed, tick: s, s: s as f64, t: 1_781_467_800.0 + s as f64, misclassification_rate: 0.0, sensing: &sensing };
            e.step(&st, &obs, std::slice::from_ref(&boat));
            if s < 60 {
                continue;
            }
            n += 1;
            let Some((az, el, nar, busy, _)) = e.gimbal_of(2, reaper_si) else { continue };
            if busy > s as f64 {
                continue;
            }
            let t = Target::new(&boat, None, None);
            let p = pair(&reaper, sensor_height_m(&reaper), &t);
            if in_field((az, el), (p.bearing, p.el), if nar { narrow } else { wide }) {
                if nar {
                    in_narrow += 1;
                } else {
                    in_wide += 1;
                }
            }
        }
    }
    println!(
        "REAPER 1's camera has the boat in its field {:.0} % of seconds ({:.0} % wide, {:.0} % narrow; {n} seconds over ten loiters, the boat alone in the picture)",
        100.0 * (in_wide + in_narrow) as f64 / n as f64,
        100.0 * in_wide as f64 / n as f64,
        100.0 * in_narrow as f64 / n as f64
    );
}

/// The same through the bridge as built (`Tracks::on_truth_with`, the physical model, the weapon
/// sensing and picture): 50 attacks, each on its own seed, HUNTER 1 1 km east of the boat and
/// REAPER 1 at 7,600 m loitering 3,500 m about its spawn, two minutes of the boat holding before
/// the launch. How many launches were seen, by whom, and what fixed the weapon.
#[test]
#[ignore = "a probe: prints the as-built measurement"]
fn weapon_pipeline_probe() {
    use super::super::truth::{ActTruth, ImpactTruth, TruthMessage, WeaponTruth};
    use super::super::{Settings, Tracks};
    let cat = catalog();
    let (hlat, hlon) = at(BOAT.0, BOAT.1, 1000.0, 0.0);
    let hunter = Observer {
        uid: 1,
        lat: hlat,
        lon: hlon,
        mast_m: cat.get("seahunter").expect("seahunter").mast_m,
        sensors: cat.get("seahunter").expect("seahunter").sensors.clone(),
        heading_deg: Some(0.0),
        ..Observer::default()
    };
    let boat = ContactTruth {
        id: "boat".into(),
        name: "boat".into(),
        affiliation: TruthAffiliation::Hostile,
        domain: Domain::Surface,
        kind: "small_fast_boat".into(),
        lat: BOAT.0,
        lon: BOAT.1,
        alt_m: 0.0,
        speed_mps: 0.0,
        course_deg: 0.0,
        emits: false,
        signature: 0.35,
        ais: None,
    };
    let t0 = 1_781_431_200.0;
    let (mut seen, mut by_reaper, mut any_fix, mut radar_fix, mut eo_fix) = (0, 0, 0, 0, 0);
    let mut first_fix_lead = Vec::new();
    let n = 50u64;
    for seed in 1..=n {
        let settings = Settings {
            seed: Some(seed),
            misclassification_rate: Some(0.0),
            eval_truth: false,
            drop_s: 180.0,
            model: Some(super::super::truth::SensingModel::Physical),
            sim_token: None,
        };
        let tracks = Tracks::new(catalog(), settings);
        let launch = 120.0 + (seed % 7) as f64;
        let w = WeaponTruth {
            id: "a:weapon".into(),
            act: "a".into(),
            by: "boat".into(),
            name: "ATGM-class missile".into(),
            kind: "atgm".into(),
            speed_mps: 250.0,
            launch_t: t0 + launch,
            impact_t: t0 + launch + 4.0,
            from: [BOAT.1, BOAT.0, 4.0],
            to: [hlon, hlat, 4.0],
            against_vehicle: Some(1),
            against_contact: None,
        };
        for s in 0..(launch as i64 + 6) {
            let s = s as f64;
            let a = 90.0 / 3500.0 * s + seed as f64;
            let (rlat, rlon) = at(REAPER.0, REAPER.1, 3500.0 * a.sin(), 3500.0 * a.cos());
            let reaper = Observer {
                uid: 2,
                lat: rlat,
                lon: rlon,
                alt_msl: 7600.0,
                sensors: cat.get("mq9b_armed").expect("mq9b").sensors.clone(),
                heading_deg: Some((a.to_degrees() + 90.0).rem_euclid(360.0)),
                ..Observer::default()
            };
            let mut m = TruthMessage {
                t: t0 + s,
                s,
                seq: s as u64,
                scenario: "probe".into(),
                seed,
                sensing: Sensing { conditions: Some(Conditions { sun_elevation_deg: Some(45.0), ..Conditions::default() }), ..Sensing::default() },
                contacts: vec![boat.clone()],
                acts: Vec::new(),
                aim_at: vec![1],
                weapons: Vec::new(),
                impacts: Vec::new(),
            };
            if s == launch {
                m.acts = vec![ActTruth {
                    id: "a".into(),
                    s,
                    t: t0 + s,
                    by: "boat".into(),
                    lat: BOAT.0,
                    lon: BOAT.1,
                    against_vehicle: Some(1),
                    against_contact: None,
                    label: None,
                    weapon: Some(w.id.clone()),
                    held: None,
                }];
            }
            if s >= launch && s <= launch + 4.0 {
                m.weapons = vec![w.clone()];
            }
            if s == launch + 4.0 {
                m.impacts = vec![ImpactTruth { weapon: w.id.clone(), act: "a".into(), t: w.impact_t, lat: hlat, lon: hlon, against_vehicle: Some(1), against_contact: None }];
            }
            tracks.on_truth_with(m, &[hunter.clone(), reaper], t0 + s).expect("ok");
        }
        let acts = tracks.take_acts();
        if let Some(r) = acts.first() {
            seen += 1;
            if r.sensors.iter().any(|s| s.starts_with("2:")) {
                by_reaper += 1;
            }
        }
        let inner = super::super::lock(&tracks.inner);
        let wt = &inner.weapons.tracks[0];
        if let Some(f) = wt.fixes.first() {
            any_fix += 1;
            first_fix_lead.push(((t0 + launch + 4.0 - f.t) * 10.0).round() / 10.0);
        }
        if wt.fixes.iter().any(|f| f.sensor.ends_with("radar")) {
            radar_fix += 1;
        }
        if wt.fixes.iter().any(|f| f.sensor.ends_with("eo_ir")) {
            eo_fix += 1;
        }
    }
    first_fix_lead.sort_by(f64::total_cmp);
    println!(
        "{n} attacks: launch seen {seen} (REAPER 1's camera among them {by_reaper}); weapon fixed in flight {any_fix} (radar {radar_fix}, camera {eo_fix}); first fix before the strike, s: {first_fix_lead:?}"
    );
}
