//! Rules 33 (docs/SCENARIO-SETTINGS.md section 3, stage S4): the tracker reads the run's watch
//! settings, as the decision layer hands them to the store (`Tracks::set_watch`). A neutral call by
//! classification waits for the watch's criteria (the kind at the domain's level, from one of the
//! domain's sensors, at the bar) under the physical model; the legacy model and rules below 33
//! keep the call from 0.3. AIS calls and suspect calls are unchanged. A track is stale after the
//! run's `stale_s`.

use std::sync::Arc;

use murmur_decide::{Affiliation, WatchSettings};

use super::super::model::{STALE_S, Track, TrackerWatch};
use super::super::physical::conditions::Conditions;
use super::super::sensing::{AisHeard, Detection, Level, Look, Observer, SensorType};
use super::super::truth::{ContactTruth, Domain, Sensing, SensingModel, TruthAffiliation, TruthMessage};
use super::super::{Settings, Tracks};
use super::{BASE, at, catalog};

/// One look of the physical model's: `sensor` of vehicle `uid` saw `label` at `level`.
fn look(domain: Domain, uid: u64, sensor: SensorType, level: Level, label: &str, confidence: f64) -> Detection {
    Detection {
        contact_id: "c".into(),
        uid,
        sensor,
        lat: BASE.0,
        lon: BASE.1,
        alt_m: None,
        sigma_m: 30.0,
        range_m: 1_000.0,
        velocity_mps: None,
        label: Some(label.into()),
        ais: None,
        domain,
        measured_t: Some(1_000.0),
        slant_from: None,
        look: Some(Look { level, confidence }),
        scan_s: None,
    }
}

/// The standing watch of rules 33 (`sim/policy.toml`'s `[watch]`: the bar 0.6, neutral after it).
fn rules33() -> Arc<TrackerWatch> {
    Arc::new(TrackerWatch::of(&WatchSettings::default()))
}

/// Under rules below 33 the tracker reads the legacy watch, which is exactly what it did before:
/// stale after 10 s, a neutral call by classification from 0.3.
#[test]
fn rules_below_33_change_nothing_the_tracker_did() {
    assert_eq!(TrackerWatch::of(WatchSettings::legacy()), *TrackerWatch::standing());
    assert_eq!(TrackerWatch::standing().stale_s, STALE_S);
    assert!(TrackerWatch::standing().neutral_after.is_none());
    // and the legacy sensing model keeps the call from 0.3 under rules 33 too
    assert!(rules33().legacy_model().neutral_after.is_none());
    assert_eq!(rules33().legacy_model().stale_s, WatchSettings::default().stale_s);
}

/// A fishing vessel identified by one camera at 0.45: neutral at once before rules 33 (0.8 of the
/// identity), unknown under rules 33 saying why, until a second camera's look takes the identity
/// past the bar (0.725), when it is neutral as before. A size class or a recognition never makes
/// the call (the kind is an identification's), and a suspect call is unchanged.
#[test]
fn a_neutral_call_by_classification_waits_for_the_bar_under_rules_33() {
    let first = look(Domain::Surface, 1, SensorType::EoIr, Level::Identify, "fishing_vessel", 0.45);
    let second = look(Domain::Surface, 2, SensorType::EoIr, Level::Identify, "fishing_vessel", 0.5);
    let mut before = Track::new("T-0001".into(), 1, &first, 1_000.0);
    let mut now = before.clone();
    assert!(now.set_watch(&rules33()), "a watch that differs");
    assert!(!now.set_watch(&rules33()), "the same watch again changes nothing");
    let (aff, conf, why) = before.estimate();
    assert_eq!(aff, Affiliation::Neutral);
    assert!((conf - 0.45 * 0.8).abs() < 1e-9, "{conf}");
    assert_eq!(why, "fishing_vessel, small craft not required to carry AIS");
    let (aff, conf, why) = now.estimate();
    assert_eq!(aff, Affiliation::Unknown);
    assert!((conf - 0.45).abs() < 1e-9, "{conf}");
    assert_eq!(why, "fishing_vessel at 0.45, under the watch bar 0.6: not called neutral until identified");
    // the picture says it the same way (the feed's kept text included: a test build checks it)
    assert_eq!(now.json(1_000.0)["affiliation"], "unknown");
    assert_eq!(now.json(1_000.0)["affiliation_reason"], why.as_str());
    assert!(now.feed_json(1_000.0, 50).contains("under the watch bar 0.6"));
    assert_eq!(now.facts(1_000.0).affiliation.value, Affiliation::Unknown);
    // a second camera's look: 1 - 0.55 x 0.5 = 0.725, past the bar
    before.update(&second, 1_001.0);
    now.update(&second, 1_001.0);
    let (aff, conf, why) = now.estimate();
    assert_eq!(aff, Affiliation::Neutral, "{why}");
    assert!((conf - 0.725 * 0.8).abs() < 1e-9, "{conf}");
    assert_eq!(before.estimate(), now.estimate(), "past the bar the call is as before");
    // the identity itself is the same under either watch
    assert_eq!(format!("{:?}", before.identity()), format!("{:?}", now.identity()));
    // an aircraft's kind alike
    let mut plane = Track::new("T-0002".into(), 2, &look(Domain::Air, 1, SensorType::EoIr, Level::Identify, "light_aircraft", 0.59), 1_000.0);
    assert_eq!(plane.estimate().0, Affiliation::Neutral);
    plane.set_watch(&rules33());
    assert_eq!(plane.estimate().2, "light_aircraft at 0.59, under the watch bar 0.6: not called neutral until identified");
    // a suspect call by classification does not wait
    let mut fast = Track::new("T-0003".into(), 3, &look(Domain::Surface, 1, SensorType::EoIr, Level::Identify, "small_fast_boat", 0.45), 1_000.0);
    fast.set_watch(&rules33());
    assert_eq!(fast.estimate().0, Affiliation::Suspect);
}

/// The watch's other criteria: the kind from one of the domain's sensors (a surface contact's
/// camera or its own AIS; a sonar's identification of a fishing vessel is none of them), and the
/// domain's level. A scenario's own settings are read: a bar of 0.75, or `neutral_after_bar`
/// false (the call from 0.3, as before).
#[test]
fn the_call_needs_the_domains_sensor_and_reads_the_scenarios_settings() {
    let heard = look(Domain::Surface, 4, SensorType::Sonar, Level::Identify, "fishing_vessel", 0.8);
    let mut t = Track::new("T-0001".into(), 1, &heard, 1_000.0);
    t.set_watch(&rules33());
    let (aff, _, why) = t.estimate();
    assert_eq!(aff, Affiliation::Unknown);
    assert_eq!(why, "fishing_vessel at 0.80, supported by sonar, not by the watch's eo_ir or ais for a surface contact: not called neutral until identified");
    // a scenario that counts a sonar's identification on the surface
    let mut w = WatchSettings::default();
    w.domain.surface.sensors.push("sonar".into());
    t.set_watch(&Arc::new(TrackerWatch::of(&w)));
    assert_eq!(t.estimate().0, Affiliation::Neutral);
    // a bar of 0.75: a camera's 0.7 is under it
    let mut w = WatchSettings { bar: 0.75, ..WatchSettings::default() };
    let mut c = Track::new("T-0002".into(), 2, &look(Domain::Surface, 1, SensorType::EoIr, Level::Identify, "sailboat", 0.7), 1_000.0);
    c.set_watch(&Arc::new(TrackerWatch::of(&w)));
    assert_eq!(c.estimate().2, "sailboat at 0.70, under the watch bar 0.75: not called neutral until identified");
    // a scenario that turns it off calls it from 0.3
    w.neutral_after_bar = false;
    c.set_watch(&Arc::new(TrackerWatch::of(&w)));
    assert_eq!(c.estimate().0, Affiliation::Neutral);
    // an AIS identity's call is unchanged: a fishing vessel's own AIS, no camera yet
    let mut a = Track::new(
        "T-0003".into(),
        3,
        &Detection {
            sensor: SensorType::Ais,
            label: None,
            look: None,
            ais: Some(AisHeard { mmsi: "366000009".into(), name: "F/V TEST".into(), ship_type: "fishing".into() }),
            ..look(Domain::Surface, 1, SensorType::Ais, Level::Detect, "", 0.0)
        },
        1_000.0,
    );
    a.set_watch(&rules33());
    assert_eq!(a.estimate().0, Affiliation::Neutral);
}

/// A track is stale after the run's `stale_s`: the track itself, the store's answers and its feed,
/// and the keep-out explanation's memory all read it.
#[test]
fn a_track_is_stale_after_the_runs_stale_s() {
    let w = WatchSettings { stale_s: 13.0, ..WatchSettings::default() };
    let mut t = Track::new("T-0001".into(), 1, &look(Domain::Surface, 1, SensorType::EoIr, Level::Identify, "small_fast_boat", 0.9), 1_000.0);
    assert!(t.is_stale(1_000.0 + STALE_S + 1.0), "the standing 10 s");
    t.set_watch(&Arc::new(TrackerWatch::of(&w)));
    assert!(!t.is_stale(1_012.0) && t.is_stale(1_014.0));
    assert_eq!(t.json(1_012.0)["state"], "active");
    let tracks = Tracks::new(catalog(), super::settings());
    assert_eq!(tracks.api_json(1_000.0)["stale_after_s"], STALE_S);
    tracks.set_watch(TrackerWatch::of(&w));
    assert_eq!(tracks.stale_s(), 13.0);
    assert_eq!(tracks.api_json(1_000.0)["stale_after_s"], 13.0);
    assert_eq!(tracks.memory().stale_after_s, 13.0);
}

/// A second of the run: the track's kind, its level and confidence, its affiliation and why.
type Seen = (Option<String>, Option<Level>, f64, Affiliation, String);

/// HUNTER 1 (a Seahunter's ring of cameras and its radar) watching a fishing vessel without AIS
/// come in from 7.5 km to 3.75 km at 25 m/s, by day: its camera's identification at 0.32 out
/// there, 0.6 at 5 km, 0.78 at 4 (the physical model's look at the range).
fn fishing_run(model: SensingModel, watch: Option<TrackerWatch>) -> Vec<Seen> {
    const T0: f64 = 1_781_431_200.0;
    let settings = Settings { seed: Some(5), misclassification_rate: Some(0.0), eval_truth: false, drop_s: 180.0, model: Some(model), sim_token: None };
    let tracks = Tracks::new(catalog(), settings);
    if let Some(w) = watch {
        tracks.set_watch(w);
    }
    let p = catalog().get("seahunter").expect("seahunter").clone();
    let o = Observer { uid: 1, lat: BASE.0, lon: BASE.1, alt_msl: 0.0, mast_m: p.mast_m, sensors: p.sensors, heading_deg: Some(0.0), ..Observer::default() };
    let boat = ContactTruth {
        id: "fisher".into(),
        name: "fisher".into(),
        affiliation: TruthAffiliation::Neutral,
        domain: Domain::Surface,
        kind: "fishing_vessel".into(),
        lat: BASE.0,
        lon: BASE.1,
        alt_m: 0.0,
        speed_mps: 25.0,
        course_deg: 270.0,
        emits: false,
        signature: 0.7,
        ais: None,
    };
    let mut seen = Vec::new();
    for s in 0..150 {
        let s = f64::from(s);
        let mut boat = boat.clone();
        (boat.lat, boat.lon) = at(BASE.0, BASE.1, 7_500.0 - 25.0 * s, 0.0);
        let m = TruthMessage {
            t: T0 + s,
            s,
            seq: s as u64,
            scenario: "watch".into(),
            seed: 5,
            sensing: Sensing { conditions: Some(Conditions { sun_elevation_deg: Some(45.0), ..Conditions::default() }), ..Sensing::default() },
            contacts: vec![boat],
            acts: Vec::new(),
            aim_at: Vec::new(),
            weapons: Vec::new(),
            impacts: Vec::new(),
            mines_live: Vec::new(),
            look: Vec::new(),
        };
        tracks.on_truth_with(m, std::slice::from_ref(&o), T0 + s).expect("sensing");
        if let Some(t) = tracks.track("T-0001") {
            let id = t.identity();
            let (aff, _, why) = t.estimate();
            seen.push((id.kind, id.level, id.confidence, aff, why));
        }
    }
    seen
}

/// Through the store, as a run senses: the same detections and identities under either watch;
/// under rules 33 the physical model calls the fishing vessel neutral only once its camera's
/// identification reaches the bar (unknown before, saying so), while before rules 33 it was
/// neutral from 0.3. The legacy model keeps the call from 0.3 under rules 33.
#[test]
fn through_the_store_the_physical_model_calls_neutral_after_the_bar() {
    let before = fishing_run(SensingModel::Physical, None);
    let now = fishing_run(SensingModel::Physical, Some(TrackerWatch::of(&WatchSettings::default())));
    assert_eq!(before.len(), now.len());
    let mut held = 0;
    for ((k, l, c, a0, _), (k1, l1, c1, a1, why)) in before.iter().zip(&now) {
        assert_eq!((k, l, c), (k1, l1, c1), "the identity is the same under either watch");
        let fishing = k.as_deref() == Some("fishing_vessel") && *l == Some(Level::Identify);
        if fishing && *c >= 0.3 {
            assert_eq!(*a0, Affiliation::Neutral, "{c}");
        }
        if fishing && *c >= 0.6 {
            assert_eq!(*a1, Affiliation::Neutral, "{c}: {why}");
        } else if fishing && *c >= 0.3 {
            assert_eq!(*a1, Affiliation::Unknown, "{c}");
            assert!(why.contains("under the watch bar 0.6"), "{why}");
            held += 1;
        } else {
            assert_eq!(a0, a1, "{k:?} {l:?} {c}");
        }
    }
    assert!(held > 0, "the run shows a call held for the bar: {before:?}");
    assert!(now.iter().any(|(_, _, c, a, _)| *a == Affiliation::Neutral && *c >= 0.6), "and made past it: {now:?}");
    // the legacy model keeps the call from 0.3 whatever the watch
    let legacy = fishing_run(SensingModel::Legacy, None);
    let legacy33 = fishing_run(SensingModel::Legacy, Some(TrackerWatch::of(&WatchSettings::default())));
    assert_eq!(format!("{legacy:?}"), format!("{legacy33:?}"));
    assert!(legacy.iter().any(|(_, _, c, a, _)| *a == Affiliation::Neutral && *c < 0.6), "{legacy:?}");
}

