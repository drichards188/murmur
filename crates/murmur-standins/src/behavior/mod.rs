//! Behaviors: which ArduPilot autopilot a stand-in imitates (MAV_TYPE, mode table) and how it
//! moves. Adding one = a file here plus a line in `create`.
//!
//! What every autopilot shares (parameters, telemetry rates, the mission protocol, failsafes,
//! battery, navigation error) lives outside the behaviors and keys off `Behavior::firmware`,
//! which defaults from the mode table, so a new behavior gets the right ArduPilot semantics
//! by picking its `modes::` table.

use mavlink::dialects::ardupilotmega::{
    GpsFixType, MavCmd, MavParamType, MavResult, MavState, MavType,
};

use crate::conditions::Sample;
use crate::config::Spec;
use crate::geo::Gauss;
use crate::kinematics::{Out, State};
use crate::mission::Mission;
use crate::params::Params;

mod apmission;
mod auto;
mod blimp;
mod boat;
mod copter;
mod drifter;
mod marine;
pub mod modes;
mod plane;
mod rover;
mod sailboat;
mod sub;
mod ugv;
mod vtol;

/// Everything a physics tick may read or change.
pub struct Tick<'a> {
    pub spec: &'a Spec,
    /// the vehicle's parameter store (ArduPilot names; see params.rs)
    pub params: &'a Params,
    pub st: &'a mut State,
    pub mission: &'a mut Mission,
    /// latest tidewater sample at (about) this position, if conditions are on
    pub water: Option<&'a Sample>,
    pub rng: &'a mut Gauss,
    pub out: &'a mut Vec<Out>,
    pub dt: f64,
}

/// What a behavior's own command handling may read or change.
pub struct Ctx<'a> {
    pub spec: &'a Spec,
    pub params: &'a Params,
    pub st: &'a mut State,
    pub mission: &'a mut Mission,
    /// sent before the COMMAND_ACK, in order
    pub out: &'a mut Vec<Out>,
}

/// A COMMAND_LONG or COMMAND_INT: params 1-4, then x / y (degrees for COMMAND_INT) and z.
#[derive(Debug, Clone, Copy)]
pub struct Command {
    pub command: MavCmd,
    pub p: [f32; 4],
    pub x: f64,
    pub y: f64,
    pub z: f32,
}

/// The ArduPilot firmware a behavior imitates. Picks the parameter table, the failsafe
/// actions, mission-item support and what happens when a mission ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Firmware {
    Plane,
    Rover,
    Sub,
    Copter,
    Blimp,
    Tracker,
}

impl Firmware {
    /// Which firmware a mode table belongs to (each firmware numbers its modes differently).
    pub fn from_modes(table: &'static [(u32, &'static str)]) -> Firmware {
        // by content: a const's address isn't stable across uses
        let is = |t: &'static [(u32, &'static str)]| table == t;
        if is(modes::PLANE) {
            Firmware::Plane
        } else if is(modes::SUB) {
            Firmware::Sub
        } else if is(modes::COPTER) {
            Firmware::Copter
        } else if is(modes::BLIMP) {
            Firmware::Blimp
        } else if is(modes::TRACKER) {
            Firmware::Tracker
        } else {
            Firmware::Rover
        }
    }
}

pub trait Behavior: Send {
    fn mav_type(&self) -> MavType;

    /// ArduPilot custom_mode number -> name, as pymavlink's mode_mapping_bynumber.
    fn modes(&self) -> &'static [(u32, &'static str)];

    /// The mode for "hold position", and the passive default when there is no MANUAL.
    fn hold_mode(&self) -> &'static str;

    /// Called once at spawn, after the common defaults (home = spawn point, disarmed, MANUAL).
    fn spawn(&self, _spec: &Spec, _st: &mut State) {}

    fn airborne(&self, _st: &State) -> bool {
        false
    }

    /// Whether DO_SET_MODE to this (known) mode is accepted.
    fn accepts_mode(&self, _name: &str) -> bool {
        true
    }

    fn accepts_missions(&self) -> bool {
        true
    }

    /// Why arming is refused in the current state, as the autopilot's arming checks word it
    /// ("Mode not armable"); None = allowed. The shared arm command prefixes it "Arm: ".
    fn arming_refusal(&self, _st: &State) -> Option<&'static str> {
        None
    }

    /// Whether the behavior gets tidewater conditions: currents, depth and grounding for
    /// boats and subs, elevation for a ground vehicle, wind for a sailboat or a blimp.
    fn wants_conditions(&self) -> bool {
        false
    }

    /// Deprecated name of `wants_conditions`, kept so code written against it still builds;
    /// the engine asks `wants_conditions`, so an override of this one has no effect.
    fn marine(&self) -> bool {
        self.wants_conditions()
    }

    /// GPS fix type and satellites in view.
    fn gps(&self, _st: &State) -> (GpsFixType, u8) {
        (GpsFixType::GPS_FIX_TYPE_3D_FIX, 14)
    }

    fn step(&mut self, t: &mut Tick);

    /// A command this autopilot handles itself (its arming checks, mode refusals, user
    /// takeoff): Some(result) to ACK with, None for the shared handling in `commands`.
    fn command(&mut self, _c: &mut Ctx, _cmd: &Command) -> Option<MavResult> {
        None
    }

    /// Which ArduPilot firmware this imitates. Default: from the mode table.
    fn firmware(&self) -> Firmware {
        Firmware::from_modes(self.modes())
    }

    /// Parameters on top of the firmware's table (params.rs): (name, type, value). A name
    /// already in the table takes this value.
    fn extra_params(&self, _spec: &Spec) -> Vec<(&'static str, MavParamType, f32)> {
        Vec::new()
    }

    /// HEARTBEAT system_status, as each firmware's vehicle_system_status() computes it
    /// (failsafe = any failsafe triggered).
    fn system_status(&self, st: &State, failsafe: bool) -> MavState {
        if failsafe {
            return MavState::MAV_STATE_CRITICAL;
        }
        let active = match self.firmware() {
            // ArduPlane: ACTIVE while flying, else STANDBY
            Firmware::Plane => self.airborne(st),
            // Rover: STANDBY in HOLD, ACTIVE otherwise (armed or not)
            Firmware::Rover => st.mode != "HOLD",
            _ => st.armed,
        };
        if active {
            MavState::MAV_STATE_ACTIVE
        } else {
            MavState::MAV_STATE_STANDBY
        }
    }

    fn mode_number(&self, name: &str) -> Option<u32> {
        self.modes()
            .iter()
            .find(|(_, n)| *n == name)
            .map(|(k, _)| *k)
    }

    fn mode_name(&self, number: u32) -> Option<&'static str> {
        self.modes()
            .iter()
            .find(|(k, _)| *k == number)
            .map(|(_, n)| *n)
    }
}

/// The registry: `behavior` in platforms.toml -> implementation.
pub fn create(name: &str) -> Option<Box<dyn Behavior>> {
    Some(match name {
        "plane" => Box::new(plane::Plane),
        "boat" => Box::new(boat::Boat),
        "drifter" => Box::new(drifter::Drifter),
        "sub" => Box::new(sub::Sub),
        "copter" => Box::new(copter::Copter::new()),
        "vtol" => Box::new(vtol::Vtol::new()),
        "blimp" => Box::new(blimp::Blimp::new()),
        "ugv" => Box::new(ugv::Ugv::new()),
        "sailboat" => Box::new(sailboat::Sailboat::new()),
        _ => return None,
    })
}
