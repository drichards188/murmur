//! Rules 32 (bd murmur-ahss): a sensor's reach judged under the conditions where its vehicle is now
//! (the vehicle fact `reach_now`, the bridge's `tracks::reach`): an identify order's standoff and
//! refusal read its camera's identification reach now, an engagement's assessment each sensor's
//! detection reach now; older rules and the legacy model's facts decide as before, and the engine
//! logs the fact only where a decision reads it, under rules 32.
#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use common::*;
use murmur_decide::identify::{self, Level, TrackLevel};
use murmur_decide::replay::{ReplayOptions, RulesAccepted, replay_str_with};
use murmur_decide::scenario::{self, RunOptions};
use murmur_decide::*;

/// Where the track holds, over the water west of the bridge, and where our vehicle is, 1 km north.
const AT: LonLat = LonLat::new(-122.4950, 37.8120);
const FROM: LonLat = LonLat::new(-122.4930, 37.8210);

fn track(identity: &str, domain: Domain) -> TrackFacts {
    TrackFacts {
        position: Fact::new(AT, T0),
        alt_m: None,
        alt_t: None,
        velocity_mps: Some([0.0, 0.0]),
        pos_sigma_m: 15.0,
        sigma_growth_mps: Some(1.0),
        confidence: 0.9,
        affiliation: Fact::new(Affiliation::Unknown, T0),
        affiliation_confidence: 0.8,
        identity: Some(identity.to_string()),
        identity_confidence: 0.99,
        sources: vec!["3:radar".into()],
        source_t: Default::default(),
        domain,
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

/// A Puma's camera's reach now, 150 m up, as the bridge computes it (the reach display's figures):
/// in fog of 50 m visibility, at night, and on a clear day.
fn fog() -> ReachNow {
    reach(50.0, 30.0, [113.0, 75.0, 77.0], [188.0, 166.0, 166.0], [160.0, 188.0, 166.0, 166.0, 165.0, 166.0], true)
}

fn night() -> ReachNow {
    reach(20_000.0, -20.0, [13_429.0, 10_153.0, 14_668.0], [1_730.0, 1_326.0, 2_883.0], [202.0, 2_306.0, 2_018.0, 4_612.0, 432.0, 1_326.0], true)
}

fn clear() -> ReachNow {
    reach(20_000.0, 45.0, [16_707.0, 14_755.0, 14_763.0], [9_767.0, 7_584.0, 12_849.0], [1_167.0, 12_200.0, 10_710.0, 14_357.0, 2_500.0, 7_584.0], true)
}

/// An EO/IR's reach now 150 m up: detection by air, ground, surface; identification by the same and
/// by class (small air, large air, small surface, large surface, small ground, large ground).
fn reach(visibility_m: f64, sun: f64, detect: [f64; 3], identify: [f64; 3], class: [f64; 6], thermal: bool) -> ReachNow {
    let by = |d: [f64; 3]| SensorReach { air: Some(d[0]), ground: Some(d[1]), surface: Some(d[2]), ..SensorReach::default() };
    let mut id = by(identify);
    id.size_class = Some(ClassReach {
        small_air: Some(class[0]),
        large_air: Some(class[1]),
        small_surface: Some(class[2]),
        large_surface: Some(class[3]),
        small_ground: Some(class[4]),
        large_ground: Some(class[5]),
    });
    ReachNow {
        alt_m: 150.0,
        visibility_m,
        sun_elevation_deg: sun,
        sensors: vec![SensorNow { kind: "eo_ir".into(), detect_m: by(detect), identify_m: Some(id), thermal }],
    }
}

/// PUMA 1 (uid v-1), 150 m up, its facts on `reaches`, its reach now `now` (stamped at T0), and a
/// track the picture shows as `identity` (a detection's size class) in `domain`.
fn scene(reaches: Reaches, now: Option<ReachNow>, identity: &str, domain: Domain) -> WorldSnapshot {
    let mut w = WorldSnapshot::new(T0);
    w.areas = Some(areas());
    let mut v = vehicle("puma", FROM);
    v.platform = catalog().reaching(reaches).get("puma").cloned();
    v.alt_msl_m = Some(Fact::new(150.0, T0));
    v.reach_now = now.map(|r| Fact::new(r, T0));
    w.vehicles.insert(uid("v-1"), v);
    w.callsigns.insert(uid("v-1"), "PUMA 1".into());
    w.tracks.insert("T-0001".into(), track(identity, domain));
    w.track_levels.insert(
        "T-0001".into(),
        TrackLevel { level: Level::Detect, by: Vec::new(), own_t: None, conflict: None, own: Default::default() },
    );
    w
}

fn spec() -> IntentSpec {
    IntentSpec {
        id: IntentId(1),
        created_t: T0,
        source: Source::Human { id: OperatorId::new("op-1") },
        task: Task::Identify { track: "T-0001".into(), standoff_m: None },
        uids: vec![uid("v-1")],
        constraints: Constraints::default(),
        rationale: None,
    }
}

fn rules(version: u32) -> RulesConfig {
    RulesConfig { rules_version: version, ..RulesConfig::default() }
}

/// In fog of 50 m PUMA 1's camera identifies a small drone only within about 160 m of slant range:
/// its tightest orbit at 150 m keeps it beyond that, and the order is refused saying the fog and the
/// reach; rules 31 planned it the clear day's 788 m orbit from the catalog. In 1.2 km of visibility
/// the reach now (882 m) sizes the orbit and the detail says the visibility. The conditions only
/// take reach away: on a clear day, where the camera's identification reach now (1,167 m) is more
/// than rules 31's eighth of its detection reach (1,145 m), and at night, where its LWIR's reach of
/// a small fast boat (2,018 m) is more than that (1,793 m), the plan is rules 31's.
#[test]
fn the_identify_plan_reads_the_cameras_reach_now() {
    let w = scene(Reaches::Physical, Some(fog()), "small_air", Domain::Air);
    let why = identify::plan(&spec(), &w, &rules(REACH_NOW_SINCE)).unwrap_err();
    assert_eq!(
        why,
        "in fog of 50 m visibility PUMA 1's camera identifies a small_air contact only within about 160 m: its tightest orbit, 66 m, keeps it beyond that"
    );
    assert_eq!(identify::plan(&spec(), &w, &rules(IDENTIFY_CLASS_SINCE)).unwrap().radius_m, 788.0);
    let checks = rules::evaluate(&spec(), &w, &rules(REACH_NOW_SINCE));
    assert!(!rules::passed(&checks, TaskKind::Identify));
    // thinning fog: the reach now sizes the orbit
    let mut thin = fog();
    thin.visibility_m = 1_200.0;
    thin.sensors[0].identify_m.as_mut().unwrap().size_class.as_mut().unwrap().small_air = Some(882.0);
    let w = scene(Reaches::Physical, Some(thin), "small_air", Domain::Air);
    let p = identify::plan(&spec(), &w, &rules(REACH_NOW_SINCE)).unwrap();
    assert_eq!((p.reach_m, p.conditions.as_deref()), (882.0, Some("in 1.2 km visibility")));
    assert_eq!(p.radius_m, ((identify::STANDOFF_SHARE * 882.0).powi(2) - 150.0f64.powi(2)).sqrt().ceil());
    let c = rules::evaluate(&spec(), &w, &rules(REACH_NOW_SINCE)).into_iter().find(|c| c.rule == RuleId::Identify).unwrap();
    assert_eq!(c.outcome, Outcome::Pass);
    assert!(
        c.detail.contains(&format!("an orbit of {:.0} m, inside its EO/IR's identification reach of about 882 m for a small_air contact in 1.2 km visibility,", p.radius_m)),
        "{}",
        c.detail
    );
    // a clear day, and a boat at night on the LWIR: rules 31's plan
    for (now, identity, domain) in [(clear(), "small_air", Domain::Air), (night(), "small_surface", Domain::Surface)] {
        let w = scene(Reaches::Physical, Some(now), identity, domain);
        let p = identify::plan(&spec(), &w, &rules(REACH_NOW_SINCE)).unwrap();
        assert_eq!(p, identify::plan(&spec(), &w, &rules(IDENTIFY_CLASS_SINCE)).unwrap(), "{identity}");
        assert_eq!(p.conditions, None);
    }
}

/// A camera without a thermal channel identifies nothing at night: refused, saying so. One that
/// reaches no contact of a domain at all (under water) is refused as before.
#[test]
fn a_camera_that_identifies_nothing_now_is_refused_saying_why() {
    let mut dark = night();
    for s in &mut dark.sensors {
        s.thermal = false;
        for r in std::iter::once(&mut s.detect_m).chain(s.identify_m.as_mut()) {
            *r = SensorReach { air: Some(0.0), ground: Some(0.0), surface: Some(0.0), size_class: r.size_class.as_ref().map(|_| ClassReach {
                small_air: Some(0.0),
                large_air: Some(0.0),
                small_surface: Some(0.0),
                large_surface: Some(0.0),
                small_ground: Some(0.0),
                large_ground: Some(0.0),
            }), ..SensorReach::default() };
        }
    }
    let w = scene(Reaches::Physical, Some(dark), "small_surface", Domain::Surface);
    assert_eq!(
        identify::plan(&spec(), &w, &rules(REACH_NOW_SINCE)).unwrap_err(),
        "at night PUMA 1's camera identifies no small_surface contact from where it is now, 150 m up: it has no thermal channel"
    );
    // a domain no camera sees: as before
    let w = scene(Reaches::Physical, Some(clear()), "submerged", Domain::Subsurface);
    assert_eq!(identify::plan(&spec(), &w, &rules(REACH_NOW_SINCE)).unwrap_err(), "puma carries no EO/IR that reaches a contact in the subsurface domain");
}

/// Without a reach now, or with one older than the vehicle's freshness limit, the plan cannot be
/// evaluated (never a clear day's reach in its place).
#[test]
fn a_missing_or_stale_reach_now_cannot_be_evaluated() {
    let w = scene(Reaches::Physical, None, "small_air", Domain::Air);
    assert_eq!(
        identify::plan(&spec(), &w, &rules(REACH_NOW_SINCE)).unwrap_err(),
        "cannot evaluate: no sensor reach under the conditions now reported"
    );
    let mut w = scene(Reaches::Physical, None, "small_air", Domain::Air);
    w.vehicles.get_mut(&uid("v-1")).unwrap().reach_now = Some(Fact::new(clear(), T0.plus_ms(-30_000)));
    let why = identify::plan(&spec(), &w, &rules(REACH_NOW_SINCE)).unwrap_err();
    assert!(why.starts_with("cannot evaluate: sensor reach under the conditions now is 30.0 s old"), "{why}");
}

/// The legacy model's facts carry no physical reach: under rules 32 they decide as under rules 31,
/// whatever reach now the facts carry.
#[test]
fn the_legacy_models_facts_decide_as_before() {
    for now in [None, Some(fog())] {
        let w = scene(Reaches::Legacy, now, "small_air", Domain::Air);
        let p = identify::plan(&spec(), &w, &rules(REACH_NOW_SINCE)).unwrap();
        assert_eq!(p, identify::plan(&spec(), &w, &rules(IDENTIFY_CLASS_SINCE)).unwrap());
        assert_eq!(p.reach_m, 500.0, "range_m 4,000 m, an eighth of it");
    }
}

/// The assessment's cover: under rules 32 a sensor with a physical reach covers within its
/// detection reach now and nowhere while that is not known; under rules 31 its clear-day reach.
#[test]
fn the_assessment_reads_each_sensors_detection_reach_now() {
    let w = |now: Option<ReachNow>| scene(Reaches::Physical, now, "small_surface", Domain::Surface);
    let cover = |w: &WorldSnapshot, d: f64, version: u32| {
        engage::covering_sensor(&w.vehicles[&uid("v-1")], Domain::Surface, d, w, &rules(version)).map(|s| s.kind.clone())
    };
    let foggy = w(Some(fog()));
    assert_eq!(cover(&foggy, 70.0, REACH_NOW_SINCE).as_deref(), Some("eo_ir"));
    assert_eq!(cover(&foggy, 1_450.0, REACH_NOW_SINCE), None, "77 m in the fog");
    assert_eq!(cover(&foggy, 1_450.0, IDENTIFY_CLASS_SINCE).as_deref(), Some("eo_ir"), "the clear day's 14,470 m");
    assert_eq!(cover(&w(None), 70.0, REACH_NOW_SINCE), None, "not known: no cover");
    let legacy = scene(Reaches::Legacy, Some(fog()), "small_surface", Domain::Surface);
    assert_eq!(cover(&legacy, 1_450.0, REACH_NOW_SINCE).as_deref(), Some("eo_ir"), "range_m 4,000 m");
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The engine logs a reach now only where a decision reads it (an identify order's decisions, an
/// assessment's observation steps), under rules 32 only; each such log replays, and one relabelled
/// rules 31 is caught for the fact no build of rules 31 logged.
#[test]
fn the_reach_now_is_logged_where_it_is_read_and_under_rules_32_only() {
    let f = root().join("sim/scenarios/engage/146-identify-in-fog-refused-until-it-lifts.toml");
    let now = scenario::run_file(&f, &root(), &RunOptions::default()).unwrap();
    assert!(now.ok, "{:?}", now.failures);
    assert!(now.replay_ok, "{:?}", now.replay_mismatches);
    assert!(now.log.contains("\"reach_now\":{\"value\":{\"alt_m\":150.0,\"visibility_m\":50.0"));
    assert!(now.log.contains("\"callsigns\":{\"puma-1\":\"PUMA 1\"}"), "the refusal's callsign logged");
    let old = scenario::run_file(&f, &root(), &RunOptions { rules_version: Some(IDENTIFY_CLASS_SINCE), ..RunOptions::default() }).unwrap();
    assert!(!old.log.contains("reach_now"), "rules 31 logs no reach now");
    assert!(!old.log.contains("\"callsigns\""), "nor the callsign it does not word");
    let opts = ReplayOptions { rules: RulesAccepted::Exactly(IDENTIFY_CLASS_SINCE), ..ReplayOptions::default() };
    let r = replay_str_with(&old.log, &opts).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    // an engagement: only its observation steps read and log the reach now
    let f = root().join("sim/scenarios/engage/148-assessment-in-fog-sees-nothing.toml");
    let eng = scenario::run_file(&f, &root(), &RunOptions::default()).unwrap();
    assert!(eng.ok, "{:?}", eng.failures);
    assert_eq!(eng.assessment.as_deref(), Some("unknown"));
    let with_reach: Vec<&str> = eng.log.lines().filter(|l| l.contains("\"reach_now\"")).collect();
    assert!(!with_reach.is_empty());
    assert!(with_reach.iter().all(|l| l.contains("\"type\":\"engage.observe\"")), "only the observations log it");
    // relabelled rules 31, the log carries what no build of rules 31 logged
    let v32 = format!("\"rules_version\":{REACH_NOW_SINCE},");
    let relabelled = now.log.replacen(&v32, &format!("\"rules_version\":{IDENTIFY_CLASS_SINCE},"), 1);
    assert_ne!(relabelled, now.log);
    let r = replay_str_with(&relabelled, &opts).unwrap();
    assert!(
        r.mismatches.iter().any(|m| m.what.starts_with("a vehicle's sensors' reach under the conditions now")),
        "{:?}",
        r.mismatches
    );
}
