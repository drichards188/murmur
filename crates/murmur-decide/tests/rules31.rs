//! Rules 31 (bd murmur-8vj8, eval/SCORING-V3.md 17.11): an identify order's standoff sized by the
//! camera's reach of the size class the picture shows the track as (`identify::shown_class`), from
//! the catalog's physical reaches by class; the domain's reach as before where the picture shows
//! no size or the facts carry no reaches by class; and older rules, which log none of it.
#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use common::*;
use murmur_decide::identify::{self, IDENTIFY_PX_SHARE, Level, TrackLevel};
use murmur_decide::replay::{ReplayOptions, RulesAccepted, replay_str_with};
use murmur_decide::scenario::{self, RunOptions};
use murmur_decide::*;

/// Where the track holds, over the water west of the bridge, and where our vehicle is, 1 km north.
const AT: LonLat = LonLat::new(-122.4950, 37.8120);
const FROM: LonLat = LonLat::new(-122.4930, 37.8210);

fn track(identity: Option<&str>, domain: Domain) -> TrackFacts {
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
        identity: identity.map(str::to_string),
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

/// One of ours on `platform`, `alt` m up, and a track the picture shows as `identity` at `level`
/// (no altitude: a 2D radar's), on the sensing model's `reaches`.
fn scene(platform: &str, alt: f64, identity: Option<&str>, level: Option<Level>, reaches: Reaches) -> WorldSnapshot {
    let mut w = WorldSnapshot::new(T0);
    w.areas = Some(areas());
    let mut v = vehicle(platform, FROM);
    v.platform = catalog().reaching(reaches).get(platform).cloned();
    v.alt_msl_m = Some(Fact::new(alt, T0));
    w.vehicles.insert(uid("v-1"), v);
    w.tracks.insert("T-0001".into(), track(identity, Domain::Air));
    if let Some(level) = level {
        w.track_levels.insert(
            "T-0001".into(),
            TrackLevel { level, by: Vec::new(), own_t: None, conflict: None, own: Default::default() },
        );
    }
    w
}

fn drone(platform: &str, alt: f64) -> WorldSnapshot {
    scene(platform, alt, Some("small_air"), Some(Level::Detect), Reaches::Physical)
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
        grouping: Default::default(),
    }
}

fn rules(version: u32) -> RulesConfig {
    RulesConfig { rules_version: version, ..RulesConfig::default() }
}

/// A platform's camera reach of a contact of `class` and of its domain on the physical model, m.
fn reaches(platform: &str, class: SizeClass) -> (f64, f64) {
    let cat = catalog().reaching(Reaches::Physical);
    let eo = cat.get(platform).unwrap().sensors.iter().find(|s| s.kind == "eo_ir").unwrap().clone();
    (eo.reach_for_class(class).flatten().unwrap(), eo.reach_for(class.domain()).unwrap())
}

/// On a track the picture shows as `small_air`, each aircraft's orbit is sized by its camera's
/// reach of a small drone, an eighth of it as the identification reach, the orbit inside it; under
/// rules 30 by the domain's reference aircraft, several times farther out. At the heights the v3
/// fleets fly them (the MQ-9B at 3,000 m: its tightest orbit, within its camera's reach of one).
#[test]
fn a_small_air_track_is_planned_for_a_small_drone() {
    // (platform, height, the orbit now, the orbit under rules 30), m
    for (platform, alt, now, before) in [
        ("puma", 150.0, 788.0, 1_411.0),
        ("fvr90", 150.0, 921.0, 1_684.0),
        ("x10", 100.0, 420.0, 1_380.0),
        ("lta", 250.0, 1_110.0, 1_800.0),
        ("mq9b_armed", 3_000.0, 1_891.0, 8_947.0),
    ] {
        let w = drone(platform, alt);
        let (class, domain) = reaches(platform, SizeClass::SmallAir);
        let p = identify::plan(&spec(), &w, &rules(IDENTIFY_CLASS_SINCE)).unwrap();
        assert_eq!(p.size_class, Some(SizeClass::SmallAir), "{platform}");
        assert_eq!(p.reach_m, (class * IDENTIFY_PX_SHARE).round(), "{platform}");
        assert_eq!(p.radius_m, now, "{platform}");
        assert!(p.radius_m.hypot(alt) <= p.reach_m, "{platform}: inside its identification reach of a small drone");
        let old = identify::plan(&spec(), &w, &rules(NODE_LOSS_SINCE)).unwrap();
        assert_eq!(old.size_class, None, "{platform}");
        assert_eq!(old.reach_m, (domain * IDENTIFY_PX_SHARE).round(), "{platform}");
        assert_eq!(old.radius_m, before, "{platform}");
    }
    // a Puma's camera identifies a small drone within about 1 km by day (SCORING-V3.md 13.6): the
    // orbit's slant range from 150 m up is inside it
    let p = identify::plan(&spec(), &drone("puma", 150.0), &rules(IDENTIFY_CLASS_SINCE)).unwrap();
    assert!(p.radius_m.hypot(150.0) < 1_000.0);
    // the check says what the reach is for
    let checks = rules::evaluate(&spec(), &drone("puma", 150.0), &rules(IDENTIFY_CLASS_SINCE));
    let c = checks.iter().find(|c| c.rule == RuleId::Identify).unwrap();
    assert_eq!(c.outcome, Outcome::Pass);
    assert!(c.detail.contains("an orbit of 788 m, inside its EO/IR's identification reach of about 1145 m for a small_air contact"), "{}", c.detail);
}

/// The MQ-9B at its catalog operating height, 7,600 m, is refused an identify order on a small
/// drone (its height over the contact is beyond its camera's identification reach of one); under
/// rules 30 it was sent to an orbit 5.6 km out. A light aircraft from the same height it is sent to.
#[test]
fn the_mq9b_from_its_operating_height_is_refused_for_a_small_drone() {
    let w = drone("mq9b", 7_600.0);
    let why = identify::plan(&spec(), &w, &rules(IDENTIFY_CLASS_SINCE)).unwrap_err();
    assert_eq!(why, "its height over the contact, 7600 m, is beyond its camera's identification reach of about 4068 m for a small_air contact");
    let checks = rules::evaluate(&spec(), &w, &rules(IDENTIFY_CLASS_SINCE));
    let c = checks.iter().find(|c| c.rule == RuleId::Identify).unwrap();
    assert_eq!(c.outcome, Outcome::Fail);
    assert!(!rules::passed(&checks, TaskKind::Identify));
    assert_eq!(identify::plan(&spec(), &w, &rules(NODE_LOSS_SINCE)).unwrap().radius_m, 5_593.0);
    let aircraft = scene("mq9b", 7_600.0, Some("large_air"), Some(Level::Detect), Reaches::Physical);
    let p = identify::plan(&spec(), &aircraft, &rules(IDENTIFY_CLASS_SINCE)).unwrap();
    assert_eq!(p.size_class, Some(SizeClass::LargeAir));
    assert!(p.radius_m.hypot(7_600.0) <= p.reach_m);
}

/// The size class a track's shown identity says, read at its level: a detection's size class; ESM's
/// words for a domain no size; a recognition's family and an identification's kind their class;
/// with no level, any label but ESM's words; a class of another domain than the track's none.
#[test]
fn the_shown_identity_says_the_class() {
    let class = |label: Option<&str>, level: Option<Level>, domain: Domain| identify::shown_class(&track(label, domain), level);
    use Level::*;
    use SizeClass::*;
    for (label, level, domain, want) in [
        (Some("small_air"), Some(Detect), Domain::Air, Some(SmallAir)),
        (Some("aircraft"), Some(Detect), Domain::Air, None),
        (Some("vessel"), Some(Detect), Domain::Surface, None),
        (Some("submerged"), Some(Detect), Domain::Subsurface, None),
        (Some("aircraft"), Some(Recognise), Domain::Air, Some(LargeAir)),
        (Some("small_uas"), Some(Recognise), Domain::Air, Some(SmallAir)),
        (Some("small_craft"), Some(Recognise), Domain::Surface, Some(SmallSurface)),
        (Some("small_drone"), Some(Identify), Domain::Air, Some(SmallAir)),
        (Some("cargo_ship"), Some(Identify), Domain::Surface, Some(LargeSurface)),
        (Some("small_air"), None, Domain::Air, Some(SmallAir)),
        (Some("small_uas"), None, Domain::Air, Some(SmallAir)),
        (Some("aircraft"), None, Domain::Air, None),
        (Some("small_air"), Some(Detect), Domain::Surface, None),
        (Some("unknown"), Some(Detect), Domain::Air, None),
        (None, None, Domain::Air, None),
    ] {
        assert_eq!(class(label, level, domain), want, "{label:?} {level:?} {domain:?}");
    }
}

/// A track that shows no size (ESM's `aircraft`, no identity) keeps its domain's reach; so does any
/// track on the legacy model's facts, which carry no reaches by class.
#[test]
fn a_track_that_shows_no_size_keeps_its_domains_reach() {
    let (_, air) = reaches("puma", SizeClass::SmallAir);
    for w in [
        scene("puma", 150.0, Some("aircraft"), Some(Level::Detect), Reaches::Physical),
        scene("puma", 150.0, None, None, Reaches::Physical),
    ] {
        let p = identify::plan(&spec(), &w, &rules(IDENTIFY_CLASS_SINCE)).unwrap();
        assert_eq!((p.size_class, p.reach_m), (None, (air * IDENTIFY_PX_SHARE).round()));
        assert_eq!(p, identify::plan(&spec(), &w, &rules(NODE_LOSS_SINCE)).unwrap());
    }
    let legacy = scene("puma", 150.0, Some("small_air"), Some(Level::Detect), Reaches::Legacy);
    let p = identify::plan(&spec(), &legacy, &rules(IDENTIFY_CLASS_SINCE)).unwrap();
    assert_eq!((p.size_class, p.reach_m), (None, 500.0), "range_m 4,000 m, an eighth of it");
    assert_eq!(p, identify::plan(&spec(), &legacy, &rules(NODE_LOSS_SINCE)).unwrap());
}

/// A camera that reaches no contact of the track's class (the FPV's, which the physical model gives
/// no small-drone reach at all) is refused, saying so.
#[test]
fn a_camera_that_reaches_no_contact_of_the_class_is_refused() {
    let w = drone("fpv", 50.0);
    let why = identify::plan(&spec(), &w, &rules(IDENTIFY_CLASS_SINCE)).unwrap_err();
    assert_eq!(why, "fpv carries no EO/IR that reaches a contact the picture shows as small_air");
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Scenario 144 under rules 31 logs its camera's reaches by class and the class its orbit was
/// sized for, and replays; under rules 30 the engine drops the reaches by class from every fact it
/// logs (no build of rules 30 logged one), sizes the orbit by the domain, and the log replays under
/// rules 30.
#[test]
fn older_rules_log_no_reach_by_class() {
    let f = root().join("sim/scenarios/engage/144-identify-a-small-drone-close-in.toml");
    let now = scenario::run_file(&f, &root(), &RunOptions { rules_version: Some(IDENTIFY_CLASS_SINCE), ..RunOptions::default() }).unwrap();
    // the file's designed ends are rules 33's words; rules 31 ended it done, as its notes say
    assert_eq!(now.final_state.as_deref(), Some("done"));
    assert!(now.identify.iter().any(|l| l.contains("identified as small_drone by puma-1's eo_ir")), "{:?}", now.identify);
    assert!(now.replay_ok, "{:?}", now.replay_mismatches);
    assert!(now.log.contains("\"size_class\":{\"small_air\":9160.0"), "the reaches by class logged");
    assert!(now.log.contains("\"size_class\":\"small_air\""), "the plan's class logged");
    let old = scenario::run_file(&f, &root(), &RunOptions { rules_version: Some(NODE_LOSS_SINCE), ..RunOptions::default() }).unwrap();
    assert!(!old.log.contains("size_class"), "rules 30 logs no reach by class");
    assert!(old.log.contains("\"reach_m\":{\"air\":16210.0"), "but the physical reaches by domain");
    assert!(old.log.contains("\"radius_m\":1411.0"), "the orbit sized by the domain");
    let opts = ReplayOptions { rules: RulesAccepted::Exactly(NODE_LOSS_SINCE), ..ReplayOptions::default() };
    let r = replay_str_with(&old.log, &opts).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}
