//! Decision-rules versioning: every log a released build wrote replays with the rules it was
//! produced under (`run.started`'s `rules_version`; none means version 1), but only when the
//! verifier accepts those rules (by default only the current ones), and nothing in the log
//! may contradict its claim. The fixtures in `tests/data/audit/` are committed logs of every
//! rules version so far; a rules change that alters decisions bumps `RULES_VERSION`, gates
//! the change on it, and adds a log of the new version here.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use murmur_decide::audit::{EventBody, parse_log};
use murmur_decide::replay::{
    ReplayOptions, ReplayReport, RulesAccepted, replay, replay_str, replay_str_with,
};
use murmur_decide::*;

fn fixture(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/audit")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// (file, the rules version it was written under).
const LOGS: [(&str, u32); 5] = [
    // master before Phase E: no engagement
    ("v1-pre-engagement-live-c7.jsonl", 1),
    // master 977854f: an engagement, the first frozen-plan rules
    ("v1-engagement-phase-e-review2.jsonl", 1),
    // master 6da8909: a live engagement aborted with its fallback, and a scenario's effect
    ("v2-engagement-live.jsonl", 2),
    ("v2-engagement-scenario-01.jsonl", 2),
    // rules 3: a scenario's effect
    ("v3-engagement-scenario-01.jsonl", 3),
];

fn accepting(rules: RulesAccepted) -> ReplayOptions<'static> {
    ReplayOptions {
        rules,
        ..ReplayOptions::default()
    }
}

fn with(text: &str, rules: RulesAccepted) -> ReplayReport {
    replay_str_with(text, &accepting(rules)).unwrap()
}

fn whats(r: &ReplayReport) -> Vec<&str> {
    r.mismatches.iter().map(|m| m.what.as_str()).collect()
}

/// Renumber and re-hash a log's lines, every other byte kept (a forger's tool).
fn rechain(lines: &[String]) -> String {
    let mut prev = "0".repeat(64);
    let mut out = String::new();
    for (n, l) in lines.iter().enumerate() {
        let t_at = l.find(",\"t\":").unwrap();
        let p_at = l.find(",\"prev\":\"").unwrap();
        let after = &l[p_at + 9 + 64 + 1..];
        let line = format!(
            "{{\"seq\":{}{},\"prev\":\"{prev}\"{after}",
            n + 1,
            &l[t_at..p_at]
        );
        prev = murmur_decide::hash::sha256_hex(line.as_bytes());
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

#[test]
fn logs_of_every_rules_version_replay_when_their_rules_are_accepted() {
    for (name, version) in LOGS {
        let text = fixture(name);
        for rules in [RulesAccepted::Exactly(version), RulesAccepted::AtLeast(version)] {
            let r = with(&text, rules);
            assert!(r.ok(), "{name} {rules:?}: {:?}", r.mismatches);
            assert_eq!(r.rules_version, version, "{name}");
        }
    }
    // and a fixture of the current rules exists, which replays by default
    let (name, _) = LOGS.iter().find(|(_, v)| *v == RULES_VERSION).unwrap();
    let r = replay_str(&fixture(name)).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    assert!(r.build.is_some(), "a current log names its build");
}

#[test]
fn an_older_log_is_refused_unless_its_rules_are_accepted() {
    for (name, version) in LOGS.iter().filter(|(_, v)| *v < RULES_VERSION) {
        let text = fixture(name);
        // the default: the current rules only
        let r = replay_str(&text).unwrap();
        assert!(
            whats(&r).iter().any(|w| w.contains("older decision rules than this replay accepts")),
            "{name}: {:?}",
            whats(&r)
        );
        // asked for exactly another version
        let r = with(&text, RulesAccepted::Exactly(version + 1));
        assert!(
            whats(&r).iter().any(|w| w.contains("other decision rules")),
            "{name}: {:?}",
            whats(&r)
        );
    }
}

#[test]
fn a_log_is_recomputed_with_its_own_rules_not_todays() {
    // the 977854f log claimed for the current rules no longer reproduces: the rules changed
    // decisions (one engagement per platform, the target's motion model, GPS at release)
    let mut events = parse_log(&fixture("v1-engagement-phase-e-review2.jsonl")).unwrap();
    for e in &mut events {
        if let EventBody::RunStarted {
            rules_version,
            policy_schema,
            ..
        } = &mut e.body
        {
            *rules_version = Some(RULES_VERSION);
            *policy_schema = Some(POLICY_SCHEMA);
        }
    }
    let r = replay(&events);
    assert!(
        whats(&r).contains(&"check results differ"),
        "{:?}",
        whats(&r)
    );
}

#[test]
fn a_log_from_newer_rules_is_flagged() {
    let mut events = parse_log(&fixture("v3-engagement-scenario-01.jsonl")).unwrap();
    for e in &mut events {
        if let EventBody::RunStarted { rules_version, .. } = &mut e.body {
            *rules_version = Some(RULES_VERSION + 1);
        }
    }
    let r = replay(&events);
    assert!(
        whats(&r).iter().any(|w| w.contains("newer decision rules")),
        "{:?}",
        whats(&r)
    );
}

#[test]
fn a_run_records_its_rules_schema_and_build() {
    let text = fixture("v3-engagement-scenario-01.jsonl");
    let first = text.lines().next().unwrap();
    assert!(first.contains(&format!("\"rules_version\":{RULES_VERSION}")), "{first}");
    assert!(first.contains(&format!("\"policy_schema\":{POLICY_SCHEMA}")), "{first}");
    assert!(first.contains("\"build\":{\"commit\":\""), "{first}");
}

#[test]
fn a_run_started_inside_a_log_is_flagged() {
    // a second run.started, claiming rules 1, spliced into a rules 3 log: the rest would be
    // recomputed with rules 1
    let mut ls = lines(&fixture("v3-engagement-scenario-01.jsonl"));
    let v1 = ls[0]
        .replacen(&format!("\"rules_version\":{RULES_VERSION},"), "", 1)
        .replacen(&format!("\"policy_schema\":{POLICY_SCHEMA},"), "", 1);
    let v1 = match (v1.find(",\"build\":{"), v1.find(",\"policy\":")) {
        (Some(b), Some(p)) => format!("{}{}", &v1[..b], &v1[p..]),
        _ => panic!("no build"),
    };
    ls.insert(10, v1);
    let r = with(&rechain(&ls), RulesAccepted::AtLeast(1));
    assert!(
        whats(&r).iter().any(|w| w.contains("run.started is not the log's first event")),
        "{:?}",
        whats(&r)
    );
}

#[test]
fn a_log_that_does_not_start_with_run_started_is_flagged() {
    let ls = lines(&fixture("v3-engagement-scenario-01.jsonl"));
    let r = replay_str(&rechain(&ls[1..])).unwrap();
    assert!(
        whats(&r).iter().any(|w| w.contains("does not start with run.started")),
        "{:?}",
        whats(&r)
    );
}

#[test]
fn a_downgraded_claim_is_caught_by_what_the_log_carries() {
    // a rules 2 log stripped of its version to pass as rules 1, every hash recomputed: it
    // still carries what only rules 2 builds write
    let mut ls = lines(&fixture("v2-engagement-live.jsonl"));
    assert!(ls[0].contains("\"rules_version\":2,\"policy_schema\":2,"));
    ls[0] = ls[0].replacen("\"rules_version\":2,\"policy_schema\":2,", "", 1);
    let r = with(&rechain(&ls), RulesAccepted::Exactly(1));
    let w = whats(&r);
    for marker in ["checked_t", "GPS fix fact", "GPS glitch fact", "platform_free"] {
        assert!(
            w.iter().any(|m| m.contains(marker) && m.contains("only builds of rules 2")),
            "{marker}: {w:?}"
        );
    }
    assert!(w.iter().any(|m| m.contains("a key of a later policy schema")), "{w:?}");
    // a rules 3 log claiming rules 2 still names its build, which only rules 3 builds write
    let mut ls = lines(&fixture("v3-engagement-scenario-01.jsonl"));
    ls[0] = ls[0].replacen(&format!("\"rules_version\":{RULES_VERSION},"), "\"rules_version\":2,", 1);
    let r = with(&rechain(&ls), RulesAccepted::Exactly(2));
    assert!(
        whats(&r).iter().any(|m| m.contains("the build in run.started") && m.contains("rules 3")),
        "{:?}",
        whats(&r)
    );
}

#[test]
fn a_policy_schema_claim_must_fit_its_keys() {
    let mut ls = lines(&fixture("v2-engagement-scenario-01.jsonl"));
    ls[0] = ls[0].replacen("\"policy_schema\":2,", "\"policy_schema\":1,", 1);
    let r = with(&rechain(&ls), RulesAccepted::Exactly(2));
    assert!(
        whats(&r).iter().any(|m| m.contains("a key of a later policy schema")),
        "{:?}",
        whats(&r)
    );
    // one of rules_version and policy_schema without the other: no build writes that
    let mut ls = lines(&fixture("v2-engagement-scenario-01.jsonl"));
    ls[0] = ls[0].replacen("\"policy_schema\":2,", "", 1);
    let r = with(&rechain(&ls), RulesAccepted::Exactly(2));
    assert!(
        whats(&r).iter().any(|m| m.contains("without the other")),
        "{:?}",
        whats(&r)
    );
    // a schema newer than replay knows
    let mut ls = lines(&fixture("v3-engagement-scenario-01.jsonl"));
    ls[0] = ls[0].replacen(
        &format!("\"policy_schema\":{POLICY_SCHEMA},"),
        &format!("\"policy_schema\":{},", POLICY_SCHEMA + 1),
        1,
    );
    let r = replay_str(&rechain(&ls)).unwrap();
    assert!(
        whats(&r).iter().any(|m| m.contains("policy schema is newer")),
        "{:?}",
        whats(&r)
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
