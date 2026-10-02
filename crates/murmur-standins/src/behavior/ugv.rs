//! Ground vehicle (ArduRover, skid steer): turns in place, stops at the shoreline, slows on
//! steep ground.
//!
//! Wire: MAV_TYPE_GROUND_ROVER, Rover mode numbers, ArduRover's mission messages (rover.rs).
//!
//! Movement: skid steering turns at up to ATC_STR_RAT_MAX (120 deg/s), limited at speed by
//! ATC_TURN_MAX_G (0.6 g lateral); forward speed scales with cos(heading error), so a large
//! heading error turns it on the spot before it drives (pivot turns, WP_PIVOT_ANGLE, are off by
//! default in ArduRover and here). WP_RADIUS 3 m, as the rover-skid SITL frame.
//!
//! Land only (with tidewater conditions): the latest sample is where the vehicle was up to a
//! poll ago. A wet sample (water depth > 0) is a shoreline: the vehicle stops, backs up to the
//! last dry sample point, says so in a STATUSTEXT once, and refuses to drive toward the water
//! (within 60 deg of the bearing from the last dry point to the wet one). In AUTO it stays on
//! that waypoint, so the mission does not complete. Driving away (another mode, another
//! waypoint) clears the block once 15 m from the wet point.
//!
//! Slope model: grade along the track from consecutive samples (smoothed). Full speed up to a
//! 10 % grade, then linearly down to 30 % of the commanded speed at a 60 % grade (31 deg),
//! 30 % beyond; uphill and downhill alike. Altitude follows the DEM relative to the spawn
//! point. Without tidewater there is no land check and no slope (logged once).

use std::sync::atomic::{AtomicBool, Ordering};

use mavlink::dialects::ardupilotmega::{MavSeverity, MavType};

use super::rover::{self, Goal, Nav};
use super::{Behavior, Tick, modes};
use crate::conditions::Sample;
use crate::geo::{bearing_deg, distance_m, wrap180};
use crate::kinematics::{Out, State, integrate};

const WP_RADIUS_M: f64 = 3.0;
const STR_RAT_MAX_DPS: f64 = 120.0;
const TURN_MAX_G: f64 = 0.6;
const ACCEL_MPS2: f64 = 1.0;
/// reversing off the shoreline
const BACKUP_MPS: f64 = 1.0;
const BLOCK_CONE_DEG: f64 = 60.0;
const BLOCK_CLEAR_M: f64 = 15.0;

static NO_CONDITIONS_LOGGED: AtomicBool = AtomicBool::new(false);

/// Speed factor for a grade (rise over run, either sign). See the module docs.
pub fn slope_factor(grade: f64) -> f64 {
    let g = grade.abs();
    if g <= 0.1 {
        1.0
    } else {
        (1.0 - (g - 0.1) / 0.5 * 0.7).max(0.3)
    }
}

#[derive(Debug, Clone, Copy)]
struct Block {
    wet: (f64, f64),
    dry: Option<(f64, f64)>,
    /// bearing from the dry side toward the water
    bearing: f64,
    backing: bool,
}

pub struct Ugv {
    nav: Nav,
    last_dry: Option<(f64, f64)>,
    block: Option<Block>,
    /// last sample used for the grade: (lat, lon, elevation)
    prev: Option<(f64, f64, f64)>,
    grade: f64,
    /// DEM elevation at the first sample, for terrain-following altitude
    elev0: Option<f64>,
    looked: bool,
}

impl Ugv {
    pub fn new() -> Ugv {
        Ugv {
            nav: Nav::new(WP_RADIUS_M, false, 2.0),
            last_dry: None,
            block: None,
            prev: None,
            grade: 0.0,
            elev0: None,
            looked: false,
        }
    }

    /// Land/water and slope from the latest sample.
    fn terrain(&mut self, t: &mut Tick) {
        let Some(s) = t.water else {
            if !self.looked && !NO_CONDITIONS_LOGGED.swap(true, Ordering::Relaxed) {
                println!("ugv stand-ins: no tidewater conditions, so no land/water check or slope");
            }
            self.looked = true;
            return;
        };
        self.looked = true;
        if !s.in_area() {
            return;
        }
        let elev = s.elevation_m.unwrap_or(0.0);
        self.slope(s, elev);
        let base = *self.elev0.get_or_insert(elev);
        let want_alt = t.st.home.2 + (elev - base);
        t.st.vz = (want_alt - t.st.alt).clamp(-2.0, 2.0);
        t.st.alt += t.st.vz * t.dt;
        if wet(s) {
            if self.block.is_none() {
                let bearing = match self.last_dry {
                    Some((dlat, dlon)) if distance_m(dlat, dlon, s.lat, s.lon) > 0.5 => {
                        bearing_deg(dlat, dlon, s.lat, s.lon)
                    }
                    _ => t.st.heading,
                };
                self.block = Some(Block {
                    wet: (s.lat, s.lon),
                    dry: self.last_dry,
                    bearing,
                    backing: self.last_dry.is_some(),
                });
                t.out.push(Out::Text(
                    MavSeverity::MAV_SEVERITY_WARNING,
                    "Shoreline: water ahead, stopped".into(),
                ));
            }
        } else {
            self.last_dry = Some((s.lat, s.lon));
        }
    }

    fn slope(&mut self, s: &Sample, elev: f64) {
        if let Some((plat, plon, pelev)) = self.prev {
            let d = distance_m(plat, plon, s.lat, s.lon);
            if d < 2.0 {
                return; // same sample, or too close for a grade
            }
            let g = (elev - pelev) / d;
            self.grade = 0.5 * self.grade + 0.5 * g;
        }
        self.prev = Some((s.lat, s.lon, elev));
    }

    /// Is driving on `bearing` toward the water that blocked us? Clears the block once clear.
    fn blocked(&mut self, st: &State, bearing: f64) -> bool {
        let Some(b) = self.block else { return false };
        if distance_m(st.lat, st.lon, b.wet.0, b.wet.1) > BLOCK_CLEAR_M && !b.backing {
            self.block = None;
            return false;
        }
        wrap180(bearing - b.bearing).abs() < BLOCK_CONE_DEG
    }
}

impl Default for Ugv {
    fn default() -> Self {
        Ugv::new()
    }
}

fn wet(s: &Sample) -> bool {
    s.water_depth_m.is_some_and(|d| d > 0.0)
}

impl Behavior for Ugv {
    fn mav_type(&self) -> MavType {
        MavType::MAV_TYPE_GROUND_ROVER
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

    /// Not marine, but it needs tidewater samples (elevation, water depth) for the land check;
    /// `marine()` is what gives a behavior conditions.
    fn marine(&self) -> bool {
        true
    }

    fn step(&mut self, t: &mut Tick) {
        self.terrain(t);
        let goal = self.nav.goal(t);
        let st = &mut *t.st;
        let dt = t.dt;

        // backing off the shoreline comes first
        if let Some(b) = self.block.as_mut().filter(|b| b.backing) {
            match b.dry {
                Some((dlat, dlon)) if distance_m(st.lat, st.lon, dlat, dlon) > 0.3 => {
                    // reverse along the line we came in on, nose still toward the water
                    let away = bearing_deg(st.lat, st.lon, dlat, dlon);
                    st.heading = (away + 180.0).rem_euclid(360.0);
                    st.speed = -BACKUP_MPS;
                    integrate(st, dt, 0.0, 0.0);
                    return;
                }
                _ => b.backing = false,
            }
        }

        let (want_heading, mut want_speed) = match goal {
            Goal::Stop => (st.heading, 0.0),
            Goal::Go {
                lat,
                lon,
                speed,
                stop,
            } => {
                let d = distance_m(st.lat, st.lon, lat, lon);
                let mut v = speed;
                if stop {
                    v = v.min((2.0 * ACCEL_MPS2 * (d - 0.5).max(0.0)).sqrt());
                }
                (bearing_deg(st.lat, st.lon, lat, lon), v)
            }
            Goal::Circle {
                lat,
                lon,
                radius,
                speed,
            } => (rover::ring_heading(st.lat, st.lon, lat, lon, radius), speed),
            // a ground rover's loiter is a stop (Nav only asks boats to loiter)
            Goal::Loiter { .. } => (st.heading, 0.0),
        };
        want_speed *= slope_factor(self.grade);
        if want_speed > 0.0 && self.blocked(st, want_heading) {
            want_speed = 0.0; // turning in place is still allowed
        }
        // skid steer: turn rate limited by the steering rate and, at speed, lateral accel
        let v = st.speed.abs().max(0.1);
        let rate = STR_RAT_MAX_DPS.min((TURN_MAX_G * 9.81 / v).to_degrees());
        let err = wrap180(want_heading - st.heading);
        st.heading = (st.heading + err.clamp(-rate * dt, rate * dt)).rem_euclid(360.0);
        let err = wrap180(want_heading - st.heading);
        let v_cmd = want_speed * err.to_radians().cos().max(0.0);
        let dv = (v_cmd - st.speed).clamp(-ACCEL_MPS2 * dt, ACCEL_MPS2 * dt);
        st.speed += dv;
        if st.speed.abs() < 1e-3 {
            st.speed = 0.0;
        }
        integrate(st, dt, 0.0, 0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slope_model() {
        assert_eq!(slope_factor(0.05), 1.0);
        assert_eq!(slope_factor(-0.1), 1.0);
        assert!((slope_factor(0.35) - 0.65).abs() < 1e-9);
        assert!((slope_factor(-0.6) - 0.3).abs() < 1e-9);
        assert_eq!(slope_factor(2.0), 0.3);
    }

    use crate::config::Spec;
    use crate::geo::{Gauss, M_PER_DEG};
    use crate::mission::Mission;
    use mavlink::dialects::ardupilotmega::{MISSION_ITEM_INT_DATA, MavCmd, MavFrame};

    #[allow(deprecated)]
    fn wp(seq: u16, north_m: f64) -> MISSION_ITEM_INT_DATA {
        MISSION_ITEM_INT_DATA {
            seq,
            command: MavCmd::MAV_CMD_NAV_WAYPOINT,
            x: ((37.8 + north_m / M_PER_DEG) * 1e7) as i32,
            y: (-122.45 * 1e7) as i32,
            frame: MavFrame::MAV_FRAME_GLOBAL_RELATIVE_ALT_INT,
            ..Default::default()
        }
    }

    /// Drive north toward a waypoint in the water (wet beyond 50 m north); samples lag one
    /// second behind the vehicle, as the poller's do.
    fn drive(
        secs: f64,
        mode: &'static str,
        ugv: &mut Ugv,
        st: &mut State,
        m: &mut Mission,
    ) -> Vec<Out> {
        let spec = Spec {
            name: "ugv".into(),
            behavior: "ugv".into(),
            cruise_mps: 3.0,
            max_mps: 0.0,
            climb_mps: None,
            turn_dps: None,
            loiter_radius_m: None,
            endurance_h: None,
        };
        let mut rng = Gauss::new(1);
        let mut out = Vec::new();
        let mut trail = std::collections::VecDeque::new();
        st.mode = mode;
        for _ in 0..(secs * 10.0) as usize {
            trail.push_back((st.lat, st.lon));
            let (lat, lon) = if trail.len() > 10 {
                trail.pop_front().unwrap_or_default()
            } else {
                trail[0]
            };
            let north = (lat - 37.8) * M_PER_DEG;
            let sample = Sample {
                lat,
                lon,
                elevation_m: Some(2.0 - north * 0.05),
                water_depth_m: Some(north * 0.05 - 2.5),
                ..Default::default()
            };
            ugv.step(&mut Tick {
                spec: &spec,
                st,
                mission: m,
                water: Some(&sample),
                rng: &mut rng,
                out: &mut out,
                dt: 0.1,
            });
        }
        out
    }

    #[test]
    fn stops_at_the_shoreline_and_can_leave() {
        let mut ugv = Ugv::new();
        let mut st = State {
            lat: 37.8,
            lon: -122.45,
            alt: 2.0,
            heading: 90.0, // facing east: turns in place first
            speed: 0.0,
            vz: 0.0,
            ve: 0.0,
            vn: 0.0,
            home: (37.8, -122.45, 2.0),
            armed: true,
            mode: "MANUAL",
            hold_point: (37.8, -122.45),
            aground: false,
        };
        let mut m = Mission::default();
        m.install(vec![wp(0, 0.0), wp(1, 100.0)]);
        let out = drive(60.0, "AUTO", &mut ugv, &mut st, &mut m);
        let north = (st.lat - 37.8) * M_PER_DEG;
        assert!((45.0..=50.0).contains(&north), "stopped {north:.1} m north");
        let shore = out
            .iter()
            .filter(|o| matches!(o, Out::Text(_, s) if s.starts_with("Shoreline")))
            .count();
        assert_eq!(shore, 1);
        assert!(!out.iter().any(|o| matches!(o, Out::ItemReached(_))));
        assert_eq!(m.seq, 1, "still on the waypoint in the water");
        // RTL drives away from the water and gets home
        let out = drive(40.0, "RTL", &mut ugv, &mut st, &mut m);
        assert!(
            out.iter()
                .any(|o| matches!(o, Out::Text(_, s) if s == "Reached destination"))
        );
        assert!((st.lat - 37.8) * M_PER_DEG < 3.0);
    }
}
