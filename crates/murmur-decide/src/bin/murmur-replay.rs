//! `murmur-replay <log.jsonl> [--rules N | --min-rules N] [--anchors <file>]
//! [--space <constellations.toml> <cache-dir>]`: recompute every check and policy decision in
//! an audit log from its logged inputs and report any mismatch.
//!
//! - A log is recomputed with the decision rules it records, but only rules accepted: by
//!   default the current ones. `--rules N` accepts exactly version N, `--min-rules N` version
//!   N or later: an older log is replayed with its own rules only when asked for, so a log
//!   can't pass by claiming older, looser rules.
//! - `--anchors <file>` checks the log against its run-time anchors (`logs/anchors/`): its
//!   first line and every anchored head must hash as anchored, and the log must reach the
//!   last anchor.
//! - `--space` also re-runs every logged space query (collect, cover) on that space segment
//!   and compares; a different segment is a cache mismatch, reported separately.
//! - Rules 5: a log whose `intent.resumed` events carry intents over from the previous run
//!   is checked against that log: `--prev <file>`, or by default the file the events name in
//!   the same directory. `--trust-resumed` accepts them without it (trusting their origin).
//!
//! Exit status: 0 all decisions reproduced (and, with `--space`, every space query too; with
//! `--anchors`, the anchors hold), 1 decision or anchor mismatches found, 2 unreadable log or
//! usage, 3 decisions reproduced but some space queries could not be recomputed (cache
//! mismatch).

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use murmur_decide::SpaceSegment;
use murmur_decide::replay::{Mismatch, ReplayOptions, RulesAccepted, replay_str_with};

const USAGE: &str = "usage: murmur-replay <audit-log.jsonl> [--rules N | --min-rules N] [--anchors <file>] [--space <constellations.toml> <cache-dir>] [--prev <previous-log.jsonl> | --trust-resumed]";

struct Args {
    path: String,
    rules: RulesAccepted,
    anchors: Option<String>,
    space: Option<(String, String)>,
    prev: Option<String>,
    trust_resumed: bool,
}

fn version(v: Option<&String>, flag: &str) -> Result<u32, String> {
    v.and_then(|v| v.parse().ok())
        .filter(|n| *n >= 1)
        .ok_or_else(|| format!("{flag} needs a rules version (1, 2, ...)"))
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut path = None;
    let mut rules = None;
    let mut anchors = None;
    let mut space = None;
    let mut prev = None;
    let mut trust_resumed = false;
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--rules" | "--min-rules" => {
                if rules.is_some() {
                    return Err("give one of --rules and --min-rules".into());
                }
                let n = version(it.next(), a)?;
                rules = Some(if a == "--rules" {
                    RulesAccepted::Exactly(n)
                } else {
                    RulesAccepted::AtLeast(n)
                });
            }
            "--anchors" => anchors = Some(it.next().ok_or("--anchors needs a file")?.clone()),
            "--prev" => prev = Some(it.next().ok_or("--prev needs a file")?.clone()),
            "--trust-resumed" => trust_resumed = true,
            "--space" => {
                let catalog = it.next().ok_or("--space needs a catalog and a cache dir")?;
                let cache = it.next().ok_or("--space needs a catalog and a cache dir")?;
                space = Some((catalog.clone(), cache.clone()));
            }
            f if f.starts_with("--") => return Err(format!("unknown option {f}")),
            p if path.is_none() => path = Some(p.to_string()),
            _ => return Err("one log at a time".into()),
        }
    }
    Ok(Args {
        path: path.ok_or("no log given")?,
        rules: rules.unwrap_or_default(),
        anchors,
        space,
        prev,
        trust_resumed,
    })
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let args = match parse_args(&args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("murmur-replay: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let path = &args.path;
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("murmur-replay: {path}: {e}");
            return ExitCode::from(2);
        }
    };
    let segment = match &args.space {
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
    // rules 5: the previous run's log, for the intents this one carried over
    let prev_text = match previous_log(path, &text, args.prev.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("murmur-replay: {e}");
            return ExitCode::from(2);
        }
    };
    if let Some((p, _)) = &prev_text {
        println!("previous run's log (for carried-over intents): {p}");
    }
    let opts = ReplayOptions {
        rules: args.rules,
        space: oracle,
        previous: prev_text.as_ref().map(|(_, t)| t.as_str()),
        trust_resumed: args.trust_resumed,
    };
    let report = match replay_str_with(&text, &opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("murmur-replay: {path}: {e}");
            return ExitCode::from(2);
        }
    };
    let anchors = match &args.anchors {
        None => None,
        Some(file) => match std::fs::read_to_string(file) {
            Ok(a) => Some((file, murmur_decide::anchor::verify(&text, &a))),
            Err(e) => {
                eprintln!("murmur-replay: anchors {file}: {e}");
                return ExitCode::from(2);
            }
        },
    };
    let accepted = match args.rules {
        RulesAccepted::Exactly(n) => format!("accepting rules {n} only"),
        RulesAccepted::AtLeast(n) => format!("accepting rules {n} or later"),
    };
    println!(
        "{path}: decision rules version {} ({accepted}); {} events, {} intents, {} checks and {} policy decisions recomputed, {} transitions verified",
        report.rules_version,
        report.events,
        report.intents,
        report.checks_recomputed,
        report.policy_recomputed,
        report.transitions_verified
    );
    if let Some(b) = &report.build {
        let dirty = match b.dirty {
            Some(true) => ", working tree dirty",
            Some(false) => "",
            None => ", working tree unknown",
        };
        println!("written by build {}{dirty}", b.commit);
    }
    // rules 7: whose decisions these are, as far as the log can say
    match report.auth {
        Some(murmur_decide::AuthMode::Credentials) => println!(
            "auth: credentials (operators logged in with their own password; {} step-up(s), {} signature(s) quoting one, {} session(s) ended)",
            report.step_ups, report.stepped_signatures, report.sessions_closed
        ),
        Some(murmur_decide::AuthMode::Dev) => println!(
            "auth: DEV (operator ids asserted by request headers, not authenticated): not evidence about people; exclude from any human-control claim"
        ),
        None => println!(
            "auth: none recorded (rules before {}: operator ids asserted by request headers): not evidence about people",
            murmur_decide::OPERATOR_AUTH_SINCE
        ),
    }
    println!(
        "{} area set(s) logged; {} approval(s), {} self-approved; {} authorized at submission",
        report.area_sets, report.approvals, report.self_approvals, report.approvals_at_submission
    );
    if report.engagements > 0 {
        println!(
            "{} engagement(s): {} concurrence(s) before final approvals, {} picture verdict(s) recomputed ({} void(s)), {} effect(s), {} abort(s), {} assessment(s)",
            report.engagements,
            report.concurrences,
            report.pictures_recomputed,
            report.picture_voids,
            report.effects,
            report.aborts,
            report.assessments
        );
    }
    if report.queued + report.rechecks + report.resumed + report.reboots + report.requeued > 0 {
        println!(
            "queue: {} queued, {} sent back after a lost link, {} re-check(s) recomputed ({} dispatched, {} back to the inbox, {} voided), {} expired; {} carried over from a previous run; {} vehicle reboot(s)",
            report.queued,
            report.requeued,
            report.rechecks,
            report.recheck_dispatched,
            report.recheck_inbox,
            report.recheck_voided,
            report.queue_expired,
            report.resumed,
            report.reboots
        );
    }
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
    // rules 6: not a decision mismatch, but the operator should know
    for u in &report.unrestored {
        println!("UNRESTORED: {u}");
    }
    let anchors_ok = match &anchors {
        None => true,
        Some((file, a)) => {
            let last = a.last_anchored.map_or("none".to_string(), |s| s.to_string());
            println!(
                "{} anchor(s) in {file}: up to seq {last}, {} event(s) after it not covered{}",
                a.anchors,
                a.after_last,
                if a.shutdown {
                    "; ended with a shutdown anchor"
                } else {
                    ""
                }
            );
            if a.ok() {
                println!("ANCHORS OK: the log matches every anchor");
            } else {
                println!("ANCHOR MISMATCH: {} problem(s)", a.problems.len());
                print_all(&a.problems);
            }
            a.ok()
        }
    };
    if !report.ok() {
        println!("MISMATCH: {} problem(s)", report.mismatches.len());
        print_all(&report.mismatches);
        return ExitCode::from(1);
    }
    println!("OK: every decision reproduced exactly");
    if !anchors_ok {
        return ExitCode::from(1);
    }
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

/// The previous run's log: `--prev`, else (when the log carries intents over) the file its
/// first `intent.resumed` names, beside it. None when there is nothing carried over.
fn previous_log(
    path: &str,
    text: &str,
    given: Option<&str>,
) -> Result<Option<(String, String)>, String> {
    let named = text.lines().find_map(|l| {
        if !l.contains("\"type\":\"intent.resumed\"") {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(l).ok()?;
        v.get("from")?.get("log")?.as_str().map(str::to_string)
    });
    let file = match (given, named) {
        (Some(g), _) => g.to_string(),
        (None, Some(n)) => {
            let beside = Path::new(path)
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(&n);
            if !beside.exists() {
                return Ok(None);
            }
            beside.display().to_string()
        }
        (None, None) => return Ok(None),
    };
    std::fs::read_to_string(&file)
        .map(|t| Some((file.clone(), t)))
        .map_err(|e| format!("{file}: {e}"))
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
