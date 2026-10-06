//! `murmur-auth`: manage Murmur's operator and agent credentials.
//!
//! ```text
//! murmur-auth [--file PATH] list
//! murmur-auth [--file PATH] add <operator> [--engage] [--scripted]   password from stdin
//! murmur-auth [--file PATH] passwd <operator>             password from stdin
//! murmur-auth [--file PATH] engage <operator> on|off      engagement authority
//! murmur-auth [--file PATH] scripted <operator> on|off    harness-held credential (not a person)
//! murmur-auth [--file PATH] remove <operator>
//! murmur-auth [--file PATH] agent-add <agent>             prints the new API key, once
//! murmur-auth [--file PATH] agent-rotate <agent>          prints a new key; the old one stops working
//! murmur-auth [--file PATH] agent-remove <agent>
//! murmur-auth [--file PATH] passkeys <operator>           list an operator's passkeys
//! murmur-auth [--file PATH] passkey-invite <operator> [--minutes N] [--url BASE]
//!                                                         a one-time registration link
//! murmur-auth [--file PATH] passkey-remove <operator> <label | id prefix>
//! murmur-auth [--file PATH] import <operator> --from FILE copy an operator's credential (its
//!                                                         password hash, flags and passkeys) from another file
//! murmur-auth [--file PATH] path                          the file it uses
//! ```
//!
//! Passkeys (rules 10): an operator registers one at the console while signed in (their
//! operator menu; the password is asked first), or through a one-time invitation:
//! `passkey-invite` prints a link to the console (`--url`, default
//! `http://localhost:$MURMUR_WEB_PORT`, 5173) carrying a code valid `--minutes` (30), used
//! once; whoever opens it registers a passkey for that operator, so hand it over as you would
//! the password. The file keeps public keys and counters only. `passkey-remove` removes one
//! (by its label, or the start of its credential id as `passkeys` lists it) and ends the
//! operator's sessions (a lost authenticator). A scripted credential holds no passkey.
//!
//! The file: `--file`, else `MURMUR_CREDENTIALS`, else `$XDG_CONFIG_HOME/murmur/credentials.json`,
//! else `~/.config/murmur/credentials.json`. Passwords come from stdin: at a terminal it
//! prompts twice without echo; otherwise it reads one line (for scripts and tests:
//! `printf '%s\n' "$PW" | murmur-auth add op-1 --engage`). A running bridge notices a change
//! within seconds: a removed operator's or agent's sessions end, and so do an operator's
//! after a new password or a change of authority. `--scripted` marks a credential a harness
//! holds (an evaluation's scripted operators, logged in by its runner): its logins are
//! recorded `auth: scripted`, authenticated but never a person's. Exit 0 on success, 1 on a refused change,
//! 2 on usage.

use std::io::{BufRead as _, IsTerminal as _};
use std::path::PathBuf;
use std::process::ExitCode;

use murmur_auth::{AuthError, CredentialFile, FileLock, default_path};

const USAGE: &str = "usage: murmur-auth [--file PATH] list | add <operator> [--engage] [--scripted] | passwd <operator> | engage <operator> on|off | scripted <operator> on|off | remove <operator> | agent-add <agent> | agent-rotate <agent> | agent-remove <agent> | passkeys <operator> | passkey-invite <operator> [--minutes N] [--url BASE] | passkey-remove <operator> <label|id prefix> | import <operator> --from FILE | path\n(passwords from stdin; see the module docs)";

fn read_password(id: &str) -> Result<String, AuthError> {
    let io = |e| AuthError::Io("stdin".into(), e);
    if std::io::stdin().is_terminal() {
        let a = rpassword::prompt_password(format!("password for {id}: ")).map_err(io)?;
        let b = rpassword::prompt_password("again: ").map_err(io)?;
        if a != b {
            return Err(AuthError::Invalid("the two passwords differ".into()));
        }
        return Ok(a);
    }
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).map_err(io)?;
    let pw = line.strip_suffix('\n').unwrap_or(&line);
    let pw = pw.strip_suffix('\r').unwrap_or(pw);
    if pw.is_empty() {
        return Err(AuthError::Invalid("no password on stdin".into()));
    }
    Ok(pw.to_string())
}

/// `2026-10-04 12:34Z` from Unix seconds (UTC, proleptic Gregorian).
fn utc(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // days from civil (H. Hinnant), inverted
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}Z", rem / 3600, (rem % 3600) / 60)
}

fn run(args: &[String]) -> Result<(), (u8, String)> {
    let usage = |m: &str| (2u8, format!("{m}\n{USAGE}"));
    let refused = |e: AuthError| (1u8, e.to_string());
    let mut it = args.iter().skip(1).peekable();
    let mut file: Option<PathBuf> = None;
    if it.peek().map(|a| a.as_str()) == Some("--file") {
        it.next();
        file = Some(PathBuf::from(it.next().ok_or_else(|| usage("--file needs a path"))?));
    }
    let cmd = it.next().ok_or_else(|| usage("no command"))?.clone();
    let rest: Vec<String> = it.cloned().collect();
    let path = file
        .or_else(default_path)
        .ok_or_else(|| usage("no credential file: set MURMUR_CREDENTIALS or HOME, or pass --file"))?;
    let one = |what: &str| -> Result<String, (u8, String)> {
        match rest.first() {
            Some(id) if !id.starts_with("--") => Ok(id.clone()),
            _ => Err(usage(&format!("{cmd} needs {what}"))),
        }
    };
    if cmd == "path" {
        println!("{}", path.display());
        return Ok(());
    }
    if cmd == "list" {
        let f = CredentialFile::load_or_new(&path).map_err(refused)?;
        println!("credentials: {}", path.display());
        if CredentialFile::readable_by_others(&path) {
            println!("warning: readable by other users; chmod 600 it");
        }
        println!("operators ({}):", f.operators.len());
        for (id, e) in &f.operators {
            let auth = match (e.engage, e.scripted) {
                (true, false) => "engagement authority",
                (false, false) => "operator",
                (true, true) => "engagement, scripted",
                (false, true) => "operator, scripted",
            };
            let keys = match e.passkeys.len() {
                0 => String::new(),
                1 => ", 1 passkey".to_string(),
                n => format!(", {n} passkeys"),
            };
            println!("  {id:<24} {auth:<22} changed {}{keys}", utc(e.updated_unix));
        }
        return Ok(());
    }
    if cmd == "passkeys" {
        let id = one("an operator id")?;
        let f = CredentialFile::load_or_new(&path).map_err(refused)?;
        let e = f.operators.get(&id).ok_or_else(|| (1u8, format!("no operator {id}")))?;
        println!("passkeys of {id} ({}):", e.passkeys.len());
        for p in &e.passkeys {
            let used = p.last_used_unix.map_or_else(|| "never used".to_string(), |t| format!("last used {}", utc(t)));
            let label = if p.label.is_empty() { "(no label)" } else { p.label.as_str() };
            println!(
                "  {label:<20} id {}...  counter {:<6} registered {}, {used}",
                p.id.chars().take(12).collect::<String>(),
                p.counter,
                utc(p.created_unix)
            );
        }
        if let Some(i) = &e.passkey_invite {
            println!("  an invitation to register one is open until {}", utc(i.expires_unix));
        }
        println!("agents ({}):", f.agents.len());
        for (id, e) in &f.agents {
            println!("  {id:<24} key mma_{}_...          changed {}", e.key_id, utc(e.updated_unix));
        }
        return Ok(());
    }
    let _lock = FileLock::acquire(&path).map_err(refused)?;
    let mut f = CredentialFile::load_or_new(&path).map_err(refused)?;
    let mut key = None;
    let what = match cmd.as_str() {
        "add" => {
            let id = one("an operator id")?;
            let (mut engage, mut scripted) = (false, false);
            for o in rest.iter().skip(1) {
                match o.as_str() {
                    "--engage" => engage = true,
                    "--scripted" => scripted = true,
                    o => return Err(usage(&format!("unknown option {o}"))),
                }
            }
            murmur_auth::validate_id(&id).map_err(|e| (1, e))?;
            if f.operators.contains_key(&id) {
                return Err((1, format!("operator {id} exists (passwd changes the password)")));
            }
            let pw = read_password(&id).map_err(refused)?;
            f.add_operator(&id, &pw, engage).map_err(refused)?;
            f.set_scripted(&id, scripted).map_err(refused)?;
            format!(
                "operator {id} added{}{}",
                if engage { " with engagement authority" } else { "" },
                if scripted { " (scripted: a harness-held credential, not a person)" } else { "" }
            )
        }
        "passwd" => {
            let id = one("an operator id")?;
            if !f.operators.contains_key(&id) {
                return Err((1, format!("no operator {id}")));
            }
            let pw = read_password(&id).map_err(refused)?;
            f.set_password(&id, &pw).map_err(refused)?;
            format!("password of {id} changed; their sessions end")
        }
        "engage" => {
            let id = one("an operator id")?;
            let on = match rest.get(1).map(String::as_str) {
                Some("on") => true,
                Some("off") => false,
                _ => return Err(usage("engage <operator> on|off")),
            };
            f.set_engage(&id, on).map_err(refused)?;
            format!(
                "{id}: engagement authority {}; their sessions end",
                if on { "granted" } else { "revoked" }
            )
        }
        "scripted" => {
            let id = one("an operator id")?;
            let on = match rest.get(1).map(String::as_str) {
                Some("on") => true,
                Some("off") => false,
                _ => return Err(usage("scripted <operator> on|off")),
            };
            f.set_scripted(&id, on).map_err(refused)?;
            format!(
                "{id}: {}; their sessions end",
                if on { "scripted (a harness-held credential, not a person)" } else { "a person's credential" }
            )
        }
        "remove" => {
            let id = one("an operator id")?;
            f.remove_operator(&id).map_err(refused)?;
            format!("operator {id} removed; their sessions end")
        }
        "agent-add" => {
            let id = one("an agent id")?;
            key = Some(f.add_agent(&id).map_err(refused)?);
            format!("agent {id} added")
        }
        "agent-rotate" => {
            let id = one("an agent id")?;
            key = Some(f.rotate_agent(&id).map_err(refused)?);
            format!("agent {id}: new key; the old one stops working")
        }
        "agent-remove" => {
            let id = one("an agent id")?;
            f.remove_agent(&id).map_err(refused)?;
            format!("agent {id} removed")
        }
        "passkey-invite" => {
            let id = one("an operator id")?;
            let mut minutes = 30u64;
            let web = std::env::var("MURMUR_WEB_PORT").ok().filter(|p| p.parse::<u16>().is_ok()).unwrap_or_else(|| "5173".into());
            let mut base = format!("http://localhost:{web}");
            let mut opts = rest.iter().skip(1);
            while let Some(o) = opts.next() {
                match o.as_str() {
                    "--minutes" => {
                        minutes = opts
                            .next()
                            .and_then(|m| m.parse::<u64>().ok())
                            .filter(|m| (1..=7 * 24 * 60).contains(m))
                            .ok_or_else(|| usage("--minutes takes 1 to 10080"))?;
                    }
                    "--url" => {
                        base = opts.next().ok_or_else(|| usage("--url takes the console's base URL"))?.trim_end_matches('/').to_string();
                    }
                    o => return Err(usage(&format!("unknown option {o}"))),
                }
            }
            let code = f.invite_passkey(&id, minutes).map_err(refused)?;
            key = Some(format!("{base}/#enroll={id}.{code}"));
            format!(
                "a one-time invitation for {id} to register a passkey, valid {minutes} min: open the link below in the browser that holds the authenticator (on localhost, not 127.0.0.1: WebAuthn takes no IP address); whoever opens it registers a passkey for {id}"
            )
        }
        "passkey-remove" => {
            let id = one("an operator id")?;
            let which = rest.get(1).ok_or_else(|| usage("passkey-remove <operator> <label | id prefix>"))?;
            let gone = f.remove_passkey(&id, which).map_err(refused)?;
            format!("{id}: passkey {}... removed; their sessions end", gone.chars().take(12).collect::<String>())
        }
        "import" => {
            let id = one("an operator id")?;
            let from = match (rest.get(1).map(String::as_str), rest.get(2)) {
                (Some("--from"), Some(p)) => PathBuf::from(p),
                _ => return Err(usage("import <operator> --from <credential file>")),
            };
            let other = CredentialFile::load(&from).map_err(refused)?;
            let entry = other
                .operators
                .get(&id)
                .cloned()
                .ok_or_else(|| (1u8, format!("no operator {id} in {}", from.display())))?;
            if f.agents.contains_key(&id) {
                return Err((1, format!("{id} is an agent here: an id is an operator or an agent, not both")));
            }
            let n = entry.passkeys.len();
            f.operators.insert(id.clone(), entry);
            format!("operator {id} imported from {} (password hash, flags, {n} passkey(s))", from.display())
        }
        other => return Err(usage(&format!("unknown command {other}"))),
    };
    f.save(&path).map_err(refused)?;
    eprintln!("{what} ({})", path.display());
    if let Some(k) = key {
        // the key (or the invitation link) alone on stdout, so a script can capture it; it is
        // shown only this once
        println!("{k}");
        if cmd == "passkey-invite" {
            eprintln!("the link above is shown once and works once");
        } else {
            eprintln!("the key above is shown once; give it to the agent as Authorization: Bearer <key>");
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, msg)) => {
            eprintln!("murmur-auth: {msg}");
            ExitCode::from(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::utc;

    #[test]
    fn dates() {
        assert_eq!(utc(0), "1970-01-01 00:00Z");
        assert_eq!(utc(1_790_000_000), "2026-09-21 14:13Z");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00Z");
    }
}
