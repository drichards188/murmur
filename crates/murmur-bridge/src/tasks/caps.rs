//! The capability table: per autopilot family, which tasks it can do and how it flies them.
//! Each ArduPilot firmware (Plane, Copter, Rover, Sub, Blimp, AntennaTracker) numbers its
//! modes differently and supports different tasks.

use super::TaskKind;
use crate::mav::{Kind, apm::MavCmd};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrbitStyle {
    /// fixed wing: one loiter-forever item at the center, the autopilot flies the circle
    Loiter,
    /// everyone else: a ring of waypoints, looped
    Waypoints,
}

#[derive(Debug)]
pub struct Caps {
    pub tasks: &'static [TaskKind],
    pub auto: &'static str,
    /// None: no RTL mode (Sub); "rtl" is flown as a one-waypoint mission back to home
    pub rtl: Option<&'static str>,
    /// The mode "hold" commands. It must keep the vehicle where it is with no RC transmitter
    /// and no further commands: a mode whose climb rate or throttle is the pilot's stick
    /// (ArduCopter LOITER, for one) descends and lands, and a mode that only stops the motors
    /// (ArduRover HOLD) lets a boat drift off with the current. See the notes above `PLANE`.
    pub hold: &'static str,
    /// A mode the firmware arms in, switched to when arming is refused in a return, landing
    /// or brake mode (a lost-link failsafe's, an earlier hold's): ArduCopter does not arm in
    /// BRAKE (its hold), so a copter arms in GUIDED; the rest arm in their hold mode.
    pub arm: &'static str,
    /// mission altitude: base, and step per vehicle of that kind in the same tasking (vertical
    /// separation); negative is depth
    pub alt: (f64, f64),
    /// how it gets off the ground, if it has to
    pub takeoff: Option<MavCmd>,
    pub orbit: Option<OrbitStyle>,
    /// how close to home counts as "home" (for fixed wing: to its RTL circle, see
    /// [`Caps::home_radius`])
    pub home_tol: f64,
}

impl Caps {
    /// Distance from home that counts as "home". A fixed wing can't stop: RTL ends circling
    /// home on its RTL loiter circle (an RQ-4-class aircraft 9 km out, a SITL plane 150 m),
    /// so for it home is that circle's radius plus the tolerance.
    pub fn home_radius(&self, v: &crate::vehicle::Vehicle) -> f64 {
        let circle = if self.orbit == Some(OrbitStyle::Loiter) {
            v.rtl_loiter_radius_m().unwrap_or(0.0)
        } else {
            0.0
        };
        circle + self.home_tol
    }
}

use TaskKind::{Hold, Orbit, Patrol, Rtl, Track};

const MOVERS: &[TaskKind] = &[Orbit, Patrol, Hold, Rtl];

// Hold modes, checked against ArduPilot 4.8 with no RC input:
// - Plane, VTOL: LOITER circles the point where it was entered at the current altitude (the
//   altitude comes from TECS; only the ENABLE_LOITER_ALT_CONTROL flight option puts it on the
//   stick, and that is off). A fixed wing cannot stop, so circling is its hold.
// - Copter: BRAKE (see COPTER). LOITER and POSHOLD take the climb rate from the throttle stick.
// - Blimp: LOITER latches the position and yaw on entry and the pilot input only moves that
//   target; with none it holds all four axes.
// - Boat: LOITER holds station (it drives back inside LOIT_RADIUS and stops there); HOLD
//   only zeroes throttle and steering, so a boat in a tidal current drifts away.
// - UGV: HOLD stops the motors and centres the steering; a ground vehicle stays put.
// - Sub: POSHOLD holds position and depth (no pilot input = zero rates). It needs a position
//   estimate (refused otherwise); ALT_HOLD would keep depth only.
// - Tracker: STOP holds the current pan and tilt.
static PLANE: Caps = Caps {
    tasks: MOVERS,
    auto: "AUTO",
    rtl: Some("RTL"),
    hold: "LOITER",
    arm: "LOITER",
    alt: (80.0, 15.0),
    takeoff: Some(MavCmd::MAV_CMD_NAV_TAKEOFF),
    orbit: Some(OrbitStyle::Loiter),
    home_tol: 250.0,
};
static VTOL: Caps = Caps {
    alt: (70.0, 15.0),
    takeoff: Some(MavCmd::MAV_CMD_NAV_VTOL_TAKEOFF),
    ..PLANE
};
// ArduCopter has no RC here, so the throttle stick sits at the bottom: STABILIZE, ALT_HOLD,
// LOITER, POSHOLD and the rest that read it descend at PILOT_SPD_DN, land and auto-disarm.
// BRAKE (mode_brake.cpp) runs the position controller to a zero velocity target (it stops
// within a few metres at BRAKE_MODE_DECEL_RATE) and holds that point, and its vertical
// controller targets a zero climb rate (it holds the altitude it has). It reads no RC, has no
// timeout (only the Solo pause button sets one) and nothing makes it exit but a mode change or
// a failsafe. GUIDED would also hold, but needs a position target message sent after the
// mode change; BRAKE needs nothing more. Like LOITER it requires a position estimate.
static COPTER: Caps = Caps {
    tasks: MOVERS,
    auto: "AUTO",
    rtl: Some("RTL"),
    hold: "BRAKE",
    arm: "GUIDED",
    alt: (30.0, 5.0),
    takeoff: Some(MavCmd::MAV_CMD_NAV_TAKEOFF),
    orbit: Some(OrbitStyle::Waypoints),
    home_tol: 15.0,
};
static BLIMP: Caps = Caps {
    hold: "LOITER",
    arm: "LOITER",
    alt: (20.0, 5.0),
    takeoff: None,
    home_tol: 30.0,
    ..COPTER
};
static BOAT: Caps = Caps {
    tasks: MOVERS,
    auto: "AUTO",
    rtl: Some("RTL"),
    hold: "LOITER",
    arm: "LOITER",
    alt: (0.0, 0.0),
    takeoff: None,
    orbit: Some(OrbitStyle::Waypoints),
    home_tol: 15.0,
};
static UGV: Caps = Caps {
    hold: "HOLD",
    arm: "HOLD",
    home_tol: 10.0,
    ..BOAT
};
static SUB: Caps = Caps {
    rtl: None,
    hold: "POSHOLD",
    arm: "POSHOLD",
    alt: (-5.0, -2.0),
    home_tol: 20.0,
    ..BOAT
};
static TRACKER: Caps = Caps {
    tasks: &[Track, Hold],
    auto: "AUTO",
    rtl: None,
    hold: "STOP",
    arm: "STOP",
    alt: (0.0, 0.0),
    takeoff: None,
    orbit: None,
    home_tol: 0.0,
};

pub fn caps(kind: Kind) -> Option<&'static Caps> {
    Some(match kind {
        Kind::Plane => &PLANE,
        Kind::Vtol => &VTOL,
        Kind::Copter => &COPTER,
        Kind::Blimp => &BLIMP,
        Kind::Boat => &BOAT,
        Kind::Ugv => &UGV,
        Kind::Sub => &SUB,
        Kind::Tracker => &TRACKER,
        Kind::Unknown => return None,
    })
}

pub fn can(kind: Option<Kind>, task: TaskKind) -> bool {
    kind.and_then(caps).is_some_and(|c| c.tasks.contains(&task))
}

/// Mission altitude relative to home (negative = depth) for the vehicle in `slot` among
/// same-kind vehicles in one tasking (stacked apart): the platform's own cruise altitude when the
/// fleet manifest knows it (`cruise_alt_m`: a Global Hawk at 18 km, an AUV at 40 m depth), else
/// the per-autopilot default (`Caps::alt`).
pub fn mission_alt(caps: &Caps, cruise_alt_m: Option<f64>, home_alt: Option<f64>, slot: usize) -> f64 {
    let (mut base, mut step) = caps.alt;
    if let Some(cruise) = cruise_alt_m
        && (cruise > 50.0 || cruise < 0.0)
    {
        base = if cruise > 0.0 {
            cruise - home_alt.unwrap_or(0.0)
        } else {
            cruise
        };
        let sign = if step == 0.0 { 1.0 } else { step };
        step = step.abs().max(cruise.abs() * 0.02).copysign(sign);
    }
    base + step * slot as f64
}

/// Takeoff altitude cap for aircraft that take off from the ground.
pub fn takeoff_alt(kind: Kind) -> f64 {
    match kind {
        Kind::Plane => 60.0,
        Kind::Vtol => 40.0,
        _ => 30.0,
    }
}

/// Which bridge kind each platform in sim/platforms.toml runs as (for the Platforms panel):
/// SITL platforms by `vehicle`, stand-ins by the autopilot their `behavior` imitates. A
/// drifter has no propulsion and no row.
pub fn platform_kind(sim: &str, vehicle: Option<&str>, behavior: Option<&str>) -> Option<Kind> {
    let name = if sim == "sitl" { vehicle? } else { behavior? };
    Some(match name {
        "plane" => Kind::Plane,
        "vtol" => Kind::Vtol,
        "copter" => Kind::Copter,
        "blimp" | "airship" => Kind::Blimp,
        "tracker" => Kind::Tracker,
        "boat" | "sailboat" => Kind::Boat,
        "ugv" => Kind::Ugv,
        "sub" => Kind::Sub,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vehicle::Vehicle;

    #[test]
    fn fixed_wing_home_is_its_rtl_circle() {
        let mut v = Vehicle::default();
        v.loiter_radius_m = Some(3_500.0);
        v.rtl_radius_m = Some(0.0);
        // an MQ-9B-class stand-in circles home 3.5 km out; a copter's home is a point
        assert_eq!(PLANE.home_radius(&v), 3_750.0);
        assert_eq!(VTOL.home_radius(&v), 3_750.0);
        assert_eq!(COPTER.home_radius(&v), 15.0);
        // SITL plane (plane.parm 150 m) and a plane whose parameters never came back
        v.loiter_radius_m = Some(150.0);
        assert_eq!(PLANE.home_radius(&v), 400.0);
        let unknown = Vehicle::default();
        assert_eq!(PLANE.home_radius(&unknown), 250.0);
    }
}
