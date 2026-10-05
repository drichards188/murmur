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
fn a_redelivery_that_only_follows_the_order_needs_no_new_start() {
    // the link lost after the start went out (unconfirmed): back to the queue; the next
    // delivery only confirms it, and the vehicle running it is followed (acked, no start)
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
    e.record_op_state(id, &sky, "acked", "the vehicle already runs it: followed, not sent again").unwrap();
    let r = replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

// ---- murmur-636: an order queued for an engagement's platform supersedes it ---------------

#[test]
fn an_order_queued_for_an_engaging_platform_supersedes_it_and_rules_6_did_not() {
    let now = run("66-queued-order-supersedes-engagement.toml", None);
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
    let before = run("66-queued-order-supersedes-engagement.toml", Some(6));
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

// ---- murmur-85l: a mission waits for its platform's own lost-link values ----------------

#[test]
fn a_mission_waits_for_its_platforms_own_lost_link_values_and_rules_6_sent_it_at_once() {
    let now = run("67-mission-waits-for-restore.toml", None);
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
    let before = run("67-mission-waits-for-restore.toml", Some(6));
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
    let r = run("68-restore-never-confirmed.toml", None);
    assert!(r.ok, "{:?}", r.failures);
    assert!(r.log.contains("back to the inbox for a new decision (its vehicles could not be reached: viper-1: heard, but its own lost-link failsafe"), "the reason says why");
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
