//! What ArduPilot's AP_Mission says while it runs a mission, for behaviors that imitate it on
//! top of the shared mission store: "Mission: 2 WP" when it starts a command and
//! "Mission: 5 Jump 2/unlimited" when a DO_JUMP runs again (the first pass is silent).

use std::collections::HashMap;

use mavlink::dialects::ardupilotmega::{MavCmd, MavSeverity};

use crate::kinematics::Out;

/// AP_Mission's Mission_Command::type() for the commands a mission is likely to hold.
pub fn command_name(cmd: MavCmd) -> &'static str {
    use MavCmd::*;
    match cmd {
        MAV_CMD_NAV_WAYPOINT => "WP",
        MAV_CMD_NAV_SPLINE_WAYPOINT => "SplineWP",
        MAV_CMD_NAV_RETURN_TO_LAUNCH => "RTL",
        MAV_CMD_NAV_LOITER_UNLIM => "LoitUnlim",
        MAV_CMD_NAV_LOITER_TIME => "LoitTime",
        MAV_CMD_NAV_LOITER_TURNS => "LoitTurns",
        MAV_CMD_NAV_TAKEOFF => "Takeoff",
        MAV_CMD_NAV_LAND => "Land",
        MAV_CMD_NAV_VTOL_TAKEOFF => "VTOLTakeoff",
        MAV_CMD_NAV_VTOL_LAND => "VTOLLand",
        MAV_CMD_DO_CHANGE_SPEED => "ChangeSpeed",
        MAV_CMD_DO_SET_HOME => "SetHome",
        MAV_CMD_CONDITION_DELAY => "CondDelay",
        _ => "?",
    }
}

pub fn started(seq: u16, cmd: MavCmd, out: &mut Vec<Out>) {
    out.push(Out::Text(
        MavSeverity::MAV_SEVERITY_INFO,
        format!("Mission: {seq} {}", command_name(cmd)),
    ));
}

/// AP_Mission's jump tracking: how often each DO_JUMP has run.
#[derive(Debug, Default)]
pub struct Jumps {
    runs: HashMap<u16, i32>,
}

impl Jumps {
    pub fn clear(&mut self) {
        self.runs.clear();
    }

    pub fn runs(&self, seq: u16) -> i32 {
        self.runs.get(&seq).copied().unwrap_or(0)
    }

    /// The DO_JUMP at `seq` (repeat `times`, negative = forever) runs once more.
    pub fn ran(&mut self, seq: u16, times: i32, out: &mut Vec<Out>) {
        let runs = self.runs(seq) + 1;
        self.runs.insert(seq, runs);
        if runs > 1 {
            let of = if times < 0 {
                "unlimited".to_string()
            } else {
                times.to_string()
            };
            out.push(Out::Text(
                MavSeverity::MAV_SEVERITY_INFO,
                format!("Mission: {seq} Jump {runs}/{of}"),
            ));
        }
    }
}
