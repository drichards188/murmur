//! Decision rules 13 (`CARRY_FOLLOWUPS_SINCE`): the follow-ups of rules 12's carries
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

/// What the bridge records in `run.started` for the log `name` it read (`text`), rules 13: its
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
    // refused: no note, a platform not spent, one not heard since the effect (its facts from
    // before this run started); nothing logged
    let n = log.lines().len();
    let w = world_at(clock.now(), Some(blade()));
    assert!(matches!(e.unspend_platform(auth, uid("blade-1"), "  ", &w), Err(EngineError::NotUnspent(_))));
    assert!(matches!(e.unspend_platform(auth, uid("sky-1"), "fine", &w), Err(EngineError::NotUnspent(_))));
    let silent = world_heard(clock.now(), started.plus_ms(-1_000), Some(blade()));
    let err = e.unspend_platform(auth, uid("blade-1"), "it survived", &silent).unwrap_err();
    assert!(err.to_string().contains("has not been heard since the effect"), "{err}");
    assert_eq!(log.lines().len(), n, "nothing logged");
    // heard in this run: un-spent, its own values owed back again
    e.unspend_platform(auth, uid("blade-1"), "still reporting and flying its return: the effect missed", &w).unwrap();
    assert_eq!(log.lines().len(), n + 1);
    let owed = e.owed_to(&uid("blade-1")).unwrap();
    assert_eq!(owed.values, values);
    assert_eq!(owed.vehicle.as_ref(), Some(&blade()));
    assert!(e.unspend_platform(auth, uid("blade-1"), "again", &w).is_err(), "not spent any more");
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
            "an un-spend of a platform not heard after the effect that spent it",
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
        e12.unspend_platform(op2.engagement_authority().unwrap(), uid("blade-1"), "it survived", &w),
        Err(EngineError::NotUnspent(_))
    ));
}

/// Scenario 83's un-spend of a platform spent in the same run: heard strictly after the mark.
#[test]
fn an_unspend_in_the_run_that_spent_it_needs_a_report_after_the_effect() {
    let text = scenario_log("83-spent-munition-unspent.toml");
    let r = replay_str(&text).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.unspent.len(), 1, "{:?}", r.unspent);
    let closed_t = parse_log(&text)
        .unwrap()
        .iter()
        .find(|e| matches!(e.body, EventBody::RestoreClosed { .. }))
        .map(|e| e.t)
        .unwrap();
    // forged: the facts it was judged on dated at the closure (not after it): caught
    let at_effect = forge(&text, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::PlatformUnspent { vehicle, .. } = &mut e.body {
                for f in [vehicle.autopilot.as_mut().map(|f| &mut f.t), vehicle.position.as_mut().map(|f| &mut f.t), vehicle.nav_ready.as_mut().map(|f| &mut f.t)].into_iter().flatten() {
                    *f = closed_t;
                }
            }
        }
    });
    assert!(has(&whats(&replay_str(&at_effect).unwrap()), "not heard after the effect"));
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
    for (to, what) in [
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
