//! `murmur-replay <log.jsonl>`: recompute every check and policy decision in an audit log
//! from its logged inputs and report any mismatch.
//!
//! Exit status: 0 all decisions reproduced, 1 mismatches found, 2 unreadable log or usage.

use std::process::ExitCode;

use murmur_decide::replay::replay_str;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let [_, path] = args.as_slice() else {
        eprintln!("usage: murmur-replay <audit-log.jsonl>");
        return ExitCode::from(2);
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("murmur-replay: {path}: {e}");
            return ExitCode::from(2);
        }
    };
    let report = match replay_str(&text) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("murmur-replay: {path}: {e}");
            return ExitCode::from(2);
        }
    };
    println!(
        "{path}: {} events, {} intents, {} checks and {} policy decisions recomputed, {} transitions verified",
        report.events,
        report.intents,
        report.checks_recomputed,
        report.policy_recomputed,
        report.transitions_verified
    );
    if report.ok() {
        println!("OK: every decision reproduced exactly");
        return ExitCode::SUCCESS;
    }
    println!("MISMATCH: {} problem(s)", report.mismatches.len());
    for m in &report.mismatches {
        let intent = m.intent.map(|i| format!(" {i}")).unwrap_or_default();
        println!("  seq {}{intent}: {}", m.seq, m.what);
        if !m.logged.is_empty() || !m.recomputed.is_empty() {
            println!("    logged:     {}", m.logged);
            println!("    recomputed: {}", m.recomputed);
        }
    }
    ExitCode::from(1)
}
