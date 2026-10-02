//! Multirotor (ArduCopter 4.8): hovers, takes off and lands vertically, holds position.
//!
//! What the GCS sees follows ArduCopter as SITL shows it (sim/tests/standins_vs_sitl_air.py):
//! mode numbers and the modes that refuse arming, "No such mode", AUTO refused when flying
//! without a mission, GUIDED takeoff (MAV_CMD_NAV_TAKEOFF), no disarming in flight, auto-disarm
//! DISARM_DELAY after landing or idling armed, RTL (climb to RTL_ALT_M, return, loiter
//! RTL_LOIT_TIME, land, disarm), LAND (fast to LAND_ALT_LOW, then LAND_SPD), AUTO_OPTIONS 3 as
//! Murmur sets it (arming in AUTO, takeoff without throttle). Movement is simple: velocity
//! toward the target within WP_SPD (the catalog cruise speed) and WP_ACC, climb WP_SPD_UP,
//! descend WP_SPD_DN.

use mavlink::dialects::ardupilotmega::{
    MavCmd, MavModeFlag, MavParamType, MavResult, MavSeverity, MavType,
};

use super::auto::{Auto, Nav};
use super::{Behavior, Command, Ctx, Tick, modes};
use crate::config::Spec;
use crate::geo::{M_PER_DEG, wrap180};
use crate::kinematics::{Out, State};
use crate::mission::Mission;
use crate::params::Params;

// ArduCopter 4.8 parameters this behavior flies by, with their defaults (extra_params puts
// them in the parameter store under these names; WP_SPD is the catalog cruise speed)
const DEFAULTS: &[(&str, MavParamType, f32)] = &[
    ("WP_SPD_UP", F32, 2.5),
    ("WP_SPD_DN", F32, 1.5),
    ("WP_ACC", F32, 2.5),
    ("WP_RADIUS_M", F32, 2.0),
    ("RTL_ALT_M", F32, 15.0),
    ("RTL_LOIT_TIME", I32, 5000.0),
    ("LAND_SPD_MS", F32, 0.5),
    ("LAND_ALT_LOW_M", F32, 10.0),
    ("PILOT_SPD_UP", F32, 2.5),
    ("PILOT_SPD_DN", F32, 0.0),
    ("DISARM_DELAY", I8, 10.0),
];
const F32: MavParamType = MavParamType::MAV_PARAM_TYPE_REAL32;
const I32: MavParamType = MavParamType::MAV_PARAM_TYPE_INT32;
const I8: MavParamType = MavParamType::MAV_PARAM_TYPE_INT8;

/// The parameters as this tick reads them.
#[derive(Debug, Clone, Copy)]
struct Tune {
    wp_spd: f64,
    spd_up: f64,
    spd_dn: f64,
    wp_acc: f64,
    wp_radius: f64,
    rtl_alt: f64,
    rtl_loit_s: f64,
    land_spd: f64,
    land_alt_low: f64,
    /// PILOT_SPD_DN, 0 meaning PILOT_SPD_UP
    pilot_dn: f64,
    disarm_delay: f64,
}

impl Tune {
    fn read(p: &Params, spec: &Spec) -> Tune {
        let d = |name: &str| {
            let fallback = DEFAULTS
                .iter()
                .find(|(n, _, _)| *n == name)
                .map_or(0.0, |x| x.2);
            p.f(name, f64::from(fallback))
        };
        let pilot_dn = d("PILOT_SPD_DN");
        Tune {
            wp_spd: p.f("WP_SPD", spec.cruise_mps).max(0.5),
            spd_up: d("WP_SPD_UP").max(0.1),
            spd_dn: d("WP_SPD_DN").max(0.1),
            wp_acc: d("WP_ACC").max(0.1),
            wp_radius: d("WP_RADIUS_M").max(0.05),
            rtl_alt: d("RTL_ALT_M"),
            rtl_loit_s: d("RTL_LOIT_TIME") / 1000.0,
            land_spd: d("LAND_SPD_MS").max(0.1),
            land_alt_low: d("LAND_ALT_LOW_M"),
            pilot_dn: if pilot_dn > 0.0 {
                pilot_dn
            } else {
                d("PILOT_SPD_UP").max(0.1)
            },
            disarm_delay: d("DISARM_DELAY"),
        }
    }
}

/// how fast a pilot descent builds up, as SITL shows it
const PILOT_ACCEL_Z: f64 = 0.8;
/// the land detector: on the ground this long before "landed"
const LAND_DETECT_S: f64 = 1.0;
/// ATC_SLEW_YAW
const YAW_RATE_DPS: f64 = 60.0;
/// AUTO_OPTIONS bit 0: arming in AUTO allowed (Murmur sets AUTO_OPTIONS 3)
const AUTO_ALLOWS_ARMING: bool = true;
/// BRAKE_MODE_DECEL_RATE_MSS (config.h)
const BRAKE_DECEL: f64 = 7.5;
/// MAV_CMD_COMPONENT_ARM_DISARM param2 that forces a disarm
const FORCE_DISARM: f32 = 21196.0;

/// Mode numbers ArduCopter 4.8 has (SITL build); the rest of pymavlink's table is "No such mode".
const EXISTS: &[u32] = &[
    0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 27,
];

#[derive(Debug, Clone, Copy, PartialEq)]
enum Rtl {
    Climb,
    Return,
    Loiter(f64),
    Land,
}

#[derive(Debug, Clone, Copy)]
enum Vert {
    Alt(f64),
    Land,
    /// the pilot's stick at the bottom: down at this rate
    Descend(f64),
}

#[derive(Debug, Clone, Copy)]
struct Goal {
    /// where to be; None = stay
    to: Option<(f64, f64)>,
    /// stop at `to` (false: fly through it, an intermediate waypoint)
    stop: bool,
    vert: Vert,
}

pub struct Copter {
    auto: Auto,
    landed: bool,
    /// armed and landed for this long (auto-disarm)
    idle_s: f64,
    /// touching the ground during a landing for this long (land detector)
    touch_s: f64,
    /// GUIDED destination, or a user takeoff's (lat, lon, alt MSL)
    guided: Option<(f64, f64, f64)>,
    rtl: Rtl,
    /// altitude the position-hold modes keep
    hold_alt: f64,
    /// a waypoint's hold time left (param1)
    delay_s: Option<f64>,
    vel: (f64, f64),
    k: Tune,
    /// the mode the behavior last saw (a failsafe changes modes behind its back)
    mode_seen: &'static str,
}

impl Default for Copter {
    fn default() -> Self {
        Self::new()
    }
}

impl Copter {
    pub fn new() -> Self {
        Copter {
            auto: Auto::default(),
            landed: true,
            idle_s: 0.0,
            touch_s: 0.0,
            guided: None,
            rtl: Rtl::Climb,
            hold_alt: 0.0,
            delay_s: None,
            vel: (0.0, 0.0),
            k: Tune {
                wp_spd: 10.0,
                spd_up: 2.5,
                spd_dn: 1.5,
                wp_acc: 2.5,
                wp_radius: 2.0,
                rtl_alt: 15.0,
                rtl_loit_s: 5.0,
                land_spd: 0.5,
                land_alt_low: 10.0,
                pilot_dn: 2.5,
                disarm_delay: 10.0,
            },
            mode_seen: "STABILIZE",
        }
    }
}

/// Mode name as ArduCopter's Mode::name() prints it in its texts.
fn display(name: &str) -> &str {
    match name {
        "STABILIZE" => "Stabilize",
        "ACRO" => "Acro",
        "ALT_HOLD" => "Altitude Hold",
        "AUTO" => "Auto",
        "GUIDED" => "Guided",
        "LOITER" => "Loiter",
        "CIRCLE" => "Circle",
        "LAND" => "Land",
        "DRIFT" => "Drift",
        "SPORT" => "Sport",
        "FLIP" => "Flip",
        "AUTOTUNE" => "Autotune",
        "POSHOLD" => "Position Hold",
        "BRAKE" => "Brake",
        "THROW" => "Throw",
        "AVOID_ADSB" => "Avoid ADSB",
        "GUIDED_NOGPS" => "Guided No GPS",
        "SMART_RTL" => "Smart RTL",
        "FLOWHOLD" => "Flow Hold",
        "FOLLOW" => "Follow",
        "ZIGZAG" => "ZigZag",
        "SYSTEMID" => "SystemID",
        "AUTOROTATE" => "Autorotate",
        "AUTO_RTL" => "Auto RTL",
        "TURTLE" => "Turtle",
        n => n, // RTL
    }
}

/// Modes whose climb rate (or throttle) is the pilot's stick. There is no RC transmitter in
/// this sim and SITL's throttle sits at the bottom, so in these modes a copter descends at
/// PILOT_SPD_DN (0: PILOT_SPD_UP) until it lands, then auto-disarms.
fn pilot_throttle(mode: &str) -> bool {
    matches!(
        mode,
        "STABILIZE"
            | "ACRO"
            | "ALT_HOLD"
            | "LOITER"
            | "POSHOLD"
            | "SPORT"
            | "DRIFT"
            | "FLOWHOLD"
            | "CIRCLE"
            | "ZIGZAG"
            | "AUTOTUNE"
            | "FLIP"
            | "SYSTEMID"
            | "TURTLE"
    )
}

/// Modes that disarm as soon as they land.
fn lands_and_disarms(mode: &str) -> bool {
    matches!(mode, "LAND" | "RTL" | "AUTO" | "SMART_RTL" | "AUTO_RTL")
}

fn allows_arming(mode: &str) -> bool {
    match mode {
        "AUTO" => AUTO_ALLOWS_ARMING,
        "RTL" | "LAND" | "BRAKE" | "CIRCLE" | "FLIP" | "AUTOTUNE" | "SMART_RTL" | "SYSTEMID"
        | "AVOID_ADSB" | "FOLLOW" | "AUTO_RTL" => false,
        _ => true,
    }
}

/// Modes that take a MAVLink takeoff; `must_navigate` = the GCS leaves horizontal control
/// to the autopilot (param3 = 0).
fn has_user_takeoff(mode: &str, must_navigate: bool) -> bool {
    match mode {
        "GUIDED" | "LOITER" | "POSHOLD" => true,
        "ALT_HOLD" | "SPORT" | "FLOWHOLD" => !must_navigate,
        _ => false,
    }
}

fn text(out: &mut Vec<Out>, sev: MavSeverity, s: impl Into<String>) {
    out.push(Out::Text(sev, s.into()));
}

/// Metres east and north from `st` to (lat, lon), flat earth.
pub(super) fn offset_m(st: &State, lat: f64, lon: f64) -> (f64, f64) {
    let n = (lat - st.lat) * M_PER_DEG;
    let e = (lon - st.lon) * M_PER_DEG * st.lat.to_radians().cos();
    (e, n)
}

fn first_nav_is_takeoff(m: &Mission) -> bool {
    m.items
        .iter()
        .skip(1)
        .find(|it| super::auto::is_nav(it.command))
        .is_some_and(|it| {
            matches!(
                it.command,
                MavCmd::MAV_CMD_NAV_TAKEOFF | MavCmd::MAV_CMD_NAV_VTOL_TAKEOFF
            )
        })
}

impl Copter {
    fn rel(st: &State) -> f64 {
        st.alt - st.home.2
    }

    /// Switch modes with ArduCopter's checks. Returns the ACK result.
    fn set_mode(&mut self, c: &mut Ctx, number: u32) -> MavResult {
        let name = match modes::COPTER.iter().find(|(k, _)| *k == number) {
            Some((_, n)) if EXISTS.contains(&number) => *n,
            _ => {
                text(
                    c.out,
                    MavSeverity::MAV_SEVERITY_WARNING,
                    format!("No such mode {number}"),
                );
                return MavResult::MAV_RESULT_FAILED;
            }
        };
        if name == c.st.mode {
            return MavResult::MAV_RESULT_ACCEPTED;
        }
        let fail = |out: &mut Vec<Out>, why: &str| {
            text(
                out,
                MavSeverity::MAV_SEVERITY_WARNING,
                format!("Mode change to {} failed: {why}", display(name)),
            );
            MavResult::MAV_RESULT_FAILED
        };
        if name == "AUTO_RTL" {
            return fail(c.out, "no landing sequence"); // no DO_LAND_START in our missions
        }
        // disarmed, every mode is allowed (the arming checks catch the rest)
        if c.st.armed && name == "AUTO" {
            if !Auto::present(c.mission) {
                return fail(c.out, "init failed");
            }
            if self.landed && !first_nav_is_takeoff(c.mission) {
                text(
                    c.out,
                    MavSeverity::MAV_SEVERITY_CRITICAL,
                    "Auto: Missing Takeoff Cmd",
                );
                return fail(c.out, "init failed");
            }
        }
        self.enter(c.st, c.mission, c.out, name);
        MavResult::MAV_RESULT_ACCEPTED
    }

    fn enter(
        &mut self,
        st: &mut State,
        mission: &mut Mission,
        out: &mut Vec<Out>,
        name: &'static str,
    ) {
        st.mode = name;
        st.hold_point = (st.lat, st.lon);
        self.on_mode(st, mission, out);
    }

    /// Entering `st.mode`, whoever set it (a command, a failsafe).
    fn on_mode(&mut self, st: &mut State, mission: &mut Mission, out: &mut Vec<Out>) {
        self.mode_seen = st.mode;
        self.hold_alt = st.alt;
        self.guided = None;
        self.delay_s = None;
        self.rtl = Rtl::Climb;
        if st.mode == "AUTO" {
            self.auto.enter(mission, out);
        }
    }

    fn disarm(&mut self, st: &mut State, mission: &mut Mission, out: &mut Vec<Out>) {
        st.armed = false;
        self.landed = true;
        self.idle_s = 0.0;
        self.guided = None;
        text(out, MavSeverity::MAV_SEVERITY_INFO, "Disarming motors");
        self.auto.reset(mission, out);
        if st.mode == "AUTO" && Auto::present(mission) {
            // still in AUTO: the reset mission starts over at item 1, disarmed
            self.auto.enter(mission, out);
            self.auto.current(st, mission, out);
        }
    }

    fn arm_disarm(&mut self, c: &mut Ctx, m: &Command) -> MavResult {
        if m.p[0] == 1.0 {
            if c.st.armed {
                return MavResult::MAV_RESULT_ACCEPTED;
            }
            if !allows_arming(c.st.mode) {
                text(
                    c.out,
                    MavSeverity::MAV_SEVERITY_CRITICAL,
                    format!("Arm: {} mode not armable", display(c.st.mode)),
                );
                return MavResult::MAV_RESULT_FAILED;
            }
            c.st.armed = true;
            self.landed = Self::rel(c.st) < 0.5;
            if self.landed {
                // ArduPilot sets home to where it arms (and says so: HOME_POSITION)
                c.st.home = (c.st.lat, c.st.lon, c.st.alt);
                c.mission.new_home = Some(c.st.home);
            }
            self.idle_s = 0.0;
            self.hold_alt = c.st.alt;
            text(c.out, MavSeverity::MAV_SEVERITY_INFO, "Arming motors");
            return MavResult::MAV_RESULT_ACCEPTED;
        }
        if m.p[0] == 0.0 {
            if !c.st.armed {
                return MavResult::MAV_RESULT_ACCEPTED;
            }
            if !self.landed && m.p[1] != FORCE_DISARM {
                return MavResult::MAV_RESULT_FAILED; // "do not allow disarm via mavlink if we think we are flying"
            }
            self.disarm(c.st, c.mission, c.out);
            return MavResult::MAV_RESULT_ACCEPTED;
        }
        MavResult::MAV_RESULT_UNSUPPORTED
    }

    fn takeoff(&mut self, c: &mut Ctx, m: &Command) -> MavResult {
        let alt = f64::from(m.z);
        if !c.st.armed
            || !self.landed
            || !has_user_takeoff(c.st.mode, m.p[2] == 0.0)
            || alt <= Self::rel(c.st)
        {
            return MavResult::MAV_RESULT_FAILED;
        }
        self.guided = Some((c.st.lat, c.st.lon, c.st.home.2 + alt));
        self.hold_alt = c.st.home.2 + alt;
        self.landed = false;
        MavResult::MAV_RESULT_ACCEPTED
    }

    /// What the current mode wants. May advance the mission or change the RTL phase.
    fn goal(&mut self, t: &mut Tick) -> Goal {
        let st = &*t.st;
        let hold = Goal {
            to: Some(st.hold_point),
            stop: true,
            vert: Vert::Alt(self.hold_alt),
        };
        match st.mode {
            "GUIDED" => match self.guided {
                Some((lat, lon, alt)) => Goal {
                    to: Some((lat, lon)),
                    stop: true,
                    vert: Vert::Alt(alt),
                },
                None => hold,
            },
            "LAND" => Goal {
                to: Some(st.hold_point),
                stop: true,
                vert: Vert::Land,
            },
            // BRAKE: a zero velocity target, so it stops where the braking ends (not back at
            // the entry point), at the altitude it had; the stick plays no part
            "BRAKE" => Goal {
                to: None,
                stop: true,
                vert: Vert::Alt(self.hold_alt),
            },
            "RTL" | "SMART_RTL" => self.rtl_goal(st),
            "AUTO" => self.auto_goal(t),
            m if pilot_throttle(m) => match self.guided {
                // a user takeoff climbs to its altitude, then the stick takes over
                Some((lat, lon, alt)) if st.alt < alt - 0.3 => Goal {
                    to: Some((lat, lon)),
                    stop: true,
                    vert: Vert::Alt(alt),
                },
                _ => {
                    self.guided = None;
                    Goal {
                        to: Some(st.hold_point),
                        stop: true,
                        vert: Vert::Descend(self.k.pilot_dn),
                    }
                }
            },
            _ => hold,
        }
    }

    fn rtl_goal(&mut self, st: &State) -> Goal {
        let (hlat, hlon, halt) = st.home;
        let rtl_alt = (halt + self.k.rtl_alt).max(st.alt);
        loop {
            match self.rtl {
                Rtl::Climb => {
                    if st.alt >= halt + self.k.rtl_alt - 0.5 {
                        self.hold_alt = rtl_alt;
                        self.rtl = Rtl::Return;
                        continue;
                    }
                    return Goal {
                        to: Some(st.hold_point),
                        stop: true,
                        vert: Vert::Alt(halt + self.k.rtl_alt),
                    };
                }
                Rtl::Return => {
                    let (e, n) = offset_m(st, hlat, hlon);
                    if e.hypot(n) < self.k.wp_radius && st.speed < 0.5 {
                        self.rtl = Rtl::Loiter(self.k.rtl_loit_s);
                        continue;
                    }
                    return Goal {
                        to: Some((hlat, hlon)),
                        stop: true,
                        vert: Vert::Alt(self.hold_alt),
                    };
                }
                Rtl::Loiter(left) => {
                    if left <= 0.0 {
                        self.rtl = Rtl::Land;
                        continue;
                    }
                    return Goal {
                        to: Some((hlat, hlon)),
                        stop: true,
                        vert: Vert::Alt(self.hold_alt),
                    };
                }
                Rtl::Land => {
                    return Goal {
                        to: Some((hlat, hlon)),
                        stop: true,
                        vert: Vert::Land,
                    };
                }
            }
        }
    }

    fn auto_goal(&mut self, t: &mut Tick) -> Goal {
        let st = &*t.st;
        let hold = Goal {
            to: Some(st.hold_point),
            stop: true,
            vert: Vert::Alt(self.hold_alt),
        };
        let Some(nav) = self.auto.current(st, t.mission, t.out) else {
            return hold; // mission complete: hover where it ended
        };
        let here = (st.lat, st.lon);
        match nav.command {
            MavCmd::MAV_CMD_NAV_TAKEOFF | MavCmd::MAV_CMD_NAV_VTOL_TAKEOFF => {
                let alt = st.home.2 + f64::from(nav.item.z);
                if st.alt >= alt - 0.3 {
                    self.hold_alt = st.alt.max(alt);
                    self.auto.done(t.mission, t.out, None);
                    t.st.hold_point = here;
                    return self.auto_goal(t);
                }
                self.landed = false;
                Goal {
                    to: Some(st.hold_point),
                    stop: true,
                    vert: Vert::Alt(alt),
                }
            }
            MavCmd::MAV_CMD_NAV_LAND | MavCmd::MAV_CMD_NAV_VTOL_LAND => {
                let to = nav.pos.unwrap_or(st.hold_point);
                let (e, n) = offset_m(st, to.0, to.1);
                let over = e.hypot(n) < self.k.wp_radius && st.speed < 0.5;
                Goal {
                    to: Some(to),
                    stop: true,
                    vert: if over {
                        Vert::Land
                    } else {
                        Vert::Alt(self.hold_alt)
                    },
                }
            }
            MavCmd::MAV_CMD_NAV_RETURN_TO_LAUNCH => self.rtl_goal(st),
            MavCmd::MAV_CMD_NAV_LOITER_UNLIM => Goal {
                to: Some(nav.pos.unwrap_or(st.hold_point)),
                stop: true,
                vert: Vert::Alt(self.nav_alt(st, &nav)),
            },
            _ => self.waypoint(t, nav),
        }
    }

    fn nav_alt(&self, st: &State, nav: &Nav) -> f64 {
        if nav.item.z == 0.0 { st.alt } else { nav.alt }
    }

    /// Waypoint-like items (WP, LOITER_TIME, LOITER_TURNS, ...): go there; done on arrival
    /// (after the item's hold time).
    fn waypoint(&mut self, t: &mut Tick, nav: Nav) -> Goal {
        let st = &*t.st;
        let to = nav.pos.unwrap_or(st.hold_point);
        let alt = self.nav_alt(st, &nav);
        let (e, n) = offset_m(st, to.0, to.1);
        let next_is_wp = t
            .mission
            .items
            .get(usize::from(nav.seq) + 1)
            .is_some_and(|it| it.command == MavCmd::MAV_CMD_NAV_WAYPOINT);
        let hold_s = match nav.command {
            MavCmd::MAV_CMD_NAV_WAYPOINT => f64::from(nav.item.param1).max(0.0),
            MavCmd::MAV_CMD_NAV_LOITER_TIME => f64::from(nav.item.param1).max(0.0),
            MavCmd::MAV_CMD_NAV_LOITER_TURNS => 30.0 * f64::from(nav.item.param1).max(0.0),
            _ => 0.0,
        };
        let stop = hold_s > 0.0 || !next_is_wp;
        let reach = if stop {
            self.k.wp_radius
        } else {
            self.k.wp_radius.max(st.speed * 1.5)
        };
        let arrived = e.hypot(n) < reach && (st.alt - alt).abs() < 1.0;
        if arrived {
            let left = self.delay_s.get_or_insert(hold_s);
            *left -= t.dt;
            if *left <= 0.0 {
                self.delay_s = None;
                self.hold_alt = alt;
                t.st.hold_point = to;
                let text = format!("Reached command #{}", nav.seq);
                self.auto.done(t.mission, t.out, Some(text));
            }
        }
        Goal {
            to: Some(to),
            stop,
            vert: Vert::Alt(alt),
        }
    }

    /// Velocity toward the goal within WP_SPD / WP_ACC, climb and descent limits.
    fn fly(&mut self, t: &mut Tick, g: Goal) -> bool {
        let dt = t.dt;
        let k = self.k;
        let st = &mut *t.st;
        let to = if self.landed { None } else { g.to };
        let acc = if st.mode == "BRAKE" {
            BRAKE_DECEL
        } else {
            k.wp_acc
        };
        hover(st, &mut self.vel, to, g.stop, k.wp_spd, acc, dt);
        // vertical
        let ground = st.home.2;
        let touched = match g.vert {
            Vert::Alt(alt) => {
                let alt = alt.max(ground);
                st.vz = (alt - st.alt).clamp(-k.spd_dn, k.spd_up);
                false
            }
            Vert::Land => {
                let rate = if st.alt - ground > k.land_alt_low {
                    k.spd_dn
                } else {
                    k.land_spd
                };
                st.vz = -rate;
                st.alt + st.vz * dt <= ground
            }
            Vert::Descend(rate) => {
                // the stick goes to the bottom at once; the descent builds up (SITL ~0.8 m/s/s)
                st.vz = (st.vz.min(0.0) - PILOT_ACCEL_Z * dt).max(-rate);
                st.alt + st.vz * dt <= ground
            }
        };
        if self.landed && st.vz < 0.0 {
            st.vz = 0.0;
        }
        st.alt = (st.alt + st.vz * dt).max(ground);
        if st.alt <= ground {
            st.vz = 0.0;
        }
        touched || (matches!(g.vert, Vert::Land | Vert::Descend(_)) && st.alt <= ground)
    }
}

impl Behavior for Copter {
    fn mav_type(&self) -> MavType {
        MavType::MAV_TYPE_QUADROTOR
    }

    fn modes(&self) -> &'static [(u32, &'static str)] {
        modes::COPTER
    }

    fn hold_mode(&self) -> &'static str {
        "LOITER"
    }

    fn spawn(&self, _spec: &Spec, st: &mut State) {
        st.mode = "STABILIZE"; // ArduCopter boots in STABILIZE
    }

    fn airborne(&self, _st: &State) -> bool {
        !self.landed
    }

    fn extra_params(&self, spec: &Spec) -> Vec<(&'static str, MavParamType, f32)> {
        let mut v = vec![("WP_SPD", F32, spec.cruise_mps as f32)];
        v.extend_from_slice(DEFAULTS);
        v
    }

    fn command(&mut self, c: &mut Ctx, m: &Command) -> Option<MavResult> {
        self.k = Tune::read(c.params, c.spec);
        Some(match m.command {
            MavCmd::MAV_CMD_COMPONENT_ARM_DISARM => self.arm_disarm(c, m),
            MavCmd::MAV_CMD_DO_SET_MODE => {
                let custom = MavModeFlag::MAV_MODE_FLAG_CUSTOM_MODE_ENABLED.bits();
                if !m.p[0].is_finite() || (m.p[0] as u32 as u8) & custom == 0 {
                    return Some(MavResult::MAV_RESULT_UNSUPPORTED);
                }
                if !m.p[1].is_finite() || m.p[1] < 0.0 {
                    return Some(MavResult::MAV_RESULT_FAILED);
                }
                self.set_mode(c, m.p[1] as u32)
            }
            MavCmd::MAV_CMD_NAV_TAKEOFF => self.takeoff(c, m),
            MavCmd::MAV_CMD_NAV_LAND => self.set_mode(c, 9),
            MavCmd::MAV_CMD_NAV_RETURN_TO_LAUNCH => self.set_mode(c, 6),
            _ => return None,
        })
    }

    fn step(&mut self, t: &mut Tick) {
        self.k = Tune::read(t.params, t.spec);
        if t.st.mode != self.mode_seen {
            self.on_mode(t.st, t.mission, t.out);
        }
        if !t.st.armed {
            self.vel = (0.0, 0.0);
            t.st.speed = 0.0;
            t.st.ve = 0.0;
            t.st.vn = 0.0;
            self.landed = super::auto::fall(t.st, t.dt);
            return;
        }
        if let Rtl::Loiter(left) = self.rtl {
            self.rtl = Rtl::Loiter(left - t.dt);
        }
        let goal = self.goal(t);
        let climbing = matches!(goal.vert, Vert::Alt(a) if a > t.st.home.2 + 0.3);
        if self.landed && climbing {
            self.landed = false; // takeoff
        }
        let touched = self.fly(t, goal);
        if self.landed {
            // armed on the ground and not taking off: auto-disarm after DISARM_DELAY
            self.idle_s += t.dt;
            if self.k.disarm_delay > 0.0 && self.idle_s >= self.k.disarm_delay {
                self.disarm(t.st, t.mission, t.out);
            }
            return;
        }
        self.idle_s = 0.0;
        if touched {
            self.touch_s += t.dt;
            if self.touch_s >= LAND_DETECT_S {
                self.touch_s = 0.0;
                self.landed = true;
                self.idle_s = 0.0;
                // LAND, RTL and AUTO landings disarm as soon as the land detector trips; the
                // pilot modes wait for DISARM_DELAY
                if lands_and_disarms(t.st.mode) {
                    self.disarm(t.st, t.mission, t.out);
                }
            }
        } else {
            self.touch_s = 0.0;
        }
    }
}

/// Multirotor horizontal motion (also a QuadPlane on its lift motors): a velocity vector
/// toward `to` within `speed`, changed at most `acc` m/s/s, slowing to stop there if `stop`.
/// `to` None: brake to a stop.
pub(super) fn hover(
    st: &mut State,
    vel: &mut (f64, f64),
    to: Option<(f64, f64)>,
    stop: bool,
    speed: f64,
    acc: f64,
    dt: f64,
) {
    let (mut want_e, mut want_n) = (0.0, 0.0);
    if let Some((lat, lon)) = to {
        let (e, n) = offset_m(st, lat, lon);
        let d = e.hypot(n);
        if d > 0.05 {
            let v = if stop {
                speed.min((2.0 * acc * d).sqrt()).min(2.0 * d)
            } else {
                speed
            };
            (want_e, want_n) = (e / d * v, n / d * v);
        }
    }
    let (dve, dvn) = (want_e - vel.0, want_n - vel.1);
    let dv = dve.hypot(dvn);
    let max_dv = acc * dt;
    let k = if dv > max_dv { max_dv / dv } else { 1.0 };
    *vel = (vel.0 + dve * k, vel.1 + dvn * k);
    let (ve, vn) = *vel;
    st.lat += vn * dt / M_PER_DEG;
    st.lon += ve * dt / (M_PER_DEG * st.lat.to_radians().cos());
    st.ve = ve;
    st.vn = vn;
    st.speed = ve.hypot(vn);
    if st.speed > 1.0 {
        let want = ve.atan2(vn).to_degrees();
        let turn = YAW_RATE_DPS * dt;
        let d = wrap180(want - st.heading).clamp(-turn, turn);
        st.heading = (st.heading + d).rem_euclid(360.0);
    }
}

#[cfg(test)]
mod tests {
    use mavlink::dialects::ardupilotmega::{MavCmd, MavResult};

    use super::super::auto::testkit::Sim;

    const OK: Option<MavResult> = Some(MavResult::MAV_RESULT_ACCEPTED);
    const FAILED: Option<MavResult> = Some(MavResult::MAV_RESULT_FAILED);
    const TAKEOFF: MavCmd = MavCmd::MAV_CMD_NAV_TAKEOFF;
    const STABILIZE: u32 = 0;
    const AUTO: u32 = 3;
    const GUIDED: u32 = 4;
    const LOITER: u32 = 5;
    const RTL: u32 = 6;
    const BRAKE: u32 = 17;

    fn copter() -> Sim {
        Sim::new("copter", 12.0, None)
    }

    fn takeoff(s: &mut Sim, alt: f32) -> Option<MavResult> {
        s.command(TAKEOFF, [0., 0., 0., 0., 0., 0., alt])
    }

    #[test]
    fn boots_in_stabilize_and_refuses_what_arducopter_refuses() {
        let mut s = copter();
        assert_eq!(s.st.mode, "STABILIZE");
        assert_eq!(s.mode(99), FAILED);
        assert!(s.said("No such mode 99"));
        assert_eq!(s.mode(8), FAILED, "POSITION is gone from ArduCopter 4.8");
        assert_eq!(takeoff(&mut s, 10.0), FAILED, "takeoff while disarmed");
        assert_eq!(s.mode(RTL), OK);
        assert_eq!(s.arm(true), FAILED);
        assert!(s.said("Arm: RTL mode not armable"));
        assert_eq!(
            s.mode(AUTO),
            OK,
            "disarmed, AUTO is allowed even without a mission"
        );
        assert_eq!(s.mode(STABILIZE), OK);
        assert_eq!(s.arm(true), OK);
        assert_eq!(
            takeoff(&mut s, 10.0),
            FAILED,
            "no user takeoff in STABILIZE"
        );
    }

    #[test]
    fn guided_takeoff_then_no_disarm_in_flight_and_no_auto_without_mission() {
        let mut s = copter();
        assert_eq!(s.mode(GUIDED), OK);
        assert_eq!(s.arm(true), OK);
        assert_eq!(takeoff(&mut s, 20.0), OK);
        let t = s.until(30.0, |s| s.rel_alt() > 19.5).expect("climbs");
        assert!(
            (7.0..14.0).contains(&t),
            "20 m at WP_SPD_UP 2.5 m/s took {t}"
        );
        assert!(s.b.airborne(&s.st));
        assert_eq!(takeoff(&mut s, 30.0), FAILED, "already flying");
        assert_eq!(s.arm(false), FAILED, "no disarm in flight");
        assert_eq!(s.mode(AUTO), FAILED);
        assert!(s.said("Mode change to Auto failed: init failed"));
        s.run(5.0);
        assert!((s.rel_alt() - 20.0).abs() < 0.5, "GUIDED holds altitude");
    }

    #[test]
    fn idle_armed_auto_disarms_after_disarm_delay() {
        let mut s = copter();
        assert_eq!(s.arm(true), OK);
        s.run(9.5);
        assert!(s.st.armed);
        s.run(1.0);
        assert!(!s.st.armed);
        assert_eq!(s.said, ["Arming motors", "Disarming motors"]);
    }

    #[test]
    fn loiter_with_the_throttle_low_descends_lands_and_disarms() {
        let mut s = copter();
        s.mode(GUIDED);
        s.arm(true);
        takeoff(&mut s, 10.0);
        s.until(20.0, |s| s.rel_alt() > 9.5).expect("climbs");
        assert_eq!(s.mode(LOITER), OK);
        s.until(10.0, |s| s.rel_alt() < 0.01).expect("descends");
        assert!(s.st.armed);
        let t = s.until(15.0, |s| !s.st.armed).expect("auto-disarms");
        assert!((9.0..12.5).contains(&t), "{t}");
    }

    #[test]
    fn brake_stops_where_it_is_and_holds_altitude_with_no_throttle() {
        let mut s = copter();
        s.mission(&[
            (TAKEOFF, 0.0, 0.0, 20.0),
            (MavCmd::MAV_CMD_NAV_WAYPOINT, 400.0, 0.0, 20.0),
        ]);
        s.arm(true);
        s.mode(AUTO);
        s.until(60.0, |s| s.st.speed > 5.0 && s.rel_alt() > 19.0)
            .expect("under way");
        assert_eq!(s.mode(BRAKE), OK);
        s.until(10.0, |s| s.st.speed < 0.1).expect("stops");
        let (lat, lon, alt) = (s.st.lat, s.st.lon, s.st.alt);
        assert!(
            s.dist_to(400.0, 0.0) > 20.0,
            "it stopped short of the waypoint"
        );
        s.run(60.0);
        assert!(s.st.armed, "BRAKE never lands or disarms");
        assert_eq!((s.st.lat, s.st.lon, s.st.alt), (lat, lon, alt));
        assert_eq!(s.st.mode, "BRAKE");
    }

    #[test]
    fn mission_from_the_ground_takes_off_flies_and_lands() {
        let mut s = copter();
        s.mission(&[
            (TAKEOFF, 0.0, 0.0, 15.0),
            (MavCmd::MAV_CMD_NAV_WAYPOINT, -60.0, 60.0, 15.0),
            (MavCmd::MAV_CMD_NAV_LAND, -60.0, 60.0, 0.0),
        ]);
        assert_eq!(s.mode(LOITER), OK);
        s.mission.set_current(1);
        assert_eq!(s.arm(true), OK);
        assert_eq!(s.mode(AUTO), OK);
        s.until(120.0, |s| !s.st.armed).expect("lands and disarms");
        assert!(s.dist_to(-60.0, 60.0) < 2.0);
        assert_eq!(s.st.mode, "AUTO");
        assert_eq!(s.reached, [1, 2]);
        let want = [
            "Arming motors",
            "Mission: 1 Takeoff",
            "Mission: 2 WP",
            "Reached command #2",
            "Mission: 3 Land",
            "Disarming motors",
            "Mission: 1 Takeoff",
        ];
        assert_eq!(s.said, want);
    }

    #[test]
    fn auto_from_the_ground_needs_a_takeoff_first() {
        let mut s = copter();
        s.mission(&[(MavCmd::MAV_CMD_NAV_WAYPOINT, 100.0, 0.0, 20.0)]);
        s.arm(true);
        assert_eq!(s.mode(AUTO), FAILED);
        assert!(s.said("Auto: Missing Takeoff Cmd"));
    }

    #[test]
    fn rtl_climbs_returns_loiters_and_lands_at_home() {
        let mut s = copter();
        s.mission(&[
            (TAKEOFF, 0.0, 0.0, 10.0),
            (MavCmd::MAV_CMD_NAV_WAYPOINT, 150.0, 0.0, 10.0),
        ]);
        s.arm(true);
        s.mode(AUTO);
        s.until(60.0, |s| s.reached.contains(&2))
            .expect("flies the mission");
        assert_eq!(s.mode(RTL), OK);
        s.run(3.0);
        assert!(s.rel_alt() > 10.5, "climbs toward RTL_ALT_M first");
        let t = s.until(150.0, |s| !s.st.armed).expect("lands and disarms");
        assert!(
            t > 40.0,
            "15 m down at 1.5 then 0.5 m/s, after 5 s overhead: {t}"
        );
        assert!(s.dist_to(0.0, 0.0) < 2.0);
        assert_eq!(s.st.mode, "RTL");
        assert_eq!(s.mission.seq, 0, "disarming resets the mission");
    }
}
