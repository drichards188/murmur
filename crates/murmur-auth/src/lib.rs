//! Who may act in a Murmur run, and how they prove it.
//!
//! Murmur measures whether human judgment over consequential decisions stays meaningful when
//! one operator supervises a fleet. That claim needs every human decision bound to a person
//! who proved who they are, not to a name a request asserted. This crate is the credential
//! side of that binding; the bridge (`crates/murmur-bridge/src/auth.rs`) uses it to log
//! operators in and to check agents' keys, and the decision engine (`murmur-decide`) records
//! how each session was authenticated and refuses what needs a step-up without one.
//!
//! - **Operators** ([`store::CredentialFile`]): an id, an argon2id password hash
//!   ([`password`]), and whether they hold engagement authority. Managed with the
//!   `murmur-auth` CLI (`add`, `passwd`, `engage`, `remove`, `list`; the password from stdin,
//!   so scripts and tests can drive it).
//! - **Agents**: an id and the SHA-256 of a random API key ([`keys`]), printed once when it is
//!   issued (`murmur-auth agent-add`, `agent-rotate`).
//! - **Login sessions** ([`sessions::SessionTable`]): a random token in an HttpOnly,
//!   SameSite=Strict cookie, its digest in the bridge, an idle and an absolute expiry.
//! - **Failed attempts** ([`limit::Limiter`]): five failures for an id lock it, the lock
//!   doubling with each lockout.
//! - **Passkeys** (rules 10, [`passkeys`]): an operator's WebAuthn credentials, registered at
//!   the console (signed in, the password re-entered) or through a one-time invitation
//!   (`murmur-auth passkey-invite`); the file keeps each one's public key and counter,
//!   `murmur-auth passkeys` lists them and `passkey-remove` removes one.
//!
//! The file lives outside the repository ([`store::default_path`]) and holds no secret:
//! passwords are argon2id hashes, keys and invitation codes are digests, passkeys are public
//! keys; secrets are compared in constant time.

pub mod keys;
pub mod limit;
pub mod passkeys;
pub mod password;
pub mod sessions;
pub mod store;

pub use limit::Limiter;
pub use passkeys::RelyingParty;
pub use sessions::{Expiry, Lookup, SessionTable};
pub use store::{AgentEntry, CredentialFile, FileLock, Invite, OperatorEntry, PasskeyEntry, default_path, validate_id};

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
    #[error("credential file: {0}")]
    Format(String),
    #[error("{0}")]
    Invalid(String),
    #[error("password hashing: {0}")]
    Hash(String),
    #[error("random source: {0}")]
    Random(String),
}
