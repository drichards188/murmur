//! `murmur-replay <log.jsonl> [--space <constellations.toml> <cache-dir>]`: recompute every
//! check and policy decision in an audit log from its logged inputs and report any mismatch.
//! With `--space`, also re-run every logged space query (collect, cover) on that space
//! segment and compare; a different segment is a cache mismatch, reported separately.
//!
//! Exit status: 0 all decisions reproduced (and, with `--space`, every space query too),
//! 1 decision mismatches found, 2 unreadable log or usage, 3 decisions reproduced but some
//! space queries could not be recomputed (cache mismatch).

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use murmur_decide::SpaceSegment;
use murmur_decide::replay::{Mismatch, replay_str_with_space};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let (path, space) = match args.as_slice() {
        [_, path] => (path, None),
        [_, path, flag, catalog, cache] if flag == "--space" => (path, Some((catalog, cache))),
        _ => {
            eprintln!(
                "usage: murmur-replay <audit-log.jsonl> [--space <constellations.toml> <cache-dir>]"
            );
            return ExitCode::from(2);
        }
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("murmur-replay: {path}: {e}");
            return ExitCode::from(2);
        }
    };
    let segment = match space {
        None => None,
        Some((catalog, cache)) => {
            // the run's pinned time shift (space.loaded), so answers are recomputed on the
            // same propagation day
            let pin = murmur_decide::space::logged_pinned_shift(&text);
            let loaded = murmur_space::Space::load(Path::new(catalog), Path::new(cache))
                .map_err(|e| e.to_string())
                .and_then(|mut s| {
                    s.pinned_shift_days = pin;
                    SpaceSegment::new(Arc::new(s))
                });
            match loaded {
                Ok(s) => Some(s),
                Err(e) => {
                    eprintln!("murmur-replay: space segment: {e}");
                    return ExitCode::from(2);
                }
            }
        }
    };
    let oracle = segment
        .as_ref()
        .map(|s| s as &dyn murmur_decide::SpaceOracle);
    let report = match replay_str_with_space(&text, oracle) {
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
    println!(
        "{} area set(s) logged; {} approval(s), {} self-approved; {} authorized at submission",
        report.area_sets, report.approvals, report.self_approvals, report.approvals_at_submission
    );
    if report.space_queries > 0 {
        let how = match &segment {
            None => "not recomputed (pass --space to re-run them)".to_string(),
            Some(_) => format!("{} recomputed", report.space_recomputed),
        };
        println!(
            "{} space query answer(s) logged; {how}",
            report.space_queries
        );
    }
    if let Some(head) = &report.head_sha256 {
        println!("head {head} (compare with an external anchor to detect a rewrite or truncation)");
    }
    if !report.ok() {
        println!("MISMATCH: {} problem(s)", report.mismatches.len());
        print_all(&report.mismatches);
        return ExitCode::from(1);
    }
    println!("OK: every decision reproduced exactly");
    if !report.cache_mismatches.is_empty() {
        println!(
            "CACHE MISMATCH: {} space query(ies) ran on a different space segment and were not recomputed",
            report.cache_mismatches.len()
        );
        print_all(&report.cache_mismatches);
        return ExitCode::from(3);
    }
    ExitCode::SUCCESS
}

fn print_all(ms: &[Mismatch]) {
    for m in ms {
        let intent = m.intent.map(|i| format!(" {i}")).unwrap_or_default();
        println!("  seq {}{intent}: {}", m.seq, m.what);
        if !m.logged.is_empty() || !m.recomputed.is_empty() {
            println!("    logged:     {}", m.logged);
            println!("    recomputed: {}", m.recomputed);
        }
    }
}
