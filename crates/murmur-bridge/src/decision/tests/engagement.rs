//! Engagements through the bridge: an operator nominates on a declared-hostile track, only
//! operators with engagement authority (never the proposer) sign it, the platform is sent to
//! loiter at the effect point, the loop releases the abstract effect onto the simulation
//! channel; an operator's abort and a changed picture stop it with the recorded fallback.
//! Every log replays.
//!
//! Time: the engine's clock is advanced a second per tick while vehicle facts are stamped
//! by the wall clock, so these runs stay within a few engine seconds (the platform starts
//! on the target unless a test says otherwise, the dispatch delay is zero); the full
//! timeline is the decision layer's scenarios (`sim/scenarios/engage`). Sensing noise is
//! drawn per wall-clock second, so a test whose platform starts on the target may find the
//! release ready on its first tick at some start seconds (bd murmur-ek3).

use murmur_decide::Affiliation;

use super::*;
use crate::tracks::Tracks;
use crate::tracks::sensing::{Observer, SensorSpec, SensorType};
use crate::tracks::truth::{ContactTruth, Domain as TDomain, Sensing, TruthAffiliation, TruthMessage};

/// In ez-gate-west, west of the bridge.
const TARGET: (f64, f64) = (37.8120, -122.4950);

pub(super) struct Erig {
    pub(super) r: Rig,
    pub(super) tracks: Arc<Tracks>,
}

fn engage_rig() -> Erig {
    engage_rig_on(None)
}

/// The engagement rig, on `world` when given (a second run of the bridge beside the same
/// vehicles).
fn engage_rig_on(on: Option<(Arc<World>, Arc<Tracks>)>) -> Erig {
    engage_rig_with(on, PathBuf::from("memory"))
}

/// The engagement rig whose audit log is said to be `audit_path` (the sink is in memory).
fn engage_rig_with(on: Option<(Arc<World>, Arc<Tracks>)>, audit_path: PathBuf) -> Erig {
    engage_rig_auth(on, audit_path, murmur_decide::AuthMode::Dev)
}

/// [`engage_rig_with`], its policy changed by `tweak` first.
fn engage_rig_policy(on: Option<(Arc<World>, Arc<Tracks>)>, audit_path: PathBuf, tweak: impl FnOnce(&mut Policy)) -> Erig {
    engage_rig_auth_policy(on, audit_path, murmur_decide::AuthMode::Dev, tweak)
}

/// [`engage_rig_with`] authenticating its operators as `auth` says (rules 8).
pub(super) fn engage_rig_auth(
    on: Option<(Arc<World>, Arc<Tracks>)>,
    audit_path: PathBuf,
    auth: murmur_decide::AuthMode,
) -> Erig {
    engage_rig_auth_policy(on, audit_path, auth, |_| {})
}

/// [`engage_rig_auth`] with the sim policy changed by `tweak` first.
pub(super) fn engage_rig_auth_policy(
    on: Option<(Arc<World>, Arc<Tracks>)>,
    audit_path: PathBuf,
    auth: murmur_decide::AuthMode,
    tweak: impl FnOnce(&mut Policy),
) -> Erig {
    engage_rig_passkeys(on, audit_path, auth, tweak, None, None)
}

/// [`engage_rig_auth_policy`] with (rules 10) the run's relying party and passkeys, and a study.
pub(super) fn engage_rig_passkeys(
    on: Option<(Arc<World>, Arc<Tracks>)>,
    audit_path: PathBuf,
    auth: murmur_decide::AuthMode,
    tweak: impl FnOnce(&mut Policy),
    passkeys: Option<murmur_decide::PasskeyRoster>,
    study: Option<murmur_decide::Study>,
) -> Erig {
    let root = root();
    let mut policy = Policy::load(&root.join("sim/policy.toml")).expect("policy");
    tweak(&mut policy);
    let mut e = policy.rules.engage_rules();
    e.dispatch_delay_s = 0.0;
    policy.rules.engage = Some(e);
    let (world, tracks) = on.unwrap_or_else(|| {
        let tracks = Arc::new(Tracks::load(&root.join("sim/platforms.toml")).expect("sensors"));
        let world = World::new(Config {
            base_port: 0,
            http_port: 0,
            default_ports: 0,
            manifest: root.join("sim/run/no-such-manifest.json"),
            platforms: root.join("sim/platforms.toml"),
            root: root.clone(),
        });
        (world, tracks)
    });
    let clock = ManualClock::new(snapshot::ms(clock::wall()));
    let log = MemorySink::new();
    let terrain = Terrain::fixed(PolygonTerrain {
        coverage: ring(-122.6, 37.7, -122.3, 37.9),
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
            audit_path,
            clock: Arc::new(clock.clone()),
            terrain,
            operators: vec![OperatorId::new("op-1"), OperatorId::new("op-2"), OperatorId::new("op-3")],
            engage_operators: vec![OperatorId::new("op-2"), OperatorId::new("op-3")],
            auth,
            tracks: Some(tracks.clone()),
            passkeys,
            study,
        },
    )
    .expect("engine");
    Erig {
        r: Rig {
            world,
            d,
            clock,
            log,
        },
        tracks,
    }
}

fn op(s: &str) -> Actor {
    Actor::asserted(OperatorId::new(s))
}

impl Erig {
    pub(super) fn now_s(&self) -> f64 {
        self.r.clock.now().0 as f64 / 1000.0
    }

    /// One truth tick: the hostile boat holding at TARGET, seen by a radar and EO/IR
    /// observer 1.5 km east (not one of our fleet vehicles: an explicit observer).
    fn sense(&self, with_target: bool) {
        self.sense_at(self.now_s(), with_target);
    }

    pub(super) fn sense_at(&self, now_s: f64, with_target: bool) {
        let observer = Observer {
            uid: 9,
            lat: TARGET.0,
            lon: TARGET.1 + 0.017,
            alt_msl: 0.0,
            mast_m: 4.0,
            submerged: false,
            sensors: vec![
                SensorSpec { kind: SensorType::Radar, range_m: 8_000.0, guess: true, height: false },
                SensorSpec { kind: SensorType::EoIr, range_m: 5_000.0, guess: true, height: false },
            ],
        };
        let contacts = if with_target {
            vec![ContactTruth {
                id: "fastboat".into(),
                name: "fast boat".into(),
                affiliation: TruthAffiliation::Hostile,
                domain: TDomain::Surface,
                kind: "small_fast_boat".into(),
                lat: TARGET.0,
                lon: TARGET.1,
                alt_m: 0.0,
                speed_mps: 0.0,
                course_deg: 0.0,
                emits: false,
                signature: 1.0,
                ais: None,
            }]
        } else {
            Vec::new()
        };
        let msg = TruthMessage {
            t: now_s,
            s: now_s,
            seq: 0,
            scenario: "test".into(),
            seed: 1,
            sensing: Sensing {
                misclassification_rate: 0.0,
            },
            contacts,
            acts: Vec::new(),
        };
        self.tracks
            .on_truth_with(msg, std::slice::from_ref(&observer), now_s)
            .expect("sensing");
    }

    /// A minute of detections before anyone acts (identity confidence builds with dwell),
    /// then the declaration.
    async fn picture(&self) {
        // the last 90 s, ending now (the vehicles' facts are stamped with the wall clock)
        self.r.clock.set(snapshot::ms(clock::wall()));
        let now = self.now_s();
        for s in 0..90 {
            self.sense_at(now - 90.0 + f64::from(s), true);
        }
        self.sense(true);
        // rules 9: through the engine's identification ledger, under op-1's session
        self.r
            .d
            .declare(&op("op-1"), None, "T-0001", Affiliation::Hostile, None, None)
            .expect("declared");
    }

    pub(super) fn nomination(&self, fallback: &str) -> IntentRequest {
        IntentRequest {
            task: "engage".into(),
            uids: vec![1],
            params: TaskParams::default(),
            space: space::SpaceParams::default(),
            engage: engage::EngageParams {
                track: Some("T-0001".into()),
                effect_radius_m: Some(100.0),
                window_s: Some(300),
                fallback: Some(fallback.into()),
                accept_unknown: Vec::new(),
            },
            constraints: Constraints::default(),
            rationale: None,
        }
    }

    /// What the console asks for before a signature: the picture token (the plan and the
    /// track as the engine shows them to this operator).
    pub(super) fn look(&self, who: &str, id: IntentId) -> Result<Value, DecisionError> {
        self.r.d.issue_picture(&op(who), id.0)
    }

    /// A signature as the console sends it: look, then sign quoting the token.
    pub(super) async fn sign(&self, who: &str, id: IntentId) -> Result<Outcome, DecisionError> {
        // the console shows what refused the look (no authority, the proposer, signed)
        let token = self.look(who, id)?["id"].as_u64();
        self.r.d.approve_with(&op(who), id.0, None, token, false).await
    }

    /// Nominate (op-1) and sign twice (op-2, op-3): approved and dispatched.
    async fn approved(&self, fallback: &str) -> IntentId {
        self.approved_at(fallback, TARGET).await // armed, on the target
    }

    /// [`Erig::approved`] with the platform starting at `at` (lat, lon).
    async fn approved_at(&self, fallback: &str, at: (f64, f64)) -> IntentId {
        self.r.vehicle(1, Firmware::Copter, "fpv", at);
        sleep(Duration::from_millis(500)).await;
        self.picture().await;
        let out = self
            .r
            .d
            .submit(&op("op-1"), self.nomination(fallback))
            .await
            .expect("nominated");
        assert_eq!(out.state, State::PendingApproval, "{}", out.intent);
        let id = out.id;
        // no authority, the proposer's own (would be refused for the proposer anyway)
        let e = self.sign("op-1", id).await.expect_err("no authority");
        assert_eq!(e.status, 403, "{}", e.detail);
        let o = self.sign("op-2", id).await.expect("signs");
        assert_eq!(o.state, State::PendingApproval);
        let e = self.sign("op-2", id).await.expect_err("twice");
        assert_eq!(e.status, 409, "{}", e.detail);
        let o = self.sign("op-3", id).await.expect("signs");
        assert_eq!(o.state, State::Dispatched, "{}", o.intent);
        // rules 6: the intercept waits for the platform's lost-link preload to confirm
        assert!(o.tasking.is_none(), "no intercept before the preload confirms");
        self.preloaded(id).await;
        id
    }

    /// Rules 6: the loop reports the platform's lost-link preload (read, set, read back on
    /// the fake autopilot) and only then sends the intercept (its preload step alone here,
    /// so the engagement is still under way when the test goes on).
    pub(super) async fn preloaded(&self, id: IntentId) {
        for _ in 0..10 {
            sleep(Duration::from_millis(500)).await;
            let mut guard = self.r.d.lock();
            self.r.d.step_params(&mut guard);
            if guard.taskings.contains_key(&id) {
                break;
            }
        }
        let core = self.r.d.lock();
        let e = core.engine.intent(id).and_then(Intent::engagement).expect("engagement");
        let set = e.preloaded.as_ref().expect("the preload was reported");
        assert!(set.confirmed, "{set:?}");
        assert!(core.taskings.contains_key(&id), "the intercept went out after it");
    }

    /// One second: engine clock, a truth tick, the decision loop.
    async fn tick(&self, with_target: bool) {
        self.r.clock.advance_ms(1000);
        self.sense(with_target);
        self.r.d.tick();
        sleep(Duration::from_secs(1)).await;
    }
}

#[tokio::test(start_paused = true)]
async fn an_engagement_through_the_bridge_releases_an_abstract_effect() {
    let e = engage_rig();
    let id = e.approved("return").await;
    for _ in 0..4 {
        e.tick(true).await;
        if e.r.state(id) == State::Effected {
            break;
        }
    }
    assert_eq!(e.r.state(id), State::Effected, "{}", e.r.reason(id));
    // the effect waits on the simulation channel for the contacts process, with the
    // contact the track was associated with (truth side only)
    let effects = e.tracks.take_effects();
    assert_eq!(effects.len(), 1, "{effects:?}");
    assert_eq!(effects[0]["target_contact"], "fastboat");
    assert_eq!(effects[0]["radius_m"], 100.0);
    // the engaging platform is named apart, never among "ours" (collateral)
    assert_eq!(effects[0]["platform"], "1");
    assert!(
        effects[0]["ours"].as_array().expect("ours").iter().all(|o| o["uid"] != "1"),
        "{effects:?}"
    );
    // the console's view: who signed, the engagement record
    let msg: serde_json::Value = serde_json::from_str(&e.r.d.message()).expect("json");
    let mine = msg["intents"]
        .as_array()
        .expect("intents")
        .iter()
        .find(|i| i["id"] == id.0)
        .expect("intent")
        .clone();
    assert_eq!(mine["engage"]["signed"], json!(["op-2", "op-3"]));
    assert!(!mine["engagement"]["effect"].is_null(), "{mine}");
    assert_eq!(msg["operators"]["engage"], json!(["op-2", "op-3"]));
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
    assert_eq!((rep.concurrences, rep.approvals, rep.effects), (1, 1, 1));
}

#[tokio::test(start_paused = true)]
async fn an_operator_abort_commands_the_recorded_fallback() {
    let e = engage_rig();
    let id = e.approved("return").await;
    // any operator, no engagement authority needed
    let out = e.r.d.abort(&op("op-1"), id.0, Some("hold fire".into())).expect("aborted");
    assert_eq!(out.state, State::Aborted, "{}", out.intent);
    let ops = e.r.world.tasks.current_ops();
    let op1 = ops.iter().find(|o| o.uid == 1).expect("an op on the platform");
    assert_eq!(op1.task, crate::tasks::TaskKind::Rtl);
    assert!(e.r.d.abort(&op("op-1"), id.0, None).is_err(), "only once");
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
    assert_eq!(rep.aborts, 1);
}

#[tokio::test(start_paused = true)]
async fn a_changed_picture_aborts_with_the_fallback() {
    let e = engage_rig();
    let id = e.approved("abort").await;
    // the declaration is withdrawn (op-1 declares it neutral): the hostile one is revoked
    // first, which stops the engagement at once, with its fallback (rules 9: cause revoked)
    e.r.d
        .declare(&op("op-1"), None, "T-0001", Affiliation::Neutral, None, None)
        .expect("declared");
    e.tick(true).await;
    assert_eq!(e.r.state(id), State::Aborted, "{}", e.r.reason(id));
    assert!(e.r.reason(id).contains("target_declared"), "{}", e.r.reason(id));
    assert!(e.r.reason(id).contains("revoked by op-1"), "{}", e.r.reason(id));
    let ops = e.r.world.tasks.current_ops();
    let op1 = ops.iter().find(|o| o.uid == 1).expect("an op on the platform");
    assert_eq!(op1.task, crate::tasks::TaskKind::Hold);
    assert!(e.tracks.take_effects().is_empty(), "no effect after a void");
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
    assert_eq!((rep.revocations, rep.aborts), (1, 1));
}

#[tokio::test(start_paused = true)]
async fn the_proposer_with_authority_still_cannot_sign() {
    let e = engage_rig();
    e.r.vehicle(1, Firmware::Copter, "fpv", TARGET);
    sleep(Duration::from_millis(500)).await;
    e.picture().await;
    let id = e
        .r
        .d
        .submit(&op("op-2"), e.nomination("loiter"))
        .await
        .expect("nominated")
        .id;
    let err = e.sign("op-2", id).await.expect_err("proposer");
    assert_eq!(err.status, 403, "{}", err.detail);
    assert!(err.detail.contains("may not approve"), "{}", err.detail);
    assert_eq!(e.r.state(id), State::PendingApproval);
    // the inbox says who may still sign
    let inbox = e.r.d.inbox_json();
    let p = &inbox["pending"][0];
    assert_eq!(p["engage"]["eligible"], json!(["op-3"]), "{p}");
    assert!(p["engage"]["live"]["checks"].is_array(), "{p}");
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
}

#[tokio::test(start_paused = true)]
async fn a_signature_quotes_the_picture_token_it_was_shown() {
    let e = engage_rig();
    e.r.vehicle(1, Firmware::Copter, "fpv", TARGET);
    sleep(Duration::from_millis(500)).await;
    e.picture().await;
    let id = e.r.d.submit(&op("op-1"), e.nomination("return")).await.expect("nominated").id;
    // no engagement authority: nothing to sign
    let err = e.look("op-1", id).expect_err("no authority");
    assert_eq!(err.status, 403, "{}", err.detail);
    // no token: refused before the engine
    let err = e.r.d.approve_with(&op("op-2"), id.0, None, None, false).await.expect_err("no token");
    assert_eq!(err.status, 400, "{}", err.detail);
    // what op-2 is shown: the plan a first signature freezes, and the track
    let shown = e.look("op-2", id).expect("token");
    assert_eq!(shown["frozen"], false, "{shown}");
    assert!(shown["plan"]["effect_point"].is_array(), "{shown}");
    assert_eq!(shown["track"]["identity"], "small_fast_boat", "{shown}");
    // another signer's token
    let theirs = e.look("op-3", id).expect("token");
    let err = e
        .r
        .d
        .approve_with(&op("op-2"), id.0, None, theirs["id"].as_u64(), false)
        .await
        .expect_err("not mine");
    assert_eq!(err.status, 409, "{}", err.detail);
    assert_eq!(e.r.state(id), State::PendingApproval);
    // the one just shown: signed, 1 of 2; the plan is frozen and the console shows it
    e.sign("op-2", id).await.expect("signs");
    let inbox = e.r.d.inbox_json();
    let p = &inbox["pending"][0];
    assert_eq!(p["engage"]["signatures"][0]["by"], "op-2", "{p}");
    assert_eq!(p["engage"]["plan"]["by"], "op-2", "{p}");
    let next = e.look("op-3", id).expect("token");
    assert_eq!(next["frozen"], true, "{next}");
    assert_eq!(next["plan"]["effect_point"], p["engage"]["plan"]["effect_point"], "{next}");
    // 20 s later that token has expired: refused by the engine, and logged
    e.r.clock.advance_ms(20_000);
    let err = e
        .r
        .d
        .approve_with(&op("op-3"), id.0, None, next["id"].as_u64(), false)
        .await
        .expect_err("expired");
    assert_eq!(err.status, 409, "{}", err.detail);
    assert!(err.detail.contains("expired"), "{}", err.detail);
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
    assert_eq!(rep.concurrences, 1);
}

#[tokio::test(start_paused = true)]
async fn a_fleet_vehicle_never_heard_from_is_not_clear_of_the_keep_out() {
    let e = engage_rig();
    // the manifest names vehicle 2 (a USV), which never reports
    let dir = std::env::temp_dir().join(format!("murmur-engage-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let manifest = dir.join("fleet.json");
    std::fs::write(
        &manifest,
        json!({ "vehicles": [
            { "uid": 1, "sysid": 1, "port": 1, "platform": "fpv", "callsign": "FPV 1" },
            { "uid": 2, "sysid": 2, "port": 2, "platform": "seahunter", "callsign": "USV 2" },
        ]})
        .to_string(),
    )
    .expect("manifest");
    let world = World::new(Config {
        base_port: 0,
        http_port: 0,
        default_ports: 0,
        manifest: manifest.clone(),
        platforms: root().join("sim/platforms.toml"),
        root: root(),
    });
    world.load_manifest();
    let snap = super::snapshot::build(
        &world,
        &Catalog::load(&root().join("sim/platforms.toml")).expect("catalog"),
        &AreaSet::empty("test", e.r.clock.now()),
        e.r.clock.now(),
        None,
    );
    let v2 = snap.vehicles.get(&murmur_decide::Uid::new("2")).expect("in the world model");
    assert!(v2.position.is_none() && v2.platform.is_some(), "{v2:?}");
    // and the friendly keep-out fails closed on it
    let r = murmur_decide::engage::keep_out_friendly(
        murmur_decide::LonLat::new(TARGET.1, TARGET.0),
        e.r.clock.now(),
        300.0,
        Some(&murmur_decide::Uid::new("1")),
        &snap,
        &murmur_decide::RulesConfig::default(),
        None,
    );
    assert_eq!(r.outcome, murmur_decide::Outcome::Fail, "{r:?}");
    assert!(r.detail.contains("no position"), "{}", r.detail);
    let _ = std::fs::remove_dir_all(&dir);
}


#[tokio::test(start_paused = true)]
async fn a_platform_leaving_its_intercept_stops_the_engagement() {
    let e = engage_rig();
    // en route to the effect point, 500 m out: no release can be ready before it leaves (on
    // the point, a picture whose sensed track reads as moving, 0.3 m/s or more, may release
    // at once: the rig's noise draws follow the wall clock)
    let id = e.approved_at("return", (TARGET.0, TARGET.1 + 0.0057)).await;
    let cell = e.r.world.vehicles().into_iter().find(|c| c.uid == 1).expect("platform");
    let mode = |text: &str| {
        cell.write(|v| {
            v.events.push_back(crate::vehicle::VehicleEvent {
                kind: "mode",
                text: text.into(),
            });
        });
    };
    // entering the mission mode is the intercept starting, not leaving it
    mode("RTL -> AUTO");
    e.r.d.tick();
    assert_eq!(e.r.state(id), State::Dispatched, "{}", e.r.reason(id));
    // its GCS failsafe switched it to RTL, and the failsafe's message was lost on the link
    // that failed: only the mode change arrives
    mode("AUTO -> RTL");
    e.r.d.tick();
    assert_eq!(e.r.state(id), State::Aborted, "{}", e.r.reason(id));
    assert!(e.r.reason(id).contains("left its intercept (mode AUTO -> RTL)"), "{}", e.r.reason(id));
    assert!(e.tracks.take_effects().is_empty(), "no effect");
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
}

#[tokio::test(start_paused = true)]
async fn a_failsafe_that_keeps_the_mission_does_not_stop_the_engagement() {
    let e = engage_rig();
    let id = e.approved("return").await;
    let cell = e.r.world.vehicles().into_iter().find(|c| c.uid == 1).expect("platform");
    for text in [
        "[severity 4] Failsafe - Continuing Auto Mode",
        "[severity 4] RC Short Failsafe On",
        "[severity 4] Throttle failsafe off",
        "system status MAV_STATE_CRITICAL",
    ] {
        cell.write(|v| {
            v.events.push_back(crate::vehicle::VehicleEvent {
                kind: "failsafe",
                text: text.into(),
            });
        });
        e.r.d.tick();
        assert_ne!(e.r.state(id), State::Aborted, "{text}: {}", e.r.reason(id));
    }
}

#[tokio::test(start_paused = true)]
async fn the_platforms_own_failsafe_stops_the_engagement() {
    let e = engage_rig();
    let id = e.approved("return").await;
    // the platform's GCS failsafe takes over (it switched to RTL on its own)
    let cell = e.r.world.vehicles().into_iter().find(|c| c.uid == 1).expect("platform");
    cell.write(|v| {
        v.events.push_back(crate::vehicle::VehicleEvent {
            kind: "failsafe",
            text: "[severity 4] GCS Failsafe On: switched to RTL".into(),
        });
    });
    e.r.d.tick();
    assert_eq!(e.r.state(id), State::Aborted, "{}", e.r.reason(id));
    assert!(e.r.reason(id).contains("failsafe took over"), "{}", e.r.reason(id));
    assert!(e.tracks.take_effects().is_empty(), "no effect");
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
}

/// The `engage.preload` events of the log, as (phase, readback, confirmed).
fn preloads(log: &MemorySink) -> Vec<(String, Value, bool)> {
    log.lines()
        .iter()
        .filter(|l| l.contains("\"type\":\"engage.preload\""))
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("json");
            (
                v["phase"].as_str().unwrap_or_default().to_string(),
                v["readback"].clone(),
                v["confirmed"].as_bool().unwrap_or(false),
            )
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn the_fallback_is_preloaded_as_the_lost_link_action_then_restored() {
    let e = engage_rig();
    // abort on a multirotor: ArduCopter FS_GCS_ENABLE 7 (Brake, else Land), FS_OPTIONS bit 1
    // clear, FS_GCS_TIMEOUT at most the approval's 5 s; read back before the intercept
    let id = e.approved("abort").await;
    let order = e
        .r
        .log
        .lines()
        .into_iter()
        .find(|l| l.contains("\"type\":\"intent.dispatched\""))
        .expect("dispatched");
    assert!(order.contains("\"preload\":{\"firmware\":\"ArduCopter\""), "{order}");
    // its own values logged first (the write-ahead, with the vehicle's identity), then set
    let all = preloads(&e.r.log);
    assert_eq!(all.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(), ["intent", "set"], "{all:?}");
    let intent = e.r.log.lines().into_iter().find(|l| l.contains("\"phase\":\"intent\"")).expect("intent");
    assert!(intent.contains("\"before\":{\"FS_GCS_ENABLE\":1.0"), "{intent}");
    assert!(intent.contains("\"vehicle\":{\"platform\":\"fpv\",\"callsign\":\"fpv 1\",\"sysid\":"), "{intent}");
    let set = &all[1..];
    assert!(set[0].2, "confirmed: {set:?}");
    assert_eq!(set[0].1["FS_GCS_ENABLE"], 7.0, "{set:?}");
    assert_eq!(set[0].1["FS_OPTIONS"], 16.0, "{set:?}");
    assert_eq!(set[0].1["FS_GCS_TIMEOUT"], 5.0, "{set:?}");
    // an operator aborts: the engagement is over, the platform's own values go back
    e.r.d.abort(&op("op-1"), id.0, None).expect("aborted");
    for _ in 0..6 {
        e.tick(true).await;
    }
    let all = preloads(&e.r.log);
    assert_eq!(all.len(), 3, "{all:?}");
    assert_eq!(all[2].0, "restore");
    assert!(all[2].2, "confirmed: {all:?}");
    assert_eq!(all[2].1["FS_GCS_ENABLE"], 1.0, "its own action back: {all:?}");
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
}

#[tokio::test(start_paused = true)]
async fn an_engagement_superseded_before_its_preload_is_written_leaves_the_platform_alone() {
    let e = engage_rig();
    e.r.vehicle(1, Firmware::Copter, "fpv", TARGET);
    sleep(Duration::from_millis(500)).await;
    e.picture().await;
    let id = e.r.d.submit(&op("op-1"), e.nomination("abort")).await.expect("nominated").id;
    e.sign("op-2", id).await.expect("signs");
    let o = e.sign("op-3", id).await.expect("signs");
    assert_eq!(o.state, State::Dispatched);
    // while the platform's own values are being read, an operator's own hold takes the
    // platform: the engagement stops (superseded), its intercept is never sent
    let hold = e.r.d.submit(&op("op-1"), simple("hold", 1)).await.expect("hold");
    assert_eq!(e.r.state(id), State::Aborted, "{}", e.r.reason(id));
    for _ in 0..6 {
        sleep(Duration::from_millis(500)).await;
        e.r.d.tick();
    }
    assert!(!e.r.d.lock().taskings.contains_key(&id), "no intercept after the stop");
    // the read finishes after the stop: nothing is written (no write-ahead, no set), so
    // nothing is owed back either
    let all = preloads(&e.r.log);
    assert!(all.is_empty(), "{all:?}");
    assert!(e.r.d.lock().params.get(&1).is_none_or(|l| l.owed_values().is_none()));
    assert_ne!(hold.id, id);
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
}

#[tokio::test(start_paused = true)]
async fn an_agent_cannot_accept_a_lost_vehicle() {
    let e = engage_rig();
    e.r.vehicle(1, Firmware::Copter, "fpv", TARGET);
    sleep(Duration::from_millis(500)).await;
    e.picture().await;
    let mut req = e.nomination("return");
    req.rationale = Some("the declared fast boat west of the bridge".into());
    req.engage.accept_unknown = vec![engage::AcceptUnknown { uid: 2, since: None }];
    let err = e
        .r
        .d
        .agent_propose(&AgentId::new("agent-planner"), req)
        .await
        .expect_err("refused");
    assert_eq!(err.status, 403, "{}", err.detail);
    assert!(err.detail.contains("only an operator"), "{}", err.detail);
    assert_eq!(e.r.events("engage.accepted"), 0);
    assert_eq!(e.r.events("intent.created"), 0, "refused before any intent exists");
}

/// The `platform.restore` events of the log, as (carried, values, confirmed).
fn platform_restores(log: &MemorySink) -> Vec<(bool, Value, bool)> {
    log.lines()
        .iter()
        .filter(|l| l.contains("\"type\":\"platform.restore\""))
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("json");
            (
                v["carried"].as_bool().unwrap_or(false),
                v["values"].clone(),
                v["confirmed"].as_bool().unwrap_or(false),
            )
        })
        .collect()
}

/// Nominate (op-1) and sign (op-2, op-3) on the platform already in the fleet: dispatched.
async fn dispatch_again(e: &Erig, fallback: &str) -> IntentId {
    let id = e.r.d.submit(&op("op-1"), e.nomination(fallback)).await.expect("nominated").id;
    e.sign("op-2", id).await.expect("signs");
    let o = e.sign("op-3", id).await.expect("signs");
    assert_eq!(o.state, State::Dispatched, "{}", o.intent);
    id
}

#[tokio::test(start_paused = true)]
async fn a_newer_engagement_takes_over_the_values_owed_and_a_restore_never_undoes_it() {
    let e = engage_rig();
    // A preloads the copter (FS_GCS_ENABLE 7) and is aborted; before its restore can run, B
    // is dispatched on the same platform
    let a = e.approved("abort").await;
    e.r.d.abort(&op("op-1"), a.0, None).expect("aborted");
    let b = dispatch_again(&e, "return").await;
    e.preloaded(b).await;
    // B's set did not read the preloaded 7 as the platform's own: it took over A's values
    let lines = e.r.log.lines();
    let sets: Vec<Value> = lines
        .iter()
        .filter(|l| l.contains("\"type\":\"engage.preload\"") && l.contains("\"phase\":\"set\""))
        .map(|l| serde_json::from_str(l).expect("json"))
        .collect();
    assert_eq!(sets.len(), 2, "{sets:?}");
    assert_eq!(sets[0]["readback"]["FS_GCS_ENABLE"], 7.0, "A preloaded: {sets:?}");
    assert_eq!(sets[1]["before"]["FS_GCS_ENABLE"], 1.0, "its own, from A's set: {sets:?}");
    // and no restore ran while B held the platform
    assert!(preloads(&e.r.log).iter().all(|p| p.0 != "restore"), "{:?}", preloads(&e.r.log));
    // B ends: its restore puts back the values from before A
    e.r.d.abort(&op("op-1"), b.0, None).expect("aborted");
    for _ in 0..8 {
        e.tick(true).await;
    }
    let all = preloads(&e.r.log);
    let restore = all.iter().find(|p| p.0 == "restore").expect("restored");
    assert!(restore.2, "{all:?}");
    assert_eq!(restore.1["FS_GCS_ENABLE"], 1.0, "{all:?}");
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
    assert!(rep.unrestored.is_empty(), "{:?}", rep.unrestored);
}

#[tokio::test(start_paused = true)]
async fn the_preload_is_read_again_before_the_release() {
    let e = engage_rig();
    let id = e.approved("return").await;
    // its window is open at once here: 6 s after the set the preload is due to be read again
    e.r.clock.advance_ms(6_000);
    for _ in 0..10 {
        sleep(Duration::from_millis(500)).await;
        let mut guard = e.r.d.lock();
        e.r.d.step_params(&mut guard);
        drop(guard);
        if preloads(&e.r.log).iter().any(|p| p.0 == "verify") {
            break;
        }
    }
    let all = preloads(&e.r.log);
    assert_eq!(all.iter().map(|p| (p.0.as_str(), p.2)).collect::<Vec<_>>(), [("intent", true), ("set", true), ("verify", true)], "{all:?}");
    assert_eq!(all[2].1["FS_GCS_ENABLE"], 1.0, "{all:?}");
    assert_eq!(e.r.state(id), State::Dispatched, "{}", e.r.reason(id));
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
}

#[tokio::test(start_paused = true)]
async fn values_still_owed_when_the_bridge_stopped_are_carried_over_and_put_back() {
    // the first run: preloaded, then aborted, and the bridge stops before the restore
    let e = engage_rig();
    let a = e.approved("abort").await;
    e.r.d.abort(&op("op-1"), a.0, None).expect("aborted");
    let first = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(first.ok(), "{:?}", first.mismatches);
    assert_eq!(first.unrestored.len(), 1, "replay reports it: {:?}", first.unrestored);
    let dir = std::env::temp_dir().join(format!("murmur-carry-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let log = dir.join("run-1.jsonl");
    std::fs::write(&log, e.r.log.text()).expect("log");
    let pointer = dir.join(".last-run-0");
    std::fs::write(&pointer, log.display().to_string()).expect("pointer");
    // the next run beside the same vehicle: carried over from the first run's log at start,
    // then put back once the platform answers
    let e2 = engage_rig_on(Some((e.r.world.clone(), e.tracks.clone())));
    e2.r.d.carry_restores(&pointer);
    assert_eq!(std::fs::read_to_string(&pointer).expect("pointer"), "memory", "now names this run's log");
    for _ in 0..4 {
        e2.r.d.tick();
        sleep(Duration::from_secs(1)).await;
    }
    let all = platform_restores(&e2.r.log);
    assert_eq!(all.len(), 2, "{all:?}");
    assert!(all[0].0 && !all[0].2, "carried: {all:?}");
    assert_eq!(all[0].1["FS_GCS_ENABLE"], 1.0, "{all:?}");
    assert!(!all[1].0 && all[1].2, "put back and confirmed: {all:?}");
    // replayed against what the first run's log still owed (murmur-aca): exactly what it
    // owed, on the vehicle and for the engagement it names, and that log replays
    let first_text = std::fs::read_to_string(&log).expect("first log");
    let opts = replay::ReplayOptions {
        restores_from: Some(("run-1.jsonl", &first_text)),
        ..replay::ReplayOptions::default()
    };
    let rep = replay::replay_str_with(&e2.r.log.text(), &opts).expect("parses");
    assert!(rep.ok() && rep.unrestored.is_empty(), "{:?} {:?}", rep.mismatches, rep.unrestored);
    let checked: Vec<_> = rep.carried_restores.iter().map(|c| (c.log.as_str(), c.restores, c.checked)).collect();
    assert_eq!(checked, [("run-1.jsonl", 1, true)]);
    // without that log the carried values are not verified: a mismatch, unless trusted
    let rep = replay::replay_str(&e2.r.log.text()).expect("parses");
    assert!(rep.mismatches.iter().all(|m| m.what.contains("not verified")) && !rep.ok(), "{:?}", rep.mismatches);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(start_paused = true)]
async fn a_restore_that_does_not_confirm_is_retried_logged_and_alerted() {
    let e = engage_rig();
    e.r.vehicle(1, Firmware::Copter, "fpv", TARGET);
    sleep(Duration::from_millis(500)).await;
    // a previous run owed a value the vehicle never answers for
    let clock = ManualClock::new(e.r.clock.now());
    let prev = MemorySink::new();
    let mut eng = murmur_decide::Engine::new(
        Policy::load(&root().join("sim/policy.toml")).expect("policy"),
        murmur_decide::Roster::new(vec![OperatorId::new("op-1")], vec![]).expect("roster"),
        Box::new(clock),
        Box::new(prev.clone()),
    )
    .expect("engine");
    eng.record_platform_restore(murmur_decide::PlatformRestoreReport {
        uid: duid(1),
        values: [("FS_GCS_ENABLE".to_string(), 1.0), ("FS_NO_SUCH".to_string(), 3.0)].into_iter().collect(),
        carried: true,
        from: None,
        readback: BTreeMap::new(),
        error: None,
        vehicle: None,
    })
    .expect("logged");
    let dir = std::env::temp_dir().join(format!("murmur-alert-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let log = dir.join("run-0.jsonl");
    std::fs::write(&log, prev.text()).expect("log");
    let pointer = dir.join(".last-run-0");
    std::fs::write(&pointer, log.display().to_string()).expect("pointer");
    e.r.d.carry_restores(&pointer);
    for _ in 0..90 {
        e.r.d.tick();
        sleep(Duration::from_secs(1)).await;
    }
    // every attempt logged, none confirmed
    let all = platform_restores(&e.r.log);
    let attempts: Vec<_> = all.iter().filter(|a| !a.0).collect();
    assert!(attempts.len() >= 2, "{all:?}");
    assert!(attempts.iter().all(|a| !a.2), "{all:?}");
    // and the inbox says so
    let inbox = e.r.d.inbox_json();
    let alert = inbox["alerts"]
        .as_array()
        .expect("alerts")
        .iter()
        .find(|a| a["kind"] == "restore_pending")
        .cloned()
        .expect("a restore alert");
    assert_eq!(alert["id"], "restore:1");
    assert!(alert["detail"].as_str().unwrap_or_default().contains("FS_NO_SUCH 3"), "{alert}");
    // its carried values trusted (the earlier log here is a bare engine's, which names no
    // log of its own)
    let trusted = replay::ReplayOptions {
        trust_resumed: true,
        ..replay::ReplayOptions::default()
    };
    let rep = replay::replay_str_with(&e.r.log.text(), &trusted).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
    assert_eq!(rep.unrestored.len(), 1, "{:?}", rep.unrestored);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(start_paused = true)]
async fn only_engagement_authority_accepts_a_lost_vehicle() {
    let e = engage_rig();
    e.r.vehicle(1, Firmware::Copter, "fpv", TARGET);
    sleep(Duration::from_millis(500)).await;
    e.picture().await;
    // op-1 has no engagement authority: its nomination with an acceptance is refused whole
    let mut req = e.nomination("return");
    req.engage.accept_unknown = vec![engage::AcceptUnknown { uid: 2, since: None }];
    let err = e.r.d.submit(&op("op-1"), req).await.expect_err("refused");
    assert_eq!(err.status, 403, "{}", err.detail);
    assert!(err.detail.contains("engagement authority"), "{}", err.detail);
    // and so is its acceptance on a waiting nomination
    let id = e.r.d.submit(&op("op-1"), e.nomination("return")).await.expect("nominated").id;
    let err = e
        .r
        .d
        .accept_unknown(&op("op-1"), id.0, &engage::AcceptUnknown { uid: 2, since: None }, None)
        .expect_err("refused");
    assert_eq!(err.status, 403, "{}", err.detail);
    assert_eq!(e.r.events("engage.accepted"), 0);
}

#[tokio::test(start_paused = true)]
async fn every_signer_sees_an_acceptance_and_any_operator_withdraws_it() {
    // the manifest names vehicle 2 (a USV) that never reports: it blocks the keep-out until
    // an operator with engagement authority accepts it as lost
    let dir = std::env::temp_dir().join(format!("murmur-accept-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let manifest = dir.join("fleet.json");
    std::fs::write(
        &manifest,
        json!({ "vehicles": [
            { "uid": 1, "sysid": 1, "port": 1, "platform": "fpv", "callsign": "FPV 1" },
            { "uid": 2, "sysid": 2, "port": 2, "platform": "seahunter", "callsign": "USV 2" },
        ]})
        .to_string(),
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
    let tracks = Arc::new(Tracks::load(&root().join("sim/platforms.toml")).expect("sensors"));
    let e = engage_rig_on(Some((world, tracks)));
    e.r.vehicle(1, Firmware::Copter, "fpv", TARGET);
    sleep(Duration::from_millis(500)).await;
    e.picture().await;
    // without it: rejected, vehicle 2 has no position
    let out = e.r.d.submit(&op("op-1"), e.nomination("return")).await.expect("nominated");
    assert_eq!(out.state, State::Rejected, "{}", out.intent);
    // op-2 (engagement authority) nominates accepting it, lost since it never reported
    let mut req = e.nomination("return");
    req.engage.accept_unknown = vec![engage::AcceptUnknown { uid: 2, since: None }];
    let out = e.r.d.submit(&op("op-2"), req).await.expect("nominated");
    assert_eq!(out.state, State::PendingApproval, "{}", out.intent);
    let id = out.id;
    // the console's view shows it to every signer, and op-3's picture token carries it
    let msg: Value = serde_json::from_str(&e.r.d.message()).expect("json");
    let mine = msg["inbox"]["pending"].as_array().expect("pending").iter().find(|i| i["id"] == id.0).cloned().expect("intent");
    assert_eq!(mine["engage"]["accepted"], json!([{ "uid": 2, "engine_uid": "2", "since": null, "by": "op-2" }]), "{mine}");
    let token = e.look("op-3", id).expect("picture");
    assert_eq!(token["unknown_accepted"]["2"]["by"], "op-2", "{token}");
    // any operator withdraws it (op-1 has no engagement authority); op-3's token from before
    // is refused, and on a fresh look the check counts vehicle 2 again: the approval is void
    e.r.d.revoke_unknown(&op("op-1"), id.0, 2).expect("revoked");
    let err = e.r.d.approve_with(&op("op-3"), id.0, None, token["id"].as_u64(), false).await.expect_err("old token");
    assert!(err.detail.contains("changed since you looked"), "{}", err.detail);
    let err = e.sign("op-3", id).await.expect_err("void");
    assert!(err.detail.contains("keep_out_friendly 2: vehicle 2 has no position"), "{}", err.detail);
    assert_eq!(e.r.state(id), State::Voided);
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
    assert_eq!((e.r.events("engage.accepted"), e.r.events("engage.acceptance_revoked")), (1, 1));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A previous run's log that owes vehicle 1's own lost-link values (carried into it), as
/// preloaded on `vehicle`.
fn owing_log(vehicle: murmur_decide::VehicleIdentity) -> String {
    let prev = MemorySink::new();
    let mut eng = murmur_decide::Engine::new(
        Policy::load(&root().join("sim/policy.toml")).expect("policy"),
        murmur_decide::Roster::new(vec![OperatorId::new("op-1")], vec![]).expect("roster"),
        Box::new(ManualClock::new(snapshot::ms(clock::wall()))),
        Box::new(prev.clone()),
    )
    .expect("engine");
    eng.record_platform_restore(murmur_decide::PlatformRestoreReport {
        uid: duid(1),
        values: [("FS_GCS_ENABLE".to_string(), 1.0)].into_iter().collect(),
        carried: true,
        from: None,
        readback: BTreeMap::new(),
        error: None,
        vehicle: Some(vehicle),
    })
    .expect("logged");
    prev.text()
}

#[tokio::test(start_paused = true)]
async fn a_carried_restore_is_not_written_to_another_vehicle_with_the_uid() {
    let e = engage_rig();
    e.r.vehicle(1, Firmware::Copter, "fpv", TARGET);
    sleep(Duration::from_millis(500)).await;
    // the last run preloaded a magura as vehicle 1; this run's vehicle 1 is an FPV quadcopter
    let dir = std::env::temp_dir().join(format!("murmur-carry-other-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let log = dir.join("run-0.jsonl");
    std::fs::write(
        &log,
        owing_log(murmur_decide::VehicleIdentity {
            platform: Some("magura".into()),
            callsign: Some("VIPER 1".into()),
            sysid: Some(1),
            home: None,
        }),
    )
    .expect("log");
    let pointer = dir.join(".last-run-0");
    std::fs::write(&pointer, log.display().to_string()).expect("pointer");
    e.r.d.carry_restores(&pointer);
    for _ in 0..45 {
        e.r.d.tick();
        sleep(Duration::from_secs(1)).await;
    }
    // carried, never written, and the inbox says why
    let all = platform_restores(&e.r.log);
    assert_eq!(all.len(), 1, "only the carried event: {all:?}");
    let inbox = e.r.d.inbox_json();
    let alert = inbox["alerts"].as_array().expect("alerts").iter().find(|a| a["id"] == "restore:1").cloned().expect("alert");
    let detail = alert["detail"].as_str().unwrap_or_default();
    assert!(detail.contains("may not be the one preloaded (its platform is \"fpv\", not \"magura\")"), "{detail}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(start_paused = true)]
async fn a_start_that_cannot_read_the_last_runs_log_says_so() {
    let e = engage_rig();
    let dir = std::env::temp_dir().join(format!("murmur-carry-missing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let pointer = dir.join(".last-run-0");
    std::fs::write(&pointer, dir.join("gone.jsonl").display().to_string()).expect("pointer");
    e.r.d.carry_restores(&pointer);
    let inbox = e.r.d.inbox_json();
    let alert = inbox["alerts"].as_array().expect("alerts").iter().find(|a| a["id"] == "restore:carry").cloned().expect("alert");
    assert!(alert["detail"].as_str().unwrap_or_default().contains("gone.jsonl can't be read"), "{alert}");
    // a first start (no pointer yet) is not a problem
    let e2 = engage_rig();
    e2.r.d.carry_restores(&dir.join(".last-run-1"));
    assert!(e2.r.d.inbox_json()["alerts"].as_array().expect("alerts").is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(start_paused = true)]
async fn a_reused_run_id_carries_from_the_same_file() {
    // the run id was reused: the log file is this run's too, the last run's events first
    let dir = std::env::temp_dir().join(format!("murmur-carry-reused-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let file = dir.join("run.jsonl");
    let earlier = owing_log(murmur_decide::VehicleIdentity::default());
    let this_run = earlier.lines().next().expect("run.started").to_string();
    std::fs::write(&file, format!("{earlier}{this_run}\n")).expect("log");
    let pointer = dir.join(".last-run-0");
    std::fs::write(&pointer, file.display().to_string()).expect("pointer");
    let e = engage_rig_with(None, file.clone());
    e.r.d.carry_restores(&pointer);
    let all = platform_restores(&e.r.log);
    assert_eq!(all.len(), 1, "carried from the earlier run's part of the file: {all:?}");
    assert!(all[0].0, "{all:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(start_paused = true)]
async fn a_mode_change_received_before_the_release_picture_is_handled_before_the_release() {
    // the platform's AUTO -> RTL reaches the bridge after this tick drained the vehicle
    // events, before the engagement step takes the picture a release would rest on: it is
    // handled first, so no effect is released on a platform that left its intercept
    let e = engage_rig();
    let id = e.approved("return").await;
    // ready to release: on the point, past the plan's time, the track held
    e.r.clock.advance_ms(2_000);
    e.sense(true);
    let cell = e.r.world.vehicles().into_iter().find(|c| c.uid == 1).expect("platform");
    cell.write(|v| {
        v.events.push_back(crate::vehicle::VehicleEvent {
            kind: "mode",
            text: "AUTO -> RTL".into(),
        });
    });
    // the engagement step alone, as on a tick whose event drain came before the event
    {
        let mut core = e.r.d.lock();
        e.r.d.step_engagements(&mut core);
    }
    assert_eq!(e.r.state(id), State::Aborted, "{}", e.r.reason(id));
    assert!(e.r.reason(id).contains("left its intercept (mode AUTO -> RTL)"), "{}", e.r.reason(id));
    assert!(e.tracks.take_effects().is_empty(), "no effect");
    // without the mode change, the same picture releases (the test is not vacuous)
    let e2 = engage_rig();
    let id2 = e2.approved("return").await;
    e2.r.clock.advance_ms(2_000);
    e2.sense(true);
    {
        let mut core = e2.r.d.lock();
        e2.r.d.step_engagements(&mut core);
    }
    assert_eq!(e2.r.state(id2), State::Effected, "{}", e2.r.reason(id2));
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
}

// ---- rules 7 ----------------------------------------------------------------------------

impl Erig {
    /// [`Erig::approved`] on a watched fake (its link can be cut): nominated, signed,
    /// dispatched, its lost-link preload confirmed and its intercept sent.
    async fn approved_watched(&self, fallback: &str) -> (IntentId, Arc<VehicleCell>, crate::tasks::tests::Watch) {
        self.approved_watched_at(fallback, TARGET).await
    }

    /// [`Erig::approved_watched`] with the platform starting at `at` (lat, lon).
    async fn approved_watched_at(
        &self,
        fallback: &str,
        at: (f64, f64),
    ) -> (IntentId, Arc<VehicleCell>, crate::tasks::tests::Watch) {
        let (cell, w) = self.r.watched(1, Firmware::Copter, "fpv", at, quick());
        sleep(Duration::from_millis(500)).await;
        self.picture().await;
        let id = self.r.d.submit(&op("op-1"), self.nomination(fallback)).await.expect("nominated").id;
        self.sign("op-2", id).await.expect("signs");
        let o = self.sign("op-3", id).await.expect("signs");
        assert_eq!(o.state, State::Dispatched, "{}", o.intent);
        self.preloaded(id).await;
        (id, cell, w)
    }

    /// One second of the loop with the engine clock on the wall clock the vehicles' facts are
    /// stamped with (long runs: advancing it a second per tick would age every fact).
    async fn tick_on_wall(&self) {
        self.r.clock.set(snapshot::ms(clock::wall()));
        self.sense(true);
        self.r.d.tick();
        sleep(Duration::from_secs(1)).await;
    }
}

/// The `engage.stopped` event of intent `id`.
fn stopped(log: &MemorySink, id: IntentId) -> Value {
    let line = log
        .lines()
        .into_iter()
        .find(|l| l.contains("\"type\":\"engage.stopped\"") && l.contains(&format!("\"intent\":{}", id.0)))
        .unwrap_or_else(|| panic!("{id} was not stopped"));
    serde_json::from_str(&line).expect("json")
}

#[tokio::test(start_paused = true)]
async fn an_order_queued_for_an_engaging_platform_supersedes_it_without_its_fallback() {
    // murmur-636 (rules 7): the platform's link is cut before its intercept started; an
    // operator orders it to hold, and the hold can only be queued. It stands like a
    // dispatched one: the engagement is stopped at once, superseded by the hold, and no
    // fallback is commanded. (Before, the queue abandoned the intercept, the engagement read
    // that as its intercept failing, stopped as a system failure and commanded its fallback,
    // which flew when the link came back while the operator's hold still waited.)
    let e = engage_rig();
    let (id, cell, w) = e.approved_watched("return").await;
    super::queue::go_silent(&cell, &w, 5.0);
    let hold = e.r.d.submit(&op("op-1"), simple("hold", 1)).await.expect("hold");
    assert_eq!(hold.state, State::Queued, "{}", hold.intent);
    for _ in 0..3 {
        e.tick_on_wall().await;
    }
    assert_eq!(e.r.state(id), State::Aborted, "{}", e.r.reason(id));
    let stop = stopped(&e.r.log, id);
    assert_eq!(stop["cause"]["kind"], "superseded", "{stop}");
    assert_eq!(stop["cause"]["by"], hold.id.0, "{stop}");
    assert!(stop["fallback"].is_null(), "{stop}");
    let fallbacks = e
        .r
        .log
        .lines()
        .into_iter()
        .filter(|l| l.contains("\"type\":\"op.state\"") && l.contains("\"state\":\"fallback"))
        .count();
    assert_eq!(fallbacks, 0, "no fallback commanded");
    // the link returns: the hold goes out (a safety command waits for no restore), and the
    // platform's own lost-link values go back
    w.set_cut(false);
    for _ in 0..10 {
        e.tick_on_wall().await;
    }
    let went = e
        .r
        .d
        .lock()
        .engine
        .intent(hold.id)
        .is_some_and(|i| i.history().iter().any(|h| h.state == State::Dispatched));
    assert!(went, "the hold went out: {}", e.r.reason(hold.id));
    let ops = e.r.world.tasks.current_ops();
    let op1 = ops.iter().find(|o| o.uid == 1).expect("an op on the platform");
    assert_eq!((op1.intent_id, op1.task), (hold.id.0, crate::tasks::TaskKind::Hold), "the operator's hold, not a fallback");
    assert!(preloads(&e.r.log).iter().any(|p| p.0 == "restore" && p.2), "{:?}", preloads(&e.r.log));
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
}

#[tokio::test(start_paused = true)]
async fn a_mission_for_an_engaging_platform_stops_it_and_goes_once_its_own_values_are_back() {
    // rules 7: an operator's own orbit for the platform of an engagement under way (its link
    // fine): it waits in the queue for the platform's own lost-link values, so it supersedes
    // the engagement as it is queued (no fallback), the restore runs, and the orbit goes
    let e = engage_rig_policy(None, PathBuf::from("memory"), |p| p.rules.task_duration_s = 60);
    let (id, _cell, _w) = e.approved_watched("return").await;
    let mut req = orbit(1);
    req.params.center = Some([TARGET.1 + 0.004, TARGET.0]);
    let o = e.r.d.submit(&op("op-1"), req).await.expect("orbit");
    assert_eq!(o.state, State::Queued, "{}", o.intent);
    assert!(o.intent["history"].to_string().contains("its own lost-link failsafe"), "{}", o.intent);
    assert_eq!(e.r.state(id), State::Aborted, "{}", e.r.reason(id));
    assert_eq!(stopped(&e.r.log, id)["cause"]["kind"], "superseded");
    for _ in 0..20 {
        e.tick_on_wall().await;
        if e.r.state(o.id) == State::Dispatched {
            break;
        }
    }
    assert_eq!(e.r.state(o.id), State::Dispatched, "{}", e.r.reason(o.id));
    let lines = e.r.log.lines();
    let restored = lines
        .iter()
        .position(|l| l.contains("\"phase\":\"restore\"") && l.contains("\"confirmed\":true"))
        .expect("its own values back");
    let sent = lines
        .iter()
        .position(|l| l.contains("\"type\":\"intent.dispatched\"") && l.contains(&format!("\"intent\":{}", o.id.0)))
        .expect("the orbit dispatched");
    assert!(restored < sent, "its own lost-link values back before the orbit went out");
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
}

#[tokio::test(start_paused = true)]
async fn a_mission_refused_for_its_platforms_own_values_still_supersedes_the_engagement() {
    // re-check of c577d5d, item A (rules 7): under a policy that never queues an orbit, an
    // operator's orbit for the platform of an engagement under way (its link fine) can't be
    // sent over the engagement's lost-link action and can't wait: it is refused. The newest
    // order stands all the same, as a dispatched or queued one does: the engagement is stopped
    // in the refusal's step, superseded, no fallback, and the platform's own values go back;
    // the orbit given again then goes. (Before, the orbit was voided and the engagement went
    // on; its reason said the orbit waits for a restore that could not happen.)
    let e = engage_rig_policy(None, PathBuf::from("memory"), |p| {
        p.rules.task_duration_s = 60;
        let mut q = p.queue();
        q.kind.insert(murmur_decide::TaskKind::Orbit, murmur_decide::KindQueue { expire_s: Some(0) });
        p.queue = Some(q);
    });
    let (id, _cell, _w) = e.approved_watched("return").await;
    let mut req = orbit(1);
    req.params.center = Some([TARGET.1 + 0.004, TARGET.0]);
    let o = e.r.d.submit(&op("op-1"), req.clone()).await.expect("orbit");
    assert_eq!(o.state, State::Voided, "{}", o.intent);
    let why = e.r.reason(o.id);
    assert!(why.contains(&format!("this order stops engagement {id} (superseded, no fallback)")), "{why}");
    assert!(why.contains("a hold or rtl goes at once"), "{why}");
    assert_eq!(e.r.state(id), State::Aborted, "{}", e.r.reason(id));
    let stop = stopped(&e.r.log, id);
    assert_eq!(stop["cause"]["kind"], "superseded", "{stop}");
    assert_eq!(stop["cause"]["by"], o.id.0, "{stop}");
    assert!(stop["fallback"].is_null(), "{stop}");
    // the stop right after the refusal, nothing logged between
    let lines = e.r.log.lines();
    let void = lines
        .iter()
        .position(|l| l.contains("\"to\":\"voided\"") && l.contains(&format!("\"intent\":{}", o.id.0)))
        .expect("the refusal");
    assert!(lines[void + 1].contains("\"type\":\"engage.stopped\""), "{}", lines[void + 1]);
    // its own values go back; the orbit given again goes at once
    for _ in 0..20 {
        e.tick_on_wall().await;
        if preloads(&e.r.log).iter().any(|p| p.0 == "restore" && p.2) {
            break;
        }
    }
    assert!(preloads(&e.r.log).iter().any(|p| p.0 == "restore" && p.2), "{:?}", preloads(&e.r.log));
    let again = e.r.d.submit(&op("op-1"), req).await.expect("orbit");
    assert_eq!(again.state, State::Dispatched, "{}", again.intent);
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
}

#[tokio::test(start_paused = true)]
async fn after_an_outage_the_platforms_own_values_go_back_before_a_queued_mission() {
    // murmur-85l (rules 7): an operator aborts an engagement while its platform is out of
    // contact; its own lost-link values are owed back and can't be put back. During the
    // outage the operator orders the platform on an orbit (queued). When the link returns
    // the restore goes at once (not after the backoff its failed attempts built up), and the
    // orbit waits for it: the platform never flies the operator's mission with the ended
    // engagement's lost-link action. (Before, the restore waited out minutes of backoff and
    // the orbit went out on the first tick the link was back.)
    // a short orbit (the fpv-class platform's 9 min of endurance would fail the reserves on
    // the default 15 min of time on task)
    let e = engage_rig_policy(None, PathBuf::from("memory"), |p| p.rules.task_duration_s = 60);
    let (id, cell, w) = e.approved_watched("abort").await;
    super::queue::go_silent(&cell, &w, 5.0);
    e.r.d.abort(&op("op-1"), id.0, Some("hold fire".into())).expect("aborted");
    // a few minutes out of contact: the restore can't go
    for _ in 0..150 {
        e.tick_on_wall().await;
    }
    let mut req = orbit(1);
    req.params.center = Some([TARGET.1 + 0.004, TARGET.0]);
    let o = e.r.d.submit(&op("op-1"), req).await.expect("orbit");
    assert_eq!(o.state, State::Queued, "{}", o.intent);
    for _ in 0..50 {
        e.tick_on_wall().await;
    }
    assert!(preloads(&e.r.log).iter().all(|p| p.0 != "restore" || !p.2), "not back during the outage");
    // the link is back
    w.set_cut(false);
    let back = e.r.log.lines().len();
    let mut ticks = 0;
    for _ in 0..60 {
        e.tick_on_wall().await;
        ticks += 1;
        if e.r.state(o.id) == State::Dispatched {
            break;
        }
    }
    assert_eq!(e.r.state(o.id), State::Dispatched, "{}", e.r.reason(o.id));
    let lines = e.r.log.lines();
    let restored = lines
        .iter()
        .position(|l| l.contains("\"type\":\"engage.preload\"") && l.contains("\"phase\":\"restore\"") && l.contains("\"confirmed\":true"))
        .expect("its own values back");
    let sent = lines
        .iter()
        .position(|l| l.contains("\"type\":\"intent.dispatched\"") && l.contains(&format!("\"intent\":{}", o.id.0)))
        .expect("the orbit dispatched");
    assert!(restored >= back, "the restore went once the link was back");
    assert!(restored < sent, "its own lost-link values back before the orbit went out");
    assert!(ticks <= 10, "the restore went at once, not after the backoff: {ticks} s");
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
}

#[tokio::test(start_paused = true)]
async fn a_platform_left_on_a_superseded_intercept_says_so_in_its_restore_alert() {
    // review of 1a3cadb, finding 5: an orbit queued for the engaging platform (its link cut)
    // supersedes the engagement; the platform's own lost-link values can't go back, so the
    // orbit waits, and the platform keeps loitering at the effect point on the stopped
    // engagement's intercept. Nothing can be released; the restore alert says where it is
    // The platform starts 500 m off the target, so no release can be ready before the orbit
    // supersedes the engagement (re-check of c577d5d, item B: on the target, at some start
    // seconds the effect was released during its ticks on station; bd murmur-ek3)
    let e = engage_rig_policy(None, PathBuf::from("memory"), |p| p.rules.task_duration_s = 60);
    let (id, cell, w) = e.approved_watched_at("return", (TARGET.0, TARGET.1 + 0.0057)).await;
    // on its way to the effect point, the intercept under way
    for _ in 0..3 {
        e.tick_on_wall().await;
    }
    assert_eq!(e.r.state(id), State::Dispatched, "{}", e.r.reason(id));
    assert!(e.tracks.take_effects().is_empty(), "nothing released before the orbit");
    super::queue::go_silent(&cell, &w, 5.0);
    let mut req = orbit(1);
    req.params.center = Some([TARGET.1 + 0.004, TARGET.0]);
    let o = e.r.d.submit(&op("op-1"), req).await.expect("orbit");
    assert_eq!(o.state, State::Queued, "{}", o.intent);
    assert_eq!(e.r.state(id), State::Aborted, "{}", e.r.reason(id));
    for _ in 0..40 {
        e.tick_on_wall().await;
    }
    let alerts = e.r.d.inbox_json()["alerts"].clone();
    let alert = alerts
        .as_array()
        .and_then(|a| a.iter().find(|a| a["kind"] == "restore_pending"))
        .cloned()
        .unwrap_or_else(|| panic!("{alerts}"));
    let detail = alert["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains(&format!("still on stopped engagement {id}'s intercept path")),
        "{detail}"
    );
    assert!(detail.contains(&format!("{} (orbit)", o.id)), "the waiting orbit listed: {detail}");
    assert!(e.tracks.take_effects().is_empty(), "nothing released");
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
}
