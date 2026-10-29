//! Opponent weapons through the bridge (docs/OPPONENT-WEAPONS.md): the tutorial's geometry, a
//! fast boat firing an ATGM-class missile at HUNTER 1 1 km east of it, by day, physical model.
//! The launch is the act's observation, by HUNTER 1's ring camera alone; the weapon and its
//! strike live in the weapon picture, never in the track picture; and the contacts' detections
//! are the same with a weapon in the truth as without.

use super::super::physical::conditions::Conditions;
use super::super::sensing::Observer;
use super::super::truth::{ActTruth, ContactTruth, Domain, ImpactTruth, Sensing, SensingModel, TruthAffiliation, TruthMessage, WeaponTruth};
use super::super::{Settings, Tracks};
use super::{at, catalog};

const T0: f64 = 1_781_431_200.0;
const BOAT: (f64, f64) = (37.8120, -122.4950);

fn settings() -> Settings {
    Settings {
        seed: Some(17),
        misclassification_rate: Some(0.0),
        eval_truth: true,
        drop_s: 180.0,
        model: Some(SensingModel::Physical),
        sim_token: Some("test-token".into()),
    }
}

fn boat() -> ContactTruth {
    ContactTruth {
        id: "fastboat-raider".into(),
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
    }
}

fn hunter() -> Observer {
    let (lat, lon) = at(BOAT.0, BOAT.1, 1_000.0, 0.0);
    let p = catalog().get("seahunter").expect("seahunter").clone();
    Observer { uid: 1, lat, lon, alt_msl: 0.0, mast_m: p.mast_m, sensors: p.sensors, heading_deg: Some(0.0), ..Observer::default() }
}

fn msg(s: f64) -> TruthMessage {
    TruthMessage {
        t: T0 + s,
        s,
        seq: s as u64,
        scenario: "weapons".into(),
        seed: 17,
        sensing: Sensing { conditions: Some(Conditions { sun_elevation_deg: Some(45.0), ..Conditions::default() }), ..Sensing::default() },
        contacts: vec![boat()],
        acts: Vec::new(),
        aim_at: vec![1],
        weapons: Vec::new(),
        impacts: Vec::new(),
    }
}

/// The flight from the boat to HUNTER 1, launched at second 20: 1 km at 250 m/s.
fn flight() -> WeaponTruth {
    let o = hunter();
    WeaponTruth {
        id: "act-1:weapon".into(),
        act: "act-1".into(),
        by: "fastboat-raider".into(),
        name: "ATGM-class missile".into(),
        kind: "atgm".into(),
        speed_mps: 250.0,
        launch_t: T0 + 20.0,
        impact_t: T0 + 24.0,
        from: [BOAT.1, BOAT.0, 4.0],
        to: [o.lon, o.lat, 4.0],
        against_vehicle: Some(1),
        against_contact: None,
    }
}

/// Run 30 s with the boat holding; with `armed` it fires at 20 s, its weapon in flight to 24.
fn run(armed: bool) -> (Tracks, Vec<std::collections::BTreeMap<String, murmur_decide::TrackFacts>>) {
    let tracks = Tracks::new(catalog(), settings());
    let o = [hunter()];
    let mut facts = Vec::new();
    for s in 0..30 {
        let s = f64::from(s);
        let mut m = msg(s);
        if armed {
            let f = flight();
            if s == 20.0 {
                m.acts = vec![ActTruth {
                    id: "act-1".into(),
                    s,
                    t: T0 + s,
                    by: "fastboat-raider".into(),
                    lat: BOAT.0,
                    lon: BOAT.1,
                    against_vehicle: Some(1),
                    against_contact: None,
                    label: None,
                    weapon: Some(f.id.clone()),
                    held: None,
                }];
            }
            if (20.0..=24.0).contains(&s) {
                m.weapons = vec![f.clone()];
            }
            if s == 24.0 {
                m.impacts = vec![ImpactTruth { weapon: f.id, act: f.act, t: f.impact_t, lat: f.to[1], lon: f.to[0], against_vehicle: Some(1), against_contact: None }];
            }
        }
        tracks.on_truth_with(m, &o, T0 + s).expect("ok");
        facts.push(tracks.facts(T0 + s));
    }
    (tracks, facts)
}

/// The launch is seen by HUNTER 1's ring camera, which alone observes the act (no radar, no
/// sonar); the weapon's track ends expended at the strike with the warning from the launch; the
/// strike is a medium alert (the launch was seen); and nothing of the weapon reaches the track
/// picture, which is the same as a run where the boat never fired.
#[test]
fn a_weapon_is_seen_launched_and_strikes_without_entering_the_track_picture() {
    let (tracks, facts) = run(true);
    let reports = tracks.take_acts();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(reports[0].sensors, vec!["1:eo_ir".to_string()], "the launch alone, by the camera");
    let off = murmur_decide::geo::haversine_m(reports[0].location, murmur_decide::LonLat::new(BOAT.1, BOAT.0));
    assert!(off < 4.0 * reports[0].sigma_m, "{off} m off, sigma {}", reports[0].sigma_m);
    let msg: serde_json::Value = serde_json::from_str(&tracks.message(T0 + 29.0)).expect("json");
    let w = &msg["weapons"][0];
    assert_eq!(w["id"], "W-1");
    assert_eq!(w["status"], "expended");
    assert_eq!(w["report"], "A-1");
    assert_eq!(w["struck"]["uid"], "1");
    assert_eq!(w["aimed_at"], "1");
    assert_eq!(w["class"], "ATGM-class missile");
    assert!((w["warning_s"].as_f64().expect("warning") - 4.0).abs() < 1e-6, "{w}");
    let alerts = tracks.strike_alerts(&|_| Some("HUNTER 1".into()));
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].kind, "struck");
    assert_eq!(alerts[0].severity, crate::decision::alerts::Severity::Medium);
    assert_eq!(std::mem::take(&mut super::super::lock(&tracks.inner).strikes).len(), 1, "the strike waits for its vehicle's events");
    // the track picture: the boat's track alone, second by second as if the boat never fired
    let (_, quiet) = run(false);
    assert_eq!(facts, quiet);
    assert!(facts.iter().all(|f| f.len() == 1));
}

/// A strike no sensor saw coming (the launch unseen: HUNTER 1 looking with nothing, here a
/// camera-less observer) is still in the picture and a high alert, and no act reaches the
/// engine (no shooter is placed).
#[test]
fn an_unseen_launch_is_known_at_its_strike() {
    let tracks = Tracks::new(catalog(), settings());
    let mut o = hunter();
    o.sensors.retain(|s| s.kind != super::super::sensing::SensorType::EoIr);
    let f = flight();
    for s in 18..26 {
        let s = f64::from(s);
        let mut m = msg(s);
        if s == 20.0 {
            m.acts = vec![ActTruth {
                id: "act-1".into(),
                s,
                t: T0 + s,
                by: "fastboat-raider".into(),
                lat: BOAT.0,
                lon: BOAT.1,
                against_vehicle: Some(1),
                against_contact: None,
                label: None,
                weapon: Some(f.id.clone()),
                held: None,
            }];
        }
        if (20.0..=24.0).contains(&s) {
            m.weapons = vec![f.clone()];
        }
        if s == 24.0 {
            m.impacts = vec![ImpactTruth { weapon: f.id.clone(), act: f.act.clone(), t: f.impact_t, lat: f.to[1], lon: f.to[0], against_vehicle: Some(1), against_contact: None }];
        }
        tracks.on_truth_with(m, std::slice::from_ref(&o), T0 + s).expect("ok");
    }
    assert!(tracks.take_acts().is_empty(), "nobody saw the launch: no act report");
    let alerts = tracks.strike_alerts(&|_| None);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].severity, crate::decision::alerts::Severity::High);
    assert!(alerts[0].detail.contains("no sensor saw the launch"), "{}", alerts[0].detail);
}
