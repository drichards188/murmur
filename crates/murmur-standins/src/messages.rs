//! Constructors for the messages a stand-in sends, field for field as standins.py encodes them.

use mavlink::dialects::ardupilotmega::*;

pub const GCS_SYSID: u8 = 255;
pub const GCS_COMPID: u8 = 190;

pub fn statustext(severity: MavSeverity, s: &str) -> MavMessage {
    // 50 bytes, cut on a char boundary
    let mut end = s.len().min(50);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    MavMessage::STATUSTEXT(STATUSTEXT_DATA {
        severity,
        text: s[..end].into(),
        ..Default::default()
    })
}

pub fn mission_current(seq: u16) -> MavMessage {
    MavMessage::MISSION_CURRENT(MISSION_CURRENT_DATA {
        seq,
        ..Default::default()
    })
}

pub fn mission_item_reached(seq: u16) -> MavMessage {
    MavMessage::MISSION_ITEM_REACHED(MISSION_ITEM_REACHED_DATA { seq })
}

pub fn command_ack(command: MavCmd, result: MavResult) -> MavMessage {
    MavMessage::COMMAND_ACK(COMMAND_ACK_DATA {
        command,
        result,
        ..Default::default()
    })
}

pub fn mission_ack(result: MavMissionResult, mission_type: MavMissionType) -> MavMessage {
    MavMessage::MISSION_ACK(MISSION_ACK_DATA {
        target_system: GCS_SYSID,
        target_component: GCS_COMPID,
        mavtype: result,
        mission_type,
        ..Default::default()
    })
}

pub fn mission_request_int(seq: u16) -> MavMessage {
    MavMessage::MISSION_REQUEST_INT(MISSION_REQUEST_INT_DATA {
        seq,
        target_system: GCS_SYSID,
        target_component: GCS_COMPID,
        mission_type: MavMissionType::MAV_MISSION_TYPE_MISSION,
    })
}

pub fn param_value(
    param_id: mavlink::types::CharArray<16>,
    value: f32,
    param_type: MavParamType,
) -> MavMessage {
    MavMessage::PARAM_VALUE(PARAM_VALUE_DATA {
        param_value: value,
        param_count: 1,
        param_index: 0,
        param_id,
        param_type,
    })
}
