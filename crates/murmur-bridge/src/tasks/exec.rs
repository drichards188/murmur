//! Running one op: the MAVLink exchanges (commands with retries, mission upload, arming, mode
//! changes) and the telemetry judgements (active, done, left the task, not moving).
//!
//! Every wait is the link's (`crate::link::LinkTiming`, read afresh for each exchange): the
//! floors on a fast radio, the measured round trip on a slow one (GEO SATCOM, Iridium, an AUV's
//! modem). The formula and its sources are on `LinkTiming`.
//!
//! **Exactly once** (rules 5, murmur-5p4). An order reaches the vehicle once and is started
//! once, across retries, a link that drops mid-exchange, and a bridge restart:
//!
//! - A mission is delivered as a transaction: COUNT, the items, the final ACK; then started
//!   (current item, arm, the mission mode). A lost final ACK is settled by asking the vehicle
//!   what it holds (`confirm_mission`), never by uploading again.
//! - A command is retried with its confirmation field counting the retries, and only when
//!   neither its ACK nor telemetry showing it took effect (a heartbeat in the commanded mode,
//!   armed; the mission's current item) came back within the link's timeout: an ACK lost on
//!   the way back does not make the vehicle execute it twice.
//! - A link that goes silent mid-exchange (nothing heard for its lost-link time) is waited for
//!   up to the op's `patience` (the policy's queue wait for the kind; none: the op fails as it
//!   always did). When it is back, the vehicle is asked what it holds and runs (a full mission
//!   download, compared item by item): running our mission, the op follows it; holding it,
//!   only the start is sent; neither, the transaction starts over.
//! - After a bridge restart an op is told what the last run did (`Resume`): a vehicle that had
//!   started the order is followed while it still runs it and never sent it again; one that
//!   may have been sent it but not started it is checked before anything is sent.
//! - A reboot (the vehicle's time since boot went back) ends the op: the vehicle lost the mode
//!   and armed state the order set, and the order is not re-sent behind anyone's back.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast::{self, error::RecvError};
use tokio::time::{Instant, sleep, timeout_at};

use super::caps::{Caps, OrbitStyle, caps, takeoff_alt};
use super::op::{OpCell, OpState};
use super::{TaskKind, TaskParams};
use crate::clock::{self, round_int};
use crate::geo;
use crate::link::LinkTiming;
use crate::mav::{self, Kind, MavMessage, apm};
use crate::vehicle::{Vehicle, VehicleCell};

const ACK_RETRIES: u8 = 4;
/// Item timeouts in a row (nothing heard from the vehicle) before a mission upload fails.
const MISSION_RETRIES: u32 = 5;
const READY_TIMEOUT: Duration = Duration::from_secs(120);
const ARM_PATIENCE: Duration = Duration::from_secs(30);
const MODE_PATIENCE: Duration = Duration::from_secs(20);
const RETRY_PAUSE: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(500);
/// A vehicle may be out of its task's mode this long (a transient) before the op fails.
const LEFT_MODE_GRACE_S: f64 = 3.0;
const STALL_MIN_SPEED: f64 = 0.3;

/// How an op delivers its order: fresh, or (after a bridge restart) knowing the vehicle may
/// hold it already.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Resume {
    /// A new order: deliver it and start it.
    #[default]
    Fresh,
    /// It may have been delivered before a bridge restart, not started: if the vehicle runs
    /// exactly it, follow it; if it holds it, only start it; else deliver it.
    Verify,
    /// The vehicle had started it before a bridge restart: follow it while it still runs it;
    /// otherwise the op ends (it is never sent again).
    Adopt,
    /// Another vehicle of the order had started it before a bridge restart, this one had not
    /// been sent it: it is not sent now (the op ends, failed, saying so).
    Undelivered,
    /// This vehicle's op had ended (failed or replaced) before a bridge restart: it stays
    /// ended (failed, saying so), never sent again.
    Ended,
}

/// Why an op stopped short.
enum Stop {
    Failed(String),
    /// The link went silent mid-exchange (nothing heard for its lost-link time): `what` was
    /// in progress.
    LinkLost(String),
}

type Result<T = ()> = std::result::Result<T, Stop>;

fn fail<T>(s: impl Into<String>) -> Result<T> {
    Err(Stop::Failed(s.into()))
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
    /// how the order is delivered (after a bridge restart the vehicle may hold it already)
    pub resume: Resume,
    /// how long a link lost mid-exchange is waited for before the op fails (zero: not at all)
    pub patience: Duration,
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
    match run_inner(&ctx).await {
        Ok(()) => {}
        Err(Stop::Failed(why)) => {
            ctx.op.set(OpState::Failed, why);
        }
        Err(Stop::LinkLost(what)) => {
            ctx.op.set(OpState::Failed, format!("link lost {what}"));
        }
    }
}

async fn run_inner(ctx: &OpCtx) -> Result {
    match ctx.resume {
        Resume::Undelivered => {
            return fail(
                "not sent before the bridge restarted (another vehicle of the order had started it): not sent now; re-issue it for this vehicle",
            );
        }
        Resume::Ended => {
            return fail("its op had ended before the bridge restarted: not sent again");
        }
        _ => {}
    }
    if !ctx.v.read(|s| s.has_position(clock::mono())) {
        let deadline = Instant::now() + READY_TIMEOUT.max(ctx.patience);
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

    /// Whether a vehicle's copy is this item. ArduPilot stores items compactly and answers in
    /// its own frame (non-INT), so compare what survives: the command, the position to the
    /// 1e-7 degree and, for a jump, its target.
    fn same_as(&self, it: &apm::MISSION_ITEM_INT_DATA) -> bool {
        let close = |a: i32, b: f64| (i64::from(a) - (b * 1e7) as i64).abs() <= 2;
        it.command == self.command
            && close(it.x, self.lat)
            && close(it.y, self.lon)
            && (self.command != apm::MavCmd::MAV_CMD_DO_JUMP
                || round_int(f64::from(it.param1)) == round_int(f64::from(self.p[0])))
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

/// Whether a mission the vehicle holds is ours: as many items, and every one past the home
/// slot (item 0, which ArduPilot overwrites with home) the same.
fn same_mission(ours: &[Item], held: &[apm::MISSION_ITEM_INT_DATA]) -> bool {
    ours.len() == held.len() && ours.iter().zip(held).skip(1).all(|(a, b)| a.same_as(b))
}

/// Add the jump back to `loop_from` that makes a mission loop.
fn with_jump(mut items: Vec<Item>, loop_from: Option<usize>) -> Vec<Item> {
    if let Some(from) = loop_from {
        let mut jump = Item::nav(apm::MavCmd::MAV_CMD_DO_JUMP, 0.0, 0.0, 0.0);
        jump.frame = apm::MavFrame::MAV_FRAME_MISSION;
        jump.p[0] = from as f32;
        jump.p[1] = -1.0;
        items.push(jump);
    }
    items
}

/// What a vehicle holds and runs, against our mission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnVehicle {
    /// In its mission mode, armed, holding exactly our mission: it runs it.
    Running,
    /// It holds our mission but does not run it.
    Stored,
    /// It holds something else (or is still receiving).
    Other,
}

/// "NOT MOVING" detection: if a vehicle that should be moving shows no ground speed for a
/// while, say so. The autopilot can be in AUTO and still not going anywhere (aground, fouled,
/// stuck): that's what the operator needs to see. "A while" is 10 s on a streaming link and
/// longer where speed is reported less often (`LinkTiming::stall_after_s`): a vehicle on
/// HIGH_LATENCY2 every 5 s, or 20 s under water, must not read as stuck between reports.
struct StallWatch {
    moving_at: f64,
}

impl StallWatch {
    fn new() -> StallWatch {
        StallWatch {
            moving_at: clock::mono(),
        }
    }

    fn detail(&mut self, v: &Vehicle, timing: &LinkTiming, base: String) -> String {
        let now = clock::mono();
        if v.groundspeed.unwrap_or(0.0) >= STALL_MIN_SPEED {
            self.moving_at = now;
        }
        let stalled = now - self.moving_at;
        if stalled >= timing.stall_after_s() {
            format!("NOT MOVING for {stalled:.0}s (aground or stuck?) · {base}")
        } else {
            base
        }
    }
}

/// The circle an orbit task flies: its radius (the one asked, 200 m by default, widened to
/// what the airframe can turn) and the tolerance it counts as on station within.
pub(crate) fn orbit_geometry(
    radius: Option<f64>,
    min_turn: Option<f64>,
    style: Option<OrbitStyle>,
) -> (f64, bool, f64) {
    let mut radius = radius.unwrap_or(200.0);
    let mut widened = false;
    if let Some(min_r) = min_turn.filter(|r| *r != 0.0)
        && radius < min_r * 1.1
    {
        widened = true;
        radius = round_int(min_r * 1.1) as f64; // a 160 m/s HALE can't fly a 250 m circle
    }
    let tol = if style == Some(OrbitStyle::Loiter) {
        (radius * 0.4).max(60.0)
    } else {
        (radius * 0.3).max(25.0)
    };
    (radius, widened, tol)
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

    fn timing(&self) -> LinkTiming {
        self.v().link_timing()
    }

    /// The vehicle rebooted since the op started: it lost the mode and armed state (a
    /// stand-in, the mission too) its order set, so the op is over (never re-sent quietly).
    fn check_reboot(&self) -> Result {
        if self.v().read(|s| s.boot_epoch) != self.op().boot_epoch {
            return fail(
                "the vehicle rebooted (its time since boot went back): its mode and armed state are gone; the order is not sent again",
            );
        }
        Ok(())
    }

    /// Nothing heard from the vehicle for its link's lost-link time (3 of its expected
    /// report intervals).
    fn link_silent(&self) -> bool {
        let heard = self.v().read(|s| s.heard_mono);
        let stale = self.v().link_snapshot(clock::mono()).stale_after_s();
        heard.is_none_or(|t| clock::mono() - t > stale)
    }

    /// The exchange in progress could not finish: a lost link if the vehicle is silent, else
    /// a failure saying `failed`.
    fn stopped<T>(&self, what: &str, failed: String) -> Result<T> {
        if self.link_silent() {
            Err(Stop::LinkLost(what.to_string()))
        } else {
            Err(Stop::Failed(failed))
        }
    }

    /// The link went silent `what`: wait until the vehicle is heard again, at most until
    /// `deadline` (from the op's patience; with none, the op fails at once, as it always did).
    async fn wait_link(&self, deadline: Instant, what: &str) -> Result {
        if self.ctx.patience.is_zero() {
            return Err(Stop::LinkLost(what.to_string()));
        }
        loop {
            self.check_reboot()?;
            if !self.link_silent() {
                self.op().narrate(format!("link back {what}: checking what the vehicle holds"));
                return Ok(());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Stop::LinkLost(format!(
                    "{what}: not back within {:.0} s",
                    self.ctx.patience.as_secs_f64()
                )));
            }
            self.op().narrate(format!(
                "link lost {what}: waiting for the vehicle ({:.0} s left)",
                left.as_secs_f64()
            ));
            sleep(POLL).await;
        }
    }

    /// COMMAND_LONG with retries. The `confirmation` field increments on each retry so the
    /// autopilot can tell a retransmission from a new command. Each try waits the link's ACK
    /// timeout, a little longer each time; a late ACK to an earlier try still counts, and so
    /// does telemetry showing the command took effect (`took`): the retry is sent only when
    /// neither came back. MAV_RESULT_IN_PROGRESS waits on without sending again.
    async fn command_long(
        &self,
        command: apm::MavCmd,
        params: &[f32],
        took: impl Fn(&MavMessage) -> bool,
    ) -> Result {
        let mut p = [0.0f32; 7];
        p[..params.len()].copy_from_slice(params);
        let mut rx = self.v().subscribe();
        let is_ack = |m: &MavMessage| matches!(m, MavMessage::COMMAND_ACK(a) if a.command == command);
        for confirmation in 0..=ACK_RETRIES {
            self.check_reboot()?;
            self.v()
                .send(&mav::command_long(self.v().sysid, command, confirmation, p));
            let wait = self.timing().ack_timeout(u32::from(confirmation));
            let mut answer = recv_until(&mut rx, wait, |m| is_ack(m) || took(m)).await;
            // in progress: the vehicle is on it; wait for its final answer, don't send again
            while let Some(MavMessage::COMMAND_ACK(a)) = answer.as_deref()
                && a.result == apm::MavResult::MAV_RESULT_IN_PROGRESS
            {
                answer = recv_until(&mut rx, wait, |m| is_ack(m) || took(m)).await;
            }
            match answer.as_deref() {
                None => {}
                Some(MavMessage::COMMAND_ACK(a)) => match a.result {
                    apm::MavResult::MAV_RESULT_ACCEPTED => return Ok(()),
                    r => return fail(format!("{} rejected: {r:?}", mav::cmd_name(command))),
                },
                // the vehicle's own telemetry says it took effect: its ACK was lost
                Some(_) => return Ok(()),
            }
        }
        let name = mav::cmd_name(command);
        self.stopped(
            &format!("waiting for the {name} ACK"),
            format!("no ACK for {name} after {} tries", ACK_RETRIES + 1),
        )
    }

    /// MAVLink mission protocol: COUNT -> (REQUEST_INT seq -> ITEM_INT seq)* -> ACK. The
    /// vehicle drives the transfer and re-requests a missing item every second (ArduPilot's
    /// MissionItemProtocol), so on a link whose round trip is longer than that, requests come
    /// twice. Tolerating that, and loss, the way the protocol intends:
    /// - a request for an item still in flight (sent less than `LinkTiming::in_flight` ago) or
    ///   for one the vehicle already moved past is not answered again;
    /// - MAV_MISSION_INVALID_SEQUENCE is the vehicle ignoring a duplicate we sent anyway; it
    ///   keeps waiting for the item it asked for, so the upload goes on;
    /// - silence for the link's item timeout: resend whatever went last (the vehicle may also
    ///   have lost the COUNT);
    /// - after the last item, silence or MAV_MISSION_ERROR (no longer receiving: its ACCEPTED
    ///   was lost) is settled by asking the vehicle what it holds (`confirm_mission`), never
    ///   by uploading again;
    /// - silence because the link went down (nothing heard at all): a lost link, which the
    ///   caller waits out and then checks what the vehicle holds.
    ///
    /// Older firmware asks with the deprecated MISSION_REQUEST.
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
        let mut sent: Option<(u16, Instant)> = None;
        let mut asked: Option<u16> = None;
        let mut timeouts = 0;
        loop {
            self.check_reboot()?;
            let timing = self.timing();
            let msg = recv_until(&mut rx, timing.item_timeout(), |m| {
                matches!(
                    m,
                    MavMessage::MISSION_REQUEST_INT(_)
                        | MavMessage::MISSION_REQUEST(_)
                        | MavMessage::MISSION_ACK(_)
                )
            })
            .await;
            let final_sent = sent.is_some_and(|(seq, _)| seq + 1 == count);
            let Some(msg) = msg else {
                timeouts += 1;
                if timeouts > MISSION_RETRIES {
                    return self.stopped("mid-upload", "mission upload timed out".into());
                }
                if final_sent && self.confirm_mission(items, &mut rx).await? {
                    return Ok(());
                }
                self.v().send(&last);
                continue;
            };
            let seq = match &*msg {
                MavMessage::MISSION_ACK(a) => match a.mavtype {
                    apm::MavMissionResult::MAV_MISSION_ACCEPTED => return Ok(()),
                    apm::MavMissionResult::MAV_MISSION_INVALID_SEQUENCE => continue,
                    apm::MavMissionResult::MAV_MISSION_ERROR if final_sent => {
                        if self.confirm_mission(items, &mut rx).await? {
                            return Ok(());
                        }
                        return fail("mission upload lost: the vehicle stopped receiving");
                    }
                    // ArduPilot gave up on the transfer (8 s without an item) while the link
                    // was down: start over once it is back
                    apm::MavMissionResult::MAV_MISSION_OPERATION_CANCELLED if self.link_silent() => {
                        return Err(Stop::LinkLost("mid-upload".into()));
                    }
                    r => return fail(format!("mission rejected: {r:?}")),
                },
                MavMessage::MISSION_REQUEST_INT(r) => r.seq,
                MavMessage::MISSION_REQUEST(r) => r.seq,
                _ => continue,
            };
            timeouts = 0;
            if seq >= count {
                return fail(format!("vehicle requested item {seq} of {count}"));
            }
            if asked.is_some_and(|a| seq < a) {
                continue; // a stale re-request: the vehicle already has that one
            }
            if sent.is_some_and(|(s, at)| s == seq && at.elapsed() < timing.in_flight()) {
                continue; // it crossed our copy, which is still on its way
            }
            asked = Some(seq);
            self.op()
                .set_progress(Some(format!("upload {}/{count}", seq + 1)));
            last = items[usize::from(seq)].encode(seq, sysid);
            self.v().send(&last);
            sent = Some((seq, Instant::now()));
        }
    }

    /// Whether the vehicle holds the mission just uploaded, when its final ACK never came:
    /// MISSION_REQUEST_LIST for the count, then the last item. The vehicle stores items only
    /// in order, so it asked for the last item only after taking every other one; if it also
    /// holds our last item, the whole mission is ours. Ok(false): it is still receiving (it
    /// answers DENIED) or holds something else; the upload goes on or fails.
    #[allow(deprecated)]
    async fn confirm_mission(
        &self,
        items: &[Item],
        rx: &mut broadcast::Receiver<Arc<MavMessage>>,
    ) -> Result<bool> {
        let sysid = self.v().sysid;
        let count = items.len() as u16;
        let target = (sysid, mav::AUTOPILOT_COMPID);
        let denied = |m: &MavMessage| {
            matches!(m, MavMessage::MISSION_ACK(a)
                if a.mavtype == apm::MavMissionResult::MAV_MISSION_DENIED)
        };
        self.op().narrate("confirming the mission upload");
        let mut held = None;
        for attempt in 0..=u32::from(ACK_RETRIES) {
            self.v().send(&MavMessage::MISSION_REQUEST_LIST(
                apm::MISSION_REQUEST_LIST_DATA {
                    target_system: target.0,
                    target_component: target.1,
                    ..Default::default()
                },
            ));
            let wait = self.timing().ack_timeout(attempt);
            let m = recv_until(rx, wait, |m| {
                matches!(m, MavMessage::MISSION_COUNT(_)) || denied(m)
            })
            .await;
            match m.as_deref() {
                Some(MavMessage::MISSION_COUNT(c)) => {
                    held = Some(c.count);
                    break;
                }
                Some(_) => return Ok(false), // still receiving
                None => {}
            }
        }
        let Some(held) = held else {
            return self.stopped(
                "confirming the mission upload",
                "mission upload unconfirmed: no answer to MISSION_REQUEST_LIST".into(),
            );
        };
        if held != count {
            return Ok(false);
        }
        let seq = count - 1;
        for attempt in 0..=u32::from(ACK_RETRIES) {
            self.v().send(&MavMessage::MISSION_REQUEST_INT(
                apm::MISSION_REQUEST_INT_DATA {
                    seq,
                    target_system: target.0,
                    target_component: target.1,
                    ..Default::default()
                },
            ));
            let wait = self.timing().ack_timeout(attempt);
            let m = recv_until(rx, wait, |m| {
                matches!(m, MavMessage::MISSION_ITEM_INT(it) if it.seq == seq) || denied(m)
            })
            .await;
            match m.as_deref() {
                Some(MavMessage::MISSION_ITEM_INT(it)) => {
                    return Ok(items[usize::from(seq)].same_as(it));
                }
                Some(_) => return Ok(false),
                None => {}
            }
        }
        self.stopped(
            "confirming the mission upload",
            "mission upload unconfirmed: the vehicle did not send its last item".into(),
        )
    }

    /// The whole mission the vehicle holds (MISSION_REQUEST_LIST, then every item), to
    /// compare with ours item by item after a lost link or a bridge restart. None: it is
    /// still receiving an upload (it answers DENIED).
    #[allow(deprecated)]
    async fn download_mission(&self) -> Result<Option<Vec<apm::MISSION_ITEM_INT_DATA>>> {
        let sysid = self.v().sysid;
        let target = (sysid, mav::AUTOPILOT_COMPID);
        let mut rx = self.v().subscribe();
        let denied = |m: &MavMessage| {
            matches!(m, MavMessage::MISSION_ACK(a)
                if a.mavtype == apm::MavMissionResult::MAV_MISSION_DENIED)
        };
        self.op().narrate("asking the vehicle what mission it holds");
        let mut count = None;
        for attempt in 0..=u32::from(ACK_RETRIES) {
            self.check_reboot()?;
            self.v().send(&MavMessage::MISSION_REQUEST_LIST(
                apm::MISSION_REQUEST_LIST_DATA {
                    target_system: target.0,
                    target_component: target.1,
                    ..Default::default()
                },
            ));
            let wait = self.timing().ack_timeout(attempt);
            match recv_until(&mut rx, wait, |m| {
                matches!(m, MavMessage::MISSION_COUNT(_)) || denied(m)
            })
            .await
            .as_deref()
            {
                Some(MavMessage::MISSION_COUNT(c)) => {
                    count = Some(c.count);
                    break;
                }
                Some(_) => return Ok(None),
                None => {}
            }
        }
        let Some(count) = count else {
            return self.stopped(
                "asking what mission it holds",
                "no answer to MISSION_REQUEST_LIST".into(),
            );
        };
        let mut held = Vec::with_capacity(usize::from(count));
        for seq in 0..count {
            let mut got = None;
            for attempt in 0..=u32::from(ACK_RETRIES) {
                self.check_reboot()?;
                self.v().send(&MavMessage::MISSION_REQUEST_INT(
                    apm::MISSION_REQUEST_INT_DATA {
                        seq,
                        target_system: target.0,
                        target_component: target.1,
                        ..Default::default()
                    },
                ));
                let wait = self.timing().ack_timeout(attempt);
                match recv_until(&mut rx, wait, |m| {
                    matches!(m, MavMessage::MISSION_ITEM_INT(it) if it.seq == seq) || denied(m)
                })
                .await
                .as_deref()
                {
                    Some(MavMessage::MISSION_ITEM_INT(it)) => {
                        got = Some(it.clone());
                        break;
                    }
                    Some(_) => return Ok(None),
                    None => {}
                }
            }
            match got {
                Some(it) => held.push(it),
                None => {
                    return self.stopped(
                        "asking what mission it holds",
                        format!("the vehicle did not send mission item {seq}"),
                    );
                }
            }
            self.op()
                .set_progress(Some(format!("checking {}/{count}", seq + 1)));
        }
        self.op().set_progress(None);
        Ok(Some(held))
    }

    /// The mission mode's number, if the vehicle has one.
    fn auto_mode(&self) -> Option<u32> {
        let t = self.v().read(|s| s.mav_type)?;
        mav::mode_number(t, self.caps.auto)
    }

    /// What the vehicle holds and runs, against any of our mission's forms (`candidates`:
    /// with and without a takeoff, which depends on whether it was airborne when it was
    /// built).
    async fn mission_held(&self, candidates: &[Vec<Item>]) -> Result<OnVehicle> {
        let Some(held) = self.download_mission().await? else {
            return Ok(OnVehicle::Other);
        };
        if !candidates.iter().any(|c| same_mission(c, &held)) {
            return Ok(OnVehicle::Other);
        }
        let auto = self.auto_mode();
        let running = self
            .v()
            .read(|s| s.armed && auto.is_some() && s.custom_mode == auto);
        Ok(if running { OnVehicle::Running } else { OnVehicle::Stored })
    }

    async fn ensure_armed(&self) -> Result {
        if self.v().read(|s| s.armed) {
            return Ok(());
        }
        let deadline = Instant::now() + ARM_PATIENCE;
        let armed = |m: &MavMessage| {
            matches!(m, MavMessage::HEARTBEAT(h)
                if h.autopilot != apm::MavAutopilot::MAV_AUTOPILOT_INVALID
                    && h.base_mode.contains(apm::MavModeFlag::MAV_MODE_FLAG_SAFETY_ARMED))
        };
        let mut left_return_mode = false;
        loop {
            match self
                .command_long(apm::MavCmd::MAV_CMD_COMPONENT_ARM_DISARM, &[1.0], armed)
                .await
            {
                Ok(()) => break,
                Err(Stop::Failed(e)) => {
                    // a vehicle its lost-link failsafe sent home while the order waited (an
                    // outage) is in a mode no autopilot arms in: hold first, then arm (the
                    // order's own mode follows)
                    let mode = self.v().read(|s| s.mode.clone()).unwrap_or_default();
                    if !left_return_mode
                        && matches!(mode.as_str(), "RTL" | "SMART_RTL" | "QRTL" | "LAND" | "QLAND")
                    {
                        left_return_mode = true;
                        self.op().narrate(format!(
                            "arming refused in {mode} (its failsafe?): switching to {} to arm",
                            self.caps.hold
                        ));
                        self.set_mode(self.caps.hold).await?;
                        continue;
                    }
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
                Err(lost) => return Err(lost),
            }
        }
        // the next heartbeat says so: a second away when streaming, 20 s from an AUV under water
        let deadline = Instant::now() + self.timing().confirm_wait();
        while Instant::now() < deadline {
            if self.v().read(|s| s.armed) {
                return Ok(());
            }
            sleep(Duration::from_millis(250)).await;
        }
        if self.v().read(|s| s.armed) {
            return Ok(());
        }
        fail("arm accepted but vehicle never reported armed")
    }

    /// Set a mode by name; returns the mode number to watch for in heartbeats. Autopilots
    /// refuse modes transiently (e.g. AUTO while the EKF settles), so a rejection is retried
    /// for a while before the op fails. A heartbeat (or HIGH_LATENCY2) in the mode counts as
    /// the answer: a lost ACK is not followed by a second mode change.
    async fn set_mode(&self, name: &str) -> Result<u32> {
        let mav_type = self.v().read(|s| s.mav_type);
        let Some(number) = mav_type.and_then(|t| mav::mode_number(t, name)) else {
            return fail(format!("{} has no {name} mode", self.kind.name()));
        };
        let in_mode = move |m: &MavMessage| match m {
            MavMessage::HEARTBEAT(h) => {
                h.autopilot != apm::MavAutopilot::MAV_AUTOPILOT_INVALID && h.custom_mode == number
            }
            MavMessage::HIGH_LATENCY2(h) => u32::from(h.custom_mode) == number,
            _ => false,
        };
        let deadline = Instant::now() + MODE_PATIENCE;
        loop {
            let flag = apm::MavModeFlag::MAV_MODE_FLAG_CUSTOM_MODE_ENABLED.bits();
            match self
                .command_long(
                    apm::MavCmd::MAV_CMD_DO_SET_MODE,
                    &[f32::from(flag), number as f32],
                    in_mode,
                )
                .await
            {
                Ok(()) => return Ok(number),
                Err(Stop::Failed(e)) => {
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
                Err(lost) => return Err(lost),
            }
        }
    }

    /// Poll until `cond`. `detail` narrates progress (given the link's timing); `guard` may
    /// fail the op early. A reboot ends it.
    async fn wait_until(
        &self,
        mut cond: impl FnMut(&Vehicle) -> bool,
        mut detail: impl FnMut(&Vehicle, &LinkTiming) -> String,
        mut guard: Option<&mut ModeGuard>,
    ) -> Result {
        loop {
            self.check_reboot()?;
            let timing = self.timing();
            let step = self.v().read(|v| {
                if cond(v) {
                    return Ok(None);
                }
                if let Some(err) = guard.as_mut().and_then(|g| g.check(v)) {
                    return Err(Stop::Failed(err));
                }
                Ok(Some(detail(v, &timing)))
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

    /// Whether a mission built now starts with a takeoff (grounded aircraft).
    fn needs_takeoff(&self) -> bool {
        self.caps.takeoff.is_some() && !self.v().read(|s| s.airborne())
    }

    /// The takeoff choices a mission may have been built with: both for an aircraft (it may
    /// have been grounded or airborne then), none for the rest.
    fn takeoff_choices(&self) -> Vec<bool> {
        if self.caps.takeoff.is_some() {
            vec![true, false]
        } else {
            vec![false]
        }
    }

    /// Item 0 is the home slot (ArduPilot overwrites it). With `takeoff`, a takeoff item.
    fn mission_prefix(&self, alt: f64, takeoff: bool) -> Vec<Item> {
        let pos = self.v().read(|s| s.pos.unwrap_or((0.0, 0.0)));
        let mut items = vec![Item::waypoint(pos.0, pos.1, 0.0)];
        if let Some(cmd) = self.caps.takeoff
            && takeoff
        {
            let mut it = Item::nav(cmd, 0.0, 0.0, alt.min(takeoff_alt(self.kind)));
            it.p[0] = if self.kind == Kind::Plane { 15.0 } else { 0.0 }; // plane: takeoff pitch
            items.push(it);
        }
        items
    }

    /// Deliver the mission and start it, as one transaction. A link lost mid-way is waited
    /// for (the op's patience); then the vehicle is asked what it holds: running our mission,
    /// it is followed (true: adopted, nothing more sent); holding it, only the start is sent;
    /// neither, the upload starts over. Returns whether it was adopted.
    async fn deliver_mission(
        &self,
        items: &[Item],
        candidates: &[Vec<Item>],
        known: OnVehicle,
    ) -> Result<bool> {
        let deadline = Instant::now() + self.ctx.patience;
        let mut held = known;
        let mut after_outage = false;
        loop {
            if after_outage {
                match self.mission_held(candidates).await {
                    Ok(OnVehicle::Running) => return Ok(true),
                    Ok(h) => held = h,
                    Err(Stop::LinkLost(w)) => {
                        self.wait_link(deadline, &w).await?;
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }
            if held != OnVehicle::Stored {
                self.op().set(
                    OpState::Sent,
                    format!("uploading {} mission items", items.len()),
                );
                match self.upload_mission(items).await {
                    Ok(()) => {}
                    Err(Stop::LinkLost(w)) => {
                        self.wait_link(deadline, &w).await?;
                        after_outage = true;
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }
            match self.start_mission().await {
                Ok(()) => return Ok(false),
                Err(Stop::LinkLost(w)) => {
                    self.wait_link(deadline, &w).await?;
                    after_outage = true;
                    held = OnVehicle::Other;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Start the mission the vehicle holds: from item 1 (confirmed by its MISSION_CURRENT),
    /// armed, in the mission mode.
    async fn start_mission(&self) -> Result {
        self.op().set(OpState::Sent, "starting mission");
        let first = |m: &MavMessage| match m {
            MavMessage::MISSION_CURRENT(c) => c.seq == 1,
            MavMessage::HIGH_LATENCY2(h) => h.wp_num == 1,
            _ => false,
        };
        self.command_long(apm::MavCmd::MAV_CMD_DO_SET_MISSION_CURRENT, &[1.0], first)
            .await?;
        self.ensure_armed().await?;
        self.set_mode(self.caps.auto).await?;
        Ok(())
    }

    async fn run_mission(
        &self,
        items: Vec<Item>,
        candidates: Vec<Vec<Item>>,
        done: impl FnMut(&Vehicle) -> bool,
        done_text: String,
        mut progress: impl FnMut(&Vehicle) -> String,
    ) -> Result {
        let adopted = match self.ctx.resume {
            Resume::Fresh | Resume::Undelivered | Resume::Ended => {
                self.deliver_mission(&items, &candidates, OnVehicle::Other).await?
            }
            Resume::Verify | Resume::Adopt => {
                let held = self.held_after_restart(&candidates).await?;
                match (held, self.ctx.resume) {
                    (OnVehicle::Running, _) => true,
                    (_, Resume::Adopt) => {
                        let mode = self.v().read(|s| s.mode.clone()).unwrap_or_default();
                        return fail(format!(
                            "after the bridge restart the vehicle no longer runs it (mode {mode}, {}): it had started it, so it is not sent again",
                            if held == OnVehicle::Stored { "mission still held" } else { "another mission" }
                        ));
                    }
                    // held, not started: only the start goes; else the whole order
                    (h, _) => self.deliver_mission(&items, &candidates, h).await?,
                }
            }
        };
        let auto = self
            .auto_mode()
            .ok_or_else(|| Stop::Failed(format!("{} has no {} mode", self.kind.name(), self.caps.auto)))?;
        if adopted {
            self.op().set(
                OpState::Acked,
                "the vehicle already runs it: followed, not sent again",
            );
        } else {
            self.op()
                .set(OpState::Acked, "accepted, waiting for vehicle to execute");
        }
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
        self.wait_until(
            done,
            |v, timing| stall.detail(v, timing, progress(v)),
            Some(&mut guard),
        )
        .await?;
        self.op().set(OpState::Done, done_text);
        Ok(())
    }

    /// After a bridge restart: what the vehicle holds and runs, waiting out a lost link (the
    /// op's patience).
    async fn held_after_restart(&self, candidates: &[Vec<Item>]) -> Result<OnVehicle> {
        let deadline = Instant::now() + self.ctx.patience;
        self.op()
            .narrate("after a bridge restart: asking the vehicle what it runs");
        loop {
            if self.link_silent() {
                self.wait_link(deadline, "after the bridge restart").await?;
            }
            match self.mission_held(candidates).await {
                Err(Stop::LinkLost(w)) => self.wait_link(deadline, &w).await?,
                other => return other,
            }
        }
    }

    // ---- tasks -------------------------------------------------------------------------------

    async fn orbit(&self) -> Result {
        let Some([lon, lat]) = self.ctx.params.center else {
            return fail("orbit needs center [lon, lat]");
        };
        let min_r = self
            .v()
            .read(|s| s.identity.as_ref().and_then(|i| i.num("min_turn_radius_m")));
        let (radius, widened, tol) = orbit_geometry(self.ctx.params.radius, min_r, self.caps.orbit);
        let alt = self.task_alt();
        let build = |takeoff: bool| {
            let mut items = self.mission_prefix(alt, takeoff);
            let loop_from = if self.caps.orbit == Some(OrbitStyle::Loiter) {
                let mut it = Item::nav(apm::MavCmd::MAV_CMD_NAV_LOITER_UNLIM, lat, lon, alt);
                it.p[2] = radius as f32;
                items.push(it);
                None
            } else {
                // same-kind vehicles start spread around the ring
                let phase = 2.0 * std::f64::consts::PI * self.ctx.op.slot as f64
                    / self.ctx.same_kind.max(1) as f64;
                let from = items.len();
                for (plat, plon) in geo::circle_points(lat, lon, radius, 12, phase) {
                    items.push(Item::waypoint(plat, plon, alt));
                }
                Some(from)
            };
            with_jump(items, loop_from)
        };
        let items = build(self.needs_takeoff());
        let candidates = self.takeoff_choices().into_iter().map(build).collect();
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
        self.run_mission(items, candidates, on_station, text, progress)
            .await?;
        self.op().set_progress(Some("orbiting".into()));
        Ok(())
    }

    async fn patrol(&self) -> Result {
        let route = self.ctx.params.route.clone().unwrap_or_default();
        let alt = self.task_alt();
        let build = |takeoff: bool| {
            let mut items = self.mission_prefix(alt, takeoff);
            let loop_from = items.len();
            for [lon, lat] in &route {
                items.push(Item::waypoint(*lat, *lon, alt));
            }
            (with_jump(items, Some(loop_from)), loop_from)
        };
        let (items, loop_from) = build(self.needs_takeoff());
        let candidates = self
            .takeoff_choices()
            .into_iter()
            .map(|t| build(t).0)
            .collect();
        // the last waypoint (the jump back follows it)
        let last_wp = (items.len() - 2) as u16;
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
            candidates,
            watch,
            "first lap complete, patrolling".into(),
            progress,
        )
        .await?;
        self.op().set_progress(Some("patrolling".into()));
        Ok(())
    }

    /// After a bridge restart, for an order that is a mode (hold, rtl): whether the vehicle
    /// is in it. An order it had started (Adopt) and no longer is in ends here.
    fn mode_after_restart(&self, number: u32, what: &str) -> Result<bool> {
        if !matches!(self.ctx.resume, Resume::Verify | Resume::Adopt) {
            return Ok(false);
        }
        let (now, name) = self.v().read(|s| (s.custom_mode, s.mode.clone()));
        if now == Some(number) {
            return Ok(true);
        }
        if self.ctx.resume == Resume::Adopt {
            return fail(format!(
                "after the bridge restart the vehicle is no longer {what} (mode {}): it had started it, so it is not sent again",
                name.unwrap_or_default()
            ));
        }
        Ok(false)
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
            let candidates = vec![items.clone()];
            return self
                .run_mission(items, candidates, at_home, "home".into(), progress)
                .await;
        };
        let number = self
            .v()
            .read(|s| s.mav_type)
            .and_then(|t| mav::mode_number(t, rtl));
        let mode = match number {
            Some(n) if self.mode_after_restart(n, "returning")? => {
                self.op().set(
                    OpState::Acked,
                    "the vehicle is already returning: followed, not sent again",
                );
                n
            }
            _ => {
                self.op().set(OpState::Sent, "commanding RTL");
                let mode = self.set_mode(rtl).await?;
                self.op().set(OpState::Acked, "accepted");
                mode
            }
        };
        self.wait_mode(mode).await?;
        self.op().set(OpState::Active, "returning");
        self.wait_until(at_home, |v, _| progress(v), None).await?;
        self.op().set(OpState::Done, "home");
        Ok(())
    }

    async fn hold(&self) -> Result {
        let number = self
            .v()
            .read(|s| s.mav_type)
            .and_then(|t| mav::mode_number(t, self.caps.hold));
        if let Some(n) = number
            && self.mode_after_restart(n, "holding")?
        {
            self.op().set(
                OpState::Done,
                "holding (it already was: followed, not sent again)",
            );
            return Ok(());
        }
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
    /// target's position messages to the tracker and sets SYSID_TARGET. Every step is
    /// idempotent (the relay, the parameter, the mode), so after a restart it is simply done
    /// again.
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
        for attempt in 0..=u32::from(ACK_RETRIES) {
            self.check_reboot()?;
            tracker.send(&MavMessage::PARAM_SET(apm::PARAM_SET_DATA {
                param_value: f32::from(target.sysid),
                target_system: tracker.sysid,
                target_component: mav::AUTOPILOT_COMPID,
                param_id: mav::char_array("SYSID_TARGET"),
                param_type: apm::MavParamType::MAV_PARAM_TYPE_REAL32,
            }));
            let echo = recv_until(&mut rx, self.timing().ack_timeout(attempt), |m| {
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
            return self.stopped(
                "setting SYSID_TARGET",
                "tracker did not confirm SYSID_TARGET".into(),
            );
        }
        self.ensure_armed().await?;
        let auto = self.set_mode(self.caps.auto).await?;
        self.op().set(OpState::Acked, "accepted");
        self.wait_mode(auto).await?;
        self.op().set(OpState::Active, "slewing");

        // pointing error, degrees; one vehicle's lock at a time (never nested)
        loop {
            self.check_reboot()?;
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

fn mode_detail(v: &Vehicle, _: &LinkTiming) -> String {
    format!("mode {}", v.mode.as_deref().unwrap_or("None"))
}
