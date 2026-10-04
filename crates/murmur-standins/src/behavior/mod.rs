//! Behaviors: which ArduPilot autopilot a stand-in imitates (MAV_TYPE, mode table) and how it
//! moves. Adding one = a file here plus a line in `create`.
//!
//! What every autopilot shares (parameters, telemetry rates, the mission protocol, failsafes,
//! battery, navigation error) lives outside the behaviors and keys off `Behavior::firmware`,
//! which defaults from the mode table, so a new behavior gets the right ArduPilot semantics
//! by picking its `modes::` table.

use mavlink::MavHeader;
use mavlink::dialects::ardupilotmega::{
    GpsFixType, MavCmd, MavMessage, MavModeFlag, MavParamType, MavResult, MavState, MavType,
    NAV_CONTROLLER_OUTPUT_DATA,
};

use crate::conditions::Sample;
use crate::config::Spec;
use crate::geo::Gauss;
use crate::kinematics::{Out, State};
use crate::mission::Mission;
use crate::params::Params;

pub(crate) mod apmission;
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
mod tracker;
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

/// A message the vehicle received, for behaviors that act on traffic that is not addressed to
/// them (an antenna tracker listens to its target's GLOBAL_POSITION_INT). Seen before the
/// target-system filter, as ArduPilot's `packetReceived` sees it.
pub struct Observe<'a> {
    pub from: &'a MavHeader,
    /// this vehicle's own system id
    pub sysid: u8,
    pub msg: &'a MavMessage,
    /// the vehicle's parameter store (a behavior may change its own, e.g. SYSID_TARGET)
    pub params: &'a mut Params,
    pub st: &'a State,
    /// messages to send in reply, on the vehicle's own link
    pub send: Vec<MavMessage>,
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

    /// Every message that arrives, before the target-system filter (default: ignore).
    fn observe(&mut self, _o: &mut Observe) {}

    /// (roll, pitch) in radians for ATTITUDE when the vehicle is not a free-flying body (a
    /// tracker's mount). None: the generic estimate (level on the ground, banked in turns).
    fn attitude(&self, _st: &State) -> Option<(f64, f64)> {
        None
    }

    /// The firmware's own NAV_CONTROLLER_OUTPUT, when it differs from the generic one.
    fn nav_controller_output(&self, _st: &State) -> Option<NAV_CONTROLLER_OUTPUT_DATA> {
        None
    }

    /// HEARTBEAT base_mode flags for a mode, besides CUSTOM_MODE_ENABLED and SAFETY_ARMED
    /// (each firmware's `base_mode()` sets its own).
    fn base_mode_flags(&self, _mode: &str) -> MavModeFlag {
        MavModeFlag::empty()
    }

    /// Whether entering this mode arms or disarms the vehicle (AntennaTracker: modes that
    /// drive the servos arm them). None: a mode change leaves arming alone.
    fn mode_arms(&self, _mode: &str) -> Option<bool> {
        None
    }

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
        "plane" => Box::new(plane::Plane::new()),
        "boat" => Box::new(boat::Boat::new()),
        "drifter" => Box::new(drifter::Drifter),
        "sub" => Box::new(sub::Sub),
        "tracker" => Box::new(tracker::Tracker::default()),
        "copter" => Box::new(copter::Copter::new()),
        "vtol" => Box::new(vtol::Vtol::new()),
        "blimp" => Box::new(blimp::Blimp::new()),
        "ugv" => Box::new(ugv::Ugv::new()),
        "sailboat" => Box::new(sailboat::Sailboat::new()),
        _ => return None,
    })
}
