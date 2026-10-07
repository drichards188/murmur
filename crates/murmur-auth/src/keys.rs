//! Random secrets and their digests: login session tokens and agent API keys.
//!
//! Both are 256-bit random values from the operating system (`getrandom`). Neither is
//! stored: a session token lives only in the operator's cookie and the bridge keeps its
//! SHA-256; an agent key is printed once by `murmur-auth agent-add` and the credential file
//! keeps its SHA-256. A plain hash is enough for these (unlike passwords, which a person
//! chooses and an attacker can guess): there is nothing to guess in 256 random bits, and a
//! fast hash lets the bridge check a key on every MCP request. Digests are compared in
//! constant time.
//!
//! An agent key reads `mma_<key id>_<secret>`: the key id (8 hex characters, random, not
//! secret) names the key in the credential file and the audit log; the secret is 43
//! characters of URL-safe base64.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::AuthError;

/// The prefix every agent key carries (so a key pasted into the wrong place is recognisable).
pub const AGENT_KEY_PREFIX: &str = "mma_";

/// Fill `buf` from the operating system's random source.
pub fn fill(buf: &mut [u8]) -> Result<(), AuthError> {
    getrandom::fill(buf).map_err(|e| AuthError::Random(e.to_string()))
}

/// URL-safe base64 without padding.
pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// A fresh 256-bit secret as URL-safe base64 (43 characters): a session token.
pub fn token() -> Result<String, AuthError> {
    let mut b = [0u8; 32];
    fill(&mut b)?;
    Ok(b64(&b))
}

/// SHA-256 of a secret.
pub fn digest(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

/// SHA-256 of a secret, lowercase hex.
pub fn digest_hex(secret: &str) -> String {
    hex(&digest(secret))
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Constant-time equality of two byte strings (their lengths are not secret).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

/// A random stamp (16 hex characters) marking one version of a credential entry: sessions
/// opened under another stamp end when the entry changes.
pub fn stamp() -> Result<String, AuthError> {
    let mut b = [0u8; 8];
    fill(&mut b)?;
    Ok(hex(&b))
}

/// A new agent key: (the key, its id, the SHA-256 of the whole key in hex).
pub fn new_agent_key() -> Result<(String, String, String), AuthError> {
    let mut id = [0u8; 4];
    fill(&mut id)?;
    let key_id = hex(&id);
    let key = format!("{AGENT_KEY_PREFIX}{key_id}_{}", token()?);
    let sha = digest_hex(&key);
    Ok((key, key_id, sha))
}

/// HMAC-SHA-256 (RFC 2104) of the concatenated `parts` under `key`: a keyed function whose
/// outputs say nothing of the key or of each other (the passkey decoys are drawn from it).
pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let pad = |x: u8| k.map(|b| b ^ x);
    let mut inner = Sha256::new();
    inner.update(pad(0x36));
    for p in parts {
        inner.update(p);
    }
    let mut outer = Sha256::new();
    outer.update(pad(0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// A 256-bit secret kept in `path` (64 hex characters, mode 0600), made the first time and
/// read after: the server's own key for what must stay the same across restarts but can't be
/// guessed (the passkey decoys). Not a credential: losing it changes the decoys, no more.
///
/// Made whole before it has its name: written to a temporary file beside it, synced, then
/// hard-linked into place (which fails if the name exists: a second process making one at the
/// same time keeps the first's) and read back. So the name never holds an empty or partial
/// key, whatever crashes when. A file there that is not a key is an error, never replaced:
/// the caller says so (the bridge refuses to start), since a new key changes every decoy.
pub fn secret_file(path: &std::path::Path) -> Result<[u8; 32], AuthError> {
    let io = |e| AuthError::Io(path.display().to_string(), e);
    if std::fs::symlink_metadata(path).is_err() {
        let mut b = [0u8; 32];
        fill(&mut b)?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let tmp = path.with_file_name(format!(".{name}.{}.{}.tmp", std::process::id(), stamp()?));
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            o.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let written = o.open(&tmp).and_then(|mut f| {
            use std::io::Write as _;
            f.write_all(format!("{}\n", hex(&b)).as_bytes())?;
            f.sync_all()
        });
        let linked = written.and_then(|()| std::fs::hard_link(&tmp, path));
        let _ = std::fs::remove_file(&tmp);
        match linked {
            Ok(()) => {
                // the new name itself, durable
                #[cfg(unix)]
                if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                    let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
                }
            }
            // made by another process in between: its key is the one
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io(e)),
        }
    }
    let text = std::fs::read_to_string(path).map_err(io)?;
    let t = text.trim();
    if t.len() != 64 || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(AuthError::Format(format!(
            "{}: not a key (64 hex characters; {} bytes there)",
            path.display(),
            text.len()
        )));
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&t[2 * i..2 * i + 2], 16).map_err(|e| AuthError::Format(e.to_string()))?;
    }
    Ok(out)
}

/// The key id an agent key names, if it has the agent key's shape.
pub fn agent_key_id(key: &str) -> Option<&str> {
    let rest = key.strip_prefix(AGENT_KEY_PREFIX)?;
    let (id, secret) = rest.split_once('_')?;
    (id.len() == 8 && id.bytes().all(|b| b.is_ascii_hexdigit()) && secret.len() == 43).then_some(id)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn keys_have_their_shape_and_differ() {
        let (a, id, sha) = new_agent_key().unwrap();
        let (b, _, _) = new_agent_key().unwrap();
        assert_ne!(a, b);
        assert_eq!(agent_key_id(&a), Some(id.as_str()));
        assert_eq!(sha, digest_hex(&a));
        assert!(agent_key_id("mma_xyz_abc").is_none());
        assert!(agent_key_id(&a.replace("mma_", "mmb_")).is_none());
        let t = token().unwrap();
        assert_eq!(t.len(), 43);
        assert!(ct_eq(&digest(&t), &digest(&t)));
        assert!(!ct_eq(&digest(&t), &digest(&token().unwrap())));
        assert!(!ct_eq(b"ab", b"abc"));
    }

    /// RFC 4231 test cases 1 and 6 (a short key, a key longer than the block).
    #[test]
    fn hmac_sha256_matches_rfc_4231() {
        let mac = hmac_sha256(&[0x0b; 20], &[b"Hi There"]);
        assert_eq!(hex(&mac), "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
        let mac = hmac_sha256(&[0xaa; 131], &[b"Test Using Larger Than Block-Size Key", b" - Hash Key First"]);
        assert_eq!(hex(&mac), "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54");
    }

    #[test]
    fn a_secret_file_is_made_once_and_read_after() {
        let dir = std::env::temp_dir().join(format!("murmur-auth-secret-{}-{}", std::process::id(), stamp().unwrap()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("decoy.key");
        let a = secret_file(&p).unwrap();
        assert_eq!(secret_file(&p).unwrap(), a);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // nothing left beside it
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        // a file there that is not a key (empty, as a crash mid-write left one before the
        // review) is an error, never replaced
        std::fs::write(&p, "").unwrap();
        assert!(secret_file(&p).unwrap_err().to_string().contains("not a key"));
        std::fs::write(&p, "not hex").unwrap();
        assert!(secret_file(&p).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "not hex");
        // two processes making one at once: both read the one that won
        let q = dir.join("raced.key");
        let keys: Vec<[u8; 32]> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..8).map(|_| s.spawn(|| secret_file(&q).unwrap())).collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(keys.iter().all(|k| *k == keys[0]));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "no temporary file left");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
