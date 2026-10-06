//! Run-time anchors: the log's first line and head, written outside it as it grows, catch a
//! rewritten log, a truncated tail and events appended after a clean shutdown.
#![allow(clippy::unwrap_used)]

mod common;

use std::path::{Path, PathBuf};

use common::*;
use murmur_decide::anchor::verify;
use murmur_decide::*;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("murmur-anchors-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A short run whose log is anchored every 5 events, and its anchor file's text.
fn anchored_run(dir: &Path, cmd: Option<String>) -> (MemorySink, String) {
    let cfg = AnchorConfig {
        path: dir.join("run.anchors"),
        every: 5,
        cmd,
    };
    let log = MemorySink::new();
    let anchors = Anchors::open(&cfg, Path::new("memory")).unwrap();
    let clock = ManualClock::new(T0);
    let mut e = Engine::with_areas(
        policy(),
        Some(areas()),
        // the twelve operators the run opens sessions for
        Roster::new((0..12).map(|n| OperatorId::new(format!("op-{n}"))).collect(), Vec::new())
            .unwrap(),
        Box::new(clock.clone()),
        Box::new(anchors.sink(log.clone())),
    )
    .unwrap();
    for n in 0..12 {
        clock.advance_ms(1000);
        e.open_operator_session(OperatorId::new(format!("op-{n}")))
            .unwrap();
    }
    // a clean stop: the shutdown anchor, and the log closed behind it
    anchors.close().unwrap();
    anchors.flush_hook();
    let before = log.lines().len();
    assert!(
        e.open_operator_session(OperatorId::new("op-late")).is_err(),
        "nothing is logged, or done, after the shutdown anchor"
    );
    assert_eq!(log.lines().len(), before);
    let text = std::fs::read_to_string(&cfg.path).unwrap();
    (log, text)
}

fn lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
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

fn whats(r: &AnchorReport) -> Vec<&str> {
    r.problems.iter().map(|m| m.what.as_str()).collect()
}

#[test]
fn a_log_matches_its_anchors() {
    let dir = scratch("ok");
    let (log, anchors) = anchored_run(&dir, None);
    let r = verify(&log.text(), &anchors);
    assert!(r.ok(), "{:?}", r.problems);
    // start (seq 1), every 5 events, the shutdown
    let n = lines(&log.text()).len() as u64;
    assert_eq!(r.anchors, lines(&anchors).len());
    assert!(r.anchors >= 3, "{anchors}");
    assert_eq!(r.last_anchored, Some(n));
    assert_eq!(r.after_last, 0);
    assert!(r.shutdown);
    // and replay itself still passes
    let rep = replay::replay_str(&log.text()).unwrap();
    assert!(rep.ok(), "{:?}", rep.mismatches);
}

#[test]
fn a_rewritten_log_does_not_match_its_anchors() {
    let dir = scratch("rewrite");
    let (log, anchors) = anchored_run(&dir, None);
    let mut ls = lines(&log.text());
    // a session credited to another operator on the roster (one off it, replay catches)
    assert!(ls[3].contains("\"operator\":\"op-2\""), "{}", ls[3]);
    ls[3] = ls[3].replacen("\"operator\":\"op-2\"", "\"operator\":\"op-11\"", 1);
    let forged = rechain(&ls);
    // the chain is consistent again: replay alone can't tell
    assert!(replay::replay_str(&forged).unwrap().ok());
    let mut off = lines(&log.text());
    off[3] = off[3].replacen("\"operator\":\"op-2\"", "\"operator\":\"op-x\"", 1);
    assert!(!replay::replay_str(&rechain(&off)).unwrap().ok());
    let r = verify(&forged, &anchors);
    assert!(
        whats(&r)
            .iter()
            .any(|w| w.contains("the log was rewritten")),
        "{:?}",
        whats(&r)
    );
}

#[test]
fn a_truncated_log_and_one_appended_after_shutdown_are_caught() {
    let dir = scratch("truncate");
    let (log, anchors) = anchored_run(&dir, None);
    let ls = lines(&log.text());
    let r = verify(&rechain(&ls[..ls.len() - 3]), &anchors);
    assert!(
        whats(&r).iter().any(|w| w.contains("log truncated")),
        "{:?}",
        whats(&r)
    );
    let mut more = ls.clone();
    more.push(ls[ls.len() - 1].replacen("op-", "op-late-", 1));
    let r = verify(&rechain(&more), &anchors);
    assert!(
        whats(&r)
            .iter()
            .any(|w| w.contains("after the shutdown anchor")),
        "{:?}",
        whats(&r)
    );
}

#[test]
fn another_runs_log_does_not_match() {
    let (log, anchors) = anchored_run(&scratch("other"), None);
    // the same events under another run.started (another policy value), re-chained
    let mut ls = lines(&log.text());
    ls[0] = ls[0].replacen("\"expire_s\":120", "\"expire_s\":121", 1);
    let r = verify(&rechain(&ls), &anchors);
    assert!(
        whats(&r).iter().any(|w| w.contains("first line")),
        "{:?}",
        whats(&r)
    );
    assert!(!verify(&log.text(), "").ok(), "no anchors is not a pass");
}

#[test]
fn the_anchor_command_receives_every_anchor() {
    let dir = scratch("cmd");
    let posted = dir.join("posted");
    let (_, anchors) = anchored_run(&dir, Some(format!("cat >> '{}'", posted.display())));
    let got = std::fs::read_to_string(&posted).unwrap();
    assert_eq!(got, anchors);
}

#[test]
fn removed_anchors_are_caught() {
    let (log, anchors) = anchored_run(&scratch("removed"), None);
    let text = log.text();
    let al = lines(&anchors);
    assert!(al.len() >= 4, "{anchors}");
    let without = |i: usize| {
        let mut v = al.clone();
        v.remove(i);
        v.join("\n")
    };
    // an anchor in the middle: the cadence breaks
    let r = verify(&text, &without(1));
    assert!(
        whats(&r).iter().any(|w| w.contains("anchor missing")),
        "{:?}",
        whats(&r)
    );
    // the shutdown anchor: no clean end
    let r = verify(&text, &without(al.len() - 1));
    assert!(
        whats(&r).iter().any(|w| w.contains("no shutdown anchor")),
        "{:?}",
        whats(&r)
    );
    // the start anchor
    let r = verify(&text, &without(0));
    assert!(
        whats(&r).iter().any(|w| w.contains("not the start anchor")),
        "{:?}",
        whats(&r)
    );
    // only the start
    let r = verify(&text, &al[0]);
    assert!(
        whats(&r).iter().any(|w| w.contains("no shutdown anchor")),
        "{:?}",
        whats(&r)
    );
}

#[test]
fn a_crashed_log_is_anchored_up_to_its_last_decision() {
    // review 2: every intent transition and op state is anchored as it is written, so a log
    // whose run crashed (no shutdown anchor) is covered up to its last decision, not only up
    // to the last cadence anchor
    let dir = scratch("decisions");
    let cfg = AnchorConfig {
        path: dir.join("run.anchors"),
        every: 100,
        cmd: None,
    };
    let log = MemorySink::new();
    let anchors = Anchors::open(&cfg, Path::new("memory")).unwrap();
    let clock = ManualClock::new(T0);
    let mut e = Engine::new(
        policy(),
        roster(),
        Box::new(clock.clone()),
        Box::new(anchors.sink(log.clone())),
    )
    .unwrap();
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let w = world();
    let id = e
        .submit(op.authority(), orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w, &terrain())
        .unwrap()
        .id();
    e.dispatch(id, &w).unwrap();
    e.record_op_state(id, &uid("sky-1"), "sent", "uploading").unwrap();
    // a crash: no shutdown anchor
    let text = std::fs::read_to_string(&cfg.path).unwrap();
    let ls = lines(&log.text());
    let r = verify(&log.text(), &text);
    assert!(!r.shutdown);
    assert!(
        r.problems.iter().all(|p| p.what.starts_with("no shutdown anchor")),
        "{:?}",
        r.problems
    );
    assert_eq!(r.last_anchored, Some(ls.len() as u64), "the last event, an op state, is anchored");
    assert!(lines(&text).iter().any(|a| a.contains("\"why\":\"decision\"")), "{text}");
    // a decision anchor removed is caught: the cadence anchor after it is no longer where it
    // belongs, or the next decision anchor too far
    let al = lines(&text);
    let i = al.iter().position(|a| a.contains("\"why\":\"decision\"")).unwrap();
    let mut v = al.clone();
    v.remove(i);
    let r2 = verify(&log.text(), &v.join("\n"));
    assert!(r2.problems.len() >= r.problems.len());
    // the decisions' tail truncated: caught
    let r3 = verify(&rechain(&ls[..ls.len() - 1]), &text);
    assert!(whats(&r3).iter().any(|w| w.contains("log truncated")), "{:?}", whats(&r3));
}

/// murmur-dup: an anchor is timed by its writer's clock, and the verifier holds the file to
/// times that never go back. A clock that steps back between two anchors (this host's wall
/// clock steps back about 0.65 s every 32 s; anchors come every few events and at every
/// decision) must not make a genuine log fail its anchors.
#[test]
fn a_clock_stepping_back_leaves_the_anchors_in_order() {
    let dir = scratch("step");
    let cfg = AnchorConfig {
        path: dir.join("run.anchors"),
        every: 2,
        cmd: None,
    };
    let wall = ManualClock::new(T0);
    // each anchor also records the wall clock beside the writer's (here 1.5 s behind it), so
    // a log's times map to UTC
    let anchors = Anchors::open_with(&cfg, Path::new("memory"), Box::new(wall.clone()))
        .unwrap()
        .with_utc(Box::new(ManualClock::new(T0.plus_ms(-1500))));
    let log = MemorySink::new();
    let clock = ManualClock::new(T0);
    let mut e = Engine::with_areas(
        policy(),
        Some(areas()),
        Roster::new((0..6).map(|n| OperatorId::new(format!("op-{n}"))).collect(), Vec::new())
            .unwrap(),
        Box::new(clock.clone()),
        Box::new(anchors.sink(log.clone())),
    )
    .unwrap();
    for n in 0..6 {
        clock.advance_ms(300);
        wall.advance_ms(300);
        if n == 3 {
            wall.advance_ms(-940);
        }
        e.open_operator_session(OperatorId::new(format!("op-{n}")))
            .unwrap();
    }
    anchors.close().unwrap();
    let text = std::fs::read_to_string(&cfg.path).unwrap();
    let r = verify(&log.text(), &text);
    assert!(r.ok(), "{:?}", r.problems);
    let ts: Vec<i64> = lines(&text)
        .iter()
        .map(|l| serde_json::from_str::<Anchor>(l).unwrap().t)
        .collect();
    assert!(ts.windows(2).all(|w| w[0] <= w[1]), "{ts:?}");
    for l in lines(&text) {
        assert_eq!(serde_json::from_str::<Anchor>(&l).unwrap().utc, Some(T0.0 - 1500), "{l}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
