//! Behaviors: which ArduPilot autopilot a stand-in imitates (MAV_TYPE, mode table) and how it
//! moves. Adding one = a file here plus a line in `create`.

use mavlink::dialects::ardupilotmega::{GpsFixType, MavType};

use crate::conditions::Sample;
use crate::config::Spec;
use crate::geo::Gauss;
use crate::kinematics::{Out, State};
use crate::mission::Mission;

mod boat;
mod drifter;
mod marine;
pub mod modes;
mod plane;
mod sub;

/// Everything a physics tick may read or change.
pub struct Tick<'a> {
    pub spec: &'a Spec,
    pub st: &'a mut State,
    pub mission: &'a mut Mission,
    /// latest tidewater sample at (about) this position, if conditions are on
    pub water: Option<&'a Sample>,
    pub rng: &'a mut Gauss,
    pub out: &'a mut Vec<Out>,
    pub dt: f64,
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
        _ => return None,
    })
}
