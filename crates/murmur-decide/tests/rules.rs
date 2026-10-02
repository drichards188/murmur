//! Every rule: pass, fail, and cannot-evaluate (which fails closed).
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use murmur_decide::rules::{self, Fix, UnknownEndurance};
use murmur_decide::*;

fn cfg() -> RulesConfig {
    policy().rules
}

/// Results of one rule for one intent, against `world` with terrain from `oracle`.
fn run(
    rule: RuleId,
    p: Proposal,
    world: &WorldSnapshot,
    oracle: &dyn TerrainOracle,
) -> Vec<CheckResult> {
    let s = spec(human(), p);
    let w = world.for_intent(&s, oracle);
    rules::evaluate(&s, &w, &cfg())
        .into_iter()
        .filter(|r| r.rule == rule)
        .collect()
}

fn one(rule: RuleId, p: Proposal, world: &WorldSnapshot) -> CheckResult {
    let mut v = run(rule, p, world, &terrain());
    assert_eq!(v.len(), 1, "{v:?}");
    v.remove(0)
}

fn assert_outcome(r: &CheckResult, o: Outcome, needle: &str) {
    assert_eq!(r.outcome, o, "{r:?}");
    assert!(
        r.detail.contains(needle),
        "detail {:?} lacks {needle:?}",
        r.detail
    );
}

// ---- capability -------------------------------------------------------------------------

#[test]
fn capability_pass() {
    let r = one(
        RuleId::Capability,
        orbit(NEAR_CRISSY, 150.0, &["sky-1"]),
        &world(),
    );
    assert_outcome(&r, Outcome::Pass, "copter supports orbit");
}

#[test]
fn capability_fail_unsupported_task() {
    let r = one(
        RuleId::Capability,
        orbit(NEAR_CRISSY, 150.0, &["track-1"]),
        &world(),
    );
    assert_outcome(&r, Outcome::Fail, "can't orbit; supports: track, hold");
    let r = one(RuleId::Capability, hold(&["buoy-1"]), &world());
    assert_outcome(&r, Outcome::Fail, "supports: nothing");
}

#[test]
fn capability_fail_unknown_track_target() {
    let p = Proposal::new(
        Task::Track {
            target: uid("ghost"),
        },
        vec![uid("track-1")],
    );
    let r = one(RuleId::Capability, p, &world());
    assert_outcome(
        &r,
        Outcome::Fail,
        "track target ghost is not in the world model",
    );
}

#[test]
fn capability_cannot_evaluate() {
    let r = one(RuleId::Capability, hold(&["ghost"]), &world());
    assert_outcome(&r, Outcome::Fail, "not in the world model");

    let mut w = world();
    w.vehicles.get_mut(&uid("sky-1")).unwrap().autopilot = None;
    let r = one(RuleId::Capability, hold(&["sky-1"]), &w);
    assert_outcome(&r, Outcome::Fail, "cannot evaluate: autopilot type unknown");

    let mut w = world();
    w.vehicles.get_mut(&uid("sky-1")).unwrap().platform = None;
    let r = one(RuleId::Capability, hold(&["sky-1"]), &w);
    assert_outcome(
        &r,
        Outcome::Fail,
        "cannot evaluate: platform not in the catalog",
    );
}

// ---- readiness --------------------------------------------------------------------------

#[test]
fn readiness_pass_and_exempt() {
    let r = one(
        RuleId::Readiness,
        orbit(NEAR_CRISSY, 150.0, &["sky-1"]),
        &world(),
    );
    assert_outcome(&r, Outcome::Pass, "navigation ready");
    let mut w = world();
    w.vehicles.get_mut(&uid("sky-1")).unwrap().nav_ready = None;
    let r = one(RuleId::Readiness, hold(&["sky-1"]), &w);
    assert_outcome(&r, Outcome::Pass, "not required for hold");
}

#[test]
fn readiness_fail_not_ready() {
    let mut w = world();
    w.vehicles.get_mut(&uid("sky-1")).unwrap().nav_ready = Some(Fact::new(false, T0));
    let r = one(RuleId::Readiness, orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w);
    assert_outcome(&r, Outcome::Fail, "navigation not ready");
}

#[test]
fn readiness_cannot_evaluate_missing_or_stale() {
    let mut w = world();
    w.vehicles.get_mut(&uid("sky-1")).unwrap().nav_ready = None;
    let r = one(RuleId::Readiness, orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w);
    assert_outcome(&r, Outcome::Fail, "cannot evaluate: no navigation status");

    let mut w = world();
    w.vehicles.get_mut(&uid("sky-1")).unwrap().nav_ready =
        Some(Fact::new(true, T0.plus_ms(-10_500)));
    let r = one(RuleId::Readiness, orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w);
    assert_outcome(
        &r,
        Outcome::Fail,
        "navigation status is 10.5 s old (max 10 s)",
    );
}

#[test]
fn readiness_track_needs_fresh_target_position() {
    let p = || {
        Proposal::new(
            Task::Track {
                target: uid("sky-1"),
            },
            vec![uid("track-1")],
        )
    };
    let r = one(RuleId::Readiness, p(), &world());
    assert_outcome(&r, Outcome::Pass, "navigation ready");
    let mut w = world();
    w.vehicles.get_mut(&uid("sky-1")).unwrap().position =
        Some(Fact::new(CRISSY, T0.plus_ms(-60_000)));
    let r = one(RuleId::Readiness, p(), &w);
    assert_outcome(&r, Outcome::Fail, "target position is 60.0 s old");
}

// ---- platform limits ---------------------------------------------------------------------

#[test]
fn limits_pass() {
    let r = one(
        RuleId::PlatformLimits,
        orbit(NEAR_CRISSY, 5000.0, &["hawk-1"]),
        &world(),
    );
    assert_outcome(&r, Outcome::Pass, "minimum turn radius 3667 m");
    let r = one(
        RuleId::PlatformLimits,
        orbit(NEAR_CRISSY, 20.0, &["sky-1"]),
        &world(),
    );
    assert_outcome(&r, Outcome::Pass, "copter has no minimum turn radius");
    let r = one(RuleId::PlatformLimits, hold(&["hawk-1"]), &world());
    assert_outcome(&r, Outcome::Pass, "no platform limit applies to hold");
}

#[test]
fn limits_fail_with_suggested_fix_not_silent_widening() {
    let r = one(
        RuleId::PlatformLimits,
        orbit(NEAR_CRISSY, 250.0, &["hawk-1"]),
        &world(),
    );
    assert_outcome(&r, Outcome::Fail, "radius 250 m is below 4034 m");
    assert_eq!(r.fix, Some(Fix::WidenRadius { radius_m: 4034.0 }));
    // fixed wing without catalog turn figures uses the ArduPlane default
    let r = one(
        RuleId::PlatformLimits,
        orbit(NEAR_CRISSY, 50.0, &["blade-1"]),
        &world(),
    );
    assert_outcome(&r, Outcome::Fail, "default fixed-wing loiter radius 60 m");
    assert_eq!(r.fix, Some(Fix::WidenRadius { radius_m: 66.0 }));
}

#[test]
fn limits_cannot_evaluate() {
    let mut w = world();
    w.vehicles.get_mut(&uid("hawk-1")).unwrap().autopilot = None;
    let r = one(
        RuleId::PlatformLimits,
        orbit(NEAR_CRISSY, 5000.0, &["hawk-1"]),
        &w,
    );
    assert_outcome(&r, Outcome::Fail, "cannot evaluate");
}

// ---- domain -----------------------------------------------------------------------------

#[test]
fn domain_pass() {
    let r = one(
        RuleId::Domain,
        orbit(ON_WATER, 200.0, &["boat-1"]),
        &world(),
    );
    assert_outcome(&r, Outcome::Pass, "all 12 waypoints on water");
    let r = one(
        RuleId::Domain,
        orbit(NEAR_CRISSY, 100.0, &["mule-1"]),
        &world(),
    );
    assert_outcome(&r, Outcome::Pass, "on land");
    let r = one(
        RuleId::Domain,
        orbit(NEAR_CRISSY, 100.0, &["sky-1"]),
        &world(),
    );
    assert_outcome(&r, Outcome::Pass, "no surface constraint");
}

#[test]
fn domain_fail_wrong_surface() {
    let r = one(
        RuleId::Domain,
        orbit(NEAR_CRISSY, 100.0, &["boat-1"]),
        &world(),
    );
    assert_outcome(&r, Outcome::Fail, "is not water; seahunter needs water");
    let r = one(
        RuleId::Domain,
        orbit(ON_WATER, 100.0, &["mule-1"]),
        &world(),
    );
    assert_outcome(&r, Outcome::Fail, "is not land");
    let r = one(
        RuleId::Domain,
        orbit(ON_WATER, 100.0, &["remus-1"]),
        &world(),
    );
    assert_outcome(&r, Outcome::Pass, "water");
}

#[test]
fn domain_not_evaluated_without_oracle_is_recorded_but_not_blocking() {
    let p = orbit(ON_WATER, 200.0, &["boat-1"]);
    let mut v = run(RuleId::Domain, p, &world(), &NoTerrain);
    let r = v.remove(0);
    assert_outcome(
        &r,
        Outcome::NotEvaluated,
        "terrain unavailable: no terrain oracle installed",
    );
    assert!(
        rules::passed(&[r], TaskKind::Orbit),
        "only consequential kinds block on not-evaluated"
    );
}

#[test]
fn domain_cannot_evaluate() {
    // outside the oracle's coverage
    let r = one(
        RuleId::Domain,
        orbit(LonLat::new(-122.9, 37.8), 200.0, &["boat-1"]),
        &world(),
    );
    assert_outcome(&r, Outcome::Fail, "cannot evaluate: waypoint 1");
    assert!(r.detail.contains("outside terrain coverage"));
    // terrain never sampled (bypassing for_intent)
    let s = spec(human(), orbit(ON_WATER, 200.0, &["boat-1"]));
    let w = world().subset_for(&s);
    let r: Vec<_> = rules::domain(&s, &w);
    assert_outcome(
        &r[0],
        Outcome::Fail,
        "cannot evaluate: terrain was not sampled",
    );
}

// ---- geofence ---------------------------------------------------------------------------

#[test]
fn geofence_pass() {
    let r = one(
        RuleId::Geofence,
        orbit(NEAR_CRISSY, 150.0, &["sky-1"]),
        &world(),
    );
    assert_outcome(&r, Outcome::Pass, "clear of 2 no-go areas");
    let r = one(RuleId::Geofence, hold(&["sky-1"]), &world());
    assert_outcome(&r, Outcome::Pass, "no waypoints");
}

#[test]
fn geofence_fail_point_or_leg() {
    let r = one(
        RuleId::Geofence,
        orbit(LonLat::new(-122.4230, 37.8267), 100.0, &["sky-1"]),
        &world(),
    );
    assert_outcome(&r, Outcome::Fail, "waypoint 1 is inside alcatraz");
    let route = vec![
        LonLat::new(-122.435, 37.8267),
        LonLat::new(-122.410, 37.8267),
    ];
    let p = Proposal::new(Task::Patrol { route }, vec![uid("sky-1")]);
    let r = one(RuleId::Geofence, p, &world());
    assert_outcome(&r, Outcome::Fail, "leg 1-2 crosses alcatraz");
}

#[test]
fn geofence_cannot_evaluate_without_areas() {
    let mut w = world();
    w.areas = None;
    let r = one(RuleId::Geofence, orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w);
    assert_outcome(&r, Outcome::Fail, "cannot evaluate: no area set loaded");
}

// ---- reserves ---------------------------------------------------------------------------

#[test]
fn reserves_pass() {
    let r = one(
        RuleId::Reserves,
        orbit(NEAR_CRISSY, 150.0, &["sky-1"]),
        &world(),
    );
    assert_outcome(&r, Outcome::Pass, "reserve 8 min (20%)");
    let r = one(RuleId::Reserves, hold(&["sky-1"]), &world());
    assert_outcome(&r, Outcome::Pass, "not required for hold");
}

#[test]
fn reserves_fail_low_battery() {
    let mut w = world();
    w.vehicles.get_mut(&uid("sky-1")).unwrap().battery_pct = Some(Fact::new(25.0, T0));
    let r = one(RuleId::Reserves, orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w);
    assert_outcome(&r, Outcome::Fail, "left after task");
}

#[test]
fn reserves_constraints_only_tighten() {
    let mut w = world();
    w.vehicles.get_mut(&uid("sky-1")).unwrap().battery_pct = Some(Fact::new(25.0, T0));
    let mut p = orbit(NEAR_CRISSY, 150.0, &["sky-1"]);
    p.constraints.reserve_pct = Some(0.0);
    p.constraints.duration_s = Some(1);
    let r = one(RuleId::Reserves, p, &w);
    assert_outcome(&r, Outcome::Fail, "reserve 8 min (20%)");

    let mut p = orbit(NEAR_CRISSY, 150.0, &["sky-1"]);
    p.constraints.reserve_pct = Some(90.0);
    let r = one(RuleId::Reserves, p, &world());
    assert_outcome(&r, Outcome::Fail, "(90%)");
}

#[test]
fn reserves_cannot_evaluate() {
    // the logistics UGV has no endurance figure in the catalog
    let r = one(
        RuleId::Reserves,
        orbit(NEAR_CRISSY, 100.0, &["mule-1"]),
        &world(),
    );
    assert_outcome(
        &r,
        Outcome::Fail,
        "cannot evaluate: no endurance figure for ugv_logistics",
    );

    let mut w = world();
    w.vehicles.get_mut(&uid("sky-1")).unwrap().battery_pct = None;
    let r = one(RuleId::Reserves, orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w);
    assert_outcome(&r, Outcome::Fail, "cannot evaluate: no battery level");

    let mut w = world();
    w.vehicles.get_mut(&uid("sky-1")).unwrap().home = None;
    let r = one(RuleId::Reserves, orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w);
    assert_outcome(&r, Outcome::Fail, "cannot evaluate: no home position");
}

#[test]
fn reserves_battery_now_mode_for_unknown_endurance() {
    let s = spec(human(), orbit(NEAR_CRISSY, 100.0, &["mule-1"]));
    let w = world().for_intent(&s, &terrain());
    let c = RulesConfig {
        unknown_endurance: UnknownEndurance::BatteryNow,
        ..cfg()
    };
    let r = rules::reserves(&s, &w, &c);
    assert_outcome(&r[0], Outcome::Pass, "task cost not estimated");
}

// ---- whole set --------------------------------------------------------------------------

#[test]
fn evaluate_is_ordered_and_complete() {
    let s = spec(human(), orbit(NEAR_CRISSY, 150.0, &["sky-1"]));
    let w = world().for_intent(&s, &terrain());
    let results = rules::evaluate(&s, &w, &cfg());
    let order: Vec<_> = results.iter().map(|r| r.rule).collect();
    assert_eq!(
        order,
        vec![
            RuleId::Capability,
            RuleId::Readiness,
            RuleId::PlatformLimits,
            RuleId::Domain,
            RuleId::Geofence,
            RuleId::Reserves
        ]
    );
    assert!(rules::passed(&results, TaskKind::Orbit), "{results:#?}");
}
