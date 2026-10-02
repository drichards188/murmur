//! Navigation truth vs reported state for vehicles without GPS (a submerged AUV).
//!
//! While the behavior reports no GPS fix, the vehicle navigates on inertial + DVL dead
//! reckoning and its estimate drifts from the truth. `State` holds the estimate (what the
//! autopilot steers by and reports); this keeps `error = estimate - truth`. On surfacing the
//! GPS needs a moment to fix again, then the estimate snaps back to the truth.
//!
//! The model (all figures are estimates, chosen to land in the error growth quoted for
//! DVL-aided INS on survey AUVs: roughly 0.1-0.5 % of distance travelled):
//! - a constant heading error per vehicle, normal with sd `HEADING_ERR_SD`, that also
//!   random-walks slowly (gyro drift, `HEADING_WALK`): error grows across track with
//!   distance travelled;
//! - a constant DVL scale-factor error, sd `SCALE_ERR_SD`: error grows along track;
//! - white velocity noise integrated into a position random walk (`POS_WALK`);
//! - water current the DVL can't see: beyond `DVL_RANGE_M` above the seabed it loses bottom
//!   lock and measures speed through the water, so the current's drift goes unmeasured.
//!   With no depth figure it is assumed to keep bottom lock (coastal water).
//!
//! The truth never goes on the MAVLink wire; it goes to the truth log for evaluation.

use mavlink::dialects::ardupilotmega::MavSeverity;

use crate::conditions::Sample;
use crate::geo::Gauss;
use crate::kinematics::Out;

/// Heading error of the INS, rad (0.11 degrees).
pub const HEADING_ERR_SD: f64 = 0.002;
/// Random walk of that heading error, rad per sqrt(s).
pub const HEADING_WALK: f64 = 1.0e-5;
/// DVL scale-factor error (0.2 %).
pub const SCALE_ERR_SD: f64 = 0.002;
/// Position random walk from velocity noise, m per sqrt(s).
pub const POS_WALK: f64 = 0.01;
/// Bottom-lock range of a 600 kHz-class DVL, m above the seabed.
pub const DVL_RANGE_M: f64 = 80.0;
/// From surfacing to a usable GPS fix, s (antenna clear of the water, hot start).
pub const GPS_REACQUIRE_S: f64 = 10.0;

#[derive(Debug, Clone)]
pub struct Nav {
    /// estimate - truth, metres north and east
    pub err_n: f64,
    pub err_e: f64,
    heading_err: f64,
    scale_err: f64,
    /// seconds without a GPS fix (0 with one)
    pub dead_reckoning_s: f64,
    /// whether the reported position has a GPS fix behind it
    pub gps: bool,
    /// GPS sees the sky again (behavior's fix) since this time; the fix is usable after
    /// GPS_REACQUIRE_S
    sky_since: Option<f64>,
    /// the true position at the last fix (what GPS_RAW_INT keeps reporting without one)
    pub last_fix: (f64, f64, f64),
    /// has ever lost GPS (the truth log follows these)
    pub tracked: bool,
}

impl Nav {
    pub fn new(rng: &mut Gauss, at: (f64, f64, f64)) -> Nav {
        Nav {
            err_n: 0.0,
            err_e: 0.0,
            heading_err: rng.sample(0.0, HEADING_ERR_SD),
            scale_err: rng.sample(0.0, SCALE_ERR_SD),
            dead_reckoning_s: 0.0,
            gps: true,
            sky_since: None,
            last_fix: at,
            tracked: false,
        }
    }

    pub fn error_m(&self) -> f64 {
        self.err_n.hypot(self.err_e)
    }

    /// One tick. `sky`: the behavior has a GPS fix (above water); `(ve, vn)`: true velocity
    /// over ground; `water`: tidewater sample at the true position; `truth`: true position.
    /// Returns how far the estimate moves (north, east metres) on top of the physics.
    #[allow(clippy::too_many_arguments)]
    pub fn step(
        &mut self,
        dt: f64,
        now: f64,
        sky: bool,
        (ve, vn): (f64, f64),
        depth_m: f64,
        water: Option<&Sample>,
        truth: (f64, f64, f64),
        rng: &mut Gauss,
        out: &mut Vec<Out>,
    ) -> (f64, f64) {
        if sky {
            let since = *self.sky_since.get_or_insert(now);
            if !self.gps && now - since >= GPS_REACQUIRE_S {
                // fix again: the EKF resets onto GPS
                self.gps = true;
                self.dead_reckoning_s = 0.0;
                out.push(Out::Text(
                    MavSeverity::MAV_SEVERITY_INFO,
                    "EKF3 IMU0 is using GPS".into(),
                ));
                let snap = (-self.err_n, -self.err_e);
                self.err_n = 0.0;
                self.err_e = 0.0;
                self.last_fix = truth;
                return snap;
            }
            if self.gps {
                self.last_fix = truth;
                return (0.0, 0.0);
            }
        } else {
            self.sky_since = None;
            if self.gps {
                self.gps = false;
                self.tracked = true;
            }
        }
        if self.gps {
            return (0.0, 0.0);
        }
        // dead reckoning
        self.dead_reckoning_s += dt;
        self.heading_err += rng.sample(0.0, HEADING_WALK * dt.sqrt());
        let (h, s) = (self.heading_err, self.scale_err);
        // velocity the INS integrates, minus the true one: rotated by h, scaled by 1 + s
        let mut de = (ve * h.cos() + vn * h.sin()) * (1.0 + s) - ve;
        let mut dn = (vn * h.cos() - ve * h.sin()) * (1.0 + s) - vn;
        if let Some(w) = water.filter(|w| w.in_area()) {
            let above_seabed = w.water_depth_m.map_or(0.0, |d| d - depth_m);
            if above_seabed > DVL_RANGE_M {
                de -= w.current_east();
                dn -= w.current_north();
            }
        }
        let walk = POS_WALK * dt.sqrt();
        let step_n = dn * dt + rng.sample(0.0, walk);
        let step_e = de * dt + rng.sample(0.0, walk);
        self.err_n += step_n;
        self.err_e += step_e;
        (step_n, step_e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_grows_submerged_and_resets_after_surfacing() {
        let mut rng = Gauss::new(3);
        let mut nav = Nav::new(&mut rng, (0.0, 0.0, 0.0));
        // force a visible bias
        nav.heading_err = 0.003;
        nav.scale_err = 0.002;
        let mut out = Vec::new();
        let dt = 0.1;
        // one hour at 2 m/s, submerged: 7.2 km travelled
        for i in 0..36_000 {
            let now = f64::from(i) * dt;
            nav.step(
                dt,
                now,
                false,
                (2.0, 0.0),
                40.0,
                None,
                (0.0, 0.0, -40.0),
                &mut rng,
                &mut out,
            );
        }
        assert!(!nav.gps);
        let e = nav.error_m();
        assert!(
            e > 7.2 && e < 60.0,
            "{e:.1} m after 7.2 km: 0.1-0.8 % of distance"
        );
        assert!((nav.dead_reckoning_s - 3600.0).abs() < 1.0);
        // surface: no fix for GPS_REACQUIRE_S, then the estimate snaps back
        let mut now = 3600.0;
        let mut snapped = (0.0, 0.0);
        while now < 3600.0 + GPS_REACQUIRE_S + 1.0 {
            let d = nav.step(
                dt,
                now,
                true,
                (0.0, 0.0),
                0.0,
                None,
                (1.0, 2.0, 0.0),
                &mut rng,
                &mut out,
            );
            if nav.gps && snapped == (0.0, 0.0) {
                snapped = d;
            }
            now += dt;
        }
        assert!(nav.gps);
        assert_eq!(nav.error_m(), 0.0);
        assert!(
            (snapped.0.hypot(snapped.1) - e).abs() < 1.0,
            "snap undoes the error"
        );
        assert_eq!(nav.last_fix, (1.0, 2.0, 0.0));
        assert!(
            out.iter()
                .any(|o| matches!(o, Out::Text(_, s) if s == "EKF3 IMU0 is using GPS"))
        );
    }

    #[test]
    fn unseen_current_beyond_dvl_range() {
        let mut rng = Gauss::new(5);
        let mut nav = Nav::new(&mut rng, (0.0, 0.0, 0.0));
        nav.heading_err = 0.0;
        nav.scale_err = 0.0;
        let deep = Sample {
            elevation_m: Some(-500.0),
            water_depth_m: Some(500.0),
            current_east: Some(0.5),
            current_north: Some(0.0),
            ..Default::default()
        };
        let mut out = Vec::new();
        for i in 0..6000 {
            let now = f64::from(i) * 0.1;
            nav.step(
                0.1,
                now,
                false,
                (0.5, 0.0),
                40.0,
                Some(&deep),
                (0.0, 0.0, -40.0),
                &mut rng,
                &mut out,
            );
        }
        // 10 minutes of a 0.5 m/s current it can't see: ~300 m behind (estimate west of truth)
        assert!((nav.err_e + 300.0).abs() < 10.0, "{}", nav.err_e);
    }
}
