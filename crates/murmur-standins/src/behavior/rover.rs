//! ArduRover mode semantics shared by the ground vehicle, the sailboat and the motorboat: what
//! each mode asks the vehicle to do (a goal), and what ArduRover adds on top of the shared
//! mission walker (mission.rs, which says AP_Mission's "Mission: N WP" as each command starts
//! and counts its DO_JUMPs): "Reached waypoint #N" with MISSION_ITEM_REACHED, "Reached
//! destination" in RTL, AR_WPNav's waypoint acceptance (inside WP_RADIUS, or past the line
//! through the waypoint square to the leg), the mission reset on a disarm outside AUTO, and
//! MIS_DONE_BEHAVE at the end.

use mavlink::dialects::ardupilotmega::{MavCmd, MavSeverity};

use super::Tick;
use crate::geo::{M_PER_DEG, bearing_deg, distance_m, wrap180};
use crate::kinematics::{Out, Target, TargetKind};
use crate::mission;

/// Modes ArduRover refuses here: LEARNING (2) is gone from the firmware, INITIALISING is not
/// selectable, DOCK needs precision landing, FOLLOW needs FOLL_ENABLE (default 0).
pub fn accepts_mode(name: &str) -> bool {
    !matches!(name, "LEARNING" | "INITIALISING" | "DOCK" | "FOLLOW")
}

/// AP_Arming_Rover::mode_checks: RTL, SMART_RTL and INITIALISING do not allow arming.
pub fn arming_refusal(mode: &str) -> Option<&'static str> {
    matches!(mode, "RTL" | "SMART_RTL" | "INITIALISING").then_some("Mode not armable")
}

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
    /// boats loiter where rovers stop (RTL at home, LOITER mode)
    boat: bool,
    /// the parameter holding the loiter radius (LOIT_RADIUS, or SAIL_LOIT_RADIUS)
    loiter_param: &'static str,
    mode: &'static str,
    armed: bool,
    /// start of the current leg (for "passed the waypoint" and cross-track)
    pub origin: (f64, f64),
    rtl_reached: bool,
    circle: Option<(f64, f64)>,
    wp_radius: f64,
}

impl Nav {
    pub fn new(boat: bool, loiter_param: &'static str) -> Nav {
        Nav {
            boat,
            loiter_param,
            mode: "",
            armed: false,
            origin: (0.0, 0.0),
            rtl_reached: false,
            circle: None,
            wp_radius: 2.0,
        }
    }

    fn loiter_radius(&self, t: &Tick) -> f64 {
        t.params.f(self.loiter_param, 2.0)
    }

    /// What the current mode wants this tick. Emits the mode's events into `t.out`.
    pub fn goal(&mut self, t: &mut Tick) -> Goal {
        let (lat, lon) = (t.st.lat, t.st.lon);
        self.wp_radius = t.params.f("WP_RADIUS", 2.0);
        if self.armed && !t.st.armed && t.st.mode != "AUTO" {
            // ArduRover resets the mission on a disarm outside AUTO (AP_Arming_Rover::disarm)
            t.mission.set_current(0);
            t.mission.jumps.clear();
        }
        if t.st.mode != self.mode || t.st.armed != self.armed {
            self.mode = t.st.mode;
            self.armed = t.st.armed;
            self.origin = (lat, lon);
            // "Reached destination" is for an RTL that had somewhere to go: entered already
            // within WP_RADIUS of home, ArduRover (motorboat SITL) says nothing
            let (hlat, hlon, _) = t.st.home;
            self.rtl_reached = matches!(t.st.mode, "RTL" | "SMART_RTL")
                && distance_m(lat, lon, hlat, hlon) <= self.wp_radius;
            self.circle = None;
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
                    return self.rest(t, hlat, hlon);
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
                self.rest(t, plat, plon)
            }
            "CIRCLE" => {
                // CIRC_RADIUS ahead of the vehicle, clockwise (CIRC_DIR 0)
                let r = t.params.f("CIRC_RADIUS", 20.0);
                let (clat, clon) = *self
                    .circle
                    .get_or_insert_with(|| ahead(lat, lon, t.st.heading, r));
                Goal::Circle {
                    lat: clat,
                    lon: clon,
                    radius: r,
                    speed: cruise,
                }
            }
            // MANUAL, ACRO, STEERING, SIMPLE: no RC input, so no throttle. HOLD stops; GUIDED
            // starts stopped and waits for a target.
            _ => Goal::Stop,
        }
    }

    /// Rovers stop at a point they have reached; boats hold station within the loiter radius.
    fn rest(&self, t: &Tick, lat: f64, lon: f64) -> Goal {
        if self.boat {
            Goal::Loiter {
                lat,
                lon,
                radius: self.loiter_radius(t),
            }
        } else {
            Goal::Stop
        }
    }

    fn auto(&mut self, t: &mut Tick) -> Goal {
        let (lat, lon) = (t.st.lat, t.st.lon);
        if !t.mission.present() {
            return Goal::Stop;
        }
        let tgt = self.walk(t);
        let seq = t.mission.seq;
        match tgt.kind {
            TargetKind::Hold => {
                // the mission is over: MIS_DONE_BEHAVE 0 holds, 1 loiters (boats)
                if t.params.f("MIS_DONE_BEHAVE", 0.0) == 1.0 {
                    self.rest(t, tgt.lat, tgt.lon)
                } else {
                    Goal::Stop
                }
            }
            TargetKind::Waypoint if self.arrived(lat, lon, tgt.lat, tgt.lon) => {
                // MISSION_ITEM_REACHED, then the text (in that order on the wire, seen in
                // SITL), then the DO commands that follow, all in one AP_Mission update
                let is_wp = t
                    .mission
                    .items
                    .get(usize::from(seq))
                    .is_some_and(|it| it.command == MavCmd::MAV_CMD_NAV_WAYPOINT);
                let text = is_wp.then(|| format!("Reached waypoint #{seq}"));
                t.mission.nav_done(t.st, t.out, text);
                self.origin = (tgt.lat, tgt.lon);
                self.walk(t);
                Goal::Stop
            }
            TargetKind::Loiter if distance_m(lat, lon, tgt.lat, tgt.lon) <= self.wp_radius => {
                // LOITER_UNLIM / a waypoint's hold time: stay (boats loiter around it)
                self.rest(t, tgt.lat, tgt.lon)
            }
            _ => Goal::Go {
                lat: tgt.lat,
                lon: tgt.lon,
                speed: tgt.speed,
                stop: tgt.kind != TargetKind::Waypoint || !has_nav_after(t, seq),
            },
        }
    }

    /// The shared walker to the current navigation command (it announces each command as it
    /// starts and each DO_JUMP as it runs). A finished mission keeps its last item as the
    /// current one, as AP_Mission reports it.
    fn walk(&mut self, t: &mut Tick) -> Target {
        let cruise = t.spec.cruise_mps;
        let loiter = self.loiter_radius(t);
        t.mission.auto_target(t.st, cruise, 0.0, loiter, t.out)
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

/// Is there more to drive after item `seq`: a navigation command or a DO_JUMP that repeats?
/// Decides whether the vehicle slows to stop at this waypoint.
fn has_nav_after(t: &Tick, seq: u16) -> bool {
    t.mission.items.iter().skip(usize::from(seq) + 1).any(|it| {
        if it.command == MavCmd::MAV_CMD_DO_JUMP {
            it.param2 != 0.0
        } else {
            mission::is_nav(it.command)
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
