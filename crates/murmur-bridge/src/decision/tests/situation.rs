//! Situation status through the bridge's whole HTTP surface, as `main` serves it
//! (`crate::situation`): the runner's document is taken only with the stack's simulation
//! token, is served back to the console, and never reaches the decision engine: no audit
//! event, no intent, nothing in the world or the tracks, nothing an agent reads over MCP, and
//! the log replays as before.

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
    let agents = crate::mcp::Settings { agents: vec![AgentId::new("agent-planner")], keys: None, idle: crate::mcp::IDLE_LIMIT };
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

/// The operator's dry run a situation's runner asks before a scripted id submits an opening
/// order: the tier and whether the submission would be its own approval, nothing created and
/// nothing logged.
#[tokio::test(start_paused = true)]
async fn an_operators_dry_run_says_whether_the_submission_would_authorize_and_logs_nothing() {
    let h = harness();
    // an armed platform with the endurance for an orbit where it is
    h.e.r.vehicle(1, Firmware::Copter, "switchblade", (37.8120, -122.4950));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = h.log();
    let op = [("x-murmur-operator", "op-1")];
    // an armed platform: a human tier the submitter authorizes
    let (s, _, v) = h
        .call("POST", "/api/intents/preview", Some(json!({ "task": "orbit", "uids": [1], "center": [-122.495, 37.812], "radius": 100 })), &op)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["policy"]["tier"], "human", "{v}");
    assert_eq!(v["would_reach"], "approved", "{v}");
    assert_eq!(v["submission_authorizes"], true);
    // a hold: the safety tier, by policy
    let (s, _, v) = h.call("POST", "/api/intents/preview", Some(json!({ "task": "hold", "uids": [1] })), &op).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["would_reach"], "auto_approved", "{v}");
    assert_eq!(v["submission_authorizes"], false);
    // not a vehicle task, an unknown vehicle, no operator
    let (s, _, _) = h.call("POST", "/api/intents/preview", Some(json!({ "task": "engage", "uids": [1] })), &op).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _, _) = h.call("POST", "/api/intents/preview", Some(json!({ "task": "hold", "uids": [9] })), &op).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = h.call("POST", "/api/intents/preview", Some(json!({ "task": "hold", "uids": [1] })), &[]).await;
    assert!(s.is_client_error(), "{s}");
    assert_eq!(h.log(), before, "a dry run is not logged");
    assert_eq!(h.e.r.d.intents_json()["intents"].as_array().map(Vec::len), Some(0), "nothing created");
}

#[tokio::test]
async fn a_situation_status_never_reaches_the_decision_engine() {
    let h = harness();
    let before = h.log();
    let intents = h.e.r.d.intents_json();
    // what a revealed status may carry: a director's note about truth
    let doc = json!({
        "title": "Harbor watch",
        "reveal": true,
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
    // and nothing of it reaches an agent: its fleet, its events, its resources
    let agent = [("x-murmur-agent", "agent-planner")];
    let init = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "agent-planner", "version": "0" } },
    });
    let (s, hd, v) = h.call("POST", "/mcp", Some(init), &agent).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let session = hd.get("mcp-session-id").and_then(|v| v.to_str().ok()).expect("session").to_string();
    let with = [("x-murmur-agent", "agent-planner"), ("mcp-session-id", session.as_str())];
    let (s, _, _) = h.call("POST", "/mcp", Some(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })), &with).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let mut reads = Vec::new();
    for (n, tool) in ["get_fleet", "get_recent_events", "get_inbox", "get_tracks"].iter().enumerate() {
        let body = json!({ "jsonrpc": "2.0", "id": 10 + n, "method": "tools/call", "params": { "name": tool, "arguments": {} } });
        reads.push((tool.to_string(), h.call("POST", "/mcp", Some(body), &with).await));
    }
    for (n, uri) in ["murmur://fleet", "murmur://events", "murmur://inbox"].iter().enumerate() {
        let body = json!({ "jsonrpc": "2.0", "id": 20 + n, "method": "resources/read", "params": { "uri": uri } });
        reads.push((uri.to_string(), h.call("POST", "/mcp", Some(body), &with).await));
    }
    for (what, (s, _, v)) in reads {
        assert_eq!(s, StatusCode::OK, "{what}: {v}");
        assert!(v.get("result").is_some(), "{what}: {v}");
        let text = v.to_string();
        assert!(!text.contains("fast boat") && !text.contains("Harbor watch"), "{what}: {text}");
    }
    assert!(!h.log().contains("Harbor watch"), "the audit log never names the situation");
    let r = replay::replay_str(&h.log()).expect("parses");
    assert!(r.ok(), "{:?}", r.mismatches);
}
