//! Vehicle state and the shared steps of a physics tick: steer toward a target, apply the
//! platform's turn / acceleration / climb limits, integrate position. Behaviors compose these.

use crate::geo::{M_PER_DEG, bearing_deg, distance_m, wrap180};
use crate::mission::Mission;
use mavlink::dialects::ardupilotmega::MavSeverity;

/// What the autopilot knows about itself. Truth and reported state are the same today.
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
}

#[derive(Debug, Clone, Copy)]
pub struct Want {
    pub speed: f64,
    pub heading: f64,
    pub alt: f64,
}

/// Steering toward `tgt`. `reach_m` is the waypoint acceptance radius; `stop_at_rtl` makes a
/// vehicle that can stop (boat, sub) do so on reaching home.
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
        if st.alt >= t.alt - 5.0 {
            mission.advance(out);
        }
    } else if t.radius != 0.0 {
        // loiter / orbit, clockwise
        let corr = clamp((dist - t.radius) / t.radius.max(1.0) * 90.0, 90.0);
        want.heading = btc - 90.0 + corr;
    } else {
        want.heading = btc;
        if dist < reach_m && t.kind == TargetKind::Waypoint {
            mission.reached(out);
        } else if dist < reach_m && t.kind == TargetKind::Rtl && stop_at_rtl {
            want.speed = 0.0;
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
