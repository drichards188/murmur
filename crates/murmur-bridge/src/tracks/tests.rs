//! The sensing model and the tracker against synthetic truth: detection against range,
//! confidence growth with dwell and sensors, uncertainty growth with age, AIS identity and
//! spoofing, sonar-only subsurface, determinism, and that truth never leaks into anything
//! the console or an agent reads.

use super::model::{STALE_S, Track};
use super::sensing::{
    Detection, Observer, Params, SensorCatalog, SensorSpec, SensorType, detect, horizon_m, pd,
};
use super::truth::{Ais, ContactTruth, Domain, Sensing, SensingModel, TruthAffiliation, TruthMessage};
use super::{Settings, Tracks};
use murmur_decide::Affiliation;

const T0: f64 = 1_000_000.0;

#[allow(dead_code)]
mod before_oho;
mod calib;
mod calib_air;
mod catalog;
mod physical;

fn catalog() -> SensorCatalog {
    SensorCatalog::from_toml(include_str!("../../../../sim/platforms.toml")).expect("catalog")
}

/// These tests are the legacy model's (the physical model's are `physical`): pinned to it, as
/// `MURMUR_SENSE_MODEL=legacy` pins a v1 or v2 episode.
fn settings() -> Settings {
    Settings {
        seed: Some(42),
        misclassification_rate: Some(0.0),
        eval_truth: false,
        drop_s: 180.0,
        model: Some(SensingModel::Legacy),
        sim_token: Some("test-token".into()),
    }
}

fn contact(id: &str, kind: &str, domain: Domain, lat: f64, lon: f64) -> ContactTruth {
    let (signature, emits, alt_m) = match kind {
        "cargo_ship" => (1.6, true, 0.0),
        "ferry" => (1.2, true, 0.0),
        "fishing_vessel" => (0.7, true, 0.0),
        "small_fast_boat" => (0.35, false, 0.0),
        "submarine" => (0.3, false, -40.0),
        "unknown_uas" => (0.15, false, 120.0),
        "light_aircraft" => (0.6, true, 400.0),
        _ => (1.0, false, 0.0),
    };
    ContactTruth {
        id: id.into(),
        name: id.into(),
        affiliation: TruthAffiliation::Neutral,
        domain,
        kind: kind.into(),
        lat,
        lon,
        alt_m,
        speed_mps: 5.0,
        course_deg: 90.0,
        emits,
        signature,
        ais: None,
    }
}

fn observer(uid: u64, lat: f64, lon: f64, alt: f64, sensors: &[(SensorType, f64)]) -> Observer {
    Observer {
        uid,
        lat,
        lon,
        alt_msl: alt,
        mast_m: 4.0,
        submerged: false,
        heading_deg: None,
        area: None,
        sensors: sensors
            .iter()
            .map(|(kind, range_m)| SensorSpec {
                kind: *kind,
                range_m: *range_m,
                guess: true,
                height: false,
                physical: Default::default(),
            })
            .collect(),
    }
}

fn truth(s: f64, contacts: Vec<ContactTruth>) -> TruthMessage {
    TruthMessage {
        t: T0 + s,
        s,
        seq: s as u64,
        scenario: "test".into(),
        seed: 42,
        sensing: Sensing {
            misclassification_rate: 0.0,
            ..Default::default()
        },
        contacts,
        acts: Vec::new(),
    }
}

/// Offset a point by metres east and north.
fn at(lat: f64, lon: f64, east_m: f64, north_m: f64) -> (f64, f64) {
    (
        lat + north_m / 111_320.0,
        lon + east_m / (111_320.0 * lat.to_radians().cos()),
    )
}

const BASE: (f64, f64) = (37.82, -122.48);

#[test]
fn catalog_lists_sensors_for_every_platform_with_sonar_on_subs() {
    let c = catalog();
    assert!(c.platforms.len() >= 20);
    for sub in ["remus", "orca", "bluerov"] {
        let p = c.get(sub).expect(sub);
        assert!(p.sensors.iter().all(|s| s.kind == SensorType::Sonar), "{sub}");
    }
    assert!(c.get("tracker").expect("tracker").sensors.is_empty());
    assert_eq!(c.get("seahunter").expect("seahunter").mast_m, 10.0);
    assert!(c.get("rq4").expect("rq4").sensors.iter().any(|s| s.kind == SensorType::Radar));
}

#[test]
fn detection_probability_falls_with_range_and_ends() {
    assert!((pd(0.0, 0.95) - 0.95).abs() < 1e-9);
    assert!((pd(1.0, 0.95) - 0.475).abs() < 1e-9);
    assert!(pd(0.5, 0.95) > 0.9);
    assert!(pd(1.3, 0.95) < 0.2);
    assert_eq!(pd(1.51, 0.95), 0.0);
    // over many ticks, a radar sees a nominal contact nearly always at half range, about
    // half the time at nominal range and never at twice the range
    let mut hits = [0u32; 3];
    let sensors = [(SensorType::Radar, 10_000.0)];
    for tick in 0..400 {
        let p = Params {
            seed: 1,
            tick,
            misclassification_rate: 0.0,
        };
        let o = observer(1, BASE.0, BASE.1, 60.0, &sensors);
        for (i, r) in [5_000.0, 10_000.0, 20_000.0].into_iter().enumerate() {
            let (lat, lon) = at(BASE.0, BASE.1, r, 0.0);
            let mut c = contact("c", "boat", Domain::Surface, lat, lon);
            c.signature = 1.0;
            if !detect(p, std::slice::from_ref(&o), &[c], &|_, _| false).is_empty() {
                hits[i] += 1;
            }
        }
    }
    assert!(hits[0] > 340, "{hits:?}");
    assert!((150..=240).contains(&hits[1]), "{hits:?}");
    assert_eq!(hits[2], 0, "{hits:?}");
}

#[test]
fn radio_horizon_limits_surface_sensors_but_not_sonar() {
    // a 4 m mast against a 2 m boat: horizon about 14 km, well inside a 40 km radar
    assert!((horizon_m(4.0, 2.0) - 14_070.0).abs() < 50.0);
    let sensors = [(SensorType::Radar, 40_000.0), (SensorType::Sonar, 40_000.0)];
    let o = observer(1, BASE.0, BASE.1, 0.0, &sensors);
    let (lat, lon) = at(BASE.0, BASE.1, 18_000.0, 0.0);
    let boat = contact("boat", "small_fast_boat", Domain::Surface, lat, lon);
    let mut seen = std::collections::BTreeSet::new();
    for tick in 0..100 {
        let p = Params {
            seed: 3,
            tick,
            misclassification_rate: 0.0,
        };
        for d in detect(p, std::slice::from_ref(&o), std::slice::from_ref(&boat), &|_, _| false) {
            seen.insert(d.sensor);
        }
    }
    assert!(!seen.contains(&SensorType::Radar), "beyond the horizon");
    assert!(seen.contains(&SensorType::Sonar), "sonar has no horizon");
    // the same radar from 7,600 m up sees it
    let high = observer(2, BASE.0, BASE.1, 7_600.0, &[(SensorType::Radar, 40_000.0)]);
    let any = (0..50).any(|tick| {
        let p = Params {
            seed: 3,
            tick,
            misclassification_rate: 0.0,
        };
        !detect(p, std::slice::from_ref(&high), std::slice::from_ref(&boat), &|_, _| false).is_empty()
    });
    assert!(any);
}

#[test]
fn subsurface_contacts_are_sonar_only_and_submerged_vehicles_only_hear() {
    let (lat, lon) = at(BASE.0, BASE.1, 800.0, 0.0);
    let sub = contact("sub", "submarine", Domain::Subsurface, lat, lon);
    let all = [
        (SensorType::Radar, 50_000.0),
        (SensorType::EoIr, 50_000.0),
        (SensorType::Ais, 50_000.0),
        (SensorType::Esm, 50_000.0),
        (SensorType::Sonar, 5_000.0),
    ];
    let o = observer(1, BASE.0, BASE.1, 0.0, &all);
    let mut sensors = std::collections::BTreeSet::new();
    for tick in 0..60 {
        let p = Params {
            seed: 9,
            tick,
            misclassification_rate: 0.0,
        };
        for d in detect(p, std::slice::from_ref(&o), std::slice::from_ref(&sub), &|_, _| false) {
            sensors.insert(d.sensor);
        }
    }
    assert_eq!(sensors.into_iter().collect::<Vec<_>>(), vec![SensorType::Sonar]);
    // our own submerged AUV: its (hypothetical) radar is useless under water
    let mut auv = observer(2, BASE.0, BASE.1, -30.0, &[(SensorType::Radar, 50_000.0)]);
    auv.submerged = true;
    let boat = contact("b", "ferry", Domain::Surface, lat, lon);
    let p = Params {
        seed: 9,
        tick: 0,
        misclassification_rate: 0.0,
    };
    assert!(detect(p, &[auv], &[boat], &|_, _| false).is_empty());
}

#[test]
fn ais_gives_identity_and_only_for_cooperative_ships() {
    let (lat, lon) = at(BASE.0, BASE.1, 3_000.0, 0.0);
    let mut ferry = contact("ferry", "ferry", Domain::Surface, lat, lon);
    ferry.ais = Some(Ais {
        mmsi: "366000001".into(),
        name: "MV TEST".into(),
        ship_type: "passenger".into(),
        spoofed: false,
    });
    let dark = contact("dark", "small_fast_boat", Domain::Surface, lat, lon);
    let o = observer(1, BASE.0, BASE.1, 0.0, &[(SensorType::Ais, 30_000.0)]);
    let p = Params {
        seed: 5,
        tick: 0,
        misclassification_rate: 0.0,
    };
    let ds = detect(p, &[o], &[ferry, dark], &|_, _| false);
    assert_eq!(ds.len(), 1);
    assert_eq!(ds[0].contact_id, "ferry");
    let heard = ds[0].ais.as_ref().expect("ais heard");
    assert_eq!(heard.name, "MV TEST");
    // the AIS receiver gives an identity at once and a neutral affiliation
    let t = Track::new("T-0001".into(), 1, &ds[0], T0);
    let id = t.identity();
    assert_eq!(id.kind.as_deref(), Some("ferry"));
    assert!(id.confidence > 0.8);
    let (aff, _, why) = t.affiliation();
    assert_eq!(aff, Affiliation::Neutral);
    assert!(why.contains("AIS"), "{why}");
    // AIS carries course and speed over ground: the track has them at once (5 m/s east)
    let (e, n) = t.vel.expect("velocity from AIS");
    assert!((e - 5.0).abs() < 0.8 && n.abs() < 0.8, "{e} {n}");
    // rules 16: the decision layer is given the AIS identity it broadcasts
    let ais = t.ais_identity().expect("ais for the decision layer");
    assert_eq!((ais.mmsi.as_str(), ais.name.as_str()), (heard.mmsi.as_str(), "MV TEST"));
}

#[test]
fn spoofed_ais_becomes_suspect_once_eo_ir_sees_it() {
    let (lat, lon) = at(BASE.0, BASE.1, 1_500.0, 0.0);
    let mut boat = contact("fast", "small_fast_boat", Domain::Surface, lat, lon);
    boat.ais = Some(Ais {
        mmsi: "366000002".into(),
        name: "F/V PACIFIC DAWN".into(),
        ship_type: "fishing".into(),
        spoofed: true,
    });
    let o = observer(
        1,
        BASE.0,
        BASE.1,
        0.0,
        &[(SensorType::Ais, 30_000.0), (SensorType::EoIr, 5_000.0)],
    );
    let tracks = Tracks::new(catalog(), settings());
    let mut aff_at = Vec::new();
    for s in 0..90 {
        tracks
            .on_truth_with(truth(f64::from(s), vec![boat.clone()]), std::slice::from_ref(&o), T0 + f64::from(s))
            .expect("ok");
        let t = tracks.track("T-0001").expect("track");
        aff_at.push(t.affiliation().0);
    }
    // the AIS identity is believed at first (neutral fishing vessel) and contradicted as
    // the EO/IR classification matures
    assert_eq!(aff_at[0], Affiliation::Neutral);
    assert_eq!(*aff_at.last().expect("some"), Affiliation::Suspect);
    let t = tracks.track("T-0001").expect("track");
    let id = t.identity();
    assert_eq!(id.kind.as_deref(), Some("small_fast_boat"));
    assert!(id.conflict.as_deref().is_some_and(|c| c.contains("AIS says fishing")), "{:?}", id.conflict);
    // the public JSON never says the AIS was spoofed, nor names the contact
    let text = serde_json::to_string(&t.json(T0 + 100.0)).expect("json");
    assert!(!text.contains("spoofed"));
    assert!(!text.contains("\"fast\""));
}

#[test]
fn identity_confidence_grows_with_dwell_and_independent_sensors() {
    let (lat, lon) = at(BASE.0, BASE.1, 1_000.0, 0.0);
    let boat = contact("f", "fishing_vessel", Domain::Surface, lat, lon);
    let one = observer(1, BASE.0, BASE.1, 0.0, &[(SensorType::EoIr, 5_000.0)]);
    let two = observer(2, BASE.0, BASE.1, 300.0, &[(SensorType::Radar, 20_000.0)]);
    let run = |observers: &[Observer], secs: i32| {
        let tracks = Tracks::new(catalog(), settings());
        let mut conf = Vec::new();
        for s in 0..secs {
            tracks
                .on_truth_with(truth(f64::from(s), vec![boat.clone()]), observers, T0 + f64::from(s))
                .expect("ok");
            conf.push(tracks.track("T-0001").expect("track").identity().confidence);
        }
        conf
    };
    let single = run(std::slice::from_ref(&one), 120);
    assert!(single[5] < single[30] && single[30] < single[119], "{single:?}");
    assert!(single[119] > 0.8, "{}", single[119]);
    let double = run(&[one, two], 120);
    assert!(double[119] > single[119], "two sensors beat one: {} vs {}", double[119], single[119]);
    let tracks = Tracks::new(catalog(), settings());
    tracks
        .on_truth_with(truth(0.0, vec![boat]), &[], T0)
        .expect("ok");
    assert!(tracks.tracks_json(T0).is_empty(), "no sensors, no tracks");
}

#[test]
fn uncertainty_and_staleness_grow_with_age_and_tracks_drop() {
    let (lat, lon) = at(BASE.0, BASE.1, 2_000.0, 0.0);
    let ferry = contact("ferry", "ferry", Domain::Surface, lat, lon);
    let o = observer(1, BASE.0, BASE.1, 0.0, &[(SensorType::Radar, 20_000.0)]);
    let tracks = Tracks::new(catalog(), settings());
    for s in 0..20 {
        tracks
            .on_truth_with(truth(f64::from(s), vec![ferry.clone()]), std::slice::from_ref(&o), T0 + f64::from(s))
            .expect("ok");
    }
    let t = tracks.track("T-0001").expect("track");
    let last = t.last_seen_t;
    assert!(t.sigma_at(last) < 60.0);
    assert!(t.sigma_at(last + 60.0) > t.sigma_at(last) + 50.0);
    assert!(!t.is_stale(last + STALE_S - 1.0));
    assert!(t.is_stale(last + STALE_S + 1.0));
    assert!(t.confidence_at(last + 120.0) < 0.5 * t.confidence_at(last));
    // the symbol goes anticipated (dashed) when stale
    assert_eq!(t.sidc(last).chars().nth(3), Some('P'));
    assert_eq!(t.sidc(last + 30.0).chars().nth(3), Some('A'));
    let facts = tracks.facts(last + 30.0);
    assert!(facts["T-0001"].pos_sigma_m > facts["T-0001"].pos_sigma_m - 1.0);
    assert_eq!(facts["T-0001"].sources, vec!["1:radar"]);
    // silence past the drop limit removes it; seen again, a new track id
    tracks
        .on_truth_with(truth(400.0, vec![]), std::slice::from_ref(&o), T0 + 400.0)
        .expect("ok");
    assert!(tracks.tracks_json(T0 + 400.0).is_empty());
    // rules 16: the decision facts keep it, as it was at the drop, until the engine takes it
    let facts = tracks.facts(T0 + 400.0);
    assert!(facts.contains_key("T-0001"), "{facts:?}");
    let drops = tracks.take_drops();
    assert_eq!(drops.len(), 1);
    assert_eq!(drops[0].0, "T-0001");
    assert_eq!(drops[0].1.last_seen_t, facts["T-0001"].last_seen_t);
    assert!(drops[0].1.declaration.is_none() && drops[0].1.declared_by.is_none());
    assert!(tracks.facts(T0 + 400.0).is_empty() && tracks.take_drops().is_empty());
    // one the engine could not log goes back, whole, and stays in the decision facts
    tracks.restore_drops(drops.clone());
    assert!(tracks.facts(T0 + 400.0).contains_key("T-0001"));
    let again = tracks.take_drops();
    assert_eq!((again.len(), again[0].1.last_seen_t), (1, drops[0].1.last_seen_t));
    tracks
        .on_truth_with(truth(401.0, vec![ferry]), &[o], T0 + 401.0)
        .expect("ok");
    assert!(tracks.track("T-0002").is_some());
}

#[test]
fn deterministic_for_a_seed_and_different_across_seeds() {
    let (lat, lon) = at(BASE.0, BASE.1, 9_000.0, 2_000.0);
    let boat = contact("b", "fishing_vessel", Domain::Surface, lat, lon);
    let o = observer(1, BASE.0, BASE.1, 500.0, &[(SensorType::Radar, 10_000.0)]);
    let run = |seed: u64| -> Vec<Detection> {
        (0..200)
            .flat_map(|tick| {
                detect(
                    Params {
                        seed,
                        tick,
                        misclassification_rate: 0.0,
                    },
                    std::slice::from_ref(&o),
                    std::slice::from_ref(&boat),
                    &|_, _| false,
                )
            })
            .collect()
    };
    let a = run(7);
    let b = run(7);
    let c = run(8);
    assert_eq!(a, b);
    assert!(!a.is_empty());
    assert_ne!(a, c);
    // the bridge-side seed override wins over the scenario's
    let s1 = Tracks::new(catalog(), settings());
    let s2 = Tracks::new(catalog(), settings());
    for s in 0..30 {
        let mut m = truth(f64::from(s), vec![boat.clone()]);
        m.seed = 99;
        s1.on_truth_with(m.clone(), std::slice::from_ref(&o), T0 + f64::from(s)).expect("ok");
        m.seed = 100;
        s2.on_truth_with(m, std::slice::from_ref(&o), T0 + f64::from(s)).expect("ok");
    }
    assert_eq!(s1.tracks_json(T0 + 30.0), s2.tracks_json(T0 + 30.0));
}

#[test]
fn misclassification_is_injectable_and_persistent() {
    let (lat, lon) = at(BASE.0, BASE.1, 1_000.0, 0.0);
    let boat = contact("fv", "fishing_vessel", Domain::Surface, lat, lon);
    let o = observer(1, BASE.0, BASE.1, 0.0, &[(SensorType::EoIr, 5_000.0)]);
    let mut st = settings();
    st.misclassification_rate = Some(1.0);
    let tracks = Tracks::new(catalog(), st);
    for s in 0..60 {
        tracks
            .on_truth_with(truth(f64::from(s), vec![boat.clone()]), std::slice::from_ref(&o), T0 + f64::from(s))
            .expect("ok");
    }
    let t = tracks.track("T-0001").expect("track");
    assert_eq!(t.identity().kind.as_deref(), Some("small_fast_boat"));
    assert_eq!(t.affiliation().0, Affiliation::Suspect);
    // at rate 0 the call is right
    let tracks = Tracks::new(catalog(), settings());
    for s in 0..60 {
        tracks
            .on_truth_with(truth(f64::from(s), vec![boat.clone()]), std::slice::from_ref(&o), T0 + f64::from(s))
            .expect("ok");
    }
    assert_eq!(
        tracks.track("T-0001").expect("track").identity().kind.as_deref(),
        Some("fishing_vessel")
    );
}

#[test]
fn operator_declaration_overrides_the_estimate() {
    let (lat, lon) = at(BASE.0, BASE.1, 1_000.0, 0.0);
    let boat = contact("x", "small_fast_boat", Domain::Surface, lat, lon);
    let o = observer(1, BASE.0, BASE.1, 0.0, &[(SensorType::EoIr, 5_000.0)]);
    let tracks = Tracks::new(catalog(), settings());
    for s in 0..5 {
        tracks
            .on_truth_with(truth(f64::from(s), vec![boat.clone()]), std::slice::from_ref(&o), T0 + f64::from(s))
            .expect("ok");
    }
    // rules 9: the declaration is the decision engine's ledger; the store mirrors it
    let mut ledger = murmur_decide::ident::Ledger::default();
    ledger.step(
        murmur_decide::Time((T0 * 1000.0) as i64 + 5000),
        &declared(1, "T-0001", murmur_decide::Declarer::Operator {
            operator: murmur_decide::OperatorId::new("op-1"),
            session: murmur_decide::SessionId(1),
            note: Some("fired on the cutter".into()),
            flag: None,
            step_up: None,
        }),
    );
    tracks.mirror(&ledger);
    let v = tracks.tracks_json(T0 + 5.0).into_iter().find(|t| t["id"] == "T-0001").expect("track");
    assert_eq!(v["affiliation"], "hostile");
    assert_eq!(v["declared"]["by"], "op-1");
    assert_eq!(v["declared"]["note"], "fired on the cutter");
    assert!(v["sidc"].as_str().expect("sidc").starts_with("SHSP"));
    // the decision facts are the estimate: the engine fills the declaration from its ledger
    let facts = tracks.facts(T0 + 6.0);
    assert_ne!(facts["T-0001"].affiliation.value, Affiliation::Hostile);
    assert_eq!(facts["T-0001"].declared_by, None);
    // a rule declaration shows its basis, and that nobody confirmed it
    let evidence = murmur_decide::RuleEvidence {
        act: "A-1".into(),
        against: murmur_decide::Against::Vehicle { uid: murmur_decide::Uid::new("1") },
        observed_t: murmur_decide::Time((T0 * 1000.0) as i64 + 5000),
        sensors: vec!["1:eo_ir".into()],
        attribution: 0.97,
        threshold: 0.9,
        corroborated_by: None,
    };
    let mut ledger = murmur_decide::ident::Ledger::default();
    ledger.step(
        murmur_decide::Time((T0 * 1000.0) as i64 + 5000),
        &declared(1, "T-0001", murmur_decide::Declarer::Rule { rule: "hostile_act".into(), evidence }),
    );
    tracks.mirror(&ledger);
    let v = tracks.tracks_json(T0 + 5.0).into_iter().find(|t| t["id"] == "T-0001").expect("track");
    assert_eq!(v["declared"]["by"], "rule:hostile_act");
    assert_eq!(v["declared"]["rule"], true);
    assert!(v["declared"]["confirmed_by"].is_null());
    assert!(v["affiliation_reason"].as_str().expect("why").contains("not confirmed by an operator"), "{v}");
}

/// An `ident.declared` event body for a ledger (rules 9).
fn declared(id: u64, track: &str, by: murmur_decide::Declarer) -> murmur_decide::EventBody {
    murmur_decide::EventBody::IdentDeclared {
        declaration: Box::new(murmur_decide::Declaration {
            id,
            track: track.into(),
            affiliation: Affiliation::Hostile,
            t: murmur_decide::Time((T0 * 1000.0) as i64 + 5000),
            by,
            confirmed: None,
        }),
    }
}

#[test]
fn a_hostile_act_is_observed_by_the_sensors_that_could_see_its_author() {
    let (lat, lon) = at(BASE.0, BASE.1, 1_000.0, 0.0);
    let boat = contact("raider", "small_fast_boat", Domain::Surface, lat, lon);
    let (flat, flon) = at(BASE.0, BASE.1, 30_000.0, 0.0);
    let far = contact("far", "small_fast_boat", Domain::Surface, flat, flon);
    let o = observer(1, BASE.0, BASE.1, 0.0, &[(SensorType::EoIr, 5_000.0), (SensorType::Ais, 30_000.0)]);
    let tracks = Tracks::new(catalog(), settings());
    for s in 0..3 {
        tracks
            .on_truth_with(truth(f64::from(s), vec![boat.clone(), far.clone()]), std::slice::from_ref(&o), T0 + f64::from(s))
            .expect("ok");
    }
    let act = |id: &str, by: &str, la: f64, lo: f64| crate::tracks::truth::ActTruth {
        id: id.into(),
        s: 3.0,
        t: T0 + 3.0,
        by: by.into(),
        lat: la,
        lon: lo,
        against_vehicle: Some(1),
        against_contact: None,
        label: None,
    };
    let mut msg = truth(3.0, vec![boat.clone(), far.clone()]);
    msg.acts = vec![act("act-1", "raider", lat, lon), act("act-2", "far", flat, flon)];
    tracks.on_truth_with(msg, std::slice::from_ref(&o), T0 + 3.0).expect("ok");
    // the near one: seen by the EO/IR (not the AIS receiver), its author placed near it
    let reports = tracks.take_acts();
    assert_eq!(reports.len(), 1, "{reports:?}");
    let r = &reports[0];
    assert_eq!(r.act, "A-1");
    assert_eq!(r.sensors, vec!["1:eo_ir".to_string()]);
    assert_eq!(r.against, murmur_decide::Against::Vehicle { uid: murmur_decide::Uid::new("1") });
    let off = murmur_decide::geo::haversine_m(r.location, murmur_decide::LonLat::new(lon, lat));
    assert!(off < 5.0 * r.sigma_m, "{off} m off, sigma {}", r.sigma_m);
    assert!(tracks.take_acts().is_empty(), "each once");
    // the far one: nobody saw it; only the evaluation view knows it happened
    let ev = tracks.eval_json(T0 + 4.0);
    let acts = ev["acts"].as_array().expect("acts");
    assert_eq!(acts.len(), 2);
    assert_eq!(acts[0]["observed"], true);
    assert_eq!(acts[0]["report"], "A-1");
    assert_eq!(acts[1]["act"], "act-2");
    assert_eq!(acts[1]["observed"], false);
    // and the same act sent again is sensed once
    let mut again = truth(4.0, vec![boat, far]);
    again.acts = vec![act("act-1", "raider", lat, lon)];
    tracks.on_truth_with(again, std::slice::from_ref(&o), T0 + 4.0).expect("ok");
    assert!(tracks.take_acts().is_empty());
}

/// Rules 9's act sensing for an aircraft (the review of 10-07): a 2D radar measures its slant
/// range and bearing, not its height, so alone it places the author as the tracker reads such a
/// fix, at the middle of the bearing segment with a third of its length for its error, not at
/// its true ground position; an EO/IR fix, smaller, places it when there is one.
#[test]
fn an_act_by_an_aircraft_only_a_2d_radar_sees_is_placed_on_its_bearing_segment() {
    let (lat, lon) = at(BASE.0, BASE.1, 0.0, 3_000.0);
    let mut plane = contact("raider", "light_aircraft", Domain::Air, lat, lon);
    plane.alt_m = 1_500.0;
    let radar = observer(1, BASE.0, BASE.1, 0.0, &[(SensorType::Radar, 40_000.0)]);
    let both = observer(1, BASE.0, BASE.1, 0.0, &[(SensorType::Radar, 40_000.0), (SensorType::EoIr, 15_000.0)]);
    let observed = |o: &Observer| {
        (0..40)
            .find_map(|tick| super::sensing::observe_act(Params { seed: 3, tick, misclassification_rate: 0.0 }, std::slice::from_ref(o), &plane, "act-1"))
            .expect("observed within 40 draws")
    };
    let a = observed(&radar);
    let slant = 3_000f64.hypot(1_500.0);
    let from_radar = murmur_decide::geo::haversine_m(murmur_decide::LonLat::new(BASE.1, BASE.0), murmur_decide::LonLat::new(a.lon, a.lat));
    // the segment's middle (its length the slant range and two of the radar's sigmas), a third
    // of its length for its error: two thirds of the distance out, not the radar's 60 m
    assert!((0.4 * slant..0.6 * slant).contains(&from_radar), "the segment's middle: {from_radar} m out of {slant}");
    assert!((a.sigma_m - 2.0 * from_radar / 3.0).abs() < 3.0, "sigma {}, {from_radar} m out", a.sigma_m);
    let off = murmur_decide::geo::haversine_m(murmur_decide::LonLat::new(lon, lat), murmur_decide::LonLat::new(a.lon, a.lat));
    assert!(off <= 1.5 * a.sigma_m + 100.0, "the truth on the segment, {off} m off, sigma {}", a.sigma_m);
    let b = observed(&both);
    assert!(b.sigma_m < 100.0 && b.sensors.iter().any(|s| s == "1:eo_ir"), "{b:?}");
}

#[test]
fn truth_never_reaches_the_public_outputs() {
    let (lat, lon) = at(BASE.0, BASE.1, 1_500.0, 0.0);
    let mut boat = contact("secret-contact-id", "small_fast_boat", Domain::Surface, lat, lon);
    boat.name = "Truth-only name".into();
    boat.affiliation = TruthAffiliation::Hostile;
    boat.ais = Some(Ais {
        mmsi: "1".into(),
        name: "F/V COVER".into(),
        ship_type: "fishing".into(),
        spoofed: true,
    });
    let o = observer(
        1,
        BASE.0,
        BASE.1,
        0.0,
        &[(SensorType::Ais, 30_000.0), (SensorType::EoIr, 5_000.0)],
    );
    let tracks = Tracks::new(catalog(), settings());
    for s in 0..30 {
        tracks
            .on_truth_with(truth(f64::from(s), vec![boat.clone()]), std::slice::from_ref(&o), T0 + f64::from(s))
            .expect("ok");
    }
    let now = T0 + 30.0;
    let public = [
        serde_json::to_string(&tracks.api_json(now)).expect("json"),
        tracks.message(now),
        serde_json::to_string(&tracks.facts(now)).expect("json"),
        tracks
            .cot_samples(now)
            .iter()
            .map(|t| crate::cot::build_track_event(&crate::cot::CotConfig::default(), t, now, 1.0))
            .collect::<Vec<_>>()
            .join(""),
    ];
    for text in &public {
        assert!(!text.contains("secret-contact-id"), "{text}");
        assert!(!text.contains("Truth-only name"), "{text}");
        assert!(!text.contains("spoofed"), "{text}");
        assert!(!text.contains("\"hostile\""), "estimated, never truth: {text}");
    }
    // the AIS broadcast itself is legitimately heard
    assert!(public[0].contains("F/V COVER"));
    // the evaluation hook is the one place truth appears, and it is off by default
    assert!(!tracks.eval_truth_enabled());
    let eval = serde_json::to_string(&tracks.eval_json(now)).expect("json");
    assert!(eval.contains("secret-contact-id") && eval.contains("\"association\""));
}

#[test]
fn cot_track_types_follow_the_estimated_affiliation() {
    use crate::cot::track_type;
    assert_eq!(track_type("SHSPXR---------"), "a-h-S-X-R");
    assert_eq!(track_type("SSSPXF---------"), "a-s-S-X-F");
    assert_eq!(track_type("SNSPXMP--------"), "a-n-S-X-M-P");
    assert_eq!(track_type("SUAPMFQ--------"), "a-u-A-M-F-Q");
    assert_eq!(track_type("SAUPS----------"), "a-a-U-S");
    assert_eq!(track_type("SUSA-----------"), "a-u-S");
    assert_eq!(track_type("??U"), "a-u-U");
    let (lat, lon) = at(BASE.0, BASE.1, 800.0, 0.0);
    let sub = contact("s", "submarine", Domain::Subsurface, lat, lon);
    let o = observer(1, BASE.0, BASE.1, 0.0, &[(SensorType::Sonar, 5_000.0)]);
    let tracks = Tracks::new(catalog(), settings());
    // sonar classifies slowly (tau 120 s): a few minutes of contact before it is confident
    for s in 0..240 {
        tracks
            .on_truth_with(truth(f64::from(s), vec![sub.clone()]), std::slice::from_ref(&o), T0 + f64::from(s))
            .expect("ok");
    }
    let samples = tracks.cot_samples(T0 + 240.0);
    assert_eq!(samples.len(), 1);
    let xml = crate::cot::build_track_event(&crate::cot::CotConfig::default(), &samples[0], T0 + 240.0, 1.0);
    assert!(xml.contains("type=\"a-s-U-S\""), "{xml}");
    assert!(xml.contains("how=\"m-f\""));
    assert!(xml.contains("uid=\"murmur-main-track-1\""));
    // a contact that doesn't move must not read as moving, sonar noise notwithstanding
    assert!(samples[0].speed < 2.0, "{}", samples[0].speed);
    let stale = crate::cot::build_track_event(
        &crate::cot::CotConfig::default(),
        &tracks.cot_samples(T0 + 300.0)[0],
        T0 + 300.0,
        1.0,
    );
    assert!(stale.contains("how=\"m-f\"") && stale.contains("predicted=\"1\""));
    assert!(!xml.contains("predicted="));
}

/// Review, fix 1: the simulation channel's token, compared as digests; no token configured,
/// the channel is closed.
#[test]
fn the_simulation_channel_takes_only_its_stacks_token() {
    use axum::http::HeaderMap;
    let tracks = Tracks::new(catalog(), settings());
    let with = |v: &str| {
        let mut h = HeaderMap::new();
        h.insert(crate::tracks::SIM_TOKEN_HEADER, v.parse().expect("header"));
        h
    };
    assert!(tracks.sim_token_ok(&with("test-token")).is_ok());
    assert!(tracks.sim_token_ok(&with("test-tokeN")).is_err());
    assert!(tracks.sim_token_ok(&HeaderMap::new()).is_err());
    let closed = Tracks::new(catalog(), Settings { sim_token: None, ..settings() });
    assert!(closed.sim_token_ok(&with("test-token")).is_err());
}

/// Review B2: only vehicles heard within their link's stale age are sensors. A link's last
/// packet is on the wall clock; compared with the monotonic clock every vehicle ever heard
/// counted as live, and in a comms cut a dead-link vehicle kept detecting from its last
/// position.
#[test]
fn a_vehicle_silent_past_its_stale_age_senses_nothing() {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use crate::link::{LinkStats, Transmit};
    use crate::vehicle::{Identity, VehicleCell};
    use crate::world::{Config, World};

    struct NoTx;
    impl Transmit for NoTx {
        fn send(&self, _: &crate::mav::MavMessage) {}
        fn send_raw(&self, _: &[u8]) {}
        fn port(&self) -> u16 {
            0
        }
    }
    let world = World::new(Config {
        base_port: 0,
        http_port: 0,
        default_ports: 0,
        manifest: PathBuf::from("/nonexistent/fleet.json"),
        platforms: PathBuf::from("/nonexistent/platforms.toml"),
        root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
    });
    let wall = crate::clock::wall();
    let mono = crate::clock::mono();
    // uid 1 heard just now, uid 2 last heard a minute ago (a corsair: radar, EO/IR, AIS)
    for (uid, heard) in [(1u64, wall), (2, wall - 60.0)] {
        let mut stats = LinkStats::default();
        stats.on_packet(0, mono, heard);
        let mut m = serde_json::Map::new();
        m.insert("platform".into(), serde_json::json!("corsair"));
        m.insert("callsign".into(), serde_json::json!(format!("MARLIN {uid}")));
        let cell = Arc::new(VehicleCell::new(
            uid,
            uid as u8,
            Arc::new(NoTx),
            Arc::new(Mutex::new(stats)),
            Some(Arc::new(Identity::from_manifest(&m))),
        ));
        cell.write(|v| v.pos = Some((37.82, -122.466)));
        world.insert_vehicle(cell);
    }
    let tracks = Tracks::new(catalog(), settings());
    let uids: Vec<_> = tracks.observers(&world).iter().map(|o| o.uid).collect();
    assert_eq!(uids, vec![1], "the silent vehicle is no sensor");
}

// The tracker's uncertainty against truth, and a keep-out's hold a minute ahead, old filter
// against new: `tests/calib.rs`.

/// Rules 16: the tracker's stale age is the engine's out-of-view age (a track the console fades
/// is the one a keep-out counts at its domain's top speed), and the explanation's default.
#[test]
fn stale_is_out_of_view() {
    assert_eq!(super::model::STALE_S, murmur_decide::engage::OUT_OF_VIEW_S);
    assert_eq!(murmur_decide::keepout::TrackerMemory::default().stale_after_s, murmur_decide::engage::OUT_OF_VIEW_S);
}

/// The physical model is the default (WORLD-V3-PLAN section 3: the catalog carries every
/// sensor's figures): a scenario that names no model, with no `MURMUR_SENSE_MODEL`, runs on it;
/// a scenario naming `legacy`, or the environment (as the v1 and v2 worlds and the Phase G stacks
/// set it), runs on the legacy model.
#[test]
fn the_physical_model_is_the_default() {
    let unpinned = Tracks::new(catalog(), Settings { model: None, ..settings() });
    assert_eq!(unpinned.model_for(&truth(1.0, Vec::new())), SensingModel::Physical);
    let mut named = truth(2.0, Vec::new());
    named.sensing.model = Some(SensingModel::Legacy);
    assert_eq!(unpinned.model_for(&named), SensingModel::Legacy);
    let pinned = Tracks::new(catalog(), settings());
    assert_eq!(pinned.model_for(&truth(1.0, Vec::new())), SensingModel::Legacy);
}
