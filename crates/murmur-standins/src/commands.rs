//! What a stand-in does with what the GCS sends: TIMESYNC, parameters, COMMAND_LONG, and the
//! mission upload protocol. Behavior-specific refusals come from the `Behavior` trait.

use std::time::Instant;

use mavlink::dialects::ardupilotmega::*;

use crate::behavior::Ctx;
use crate::messages;
use crate::mission::Upload;
use crate::telemetry::request_item;
use crate::vehicle::Vehicle;

// MISSION_SET_CURRENT is deprecated but still what older GCSs send
#[allow(deprecated)]
pub fn handle(v: &mut Vehicle, msg: &MavMessage) {
    if let Some(target) = target_system(msg)
        && target != 0
        && target != v.sysid()
    {
        return;
    }
    match msg {
        MavMessage::TIMESYNC(m) => {
            if m.tc1 == 0 {
                let now_ns = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0);
                let reply = TIMESYNC_DATA {
                    tc1: now_ns,
                    ts1: m.ts1,
                    ..Default::default()
                };
                v.link.send(&MavMessage::TIMESYNC(reply));
            }
        }
        MavMessage::PARAM_REQUEST_READ(m) => {
            // no parameter store: every read is 0 (e.g. Q_ENABLE: stand-in planes are not
            // QuadPlanes) unless the behavior reports the parameter
            let name = m.param_id.to_str().unwrap_or("");
            let value = v.behavior.param(name).unwrap_or(0.0);
            let reply =
                messages::param_value(m.param_id, value, MavParamType::MAV_PARAM_TYPE_REAL32);
            v.link.send(&reply);
        }
        MavMessage::PARAM_SET(m) => {
            v.link.send(&messages::param_value(
                m.param_id,
                m.param_value,
                m.param_type,
            ));
        }
        MavMessage::COMMAND_LONG(m) => command_long(v, m),
        MavMessage::MISSION_SET_CURRENT(m) => v.mission.seq = m.seq,
        MavMessage::MISSION_COUNT(m) => mission_count(v, m),
        MavMessage::MISSION_ITEM_INT(m) => mission_item_int(v, m),
        _ => {}
    }
}

#[allow(deprecated)]
fn target_system(msg: &MavMessage) -> Option<u8> {
    Some(match msg {
        MavMessage::TIMESYNC(m) => m.target_system,
        MavMessage::PARAM_REQUEST_READ(m) => m.target_system,
        MavMessage::PARAM_SET(m) => m.target_system,
        MavMessage::COMMAND_LONG(m) => m.target_system,
        MavMessage::MISSION_SET_CURRENT(m) => m.target_system,
        MavMessage::MISSION_COUNT(m) => m.target_system,
        MavMessage::MISSION_ITEM_INT(m) => m.target_system,
        _ => return None,
    })
}

fn ack(v: &mut Vehicle, command: MavCmd, result: MavResult) {
    v.link.send(&messages::command_ack(command, result));
}

fn command_long(v: &mut Vehicle, m: &COMMAND_LONG_DATA) {
    let c = m.command;
    let mut out = Vec::new();
    let own = v.behavior.command(
        &mut Ctx {
            spec: &v.cfg.spec,
            st: &mut v.st,
            mission: &mut v.mission,
            out: &mut out,
        },
        m,
    );
    for o in out {
        v.link.send(&messages::out(o));
    }
    if let Some(result) = own {
        return ack(v, c, result);
    }
    match c {
        MavCmd::MAV_CMD_COMPONENT_ARM_DISARM => {
            if m.param1 == 0.0 && v.airborne() {
                return ack(v, c, MavResult::MAV_RESULT_DENIED); // no disarming in flight
            }
            v.st.armed = m.param1 == 1.0;
            let text = if v.st.armed {
                "Arming motors"
            } else {
                "Disarming motors"
            };
            v.text(MavSeverity::MAV_SEVERITY_INFO, text);
            ack(v, c, MavResult::MAV_RESULT_ACCEPTED);
        }
        MavCmd::MAV_CMD_DO_SET_MODE => {
            let number = m.param2.is_finite().then(|| m.param2.trunc() as i64);
            let name = number
                .and_then(|n| u32::try_from(n).ok())
                .and_then(|n| v.behavior.mode_name(n));
            let Some(name) = name else {
                return ack(v, c, MavResult::MAV_RESULT_UNSUPPORTED);
            };
            if !v.behavior.accepts_mode(name) {
                return ack(v, c, MavResult::MAV_RESULT_UNSUPPORTED); // e.g. a drifter has no propulsion
            }
            v.set_mode(name);
            ack(v, c, MavResult::MAV_RESULT_ACCEPTED);
        }
        MavCmd::MAV_CMD_DO_SET_MISSION_CURRENT => {
            v.mission.seq = m.param1 as u16;
            ack(v, c, MavResult::MAV_RESULT_ACCEPTED);
        }
        MavCmd::MAV_CMD_SET_MESSAGE_INTERVAL | MavCmd::MAV_CMD_REQUEST_MESSAGE => {
            ack(v, c, MavResult::MAV_RESULT_ACCEPTED);
        }
        _ => ack(v, c, MavResult::MAV_RESULT_UNSUPPORTED),
    }
}

fn mission_count(v: &mut Vehicle, m: &MISSION_COUNT_DATA) {
    if m.mission_type != MavMissionType::MAV_MISSION_TYPE_MISSION {
        let reply =
            messages::mission_ack(MavMissionResult::MAV_MISSION_UNSUPPORTED, m.mission_type);
        return v.link.send(&reply);
    }
    if !v.behavior.accepts_missions() {
        let reply = messages::mission_ack(
            MavMissionResult::MAV_MISSION_UNSUPPORTED,
            MavMissionType::MAV_MISSION_TYPE_MISSION,
        );
        return v.link.send(&reply);
    }
    if m.count == 0 {
        // clear, as ArduPilot does (standins.py has no case for it and crashes)
        v.mission.upload = None;
        v.mission.install(Vec::new());
        let reply = messages::mission_ack(
            MavMissionResult::MAV_MISSION_ACCEPTED,
            MavMissionType::MAV_MISSION_TYPE_MISSION,
        );
        return v.link.send(&reply);
    }
    v.mission.upload = Some(Upload {
        items: vec![None; m.count as usize],
        last_request: Instant::now(),
    });
    request_item(v, 0);
}

fn mission_item_int(v: &mut Vehicle, m: &MISSION_ITEM_INT_DATA) {
    let Some(up) = &mut v.mission.upload else {
        return;
    };
    let Some(slot) = up.items.get_mut(m.seq as usize) else {
        return;
    };
    *slot = Some(m.clone());
    match up.next_missing() {
        Some(seq) => request_item(v, seq),
        None => {
            let items: Vec<_> = v
                .mission
                .upload
                .take()
                .into_iter()
                .flat_map(|u| u.items)
                .flatten()
                .collect();
            let n = items.len();
            v.mission.install(items);
            let reply = messages::mission_ack(
                MavMissionResult::MAV_MISSION_ACCEPTED,
                MavMissionType::MAV_MISSION_TYPE_MISSION,
            );
            v.link.send(&reply);
            v.text(
                MavSeverity::MAV_SEVERITY_INFO,
                &format!("Mission: {} WP", n as i64 - 1),
            );
        }
    }
}
