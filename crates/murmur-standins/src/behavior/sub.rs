//! Submarine / AUV (ArduSub numbering): no RTL mode, no GPS while submerged (it navigates on
//! inertial + DVL, see nav.rs), rests on the seabed. SURFACE ascends in place and, at the
//! surface, switches to ALT_HOLD (ArduSub mode_surface.cpp). Vertical speed is ArduSub's
//! WP_SPD_UP / WP_SPD_DN (AC_WPNav), seeded from the catalog's climb rate when it has one
//! (the platform's limit), else ArduSub's defaults; SURFACE ascends at WP_SPD_UP as well.

use mavlink::dialects::ardupilotmega::{GpsFixType, MavParamType, MavType};

use super::marine::{self, Hull};
use super::{Behavior, Tick, modes};
use crate::config::Spec;
use crate::kinematics::{Out, State, Want, climb, guide, integrate, turn_and_accelerate};

pub struct Sub;

/// AC_WPNav's WP_SPD_UP_DEFAULT / WP_SPD_DOWN_DEFAULT, m/s.
const WP_SPD_UP_DEFAULT: f32 = 2.5;
const WP_SPD_DN_DEFAULT: f32 = 1.5;

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

    fn extra_params(&self, spec: &Spec) -> Vec<(&'static str, MavParamType, f32)> {
        let f32_ = MavParamType::MAV_PARAM_TYPE_REAL32;
        let climb = spec.climb_mps.map(|c| c as f32);
        vec![
            ("WP_SPD_UP", f32_, climb.unwrap_or(WP_SPD_UP_DEFAULT)),
            ("WP_SPD_DN", f32_, climb.unwrap_or(WP_SPD_DN_DEFAULT)),
        ]
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
        let rate = if want.alt > t.st.alt {
            t.params.f("WP_SPD_UP", f64::from(WP_SPD_UP_DEFAULT))
        } else {
            t.params.f("WP_SPD_DN", f64::from(WP_SPD_DN_DEFAULT))
        };
        climb(t.st, &want, t.dt, rate.max(0.0));
        let drift = marine::water(t, Hull::Submerged);
        let (e, n) = marine::settle(t, drift);
        integrate(t.st, t.dt, e, n);
    }
}
