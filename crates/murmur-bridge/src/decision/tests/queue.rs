//! The safe command queue through the bridge (rules 5, murmur-5p4): an order for a silent
//! vehicle is held, not sent into the dead link, and goes once the link is back (re-checked
//! first); it expires, comes back to the inbox when a human's picture changed, survives a
//! bridge restart, and a vehicle reboot mid-task ends its op without a resend. Every log
//! replays (a restarted run's against the run before it).

use std::sync::atomic::Ordering;

use super::*;
use crate::tasks::tests::{Quirks, Watch};
use murmur_decide::replay::{ReplayOptions, replay_str_with};

impl Rig {
    /// A fake whose link can be cut, whose received commands are counted, and which can be
    /// rebooted.
    fn watched(&self, uid: Uid, fw: Firmware, platform: &str, at: (f64, f64), q: Quirks) -> (Arc<VehicleCell>, Watch) {
        let (q, w) = q.watched();
        let cell = spawn_fake(uid, fw, at.0, at.1, q);
        let mut m = serde_json::Map::new();
        m.insert("platform".into(), json!(platform));
        m.insert("callsign".into(), json!(format!("{platform} {uid}")));
        cell.write(|v| v.identity = Some(Arc::new(Identity::from_manifest(&m))));
        self.world.insert_vehicle(cell.clone());
        (cell, w)
    }
}

/// Cut a vehicle's link, and make its last heartbeat `s` seconds old (the engine's clock is
/// manual, the facts' times are the wall clock's: this is the silence the engine sees).
fn go_silent(cell: &VehicleCell, w: &Watch, s: f64) {
    w.set_cut(true);
    cell.write(|v| v.seen.heartbeat = Some(clock::wall() - s));
}

/// Tick once a second until `id` is no longer queued.
async fn tick_until_dequeued(r: &Rig, id: IntentId, limit_s: u64) -> State {
    for _ in 0..limit_s {
        r.d.tick();
        let s = r.state(id);
        if s != State::Queued {
            return s;
        }
        sleep(Duration::from_secs(1)).await;
    }
    r.state(id)
}

#[tokio::test(start_paused = true)]
async fn an_order_for_a_silent_vehicle_queues_then_goes_once_the_link_is_back() {
    let r = rig();
    let (cell, w) = r.watched(40, Firmware::Copter, "x10", (LAT, LON), quick());
    sleep(Duration::from_secs(1)).await;
    go_silent(&cell, &w, 5.0);
    let out = r.d.submit(&op1(), orbit(40)).await.expect("submit");
    assert_eq!(out.state, State::Queued, "{}", r.reason(out.id));
    assert!(out.tasking.is_none(), "nothing sent into the dead link");
    assert!(r.d.world.tasks.snapshot().is_empty());
    assert_eq!(r.events("intent.queued"), 1);
    // the console sees it, with why it waits now
    let inbox = r.d.inbox_json();
    assert_eq!(inbox["counts"]["queued"], 1);
    let why = inbox["queued"][0]["reach_now"][0]["detail"].to_string();
    assert!(why.contains("silent for"), "{why}");
    // still silent: it waits
    r.d.tick();
    assert_eq!(r.state(out.id), State::Queued);
    // the link is back: re-checked, dispatched, flown
    w.set_cut(false);
    sleep(Duration::from_secs(1)).await;
    assert_eq!(tick_until_dequeued(&r, out.id, 10).await, State::Dispatched, "{}", r.reason(out.id));
    assert_eq!(r.run_until_finished(out.id, 300).await, State::Done);
    assert_eq!(w.count("MISSION_COUNT"), 1, "sent once");
    assert_eq!(w.count("MAV_CMD_DO_SET_MISSION_CURRENT"), 1, "started once");
    let report = r.replays();
    assert_eq!((report.queued, report.recheck_dispatched), (1, 1));
}

#[tokio::test(start_paused = true)]
async fn a_queued_order_expires_and_can_be_cancelled() {
    let mut p = Policy::load(&root().join("sim/policy.toml")).expect("policy");
    p.queue = Some(murmur_decide::QueueConfig {
        expire_s: 60,
        ..murmur_decide::QueueConfig::default()
    });
    let r = rig_with(p);
    let (cell, w) = r.watched(41, Firmware::Copter, "x10", (LAT, LON), quick());
    let (cell2, w2) = r.watched(42, Firmware::Copter, "x10", (LAT, LON + 0.001), quick());
    sleep(Duration::from_secs(1)).await;
    go_silent(&cell, &w, 5.0);
    go_silent(&cell2, &w2, 5.0);
    let a = r.d.submit(&op1(), orbit(41)).await.expect("submit");
    let b = r.d.submit(&op1(), orbit(42)).await.expect("submit");
    assert_eq!((a.state, b.state), (State::Queued, State::Queued));
    // the operator cancels one
    let c = r.d.cancel(&op1(), b.id.0, Some("not needed".into())).expect("cancel");
    assert_eq!(c.state, State::Voided);
    // the other waits out its minute and expires, saying why
    r.clock.set(r.clock.now().plus_ms(61_000));
    r.d.tick();
    assert_eq!(r.state(a.id), State::Expired);
    assert!(r.reason(a.id).starts_with("not dispatched within 60 s"), "{}", r.reason(a.id));
    // back now: nothing goes
    w.set_cut(false);
    w2.set_cut(false);
    sleep(Duration::from_secs(2)).await;
    r.d.tick();
    assert!(r.d.world.tasks.snapshot().is_empty());
    assert_eq!(w.count("MISSION_COUNT") + w2.count("MISSION_COUNT"), 0);
    let report = r.replays();
    assert_eq!(report.queue_expired, 1);
}

#[tokio::test(start_paused = true)]
async fn a_newer_order_supersedes_an_older_queued_one() {
    let r = rig();
    let (cell, w) = r.watched(43, Firmware::Copter, "x10", (LAT, LON), quick());
    sleep(Duration::from_secs(1)).await;
    go_silent(&cell, &w, 5.0);
    let first = r.d.submit(&op1(), orbit(43)).await.expect("submit");
    let second = r.d.submit(&op1(), simple("rtl", 43)).await.expect("submit");
    assert_eq!(r.state(first.id), State::Voided, "{}", r.reason(first.id));
    assert!(r.reason(first.id).contains("superseded"), "{}", r.reason(first.id));
    assert_eq!(second.state, State::Queued);
    w.set_cut(false);
    sleep(Duration::from_secs(1)).await;
    assert_eq!(tick_until_dequeued(&r, second.id, 10).await, State::Dispatched);
    assert_eq!(w.count("MISSION_COUNT"), 0, "the orbit never went");
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn lost_acks_through_the_decision_layer_execute_once() {
    let r = rig();
    let q = Quirks {
        lose_final_ack: true,
        lose_ack: Some(crate::mav::apm::MavCmd::MAV_CMD_DO_SET_MODE),
        ..quick()
    };
    let (_cell, w) = r.watched(44, Firmware::Copter, "x10", (LAT, LON), q);
    sleep(Duration::from_secs(1)).await;
    let out = r.d.submit(&op1(), orbit(44)).await.expect("submit");
    assert_eq!(out.state, State::Dispatched);
    assert_eq!(r.run_until_finished(out.id, 300).await, State::Done);
    assert_eq!(w.count("MISSION_COUNT"), 1);
    assert_eq!(w.count("MAV_CMD_DO_SET_MODE"), 1);
    assert_eq!(w.count("MAV_CMD_DO_SET_MISSION_CURRENT"), 1);
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn a_reboot_mid_task_is_logged_and_its_intent_fails_without_a_resend() {
    let r = rig();
    let (cell, w) = r.watched(45, Firmware::Copter, "x10", (LAT, LON), quick());
    sleep(Duration::from_secs(2)).await;
    let mut req = orbit(45);
    req.params.center = Some([LON + 0.02, LAT]);
    let out = r.d.submit(&op1(), req).await.expect("submit");
    for _ in 0..60 {
        r.d.tick();
        if r.d.world.tasks.current_ops().iter().any(|o| o.state == OpState::Active) {
            break;
        }
        sleep(Duration::from_secs(1)).await;
    }
    w.reboot.store(true, Ordering::SeqCst);
    assert_eq!(r.run_until_finished(out.id, 60).await, State::Failed);
    assert!(r.reason(out.id).contains("rebooted"), "{}", r.reason(out.id));
    assert_eq!(r.events("vehicle.rebooted"), 1);
    assert_eq!(cell.read(|v| v.boot_epoch), 1);
    assert_eq!(w.count("MISSION_COUNT"), 1, "not sent again");
    let report = r.replays();
    assert_eq!(report.reboots, 1);
}

#[tokio::test(start_paused = true)]
async fn a_human_approved_order_queued_across_a_reboot_returns_to_the_inbox() {
    let r = rig();
    // armed: a human tier, the operator's own submission approves it
    let (cell, w) = r.watched(46, Firmware::Copter, "switchblade", (LAT, LON), quick());
    sleep(Duration::from_secs(1)).await;
    go_silent(&cell, &w, 5.0);
    let out = r.d.submit(&op1(), orbit(46)).await.expect("submit");
    assert_eq!(out.state, State::Queued, "{}", r.reason(out.id));
    // it rebooted while out of contact: when it is heard again it booted a minute after the
    // approval (the bridge's estimate; the test's wall clock does not move that far)
    w.reboot.store(true, Ordering::SeqCst);
    w.set_cut(false);
    sleep(Duration::from_secs(1)).await;
    cell.write(|v| v.booted_wall = Some(clock::wall() + 60.0));
    assert_eq!(tick_until_dequeued(&r, out.id, 10).await, State::PendingApproval);
    assert!(r.reason(out.id).contains("rebooted after the approval"), "{}", r.reason(out.id));
    assert_eq!(w.count("MISSION_COUNT"), 0, "not sent: a human looks again");
    assert_eq!(r.d.inbox_json()["counts"]["pending"], 1);
    // another operator approves it again: re-checked, sent
    let a = r.d.approve(&OperatorId::new("op-2"), out.id.0, None).await.expect("approve");
    assert_eq!(a.state, State::Dispatched, "{}", r.reason(out.id));
    let report = r.replays();
    assert_eq!(report.recheck_inbox, 1);
}

/// A second bridge on the same world, its own engine and log (a restart).
fn restart(r: &Rig) -> (Arc<Decision>, MemorySink) {
    let root = root();
    let log = MemorySink::new();
    let d = Decision::from_parts(
        r.world.clone(),
        Parts {
            policy: Policy::load(&root.join("sim/policy.toml")).expect("policy"),
            catalog: Catalog::load(&root.join("sim/platforms.toml")).expect("catalog"),
            areas: AreaSet::load_dir(&root.join("sim/areas"), r.clock.now()).expect("areas"),
            sink: Box::new(log.clone()),
            audit_path: PathBuf::from("memory-b"),
            clock: Arc::new(r.clock.clone()),
            terrain: Terrain::fixed(PolygonTerrain {
                coverage: ring(-122.6, 37.7, -122.3, 37.9),
                land: vec![ring(-122.48, 37.79, -122.44, 37.808)],
                water_depth_m: 12.0,
            }),
            operators: vec![OperatorId::new("op-1"), OperatorId::new("op-2")],
            engage_operators: Vec::new(),
            tracks: None,
        },
    )
    .expect("engine");
    (d, log)
}

fn replays_after(b: &MemorySink, a: &MemorySink) {
    let a_text = a.text();
    let rb = replay_str_with(
        &b.text(),
        &ReplayOptions {
            previous: Some(&a_text),
            ..ReplayOptions::default()
        },
    )
    .expect("parses");
    assert!(rb.ok(), "{:?}", rb.mismatches);
    assert!(rb.resumed > 0);
}

#[tokio::test(start_paused = true)]
async fn a_bridge_restart_mid_queue_resumes_and_sends_once() {
    let r = rig();
    let (cell, w) = r.watched(47, Firmware::Copter, "x10", (LAT, LON), quick());
    sleep(Duration::from_secs(1)).await;
    go_silent(&cell, &w, 5.0);
    let out = r.d.submit(&op1(), orbit(47)).await.expect("submit");
    assert_eq!(out.state, State::Queued);
    r.replays();
    // the bridge dies; a new one starts and carries the queued order over from the log
    let (b, blog) = restart(&r);
    let carried = b.carry_over("a.jsonl", &r.log.text()).expect("carried");
    assert_eq!(carried, vec![out.id]);
    let state = |id: IntentId| b.lock().engine.intent(id).map(Intent::state);
    assert_eq!(state(out.id), Some(State::Queued));
    // the link comes back: re-checked by the new bridge, sent once
    w.set_cut(false);
    sleep(Duration::from_secs(1)).await;
    for _ in 0..10 {
        b.tick();
        if state(out.id) != Some(State::Queued) {
            break;
        }
        sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(state(out.id), Some(State::Dispatched));
    for _ in 0..300 {
        b.tick();
        if state(out.id).is_some_and(State::is_terminal) {
            break;
        }
        sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(state(out.id), Some(State::Done));
    assert_eq!(w.count("MISSION_COUNT"), 1);
    replays_after(&blog, &r.log);
}

#[tokio::test(start_paused = true)]
async fn a_bridge_restart_with_an_order_under_way_follows_it_without_a_resend() {
    let r = rig();
    let (_cell, w) = r.watched(48, Firmware::Rover, "seahunter", SEA, quick());
    sleep(Duration::from_secs(1)).await;
    let route = vec![[SEA.1 + 0.01, SEA.0], [SEA.1 + 0.01, SEA.0 + 0.01]];
    let out = r.d.submit(&op1(), patrol(48, route)).await.expect("submit");
    assert_eq!(out.state, State::Dispatched, "{}", r.reason(out.id));
    // under way: acked and active in the log
    for _ in 0..60 {
        r.d.tick();
        if r.log.text().contains("\"state\":\"active\"") {
            break;
        }
        sleep(Duration::from_secs(1)).await;
    }
    assert!(r.log.text().contains("\"state\":\"active\""));
    let (b, blog) = restart(&r);
    b.carry_over("a.jsonl", &r.log.text()).expect("carried");
    let state = |id: IntentId| b.lock().engine.intent(id).map(Intent::state);
    assert_eq!(state(out.id), Some(State::Dispatched), "resumed under way");
    for _ in 0..600 {
        b.tick();
        if state(out.id).is_some_and(State::is_terminal) {
            break;
        }
        sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(state(out.id), Some(State::Done));
    assert_eq!(w.count("MISSION_COUNT"), 1, "followed, not sent again");
    assert_eq!(w.count("MAV_CMD_DO_SET_MISSION_CURRENT"), 1, "not restarted");
    assert!(!blog.text().contains("\"type\":\"intent.dispatched\""));
    replays_after(&blog, &r.log);
}

#[test]
fn the_pointer_names_the_log_to_resume_from() {
    use crate::decision::queue::{Pointer, ResumeFrom, previous_log};
    let dir = std::env::temp_dir().join(format!("murmur-resume-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let theirs = dir.join("a.jsonl");
    std::fs::write(&theirs, "").expect("log");
    let ours = dir.join("b.jsonl");
    let ptr = dir.join(".resume-1.json");
    let write = |log: &PathBuf, t: f64| {
        let p = Pointer { log: log.clone(), open: Vec::new(), t };
        std::fs::write(&ptr, serde_json::to_vec(&p).expect("json")).expect("pointer");
    };
    let window = ResumeFrom::Pointer { window_s: 900.0 };
    // fresh: that log, whatever its (once a tick) list of open ids says: the log decides
    write(&theirs, clock::wall());
    assert_eq!(previous_log(&window, Some(&ptr), &ours), Some(theirs.clone()));
    // stale, our own log, a log that is gone, or turned off: none
    write(&theirs, clock::wall() - 1_000.0);
    assert_eq!(previous_log(&window, Some(&ptr), &ours), None);
    write(&ours, clock::wall());
    assert_eq!(previous_log(&window, Some(&ptr), &ours), None);
    write(&dir.join("gone.jsonl"), clock::wall());
    assert_eq!(previous_log(&window, Some(&ptr), &ours), None);
    assert_eq!(previous_log(&ResumeFrom::Off, Some(&ptr), &ours), None);
    // or a log named outright
    assert_eq!(previous_log(&ResumeFrom::Log(theirs.clone()), None, &ours), Some(theirs));
    let _ = std::fs::remove_dir_all(&dir);
}
