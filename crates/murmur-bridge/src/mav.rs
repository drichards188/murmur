//! MAVLink plumbing: framing in and out, ArduPilot mode tables, vehicle kinds.

use mavlink::peek_reader::PeekReader;
use mavlink::{MAVLinkMessageRaw, MavHeader, MavlinkVersion, Message};
use serde::Serialize;

pub use apm::MavMessage;
pub use mavlink::dialects::ardupilotmega as apm;

pub const GCS_SYSID: u8 = 255;
pub const GCS_COMPID: u8 = 190;
pub const AUTOPILOT_COMPID: u8 = 1;

/// The bridge's name for an autopilot family: the row of the task capability table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Plane,
    Vtol,
    Copter,
    Blimp,
    Boat,
    Ugv,
    Sub,
    Tracker,
    Unknown,
}

impl Kind {
    pub fn from_mav_type(t: apm::MavType) -> Kind {
        use apm::MavType::*;
        match t {
            MAV_TYPE_FIXED_WING => Kind::Plane,
            MAV_TYPE_VTOL_TAILSITTER_DUOROTOR
            | MAV_TYPE_VTOL_TAILSITTER_QUADROTOR
            | MAV_TYPE_VTOL_TILTROTOR => Kind::Vtol,
            MAV_TYPE_QUADROTOR | MAV_TYPE_HEXAROTOR | MAV_TYPE_OCTOROTOR | MAV_TYPE_TRICOPTER
            | MAV_TYPE_COAXIAL | MAV_TYPE_HELICOPTER => Kind::Copter,
            MAV_TYPE_AIRSHIP => Kind::Blimp,
            MAV_TYPE_SURFACE_BOAT => Kind::Boat,
            MAV_TYPE_GROUND_ROVER => Kind::Ugv,
            MAV_TYPE_SUBMARINE => Kind::Sub,
            MAV_TYPE_ANTENNA_TRACKER => Kind::Tracker,
            _ => Kind::Unknown,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Plane => "plane",
            Kind::Vtol => "vtol",
            Kind::Copter => "copter",
            Kind::Blimp => "blimp",
            Kind::Boat => "boat",
            Kind::Ugv => "ugv",
            Kind::Sub => "sub",
            Kind::Tracker => "tracker",
            Kind::Unknown => "unknown",
        }
    }

    pub fn flying(self) -> bool {
        matches!(self, Kind::Plane | Kind::Vtol | Kind::Copter | Kind::Blimp)
    }

    /// Height above home that counts as "in the air".
    pub fn airborne_alt(self) -> Option<f64> {
        match self {
            Kind::Plane => Some(15.0),
            Kind::Vtol => Some(10.0),
            Kind::Copter => Some(5.0),
            Kind::Blimp => Some(3.0),
            _ => None,
        }
    }
}

#[rustfmt::skip]
const PLANE_MODES: &[(u32, &str)] = &[
    (0, "MANUAL"), (1, "CIRCLE"), (2, "STABILIZE"), (3, "TRAINING"), (4, "ACRO"), (5, "FBWA"),
    (6, "FBWB"), (7, "CRUISE"), (8, "AUTOTUNE"), (10, "AUTO"), (11, "RTL"), (12, "LOITER"),
    (13, "TAKEOFF"), (14, "AVOID_ADSB"), (15, "GUIDED"), (16, "INITIALISING"), (17, "QSTABILIZE"),
    (18, "QHOVER"), (19, "QLOITER"), (20, "QLAND"), (21, "QRTL"), (22, "QAUTOTUNE"), (23, "QACRO"),
    (24, "THERMAL"), (25, "LOITERALTQLAND"), (26, "AUTOLAND"),
];
#[rustfmt::skip]
const COPTER_MODES: &[(u32, &str)] = &[
    (0, "STABILIZE"), (1, "ACRO"), (2, "ALT_HOLD"), (3, "AUTO"), (4, "GUIDED"), (5, "LOITER"),
    (6, "RTL"), (7, "CIRCLE"), (8, "POSITION"), (9, "LAND"), (10, "OF_LOITER"), (11, "DRIFT"),
    (13, "SPORT"), (14, "FLIP"), (15, "AUTOTUNE"), (16, "POSHOLD"), (17, "BRAKE"), (18, "THROW"),
    (19, "AVOID_ADSB"), (20, "GUIDED_NOGPS"), (21, "SMART_RTL"), (22, "FLOWHOLD"), (23, "FOLLOW"),
    (24, "ZIGZAG"), (25, "SYSTEMID"), (26, "AUTOROTATE"), (27, "AUTO_RTL"), (28, "TURTLE"),
    (29, "RATE_ACRO"),
];
#[rustfmt::skip]
const ROVER_MODES: &[(u32, &str)] = &[
    (0, "MANUAL"), (1, "ACRO"), (2, "LEARNING"), (3, "STEERING"), (4, "HOLD"), (5, "LOITER"),
    (6, "FOLLOW"), (7, "SIMPLE"), (8, "DOCK"), (9, "CIRCLE"), (10, "AUTO"), (11, "RTL"),
    (12, "SMART_RTL"), (15, "GUIDED"), (16, "INITIALISING"),
];
#[rustfmt::skip]
const SUB_MODES: &[(u32, &str)] = &[
    (0, "STABILIZE"), (1, "ACRO"), (2, "ALT_HOLD"), (3, "AUTO"), (4, "GUIDED"), (7, "CIRCLE"),
    (9, "SURFACE"), (16, "POSHOLD"), (19, "MANUAL"),
];
#[rustfmt::skip]
const TRACKER_MODES: &[(u32, &str)] = &[
    (0, "MANUAL"), (1, "STOP"), (2, "SCAN"), (4, "GUIDED"), (10, "AUTO"), (16, "INITIALISING"),
];
#[rustfmt::skip]
const BLIMP_MODES: &[(u32, &str)] = &[
    (0, "LAND"), (1, "MANUAL"), (2, "VELOCITY"), (3, "LOITER"), (4, "RTL"), (5, "AUTO"), (6, "HOLD"),
];

/// ArduPilot's mode numbering per MAV_TYPE (pymavlink's AP_MAV_TYPE_MODE_MAP).
pub fn mode_table(t: apm::MavType) -> Option<&'static [(u32, &'static str)]> {
    use apm::MavType::*;
    Some(match t {
        MAV_TYPE_HELICOPTER | MAV_TYPE_TRICOPTER | MAV_TYPE_QUADROTOR | MAV_TYPE_HEXAROTOR
        | MAV_TYPE_OCTOROTOR | MAV_TYPE_DECAROTOR | MAV_TYPE_DODECAROTOR | MAV_TYPE_COAXIAL => {
            COPTER_MODES
        }
        MAV_TYPE_FIXED_WING
        | MAV_TYPE_VTOL_TILTROTOR
        | MAV_TYPE_VTOL_TAILSITTER_DUOROTOR
        | MAV_TYPE_VTOL_TAILSITTER_QUADROTOR => PLANE_MODES,
        MAV_TYPE_GROUND_ROVER | MAV_TYPE_SURFACE_BOAT => ROVER_MODES,
        MAV_TYPE_ANTENNA_TRACKER => TRACKER_MODES,
        MAV_TYPE_SUBMARINE => SUB_MODES,
        MAV_TYPE_AIRSHIP => BLIMP_MODES,
        _ => return None,
    })
}

pub fn mode_name(t: apm::MavType, custom_mode: u32) -> String {
    mode_table(t)
        .and_then(|tab| tab.iter().find(|(n, _)| *n == custom_mode))
        .map_or_else(|| custom_mode.to_string(), |(_, name)| (*name).to_string())
}

pub fn mode_number(t: apm::MavType, name: &str) -> Option<u32> {
    mode_table(t)?
        .iter()
        .find(|(_, n)| *n == name)
        .map(|(num, _)| *num)
}

/// One message out of a datagram. `raw` is the frame as received, for relaying unchanged.
pub struct Frame {
    pub sysid: u8,
    pub compid: u8,
    pub seq: u8,
    pub msg: MavMessage,
    pub raw: Vec<u8>,
}

/// Every valid MAVLink 1 or 2 frame in a datagram; garbage and bad CRCs are skipped.
pub fn decode(datagram: &[u8]) -> Vec<Frame> {
    let mut reader = PeekReader::<_, 280>::new(datagram);
    let mut out = Vec::new();
    while let Ok(raw) = mavlink::read_any_raw_message::<MavMessage, _>(&mut reader) {
        let (version, bytes) = match &raw {
            MAVLinkMessageRaw::V1(m) => (MavlinkVersion::V1, m.raw_bytes()),
            MAVLinkMessageRaw::V2(m) => (MavlinkVersion::V2, m.raw_bytes()),
        };
        // a message outside the dialect or with an out-of-range enum is dropped, like
        // pymavlink's BAD_DATA
        if let Ok(msg) = MavMessage::parse(version, raw.message_id(), raw.payload()) {
            out.push(Frame {
                sysid: raw.system_id(),
                compid: raw.component_id(),
                seq: raw.sequence(),
                msg,
                raw: bytes.to_vec(),
            });
        }
    }
    out
}

/// A MAVLink 2 frame from the GCS identity.
pub fn encode(msg: &MavMessage, seq: u8) -> Vec<u8> {
    encode_as(msg, GCS_SYSID, GCS_COMPID, seq)
}

pub fn encode_as(msg: &MavMessage, sysid: u8, compid: u8, seq: u8) -> Vec<u8> {
    let mut raw = mavlink::MAVLinkV2MessageRaw::new();
    raw.serialize_message(
        MavHeader {
            system_id: sysid,
            component_id: compid,
            sequence: seq,
        },
        msg,
    );
    raw.raw_bytes().to_vec()
}

/// A fixed-size MAVLink string field (param ids, status text).
pub fn char_array<const N: usize>(s: &str) -> mavlink::types::CharArray<N> {
    s.into()
}

pub fn chars_to_string<const N: usize>(c: &mavlink::types::CharArray<N>) -> String {
    // not to_str(): a status text cut mid-character should still show
    let bytes: &[u8; N] = c;
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(N);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// `MAV_CMD_DO_SET_MODE` etc.: the enum's own name, as pymavlink prints it.
pub fn cmd_name(c: apm::MavCmd) -> String {
    format!("{c:?}")
}

pub fn command_long(target: u8, command: apm::MavCmd, confirmation: u8, p: [f32; 7]) -> MavMessage {
    MavMessage::COMMAND_LONG(apm::COMMAND_LONG_DATA {
        param1: p[0],
        param2: p[1],
        param3: p[2],
        param4: p[3],
        param5: p[4],
        param6: p[5],
        param7: p[6],
        command,
        target_system: target,
        target_component: AUTOPILOT_COMPID,
        confirmation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_skip_garbage() {
        let hb = MavMessage::HEARTBEAT(apm::HEARTBEAT_DATA {
            custom_mode: 10,
            mavtype: apm::MavType::MAV_TYPE_FIXED_WING,
            autopilot: apm::MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA,
            base_mode: apm::MavModeFlag::MAV_MODE_FLAG_SAFETY_ARMED,
            system_status: apm::MavState::MAV_STATE_ACTIVE,
            mavlink_version: 3,
        });
        let mut buf = vec![0x00, 0x13, 0xfd, 0x01];
        buf.extend(encode_as(&hb, 7, 1, 42));
        buf.extend(encode_as(&hb, 7, 1, 43));
        let frames = decode(&buf);
        assert_eq!(frames.len(), 2);
        assert_eq!(
            (frames[0].sysid, frames[0].compid, frames[0].seq),
            (7, 1, 42)
        );
        assert_eq!(frames[1].seq, 43);
        assert_eq!(decode(&frames[0].raw).len(), 1);
    }

    #[test]
    fn modes() {
        assert_eq!(mode_name(apm::MavType::MAV_TYPE_FIXED_WING, 10), "AUTO");
        assert_eq!(mode_name(apm::MavType::MAV_TYPE_SUBMARINE, 5), "5");
        assert_eq!(
            mode_number(apm::MavType::MAV_TYPE_ANTENNA_TRACKER, "STOP"),
            Some(1)
        );
        assert_eq!(mode_number(apm::MavType::MAV_TYPE_SUBMARINE, "RTL"), None);
    }
}
