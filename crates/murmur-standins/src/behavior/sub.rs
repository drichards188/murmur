//! Submarine / AUV (ArduSub numbering): no RTL mode, no GPS while submerged (it navigates on
//! inertial + DVL, see nav.rs), rests on the seabed. SURFACE ascends in place and, at the
//! surface, switches to ALT_HOLD (ArduSub mode_surface.cpp).

use mavlink::dialects::ardupilotmega::{GpsFixType, MavType};

use super::marine::{self, Hull};
use super::{Behavior, Tick, modes};
use crate::kinematics::{Out, State, Want, climb, guide, integrate, turn_and_accelerate};

pub struct Sub;

impl Behavior for Sub {
    fn mav_type(&self) -> MavType {
        MavType::MAV_TYPE_SUBMARINE
    }

    fn modes(&self) -> &'static [(u32, &'static str)] {
        modes::SUB
    }

    fn hold_mode(&self) -> &'static str {
        "POSHOLD"
    }

    fn wants_conditions(&self) -> bool {
        true
    }

    fn gps(&self, st: &State) -> (GpsFixType, u8) {
        // no GPS underwater: AUVs navigate on INS + DVL
        if st.depth() > 1.0 {
            (GpsFixType::GPS_FIX_TYPE_NO_GPS, 0)
        } else {
            (GpsFixType::GPS_FIX_TYPE_3D_FIX, 14)
        }
    }

    fn step(&mut self, t: &mut Tick) {
        let tgt = marine::target(t);
        let reach = t.params.f("WP_RADIUS_M", 10.0);
        let mut want = guide(t.st, tgt.as_ref(), reach, true, t.mission, t.out);
        if t.st.mode == "SURFACE" && t.st.armed {
            want = Want {
                speed: 0.0,
                heading: t.st.heading,
                alt: 0.0,
            };
            // at the surface: SURFACE_DEPTH is in cm (negative = below)
            if t.st.alt >= t.params.f("SURFACE_DEPTH", -10.0) / 100.0 {
                t.out.push(Out::SetMode("ALT_HOLD"));
            }
        }
        turn_and_accelerate(t.st, &want, t.dt, t.spec.turn_dps(), 0.5);
        climb(t.st, &want, t.dt, t.spec.climb_mps());
        let drift = marine::water(t, Hull::Submerged);
        let (e, n) = marine::settle(t, drift);
        integrate(t.st, t.dt, e, n);
    }
}
