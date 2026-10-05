//! Decision rules 7 (`QUEUE_FOLLOWUPS_SINCE`, provisional number): the queue's follow-ups to
//! rules 5 and 6. A vehicle start is logged ahead of sending it (murmur-dma), a mission waits
//! for its platform's own lost-link values to be confirmed back after an engagement preloaded
//! them (murmur-85l), and an order queued for an engagement's platform supersedes the
//! engagement as a dispatched one does (murmur-636). Older rules decide as they did, and a log
//! claiming them can't carry any of it.
#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use common::*;
use murmur_decide::audit::EventBody;
use murmur_decide::replay::{ReplayOptions, RulesAccepted, replay_str, replay_str_with};
use murmur_decide::scenario::{self, RunOptions, ScenarioResult};
use murmur_decide::*;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(name: &str, rules_version: Option<u32>) -> ScenarioResult {
    let f = root().join("sim/scenarios/engage").join(name);
    scenario::run_file(
        &f,
        &root(),
        &RunOptions {
            rules_version,
            ..RunOptions::default()
        },
    )
    .unwrap()
}

fn whats(text: &str, rules: RulesAccepted) -> Vec<String> {
    let r = replay_str_with(
        text,
        &ReplayOptions {
            rules,
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    r.mismatches.into_iter().map(|m| m.what).collect()
}

fn engine_on(version: u32) -> (Engine, MemorySink, ManualClock, OperatorSession) {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut p = policy();
    p.rules.rules_version = version;
    let mut e = Engine::new(p, roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let s = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    (e, log, clock, s)
}

/// The world at `t`, every fact fresh then.
fn world_at(t: Time) -> WorldSnapshot {
    let mut w = world();
    w.t = t;
    for v in w.vehicles.values_mut() {
        if let Some(f) = &mut v.autopilot {
            f.t = t;
        }
        if let Some(f) = &mut v.nav_ready {
            f.t = t;
        }
        if let Some(f) = &mut v.position {
            f.t = t;
        }
        if let Some(f) = &mut v.battery_pct {
            f.t = t;
        }
        v.report_interval_ms = Some(1_000);
    }
    w
}

// ---- murmur-dma: a vehicle start is logged ahead of sending it ---------------------------

#[test]
fn a_start_is_logged_ahead_and_the_vehicle_takes_an_order_only_after_it() {
    let (mut e, log, clock, s) = engine_on(RULES_VERSION);
    let w = world_at(clock.now());
    let id = e.submit(s.authority(), orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w, &terrain()).unwrap().id();
    e.dispatch(id, &w).unwrap();
    let sky = uid("sky-1");
    let refused = |r: Result<(), EngineError>| matches!(r, Err(EngineError::NotItsOrder { .. }));
    // a start reported after the fact, and the vehicle taking an order whose start was never
    // logged: neither is what a rules 7 bridge writes
    assert!(refused(e.record_op_state(id, &sky, "starting", "mission mode commanded")));
    assert!(refused(e.record_op_state(id, &sky, "acked", "accepted")));
    e.record_op_state(id, &sky, "sent", "uploading").unwrap();
    e.record_op_state_ahead(id, &sky, "starting mission: mission mode commanded").unwrap();
    e.record_op_state(id, &sky, "acked", "accepted").unwrap();
    e.record_op_state(id, &sky, "active", "on station").unwrap();
    let text = log.text();
    assert!(text.contains("\"state\":\"starting\",\"detail\":\"starting mission: mission mode commanded\",\"ahead\":true"), "{text}");
    let r = replay_str(&text).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    // forged: the start dropped (a carry-over after a crash would then restart the order),
    // or written as if reported afterwards
    let dropped = forge(&text, |ev| ev.retain(|e| !matches!(&e.body, EventBody::OpState { ahead: true, .. })));
    let w = whats(&dropped, RulesAccepted::default());
    assert!(w.iter().any(|m| m.contains("a vehicle took an order whose start was not logged ahead of it")), "{w:?}");
    let after = forge(&text, |ev| {
        for e in ev {
            if let EventBody::OpState { ahead, .. } = &mut e.body {
                *ahead = false;
            }
        }
    });
    let w = whats(&after, RulesAccepted::default());
    assert!(w.iter().any(|m| m.contains("a vehicle start not logged ahead of sending it")), "{w:?}");
    // and a log claiming rules 6 can't carry a start logged ahead
    let claimed = forge(&text, |ev| {
        if let EventBody::RunStarted { rules_version, .. } = &mut ev[0].body {
            *rules_version = Some(6);
        }
    });
    let w = whats(&claimed, RulesAccepted::Exactly(6));
    assert!(
        w.iter().any(|m| m.contains("a vehicle start logged ahead of sending it") && m.contains(&format!("only builds of rules {QUEUE_FOLLOWUPS_SINCE}"))),
        "{w:?}"
    );
}

#[test]
fn rules_6_logged_starts_when_they_were_seen() {
    let (mut e, log, clock, s) = engine_on(6);
    let w = world_at(clock.now());
    let id = e.submit(s.authority(), orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w, &terrain()).unwrap().id();
    e.dispatch(id, &w).unwrap();
    let sky = uid("sky-1");
    assert!(e.record_op_state_ahead(id, &sky, "mission mode commanded").is_err(), "no write-ahead under rules 6");
    e.record_op_state(id, &sky, "starting", "mission mode commanded").unwrap();
    e.record_op_state(id, &sky, "active", "on station").unwrap();
    let r = replay_str_with(
        &log.text(),
        &ReplayOptions {
            rules: RulesAccepted::Exactly(6),
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

#[test]
fn a_redelivery_logs_its_start_or_its_follow_ahead_too() {
    // the link lost after the start went out (unconfirmed): back to the queue; the next
    // delivery only confirms it, and the vehicle running it is followed: the follow is logged
    // ahead like a start (from then on the vehicle runs it), and only then is it acked
    let (mut e, log, clock, s) = engine_on(RULES_VERSION);
    let w = world_at(clock.now());
    let id = e.submit(s.authority(), orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w, &terrain()).unwrap().id();
    e.dispatch_or_queue(id, &w).unwrap();
    let sky = uid("sky-1");
    e.record_op_state(id, &sky, "sent", "uploading").unwrap();
    e.record_op_state_ahead(id, &sky, "starting mission: mission mode commanded").unwrap();
    e.record_op_state(id, &sky, "unconfirmed", "link lost after its start went out").unwrap();
    e.requeue(id).unwrap();
    clock.advance_ms(20_000);
    let w = world_at(clock.now());
    assert!(matches!(e.dequeue(id, &w, &terrain()).unwrap(), Dequeued::Dispatched(_)));
    let refused = |r: Result<(), EngineError>| matches!(r, Err(EngineError::NotItsOrder { .. }));
    assert!(refused(e.record_op_state(id, &sky, "acked", "the vehicle already runs it: followed, not sent again")));
    e.record_op_state_ahead(id, &sky, "the vehicle already runs it: followed, not sent again").unwrap();
    e.record_op_state(id, &sky, "acked", "the vehicle already runs it: followed, not sent again").unwrap();
    let text = log.text();
    let r = replay_str(&text).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    // forged: the redelivery's follow dropped (review of 1a3cadb, finding 4: only a first
    // delivery was held to it)
    let follow_seq = parse_log(&text)
        .unwrap()
        .iter()
        .filter(|e| matches!(&e.body, EventBody::OpState { ahead: true, .. }))
        .map(|e| e.seq)
        .max()
        .unwrap();
    let dropped = forge(&text, |ev| ev.retain(|e| e.seq != follow_seq));
    let w = whats(&dropped, RulesAccepted::default());
    assert!(w.iter().any(|m| m.contains("start was not logged ahead of it on this delivery")), "{w:?}");
}

#[test]
fn a_requeued_order_redelivered_logs_its_start_ahead() {
    // review of 1a3cadb, finding 4: requeued before its start went out, then dequeued: the
    // redelivery's start goes through the write-ahead like the first's
    let (mut e, log, clock, s) = engine_on(RULES_VERSION);
    let w = world_at(clock.now());
    let id = e.submit(s.authority(), orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w, &terrain()).unwrap().id();
    e.dispatch_or_queue(id, &w).unwrap();
    let sky = uid("sky-1");
    e.record_op_state(id, &sky, "sent", "uploading").unwrap();
    e.record_op_state(id, &sky, "requeued", "link lost mid-upload").unwrap();
    e.requeue(id).unwrap();
    clock.advance_ms(20_000);
    let w = world_at(clock.now());
    assert!(matches!(e.dequeue(id, &w, &terrain()).unwrap(), Dequeued::Dispatched(_)));
    let refused = |r: Result<(), EngineError>| matches!(r, Err(EngineError::NotItsOrder { .. }));
    assert!(refused(e.record_op_state(id, &sky, "acked", "accepted")));
    assert!(refused(e.record_op_state(id, &sky, "active", "executing")));
    e.record_op_state_ahead(id, &sky, "starting mission: mission mode commanded").unwrap();
    e.record_op_state(id, &sky, "acked", "accepted").unwrap();
    let r = replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

// ---- murmur-636: an order queued for an engagement's platform supersedes it ---------------

#[test]
fn an_order_queued_for_an_engaging_platform_supersedes_it_and_rules_6_did_not() {
    let now = run("67-queued-order-supersedes-engagement.toml", None);
    assert!(now.ok, "{:?}", now.failures);
    let stop = now
        .log
        .lines()
        .find(|l| l.contains("\"type\":\"engage.stopped\""))
        .unwrap()
        .to_string();
    assert!(stop.contains("\"kind\":\"superseded\"") && stop.contains("\"fallback\":null"), "{stop}");
    assert!(stop.contains("queued until its vehicles can be reached"), "{stop}");
    // under rules 6 the queued hold left the engagement running until its picture failed,
    // and its fallback was commanded over the operator's hold
    let before = run("67-queued-order-supersedes-engagement.toml", Some(6));
    assert!(!before.ok);
    assert!(
        before.failures.iter().any(|f| f.contains("expected the engagement stopped as superseded, it was picture")),
        "{:?}",
        before.failures
    );
    assert_eq!(before.fallback.as_deref(), Some("loiter"));
    // a log claiming rules 6 can't carry an engagement superseded by a queued order
    let claimed = forge(&now.log, |ev| {
        if let EventBody::RunStarted { rules_version, .. } = &mut ev[0].body {
            *rules_version = Some(6);
        }
    });
    let w = whats(&claimed, RulesAccepted::Exactly(6));
    assert!(w.iter().any(|m| m == "superseded by an order that was not dispatched"), "{w:?}");
    // nor can a rules 7 log have it superseded by an order before that order was queued
    let forged = forge(&now.log, |ev| {
        let stop = ev
            .iter()
            .position(|e| matches!(&e.body, EventBody::EngageStopped { .. }))
            .unwrap();
        let queued = ev
            .iter()
            .position(|e| matches!(&e.body, EventBody::IntentQueued { .. }))
            .unwrap();
        assert!(queued < stop);
        let e = ev.remove(stop);
        ev.insert(queued, e);
    });
    let w = whats(&forged, RulesAccepted::default());
    assert!(w.iter().any(|m| m == "superseded by an order that was not dispatched"), "{w:?}");
}

/// `text` with the first `engage.stopped` (and the transition that follows it) moved `later`
/// events further on, and re-chained: the stop now comes after other events.
fn stop_moved_later(text: &str, later: usize) -> String {
    forge(text, |ev| {
        let stop = ev
            .iter()
            .position(|e| matches!(&e.body, EventBody::EngageStopped { .. }))
            .unwrap();
        let pair: Vec<_> = ev.drain(stop..stop + 2).collect();
        let at = (stop + later).min(ev.len());
        for (k, e) in pair.into_iter().enumerate() {
            ev.insert(at + k, e);
        }
    })
}

#[test]
fn a_supersede_is_only_in_the_step_that_queued_or_dispatched_the_order() {
    // review of 1a3cadb, finding 3: the stop moved seconds after the queueing passed replay,
    // so a later picture or system stop could have been relabelled superseded
    let queued = run("67-queued-order-supersedes-engagement.toml", None);
    assert!(queued.ok, "{:?}", queued.failures);
    let w = whats(&stop_moved_later(&queued.log, 3), RulesAccepted::default());
    assert!(w.iter().any(|m| m == "superseded by a queued order outside the step that queued it"), "{w:?}");
    // the same for an order dispatched to the platform (scenario 22: an operator's hold)
    let dispatched = run("22-agent-order-on-engaging-platform.toml", None);
    assert!(dispatched.ok, "{:?}", dispatched.failures);
    let w = whats(&stop_moved_later(&dispatched.log, 3), RulesAccepted::default());
    assert!(w.iter().any(|m| m == "superseded by an order outside the step that dispatched it"), "{w:?}");
    // the genuine logs: each stop right after its order's transition
    for log in [&queued.log, &dispatched.log] {
        let r = replay_str(log).unwrap();
        assert!(r.ok(), "{:?}", r.mismatches);
    }
}

#[test]
fn with_no_queue_for_its_kind_a_mission_is_refused_not_sent_over_the_lost_link_action() {
    // review of 1a3cadb, finding 1: [queue.kind.orbit] expire_s = 0 (a policy that never
    // queues an orbit) let the orbit go at once at t=35 with the restore confirmed at t=60
    let now = run("70-mission-refused-without-queue.toml", None);
    assert!(now.ok, "{:?}", now.failures);
    assert_eq!(now.tamper_detected, Some(true), "{:?}", now.tamper_mismatches);
    let voided = now
        .log
        .lines()
        .find(|l| l.contains("\"to\":\"voided\"") && l.contains("own lost-link failsafe"))
        .unwrap_or_else(|| panic!("the orbit refused, saying why:\n{}", now.log));
    // the engagement had ended (aborted at t=30): the restore is pending, nothing stopped
    assert!(voided.contains("the policy queues no orbit order"), "{voided}");
    assert!(voided.contains("is not confirmed back since engagement i1 preloaded it"), "{voided}");
    assert!(voided.contains("a hold or rtl goes at once"), "{voided}");
    assert!(!voided.contains("this order stops engagement"), "{voided}");
    // rules 6 sent it at once; that log, claimed as rules 7, is a mission dispatched to a
    // vehicle still owed its values
    let before = run("70-mission-refused-without-queue.toml", Some(6));
    assert!(before.failures.iter().any(|f| f.contains("t=36: expected the task voided, got dispatched")), "{:?}", before.failures);
    let claimed = forge(&before.log, |ev| {
        if let EventBody::RunStarted { rules_version, .. } = &mut ev[0].body {
            *rules_version = Some(RULES_VERSION);
        }
    });
    let w = whats(&claimed, RulesAccepted::default());
    assert!(w.iter().any(|m| m == "a mission dispatched to a vehicle still owed its own lost-link values (rules 7)"), "{w:?}");
}

#[test]
fn a_mission_refused_for_its_platforms_own_values_still_supersedes_the_engagement() {
    // re-check of c577d5d, item A: under a policy that never queues an orbit, the orbit for a
    // platform whose engagement was under way was voided and the engagement went on (the
    // probe: still dispatched at t=200), while a dispatched (rules 6) or queued (rules 7)
    // orbit stopped it. The newest order stands either way: the refusal stops it, superseded,
    // in the same step
    let now = run("71-refused-mission-supersedes-engagement.toml", None);
    assert!(now.ok, "{:?}", now.failures);
    assert_eq!(now.tamper_detected, Some(true), "{:?}", now.tamper_mismatches);
    let lines: Vec<&str> = now.log.lines().collect();
    let refused: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains("\"to\":\"voided\"") && l.contains("system: not sent: "))
        .map(|(k, _)| k)
        .collect();
    assert_eq!(refused.len(), 2, "refused at t=20 and again at t=30:\n{}", now.log);
    // the first, the engagement under way: it stops it, and says so; the stop is next
    let first = lines[refused[0]];
    assert!(first.contains("carries engagement i1's lost-link action"), "{first}");
    assert!(first.contains("this order stops engagement i1 (superseded, no fallback)"), "{first}");
    assert!(first.contains("the restore of its own values starts now (as soon as the platform is heard)"), "{first}");
    assert!(first.contains("a hold or rtl goes at once"), "{first}");
    let stop = lines[refused[0] + 1];
    assert!(stop.contains("\"type\":\"engage.stopped\"") && stop.contains("\"kind\":\"superseded\""), "{stop}");
    assert!(stop.contains("\"fallback\":null"), "{stop}");
    // the second, the engagement already stopped: the restore is pending
    let second = lines[refused[1]];
    assert!(second.contains("is not confirmed back since engagement i1 preloaded it"), "{second}");
    assert!(second.contains("a hold or rtl goes at once"), "{second}");
    assert!(!second.contains("this order stops engagement"), "{second}");
    // under rules 6 the same orbit went out at once (and superseded the engagement)
    let before = run("71-refused-mission-supersedes-engagement.toml", Some(6));
    assert!(before.failures.iter().any(|f| f.contains("t=21: expected the task voided, got dispatched")), "{:?}", before.failures);
    // a stop by a refused order only in the step that refused it
    let w = whats(&stop_moved_later(&now.log, 3), RulesAccepted::default());
    assert!(w.iter().any(|m| m == "superseded by a voided order outside the step that refused it"), "{w:?}");
    // and only by a refusal: a void that is not one opens no step
    let forged = forge(&now.log, |ev| {
        let void = ev
            .iter()
            .position(|e| matches!(&e.body, EventBody::IntentState { reason, .. } if reason.starts_with("system: not sent: ")))
            .unwrap();
        if let EventBody::IntentState { reason, .. } = &mut ev[void].body {
            *reason = "system: withdrawn".into();
        }
    });
    let w = whats(&forged, RulesAccepted::default());
    assert!(w.iter().any(|m| m == "superseded by a voided order outside the step that refused it"), "{w:?}");
    // a log claiming rules 6 can't carry an engagement superseded by a refused order
    let claimed = forge(&now.log, |ev| {
        if let EventBody::RunStarted { rules_version, .. } = &mut ev[0].body {
            *rules_version = Some(6);
        }
    });
    let w = whats(&claimed, RulesAccepted::Exactly(6));
    assert!(w.iter().any(|m| m == "superseded by an order that was not dispatched"), "{w:?}");
}

// ---- murmur-85l: a mission waits for its platform's own lost-link values ----------------

#[test]
fn a_mission_waits_for_its_platforms_own_lost_link_values_and_rules_6_sent_it_at_once() {
    let now = run("68-mission-waits-for-restore.toml", None);
    assert!(now.ok, "{:?}", now.failures);
    assert_eq!(now.tamper_detected, Some(true), "{:?}", now.tamper_mismatches);
    assert!(
        now.tamper_mismatches.iter().any(|m| m.contains("lost-link values owed back differ from the log's ledger")),
        "{:?}",
        now.tamper_mismatches
    );
    // the orbit was queued while the values were owed, and went the second they were back
    let lines: Vec<&str> = now.log.lines().collect();
    let queued = lines.iter().position(|l| l.contains("\"type\":\"intent.queued\"")).unwrap();
    assert!(lines[queued].contains("its own lost-link failsafe") && lines[queued].contains("\"restore_owed\":{\"viper-1\""), "{}", lines[queued]);
    let restored = lines
        .iter()
        .position(|l| l.contains("\"phase\":\"restore\"") && l.contains("\"confirmed\":true"))
        .unwrap();
    let orbit = lines
        .iter()
        .rposition(|l| l.contains("\"type\":\"intent.dispatched\""))
        .unwrap();
    assert!(queued < restored && restored < orbit, "queued {queued}, restored {restored}, sent {orbit}");
    // under rules 6 the same orbit went out at once
    let before = run("68-mission-waits-for-restore.toml", Some(6));
    assert!(!before.ok);
    assert!(before.failures.iter().any(|f| f.contains("t=36: expected the task queued, got dispatched")), "{:?}", before.failures);
    // a log claiming rules 6 can't carry the fact
    let claimed = forge(&now.log, |ev| {
        if let EventBody::RunStarted { rules_version, .. } = &mut ev[0].body {
            *rules_version = Some(6);
        }
    });
    let w = whats(&claimed, RulesAccepted::Exactly(6));
    assert!(w.iter().any(|m| m.contains("a vehicle's lost-link values owed back")), "{w:?}");
}

#[test]
fn a_restore_that_never_confirms_sends_the_mission_back_to_a_human_and_a_hold_still_goes() {
    let r = run("69-restore-never-confirmed.toml", None);
    assert!(r.ok, "{:?}", r.failures);
    assert!(r.log.contains("back to the inbox for a new decision (queued because its vehicles could not be reached: viper-1: heard, but its own lost-link failsafe"), "the reason says why");
}

#[test]
fn a_hold_or_rtl_is_never_held_for_a_restore() {
    // the reach of each kind for a vehicle still owed its values: only a mission waits
    let mut w = world_at(T0);
    let owed = queue::RestoreOwed {
        values: [("FS_ACTION".to_string(), 1.0), ("FS_GCS_TIMEOUT".to_string(), 30.0)].into_iter().collect(),
        by: vec![IntentId(1)],
    };
    w.restore_owed.insert(uid("sky-1"), owed);
    let cfg = QueueConfig::default();
    let reach = |p: Proposal, version: u32| {
        let rules = RulesConfig {
            rules_version: version,
            ..RulesConfig::default()
        };
        queue::reach(&spec(human(), p), &w, &cfg, &rules)
    };
    for p in [hold(&["sky-1"]), rtl(&["sky-1"])] {
        assert!(queue::all_reachable(&reach(p, RULES_VERSION)));
    }
    let r = reach(orbit(NEAR_CRISSY, 150.0, &["sky-1"]), RULES_VERSION);
    assert!(!queue::all_reachable(&r));
    assert!(r[0].detail.contains("FS_ACTION 1, FS_GCS_TIMEOUT 30") && r[0].detail.contains("engagement i1"), "{}", r[0].detail);
    // rules 6 never held one
    assert!(queue::all_reachable(&reach(orbit(NEAR_CRISSY, 150.0, &["sky-1"]), 6)));
}

// ---- murmur-a9h: the airship's refusal says what its firmware does -----------------------

#[test]
fn an_airships_fallback_refusal_says_its_failsafe_only_disarms() {
    let mut w = world_at(T0);
    let mut lta = vehicle("lta", NEAR_CRISSY);
    lta.autopilot = Some(Fact::new(AutopilotKind::Blimp, T0));
    w.vehicles.insert(uid("lta-1"), lta);
    let p = Proposal::new(
        Task::Engage {
            track: "T-0001".into(),
            effect_radius_m: 75.0,
            window: None,
            fallback: Fallback::Return,
        },
        vec![uid("lta-1")],
    );
    let s = spec(human(), p);
    let detail = |version: u32| {
        let mut r = policy().rules;
        r.rules_version = version;
        rules::evaluate(&s, &w, &r)
            .into_iter()
            .find(|c| c.rule == RuleId::FallbackPreload)
            .map(|c| (c.outcome, c.detail))
            .unwrap()
    };
    let (outcome, now) = detail(RULES_VERSION);
    assert_eq!(outcome, Outcome::Fail);
    assert!(now.contains("only lost-link action is to disarm"), "{now}");
    // rules 6 logs keep the text they were decided with
    let (_, before) = detail(6);
    assert!(before.contains("only lost-link action is Land"), "{before}");
}

#[test]
fn the_airship_scenario_under_rules_7_words_its_refusal_as_the_firmware_does() {
    // the rules 7 counterpart of the committed rules 6 log of scenario 66 (versions.rs)
    let r = run("66-airship-cannot-preload.toml", None);
    assert!(r.ok, "{:?}", r.failures);
    assert!(r.log.contains("only lost-link action is to disarm"), "the rules 7 wording");
    assert!(!r.log.contains("only lost-link action is Land"));
    // claimed as rules 6, its refusal is not what rules 6 decided
    let claimed = forge(&r.log, |ev| {
        if let EventBody::RunStarted { rules_version, .. } = &mut ev[0].body {
            *rules_version = Some(6);
        }
    });
    let w = whats(&claimed, RulesAccepted::Exactly(6));
    assert!(w.iter().any(|m| m == "check results differ"), "{w:?}");
}
