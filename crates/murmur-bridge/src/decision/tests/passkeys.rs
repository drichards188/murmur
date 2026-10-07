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
    let auth = Auth::new(settings, e.r.d.clone(), c).expect("auth");
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

/// Rules 13 with rules 10: where the policy steps up `accept_unknown` and asks for a passkey,
/// accepting a lost contact with a nomination takes the operator's passkey, its challenge
/// derived from exactly those contacts (each since its last fix): the password is refused, an
/// assertion made for another fix is refused, and the right one accepts it (`engage.accepted`
/// quoting the passkey step-up, which replay verifies again).
#[tokio::test]
async fn accepting_a_lost_contact_takes_a_passkey_made_for_it() {
    let (h, mut keys) = harness(
        &["op-2"],
        |p| {
            passkey_policy(p);
            p.approval.step_up_acts = Some(vec![murmur_decide::StepUpAct::AcceptUnknown]);
        },
        None,
    );
    let op1 = h.login("op-1", PW1).await;
    let mut k2 = keys.remove("op-2").expect("op-2's");
    let op2 = h.passkey_login("op-2", &mut k2).await.expect("signed in");
    let since = h.picture_with_lost(&op1).await;
    let nomination = |extra: Value| {
        let mut b = json!({
            "task": "engage", "uids": [1], "track": "T-0001",
            "effect_radius_m": 100, "window_s": 300, "fallback": "return",
            "accept_lost": [{ "track": "T-0002", "since": since }],
        });
        for (k, v) in extra.as_object().expect("object") {
            b[k] = v.clone();
        }
        b
    };
    // the password, where the policy asks for a passkey: refused
    let (s, v) = h.local(Some(&op2), "/api/intents", nomination(json!({ "password": PW2 }))).await;
    assert_eq!((s, v["factor"].clone()), (StatusCode::UNAUTHORIZED, json!("passkey")), "{v}");
    // a passkey made for an acceptance of another fix: the engine compares the act and refuses
    let other = json!({ "act": "accept_unknown", "intent": null, "accept_lost": [{ "track": "T-0002", "since": since + 1000 }] });
    let (s, o) = h.step_options(&op2, other).await;
    assert_eq!(s, StatusCode::OK, "{o}");
    let a = k2.assertion_json("localhost", LOCAL, &challenge(&o));
    let (s, v) = h.local(Some(&op2), "/api/intents", nomination(json!({ "passkey": a }))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    let made = format!("was made for an acceptance of lost contacts T-0002 since t={} with a nomination, not for", since + 1000);
    assert!(v["detail"].as_str().unwrap_or("").contains(&made), "{v}");
    assert_eq!(h.e.r.events("engage.accepted"), 0, "nothing accepted");
    // made for these contacts: accepted with the nomination, which waits for its signers
    let act = json!({ "act": "accept_unknown", "intent": null, "accept_lost": [{ "track": "T-0002", "since": since }] });
    let (s, o) = h.step_options(&op2, act).await;
    assert_eq!(s, StatusCode::OK, "{o}");
    let a = k2.assertion_json("localhost", LOCAL, &challenge(&o));
    let (s, v) = h.local(Some(&op2), "/api/intents", nomination(json!({ "passkey": a }))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["intent"]["state"], "pending_approval", "{v}");
    let log = h.log();
    let accepted = log.lines().find(|l| l.contains("\"type\":\"engage.accepted\"")).expect("accepted").to_string();
    assert!(accepted.contains("\"contact\":\"T-0002\"") && accepted.contains("\"step_up\":"), "{accepted}");
    // both assertions are on the record as step-ups (the refused one too: its act refused it),
    // each verified again by replay
    let r = h.replays_authenticated();
    assert_eq!((r.passkey_logins, r.passkey_step_ups, r.lost_contacts), (1, 2, 1));
    h.no_secret_logged(&[&op1, &op2]);
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

/// The credential ids a login's options name, decoded.
fn credential_ids(options: &Value) -> Vec<Vec<u8>> {
    options["publicKey"]["allowCredentials"]
        .as_array()
        .expect("allowCredentials")
        .iter()
        .map(|c| murmur_decide::passkey::unb64(c["id"].as_str().expect("an id")).expect("b64"))
        .collect()
}

/// murmur-i58: a login's options do not say which ids exist or hold a passkey. One shape for
/// all; a decoy names as many credentials, as long, as real authenticators make (it named one
/// of 16 bytes, while real ones are what the authenticator chose: 32 for the virtual one, 64
/// for a security key), the same at every request; and a wrong answer is refused in the same
/// words for an id with a passkey and one without.
#[tokio::test]
async fn login_options_do_not_say_who_has_a_passkey() {
    let (h, mut keys) = harness(&["op-2"], |_| {}, None);
    let (s1, a) = h.local(None, "/api/auth/login/passkey", json!({ "operator": "nobody" })).await;
    let (s2, b) = h.local(None, "/api/auth/login/passkey", json!({ "operator": "op-1" })).await;
    let (s3, c) = h.local(None, "/api/auth/login/passkey", json!({ "operator": "op-2" })).await;
    assert_eq!((s1, s2, s3), (StatusCode::OK, StatusCode::OK, StatusCode::OK));
    // one shape for all three: the same keys and values but the challenge and the ids
    let shape = |v: &Value| {
        let mut v = v.clone();
        v["publicKey"]["challenge"] = json!("");
        v["publicKey"]["allowCredentials"] = json!([]);
        v
    };
    let lengths: Vec<usize> = murmur_auth::passkeys::DECOY_IDS.iter().map(|k| k.len).collect();
    for v in [&a, &b, &c] {
        assert_eq!(v["publicKey"]["userVerification"], "required");
        assert_eq!(shape(v), shape(&c), "{v} against op-2's real options {c}");
        for e in v["publicKey"]["allowCredentials"].as_array().expect("ids") {
            assert_eq!(e.as_object().map(|o| o.len()), Some(2), "type and id only: {e}");
            assert_eq!(e["type"], "public-key");
        }
    }
    for v in [&a, &b] {
        let ids = credential_ids(v);
        assert!((1..=3).contains(&ids.len()), "{v}");
        assert!(ids.iter().all(|i| lengths.contains(&i.len())), "{v}");
    }
    // the same ids at every request (as a real operator's), and across ids drawn like real
    // ones: several lengths, sometimes more than one credential
    let (_, a2) = h.local(None, "/api/auth/login/passkey", json!({ "operator": "nobody" })).await;
    assert_eq!(credential_ids(&a2), credential_ids(&a));
    let mut seen_lengths = std::collections::BTreeSet::new();
    let mut seen_counts = std::collections::BTreeSet::new();
    for i in 0..60 {
        let (_, o) = h.local(None, "/api/auth/login/passkey", json!({ "operator": format!("someone-{i}") })).await;
        let ids = credential_ids(&o);
        seen_counts.insert(ids.len());
        seen_lengths.extend(ids.iter().map(Vec::len));
    }
    assert!(seen_lengths.len() >= 3 && seen_counts.len() >= 2, "{seen_lengths:?} {seen_counts:?}");
    // nothing logged for any of them (a challenge is not an act)
    assert_eq!(h.log().lines().count(), 1, "{}", h.log());
    // a wrong answer, refused alike: a decoy's challenge answered by an authenticator that
    // does not hold the decoy, and op-2's own challenge answered with op-2's credential id but
    // a signature that is not op-2's (both what a stranger can make)
    let mut stranger = SoftAuthenticator::new("stranger");
    let decoy_answer = stranger.assertion_json("localhost", LOCAL, &challenge(&a2));
    let mut forged = keys.remove("op-2").expect("op-2's").assertion_json("localhost", LOCAL, &challenge(&c));
    let sig = murmur_decide::passkey::unb64(forged["response"]["signature"].as_str().expect("sig")).expect("b64");
    let mut bad = sig.clone();
    if let Some(last) = bad.last_mut() {
        *last ^= 1;
    }
    forged["response"]["signature"] = json!(murmur_decide::passkey::b64(&bad));
    let (sa, _, va) = h.call("POST", "/api/auth/login", Some(json!({ "operator": "nobody", "passkey": decoy_answer })), &[("origin", LOCAL)]).await;
    let (sc, _, vc) = h.call("POST", "/api/auth/login", Some(json!({ "operator": "op-2", "passkey": forged })), &[("origin", LOCAL)]).await;
    assert_eq!((sa, sc), (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED));
    assert_eq!(va, vc, "the same refusal for an id without a passkey and one with");
    // and a made-up challenge alike
    let (_, _, vm) = h
        .call("POST", "/api/auth/login", Some(json!({ "operator": "op-1", "passkey": stranger.assertion_json("localhost", LOCAL, &[9u8; 32]) })), &[("origin", LOCAL)])
        .await;
    assert_eq!(vm, vc);
}

/// The review of murmur-i58: a flood of made-up ids pushed an operator's live challenge out of
/// the login table, and their genuine answer then failed and counted against their own limit
/// (the reviewer's probe: four rounds 401, the fifth 429). An answer to a challenge that is not
/// live is now refused without counting or logging it: after six rounds the operator signs in.
/// One round at the real cap too (10,000 made-up ids, eviction in constant time).
#[tokio::test]
async fn a_flood_of_made_up_ids_does_not_push_an_operator_toward_the_lock() {
    let (h, mut keys) = harness(&["op-2"], |_| {}, None);
    let mut k2 = keys.remove("op-2").expect("op-2's");
    let ask = |op: String| {
        let h = &h;
        async move {
            let (s, o) = h.local(None, "/api/auth/login/passkey", json!({ "operator": op })).await;
            assert_eq!(s, StatusCode::OK, "{o}");
            o
        }
    };
    let flood = |round: usize, n: usize| {
        let h = &h;
        async move {
            for i in 0..n {
                let (s, _) = h.local(None, "/api/auth/login/passkey", json!({ "operator": format!("flood-{round}-{i}") })).await;
                assert_eq!(s, StatusCode::OK);
            }
        }
    };
    h.auth.set_login_cap(100);
    for round in 0..6 {
        let o = ask("op-2".into()).await;
        flood(round, 150).await;
        let answer = json!({ "operator": "op-2", "passkey": k2.assertion_json("localhost", LOCAL, &challenge(&o)) });
        let (s, _, v) = h.call("POST", "/api/auth/login", Some(answer), &[("origin", LOCAL)]).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "round {round}: {v}");
        assert_eq!(v["detail"], crate::auth::LOGIN_PASSKEY_REFUSED, "round {round}");
    }
    assert!(!h.log().contains("auth.refused"), "not a guess, not on the record: {}", h.log());
    // the real cap: one round of as many made-up ids
    h.auth.set_login_cap(crate::auth::LOGIN_IDS_CAP);
    let o = ask("op-2".into()).await;
    let started = std::time::Instant::now();
    flood(6, crate::auth::LOGIN_IDS_CAP).await;
    eprintln!("{} made-up ids in {:?}", crate::auth::LOGIN_IDS_CAP, started.elapsed());
    let answer = json!({ "operator": "op-2", "passkey": k2.assertion_json("localhost", LOCAL, &challenge(&o)) });
    let (s, _, v) = h.call("POST", "/api/auth/login", Some(answer), &[("origin", LOCAL)]).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    // asked again, signed in
    let o = ask("op-2".into()).await;
    let answer = json!({ "operator": "op-2", "passkey": k2.assertion_json("localhost", LOCAL, &challenge(&o)) });
    let (s, _, v) = h.call("POST", "/api/auth/login", Some(answer), &[("origin", LOCAL)]).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    h.replays_authenticated();
}

/// murmur-i58: a login's options are limited per id without turning its operator away: at
/// most four live per id, a request past them served the newest again (never refused, and the
/// operator's own challenge never pushed out), ids without a passkey alike; step-ups stay
/// bounded per session.
#[tokio::test]
async fn login_options_are_limited_per_id_and_a_flood_cannot_turn_the_operator_away() {
    let (h, mut keys) = harness(&["op-2"], |_| {}, None);
    let mut k2 = keys.remove("op-2").expect("op-2's");
    let ask = |op: String| {
        let h = &h;
        async move {
            let (s, o) = h.local(None, "/api/auth/login/passkey", json!({ "operator": op })).await;
            assert_eq!(s, StatusCode::OK, "{o}");
            o
        }
    };
    // the operator's own first, then a stranger's flood for the same id
    let own = ask("op-2".into()).await;
    let mut flood = Vec::new();
    for _ in 0..200 {
        flood.push(challenge(&ask("op-2".into()).await));
    }
    let distinct: std::collections::BTreeSet<&Vec<u8>> = flood.iter().collect();
    assert_eq!(distinct.len(), 3, "three more issued, then the newest served again");
    assert!(flood[3..].iter().all(|c| c == &flood[2]), "past the bound: the newest again");
    // ids without a passkey are limited alike (their options are decoys)
    let mut decoys = std::collections::BTreeSet::new();
    for _ in 0..50 {
        decoys.insert(challenge(&ask("nobody".into()).await));
    }
    assert_eq!(decoys.len(), 4);
    // the operator's own challenge still answers: signed in
    let answer = json!({ "operator": "op-2", "passkey": k2.assertion_json("localhost", LOCAL, &challenge(&own)) });
    let (s, _, v) = h.call("POST", "/api/auth/login", Some(answer), &[("origin", LOCAL)]).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // so does the one the flood was served again (an operator asking then was given it)
    let answer = json!({ "operator": "op-2", "passkey": k2.assertion_json("localhost", LOCAL, &flood[2]) });
    let (s, _, v) = h.call("POST", "/api/auth/login", Some(answer), &[("origin", LOCAL)]).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // a slot freed: the next request is a fresh challenge, not one already served
    let next = challenge(&ask("op-2".into()).await);
    assert!(!distinct.contains(&next) && next != challenge(&own));
    h.replays_authenticated();
}

/// The review of murmur-i58: the decoys' key is made whole beside the credential file and read
/// after, so decoys survive a restart; a file there that is not a key (an empty one, as a
/// crash between creating and writing it left before) stops the bridge saying what to do,
/// instead of a per-run key that would change every decoy at each restart.
#[tokio::test]
async fn an_unusable_decoy_key_stops_the_bridge_instead_of_changing_the_decoys() {
    let dir = scratch();
    let file = dir.join("credentials.json");
    let a = crate::auth::decoy_secret(Some(&file)).expect("made");
    assert_eq!(crate::auth::decoy_secret(Some(&file)).expect("read"), a, "the same key after");
    std::fs::write(dir.join("credentials.json.decoy-key"), "").expect("emptied");
    let err = crate::auth::decoy_secret(Some(&file)).expect_err("refused");
    assert!(err.contains("not a key") && err.contains("remove") && err.contains("credentials.json.decoy-key"), "{err}");
    assert!(crate::auth::decoy_secret(None).is_err(), "no file to keep it beside");
    // through the bridge's start: a credentials run with passkeys does not come up
    let (h, _) = harness(&["op-2"], |_| {}, None);
    std::fs::write(h.file.with_file_name("credentials.json.decoy-key"), "").expect("emptied");
    let c = CredentialFile::load(&h.file).expect("file");
    let settings = AuthSettings {
        mode: AuthMode::Credentials,
        file: Some(h.file.clone()),
        cookie: "murmur_session_test".into(),
        secure_cookie: false,
        idle: Duration::from_secs(1800),
        max: Duration::from_secs(8 * 3600),
        origins: default_origins(5173, 8765),
        passkey_rp: "localhost".into(),
        passkey_origins: origins(),
        study: None,
    };
    let err = Auth::new(settings, h.e.r.d.clone(), c).err().expect("refused");
    assert!(err.starts_with("passkey decoys: "), "{err}");
}

#[tokio::test]
async fn a_dev_run_is_unchanged() {
    let e = super::engagement::engage_rig_auth(None, PathBuf::from("memory"), AuthMode::Dev);
    let auth = Auth::new(AuthSettings::dev(), e.r.d.clone(), CredentialFile::default()).expect("auth");
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
