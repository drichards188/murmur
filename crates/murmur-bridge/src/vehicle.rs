//! Vehicle state decoded from its MAVLink link, and the cell that shares it.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

pub use murmur_decide::world::NavSource;
use serde::Serialize;
use serde_json::{Map, Value};
use tokio::sync::broadcast;

use crate::clock::{self, round};
use crate::link::{LinkCounters, LinkSnapshot, LinkStats, Transmit};
use crate::mav::{self, Kind, MavMessage, apm};

/// Fleet-unique vehicle id (the launcher's manifest), what the API and console key on.
/// The MAVLink sysid is one byte and unique only per link.
pub type Uid = u64;

const STATUSTEXT_KEEP: usize = 8;
/// Vehicle events (mode changes, failsafes, STATUSTEXT) waiting for the audit log; the
/// decision loop drains them about once a second. Bounded so a silent consumer can't grow it.
const EVENTS_KEEP: usize = 256;
pub const TRAIL_KEEP: usize = 120;
/// Firmware that doesn't report EKF status gets this long after a 3D fix before it counts as
/// ready to navigate.
const NO_EKF_GRACE_S: f64 = 15.0;

/// What the launcher's fleet manifest says a node is: callsign, platform, role, domain, link...
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Identity(pub Map<String, Value>);

impl Identity {
    const KEYS: [&str; 13] = [
        "callsign",
        "platform",
        "name",
        "short",
        "role",
        "domain",
        "link",
        // the SATCOM constellation class (starlink, iridium, wgs, ku-geo): the telemetry plan
        "space_class",
        "sidc",
        "sim",
        "armed",
        "cruise_alt_m",
        "min_turn_radius_m",
    ];

    pub fn from_manifest(entry: &Map<String, Value>) -> Identity {
        Identity(
            Self::KEYS
                .iter()
                .map(|k| {
                    (
                        (*k).to_string(),
                        entry.get(*k).cloned().unwrap_or(Value::Null),
                    )
                })
                .collect(),
        )
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(Value::as_str)
    }

    pub fn num(&self, key: &str) -> Option<f64> {
        self.0.get(key).and_then(Value::as_f64)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusText {
    pub t: f64,
    pub severity: u8,
    pub text: String,
}

/// The vehicle as the console sees it. Rounded so tiny jitter doesn't count as a change
/// (see the change-only push).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VehicleSnapshot {
    pub uid: Uid,
    pub sysid: u8,
    /// The push layer takes this out before serializing (it goes to each client once,
    /// `push.rs`), so a snapshot without one has no `identity` key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<Arc<Identity>>,
    pub kind: Option<Kind>,
    pub mode: Option<String>,
    pub armed: bool,
    pub position: Option<[f64; 2]>,
    pub alt_rel: Option<f64>,
    pub alt_msl: Option<f64>,
    pub roll: Option<f64>,
    pub pitch: Option<f64>,
    pub heading: Option<f64>,
    pub yaw: Option<f64>,
    pub tracking: Option<Uid>,
    pub nav_bearing: Option<i16>,
    pub nav_ready: bool,
    /// gnss | dead_reckoning (a submerged AUV: ready, but its position drifts) | none
    pub nav_source: NavSource,
    pub groundspeed: Option<f64>,
    pub airspeed: Option<f64>,
    pub battery_pct: Option<i8>,
    pub gps_fix: u8,
    pub satellites: u8,
    pub mission_seq: Option<u16>,
    pub home: Option<[f64; 2]>,
    pub link: LinkSnapshot,
    pub statustext: Vec<StatusText>,
}

/// Decoded state of one vehicle. Plain data: only its cell's lock guards it.
#[derive(Default)]
pub struct Vehicle {
    pub identity: Option<Arc<Identity>>,
    pub kind: Option<Kind>,
    pub mav_type: Option<apm::MavType>,
    pub mode: Option<String>,
    pub custom_mode: Option<u32>,
    pub armed: bool,
    /// (lat, lon)
    pub pos: Option<(f64, f64)>,
    pub alt_rel: Option<f64>,
    pub alt_msl: Option<f64>,
    pub heading: Option<f64>,
    pub yaw: Option<f64>,
    pub roll: Option<f64>,
    pub pitch: Option<f64>,
    /// Velocity over ground, [east, north] m/s, with the position it came with:
    /// GLOBAL_POSITION_INT's vy and vx; HIGH_LATENCY2 has none, so there its heading and
    /// ground speed.
    pub vel_en: Option<[f64; 2]>,
    pub groundspeed: Option<f64>,
    pub airspeed: Option<f64>,
    pub battery_pct: Option<i8>,
    pub gps_fix: u8,
    pub satellites: u8,
    pub mission_seq: Option<u16>,
    /// (lat, lon)
    pub home: Option<(f64, f64)>,
    pub home_alt: Option<f64>,
    pub statustext: VecDeque<StatusText>,
    /// (lon, lat, metres above mean sea level), one point a second at most
    pub trail: VecDeque<[f64; 3]>,
    /// total points ever appended, so clients can ask for "new since n"
    pub trail_count: u64,
    last_trail: Option<f64>,
    /// links that get a copy of this vehicle's position (antenna trackers pointed at it)
    pub forward_to: BTreeMap<u16, Arc<dyn Transmit>>,
    /// for trackers: the uid being tracked
    pub tracking: Option<Uid>,
    /// planes only: None until Q_ENABLE is read
    pub is_vtol: Option<bool>,
    /// fixed wing only: WP_LOITER_RAD and RTL_RADIUS (m), None until read
    pub loiter_radius_m: Option<f64>,
    pub rtl_radius_m: Option<f64>,
    pub ekf_flags: Option<apm::EkfStatusFlags>,
    /// failure flags of the last HIGH_LATENCY2 (a vehicle on its low-rate link)
    pub hl_failure: Option<apm::HlFailureFlag>,
    pub nav_bearing: Option<i16>,
    pub target_dist: Option<u16>,
    /// Where the autopilot navigates to (NAV_CONTROLLER_OUTPUT's target bearing and
    /// distance from the position then): the loiter center in LOITER, home or a rally point
    /// in RTL. Stamped `seen.nav_target`.
    pub nav_target: Option<(f64, f64)>,
    /// When the mode last changed (wall clock, s): a navigation target reported before it
    /// belongs to the previous mode (the last AUTO waypoint, say).
    pub mode_since: Option<f64>,
    /// The GPS fix comes from a HIGH_LATENCY2 report (which carries no fix type: without
    /// its GPS failure flag it counts as a 3D fix), not from GPS_RAW_INT.
    pub gps_fix_hl2: bool,
    fix_since: Option<f64>,
    /// the telemetry plan went out at least once
    pub rates_sent: bool,
    /// When each fact the decision layer reads last arrived (wall clock, seconds): every
    /// fact in a decision's world snapshot carries its age.
    pub seen: FactTimes,
    /// Events for the audit log (`vehicle.event`), oldest first.
    pub events: VecDeque<VehicleEvent>,
    system_status: Option<apm::MavState>,
}

/// Arrival times (wall clock, seconds) of the messages behind each decision fact.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FactTimes {
    /// autopilot HEARTBEAT (kind, mode)
    pub heartbeat: Option<f64>,
    pub position: Option<f64>,
    pub home: Option<f64>,
    pub battery: Option<f64>,
    pub gps: Option<f64>,
    pub ekf: Option<f64>,
    /// HIGH_LATENCY2 (position, battery and navigation health in one low-rate report)
    pub hl2: Option<f64>,
    /// NAV_CONTROLLER_OUTPUT (the navigation target)
    pub nav_target: Option<f64>,
}

/// The latest report from a vehicle's navigation filter, with its arrival time.
#[derive(Clone, Copy)]
enum NavReport {
    Ekf(apm::EkfStatusFlags, Option<f64>),
    HighLatency(apm::HlFailureFlag, Option<f64>),
}

/// How old a navigation target may be to center a loiter circle, s (NAV_CONTROLLER_OUTPUT
/// comes every 2 s on a full link, every 5 s on a reduced one): the engine's own bound on a
/// circle's center, so the bridge reports no circle the engine would refuse for its age.
pub const NAV_TARGET_MAX_AGE_S: f64 =
    murmur_decide::engage::CIRCLE_CENTER_MAX_AGE_MS as f64 / 1000.0;

/// A circle a fixed wing flies on its own ([`Vehicle::loiter_circle`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoiterCircle {
    /// (lat, lon)
    pub center: (f64, f64),
    /// with the tolerance it is flown to
    pub radius_m: f64,
    /// circling in RTL (else LOITER)
    pub rtl: bool,
    /// when the center was reported (wall clock, s)
    pub center_t: f64,
}

/// Something the vehicle did or said that belongs in the audit log.
#[derive(Debug, Clone, PartialEq)]
pub struct VehicleEvent {
    /// "mode", "failsafe" or "statustext"
    pub kind: &'static str,
    pub text: String,
}

impl Vehicle {
    /// Apply one message. False when the message carries nothing usable (a GCS heartbeat, a
    /// zero position), in which case nobody waiting on the vehicle hears about it either.
    pub fn apply(&mut self, msg: &MavMessage, raw: &[u8], now: f64) -> bool {
        match msg {
            MavMessage::HEARTBEAT(m) => {
                if m.autopilot == apm::MavAutopilot::MAV_AUTOPILOT_INVALID {
                    return false;
                }
                self.mav_type = Some(m.mavtype);
                let mut kind = Kind::from_mav_type(m.mavtype);
                // ArduPlane reports FIXED_WING even as a QuadPlane; Q_ENABLE tells them apart
                if kind == Kind::Plane && self.is_vtol == Some(true) {
                    kind = Kind::Vtol;
                }
                self.kind = Some(kind);
                self.set_mode(m.mavtype, m.custom_mode);
                let critical = |s: Option<apm::MavState>| {
                    matches!(
                        s,
                        Some(
                            apm::MavState::MAV_STATE_CRITICAL | apm::MavState::MAV_STATE_EMERGENCY
                        )
                    )
                };
                let now_critical = critical(Some(m.system_status));
                if self.system_status.is_some() && now_critical != critical(self.system_status) {
                    self.push_event("failsafe", format!("system status {:?}", m.system_status));
                }
                self.system_status = Some(m.system_status);
                self.seen.heartbeat = Some(clock::wall());
                self.armed = m
                    .base_mode
                    .contains(apm::MavModeFlag::MAV_MODE_FLAG_SAFETY_ARMED);
            }
            MavMessage::GLOBAL_POSITION_INT(m) => {
                if m.lat == 0 && m.lon == 0 {
                    return false;
                }
                let (lat, lon) = (f64::from(m.lat) / 1e7, f64::from(m.lon) / 1e7);
                let (alt_rel, alt_msl) = (
                    f64::from(m.relative_alt) / 1000.0,
                    f64::from(m.alt) / 1000.0,
                );
                if self.home.is_none() {
                    self.home = Some((lat, lon));
                    self.home_alt = Some(alt_msl - alt_rel);
                    self.seen.home = Some(clock::wall());
                }
                self.set_position(lat, lon, alt_msl, Some(alt_rel), now);
                if m.hdg != u16::MAX {
                    self.heading = Some(f64::from(m.hdg) / 100.0);
                }
                // vx north, vy east, cm/s
                self.vel_en = Some([f64::from(m.vy) / 100.0, f64::from(m.vx) / 100.0]);
                for link in self.forward_to.values() {
                    link.send_raw(raw);
                }
            }
            MavMessage::HIGH_LATENCY2(m) => {
                // the low-rate report of a vehicle that can't stream (an AUV on its acoustic
                // modem): everything the console and the task layer follow, in one message
                let mav_type = self.mav_type.unwrap_or(m.mavtype);
                self.set_mode(mav_type, u32::from(m.custom_mode));
                let wall = clock::wall();
                if m.latitude != 0 || m.longitude != 0 {
                    let (lat, lon) = (f64::from(m.latitude) / 1e7, f64::from(m.longitude) / 1e7);
                    let alt_msl = f64::from(m.altitude);
                    let alt_rel = self.home_alt.map(|h| alt_msl - h);
                    self.set_position(lat, lon, alt_msl, alt_rel, now);
                }
                self.heading = Some(f64::from(m.heading) * 2.0);
                self.groundspeed = Some(f64::from(m.groundspeed) / 5.0);
                // no velocity components in HIGH_LATENCY2: its heading and ground speed
                let (h, gs) = (f64::from(m.heading) * 2.0, f64::from(m.groundspeed) / 5.0);
                let r = h.to_radians();
                self.vel_en = Some([gs * r.sin(), gs * r.cos()]);
                self.airspeed = Some(f64::from(m.airspeed) / 5.0);
                self.mission_seq = Some(m.wp_num);
                self.battery_pct = (m.battery >= 0).then_some(m.battery);
                self.seen.battery = self.battery_pct.map(|_| wall);
                // no fix type in HIGH_LATENCY2: its GPS failure flag says no fix, its absence
                // counts as a 3D fix (an armed platform on this link alone could otherwise
                // never show a fresh fix)
                if m.failure_flags
                    .contains(apm::HlFailureFlag::HL_FAILURE_FLAG_GPS)
                {
                    self.gps_fix = 0;
                    self.satellites = 0;
                } else {
                    self.gps_fix = 3;
                }
                self.gps_fix_hl2 = true;
                self.seen.gps = Some(wall);
                self.hl_failure = Some(m.failure_flags);
                self.seen.hl2 = Some(wall);
            }
            MavMessage::PARAM_VALUE(m) => match mav::chars_to_string(&m.param_id).as_str() {
                "Q_ENABLE" => {
                    let vtol = m.param_value > 0.0;
                    self.is_vtol = Some(vtol);
                    if self.kind == Some(Kind::Plane) && vtol {
                        self.kind = Some(Kind::Vtol);
                    }
                }
                "WP_LOITER_RAD" => self.loiter_radius_m = Some(f64::from(m.param_value)),
                "RTL_RADIUS" => self.rtl_radius_m = Some(f64::from(m.param_value)),
                _ => {}
            },
            MavMessage::NAV_CONTROLLER_OUTPUT(m) => {
                self.nav_bearing = Some(m.nav_bearing);
                self.target_dist = Some(m.wp_dist);
                self.nav_target = self.pos.map(|(lat, lon)| {
                    crate::geo::destination(lat, lon, f64::from(m.target_bearing), f64::from(m.wp_dist))
                });
                self.seen.nav_target = self.nav_target.map(|_| clock::wall());
            }
            MavMessage::ATTITUDE(m) => {
                self.yaw = Some(f64::from(m.yaw).to_degrees().rem_euclid(360.0));
                self.roll = Some(f64::from(m.roll).to_degrees());
                self.pitch = Some(f64::from(m.pitch).to_degrees());
            }
            MavMessage::HOME_POSITION(m) => {
                self.home = Some((f64::from(m.latitude) / 1e7, f64::from(m.longitude) / 1e7));
                self.home_alt = Some(f64::from(m.altitude) / 1000.0);
                self.seen.home = Some(clock::wall());
            }
            MavMessage::VFR_HUD(m) => {
                self.groundspeed = Some(f64::from(m.groundspeed));
                self.airspeed = Some(f64::from(m.airspeed));
            }
            MavMessage::SYS_STATUS(m) => {
                self.battery_pct = (m.battery_remaining >= 0).then_some(m.battery_remaining);
                self.seen.battery = self.battery_pct.map(|_| clock::wall());
            }
            MavMessage::GPS_RAW_INT(m) => {
                self.gps_fix = m.fix_type as u8;
                self.gps_fix_hl2 = false;
                self.satellites = m.satellites_visible;
                self.seen.gps = Some(clock::wall());
                if self.gps_fix >= 3 && self.fix_since.is_none() {
                    self.fix_since = Some(now);
                }
            }
            MavMessage::EKF_STATUS_REPORT(m) => {
                self.ekf_flags = Some(m.flags);
                self.seen.ekf = Some(clock::wall());
            }
            MavMessage::MISSION_CURRENT(m) => self.mission_seq = Some(m.seq),
            MavMessage::STATUSTEXT(m) => {
                if self.statustext.len() == STATUSTEXT_KEEP {
                    self.statustext.pop_front();
                }
                let text = mav::chars_to_string(&m.text);
                let kind = if text.to_ascii_lowercase().contains("failsafe") {
                    "failsafe"
                } else {
                    "statustext"
                };
                self.push_event(kind, format!("[severity {}] {text}", m.severity as u8));
                self.statustext.push_back(StatusText {
                    t: clock::wall(),
                    severity: m.severity as u8,
                    text,
                });
            }
            _ => {}
        }
        true
    }

    /// Which report of the vehicle's own navigation filter is the latest: HIGH_LATENCY2
    /// (failure flags) or EKF_STATUS_REPORT. None when it sends neither (or is a tracker,
    /// which doesn't navigate: a GPS fix is enough to know where it stands).
    fn nav_report(&self) -> Option<NavReport> {
        if self.kind == Some(Kind::Tracker) {
            return None;
        }
        let (hl2, ekf) = (self.seen.hl2, self.seen.ekf);
        match (self.hl_failure, self.ekf_flags) {
            // Option order: a report that arrived beats one that has no time
            (Some(f), Some(_)) if hl2 >= ekf => Some(NavReport::HighLatency(f, hl2)),
            (Some(f), None) => Some(NavReport::HighLatency(f, hl2)),
            (_, Some(f)) => Some(NavReport::Ekf(f, ekf)),
            (None, None) => None,
        }
    }

    /// What the position rests on. The vehicle's own navigation filter is the authority: a
    /// submerged AUV has no GPS fix but navigates on inertial + DVL (relative position only,
    /// EKF_POS_HORIZ_REL; on its acoustic link, HIGH_LATENCY2 with the GPS failure flag).
    pub fn nav_source(&self, now: f64) -> NavSource {
        if self.pos.is_none() {
            return NavSource::None;
        }
        match self.nav_report() {
            Some(NavReport::Ekf(f, _)) => {
                use apm::EkfStatusFlags as E;
                if f.contains(E::EKF_CONST_POS_MODE) {
                    NavSource::None
                } else if f.contains(E::EKF_POS_HORIZ_ABS) {
                    NavSource::Gnss
                } else if f.contains(E::EKF_POS_HORIZ_REL) {
                    NavSource::DeadReckoning
                } else {
                    NavSource::None
                }
            }
            Some(NavReport::HighLatency(f, _)) => {
                use apm::HlFailureFlag as H;
                if f.contains(H::HL_FAILURE_FLAG_ESTIMATOR) {
                    NavSource::None
                } else if f.contains(H::HL_FAILURE_FLAG_GPS) {
                    NavSource::DeadReckoning
                } else {
                    NavSource::Gnss
                }
            }
            None if self.gps_fix < 3 => NavSource::None,
            None if self.kind == Some(Kind::Tracker) => NavSource::Gnss,
            // no EKF report: the fix arrives seconds before the EKF sets its origin
            None => match self.fix_since {
                Some(since) if now - since > NO_EKF_GRACE_S => NavSource::Gnss,
                _ => NavSource::None,
            },
        }
    }

    /// Ready to navigate: the autopilot's EKF trusts its absolute position (GPS), or its
    /// dead-reckoned one where the vehicle navigates on it: a sub (DVL) and a fixed-wing
    /// (airspeed; ArduPlane flies on without GNSS, so a plane in a jammer can be tasked out
    /// of it, and the readiness rule has a human judge the drifting position). A copter,
    /// rover, boat or blimp without GNSS has no position its autopilot trusts. Tasks wait
    /// until then.
    pub fn has_position(&self, now: f64) -> bool {
        match self.nav_source(now) {
            NavSource::Gnss => true,
            NavSource::DeadReckoning => {
                matches!(self.kind, Some(Kind::Sub | Kind::Plane | Kind::Vtol))
            }
            NavSource::None => false,
        }
    }

    fn push_event(&mut self, kind: &'static str, text: String) {
        if self.events.len() == EVENTS_KEEP {
            self.events.pop_front();
        }
        self.events.push_back(VehicleEvent { kind, text });
    }

    fn set_mode(&mut self, mav_type: apm::MavType, custom_mode: u32) {
        self.custom_mode = Some(custom_mode);
        let mode = mav::mode_name(mav_type, custom_mode);
        if let Some(prev) = &self.mode
            && *prev != mode
        {
            let text = format!("{prev} -> {mode}");
            self.push_event("mode", text);
            self.mode_since = Some(clock::wall());
        }
        self.mode = Some(mode);
    }

    /// A reported position (and the trail, one point a second at most).
    fn set_position(&mut self, lat: f64, lon: f64, alt_msl: f64, alt_rel: Option<f64>, now: f64) {
        self.pos = Some((lat, lon));
        self.seen.position = Some(clock::wall());
        self.alt_rel = alt_rel.or(self.alt_rel);
        self.alt_msl = Some(alt_msl);
        if self.last_trail.is_none_or(|t| now - t > 1.0) {
            self.last_trail = Some(now);
            if self.trail.len() == TRAIL_KEEP {
                self.trail.pop_front();
            }
            self.trail
                .push_back([round(lon, 6), round(lat, 6), round(alt_msl, 1)]);
            self.trail_count += 1;
        }
    }

    /// Navigation readiness as a fact: the value and when the message it rests on arrived
    /// (the latest navigation filter report when the vehicle sends one, else the GPS report).
    /// None before either.
    pub fn nav_ready_fact(&self, now: f64) -> Option<(bool, f64)> {
        let t = self.nav_report_t()?;
        Some((self.has_position(now), t))
    }

    /// The GPS receiver's fix type as a fact (GPS_RAW_INT, or 0 from HIGH_LATENCY2's GPS
    /// failure flag), stamped when it was reported.
    pub fn gps_fix_fact(&self) -> Option<(u8, f64)> {
        self.seen.gps.map(|t| (self.gps_fix, t))
    }

    /// The circle a fixed wing flies on its own: in LOITER around the point it navigates
    /// to, in RTL around its navigation target (home, or a rally point) once there. The
    /// center is NAV_CONTROLLER_OUTPUT's target, reported in this mode and at most
    /// [`NAV_TARGET_MAX_AGE_S`] ago; the radius WP_LOITER_RAD (RTL: RTL_RADIUS, WP_LOITER_RAD
    /// when 0), no tighter than the airframe turns, plus the tolerance it is flown to. None
    /// when not a fixed wing, in another mode, a radius or the center unknown or old, or not
    /// on its circle yet (still flying to it). The engine trusts it only while the
    /// position is on it, too.
    pub fn loiter_circle(&self) -> Option<LoiterCircle> {
        if !matches!(self.kind, Some(Kind::Plane | Kind::Vtol)) {
            return None;
        }
        let center_t = self.seen.nav_target?;
        if self.mode_since.is_some_and(|m| center_t < m)
            || clock::wall() - center_t > NAV_TARGET_MAX_AGE_S
        {
            return None;
        }
        let min_turn = self
            .identity
            .as_ref()
            .and_then(|i| i.num("min_turn_radius_m"))
            .unwrap_or(0.0);
        let loiter = self.loiter_radius_m?.abs();
        let center = self.nav_target?;
        let (rtl, radius) = match self.mode.as_deref()? {
            "LOITER" => (false, loiter),
            "RTL" => {
                let r = self.rtl_radius_m?;
                (true, if r == 0.0 { loiter } else { r.abs() })
            }
            _ => return None,
        };
        let radius = radius.max(min_turn);
        if radius <= 0.0 {
            return None;
        }
        let r = radius + (radius * 0.4).max(60.0);
        let (lat, lon) = self.pos?;
        (crate::geo::haversine_m(lat, lon, center.0, center.1) <= r).then_some(LoiterCircle {
            center,
            radius_m: r,
            rtl,
            center_t,
        })
    }

    /// Whether the navigation filter flags its GPS as glitching (EKF_STATUS_REPORT's
    /// EKF_GPS_GLITCHING; on HIGH_LATENCY2, its GPS failure flag), stamped with that report.
    /// ArduPilot keeps its absolute-position flag for seconds after GPS loss and while it
    /// rejects a glitching (or spoofed) fix, so an engagement's release reads this too.
    pub fn gps_glitch_fact(&self) -> Option<(bool, f64)> {
        match self.nav_report()? {
            NavReport::Ekf(f, t) => t.map(|t| (f.contains(apm::EkfStatusFlags::EKF_GPS_GLITCHING), t)),
            NavReport::HighLatency(f, t) => {
                t.map(|t| (f.contains(apm::HlFailureFlag::HL_FAILURE_FLAG_GPS), t))
            }
        }
    }

    /// The navigation source as a fact, stamped like [`Self::nav_ready_fact`].
    pub fn nav_source_fact(&self, now: f64) -> Option<(NavSource, f64)> {
        let t = self.nav_report_t()?;
        Some((self.nav_source(now), t))
    }

    fn nav_report_t(&self) -> Option<f64> {
        match self.nav_report() {
            Some(NavReport::Ekf(_, t) | NavReport::HighLatency(_, t)) => t,
            None => self.seen.gps,
        }
    }

    /// The circle a fixed wing flies around home in RTL, m: RTL_RADIUS (its magnitude; the
    /// sign is only the direction), or WP_LOITER_RAD when that is 0, as ArduPlane does; never
    /// tighter than the airframe's minimum turn radius from the catalog (the L1 controller
    /// widens a loiter it can't fly). The catalog figure alone until the parameters are read.
    pub fn rtl_loiter_radius_m(&self) -> Option<f64> {
        let min_turn = self
            .identity
            .as_ref()
            .and_then(|i| i.num("min_turn_radius_m"))
            .filter(|r| *r > 0.0);
        let param = match (self.rtl_radius_m, self.loiter_radius_m) {
            (Some(r), _) if r != 0.0 => Some(r.abs()),
            (_, Some(l)) => Some(l.abs()),
            _ => None,
        };
        match (param, min_turn) {
            (Some(p), Some(m)) => Some(p.max(m)),
            (p, m) => p.or(m),
        }
    }

    pub fn airborne(&self) -> bool {
        match (self.kind.and_then(Kind::airborne_alt), self.alt_rel) {
            (Some(min), Some(alt)) => alt > min,
            _ => false,
        }
    }

    pub fn distance_to(&self, lat: f64, lon: f64) -> Option<f64> {
        self.pos
            .map(|(la, lo)| crate::geo::haversine_m(la, lo, lat, lon))
    }

    pub fn last_statustext(&self) -> Option<&str> {
        self.statustext.back().map(|s| s.text.as_str())
    }

    pub fn snapshot(&self, uid: Uid, sysid: u8, link: LinkSnapshot, now: f64) -> VehicleSnapshot {
        let r = |x: Option<f64>, n| x.map(|v| round(v, n));
        VehicleSnapshot {
            uid,
            sysid,
            identity: self.identity.clone(),
            kind: self.kind,
            mode: self.mode.clone(),
            armed: self.armed,
            position: self.pos.map(|(lat, lon)| [round(lon, 6), round(lat, 6)]),
            alt_rel: r(self.alt_rel, 1),
            alt_msl: r(self.alt_msl, 1),
            roll: r(self.roll, 0),
            pitch: r(self.pitch, 0),
            heading: r(self.heading, 0),
            yaw: r(self.yaw, 0),
            tracking: self.tracking,
            nav_bearing: self.nav_bearing,
            nav_ready: self.has_position(now),
            nav_source: self.nav_source(now),
            groundspeed: r(self.groundspeed, 1),
            airspeed: r(self.airspeed, 1),
            battery_pct: self.battery_pct,
            gps_fix: self.gps_fix,
            satellites: self.satellites,
            mission_seq: self.mission_seq,
            home: self.home.map(|(lat, lon)| [round(lon, 6), round(lat, 6)]),
            link,
            statustext: self
                .statustext
                .iter()
                .rev()
                .take(3)
                .rev()
                .cloned()
                .collect(),
        }
    }
}

/// One vehicle in the world model. Its link's receive task is the only writer; readers
/// (the push loop, the API, the task layer, later the rules layer) take the lock briefly
/// to copy what they need. There is no global lock on the packet path.
pub struct VehicleCell {
    pub uid: Uid,
    pub sysid: u8,
    pub tx: Arc<dyn Transmit>,
    stats: Arc<Mutex<LinkStats>>,
    state: Mutex<Vehicle>,
    /// every message from the vehicle, for the task layer waiting on ACKs and mission requests
    events: broadcast::Sender<Arc<MavMessage>>,
}

impl VehicleCell {
    pub fn new(
        uid: Uid,
        sysid: u8,
        tx: Arc<dyn Transmit>,
        stats: Arc<Mutex<LinkStats>>,
        identity: Option<Arc<Identity>>,
    ) -> VehicleCell {
        let state = Vehicle {
            identity,
            ..Vehicle::default()
        };
        VehicleCell {
            uid,
            sysid,
            tx,
            stats,
            state: Mutex::new(state),
            events: broadcast::channel(256).0,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vehicle> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn read<R>(&self, f: impl FnOnce(&Vehicle) -> R) -> R {
        f(&self.lock())
    }

    pub fn write<R>(&self, f: impl FnOnce(&mut Vehicle) -> R) -> R {
        f(&mut self.lock())
    }

    /// A message from this vehicle (any component). `raw` is the frame as received.
    pub fn handle(&self, msg: &MavMessage, raw: &[u8], now: f64) {
        if let MavMessage::TIMESYNC(m) = msg
            && m.tc1 != 0
        {
            self.stats
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pong(m.ts1, now);
        }
        let notify = self.write(|v| v.apply(msg, raw, now));
        if notify && self.events.receiver_count() > 0 {
            let _ = self.events.send(Arc::new(msg.clone()));
        }
    }

    /// Test hook: feed a message to the link statistics (the receive loop does this).
    #[cfg(test)]
    pub(crate) fn link_on_message(&self, msg: &MavMessage) {
        self.stats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .on_message(msg, clock::mono());
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<MavMessage>> {
        self.events.subscribe()
    }

    pub fn send(&self, msg: &MavMessage) {
        self.tx.send(msg);
    }

    pub fn link_snapshot(&self, now: f64) -> LinkSnapshot {
        self.stats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot(now)
    }

    /// The link emulator's view of this vehicle's link (`crate::emu`): the brief for the
    /// fleet row and the figures for timing and the telemetry plan.
    pub fn set_emu(
        &self,
        brief: Option<crate::emu::EmuBrief>,
        figures: Option<crate::link::EmuLink>,
    ) {
        let mut stats = self.stats.lock().unwrap_or_else(|e| e.into_inner());
        stats.emu = brief;
        stats.emu_link = figures;
    }

    /// How long the task layer waits for this vehicle's answers (`crate::link::LinkTiming`).
    pub fn link_timing(&self) -> crate::link::LinkTiming {
        self.stats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .timing()
    }

    /// Test hook: the link's measured round trip, as TIMESYNC would set it.
    #[cfg(test)]
    pub(crate) fn set_link_rtt(&self, rtt_s: f64, var_s: f64) {
        let mut stats = self.stats.lock().unwrap_or_else(|e| e.into_inner());
        stats.srtt_s = Some(rtt_s);
        stats.rttvar_s = var_s;
    }

    pub fn link_counters(&self) -> LinkCounters {
        self.stats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .counters()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpi(lat: f64, lon: f64, alt: f64, rel: f64) -> MavMessage {
        MavMessage::GLOBAL_POSITION_INT(apm::GLOBAL_POSITION_INT_DATA {
            lat: (lat * 1e7) as i32,
            lon: (lon * 1e7) as i32,
            alt: (alt * 1000.0) as i32,
            relative_alt: (rel * 1000.0) as i32,
            hdg: 9000,
            ..Default::default()
        })
    }

    #[test]
    fn trail_one_point_a_second_capped() {
        let mut v = Vehicle::default();
        for i in 0..400 {
            v.apply(&gpi(37.8, -122.4, 10.0, 5.0), &[], f64::from(i) * 0.5);
        }
        assert_eq!(v.trail_count, 134); // a point when more than 1 s has passed: every 1.5 s
        assert_eq!(v.trail.len(), TRAIL_KEEP);
        assert_eq!(v.home, Some((37.8, -122.4)));
        assert_eq!(v.heading, Some(90.0));
    }

    #[test]
    fn readiness_trusts_ekf() {
        let mut v = Vehicle {
            kind: Some(Kind::Sub),
            ..Vehicle::default()
        };
        v.apply(&gpi(37.8, -122.4, -5.0, -5.0), &[], 0.0);
        assert!(!v.has_position(0.0));
        // no GPS fix at all, but the EKF has an absolute position: ready
        v.ekf_flags = Some(apm::EkfStatusFlags::EKF_POS_HORIZ_ABS);
        assert!(v.has_position(0.0));
        v.ekf_flags =
            Some(apm::EkfStatusFlags::EKF_POS_HORIZ_ABS | apm::EkfStatusFlags::EKF_CONST_POS_MODE);
        assert!(!v.has_position(0.0));
        // no EKF report: a grace period after the fix
        let mut w = Vehicle {
            kind: Some(Kind::Boat),
            ..Vehicle::default()
        };
        w.apply(&gpi(37.8, -122.4, 0.0, 0.0), &[], 0.0);
        let fix = MavMessage::GPS_RAW_INT(apm::GPS_RAW_INT_DATA {
            fix_type: apm::GpsFixType::GPS_FIX_TYPE_3D_FIX,
            ..Default::default()
        });
        w.apply(&fix, &[], 1.0);
        assert!(!w.has_position(10.0));
        assert!(w.has_position(16.5));
    }

    fn ekf(flags: apm::EkfStatusFlags) -> MavMessage {
        MavMessage::EKF_STATUS_REPORT(apm::EKF_STATUS_REPORT_DATA {
            flags,
            ..Default::default()
        })
    }

    #[test]
    fn dead_reckoning_is_ready_for_subs_and_fixed_wing() {
        use apm::EkfStatusFlags as E;
        for (kind, ready) in [
            (Kind::Sub, true),
            (Kind::Plane, true),
            (Kind::Vtol, true),
            (Kind::Boat, false),
            (Kind::Copter, false),
            (Kind::Ugv, false),
            (Kind::Blimp, false),
        ] {
            let mut v = Vehicle {
                kind: Some(kind),
                ..Vehicle::default()
            };
            v.apply(&gpi(37.8, -122.4, -5.0, -5.0), &[], 0.0);
            v.apply(&ekf(E::EKF_POS_HORIZ_REL | E::EKF_ATTITUDE), &[], 0.0);
            assert_eq!(v.nav_source(0.0), NavSource::DeadReckoning, "{kind:?}");
            assert_eq!(v.has_position(0.0), ready, "{kind:?}");
            v.apply(&ekf(E::EKF_POS_HORIZ_REL | E::EKF_POS_HORIZ_ABS), &[], 0.0);
            assert_eq!(v.nav_source(0.0), NavSource::Gnss);
            assert!(v.has_position(0.0));
        }
    }

    #[test]
    fn high_latency2_without_its_gps_failure_flag_is_a_fresh_3d_fix() {
        let mut v = Vehicle {
            kind: Some(Kind::Boat),
            mav_type: Some(apm::MavType::MAV_TYPE_SURFACE_BOAT),
            ..Vehicle::default()
        };
        let hl2 = |failure_flags| {
            MavMessage::HIGH_LATENCY2(apm::HIGH_LATENCY2_DATA {
                latitude: 378_010_000,
                longitude: -1_224_020_000,
                custom_mode: 10, // AUTO
                failure_flags,
                mavtype: apm::MavType::MAV_TYPE_SURFACE_BOAT,
                ..Default::default()
            })
        };
        // no fix type in HIGH_LATENCY2: without the failure flag, a 3D fix, said so
        v.apply(&hl2(apm::HlFailureFlag::empty()), &[], 0.0);
        assert_eq!(v.gps_fix_fact().map(|f| f.0), Some(3));
        assert!(v.gps_fix_hl2);
        // with it, no fix
        v.apply(&hl2(apm::HlFailureFlag::HL_FAILURE_FLAG_GPS), &[], 5.0);
        assert_eq!(v.gps_fix_fact().map(|f| f.0), Some(0));
        // the receiver's own report takes over
        let fix = MavMessage::GPS_RAW_INT(apm::GPS_RAW_INT_DATA {
            fix_type: apm::GpsFixType::GPS_FIX_TYPE_3D_FIX,
            satellites_visible: 9,
            ..Default::default()
        });
        v.apply(&fix, &[], 6.0);
        assert!(!v.gps_fix_hl2);
        assert_eq!(v.gps_fix_fact().map(|f| f.0), Some(3));
    }

    #[test]
    fn a_fixed_wing_loitering_or_circling_home_reports_its_circle() {
        let mut v = Vehicle {
            kind: Some(Kind::Plane),
            mode: Some("LOITER".into()),
            loiter_radius_m: Some(150.0),
            rtl_radius_m: Some(0.0),
            pos: Some((37.80, -122.45)),
            home: Some((37.81, -122.45)),
            ..Vehicle::default()
        };
        // unknown center: no circle
        assert_eq!(v.loiter_circle(), None);
        // NAV_CONTROLLER_OUTPUT: the loiter center 150 m east
        let nav = MavMessage::NAV_CONTROLLER_OUTPUT(apm::NAV_CONTROLLER_OUTPUT_DATA {
            target_bearing: 90,
            wp_dist: 150,
            ..Default::default()
        });
        v.apply(&nav, &[], 0.0);
        let c = v.loiter_circle().expect("a circle");
        assert!((c.radius_m - 210.0).abs() < 1e-9, "{c:?}"); // 150 m and its 60 m tolerance
        assert!(!c.rtl);
        assert_eq!(Some(c.center_t), v.seen.nav_target);
        let d = crate::geo::haversine_m(37.80, -122.45, c.center.0, c.center.1);
        assert!((d - 150.0).abs() < 1.0, "{d}");
        // far from its circle: still flying to it
        v.pos = Some((37.70, -122.45));
        assert_eq!(v.loiter_circle(), None);
        v.pos = Some((37.80, -122.45));
        // a center reported before the mode changed is the previous mode's target
        v.mode_since = Some(clock::wall() + 1.0);
        assert_eq!(v.loiter_circle(), None);
        v.mode_since = None;
        // a center that stopped coming is not trusted for long
        v.seen.nav_target = Some(clock::wall() - NAV_TARGET_MAX_AGE_S - 1.0);
        assert_eq!(v.loiter_circle(), None);
        // RTL: around its navigation target (home here; a rally point if it has one) once
        // there, at WP_LOITER_RAD when RTL_RADIUS is 0
        v.mode = Some("RTL".into());
        v.apply(&gpi(37.811, -122.45, 100.0, 100.0), &[], 1.0);
        let nav_home = MavMessage::NAV_CONTROLLER_OUTPUT(apm::NAV_CONTROLLER_OUTPUT_DATA {
            target_bearing: 180,
            wp_dist: 111,
            ..Default::default()
        });
        v.apply(&nav_home, &[], 1.0);
        let c = v.loiter_circle().expect("circling home");
        assert!(c.rtl);
        assert!((c.radius_m - 210.0).abs() < 1e-9, "{c:?}");
        assert!(crate::geo::haversine_m(37.81, -122.45, c.center.0, c.center.1) < 2.0, "{c:?}");
        // in AUTO it flies its mission (an orbit task reports its own circle)
        v.mode = Some("AUTO".into());
        assert_eq!(v.loiter_circle(), None);
        // a copter holds a point instead: no circle
        v.mode = Some("LOITER".into());
        v.kind = Some(Kind::Copter);
        assert_eq!(v.loiter_circle(), None);
    }

    #[test]
    fn high_latency2_keeps_a_submerged_auv_current() {
        use apm::EkfStatusFlags as E;
        let mut v = Vehicle {
            kind: Some(Kind::Sub),
            mav_type: Some(apm::MavType::MAV_TYPE_SUBMARINE),
            ..Vehicle::default()
        };
        // at the surface: a fix, home at sea level
        v.apply(&gpi(37.8, -122.4, 0.0, 0.0), &[], 0.0);
        v.apply(&ekf(E::EKF_POS_HORIZ_ABS | E::EKF_POS_HORIZ_REL), &[], 0.0);
        assert_eq!(v.nav_source(0.0), NavSource::Gnss);
        // dived: no more streams, one acoustic status every 20 s
        let hl2 = MavMessage::HIGH_LATENCY2(apm::HIGH_LATENCY2_DATA {
            latitude: 378_010_000,
            longitude: -1_224_020_000,
            altitude: -40,
            custom_mode: 3, // AUTO
            heading: 45,    // 90 degrees
            groundspeed: 10,
            battery: 71,
            wp_num: 2,
            failure_flags: apm::HlFailureFlag::HL_FAILURE_FLAG_GPS,
            mavtype: apm::MavType::MAV_TYPE_SUBMARINE,
            ..Default::default()
        });
        v.apply(&hl2, &[], 20.0);
        assert_eq!(v.pos, Some((37.801, -122.402)));
        assert_eq!((v.alt_msl, v.alt_rel), (Some(-40.0), Some(-40.0)));
        assert_eq!(v.mode.as_deref(), Some("AUTO"));
        assert_eq!(v.heading, Some(90.0));
        assert_eq!(v.groundspeed, Some(2.0));
        let [e, n] = v.vel_en.expect("velocity");
        assert!((e - 2.0).abs() < 1e-9 && n.abs() < 1e-9, "{e} {n}");
        assert_eq!(v.battery_pct, Some(71));
        assert_eq!(v.mission_seq, Some(2));
        assert_eq!(v.gps_fix, 0);
        assert_eq!(v.trail_count, 2);
        // the newer report wins: dead reckoning, still ready for a sub, stamped with it
        assert_eq!(v.nav_source(20.0), NavSource::DeadReckoning);
        assert!(v.has_position(20.0));
        assert_eq!(v.nav_ready_fact(20.0).map(|f| f.1), v.seen.hl2);
        // surfaced: the EKF is back on GPS
        v.seen.hl2 = Some(0.0);
        v.apply(&ekf(E::EKF_POS_HORIZ_ABS), &[], 40.0);
        assert_eq!(v.nav_source(40.0), NavSource::Gnss);
        // an estimator failure is no position at all
        v.seen.hl2 = None;
        v.ekf_flags = None;
        v.hl_failure = Some(apm::HlFailureFlag::HL_FAILURE_FLAG_ESTIMATOR);
        assert!(!v.has_position(40.0));
    }

    #[test]
    fn rtl_circle_from_params_then_catalog() {
        let ident = |r: f64| {
            let m = serde_json::json!({"min_turn_radius_m": r});
            Some(Arc::new(Identity::from_manifest(
                m.as_object().expect("object"),
            )))
        };
        let mut v = Vehicle {
            kind: Some(Kind::Plane),
            identity: ident(3_000.0),
            ..Vehicle::default()
        };
        assert_eq!(v.rtl_loiter_radius_m(), Some(3_000.0)); // parameters not read yet
        v.apply(&param("RTL_RADIUS", 0.0), &[], 0.0);
        assert_eq!(v.rtl_loiter_radius_m(), Some(3_000.0)); // 0: waits for WP_LOITER_RAD
        v.apply(&param("WP_LOITER_RAD", 3_500.0), &[], 0.0);
        assert_eq!(v.rtl_loiter_radius_m(), Some(3_500.0));
        v.apply(&param("RTL_RADIUS", -9_000.0), &[], 0.0); // negative: counter-clockwise
        assert_eq!(v.rtl_loiter_radius_m(), Some(9_000.0));
        // never tighter than the airframe can turn
        v.identity = ident(12_000.0);
        assert_eq!(v.rtl_loiter_radius_m(), Some(12_000.0));
        // a SITL plane: no catalog turn figure, plane.parm's 150 m
        let mut w = Vehicle {
            kind: Some(Kind::Plane),
            ..Vehicle::default()
        };
        assert_eq!(w.rtl_loiter_radius_m(), None);
        w.apply(&param("RTL_RADIUS", 0.0), &[], 0.0);
        w.apply(&param("WP_LOITER_RAD", 150.0), &[], 0.0);
        assert_eq!(w.rtl_loiter_radius_m(), Some(150.0));
    }

    fn param(id: &str, value: f32) -> MavMessage {
        MavMessage::PARAM_VALUE(apm::PARAM_VALUE_DATA {
            param_value: value,
            param_id: mav::char_array(id),
            ..Default::default()
        })
    }
}
