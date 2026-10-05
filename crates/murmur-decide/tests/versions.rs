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
const LOGS: [(&str, u32); 29] = [
    // master before Phase E: no engagement
    ("v1-pre-engagement-live-c7.jsonl", 1),
    // master 977854f: an engagement, the first frozen-plan rules
    ("v1-engagement-phase-e-review2.jsonl", 1),
    // master 6da8909: a live engagement aborted with its fallback, and a scenario's effect
    ("v2-engagement-live.jsonl", 2),
    ("v2-engagement-scenario-01.jsonl", 2),
    // rules 3: a scenario's effect, and a live effect by the USV while our fixed wing
    // circles (built from a clean 2146fef, anchored as it ran)
    ("v3-engagement-scenario-01.jsonl", 3),
    ("v3-engagement-live.jsonl", 3),
    // rules 4: the same scenario, its run.started naming the operator roster and its
    // engagement order the loiter radius
    ("v4-engagement-scenario-01.jsonl", 4),
    // rules 5: the same scenario, its policy carrying the [queue] for unreachable vehicles
    // (policy schema 3)
    ("v5-engagement-scenario-01.jsonl", 5),
    // rules 5, live: orders queued for an AUV under water and a copter whose link was cut
    // (the run the next one resumed from; that one is checked against it below)
    ("v5-queue-live-before-restart.jsonl", 5),
    // rules 6 (clean builds): the same scenario with the fallback preloaded as the platform's
    // lost-link action (the order's preload, its write-ahead, set, re-reads and restore), and
    // scenario 54, a lost friendly an operator accepted (engage.accepted, the catalog's climb
    // rate in the world facts); and a live run, anchored as it ran: two nominations rejected
    // for a fast ferry by the point, a third clear of a far fast ferry and, by altitude, of
    // our aircraft at 9,000 m, its USV's lost-link action preloaded, re-read, released,
    // restored
    ("v6-engagement-scenario-01.jsonl", 6),
    ("v6-lost-friendly-accepted-scenario-54.jsonl", 6),
    ("v6-engagement-live.jsonl", 6),
    // rules 6, master 981cdf3: scenario 66, the small airship nominated; its
    // fallback_preload refusal as rules 6 worded it (see the test below)
    ("v6-airship-nomination-scenario-66.jsonl", 6),
    // rules 7 (the queue's follow-ups; a provisional number, so the table names the constant):
    // the same scenario; scenario 67, an operator's hold queued for the engaging USV whose link
    // is cut supersedes the engagement (no fallback); scenario 68, an orbit ordered after an
    // abort waits for the USV's own lost-link values to be confirmed back; scenario 71, an
    // orbit refused for them under a policy that never queues an orbit still supersedes the
    // engagement under way, in the refusal's step (no fallback); and a live run,
    // anchored as it ran (an orbit for the engaging USV queued for its own values, superseding
    // the engagement, sent once they were back, its start logged ahead; a second engagement
    // aborted while the USV's link was cut, its values back the second the link returned; a
    // third the same, with an orbit queued in the outage that went only after the restore)
    ("v7-engagement-scenario-01.jsonl", QUEUE_FOLLOWUPS_SINCE),
    ("v7-queued-order-supersedes-scenario-67.jsonl", QUEUE_FOLLOWUPS_SINCE),
    ("v7-mission-waits-for-restore-scenario-68.jsonl", QUEUE_FOLLOWUPS_SINCE),
    ("v7-refused-mission-supersedes-scenario-71.jsonl", QUEUE_FOLLOWUPS_SINCE),
    ("v7-queue-followups-live.jsonl", QUEUE_FOLLOWUPS_SINCE),
    // rules 8 (operators authenticated; clean builds): the same scenario, its roster saying
    // its operators were asserted (auth dev: the scenario runner); and a live run, anchored as
    // it ran, whose operators logged in with their own passwords and stepped up to sign (one
    // of them at the console)
    ("v8-engagement-scenario-01.jsonl", OPERATOR_AUTH_SINCE),
    ("v8-auth-live.jsonl", OPERATOR_AUTH_SINCE),
    // rules 9 (rule-based identification; the table names the constant): the same scenario,
    // its declaration through the identification ledger (`ident.declared` by op-1, filled
    // into every picture); scenario 72, a clear hostile act declared by rule and confirmed;
    // 73, an act between two tracks close together, flagged; 74, a neutral alongside a
    // hostile, raised to suspect and flagged, then a misattributed act on it flagged for its
    // conflicting evidence; 75, a false rule declaration nominated before a confirmation and
    // revoked en route (the engagement stopped, cause revoked); 76, an unconfirmed rule
    // declaration protected in another engagement's keep-out until confirmed; and a live run,
    // anchored as it ran (see the test below)
    ("v9-engagement-scenario-01.jsonl", RULE_ID_SINCE),
    ("v9-hostile-act-declared-scenario-72.jsonl", RULE_ID_SINCE),
    ("v9-ambiguous-attribution-flagged-scenario-73.jsonl", RULE_ID_SINCE),
    ("v9-neutral-alongside-flagged-scenario-74.jsonl", RULE_ID_SINCE),
    ("v9-revoked-mid-engagement-scenario-75.jsonl", RULE_ID_SINCE),
    ("v9-bystander-protected-scenario-76.jsonl", RULE_ID_SINCE),
    // 77 (the review of rules 9): a bystander whose declaration exempted it from an
    // engagement's keep-out, revoked: the engagement stopped in the revocation's step
    ("v9-revoked-bystander-scenario-77.jsonl", RULE_ID_SINCE),
    // 78 (the second review): the same revocation before the engagement's first picture,
    // named and stopped in the revocation's step
    ("v9-revoked-before-picture-scenario-78.jsonl", RULE_ID_SINCE),
    ("v9-identification-live.jsonl", RULE_ID_SINCE),
];

/// The committed log of scenario 66 under rules 6, written by master's murmur-scenarios.
const V6_AIRSHIP: &str = "v6-airship-nomination-scenario-66.jsonl";

/// The committed log of the current rules.
const CURRENT: &str = "v9-engagement-scenario-01.jsonl";

/// The committed scenario log of rules 6 (the lost-link preload).
const V6_SCENARIO: &str = "v6-engagement-scenario-01.jsonl";

/// The committed scenario log of rules 5 (the queue).
const V5_SCENARIO: &str = "v5-engagement-scenario-01.jsonl";

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
        for rules in [
            RulesAccepted::Exactly(version),
            RulesAccepted::AtLeast(version),
        ] {
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

/// A check's detail is compared by replay, so its wording is part of the rules version: the
/// airship's `fallback_preload` refusal still reads as rules 6 wrote it ("only lost-link
/// action is Land"), although Blimp 4.8's GCS failsafe can only disarm (murmur-3zj). A
/// build that rewords it without bumping `RULES_VERSION` fails here on a master-written
/// log.
#[test]
fn a_rules_6_airship_nomination_written_by_master_still_replays() {
    let text = fixture(V6_AIRSHIP);
    // rules 6 accepted explicitly: the wording it pins is rules 6's
    let r = with(&text, RulesAccepted::Exactly(6));
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.rules_version, 6);
    assert!(r.checks_recomputed > 0);
    let wording = "Blimp 4.8's only lost-link action is Land (FS_GCS_ENABLE 5): fallback return can't be its own failsafe";
    assert!(text.contains(wording), "the rules 6 wording of the refusal");
    let events = parse_log(&text).unwrap();
    assert!(
        events.iter().any(|e| matches!(&e.body, EventBody::IntentState { to: State::Rejected, reason, .. } if reason.contains("fallback_preload"))),
        "rejected for fallback_preload"
    );
}

#[test]
fn the_live_log_matches_the_anchors_written_as_it_ran() {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/anchors/v3-engagement-live.anchors");
    let anchors = std::fs::read_to_string(p).unwrap();
    let log = fixture("v3-engagement-live.jsonl");
    let a = anchor::verify(&log, &anchors);
    assert!(a.ok(), "{:?}", a.problems);
    assert!(a.shutdown, "a clean stop anchored the head");
    assert_eq!(a.after_last, 0);
    // the bridge reported our circling fixed wing's circle, and the release went ahead
    assert!(log.contains("\"circling\":{\"value\":{\"center\""));
    let r = with(&log, RulesAccepted::Exactly(3));
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!((r.effects, r.assessments), (1, 1));
    assert_eq!(r.build.as_ref().map(|b| b.dirty), Some(Some(false)));
}

#[test]
fn the_rules_6_live_log_preloads_restores_and_reads_altitude() {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/anchors/v6-engagement-live.anchors");
    let anchors = std::fs::read_to_string(p).unwrap();
    let log = fixture("v6-engagement-live.jsonl");
    let a = anchor::verify(&log, &anchors);
    assert!(a.ok(), "{:?}", a.problems);
    assert!(a.shutdown, "a clean stop anchored the head");
    let r = with(&log, RulesAccepted::Exactly(6));
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!((r.engagements, r.effects, r.assessments), (3, 1, 1));
    assert_eq!(r.build.as_ref().map(|b| b.dirty), Some(Some(false)));
    assert!(r.unrestored.is_empty(), "{:?}", r.unrestored);
    // the fast ferry by the point blocked twice; then the USV's FS_ACTION 6 (Loiter) and its
    // 5 s timer (its own: 1 and 30 s) read back before the intercept, read again every few
    // seconds before the release, and put back after
    let events = parse_log(&log).unwrap();
    let rejected = events
        .iter()
        .filter(|e| matches!(&e.body, EventBody::IntentState { to: State::Rejected, reason, .. } if reason.contains("keep_out_tracks: track T-") && reason.contains("(neutral, ferry)")))
        .count();
    assert_eq!(rejected, 2);
    let preloads: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::EngagePreload { phase, before, readback, confirmed, .. } => Some((e.t, *phase, before.clone(), readback.clone(), *confirmed)),
            _ => None,
        })
        .collect();
    // its own values logged ahead of the write (with the USV's identity), then set
    let (intent, set, restore) = (&preloads[0], &preloads[1], &preloads[preloads.len() - 1]);
    assert_eq!(intent.1, engage::PreloadPhase::Intent);
    assert!(intent.4);
    assert_eq!(intent.2, set.2, "the set put aside what its write-ahead logged");
    assert!(log.contains("\"phase\":\"intent\"") && log.contains("\"vehicle\":{\"platform\":\"magura\",\"callsign\":\"VIPER 1\""));
    assert_eq!(set.1, engage::PreloadPhase::Set);
    assert!(set.4);
    assert_eq!((set.2["FS_ACTION"], set.2["FS_GCS_TIMEOUT"]), (1.0, 30.0));
    assert_eq!((set.3["FS_ACTION"], set.3["FS_GCS_TIMEOUT"]), (6.0, 5.0));
    let verifies = &preloads[2..preloads.len() - 1];
    assert!(!verifies.is_empty() && verifies.iter().all(|v| v.1 == engage::PreloadPhase::Verify && v.4), "{preloads:?}");
    let effect_t = events.iter().find(|e| matches!(e.body, EventBody::EngageEffect { .. })).unwrap().t;
    let last_read = verifies.iter().map(|v| v.0).filter(|t| *t <= effect_t).max().unwrap();
    assert!(effect_t.ms_since(last_read) <= engage::PRELOAD_FRESH_MS, "read back {} ms before the release", effect_t.ms_since(last_read));
    assert_eq!(restore.1, engage::PreloadPhase::Restore);
    assert!(restore.4);
    assert_eq!(restore.3, set.2, "its own values back");
    // our vehicles' altitude in the world facts, and the keep-out read in 3D: our MALE
    // aircraft 9,000 m over the point, clear even diving at 44 m/s; the tracks' altitudes
    // with their times and their sources' last hits
    assert!(log.contains("\"alt_msl_m\":{\"value\":9000.0"));
    assert!(log.contains("clear by altitude: 5 at 9000 m"));
    assert!(log.contains("\"alt_t\":") && log.contains("\"source_t\":{"));
    assert!(log.contains("\"identity\":\"light_aircraft\""));
}

/// The position of the first line of `lines` from `from` on that has every needle.
fn at(lines: &[String], from: usize, needles: &[&str]) -> usize {
    lines[from..]
        .iter()
        .position(|l| needles.iter().all(|n| l.contains(n)))
        .map(|i| i + from)
        .unwrap_or_else(|| panic!("no line from {from} with {needles:?}"))
}

#[test]
fn the_rules_7_live_log_holds_missions_for_the_restore_and_logs_starts_ahead() {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/anchors/v7-queue-followups-live.anchors");
    let anchors = std::fs::read_to_string(p).unwrap();
    let log = fixture("v7-queue-followups-live.jsonl");
    let a = anchor::verify(&log, &anchors);
    assert!(a.ok(), "{:?}", a.problems);
    assert!(a.shutdown, "a clean stop anchored the head");
    // a log of rules 7 now, replayed with its own rules
    let r = with(&log, RulesAccepted::Exactly(QUEUE_FOLLOWUPS_SINCE));
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.rules_version, QUEUE_FOLLOWUPS_SINCE);
    assert_eq!(r.build.as_ref().map(|b| b.dirty), Some(Some(false)));
    assert!(r.unrestored.is_empty(), "{:?}", r.unrestored);
    // thirteen nominations rejected first (a ferry, then a light aircraft, predicted inside
    // the keep-out), then the three engagements
    assert_eq!((r.engagements, r.effects, r.aborts), (16, 0, 3));
    let ls = lines(&log);
    // the first engagement under way: an operator's orbit for its USV waits for the USV's own
    // lost-link values, and, queued, supersedes the engagement (no fallback commanded)
    let queued = at(&ls, 0, &["\"type\":\"intent.queued\"", "its own lost-link failsafe", "\"restore_owed\":{\"2\""]);
    let stop = at(&ls, queued, &["\"type\":\"engage.stopped\"", "\"kind\":\"superseded\"", "queued until its vehicles can be reached"]);
    assert!(ls[stop].contains("\"fallback\":null"), "{}", ls[stop]);
    // its own values back, then the orbit, its start logged ahead of sending it
    let restored = at(&ls, stop, &["\"type\":\"engage.preload\"", "\"phase\":\"restore\"", "\"confirmed\":true"]);
    let sent = at(&ls, stop, &["\"type\":\"intent.dispatched\"", "\"kind\":\"orbit\""]);
    assert!(restored < sent, "restored at {restored}, the orbit sent at {sent}");
    let ahead = at(&ls, sent, &["\"type\":\"op.state\"", "\"state\":\"starting\"", "\"ahead\":true"]);
    assert!(ahead > sent);
    // the second engagement, aborted by an operator while its USV was cut off: a restore tried
    // as the link went fails, the next goes when it is back; the third the same, with an orbit
    // queued in the outage: once the USV is heard again its own values go back first
    let abort = at(&ls, ahead, &["\"type\":\"engage.stopped\"", "\"kind\":\"operator\""]);
    let silent = at(&ls, abort, &["\"type\":\"intent.queued\"", "silent for"]);
    let back = at(&ls, silent, &["\"type\":\"engage.preload\"", "\"phase\":\"restore\"", "\"confirmed\":true"]);
    let second = at(&ls, silent, &["\"type\":\"intent.dispatched\"", "\"kind\":\"orbit\""]);
    assert!(back < second, "restored at {back}, the second orbit sent at {second}");
}

#[test]
fn an_older_log_is_refused_unless_its_rules_are_accepted() {
    for (name, version) in LOGS.iter().filter(|(_, v)| *v < RULES_VERSION) {
        let text = fixture(name);
        // the default: the current rules only
        let r = replay_str(&text).unwrap();
        assert!(
            whats(&r)
                .iter()
                .any(|w| w.contains("older decision rules than this replay accepts")),
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
    let mut events = parse_log(&fixture(CURRENT)).unwrap();
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
    let text = fixture(CURRENT);
    let first = text.lines().next().unwrap();
    assert!(
        first.contains(&format!("\"rules_version\":{RULES_VERSION}")),
        "{first}"
    );
    assert!(
        first.contains(&format!("\"policy_schema\":{POLICY_SCHEMA}")),
        "{first}"
    );
    assert!(first.contains("\"build\":{\"commit\":\""), "{first}");
    // rules 4: who could act as an operator, and with engagement authority; rules 8: how they
    // proved it (the scenario runner asserts its ids: dev)
    assert!(
        first.contains("\"roster\":{\"operators\":[\"op-1\",\"op-2\",\"op-3\"],\"engage\":[\"op-2\",\"op-3\"],\"auth\":\"dev\"}"),
        "{first}"
    );
    let r = replay_str(&text).unwrap();
    assert_eq!(r.auth, Some(AuthMode::Dev));
    assert!(r.ok() && !r.authenticated(), "a dev run is not evidence about people");
}

#[test]
fn a_run_started_inside_a_log_is_flagged() {
    // a second run.started, claiming rules 1, spliced into a current log: the rest would be
    // recomputed with rules 1
    let mut ls = lines(&fixture(CURRENT));
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
        whats(&r)
            .iter()
            .any(|w| w.contains("run.started is not the log's first event")),
        "{:?}",
        whats(&r)
    );
}

#[test]
fn a_log_that_does_not_start_with_run_started_is_flagged() {
    let ls = lines(&fixture(CURRENT));
    let r = replay_str(&rechain(&ls[1..])).unwrap();
    assert!(
        whats(&r)
            .iter()
            .any(|w| w.contains("does not start with run.started")),
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
    for marker in [
        "checked_t",
        "GPS fix fact",
        "GPS glitch fact",
        "platform_free",
    ] {
        assert!(
            w.iter()
                .any(|m| m.contains(marker) && m.contains("only builds of rules 2")),
            "{marker}: {w:?}"
        );
    }
    assert!(
        w.iter()
            .any(|m| m.contains("a key of a later policy schema")),
        "{w:?}"
    );
    // a rules 3 log claiming rules 2 still names its build, which only rules 3 builds write
    let mut ls = lines(&fixture("v3-engagement-scenario-01.jsonl"));
    assert!(ls[0].contains("\"rules_version\":3,"));
    ls[0] = ls[0].replacen("\"rules_version\":3,", "\"rules_version\":2,", 1);
    let r = with(&rechain(&ls), RulesAccepted::Exactly(2));
    assert!(
        whats(&r)
            .iter()
            .any(|m| m.contains("the build in run.started") && m.contains("rules 3")),
        "{:?}",
        whats(&r)
    );
    // a rules 6 log claiming rules 5 still carries the preload and the fallback_preload check,
    // and one with an accepted lost friendly carries the acceptance and the climb rates
    for (name, markers) in [
        (
            V6_SCENARIO,
            &["a lost-link preload report", "an engagement order's lost-link preload", "a fallback_preload check"][..],
        ),
        (
            "v6-lost-friendly-accepted-scenario-54.jsonl",
            &["an acceptance of a lost vehicle", "an accepted lost vehicle in decision inputs", "a platform's climb rate"][..],
        ),
        ("v6-engagement-live.jsonl", &["a vehicle's altitude fact", "a lost-link preload report", "a track altitude's time"][..]),
    ] {
        let mut ls = lines(&fixture(name));
        assert!(ls[0].contains("\"rules_version\":6,"));
        ls[0] = ls[0].replacen("\"rules_version\":6,", "\"rules_version\":5,", 1);
        let r = with(&rechain(&ls), RulesAccepted::Exactly(5));
        let w = whats(&r);
        for marker in markers {
            assert!(
                w.iter().any(|m| m.contains(marker) && m.contains("only builds of rules 6")),
                "{name} {marker}: {w:?}"
            );
        }
    }
    // a rules 7 log claiming rules 6 still carries its lost-link values owed back in a
    // decision's facts and its starts logged ahead
    let v7 = QUEUE_FOLLOWUPS_SINCE;
    for (name, markers) in [
        ("v7-mission-waits-for-restore-scenario-68.jsonl", &["a vehicle's lost-link values owed back"][..]),
        ("v7-queue-followups-live.jsonl", &["a vehicle's lost-link values owed back", "a vehicle start logged ahead of sending it"][..]),
    ] {
        let mut ls = lines(&fixture(name));
        assert!(ls[0].contains(&format!("\"rules_version\":{v7},")));
        ls[0] = ls[0].replacen(&format!("\"rules_version\":{v7},"), "\"rules_version\":6,", 1);
        let r = with(&rechain(&ls), RulesAccepted::Exactly(6));
        let w = whats(&r);
        for marker in markers {
            assert!(
                w.iter().any(|m| m.contains(marker) && m.contains(&format!("only builds of rules {v7}"))),
                "{name} {marker}: {w:?}"
            );
        }
    }
    // and scenario 67's engagement, superseded by a queued order, and 71's, by a refused one,
    // no rules 6 build could stop so
    for name in ["v7-queued-order-supersedes-scenario-67.jsonl", "v7-refused-mission-supersedes-scenario-71.jsonl"] {
        let mut ls = lines(&fixture(name));
        ls[0] = ls[0].replacen(&format!("\"rules_version\":{v7},"), "\"rules_version\":6,", 1);
        let r = with(&rechain(&ls), RulesAccepted::Exactly(6));
        assert!(whats(&r).contains(&"superseded by an order that was not dispatched"), "{name}: {:?}", whats(&r));
    }
    // a rules 4 log claiming rules 3 still names its roster and its order's loiter radius
    let mut ls = lines(&fixture("v4-engagement-scenario-01.jsonl"));
    assert!(ls[0].contains("\"rules_version\":4,"));
    ls[0] = ls[0].replacen("\"rules_version\":4,", "\"rules_version\":3,", 1);
    let r = with(&rechain(&ls), RulesAccepted::Exactly(3));
    let w = whats(&r);
    for marker in ["the operator roster in run.started", "loiter radius"] {
        assert!(
            w.iter().any(|m| m.contains(marker) && m.contains("only builds of rules 4")),
            "{marker}: {w:?}"
        );
    }
    // and one that dropped its roster still claims rules 4, which always name one
    let mut ls = lines(&fixture("v4-engagement-scenario-01.jsonl"));
    let (b, p) = (ls[0].find(",\"roster\":{").unwrap(), ls[0].find(",\"policy\":").unwrap());
    ls[0] = format!("{}{}", &ls[0][..b], &ls[0][p..]);
    let r = with(&rechain(&ls), RulesAccepted::Exactly(4));
    assert!(
        whats(&r).contains(&"a rules 4 log names its operator roster in run.started"),
        "{:?}",
        whats(&r)
    );
    // a rules 5 log claiming rules 4 (and policy schema 2) still carries its [queue]
    let mut ls = lines(&fixture(V5_SCENARIO));
    assert!(ls[0].contains("\"rules_version\":5,\"policy_schema\":3,"));
    ls[0] = ls[0].replacen(
        "\"rules_version\":5,\"policy_schema\":3,",
        "\"rules_version\":4,\"policy_schema\":2,",
        1,
    );
    let r = with(&rechain(&ls), RulesAccepted::Exactly(4));
    let w = whats(&r);
    assert!(
        w.iter()
            .any(|m| m.contains("a [queue] policy") && m.contains("only builds of rules 5")),
        "{w:?}"
    );
    assert!(
        w.iter().any(|m| m.contains("a key of a later policy schema")),
        "{w:?}"
    );
}

#[test]
fn a_policy_schema_claim_must_fit_its_keys() {
    let mut ls = lines(&fixture("v2-engagement-scenario-01.jsonl"));
    ls[0] = ls[0].replacen("\"policy_schema\":2,", "\"policy_schema\":1,", 1);
    let r = with(&rechain(&ls), RulesAccepted::Exactly(2));
    assert!(
        whats(&r)
            .iter()
            .any(|m| m.contains("a key of a later policy schema")),
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
    // a rules 6 policy (schema 4 keys) claiming schema 3
    let mut ls = lines(&fixture(CURRENT));
    ls[0] = ls[0].replacen(&format!("\"policy_schema\":{POLICY_SCHEMA},"), "\"policy_schema\":3,", 1);
    let r = replay_str(&rechain(&ls)).unwrap();
    let later: Vec<&str> = r
        .mismatches
        .iter()
        .filter(|m| m.what.contains("a key of a later policy schema"))
        .map(|m| m.logged.as_str())
        .collect();
    for key in ["track_velocity_sigma_mps", "track_accel_mps2", "vertical_rate_mps", "keep_out_vertical_m", "track_alt_max_age_s"] {
        assert!(later.contains(&format!("rules.engage.{key} (schema 4)").as_str()), "{key}: {later:?}");
    }
    // a schema newer than replay knows
    let mut ls = lines(&fixture(CURRENT));
    ls[0] = ls[0].replacen(
        &format!("\"policy_schema\":{POLICY_SCHEMA},"),
        &format!("\"policy_schema\":{},", POLICY_SCHEMA + 1),
        1,
    );
    let r = replay_str(&rechain(&ls)).unwrap();
    assert!(
        whats(&r)
            .iter()
            .any(|m| m.contains("policy schema is newer")),
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

/// Rules 5, live (murmur-5p4, after its review): the queue across a bridge crash, from a clean
/// build. The first run queued an orbit for an AUV under water (sent when it surfaced), and
/// sent an orbit to a SITL copter whose link was cut before it could start it: the order came
/// back to the queue (`intent.requeued`) and went again once the link was back, re-checked.
/// The bridge was then killed (SIGKILL: no shutdown anchor, every decision anchored up to the
/// last event) with the copter en route; the second run carried both orbits over and followed
/// the copter's (ArduCopter's own mission compared item by item: not sent again), then queued
/// and sent a boat's patrol through a Starlink outage, and stopped cleanly. Each was anchored
/// as it ran.
const QUEUE_BEFORE: &str = "v5-queue-live-before-restart.jsonl";
const QUEUE_AFTER: &str = "v5-queue-live-after-restart.jsonl";

fn anchors_of(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/anchors")
        .join(name.replace(".jsonl", ".anchors"));
    std::fs::read_to_string(p).unwrap()
}

#[test]
fn the_live_queue_logs_replay_across_the_restart() {
    let before = fixture(QUEUE_BEFORE);
    let after = fixture(QUEUE_AFTER);
    // the first run crashed: its anchors hold up to the last one, with no shutdown anchor,
    // and its last decision is anchored (review 2: no unanchored tail of decisions)
    let a = anchor::verify(&before, &anchors_of(QUEUE_BEFORE));
    assert!(!a.shutdown);
    assert!(
        a.problems.iter().all(|p| p.what.starts_with("no shutdown anchor")),
        "{:?}",
        a.problems
    );
    let last_decision = lines(&before)
        .iter()
        .rev()
        .find(|l| l.contains("\"type\":\"intent.state\"") || l.contains("\"type\":\"op.state\""))
        .and_then(|l| serde_json::from_str::<audit::Event>(l).ok())
        .map(|e| e.seq);
    assert!(a.last_anchored >= last_decision, "{:?} {last_decision:?}", a.last_anchored);
    let a = anchor::verify(&after, &anchors_of(QUEUE_AFTER));
    assert!(a.ok(), "{:?}", a.problems);
    assert!(a.shutdown, "a clean stop anchored the head");
    // rules 5 logs: replayed with their rules accepted
    let r = with(&before, RulesAccepted::Exactly(5));
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.rules_version, 5);
    assert!(r.queued >= 1 && r.requeued >= 1 && r.recheck_dispatched >= 2, "{r:?}");
    assert_eq!(r.build.as_ref().map(|b| b.dirty), Some(Some(false)));
    // the second run, checked against the first
    let opts = |previous| ReplayOptions {
        previous,
        rules: RulesAccepted::Exactly(5),
        ..ReplayOptions::default()
    };
    let r = replay_str_with(&after, &opts(Some(&before))).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    assert!(r.resumed >= 1, "{r:?}");
    assert!(r.recheck_dispatched >= 1, "{r:?}");
    // the orbit started in the first run is followed in the second, never sent again
    assert!(after.contains("\"type\":\"intent.resumed\""));
    assert!(after.contains("\"state\":\"dispatched\""));
    // review T6, on the live log: the AUV's orbit was still waiting to be followed when the
    // second run stopped (its follow op's last state `queued`); a third run would still
    // follow it, as the AUV had started it in the first
    let auv = r
        .open
        .iter()
        .find(|o| o.spec.uids == vec![murmur_decide::Uid::new("1")])
        .expect("the AUV's orbit is open");
    assert_eq!(auv.ops.get(&murmur_decide::Uid::new("1")).map(String::as_str), Some("queued"));
    assert_eq!(auv.committed(), vec![murmur_decide::Uid::new("1")]);
    assert_eq!(auv.resumes_as(), State::Dispatched);
    // not without it, nor against an edited one
    let r = with(&after, RulesAccepted::Exactly(5));
    assert!(whats(&r).iter().any(|w| w.contains("not verified")), "{:?}", whats(&r));
    let mut ls = lines(&before);
    let n = ls.len();
    ls[n - 1] = ls[n - 1].replacen("\"t\":", "\"t\": ", 1);
    let edited = ls.join("\n");
    let r = replay_str_with(&after, &opts(Some(&edited))).unwrap();
    assert!(!r.ok(), "an edited previous log is not the one carried from");
}

/// Rules 8, live (murmur-der.1, after the security review), from a clean build of the merged
/// head, anchored as it ran: op-3 signed in at the console in a browser first; a wrong password
/// for op-2 (`auth.refused`), an unknown operator (nothing logged), a bare header, an agent's
/// key on an operator endpoint, a request from another origin and one carrying two session
/// cookies refused; op-1 and op-2 logged in over the HTTP API; a stranger's five wrong
/// passwords for op-3 (each `auth.refused`, the fifth locking op-3's next login for 30 s; op-3's
/// own password then refused unchecked, on stdout only); an agent on its own key; op-1, without
/// engagement authority, declared and nominated; op-2's signature refused without a password,
/// with a wrong one and with op-3's (each logged `step_up.refused`), then made with their own;
/// op-2 again over a second login, stepping up, refused by the engine (already signed); op-3's
/// signature from the console, stepped up, on the session the lock never touched; the effect
/// released and assessed; every session closed at logout.
const AUTH_LIVE: &str = "v8-auth-live.jsonl";

#[test]
fn the_rules_8_live_log_binds_every_signature_to_a_person() {
    let log = fixture(AUTH_LIVE);
    let a = anchor::verify(&log, &anchors_of(AUTH_LIVE));
    assert!(a.ok(), "{:?}", a.problems);
    assert!(a.shutdown, "a clean stop anchored the head");
    // a log of rules 8 now, replayed with its own rules
    let r = with(&log, RulesAccepted::Exactly(OPERATOR_AUTH_SINCE));
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.rules_version, OPERATOR_AUTH_SINCE);
    assert_eq!(r.build.as_ref().map(|b| b.dirty), Some(Some(false)));
    assert_eq!(r.auth, Some(AuthMode::Credentials));
    assert!(r.authenticated(), "a credentials run that reproduced");
    assert_eq!((r.step_ups, r.stepped_signatures, r.sessions_closed), (3, 2, 4));
    let ids = |v: &[&str]| v.iter().map(|o| OperatorId::new(*o)).collect::<std::collections::BTreeSet<_>>();
    assert_eq!((r.people.clone(), r.scripted.clone()), (ids(&["op-1", "op-2", "op-3"]), ids(&[])));
    // the failed logins on the record (op-2's typo, the stranger's five for op-3, the last
    // locking it), never a password
    assert_eq!(r.logins_refused, 6);
    assert_eq!(log.matches("\"type\":\"auth.refused\",\"operator\":\"op-3\",\"reason\":\"wrong password\"").count(), 5);
    assert!(log.contains("\"type\":\"auth.refused\",\"operator\":\"op-3\",\"reason\":\"wrong password\",\"locked_s\":30"));
    assert!(!log.contains("mallory") && !log.contains("passphrase"));
    // the lock ended no session: op-3 signed on the session opened before it
    assert!(!log.contains("step-up failed repeatedly"));
    assert_eq!((r.engagements, r.concurrences, r.approvals, r.effects, r.assessments), (1, 1, 1, 1, 1));
    let events = parse_log(&log).unwrap();
    // every session opened on a checked password, every one closed at logout
    let actions: Vec<(String, u64, String, String)> = events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::OperatorAction { operator, session, action, detail, .. } => {
                Some((operator.to_string(), session.0, action.clone(), detail.clone()))
            }
            _ => None,
        })
        .collect();
    let opened = actions.iter().filter(|a| a.2 == "session.opened").count();
    assert_eq!(opened, 4);
    assert_eq!(log.matches("\"action\":\"session.opened\"").count(), log.matches("\"action\":\"session.opened\",\"detail\":\"\",\"auth\":\"password\"").count() + log.matches("\"grants\":{\"engage\":true},\"auth\":\"password\"").count());
    assert_eq!(actions.iter().filter(|a| a.2 == "session.closed" && a.3 == "logout").count(), 4);
    // the attempts the person signing could not back with their credential, on the record
    let refused: Vec<&str> = actions.iter().filter(|a| a.2 == "step_up.refused").map(|a| a.3.as_str()).collect();
    assert_eq!(refused, vec!["i2: no password given", "i2: wrong password", "i2: wrong password"]);
    assert!(actions.iter().any(|a| a.0 == "op-2" && a.1 == 4 && a.2 == "approval.refused" && a.3.contains("already signed")));
    // two people, each signature quoting a step-up under its own session
    let sigs: Vec<(String, u64, Option<u64>)> = events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::ApprovalConcurred { by, session, step_up, .. } | EventBody::ApprovalGranted { by, session, step_up, .. } => {
                Some((by.to_string(), session.0, step_up.map(|s| s.seq)))
            }
            _ => None,
        })
        .collect();
    assert_eq!(sigs.len(), 2);
    assert_eq!((sigs[0].0.as_str(), sigs[1].0.as_str()), ("op-2", "op-3"));
    assert_eq!(sigs[1].1, 1, "op-3's console session, opened before the lock");
    // the nomination was made in op-1's live session; the agent's intent in none
    let created: Vec<(bool, Option<u64>)> = events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::IntentCreated { intent, session } => Some((intent.source.is_agent(), session.map(|s| s.0))),
            _ => None,
        })
        .collect();
    assert_eq!(created, vec![(true, None), (false, Some(2))]);
    for (by, session, seq) in &sigs {
        let seq = seq.expect("every engagement signature steps up");
        let step = events.iter().find(|e| e.seq == seq).expect("the step-up");
        assert!(
            matches!(&step.body, EventBody::OperatorAction { operator, session: s, action, .. } if operator.as_str() == by && s.0 == *session && action == "step_up"),
            "{step:?}"
        );
    }
    // the agent connected on its key, named by the key's id
    assert!(log.contains("\"action\":\"session.connected\",\"detail\":\"via API key "));
}

#[test]
fn a_rules_8_log_claiming_rules_7_still_carries_its_authentication() {
    let (now, before) = (OPERATOR_AUTH_SINCE, OPERATOR_AUTH_SINCE - 1);
    let mut ls = lines(&fixture(AUTH_LIVE));
    assert!(ls[0].contains(&format!("\"rules_version\":{now},")));
    ls[0] = ls[0].replacen(&format!("\"rules_version\":{now},"), &format!("\"rules_version\":{before},"), 1);
    let r = with(&rechain(&ls), RulesAccepted::Exactly(before));
    let w = whats(&r);
    for marker in ["how an operator was authenticated", "a step-up", "a session's end", "a refused login"] {
        assert!(
            w.iter().any(|m| m.contains(marker) && m.contains(&format!("only builds of rules {OPERATOR_AUTH_SINCE}"))),
            "{marker}: {w:?}"
        );
    }
}

/// Rules 9, live, from a clean build of the review round's head (7508c90), anchored as it ran: a
/// credentials run on its own ports (stand-ins, the engagement demo fleet,
/// `sim/scenarios/golden-gate-ident.toml`, the honest tracker). Before the acts, a truth
/// message with a fabricated contact and act was posted four ways without the stack's
/// simulation token (none, a guess, an agent's key, an operator's session): each refused, 401,
/// nothing logged. Three scripted acts came in through the sensing model: the fast boat firing
/// on the USV, observed 97 m from its track (attribution 0.46, flagged); the skiff alongside the
/// crab boat firing on the sensor ship (0.54 to the crab boat, 0.46 to the skiff: flagged); the fast
/// boat again, declared hostile by rule (0.99). op-1 (no engagement authority) confirmed the
/// rule declaration under their session, op-2 dismissed the ambiguous flag and op-1 confirmed
/// the other; op-1 nominated the switchblade on the boat, op-2 and op-3 looked and signed, each
/// with their password again; op-3 then revoked the declaration, and the engagement stopped
/// before anything else (cause revoked), its fallback commanded; everyone logged out.
const IDENT_LIVE: &str = "v9-identification-live.jsonl";

#[test]
fn the_rules_9_live_log_declares_by_rule_and_operators_review_it_under_their_sessions() {
    let log = fixture(IDENT_LIVE);
    let a = anchor::verify(&log, &anchors_of(IDENT_LIVE));
    assert!(a.ok(), "{:?}", a.problems);
    assert!(a.shutdown, "a clean stop anchored the head");
    let r = with(&log, RulesAccepted::Exactly(RULE_ID_SINCE));
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.rules_version, RULE_ID_SINCE);
    assert_eq!(r.build.as_ref().map(|b| b.dirty), Some(Some(false)));
    assert_eq!(r.auth, Some(AuthMode::Credentials));
    assert!(r.authenticated(), "a credentials run that reproduced");
    assert_eq!(
        (r.acts, r.rule_declarations, r.operator_declarations, r.flags, r.confirmations, r.revocations, r.flags_resolved),
        (3, 1, 0, 2, 1, 1, 2)
    );
    assert_eq!((r.engagements, r.concurrences, r.approvals, r.aborts, r.effects), (1, 1, 1, 1, 0));
    assert_eq!((r.step_ups, r.stepped_signatures), (2, 2), "the signatures stepped up, the confirmation did not");
    let events = parse_log(&log).unwrap();
    // the rule's declaration is the engine's: no session, no step-up, its evidence named
    let declared: Vec<&ident::Declaration> = events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::IdentDeclared { declaration } => Some(&**declaration),
            _ => None,
        })
        .collect();
    assert_eq!(declared.len(), 1);
    assert!(
        matches!(&declared[0].by, ident::Declarer::Rule { rule, evidence } if rule == "hostile_act" && evidence.act == "A-3" && evidence.attribution >= evidence.threshold),
        "{:?}",
        declared[0]
    );
    // every human act under its operator's own session
    let sessions: std::collections::BTreeMap<u64, String> = events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::OperatorAction { operator, session, action, .. } if action == "session.opened" => {
                Some((session.0, operator.to_string()))
            }
            _ => None,
        })
        .collect();
    let mut acts = Vec::new();
    for e in &events {
        match &e.body {
            EventBody::IdentConfirmed { by, session, step_up, .. } => {
                assert!(step_up.is_none());
                acts.push(("confirmed", by.to_string(), session.0));
            }
            EventBody::IdentFlagResolved { by, session, .. } => acts.push(("resolved", by.to_string(), session.0)),
            EventBody::IdentRevoked { by, session, engagements, .. } => {
                assert_eq!(engagements.len(), 1, "the engagement in play on it");
                acts.push(("revoked", by.to_string(), session.0));
            }
            _ => {}
        }
    }
    assert_eq!(acts.len(), 4);
    for (what, by, session) in &acts {
        assert_eq!(sessions.get(session), Some(by), "{what} by {by} under session {session}");
    }
    assert!(acts.iter().any(|a| a.0 == "confirmed" && a.1 == "op-1"));
    assert!(acts.iter().any(|a| a.0 == "revoked" && a.1 == "op-3"));
    // the revocation stopped the engagement next, cause revoked
    let i = events.iter().position(|e| matches!(e.body, EventBody::IdentRevoked { .. })).unwrap();
    assert!(
        matches!(&events[i + 1].body, EventBody::EngageStopped { cause: engage::StopCause::Revoked { .. }, .. }),
        "{:?}",
        events[i + 1]
    );
    // every act came in on the stack's own simulation channel (its token checked); the
    // review's injection, tried four ways before the acts (no token, a guess, an agent's key,
    // an operator's session), was refused before anything read it: nothing of it is here
    let channels: Vec<Option<String>> = events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::IdentAct { act, .. } => Some(act.channel.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(channels, vec![Some("sim:contacts".to_string()); 3]);
    assert!(!log.contains("injected") && !log.contains("fishing-x"));
    // the signers' pictures showed how the target was declared
    let pictures: Vec<&str> = log.lines().filter(|l| l.contains("\"type\":\"picture.issued\"")).collect();
    assert_eq!(pictures.len(), 2);
    assert!(pictures.iter().all(|l| l.contains("\"rule\":\"hostile_act\"")), "{pictures:?}");
}
