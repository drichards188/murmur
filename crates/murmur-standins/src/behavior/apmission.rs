//! What ArduPilot's AP_Mission says while it runs a mission, for every behavior that imitates
//! it: the command type names (AP_Mission::Mission_Command::type), "Mission: 2 WP" when a
//! command starts and "Mission: 5 Jump 2/unlimited" when a DO_JUMP runs again (the first pass
//! is silent). One place for the texts; the walkers (`auto::Auto` for copter and VTOL, the
//! rover and blimp ones) only decide when to say them.

use std::collections::HashMap;

use mavlink::dialects::ardupilotmega::{MavCmd, MavSeverity};

use crate::kinematics::Out;

/// AP_Mission::Mission_Command::type() for the commands a mission is likely to hold
/// ("?" for what ArduPilot has no name for).
pub fn type_name(cmd: MavCmd) -> &'static str {
    use MavCmd::*;
    match cmd {
        MAV_CMD_NAV_WAYPOINT => "WP",
        MAV_CMD_NAV_SPLINE_WAYPOINT => "SplineWP",
        MAV_CMD_NAV_RETURN_TO_LAUNCH => "RTL",
        MAV_CMD_NAV_LOITER_UNLIM => "LoitUnlim",
        MAV_CMD_NAV_LOITER_TIME => "LoitTime",
        MAV_CMD_NAV_LOITER_TURNS => "LoitTurns",
        MAV_CMD_NAV_LOITER_TO_ALT => "LoitAltitude",
        MAV_CMD_NAV_GUIDED_ENABLE => "GuidedEnable",
        MAV_CMD_NAV_TAKEOFF => "Takeoff",
        MAV_CMD_NAV_LAND => "Land",
        MAV_CMD_NAV_CONTINUE_AND_CHANGE_ALT => "ContinueAndChangeAlt",
        MAV_CMD_NAV_VTOL_TAKEOFF => "VTOLTakeoff",
        MAV_CMD_NAV_VTOL_LAND => "VTOLLand",
        MAV_CMD_NAV_DELAY => "Delay",
        MAV_CMD_DO_CHANGE_SPEED => "ChangeSpeed",
        MAV_CMD_DO_SET_HOME => "SetHome",
        MAV_CMD_DO_JUMP => "Jump",
        MAV_CMD_DO_LAND_START => "LandStart",
        MAV_CMD_DO_VTOL_TRANSITION => "VTOLTransition",
        MAV_CMD_CONDITION_DELAY => "CondDelay",
        MAV_CMD_CONDITION_YAW => "CondYaw",
        _ => "?",
    }
}

/// "Mission: <seq> <type>", said when AP_Mission starts a command.
pub fn started(seq: u16, cmd: MavCmd, out: &mut Vec<Out>) {
    out.push(Out::Text(
        MavSeverity::MAV_SEVERITY_INFO,
        format!("Mission: {seq} {}", type_name(cmd)),
    ));
}

/// The text of a DO_JUMP at `seq` running for the `run`th time of `times` (negative = forever).
pub fn jump_text(seq: u16, run: i32, times: i32) -> String {
    if times < 0 {
        format!("Mission: {seq} Jump {run}/unlimited")
    } else {
        format!("Mission: {seq} Jump {run}/{times}")
    }
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
            out.push(Out::Text(
                MavSeverity::MAV_SEVERITY_INFO,
                jump_text(seq, runs, times),
            ));
        }
    }
}
