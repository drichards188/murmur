//! ArduRover mode semantics shared by the ground vehicle and the sailboat: what each mode asks
//! the vehicle to do (a goal), the mission walk with ArduRover's messages ("Reached waypoint
//! #N", "Mission Complete", "Reached destination"), and AR_WPNav's waypoint acceptance (inside
//! WP_RADIUS, or past the line through the waypoint square to the leg).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use mavlink::dialects::ardupilotmega::{MavCmd, MavSeverity};

use super::Tick;
use super::apmission::{self, Jumps};
use crate::geo::{M_PER_DEG, bearing_deg, distance_m, wrap180};
use crate::kinematics::{Out, Target, TargetKind};

/// Modes ArduRover refuses here: LEARNING (2) is gone from the firmware, INITIALISING is not
/// selectable, DOCK needs precision landing, FOLLOW needs FOLL_ENABLE (default 0).
pub fn accepts_mode(name: &str) -> bool {
    !matches!(name, "LEARNING" | "INITIALISING" | "DOCK" | "FOLLOW")
}

/// CIRC_RADIUS default: CIRCLE mode circles a point this far ahead, clockwise (CIRC_DIR 0).
const CIRCLE_RADIUS_M: f64 = 20.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Goal {
    /// stop (or drift, for a sailboat with its sail let out)
    Stop,
    /// drive to a point; `stop` = come to rest there (decelerate on the way in)
    Go {
        lat: f64,
        lon: f64,
        speed: f64,
        stop: bool,
    },
    /// stay within `radius` of a point (boat LOITER)
    Loiter { lat: f64, lon: f64, radius: f64 },
    /// drive a circle clockwise
    Circle {
        lat: f64,
        lon: f64,
        radius: f64,
        speed: f64,
    },
}

/// Per-vehicle ArduRover navigation state.
pub struct Nav {
    /// metres: AR_WPNav acceptance radius
    pub wp_radius: f64,
    /// boats loiter where rovers stop (RTL at home, LOITER mode)
    pub boat: bool,
    pub loiter_radius: f64,
    mode: &'static str,
    armed: bool,
    seq: u16,
    /// start of the current leg (for "passed the waypoint" and cross-track)
    pub origin: (f64, f64),
    rtl_reached: bool,
    complete: bool,
    circle: Option<(f64, f64)>,
    /// the mission item whose start AP_Mission has announced
    announced: Option<u16>,
    jumps: Jumps,
    /// the mission (and current item) as it finished, to notice a new one
    finished: u64,
}

impl Nav {
    pub fn new(wp_radius: f64, boat: bool, loiter_radius: f64) -> Nav {
        Nav {
            wp_radius,
            boat,
            loiter_radius,
            mode: "",
            armed: false,
            seq: u16::MAX,
            origin: (0.0, 0.0),
            rtl_reached: false,
            complete: false,
            circle: None,
            announced: None,
            jumps: Jumps::default(),
            finished: 0,
        }
    }

    /// What the current mode wants this tick. Emits the mode's events into `t.out`.
    pub fn goal(&mut self, t: &mut Tick) -> Goal {
        let (lat, lon) = (t.st.lat, t.st.lon);
        if t.st.mode == "AUTO" && self.mode != "AUTO" && t.mission.items.len() <= 1 {
            // ArduRover refuses AUTO without a mission (AP_Mission::present: more than home).
            // The refusal belongs in the DO_SET_MODE handler (MAV_RESULT_FAILED); behaviors
            // have no hook there yet, so the mode change is undone here, within one tick.
            t.st.mode = if self.mode.is_empty() {
                "MANUAL"
            } else {
                self.mode
            };
            for (sev, text) in [
                (
                    MavSeverity::MAV_SEVERITY_NOTICE,
                    "No Mission. Can't set AUTO.",
                ),
                (
                    MavSeverity::MAV_SEVERITY_WARNING,
                    "Flight mode change failed",
                ),
            ] {
                t.out.push(Out::Text(sev, text.into()));
            }
        }
        if t.st.mode != self.mode {
            self.announced = None; // entering AUTO starts (or resumes) the mission
            if t.st.mode == "AUTO" && self.complete {
                self.restart(t); // MIS_RESTART 0 resumes, but a finished mission starts over
            }
        }
        if self.armed && !t.st.armed && t.st.mode != "AUTO" {
            // ArduRover resets the mission on a disarm outside AUTO
            self.restart(t);
            t.mission.seq = 0;
        }
        if self.complete && fingerprint(t) != self.finished {
            self.restart(t); // a new mission, or DO_SET_MISSION_CURRENT: run it
        }
        if t.st.mode != self.mode || t.st.armed != self.armed {
            self.mode = t.st.mode;
            self.armed = t.st.armed;
            self.origin = (lat, lon);
            self.rtl_reached = false;
            self.circle = None;
            self.seq = u16::MAX;
        }
        if !t.st.armed {
            return Goal::Stop;
        }
        let cruise = t.spec.cruise_mps;
        match t.st.mode {
            "AUTO" => self.auto(t),
            "RTL" | "SMART_RTL" => {
                // SMART_RTL retraces the recorded path; here it heads straight home like RTL
                let (hlat, hlon, _) = t.st.home;
                if !self.rtl_reached && self.arrived(lat, lon, hlat, hlon) {
                    self.rtl_reached = true;
                    t.out.push(Out::Text(
                        MavSeverity::MAV_SEVERITY_INFO,
                        "Reached destination".into(),
                    ));
                }
                if self.rtl_reached {
                    return self.rest(hlat, hlon);
                }
                Goal::Go {
                    lat: hlat,
                    lon: hlon,
                    speed: cruise,
                    stop: true,
                }
            }
            "LOITER" => {
                let (plat, plon) = t.st.hold_point;
                self.rest(plat, plon)
            }
            "CIRCLE" => {
                let (clat, clon) = *self
                    .circle
                    .get_or_insert_with(|| ahead(lat, lon, t.st.heading, CIRCLE_RADIUS_M));
                Goal::Circle {
                    lat: clat,
                    lon: clon,
                    radius: CIRCLE_RADIUS_M,
                    speed: cruise,
                }
            }
            // MANUAL, ACRO, STEERING, SIMPLE: no RC input, so no throttle. HOLD stops; GUIDED
            // starts stopped and waits for a target.
            _ => Goal::Stop,
        }
    }

    /// Rovers stop at a point they have reached; boats hold station within the loiter radius.
    fn rest(&self, lat: f64, lon: f64) -> Goal {
        if self.boat {
            Goal::Loiter {
                lat,
                lon,
                radius: self.loiter_radius,
            }
        } else {
            Goal::Stop
        }
    }

    fn auto(&mut self, t: &mut Tick) -> Goal {
        let (lat, lon) = (t.st.lat, t.st.lon);
        let cruise = t.spec.cruise_mps;
        if t.mission.items.len() <= 1 || self.complete {
            return Goal::Stop; // MIS_DONE_BEHAVE 0: hold where it finished
        }
        let Some(tgt) = self.walk(t) else {
            return Goal::Stop;
        };
        let seq = t.mission.seq;
        let done = match tgt.kind {
            TargetKind::Waypoint | TargetKind::Rtl => self.arrived(lat, lon, tgt.lat, tgt.lon),
            TargetKind::Takeoff => true, // nothing to take off from: on to the next
            TargetKind::Loiter | TargetKind::Hold => false,
        };
        if done {
            if tgt.kind != TargetKind::Takeoff {
                // on the wire MISSION_ITEM_REACHED arrives before the text (seen in SITL)
                t.out.push(Out::ItemReached(seq));
                if tgt.kind == TargetKind::Waypoint {
                    t.out.push(Out::Text(
                        MavSeverity::MAV_SEVERITY_INFO,
                        format!("Reached waypoint #{seq}"),
                    ));
                }
            }
            self.origin = (tgt.lat, tgt.lon);
            if usize::from(seq) + 1 >= t.mission.items.len() {
                self.finish(t);
            } else {
                t.mission.seq = seq + 1;
                if self.walk(t).is_some() {
                    t.out.push(Out::MissionCurrent(t.mission.seq));
                }
            }
            return Goal::Stop;
        }
        match tgt.kind {
            TargetKind::Loiter if distance_m(lat, lon, tgt.lat, tgt.lon) <= self.wp_radius => {
                // LOITER_UNLIM: stay (boats loiter around it)
                self.rest(tgt.lat, tgt.lon)
            }
            _ => Goal::Go {
                lat: tgt.lat,
                lon: tgt.lon,
                speed: cruise,
                stop: tgt.kind != TargetKind::Waypoint || !has_nav_after(t, seq),
            },
        }
    }

    /// AP_Mission's walk to the current navigation command: the shared walker takes DO_JUMPs
    /// (and says so, as AP_Mission does from a jump's second run), then the start of a new
    /// command is announced. None = the mission has run out (completed here).
    fn walk(&mut self, t: &mut Tick) -> Option<Target> {
        let s0 = t.mission.seq;
        if let Some(it) = t.mission.items.get(usize::from(s0))
            && it.command == MavCmd::MAV_CMD_DO_JUMP
            && t.mission
                .jumps
                .get(&s0)
                .copied()
                .unwrap_or(it.param2 as i32)
                != 0
        {
            self.jumps.ran(s0, it.param2 as i32, t.out);
        }
        let tgt = t
            .mission
            .auto_target(t.st, t.spec.cruise_mps, 0.0, self.loiter_radius);
        if tgt.kind == TargetKind::Hold {
            self.finish(t);
            return None;
        }
        let seq = t.mission.seq;
        if self.announced != Some(seq)
            && let Some(it) = t.mission.items.get(usize::from(seq))
        {
            self.announced = Some(seq);
            apmission::started(seq, it.command, t.out);
        }
        Some(tgt)
    }

    /// Mission done: "Mission Complete", and the current item stays the last one (as AP_Mission
    /// reports it) until a disarm outside AUTO resets the mission.
    fn finish(&mut self, t: &mut Tick) {
        let last = t.mission.items.len().saturating_sub(1) as u16;
        t.mission.seq = last;
        self.finished = fingerprint(t);
        if !self.complete {
            self.complete = true;
            t.out.push(Out::Text(
                MavSeverity::MAV_SEVERITY_NOTICE,
                "Mission Complete".into(),
            ));
        }
    }

    fn restart(&mut self, t: &mut Tick) {
        self.complete = false;
        self.announced = None;
        self.jumps.clear();
        t.mission.jumps.clear();
        t.mission.seq = 1;
    }

    /// AR_WPNav: within WP_RADIUS, or past the waypoint (beyond the line through it square to
    /// the leg from `origin`).
    pub fn arrived(&self, lat: f64, lon: f64, tlat: f64, tlon: f64) -> bool {
        if distance_m(lat, lon, tlat, tlon) <= self.wp_radius {
            return true;
        }
        let (on, oe) = local(self.origin, (tlat, tlon));
        let (pn, pe) = local(self.origin, (lat, lon));
        let len2 = on * on + oe * oe;
        len2 > 1.0 && (pn * on + pe * oe) / len2 >= 1.0
    }

    /// Signed cross-track error (m) from the leg origin -> (tlat, tlon): positive = left of the
    /// track, as AR_WPNav::crosstrack_error_m.
    pub fn crosstrack(&self, lat: f64, lon: f64, tlat: f64, tlon: f64) -> f64 {
        let (on, oe) = local(self.origin, (tlat, tlon));
        let (pn, pe) = local(self.origin, (lat, lon));
        let len = on.hypot(oe);
        if len < 1.0 {
            return 0.0;
        }
        (pn * oe - pe * on) / len
    }
}

/// The mission's content and current item, hashed.
pub fn fingerprint(t: &Tick) -> u64 {
    let mut h = DefaultHasher::new();
    t.mission.seq.hash(&mut h);
    for it in &t.mission.items {
        (
            it.command as u32,
            it.x,
            it.y,
            it.z.to_bits(),
            it.param1.to_bits(),
        )
            .hash(&mut h);
    }
    h.finish()
}

/// Is there more to drive after item `seq`: a navigation command (ids below 100) or a DO_JUMP
/// that repeats? Decides whether the vehicle slows to stop at this waypoint.
fn has_nav_after(t: &Tick, seq: u16) -> bool {
    t.mission.items.iter().skip(usize::from(seq) + 1).any(|it| {
        if it.command == MavCmd::MAV_CMD_DO_JUMP {
            it.param2 != 0.0
        } else {
            (it.command as u32) < 100
        }
    })
}

/// (north, east) metres of `p` from `o`, flat earth.
pub fn local(o: (f64, f64), p: (f64, f64)) -> (f64, f64) {
    let n = (p.0 - o.0) * M_PER_DEG;
    let e = (p.1 - o.1) * M_PER_DEG * o.0.to_radians().cos();
    (n, e)
}

/// The point `dist` metres ahead on `heading`.
pub fn ahead(lat: f64, lon: f64, heading: f64, dist: f64) -> (f64, f64) {
    let (s, c) = heading.to_radians().sin_cos();
    (
        lat + dist * c / M_PER_DEG,
        lon + dist * s / (M_PER_DEG * lat.to_radians().cos()),
    )
}

/// Heading for a circle or loiter ring around a point, clockwise (as kinematics::guide).
pub fn ring_heading(lat: f64, lon: f64, clat: f64, clon: f64, radius: f64) -> f64 {
    let dist = distance_m(lat, lon, clat, clon);
    let btc = bearing_deg(lat, lon, clat, clon);
    let corr = ((dist - radius) / radius.max(1.0) * 90.0).clamp(-90.0, 90.0);
    wrap180(btc - 90.0 + corr).rem_euclid(360.0)
}
