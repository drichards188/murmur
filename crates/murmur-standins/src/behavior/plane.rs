//! Fixed wing (ArduPlane): can't stop or hover, can't disarm in flight, holds by circling.

use mavlink::dialects::ardupilotmega::MavType;

use super::{Behavior, Tick, modes};
use crate::config::Spec;
use crate::kinematics::{State, Target, TargetKind, climb, guide, integrate, turn_and_accelerate};

pub struct Plane;

/// Height above home that counts as flying (ArduPlane's arming/disarm logic in spirit).
const AIRBORNE_M: f64 = 15.0;

impl Plane {
    fn target(&self, t: &mut Tick) -> Option<Target> {
        let st = &*t.st;
        if !st.armed {
            return None;
        }
        let cruise = t.spec.cruise_mps;
        let hold_r = t.spec.loiter_radius_m.unwrap_or(0.0);
        if st.mode == "AUTO" && !t.mission.items.is_empty() {
            let loiter = t.spec.loiter_radius_m.unwrap_or(200.0);
            return Some(t.mission.auto_target(st, cruise, hold_r, loiter));
        }
        let (lat, lon, radius, kind) = if st.mode == "RTL" {
            (st.home.0, st.home.1, hold_r, TargetKind::Rtl)
        } else {
            // planes can't stop: every other mode circles the hold point
            let r = if hold_r != 0.0 { hold_r } else { 300.0 };
            (st.hold_point.0, st.hold_point.1, r, TargetKind::Hold)
        };
        Some(Target {
            lat,
            lon,
            alt: st.alt,
            radius,
            speed: cruise,
            kind,
        })
    }
}

impl Behavior for Plane {
    fn mav_type(&self) -> MavType {
        MavType::MAV_TYPE_FIXED_WING
    }

    fn modes(&self) -> &'static [(u32, &'static str)] {
        modes::PLANE
    }

    fn hold_mode(&self) -> &'static str {
        "LOITER"
    }

    fn spawn(&self, spec: &Spec, st: &mut State) {
        if st.alt > 50.0 {
            // spawned high: on station, flying, home on the surface below
            st.home.2 = 0.0;
            st.speed = spec.cruise_mps;
            st.armed = true;
            st.mode = self.hold_mode();
        }
    }

    fn airborne(&self, st: &State) -> bool {
        st.alt - st.home.2 > AIRBORNE_M
    }

    fn step(&mut self, t: &mut Tick) {
        let tgt = self.target(t);
        let reach = (t.st.speed * 3.0).max(20.0);
        let mut want = guide(t.st, tgt.as_ref(), reach, false, t.mission, t.out);
        let cruise = t.spec.cruise_mps;
        let airborne = self.airborne(t.st);
        if airborne {
            want.speed = want.speed.max(cruise * 0.8); // fixed wing keeps flying
        }
        turn_and_accelerate(t.st, &want, t.dt, t.spec.turn_dps(), 3.0);
        let mut max_climb = t.spec.climb_mps();
        if !airborne && t.st.speed < cruise * 0.6 {
            max_climb = 0.0; // still on the runway
        }
        climb(t.st, &want, t.dt, max_climb);
        integrate(t.st, t.dt, 0.0, 0.0);
    }
}
