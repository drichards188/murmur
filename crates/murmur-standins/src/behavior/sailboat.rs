//! Sailboat (ArduRover with SAIL_ENABLE, as the `sailboat` SITL frame): wind-driven, tacks
//! upwind, drifts with the current.
//!
//! Wire: MAV_TYPE_SURFACE_BOAT (ArduRover reports boats so; the sailboat SITL frame is a boat,
//! FRAME_CLASS 2), Rover mode numbers, ArduRover's mission messages (rover.rs) and its
//! "Sailboat: Tacking" / "Sailboat: Tacking timed out".
//!
//! Heading: ArduRover's Sailboat::calc_heading. A waypoint within SAIL_NO_GO_ANGLE (45 deg)
//! of the true wind (plus a 10 deg pad) is sailed indirectly, close-hauled on the current tack;
//! it tacks when the cross-track error reaches SAIL_XTRACK_MAX (10 m) on the tack that takes
//! it further out, or when the direct heading opens on the other tack. A tack holds the new
//! close-hauled heading until within 10 deg of it or 5 s pass ("timed out"), and the next one
//! can start 5 s after.
//!
//! Speed: a polar diagram, boat speed as a fraction of true wind speed by true wind angle,
//! shaped like a wing-sail USV's (none inside 40 deg, best on a beam reach), capped at the
//! catalog's max_mps (Saildrone-class: about 2 m/s typical, 4 m/s top). Wind from tidewater;
//! without it, ArduPilot SITL's sailboat default (3 m/s from 180, SIM_WIND_SPD / SIM_WIND_DIR),
//! logged once. Currents drift it and shallows ground it, as the motorboat (marine.rs).

use std::sync::atomic::{AtomicBool, Ordering};

use mavlink::dialects::ardupilotmega::{MavSeverity, MavType};

use super::marine::{self, Hull};
use super::rover::{self, Goal, Nav};
use super::{Behavior, Tick, modes};
use crate::geo::{bearing_deg, distance_m, wrap180};
use crate::kinematics::{Out, integrate};

const WP_RADIUS_M: f64 = 3.0;
/// SAIL_LOIT_RADIUS
const LOIT_RADIUS_M: f64 = 5.0;
const NO_GO_DEG: f64 = 45.0;
const NO_GO_PAD_DEG: f64 = 10.0;
const XTRACK_MAX_M: f64 = 10.0;
const TACK_ACCURACY_DEG: f64 = 10.0;
const TACK_TIMEOUT_S: f64 = 5.0;
const TACK_RETRY_S: f64 = 5.0;
const ACCEL_MPS2: f64 = 0.3;
const COAST_DECEL_MPS2: f64 = 0.15;
/// SITL's sailboat wind, used without tidewater: (speed m/s, direction it comes from)
const DEFAULT_WIND: (f64, f64) = (3.0, 180.0);

/// (true wind angle deg, boat speed / true wind speed)
const POLAR: &[(f64, f64)] = &[
    (0.0, 0.0),
    (40.0, 0.0),
    (45.0, 0.28),
    (60.0, 0.38),
    (90.0, 0.45),
    (110.0, 0.45),
    (135.0, 0.40),
    (150.0, 0.35),
    (180.0, 0.30),
];

static DEFAULT_WIND_LOGGED: AtomicBool = AtomicBool::new(false);

/// Boat speed for a true wind angle (either side) and speed, from the polar.
pub fn polar_speed(twa_deg: f64, tws: f64) -> f64 {
    let a = wrap180(twa_deg).abs();
    let ratio = POLAR
        .windows(2)
        .find(|w| a <= w[1].0)
        .map_or(POLAR[POLAR.len() - 1].1, |w| {
            let f = (a - w[0].0) / (w[1].0 - w[0].0);
            w[0].1 + f * (w[1].1 - w[0].1)
        });
    ratio * tws
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tack {
    Port,
    Starboard,
}

/// Which tack a heading is on for a wind from `twd`: wind over the port side = port tack.
fn tack_of(twd: f64, heading: f64) -> Tack {
    if wrap180(twd - heading) < 0.0 {
        Tack::Port
    } else {
        Tack::Starboard
    }
}

pub struct Sailboat {
    nav: Nav,
    now: f64,
    tacking: bool,
    tack_heading: f64,
    tack_start: f64,
    tack_clear: f64,
}

impl Sailboat {
    pub fn new() -> Sailboat {
        Sailboat {
            nav: Nav::new(WP_RADIUS_M, true, LOIT_RADIUS_M),
            now: 0.0,
            tacking: false,
            tack_heading: 0.0,
            tack_start: 0.0,
            tack_clear: f64::NEG_INFINITY,
        }
    }

    fn clear_tack(&mut self) {
        self.tacking = false;
        self.tack_clear = self.now;
    }

    /// ArduRover's Sailboat::calc_heading (with its use_indirect_route check folded in).
    /// `xte` is the cross-track error, positive left of the track.
    fn sail_heading(
        &mut self,
        desired: f64,
        twd: f64,
        heading: f64,
        xte: f64,
        out: &mut Vec<Out>,
    ) -> f64 {
        let off_wind = wrap180(twd - desired);
        if !self.tacking && off_wind.abs() > NO_GO_DEG + NO_GO_PAD_DEG {
            return desired; // can sail straight there
        }
        let current = tack_of(twd, heading);
        let mut should_tack = false;
        if off_wind.abs() > NO_GO_DEG && !self.tacking {
            let new = if off_wind < 0.0 {
                Tack::Port
            } else {
                Tack::Starboard
            };
            // a tack (bow through the wind), not a gybe
            if new != current && wrap180(twd - heading).abs() + off_wind.abs() < 180.0 {
                should_tack = true;
            }
            if !should_tack {
                return desired;
            }
        }
        if xte.abs() >= XTRACK_MAX_M && !should_tack && !self.tacking {
            should_tack =
                (xte > 0.0 && current == Tack::Starboard) || (xte < 0.0 && current == Tack::Port);
        }
        let left = (twd + NO_GO_DEG).rem_euclid(360.0);
        let right = (twd - NO_GO_DEG).rem_euclid(360.0);
        if should_tack && self.now - self.tack_clear > TACK_RETRY_S {
            out.push(Out::Text(
                MavSeverity::MAV_SEVERITY_INFO,
                "Sailboat: Tacking".into(),
            ));
            self.tack_heading = match current {
                Tack::Port => right,
                Tack::Starboard => left,
            };
            self.tacking = true;
            self.tack_start = self.now;
        }
        if self.tacking {
            if wrap180(self.tack_heading - heading).abs() <= TACK_ACCURACY_DEG {
                self.clear_tack();
            } else if self.now - self.tack_start > TACK_TIMEOUT_S {
                out.push(Out::Text(
                    MavSeverity::MAV_SEVERITY_INFO,
                    "Sailboat: Tacking timed out".into(),
                ));
                self.clear_tack();
            }
            return self.tack_heading;
        }
        match current {
            Tack::Port => left,
            Tack::Starboard => right,
        }
    }
}

impl Default for Sailboat {
    fn default() -> Self {
        Sailboat::new()
    }
}

/// True wind as (speed m/s, direction it comes from, deg) from a sample's (east, north) vector.
fn true_wind(t: &Tick) -> (f64, f64) {
    let vector = t
        .water
        .and_then(|s| s.wind_east.zip(s.wind_north))
        .filter(|_| t.water.is_some_and(|s| s.in_area()));
    match vector {
        Some((e, n)) => (e.hypot(n), (-e).atan2(-n).to_degrees().rem_euclid(360.0)),
        None => {
            if t.water.is_none() && !DEFAULT_WIND_LOGGED.swap(true, Ordering::Relaxed) {
                println!(
                    "sailboat stand-ins: no tidewater conditions, sailing in SITL's default wind \
                     ({} m/s from {})",
                    DEFAULT_WIND.0, DEFAULT_WIND.1
                );
            }
            DEFAULT_WIND
        }
    }
}

impl Behavior for Sailboat {
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

    fn marine(&self) -> bool {
        true
    }

    fn step(&mut self, t: &mut Tick) {
        self.now += t.dt;
        let (tws, twd) = true_wind(t);
        let goal = self.nav.goal(t);
        let (lat, lon) = (t.st.lat, t.st.lon);
        // where to go: None = sail let out (HOLD, stopped, inside the loiter circle)
        let course = match goal {
            Goal::Stop => None,
            Goal::Go {
                lat: glat,
                lon: glon,
                stop,
                ..
            } => Some((glat, glon, stop)),
            Goal::Loiter {
                lat: glat,
                lon: glon,
                radius,
            } => (distance_m(lat, lon, glat, glon) > radius).then_some((glat, glon, true)),
            Goal::Circle {
                lat: clat,
                lon: clon,
                radius,
                ..
            } => {
                let h = rover::ring_heading(lat, lon, clat, clon, radius);
                let (alat, alon) = rover::ahead(lat, lon, h, 50.0);
                Some((alat, alon, false))
            }
        };
        let st = &mut *t.st;
        let (want_heading, want_speed) = match course {
            None => {
                self.tacking = false;
                (st.heading, 0.0)
            }
            Some((glat, glon, stop)) => {
                let desired = bearing_deg(lat, lon, glat, glon);
                let xte = self.nav.crosstrack(lat, lon, glat, glon);
                let h = self.sail_heading(desired, twd, st.heading, xte, t.out);
                let cap = if t.spec.max_mps > 0.0 {
                    t.spec.max_mps
                } else {
                    2.0 * t.spec.cruise_mps
                };
                let mut v = polar_speed(twd - st.heading, tws).min(cap);
                if stop {
                    let d = distance_m(lat, lon, glat, glon);
                    v = v.min((2.0 * ACCEL_MPS2 * d).sqrt().max(0.3));
                }
                (h, v)
            }
        };
        // the rudder needs way on: slower turns at low speed, never none (it coasts round)
        let authority = (st.speed / 0.5).clamp(0.3, 1.0);
        let turn = t.spec.turn_dps() * authority * t.dt;
        let err = wrap180(want_heading - st.heading);
        st.heading = (st.heading + err.clamp(-turn, turn)).rem_euclid(360.0);
        // the sail can only drive the boat at what this heading allows
        let can = polar_speed(twd - st.heading, tws);
        let target = want_speed.min(can.max(0.0));
        let dv = if target > st.speed {
            (target - st.speed).min(ACCEL_MPS2 * t.dt)
        } else {
            (target - st.speed).max(-COAST_DECEL_MPS2 * t.dt)
        };
        st.speed = (st.speed + dv).max(0.0);
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
    use super::*;

    #[test]
    fn polar_shape() {
        assert_eq!(polar_speed(0.0, 5.0), 0.0);
        assert_eq!(polar_speed(-30.0, 5.0), 0.0);
        assert!((polar_speed(90.0, 5.0) - 2.25).abs() < 1e-9);
        assert!((polar_speed(-90.0, 5.0) - 2.25).abs() < 1e-9);
        assert!(polar_speed(45.0, 5.0) < polar_speed(60.0, 5.0));
        assert!((polar_speed(180.0, 4.0) - 1.2).abs() < 1e-9);
    }

    #[test]
    fn upwind_waypoint_sails_close_hauled_then_tacks() {
        let mut s = Sailboat::new();
        let mut out = Vec::new();
        // wind from 180, waypoint dead upwind, heading 225 (port tack)
        let h = s.sail_heading(180.0, 180.0, 225.0, 0.0, &mut out);
        assert_eq!(h, 225.0);
        assert!(out.is_empty());
        // drifted 10 m right of the track (negative) on port tack: tack to starboard
        s.now = 100.0;
        let h = s.sail_heading(180.0, 180.0, 225.0, -10.0, &mut out);
        assert_eq!(h, 135.0);
        assert_eq!(
            out,
            vec![Out::Text(
                MavSeverity::MAV_SEVERITY_INFO,
                "Sailboat: Tacking".into()
            )]
        );
        // through the wind and within 10 deg: tack done, starboard close-hauled
        let h = s.sail_heading(180.0, 180.0, 140.0, -10.0, &mut out);
        assert_eq!(h, 135.0);
        assert!(!s.tacking);
        // a reachable waypoint is sailed directly
        assert_eq!(s.sail_heading(90.0, 180.0, 140.0, 0.0, &mut out), 90.0);
    }

    #[test]
    fn tack_times_out() {
        let mut s = Sailboat::new();
        let mut out = Vec::new();
        s.now = 10.0;
        s.sail_heading(180.0, 180.0, 225.0, -12.0, &mut out);
        assert!(s.tacking);
        s.now = 16.0;
        s.sail_heading(180.0, 180.0, 200.0, -12.0, &mut out);
        assert!(!s.tacking);
        assert_eq!(
            out.last(),
            Some(&Out::Text(
                MavSeverity::MAV_SEVERITY_INFO,
                "Sailboat: Tacking timed out".into()
            ))
        );
    }
}
