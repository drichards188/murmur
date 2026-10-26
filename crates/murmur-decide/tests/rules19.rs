//! Decision rules 19 (bd murmur-j0x, the owner's decision B of 2026-10-26, option 2 of
//! docs/ENGAGEMENT-IN-TRAFFIC.md): an operator with engagement authority may accept one live
//! track that blocks an engagement's keep-out "as it is behaving now". For that engagement the
//! keep-out then counts it along the path accepted for it (its accepted velocity, with the
//! prediction's uncertainty growing as rules 18 grows it) and along its course now, instead of
//! anywhere it could turn or speed up to; the acceptance lapses on its own when the track's
//! estimate departs from the accepted prediction beyond its uncertainty, or it goes out of view,
//! and a new one needs a person again. The scenarios are 99 to 105 in `sim/scenarios/engage/`.
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use common::*;
use murmur_decide::audit::{EventBody, parse_log};
use murmur_decide::catalog::Domain;
use murmur_decide::engage::{
    EngageWindow, Fallback, LapseKind, ReachKind, behaving_acceptable, behaving_lapse, behaving_regions, offset,
    track_reach,
};
use murmur_decide::replay::{RulesAccepted, replay_str, replay_str_with};
use murmur_decide::scenario::{self, RunOptions, ScenarioResult};
use murmur_decide::*;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(name: &str, opts: &RunOptions) -> ScenarioResult {
    scenario::run_file(&root().join("sim/scenarios/engage").join(name), &root(), opts).unwrap()
}

const SCENARIOS: [&str; 7] = [
    "99-aircraft-accepted-as-behaving.toml",
    "100-behaving-acceptance-lapses-on-a-jump.toml",
    "101-behaving-acceptance-lapses-out-of-view.toml",
    "102-behaving-acceptance-revoked.toml",
    "103-reset-clears-behaving-acceptance.toml",
    "104-agent-cannot-accept-behaving.toml",
    "105-behaving-acceptance-lapses-under-way.toml",
];

/// Each scenario ends as designed, replays, and its log attack is caught.
#[test]
fn every_behaving_scenario_ends_as_designed_replays_and_catches_its_tamper() {
    for name in SCENARIOS {
        let r = run(name, &RunOptions::default());
        assert!(r.ok, "{name}: {:?}", r.failures);
        assert!(r.replay_ok, "{name}: {:?}", r.replay_mismatches);
        if r.tamper.is_some() {
            assert_eq!(r.tamper_detected, Some(true), "{name}: {:?}", r.tamper_not_applicable);
        }
    }
}

/// The accepted aircraft keeps its course: the acceptance holds every second to the effect, and
/// a tracker's noise (3 m on every fix, 0.5 m/s on the velocity, correlated over 10 s) does not
/// lapse it.
#[test]
fn an_acceptance_holds_while_the_track_keeps_its_course_noise_and_all() {
    let r = run("99-aircraft-accepted-as-behaving.toml", &RunOptions::default());
    assert!(r.lapsed.is_empty(), "{:?}", r.lapsed);
    assert_eq!(r.final_state.as_deref(), Some("done"));
    for seed in 1..=5 {
        let noisy = RunOptions {
            seed: Some(seed),
            track_noise_m: 3.0,
            velocity_noise_mps: 0.5,
            ..RunOptions::default()
        };
        let r = run("99-aircraft-accepted-as-behaving.toml", &noisy);
        assert!(r.lapsed.is_empty(), "seed {seed}: {:?}", r.lapsed);
        assert!(r.replay_ok, "seed {seed}: {:?}", r.replay_mismatches);
    }
}

/// The aircraft that turns back under way lapses its acceptance at the engine's first step after
/// the turn, and the engagement stops at that same step: nothing is released.
#[test]
fn a_turn_under_way_lapses_the_acceptance_at_the_next_step_and_stops_it() {
    let r = run("105-behaving-acceptance-lapses-under-way.toml", &RunOptions::default());
    assert_eq!(r.lapsed.len(), 1, "{:?}", r.lapsed);
    assert!(r.lapsed[0].starts_with("t=30 T-0002 lapsed (velocity)"), "{:?}", r.lapsed);
    assert_eq!(r.final_state.as_deref(), Some("aborted"));
    assert!(r.final_reason.as_deref().is_some_and(|w| w.contains("keep_out_tracks")), "{:?}", r.final_reason);
}

/// Before rules 19 nobody may accept a live track: the engine refuses the whole nomination that
/// carries one, so 83's rejection stands.
#[test]
fn before_rules_19_a_live_track_cannot_be_accepted() {
    let v18 = RunOptions {
        rules_version: Some(VELOCITY_SIGMA_SINCE),
        ..RunOptions::default()
    };
    let r = run("99-aircraft-accepted-as-behaving.toml", &v18);
    assert!(
        r.refusals.iter().any(|w| w.contains("no acceptance of a live track as behaving now")),
        "{:?}",
        r.refusals
    );
    assert_eq!(r.final_state, None, "{:?}", r.final_reason);
    assert!(r.replay_ok, "{:?}", r.replay_mismatches);
}

// ---- the rule's functions ---------------------------------------------------------------------

/// 83's light aircraft: 2 km north of the target, flying north at 50 m/s on a steady course,
/// followed ten minutes, its position known to 15 m, no velocity error given (the policy's 1 m/s).
fn aircraft() -> TrackFacts {
    let at = LonLat::new(-122.4950, 37.8300);
    TrackFacts {
        position: Fact::new(at, T0),
        alt_m: None,
        alt_t: None,
        velocity_mps: Some([0.0, 50.0]),
        pos_sigma_m: 15.0,
        sigma_growth_mps: Some(15.0),
        confidence: 0.9,
        affiliation: Fact::new(Affiliation::Neutral, T0),
        affiliation_confidence: 0.8,
        identity: Some("light_aircraft".into()),
        identity_confidence: 0.9,
        sources: vec!["3:radar".into(), "3:eo_ir".into()],
        source_t: BTreeMap::new(),
        domain: Domain::Air,
        last_seen_t: T0,
        declared_by: None,
        declaration: None,
        review_flags: Vec::new(),
        manoeuvre: None,
        mean_velocity_mps: Some([0.0, 50.0]),
        first_seen_t: Some(T0.plus_ms(-600_000)),
        velocity_sigma_mps: None,
    }
}

fn ec() -> EngageRules {
    EngageRules {
        rules_version: ACCEPT_BEHAVING_SINCE,
        ..EngageRules::default()
    }
}

fn accepted(t: &TrackFacts) -> AcceptedBehaving {
    AcceptedBehaving {
        by: OperatorId::new("op-1"),
        t: t.position.t,
        facts: t.clone(),
    }
}

/// The aircraft's estimate `s` seconds on: where its course puts it, `east` m off it, its velocity
/// `v`.
fn later(s: i64, east: f64, v: Option<[f64; 2]>) -> TrackFacts {
    let mut t = aircraft();
    let at = T0.plus_ms(s * 1000);
    t.position = Fact::new(offset(t.position.value, east, 50.0 * s as f64), at);
    t.last_seen_t = at;
    t.velocity_mps = v;
    t
}

#[test]
fn only_a_protected_track_in_view_with_a_course_may_be_accepted() {
    let ec = ec();
    let a = aircraft();
    assert_eq!(behaving_acceptable("T-0002", Some(&a), T0, "T-0001", &ec), Ok(()));
    let no = |t: Option<&TrackFacts>, id: &str| behaving_acceptable(id, t, T0.plus_ms(1_000), "T-0001", &ec).unwrap_err();
    assert!(no(None, "T-0002").contains("not in the picture"));
    assert!(no(Some(&a), "T-0001").contains("the target"));
    let mut unseen = a.clone();
    unseen.last_seen_t = T0.plus_ms(-9_000);
    assert!(no(Some(&unseen), "T-0002").contains("out of view"));
    let mut still = a.clone();
    still.velocity_mps = None;
    assert!(no(Some(&still), "T-0002").contains("no velocity estimate"));
}

/// The lapse test: the estimate within `keep_out_sigma` of the combined 1-sigma of the accepted
/// prediction and itself holds; beyond it lapses; and a velocity the same. 10 s on: the accepted
/// prediction's 1-sigma is sqrt(15^2 + (1 x 10)^2 + 2^2 x 10^3 / 3) = 40.7 m, with the estimate's
/// 15 m 43.4 m, so 87 m at 2-sigma; the velocity's sqrt(1 + 1 + 4 x 10) = 6.5 m/s, so 13 m/s.
#[test]
fn an_acceptance_lapses_when_the_estimate_leaves_the_accepted_path_or_course() {
    let ec = ec();
    let a = accepted(&aircraft());
    let lapse = |t: Option<&TrackFacts>, now_s: i64| behaving_lapse("T-0002", &a, t, T0.plus_ms(now_s * 1000), &ec);
    let on = Some([0.0, 50.0]);
    assert_eq!(lapse(Some(&later(10, 0.0, on)), 10), None);
    assert_eq!(lapse(Some(&later(10, 80.0, on)), 10), None);
    let off = lapse(Some(&later(10, 95.0, on)), 10).unwrap();
    assert_eq!(off.kind, LapseKind::Position, "{}", off.detail);
    assert!(off.detail.contains("beyond the 87 m"), "{}", off.detail);
    assert_eq!(lapse(Some(&later(10, 0.0, Some([12.0, 50.0]))), 10), None);
    let turned = lapse(Some(&later(10, 0.0, Some([14.0, 50.0]))), 10).unwrap();
    assert_eq!(turned.kind, LapseKind::Velocity, "{}", turned.detail);
    assert_eq!(lapse(Some(&later(10, 0.0, None)), 10).unwrap().kind, LapseKind::NoVelocity);
    // unseen 10 s: out of view, whatever its last estimate said
    assert_eq!(lapse(Some(&later(5, 0.0, on)), 15).unwrap().kind, LapseKind::OutOfView);
    assert_eq!(lapse(None, 10).unwrap().kind, LapseKind::Gone);
}

/// Accepted, the aircraft counts along its accepted path (with the prediction's 2-sigma and the
/// tracker's lag) and its course now, and no longer anywhere it could turn or speed up to.
#[test]
fn an_accepted_aircraft_counts_along_its_path_not_anywhere_it_could_turn() {
    let ec = ec();
    let t = aircraft();
    let effect_t = T0.plus_ms(90_000);
    let all: Vec<ReachKind> = track_reach(&t, effect_t, &ec).iter().map(|r| r.kind).collect();
    assert!(all.contains(&ReachKind::Aircraft), "{all:?}");
    let regions = behaving_regions(&accepted(&t), &t, effect_t, &ec, true);
    let kinds: Vec<ReachKind> = regions.iter().map(|r| r.kind).collect();
    assert_eq!(kinds, vec![ReachKind::Accepted, ReachKind::Velocity, ReachKind::MeanVelocity]);
    let sigma = engage::track_prediction_sigma(&t, 90.0, &ec);
    assert!((regions[0].radius_m - (2.0 * sigma + engage::AIRCRAFT_LAG_M)).abs() < 1e-9);
    assert!(regions[0].how.contains("accepted as behaving now by op-1"), "{}", regions[0].how);
}

/// A nomination on a target south of the aircraft: what the keep-out explanation says accepting
/// the aircraft as behaving now would do (the console's and the scripted reviewer's reading).
fn nomination(aircraft: TrackFacts) -> (IntentSpec, WorldSnapshot) {
    let target_at = LonLat::new(-122.4950, 37.8120);
    let mut target = aircraft.clone();
    target.position = Fact::new(target_at, T0);
    target.velocity_mps = Some([0.0, 0.0]);
    target.mean_velocity_mps = Some([0.0, 0.0]);
    target.domain = Domain::Surface;
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
    blade.autopilot = Some(Fact::new(catalog::AutopilotKind::Plane, T0));
    w.vehicles.insert(uid("blade-1"), blade);
    w.tracks.insert("T-0001".into(), target);
    w.tracks.insert("T-0002".into(), aircraft);
    (spec, w)
}

#[test]
fn the_explanation_says_whether_accepting_a_blocker_as_behaving_now_would_clear_it() {
    let cfg = policy().rules;
    let explain = |a: TrackFacts| {
        let (spec, w) = nomination(a);
        keepout::explain(&spec, &w, &cfg, &keepout::Options::default()).unwrap()
    };
    // flying away: it blocks by its reach, and accepting it would clear it
    let ex = explain(aircraft());
    let item = ex.items.iter().find(|i| i.id == "T-0002").unwrap();
    assert!(item.blocks);
    assert_eq!(item.region.as_ref().unwrap().kind, "aircraft");
    let b = item.behaving.as_ref().unwrap();
    assert!(b.acceptable && b.clears, "{}", b.reason);
    assert_eq!(b.region.as_ref().unwrap().kind, "accepted");
    // flying toward the point, there by the effect time: accepting it would not
    let mut toward = aircraft();
    toward.velocity_mps = Some([0.0, -15.0]);
    toward.mean_velocity_mps = Some([0.0, -15.0]);
    let ex = explain(toward);
    let b = ex.items.iter().find(|i| i.id == "T-0002").unwrap().behaving.clone().unwrap();
    assert!(b.acceptable && !b.clears, "{}", b.reason);
    // no velocity: nothing to accept
    let mut still = aircraft();
    still.velocity_mps = None;
    let ex = explain(still);
    let b = ex.items.iter().find(|i| i.id == "T-0002").unwrap().behaving.clone().unwrap();
    assert!(!b.acceptable && !b.clears, "{}", b.reason);
}

/// The engine's own refusals: a track already accepted, after a signature, a track out of view.
#[test]
fn the_engine_refuses_an_acceptance_it_may_not_take() {
    let r = run("100-behaving-acceptance-lapses-on-a-jump.toml", &RunOptions::default());
    assert!(r.ok, "{:?}", r.failures);
    // the scenario's refusal: a token from before the lapse
    assert!(r.refusals.iter().any(|w| w.contains("tracks accepted for it as behaving now changed since you looked")), "{:?}", r.refusals);
    // the log of 99, its acceptance given again in the log while it is in force, is caught
    let text = run("99-aircraft-accepted-as-behaving.toml", &RunOptions::default()).log;
    let mut events = parse_log(&text).unwrap();
    let i = events
        .iter()
        .position(|e| matches!(e.body, EventBody::EngageAccepted { track: Some(_), .. }))
        .unwrap();
    let again = events[i].clone();
    events.insert(i + 1, again);
    let forged = rechain(&events);
    let rep = replay_str(&forged).unwrap();
    assert!(rep.mismatches.iter().any(|m| m.what.contains("accepted as behaving now again")), "{:?}", rep.mismatches);
}

/// A rules 19 log claiming rules 18 carries what only rules 19 writes: caught by its markers.
#[test]
fn a_rules_19_log_claiming_rules_18_is_caught_by_its_markers() {
    let text = run("105-behaving-acceptance-lapses-under-way.toml", &RunOptions::default()).log;
    assert!(replay_str(&text).unwrap().ok());
    let mut events = parse_log(&text).unwrap();
    for e in &mut events {
        if let EventBody::RunStarted { rules_version, .. } = &mut e.body {
            *rules_version = Some(VELOCITY_SIGMA_SINCE);
        }
    }
    let forged = rechain(&events);
    let r = replay_str_with(
        &forged,
        &replay::ReplayOptions { rules: RulesAccepted::Exactly(VELOCITY_SIGMA_SINCE), ..Default::default() },
    )
    .unwrap();
    assert!(
        r.mismatches.iter().any(|m| m.what.contains("lapsed acceptance of a live track")),
        "{:?}",
        r.mismatches
    );
}

/// Re-number and re-hash events as a forger would.
fn rechain(events: &[audit::Event]) -> String {
    let mut prev = murmur_decide::hash::GENESIS.to_string();
    let mut out = String::new();
    for (n, e) in events.iter().enumerate() {
        let mut ev = e.clone();
        ev.seq = n as u64 + 1;
        ev.prev = prev.clone();
        let line = serde_json::to_string(&ev).unwrap();
        prev = murmur_decide::hash::sha256_hex(line.as_bytes());
        out.push_str(&line);
        out.push('\n');
    }
    out
}
