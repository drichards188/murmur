//! Situation status through the bridge's whole HTTP surface, as `main` serves it
//! (`crate::situation`): the runner's document is taken only with the stack's simulation
//! token, is served back to the console, and never reaches the decision engine: no audit
//! event, no intent, nothing in the world or the tracks, and the log replays as before.

use axum::http::StatusCode;
use murmur_auth::CredentialFile;
use murmur_decide::AuthMode;
use serde_json::json;
use tokio::sync::watch;

use super::auth::{CONSOLE, H};
use super::engagement::engage_rig_auth;
use super::*;
use crate::auth::{Auth, Settings as AuthSettings, default_origins};

const TOKEN: &str = "situation-surface-token";

/// A dev run on the engagement rig with the situation routes merged beside the API, as in
/// `main`.
fn harness() -> H {
    let e = engage_rig_auth(None, PathBuf::from("memory"), AuthMode::Dev);
    let auth = Auth::new(AuthSettings::dev(), e.r.d.clone(), CredentialFile::default()).expect("auth");
    let api = crate::api::router(crate::api::App {
        world: e.r.world.clone(),
        frames: Arc::new(watch::channel(None).0),
        decision: e.r.d.clone(),
        tracks: e.tracks.clone(),
        lod: crate::push::LodSettings::default(),
        auth: auth.clone(),
    })
    .merge(crate::situation::router(crate::situation::Situation::new(Some(TOKEN.into()))));
    let agents = crate::mcp::Settings { agents: Vec::new(), keys: None };
    let app = crate::app_router(
        api,
        agents,
        e.r.world.clone(),
        e.r.d.clone(),
        auth.clone(),
        Arc::new(default_origins(5173, 8765)),
    );
    H { app, e, auth, file: PathBuf::new(), key: "no key".into() }
}

#[tokio::test]
async fn a_situation_status_never_reaches_the_decision_engine() {
    let h = harness();
    let before = h.log();
    let intents = h.e.r.d.intents_json();
    // what a revealed status may carry: a director's note about truth
    let doc = json!({
        "title": "Harbor watch",
        "brief": "Keep the Gate under watch.",
        "director": "a fast boat without AIS comes in from the west at 2:30",
        "timeline": { "done": [{ "t_s": 12.0, "kind": "phase", "text": "fleet ready" }] },
    });
    // without the stack's token it is refused, from a script or from the console's origin
    let (s, _, v) = h.call("POST", "/api/sim/situation", Some(doc.clone()), &[]).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    let (s, _, v) = h.call("POST", "/api/sim/situation", Some(doc.clone()), &[("origin", CONSOLE)]).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    // a browser on another origin is turned away before that
    let (s, _, _) = h
        .call("POST", "/api/sim/situation", Some(doc.clone()), &[("origin", "http://elsewhere.example"), ("x-murmur-sim-token", TOKEN)])
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, v) = h.call("POST", "/api/sim/situation", Some(doc.clone()), &[("x-murmur-sim-token", TOKEN)]).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _, v) = h.call("GET", "/api/situation", None, &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["situation"]["title"], "Harbor watch");
    assert_eq!(v["situation"]["director"], doc["director"]);
    // nothing of it in the audit log, the intents, the world or the tracks
    assert_eq!(h.log(), before, "the status is not an audit event");
    assert_eq!(h.e.r.d.intents_json(), intents);
    for path in ["/api/state", "/api/tracks", "/api/inbox"] {
        let (s, _, v) = h.call("GET", path, None, &[]).await;
        assert_eq!(s, StatusCode::OK, "{path}");
        let text = v.to_string();
        assert!(!text.contains("fast boat") && !text.contains("Harbor watch"), "{path}: {text}");
    }
    let r = replay::replay_str(&h.log()).expect("parses");
    assert!(r.ok(), "{:?}", r.mismatches);
}
