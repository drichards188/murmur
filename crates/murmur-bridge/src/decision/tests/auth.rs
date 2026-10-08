//! Rules 8 through the bridge's whole HTTP surface (the router `main` serves, `app_router`):
//! operators log in with their own password, sessions are cookies that end at logout,
//! expiry or a credential change, browsers from other origins are refused, a consequential
//! signature re-enters the signer's password, two signatures need two people, the proposer
//! never signs, an agent's key never authorizes, and a dev run says what it is. Every log
//! replays, and none holds a secret.

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use murmur_auth::CredentialFile;
use murmur_decide::AuthMode;
use serde_json::Value;
use tokio::sync::watch;
use tower::ServiceExt;

use super::engagement::{Erig, engage_rig_auth};
use super::*;
use crate::auth::{Auth, Settings as AuthSettings, default_origins};

const PW1: &str = "op one has a long passphrase";
const PW2: &str = "op two has a long passphrase";
const PW3: &str = "op three has a long passphrase";
/// In ez-gate-west, west of the bridge (as the engagement tests).
const TARGET: (f64, f64) = (37.8120, -122.4950);
pub(super) const CONSOLE: &str = "http://127.0.0.1:5173";

pub(super) struct H {
    pub(super) e: Erig,
    pub(super) app: axum::Router,
    pub(super) auth: Arc<Auth>,
    pub(super) file: PathBuf,
    pub(super) key: String,
}

pub(super) fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "murmur-bridge-auth-{}-{}",
        std::process::id(),
        murmur_auth::keys::stamp().expect("random")
    ));
    std::fs::create_dir_all(&d).expect("scratch dir");
    d
}

/// A credentials run on the engagement rig: op-1 (no engagement authority), op-2 and op-3
/// (engagement authority), and agent-planner's key, sessions ending after `idle`.
fn harness(idle: Duration) -> H {
    harness_with(idle, &[], |_| {})
}

/// [`harness`] with the credentials of `scripted` marked harness-held and the policy changed
/// by `edit`.
fn harness_with(idle: Duration, scripted: &[&str], edit: impl FnOnce(&mut Policy)) -> H {
    let file = scratch().join("credentials.json");
    let mut c = CredentialFile::default();
    c.add_operator("op-1", PW1, false).expect("op-1");
    c.add_operator("op-2", PW2, true).expect("op-2");
    c.add_operator("op-3", PW3, true).expect("op-3");
    for op in scripted {
        c.set_scripted(op, true).expect("scripted");
    }
    let key = c.add_agent("agent-planner").expect("agent");
    c.save(&file).expect("saved");
    // rules 12: the harness-held credentials are on the run's roster from its start
    let e = super::engagement::engage_rig_full(
        None,
        PathBuf::from("memory"),
        AuthMode::Credentials,
        edit,
        None,
        None,
        super::EngineClock::Manual,
        scripted.iter().map(|o| OperatorId::new(*o)).collect(),
        Vec::new(),
    );
    let settings = AuthSettings {
        mode: AuthMode::Credentials,
        file: Some(file.clone()),
        cookie: "murmur_session_test".into(),
        secure_cookie: false,
        idle,
        max: Duration::from_secs(8 * 3600),
        origins: default_origins(5173, 8765),
        passkey_rp: "localhost".into(),
        passkey_origins: crate::auth::passkey_origins("localhost", &default_origins(5173, 8765)),
        study: None,
    };
    let auth = Auth::new(settings, e.r.d.clone(), c).expect("auth");
    let app = app(&e, auth.clone());
    H { e, app, auth, file, key }
}

pub(super) fn app(e: &Erig, auth: Arc<Auth>) -> axum::Router {
    let api = crate::api::router(crate::api::App {
        world: e.r.world.clone(),
        frames: Arc::new(watch::channel(None).0),
        decision: e.r.d.clone(),
        tracks: e.tracks.clone(),
        lod: crate::push::LodSettings::default(),
        auth: auth.clone(),
    });
    let agents = crate::mcp::Settings {
        agents: Vec::new(),
        keys: (auth.mode() == AuthMode::Credentials).then(|| auth.clone()),
    };
    crate::app_router(
        api,
        agents,
        e.r.world.clone(),
        e.r.d.clone(),
        auth,
        Arc::new(default_origins(5173, 8765)),
    )
}

impl H {
    pub(super) async fn call(&self, method: &str, path: &str, body: Option<Value>, headers: &[(&str, &str)]) -> (StatusCode, HeaderMap, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header(header::HOST, "127.0.0.1");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let req = b
            .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
            .expect("request");
        let resp = self.app.clone().oneshot(req).await.expect("response");
        let status = resp.status();
        let headers = resp.headers().clone();
        let sse = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("text/event-stream"));
        let bytes = tokio::time::timeout(Duration::from_secs(10), axum::body::to_bytes(resp.into_body(), 4 << 20))
            .await
            .expect("the body ends")
            .expect("body");
        let text = String::from_utf8_lossy(&bytes);
        let v = if sse {
            text.lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(str::trim)
                .rfind(|l| !l.is_empty())
                .and_then(|l| serde_json::from_str(l).ok())
                .unwrap_or(Value::Null)
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        (status, headers, v)
    }

    /// Log in from the console; the cookie as a browser sends it back.
    pub(super) async fn login(&self, op: &str, pw: &str) -> String {
        let (s, h, v) = self
            .call("POST", "/api/auth/login", Some(json!({ "operator": op, "password": pw })), &[("origin", CONSOLE)])
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let set = h.get(header::SET_COOKIE).and_then(|c| c.to_str().ok()).expect("a cookie");
        set.split(';').next().expect("name=value").to_string()
    }

    /// A console request by the operator whose cookie this is.
    pub(super) async fn post(&self, cookie: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let (s, _, v) = self
            .call("POST", path, Some(body), &[("origin", CONSOLE), ("cookie", cookie)])
            .await;
        (s, v)
    }

    pub(super) fn log(&self) -> String {
        self.e.r.log.text()
    }

    /// Our armed platform on the target and the boat declared hostile by op-1 (through the
    /// API), after a minute and a half of detections.
    pub(super) async fn picture(&self, op1: &str) {
        self.e.r.vehicle(1, Firmware::Copter, "fpv", TARGET);
        sleep(Duration::from_millis(500)).await;
        self.e.r.clock.set(snapshot::ms(clock::wall()));
        let now = self.e.now_s();
        for s in 0..90 {
            self.e.sense_at(now - 90.0 + f64::from(s), true);
        }
        self.e.sense_at(now, true);
        let (s, v) = self
            .post(op1, "/api/tracks/T-0001/declare", json!({ "affiliation": "hostile" }))
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }

    /// Rules 16: [`H::picture`] with a fishing boat 400 m north of the target seen for its first
    /// 100 s of 400, then out of view: the tracker drops it 180 s later, the decision tick hands
    /// the drop to the engine (`track.dropped`). Its last fix (engine ms), as an acceptance
    /// names it.
    pub(super) async fn picture_with_lost(&self, op1: &str) -> i64 {
        self.e.r.vehicle(1, Firmware::Copter, "fpv", TARGET);
        sleep(Duration::from_millis(500)).await;
        self.e.r.clock.set(snapshot::ms(clock::wall()));
        let now = self.e.now_s();
        let boat = crate::tracks::truth::ContactTruth {
            id: "fisher".into(),
            name: "fishing boat".into(),
            affiliation: crate::tracks::truth::TruthAffiliation::Neutral,
            domain: crate::tracks::truth::Domain::Surface,
            kind: "fishing_vessel".into(),
            lat: TARGET.0 + 400.0 / 111_320.0,
            lon: TARGET.1,
            alt_m: 0.0,
            speed_mps: 0.0,
            course_deg: 0.0,
            emits: false,
            signature: 1.0,
            ais: None,
        };
        for s in 0..400 {
            let seen: &[crate::tracks::truth::ContactTruth] = if s < 100 { std::slice::from_ref(&boat) } else { &[] };
            self.e.sense_at_with(now - 400.0 + f64::from(s), true, seen);
        }
        self.e.sense_at(now, true);
        self.e.r.d.tick();
        assert_eq!(self.e.r.events("track.dropped"), 1, "the boat dropped");
        let (s, v) = self
            .post(op1, "/api/tracks/T-0001/declare", json!({ "affiliation": "hostile" }))
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let lost = self.e.tracks.lost_json();
        assert_eq!(lost.len(), 1, "{lost:?}");
        lost[0]["since_t"].as_i64().expect("since")
    }

    /// Rules 16: a nomination waiting for its signers while a fishing boat 1.5 km north of the
    /// target is in view, then the boat out of view for 185 s: the tracker drops it and the
    /// decision tick hands the drop to the engine. The nomination (by `op1`) and the boat's
    /// last fix (engine ms), as an acceptance names it.
    pub(super) async fn waiting_then_lost(&self, op1: &str) -> (u64, i64) {
        self.e.r.vehicle(1, Firmware::Copter, "fpv", TARGET);
        sleep(Duration::from_millis(500)).await;
        self.e.r.clock.set(snapshot::ms(clock::wall()));
        let boat = crate::tracks::truth::ContactTruth {
            id: "fisher".into(),
            name: "fishing boat".into(),
            affiliation: crate::tracks::truth::TruthAffiliation::Neutral,
            domain: crate::tracks::truth::Domain::Surface,
            kind: "fishing_vessel".into(),
            lat: TARGET.0 + 1_500.0 / 111_320.0,
            lon: TARGET.1,
            alt_m: 0.0,
            speed_mps: 0.0,
            course_deg: 0.0,
            emits: false,
            signature: 1.0,
            ais: None,
        };
        let now = self.e.now_s();
        for s in 0..90 {
            self.e.sense_at_with(now - 90.0 + f64::from(s), true, std::slice::from_ref(&boat));
        }
        self.e.sense_at_with(now, true, std::slice::from_ref(&boat));
        let (s, v) = self
            .post(op1, "/api/tracks/T-0001/declare", json!({ "affiliation": "hostile" }))
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let id = self.nominate(op1).await;
        // out of view from now on: dropped 180 s later, handed over at the tick
        for _ in 0..185 {
            self.e.r.clock.advance_ms(1000);
            self.e.sense(true);
        }
        // the platform's heartbeats at the new time (it has not gone quiet)
        sleep(Duration::from_millis(500)).await;
        self.e.r.d.tick();
        assert_eq!(self.e.r.events("track.dropped"), 1, "the boat dropped");
        let lost = self.e.tracks.lost_json();
        assert_eq!(lost.len(), 1, "{lost:?}");
        (id, lost[0]["since_t"].as_i64().expect("since"))
    }

    /// A nomination on the boat by whoever's cookie this is.
    pub(super) async fn nominate(&self, cookie: &str) -> u64 {
        let body = json!({
            "task": "engage", "uids": [1], "track": "T-0001",
            "effect_radius_m": 100, "window_s": 300, "fallback": "return",
        });
        let (s, v) = self.post(cookie, "/api/intents", body).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        assert_eq!(v["intent"]["state"], "pending_approval", "{v}");
        v["intent"]["id"].as_u64().expect("id")
    }

    /// Look, then sign with `password` (None: no step-up).
    pub(super) async fn sign(&self, cookie: &str, id: u64, password: Option<&str>) -> (StatusCode, Value) {
        let (s, t) = self.post(cookie, &format!("/api/intents/{id}/picture"), json!({})).await;
        if s != StatusCode::OK {
            return (s, t);
        }
        let mut body = json!({ "token": t["id"] });
        if let Some(p) = password {
            body["password"] = json!(p);
        }
        self.post(cookie, &format!("/api/intents/{id}/approve"), body).await
    }

    pub(super) fn replays_authenticated(&self) -> replay::ReplayReport {
        let r = replay::replay_str(&self.log()).expect("parses");
        assert!(r.ok(), "{:?}", r.mismatches);
        assert_eq!(r.auth, Some(AuthMode::Credentials));
        assert!(r.authenticated());
        r
    }

    /// No password, cookie token or agent key anywhere in the audit log.
    pub(super) fn no_secret_logged(&self, cookies: &[&str]) {
        let log = self.log();
        for s in [PW1, PW2, PW3, self.key.as_str()] {
            assert!(!log.contains(s), "a secret in the audit log");
        }
        for c in cookies {
            let token = c.split_once('=').map_or(*c, |(_, t)| t);
            assert!(!log.contains(token), "a session token in the audit log");
        }
    }
}

#[tokio::test]
async fn a_wrong_password_and_an_unknown_operator_get_the_same_answer() {
    let h = harness(Duration::from_secs(1800));
    let (s1, _, v1) = h
        .call("POST", "/api/auth/login", Some(json!({ "operator": "op-1", "password": "not the password at all" })), &[])
        .await;
    let (s2, _, v2) = h
        .call("POST", "/api/auth/login", Some(json!({ "operator": "nobody", "password": PW1 })), &[])
        .await;
    assert_eq!((s1, s2), (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED));
    assert_eq!(v1, v2, "no hint which ids exist");
    assert!(!v1.to_string().contains("not the password"), "the password is never echoed");
    // nothing opened, nothing logged but the run's start
    assert!(!h.log().contains("session.opened"));
    // a malformed body is refused without echoing it
    let (s, _, v) = h
        .call("POST", "/api/auth/login", Some(json!({ "operator": "op-1", "pw": PW1 })), &[])
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!v.to_string().contains(PW1));
}

#[tokio::test]
async fn failed_logins_lock_the_id() {
    let h = harness(Duration::from_secs(1800));
    let wrong = json!({ "operator": "op-1", "password": "wrong wrong wrong wrong" });
    for _ in 0..4 {
        let (s, _, _) = h.call("POST", "/api/auth/login", Some(wrong.clone()), &[]).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    let (s, hd, _) = h.call("POST", "/api/auth/login", Some(wrong.clone()), &[]).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert!(hd.get(header::RETRY_AFTER).is_some());
    // locked: even the right password is refused, without being checked
    let (s, _, _) = h
        .call("POST", "/api/auth/login", Some(json!({ "operator": "op-1", "password": PW1 })), &[])
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    // another operator is not affected
    h.login("op-2", PW2).await;
}

#[tokio::test]
async fn a_session_is_a_strict_httponly_cookie_that_dies_at_logout() {
    let h = harness(Duration::from_secs(1800));
    let (s, hd, v) = h
        .call("POST", "/api/auth/login", Some(json!({ "operator": "op-1", "password": PW1 })), &[("origin", CONSOLE)])
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["operator"], "op-1");
    assert_eq!(v["engage"], false);
    let set = hd.get(header::SET_COOKIE).and_then(|c| c.to_str().ok()).expect("cookie").to_string();
    for attr in ["HttpOnly", "SameSite=Strict", "Path=/", "Max-Age=28800"] {
        assert!(set.contains(attr), "{set}");
    }
    let cookie = set.split(';').next().expect("pair").to_string();
    let (_, _, me) = h.call("GET", "/api/auth/session", None, &[("cookie", &cookie)]).await;
    assert_eq!((me["signed_in"].clone(), me["operator"].clone()), (json!(true), json!("op-1")));
    // the operator acts as themselves
    let (s, v) = h
        .post(&cookie, "/api/intents", json!({ "task": "hold", "uids": [1] }))
        .await;
    assert!(s == StatusCode::CREATED || s == StatusCode::NOT_FOUND, "{s} {v}");
    // logout: the session ends in the log, and the cookie is dead (replayed: 401)
    let (s, hd, _) = h.call("POST", "/api/auth/logout", None, &[("origin", CONSOLE), ("cookie", &cookie)]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(hd.get(header::SET_COOKIE).and_then(|c| c.to_str().ok()).is_some_and(|c| c.contains("Max-Age=0")));
    assert!(h.log().contains("\"action\":\"session.closed\",\"detail\":\"logout\""));
    let (s, v) = h.post(&cookie, "/api/tracks/T-0001/declare", json!({ "affiliation": "hostile" })).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    let (_, _, me) = h.call("GET", "/api/auth/session", None, &[("cookie", &cookie)]).await;
    assert_eq!(me["signed_in"], false);
    h.replays_authenticated();
    h.no_secret_logged(&[&cookie]);
}

#[tokio::test]
async fn an_idle_session_expires_and_says_so() {
    let h = harness(Duration::from_millis(400));
    let cookie = h.login("op-1", PW1).await;
    // a status query is not activity
    sleep(Duration::from_millis(250)).await;
    let _ = h.call("GET", "/api/auth/session", None, &[("cookie", &cookie)]).await;
    sleep(Duration::from_millis(250)).await;
    let (s, _) = h.post(&cookie, "/api/intents", json!({ "task": "hold", "uids": [1] })).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert!(h.log().contains("\"action\":\"session.closed\",\"detail\":\"expired (idle)\""));
    // and a sweep closes one nobody touches
    let other = h.login("op-2", PW2).await;
    sleep(Duration::from_millis(500)).await;
    h.auth.sweep();
    assert_eq!(h.log().matches("expired (idle)").count(), 2);
    let (s, _) = h.post(&other, "/api/intents", json!({ "task": "hold", "uids": [1] })).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    h.replays_authenticated();
}

/// murmur-dup: sessions and locks run on the monotonic clock, so a wall clock stepping past
/// the 8 hours a session may last, or back, ends no session.
#[tokio::test]
async fn a_wall_clock_that_steps_ends_no_session() {
    let h = harness(Duration::from_secs(1800));
    let cookie = h.login("op-1", PW1).await;
    clock::test_clock::step_utc(9.0 * 3600.0);
    h.auth.sweep();
    let (s, v) = h.post(&cookie, "/api/intents", json!({ "task": "hold", "uids": [1] })).await;
    assert!(s == StatusCode::CREATED || s == StatusCode::NOT_FOUND, "{s} {v}");
    clock::test_clock::step_utc(-9.0 * 3600.0 - 3600.0);
    h.auth.sweep();
    let (s, v) = h.post(&cookie, "/api/intents", json!({ "task": "hold", "uids": [1] })).await;
    assert!(s == StatusCode::CREATED || s == StatusCode::NOT_FOUND, "{s} {v}");
    assert!(!h.log().contains("session.closed"));
    h.replays_authenticated();
}

#[tokio::test]
async fn a_browser_on_another_origin_changes_nothing() {
    let h = harness(Duration::from_secs(1800));
    let cookie = h.login("op-1", PW1).await;
    let before = h.log().lines().count();
    for headers in [
        vec![("origin", "http://evil.example"), ("cookie", cookie.as_str())],
        // another port on the same host: same site, other origin
        vec![("origin", "http://127.0.0.1:9999"), ("cookie", cookie.as_str())],
        vec![("sec-fetch-site", "cross-site"), ("cookie", cookie.as_str())],
    ] {
        let (s, _, v) = h
            .call("POST", "/api/tracks/T-0001/declare", Some(json!({ "affiliation": "hostile" })), &headers)
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
        // a login from another origin too (no login CSRF)
        let (s, _, _) = h
            .call("POST", "/api/auth/login", Some(json!({ "operator": "op-1", "password": PW1 })), &headers)
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    assert_eq!(h.log().lines().count(), before, "nothing was logged");
    // reads stay open; the console's own origin goes through
    let (s, _, _) = h.call("GET", "/api/inbox", None, &[("origin", "http://evil.example")]).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn a_header_does_not_name_the_operator_in_a_credentials_run() {
    let h = harness(Duration::from_secs(1800));
    // the dev-mode header alone: not signed in
    let (s, _, v) = h
        .call("POST", "/api/intents", Some(json!({ "task": "hold", "uids": [1] })), &[("x-murmur-operator", "op-2")])
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    // the console's legacy path has no headerless default either
    let (s, _, _) = h.call("POST", "/api/tasks", Some(json!({ "task": "hold", "uids": [1] })), &[]).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // signed in as op-1, naming op-2: refused
    let cookie = h.login("op-1", PW1).await;
    let (s, _, _) = h
        .call(
            "POST",
            "/api/intents",
            Some(json!({ "task": "hold", "uids": [1] })),
            &[("cookie", &cookie), ("x-murmur-operator", "op-2")],
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn two_people_sign_each_with_their_password_and_one_person_cannot_sign_twice() {
    let h = harness(Duration::from_secs(1800));
    let op1 = h.login("op-1", PW1).await;
    let op2 = h.login("op-2", PW2).await;
    h.picture(&op1).await;
    let id = h.nominate(&op1).await;
    // op-1 has no engagement authority: nothing to sign
    let (s, _) = h.sign(&op1, id, Some(PW1)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // op-2 without re-entering the password: refused, and told why
    let (s, v) = h.sign(&op2, id, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    assert_eq!(v["step_up"], true);
    // with a wrong one, or another person's: refused
    let (s, v) = h.sign(&op2, id, Some("not op-2's password at all")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    let (s, _) = h.sign(&op2, id, Some(PW3)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "op-3's password does not sign through op-2's session");
    // each refusal is on the record, under op-2's session, without the password
    let log = h.log();
    assert!(log.contains(&format!("\"operator\":\"op-2\",\"session\":2,\"action\":\"step_up.refused\",\"detail\":\"i{id}: no password given\"")), "{log}");
    assert_eq!(log.matches(&format!("\"action\":\"step_up.refused\",\"detail\":\"i{id}: wrong password\"")).count(), 2);
    // with their own: the first signature
    let (s, v) = h.sign(&op2, id, Some(PW2)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["intent"]["state"], "pending_approval");
    // the same person again, however they come: refused
    let (s, _) = h.sign(&op2, id, Some(PW2)).await;
    assert_eq!(s, StatusCode::CONFLICT, "already signed");
    let again = h.login("op-2", PW2).await;
    let (s, _) = h.sign(&again, id, Some(PW2)).await;
    assert_eq!(s, StatusCode::CONFLICT, "a second login is the same person");
    // the second person: approved and dispatched
    let op3 = h.login("op-3", PW3).await;
    let (s, v) = h.sign(&op3, id, Some(PW3)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["intent"]["state"], "dispatched", "{v}");
    let r = h.replays_authenticated();
    assert_eq!((r.concurrences, r.approvals, r.stepped_signatures), (1, 1, 2));
    // every session says it was opened on a password, every signature names its step-up
    let log = h.log();
    assert_eq!(log.matches("\"action\":\"session.opened\"").count(), log.matches("\"auth\":\"password\"").count() - r.step_ups);
    assert_eq!(log.matches("\"step_up\":{\"seq\":").count(), 2);
    h.no_secret_logged(&[&op1, &op2, &op3, &again]);
}

#[tokio::test]
async fn the_proposer_cannot_sign_even_with_their_password() {
    let h = harness(Duration::from_secs(1800));
    let op1 = h.login("op-1", PW1).await;
    let op2 = h.login("op-2", PW2).await;
    h.picture(&op1).await;
    let id = h.nominate(&op2).await;
    let (s, v) = h.sign(&op2, id, Some(PW2)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    let (s, v) = h
        .post(&op2, &format!("/api/intents/{id}/approve"), json!({ "token": 1, "password": PW2 }))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert!(h.log().contains("the proposer may not approve it"));
    h.replays_authenticated();
}

#[tokio::test]
async fn repeated_wrong_step_ups_end_the_session() {
    let h = harness(Duration::from_secs(1800));
    let op1 = h.login("op-1", PW1).await;
    let op2 = h.login("op-2", PW2).await;
    h.picture(&op1).await;
    let id = h.nominate(&op1).await;
    for _ in 0..4 {
        let (s, _) = h.sign(&op2, id, Some("guessing guessing guessing")).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    let (s, v) = h.sign(&op2, id, Some("guessing guessing guessing")).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    // the limit is the session's own, and its answer says so (murmur-jfi review)
    assert!(v["detail"].as_str().unwrap_or_default().starts_with("too many failed attempts for this session"), "{v}");
    assert!(h.log().contains("step-up failed repeatedly"));
    // the refusals came first, under the session, then its end
    let log = h.log();
    let refused = log.find("too many failed attempts").expect("the lock is on the record");
    let ended = log.find("step-up failed repeatedly").expect("the session's end");
    assert!(refused < ended);
    let (s, _) = h.sign(&op2, id, Some(PW2)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "the session is gone");
    h.replays_authenticated();
}

#[tokio::test]
async fn an_agent_key_never_authorizes() {
    let h = harness(Duration::from_secs(1800));
    let op1 = h.login("op-1", PW1).await;
    h.picture(&op1).await;
    let id = h.nominate(&op1).await;
    let bearer = format!("Bearer {}", h.key);
    // the key on an operator's endpoints: not a login
    for (path, body) in [
        (format!("/api/intents/{id}/approve"), json!({ "token": 1 })),
        (format!("/api/intents/{id}/picture"), json!({})),
        ("/api/intents".to_string(), json!({ "task": "hold", "uids": [1] })),
        ("/api/tracks/T-0001/declare".to_string(), json!({ "affiliation": "hostile" })),
    ] {
        let (s, _, v) = h.call("POST", &path, Some(body), &[("authorization", &bearer)]).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{path}: {v}");
    }
    // /mcp: no key, or a bare id, is refused before any MCP session exists
    let init = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "agent-planner", "version": "0" } },
    });
    let (s, _, _) = h.call("POST", "/mcp", Some(init.clone()), &[]).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _, _) = h.call("POST", "/mcp", Some(init.clone()), &[("x-murmur-agent", "agent-planner")]).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let wrong = format!("Bearer {}x", &h.key[..h.key.len() - 1]);
    let (s, _, _) = h.call("POST", "/mcp", Some(init.clone()), &[("authorization", &wrong)]).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // with its key it connects, and is logged by the key's id, never the key
    let (s, hd, v) = h.call("POST", "/mcp", Some(init), &[("authorization", &bearer)]).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let session = hd.get("mcp-session-id").and_then(|v| v.to_str().ok()).expect("session").to_string();
    let (s, _, _) = h
        .call(
            "POST",
            "/mcp",
            Some(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })),
            &[("authorization", &bearer), ("mcp-session-id", &session)],
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let (s, _, v) = h
        .call(
            "POST",
            "/mcp",
            Some(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "get_inbox", "arguments": {} } })),
            &[("authorization", &bearer), ("mcp-session-id", &session)],
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let key_id = murmur_auth::keys::agent_key_id(&h.key).expect("key id");
    assert!(h.log().contains(&format!("via API key {key_id}")), "{}", h.log());
    // and the nomination is still waiting: nothing an agent did moved it
    assert_eq!(h.e.r.state(IntentId(id)), State::PendingApproval);
    h.replays_authenticated();
    h.no_secret_logged(&[&op1]);
}

#[tokio::test]
async fn removing_or_changing_a_credential_ends_its_sessions() {
    let h = harness(Duration::from_secs(1800));
    let op1 = h.login("op-1", PW1).await;
    let op2 = h.login("op-2", PW2).await;
    let mut c = CredentialFile::load(&h.file).expect("file");
    c.remove_operator("op-1").expect("removed");
    c.set_password("op-2", "op two's brand new passphrase").expect("changed");
    // a different size (or a later time) is what the bridge notices
    c.save(&h.file).expect("saved");
    h.auth.refresh();
    let log = h.log();
    assert!(log.contains("\"detail\":\"credential removed\""), "{log}");
    assert!(log.contains("\"detail\":\"credential changed\""), "{log}");
    for cookie in [&op1, &op2] {
        let (s, _) = h.post(cookie, "/api/intents", json!({ "task": "hold", "uids": [1] })).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    // the removed operator can't log in; the changed one only with the new password
    let (s, _, _) = h
        .call("POST", "/api/auth/login", Some(json!({ "operator": "op-1", "password": PW1 })), &[])
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _, _) = h
        .call("POST", "/api/auth/login", Some(json!({ "operator": "op-2", "password": PW2 })), &[])
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    h.login("op-2", "op two's brand new passphrase").await;
    // an agent whose key was removed is refused at once
    let mut c = CredentialFile::load(&h.file).expect("file");
    c.remove_agent("agent-planner").expect("removed");
    c.save(&h.file).expect("saved");
    h.auth.refresh();
    let (s, _, _) = h
        .call("POST", "/mcp", Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })), &[("authorization", &format!("Bearer {}", h.key))])
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    h.replays_authenticated();
}

#[tokio::test]
async fn a_dev_run_says_so_in_run_started_and_has_no_login() {
    let e = engage_rig_auth(None, PathBuf::from("memory"), AuthMode::Dev);
    let auth = Auth::new(AuthSettings::dev(), e.r.d.clone(), CredentialFile::default()).expect("auth");
    let h = H {
        app: app(&e, auth.clone()),
        e,
        auth,
        file: PathBuf::new(),
        key: "no key".into(),
    };
    let first = h.log().lines().next().expect("run.started").to_string();
    assert!(first.contains("\"auth\":\"dev\""), "{first}");
    let (s, _, v) = h
        .call("POST", "/api/auth/login", Some(json!({ "operator": "op-1", "password": PW1 })), &[])
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    let (_, _, me) = h.call("GET", "/api/auth/session", None, &[]).await;
    assert_eq!(me["auth"], "dev");
    assert!(me["warning"].as_str().is_some_and(|w| w.contains("no human-control claim")));
    // the header names the operator, as before
    let (s, _, v) = h
        .call("POST", "/api/tracks/T-0001/declare", Some(json!({ "affiliation": "hostile" })), &[("x-murmur-operator", "op-1")])
        .await;
    assert!(s == StatusCode::OK || s == StatusCode::NOT_FOUND, "{s} {v}");
    let r = replay::replay_str(&h.log()).expect("parses");
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.auth, Some(AuthMode::Dev));
    assert!(!r.authenticated(), "a dev run is not evidence about people");
}

// ---- the security review of 91dd342 ----------------------------------------------------------

/// Review fix 1: someone else's failed logins lock op-3's next login, never op-3's live
/// session: op-3's step-up still signs, and the record says whose logins failed.
#[tokio::test]
async fn failed_logins_by_someone_else_do_not_end_a_signers_session() {
    let h = harness(Duration::from_secs(1800));
    let op1 = h.login("op-1", PW1).await;
    let op2 = h.login("op-2", PW2).await;
    let op3 = h.login("op-3", PW3).await;
    h.picture(&op1).await;
    let id = h.nominate(&op1).await;
    let (s, v) = h.sign(&op2, id, Some(PW2)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // any local process can try op-3's password
    for _ in 0..5 {
        let _ = h
            .call("POST", "/api/auth/login", Some(json!({ "operator": "op-3", "password": "a guess at op-3's password" })), &[])
            .await;
    }
    let (s, _, _) = h.call("POST", "/api/auth/login", Some(json!({ "operator": "op-3", "password": PW3 })), &[]).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "op-3's next login is locked (the DoS that remains)");
    // but op-3, signed in, signs with their password: the lock is on logins, not on op-3's session
    let (s, v) = h.sign(&op3, id, Some(PW3)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["intent"]["state"], "dispatched", "{v}");
    let log = h.log();
    assert!(!log.contains("step-up failed repeatedly"), "op-3's session was not ended");
    assert!(!log.contains("step_up.refused"), "nothing is blamed on op-3's session");
    // the failed logins are on the record: whose id, why, and the lock
    assert_eq!(log.matches("\"type\":\"auth.refused\",\"operator\":\"op-3\",\"reason\":\"wrong password\"").count(), 5, "{log}");
    assert!(log.contains("\"locked_s\":30"), "{log}");
    h.replays_authenticated();
}

/// Review fix 4: the console's view reports are instrumentation, not activity: an idle
/// session still ends while a card is shown.
#[tokio::test]
async fn view_reports_do_not_keep_a_session_alive() {
    let h = harness(Duration::from_millis(400));
    let cookie = h.login("op-1", PW1).await;
    let view = json!({ "intent_id": 999, "event": "close", "shown": { "checks": [], "warnings": [], "rationale": false, "tracks": [] }, "client_t": 0 });
    for _ in 0..4 {
        sleep(Duration::from_millis(150)).await;
        let _ = h.post(&cookie, "/api/operator/view", view.clone()).await;
    }
    let (s, v) = h.post(&cookie, "/api/intents", json!({ "task": "hold", "uids": [1] })).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    assert!(h.log().contains("\"detail\":\"expired (idle)\""));
}

/// Review fix 6: concurrent wrong logins can't outrun the lock: at most the limit's worth of
/// passwords is checked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_wrong_logins_check_no_more_passwords_than_the_limit() {
    let h = Arc::new(harness(Duration::from_secs(1800)));
    let mut tasks = Vec::new();
    for _ in 0..40 {
        let h = h.clone();
        tasks.push(tokio::spawn(async move {
            h.call("POST", "/api/auth/login", Some(json!({ "operator": "op-1", "password": "wrong wrong wrong wrong" })), &[])
                .await
                .0
        }));
    }
    let mut wrong = 0;
    for t in tasks {
        if t.await.expect("task") == StatusCode::UNAUTHORIZED {
            wrong += 1;
        }
    }
    assert!(wrong <= 5, "{wrong} passwords checked before the lock");
}

/// Review fix 8: a request carrying two session cookies (one shadowing the other, from a page
/// on another port of the host) is refused, whichever comes first.
#[tokio::test]
async fn a_request_with_two_session_cookies_is_refused() {
    let h = harness(Duration::from_secs(1800));
    let good = h.login("op-1", PW1).await;
    for cookie in [format!("{good}; murmur_session_test=bogus"), format!("murmur_session_test=bogus; {good}")] {
        let (s, v) = h.post(&cookie, "/api/intents", json!({ "task": "hold", "uids": [1] })).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{cookie}: {v}");
    }
}

/// Review fix 2: a harness-held credential logs in as `auth: scripted`: authenticated, and
/// signing works the same, but replay names it apart from the people.
#[tokio::test]
async fn a_scripted_credential_logs_in_as_scripted_and_is_no_person() {
    let h = harness_with(Duration::from_secs(1800), &["op-2"], |_| {});
    let op1 = h.login("op-1", PW1).await;
    let (s, _, v) = h
        .call("POST", "/api/auth/login", Some(json!({ "operator": "op-2", "password": PW2 })), &[("origin", CONSOLE)])
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["scripted"], true);
    let op3 = h.login("op-3", PW3).await;
    h.picture(&op1).await;
    let id = h.nominate(&op1).await;
    let op2 = h.login("op-2", PW2).await;
    let (s, v) = h.sign(&op2, id, Some(PW2)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = h.sign(&op3, id, Some(PW3)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let log = h.log();
    assert!(log.contains("\"operator\":\"op-2\",\"session\":"), "{log}");
    assert!(log.contains("\"auth\":\"scripted\""), "{log}");
    let r = h.replays_authenticated();
    let ids = |v: &[&str]| v.iter().map(|o| OperatorId::new(*o)).collect::<std::collections::BTreeSet<_>>();
    assert_eq!(r.people, ids(&["op-1", "op-3"]));
    assert_eq!(r.scripted, ids(&["op-2"]));
}

/// Rules 12 (murmur-jfi): which credentials are harness-held is fixed when the run starts
/// (the roster's `scripted`, in `run.started`). Re-marking one mid-run (`murmur-auth scripted
/// <op> off`) ends its sessions as any credential change does, and its next login is refused,
/// saying why, until the bridge restarts: before, it logged in as a person's and replay listed
/// the operator among the people. Re-marking a person's credential scripted is refused the
/// same way. Such a refused login is logged (`auth.refused`, saying why) like a wrong
/// password, and the session the browser already held (op-1's here) stays (review L8).
#[tokio::test]
async fn a_credential_re_marked_mid_run_logs_in_only_after_a_restart() {
    let h = harness_with(Duration::from_secs(1800), &["op-2"], |_| {});
    assert!(h.log().lines().next().unwrap_or_default().contains("\"scripted\":[\"op-2\"]"), "{}", h.log());
    let op1 = h.login("op-1", PW1).await;
    let op2 = h.login("op-2", PW2).await;
    let op3 = h.login("op-3", PW3).await;
    let mut c = CredentialFile::load(&h.file).expect("file");
    c.set_scripted("op-2", false).expect("a person's now");
    c.set_scripted("op-3", true).expect("a harness's now");
    c.save(&h.file).expect("saved");
    h.auth.refresh();
    for cookie in [&op2, &op3] {
        let (s, _) = h.post(cookie, "/api/intents", json!({ "task": "hold", "uids": [1] })).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "the change ended its sessions");
    }
    for (who, pw, why) in [("op-2", PW2, "was harness-held (scripted) when the run started"), ("op-3", PW3, "was a person's when the run started")] {
        let (s, _, v) = h
            .call(
                "POST",
                "/api/auth/login",
                Some(json!({ "operator": who, "password": pw })),
                &[("origin", CONSOLE), ("cookie", &op1)],
            )
            .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{who}: {v}");
        assert!(v["detail"].as_str().unwrap_or_default().contains(why), "{who}: {v}");
    }
    let refused: Vec<String> = h.log().lines().filter(|l| l.contains("\"type\":\"auth.refused\"")).map(str::to_string).collect();
    assert_eq!(refused.len(), 2, "{refused:?}");
    assert!(refused[0].contains("\"operator\":\"op-2\"") && refused[0].contains("harness-held"), "{refused:?}");
    let (s, v) = h.post(&op1, "/api/intents", json!({ "task": "hold", "uids": [1] })).await;
    assert_ne!(s, StatusCode::UNAUTHORIZED, "op-1's session in that browser stays: {v}");
    let r = h.replays_authenticated();
    let ids = |v: &[&str]| v.iter().map(|o| OperatorId::new(*o)).collect::<std::collections::BTreeSet<_>>();
    assert_eq!((r.people.clone(), r.scripted.clone()), (ids(&["op-1", "op-3"]), ids(&["op-2"])));
}

/// User decision (step-up scope): with `[approval] step_up_acts = ["declare_hostile"]`, a
/// hostile declaration through the API needs the password again, and nothing changes
/// without it; another affiliation does not. The console learns the list from the session.
#[tokio::test]
async fn a_listed_act_steps_up_through_the_api() {
    let h = harness_with(Duration::from_secs(1800), &[], |p| {
        p.approval.step_up_acts = Some(vec![murmur_decide::StepUpAct::DeclareHostile]);
    });
    let op1 = h.login("op-1", PW1).await;
    let (_, _, me) = h.call("GET", "/api/auth/session", None, &[("cookie", &op1)]).await;
    assert_eq!(me["step_up_acts"], json!(["declare_hostile"]), "{me}");
    h.e.r.vehicle(1, Firmware::Copter, "fpv", TARGET);
    sleep(Duration::from_millis(500)).await;
    h.e.r.clock.set(snapshot::ms(clock::wall()));
    let now = h.e.now_s();
    for s in 0..30 {
        h.e.sense_at(now - 30.0 + f64::from(s), true);
    }
    // rules 9: the declaration is the engine's ledger, mirrored into the track store
    let declared = |h: &H| {
        h.e.tracks
            .tracks_json(h.e.now_s())
            .into_iter()
            .find(|t| t["id"] == "T-0001")
            .and_then(|t| t["declared"]["by"].as_str().map(str::to_string))
    };
    let (s, v) = h.post(&op1, "/api/tracks/T-0001/declare", json!({ "affiliation": "hostile" })).await;
    assert_eq!((s, v["step_up"].clone()), (StatusCode::UNAUTHORIZED, json!(true)), "{v}");
    let (s, v) = h
        .post(&op1, "/api/tracks/T-0001/declare", json!({ "affiliation": "hostile", "password": "not op one's password" }))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    assert_eq!(declared(&h), None, "refused: nothing changed");
    let (s, v) = h.post(&op1, "/api/tracks/T-0001/declare", json!({ "affiliation": "neutral" })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = h
        .post(&op1, "/api/tracks/T-0001/declare", json!({ "affiliation": "hostile", "password": PW1 }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(declared(&h).as_deref(), Some("op-1"));
    let log = h.log();
    assert!(log.contains("\"action\":\"step_up.refused\",\"detail\":\"declare T-0001 hostile: no password given\""), "{log}");
    let r = h.replays_authenticated();
    assert_eq!(r.stepped_acts, 1);
    h.no_secret_logged(&[&op1]);
}

/// Rules 16 with rules 8's step-up: with `accept_unknown` in `[approval] step_up_acts`,
/// accepting a lost contact takes the same step-up as accepting a lost vehicle of ours, through
/// the API whatever the console does: a nomination accepting one without the password, or with
/// a wrong one, is refused (401, the console asked for it) and nothing is accepted or
/// nominated; with it, the nomination waits for its signers and `engage.accepted` names the
/// contact and quotes the step-up. An acceptance for a waiting nomination is refused the same.
#[tokio::test]
async fn accepting_a_lost_contact_steps_up_through_the_api() {
    let h = harness_with(Duration::from_secs(1800), &[], |p| {
        p.approval.step_up_acts = Some(vec![murmur_decide::StepUpAct::AcceptUnknown]);
    });
    let op1 = h.login("op-1", PW1).await;
    let op2 = h.login("op-2", PW2).await;
    let (_, _, me) = h.call("GET", "/api/auth/session", None, &[("cookie", &op2)]).await;
    assert_eq!(me["step_up_acts"], json!(["accept_unknown"]), "{me}");
    let since = h.picture_with_lost(&op1).await;
    let body = |extra: Value| {
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
    let nominated = |h: &H| h.e.r.events("intent.created");
    let before = nominated(&h);
    // no step-up: refused, the console told to ask for it; nothing accepted or nominated
    let (s, v) = h.post(&op2, "/api/intents", body(json!({}))).await;
    assert_eq!((s, v["step_up"].clone(), v["factor"].clone()), (StatusCode::UNAUTHORIZED, json!(true), json!("password")), "{v}");
    // a wrong password: refused too
    let (s, v) = h.post(&op2, "/api/intents", body(json!({ "password": "not op two's password" }))).await;
    assert_eq!((s, v["step_up"].clone()), (StatusCode::UNAUTHORIZED, json!(true)), "{v}");
    assert_eq!(nominated(&h), before, "nothing nominated");
    assert_eq!(h.e.r.events("engage.accepted"), 0, "nothing accepted");
    // the password again: accepted with the nomination, which waits for its signers
    let (s, v) = h.post(&op2, "/api/intents", body(json!({ "password": PW2 }))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["intent"]["state"], "pending_approval", "{v}");
    assert_eq!(nominated(&h), before + 1);
    let id = v["intent"]["id"].as_u64().expect("id");
    let log = h.log();
    let accepted = log.lines().find(|l| l.contains("\"type\":\"engage.accepted\"")).expect("accepted").to_string();
    assert!(accepted.contains("\"contact\":\"T-0002\"") && accepted.contains("\"by\":\"op-2\",\"session\":"), "{accepted}");
    assert!(accepted.contains("\"step_up\":"), "the step-up quoted: {accepted}");
    assert!(log.contains("\"action\":\"step_up.refused\",\"detail\":\"accept_unknown: no password given\""), "{log}");
    assert!(log.contains("\"action\":\"step_up.refused\",\"detail\":\"accept_unknown: wrong password\""), "{log}");
    // the acceptance for a waiting nomination is stepped up the same (refused before the engine)
    let (s, v) = h.post(&op2, &format!("/api/intents/{id}/accept_unknown"), json!({ "track": "T-0002", "since": since })).await;
    assert_eq!((s, v["step_up"].clone()), (StatusCode::UNAUTHORIZED, json!(true)), "{v}");
    assert!(h.log().contains(&format!("\"detail\":\"i{id} accept_unknown: no password given\"")), "{}", h.log());
    let r = h.replays_authenticated();
    assert_eq!(r.lost_contacts, 1);
    h.no_secret_logged(&[&op1, &op2]);
}

/// Rules 16 with rules 8's step-up, a nomination already waiting: `POST
/// /api/intents/{id}/accept_unknown` with `{"track", "since", "password"}` takes the password with
/// the contact (the review found it refused as an unknown field, so the acceptance could never
/// be made where the policy steps it up): without it 401, with it the acceptance is logged under
/// the operator's session quoting the step-up.
#[tokio::test]
async fn accepting_a_lost_contact_for_a_waiting_nomination_takes_the_password() {
    let h = harness_with(Duration::from_secs(1800), &[], |p| {
        p.approval.step_up_acts = Some(vec![murmur_decide::StepUpAct::AcceptUnknown]);
    });
    let op1 = h.login("op-1", PW1).await;
    let op2 = h.login("op-2", PW2).await;
    let (id, since) = h.waiting_then_lost(&op1).await;
    let path = format!("/api/intents/{id}/accept_unknown");
    let (s, v) = h.post(&op2, &path, json!({ "track": "T-0002", "since": since })).await;
    assert_eq!((s, v["step_up"].clone()), (StatusCode::UNAUTHORIZED, json!(true)), "{v}");
    // anything else beside the contact and the step-up is still refused
    let (s, v) = h.post(&op2, &path, json!({ "track": "T-0002", "since": since, "password": PW2, "extra": 1 })).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert_eq!(h.e.r.events("engage.accepted"), 0, "nothing accepted");
    let (s, v) = h.post(&op2, &path, json!({ "track": "T-0002", "since": since, "password": PW2 })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let log = h.log();
    let accepted = log.lines().find(|l| l.contains("\"type\":\"engage.accepted\"")).expect("accepted").to_string();
    assert!(accepted.contains(&format!("\"intent\":{id},\"contact\":\"T-0002\"")), "{accepted}");
    assert!(accepted.contains("\"by\":\"op-2\",\"session\":") && accepted.contains("\"step_up\":"), "{accepted}");
    let r = h.replays_authenticated();
    assert_eq!(r.lost_contacts, 1);
    h.no_secret_logged(&[&op1, &op2]);
}

/// Rules 9 in a credentials run: the engine's rule declaration carries no session; an
/// operator's confirmation, a flag's resolution and a revocation each go through the signed-in
/// session (no cookie, another origin or an agent's key: refused, nothing logged). With
/// `confirm_declaration` listed in `[approval] step_up_acts` (off by default) a confirmation
/// re-enters the password.
#[tokio::test]
async fn a_rule_declaration_is_confirmed_and_revoked_under_the_operators_session() {
    let h = harness_with(Duration::from_secs(1800), &[], |p| {
        super::identification::rule_on(p, false);
        p.approval.step_up_acts = Some(vec![murmur_decide::StepUpAct::ConfirmDeclaration]);
    });
    let op1 = h.login("op-1", PW1).await;
    let (_, _, me) = h.call("GET", "/api/auth/session", None, &[("cookie", &op1)]).await;
    assert_eq!(me["step_up_acts"], json!(["confirm_declaration"]), "{me}");
    super::identification::picture(&h.e).await;
    super::identification::act_now(&h.e, None);
    let (s, _, review) = h.call("GET", "/api/review", None, &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(review["declarations"][0]["declared_by"], "rule:hostile_act", "{review}");
    let log = h.log();
    let declared = log.lines().find(|l| l.contains("\"type\":\"ident.declared\"")).expect("declared");
    assert!(!declared.contains("\"session\""), "the engine's, under no session: {declared}");
    // refused without a session, from another origin, or with an agent's key
    let bearer = format!("Bearer {}", h.key);
    let before = h.log().lines().count();
    for headers in [
        vec![],
        vec![("origin", "http://evil.example"), ("cookie", op1.as_str())],
        vec![("authorization", bearer.as_str())],
    ] {
        for path in ["/api/tracks/T-0001/confirm", "/api/tracks/T-0001/revoke"] {
            let (s, _, v) = h.call("POST", path, Some(json!({})), &headers).await;
            assert!(s == StatusCode::UNAUTHORIZED || s == StatusCode::FORBIDDEN, "{path} {headers:?}: {s} {v}");
        }
    }
    assert_eq!(h.log().lines().count(), before, "nothing logged");
    // the listed step-up: no password, nothing changes; the password, confirmed
    let (s, v) = h.post(&op1, "/api/tracks/T-0001/confirm", json!({})).await;
    assert_eq!((s, v["step_up"].clone()), (StatusCode::UNAUTHORIZED, json!(true)), "{v}");
    let (s, v) = h.post(&op1, "/api/tracks/T-0001/confirm", json!({ "password": PW1 })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["declared"]["confirmed_by"], "op-1", "{v}");
    let confirmed = h.log().lines().find(|l| l.contains("\"type\":\"ident.confirmed\"")).expect("logged").to_string();
    assert!(confirmed.contains("\"by\":\"op-1\",\"session\":"), "{confirmed}");
    assert!(confirmed.contains("\"step_up\":"), "{confirmed}");
    // another operator revokes it under their own session (never a step-up: it only restricts)
    let op3 = h.login("op-3", PW3).await;
    let (s, v) = h.post(&op3, "/api/tracks/T-0001/revoke", json!({ "note": "not the shooter" })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["declared"].is_null(), "{v}");
    let revoked = h.log().lines().find(|l| l.contains("\"type\":\"ident.revoked\"")).expect("logged").to_string();
    assert!(revoked.contains("\"by\":\"op-3\",\"session\":"), "{revoked}");
    let r = h.replays_authenticated();
    assert_eq!((r.rule_declarations, r.confirmations, r.revocations), (1, 1, 1));
    h.no_secret_logged(&[&op1, &op3]);
}

/// Review, fix 1: the simulation channel is the stack's contacts process's alone. With an
/// agent's key, or no credential at all, an injected truth message carrying a hostile act is
/// refused before anything reads it (the review's probe got a rule declaration this way);
/// the rig's bridge has no token configured, so the channel is closed to everyone.
#[tokio::test]
async fn an_injected_act_on_the_simulation_channel_is_refused() {
    let h = harness_with(Duration::from_secs(1800), &[], |p| {
        super::identification::rule_on(p, false);
    });
    super::identification::picture(&h.e).await;
    let now = h.e.now_s() + 1.0;
    let msg = json!({
        "t": now, "s": 2000.0, "seq": 1, "scenario": "injected", "seed": 1,
        "sensing": { "misclassification_rate": 0.0 },
        "contacts": [{
            "id": "fastboat", "name": "x", "affiliation": "hostile", "domain": "surface",
            "kind": "small_fast_boat", "lat": TARGET.0, "lon": TARGET.1, "alt_m": 0.0,
            "speed_mps": 0.0, "course_deg": 0.0, "emits": false, "signature": 1.0
        }],
        "acts": [{ "id": "evil", "s": 2000.0, "t": now, "by": "fastboat", "lat": TARGET.0, "lon": TARGET.1, "against_vehicle": 1 }],
    });
    let bearer = format!("Bearer {}", h.key);
    for headers in [vec![], vec![("authorization", bearer.as_str())], vec![("x-murmur-sim-token", "guess")]] {
        let (s, _, v) = h.call("POST", "/api/sim/contacts", Some(msg.clone()), &headers).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{headers:?}: {v}");
    }
    h.e.r.d.tick();
    assert!(!h.log().contains("ident.act"), "nothing reached the engine");
    let (_, _, review) = h.call("GET", "/api/review", None, &[]).await;
    assert_eq!(review["counts"]["declarations"], 0, "{review}");
}
