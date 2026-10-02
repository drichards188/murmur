//! Navigation truth vs reported state: what the autopilot's EKF believes when GNSS is lost,
//! denied or spoofed.
//!
//! `State` holds the estimate (what the autopilot steers by and reports); this keeps
//! `error = estimate - truth`. The receiver side (satellites, fix, jamming) is gnss.rs; this
//! is the EKF3 side, as NavEKF3_core runs it (AP_NavEKF3_Control.cpp setAidingMode and
//! updateFilterStatus, timings from AP_NavEKF3.h):
//!
//! - **With a 3D fix** the EKF fuses GPS: absolute position, error 0 (or, spoofed, the
//!   spoofer's offset: the EKF follows the receiver).
//! - **Fix lost**: for `posRetryTimeUseVel_ms` (10 s) when something still constrains the
//!   velocity (a sub's DVL, a plane's airspeed), else `posRetryTimeNoVel_ms` (7 s), the
//!   flags still claim absolute position (posTimeout not yet set) while it coasts.
//! - Then, with velocity aiding, **dead reckoning**: relative position only (horiz_pos_rel
//!   from body odometry or wind-relative navigation, `dead_reckoning` flag). Without it
//!   (copter, rover, boat, blimp: no optical flow, wheel encoders or drag fusion modelled),
//!   **no horizontal position** at all; after `tiltDriftTimeMax_ms` (15 s) the filter stops
//!   aiding ("EKF3 IMU0 stopped aiding", constant position mode: it reports the last known
//!   position while the vehicle moves on).
//! - **Fix again** (after `GPS_REACQUIRE_S`): the EKF resets onto GPS and the error goes back
//!   to 0. From constant position mode it starts aiding again ("EKF3 IMU0 is using GPS");
//!   otherwise it never stopped aiding and says nothing.
//! - **Spoofing ends** with the spoofer's offset still in the estimate: the true fix is that
//!   far off, outside the innovation gate (`GLITCH_M`), so the EKF rejects GPS
//!   (gps_glitching) for 10 s, then resets onto it.
//!
//! The drift model while dead reckoning (all figures are estimates; for a sub they land in
//! the error growth quoted for DVL-aided INS on survey AUVs, roughly 0.1-0.5 % of distance
//! travelled):
//! - a constant heading error per vehicle, normal with sd `HEADING_ERR_SD`, that also
//!   random-walks slowly (gyro drift, `HEADING_WALK`): error grows across track;
//! - a constant velocity scale-factor error (DVL or airspeed), sd `SCALE_ERR_SD`: error grows
//!   along track;
//! - white velocity noise integrated into a position random walk (`POS_WALK`);
//! - what the aiding sensor can't see: water current beyond `DVL_RANGE_M` above the seabed
//!   (the DVL loses bottom lock and measures speed through the water), the change in wind
//!   since the plane's wind estimate was last observable (`WIND_ERR_SD`);
//! - without velocity aiding: an accelerometer bias integrated twice (`ACCEL_BIAS_SD`) until
//!   the filter stops aiding, and for an aircraft the wind it can no longer see
//!   (`DRIFT_SD`) carrying it off its frozen estimate.
//!
//! The truth never goes on the MAVLink wire; it goes to the truth log for evaluation.

use mavlink::dialects::ardupilotmega::MavSeverity;

use crate::behavior::Firmware;
use crate::conditions::Sample;
use crate::geo::Gauss;
use crate::gnss::Spoof;
use crate::kinematics::Out;

/// Heading error of the INS, rad (0.11 degrees).
pub const HEADING_ERR_SD: f64 = 0.002;
/// Random walk of that heading error, rad per sqrt(s).
pub const HEADING_WALK: f64 = 1.0e-5;
/// DVL or airspeed scale-factor error (0.2 %).
pub const SCALE_ERR_SD: f64 = 0.002;
/// Position random walk from velocity noise, m per sqrt(s).
pub const POS_WALK: f64 = 0.01;
/// Bottom-lock range of a 600 kHz-class DVL, m above the seabed.
pub const DVL_RANGE_M: f64 = 80.0;
/// From the antenna seeing the sky (surfacing, leaving a jammer) to a usable fix, s.
pub const GPS_REACQUIRE_S: f64 = 10.0;
/// Wind change a plane's frozen wind estimate misses, m/s per axis.
pub const WIND_ERR_SD: f64 = 1.0;
/// Unaided accelerometer bias, m/s^2 per axis (MEMS class).
pub const ACCEL_BIAS_SD: f64 = 0.05;
/// Wind that carries an aircraft without position aiding off its estimate, m/s per axis.
pub const DRIFT_SD: f64 = 1.5;
/// EKF3 posRetryTimeUseVel_ms / posRetryTimeNoVel_ms / tiltDriftTimeMax_ms, s.
pub const POS_RETRY_VEL_S: f64 = 10.0;
pub const POS_RETRY_NO_VEL_S: f64 = 7.0;
pub const TILT_DRIFT_MAX_S: f64 = 15.0;
/// A GPS jump beyond this fails the EKF's position innovation gate, m (EK3_POS_I_GATE 5
/// sigma on a few metres of GPS accuracy; estimate).
pub const GLITCH_M: f64 = 25.0;

/// What constrains the velocity when GPS is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aid {
    /// a sub's DVL (EKF3 body odometry)
    Dvl,
    /// a plane's airspeed and zero-sideslip fusion (wind-relative dead reckoning)
    Airspeed,
    /// nothing (no optical flow, wheel encoders or drag fusion)
    None,
}

impl Aid {
    pub fn for_firmware(fw: Firmware) -> Aid {
        match fw {
            Firmware::Sub => Aid::Dvl,
            Firmware::Plane => Aid::Airspeed,
            _ => Aid::None,
        }
    }
}

/// The EKF's horizontal navigation state, as EKF_STATUS_REPORT shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ekf {
    /// fusing GPS
    Gps,
    /// rejecting GPS (innovation gate) but not yet timed out: still absolute, glitching
    Glitch,
    /// GPS lost, not yet timed out: flags still absolute
    Coasting,
    /// relative position only
    DeadReckoning,
    /// no horizontal position, still trying
    NoPosition,
    /// stopped aiding: reports the last known position
    ConstPos,
}

impl Ekf {
    pub fn name(self) -> &'static str {
        match self {
            Ekf::Gps => "gps",
            Ekf::Glitch => "glitch",
            Ekf::Coasting => "coasting",
            Ekf::DeadReckoning => "dead_reckoning",
            Ekf::NoPosition => "no_position",
            Ekf::ConstPos => "const_pos",
        }
    }

    pub fn abs(self) -> bool {
        matches!(self, Ekf::Gps | Ekf::Glitch | Ekf::Coasting)
    }

    pub fn rel(self) -> bool {
        self.abs() || self == Ekf::DeadReckoning
    }
}

/// One tick's inputs.
#[derive(Debug, Clone, Copy)]
pub struct Input<'a> {
    pub dt: f64,
    pub now: f64,
    /// the receiver has signals to fix on (antenna above water, enough satellites tracked)
    pub signal: bool,
    /// a spoofer has the receiver
    pub spoof: Option<Spoof>,
    /// true velocity over ground (east, north), m/s
    pub vel: (f64, f64),
    /// how far the physics moved the estimate this tick (north, east), m
    pub moved: (f64, f64),
    pub depth_m: f64,
    pub water: Option<&'a Sample>,
    /// true position
    pub truth: (f64, f64, f64),
    pub airborne: bool,
}

#[derive(Debug, Clone)]
pub struct Nav {
    /// estimate - truth, metres north and east
    pub err_n: f64,
    pub err_e: f64,
    heading_err: f64,
    scale_err: f64,
    pub aid: Aid,
    pub ekf: Ekf,
    /// seconds without GPS in the EKF (0 while fusing it)
    pub dead_reckoning_s: f64,
    /// the EKF is fusing GPS (Gps or Glitch)
    pub gps: bool,
    /// the receiver has a usable 3D fix
    pub fix: bool,
    /// signals since this time; the fix is usable GPS_REACQUIRE_S later
    sky_since: Option<f64>,
    /// when the EKF last had GPS, and when it began rejecting it
    lost_at: f64,
    glitch_at: f64,
    /// the receiver's position: the truth (plus any spoofing) at the last fix; GPS_RAW_INT
    /// keeps reporting it without one
    pub last_fix: (f64, f64, f64),
    /// the spoofer's current offset in the receiver's position, metres north and east
    pub spoof_n: f64,
    pub spoof_e: f64,
    pub spoofed: bool,
    /// per outage: unseen velocity (wind, current) and accelerometer bias, m/s and m/s^2
    drift: (f64, f64),
    bias: (f64, f64),
    /// has ever lost GPS or been spoofed (the truth log follows these)
    pub tracked: bool,
}

const INFO: MavSeverity = MavSeverity::MAV_SEVERITY_INFO;
const WARN: MavSeverity = MavSeverity::MAV_SEVERITY_WARNING;

impl Nav {
    /// `fix`: whether the receiver has a fix at spawn (a sub spawned under water has none).
    pub fn new(rng: &mut Gauss, at: (f64, f64, f64), aid: Aid, fix: bool) -> Nav {
        Nav {
            err_n: 0.0,
            err_e: 0.0,
            heading_err: rng.sample(0.0, HEADING_ERR_SD),
            scale_err: rng.sample(0.0, SCALE_ERR_SD),
            aid,
            ekf: match (fix, aid) {
                (true, _) => Ekf::Gps,
                (false, Aid::None) => Ekf::NoPosition,
                (false, _) => Ekf::DeadReckoning,
            },
            dead_reckoning_s: 0.0,
            gps: fix,
            fix,
            sky_since: None,
            lost_at: 0.0,
            glitch_at: 0.0,
            last_fix: at,
            spoof_n: 0.0,
            spoof_e: 0.0,
            spoofed: false,
            drift: (0.0, 0.0),
            bias: (0.0, 0.0),
            tracked: !fix,
        }
    }

    pub fn error_m(&self) -> f64 {
        self.err_n.hypot(self.err_e)
    }

    pub fn spoof_m(&self) -> f64 {
        self.spoof_n.hypot(self.spoof_e)
    }

    fn set_err(&mut self, n: f64, e: f64) -> (f64, f64) {
        let shift = (n - self.err_n, e - self.err_e);
        self.err_n = n;
        self.err_e = e;
        shift
    }

    /// One tick. Returns how far the estimate moves (north, east metres) on top of the
    /// physics.
    pub fn step(&mut self, i: &Input, rng: &mut Gauss, out: &mut Vec<Out>) -> (f64, f64) {
        // the receiver: a fix GPS_REACQUIRE_S after signals return
        if i.signal {
            let since = *self.sky_since.get_or_insert(i.now);
            if !self.fix && i.now - since >= GPS_REACQUIRE_S {
                self.fix = true;
            }
        } else {
            self.sky_since = None;
            self.fix = false;
        }
        // the spoofer: the receiver's position walks off the truth while it is captured
        match i.spoof.filter(|_| self.fix) {
            Some(s) => {
                if !self.spoofed {
                    self.spoofed = true;
                    self.tracked = true;
                }
                let b = s.bearing_deg.to_radians();
                let (n, e) = (
                    self.spoof_n + s.walk_mps * i.dt * b.cos(),
                    self.spoof_e + s.walk_mps * i.dt * b.sin(),
                );
                let m = n.hypot(e);
                let k = if m > s.max_offset_m {
                    s.max_offset_m / m
                } else {
                    1.0
                };
                (self.spoof_n, self.spoof_e) = (n * k, e * k);
            }
            None if self.spoofed => {
                // the true signals again: the receiver jumps back by the whole offset
                self.spoofed = false;
                (self.spoof_n, self.spoof_e) = (0.0, 0.0);
            }
            None => {}
        }
        if self.fix {
            self.fixed(i, rng, out)
        } else {
            self.unaided(i, rng, out)
        }
    }

    /// With a fix: fuse it, or (glitch) reject it until the retry timeout.
    fn fixed(&mut self, i: &Input, rng: &mut Gauss, out: &mut Vec<Out>) -> (f64, f64) {
        let (tn, te, ta) = i.truth;
        let m_per_deg = crate::geo::M_PER_DEG;
        self.last_fix = (
            tn + self.spoof_n / m_per_deg,
            te + self.spoof_e / (m_per_deg * tn.to_radians().cos()),
            ta,
        );
        let (gn, ge) = (self.spoof_n, self.spoof_e); // GPS position - truth
        let jump = (gn - self.err_n).hypot(ge - self.err_e);
        match self.ekf {
            Ekf::Gps if jump > GLITCH_M => {
                self.ekf = Ekf::Glitch;
                self.glitch_at = i.now;
                self.gps = true;
                self.drift_step(i, rng)
            }
            Ekf::Glitch if jump > GLITCH_M && i.now - self.glitch_at < POS_RETRY_VEL_S => {
                self.drift_step(i, rng)
            }
            prev => {
                if prev == Ekf::ConstPos {
                    out.push(Out::Text(INFO, "EKF3 IMU0 is using GPS".into()));
                }
                self.ekf = Ekf::Gps;
                self.gps = true;
                self.dead_reckoning_s = 0.0;
                self.set_err(gn, ge)
            }
        }
    }

    /// Without a fix: coast, dead reckon or lose position, as the aiding allows.
    fn unaided(&mut self, i: &Input, rng: &mut Gauss, out: &mut Vec<Out>) -> (f64, f64) {
        if self.ekf.abs() && self.ekf != Ekf::Coasting {
            self.ekf = Ekf::Coasting;
            self.gps = false;
            self.lost_at = i.now;
            self.tracked = true;
            let (sd_drift, sd_bias) = match self.aid {
                Aid::Dvl => (0.0, 0.0),
                Aid::Airspeed => (WIND_ERR_SD, 0.0),
                Aid::None => (if i.airborne { DRIFT_SD } else { 0.0 }, ACCEL_BIAS_SD),
            };
            self.drift = (rng.sample(0.0, sd_drift), rng.sample(0.0, sd_drift));
            self.bias = (rng.sample(0.0, sd_bias), rng.sample(0.0, sd_bias));
        }
        let lost = i.now - self.lost_at;
        let next = match (self.ekf, self.aid) {
            (Ekf::Coasting, Aid::Dvl | Aid::Airspeed) if lost >= POS_RETRY_VEL_S => {
                Ekf::DeadReckoning
            }
            (Ekf::Coasting, Aid::None) if lost >= POS_RETRY_NO_VEL_S => Ekf::NoPosition,
            (Ekf::Coasting | Ekf::NoPosition, Aid::None) if lost >= TILT_DRIFT_MAX_S => {
                out.push(Out::Text(WARN, "EKF3 IMU0 stopped aiding".into()));
                Ekf::ConstPos
            }
            (e, _) => e,
        };
        self.ekf = next;
        self.dead_reckoning_s += i.dt;
        if self.ekf == Ekf::ConstPos {
            // the estimate stays put; the vehicle moves (and drifts) on
            let (mn, me) = i.moved;
            let (dn, de) = (self.drift.1 * i.dt, self.drift.0 * i.dt);
            self.err_n -= mn + dn;
            self.err_e -= me + de;
            return (-mn, -me);
        }
        match self.aid {
            Aid::None => {
                // unaided inertial: the bias integrates twice
                let (n, e) = (
                    self.bias.1 * lost * i.dt - self.drift.1 * i.dt,
                    self.bias.0 * lost * i.dt - self.drift.0 * i.dt,
                );
                self.err_n += n;
                self.err_e += e;
                (n, e)
            }
            _ => self.drift_step(i, rng),
        }
    }

    /// Dead reckoning on velocity aiding: heading and scale errors, noise, what the aiding
    /// can't see.
    fn drift_step(&mut self, i: &Input, rng: &mut Gauss) -> (f64, f64) {
        let dt = i.dt;
        let (ve, vn) = i.vel;
        self.heading_err += rng.sample(0.0, HEADING_WALK * dt.sqrt());
        let (h, s) = (self.heading_err, self.scale_err);
        // velocity the INS integrates, minus the true one: rotated by h, scaled by 1 + s
        let mut de = (ve * h.cos() + vn * h.sin()) * (1.0 + s) - ve;
        let mut dn = (vn * h.cos() - ve * h.sin()) * (1.0 + s) - vn;
        match self.aid {
            Aid::Dvl => {
                if let Some(w) = i.water.filter(|w| w.in_area()) {
                    let above_seabed = w.water_depth_m.map_or(0.0, |d| d - i.depth_m);
                    if above_seabed > DVL_RANGE_M {
                        de -= w.current_east();
                        dn -= w.current_north();
                    }
                }
            }
            _ => {
                de -= self.drift.0;
                dn -= self.drift.1;
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

    fn input(now: f64, signal: bool, vel: (f64, f64), truth: (f64, f64, f64)) -> Input<'static> {
        Input {
            dt: 0.1,
            now,
            signal,
            spoof: None,
            vel,
            moved: (vel.1 * 0.1, vel.0 * 0.1),
            depth_m: 40.0,
            water: None,
            truth,
            airborne: false,
        }
    }

    #[test]
    fn error_grows_submerged_and_resets_after_surfacing() {
        let mut rng = Gauss::new(3);
        let mut nav = Nav::new(&mut rng, (0.0, 0.0, 0.0), Aid::Dvl, true);
        // force a visible bias
        nav.heading_err = 0.003;
        nav.scale_err = 0.002;
        let mut out = Vec::new();
        // one hour at 2 m/s, submerged: 7.2 km travelled
        for i in 0..36_000 {
            let now = f64::from(i) * 0.1;
            nav.step(
                &input(now, false, (2.0, 0.0), (0.0, 0.0, -40.0)),
                &mut rng,
                &mut out,
            );
            if now < POS_RETRY_VEL_S - 0.5 {
                assert_eq!(nav.ekf, Ekf::Coasting, "flags hold for posRetryTime");
            }
        }
        assert!(!nav.gps && !nav.fix);
        assert_eq!(nav.ekf, Ekf::DeadReckoning);
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
                &input(now, true, (0.0, 0.0), (1.0, 2.0, 0.0)),
                &mut rng,
                &mut out,
            );
            if nav.gps && snapped == (0.0, 0.0) {
                snapped = d;
            }
            now += 0.1;
        }
        assert!(nav.gps && nav.fix);
        assert_eq!(nav.error_m(), 0.0);
        assert!(
            (snapped.0.hypot(snapped.1) - e).abs() < 1.0,
            "snap undoes the error"
        );
        assert_eq!(nav.last_fix, (1.0, 2.0, 0.0));
        // a DVL keeps the EKF aiding: it never stopped, so it says nothing on the way back
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn unseen_current_beyond_dvl_range() {
        let mut rng = Gauss::new(5);
        let mut nav = Nav::new(&mut rng, (0.0, 0.0, 0.0), Aid::Dvl, false);
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
            let mut inp = input(f64::from(i) * 0.1, false, (0.5, 0.0), (0.0, 0.0, -40.0));
            inp.water = Some(&deep);
            nav.step(&inp, &mut rng, &mut out);
        }
        // 10 minutes of a 0.5 m/s current it can't see: ~300 m behind (estimate west of truth)
        assert!((nav.err_e + 300.0).abs() < 10.0, "{}", nav.err_e);
    }

    /// A copter or rover without velocity aiding: absolute flags for 7 s, then no position,
    /// then constant position mode at 15 s with the vehicle drifting away from its frozen
    /// estimate; the fix back resets everything.
    #[test]
    fn no_velocity_aiding_loses_position_then_stops_aiding() {
        let mut rng = Gauss::new(9);
        let mut nav = Nav::new(&mut rng, (0.0, 0.0, 0.0), Aid::None, true);
        let mut out = Vec::new();
        let mut seen = Vec::new();
        let mut frozen = 0.0;
        for i in 0..400 {
            let now = f64::from(i) * 0.1;
            let mut inp = input(now, false, (1.0, 0.0), (0.0, 0.0, 30.0));
            inp.airborne = true;
            let shift = nav.step(&inp, &mut rng, &mut out);
            if seen.last() != Some(&nav.ekf) {
                seen.push(nav.ekf);
            }
            if nav.ekf == Ekf::ConstPos {
                // what physics moved is undone: the estimate stays where it was
                frozen += inp.moved.1 + shift.1;
            }
        }
        assert_eq!(seen, vec![Ekf::Coasting, Ekf::NoPosition, Ekf::ConstPos]);
        assert!(frozen.abs() < 1e-9);
        assert!(
            nav.error_m() > 20.0,
            "moved 25 s at 1 m/s from a frozen estimate"
        );
        assert!(
            out.iter()
                .any(|o| matches!(o, Out::Text(_, s) if s == "EKF3 IMU0 stopped aiding"))
        );
        out.clear();
        for i in 0..120 {
            let now = 40.0 + f64::from(i) * 0.1;
            nav.step(
                &input(now, true, (0.0, 0.0), (0.0, 0.0, 30.0)),
                &mut rng,
                &mut out,
            );
        }
        assert_eq!(nav.ekf, Ekf::Gps);
        assert_eq!(nav.error_m(), 0.0);
        assert!(
            out.iter()
                .any(|o| matches!(o, Out::Text(_, s) if s == "EKF3 IMU0 is using GPS"))
        );
    }

    #[test]
    fn plane_dead_reckons_on_airspeed_and_drifts_with_unseen_wind() {
        let mut rng = Gauss::new(11);
        let mut nav = Nav::new(&mut rng, (0.0, 0.0, 0.0), Aid::Airspeed, true);
        let mut out = Vec::new();
        for i in 0..6000 {
            let now = f64::from(i) * 0.1;
            nav.step(
                &input(now, false, (40.0, 0.0), (0.0, 0.0, 1000.0)),
                &mut rng,
                &mut out,
            );
        }
        assert_eq!(nav.ekf, Ekf::DeadReckoning);
        assert!(nav.ekf.rel() && !nav.ekf.abs());
        let e = nav.error_m();
        // ten minutes, 24 km: wind error ~1 m/s plus 0.2 % scale and heading
        assert!(e > 20.0 && e < 3000.0, "{e:.0} m");
    }

    #[test]
    fn spoofing_walks_the_estimate_off_and_flags_stay_good() {
        let mut rng = Gauss::new(13);
        let mut nav = Nav::new(&mut rng, (37.8, -122.5, 0.0), Aid::None, true);
        let spoof = Spoof {
            walk_mps: 2.0,
            bearing_deg: 90.0,
            max_offset_m: 500.0,
        };
        let mut out = Vec::new();
        let truth = (37.8, -122.5, 0.0);
        for i in 0..1000 {
            let mut inp = input(f64::from(i) * 0.1, true, (0.0, 0.0), truth);
            inp.spoof = Some(spoof);
            nav.step(&inp, &mut rng, &mut out);
            assert_eq!(nav.ekf, Ekf::Gps, "the EKF sees nothing wrong");
        }
        // 100 s at 2 m/s east: 200 m, all of it in the estimate
        assert!((nav.err_e - 200.0).abs() < 0.5, "{}", nav.err_e);
        assert!(nav.err_n.abs() < 1e-6);
        assert!(nav.spoofed && nav.tracked && nav.fix);
        assert!(
            nav.last_fix.1 > truth.1,
            "the receiver reports east of the truth"
        );
        // the cap
        for i in 1000..3000 {
            let mut inp = input(f64::from(i) * 0.1, true, (0.0, 0.0), truth);
            inp.spoof = Some(spoof);
            nav.step(&inp, &mut rng, &mut out);
        }
        assert!((nav.spoof_m() - 500.0).abs() < 1e-6);
        // released: the true fix is 500 m away, past the gate: rejected for 10 s, then reset
        let mut states = Vec::new();
        for i in 3000..3200 {
            nav.step(
                &input(f64::from(i) * 0.1, true, (0.0, 0.0), truth),
                &mut rng,
                &mut out,
            );
            if states.last() != Some(&nav.ekf) {
                states.push(nav.ekf);
            }
        }
        assert_eq!(states, vec![Ekf::Glitch, Ekf::Gps]);
        assert_eq!(nav.error_m(), 0.0);
        assert!(!nav.spoofed);
    }
}
