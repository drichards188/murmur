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

struct Erig {
    r: Rig,
    tracks: Arc<Tracks>,
}

fn engage_rig() -> Erig {
    let root = root();
    let mut policy = Policy::load(&root.join("sim/policy.toml")).expect("policy");
    let mut e = policy.rules.engage_rules();
    e.dispatch_delay_s = 0.0;
    policy.rules.engage = Some(e);
    let tracks = Arc::new(Tracks::load(&root.join("sim/platforms.toml")).expect("sensors"));
    let world = World::new(Config {
        base_port: 0,
        http_port: 0,
        default_ports: 0,
        manifest: root.join("sim/run/no-such-manifest.json"),
        platforms: root.join("sim/platforms.toml"),
        root: root.clone(),
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
            audit_path: PathBuf::from("memory"),
            clock: Arc::new(clock.clone()),
            terrain,
            operators: vec![OperatorId::new("op-1"), OperatorId::new("op-2"), OperatorId::new("op-3")],
            engage_operators: vec![OperatorId::new("op-2"), OperatorId::new("op-3")],
            tracks: Some(tracks.clone()),
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

fn op(s: &str) -> OperatorId {
    OperatorId::new(s)
}

impl Erig {
    fn now_s(&self) -> f64 {
        self.r.clock.now().0 as f64 / 1000.0
    }

    /// One truth tick: the hostile boat holding at TARGET, seen by a radar and EO/IR
    /// observer 1.5 km east (not one of our fleet vehicles: an explicit observer).
    fn sense(&self, with_target: bool) {
        self.sense_at(self.now_s(), with_target);
    }

    fn sense_at(&self, now_s: f64, with_target: bool) {
        let observer = Observer {
            uid: 9,
            lat: TARGET.0,
            lon: TARGET.1 + 0.017,
            alt_msl: 0.0,
            mast_m: 4.0,
            submerged: false,
            sensors: vec![
                SensorSpec { kind: SensorType::Radar, range_m: 8_000.0, guess: true },
                SensorSpec { kind: SensorType::EoIr, range_m: 5_000.0, guess: true },
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
        self.tracks
            .declare("T-0001", Affiliation::Hostile, "op-1", None, self.now_s())
            .expect("track");
        self.r
            .d
            .record_action(&op("op-1"), "track.declare", "track T-0001 declared hostile")
            .expect("logged");
    }

    fn nomination(&self, fallback: &str) -> IntentRequest {
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
            },
            constraints: Constraints::default(),
            rationale: None,
        }
    }

    /// What the console asks for before a signature: the picture token (the plan and the
    /// track as the engine shows them to this operator).
    fn look(&self, who: &str, id: IntentId) -> Result<Value, DecisionError> {
        self.r.d.issue_picture(&op(who), id.0)
    }

    /// A signature as the console sends it: look, then sign quoting the token.
    async fn sign(&self, who: &str, id: IntentId) -> Result<Outcome, DecisionError> {
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
        assert!(o.tasking.is_some());
        id
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
    // the declaration is withdrawn: the picture the approvers saw no longer holds
    e.tracks
        .declare("T-0001", Affiliation::Neutral, "op-1", None, e.now_s())
        .expect("track");
    e.tick(true).await;
    assert_eq!(e.r.state(id), State::Aborted, "{}", e.r.reason(id));
    assert!(e.r.reason(id).contains("target_declared"), "{}", e.r.reason(id));
    let ops = e.r.world.tasks.current_ops();
    let op1 = ops.iter().find(|o| o.uid == 1).expect("an op on the platform");
    assert_eq!(op1.task, crate::tasks::TaskKind::Hold);
    assert!(e.tracks.take_effects().is_empty(), "no effect after a void");
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
    assert_eq!(rep.picture_voids, 1);
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
