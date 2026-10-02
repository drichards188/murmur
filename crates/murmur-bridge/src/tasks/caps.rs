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
    pub hold: &'static str,
    /// mission altitude: base, and step per vehicle of that kind in the same tasking (vertical
    /// separation); negative is depth
    pub alt: (f64, f64),
    /// how it gets off the ground, if it has to
    pub takeoff: Option<MavCmd>,
    pub orbit: Option<OrbitStyle>,
    /// how close to home counts as "home"
    pub home_tol: f64,
}

use TaskKind::{Hold, Orbit, Patrol, Rtl, Track};

const MOVERS: &[TaskKind] = &[Orbit, Patrol, Hold, Rtl];

static PLANE: Caps = Caps {
    tasks: MOVERS,
    auto: "AUTO",
    rtl: Some("RTL"),
    hold: "LOITER",
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
static COPTER: Caps = Caps {
    tasks: MOVERS,
    auto: "AUTO",
    rtl: Some("RTL"),
    hold: "LOITER",
    alt: (30.0, 5.0),
    takeoff: Some(MavCmd::MAV_CMD_NAV_TAKEOFF),
    orbit: Some(OrbitStyle::Waypoints),
    home_tol: 15.0,
};
static BLIMP: Caps = Caps {
    alt: (20.0, 5.0),
    takeoff: None,
    home_tol: 30.0,
    ..COPTER
};
static BOAT: Caps = Caps {
    tasks: MOVERS,
    auto: "AUTO",
    rtl: Some("RTL"),
    hold: "HOLD",
    alt: (0.0, 0.0),
    takeoff: None,
    orbit: Some(OrbitStyle::Waypoints),
    home_tol: 15.0,
};
static UGV: Caps = Caps {
    home_tol: 10.0,
    ..BOAT
};
static SUB: Caps = Caps {
    rtl: None,
    hold: "POSHOLD",
    alt: (-5.0, -2.0),
    home_tol: 20.0,
    ..BOAT
};
static TRACKER: Caps = Caps {
    tasks: &[Track, Hold],
    auto: "AUTO",
    rtl: None,
    hold: "STOP",
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
