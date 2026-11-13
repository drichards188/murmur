//! Rules 29 (`HANDOVER_SINCE`): a fleet file's starting tasks, handed over at the start. Each is
//! carried in queued (`intent.handed_over`) with the previous watch's approval as its basis,
//! re-checked on the world when its vehicle can be reached, dispatched when its checks pass,
//! voided when one fails, and sent to a person when one could not be verified (the policy asked
//! only who); replay recomputes it all and checks each against the fleet the run logged.
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use murmur_decide::audit::EventBody;
use murmur_decide::replay::{ReplayOptions, ReplayReport, RulesAccepted, replay_str_with};
use murmur_decide::*;

fn from() -> HandedOverFrom {
    HandedOverFrom {
        fleet: "sim/fleets/watch.toml".into(),
        sha256: "ab".repeat(32),
    }
}

/// Every fact fresh at `t`, reporting every second (reachable).
fn world_at(t: Time) -> WorldSnapshot {
    let mut w = world();
    w.t = t;
    for v in w.vehicles.values_mut() {
        if let Some(f) = &mut v.autopilot {
            f.t = t;
        }
        if let Some(f) = &mut v.position {
            f.t = t;
        }
        if let Some(f) = &mut v.nav_ready {
            f.t = t;
        }
        if let Some(f) = &mut v.battery_pct {
            f.t = t;
        }
        v.report_interval_ms = Some(1_000);
    }
    w
}

/// sky-1's orbit near Crissy Field (it passes), boat-1's orbit on land (the domain rule fails).
fn starts() -> (Proposal, Proposal) {
    (orbit(NEAR_CRISSY, 150.0, &["sky-1"]), orbit(CRISSY, 150.0, &["boat-1"]))
}

/// The fleet file a run with those starts writes beside its log (`<log stem>.fleet.json`).
fn fleet_file(sky: &Task, boat: &Task) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "sky-1": { "platform": "x10", "callsign": "SKY 1", "home": [37.8068, -122.446], "start": sky },
        "boat-1": { "platform": "seahunter", "callsign": "BOAT 1", "home": [37.828, -122.47], "start": boat },
        "mule-1": { "platform": "ugv_logistics", "callsign": "MULE 1", "home": [37.8068, -122.446], "start": orbit(NEAR_CRISSY, 150.0, &["mule-1"]).task },
        "hawk-1": { "platform": "rq4", "callsign": "HAWK 1", "home": [37.8068, -122.446] },
    }))
    .unwrap()
}

fn replay_with(text: &str, fleet: Option<&[u8]>) -> ReplayReport {
    replay_str_with(
        text,
        &ReplayOptions {
            own_fleet: fleet,
            ..ReplayOptions::default()
        },
    )
    .unwrap()
}

#[test]
fn handed_over_orders_go_when_their_checks_pass_and_are_voided_when_one_fails() {
    let clock = ManualClock::new(T0);
    let (mut e, log) = engine(&clock);
    let (sky, boat) = starts();
    let (sky_task, boat_task) = (sky.task.clone(), boat.task.clone());
    let a = e.hand_over(sky, from()).unwrap();
    let b = e.hand_over(boat, from()).unwrap();
    // mule-1's platform has no endurance figure: its reserves can't be verified
    let c = e.hand_over(orbit(NEAR_CRISSY, 150.0, &["mule-1"]), from()).unwrap();
    for id in [a, b, c] {
        let i = e.intent(id).unwrap();
        assert_eq!(i.state(), State::Queued);
        assert_eq!(i.handed_over(), Some(&from()));
        assert!(i.approval().is_none() && i.policy().is_none());
        assert_eq!(i.queue().unwrap().expires_t, T0.plus_ms(900_000));
        assert!(i.history().last().unwrap().reason.contains("the previous watch's order"));
    }
    // before its vehicles are heard: nothing happens, nothing logged
    let lines = log.lines().len();
    let mut silent = world_at(T0);
    silent.t = T0.plus_ms(10_000);
    clock.set(silent.t);
    assert!(matches!(e.dequeue(a, &silent, &terrain()).unwrap(), Dequeued::Waiting(_)));
    assert_eq!(log.lines().len(), lines);
    // heard, but not its first full report (no home yet): it waits, nothing logged, rather than
    // fail its checks on a fact still to come
    let mut early = world_at(clock.now());
    early.vehicles.get_mut(&uid("sky-1")).unwrap().home = None;
    let Dequeued::Waiting(reach) = e.dequeue(a, &early, &terrain()).unwrap() else { panic!("not waiting") };
    assert!(reach[0].detail.contains("waiting for its home position"), "{}", reach[0].detail);
    assert_eq!(log.lines().len(), lines);
    // heard in full: re-checked on the world now; the orbit goes at once, the policy asked nothing
    let w = world_at(clock.now());
    let Dequeued::Dispatched(Order::Vehicles(o)) = e.dequeue(a, &w, &terrain()).unwrap() else {
        panic!("not dispatched: {:?}", e.intent(a).unwrap().history())
    };
    assert_eq!(o.uids, vec![uid("sky-1")]);
    assert_eq!(e.intent(a).unwrap().state(), State::Dispatched);
    // a boat's orbit on land fails its domain rule: voided, never sent, the failing check said
    assert!(matches!(e.dequeue(b, &w, &terrain()).unwrap(), Dequeued::Voided));
    let i = e.intent(b).unwrap();
    assert_eq!(i.state(), State::Voided);
    let rc = i.queue().unwrap().recheck.as_ref().unwrap();
    assert_eq!(rc.verdict, Verdict::Void);
    assert!(rc.changes.iter().any(|c| c.contains("fails")), "{:?}", rc.changes);
    assert!(i.history().last().unwrap().reason.starts_with("re-check failed at dequeue"));
    // a check nobody could verify: to a person, as the policy routes any order with one
    let out = e.dequeue(c, &w, &terrain()).unwrap();
    assert!(matches!(out, Dequeued::Inbox), "{out:?} {:?}", e.intent(c).unwrap().history());
    let i = e.intent(c).unwrap();
    assert_eq!(i.state(), State::PendingApproval);
    assert!(i.queue().unwrap().recheck.as_ref().unwrap().changes.iter().any(|c| c.contains("a person judges it")));
    let text = log.text();
    assert!(!text.contains("\"type\":\"policy.decision\""), "the policy decides nothing for them");
    assert!(!text.contains("\"type\":\"intent.created\""));
    for l in text.lines().filter(|l| l.contains("\"type\":\"queue.recheck\"")) {
        // the policy is asked only who judges the unverified one
        assert_eq!(l.contains("\"decision\""), l.contains("\"intent\":3,"), "{l}");
    }

    // replay recomputes it all, and checks each against the fleet the run logged
    let r = replay_with(&text, None);
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!((r.handed_over, r.recheck_dispatched, r.recheck_voided, r.recheck_inbox), (3, 1, 1, 1));
    assert_eq!(r.handover_fleet, Some(false), "not checked without the fleet file");
    let fleet = fleet_file(&sky_task, &boat_task);
    let r = replay_with(&text, Some(&fleet));
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.handover_fleet, Some(true));
    // another fleet: a starting task that differs, and one never handed over
    let other = fleet_file(&orbit(BAY, 150.0, &["sky-1"]).task, &boat_task);
    let r = replay_with(&text, Some(&other));
    assert!(r.mismatches.iter().any(|m| m.what.contains("differs from its vehicle's starting task")), "{:?}", r.mismatches);
    let mut more: serde_json::Value = serde_json::from_slice(&fleet).unwrap();
    more["hawk-1"]["start"] = serde_json::to_value(&hold(&["hawk-1"]).task).unwrap();
    let r = replay_with(&text, Some(&serde_json::to_vec(&more).unwrap()));
    assert!(r.mismatches.iter().any(|m| m.what.contains("never handed over")), "{:?}", r.mismatches);
}

#[test]
fn a_handover_is_only_a_routine_starting_task_at_the_start() {
    let clock = ManualClock::new(T0);
    let (mut e, _log) = engine(&clock);
    // never anything but an orbit, a patrol or a hold, for one vehicle
    let err = e.hand_over(rtl(&["sky-1"]), from()).unwrap_err().to_string();
    assert!(err.contains("an orbit, a patrol or a hold"), "{err}");
    let err = e.hand_over(hold(&["sky-1", "boat-1"]), from()).unwrap_err().to_string();
    assert!(err.contains("one vehicle's"), "{err}");
    e.hand_over(hold(&["sky-1"]), from()).unwrap();
    let err = e.hand_over(hold(&["sky-1"]), from()).unwrap_err().to_string();
    assert!(err.contains("has a starting task already"), "{err}");
    // once any other order exists, nothing more is handed over
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    e.submit(op.authority(), hold(&["boat-1"]), &world_at(T0), &terrain()).unwrap();
    let err = e.hand_over(hold(&["remus-1"]), from()).unwrap_err().to_string();
    assert!(err.contains("only at the start"), "{err}");
    // and under rules before 29, none at all
    let mut p = policy();
    p.rules.rules_version = MISSILE_STANDOFF_SINCE;
    let mut old = Engine::new(p, roster(), Box::new(clock.clone()), Box::new(MemorySink::new())).unwrap();
    let err = old.hand_over(hold(&["sky-1"]), from()).unwrap_err().to_string();
    assert!(err.contains("from rules 29"), "{err}");
}

/// A log of rules 29 with a handed-over order, relabelled rules 28, carries what no build of
/// rules 28 wrote.
#[test]
fn a_handover_in_a_log_claiming_rules_28_is_a_mismatch() {
    let clock = ManualClock::new(T0);
    let (mut e, log) = engine(&clock);
    e.hand_over(hold(&["sky-1"]), from()).unwrap();
    let forged = forge(&log.text(), |events| {
        if let EventBody::RunStarted { rules_version, .. } = &mut events[0].body {
            *rules_version = Some(MISSILE_STANDOFF_SINCE);
        }
    });
    let r = replay_str_with(
        &forged,
        &ReplayOptions {
            rules: RulesAccepted::AtLeast(MISSILE_STANDOFF_SINCE),
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(!r.ok());
    assert!(r.mismatches.iter().any(|m| m.what.contains("handed over")), "{:?}", r.mismatches);
}
