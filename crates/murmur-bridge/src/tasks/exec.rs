//! Running one op: the MAVLink exchanges (commands with retries, mission upload, arming, mode
//! changes) and the telemetry judgements (active, done, left the task, not moving).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast::{self, error::RecvError};
use tokio::time::{Instant, sleep, timeout_at};

use super::caps::{Caps, OrbitStyle, caps, takeoff_alt};
use super::op::{OpCell, OpState};
use super::{TaskKind, TaskParams};
use crate::clock::{self, round_int};
use crate::geo;
use crate::mav::{self, Kind, MavMessage, apm};
use crate::vehicle::{Vehicle, VehicleCell};

const ACK_TIMEOUT: Duration = Duration::from_secs(1);
const ACK_RETRIES: u8 = 4;
const MISSION_TIMEOUT: Duration = Duration::from_millis(1500);
const MISSION_RETRIES: u32 = 5;
const READY_TIMEOUT: Duration = Duration::from_secs(120);
const ARM_PATIENCE: Duration = Duration::from_secs(30);
const MODE_PATIENCE: Duration = Duration::from_secs(20);
const RETRY_PAUSE: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(500);
/// A vehicle may be out of its task's mode this long (a transient) before the op fails.
const LEFT_MODE_GRACE_S: f64 = 3.0;
const STALL_AFTER_S: f64 = 10.0;
const STALL_MIN_SPEED: f64 = 0.3;

pub struct OpFailed(pub String);

type Result<T = ()> = std::result::Result<T, OpFailed>;

fn fail<T>(s: impl Into<String>) -> Result<T> {
    Err(OpFailed(s.into()))
}

pub struct OpCtx {
    pub op: Arc<OpCell>,
    pub v: Arc<VehicleCell>,
    pub kind: TaskKind,
    pub params: TaskParams,
    /// how many vehicles of this one's kind the tasking has (orbit phase spreading)
    pub same_kind: usize,
    pub target: Option<Arc<VehicleCell>>,
    pub fleet: Vec<Arc<VehicleCell>>,
}

/// Start the op. A panic inside it fails the op instead of leaving it hanging.
pub fn spawn(ctx: OpCtx) {
    let op = ctx.op.clone();
    let handle = tokio::spawn(run(ctx));
    *op.abort.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle.abort_handle());
    tokio::spawn(async move {
        if let Err(e) = handle.await
            && e.is_panic()
        {
            op.set(OpState::Failed, format!("internal error: {e}"));
        }
    });
}

async fn run(ctx: OpCtx) {
    if let Err(OpFailed(why)) = run_inner(&ctx).await {
        ctx.op.set(OpState::Failed, why);
    }
}

async fn run_inner(ctx: &OpCtx) -> Result {
    if !ctx.v.read(|s| s.has_position(clock::mono())) {
        let deadline = Instant::now() + READY_TIMEOUT;
        while !ctx.v.read(|s| s.has_position(clock::mono())) {
            if Instant::now() > deadline {
                return fail("vehicle never got a position fix");
            }
            sleep(POLL).await;
        }
    }
    let kind = ctx.v.read(|s| s.kind).unwrap_or(Kind::Unknown);
    let Some(caps) = caps(kind) else {
        return fail(format!("a {} can't {}", kind.name(), ctx.kind.name()));
    };
    let t = Task { ctx, caps, kind };
    match ctx.kind {
        TaskKind::Orbit => t.orbit().await,
        TaskKind::Patrol => t.patrol().await,
        TaskKind::Rtl => t.rtl().await,
        TaskKind::Hold => t.hold().await,
        TaskKind::Track => t.track().await,
    }
}

/// Next message from the vehicle matching `pred`, or None on timeout.
async fn recv_until(
    rx: &mut broadcast::Receiver<Arc<MavMessage>>,
    timeout: Duration,
    pred: impl Fn(&MavMessage) -> bool,
) -> Option<Arc<MavMessage>> {
    let deadline = Instant::now() + timeout;
    loop {
        match timeout_at(deadline, rx.recv()).await {
            Err(_) | Ok(Err(RecvError::Closed)) => return None,
            Ok(Err(RecvError::Lagged(_))) => {}
            Ok(Ok(m)) if pred(&m) => return Some(m),
            Ok(Ok(_)) => {}
        }
    }
}

#[derive(Debug, Clone)]
struct Item {
    command: apm::MavCmd,
    frame: apm::MavFrame,
    lat: f64,
    lon: f64,
    alt: f64,
    p: [f32; 4],
}

impl Item {
    // the _INT frame is what ArduPilot (and the Python bridge) use in MISSION_ITEM_INT
    #[allow(deprecated)]
    fn nav(command: apm::MavCmd, lat: f64, lon: f64, alt: f64) -> Item {
        Item {
            command,
            frame: apm::MavFrame::MAV_FRAME_GLOBAL_RELATIVE_ALT_INT,
            lat,
            lon,
            alt,
            p: [0.0; 4],
        }
    }

    fn waypoint(lat: f64, lon: f64, alt: f64) -> Item {
        Item::nav(apm::MavCmd::MAV_CMD_NAV_WAYPOINT, lat, lon, alt)
    }

    fn encode(&self, seq: u16, target: u8) -> MavMessage {
        MavMessage::MISSION_ITEM_INT(apm::MISSION_ITEM_INT_DATA {
            param1: self.p[0],
            param2: self.p[1],
            param3: self.p[2],
            param4: self.p[3],
            x: (self.lat * 1e7) as i32,
            y: (self.lon * 1e7) as i32,
            z: self.alt as f32,
            seq,
            command: self.command,
            target_system: target,
            target_component: mav::AUTOPILOT_COMPID,
            frame: self.frame,
            current: 0,
            autocontinue: 1,
            ..Default::default()
        })
    }
}

/// "NOT MOVING" detection: if a vehicle that should be moving shows no ground speed for a
/// while, say so. The autopilot can be in AUTO and still not going anywhere (aground, fouled,
/// stuck): that's what the operator needs to see.
struct StallWatch {
    moving_at: f64,
}

impl StallWatch {
    fn new() -> StallWatch {
        StallWatch {
            moving_at: clock::mono(),
        }
    }

    fn detail(&mut self, v: &Vehicle, base: String) -> String {
        let now = clock::mono();
        if v.groundspeed.unwrap_or(0.0) >= STALL_MIN_SPEED {
            self.moving_at = now;
        }
        let stalled = now - self.moving_at;
        if stalled >= STALL_AFTER_S {
            format!("NOT MOVING for {stalled:.0}s (aground or stuck?) · {base}")
        } else {
            base
        }
    }
}

/// "Left the task" detection: fail if the vehicle leaves `mode` (failsafe, pilot, another GCS)
/// for longer than a grace period, quoting what the autopilot last said.
struct ModeGuard {
    mode: u32,
    left_at: Option<f64>,
}

impl ModeGuard {
    fn check(&mut self, v: &Vehicle) -> Option<String> {
        if v.custom_mode == Some(self.mode) {
            self.left_at = None;
            return None;
        }
        let now = clock::mono();
        let left_at = *self.left_at.get_or_insert(now);
        if now - left_at < LEFT_MODE_GRACE_S {
            return None;
        }
        let said = v
            .last_statustext()
            .map(|t| format!(": {t}"))
            .unwrap_or_default();
        Some(format!(
            "vehicle left the task, now in {}{said}",
            v.mode.as_deref().unwrap_or("None")
        ))
    }
}

struct Task<'a> {
    ctx: &'a OpCtx,
    caps: &'static Caps,
    kind: Kind,
}

impl Task<'_> {
    fn v(&self) -> &VehicleCell {
        &self.ctx.v
    }

    fn op(&self) -> &OpCell {
        &self.ctx.op
    }

    // ---- MAVLink building blocks ----------------------------------------------------------

    /// COMMAND_LONG with retries. The `confirmation` field increments on each retry so the
    /// autopilot can tell a retransmission from a new command.
    async fn command_long(&self, command: apm::MavCmd, params: &[f32]) -> Result {
        let mut p = [0.0f32; 7];
        p[..params.len()].copy_from_slice(params);
        let mut rx = self.v().subscribe();
        for confirmation in 0..=ACK_RETRIES {
            self.v()
                .send(&mav::command_long(self.v().sysid, command, confirmation, p));
            let ack = recv_until(
                &mut rx,
                ACK_TIMEOUT,
                |m| matches!(m, MavMessage::COMMAND_ACK(a) if a.command == command),
            )
            .await;
            let Some(ack) = ack else { continue };
            let MavMessage::COMMAND_ACK(ack) = &*ack else {
                continue;
            };
            match ack.result {
                apm::MavResult::MAV_RESULT_ACCEPTED => return Ok(()),
                apm::MavResult::MAV_RESULT_IN_PROGRESS => continue,
                r => return fail(format!("{} rejected: {r:?}", mav::cmd_name(command))),
            }
        }
        fail(format!(
            "no ACK for {} after {} tries",
            mav::cmd_name(command),
            ACK_RETRIES + 1
        ))
    }

    /// MAVLink mission protocol: COUNT -> (REQUEST_INT seq -> ITEM_INT seq)* -> ACK. The
    /// vehicle drives the transfer, so a lost item just gets requested again; on silence we
    /// resend whatever we sent last. Older firmware asks with the deprecated MISSION_REQUEST.
    #[allow(deprecated)]
    async fn upload_mission(&self, items: &[Item]) -> Result {
        let sysid = self.v().sysid;
        let count = items.len() as u16;
        let mut rx = self.v().subscribe();
        let mut last = MavMessage::MISSION_COUNT(apm::MISSION_COUNT_DATA {
            count,
            target_system: sysid,
            target_component: mav::AUTOPILOT_COMPID,
            ..Default::default()
        });
        self.v().send(&last);
        let mut timeouts = 0;
        loop {
            let msg = recv_until(&mut rx, MISSION_TIMEOUT, |m| {
                matches!(
                    m,
                    MavMessage::MISSION_REQUEST_INT(_)
                        | MavMessage::MISSION_REQUEST(_)
                        | MavMessage::MISSION_ACK(_)
                )
            })
            .await;
            let Some(msg) = msg else {
                timeouts += 1;
                if timeouts > MISSION_RETRIES {
                    return fail("mission upload timed out");
                }
                self.v().send(&last);
                continue;
            };
            timeouts = 0;
            let seq = match &*msg {
                MavMessage::MISSION_ACK(a) => {
                    return match a.mavtype {
                        apm::MavMissionResult::MAV_MISSION_ACCEPTED => Ok(()),
                        r => fail(format!("mission rejected: {r:?}")),
                    };
                }
                MavMessage::MISSION_REQUEST_INT(r) => r.seq,
                MavMessage::MISSION_REQUEST(r) => r.seq,
                _ => continue,
            };
            if seq >= count {
                return fail(format!("vehicle requested item {seq} of {count}"));
            }
            self.op()
                .set_progress(Some(format!("upload {}/{count}", seq + 1)));
            last = items[usize::from(seq)].encode(seq, sysid);
            self.v().send(&last);
        }
    }

    async fn ensure_armed(&self) -> Result {
        if self.v().read(|s| s.armed) {
            return Ok(());
        }
        let deadline = Instant::now() + ARM_PATIENCE;
        loop {
            match self
                .command_long(apm::MavCmd::MAV_CMD_COMPONENT_ARM_DISARM, &[1.0])
                .await
            {
                Ok(()) => break,
                Err(OpFailed(e)) => {
                    // pre-arm checks can fail for a while after boot (EKF settling)
                    if Instant::now() > deadline {
                        return fail(e);
                    }
                    let reason = self
                        .v()
                        .read(|s| s.last_statustext().map(str::to_string))
                        .unwrap_or(e);
                    self.op().narrate(format!("arming: {reason}"));
                    sleep(RETRY_PAUSE).await;
                }
            }
        }
        for _ in 0..20 {
            if self.v().read(|s| s.armed) {
                return Ok(());
            }
            sleep(Duration::from_millis(250)).await;
        }
        fail("arm accepted but vehicle never reported armed")
    }

    /// Set a mode by name; returns the mode number to watch for in heartbeats. Autopilots
    /// refuse modes transiently (e.g. AUTO while the EKF settles), so a rejection is retried
    /// for a while before the op fails.
    async fn set_mode(&self, name: &str) -> Result<u32> {
        let mav_type = self.v().read(|s| s.mav_type);
        let Some(number) = mav_type.and_then(|t| mav::mode_number(t, name)) else {
            return fail(format!("{} has no {name} mode", self.kind.name()));
        };
        let deadline = Instant::now() + MODE_PATIENCE;
        loop {
            let flag = apm::MavModeFlag::MAV_MODE_FLAG_CUSTOM_MODE_ENABLED.bits();
            match self
                .command_long(
                    apm::MavCmd::MAV_CMD_DO_SET_MODE,
                    &[f32::from(flag), number as f32],
                )
                .await
            {
                Ok(()) => return Ok(number),
                Err(OpFailed(e)) => {
                    if Instant::now() > deadline {
                        return fail(
                            match self.v().read(|s| s.last_statustext().map(str::to_string)) {
                                Some(reason) => format!("{e} ({reason})"),
                                None => e,
                            },
                        );
                    }
                    self.op().narrate(format!("{name} refused, retrying"));
                    sleep(RETRY_PAUSE).await;
                }
            }
        }
    }

    /// Poll until `cond`. `detail` narrates progress; `guard` may fail the op early.
    async fn wait_until(
        &self,
        mut cond: impl FnMut(&Vehicle) -> bool,
        mut detail: impl FnMut(&Vehicle) -> String,
        mut guard: Option<&mut ModeGuard>,
    ) -> Result {
        loop {
            let step = self.v().read(|v| {
                if cond(v) {
                    return Ok(None);
                }
                if let Some(err) = guard.as_mut().and_then(|g| g.check(v)) {
                    return Err(OpFailed(err));
                }
                Ok(Some(detail(v)))
            })?;
            match step {
                None => return Ok(()),
                Some(d) => self.op().narrate(d),
            }
            sleep(POLL).await;
        }
    }

    fn wait_mode(&self, mode: u32) -> impl std::future::Future<Output = Result> + '_ {
        self.wait_until(move |v| v.custom_mode == Some(mode), mode_detail, None)
    }

    // ---- shared mission plumbing -------------------------------------------------------------

    /// Mission altitude (relative to home; negative = depth). The platform's own cruise altitude
    /// when the fleet manifest knows it (a Global Hawk at 18 km, an AUV at 40 m depth), else
    /// the per-autopilot default. Same-kind vehicles in one tasking are stacked apart.
    fn task_alt(&self) -> f64 {
        let (mut base, mut step) = self.caps.alt;
        let (cruise, home_alt) = self.v().read(|s| {
            (
                s.identity.as_ref().and_then(|i| i.num("cruise_alt_m")),
                s.home_alt,
            )
        });
        if let Some(cruise) = cruise
            && (cruise > 50.0 || cruise < 0.0)
        {
            base = if cruise > 0.0 {
                cruise - home_alt.unwrap_or(0.0)
            } else {
                cruise
            };
            let sign = if step == 0.0 { 1.0 } else { step };
            step = step.abs().max(cruise.abs() * 0.02).copysign(sign);
        }
        base + step * self.ctx.op.slot as f64
    }

    /// Item 0 is the home slot (ArduPilot overwrites it). Grounded aircraft get a takeoff.
    fn mission_prefix(&self, alt: f64) -> Vec<Item> {
        let (pos, airborne) = self
            .v()
            .read(|s| (s.pos.unwrap_or((0.0, 0.0)), s.airborne()));
        let mut items = vec![Item::waypoint(pos.0, pos.1, 0.0)];
        if let Some(takeoff) = self.caps.takeoff
            && !airborne
        {
            let mut it = Item::nav(takeoff, 0.0, 0.0, alt.min(takeoff_alt(self.kind)));
            it.p[0] = if self.kind == Kind::Plane { 15.0 } else { 0.0 }; // plane: takeoff pitch
            items.push(it);
        }
        items
    }

    async fn run_mission(
        &self,
        mut items: Vec<Item>,
        loop_from: Option<usize>,
        done: impl FnMut(&Vehicle) -> bool,
        done_text: String,
        mut progress: impl FnMut(&Vehicle) -> String,
    ) -> Result {
        if let Some(from) = loop_from {
            let mut jump = Item::nav(apm::MavCmd::MAV_CMD_DO_JUMP, 0.0, 0.0, 0.0);
            jump.frame = apm::MavFrame::MAV_FRAME_MISSION;
            jump.p[0] = from as f32;
            jump.p[1] = -1.0;
            items.push(jump);
        }
        self.op().set(
            OpState::Sent,
            format!("uploading {} mission items", items.len()),
        );
        self.upload_mission(&items).await?;
        self.op().set(OpState::Sent, "starting mission");
        self.command_long(apm::MavCmd::MAV_CMD_DO_SET_MISSION_CURRENT, &[1.0])
            .await?;
        self.ensure_armed().await?;
        let auto = self.set_mode(self.caps.auto).await?;
        self.op()
            .set(OpState::Acked, "accepted, waiting for vehicle to execute");
        self.op().set_progress(None);
        self.wait_until(
            |v| v.custom_mode == Some(auto) && v.armed,
            mode_detail,
            None,
        )
        .await?;
        self.op().set(OpState::Active, "executing");
        let mut stall = StallWatch::new();
        let mut guard = ModeGuard {
            mode: auto,
            left_at: None,
        };
        self.wait_until(done, |v| stall.detail(v, progress(v)), Some(&mut guard))
            .await?;
        self.op().set(OpState::Done, done_text);
        Ok(())
    }

    // ---- tasks -------------------------------------------------------------------------------

    async fn orbit(&self) -> Result {
        let Some([lon, lat]) = self.ctx.params.center else {
            return fail("orbit needs center [lon, lat]");
        };
        let mut radius = self.ctx.params.radius.unwrap_or(200.0);
        let min_r = self
            .v()
            .read(|s| s.identity.as_ref().and_then(|i| i.num("min_turn_radius_m")));
        let mut widened = false;
        if let Some(min_r) = min_r.filter(|r| *r != 0.0)
            && radius < min_r * 1.1
        {
            widened = true;
            radius = round_int(min_r * 1.1) as f64; // a 160 m/s HALE can't fly a 250 m circle
        }
        let alt = self.task_alt();
        let mut items = self.mission_prefix(alt);
        let (loop_from, tol) = if self.caps.orbit == Some(OrbitStyle::Loiter) {
            let mut it = Item::nav(apm::MavCmd::MAV_CMD_NAV_LOITER_UNLIM, lat, lon, alt);
            it.p[2] = radius as f32;
            items.push(it);
            (None, (radius * 0.4).max(60.0))
        } else {
            // same-kind vehicles start spread around the ring
            let phase = 2.0 * std::f64::consts::PI * self.ctx.op.slot as f64
                / self.ctx.same_kind.max(1) as f64;
            let from = items.len();
            for (plat, plon) in geo::circle_points(lat, lon, radius, 12, phase) {
                items.push(Item::waypoint(plat, plon, alt));
            }
            (Some(from), (radius * 0.3).max(25.0))
        };
        let kind = self.kind;
        let on_station = move |v: &Vehicle| {
            v.distance_to(lat, lon)
                .is_some_and(|d| (d - radius).abs() < tol)
                && (!kind.flying() || v.airborne())
        };
        let progress = move |v: &Vehicle| {
            v.distance_to(lat, lon)
                .map_or(String::new(), |d| format!("{d:.0} m from center"))
        };
        let text = if widened {
            format!("on station (radius widened to {radius:.0} m, platform minimum)")
        } else {
            "on station".into()
        };
        self.run_mission(items, loop_from, on_station, text, progress)
            .await?;
        self.op().set_progress(Some("orbiting".into()));
        Ok(())
    }

    async fn patrol(&self) -> Result {
        let route = self.ctx.params.route.clone().unwrap_or_default();
        let alt = self.task_alt();
        let mut items = self.mission_prefix(alt);
        let loop_from = items.len();
        for [lon, lat] in &route {
            items.push(Item::waypoint(*lat, *lon, alt));
        }
        let last_wp = (items.len() - 1) as u16;
        let mut reached_last = false;
        // lap complete = we were heading to the last waypoint, then DO_JUMP sent us back
        let watch = move |v: &Vehicle| {
            if v.mission_seq == Some(last_wp) {
                reached_last = true;
            }
            reached_last && v.mission_seq.is_some_and(|s| s < last_wp)
        };
        let has_takeoff = self.caps.takeoff.is_some();
        let n = route.len();
        let progress = move |v: &Vehicle| match v.mission_seq {
            Some(seq) if usize::from(seq) >= loop_from => {
                format!("waypoint {}/{n}", usize::from(seq) - loop_from + 1)
            }
            _ if has_takeoff && !v.airborne() => "taking off".into(),
            _ => "en route to first waypoint".into(),
        };
        self.run_mission(
            items,
            Some(loop_from),
            watch,
            "first lap complete, patrolling".into(),
            progress,
        )
        .await?;
        self.op().set_progress(Some("patrolling".into()));
        Ok(())
    }

    async fn rtl(&self) -> Result {
        let caps = self.caps;
        let home_dist = |v: &Vehicle| v.home.and_then(|(la, lo)| v.distance_to(la, lo));
        // exactly at home (a stand-in that never moved) counts as home
        let at_home = move |v: &Vehicle| home_dist(v).unwrap_or(1e9) < caps.home_radius(v);
        let progress = move |v: &Vehicle| {
            home_dist(v).map_or(String::new(), |d| format!("{d:.0} m from home"))
        };
        let Some(rtl) = self.caps.rtl else {
            // no RTL mode (Sub): fly a one-waypoint mission home at the current task depth
            let Some((pos, home)) = self.v().read(|s| s.pos.zip(s.home)) else {
                return fail("home position unknown");
            };
            let items = vec![
                Item::waypoint(pos.0, pos.1, 0.0),
                Item::waypoint(home.0, home.1, self.task_alt()),
            ];
            return self
                .run_mission(items, None, at_home, "home".into(), progress)
                .await;
        };
        self.op().set(OpState::Sent, "commanding RTL");
        let mode = self.set_mode(rtl).await?;
        self.op().set(OpState::Acked, "accepted");
        self.wait_mode(mode).await?;
        self.op().set(OpState::Active, "returning");
        self.wait_until(at_home, progress, None).await?;
        self.op().set(OpState::Done, "home");
        Ok(())
    }

    async fn hold(&self) -> Result {
        self.op()
            .set(OpState::Sent, format!("commanding {}", self.caps.hold));
        let mode = self.set_mode(self.caps.hold).await?;
        self.op().set(OpState::Acked, "accepted");
        self.wait_mode(mode).await?;
        self.op().set(OpState::Done, "holding");
        Ok(())
    }

    /// Point an antenna tracker at another vehicle. AntennaTracker in AUTO steers toward
    /// whatever GLOBAL_POSITION_INT it hears from SYSID_TARGET, so the bridge relays the
    /// target's position messages to the tracker and sets SYSID_TARGET.
    async fn track(&self) -> Result {
        let tracker = self.v();
        let target = match &self.ctx.target {
            Some(t) if t.uid != tracker.uid => t.clone(),
            _ => return fail("pick one other vehicle to track"),
        };
        self.op()
            .set(OpState::Sent, format!("pointing at uid {}", target.uid));
        let port = tracker.tx.port();
        for v in &self.ctx.fleet {
            v.write(|s| s.forward_to.remove(&port));
        }
        target.write(|s| s.forward_to.insert(port, tracker.tx.clone()));
        tracker.write(|s| s.tracking = Some(target.uid));

        let mut rx = tracker.subscribe();
        let mut confirmed = false;
        for _ in 0..=ACK_RETRIES {
            tracker.send(&MavMessage::PARAM_SET(apm::PARAM_SET_DATA {
                param_value: f32::from(target.sysid),
                target_system: tracker.sysid,
                target_component: mav::AUTOPILOT_COMPID,
                param_id: mav::char_array("SYSID_TARGET"),
                param_type: apm::MavParamType::MAV_PARAM_TYPE_REAL32,
            }));
            let echo = recv_until(&mut rx, ACK_TIMEOUT, |m| {
                matches!(m, MavMessage::PARAM_VALUE(p) if mav::chars_to_string(&p.param_id) == "SYSID_TARGET")
            })
            .await;
            if let Some(echo) = echo
                && let MavMessage::PARAM_VALUE(p) = &*echo
                && round_int(f64::from(p.param_value)) == i64::from(target.sysid)
            {
                confirmed = true;
                break;
            }
        }
        if !confirmed {
            return fail("tracker did not confirm SYSID_TARGET");
        }
        self.ensure_armed().await?;
        let auto = self.set_mode(self.caps.auto).await?;
        self.op().set(OpState::Acked, "accepted");
        self.wait_mode(auto).await?;
        self.op().set(OpState::Active, "slewing");

        // pointing error, degrees; one vehicle's lock at a time (never nested)
        loop {
            let goal = target.read(|t| t.pos);
            let error = tracker.read(|v| {
                let ((tlat, tlon), yaw, (glat, glon)) = (v.pos?, v.yaw?, goal?);
                let want = geo::bearing_deg(tlat, tlon, glat, glon);
                Some(((yaw - want + 180.0).rem_euclid(360.0) - 180.0).abs())
            });
            if error.unwrap_or(999.0) < 10.0 {
                break;
            }
            self.op().narrate(error.map_or_else(
                || "waiting for attitude".into(),
                |e| format!("{e:.0}° off target"),
            ));
            sleep(POLL).await;
        }
        self.op().set(OpState::Done, "locked on");
        self.op().set_progress(Some("tracking".into()));
        Ok(())
    }
}

fn mode_detail(v: &Vehicle) -> String {
    format!("mode {}", v.mode.as_deref().unwrap_or("None"))
}
