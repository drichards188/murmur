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
            pointer: None,
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
        )
        .expect("auth"),
    });
    let mcp = Mcp::new(
        Settings {
            agents: vec![planner(), watch()],
            keys: None,
            idle: IDLE_LIMIT,
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

/// `if_state`: a withdrawal only from the state the agent read, checked with the withdrawal
/// (the playbook reads pending_approval at a tick's start and withdraws at its end; an
/// operator's approval in between must not be undone). A mismatch is refused and logs nothing.
#[tokio::test(start_paused = true)]
async fn a_withdrawal_with_if_state_happens_only_from_that_state() {
    let r = rig();
    r.vehicle(2, Firmware::Copter, "switchblade", (LAT, LON));
    sleep(Duration::from_millis(500)).await;
    let id = r.ok(&planner(), "propose_intent", orbit(2, "watch the strait")).await["id"]
        .as_u64()
        .expect("id");
    assert_eq!(r.state(id), State::PendingApproval);
    let (err, v) = r
        .call(
            &planner(),
            "withdraw_intent",
            json!({ "id": id, "reason": "premise gone", "if_state": "approved" }),
        )
        .await;
    assert!(err, "{v}");
    assert_eq!(v["status"], 409);
    assert!(
        v["error"].to_string().contains("is pending_approval, not approved"),
        "{v}"
    );
    assert_eq!(r.state(id), State::PendingApproval, "nothing withdrawn");
    assert_eq!(r.actions("intent.withdraw"), 0, "nothing logged");
    let (err, v) = r
        .call(
            &planner(),
            "withdraw_intent",
            json!({ "id": 999, "reason": "x", "if_state": "pending_approval" }),
        )
        .await;
    assert!(err && v["status"] == 404, "{v}");
    let v = r
        .ok(
            &planner(),
            "withdraw_intent",
            json!({ "id": id, "reason": "premise gone", "if_state": "pending_approval" }),
        )
        .await;
    assert_eq!(v["state"], "voided");
    assert_eq!(r.actions("intent.withdraw"), 1);
    r.replays();
}

/// `if_state` checks the owner before the state (another agent's intent is 403 whatever state
/// it names, so its state is not said), and refuses a value that is no intent state (400).
#[tokio::test(start_paused = true)]
async fn if_state_checks_the_owner_first_and_knows_the_states() {
    let r = rig();
    r.vehicle(2, Firmware::Copter, "switchblade", (LAT, LON));
    sleep(Duration::from_millis(500)).await;
    let id = r.ok(&planner(), "propose_intent", orbit(2, "watch the strait")).await["id"]
        .as_u64()
        .expect("id");
    for want in ["approved", "pending_approval"] {
        let (err, v) = r
            .call(
                &watch(),
                "withdraw_intent",
                json!({ "id": id, "reason": "not mine", "if_state": want }),
            )
            .await;
        assert!(err && v["status"] == 403, "{want}: {v}");
        assert!(!v["error"].to_string().contains("pending_approval, not"), "{v}");
    }
    let (err, v) = r
        .call(
            &planner(),
            "withdraw_intent",
            json!({ "id": id, "reason": "x", "if_state": "pending" }),
        )
        .await;
    assert!(err && v["status"] == 400, "{v}");
    assert_eq!(r.state(id), State::PendingApproval, "nothing withdrawn");
    assert_eq!(r.actions("intent.withdraw"), 0, "nothing logged");
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

    // the compact view: the same facts, one short entry, the stale ones named
    let full = r.ok(&planner(), "get_fleet", json!({ "uids": [1] })).await;
    let small = r.ok(&planner(), "get_fleet", json!({ "uids": [1], "compact": true })).await;
    assert_eq!((small["count"].as_u64(), small["compact"].as_bool()), (Some(1), Some(true)));
    let (f, c) = (&full["vehicles"][0], &small["vehicles"][0]);
    assert_eq!(
        (&c["uid"], &c["callsign"], &c["platform"], &c["armed"], &c["lon"], &c["lat"]),
        (&f["uid"], &f["callsign"], &f["platform"]["id"], &f["platform"]["armed"], &f["position"]["lon"], &f["position"]["lat"]),
        "{c}"
    );
    assert_eq!((&c["nav_ready"], &c["link_lost"]), (&json!(true), &json!(false)));
    assert!(c["stale"].as_array().is_some_and(|s| s.contains(&json!("position"))), "{c}");
    assert!(!c["stale"].as_array().is_some_and(|s| s.contains(&json!("nav"))), "{c}");
    let glitch_stale = f["nav"]["gps_glitch"]["stale"].as_bool().unwrap_or(true);
    assert_eq!(
        c["stale"].as_array().is_some_and(|s| s.contains(&json!("gps_glitch"))),
        glitch_stale,
        "the glitch flag's own staleness: {c}"
    );
    assert!(small["note"].as_str().is_some_and(|n| n.contains("get_vehicle")));
    let size = |v: &Value| v["vehicles"][0].to_string().len();
    assert!(size(&small) * 3 < size(&full), "{} against {}", size(&small), size(&full));
    // the default is the full view, and uids filters either
    r.vehicle(2, Firmware::Copter, "x10", (LAT, LON + 0.001));
    sleep(Duration::from_millis(500)).await;
    let both = r.ok(&planner(), "get_fleet", json!({ "compact": true })).await;
    assert_eq!(both["count"], 2);
    assert_eq!(r.ok(&planner(), "get_fleet", json!({})).await["compact"], false);

    // the same views as resources
    let fleet = read(&r.mcp, &planner(), "murmur://fleet").await.expect("resource");
    assert_eq!(fleet["count"], 2);
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

/// The kinds of a `get_changes` answer, and the first change of `kind`.
fn change<'a>(v: &'a Value, kind: &str) -> Option<&'a Value> {
    v["changes"]
        .as_array()
        .and_then(|c| c.iter().find(|c| c["kind"] == kind))
}

fn kinds(v: &Value) -> Vec<String> {
    v["changes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["kind"].as_str().map(str::to_string))
        .collect()
}

/// A cursor's number (`<feed>:<n>`).
fn seq(cursor: &Value) -> u64 {
    cursor
        .as_str()
        .and_then(|c| c.rsplit_once(':'))
        .and_then(|(_, n)| n.parse().ok())
        .unwrap_or_else(|| panic!("a cursor: {cursor}"))
}

/// `get_changes` is a cursor over what changed between reads. The first read is a baseline;
/// then an agent's proposal, a track that turns up and is declared hostile, re-declared
/// (neutral, then hostile again: each a new declaration), a GPS glitch flag, a lost link, an
/// operator's rejection with its note and a withdrawal each show up once, the intent marked
/// mine for its proposer only. Reading logs nothing, a read within a second of the last digest
/// answers from the feed, an empty read keeps the cursor, and a cursor of an earlier bridge is
/// told to start again even when this bridge's feed has gone past its number. `get_fleet`
/// carries the glitch flag with its age.
#[tokio::test(start_paused = true)]
async fn get_changes_reports_what_changed_since_a_cursor() {
    let r = rig();
    r.vehicle(2, Firmware::Copter, "switchblade", (LAT, LON)); // armed: human tier
    // a fake whose link can be cut, so that its own reports stop overwriting what we set
    let (q, w) = quick().watched();
    let cell = spawn_fake(3, Firmware::Copter, LAT, LON + 0.002, q);
    let mut m = serde_json::Map::new();
    m.insert("platform".into(), json!("x10"));
    m.insert("callsign".into(), json!("x10 3"));
    cell.write(|v| v.identity = Some(Arc::new(Identity::from_manifest(&m))));
    r.world.insert_vehicle(cell.clone());
    sleep(Duration::from_millis(500)).await;
    r.clock.set(snapshot::ms(crate::clock::wall()));
    // the feed takes a new digest at most once a second
    let tick = || sleep(changes::REFRESH_EVERY);

    let base = r.ok(&watch(), "get_changes", json!({})).await;
    assert_eq!(base["count"], 0, "a baseline: {base}");
    assert_eq!(base["reset"], false);
    let c0 = base["cursor"].clone();
    let fleet = r.ok(&watch(), "get_vehicle", json!({ "uid": 3 })).await;
    assert_eq!(fleet["vehicle"]["nav"]["gps_glitch"]["value"], false, "{fleet}");
    assert!(fleet["vehicle"]["nav"]["gps_glitch"]["age_s"].as_f64().is_some());

    let id = r
        .ok(&watch(), "propose_intent", orbit(2, "watch the strait"))
        .await["id"]
        .as_u64()
        .expect("id");
    hostile_track(&r);
    // too soon for a new digest: the feed answers what it has
    let soon = r.ok(&watch(), "get_changes", json!({ "since": c0 })).await;
    assert_eq!(soon["count"], 0, "{soon}");
    tick().await;
    let lines = r.log.lines().len();
    let v = r.ok(&watch(), "get_changes", json!({ "since": c0 })).await;
    assert_eq!(r.log.lines().len(), lines, "reading logs nothing");
    let new = change(&v, "intent.new").expect("intent.new");
    assert_eq!(new["intent"], id);
    assert_eq!(new["state"], "pending_approval");
    assert_eq!(new["task"], "orbit");
    assert_eq!(new["mine"], true);
    let t = change(&v, "track.new").expect("track.new");
    assert_eq!(t["track"], "T-0001");
    assert!(t.get("identity").is_some() && t.get("position").is_some(), "{t}");
    let d = change(&v, "track.declared").expect("track.declared");
    assert_eq!((d["track"].as_str(), d["declared"].as_str()), (Some("T-0001"), Some("hostile")));
    assert_eq!((d["by"].as_str(), d["from"].as_str()), (Some("op-1"), None));
    let k = kinds(&v);
    assert!(
        k.iter().position(|k| k == "intent.new") < k.iter().position(|k| k == "track.new"),
        "intents first: {k:?}"
    );
    let seqs: Vec<u64> = v["changes"]
        .as_array()
        .expect("changes")
        .iter()
        .filter_map(|c| c["seq"].as_u64())
        .collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]) && seqs[0] == seq(&c0) + 1, "{seqs:?}");
    let c1 = v["cursor"].clone();
    assert_eq!(seq(&c1), *seqs.last().expect("some"));

    // nothing new: no changes, the same cursor
    tick().await;
    let again = r.ok(&watch(), "get_changes", json!({ "since": c1 })).await;
    assert_eq!((again["count"].as_u64(), &again["cursor"]), (Some(0), &c1), "{again}");
    // another agent reads the same changes; the intent is not its own
    let theirs = r.ok(&planner(), "get_changes", json!({ "since": c0 })).await;
    assert_eq!(change(&theirs, "intent.new").expect("intent.new")["mine"], false);
    // a small page: more, and the cursor of the last change returned
    let page = r.ok(&planner(), "get_changes", json!({ "since": c0, "limit": 1 })).await;
    assert_eq!((page["count"].as_u64(), page["more"].as_bool()), (Some(1), Some(true)));
    assert_eq!(seq(&page["cursor"]), seq(&c0) + 1);

    // re-declared: an operator replaces the declaration (revoke and declare in one step),
    // then declares it hostile again; each is a new declaration, with what it replaced
    let op = |o: &str| crate::decision::Actor::asserted(OperatorId::new(o));
    let declare = |o: &str, aff| {
        r.d.declare(&op(o), None, "T-0001", aff, None, None)
            .expect("declared")
    };
    declare("op-2", murmur_decide::Affiliation::Neutral);
    tick().await;
    let v = r.ok(&watch(), "get_changes", json!({ "since": c1 })).await;
    let d = change(&v, "track.declared").expect("re-declared");
    assert_eq!((d["declared"].as_str(), d["from"].as_str()), (Some("neutral"), Some("hostile")), "{v}");
    declare("op-1", murmur_decide::Affiliation::Hostile);
    tick().await;
    let v = r.ok(&watch(), "get_changes", json!({ "since": v["cursor"] })).await;
    let d = change(&v, "track.declared").expect("hostile again");
    assert_eq!((d["declared"].as_str(), d["from"].as_str()), (Some("hostile"), Some("neutral")), "{v}");
    let c1 = v["cursor"].clone();

    // the navigation filter flags the GPS as glitching (the link cut keeps the fake's own
    // reports from overwriting it; the heartbeat is fresh, so the link is not lost yet)
    w.set_cut(true);
    let now = crate::clock::wall();
    cell.write(|v| {
        v.ekf_flags = Some(
            crate::mav::apm::EkfStatusFlags::EKF_POS_HORIZ_ABS
                | crate::mav::apm::EkfStatusFlags::EKF_GPS_GLITCHING,
        );
        v.seen.ekf = Some(now);
        v.seen.heartbeat = Some(now);
    });
    let fleet = r.ok(&watch(), "get_vehicle", json!({ "uid": 3 })).await;
    assert_eq!(fleet["vehicle"]["nav"]["gps_glitch"]["value"], true, "{fleet}");
    tick().await;
    cell.write(|v| {
        v.seen.ekf = Some(crate::clock::wall());
        v.seen.heartbeat = Some(crate::clock::wall());
    });
    let v = r.ok(&watch(), "get_changes", json!({ "since": c1 })).await;
    let g = change(&v, "vehicle.gps_glitch").expect("vehicle.gps_glitch");
    assert_eq!((g["uid"].as_u64(), g["glitch"].as_bool()), (Some(3), Some(true)), "{v}");
    assert!(change(&v, "vehicle.link_lost").is_none(), "{v}");
    let c2 = v["cursor"].clone();

    // its link goes silent
    cell.write(|v| v.seen.heartbeat = Some(crate::clock::wall() - 30.0));
    tick().await;
    let v = r.ok(&watch(), "get_changes", json!({ "since": c2 })).await;
    let l = change(&v, "vehicle.link_lost").expect("vehicle.link_lost");
    assert_eq!((l["uid"].as_u64(), l["armed"].as_bool()), (Some(3), Some(false)), "{v}");
    let c3 = v["cursor"].clone();

    // an operator rejects a second proposal, saying why; the agent withdraws the first
    let id2 = r
        .ok(&watch(), "propose_intent", orbit(2, "watch the north tower"))
        .await["id"]
        .as_u64()
        .expect("id");
    r.d.reject(&op("op-1"), id2, Some("not now: the shore radar has it".into()))
        .expect("rejected");
    r.ok(&watch(), "withdraw_intent", json!({ "id": id, "reason": "the picture changed" }))
        .await;
    tick().await;
    let v = r.ok(&watch(), "get_changes", json!({ "since": c3 })).await;
    let s = change(&v, "intent.state").expect("intent.state");
    assert_eq!(
        (s["intent"].as_u64(), s["from"].as_str(), s["to"].as_str(), s["mine"].as_bool()),
        (Some(id), Some("pending_approval"), Some("voided"), Some(true)),
        "{v}"
    );
    let rej = change(&v, "intent.new").expect("the second, already decided");
    assert_eq!((rej["intent"].as_u64(), rej["state"].as_str()), (Some(id2), Some("rejected")));
    assert_eq!(rej["operator_note"], json!({ "by": "op-1", "note": "not now: the shore radar has it" }));
    let c4 = v["cursor"].clone();
    // another agent sees the same change, but not the operator's words to this one
    let theirs = r.ok(&planner(), "get_changes", json!({ "since": c3 })).await;
    let rej = change(&theirs, "intent.new").expect("intent.new for the planner");
    assert_eq!((rej["mine"].as_bool(), rej.get("operator_note")), (Some(false), None), "{theirs}");
    // a note is a reason, not a document: kept to REJECTION_NOTE_CHARS
    let id3 = r
        .ok(&watch(), "propose_intent", orbit(2, "watch the south tower"))
        .await["id"]
        .as_u64()
        .expect("id");
    r.d.reject(&op("op-1"), id3, Some("x".repeat(5000)))
        .expect("rejected");
    tick().await;
    let v = r.ok(&watch(), "get_changes", json!({ "since": c4 })).await;
    let long = change(&v, "intent.new").expect("the third, already decided");
    let note = long["operator_note"]["note"].as_str().expect("a note");
    assert_eq!(
        note.chars().count(),
        crate::decision::agent::REJECTION_NOTE_CHARS + 3,
        "cut, with ..."
    );
    let c4 = v["cursor"].clone();

    // a cursor ahead of the feed: start again, nothing to report
    let ahead = r.ok(&watch(), "get_changes", json!({ "since": 1_000_000 })).await;
    assert_eq!((ahead["reset"].as_bool(), ahead["count"].as_u64()), (Some(true), Some(0)));
    assert!(seq(&ahead["cursor"]) < 1_000_000);

    // a new bridge whose feed has gone past this one's number: the cursor names another
    // feed, so it starts again instead of reading on mid-feed
    let r2 = rig();
    r2.ok(&watch(), "get_changes", json!({})).await;
    for uid in 10..10 + seq(&c4) + 3 {
        r2.vehicle(uid, Firmware::Copter, "x10", (LAT, LON + 0.0001 * uid as f64));
    }
    sleep(Duration::from_millis(500)).await;
    tick().await;
    let fresh = r2.ok(&watch(), "get_changes", json!({})).await;
    assert!(seq(&fresh["cursor"]) > seq(&c4), "{fresh}");
    let old = r2.ok(&watch(), "get_changes", json!({ "since": c4 })).await;
    assert_eq!((old["reset"].as_bool(), old["count"].as_u64()), (Some(true), Some(0)), "{old}");
    r.replays();
}

#[tokio::test]
async fn tracks_are_perception_with_age_and_never_truth() {
    use crate::tracks::sensing::{Observer, SensorSpec, SensorType};
    use crate::tracks::truth::{Ais, ContactTruth, Domain, Sensing, SensingModel, TruthAffiliation, TruthMessage};
    let r = rig();
    // a dark fast boat 1.5 km from a boat of ours with radar and EO/IR, seen for a minute
    let observer = Observer {
        uid: 3,
        lat: LAT,
        lon: LON,
        alt_msl: 0.0,
        mast_m: 4.0,
        submerged: false,
        heading_deg: None,
        area: None,
        sensors: vec![
            SensorSpec { kind: SensorType::Radar, range_m: 8_000.0, guess: true, height: false, physical: Default::default() },
            SensorSpec { kind: SensorType::EoIr, range_m: 5_000.0, guess: true, height: false, physical: Default::default() },
            SensorSpec { kind: SensorType::Ais, range_m: 15_000.0, guess: true, height: false, physical: Default::default() },
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
            sensing: Sensing { misclassification_rate: 0.0, model: Some(SensingModel::Legacy), ..Default::default() },
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
                idle: IDLE_LIMIT,
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
    // every tool says how large an answer its client should hand the model whole (Claude
    // Code saved larger ones to a file a model with no file tools cannot read)
    for t in list["result"]["tools"].as_array().expect("tools") {
        assert_eq!(
            t["_meta"]["anthropic/maxResultSizeChars"].as_u64(),
            Some(tools::RESULT_MAX_CHARS),
            "{t}"
        );
    }
    // protocol 2026-07-28 requires both on list results; Claude Code 2.1.294 refused the
    // list without them and offered the model no tool at all
    for method in ["tools/list", "resources/list", "resources/templates/list"] {
        let (_, _, list) = rpc(
            &router,
            Some(&session),
            None,
            json!({ "jsonrpc": "2.0", "id": 5, "method": method }),
        )
        .await;
        assert!(list["result"]["ttlMs"].as_u64().is_some(), "{method}: {list}");
        assert_eq!(list["result"]["cacheScope"], "private", "{method}: {list}");
    }

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

/// An agent session (a client on a protocol version before 2026-07-28 holds one; the Python
/// SDK negotiates 2025-11-25) survives a quiet spell of ten minutes: an agent woken on events
/// can be quiet that long, and rmcp's own five-minute limit dropped the session of an agent
/// that connected once and waited (the situation framework's storyline agent, live). A
/// session idle past the limit is still dropped, and the limit is the bridge's setting
/// (`MURMUR_MCP_IDLE_S`). One agent holds at most `SESSIONS_PER_AGENT`: past that the one it
/// used least recently is closed.
#[tokio::test(start_paused = true)]
async fn an_agent_session_survives_a_quiet_spell_and_the_idle_limit_is_set_by_the_bridge() {
    let r = rig();
    r.vehicle(1, Firmware::Copter, "x10", (LAT, LON));
    sleep(Duration::from_millis(500)).await;
    let mount = |idle: Duration| {
        r.api.clone().nest_service(
            "/mcp",
            service(
                Settings { agents: vec![planner()], keys: None, idle },
                r.world.clone(),
                r.d.clone(),
                r.api.clone(),
            ),
        )
    };
    async fn open(router: &Router) -> String {
        let (status, session, init) = rpc(
            router,
            None,
            None,
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25", "capabilities": {},
                    "clientInfo": { "name": "agent-planner", "version": "0.1" },
                },
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{init}");
        let session = session.expect("a session: this protocol version keeps one");
        rpc(
            router,
            Some(&session),
            None,
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        )
        .await;
        session
    }
    async fn fleet(router: &Router, session: &str) -> (StatusCode, Value) {
        let (status, _, v) = rpc(
            router,
            Some(session),
            Some("agent-planner"),
            json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": { "name": "get_fleet", "arguments": {} },
            }),
        )
        .await;
        (status, v)
    }
    // the bridge's default: ten quiet minutes, then a call on the same session
    assert!(IDLE_LIMIT > Duration::from_secs(600), "{IDLE_LIMIT:?}");
    let router = mount(IDLE_LIMIT);
    let session = open(&router).await;
    sleep(Duration::from_secs(600)).await;
    let (status, v) = fleet(&router, &session).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["result"]["structuredContent"]["count"], 1, "{v}");
    // past the limit the session is gone
    sleep(IDLE_LIMIT + Duration::from_secs(5)).await;
    let (status, v) = fleet(&router, &session).await;
    assert_ne!(status, StatusCode::OK, "{v}");
    // a short limit, as `MURMUR_MCP_IDLE_S` sets it
    let short = mount(Duration::from_secs(20));
    let session = open(&short).await;
    sleep(Duration::from_secs(10)).await;
    assert_eq!(fleet(&short, &session).await.0, StatusCode::OK);
    sleep(Duration::from_secs(30)).await;
    assert_ne!(fleet(&short, &session).await.0, StatusCode::OK);
    // one agent past its cap of sessions: the one it used least recently is closed (a client
    // that opens sessions and never closes them holds SESSIONS_PER_AGENT, not one per retry)
    let capped = mount(IDLE_LIMIT);
    let mut held = Vec::new();
    for _ in 0..=SESSIONS_PER_AGENT {
        let s = open(&capped).await;
        assert_eq!(fleet(&capped, &s).await.0, StatusCode::OK);
        held.push(s);
        sleep(Duration::from_secs(1)).await;
    }
    sleep(Duration::from_millis(50)).await; // the close runs on its own task
    assert_ne!(fleet(&capped, &held[0]).await.0, StatusCode::OK, "the oldest was closed");
    for s in &held[1..] {
        assert_eq!(fleet(&capped, s).await.0, StatusCode::OK, "the others are kept");
    }
    // sessions their client closed do not count: a quiet one is kept however many others
    // come and go (the storyline agent opens and closes one per proposal)
    async fn close(router: &Router, session: &str) -> StatusCode {
        let req = Request::builder()
            .method("DELETE")
            .uri("/mcp")
            .header(header::HOST, "127.0.0.1")
            .header(AGENT_HEADER, "agent-planner")
            .header("mcp-session-id", session)
            .header("mcp-protocol-version", "2025-11-25")
            .body(Body::empty())
            .expect("request");
        router.clone().oneshot(req).await.expect("response").status()
    }
    let tidy = mount(IDLE_LIMIT);
    let quiet = open(&tidy).await;
    assert_eq!(fleet(&tidy, &quiet).await.0, StatusCode::OK);
    for _ in 0..2 * SESSIONS_PER_AGENT {
        sleep(Duration::from_secs(1)).await;
        let s = open(&tidy).await;
        assert_eq!(fleet(&tidy, &s).await.0, StatusCode::OK);
        let closed = close(&tidy, &s).await;
        assert!(closed.is_success(), "{closed}");
        assert_ne!(fleet(&tidy, &s).await.0, StatusCode::OK, "closed");
    }
    sleep(Duration::from_millis(50)).await;
    assert_eq!(fleet(&tidy, &quiet).await.0, StatusCode::OK, "the quiet one is kept");
    // a session counts from its initialize: sessions that never call a tool are capped too
    let silent = mount(IDLE_LIMIT);
    let mut opened = Vec::new();
    for _ in 0..=SESSIONS_PER_AGENT {
        opened.push(open(&silent).await);
        sleep(Duration::from_secs(1)).await;
    }
    sleep(Duration::from_millis(50)).await;
    assert_ne!(fleet(&silent, &opened[0]).await.0, StatusCode::OK, "the oldest was closed");
    for s in &opened[1..] {
        assert_eq!(fleet(&silent, s).await.0, StatusCode::OK, "the others are kept");
    }
}

/// Final review, nits 1 and 2: an opening request over 1 MB was forwarded with an empty body
/// (now 413), and sessions opened by a claimant naming no listed agent counted against no cap
/// (now one shared bucket, capped like an agent's).
#[tokio::test(start_paused = true)]
async fn an_opening_request_too_large_is_refused_and_unlisted_sessions_are_capped_together() {
    let r = rig();
    let svc = service(
        Settings { agents: vec![planner()], keys: None, idle: IDLE_LIMIT },
        r.world.clone(),
        r.d.clone(),
        r.api.clone(),
    );
    let shared = svc.shared.clone();
    let router = r.api.clone().nest_service("/mcp", svc);
    let big = format!("{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"pad\":\"{}\"}}", "x".repeat(2 << 20));
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, "127.0.0.1")
        .body(Body::from(big))
        .expect("request");
    let resp = router.clone().oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    // nobody listed: each initialize opens a session (its calls are refused), all in one bucket
    for _ in 0..=SESSIONS_PER_AGENT {
        let (status, session, init) = rpc(
            &router,
            None,
            None,
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25", "capabilities": {},
                    "clientInfo": { "name": "nobody", "version": "0.1" },
                },
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{init}");
        assert!(session.is_some());
        sleep(Duration::from_secs(1)).await;
    }
    let bucket = AgentId::new(UNLISTED);
    assert_eq!(shared.sessions_of(&bucket).len(), SESSIONS_PER_AGENT, "capped together");
    assert!(shared.sessions_of(&AgentId::new("nobody")).is_empty());
}

/// Third review, item 5: the cap closed the session used least recently even with a request
/// in flight on it (a long proposal or dry run lost its answer, and a client that retries
/// may propose twice). A session with a request in flight is never closed: the next idle one
/// goes, and with none idle the cap waits.
#[tokio::test(start_paused = true)]
async fn the_session_cap_never_closes_a_session_with_a_request_in_flight() {
    let r = rig();
    let a = planner();
    r.mcp.note_session_id(&a, "s0");
    let busy = r.mcp.busy(&a, Some("s0".into())).expect("s0 is held");
    for i in 1..=SESSIONS_PER_AGENT {
        sleep(Duration::from_secs(1)).await;
        r.mcp.note_session_id(&a, &format!("s{i}"));
    }
    let held = |r: &Rig| -> Vec<String> { r.mcp.sessions_of(&a).into_iter().map(|(s, _)| s).collect() };
    assert!(held(&r).contains(&"s0".to_string()), "in flight: kept {:?}", held(&r));
    assert!(!held(&r).contains(&"s1".to_string()), "the oldest idle one went instead");
    assert_eq!(held(&r).len(), SESSIONS_PER_AGENT);
    // every one busy: the cap waits rather than cut a request off
    let all: Vec<_> = held(&r)
        .into_iter()
        .filter_map(|s| r.mcp.busy(&a, Some(s)))
        .collect();
    r.mcp.note_session_id(&a, "s99");
    assert_eq!(held(&r).len(), SESSIONS_PER_AGENT + 1, "none closed while all are busy");
    drop(all);
    drop(busy);
    assert!(r.mcp.sessions_of(&a).iter().all(|(_, n)| *n == 0), "the guards ended");
    sleep(Duration::from_secs(1)).await;
    r.mcp.note_session_id(&a, "s100");
    assert_eq!(held(&r).len(), SESSIONS_PER_AGENT, "and then it closes the oldest idle ones");
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
    use crate::tracks::truth::{ContactTruth, Domain, Sensing, SensingModel, TruthAffiliation, TruthMessage};
    let observer = Observer {
        uid: 9,
        lat: 37.8120,
        lon: -122.4780,
        alt_msl: 0.0,
        mast_m: 4.0,
        submerged: false,
        heading_deg: None,
        area: None,
        sensors: vec![
            SensorSpec { kind: SensorType::Radar, range_m: 8_000.0, guess: true, height: false, physical: Default::default() },
            SensorSpec { kind: SensorType::EoIr, range_m: 5_000.0, guess: true, height: false, physical: Default::default() },
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
            sensing: Sensing { misclassification_rate: 0.0, model: Some(SensingModel::Legacy), ..Default::default() },
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
    use crate::tracks::truth::{ContactTruth, Domain, Sensing, SensingModel, TruthAffiliation, TruthMessage};
    let observer = Observer {
        uid: 9,
        lat: 37.8120,
        lon: -122.4780,
        alt_msl: 0.0,
        mast_m: 4.0,
        submerged: false,
        heading_deg: None,
        area: None,
        sensors: vec![
            SensorSpec { kind: SensorType::Radar, range_m: 8_000.0, guess: true, height: false, physical: Default::default() },
            SensorSpec { kind: SensorType::EoIr, range_m: 5_000.0, guess: true, height: false, physical: Default::default() },
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
            sensing: Sensing { misclassification_rate: 0.0, model: Some(SensingModel::Legacy), ..Default::default() },
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

/// Rules 16: [`neutral_alongside`]'s fishing boat seen for the first 100 s of 400 and then out
/// of view: the tracker drops it 180 s later, and the decision tick hands the drop to the
/// engine (`track.dropped`), which holds it as a lost contact. The hostile boat declared.
fn lost_alongside(r: &Rig) {
    use crate::tracks::sensing::{Observer, SensorSpec, SensorType};
    use crate::tracks::truth::{ContactTruth, Domain, Sensing, SensingModel, TruthAffiliation, TruthMessage};
    let observer = Observer {
        uid: 9,
        lat: 37.8120,
        lon: -122.4780,
        alt_msl: 0.0,
        mast_m: 4.0,
        submerged: false,
        heading_deg: None,
        area: None,
        sensors: vec![
            SensorSpec { kind: SensorType::Radar, range_m: 8_000.0, guess: true, height: false, physical: Default::default() },
            SensorSpec { kind: SensorType::EoIr, range_m: 5_000.0, guess: true, height: false, physical: Default::default() },
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
    for s in 0..=400 {
        let mut contacts = vec![contact("fastboat", TruthAffiliation::Hostile, "small_fast_boat", 37.8120)];
        if s < 100 {
            contacts.push(contact("fisher", TruthAffiliation::Neutral, "fishing_vessel", 37.8138));
        }
        let msg = TruthMessage {
            t: now,
            s: f64::from(s),
            seq: s as u64,
            scenario: "test".into(),
            seed: 1,
            sensing: Sensing { misclassification_rate: 0.0, model: Some(SensingModel::Legacy), ..Default::default() },
            contacts,
            acts: Vec::new(),
        };
        r.tracks
            .on_truth_with(msg, std::slice::from_ref(&observer), now - 400.0 + f64::from(s))
            .expect("sensing");
    }
    r.d.tick();
    assert_eq!(r.events("track.dropped"), 1, "the fishing boat dropped");
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

/// Rules 16: agents see the contacts the tracker dropped, read-only, and cannot accept one.
/// `get_tracks` lists the lost contact under `lost` (since its last fix, the regions counted
/// now), not among the tracks; `get_nomination_options` shows it blocking, as an item of
/// subject `lost_contact` (since when, what an operator's acceptance would do); reading logs
/// nothing. No tool accepts one: none is named for it, no tool's arguments carry an
/// acceptance, and a nomination an agent sends naming one is decided without it (rejected on
/// the lost contact, nothing accepted).
#[tokio::test(start_paused = true)]
async fn agents_read_lost_contacts_and_no_tool_accepts_one() {
    let r = rig();
    r.vehicle(2, Firmware::Copter, "fpv", (37.8120, -122.4900)); // armed, 440 m away
    sleep(Duration::from_millis(500)).await;
    r.clock.set(snapshot::ms(crate::clock::wall()));
    lost_alongside(&r);
    let lines = r.log.lines().len();
    // get_tracks: under lost, not among the tracks
    let v = r.ok(&watch(), "get_tracks", json!({ "include_stale": true })).await;
    let lost = v["lost"].as_array().expect("lost");
    assert_eq!(lost.len(), 1, "{v}");
    assert_eq!(lost[0]["id"], "T-0002", "{v}");
    assert!(lost[0]["out_of_view_s"].as_f64().unwrap_or(0.0) > 180.0, "{v}");
    assert!(!lost[0]["regions"].as_array().expect("regions").is_empty(), "{v}");
    let since = lost[0]["since_t"].as_i64().expect("since");
    assert!(v["tracks"].as_array().expect("tracks").iter().all(|t| t["id"] != "T-0002"), "{v}");
    // get_nomination_options: it blocks, an item of its own
    let v = r
        .ok(&planner(), "get_nomination_options", json!({ "track_id": "T-0001", "effect_radius_m": 75.0 }))
        .await;
    let o = &v["options"][0];
    assert_eq!(o["passed"], false, "{o}");
    let items = o["keep_outs"]["items"].as_array().expect("items");
    let item = items.iter().find(|i| i["subject"] == "lost_contact").expect("the lost contact");
    assert_eq!(item["id"], "T-0002", "{item}");
    assert_eq!(item["blocks"], true, "{item}");
    assert_eq!(item["lost"]["track"], "T-0002", "{item}");
    assert_eq!(item["lost"]["since"], since, "{item}");
    assert!(item["reason"].as_str().unwrap_or("").starts_with("Out of view since"), "{item}");
    assert!(item["other"].as_str().unwrap_or("").contains("An operator with engagement authority"), "{item}");
    assert_eq!(r.log.lines().len(), lines, "reading logs nothing");
    // no tool accepts one: none is named for it, none takes an acceptance; both reads are
    // marked read-only
    for t in tools::tools() {
        let schema = serde_json::to_string(&t.input_schema).expect("schema");
        assert!(!t.name.contains("accept"), "{}", t.name);
        assert!(!schema.contains("accept_lost") && !schema.contains("accept_unknown"), "{}: {schema}", t.name);
        if t.name == "get_tracks" || t.name == "get_nomination_options" {
            assert_eq!(t.annotations.as_ref().and_then(|a| a.read_only_hint), Some(true), "{}", t.name);
        }
    }
    // a nomination naming an acceptance: not an agent's to give, so refused (403, as the REST
    // path refuses it), never decided without it; nothing nominated or accepted
    let created = r.events("intent.created");
    let mut n = nomination(2, Some("T-0001 is declared hostile and holding in ez-gate-west"));
    n["accept_lost"] = json!([{ "track": "T-0002", "since": since }]);
    let (refused, out) = r.call(&planner(), "propose_intent", n).await;
    assert!(refused && out["status"] == 403, "{out}");
    assert!(out["error"].as_str().unwrap_or("").contains("an agent can't accept a lost contact"), "{out}");
    let mut n = nomination(2, Some("T-0001 is declared hostile and holding in ez-gate-west"));
    n["accept_unknown"] = json!([{ "uid": 3, "since": null }]);
    let (refused, out) = r.call(&planner(), "propose_intent", n).await;
    assert!(refused && out["status"] == 403, "{out}");
    assert_eq!(r.events("intent.created"), created, "nothing nominated");
    assert_eq!(r.events("engage.accepted"), 0, "nothing accepted");
    let rep = r.replays();
    assert_eq!(rep.lost_contacts, 1);
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

/// Review M2: the nomination dry run reads what a decision would: with a nomination already
/// waiting on a platform, `platform_free` fails there, in the console's API and for agents
/// (before, its inputs left the engagements in play out and it passed). Nothing is logged; an
/// agent's dry runs are at most one a second.
#[tokio::test(start_paused = true)]
async fn the_dry_run_sees_an_engagement_already_in_play_on_the_platform() {
    let r = rig();
    r.vehicle(2, Firmware::Copter, "fpv", (37.8120, -122.4900)); // armed, 440 m away
    sleep(Duration::from_millis(500)).await;
    r.clock.set(snapshot::ms(crate::clock::wall()));
    hostile_track(&r);
    let out = r
        .ok(&planner(), "propose_intent", nomination(2, Some("T-0001 is declared hostile and holding in ez-gate-west")))
        .await;
    let id = out["id"].as_u64().expect("id");
    assert_eq!(out["state"], "pending_approval", "{out}");
    let lines = r.log.lines().len();
    let free = |v: &Value| {
        v["options"][0]["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .find(|c| c["rule"] == "platform_free")
            .cloned()
            .expect("platform_free")
    };
    // agents
    let args = json!({ "track_id": "T-0001", "effect_radius_m": 100.0 });
    let v = r.ok(&watch(), "get_nomination_options", args.clone()).await;
    let c = free(&v);
    assert_eq!(c["outcome"], "fail", "{c}");
    assert!(c["detail"].as_str().unwrap_or("").contains(&format!("engagement i{id} in play")), "{c}");
    // the console's API, as an operator
    let req = Request::builder()
        .method("POST")
        .uri("/api/engage/options")
        .header(header::CONTENT_TYPE, "application/json")
        .header("X-Murmur-Operator", "op-1")
        .body(Body::from(json!({ "track": "T-0001", "effect_radius_m": 100.0 }).to_string()))
        .expect("request");
    let resp = r.api.clone().oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22).await.expect("body");
    let v: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(free(&v)["outcome"], "fail", "{v}");
    assert_eq!(r.log.lines().len(), lines, "a dry run logs nothing");
    // at most one a second per agent; another agent is not held up, and a second later it may
    let (err, v) = r.call(&watch(), "get_nomination_options", args.clone()).await;
    assert!(err && v["error"].as_str().unwrap_or("").contains("at most one call a second"), "{v}");
    let _ = r.ok(&planner(), "get_nomination_options", args.clone()).await;
    sleep(Duration::from_secs(1)).await;
    let _ = r.ok(&watch(), "get_nomination_options", args).await;
    // passed_only: whether each passes, and nothing else (the watch officer's own poll)
    sleep(Duration::from_secs(1)).await;
    let small = r
        .ok(
            &watch(),
            "get_nomination_options",
            json!({ "track_id": "T-0001", "effect_radius_m": 100.0, "uids": [2], "passed_only": true }),
        )
        .await;
    let o = &small["options"][0];
    assert_eq!(
        (o["uid"].as_u64(), o["passed"].as_bool(), o["callsign"].is_string()),
        (Some(2), Some(false), true),
        "{small}"
    );
    assert_eq!(o.as_object().map(|m| m.len()), Some(3), "uid, callsign, passed: {o}");
    assert_eq!(r.state(id), State::PendingApproval);
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

