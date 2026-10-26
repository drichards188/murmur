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
const LOGS: [(&str, u32); 62] = [
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
    // rules 10 (passkeys; the table names the constant): the same scenario (the runner asserts
    // its ids: no passkeys) and a live run, anchored as it ran, whose signers registered
    // passkeys and signed with them (see the test below)
    ("v10-engagement-scenario-01.jsonl", PASSKEY_SINCE),
    ("v10-passkeys-live.jsonl", PASSKEY_SINCE),
    // rules 11 (space tasks followed up after dispatch, covers over the area; the table
    // names the constant): the same scenario; the engine on the element-set fixture (an
    // area cover watched to its end, a collect cancelled before its pass, a collect
    // re-checked on a reloaded segment and missed; written by tests/space.rs); and a live
    // run with its anchors (stand-ins, a cover over the Golden Gate box evaluated at its
    // sample points, a collect chosen, dispatched and cancelled before its pass; see the
    // test below)
    ("v11-engagement-scenario-01.jsonl", SPACE_FOLLOWUPS_SINCE),
    ("v11-space-followups-engine.jsonl", SPACE_FOLLOWUPS_SINCE),
    ("v11-space-followups-live.jsonl", SPACE_FOLLOWUPS_SINCE),
    // rules 12 (the reviews' follow-ups; the table names the constant): the same scenario, its
    // switchblade spent by its effect and owed nothing back after the release (restore.closed,
    // expended, as scenario 80); scenario 79, a young track blocking by its reach until the
    // tracker has its average velocity; 81, a refused orbit naming the older hold still queued;
    // 82, an operator writing off values that will never be confirmed back. The live logs,
    // across two crashes, are checked against each other below
    ("v12-engagement-scenario-01.jsonl", REVIEW_FOLLOWUPS_SINCE),
    ("v12-young-track-blocks-scenario-79.jsonl", REVIEW_FOLLOWUPS_SINCE),
    ("v12-refusal-names-queued-order-scenario-81.jsonl", REVIEW_FOLLOWUPS_SINCE),
    ("v12-restore-written-off-scenario-82.jsonl", REVIEW_FOLLOWUPS_SINCE),
    // rules 13 (space answers held to their time; the table names the constant): the same
    // scenario, and the engine on the element-set fixture as for rules 11, every answer with
    // when it was computed (see the test below)
    ("v13-engagement-scenario-01.jsonl", SPACE_REPLAY_SINCE),
    ("v13-space-replay-engine.jsonl", SPACE_REPLAY_SINCE),
    // rules 14 (an aircraft counts anywhere it could turn or speed up to, murmur-oho; the table
    // names the constant): the same scenario, and scenario 83, an aircraft flying away 2 km from
    // the effect point rejected by its reach, the same nomination pending once it is 12 km out
    ("v14-engagement-scenario-01.jsonl", AIRCRAFT_REACH_SINCE),
    ("v14-aircraft-may-turn-back-scenario-83.jsonl", AIRCRAFT_REACH_SINCE),
    // rules 15 (the carries' follow-ups; the table names the constant): the same scenario;
    // scenario 84, the switchblade spent by its effect, un-spent by op-2 (engagement authority)
    // once its engagement ended and it was heard since, its own values owed again and restored
    // before an orbit went; 85, a nomination refused while a failed carry may owe its USV values,
    // then taken once the log was written off. The live logs, across two crashes, are checked
    // against each other below
    ("v15-engagement-scenario-01.jsonl", CARRY_FOLLOWUPS_SINCE),
    ("v15-spent-munition-unspent-scenario-84.jsonl", CARRY_FOLLOWUPS_SINCE),
    ("v15-nomination-refused-while-a-carry-may-owe-scenario-85.jsonl", CARRY_FOLLOWUPS_SINCE),
    // rules 16 (lost contacts, murmur-533 and its reviews; the table names the constant; a clean
    // build of 75c505b, rules 15 merged): the same scenario; scenario 86, a fishing boat dropped by the tracker by
    // the target, still blocking; 87, accepted at the nomination; 88, an accepted tour boat seen
    // again by its AIS under a new track id, its claim confirmed, the live track aborting the
    // engagement; 90, accepted while the nomination waits; 93, a spoofed AIS's claim confirmed,
    // then void
    ("v16-engagement-scenario-01.jsonl", LOST_CONTACT_SINCE),
    ("v16-lost-contact-blocks-scenario-86.jsonl", LOST_CONTACT_SINCE),
    ("v16-lost-contact-accepted-scenario-87.jsonl", LOST_CONTACT_SINCE),
    ("v16-lost-contact-reappears-scenario-88.jsonl", LOST_CONTACT_SINCE),
    ("v16-lost-contact-accepted-waiting-scenario-90.jsonl", LOST_CONTACT_SINCE),
    ("v16-lost-contact-claimed-by-a-spoof-scenario-93.jsonl", LOST_CONTACT_SINCE),
    // and a live run with its anchors (see the test below)
    ("v16-lost-contacts-live.jsonl", LOST_CONTACT_SINCE),
    // rules 17 (a clean build of this branch): scenario 01; 95, an orbit and a nomination
    // refused for a transit across the shore; 96, a patrol round the point accepted
    ("v17-engagement-scenario-01.jsonl", ROUTE_SURFACE_SINCE),
    ("v17-transit-crosses-land-scenario-95.jsonl", ROUTE_SURFACE_SINCE),
    ("v17-route-round-the-point-scenario-96.jsonl", ROUTE_SURFACE_SINCE),
    // rules 18 (murmur-vni, a track predicted with its own velocity error; a clean build of this
    // branch): scenario 01; 97, a nomination rejected for a boat whose velocity is known to 3 m/s,
    // then waiting for its signers once the boat is farther; 98, an engagement under way aborted
    // when a ferry's velocity error grows from 0.5 to 4 m/s
    ("v18-engagement-scenario-01.jsonl", VELOCITY_SIGMA_SINCE),
    ("v18-velocity-known-to-3-mps-scenario-97.jsonl", VELOCITY_SIGMA_SINCE),
    ("v18-velocity-error-grows-under-way-scenario-98.jsonl", VELOCITY_SIGMA_SINCE),
    // rules 19 (bd murmur-j0x, a live track accepted as behaving now; a clean build of 502e0da):
    // scenario 01; 99, an aircraft accepted at the nomination, held to the effect; 100, the
    // acceptance lapsed by a jump off the accepted path, a token from before it refused, accepted
    // again; 105, a late lapse by a turn that stops the engagement under way
    ("v19-engagement-scenario-01.jsonl", ACCEPT_BEHAVING_SINCE),
    ("v19-aircraft-accepted-as-behaving-scenario-99.jsonl", ACCEPT_BEHAVING_SINCE),
    ("v19-behaving-acceptance-lapses-on-a-jump-scenario-100.jsonl", ACCEPT_BEHAVING_SINCE),
    ("v19-behaving-acceptance-lapses-under-way-scenario-105.jsonl", ACCEPT_BEHAVING_SINCE),
];

/// The committed log of scenario 66 under rules 6, written by master's murmur-scenarios.
const V6_AIRSHIP: &str = "v6-airship-nomination-scenario-66.jsonl";

/// The committed log of the current rules.
const CURRENT: &str = "v19-engagement-scenario-01.jsonl";

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

/// Rules 9, live, from a clean build of the second review round's head (02eacca), anchored as it
/// ran: a credentials run on its own ports (stand-ins, the engagement demo fleet,
/// `sim/scenarios/golden-gate-ident.toml`, the two-model tracker with its fixes dated by the
/// truth's own clock; tracks carry their average velocity and, after a manoeuvre, the fastest
/// they went). Before the acts, a truth message with a fabricated contact and act was posted
/// four ways without the stack's simulation token (none, a guess, an agent's key, an operator's
/// session): each refused, 401, nothing logged. Three scripted acts came in through the sensing
/// model: the fast boat firing on the USV, observed 83 m from its track (attribution 0.81,
/// flagged); the skiff alongside the crab boat firing on the sensor ship (0.53 to the skiff,
/// 0.47 to the crab boat: flagged); the fast boat again, declared hostile by rule (0.99). op-1 (no engagement authority) confirmed the
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

/// Rules 10, live, from a clean build of 2eff85c, anchored as it ran: a credentials run on its
/// own ports (stand-ins, the engagement demo fleet, `sim/scenarios/golden-gate-ident.toml`), the
/// shipped policy with `[approval] step_up_factor = "passkey"`, the console in Chrome on
/// localhost with a virtual authenticator (the DevTools protocol's WebAuthn domain; on
/// 127.0.0.1 the sign-in screen offered no passkey and pointed to localhost). op-2 registered a
/// passkey through a one-time invitation link (`murmur-auth passkey-invite`) and signed in with
/// it; op-3 signed in by password and registered one from the operator menu, refused with a
/// wrong password, then with their own; op-1 declared the fast boat hostile and nominated the
/// switchblade; op-2, signed in by password over the API, tried to sign with the password and
/// was refused unchecked (the policy asks for a passkey); op-3 (still the password session)
/// stepped up with their passkey but the picture they were shown had expired when the signature
/// arrived (refused, the step-up left unused); op-2 signed in by passkey and signed with it,
/// then op-3, signed in by passkey too; the effect was released and assessed (the expended
/// switchblade never answered its restore); everyone signed out and the bridge stopped.
const PASSKEY_LIVE: &str = "v10-passkeys-live.jsonl";

#[test]
fn the_rules_10_live_log_signs_with_passkeys_bound_to_their_acts() {
    let log = fixture(PASSKEY_LIVE);
    let a = anchor::verify(&log, &anchors_of(PASSKEY_LIVE));
    assert!(a.ok(), "{:?}", a.problems);
    assert!(a.shutdown, "a clean stop anchored the head");
    let r = with(&log, RulesAccepted::Exactly(PASSKEY_SINCE));
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.rules_version, PASSKEY_SINCE);
    assert_eq!(r.build.as_ref().map(|b| b.dirty), Some(Some(false)));
    assert!(r.authenticated(), "a credentials run that reproduced");
    // two registrations during the run (an invitation, a session quoting its password
    // step-up), three passkey logins, three passkey step-ups (one left unused: its signature came
    // after the picture expired), one password step-up (the registration's)
    assert_eq!((r.passkeys_registered, r.passkey_logins, r.passkey_step_ups, r.password_step_ups), (2, 3, 3, 1));
    // three nominations, two rejected for the engagement zone while the boat came to its hold
    assert_eq!((r.stepped_signatures, r.engagements, r.effects, r.assessments), (2, 3, 1, 1));
    let first = log.lines().next().unwrap();
    assert!(
        first.contains("\"passkeys\":{\"rp_id\":\"localhost\",\"origins\":[\"http://localhost:27173\",\"http://localhost:27765\"]}"),
        "{first}"
    );
    assert!(first.contains("\"step_up_factor\":\"passkey\""), "{first}");
    // the refusals on the record: a registration's wrong password, a password signature where
    // the policy asks for a passkey (not even checked)
    let events = parse_log(&log).unwrap();
    let refused: Vec<&str> = events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::OperatorAction { action, detail, .. } if action == "step_up.refused" => Some(detail.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(refused, vec!["register a passkey: wrong password", "i3: a password where the policy asks for a passkey"]);
    // each signature quotes a passkey step-up made for it: its intent, the picture token it
    // quotes and that token's hash
    let mut signed = 0;
    for e in &events {
        let (by, token, step) = match &e.body {
            EventBody::ApprovalConcurred { by, token: Some(t), step_up: Some(s), .. }
            | EventBody::ApprovalGranted { by, token: Some(t), step_up: Some(s), .. } => (by, t, s),
            _ => continue,
        };
        let step = events.iter().find(|x| x.seq == step.seq).expect("the step-up");
        let EventBody::OperatorAction { operator, auth, passkey: Some(u), .. } = &step.body else {
            panic!("not a passkey step-up: {step:?}")
        };
        assert_eq!((operator, *auth), (by, Some(AuthMethod::Passkey)));
        assert_eq!(
            u.purpose,
            StepUpFor::Sign { intent: IntentId(3), token: Some(token.id), picture_sha256: Some(passkey::picture_sha256(token)) }
        );
        signed += 1;
    }
    assert_eq!(signed, 2);
    // forged, on the browser's own assertions: op-2's signature relabelled a password's (its
    // assertion dropped), where the policy asks for a passkey; op-3's assertion moved onto
    // op-2's step-up
    let mut ev = events.clone();
    for e in ev.iter_mut() {
        if let EventBody::OperatorAction { operator, action, auth, passkey, .. } = &mut e.body
            && action == "step_up"
            && operator.as_str() == "op-2"
        {
            *auth = Some(AuthMethod::Password);
            *passkey = None;
        }
    }
    let w: Vec<String> = replay(&rechain_events(ev)).mismatches.into_iter().map(|m| m.what).collect();
    assert!(w.iter().any(|m| m.contains("by password where the policy requires a passkey")), "{w:?}");
    let op3 = events
        .iter()
        .find_map(|e| match &e.body {
            EventBody::OperatorAction { operator, passkey: Some(u), action, .. } if action == "step_up" && operator.as_str() == "op-3" => {
                Some(u.clone())
            }
            _ => None,
        })
        .unwrap();
    let mut ev = events.clone();
    for e in ev.iter_mut() {
        if let EventBody::OperatorAction { operator, action, passkey: Some(u), .. } = &mut e.body
            && action == "step_up"
            && operator.as_str() == "op-2"
        {
            *u = op3.clone();
        }
    }
    let w: Vec<String> = replay(&rechain_events(ev)).mismatches.into_iter().map(|m| m.what).collect();
    assert!(w.iter().any(|m| m.starts_with("passkey step-up:")), "{w:?}");
}

/// The events renumbered and re-hashed as their serialization (a forger's tool).
fn rechain_events(mut events: Vec<audit::Event>) -> Vec<audit::Event> {
    let mut prev = murmur_decide::hash::GENESIS.to_string();
    for (i, e) in events.iter_mut().enumerate() {
        e.seq = i as u64 + 1;
        e.prev = prev.clone();
        prev = murmur_decide::hash::sha256_hex(serde_json::to_string(&*e).unwrap().as_bytes());
    }
    events
}

/// Rules 12, live (murmur-cq3, murmur-qjf and their review; replayed with rules 12 accepted),
/// from a clean build of the rules 12 head, anchored as they ran, stand-ins at the Gate on their own ports. The first of these
/// runs (`CRASH_1`) started on a `.last-run` pointer left empty (as a crash mid-write leaves
/// it): it started all the same, recording the pointer as a failed carry (owed, unknown) in
/// `run.started` and alerting; an operator's orbit took the USV near the fast boat, two
/// signers sent it to engage, and the bridge was killed (SIGKILL) a second after the USV's
/// lost-link preload was confirmed, its own values owed. The next run (`CRASH_2`) recorded
/// that log in `run.started` with its head and what it owed (the USV's values, the vehicle
/// they were preloaded on, the pointer still pending) and the pointer tried again, failed;
/// op-1 wrote the pointer off; the values went back when the USV answered; it engaged again
/// the same way and was killed the same way. The last (`AFTER`) carried from it with the USV
/// spawned 1.2 km east of where it was preloaded (its home about 980 m from the one logged
/// then, as a vehicle that armed elsewhere reports) and heard, but its downlink cut for
/// 150 s: an orbit for it waited in the queue for the values carried over (the attempts
/// unanswered; the home alone said nothing), went the second the restore confirmed, and the
/// run stopped cleanly owing nothing.
const CRASH_1: (&str, &str) = ("v12-carry-live-crash-1.jsonl", "20261006T173331Z-2705405.jsonl");
const CRASH_2: (&str, &str) = ("v12-carry-live-crash-2.jsonl", "20261006T173940Z-2717315.jsonl");
const AFTER: &str = "v12-carry-live-after.jsonl";

#[test]
fn the_rules_12_live_logs_carry_across_two_crashes_in_one_line_each() {
    let (c1, c2, after) = (fixture(CRASH_1.0), fixture(CRASH_2.0), fixture(AFTER));
    // both crashed: anchored up to their last decision, no shutdown anchor; the last stopped
    // cleanly
    for (name, text) in [(CRASH_1.0, &c1), (CRASH_2.0, &c2)] {
        let a = anchor::verify(text, &anchors_of(name));
        assert!(!a.shutdown, "{name}");
        assert!(a.problems.iter().all(|p| p.what.starts_with("no shutdown anchor")), "{name}: {:?}", a.problems);
    }
    let a = anchor::verify(&after, &anchors_of(AFTER));
    assert!(a.ok() && a.shutdown, "{:?}", a.problems);
    // the first: started on an empty pointer, recorded as a failed carry under its own name;
    // it owes the USV's values when it ends, and the pointer's unknown debt
    let first = c1.lines().next().unwrap();
    assert!(
        first.contains("\"carried_from\":[{\"log\":\".last-run-27765\",\"error\":\"the pointer is empty"),
        "{first}"
    );
    let r = with(&c1, RulesAccepted::Exactly(REVIEW_FOLLOWUPS_SINCE));
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.build.as_ref().map(|b| b.dirty), Some(Some(false)));
    assert_eq!(r.carried_restores.len(), 1);
    assert!(r.carried_restores[0].failed.as_deref().is_some_and(|f| f.starts_with("the pointer is empty")), "{:?}", r.carried_restores);
    assert_eq!(r.unrestored.len(), 2, "{:?}", r.unrestored);
    let owed = murmur_decide::replay::owed_debts(&c1);
    assert_eq!(owed.owed.len(), 1);
    assert_eq!(owed.owed[0].vehicle.as_ref().and_then(|v| v.callsign.as_deref()), Some("VIPER 1"));
    assert_eq!(owed.pending, [".last-run-27765"]);
    // the second: its first line names the first log, its head and last seq, what it owed and
    // the pointer still pending, then the pointer, failed again; checked against the first,
    // the values put back on the same vehicle, the pointer written off by op-1
    let (head, seq) = murmur_decide::replay::log_head(&c1).unwrap();
    let first = c2.lines().next().unwrap();
    assert!(first.contains(&format!("\"carried_from\":[{{\"log\":\"{}\",\"head\":\"{head}\",\"last_seq\":{seq},\"owed\":[{{\"uid\":\"2\"", CRASH_1.1)), "{first}");
    assert!(first.contains("\"pending\":[\".last-run-27765\"]},{\"log\":\".last-run-27765\",\"error\":"), "{first}");
    let logs = [(CRASH_1.1, c1.as_str())];
    let opts = ReplayOptions {
        carried_logs: &logs,
        rules: RulesAccepted::Exactly(REVIEW_FOLLOWUPS_SINCE),
        ..ReplayOptions::default()
    };
    let r = replay_str_with(&c2, &opts).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    assert!(r.carried_restores[0].checked && r.carried_restores[0].restores == 1, "{:?}", r.carried_restores);
    assert_eq!(r.restores_closed.len(), 1, "{:?}", r.restores_closed);
    assert!(r.restores_closed[0].starts_with("earlier log .last-run-27765: what it owed (its carry had failed): written off by op-1"), "{:?}", r.restores_closed);
    assert_eq!(r.unrestored.len(), 1, "only the second engagement's values: {:?}", r.unrestored);
    assert!(c2.contains("\"type\":\"platform.restore\",\"uid\":\"2\"") && c2.contains("\"confirmed\":true"));
    assert!(!c2.contains("\"carried\":true"), "no carried events under rules 12");
    let r = with(&c2, RulesAccepted::Exactly(REVIEW_FOLLOWUPS_SINCE));
    assert!(whats(&r).iter().any(|w| w.contains("not verified")), "{:?}", whats(&r));
    // the last: the orbit held for the carried values on the same vehicle, its home moved
    // (no other_vehicle), sent only once they were back
    let logs = [(CRASH_2.1, c2.as_str())];
    let opts = ReplayOptions {
        carried_logs: &logs,
        rules: RulesAccepted::Exactly(REVIEW_FOLLOWUPS_SINCE),
        ..ReplayOptions::default()
    };
    let r = replay_str_with(&after, &opts).unwrap();
    assert!(r.ok() && r.unrestored.is_empty(), "{:?} {:?}", r.mismatches, r.unrestored);
    assert_eq!((r.queued, r.recheck_dispatched), (1, 1));
    let ls = lines(&after);
    let queued = at(&ls, 0, &["\"type\":\"intent.queued\"", &format!("since it was carried over from {}", CRASH_2.1), "\"carried\":{"]);
    assert!(!after.contains("\"other_vehicle\""), "the home alone says nothing");
    let held: Event = serde_json::from_str(&ls[queued]).unwrap();
    let EventBody::IntentQueued { inputs, .. } = &held.body else { panic!("queued") };
    let carried = inputs.restore_owed.values().find_map(|o| o.carried.as_deref()).expect("carried");
    let (was, now) = (carried.preloaded_on.as_ref().and_then(|v| v.home).unwrap(), carried.now.as_ref().and_then(|v| v.home).unwrap());
    let moved = murmur_decide::geo::haversine_m(was, now);
    assert!(moved > murmur_decide::VehicleIdentity::HOME_TOLERANCE_M, "its home moved {moved:.0} m");
    let restored = at(&ls, queued, &["\"type\":\"platform.restore\"", "\"confirmed\":true"]);
    let sent = at(&ls, queued, &["\"type\":\"intent.dispatched\"", "\"kind\":\"orbit\""]);
    assert!(restored < sent, "its own values back at {restored}, the orbit sent at {sent}");
    assert!(ls[..restored].iter().any(|l| l.contains("\"type\":\"platform.restore\"") && l.contains("\"error\":")), "attempts unanswered while the downlink was cut");
    // forgery C3 on the live log: the carry emptied, or another log's head, caught against
    // the crashed log (each re-chained, consistent on its own)
    let mut emptied = lines(&after);
    let i = emptied[0].find(",\"owed\":[").unwrap();
    let j = emptied[0][i..].find("}}]").unwrap() + i + 3;
    emptied[0].replace_range(i..j, "");
    let emptied = rechain(&emptied);
    let r = replay_str_with(&emptied, &opts).unwrap();
    let w = whats(&r);
    assert!(w.iter().any(|m| m.contains("carried values differ from what the log they name still owed")), "{w:?}");
    let mut other = lines(&after);
    let (h2, _) = murmur_decide::replay::log_head(&c2).unwrap();
    other[0] = other[0].replacen(&h2, &"1".repeat(64), 1);
    let r = replay_str_with(&rechain(&other), &opts).unwrap();
    let w = whats(&r);
    assert!(w.iter().any(|m| m.contains("the log given is not the one carried over from")), "{w:?}");
    // and the held decision forged to say another vehicle (on the moved home, as rules 12
    // before its review judged it): caught against the identities it logged
    let mut forged: Vec<Event> = ls.iter().map(|l| serde_json::from_str(l).unwrap()).collect();
    if let EventBody::IntentQueued { inputs, .. } = &mut forged[queued].body {
        for o in inputs.restore_owed.values_mut() {
            if let Some(c) = o.carried.as_deref_mut() {
                c.other_vehicle = Some(format!("its home is {moved:.0} m from the one preloaded"));
            }
        }
    }
    let forged: Vec<String> = rechain_events(forged).iter().map(|e| serde_json::to_string(e).unwrap()).collect();
    let r = replay_str_with(&forged.join("\n"), &opts).unwrap();
    let w = whats(&r);
    assert!(w.iter().any(|m| m.contains("lost-link values owed back differ from the log's ledger")), "{w:?}");
}

/// Rules 15, live (murmur-zq3, murmur-imu, after two reviews), from a clean build of `3f5f7bb`,
/// anchored as they ran, stand-ins at the Gate (the engage demo fleet; the fast boat alone as
/// contacts) on their own ports. `L15_CRASH_1`: the switchblade engaged the fast boat holding
/// west of the bridge (a first nomination aborted, a second released), spent by its effect
/// (`restore.closed`, expended); while that engagement was still observing, no SPENT alert and
/// op-2's un-spend refused (a 409, nothing logged); once it was done, the alert; the bridge killed
/// (SIGKILL). The stack restarted with the same fleet (stand-ins respawned under the same
/// identity). `L15_CRASH_2`: the start carried the mark in `run.started` (`spent`, its engagement
/// ended there); an orbit for the switchblade refused (readiness, carried over); op-1's un-spend
/// refused (no engagement authority: a 403, nothing logged); op-2's taken (`platform.unspent`,
/// with the identity it was judged on): its own values owed back again, an orbit queued for the
/// restore and sent the moment it confirmed; the switchblade engaged again, spent, the un-spend
/// refused again while that engagement observed, and the bridge killed then, inside its
/// assessment window. `L15_CLEAN`: carried the new mark, its engagement not ended (`spent_t`, no
/// `ended`); an orbit and a nomination refused, no SPENT alert and op-2's un-spend refused (the
/// window not yet past: a 409, nothing logged); a clean stop. `L15_FALLBACK`: started on a pointer
/// of two different torn lines: it carried from `L15_CLEAN` in its place (the newest of the
/// station by its carry chain, `pointer_failed`), the mark on; an orbit and a nomination refused;
/// the window past, the alert and op-2's un-spend taken; a clean stop.
const L15_CRASH_1: (&str, &str) = ("v15-carry-live-crash-1.jsonl", "20261008T011527Z-2035868.jsonl");
const L15_CRASH_2: (&str, &str) = ("v15-carry-live-crash-2.jsonl", "20261008T012455Z-2054134.jsonl");
const L15_CLEAN: (&str, &str) = ("v15-carry-live-clean.jsonl", "20261008T013234Z-2069100.jsonl");
const L15_FALLBACK: &str = "v15-carry-live-fallback.jsonl";

/// Replay options carrying `logs` (each earlier log's name as the start recorded it, its text),
/// accepting rules 15 (the live logs' own).
fn carrying<'a>(logs: &'a [(&'a str, &'a str)]) -> ReplayOptions<'a> {
    ReplayOptions {
        rules: RulesAccepted::Exactly(CARRY_FOLLOWUPS_SINCE),
        carried_logs: logs,
        ..ReplayOptions::default()
    }
}

#[test]
fn the_rules_15_live_logs_carry_a_spent_mark_an_unspend_and_a_fallback() {
    let (c1, c2, clean, fb) = (fixture(L15_CRASH_1.0), fixture(L15_CRASH_2.0), fixture(L15_CLEAN.0), fixture(L15_FALLBACK));
    // the crashed ones anchored up to their last decision, no shutdown anchor; the others whole
    for (name, text) in [(L15_CRASH_1.0, &c1), (L15_CRASH_2.0, &c2)] {
        let a = anchor::verify(text, &anchors_of(name));
        assert!(!a.shutdown, "{name}");
        assert!(a.problems.iter().all(|p| p.what.starts_with("no shutdown anchor")), "{name}: {:?}", a.problems);
    }
    for (name, text) in [(L15_CLEAN.0, &clean), (L15_FALLBACK, &fb)] {
        let a = anchor::verify(text, &anchors_of(name));
        assert!(a.ok() && a.shutdown, "{name}: {:?}", a.problems);
    }
    // each replays against the one it carried from, every build clean
    let r1 = with(&c1, RulesAccepted::Exactly(CARRY_FOLLOWUPS_SINCE));
    assert!(r1.ok(), "{:?}", r1.mismatches);
    assert_eq!(r1.build.as_ref().map(|b| b.dirty), Some(Some(false)));
    let spent1 = murmur_decide::replay::owed_debts(&c1).spent;
    assert_eq!(spent1.len(), 1, "{spent1:?}");
    assert_eq!(spent1[0].vehicle.as_ref().and_then(|v| v.callsign.as_deref()), Some("BLADE 1"));
    let logs1 = [(L15_CRASH_1.1, c1.as_str())];
    let r2 = replay_str_with(&c2, &carrying(&logs1)).unwrap();
    assert!(r2.ok(), "{:?}", r2.mismatches);
    assert!(r2.carried_restores[0].checked, "{:?}", r2.carried_restores);
    assert_eq!(r2.unspent.len(), 1, "{:?}", r2.unspent);
    assert!(r2.unspent[0].starts_with(&format!("vehicle 1: un-spent by op-2 (spent by engagement i{} of {})", spent1[0].by.0, L15_CRASH_1.1)), "{:?}", r2.unspent);
    let logs2 = [(L15_CRASH_2.1, c2.as_str())];
    let r3 = replay_str_with(&clean, &carrying(&logs2)).unwrap();
    assert!(r3.ok(), "{:?}", r3.mismatches);
    let logs3 = [(L15_CLEAN.1, clean.as_str())];
    let r4 = replay_str_with(&fb, &carrying(&logs3)).unwrap();
    assert!(r4.ok(), "{:?}", r4.mismatches);
    for r in [&r2, &r3, &r4] {
        assert_eq!(r.build.as_ref().map(|b| b.dirty), Some(Some(false)));
    }
    // the second run: the mark in its first line; the orbit refused for it; the un-spend; an
    // orbit queued for the restore, sent after it confirmed; spent again by a second effect
    assert!(!c1.contains("\"type\":\"platform.unspent\""), "the un-spend while its engagement observed was refused");
    let ls = lines(&c2);
    assert!(ls[0].contains("\"spent\":[{\"uid\":\"1\",\"by\":") && ls[0].contains("\"pointer\":\".last-run-8885\""), "{}", ls[0]);
    assert!(ls[0].contains("\"ended\":true"), "its engagement ended in the first run: {}", ls[0]);
    let refused = at(&ls, 0, &["\"type\":\"intent.state\"", "\"to\":\"rejected\"", "carried over): a spent platform takes no order"]);
    let unspent = at(&ls, refused, &["\"type\":\"platform.unspent\"", "\"operator\":\"op-2\""]);
    assert!(ls[unspent].contains("\"identity\":{\"platform\":\"switchblade\",\"callsign\":\"BLADE 1\",\"sysid\":1"), "{}", ls[unspent]);
    assert_eq!(c2.matches("\"type\":\"platform.unspent\"").count(), 1, "the second engagement's refusal logged nothing");
    let queued = at(&ls, unspent, &["\"type\":\"intent.queued\"", "lost-link failsafe"]);
    let restored = at(&ls, queued, &["\"type\":\"platform.restore\"", "\"confirmed\":true"]);
    let sent = at(&ls, queued, &["\"type\":\"intent.dispatched\"", "\"kind\":\"orbit\""]);
    assert!(restored < sent, "restored at {restored}, the orbit sent at {sent}");
    at(&ls, sent, &["\"type\":\"restore.closed\"", "\"kind\":\"expended\""]);
    let spent2 = murmur_decide::replay::owed_debts(&c2).spent;
    assert_eq!(spent2.len(), 1, "spent again");
    assert!(!spent2[0].ended, "killed inside its assessment window");
    let window_end = spent2[0].spent_t.unwrap().plus_ms(120_000);
    // the clean run: the new mark carried, naming the log whose engagement spent it
    let ls = lines(&clean);
    assert!(ls[0].contains("\"spent\":[{\"uid\":\"1\"") && ls[0].contains("\"spent_t\":") && !ls[0].contains("\"ended\""), "{}", ls[0]);
    assert!(!clean.contains("\"type\":\"platform.unspent\""), "refused inside the window: nothing logged");
    assert!(clean.matches("carried over): a spent platform takes no order").count() >= 2, "an orbit and a nomination refused");
    // the fallback: the pointer torn, the clean run's log in its place, the mark carried on from
    // the log that spent it
    let ls = lines(&fb);
    let (head, _) = murmur_decide::replay::log_head(&clean).unwrap();
    assert!(ls[0].contains(&format!("\"carried_from\":[{{\"log\":\"{}\",\"head\":\"{head}\"", L15_CLEAN.1)), "{}", ls[0]);
    assert!(ls[0].contains("\"pointer_failed\":\"the pointer names no log"), "{}", ls[0]);
    assert!(ls[0].contains(&format!("\"from\":\"{}\"", L15_CRASH_2.1)), "{}", ls[0]);
    assert!(r4.carried_restores[0].pointer_failed.is_some(), "{:?}", r4.carried_restores);
    assert!(fb.matches("carried over): a spent platform takes no order").count() >= 2, "an orbit and a nomination refused");
    // the window past: op-2's un-spend taken, judged after the latest that engagement would have
    // ended
    let unspent = lines(&fb).into_iter().find(|l| l.contains("\"type\":\"platform.unspent\"")).expect("un-spent");
    let ev: audit::Event = serde_json::from_str(&unspent).unwrap();
    assert!(ev.t > window_end, "{} after {}", ev.t.0, window_end.0);
    assert_eq!(r4.unspent.len(), 1, "{:?}", r4.unspent);
    // forgeries, re-chained: the mark dropped from the second run's first line; the un-spend
    // credited to op-1 (no engagement authority); the fallback's log passed off as another
    // station's
    let mut dropped: Vec<audit::Event> = lines(&c2).iter().map(|l| serde_json::from_str(l).unwrap()).collect();
    if let EventBody::RunStarted { carried_from, .. } = &mut dropped[0].body {
        carried_from[0].spent.clear();
    }
    let dropped: Vec<String> = rechain_events(dropped).iter().map(|e| serde_json::to_string(e).unwrap()).collect();
    let w = whats(&replay_str_with(&dropped.join("\n"), &carrying(&logs1)).unwrap()).into_iter().map(str::to_string).collect::<Vec<_>>();
    assert!(w.iter().any(|m| m.contains("the platforms carried as spent differ")), "{w:?}");
    let mut by_op1: Vec<audit::Event> = lines(&c2).iter().map(|l| serde_json::from_str(l).unwrap()).collect();
    let op1 = by_op1
        .iter()
        .find_map(|e| match &e.body {
            EventBody::OperatorAction { operator, session, action, .. } if action == "session.opened" && operator.as_str() == "op-1" => Some(*session),
            _ => None,
        })
        .unwrap();
    for e in &mut by_op1 {
        if let EventBody::PlatformUnspent { operator, session, .. } = &mut e.body {
            *operator = OperatorId::new("op-1");
            *session = op1;
        }
    }
    let forged: Vec<String> = rechain_events(by_op1).iter().map(|e| serde_json::to_string(e).unwrap()).collect();
    let r = replay_str_with(&forged.join("\n"), &carrying(&logs1)).unwrap();
    assert!(whats(&r).iter().any(|m| m.contains("a platform un-spent without engagement authority")), "{:?}", whats(&r));
    let other = clean.replacen("\"pointer\":\".last-run-8885\"", "\"pointer\":\".last-run-8765\"", 1);
    let logs_other = [(L15_CLEAN.1, other.as_str())];
    let r = replay_str_with(
        &fb,
        &ReplayOptions {
            rules: RulesAccepted::Exactly(CARRY_FOLLOWUPS_SINCE),
            carried_logs: &logs_other,
            trust_resumed: true,
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(whats(&r).iter().any(|m| m.contains("not this station's")), "{:?}", whats(&r));
}

#[test]
fn a_rules_10_log_claiming_rules_9_still_carries_its_passkeys() {
    let (now, before) = (PASSKEY_SINCE, PASSKEY_SINCE - 1);
    let mut ls = lines(&fixture(PASSKEY_LIVE));
    assert!(ls[0].contains(&format!("\"rules_version\":{now},")));
    ls[0] = ls[0].replacen(&format!("\"rules_version\":{now},"), &format!("\"rules_version\":{before},"), 1);
    let r = with(&rechain(&ls), RulesAccepted::Exactly(before));
    let w = whats(&r);
    for marker in [
        "a passkey login or step-up",
        "a passkey assertion",
        "the run's relying party and passkeys",
        "a passkey registered",
        "a policy's step-up factor",
    ] {
        assert!(
            w.iter().any(|m| m.contains(marker) && m.contains(&format!("only builds of rules {PASSKEY_SINCE}"))),
            "{marker}: {w:?}"
        );
    }
}

/// Rules 14 logs nothing new, so a rules 14 log can claim rules 13 without a marker giving it
/// away; but replay recomputes every decision with the rules claimed, and scenario 83's first
/// nomination, rejected by the aircraft's reach under rules 14, would have waited for its
/// signers under rules 13: the claim is caught on the decision itself.
#[test]
fn a_rules_14_log_claiming_rules_13_is_caught_where_an_aircraft_decided() {
    let (now, before) = (AIRCRAFT_REACH_SINCE, AIRCRAFT_REACH_SINCE - 1);
    let mut ls = lines(&fixture("v14-aircraft-may-turn-back-scenario-83.jsonl"));
    assert!(ls[0].contains(&format!("\"rules_version\":{now},")));
    ls[0] = ls[0].replacen(&format!("\"rules_version\":{now},"), &format!("\"rules_version\":{before},"), 1);
    let r = with(&rechain(&ls), RulesAccepted::Exactly(before));
    assert!(!r.ok(), "rules 13 would have passed the first nomination");
    assert!(r.mismatches.iter().any(|m| m.what.contains("keep_out_tracks") || m.what.contains("rejected")), "{:?}", whats(&r));
    // under its own rules it replays
    assert!(with(&fixture("v14-aircraft-may-turn-back-scenario-83.jsonl"), RulesAccepted::Exactly(now)).ok());
}

/// Rules 15: the live log of the un-spend claiming rules 14 is caught on what only rules 15 logs
/// (the spent marks carried, the un-spend, the station's pointer), and on the decisions the
/// carried mark refused; under its own rules it replays.
#[test]
fn a_rules_15_log_claiming_rules_14_is_caught_on_its_carries() {
    let (now, before) = (CARRY_FOLLOWUPS_SINCE, CARRY_FOLLOWUPS_SINCE - 1);
    let text = fixture(L15_CRASH_2.0);
    let mut ls = lines(&text);
    assert!(ls[0].contains(&format!("\"rules_version\":{now},")));
    ls[0] = ls[0].replacen(&format!("\"rules_version\":{now},"), &format!("\"rules_version\":{before},"), 1);
    let c1 = fixture(L15_CRASH_1.0);
    let logs = [(L15_CRASH_1.1, c1.as_str())];
    let r = replay_str_with(
        &rechain(&ls),
        &ReplayOptions {
            rules: RulesAccepted::Exactly(before),
            carried_logs: &logs,
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    let w = whats(&r);
    for marker in ["a platform un-spent by an operator", "the platforms an earlier log had spent, carried over", "the station's .last-run pointer"] {
        assert!(w.iter().any(|m| m.contains(marker)), "{marker}: {w:?}");
    }
    let r = replay_str_with(
        &text,
        &ReplayOptions {
            rules: RulesAccepted::Exactly(now),
            carried_logs: &logs,
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

/// Rules 11, the engine on the element-set fixture (`tests/space.rs`,
/// `writes_the_space_engine_fixture_when_asked`): an area cover watched to its end, a collect
/// cancelled before its pass, a collect re-checked on a reloaded segment and missed.
const SPACE_ENGINE: &str = "v11-space-followups-engine.jsonl";
/// Rules 11, live: stand-ins at the Gate, the space segment from the real cache.
const SPACE_LIVE: &str = "v11-space-followups-live.jsonl";

#[test]
fn the_rules_11_engine_log_cancels_and_rechecks_space_tasks() {
    let log = fixture(SPACE_ENGINE);
    let r = with(&log, RulesAccepted::Exactly(SPACE_FOLLOWUPS_SINCE));
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.rules_version, SPACE_FOLLOWUPS_SINCE);
    assert_eq!((r.space_cancelled, r.space_rechecked, r.space_missed), (1, 0, 1));
    assert_eq!(r.space_segments, 2, "the segment reloaded once");
    // the cover over the area: one answer per sample point, the least covered deciding
    assert!(log.contains("\"query\":\"cover_links\""));
    assert!(log.contains("at the least covered of 9 sample points over the area ("));
    // the cancel: the operator's, the engine's report, the transition naming the operator
    let ls = lines(&log);
    let cancel = at(&ls, 0, &["\"action\":\"intent.cancel\""]);
    let report = at(&ls, cancel, &["\"type\":\"op.state\"", "\"state\":\"cancelled\"", "tasking request withdrawn"]);
    let aborted = at(&ls, report, &["\"transition\":\"cancel\"", "\"to\":\"aborted\"", "cancelled by op-1: "]);
    assert_eq!((report, aborted), (cancel + 1, cancel + 2));
    // the re-check: the new segment, the dispatched-phase check, the missed report with the
    // fresh options, the failure quoting it
    let loaded = at(&ls, aborted, &["\"type\":\"space.loaded\""]);
    let check = at(&ls, loaded, &["\"type\":\"check.result\"", "\"phase\":\"dispatched\"", "\"passed\":false"]);
    let missed = at(&ls, check, &["\"state\":\"missed\"", "no longer feasible: feasibility: NORAD"]);
    let failed = at(&ls, missed, &["\"transition\":\"fail\"", "collection missed: no longer feasible: "]);
    assert_eq!((missed, failed), (check + 1, check + 2));
}

#[test]
fn the_rules_11_live_log_covers_the_area_and_cancels_a_tasked_collect() {
    let log = fixture(SPACE_LIVE);
    let a = anchor::verify(&log, &anchors_of(SPACE_LIVE));
    assert!(a.ok(), "{:?}", a.problems);
    assert!(a.shutdown, "a clean stop anchored the head");
    let r = with(&log, RulesAccepted::Exactly(SPACE_FOLLOWUPS_SINCE));
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.rules_version, SPACE_FOLLOWUPS_SINCE);
    assert_eq!(r.build.as_ref().map(|b| b.dirty), Some(Some(false)));
    assert!(r.space_queries >= 2);
    assert_eq!(r.space_cancelled, 1);
    let ls = lines(&log);
    // the cover, evaluated over the area's sample points and watched
    let cover = at(&ls, 0, &["\"type\":\"check.result\"", "\"rule\":\"coverage\"", "sample points over the area ("]);
    at(&ls, cover, &["\"type\":\"op.state\"", "\"state\":\"watching\""]);
    // the collect: chosen, dispatched, tasked, cancelled before its pass by the operator
    let tasked = at(&ls, 0, &["\"type\":\"op.state\"", "\"state\":\"tasked\""]);
    let cancel = at(&ls, tasked, &["\"action\":\"intent.cancel\""]);
    let report = at(&ls, cancel, &["\"state\":\"cancelled\"", "tasking request withdrawn"]);
    let aborted = at(&ls, report, &["\"transition\":\"cancel\"", "\"to\":\"aborted\""]);
    assert_eq!((report, aborted), (cancel + 1, cancel + 2));
}

#[test]
fn a_rules_11_log_claiming_rules_10_still_carries_its_follow_ups() {
    let v11 = SPACE_FOLLOWUPS_SINCE;
    for (name, markers) in [
        (SPACE_ENGINE, &["a SATCOM cover evaluated over the area's sample points", "a space task cancelled after dispatch", "a space task's cancelled report", "a space task re-checked after dispatch"][..]),
        (SPACE_LIVE, &["a SATCOM cover evaluated over the area's sample points", "a space task cancelled after dispatch", "a space task's cancelled report"][..]),
    ] {
        let mut ls = lines(&fixture(name));
        assert!(ls[0].contains(&format!("\"rules_version\":{v11},")), "{name}");
        ls[0] = ls[0].replacen(&format!("\"rules_version\":{v11},"), &format!("\"rules_version\":{PASSKEY_SINCE},"), 1);
        let r = with(&rechain(&ls), RulesAccepted::Exactly(PASSKEY_SINCE));
        let w = whats(&r);
        for marker in markers {
            assert!(
                w.iter().any(|m| m.contains(marker) && m.contains(&format!("only builds of rules {v11}"))),
                "{name} {marker}: {w:?}"
            );
        }
    }
}

/// Rules 12 after master's rules 11: a log of the reviews' follow-ups claiming the version
/// before (the space follow-ups, 11) is caught by what only builds of 12 write.
#[test]
fn a_rules_12_log_claiming_rules_11_still_carries_its_follow_ups() {
    let (now, before) = (REVIEW_FOLLOWUPS_SINCE, SPACE_FOLLOWUPS_SINCE);
    assert_eq!(before + 1, now, "the version just before");
    for (name, markers) in [
        (
            "v12-engagement-scenario-01.jsonl",
            &["a restore closed without a restore", "when the tracker started a track"][..],
        ),
        ("v12-young-track-blocks-scenario-79.jsonl", &["when the tracker started a track"][..]),
        ("v12-restore-written-off-scenario-82.jsonl", &["a restore closed without a restore"][..]),
        (CRASH_2.0, &["what a start carried over from earlier runs", "when the tracker started a track"][..]),
        (AFTER, &["what a start carried over from earlier runs", "values carried over holding a vehicle"][..]),
    ] {
        let mut ls = lines(&fixture(name));
        assert!(ls[0].contains(&format!("\"rules_version\":{now},")), "{name}");
        ls[0] = ls[0].replacen(&format!("\"rules_version\":{now},"), &format!("\"rules_version\":{before},"), 1);
        let r = with(&rechain(&ls), RulesAccepted::Exactly(before));
        let w = whats(&r);
        for marker in markers {
            assert!(
                w.iter().any(|m| m.contains(marker) && m.contains(&format!("only builds of rules {now}"))),
                "{name} {marker}: {w:?}"
            );
        }
    }
}

/// Rules 13, the engine on the element-set fixture (`tests/space.rs`,
/// `writes_the_space_engine_fixture_when_asked`, a clean build): the rules 11 sequence, every
/// decision on a space answer saying when it was computed.
const SPACE_REPLAY_ENGINE: &str = "v13-space-replay-engine.jsonl";

#[test]
fn the_rules_13_engine_log_says_when_every_answer_was_computed() {
    let log = fixture(SPACE_REPLAY_ENGINE);
    let r = with(&log, RulesAccepted::Exactly(SPACE_REPLAY_SINCE));
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.rules_version, SPACE_REPLAY_SINCE);
    assert_eq!(r.build.as_ref().map(|b| b.dirty), Some(Some(false)));
    assert_eq!((r.space_cancelled, r.space_rechecked, r.space_missed), (1, 0, 1));
    assert_eq!(r.space_segments, 2, "the segment reloaded once, on the same pin");
    let events = parse_log(&log).unwrap();
    let mut answered = 0;
    for e in &events {
        if let EventBody::CheckResult { inputs, .. } | EventBody::PolicyDecision { inputs, .. } = &e.body
            && let LoggedSpace::Answered { computed_t, .. } = &inputs.space
        {
            answered += 1;
            let c = computed_t.expect("computed_t");
            assert!(inputs.t.ms_since(c).abs() <= PREPARED_SPACE_MAX_AGE_MS, "{e:?}");
        }
    }
    assert!(answered >= 6, "{answered}");
    // the re-check on the reloaded segment, its time logged with it
    let ls = lines(&log);
    let check = at(&ls, 0, &["\"type\":\"check.result\"", "\"phase\":\"dispatched\"", "\"computed_t\":"]);
    at(&ls, check, &["\"state\":\"missed\"", "no longer feasible: feasibility: NORAD"]);
}

/// Rules 13 after rules 12: a log of it claiming 12 is caught by the computation times only
/// builds of 13 write.
#[test]
fn a_rules_13_log_claiming_rules_12_still_carries_its_answers_times() {
    let (now, before) = (SPACE_REPLAY_SINCE, REVIEW_FOLLOWUPS_SINCE);
    assert_eq!(before + 1, now, "the version just before");
    let mut ls = lines(&fixture(SPACE_REPLAY_ENGINE));
    assert!(ls[0].contains(&format!("\"rules_version\":{now},")));
    ls[0] = ls[0].replacen(&format!("\"rules_version\":{now},"), &format!("\"rules_version\":{before},"), 1);
    let r = with(&rechain(&ls), RulesAccepted::Exactly(before));
    let w = whats(&r);
    assert!(
        w.iter().any(|m| m.contains("when a space answer was computed") && m.contains(&format!("only builds of rules {now}"))),
        "{w:?}"
    );
}

/// Rules 16: the committed logs carry the lost contacts as the engine kept them: a drop, an
/// acceptance naming the contact, a claim on its AIS and an operator's confirmation of it, a
/// confirmation void; and claimed as rules 15 (the version before), a log with any of them is
/// caught by its markers.
#[test]
fn the_rules_16_logs_carry_lost_contacts_and_cannot_claim_rules_15() {
    let blocks = fixture("v16-lost-contact-blocks-scenario-86.jsonl");
    assert!(blocks.contains("\"type\":\"track.dropped\",\"track\":\"T-0002\""));
    assert!(blocks.contains("lost contact T-0002 (neutral, fishing_vessel) could be"), "the check names it");
    let r = with(&blocks, RulesAccepted::Exactly(LOST_CONTACT_SINCE));
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.lost_contacts, 1);
    let accepted = fixture("v16-lost-contact-accepted-scenario-87.jsonl");
    assert!(accepted.contains("\"type\":\"engage.accepted\",\"intent\":1,\"contact\":\"T-0002\""));
    assert!(accepted.contains("\"lost_accepted\":{\"T-0002\""));
    let back = fixture("v16-lost-contact-reappears-scenario-88.jsonl");
    assert!(back.contains("\"type\":\"lost.claimed\",\"track\":\"T-0002\",\"by_track\":\"T-0005\""));
    assert!(back.contains("\"type\":\"track.reacquired\",\"track\":\"T-0002\",\"by_track\":\"T-0005\",\"by\":\"op-2\""));
    let spoof = fixture("v16-lost-contact-claimed-by-a-spoof-scenario-93.jsonl");
    assert!(spoof.contains("\"type\":\"lost.reinstated\",\"track\":\"T-0002\",\"by_track\":\"T-0009\""));
    let r = with(&spoof, RulesAccepted::Exactly(LOST_CONTACT_SINCE));
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!((r.lost_claims, r.lost_confirmed, r.lost_reinstated), (1, 1, 1));
    for (name, text) in [("86", &blocks), ("87", &accepted), ("88", &back), ("93", &spoof)] {
        let mut ls = lines(text);
        let now = format!("\"rules_version\":{LOST_CONTACT_SINCE},");
        assert!(ls[0].contains(&now), "{name}");
        ls[0] = ls[0].replacen(&now, &format!("\"rules_version\":{CARRY_FOLLOWUPS_SINCE},"), 1);
        let r = with(&rechain(&ls), RulesAccepted::Exactly(CARRY_FOLLOWUPS_SINCE));
        assert!(
            whats(&r).iter().any(|w| w.contains("a track the tracker dropped") || w.contains("lost contacts in decision inputs")),
            "{name}: {:?}",
            whats(&r)
        );
    }
}

/// Rules 16, live (a clean build of 75c505b, rules 15 merged; stand-ins of the engage demo at the
/// Gate, `sim/scenarios/golden-gate-lost.toml`, a dev run on a fresh station), anchored as it ran:
/// a fishing boat 400 m north of where the fast boat holds and a tour boat on AIS go out of view
/// at 4 minutes. Unseen 100 s and not yet dropped, they block op-1's nomination by their
/// out-of-view reach (the review's item 4: "out of view (no detection for 100 s)"); the tracker
/// drops both 180 s after their last fix (`track.dropped`, the tour boat's with its AIS); op-1's
/// nomination is rejected on a lost contact, then nominated again accepting both
/// (`engage.accepted` naming each), shown to op-2 and op-3 on their picture tokens, signed and
/// dispatched; the tour boat comes back into view under its AIS as a new track, 300 m from its
/// last fix and about 1 km from the effect point, a claim for review (`lost.claimed`, nothing
/// against it); the effect is released (the new track clear of the keep-out, the lost tour boat
/// accepted for this engagement) and the assessment ends it `done`. Then, with the switchblade
/// spent and still heard (rules 15's SPENT, STILL HEARD alert) beside the open claim in the
/// inbox, op-1 confirms the claim (`track.reacquired` under op-1's session), the fishing boat
/// still lost. A clean stop. The start names the station's pointer (rules 15's `pointer`) and the
/// tracker's drop age (`track_drop_s`), which every drop is held to.
#[test]
fn the_rules_16_live_log_holds_lost_contacts_until_accepted_or_confirmed() {
    let name = "v16-lost-contacts-live.jsonl";
    let text = fixture(name);
    let a = anchor::verify(&text, &anchors_of(name));
    assert!(a.ok() && a.shutdown, "{:?}", a.problems);
    let r = with(&text, RulesAccepted::Exactly(LOST_CONTACT_SINCE));
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.build.as_ref().map(|b| b.dirty), Some(Some(false)));
    assert_eq!((r.lost_contacts, r.lost_claims, r.lost_confirmed), (2, 1, 1));
    assert_eq!(text.matches("\"type\":\"track.dropped\"").count(), 2);
    assert!(text.contains("out of view (no detection for"), "a nomination blocked before the drop");
    assert!(text.contains("could be") && text.contains("lost contact T-"), "rejected on a lost contact");
    assert_eq!(text.matches("\"type\":\"engage.accepted\"").count(), 2);
    assert!(text.contains("\"type\":\"lost.claimed\""));
    assert!(text.contains("\"type\":\"track.reacquired\"") && text.contains("\"by\":\"op-1\",\"session\":"));
    assert!(text.contains("\"type\":\"engage.effect\"") && text.contains("assessed ceased"), "released and assessed");
    let claimed = text.find("\"type\":\"lost.claimed\"").unwrap();
    let confirmed = text.find("\"type\":\"track.reacquired\"").unwrap();
    let assessed = text.find("\"type\":\"engage.assessed\"").unwrap();
    assert!(claimed < assessed && assessed < confirmed, "claimed, assessed, then confirmed");
    // a v16 log carries rules 15's too: the station's pointer in its first line; and the drop age
    assert!(lines(&text)[0].contains("\"pointer\":\".last-run-"), "rules 15's pointer");
    assert!(lines(&text)[0].contains("\"track_drop_s\":180"), "the tracker's drop age");
}

/// Rules 17 (murmur-k8i item 2): the committed logs carry the land and water answered along each
/// leg of a boat's route (`along`, one letter a point) and the domain rule's refusals naming the
/// leg and where it reaches land; a patrol round the point is judged leg by leg and passes; and
/// claimed as rules 16 (the version before), a log with the answers is caught by its markers.
#[test]
fn the_rules_17_logs_judge_a_routes_legs_and_cannot_claim_rules_16() {
    let (now, before) = (ROUTE_SURFACE_SINCE, LOST_CONTACT_SINCE);
    assert_eq!(before + 1, now, "the version just before");
    let refused = fixture("v17-transit-crosses-land-scenario-95.jsonl");
    assert!(refused.contains("\"along\":[{\"from_e7\":[-1224500000,378450000]"), "answers from the boat's position");
    assert!(refused.contains("transit leg (position to waypoint 1) crosses land at ("), "the orbit");
    assert!(refused.contains("transit leg (position to effect point) crosses land at ("), "the nomination");
    let round = fixture("v17-route-round-the-point-scenario-96.jsonl");
    assert!(round.contains("all 5 waypoints on water, and the transit, task path, return ("), "the patrol");
    for (name, text) in [("95", &refused), ("96", &round)] {
        let r = with(text, RulesAccepted::Exactly(now));
        assert!(r.ok(), "{name}: {:?}", r.mismatches);
        let mut ls = lines(text);
        let tag = format!("\"rules_version\":{now},");
        assert!(ls[0].contains(&tag), "{name}");
        ls[0] = ls[0].replacen(&tag, &format!("\"rules_version\":{before},"), 1);
        let r = with(&rechain(&ls), RulesAccepted::Exactly(before));
        assert!(
            whats(&r).iter().any(|w| w.contains("land/water answers along a route's legs") && w.contains(&format!("only builds of rules {now}"))),
            "{name}: {:?}",
            whats(&r)
        );
    }
}

/// Rules 18 (murmur-vni): the committed logs carry the tracker's velocity error in the facts each
/// decision read (`velocity_sigma_mps`, logged since rules 14) and the keep-out's detail naming the
/// error a track was predicted with; claimed as rules 17 (the version before), which took every
/// velocity as good to the policy's 1 m/s, replay recomputes the decisions with that and finds
/// them differ: no new marker is needed, the decisions themselves give it away.
#[test]
fn the_rules_18_logs_read_a_tracks_velocity_error_and_cannot_claim_rules_17() {
    let (now, before) = (VELOCITY_SIGMA_SINCE, ROUTE_SURFACE_SINCE);
    assert_eq!(before + 1, now, "the version just before");
    let rejected = fixture("v18-velocity-known-to-3-mps-scenario-97.jsonl");
    assert!(rejected.contains("\"velocity_sigma_mps\":3.0"), "the fact it read");
    assert!(rejected.contains("velocity 300 m at 3.0 m/s"), "the detail names the error");
    let aborted = fixture("v18-velocity-error-grows-under-way-scenario-98.jsonl");
    assert!(aborted.contains("\"velocity_sigma_mps\":4.0") && aborted.contains("at 4.0 m/s"), "the error grown under way");
    // 97's nomination rejected by a keep-out rules 17 passes; 98's abort for a picture rules 17
    // finds holding
    for (name, text, differs) in [("97", &rejected, "rejected by rules that passed"), ("98", &aborted, "stopped for the picture, but the picture held")] {
        let r = with(text, RulesAccepted::Exactly(now));
        assert!(r.ok(), "{name}: {:?}", r.mismatches);
        let mut ls = lines(text);
        let tag = format!("\"rules_version\":{now},");
        assert!(ls[0].contains(&tag), "{name}");
        ls[0] = ls[0].replacen(&tag, &format!("\"rules_version\":{before},"), 1);
        let r = with(&rechain(&ls), RulesAccepted::Exactly(before));
        assert!(whats(&r).iter().any(|w| w.contains(differs)), "{name}: {:?}", whats(&r));
    }
}
