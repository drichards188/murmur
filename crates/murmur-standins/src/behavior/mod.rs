//! Behaviors: which ArduPilot autopilot a stand-in imitates (MAV_TYPE, mode table) and how it
//! moves. Adding one = a file here plus a line in `create`.
//!
//! What every autopilot shares (parameters, telemetry rates, the mission protocol, failsafes,
//! battery, navigation error) lives outside the behaviors and keys off `Behavior::firmware`,
//! which defaults from the mode table, so a new behavior gets the right ArduPilot semantics
//! by picking its `modes::` table.

use mavlink::dialects::ardupilotmega::{GpsFixType, MavParamType, MavState, MavType};

use crate::conditions::Sample;
use crate::config::Spec;
use crate::geo::Gauss;
use crate::kinematics::{Out, State};
use crate::mission::Mission;
use crate::params::Params;

mod apmission;
mod blimp;
mod boat;
mod drifter;
mod marine;
pub mod modes;
mod plane;
mod rover;
mod sailboat;
mod sub;
mod ugv;

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

    /// Marine behaviors get tidewater conditions (currents, depth, grounding).
    fn marine(&self) -> bool {
        false
    }

    /// GPS fix type and satellites in view.
    fn gps(&self, _st: &State) -> (GpsFixType, u8) {
        (GpsFixType::GPS_FIX_TYPE_3D_FIX, 14)
    }

    fn step(&mut self, t: &mut Tick);

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
        "blimp" => Box::new(blimp::Blimp::new()),
        "ugv" => Box::new(ugv::Ugv::new()),
        "sailboat" => Box::new(sailboat::Sailboat::new()),
        _ => return None,
    })
}
