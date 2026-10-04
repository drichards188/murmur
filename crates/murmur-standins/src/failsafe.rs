//! Failsafes as each firmware runs them: GCS heartbeat loss, the battery and the EKF.
//!
//! GCS: only heartbeats from MAV_GCS_SYSID count, and nothing triggers before the first one
//! (GCS_MAVLINK::handle_heartbeat, sysid_mygcs_seen). Then, per firmware:
//! - Plane (FS_GCS_ENABL, FS_LONG_TIMEOUT, FS_LONG_ACTN; events.cpp failsafe_long_on_event,
//!   system.cpp check_long_failsafe): armed or not, except in the landing flight stage; in
//!   AUTO/GUIDED continue (FS_LONG_ACTN 0) or RTL (1); in other modes RTL; "GCS Failsafe On:
//!   switched to <mode>", later "GCS Failsafe Cleared".
//! - Rover (FS_GCS_ENABLE, FS_GCS_TIMEOUT, then FS_TIMEOUT, FS_ACTION; failsafe.cpp
//!   failsafe_trigger): not in RTL or HOLD, armed or not. Announced as "Radio Failsafe": the
//!   text names whichever check calls failsafe_trigger first once FS_TIMEOUT has passed, and
//!   the radio check (50 Hz, on while FS_THR_ENABLE is set) beats the GCS check (10 Hz) to it
//!   (seen in the motorboat SITL); "GCS Failsafe" with the radio failsafe off. Cleared by the
//!   GCS check alone: "GCS Failsafe Cleared".
//! - Sub (FS_GCS_ENABLE action, FS_GCS_TIMEOUT; failsafe.cpp failsafe_gcs_check): warns
//!   "MYGCS: heartbeat lost" every 30 s; acts once, if armed (disarm, depth hold, surface).
//!
//! Battery (AP_BattMonitor::check_failsafes): "Battery 1 is low|critical <V>V used <mAh> mAh",
//! then the firmware's handle_battery_failsafe for BATT_FS_LOW_ACT / BATT_FS_CRT_ACT.
//!
//! EKF (navigation lost, e.g. GNSS jammed: nav.rs), each firmware's ekf_check.cpp at 10 Hz:
//! 10 failing checks in a row (EKF_CHECK_ITERATIONS_MAX, 1 s) trigger, as many passing ones
//! clear it. A check fails when the EKF has neither absolute nor relative horizontal
//! position (the variance tests are not modelled), only while FS_EKF_THRESH > 0.
//! - Copter (ArduCopter/ekf_check.cpp; runs disarmed too, acts only armed): "EKF variance:
//!   position lost"; in a mode that requires a position, FS_EKF_ACTION 1 or 3 -> LAND, 2 ->
//!   ALT_HOLD, "EKF Failsafe: changed to <mode> Mode"; else "EKF Failsafe". Cleared: "EKF
//!   Failsafe Cleared" (it stays in LAND). gpsglitch_check: "GPS Glitch or Compass error" /
//!   "Glitch cleared" while the EKF rejects GPS.
//! - Rover (Rover/ekf_check.cpp; armed only, and const_pos_mode fails it too): "EKF
//!   variance"; in a mode that requires a position, FS_EKF_ACTION 1 -> HOLD with "EKF
//!   failsafe" (2: the text only); "EKF failsafe cleared".
//! - Blimp (Blimp/ekf_check.cpp; armed only): "EKF variance"; LAND in a mode that requires
//!   GPS (or always with FS_EKF_ACTION 3); no text on clearing.
//! - Plane: ArduPlane/ekf_check.cpp only acts for a quadplane in a VTOL position mode
//!   (QLAND / QHOVER); a fixed-wing flies on, dead reckoning on airspeed. The stand-in's
//!   VTOL is treated as a fixed-wing here (gap: no Q-mode EKF failsafe). Sub and
//!   AntennaTracker: none (a sub keeps relative position on its DVL).

use std::sync::atomic::{AtomicU64, Ordering};

use mavlink::dialects::ardupilotmega::MavSeverity;

use crate::battery::Level;
use crate::behavior::Firmware;
use crate::vehicle::Vehicle;

#[derive(Debug, Clone, Default)]
pub struct Failsafes {
    /// sim time of the last heartbeat from MAV_GCS_SYSID; None until the first
    pub gcs_last: Option<f64>,
    /// GCS failsafe triggered
    pub gcs: bool,
    /// Rover: since when the GCS has been missing
    rover_since: Option<f64>,
    /// Sub: last "heartbeat lost" warning
    sub_warned: Option<f64>,
    /// EKF check: consecutive failures (capped at EKF_CHECK_ITERATIONS), bad variance
    /// declared, ever passed, the failsafe on, last warning, GPS glitch reported
    ekf_count: u8,
    ekf_bad: bool,
    ekf_passed: bool,
    pub ekf: bool,
    ekf_warned: Option<f64>,
    /// Copter: AP_Notify's failsafe_ekf (set when it triggered armed; gates the clear text)
    ekf_notify: bool,
    glitch: bool,
}

/// The GCS heartbeat as the vehicles receive it, all stand-ins together, for the engine's
/// stats line (and reset by it): the longest gap between two heartbeats from the GCS on the
/// failsafe clock, in ms, and how many GCS failsafes triggered.
pub static GCS_GAP_MAX_MS: AtomicU64 = AtomicU64::new(0);
pub static GCS_FAILSAFES: AtomicU64 = AtomicU64::new(0);

/// ArduPilot's EKF_CHECK_ITERATIONS_MAX (1 s at 10 Hz) and EKF_CHECK_WARNING_TIME.
pub const EKF_CHECK_ITERATIONS: u8 = 10;
pub const EKF_CHECK_WARNING_S: f64 = 30.0;

/// Modes whose Mode::requires_position() (Copter, Rover) or requires_GPS() (Blimp) is true.
fn requires_position(fw: Firmware, mode: &str) -> bool {
    match fw {
        Firmware::Copter => matches!(
            mode,
            "AUTO"
                | "GUIDED"
                | "LOITER"
                | "RTL"
                | "CIRCLE"
                | "DRIFT"
                | "POSHOLD"
                | "BRAKE"
                | "THROW"
                | "AVOID_ADSB"
                | "SMART_RTL"
                | "FOLLOW"
                | "ZIGZAG"
                | "AUTO_RTL"
        ),
        Firmware::Rover => !matches!(
            mode,
            "MANUAL" | "ACRO" | "STEERING" | "HOLD" | "INITIALISING"
        ),
        Firmware::Blimp => matches!(mode, "VELOCITY" | "LOITER" | "RTL" | "AUTO"),
        _ => false,
    }
}

const WARN: MavSeverity = MavSeverity::MAV_SEVERITY_WARNING;

impl Vehicle {
    /// Any failsafe triggered, as each firmware's vehicle_system_status counts them (Rover's
    /// counts radio and GCS, not the battery).
    pub fn failsafe_active(&self) -> bool {
        let battery = self.battery.level > Level::Ok;
        match self.firmware {
            Firmware::Rover => self.fs.gcs || self.fs.ekf,
            Firmware::Copter | Firmware::Blimp => self.fs.gcs || battery || self.fs.ekf,
            _ => self.fs.gcs || battery,
        }
    }

    pub(crate) fn check_failsafes(&mut self, now: f64) {
        self.gcs_failsafe(now);
        self.battery_failsafe();
        self.ekf_failsafe(now);
    }

    /// One 10 Hz EKF check (see the module doc).
    fn ekf_failsafe(&mut self, now: f64) {
        let fw = self.firmware;
        if !matches!(fw, Firmware::Copter | Firmware::Rover | Firmware::Blimp) {
            return;
        }
        if fw == Firmware::Copter {
            let glitch = self.nav.ekf == crate::nav::Ekf::Glitch;
            if glitch != self.fs.glitch {
                self.fs.glitch = glitch;
                let t = if glitch {
                    "GPS Glitch or Compass error"
                } else {
                    "Glitch cleared"
                };
                self.text(MavSeverity::MAV_SEVERITY_CRITICAL, t);
            }
        }
        let disabled = self.params.f("FS_EKF_THRESH", 0.8) <= 0.0;
        if disabled || (fw != Firmware::Copter && !self.st.armed) {
            self.fs.ekf_count = 0;
            self.fs.ekf_bad = false;
            self.ekf_failsafe_off();
            return;
        }
        let passed = self.nav.ekf.rel();
        self.fs.ekf_passed |= passed;
        if !self.fs.ekf_passed {
            return;
        }
        if passed {
            if self.fs.ekf_count > 0 {
                self.fs.ekf_count -= 1;
                if self.fs.ekf_bad && self.fs.ekf_count == 0 {
                    self.fs.ekf_bad = false;
                    self.ekf_failsafe_off();
                }
            }
            return;
        }
        if self.fs.ekf_bad {
            return;
        }
        self.fs.ekf_count += 1;
        if self.fs.ekf_count < EKF_CHECK_ITERATIONS {
            return;
        }
        self.fs.ekf_bad = true;
        if self
            .fs
            .ekf_warned
            .is_none_or(|t| now - t > EKF_CHECK_WARNING_S)
        {
            self.fs.ekf_warned = Some(now);
            let t = if fw == Firmware::Copter {
                "EKF variance: position lost"
            } else {
                "EKF variance"
            };
            self.text(MavSeverity::MAV_SEVERITY_CRITICAL, t);
        }
        self.ekf_failsafe_on();
    }

    fn ekf_failsafe_on(&mut self) {
        const CRIT: MavSeverity = MavSeverity::MAV_SEVERITY_CRITICAL;
        let action = self.params.f("FS_EKF_ACTION", 1.0) as i64;
        let needs = requires_position(self.firmware, self.st.mode);
        match self.firmware {
            Firmware::Copter => {
                self.fs.ekf = true;
                if !self.st.armed {
                    return;
                }
                self.fs.ekf_notify = true;
                if action == 0 || (!needs && action != 3) {
                    self.text(CRIT, "EKF Failsafe");
                    return;
                }
                if action != 2 || !self.try_mode("ALT_HOLD") {
                    self.try_mode("LAND");
                }
                let t = format!("EKF Failsafe: changed to {} Mode", self.st.mode);
                self.text(CRIT, &t);
            }
            Firmware::Rover => {
                if self.fs.ekf {
                    return;
                }
                self.fs.ekf = true;
                if !needs || action == 0 {
                    return;
                }
                if action != 2 {
                    self.try_mode("HOLD");
                }
                self.text(CRIT, "EKF failsafe");
            }
            Firmware::Blimp => {
                if self.fs.ekf {
                    return;
                }
                self.fs.ekf = true;
                if needs || action == 3 {
                    self.try_mode("LAND");
                }
            }
            _ => {}
        }
    }

    fn ekf_failsafe_off(&mut self) {
        if !self.fs.ekf {
            return;
        }
        self.fs.ekf = false;
        match self.firmware {
            Firmware::Copter if self.fs.ekf_notify => {
                self.fs.ekf_notify = false;
                self.text(MavSeverity::MAV_SEVERITY_CRITICAL, "EKF Failsafe Cleared");
            }
            Firmware::Rover => {
                self.text(MavSeverity::MAV_SEVERITY_CRITICAL, "EKF failsafe cleared");
            }
            _ => {}
        }
    }

    /// A heartbeat from MAV_GCS_SYSID (GCS_MAVLINK::handle_heartbeat, sysid_mygcs_seen).
    pub fn gcs_heartbeat(&mut self) {
        let now = self.now();
        if let Some(prev) = self.fs.gcs_last {
            GCS_GAP_MAX_MS.fetch_max(((now - prev) * 1000.0) as u64, Ordering::Relaxed);
        }
        self.fs.gcs_last = Some(now);
    }

    fn gcs_lost(&self, now: f64, timeout: f64) -> bool {
        self.fs.gcs_last.is_some_and(|t| now - t > timeout)
    }

    /// Try modes in order (ArduPilot's FALLTHROUGH chains); true if one took.
    fn fall_through(&mut self, modes: &[&'static str]) -> bool {
        modes.iter().any(|m| self.try_mode(m))
    }

    fn disarm(&mut self) {
        if self.st.armed {
            self.st.armed = false;
            let t = self.arm_text(false);
            self.text(MavSeverity::MAV_SEVERITY_INFO, t);
        }
    }

    fn gcs_failsafe(&mut self, now: f64) {
        let p = |v: &Vehicle, name: &str, d: f64| v.params.f(name, d);
        match self.firmware {
            Firmware::Plane => {
                let enabled = p(self, "FS_GCS_ENABL", 0.0) != 0.0;
                let lost = enabled && self.gcs_lost(now, p(self, "FS_LONG_TIMEOUT", 5.0));
                // flight_stage LAND: a landing in progress is never interrupted
                let landing =
                    self.st.mode == "AUTO" && self.mission.landing() && !self.mission.complete;
                if lost && !self.fs.gcs && !landing {
                    self.fs.gcs = true;
                    GCS_FAILSAFES.fetch_add(1, Ordering::Relaxed);
                    let action = p(self, "FS_LONG_ACTN", 0.0) as i64;
                    match self.st.mode {
                        "AUTO" | "GUIDED" | "AVOID_ADSB" => match action {
                            1 | 5 => _ = self.try_mode("RTL"),
                            2 => _ = self.try_mode("FBWA"),
                            _ => {} // 0: carry on with the mission
                        },
                        "RTL" | "QLAND" | "QRTL" | "LOITERALTQLAND" | "AUTOLAND" => {}
                        _ => match action {
                            2 => _ = self.try_mode("FBWA"),
                            4 => _ = self.try_mode("AUTO"),
                            _ => _ = self.try_mode("RTL"),
                        },
                    }
                    let mode = self.st.mode;
                    self.text(WARN, &format!("GCS Failsafe On: switched to {mode}"));
                } else if !lost && self.fs.gcs {
                    self.fs.gcs = false;
                    self.text(WARN, "GCS Failsafe Cleared");
                }
            }
            Firmware::Rover => {
                let enable = p(self, "FS_GCS_ENABLE", 0.0) as i64;
                let lost = enable != 0 && self.gcs_lost(now, p(self, "FS_GCS_TIMEOUT", 5.0));
                if !lost {
                    self.fs.rover_since = None;
                    if self.fs.gcs {
                        self.fs.gcs = false;
                        self.text(MavSeverity::MAV_SEVERITY_INFO, "GCS Failsafe Cleared");
                    }
                    return;
                }
                let since = *self.fs.rover_since.get_or_insert(now);
                let in_hold_ok = (p(self, "FS_OPTIONS", 0.0) as i64) & 1 != 0;
                let mode = self.st.mode;
                if self.fs.gcs
                    || now - since <= p(self, "FS_TIMEOUT", 1.5)
                    || mode == "RTL"
                    || (mode == "HOLD" && !in_hold_ok)
                {
                    return;
                }
                self.fs.gcs = true;
                GCS_FAILSAFES.fetch_add(1, Ordering::Relaxed);
                let announced = if p(self, "FS_THR_ENABLE", 1.0) != 0.0 {
                    "Radio Failsafe"
                } else {
                    "GCS Failsafe"
                };
                self.text(WARN, announced);
                if mode == "AUTO" && enable == 2 {
                    self.text(WARN, "Failsafe - Continuing Auto Mode");
                } else {
                    let action = p(self, "FS_ACTION", 2.0) as i64;
                    self.rover_action(action);
                }
            }
            Firmware::Sub => {
                let action = p(self, "FS_GCS_ENABLE", 0.0) as i64;
                if action == 0 || self.fs.gcs_last.is_none() {
                    return;
                }
                if !self.gcs_lost(now, p(self, "FS_GCS_TIMEOUT", 5.0)) {
                    if self.fs.gcs {
                        self.text(WARN, "GCS Failsafe Cleared");
                    }
                    self.fs.gcs = false;
                    return;
                }
                if self.fs.sub_warned.is_none_or(|t| now - t > 30.0) {
                    self.fs.sub_warned = Some(now);
                    self.text(WARN, "MYGCS: heartbeat lost");
                }
                if self.fs.gcs || !self.st.armed {
                    return;
                }
                self.fs.gcs = true;
                GCS_FAILSAFES.fetch_add(1, Ordering::Relaxed);
                match action {
                    2 => self.disarm(),
                    3 => {
                        if !self.try_mode("ALT_HOLD") {
                            self.disarm();
                        }
                    }
                    4 => _ = self.try_mode("SURFACE"),
                    _ => {} // 1: warn only
                }
            }
            Firmware::Copter | Firmware::Blimp | Firmware::Tracker => {
                let enable = p(self, "FS_GCS_ENABLE", 0.0) as i64;
                let lost = enable != 0 && self.gcs_lost(now, p(self, "FS_GCS_TIMEOUT", 5.0));
                if lost && !self.fs.gcs && self.st.armed {
                    self.fs.gcs = true;
                    GCS_FAILSAFES.fetch_add(1, Ordering::Relaxed);
                    self.text(WARN, "GCS Failsafe");
                    self.fall_through(&["RTL", "LAND"]);
                } else if !lost && self.fs.gcs {
                    self.fs.gcs = false;
                    self.text(WARN, "GCS Failsafe Cleared");
                }
            }
        }
    }

    /// Rover's FailsafeAction (SmartRTL needs a recorded path the stand-in doesn't keep).
    fn rover_action(&mut self, action: i64) {
        match action {
            1 | 3 => _ = self.fall_through(&["RTL", "HOLD"]),
            2 | 4 => _ = self.try_mode("HOLD"),
            5 => self.disarm(),
            6 => _ = self.fall_through(&["LOITER", "HOLD"]),
            _ => {}
        }
    }

    fn battery_failsafe(&mut self) {
        let cap = self.capacity_mah();
        let p = |name: &str| self.params.f(name, 0.0);
        let level = self.battery.assess(
            cap,
            p("BATT_LOW_MAH"),
            p("BATT_CRT_MAH"),
            p("BATT_LOW_VOLT"),
            p("BATT_CRT_VOLT"),
        );
        if level <= self.battery.level {
            return;
        }
        self.battery.level = level;
        let (kind, act) = if level == Level::Critical {
            ("critical", p("BATT_FS_CRT_ACT") as i64)
        } else {
            ("low", p("BATT_FS_LOW_ACT") as i64)
        };
        let text = format!(
            "Battery 1 is {kind} {:.2}V used {:.0} mAh",
            self.battery.voltage(cap),
            self.battery.consumed_mah
        );
        self.text(WARN, &text);
        if !self.st.armed {
            return;
        }
        match self.firmware {
            Firmware::Plane => match act {
                // 2 Land: without a DO_LAND_START sequence ArduPlane falls back to RTL
                1 | 2 | 7 => _ = self.try_mode("RTL"),
                3 => self.disarm(),
                4 | 6 => {
                    let q = self.params.get("Q_ENABLE") == Some(1.0);
                    let mode = if q { "QLAND" } else { "RTL" };
                    _ = self.try_mode(mode);
                }
                _ => {}
            },
            Firmware::Rover => self.rover_action(act),
            Firmware::Sub => match act {
                2 => self.disarm(),
                3 => _ = self.try_mode("SURFACE"),
                _ => {}
            },
            Firmware::Copter => match act {
                1 | 7 => _ = self.try_mode("LAND"),
                2 | 3 | 6 => _ = self.fall_through(&["RTL", "LAND"]),
                4 => _ = self.try_mode("LAND"),
                5 => self.disarm(),
                _ => {}
            },
            Firmware::Blimp => {
                if act == 1 {
                    _ = self.try_mode("LAND");
                }
            }
            Firmware::Tracker => {}
        }
    }
}
