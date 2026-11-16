//! `murmur-policy show <policy.toml> [--rules N]`: the policy a run given this file decides with,
//! as one line of JSON, for anything outside the engine that needs the merged values
//! (docs/SCENARIO-SETTINGS.md section 3: Python reads them only from here or from a log).
//!
//! `{"policy": ..., "layers": [{"path": ..., "sha256": ...}], "watch": null}`:
//! - `policy`: exactly what `run.started` logs for it (`Policy::load`, then `Policy::as_logged`):
//!   a supplement (`extends`) merged over the files it extends, its defaults written out;
//! - `layers`: the files it was built from, the one given first, then each file it extends in
//!   turn, each path as written (the first as given, each further one as its child's `extends`
//!   says, relative to the child) with the SHA-256 of its bytes;
//! - `watch`: the scenario's settings for watching (none yet).
//!
//! `--rules N` reads it as a run deciding under rules N would (the bridge's `MURMUR_RULES`, the
//! scenario runner's `--rules`): older rules log no `[identification]` and no step-up factors.
//! Default: the current rules.
//!
//! Exit status: 0 printed; 2 the file was refused (the reason on stderr, naming the file and
//! the key) or usage.

use std::path::Path;
use std::process::ExitCode;

use murmur_decide::rules::RULES_NEWEST;
use murmur_decide::{Layer, Policy};

const USAGE: &str = "usage: murmur-policy show <policy.toml> [--rules N]";

#[derive(serde::Serialize)]
struct Show<'a> {
    policy: &'a Policy,
    layers: &'a [Layer],
    /// The scenario's `[watch]` settings (docs/SCENARIO-SETTINGS.md stage S2): none yet.
    watch: Option<()>,
}

fn parse_args(args: &[String]) -> Result<(String, Option<u32>), String> {
    let mut it = args.iter().skip(1);
    match it.next().map(String::as_str) {
        Some("show") => {}
        Some(c) => return Err(format!("unknown command {c}")),
        None => return Err("no command".into()),
    }
    let mut path = None;
    let mut rules = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--rules" => {
                let n = it
                    .next()
                    .and_then(|v| v.parse::<u32>().ok())
                    .filter(|n| (1..=RULES_NEWEST).contains(n))
                    .ok_or_else(|| format!("--rules needs a rules version, 1 to {RULES_NEWEST}"))?;
                rules = Some(n);
            }
            f if f.starts_with("--") => return Err(format!("unknown option {f}")),
            p if path.is_none() => path = Some(p.to_string()),
            _ => return Err("one policy file at a time".into()),
        }
    }
    Ok((path.ok_or("no policy file given")?, rules))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let (path, rules) = match parse_args(&args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("murmur-policy: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let (mut policy, layers) = match Policy::load_layers(Path::new(&path)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("murmur-policy: {e}");
            return ExitCode::from(2);
        }
    };
    if let Some(n) = rules {
        policy.rules.rules_version = n;
    }
    let policy = policy.as_logged();
    match serde_json::to_string(&Show { policy: &policy, layers: &layers, watch: None }) {
        Ok(s) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("murmur-policy: {e}");
            ExitCode::from(2)
        }
    }
}
