//! Operator passwords: argon2id with a random salt per password, stored as a PHC string
//! (`$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>`). The parameters are pinned here
//! (OWASP's argon2id recommendation: 19 MiB, 2 passes, 1 lane) rather than taken from the
//! crate's default, so a dependency update can't change them silently; verification reads
//! them from each stored string, so stronger parameters later still verify older hashes.
//! Verification compares in constant time (the `password-hash` crate's output comparison).

use std::sync::OnceLock;

use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::{Algorithm, Argon2, Params, Version};

use crate::AuthError;

/// Memory cost in KiB (19 MiB), passes, lanes.
pub const ARGON2_M_KIB: u32 = 19 * 1024;
pub const ARGON2_T: u32 = 2;
pub const ARGON2_P: u32 = 1;

/// The shortest password accepted. NIST SP 800-63B-4 (2025): a password used as a
/// single-factor authenticator is at least 15 characters; Murmur's is single-factor.
pub const MIN_PASSWORD_CHARS: usize = 15;
/// The longest (in bytes): hashing a megabyte for every login attempt is a cost, not security.
pub const MAX_PASSWORD_BYTES: usize = 1024;

fn hasher() -> Result<Argon2<'static>, AuthError> {
    let params = Params::new(ARGON2_M_KIB, ARGON2_T, ARGON2_P, None)
        .map_err(|e| AuthError::Hash(e.to_string()))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

/// Whether `password` may be set for operator `id`: long enough, not too long, not the id
/// itself, no control characters (a pasted newline would make it untypeable at a prompt).
pub fn check_policy(id: &str, password: &str) -> Result<(), String> {
    let chars = password.chars().count();
    if chars < MIN_PASSWORD_CHARS {
        return Err(format!(
            "a password needs at least {MIN_PASSWORD_CHARS} characters (it got {chars})"
        ));
    }
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(format!("a password may be at most {MAX_PASSWORD_BYTES} bytes"));
    }
    if password.chars().any(char::is_control) {
        return Err("a password may not contain control characters".into());
    }
    if password.trim().eq_ignore_ascii_case(id.trim()) {
        return Err("a password may not be the operator id".into());
    }
    Ok(())
}

/// The PHC string for `password`, with a fresh random salt.
pub fn hash(password: &str) -> Result<String, AuthError> {
    let h = hasher()?
        .hash_password(password.as_bytes())
        .map_err(|e| AuthError::Hash(e.to_string()))?;
    Ok(h.to_string())
}

/// Whether `password` matches the stored PHC string. A malformed string never matches.
pub fn verify(phc: &str, password: &str) -> bool {
    if password.len() > MAX_PASSWORD_BYTES {
        return false;
    }
    let Ok(parsed) = PasswordHash::new(phc) else {
        return false;
    };
    let Ok(a) = hasher() else { return false };
    a.verify_password(password.as_bytes(), &parsed).is_ok()
}

/// The same work as [`verify`] for an operator id that does not exist, so the time a login
/// takes does not say whether the id is known. Always false.
pub fn verify_dummy(password: &str) -> bool {
    static DUMMY: OnceLock<Option<String>> = OnceLock::new();
    let phc = DUMMY.get_or_init(|| {
        let mut secret = [0u8; 24];
        crate::keys::fill(&mut secret).ok()?;
        hash(&crate::keys::b64(&secret)).ok()
    });
    if let Some(phc) = phc {
        let _ = verify(phc, password);
    }
    false
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn hashes_verify_and_salts_differ() {
        let a = hash("correct horse battery staple").unwrap();
        let b = hash("correct horse battery staple").unwrap();
        assert!(a.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{a}");
        assert_ne!(a, b, "a fresh salt each time");
        assert!(verify(&a, "correct horse battery staple"));
        assert!(!verify(&a, "correct horse battery stapl"));
        assert!(!verify(&a, ""));
        assert!(!verify("not a phc string", "correct horse battery staple"));
        assert!(!verify_dummy("correct horse battery staple"));
    }

    #[test]
    fn the_policy_refuses_short_long_and_odd_passwords() {
        assert!(check_policy("op-1", "fifteen chars!!").is_ok());
        assert!(check_policy("op-1", "fourteen chars").is_err());
        assert!(check_policy("op-1", &"x".repeat(MAX_PASSWORD_BYTES + 1)).is_err());
        assert!(check_policy("op-1", "line one\nline two!").is_err());
        assert!(check_policy("a-long-operator-id", "a-long-operator-id").is_err());
    }
}
