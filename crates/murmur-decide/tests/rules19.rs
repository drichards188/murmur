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

use std::path::PathBuf;

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
