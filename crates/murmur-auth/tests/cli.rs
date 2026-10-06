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
    // a command refused before it reads its password exits first: its stdin is closed then
    // (a broken pipe here is that, not a failure)
    if let Err(e) = c.stdin.take().unwrap().write_all(stdin.as_bytes()) {
        assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe, "{e}");
    }
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

    // a harness-held credential: marked, and listed so
    let out = cli(&file, &["add", "op-planner", "--scripted", "--engage"], &format!("{PW}\n"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a person"));
    let f = CredentialFile::load(&file).unwrap();
    assert!(f.operators["op-planner"].scripted && f.operators["op-planner"].engage && !f.operators["op-1"].scripted);
    assert!(cli(&file, &["scripted", "op-planner", "off"], "").status.success());
    assert!(!CredentialFile::load(&file).unwrap().operators["op-planner"].scripted);
    assert!(cli(&file, &["scripted", "op-planner", "on"], "").status.success());
    let text = String::from_utf8_lossy(&cli(&file, &["list"], "").stdout).to_string();
    assert!(text.contains("op-1") && text.contains("engagement authority"), "{text}");
    assert!(text.contains("engagement, scripted"), "{text}");
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

/// Rules 10: a one-time invitation link, the passkeys listed, one removed (the operator's
/// sessions end: the stamp is renewed), a scripted credential holding none.
#[test]
fn passkeys_are_invited_listed_and_removed() {
    use murmur_auth::RelyingParty;
    use murmur_decide::passkey::soft::SoftAuthenticator;

    let dir = scratch();
    let file = dir.join("credentials.json");
    assert!(cli(&file, &["add", "op-2", "--engage"], &format!("{PW}\n")).status.success());
    assert!(cli(&file, &["add", "op-planner", "--scripted"], &format!("{PW}\n")).status.success());
    let out = cli(&file, &["passkey-invite", "op-2", "--minutes", "5", "--url", "http://localhost:27173/"], "");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let link = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let code = link.strip_prefix("http://localhost:27173/#enroll=op-2.").expect(&link).to_string();
    let f = CredentialFile::load(&file).unwrap();
    assert!(!std::fs::read_to_string(&file).unwrap().contains(&code), "only the code's digest is kept");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    assert!(f.invite_valid("op-2", &code, now));
    assert!(!f.invite_valid("op-2", &code, now + 6 * 60), "expired after its minutes");
    assert!(!f.invite_valid("op-2", "not the code", now));
    assert!(!f.invite_valid("nobody", &code, now));
    // a harness's credential holds no passkey
    assert_eq!(cli(&file, &["passkey-invite", "op-planner"], "").status.code(), Some(1));

    // two passkeys registered (as the bridge does), then listed and one removed
    let rp = RelyingParty::new("localhost", &["http://localhost:27173".to_string()]).unwrap();
    let mut f = CredentialFile::load(&file).unwrap();
    for (seed, label) in [("op-2 laptop", "laptop"), ("op-2 phone", "phone")] {
        let mut a = SoftAuthenticator::new(seed);
        let existing = f.operators["op-2"].passkeys.clone();
        let (options, reg) = rp.start_registration("op-2", &existing).unwrap();
        let c = murmur_decide::passkey::unb64(options["publicKey"]["challenge"].as_str().unwrap()).unwrap();
        let entry = rp
            .finish_registration(&reg, &a.register("localhost", "http://localhost:27173", &c), label, now)
            .unwrap();
        f.add_passkey("op-2", entry.clone()).unwrap();
        assert!(f.add_passkey("op-2", entry.clone()).is_err(), "a credential is registered once");
        assert!(f.add_passkey("op-planner", entry).is_err());
    }
    let stamp = f.operators["op-2"].stamp.clone();
    f.save(&file).unwrap();
    let text = String::from_utf8_lossy(&cli(&file, &["passkeys", "op-2"], "").stdout).to_string();
    assert!(text.contains("passkeys of op-2 (2)") && text.contains("laptop") && text.contains("phone"), "{text}");
    assert!(String::from_utf8_lossy(&cli(&file, &["list"], "").stdout).contains("2 passkeys"));
    // a credential copied into another file (a study's run-local one), passkeys and all
    let run_file = dir.join("run-credentials.json");
    let out = cli(&run_file, &["import", "op-2", "--from", file.to_str().unwrap()], "");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let copied = CredentialFile::load(&run_file).unwrap();
    assert_eq!(copied.operators["op-2"], CredentialFile::load(&file).unwrap().operators["op-2"]);
    assert_eq!(cli(&run_file, &["import", "op-9", "--from", file.to_str().unwrap()], "").status.code(), Some(1));
    assert_eq!(cli(&file, &["passkey-remove", "op-2", "tablet"], "").status.code(), Some(1), "no such passkey");
    assert!(cli(&file, &["passkey-remove", "op-2", "laptop"], "").status.success());
    let f = CredentialFile::load(&file).unwrap();
    assert_eq!(f.operators["op-2"].passkeys.len(), 1);
    assert_eq!(f.operators["op-2"].passkeys[0].label, "phone");
    assert_ne!(f.operators["op-2"].stamp, stamp, "removing a passkey ends the operator's sessions");
    // and a credential holding passkeys can't be made a harness's
    assert_eq!(cli(&file, &["scripted", "op-2", "on"], "").status.code(), Some(1));
    std::fs::remove_dir_all(dir).unwrap();
}
