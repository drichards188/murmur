//! murmur-dup: a clock that steps back. Before the bridge kept a clock of its own, the
//! engine read the wall clock (`SystemClock`), and so did every fact's stamp; this host's
//! wall clock steps back about 0.65 s every 32 s. The engine never logs a time earlier than
//! the last one and decides only on the times it logs, so even a clock that reads earlier
//! than it did a second before leaves every log in order, and replay reproduces every
//! decision: run here for every adversarial scenario, on clocks stepping back 1.5 s every
//! 32 s and 3 s every 5 s (the scenarios tick once a second, so both read earlier than the
//! tick before at every step).
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use murmur_decide::scenario::{self, ClockStep, RunOptions};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn a_clock_stepping_back_leaves_every_log_in_order_and_replayed() {
    let files = scenario::files(&root().join("sim/scenarios/engage")).unwrap();
    assert!(files.len() >= 78, "{} scenarios", files.len());
    for step in [
        ClockStep { every_s: 32, back_ms: 1_500 },
        ClockStep { every_s: 5, back_ms: 3_000 },
    ] {
        let opts = RunOptions {
            clock_step: Some(step),
            ..RunOptions::default()
        };
        let mut held = 0;
        let mut otherwise = Vec::new();
        for f in &files {
            let r = scenario::run_file(f, &root(), &opts).unwrap();
            assert!(r.replay_ok, "{} on {step:?}: {:?}", r.file, r.replay_mismatches);
            let ts: Vec<i64> = r
                .log
                .lines()
                .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["t"].as_i64().unwrap())
                .collect();
            assert!(ts.windows(2).all(|w| w[0] <= w[1]), "{}: a time went back", r.file);
            // a forged log is caught whenever the run wrote what the forgery edits
            assert_ne!(r.tamper_detected, Some(false), "{}", r.file);
            if r.ok {
                held += 1;
            } else {
                otherwise.push(format!("{}: {:?}", r.file.rsplit('/').next().unwrap_or(""), r.failures));
            }
        }
        // a designed end may move with time itself (a clock that loses 3 s in every 5 runs
        // timelines at less than half speed): reported, not a replay question
        println!("{step:?}: {held} of {} scenarios ended as designed; otherwise: {otherwise:#?}", files.len());
    }
}
