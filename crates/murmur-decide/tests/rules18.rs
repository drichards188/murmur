//! Decision rules 18 (`VELOCITY_SIGMA_SINCE`, murmur-vni, the owner's decision A of 2026-10-26):
//! a protected track is predicted with its own velocity error. The regions along its velocity and
//! its average velocity grow with the 1-sigma the tracker reports (`velocity_sigma_mps`), or the
//! policy's `track_velocity_sigma_mps` when that is larger or none is given, and a young track's
//! speed bound adds `keep_out_sigma` of the same. Up to rules 17 every shown velocity counted as
//! good to the policy's 1 m/s, whatever the tracker said. The scenarios are 97
//! (`sim/scenarios/engage/97-velocity-known-to-3-mps.toml`) and 98
//! (`98-velocity-error-grows-under-way.toml`).
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;

use common::*;
use murmur_decide::catalog::Domain;
use murmur_decide::engage::{
    ReachKind, keep_out_tracks, offset, top_speed_mps, track_prediction_sigma, track_reach, track_velocity_error,
};
use murmur_decide::geo::haversine_m;
use murmur_decide::*;

const AT: LonLat = LonLat::new(-122.4950, 37.8120);

/// A fishing boat followed for ten minutes on a steady course east at 6 m/s (an average velocity,
/// no manoeuvre seen), its position known to 15 m and its velocity to `sigma` m/s as the tracker
/// reports it (None: not given).
fn boat(sigma: Option<f64>) -> TrackFacts {
    TrackFacts {
        position: Fact::new(AT, T0),
        alt_m: None,
        alt_t: None,
        velocity_mps: Some([6.0, 0.0]),
        pos_sigma_m: 15.0,
        sigma_growth_mps: Some(1.8),
        confidence: 0.9,
        affiliation: Fact::new(Affiliation::Neutral, T0),
        affiliation_confidence: 0.8,
        identity: Some("fishing_vessel".into()),
        identity_confidence: 0.9,
        sources: vec!["3:radar".into()],
        source_t: BTreeMap::new(),
        domain: Domain::Surface,
        last_seen_t: T0,
        declared_by: None,
        declaration: None,
        review_flags: Vec::new(),
        manoeuvre: None,
        mean_velocity_mps: Some([6.0, 0.0]),
        first_seen_t: Some(T0.plus_ms(-600_000)),
        velocity_sigma_mps: sigma,
    }
}

fn rules(version: u32) -> EngageRules {
    EngageRules {
        rules_version: version,
        ..EngageRules::default()
    }
}

/// The white-noise prediction's 2-sigma radius over `span` s with a velocity error of `v` m/s, as
/// the README states it: `2 sqrt(pos^2 + (v span)^2 + q span^3 / 3)`, `q` the surface's 0.2^2.
fn two_sigma(pos: f64, v: f64, span: f64) -> f64 {
    2.0 * (pos * pos + (v * span).powi(2) + 0.04 * span.powi(3) / 3.0).sqrt()
}

#[test]
fn the_velocity_region_reads_the_trackers_own_velocity_error() {
    let span = 90.0;
    let effect_t = T0.plus_ms(90_000);
    let (now, before) = (rules(VELOCITY_SIGMA_SINCE), rules(VELOCITY_SIGMA_SINCE - 1));
    let coarse = boat(Some(3.0));
    // its own error, larger than the policy's 1 m/s: read from rules 18 on, never before
    assert_eq!(track_velocity_error(&coarse, &now), 3.0);
    assert_eq!(track_velocity_error(&coarse, &before), 1.0);
    // below the policy's, or not given (a scenario): the policy's floor stands
    assert_eq!(track_velocity_error(&boat(Some(0.3)), &now), 1.0);
    assert_eq!(track_velocity_error(&boat(None), &now), 1.0);
    let region = |t: &TrackFacts, ec: &EngageRules, kind: ReachKind| {
        track_reach(t, effect_t, ec).into_iter().find(|r| r.kind == kind).unwrap()
    };
    // the region along its velocity: 2-sigma of the prediction with the error it was given
    let v18 = region(&coarse, &now, ReachKind::Velocity);
    let v17 = region(&coarse, &before, ReachKind::Velocity);
    assert!((v18.radius_m - two_sigma(15.0, 3.0, span)).abs() < 1e-6, "{}", v18.radius_m);
    assert!((v17.radius_m - two_sigma(15.0, 1.0, span)).abs() < 1e-6, "{}", v17.radius_m);
    assert!((v18.radius_m - 2.0 * track_prediction_sigma(&coarse, span, &now)).abs() < 1e-9);
    // 576 m against 269 m: the same centre, along its course
    assert_eq!(v18.center, v17.center);
    assert!(v18.radius_m > 570.0 && v17.radius_m < 270.0, "{} {}", v18.radius_m, v17.radius_m);
    // and the detail says which error it was predicted with (rules 17's words as they were)
    assert!(v18.how.contains("velocity 270 m at 3.0 m/s"), "{}", v18.how);
    assert!(v17.how.contains("velocity 90 m, manoeuvre"), "{}", v17.how);
    // the region along its average velocity takes the same uncertainty
    let m18 = region(&coarse, &now, ReachKind::MeanVelocity);
    assert!((m18.radius_m - v18.radius_m).abs() < 1e-9);
    // a velocity the tracker holds better than the policy's figure is judged as before
    for t in [boat(Some(0.3)), boat(None)] {
        assert_eq!(region(&t, &now, ReachKind::Velocity).radius_m, v17.radius_m);
        assert_eq!(region(&t, &now, ReachKind::Velocity).center, v17.center);
    }
}

#[test]
fn a_keep_out_holds_against_where_an_uncertain_velocity_may_put_a_track() {
    let effect_t = T0.plus_ms(90_000);
    let (now, before) = (rules(VELOCITY_SIGMA_SINCE), rules(VELOCITY_SIGMA_SINCE - 1));
    let clearance = 75.0 + now.keep_out_m;
    // the effect point 700 m north of where the boat's course puts it at the effect time: clear
    // of a 269 m 2-sigma, inside a 576 m one
    let predicted = offset(AT, 6.0 * 90.0, 0.0);
    let point = offset(predicted, 0.0, 700.0);
    assert!((haversine_m(predicted, point) - 700.0).abs() < 1.0);
    let check = |t: &TrackFacts, ec: &EngageRules| {
        let mut w = world();
        w.tracks = [("T-7".to_string(), t.clone())].into();
        keep_out_tracks(point, effect_t, clearance, "T-0001", &w, ec, None)
    };
    let coarse = boat(Some(3.0));
    assert_eq!(check(&coarse, &before).outcome, Outcome::Pass);
    let c = check(&coarse, &now);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("at 3.0 m/s"), "{}", c.detail);
    // the same boat with its velocity known to 0.8 m/s (AIS) is clear under both
    assert_eq!(check(&boat(Some(0.8)), &now).outcome, Outcome::Pass);
    assert_eq!(check(&boat(None), &now).outcome, Outcome::Pass);
}

#[test]
fn a_young_tracks_speed_bound_adds_its_own_velocity_error() {
    let span = 60.0;
    let effect_t = T0.plus_ms(60_000);
    let (now, before) = (rules(VELOCITY_SIGMA_SINCE), rules(VELOCITY_SIGMA_SINCE - 1));
    // a fast boat tracked for 20 s: no average velocity yet, so possibly manoeuvring
    let mut t = boat(Some(4.0));
    t.velocity_mps = Some([20.0, 0.0]);
    t.mean_velocity_mps = None;
    t.first_seen_t = Some(T0.plus_ms(-20_000));
    let young = |ec: &EngageRules| track_reach(&t, effect_t, ec).into_iter().find(|r| r.kind == ReachKind::Young).unwrap();
    // rules 17: its speed and two of the policy's 1 m/s, 22 m/s, under the surface's 25
    assert!((young(&before).radius_m - (2.0 * 15.0 + top_speed_mps(Domain::Surface) * span)).abs() < 1e-9);
    // rules 18: two of its own 4 m/s, 28 m/s, over it
    assert!((young(&now).radius_m - (2.0 * 15.0 + 28.0 * span)).abs() < 1e-9, "{}", young(&now).radius_m);
}

/// A dry run reads the region as the decision does: the nomination form's options
/// (`Engine::dry_run_inputs`, then the checks) and the keep-out explanation agree, and the
/// explanation says what the velocity's error is.
#[test]
fn a_dry_run_and_its_explanation_read_the_trackers_velocity_error() {
    use murmur_decide::engage::{EngageWindow, Fallback};
    let target_at = LonLat::new(-122.4950, 37.8120);
    let mut target = boat(Some(0.5));
    target.position = Fact::new(target_at, T0);
    target.velocity_mps = Some([0.0, 0.0]);
    target.mean_velocity_mps = Some([0.0, 0.0]);
    target.affiliation = Fact::new(Affiliation::Hostile, T0);
    target.identity = Some("small_fast_boat".into());
    target.declared_by = Some("op-1".into());
    let spec = IntentSpec {
        id: IntentId(0),
        created_t: T0,
        source: Source::Human { id: OperatorId::new("op-1") },
        task: Task::Engage {
            track: "T-0001".into(),
            effect_radius_m: 75.0,
            window: Some(EngageWindow { from: None, until: T0.plus_ms(600_000) }),
            fallback: Fallback::Return,
        },
        uids: vec![uid("blade-1")],
        constraints: Constraints::default(),
        rationale: None,
    };
    let mut w = WorldSnapshot::new(T0);
    w.areas = Some(areas());
    let mut blade = vehicle("switchblade", offset(target_at, 0.0, -3_000.0));
    blade.autopilot = Some(Fact::new(AutopilotKind::Plane, T0));
    w.vehicles.insert(uid("blade-1"), blade);
    w.tracks.insert("T-0001".into(), target);
    let cfg18 = policy().rules;
    assert_eq!(cfg18.rules_version, VELOCITY_SIGMA_SINCE);
    let pv = engage::preview(&spec, &w, &cfg18).unwrap();
    let span = (pv.effect_t.0 - T0.0) as f64 / 1000.0;
    // a boat whose course passes 700 m from the effect point at the effect time
    let mut coarse = boat(Some(3.0));
    coarse.position = Fact::new(offset(pv.effect_point, -6.0 * span, 700.0), T0);
    w.tracks.insert("T-0007".into(), coarse);
    let keep_out = |cfg: &RulesConfig, w: &WorldSnapshot| {
        let clock = ManualClock::new(T0);
        let mut p = policy();
        p.rules.rules_version = cfg.rules_version;
        let e = Engine::new(p, roster(), Box::new(clock), Box::new(MemorySink::new())).unwrap();
        let inputs = e.dry_run_inputs(&spec, w, &terrain());
        rules::evaluate(&spec, &inputs, &e.policy().rules)
            .into_iter()
            .find(|c| c.rule == RuleId::KeepOutTracks)
            .unwrap()
    };
    let mut cfg17 = cfg18.clone();
    cfg17.rules_version = VELOCITY_SIGMA_SINCE - 1;
    assert_eq!(keep_out(&cfg17, &w).outcome, Outcome::Pass, "{}", keep_out(&cfg17, &w).detail);
    let c = keep_out(&cfg18, &w);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("T-0007"), "{}", c.detail);
    // the explanation names the region along its course and the velocity's error
    let ex = keepout::explain(&spec, &w, &cfg18, &keepout::Options::default()).unwrap();
    let item = ex.items.iter().find(|i| i.id == "T-0007").unwrap();
    assert!(item.blocks);
    let region = item.region.as_ref().unwrap();
    assert_eq!(region.kind, "velocity");
    assert!(item.reason.contains("its velocity known to 3.0 m/s"), "{}", item.reason);
    // known to 1 m/s, as rules 17 took every velocity to be, the same boat is clear
    let mut w1 = w.clone();
    if let Some(t) = w1.tracks.get_mut("T-0007") {
        t.velocity_sigma_mps = Some(1.0);
    }
    assert_eq!(keep_out(&cfg18, &w1).outcome, Outcome::Pass);
}
