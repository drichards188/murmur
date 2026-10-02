//! The task layer against a fake autopilot: a tiny ArduPilot-alike that answers commands,
//! takes missions, changes modes and moves, on tokio's paused clock (minutes run in
//! milliseconds).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::sleep;

use super::*;
use crate::clock;
use crate::geo;
use crate::link::{LinkStats, Transmit};
use crate::mav::{self, MavMessage, apm};
use crate::vehicle::VehicleCell;

struct FakeTx {
    port: u16,
    tx: mpsc::UnboundedSender<MavMessage>,
}

impl Transmit for FakeTx {
    fn send(&self, msg: &MavMessage) {
        let _ = self.tx.send(msg.clone());
    }
    fn send_raw(&self, frame: &[u8]) {
        for f in mav::decode(frame) {
            let _ = self.tx.send(f.msg);
        }
    }
    fn port(&self) -> u16 {
        self.port
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Firmware {
    Copter,
    Rover,
    Sub,
    Tracker,
}

impl Firmware {
    fn mav_type(self) -> apm::MavType {
        match self {
            Firmware::Copter => apm::MavType::MAV_TYPE_QUADROTOR,
            Firmware::Rover => apm::MavType::MAV_TYPE_SURFACE_BOAT,
            Firmware::Sub => apm::MavType::MAV_TYPE_SUBMARINE,
            Firmware::Tracker => apm::MavType::MAV_TYPE_ANTENNA_TRACKER,
        }
    }
    fn mode(self, name: &str) -> u32 {
        mav::mode_number(self.mav_type(), name).unwrap_or(u32::MAX)
    }
}

/// How the fake misbehaves.
#[derive(Clone, Default)]
pub(crate) struct Quirks {
    /// never answer DO_SET_MODE
    ignore_mode: bool,
    /// metres per second (0: stuck)
    speed: f64,
    /// after this long in AUTO, drop to `mode` saying `text`
    bail_after: Option<(f64, &'static str, &'static str)>,
}

struct Fake {
    fw: Firmware,
    sysid: u8,
    cell: Arc<VehicleCell>,
    q: Quirks,
    lat: f64,
    lon: f64,
    alt_rel: f64,
    home: (f64, f64),
    armed: bool,
    mode: u32,
    yaw: f64,
    speed_now: f64,
    mission: Vec<apm::MISSION_ITEM_INT_DATA>,
    upload_count: u16,
    seq: u16,
    auto_since: Option<f64>,
    target_pos: Option<(f64, f64)>,
    /// set when the fake reports a mode change it made itself
    said: Option<String>,
}

impl Fake {
    fn emit(&self, msg: MavMessage) {
        self.cell
            .handle(&msg, &mav::encode_as(&msg, self.sysid, 1, 0), clock::mono());
    }

    fn ack(&self, command: apm::MavCmd, result: apm::MavResult) {
        self.emit(MavMessage::COMMAND_ACK(apm::COMMAND_ACK_DATA {
            command,
            result,
        }));
    }

    #[allow(deprecated)]
    fn on_msg(&mut self, msg: MavMessage) {
        match msg {
            MavMessage::COMMAND_LONG(c) => match c.command {
                apm::MavCmd::MAV_CMD_COMPONENT_ARM_DISARM => {
                    self.armed = c.param1 > 0.5;
                    self.ack(c.command, apm::MavResult::MAV_RESULT_ACCEPTED);
                }
                apm::MavCmd::MAV_CMD_DO_SET_MODE => {
                    if !self.q.ignore_mode {
                        self.mode = c.param2 as u32;
                        self.auto_since = None;
                        self.ack(c.command, apm::MavResult::MAV_RESULT_ACCEPTED);
                    }
                }
                apm::MavCmd::MAV_CMD_DO_SET_MISSION_CURRENT => {
                    self.seq = c.param1 as u16;
                    self.ack(c.command, apm::MavResult::MAV_RESULT_ACCEPTED);
                }
                _ => self.ack(c.command, apm::MavResult::MAV_RESULT_ACCEPTED),
            },
            MavMessage::MISSION_COUNT(m) => {
                self.upload_count = m.count;
                self.mission.clear();
                self.request(0);
            }
            MavMessage::MISSION_ITEM_INT(it) => {
                if usize::from(it.seq) == self.mission.len() {
                    self.mission.push(it);
                }
                if self.mission.len() == usize::from(self.upload_count) {
                    self.emit(MavMessage::MISSION_ACK(apm::MISSION_ACK_DATA {
                        target_system: 255,
                        target_component: 190,
                        mavtype: apm::MavMissionResult::MAV_MISSION_ACCEPTED,
                    }));
                } else {
                    self.request(self.mission.len() as u16);
                }
            }
            MavMessage::PARAM_SET(p) => self.emit(MavMessage::PARAM_VALUE(apm::PARAM_VALUE_DATA {
                param_value: p.param_value,
                param_count: 1,
                param_index: 0,
                param_id: p.param_id,
                param_type: p.param_type,
            })),
            // a relayed target position (antenna tracker)
            MavMessage::GLOBAL_POSITION_INT(g) => {
                self.target_pos = Some((f64::from(g.lat) / 1e7, f64::from(g.lon) / 1e7));
            }
            _ => {}
        }
    }

    fn request(&self, seq: u16) {
        self.emit(MavMessage::MISSION_REQUEST_INT(
            apm::MISSION_REQUEST_INT_DATA {
                seq,
                target_system: 255,
                target_component: 190,
            },
        ));
    }

    fn move_toward(&mut self, lat: f64, lon: f64, dt: f64) -> bool {
        let d = geo::haversine_m(self.lat, self.lon, lat, lon);
        let step = self.q.speed * dt;
        self.speed_now = self.q.speed;
        if d <= step.max(2.0) {
            if self.q.speed > 0.0 {
                (self.lat, self.lon) = (lat, lon);
            }
            return self.q.speed > 0.0 || d < 2.0;
        }
        let f = step / d;
        self.lat += (lat - self.lat) * f;
        self.lon += (lon - self.lon) * f;
        false
    }

    fn step(&mut self, dt: f64) {
        self.speed_now = 0.0;
        let now = clock::mono();
        let auto = self.mode == self.fw.mode("AUTO");
        if auto && self.armed {
            let since = *self.auto_since.get_or_insert(now);
            if let Some((after, mode, text)) = self.q.bail_after
                && now - since > after
            {
                self.mode = self.fw.mode(mode);
                self.said = Some(text.into());
            }
        }
        if auto && self.armed && matches!(self.fw, Firmware::Tracker) {
            if let Some((la, lo)) = self.target_pos {
                self.yaw = geo::bearing_deg(self.lat, self.lon, la, lo);
            }
        } else if auto && self.armed {
            if let Some(it) = self.mission.get(usize::from(self.seq)).cloned() {
                let (lat, lon) = (f64::from(it.x) / 1e7, f64::from(it.y) / 1e7);
                match it.command {
                    apm::MavCmd::MAV_CMD_NAV_TAKEOFF => {
                        self.alt_rel = f64::from(it.z);
                        self.seq += 1;
                    }
                    apm::MavCmd::MAV_CMD_DO_JUMP => self.seq = it.param1 as u16,
                    _ => {
                        if self.move_toward(lat, lon, dt)
                            && usize::from(self.seq) + 1 < self.mission.len()
                        {
                            self.seq += 1;
                        }
                    }
                }
            }
        } else if self.mode == self.fw.mode("RTL") {
            let (la, lo) = self.home;
            self.move_toward(la, lo, dt);
        }
        if let Some(text) = self.said.take() {
            self.emit(MavMessage::STATUSTEXT(apm::STATUSTEXT_DATA {
                severity: apm::MavSeverity::MAV_SEVERITY_WARNING,
                text: mav::char_array(&text),
            }));
        }
        let base = if self.armed {
            apm::MavModeFlag::MAV_MODE_FLAG_SAFETY_ARMED
        } else {
            apm::MavModeFlag::empty()
        };
        self.emit(MavMessage::HEARTBEAT(apm::HEARTBEAT_DATA {
            custom_mode: self.mode,
            mavtype: self.fw.mav_type(),
            autopilot: apm::MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA,
            base_mode: base | apm::MavModeFlag::MAV_MODE_FLAG_CUSTOM_MODE_ENABLED,
            system_status: apm::MavState::MAV_STATE_ACTIVE,
            mavlink_version: 3,
        }));
        self.emit(MavMessage::GLOBAL_POSITION_INT(
            apm::GLOBAL_POSITION_INT_DATA {
                lat: (self.lat * 1e7) as i32,
                lon: (self.lon * 1e7) as i32,
                alt: (self.alt_rel * 1000.0) as i32,
                relative_alt: (self.alt_rel * 1000.0) as i32,
                hdg: 0,
                ..Default::default()
            },
        ));
        self.emit(MavMessage::VFR_HUD(apm::VFR_HUD_DATA {
            groundspeed: self.speed_now as f32,
            ..Default::default()
        }));
        self.emit(MavMessage::MISSION_CURRENT(apm::MISSION_CURRENT_DATA {
            seq: self.seq,
        }));
        self.emit(MavMessage::ATTITUDE(apm::ATTITUDE_DATA {
            yaw: self.yaw.to_radians() as f32,
            ..Default::default()
        }));
        self.emit(MavMessage::GPS_RAW_INT(apm::GPS_RAW_INT_DATA {
            fix_type: apm::GpsFixType::GPS_FIX_TYPE_3D_FIX,
            satellites_visible: 12,
            ..Default::default()
        }));
        self.emit(MavMessage::EKF_STATUS_REPORT(apm::EKF_STATUS_REPORT_DATA {
            flags: apm::EkfStatusFlags::EKF_POS_HORIZ_ABS,
            ..Default::default()
        }));
        self.emit(MavMessage::SYS_STATUS(apm::SYS_STATUS_DATA {
            battery_remaining: 90,
            ..Default::default()
        }));
    }
}

impl TaskManager {
    /// The task layer on its own, without the decision engine in front (these tests are
    /// about the MAVLink side; `crate::decision::tests` covers the whole path).
    fn dispatch_unchecked(&self, order: TaskOrder) -> Arc<Tasking> {
        self.dispatch(AuthorizedOrder::unchecked(order))
    }
}

/// A fake vehicle at (lat, lon), running until the test ends.
pub(crate) fn spawn_fake(
    uid: Uid,
    fw: Firmware,
    lat: f64,
    lon: f64,
    q: Quirks,
) -> Arc<VehicleCell> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let port = 20000 + uid as u16;
    let sysid = (uid % 250 + 1) as u8;
    let cell = Arc::new(VehicleCell::new(
        uid,
        sysid,
        Arc::new(FakeTx { port, tx }),
        Arc::new(Mutex::new(LinkStats::default())),
        None,
    ));
    let mut fake = Fake {
        fw,
        sysid,
        cell: cell.clone(),
        q,
        lat,
        lon,
        alt_rel: 0.0,
        home: (lat, lon),
        armed: false,
        mode: 0,
        yaw: 0.0,
        speed_now: 0.0,
        mission: Vec::new(),
        upload_count: 0,
        seq: 0,
        auto_since: None,
        target_pos: None,
        said: None,
    };
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        loop {
            tokio::select! {
                Some(m) = rx.recv() => fake.on_msg(m),
                _ = tick.tick() => fake.step(0.2),
            }
        }
    });
    cell
}

pub(crate) fn quick() -> Quirks {
    Quirks {
        speed: 20.0,
        ..Quirks::default()
    }
}

fn fleet(cells: &[&Arc<VehicleCell>]) -> BTreeMap<Uid, Arc<VehicleCell>> {
    cells.iter().map(|c| (c.uid, (*c).clone())).collect()
}

fn request(task: &str, uids: &[Uid], params: TaskParams) -> TaskRequest {
    TaskRequest {
        task: task.into(),
        uids: uids.to_vec(),
        params,
    }
}

pub(crate) async fn settle(op: &OpCell, limit_s: u64) -> OpSnapshot {
    for _ in 0..limit_s * 10 {
        if op.finished() {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    op.snapshot()
}

fn states(s: &OpSnapshot) -> Vec<OpState> {
    s.history.iter().map(|h| h.state).collect()
}

const LAT: f64 = 37.80;
const LON: f64 = -122.45;
use OpState::*;

#[tokio::test(start_paused = true)]
async fn copter_orbit_walks_the_whole_lifecycle() {
    let copter = spawn_fake(1, Firmware::Copter, LAT, LON, quick());
    sleep(Duration::from_millis(500)).await;
    let tm = TaskManager::default();
    let center = [LON + 0.004, LAT];
    let order = TaskOrder::new(
        request(
            "orbit",
            &[1],
            TaskParams {
                center: Some(center),
                radius: Some(200.0),
                ..TaskParams::default()
            },
        ),
        &fleet(&[&copter]),
    );
    let tasking = tm.dispatch_unchecked(order.unwrap_or_else(|e| panic!("{e}")));
    let s = settle(&tasking.ops[0], 300).await;
    assert_eq!(states(&s), [Queued, Sent, Acked, Active, Done], "{s:?}");
    assert_eq!(s.detail, "on station");
    assert_eq!(s.progress.as_deref(), Some("orbiting"));
    // the copter took off and is on the ring
    let d = copter
        .read(|v| v.distance_to(LAT, center[0]))
        .unwrap_or(0.0);
    assert!((d - 200.0).abs() < 60.0, "{d}");
    assert!(copter.read(|v| v.airborne()));
    let snap = &tm.snapshot()[0];
    assert_eq!(
        (snap.task, snap.params.radius),
        (TaskKind::Orbit, Some(200.0))
    );
}

#[tokio::test(start_paused = true)]
async fn boat_patrol_completes_a_lap() {
    let boat = spawn_fake(2, Firmware::Rover, LAT, LON, quick());
    sleep(Duration::from_millis(500)).await;
    let tm = TaskManager::default();
    let route = vec![[LON + 0.002, LAT], [LON + 0.002, LAT + 0.002]];
    let order = TaskOrder::new(
        request(
            "patrol",
            &[2],
            TaskParams {
                route: Some(route),
                ..TaskParams::default()
            },
        ),
        &fleet(&[&boat]),
    );
    let tasking = tm.dispatch_unchecked(order.unwrap_or_else(|e| panic!("{e}")));
    let s = settle(&tasking.ops[0], 300).await;
    assert_eq!(states(&s), [Queued, Sent, Acked, Active, Done], "{s:?}");
    assert_eq!(s.detail, "first lap complete, patrolling");
    assert_eq!(s.progress.as_deref(), Some("patrolling"));
}

#[tokio::test(start_paused = true)]
async fn hold_and_rtl() {
    let boat = spawn_fake(3, Firmware::Rover, LAT, LON, quick());
    sleep(Duration::from_millis(500)).await;
    let tm = TaskManager::default();
    let fl = fleet(&[&boat]);
    let hold = tm.dispatch_unchecked(
        TaskOrder::new(request("hold", &[3], TaskParams::default()), &fl)
            .unwrap_or_else(|e| panic!("{e}")),
    );
    let s = settle(&hold.ops[0], 60).await;
    assert_eq!(states(&s), [Queued, Sent, Acked, Done], "{s:?}");
    assert_eq!(s.detail, "holding");
    assert_eq!(boat.read(|v| v.mode.clone()).as_deref(), Some("HOLD"));
    // RTL from exactly home: counts as home at once
    let rtl = tm.dispatch_unchecked(
        TaskOrder::new(request("rtl", &[3], TaskParams::default()), &fl)
            .unwrap_or_else(|e| panic!("{e}")),
    );
    let s = settle(&rtl.ops[0], 60).await;
    assert_eq!(states(&s), [Queued, Sent, Acked, Active, Done], "{s:?}");
    assert_eq!(s.detail, "home");
}

#[tokio::test(start_paused = true)]
async fn sub_rtl_is_a_mission_home() {
    let sub = spawn_fake(4, Firmware::Sub, LAT, LON, quick());
    sleep(Duration::from_millis(500)).await;
    sub.write(|v| v.home = Some((LAT + 0.002, LON))); // away from home
    let tm = TaskManager::default();
    let rtl = tm.dispatch_unchecked(
        TaskOrder::new(request("rtl", &[4], TaskParams::default()), &fleet(&[&sub]))
            .unwrap_or_else(|e| panic!("{e}")),
    );
    let s = settle(&rtl.ops[0], 120).await;
    assert_eq!(states(&s), [Queued, Sent, Acked, Active, Done], "{s:?}");
    assert_eq!(s.detail, "home");
    assert_eq!(sub.read(|v| v.mode.clone()).as_deref(), Some("AUTO"));
}

#[tokio::test(start_paused = true)]
async fn leaving_the_task_fails_the_op_with_the_autopilots_words() {
    let q = Quirks {
        bail_after: Some((2.0, "RTL", "Failsafe: battery")),
        ..quick()
    };
    let copter = spawn_fake(5, Firmware::Copter, LAT, LON, q);
    sleep(Duration::from_millis(500)).await;
    let tm = TaskManager::default();
    let params = TaskParams {
        center: Some([LON + 0.05, LAT]),
        radius: Some(300.0),
        ..TaskParams::default()
    };
    let t = tm.dispatch_unchecked(
        TaskOrder::new(request("orbit", &[5], params), &fleet(&[&copter]))
            .unwrap_or_else(|e| panic!("{e}")),
    );
    let s = settle(&t.ops[0], 120).await;
    assert_eq!(states(&s), [Queued, Sent, Acked, Active, Failed], "{s:?}");
    assert_eq!(
        s.detail,
        "vehicle left the task, now in RTL: Failsafe: battery"
    );
}

#[tokio::test(start_paused = true)]
async fn a_stuck_vehicle_says_not_moving() {
    let boat = spawn_fake(
        6,
        Firmware::Rover,
        LAT,
        LON,
        Quirks {
            speed: 0.0,
            ..Quirks::default()
        },
    );
    sleep(Duration::from_millis(500)).await;
    let tm = TaskManager::default();
    let route = vec![[LON + 0.01, LAT], [LON + 0.01, LAT + 0.01]];
    let t = tm.dispatch_unchecked(
        TaskOrder::new(
            request(
                "patrol",
                &[6],
                TaskParams {
                    route: Some(route),
                    ..TaskParams::default()
                },
            ),
            &fleet(&[&boat]),
        )
        .unwrap_or_else(|e| panic!("{e}")),
    );
    sleep(Duration::from_secs(20)).await;
    let s = t.ops[0].snapshot();
    assert_eq!(s.state, Active, "{s:?}");
    assert!(s.detail.starts_with("NOT MOVING for 1"), "{}", s.detail);
    assert!(
        s.detail.ends_with("(aground or stuck?) · waypoint 1/2"),
        "{}",
        s.detail
    );
}

#[tokio::test(start_paused = true)]
async fn a_new_task_supersedes_the_running_one() {
    let boat = spawn_fake(7, Firmware::Rover, LAT, LON, quick());
    sleep(Duration::from_millis(500)).await;
    let tm = TaskManager::default();
    let fl = fleet(&[&boat]);
    let route = vec![[LON + 0.05, LAT], [LON + 0.05, LAT + 0.05]];
    let first = tm.dispatch_unchecked(
        TaskOrder::new(
            request(
                "patrol",
                &[7],
                TaskParams {
                    route: Some(route),
                    ..TaskParams::default()
                },
            ),
            &fl,
        )
        .unwrap_or_else(|e| panic!("{e}")),
    );
    sleep(Duration::from_secs(10)).await;
    assert_eq!(first.ops[0].state(), Active);
    let second = tm.dispatch_unchecked(
        TaskOrder::new(request("hold", &[7], TaskParams::default()), &fl)
            .unwrap_or_else(|e| panic!("{e}")),
    );
    let s2 = settle(&second.ops[0], 60).await;
    let s1 = first.ops[0].snapshot();
    assert_eq!(
        (s1.state, s1.detail.as_str()),
        (Superseded, "replaced by hold #2")
    );
    assert_eq!(s2.state, Done);
    // the superseded op's executor is gone: its record never changes again
    sleep(Duration::from_secs(30)).await;
    assert_eq!(first.ops[0].snapshot().history.len(), s1.history.len());
    let ids: Vec<u64> = tm.snapshot().iter().map(|t| t.id).collect();
    assert_eq!(ids, [2, 1]);
}

#[tokio::test(start_paused = true)]
async fn incapable_vehicles_fail_the_rest_go() {
    let tracker = spawn_fake(8, Firmware::Tracker, LAT, LON, quick());
    let boat = spawn_fake(9, Firmware::Rover, LAT, LON + 0.001, quick());
    sleep(Duration::from_millis(500)).await;
    let tm = TaskManager::default();
    let t = tm.dispatch_unchecked(
        TaskOrder::new(
            request("hold", &[8, 9], TaskParams::default()),
            &fleet(&[&tracker, &boat]),
        )
        .unwrap_or_else(|e| panic!("{e}")),
    );
    let s = settle(&t.ops[0], 60).await;
    assert_eq!((s.state, s.detail.as_str()), (Done, "holding")); // a tracker holds in STOP
    let o = TaskOrder::new(
        request(
            "orbit",
            &[8, 9],
            TaskParams {
                center: Some([LON, LAT + 0.003]),
                ..TaskParams::default()
            },
        ),
        &fleet(&[&tracker, &boat]),
    );
    let t = tm.dispatch_unchecked(o.unwrap_or_else(|e| panic!("{e}")));
    let s = t.ops[0].snapshot();
    assert_eq!(
        (s.state, s.detail.as_str()),
        (Failed, "a tracker can't orbit")
    );
    assert_eq!(settle(&t.ops[1], 300).await.state, Done);
}

#[tokio::test(start_paused = true)]
async fn no_ack_fails_after_retries() {
    let boat = spawn_fake(
        10,
        Firmware::Rover,
        LAT,
        LON,
        Quirks {
            ignore_mode: true,
            ..quick()
        },
    );
    sleep(Duration::from_millis(500)).await;
    let tm = TaskManager::default();
    let t = tm.dispatch_unchecked(
        TaskOrder::new(
            request("hold", &[10], TaskParams::default()),
            &fleet(&[&boat]),
        )
        .unwrap_or_else(|e| panic!("{e}")),
    );
    sleep(Duration::from_secs(3)).await;
    assert_eq!(t.ops[0].snapshot().detail, "commanding HOLD"); // still trying
    let s = settle(&t.ops[0], 60).await;
    assert_eq!(
        (s.state, s.detail.as_str()),
        (Failed, "no ACK for MAV_CMD_DO_SET_MODE after 5 tries")
    );
}

#[tokio::test(start_paused = true)]
async fn tracker_locks_on_its_target() {
    let tracker = spawn_fake(11, Firmware::Tracker, LAT, LON, quick());
    let boat = spawn_fake(12, Firmware::Rover, LAT + 0.003, LON + 0.003, quick());
    sleep(Duration::from_millis(500)).await;
    let tm = TaskManager::default();
    let fl = fleet(&[&tracker, &boat]);
    let t = tm.dispatch_unchecked(
        TaskOrder::new(
            request(
                "track",
                &[11],
                TaskParams {
                    target: Some(12),
                    ..TaskParams::default()
                },
            ),
            &fl,
        )
        .unwrap_or_else(|e| panic!("{e}")),
    );
    let s = settle(&t.ops[0], 60).await;
    assert_eq!(states(&s), [Queued, Sent, Acked, Active, Done], "{s:?}");
    assert_eq!(
        (s.detail.as_str(), s.progress.as_deref()),
        ("locked on", Some("tracking"))
    );
    assert_eq!(tracker.read(|v| v.tracking), Some(12));
    assert!(boat.read(|v| v.forward_to.contains_key(&tracker.tx.port())));
    // pointing a vehicle at itself is refused
    let t = tm.dispatch_unchecked(
        TaskOrder::new(
            request(
                "track",
                &[11],
                TaskParams {
                    target: Some(11),
                    ..TaskParams::default()
                },
            ),
            &fl,
        )
        .unwrap_or_else(|e| panic!("{e}")),
    );
    let s = settle(&t.ops[0], 10).await;
    assert_eq!(
        (s.state, s.detail.as_str()),
        (Failed, "pick one other vehicle to track")
    );
}

#[tokio::test(start_paused = true)]
async fn orders_are_checked_before_dispatch() {
    let boat = spawn_fake(13, Firmware::Rover, LAT, LON, quick());
    let fl = fleet(&[&boat]);
    let err = |r| TaskOrder::new(r, &fl).err().map(|e| e.to_string());
    let p = TaskParams::default;
    assert_eq!(
        err(request("hold", &[13, 99, 98], p())),
        Some("unknown vehicles [99, 98]".into())
    );
    assert!(matches!(
        TaskOrder::new(request("hold", &[99], p()), &fl),
        Err(OrderError::UnknownVehicles(_))
    ));
    assert_eq!(
        err(request("hold", &[], p())),
        Some("select at least one vehicle".into())
    );
    assert_eq!(
        err(request("dance", &[13], p())),
        Some("unknown task dance".into())
    );
    assert_eq!(
        err(request("orbit", &[13], p())),
        Some("orbit needs center [lon, lat]".into())
    );
    let one = TaskParams {
        route: Some(vec![[LON, LAT]]),
        ..p()
    };
    assert_eq!(
        err(request("patrol", &[13], one)),
        Some("patrol needs a route of at least 2 points".into())
    );
    assert_eq!(
        err(request(
            "track",
            &[13],
            TaskParams {
                target: Some(5),
                ..p()
            }
        )),
        Some("track needs a target vehicle".into())
    );
    assert!(TaskOrder::new(request("rtl", &[13], p()), &fl).is_ok());
}

#[test]
fn request_json_matches_the_console() {
    let r: TaskRequest = serde_json::from_str(
        r#"{"task":"orbit","uids":[3,251],"center":[-122.45,37.8],"radius":250}"#,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(r.uids, [3, 251]);
    assert_eq!(r.params.radius, Some(250.0));
    // params echo back with only what was given, floats as floats
    assert_eq!(
        serde_json::to_string(&r.params).ok().as_deref(),
        Some(r#"{"center":[-122.45,37.8],"radius":250.0}"#)
    );
}
