//! `murmur-policy show <policy.toml> [--rules N]`: the policy a run given this file decides with,
//! as one line of JSON, for anything outside the engine that needs the merged values
//! (docs/SCENARIO-SETTINGS.md section 3: Python reads them only from here or from a log).
//!
//! `{"policy": ..., "layers": [{"path": ..., "sha256": ...}], "watch": {...}}`:
//! - `policy`: exactly what `run.started` logs for it (`Policy::load`, then `Policy::as_logged`):
//!   a supplement (`extends`) merged over the files it extends, its defaults written out;
//! - `layers`: the files it was built from, the one given first, then each file it extends in
//!   turn, each path as written (the first as given, each further one as its child's `extends`
//!   says, relative to the child) with the SHA-256 of its bytes;
//! - `watch`: the watch settings a run given this file decides under (`Policy::effective_watch`,
//!   what MCP `get_policy` serves): from rules 33 the policy's `[watch]`; under older rules what
//!   they meant, `sim/policy-v0-watch.toml`'s `[watch]`, with the file's `[watch.area]`.
//!
//! `--rules N` reads it as a run deciding under rules N would (the bridge's `MURMUR_RULES`, the
//! scenario runner's `--rules`): older rules log no `[identification]` and no step-up factors,
//! and none of rules 33's settings, and refuse a file whose settings differ from the standing
//! values (as the bridge does). Default: the build's default rules (`RULES_VERSION`).
//!
//! Exit status: 0 printed; 2 the file was refused (the reason on stderr, naming the file and
//! the key) or usage.

use std::path::Path;
use std::process::ExitCode;

use murmur_decide::rules::RULES_NEWEST;
use murmur_decide::{Layer, Policy, WatchSettings};

const USAGE: &str = "usage: murmur-policy show <policy.toml> [--rules N]";

#[derive(serde::Serialize)]
struct Show<'a> {
    policy: &'a Policy,
    layers: &'a [Layer],
    /// The watch settings a run of this policy decides under (`Policy::effective_watch`).
    watch: &'a WatchSettings,
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
    // what the bridge refuses under these rules, refused here too
    if let Err(e) = policy.fits_rules() {
        eprintln!("murmur-policy: {path}: {e}");
        return ExitCode::from(2);
    }
    let watch = policy.effective_watch();
    let policy = policy.as_logged();
    match serde_json::to_string(&Show { policy: &policy, layers: &layers, watch: &watch }) {
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
