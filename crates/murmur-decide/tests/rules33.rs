//! Rules 33, stage S3 of docs/SCENARIO-SETTINGS.md (`IDENTIFY_CRITERIA_SINCE`,
//! `ENGAGE_IDENTITY_LEVEL_SINCE`): the engine reads the scenario's settings. An identify order ends
//! only on the vehicle sent's own look that meets its track's domain criteria (a naming below them
//! is logged and the order goes on), its words say the look's confidence and the bar, its time on
//! station is the policy's; an engagement's target must show its identity at the identification
//! level as well as the bar; out of view is the policy's `stale_s`, a silent vehicle's drift its
//! `[rules.silent]`. Under rules 32 every one of them decides, and says, what it always did.
#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use common::*;
use murmur_decide::identify::{self, EndKind, Level, OwnLook, Shortfall, TrackLevel};
use murmur_decide::replay::{ReplayOptions, RulesAccepted, replay_str_with};
use murmur_decide::scenario::{self, RunOptions};
use murmur_decide::*;

const AT: LonLat = LonLat::new(-122.4950, 37.8120);

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn rules(version: u32) -> RulesConfig {
    RulesConfig { rules_version: version, ..RulesConfig::default() }
}

fn track(identity: &str, domain: Domain, last_seen_t: Time) -> TrackFacts {
    TrackFacts {
        position: Fact::new(AT, last_seen_t),
        alt_m: None,
        alt_t: None,
        velocity_mps: Some([0.0, 0.0]),
        pos_sigma_m: 15.0,
        sigma_growth_mps: Some(1.0),
        confidence: 0.9,
        affiliation: Fact::new(Affiliation::Unknown, last_seen_t),
        affiliation_confidence: 0.8,
        identity: Some(identity.to_string()),
        identity_confidence: 0.9,
        sources: vec!["puma-1:eo_ir".into()],
        source_t: Default::default(),
        domain,
        last_seen_t,
        declared_by: None,
        declaration: None,
        review_flags: Vec::new(),
        manoeuvre: None,
        mean_velocity_mps: None,
        first_seen_t: None,
        velocity_sigma_mps: None,
    }
}

fn look(sensor: &str, level: Level, confidence: f64, t: Time) -> OwnLook {
    OwnLook { t, kind: Some("small_fast_boat".into()), sensor: Some(sensor.into()), level: Some(level), confidence: Some(confidence) }
}

/// PUMA 1's own look at a surface track T-0001, the picture at `t`.
fn scene(own: OwnLook, conflict: Option<&str>, t: Time) -> WorldSnapshot {
    let mut w = WorldSnapshot::new(t);
    w.tracks.insert("T-0001".into(), track("small_fast_boat", Domain::Surface, t));
    w.callsigns.insert(uid("puma-1"), "PUMA 1".into());
    w.track_levels.insert(
        "T-0001".into(),
        TrackLevel {
            level: own.level.unwrap_or(Level::Identify),
            by: vec!["puma-1:eo_ir".into()],
            own_t: Some(own.t),
            conflict: conflict.map(str::to_string),
            own: [("puma-1".to_string(), own)].into(),
        },
    );
    w
}

/// Each criterion in its turn: the level, the sensor (AIS never), the bar (met at equality), the
/// conflict; a look without its details meets nothing.
#[test]
fn an_own_look_meets_its_domains_criteria_or_says_which_it_falls_short_of() {
    let w = WatchSettings::default();
    let short = |o: &OwnLook, d: Domain, c: Option<&str>| identify::shortfall(o, d, &w, c);
    let t = T0;
    assert_eq!(short(&look("eo_ir", Level::Identify, 0.6, t), Domain::Surface, None), None, "the bar met at equality");
    assert_eq!(short(&look("eo_ir", Level::Identify, 0.59, t), Domain::Surface, None), Some(Shortfall::Bar));
    assert_eq!(short(&look("eo_ir", Level::Recognise, 0.9, t), Domain::Surface, None), Some(Shortfall::Level));
    assert_eq!(short(&look("sonar", Level::Identify, 0.9, t), Domain::Surface, None), Some(Shortfall::Sensor));
    // AIS is among the surface domain's sensors, and never ends an identify order
    assert!(w.domain.surface.sensors.iter().any(|s| s == "ais"));
    assert_eq!(short(&look("ais", Level::Identify, 0.99, t), Domain::Surface, None), Some(Shortfall::Sensor));
    assert_eq!(
        short(&look("eo_ir", Level::Identify, 0.9, t), Domain::Surface, Some("AIS says ferry")),
        Some(Shortfall::Conflict("AIS says ferry".into()))
    );
    // under water a recognition by sonar is the criterion
    assert_eq!(short(&look("sonar", Level::Recognise, 0.7, t), Domain::Subsurface, None), None);
    assert_eq!(short(&look("eo_ir", Level::Identify, 0.9, t), Domain::Subsurface, None), Some(Shortfall::Sensor));
    let bare = OwnLook { t, kind: Some("small_fast_boat".into()), ..OwnLook::default() };
    assert_eq!(short(&bare, Domain::Surface, None), Some(Shortfall::Level));
    assert_eq!(identify::criteria_words(&w, Domain::Surface), "the bar is 0.6, identification by EO/IR");
    assert_eq!(identify::criteria_words(&w, Domain::Subsurface), "the bar is 0.6, recognition by sonar");
}

/// The outcome under rules 33: a naming below the bar is a `named` step's words and no end; at the
/// bar it ends, saying the confidence and the criteria; the scenario's bar is the one said; at the
/// deadline the timeout names the best look. The same pictures under rules 32 end at once.
#[test]
fn the_outcome_reads_the_scenarios_criteria_and_says_them() {
    let sent = T0;
    let deadline = T0.plus_ms(400_000);
    let now = T0.plus_ms(30_000);
    let puma = uid("puma-1");
    let cfg = rules(SCENARIO_SETTINGS_SINCE);
    let run = |named: Vec<identify::Noted>| identify::Run {
        plan: identify::Plan {
            track: "T-0001".into(),
            center: AT,
            radius_m: 400.0,
            reach_m: 500.0,
            size_class: None,
            conditions: None,
            set_by_order: false,
            transit_s: 60.0,
        },
        deadline_t: deadline,
        resumes: None,
        held: None,
        ended: None,
        sent_t: Some(sent),
        before: Some("small_craft (recognise)".into()),
        meanwhile: Vec::new(),
        named,
    };
    let first = scene(look("eo_ir", Level::Identify, 0.11, now), None, now);
    let r = run(Vec::new());
    let k = identify::Look::of(&r, &puma, &cfg).unwrap();
    assert_eq!(identify::outcome("T-0001", &first, deadline, Some(k)), None, "it goes on");
    assert_eq!(
        identify::named_due("T-0001", &first, &k),
        Some((
            "puma-1".into(),
            Some("small_fast_boat".into()),
            "PUMA 1 named it small_fast_boat at 0.11, not yet identified (the bar is 0.6); it goes on".into()
        ))
    );
    // noted once for its kind
    let r = run(vec![identify::Noted { by: "puma-1".into(), kind: Some("small_fast_boat".into()), t: now }]);
    let k = identify::Look::of(&r, &puma, &cfg).unwrap();
    assert_eq!(identify::named_due("T-0001", &first, &k), None);
    let sure = scene(look("eo_ir", Level::Identify, 0.71, now), None, now);
    assert_eq!(
        identify::outcome("T-0001", &sure, deadline, Some(k)),
        Some((
            EndKind::Identified,
            "track T-0001 identified as small_fast_boat at 0.71 by PUMA 1's eo_ir (the bar is 0.6, identification by EO/IR); it was small_craft (recognise) before".into()
        ))
    );
    // the scenario's bar is the one read and said
    let mut strict = cfg.clone();
    strict.watch.bar = 0.75;
    let k75 = identify::Look::of(&r, &puma, &strict).unwrap();
    assert_eq!(identify::outcome("T-0001", &sure, deadline, Some(k75)), None);
    let r0 = run(Vec::new());
    let k75 = identify::Look::of(&r0, &puma, &strict).unwrap();
    assert_eq!(
        identify::named_due("T-0001", &sure, &k75).map(|n| n.2),
        Some("PUMA 1 named it small_fast_boat at 0.71, not yet identified (the bar is 0.75); it goes on".into())
    );
    // at the deadline: the best look of its own
    let mut late = scene(look("eo_ir", Level::Identify, 0.45, now), None, deadline);
    late.t = deadline;
    assert_eq!(
        identify::outcome("T-0001", &late, deadline, Some(k)),
        Some((
            EndKind::TimedOut,
            "track T-0001 not identified by the order's deadline; PUMA 1's best look named it small_fast_boat at 0.45 by its eo_ir (the bar is 0.6, identification by EO/IR)".into()
        ))
    );
    // a look from before the order went out is none of its own
    let mut stale = scene(look("eo_ir", Level::Identify, 0.9, T0.plus_ms(-5_000)), None, deadline);
    stale.t = deadline;
    let (kind, why) = identify::outcome("T-0001", &stale, deadline, Some(k)).unwrap();
    assert_eq!(kind, EndKind::TimedOut);
    assert!(why.contains("PUMA 1 had no look of its own at it since the order went out"), "{why}");
    // the progress the tools show, read-only
    let p = identify::progress("T-0001", &first, &r0, &puma, &cfg, "PUMA 1").unwrap();
    assert!(!p.identified);
    assert_eq!(
        p.words,
        "PUMA 1's best look so far named it small_fast_boat at 0.11 by its eo_ir, not yet identified (below the bar; the bar is 0.6, identification by EO/IR)"
    );
    let p = identify::progress("T-0001", &sure, &r0, &puma, &cfg, "PUMA 1").unwrap();
    assert!(p.identified);
    assert_eq!(p.words, "PUMA 1's best look named it small_fast_boat at 0.71 by its eo_ir: identified (the bar is 0.6, identification by EO/IR)");
    assert_eq!(identify::progress("T-0001", &sure, &r0, &puma, &rules(REACH_NOW_SINCE), "PUMA 1"), None, "none under rules 32");
    // rules 32: no criteria, the first naming ends it (the F8 boat)
    let old = rules(REACH_NOW_SINCE);
    let k32 = identify::Look::of(&r0, &puma, &old).unwrap();
    assert!(k32.criteria.is_none());
    assert_eq!(
        identify::outcome("T-0001", &first, deadline, Some(k32)).unwrap(),
        (EndKind::Identified, "track T-0001 identified as small_fast_boat by PUMA 1's eo_ir; it was small_craft (recognise) before".into())
    );
}

/// The time on station is the policy's `identify_on_station_s` from rules 33, and 300 s before
/// whatever the settings say.
#[test]
fn the_time_on_station_is_the_scenarios() {
    let plan = identify::Plan {
        track: "T-0001".into(),
        center: AT,
        radius_m: 400.0,
        reach_m: 500.0,
        size_class: None,
        conditions: None,
        set_by_order: false,
        transit_s: 60.0,
    };
    let mut cfg = rules(SCENARIO_SETTINGS_SINCE);
    cfg.watch.identify_on_station_s = 120.0;
    assert_eq!(identify::deadline(T0, &plan, &cfg), T0.plus_ms(180_000));
    cfg.rules_version = REACH_NOW_SINCE;
    assert_eq!(identify::deadline(T0, &plan, &cfg), T0.plus_ms(360_000));
}

/// Out of view after the policy's `stale_s` from rules 33; after 10 s before, whatever it says.
#[test]
fn out_of_view_is_the_scenarios_stale_time() {
    let mut cfg = rules(SCENARIO_SETTINGS_SINCE);
    cfg.watch.stale_s = 13.0;
    let t = track("small_fast_boat", Domain::Surface, T0);
    let ec = cfg.engage_rules();
    assert!(!engage::out_of_view(&t, T0.plus_ms(12_000), &ec));
    assert!(engage::out_of_view(&t, T0.plus_ms(13_000), &ec));
    cfg.rules_version = REACH_NOW_SINCE;
    let ec = cfg.engage_rules();
    assert!(engage::out_of_view(&t, T0.plus_ms(10_000), &ec), "10 s under rules 32");
    assert!(!engage::out_of_view(&t, T0.plus_ms(9_999), &ec));
}

/// A silent vehicle's drift is the policy's `[rules.silent] drift_mps` from rules 33; the constants
/// before.
#[test]
fn a_silent_vehicles_drift_is_the_scenarios() {
    let boat = vehicle("seahunter", BAY);
    let mut cfg = rules(SCENARIO_SETTINGS_SINCE);
    assert_eq!(silent::carried_mps(&boat, &cfg), silent::SURFACE_DRIFT_MPS, "the standing values are today's");
    let mut s = silent::SilentRules::default();
    s.drift_mps.surface = 1.2;
    cfg.silent = Some(s);
    assert_eq!(silent::carried_mps(&boat, &cfg), 1.2);
    cfg.rules_version = REACH_NOW_SINCE;
    assert_eq!(silent::carried_mps(&boat, &cfg), silent::SURFACE_DRIFT_MPS, "rules 32 read the constants");
}

/// What a picture's label says of its level: the scenario runner's level for a track its file
/// names without one (the engagement check reads it from rules 33).
#[test]
fn a_labels_level() {
    assert_eq!(identify::label_level("small_air"), Some(Level::Detect));
    assert_eq!(identify::label_level("small_craft"), Some(Level::Recognise));
    assert_eq!(identify::label_level("small_fast_boat"), Some(Level::Identify));
    assert_eq!(identify::label_level("airliner"), None);
}

fn run(name: &str, version: Option<u32>) -> scenario::ScenarioResult {
    let f = root().join("sim/scenarios/engage").join(name);
    scenario::run_file(&f, &root(), &RunOptions { rules_version: version, ..RunOptions::default() }).unwrap()
}

/// Scenarios 149 to 157 end as designed under rules 33 (asked for by name) and under the current
/// rules, each replaying and its tamper caught.
#[test]
fn the_rules_33_scenarios_end_as_designed() {
    for name in [
        "149-identify-goes-on-past-a-first-naming.toml",
        "150-identify-a-submerged-contact-by-sonar.toml",
        "151-ais-never-ends-an-identify.toml",
        "152-a-conflict-holds-an-identify.toml",
        "153-identify-timeout-names-the-best-look.toml",
        "154-size-class-is-not-an-identification.toml",
        "155-identified-below-the-bar.toml",
        "156-a-stricter-bar-holds-both.toml",
        "157-a-named-loosening-and-its-name-dropped.toml",
    ] {
        let r = run(name, Some(SCENARIO_SETTINGS_SINCE));
        assert!(r.ok, "{name}: {:?}", r.failures);
        assert!(r.replay_ok, "{name}: {:?}", r.replay_mismatches);
        assert!(r.log.contains(&format!("\"rules_version\":{SCENARIO_SETTINGS_SINCE},")), "{name}");
        let r = run(name, None);
        assert!(r.ok && r.replay_ok, "{name} under rules {RULES_VERSION}: {:?} {:?}", r.failures, r.replay_mismatches);
    }
}

/// The engagement's identity check names the level, the confidence and the bar from rules 33; a
/// size class at 0.93 passed it under rules 32.
#[test]
fn the_engagement_identity_check_names_the_level_and_the_bar() {
    let r = run("154-size-class-is-not-an-identification.toml", None);
    assert_eq!(
        r.final_reason.as_deref(),
        Some("target_identity: T-0001 identity small_air at 0.93 is a detection (its size class), not an identification: an engagement needs identification at 0.6 or more")
    );
    let old = run("154-size-class-is-not-an-identification.toml", Some(REACH_NOW_SINCE));
    assert_eq!(old.final_state.as_deref(), Some("pending_approval"), "a size class at 0.93 passed rules 32");
    let r = run("155-identified-below-the-bar.toml", None);
    assert_eq!(
        r.final_reason.as_deref(),
        Some("target_identity: T-0001 identity small_fast_boat at 0.55 is below the bar, 0.6: an engagement needs identification at 0.6 or more")
    );
    let old = run("155-identified-below-the-bar.toml", Some(REACH_NOW_SINCE));
    assert_eq!(old.final_reason.as_deref(), Some("target_identity: T-0001 identity small_fast_boat at 0.55 is below 0.60"));
    let r = run("156-a-stricter-bar-holds-both.toml", None);
    assert!(r.final_reason.as_deref().is_some_and(|w| w.ends_with("at 0.70 is below the bar, 0.75: an engagement needs identification at 0.75 or more")));
    // a passing check says what it passed on
    let r = run("157-a-named-loosening-and-its-name-dropped.toml", None);
    assert!(
        r.log.contains("T-0001 identity small_fast_boat at 0.55, an identification (its kind), at or above the bar of 0.5, existence"),
        "the passing check's words"
    );
}

/// An own look's details are logged with the picture under rules 33 only: the same timeline under
/// rules 32 logs none (its pictures hash as they always did) and ends on the first naming; a log of
/// rules 33 relabelled 32 is caught for what no build of rules 32 logged.
#[test]
fn an_own_looks_details_are_logged_under_rules_33_only() {
    let now = run("149-identify-goes-on-past-a-first-naming.toml", Some(SCENARIO_SETTINGS_SINCE));
    assert!(now.log.contains("\"sensor\":\"eo_ir\",\"level\":\"identify\",\"confidence\":0.11"), "the naming's look");
    assert!(now.log.contains("\"step\":\"named\""));
    let old = run("149-identify-goes-on-past-a-first-naming.toml", Some(REACH_NOW_SINCE));
    assert!(!old.log.contains("\"sensor\":"), "rules 32 log no detail");
    assert!(!old.log.contains("\"step\":\"named\""));
    assert!(old.identify.iter().any(|l| l.starts_with("t=30 ") && l.contains("ended done")), "{:?}", old.identify);
    let opts = ReplayOptions { rules: RulesAccepted::Exactly(REACH_NOW_SINCE), ..ReplayOptions::default() };
    assert!(replay_str_with(&old.log, &opts).unwrap().ok());
    let v33 = format!("\"rules_version\":{SCENARIO_SETTINGS_SINCE},");
    let relabelled = now.log.replacen(&v33, &format!("\"rules_version\":{REACH_NOW_SINCE},"), 1);
    assert_ne!(relabelled, now.log);
    let r = replay_str_with(&relabelled, &opts).unwrap();
    for what in ["the sensor of a vehicle's own look", "an identify order's naming below the scenario's criteria"] {
        assert!(r.mismatches.iter().any(|m| m.what.starts_with(what)), "{what}: {:?}", r.mismatches);
    }
}

/// A scenario designed before rules 33 that it changes carries its rules 33 design (the `[since.33]`
/// tables of the staged build, written into the files when rules 33 became the default), and says
/// in its notes what rules 32 did: 117's second look, contradicting the boat's AIS, ends it done
/// under rules 32 and holds it on the conflict until its deadline under rules 33.
#[test]
fn a_scenario_rules_33_changed_ends_as_its_notes_say_under_rules_32() {
    let name = "117-second-look-contradicts-ais.toml";
    let now = run(name, None);
    assert!(now.ok, "{:?}", now.failures);
    assert_eq!(now.final_state.as_deref(), Some("failed"), "rules 33 hold it on the conflict until its deadline");
    let old = run(name, Some(REACH_NOW_SINCE));
    assert_eq!(old.final_state.as_deref(), Some("done"), "rules 32 end it on the contradicting look");
    assert!(
        old.identify.iter().any(|l| l.contains("identified as patrol_boat by puma-1's eo_ir; it contradicts its identity before")),
        "{:?}",
        old.identify
    );
}
