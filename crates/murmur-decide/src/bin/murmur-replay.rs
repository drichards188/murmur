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
//!   last anchor. A rewritten log replays as well as the original: a credentials
//!   run's log is evidence about people only with its anchors checked, and without
//!   `--anchors` the output says they were not.
//! - `--space` also re-runs every logged space query (collect, cover) on that space segment
//!   and compares; a different segment is a cache mismatch, reported separately.
//! - Rules 10: every passkey login and step-up's assertion is verified against the public key
//!   the run logged (its challenge recomputed from the run, the session and the act it was made
//!   for, its counter counting up, each challenge answered once); the output counts them, and
//!   for a study says whether its participant's every consequential act was by passkey.
//! - Rules 5: a log whose `intent.resumed` events carry intents over from the previous run
//!   is checked against that log: `--prev <file>`, or by default the file the events name in
//!   the same directory.
//! - Rules 6: a log whose carried `platform.restore` events put back lost-link values an
//!   earlier run still owed is checked against the log they name: that log must replay, and
//!   they must be exactly what it still owed. It is found the same way, and it can be another
//!   log than the intents' (`.last-run-<port>` and `.resume-<port>.json` can name different
//!   logs), so `--prev` may be given once for each: each is matched to the events by its file
//!   name (a single `--prev` that matches neither is taken for the intents, as before). Only
//!   a bare file name is looked for beside the log, never a path.
//! - Either carry-over whose earlier log is not given or found is a mismatch;
//!   `--trust-resumed` accepts both without their logs (trusting their origin), and the
//!   output says so (`UNCHECKED:`). It also says which log each check used, with the head of
//!   the log the carried values name, to compare with that log's anchors.
//!
//! Exit status: 0 all decisions reproduced (and, with `--space`, every space query too; with
//! `--anchors`, the anchors hold), 1 decision or anchor mismatches found (a carry-over whose
//! earlier log was not given or found, without `--trust-resumed`, included), 2 unreadable log
//! or usage, 3 decisions reproduced but some space queries could not be recomputed (cache
//! mismatch).

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use murmur_decide::SpaceSegment;
use murmur_decide::replay::{
    Mismatch, ReplayOptions, ReplayReport, RulesAccepted, carried_restore_logs, is_log_file_name,
    replay_str_with,
};

const USAGE: &str = "usage: murmur-replay <audit-log.jsonl> [--rules N | --min-rules N] [--anchors <file>] [--space <constellations.toml> <cache-dir>] [--prev <previous-log.jsonl>]... [--trust-resumed]";

struct Args {
    path: String,
    rules: RulesAccepted,
    anchors: Option<String>,
    space: Option<(String, String)>,
    prev: Vec<String>,
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
    let mut prev = Vec::new();
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
            "--prev" => prev.push(it.next().ok_or("--prev needs a file")?.clone()),
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
    // the earlier logs this one carries from: rules 5 intents, rules 6 lost-link values
    let earlier = match Earlier::find(path, &text, &args.prev) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("murmur-replay: {e}");
            return ExitCode::from(2);
        }
    };
    for p in &earlier.unused {
        println!("--prev {p}: not a log this one's carried-over intents or lost-link values name; not used");
    }
    let opts = ReplayOptions {
        rules: args.rules,
        space: oracle,
        previous: earlier.intents.as_ref().map(|(_, t)| t.as_str()),
        trust_resumed: args.trust_resumed,
        restores_from: earlier
            .restores
            .as_ref()
            .map(|(name, _, t)| (name.as_str(), t.as_str())),
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
    // rules 8: whose decisions these are, as far as the log can say
    match report.auth {
        Some(murmur_decide::AuthMode::Credentials) => {
            let list = |s: &std::collections::BTreeSet<murmur_decide::OperatorId>| {
                if s.is_empty() {
                    "none".to_string()
                } else {
                    s.iter().map(|o| o.as_str()).collect::<Vec<_>>().join(", ")
                }
            };
            println!(
                "auth: credentials (operators logged in with a checked credential; {} step-up(s), {} signature(s) and {} other act(s) quoting one, {} session(s) ended, {} login(s) refused)",
                report.step_ups, report.stepped_signatures, report.stepped_acts, report.sessions_closed, report.logins_refused
            );
            println!(
                "people (own password or passkey): {}; scripted (harness-held credential, not people): {}",
                list(&report.people),
                list(&report.scripted)
            );
            // rules 10: passkeys, each use verified against the public key the run logged
            if report.passkey_logins + report.passkey_step_ups + report.passkeys_registered + report.passkeys_removed > 0 {
                println!(
                    "passkeys: {} login(s) and {} step-up(s) by passkey, each assertion verified against the run's logged key and bound to its act; {} step-up(s) by password; {} registered and {} removed during the run",
                    report.passkey_logins,
                    report.passkey_step_ups,
                    report.password_step_ups,
                    report.passkeys_registered,
                    report.passkeys_removed
                );
            }
            if let Some(s) = &report.study {
                let verdict = if report.study_evidence() {
                    "every consequential act of theirs by passkey"
                } else if s.password_step_up.is_some() {
                    "PASSWORD STEP-UPS ALLOWED by the override: a pilot, NOT evidence about people"
                } else {
                    "NOT evidence about people (see the counts)"
                };
                println!(
                    "study: participant {} ({} act(s) by passkey, {} by password{}): {verdict}",
                    s.participant,
                    s.passkey_acts,
                    s.password_acts,
                    s.password_step_up.as_ref().map(|w| format!("; override: {w}")).unwrap_or_default()
                );
            }
            // a rewritten log replays too: only its anchors tell
            if args.anchors.is_none() {
                println!(
                    "anchors: NOT checked (pass --anchors <file>): until they are, this log is not evidence about people"
                );
            }
        }
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
    // rules 9: every act's attribution and verdict, every rule declaration and flag recomputed
    let ident = report.acts + report.rule_declarations + report.operator_declarations + report.flags;
    if ident > 0 {
        println!(
            "identification: {} act(s) attributed again, {} rule declaration(s) and {} flag(s) recomputed, {} operator declaration(s), {} confirmation(s), {} revocation(s), {} flag(s) resolved",
            report.acts,
            report.rule_declarations,
            report.flags,
            report.operator_declarations,
            report.confirmations,
            report.revocations,
            report.flags_resolved
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
    earlier.print(&report, args.trust_resumed);
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

/// The earlier logs this one carries from, each read from `--prev` or found beside it.
struct Earlier {
    /// Rules 5: the log the first `intent.resumed` names, and the one used (path, text).
    intents_named: Option<String>,
    intents: Option<(String, String)>,
    /// Rules 6: the log the carried `platform.restore` events name (the first, if they name
    /// several: a mismatch) as used: its name, path, text.
    restores: Option<(String, String, String)>,
    /// `--prev` files neither is.
    unused: Vec<String>,
}

impl Earlier {
    fn find(path: &str, text: &str, given: &[String]) -> Result<Earlier, String> {
        let intents_named = text.lines().find_map(|l| {
            if !l.contains("\"type\":\"intent.resumed\"") {
                return None;
            }
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            v.get("from")?.get("log")?.as_str().map(str::to_string)
        });
        let restores_named = carried_restore_logs(text).into_iter().next();
        let named = |n: &str| given.iter().find(|g| file_name(g).as_deref() == Some(n)).cloned();
        let intents_file = intents_named.as_deref().and_then(|n| {
            named(n)
                .or_else(|| {
                    // as before: a single --prev is the intents' log, whatever its name (the
                    // head and last seq the events name bind it), unless it is the restores'
                    let only = (given.len() == 1).then(|| given[0].clone())?;
                    (file_name(&only) != restores_named).then_some(only)
                })
                .or_else(|| beside(path, n))
        });
        let restores_file = restores_named
            .as_deref()
            .and_then(|n| named(n).or_else(|| beside(path, n)));
        let read = |f: &String| std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"));
        let intents = match &intents_file {
            Some(f) => Some((f.clone(), read(f)?)),
            None => None,
        };
        let restores = match (&restores_named, &restores_file) {
            (Some(n), Some(f)) => Some((n.clone(), f.clone(), read(f)?)),
            _ => None,
        };
        let unused = given
            .iter()
            .filter(|g| Some(*g) != intents_file.as_ref() && Some(*g) != restores_file.as_ref())
            .cloned()
            .collect();
        Ok(Earlier {
            intents_named,
            intents,
            restores,
            unused,
        })
    }

    /// Which earlier log each check used, and what was not checked for want of one.
    fn print(&self, report: &ReplayReport, trusted: bool) {
        if report.resumed > 0 {
            let from = self.intents_named.as_deref().unwrap_or("?");
            match &self.intents {
                Some((p, _)) => println!(
                    "carried-over intents: {} from {from}, checked against {p}",
                    report.resumed
                ),
                None if trusted => println!(
                    "UNCHECKED: {} intent(s) carried over from {from}: trusted (--trust-resumed); that log was not given (--prev) or found beside this one",
                    report.resumed
                ),
                None => println!(
                    "carried-over intents: {} from {from}, NOT checked: that log was not given (--prev) or found beside this one",
                    report.resumed
                ),
            }
        }
        for c in &report.carried_restores {
            let from = &c.log;
            match &self.restores {
                Some((name, p, t)) if c.checked && name == from => {
                    let same = match &self.intents {
                        Some((_, u)) if u == t => ", the carried-over intents' log too",
                        _ => "",
                    };
                    let head = c.head.as_deref().unwrap_or("none: it can't be read");
                    println!(
                        "carried lost-link values: {} platform(s) from {from}, checked against what {p} still owed{same} (its head {head}: compare with its anchors)",
                        c.restores
                    );
                }
                _ if trusted => println!(
                    "UNCHECKED: {} platform(s)' carried lost-link values from {from}: trusted (--trust-resumed); that log was not given (--prev) or found beside this one",
                    c.restores
                ),
                _ => println!(
                    "carried lost-link values: {} platform(s) from {from}, NOT checked: that log was not given (--prev) or found beside this one",
                    c.restores
                ),
            }
        }
    }
}

/// A path's file name, as a log's carried events name a log.
fn file_name(p: &str) -> Option<String> {
    Path::new(p)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
}

/// The file `name` beside the log at `path`, if `name` is a bare file name (never a path out
/// of the log's directory), the file exists and is another file than the log.
fn beside(path: &str, name: &str) -> Option<String> {
    if !is_log_file_name(name) {
        return None;
    }
    let dir = Path::new(path).parent().unwrap_or_else(|| Path::new("."));
    let file = dir.join(name);
    let same = match (file.canonicalize(), Path::new(path).canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    (file.exists() && !same).then(|| file.display().to_string())
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
