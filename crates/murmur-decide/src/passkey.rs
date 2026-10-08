//! Passkeys (rules 10, [`crate::rules::PASSKEY_SINCE`]): what the audit log records of a
//! WebAuthn assertion, and the checks the engine and replay run on it.
//!
//! A credentials run logs, in `run.started`, the relying party (`rp_id`, the console's origins)
//! and every operator's registered passkeys as public keys ([`PasskeyRoster`]; never private
//! material). A passkey registered while the run is up is logged as `passkey.registered`, one
//! removed as `passkey.removed`. Each use of a passkey (a login, a step-up) is logged with the
//! assertion the authenticator made ([`PasskeyUse`]: the authenticator data, the client data,
//! the signature; none of them secret), what it was for ([`StepUpFor`]), and the server's nonce
//! and issue time. The challenge the authenticator signed is not random: it is derived
//! ([`challenge`]) from the run (the hash of the log's first line), the session, the operator,
//! what the step-up is for (an engagement signature: the intent, the picture token quoted and
//! the SHA-256 of that token, which carries the plan), the nonce and the issue time. So the
//! signature itself binds the person's authenticator to that one act in that one run: replay
//! recomputes the challenge from the log, checks the client data carries it, and verifies the
//! ECDSA signature against the public key the run logged ([`verify`]), with this crate's own
//! code, not the bridge's WebAuthn library. A forger without the private key can't relabel a
//! password step-up as a passkey's, move an assertion to another act or another run, or claim
//! another operator's credential; one who also rewrites `run.started` is caught by the anchors
//! (the first line is anchored).
//!
//! Only ES256 (ECDSA P-256 with SHA-256, COSE algorithm -7) is accepted: every current
//! platform authenticator and security key offers it.
//!
//! The sign counter: an authenticator that counts must count up; a count that does not
//! (`counter_ok`) means the credential may have been cloned, and the use is refused. Synced
//! passkeys often report 0 every time, which says nothing either way.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ids::{IntentId, OperatorId, SessionId, Uid};
use crate::time::Time;

/// How long an issued challenge may be answered: the person finds their authenticator,
/// unlocks it and touches it. The step-up it makes is then good for
/// [`crate::STEP_UP_MAX_AGE_MS`] more.
pub const CHALLENGE_TTL_MS: i64 = 120_000;

/// The domain-separation tag hashed into every challenge.
pub const CHALLENGE_DOMAIN: &str = "murmur.step_up/1";

/// COSE algorithm ES256.
pub const ES256: i64 = -7;

/// The relying party and the passkeys a credentials run's operators had when it started
/// (`run.started`'s roster, rules 10).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasskeyRoster {
    /// The WebAuthn relying party id: a domain (`localhost` by default), never an IP
    /// address, which WebAuthn does not accept.
    pub rp_id: String,
    /// The console origins an assertion may come from (`http://localhost:<port>`).
    pub origins: Vec<String>,
    /// Every operator's passkeys, as public keys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<PasskeyKey>,
}

/// One registered passkey, public parts only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasskeyKey {
    pub operator: OperatorId,
    /// The credential id, base64url.
    pub id: String,
    /// The ES256 public key: the SEC1 uncompressed point (65 bytes), base64url.
    pub public_key: String,
    /// The authenticator's signature counter when the run started (or the key was
    /// registered).
    pub counter: u32,
}

/// A study run (rules 10): its participant (a person's credential, the only operator whose
/// acts are evidence) and, only by explicit choice, the override that lets the participant
/// step up with a password. A run with the override is never evidence about people.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Study {
    pub participant: OperatorId,
    /// The reason given for allowing password step-ups (`MURMUR_STUDY_PASSWORD_STEP_UP`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_step_up: Option<String>,
}

/// A lost vehicle an acceptance names (as `engage.accepted` does).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedVehicle {
    pub uid: Uid,
    #[serde(default)]
    pub since: Option<Time>,
}

/// What a passkey assertion authorizes: hashed into its challenge, logged with it, and
/// compared by the engine (and replay) with the act that quotes the step-up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "act", rename_all = "snake_case")]
pub enum StepUpFor {
    /// Opening a session (a passkey login).
    Login,
    /// A signature on an intent; for an engagement, the picture token it quotes and the
    /// SHA-256 of that token's serialization (the plan, the track's identity, the session it
    /// was shown in).
    Sign {
        intent: IntentId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        picture_sha256: Option<String>,
    },
    /// Accepting lost vehicles for an engagement: at its nomination (`intent` absent: it has
    /// no number yet) or while it waits.
    AcceptUnknown {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intent: Option<IntentId>,
        vehicles: Vec<AcceptedVehicle>,
    },
    /// Declaring a track hostile.
    DeclareHostile { track: String },
    /// Confirming the rule declaration in force on a track.
    ConfirmDeclaration { track: String, declaration: u64 },
    /// Rules 15: un-spending a platform spent by its own effect: the platform, the engagement
    /// that spent it and, for a mark carried over, the earlier log it is in.
    Unspend {
        uid: Uid,
        spent_by: IntentId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
}

impl StepUpFor {
    /// An acceptance of lost vehicles (`intent` None: with its nomination), the vehicles in
    /// order.
    pub fn accepting(intent: Option<IntentId>, vehicles: &[(Uid, Option<Time>)]) -> StepUpFor {
        let mut v: Vec<AcceptedVehicle> = vehicles
            .iter()
            .map(|(uid, since)| AcceptedVehicle { uid: uid.clone(), since: *since })
            .collect();
        v.sort();
        v.dedup();
        StepUpFor::AcceptUnknown { intent, vehicles: v }
    }

    /// A short description for messages.
    pub fn describe(&self) -> String {
        match self {
            StepUpFor::Login => "a login".into(),
            StepUpFor::Sign { intent, token: Some(t), .. } => format!("a signature on {intent} (picture token {t})"),
            StepUpFor::Sign { intent, .. } => format!("a signature on {intent}"),
            StepUpFor::AcceptUnknown { intent: Some(i), .. } => format!("an acceptance of lost vehicles for {i}"),
            StepUpFor::AcceptUnknown { .. } => "an acceptance of lost vehicles with a nomination".into(),
            StepUpFor::DeclareHostile { track } => format!("declaring {track} hostile"),
            StepUpFor::ConfirmDeclaration { track, declaration } => {
                format!("confirming declaration {declaration} of {track}")
            }
            StepUpFor::Unspend { uid, spent_by, .. } => format!("un-spending vehicle {uid} (spent by {spent_by})"),
        }
    }
}

/// The SHA-256 of a picture token's serialization (what a passkey signature on an engagement
/// commits to), lowercase hex.
pub fn picture_sha256(token: &crate::engage::PictureToken) -> String {
    crate::hash::sha256_hex(serde_json::to_string(token).unwrap_or_default().as_bytes())
}

/// A WebAuthn assertion as the browser returned it, base64url (no padding). Nothing in it is
/// secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assertion {
    pub authenticator_data: String,
    pub client_data_json: String,
    /// ASN.1 DER ECDSA signature over `authenticator_data || SHA-256(client_data_json)`.
    pub signature: String,
}

/// One use of a passkey, as logged (`passkey` on `session.opened` and `operator.action
/// step_up`, `auth: passkey`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasskeyUse {
    /// The credential id, base64url.
    pub credential: String,
    /// The signature counter the authenticator reported.
    pub counter: u32,
    /// What it authorizes.
    #[serde(rename = "for")]
    pub purpose: StepUpFor,
    /// The server's random nonce (16 bytes, base64url) and when the challenge was issued.
    pub nonce: String,
    pub issued_t: Time,
    pub assertion: Assertion,
}

/// A challenge the engine issued for a passkey use (not logged: the use logs its parts).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issued {
    pub purpose: StepUpFor,
    pub nonce: String,
    pub issued_t: Time,
    pub challenge: [u8; 32],
}

/// The challenge an authenticator signs for `purpose` (see the module docs): SHA-256 of the
/// canonical JSON of the run's first-line hash, the session (none for a login), the
/// operator, the purpose, the nonce and the issue time.
pub fn challenge(
    run: &str,
    session: Option<SessionId>,
    operator: &OperatorId,
    purpose: &StepUpFor,
    nonce: &str,
    issued_t: Time,
) -> [u8; 32] {
    #[derive(Serialize)]
    struct Input<'a> {
        murmur: &'static str,
        run: &'a str,
        session: Option<SessionId>,
        operator: &'a OperatorId,
        #[serde(rename = "for")]
        purpose: &'a StepUpFor,
        nonce: &'a str,
        issued_t: Time,
    }
    let text = serde_json::to_vec(&Input {
        murmur: CHALLENGE_DOMAIN,
        run,
        session,
        operator,
        purpose,
        nonce,
        issued_t,
    })
    .unwrap_or_default();
    Sha256::digest(&text).into()
}

/// base64url without padding (what WebAuthn's JSON uses).
pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Decode base64url (padding tolerated).
pub fn unb64(s: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .map_err(|e| format!("not base64url: {e}"))
}

/// The SEC1 uncompressed point of a P-256 key given its coordinates, base64url, if it is a
/// point on the curve.
pub fn sec1_from_xy(x: &[u8], y: &[u8]) -> Result<String, String> {
    if x.len() != 32 || y.len() != 32 {
        return Err("a P-256 coordinate is 32 bytes".into());
    }
    let mut p = Vec::with_capacity(65);
    p.push(0x04);
    p.extend_from_slice(x);
    p.extend_from_slice(y);
    check_key(&b64(&p))?;
    Ok(b64(&p))
}

/// Whether `public_key` (base64url SEC1) is a P-256 point.
pub fn check_key(public_key: &str) -> Result<(), String> {
    let bytes = unb64(public_key)?;
    p256::ecdsa::VerifyingKey::from_sec1_bytes(&bytes)
        .map(|_| ())
        .map_err(|_| "not a P-256 public key".into())
}

/// Whether a use's counter may follow the last one seen for its credential: an authenticator
/// that counts must count up (WebAuthn §6.1.1); one that never counts reports 0 each time.
pub fn counter_ok(last: u32, now: u32) -> bool {
    (last == 0 && now == 0) || now > last
}

/// What [`verify`] read from a valid assertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verified {
    pub counter: u32,
}

/// Verify a logged assertion (WebAuthn §7.2, the parts the log can show): the client data is
/// a `webauthn.get` for exactly `challenge`, from one of `origins`, not cross-origin; the
/// authenticator data is for `rp_id`, with the user present and verified; and the signature
/// over `authenticator_data || SHA-256(client_data_json)` verifies under `key`'s ES256 public
/// key. Err says why not.
pub fn verify(key: &PasskeyKey, rp_id: &str, origins: &[String], challenge: &[u8; 32], a: &Assertion) -> Result<Verified, String> {
    let ad = unb64(&a.authenticator_data).map_err(|e| format!("authenticator data {e}"))?;
    let cdj = unb64(&a.client_data_json).map_err(|e| format!("client data {e}"))?;
    let sig = unb64(&a.signature).map_err(|e| format!("signature {e}"))?;
    let pk = unb64(&key.public_key).map_err(|e| format!("public key {e}"))?;
    if ad.len() < 37 {
        return Err("authenticator data shorter than 37 bytes".into());
    }
    let rp_hash: [u8; 32] = Sha256::digest(rp_id.as_bytes()).into();
    if ad[..32] != rp_hash {
        return Err(format!("the authenticator data is for another relying party than {rp_id}"));
    }
    let flags = ad[32];
    if flags & 0x01 == 0 {
        return Err("the user was not present (UP flag clear)".into());
    }
    if flags & 0x04 == 0 {
        return Err("the user was not verified (UV flag clear): a passkey use needs user verification".into());
    }
    let counter = u32::from_be_bytes([ad[33], ad[34], ad[35], ad[36]]);
    let client: serde_json::Value =
        serde_json::from_slice(&cdj).map_err(|e| format!("client data is not JSON: {e}"))?;
    if client.get("type").and_then(|v| v.as_str()) != Some("webauthn.get") {
        return Err("client data type is not webauthn.get".into());
    }
    let want = b64(challenge);
    let got = client.get("challenge").and_then(|v| v.as_str()).unwrap_or("");
    if got.trim_end_matches('=') != want {
        return Err("the assertion signed another challenge than this act's".into());
    }
    let origin = client.get("origin").and_then(|v| v.as_str()).unwrap_or("");
    if !origins.iter().any(|o| o == origin) {
        return Err(format!("the assertion came from origin {origin:?}, not the console's"));
    }
    if client.get("crossOrigin").and_then(serde_json::Value::as_bool) == Some(true) {
        return Err("a cross-origin assertion".into());
    }
    let vk = p256::ecdsa::VerifyingKey::from_sec1_bytes(&pk).map_err(|_| "not a P-256 public key".to_string())?;
    let sig = p256::ecdsa::Signature::from_der(&sig).map_err(|_| "the signature is not DER ECDSA".to_string())?;
    let mut msg = ad.clone();
    msg.extend_from_slice(&Sha256::digest(&cdj));
    use p256::ecdsa::signature::Verifier as _;
    vk.verify(&msg, &sig.normalize_s())
        .map_err(|_| "the signature does not verify under the credential's public key".to_string())?;
    Ok(Verified { counter })
}

/// A software authenticator for tests (feature `soft-authenticator`): one ES256 credential
/// from a seed, a counter, and the browser's part (the client data JSON). It makes
/// registrations (a `none` attestation, as `navigator.credentials.create` returns them) and
/// assertions (as `navigator.credentials.get`), and can be told to misbehave (no user
/// verification, a counter set back) so tests can check each refusal.
#[cfg(feature = "soft-authenticator")]
pub mod soft {
    use super::{Assertion, b64};
    use p256::ecdsa::{DerSignature, SigningKey, signature::Signer as _};
    use sha2::{Digest, Sha256};

    pub struct SoftAuthenticator {
        key: SigningKey,
        pub id: Vec<u8>,
        pub counter: u32,
        /// Whether it verifies the user (sets UV).
        pub uv: bool,
        /// Whether it counts (false: reports 0, as synced passkeys do).
        pub counts: bool,
    }

    impl SoftAuthenticator {
        /// A credential whose key and id derive from `seed` (tests are reproducible).
        pub fn new(seed: &str) -> SoftAuthenticator {
            let mut n = 0u32;
            let key = loop {
                let d: [u8; 32] = Sha256::digest(format!("murmur soft authenticator {seed} {n}").as_bytes()).into();
                if let Ok(k) = SigningKey::from_slice(&d) {
                    break k;
                }
                n += 1;
            };
            let id: [u8; 32] = Sha256::digest(format!("murmur soft credential {seed}").as_bytes()).into();
            SoftAuthenticator {
                key,
                id: id[..16].to_vec(),
                counter: 0,
                uv: true,
                counts: true,
            }
        }

        pub fn credential_id(&self) -> String {
            b64(&self.id)
        }

        /// The SEC1 uncompressed public key, base64url.
        pub fn public_key(&self) -> String {
            b64(self.key.verifying_key().to_sec1_point(false).as_bytes())
        }

        fn xy(&self) -> (Vec<u8>, Vec<u8>) {
            let p = self.key.verifying_key().to_sec1_point(false);
            let b = p.as_bytes();
            (b[1..33].to_vec(), b[33..65].to_vec())
        }

        fn flags(&self) -> u8 {
            0x01 | if self.uv { 0x04 } else { 0 }
        }

        fn next_counter(&mut self) -> u32 {
            if self.counts {
                self.counter += 1;
                self.counter
            } else {
                0
            }
        }

        /// The COSE_Key of the public key (EC2, ES256, P-256).
        fn cose_key(&self) -> Vec<u8> {
            let (x, y) = self.xy();
            let mut k = vec![0xa5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21, 0x58, 0x20];
            k.extend_from_slice(&x);
            k.extend_from_slice(&[0x22, 0x58, 0x20]);
            k.extend_from_slice(&y);
            k
        }

        fn client_data(kind: &str, challenge: &[u8], origin: &str) -> Vec<u8> {
            serde_json::json!({
                "type": kind,
                "challenge": b64(challenge),
                "origin": origin,
                "crossOrigin": false,
            })
            .to_string()
            .into_bytes()
        }

        /// A registration response (`RegisterPublicKeyCredential` JSON) for `challenge`.
        pub fn register(&mut self, rp_id: &str, origin: &str, challenge: &[u8]) -> serde_json::Value {
            let rp: [u8; 32] = Sha256::digest(rp_id.as_bytes()).into();
            let mut ad = rp.to_vec();
            ad.push(self.flags() | 0x40);
            ad.extend_from_slice(&self.next_counter().to_be_bytes());
            ad.extend_from_slice(&[0u8; 16]);
            let len = u16::try_from(self.id.len()).unwrap_or(0);
            ad.extend_from_slice(&len.to_be_bytes());
            ad.extend_from_slice(&self.id);
            ad.extend_from_slice(&self.cose_key());
            // {"fmt": "none", "attStmt": {}, "authData": ad}
            let mut att = vec![0xa3, 0x63];
            att.extend_from_slice(b"fmt");
            att.push(0x64);
            att.extend_from_slice(b"none");
            att.push(0x67);
            att.extend_from_slice(b"attStmt");
            att.push(0xa0);
            att.push(0x68);
            att.extend_from_slice(b"authData");
            let n = ad.len();
            if n < 256 {
                att.extend_from_slice(&[0x58, n as u8]);
            } else {
                att.push(0x59);
                att.extend_from_slice(&u16::try_from(n).unwrap_or(0).to_be_bytes());
            }
            att.extend_from_slice(&ad);
            let cdj = Self::client_data("webauthn.create", challenge, origin);
            serde_json::json!({
                "id": self.credential_id(),
                "rawId": self.credential_id(),
                "type": "public-key",
                "response": {
                    "attestationObject": b64(&att),
                    "clientDataJSON": b64(&cdj),
                    "transports": ["internal"],
                },
                "extensions": {},
            })
        }

        /// An assertion for `challenge`, as the log records it.
        pub fn assert(&mut self, rp_id: &str, origin: &str, challenge: &[u8]) -> Assertion {
            let rp: [u8; 32] = Sha256::digest(rp_id.as_bytes()).into();
            let mut ad = rp.to_vec();
            ad.push(self.flags());
            ad.extend_from_slice(&self.next_counter().to_be_bytes());
            let cdj = Self::client_data("webauthn.get", challenge, origin);
            let mut msg = ad.clone();
            msg.extend_from_slice(&Sha256::digest(&cdj));
            let sig: DerSignature = self.key.sign(&msg);
            Assertion {
                authenticator_data: b64(&ad),
                client_data_json: b64(&cdj),
                signature: b64(sig.as_bytes()),
            }
        }

        /// An assertion as the browser returns it (`PublicKeyCredential` JSON).
        pub fn assertion_json(&mut self, rp_id: &str, origin: &str, challenge: &[u8]) -> serde_json::Value {
            let a = self.assert(rp_id, origin, challenge);
            serde_json::json!({
                "id": self.credential_id(),
                "rawId": self.credential_id(),
                "type": "public-key",
                "response": {
                    "authenticatorData": a.authenticator_data,
                    "clientDataJSON": a.client_data_json,
                    "signature": a.signature,
                    "userHandle": null,
                },
                "extensions": {},
            })
        }
    }
}

#[cfg(all(test, feature = "soft-authenticator"))]
mod tests {
    use super::soft::SoftAuthenticator;
    use super::*;

    const RP: &str = "localhost";
    const ORIGIN: &str = "http://localhost:5173";

    fn key(a: &SoftAuthenticator) -> PasskeyKey {
        PasskeyKey {
            operator: OperatorId::new("op-2"),
            id: a.credential_id(),
            public_key: a.public_key(),
            counter: 0,
        }
    }

    fn purpose() -> StepUpFor {
        StepUpFor::Sign {
            intent: IntentId(4),
            token: Some(7),
            picture_sha256: Some("ab".repeat(32)),
        }
    }

    #[test]
    fn an_assertion_verifies_for_its_own_challenge_only() {
        let mut a = SoftAuthenticator::new("op-2 laptop");
        let k = key(&a);
        let op = OperatorId::new("op-2");
        let c = challenge("runhash", Some(SessionId(3)), &op, &purpose(), "nonce", Time(1000));
        let s = a.assert(RP, ORIGIN, &c);
        let origins = vec![ORIGIN.to_string()];
        assert_eq!(verify(&k, RP, &origins, &c, &s), Ok(Verified { counter: 1 }));
        // the challenge commits to every part: another intent, token, session, run, nonce,
        // time or operator is another challenge, which this signature did not sign
        let others = [
            challenge("runhash", Some(SessionId(3)), &op, &StepUpFor::Sign { intent: IntentId(5), token: Some(7), picture_sha256: Some("ab".repeat(32)) }, "nonce", Time(1000)),
            challenge("runhash", Some(SessionId(3)), &op, &StepUpFor::Sign { intent: IntentId(4), token: Some(8), picture_sha256: Some("ab".repeat(32)) }, "nonce", Time(1000)),
            challenge("runhash", Some(SessionId(3)), &op, &StepUpFor::Sign { intent: IntentId(4), token: Some(7), picture_sha256: Some("cd".repeat(32)) }, "nonce", Time(1000)),
            challenge("runhash", Some(SessionId(4)), &op, &purpose(), "nonce", Time(1000)),
            challenge("otherrun", Some(SessionId(3)), &op, &purpose(), "nonce", Time(1000)),
            challenge("runhash", Some(SessionId(3)), &op, &purpose(), "nonce2", Time(1000)),
            challenge("runhash", Some(SessionId(3)), &op, &purpose(), "nonce", Time(1001)),
            challenge("runhash", Some(SessionId(3)), &OperatorId::new("op-3"), &purpose(), "nonce", Time(1000)),
        ];
        for o in others {
            assert_ne!(o, c);
            assert!(verify(&k, RP, &origins, &o, &s).is_err());
        }
        // another relying party, another origin, another key
        assert!(verify(&k, "example.org", &origins, &c, &s).is_err());
        assert!(verify(&k, RP, &["http://localhost:9999".to_string()], &c, &s).is_err());
        let b = SoftAuthenticator::new("op-3 phone");
        assert!(verify(&key(&b), RP, &origins, &c, &s).is_err());
        // a tampered authenticator data (the counter, say) breaks the signature
        let mut ad = unb64(&s.authenticator_data).expect("b64");
        ad[36] ^= 1;
        let forged = Assertion { authenticator_data: b64(&ad), ..s.clone() };
        assert!(verify(&k, RP, &origins, &c, &forged).is_err());
    }

    #[test]
    fn user_verification_is_required() {
        let mut a = SoftAuthenticator::new("no uv");
        a.uv = false;
        let k = key(&a);
        let c = [7u8; 32];
        let s = a.assert(RP, ORIGIN, &c);
        let e = verify(&k, RP, &[ORIGIN.to_string()], &c, &s).expect_err("refused");
        assert!(e.contains("not verified"), "{e}");
    }

    #[test]
    fn counters_count_up_or_never() {
        assert!(counter_ok(0, 0));
        assert!(counter_ok(0, 1));
        assert!(counter_ok(4, 5));
        assert!(!counter_ok(5, 5), "the same count twice: cloned");
        assert!(!counter_ok(5, 3));
        assert!(!counter_ok(5, 0), "a counting authenticator that reports 0");
    }

    #[test]
    fn keys_are_points_on_the_curve() {
        let a = SoftAuthenticator::new("k");
        assert!(check_key(&a.public_key()).is_ok());
        let mut bad = unb64(&a.public_key()).expect("b64");
        bad[64] ^= 1;
        assert!(check_key(&b64(&bad)).is_err());
        let p = unb64(&a.public_key()).expect("b64");
        assert_eq!(sec1_from_xy(&p[1..33], &p[33..65]), Ok(a.public_key()));
    }
}
