//! Antenna tracker (ArduPilot AntennaTracker): a pan/tilt mount that points at another vehicle.
//!
//! Semantics follow AntennaTracker (tracking.cpp, mode.cpp, servos.cpp, GCS_MAVLink_Tracker.cpp):
//!
//! - It listens to the target's GLOBAL_POSITION_INT (the bridge relays it) and keeps the last
//!   position and velocity; for up to 5 s after the last one it dead-reckons the target
//!   (north/east by velocity, and "up" by the message's `vz`, which is positive down: an
//!   ArduPilot quirk kept as is), then the target is lost.
//! - Bearing and distance come from the tracker's own GPS position; elevation from the altitude
//!   difference per ALT_SOURCE (default 0, barometer: only SCALED_PRESSURE feeds that, so with
//!   a target that sends no pressure the elevation stays 0; 1: both GPS altitudes; 2: the
//!   target's altitude above its own home).
//! - AUTO points the mount at the target (YAW_TRIM, PITCH_TRIM, PITCH_MIN/MAX, DISTANCE_MIN
//!   apply). A target out of reach of the yaw range is approached from whichever side ends
//!   closest, with a latch against flip-flopping (`get_ef_yaw_direction`): the long way round
//!   when the short way hits a limit. SCAN sweeps; GUIDED follows SET_ATTITUDE_TARGET;
//!   SERVOTEST drives the servos from DO_SET_SERVO; MANUAL holds the RC trim (no RC in
//!   a stand-in, so the mount goes to its centre); STOP leaves the servos alone.
//! - Modes that drive the servos arm them, STOP and INITIALISING disarm; a disarmed mount
//!   does not move. COMMAND_COMPONENT_ARM_DISARM takes 1 and 0 only, with no text.
//! - SYSID_TARGET 0 means "first vehicle that sends a heartbeat": the heartbeat of a vehicle
//!   that is not a tracker, GCS, companion computer or gimbal sets it and asks that vehicle
//!   for position and pressure streams (REQUEST_DATA_STREAM). Once set, only heartbeats from
//!   that sysid are considered. Position is accepted from any sender, as the firmware does.
//! - HEARTBEAT system_status is ACTIVE always (CALIBRATING while initialising). ATTITUDE is the
//!   mount's yaw (the tracker carries the AHRS) and pitch. NAV_CONTROLLER_OUTPUT carries the
//!   bearing, elevation and distance to the target and the altitude difference.
//!
//! The mount itself is the simple part: yaw slews at the SITL mount's 9 deg/s inside
//! +-YAW_RANGE/2 of the heading it started on, pitch at 1 deg/s inside PITCH_MIN..PITCH_MAX
//! (libraries/SITL/SIM_Tracker.cpp), closing a fixed fraction of the error per second below
//! that (the servo loop's lag, `MOUNT_GAIN_PER_S`). The PID loops themselves are not modelled.
//! A real tracker whose servos are faster would change only these constants.

use mavlink::dialects::ardupilotmega::{
    AttitudeTargetTypemask, MavCmd, MavMessage, MavModeFlag, MavParamType, MavResult, MavType,
    NAV_CONTROLLER_OUTPUT_DATA,
};
// ArduPilot still asks its target for streams this way
#[allow(deprecated)]
use mavlink::dialects::ardupilotmega::REQUEST_DATA_STREAM_DATA;

use super::{Behavior, Command, Ctx, Observe, Tick, modes};
use crate::config::Spec;
use crate::geo::{M_PER_DEG, bearing_deg, distance_m, wrap180};
use crate::kinematics::State;
use crate::params::Params;

/// SITL mount slew rates, deg/s (SIM_Tracker: yawrate 9, pitchrate 1).
pub const MOUNT_YAW_DPS: f64 = 9.0;
pub const MOUNT_PITCH_DPS: f64 = 1.0;
/// Servo loop response, 1/s: the mount closes this fraction of its error per second, up to the
/// slew rate. Measured on SITL, which trails a target moving at 3 deg/s by 4.6 deg (a lag of
/// about 1.5 s: the yaw PID's output runs ahead of the mount, mode.cpp get_ef_yaw_direction).
const MOUNT_GAIN_PER_S: f64 = 0.65;
/// config.h TRACKING_TIMEOUT_SEC: no position for this long and the target is lost.
const TRACKING_TIMEOUT_S: f64 = 5.0;
/// Margin before the latched turning direction changes, degrees (mode.cpp margin_cd 500).
const DIRECTION_MARGIN_DEG: f64 = 5.0;

const MAV_DATA_STREAM_RAW_SENSORS: u8 = 1;
const MAV_DATA_STREAM_POSITION: u8 = 6;

#[derive(Debug, Clone, Copy, Default)]
struct Target {
    lat: f64,
    lon: f64,
    /// metres above MSL
    alt: f64,
    /// metres above the target's own home (GLOBAL_POSITION_INT relative_alt)
    rel_alt: f64,
    /// north, east, "z" (the message's vz, down positive), m/s
    vel: (f64, f64, f64),
    /// when it arrived, behavior clock
    at: f64,
}

#[derive(Debug, Default)]
pub struct Tracker {
    /// behavior clock, s (sum of dt)
    t: f64,
    /// heading the mount was set up facing: the centre of its yaw range
    zero_yaw: Option<f64>,
    last_mode: &'static str,
    /// last position of the target and the dead-reckoned estimate of it now
    target: Option<Target>,
    estimate: Option<Target>,
    /// a heartbeat from the target was seen (ArduPilot's `target_set`)
    target_set: bool,
    // nav_status
    bearing: f64,
    pitch: f64,
    distance: f64,
    alt_diff_gps: f64,
    alt_diff_baro: f64,
    manual_yaw: bool,
    manual_pitch: bool,
    scan_reverse_yaw: bool,
    scan_reverse_pitch: bool,
    /// latched turning direction: +1 clockwise, -1 counter-clockwise, 0 none yet
    yaw_direction: i8,
    /// the mount's pitch, degrees
    mount_pitch: f64,
    /// GUIDED: demanded yaw and pitch, degrees
    guided: Option<(f64, f64)>,
    /// SERVOTEST: demanded pwm per servo (None: not set, hold)
    servo: [Option<f64>; 2],
}

fn is_target_type(t: MavType) -> bool {
    !matches!(
        t,
        MavType::MAV_TYPE_ANTENNA_TRACKER
            | MavType::MAV_TYPE_GCS
            | MavType::MAV_TYPE_ONBOARD_CONTROLLER
            | MavType::MAV_TYPE_GIMBAL
    )
}

/// AP_Baro::get_altitude_difference: height of `pressure` above `base`, metres, at 25 C.
fn baro_altitude_difference(base_pa: f64, pressure_pa: f64) -> f64 {
    let scaling = pressure_pa / base_pa;
    153.8462 * (273.15 + 25.0) * (1.0 - (0.190259 * scaling.ln()).exp())
}

/// ISA pressure at an altitude, Pa (the SITL barometer's atmosphere).
fn isa_pressure(alt_m: f64) -> f64 {
    101_325.0 * (1.0 - alt_m / 44_330.77).max(0.01).powf(5.255_88)
}

/// SRV_Channels angle scaling: pwm in [min, max], trim in the middle, +-`limit` degrees at the
/// ends.
fn servo_angle(pwm: f64, min: f64, trim: f64, max: f64, limit: f64) -> f64 {
    let pwm = pwm.clamp(min, max);
    let span = if pwm >= trim { max - trim } else { trim - min };
    if span <= 0.0 {
        return 0.0;
    }
    (pwm - trim) / span * limit
}

impl Tracker {
    fn param(p: &Params, name: &str, default: f64) -> f64 {
        p.f(name, default)
    }

    fn yaw_range(p: &Params) -> f64 {
        Self::param(p, "YAW_RANGE", 360.0).clamp(0.0, 360.0)
    }

    fn pitch_limits(p: &Params) -> (f64, f64) {
        (
            Self::param(p, "PITCH_MIN", -90.0),
            Self::param(p, "PITCH_MAX", 90.0),
        )
    }

    /// Where the mount points relative to its centre, degrees.
    fn rel_yaw(&self, st: &State) -> f64 {
        wrap180(st.heading - self.zero_yaw.unwrap_or(st.heading))
    }

    /// Slew the mount toward a yaw relative to its centre (within the range) and a pitch.
    fn slew(&mut self, t: &mut Tick, yaw_rel: Option<f64>, pitch: Option<f64>) {
        let (lo, hi) = Self::pitch_limits(t.params);
        if let Some(target) = yaw_rel {
            let half = Self::yaw_range(t.params) / 2.0;
            let rel = self.rel_yaw(t.st);
            let step = MOUNT_YAW_DPS * t.dt;
            let want = (target.clamp(-half, half) - rel) * MOUNT_GAIN_PER_S * t.dt;
            let next = rel + want.clamp(-step, step);
            t.st.heading = (self.zero_yaw.unwrap_or(0.0) + next).rem_euclid(360.0);
        }
        if let Some(target) = pitch {
            let step = MOUNT_PITCH_DPS * t.dt;
            let want = (target.clamp(lo, hi) - self.mount_pitch) * MOUNT_GAIN_PER_S * t.dt;
            self.mount_pitch += want.clamp(-step, step);
        }
    }

    /// `update_vehicle_pos_estimate`, then `update_bearing_and_distance`.
    fn update_estimate(&mut self, t: &Tick) {
        self.estimate = self.target.and_then(|v| {
            let dt = self.t - v.at;
            if dt >= TRACKING_TIMEOUT_S {
                return None;
            }
            let mut e = v;
            e.lat += v.vel.0 * dt / M_PER_DEG;
            e.lon += v.vel.1 * dt / (M_PER_DEG * v.lat.to_radians().cos());
            // Location::offset_up_m(vel.z * dt) with vel.z = the message's vz (down positive)
            e.alt += v.vel.2 * dt;
            Some(e)
        });
        let Some(e) = self.estimate else { return };
        let st = &*t.st;
        let scan = t.st.mode == "SCAN";
        if !scan && !self.manual_yaw {
            self.bearing = bearing_deg(st.lat, st.lon, e.lat, e.lon);
        }
        self.distance = distance_m(st.lat, st.lon, e.lat, e.lon);
        self.alt_diff_gps = if Self::param(t.params, "ALT_SOURCE", 0.0) as i64 == 2 {
            e.rel_alt
        } else {
            e.alt - st.alt
        };
        if !scan && !self.manual_pitch {
            let diff = if Self::param(t.params, "ALT_SOURCE", 0.0) as i64 == 0 {
                self.alt_diff_baro
            } else {
                self.alt_diff_gps
            };
            self.pitch = diff.atan2(self.distance).to_degrees();
        }
    }

    /// `Mode::get_ef_yaw_direction`: true when the mount should go the long way round.
    fn long_way(&mut self, st: &State, p: &Params) -> bool {
        let half = Self::yaw_range(p) / 2.0;
        let pos = self.rel_yaw(st);
        // distances from here to the two limits, in the direction of travel
        let limit_lower = -half - pos;
        let limit_upper = half - pos;
        let current = wrap180(st.heading);
        let target = wrap180(self.bearing + Self::param(p, "YAW_TRIM", 0.0));
        let error = wrap180(target - current);
        let (upper, lower) = if error >= 0.0 {
            (error, error - 360.0)
        } else {
            (360.0 + error, error)
        };
        let overshoot_cw = upper - limit_upper;
        let overshoot_ccw = limit_lower - lower;
        let score_cw = if overshoot_cw > 0.0 {
            360.0 + overshoot_cw
        } else {
            upper
        };
        let score_ccw = if overshoot_ccw > 0.0 {
            360.0 + overshoot_ccw
        } else {
            -lower
        };
        let d = &mut self.yaw_direction;
        if *d == 0 {
            *d = if score_cw <= score_ccw { 1 } else { -1 };
        } else if *d > 0 && score_ccw < score_cw - DIRECTION_MARGIN_DEG {
            *d = -1;
        } else if *d < 0 && score_cw < score_ccw - DIRECTION_MARGIN_DEG {
            *d = 1;
        }
        if error == 0.0 {
            return false;
        }
        (*d > 0) != (error > 0.0)
    }

    /// `Mode::update_auto`.
    fn update_auto(&mut self, t: &mut Tick) {
        let yaw = wrap180(self.bearing + Self::param(t.params, "YAW_TRIM", 0.0));
        let (lo, hi) = Self::pitch_limits(t.params);
        let pitch = (self.pitch + Self::param(t.params, "PITCH_TRIM", 0.0)).clamp(lo, hi);
        let reversed = self.long_way(t.st, t.params);
        let mut err = wrap180(yaw - wrap180(t.st.heading));
        if reversed {
            err += if err > 0.0 { -360.0 } else { 360.0 };
        }
        let min = Self::param(t.params, "DISTANCE_MIN", 5.0);
        if min <= 0.0 || self.distance >= min || self.estimate.is_none() {
            let rel = self.rel_yaw(t.st);
            self.slew(t, Some(rel + err), Some(pitch));
        }
    }

    /// `Mode::update_scan`: sweep the demanded bearing and pitch, then track the demand.
    fn update_scan(&mut self, t: &mut Tick) {
        if !self.manual_yaw {
            let delta = Self::param(t.params, "SCAN_SPEED_YAW", 2.0) * t.dt;
            self.bearing += if self.scan_reverse_yaw { -delta } else { delta };
            if self.bearing < 0.0 && self.scan_reverse_yaw {
                self.scan_reverse_yaw = false;
            }
            if self.bearing > 360.0 && !self.scan_reverse_yaw {
                self.scan_reverse_yaw = true;
            }
            self.bearing = self.bearing.clamp(0.0, 360.0);
        }
        if !self.manual_pitch {
            let (lo, hi) = Self::pitch_limits(t.params);
            let delta = Self::param(t.params, "SCAN_SPEED_PIT", 5.0) * t.dt;
            if self.scan_reverse_pitch {
                self.pitch -= delta;
                if self.pitch < lo {
                    self.scan_reverse_pitch = false;
                }
            } else {
                self.pitch += delta;
                if self.pitch > hi {
                    self.scan_reverse_pitch = true;
                }
            }
            self.pitch = self.pitch.clamp(lo, hi);
        }
        self.update_auto(t);
    }
}

impl Tracker {
    /// DO_SET_SERVO: servo_num is a uint8 there, only 1 (yaw) and 2 (pitch) exist.
    fn set_servo(&mut self, params: &Params, servo: f32, pwm: f32) -> MavResult {
        let n = if (1.0..3.0).contains(&servo) {
            servo as usize
        } else {
            0
        };
        if n == 0 || !pwm.is_finite() {
            return MavResult::MAV_RESULT_FAILED;
        }
        let min = params.f(&format!("SERVO{n}_MIN"), 1000.0);
        let max = params.f(&format!("SERVO{n}_MAX"), 2000.0);
        self.servo[n - 1] = Some(f64::from(pwm).clamp(min, max));
        MavResult::MAV_RESULT_ACCEPTED
    }
}

impl Behavior for Tracker {
    fn mav_type(&self) -> MavType {
        MavType::MAV_TYPE_ANTENNA_TRACKER
    }

    fn modes(&self) -> &'static [(u32, &'static str)] {
        modes::TRACKER
    }

    fn hold_mode(&self) -> &'static str {
        "STOP"
    }

    fn spawn(&self, _spec: &Spec, st: &mut State) {
        // INITIAL_MODE defaults to AUTO, which arms the servos
        st.mode = "AUTO";
        st.armed = true;
    }

    /// Everything but INITIALISING can be selected (`Tracker::set_mode`).
    fn accepts_mode(&self, name: &str) -> bool {
        name != "INITIALISING"
    }

    fn accepts_missions(&self) -> bool {
        false
    }

    /// GCS_MAVLINK_Tracker::base_mode
    fn base_mode_flags(&self, mode: &str) -> MavModeFlag {
        match mode {
            "MANUAL" => MavModeFlag::MAV_MODE_FLAG_MANUAL_INPUT_ENABLED,
            "SCAN" | "SERVOTEST" | "AUTO" | "GUIDED" => {
                MavModeFlag::MAV_MODE_FLAG_GUIDED_ENABLED
                    | MavModeFlag::MAV_MODE_FLAG_STABILIZE_ENABLED
            }
            _ => MavModeFlag::empty(),
        }
    }

    fn mode_arms(&self, mode: &str) -> Option<bool> {
        Some(!matches!(mode, "STOP" | "INITIALISING"))
    }

    /// COMMAND_COMPONENT_ARM_DISARM (1 and 0 only, no text) and DO_SET_SERVO, which enters
    /// SERVOTEST first even when the servo is refused.
    fn command(&mut self, c: &mut Ctx, cmd: &Command) -> Option<MavResult> {
        match cmd.command {
            MavCmd::MAV_CMD_COMPONENT_ARM_DISARM => Some(if cmd.p[0] == 1.0 {
                c.st.armed = true;
                MavResult::MAV_RESULT_ACCEPTED
            } else if cmd.p[0] == 0.0 {
                c.st.armed = false;
                MavResult::MAV_RESULT_ACCEPTED
            } else {
                MavResult::MAV_RESULT_UNSUPPORTED
            }),
            MavCmd::MAV_CMD_DO_SET_SERVO => {
                if c.st.mode != "SERVOTEST" {
                    c.st.mode = "SERVOTEST";
                    c.st.hold_point = (c.st.lat, c.st.lon);
                    c.st.armed = true;
                }
                Some(self.set_servo(c.params, cmd.p[0], cmd.p[1]))
            }
            _ => None,
        }
    }

    fn attitude(&self, _st: &State) -> Option<(f64, f64)> {
        Some((0.0, self.mount_pitch.to_radians()))
    }

    fn nav_controller_output(&self, _st: &State) -> Option<NAV_CONTROLLER_OUTPUT_DATA> {
        let bearing = self.bearing as i16;
        Some(NAV_CONTROLLER_OUTPUT_DATA {
            nav_roll: 0.0,
            nav_pitch: self.pitch as f32,
            alt_error: self.alt_diff_gps as f32,
            aspd_error: 0.0,
            xtrack_error: 0.0,
            nav_bearing: bearing,
            target_bearing: bearing,
            wp_dist: self.distance.min(65_535.0) as u16,
        })
    }

    fn system_status(
        &self,
        _st: &State,
        _failsafe: bool,
    ) -> mavlink::dialects::ardupilotmega::MavState {
        // vehicle_system_status: ACTIVE, armed or not (CALIBRATING while initialising, which
        // the stand-in never is)
        mavlink::dialects::ardupilotmega::MavState::MAV_STATE_ACTIVE
    }

    fn extra_params(&self, _spec: &Spec) -> Vec<(&'static str, MavParamType, f32)> {
        use MavParamType::{
            MAV_PARAM_TYPE_INT8 as I8, MAV_PARAM_TYPE_INT16 as I16, MAV_PARAM_TYPE_REAL32 as F32,
        };
        vec![
            // Parameters.cpp defaults, with sim/params/tracker.parm on top (the SITL mount)
            ("YAW_RANGE", I16, 340.0),
            ("PITCH_MIN", I16, -45.0),
            ("PITCH_MAX", I16, 45.0),
            ("YAW_TRIM", F32, 0.0),
            ("PITCH_TRIM", F32, 0.0),
            ("YAW_SLEW_TIME", F32, 2.0),
            ("DISTANCE_MIN", F32, 5.0),
            ("ALT_SOURCE", I8, 0.0),
            ("MAV_UPDATE_RATE", I8, 1.0),
            ("SCAN_SPEED_YAW", F32, 2.0),
            ("SCAN_SPEED_PIT", F32, 5.0),
            ("INITIAL_MODE", I8, 10.0),
            ("SAFE_DISARM_PWM", I8, 0.0),
            ("AUTO_OPTIONS", I8, 0.0),
            ("STARTUP_DELAY", F32, 0.0),
            ("SERVO1_MIN", I16, 1000.0),
            ("SERVO1_MAX", I16, 2000.0),
            ("SERVO1_TRIM", I16, 1500.0),
            ("SERVO2_MIN", I16, 1000.0),
            ("SERVO2_MAX", I16, 2000.0),
            ("SERVO2_TRIM", I16, 1500.0),
        ]
    }

    #[allow(deprecated)]
    fn observe(&mut self, o: &mut Observe) {
        let sysid_target = o.params.f("SYSID_TARGET", 0.0) as i64;
        let for_me = |target: u8| target == 0 || target == o.sysid;
        match o.msg {
            // packetReceived: only the target's (or anyone's, with SYSID_TARGET 0) heartbeat
            MavMessage::HEARTBEAT(hb)
                if (sysid_target == 0 || sysid_target == i64::from(o.from.system_id))
                    && !self.target_set
                    && is_target_type(hb.mavtype) =>
            {
                if sysid_target == 0 {
                    o.params.update("SYSID_TARGET", f32::from(o.from.system_id));
                }
                let rate = o.params.f("MAV_UPDATE_RATE", 1.0) as u8;
                for stream in [MAV_DATA_STREAM_POSITION, MAV_DATA_STREAM_RAW_SENSORS] {
                    o.send
                        .push(MavMessage::REQUEST_DATA_STREAM(REQUEST_DATA_STREAM_DATA {
                            req_message_rate: u16::from(rate),
                            target_system: o.from.system_id,
                            target_component: o.from.component_id,
                            req_stream_id: stream,
                            start_stop: 1,
                        }));
                }
                self.target_set = true;
            }
            // handle_message: no sysid filter on position and pressure
            MavMessage::GLOBAL_POSITION_INT(m) => {
                // reject (0;0)
                if m.lat == 0 && m.lon == 0 {
                    return;
                }
                self.target = Some(Target {
                    lat: f64::from(m.lat) / 1e7,
                    lon: f64::from(m.lon) / 1e7,
                    alt: f64::from(m.alt) / 1000.0,
                    rel_alt: f64::from(m.relative_alt) / 1000.0,
                    vel: (
                        f64::from(m.vx) * 0.01,
                        f64::from(m.vy) * 0.01,
                        f64::from(m.vz) * 0.01,
                    ),
                    at: self.t,
                });
            }
            MavMessage::SCALED_PRESSURE(m) => {
                let local = isa_pressure(o.st.alt);
                let diff = baro_altitude_difference(local, f64::from(m.press_abs) * 100.0);
                if diff.is_finite() {
                    self.alt_diff_baro = diff;
                }
            }
            MavMessage::MANUAL_CONTROL(m) if for_me(m.target) => {
                self.bearing = f64::from(m.x);
                self.pitch = f64::from(m.y);
                self.distance = 0.0;
                self.manual_yaw = m.x != 0x7FFF;
                self.manual_pitch = m.y != 0x7FFF;
            }
            MavMessage::SET_ATTITUDE_TARGET(m) if for_me(m.target_system) => {
                use AttitudeTargetTypemask as M;
                // handle_set_attitude_target: only in GUIDED, and only the shapes it accepts
                let ignored = |f: M| m.type_mask.contains(f);
                if o.st.mode != "GUIDED"
                    || m.body_roll_rate != 0.0
                    || !ignored(M::ATTITUDE_TARGET_TYPEMASK_BODY_ROLL_RATE_IGNORE)
                    || !ignored(M::ATTITUDE_TARGET_TYPEMASK_THROTTLE_IGNORE)
                    || ignored(M::ATTITUDE_TARGET_TYPEMASK_ATTITUDE_IGNORE)
                    || (ignored(M::ATTITUDE_TARGET_TYPEMASK_BODY_PITCH_RATE_IGNORE)
                        && ignored(M::ATTITUDE_TARGET_TYPEMASK_BODY_YAW_RATE_IGNORE))
                {
                    return;
                }
                let [w, x, y, z] = m.q.map(f64::from);
                let yaw = (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z));
                let pitch = (2.0 * (w * y - z * x)).clamp(-1.0, 1.0).asin();
                self.guided = Some((yaw.to_degrees(), pitch.to_degrees()));
            }
            _ => {}
        }
    }

    fn step(&mut self, t: &mut Tick) {
        self.t += t.dt;
        self.zero_yaw.get_or_insert(t.st.heading);
        if t.st.mode != self.last_mode {
            // Tracker::set_mode: the demanded bearing starts at where the mount points
            self.last_mode = t.st.mode;
            self.bearing = t.st.heading.rem_euclid(360.0);
        }
        self.update_estimate(t);
        if !t.st.armed {
            return; // disarmed servos: the mount is where it was
        }
        match t.st.mode {
            "AUTO" => {
                if self.estimate.is_some() {
                    self.update_auto(t);
                } else if self.target_set || (t.params.f("AUTO_OPTIONS", 0.0) as i64) & 1 != 0 {
                    self.update_scan(t);
                }
            }
            "SCAN" => self.update_scan(t),
            "GUIDED" => {
                if let Some((yaw, pitch)) = self.guided {
                    let err = wrap180(yaw - wrap180(t.st.heading));
                    let rel = self.rel_yaw(t.st);
                    self.slew(t, Some(rel + err), Some(pitch));
                }
            }
            "SERVOTEST" => {
                let p = t.params;
                let half = Self::yaw_range(p) / 2.0;
                let (lo, hi) = Self::pitch_limits(p);
                let angle = |n: usize, limit: f64| {
                    self.servo[n - 1].map(|pwm| {
                        servo_angle(
                            pwm,
                            p.f(&format!("SERVO{n}_MIN"), 1000.0),
                            p.f(&format!("SERVO{n}_TRIM"), 1500.0),
                            p.f(&format!("SERVO{n}_MAX"), 2000.0),
                            limit,
                        )
                    })
                };
                let (yaw, pitch) = (angle(1, half), angle(2, (hi - lo) / 2.0));
                self.slew(t, yaw, pitch);
            }
            "MANUAL" => {
                // the RC sticks at trim (a stand-in has no radio): the mount goes to its centre
                self.slew(t, Some(0.0), Some(0.0));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior::Firmware;
    use crate::geo::Gauss;
    use crate::kinematics::Out;
    use crate::mission::Mission;
    use mavlink::MavHeader;
    use mavlink::dialects::ardupilotmega::{GLOBAL_POSITION_INT_DATA, HEARTBEAT_DATA};

    const HOME: (f64, f64, f64) = (37.8105, -122.4770, 10.0);

    fn spec() -> Spec {
        Spec {
            name: "tracker".into(),
            behavior: "tracker".into(),
            cruise_mps: 0.0,
            max_mps: 0.0,
            climb_mps: None,
            turn_dps: None,
            loiter_radius_m: None,
            endurance_h: None,
            link: None,
        }
    }

    struct Rig {
        b: Tracker,
        st: State,
        params: Params,
        mission: Mission,
        rng: Gauss,
        sent: Vec<MavMessage>,
    }

    impl Rig {
        fn new() -> Rig {
            let b = Tracker::default();
            let mut params = Params::seed(Firmware::Tracker, &spec(), 7);
            params.extend(b.extra_params(&spec()));
            let mut st = State {
                lat: HOME.0,
                lon: HOME.1,
                alt: HOME.2,
                heading: 0.0,
                speed: 0.0,
                vz: 0.0,
                ve: 0.0,
                vn: 0.0,
                home: HOME,
                armed: false,
                mode: "MANUAL",
                hold_point: (HOME.0, HOME.1),
                aground: false,
                demand: 0.0,
            };
            b.spawn(&spec(), &mut st);
            Rig {
                b,
                st,
                params,
                mission: Mission::default(),
                rng: Gauss::new(1),
                sent: Vec::new(),
            }
        }

        fn step(&mut self, seconds: f64) {
            for _ in 0..(seconds * 10.0).round() as usize {
                let mut out: Vec<Out> = Vec::new();
                let spec = spec();
                self.b.step(&mut Tick {
                    spec: &spec,
                    params: &self.params,
                    st: &mut self.st,
                    mission: &mut self.mission,
                    water: None,
                    rng: &mut self.rng,
                    out: &mut out,
                    dt: 0.1,
                });
            }
        }

        fn hear(&mut self, sysid: u8, msg: MavMessage) {
            let from = MavHeader {
                system_id: sysid,
                component_id: 1,
                sequence: 0,
            };
            let mut o = Observe {
                from: &from,
                sysid: 7,
                msg: &msg,
                params: &mut self.params,
                st: &self.st,
                send: Vec::new(),
            };
            self.b.observe(&mut o);
            self.sent.extend(o.send);
        }

        /// A target at `bearing` (true), `elev` degrees up, `dist` m away, from `sysid`.
        fn target(&mut self, sysid: u8, bearing: f64, elev: f64, dist: f64) {
            let b = bearing.to_radians();
            let lat = HOME.0 + dist * b.cos() / M_PER_DEG;
            let lon = HOME.1 + dist * b.sin() / (M_PER_DEG * HOME.0.to_radians().cos());
            let alt = HOME.2 + dist * elev.to_radians().tan();
            self.hear(
                sysid,
                MavMessage::GLOBAL_POSITION_INT(GLOBAL_POSITION_INT_DATA {
                    lat: (lat * 1e7) as i32,
                    lon: (lon * 1e7) as i32,
                    alt: (alt * 1000.0) as i32,
                    relative_alt: ((alt - HOME.2) * 1000.0) as i32,
                    ..Default::default()
                }),
            );
        }

        /// Relay a still target twice a second for `seconds`.
        fn track(&mut self, bearing: f64, elev: f64, seconds: f64) {
            for _ in 0..(seconds * 2.0) as usize {
                self.target(42, bearing, elev, 1200.0);
                self.step(0.5);
            }
        }

        fn arm(&mut self, p1: f32, p2: f32) -> Option<MavResult> {
            let spec = spec();
            let mut out = Vec::new();
            self.b.command(
                &mut Ctx {
                    spec: &spec,
                    params: &self.params,
                    st: &mut self.st,
                    mission: &mut self.mission,
                    out: &mut out,
                },
                &Command {
                    command: MavCmd::MAV_CMD_COMPONENT_ARM_DISARM,
                    p: [p1, p2, 0.0, 0.0],
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
            )
        }

        fn yaw_error(&self, bearing: f64) -> f64 {
            wrap180(self.st.heading - bearing)
        }

        fn pitch(&self) -> f64 {
            self.b.attitude(&self.st).expect("attitude").1.to_degrees()
        }
    }

    #[test]
    fn boots_armed_in_auto_and_points_around_the_compass() {
        let mut r = Rig::new();
        assert_eq!((r.st.mode, r.st.armed), ("AUTO", true));
        r.params.update("ALT_SOURCE", 1.0);
        for (bearing, elev) in [(90.0, 20.0), (170.0, 5.0), (300.0, 30.0), (10.0, -5.0)] {
            r.track(bearing, elev, 90.0);
            assert!(
                r.yaw_error(bearing).abs() < 0.5,
                "yaw {} for bearing {bearing}",
                r.st.heading
            );
            assert!((r.pitch() - elev).abs() < 0.5, "pitch for {elev}");
        }
    }

    #[test]
    fn slews_at_the_mount_rate() {
        let mut r = Rig::new();
        r.target(42, 90.0, 0.0, 1200.0);
        r.step(1.0);
        assert!((r.st.heading - 9.0).abs() < 0.2, "{}", r.st.heading);
    }

    #[test]
    fn goes_the_long_way_when_the_short_way_hits_a_limit() {
        let mut r = Rig::new();
        r.track(170.0, 0.0, 40.0);
        assert!(r.yaw_error(170.0).abs() < 0.5);
        // 225 is 55 deg further round, past the +170 limit: back through north, 305 deg
        let mut min_heading: f64 = 360.0;
        for _ in 0..100 {
            r.track(225.0, 0.0, 0.5);
            min_heading = min_heading.min(r.st.heading);
        }
        assert!(r.yaw_error(225.0).abs() < 0.5, "{}", r.st.heading);
        assert!(min_heading < 10.0, "it passed through north: {min_heading}");
    }

    #[test]
    fn a_target_in_the_gap_is_approached_as_far_as_the_range_allows() {
        let mut r = Rig::new();
        r.track(183.0, 0.0, 60.0);
        // the range is +-170 about north: 183 = -177 is 7 deg past the stop
        assert!(
            (r.yaw_error(183.0).abs() - 7.0).abs() < 0.5,
            "{}",
            r.st.heading
        );
        r.track(177.0, 0.0, 80.0);
        assert!(
            (r.yaw_error(177.0).abs() - 7.0).abs() < 0.5,
            "{}",
            r.st.heading
        );
    }

    #[test]
    fn elevation_follows_alt_source() {
        let mut r = Rig::new();
        // default ALT_SOURCE 0 is the barometer: nothing sends it pressure, so elevation 0
        r.track(0.0, 20.0, 30.0);
        assert!(r.pitch().abs() < 1e-6);
        r.params.update("ALT_SOURCE", 2.0);
        r.track(0.0, 20.0, 30.0);
        assert!((r.pitch() - 20.0).abs() < 0.5);
        // beyond PITCH_MAX it stops at the limit
        r.track(0.0, 60.0, 60.0);
        assert!((r.pitch() - 45.0).abs() < 0.5);
    }

    #[test]
    fn disarmed_or_stopped_it_does_not_move() {
        let mut r = Rig::new();
        r.st.armed = false;
        r.track(90.0, 0.0, 20.0);
        assert_eq!(r.st.heading, 0.0);
        assert_eq!(r.b.mode_arms("STOP"), Some(false));
        assert_eq!(r.b.mode_arms("AUTO"), Some(true));
        assert_eq!(r.b.mode_arms("INITIALISING"), Some(false));
    }

    #[test]
    fn lost_target_holds_until_a_target_was_set_then_scans() {
        let mut r = Rig::new();
        r.track(90.0, 0.0, 30.0);
        r.step(10.0); // past the 5 s timeout; no target ever announced itself: hold
        let held = r.st.heading;
        r.step(10.0);
        assert!((r.st.heading - held).abs() < 0.01);
        let hb = MavMessage::HEARTBEAT(HEARTBEAT_DATA {
            mavtype: MavType::MAV_TYPE_FIXED_WING,
            ..Default::default()
        });
        r.hear(42, hb);
        r.step(10.0);
        assert!((r.st.heading - held).abs() > 1.0, "scanning");
    }

    #[test]
    fn a_heartbeat_locks_sysid_target_once_and_asks_for_streams() {
        let mut r = Rig::new();
        let hb = |t| {
            MavMessage::HEARTBEAT(HEARTBEAT_DATA {
                mavtype: t,
                ..Default::default()
            })
        };
        r.hear(255, hb(MavType::MAV_TYPE_GCS));
        r.hear(23, hb(MavType::MAV_TYPE_ANTENNA_TRACKER));
        r.hear(24, hb(MavType::MAV_TYPE_GIMBAL));
        assert_eq!(r.params.get("SYSID_TARGET"), Some(0.0));
        assert!(r.sent.is_empty());
        r.hear(55, hb(MavType::MAV_TYPE_QUADROTOR));
        assert_eq!(r.params.get("SYSID_TARGET"), Some(55.0));
        assert_eq!(r.sent.len(), 2, "position and pressure streams");
        r.hear(56, hb(MavType::MAV_TYPE_QUADROTOR));
        assert_eq!(r.params.get("SYSID_TARGET"), Some(55.0));
        assert_eq!(r.sent.len(), 2);
    }

    #[test]
    fn position_is_taken_from_any_sender_and_zero_zero_is_rejected() {
        let mut r = Rig::new();
        r.params.update("SYSID_TARGET", 42.0);
        for _ in 0..60 {
            // not the target, still followed (the firmware does)
            r.target(99, 90.0, 0.0, 1200.0);
            r.step(0.5);
        }
        assert!(r.yaw_error(90.0).abs() < 0.5);
        r.hear(
            42,
            MavMessage::GLOBAL_POSITION_INT(GLOBAL_POSITION_INT_DATA::default()),
        );
        r.step(1.0);
        assert!(r.b.estimate.is_some_and(|e| e.lat != 0.0));
    }

    #[test]
    fn servotest_and_manual_drive_the_mount_from_pwm() {
        let mut r = Rig::new();
        r.st.mode = "SERVOTEST";
        assert_eq!(
            r.b.set_servo(&r.params, 1.0, 1750.0),
            MavResult::MAV_RESULT_ACCEPTED
        );
        assert_eq!(
            r.b.set_servo(&r.params, 3.0, 1750.0),
            MavResult::MAV_RESULT_FAILED
        );
        r.step(20.0);
        assert!((r.st.heading - 85.0).abs() < 0.5, "{}", r.st.heading);
        r.st.mode = "MANUAL";
        r.step(20.0);
        assert!(wrap180(r.st.heading).abs() < 0.5);
    }

    #[test]
    fn arm_command_takes_only_zero_and_one() {
        let mut r = Rig::new();
        assert_eq!(r.arm(0.0, 0.0), Some(MavResult::MAV_RESULT_ACCEPTED));
        assert!(!r.st.armed);
        assert_eq!(r.arm(2.0, 0.0), Some(MavResult::MAV_RESULT_UNSUPPORTED));
        assert_eq!(r.arm(1.0, 0.0), Some(MavResult::MAV_RESULT_ACCEPTED));
        assert!(r.st.armed);
    }

    #[test]
    fn nav_controller_output_reports_the_demand() {
        let mut r = Rig::new();
        r.params.update("ALT_SOURCE", 1.0);
        r.track(90.0, 15.0, 5.0);
        let n = r.b.nav_controller_output(&r.st).expect("always sent");
        // int16 truncation of 89.99, as ArduPilot sends it
        assert_eq!((n.nav_bearing, n.target_bearing), (89, 89));
        assert!((f64::from(n.nav_pitch) - 15.0).abs() < 0.5);
        assert!((f64::from(n.wp_dist) - 1200.0).abs() < 3.0, "{}", n.wp_dist);
    }
}
