//! Rules 30 through the bridge (docs/NODE-LOSS.md sections 4 and 5, bd murmur-zb1d.5): the review
//! lists one of ours presumed lost or flagged by hand; only an operator with engagement authority
//! confirms it lost (an agent never: 403 whatever it sends), on the silence the review showed (409
//! otherwise), before any credential is asked for; its orders end, its op stops, its owed values
//! close and their restore stops, its alerts close behind a notice; a report from it revokes the
//! loss (an alert says so) and any operator may revoke it with a note. A rules 29 bridge has none
//! of it. Every log replays.

use axum::body::Body;
use axum::http::{Request, header};
use murmur_decide::{NODE_LOSS_SINCE, StartCarry};
use serde_json::Value;
use tower::ServiceExt;

use super::engagement::{Erig, engage_rig_full};
use super::*;
use crate::tasks::tests::Watch;

/// The engagement rig (op-2 and op-3 with engagement authority) deciding under rules 30,
/// carrying `carry` at its start.
fn rig30(carry: StartCarry) -> Erig {
    engage_rig_full(
        None,
        PathBuf::from("memory"),
        murmur_decide::AuthMode::Dev,
        |p| p.rules.rules_version = NODE_LOSS_SINCE,
        None,
        None,
        EngineClock::Manual,
        Vec::new(),
        carry,
    )
}

/// The console's API on a rig, as the bridge serves it (a dev run).
fn api(e: &Erig) -> axum::Router {
    let auth = crate::auth::Auth::new(crate::auth::Settings::dev(), e.r.d.clone(), murmur_auth::CredentialFile::default()).expect("auth");
    crate::api::router(crate::api::App {
        world: e.r.world.clone(),
        frames: Arc::new(tokio::sync::watch::channel(None).0),
        decision: e.r.d.clone(),
        tracks: e.tracks.clone(),
        lod: crate::push::LodSettings::default(),
        auth,
    })
}

/// `method path` with `body` and `headers`: the status and the answer.
async fn call(api: &axum::Router, method: &str, path: &str, body: Option<Value>, headers: &[(&str, &str)]) -> (u16, Value) {
    let mut b = Request::builder().method(method).uri(path).header(header::CONTENT_TYPE, "application/json");
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let req = b.body(body.map_or_else(Body::empty, |v| Body::from(v.to_string()))).expect("request");
    let resp = api.clone().oneshot(req).await.expect("response");
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22).await.expect("body");
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn post(api: &axum::Router, who: &str, path: &str, body: Value) -> (u16, Value) {
    call(api, "POST", path, Some(body), &[("x-murmur-operator", who)]).await
}

async fn review(api: &axum::Router) -> Value {
    let (s, v) = call(api, "GET", "/api/review", None, &[]).await;
    assert_eq!(s, 200, "{v}");
    v
}

/// One second: the engine's (and the bridge's) clock, the decision loop.
async fn second(e: &Erig) {
    e.r.clock.advance_ms(1000);
    e.r.d.tick();
    sleep(Duration::from_secs(1)).await;
}

/// Cut a vehicle's link (its last packet counted now: the fakes count none, and the lost link
/// alert reads the count) and let its position go stale (past the policy's 10 s).
async fn silence(e: &Erig, cell: &VehicleCell, w: &Watch, s: u32) {
    cell.link_on_packet(0, clock::wall());
    w.set_cut(true);
    for _ in 0..s {
        second(e).await;
    }
}

fn alerts(e: &Erig) -> Vec<(String, String, String)> {
    let inbox = e.r.d.inbox_json();
    inbox["alerts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|a| {
            (
                a["kind"].as_str().unwrap_or_default().to_string(),
                a["severity"].as_str().unwrap_or_default().to_string(),
                a["uid"].to_string(),
            )
        })
        .collect()
}

fn has_alert(e: &Erig, kind: &str, uid: Uid) -> bool {
    alerts(e).iter().any(|(k, _, u)| k == kind && *u == uid.to_string())
}

fn replays(e: &Erig) {
    let r = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(r.ok(), "{:?}", r.mismatches);
}

/// A silent x10 (nothing presumes it lost) on an orbit, with a second order queued for it: an
/// operator flags it by hand; an agent (its header, its key), an operator without engagement
/// authority, a stale `since` and a missing note are refused before anything is logged; op-2
/// confirms it with a note: the queued order is voided, the orbit's op stops and the orbit ends,
/// it takes no order, its lost link alert closes behind a `node_lost` notice, and the review
/// shows it lost. Then it reports again: the loss is revoked before any decision reads it, and the
/// inbox says so (high).
#[tokio::test(start_paused = true)]
async fn only_engagement_authority_confirms_a_silent_vehicle_lost_and_a_report_revokes_it() {
    let e = rig30(StartCarry::default());
    let (cell, w) = e.r.watched(5, Firmware::Copter, "x10", (LAT, LON), quick());
    sleep(Duration::from_secs(1)).await;
    let api = api(&e);
    let orbit = e.r.d.submit(&op1(), orbit(5)).await.expect("orbit");
    assert_eq!(orbit.state, State::Dispatched, "{}", e.r.reason(orbit.id));
    for _ in 0..3 {
        second(&e).await;
    }
    // reporting: nothing to flag
    let (s, v) = post(&api, "op-1", "/api/review/nodes/5/flag", json!({ "note": "no reason" })).await;
    assert_eq!(s, 409, "{v}");
    silence(&e, &cell, &w, 15).await;
    assert!(has_alert(&e, "lost_link", 5), "{:?}", alerts(&e));
    // silence alone presumes nothing
    let r = review(&api).await;
    assert_eq!(r["node_losses"], json!([]), "{r}");
    assert_eq!(r["nodes_lost"], json!([]), "{r}");
    assert!(!e.r.log.text().contains("\"type\":\"node.presumed\""));
    // an order for it now waits in the queue
    let held = e.r.d.submit(&op1(), simple("rtl", 5)).await.expect("rtl");
    // flagged by hand (any operator, with a note): on the review list
    let (s, v) = post(&api, "op-1", "/api/review/nodes/5/flag", json!({ "note": "" })).await;
    assert_eq!(s, 422, "{v}");
    let (s, v) = post(&api, "op-1", "/api/review/nodes/5/flag", json!({ "note": "a crew saw it go down" })).await;
    assert_eq!(s, 200, "{v}");
    let r = review(&api).await;
    let item = &r["node_losses"][0];
    assert_eq!((item["uid"].clone(), item["state"].clone(), item["needs_note"].clone()), (json!(5), json!("silent"), json!(true)), "{item}");
    assert_eq!(item["actions"], json!(["confirm", "dismiss"]), "{item}");
    assert_eq!(item["flagged"]["by"], "op-1", "{item}");
    assert!(item["would_end"].as_array().is_some_and(|a| a.contains(&json!(orbit.id.0))), "{item}");
    // where it can be, as the keep-out counts it (stage 2): its orbit among the parts
    assert!(item["region"]["parts"].as_array().is_some_and(|p| !p.is_empty()), "{item}");
    let (s, nodes) = call(&api, "GET", "/api/nodes", None, &[]).await;
    assert_eq!(s, 200, "{nodes}");
    let n = &nodes["nodes"][0];
    assert_eq!((n["uid"].clone(), n["state"].clone()), (json!(5), json!("silent")), "{nodes}");
    assert_eq!(n["region"], item["region"], "one region, as of the last tick");
    assert_eq!(r["counts"]["node_losses"], 1, "{r}");
    assert_eq!(e.r.d.inbox_json()["counts"]["review"], 1);
    let since = item["since"].as_i64().expect("its last report");
    let lost = "/api/review/nodes/5/lost";
    let body = |since: i64, note: &str| json!({ "since": since, "note": note });
    let before = e.r.log.lines().len();
    // an agent, by its header or its key, whoever it names: never
    for headers in [
        vec![("x-murmur-agent", "agent-planner")],
        vec![("x-murmur-agent", "agent-planner"), ("x-murmur-operator", "op-2")],
        vec![("authorization", "Bearer some-agent-key"), ("x-murmur-operator", "op-2")],
    ] {
        let (s, v) = call(&api, "POST", lost, Some(body(since, "the agent thinks so")), &headers).await;
        assert_eq!(s, 403, "{headers:?}: {v}");
        let (s, v) = call(&api, "POST", "/api/review/nodes/5/revoke", Some(json!({ "note": "x" })), &headers).await;
        assert_eq!(s, 403, "{headers:?}: {v}");
    }
    // no engagement authority
    let (s, v) = post(&api, "op-1", lost, body(since, "a crew saw it go down")).await;
    assert_eq!(s, 403, "{v}");
    // not the silence the review showed, then no note where nothing presumes it lost
    let (s, v) = post(&api, "op-2", lost, body(since + 1, "a crew saw it go down")).await;
    assert_eq!(s, 409, "{v}");
    let (s, v) = post(&api, "op-2", lost, body(since, " ")).await;
    assert_eq!(s, 409, "{v}");
    assert!(v["detail"].as_str().unwrap_or_default().contains("nothing presumes it lost"), "{v}");
    assert!(
        e.r.log.lines()[before..].iter().all(|l| !l.contains("\"type\":\"node.lost\"")),
        "nothing confirmed"
    );
    // op-2, on what the review showed, saying why
    let (s, v) = post(&api, "op-2", lost, body(since, "a crew saw it go down")).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["lost"]["voided"], json!([held.id.0]), "{v}");
    assert_eq!(v["lost"]["ended"], json!([orbit.id.0]), "{v}");
    assert_eq!(v["lost"]["finished"], json!([orbit.id.0]), "{v}");
    assert_eq!(e.r.state(held.id), State::Voided);
    assert_eq!(e.r.state(orbit.id), State::Failed);
    // its op stopped, never reported to the engine after the engine ended it
    let ops = e.r.world.tasks.current_ops();
    let op = ops.iter().find(|o| o.uid == 5).expect("its op");
    assert_eq!(op.state, crate::tasks::OpState::Superseded, "{op:?}");
    second(&e).await;
    second(&e).await;
    assert!(!has_alert(&e, "lost_link", 5), "{:?}", alerts(&e));
    assert!(alerts(&e).contains(&("node_lost".into(), "info".into(), "5".into())), "{:?}", alerts(&e));
    let r = review(&api).await;
    assert_eq!(r["node_losses"], json!([]), "the hand flag went with the confirmation: {r}");
    let (_, nodes) = call(&api, "GET", "/api/nodes", None, &[]).await;
    let n = &nodes["nodes"][0];
    assert_eq!((n["state"].clone(), n["region"].clone()), (json!("lost"), Value::Null), "out of every keep-out: {nodes}");
    let l = &r["nodes_lost"][0];
    assert_eq!((l["uid"].clone(), l["by"].clone(), l["note"].clone()), (json!(5), json!("op-2"), json!("a crew saw it go down")), "{l}");
    assert_eq!(l["actions"], json!(["revoke"]), "{l}");
    // lost: it takes no order
    let refused = e.r.d.submit(&op1(), simple("hold", 5)).await.expect("submitted");
    assert_eq!(refused.state, State::Rejected, "{}", e.r.reason(refused.id));
    assert!(e.r.reason(refused.id).contains("confirmed lost"), "{}", e.r.reason(refused.id));
    // confirmed again: refused (409)
    let (s, v) = post(&api, "op-3", lost, body(since, "again")).await;
    assert_eq!(s, 409, "{v}");
    // it reports again: the loss is revoked (cause heard), and the inbox says so
    w.set_cut(false);
    for _ in 0..4 {
        second(&e).await;
        if e.r.log.text().contains("\"type\":\"node.loss_revoked\"") {
            break;
        }
    }
    second(&e).await;
    let log = e.r.log.text();
    let revoked = log.lines().find(|l| l.contains("\"type\":\"node.loss_revoked\"")).expect("revoked");
    assert!(revoked.contains("\"kind\":\"heard\""), "{revoked}");
    assert!(alerts(&e).contains(&("node_reported_again".into(), "high".into(), "5".into())), "{:?}", alerts(&e));
    assert!(!has_alert(&e, "node_lost", 5), "{:?}", alerts(&e));
    assert_eq!(review(&api).await["nodes_lost"], json!([]));
    replays(&e);
}

/// A loss any operator revokes, saying why (no step-up, no engagement authority: it only
/// restricts); refused without a note or for a vehicle not lost. A struck report within the
/// window and the silence after it presume the vehicle lost: the review shows the evidence, a
/// confirmation needs no note then, and a dismiss is only for a hand flag.
#[tokio::test(start_paused = true)]
async fn a_presumed_loss_is_confirmed_without_a_note_and_any_operator_revokes_it() {
    let e = rig30(StartCarry::default());
    let (cell, w) = e.r.watched(6, Firmware::Copter, "x10", (LAT, LON), quick());
    sleep(Duration::from_secs(1)).await;
    let api = api(&e);
    second(&e).await;
    cell.write(|v| v.report_struck("struck by W-1".into()));
    silence(&e, &cell, &w, 15).await;
    assert!(e.r.log.text().contains("\"type\":\"node.presumed\""), "presumed");
    let r = review(&api).await;
    let item = &r["node_losses"][0];
    assert_eq!((item["state"].clone(), item["needs_note"].clone()), (json!("presumed_lost"), json!(false)), "{item}");
    assert_eq!(item["evidence"][0]["kind"], "struck", "{item}");
    assert_eq!(item["actions"], json!(["confirm"]), "{item}");
    let (s, v) = post(&api, "op-1", "/api/review/nodes/6/dismiss", json!({})).await;
    assert_eq!(s, 409, "a presumption is the engine's: {v}");
    let since = item["since"].as_i64().expect("since");
    let (s, v) = post(&api, "op-3", "/api/review/nodes/6/lost", json!({ "since": since })).await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = post(&api, "op-1", "/api/review/nodes/6/revoke", json!({ "note": " " })).await;
    assert_eq!(s, 409, "{v}");
    let (s, v) = post(&api, "op-1", "/api/review/nodes/7/revoke", json!({ "note": "not lost" })).await;
    assert_eq!(s, 409, "{v}");
    let (s, v) = post(&api, "op-1", "/api/review/nodes/6/revoke", json!({ "note": "its crew says it is afloat" })).await;
    assert_eq!(s, 200, "{v}");
    let log = e.r.log.text();
    let revoked = log.lines().find(|l| l.contains("\"type\":\"node.loss_revoked\"")).expect("revoked");
    assert!(revoked.contains("\"operator\":\"op-1\""), "{revoked}");
    second(&e).await;
    assert!(!has_alert(&e, "node_reported_again", 6), "an operator's revocation is no report: {:?}", alerts(&e));
    assert!(has_alert(&e, "lost_link", 6), "silent still: {:?}", alerts(&e));
    replays(&e);
}

/// A loss closes the lost-link values owed back to the vehicle (here carried over from an
/// earlier run, never heard in this one): the restore stops and its alert closes; revoked, they
/// are owed again and the alert is back.
#[tokio::test(start_paused = true)]
async fn a_loss_closes_the_values_owed_and_stops_their_restore() {
    let values: BTreeMap<String, f64> = [("FS_GCS_ENABLE".to_string(), 1.0)].into_iter().collect();
    let owed = murmur_decide::replay::OwedRestore {
        uid: murmur_decide::Uid::new("8"),
        values: values.clone(),
        by: Vec::new(),
        vehicle: Some(murmur_decide::VehicleIdentity {
            platform: Some("x10".into()),
            callsign: Some("x10 8".into()),
            sysid: None,
            home: None,
        }),
    };
    // run-1.jsonl: a run that carried those values on from run-0.jsonl
    let run1 = MemorySink::new();
    let mut policy = Policy::load(&root().join("sim/policy.toml")).expect("policy");
    policy.rules.rules_version = NODE_LOSS_SINCE;
    murmur_decide::Engine::start(
        policy,
        None,
        murmur_decide::Roster::new(vec![OperatorId::new("op-1")], vec![]).expect("roster"),
        vec![murmur_decide::replay::CarriedFrom {
            log: "run-0.jsonl".into(),
            head: Some("0".repeat(64)),
            last_seq: Some(9),
            owed: vec![owed],
            ..Default::default()
        }],
        Box::new(ManualClock::new(snapshot::ms(T0_S).plus_ms(-60_000))),
        Box::new(run1.clone()),
    )
    .expect("run 1");
    let first = run1.text();
    let e = rig30(StartCarry {
        pointer: None,
        from: vec![super::engagement::carry_of("run-1.jsonl", &first)],
        ..Default::default()
    });
    // its link down from the start: never heard in this run
    let (q, w) = quick().watched();
    w.set_cut(true);
    let cell = crate::tasks::tests::spawn_fake(8, Firmware::Copter, LAT, LON, q);
    let mut m = serde_json::Map::new();
    m.insert("platform".into(), json!("x10"));
    m.insert("callsign".into(), json!("x10 8"));
    cell.write(|v| v.identity = Some(Arc::new(crate::vehicle::Identity::from_manifest(&m))));
    e.r.world.insert_vehicle(cell);
    // what the bridge's start does after the engine's (`Decision::load`): the carried values
    // on its lane, restored once it is heard
    e.r.d.seed_carried(None);
    for _ in 0..32 {
        second(&e).await;
    }
    assert!(has_alert(&e, "restore_pending", 8), "{:?}", alerts(&e));
    let api = api(&e);
    let (s, v) = post(&api, "op-2", "/api/review/nodes/8/lost", json!({ "since": null, "note": "sunk at its mooring last night" })).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["lost"]["closed"], json!(values), "{v}");
    second(&e).await;
    assert!(!has_alert(&e, "restore_pending", 8), "{:?}", alerts(&e));
    assert!(e.r.log.text().contains("\"closed_by\":{\"kind\":\"lost\""), "restore.closed lost");
    let (s, v) = post(&api, "op-1", "/api/review/nodes/8/revoke", json!({ "note": "it was only out of range" })).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["revoked"]["owed"], json!(values), "owed back again: {v}");
    for _ in 0..32 {
        second(&e).await;
    }
    assert!(has_alert(&e, "restore_pending", 8), "{:?}", alerts(&e));
    let logs = [("run-1.jsonl", first.as_str())];
    let r = replay::replay_str_with(
        &e.r.log.text(),
        &replay::ReplayOptions {
            carried_logs: &logs,
            ..replay::ReplayOptions::default()
        },
    )
    .expect("parses");
    assert!(r.ok(), "{:?}", r.mismatches);
}

/// A rules 29 bridge serves, logs and shows what it did before: no node routes (404, nothing
/// opened or logged), no node items in the review, no step on the tick.
#[tokio::test(start_paused = true)]
async fn a_rules_29_bridge_has_none_of_it() {
    let e = super::engagement::engage_rig_auth_policy(None, PathBuf::from("memory"), murmur_decide::AuthMode::Dev, |p| {
        p.rules.rules_version = murmur_decide::HANDOVER_SINCE
    });
    let (_cell, w) = e.r.watched(5, Firmware::Copter, "x10", (LAT, LON), quick());
    sleep(Duration::from_secs(1)).await;
    let api = api(&e);
    w.set_cut(true);
    for _ in 0..15 {
        second(&e).await;
    }
    let before = e.r.log.text();
    for route in ["lost", "revoke", "flag", "dismiss"] {
        let (s, v) = post(&api, "op-2", &format!("/api/review/nodes/5/{route}"), json!({ "since": null, "note": "x" })).await;
        assert_eq!(s, 404, "{route}: {v}");
    }
    let (s, v) = call(&api, "GET", "/api/nodes", None, &[]).await;
    assert_eq!(s, 404, "{v}");
    assert_eq!(e.r.log.text(), before, "nothing opened or logged");
    let r = review(&api).await;
    assert!(r.get("node_losses").is_none() && r.get("nodes_lost").is_none(), "{r}");
    assert!(r["counts"].get("node_losses").is_none(), "{r}");
    assert!(!e.r.log.text().contains("\"type\":\"node."));
    let opts = replay::ReplayOptions { rules: replay::RulesAccepted::Exactly(murmur_decide::HANDOVER_SINCE), ..Default::default() };
    let r = replay::replay_str_with(&e.r.log.text(), &opts).expect("parses");
    assert!(r.ok(), "{:?}", r.mismatches);
}

/// Rules 30: a vehicle the fleet manifest names that has never been heard is in the snapshot
/// with who it is (platform, callsign, system id), so a loss an earlier run confirmed on it holds
/// from the start; under rules 29 the snapshot is as it was (no identity until it is heard).
#[test]
fn a_manifest_vehicle_never_heard_carries_its_identity_under_rules_30() {
    let dir = std::env::temp_dir().join(format!("murmur-node-loss-manifest-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let manifest = dir.join("fleet.json");
    std::fs::write(
        &manifest,
        json!({ "vehicles": [{ "uid": 2, "sysid": 7, "port": 2, "platform": "seahunter", "callsign": "USV 2" }] }).to_string(),
    )
    .expect("manifest");
    let world = World::new(Config {
        base_port: 0,
        http_port: 0,
        default_ports: 0,
        manifest,
        platforms: root().join("sim/platforms.toml"),
        root: root(),
    });
    world.load_manifest();
    let catalog = Catalog::load(&root().join("sim/platforms.toml")).expect("catalog");
    let areas = AreaSet::empty("test", murmur_decide::Time(0));
    let at = |node_loss: bool| snapshot::build(&world, &catalog, &areas, murmur_decide::Time(0), None, node_loss);
    let uid = murmur_decide::Uid::new("2");
    assert!(!at(false).identities.contains_key(&uid), "rules 29: as before");
    let id = at(true).identities.get(&uid).cloned().expect("rules 30: who it is");
    assert_eq!(
        (id.platform.as_deref(), id.callsign.as_deref(), id.sysid),
        (Some("seahunter"), Some("USV 2"), Some(7)),
        "{id:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
