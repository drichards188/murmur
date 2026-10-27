//! Every rule: pass, fail, and cannot-evaluate (which fails closed), plus the two
//! recorded exceptions (not evaluated, unverified).
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use murmur_decide::rules::{self, Fix, UnknownEndurance};
use murmur_decide::*;

fn cfg() -> RulesConfig {
    policy().rules
}

/// Rules 21: before route planning (rules 22 routes a leg round a no-go area before refusing it).
fn v21() -> RulesConfig {
    let mut c = cfg();
    c.rules_version = IDENTIFY_SINCE;
    c
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
    one_with(&cfg(), rule, p, world)
}

fn one_with(c: &RulesConfig, rule: RuleId, p: Proposal, world: &WorldSnapshot) -> CheckResult {
    let s = spec(human(), p);
    let w = world.for_intent(&s, &terrain());
    let mut v: Vec<_> = rules::evaluate(&s, &w, c)
        .into_iter()
        .filter(|r| r.rule == rule)
        .collect();
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
fn readiness_dead_reckoning_passes_for_subs_only() {
    use murmur_decide::world::NavSource;
    let mut w = world();
    for u in ["remus-1", "boat-1"] {
        w.vehicles.get_mut(&uid(u)).unwrap().nav_source =
            Some(Fact::new(NavSource::DeadReckoning, T0));
    }
    let r = one(RuleId::Readiness, orbit(BAY, 300.0, &["remus-1"]), &w);
    assert_outcome(&r, Outcome::Pass, "dead reckoning");
    let r = one(RuleId::Readiness, orbit(BAY, 300.0, &["boat-1"]), &w);
    assert_outcome(&r, Outcome::Fail, "dead reckoning only");
    // the same sub on a satellite fix reads as plain ready
    w.vehicles.get_mut(&uid("remus-1")).unwrap().nav_source = Some(Fact::new(NavSource::Gnss, T0));
    let r = one(RuleId::Readiness, orbit(BAY, 300.0, &["remus-1"]), &w);
    assert_eq!(r.detail, "navigation ready");
}

/// A fixed-wing on dead reckoning (a plane in a GNSS jammer) stays taskable, out of the
/// jammer for one, but a human judges it; vehicles whose EKF gives up the position fail;
/// a sub passes; hold and rtl stay exempt.
#[test]
fn readiness_dead_reckoning_by_kind() {
    use murmur_decide::policy::{Tier, decide};
    use murmur_decide::world::NavSource;
    let mut w = world();
    w.vehicles.insert(uid("fvr-1"), vehicle("fvr90", CRISSY));
    w.vehicles.insert(uid("lta-1"), vehicle("lta", CRISSY));
    for v in w.vehicles.values_mut() {
        v.nav_source = Some(Fact::new(NavSource::DeadReckoning, T0));
    }
    let detail = "navigation on dead reckoning (no GNSS fix): position error grows with time";
    let (fail, sub) = ("dead reckoning only", "navigation ready (dead reckoning");
    for (u, at, outcome, needle) in [
        ("hawk-1", BAY, Outcome::Unverified, detail), // plane (RQ-4 stand-in)
        ("blade-1", BAY, Outcome::Unverified, detail), // plane (Switchblade)
        ("fvr-1", BAY, Outcome::Unverified, detail),  // VTOL
        ("sky-1", CRISSY, Outcome::Fail, fail),       // copter
        ("lta-1", CRISSY, Outcome::Fail, fail),       // blimp
        ("mule-1", CRISSY, Outcome::Fail, fail),      // ugv
        ("boat-1", BAY, Outcome::Fail, fail),         // boat
        ("remus-1", BAY, Outcome::Pass, sub),         // sub
    ] {
        let r = one(RuleId::Readiness, orbit(at, 9000.0, &[u]), &w);
        assert_outcome(&r, outcome, needle);
        let r = one(RuleId::Readiness, rtl(&[u]), &w);
        assert_outcome(&r, Outcome::Pass, "not required for rtl");
    }
    // unverified passes the rules gate for an orbit and routes it to a human: an agent's
    // proposal waits in the inbox, the operator's own submission is its own approval
    let s = spec(agent(), orbit(BAY, 9000.0, &["hawk-1"]));
    let checks = rules::evaluate(&s, &w.for_intent(&s, &terrain()), &cfg());
    assert!(rules::passed(&checks, s.kind()), "{checks:?}");
    let d = decide(&s, &w.for_intent(&s, &terrain()), &policy());
    assert_eq!(d.tier, Tier::Human);
    assert!(!d.submitter_authorizes);
    let why = d.facts.unverified.join("; ");
    assert!(why.contains(detail), "{why}");
    let s = spec(human(), orbit(BAY, 9000.0, &["hawk-1"]));
    let d = decide(&s, &w.for_intent(&s, &terrain()), &policy());
    assert_eq!(d.tier, Tier::Human);
    assert!(d.submitter_authorizes);
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
        "navigation status is 10.5 s old (limit 10 s)",
    );
}

fn old_behaviour() -> RulesConfig {
    RulesConfig {
        fact_age_intervals: None,
        ..cfg()
    }
}

#[test]
fn readiness_limit_follows_the_link_cadence() {
    let c = cfg();
    assert_eq!(c.fact_age_intervals, Some(3));
    let stale = Some(Fact::new(true, T0.plus_ms(-15_000)));
    let spec = || orbit(BAY, 300.0, &["remus-1"]);
    // an acoustic AUV reporting every 20 s: 15 s old facts are current (limit 60 s)
    let mut w = world();
    let v = w.vehicles.get_mut(&uid("remus-1")).unwrap();
    v.nav_ready = stale.clone();
    v.report_interval_ms = Some(20_000);
    let r = one_with(&c, RuleId::Readiness, spec(), &w);
    assert_eq!(r.outcome, Outcome::Pass, "{}", r.detail);
    // a streaming vehicle with the same age fails, naming the plain limit
    let mut w = world();
    let v = w.vehicles.get_mut(&uid("remus-1")).unwrap();
    v.nav_ready = stale.clone();
    v.report_interval_ms = Some(1000);
    let r = one_with(&c, RuleId::Readiness, spec(), &w);
    assert_outcome(&r, Outcome::Fail, "15.0 s old (limit 10 s)");
    // past 3 intervals the acoustic vehicle fails too, and the detail says why
    let mut w = world();
    let v = w.vehicles.get_mut(&uid("remus-1")).unwrap();
    v.nav_ready = Some(Fact::new(true, T0.plus_ms(-61_000)));
    v.report_interval_ms = Some(20_000);
    let r = one_with(&c, RuleId::Readiness, spec(), &w);
    assert_outcome(
        &r,
        Outcome::Fail,
        "61.0 s old (limit 60 s: link reports every 20 s)",
    );
    // key absent: the old behaviour, whatever the interval fact says
    let mut w = world();
    let v = w.vehicles.get_mut(&uid("remus-1")).unwrap();
    v.nav_ready = stale;
    v.report_interval_ms = Some(20_000);
    let r = one_with(&old_behaviour(), RuleId::Readiness, spec(), &w);
    assert_outcome(&r, Outcome::Fail, "15.0 s old (limit 10 s)");
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
    let r: Vec<_> = rules::domain(&s, &w, &cfg());
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
    assert!(r.detail.contains("(transit, task path, return)"), "{r:?}");
    let r = one(RuleId::Geofence, hold(&["sky-1"]), &world());
    assert_outcome(&r, Outcome::Pass, "no path for hold");
    let r = one(RuleId::Geofence, rtl(&["sky-1"]), &world());
    assert_outcome(&r, Outcome::Pass, "clear of 2 no-go areas (return)");
}

#[test]
fn geofence_fail_point_or_leg() {
    let r = one(
        RuleId::Geofence,
        orbit(LonLat::new(-122.4230, 37.8267), 100.0, &["sky-1"]),
        &world(),
    );
    assert_outcome(
        &r,
        Outcome::Fail,
        "task path: waypoint 1 is inside alcatraz",
    );
    let route = vec![
        LonLat::new(-122.435, 37.8267),
        LonLat::new(-122.410, 37.8267),
    ];
    let p = Proposal::new(Task::Patrol { route }, vec![uid("sky-1")]);
    let r = one_with(&v21(), RuleId::Geofence, p.clone(), &world());
    assert_outcome(&r, Outcome::Fail, "task path: leg 1-2 crosses alcatraz");
    // rules 22: the leg (and the loop back) routed round it, keeping clear of its edge
    let r = one(RuleId::Geofence, p, &world());
    assert_outcome(&r, Outcome::Pass, "task path leg 1-2 routed around no-go alcatraz");
}

/// West and east of Alcatraz, on the same latitude: the straight line between crosses it.
const WEST_OF_ALCATRAZ: LonLat = LonLat::new(-122.435, 37.8267);
const EAST_OF_ALCATRAZ: LonLat = LonLat::new(-122.405, 37.8267);
const IN_ALCATRAZ: LonLat = LonLat::new(-122.4230, 37.8267);

fn placed(w: &mut WorldSnapshot, u: &str, pos: LonLat, home: LonLat) {
    let v = w.vehicles.get_mut(&uid(u)).unwrap();
    v.position = Some(Fact::new(pos, T0));
    v.home = Some(Fact::new(home, T0));
}

#[test]
fn geofence_transit_leg_is_checked_and_named() {
    // sky-1 sits west of Alcatraz; the orbit is east of it. The task path is clear, the
    // flight to it is not.
    let mut w = world();
    placed(&mut w, "sky-1", WEST_OF_ALCATRAZ, WEST_OF_ALCATRAZ);
    let r = one_with(&v21(), RuleId::Geofence, orbit(EAST_OF_ALCATRAZ, 150.0, &["sky-1"]), &w);
    assert_outcome(
        &r,
        Outcome::Fail,
        "transit leg (position to waypoint 1) crosses alcatraz (no-go: Alcatraz Island)",
    );
    assert_eq!(r.uid, Some(uid("sky-1")));
    // rules 22: the transit routed round it; its return home (across it) stays straight: refused
    let r = one(RuleId::Geofence, orbit(EAST_OF_ALCATRAZ, 150.0, &["sky-1"]), &w);
    assert_outcome(&r, Outcome::Fail, "return leg (waypoint 1 to home) crosses alcatraz");
}

#[test]
fn geofence_return_leg_is_checked_and_named() {
    // Starts next to the orbit (transit clear), but home is across Alcatraz.
    let mut w = world();
    placed(&mut w, "sky-1", EAST_OF_ALCATRAZ, WEST_OF_ALCATRAZ);
    let r = one(
        RuleId::Geofence,
        orbit(EAST_OF_ALCATRAZ, 150.0, &["sky-1"]),
        &w,
    );
    assert_outcome(
        &r,
        Outcome::Fail,
        "return leg (waypoint 1 to home) crosses alcatraz",
    );

    // Home inside the no-go area.
    placed(&mut w, "sky-1", EAST_OF_ALCATRAZ, IN_ALCATRAZ);
    let r = one(
        RuleId::Geofence,
        orbit(EAST_OF_ALCATRAZ, 150.0, &["sky-1"]),
        &w,
    );
    assert_outcome(
        &r,
        Outcome::Fail,
        "return leg (waypoint 1 to home): home is inside alcatraz",
    );

    // A patrol (open in the sense of order, closed as flown) returns from waypoint 1 too.
    placed(&mut w, "sky-1", EAST_OF_ALCATRAZ, WEST_OF_ALCATRAZ);
    let route = vec![EAST_OF_ALCATRAZ, LonLat::new(-122.405, 37.835)];
    let p = Proposal::new(Task::Patrol { route }, vec![uid("sky-1")]);
    let r = one(RuleId::Geofence, p, &w);
    assert_outcome(
        &r,
        Outcome::Fail,
        "return leg (waypoint 1 to home) crosses alcatraz",
    );
}

#[test]
fn geofence_rtl_checks_only_the_way_home() {
    let mut w = world();
    placed(&mut w, "sky-1", EAST_OF_ALCATRAZ, WEST_OF_ALCATRAZ);
    let r = one(RuleId::Geofence, rtl(&["sky-1"]), &w);
    assert_outcome(
        &r,
        Outcome::Fail,
        "return leg (position to home) crosses alcatraz",
    );

    // A vehicle that strayed into a no-go area can still be brought out.
    placed(&mut w, "sky-1", IN_ALCATRAZ, WEST_OF_ALCATRAZ);
    let r = one(RuleId::Geofence, rtl(&["sky-1"]), &w);
    assert_outcome(&r, Outcome::Pass, "starts inside alcatraz and leaves it");
    // ...but not sent home into one.
    placed(&mut w, "sky-1", WEST_OF_ALCATRAZ, IN_ALCATRAZ);
    let r = one(RuleId::Geofence, rtl(&["sky-1"]), &w);
    assert_outcome(
        &r,
        Outcome::Fail,
        "return leg (position to home): home is inside alcatraz",
    );
}

#[test]
fn geofence_rtl_that_cannot_be_checked_is_not_evaluated_not_blocked() {
    // rtl is the safety command: an unknown position must not stop it.
    let mut w = world();
    w.vehicles.get_mut(&uid("sky-1")).unwrap().position = None;
    let r = one(RuleId::Geofence, rtl(&["sky-1"]), &w);
    assert_outcome(
        &r,
        Outcome::NotEvaluated,
        "return leg not checked (rtl is a safety command): cannot evaluate: no position reported",
    );
    assert!(rules::passed(&[r], TaskKind::Rtl));
    let mut w = world();
    w.areas = None;
    let r = one(RuleId::Geofence, rtl(&["sky-1"]), &w);
    assert_outcome(&r, Outcome::NotEvaluated, "no area set loaded");

    // Any other kind fails closed.
    let mut w = world();
    w.vehicles.get_mut(&uid("sky-1")).unwrap().position = None;
    let r = one(RuleId::Geofence, orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w);
    assert_outcome(&r, Outcome::Fail, "cannot evaluate: no position reported");
    let mut w = world();
    w.vehicles.get_mut(&uid("sky-1")).unwrap().home = None;
    let r = one(RuleId::Geofence, orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w);
    assert_outcome(&r, Outcome::Fail, "cannot evaluate: no home position");
}

#[test]
fn geofence_is_per_vehicle() {
    // Two vehicles on the same orbit: only the one whose transit crosses fails.
    let mut w = world();
    placed(&mut w, "sky-1", EAST_OF_ALCATRAZ, EAST_OF_ALCATRAZ);
    placed(&mut w, "hawk-1", WEST_OF_ALCATRAZ, EAST_OF_ALCATRAZ);
    let s = spec(human(), orbit(EAST_OF_ALCATRAZ, 150.0, &["sky-1", "hawk-1"]));
    let inputs = w.for_intent(&s, &terrain());
    let v: Vec<_> = rules::evaluate(&s, &inputs, &v21()).into_iter().filter(|r| r.rule == RuleId::Geofence).collect();
    assert_eq!(v.len(), 2);
    assert_eq!(v[0].outcome, Outcome::Pass, "{v:?}");
    assert_eq!(v[1].uid, Some(uid("hawk-1")));
    assert_outcome(&v[1], Outcome::Fail, "transit leg (position to waypoint 1)");
    // rules 22: hawk-1's transit routed round it, sky-1's untouched
    let v = run(RuleId::Geofence, orbit(EAST_OF_ALCATRAZ, 150.0, &["sky-1", "hawk-1"]), &w, &terrain());
    assert_eq!(v[0].outcome, Outcome::Pass, "{v:?}");
    assert!(!v[0].detail.contains("routed"), "{v:?}");
    assert_outcome(&v[1], Outcome::Pass, "transit leg (position to waypoint 1) routed around no-go alcatraz");
}

#[test]
fn geofence_cannot_evaluate_without_areas() {
    let mut w = world();
    w.areas = None;
    let r = one(RuleId::Geofence, orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w);
    assert_outcome(&r, Outcome::Fail, "cannot evaluate: no area set loaded");
}

#[test]
fn reserves_count_the_transit_and_return_legs() {
    // The same orbit costs more when the vehicle starts (and lives) far from it.
    let near = one(
        RuleId::Reserves,
        orbit(NEAR_CRISSY, 150.0, &["sky-1"]),
        &world(),
    );
    let mut w = world();
    let far = LonLat::new(-122.52, 37.86);
    let v = w.vehicles.get_mut(&uid("sky-1")).unwrap();
    v.position = Some(Fact::new(far, T0));
    v.home = Some(Fact::new(far, T0));
    let r = one(RuleId::Reserves, orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w);
    assert_ne!(near.detail, r.detail);
    let task_min = |d: &str| -> f64 {
        let i = d.find("(task ").unwrap() + 6;
        d[i..].split(' ').next().unwrap().parse().unwrap()
    };
    assert!(
        task_min(&r.detail) > task_min(&near.detail) + 10.0,
        "{near:?} {r:?}"
    );
    let r = one(RuleId::Reserves, rtl(&["sky-1"]), &w);
    assert_outcome(&r, Outcome::Pass, "not required for rtl");
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
fn reserves_unknown_endurance_is_unverified_not_failed() {
    // The logistics UGV has no endurance figure in the catalog. Default: a distinct
    // "not evaluated: endurance unknown" result that policy routes to a human.
    let r = one(
        RuleId::Reserves,
        orbit(NEAR_CRISSY, 100.0, &["mule-1"]),
        &world(),
    );
    assert_outcome(
        &r,
        Outcome::Unverified,
        "not evaluated: endurance unknown for ugv_logistics",
    );
    assert!(r.detail.contains("needs a human"));
    assert!(rules::passed(std::slice::from_ref(&r), TaskKind::Orbit));
    assert_eq!(
        rules::unverified(&[r]),
        vec![
            "reserves mule-1: not evaluated: endurance unknown for ugv_logistics; battery 95% >= reserve 20% now, task cost not estimated; needs a human"
        ]
    );

    // A battery already below the reserve is a plain failure.
    let mut w = world();
    w.vehicles.get_mut(&uid("mule-1")).unwrap().battery_pct = Some(Fact::new(10.0, T0));
    let r = one(RuleId::Reserves, orbit(NEAR_CRISSY, 100.0, &["mule-1"]), &w);
    assert_outcome(&r, Outcome::Fail, "battery 10% below reserve 20%");

    // Hold and rtl stay exempt.
    for p in [hold(&["mule-1"]), rtl(&["mule-1"])] {
        let r = one(RuleId::Reserves, p, &world());
        assert_outcome(&r, Outcome::Pass, "not required for");
    }

    // "fail" keeps the old fail-closed behavior.
    let s = spec(human(), orbit(NEAR_CRISSY, 100.0, &["mule-1"]));
    let w = world().for_intent(&s, &terrain());
    let c = RulesConfig {
        unknown_endurance: UnknownEndurance::Fail,
        ..cfg()
    };
    assert_outcome(
        &rules::reserves(&s, &w, &c)[0],
        Outcome::Fail,
        "cannot evaluate: no endurance figure for ugv_logistics",
    );
}

#[test]
fn reserves_cannot_evaluate() {
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
