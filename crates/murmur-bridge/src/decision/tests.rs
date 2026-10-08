//! The whole path against a fake world: fake autopilots (the task layer's test doubles) in
//! a real `World`, the real policy, catalog and areas from `sim/`, a fixed land/water map,
//! a manual engine clock and an in-memory audit log that must replay.
//!
//! Time: every rig starts at [`T0_S`] and runs the bridge's clock on its engine's
//! (`clock::test_clock`, per thread): the manual clock the test moves, or the bridge's own
//! clock moving with the paused runtime's time. What a test stamps (the vehicles' facts, the
//! sensing model's draws) is then the same whatever second it starts in and however loaded
//! the host is (bd murmur-ek3); before, facts were stamped by the wall clock while the
//! engine's clock was the test's.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use murmur_decide::{
    AgentId, Catalog, ManualClock, MemorySink, Polygon, PolygonTerrain, Rationale, replay,
};
use serde_json::json;
use tokio::time::sleep;

use super::*;
use crate::tasks::tests::{Firmware, quick, spawn_fake};
use crate::vehicle::Identity;
use crate::world::Config;

// Inside the approved Golden Gate area, south of the strait.
const LAT: f64 = 37.80;
const LON: f64 = -122.45;
/// On the water north of Crissy Field.
const SEA: (f64, f64) = (37.825, -122.47);

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

struct Rig {
    world: Arc<World>,
    d: Arc<Decision>,
    clock: ManualClock,
    log: MemorySink,
}

/// Where every rig's clock starts: 2026-06-14T10:00:00Z.
pub(super) const T0_S: f64 = 1_781_431_200.0;

/// The clock a rig's engine reads: the manual one the test moves (`Rig::clock`), or the
/// bridge's own as `Decision::load` gives it ([`clock::BridgeClock`]: the monotonic rate,
/// steered within about 2 s of the wall clock; pinned to the rig's start here).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum EngineClock {
    Manual,
    Bridge,
}

/// The sim policy as shipped (a human's own request in a human tier is approved at
/// submission).
fn rig() -> Rig {
    rig_with(Policy::load(&root().join("sim/policy.toml")).expect("policy"))
}

/// The sim policy with `submitter_authorizes = false` everywhere: an operator's own request
/// in a human tier waits in the inbox (as a two-person tier would), so the inbox paths run.
fn waiting_rig() -> Rig {
    rig_with(waiting_policy())
}

/// The policy of [`waiting_rig`].
fn waiting_policy() -> Policy {
    let mut p = Policy::load(&root().join("sim/policy.toml")).expect("policy");
    p.approval.submitter_authorizes = Some(false);
    for t in &mut p.tiers {
        t.submitter_authorizes = None;
    }
    p
}

fn rig_with(policy: Policy) -> Rig {
    rig_auth(policy, murmur_decide::AuthMode::Dev)
}

/// [`rig_with`] authenticating operators as `auth` says (rules 8).
fn rig_auth(policy: Policy, auth: murmur_decide::AuthMode) -> Rig {
    rig_clocked(policy, auth, EngineClock::Manual)
}

/// [`rig_auth`] with the engine on `engine_clock`.
fn rig_clocked(policy: Policy, auth: murmur_decide::AuthMode, engine_clock: EngineClock) -> Rig {
    let root = root();
    let world = World::new(Config {
        base_port: 0,
        http_port: 0,
        default_ports: 0,
        manifest: root.join("sim/run/no-such-manifest.json"),
        platforms: root.join("sim/platforms.toml"),
        root: root.clone(),
    });
    // the bridge's clock: the test's manual one, or (the engine on the bridge's own) pinned
    // to the same start and moving with the paused runtime's time
    let clock = ManualClock::new(snapshot::ms(T0_S));
    let engine: Arc<dyn Clock + Send + Sync> = match engine_clock {
        EngineClock::Manual => {
            clock::test_clock::follow(&clock);
            Arc::new(clock.clone())
        }
        EngineClock::Bridge => {
            clock::test_clock::pin(T0_S);
            Arc::new(clock::BridgeClock)
        }
    };
    let log = MemorySink::new();
    let terrain = Terrain::fixed(PolygonTerrain {
        coverage: ring(-122.6, 37.7, -122.3, 37.9),
        // the Presidio shore, south of the test boat
        land: vec![ring(-122.48, 37.79, -122.44, 37.808)],
        water_depth_m: 12.0,
    });
    let d = Decision::from_parts(
        world.clone(),
        Parts {
            policy,
            catalog: Catalog::load(&root.join("sim/platforms.toml")).expect("catalog"),
            areas: AreaSet::load_dir(&root.join("sim/areas"), clock.now()).expect("areas"),
            sink: Box::new(log.clone()),
            audit_path: PathBuf::from("memory"),
            clock: engine,
            terrain,
            operators: vec![OperatorId::new("op-1"), OperatorId::new("op-2")],
            engage_operators: Vec::new(),
            auth,
            tracks: None,
            passkeys: None,
            study: None,
            scripted: Vec::new(),
            carried_from: Vec::new(),
            pointer: None,
        },
    )
    .expect("engine");
    Rig {
        world,
        d,
        clock,
        log,
    }
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

    fn state(&self, id: IntentId) -> State {
        self.d
            .lock()
            .engine
            .intent(id)
            .map(Intent::state)
            .expect("intent")
    }

    fn reason(&self, id: IntentId) -> String {
        let core = self.d.lock();
        let i = core.engine.intent(id).expect("intent");
        i.history()
            .last()
            .map(|h| h.reason.clone())
            .unwrap_or_default()
    }

    /// Tick once a second (tokio time) until the intent leaves `dispatched`.
    async fn run_until_finished(&self, id: IntentId, limit_s: u64) -> State {
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
}

fn op1() -> Actor {
    Actor::asserted(OperatorId::new("op-1"))
}

fn orbit(uid: Uid) -> IntentRequest {
    IntentRequest {
        task: "orbit".into(),
        uids: vec![uid],
        params: TaskParams {
            center: Some([LON + 0.004, LAT]),
            radius: Some(150.0),
            ..TaskParams::default()
        },
        space: space::SpaceParams::default(),
        engage: engage::EngageParams::default(),
        constraints: Constraints::default(),
        rationale: None,
    }
}

fn simple(task: &str, uid: Uid) -> IntentRequest {
    IntentRequest {
        task: task.into(),
        uids: vec![uid],
        params: TaskParams::default(),
        space: space::SpaceParams::default(),
        engage: engage::EngageParams::default(),
        constraints: Constraints::default(),
        rationale: None,
    }
}

fn patrol(uid: Uid, route: Vec<[f64; 2]>) -> IntentRequest {
    IntentRequest {
        task: "patrol".into(),
        uids: vec![uid],
        params: TaskParams {
            route: Some(route),
            ..TaskParams::default()
        },
        space: space::SpaceParams::default(),
        engage: engage::EngageParams::default(),
        constraints: Constraints::default(),
        rationale: None,
    }
}

#[test]
fn the_bridge_does_not_start_with_an_engagement_operator_who_is_no_operator() {
    let root = root();
    let parts = |operators: Vec<OperatorId>, engage_operators: Vec<OperatorId>| Parts {
        policy: Policy::load(&root.join("sim/policy.toml")).expect("policy"),
        catalog: Catalog::load(&root.join("sim/platforms.toml")).expect("catalog"),
        areas: AreaSet::empty("test", murmur_decide::Time(0)),
        sink: Box::new(MemorySink::new()),
        audit_path: PathBuf::from("memory"),
        clock: Arc::new(ManualClock::new(murmur_decide::Time(0))),
        terrain: Terrain::fixed(PolygonTerrain {
            coverage: ring(-122.6, 37.7, -122.3, 37.9),
            land: Vec::new(),
            water_depth_m: 12.0,
        }),
        operators,
        engage_operators,
        auth: murmur_decide::AuthMode::Dev,
        tracks: None,
        passkeys: None,
        study: None,
        scripted: Vec::new(),
        carried_from: Vec::new(),
        pointer: None,
    };
    let world = || {
        World::new(Config {
            base_port: 0,
            http_port: 0,
            default_ports: 0,
            manifest: root.join("sim/run/no-such-manifest.json"),
            platforms: root.join("sim/platforms.toml"),
            root: root.clone(),
        })
    };
    // MURMUR_ENGAGE_OPERATORS=op-2 with MURMUR_OPERATORS=op-1: refused
    let err = Decision::from_parts(world(), parts(vec![OperatorId::new("op-1")], vec![OperatorId::new("op-2")]))
        .err()
        .expect("refused");
    assert!(err.contains("op-2 is not on the operator roster"), "{err}");
    // listed as an operator too: it starts, and run.started names the roster
    let log = MemorySink::new();
    let mut p = parts(vec![OperatorId::new("op-1"), OperatorId::new("op-2")], vec![OperatorId::new("op-2")]);
    p.sink = Box::new(log.clone());
    Decision::from_parts(world(), p).expect("starts");
    let first = log.lines().into_iter().next().expect("run.started");
    assert!(
        first.contains("\"roster\":{\"operators\":[\"op-1\",\"op-2\"],\"engage\":[\"op-2\"],\"auth\":\"dev\"}"),
        "{first}"
    );
}

#[tokio::test(start_paused = true)]
async fn auto_path_dispatches_and_finishes() {
    let r = rig();
    r.vehicle(1, Firmware::Copter, "x10", (LAT, LON));
    sleep(Duration::from_millis(500)).await;
    let out = r.d.submit(&op1(), orbit(1)).await.expect("submit");
    assert_eq!(out.state, State::Dispatched, "{}", r.reason(out.id));
    let tasking = out.tasking.expect("dispatched now");
    assert_eq!(tasking.intent_id, out.id.0);
    assert_eq!(r.run_until_finished(out.id, 300).await, State::Done);
    assert!(r.events("op.state") >= 2); // sampled once a second, ending with done
    let report = r.replays();
    assert_eq!(report.approvals, 0); // policy, not a human
}

#[tokio::test(start_paused = true)]
async fn human_path_waits_then_runs_once_approved() {
    let r = waiting_rig();
    r.vehicle(2, Firmware::Copter, "switchblade", (LAT, LON)); // armed
    sleep(Duration::from_millis(500)).await;
    let out = r.d.submit(&op1(), orbit(2)).await.expect("submit");
    assert_eq!(out.state, State::PendingApproval);
    assert!(out.tasking.is_none());
    assert!(
        r.d.world.tasks.snapshot().is_empty(),
        "nothing reached the vehicles"
    );
    let inbox = r.d.inbox_json();
    assert_eq!(inbox["counts"]["pending"], 1);
    let why = inbox["pending"][0]["why"].to_string();
    assert!(why.contains("armed"), "{why}");

    // a second operator approves: dispatched, flown, done
    let approved =
        r.d.approve(&Actor::asserted(OperatorId::new("op-2")), out.id.0, Some("clear".into()))
            .await
            .expect("approve");
    assert_eq!(approved.state, State::Dispatched);
    assert!(approved.tasking.is_some());
    assert_eq!(r.run_until_finished(out.id, 300).await, State::Done);
    let report = r.replays();
    assert_eq!((report.approvals, report.self_approvals), (1, 0));
}

#[tokio::test(start_paused = true)]
async fn an_operators_own_human_tier_task_runs_at_once_an_agents_waits() {
    let r = rig();
    r.vehicle(11, Firmware::Copter, "switchblade", (LAT, LON)); // armed
    r.vehicle(12, Firmware::Copter, "switchblade", (LAT, LON + 0.001));
    sleep(Duration::from_millis(500)).await;

    // The operator's own request: the submission is the human decision.
    let out = r.d.submit(&op1(), orbit(11)).await.expect("submit");
    assert_eq!(out.state, State::Dispatched, "{}", r.reason(out.id));
    assert!(out.tasking.is_some(), "dispatched now");
    let auth = out.authorization.expect("authorized at submission");
    assert_eq!(auth["by"], "op-1");
    assert_eq!(auth["tier"], "armed-platform");
    assert!(auth["why"].to_string().contains("armed"), "{auth}");
    assert_eq!(out.intent["approval"]["at_submission"], true);
    assert_eq!(out.intent["approval"]["self_approved"], true);
    assert_eq!(r.events("approval.at_submission"), 1);
    assert_eq!(r.events("approval.requested"), 0);

    // An agent's proposal in the same tier waits in the inbox for an operator.
    let proposal = proposal(&orbit(12)).expect("proposal");
    let id = {
        let mut guard = r.d.lock();
        let world = r.d.snapshot();
        let core = &mut *guard;
        core.engine
            .agent_port(AgentId::new("planner"))
            .propose(
                proposal,
                Rationale::new("cover the east approach").expect("rationale"),
                &world,
                &r.d.terrain.oracle(),
            )
            .expect("propose")
            .id()
    };
    assert_eq!(r.state(id), State::PendingApproval);
    let inbox = r.d.inbox_json();
    assert_eq!(inbox["counts"]["pending"], 1);
    assert_eq!(inbox["pending"][0]["id"], id.0);
    assert_eq!(inbox["pending"][0]["source"]["kind"], "agent");
    let approved = r.d.approve(&op1(), id.0, None).await.expect("approve");
    assert_eq!(approved.state, State::Dispatched);
    assert!(approved.authorization.is_none(), "an inbox approval");
    let report = r.replays();
    assert_eq!((report.approvals, report.approvals_at_submission), (1, 1));
}

#[tokio::test(start_paused = true)]
async fn rejected_and_expired_intents_never_reach_a_vehicle() {
    let r = waiting_rig();
    r.vehicle(3, Firmware::Copter, "switchblade", (LAT, LON));
    r.vehicle(4, Firmware::Copter, "switchblade", (LAT, LON + 0.001));
    sleep(Duration::from_millis(500)).await;
    let a = r.d.submit(&op1(), orbit(3)).await.expect("submit").id;
    let b = r.d.submit(&op1(), orbit(4)).await.expect("submit").id;
    assert_eq!(
        r.d.reject(&op1(), a.0, Some("no".into()))
            .expect("reject")
            .state,
        State::Rejected
    );
    // rejecting twice is a conflict, not a second decision
    assert_eq!(
        r.d.reject(&op1(), a.0, None).err().map(|e| e.status),
        Some(409)
    );

    r.clock.advance_ms(121_000); // past approval.expire_s
    r.d.tick();
    assert_eq!(r.state(b), State::Expired);
    let late = r.d.approve(&op1(), b.0, None).await.expect_err("too late");
    assert_eq!(late.status, 409);
    assert!(r.d.world.tasks.snapshot().is_empty());
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn agents_propose_but_only_listed_operators_approve() {
    let r = rig();
    r.vehicle(5, Firmware::Copter, "switchblade", (LAT, LON));
    sleep(Duration::from_millis(500)).await;
    let proposal = proposal(&orbit(5)).expect("proposal");
    let id = {
        let mut guard = r.d.lock();
        let world = r.d.snapshot();
        let core = &mut *guard;
        core.engine
            .agent_port(AgentId::new("planner"))
            .propose(
                proposal,
                Rationale::new("cover the east approach").expect("rationale"),
                &world,
                &r.d.terrain.oracle(),
            )
            .expect("propose")
            .id()
    };
    assert_eq!(r.state(id), State::PendingApproval);
    // the agent can't authenticate as an operator, and a request without one is refused
    assert_eq!(
        r.d.operator(Some("planner"), false).err().map(|e| e.status),
        Some(403)
    );
    assert_eq!(r.d.operator(None, false).err().map(|e| e.status), Some(401));
    assert_eq!(r.state(id), State::PendingApproval);
    // a human can
    let out = r.d.approve(&op1(), id.0, None).await.expect("approve");
    assert_eq!(out.state, State::Dispatched);
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn the_world_voids_waiting_intents() {
    let r = waiting_rig();
    r.vehicle(6, Firmware::Copter, "switchblade", (LAT, LON));
    r.vehicle(7, Firmware::Copter, "switchblade", (LAT, LON + 0.001));
    sleep(Duration::from_millis(500)).await;

    // superseded: a hold (auto) dispatched to the vehicle voids the orbit waiting for it
    let waiting = r.d.submit(&op1(), orbit(6)).await.expect("submit").id;
    let hold = r.d.submit(&op1(), simple("hold", 6)).await.expect("hold");
    assert_eq!(hold.state, State::Dispatched);
    assert_eq!(r.state(waiting), State::Voided);
    assert!(
        r.reason(waiting).contains("superseded"),
        "{}",
        r.reason(waiting)
    );

    // lost: no heartbeat for longer than max_fact_age_s
    let lost = r.d.submit(&op1(), orbit(7)).await.expect("submit").id;
    let cell = r
        .world
        .vehicles()
        .into_iter()
        .find(|c| c.uid == 7)
        .expect("cell");
    cell.write(|v| v.seen.heartbeat = Some(clock::wall() - 60.0));
    r.d.tick();
    assert_eq!(r.state(lost), State::Voided);
    assert!(r.reason(lost).contains("lost"), "{}", r.reason(lost));
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn a_low_rate_link_is_lost_only_after_missed_reports() {
    use crate::mav::MavMessage;
    let r = waiting_rig();
    r.vehicle(7, Firmware::Copter, "switchblade", (LAT, LON));
    sleep(Duration::from_millis(500)).await;
    let cell = r
        .world
        .vehicles()
        .into_iter()
        .find(|c| c.uid == 7)
        .expect("cell");
    // an acoustic link: reports every 20 s, so 60 s of silence is the limit
    cell.link_on_message(&MavMessage::HIGH_LATENCY2(Default::default()));
    let waiting = r.d.submit(&op1(), orbit(7)).await.expect("submit").id;
    assert_eq!(r.state(waiting), State::PendingApproval);
    // between reports (45 s, past max_fact_age_s but under 3 intervals): not voided
    cell.write(|v| v.seen.heartbeat = Some(clock::wall() - 45.0));
    r.d.tick();
    assert_eq!(r.state(waiting), State::PendingApproval);
    // three missed reports: voided
    cell.write(|v| v.seen.heartbeat = Some(clock::wall() - 65.0));
    r.d.tick();
    assert_eq!(r.state(waiting), State::Voided);
    assert!(r.reason(waiting).contains("lost"), "{}", r.reason(waiting));
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn the_rules_reject_land_waypoints_and_no_go_crossings() {
    let r = rig();
    r.vehicle(8, Firmware::Rover, "magura", SEA);
    r.vehicle(9, Firmware::Copter, "x10", (LAT, LON));
    sleep(Duration::from_millis(500)).await;

    // a boat patrol with its second waypoint on the Presidio
    let boat =
        r.d.submit(&op1(), patrol(8, vec![[SEA.1, SEA.0], [-122.46, 37.80]]))
            .await
            .expect("submit");
    assert_eq!(boat.state, State::Rejected);
    assert!(
        r.reason(boat.id).contains("domain"),
        "{}",
        r.reason(boat.id)
    );

    // a patrol straight across Alcatraz
    let across =
        r.d.submit(
            &op1(),
            patrol(9, vec![[-122.432, 37.8265], [-122.414, 37.8265]]),
        )
        .await
        .expect("submit");
    assert_eq!(across.state, State::Rejected);
    assert!(
        r.reason(across.id).contains("alcatraz"),
        "{}",
        r.reason(across.id)
    );
    assert!(r.d.world.tasks.snapshot().is_empty());

    // malformed requests never become intents
    let unknown =
        r.d.submit(&op1(), simple("hold", 99))
            .await
            .expect_err("404");
    assert_eq!(unknown.status, 404);
    // a collect names no vehicles: refused before any intent
    let space =
        r.d.submit(&op1(), simple("collect", 9))
            .await
            .expect_err("400");
    assert_eq!(space.status, 400);
    assert!(space.detail.contains("satellites"), "{}", space.detail);
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn vehicle_events_reach_the_audit_log() {
    let r = rig();
    r.vehicle(10, Firmware::Copter, "x10", (LAT, LON));
    sleep(Duration::from_millis(500)).await;
    let out = r.d.submit(&op1(), simple("hold", 10)).await.expect("hold");
    assert_eq!(r.run_until_finished(out.id, 60).await, State::Done);
    assert!(
        r.events("vehicle.event") >= 1,
        "the mode change to BRAKE is logged"
    );
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn operator_views_are_logged_under_the_session() {
    let r = waiting_rig();
    r.vehicle(2, Firmware::Copter, "switchblade", (LAT, LON)); // armed: waits
    sleep(Duration::from_millis(500)).await;
    let out = r.d.submit(&op1(), orbit(2)).await.expect("submit");
    assert_eq!(out.state, State::PendingApproval);
    let view = |event: &str, action: Option<&str>, open_ms: Option<u64>| OperatorView {
        event: event.into(),
        action: action.map(str::to_string),
        shown: murmur_decide::ViewShown {
            checks: vec!["capability".into(), "readiness".into()],
            warnings: vec!["armed platform: vehicle 2".into()],
            rationale: false,
            tracks: vec!["T-0001".into()],
        },
        open_ms,
        client_t: None,
    };
    r.d.record_view(&op1(), out.id.0, view("open", None, None))
        .expect("open");
    let bad =
        r.d.record_view(&op1(), out.id.0, view("decide", None, Some(5)))
            .expect_err("malformed");
    assert_eq!(bad.status, 422);
    let missing =
        r.d.record_view(&op1(), 404, view("open", None, None))
            .expect_err("unknown");
    assert_eq!(missing.status, 404);
    r.d.record_view(
        &op1(),
        out.id.0,
        view("decide", Some("reject"), Some(12_000)),
    )
    .expect("decide");
    r.d.reject(&op1(), out.id.0, Some("not now".into()))
        .expect("reject");
    let late =
        r.d.record_view(&op1(), out.id.0, view("decide", Some("approve"), Some(1)))
            .expect_err("late");
    assert_eq!(late.status, 409, "a decide view after the decision");
    r.d.record_view(&op1(), out.id.0, view("close", None, Some(13_000)))
        .expect("a close is taken after the decision");
    // a burst beyond the budget is refused (instrumentation, not storage)
    let refused = (0..60)
        .filter_map(|_| {
            r.d.record_view(&op1(), out.id.0, view("close", None, None))
                .err()
        })
        .collect::<Vec<_>>();
    assert!(!refused.is_empty() && refused.iter().all(|e| e.status == 429), "{refused:?}");
    assert!(r.events("operator.view") < 3 + 60);
    assert!(
        r.log.lines().iter().any(
            |l| l.contains("\"type\":\"operator.view\"") && l.contains("\"operator\":\"op-1\"")
        ),
        "the view names the operator"
    );
    assert_eq!(r.state(out.id), State::Rejected);
    r.replays();
}

/// Collect and cover through the bridge (`tests/space_tasking.rs`).
mod space_tasking;

/// Engagements through the bridge (`tests/engagement.rs`).
mod engagement;

/// The queue for unreachable vehicles through the bridge (`tests/queue.rs`, rules 5).
mod queue;

/// Rules 8 through the bridge's HTTP surface: logins, step-ups, agent keys (`tests/auth.rs`).
mod auth;

/// Rules 10 through the bridge's HTTP surface: passkeys registered, used to sign in and to
/// step up, bound to the act, refused when replayed, moved or cloned (`tests/passkeys.rs`).
mod passkeys;

/// Rules 9 through the bridge: observed acts, rule declarations, review, revocation
/// (`tests/identification.rs`).
mod identification;

/// A wall clock that steps, through the bridge (`tests/clock_steps.rs`, murmur-dup).
mod clock_steps;

/// A situation runner's status through the bridge: display only (`tests/situation.rs`).
mod situation;
