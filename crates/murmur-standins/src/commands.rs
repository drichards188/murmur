//! What a stand-in does with what the GCS sends: heartbeats, TIMESYNC, the parameter
//! protocol, COMMAND_LONG / COMMAND_INT, and the mission protocol (upload and download, for
//! mission, fence and rally). Replies follow ArduPilot's GCS_MAVLink (GCS_Common.cpp,
//! GCS_Param.cpp, MissionItemProtocol.cpp): acks go back to the sender, refusals use the
//! result codes ArduPilot uses. Behavior-specific refusals come from the `Behavior` trait.

use mavlink::MavHeader;
use mavlink::MessageData;
use mavlink::dialects::ardupilotmega::*;

use crate::behavior::{Command, Ctx, Firmware};
use crate::link::Inbound;
use crate::messages;
use crate::mission::{Mission, UPLOAD_RETRY_S, UPLOAD_TIMEOUT_S, Upload};
use crate::params::SetError;
use crate::telemetry::{self, default_interval_ms, known};
use crate::vehicle::Vehicle;

/// PARAM_VALUEs per physics tick while answering PARAM_REQUEST_LIST (100/s).
const PARAMS_PER_TICK: usize = 10;

// MISSION_SET_CURRENT and MISSION_REQUEST/MISSION_ITEM are deprecated but still what older
// GCSs send (and MISSION_REQUEST is what ArduPilot itself asks for items with)
#[allow(deprecated)]
pub fn handle(v: &mut Vehicle, h: &MavHeader, msg: &MavMessage) {
    if let Some(target) = target_system(msg)
        && target != 0
        && target != v.sysid()
    {
        return;
    }
    let from = (h.system_id, h.component_id);
    match msg {
        MavMessage::HEARTBEAT(_) => {
            if f32::from(h.system_id) == v.params.get("MAV_GCS_SYSID").unwrap_or(255.0) {
                v.fs.gcs_last = Some(v.now());
            }
        }
        MavMessage::TIMESYNC(m) => {
            if m.tc1 == 0 {
                // tc1: this system's monotonic clock, ns (timesync_receive_timestamp_ns)
                let reply = TIMESYNC_DATA {
                    tc1: v.started.elapsed().as_nanos() as i64,
                    ts1: m.ts1,
                    ..Default::default()
                };
                v.link.send(&MavMessage::TIMESYNC(reply));
            }
        }
        MavMessage::PARAM_REQUEST_READ(m) => param_read(v, from, m),
        MavMessage::PARAM_REQUEST_LIST(_) => {
            banner(v);
            v.param_stream = Some(0);
        }
        MavMessage::PARAM_SET(m) => param_set(v, from, m),
        MavMessage::COMMAND_LONG(m) => command(
            v,
            from,
            Cmd {
                command: m.command,
                p: [m.param1, m.param2, m.param3, m.param4],
                x: f64::from(m.param5),
                y: f64::from(m.param6),
                z: m.param7,
            },
        ),
        MavMessage::COMMAND_INT(m) => command(
            v,
            from,
            Cmd {
                command: m.command,
                p: [m.param1, m.param2, m.param3, m.param4],
                x: f64::from(m.x) / 1e7,
                y: f64::from(m.y) / 1e7,
                z: m.z,
            },
        ),
        MavMessage::MISSION_SET_CURRENT(m) => v.mission.set_current(m.seq),
        MavMessage::MISSION_COUNT(m) => mission_count(v, from, m),
        MavMessage::MISSION_ITEM_INT(m) => mission_item(v, from, m.clone()),
        MavMessage::MISSION_ITEM(m) => {
            if !v.mission_item_warned {
                v.mission_item_warned = true;
                v.text(
                    MavSeverity::MAV_SEVERITY_WARNING,
                    "got MISSION_ITEM; GCS should send MISSION_ITEM_INT",
                );
            }
            mission_item(v, from, item_int(m));
        }
        MavMessage::MISSION_REQUEST_LIST(m) => request_list(v, from, m.mission_type),
        MavMessage::MISSION_REQUEST_INT(m) => request_item(v, from, m.mission_type, m.seq, true),
        MavMessage::MISSION_REQUEST(m) => request_item(v, from, m.mission_type, m.seq, false),
        MavMessage::MISSION_CLEAR_ALL(m) => clear_all(v, from, m.mission_type),
        _ => {}
    }
}

/// A command or mission item the dialect couldn't decode: answer as ArduPilot does.
pub fn handle_unknown(v: &mut Vehicle, m: &Inbound) {
    match *m {
        Inbound::UnknownCommand {
            header,
            command,
            target,
        } => {
            if target.0 != 0 && target.0 != v.sysid() {
                return;
            }
            let to = (header.system_id, header.component_id);
            v.link
                .send_ack_for_id(command, MavResult::MAV_RESULT_UNSUPPORTED, to);
        }
        Inbound::UnknownMissionItem { header, seq } => {
            let to = (header.system_id, header.component_id);
            let Some(up) = &v.mission.upload else { return };
            if seq != up.next() || to != up.from {
                return;
            }
            let ty = up.mission_type;
            v.mission.upload = None;
            let r = MavMissionResult::MAV_MISSION_UNSUPPORTED;
            v.link.send(&messages::mission_ack_to(r, ty, to));
        }
        Inbound::Msg(..) => {}
    }
}

#[allow(deprecated)]
fn target_system(msg: &MavMessage) -> Option<u8> {
    Some(match msg {
        MavMessage::TIMESYNC(m) => m.target_system,
        MavMessage::PARAM_REQUEST_READ(m) => m.target_system,
        MavMessage::PARAM_REQUEST_LIST(m) => m.target_system,
        MavMessage::PARAM_SET(m) => m.target_system,
        MavMessage::COMMAND_LONG(m) => m.target_system,
        MavMessage::COMMAND_INT(m) => m.target_system,
        MavMessage::MISSION_SET_CURRENT(m) => m.target_system,
        MavMessage::MISSION_COUNT(m) => m.target_system,
        MavMessage::MISSION_ITEM_INT(m) => m.target_system,
        MavMessage::MISSION_ITEM(m) => m.target_system,
        MavMessage::MISSION_REQUEST_LIST(m) => m.target_system,
        MavMessage::MISSION_REQUEST_INT(m) => m.target_system,
        MavMessage::MISSION_REQUEST(m) => m.target_system,
        MavMessage::MISSION_CLEAR_ALL(m) => m.target_system,
        _ => return None,
    })
}

// ---- parameters ----

fn param_read(v: &mut Vehicle, from: (u8, u8), m: &PARAM_REQUEST_READ_DATA) {
    let count = v.params.len();
    // by index unless the index is -1; the reply echoes the requested index
    let found = if m.param_index == -1 {
        let name = m.param_id.to_str().unwrap_or("").to_owned();
        v.params.find(&name).map(|(_, p)| p.clone())
    } else {
        usize::try_from(m.param_index)
            .ok()
            .and_then(|i| v.params.at(i))
            .cloned()
    };
    let msg = match found {
        Some(p) => telemetry::param_value(p.name, p.value, p.ty, count, m.param_index as u16),
        None => telemetry::param_error(
            m.param_id,
            m.param_index,
            MavParamError::MAV_PARAM_ERROR_DOES_NOT_EXIST,
            from,
        ),
    };
    v.link.send(&msg);
}

fn param_set(v: &mut Vehicle, from: (u8, u8), m: &PARAM_SET_DATA) {
    let name = m.param_id.to_str().unwrap_or("").to_owned();
    let count = v.params.len();
    let error = |e| telemetry::param_error(m.param_id, -1, e, from);
    match v.params.set(&name, m.param_value) {
        Ok((_, p)) => {
            // AP_Param::save announces the stored value, index -1
            let msg = telemetry::param_value(p.name, p.value, p.ty, count, u16::MAX);
            v.link.send(&msg);
        }
        Err(SetError::DoesNotExist) => {
            v.link
                .send(&error(MavParamError::MAV_PARAM_ERROR_DOES_NOT_EXIST));
        }
        Err(SetError::OutOfRange) => {
            v.link
                .send(&error(MavParamError::MAV_PARAM_ERROR_VALUE_OUT_OF_RANGE));
        }
        Err(SetError::ReadOnly) => {
            v.text(
                MavSeverity::MAV_SEVERITY_WARNING,
                &format!("Param write denied ({name})"),
            );
            v.link
                .send(&error(MavParamError::MAV_PARAM_ERROR_PERMISSION_DENIED));
            if let Some((_, p)) = v.params.find(&name) {
                let msg = telemetry::param_value(p.name, p.value, p.ty, count, u16::MAX);
                v.link.send(&msg);
            }
        }
    }
}

/// PARAM_REQUEST_LIST: the whole table, in index order, with count and index.
pub fn param_stream_tick(v: &mut Vehicle) {
    let Some(start) = v.param_stream else { return };
    let count = v.params.len();
    let end = (start + PARAMS_PER_TICK).min(count);
    for i in start..end {
        if let Some(p) = v.params.at(i) {
            let msg = telemetry::param_value(p.name, p.value, p.ty, count, i as u16);
            v.link.send(&msg);
        }
    }
    v.param_stream = (end < count).then_some(end);
}

/// GCS_MAVLINK::send_banner (sent on PARAM_REQUEST_LIST and DO_SEND_BANNER).
fn banner(v: &mut Vehicle) {
    let name = match v.firmware {
        Firmware::Plane => "ArduPlane",
        Firmware::Rover => "ArduRover",
        Firmware::Sub => "ArduSub",
        Firmware::Copter => "ArduCopter",
        Firmware::Blimp => "Blimp",
        Firmware::Tracker => "AntennaTracker",
    };
    v.text(
        MavSeverity::MAV_SEVERITY_INFO,
        &format!("{name} V4.8.0-dev (stand-in)"),
    );
}

// ---- commands ----

/// COMMAND_LONG and COMMAND_INT alike: params 1-4, then x/y as degrees, z.
struct Cmd {
    command: MavCmd,
    p: [f32; 4],
    x: f64,
    y: f64,
    z: f32,
}

// GET_MESSAGE_INTERVAL and REQUEST_AUTOPILOT_CAPABILITIES are deprecated, ArduPilot still answers
#[allow(deprecated)]
fn command(v: &mut Vehicle, from: (u8, u8), c: Cmd) {
    use MavCmd::*;
    // the behavior's own handling first (arming checks, mode refusals, user takeoff); what
    // it says goes out ahead of the ACK
    let mut out = Vec::new();
    let own = v.behavior.command(
        &mut Ctx {
            spec: &v.cfg.spec,
            params: &v.params,
            st: &mut v.st,
            mission: &mut v.mission,
            out: &mut out,
        },
        &Command {
            command: c.command,
            p: c.p,
            x: c.x,
            y: c.y,
            z: c.z,
        },
    );
    v.emit(&mut out);
    if let Some(r) = own {
        v.link.send(&messages::command_ack_to(c.command, r, from));
        return;
    }
    if !firmware_handles(v.firmware, c.command) {
        let r = firmware_command(v.firmware, c.command);
        v.link.send(&messages::command_ack_to(c.command, r, from));
        return;
    }
    let r = match c.command {
        MAV_CMD_COMPONENT_ARM_DISARM => arm(v, &c),
        MAV_CMD_DO_SET_MODE => set_mode_command(v, &c),
        MAV_CMD_MISSION_START => request_mode(v, "AUTO"),
        MAV_CMD_NAV_RETURN_TO_LAUNCH => match v.behavior.mode_number("RTL") {
            Some(_) => request_mode(v, "RTL"),
            None => MavResult::MAV_RESULT_UNSUPPORTED,
        },
        MAV_CMD_DO_SET_MISSION_CURRENT => {
            let seq = c.p[0].max(0.0) as u16;
            if usize::from(seq) >= v.mission.items.len().max(1) {
                MavResult::MAV_RESULT_FAILED
            } else {
                v.mission.set_current(seq);
                if c.p[1] == 1.0 {
                    v.mission.jumps.clear();
                }
                MavResult::MAV_RESULT_ACCEPTED
            }
        }
        MAV_CMD_DO_CHANGE_SPEED => {
            if c.p[1] > 0.0 {
                let max = v.cfg.spec.max_mps.max(v.cfg.spec.cruise_mps);
                v.mission.speed = Some(f64::from(c.p[1]).min(max));
            }
            MavResult::MAV_RESULT_ACCEPTED
        }
        MAV_CMD_DO_SET_HOME => {
            v.st.home = if c.p[0] == 1.0 {
                (v.st.lat, v.st.lon, v.st.alt)
            } else {
                (c.x, c.y, f64::from(c.z))
            };
            v.streams.once.push(HOME_POSITION_DATA::ID);
            MavResult::MAV_RESULT_ACCEPTED
        }
        MAV_CMD_GET_HOME_POSITION => {
            v.streams.once.push(HOME_POSITION_DATA::ID);
            MavResult::MAV_RESULT_ACCEPTED
        }
        MAV_CMD_REQUEST_AUTOPILOT_CAPABILITIES => {
            v.streams.once.push(AUTOPILOT_VERSION_DATA::ID);
            MavResult::MAV_RESULT_ACCEPTED
        }
        MAV_CMD_DO_SEND_BANNER => {
            banner(v);
            MavResult::MAV_RESULT_ACCEPTED
        }
        MAV_CMD_SET_MESSAGE_INTERVAL => set_interval(v, &c),
        MAV_CMD_GET_MESSAGE_INTERVAL => get_interval(v, c.p[0] as u32),
        MAV_CMD_REQUEST_MESSAGE => {
            let id = c.p[0] as u32;
            if id == MESSAGE_INTERVAL_DATA::ID {
                get_interval(v, c.p[1] as u32)
            } else if known(id) {
                v.streams.once.push(id);
                MavResult::MAV_RESULT_ACCEPTED
            } else {
                MavResult::MAV_RESULT_FAILED
            }
        }
        _ => MavResult::MAV_RESULT_UNSUPPORTED,
    };
    v.link.send(&messages::command_ack_to(c.command, r, from));
}

/// The commands that only some firmwares' GCS_MAVLINK subclasses handle
/// (handle_command_int_packet in each vehicle's GCS_MAVLink_*.cpp, ArduPilot 4.8); all else
/// in this file is the common GCS_MAVLINK table. A COMMAND_LONG goes through the same table.
fn firmware_handles(fw: Firmware, cmd: MavCmd) -> bool {
    use Firmware::*;
    use MavCmd::*;
    match cmd {
        // Blimp's own table is DO_REPOSITION and NAV_TAKEOFF (see `firmware_command`)
        MAV_CMD_MISSION_START | MAV_CMD_NAV_TAKEOFF => fw != Blimp,
        MAV_CMD_NAV_RETURN_TO_LAUNCH => matches!(fw, Plane | Copter | Rover),
        MAV_CMD_DO_CHANGE_SPEED => matches!(fw, Plane | Copter | Rover | Sub),
        _ => true,
    }
}

/// What a firmware answers to a command in its own table it does not handle, or handles
/// without doing anything: Blimp ACKs NAV_TAKEOFF ACCEPTED whatever its state (it has no
/// takeoff; GCS_MAVLINK_Blimp::handle_command_int_packet), the rest is UNSUPPORTED.
fn firmware_command(fw: Firmware, cmd: MavCmd) -> MavResult {
    if fw == Firmware::Blimp && cmd == MavCmd::MAV_CMD_NAV_TAKEOFF {
        return MavResult::MAV_RESULT_ACCEPTED;
    }
    MavResult::MAV_RESULT_UNSUPPORTED
}

fn arm(v: &mut Vehicle, c: &Cmd) -> MavResult {
    let arm = c.p[0] == 1.0;
    if !arm && v.airborne() && c.p[1] != 21196.0 {
        return MavResult::MAV_RESULT_FAILED; // no disarming in flight (unless forced)
    }
    if arm
        && !v.st.armed
        && let Some(why) = v.behavior.arming_refusal(&v.st)
    {
        // the pre-arm checks run for the arm command: "Arm: <reason>" (AP_Arming::check_failed)
        v.text(MavSeverity::MAV_SEVERITY_CRITICAL, &format!("Arm: {why}"));
        return MavResult::MAV_RESULT_FAILED;
    }
    if arm != v.st.armed {
        v.st.armed = arm;
        let text = v.arm_text(arm);
        v.text(MavSeverity::MAV_SEVERITY_INFO, text);
    }
    MavResult::MAV_RESULT_ACCEPTED
}

/// MAV_CMD_DO_SET_MODE: the custom mode number is a uint32 that set_mode(uint8_t) narrows. A
/// number the firmware has no mode for is "No such mode N" (AP_Vehicle::notify_no_such_mode;
/// AntennaTracker says nothing).
fn set_mode_command(v: &mut Vehicle, c: &Cmd) -> MavResult {
    if !c.p[1].is_finite() {
        return MavResult::MAV_RESULT_FAILED;
    }
    let number = (c.p[1].trunc() as i64 as u32) & 0xff;
    let known = v
        .behavior
        .mode_name(number)
        .filter(|n| !firmware_lacks_mode(v.firmware, n));
    match known {
        Some(name) => request_mode(v, name),
        None => {
            if v.firmware != Firmware::Tracker {
                v.text(
                    MavSeverity::MAV_SEVERITY_WARNING,
                    &format!("No such mode {number}"),
                );
            }
            MavResult::MAV_RESULT_FAILED
        }
    }
}

/// Modes in pymavlink's table that the firmware's mode_from_mode_num has no object for.
fn firmware_lacks_mode(fw: Firmware, name: &str) -> bool {
    // ArduRover dropped LEARNING
    fw == Firmware::Rover && name == "LEARNING"
}

/// A mode change asked for by the GCS. Refusals are MAV_RESULT_FAILED, as
/// GCS_MAVLINK::_set_mode_common returns when the vehicle's set_mode fails; ArduRover (and
/// ArduSub, with the mode's name) say so as well: "Flight mode change failed".
fn request_mode(v: &mut Vehicle, name: &'static str) -> MavResult {
    let Some(name) = v
        .behavior
        .mode_number(name)
        .and_then(|n| v.behavior.mode_name(n))
    else {
        return MavResult::MAV_RESULT_FAILED;
    };
    if !v.behavior.accepts_mode(name) {
        return refused(v, name); // e.g. a drifter has no propulsion
    }
    if name == "AUTO" && !v.mission.present() {
        match v.firmware {
            Firmware::Rover => {
                v.text(
                    MavSeverity::MAV_SEVERITY_NOTICE,
                    "No Mission. Can't set AUTO.",
                );
                return refused(v, name);
            }
            Firmware::Sub => return refused(v, name),
            Firmware::Copter => return MavResult::MAV_RESULT_FAILED,
            // ArduPlane takes AUTO with no mission and finishes it at once (RTL)
            _ => {}
        }
    }
    v.set_mode(name);
    MavResult::MAV_RESULT_ACCEPTED
}

/// A mode whose entry failed: Rover's set_mode says "Flight mode change failed", ArduSub's
/// "Flight mode change failed <Name>".
fn refused(v: &mut Vehicle, name: &str) -> MavResult {
    let text = match v.firmware {
        Firmware::Rover => Some("Flight mode change failed".to_string()),
        Firmware::Sub => Some(format!("Flight mode change failed {}", sub_mode_name(name))),
        _ => None,
    };
    if let Some(text) = text {
        v.text(MavSeverity::MAV_SEVERITY_WARNING, &text);
    }
    MavResult::MAV_RESULT_FAILED
}

/// ArduSub's Mode::name().
fn sub_mode_name(name: &str) -> &str {
    match name {
        "MANUAL" => "Manual",
        "ACRO" => "Acro",
        "STABILIZE" => "Stabilize",
        "ALT_HOLD" => "Depth Hold",
        "GUIDED" => "Guided",
        "AUTO" => "Auto",
        "POSHOLD" => "Position Hold",
        "CIRCLE" => "Circle",
        "SURFACE" => "Surface",
        n => n,
    }
}

/// SET_MESSAGE_INTERVAL (GCS_MAVLINK::set_message_interval): param2 in us; -1 stops, 0 the
/// default rate.
fn set_interval(v: &mut Vehicle, c: &Cmd) -> MavResult {
    if c.p[2] != 0.0 {
        return MavResult::MAV_RESULT_DENIED; // response target: not supported by ArduPilot
    }
    let id = c.p[0] as u32;
    if !known(id) {
        v.text(
            MavSeverity::MAV_SEVERITY_INFO,
            &format!("No ap_message for mavlink id ({id})"),
        );
        return MavResult::MAV_RESULT_DENIED;
    }
    let us = c.p[1] as i64;
    let ms = match us {
        0 => default_interval_ms(id),
        -1 => 0,
        i64::MIN..=-2 => return MavResult::MAV_RESULT_DENIED,
        1..=999 => 1,
        _ => (us.min(60_000_000) / 1000) as u32,
    };
    let now_ms = v.time_boot_ms();
    v.streams.set(id, ms, now_ms);
    MavResult::MAV_RESULT_ACCEPTED
}

fn get_interval(v: &mut Vehicle, id: u32) -> MavResult {
    if !known(id) {
        v.link.send(&telemetry::message_interval(id, 0));
        return MavResult::MAV_RESULT_FAILED;
    }
    let ms = v.streams.interval_ms(id);
    let us = if ms == 0 { -1 } else { ms as i32 * 1000 };
    v.link.send(&telemetry::message_interval(id, us));
    MavResult::MAV_RESULT_ACCEPTED
}

// ---- the mission protocol ----

fn supported_type(t: MavMissionType) -> bool {
    matches!(
        t,
        MavMissionType::MAV_MISSION_TYPE_MISSION
            | MavMissionType::MAV_MISSION_TYPE_FENCE
            | MavMissionType::MAV_MISSION_TYPE_RALLY
    )
}

fn mission_ack(v: &mut Vehicle, r: MavMissionResult, ty: MavMissionType, to: (u8, u8)) {
    v.link.send(&messages::mission_ack_to(r, ty, to));
}

/// An upload from someone else is in progress: refuse (MissionItemProtocol::cancel_upload).
fn busy(v: &Vehicle, from: (u8, u8), ty: MavMissionType) -> bool {
    v.mission
        .upload
        .as_ref()
        .is_some_and(|u| u.mission_type == ty && u.from != from)
}

fn mission_count(v: &mut Vehicle, from: (u8, u8), m: &MISSION_COUNT_DATA) {
    let ty = m.mission_type;
    if !v.behavior.accepts_missions() || !supported_type(ty) {
        return mission_ack(v, MavMissionResult::MAV_MISSION_UNSUPPORTED, ty, from);
    }
    if busy(v, from, ty) {
        return mission_ack(v, MavMissionResult::MAV_MISSION_DENIED, ty, from);
    }
    let now = v.now();
    v.mission.upload = Some(Upload {
        mission_type: ty,
        count: m.count,
        items: Vec::with_capacity(usize::from(m.count)),
        from,
        last_request: now,
        last_receive: now,
    });
    if m.count == 0 {
        complete_upload(v);
    } else {
        request_next(v);
    }
}

fn mission_item(v: &mut Vehicle, from: (u8, u8), item: MISSION_ITEM_INT_DATA) {
    let Some(up) = &v.mission.upload else { return };
    if item.mission_type != up.mission_type {
        return;
    }
    let ty = up.mission_type;
    if item.seq != up.next() {
        return mission_ack(v, MavMissionResult::MAV_MISSION_INVALID_SEQUENCE, ty, from);
    }
    if from != up.from {
        return mission_ack(v, MavMissionResult::MAV_MISSION_DENIED, ty, from);
    }
    let r = Mission::validate(ty, &item, up.count);
    if r != MavMissionResult::MAV_MISSION_ACCEPTED {
        v.mission.upload = None;
        return mission_ack(v, r, ty, from);
    }
    let now = v.now();
    let Some(up) = &mut v.mission.upload else {
        return;
    };
    up.items.push(item);
    up.last_receive = now;
    if up.items.len() >= usize::from(up.count) {
        complete_upload(v);
    } else {
        request_next(v);
    }
}

fn complete_upload(v: &mut Vehicle) {
    let Some(up) = v.mission.upload.take() else {
        return;
    };
    let ty = up.mission_type;
    let n = up.items.len();
    v.mission.store(ty, up.items);
    match ty {
        MavMissionType::MAV_MISSION_TYPE_MISSION => {
            v.params.update("MIS_TOTAL", n as f32);
            v.text(MavSeverity::MAV_SEVERITY_INFO, "Flight plan received");
        }
        MavMissionType::MAV_MISSION_TYPE_RALLY => v.params.update("RALLY_TOTAL", n as f32),
        _ => {}
    }
    mission_ack(v, MavMissionResult::MAV_MISSION_ACCEPTED, ty, up.from);
}

/// Ask for the next item. ArduPilot asks with MISSION_REQUEST (queued_request_send) and
/// takes MISSION_ITEM_INT in reply.
#[allow(deprecated)]
fn request_next(v: &mut Vehicle) {
    let now = v.now();
    let Some(up) = &mut v.mission.upload else {
        return;
    };
    up.last_request = now;
    let msg = MavMessage::MISSION_REQUEST(MISSION_REQUEST_DATA {
        seq: up.next(),
        target_system: up.from.0,
        target_component: up.from.1,
        mission_type: up.mission_type,
    });
    v.link.send(&msg);
}

/// Each tick: ask again for a lost item after 1 s, give up 8 s after the last one.
pub fn upload_tick(v: &mut Vehicle, now: f64) {
    let Some(up) = &v.mission.upload else { return };
    if now - up.last_receive > UPLOAD_TIMEOUT_S {
        let (ty, from) = (up.mission_type, up.from);
        v.mission.upload = None;
        let what = match ty {
            MavMissionType::MAV_MISSION_TYPE_FENCE => "Fence",
            MavMissionType::MAV_MISSION_TYPE_RALLY => "Rally",
            _ => "Mission",
        };
        v.text(
            MavSeverity::MAV_SEVERITY_WARNING,
            &format!("{what} upload timeout"),
        );
        mission_ack(
            v,
            MavMissionResult::MAV_MISSION_OPERATION_CANCELLED,
            ty,
            from,
        );
    } else if now - up.last_request > UPLOAD_RETRY_S {
        request_next(v);
    }
}

fn request_list(v: &mut Vehicle, from: (u8, u8), ty: MavMissionType) {
    if v.mission
        .upload
        .as_ref()
        .is_some_and(|u| u.mission_type == ty)
    {
        return mission_ack(v, MavMissionResult::MAV_MISSION_DENIED, ty, from);
    }
    let Some(count) = v.mission.download_count(ty) else {
        return mission_ack(v, MavMissionResult::MAV_MISSION_UNSUPPORTED, ty, from);
    };
    v.link.send(&MavMessage::MISSION_COUNT(MISSION_COUNT_DATA {
        count,
        target_system: from.0,
        target_component: from.1,
        mission_type: ty,
        opaque_id: 0,
    }));
}

#[allow(deprecated)]
fn request_item(v: &mut Vehicle, from: (u8, u8), ty: MavMissionType, seq: u16, int: bool) {
    if v.mission
        .upload
        .as_ref()
        .is_some_and(|u| u.mission_type == ty)
    {
        return mission_ack(v, MavMissionResult::MAV_MISSION_DENIED, ty, from);
    }
    let Some(mut it) = v.mission.download_item(ty, seq, v.st.home) else {
        if v.mission.download_count(ty).is_none() {
            return mission_ack(v, MavMissionResult::MAV_MISSION_UNSUPPORTED, ty, from);
        }
        // educate the GCS on the real size, then refuse
        request_list(v, from, ty);
        return mission_ack(v, MavMissionResult::MAV_MISSION_INVALID_SEQUENCE, ty, from);
    };
    it.target_system = from.0;
    it.target_component = from.1;
    if int {
        v.link.send(&MavMessage::MISSION_ITEM_INT(it));
        return;
    }
    if !v.mission_request_warned {
        v.mission_request_warned = true;
        v.text(
            MavSeverity::MAV_SEVERITY_WARNING,
            "got MISSION_REQUEST; use MISSION_REQUEST_INT!",
        );
    }
    let has_location = it.x != 0 || it.y != 0;
    let deg = |v: i32| {
        if has_location {
            v as f32 / 1e7
        } else {
            v as f32
        }
    };
    v.link.send(&MavMessage::MISSION_ITEM(MISSION_ITEM_DATA {
        param1: it.param1,
        param2: it.param2,
        param3: it.param3,
        param4: it.param4,
        x: deg(it.x),
        y: deg(it.y),
        z: it.z,
        seq: it.seq,
        command: it.command,
        target_system: it.target_system,
        target_component: it.target_component,
        frame: it.frame,
        current: it.current,
        autocontinue: it.autocontinue,
        mission_type: it.mission_type,
    }));
}

fn clear_all(v: &mut Vehicle, from: (u8, u8), ty: MavMissionType) {
    if busy(v, from, ty) {
        return mission_ack(v, MavMissionResult::MAV_MISSION_DENIED, ty, from);
    }
    if v.mission.upload.as_ref().is_some_and(|u| u.from == from) {
        v.mission.upload = None;
    }
    v.mission.clear(ty);
    if matches!(
        ty,
        MavMissionType::MAV_MISSION_TYPE_MISSION | MavMissionType::MAV_MISSION_TYPE_ALL
    ) {
        v.params.update("MIS_TOTAL", 0.0);
    }
    mission_ack(v, MavMissionResult::MAV_MISSION_ACCEPTED, ty, from);
}

/// MISSION_ITEM (float degrees) as MISSION_ITEM_INT.
#[allow(deprecated)]
fn item_int(m: &MISSION_ITEM_DATA) -> MISSION_ITEM_INT_DATA {
    let located = crate::mission::is_nav(m.command);
    let fixed = |d: f32| {
        if located {
            (f64::from(d) * 1e7).round() as i32
        } else {
            d as i32
        }
    };
    MISSION_ITEM_INT_DATA {
        param1: m.param1,
        param2: m.param2,
        param3: m.param3,
        param4: m.param4,
        x: fixed(m.x),
        y: fixed(m.y),
        z: m.z,
        seq: m.seq,
        command: m.command,
        target_system: m.target_system,
        target_component: m.target_component,
        frame: m.frame,
        current: m.current,
        autocontinue: m.autocontinue,
        mission_type: m.mission_type,
    }
}
