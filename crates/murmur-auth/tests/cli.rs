//! The CLI, driven non-interactively as scripts and tests drive it (password on stdin).
#![allow(clippy::unwrap_used)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use murmur_auth::CredentialFile;

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "murmur-auth-cli-{}-{}",
        std::process::id(),
        murmur_auth::keys::stamp().unwrap()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn cli(file: &Path, args: &[&str], stdin: &str) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_murmur-auth"))
        .arg("--file")
        .arg(file)
        .args(args)
        .env_remove("MURMUR_CREDENTIALS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    c.wait_with_output().unwrap()
}

const PW: &str = "correct horse battery staple";

#[test]
fn add_list_change_and_remove_without_a_terminal() {
    let dir = scratch();
    let file = dir.join("credentials.json");
    let out = cli(&file, &["add", "op-1"], &format!("{PW}\n"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = cli(&file, &["add", "op-2", "--engage"], "another long passphrase\r\n");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // refused: a duplicate, a short password, an empty stdin, a bad id
    assert_eq!(cli(&file, &["add", "op-1"], &format!("{PW}\n")).status.code(), Some(1));
    assert_eq!(cli(&file, &["add", "op-3"], "too short\n").status.code(), Some(1));
    assert_eq!(cli(&file, &["add", "op-3"], "").status.code(), Some(1));
    assert_eq!(cli(&file, &["add", "Jane Doe"], &format!("{PW}\n")).status.code(), Some(1));
    assert_eq!(cli(&file, &["frobnicate"], "").status.code(), Some(2));

    let f = CredentialFile::load(&file).unwrap();
    assert!(f.verify_operator("op-1", PW).is_some());
    assert!(f.verify_operator("op-2", "another long passphrase").is_some(), "the CR is stripped");
    assert!(!f.operators["op-1"].engage && f.operators["op-2"].engage);

    let list = cli(&file, &["list"], "");
    let text = String::from_utf8_lossy(&list.stdout);
    assert!(text.contains("op-1") && text.contains("engagement authority"), "{text}");
    assert!(!text.contains("argon2"), "no hash in a listing: {text}");

    let stamp = f.operators["op-1"].stamp.clone();
    assert!(cli(&file, &["engage", "op-1", "on"], "").status.success());
    assert!(cli(&file, &["passwd", "op-1"], "a brand new passphrase\n").status.success());
    let f = CredentialFile::load(&file).unwrap();
    assert!(f.operators["op-1"].engage);
    assert_ne!(f.operators["op-1"].stamp, stamp);
    assert!(f.verify_operator("op-1", PW).is_none());
    assert!(f.verify_operator("op-1", "a brand new passphrase").is_some());

    // an agent's key, printed once on stdout, stored only as its digest
    let out = cli(&file, &["agent-add", "agent-planner"], "");
    assert!(out.status.success());
    let key = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(key.starts_with("mma_"), "{key}");
    let stored = std::fs::read_to_string(&file).unwrap();
    assert!(!stored.contains(&key));
    assert!(CredentialFile::load(&file).unwrap().agent_for_key(&key).is_some());
    let out = cli(&file, &["agent-rotate", "agent-planner"], "");
    let key2 = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let f = CredentialFile::load(&file).unwrap();
    assert!(f.agent_for_key(&key).is_none() && f.agent_for_key(&key2).is_some());

    assert!(cli(&file, &["remove", "op-1"], "").status.success());
    assert!(cli(&file, &["agent-remove", "agent-planner"], "").status.success());
    let f = CredentialFile::load(&file).unwrap();
    assert!(!f.operators.contains_key("op-1") && f.agents.is_empty());
    assert!(!file.with_extension("lock").exists(), "the lock is released");
    std::fs::remove_dir_all(dir).unwrap();
}
