//! QuadPlane (ArduPlane 4.8 with Q_ENABLE 1): takes off and lands on its lift motors,
//! transitions to wing-borne flight for cruise, back-transitions to land.
//!
//! What the GCS sees follows ArduPlane as SITL shows it (sim/tests/standins_vs_sitl_air.py):
//! MAV_TYPE_FIXED_WING (a plain QuadPlane says it is a plane; Q_ENABLE = 1 tells them apart),
//! Plane mode numbers with the Q modes, "Throttle armed" / "Throttle disarmed", no disarming
//! in flight, GUIDED takeoff (MAV_CMD_NAV_TAKEOFF; NAV_VTOL_TAKEOFF as a command is
//! unsupported), NAV_VTOL_TAKEOFF / NAV_VTOL_LAND in missions, the transition texts
//! ("Transition started airspeed", "... airspeed reached", "Transition done"), the VTOL
//! landing sequence (approach, airbrake, position1, position2, descend, final, "Land
//! complete"), RTL as Q_RTL_MODE 0 (wing-borne home, loiter at RTL_ALTITUDE), QRTL and QLAND,
//! and "Mission complete, changing mode to RTL" when a mission runs out in the air.
//! Movement is simple: hover like a copter (Q_WP_SPD, Q_WP_ACC), fly like the plane
//! stand-in (catalog cruise, turn rate, TECS climb limits, a glide slope to the next altitude).

use mavlink::dialects::ardupilotmega::{
    MavCmd, MavModeFlag, MavParamType, MavResult, MavSeverity, MavType,
};

use super::auto::Auto;
use super::copter::{hover, offset_m};
use super::{Behavior, Command, Ctx, Tick, modes};
use crate::config::Spec;
use crate::geo::{bearing_deg, distance_m};
use crate::kinematics::{Out, State, Want, integrate, turn_and_accelerate};
use crate::mission::Mission;
use crate::params::Params;

// ArduPlane 4.8 QuadPlane parameters this behavior flies by, with their defaults (the SITL
// quadplane model's where it sets them; extra_params puts them in the parameter store)
const DEFAULTS: &[(&str, MavParamType, f32)] = &[
    ("Q_WP_SPD", F32, 10.0),
    ("Q_WP_SPD_UP", F32, 2.5),
    ("Q_WP_SPD_DN", F32, 1.5),
    ("Q_WP_ACC", F32, 2.5),
    ("Q_LAND_FINAL_ALT", F32, 6.0),
    ("Q_LAND_FINAL_SPD", F32, 0.5),
    ("Q_RTL_ALT", F32, 15.0),
    ("Q_RTL_MODE", I8, 0.0),
    ("Q_TRANSITION_MS", I16, 5000.0),
    ("Q_TRANS_DECEL", F32, 2.0),
    ("Q_ASSIST_SPEED", F32, 18.0),
    // the SITL quadplane model's RTL_ALTITUDE (ArduPlane's own default is 100)
    ("RTL_ALTITUDE", F32, 20.0),
];
const F32: MavParamType = MavParamType::MAV_PARAM_TYPE_REAL32;
const I16: MavParamType = MavParamType::MAV_PARAM_TYPE_INT16;
const I8: MavParamType = MavParamType::MAV_PARAM_TYPE_INT8;

/// The parameters as this tick reads them.
#[derive(Debug, Clone, Copy)]
struct Tune {
    q_wp_spd: f64,
    q_up: f64,
    q_dn: f64,
    q_acc: f64,
    land_final_alt: f64,
    land_final_spd: f64,
    q_rtl_alt: f64,
    transition_s: f64,
    trans_decel: f64,
    assist: f64,
    rtl_altitude: f64,
    loiter_rad: f64,
    wp_radius: f64,
}

impl Tune {
    fn read(p: &Params) -> Tune {
        let d = |name: &str| {
            let fallback = DEFAULTS
                .iter()
                .find(|(n, _, _)| *n == name)
                .map_or(0.0, |x| x.2);
            p.f(name, f64::from(fallback))
        };
        Tune {
            q_wp_spd: d("Q_WP_SPD").max(0.5),
            q_up: d("Q_WP_SPD_UP").max(0.1),
            q_dn: d("Q_WP_SPD_DN").max(0.1),
            q_acc: d("Q_WP_ACC").max(0.1),
            land_final_alt: d("Q_LAND_FINAL_ALT"),
            land_final_spd: d("Q_LAND_FINAL_SPD").max(0.1),
            q_rtl_alt: d("Q_RTL_ALT"),
            transition_s: d("Q_TRANSITION_MS") / 1000.0,
            trans_decel: d("Q_TRANS_DECEL").max(0.1),
            assist: d("Q_ASSIST_SPEED"),
            rtl_altitude: d("RTL_ALTITUDE"),
            loiter_rad: p.f("self.k.loiter_rad", 150.0).abs().max(1.0),
            wp_radius: p.f("WP_RADIUS", 90.0).max(1.0),
        }
    }
}

/// Q_PILOT_SPD_DN 0 means Q_PILOT_SPD_UP
const Q_PILOT_SPD_DN: f64 = 2.5;
/// forward acceleration in a transition (lift and forward motors together), from SITL
const TRANSITION_ACC: f64 = 4.2;
const TECS_CLMB_MAX: f64 = 5.0;
const TECS_SINK_MAX: f64 = 5.0;
/// the landing detector: on the ground this long before "Land complete"
const LAND_DETECT_S: f64 = 6.0;
/// MAV_CMD_COMPONENT_ARM_DISARM param2 that forces a disarm
const FORCE_DISARM: f32 = 21196.0;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Approach,
    Airbrake(f64),
    Position1,
    Position2,
    Descend,
    Final,
}

/// A VTOL landing at (lat, lon): QRTL, NAV_VTOL_LAND, or QLAND (`quiet`: no sequence texts).
#[derive(Debug, Clone, Copy)]
struct Landing {
    at: (f64, f64),
    phase: Phase,
    quiet: bool,
    /// wing-borne approach altitude (MSL)
    alt: f64,
}

#[derive(Debug, Clone, Copy)]
struct Transition {
    /// seconds since "Transition airspeed reached"
    timer: Option<f64>,
}

pub struct Vtol {
    auto: Auto,
    landed: bool,
    /// on the lift motors (false: wing-borne)
    hovering: bool,
    transition: Option<Transition>,
    landing: Option<Landing>,
    touch_s: f64,
    hold_alt: f64,
    /// a GUIDED user takeoff's altitude (MSL)
    takeoff: Option<f64>,
    vel: (f64, f64),
    /// seconds since the pre-arm failure was last shown (PREARM_DISPLAY_PERIOD)
    prearm_s: f64,
    k: Tune,
    /// the mode the behavior last saw (a failsafe changes modes behind its back)
    mode_seen: &'static str,
}

/// PREARM_DISPLAY_PERIOD: a failing pre-arm check is reported this often while disarmed.
const PREARM_DISPLAY_S: f64 = 30.0;

impl Default for Vtol {
    fn default() -> Self {
        Self::new()
    }
}

/// What the current mode wants of the airframe.
#[derive(Debug, Clone, Copy)]
enum Goal {
    /// on the lift motors: go to (and stop at) a point at an altitude
    Hover((f64, f64), f64),
    /// on the lift motors: climb straight up
    Climb(f64),
    /// wing-borne toward a point (radius 0) or circling it
    Fly((f64, f64), f64, f64),
    /// a VTOL landing (state in `landing`)
    Land,
    /// on the lift motors with the pilot's throttle at the bottom: down at this rate
    Sink((f64, f64), f64),
    /// on the ground
    Idle,
}

fn text(out: &mut Vec<Out>, s: impl Into<String>) {
    out.push(Out::Text(MavSeverity::MAV_SEVERITY_INFO, s.into()));
}

fn is_q(mode: &str) -> bool {
    mode.starts_with('Q') || mode == "LOITERALTQLAND"
}

/// Q modes flown on the pilot's throttle: with no RC transmitter in this sim (SITL's throttle
/// at the bottom) they descend at Q_PILOT_SPD_DN (0: Q_PILOT_SPD_UP) and sit on the ground.
fn pilot_throttle(mode: &str) -> bool {
    matches!(
        mode,
        "QSTABILIZE" | "QHOVER" | "QLOITER" | "QACRO" | "QAUTOTUNE"
    )
}

/// Mode name as ArduPlane's Mode::name() prints it in its texts.
fn display(name: &str) -> &str {
    match name {
        "MANUAL" => "Manual",
        "CIRCLE" => "Circle",
        "STABILIZE" => "Stabilize",
        "TRAINING" => "Training",
        "ACRO" => "Acro",
        "CRUISE" => "Cruise",
        "AUTOTUNE" => "Autotune",
        "AUTO" => "Auto",
        "LOITER" => "Loiter",
        "TAKEOFF" => "Takeoff",
        "AVOID_ADSB" => "Avoid ADSB",
        "GUIDED" => "Guided",
        "QSTABILIZE" => "QStabilize",
        "QHOVER" => "QHover",
        "QLOITER" => "QLoiter",
        "QLAND" => "QLand",
        "QAUTOTUNE" => "QAutotune",
        "QACRO" => "QAcro",
        "THERMAL" => "Thermal",
        "LOITERALTQLAND" => "Loiter to QLand",
        "AUTOLAND" => "Autoland",
        n => n, // FBWA, FBWB, RTL, QRTL
    }
}

/// Why this mode can't be armed in (Mode::pre_arm_checks), if it can't.
fn arming_refusal(mode: &str, m: &Mission) -> Option<&'static str> {
    match mode {
        "RTL" | "QLAND" | "QRTL" => Some("mode not armable"),
        "AUTO" if !first_nav_is_takeoff(m) => Some("missing takeoff waypoint"),
        _ => None,
    }
}

impl Vtol {
    pub fn new() -> Self {
        Vtol {
            auto: Auto::default(),
            landed: true,
            hovering: true,
            transition: None,
            landing: None,
            touch_s: 0.0,
            hold_alt: 0.0,
            takeoff: None,
            vel: (0.0, 0.0),
            prearm_s: 0.0,
            k: Tune {
                q_wp_spd: 10.0,
                q_up: 2.5,
                q_dn: 1.5,
                q_acc: 2.5,
                land_final_alt: 6.0,
                land_final_spd: 0.5,
                q_rtl_alt: 15.0,
                transition_s: 5.0,
                trans_decel: 2.0,
                assist: 18.0,
                rtl_altitude: 20.0,
                loiter_rad: 150.0,
                wp_radius: 90.0,
            },
            mode_seen: "FBWA",
        }
    }

    fn rel(st: &State) -> f64 {
        st.alt - st.home.2
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
        let name = st.mode;
        self.mode_seen = name;
        self.hold_alt = st.alt;
        self.landing = None;
        self.takeoff = None;
        if is_q(name) && !self.hovering {
            self.enter_hover(st);
        }
        match name {
            "AUTO" if !Auto::present(mission) => {
                // nothing to fly: ArduPlane ends the (empty) mission at once
                text(out, "Mission complete, changing mode to RTL");
                self.enter(st, mission, out, "RTL");
            }
            "AUTO" => self.auto.enter(mission, out),
            "GUIDED" => out.push(Out::ItemReached(0)), // at the guided point already
            "QLAND" | "LOITERALTQLAND" => self.land_here(st, true),
            "QRTL" => self.qrtl(st, out),
            "RTL" => self.hold_alt = st.home.2 + self.k.rtl_altitude,
            _ => {}
        }
    }

    fn enter_hover(&mut self, st: &State) {
        self.hovering = true;
        self.transition = None;
        self.vel = (st.ve, st.vn);
    }

    fn land_here(&mut self, st: &State, quiet: bool) {
        self.landing = Some(Landing {
            at: (st.lat, st.lon),
            phase: Phase::Descend,
            quiet,
            alt: st.alt,
        });
    }

    fn qrtl(&mut self, st: &State, out: &mut Vec<Out>) {
        let at = (st.home.0, st.home.1);
        let alt = (st.home.2 + self.k.q_rtl_alt).max(st.alt.min(st.home.2 + self.k.rtl_altitude));
        self.start_landing(st, out, at, alt);
    }

    /// A VTOL landing at `at`: wing-borne approach first if flying on the wing.
    fn start_landing(&mut self, st: &State, out: &mut Vec<Out>, at: (f64, f64), alt: f64) {
        let phase = if self.hovering && self.transition.is_none() {
            Phase::Position1
        } else {
            let d = distance_m(st.lat, st.lon, at.0, at.1);
            text(out, format!("VTOL approach d={d:.1}"));
            Phase::Approach
        };
        self.landing = Some(Landing {
            at,
            phase,
            quiet: false,
            alt,
        });
    }

    fn set_mode(&mut self, c: &mut Ctx, number: u32) -> MavResult {
        let name = match modes::PLANE.iter().find(|(k, _)| *k == number) {
            Some((_, n)) if *n != "INITIALISING" => *n,
            _ => {
                c.out.push(Out::Text(
                    MavSeverity::MAV_SEVERITY_WARNING,
                    format!("No such mode {number}"),
                ));
                return MavResult::MAV_RESULT_FAILED;
            }
        };
        if name == c.st.mode {
            return MavResult::MAV_RESULT_ACCEPTED;
        }
        self.enter(c.st, c.mission, c.out, name);
        MavResult::MAV_RESULT_ACCEPTED
    }

    fn arm_disarm(&mut self, c: &mut Ctx, m: &Command) -> MavResult {
        if m.p[0] == 1.0 {
            if c.st.armed {
                return MavResult::MAV_RESULT_ACCEPTED;
            }
            if let Some(why) = arming_refusal(c.st.mode, c.mission) {
                c.out.push(Out::Text(
                    MavSeverity::MAV_SEVERITY_CRITICAL,
                    format!("Arm: {} {why}", display(c.st.mode)),
                ));
                return MavResult::MAV_RESULT_FAILED;
            }
            c.st.armed = true;
            self.landed = Self::rel(c.st) < 0.5;
            if self.landed {
                // ArduPilot sets home to where it arms (and says so: HOME_POSITION)
                c.st.home = (c.st.lat, c.st.lon, c.st.alt);
                c.mission.new_home = Some(c.st.home);
            }
            self.hovering = true;
            self.hold_alt = c.st.alt;
            text(c.out, "Throttle armed");
            return MavResult::MAV_RESULT_ACCEPTED;
        }
        if m.p[0] == 0.0 {
            if !c.st.armed {
                return MavResult::MAV_RESULT_ACCEPTED;
            }
            if !self.landed && m.p[1] != FORCE_DISARM {
                return MavResult::MAV_RESULT_FAILED;
            }
            self.disarm(c.st, c.mission, c.out);
            return MavResult::MAV_RESULT_ACCEPTED;
        }
        MavResult::MAV_RESULT_UNSUPPORTED
    }

    fn disarm(&mut self, st: &mut State, mission: &mut Mission, out: &mut Vec<Out>) {
        st.armed = false;
        self.landed = true;
        self.hovering = true;
        self.transition = None;
        self.landing = None;
        self.takeoff = None;
        text(out, "Throttle disarmed");
        self.auto.reset(mission, out);
    }

    fn user_takeoff(&mut self, c: &mut Ctx, m: &Command) -> MavResult {
        let refusal = if c.st.mode != "GUIDED" {
            Some("User Takeoff only in GUIDED mode")
        } else if !c.st.armed {
            Some("Must be armed for takeoff")
        } else if !self.landed {
            Some("Already flying - no takeoff")
        } else {
            None
        };
        if let Some(why) = refusal {
            text(c.out, why);
            return MavResult::MAV_RESULT_FAILED;
        }
        let alt = c.st.home.2 + f64::from(m.z);
        self.takeoff = Some(alt);
        self.hold_alt = alt;
        self.landed = false;
        text(c.out, "VTOL Position1 d=0.0");
        MavResult::MAV_RESULT_ACCEPTED
    }

    fn goal(&mut self, t: &mut Tick) -> Goal {
        if self.landing.is_some() {
            return Goal::Land;
        }
        let st = &*t.st;
        let hold = (st.hold_point, self.hold_alt);
        match st.mode {
            "AUTO" => self.auto_goal(t),
            "GUIDED" => match self.takeoff {
                Some(alt) => Goal::Hover(st.hold_point, alt),
                None if self.hovering => Goal::Hover(hold.0, hold.1),
                None => Goal::Fly(hold.0, self.k.loiter_rad, hold.1),
            },
            "RTL" => Goal::Fly((st.home.0, st.home.1), self.k.loiter_rad, self.hold_alt),
            m if pilot_throttle(m) && !self.landed => Goal::Sink(hold.0, Q_PILOT_SPD_DN),
            m if is_q(m) => Goal::Hover(hold.0, hold.1),
            // LOITER, CIRCLE and the manual modes (no RC in this sim): circle the hold point
            _ if self.landed => Goal::Idle,
            _ => Goal::Fly(hold.0, self.k.loiter_rad, hold.1),
        }
    }

    fn auto_goal(&mut self, t: &mut Tick) -> Goal {
        let st = &*t.st;
        let Some(nav) = self.auto.current(st, t.mission, t.out) else {
            if self.landed {
                return Goal::Idle;
            }
            // out of mission items in the air
            text(t.out, "Mission complete, changing mode to RTL");
            self.enter(t.st, t.mission, t.out, "RTL");
            return self.goal(t);
        };
        let at = nav.pos.unwrap_or(st.hold_point);
        match nav.command {
            MavCmd::MAV_CMD_NAV_VTOL_TAKEOFF | MavCmd::MAV_CMD_NAV_TAKEOFF => {
                let alt = st.home.2 + f64::from(nav.item.z);
                if st.alt >= alt - 0.5 {
                    self.hold_alt = st.alt;
                    self.auto.done(t.mission, t.out, None);
                    t.st.hold_point = (t.st.lat, t.st.lon);
                    return self.auto_goal(t);
                }
                self.landed = false;
                Goal::Climb(alt)
            }
            MavCmd::MAV_CMD_NAV_VTOL_LAND | MavCmd::MAV_CMD_NAV_LAND => {
                self.start_landing(st, t.out, at, st.alt);
                Goal::Land
            }
            MavCmd::MAV_CMD_NAV_RETURN_TO_LAUNCH => {
                self.hold_alt = st.home.2 + self.k.rtl_altitude;
                Goal::Fly((st.home.0, st.home.1), self.k.loiter_rad, self.hold_alt)
            }
            MavCmd::MAV_CMD_NAV_LOITER_UNLIM => {
                let r = f64::from(nav.item.param3).abs();
                let r = if r > 0.0 { r } else { self.k.loiter_rad };
                Goal::Fly(at, r, nav_alt(st, nav.item.z, nav.alt))
            }
            _ => {
                let d = distance_m(st.lat, st.lon, at.0, at.1);
                if d <= self.k.wp_radius && !self.hovering {
                    let text = format!("Reached waypoint #{} dist {}m", nav.seq, d.round() as i64);
                    self.hold_alt = nav_alt(st, nav.item.z, nav.alt);
                    t.st.hold_point = at;
                    self.auto.done(t.mission, t.out, Some(text));
                    return self.auto_goal(t);
                }
                Goal::Fly(at, 0.0, nav_alt(st, nav.item.z, nav.alt))
            }
        }
    }

    /// Wing-borne (or transitioning) toward `at`, circling it if `radius` > 0.
    fn fly_wing(&mut self, t: &mut Tick, at: (f64, f64), radius: f64, alt: f64) {
        let dt = t.dt;
        let cruise = t.spec.cruise_mps.max(self.k.assist + 1.0);
        let st = &mut *t.st;
        if self.hovering && self.transition.is_none() {
            text(
                t.out,
                format!("Transition started airspeed {:.1}", st.speed),
            );
            self.transition = Some(Transition { timer: None });
        }
        let d = distance_m(st.lat, st.lon, at.0, at.1);
        let btc = bearing_deg(st.lat, st.lon, at.0, at.1);
        let heading = if radius > 0.0 {
            let corr = ((d - radius) / radius.max(1.0) * 90.0).clamp(-90.0, 90.0);
            btc - 90.0 + corr
        } else {
            btc
        };
        let accel = if self.transition.is_some() {
            TRANSITION_ACC
        } else {
            3.0
        };
        let want = Want {
            speed: cruise,
            heading,
            alt,
        };
        turn_and_accelerate(st, &want, dt, t.spec.turn_dps(), accel);
        if let Some(tr) = &mut self.transition {
            match &mut tr.timer {
                None if st.speed > self.k.assist => {
                    text(
                        t.out,
                        format!("Transition airspeed reached {:.1}", st.speed),
                    );
                    tr.timer = Some(0.0);
                }
                Some(s) => {
                    *s += dt;
                    if *s >= self.k.transition_s {
                        text(t.out, "Transition done");
                        self.transition = None;
                        self.hovering = false;
                    }
                }
                None => {}
            }
        }
        // a glide slope to the target altitude, within the TECS limits
        let to_go = (d - radius).max(1.0);
        let vz = if radius == 0.0 || d > radius * 1.5 {
            (alt - st.alt) * st.speed / to_go
        } else {
            (alt - st.alt) * 0.5
        };
        st.vz = vz.clamp(-TECS_SINK_MAX, TECS_CLMB_MAX);
        st.alt += st.vz * dt;
        integrate(st, dt, 0.0, 0.0);
    }

    /// The VTOL landing sequence. Returns whether the airframe is on the ground.
    fn fly_landing(&mut self, t: &mut Tick) -> bool {
        let Some(mut l) = self.landing else {
            return false;
        };
        let dt = t.dt;
        let (e, n) = offset_m(t.st, l.at.0, l.at.1);
        let d = e.hypot(n);
        let h = Self::rel(t.st);
        let mut touched = false;
        match l.phase {
            Phase::Approach => {
                let v = t.st.speed;
                let sd = v * v / (2.0 * self.k.trans_decel) + 2.0 * v;
                if d < sd {
                    text(
                        t.out,
                        format!("VTOL airbrake v={v:.1} d={d:.0} sd={sd:.0} h={h:.1}"),
                    );
                    l.phase = Phase::Airbrake(1.0);
                }
                self.landing = Some(l);
                self.fly_wing(t, l.at, 0.0, l.alt);
                return false;
            }
            Phase::Airbrake(left) => {
                if left <= 0.0 {
                    let v = t.st.speed;
                    let dc = v.min((2.0 * self.k.trans_decel * d).sqrt());
                    text(
                        t.out,
                        format!("VTOL position1 v={v:.1} d={d:.1} h={h:.1} dc={dc:.1}"),
                    );
                    l.phase = Phase::Position1;
                    self.enter_hover(t.st);
                } else {
                    l.phase = Phase::Airbrake(left - dt);
                    self.landing = Some(l);
                    self.fly_wing(t, l.at, 0.0, l.alt);
                    return false;
                }
            }
            Phase::Position1 => {
                if d < 10.0 && t.st.speed < 7.0 {
                    if !l.quiet {
                        let v = t.st.speed;
                        text(
                            t.out,
                            format!("VTOL position2 started v={v:.1} d={d:.1} h={h:.1}"),
                        );
                    }
                    l.phase = Phase::Position2;
                }
            }
            Phase::Position2 => {
                if d < 1.0 && t.st.speed < 0.5 {
                    if !l.quiet {
                        text(t.out, "Land descend started");
                    }
                    l.phase = Phase::Descend;
                }
            }
            Phase::Descend | Phase::Final => {}
        }
        // on the lift motors from here on
        self.hovering = true;
        self.transition = None;
        let decel = if l.phase == Phase::Position1 {
            self.k.trans_decel
        } else {
            self.k.q_acc
        };
        hover(
            t.st,
            &mut self.vel,
            Some(l.at),
            true,
            self.k.q_wp_spd.max(t.st.speed),
            decel,
            dt,
        );
        let st = &mut *t.st;
        let ground = st.home.2;
        st.vz = match l.phase {
            Phase::Descend => {
                if h <= self.k.land_final_alt {
                    if !l.quiet {
                        text(t.out, "Land final started");
                    }
                    l.phase = Phase::Final;
                    -self.k.land_final_spd
                } else {
                    -self.k.q_dn
                }
            }
            Phase::Final => -self.k.land_final_spd,
            _ => (l.alt - st.alt).clamp(-self.k.q_dn, self.k.q_up),
        };
        st.alt += st.vz * dt;
        if st.alt <= ground {
            st.alt = ground;
            st.vz = 0.0;
            touched = true;
        }
        self.landing = Some(l);
        touched
    }
}

fn nav_alt(st: &State, z: f32, alt: f64) -> f64 {
    if z == 0.0 { st.alt } else { alt }
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

impl Behavior for Vtol {
    fn mav_type(&self) -> MavType {
        // QuadPlane::get_mav_type(): a plain quad QuadPlane (no tilt, no tailsitter) says plane
        MavType::MAV_TYPE_FIXED_WING
    }

    fn modes(&self) -> &'static [(u32, &'static str)] {
        modes::PLANE
    }

    fn hold_mode(&self) -> &'static str {
        "LOITER"
    }

    fn spawn(&self, _spec: &Spec, st: &mut State) {
        // Murmur's SITL quadplane boots in FBWA (the model's FLTMODE4, RC switch at mid)
        st.mode = "FBWA";
    }

    fn airborne(&self, _st: &State) -> bool {
        !self.landed
    }

    fn extra_params(&self, _spec: &Spec) -> Vec<(&'static str, MavParamType, f32)> {
        DEFAULTS.to_vec()
    }

    fn command(&mut self, c: &mut Ctx, m: &Command) -> Option<MavResult> {
        self.k = Tune::read(c.params);
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
            MavCmd::MAV_CMD_NAV_TAKEOFF => self.user_takeoff(c, m),
            MavCmd::MAV_CMD_NAV_VTOL_TAKEOFF => MavResult::MAV_RESULT_UNSUPPORTED,
            MavCmd::MAV_CMD_NAV_RETURN_TO_LAUNCH => self.set_mode(c, 11),
            _ => return None,
        })
    }

    fn step(&mut self, t: &mut Tick) {
        self.k = Tune::read(t.params);
        if t.st.mode != self.mode_seen {
            self.on_mode(t.st, t.mission, t.out);
        }
        // AP_Arming::update: the pre-arm checks are shown every PREARM_DISPLAY_PERIOD from boot
        self.prearm_s += t.dt;
        let show = self.prearm_s >= PREARM_DISPLAY_S;
        if show {
            self.prearm_s -= PREARM_DISPLAY_S;
        }
        if !t.st.armed {
            if let Some(why) = arming_refusal(t.st.mode, t.mission)
                && show
            {
                t.out.push(Out::Text(
                    MavSeverity::MAV_SEVERITY_CRITICAL,
                    format!("PreArm: {} {why}", display(t.st.mode)),
                ));
            }
            self.vel = (0.0, 0.0);
            t.st.speed = 0.0;
            t.st.vz = 0.0;
            t.st.ve = 0.0;
            t.st.vn = 0.0;
            return;
        }
        let mut goal = self.goal(t);
        let ground = t.st.home.2;
        if self.landed {
            // armed on the ground: only a climb gets it off (no runway takeoffs here)
            goal = match goal {
                Goal::Climb(alt) | Goal::Hover(_, alt) if alt > ground + 0.3 => goal,
                Goal::Land => goal,
                _ => Goal::Idle,
            };
        }
        let dt = t.dt;
        let touched = match goal {
            Goal::Idle => {
                hover(t.st, &mut self.vel, None, true, 0.0, self.k.q_acc, dt);
                t.st.vz = 0.0;
                false
            }
            Goal::Climb(alt) | Goal::Hover(_, alt) => {
                self.landed = false;
                if !self.hovering {
                    self.enter_hover(t.st);
                }
                let to = match goal {
                    Goal::Hover(p, _) => p,
                    _ => t.st.hold_point,
                };
                hover(
                    t.st,
                    &mut self.vel,
                    Some(to),
                    true,
                    self.k.q_wp_spd,
                    self.k.q_acc,
                    dt,
                );
                let st = &mut *t.st;
                st.vz = (alt.max(st.home.2) - st.alt).clamp(-self.k.q_dn, self.k.q_up);
                st.alt += st.vz * dt;
                false
            }
            Goal::Fly(at, r, alt) => {
                self.landed = false;
                self.fly_wing(t, at, r, alt.max(t.st.home.2 + 10.0));
                false
            }
            Goal::Land => self.fly_landing(t),
            Goal::Sink(at, rate) => {
                if !self.hovering {
                    self.enter_hover(t.st);
                }
                hover(
                    t.st,
                    &mut self.vel,
                    Some(at),
                    true,
                    self.k.q_wp_spd,
                    self.k.q_acc,
                    dt,
                );
                let st = &mut *t.st;
                st.vz = -rate;
                st.alt += st.vz * dt;
                if st.alt <= st.home.2 {
                    st.alt = st.home.2;
                    st.vz = 0.0;
                    self.landed = true; // on the ground, still armed (no auto-disarm in Q modes)
                }
                false
            }
        };
        if touched {
            self.touch_s += dt;
            if self.touch_s >= LAND_DETECT_S {
                self.touch_s = 0.0;
                text(t.out, "Land complete");
                self.disarm(t.st, t.mission, t.out);
            }
        } else {
            self.touch_s = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use mavlink::dialects::ardupilotmega::{MavCmd, MavResult, MavType};

    use super::super::auto::testkit::Sim;

    const OK: Option<MavResult> = Some(MavResult::MAV_RESULT_ACCEPTED);
    const FAILED: Option<MavResult> = Some(MavResult::MAV_RESULT_FAILED);
    const AUTO: u32 = 10;
    const RTL: u32 = 11;
    const GUIDED: u32 = 15;
    const QLOITER: u32 = 19;
    const QLAND: u32 = 20;
    const QRTL: u32 = 21;
    const WP: MavCmd = MavCmd::MAV_CMD_NAV_WAYPOINT;
    const VTOL_TAKEOFF: MavCmd = MavCmd::MAV_CMD_NAV_VTOL_TAKEOFF;

    fn vtol() -> Sim {
        Sim::new("vtol", 24.0, Some(23.0))
    }

    fn takeoff(s: &mut Sim, command: MavCmd, alt: f32) -> Option<MavResult> {
        s.command(command, [0., 0., 0., 0., 0., 0., alt])
    }

    #[test]
    fn says_it_is_a_plane_with_q_enable() {
        let s = vtol();
        assert_eq!(s.b.mav_type(), MavType::MAV_TYPE_FIXED_WING);
        assert_eq!(s.params.get("Q_ENABLE"), Some(1.0));
        assert_eq!(s.st.mode, "FBWA");
    }

    #[test]
    fn refusals() {
        let mut s = vtol();
        assert_eq!(s.mode(99), FAILED);
        assert_eq!(s.mode(16), FAILED, "INITIALISING can't be set");
        assert_eq!(s.mode(AUTO), OK);
        assert_eq!(s.st.mode, "RTL", "an empty mission completes at once");
        assert_eq!(s.arm(true), FAILED);
        assert!(s.said("Arm: RTL mode not armable"));
        s.run(30.5);
        assert!(
            s.said("PreArm: RTL mode not armable"),
            "shown every 30 s while disarmed"
        );
        assert_eq!(s.mode(GUIDED), OK);
        assert_eq!(takeoff(&mut s, MavCmd::MAV_CMD_NAV_TAKEOFF, 10.0), FAILED);
        assert!(s.said("Must be armed for takeoff"));
        assert_eq!(
            takeoff(&mut s, VTOL_TAKEOFF, 10.0),
            Some(MavResult::MAV_RESULT_UNSUPPORTED)
        );
        assert_eq!(s.mode(QLOITER), OK);
        assert_eq!(s.arm(true), OK);
        assert_eq!(takeoff(&mut s, MavCmd::MAV_CMD_NAV_TAKEOFF, 10.0), FAILED);
        assert!(s.said("User Takeoff only in GUIDED mode"));
        s.run(30.0);
        assert!(s.st.armed, "no auto-disarm on the ground in Q modes");
    }

    #[test]
    fn guided_takeoff_hover_then_qland() {
        let mut s = vtol();
        s.mode(GUIDED);
        s.arm(true);
        assert_eq!(takeoff(&mut s, MavCmd::MAV_CMD_NAV_TAKEOFF, 30.0), OK);
        s.until(30.0, |s| s.rel_alt() > 29.0).expect("climbs");
        assert_eq!(s.arm(false), FAILED, "no disarm in flight");
        s.run(5.0);
        assert!(s.dist_to(0.0, 0.0) < 1.0, "hovers over the takeoff point");
        assert_eq!(s.mode(QLAND), OK);
        let t = s.until(60.0, |s| !s.st.armed).expect("lands and disarms");
        assert!((30.0..45.0).contains(&t), "QLAND from 30 m took {t}");
        assert!(s.said("Land complete"));
        assert_eq!(s.said.last().map(String::as_str), Some("Throttle disarmed"));
    }

    #[test]
    fn mission_vtol_takeoff_transition_cruise_and_vtol_landing() {
        let mut s = vtol();
        s.mission(&[
            (VTOL_TAKEOFF, 0.0, 0.0, 40.0),
            (WP, 1500.0, 0.0, 70.0),
            (WP, 1500.0, 1500.0, 70.0),
            (MavCmd::MAV_CMD_NAV_VTOL_LAND, 300.0, 300.0, 0.0),
        ]);
        s.mode(QLOITER);
        assert_eq!(s.arm(true), OK);
        assert_eq!(s.mode(AUTO), OK);
        s.until(600.0, |s| !s.st.armed).expect("lands and disarms");
        assert!(s.dist_to(300.0, 300.0) < 2.0);
        assert_eq!(s.reached, [1, 2, 3]);
        let masked: Vec<String> = s
            .said
            .iter()
            .map(|t| {
                t.split(' ')
                    .map(|w| {
                        let n = w.trim_start_matches('#').trim_end_matches('m');
                        if w.contains('=') || n.parse::<f64>().is_ok() {
                            "#"
                        } else {
                            w
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        let want = [
            "Throttle armed",
            "Mission: # VTOLTakeoff",
            "Mission: # WP",
            "Transition started airspeed #",
            "Transition airspeed reached #",
            "Transition done",
            "Reached waypoint # dist #",
            "Mission: # WP",
            "Reached waypoint # dist #",
            "Mission: # VTOLLand",
            "VTOL approach #",
            "VTOL airbrake # # # #",
            "VTOL position1 # # # #",
            "VTOL position2 started # # #",
            "Land descend started",
            "Land final started",
            "Land complete",
            "Throttle disarmed",
        ];
        assert_eq!(masked, want);
    }

    #[test]
    fn rtl_flies_home_on_the_wing_and_qrtl_lands_there() {
        let mut s = vtol();
        s.mission(&[
            (VTOL_TAKEOFF, 0.0, 0.0, 40.0),
            (MavCmd::MAV_CMD_NAV_LOITER_UNLIM, 1500.0, 0.0, 70.0),
        ]);
        s.arm(true);
        s.mode(AUTO);
        s.until(200.0, |s| s.dist_to(1500.0, 0.0) < 300.0)
            .expect("on station");
        assert_eq!(s.mode(RTL), OK);
        s.until(200.0, |s| s.dist_to(0.0, 0.0) < 250.0)
            .expect("home");
        s.run(60.0);
        assert!(s.st.armed && s.dist_to(0.0, 0.0) < 250.0, "loiters at home");
        assert!((s.rel_alt() - 20.0).abs() < 3.0, "at RTL_ALTITUDE");
        assert_eq!(s.mode(QRTL), OK);
        s.until(200.0, |s| !s.st.armed).expect("lands");
        assert!(s.dist_to(0.0, 0.0) < 2.0);
    }
}
