//! The sensing model and the tracker against synthetic truth: detection against range,
//! confidence growth with dwell and sensors, uncertainty growth with age, AIS identity and
//! spoofing, sonar-only subsurface, determinism, and that truth never leaks into anything
//! the console or an agent reads.

use super::model::{STALE_S, Track};
use super::sensing::{
    Detection, Observer, Params, SensorCatalog, SensorSpec, SensorType, detect, horizon_m, pd,
};
use super::truth::{Ais, ContactTruth, Domain, Sensing, TruthAffiliation, TruthMessage};
use super::{Settings, Tracks};
use murmur_decide::Affiliation;

const T0: f64 = 1_000_000.0;

fn catalog() -> SensorCatalog {
    SensorCatalog::from_toml(include_str!("../../../../sim/platforms.toml")).expect("catalog")
}

fn settings() -> Settings {
    Settings {
        seed: Some(42),
        misclassification_rate: Some(0.0),
        eval_truth: false,
        drop_s: 180.0,
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
        sensors: sensors
            .iter()
            .map(|(kind, range_m)| SensorSpec {
                kind: *kind,
                range_m: *range_m,
                guess: true,
                height: false,
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

// ---- the tracker's uncertainty against truth (review of rules 9: a track reported 8 m while
// about 20 m off) ---------------------------------------------------------------------------

/// One run of the tracker against a contact moving as `pos(s)` (east, north metres from BASE
/// at scenario second s; speed east, north), seen by `observers`, for `n` seconds: the mean
/// normalized error (NEES: squared horizontal error over the per-axis 1-sigma squared,
/// expected 2 for an honest 2D Gaussian) and the share of seconds with the truth within two
/// sigmas (expected 0.865), after a 30 s settling time.
fn calibration(
    kind: &str,
    ais: bool,
    observers: &[Observer],
    n: u32,
    seed: u64,
    pos: impl Fn(f64) -> ((f64, f64), (f64, f64)),
) -> (f64, f64, f64) {
    let tracks = Tracks::new(catalog(), Settings { seed: Some(seed), ..settings() });
    let mut nees = Vec::new();
    let mut inside = 0usize;
    let mut sig = Vec::new();
    for s in 0..n {
        let sf = f64::from(s);
        let ((e, nn), (ve, vn)) = pos(sf);
        let (lat, lon) = at(BASE.0, BASE.1, e, nn);
        let mut c = contact("c", kind, Domain::Surface, lat, lon);
        c.speed_mps = ve.hypot(vn);
        c.course_deg = ve.atan2(vn).to_degrees().rem_euclid(360.0);
        if ais {
            c.ais = Some(crate::tracks::truth::Ais {
                mmsi: "366000001".into(),
                name: "MV TEST".into(),
                ship_type: "passenger".into(),
                spoofed: false,
            });
            c.emits = true;
        }
        tracks.on_truth_with(truth(sf, vec![c]), observers, T0 + sf).expect("ok");
        if s < 30 {
            continue;
        }
        let Some(t) = tracks.track("T-0001") else { continue };
        if (t.last_seen_t - (T0 + sf)).abs() > 0.01 {
            continue;
        }
        let de = (t.lon - lon) * 111_320.0 * lat.to_radians().cos();
        let dn = (t.lat - lat) * 111_320.0;
        let d2 = de * de + dn * dn;
        nees.push(d2 / (t.sigma_m * t.sigma_m));
        sig.push(t.sigma_m);
        if d2.sqrt() <= 2.0 * t.sigma_m {
            inside += 1;
        }
    }
    let k = nees.len().max(1) as f64;
    (nees.iter().sum::<f64>() / k, inside as f64 / k, sig.iter().sum::<f64>() / k)
}

#[test]
fn the_trackers_uncertainty_covers_the_truth() {
    let near = observer(1, BASE.0, BASE.1 - 0.027, 0.0, &[(SensorType::Radar, 8_000.0), (SensorType::EoIr, 5_000.0)]);
    let far = observer(2, BASE.0 + 0.05, BASE.1, 0.0, &[(SensorType::Radar, 12_000.0)]);
    let near2 = observer(4, BASE.0 + 0.025, BASE.1 + 0.002, 0.0, &[(SensorType::Radar, 20_000.0), (SensorType::EoIr, 8_000.0)]);
    let blade = observer(5, BASE.0 - 0.01, BASE.1 + 0.03, 80.0, &[(SensorType::EoIr, 3_000.0)]);
    let quad = observer(6, BASE.0 - 0.005, BASE.1 + 0.012, 60.0, &[(SensorType::EoIr, 1_500.0), (SensorType::Radar, 5_000.0)]);
    let ais_rx = observer(3, BASE.0, BASE.1 + 0.03, 0.0, &[(SensorType::Ais, 30_000.0), (SensorType::Radar, 8_000.0)]);
    let r = 400.0;
    let w = 6.0 / r;
    // (name, kind, ais, observers, motion)
    #[allow(clippy::type_complexity)]
    let cases: Vec<(&str, &str, bool, Vec<Observer>, Box<dyn Fn(f64) -> ((f64, f64), (f64, f64))>)> = vec![
        ("holding", "small_fast_boat", false, vec![near.clone()], Box::new(|_| ((0.0, 0.0), (0.0, 0.0)))),
        ("holding, two observers", "small_fast_boat", false, vec![near.clone(), far.clone()], Box::new(|_| ((0.0, 0.0), (0.0, 0.0)))),
        // the live run's geometry: two ships 2.4 and 2.8 km off, each with radar and EO/IR
        ("holding, four sensors (live)", "small_fast_boat", false, vec![near.clone(), near2.clone()], Box::new(|_| ((0.0, 0.0), (0.0, 0.0)))),
        ("transit 12 m/s, four sensors", "small_fast_boat", false, vec![near.clone(), near2.clone()], Box::new(|s| ((-1500.0 + 12.0 * s, 0.0), (12.0, 0.0)))),
        (
            "stops after a transit, then holds 15 min, seven sensors",
            "small_fast_boat",
            false,
            vec![near.clone(), near2.clone(), blade.clone(), quad.clone()],
            Box::new(|s| if s < 120.0 { ((-1440.0 + 12.0 * s, 0.0), (12.0, 0.0)) } else { ((0.0, 0.0), (0.0, 0.0)) }),
        ),
        (
            "stops after a transit, four sensors",
            "small_fast_boat",
            false,
            vec![near.clone(), near2.clone()],
            Box::new(|s| if s < 120.0 { ((-1440.0 + 12.0 * s, 0.0), (12.0, 0.0)) } else { ((0.0, 0.0), (0.0, 0.0)) }),
        ),
        ("transit 10 m/s", "fishing_vessel", false, vec![near.clone(), far.clone()], Box::new(|s| ((-1500.0 + 10.0 * s, 0.0), (10.0, 0.0)))),
        (
            "a 90 degree turn at 3 deg/s",
            "fishing_vessel",
            false,
            vec![near.clone(), far.clone()],
            Box::new(move |s| {
                // east at 8 m/s for 150 s, a 30 s turn to north, then north
                let v = 8.0;
                let rt = v / (3f64.to_radians());
                if s < 150.0 {
                    ((-1200.0 + v * s, 0.0), (v, 0.0))
                } else if s < 180.0 {
                    let a = (s - 150.0) * 3f64.to_radians();
                    ((-1200.0 + v * 150.0 + rt * a.sin(), rt * (1.0 - a.cos())), (v * a.cos(), v * a.sin()))
                } else {
                    ((-1200.0 + v * 150.0 + rt, rt + v * (s - 180.0)), (0.0, v))
                }
            }),
        ),
        (
            "circling 400 m at 6 m/s",
            "ferry",
            false,
            vec![near.clone()],
            Box::new(move |s| {
                let a = w * s;
                ((r * a.sin(), r * a.cos()), (6.0 * a.cos(), -6.0 * a.sin()))
            }),
        ),
        ("ferry with AIS, 10 m/s", "ferry", true, vec![ais_rx.clone()], Box::new(|s| ((-1500.0 + 10.0 * s, 300.0), (10.0, 0.0)))),
        ("radar only, 6 km", "cargo_ship", false, vec![far.clone()], Box::new(|s| ((0.0, -500.0 + 4.0 * s), (0.0, 4.0)))),
    ];
    let mut report = Vec::new();
    let mut bad = Vec::new();
    let seeds = [1u64, 2, 3, 4, 5];
    for (name, kind, ais, obs, motion) in &cases {
        let (mut nees_sum, mut cover_sum) = (0.0, 0.0);
        for seed in seeds {
            let n = if name.contains("15 min") { 1020 } else { 300 };
            let (nees, cover, sigma) = calibration(kind, *ais, obs, n, seed, motion);
            report.push(format!("{name} seed {seed}: NEES {nees:.2}, within 2 sigma {:.0}%, mean sigma {sigma:.1} m", 100.0 * cover));
            nees_sum += nees;
            cover_sum += cover;
        }
        let (nees, cover) = (nees_sum / seeds.len() as f64, cover_sum / seeds.len() as f64);
        // honest, over the seeds: the truth within two sigmas at least about as often as a 2D
        // Gaussian says (86.5%; 83% allows for the runs' few independent samples), and the
        // mean normalized error (expected 2) neither above 2.6 (errors larger than the sigma
        // says, which would let a keep-out or an attribution trust it too much) nor below 1
        // (a sigma more than 1.4 times the errors, which would block for nothing)
        if !(cover >= 0.83 && (1.0..=2.6).contains(&nees)) {
            bad.push(format!("{name}: NEES {nees:.2}, coverage {cover:.2} over {} seeds", seeds.len()));
        }
    }
    eprintln!("{}", report.join("\n"));
    assert!(bad.is_empty(), "uncertainty not honest:\n{}\n\nall:\n{}", bad.join("\n"), report.join("\n"));
}
