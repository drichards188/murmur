//! `murmur-scenarios [--json] [--seed N --noise M] [--root DIR] [--log-dir DIR] <file-or-dir>...`
//!
//! Run engagement scenarios (`sim/scenarios/engage/*.toml`, format in
//! `crate::scenario` and `sim/scenarios/engage/README.md`) against the decision engine:
//! each plays its timeline, steps engagements as the bridge does, replays its audit log and,
//! if it names a tamper, checks that replay catches the forged log.
//!
//! `--json` prints one JSON object per scenario (one per line) for a harness; otherwise a
//! table. `--seed`/`--noise` add seeded Gaussian noise (metres, 1-sigma) to the track
//! estimates. `--log-dir` writes each run's audit log as `<scenario>.jsonl`. `--root` is the
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
            _ if a.starts_with("--") => {
                eprintln!("usage: murmur-scenarios [--json] [--seed N --noise M] [--root DIR] [--log-dir DIR] <file-or-dir>...");
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
            let tamper = match (&r.tamper, r.tamper_detected) {
                (Some(t), Some(true)) => format!(", tamper {t} caught"),
                (Some(t), _) => format!(", tamper {t} NOT caught"),
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
