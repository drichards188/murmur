//! Passkeys (rules 10): the WebAuthn relying party, on kanidm's `webauthn-rs`.
//!
//! **Registration** ([`RelyingParty::start_registration`], [`RelyingParty::finish_registration`]):
//! a passkey with user verification required (a PIN, a fingerprint, the device's unlock),
//! resident keys preferred but not required (the operator names themselves first, so Murmur
//! never needs a discoverable credential), no attestation asked for, ES256 only (every current
//! platform authenticator and security key offers it, and replay verifies it with its own
//! code). The credential file keeps the result ([`crate::store::PasskeyEntry`]): the
//! credential id, its public key, its signature counter, and `webauthn-rs`'s record of it;
//! never private material, which stays in the authenticator.
//!
//! **Assertions** ([`RelyingParty::start_assertion`], [`RelyingParty::finish_assertion`]): for
//! a login or a step-up. The challenge is not `webauthn-rs`'s random one: the bridge derives it
//! from the run, the session and the act (`murmur_decide::passkey::challenge`) and puts it into
//! the ceremony's state, so the authenticator's signature binds the person to that one act.
//! `webauthn-rs` checks the origin, the relying party, user presence and verification, the
//! signature and the counter (a count that does not go up is refused: a cloned authenticator).
//!
//! **The relying party id** is a domain, `localhost` by default (`MURMUR_PASSKEY_RP_ID`):
//! WebAuthn does not accept an IP address as one, so the console at `http://127.0.0.1:<port>`
//! can't use a passkey and says to open `http://localhost:<port>` instead. A passkey registered
//! for `localhost` works on any port of it; the origins it may come from are the console's (and
//! the bridge's) on that host, listed explicitly.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use webauthn_rs::prelude::{
    COSEAlgorithm, COSEKeyType, ECDSACurve, Passkey, PasskeyAuthentication, PasskeyRegistration, PublicKeyCredential,
    RegisterPublicKeyCredential, Url, Uuid, Webauthn, WebauthnBuilder, WebauthnError,
};

use crate::AuthError;
use crate::store::PasskeyEntry;

fn invalid(e: impl std::fmt::Display) -> AuthError {
    AuthError::Invalid(e.to_string())
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The relying party: its id, the origins an assertion may come from, and `webauthn-rs`.
pub struct RelyingParty {
    pub rp_id: String,
    pub origins: Vec<String>,
    webauthn: Webauthn,
}

/// A registration in progress (kept by the bridge, never sent).
pub struct Registration {
    state: PasskeyRegistration,
}

/// An assertion in progress, for one derived challenge (kept by the bridge, never sent).
pub struct Pending {
    state: PasskeyAuthentication,
    challenge: [u8; 32],
}

impl Pending {
    pub fn challenge(&self) -> &[u8; 32] {
        &self.challenge
    }
}

/// What a checked assertion gives the audit log (nothing secret): the credential, its
/// counter, and the three parts of the assertion, base64url.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    pub credential: String,
    pub counter: u32,
    pub authenticator_data: String,
    pub client_data_json: String,
    pub signature: String,
}

/// Why an assertion was refused, for the bridge to say and count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The counter did not count up: the credential may have been cloned.
    Counter,
    /// Anything else (another challenge, origin or relying party, no user verification, a bad
    /// signature, an unknown credential).
    Invalid(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Counter => f.write_str("the authenticator's counter did not count up: it may have been cloned"),
            Refused::Invalid(why) => f.write_str(why),
        }
    }
}

/// One kind of credential id a decoy draws from: its length in bytes, whether it is a version
/// 4 UUID (its version and variant bits set, as a UUID-based password manager's ids are), and
/// its weight (percent).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecoyId {
    pub len: usize,
    pub uuid: bool,
    pub weight: u32,
}

/// The credential ids a decoy draws from: what the authenticators people sign in with make for
/// an ES256 passkey. 16 bytes: a version 4 UUID from the password managers that make one
/// (Bitwarden's are), or 16 random bytes from providers that don't (the split of that class,
/// 15 to 10, is a judgment); 20: iCloud Keychain; 32: Windows Hello, and browsers' virtual
/// authenticators; 64: a YubiKey 5; 65: an Android phone as a security key. A real operator's
/// ids are whatever their authenticator chose, so a decoy is told from them by neither its
/// length nor its count nor a UUID's fixed bits, short of knowing which authenticators a
/// site's people use (the structure inside a security key's longer ids is not imitated).
pub const DECOY_IDS: [DecoyId; 6] = [
    DecoyId { len: 16, uuid: true, weight: 15 },
    DecoyId { len: 16, uuid: false, weight: 10 },
    DecoyId { len: 20, uuid: false, weight: 25 },
    DecoyId { len: 32, uuid: false, weight: 25 },
    DecoyId { len: 64, uuid: false, weight: 15 },
    DecoyId { len: 65, uuid: false, weight: 10 },
];

/// How many passkeys a decoy names, with weights (percent): most people register one device,
/// some a second, few a third.
pub const DECOY_COUNTS: [(usize, u32); 3] = [(1, 70), (2, 25), (3, 5)];

/// The entry of a weighted table (weights summing to 100) that `x` falls on.
fn weighted<T: Copy>(table: &[T], weight: impl Fn(&T) -> u32, x: [u8; 32]) -> Option<T> {
    let mut r = u32::from_le_bytes([x[0], x[1], x[2], x[3]]) % 100;
    for v in table {
        let w = weight(v);
        if r < w {
            return Some(*v);
        }
        r -= w;
    }
    table.last().copied()
}

/// The made-up credential ids a decoy names for `operator` (murmur-i58): deterministic in
/// `secret` and the id (the same answer at every request, and across restarts with the same
/// secret, as a real operator's), unpredictable without the secret (HMAC-SHA-256 under it), as
/// many as [`DECOY_COUNTS`] draws and each of a kind [`DECOY_IDS`] draws.
pub fn decoy_credential_ids(secret: &[u8], operator: &str) -> Vec<Vec<u8>> {
    let op = operator.as_bytes();
    let len = u32::try_from(op.len()).unwrap_or(u32::MAX).to_le_bytes();
    let mac = |label: &[u8], i: u8, block: u8| crate::keys::hmac_sha256(secret, &[label, &len, op, &[i, block]]);
    let count = weighted(&DECOY_COUNTS, |c| c.1, mac(b"murmur decoy count\0", 0, 0)).map_or(1, |c| c.0);
    (0..count)
        .map(|i| {
            let i = u8::try_from(i).unwrap_or(u8::MAX);
            let kind = weighted(&DECOY_IDS, |k| k.weight, mac(b"murmur decoy length\0", i, 0)).unwrap_or(DECOY_IDS[2]);
            let mut id = Vec::with_capacity(kind.len + 32);
            let mut block = 0u8;
            while id.len() < kind.len {
                id.extend_from_slice(&mac(b"murmur decoy id\0", i, block));
                block = block.wrapping_add(1);
            }
            id.truncate(kind.len);
            if kind.uuid {
                // RFC 9562: version 4 in the high nibble of byte 6, variant 10 in byte 8
                id[6] = (id[6] & 0x0f) | 0x40;
                id[8] = (id[8] & 0x3f) | 0x80;
            }
            id
        })
        .collect()
}

/// Whether `id` has a version 4 UUID's fixed bits.
pub fn is_uuid_v4(id: &[u8]) -> bool {
    id.len() == 16 && id[6] >> 4 == 4 && id[8] >> 6 == 0b10
}

/// A stable WebAuthn user handle for an operator id (the authenticator stores it with a
/// resident key; it names no person).
pub fn user_handle(operator: &str) -> Uuid {
    let d: [u8; 32] = Sha256::digest(format!("murmur operator {operator}").as_bytes()).into();
    let mut b = [0u8; 16];
    b.copy_from_slice(&d[..16]);
    Uuid::from_bytes(b)
}

impl RelyingParty {
    /// A relying party `rp_id` (a domain) taking assertions from `origins` (each must be on
    /// `rp_id` or a subdomain of it).
    pub fn new(rp_id: &str, origins: &[String]) -> Result<RelyingParty, AuthError> {
        if rp_id.parse::<std::net::IpAddr>().is_ok() {
            return Err(invalid(format!("{rp_id}: a relying party id is a domain; WebAuthn takes no IP address")));
        }
        let urls: Vec<Url> = origins
            .iter()
            .map(|o| Url::parse(o).map_err(|e| invalid(format!("origin {o}: {e}"))))
            .collect::<Result<_, _>>()?;
        let first = urls
            .first()
            .ok_or_else(|| invalid(format!("no console origin on {rp_id}: passkeys need one (http://{rp_id}:<port>)")))?;
        let mut b = WebauthnBuilder::new(rp_id, first)
            .map_err(|e| invalid(format!("relying party {rp_id} for {first}: {e}")))?
            .rp_name("Murmur");
        for u in &urls[1..] {
            b = b.append_allowed_origin(u);
        }
        let webauthn = b.build().map_err(|e| invalid(format!("relying party {rp_id}: {e}")))?;
        Ok(RelyingParty {
            rp_id: rp_id.to_string(),
            origins: origins.to_vec(),
            webauthn,
        })
    }

    /// Start registering a passkey for `operator` (excluding the ones they have): the options
    /// for `navigator.credentials.create` (as JSON: `{"publicKey": {...}}`) and the state.
    pub fn start_registration(&self, operator: &str, existing: &[PasskeyEntry]) -> Result<(Value, Registration), AuthError> {
        let exclude: Vec<_> = existing.iter().filter_map(|p| p.passkey().ok()).map(|p| p.cred_id().clone()).collect();
        let (ccr, state) = self
            .webauthn
            .start_passkey_registration(user_handle(operator), operator, operator, Some(exclude))
            .map_err(|e| invalid(format!("passkey registration: {e}")))?;
        let mut v = serde_json::to_value(&ccr).map_err(invalid)?;
        // ES256 only (replay verifies it with its own code); a resident key preferred, never
        // required (Murmur names the operator first)
        v["publicKey"]["pubKeyCredParams"] = json!([{ "type": "public-key", "alg": -7 }]);
        if let Some(sel) = v["publicKey"].get_mut("authenticatorSelection").and_then(Value::as_object_mut) {
            sel.insert("residentKey".into(), json!("preferred"));
            sel.insert("requireResidentKey".into(), json!(false));
            sel.insert("userVerification".into(), json!("required"));
        }
        Ok((v, Registration { state }))
    }

    /// Finish a registration with the browser's response: the passkey to keep (ES256 on P-256
    /// only), labelled `label`.
    pub fn finish_registration(&self, reg: &Registration, response: &Value, label: &str, now_unix: u64) -> Result<PasskeyEntry, AuthError> {
        let rsp: RegisterPublicKeyCredential =
            serde_json::from_value(response.clone()).map_err(|e| invalid(format!("not a registration response: {e}")))?;
        let pk = self
            .webauthn
            .finish_passkey_registration(&rsp, &reg.state)
            .map_err(|e| invalid(format!("passkey registration refused: {e}")))?;
        let public_key = es256_key(&pk)?;
        let webauthn = serde_json::to_value(&pk).map_err(invalid)?;
        // the signature counter the authenticator reported at registration (webauthn-rs keeps
        // it in its record): the first use must count up from it
        let counter = webauthn["cred"]["counter"].as_u64().and_then(|c| u32::try_from(c).ok()).unwrap_or(0);
        Ok(PasskeyEntry {
            id: b64(pk.cred_id().as_ref()),
            label: label.trim().chars().take(64).collect(),
            public_key,
            counter,
            created_unix: now_unix,
            last_used_unix: None,
            webauthn,
        })
    }

    /// Start an assertion over `entries` (an operator's passkeys) for `challenge` (derived by
    /// the caller from the act): the options for `navigator.credentials.get` (`{"publicKey":
    /// {...}}`) and the state.
    pub fn start_assertion(&self, entries: &[PasskeyEntry], challenge: [u8; 32]) -> Result<(Value, Pending), AuthError> {
        let keys: Vec<Passkey> = entries.iter().map(PasskeyEntry::passkey).collect::<Result<_, _>>()?;
        if keys.is_empty() {
            return Err(invalid("no passkey registered"));
        }
        let (rcr, state) = self
            .webauthn
            .start_passkey_authentication(&keys)
            .map_err(|e| invalid(format!("passkey assertion: {e}")))?;
        // the challenge is the act's, not a random one: put it into the options and the state.
        // The options are written out in one shape (the decoy's): the challenge, the timeout,
        // the relying party, the credential ids (no transport hints), user verification
        let full = serde_json::to_value(&rcr).map_err(invalid)?;
        let ids: Vec<Value> = full["publicKey"]["allowCredentials"]
            .as_array()
            .map(|a| a.iter().map(|c| json!({ "type": "public-key", "id": c["id"] })).collect())
            .unwrap_or_default();
        let options = json!({
            "publicKey": {
                "challenge": b64(&challenge),
                "timeout": full["publicKey"]["timeout"],
                "rpId": self.rp_id,
                "allowCredentials": ids,
                "userVerification": "required",
            },
            "mediation": null,
        });
        let mut st = serde_json::to_value(&state).map_err(invalid)?;
        let slot = st
            .get_mut("ast")
            .and_then(|a| a.get_mut("challenge"))
            .ok_or_else(|| invalid("webauthn-rs state without its challenge (a library change: check the pin)"))?;
        *slot = json!(b64(&challenge));
        let state: PasskeyAuthentication = serde_json::from_value(st).map_err(invalid)?;
        Ok((options, Pending { state, challenge }))
    }

    /// Options shaped as [`RelyingParty::start_assertion`]'s for an operator who has no passkey
    /// (or no such operator), naming made-up credentials ([`decoy_credential_ids`]: as many,
    /// and as long, as real authenticators make, the same for an id at every request) that no
    /// authenticator holds, so the answer does not say which ids exist or have a passkey.
    pub fn decoy_options(&self, operator: &str, secret: &[u8], challenge: [u8; 32]) -> Value {
        let ids: Vec<Value> = decoy_credential_ids(secret, operator)
            .iter()
            .map(|id| json!({ "type": "public-key", "id": b64(id) }))
            .collect();
        json!({
            "publicKey": {
                "challenge": b64(&challenge),
                "timeout": 300_000,
                "rpId": self.rp_id,
                "allowCredentials": ids,
                "userVerification": "required",
            },
            "mediation": null,
        })
    }

    /// Check the browser's assertion (`PublicKeyCredential` JSON) against `pending` and the
    /// operator's passkeys as they are now (`entries`: its credential must be one of them, its
    /// counter must count up from the stored one). Ok with what the log records, and the
    /// updated entry to store.
    pub fn finish_assertion(&self, pending: &Pending, entries: &[PasskeyEntry], response: &Value) -> Result<(Checked, PasskeyEntry), Refused> {
        let rsp: PublicKeyCredential = serde_json::from_value(response.clone())
            .map_err(|e| Refused::Invalid(format!("not an assertion: {e}")))?;
        let id = b64(rsp.raw_id.as_ref());
        let Some(entry) = entries.iter().find(|p| p.id == id) else {
            return Err(Refused::Invalid("not one of your passkeys".into()));
        };
        let res = self.webauthn.finish_passkey_authentication(&rsp, &pending.state).map_err(|e| match e {
            WebauthnError::CredentialPossibleCompromise => Refused::Counter,
            other => Refused::Invalid(format!("passkey refused: {other}")),
        })?;
        if !res.user_verified() {
            return Err(Refused::Invalid("the authenticator did not verify you (user verification is required)".into()));
        }
        let counter = res.counter();
        // the stored counter is ours too: it must count up (or the authenticator never counts)
        if !((entry.counter == 0 && counter == 0) || counter > entry.counter) {
            return Err(Refused::Counter);
        }
        let mut updated = entry.clone();
        let mut pk = entry.passkey().map_err(|e| Refused::Invalid(e.to_string()))?;
        pk.update_credential(&res);
        updated.webauthn = serde_json::to_value(&pk).map_err(|e| Refused::Invalid(e.to_string()))?;
        updated.counter = counter;
        let checked = Checked {
            credential: id,
            counter,
            authenticator_data: b64(rsp.response.authenticator_data.as_ref()),
            client_data_json: b64(rsp.response.client_data_json.as_ref()),
            signature: b64(rsp.response.signature.as_ref()),
        };
        Ok((checked, updated))
    }
}

/// The SEC1 public key (base64url) of an ES256 P-256 passkey; refused otherwise.
fn es256_key(pk: &Passkey) -> Result<String, AuthError> {
    if *pk.cred_algorithm() != COSEAlgorithm::ES256 {
        return Err(invalid(format!(
            "this authenticator made a {:?} key: Murmur takes ES256 (ECDSA P-256) passkeys only",
            pk.cred_algorithm()
        )));
    }
    match &pk.get_public_key().key {
        COSEKeyType::EC_EC2(k) if k.curve == ECDSACurve::SECP256R1 => {
            let (x, y): (&[u8], &[u8]) = (k.x.as_ref(), k.y.as_ref());
            if x.len() != 32 || y.len() != 32 {
                return Err(invalid("a P-256 coordinate is 32 bytes"));
            }
            let mut p = Vec::with_capacity(65);
            p.push(0x04);
            p.extend_from_slice(x);
            p.extend_from_slice(y);
            Ok(b64(&p))
        }
        _ => Err(invalid("not a P-256 key")),
    }
}

impl PasskeyEntry {
    /// `webauthn-rs`'s record of the credential.
    pub fn passkey(&self) -> Result<Passkey, AuthError> {
        serde_json::from_value(self.webauthn.clone()).map_err(|e| AuthError::Format(format!("passkey {}: {e}", self.id)))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use std::collections::BTreeMap;

    use super::*;
    use murmur_decide::passkey::soft::SoftAuthenticator;

    const ORIGIN: &str = "http://localhost:5173";

    fn rp() -> RelyingParty {
        RelyingParty::new("localhost", &[ORIGIN.to_string(), "http://localhost:8765".to_string()]).unwrap()
    }

    fn registered(a: &mut SoftAuthenticator) -> PasskeyEntry {
        let rp = rp();
        let (options, reg) = rp.start_registration("op-2", &[]).unwrap();
        assert_eq!(options["publicKey"]["pubKeyCredParams"], json!([{ "type": "public-key", "alg": -7 }]));
        assert_eq!(options["publicKey"]["authenticatorSelection"]["userVerification"], "required");
        assert_eq!(options["publicKey"]["authenticatorSelection"]["residentKey"], "preferred");
        assert_eq!(options["publicKey"]["rp"]["id"], "localhost");
        let challenge = URL_SAFE_NO_PAD.decode(options["publicKey"]["challenge"].as_str().unwrap()).unwrap();
        let rsp = a.register("localhost", ORIGIN, &challenge);
        rp.finish_registration(&reg, &rsp, "laptop", 1).unwrap()
    }

    #[test]
    fn a_registration_keeps_the_public_key_only() {
        let mut a = SoftAuthenticator::new("op-2 laptop");
        let p = registered(&mut a);
        assert_eq!(p.id, a.credential_id());
        assert_eq!(p.public_key, a.public_key());
        assert_eq!(p.label, "laptop");
        // a response for another challenge, origin or relying party is refused
        let rp = rp();
        let (_, reg) = rp.start_registration("op-2", &[]).unwrap();
        let rsp = a.register("localhost", ORIGIN, &[9u8; 32]);
        assert!(rp.finish_registration(&reg, &rsp, "x", 1).is_err());
        let (options, reg) = rp.start_registration("op-2", &[]).unwrap();
        let c = URL_SAFE_NO_PAD.decode(options["publicKey"]["challenge"].as_str().unwrap()).unwrap();
        assert!(rp.finish_registration(&reg, &a.register("localhost", "http://evil.example", &c), "x", 1).is_err());
        assert!(rp.finish_registration(&reg, &a.register("example.org", ORIGIN, &c), "x", 1).is_err());
        // an authenticator that does not verify the user can't register
        let mut no_uv = SoftAuthenticator::new("no uv");
        no_uv.uv = false;
        let (options, reg) = rp.start_registration("op-2", &[]).unwrap();
        let c = URL_SAFE_NO_PAD.decode(options["publicKey"]["challenge"].as_str().unwrap()).unwrap();
        assert!(rp.finish_registration(&reg, &no_uv.register("localhost", ORIGIN, &c), "x", 1).is_err());
    }

    #[test]
    fn an_assertion_is_checked_on_the_derived_challenge() {
        let mut a = SoftAuthenticator::new("op-2 laptop");
        let p = registered(&mut a);
        let rp = rp();
        let challenge = [42u8; 32];
        let (options, pending) = rp.start_assertion(std::slice::from_ref(&p), challenge).unwrap();
        assert_eq!(options["publicKey"]["challenge"], b64(&challenge));
        assert_eq!(options["publicKey"]["userVerification"], "required");
        // signed over another challenge: refused
        let wrong = a.assertion_json("localhost", ORIGIN, &[41u8; 32]);
        assert!(rp.finish_assertion(&pending, std::slice::from_ref(&p), &wrong).is_err());
        let rsp = a.assertion_json("localhost", ORIGIN, &challenge);
        let (checked, updated) = rp.finish_assertion(&pending, std::slice::from_ref(&p), &rsp).unwrap();
        assert_eq!(checked.credential, p.id);
        assert!(checked.counter > p.counter);
        assert_eq!(updated.counter, checked.counter);
        // what the log keeps verifies with the decision layer's own code
        let key = murmur_decide::PasskeyKey {
            operator: murmur_decide::OperatorId::new("op-2"),
            id: p.id.clone(),
            public_key: p.public_key.clone(),
            counter: 0,
        };
        let logged = murmur_decide::Assertion {
            authenticator_data: checked.authenticator_data.clone(),
            client_data_json: checked.client_data_json.clone(),
            signature: checked.signature.clone(),
        };
        assert!(murmur_decide::passkey::verify(&key, "localhost", &[ORIGIN.to_string()], &challenge, &logged).is_ok());
        // a clone behind the original: its count does not go up from the stored one
        let mut clone = SoftAuthenticator::new("op-2 laptop");
        let (_, pending) = rp.start_assertion(std::slice::from_ref(&updated), challenge).unwrap();
        clone.counter = 0;
        let rsp = clone.assertion_json("localhost", ORIGIN, &challenge);
        assert_eq!(rp.finish_assertion(&pending, std::slice::from_ref(&updated), &rsp).unwrap_err(), Refused::Counter);
        // someone else's credential
        let mut other = SoftAuthenticator::new("op-3 phone");
        let rsp = other.assertion_json("localhost", ORIGIN, &challenge);
        assert!(rp.finish_assertion(&pending, std::slice::from_ref(&updated), &rsp).is_err());
    }

    #[test]
    fn an_ip_address_is_no_relying_party_and_decoys_look_alike() {
        assert!(RelyingParty::new("127.0.0.1", &["http://127.0.0.1:5173".to_string()]).is_err());
        assert!(RelyingParty::new("localhost", &[]).is_err());
        let rp = rp();
        let a = rp.decoy_options("nobody", b"secret", [1u8; 32]);
        let b = rp.decoy_options("op-2", b"secret", [1u8; 32]);
        assert_eq!(a["publicKey"]["challenge"], b["publicKey"]["challenge"]);
        assert_ne!(a["publicKey"]["allowCredentials"], b["publicKey"]["allowCredentials"]);
        assert_eq!(a, rp.decoy_options("nobody", b"secret", [1u8; 32]), "stable per id");
    }

    /// murmur-i58: a decoy names as many credentials, as long, as real authenticators make
    /// (it named one id of 16 bytes, while the virtual authenticator's are 32 and a security
    /// key's 64: the length told who had a passkey), the same for an id at every request,
    /// another under another secret.
    #[test]
    fn decoys_are_drawn_like_real_credentials_stable_per_id_and_keyed() {
        let secret = [7u8; 32];
        let n = 20_000;
        let mut lengths: BTreeMap<usize, usize> = BTreeMap::new();
        let mut counts: BTreeMap<usize, usize> = BTreeMap::new();
        let mut uuids = 0;
        for k in 0..n {
            let op = format!("op-{k}");
            let ids = decoy_credential_ids(&secret, &op);
            assert_eq!(ids, decoy_credential_ids(&secret, &op), "stable per id");
            *counts.entry(ids.len()).or_default() += 1;
            for id in &ids {
                *lengths.entry(id.len()).or_default() += 1;
                uuids += usize::from(is_uuid_v4(id));
            }
        }
        // every entry of the tables, near its weight (within 1.5 points of 100)
        let near = |seen: &BTreeMap<usize, usize>, table: &[(usize, u32)]| {
            let total: usize = seen.values().sum();
            assert_eq!(seen.len(), table.len(), "{seen:?}");
            for &(v, w) in table {
                let pct = 100.0 * seen[&v] as f64 / total as f64;
                assert!((pct - f64::from(w)).abs() < 1.5, "{v}: {pct:.1}% for {w}% ({seen:?})");
            }
        };
        let mut by_len: BTreeMap<usize, u32> = BTreeMap::new();
        for k in DECOY_IDS {
            *by_len.entry(k.len).or_default() += k.weight;
        }
        near(&lengths, &by_len.into_iter().collect::<Vec<_>>());
        near(&counts, &DECOY_COUNTS);
        // the review: a UUID-based manager's 16 bytes carry a version 4 UUID's fixed bits, and
        // so does the share of 16-byte decoys the table gives those managers (15 of 25, plus
        // the 1 in 64 of the random ones that happen to)
        let sixteen = lengths[&16] as f64;
        let share = 100.0 * uuids as f64 / sixteen;
        let want = 100.0 * (15.0 + 10.0 / 64.0) / 25.0;
        assert!((share - want).abs() < 3.0, "{share:.1}% of 16-byte decoys are UUIDs, want {want:.1}%");
        // keyed: another secret, other ids; and no two ids of one decoy alike
        assert_ne!(decoy_credential_ids(&secret, "op-2"), decoy_credential_ids(&[8u8; 32], "op-2"));
        for k in 0..200 {
            let ids = decoy_credential_ids(&secret, &format!("x{k}"));
            for (i, a) in ids.iter().enumerate() {
                assert!(ids[i + 1..].iter().all(|b| b != a));
            }
        }
        // the options carry them all, as a real operator's carry theirs
        let rp = rp();
        let o = rp.decoy_options("op-9", &secret, [1u8; 32]);
        let want: Vec<String> = decoy_credential_ids(&secret, "op-9").iter().map(|i| b64(i)).collect();
        let got: Vec<&str> = o["publicKey"]["allowCredentials"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect();
        assert_eq!(got, want);
    }
}
