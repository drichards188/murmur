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

use mavlink::dialects::ardupilotmega::{MavCmd, MavResult, MavSeverity, MavType};

use super::{Behavior, Command, Ctx, Tick, modes};
use crate::config::Spec;
use crate::geo::{bearing_deg, distance_m, wrap180};
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

/// ArduPlane's VTOL modes (Mode::is_vtol_mode): refused with Q_ENABLE 0.
fn is_q_mode(number: u32) -> bool {
    (17..=23).contains(&number)
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
        }
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
        let (lat, lon, radius, kind, alt) = if st.mode == "RTL" {
            let a = t.params.f("RTL_ALTITUDE", -1.0);
            let alt = if a < 0.0 { st.alt } else { st.home.2 + a };
            (st.home.0, st.home.1, rtl_r, TargetKind::Rtl, alt)
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
            text(t.out, format!("Autoland direction= {}", hdg.round() as i64));
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
                None
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
        if t.st.mode != self.mode_seen {
            self.on_mode(t, airborne);
        }
        if !t.st.armed && airborne {
            return self.glide(t);
        }
        let tgt = self.target(t);
        let landing = tgt.is_some_and(|g| g.kind == TargetKind::Land);
        let taking_off = tgt.is_some_and(|g| g.kind == TargetKind::Takeoff);
        if !taking_off {
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
        if let Some(g) = tgt.filter(|_| taking_off) {
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
        } else if on_ground && !taking_off && !landing {
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
