//! `murmur-scenarios [--json] [--seed N --noise M --vnoise V] [--rules N] [--reaches legacy|physical] [--root DIR] [--log-dir DIR] <file-or-dir>...`
//!
//! Run engagement scenarios (`sim/scenarios/engage/*.toml`, format in
//! `crate::scenario` and `sim/scenarios/engage/README.md`) against the decision engine:
//! each plays its timeline, steps engagements as the bridge does, replays its audit log and,
//! if it names a tamper, checks that replay catches the forged log.
//!
//! `--json` prints one JSON object per scenario (one per line) for a harness; otherwise a
//! table. `--seed`/`--noise` add seeded Gaussian noise (metres, 1-sigma) to the track
//! position estimates, `--vnoise` (m/s, 1-sigma, correlated over about 10 s) to their
//! velocity estimates. `--rules N` decides under older decision rules (what they did with
//! the same timeline; the designed ends are the current rules'). Rules 9: `--ident on|off`
//! sets the policy's `[identification] rule_declaration` (the evaluation's rule-versus-manual
//! comparison on the same seeds), `--threshold X` its attribution threshold and
//! `--nominate-unconfirmed` its nomination setting (the file's other keys stay);
//! `--corroboration none|second_observation` its corroboration; `--act-noise` adds seeded
//! noise of each act's own sigma to where its author (and each further observer's fix) was
//! observed; `--proxy` has a scripted operator review what the identification puts in the
//! inbox by its own procedure over what the console shows (`scenario::ReviewProxy`; with
//! `--ident off` it declares from the acts itself; a model, never human data), and
//! `--nominator` nominates every declared track and has two signers sign it.
//! `--reaches legacy|physical` (bd murmur-1u4): which reach of our sensors the engine reads (the
//! assessment's coverage), as the bridge's facts carry it on that sensing model: `range_m`
//! (`legacy`, the default, the designed ends') or the catalog's `physical_reach_m`.
//! `--log-dir` writes each run's audit log as `<scenario>.jsonl`. `--root` is the
//! repo (default: the current directory) for `sim/policy.toml`, `sim/platforms.toml` and
//! `sim/areas`.
//!
//! Exit status: 0 every scenario ended as designed, 1 some did not, 2 usage or a scenario
//! could not be loaded.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use murmur_decide::scenario::{RunOptions, files, run_file};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut json = false;
    let mut root = PathBuf::from(".");
    let mut log_dir: Option<PathBuf> = None;
    let mut opts = RunOptions::default();
    let mut paths = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--json" => json = true,
            "--root" => root = args.next().map(PathBuf::from).unwrap_or(root),
            "--log-dir" => log_dir = args.next().map(PathBuf::from),
            "--seed" => opts.seed = args.next().and_then(|s| s.parse().ok()),
            "--noise" => opts.track_noise_m = args.next().and_then(|s| s.parse().ok()).unwrap_or(0.0),
            "--vnoise" => {
                opts.velocity_noise_mps = args.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
            }
            // decide under older decision rules (what they did with the same timeline)
            "--rules" => opts.rules_version = args.next().and_then(|s| s.parse().ok()),
            // rules 9: the policy's [identification] instead of the file's (manual: off; rule:
            // on), its threshold and nomination setting, and seeded noise on acts' positions
            "--ident" => {
                let on = match args.next().as_deref() {
                    Some("on" | "rule") => true,
                    Some("off" | "manual") => false,
                    _ => {
                        eprintln!("murmur-scenarios: --ident on|off");
                        return ExitCode::from(2);
                    }
                };
                opts.identification.rule_declaration = Some(on);
            }
            "--threshold" => opts.identification.attribution_threshold = args.next().and_then(|s| s.parse().ok()),
            "--nominate-unconfirmed" => opts.identification.nominate_unconfirmed = Some(true),
            "--act-noise" => opts.act_noise = true,
            "--corroboration" => {
                opts.identification.corroboration = match args.next().as_deref() {
                    Some("none") => Some(murmur_decide::ident::Corroboration::None),
                    Some("second_observation") => Some(murmur_decide::ident::Corroboration::SecondObservation),
                    _ => {
                        eprintln!("murmur-scenarios: --corroboration none|second_observation");
                        return ExitCode::from(2);
                    }
                };
            }
            // the evaluation: a scripted operator reviewing the identification (a model, never
            // human data), its parameters, and the nominator
            "--proxy" => {
                opts.proxy.get_or_insert_with(Default::default);
            }
            "--proxy-diligence" => {
                let v = args.next().and_then(|s| s.parse().ok()).unwrap_or(0.8);
                opts.proxy.get_or_insert_with(Default::default).diligence = v;
            }
            "--proxy-misread" => {
                let v = args.next().and_then(|s| s.parse().ok()).unwrap_or(0.05);
                opts.proxy.get_or_insert_with(Default::default).misread = v;
            }
            "--proxy-attend" => {
                let v = args.next().and_then(|s| s.parse().ok()).unwrap_or(20.0);
                opts.proxy.get_or_insert_with(Default::default).attend_s = v;
            }
            "--nominator" => opts.nominator = true,
            "--reaches" => {
                opts.reaches = match args.next().as_deref() {
                    Some("legacy") => murmur_decide::Reaches::Legacy,
                    Some("physical") => murmur_decide::Reaches::Physical,
                    _ => {
                        eprintln!("murmur-scenarios: --reaches legacy|physical");
                        return ExitCode::from(2);
                    }
                };
            }
            _ if a.starts_with("--") => {
                eprintln!("usage: murmur-scenarios [--json] [--seed N --noise M --vnoise V] [--rules N] [--ident on|off] [--threshold X] [--nominate-unconfirmed] [--corroboration none|second_observation] [--act-noise] [--proxy [--proxy-diligence P] [--proxy-misread P] [--proxy-attend S]] [--nominator] [--reaches legacy|physical] [--root DIR] [--log-dir DIR] <file-or-dir>...");
                return ExitCode::from(2);
            }
            _ => paths.push(PathBuf::from(a)),
        }
    }
    if paths.is_empty() {
        paths.push(root.join("sim/scenarios/engage"));
    }
    let mut all = Vec::new();
    for p in &paths {
        if p.is_dir() {
            match files(p) {
                Ok(v) => all.extend(v),
                Err(e) => {
                    eprintln!("murmur-scenarios: {e}");
                    return ExitCode::from(2);
                }
            }
        } else {
            all.push(p.clone());
        }
    }
    let mut bad = 0;
    for f in &all {
        let r = match run_file(f, &root, &opts) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("murmur-scenarios: {e}");
                return ExitCode::from(2);
            }
        };
        if let Some(dir) = &log_dir {
            let stem = Path::new(f).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            if let Err(e) = std::fs::create_dir_all(dir)
                .and_then(|()| std::fs::write(dir.join(format!("{stem}.jsonl")), &r.log))
            {
                eprintln!("murmur-scenarios: {}: {e}", dir.display());
            }
        }
        if !r.ok {
            bad += 1;
        }
        if json {
            println!("{}", serde_json::to_string(&r).unwrap_or_default());
        } else {
            let mark = if r.ok { "ok  " } else { "FAIL" };
            let tamper = match (&r.tamper, r.tamper_detected, &r.tamper_not_applicable) {
                (Some(t), Some(true), _) => format!(", tamper {t} caught"),
                (Some(t), None, Some(why)) => format!(", tamper {t} not applicable ({why})"),
                (Some(t), _, _) => format!(", tamper {t} NOT caught"),
                _ => String::new(),
            };
            println!(
                "{mark} {:28} {:>16} [{}]{} replay {} ({} events){tamper}",
                r.invariant,
                r.final_state.as_deref().unwrap_or("-"),
                r.failed_rules.join(", "),
                r.assessment.as_deref().map(|a| format!(" assessed {a}")).unwrap_or_default(),
                if r.replay_ok { "ok" } else { "MISMATCH" },
                r.replay_events,
            );
            println!("     {}: {}", r.title, r.attack);
            for f in &r.failures {
                println!("     ! {f}");
            }
        }
    }
    if bad == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
