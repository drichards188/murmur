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
}
