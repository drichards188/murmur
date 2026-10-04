//! Decision-rules versioning: every log a released build wrote replays with the rules it was
//! produced under (`run.started`'s `rules_version`; none means version 1), and old policy
//! key names still parse. The fixtures in `tests/data/audit/` are committed logs of every
//! rules version so far; a rules change that alters decisions bumps `RULES_VERSION`, gates
//! the change on it, and adds a log of the new version here.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use murmur_decide::audit::{EventBody, parse_log};
use murmur_decide::replay::{replay, replay_str};
use murmur_decide::*;

fn fixture(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/audit")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// (file, the rules version it was written under).
const LOGS: [(&str, u32); 4] = [
    // master before Phase E: no engagement
    ("v1-pre-engagement-live-c7.jsonl", 1),
    // master 977854f: an engagement, the first frozen-plan rules
    ("v1-engagement-phase-e-review2.jsonl", 1),
    // rules 2: a live engagement aborted with its fallback, and a scenario's effect
    ("v2-engagement-live.jsonl", 2),
    ("v2-engagement-scenario-01.jsonl", 2),
];

#[test]
fn logs_of_every_rules_version_replay() {
    for (name, version) in LOGS {
        let r = replay_str(&fixture(name)).unwrap();
        assert!(r.ok(), "{name}: {:?}", r.mismatches);
        assert_eq!(r.rules_version, version, "{name}");
    }
    // and a fixture of the current rules exists
    assert!(LOGS.iter().any(|(_, v)| *v == RULES_VERSION));
}

#[test]
fn a_log_is_recomputed_with_its_own_rules_not_todays() {
    // the 977854f log claimed for the current rules no longer reproduces: the rules changed
    // decisions (one engagement per platform, the target's motion model, GPS at release)
    let mut events = parse_log(&fixture("v1-engagement-phase-e-review2.jsonl")).unwrap();
    for e in &mut events {
        if let EventBody::RunStarted { rules_version, .. } = &mut e.body {
            *rules_version = Some(RULES_VERSION);
        }
    }
    let r = replay(&events);
    assert!(
        r.mismatches
            .iter()
            .any(|m| m.what == "check results differ"),
        "{:?}",
        r.mismatches.iter().map(|m| &m.what).collect::<Vec<_>>()
    );
}

#[test]
fn a_log_from_newer_rules_is_flagged() {
    let mut events = parse_log(&fixture("v2-engagement-scenario-01.jsonl")).unwrap();
    for e in &mut events {
        if let EventBody::RunStarted { rules_version, .. } = &mut e.body {
            *rules_version = Some(RULES_VERSION + 1);
        }
    }
    let r = replay(&events);
    assert!(
        r.mismatches
            .iter()
            .any(|m| m.what.contains("newer decision rules")),
        "{:?}",
        r.mismatches
    );
}

#[test]
fn a_run_records_its_rules_and_schema() {
    let text = fixture("v2-engagement-scenario-01.jsonl");
    let first = text.lines().next().unwrap();
    assert!(
        first.contains(&format!("\"rules_version\":{RULES_VERSION}")),
        "{first}"
    );
    assert!(
        first.contains(&format!("\"policy_schema\":{POLICY_SCHEMA}")),
        "{first}"
    );
}

/// The fixture's `run.started` line, with its policy's `token_ttl_s` under another name.
fn run_started_with_ttl_key(key: &str) -> String {
    let v1 = fixture("v1-engagement-phase-e-review2.jsonl");
    let first = v1.lines().next().unwrap();
    assert!(first.contains("\"token_ttl_s\":15.0,"));
    first.replace("\"token_ttl_s\":15.0,", &format!("\"{key}\":20.0,"))
}

#[test]
fn old_policy_key_names_still_parse_and_reserialize() {
    // an unreleased build called the picture token's lifetime `seen_max_age_s`: a policy
    // file with that name still loads
    let text = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../sim/policy.toml"),
    )
    .unwrap()
    .replace("token_ttl_s = 15 ", "seen_max_age_s = 20 ");
    let p = Policy::from_toml(&text).unwrap();
    assert_eq!(p.rules.engage_rules().token_ttl(), 20.0);
    // and a logged policy with it parses and is still its line's serialization (replay
    // requires that), with the old name's value in force
    let old = run_started_with_ttl_key("seen_max_age_s");
    let e: audit::Event = serde_json::from_str(&old).unwrap();
    assert_eq!(serde_json::to_string(&e).unwrap(), old);
    let EventBody::RunStarted { policy, .. } = &e.body else {
        panic!("not run.started")
    };
    assert_eq!(policy.rules.engage_rules().token_ttl(), 20.0);
    // a log of schema 1 (no schema 2 keys) re-serializes byte for byte too
    let v1 = fixture("v1-engagement-phase-e-review2.jsonl");
    let first = v1.lines().next().unwrap();
    let e: audit::Event = serde_json::from_str(first).unwrap();
    assert_eq!(serde_json::to_string(&e).unwrap(), first);
}

#[test]
fn an_unknown_policy_key_still_fails_to_parse() {
    // the old names are known keys, not a loosened parser
    let line = run_started_with_ttl_key("ttl_whatever_s");
    assert!(serde_json::from_str::<audit::Event>(&line).is_err());
}
