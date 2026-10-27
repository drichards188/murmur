//! Rules 21 (bd murmur-bia): the identify order's plan, its refusals, and the end of its look.
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use murmur_decide::identify::{self, EndKind, Level, TrackLevel};
use murmur_decide::*;

const BOAT: LonLat = LonLat::new(-122.4950, 37.8120);
const PUMA_AT: LonLat = LonLat::new(-122.4930, 37.8210);

fn track(at: LonLat) -> TrackFacts {
    TrackFacts {
        position: Fact::new(at, T0),
        alt_m: None,
        alt_t: None,
        velocity_mps: Some([0.0, 0.0]),
        pos_sigma_m: 15.0,
        sigma_growth_mps: Some(1.0),
        confidence: 0.9,
        affiliation: Fact::new(Affiliation::Unknown, T0),
        affiliation_confidence: 0.8,
        identity: Some("small_craft".into()),
        identity_confidence: 0.7,
        sources: vec!["3:radar".into()],
        source_t: Default::default(),
        domain: Domain::Surface,
        last_seen_t: T0,
        declared_by: None,
        declaration: None,
        review_flags: Vec::new(),
        manoeuvre: None,
        mean_velocity_mps: None,
        first_seen_t: None,
        velocity_sigma_mps: None,
    }
}

/// A Puma at 120 m north of a boat the radar recognises.
fn scene(platform: &str) -> WorldSnapshot {
    let mut w = WorldSnapshot::new(T0);
    w.areas = Some(areas());
    let mut v = vehicle(platform, PUMA_AT);
    v.alt_msl_m = Some(Fact::new(120.0, T0));
    w.vehicles.insert(uid("puma-1"), v);
    w.tracks.insert("T-0001".into(), track(BOAT));
    w.track_levels.insert(
        "T-0001".into(),
        TrackLevel {
            level: Level::Recognise,
            by: Vec::new(),
            own_t: None,
            conflict: None,
            own: Default::default(),
        },
    );
    w
}

fn spec(standoff_m: Option<f64>) -> IntentSpec {
    IntentSpec {
        id: IntentId(1),
        created_t: T0,
        source: Source::Human { id: OperatorId::new("op-1") },
        task: Task::Identify {
            track: "T-0001".into(),
            standoff_m,
        },
        uids: vec![uid("puma-1")],
        constraints: Constraints::default(),
        rationale: None,
    }
}

fn cfg() -> RulesConfig {
    RulesConfig::default()
}

#[test]
fn the_standoff_is_inside_the_cameras_identification_reach() {
    // the legacy reach: range_m 4,000 m, an eighth of it 500 m; 70 % of that as slant range
    // from 120 m up is about 329 m on the ground
    let p = identify::plan(&spec(None), &scene("puma"), &cfg()).unwrap();
    assert_eq!(p.reach_m, 500.0);
    let want = (350.0f64 * 350.0 - 120.0 * 120.0).sqrt().ceil();
    assert_eq!(p.radius_m, want);
    assert!(p.radius_m.hypot(120.0) <= p.reach_m);
    assert_eq!(p.center, BOAT);
    assert!(!p.set_by_order);
    // the time to get there: the Puma's distance to the ring at its cruise speed
    let cruise = catalog().get("puma").unwrap().cruise_mps.unwrap();
    let d = murmur_decide::geo::haversine_m(PUMA_AT, BOAT) - p.radius_m;
    assert_eq!(p.transit_s, (d / cruise).round());
}

#[test]
fn the_physical_reach_gives_the_standoff_on_that_model() {
    let mut w = scene("puma");
    let cat = catalog().reaching(Reaches::Physical);
    w.vehicles.get_mut(&uid("puma-1")).unwrap().platform = cat.get("puma").cloned();
    let p = identify::plan(&spec(None), &w, &cfg()).unwrap();
    let surface = cat.get("puma").unwrap().sensors[0].reach_m.as_ref().unwrap().surface.unwrap();
    assert_eq!(p.reach_m, (surface * identify::IDENTIFY_PX_SHARE).round());
    assert!(p.radius_m > 1_000.0, "{}", p.radius_m);
}

#[test]
fn an_operators_standoff_replaces_the_engines() {
    let p = identify::plan(&spec(Some(800.0)), &scene("puma"), &cfg()).unwrap();
    assert_eq!(p.radius_m, 800.0);
    assert!(p.set_by_order);
}

#[test]
fn the_order_is_refused_saying_why() {
    let why = |w: &WorldSnapshot, c: &RulesConfig| identify::plan(&spec(None), w, c).unwrap_err();
    // identified already: refused under rules 21; from rules 22 a second look is an order too
    let mut w = scene("puma");
    w.track_levels.get_mut("T-0001").unwrap().level = Level::Identify;
    let mut v21 = cfg();
    v21.rules_version = IDENTIFY_SINCE;
    assert!(why(&w, &v21).contains("is identified already"), "{}", why(&w, &v21));
    assert!(identify::plan(&spec(None), &w, &cfg()).is_ok());
    // rules 22: a contact out of view is not live
    let mut w = scene("puma");
    w.t = T0.plus_ms(30_000);
    assert!(why(&w, &cfg()).contains("is out of view"), "{}", why(&w, &cfg()));
    // not in the picture
    let mut w = scene("puma");
    w.tracks.clear();
    assert!(why(&w, &cfg()).contains("is not in the picture"));
    // a radar-only aircraft
    assert!(why(&scene("mq9b_radar"), &cfg()).contains("carries no EO/IR"));
    // older rules take no identify order
    let mut old = cfg();
    old.rules_version = MUNITION_FLIGHT_SINCE;
    assert!(why(&scene("puma"), &old).contains("take no identify order"));
    // the rule says so, and the vehicle rules on the orbit do not run without a plan
    let mut w = scene("puma");
    w.track_levels.get_mut("T-0001").unwrap().level = Level::Identify;
    let checks = rules::evaluate(&spec(None), &w, &v21);
    let r = checks.iter().find(|c| c.rule == RuleId::Identify).unwrap();
    assert_eq!(r.outcome, Outcome::Fail);
    assert!(!checks.iter().any(|c| c.rule == RuleId::Geofence));
}

#[test]
fn a_planned_identify_is_judged_as_its_orbit() {
    let checks = rules::evaluate(&spec(None), &scene("puma"), &cfg());
    for rule in [RuleId::Capability, RuleId::Identify, RuleId::Readiness, RuleId::PlatformLimits, RuleId::Domain, RuleId::Geofence, RuleId::Reserves] {
        let c = checks.iter().find(|c| c.rule == rule).unwrap_or_else(|| panic!("{rule:?} in {checks:?}"));
        assert_eq!(c.outcome, Outcome::Pass, "{c:?}");
    }
    // an operator's standoff below the platform's tightest orbit fails its limit, as an orbit's would
    let checks = rules::evaluate(&spec(Some(20.0)), &scene("puma"), &cfg());
    assert!(checks.iter().any(|c| c.rule == RuleId::PlatformLimits && c.outcome == Outcome::Fail));
}

#[test]
fn the_look_ends_identified_lost_or_out_of_time() {
    let deadline = T0.plus_ms(60_000);
    let w = scene("puma");
    assert_eq!(identify::outcome("T-0001", &w, deadline, None), None);
    // rules 22: only a fresh identification by our own sensors, after the order went out
    let sent = T0.plus_ms(1_000);
    let second = |before| Some(identify::Look { sent_t: sent, before, own: None, meanwhile: &[] });
    let mut old = w.clone();
    old.track_levels.insert(
        "T-0001".into(),
        TrackLevel { level: Level::Identify, by: vec!["1:ais".into()], own_t: None, conflict: None, own: Default::default() },
    );
    assert_eq!(identify::outcome("T-0001", &old, deadline, second(Some("ferry (identify)"))), None);
    let mut fresh = old.clone();
    fresh.track_levels.get_mut("T-0001").unwrap().own_t = Some(T0.plus_ms(5_000));
    fresh.track_levels.get_mut("T-0001").unwrap().conflict = Some("AIS says passenger but eo_ir classifies it as small_fast_boat".into());
    let (k, why) = identify::outcome("T-0001", &fresh, deadline, second(Some("ferry (identify)"))).unwrap();
    assert_eq!(k, EndKind::Identified);
    assert!(why.contains("it contradicts its identity before, ferry (identify)"), "{why}");
    // rules 23: only the vehicle sent's own identification; another's is said, not an end
    let puma = uid("puma-1");
    let noted = vec![("hunter-1".to_string(), Some("ferry".to_string()))];
    let own = |meanwhile| Some(identify::Look { sent_t: sent, before: Some("ferry (identify)"), own: Some(&puma), meanwhile });
    let mut other = fresh.clone();
    other.callsigns.insert(uid("puma-1"), "PUMA 1".into());
    other.callsigns.insert(uid("hunter-1"), "HUNTER 1".into());
    let l = other.track_levels.get_mut("T-0001").unwrap();
    l.by = vec!["hunter-1:eo_ir".into()];
    l.conflict = None;
    l.own.insert("hunter-1".into(), identify::OwnLook { t: T0.plus_ms(5_000), kind: Some("ferry".into()) });
    assert_eq!(identify::outcome("T-0001", &other, deadline, own(&[])), None);
    let due = identify::meanwhile_due("T-0001", &other, &puma, sent, &[]);
    assert_eq!(due, noted);
    assert_eq!(
        identify::meanwhile_detail(&other, "hunter-1", Some("ferry"), &puma),
        "HUNTER 1 identified it as ferry meanwhile; PUMA 1 continues for its own look"
    );
    assert!(identify::meanwhile_due("T-0001", &other, &puma, sent, &noted).is_empty(), "noted once");
    let mut then = other.clone();
    let l = then.track_levels.get_mut("T-0001").unwrap();
    l.by.push("puma-1:eo_ir".into());
    l.own.insert("puma-1".into(), identify::OwnLook { t: T0.plus_ms(9_000), kind: Some("ferry".into()) });
    let (k, why) = identify::outcome("T-0001", &then, deadline, own(&noted)).unwrap();
    assert_eq!(k, EndKind::Identified);
    assert_eq!(
        why,
        "track T-0001 identified as ferry by PUMA 1's eo_ir; it agrees with its identity before, ferry (identify); meanwhile HUNTER 1 identified it as ferry"
    );
    let mut late_too = other.clone();
    late_too.t = deadline;
    let (k, why) = identify::outcome("T-0001", &late_too, deadline, own(&noted)).unwrap();
    assert_eq!(k, EndKind::TimedOut);
    assert!(why.ends_with("by the order's deadline; meanwhile HUNTER 1 identified it as ferry"), "{why}");
    let mut done = w.clone();
    done.track_levels.insert(
        "T-0001".into(),
        TrackLevel {
            level: Level::Identify,
            by: vec!["puma-1:eo_ir".into()],
            own_t: Some(T0),
            conflict: None,
            own: Default::default(),
        },
    );
    let (k, why) = identify::outcome("T-0001", &done, deadline, None).unwrap();
    assert_eq!(k, EndKind::Identified);
    assert!(why.contains("by puma-1:eo_ir"), "{why}");
    let mut gone = w.clone();
    gone.tracks.clear();
    assert_eq!(identify::outcome("T-0001", &gone, deadline, None).unwrap().0, EndKind::Lost);
    let mut late = w.clone();
    late.t = deadline;
    assert_eq!(identify::outcome("T-0001", &late, deadline, None).unwrap().0, EndKind::TimedOut);
}

#[test]
fn the_orbit_moves_once_the_track_is_far_enough_from_its_centre() {
    let mut w = scene("puma");
    let p = identify::plan(&spec(None), &w, &cfg()).unwrap();
    assert_eq!(identify::recentre(&p, &w, &cfg()), None);
    let threshold = (identify::RECENTRE_SHARE * p.radius_m).max(identify::RECENTRE_MIN_M);
    let near = murmur_decide::engage::offset(BOAT, threshold - 5.0, 0.0);
    w.tracks.insert("T-0001".into(), track(near));
    assert_eq!(identify::recentre(&p, &w, &cfg()), None);
    let far = murmur_decide::engage::offset(BOAT, threshold + 5.0, 0.0);
    w.tracks.insert("T-0001".into(), track(far));
    assert_eq!(identify::recentre(&p, &w, &cfg()), Some(far));
}
