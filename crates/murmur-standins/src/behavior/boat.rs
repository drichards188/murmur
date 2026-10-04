//! Surface vessel (ArduRover, boat frame): ArduRover's mode semantics through `rover::Nav`,
//! as the ground vehicle and the sailboat have them, checked against the motorboat SITL
//! (sim/tests/standins_vs_sitl_plane_boat_sub.py): AUTO with AP_Mission's texts, "Reached
//! destination" in RTL, LOITER holding station within LOIT_RADIUS, HOLD stopping the motors
//! (the current then carries it), MIS_DONE_BEHAVE's loiter at the mission's end, the mission
//! reset on a disarm outside AUTO, arming refused in RTL.
//!
//! Movement: heads for the goal at the mission's speed (the catalog cruise, or a
//! DO_CHANGE_SPEED), accelerates at ACCEL_MPS2, turns at the catalog rate, brakes to stop at
//! a point it holds; drifts with the current and runs aground (marine.rs).

use mavlink::dialects::ardupilotmega::{MavParamType, MavType};

use super::marine::{self, Hull};
use super::rover::{self, Goal, Nav};
use super::{Behavior, Tick, modes};
use crate::config::Spec;
use crate::geo::{bearing_deg, distance_m};
use crate::kinematics::{State, Want, integrate, turn_and_accelerate};

/// A hull's acceleration and braking, m/s/s (estimate).
const ACCEL_MPS2: f64 = 0.5;

pub struct Boat {
    nav: Nav,
}

impl Boat {
    pub fn new() -> Boat {
        Boat {
            nav: Nav::new(true, "LOIT_RADIUS"),
        }
    }
}

impl Default for Boat {
    fn default() -> Self {
        Boat::new()
    }
}

impl Behavior for Boat {
    fn mav_type(&self) -> MavType {
        MavType::MAV_TYPE_SURFACE_BOAT
    }

    fn modes(&self) -> &'static [(u32, &'static str)] {
        modes::ROVER
    }

    fn hold_mode(&self) -> &'static str {
        "HOLD"
    }

    fn accepts_mode(&self, name: &str) -> bool {
        rover::accepts_mode(name)
    }

    fn arming_refusal(&self, st: &State) -> Option<&'static str> {
        rover::arming_refusal(st.mode)
    }

    fn wants_conditions(&self) -> bool {
        true
    }

    /// sim/params/boat.parm's WP_RADIUS, which the SITL boats get from their defaults file
    fn extra_params(&self, _spec: &Spec) -> Vec<(&'static str, MavParamType, f32)> {
        vec![("WP_RADIUS", MavParamType::MAV_PARAM_TYPE_REAL32, 8.0)]
    }

    fn step(&mut self, t: &mut Tick) {
        let goal = self.nav.goal(t);
        let st = &*t.st;
        let brake = |d: f64| (2.0 * ACCEL_MPS2 * (d - 1.0).max(0.0)).sqrt();
        let (heading, speed) = match goal {
            Goal::Stop => (st.heading, 0.0),
            Goal::Go {
                lat,
                lon,
                speed,
                stop,
            } => {
                let d = distance_m(st.lat, st.lon, lat, lon);
                let v = if stop { speed.min(brake(d)) } else { speed };
                (bearing_deg(st.lat, st.lon, lat, lon), v)
            }
            Goal::Loiter { lat, lon, radius } => {
                // outside the loiter radius: back to the point, and stop there
                let d = distance_m(st.lat, st.lon, lat, lon);
                if d > radius {
                    let v = t.spec.cruise_mps.min(brake(d));
                    (bearing_deg(st.lat, st.lon, lat, lon), v)
                } else {
                    (st.heading, 0.0)
                }
            }
            Goal::Circle {
                lat,
                lon,
                radius,
                speed,
            } => (rover::ring_heading(st.lat, st.lon, lat, lon, radius), speed),
        };
        let want = Want {
            speed,
            heading,
            alt: st.alt,
        };
        turn_and_accelerate(t.st, &want, t.dt, t.spec.turn_dps(), ACCEL_MPS2);
        let drift = marine::water(
            t,
            Hull::Surface {
                draft_m: marine::draft_m(t.spec.max_mps),
            },
        );
        let (e, n) = marine::settle(t, drift);
        integrate(t.st, t.dt, e, n);
    }
}

#[cfg(test)]
mod tests {
    use mavlink::dialects::ardupilotmega::{MISSION_ITEM_INT_DATA, MavCmd, MavFrame};

    use super::super::auto::testkit::{Sim, offset};

    /// The motorboat SITL's mission step in sim/tests/standins_vs_sitl_plane_boat_sub.py:
    /// two passes over a waypoint, a DO_CHANGE_SPEED, a waypoint, a hold-time waypoint and a
    /// DO_JUMP. MISSION_ITEM_REACHED for 2 (the DO item takes waypoint 1's), 3 and 4 on
    /// each pass; AP_Mission's texts in order.
    #[test]
    #[allow(deprecated)]
    fn two_passes_over_a_mission_with_do_items_report_as_ardurover() {
        let mut s = Sim::new("boat", 12.0, Some(10.0));
        let at = |seq: u16, cmd: MavCmd, n: f64, e: f64, p1: f32, p2: f32| {
            let (lat, lon) = offset(n, e);
            let located = (n, e) != (0.0, 0.0);
            MISSION_ITEM_INT_DATA {
                seq,
                command: cmd,
                frame: MavFrame::MAV_FRAME_GLOBAL_RELATIVE_ALT_INT,
                x: if located { (lat * 1e7) as i32 } else { 0 },
                y: if located { (lon * 1e7) as i32 } else { 0 },
                param1: p1,
                param2: p2,
                ..Default::default()
            }
        };
        s.mission.install(vec![
            at(0, MavCmd::MAV_CMD_NAV_WAYPOINT, 0.0, 0.0, 0.0, 0.0),
            at(1, MavCmd::MAV_CMD_NAV_WAYPOINT, 0.0, 150.0, 0.0, 0.0),
            at(2, MavCmd::MAV_CMD_DO_CHANGE_SPEED, 0.0, 0.0, 1.0, 6.0),
            at(3, MavCmd::MAV_CMD_NAV_WAYPOINT, 150.0, 150.0, 0.0, 0.0),
            at(4, MavCmd::MAV_CMD_NAV_WAYPOINT, 150.0, 0.0, 5.0, 0.0),
            at(5, MavCmd::MAV_CMD_DO_JUMP, 0.0, 0.0, 1.0, 1.0),
        ]);
        s.st.armed = true;
        s.st.mode = "AUTO";
        s.mission.start_or_resume(false);
        let t = s.until(400.0, |s| s.said("Mission Complete"));
        assert!(t.is_some(), "{:?}", s.said);
        assert_eq!(s.reached, vec![2, 3, 4, 2, 3, 4], "{:?}", s.said);
        let pass = [
            "Mission: 1 WP",
            "Reached waypoint #1",
            "Mission: 2 ChangeSpeed",
            "speed: 6.0 m/s",
            "Mission: 3 WP",
            "Reached waypoint #3",
            "Mission: 4 WP",
            "Reached waypoint #4. Loiter for 5 seconds",
        ];
        let mut want: Vec<&str> = pass.to_vec();
        want.push("Mission: 5 Jump 1/1");
        want.extend(pass);
        want.push("Mission Complete");
        assert_eq!(s.said, want);
    }
}
