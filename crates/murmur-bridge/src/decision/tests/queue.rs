//! The safe command queue through the bridge (rules 5, murmur-5p4): an order for a silent
//! vehicle is held, not sent into the dead link, and goes once the link is back (re-checked
//! first); it expires, comes back to the inbox when a human's picture changed, survives a
//! bridge restart, and a vehicle reboot mid-task ends its op without a resend. Every log
//! replays (a restarted run's against the run before it).

use std::sync::atomic::Ordering;

use super::*;
use crate::decision::queue::CarryCheck;
use crate::tasks::tests::{Quirks, Watch};
use murmur_decide::replay::{ReplayOptions, replay_str_with};

impl Rig {
    /// A fake whose link can be cut, whose received commands are counted, and which can be
    /// rebooted.
    pub(super) fn watched(&self, uid: Uid, fw: Firmware, platform: &str, at: (f64, f64), q: Quirks) -> (Arc<VehicleCell>, Watch) {
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
pub(super) fn go_silent(cell: &VehicleCell, w: &Watch, s: f64) {
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
    let a = r.d.approve(&Actor::asserted(OperatorId::new("op-2")), out.id.0, None).await.expect("approve");
    assert_eq!(a.state, State::Dispatched, "{}", r.reason(out.id));
    let report = r.replays();
    assert_eq!(report.recheck_inbox, 1);
}

/// A second bridge on the same world, its own engine and log (a restart).
fn restart(r: &Rig) -> (Arc<Decision>, MemorySink) {
    restart_as(r, murmur_decide::AuthMode::Dev)
}

/// [`restart`], the new bridge authenticating operators as `auth` says.
fn restart_as(r: &Rig, auth: murmur_decide::AuthMode) -> (Arc<Decision>, MemorySink) {
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
            auth,
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
    let carried = b.carry_over("a.jsonl", &r.log.text(), &CarryCheck::default()).expect("carried");
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

/// Review fix 3: a crashed dev run's open orders (asserted ids, no step-up) are not carried
/// into a credentials run, whose log would then hold them under authenticated people's name;
/// nor the other way. Nothing is carried and the new log says nothing was.
#[tokio::test(start_paused = true)]
async fn open_orders_are_not_carried_across_authentication_modes() {
    use murmur_decide::AuthMode;
    for (from, to) in [(AuthMode::Dev, AuthMode::Credentials), (AuthMode::Credentials, AuthMode::Dev)] {
        let r = rig_auth(Policy::load(&root().join("sim/policy.toml")).expect("policy"), from);
        let (cell, w) = r.watched(47, Firmware::Copter, "x10", (LAT, LON), quick());
        sleep(Duration::from_secs(1)).await;
        go_silent(&cell, &w, 5.0);
        let by = match from {
            AuthMode::Dev => op1(),
            AuthMode::Credentials => {
                let op = OperatorId::new("op-1");
                let sid = r.d.open_login(&op, false, false).expect("login");
                Actor::logged_in(op, sid)
            }
        };
        let out = r.d.submit(&by, orbit(47)).await.expect("submit");
        assert_eq!(out.state, State::Queued);
        r.replays();
        let (b, blog) = restart_as(&r, to);
        let carried = b.carry_over("a.jsonl", &r.log.text(), &CarryCheck::default());
        assert!(carried.is_err(), "{from:?} -> {to:?}: {carried:?}");
        assert!(b.lock().engine.intent(out.id).is_none());
        assert!(!blog.text().contains("\"type\":\"intent.resumed\""));
    }
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
    b.carry_over("a.jsonl", &r.log.text(), &CarryCheck::default()).expect("carried");
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
    let write_by = |log: &PathBuf, t: f64, pid: u32| {
        let p = Pointer { log: log.clone(), open: Vec::new(), t, pid };
        std::fs::write(&ptr, serde_json::to_vec(&p).expect("json")).expect("pointer");
    };
    let write = |log: &PathBuf, t: f64| write_by(log, t, 0);
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
    assert_eq!(previous_log(&ResumeFrom::Log(theirs.clone()), None, &ours), Some(theirs.clone()));
    // written by a bridge that still runs (pid 1 is always alive): not taken over
    write_by(&theirs, clock::wall(), 1);
    assert_eq!(previous_log(&window, Some(&ptr), &ours), None);
    // a clean shutdown deletes the pointer to its own log (only a crash leaves one behind);
    // one naming another log is left alone
    write(&theirs, clock::wall());
    crate::decision::queue::drop_pointer(&ptr, &ours);
    assert!(ptr.exists());
    crate::decision::queue::drop_pointer(&ptr, &theirs);
    assert!(!ptr.exists());
    assert_eq!(previous_log(&window, Some(&ptr), &ours), None);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- review fixes ---------------------------------------------------------------------

/// Tick once a second until `f` holds.
async fn tick_until(r: &Rig, limit_s: u64, f: impl Fn(&Rig) -> bool) -> bool {
    for _ in 0..limit_s {
        r.d.tick();
        if f(r) {
            return true;
        }
        sleep(Duration::from_secs(1)).await;
    }
    f(r)
}

#[tokio::test(start_paused = true)]
async fn a_link_lost_before_the_start_sends_the_order_back_to_the_queue_and_it_goes_once() {
    // review T1, the way back: the link drops the moment the vehicle has the whole orbit,
    // before its ACK. The op ends `requeued`; the order goes back to the queue; when the link
    // is back it is re-checked, and the vehicle (asked first) holds it: only the start goes
    let r = rig();
    let q = Quirks {
        cut_after_upload: true,
        ..quick()
    };
    let (_cell, w) = r.watched(61, Firmware::Copter, "x10", (LAT, LON), q);
    sleep(Duration::from_secs(1)).await;
    let out = r.d.submit(&op1(), orbit(61)).await.expect("submit");
    assert_eq!(out.state, State::Dispatched, "{}", r.reason(out.id));
    assert!(tick_until(&r, 60, |r| r.state(out.id) == State::Queued).await, "{}", r.reason(out.id));
    assert!(r.reason(out.id).starts_with("back to the queue"), "{}", r.reason(out.id));
    assert_eq!(r.events("intent.requeued"), 1);
    assert_eq!(w.count("MAV_CMD_DO_SET_MISSION_CURRENT"), 0, "not started");
    w.set_cut(false);
    sleep(Duration::from_secs(1)).await;
    assert_eq!(tick_until_dequeued(&r, out.id, 10).await, State::Dispatched, "{}", r.reason(out.id));
    assert_eq!(r.run_until_finished(out.id, 300).await, State::Done, "{}", r.reason(out.id));
    assert_eq!(w.count("MISSION_COUNT"), 1, "not uploaded again: it held the orbit");
    assert_eq!(w.count("MAV_CMD_DO_SET_MISSION_CURRENT"), 1, "started once");
    let report = r.replays();
    assert_eq!((report.requeued, report.recheck_dispatched), (1, 1));
}

#[tokio::test(start_paused = true)]
async fn a_requeued_order_is_superseded_by_a_newer_one_and_never_starts() {
    // review T1: an orbit loses its link before it started; the operator orders a hold while
    // the vehicle is silent (queued); the hold voids the older orbit, then expires; when the
    // link is back minutes later nothing starts
    let r = rig();
    let q = Quirks {
        cut_after_upload: true,
        ..quick()
    };
    let (cell, w) = r.watched(60, Firmware::Copter, "x10", (LAT, LON), q);
    sleep(Duration::from_secs(1)).await;
    let out = r.d.submit(&op1(), orbit(60)).await.expect("submit");
    assert_eq!(out.state, State::Dispatched, "{}", r.reason(out.id));
    assert!(tick_until(&r, 60, |r| r.state(out.id) == State::Queued).await, "{}", r.reason(out.id));
    go_silent(&cell, &w, 5.0);
    let h = r.d.submit(&op1(), simple("hold", 60)).await.expect("submit");
    assert_eq!(h.state, State::Queued, "{}", r.reason(h.id));
    assert_eq!(r.state(out.id), State::Voided, "{}", r.reason(out.id));
    assert!(r.reason(out.id).contains("superseded"), "{}", r.reason(out.id));
    r.clock.set(r.clock.now().plus_ms(61_000));
    r.d.tick();
    assert_eq!(r.state(h.id), State::Expired, "{}", r.reason(h.id));
    sleep(Duration::from_secs(240)).await;
    w.set_cut(false);
    for _ in 0..60 {
        r.d.tick();
        sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(w.count("MAV_CMD_DO_SET_MISSION_CURRENT"), 0, "the older orbit never started");
    assert_eq!(w.count("MAV_CMD_DO_SET_MODE"), 0);
    assert!(
        !r.log.text().contains(&format!("\"type\":\"queue.recheck\",\"intent\":{}", out.id.0)),
        "nothing re-checked it"
    );
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn an_older_queued_order_never_voids_a_newer_pending_one() {
    // review T2: the older order waits in the queue; a newer one for the same vehicle waits
    // for a second approver. The link comes back: the older is voided, the newer stands
    let r = waiting_rig();
    let (cell, w) = r.watched(72, Firmware::Copter, "switchblade", (LAT, LON), quick());
    sleep(Duration::from_secs(1)).await;
    go_silent(&cell, &w, 5.0);
    let a = r.d.submit(&op1(), orbit(72)).await.expect("submit");
    assert_eq!(a.state, State::PendingApproval, "{}", r.reason(a.id));
    let a2 = r.d.approve(&Actor::asserted(OperatorId::new("op-2")), a.id.0, None).await.expect("approve");
    assert_eq!(a2.state, State::Queued, "{}", r.reason(a.id));
    let mut req = orbit(72);
    req.params.center = Some([LON - 0.004, LAT]);
    let b = r.d.submit(&op1(), req).await.expect("submit");
    assert_eq!(b.state, State::PendingApproval, "{}", r.reason(b.id));
    w.set_cut(false);
    sleep(Duration::from_secs(1)).await;
    let s = tick_until_dequeued(&r, a.id, 10).await;
    assert_eq!(s, State::Voided, "{}", r.reason(a.id));
    assert!(r.reason(a.id).contains("newer order"), "{}", r.reason(a.id));
    assert_eq!(r.state(b.id), State::PendingApproval, "the newer order stands");
    assert_eq!(w.count("MISSION_COUNT"), 0);
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn another_fleet_under_the_same_uids_inherits_nothing_and_a_carried_approval_is_looked_at_again() {
    // review T3: a new run (another exercise) whose uid 70 is another platform does not
    // inherit the old run's order for uid 70
    let r = rig();
    let (cell, w) = r.watched(70, Firmware::Copter, "x10", (LAT, LON), quick());
    sleep(Duration::from_secs(1)).await;
    go_silent(&cell, &w, 5.0);
    let out = r.d.submit(&op1(), orbit(70)).await.expect("submit");
    assert_eq!(out.state, State::Queued);
    let r2 = rig();
    let (_cell2, w2) = r2.watched(70, Firmware::Rover, "seahunter", SEA, quick());
    sleep(Duration::from_secs(2)).await;
    let carried = r2.d.carry_over("a.jsonl", &r.log.text(), &CarryCheck::default()).expect("replays");
    assert!(carried.is_empty(), "{carried:?}");
    for _ in 0..10 {
        r2.d.tick();
        sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(w2.count("MISSION_COUNT"), 0, "nothing sent to the new run's vehicle");
    // the same platform under the uid (a crash, the same fleet): carried, and a human's
    // approval from the run before goes back to the inbox
    let r = rig();
    let (cell, w) = r.watched(73, Firmware::Copter, "switchblade", (LAT, LON), quick());
    sleep(Duration::from_secs(1)).await;
    go_silent(&cell, &w, 5.0);
    let out = r.d.submit(&op1(), orbit(73)).await.expect("submit");
    assert_eq!(out.state, State::Queued, "{}", r.reason(out.id));
    let (b, blog) = restart(&r);
    let carried = b.carry_over("a.jsonl", &r.log.text(), &CarryCheck::default()).expect("carried");
    assert_eq!(carried, vec![out.id]);
    w.set_cut(false);
    sleep(Duration::from_secs(1)).await;
    let state = |id: IntentId| b.lock().engine.intent(id).map(Intent::state);
    for _ in 0..10 {
        b.tick();
        if state(out.id) != Some(State::Queued) {
            break;
        }
        sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(state(out.id), Some(State::PendingApproval));
    assert!(blog.text().contains("approved before a bridge restart"));
    assert_eq!(w.count("MISSION_COUNT"), 0, "not sent: a human looks again");
    replays_after(&blog, &r.log);
    // a log that does not match its anchors carries nothing
    let anchors = "{\"seq\":1,\"t\":0,\"head\":\"00\",\"first\":\"00\",\"why\":\"start\"}\n";
    let (c, _) = restart(&r);
    assert!(c.carry_over("a.jsonl", &r.log.text(), &CarryCheck { anchors: Some(anchors.into()), fleet: None }).is_err());
}

#[tokio::test(start_paused = true)]
async fn a_reboot_during_an_outage_longer_than_its_uptime_is_seen() {
    // review T5: up a minute, then out of contact for 90 s and rebooted in it: its time since
    // boot (90 s) does not go back past the last one it reported (60 s), but the boot instant
    // it implies jumps a minute later. The reboot is logged, and the human's approval from
    // before it goes back to the inbox
    let r = rig();
    let (cell, w) = r.watched(71, Firmware::Copter, "switchblade", (LAT, LON), quick());
    sleep(Duration::from_secs(60)).await;
    go_silent(&cell, &w, 5.0);
    let out = r.d.submit(&op1(), orbit(71)).await.expect("submit");
    assert_eq!(out.state, State::Queued, "{}", r.reason(out.id));
    w.reboot.store(true, Ordering::SeqCst);
    sleep(Duration::from_secs(90)).await;
    // the engine's clock is manual (the facts' times are the wall clock's, which a paused
    // test barely moves): a moment on, so the reboot is logged after the approval
    r.clock.set(r.clock.now().plus_ms(1_500));
    w.set_cut(false);
    sleep(Duration::from_secs(1)).await;
    let s = tick_until_dequeued(&r, out.id, 10).await;
    assert_eq!(cell.read(|v| v.boot_epoch), 1, "the reboot was seen");
    assert_eq!(r.events("vehicle.rebooted"), 1);
    assert_eq!(s, State::PendingApproval, "{}", r.reason(out.id));
    assert!(r.reason(out.id).contains("rebooted after the approval"), "{}", r.reason(out.id));
    assert_eq!(w.count("MISSION_COUNT"), 0);
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn a_second_restart_still_follows_an_order_started_two_runs_ago() {
    // review T6: run A's boat started a patrol; run B carried it over and its follow op
    // reported `queued` before B crashed; run C still follows it and never sends it again
    let r = rig();
    let (_cell, w) = r.watched(49, Firmware::Rover, "seahunter", SEA, quick());
    sleep(Duration::from_secs(1)).await;
    let route = vec![[SEA.1 + 0.01, SEA.0], [SEA.1 + 0.01, SEA.0 + 0.01]];
    let out = r.d.submit(&op1(), patrol(49, route)).await.expect("submit");
    assert_eq!(out.state, State::Dispatched, "{}", r.reason(out.id));
    assert!(tick_until(&r, 60, |r| r.log.text().contains("\"state\":\"active\"")).await);
    let (b, blog) = restart(&r);
    b.carry_over("a.jsonl", &r.log.text(), &CarryCheck::default()).expect("carried");
    // B's follow op reports its first state, then B crashes
    for _ in 0..3 {
        b.tick();
        if blog.text().contains("\"type\":\"op.state\"") {
            break;
        }
        sleep(Duration::from_millis(200)).await;
    }
    let cut: String = blog
        .lines()
        .iter()
        .take_while(|l| !l.contains("\"state\":\"acked\""))
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(cut.contains("\"type\":\"op.state\""), "{cut}");
    let (c, clog) = restart(&r);
    let carried = c.carry_over("b.jsonl", &cut, &CarryCheck::default()).expect("carried");
    assert_eq!(carried, vec![out.id]);
    let state = |id: IntentId| c.lock().engine.intent(id).map(Intent::state);
    assert_eq!(state(out.id), Some(State::Dispatched), "followed, as it started in run A");
    for _ in 0..600 {
        c.tick();
        if state(out.id).is_some_and(State::is_terminal) {
            break;
        }
        sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(state(out.id), Some(State::Done));
    assert_eq!(w.count("MISSION_COUNT"), 1, "never sent again");
    assert_eq!(w.count("MAV_CMD_DO_SET_MISSION_CURRENT"), 1);
    let rc = replay_str_with(
        &clog.text(),
        &ReplayOptions {
            previous: Some(&cut),
            ..ReplayOptions::default()
        },
    )
    .expect("parses");
    assert!(rc.ok(), "{:?}", rc.mismatches);
}

// ---- second review --------------------------------------------------------------------

fn ops_of(r: &Rig) -> Vec<(u64, OpState, String)> {
    r.d.world
        .tasks
        .current_ops()
        .iter()
        .map(|o| (o.intent_id, o.state, o.detail.clone()))
        .collect()
}

#[tokio::test(start_paused = true)]
async fn an_order_whose_start_went_out_unconfirmed_is_followed_when_the_vehicle_runs_it() {
    // review 2, P2a: the copter switched to AUTO (it started the orbit) but the link died
    // before the ACK. The op ends `unconfirmed`, the order goes back to the queue; when the
    // link is back it is re-checked and only confirmed: the copter runs it, so it is
    // followed, nothing sent again
    let r = rig();
    let q = Quirks {
        cut_after_auto: true,
        ..quick()
    };
    let (_cell, w) = r.watched(80, Firmware::Copter, "x10", (LAT, LON), q);
    sleep(Duration::from_secs(1)).await;
    let out = r.d.submit(&op1(), orbit(80)).await.expect("submit");
    assert_eq!(out.state, State::Dispatched);
    assert!(tick_until(&r, 60, |r| r.state(out.id) == State::Queued).await, "{}", r.reason(out.id));
    assert!(r.reason(out.id).contains("after its start went out"), "{}", r.reason(out.id));
    assert!(r.log.text().contains("\"state\":\"unconfirmed\""));
    sleep(Duration::from_secs(20)).await;
    w.set_cut(false);
    sleep(Duration::from_secs(1)).await;
    assert_eq!(tick_until_dequeued(&r, out.id, 10).await, State::Dispatched, "{}", r.reason(out.id));
    assert!(tick_until(&r, 30, |r| ops_of(r).iter().any(|(i, s, _)| *i == out.id.0 && matches!(s, OpState::Active | OpState::Done))).await, "{:?}", ops_of(&r));
    assert_eq!(w.count("MISSION_COUNT"), 1);
    assert_eq!(w.count("MAV_CMD_DO_SET_MISSION_CURRENT"), 1);
    assert_eq!(w.count("MAV_CMD_DO_SET_MODE"), 1, "followed, not started again");
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn an_order_that_may_have_started_and_does_not_run_goes_to_a_human() {
    // review 2, P2b: as above, but the copter's failsafe took it out of AUTO during the
    // outage. The confirming delivery finds the orbit held, not running: it is not started
    // again on its own (no second SET_MISSION_CURRENT or AUTO); a human decides, and when one
    // approves it again it is only started (it is held)
    let r = rig();
    let q = Quirks {
        cut_after_auto: true,
        bail_after: Some((5.0, "RTL", "Failsafe: GCS")),
        ..quick()
    };
    let (cell, w) = r.watched(81, Firmware::Copter, "x10", (LAT, LON), q);
    sleep(Duration::from_secs(1)).await;
    let out = r.d.submit(&op1(), orbit(81)).await.expect("submit");
    assert!(tick_until(&r, 60, |r| r.state(out.id) == State::Queued).await);
    sleep(Duration::from_secs(20)).await;
    w.set_cut(false);
    sleep(Duration::from_secs(1)).await;
    assert!(
        tick_until(&r, 30, |r| r.state(out.id) == State::PendingApproval).await,
        "{} {:?}",
        r.reason(out.id),
        ops_of(&r)
    );
    let why = r.reason(out.id);
    assert!(why.contains("does not run now") && why.contains("a human decides"), "{why}");
    assert!(why.contains("mode RTL"), "{why}");
    assert_eq!(w.count("MAV_CMD_DO_SET_MISSION_CURRENT"), 1, "not started again");
    assert_eq!(w.count("MAV_CMD_DO_SET_MODE"), 1);
    assert_eq!(cell.read(|v| v.mode.clone()).as_deref(), Some("RTL"), "the failsafe's mode stands");
    r.replays();
    // a human approves it again: asked first, it is held, so only the start goes
    let a = r.d.approve(&Actor::asserted(OperatorId::new("op-2")), out.id.0, None).await.expect("approve");
    assert_eq!(a.state, State::Dispatched, "{}", r.reason(out.id));
    assert!(tick_until(&r, 60, |r| ops_of(r).iter().any(|(i, s, _)| *i == out.id.0 && matches!(s, OpState::Active | OpState::Done))).await, "{:?}", ops_of(&r));
    assert_eq!(w.count("MISSION_COUNT"), 1, "held: not uploaded again");
    assert_eq!(w.count("MAV_CMD_DO_SET_MISSION_CURRENT"), 2, "started again, as the human decided");
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn an_order_that_may_be_running_says_so_and_the_vehicle_running_it_raises_an_alert() {
    // review 2, P2c: as P2a; during the outage the operator orders a hold (queued): it voids
    // the orbit, then expires. The orbit's end says it may be running; when the link is back
    // the copter flies it, armed: an alert says so, and its tasking stays on the map
    let r = rig();
    let q = Quirks {
        cut_after_auto: true,
        ..quick()
    };
    let (cell, w) = r.watched(82, Firmware::Copter, "x10", (LAT, LON), q);
    sleep(Duration::from_secs(1)).await;
    let out = r.d.submit(&op1(), orbit(82)).await.expect("submit");
    assert!(tick_until(&r, 60, |r| r.state(out.id) == State::Queued).await);
    go_silent(&cell, &w, 5.0);
    let h = r.d.submit(&op1(), simple("hold", 82)).await.expect("submit");
    assert_eq!(h.state, State::Queued);
    assert_eq!(r.state(out.id), State::Voided);
    assert!(r.reason(out.id).contains("may be running on vehicle(s) 82"), "{}", r.reason(out.id));
    r.clock.set(r.clock.now().plus_ms(61_000));
    r.d.tick();
    assert_eq!(r.state(h.id), State::Expired);
    sleep(Duration::from_secs(240)).await;
    w.set_cut(false);
    for _ in 0..10 {
        r.d.tick();
        sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(cell.read(|v| (v.mode.clone(), v.armed)), (Some("AUTO".into()), true));
    let alerts = r.d.inbox_json()["alerts"].clone();
    let a = alerts
        .as_array()
        .and_then(|a| a.iter().find(|a| a["kind"] == "may_be_running"))
        .cloned()
        .unwrap_or_else(|| panic!("{alerts}"));
    assert_eq!(a["severity"], "high");
    assert!(a["detail"].as_str().unwrap_or("").contains("in AUTO, armed"), "{a}");
    assert!(
        r.d.world.tasks.current_ops().iter().any(|o| o.intent_id == out.id.0 && o.state == OpState::Unconfirmed),
        "its tasking stays on the map"
    );
    assert_eq!(w.count("MAV_CMD_DO_SET_MODE"), 1);
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn an_op_whose_start_went_out_is_not_abandoned_as_unstarted() {
    // review 2, P3: a newer order queued while the older one's AUTO went out unconfirmed:
    // the older op is not "abandoned before the vehicle started it"; it ends unconfirmed, and
    // the older order's end says it may be running
    let r = rig();
    let q = Quirks {
        cut_after_auto: true,
        ..quick()
    };
    let (cell, w) = r.watched(83, Firmware::Copter, "x10", (LAT, LON), q);
    sleep(Duration::from_secs(1)).await;
    let out = r.d.submit(&op1(), orbit(83)).await.expect("submit");
    for _ in 0..2000 {
        if w.cut.load(Ordering::SeqCst) {
            break;
        }
        sleep(Duration::from_millis(5)).await;
    }
    go_silent(&cell, &w, 5.0);
    let h = r.d.submit(&op1(), simple("hold", 83)).await.expect("submit");
    assert_eq!(h.state, State::Queued);
    assert!(tick_until(&r, 60, |r| r.state(out.id).is_terminal()).await, "{}", r.reason(out.id));
    let ops = ops_of(&r);
    assert!(
        ops.iter().all(|(_, _, d)| !d.contains("abandoned before the vehicle started it")),
        "{ops:?}"
    );
    assert!(ops.iter().any(|(i, s, _)| *i == out.id.0 && *s == OpState::Unconfirmed), "{ops:?}");
    assert!(r.reason(out.id).contains("may be running"), "{}", r.reason(out.id));
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn a_carried_order_needs_the_same_vehicle_not_only_the_same_platform() {
    // review 2, P5: a crash, then a run whose uid 70 is the same platform (and callsign) but
    // another vehicle 1.4 km away: not carried. The same vehicle (its home where it was):
    // carried
    let r = rig();
    let (cell, w) = r.watched(70, Firmware::Copter, "x10", (LAT, LON), quick());
    sleep(Duration::from_secs(1)).await;
    go_silent(&cell, &w, 5.0);
    let out = r.d.submit(&op1(), orbit(70)).await.expect("submit");
    assert_eq!(out.state, State::Queued);
    let was = r.d.fleet_identity();
    assert!(was.get(&70).and_then(|v| v.home).is_some(), "{was:?}");
    let check = CarryCheck {
        anchors: None,
        fleet: Some(was),
    };
    let r2 = rig();
    let (_c2, w2) = r2.watched(70, Firmware::Copter, "x10", (LAT - 0.01, LON - 0.01), quick());
    sleep(Duration::from_secs(2)).await;
    let carried = r2.d.carry_over("a.jsonl", &r.log.text(), &check).expect("replays");
    assert!(carried.is_empty(), "{carried:?}");
    for _ in 0..20 {
        r2.d.tick();
        sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(w2.count("MISSION_COUNT"), 0, "nothing sent to another vehicle");
    let (b, _) = restart(&r);
    let carried = b.carry_over("a.jsonl", &r.log.text(), &check).expect("carried");
    assert_eq!(carried, vec![out.id], "the same vehicle");
}

#[tokio::test(start_paused = true)]
async fn a_slow_simulator_clock_is_not_a_reboot() {
    // review 2, P6b: no reboot; a simulator whose clock runs at 0.9 of ours, silent 700 s:
    // its time since boot lags the silence by 70 s, which the clock rate it showed explains
    let r = rig();
    let q = Quirks {
        boot_rate: 0.9,
        ..quick()
    };
    let (cell, w) = r.watched(85, Firmware::Copter, "x10", (LAT, LON), q);
    sleep(Duration::from_secs(60)).await;
    w.set_cut(true);
    sleep(Duration::from_secs(700)).await;
    w.set_cut(false);
    sleep(Duration::from_secs(3)).await;
    r.d.tick();
    assert_eq!(cell.read(|v| v.boot_epoch), 0);
    assert_eq!(r.events("vehicle.rebooted"), 0);
    // a real reboot on that clock, in an outage longer than the uptime, is still seen
    sleep(Duration::from_secs(120)).await;
    w.set_cut(true);
    w.reboot.store(true, Ordering::SeqCst);
    sleep(Duration::from_secs(300)).await;
    w.set_cut(false);
    sleep(Duration::from_secs(3)).await;
    r.d.tick();
    assert_eq!(cell.read(|v| v.boot_epoch), 1);
    assert_eq!(r.events("vehicle.rebooted"), 1);
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn an_order_approved_again_after_a_restart_asks_the_vehicle_first() {
    // review 2, P8: a human-approved orbit; the copter switched to AUTO but the ACK was lost
    // with the link, and the bridge crashed then. The next run carries it over, its re-check
    // sends it to the inbox (approved before a restart), a human approves it again: it is
    // delivered asking the copter first, which runs it: followed, nothing sent again
    let r = rig();
    let q = Quirks {
        cut_after_auto: true,
        ..quick()
    };
    let (cell, w) = r.watched(86, Firmware::Copter, "switchblade", (LAT, LON), q);
    sleep(Duration::from_secs(1)).await;
    let out = r.d.submit(&op1(), orbit(86)).await.expect("submit");
    assert_eq!(out.state, State::Dispatched, "{}", r.reason(out.id));
    for _ in 0..2000 {
        if w.cut.load(Ordering::SeqCst) {
            break;
        }
        sleep(Duration::from_millis(5)).await;
    }
    r.d.tick();
    let crashed = r.log.text();
    assert!(crashed.contains("\"state\":\"starting\""), "the start went out");
    let (b, blog) = restart(&r);
    let carried = b.carry_over("a.jsonl", &crashed, &CarryCheck::default()).expect("carried");
    assert_eq!(carried, vec![out.id]);
    let state = |id: IntentId| b.lock().engine.intent(id).map(Intent::state);
    w.set_cut(false);
    sleep(Duration::from_secs(2)).await;
    for _ in 0..10 {
        b.tick();
        if state(out.id) != Some(State::Queued) {
            break;
        }
        sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(state(out.id), Some(State::PendingApproval));
    assert_eq!(cell.read(|v| v.mode.clone()).as_deref(), Some("AUTO"));
    let a = b.approve(&Actor::asserted(OperatorId::new("op-2")), out.id.0, None).await.expect("approve");
    assert_eq!(a.state, State::Dispatched);
    for _ in 0..20 {
        b.tick();
        sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(w.count("MISSION_COUNT"), 1, "not uploaded again");
    assert_eq!(w.count("MAV_CMD_DO_SET_MISSION_CURRENT"), 1, "not started again");
    let b_text = blog.text();
    assert!(!b_text.contains("\"state\":\"sent\""), "followed: nothing sent in the second run");
    // rules 7: the follow is logged ahead like a start (from then on the copter runs it), and
    // says it was followed
    assert!(b_text.contains("followed, not sent again"), "the follow logged ahead");
    for l in b_text.lines().filter(|l| l.contains("\"state\":\"starting\"")) {
        assert!(l.contains("followed, not sent again") && l.contains("\"ahead\":true"), "{l}");
    }
    replays_after(&blog, &r.log);
}

#[test]
fn the_fleet_identity_takes_each_vehicles_spawn_point_from_the_manifest() {
    // found live: the world's identities leave the manifest's `home` out, so a run's fleet
    // identity had homes only for vehicles already connected, and a carry-over at start
    // (none connected yet) could not tell any vehicle the same
    let manifest = json!({ "vehicles": [
        { "uid": 1, "sysid": 1, "platform": "remus", "callsign": "REMUS 1", "home": [37.825, -122.44, 0, 90] },
        { "sysid": 7, "platform": "x10", "callsign": "SKY 1", "home": [37.805, -122.45, 0, 0] },
    ]});
    let f = crate::decision::queue::manifest_identities(&manifest);
    assert_eq!(f.get(&1).and_then(|v| v.home), Some([37.825, -122.44]));
    assert_eq!(f.get(&7).and_then(|v| v.callsign.clone()).as_deref(), Some("SKY 1"));
}

#[tokio::test(start_paused = true)]
async fn the_pointer_lists_the_open_orders() {
    use crate::decision::queue::Pointer;
    let r = rig();
    let dir = std::env::temp_dir().join(format!("murmur-pointer-open-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let ptr = dir.join(".resume-1.json");
    let _ = r.d.pointer.set(ptr.clone());
    let (cell, w) = r.watched(74, Firmware::Copter, "x10", (LAT, LON), quick());
    sleep(Duration::from_secs(1)).await;
    let a = r.d.submit(&op1(), orbit(74)).await.expect("submit");
    assert_eq!(a.state, State::Dispatched);
    r.d.tick();
    let p: Pointer = serde_json::from_slice(&std::fs::read(&ptr).expect("pointer")).expect("json");
    assert_eq!(p.open, vec![a.id.0], "{p:?}");
    go_silent(&cell, &w, 5.0);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- rules 7 ----------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_start_logged_ahead_is_not_lost_to_a_crash_right_after_it() {
    // murmur-dma (rules 7): the copter switched to AUTO (its start went out), the link died
    // before the ACK, and the bridge crashed before its next tick. The start was logged ahead
    // of sending it, so the next run finds that the orbit may have started: when the link is
    // back and the copter (its failsafe took it home meanwhile) does not run it, a human
    // decides; it is never started again on its own. (Logged at the tick, as before rules 7,
    // the crashed log held no start, and the next run restarted the mission over the RTL.)
    let r = rig();
    let q = Quirks {
        cut_after_auto: true,
        bail_after: Some((5.0, "RTL", "Failsafe: GCS")),
        ..quick()
    };
    let (cell, w) = r.watched(87, Firmware::Copter, "x10", (LAT, LON), q);
    sleep(Duration::from_secs(1)).await;
    let out = r.d.submit(&op1(), orbit(87)).await.expect("submit");
    assert_eq!(out.state, State::Dispatched, "{}", r.reason(out.id));
    for _ in 0..2000 {
        if w.cut.load(Ordering::SeqCst) {
            break;
        }
        sleep(Duration::from_millis(5)).await;
    }
    // the crash: no tick since the dispatch, the log as it stands
    let crashed = r.log.text();
    let start = crashed
        .lines()
        .find(|l| l.contains("\"type\":\"op.state\"") && l.contains("\"state\":\"starting\""))
        .unwrap_or_else(|| panic!("the start was not logged before it went out:\n{crashed}"));
    assert!(start.contains("\"ahead\":true"), "{start}");
    // the first bridge's op gives up on the dead link before the link comes back
    sleep(Duration::from_secs(40)).await;
    let (b, blog) = restart(&r);
    let carried = b.carry_over("a.jsonl", &crashed, &CarryCheck::default()).expect("carried");
    assert_eq!(carried, vec![out.id]);
    let state = |id: IntentId| b.lock().engine.intent(id).map(Intent::state);
    assert_eq!(state(out.id), Some(State::Queued));
    w.set_cut(false);
    sleep(Duration::from_secs(2)).await;
    for _ in 0..30 {
        b.tick();
        if state(out.id) == Some(State::PendingApproval) {
            break;
        }
        sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(state(out.id), Some(State::PendingApproval), "a human decides: {:?}", ops_of(&r));
    assert_eq!(cell.read(|v| v.mode.clone()).as_deref(), Some("RTL"), "the failsafe's mode stands");
    assert_eq!(w.count("MAV_CMD_DO_SET_MISSION_CURRENT"), 1, "never started again on its own");
    assert_eq!(w.count("MAV_CMD_DO_SET_MODE"), 1);
    let rb = replay_str_with(
        &blog.text(),
        &ReplayOptions {
            previous: Some(&crashed),
            ..ReplayOptions::default()
        },
    )
    .expect("parses");
    assert!(rb.ok(), "{:?}", rb.mismatches);
}
