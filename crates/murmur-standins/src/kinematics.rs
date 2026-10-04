//! Vehicle state and the shared steps of a physics tick: steer toward a target, apply the
//! platform's turn / acceleration / climb limits, integrate position. Behaviors compose these.

use crate::geo::{M_PER_DEG, bearing_deg, distance_m, wrap180};
use crate::mission::Mission;
use mavlink::dialects::ardupilotmega::MavSeverity;

/// What the autopilot knows about itself: its navigation estimate. For most vehicles that is
/// the truth; a submerged AUV's estimate drifts from it (nav.rs keeps the difference, and
/// truth = estimate - error). Physics steps move it by the vehicle's true velocity.
#[derive(Debug, Clone)]
pub struct State {
    pub lat: f64,
    pub lon: f64,
    /// metres above MSL (negative = depth)
    pub alt: f64,
    pub heading: f64,
    pub speed: f64,
    pub vz: f64,
    pub ve: f64,
    pub vn: f64,
    /// the surface/ground at the spawn point; aircraft spawned high have home alt 0
    pub home: (f64, f64, f64),
    pub armed: bool,
    pub mode: &'static str,
    pub hold_point: (f64, f64),
    pub aground: bool,
    /// speed demand of the last physics step, m/s (`turn_and_accelerate` records it; VFR_HUD
    /// throttle and the battery load come from it)
    pub demand: f64,
}

impl State {
    pub fn depth(&self) -> f64 {
        (-self.alt).max(0.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TargetKind {
    Takeoff,
    Loiter,
    Rtl,
    Waypoint,
    Hold,
    /// fixed wing NAV_LAND: glide down to `alt` at the point; the behavior detects touchdown
    Land,
}

/// Where the current mode or mission item wants the vehicle. `radius` 0 = go straight to the
/// point (standins.py uses None or 0 for that, both falsy).
#[derive(Debug, Clone, Copy)]
pub struct Target {
    pub lat: f64,
    pub lon: f64,
    pub alt: f64,
    pub radius: f64,
    pub speed: f64,
    pub kind: TargetKind,
}

/// Messages a tick produces as events (not streams); sent right after the tick, before telemetry.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    Text(MavSeverity, String),
    MissionCurrent(u16),
    ItemReached(u16),
    /// the autopilot changes its own mode (mission end, an RTL item, SURFACE complete)
    SetMode(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub struct Want {
    pub speed: f64,
    pub heading: f64,
    pub alt: f64,
}

/// Steering toward `tgt`. `reach_m` is the waypoint acceptance radius; `stop_at_rtl` marks a
/// vehicle that can stop (boat, sub): it brakes into a point it holds at (home, a hold or
/// loiter point) and stops there, as Rover and Sub do, instead of circling it.
pub fn guide(
    st: &State,
    tgt: Option<&Target>,
    reach_m: f64,
    stop_at_rtl: bool,
    mission: &mut Mission,
    out: &mut Vec<Out>,
) -> Want {
    let mut want = Want {
        speed: 0.0,
        heading: st.heading,
        alt: st.alt,
    };
    let Some(t) = tgt else { return want };
    want.alt = t.alt;
    want.speed = t.speed;
    let dist = distance_m(st.lat, st.lon, t.lat, t.lon);
    let btc = bearing_deg(st.lat, st.lon, t.lat, t.lon);
    if t.kind == TargetKind::Takeoff {
        // done at the takeoff altitude (ArduPlane's verify_takeoff: relative altitude reached)
        if st.alt >= t.alt - 0.5 {
            mission.takeoff_done(st, out, st.alt - st.home.2);
        }
    } else if t.kind == TargetKind::Land {
        // a ~4.6 degree glide path to a touchdown `reach_m` short of the point (the roll-out)
        want.heading = btc;
        want.alt = (t.alt + (dist - reach_m).max(0.0) * 0.08).min(st.alt);
    } else if t.radius != 0.0 {
        // loiter / orbit, clockwise
        let corr = clamp((dist - t.radius) / t.radius.max(1.0) * 90.0, 90.0);
        want.heading = btc - 90.0 + corr;
    } else {
        want.heading = btc;
        let holds = matches!(
            t.kind,
            TargetKind::Rtl | TargetKind::Hold | TargetKind::Loiter
        );
        if dist < reach_m && t.kind == TargetKind::Waypoint {
            mission.reached_at(st, out, dist);
        } else if holds && stop_at_rtl {
            if dist < reach_m {
                want.speed = 0.0;
                want.heading = st.heading;
            } else {
                // brake at 0.4 m/s2 so it stops near the point instead of overshooting
                let brake = (2.0 * 0.4 * (dist - reach_m * 0.5).max(0.0)).sqrt();
                want.speed = want.speed.min(brake);
            }
        }
    }
    want
}

/// Turn and accelerate toward `want` within the platform's limits.
pub fn turn_and_accelerate(st: &mut State, want: &Want, dt: f64, turn_dps: f64, accel_mps2: f64) {
    let turn = turn_dps * dt;
    st.heading = (st.heading + clamp(wrap180(want.heading - st.heading), turn)).rem_euclid(360.0);
    let accel = accel_mps2 * dt;
    st.speed += clamp(want.speed - st.speed, accel);
    st.demand = want.speed;
}

/// Climb or dive toward `want.alt`, at most `climb_mps`.
pub fn climb(st: &mut State, want: &Want, dt: f64, climb_mps: f64) {
    st.vz = clamp(want.alt - st.alt, climb_mps);
    st.alt += st.vz * dt;
}

/// Python's `max(-lim, min(lim, x))`, operand order included, so a zero limit gives +0.0 like
/// Python's int 0 (a -0.0 would change the bytes on the wire). Never panics.
fn clamp(x: f64, lim: f64) -> f64 {
    let m = if x < lim { x } else { lim };
    let lo = 0.0 - lim;
    if m > lo { m } else { lo }
}

/// Move by own velocity plus drift (water current), flat-earth, latitude first.
pub fn integrate(st: &mut State, dt: f64, drift_e: f64, drift_n: f64) {
    let ve = st.speed * st.heading.to_radians().sin() + drift_e;
    let vn = st.speed * st.heading.to_radians().cos() + drift_n;
    st.lat += vn * dt / M_PER_DEG;
    st.lon += ve * dt / (M_PER_DEG * st.lat.to_radians().cos());
    st.ve = ve;
    st.vn = vn;
}
