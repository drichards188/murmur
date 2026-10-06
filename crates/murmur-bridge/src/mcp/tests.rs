//! The MCP server against a fake world: fake autopilots (the task layer's test doubles) in
//! a real `World`, the real policy, catalog and areas from `sim/`, a fixed land/water map,
//! a manual engine clock and an in-memory audit log that must replay. Tools are called the
//! way the handler calls them (a resolved agent id, a tool name, JSON arguments); one test
//! drives the mounted `/mcp` endpoint over HTTP to prove the header and session plumbing.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use murmur_decide::{
    AgentId, AreaSet, Catalog, Clock, IntentId, LonLat, ManualClock, MemorySink, OperatorId,
    Policy, Polygon, PolygonTerrain, ScenarioMap, State, Time, replay,
};
use rmcp::model::CallToolResult;
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::time::sleep;
use tower::ServiceExt;

use super::*;
use crate::api;
use crate::decision::Actor;
use crate::decision::{Parts, Terrain, snapshot};
use crate::tasks::tests::{Firmware, quick, spawn_fake};
use crate::vehicle::{Identity, Uid};
use crate::world::Config;

// Inside the approved Golden Gate area, south of the strait.
const LAT: f64 = 37.80;
const LON: f64 = -122.45;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn ring(w: f64, s: f64, e: f64, n: f64) -> Polygon {
    Polygon::new(vec![
        LonLat::new(w, s),
        LonLat::new(e, s),
        LonLat::new(e, n),
        LonLat::new(w, n),
        LonLat::new(w, s),
    ])
}

fn planner() -> AgentId {
    AgentId::new("agent-planner")
}

fn watch() -> AgentId {
    AgentId::new("agent-watch")
}

struct Rig {
    world: Arc<World>,
    d: Arc<Decision>,
    mcp: Arc<Mcp>,
    api: Router,
    clock: ManualClock,
    log: MemorySink,
    /// contacts that are not ours, as the API's track store (fed directly in tests)
    tracks: Arc<crate::tracks::Tracks>,
}

/// The sim policy as shipped.
fn rig() -> Rig {
    let root = root();
    let world = World::new(Config {
        base_port: 0,
        http_port: 0,
        default_ports: 0,
        manifest: root.join("sim/run/no-such-manifest.json"),
        platforms: root.join("sim/platforms.toml"),
        root: root.clone(),
    });
    let clock = ManualClock::new(snapshot::ms(crate::clock::wall()));
    let log = MemorySink::new();
    let terrain = Terrain::fixed(PolygonTerrain {
        coverage: ring(-122.6, 37.7, -122.3, 37.9),
        land: vec![ring(-122.48, 37.79, -122.44, 37.808)],
        water_depth_m: 12.0,
    });
    let tracks = Arc::new(
        crate::tracks::Tracks::load(&root.join("sim/platforms.toml")).expect("sensors"),
    );
    let d = Decision::from_parts(
        world.clone(),
        Parts {
            policy: Policy::load(&root.join("sim/policy.toml")).expect("policy"),
            catalog: Catalog::load(&root.join("sim/platforms.toml")).expect("catalog"),
            areas: AreaSet::load_dir(&root.join("sim/areas"), clock.now()).expect("areas"),
            sink: Box::new(log.clone()),
            audit_path: PathBuf::from("memory"),
            clock: Arc::new(clock.clone()),
            terrain,
            operators: vec![OperatorId::new("op-1"), OperatorId::new("op-2"), OperatorId::new("op-3")],
            engage_operators: vec![OperatorId::new("op-2"), OperatorId::new("op-3")],
            auth: murmur_decide::AuthMode::Dev,
            tracks: Some(tracks.clone()),
            passkeys: None,
            study: None,
            scripted: Vec::new(),
            carried_from: Vec::new(),
        },
    )
    .expect("engine");
    let api = api::router(api::App {
        world: world.clone(),
        frames: Arc::new(watch::channel(None).0),
        decision: d.clone(),
        tracks: tracks.clone(),
        lod: crate::push::LodSettings::default(),
        auth: crate::auth::Auth::new(
            crate::auth::Settings::dev(),
            d.clone(),
            murmur_auth::CredentialFile::default(),
        ),
    });
    let mcp = Mcp::new(
        Settings {
            agents: vec![planner(), watch()],
            keys: None,
        },
        world.clone(),
        d.clone(),
        api.clone(),
    );
    Rig {
        world,
        d,
        mcp,
        api,
        clock,
        log,
        tracks,
    }
}

// The element-set fixture (CelesTrak GP data of 2026-10-02) the decision layer's own space
// tests use, so a collect has real passes to choose from.
const GP_FIXTURE: &str = include_str!("../../../murmur-space/tests/data/gp_fixture.json");

fn fixture_space() -> Arc<murmur_space::Space> {
    let catalog =
        murmur_space::Catalog::parse(include_str!("../../../../sim/space/constellations.toml"))
            .expect("catalog");
    let sources: Vec<(String, String)> = catalog
        .queries()
        .into_iter()
        .map(|q| (q, GP_FIXTURE.to_string()))
        .collect();
    Arc::new(murmur_space::Space::from_sources(catalog, &sources).expect("space"))
}

/// Scenario start, on the fixture's epoch.
fn t0() -> Time {
    Time(
        chrono::DateTime::parse_from_rfc3339("2026-10-02T14:00:00Z")
            .expect("time")
            .timestamp_millis(),
    )
}

/// A tool result as (is_error, structured content).
fn unpack(r: CallToolResult) -> (bool, Value) {
    (
        r.is_error.unwrap_or(false),
        r.structured_content.unwrap_or(Value::Null),
    )
}

impl Rig {
    fn vehicle(&self, uid: Uid, fw: Firmware, platform: &str, at: (f64, f64)) {
        let cell = spawn_fake(uid, fw, at.0, at.1, quick());
        let mut m = serde_json::Map::new();
        m.insert("platform".into(), json!(platform));
        m.insert("callsign".into(), json!(format!("{platform} {uid}")));
        cell.write(|v| v.identity = Some(Arc::new(Identity::from_manifest(&m))));
        self.world.insert_vehicle(cell);
    }

    async fn call(&self, agent: &AgentId, tool: &str, args: Value) -> (bool, Value) {
        let args = args.as_object().cloned().unwrap_or_default();
        let r = tools::call(&self.mcp, agent, tool, args)
            .await
            .unwrap_or_else(|e| panic!("{tool}: protocol error {e}"));
        unpack(r)
    }

    /// A call that must succeed.
    async fn ok(&self, agent: &AgentId, tool: &str, args: Value) -> Value {
        let (err, v) = self.call(agent, tool, args).await;
        assert!(!err, "{tool} refused: {v}");
        v
    }

    fn state(&self, id: u64) -> State {
        let v = self.d.intent_json(id).expect("intent");
        serde_json::from_value(v["intent"]["state"].clone()).expect("state")
    }

    /// Tick once a second (tokio time) until the intent leaves `dispatched`.
    async fn run_until_finished(&self, id: u64, limit_s: u64) -> State {
        for _ in 0..limit_s {
            self.d.tick();
            let s = self.state(id);
            if s.is_terminal() {
                return s;
            }
            sleep(Duration::from_secs(1)).await;
        }
        self.state(id)
    }

    fn replays(&self) -> replay::ReplayReport {
        let report = replay::replay_str(&self.log.text()).expect("log parses");
        assert!(report.ok(), "replay mismatches: {:?}", report.mismatches);
        report
    }

    fn events(&self, kind: &str) -> usize {
        self.log
            .lines()
            .iter()
            .filter(|l| l.contains(&format!("\"type\":\"{kind}\"")))
            .count()
    }

    fn actions(&self, action: &str) -> usize {
        self.log
            .lines()
            .iter()
            .filter(|l| {
                l.contains("\"type\":\"agent.action\"")
                    && l.contains(&format!("\"action\":\"{action}\""))
            })
            .count()
    }
}

fn orbit(uid: Uid, rationale: &str) -> Value {
    json!({
        "task": "orbit", "uids": [uid], "center": [LON + 0.004, LAT], "radius_m": 150.0,
        "rationale": rationale,
    })
}

// ---- the guarantee -------------------------------------------------------------------

/// Stems no tool name may contain: anything that approves, authorizes, dispatches or
/// drives a vehicle.
const FORBIDDEN_NAME_STEMS: &[&str] = &[
    "approv", "reject", "authoriz", "authoris", "grant", "dispatch", "execut", "command",
    "control", "arm", "takeoff", "land", "goto", "fly", "drive", "steer", "mode", "mission",
    "waypoint", "upload", "send", "mavlink", "cancel", "override", "abort", "engage", "fire",
    "strike", "launch",
    // rules 6: accepting a lost friendly's unknown position is an operator's decision
    "accept", "acknowledg", "waive",
];

/// Phrases no tool description may contain: the tool, as the subject, approving, deciding
/// or driving. Descriptions may say what humans or policy do.
const FORBIDDEN_PHRASES: &[&str] = &[
    "approves ", "approve the", "approve an", "approve this", "approve it", "approve your",
    "grants approval", "authorizes", "authorize the", "authorise", "rejects ", "reject the",
    "dispatches", "dispatch the", "dispatch it", "executes", "execute the", "commands the",
    "command the", "flies the", "fly the", "drives the", "drive the", "steers", "sets the mode",
    "set the mode", "arms the", "arm the", "disarm", "takes off", "sends the vehicle",
    "send the vehicle", "sends a mission", "uploads a mission", "controls the", "control the",
    "directly control", "direct control", "overrides", "override the", "aborts", "abort the",
    "on behalf of the operator", "as the operator", "bypass",
];

#[test]
fn no_tool_implies_approval_or_control() {
    let tools = tools::tools();
    assert!(tools.len() >= 10);
    for t in &tools {
        let name = t.name.to_lowercase();
        for stem in FORBIDDEN_NAME_STEMS {
            assert!(
                !name.contains(stem),
                "tool name {name:?} contains {stem:?}: it implies approval or control"
            );
        }
        let description = t
            .description
            .as_deref()
            .expect("every tool is described")
            .to_lowercase();
        assert!(!description.contains('\u{2014}'), "{name}: em dash");
        for phrase in FORBIDDEN_PHRASES {
            assert!(
                !description.contains(phrase),
                "tool {name}'s description contains {phrase:?}: it implies approval or control"
            );
        }
        let read_only = t
            .annotations
            .as_ref()
            .and_then(|a| a.read_only_hint)
            .expect("every tool says whether it is read-only");
        let writer = matches!(name.as_str(), "propose_intent" | "withdraw_intent");
        assert_eq!(read_only, !writer, "{name}: read_only_hint");
    }
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    for required in [
        "get_fleet",
        "get_inbox",
        "preview_intent",
        "propose_intent",
        "explain_intent",
        "withdraw_intent",
    ] {
        assert!(names.contains(&required), "missing {required}");
    }
    // the server's own instructions say so too
    let lower = INSTRUCTIONS.to_lowercase();
    assert!(lower.contains("cannot approve"));
    assert!(lower.contains("stale"));
    assert!(!INSTRUCTIONS.contains('\u{2014}'));
    for r in resources() {
        assert!(!r.description.unwrap_or_default().contains('\u{2014}'));
    }
}

#[tokio::test(start_paused = true)]
async fn tools_that_would_approve_or_drive_do_not_exist() {
    let r = rig();
    r.vehicle(1, Firmware::Copter, "switchblade", (LAT, LON));
    sleep(Duration::from_millis(500)).await;
    let id = r.ok(&planner(), "propose_intent", orbit(1, "cover the east approach")).await["id"]
        .as_u64()
        .expect("id");
    assert_eq!(r.state(id), State::PendingApproval);
    for name in [
        "approve_intent",
        "approve",
        "reject_intent",
        "cancel_intent",
        "dispatch_intent",
        "authorize_intent",
        "set_mode",
        "arm_vehicle",
        "goto",
        "send_command",
        "open_operator_session",
        // Phase E: nothing an agent can call signs, stops or releases an engagement
        "approve_engagement",
        "authorize_engagement",
        "sign_engagement",
        "abort_engagement",
        "stop_engagement",
        "release_effect",
        "engage",
        "fire",
        "declare_track",
        "declare_hostile",
        // rules 9: identification is an operator's or the engine's rule's, never an agent's
        "confirm_declaration",
        "revoke_declaration",
        "resolve_flag",
        "dismiss_flag",
        "confirm_flag",
        "record_act",
        "report_act",
    ] {
        let args = json!({ "id": id, "uid": 1, "note": "please" })
            .as_object()
            .cloned()
            .expect("object");
        let err = tools::call(&r.mcp, &planner(), name, args)
            .await
            .expect_err(name);
        assert!(err.message.contains("unknown tool"), "{name}: {}", err.message);
    }
    assert_eq!(r.state(id), State::PendingApproval);
    assert_eq!(r.events("approval.granted"), 0);
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn no_tool_moves_a_pending_intent_toward_approval() {
    let r = rig();
    r.vehicle(2, Firmware::Copter, "switchblade", (LAT, LON)); // armed: human tier
    sleep(Duration::from_millis(500)).await;
    let id = r.ok(&planner(), "propose_intent", orbit(2, "watch the strait")).await["id"]
        .as_u64()
        .expect("id");
    assert_eq!(r.state(id), State::PendingApproval);
    // every tool, with every argument that could name the intent or the vehicle
    let mut names: Vec<String> = tools::tools()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    names.sort_by_key(|n| n == "withdraw_intent"); // last: it voids the intent, legitimately
    for name in &names {
        let mut args = orbit(2, "every tool, same arguments");
        args["id"] = json!(id);
        args["uid"] = json!(2);
        args["reason"] = json!("checking that nothing here approves");
        args["limit"] = json!(10);
        let (_, _) = r.call(&planner(), name, args).await;
        let state = r.state(id);
        assert!(
            matches!(state, State::PendingApproval | State::Voided),
            "after {name}: intent {id} is {state:?}"
        );
        assert_eq!(r.events("approval.granted"), 0, "after {name}");
        assert_eq!(r.events("approval.at_submission"), 0, "after {name}");
        assert_eq!(r.events("intent.dispatched"), 0, "after {name}");
    }
    assert_eq!(r.state(id), State::Voided, "withdrawn by its agent at the end");
    assert!(
        r.world.tasks.snapshot().is_empty(),
        "nothing reached a vehicle"
    );
    r.replays();
}

// ---- the flows ------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn an_auto_tier_proposal_dispatches_and_runs() {
    let r = rig();
    r.vehicle(1, Firmware::Copter, "x10", (LAT, LON));
    sleep(Duration::from_millis(500)).await;
    let out = r
        .ok(&planner(), "propose_intent", orbit(1, "routine coverage of the anchorage"))
        .await;
    assert_eq!(out["state"], "dispatched", "{out}");
    assert_eq!(out["dispatched"], true);
    assert_eq!(out["intent"]["source"], json!({"kind": "agent", "id": "agent-planner"}));
    assert_eq!(out["intent"]["policy"]["tier"], "auto");
    assert!(
        out["summary"].as_str().unwrap_or("").contains("policy (auto tier)"),
        "{out}"
    );
    let id = out["id"].as_u64().expect("id");
    assert_eq!(r.run_until_finished(id, 300).await, State::Done);
    assert_eq!(r.actions("intent.propose"), 1);
    assert_eq!(r.actions("session.connected"), 0, "the handler logs that, not the tool");
    let report = r.replays();
    assert_eq!(report.approvals, 0, "policy, not a human");
    let explained = r.ok(&planner(), "explain_intent", json!({ "id": id })).await;
    assert_eq!(explained["intent"]["state"], "done");
    assert!(explained["tasking"]["ops"].as_array().is_some_and(|o| !o.is_empty()));
}

#[tokio::test(start_paused = true)]
async fn a_human_tier_proposal_waits_for_an_operator_then_runs() {
    let r = rig();
    r.vehicle(2, Firmware::Copter, "switchblade", (LAT, LON));
    sleep(Duration::from_millis(500)).await;
    let out = r
        .ok(
            &planner(),
            "propose_intent",
            orbit(2, "the armed platform should watch the east approach"),
        )
        .await;
    assert_eq!(out["state"], "pending_approval", "{out}");
    assert_eq!(out["dispatched"], false);
    let id = out["id"].as_u64().expect("id");
    assert!(r.world.tasks.snapshot().is_empty(), "nothing reached the vehicle");

    // the inbox shows the agent's id and rationale, and why a human is needed
    let inbox = r.ok(&planner(), "get_inbox", json!({})).await;
    assert_eq!(inbox["counts"]["pending"], 1);
    let p = &inbox["pending"][0];
    assert_eq!(p["id"], id);
    assert_eq!(p["source"]["id"], "agent-planner");
    assert_eq!(
        p["rationale"],
        "the armed platform should watch the east approach"
    );
    assert!(p["why"].to_string().contains("armed"), "{p}");
    assert!(p["expires_in_s"].as_f64().is_some_and(|s| s > 100.0));
    let explained = r.ok(&planner(), "explain_intent", json!({ "id": id })).await;
    let summary = explained["summary"].as_str().expect("summary");
    assert!(summary.contains("waits in the operator's inbox"), "{summary}");
    assert!(summary.contains("an agent cannot approve it"), "{summary}");
    assert!(summary.contains("human (a listed operator must decide)"), "{summary}");

    // a human approves over the operator API; the checks run again and it flies
    let approved = r
        .d
        .approve(&Actor::asserted(OperatorId::new("op-1")), id, Some("cleared".into()))
        .await
        .expect("approve");
    assert_eq!(approved.state, State::Dispatched);
    assert!(approved.authorization.is_none(), "an inbox approval, not at submission");
    assert_eq!(r.run_until_finished(id, 300).await, State::Done);
    let report = r.replays();
    assert_eq!(
        (report.approvals, report.approvals_at_submission, report.self_approvals),
        (1, 0, 0)
    );
    let explained = r.ok(&planner(), "explain_intent", json!({ "id": id })).await;
    assert!(
        explained["summary"]
            .as_str()
            .expect("summary")
            .contains("Approved in the inbox by operator op-1"),
        "{explained}"
    );
}

#[tokio::test(start_paused = true)]
async fn an_agent_withdraws_only_its_own_intents() {
    let r = rig();
    r.vehicle(2, Firmware::Copter, "switchblade", (LAT, LON));
    sleep(Duration::from_millis(500)).await;
    let id = r.ok(&planner(), "propose_intent", orbit(2, "watch the strait")).await["id"]
        .as_u64()
        .expect("id");
    assert_eq!(r.state(id), State::PendingApproval);

    let (err, v) = r
        .call(&watch(), "withdraw_intent", json!({ "id": id, "reason": "not mine" }))
        .await;
    assert!(err, "{v}");
    assert_eq!(v["status"], 403);
    assert!(v["error"].to_string().contains("not its own"), "{v}");
    assert_eq!(r.state(id), State::PendingApproval);

    let v = r
        .ok(
            &planner(),
            "withdraw_intent",
            json!({ "id": id, "reason": "the picture changed" }),
        )
        .await;
    assert_eq!(v["state"], "voided");
    assert_eq!(r.state(id), State::Voided);
    assert_eq!(r.actions("intent.withdraw"), 1);
    // and not twice
    let (err, v) = r
        .call(&planner(), "withdraw_intent", json!({ "id": id, "reason": "again" }))
        .await;
    assert!(err, "{v}");
    assert_eq!(v["status"], 409);
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn preview_runs_the_checks_without_creating_an_intent() {
    let r = rig();
    r.vehicle(1, Firmware::Copter, "x10", (LAT, LON));
    r.vehicle(2, Firmware::Copter, "switchblade", (LAT, LON + 0.001));
    sleep(Duration::from_millis(500)).await;

    let auto = r.ok(&planner(), "preview_intent", orbit(1, "")).await;
    assert_eq!(auto["passed"], true, "{auto}");
    assert_eq!(auto["would_reach"], "auto_approved");
    assert_eq!(auto["policy"]["tier"], "auto");
    assert!(auto["summary"].as_str().expect("summary").starts_with("Preview only"));
    assert!(auto.get("why_human").is_none(), "no human needed: {auto}");

    let human = r.ok(&planner(), "preview_intent", orbit(2, "")).await;
    assert_eq!(human["would_reach"], "pending_approval", "{human}");
    assert!(human["why_human"].to_string().contains("armed"), "{human}");

    // a patrol straight across Alcatraz
    let bad = r
        .ok(
            &planner(),
            "preview_intent",
            json!({
                "task": "patrol", "uids": [1],
                "route": [[-122.432, 37.8265], [-122.414, 37.8265]],
            }),
        )
        .await;
    assert_eq!(bad["passed"], false, "{bad}");
    assert_eq!(bad["would_reach"], "rejected");
    assert!(bad["summary"].to_string().contains("alcatraz"), "{bad}");

    let intents = r.ok(&planner(), "get_intents", json!({})).await;
    assert_eq!(intents["count"], 0, "nothing was created");
    assert_eq!(r.actions("intent.preview"), 3);
    assert_eq!(r.events("intent.created"), 0);
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn fleet_facts_carry_age_and_staleness() {
    let r = rig();
    r.vehicle(1, Firmware::Copter, "x10", (LAT, LON));
    sleep(Duration::from_millis(500)).await;
    let fleet = r.ok(&planner(), "get_fleet", json!({})).await;
    assert_eq!(fleet["count"], 1);
    let v = &fleet["vehicles"][0];
    assert_eq!(v["uid"], 1);
    assert_eq!(v["callsign"], "x10 1");
    assert_eq!(v["platform"]["armed"], false);
    assert!(v["platform"]["tasks"].as_array().is_some_and(|t| t.contains(&json!("orbit"))));
    assert_eq!(v["position"]["stale"], false, "{v}");
    assert!(v["position"]["as_of"].as_f64().is_some());
    assert!(v["position"]["age_s"].as_f64().is_some_and(|a| a < 5.0));
    assert_eq!(v["nav"]["ready"], true);
    assert_eq!(v["nav"]["stale"], false);
    assert_eq!(v["link"]["lost"], false);
    assert_eq!(v["freshness_limit_s"], 10.0);
    assert!(fleet["note"].as_str().expect("note").contains("stale"));

    // the position report stops: the fact ages past the limit and is marked stale
    let cell = r
        .world
        .vehicles()
        .into_iter()
        .find(|c| c.uid == 1)
        .expect("cell");
    cell.write(|v| v.seen.position = Some(crate::clock::wall() - 60.0));
    let one = r.ok(&planner(), "get_vehicle", json!({ "uid": 1 })).await;
    let v = &one["vehicle"];
    assert_eq!(v["position"]["stale"], true, "{v}");
    assert!(v["position"]["age_s"].as_f64().is_some_and(|a| a >= 59.0));
    assert_eq!(v["nav"]["stale"], false, "the nav report is still fresh");

    // the same views as resources
    let fleet = read(&r.mcp, &planner(), "murmur://fleet").await.expect("resource");
    assert_eq!(fleet["count"], 1);
    let one = read(&r.mcp, &planner(), "murmur://fleet/1").await.expect("resource");
    assert_eq!(one["vehicle"]["uid"], 1);
    assert!(read(&r.mcp, &planner(), "murmur://fleet/9").await.is_err());
    assert!(read(&r.mcp, &planner(), "murmur://nothing").await.is_err());
    let (err, v) = r.call(&planner(), "get_vehicle", json!({ "uid": 9 })).await;
    assert!(err);
    assert_eq!(v["status"], 404);
}

#[tokio::test(start_paused = true)]
async fn the_other_reads_answer() {
    let r = rig();
    r.vehicle(1, Firmware::Copter, "x10", (LAT, LON));
    sleep(Duration::from_millis(500)).await;
    let areas = r.ok(&planner(), "get_areas", json!({})).await;
    assert!(areas["areas"].to_string().contains("alcatraz"));
    let policy = r.ok(&planner(), "get_policy", json!({})).await;
    assert!(policy["policy"]["tier"].as_array().is_some_and(|t| !t.is_empty()), "{policy}");
    let platforms = r.ok(&planner(), "get_platforms", json!({})).await;
    assert!(platforms["platforms"].as_array().is_some_and(|p| p.len() > 20));
    // a hold: the mode change is a vehicle event
    let out = r
        .ok(
            &planner(),
            "propose_intent",
            json!({ "task": "hold", "uids": [1], "rationale": "hold while the picture settles" }),
        )
        .await;
    let id = out["id"].as_u64().expect("id");
    assert_eq!(r.run_until_finished(id, 60).await, State::Done);
    let events = r.ok(&planner(), "get_recent_events", json!({ "uid": 1 })).await;
    assert!(events["count"].as_u64().is_some_and(|c| c >= 1), "{events}");
    assert_eq!(events["events"][0]["kind"], "mode");
    assert!(events["events"][0]["age_s"].as_f64().is_some());
    let mine = r
        .ok(&planner(), "get_intents", json!({ "mine": true }))
        .await;
    assert_eq!(mine["count"], 1);
    let theirs = r.ok(&watch(), "get_intents", json!({ "mine": true })).await;
    assert_eq!(theirs["count"], 0);
    // the space segment isn't loaded in tests: a refusal the model can read, not a crash
    let (err, v) = r
        .call(&planner(), "get_satellite_passes", json!({}))
        .await;
    assert!(err, "{v}");
    assert!(v["status"].as_u64().is_some());
    // a proposal without a rationale is refused before any intent exists
    let (err, v) = r
        .call(&planner(), "propose_intent", json!({ "task": "hold", "uids": [1] }))
        .await;
    assert!(err);
    assert!(v["error"].to_string().contains("rationale"), "{v}");
    assert_eq!(r.events("intent.created"), 1);
    r.replays();
}

#[tokio::test]
async fn tracks_are_perception_with_age_and_never_truth() {
    use crate::tracks::sensing::{Observer, SensorSpec, SensorType};
    use crate::tracks::truth::{Ais, ContactTruth, Domain, Sensing, TruthAffiliation, TruthMessage};
    let r = rig();
    // a dark fast boat 1.5 km from a boat of ours with radar and EO/IR, seen for a minute
    let observer = Observer {
        uid: 3,
        lat: LAT,
        lon: LON,
        alt_msl: 0.0,
        mast_m: 4.0,
        submerged: false,
        sensors: vec![
            SensorSpec { kind: SensorType::Radar, range_m: 8_000.0, guess: true, height: false },
            SensorSpec { kind: SensorType::EoIr, range_m: 5_000.0, guess: true, height: false },
            SensorSpec { kind: SensorType::Ais, range_m: 15_000.0, guess: true, height: false },
        ],
    };
    let contact = ContactTruth {
        id: "truth-only-contact-id".into(),
        name: "Truth-only name".into(),
        affiliation: TruthAffiliation::Hostile,
        domain: Domain::Surface,
        kind: "small_fast_boat".into(),
        lat: LAT + 0.0135,
        lon: LON,
        alt_m: 0.0,
        speed_mps: 15.0,
        course_deg: 90.0,
        emits: false,
        signature: 0.35,
        ais: Some(Ais {
            mmsi: "1".into(),
            name: "F/V COVER".into(),
            ship_type: "fishing".into(),
            spoofed: true,
        }),
    };
    let now = crate::clock::wall();
    for s in 0..60 {
        let msg = TruthMessage {
            t: now + f64::from(s),
            s: f64::from(s),
            seq: s as u64,
            scenario: "test".into(),
            seed: 1,
            sensing: Sensing { misclassification_rate: 0.0 },
            contacts: vec![contact.clone()],
            acts: Vec::new(),
        };
        r.tracks
            .on_truth_with(msg, std::slice::from_ref(&observer), now - 60.0 + f64::from(s))
            .expect("sensing");
    }
    let out = r.ok(&planner(), "get_tracks", json!({})).await;
    assert_eq!(out["count"], 1, "{out}");
    let t = &out["tracks"][0];
    assert_eq!(t["id"], "T-0001");
    for k in ["as_of", "age_s", "stale", "pos_sigma_now_m", "confidence_now", "predicted"] {
        assert!(!t[k].is_null(), "{k}: {t}");
    }
    assert!(t["age_s"].as_f64().is_some_and(|a| (0.0..5.0).contains(&a)), "{t}");
    assert_eq!(t["stale"], false);
    // the spoofed AIS is heard as an identity; the speed gives it away: suspect, not hostile
    assert_eq!(t["identity"]["ais"]["name"], "F/V COVER");
    assert_eq!(t["affiliation"], "suspect", "{t}");
    assert!(t["sources"].as_array().is_some_and(|s| s.len() >= 2));
    assert!(t["identity"]["confidence"].as_f64().is_some_and(|c| c > 0.3));
    // filters
    let none = r.ok(&planner(), "get_tracks", json!({ "affiliation": "hostile" })).await;
    assert_eq!(none["count"], 0);
    let air = r.ok(&planner(), "get_tracks", json!({ "domain": "air" })).await;
    assert_eq!(air["count"], 0);
    // the resource reads say the same
    let res = read(&r.mcp, &planner(), "murmur://tracks").await.expect("resource");
    assert_eq!(res["count"], 1);
    let one = read(&r.mcp, &planner(), "murmur://tracks/T-0001").await.expect("resource");
    assert_eq!(one["track"]["id"], "T-0001");
    assert!(read(&r.mcp, &planner(), "murmur://tracks/T-0099").await.is_err());
    // never the truth: no contact id, no truth name, no "spoofed", no truth affiliation
    for text in [out.to_string(), res.to_string(), one.to_string()] {
        assert!(!text.contains("truth-only-contact-id"), "{text}");
        assert!(!text.contains("Truth-only name"), "{text}");
        assert!(!text.contains("spoofed"), "{text}");
        assert!(!text.contains("\"hostile\""), "{text}");
    }
    // the resource is listed and described as perception
    let listed = resources();
    let tr = listed.iter().find(|x| x.uri == "murmur://tracks").expect("listed");
    assert!(tr.description.as_deref().is_some_and(|d| d.contains("perception")));
    assert!(templates().iter().any(|t| t.uri_template == "murmur://tracks/{id}"));
}

#[test]
fn unlisted_agents_are_refused() {
    let r = rig();
    assert!(r.mcp.resolve("agent-watch").is_ok());
    let err = r.mcp.resolve("nobody").expect_err("not listed");
    assert!(err.message.contains("MURMUR_AGENTS"), "{}", err.message);
}

#[tokio::test]
async fn an_agent_proposes_a_collect_and_a_cover() {
    let r = rig();
    r.d.install_space(
        fixture_space(),
        ScenarioMap {
            engine_origin: r.clock.now(),
            scenario_origin: t0(),
            rate: 60.0,
        },
    );
    let area = json!({ "kind": "circle", "center": [-122.47, 37.825], "radius_km": 5.0 });
    let window = json!({ "from": t0().0, "to": t0().0 + 12 * 3600 * 1000 });

    // a collect without a chosen option is a query: the preview lists the options and
    // creates nothing
    let query = r
        .ok(
            &planner(),
            "preview_intent",
            json!({ "task": "collect", "area": area, "window": window, "capability": "any" }),
        )
        .await;
    assert_eq!(query["would_reach"], "rejected", "{query}");
    let options = query["space"]["options"].as_array().expect("options");
    assert!(!options.is_empty(), "{query}");
    assert!(query["space"]["options_total"].as_u64().unwrap_or(0) >= options.len() as u64);
    assert!(query["summary"].to_string().contains("query"), "{query}");
    assert_eq!(r.events("intent.created"), 0);
    let chosen = json!({
        "norad_id": options[0]["norad_id"],
        "collect_time": options[0]["collect_time"],
    });

    // vehicles on a satellite request are refused before any intent exists
    let (err, v) = r
        .call(
            &planner(),
            "propose_intent",
            json!({
                "task": "collect", "uids": [1], "area": area, "window": window,
                "chosen": chosen, "rationale": "with a vehicle, by mistake",
            }),
        )
        .await;
    assert!(err, "{v}");
    assert!(v["error"].to_string().contains("no uids"), "{v}");

    // the chosen option: a human decides (satellite-collect tier), in the inbox
    let out = r
        .ok(
            &planner(),
            "propose_intent",
            json!({
                "task": "collect", "area": area, "window": window, "chosen": chosen,
                "rationale": "image the anchorage on the next pass; the fleet picture is fresh",
            }),
        )
        .await;
    assert_eq!(out["state"], "pending_approval", "{out}");
    assert_eq!(out["dispatched"], false);
    let id = out["id"].as_u64().expect("id");
    let inbox = r.ok(&planner(), "get_inbox", json!({})).await;
    assert_eq!(inbox["pending"][0]["id"], id);
    assert_eq!(inbox["pending"][0]["source"]["id"], "agent-planner");
    assert!(inbox["pending"][0]["space"]["chosen"].is_object(), "{inbox}");
    let explained = r.ok(&planner(), "explain_intent", json!({ "id": id })).await;
    assert!(
        explained["summary"]
            .to_string()
            .contains("a satellite request, no fleet vehicles"),
        "{explained}"
    );
    let approved = r
        .d
        .approve(&Actor::asserted(OperatorId::new("op-1")), id, None)
        .await
        .expect("approve");
    assert_eq!(approved.state, State::Dispatched);
    assert!(approved.space.is_some(), "a space run, not a vehicle tasking");
    let explained = r.ok(&planner(), "explain_intent", json!({ "id": id })).await;
    assert_eq!(explained["space"]["intent_id"], id, "{explained}");
    assert_eq!(explained["space"]["state"], "tasked");
    let runs = read(&r.mcp, &planner(), "murmur://space/runs")
        .await
        .expect("resource");
    assert_eq!(runs["runs"].as_array().map_or(0, Vec::len), 1);

    // a GNSS cover commands nothing: auto, dispatched as a watch
    let cover = r
        .ok(
            &planner(),
            "propose_intent",
            json!({
                "task": "cover", "area": area, "window": window,
                "service": { "type": "gnss" }, "min_level": 0.0,
                "rationale": "is GNSS good enough over the anchorage for the AUV's surfacing fixes",
            }),
        )
        .await;
    assert_eq!(cover["state"], "dispatched", "{cover}");
    assert_eq!(cover["dispatched"], true);
    assert!(cover["space"].is_object(), "{cover}");
    r.replays();
}

// ---- over HTTP --------------------------------------------------------------------------

/// One JSON-RPC exchange with the mounted endpoint; the answer may come as JSON or as an
/// SSE stream (the last `data:` event is the response).
async fn rpc(
    router: &Router,
    session: Option<&str>,
    agent: Option<&str>,
    body: Value,
) -> (StatusCode, Option<String>, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, "127.0.0.1");
    if let Some(s) = session {
        req = req.header("mcp-session-id", s);
    }
    if let Some(a) = agent {
        req = req.header(AGENT_HEADER, a);
    }
    let resp = router
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).expect("request"))
        .await
        .expect("response");
    let status = resp.status();
    let session = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let sse = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/event-stream"));
    let bytes = tokio::time::timeout(
        Duration::from_secs(10),
        axum::body::to_bytes(resp.into_body(), 4 << 20),
    )
    .await
    .expect("the response stream ends")
    .expect("body");
    let text = String::from_utf8_lossy(&bytes);
    let json = if sse {
        text.lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .and_then(|l| serde_json::from_str(l).ok())
            .unwrap_or(Value::Null)
    } else {
        serde_json::from_str(&text).unwrap_or(Value::Null)
    };
    (status, session, json)
}

#[tokio::test]
async fn the_endpoint_takes_the_agent_header_over_http() {
    let r = rig();
    r.vehicle(1, Firmware::Copter, "x10", (LAT, LON));
    sleep(Duration::from_millis(500)).await;
    let router = r.api.clone().nest_service(
        "/mcp",
        service(
            Settings {
                agents: vec![planner()],
                keys: None,
            },
            r.world.clone(),
            r.d.clone(),
            r.api.clone(),
        ),
    );

    let (status, session, init) = rpc(
        &router,
        None,
        None,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": { "name": "scripted-agent", "version": "0.1" },
            },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{init}");
    let session = session.expect("a session id");
    assert_eq!(init["result"]["serverInfo"]["name"], "murmur-bridge");
    assert!(
        init["result"]["instructions"]
            .as_str()
            .expect("instructions")
            .contains("cannot approve")
    );
    let (status, _, _) = rpc(
        &router,
        Some(&session),
        None,
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let (status, _, list) = rpc(
        &router,
        Some(&session),
        None,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"propose_intent"), "{names:?}");
    assert!(names.iter().all(|n| !n.contains("approve")), "{names:?}");

    // no agent id: refused with a hint; an unlisted one: refused; a listed one: served
    async fn call(router: &Router, session: &str, agent: &str) -> (StatusCode, Option<String>, Value) {
        rpc(
            router,
            Some(session),
            Some(agent),
            json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": { "name": "get_fleet", "arguments": {} },
            }),
        )
        .await
    }
    let (_, _, no_id) = call(&router, &session, "").await;
    // an empty header falls back to clientInfo.name, which isn't listed either
    assert!(
        no_id["error"]["message"]
            .as_str()
            .expect("error")
            .contains("MURMUR_AGENTS"),
        "{no_id}"
    );
    let (_, _, unlisted) = call(&router, &session, "nobody").await;
    assert!(unlisted["error"]["message"].to_string().contains("not in MURMUR_AGENTS"));
    let (status, _, fleet) = call(&router, &session, "agent-planner").await;
    assert_eq!(status, StatusCode::OK, "{fleet}");
    assert_eq!(fleet["result"]["structuredContent"]["count"], 1, "{fleet}");
    assert_eq!(fleet["result"]["isError"], false);
    assert_eq!(r.actions("session.connected"), 1);

    // a resource read, the same way
    let (_, _, res) = rpc(
        &router,
        Some(&session),
        Some("agent-planner"),
        json!({
            "jsonrpc": "2.0", "id": 4, "method": "resources/read",
            "params": { "uri": "murmur://inbox" },
        }),
    )
    .await;
    let text = res["result"]["contents"][0]["text"].as_str().expect("text");
    let inbox: Value = serde_json::from_str(text).expect("json");
    assert_eq!(inbox["counts"]["pending"], 0);
    assert_eq!(IntentId(0).0, 0);
}

/// `POST /api/operator/view` over HTTP (Phase G): an operator's display event is logged under
/// that operator; without the operator header, or with only an agent's, nothing is logged.
/// An agent cannot write what "the operator saw".
#[tokio::test(start_paused = true)]
async fn operator_views_need_an_operator_over_http() {
    let r = rig();
    r.vehicle(3, Firmware::Copter, "switchblade", (LAT, LON));
    sleep(Duration::from_millis(500)).await;
    let id = r
        .ok(&planner(), "propose_intent", orbit(3, "cover the approach"))
        .await["id"]
        .as_u64()
        .expect("id");
    assert_eq!(r.state(id), State::PendingApproval);
    let post = |who: Option<(&'static str, &'static str)>, body: Value| {
        let api = r.api.clone();
        async move {
            let mut req = Request::builder()
                .method("POST")
                .uri("/api/operator/view")
                .header(header::CONTENT_TYPE, "application/json");
            if let Some((h, v)) = who {
                req = req.header(h, v);
            }
            api.oneshot(req.body(Body::from(body.to_string())).expect("request"))
                .await
                .expect("response")
                .status()
        }
    };
    let view = json!({
        "intent_id": id, "event": "open",
        "shown": { "checks": ["readiness:3"], "warnings": ["armed platform: vehicle 3"], "rationale": true },
    });
    assert_eq!(post(None, view.clone()).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        post(Some((AGENT_HEADER, "agent-planner")), view.clone()).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(Some(("X-Murmur-Operator", "op-9")), view.clone()).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(r.events("operator.view"), 0);
    assert_eq!(
        post(Some(("X-Murmur-Operator", "op-1")), view).await,
        StatusCode::OK
    );
    let decide = json!({
        "intent_id": id, "event": "decide", "action": "reject", "open_ms": 4200,
        "shown": { "checks": [], "warnings": [], "rationale": true, "tracks": ["T-0001"] },
    });
    assert_eq!(
        post(Some(("X-Murmur-Operator", "op-1")), decide).await,
        StatusCode::OK
    );
    let bad = json!({ "intent_id": id, "event": "decide", "shown": {} });
    assert_eq!(
        post(Some(("X-Murmur-Operator", "op-1")), bad).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(r.events("operator.view"), 2);
    assert_eq!(
        r.state(id),
        State::PendingApproval,
        "a view decides nothing"
    );
    r.replays();
}

/// A hostile-declared fast boat holding in the engagement zone, seen for a minute by a
/// radar and EO/IR observer 1.5 km away.
fn hostile_track(r: &Rig) {
    use crate::tracks::sensing::{Observer, SensorSpec, SensorType};
    use crate::tracks::truth::{ContactTruth, Domain, Sensing, TruthAffiliation, TruthMessage};
    let observer = Observer {
        uid: 9,
        lat: 37.8120,
        lon: -122.4780,
        alt_msl: 0.0,
        mast_m: 4.0,
        submerged: false,
        sensors: vec![
            SensorSpec { kind: SensorType::Radar, range_m: 8_000.0, guess: true, height: false },
            SensorSpec { kind: SensorType::EoIr, range_m: 5_000.0, guess: true, height: false },
        ],
    };
    let contact = ContactTruth {
        id: "fastboat".into(),
        name: "fast boat".into(),
        affiliation: TruthAffiliation::Hostile,
        domain: Domain::Surface,
        kind: "small_fast_boat".into(),
        lat: 37.8120,
        lon: -122.4950,
        alt_m: 0.0,
        speed_mps: 0.0,
        course_deg: 0.0,
        emits: false,
        signature: 1.0,
        ais: None,
    };
    let now = crate::clock::wall();
    for s in 0..=90 {
        let msg = TruthMessage {
            t: now,
            s: f64::from(s),
            seq: s as u64,
            scenario: "test".into(),
            seed: 1,
            sensing: Sensing { misclassification_rate: 0.0 },
            contacts: vec![contact.clone()],
            acts: Vec::new(),
        };
        r.tracks
            .on_truth_with(msg, std::slice::from_ref(&observer), now - 90.0 + f64::from(s))
            .expect("sensing");
    }
    // rules 9: through the engine's identification ledger, under op-1's session
    r.d.declare(
        &crate::decision::Actor::asserted(OperatorId::new("op-1")),
        None,
        "T-0001",
        murmur_decide::Affiliation::Hostile,
        None,
        None,
    )
    .expect("declared");
}

/// A neutral boat holding 200 m north of the hostile one, seen by the same observer for as
/// long: inside the keep-out of any effect on the hostile boat.
fn neutral_alongside(r: &Rig) {
    use crate::tracks::sensing::{Observer, SensorSpec, SensorType};
    use crate::tracks::truth::{ContactTruth, Domain, Sensing, TruthAffiliation, TruthMessage};
    let observer = Observer {
        uid: 9,
        lat: 37.8120,
        lon: -122.4780,
        alt_msl: 0.0,
        mast_m: 4.0,
        submerged: false,
        sensors: vec![
            SensorSpec { kind: SensorType::Radar, range_m: 8_000.0, guess: true, height: false },
            SensorSpec { kind: SensorType::EoIr, range_m: 5_000.0, guess: true, height: false },
        ],
    };
    let contact = |id: &str, aff, kind: &str, lat: f64| ContactTruth {
        id: id.into(),
        name: id.into(),
        affiliation: aff,
        domain: Domain::Surface,
        kind: kind.into(),
        lat,
        lon: -122.4950,
        alt_m: 0.0,
        speed_mps: 0.0,
        course_deg: 0.0,
        emits: false,
        signature: 1.0,
        ais: None,
    };
    let now = crate::clock::wall();
    for s in 0..=90 {
        let msg = TruthMessage {
            t: now,
            s: f64::from(s),
            seq: s as u64,
            scenario: "test".into(),
            seed: 1,
            sensing: Sensing { misclassification_rate: 0.0 },
            contacts: vec![
                contact("fastboat", TruthAffiliation::Hostile, "small_fast_boat", 37.8120),
                contact("fisher", TruthAffiliation::Neutral, "fishing_vessel", 37.8138),
            ],
            acts: Vec::new(),
        };
        r.tracks
            .on_truth_with(msg, std::slice::from_ref(&observer), now - 90.0 + f64::from(s))
            .expect("sensing");
    }
    r.d.declare(
        &crate::decision::Actor::asserted(OperatorId::new("op-1")),
        None,
        "T-0001",
        murmur_decide::Affiliation::Hostile,
        None,
        None,
    )
    .expect("declared");
}

/// Murmur-12e: an agent reads why a keep-out blocks a nomination it might make (the same dry
/// run as the console's form): which track, why, by how much, what would clear it. Reading
/// it logs nothing and moves nothing.
#[tokio::test(start_paused = true)]
async fn an_agent_reads_why_a_keep_out_blocks_and_nothing_is_logged() {
    let r = rig();
    r.vehicle(2, Firmware::Copter, "fpv", (37.8120, -122.4900)); // armed, 440 m away
    sleep(Duration::from_millis(500)).await;
    r.clock.set(snapshot::ms(crate::clock::wall()));
    neutral_alongside(&r);
    let lines = r.log.lines().len();
    let v = r
        .ok(&planner(), "get_nomination_options", json!({ "track_id": "T-0001", "effect_radius_m": 75.0 }))
        .await;
    assert_eq!(r.log.lines().len(), lines, "a read logs nothing");
    let o = &v["options"][0];
    assert_eq!(o["uid"], 2, "{v}");
    assert_eq!(o["passed"], false, "{o}");
    let ko = &o["keep_outs"];
    assert_eq!(ko["blocked"], true, "{o}");
    let item = &ko["items"][0];
    assert_eq!(item["id"], "T-0002", "{ko}");
    assert_eq!(item["blocks"], true, "{ko}");
    assert_eq!(item["subject"], "track", "{ko}");
    assert!(item["short_m"].as_f64().unwrap_or(0.0) > 0.0, "{item}");
    assert!(item["reason"].as_str().unwrap_or("").contains("short by"), "{item}");
    // a boat holding 200 m off: no position of the platform clears it, nor does waiting
    assert_eq!(ko["move_within"]["possible"], false, "{ko}");
    assert!(ko["wait"].is_null() && ko["wait_none"].is_string(), "{ko}");
    // the explanation quotes the dry run's own keep-out checks
    let checks = o["checks"].as_array().expect("checks");
    for c in ko["checks"].as_array().expect("its checks") {
        assert!(checks.contains(c), "{c}");
    }
    // an explanation is not a nomination: nothing exists, nothing waits
    assert_eq!(r.events("intent.created"), 0);
    assert!(r.d.nominations_json().is_empty());
    r.replays();
}

fn nomination(uid: Uid, rationale: Option<&str>) -> Value {
    let mut v = json!({
        "task": "engage", "uids": [uid], "track_id": "T-0001", "effect_radius_m": 100.0,
        "fallback": "return", "window_s": 300,
    });
    if let Some(r) = rationale {
        v["rationale"] = json!(r);
    }
    v
}

#[tokio::test(start_paused = true)]
async fn an_agent_nominates_an_engagement_and_nothing_it_can_call_moves_it() {
    let r = rig();
    r.vehicle(2, Firmware::Copter, "fpv", (37.8120, -122.4900)); // armed, 440 m away
    sleep(Duration::from_millis(500)).await;
    r.clock.set(snapshot::ms(crate::clock::wall()));
    hostile_track(&r);
    // the preview: checks, and the intercept with the predicted effect
    let pv = r.ok(&planner(), "preview_intent", nomination(2, None)).await;
    assert_eq!(pv["would_reach"], "pending_approval", "{pv}");
    assert!(pv["engage"]["effect_point"].is_array(), "{pv}");
    // a nomination without a rationale is refused before anything exists
    let (err, v) = r.call(&planner(), "propose_intent", nomination(2, None)).await;
    assert!(err, "{v}");
    assert!(v["error"].to_string().contains("rationale"), "{v}");
    let out = r
        .ok(
            &planner(),
            "propose_intent",
            nomination(2, Some("T-0001 is declared hostile and holding in ez-gate-west")),
        )
        .await;
    let id = out["id"].as_u64().expect("id");
    assert_eq!(out["state"], "pending_approval", "{out}");
    let noms = r.ok(&planner(), "get_nominations", json!({ "mine": true })).await;
    assert_eq!(noms["count"], 1, "{noms}");
    let n = &noms["nominations"][0];
    assert_eq!(n["engage"]["approvers_needed"], 2, "{n}");
    assert_eq!(n["engage"]["eligible"], json!(["op-2", "op-3"]), "{n}");
    assert!(n["engagement"]["preview"]["ellipse"].is_object(), "{n}");
    // every tool, with arguments naming the nomination: it stays pending
    let mut names: Vec<String> = tools::tools()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    names.retain(|n| n != "withdraw_intent");
    for name in &names {
        let mut args = nomination(2, Some("every tool, same arguments"));
        args["id"] = json!(id);
        args["uid"] = json!(2);
        args["reason"] = json!("checking that nothing here signs or releases anything");
        let _ = r.call(&planner(), name, args).await;
        assert_eq!(r.state(id), State::PendingApproval, "after {name}");
        assert_eq!(r.events("approval.granted"), 0, "after {name}");
        assert_eq!(r.events("approval.concurred"), 0, "after {name}");
        assert_eq!(r.events("engage.effect"), 0, "after {name}");
    }
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn an_agent_order_on_an_engaging_platform_waits_for_a_human() {
    let r = rig();
    r.vehicle(2, Firmware::Copter, "fpv", (37.8120, -122.4900)); // armed, 440 m away
    r.vehicle(3, Firmware::Copter, "fpv", (37.8068, -122.4460)); // Crissy Field, 4 km
    sleep(Duration::from_millis(500)).await;
    r.clock.set(snapshot::ms(crate::clock::wall()));
    hostile_track(&r);
    let out = r
        .ok(
            &planner(),
            "propose_intent",
            nomination(2, Some("T-0001 is declared hostile and holding in ez-gate-west")),
        )
        .await;
    let eng = out["id"].as_u64().expect("id");
    assert_eq!(out["state"], "pending_approval", "{out}");
    let hold = |uid: Uid| json!({ "task": "hold", "uids": [uid], "rationale": "hold position" });
    // a hold runs on its own anywhere else (the safety-commands tier)...
    let other = r.ok(&planner(), "propose_intent", hold(3)).await;
    assert_eq!(other["state"], "dispatched", "{other}");
    // ...but not on a platform with an engagement in play: a human decides
    let pv = r.ok(&planner(), "preview_intent", hold(2)).await;
    assert_eq!(pv["would_reach"], "pending_approval", "{pv}");
    let mine = r.ok(&planner(), "propose_intent", hold(2)).await;
    assert_eq!(mine["state"], "pending_approval", "{mine}");
    let held = mine["id"].as_u64().expect("id");
    assert_eq!(r.state(eng), State::PendingApproval);
    let v = r.d.intent_json(held).expect("intent");
    assert_eq!(v["intent"]["policy"]["rule"], "(engagement in play)", "{v}");
    assert_eq!(r.events("engage.stopped"), 0);
    // an operator approving the agent's hold must acknowledge that it stops the engagement
    let err = r
        .d
        .approve_with(&Actor::asserted(OperatorId::new("op-1")), held, None, None, false)
        .await
        .expect_err("not acknowledged");
    assert_eq!(err.status, 409, "{}", err.detail);
    assert!(err.detail.contains("acknowledge"), "{}", err.detail);
    assert_eq!(r.state(held), State::PendingApproval);
    assert_eq!(r.state(eng), State::PendingApproval);
    let o = r
        .d
        .approve_with(&Actor::asserted(OperatorId::new("op-1")), held, None, None, true)
        .await
        .expect("acknowledged");
    assert!(o.dispatched(), "{}", o.intent);
    let granted = r
        .log
        .lines()
        .into_iter()
        .find(|l| l.contains("\"type\":\"approval.granted\""))
        .expect("approval.granted");
    assert!(granted.contains(&format!("\"supersedes\":[{eng}]")), "{granted}");
    // it stops the engagement: superseded, no fallback
    assert_eq!(r.state(eng), State::Voided);
    let stop = r
        .log
        .lines()
        .into_iter()
        .find(|l| l.contains("\"type\":\"engage.stopped\""))
        .expect("engage.stopped");
    assert!(stop.contains("\"kind\":\"superseded\""), "{stop}");
    assert!(stop.contains("\"fallback\":null"), "{stop}");
    assert!(stop.contains(&format!("\"by\":{}", o.id.0)), "{stop}");
    r.replays();
}

