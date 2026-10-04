//! The credential file: who may log in as an operator (with an argon2id password hash and
//! whether they hold engagement authority) and which agents may connect (with the SHA-256 of
//! their API key). JSON, one file, never in git: `MURMUR_CREDENTIALS`, else
//! `$XDG_CONFIG_HOME/murmur/credentials.json`, else `~/.config/murmur/credentials.json`
//! ([`default_path`]). Written atomically (a temporary file renamed over it) with mode 0600 in
//! a directory of mode 0700, under a lock file so two `murmur-auth` commands don't lose each
//! other's change. No secret is ever stored: passwords as PHC hashes, keys as digests.
//!
//! Every entry carries a `stamp`, a random value replaced on every change to it (a new
//! password, authority granted or revoked, a rotated key): a bridge that sees an entry's stamp
//! change, or the entry go, ends the sessions opened under the old one.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::AuthError;
use crate::keys::{self, ct_eq};
use crate::password;

/// The file format version (`murmur_credentials` in the file).
pub const FORMAT: u32 = 1;

/// An operator's credential.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorEntry {
    /// argon2id PHC string.
    pub password: String,
    /// Engagement authority: may sign an engagement (and accept a lost vehicle for one).
    #[serde(default)]
    pub engage: bool,
    pub stamp: String,
    /// When the entry last changed (Unix seconds).
    pub updated_unix: u64,
}

/// An agent's API key, as its id and digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentEntry {
    /// The key's public id (`mma_<key_id>_...`), shown in listings and the audit log.
    pub key_id: String,
    /// SHA-256 of the whole key, lowercase hex.
    pub key_sha256: String,
    pub stamp: String,
    pub updated_unix: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialFile {
    pub murmur_credentials: u32,
    #[serde(default)]
    pub operators: BTreeMap<String, OperatorEntry>,
    #[serde(default)]
    pub agents: BTreeMap<String, AgentEntry>,
}

impl Default for CredentialFile {
    fn default() -> Self {
        CredentialFile {
            murmur_credentials: FORMAT,
            operators: BTreeMap::new(),
            agents: BTreeMap::new(),
        }
    }
}

/// Where the credential file is when nothing says otherwise: `MURMUR_CREDENTIALS`, else
/// `$XDG_CONFIG_HOME/murmur/credentials.json`, else `$HOME/.config/murmur/credentials.json`.
pub fn default_path() -> Option<PathBuf> {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    if let Some(p) = var("MURMUR_CREDENTIALS") {
        return Some(PathBuf::from(p));
    }
    let base = var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| var("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("murmur").join("credentials.json"))
}

/// An operator or agent id: 1 to 64 characters, letters, digits, `.`, `_`, `-`, starting
/// with a letter or digit (it goes into rosters, cookies' owners and the audit log). Use a
/// role or an anonymous label (`op-1`, `p07`), never a person's name.
pub fn validate_id(id: &str) -> Result<(), String> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{id:?}: an id is 1 to 64 letters, digits, '.', '_' or '-', starting with a letter or digit"
        ))
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl CredentialFile {
    /// Read and check a credential file.
    pub fn load(path: &Path) -> Result<CredentialFile, AuthError> {
        let text = fs::read_to_string(path).map_err(|e| AuthError::Io(path.display().to_string(), e))?;
        let f: CredentialFile =
            serde_json::from_str(&text).map_err(|e| AuthError::Format(format!("{}: {e}", path.display())))?;
        if f.murmur_credentials != FORMAT {
            return Err(AuthError::Format(format!(
                "{}: credential file format {} (this build reads {FORMAT})",
                path.display(),
                f.murmur_credentials
            )));
        }
        for id in f.operators.keys().chain(f.agents.keys()) {
            validate_id(id).map_err(|e| AuthError::Format(format!("{}: {e}", path.display())))?;
        }
        Ok(f)
    }

    /// [`CredentialFile::load`], or an empty file when there is none yet.
    pub fn load_or_new(path: &Path) -> Result<CredentialFile, AuthError> {
        match fs::metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(CredentialFile::default()),
            _ => Self::load(path),
        }
    }

    /// Write atomically: a temporary file beside it (mode 0600), then a rename.
    pub fn save(&self, path: &Path) -> Result<(), AuthError> {
        let io = |e| AuthError::Io(path.display().to_string(), e);
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            create_private_dir(dir).map_err(io)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| AuthError::Format(e.to_string()))?;
        let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
        {
            let mut f = private_file(&tmp).map_err(io)?;
            f.write_all(text.as_bytes()).map_err(io)?;
            f.write_all(b"\n").map_err(io)?;
            f.sync_all().map_err(io)?;
        }
        fs::rename(&tmp, path).map_err(io)
    }

    /// Whether others than the owner can read the file (a warning, not an error: some
    /// filesystems don't keep Unix modes).
    pub fn readable_by_others(path: &Path) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o077 != 0)
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            false
        }
    }

    // ---- operators ------------------------------------------------------------------------

    pub fn add_operator(&mut self, id: &str, password: &str, engage: bool) -> Result<(), AuthError> {
        validate_id(id).map_err(AuthError::Invalid)?;
        if self.operators.contains_key(id) {
            return Err(AuthError::Invalid(format!("operator {id} exists (passwd changes the password)")));
        }
        if self.agents.contains_key(id) {
            return Err(AuthError::Invalid(format!("{id} is an agent: an id is an operator or an agent, not both")));
        }
        password::check_policy(id, password).map_err(AuthError::Invalid)?;
        self.operators.insert(
            id.to_string(),
            OperatorEntry {
                password: password::hash(password)?,
                engage,
                stamp: keys::stamp()?,
                updated_unix: now_unix(),
            },
        );
        Ok(())
    }

    pub fn set_password(&mut self, id: &str, password: &str) -> Result<(), AuthError> {
        password::check_policy(id, password).map_err(AuthError::Invalid)?;
        let hash = password::hash(password)?;
        let e = self.operator_mut(id)?;
        e.password = hash;
        e.stamp = keys::stamp()?;
        e.updated_unix = now_unix();
        Ok(())
    }

    pub fn set_engage(&mut self, id: &str, engage: bool) -> Result<(), AuthError> {
        let e = self.operator_mut(id)?;
        if e.engage != engage {
            e.engage = engage;
            e.stamp = keys::stamp()?;
            e.updated_unix = now_unix();
        }
        Ok(())
    }

    pub fn remove_operator(&mut self, id: &str) -> Result<(), AuthError> {
        self.operators
            .remove(id)
            .map(|_| ())
            .ok_or_else(|| AuthError::Invalid(format!("no operator {id}")))
    }

    fn operator_mut(&mut self, id: &str) -> Result<&mut OperatorEntry, AuthError> {
        self.operators
            .get_mut(id)
            .ok_or_else(|| AuthError::Invalid(format!("no operator {id}")))
    }

    /// The operator whose password this is, or None. An unknown id costs the same work as a
    /// known one (no timing oracle for which ids exist).
    pub fn verify_operator(&self, id: &str, password: &str) -> Option<&OperatorEntry> {
        match self.operators.get(id) {
            Some(e) if password::verify(&e.password, password) => Some(e),
            Some(_) => None,
            None => {
                password::verify_dummy(password);
                None
            }
        }
    }

    // ---- agents ---------------------------------------------------------------------------

    /// A new agent and its key (returned once; only its digest is kept).
    pub fn add_agent(&mut self, id: &str) -> Result<String, AuthError> {
        validate_id(id).map_err(AuthError::Invalid)?;
        if self.agents.contains_key(id) {
            return Err(AuthError::Invalid(format!("agent {id} exists (agent-rotate issues a new key)")));
        }
        if self.operators.contains_key(id) {
            return Err(AuthError::Invalid(format!("{id} is an operator: an id is an operator or an agent, not both")));
        }
        let (key, key_id, sha) = keys::new_agent_key()?;
        self.agents.insert(
            id.to_string(),
            AgentEntry {
                key_id,
                key_sha256: sha,
                stamp: keys::stamp()?,
                updated_unix: now_unix(),
            },
        );
        Ok(key)
    }

    /// A new key for an agent; the old one stops working.
    pub fn rotate_agent(&mut self, id: &str) -> Result<String, AuthError> {
        let (key, key_id, sha) = keys::new_agent_key()?;
        let stamp = keys::stamp()?;
        let e = self
            .agents
            .get_mut(id)
            .ok_or_else(|| AuthError::Invalid(format!("no agent {id}")))?;
        e.key_id = key_id;
        e.key_sha256 = sha;
        e.stamp = stamp;
        e.updated_unix = now_unix();
        Ok(key)
    }

    pub fn remove_agent(&mut self, id: &str) -> Result<(), AuthError> {
        self.agents
            .remove(id)
            .map(|_| ())
            .ok_or_else(|| AuthError::Invalid(format!("no agent {id}")))
    }

    /// The agent a key belongs to (by its key id, then its digest in constant time).
    pub fn agent_for_key(&self, key: &str) -> Option<(&str, &AgentEntry)> {
        let key_id = keys::agent_key_id(key)?;
        let want = keys::digest_hex(key);
        self.agents
            .iter()
            .find(|(_, e)| e.key_id == key_id)
            .filter(|(_, e)| ct_eq(e.key_sha256.as_bytes(), want.as_bytes()))
            .map(|(id, e)| (id.as_str(), e))
    }
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir)
    }
}

fn private_file(path: &Path) -> std::io::Result<fs::File> {
    let mut o = OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        o.mode(0o600);
    }
    o.open(path)
}

/// An exclusive lock on the credential file for one read-modify-write (`<file>.lock`,
/// created exclusively, removed on drop; one older than 30 s is a crashed writer's and is
/// taken over).
pub struct FileLock(PathBuf);

impl FileLock {
    pub fn acquire(path: &Path) -> Result<FileLock, AuthError> {
        let lock = path.with_extension("lock");
        if let Some(dir) = lock.parent().filter(|d| !d.as_os_str().is_empty()) {
            create_private_dir(dir).map_err(|e| AuthError::Io(dir.display().to_string(), e))?;
        }
        for _ in 0..100 {
            match OpenOptions::new().write(true).create_new(true).open(&lock) {
                Ok(_) => return Ok(FileLock(lock)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = fs::metadata(&lock)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > Duration::from_secs(30));
                    if stale {
                        let _ = fs::remove_file(&lock);
                    } else {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
                Err(e) => return Err(AuthError::Io(lock.display().to_string(), e)),
            }
        }
        Err(AuthError::Invalid(format!(
            "{} is held by another murmur-auth command",
            lock.display()
        )))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    pub(crate) fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "murmur-auth-test-{}-{name}-{}",
            std::process::id(),
            keys::stamp().unwrap()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    const PW: &str = "a long enough passphrase";

    #[test]
    fn operators_round_trip_and_verify_without_storing_the_password() {
        let dir = scratch("ops");
        let path = dir.join("sub/credentials.json");
        let mut f = CredentialFile::load_or_new(&path).unwrap();
        f.add_operator("op-1", PW, false).unwrap();
        f.add_operator("op-2", "another long passphrase", true).unwrap();
        assert!(f.add_operator("op-1", PW, false).is_err(), "no duplicate");
        assert!(f.add_operator("op 3", PW, false).is_err(), "no spaces in an id");
        assert!(f.add_operator("op-3", "short", false).is_err(), "too short");
        f.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains(PW), "the password is not in the file");
        assert!(text.contains("$argon2id$"));
        assert!(!CredentialFile::readable_by_others(&path));
        let g = CredentialFile::load(&path).unwrap();
        assert_eq!(f, g);
        assert!(g.verify_operator("op-1", PW).is_some());
        assert!(g.verify_operator("op-1", "a long enough passphrasE").is_none());
        assert!(g.verify_operator("op-9", PW).is_none());
        assert!(g.operators["op-2"].engage);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn every_change_renews_the_stamp() {
        let mut f = CredentialFile::default();
        f.add_operator("op-1", PW, false).unwrap();
        let s0 = f.operators["op-1"].stamp.clone();
        f.set_engage("op-1", true).unwrap();
        let s1 = f.operators["op-1"].stamp.clone();
        assert_ne!(s0, s1);
        f.set_engage("op-1", true).unwrap();
        assert_eq!(f.operators["op-1"].stamp, s1, "no change, same stamp");
        f.set_password("op-1", "a different passphrase").unwrap();
        assert_ne!(f.operators["op-1"].stamp, s1);
        assert!(f.verify_operator("op-1", PW).is_none());
        f.remove_operator("op-1").unwrap();
        assert!(f.remove_operator("op-1").is_err());
    }

    #[test]
    fn agent_keys_verify_by_digest_and_rotate() {
        let mut f = CredentialFile::default();
        let k = f.add_agent("agent-planner").unwrap();
        assert!(k.starts_with("mma_"));
        assert_eq!(f.agent_for_key(&k).map(|(id, _)| id), Some("agent-planner"));
        let json = serde_json::to_string(&f).unwrap();
        assert!(!json.contains(&k), "the key is not stored");
        // one character off
        let mut off = k.clone();
        let last = off.pop().unwrap();
        off.push(if last == 'A' { 'B' } else { 'A' });
        assert!(f.agent_for_key(&off).is_none());
        let k2 = f.rotate_agent("agent-planner").unwrap();
        assert!(f.agent_for_key(&k).is_none(), "the old key stops working");
        assert!(f.agent_for_key(&k2).is_some());
        assert!(f.add_operator("agent-planner", PW, false).is_err(), "an id is one or the other");
        f.remove_agent("agent-planner").unwrap();
        assert!(f.agent_for_key(&k2).is_none());
    }

    #[test]
    fn unknown_fields_and_formats_are_refused() {
        let dir = scratch("format");
        let p = dir.join("c.json");
        fs::write(&p, r#"{"murmur_credentials":1,"operators":{},"agents":{},"extra":1}"#).unwrap();
        assert!(CredentialFile::load(&p).is_err());
        fs::write(&p, r#"{"murmur_credentials":2}"#).unwrap();
        assert!(CredentialFile::load(&p).is_err());
        fs::write(&p, r#"{"murmur_credentials":1,"operators":{"bad id":{"password":"x","stamp":"s","updated_unix":0}}}"#).unwrap();
        assert!(CredentialFile::load(&p).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
