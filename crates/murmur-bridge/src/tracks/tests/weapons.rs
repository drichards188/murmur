//! Opponent weapons through the bridge (docs/OPPONENT-WEAPONS.md): the tutorial's geometry, a
//! fast boat firing an ATGM-class missile at HUNTER 1 1 km east of it, by day, physical model.
//! The launch is the act's observation, by HUNTER 1's ring camera alone; the weapon and its
//! strike live in the weapon picture, never in the track picture; and the contacts' detections
//! are the same with a weapon in the truth as without. Then every class (section 15): a rocket
//! and a gun from the boat, a missile from a UAS 300 m up, a torpedo from a submarine 30 m down;
//! a gun's episode as one act; a node's look answered.

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
        impacts: Vec::new(), mines_live: Vec::new(), look: Vec::new(),
    }
}

/// The flight from the boat to HUNTER 1, launched at second 20: 1 km at 250 m/s. Its name, its
/// stated speed and its stated victim are ones only the truth knows (the weapon picture must not
/// show them: bd murmur-ju1h); its flight (from, to, the times) is what the sensors see.
fn flight() -> WeaponTruth {
    let o = hunter();
    WeaponTruth {
        id: "act-1:weapon".into(),
        act: "act-1".into(),
        by: "fastboat-raider".into(),
        name: "TRUTH-ONLY NAME".into(),
        kind: "atgm".into(),
        speed_mps: 251.5,
        launch_t: T0 + 20.0,
        impact_t: T0 + 24.0,
        from: [BOAT.1, BOAT.0, 4.0],
        to: [o.lon, o.lat, 4.0],
        against_vehicle: Some(77),
        against_contact: None,
        rounds: None,
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
    assert!((w["warning_s"].as_f64().expect("warning") - 4.0).abs() < 1e-6, "{w}");
    // nothing of the truth's weapon is in the picture: not its name, its stated speed or victim;
    // a class only from the threat library, a target only one of ours derived from what was seen
    let text = msg["weapons"].to_string();
    assert!(!text.contains("TRUTH-ONLY") && !text.contains("251.5") && !text.contains("\"77\""), "{text}");
    assert!(w["class"].is_null() || w["class"] == "ATGM-class", "{w}");
    assert!(w["aimed_at"].is_null() || w["aimed_at"] == "1", "{w}");
    assert_ne!(w["stage"], "plot", "{w}");
    let alerts = tracks.strike_alerts(&|_| Some("HUNTER 1".into()));
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].kind, "struck");
    assert_eq!(alerts[0].severity, crate::decision::alerts::Severity::Medium);
    // the strike's vehicle event, as it was logged before the stages (put on W-1 by its victim)
    let strikes = std::mem::take(&mut super::super::lock(&tracks.inner).strikes);
    assert_eq!(strikes, vec![(1, "struck (W-1): launch seen by 1:eo_ir 4.0 s before".to_string())], "the strike waits for its vehicle's events");
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

// ---- every class (docs/OPPONENT-WEAPONS.md section 15, bd murmur-y85j.3) ----------------------

/// An armed UAS 300 m up, 1 km north of the boat (some 1.4 km from HUNTER 1).
fn uas() -> ContactTruth {
    let (lat, lon) = at(BOAT.0, BOAT.1, 0.0, 1_000.0);
    ContactTruth { id: "uas".into(), name: "uas".into(), domain: Domain::Air, kind: "unknown_uas".into(), lat, lon, alt_m: 300.0, signature: 0.15, ..boat() }
}

/// A submarine 30 m down, 1 km south of HUNTER 1.
fn sub() -> ContactTruth {
    let (lat, lon) = at(BOAT.0, BOAT.1, 1_000.0, -1_000.0);
    ContactTruth { id: "sub".into(), name: "sub".into(), domain: Domain::Subsurface, kind: "submarine".into(), lat, lon, alt_m: -30.0, signature: 0.3, ..boat() }
}

/// REAPER 1, 7,600 m up, 3 km west and 2 km south of the boat.
fn reaper() -> Observer {
    let (lat, lon) = at(BOAT.0, BOAT.1, -3_000.0, -2_000.0);
    let p = catalog().get("mq9b_armed").expect("mq9b_armed").clone();
    Observer { uid: 2, lat, lon, alt_msl: 7_600.0, sensors: p.sensors, heading_deg: Some(0.0), ..Observer::default() }
}

/// What `kind` fires at HUNTER 1 under act `act-1`, launched at second 20 (a gun: three bursts,
/// at 20, 25 and 30): from the boat a few metres up, from the UAS at its height, from the
/// submarine at its depth; at the class's speed.
fn shots(kind: &str) -> Vec<WeaponTruth> {
    let h = hunter();
    let (shooter, from, to_z, speed) = match kind {
        "air_missile" => (uas(), 300.0, 4.0, 510.0),
        "torpedo" => (sub(), -30.0, -3.0, 23.0),
        "rocket" => (boat(), 3.0, 4.0, 300.0),
        "gun" => (boat(), 3.0, 4.0, 850.0),
        _ => (boat(), 4.0, 4.0, 250.0),
    };
    let d = crate::geo::haversine_m(shooter.lat, shooter.lon, h.lat, h.lon).hypot(to_z - from);
    let launches: &[f64] = if kind == "gun" { &[20.0, 25.0, 30.0] } else { &[20.0] };
    launches
        .iter()
        .enumerate()
        .map(|(i, &at_s)| WeaponTruth {
            id: format!("act-1:w{}", i + 1),
            act: "act-1".into(),
            by: shooter.id.clone(),
            name: "TRUTH-ONLY NAME".into(),
            kind: kind.into(),
            speed_mps: speed,
            launch_t: T0 + at_s,
            impact_t: T0 + at_s + d / speed,
            from: [shooter.lon, shooter.lat, from],
            to: [h.lon, h.lat, to_z],
            against_vehicle: Some(1),
            against_contact: None,
            rounds: (kind == "gun").then_some(6),
        })
        .collect()
}

type Tick = (usize, usize, usize);

/// The scene (the boat, the UAS, the submarine; HUNTER 1 and REAPER 1, as `observers_at` gives
/// them each second) run for `secs` s, with `kind`'s shots in the truth or none: each second's
/// (contacts, detections, tracks) and track picture.
fn scene(kind: Option<&str>, secs: u32, observers_at: &dyn Fn(f64) -> Vec<Observer>) -> (Tracks, Vec<Tick>, Vec<std::collections::BTreeMap<String, murmur_decide::TrackFacts>>) {
    let tracks = Tracks::new(catalog(), settings());
    let ws = kind.map(shots).unwrap_or_default();
    let (mut ticks, mut facts) = (Vec::new(), Vec::new());
    for s in 0..secs {
        let s = f64::from(s);
        let t = T0 + s;
        let mut m = msg(s);
        m.contacts = vec![boat(), uas(), sub()];
        if let Some(first) = ws.first().filter(|w| w.launch_t == t) {
            m.acts = vec![ActTruth {
                id: "act-1".into(),
                s,
                t,
                by: first.by.clone(),
                lat: first.from[1],
                lon: first.from[0],
                against_vehicle: Some(1),
                against_contact: None,
                label: None,
                weapon: Some(first.id.clone()),
                held: None,
            }];
        }
        m.weapons = ws.iter().filter(|w| w.launch_t <= t && w.impact_t > t - 1.0).cloned().collect();
        m.impacts = ws
            .iter()
            .filter(|w| w.impact_t <= t && w.impact_t > t - 1.0)
            .map(|w| ImpactTruth { weapon: w.id.clone(), act: w.act.clone(), t: w.impact_t, lat: w.to[1], lon: w.to[0], against_vehicle: Some(1), against_contact: None })
            .collect();
        ticks.push(tracks.on_truth_with(m, &observers_at(s), t).expect("ok"));
        facts.push(tracks.facts(t));
    }
    (tracks, ticks, facts)
}

const KINDS: [&str; 5] = ["atgm", "rocket", "air_missile", "gun", "torpedo"];

/// Every class in the truth leaves the contacts' detections and the track picture exactly as a
/// run without it, second by second (weapon sensing reads the engine's state and never writes
/// it, its draws salted apart); and each is sensed as its physics says: the launch of the boat's,
/// the UAS's and the gun's by HUNTER 1's camera alone, the torpedo's by its sonar alone, the act
/// reported once (one report for the gun's three bursts), the strikes all reported.
#[test]
fn every_class_leaves_the_detections_and_the_track_picture_as_they_were() {
    let both = |_: f64| vec![hunter(), reaper()];
    let (_, quiet_ticks, quiet) = scene(None, 70, &both);
    for kind in KINDS {
        let (tracks, ticks, facts) = scene(Some(kind), 70, &both);
        assert_eq!(ticks, quiet_ticks, "{kind}: detections");
        assert_eq!(facts, quiet, "{kind}: the track picture");
        let reports = tracks.take_acts();
        assert_eq!(reports.len(), 1, "{kind}: {reports:?}");
        let by = if kind == "torpedo" { ":sonar" } else { ":eo_ir" };
        assert!(reports[0].sensors.iter().all(|s| s.ends_with(by)), "{kind}: {:?}", reports[0].sensors);
        // the engine's `ident.act` reads the same report as ever: its shape and its meaning
        let r = &reports[0];
        let back: murmur_decide::ActReport = serde_json::from_value(serde_json::to_value(r).expect("json")).expect("the same shape");
        assert_eq!((&back, r.act.as_str(), r.channel.as_deref()), (r, "A-1", Some("sim:contacts")), "{kind}");
        assert_eq!(r.against, murmur_decide::Against::Vehicle { uid: crate::decision::snapshot::duid(1) }, "{kind}");
        let strikes = std::mem::take(&mut super::super::lock(&tracks.inner).strikes);
        assert_eq!(strikes.len(), shots(kind).len(), "{kind}: {strikes:?}");
        let msg: serde_json::Value = serde_json::from_str(&tracks.message(T0 + 69.0)).expect("json");
        let text = msg["weapons"].to_string();
        assert!(!text.contains("TRUTH-ONLY") && !text.contains("\"kind\""), "{kind}: nothing of the truth's weapon is served: {text}");
    }
}

/// A gun's firing episode is one act: its three bursts are three launches in the weapon picture
/// (each told as gunfire, so each the Gun-class), all under the act's one report, none fixed in
/// flight (its rounds are not plotted or placed); each burst's strike is reported.
#[test]
fn a_gun_episode_is_one_act_and_its_bursts_are_launches_without_fixes() {
    let (tracks, _, _) = scene(Some("gun"), 40, &|_| vec![hunter(), reaper()]);
    assert_eq!(tracks.take_acts().len(), 1);
    let inner = super::super::lock(&tracks.inner);
    let launched: Vec<_> = inner.weapons.tracks.iter().filter(|w| w.launch.is_some()).collect();
    assert!(!launched.is_empty() && launched.len() <= 3, "{:?}", inner.weapons.tracks);
    assert!(inner.weapons.tracks.iter().all(|w| w.fixes.is_empty() && w.kin.is_none()), "no fix of a gun's rounds");
    for w in &launched {
        assert_eq!(w.report.as_deref(), Some("A-1"));
        assert_eq!(w.launch.as_ref().and_then(|l| l.look), Some(super::super::physical::weapon::LaunchLook::Muzzle));
        assert_eq!(w.class.as_ref().map(|a| (a.class.class, a.class.symbol)), Some(("Gun-class", "gun")), "{w:?}");
    }
    assert_eq!(inner.strikes.len(), 3);
}

/// A gun act no sensor caught at its first burst (HUNTER 1 silent then) is reported at the first
/// later burst caught, once, from where that burst's muzzle was; its record says which burst.
#[test]
fn a_gun_act_unseen_at_its_first_burst_is_reported_at_a_later_one() {
    let (tracks, _, _) = scene(Some("gun"), 40, &|s| if (19.0..=21.0).contains(&s) { Vec::new() } else { vec![hunter()] });
    let reports = tracks.take_acts();
    assert_eq!(reports.len(), 1, "{reports:?}");
    let inner = super::super::lock(&tracks.inner);
    let records: Vec<_> = inner.act_records.iter().filter(|r| r["act"] == "act-1").collect();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!((records[0]["observed"].clone(), records[0]["report"].clone()), (serde_json::json!(true), serde_json::json!("A-1")));
    assert!(matches!(records[0]["seen_at"].as_str(), Some("act-1:w2" | "act-1:w3")), "{}", records[0]);
    assert!(inner.unseen_acts.is_empty());
}

/// A torpedo is heard, never seen: a sonar that listens catches its launch transient (told as
/// one) and fixes it running; cameras and radars, HUNTER 1's and REAPER 1's, never do.
#[test]
fn a_torpedo_is_heard_and_never_seen() {
    use super::super::physical::weapon::{EoWeapon, LaunchLook};
    use super::super::physical::{Engine, Step};
    use super::super::sensing::SensorType;
    let w = shots("torpedo").remove(0);
    let mut deaf = hunter();
    deaf.sensors.retain(|s| s.kind != SensorType::Sonar);
    let sensing = msg(0.0).sensing;
    let (mut heard, mut fixes) = (0, 0);
    for seed in 1..=20u64 {
        let e = Engine::new(None);
        let st = Step { seed, tick: 20, s: 20.0, t: T0 + 20.0, misclassification_rate: 0.0, sensing: &sensing };
        let mut shooter = sub();
        (shooter.lat, shooter.lon, shooter.alt_m) = (w.from[1], w.from[0], w.from[2]);
        assert!(e.observe_launch(&st, &[deaf.clone(), reaper()], &shooter, "act-1", "torpedo").is_none());
        if let Some(l) = e.observe_launch(&st, &[hunter()], &shooter, "act-1", "torpedo") {
            heard += 1;
            assert_eq!((l.obs.sensors.clone(), l.look), (vec!["1:sonar".to_string()], Some(LaunchLook::Transient)));
        }
        let mut cams: std::collections::HashMap<(String, u64, usize), EoWeapon> = std::collections::HashMap::new();
        for s in 21..60 {
            let st = Step { seed, tick: s, s: s as f64, t: T0 + s as f64, misclassification_rate: 0.0, sensing: &sensing };
            assert!(e.sense_weapons(&st, T0 + s as f64 - 1.0, &[deaf.clone(), reaper()], std::slice::from_ref(&w), &mut cams).is_empty());
            let f = e.sense_weapons(&st, T0 + s as f64 - 1.0, &[hunter()], std::slice::from_ref(&w), &mut cams);
            assert!(f.iter().all(|f| f.sensor == SensorType::Sonar && f.alt_m.is_none()), "{f:?}");
            fixes += f.len();
        }
    }
    assert!(heard >= 15 && fixes > 0, "heard {heard} of 20, {fixes} fixes");
}

/// A node's look is answered with ours within its reach (the straight line, heights included):
/// HUNTER 1 2 km off yes, a boat 8 km off no, REAPER 1 7.6 km straight overhead no; `aim_at`
/// still works and the answer is the union, each once; a message asking nothing gets none.
#[test]
fn a_look_is_answered_with_ours_within_its_reach() {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use super::super::truth::LookTruth;
    use crate::link::{LinkStats, Transmit};
    use crate::vehicle::VehicleCell;
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
    for (uid, east, alt) in [(1u64, 2_000.0, 0.0), (2, 0.0, 7_600.0), (3, 8_000.0, 0.0)] {
        let cell = Arc::new(VehicleCell::new(uid, uid as u8, Arc::new(NoTx), Arc::new(Mutex::new(LinkStats::default())), None));
        let (lat, lon) = at(BOAT.0, BOAT.1, east, 0.0);
        cell.write(|v| {
            v.pos = Some((lat, lon));
            v.alt_msl = Some(alt);
        });
        world.insert_vehicle(cell);
    }
    let tracks = Tracks::new(catalog(), settings());
    let look = [LookTruth { node: "fastboat-raider".into(), lat: BOAT.0, lon: BOAT.1, alt_m: 2.0, reach_m: 5_000.0 }];
    let uids = |v: Option<Vec<serde_json::Value>>| v.map(|v| v.iter().map(|o| o["uid"].as_u64().expect("uid")).collect::<Vec<_>>());
    assert_eq!(uids(tracks.ours_answer(&world, &[], &[], &look)), Some(vec![1]));
    assert_eq!(uids(tracks.ours_answer(&world, &[3, 1], &[], &look)), Some(vec![1, 3]), "aim_at's, then the look's not already in");
    assert_eq!(uids(tracks.ours_answer(&world, &[], &[], &[])), None);
    let o = &tracks.ours_answer(&world, &[], &[], &look).expect("asked")[0];
    assert!(o["lat"].is_f64() && o["alt_m"] == 0.0 && o["ve"] == 0.0, "{o}");
}

/// The truth message reads `look` and a burst's `rounds`, ignores what the bridge never reads
/// (`node_events`, a contact's `variant`), and leaves both new fields out when they are empty.
#[test]
fn the_truth_message_reads_look_and_rounds_and_ignores_node_events() {
    let text = serde_json::json!({
        "t": T0, "contacts": [{ "id": "b", "affiliation": "hostile", "domain": "surface", "kind": "small_fast_boat", "variant": "armed_fast_boat", "lat": BOAT.0, "lon": BOAT.1 }],
        "look": [{ "node": "b", "lat": BOAT.0, "lon": BOAT.1, "alt_m": 2.0, "reach_m": 5000.0 }],
        "weapons": [{ "id": "b-1:w1", "act": "b-1", "by": "b", "name": "x", "kind": "gun", "speed_mps": 850.0, "launch_t": T0, "impact_t": T0 + 1.0, "from": [BOAT.1, BOAT.0, 3.0], "to": [BOAT.1, BOAT.0 + 0.008, 4.0], "rounds": 6 }],
        "node_events": [{ "node": "b", "event": "fired" }],
    });
    let m: TruthMessage = serde_json::from_value(text).expect("parses");
    m.validate().expect("valid");
    assert_eq!((m.look.len(), m.look[0].reach_m, m.weapons[0].rounds), (1, 5_000.0, Some(6)));
    let mut quiet = m.clone();
    quiet.look.clear();
    quiet.weapons[0].rounds = None;
    let back = serde_json::to_value(&quiet).expect("json");
    assert!(back.get("look").is_none() && back["weapons"][0].get("rounds").is_none(), "{back}");
    let mut bad = m;
    bad.look[0].reach_m = f64::NAN;
    assert!(bad.validate().is_err());
}
