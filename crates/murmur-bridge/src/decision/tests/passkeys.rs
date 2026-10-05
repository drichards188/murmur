//! Rules 10 through the bridge's whole HTTP surface (the router `main` serves): passkeys
//! registered at the console (signed in, the password re-entered) or by a one-time invitation;
//! a login by passkey; a step-up by passkey bound to the act it authorizes (an engagement
//! signature: its intent and picture token), refused for another act, when replayed, from a
//! cloned authenticator, or as a password where the policy (or a study) asks for a passkey.
//! The authenticator is the decision crate's software one; every log replays, and none holds
//! a secret.

use std::collections::BTreeMap;

use axum::http::{StatusCode, header};
use murmur_auth::{CredentialFile, RelyingParty};
use murmur_decide::passkey::soft::SoftAuthenticator;
use murmur_decide::{AuthMode, StepUpFactor};
use serde_json::{Value, json};

use super::auth::{CONSOLE, H, app, scratch};
use super::engagement::engage_rig_passkeys;
use super::*;
use crate::auth::{Auth, Settings as AuthSettings, default_origins, passkey_origins};

const PW1: &str = "op one has a long passphrase";
const PW2: &str = "op two has a long passphrase";
const PW3: &str = "op three has a long passphrase";
/// The console as a browser that can use passkeys sees it (WebAuthn takes no IP address).
const LOCAL: &str = "http://localhost:5173";

fn origins() -> Vec<String> {
    passkey_origins("localhost", &default_origins(5173, 8765))
}

/// A credentials run on the engagement rig whose operators in `with_keys` registered a passkey
/// before it started (each one the software authenticator seeded with its id), the policy
/// changed by `edit`, possibly a study.
fn harness(with_keys: &[&str], edit: impl FnOnce(&mut Policy), study: Option<murmur_decide::Study>) -> (H, BTreeMap<String, SoftAuthenticator>) {
    let file = scratch().join("credentials.json");
    let mut c = CredentialFile::default();
    c.add_operator("op-1", PW1, false).expect("op-1");
    c.add_operator("op-2", PW2, true).expect("op-2");
    c.add_operator("op-3", PW3, true).expect("op-3");
    let key = c.add_agent("agent-planner").expect("agent");
    let rp = RelyingParty::new("localhost", &origins()).expect("relying party");
    let mut keys = BTreeMap::new();
    for op in with_keys {
        let mut a = SoftAuthenticator::new(op);
        let (options, reg) = rp.start_registration(op, &[]).expect("options");
        let ch = murmur_decide::passkey::unb64(options["publicKey"]["challenge"].as_str().expect("challenge")).expect("b64");
        let entry = rp.finish_registration(&reg, &a.register("localhost", LOCAL, &ch), "laptop", 1).expect("registered");
        c.add_passkey(op, entry).expect("kept");
        keys.insert((*op).to_string(), a);
    }
    c.save(&file).expect("saved");
    let roster = murmur_decide::PasskeyRoster {
        rp_id: "localhost".into(),
        origins: origins(),
        keys: c
            .operators
            .iter()
            .flat_map(|(id, e)| {
                e.passkeys.iter().map(move |p| murmur_decide::PasskeyKey {
                    operator: OperatorId::new(id),
                    id: p.id.clone(),
                    public_key: p.public_key.clone(),
                    counter: p.counter,
                })
            })
            .collect(),
    };
    let e = engage_rig_passkeys(None, PathBuf::from("memory"), AuthMode::Credentials, edit, Some(roster), study.clone());
    let settings = AuthSettings {
        mode: AuthMode::Credentials,
        file: Some(file.clone()),
        cookie: "murmur_session_test".into(),
        secure_cookie: false,
        idle: Duration::from_secs(1800),
        max: Duration::from_secs(8 * 3600),
        origins: default_origins(5173, 8765),
        passkey_rp: "localhost".into(),
        passkey_origins: origins(),
        study,
    };
    let auth = Auth::new(settings, e.r.d.clone(), c);
    let app = app(&e, auth.clone());
    (H { e, app, auth, file, key }, keys)
}

fn passkey_policy(p: &mut Policy) {
    p.approval.step_up_factor = Some(StepUpFactor::Passkey);
}

/// The challenge in `navigator.credentials.get` options.
fn challenge(options: &Value) -> Vec<u8> {
    murmur_decide::passkey::unb64(options["publicKey"]["challenge"].as_str().expect("a challenge")).expect("b64")
}

impl H {
    /// A request from the console on localhost (where a passkey can be used).
    async fn local(&self, cookie: Option<&str>, path: &str, body: Value) -> (StatusCode, Value) {
        let mut hs = vec![("origin", LOCAL)];
        if let Some(c) = cookie {
            hs.push(("cookie", c));
        }
        let (s, _, v) = self.call("POST", path, Some(body), &hs).await;
        (s, v)
    }

    /// The step-up options for `act`, by whoever's cookie this is.
    async fn step_options(&self, cookie: &str, act: Value) -> (StatusCode, Value) {
        self.local(Some(cookie), "/api/auth/step_up", act).await
    }

    /// Look, ask for a passkey challenge for that very signature, sign it with `a`, and
    /// approve with the assertion.
    async fn sign_with(&self, cookie: &str, id: u64, a: &mut SoftAuthenticator) -> (StatusCode, Value) {
        let (s, t) = self.post(cookie, &format!("/api/intents/{id}/picture"), json!({})).await;
        assert_eq!(s, StatusCode::OK, "{t}");
        let token = t["id"].as_u64().expect("token");
        let (s, o) = self.step_options(cookie, json!({ "act": "sign", "intent": id, "token": token })).await;
        assert_eq!(s, StatusCode::OK, "{o}");
        let assertion = a.assertion_json("localhost", LOCAL, &challenge(&o));
        self.local(Some(cookie), &format!("/api/intents/{id}/approve"), json!({ "token": token, "passkey": assertion })).await
    }

    /// Sign in by passkey: the login options, the assertion, the login. The cookie, or the
    /// refusal.
    async fn passkey_login(&self, op: &str, a: &mut SoftAuthenticator) -> Result<String, (StatusCode, Value)> {
        let (s, o) = self.local(None, "/api/auth/login/passkey", json!({ "operator": op })).await;
        assert_eq!(s, StatusCode::OK, "{o}");
        let assertion = a.assertion_json("localhost", LOCAL, &challenge(&o));
        let (s, h, v) = self
            .call("POST", "/api/auth/login", Some(json!({ "operator": op, "passkey": assertion })), &[("origin", LOCAL)])
            .await;
        if s != StatusCode::OK {
            return Err((s, v));
        }
        let set = h.get(header::SET_COOKIE).and_then(|c| c.to_str().ok()).expect("a cookie");
        Ok(set.split(';').next().expect("name=value").to_string())
    }
}

#[tokio::test]
async fn two_people_sign_an_engagement_with_their_passkeys() {
    let (h, mut keys) = harness(&["op-2", "op-3"], passkey_policy, None);
    let op1 = h.login("op-1", PW1).await;
    // op-2 signs in by passkey, op-3 by password: both are people
    let mut k2 = keys.remove("op-2").expect("op-2's");
    let mut k3 = keys.remove("op-3").expect("op-3's");
    let op2 = h.passkey_login("op-2", &mut k2).await.expect("signed in");
    let op3 = h.login("op-3", PW3).await;
    let (_, _, me) = h.call("GET", "/api/auth/session", None, &[("cookie", &op2)]).await;
    assert_eq!((me["step_up_factor"].clone(), me["passkey"]["registered"].clone()), (json!("passkey"), json!(1)), "{me}");
    assert_eq!(me["passkey"]["rp_id"], "localhost");
    h.picture(&op1).await;
    let id = h.nominate(&op1).await;
    // the password is not enough where the policy asks for a passkey (and is not checked)
    let (s, v) = h.sign(&op2, id, Some(PW2)).await;
    assert_eq!((s, v["factor"].clone()), (StatusCode::UNAUTHORIZED, json!("passkey")), "{v}");
    // nothing at all: the console is told to use the passkey
    let (s, v) = h.sign(&op2, id, None).await;
    assert_eq!((s, v["step_up"].clone(), v["factor"].clone()), (StatusCode::UNAUTHORIZED, json!(true), json!("passkey")), "{v}");
    // with the passkey: signature 1 of 2
    let (s, v) = h.sign_with(&op2, id, &mut k2).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["intent"]["state"], "pending_approval");
    // op-3's with their own: approved and dispatched
    let (s, v) = h.sign_with(&op3, id, &mut k3).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["intent"]["state"], "dispatched", "{v}");
    let r = h.replays_authenticated();
    assert_eq!((r.passkey_logins, r.passkey_step_ups, r.stepped_signatures), (1, 2, 2));
    let log = h.log();
    assert!(log.contains("\"action\":\"step_up\",\"detail\":\"passkey for a signature on"), "{log}");
    assert!(log.contains(&format!("\"step_up.refused\",\"detail\":\"i{id}: a password where the policy asks for a passkey\"")), "{log}");
    h.no_secret_logged(&[&op1, &op2, &op3]);
    // the counters were kept in the credential file: op-2's authenticator counted its
    // registration (1), the login (2), the signature (3)
    let f = CredentialFile::load(&h.file).expect("file");
    assert_eq!(f.operators["op-2"].passkeys[0].counter, 3);
    assert!(f.operators["op-2"].passkeys[0].last_used_unix.is_some());
}

#[tokio::test]
async fn an_assertion_is_for_its_act_once_and_from_the_right_authenticator() {
    let (h, mut keys) = harness(&["op-2", "op-3"], |_| {}, None);
    let op1 = h.login("op-1", PW1).await;
    let op2 = h.login("op-2", PW2).await;
    let op3 = h.login("op-3", PW3).await;
    let mut k2 = keys.remove("op-2").expect("op-2's");
    h.picture(&op1).await;
    let id = h.nominate(&op1).await;
    // a challenge for the picture op-2 was shown first, used on a newer picture: the engine
    // compares the act and refuses (the assertion was made for another token)
    let (_, t1) = h.post(&op2, &format!("/api/intents/{id}/picture"), json!({})).await;
    let (s, o) = h.step_options(&op2, json!({ "act": "sign", "intent": id, "token": t1["id"] })).await;
    assert_eq!(s, StatusCode::OK, "{o}");
    let a1 = k2.assertion_json("localhost", LOCAL, &challenge(&o));
    let (_, t2) = h.post(&op2, &format!("/api/intents/{id}/picture"), json!({})).await;
    let (s, v) = h.local(Some(&op2), &format!("/api/intents/{id}/approve"), json!({ "token": t2["id"], "passkey": a1 })).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    assert!(v["detail"].as_str().unwrap_or("").contains(&format!("was made for a signature on i{id} (picture token")), "{v}");
    // the same assertion again (replayed): its challenge was answered
    let (s, v) = h.local(Some(&op2), &format!("/api/intents/{id}/approve"), json!({ "token": t1["id"], "passkey": a1 })).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    assert!(v["detail"].as_str().unwrap_or("").contains("no such challenge is waiting"), "{v}");
    // a challenge issued to op-2, answered through op-3's session: not theirs
    let (_, t) = h.post(&op2, &format!("/api/intents/{id}/picture"), json!({})).await;
    let (_, o) = h.step_options(&op2, json!({ "act": "sign", "intent": id, "token": t["id"] })).await;
    let a = k2.assertion_json("localhost", LOCAL, &challenge(&o));
    let (_, t3) = h.post(&op3, &format!("/api/intents/{id}/picture"), json!({})).await;
    let (s, v) = h.local(Some(&op3), &format!("/api/intents/{id}/approve"), json!({ "token": t3["id"], "passkey": a })).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    // a cloned authenticator: its counter does not count up from the one stored
    let (_, t) = h.post(&op2, &format!("/api/intents/{id}/picture"), json!({})).await;
    let (_, o) = h.step_options(&op2, json!({ "act": "sign", "intent": id, "token": t["id"] })).await;
    let mut clone = SoftAuthenticator::new("op-2");
    let a = clone.assertion_json("localhost", LOCAL, &challenge(&o));
    let (s, v) = h.local(Some(&op2), &format!("/api/intents/{id}/approve"), json!({ "token": t["id"], "passkey": a })).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    assert!(v["detail"].as_str().unwrap_or("").contains("cloned"), "{v}");
    // an assertion for another relying party or origin
    let (_, t) = h.post(&op2, &format!("/api/intents/{id}/picture"), json!({})).await;
    let (_, o) = h.step_options(&op2, json!({ "act": "sign", "intent": id, "token": t["id"] })).await;
    let a = k2.assertion_json("localhost", "http://evil.example", &challenge(&o));
    let (s, _) = h.local(Some(&op2), &format!("/api/intents/{id}/approve"), json!({ "token": t["id"], "passkey": a })).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // every refusal is on the record under the refused signer's session
    let log = h.log();
    assert!(log.contains("passkey refused: no such challenge is waiting"), "{log}");
    assert!(log.contains("passkey refused: the challenge was issued to another operator or session"), "{log}");
    assert!(log.contains("passkey refused: the authenticator's counter did not count up"), "{log}");
    assert!(log.contains(&format!("was made for a signature on i{id} (picture token")), "{log}");
    // and then a good one signs
    let (s, v) = h.sign_with(&op2, id, &mut k2).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    h.replays_authenticated();
}

#[tokio::test]
async fn a_study_takes_its_participants_passkey_never_a_password() {
    let study = murmur_decide::Study {
        participant: OperatorId::new("op-2"),
        password_step_up: None,
    };
    let (h, mut keys) = harness(&["op-2"], passkey_policy, Some(study));
    let op1 = h.login("op-1", PW1).await;
    let op2 = h.login("op-2", PW2).await;
    let op3 = h.login("op-3", PW3).await;
    h.picture(&op1).await;
    let id = h.nominate(&op1).await;
    let (s, v) = h.sign(&op2, id, Some(PW2)).await;
    assert_eq!((s, v["factor"].clone()), (StatusCode::UNAUTHORIZED, json!("passkey")), "{v}");
    let mut k2 = keys.remove("op-2").expect("op-2's");
    let (s, v) = h.sign_with(&op2, id, &mut k2).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // op-3 has no passkey: no options, and a clear word why
    let (_, t) = h.post(&op3, &format!("/api/intents/{id}/picture"), json!({})).await;
    let (s, v) = h.step_options(&op3, json!({ "act": "sign", "intent": id, "token": t["id"] })).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(v["detail"].as_str().unwrap_or("").contains("register one from your operator menu"), "{v}");
    let r = h.replays_authenticated();
    assert!(r.study_evidence(), "{:?}", r.study);
    let first = h.log().lines().next().unwrap_or_default().to_string();
    assert!(first.contains("\"study\":{\"participant\":\"op-2\"}"), "{first}");
}

#[test]
fn a_study_without_its_participants_passkey_does_not_start() {
    let study = murmur_decide::Study {
        participant: OperatorId::new("op-2"),
        password_step_up: None,
    };
    let roster = murmur_decide::PasskeyRoster { rp_id: "localhost".into(), origins: origins(), keys: Vec::new() };
    let started = std::panic::catch_unwind(|| {
        engage_rig_passkeys(None, PathBuf::from("memory"), AuthMode::Credentials, passkey_policy, Some(roster), Some(study))
    });
    assert!(started.is_err(), "the engine refuses the roster (the rig expects it to start)");
}

#[tokio::test]
async fn a_passkey_is_registered_from_a_session_with_the_password_or_by_invitation() {
    let (h, _) = harness(&[], |_| {}, None);
    let op1 = h.login("op-1", PW1).await;
    let mut k1 = SoftAuthenticator::new("op-1 new laptop");
    // the options, then the finish without the password, or a wrong one: refused
    let (s, o) = h.local(Some(&op1), "/api/auth/passkeys/register/options", json!({})).await;
    assert_eq!(s, StatusCode::OK, "{o}");
    assert_eq!(o["publicKey"]["authenticatorSelection"]["userVerification"], "required");
    let reg = k1.register("localhost", LOCAL, &challenge(&o));
    let (s, v) = h.local(Some(&op1), "/api/auth/passkeys/register", json!({ "credential": reg, "label": "laptop" })).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    let (s, o) = h.local(Some(&op1), "/api/auth/passkeys/register/options", json!({})).await;
    assert_eq!(s, StatusCode::OK);
    let reg = k1.register("localhost", LOCAL, &challenge(&o));
    let (s, _) = h
        .local(Some(&op1), "/api/auth/passkeys/register", json!({ "credential": reg, "label": "laptop", "password": "not op one's password at all" }))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // with it: registered, logged quoting the step-up, kept in the file (public key only)
    let (s, o) = h.local(Some(&op1), "/api/auth/passkeys/register/options", json!({})).await;
    assert_eq!(s, StatusCode::OK);
    let reg = k1.register("localhost", LOCAL, &challenge(&o));
    let (s, v) = h
        .local(Some(&op1), "/api/auth/passkeys/register", json!({ "credential": reg, "label": "laptop", "password": PW1 }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["passkey"]["registered"], 1, "{v}");
    let f = CredentialFile::load(&h.file).expect("file");
    assert_eq!(f.operators["op-1"].passkeys[0].public_key, k1.public_key());
    let log = h.log();
    assert!(log.contains("\"type\":\"passkey.registered\""), "{log}");
    assert!(log.contains("\"via\":\"session\",\"session\":1,\"step_up\":{\"seq\":"), "{log}");
    // and op-1 signs in with it at once
    let c = h.passkey_login("op-1", &mut k1).await.expect("signed in by the new passkey");
    let (_, _, me) = h.call("GET", "/api/auth/passkeys", None, &[("cookie", &c)]).await;
    assert_eq!(me["passkeys"][0]["label"], "laptop", "{me}");
    // an invitation for op-3: a wrong code is refused, the right one registers, once
    let mut f = CredentialFile::load(&h.file).expect("file");
    let code = f.invite_passkey("op-3", 5).expect("invited");
    f.save(&h.file).expect("saved");
    let (s, _) = h.local(None, "/api/auth/passkeys/enroll/options", json!({ "operator": "op-3", "code": "guess" })).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, o) = h.local(None, "/api/auth/passkeys/enroll/options", json!({ "operator": "op-3", "code": code })).await;
    assert_eq!(s, StatusCode::OK, "{o}");
    let mut k3 = SoftAuthenticator::new("op-3 phone");
    let reg = k3.register("localhost", LOCAL, &challenge(&o));
    let (s, v) = h
        .local(None, "/api/auth/passkeys/enroll", json!({ "operator": "op-3", "code": code, "credential": reg, "label": "phone" }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(h.log().contains("\"via\":\"invitation\""));
    let (s, _) = h.local(None, "/api/auth/passkeys/enroll/options", json!({ "operator": "op-3", "code": code })).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "an invitation is used once");
    assert!(CredentialFile::load(&h.file).expect("file").operators["op-3"].passkey_invite.is_none());
    h.passkey_login("op-3", &mut k3).await.expect("signed in");
    let r = h.replays_authenticated();
    assert_eq!((r.passkeys_registered, r.passkey_logins), (2, 2));
    h.no_secret_logged(&[&op1, &c]);
    assert!(!h.log().contains(&code), "the invitation's code is never logged");
}

#[tokio::test]
async fn a_passkey_removed_is_logged_and_ends_the_operators_sessions() {
    let (h, mut keys) = harness(&["op-2"], |_| {}, None);
    let mut k2 = keys.remove("op-2").expect("op-2's");
    let op2 = h.passkey_login("op-2", &mut k2).await.expect("signed in");
    let mut f = CredentialFile::load(&h.file).expect("file");
    f.remove_passkey("op-2", "laptop").expect("removed");
    f.save(&h.file).expect("saved");
    h.auth.refresh();
    let log = h.log();
    assert!(log.contains("\"type\":\"passkey.removed\""), "{log}");
    assert!(log.contains("\"detail\":\"credential changed\""), "{log}");
    let (s, _) = h.post(&op2, "/api/intents", json!({ "task": "hold", "uids": [1] })).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "the session ended");
    // the removed passkey no longer signs in: the options name no credential of op-2's
    let err = h.passkey_login("op-2", &mut k2).await.expect_err("refused");
    assert_eq!(err.0, StatusCode::UNAUTHORIZED, "{:?}", err.1);
    let r = h.replays_authenticated();
    assert_eq!(r.passkeys_removed, 1);
}

#[tokio::test]
async fn login_options_do_not_say_who_has_a_passkey() {
    let (h, _) = harness(&["op-2"], |_| {}, None);
    let (s1, a) = h.local(None, "/api/auth/login/passkey", json!({ "operator": "nobody" })).await;
    let (s2, b) = h.local(None, "/api/auth/login/passkey", json!({ "operator": "op-1" })).await;
    let (s3, c) = h.local(None, "/api/auth/login/passkey", json!({ "operator": "op-2" })).await;
    assert_eq!((s1, s2, s3), (StatusCode::OK, StatusCode::OK, StatusCode::OK));
    for v in [&a, &b, &c] {
        let keys: Vec<&str> = v["publicKey"].as_object().expect("options").keys().map(String::as_str).collect();
        assert!(keys.contains(&"challenge") && keys.contains(&"allowCredentials") && keys.contains(&"rpId"), "{v}");
        assert_eq!(v["publicKey"]["allowCredentials"].as_array().map(Vec::len), Some(1), "{v}");
        assert_eq!(v["publicKey"]["userVerification"], "required");
    }
    // nothing logged for any of them (a challenge is not an act)
    assert_eq!(h.log().lines().count(), 1, "{}", h.log());
}

#[tokio::test]
async fn a_dev_run_is_unchanged() {
    let e = super::engagement::engage_rig_auth(None, PathBuf::from("memory"), AuthMode::Dev);
    let auth = Auth::new(AuthSettings::dev(), e.r.d.clone(), CredentialFile::default());
    let app = app(&e, auth.clone());
    let h = H { e, app, auth, file: PathBuf::new(), key: String::new() };
    let (s, _, v) = h
        .call("POST", "/api/auth/step_up", Some(json!({ "act": "declare_hostile", "track": "T-0001" })), &[("origin", CONSOLE), ("x-murmur-operator", "op-2")])
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    let (s, _, _) = h.call("POST", "/api/auth/login/passkey", Some(json!({ "operator": "op-2" })), &[]).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (_, _, me) = h.call("GET", "/api/auth/session", None, &[]).await;
    assert_eq!(me["auth"], "dev");
    assert!(me.get("passkey").is_none());
    assert!(!h.log().contains("passkeys"), "a dev run logs no passkey");
}
