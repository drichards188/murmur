//! Fixed wing (ArduPlane): can't stop or hover, can't disarm in flight, holds by circling.
//!
//! Circles use ArduPlane's parameters: LOITER and LOITER_* items without a radius fly
//! WP_LOITER_RAD; RTL circles home at RTL_RADIUS, or WP_LOITER_RAD when that is 0
//! (ArduPlane::update_navigation, mode_rtl). No circle is tighter than the airframe can turn
//! at cruise (catalog cruise / turn rate), as the L1 controller widens a loiter it can't fly.
//! RTL returns at ALT_HOLD_RTL (cm above home; -1 = the current altitude).

use mavlink::dialects::ardupilotmega::{MavSeverity, MavType};

use super::{Behavior, Tick, modes};
use crate::config::Spec;
use crate::kinematics::{
    Out, State, Target, TargetKind, climb, guide, integrate, turn_and_accelerate,
};

pub struct Plane;

/// Height above home that counts as flying (ArduPlane's arming/disarm logic in spirit).
const AIRBORNE_M: f64 = 15.0;
/// Glide ratio with no thrust (a flat battery or a dead engine), estimate for long-wing UAVs.
const GLIDE_RATIO: f64 = 15.0;

/// (loiter radius, RTL radius), m.
fn radii(t: &Tick) -> (f64, f64) {
    let min_r = t.spec.min_turn_radius_m();
    let fallback = t.spec.loiter_radius_m.unwrap_or(150.0);
    let loiter = t
        .params
        .f("WP_LOITER_RAD", fallback)
        .abs()
        .max(min_r)
        .max(1.0);
    let r = t.params.f("RTL_RADIUS", 0.0);
    let rtl = if r == 0.0 { loiter } else { r.abs().max(min_r) };
    (loiter, rtl)
}

impl Plane {
    fn target(&self, t: &mut Tick) -> Option<Target> {
        let st = &*t.st;
        if !st.armed {
            return None;
        }
        let cruise = t.spec.cruise_mps;
        let (loiter, rtl_r) = radii(t);
        if st.mode == "AUTO" {
            // an empty mission completes at once: ArduPlane then RTLs
            return Some(t.mission.auto_target(st, cruise, loiter, loiter));
        }
        let (lat, lon, radius, kind, alt) = if st.mode == "RTL" {
            let a = t.params.f("ALT_HOLD_RTL", -1.0);
            let alt = if a < 0.0 {
                st.alt
            } else {
                st.home.2 + a / 100.0
            };
            (st.home.0, st.home.1, rtl_r, TargetKind::Rtl, alt)
        } else {
            // planes can't stop: every other mode circles the hold point
            let (lat, lon) = st.hold_point;
            (lat, lon, loiter, TargetKind::Hold, st.alt)
        };
        Some(Target {
            lat,
            lon,
            alt,
            radius,
            speed: cruise,
            kind,
        })
    }

    /// No thrust in the air: glide straight on and down until it meets the surface.
    fn glide(&self, t: &mut Tick) {
        let cruise = t.spec.cruise_mps;
        let st = &mut *t.st;
        st.speed += (cruise * 0.8 - st.speed).clamp(-3.0 * t.dt, 3.0 * t.dt);
        st.demand = 0.0;
        st.vz = -st.speed / GLIDE_RATIO;
        st.alt = (st.alt + st.vz * t.dt).max(st.home.2);
        if st.alt <= st.home.2 {
            st.speed = 0.0;
            st.vz = 0.0;
        }
        integrate(st, t.dt, 0.0, 0.0);
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
        let airborne = self.airborne(t.st);
        if !t.st.armed && airborne {
            return self.glide(t);
        }
        let tgt = self.target(t);
        let reach = (t.st.speed * 3.0).max(t.params.f("WP_RADIUS", 90.0));
        let mut want = guide(t.st, tgt.as_ref(), reach, false, t.mission, t.out);
        let cruise = t.spec.cruise_mps;
        if airborne {
            // a fixed wing keeps flying: no slower than AIRSPEED_MIN (or 0.8 cruise)
            let min = t.params.f("AIRSPEED_MIN", cruise * 0.8).min(cruise);
            want.speed = want.speed.max(min.max(cruise * 0.8));
        }
        let landing = tgt.is_some_and(|g| g.kind == TargetKind::Land);
        if landing && t.st.alt - t.st.home.2 < 1.0 {
            // touchdown: roll out, then ArduPlane disarms and the item is done
            want.speed = 0.0;
            if t.st.speed < 1.0 {
                t.st.armed = false;
                t.out.push(Out::Text(
                    MavSeverity::MAV_SEVERITY_INFO,
                    "Throttle disarmed".into(),
                ));
                t.out.push(Out::ItemReached(t.mission.seq));
                t.mission.advance(t.out);
            }
        }
        turn_and_accelerate(t.st, &want, t.dt, t.spec.turn_dps(), 3.0);
        let mut max_climb = t.spec.climb_mps();
        if !airborne && !landing && t.st.speed < cruise * 0.6 {
            max_climb = 0.0; // still on the runway
        }
        climb(t.st, &want, t.dt, max_climb);
        integrate(t.st, t.dt, 0.0, 0.0);
    }
}
