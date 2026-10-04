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
    ls[3] = ls[3].replacen("op-", "op-x", 1);
    let forged = rechain(&ls);
    // the chain is consistent again: replay alone can't tell
    assert!(replay::replay_str(&forged).unwrap().ok());
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
