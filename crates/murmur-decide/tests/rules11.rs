//! Decision rules 11 (`REVIEW_FOLLOWUPS_SINCE`): the follow-ups of the reviews of rules 6 to
//! 10. Here, what a start carries over from earlier runs' logs (murmur-cq3: recorded in
//! `run.started` in one line, checked against the logs it names, a failed carry kept owed
//! until read or written off), values carried over holding a mission for the vehicle they were
//! preloaded on (murmur-qjf), and an operator's write-off (murmur-q05). The scripted operators
//! fixed at the start (murmur-jfi) are in `tests/auth.rs`; the expended munition, the young
//! track and the refusal naming the orders still queued are scenarios 79 to 82.
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;

use common::*;
use murmur_decide::audit::EventBody;
use murmur_decide::replay::{
    CarriedFrom, OwedRestore, ReplayOptions, ReplayReport, RulesAccepted, log_head, owed_debts,
    replay_str, replay_str_with,
};
use murmur_decide::*;

/// sky-1's own lost-link values (ArduCopter: land on a lost link after 5 s).
fn own() -> BTreeMap<String, f64> {
    [("FS_GCS_ENABLE", 1.0), ("FS_GCS_TIMEOUT", 5.0), ("FS_OPTIONS", 0.0)]
        .into_iter()
        .map(|(n, v)| (n.to_string(), v))
        .collect()
}

/// The vehicle they were preloaded on: sky-1 as the common world has it.
fn sky() -> VehicleIdentity {
    VehicleIdentity {
        platform: Some("x10".into()),
        callsign: Some("SKY 1".into()),
        sysid: Some(2),
        home: Some(CRISSY),
    }
}

/// The world at `t`, every fact fresh then, each vehicle reporting every second; `who` is the
/// bridge's identity of sky-1 now (none: not known).
fn world_at(t: Time, who: Option<VehicleIdentity>) -> WorldSnapshot {
    let mut w = world();
    w.t = t;
    for v in w.vehicles.values_mut() {
        for f in [&mut v.nav_ready] {
            if let Some(f) = f {
                f.t = t;
            }
        }
        if let Some(f) = &mut v.autopilot {
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
    if let Some(i) = who {
        w.identities.insert(uid("sky-1"), i);
    }
    w
}

fn policy_on(version: u32) -> Policy {
    let mut p = policy();
    p.rules.rules_version = version;
    p
}

/// An engine of rules `version` that carried `carried` over at its start.
fn start(version: u32, carried: Vec<CarriedFrom>, clock: &ManualClock) -> (Engine, MemorySink) {
    let log = MemorySink::new();
    let e = Engine::start(policy_on(version), None, roster(), carried, Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    (e, log)
}

/// What the bridge records in `run.started` for the log `name` it read (`text`).
fn carry_of(name: &str, text: &str) -> CarriedFrom {
    let (head, seq) = log_head(text).unwrap();
    let d = owed_debts(text);
    CarriedFrom {
        log: name.into(),
        head: Some(head),
        last_seq: Some(seq),
        owed: d.owed,
        pending: d.pending,
        error: None,
    }
}

/// Run A, rules 11: it carried sky-1's own values over from an earlier `a0.jsonl` (not at
/// hand: trusted) and boat-1's too, and the vehicles never confirmed them back: it still owes
/// them both.
fn owing_run() -> String {
    let clock = ManualClock::new(T0);
    let a0 = CarriedFrom {
        log: "a0.jsonl".into(),
        head: Some("0".repeat(64)),
        last_seq: Some(40),
        owed: vec![
            OwedRestore {
                uid: uid("boat-1"),
                values: [("FS_ACTION".to_string(), 2.0)].into_iter().collect(),
                by: vec![IntentId(3)],
                vehicle: None,
            },
            OwedRestore {
                uid: uid("sky-1"),
                values: own(),
                by: vec![IntentId(5)],
                vehicle: Some(sky()),
            },
        ],
        pending: Vec::new(),
        error: None,
    };
    let (mut e, log) = start(RULES_VERSION, vec![a0], &clock);
    clock.advance_ms(5_000);
    let attempt = PlatformRestoreReport {
        uid: uid("sky-1"),
        values: own(),
        carried: false,
        from: None,
        readback: BTreeMap::new(),
        error: Some("no answer".into()),
        vehicle: None,
    };
    assert!(!e.record_platform_restore(attempt).unwrap());
    log.text()
}

fn with<'a>(logs: &'a [(&'a str, &'a str)], trusted: bool) -> ReplayOptions<'a> {
    ReplayOptions {
        carried_logs: logs,
        trust_resumed: trusted,
        ..ReplayOptions::default()
    }
}

fn whats(r: &ReplayReport) -> Vec<String> {
    r.mismatches.iter().map(|m| m.what.clone()).collect()
}

fn has(w: &[String], needle: &str) -> bool {
    w.iter().any(|m| m.contains(needle))
}

// ---- murmur-cq3: what a start carried, in one line, checked against the logs it names ----

#[test]
fn a_start_records_what_it_carried_in_one_line_and_replay_checks_it_against_that_log() {
    let a = owing_run();
    let alone = replay_str_with(&a, &with(&[], true)).unwrap();
    assert!(alone.ok(), "{:?}", alone.mismatches);
    assert_eq!(alone.unrestored.len(), 2, "{:?}", alone.unrestored);
    // B reads a.jsonl at start: what it owed, its head and last seq, in run.started
    let clock = ManualClock::new(T0.plus_ms(600_000));
    let (mut e, log) = start(RULES_VERSION, vec![carry_of("a.jsonl", &a)], &clock);
    assert_eq!(e.owed_restores().len(), 2, "owed from the first line on");
    clock.advance_ms(2_000);
    for (u, values) in [("sky-1", own()), ("boat-1", [("FS_ACTION".to_string(), 2.0)].into_iter().collect())] {
        let back = PlatformRestoreReport {
            uid: uid(u),
            values: values.clone(),
            carried: false,
            from: None,
            readback: values,
            error: None,
            vehicle: None,
        };
        assert!(e.record_platform_restore(back).unwrap());
    }
    let b = log.text();
    let first = b.lines().next().unwrap();
    let (head, seq) = log_head(&a).unwrap();
    assert!(first.contains(&format!("\"carried_from\":[{{\"log\":\"a.jsonl\",\"head\":\"{head}\",\"last_seq\":{seq},\"owed\":[")), "{first}");
    assert!(!b.contains("\"type\":\"platform.restore\",\"uid\":\"sky-1\",\"values\":{\"FS_GCS_ENABLE\":1.0,\"FS_GCS_TIMEOUT\":5.0,\"FS_OPTIONS\":0.0},\"carried\":true"), "no carried events under rules 11");
    // checked against a.jsonl: it replays, it is the log read, and it owed exactly that
    let logs = [("a.jsonl", a.as_str())];
    let r = replay_str_with(&b, &with(&logs, false)).unwrap();
    assert!(r.ok() && r.unrestored.is_empty(), "{:?} {:?}", r.mismatches, r.unrestored);
    assert_eq!(r.carried_restores.len(), 1);
    assert!(r.carried_restores[0].checked && r.carried_restores[0].restores == 2, "{:?}", r.carried_restores);
    // without it: not verified, a mismatch unless trusted
    let r = replay_str(&b).unwrap();
    assert!(has(&whats(&r), "what a.jsonl still owed, carried over, not verified"), "{:?}", whats(&r));
    assert!(replay_str_with(&b, &with(&[], true)).unwrap().ok());
    // forgery C3: the carried restores dropped from the successor (the carry emptied), each
    // forgery re-chained and consistent on its own; caught against a.jsonl
    let edit = |f: &dyn Fn(&mut CarriedFrom)| {
        forge(&b, |ev| {
            if let EventBody::RunStarted { carried_from, .. } = &mut ev[0].body {
                f(&mut carried_from[0]);
            }
        })
    };
    let dropped = forge(&b, |ev| {
        if let EventBody::RunStarted { carried_from, .. } = &mut ev[0].body {
            carried_from[0].owed.clear();
        }
        // and its restores, which would put back values no longer owed
        ev.retain(|e| !matches!(e.body, EventBody::PlatformRestore { .. }));
    });
    assert!(replay_str_with(&dropped, &with(&[], true)).unwrap().ok(), "consistent on its own");
    let w = whats(&replay_str_with(&dropped, &with(&logs, false)).unwrap());
    assert!(has(&w, "carried values differ from what the log they name still owed"), "{w:?}");
    // a partial carry (one platform of two, as a crash between two carried events left it
    // before): the same
    let partial = edit(&|c| {
        c.owed.retain(|o| o.uid.as_str() == "sky-1");
    });
    let w = whats(&replay_str_with(&partial, &with(&logs, false)).unwrap());
    assert!(has(&w, "carried values differ from what the log they name still owed"), "{w:?}");
    // another log passed off as the one read (its head): caught
    let other = edit(&|c| c.head = Some("1".repeat(64)));
    let w = whats(&replay_str_with(&other, &with(&logs, false)).unwrap());
    assert!(has(&w, "the log given is not the one carried over from"), "{w:?}");
    // the record removed from the first line altogether reads as a first start: only the
    // start anchor (the first line's hash) shows it, as for the roster
    let gone = forge(&b, |ev| {
        if let EventBody::RunStarted { carried_from, .. } = &mut ev[0].body {
            carried_from.clear();
        }
    });
    assert!(has(&whats(&replay_str(&gone).unwrap()), "restore of values not owed back"), "its restores now put back values never owed");
    // a log claiming rules 10 can't carry the record (a marker)
    let claimed = forge(&b, |ev| {
        if let EventBody::RunStarted { rules_version, .. } = &mut ev[0].body {
            *rules_version = Some(PASSKEY_SINCE);
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
    assert!(has(&whats(&r), "what a start carried over from earlier runs"), "{:?}", whats(&r));
}

/// Forgery C3 before the fix: under rules 10 a successor with every carried
/// `platform.restore` removed named no earlier log, and replay had nothing to check it
/// against (exit 0, nothing said).
#[test]
fn rules_10_could_not_tell_a_carry_was_due() {
    let clock = ManualClock::new(T0);
    let (mut e, log) = start(PASSKEY_SINCE, Vec::new(), &clock);
    let carried = PlatformRestoreReport {
        uid: uid("sky-1"),
        values: own(),
        carried: true,
        from: Some("a.jsonl (carried over into it)".into()),
        readback: BTreeMap::new(),
        error: None,
        vehicle: Some(sky()),
    };
    e.record_platform_restore(carried).unwrap();
    let b = log.text();
    let dropped = forge(&b, |ev| ev.retain(|e| !matches!(e.body, EventBody::PlatformRestore { .. })));
    let r = replay_str_with(
        &dropped,
        &ReplayOptions {
            rules: RulesAccepted::AtLeast(PASSKEY_SINCE),
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(r.ok() && r.carried_restores.is_empty() && r.unrestored.is_empty(), "{:?}", r.mismatches);
    // under rules 11 a carried event is refused: the carry is in run.started
    let (mut e, _) = start(RULES_VERSION, Vec::new(), &clock);
    let carried = PlatformRestoreReport {
        uid: uid("sky-1"),
        values: own(),
        carried: true,
        from: Some("a.jsonl (carried over into it)".into()),
        readback: BTreeMap::new(),
        error: None,
        vehicle: Some(sky()),
    };
    assert!(e.record_platform_restore(carried).is_err());
    // and an engine of rules 10 takes no carry in run.started
    let log = MemorySink::new();
    let refused = Engine::start(
        policy_on(PASSKEY_SINCE),
        None,
        roster(),
        vec![carry_of("a.jsonl", &owing_run())],
        Box::new(clock.clone()),
        Box::new(log.clone()),
    );
    assert!(refused.is_err(), "rules 10 took a carry in run.started");
    assert!(log.lines().is_empty(), "{:?}", log.lines());
}

#[test]
fn a_failed_carry_stays_owed_until_a_later_start_reads_it_or_an_operator_writes_it_off() {
    let a = owing_run();
    // B: .last-run named a log it could not read: recorded, and owed (what, unknown)
    let clock = ManualClock::new(T0.plus_ms(600_000));
    let failed = CarriedFrom {
        log: "a.jsonl".into(),
        head: None,
        last_seq: None,
        owed: Vec::new(),
        pending: Vec::new(),
        error: Some("Permission denied (os error 13)".into()),
    };
    let (_, log) = start(RULES_VERSION, vec![failed.clone()], &clock);
    let b = log.text();
    assert_eq!(owed_debts(&b).pending, ["a.jsonl"], "B still owes for a.jsonl");
    let r = replay_str(&b).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    assert!(r.unrestored.iter().any(|u| u.starts_with("earlier log a.jsonl: its carry failed")), "{:?}", r.unrestored);
    assert_eq!(r.carried_restores[0].failed.as_deref(), Some("Permission denied (os error 13)"));
    // C, the next start: .last-run names B, which still owes for a.jsonl: tried again, now
    // readable, its values carried too (both checked)
    let clock = ManualClock::new(T0.plus_ms(1_200_000));
    let (e, log) = start(RULES_VERSION, vec![carry_of("b.jsonl", &b), carry_of("a.jsonl", &a)], &clock);
    assert_eq!(e.owed_restores().len(), 2);
    let c = log.text();
    let logs = [("b.jsonl", b.as_str()), ("a.jsonl", a.as_str())];
    let r = replay_str_with(&c, &with(&logs, false)).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    assert!(r.carried_restores.iter().all(|c| c.checked), "{:?}", r.carried_restores);
    // a C that drops a.jsonl from the chain (as the pointer rewritten after a failed carry did
    // before): caught against B
    let (_, log) = start(RULES_VERSION, vec![carry_of("b.jsonl", &b)], &clock);
    let w = whats(&replay_str_with(&log.text(), &with(&logs, false)).unwrap());
    assert!(has(&w, "a log still owed for (pending) not tried again at the start"), "{w:?}");
    // a C that carries from a log nobody named: caught
    let (_, log) = start(RULES_VERSION, vec![carry_of("b.jsonl", &b), carry_of("x.jsonl", &a)], &clock);
    let w = whats(&replay_str_with(&log.text(), &with(&[("b.jsonl", &b), ("x.jsonl", &a)], false)).unwrap());
    assert!(has(&w, "neither the pointer nor an earlier carried log named"), "{w:?}");
    // an operator writes a.jsonl off in B (it is gone for good): B owes nothing for it then
    let clock = ManualClock::new(T0.plus_ms(600_000));
    let (mut e, log) = start(RULES_VERSION, vec![failed], &clock);
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    assert!(matches!(
        e.write_off_restore(op.authority(), WriteOff::Log("other.jsonl".into()), "gone"),
        Err(EngineError::NotWrittenOff(_))
    ));
    assert!(e.write_off_restore(op.authority(), WriteOff::Log("a.jsonl".into()), " ").is_err(), "a note is needed");
    e.write_off_restore(op.authority(), WriteOff::Log("a.jsonl".into()), "the disk that held it failed")
        .unwrap();
    let b = log.text();
    assert!(owed_debts(&b).is_empty());
    let r = replay_str(&b).unwrap();
    assert!(r.ok() && r.unrestored.is_empty(), "{:?} {:?}", r.mismatches, r.unrestored);
    assert_eq!(r.restores_closed, ["earlier log a.jsonl: what it owed (its carry had failed): written off by op-1: the disk that held it failed"]);
    // a write-off of a log that was not pending, forged in: caught
    let forged = forge(&b, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::RestoreClosed { log, .. } = &mut e.body {
                *log = Some("other.jsonl".into());
            }
        }
    });
    assert!(has(&whats(&replay_str(&forged).unwrap()), "a carry closed for a log that is not pending"));
}

// ---- murmur-q05: an operator writes off values that will not come back --------------------

#[test]
fn an_operator_writes_off_a_platforms_values_under_their_own_session() {
    let a = owing_run();
    let clock = ManualClock::new(T0.plus_ms(600_000));
    let (mut e, log) = start(RULES_VERSION, vec![carry_of("a.jsonl", &a)], &clock);
    let op = e.open_operator_session(OperatorId::new("op-2")).unwrap();
    let off = |e: &mut Engine, u: &str, note: &str| e.write_off_restore(op.authority(), WriteOff::Platform(uid(u)), note);
    assert!(matches!(off(&mut e, "hawk-1", "lost"), Err(EngineError::NotWrittenOff(_))), "nothing owed to it");
    assert!(off(&mut e, "sky-1", "").is_err(), "a note is needed");
    let n = log.lines().len();
    off(&mut e, "sky-1", "crashed in the bay; recovered by boat").unwrap();
    assert_eq!(log.lines().len(), n + 1);
    assert_eq!(e.owed_restores().len(), 1, "boat-1 still owed");
    assert!(!e.restore_owed_to(&uid("sky-1")));
    let text = log.text();
    let logs = [("a.jsonl", a.as_str())];
    let r = replay_str_with(&text, &with(&logs, false)).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.unrestored.len(), 1, "{:?}", r.unrestored);
    assert!(r.restores_closed[0].contains("written off by op-2: crashed in the bay"), "{:?}", r.restores_closed);
    assert_eq!(owed_debts(&text).owed.len(), 1, "not carried to the next run");
    // forged: under another operator's name, other values, without its note, or as expended
    let closed = |f: &dyn Fn(&mut EventBody)| {
        forge(&text, |ev| {
            for e in ev.iter_mut().filter(|e| matches!(e.body, EventBody::RestoreClosed { .. })) {
                f(&mut e.body);
            }
        })
    };
    for (forged, what) in [
        (
            closed(&|b| {
                if let EventBody::RestoreClosed { closed_by: RestoreClosure::WrittenOff { operator, .. }, .. } = b {
                    *operator = OperatorId::new("op-3");
                }
            }),
            "a write-off under a session that is not the operator's",
        ),
        (
            closed(&|b| {
                if let EventBody::RestoreClosed { values, .. } = b {
                    values.insert("FS_GCS_TIMEOUT".into(), 120.0);
                }
            }),
            "a restore closed for values not owed back to the platform",
        ),
        (
            closed(&|b| {
                if let EventBody::RestoreClosed { closed_by: RestoreClosure::WrittenOff { note, .. }, .. } = b {
                    note.clear();
                }
            }),
            "a write-off without a note",
        ),
        (
            closed(&|b| {
                if let EventBody::RestoreClosed { closed_by, .. } = b {
                    *closed_by = RestoreClosure::Expended { intent: IntentId(1) };
                }
            }),
            "a restore closed as expended, not right after the release",
        ),
    ] {
        let w = whats(&replay_str_with(&forged, &with(&logs, false)).unwrap());
        assert!(has(&w, what), "{what}: {w:?}");
    }
    // rules 10 has no write-off: an engine of them refuses it
    let (mut e10, _) = start(PASSKEY_SINCE, Vec::new(), &clock);
    let op = e10.open_operator_session(OperatorId::new("op-1")).unwrap();
    assert!(e10.write_off_restore(op.authority(), WriteOff::Platform(uid("sky-1")), "x").is_err());
}

// ---- murmur-qjf: values carried over hold a mission for the vehicle preloaded --------------

/// B, carrying sky-1's own values over from A, and an operator's orbit for sky-1 given on a
/// world where the bridge identifies sky-1 as `who`. Returns the engine, its log, the orbit's
/// id and how it was routed.
fn orbit_after_carry(version: u32, who: Option<VehicleIdentity>) -> (Engine, MemorySink, ManualClock, IntentId, Routed) {
    let a = owing_run();
    let clock = ManualClock::new(T0.plus_ms(600_000));
    let (mut e, log) = if version >= REVIEW_FOLLOWUPS_SINCE {
        start(version, vec![carry_of("a.jsonl", &a)], &clock)
    } else {
        // rules 10 carried with platform.restore events
        let (mut e, log) = start(version, Vec::new(), &clock);
        for o in owed_debts(&a).owed {
            e.record_platform_restore(PlatformRestoreReport {
                uid: o.uid,
                values: o.values,
                carried: true,
                from: Some("a.jsonl (carried over into it)".into()),
                readback: BTreeMap::new(),
                error: None,
                vehicle: o.vehicle,
            })
            .unwrap();
        }
        (e, log)
    };
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let w = world_at(clock.now(), who);
    let id = e.submit(op.authority(), orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w, &terrain()).unwrap().id();
    let routed = e.dispatch_or_queue(id, &w).unwrap();
    (e, log, clock, id, routed)
}

#[test]
fn values_carried_over_hold_a_mission_for_the_vehicle_they_were_preloaded_on() {
    // the same vehicle: the orbit waits for the restore, saying why
    let (mut e, log, clock, id, routed) = orbit_after_carry(RULES_VERSION, Some(sky()));
    let Routed::Queued { reach, .. } = &routed else {
        panic!("held: {routed:?}");
    };
    assert!(reach[0].detail.contains("carried over from a.jsonl"), "{reach:?}");
    // a hold or rtl, the safety commands, are never held
    let op = e.open_operator_session(OperatorId::new("op-3")).unwrap();
    let w = world_at(clock.now(), Some(sky()));
    let h = e.submit(op.authority(), hold(&["sky-1"]), &w, &terrain()).unwrap().id();
    assert!(matches!(e.dispatch_or_queue(h, &w).unwrap(), Routed::Dispatched(_)));
    // the restore confirms: the orbit goes on its re-check
    clock.advance_ms(2_000);
    let back = PlatformRestoreReport {
        uid: uid("sky-1"),
        values: own(),
        carried: false,
        from: None,
        readback: own(),
        error: None,
        vehicle: None,
    };
    assert!(e.record_platform_restore(back).unwrap());
    // (the hold, newer, stands: give the orbit again for this check)
    let w = world_at(clock.now(), Some(sky()));
    let again = e.submit(op.authority(), orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w, &terrain()).unwrap().id();
    assert!(matches!(e.dispatch_or_queue(again, &w).unwrap(), Routed::Dispatched(_)), "{:?}", e.intent(id).map(Intent::state));
    let text = log.text();
    let a = owing_run();
    let logs = [("a.jsonl", a.as_str())];
    let r = replay_str_with(&text, &with(&logs, false)).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    assert!(text.contains("\"restore_owed\":{\"sky-1\":{\"values\":{\"FS_GCS_ENABLE\":1.0,\"FS_GCS_TIMEOUT\":5.0,\"FS_OPTIONS\":0.0},\"by\":[],\"carried\":{\"from\":\"a.jsonl\",\"preloaded_on\":"), "{text}");
    // forged: the fact stripped from the decisions that held the orbit, or the vehicle said
    // to be another while its logged identity is the same one: caught against the ledger
    let stripped = forge(&text, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::IntentQueued { inputs, .. } | EventBody::CheckResult { inputs, .. } | EventBody::PolicyDecision { inputs, .. } = &mut e.body {
                inputs.restore_owed.clear();
            }
        }
    });
    let w = whats(&replay_str_with(&stripped, &with(&logs, false)).unwrap());
    assert!(has(&w, "lost-link values owed back differ from the log's ledger"), "{w:?}");
    let other = forge(&text, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::IntentQueued { inputs, .. } = &mut e.body
                && let Some(o) = inputs.restore_owed.get_mut(&uid("sky-1"))
                && let Some(c) = &mut o.carried
            {
                c.other_vehicle = Some("its callsign is \"SKY 9\"".into());
            }
        }
    });
    let w = whats(&replay_str_with(&other, &with(&logs, false)).unwrap());
    assert!(has(&w, "lost-link values owed back differ from the log's ledger"), "{w:?}");
}

#[test]
fn another_vehicle_with_the_uid_is_not_held_and_an_unknown_one_is() {
    // another vehicle under the uid now (its callsign): the values are not its own, it goes
    let mut nine = sky();
    nine.callsign = Some("SKY 9".into());
    let (_, log, _, _, routed) = orbit_after_carry(RULES_VERSION, Some(nine));
    assert!(matches!(routed, Routed::Dispatched(_)), "{routed:?}");
    let text = log.text();
    assert!(text.contains("\"other_vehicle\":\"its callsign is \\\"SKY 9\\\", not \\\"SKY 1\\\"\""), "{text}");
    let a = owing_run();
    let r = replay_str_with(&text, &with(&[("a.jsonl", &a)], false)).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    // not known yet (no identity from the bridge): held, fail closed
    let (_, _, _, _, routed) = orbit_after_carry(RULES_VERSION, None);
    assert!(matches!(routed, Routed::Queued { .. }), "{routed:?}");
    // rules 10, before the fix: the carried values held nothing, the orbit went over them
    let (_, log, _, _, routed) = orbit_after_carry(PASSKEY_SINCE, Some(sky()));
    assert!(matches!(routed, Routed::Dispatched(_)), "{routed:?}");
    let r = replay_str_with(
        &log.text(),
        &ReplayOptions {
            rules: RulesAccepted::AtLeast(PASSKEY_SINCE),
            restores_from: Some(("a.jsonl", &a)),
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(r.ok(), "rules 10 found nothing wrong with it: {:?}", r.mismatches);
}
