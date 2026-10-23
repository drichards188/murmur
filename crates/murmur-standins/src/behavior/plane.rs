//! Fixed wing (ArduPlane): can't stop or hover, can't disarm in flight, holds by circling.
//!
//! What the GCS sees follows ArduPlane 4.8 as SITL shows it (sim/tests/
//! standins_vs_sitl_plane_boat_sub.py): "Arm: RTL mode not armable" (Mode::pre_arm_checks),
//! "Q_ENABLE 0" for a Q mode on a plain plane (Plane::set_mode), a user takeoff command
//! failed (it is the QuadPlane's), MISSION_ITEM_REACHED 0 on entering GUIDED (at the guided
//! point already), the auto takeoff's texts (takeoff.cpp: "Armed AUTO", "Triggered AUTO",
//! "Holding course", the level-off, "Takeoff complete at Nm"), "Reached waypoint #N dist Mm",
//! "Mission complete, changing mode to RTL", the slope landing's texts (AP_Landing_Slope:
//! approach start, glide slope, flare), then the plane stays armed on the ground until
//! LAND_DISARMDELAY after it stops believing it flies ("Throttle disarmed", "Auto disarmed",
//! "Distance from LAND point"), and leaving AUTO with the LAND item current restarts the
//! landing sequence at the previous waypoint. The GCS long failsafe is in failsafe.rs.
//!
//! Circles use ArduPlane's parameters: LOITER and LOITER_* items without a radius fly
//! WP_LOITER_RAD; RTL circles home at RTL_RADIUS, or WP_LOITER_RAD when that is 0
//! (ArduPlane::update_navigation, mode_rtl). No circle is tighter than the airframe can turn
//! at cruise (catalog cruise / turn rate), as the L1 controller widens a loiter it can't fly.
//! RTL returns at RTL_ALTITUDE (m above home; -1 = the current altitude). A platform whose
//! catalog entry gives no climb rate climbs at TECS_CLMB_MAX's default.
//!
//! RTL ends in a landing with RTL_AUTOLAND (ModeRTL::navigate): 1 once on the RTL circle
//! within 10 m of the RTL altitude, 2 at once, it looks for a landing sequence, once per RTL,
//! and with a DO_LAND_START in the mission jumps to the nearest and switches to AUTO ("Landing
//! sequence start"), which flies it. ArduPlane with none keeps circling; the bridge's missions
//! carry none, so here the plane flies ArduPlane's own landing at home instead, the AUTOLAND
//! mode (mode_autoland.cpp): a loiter down to AUTOLAND_WP_ALT on a base-leg circle beside the
//! final approach's start (AUTOLAND_WP_DIST from home, back along the landing direction), then,
//! lined up, the slope landing at home, disarming LAND_DISARMDELAY later. That hand-over from
//! RTL to AUTOLAND is the stand-in's: ArduPlane's RTL never makes it. The landing direction is
//! the takeoff course (ArduPlane captures it at an auto takeoff, plus AUTOLAND_DIR_OFF), or for
//! a plane that starts in the air, its heading then. AUTOLAND set by a GCS on the ground is
//! refused ("Must already be flying!"). A landing under way (AUTOLAND, or AUTO on its LAND
//! item) is not broken off by a battery failsafe (failsafe.rs, Plane::handle_battery_failsafe).
//!
//! TKOFF_THR_MINACC above 0 is a hand or bungee launch: ArduPlane's auto takeoff waits for the
//! throw. A stand-in has no launch crew but where it starts (its first takeoff runs as a runway
//! one), so a plane that has flown and come down again refuses to arm ("Arm: landed, no crew
//! for a hand launch"), and one still armed on the ground waits for a throw that never comes.

use mavlink::dialects::ardupilotmega::{MavCmd, MavResult, MavSeverity, MavType};

use super::{Behavior, Command, Ctx, Tick, modes};
use crate::config::Spec;
use crate::geo::{M_PER_DEG, bearing_deg, distance_m, wrap180};
use crate::kinematics::{
    Out, State, Target, TargetKind, climb, guide, integrate, turn_and_accelerate,
};
use crate::mission::{Mission, is_nav};

/// Height above home that counts as flying (ArduPlane's arming/disarm logic in spirit).
const AIRBORNE_M: f64 = 15.0;
/// Glide ratio with no thrust (a flat battery or a dead engine), estimate for long-wing UAVs.
const GLIDE_RATIO: f64 = 15.0;
/// Climb rate when the catalog gives none: ArduPlane's TECS_CLMB_MAX default, m/s.
const CLIMB_DEFAULT_MPS: f64 = 5.0;
/// Deceleration on the ground (brakes, drag), m/s/s.
const GROUND_DECEL_MPS2: f64 = 3.0;
/// TKOFF_THR_DELAY's default (0.2 s): "Triggered AUTO" this long after "Armed AUTO".
const TKOFF_TRIGGER_S: f64 = 0.2;
/// takeoff.cpp holds the takeoff course from this ground speed ("Holding course").
const TKOFF_HOLD_COURSE_MPS: f64 = 5.0;
/// TKOFF_PLIM_SEC's default: the pitch level-off starts this long before the target altitude.
const TKOFF_LEVEL_OFF_S: f64 = 2.0;
/// LAND_FLARE_ALT and LAND_FLARE_SEC defaults: flare at 3 m, or 2 s of sink before the ground.
const LAND_FLARE_ALT_M: f64 = 3.0;
const LAND_FLARE_SEC: f64 = 2.0;
/// Plane::disarm_if_autoland_complete counts LAND_DISARMDELAY from when is_flying() last
/// held; in SITL that is about this long before the flare (the plane SITL disarmed 15 s
/// after "Flare" with the 20 s default).
const FLYING_LAPSES_BEFORE_FLARE_S: f64 = 5.0;
/// RTL_AUTOLAND 1 looks for a landing once the plane is on its RTL circle within this of the
/// RTL altitude (ModeRTL::navigate: an altitude error under 10 m).
const RTL_AUTOLAND_ALT_M: f64 = 10.0;
/// ArduPlane announces a mode change with a heartbeat (Plane::set_mode), so a GCS hears RTL
/// even when RTL_AUTOLAND moves on within a navigation step (a plane already on its RTL
/// circle). A stand-in's only heartbeat is its 1 Hz one: it moves on no sooner than this
/// after entering RTL, three heartbeats later.
const RTL_ANNOUNCED_S: f64 = 3.0;
/// AUTOLAND's loiter to altitude: the altitude counts as reached within 5 m (Plane::
/// update_loiter), and the plane leaves the circle lined up within 10 degrees of its course to
/// home (ModeLoiter::isHeadingLinedUp_cd).
const AUTOLAND_ALT_M: f64 = 5.0;
const AUTOLAND_LINED_UP_DEG: f64 = 10.0;
/// ArduPlane's mode number for AUTOLAND (mode.h).
const AUTOLAND_MODE: u32 = 26;

/// ArduPlane's VTOL modes (Mode::is_vtol_mode): refused with Q_ENABLE 0.
fn is_q_mode(number: u32) -> bool {
    (17..=23).contains(&number)
}

/// `dist` metres from (lat, lon) on `bearing` degrees, on the flat earth `integrate` moves on.
fn offset(lat: f64, lon: f64, bearing: f64, dist: f64) -> (f64, f64) {
    let (n, e) = (
        dist * bearing.to_radians().cos(),
        dist * bearing.to_radians().sin(),
    );
    let lat2 = lat + n / M_PER_DEG;
    (lat2, lon + e / (M_PER_DEG * lat2.to_radians().cos()))
}

/// The AUTOLAND mode's pattern (mode_autoland.cpp), planned on entry: the base-leg circle the
/// plane loiters down on, and whether it has left it for the final approach.
#[derive(Debug, Clone, Copy)]
struct AutoLand {
    centre: (f64, f64),
    radius: f64,
    /// the final approach's start, m above MSL
    alt: f64,
    final_leg: bool,
}

/// Hand or bungee launched (TKOFF_THR_MINACC > 0): an auto takeoff waits for a throw.
fn hand_launched(params: &crate::params::Params) -> bool {
    params.f("TKOFF_THR_MINACC", 0.0) > 0.0
}

/// (loiter radius, RTL radius), m.
fn radii(t: &Tick) -> (f64, f64) {
    let min_r = t.spec.min_turn_radius_m();
    let fallback = t.spec.loiter_radius_m.unwrap_or(150.0);
    let loiter = t
        .params
        .f("WP_LOITER_RAD", fallback)
        .abs()
        .max(min_r)
        .max(1.0);
    let r = t.params.f("RTL_RADIUS", 0.0);
    let rtl = if r == 0.0 { loiter } else { r.abs().max(min_r) };
    (loiter, rtl)
}

fn text(out: &mut Vec<Out>, s: String) {
    out.push(Out::Text(MavSeverity::MAV_SEVERITY_INFO, s));
}

/// An auto takeoff in progress and what has been said about it.
#[derive(Debug, Default)]
struct Takeoff {
    started: f64,
    triggered: bool,
    holding: bool,
    level_off: bool,
}

/// A slope landing in progress and what has been said about it.
#[derive(Debug, Default)]
struct Landing {
    slope_said: bool,
    /// sim time of the flare, once flared
    flared: Option<f64>,
}

/// Modes whose throttle is the pilot's (Mode::does_auto_throttle false).
fn pilot_throttle(mode: &str) -> bool {
    matches!(
        mode,
        "MANUAL" | "STABILIZE" | "ACRO" | "TRAINING" | "FBWA" | "AUTOTUNE"
    )
}

pub struct Plane {
    /// the mode seen last tick (mode changes are made by the shared code)
    mode_seen: &'static str,
    takeoff: Option<Takeoff>,
    landing: Option<Landing>,
    /// sim time RTL was entered, and whether RTL_AUTOLAND has looked for a landing since
    /// (auto_state.checked_for_autoland, cleared by every mode change)
    rtl_since: f64,
    rtl_checked: bool,
    /// the AUTOLAND mode's pattern while in it
    autoland: Option<AutoLand>,
    /// the landing direction AUTOLAND flies (takeoff_state.initial_direction), degrees
    autoland_dir: Option<f64>,
    /// has been in the air since it started
    flown: bool,
}

impl Default for Plane {
    fn default() -> Self {
        Plane::new()
    }
}

impl Plane {
    pub fn new() -> Plane {
        Plane {
            mode_seen: "MANUAL",
            takeoff: None,
            landing: None,
            rtl_since: 0.0,
            rtl_checked: false,
            autoland: None,
            autoland_dir: None,
            flown: false,
        }
    }

    /// The RTL altitude, m above MSL (RTL_ALTITUDE m above home; -1, the current altitude).
    fn rtl_alt(t: &Tick) -> f64 {
        let a = t.params.f("RTL_ALTITUDE", -1.0);
        if a < 0.0 { t.st.alt } else { t.st.home.2 + a }
    }

    fn target(&self, t: &mut Tick) -> Option<Target> {
        let st = &*t.st;
        let cruise = t.spec.cruise_mps;
        let (loiter, rtl_r) = radii(t);
        if st.mode == "AUTO" {
            // AP_Mission runs armed or not (its texts, DO items; an empty mission completes
            // at once and ArduPlane RTLs); only the flying needs arming
            let tgt = t.mission.auto_target(st, cruise, loiter, loiter, t.out);
            return st.armed.then_some(tgt);
        }
        if !st.armed {
            return None;
        }
        if st.mode == "AUTOLAND"
            && let Some(a) = self.autoland
        {
            let tgt = if a.final_leg {
                Target {
                    lat: st.home.0,
                    lon: st.home.1,
                    alt: st.home.2,
                    radius: 0.0,
                    speed: cruise,
                    kind: TargetKind::Land,
                }
            } else {
                Target {
                    lat: a.centre.0,
                    lon: a.centre.1,
                    alt: a.alt,
                    radius: a.radius,
                    speed: cruise,
                    kind: TargetKind::Loiter,
                }
            };
            return Some(tgt);
        }
        let (lat, lon, radius, kind, alt) = if st.mode == "RTL" {
            (
                st.home.0,
                st.home.1,
                rtl_r,
                TargetKind::Rtl,
                Self::rtl_alt(t),
            )
        } else {
            // planes can't stop: every other mode circles the hold point
            let (lat, lon) = st.hold_point;
            (lat, lon, loiter, TargetKind::Hold, st.alt)
        };
        Some(Target {
            lat,
            lon,
            alt,
            radius,
            speed: cruise,
            kind,
        })
    }

    /// No thrust in the air: glide straight on and down until it meets the surface.
    fn glide(&self, t: &mut Tick) {
        let cruise = t.spec.cruise_mps;
        let st = &mut *t.st;
        st.speed += (cruise * 0.8 - st.speed).clamp(-3.0 * t.dt, 3.0 * t.dt);
        st.demand = 0.0;
        st.vz = -st.speed / GLIDE_RATIO;
        st.alt = (st.alt + st.vz * t.dt).max(st.home.2);
        if st.alt <= st.home.2 {
            st.speed = 0.0;
            st.vz = 0.0;
        }
        integrate(st, t.dt, 0.0, 0.0);
    }

    /// A mode change made by the shared code (a command, a failsafe, the mission's end).
    fn on_mode(&mut self, t: &mut Tick, airborne: bool) {
        let from = self.mode_seen;
        self.mode_seen = t.st.mode;
        self.takeoff = None;
        // Mode::enter: checked_for_autoland = false
        self.rtl_checked = false;
        self.rtl_since = t.mission.cfg.now;
        self.autoland = (t.st.mode == "AUTOLAND")
            .then(|| self.plan_autoland(t))
            .flatten();
        if t.st.mode == "GUIDED" {
            // the guided point is where it is: ArduPlane reports it reached at once
            t.out.push(Out::ItemReached(0));
        }
        if from == "AUTO" && t.st.mode != "AUTO" {
            self.landing = None;
            let on_ground_after_landing = !t.st.armed && !airborne && t.mission.landing();
            if on_ground_after_landing && pilot_throttle(t.st.mode) {
                // Mode::enter: landed, disarmed and back on a pilot-throttle mode, the mission
                // is reset to clear the pre-arm; ModeAuto::_exit then finds nothing to restart
                t.mission.set_current(0);
                text(t.out, "In landing sequence: mission reset".into());
                t.out.push(Out::Text(
                    MavSeverity::MAV_SEVERITY_WARNING,
                    "Unable to restart landing sequence".into(),
                ));
            } else {
                restart_landing_sequence(t.mission, t.out);
            }
        }
    }

    /// ModeAutoLand::_enter: the final approach starts AUTOLAND_WP_DIST from home back along
    /// the landing direction, AUTOLAND_WP_ALT above home; the base-leg circle (a third of that
    /// distance, or WP_LOITER_RAD if less, no tighter than the airframe turns) touches the
    /// approach there. ArduPlane puts the circle on the side the plane comes from and flies it
    /// either way; a stand-in circles clockwise only, so it is always on the right of the
    /// approach. None without a landing direction (not yet flown).
    fn plan_autoland(&self, t: &Tick) -> Option<AutoLand> {
        let dir = self.autoland_dir?;
        let home = t.st.home;
        let dist = t.params.f("AUTOLAND_WP_DIST", 400.0).max(0.0);
        let (lat, lon) = offset(home.0, home.1, dir + 180.0, dist);
        let loiter = t.params.f("WP_LOITER_RAD", 150.0).abs();
        let radius = (dist * 0.333)
            .min(loiter)
            .max(t.spec.min_turn_radius_m())
            .max(1.0);
        Some(AutoLand {
            centre: offset(lat, lon, dir + 90.0, radius),
            radius,
            alt: home.2 + t.params.f("AUTOLAND_WP_ALT", 55.0),
            final_leg: false,
        })
    }

    /// ModeRTL::navigate with RTL_AUTOLAND: once per RTL, on the RTL circle at its altitude
    /// (1) or at once (2), jump to the mission's nearest landing sequence in AUTO, or (no
    /// DO_LAND_START in the mission: the stand-in's own step) land at home in AUTOLAND.
    fn rtl_autoland(&mut self, t: &mut Tick) {
        let how = t.params.f("RTL_AUTOLAND", 0.0) as i64;
        if self.rtl_checked
            || !matches!(how, 1 | 2)
            || t.mission.cfg.now - self.rtl_since < RTL_ANNOUNCED_S
        {
            return;
        }
        let st = &*t.st;
        let (_, rtl_r) = radii(t);
        let on_circle = distance_m(st.lat, st.lon, st.home.0, st.home.1) <= rtl_r * 1.1
            && (st.alt - Self::rtl_alt(t)).abs() < RTL_AUTOLAND_ALT_M;
        if how == 1 && !on_circle {
            return;
        }
        self.rtl_checked = true;
        if let Some(seq) = t.mission.landing_sequence_start(st.lat, st.lon) {
            t.mission.set_current(seq);
            t.mission.force_resume = true;
            text(t.out, "Landing sequence start".into());
            t.out.push(Out::SetMode("AUTO"));
        } else if self.autoland_dir.is_some() {
            t.out.push(Out::SetMode("AUTOLAND"));
        }
    }

    /// ModeAutoLand::navigate's loiter stage: down to the approach altitude on the base-leg
    /// circle, then off it once lined up with home (verify_loiter_to_alt, landing_lined_up).
    fn autoland_stage(&mut self, t: &Tick) {
        let st = &*t.st;
        let Some(a) = self.autoland.as_mut().filter(|a| !a.final_leg) else {
            return;
        };
        let on_circle = distance_m(st.lat, st.lon, a.centre.0, a.centre.1) <= a.radius * 1.25;
        let at_alt = (st.alt - a.alt).abs() < AUTOLAND_ALT_M;
        let to_home = bearing_deg(st.lat, st.lon, st.home.0, st.home.1);
        if on_circle && at_alt && wrap180(to_home - st.heading).abs() < AUTOLAND_LINED_UP_DEG {
            a.final_leg = true;
        }
    }

    /// What ArduPlane says through an auto takeoff (takeoff.cpp, commands_logic.cpp).
    fn takeoff_texts(&mut self, t: &mut Tick, tgt: &Target) {
        let now = t.mission.cfg.now;
        let st = &*t.st;
        if self.takeoff.is_none() {
            text(
                t.out,
                "Armed AUTO, xaccel = 0.0 m/s/s, waiting 0.2 sec".into(),
            );
            self.takeoff = Some(Takeoff {
                started: now,
                ..Default::default()
            });
        }
        let Some(tk) = &mut self.takeoff else { return };
        if !tk.triggered && now - tk.started >= TKOFF_TRIGGER_S {
            tk.triggered = true;
            text(t.out, "Triggered AUTO. GPS speed = 0.0".into());
        }
        if !tk.holding && st.speed >= TKOFF_HOLD_COURSE_MPS {
            tk.holding = true;
            let hdg = st.heading.rem_euclid(360.0);
            // ModeAutoLand::check_takeoff_direction: the course plus AUTOLAND_DIR_OFF
            let dir = (hdg + t.params.f("AUTOLAND_DIR_OFF", 0.0)).rem_euclid(360.0);
            self.autoland_dir = Some(dir);
            text(t.out, format!("Autoland direction= {}", dir.round() as i64));
            text(
                t.out,
                format!(
                    "Holding course {} at {:.1}m/s (0.0)",
                    (hdg * 100.0).round() as i64,
                    st.speed
                ),
            );
        }
        let remaining = tgt.alt - st.alt;
        if !tk.level_off
            && st.alt - st.home.2 >= 10.0
            && st.vz > 0.1
            && remaining / st.vz <= TKOFF_LEVEL_OFF_S
        {
            tk.level_off = true;
            text(
                t.out,
                format!("Takeoff level-off starting at {}m", remaining as i64),
            );
        }
    }

    /// What AP_Landing's slope landing says on the way down.
    fn landing_texts(&mut self, t: &mut Tick, tgt: &Target) {
        let st = &*t.st;
        let rel = st.alt - st.home.2;
        let dist = distance_m(st.lat, st.lon, tgt.lat, tgt.lon);
        if self.landing.is_none() {
            text(t.out, format!("Landing approach start at {rel:.1}m"));
            self.landing = Some(Landing::default());
        }
        let Some(l) = &mut self.landing else { return };
        let to = bearing_deg(st.lat, st.lon, tgt.lat, tgt.lon);
        if !l.slope_said && wrap180(to - st.heading).abs() < 30.0 {
            l.slope_said = true;
            let deg = (rel / dist.max(1.0)).atan().to_degrees();
            text(t.out, format!("Landing glide slope {deg:.1} degrees"));
        }
        if l.flared.is_none() && rel <= LAND_FLARE_ALT_M.max(-st.vz * LAND_FLARE_SEC) {
            l.flared = Some(t.mission.cfg.now);
            text(
                t.out,
                format!(
                    "Flare {rel:.1}m sink={:.2} speed={:.1} dist={:.1}",
                    -st.vz, st.speed, dist
                ),
            );
        }
    }

    /// On the ground after an auto-land: the plane stays armed, then disarms LAND_DISARMDELAY
    /// after it last believed it flew (Plane::disarm_if_autoland_complete).
    fn autoland_disarm(&mut self, t: &mut Tick, tgt: &Target) {
        let now = t.mission.cfg.now;
        let delay = t.params.f("LAND_DISARMDELAY", 20.0);
        let Some(flared) = self.landing.as_ref().and_then(|l| l.flared) else {
            return;
        };
        if delay > 0.0 && now - flared >= delay - FLYING_LAPSES_BEFORE_FLARE_S && t.st.armed {
            t.st.armed = false;
            text(t.out, "Throttle disarmed".into());
            text(t.out, "Auto disarmed".into());
            let d = distance_m(t.st.lat, t.st.lon, tgt.lat, tgt.lon);
            text(t.out, format!("Distance from LAND point={d:.2}m"));
        }
    }
}

/// Leaving AUTO while the LAND item is current (ModeAuto::_exit): AP_Landing restarts the
/// landing sequence at the previous waypoint with a location, and says so.
fn restart_landing_sequence(m: &mut Mission, out: &mut Vec<Out>) {
    if m.complete || !m.landing() {
        return;
    }
    let prev = m.items[..usize::from(m.seq)]
        .iter()
        .rposition(|it| is_nav(it.command) && (it.x != 0 || it.y != 0));
    if let Some(prev) = prev.filter(|&p| p > 0) {
        m.set_current(prev as u16);
        out.push(Out::Text(
            MavSeverity::MAV_SEVERITY_NOTICE,
            format!("Restarted landing sequence at waypoint {prev}"),
        ));
    }
}

impl Behavior for Plane {
    fn mav_type(&self) -> MavType {
        MavType::MAV_TYPE_FIXED_WING
    }

    fn modes(&self) -> &'static [(u32, &'static str)] {
        modes::PLANE
    }

    fn hold_mode(&self) -> &'static str {
        "LOITER"
    }

    fn spawn(&self, spec: &Spec, st: &mut State) {
        if st.alt > 50.0 {
            // spawned high: on station, flying, home on the surface below
            st.home.2 = 0.0;
            st.speed = spec.cruise_mps;
            st.armed = true;
            st.mode = self.hold_mode();
        }
    }

    fn airborne(&self, st: &State) -> bool {
        st.alt - st.home.2 > AIRBORNE_M
    }

    fn command(&mut self, c: &mut Ctx, m: &Command) -> Option<MavResult> {
        match m.command {
            // AP_Arming_Plane::mode_checks: RTL's pre-arm check fails (a plain plane's AUTO
            // arms with or without a takeoff item; only a QuadPlane's needs one)
            MavCmd::MAV_CMD_COMPONENT_ARM_DISARM if m.p[0] == 1.0 && !c.st.armed => {
                if c.st.mode == "RTL" {
                    c.out.push(Out::Text(
                        MavSeverity::MAV_SEVERITY_CRITICAL,
                        "Arm: RTL mode not armable".into(),
                    ));
                    return Some(MavResult::MAV_RESULT_FAILED);
                }
                // the stand-in's: down again after flying, a hand-launched plane has no one to
                // throw it (module doc)
                if hand_launched(c.params) && self.flown && !self.airborne(c.st) {
                    c.out.push(Out::Text(
                        MavSeverity::MAV_SEVERITY_CRITICAL,
                        "Arm: landed, no crew for a hand launch".into(),
                    ));
                    return Some(MavResult::MAV_RESULT_FAILED);
                }
                None
            }
            // ModeAutoLand::_enter
            MavCmd::MAV_CMD_DO_SET_MODE
                if (m.p[1].trunc() as i64 as u32) & 0xff == AUTOLAND_MODE
                    && c.st.mode != "AUTOLAND" =>
            {
                let why = if !self.airborne(c.st) {
                    "Must already be flying!"
                } else if self.autoland_dir.is_none() {
                    "Takeoff initial direction not set"
                } else {
                    return None;
                };
                c.out
                    .push(Out::Text(MavSeverity::MAV_SEVERITY_WARNING, why.into()));
                Some(MavResult::MAV_RESULT_FAILED)
            }
            // a user takeoff is the QuadPlane's (GCS_MAVLINK_Plane::
            // handle_command_MAV_CMD_NAV_TAKEOFF): without one it fails, silently
            MavCmd::MAV_CMD_NAV_TAKEOFF => Some(MavResult::MAV_RESULT_FAILED),
            MavCmd::MAV_CMD_DO_SET_MODE if is_q_mode((m.p[1].trunc() as i64 as u32) & 0xff) => {
                c.out.push(Out::Text(
                    MavSeverity::MAV_SEVERITY_INFO,
                    "Q_ENABLE 0".into(),
                ));
                Some(MavResult::MAV_RESULT_FAILED)
            }
            _ => None,
        }
    }

    fn step(&mut self, t: &mut Tick) {
        let airborne = self.airborne(t.st);
        if airborne {
            self.flown = true;
            if self.autoland_dir.is_none() {
                // started in the air: its heading then stands for the course it was launched on
                let off = t.params.f("AUTOLAND_DIR_OFF", 0.0);
                self.autoland_dir = Some((t.st.heading + off).rem_euclid(360.0));
            }
        }
        if t.st.mode != self.mode_seen {
            self.on_mode(t, airborne);
        }
        if !t.st.armed && airborne {
            return self.glide(t);
        }
        if t.st.mode == "RTL" && t.st.armed && airborne {
            self.rtl_autoland(t);
        }
        if t.st.mode == "AUTOLAND" {
            if self.autoland.is_none() && airborne {
                self.autoland = self.plan_autoland(t);
            }
            self.autoland_stage(t);
        }
        let tgt = self.target(t);
        let landing = tgt.is_some_and(|g| g.kind == TargetKind::Land);
        let taking_off = tgt.is_some_and(|g| g.kind == TargetKind::Takeoff);
        // down again with no one to throw it: the auto takeoff waits (auto_takeoff_check)
        let unthrown = taking_off && !airborne && self.flown && hand_launched(t.params);
        if !taking_off || unthrown {
            self.takeoff = None;
        }
        if !landing {
            self.landing = None;
        }
        let cruise = t.spec.cruise_mps;
        // waypoints: the turn distance; a landing: touch down a roll-out short of the point
        let reach = if landing {
            t.st.speed * t.st.speed / (2.0 * GROUND_DECEL_MPS2)
        } else {
            (t.st.speed * 3.0).max(t.params.f("WP_RADIUS", 90.0))
        };
        let mut want = guide(t.st, tgt.as_ref(), reach, false, t.mission, t.out);
        if let Some(g) = tgt.filter(|_| taking_off && !unthrown) {
            self.takeoff_texts(t, &g);
        }
        if let Some(g) = tgt.filter(|_| landing) {
            self.landing_texts(t, &g);
        }
        let on_ground = t.st.alt - t.st.home.2 < 1.0;
        if airborne {
            // a fixed wing keeps flying: no slower than AIRSPEED_MIN (or 0.8 cruise)
            let min = t.params.f("AIRSPEED_MIN", cruise * 0.8).min(cruise);
            want.speed = want.speed.max(min.max(cruise * 0.8));
        } else if on_ground && (!taking_off || unthrown) && !landing {
            // on the ground the throttle is suppressed (Plane::suppress_throttle) until an
            // auto takeoff: armed in any other mode, with no RC, it does not move
            want.speed = 0.0;
        }
        if landing && on_ground {
            // touchdown: roll out; the LAND item stays current, the plane stays armed
            want.speed = 0.0;
            if t.st.speed < 1.0
                && let Some(g) = tgt
            {
                self.autoland_disarm(t, &g);
            }
        }
        turn_and_accelerate(t.st, &want, t.dt, t.spec.turn_dps(), GROUND_DECEL_MPS2);
        let mut max_climb = t.spec.climb_mps.unwrap_or(CLIMB_DEFAULT_MPS);
        if !airborne && !landing && t.st.speed < cruise * 0.6 {
            max_climb = 0.0; // still on the runway
        }
        climb(t.st, &want, t.dt, max_climb);
        integrate(t.st, t.dt, 0.0, 0.0);
    }
}

#[cfg(test)]
mod tests {
    use mavlink::dialects::ardupilotmega::{MavCmd, MavResult};

    use super::super::auto::testkit::{LAT, LON, Sim, offset};
    use super::{AUTOLAND_MODE, RTL_ANNOUNCED_S};

    const FAILED: Option<MavResult> = Some(MavResult::MAV_RESULT_FAILED);
    const WP: MavCmd = MavCmd::MAV_CMD_NAV_WAYPOINT;

    /// A Puma-class plane (20 m/s, the default 10 deg/s turn) in the air at 120 m, `north` and
    /// `east` metres from home on the surface, circling there in LOITER, heading west; with
    /// the Puma's catalog parameters (sim/platforms.toml `standin_params`) when `puma`.
    fn aloft(north: f64, east: f64, puma: bool) -> Sim {
        let mut s = Sim::new("plane", 20.0, None);
        if puma {
            assert!(s.params.set("RTL_AUTOLAND", 1.0).is_ok());
            assert!(s.params.set("TKOFF_THR_MINACC", 15.0).is_ok());
        }
        let (lat, lon) = offset(north, east);
        s.st.lat = lat;
        s.st.lon = lon;
        s.st.alt = 120.0;
        s.st.home = (LAT, LON, 0.0);
        s.st.heading = 270.0;
        s.st.speed = 20.0;
        s.st.armed = true;
        set_mode(&mut s, "LOITER");
        s.run(0.1);
        s
    }

    /// A mode change as the shared code makes it (commands.rs, Vehicle::set_mode).
    fn set_mode(s: &mut Sim, name: &'static str) {
        s.st.mode = name;
        s.st.hold_point = (s.st.lat, s.st.lon);
    }

    #[test]
    fn rtl_autoland_lands_at_home_disarms_and_stays_down() {
        let mut s = aloft(0.0, 2000.0, true);
        set_mode(&mut s, "RTL");
        let t = s.until(400.0, |s| s.st.mode != "RTL").expect("hands over");
        assert!(s.dist_to(0.0, 0.0) < 150.0 * 1.1, "on its RTL circle first");
        assert!(t > 90.0, "flew the 2 km home first ({t:.0} s)");
        assert_eq!(s.st.mode, "AUTOLAND", "no landing sequence in the mission");
        let down = s.until(300.0, |s| !s.st.armed).expect("lands and disarms");
        assert!(down < 200.0, "the pattern took {down:.0} s");
        assert!(
            s.dist_to(0.0, 0.0) < 100.0,
            "down {:.0} m from home",
            s.dist_to(0.0, 0.0)
        );
        assert!(s.rel_alt().abs() < 0.01 && s.st.speed < 0.5);
        for said in [
            "Landing approach start",
            "Flare",
            "Throttle disarmed",
            "Auto disarmed",
        ] {
            assert!(s.said(said), "{said}: {:?}", s.said);
        }
        // AUTOLAND, disarmed, on the ground: nothing moves it
        let at = (s.st.lat, s.st.lon);
        s.run(120.0);
        assert_eq!(s.st.mode, "AUTOLAND");
        assert!(!s.st.armed && s.st.speed == 0.0 && (s.st.lat, s.st.lon) == at);
    }

    #[test]
    fn the_final_approach_comes_in_along_the_landing_direction() {
        // started in the air heading west: it lands heading west, from the east
        let mut s = aloft(0.0, 2000.0, true);
        set_mode(&mut s, "RTL");
        s.until(600.0, |s| s.said("Landing approach start"))
            .expect("approach");
        assert!(s.st.lon > LON, "the approach starts east of home");
        s.until(300.0, |s| s.said("Flare")).expect("flare");
        let h = super::wrap180(s.st.heading - 270.0).abs();
        assert!(h < 30.0, "flares heading {:.0}", s.st.heading);
    }

    #[test]
    fn at_home_already_it_keeps_rtl_for_three_heartbeats_first() {
        // circling its spawn point, which is home: on its RTL circle at once
        let mut s = aloft(0.0, 0.0, true);
        s.run(30.0);
        set_mode(&mut s, "RTL");
        s.run(RTL_ANNOUNCED_S - 0.2);
        assert_eq!(s.st.mode, "RTL", "a heartbeat must say RTL first");
        s.run(0.5);
        assert_eq!(s.st.mode, "AUTOLAND");
        s.until(300.0, |s| !s.st.armed).expect("lands");
        assert!(s.dist_to(0.0, 0.0) < 100.0);
    }

    #[test]
    fn without_rtl_autoland_rtl_circles_home_as_before() {
        let mut s = aloft(0.0, 2000.0, false);
        assert_eq!(
            s.params.get("RTL_AUTOLAND"),
            Some(0.0),
            "ArduPlane's default"
        );
        set_mode(&mut s, "RTL");
        s.run(600.0);
        assert_eq!(s.st.mode, "RTL");
        assert!(s.st.armed && (s.rel_alt() - 120.0).abs() < 0.5);
        assert!(s.dist_to(0.0, 0.0) < 150.0 * 1.2, "circling home");
        assert!(!s.said("Landing"));
    }

    #[test]
    fn a_landing_sequence_in_the_mission_is_flown_in_auto() {
        let mut s = aloft(0.0, 2000.0, true);
        s.mission(&[
            (WP, 3000.0, 3000.0, 100.0),
            (MavCmd::MAV_CMD_DO_LAND_START, 0.0, 0.0, 0.0),
            (WP, 0.0, -700.0, 40.0),
            (MavCmd::MAV_CMD_NAV_LAND, 0.0, -1200.0, 0.0),
        ]);
        set_mode(&mut s, "RTL");
        s.until(400.0, |s| s.st.mode != "RTL").expect("hands over");
        assert_eq!(s.st.mode, "AUTO");
        assert!(s.said("Landing sequence start"));
        assert_eq!(s.mission.seq, 2, "at the DO_LAND_START");
        assert!(
            s.mission.force_resume,
            "AUTO resumes there whatever MIS_RESTART says"
        );
        s.until(400.0, |s| !s.st.armed).expect("lands and disarms");
        assert!(
            s.dist_to(0.0, -1200.0) < 100.0,
            "at the LAND item, not home"
        );
        assert_eq!(s.reached, [2, 3]);
    }

    #[test]
    fn rtl_autoland_2_lands_at_once_and_looks_once_per_rtl() {
        let mut s = aloft(0.0, 2000.0, true);
        assert!(s.params.set("RTL_AUTOLAND", 2.0).is_ok());
        set_mode(&mut s, "RTL");
        s.run(RTL_ANNOUNCED_S + 0.2);
        assert_eq!(s.st.mode, "AUTOLAND", "not first home");
        // back to RTL (a GCS, a failsafe): it looks again
        set_mode(&mut s, "RTL");
        s.run(RTL_ANNOUNCED_S + 0.2);
        assert_eq!(s.st.mode, "AUTOLAND");
    }

    #[test]
    fn autoland_needs_it_flying() {
        let mut s = Sim::new("plane", 20.0, None);
        assert_eq!(s.mode(AUTOLAND_MODE), FAILED);
        assert!(s.said("Must already be flying!"));
        let mut s = aloft(0.0, 2000.0, false);
        assert_eq!(
            s.mode(AUTOLAND_MODE),
            None,
            "in the air: the shared code sets it"
        );
        set_mode(&mut s, "AUTOLAND");
        s.until(400.0, |s| !s.st.armed).expect("lands at home");
        assert!(s.dist_to(0.0, 0.0) < 100.0);
    }

    #[test]
    fn a_hand_launched_plane_down_again_refuses_to_arm() {
        let mut s = aloft(0.0, 600.0, true);
        set_mode(&mut s, "RTL");
        s.until(400.0, |s| !s.st.armed).expect("lands");
        assert_eq!(s.arm(true), FAILED);
        assert_eq!(
            s.said.last().map(String::as_str),
            Some("Arm: landed, no crew for a hand launch")
        );
        // RTL is refused first, as ever
        set_mode(&mut s, "RTL");
        assert_eq!(s.arm(true), FAILED);
        assert_eq!(
            s.said.last().map(String::as_str),
            Some("Arm: RTL mode not armable")
        );
        // a runway plane down again arms (the shared code does it)
        let mut r = aloft(0.0, 600.0, false);
        set_mode(&mut r, "AUTOLAND");
        r.until(400.0, |s| !s.st.armed).expect("lands");
        assert_eq!(r.arm(true), None);
    }

    #[test]
    fn its_first_launch_is_the_crews_and_armed_on_the_ground_it_waits_for_a_throw() {
        // on the ground where it starts: the crew is there, its takeoff runs
        let mut s = Sim::new("plane", 20.0, None);
        assert!(s.params.set("TKOFF_THR_MINACC", 15.0).is_ok());
        let takeoff = [
            (MavCmd::MAV_CMD_NAV_TAKEOFF, 0.0, 0.0, 60.0),
            (MavCmd::MAV_CMD_NAV_LOITER_UNLIM, 0.0, 0.0, 80.0),
        ];
        s.mission(&takeoff);
        assert_eq!(s.arm(true), None);
        s.st.armed = true;
        set_mode(&mut s, "AUTO");
        s.until(120.0, |s| s.rel_alt() > 50.0).expect("takes off");
        assert!(s.said("Autoland direction="));
        // home and down
        assert!(s.params.set("RTL_AUTOLAND", 1.0).is_ok());
        set_mode(&mut s, "RTL");
        s.until(600.0, |s| s.said("Flare")).expect("flares");
        s.until(60.0, |s| s.st.speed < 0.5).expect("rolls out");
        assert!(s.st.armed, "LAND_DISARMDELAY not up yet");
        // armed on the ground, told to take off again: no throw comes
        s.mission(&takeoff);
        let said = s.said.len();
        set_mode(&mut s, "AUTO");
        s.run(60.0);
        assert!(s.st.armed && s.rel_alt() < 0.5 && s.st.speed == 0.0);
        assert!(
            !s.said[said..].iter().any(|t| t.starts_with("Armed AUTO")),
            "{:?}",
            &s.said[said..]
        );
    }
}
