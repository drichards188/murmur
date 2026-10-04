//! The task layer against a fake autopilot: a tiny ArduPilot-alike that answers commands,
//! takes missions, changes modes and moves, on tokio's paused clock (minutes run in
//! milliseconds).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::{Instant, sleep, sleep_until};

use super::*;
use crate::clock;
use crate::geo;
use crate::link::{LinkStats, Transmit};
use crate::mav::{self, MavMessage, apm};
use crate::vehicle::VehicleCell;

/// Where the bridge's messages to the fake go: straight in, or through a slow link.
type Deliver = Box<dyn Fn(MavMessage) + Send + Sync>;

struct FakeTx {
    port: u16,
    tx: Deliver,
}

impl Transmit for FakeTx {
    fn send(&self, msg: &MavMessage) {
        (self.tx)(msg.clone());
    }
    fn send_raw(&self, frame: &[u8]) {
        for f in mav::decode(frame) {
            (self.tx)(f.msg);
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
    pub ignore_mode: bool,
    /// metres per second (0: stuck)
    pub speed: f64,
    /// after this long in AUTO, drop to `mode` saying `text`
    pub bail_after: Option<(f64, &'static str, &'static str)>,
    /// one-way delay of the link, both directions (in order, as the emulator delivers)
    pub delay: Duration,
    /// every `lose_every`th message in each direction is lost (0: none)
    pub lose_every: u32,
    /// the upload's final MISSION_ACK is lost (once)
    pub lose_final_ack: bool,
    /// report like an AUV under water: HEARTBEAT + HIGH_LATENCY2 every this many seconds,
    /// nothing streamed
    pub high_latency_s: Option<f64>,
    /// while set, nothing gets through in either direction (a link cut)
    pub cut: Option<Arc<AtomicBool>>,
    /// the first ACK of this command is lost on the way back (once)
    pub lose_ack: Option<apm::MavCmd>,
    /// cut the link the moment the vehicle took the last mission item (before its ACK goes)
    pub cut_after_upload: bool,
    /// what the vehicle received and acted on: command names, "MISSION_COUNT" (an upload
    /// started), "mission accepted"
    pub counts: Option<Arc<Mutex<BTreeMap<String, u32>>>>,
    /// set it and the fake reboots (and clears it): time since boot from 0, disarmed, out of
    /// AUTO, mission gone (as a stand-in restarts)
    pub reboot: Option<Arc<AtomicBool>>,
    /// the mode it starts in (its lost-link failsafe sent it home, say)
    pub start_mode: Option<&'static str>,
}

impl Quirks {
    /// A link cut switch, command counters and a reboot switch, and the quirks with them.
    pub(crate) fn watched(self) -> (Quirks, Watch) {
        let w = Watch::default();
        (
            Quirks {
                cut: Some(w.cut.clone()),
                counts: Some(w.counts.clone()),
                reboot: Some(w.reboot.clone()),
                ..self
            },
            w,
        )
    }
}

/// The handles of a watched fake: cut its link, count what it got, reboot it.
#[derive(Clone, Default)]
pub(crate) struct Watch {
    pub cut: Arc<AtomicBool>,
    pub counts: Arc<Mutex<BTreeMap<String, u32>>>,
    pub reboot: Arc<AtomicBool>,
}

impl Watch {
    pub(crate) fn count(&self, what: &str) -> u32 {
        self.counts
            .lock()
            .map(|c| c.get(what).copied().unwrap_or(0))
            .unwrap_or(0)
    }
    pub(crate) fn set_cut(&self, on: bool) {
        self.cut.store(on, Ordering::SeqCst);
    }
}

/// A one-way slow link: each message arrives `delay` after it was sent, in order; every
/// `lose_every`th is lost.
fn slow_link<T: Send + 'static>(
    delay: Duration,
    lose_every: u32,
    deliver: impl Fn(T) + Send + 'static,
) -> mpsc::UnboundedSender<(Instant, T)> {
    let (tx, mut rx) = mpsc::unbounded_channel::<(Instant, T)>();
    tokio::spawn(async move {
        let mut n = 0u32;
        while let Some((sent, m)) = rx.recv().await {
            sleep_until(sent + delay).await;
            n += 1;
            if lose_every > 0 && n.is_multiple_of(lose_every) {
                continue;
            }
            deliver(m);
        }
    });
    tx
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
    /// ArduPilot's upload state: receiving, last request and last item (monotonic s)
    receiving: bool,
    last_request: f64,
    last_receive: f64,
    /// the final ACK was already lost once (`Quirks::lose_final_ack`)
    lost_ack: bool,
    /// HIGH_LATENCY2 mode: when the next report is due
    next_report: f64,
    /// when it booted (monotonic s): time since boot goes in its telemetry
    booted: f64,
    /// `Quirks::lose_ack` was used
    lost_cmd_ack: bool,
    /// to the bridge over the slow link, when there is one
    out: Option<mpsc::UnboundedSender<(Instant, MavMessage)>>,
}

impl Fake {
    fn is_cut(&self) -> bool {
        self.q.cut.as_ref().is_some_and(|c| c.load(Ordering::SeqCst))
    }

    fn counted(&self, what: &str) {
        if let Some(c) = &self.q.counts
            && let Ok(mut c) = c.lock()
        {
            *c.entry(what.to_string()).or_default() += 1;
        }
    }

    fn boot_ms(&self) -> u32 {
        ((clock::mono() - self.booted) * 1000.0).max(1.0) as u32
    }

    fn emit(&self, msg: MavMessage) {
        if self.is_cut() {
            return;
        }
        match &self.out {
            Some(out) => {
                let _ = out.send((Instant::now(), msg));
            }
            None => deliver(
                &self.cell,
                self.sysid,
                &msg,
                self.q.high_latency_s.is_some(),
            ),
        }
    }

    fn mission_ack(&self, mavtype: apm::MavMissionResult) {
        self.emit(MavMessage::MISSION_ACK(apm::MISSION_ACK_DATA {
            target_system: 255,
            target_component: 190,
            mavtype,
            ..Default::default()
        }));
    }

    fn ack(&mut self, command: apm::MavCmd, result: apm::MavResult) {
        if self.q.lose_ack == Some(command) && !self.lost_cmd_ack {
            self.lost_cmd_ack = true;
            return;
        }
        self.emit(MavMessage::COMMAND_ACK(apm::COMMAND_ACK_DATA {
            command,
            result,
            ..Default::default()
        }));
    }

    #[allow(deprecated)]
    fn on_msg(&mut self, msg: MavMessage) {
        if self.is_cut() {
            return;
        }
        if let MavMessage::COMMAND_LONG(c) = &msg {
            self.counted(&mav::cmd_name(c.command));
        }
        match msg {
            MavMessage::COMMAND_LONG(c) => match c.command {
                apm::MavCmd::MAV_CMD_COMPONENT_ARM_DISARM => {
                    // ArduPilot arms in no return mode (AP_Arming mode checks)
                    if c.param1 > 0.5 && self.mode == self.fw.mode("RTL") {
                        return self.ack(c.command, apm::MavResult::MAV_RESULT_FAILED);
                    }
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
            // ArduPilot's MissionItemProtocol: items only in order (anything else is answered
            // INVALID_SEQUENCE and ignored), ERROR when not receiving, re-requests in `step`
            MavMessage::MISSION_COUNT(m) => {
                self.counted("MISSION_COUNT");
                self.upload_count = m.count;
                self.mission.clear();
                self.receiving = true;
                self.last_receive = clock::mono();
                self.request(0);
            }
            MavMessage::MISSION_ITEM_INT(it) => {
                if !self.receiving {
                    return self.mission_ack(apm::MavMissionResult::MAV_MISSION_ERROR);
                }
                if usize::from(it.seq) != self.mission.len() {
                    return self.mission_ack(apm::MavMissionResult::MAV_MISSION_INVALID_SEQUENCE);
                }
                self.mission.push(it);
                self.last_receive = clock::mono();
                if self.mission.len() == usize::from(self.upload_count) {
                    self.receiving = false;
                    self.counted("mission accepted");
                    if self.q.cut_after_upload
                        && let Some(c) = &self.q.cut
                        && !self.lost_ack
                    {
                        self.lost_ack = true;
                        c.store(true, Ordering::SeqCst);
                        return;
                    }
                    if self.q.lose_final_ack && !self.lost_ack {
                        self.lost_ack = true;
                    } else {
                        self.mission_ack(apm::MavMissionResult::MAV_MISSION_ACCEPTED);
                    }
                } else {
                    self.request(self.mission.len() as u16);
                }
            }
            MavMessage::MISSION_REQUEST_LIST(_) => {
                if self.receiving {
                    return self.mission_ack(apm::MavMissionResult::MAV_MISSION_DENIED);
                }
                self.emit(MavMessage::MISSION_COUNT(apm::MISSION_COUNT_DATA {
                    count: self.mission.len() as u16,
                    target_system: 255,
                    target_component: 190,
                    ..Default::default()
                }));
            }
            MavMessage::MISSION_REQUEST_INT(r) => {
                if self.receiving {
                    return self.mission_ack(apm::MavMissionResult::MAV_MISSION_DENIED);
                }
                match self.mission.get(usize::from(r.seq)) {
                    Some(it) => self.emit(MavMessage::MISSION_ITEM_INT(it.clone())),
                    None => self.mission_ack(apm::MavMissionResult::MAV_MISSION_INVALID_SEQUENCE),
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

    fn request(&mut self, seq: u16) {
        self.last_request = clock::mono();
        self.emit(MavMessage::MISSION_REQUEST_INT(
            apm::MISSION_REQUEST_INT_DATA {
                seq,
                target_system: 255,
                target_component: 190,
                ..Default::default()
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
        if let Some(r) = &self.q.reboot
            && r.swap(false, Ordering::SeqCst)
        {
            // a stand-in restarts: everything it was told is gone
            self.booted = now;
            self.armed = false;
            self.mode = 0;
            self.mission.clear();
            self.seq = 0;
            self.receiving = false;
            self.auto_since = None;
        }
        if self.receiving {
            if now - self.last_receive > 8.0 {
                self.receiving = false;
                self.mission_ack(apm::MavMissionResult::MAV_MISSION_OPERATION_CANCELLED);
            } else if now - self.last_request > 1.0 {
                self.request(self.mission.len() as u16);
            }
        }
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
                ..Default::default()
            }));
        }
        let base = if self.armed {
            apm::MavModeFlag::MAV_MODE_FLAG_SAFETY_ARMED
        } else {
            apm::MavModeFlag::empty()
        };
        let heartbeat = MavMessage::HEARTBEAT(apm::HEARTBEAT_DATA {
            custom_mode: self.mode,
            mavtype: self.fw.mav_type(),
            autopilot: apm::MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA,
            base_mode: base | apm::MavModeFlag::MAV_MODE_FLAG_CUSTOM_MODE_ENABLED,
            system_status: apm::MavState::MAV_STATE_ACTIVE,
            mavlink_version: 3,
        });
        if let Some(every) = self.q.high_latency_s {
            if now >= self.next_report {
                self.next_report = now + every;
                self.emit(heartbeat);
                self.emit(MavMessage::HIGH_LATENCY2(apm::HIGH_LATENCY2_DATA {
                    latitude: (self.lat * 1e7) as i32,
                    longitude: (self.lon * 1e7) as i32,
                    altitude: self.alt_rel as i16,
                    groundspeed: (self.speed_now * 5.0) as u8,
                    wp_num: self.seq,
                    custom_mode: self.mode as u16,
                    mavtype: self.fw.mav_type(),
                    failure_flags: apm::HlFailureFlag::HL_FAILURE_FLAG_GPS,
                    battery: 90,
                    timestamp: self.boot_ms(),
                    ..Default::default()
                }));
            }
            return;
        }
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
                time_boot_ms: self.boot_ms(),
                ..Default::default()
            },
        ));
        self.emit(MavMessage::VFR_HUD(apm::VFR_HUD_DATA {
            groundspeed: self.speed_now as f32,
            ..Default::default()
        }));
        self.emit(MavMessage::MISSION_CURRENT(apm::MISSION_CURRENT_DATA {
            seq: self.seq,
            ..Default::default()
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

fn deliver(cell: &VehicleCell, sysid: u8, msg: &MavMessage, link_stats: bool) {
    if link_stats {
        cell.link_on_message(msg); // as the receive loop does
    }
    cell.handle(msg, &mav::encode_as(msg, sysid, 1, 0), clock::mono());
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
    let slow = !q.delay.is_zero() || q.lose_every > 0;
    let to_fake: Deliver = if slow {
        let link = slow_link(q.delay, q.lose_every, move |m| {
            let _ = tx.send(m);
        });
        Box::new(move |m| {
            let _ = link.send((Instant::now(), m));
        })
    } else {
        Box::new(move |m| {
            let _ = tx.send(m);
        })
    };
    let cell = Arc::new(VehicleCell::new(
        uid,
        sysid,
        Arc::new(FakeTx { port, tx: to_fake }),
        Arc::new(Mutex::new(LinkStats::default())),
        None,
    ));
    let out = slow.then(|| {
        let c = cell.clone();
        slow_link(q.delay, q.lose_every, move |m: MavMessage| {
            deliver(&c, sysid, &m, true)
        })
    });
    let start_mode = q.start_mode.map_or(0, |m| fw.mode(m));
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
        mode: start_mode,
        yaw: 0.0,
        speed_now: 0.0,
        mission: Vec::new(),
        upload_count: 0,
        seq: 0,
        auto_since: None,
        target_pos: None,
        said: None,
        receiving: false,
        last_request: 0.0,
        last_receive: 0.0,
        lost_ack: false,
        next_report: 0.0,
        booted: clock::mono(),
        lost_cmd_ack: false,
        out,
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
    // not on station yet: no circle to report
    assert_eq!(tm.orbit_of(&copter), None);
    let s = settle(&tasking.ops[0], 300).await;
    assert_eq!(states(&s), [Queued, Sent, Acked, Active, Done], "{s:?}");
    assert_eq!(s.detail, "on station");
    // on station: the circle it flies, with its on-station tolerance
    let (c, r) = tm.orbit_of(&copter).expect("orbiting");
    assert_eq!(c, center);
    assert!(r > 200.0 && r <= 280.0, "{r}");
    // out of the mission's mode (a failsafe, a pilot): not trusted to circle
    copter.write(|v| v.mode = Some("RTL".into()));
    assert_eq!(tm.orbit_of(&copter), None);
    copter.write(|v| v.mode = Some("AUTO".into()));
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
    assert_eq!(boat.read(|v| v.mode.clone()).as_deref(), Some("LOITER"));
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
    assert_eq!(t.ops[0].snapshot().detail, "commanding LOITER"); // still trying
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

/// An Iridium-class link: 700 ms each way, one message in eleven lost each way (9%, worse
/// than the profile's 2%).
fn iridium() -> Quirks {
    Quirks {
        delay: Duration::from_millis(700),
        lose_every: 11,
        ..quick()
    }
}

fn patrol(uid: Uid, cell: &Arc<VehicleCell>, route: Vec<LngLat>) -> TaskOrder {
    TaskOrder::new(
        request(
            "patrol",
            &[uid],
            TaskParams {
                route: Some(route),
                ..TaskParams::default()
            },
        ),
        &fleet(&[cell]),
    )
    .unwrap_or_else(|e| panic!("{e}"))
}

#[tokio::test(start_paused = true)]
async fn mission_uploads_over_a_slow_lossy_link() {
    // A surfaced AUV on its mast radio: the vehicle re-requests every second, so with a 1.4 s
    // round trip every item is asked for twice. The old 1 s / 1.5 s timeouts failed this with
    // MAV_MISSION_INVALID_SEQUENCE at item 2; the duplicates are now not answered (in flight)
    // or ignored (INVALID_SEQUENCE), and the measured round trip sets the waits.
    let auv = spawn_fake(20, Firmware::Sub, LAT, LON, iridium());
    auv.set_link_rtt(1.4, 0.15);
    sleep(Duration::from_secs(2)).await;
    let tm = TaskManager::default();
    let route = vec![
        [LON + 0.002, LAT],
        [LON + 0.002, LAT + 0.002],
        [LON, LAT + 0.002],
        [LON - 0.001, LAT + 0.001],
        [LON - 0.001, LAT],
    ];
    let t = tm.dispatch_unchecked(patrol(20, &auv, route.clone()));
    let s = settle(&t.ops[0], 600).await;
    assert_eq!(states(&s), [Queued, Sent, Acked, Active, Done], "{s:?}");
    assert_eq!(s.detail, "first lap complete, patrolling");
    // nothing measured yet (no TIMESYNC answer, no emulator): the floors, and it still works
    let auv2 = spawn_fake(21, Firmware::Sub, LAT, LON, iridium());
    sleep(Duration::from_secs(2)).await;
    let t = tm.dispatch_unchecked(patrol(21, &auv2, route));
    let s = settle(&t.ops[0], 600).await;
    assert_eq!(s.state, Done, "{s:?}");
}

#[tokio::test(start_paused = true)]
async fn a_lost_final_ack_is_settled_by_asking_the_vehicle() {
    let boat = spawn_fake(
        22,
        Firmware::Rover,
        LAT,
        LON,
        Quirks {
            delay: Duration::from_millis(300),
            lose_final_ack: true,
            ..quick()
        },
    );
    boat.set_link_rtt(0.6, 0.05);
    sleep(Duration::from_secs(1)).await;
    let tm = TaskManager::default();
    let t = tm.dispatch_unchecked(patrol(
        22,
        &boat,
        vec![[LON + 0.002, LAT], [LON + 0.002, LAT + 0.002]],
    ));
    let s = settle(&t.ops[0], 300).await;
    // the resent last item met MAV_MISSION_ERROR (no longer receiving), which used to fail
    // the op; asking for the count and the last item showed the mission had arrived
    assert_eq!(states(&s), [Queued, Sent, Acked, Active, Done], "{s:?}");
}

#[tokio::test(start_paused = true)]
async fn commands_wait_for_a_slow_ack() {
    // GEO plus queueing: 1.2 s each way. A 1 s wait would resend every command and give up
    // after five tries in 5 s; the link's timeout waits for the answer.
    let boat = spawn_fake(
        23,
        Firmware::Rover,
        LAT,
        LON,
        Quirks {
            delay: Duration::from_millis(1200),
            ..quick()
        },
    );
    boat.set_link_rtt(2.4, 0.2);
    sleep(Duration::from_secs(3)).await;
    let tm = TaskManager::default();
    let t = tm.dispatch_unchecked(
        TaskOrder::new(
            request("hold", &[23], TaskParams::default()),
            &fleet(&[&boat]),
        )
        .unwrap_or_else(|e| panic!("{e}")),
    );
    let s = settle(&t.ops[0], 60).await;
    assert_eq!(states(&s), [Queued, Sent, Acked, Done], "{s:?}");
    assert_eq!(
        boat.link_timing().ack_timeout(0),
        Duration::from_millis(3450)
    );
}

#[tokio::test(start_paused = true)]
async fn rare_reports_are_not_a_stall_but_a_stall_still_is() {
    // An AUV under water: HEARTBEAT + HIGH_LATENCY2 every 20 s, nothing else. Speed is known
    // only at each report, so a 10 s NOT MOVING would fire between reports of a vehicle doing
    // 2 m/s. It waits 2 x 20 s + the round trip instead.
    let auv = spawn_fake(
        24,
        Firmware::Sub,
        LAT,
        LON,
        Quirks {
            speed: 2.0,
            high_latency_s: Some(20.0),
            ..Quirks::default()
        },
    );
    sleep(Duration::from_secs(25)).await;
    assert_eq!(auv.link_timing().heartbeat_s, 20.0);
    let tm = TaskManager::default();
    let t = tm.dispatch_unchecked(patrol(
        24,
        &auv,
        vec![[LON + 0.01, LAT], [LON + 0.01, LAT + 0.01]],
    ));
    for _ in 0..240 {
        sleep(Duration::from_millis(500)).await;
        let s = t.ops[0].snapshot();
        assert!(!s.detail.starts_with("NOT MOVING"), "{s:?}");
        assert_ne!(s.state, Failed, "{s:?}");
    }
    assert_eq!(t.ops[0].state(), Active);
    // the same AUV, stuck: NOT MOVING, once 40 s have passed without movement
    let stuck = spawn_fake(
        25,
        Firmware::Sub,
        LAT,
        LON,
        Quirks {
            speed: 0.0,
            high_latency_s: Some(20.0),
            ..Quirks::default()
        },
    );
    sleep(Duration::from_secs(25)).await;
    let t = tm.dispatch_unchecked(patrol(
        25,
        &stuck,
        vec![[LON + 0.01, LAT], [LON + 0.01, LAT + 0.01]],
    ));
    let mut said = None;
    for i in 0..400 {
        sleep(Duration::from_millis(500)).await;
        let s = t.ops[0].snapshot();
        if s.detail.starts_with("NOT MOVING") {
            said = Some((i, s.detail));
            break;
        }
    }
    let (i, detail) = said.unwrap_or_else(|| panic!("never NOT MOVING"));
    assert!(detail.starts_with("NOT MOVING for 4"), "{detail}");
    assert!(i > 80, "after {i} half seconds");
}

// ---- exactly once (rules 5, murmur-5p4) ------------------------------------------------------

fn watched(uid: Uid, fw: Firmware, q: Quirks) -> (Arc<VehicleCell>, Watch) {
    let (q, w) = q.watched();
    (spawn_fake(uid, fw, LAT, LON, q), w)
}

fn short_patrol() -> Vec<LngLat> {
    vec![[LON + 0.002, LAT], [LON + 0.002, LAT + 0.002]]
}

#[tokio::test(start_paused = true)]
async fn a_lost_mission_ack_and_lost_command_acks_execute_once() {
    // the final MISSION_ACK lost, and the first ACK of the mode change: the mission is
    // confirmed by asking the vehicle, the mode by its heartbeat; nothing is sent twice
    let (boat, w) = watched(
        30,
        Firmware::Rover,
        Quirks {
            lose_final_ack: true,
            lose_ack: Some(apm::MavCmd::MAV_CMD_DO_SET_MODE),
            ..quick()
        },
    );
    sleep(Duration::from_millis(500)).await;
    let tm = TaskManager::default();
    let t = tm.dispatch_unchecked(patrol(30, &boat, short_patrol()));
    let s = settle(&t.ops[0], 300).await;
    assert_eq!(states(&s), [Queued, Sent, Acked, Active, Done], "{s:?}");
    assert_eq!(w.count("MISSION_COUNT"), 1, "uploaded once");
    assert_eq!(w.count("mission accepted"), 1);
    assert_eq!(w.count("MAV_CMD_DO_SET_MODE"), 1, "the mode change sent once");
    assert_eq!(w.count("MAV_CMD_DO_SET_MISSION_CURRENT"), 1);
    // the lost ACK of the start: confirmed by the vehicle's MISSION_CURRENT, not resent
    let (boat, w) = watched(
        31,
        Firmware::Rover,
        Quirks {
            lose_ack: Some(apm::MavCmd::MAV_CMD_DO_SET_MISSION_CURRENT),
            ..quick()
        },
    );
    sleep(Duration::from_millis(500)).await;
    let t = tm.dispatch_unchecked(patrol(31, &boat, short_patrol()));
    let s = settle(&t.ops[0], 300).await;
    assert_eq!(s.state, Done, "{s:?}");
    assert_eq!(w.count("MAV_CMD_DO_SET_MISSION_CURRENT"), 1, "started once");
    assert_eq!(w.count("MISSION_COUNT"), 1);
}

#[tokio::test(start_paused = true)]
async fn a_link_lost_after_the_upload_is_waited_out_and_the_mission_started_once() {
    // the link drops the moment the vehicle has the whole mission, before its ACK: the op
    // waits for the link (its patience), then asks the vehicle what it holds and only starts it
    let (boat, w) = watched(
        32,
        Firmware::Rover,
        Quirks {
            cut_after_upload: true,
            ..quick()
        },
    );
    sleep(Duration::from_millis(500)).await;
    let tm = TaskManager::default();
    let mut order = patrol(32, &boat, short_patrol());
    order.patience = Duration::from_secs(120);
    let t = tm.dispatch_unchecked(order);
    sleep(Duration::from_secs(25)).await;
    let mid = t.ops[0].snapshot();
    assert_eq!(mid.state, Sent, "{mid:?}");
    assert!(mid.detail.starts_with("link lost"), "{}", mid.detail);
    w.set_cut(false);
    let s = settle(&t.ops[0], 300).await;
    assert_eq!(s.state, Done, "{s:?}");
    assert_eq!(w.count("MISSION_COUNT"), 1, "not uploaded again: it held the mission");
    assert_eq!(w.count("mission accepted"), 1);
    assert_eq!(w.count("MAV_CMD_DO_SET_MISSION_CURRENT"), 1, "started once");
}

#[tokio::test(start_paused = true)]
async fn a_link_lost_mid_upload_restarts_the_transfer_once_it_is_back() {
    let (boat, w) = watched(33, Firmware::Rover, quick());
    sleep(Duration::from_millis(500)).await;
    let tm = TaskManager::default();
    let mut order = patrol(33, &boat, short_patrol());
    order.patience = Duration::from_secs(120);
    // cut before anything gets through: the COUNT is lost
    w.set_cut(true);
    let t = tm.dispatch_unchecked(order);
    sleep(Duration::from_secs(20)).await;
    assert!(t.ops[0].snapshot().detail.starts_with("link lost"), "{:?}", t.ops[0].snapshot());
    assert_eq!(w.count("MISSION_COUNT"), 0);
    w.set_cut(false);
    let s = settle(&t.ops[0], 300).await;
    assert_eq!(s.state, Done, "{s:?}");
    assert_eq!(w.count("mission accepted"), 1);
    assert_eq!(w.count("MAV_CMD_DO_SET_MISSION_CURRENT"), 1);
}

#[tokio::test(start_paused = true)]
async fn a_link_that_never_comes_back_fails_the_op_after_its_patience() {
    let (boat, w) = watched(34, Firmware::Rover, quick());
    sleep(Duration::from_millis(500)).await;
    let tm = TaskManager::default();
    let mut order = patrol(34, &boat, short_patrol());
    order.patience = Duration::from_secs(40);
    w.set_cut(true);
    let t = tm.dispatch_unchecked(order);
    let s = settle(&t.ops[0], 300).await;
    assert_eq!(s.state, Failed, "{s:?}");
    assert!(s.detail.starts_with("link lost") && s.detail.ends_with("not back within 40 s"), "{}", s.detail);
    // with no patience (before rules 5, or a kind the policy never queues): as before
    let (boat, w) = watched(35, Firmware::Rover, quick());
    sleep(Duration::from_millis(500)).await;
    w.set_cut(true);
    let t = tm.dispatch_unchecked(patrol(35, &boat, short_patrol()));
    let s = settle(&t.ops[0], 300).await;
    assert_eq!(s.state, Failed, "{s:?}");
}

#[tokio::test(start_paused = true)]
async fn a_reboot_mid_task_ends_the_op_and_the_order_is_not_resent() {
    let (copter, w) = watched(36, Firmware::Copter, quick());
    sleep(Duration::from_secs(2)).await;
    let tm = TaskManager::default();
    let center = [LON + 0.02, LAT];
    let order = TaskOrder::new(
        request(
            "orbit",
            &[36],
            TaskParams {
                center: Some(center),
                radius: Some(200.0),
                ..TaskParams::default()
            },
        ),
        &fleet(&[&copter]),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let t = tm.dispatch_unchecked(order);
    for _ in 0..200 {
        if t.ops[0].state() == Active {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(t.ops[0].state(), Active);
    w.reboot.store(true, std::sync::atomic::Ordering::SeqCst);
    let s = settle(&t.ops[0], 60).await;
    assert_eq!(s.state, Failed, "{s:?}");
    assert!(s.detail.contains("rebooted"), "{}", s.detail);
    assert_eq!(copter.read(|v| v.boot_epoch), 1);
    assert_eq!(copter.read(|v| v.reboots.len()), 1);
    sleep(Duration::from_secs(10)).await;
    assert_eq!(w.count("MISSION_COUNT"), 1, "not sent again");
    assert_eq!(w.count("MAV_CMD_DO_SET_MISSION_CURRENT"), 1);
}

#[tokio::test(start_paused = true)]
async fn after_a_restart_an_order_the_vehicle_runs_is_followed_not_resent() {
    let (boat, w) = watched(37, Firmware::Rover, quick());
    sleep(Duration::from_millis(500)).await;
    let tm = TaskManager::default();
    let route = vec![[LON + 0.01, LAT], [LON + 0.01, LAT + 0.01]];
    let t = tm.dispatch_unchecked(patrol(37, &boat, route.clone()));
    for _ in 0..200 {
        if t.ops[0].state() == Active {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(t.ops[0].state(), Active);
    // the bridge restarted: a new op for the same order, knowing the vehicle had started it
    let mut again = patrol(37, &boat, route.clone());
    again.resume.insert(37, Resume::Adopt);
    let t2 = tm.dispatch_unchecked(again);
    for _ in 0..300 {
        if t2.ops[0].state() == Active {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    let s = t2.ops[0].snapshot();
    assert_eq!(s.state, Active, "{s:?}");
    assert_eq!(
        s.history.iter().map(|h| h.state).collect::<Vec<_>>(),
        [Queued, Acked, Active],
        "{s:?}"
    );
    assert_eq!(w.count("MISSION_COUNT"), 1, "followed, not uploaded again");
    assert_eq!(w.count("MAV_CMD_DO_SET_MISSION_CURRENT"), 1, "not restarted");
    // the vehicle left it (a hold took it out of AUTO): an adopted order ends, not resent
    let mut third = patrol(37, &boat, route.clone());
    third.resume.insert(37, Resume::Adopt);
    let hold = tm.dispatch_unchecked(
        TaskOrder::new(request("hold", &[37], TaskParams::default()), &fleet(&[&boat]))
            .unwrap_or_else(|e| panic!("{e}")),
    );
    settle(&hold.ops[0], 60).await;
    let t3 = tm.dispatch_unchecked(third);
    let s = settle(&t3.ops[0], 120).await;
    assert_eq!(s.state, Failed, "{s:?}");
    assert!(s.detail.starts_with("after the bridge restart the vehicle no longer runs it"), "{}", s.detail);
    assert_eq!(w.count("MISSION_COUNT"), 1);
    // held but not started (delivered before the restart, the start never sent): only the
    // start goes
    let mut fourth = patrol(37, &boat, route);
    fourth.resume.insert(37, Resume::Verify);
    let t4 = tm.dispatch_unchecked(fourth);
    let s = settle(&t4.ops[0], 1200).await;
    assert_eq!(s.state, Done, "{s:?}");
    assert_eq!(w.count("MISSION_COUNT"), 1, "it held the mission: not uploaded again");
    assert_eq!(w.count("MAV_CMD_DO_SET_MISSION_CURRENT"), 2, "started (once more, it was not running)");
}

#[tokio::test(start_paused = true)]
async fn a_vehicle_its_failsafe_sent_home_is_held_then_armed() {
    // an outage: the boat's lost-link failsafe put it in RTL, where ArduRover won't arm; the
    // order delivered after the outage switches it to its hold mode, arms, and goes
    let (boat, w) = watched(
        38,
        Firmware::Rover,
        Quirks {
            start_mode: Some("RTL"),
            ..quick()
        },
    );
    sleep(Duration::from_millis(500)).await;
    let tm = TaskManager::default();
    let t = tm.dispatch_unchecked(patrol(38, &boat, short_patrol()));
    let s = settle(&t.ops[0], 300).await;
    assert_eq!(s.state, Done, "{s:?}");
    assert_eq!(w.count("MAV_CMD_COMPONENT_ARM_DISARM"), 2, "refused in RTL, then armed");
    assert_eq!(w.count("MISSION_COUNT"), 1);
}
