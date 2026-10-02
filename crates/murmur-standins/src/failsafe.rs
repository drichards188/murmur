//! Failsafes as each firmware runs them: GCS heartbeat loss and the battery.
//!
//! GCS: only heartbeats from MAV_GCS_SYSID count, and nothing triggers before the first one
//! (GCS_MAVLINK::handle_heartbeat, sysid_mygcs_seen). Then, per firmware:
//! - Plane (FS_GCS_ENABL, FS_LONG_TIMEOUT, FS_LONG_ACTN; events.cpp failsafe_long_on_event):
//!   in AUTO/GUIDED continue (FS_LONG_ACTN 0) or RTL (1); in other modes RTL; "GCS Failsafe On:
//!   switched to <mode>", later "GCS Failsafe Cleared".
//! - Rover (FS_GCS_ENABLE, FS_GCS_TIMEOUT, then FS_TIMEOUT, FS_ACTION; failsafe.cpp
//!   failsafe_trigger): not in RTL or HOLD; "GCS Failsafe", "GCS Failsafe Cleared".
//! - Sub (FS_GCS_ENABLE action, FS_GCS_TIMEOUT; failsafe.cpp failsafe_gcs_check): warns
//!   "MYGCS: heartbeat lost" every 30 s; acts once, if armed (disarm, depth hold, surface).
//!
//! Battery (AP_BattMonitor::check_failsafes): "Battery 1 is low|critical <V>V used <mAh> mAh",
//! then the firmware's handle_battery_failsafe for BATT_FS_LOW_ACT / BATT_FS_CRT_ACT.

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
}

const WARN: MavSeverity = MavSeverity::MAV_SEVERITY_WARNING;

impl Vehicle {
    /// Any failsafe triggered, as each firmware's vehicle_system_status counts them (Rover's
    /// counts radio and GCS, not the battery).
    pub fn failsafe_active(&self) -> bool {
        let battery = self.battery.level > Level::Ok;
        match self.firmware {
            Firmware::Rover => self.fs.gcs,
            _ => self.fs.gcs || battery,
        }
    }

    pub(crate) fn check_failsafes(&mut self, now: f64) {
        self.gcs_failsafe(now);
        self.battery_failsafe();
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
                if lost && !self.fs.gcs && self.st.armed {
                    self.fs.gcs = true;
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
                self.text(WARN, "GCS Failsafe");
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
