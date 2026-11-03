//! Decision rules 28 (bd murmur-9001, the owner's direction of 2026-11-03): a missile on a ground
//! or surface platform is launched from a firing position within its range band of the target
//! (its minimum range plus the effect radius, to its maximum range less it), as rules 26 gave a
//! gun one, instead of being driven onto the effect point. Scenarios 131 (the armed UGV's Javelin
//! from the shore at a boat: launched where it stands), 132 (a truck inside the band: the UGV opens
//! to the near edge first) and 133 (a boat beyond the Javelin's range: the UGV closes to the far
//! edge, and no further).
#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use common::*;
use murmur_decide::audit::{EventBody, parse_log};
use murmur_decide::engage::{EngagePreview, MunitionFlight};
use murmur_decide::geo::haversine_m;
use murmur_decide::replay::{ReplayOptions, RulesAccepted, replay_str, replay_str_with};
use murmur_decide::scenario::{self, RunOptions, ScenarioResult};
use murmur_decide::*;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(name: &str, opts: &RunOptions) -> ScenarioResult {
    scenario::run_file(&root().join("sim/scenarios/engage").join(name), &root(), opts).unwrap()
}

const SHORE: &str = "131-missile-from-the-shore.toml";
const OPENS: &str = "132-missile-opens-to-its-minimum-range.toml";
const CLOSES: &str = "133-missile-closes-to-its-range.toml";

/// The plan the engagement was approved on, and the missile's flight at its release.
fn plan_and_flight(log: &str) -> (EngagePreview, MunitionFlight) {
    let events = parse_log(log).unwrap();
    let plan = events
        .iter()
        .find_map(|e| match &e.body {
            EventBody::ApprovalGranted { snapshot: Some(s), .. } => Some(s.preview.clone()),
            _ => None,
        })
        .unwrap();
    let flight = events
        .iter()
        .find_map(|e| match &e.body {
            EventBody::EngageEffect { munition: Some(m), .. } => Some(m.clone()),
            _ => None,
        })
        .unwrap();
    (plan, flight)
}

/// The scenarios end as designed and replay.
#[test]
fn the_missile_standoff_scenarios_end_as_designed_and_replay() {
    for name in [SHORE, OPENS, CLOSES] {
        let r = run(name, &RunOptions::default());
        assert!(r.ok, "{name}: {:?}", r.failures);
        assert!(r.replay_ok, "{name}: {:?}", r.replay_mismatches);
    }
}

/// The firing position: where the UGV stands when the point is inside the band (131), the band's
/// near edge when it is too close (132), its far edge when it is too far (133); the path ends there
/// and the missile is launched from there.
#[test]
fn the_missile_is_launched_from_its_firing_position() {
    // 131: 1,450 m from the boat, inside 140 to 4,675 m: where it stands
    let (plan, flight) = plan_and_flight(&run(SHORE, &RunOptions::default()).log);
    let from = plan.fire_from.unwrap();
    assert_eq!(from, plan.start);
    assert_eq!(plan.path.last(), Some(&from));
    assert!((haversine_m(from, plan.effect_point) - 1450.0).abs() < 10.0);
    assert_eq!(flight.launch_point, from);
    // 132: 55 m from the truck, inside the near edge (65 m + the 50 m effect radius): opens to 115 m
    let (plan, flight) = plan_and_flight(&run(OPENS, &RunOptions::default()).log);
    let from = plan.fire_from.unwrap();
    assert!((haversine_m(from, plan.effect_point) - 115.0).abs() < 1.0, "the near edge");
    assert!((haversine_m(plan.start, from) - 60.0).abs() < 2.0, "60 m back from where it stood");
    assert!(haversine_m(flight.launch_point, from) < 1.0);
    assert!(haversine_m(flight.launch_point, flight.impact_point) >= 65.0, "outside the minimum range");
    // 133: 5,230 m from the boat, beyond 4,750 m: closes to 4,675 m and no further
    let (plan, flight) = plan_and_flight(&run(CLOSES, &RunOptions::default()).log);
    let from = plan.fire_from.unwrap();
    assert!((haversine_m(from, plan.effect_point) - 4675.0).abs() < 1.0, "the far edge");
    assert!((haversine_m(plan.start, from) - 555.0).abs() < 10.0);
    assert!(haversine_m(flight.launch_point, from) < 1.0);
    assert!(flight.tof_s > 30.0 && flight.tof_s < 32.0, "{}", flight.tof_s);
}

/// Rules 27 decided these as before: the UGV driven onto the point, refused by the domain rule for
/// a boat on the water (131, 133) and never released inside the minimum range for a truck on land
/// (132). Rules 27 logs replay unchanged, and a rules 28 log relabelled rules 27 is caught.
#[test]
fn rules_27_logs_replay_unchanged() {
    for name in ["v27-engagement-scenario-01.jsonl", "v27-minefield-laid-on-two-signatures-scenario-128.jsonl"] {
        let text = std::fs::read_to_string(root().join("crates/murmur-decide/tests/data/audit").join(name)).unwrap();
        let r = replay_str_with(&text, &ReplayOptions { rules: RulesAccepted::Exactly(MINE_SINCE), ..ReplayOptions::default() }).unwrap();
        assert!(r.ok(), "{name}: {:?}", r.mismatches);
    }
    let v27 = RunOptions { rules_version: Some(MINE_SINCE), ..RunOptions::default() };
    for name in [SHORE, CLOSES] {
        let then = run(name, &v27);
        assert!(then.replay_ok, "{name}: {:?}", then.replay_mismatches);
        assert_eq!(then.final_state.as_deref(), Some("rejected"), "{name}");
        assert!(then.failed_rules.iter().any(|r| r == "domain"), "{name}: {:?}", then.failed_rules);
        assert!(!then.log.contains("\"fire_from\":"), "{name}: rules 27 logs a missile's firing position");
    }
    let then = run(OPENS, &v27);
    assert!(then.replay_ok, "{:?}", then.replay_mismatches);
    assert_eq!(then.final_state.as_deref(), Some("aborted"));
    assert!(!then.log.contains("engage.effect"), "never released inside the minimum range");
    assert!(!then.log.contains("\"fire_from\":"));
    let now = run(SHORE, &RunOptions::default());
    let relabelled = forge(&now.log, |ev| {
        if let EventBody::RunStarted { rules_version, .. } = &mut ev[0].body {
            *rules_version = Some(MINE_SINCE);
        }
    });
    let rr = replay_str_with(&relabelled, &ReplayOptions { rules: RulesAccepted::Exactly(MINE_SINCE), ..ReplayOptions::default() }).unwrap();
    assert!(!rr.ok());
    assert!(replay_str(&now.log).unwrap().ok());
}
