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
    // one set of stores for the API and the agents' tasking tools, as main.rs shares them with
    // the tools on (MURMUR_TASKING_TOOLS=1)
    let ops = crate::mcp::OperatorStores::default();
    ops.tasking.write().unwrap_or_else(|e| e.into_inner()).keep_replies();
    let api = crate::api::router(crate::api::App {
        groups: ops.groups.clone(),
        tasking: ops.tasking.clone(),
        world: e.r.world.clone(),
        frames: Arc::new(watch::channel(None).0),
        decision: e.r.d.clone(),
        tracks: e.tracks.clone(),
        lod: crate::push::LodSettings::default(),
        auth: auth.clone(),
    });
    let agents = crate::mcp::Settings { agents: vec![AgentId::new("agent-planner")], keys: None, idle: crate::mcp::IDLE_LIMIT, operator: Some(ops) };
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

#[tokio::test(start_paused = true)]
async fn the_officer_acknowledges_tasking_forward_only_and_can_change_nothing_else() {
    let h = harness();
    let before = h.log();
    h.call("POST", "/api/tasking", Some(json!({ "text": "watch the ferry lane" })), &OP).await;
    let ack = |state: &str, wake: u64| json!({ "state": state, "wake": wake });

    // before any agent reads it: no officer; then an agent's read is remembered, an operator's is not
    let (_, _, v) = h.call("GET", "/api/tasking", None, &OP).await;
    assert_eq!((v["officer"].clone(), v["items"][0]["delivery"].clone()), (json!(null), json!("posted")), "{v}");
    let (_, _, v) = h.call("GET", "/api/tasking", None, &AGENT).await;
    assert_eq!(v["officer"]["agent"], json!("agent-planner"), "{v}");
    assert!(v["officer"]["read_t"].is_u64(), "{v}");

    // only an agent acknowledges; an operator, nobody, or an id not in MURMUR_AGENTS cannot
    for who in [&OP[..], &[][..], &[("x-murmur-agent", "agent-nobody")][..]] {
        let (s, _, _) = h.call("POST", "/api/tasking/tk-1/ack", Some(ack("seen", 1)), who).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    // an unknown state, a missing wake, a field that is not the acknowledgement's: 400; a bad id: 404
    for bad in [json!({ "state": "read", "wake": 1 }), json!({ "state": "seen" }), json!({ "state": "seen", "wake": 1, "text": "x" }),
                json!({ "state": "seen", "wake": 1, "active": false }), json!({ "state": "waiting" })] {
        let (s, _, v) = h.call("POST", "/api/tasking/tk-1/ack", Some(bad.clone()), &AGENT).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad} {v}");
    }
    let (s, _, _) = h.call("POST", "/api/tasking/tk-9/ack", Some(ack("seen", 1)), &AGENT).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // forward: seen, then processed; never back
    let (s, _, v) = h.call("POST", "/api/tasking/tk-1/ack", Some(ack("seen", 3)), &AGENT).await;
    assert_eq!((s, v["delivery"].clone(), v["seen_wake"].clone(), v["officer"].clone()), (StatusCode::OK, json!("seen"), json!(3), json!("agent-planner")), "{v}");
    let (s, _, v) = h.call("POST", "/api/tasking/tk-1/ack", Some(json!({ "state": "failed", "wake": 3, "why": "the turn ended error" })), &AGENT).await;
    assert_eq!((s, v["failed"]["why"].clone()), (StatusCode::OK, json!("the turn ended error")), "{v}");
    let (s, _, v) = h.call("POST", "/api/tasking/tk-1/ack", Some(ack("processed", 4)), &AGENT).await;
    assert_eq!((s, v["delivery"].clone(), v["processed_wake"].clone()), (StatusCode::OK, json!("processed"), json!(4)), "{v}");
    for back in [ack("seen", 5), ack("processed", 5), json!({ "state": "waiting", "why": "x" })] {
        let (s, _, _) = h.call("POST", "/api/tasking/tk-1/ack", Some(back), &AGENT).await;
        assert_eq!(s, StatusCode::CONFLICT);
    }

    // the agent that acknowledges still cannot post, retire or edit
    let (s, _, _) = h.call("POST", "/api/tasking", Some(json!({ "text": "x" })), &AGENT).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, _) = h.call("POST", "/api/tasking/tk-1/deactivate", None, &AGENT).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (_, _, v) = h.call("GET", "/api/tasking", None, &[]).await;
    let i = &v["items"][0];
    assert_eq!((i["text"].clone(), i["by"].clone(), i["active"].clone(), i["delivery"].clone()), (json!("watch the ferry lane"), json!("op-1"), json!(true), json!("processed")), "{v}");

    // a retired item keeps its last state
    let (_, _, v) = h.call("POST", "/api/tasking/tk-1/deactivate", None, &OP).await;
    assert_eq!((v["active"].clone(), v["delivery"].clone()), (json!(false), json!("processed")), "{v}");

    assert_eq!(h.log(), before, "acknowledgements and reads are not logged");
}

/// A tool call over the mounted `/mcp` as agent-planner (a session of its own).
async fn tool(h: &H, name: &str, args: serde_json::Value) -> serde_json::Value {
    let init = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "agent-planner", "version": "0" } },
    });
    let (s, hd, v) = h.call("POST", "/mcp", Some(init), &AGENT).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let session = hd.get("mcp-session-id").and_then(|v| v.to_str().ok()).expect("session").to_string();
    let with = [AGENT[0], ("mcp-session-id", session.as_str())];
    h.call("POST", "/mcp", Some(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })), &with).await;
    let body = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": name, "arguments": args } });
    let (s, _, v) = h.call("POST", "/mcp", Some(body), &with).await;
    assert_eq!(s, StatusCode::OK, "{name}: {v}");
    v["result"]["structuredContent"].clone()
}

#[tokio::test(start_paused = true)]
async fn the_officers_groups_and_replies_reach_the_console_marked_as_its_own_and_are_logged_nowhere() {
    let h = harness();
    for uid in 1..=3 {
        h.e.r.vehicle(uid, Firmware::Copter, "switchblade", (37.8120 + 0.0001 * uid as f64, -122.4950));
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    h.call("POST", "/api/tasking", Some(json!({ "text": "split the three: one alone, two together" })), &OP).await;
    h.call("POST", "/api/groups", Some(json!({ "name": "Operator's own", "members": [1] })), &OP).await; // og-1
    // an agent's first connection is logged once (agent.action session.connected), as ever
    let read = tool(&h, "get_groups", json!({})).await;
    assert_eq!(read["count"], 1, "{read}");
    let before = h.log();

    // the officer's group (MCP only), marked as its own for tk-1
    let made = tool(&h, "make_group", json!({ "tasking_id": "tk-1", "name": "Pair", "members": [2, 3] })).await;
    assert_eq!(made["group"]["id"], "og-2", "{made}");
    let (_, _, v) = h.call("GET", "/api/groups", None, &[]).await;
    let (op, ag) = (&v["groups"][0], &v["groups"][1]);
    assert_eq!((op["by"].clone(), op.get("agent").cloned()), (json!("op-1"), None), "{v}");
    assert_eq!((ag["by"].clone(), ag["agent"].clone(), ag["tasking"].clone()), (json!("agent-planner"), json!(true), json!("tk-1")), "{v}");
    // the operator's routes stay closed to it, for its own group too
    let (s, _, _) = h.call("PATCH", "/api/groups/og-2", Some(json!({ "add": [1] })), &AGENT).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, _) = h.call("DELETE", "/api/groups/og-2", None, &AGENT).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // it cannot change the operator's group through its tool either
    let refused = tool(&h, "make_group", json!({ "tasking_id": "tk-1", "group_id": "og-1", "members": [2] })).await;
    assert_eq!(refused["status"], 403, "{refused}");

    // its answer, under the item
    let a = tool(&h, "reply_tasking", json!({ "tasking_id": "tk-1", "status": "done", "text": "Made two groups\n- og-2: the pair" })).await;
    assert_eq!(a["item"]["replies"][0]["groups"], json!(["og-2"]), "{a}");
    let (_, _, v) = h.call("GET", "/api/tasking", None, &[]).await;
    let r = &v["items"][0]["replies"][0];
    assert_eq!((r["status"].clone(), r["agent"].clone(), r["text"].clone()), (json!("done"), json!("agent-planner"), json!("Made two groups\n- og-2: the pair")), "{v}");
    assert_eq!(v["items"][0]["delivery"], "posted", "a reply moves no delivery state");
    assert_eq!(v["officer_replies"], true, "the console can tell an item not answered yet");

    // the operator may change and delete the officer's group; once changed, it is theirs
    let (s, _, v) = h.call("PATCH", "/api/groups/og-2", Some(json!({ "add": [1] })), &OP).await;
    assert_eq!((s, v["changed_by"].clone(), v["by"].clone()), (StatusCode::OK, json!("op-1"), json!("agent-planner")), "{v}");
    let refused = tool(&h, "make_group", json!({ "tasking_id": "tk-1", "group_id": "og-2", "name": "Mine again" })).await;
    assert_eq!(refused["status"], 403, "{refused}");
    let (s, _, _) = h.call("DELETE", "/api/groups/og-2", None, &OP).await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // an operator's write is never taken for an agent's: no tool call is an operator's request
    let (s, _, _) = h.call("POST", "/api/tasking", Some(json!({ "text": "x" })), &AGENT).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(h.log(), before, "the officer's groups and replies are not logged");
}
