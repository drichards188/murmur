//! Shared by everything that floats or dives: target selection for Rover/Sub-style autopilots
//! (they can stop), water currents, the seabed, and running aground.

use mavlink::dialects::ardupilotmega::MavSeverity;

use super::Tick;
use crate::kinematics::{Out, Target, TargetKind};

/// AUTO follows the mission, RTL heads home, every other mode stops.
pub fn target(t: &mut Tick) -> Option<Target> {
    let st = &*t.st;
    if !st.armed {
        return None;
    }
    let cruise = t.spec.cruise_mps;
    if st.mode == "AUTO" && !t.mission.items.is_empty() {
        let loiter = t.spec.loiter_radius_m.unwrap_or(200.0);
        return Some(t.mission.auto_target(st, cruise, 0.0, loiter, t.out));
    }
    if st.mode == "RTL" {
        let (lat, lon, _) = st.home;
        return Some(Target {
            lat,
            lon,
            alt: st.alt,
            radius: 0.0,
            speed: cruise,
            kind: TargetKind::Rtl,
        });
    }
    None
}

pub enum Hull {
    /// floats; aground when the water under the keel is shallower than `draft_m`
    Surface { draft_m: f64 },
    /// dives; rests on the seabed instead of going through it
    Submerged,
}

/// Drift from the current, and the seabed / grounding checks. Returns (east, north) m/s.
pub fn water(t: &mut Tick, hull: Hull) -> (f64, f64) {
    let Some(c) = t.water.filter(|c| c.in_area()) else {
        return (0.0, 0.0);
    };
    let drift = (c.current_east(), c.current_north());
    match hull {
        Hull::Submerged => {
            if let Some(depth) = c.water_depth_m {
                let floor = -(depth - 1.0);
                t.st.alt = t.st.alt.max(floor);
            }
        }
        Hull::Surface { draft_m } => {
            let was = t.st.aground;
            t.st.aground = !c.navigable(draft_m);
            if t.st.aground && !was {
                t.out.push(Out::Text(
                    MavSeverity::MAV_SEVERITY_WARNING,
                    "Aground".into(),
                ));
            }
        }
    }
    drift
}

/// A grounded hull doesn't move, under its own power or with the current.
pub fn settle(t: &mut Tick, drift: (f64, f64)) -> (f64, f64) {
    if t.st.aground {
        t.st.speed = 0.0;
        return (0.0, 0.0);
    }
    drift
}

/// Draft used for grounding: fast boats get 2 m of margin (standins.py).
pub fn draft_m(max_mps: f64) -> f64 {
    0.5 + if max_mps > 10.0 { 2.0 } else { 0.0 }
}
