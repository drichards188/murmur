//! Operator groups (and, below, operator tasking) through the HTTP surface: non-decision
//! stores that an operator writes, an agent may only read, and that never reach the audit log.

use axum::http::StatusCode;
use murmur_auth::CredentialFile;
use murmur_decide::AuthMode;
use serde_json::json;
use tokio::sync::watch;

use super::auth::H;
use super::engagement::engage_rig_auth;
use super::*;
use crate::auth::{Auth, Settings as AuthSettings, default_origins};

fn harness() -> H {
    let e = engage_rig_auth(None, PathBuf::from("memory"), AuthMode::Dev);
    let auth = Auth::new(AuthSettings::dev(), e.r.d.clone(), CredentialFile::default()).expect("auth");
    let api = crate::api::router(crate::api::App {
        groups: Default::default(),
        tasking: Default::default(),
        world: e.r.world.clone(),
        frames: Arc::new(watch::channel(None).0),
        decision: e.r.d.clone(),
        tracks: e.tracks.clone(),
        lod: crate::push::LodSettings::default(),
        auth: auth.clone(),
    });
    let agents = crate::mcp::Settings { agents: vec![AgentId::new("agent-planner")], keys: None, idle: crate::mcp::IDLE_LIMIT };
    let app = crate::app_router(api, agents, e.r.world.clone(), e.r.d.clone(), auth.clone(), Arc::new(default_origins(5173, 8765)));
    H { app, e, auth, file: PathBuf::new(), key: "no key".into() }
}

const OP: [(&str, &str); 1] = [("x-murmur-operator", "op-1")];
const AGENT: [(&str, &str); 1] = [("x-murmur-agent", "agent-planner")];
/// an agent's key beside an operator it names: still an agent
const KEYED: [(&str, &str); 2] = [("x-murmur-operator", "op-1"), ("authorization", "Bearer some-key")];

#[tokio::test(start_paused = true)]
async fn groups_are_made_edited_and_deleted_by_an_operator_and_logged_nowhere() {
    let h = harness();
    h.e.r.vehicle(1, Firmware::Copter, "switchblade", (37.8120, -122.4950));
    h.e.r.vehicle(2, Firmware::Copter, "switchblade", (37.8121, -122.4950));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = h.log();

    let (s, _, v) = h.call("GET", "/api/groups", None, &[]).await;
    assert_eq!((s, v["groups"].as_array().map(Vec::len)), (StatusCode::OK, Some(0)), "{v}");

    let (s, _, v) = h.call("POST", "/api/groups", Some(json!({ "name": "North", "members": [2, 1, 2, 9] })), &OP).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!((v["id"].clone(), v["members"].clone(), v["dropped"].clone()), (json!("og-1"), json!([2, 1]), json!([9])));

    // no operator, an agent, an empty name
    let (s, _, _) = h.call("POST", "/api/groups", Some(json!({ "name": "x" })), &[]).await;
    assert!(s.is_client_error() && s != StatusCode::FORBIDDEN, "{s}");
    for who in [&AGENT[..], &KEYED[..]] {
        let (s, _, _) = h.call("POST", "/api/groups", Some(json!({ "name": "x" })), who).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    let (s, _, _) = h.call("POST", "/api/groups", Some(json!({ "name": "  " })), &OP).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let (s, _, v) = h.call("PATCH", "/api/groups/og-1", Some(json!({ "remove": [2], "color": "#c49bff", "name": "N" })), &OP).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!((v["members"].clone(), v["color"].clone(), v["name"].clone()), (json!([1]), json!("#c49bff"), json!("N")));
    let (s, _, _) = h.call("PATCH", "/api/groups/og-1", Some(json!({ "add": [2] })), &AGENT).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, _) = h.call("PATCH", "/api/groups/og-7", Some(json!({ "add": [2] })), &OP).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // an agent reads
    let (s, _, v) = h.call("GET", "/api/groups", None, &AGENT).await;
    assert_eq!((s, v["groups"][0]["members"].clone()), (StatusCode::OK, json!([1])), "{v}");

    let (s, _, _) = h.call("DELETE", "/api/groups/og-1", None, &AGENT).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, _) = h.call("DELETE", "/api/groups/og-1", None, &OP).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _, _) = h.call("DELETE", "/api/groups/og-1", None, &OP).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    assert_eq!(h.log(), before, "groups are not logged");
}

#[tokio::test(start_paused = true)]
async fn tasking_is_posted_by_an_operator_read_by_anyone_and_logged_nowhere() {
    let h = harness();
    let before = h.log();
    let (s, _, v) = h.call("POST", "/api/tasking", Some(json!({ "text": " watch the ferry lane " })), &OP).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!((v["id"].clone(), v["text"].clone(), v["by"].clone(), v["active"].clone()), (json!("tk-1"), json!("watch the ferry lane"), json!("op-1"), json!(true)));
    h.call("POST", "/api/tasking", Some(json!({ "text": "hold the north pickets" })), &OP).await;

    for who in [&AGENT[..], &KEYED[..]] {
        let (s, _, _) = h.call("POST", "/api/tasking", Some(json!({ "text": "x" })), who).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    let (s, _, _) = h.call("POST", "/api/tasking", Some(json!({ "text": "x" })), &[]).await;
    assert!(s.is_client_error() && s != StatusCode::FORBIDDEN, "{s}");
    for bad in [json!({ "text": "   " }), json!({ "text": "x".repeat(501) })] {
        let (s, _, _) = h.call("POST", "/api/tasking", Some(bad), &OP).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    // an agent reads, newest first
    let (s, _, v) = h.call("GET", "/api/tasking", None, &AGENT).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!((v["items"][0]["id"].clone(), v["items"][1]["id"].clone()), (json!("tk-2"), json!("tk-1")));

    let (s, _, _) = h.call("POST", "/api/tasking/tk-1/deactivate", None, &AGENT).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, v) = h.call("POST", "/api/tasking/tk-1/deactivate", None, &OP).await;
    assert_eq!((s, v["active"].clone()), (StatusCode::OK, json!(false)), "{v}");
    let (s, _, _) = h.call("POST", "/api/tasking/tk-9/deactivate", None, &OP).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, _, v) = h.call("GET", "/api/tasking", None, &[]).await;
    assert_eq!(v["items"].as_array().map(Vec::len), Some(2), "deactivated items stay in the feed");

    assert_eq!(h.log(), before, "tasking is not logged");
}
