//! murmur-aca, through `murmur-replay`: a run's carry-over checked against the earlier logs it
//! names. A restarted bridge carries open intents (rules 5) from the log
//! `.resume-<port>.json` or `MURMUR_RESUME=<log>` names, and the lost-link values a run still
//! owed (rules 6) from the one `.last-run-<port>` names: two pointers that can name two
//! different logs. Each is found by the name the log gives it (`--prev`, or beside the log),
//! and the output says which log each check used and what it could not check.
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use common::*;
use murmur_decide::audit::EventBody;
use murmur_decide::replay::{ReplayOptions, ReplayReport, owed_restores, replay_str, replay_str_with};
use murmur_decide::*;

/// boat-1's own lost-link values (ArduRover: hold on a lost link after 5 s).
fn own() -> BTreeMap<String, f64> {
    [("FS_ACTION", 2.0), ("FS_GCS_ENABLE", 1.0), ("FS_GCS_TIMEOUT", 5.0)]
        .into_iter()
        .map(|(n, v)| (n.to_string(), v))
        .collect()
}

/// The vehicle they were preloaded on.
fn boat() -> VehicleIdentity {
    VehicleIdentity {
        platform: Some("seahunter".into()),
        callsign: Some("BOAT 1".into()),
        sysid: Some(4),
        home: Some(BAY),
    }
}

/// Run A1: an orbit queued for sky-1 (its link silent), still open when the bridge crashed.
fn queued_run() -> (String, ReplayReport) {
    let clock = ManualClock::new(T0.plus_ms(5_000));
    let (mut e, log) = engine(&clock);
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    // every fact 5 s old: fresh enough for the checks, a link silent for 3 of its intervals
    let mut w = world();
    w.t = clock.now();
    let id = e
        .submit(op.authority(), orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w, &terrain())
        .unwrap()
        .id();
    assert!(matches!(e.dispatch_or_queue(id, &w).unwrap(), Routed::Queued { .. }));
    let text = log.text();
    let r = replay_str(&text).unwrap();
    assert!(r.ok() && r.open.len() == 1, "{:?} {:?}", r.mismatches, r.open);
    (text, r)
}

/// Run A2, the last on the port: boat-1's own values, carried over into it from an earlier
/// run, never confirmed back (the boat did not answer): it still owes them.
fn owing_run() -> String {
    let clock = ManualClock::new(T0.plus_ms(30_000));
    let (mut e, log) = engine(&clock);
    let carried = PlatformRestoreReport {
        uid: uid("boat-1"),
        values: own(),
        carried: true,
        from: Some("a0.jsonl (preloaded by i3)".into()),
        readback: BTreeMap::new(),
        error: None,
        vehicle: Some(boat()),
    };
    e.record_platform_restore(carried).unwrap();
    clock.advance_ms(15_000);
    let attempt = PlatformRestoreReport {
        uid: uid("boat-1"),
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

/// Run B, started with `MURMUR_RESUME=a1.jsonl` after A2: A1's orbit carried over
/// (`intent.resumed`, naming a1.jsonl) and what A2 still owed (`platform.restore` carried, as
/// the bridge's `carry_from` writes it, naming a2.jsonl), put back once the boat answered.
fn carrying_run(a1: &ReplayReport, a2: &str) -> String {
    let clock = ManualClock::new(T0.plus_ms(120_000));
    let (mut e, log) = engine(&clock);
    e.reserve_ids(a1.next_intent);
    let open = &a1.open[0];
    let from = ResumedFrom {
        log: "a1.jsonl".into(),
        head: a1.head_sha256.clone().unwrap(),
        seq: a1.last_seq,
        was: open.state,
    };
    e.resume(open, from).unwrap();
    for o in owed_restores(a2) {
        assert!(o.by.is_empty());
        let carried = PlatformRestoreReport {
            uid: o.uid,
            values: o.values,
            carried: true,
            from: Some("a2.jsonl (carried over into it)".into()),
            readback: BTreeMap::new(),
            error: None,
            vehicle: o.vehicle,
        };
        e.record_platform_restore(carried).unwrap();
    }
    clock.advance_ms(3_000);
    let back = PlatformRestoreReport {
        uid: uid("boat-1"),
        values: own(),
        carried: false,
        from: None,
        readback: own(),
        error: None,
        vehicle: None,
    };
    assert!(e.record_platform_restore(back).unwrap());
    log.text()
}

/// A scratch directory holding the given logs, removed after the test.
struct Dir(PathBuf);

impl Dir {
    fn with(name: &str, files: &[(&str, &str)]) -> Dir {
        let dir = std::env::temp_dir().join(format!("murmur-carried-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (f, text) in files {
            std::fs::write(dir.join(f), text).unwrap();
        }
        Dir(dir)
    }

    fn path(&self, f: &str) -> String {
        self.0.join(f).display().to_string()
    }

    /// `murmur-replay <dir>/b.jsonl <args>`: its exit status and output.
    fn replay(&self, args: &[String]) -> (Option<i32>, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_murmur-replay"))
            .arg(self.path("b.jsonl"))
            .args(args)
            .output()
            .unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        (out.status.code(), text)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_run_carrying_from_two_logs_is_checked_against_each_and_says_which() {
    let (a1, r1) = queued_run();
    let a2 = owing_run();
    let b = carrying_run(&r1, &a2);
    // both beside it: each found by the name the log gives it
    let d = Dir::with("both", &[("a1.jsonl", &a1), ("a2.jsonl", &a2), ("b.jsonl", &b)]);
    let (code, out) = d.replay(&[]);
    assert_eq!(code, Some(0), "{out}");
    let intents = format!("carried-over intents: 1 from a1.jsonl, checked against {}", d.path("a1.jsonl"));
    let values = format!(
        "carried lost-link values: 1 platform(s) from a2.jsonl, checked against what {} still owed",
        d.path("a2.jsonl")
    );
    assert!(out.contains(&intents) && out.contains(&values), "{out}");
    assert!(!out.contains("UNCHECKED"), "{out}");
    // only the intents' log at hand: the carried values are said to be unchecked, not failed
    let d = Dir::with("one", &[("a1.jsonl", &a1), ("b.jsonl", &b)]);
    let (code, out) = d.replay(&[]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("carried-over intents: 1 from a1.jsonl, checked against"), "{out}");
    assert!(
        out.contains("UNCHECKED: 1 platform(s)' carried lost-link values from a2.jsonl: that log was not given (--prev) or found beside this one"),
        "{out}"
    );
    // given from elsewhere, one --prev each, matched by file name whatever their order; a
    // --prev the log names nowhere is said and not used
    let logs = Dir::with("logs", &[("a1.jsonl", &a1), ("a2.jsonl", &a2), ("x.jsonl", &a2)]);
    let d = Dir::with("alone", &[("b.jsonl", &b)]);
    let prev = |f: &str| ["--prev".to_string(), logs.path(f)];
    let args: Vec<String> = [prev("a2.jsonl"), prev("a1.jsonl"), prev("x.jsonl")].concat();
    let (code, out) = d.replay(&args);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains(&format!("checked against {}", logs.path("a1.jsonl"))), "{out}");
    assert!(out.contains(&format!("checked against what {} still owed", logs.path("a2.jsonl"))), "{out}");
    assert!(out.contains(&format!("--prev {}: not a log this one's carried-over", logs.path("x.jsonl"))), "{out}");
    // the intents' log alone, a single --prev, as before
    let (code, out) = d.replay(&prev("a1.jsonl"));
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("UNCHECKED: 1 platform(s)' carried lost-link values from a2.jsonl"), "{out}");
}

#[test]
fn carried_values_the_log_they_name_did_not_owe_are_caught() {
    let (a1, r1) = queued_run();
    let a2 = owing_run();
    let b = carrying_run(&r1, &a2);
    // the forger's B: boat-1 "restored" to a 120 s timer of its choosing (carried and put
    // back alike, so B is consistent on its own), and the values sent to another vehicle
    let forged = forge(&b, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::PlatformRestore { values, readback, vehicle, carried, .. } = &mut e.body {
                values.insert("FS_GCS_TIMEOUT".into(), 120.0);
                if !readback.is_empty() {
                    readback.insert("FS_GCS_TIMEOUT".into(), 120.0);
                }
                if *carried && let Some(v) = vehicle {
                    v.callsign = Some("BOAT 9".into());
                }
            }
        }
    });
    let trusted = ReplayOptions {
        trust_resumed: true,
        ..ReplayOptions::default()
    };
    let alone = replay_str_with(&forged, &trusted).unwrap();
    assert!(alone.ok(), "consistent on its own: {:?}", alone.mismatches);
    let d = Dir::with("forged", &[("a1.jsonl", &a1), ("a2.jsonl", &a2), ("b.jsonl", &forged)]);
    let (code, out) = d.replay(&[]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("carried values differ from what the log it names still owed"), "{out}");
    assert!(out.contains("carried restore names another vehicle than the one the log it names preloaded"), "{out}");
    // the genuine B beside the same logs passes
    let d = Dir::with("genuine", &[("a1.jsonl", &a1), ("a2.jsonl", &a2), ("b.jsonl", &b)]);
    assert_eq!(d.replay(&[]).0, Some(0));
}
