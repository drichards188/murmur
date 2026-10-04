//! The build's commit and whether the working tree differed from it, for `run.started`
//! (`audit::BuildInfo`): `MURMUR_BUILD_COMMIT` (the hash, or "unknown" outside a git
//! checkout) and `MURMUR_BUILD_DIRTY` ("1", "0", or "?" when unknown).
//!
//! Rerun when HEAD moves (checkout, commit), the index changes, or any crate's source
//! changes. A tracked file outside `crates/` edited since the last run is not seen until
//! one of those happens: the flag is about the code that decides.

use std::path::Path;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let commit = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = match git(&["status", "--porcelain", "--untracked-files=no"]) {
        Some(s) if s.is_empty() => "0",
        Some(_) => "1",
        None => "?",
    };
    println!("cargo:rustc-env=MURMUR_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=MURMUR_BUILD_DIRTY={dirty}");
    // HEAD, the index and the branch ref (in a worktree, its own git dir and the common one)
    let mut watch = vec!["HEAD".to_string(), "index".to_string()];
    if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watch.push(r);
    }
    watch.push("packed-refs".into());
    let here = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    for w in watch {
        if let Some(p) = git(&["rev-parse", "--git-path", &w]) {
            let p = Path::new(&here).join(p);
            if p.exists() {
                println!("cargo:rerun-if-changed={}", p.display());
            }
        }
    }
    // the workspace's crates (this one's sources included)
    println!("cargo:rerun-if-changed=..");
}
