//! Decision rules 26 (bd murmur-lbw8.3, the owner's direction of 2026-11-02, DECISIONS.md "Guns:
//! bursts from a counted magazine"): a gun is usable. Its plan sends the platform to a firing
//! position within the gun's range of the target; its release is the first burst and opens a
//! firing window in which a burst goes out every `burst_interval_s` while the picture holds and
//! the burst's checks pass (the target in the gun's range and seen since the last burst, the line
//! of fire clear), each spending its rounds in rules 24's ledger; the fire ends when the
//! assessment from the last burst says ceased or unknown, the rounds run out, the approved window
//! closes, or it is stopped, superseded or its picture fails. Scenarios 123 (bursts until the
//! target ceases), 124 (the rounds run out; the firing position), 125 (an operator stops it mid-fire),
//! 126 (another order supersedes it), 127 (fire held across the line of fire, stopped for a
//! keep-out).
#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use common::*;
use murmur_decide::audit::{EventBody, parse_log};
use murmur_decide::engage::{self, FireEndKind};
use murmur_decide::geo::haversine_m;
use murmur_decide::replay::{CarriedFrom, ReplayOptions, RulesAccepted, log_head, owed_debts, replay_str, replay_str_with};
use murmur_decide::scenario::{self, RunOptions, ScenarioResult};
use murmur_decide::*;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(name: &str, opts: &RunOptions) -> ScenarioResult {
    scenario::run_file(&root().join("sim/scenarios/engage").join(name), &root(), opts).unwrap()
}

const CEASES: &str = "123-gun-bursts-until-the-target-ceases.toml";
const ROUNDS_OUT: &str = "124-gun-rounds-run-out.toml";
const STOPPED: &str = "125-gun-stopped-mid-fire.toml";
const SUPERSEDED: &str = "126-gun-superseded-mid-fire.toml";
const LINE_AND_KEEP_OUT: &str = "127-gun-line-of-fire-and-keep-out.toml";
const GUN: &str = "M2 .50 HMG";

/// Every release and burst of a log: its time, burst number (1 for the release) and weapon.
fn bursts(log: &str) -> Vec<(Time, u32, WeaponRelease)> {
    parse_log(log)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.body {
            EventBody::EngageEffect { weapon: Some(w), .. } => Some((e.t, 1, w)),
            EventBody::EngageBurst { burst, weapon, .. } => Some((e.t, burst, weapon)),
            _ => None,
        })
        .collect()
}

/// The scenarios end as designed and replay, and their tampers are caught.
#[test]
fn the_gun_scenarios_end_as_designed_replay_and_catch_their_tamper() {
    for name in [CEASES, ROUNDS_OUT, STOPPED, SUPERSEDED, LINE_AND_KEEP_OUT] {
        let r = run(name, &RunOptions::default());
        assert!(r.ok, "{name}: {:?}", r.failures);
        assert!(r.replay_ok, "{name}: {:?}", r.replay_mismatches);
        if r.tamper.is_some() {
            assert_eq!(r.tamper_detected, Some(true), "{name}: {:?}", r.tamper_not_applicable);
        }
    }
    let r = run(CEASES, &RunOptions::default());
    assert!(r.tamper_mismatches.iter().any(|m| m.contains("rounds left")), "{:?}", r.tamper_mismatches);
    let r = run(STOPPED, &RunOptions::default());
    assert!(
        r.tamper_mismatches.iter().any(|m| m.contains("outside a gun's open firing window")),
        "{:?}",
        r.tamper_mismatches
    );
}

/// A burst sequence until the target ceases: the release and each burst fire 6 rounds, 5 s apart,
/// at the cyclic rate (0.69 s), the ledger counting each; once the target is no longer seen
/// nothing more goes out, and the assessment from the last burst ends it, ceased.
#[test]
fn bursts_go_out_until_the_target_ceases() {
    let r = run(CEASES, &RunOptions::default());
    let b = bursts(&r.log);
    assert_eq!(b.len(), 3, "{b:?}");
    for (n, (t, k, w)) in b.iter().enumerate() {
        assert_eq!((w.name.as_str(), w.kind, *k), (GUN, WeaponKind::Gun, n as u32 + 1));
        assert_eq!((w.rounds, w.left), (Some(6), Some(494 - 6 * n as u32)));
        if n > 0 {
            assert_eq!(t.ms_since(b[n - 1].0), 5000);
        }
    }
    let events = parse_log(&r.log).unwrap();
    assert!(events.iter().any(|e| matches!(e.body, EventBody::EngageBurst { duration_s, .. } if duration_s == 0.69)));
    // the fire is held on a target not seen since the last burst, then the assessment ends it
    let held = events.iter().any(|e| match &e.body {
        EventBody::EngagePicture { results, holds: true, ready: false, .. } => results
            .iter()
            .any(|c| c.rule == RuleId::TargetRecency && c.outcome == Outcome::Fail && c.detail.contains("not seen since: fire held")),
        _ => false,
    });
    assert!(held, "the fire held on a target not seen since the last burst");
    assert!(events.iter().any(|e| matches!(&e.body, EventBody::EngageAssessed { result: engage::AssessResult::Ceased, .. })));
    // the log's debts: 18 rounds of the M2 on bull-1
    let d = owed_debts(&r.log);
    assert_eq!(d.rounds.len(), 1, "{:?}", d.rounds);
    assert_eq!((d.rounds[0].uid.as_str(), d.rounds[0].weapon.as_str(), d.rounds[0].used), ("bull-1", GUN, 18));
}

/// The rounds run out inside the window: the last burst fires what is left (2), the fire ends on
/// its own, the assessment from the last burst follows. The plan sends the UGV from 2,000 m to its
/// firing position, the gun's range less the effect radius from the point, on the line to it.
#[test]
fn the_rounds_run_out_and_the_platform_fires_from_its_standoff() {
    let r = run(ROUNDS_OUT, &RunOptions::default());
    let b = bursts(&r.log);
    assert_eq!(b.len(), 84);
    assert_eq!((b[83].2.rounds, b[83].2.left), (Some(2), Some(0)));
    assert_eq!(b.iter().map(|(_, _, w)| w.spent()).sum::<u32>(), 500);
    let events = parse_log(&r.log).unwrap();
    let ended = events.iter().find_map(|e| match &e.body {
        EventBody::EngageFireEnded { why, detail, .. } => Some((*why, detail.clone())),
        _ => None,
    });
    assert_eq!(ended.unwrap(), (FireEndKind::RoundsOut, "no round of the M2 .50 HMG left: 84 bursts, 500 rounds fired".to_string()));
    let plan = events
        .iter()
        .find_map(|e| match &e.body {
            EventBody::ApprovalGranted { snapshot: Some(s), .. } => Some(s.preview.clone()),
            _ => None,
        })
        .unwrap();
    let from = plan.fire_from.unwrap();
    assert!((haversine_m(from, plan.effect_point) - 1725.0).abs() < 1.0, "the standoff: 1,800 m less 75 m");
    assert!((haversine_m(plan.start, from) - 275.0).abs() < 2.0, "275 m north of where it stood");
    assert_eq!(plan.path.last(), Some(&from));
}

/// An operator's stop mid-fire and another order's supersede end the fire at once: nothing goes
/// out after them; the operator's commands the recorded fallback, the supersede none; either way
/// the engagement stays effected and is assessed from its last burst.
#[test]
fn a_stop_or_a_supersede_mid_fire_ends_the_fire() {
    for (name, cause, fallback) in [(STOPPED, "operator", Some(Fallback::Abort)), (SUPERSEDED, "superseded", None)] {
        let r = run(name, &RunOptions::default());
        let events = parse_log(&r.log).unwrap();
        let (stop_t, f) = events
            .iter()
            .find_map(|e| match &e.body {
                EventBody::EngageStopped { cause: c, fallback, .. } if c.kind() == cause => Some((e.t, *fallback)),
                _ => None,
            })
            .unwrap();
        assert_eq!(f, fallback, "{name}");
        let b = bursts(&r.log);
        assert_eq!(b.len(), 2, "{name}");
        assert!(b.iter().all(|(t, _, _)| *t < stop_t), "{name}: nothing after the stop");
        assert!(!events.iter().any(|e| e.t > stop_t && matches!(e.body, EventBody::EngagePicture { .. })), "{name}");
        assert_eq!(r.final_state.as_deref(), Some("done"), "{name}");
    }
}

/// Every safeguard per burst: a protected boat on the line of fire holds the fire (the picture
/// holds, the burst's line_of_fire check fails); one of ours inside the keep-out fails the picture
/// and stops the fire with the fallback.
#[test]
fn the_line_of_fire_holds_the_fire_and_a_keep_out_stops_it() {
    let r = run(LINE_AND_KEEP_OUT, &RunOptions::default());
    let events = parse_log(&r.log).unwrap();
    let held = events.iter().any(|e| match &e.body {
        EventBody::EngagePicture { results, holds: true, ready: false, .. } => results
            .iter()
            .any(|c| c.rule == RuleId::LineOfFire && c.outcome == Outcome::Fail && c.detail.contains("T-0002")),
        _ => false,
    });
    assert!(held, "the sailboat on the line holds the fire");
    let b: Vec<i64> = bursts(&r.log).iter().map(|(t, _, _)| t.ms_since(T0) / 1000).collect();
    assert_eq!(b, vec![25, 30, 41]);
    let stop = events.iter().find_map(|e| match &e.body {
        EventBody::EngageStopped { cause, reason, fallback, .. } => Some((cause.kind(), reason.clone(), *fallback)),
        _ => None,
    });
    let (cause, reason, fallback) = stop.unwrap();
    assert_eq!((cause, fallback), ("picture", Some(Fallback::Abort)));
    assert!(reason.contains("keep_out_friendly hunter-1"), "{reason}");
}

/// A boat holding west of the bridge, declared hostile.
fn boat() -> TrackFacts {
    let at = LonLat::new(-122.4950, 37.8120);
    TrackFacts {
        position: Fact::new(at, T0),
        alt_m: None,
        alt_t: None,
        velocity_mps: Some([0.0, 0.0]),
        pos_sigma_m: 15.0,
        sigma_growth_mps: Some(1.0),
        confidence: 0.9,
        affiliation: Fact::new(Affiliation::Hostile, T0),
        affiliation_confidence: 0.8,
        identity: Some("small_fast_boat".into()),
        identity_confidence: 0.9,
        sources: vec!["3:radar".into()],
        source_t: [("3:radar".to_string(), T0)].into_iter().collect(),
        domain: Domain::Surface,
        last_seen_t: T0,
        declared_by: Some("op-1".into()),
        declaration: None,
        review_flags: Vec::new(),
        manoeuvre: None,
        mean_velocity_mps: None,
        first_seen_t: None,
        velocity_sigma_mps: None,
    }
}

fn nomination() -> IntentSpec {
    IntentSpec {
        id: IntentId(1),
        created_t: T0,
        source: human(),
        task: Task::Engage {
            track: "T-0001".into(),
            effect_radius_m: 75.0,
            window: None,
            fallback: Fallback::Abort,
            weapon: Some(GUN.into()),
        },
        uids: vec![uid("bull-1")],
        constraints: Default::default(),
        rationale: None,
    }
}

/// The rounds a gun fired stay spent across a restart, on the same vehicle (rules 24's carry, now
/// counting each burst's rounds): the next run's weapon check says 482 of 500 left, and replay
/// with the earlier log beside it agrees.
#[test]
fn a_guns_rounds_are_carried_over_a_restart() {
    let a = run(CEASES, &RunOptions::default()).log;
    let (head, seq) = log_head(&a).unwrap();
    let d = owed_debts(&a);
    let carry = CarriedFrom {
        log: "a.jsonl".into(),
        head: Some(head),
        last_seq: Some(seq),
        owed: d.owed,
        pending: d.pending,
        spent: d.spent,
        rounds: d.rounds,
        ..Default::default()
    };
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let e = Engine::start(policy(), None, roster(), vec![carry], Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    assert_eq!(e.platform_rounds(&uid("bull-1"), None).spent(GUN), 18);
    let mut w = WorldSnapshot::new(T0);
    w.areas = Some(areas());
    w.vehicles.insert(uid("bull-1"), vehicle("ugv_armed", LonLat::new(-122.4950, 37.7990)));
    w.tracks.insert("T-0001".into(), boat());
    let spec = nomination();
    let inputs = e.dry_run_inputs(&spec, &w, &terrain());
    let c = engage::evaluate(&spec, &inputs, &e.policy().rules).into_iter().find(|c| c.rule == RuleId::Weapon).unwrap();
    assert_eq!(c.outcome, Outcome::Pass, "{c:?}");
    assert!(c.detail.contains("482 of 500 rounds left"), "{}", c.detail);
    let logs = [("a.jsonl", a.as_str())];
    let r = replay_str_with(&log.text(), &ReplayOptions { carried_logs: &logs, ..ReplayOptions::default() }).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

/// Rules 25 logs replay unchanged: the fixtures of rules 24 and 25 under their own rules; a
/// scenario of rules 26 run under rules 25 leaves the M2 inert (its nomination refused) and logs
/// none of rules 26's markers; and a rules 26 log relabelled rules 25 is caught.
#[test]
fn rules_25_logs_replay_unchanged() {
    for (name, version) in [
        ("v24-engagement-scenario-01.jsonl", LOADOUT_SINCE),
        ("v24-rounds-spent-then-another-weapon-scenario-119.jsonl", LOADOUT_SINCE),
        ("v25-engagement-scenario-01.jsonl", DAMAGE_SINCE),
        ("v25-weapons-out-of-action-scenario-121.jsonl", DAMAGE_SINCE),
    ] {
        let text = std::fs::read_to_string(root().join("crates/murmur-decide/tests/data/audit").join(name)).unwrap();
        let r = replay_str_with(&text, &ReplayOptions { rules: RulesAccepted::Exactly(version), ..ReplayOptions::default() }).unwrap();
        assert!(r.ok(), "{name}: {:?}", r.mismatches);
    }
    let v25 = RunOptions { rules_version: Some(DAMAGE_SINCE), ..RunOptions::default() };
    let then = run(CEASES, &v25);
    assert!(then.replay_ok, "{:?}", then.replay_mismatches);
    assert_eq!(then.final_state.as_deref(), Some("rejected"));
    assert!(then.failed_rules.iter().any(|r| r == "weapon"), "{:?}", then.failed_rules);
    for marker in ["engage.burst", "engage.fire_ended", "\"fire_from\":", "line_of_fire"] {
        assert!(!then.log.contains(marker), "rules 25 logs {marker}");
    }
    let now = run(CEASES, &RunOptions::default());
    let relabelled = forge(&now.log, |ev| {
        if let EventBody::RunStarted { rules_version, .. } = &mut ev[0].body {
            *rules_version = Some(DAMAGE_SINCE);
        }
    });
    let rr = replay_str_with(&relabelled, &ReplayOptions { rules: RulesAccepted::Exactly(DAMAGE_SINCE), ..ReplayOptions::default() }).unwrap();
    assert!(!rr.ok());
    assert!(replay_str(&now.log).unwrap().ok());
}
