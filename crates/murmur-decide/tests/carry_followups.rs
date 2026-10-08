//! Decision rules 15 (`CARRY_FOLLOWUPS_SINCE`): the follow-ups of rules 12's carries
//! (murmur-zq3, murmur-imu). A platform spent by its own effect stays spent across a restart
//! (`run.started`'s `carried_from` carries each log's spent platforms, `spent`, checked against
//! that log), for the vehicle that was spent; an operator with engagement authority un-spends a
//! platform heard since its effect, saying why (`platform.unspent`), which owes its own lost-link
//! values back again; a nomination on a platform a failed carry may owe values to is refused
//! (`fallback_preload`) until that log is read or written off; a failed carry records the fleet
//! file it read (`fleet_read`), which replay compares when it is at hand; and a void that says it
//! was superseded names an order the log has. The scenario side is 83 (the in-run un-spend) and
//! 84 (the nomination refused); the bridge's side (the pointer's fallback, the API, the alert) is
//! in `murmur-bridge`'s tests.
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;

use common::*;
use murmur_decide::audit::EventBody;
use murmur_decide::replay::{
    CarriedFrom, FleetRead, OwedRestore, ReplayOptions, ReplayReport, RulesAccepted, fleet_may_owe, log_head,
    owed_debts, replay_str, replay_str_with,
};
use murmur_decide::scenario::{self, RunOptions};
use murmur_decide::*;

fn root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The log of an engagement scenario (`sim/scenarios/engage/<file>`), run now.
fn scenario_log(file: &str) -> String {
    let r = scenario::run_file(&root().join("sim/scenarios/engage").join(file), &root(), &RunOptions::default()).unwrap();
    assert!(r.ok, "{file}: {:?}", r.failures);
    r.log
}

/// Scenario 80's switchblade as its write-ahead names it (the vehicle its preload was set on,
/// and spent).
fn blade() -> VehicleIdentity {
    VehicleIdentity {
        platform: Some("switchblade".into()),
        callsign: Some("blade-1".into()),
        sysid: None,
        home: Some(LonLat::new(-122.47, 37.816)),
    }
}

/// The world at `t`: blade-1 (a switchblade) heard at `heard` (every fact then), the bridge's
/// identity of it `who` (none: not known yet).
fn world_heard(t: Time, heard: Time, who: Option<VehicleIdentity>) -> WorldSnapshot {
    let mut w = WorldSnapshot::new(t);
    w.areas = Some(areas());
    let mut v = vehicle("switchblade", NEAR_CRISSY);
    v.nav_ready = Some(Fact::new(true, heard));
    v.autopilot = Some(Fact::new(AutopilotKind::Plane, heard));
    v.position = Some(Fact::new(NEAR_CRISSY, heard));
    v.battery_pct = Some(Fact::new(95.0, heard));
    v.report_interval_ms = Some(1_000);
    w.vehicles.insert(uid("blade-1"), v);
    let mut sky = vehicle("x10", CRISSY);
    sky.nav_ready = Some(Fact::new(true, heard));
    sky.autopilot = Some(Fact::new(AutopilotKind::Copter, heard));
    sky.position = Some(Fact::new(CRISSY, heard));
    sky.battery_pct = Some(Fact::new(95.0, heard));
    w.vehicles.insert(uid("sky-1"), sky);
    if let Some(i) = who {
        w.identities.insert(uid("blade-1"), i);
    }
    w
}

fn world_at(t: Time, who: Option<VehicleIdentity>) -> WorldSnapshot {
    world_heard(t, t, who)
}

fn policy_on(version: u32) -> Policy {
    let mut p = policy();
    p.rules.rules_version = version;
    p
}

fn start(version: u32, carried: Vec<CarriedFrom>, clock: &ManualClock) -> (Engine, MemorySink) {
    let log = MemorySink::new();
    let e = Engine::start(policy_on(version), None, roster(), carried, Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    (e, log)
}

/// What the bridge records in `run.started` for the log `name` it read (`text`), rules 15: its
/// spent platforms too.
fn carry_of(name: &str, text: &str) -> CarriedFrom {
    let (head, seq) = log_head(text).unwrap();
    let d = owed_debts(text);
    CarriedFrom {
        log: name.into(),
        head: Some(head),
        last_seq: Some(seq),
        owed: d.owed,
        pending: d.pending,
        spent: d.spent,
        ..Default::default()
    }
}

fn with<'a>(logs: &'a [(&'a str, &'a str)]) -> ReplayOptions<'a> {
    ReplayOptions {
        carried_logs: logs,
        ..ReplayOptions::default()
    }
}

fn whats(r: &ReplayReport) -> Vec<String> {
    r.mismatches.iter().map(|m| m.what.clone()).collect()
}

fn has(w: &[String], needle: &str) -> bool {
    w.iter().any(|m| m.contains(needle))
}

fn checks(e: &Engine, id: IntentId) -> String {
    e.intent(id).unwrap().history().iter().map(|h| h.reason.clone()).collect::<Vec<_>>().join(" | ")
}

/// An orbit for `uids` near Crissy Field.
fn orbit_near(uids: &[&str]) -> Proposal {
    orbit(NEAR_CRISSY, 150.0, uids)
}

// ---- zq3 (1): the spent marker is carried across a restart -----------------------------------

#[test]
fn a_platform_spent_by_its_effect_stays_spent_across_a_restart() {
    // run A: scenario 80, the switchblade's engagement released and the platform spent
    let a = scenario_log("80-expended-munition-owes-nothing.toml");
    let d = owed_debts(&a);
    assert!(d.owed.is_empty(), "{:?}", d.owed);
    assert_eq!(d.spent.len(), 1, "{:?}", d.spent);
    assert_eq!((d.spent[0].uid.as_str(), d.spent[0].by, d.spent[0].from.as_deref()), ("blade-1", IntentId(1), None));
    assert_eq!(d.spent[0].values.len(), 3, "the values its closure closed");
    assert_eq!(d.spent[0].vehicle.as_ref(), Some(&blade()));
    // run B, after a bridge-only restart: the mark in run.started, the same vehicle under the uid
    let clock = ManualClock::new(T0.plus_ms(600_000));
    let (mut e, log) = start(RULES_VERSION, vec![carry_of("a.jsonl", &a)], &clock);
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let w = world_at(clock.now(), Some(blade()));
    let o = e.submit(op.authority(), orbit_near(&["blade-1"]), &w, &terrain()).unwrap().id();
    assert_eq!(e.intent(o).unwrap().state(), State::Rejected, "{}", checks(&e, o));
    assert!(checks(&e, o).contains("spent by its own effect (engagement i1 of a.jsonl, carried over)"), "{}", checks(&e, o));
    let h = e.submit(op.authority(), hold(&["blade-1"]), &w, &terrain()).unwrap().id();
    assert_eq!(e.intent(h).unwrap().state(), State::Rejected, "a safety command too: {}", checks(&e, h));
    let b = log.text();
    let first = b.lines().next().unwrap();
    assert!(first.contains("\"spent\":[{\"uid\":\"blade-1\",\"by\":1,\"values\":{"), "{first}");
    // checked against a.jsonl
    let logs = [("a.jsonl", a.as_str())];
    let r = replay_str_with(&b, &with(&logs)).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    // without it, the marks carried are not verified (a mismatch unless trusted)
    let w2 = whats(&replay_str(&b).unwrap());
    assert!(has(&w2, "what a.jsonl still owed, carried over, not verified"), "{w2:?}");
    // forged: the mark dropped from run.started (and the refusals with it): caught against
    // a.jsonl
    let dropped = forge(&b, |ev| {
        if let EventBody::RunStarted { carried_from, .. } = &mut ev[0].body {
            carried_from[0].spent.clear();
        }
        ev.retain(|e| !matches!(e.body, EventBody::CheckResult { .. } | EventBody::PolicyDecision { .. } | EventBody::IntentState { .. }));
    });
    let w2 = whats(&replay_str_with(&dropped, &with(&logs)).unwrap());
    assert!(has(&w2, "the platforms carried as spent differ from the ones the log had spent"), "{w2:?}");
    // forged: the decision's fact taken out (its readiness would pass): caught against the ledger
    let unfacted = forge(&b, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::CheckResult { inputs, .. } = &mut e.body {
                inputs.spent_carried.clear();
            }
        }
    });
    let w2 = whats(&replay_str_with(&unfacted, &with(&logs)).unwrap());
    assert!(has(&w2, "spent platforms carried over differ from the log's ledger"), "{w2:?}");
    // a log claiming rules 12 can't carry the marks (markers)
    let claimed = forge(&b, |ev| {
        if let EventBody::RunStarted { rules_version, .. } = &mut ev[0].body {
            *rules_version = Some(REVIEW_FOLLOWUPS_SINCE);
        }
    });
    let r = replay_str_with(
        &claimed,
        &ReplayOptions {
            rules: RulesAccepted::AtLeast(1),
            carried_logs: &logs,
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    let w2 = whats(&r);
    assert!(has(&w2, "the platforms an earlier log had spent, carried over"), "{w2:?}");
    assert!(has(&w2, "a platform spent in an earlier run (a decision's fact)"), "{w2:?}");
    // and B's own debts carry the mark on to the next run, naming the log that spent it
    let d = owed_debts(&b);
    assert_eq!(d.spent.len(), 1);
    assert_eq!(d.spent[0].from.as_deref(), Some("a.jsonl"));
    // an engine of rules 12 neither records nor holds it
    let (mut e12, _) = start(REVIEW_FOLLOWUPS_SINCE, vec![carry_of("a.jsonl", &a)], &clock);
    let op = e12.open_operator_session(OperatorId::new("op-1")).unwrap();
    let o = e12.submit(op.authority(), orbit_near(&["blade-1"]), &w, &terrain()).unwrap().id();
    assert_ne!(e12.intent(o).unwrap().state(), State::Rejected, "rules 12 took it: {}", checks(&e12, o));
}

#[test]
fn a_carried_spent_mark_holds_only_the_vehicle_that_was_spent() {
    let a = scenario_log("80-expended-munition-owes-nothing.toml");
    let clock = ManualClock::new(T0.plus_ms(600_000));
    let logs = [("a.jsonl", a.as_str())];
    // another vehicle under the uid now (its callsign): not held
    let mut other = blade();
    other.callsign = Some("blade-9".into());
    let (mut e, log) = start(RULES_VERSION, vec![carry_of("a.jsonl", &a)], &clock);
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let w = world_at(clock.now(), Some(other));
    let o = e.submit(op.authority(), orbit_near(&["blade-1"]), &w, &terrain()).unwrap().id();
    assert_ne!(e.intent(o).unwrap().state(), State::Rejected, "{}", checks(&e, o));
    let text = log.text();
    assert!(text.contains("\"spent_carried\":{\"blade-1\":{\"from\":\"a.jsonl\",\"by\":1"), "the fact names it");
    assert!(text.contains("\"other_vehicle\":\"its callsign"), "and says how it is another");
    let r = replay_str_with(&text, &with(&logs)).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    // forged: the judgement dropped (as if held) or the identity now changed: caught against the
    // identity logged
    let held = forge(&text, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::CheckResult { inputs, .. } = &mut e.body {
                for s in inputs.spent_carried.values_mut() {
                    s.other_vehicle = None;
                }
            }
        }
    });
    let w2 = whats(&replay_str_with(&held, &with(&logs)).unwrap());
    assert!(has(&w2, "spent platforms carried over differ from the log's ledger"), "{w2:?}");
    // the vehicle not identified yet: held (fail closed)
    let (mut e, _) = start(RULES_VERSION, vec![carry_of("a.jsonl", &a)], &clock);
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let w = world_at(clock.now(), None);
    let o = e.submit(op.authority(), orbit_near(&["blade-1"]), &w, &terrain()).unwrap().id();
    assert_eq!(e.intent(o).unwrap().state(), State::Rejected, "{}", checks(&e, o));
    // the home alone never says another vehicle (ArduPilot sets it again at every arming)
    let mut moved = blade();
    moved.home = Some(LonLat::new(-122.40, 37.80));
    let (mut e, _) = start(RULES_VERSION, vec![carry_of("a.jsonl", &a)], &clock);
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let w = world_at(clock.now(), Some(moved));
    let o = e.submit(op.authority(), orbit_near(&["blade-1"]), &w, &terrain()).unwrap().id();
    assert_eq!(e.intent(o).unwrap().state(), State::Rejected, "{}", checks(&e, o));
}

// ---- imu (2): an operator with engagement authority un-spends a platform -----------------

#[test]
fn an_operator_with_engagement_authority_unspends_a_platform_heard_since_its_effect() {
    let a = scenario_log("80-expended-munition-owes-nothing.toml");
    let values = owed_debts(&a).spent[0].values.clone();
    let clock = ManualClock::new(T0.plus_ms(600_000));
    let started = clock.now();
    let (mut e, log) = start(RULES_VERSION, vec![carry_of("a.jsonl", &a)], &clock);
    let op1 = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let op2 = e.open_operator_session_with(OperatorId::new("op-2"), Grants { engage: true }).unwrap();
    clock.advance_ms(5_000);
    let auth = op2.engagement_authority().unwrap();
    assert!(op1.engagement_authority().is_none(), "an operator without it can't even express it");
    // refused: no note, a platform not spent, one not heard since its engagement ended (a
    // carried mark: its facts from before this run started), one heard since but silent now
    // (beyond the link-aware fact age limit, 10 s here), one whose identity under the uid is
    // another vehicle's or not known yet; nothing logged
    let n = log.lines().len();
    let w = world_at(clock.now(), Some(blade()));
    let refused = |e: &mut Engine, w: &WorldSnapshot, note: &str, who: &str| match e.unspend_platform(auth, uid(who), note, w, None) {
        Err(EngineError::NotUnspent(why)) => why,
        other => panic!("{other:?}"),
    };
    refused(&mut e, &w, "  ", "blade-1");
    refused(&mut e, &w, "fine", "sky-1");
    let before = world_heard(clock.now(), started.plus_ms(-1_000), Some(blade()));
    let why = refused(&mut e, &before, "it survived", "blade-1");
    assert!(why.contains("has not been heard since engagement i1 ended"), "{why}");
    clock.advance_ms(30_000);
    let silent = world_heard(clock.now(), started.plus_ms(2_000), Some(blade()));
    let why = refused(&mut e, &silent, "it survived", "blade-1");
    assert!(why.contains("its link is silent: last heard 33.0 s ago (limit 10 s)"), "{why}");
    let mut nine = blade();
    nine.callsign = Some("blade-9".into());
    let why = refused(&mut e, &world_at(clock.now(), Some(nine)), "it survived", "blade-1");
    assert!(why.contains("not shown to be the one spent: its callsign is \"blade-9\""), "{why}");
    let why = refused(&mut e, &world_at(clock.now(), None), "it survived", "blade-1");
    assert!(why.contains("its platform is not known yet"), "{why}");
    // the home alone never tells a vehicle apart (ArduPilot sets it again at every arming)
    let mut moved = blade();
    moved.home = Some(LonLat::new(-122.40, 37.80));
    assert!(e.unspend_refusal(&uid("blade-1"), world_at(clock.now(), None).vehicles.get(&uid("blade-1")), Some(&moved), clock.now()).is_none());
    assert_eq!(log.lines().len(), n, "nothing logged");
    // heard in this run, now: un-spent, its own values owed back again
    let w = world_at(clock.now(), Some(blade()));
    e.unspend_platform(auth, uid("blade-1"), "still reporting and flying its return: the effect missed", &w, None).unwrap();
    assert_eq!(log.lines().len(), n + 1);
    let owed = e.owed_to(&uid("blade-1")).unwrap();
    assert_eq!(owed.values, values);
    assert_eq!(owed.vehicle.as_ref(), Some(&blade()));
    assert!(e.unspend_platform(auth, uid("blade-1"), "again", &w, None).is_err(), "not spent any more");
    // an orbit now: taken, but it waits for the restore of the values carried over
    let o = e.submit(op1.authority(), orbit_near(&["blade-1"]), &w, &terrain()).unwrap().id();
    let Routed::Queued { reach, .. } = e.dispatch_or_queue(o, &w).unwrap() else {
        panic!("queued: {}", checks(&e, o));
    };
    assert!(reach[0].detail.contains("carried over from a.jsonl"), "{reach:?}");
    // the restore confirms: the orbit goes
    clock.advance_ms(2_000);
    let back = PlatformRestoreReport {
        uid: uid("blade-1"),
        values: values.clone(),
        carried: false,
        from: None,
        readback: values.clone(),
        error: None,
        vehicle: None,
    };
    assert!(e.record_platform_restore(back).unwrap());
    let w = world_at(clock.now(), Some(blade()));
    assert!(matches!(e.dequeue(o, &w, &terrain()).unwrap(), Dequeued::Dispatched(_)), "{}", checks(&e, o));
    let text = log.text();
    let logs = [("a.jsonl", a.as_str())];
    let r = replay_str_with(&text, &with(&logs)).unwrap();
    assert!(r.ok() && r.unrestored.is_empty(), "{:?} {:?}", r.mismatches, r.unrestored);
    assert_eq!(r.unspent.len(), 1);
    assert!(r.unspent[0].starts_with("vehicle blade-1: un-spent by op-2 (spent by engagement i1 of a.jsonl): still reporting"), "{:?}", r.unspent);
    assert!(owed_debts(&text).spent.is_empty(), "not spent in the next run");
    // forgeries, each re-chained: caught
    let unspent = |f: &dyn Fn(&mut EventBody)| {
        forge(&text, |ev| {
            for e in ev.iter_mut().filter(|e| matches!(e.body, EventBody::PlatformUnspent { .. })) {
                f(&mut e.body);
            }
        })
    };
    let op1_session = op1.authority().session();
    for (forged, what) in [
        (
            unspent(&|b| {
                if let EventBody::PlatformUnspent { operator, session, .. } = b {
                    *operator = OperatorId::new("op-1");
                    *session = op1_session;
                }
            }),
            "a platform un-spent without engagement authority",
        ),
        (
            unspent(&|b| {
                if let EventBody::PlatformUnspent { operator, .. } = b {
                    *operator = OperatorId::new("op-3");
                }
            }),
            "a platform un-spent under a session that is not the operator's",
        ),
        (
            unspent(&|b| {
                if let EventBody::PlatformUnspent { note, .. } = b {
                    note.clear();
                }
            }),
            "an un-spend without a note",
        ),
        (
            unspent(&|b| {
                if let EventBody::PlatformUnspent { values, .. } = b {
                    values.clear();
                }
            }),
            "an un-spend that does not name the platform's spent mark",
        ),
        (
            unspent(&|b| {
                if let EventBody::PlatformUnspent { vehicle, .. } = b {
                    for f in [&mut vehicle.autopilot.as_mut().map(|f| &mut f.t), &mut vehicle.position.as_mut().map(|f| &mut f.t), &mut vehicle.nav_ready.as_mut().map(|f| &mut f.t)].into_iter().flatten() {
                        **f = started.plus_ms(-1_000);
                    }
                }
            }),
            "an un-spend the rules refuse on the facts it logs",
        ),
        (
            // heard after the run started, but silent at the un-spend
            unspent(&|b| {
                if let EventBody::PlatformUnspent { vehicle, .. } = b {
                    for f in [&mut vehicle.autopilot.as_mut().map(|f| &mut f.t), &mut vehicle.position.as_mut().map(|f| &mut f.t), &mut vehicle.nav_ready.as_mut().map(|f| &mut f.t)].into_iter().flatten() {
                        **f = started.plus_ms(2_000);
                    }
                }
            }),
            "an un-spend the rules refuse on the facts it logs",
        ),
        (
            // another vehicle under the uid
            unspent(&|b| {
                if let EventBody::PlatformUnspent { identity, .. } = b
                    && let Some(i) = identity.as_mut()
                {
                    i.callsign = Some("blade-9".into());
                }
            }),
            "an un-spend the rules refuse on the facts it logs",
        ),
        (
            // the identity it was judged on left out
            unspent(&|b| {
                if let EventBody::PlatformUnspent { identity, .. } = b {
                    *identity = None;
                }
            }),
            "an un-spend the rules refuse on the facts it logs",
        ),
        (
            unspent(&|b| {
                if let EventBody::PlatformUnspent { uid, .. } = b {
                    *uid = Uid::new("sky-1");
                }
            }),
            "an un-spend of a platform that is not spent",
        ),
    ] {
        let w2 = whats(&replay_str_with(&forged, &with(&logs)).unwrap());
        assert!(has(&w2, what), "{what}: {w2:?}");
    }
    // an engine of rules 12 has no un-spend
    let (mut e12, _) = start(REVIEW_FOLLOWUPS_SINCE, vec![carry_of("a.jsonl", &a)], &clock);
    let op2 = e12.open_operator_session_with(OperatorId::new("op-2"), Grants { engage: true }).unwrap();
    let w = world_at(clock.now(), Some(blade()));
    assert!(matches!(
        e12.unspend_platform(op2.engagement_authority().unwrap(), uid("blade-1"), "it survived", &w, None),
        Err(EngineError::NotUnspent(_))
    ));
}

/// Scenario 84 up to its effect (the switchblade released, the engagement still observing), then
/// `steps` (TOML), run now to 200 s without the expectations the original run checks after that
/// point, the engagement expected to end `expect`.
fn scenario_84_then(steps: &str, expect: &str) -> scenario::ScenarioResult {
    scenario_84_until(steps, expect, 200)
}

/// [`scenario_84_then`], run to `end_s`.
fn scenario_84_until(steps: &str, expect: &str, end_s: u32) -> scenario::ScenarioResult {
    let path = root().join("sim/scenarios/engage/84-spent-munition-unspent.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    let head = &text[..text.find("[[step]]\nat_s = 150").unwrap()];
    let mut sc: scenario::Scenario = toml::from_str(&format!("{head}{steps}")).unwrap();
    sc.expect_unspent.clear();
    sc.expect_refusals.clear();
    sc.expect_preload.clear();
    sc.expect_nothing_owed = false;
    sc.restore_after_s = None;
    sc.tamper = None;
    sc.end_s = Some(end_s);
    sc.expect = expect.into();
    scenario::run(&sc, "84 cut", &root(), &RunOptions::default()).unwrap()
}

/// Scenario 84's un-spend of a platform spent in the same run, and the review's two probes
/// (rules 15): "heard since the effect" was satisfied by the munition's own reports in its last
/// seconds, and an un-spend could come before the assessment. Now the engagement that spent it
/// must have ended, the platform heard after that end and not silent at the un-spend.
#[test]
fn an_unspend_in_the_run_that_spent_it_waits_for_the_engagement_to_end_and_a_live_report() {
    let text = scenario_log("84-spent-munition-unspent.toml");
    let r = replay_str(&text).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.unspent.len(), 1, "{:?}", r.unspent);
    let events = parse_log(&text).unwrap();
    let closed_t = events.iter().find(|e| matches!(e.body, EventBody::RestoreClosed { .. })).map(|e| e.t).unwrap();
    let done_t = events
        .iter()
        .find(|e| matches!(&e.body, EventBody::IntentState { intent, to: State::Done, .. } if *intent == IntentId(1)))
        .map(|e| e.t)
        .unwrap();
    assert!(closed_t < done_t);
    let dated = |t: Time, world: Option<Time>| {
        forge(&text, |ev| {
            for e in ev.iter_mut() {
                if let EventBody::PlatformUnspent { vehicle, world_t, .. } = &mut e.body {
                    for f in [vehicle.autopilot.as_mut().map(|f| &mut f.t), vehicle.position.as_mut().map(|f| &mut f.t), vehicle.nav_ready.as_mut().map(|f| &mut f.t)].into_iter().flatten() {
                        *f = t;
                    }
                    if let Some(w) = world {
                        *world_t = w;
                        e.t = w;
                    }
                }
            }
        })
    };
    // forged: the facts it was judged on dated at the closure (the munition's last reports,
    // before its engagement ended): caught
    let w = whats(&replay_str(&dated(closed_t, None)).unwrap());
    assert!(has(&w, "an un-spend the rules refuse on the facts it logs"), "{w:?}");
    // forged: heard after the end, but long before the un-spend (silent since): caught
    let w = whats(&replay_str(&dated(done_t.plus_ms(1_000), None)).unwrap());
    assert!(has(&w, "an un-spend the rules refuse on the facts it logs"), "{w:?}");
    // forged: the un-spend moved before the engagement ended (its facts then fresh): caught
    let early = forge(&text, |ev| {
        let i = ev.iter().position(|e| matches!(e.body, EventBody::PlatformUnspent { .. })).unwrap();
        let mut u = ev.remove(i);
        let at = ev.iter().position(|e| matches!(e.body, EventBody::RestoreClosed { .. })).unwrap() + 1;
        let t = closed_t.plus_ms(500);
        u.t = t;
        if let EventBody::PlatformUnspent { vehicle, world_t, .. } = &mut u.body {
            *world_t = t;
            for f in [vehicle.autopilot.as_mut().map(|f| &mut f.t), vehicle.position.as_mut().map(|f| &mut f.t), vehicle.nav_ready.as_mut().map(|f| &mut f.t)].into_iter().flatten() {
                *f = t;
            }
        }
        ev.insert(at, u);
    });
    let w = replay_str(&early).unwrap().mismatches;
    assert!(w.iter().any(|m| m.what.contains("an un-spend the rules refuse") && m.logged.contains("has not ended")), "{w:?}");
    // probe B: op-2 asks at 110 s, the engagement still observing (assessed at 122 s): refused,
    // and the munition stays spent
    let b = scenario_84_then(
        "[[step]]\nat_s = 110\naction = \"unspend\"\nby = \"op-2\"\nuid = \"blade-1\"\nnote = \"still reporting after the effect\"\n",
        "done",
    );
    assert!(b.ok, "{:?}", b.failures);
    assert!(b.refusals.iter().any(|r| r.contains("engagement i1 that spent it has not ended")), "{:?}", b.refusals);
    assert!(!b.log.contains("\"type\":\"platform.unspent\""));
    assert_eq!(owed_debts(&b.log).spent.len(), 1, "still spent");
    // probe A: the munition silent from 108 s (2 s after its release). Nothing else observes the
    // effect here, so the engagement observes until its assessment window ends (effect + 120 s,
    // about 226 s) and is then assessed unknown. op-2 asks at 170 s, the engagement still
    // observing: refused; at 240 s, over but the munition not heard since: refused; it reports
    // again at 245 s, and at 250 s the un-spend is taken (heard after the end, not silent)
    let a = scenario_84_until(
        "[[step]]\nat_s = 108\naction = \"vehicle\"\nuid = \"blade-1\"\nsilent = true\n\n[[step]]\nat_s = 170\naction = \"unspend\"\nby = \"op-2\"\nuid = \"blade-1\"\nnote = \"it survived\"\n\n[[step]]\nat_s = 240\naction = \"unspend\"\nby = \"op-2\"\nuid = \"blade-1\"\nnote = \"it survived\"\n\n[[step]]\nat_s = 245\naction = \"vehicle\"\nuid = \"blade-1\"\nsilent = false\n\n[[step]]\nat_s = 250\naction = \"unspend\"\nby = \"op-2\"\nuid = \"blade-1\"\nnote = \"reporting again after its assessment: the effect missed\"\n",
        "done",
        260,
    );
    assert!(a.ok, "{:?}", a.failures);
    assert!(a.refusals.iter().any(|r| r.contains("has not ended")), "{:?}", a.refusals);
    assert!(a.refusals.iter().any(|r| r.contains("has not been heard since engagement i1 ended")), "{:?}", a.refusals);
    assert!(a.log.contains("\"type\":\"engage.assessed\"") && a.log.contains("\"result\":\"unknown\""), "assessed unknown at the window's end");
    assert_eq!(a.log.matches("\"type\":\"platform.unspent\"").count(), 1, "taken once heard after the end");
    assert!(replay_str(&a.log).unwrap().ok());
    // and silent from 125 s, after its engagement ended (assessed at 122 s): heard after the
    // end, but not now: refused at 170 s
    let a = scenario_84_then(
        "[[step]]\nat_s = 125\naction = \"vehicle\"\nuid = \"blade-1\"\nsilent = true\n\n[[step]]\nat_s = 170\naction = \"unspend\"\nby = \"op-2\"\nuid = \"blade-1\"\nnote = \"it survived\"\n",
        "done",
    );
    assert!(a.ok, "{:?}", a.failures);
    assert!(a.refusals.iter().any(|r| r.contains("its link is silent")), "{:?}", a.refusals);
    assert!(!a.log.contains("\"type\":\"platform.unspent\""));
}

/// Rules 15 (second review, item 1): a restart inside the assessment window did not wait for the
/// engagement to be over: a carried mark counted from the new run's start whatever its engagement
/// had reached. Now a mark records when it was spent and whether its engagement ended in its log;
/// one that did not end counts from the latest it would have ended, when it was spent plus
/// `assess_window_s`, if later than the start. The reviewer's probe: scenario 84 cut at 112 s
/// (released at about 106 s, still observing), carried into a new run 3 s later.
#[test]
fn a_carried_mark_whose_engagement_did_not_end_waits_out_its_assessment_window() {
    let cut = scenario_84_until("", "effected", 112);
    assert!(cut.ok, "{:?}", cut.failures);
    let a = cut.log;
    let d = owed_debts(&a);
    assert_eq!(d.spent.len(), 1, "{:?}", d.spent);
    let (spent_t, ended) = (d.spent[0].spent_t.unwrap(), d.spent[0].ended);
    assert!(!ended, "its engagement was still observing");
    let last = parse_log(&a).unwrap().last().map(|e| e.t).unwrap();
    let clock = ManualClock::new(last.plus_ms(3_000));
    let (mut e, log) = start(RULES_VERSION, vec![carry_of("a.jsonl", &a)], &clock);
    assert!(log.text().lines().next().unwrap().contains(&format!("\"spent_t\":{}", spent_t.0)), "the mark says when");
    let op2 = e.open_operator_session_with(OperatorId::new("op-2"), Grants { engage: true }).unwrap();
    let auth = op2.engagement_authority().unwrap();
    // 4 s into the new run, heard now: refused (the probe had it taken)
    clock.advance_ms(4_000);
    let w = world_at(clock.now(), Some(blade()));
    let why = e.unspend_refusal(&uid("blade-1"), w.vehicles.get(&uid("blade-1")), Some(&blade()), w.t).unwrap();
    assert!(why.contains("the latest engagement i1 would have ended"), "{why}");
    assert!(matches!(e.unspend_platform(auth, uid("blade-1"), "it survived", &w, None), Err(EngineError::NotUnspent(_))));
    // heard after the window's end (spent + 120 s): taken
    let window_end = spent_t.plus_ms(120_000);
    clock.set(window_end.plus_ms(2_000));
    let w = world_at(clock.now(), Some(blade()));
    e.unspend_platform(auth, uid("blade-1"), "still reporting after its assessment window: the effect missed", &w, None).unwrap();
    let text = log.text();
    let logs = [("a.jsonl", a.as_str())];
    let r = replay_str_with(&text, &with(&logs)).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    // forged: the un-spend moved to 4 s into the run (its facts then): caught
    let early = forge(&text, |ev| {
        let t = last.plus_ms(7_000);
        for e in ev.iter_mut() {
            if let EventBody::PlatformUnspent { vehicle, world_t, .. } = &mut e.body {
                e.t = t;
                *world_t = t;
                for f in [vehicle.autopilot.as_mut().map(|f| &mut f.t), vehicle.position.as_mut().map(|f| &mut f.t), vehicle.nav_ready.as_mut().map(|f| &mut f.t)].into_iter().flatten() {
                    *f = t;
                }
            }
        }
    });
    let w2 = replay_str_with(&early, &with(&logs)).unwrap().mismatches;
    assert!(w2.iter().any(|m| m.what.contains("an un-spend the rules refuse") && m.logged.contains("would have ended")), "{w2:?}");
    // forged: the mark carried as ended: caught against a.jsonl
    let as_ended = forge(&text, |ev| {
        if let EventBody::RunStarted { carried_from, .. } = &mut ev[0].body {
            carried_from[0].spent[0].ended = true;
        }
    });
    assert!(has(&whats(&replay_str_with(&as_ended, &with(&logs)).unwrap()), "the platforms carried as spent differ"));
    // scenario 84 run whole: its engagement ended in its log, so a later run counts from its start
    let whole = scenario_log("80-expended-munition-owes-nothing.toml");
    assert!(owed_debts(&whole).spent[0].ended);
}

// ---- (second review, item 2): a refused fallback holds what its log owed or had spent -------

/// A start that found scenario 80's log in place of its pointer and would not carry from it (no
/// fleet file beside it) holds the munition that log had spent: an orbit for it waits; replay
/// with the log at hand checks the hold, and a start that held nothing is caught.
#[test]
fn a_refused_fallback_holds_what_its_log_owed_or_had_spent() {
    let a = scenario_log("80-expended-munition-owes-nothing.toml");
    let refused = |may_owe: Vec<OwedRestore>| CarriedFrom {
        log: "a.jsonl".into(),
        error: Some("found in place of the pointer, not carried from: this run keeps no anchors to check it by".into()),
        pointer_failed: Some("the pointer is empty (a write cut short?): the log it named is unknown".into()),
        may_owe,
        ..Default::default()
    };
    let held = vec![OwedRestore {
        uid: uid("blade-1"),
        values: BTreeMap::new(),
        by: Vec::new(),
        vehicle: Some(blade()),
    }];
    let logs = [("a.jsonl", a.as_str())];
    let clock = ManualClock::new(T0.plus_ms(600_000));
    let log = MemorySink::new();
    let mut e = Engine::start(policy_on(RULES_VERSION), None, roster(), StartCarry { pointer: Some(".last-run-0".into()), from: vec![refused(held)], ..Default::default() }, Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let w = world_at(clock.now(), Some(blade()));
    let o = e.submit(op.authority(), orbit_near(&["blade-1"]), &w, &terrain()).unwrap().id();
    assert!(matches!(e.dispatch_or_queue(o, &w).unwrap(), Routed::Queued { .. }), "held: {}", checks(&e, o));
    let r = replay_str_with(&log.text(), &with(&logs)).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    // a start that held nothing (the probe: no fleet file, nothing held): caught with the log
    let log = MemorySink::new();
    Engine::start(policy_on(RULES_VERSION), None, roster(), StartCarry { pointer: Some(".last-run-0".into()), from: vec![refused(Vec::new())], ..Default::default() }, Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let w2 = whats(&replay_str_with(&log.text(), &with(&logs)).unwrap());
    assert!(has(&w2, "was found in place of the pointer and not carried from, yet does not hold every platform"), "{w2:?}");
}

// ---- (2) an un-spend steps up in a credentials run -------------------------------------------

/// A credentials run carrying scenario 80's spent switchblade (`a`): op-1 and op-2 (engagement
/// authority), op-2's passkey `k2`, the policy asking for `factor` at a step-up.
fn credentials_start(
    a: &str,
    k2: &murmur_decide::passkey::soft::SoftAuthenticator,
    factor: murmur_decide::policy::StepUpFactor,
    clock: &ManualClock,
) -> (Engine, MemorySink) {
    let roster = Roster::new(["op-1", "op-2"].map(OperatorId::new).to_vec(), vec![OperatorId::new("op-2")])
        .unwrap()
        .with_auth(AuthMode::Credentials)
        .with_passkeys(PasskeyRoster {
            rp_id: "localhost".into(),
            origins: vec!["http://localhost:5173".into()],
            keys: vec![PasskeyKey {
                operator: OperatorId::new("op-2"),
                id: k2.credential_id(),
                public_key: k2.public_key(),
                counter: 0,
            }],
        });
    let mut p = policy();
    p.approval.step_up_factor = Some(factor);
    let log = MemorySink::new();
    let e = Engine::start(p, None, roster, vec![carry_of("a.jsonl", a)], Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    (e, log)
}

/// What the browser and the bridge do for a passkey step-up: the engine issues the challenge for
/// `purpose`, the authenticator signs it, the engine verifies and logs it.
fn passkey_step(
    e: &mut Engine,
    k: &mut murmur_decide::passkey::soft::SoftAuthenticator,
    s: &OperatorSession,
    purpose: murmur_decide::passkey::StepUpFor,
    nonce: &str,
) -> StepUp {
    let issued = e.passkey_challenge(s.operator(), Some(s), purpose, nonce.into()).unwrap();
    let assertion = k.assert("localhost", "http://localhost:5173", &issued.challenge);
    let proof = murmur_decide::PasskeyUse {
        credential: k.credential_id(),
        counter: k.counter,
        purpose: issued.purpose,
        nonce: issued.nonce,
        issued_t: issued.issued_t,
        assertion,
    };
    e.step_up_passkey(s, proof).unwrap()
}

/// Rules 15 (decided): an un-spend makes an armed platform taskable again, so in a credentials
/// run it re-enters the credential, as accepting a lost friendly does: the password or a passkey
/// per the policy's `step_up_factor`, a passkey's challenge derived from the un-spend (the
/// platform, the engagement, the log of a carried mark). Refused without one, with one made for
/// another act, and with a password where the policy asks for a passkey; replay verifies it.
#[test]
fn an_unspend_in_a_credentials_run_steps_up_for_it() {
    use murmur_decide::passkey::StepUpFor;
    use murmur_decide::passkey::soft::SoftAuthenticator;
    use murmur_decide::policy::StepUpFactor;
    let a = scenario_log("80-expended-munition-owes-nothing.toml");
    let logs = [("a.jsonl", a.as_str())];
    let note = "still reporting after its engagement ended: the effect missed";
    // the password, where the policy takes it
    let clock = ManualClock::new(T0.plus_ms(600_000));
    let mut k2 = SoftAuthenticator::new("op-2 laptop");
    let (mut e, log) = credentials_start(&a, &k2, StepUpFactor::Password, &clock);
    let op2 = e.open_authenticated_session(OperatorId::new("op-2"), Grants { engage: true }, AuthMethod::Password).unwrap();
    clock.advance_ms(5_000);
    let w = world_at(clock.now(), Some(blade()));
    let auth = op2.engagement_authority().unwrap();
    let err = e.unspend_platform(auth, uid("blade-1"), note, &w, None).unwrap_err();
    assert!(matches!(&err, EngineError::NotAuthenticated { why, .. } if why.contains("re-enter your credential")), "{err}");
    // a passkey made for another act: refused
    let other = passkey_step(&mut e, &mut k2, &op2, StepUpFor::DeclareHostile { track: "T-0001".into() }, "n1");
    let err = e.unspend_platform(auth, uid("blade-1"), note, &w, Some(other)).unwrap_err();
    assert!(err.to_string().contains("made for declaring T-0001 hostile"), "{err}");
    let s = e.step_up(&op2).unwrap();
    e.unspend_platform(auth, uid("blade-1"), note, &w, Some(s)).unwrap();
    let text = log.text();
    assert!(text.contains("\"type\":\"platform.unspent\"") && text.contains("\"step_up\":{\"seq\":"), "{text}");
    let r = replay_str_with(&text, &with(&logs)).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.stepped_acts, 1);
    // forged: the step-up taken out: caught
    let bare = forge(&text, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::PlatformUnspent { step_up, .. } = &mut e.body {
                *step_up = None;
            }
        }
    });
    assert!(has(&whats(&replay_str_with(&bare, &with(&logs)).unwrap()), "an un-spend without a step-up"));
    // a passkey, where the policy asks for one: the password refused, a passkey made for this
    // un-spend taken (its challenge names the platform, the engagement and a.jsonl)
    let clock = ManualClock::new(T0.plus_ms(600_000));
    let mut k2 = SoftAuthenticator::new("op-2 laptop");
    let (mut e, log) = credentials_start(&a, &k2, StepUpFactor::Passkey, &clock);
    let op2 = e.open_authenticated_session(OperatorId::new("op-2"), Grants { engage: true }, AuthMethod::Password).unwrap();
    clock.advance_ms(5_000);
    let w = world_at(clock.now(), Some(blade()));
    let auth = op2.engagement_authority().unwrap();
    let s = e.step_up(&op2).unwrap();
    let err = e.unspend_platform(auth, uid("blade-1"), note, &w, Some(s)).unwrap_err();
    assert!(err.to_string().contains("asks for your passkey"), "{err}");
    let purpose = e.unspend_purpose(&uid("blade-1")).unwrap();
    assert_eq!(
        purpose,
        StepUpFor::Unspend {
            uid: uid("blade-1"),
            spent_by: IntentId(1),
            from: Some("a.jsonl".into())
        }
    );
    let s = passkey_step(&mut e, &mut k2, &op2, purpose, "n2");
    e.unspend_platform(auth, uid("blade-1"), note, &w, Some(s)).unwrap();
    let text = log.text();
    let r = replay_str_with(&text, &with(&logs)).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    // forged: the un-spend credited to the password step-up refused before (the policy asks for
    // a passkey): caught
    let pw = forge(&text, |ev| {
        let first = ev
            .iter()
            .find_map(|e| match &e.body {
                EventBody::OperatorAction { action, auth: Some(AuthMethod::Password), .. } if action == "step_up" => Some(e.seq),
                _ => None,
            })
            .unwrap();
        for e in ev.iter_mut() {
            if let EventBody::PlatformUnspent { step_up: Some(r), .. } = &mut e.body {
                r.seq = first;
            }
        }
    });
    assert!(has(&whats(&replay_str_with(&pw, &with(&logs)).unwrap()), "where the policy requires a passkey"));
}

// ---- imu (1): a nomination on a platform a failed carry may owe is refused ----------------

#[test]
fn a_nomination_is_refused_while_a_failed_carry_may_owe_its_platform() {
    let failed = CarriedFrom {
        log: "gone.jsonl".into(),
        may_owe: vec![OwedRestore {
            uid: uid("blade-1"),
            values: BTreeMap::new(),
            by: Vec::new(),
            vehicle: Some(blade()),
        }],
        error: Some("Permission denied (os error 13)".into()),
        fleet_read: Some(FleetRead {
            sha256: "0".repeat(64),
            armed: catalog().platforms.iter().filter(|(_, p)| p.armed).map(|(id, _)| id.clone()).collect(),
        }),
        ..Default::default()
    };
    let clock = ManualClock::new(T0.plus_ms(600_000));
    let (e, _) = start(RULES_VERSION, vec![failed.clone()], &clock);
    let spec = IntentSpec {
        id: IntentId(1),
        created_t: clock.now(),
        source: human(),
        task: Task::Engage {
            track: "T-0001".into(),
            effect_radius_m: 75.0,
            window: None,
            fallback: Fallback::Return,
        },
        uids: vec![uid("blade-1")],
        constraints: Default::default(),
        rationale: None,
    };
    // the engine fills the fact (the nomination form's dry run reads it too) and the
    // fallback_preload check refuses on it
    let preload = |w: &WorldSnapshot| {
        let inputs = e.dry_run_inputs(&spec, w, &terrain());
        let c = rules::evaluate(&spec, &inputs, &e.policy().rules)
            .into_iter()
            .find(|c| c.rule == RuleId::FallbackPreload)
            .unwrap();
        (inputs.restore_owed.clone(), c)
    };
    let (owed, c) = preload(&world_at(clock.now(), Some(blade())));
    assert!(owed.get(&uid("blade-1")).is_some_and(|o| o.holds() && o.values.is_empty()), "{owed:?}");
    assert_eq!(c.outcome, Outcome::Fail, "{c:?}");
    assert!(c.detail.starts_with("gone.jsonl could not be read at the start and may owe this platform its own lost-link values"), "{c:?}");
    // another vehicle under the uid: not refused for it
    let mut nine = blade();
    nine.callsign = Some("blade-9".into());
    let (_, c) = preload(&world_at(clock.now(), Some(nine)));
    assert_eq!(c.outcome, Outcome::Pass, "{c:?}");
    // a mission's refusal is not an engagement's (only its fallback_preload check reads it)
    let (mut e, log) = start(RULES_VERSION, vec![failed], &clock);
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    e.write_off_restore(op.authority(), WriteOff::Log("gone.jsonl".into()), "the disk failed").unwrap();
    let (owed, c) = {
        let inputs = e.dry_run_inputs(&spec, &world_at(clock.now(), Some(blade())), &terrain());
        let c = rules::evaluate(&spec, &inputs, &e.policy().rules).into_iter().find(|c| c.rule == RuleId::FallbackPreload).unwrap();
        (inputs.restore_owed, c)
    };
    assert!(owed.is_empty() && c.outcome == Outcome::Pass, "written off: {owed:?} {c:?}");
    assert!(replay_str(&log.text()).unwrap().ok());
    // rules 12 took the nomination (the gap this closes)
    let mut r12 = policy().rules;
    r12.rules_version = REVIEW_FOLLOWUPS_SINCE;
    let (e12, _) = start(REVIEW_FOLLOWUPS_SINCE, Vec::new(), &clock);
    let mut inputs = e12.dry_run_inputs(&spec, &world_at(clock.now(), Some(blade())), &terrain());
    inputs.restore_owed = owed_hold();
    let c = rules::evaluate(&spec, &inputs, &r12).into_iter().find(|c| c.rule == RuleId::FallbackPreload).unwrap();
    assert_eq!(c.outcome, Outcome::Pass, "{c:?}");
}

/// A may-owe hold on blade-1 from `gone.jsonl`, as a decision's fact.
fn owed_hold() -> BTreeMap<Uid, RestoreOwed> {
    [(
        uid("blade-1"),
        RestoreOwed {
            values: BTreeMap::new(),
            by: Vec::new(),
            carried: Some(Box::new(CarriedHold {
                from: "gone.jsonl".into(),
                preloaded_on: Some(blade()),
                now: Some(blade()),
                other_vehicle: None,
            })),
        },
    )]
    .into_iter()
    .collect()
}

// ---- zq3 (6): a failed carry's fleet file, compared when at hand ------------------------------

#[test]
fn a_failed_carrys_fleet_file_is_compared_when_at_hand() {
    let fleet = serde_json::json!({
        "1": { "platform": "switchblade", "callsign": "BLADE 1", "home": [37.816, -122.47] },
        "2": { "platform": "x10", "callsign": "SKY 2", "home": [37.8068, -122.446] },
        "10": { "platform": "magura", "callsign": "VIPER 10", "home": [37.828, -122.47] },
    })
    .to_string();
    let armed: Vec<String> = catalog().platforms.iter().filter(|(_, p)| p.armed).map(|(id, _)| id.clone()).collect();
    let may_owe = fleet_may_owe(fleet.as_bytes(), &armed).unwrap();
    assert_eq!(may_owe.iter().map(|m| m.uid.as_str()).collect::<Vec<_>>(), ["1", "10"], "the armed ones, in uid order");
    let failed = CarriedFrom {
        log: "gone.jsonl".into(),
        may_owe,
        error: Some("Permission denied (os error 13)".into()),
        fleet_read: Some(FleetRead {
            sha256: murmur_decide::hash::sha256_hex(fleet.as_bytes()),
            armed: armed.clone(),
        }),
        ..Default::default()
    };
    let clock = ManualClock::new(T0.plus_ms(600_000));
    let (mut e, log) = start(RULES_VERSION, vec![failed], &clock);
    // a decision that reads a switchblade (armed) and an x10 (not)
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let w = world_at(clock.now(), None);
    e.submit(op.authority(), orbit_near(&["sky-1"]), &w, &terrain()).unwrap();
    e.submit(op.authority(), orbit_near(&["blade-1"]), &w, &terrain()).unwrap();
    let text = log.text();
    let fleets = [("gone.fleet.json", fleet.as_bytes())];
    let at_hand = |text: &str, fleets: &[(&str, &[u8])]| {
        replay_str_with(
            text,
            &ReplayOptions {
                fleets,
                ..ReplayOptions::default()
            },
        )
        .unwrap()
    };
    let r = at_hand(&text, &fleets);
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.carried_restores[0].fleet_checked, Some(true));
    // not at hand: reported unchecked, no mismatch (the start anchor binds the record)
    let r = replay_str(&text).unwrap();
    assert!(r.ok() && r.carried_restores[0].fleet_checked == Some(false), "{:?}", r.mismatches);
    // another fleet file given: caught
    let edited = fleet.replace("VIPER 10", "VIPER 11");
    let other = [("gone.fleet.json", edited.as_bytes())];
    assert!(has(&whats(&at_hand(&text, &other)), "the fleet file given is not the one the failed carry read"));
    // forged: a vehicle the fleet names left out of what the carry may owe: caught with the file
    let fewer = forge(&text, |ev| {
        if let EventBody::RunStarted { carried_from, .. } = &mut ev[0].body {
            carried_from[0].may_owe.retain(|m| m.uid.as_str() != "10");
        }
    });
    assert!(has(&whats(&at_hand(&fewer, &fleets)), "differ from its fleet file's armed ones"));
    // forged: the record of the file read dropped while it may owe: caught without the file
    let unread = forge(&text, |ev| {
        if let EventBody::RunStarted { carried_from, .. } = &mut ev[0].body {
            carried_from[0].fleet_read = None;
        }
    });
    assert!(has(&whats(&replay_str(&unread).unwrap()), "may owe values without the fleet file it read"));
    // forged: the switchblade left off the armed list (so the vehicle would not be held): the
    // log's own decisions read it armed
    let unarmed = forge(&text, |ev| {
        if let EventBody::RunStarted { carried_from, .. } = &mut ev[0].body
            && let Some(f) = carried_from[0].fleet_read.as_mut()
        {
            f.armed.retain(|p| p != "switchblade");
        }
    });
    assert!(has(&whats(&replay_str(&unarmed).unwrap()), "differ from the catalog's as this log's decisions read it"));
    // forged (review): the record of the file read and what the carry may owe both dropped, the
    // decisions' may-owe facts with them: caught when the file is at hand (the start claimed it
    // could not be read)
    let none = forge(&text, |ev| {
        if let EventBody::RunStarted { carried_from, .. } = &mut ev[0].body {
            carried_from[0].fleet_read = None;
            carried_from[0].may_owe.clear();
        }
        for e in ev.iter_mut() {
            if let EventBody::CheckResult { inputs, .. } = &mut e.body {
                inputs.restore_owed.clear();
            }
        }
    });
    assert!(replay_str(&none).unwrap().ok(), "without the file: bound by the start anchor only");
    assert!(has(&whats(&at_hand(&none, &fleets)), "recorded no fleet file read, though its fleet file is given and reads"));
}

/// Rules 15: a pending log tried again whose fleet file is gone too keeps the may-owe list the log
/// that named it pending recorded (its vehicles stay held); replay compares it with that log.
#[test]
fn a_retried_failed_carry_without_its_fleet_file_keeps_the_list_recorded_before() {
    let may_owe = vec![OwedRestore {
        uid: uid("blade-1"),
        values: BTreeMap::new(),
        by: Vec::new(),
        vehicle: Some(blade()),
    }];
    let failed = CarriedFrom {
        log: "gone.jsonl".into(),
        may_owe: may_owe.clone(),
        error: Some("Permission denied (os error 13)".into()),
        fleet_read: Some(FleetRead {
            sha256: "0".repeat(64),
            armed: catalog().platforms.iter().filter(|(_, p)| p.armed).map(|(id, _)| id.clone()).collect(),
        }),
        ..Default::default()
    };
    let clock = ManualClock::new(T0.plus_ms(600_000));
    let (_, log) = start(RULES_VERSION, vec![failed], &clock);
    let b = log.text();
    assert_eq!(owed_debts(&b).pending_may_owe.get("gone.jsonl"), Some(&may_owe));
    // the next start reads b.jsonl, tries gone.jsonl again (still unreadable, its fleet file
    // gone): the list b recorded, no fleet file
    let retried = CarriedFrom {
        log: "gone.jsonl".into(),
        may_owe: may_owe.clone(),
        error: Some("No such file or directory (os error 2)".into()),
        ..Default::default()
    };
    let (_, log) = start(RULES_VERSION, vec![carry_of("b.jsonl", &b), retried], &clock);
    let c = log.text();
    let logs = [("b.jsonl", b.as_str())];
    let r = replay_str_with(&c, &with(&logs)).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    // forged: the list shortened: caught against b.jsonl
    let fewer = forge(&c, |ev| {
        if let EventBody::RunStarted { carried_from, .. } = &mut ev[0].body {
            carried_from[1].may_owe.clear();
            carried_from[1].may_owe.push(OwedRestore {
                uid: uid("sky-1"),
                values: BTreeMap::new(),
                by: Vec::new(),
                vehicle: None,
            });
        }
    });
    assert!(has(&whats(&replay_str_with(&fewer, &with(&logs)).unwrap()), "differs from what the log that named it pending recorded"));
    // the first entry can't inherit: it needs the fleet file it read
    let first = CarriedFrom {
        log: "gone.jsonl".into(),
        may_owe,
        error: Some("No such file or directory (os error 2)".into()),
        ..Default::default()
    };
    let (_, log) = start(RULES_VERSION, vec![first], &clock);
    assert!(has(&whats(&replay_str(&log.text()).unwrap()), "may owe values without the fleet file it read"));
}

// ---- zq3 (5): a void that says it was superseded names an order the log has ----------------

#[test]
fn a_void_that_says_superseded_names_an_order_the_log_has() {
    let clock = ManualClock::new(T0);
    let (mut e, log) = start(RULES_VERSION, Vec::new(), &clock);
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let w = world_at(clock.now(), None);
    // an older approved orbit and a newer one for the same vehicle, dispatched: the bridge voids
    // the older one, saying so
    let older = e.submit(op.authority(), orbit_near(&["sky-1"]), &w, &terrain()).unwrap().id();
    let newer = e.submit(op.authority(), orbit_near(&["sky-1"]), &w, &terrain()).unwrap().id();
    // a third order, newer still, approved and not dispatched: it stands, but was neither
    // dispatched nor queued
    let third = e.submit(op.authority(), orbit_near(&["sky-1"]), &w, &terrain()).unwrap().id();
    assert!(matches!(e.dispatch_or_queue(newer, &w).unwrap(), Routed::Dispatched(_)));
    e.void(older, &format!("superseded: {newer} was dispatched to vehicle sky-1")).unwrap();
    let text = log.text();
    let r = replay_str(&text).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    let reason = |to: String| {
        forge(&text, |ev| {
            for e in ev.iter_mut() {
                if let EventBody::IntentState { transition: Transition::Void, reason, .. } = &mut e.body {
                    *reason = to.clone();
                }
            }
        })
    };
    let standing = reason(format!("system: superseded: {third} is a newer order for vehicle sky-1 (the newest order stands)"));
    assert!(replay_str(&standing).unwrap().ok(), "a standing order may be named by a dequeue's void");
    for (to, what) in [
        (format!("system: superseded: {newer} was queued for vehicle sky-1 (the newest order stands)"), "not in the state its reason says"),
        (format!("system: superseded: {third} was dispatched to vehicle sky-1"), "not in the state its reason says"),
        ("system: superseded: i99 was dispatched to vehicle sky-1".to_string(), "an order the log does not have"),
        (format!("system: superseded: {older} was dispatched to vehicle sky-1"), "not newer"),
        (format!("system: superseded: {newer} was dispatched to vehicle mule-1"), "not on that vehicle"),
        ("system: superseded: was dispatched to vehicle sky-1".to_string(), "names no order"),
        ("superseded by i99".to_string(), "an order the log does not have"),
    ] {
        let w2 = whats(&replay_str(&reason(to.clone())).unwrap());
        assert!(has(&w2, what), "{to}: {w2:?}");
    }
    // rules 12 did not look (the gap): the same forgery under rules 12 replays
    let forged12 = forge(&reason("system: superseded: i99 was dispatched to vehicle sky-1".into()), |ev| {
        if let EventBody::RunStarted { rules_version, .. } = &mut ev[0].body {
            *rules_version = Some(REVIEW_FOLLOWUPS_SINCE);
        }
    });
    let r = replay_str_with(
        &forged12,
        &ReplayOptions {
            rules: RulesAccepted::Exactly(REVIEW_FOLLOWUPS_SINCE),
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}
