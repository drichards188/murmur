//! Decision rules 20 (bd murmur-64z, the owner's direction of 2026-10-26, DECISIONS.md "Munitions
//! shown in flight and strikes animated"): a platform whose catalog gives it a munition (the
//! armed MQ-9B's Hellfire-class missile, 425 m/s, 8,000 m of slant range) releases it as a
//! munition in flight. It flies a straight slant line from the platform to the effect point at a
//! constant speed, so the release comes its time of flight before the plan's time, from within its
//! slant range of the point (not `arrival_m`); the strike is what the effect time, window, target
//! at the point and keep-out checks judge; the `engage.effect` logs the flight, which replay
//! recomputes; the assessment observes from the impact. A platform without one decides as before.
//! The scenarios are 106 and 107 in `sim/scenarios/engage/`.
#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use common::*;
use murmur_decide::audit::{EventBody, parse_log};
use murmur_decide::engage::{EngagePreview, MunitionFlight, release_window, shot};
use murmur_decide::geo::haversine_m;
use murmur_decide::scenario::{self, RunOptions, ScenarioResult};
use murmur_decide::*;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(name: &str, opts: &RunOptions) -> ScenarioResult {
    scenario::run_file(&root().join("sim/scenarios/engage").join(name), &root(), opts).unwrap()
}

const RELEASED_EARLY: &str = "106-standoff-missile-released-early.toml";
const OUT_OF_RANGE: &str = "107-standoff-missile-out-of-range.toml";

fn v19() -> RunOptions {
    RunOptions {
        rules_version: Some(ACCEPT_BEHAVING_SINCE),
        ..RunOptions::default()
    }
}

/// The run's `engage.effect` (its time, planned time and munition), the approval's plan, and the
/// times of every `engage.observe` and of the `engage.assessed`.
struct Released {
    t: Time,
    planned_t: Time,
    munition: Option<MunitionFlight>,
    plan: EngagePreview,
    observed: Vec<Time>,
    assessed: Option<Time>,
}

fn released(log: &str) -> Released {
    let events = parse_log(log).unwrap();
    let mut effect = None;
    let mut plan = None;
    let mut observed = Vec::new();
    let mut assessed = None;
    for e in &events {
        match &e.body {
            EventBody::EngageEffect {
                planned_t, munition, ..
            } => effect = Some((e.t, *planned_t, munition.clone())),
            EventBody::ApprovalGranted {
                snapshot: Some(s), ..
            } => plan = Some(s.preview.clone()),
            EventBody::EngageObserve { .. } => observed.push(e.t),
            EventBody::EngageAssessed { .. } => assessed = Some(e.t),
            _ => {}
        }
    }
    let (t, planned_t, munition) = effect.expect("an engage.effect");
    Released {
        t,
        planned_t,
        munition,
        plan: plan.expect("an approval with its snapshot"),
        observed,
        assessed,
    }
}

/// Both scenarios end as designed and replay, and 106's forged impact is caught.
#[test]
fn every_munition_scenario_ends_as_designed_replays_and_catches_its_tamper() {
    for name in [RELEASED_EARLY, OUT_OF_RANGE] {
        let r = run(name, &RunOptions::default());
        assert!(r.ok, "{name}: {:?}", r.failures);
        assert!(r.replay_ok, "{name}: {:?}", r.replay_mismatches);
        if r.tamper.is_some() {
            assert_eq!(r.tamper_detected, Some(true), "{name}: {:?}", r.tamper_not_applicable);
            assert!(
                r.tamper_mismatches.iter().any(|m| m.contains("munition's flight")),
                "{name}: {:?}",
                r.tamper_mismatches
            );
        }
    }
}

/// The missile is released its time of flight before the strike, which falls inside the plan's
/// time tolerance; it flies from the aircraft at 7,600 m to the point on the surface within its
/// range; the flight's times are the release and the impact; nothing is observed before the
/// impact and the assessment comes after it.
#[test]
fn a_standoff_missile_is_released_its_flight_early_and_strikes_inside_the_plan() {
    let r = run(RELEASED_EARLY, &RunOptions::default());
    assert_eq!(r.final_state.as_deref(), Some("done"), "{:?}", r.final_reason);
    let rel = released(&r.log);
    let m = rel.munition.expect("the munition's flight");
    assert_eq!(m.name, "Hellfire-class missile");
    assert_eq!(m.speed_mps, 425.0);
    assert_eq!(m.launch_t, rel.t);
    assert_eq!(m.impact_t.ms_since(m.launch_t), (m.tof_s * 1000.0).round() as i64);
    assert_eq!((m.launch_alt_m, m.impact_alt_m), (7600.0, 0.0));
    assert_eq!(m.impact_point, rel.plan.effect_point);
    let ground = haversine_m(m.launch_point, m.impact_point);
    let slant = ground.hypot(m.launch_alt_m - m.impact_alt_m);
    assert!(slant <= 8000.0, "released from {slant:.0} m");
    assert!((m.tof_s - slant / 425.0).abs() < 0.001, "{} s for {slant:.0} m", m.tof_s);
    assert!((17.5..=19.0).contains(&m.tof_s), "{} s", m.tof_s);
    // the strike, not the release, inside the plan's tolerance; the release well before it
    let ec = EngageRules {
        rules_version: MUNITION_FLIGHT_SINCE,
        ..EngageRules::default()
    };
    let (from, until) = release_window(&rel.plan, &ec);
    assert!(from <= m.impact_t && m.impact_t <= until, "{:?} not in {from:?}..{until:?}", m.impact_t);
    assert!(rel.t < from, "released {:?}, the window opens {from:?}", rel.t);
    assert_eq!(rel.planned_t, rel.plan.effect_t);
    // observed from the impact on, assessed after it
    assert!(!rel.observed.is_empty());
    assert!(rel.observed.iter().all(|t| *t >= m.impact_t), "{:?} before {:?}", rel.observed, m.impact_t);
    assert!(rel.assessed.unwrap() >= m.impact_t.plus_ms(15_000));
    // the console's JSON: the contract's keys, in its order
    let json = serde_json::to_value(&m).unwrap();
    let keys: Vec<&str> = json.as_object().unwrap().keys().map(String::as_str).collect();
    let mut want = vec![
        "name", "speed_mps", "launch_t", "launch_point", "launch_alt_m", "impact_t", "impact_point", "impact_alt_m",
        "tof_s",
    ];
    want.sort_unstable();
    let mut got = keys.clone();
    got.sort_unstable();
    assert_eq!(got, want);
    assert!(json["launch_point"].is_array() && json["launch_t"].is_i64(), "{json}");
    // served on the intent as `engagement.munition` from the release, absent before it
    let mut e = Engagement::default();
    assert!(serde_json::to_value(&e).unwrap().get("munition").is_none());
    e.munition = Some(m.clone());
    assert_eq!(serde_json::to_value(&e).unwrap()["munition"], json);
}

/// Before rules 20 the same engagement is released at the point (`arrival_m`), with no munition,
/// a time of flight or so later; from 9,000 m too, which rules 20 never releases (107).
#[test]
fn before_rules_20_the_platform_releases_at_the_point_with_no_munition() {
    let now = released(&run(RELEASED_EARLY, &RunOptions::default()).log);
    let r19 = run(RELEASED_EARLY, &v19());
    assert!(r19.replay_ok, "{:?}", r19.replay_mismatches);
    assert_eq!(r19.final_state.as_deref(), Some("done"));
    let then = released(&r19.log);
    assert!(then.munition.is_none());
    let tof = now.munition.unwrap().tof_s;
    let earlier = then.t.ms_since(now.t) as f64 / 1000.0;
    assert!(earlier >= tof - 2.0, "released {earlier} s earlier, a {tof} s flight");
    let high = run(OUT_OF_RANGE, &v19());
    assert_eq!(high.final_state.as_deref(), Some("done"), "{:?}", high.final_reason);
    assert!(released(&high.log).munition.is_none());
}

/// A platform without a munition (scenario 01's switchblade, its own munition) decides under rules
/// 20 exactly as under rules 19: every event after the run's start is the same.
#[test]
fn a_platform_without_a_munition_decides_as_before() {
    let f = "01-nominal.toml";
    let bodies = |opts: &RunOptions| -> Vec<serde_json::Value> {
        parse_log(&run(f, opts).log)
            .unwrap()
            .into_iter()
            .filter(|e| !matches!(e.body, EventBody::RunStarted { .. }))
            .map(|e| serde_json::to_value((e.t, &e.body)).unwrap())
            .collect()
    };
    let (now, then) = (bodies(&RunOptions::default()), bodies(&v19()));
    assert!(!now.is_empty());
    assert_eq!(now, then);
}

/// The shot reads the slant from the platform's position and altitude to the effect point at the
/// effect's altitude, and fails closed without an altitude; a platform without a munition, or
/// rules before 20, has none.
#[test]
fn the_shot_is_the_slant_at_the_munitions_speed_and_fails_closed() {
    let r = run(RELEASED_EARLY, &RunOptions::default());
    let plan = released(&r.log).plan;
    let ec = EngageRules {
        rules_version: MUNITION_FLIGHT_SINCE,
        ..EngageRules::default()
    };
    // 2,000 m south of the point, 7,600 m up
    let at = engage::offset(plan.effect_point, 0.0, -2000.0);
    let mut w = WorldSnapshot::new(T0);
    let mut reaper = vehicle("mq9b_armed", at);
    reaper.alt_msl_m = Some(Fact::new(7600.0, T0));
    w.vehicles.insert(uid("reaper-1"), reaper);
    w.vehicles.insert(uid("blade-1"), vehicle("switchblade", at));
    let s = shot(Some(&uid("reaper-1")), &plan, None, &w, &ec).unwrap().unwrap();
    // (the flat offset and the great circle differ by a couple of metres at 2 km)
    assert!((s.ground_m - 2000.0).abs() < 5.0, "{}", s.ground_m);
    assert_eq!(s.slant_m, s.ground_m.hypot(7600.0));
    assert!((s.slant_m - 7858.8).abs() < 2.0, "{}", s.slant_m);
    assert_eq!(s.tof_ms, (s.slant_m / 425.0 * 1000.0).round() as i64);
    assert!((18_480..=18_500).contains(&s.tof_ms), "{}", s.tof_ms);
    assert!(s.in_range());
    assert_eq!(s.lead_ms(), s.tof_ms);
    let f = s.flight(T0);
    assert_eq!((f.launch_t, f.impact_t), (T0, T0.plus_ms(s.tof_ms)));
    assert_eq!(f.tof_s, s.tof_ms as f64 / 1000.0);
    assert_eq!((f.launch_alt_m, f.impact_alt_m, f.impact_point), (7600.0, 0.0, plan.effect_point));
    // no munition, or an older rules version: no shot
    assert!(shot(Some(&uid("blade-1")), &plan, None, &w, &ec).is_none());
    let old = EngageRules {
        rules_version: ACCEPT_BEHAVING_SINCE,
        ..EngageRules::default()
    };
    assert!(shot(Some(&uid("reaper-1")), &plan, None, &w, &old).is_none());
    // no altitude reported: fail closed, saying why
    w.vehicles.get_mut(&uid("reaper-1")).unwrap().alt_msl_m = None;
    let why = shot(Some(&uid("reaper-1")), &plan, None, &w, &ec).unwrap().unwrap_err();
    assert!(why.contains("no altitude"), "{why}");
    // farther than its range: out of range, and the soonest strike is from its range
    let mut far = vehicle("mq9b_armed", engage::offset(plan.effect_point, 0.0, -5000.0));
    far.alt_msl_m = Some(Fact::new(7600.0, T0));
    w.vehicles.insert(uid("reaper-1"), far);
    let s = shot(Some(&uid("reaper-1")), &plan, None, &w, &ec).unwrap().unwrap();
    assert!(!s.in_range());
    assert_eq!(s.lead_ms(), 18_824);
}

/// The catalog gives the armed MQ-9B its missile, and only it.
#[test]
fn the_catalog_arms_only_the_armed_mq9b_with_a_munition() {
    let c = catalog();
    let m = c.get("mq9b_armed").and_then(|p| p.munition.clone()).unwrap();
    assert_eq!((m.name.as_str(), m.speed_mps, m.range_m), ("Hellfire-class missile", 425.0, 8000.0));
    let others: Vec<&str> = c
        .platforms
        .iter()
        .filter(|(id, p)| p.munition.is_some() && id.as_str() != "mq9b_armed")
        .map(|(id, _)| id.as_str())
        .collect();
    assert!(others.is_empty(), "{others:?}");
    // a munition on a platform spent by its own effect, or one that can't fly, is refused
    for bad in [
        "[x]\ndomain = \"air\"\nsim = \"standin\"\nexpended_by_effect = true\nmunition = { name = \"m\", speed_mps = 100, range_m = 1000 }\n",
        "[x]\ndomain = \"air\"\nsim = \"standin\"\nmunition = { name = \"m\", speed_mps = 0, range_m = 1000 }\n",
    ] {
        assert!(Catalog::from_toml(bad).is_err(), "{bad}");
    }
}
