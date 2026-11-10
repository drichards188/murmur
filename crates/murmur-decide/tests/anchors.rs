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

/// An engine logging to `anchors`' sink, with an orbit for sky-1 dispatched: its decisions
/// (`intent.state`) logged, nothing committed yet.
fn dispatched(anchors: &Anchors, log: &MemorySink) -> (Engine, IntentId) {
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
    (e, id)
}

fn decisions(log: &str) -> usize {
    lines(log)
        .iter()
        .filter(|l| l.contains("\"type\":\"intent.state\"") || l.contains("\"type\":\"op.state\""))
        .count()
}

fn parsed(text: &str) -> Vec<Anchor> {
    lines(text).iter().map(|l| serde_json::from_str(l).unwrap()).collect()
}

#[test]
fn a_batch_of_decisions_is_anchored_once_and_a_crash_leaves_only_the_last_batch() {
    // murmur-fqid: a decision marks an anchor due and the batch's commit writes one, on the
    // newest line, for all of the batch's decisions; a crash before the commit leaves exactly
    // that batch unanchored, everything before it anchored. (Until then every decision was
    // anchored as it was written: review 2.)
    let dir = scratch("decisions");
    let cfg = AnchorConfig {
        path: dir.join("run.anchors"),
        every: 100,
        cmd: None,
    };
    let file = || std::fs::read_to_string(&cfg.path).unwrap();
    let log = MemorySink::new();
    let anchors = Anchors::open(&cfg, Path::new("memory")).unwrap();
    let (mut e, id) = dispatched(&anchors, &log);
    for n in 0..4 {
        e.record_op_state(id, &uid("sky-1"), "sent", &format!("uploading {n}")).unwrap();
    }
    let batch = decisions(&log.text());
    assert!(batch >= 6, "{}", log.text());
    // the batch's decisions wait for its commit: the start anchor alone so far
    assert_eq!(parsed(&file()).len(), 1, "{}", file());
    let a = anchors.commit().unwrap().expect("decisions were logged");
    let first = lines(&log.text()).len();
    assert_eq!((a.why.as_str(), a.seq), ("decision", first as u64), "on the newest line");
    assert_eq!(parsed(&file()).len(), 2, "one anchor for {batch} decisions");
    assert_eq!(anchors.syncs(), 2, "one fsync for them (and the start's)");
    assert!(anchors.commit().unwrap().is_none(), "nothing due, no anchor");
    assert_eq!(anchors.syncs(), 2, "and nothing to sync");
    // the next batch, then a crash before its commit
    for n in 4..7 {
        e.record_op_state(id, &uid("sky-1"), "sent", &format!("uploading {n}")).unwrap();
    }
    let ls = lines(&log.text());
    let text = file();
    let r = verify(&log.text(), &text);
    assert!(!r.shutdown);
    // what a carry-over checks: no problem but the shutdown anchor a crash never wrote
    assert!(r.problems.iter().all(|p| p.what.starts_with("no shutdown anchor")), "{:?}", r.problems);
    assert_eq!(r.last_anchored, Some(first as u64), "the first batch, to its last line");
    assert_eq!(r.after_last, (ls.len() - first) as u64, "exactly the last batch is not");
    assert_eq!(ls.len() - first, 3);
    // the accepted cost: that batch cut away can't be told from a run that ended there...
    let r_cut = verify(&rechain(&ls[..first]), &text);
    assert!(r_cut.problems.iter().all(|p| p.what.starts_with("no shutdown anchor")), "{:?}", r_cut.problems);
    // ...but a cut into the anchored batch is caught, and so is a rewritten decision in it
    let r_short = verify(&rechain(&ls[..first - 1]), &text);
    assert!(whats(&r_short).iter().any(|w| w.contains("log truncated")), "{:?}", whats(&r_short));
    let mut forged = ls[..first].to_vec();
    let at = forged.iter().position(|l| l.contains("uploading 2")).unwrap();
    forged[at] = forged[at].replacen("uploading 2", "uploading 9", 1);
    let r_forged = verify(&rechain(&forged), &text);
    assert!(whats(&r_forged).iter().any(|w| w.contains("the log was rewritten")), "{:?}", whats(&r_forged));
    // had the batch been committed, it would be anchored too: the gap is that batch alone
    anchors.commit().unwrap().expect("due");
    let r_after = verify(&log.text(), &file());
    assert_eq!((r_after.last_anchored, r_after.after_last), (Some(ls.len() as u64), 0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_burst_of_decisions_costs_its_cadence_anchors_and_one_commit() {
    // a batch of many decisions (thousands of vehicles starting at once): the cadence anchors
    // fall as they did, so fewer than `every` events are ever unanchored, and the commit adds
    // one and syncs them all at once; the anchor command gets each anchor written, no more
    let dir = scratch("burst");
    let posted = dir.join("posted");
    let cfg = AnchorConfig {
        path: dir.join("run.anchors"),
        every: 5,
        cmd: Some(format!("cat >> '{}'", posted.display())),
    };
    let file = || std::fs::read_to_string(&cfg.path).unwrap();
    let log = MemorySink::new();
    let anchors = Anchors::open(&cfg, Path::new("memory")).unwrap();
    let (mut e, id) = dispatched(&anchors, &log);
    anchors.commit().unwrap();
    let before = parsed(&file()).len();
    let synced = anchors.syncs();
    let from = lines(&log.text()).len() as u64;
    assert_eq!(parsed(&file()).last().unwrap().seq, from, "the dispatch's batch committed");
    for n in 0..23 {
        e.record_op_state(id, &uid("sky-1"), "sent", &format!("uploading {n}")).unwrap();
        let head = lines(&log.text()).len() as u64;
        let last = parsed(&file()).last().unwrap().seq;
        assert!(head - last < 5, "{} events unanchored", head - last);
    }
    assert_eq!(anchors.syncs(), synced, "the cadence anchors wait for the commit's fsync");
    let a = anchors.commit().unwrap().expect("decisions were logged");
    assert_eq!(anchors.syncs(), synced + 1, "one fsync for the batch");
    let all = parsed(&file());
    // 23 decisions: four cadence anchors (every 5 events) and the commit's on the last three
    assert_eq!(
        all[before..].iter().map(|a| (a.why.as_str(), a.seq - from)).collect::<Vec<_>>(),
        vec![
            ("every 5 events", 5),
            ("every 5 events", 10),
            ("every 5 events", 15),
            ("every 5 events", 20),
            ("decision", 23)
        ]
    );
    assert_eq!(a.seq, from + 23);
    anchors.close().unwrap();
    anchors.flush_hook();
    let text = file();
    let r = verify(&log.text(), &text);
    assert!(r.ok(), "{:?}", r.problems);
    assert_eq!(std::fs::read_to_string(&posted).unwrap(), text, "one command a written anchor");
    // tamper evidence against batched anchors, as against any: a rewritten decision, a cut
    // tail, a removed commit anchor (the cadence anchor after it no longer where it belongs)
    let ls = lines(&log.text());
    let mut forged = ls.clone();
    let at = forged.iter().position(|l| l.contains("uploading 21")).unwrap();
    forged[at] = forged[at].replacen("uploading 21", "uploading 99", 1);
    let r = verify(&rechain(&forged), &text);
    assert!(whats(&r).iter().any(|w| w.contains("the log was rewritten")), "{:?}", whats(&r));
    let r = verify(&rechain(&ls[..ls.len() - 2]), &text);
    assert!(whats(&r).iter().any(|w| w.contains("log truncated")), "{:?}", whats(&r));
    let al = lines(&text);
    let i = al.iter().position(|a| a.contains("\"why\":\"decision\"")).unwrap();
    assert!(al[i + 1].contains("every 5 events"), "{}", al[i + 1]);
    let mut v = al.clone();
    v.remove(i);
    let r = verify(&log.text(), &v.join("\n"));
    assert!(whats(&r).iter().any(|w| w.contains("anchor missing")), "{:?}", whats(&r));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_anchor_file_with_an_anchor_on_every_decision_still_verifies() {
    // anchor files written before murmur-fqid hold a "decision" anchor on each decision; the
    // verifier reads them as it did (and the live ones committed: tests/versions.rs)
    let dir = scratch("old");
    let cfg = AnchorConfig {
        path: dir.join("run.anchors"),
        every: 100,
        cmd: None,
    };
    let log = MemorySink::new();
    let anchors = Anchors::open(&cfg, Path::new("memory")).unwrap();
    let (mut e, id) = dispatched(&anchors, &log);
    anchors.commit().unwrap();
    for n in 0..4 {
        e.record_op_state(id, &uid("sky-1"), "sent", &format!("uploading {n}")).unwrap();
        // what the writer did after every decision
        anchors.commit().unwrap();
    }
    anchors.close().unwrap();
    let text = std::fs::read_to_string(&cfg.path).unwrap();
    assert_eq!(parsed(&text).iter().filter(|a| a.why == "decision").count(), 5, "{text}");
    let r = verify(&log.text(), &text);
    assert!(r.ok(), "{:?}", r.problems);
    let _ = std::fs::remove_dir_all(&dir);
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
