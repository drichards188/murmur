//! Dead reckoning for silent vehicles (murmur-4y9): where a vehicle whose link went quiet
//! probably is now, with a region that grows with the silence, and what its autopilot is
//! probably doing about the lost link.
//!
//! A display and situational-awareness feature only. The decision core keeps its own,
//! deliberately conservative friendly-uncertainty model (`keep_out_friendly` carries a silent
//! vehicle along its velocity, else as far as it could get at cruise speed); nothing here
//! reaches it, and nothing here authorizes anything.
//!
//! What goes in the snapshot: for each vehicle past its link's stale threshold
//! (`LinkSnapshot::stale_after_s`: 3 s streaming, 60 s on an acoustic link) an `estimate`
//! that holds a *motion model* frozen at the last report, not a position recomputed every
//! frame. The model is a function of what was last known (position, velocity, mode, task),
//! so the change-only push sends it once; the console evaluates it on every clock tick, as it
//! does for tracks. `now` is the model evaluated at a coarse time step for API readers.
//!
//! The model, in order of what is known best:
//!
//! 1. The vehicle's current task, when its last report was consistent with it (the op active
//!    or done, the mode the task's): an orbit, somewhere on its ring (the phase advancing at
//!    the last speed, the spread along the ring growing); a patrol, along its route at that
//!    speed; a hold, near the hold point (a fixed wing on its loiter circle); RTL, toward home
//!    (a fixed wing then circling home at its RTL radius).
//! 2. Else the mode alone: RTL, a loiter or a hold mode as above.
//! 3. Else the last velocity, carried straight on.
//! 4. Else stationary.
//!
//! Growth (all GUESSes, stated as the region's half-extents, about two sigma):
//! - initial `s0 = 10 m + speed x report interval / 2` (where in its reporting interval the
//!   vehicle was);
//! - under a task or mode the speed is uncertain by about 15 %: along-track `0.3 x speed`
//!   per second (at least 0.5 m/s), across-track the task's own tolerance plus `0.02 x max`;
//! - carried along a velocity: along `0.4 x speed + 0.1 x max`, across `0.3 x max` per second
//!   (it may turn: an aircraft's region grows at tens of metres a second, a boat's at a few);
//! - stationary: `0.1 x max` per second if armed, else only drift (1 m/s on the water);
//! - never beyond what the platform can reach (`max x dt + s0`) nor a cap per domain
//!   (25 km air, 5 km water, 3 km under water, 1 km ground).
//!
//! The lost-link branch: each vehicle's GCS failsafe timer follows its link profile
//! (`lost_link_s` in `sim/links.toml`, written into its parameters by `sim/swarm.py`), and
//! what it does is the firmware's (`crates/murmur-standins/src/params.rs`, `sim/params/`):
//! RTL for planes, copters, boats and UGVs, surface for an AUV, land for the airship, disarm
//! for a tethered ROV. If the vehicle lost us too (the usual case: a cut link is cut both
//! ways), after that long it is probably on that branch: a second model from the moment the
//! timer fires, starting with the uncertainty the first model had reached by then.
//!
//! An AUV on its acoustic modem reports every 20 s on its own and is out of contact by
//! design while under water: the estimate says so (`note`).

use std::cell::OnceCell;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::clock::{self, round};
use crate::link::LinkSnapshot;
use crate::mav::Kind;
use crate::tasks::{OpState, TaskKind, TaskParams};
use crate::vehicle::{Uid, Vehicle};
use crate::world::World;

/// `now` is evaluated at this step, so a silent vehicle's snapshot changes this often.
pub const NOW_STEP_S: f64 = 5.0;
/// Position uncertainty of a fresh report, m. GUESS.
const S0_M: f64 = 10.0;
/// Floor of the along-track growth under a task, m/s.
const MIN_ALONG_MPS: f64 = 0.5;
/// What a vehicle on the water does when nothing drives it, m/s (the Golden Gate's currents
/// run to 2.5 m/s at springs; a boat holding station fights them). GUESS.
const DRIFT_MPS: f64 = 1.0;
/// A navigation target reported within this long before the silence counts as current.
const NAV_TARGET_FRESH_S: f64 = 15.0;
/// How often the water under silent surface vehicles is sampled and their drift integrated.
pub const WATER_STEP_S: f64 = 5.0;
/// The share of the wind a drifting hull takes on. GUESS.
pub const WINDAGE: f64 = 0.02;
/// A water sample older than this is not trusted (tidewater went away).
const WATER_FRESH_S: f64 = 60.0;

/// [lon, lat], as positions are everywhere in the API.
pub type LngLat = [f64; 2];

/// What the console draws and the details card explains.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Estimate {
    /// When the model starts: the last position report (wall clock, s). `dt` counts from it.
    pub t0: f64,
    /// orbit | patrol | hold | rtl | velocity | stationary
    pub basis: &'static str,
    /// One line for the operator: "orbit #3, 160 m/s clockwise on a 9.0 km ring".
    pub detail: String,
    pub motion: Motion,
    /// The platform's top speed: the region never exceeds `reach_mps x dt + s0`.
    pub reach_mps: f64,
    /// The region's ceiling, m (per domain).
    pub cap_m: f64,
    /// Why the silence may be nothing (an AUV under water).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// What the autopilot's lost-link failsafe would make of this silence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failsafe: Option<FailsafeBranch>,
    /// Which hypothesis leads: `task` (the model above) until the lost-link timer fires,
    /// `failsafe` from then on (the console draws the branch as the estimate and keeps the
    /// task model alongside as "if it kept its task").
    pub leading: &'static str,
    /// The model evaluated at `t` (the last `NOW_STEP_S` boundary of the silence): for API
    /// readers; the console evaluates the model itself.
    pub now: At,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct At {
    pub t: f64,
    pub position: LngLat,
    /// Radius about `position` that holds the whole region, m.
    pub radius_m: f64,
}

/// A motion model from `t0`. Distances in metres, rates per second, bearings in degrees
/// clockwise from north. Evaluated by [`eval`] here and by `estimate.ts` in the console.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Motion {
    /// Somewhere on a circle: an orbit, a fixed wing's loiter, circling home. The position is
    /// `angle0 + rate x dt` around `center`; the region is the ring (`radius +- width / 2`)
    /// within `spread0 + spread_rate x dt` degrees of it (180: the whole ring).
    Ring {
        center: LngLat,
        radius_m: f64,
        width_m: f64,
        angle0_deg: f64,
        /// signed: positive clockwise (bearing increasing)
        rate_dps: f64,
        spread0_deg: f64,
        spread_dps: f64,
    },
    /// Along a closed route at a speed (a patrol): from `lead` to `route[0]` first when still
    /// en route, else from `frac0` along segment `seg0`. The region is an ellipse along the
    /// route (`along`, `across`), the whole route once `along` covers half of it.
    Route {
        route: Vec<LngLat>,
        #[serde(skip_serializing_if = "Option::is_none")]
        lead: Option<LngLat>,
        seg0: usize,
        frac0: f64,
        speed_mps: f64,
        along0_m: f64,
        along_mps: f64,
        across0_m: f64,
        across_mps: f64,
    },
    /// A straight line from `from` along `bearing` at `speed`, stopping after `stop_m` (RTL:
    /// at home; a velocity carried on: unbounded). An ellipse along the line until it stops,
    /// then `arrive`.
    Line {
        from: LngLat,
        bearing_deg: f64,
        speed_mps: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        stop_m: Option<f64>,
        along0_m: f64,
        along_mps: f64,
        across0_m: f64,
        across_mps: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        arrive: Option<Arrival>,
    },
    /// Staying put: a circle of `radius0 + rate x dt`.
    Point {
        at: LngLat,
        radius0_m: f64,
        rate_mps: f64,
    },
}

/// Where a [`Motion::Line`] ends: a circle (home), or a ring a fixed wing circles on.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Arrival {
    pub center: LngLat,
    pub radius_m: f64,
    pub ring: bool,
    /// a ring's band, m
    pub width_m: f64,
    /// a ring's phase rate once joined, degrees/s, positive clockwise (0 for a circle)
    pub rate_dps: f64,
}

/// The lost-link failsafe hypothesis: from `fires_t` the vehicle is probably on `motion`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FailsafeBranch {
    /// rtl | surface | land | disarm
    pub action: &'static str,
    /// the timer, s (`lost_link_s` of `profile`)
    pub after_s: f64,
    pub fires_t: f64,
    pub profile: String,
    pub detail: String,
    pub motion: Motion,
    /// the timer has fired: the branch leads
    pub fired: bool,
    /// the branch evaluated at the estimate's `now.t`, once fired
    #[serde(skip_serializing_if = "Option::is_none")]
    pub now: Option<At>,
}

/// The region a model gives at some time.
#[derive(Debug, Clone, PartialEq)]
pub enum Region {
    Circle {
        center: LngLat,
        radius_m: f64,
    },
    Ellipse {
        center: LngLat,
        semi_major_m: f64,
        semi_minor_m: f64,
        bearing_deg: f64,
    },
    /// a ring (radius +- width / 2) within `spread` degrees of `angle`
    Arc {
        center: LngLat,
        radius_m: f64,
        width_m: f64,
        angle_deg: f64,
        spread_deg: f64,
    },
    /// the whole route, this wide
    Band {
        route: Vec<LngLat>,
        width_m: f64,
    },
}

impl Region {
    /// The radius about `position` holding the whole region.
    pub fn radius_about(&self, position: LngLat) -> f64 {
        match self {
            Region::Circle { center, radius_m } => radius_m + distance_m(*center, position),
            Region::Ellipse { center, semi_major_m, .. } => {
                semi_major_m + distance_m(*center, position)
            }
            Region::Arc {
                radius_m,
                width_m,
                spread_deg,
                ..
            } => {
                let chord = if *spread_deg >= 180.0 {
                    2.0 * radius_m
                } else {
                    2.0 * radius_m * (spread_deg.to_radians() / 2.0).sin()
                };
                chord + width_m / 2.0
            }
            Region::Band { route, width_m } => {
                route
                    .iter()
                    .map(|p| distance_m(*p, position))
                    .fold(0.0, f64::max)
                    + width_m / 2.0
            }
        }
    }
}

// ---- geometry (flat earth around the vehicle: fine for the kilometres involved) -------------

fn m_per_deg_lon(lat: f64) -> f64 {
    111_320.0 * lat.to_radians().cos()
}

/// Flat-earth distance, so that `step(a, bearing(a, b), distance_m(a, b))` lands on `b` (the
/// console does the same sums).
fn distance_m(a: LngLat, b: LngLat) -> f64 {
    let (e, n) = enu(a, b);
    e.hypot(n)
}

fn bearing(a: LngLat, b: LngLat) -> f64 {
    let (e, n) = enu(a, b);
    e.atan2(n).to_degrees().rem_euclid(360.0)
}

/// `dist` metres from `p` along `bearing`.
fn step(p: LngLat, bearing_deg: f64, dist: f64) -> LngLat {
    let (lat, lon) = crate::geo::destination(p[1], p[0], bearing_deg, dist);
    [lon, lat]
}

/// (east, north) metres of `p` from `origin`.
fn enu(origin: LngLat, p: LngLat) -> (f64, f64) {
    (
        (p[0] - origin[0]) * m_per_deg_lon(origin[1]),
        (p[1] - origin[1]) * 111_320.0,
    )
}

/// Along a closed route (the last point joins the first): segment lengths.
fn segments(route: &[LngLat]) -> Vec<f64> {
    (0..route.len())
        .map(|i| distance_m(route[i], route[(i + 1) % route.len()]))
        .collect()
}

/// The point `frac` of the way along segment `seg` of a closed route, and its bearing.
fn along_route(route: &[LngLat], seg: usize, frac: f64) -> (LngLat, f64) {
    let a = route[seg % route.len()];
    let b = route[(seg + 1) % route.len()];
    let bearing = bearing(a, b);
    (step(a, bearing, frac * distance_m(a, b)), bearing)
}

/// The nearest segment of a closed route to `p`, as (segment, fraction, distance off it).
/// With a heading, among segments within 1.5 x the nearest distance the one whose bearing
/// agrees best (a route that doubles back has two near segments; the heading tells them
/// apart).
fn nearest_segment(route: &[LngLat], p: LngLat, heading: Option<f64>) -> (usize, f64, f64) {
    let n = route.len();
    let mut best: Vec<(usize, f64, f64)> = Vec::with_capacity(n);
    for i in 0..n {
        let a = route[i];
        let b = route[(i + 1) % n];
        let (ax, ay) = enu(a, a);
        let (bx, by) = enu(a, b);
        let (px, py) = enu(a, p);
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 {
            (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (qx, qy) = (ax + t * dx, ay + t * dy);
        best.push((i, t, ((px - qx).powi(2) + (py - qy).powi(2)).sqrt()));
    }
    let nearest = best.iter().map(|s| s.2).fold(f64::INFINITY, f64::min);
    let Some(h) = heading else {
        return best
            .into_iter()
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .unwrap_or((0, 0.0, 0.0));
    };
    let off = |s: &(usize, f64, f64)| {
        let b0 = bearing(route[s.0], route[(s.0 + 1) % n]);
        ((b0 - h + 180.0).rem_euclid(360.0) - 180.0).abs()
    };
    best.into_iter()
        .filter(|s| s.2 <= nearest * 1.5 + 1.0)
        .min_by(|a, b| off(a).total_cmp(&off(b)))
        .unwrap_or((0, 0.0, 0.0))
}

// ---- evaluation --------------------------------------------------------------------------

/// Where the model puts the vehicle `dt` seconds after its `t0`, and the region around it,
/// bounded by what the platform can reach and the cap.
pub fn eval(m: &Motion, dt: f64, reach_mps: f64, cap_m: f64) -> (LngLat, Region) {
    let dt = dt.max(0.0);
    let bound = |x: f64, s0: f64| x.min(cap_m).min(reach_mps * dt + s0);
    match m {
        Motion::Ring {
            center,
            radius_m,
            width_m,
            angle0_deg,
            rate_dps,
            spread0_deg,
            spread_dps,
        } => {
            let angle = (angle0_deg + rate_dps * dt).rem_euclid(360.0);
            let position = step(*center, angle, *radius_m);
            // the spread along the ring, in degrees, never further than it could have flown
            let reach_deg = if *radius_m > 0.0 {
                (reach_mps * dt / radius_m).to_degrees()
            } else {
                180.0
            };
            let spread = (spread0_deg + spread_dps * dt)
                .min(spread0_deg + reach_deg)
                .min(180.0);
            (
                position,
                Region::Arc {
                    center: *center,
                    radius_m: *radius_m,
                    width_m: *width_m,
                    angle_deg: angle,
                    spread_deg: spread,
                },
            )
        }
        Motion::Route {
            route,
            lead,
            seg0,
            frac0,
            speed_mps,
            along0_m,
            along_mps,
            across0_m,
            across_mps,
        } => {
            let segs = segments(route);
            let loop_m: f64 = segs.iter().sum();
            let mut s = speed_mps * dt;
            let (mut seg, mut frac) = (*seg0, *frac0);
            let mut position = None;
            let mut bearing_deg = 0.0;
            if let Some(from) = lead {
                let lead_m = distance_m(*from, route[0]);
                let b = bearing(*from, route[0]);
                if s < lead_m {
                    position = Some(step(*from, b, s));
                    bearing_deg = b;
                } else {
                    s -= lead_m;
                    seg = 0;
                    frac = 0.0;
                }
            }
            let position = position.unwrap_or_else(|| {
                if loop_m <= 0.0 {
                    return route[0];
                }
                // advance from (seg, frac) by s, wrapping
                let mut rem = s.rem_euclid(loop_m) + frac * segs[seg % segs.len()];
                let mut i = seg % segs.len();
                loop {
                    let len = segs[i];
                    if rem <= len || len <= 0.0 {
                        let f = if len > 0.0 { rem / len } else { 0.0 };
                        let (p, b) = along_route(route, i, f);
                        bearing_deg = b;
                        return p;
                    }
                    rem -= len;
                    i = (i + 1) % segs.len();
                }
            });
            let along = bound(along0_m + along_mps * dt, *along0_m);
            let across = bound(across0_m + across_mps * dt, *across0_m);
            if lead.is_none() && along >= loop_m / 2.0 && loop_m > 0.0 {
                (
                    position,
                    Region::Band {
                        route: route.clone(),
                        width_m: 2.0 * across,
                    },
                )
            } else {
                (
                    position,
                    Region::Ellipse {
                        center: position,
                        semi_major_m: along,
                        semi_minor_m: across,
                        bearing_deg,
                    },
                )
            }
        }
        Motion::Line {
            from,
            bearing_deg,
            speed_mps,
            stop_m,
            along0_m,
            along_mps,
            across0_m,
            across_mps,
            arrive,
        } => {
            let mut d = speed_mps * dt;
            let along = bound(along0_m + along_mps * dt, *along0_m);
            let across = bound(across0_m + across_mps * dt, *across0_m);
            // the point stops at the destination; the region counts it as arrived only once
            // even a vehicle lagging by the along-track uncertainty would be there (a turn
            // out of an orbit, a slower speed over ground), the ellipse covering the lag
            // until then
            let mut arrived = false;
            let mut at_stop = false;
            if let Some(stop) = stop_m
                && d >= *stop
            {
                arrived = d >= stop + along;
                at_stop = true;
                d = *stop;
            }
            let stop_point = step(*from, *bearing_deg, d);
            // a ring joined: the point goes on around it from the moment it got there
            let position = match arrive {
                Some(a) if at_stop && a.ring && a.radius_m > 0.0 => {
                    let since = dt - stop_m.unwrap_or(0.0) / speed_mps.max(1e-9);
                    let angle = bearing(a.center, stop_point) + a.rate_dps * since.max(0.0);
                    step(a.center, angle.rem_euclid(360.0), a.radius_m)
                }
                _ => stop_point,
            };
            match arrive {
                // on the ring it circles: where on it is unknown (the whole ring), how far
                // off it is only the ring's own band
                Some(a) if arrived && a.ring => (
                    position,
                    Region::Arc {
                        center: a.center,
                        radius_m: a.radius_m,
                        width_m: a.width_m,
                        angle_deg: bearing(a.center, position),
                        spread_deg: 180.0,
                    },
                ),
                Some(a) if arrived => (
                    position,
                    Region::Circle {
                        center: a.center,
                        radius_m: a.radius_m + along0_m,
                    },
                ),
                // not surely there yet: the ellipse stays about the stop point, where a
                // lagging vehicle is still heading
                _ => (
                    position,
                    Region::Ellipse {
                        center: stop_point,
                        semi_major_m: along,
                        semi_minor_m: across,
                        bearing_deg: *bearing_deg,
                    },
                ),
            }
        }
        Motion::Point {
            at,
            radius0_m,
            rate_mps,
        } => (
            *at,
            Region::Circle {
                center: *at,
                radius_m: bound(radius0_m + rate_mps * dt, *radius0_m),
            },
        ),
    }
}

// ---- configuration: platform limits and lost-link timers ------------------------------------

/// What the estimate needs from `sim/platforms.toml` about a platform type.
#[derive(Debug, Clone, PartialEq)]
pub struct PlatformLimits {
    pub cruise_mps: f64,
    pub max_mps: f64,
    pub domain: String,
    pub loiter_radius_m: Option<f64>,
    pub link: Option<String>,
    pub space_class: Option<String>,
    /// no propulsion (a drifting buoy): it moves with the water, nothing else
    pub drifter: bool,
}

#[derive(Deserialize)]
struct RawPlatform {
    domain: Option<String>,
    behavior: Option<String>,
    cruise_mps: Option<f64>,
    max_mps: Option<f64>,
    loiter_radius_m: Option<f64>,
    link: Option<String>,
    space_class: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Limits {
    pub platforms: HashMap<String, PlatformLimits>,
}

impl Limits {
    pub fn from_toml(text: &str) -> Result<Limits, String> {
        let raw: BTreeMap<String, RawPlatform> =
            toml::from_str(text).map_err(|e| format!("platforms.toml: {e}"))?;
        let platforms = raw
            .into_iter()
            .map(|(id, r)| {
                let cruise = r.cruise_mps.unwrap_or(0.0);
                // SITL platforms list no top speed: a third over cruise (GUESS)
                let max = r.max_mps.unwrap_or(cruise * 1.3);
                (
                    id,
                    PlatformLimits {
                        cruise_mps: cruise,
                        max_mps: max,
                        domain: r.domain.unwrap_or_default(),
                        loiter_radius_m: r.loiter_radius_m,
                        link: r.link,
                        space_class: r.space_class,
                        drifter: r.behavior.as_deref() == Some("drifter"),
                    },
                )
            })
            .collect();
        Ok(Limits { platforms })
    }
}

/// The lost-link timers of `sim/links.toml`: `lost_link_s` per profile (inherited through
/// `extends`), and which profile a platform starts on (`[platform_profile]`, else SATCOM by
/// `[space_class_profile]`, else the link class; an acoustic AUV's is `acoustic`, the profile
/// its timer was written from).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct LostLink {
    timers: HashMap<String, f64>,
    platform_profile: HashMap<String, String>,
    space_class_profile: HashMap<String, String>,
}

impl LostLink {
    pub fn from_toml(text: &str) -> Result<LostLink, String> {
        let table: toml::Table = text.parse().map_err(|e| format!("links.toml: {e}"))?;
        let classes = table
            .get("class")
            .and_then(toml::Value::as_table)
            .cloned()
            .unwrap_or_default();
        let timer_of = |name: &str| -> Option<f64> {
            let mut cur = name.to_string();
            for _ in 0..8 {
                let c = classes.get(&cur)?.as_table()?;
                if let Some(t) = c.get("lost_link_s").and_then(num) {
                    return Some(t);
                }
                cur = c.get("extends")?.as_str()?.to_string();
            }
            None
        };
        let timers = classes
            .keys()
            .filter_map(|k| timer_of(k).map(|t| (k.clone(), t)))
            .collect();
        let strings = |key: &str| -> HashMap<String, String> {
            table
                .get(key)
                .and_then(toml::Value::as_table)
                .map(|t| {
                    t.iter()
                        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                        .collect()
                })
                .unwrap_or_default()
        };
        Ok(LostLink {
            timers,
            platform_profile: strings("platform_profile"),
            space_class_profile: strings("space_class_profile"),
        })
    }

    /// The profile a vehicle's timer was written from, and the timer.
    pub fn for_vehicle(
        &self,
        platform: Option<&str>,
        link_class: Option<&str>,
        space_class: Option<&str>,
    ) -> Option<(String, f64)> {
        let profile = platform
            .and_then(|p| self.platform_profile.get(p).cloned())
            .or_else(|| match (link_class?, space_class) {
                ("satcom", Some(sc)) => Some(
                    self.space_class_profile
                        .get(sc)
                        .cloned()
                        .unwrap_or_else(|| "satcom".to_string()),
                ),
                (c, _) => Some(c.to_string()),
            })?;
        let t = *self.timers.get(&profile)?;
        Some((profile, t))
    }
}

fn num(v: &toml::Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

/// Both tables, read once per process from the bridge's configured paths.
pub struct Config {
    pub limits: Limits,
    pub lost_link: LostLink,
}

impl Config {
    pub fn load(platforms: &Path, links: &Path) -> Config {
        let limits = std::fs::read_to_string(platforms)
            .map_err(|e| e.to_string())
            .and_then(|t| Limits::from_toml(&t))
            .unwrap_or_else(|e| {
                eprintln!("estimate: no platform limits ({e}); kinds' defaults");
                Limits::default()
            });
        let lost_link = std::fs::read_to_string(links)
            .map_err(|e| e.to_string())
            .and_then(|t| LostLink::from_toml(&t))
            .unwrap_or_else(|e| {
                eprintln!("estimate: no lost-link timers ({e}); no failsafe branch");
                LostLink::default()
            });
        Config { limits, lost_link }
    }
}

fn config(world: &World) -> &'static Config {
    static CONFIG: OnceLock<Config> = OnceLock::new();
    CONFIG.get_or_init(|| {
        let links = std::env::var_os("MURMUR_LINKS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| world.cfg.root.join("sim/links.toml"));
        Config::load(&world.cfg.platforms, &links)
    })
}

/// A kind's figures when the manifest names no platform (a SITL vehicle on a bare port).
fn kind_defaults(kind: Option<Kind>) -> PlatformLimits {
    let (cruise, domain) = match kind {
        Some(Kind::Plane | Kind::Vtol) => (20.0, "air"),
        Some(Kind::Copter) => (10.0, "air"),
        Some(Kind::Blimp) => (3.0, "air"),
        Some(Kind::Boat) => (8.0, "surface"),
        Some(Kind::Ugv) => (4.0, "ground"),
        Some(Kind::Sub) => (1.5, "subsurface"),
        Some(Kind::Tracker) => (0.0, "ground"),
        None | Some(Kind::Unknown) => (5.0, "ground"),
    };
    PlatformLimits {
        cruise_mps: cruise,
        max_mps: cruise * 1.3,
        domain: domain.to_string(),
        loiter_radius_m: None,
        link: None,
        space_class: None,
        drifter: false,
    }
}

fn cap_for(domain: &str) -> f64 {
    match domain {
        "air" => 25_000.0,
        "surface" => 5_000.0,
        "subsurface" => 3_000.0,
        _ => 1_000.0,
    }
}

// ---- the water under silent surface vehicles ------------------------------------------------

/// Where a silent surface vehicle has drifted to: its last known position carried along
/// tidewater's current (plus windage) every [`WATER_STEP_S`] by [`water_sampler`], with the
/// uncertainty that accumulates as the current field changes under it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaterTrack {
    /// the integrated position, [lon, lat]
    pub pos: LngLat,
    /// when it was last stepped (wall, s)
    pub t: f64,
    /// the region's half-extent then, m
    pub sigma_m: f64,
    /// the drift velocity now, [east, north] m/s (current + windage)
    pub drift_en: [f64; 2],
    pub current_en: [f64; 2],
    pub wind_en: [f64; 2],
}

fn norm(v: [f64; 2]) -> f64 {
    v[0].hypot(v[1])
}

/// How fast a drift region grows: the current changes over the distance drifted and with
/// the tide, the wind over time, and waves jitter the hull. GUESS.
pub fn drift_growth_mps(current: [f64; 2], wind: [f64; 2]) -> f64 {
    0.3 * norm(current) + WINDAGE * norm(wind) + 0.1
}

fn water_tracks() -> MutexGuard<'static, HashMap<Uid, WaterTrack>> {
    static TRACKS: OnceLock<Mutex<HashMap<Uid, WaterTrack>>> = OnceLock::new();
    TRACKS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// The drift track of a vehicle, if the sampler keeps one that is fresh.
pub fn water_track(uid: Uid, now: f64) -> Option<WaterTrack> {
    water_tracks()
        .get(&uid)
        .copied()
        .filter(|t| now - t.t <= WATER_FRESH_S)
}

/// Tests: a track as the sampler would keep it.
#[cfg(test)]
pub fn set_water_track(uid: Uid, track: WaterTrack) {
    water_tracks().insert(uid, track);
}

/// Step every surface (and subsurface: a surfaced AUV drifts too) vehicle's drift track: a
/// vehicle heard from is reset to where it is;
/// a silent one is carried along the current sampled at its track every step (the drifting
/// buoy's own motion is exactly this, `crates/murmur-standins/src/behavior/drifter.rs`).
/// Without `MURMUR_TIDEWATER_URL` there is no track and the estimate falls back to its
/// documented default bound (`DRIFT_MPS`).
pub async fn water_sampler(world: Arc<World>) {
    let Some(client) = crate::decision::terrain::Tidewater::from_env() else {
        println!("estimate: no MURMUR_TIDEWATER_URL: silent drifters get the default drift bound");
        return;
    };
    let mut interval = tokio::time::interval(Duration::from_secs_f64(WATER_STEP_S));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut failed_at: Option<f64> = None;
    loop {
        interval.tick().await;
        let wall = clock::wall();
        let cfg = config(&world);
        let mut rows: Vec<(Uid, bool, LngLat)> = Vec::new();
        {
            let tracks = water_tracks();
            for cell in world.vehicles() {
                let link = cell.link_snapshot(clock::mono());
                let stale = link
                    .last_rx_t
                    .is_some_and(|t| wall - t > link.stale_after_s());
                let (pos, platform, kind) = cell.read(|v| {
                    (
                        v.pos,
                        v.identity.as_ref().and_then(|i| i.str("platform").map(str::to_string)),
                        v.kind,
                    )
                });
                let domain = platform
                    .as_deref()
                    .and_then(|p| cfg.limits.platforms.get(p))
                    .map(|l| l.domain.clone())
                    .unwrap_or_else(|| kind_defaults(kind).domain);
                if domain != "surface" && domain != "subsurface" {
                    continue;
                }
                let Some((lat, lon)) = pos else { continue };
                let at = match (stale, tracks.get(&cell.uid)) {
                    (true, Some(t)) => t.pos,
                    _ => [lon, lat],
                };
                rows.push((cell.uid, stale, at));
            }
        }
        if rows.is_empty() {
            continue;
        }
        let points: Vec<murmur_decide::LonLat> = rows
            .iter()
            .map(|(_, _, p)| murmur_decide::LonLat { lon: p[0], lat: p[1] })
            .collect();
        let samples = match client.water(&points).await {
            Ok(s) => s,
            Err(e) => {
                if failed_at.is_none_or(|t| wall - t > 60.0) {
                    eprintln!("water sampler: {e}; drift estimates use what they have");
                }
                failed_at = Some(wall);
                continue;
            }
        };
        let mut tracks = water_tracks();
        for ((uid, stale, at), w) in rows.into_iter().zip(samples) {
            let current = w.current_en.unwrap_or([0.0, 0.0]);
            let wind = w.wind_en.unwrap_or([0.0, 0.0]);
            let drift = [current[0] + WINDAGE * wind[0], current[1] + WINDAGE * wind[1]];
            let next = match (stale, tracks.get(&uid)) {
                // carried along the drift it had since the last step; its region grows
                (true, Some(prev)) => {
                    let dt = (wall - prev.t).max(0.0);
                    let (e, n) = (prev.drift_en[0] * dt, prev.drift_en[1] * dt);
                    let pos = [
                        prev.pos[0] + e / m_per_deg_lon(prev.pos[1]),
                        prev.pos[1] + n / 111_320.0,
                    ];
                    WaterTrack {
                        pos,
                        t: wall,
                        sigma_m: prev.sigma_m + drift_growth_mps(prev.current_en, prev.wind_en) * dt,
                        drift_en: drift,
                        current_en: current,
                        wind_en: wind,
                    }
                }
                // heard from (or no track yet): the track starts where it is
                _ => WaterTrack {
                    pos: at,
                    t: wall,
                    sigma_m: S0_M,
                    drift_en: drift,
                    current_en: current,
                    wind_en: wind,
                },
            };
            tracks.insert(uid, next);
        }
    }
}

// ---- the vehicle's inputs ------------------------------------------------------------------

/// The vehicle's latest op, as the task layer reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Ordered {
    pub tasking: u64,
    pub kind: TaskKind,
    pub state: OpState,
    pub params: TaskParams,
}

/// Everything the model is built from: the vehicle's last report and its surroundings.
#[derive(Debug, Clone)]
pub struct Inputs<'a> {
    /// the last reported position
    pub position: LngLat,
    /// when it was reported (wall, s)
    pub t0: f64,
    /// the link's last packet (wall, s)
    pub last_rx_t: f64,
    pub now: f64,
    /// [east, north] m/s
    pub vel_en: Option<[f64; 2]>,
    pub groundspeed: Option<f64>,
    pub heading: Option<f64>,
    pub mode: Option<&'a str>,
    pub armed: bool,
    pub kind: Option<Kind>,
    pub limits: PlatformLimits,
    pub link_class: Option<&'a str>,
    /// seconds between position reports on this link
    pub report_s: f64,
    pub submerged: bool,
    pub home: Option<LngLat>,
    /// the autopilot's navigation target (a loiter center) and when it was reported
    pub nav_target: Option<(LngLat, f64)>,
    pub mode_since: Option<f64>,
    /// WP_LOITER_RAD as read, m
    pub loiter_radius_m: Option<f64>,
    /// the RTL circle (`Vehicle::rtl_loiter_radius_m`)
    pub rtl_radius_m: Option<f64>,
    pub min_turn_m: Option<f64>,
    pub task: Option<&'a Ordered>,
    pub lost_link: Option<(String, f64)>,
    /// the drift track the water sampler keeps for a surface vehicle
    pub water: Option<WaterTrack>,
}

/// A chosen model: what it rests on, from when, and how far the platform can get.
struct Chosen {
    basis: &'static str,
    motion: Motion,
    detail: String,
    /// the model's start (the last report, or the drift track's last step)
    t0: f64,
    reach_mps: f64,
}

impl Inputs<'_> {
    fn speed(&self) -> f64 {
        self.groundspeed.unwrap_or(0.0).max(0.0)
    }

    /// The speed the task is flown at: the last ground speed when moving, else cruise.
    fn task_speed(&self) -> f64 {
        let s = self.speed();
        if s >= 0.3 {
            s
        } else {
            self.limits.cruise_mps
        }
    }

    fn fixed_wing(&self) -> bool {
        matches!(self.kind, Some(Kind::Plane | Kind::Vtol))
    }

    fn s0(&self) -> f64 {
        S0_M + 0.5 * self.speed() * self.report_s
    }

    fn across_rate(&self) -> f64 {
        0.02 * self.limits.max_mps
    }

    /// Clockwise around `center` (bearing increasing), from the last velocity; the default
    /// when not moving (ArduPilot loiters clockwise at a positive radius, the bridge's
    /// waypoint rings run clockwise too).
    fn clockwise_about(&self, center: LngLat) -> bool {
        let Some([ve, vn]) = self.vel_en else {
            return true;
        };
        if ve.hypot(vn) < 0.3 {
            return true;
        }
        let theta = bearing(center, self.position).to_radians();
        ve * theta.cos() - vn * theta.sin() >= 0.0
    }

    /// The on-station tolerance of a ring of this radius (`tasks::exec::orbit_geometry`).
    fn ring_tolerance(&self, radius: f64) -> f64 {
        if self.fixed_wing() {
            (radius * 0.4).max(60.0)
        } else {
            (radius * 0.3).max(25.0)
        }
    }

    /// Around `center` from the phase of `at`, with `spread0_m` of initial uncertainty along
    /// the ring. The sense of rotation is judged where the velocity was measured (the last
    /// position), and only if that was on this ring too; a ring joined later is flown
    /// clockwise, as ArduPilot loiters at a positive radius.
    fn ring(&self, at: LngLat, center: LngLat, radius: f64, spread0_m: f64) -> Motion {
        let speed = self.task_speed();
        let rate = if radius > 0.0 {
            (speed / radius).to_degrees()
        } else {
            0.0
        };
        let was_on_ring =
            (distance_m(center, self.position) - radius).abs() <= self.ring_width(radius);
        let clockwise = !was_on_ring || self.clockwise_about(center);
        let sign = if clockwise { 1.0 } else { -1.0 };
        let along_mps = (0.3 * speed).max(MIN_ALONG_MPS);
        let to_deg = |m: f64| if radius > 0.0 { (m / radius).to_degrees() } else { 180.0 };
        Motion::Ring {
            center,
            radius_m: radius,
            width_m: self.ring_width(radius),
            angle0_deg: bearing(center, at),
            rate_dps: sign * rate,
            spread0_deg: to_deg(spread0_m).min(180.0),
            spread_dps: to_deg(along_mps),
        }
    }

    fn point(&self, at: LngLat, radius0: f64, rate: f64) -> Motion {
        Motion::Point {
            at,
            radius0_m: radius0,
            rate_mps: rate,
        }
    }

    /// Toward home at `speed`; a fixed wing stops on its RTL circle and circles, the rest stop
    /// at home. `along_mps` is the along-track growth: the speed's own uncertainty, plus what
    /// the caller knows about wind or current.
    fn homeward(&self, from: LngLat, speed: f64, along0: f64, along_mps: f64) -> Option<Motion> {
        let home = self.home?;
        let dist = distance_m(from, home);
        let (stop, arrive) = if self.fixed_wing() {
            let r = self
                .rtl_radius_m
                .or(self.limits.loiter_radius_m)
                .or(self.min_turn_m)
                .unwrap_or(150.0)
                .max(1.0);
            let band = self.ring_width(r) / 2.0;
            // already on its RTL circle: it circles home from here on
            if (dist - r).abs() <= band {
                return Some(self.ring(from, home, r, along0));
            }
            // inside it: ArduPlane flies out to the circle; outside: in toward it. Then it
            // circles, clockwise as ArduPilot loiters at a positive radius (a circle it was
            // already flying went through `ring` above)
            let (bearing_deg, stop) = if dist < r {
                (bearing(home, from), r - dist)
            } else {
                (bearing(from, home), dist - r)
            };
            return Some(Motion::Line {
                from,
                bearing_deg,
                speed_mps: speed,
                stop_m: Some(stop),
                along0_m: along0,
                along_mps: along_mps.max(MIN_ALONG_MPS),
                across0_m: along0,
                across_mps: self.across_rate(),
                arrive: Some(Arrival {
                    center: home,
                    radius_m: r,
                    ring: true,
                    width_m: self.ring_width(r),
                    rate_dps: (speed / r).to_degrees(),
                }),
            });
        } else {
            let tol = match self.kind {
                Some(Kind::Copter) | Some(Kind::Boat) => 15.0,
                Some(Kind::Blimp) => 30.0,
                Some(Kind::Sub) => 20.0,
                _ => 10.0,
            };
            (
                dist,
                Arrival {
                    center: home,
                    radius_m: tol,
                    ring: false,
                    width_m: 0.0,
                    rate_dps: 0.0,
                },
            )
        };
        Some(Motion::Line {
            from,
            bearing_deg: bearing(from, home),
            speed_mps: speed,
            stop_m: Some(stop),
            along0_m: along0,
            along_mps: along_mps.max(MIN_ALONG_MPS),
            across0_m: along0,
            across_mps: self.across_rate(),
            arrive: Some(arrive),
        })
    }

    /// The width of a ring region: the circle is flown to within a fraction of its radius
    /// (wind pushes a loiter out; the task layer's on-station tolerance, 0.4 R, is looser than
    /// where the vehicle really is). GUESS.
    fn ring_width(&self, radius: f64) -> f64 {
        2.0 * (radius * 0.15).max(60.0)
    }

    /// The last velocity carried on.
    fn velocity(&self) -> Option<Motion> {
        let [ve, vn] = self.vel_en?;
        let speed = ve.hypot(vn);
        if speed < 0.3 {
            return None;
        }
        let max = self.limits.max_mps.max(speed);
        Some(Motion::Line {
            from: self.position,
            bearing_deg: ve.atan2(vn).to_degrees().rem_euclid(360.0),
            speed_mps: speed,
            stop_m: None,
            along0_m: self.s0(),
            along_mps: 0.4 * speed + 0.1 * max,
            across0_m: self.s0(),
            across_mps: 0.3 * max,
            arrive: None,
        })
    }

    fn stationary(&self) -> Motion {
        let water = self.limits.domain == "surface";
        let drift = if water || self.limits.drifter {
            DRIFT_MPS
        } else {
            0.0
        };
        let rate = if self.armed && !self.limits.drifter {
            (0.1 * self.limits.max_mps).max(drift)
        } else {
            drift
        };
        self.point(self.position, self.s0(), rate)
    }

    /// A drifter (no propulsion): carried along tidewater's current from the sampler's
    /// track, the region growing with the current's uncertainty; without water data, its
    /// last reported velocity (which is the water's) or the documented default bound.
    fn drifting(&self) -> Chosen {
        if let Some(w) = self.water {
            let speed = norm(w.drift_en);
            let growth = drift_growth_mps(w.current_en, w.wind_en);
            let bearing_deg = w.drift_en[0].atan2(w.drift_en[1]).to_degrees().rem_euclid(360.0);
            return Chosen {
                basis: "drift",
                motion: Motion::Line {
                    from: w.pos,
                    bearing_deg,
                    speed_mps: speed,
                    stop_m: None,
                    along0_m: w.sigma_m,
                    along_mps: growth,
                    across0_m: w.sigma_m,
                    across_mps: growth,
                    arrive: None,
                },
                detail: format!(
                    "drifting with the water: {:.1} m/s current, {:.0} m/s wind at its track (tidewater)",
                    norm(w.current_en),
                    norm(w.wind_en)
                ),
                t0: w.t,
                reach_mps: speed + growth,
            };
        }
        if let Some(m) = self.velocity() {
            let speed = self.vel_en.map_or(0.0, norm);
            return Chosen {
                basis: "velocity",
                motion: m,
                detail: format!("drifting on at its last {} (no water data)", fmt_mps(speed)),
                t0: self.t0,
                reach_mps: speed + DRIFT_MPS,
            };
        }
        Chosen {
            basis: "stationary",
            motion: self.point(self.position, self.s0(), DRIFT_MPS),
            detail: format!("drifting: no water data, so a default {DRIFT_MPS:.0} m/s bound"),
            t0: self.t0,
            reach_mps: DRIFT_MPS,
        }
    }

    /// The hold models: a fixed wing circles the loiter center it reported (fresh in this
    /// mode when the link went quiet), everything else stays near where it was.
    fn holding(&self, label: &str) -> (Motion, String) {
        if self.fixed_wing() {
            let fresh = self.nav_target.filter(|(_, t)| {
                *t >= self.last_rx_t - NAV_TARGET_FRESH_S && self.mode_since.is_none_or(|m| *t >= m)
            });
            let r = self
                .loiter_radius_m
                .map(f64::abs)
                .or(self.limits.loiter_radius_m)
                .or(self.min_turn_m)
                .unwrap_or(150.0)
                .max(self.min_turn_m.unwrap_or(0.0));
            if let Some((center, _)) = fresh {
                return (
                    self.ring(self.position, center, r, self.s0()),
                    format!("{label}: circling its loiter center at {}", fmt_m(r)),
                );
            }
            // no center known: it circles somewhere within a loiter radius of here
            return (
                self.point(self.position, self.s0() + 2.0 * r, 0.0),
                format!("{label}: circling (loiter radius {}), center unknown", fmt_m(r)),
            );
        }
        let water = self.limits.domain == "surface";
        let rate = if water { 0.3 } else { 0.05 * self.limits.max_mps };
        (
            self.point(self.position, self.s0() + 10.0, rate),
            format!("{label}: station keeping"),
        )
    }

    /// The task's model, when the last report was consistent with it.
    fn task_model(&self) -> Option<(&'static str, Motion, String)> {
        let task = self.task?;
        if !matches!(task.state, OpState::Active | OpState::Done) {
            return None;
        }
        let mode = self.mode?;
        let auto = self.kind.and_then(crate::tasks::caps).map(|c| c.auto);
        let hold = self.kind.and_then(crate::tasks::caps).map(|c| c.hold);
        let speed = self.task_speed();
        match task.kind {
            TaskKind::Orbit if Some(mode) == auto => {
                let center = task.params.center?;
                let mut radius = task.params.radius.unwrap_or(200.0);
                if let Some(min_r) = self.min_turn_m.filter(|r| *r > 0.0)
                    && radius < min_r * 1.1
                {
                    radius = (min_r * 1.1).round();
                }
                let tol = self.ring_tolerance(radius);
                let off = (distance_m(center, self.position) - radius).abs();
                if task.state == OpState::Done || off <= 2.0 * tol {
                    let turn = if self.clockwise_about(center) { "clockwise" } else { "counter-clockwise" };
                    return Some((
                        "orbit",
                        self.ring(self.position, center, radius, self.s0()),
                        format!("orbit #{}: {} {turn} on its {} ring", task.tasking, fmt_mps(speed), fmt_m(radius)),
                    ));
                }
                // still flying to the ring: along its velocity
                let m = self.velocity()?;
                Some(("velocity", m, format!("orbit #{}: still {} from its ring, carried on its last heading", task.tasking, fmt_m(off))))
            }
            TaskKind::Patrol if Some(mode) == auto => {
                let route = task.params.route.clone()?;
                if route.len() < 2 {
                    return None;
                }
                let (seg, frac, off) = nearest_segment(&route, self.position, self.heading);
                let across0 = if self.limits.domain == "air" { 90.0 } else { 25.0 };
                let en_route = task.state == OpState::Active && off > 3.0 * across0 + self.s0();
                // on the route the model snaps it onto the line: the region keeps how far off
                // the line it really was
                let (lead, seg0, frac0, across0, what) = if en_route {
                    (Some(self.position), 0, 0.0, across0, "en route to its first waypoint, then along its route".to_string())
                } else {
                    (None, seg, frac, across0 + off, format!("along its {}-point route, leg {}", route.len(), seg + 1))
                };
                Some((
                    "patrol",
                    Motion::Route {
                        route,
                        lead,
                        seg0,
                        frac0,
                        speed_mps: speed,
                        along0_m: self.s0(),
                        along_mps: (0.3 * speed).max(MIN_ALONG_MPS),
                        across0_m: across0 + self.s0(),
                        across_mps: self.across_rate(),
                    },
                    format!("patrol #{}: {} {what}", task.tasking, fmt_mps(speed)),
                ))
            }
            TaskKind::Hold if Some(mode) == hold => {
                let (m, d) = self.holding(&format!("hold #{}", task.tasking));
                Some(("hold", m, d))
            }
            TaskKind::Rtl if mode == "RTL" || (self.kind == Some(Kind::Sub) && Some(mode) == auto) => {
                let m = self.homeward(self.position, speed, self.s0(), 0.3 * speed)?;
                Some(("rtl", m, format!("rtl #{}: {} toward home", task.tasking, fmt_mps(speed))))
            }
            _ => None,
        }
    }

    /// The mode's model, with no task to go on.
    fn mode_model(&self) -> Option<(&'static str, Motion, String)> {
        let mode = self.mode?;
        let hold = self.kind.and_then(crate::tasks::caps).map(|c| c.hold);
        if mode == "RTL" {
            let speed = self.task_speed();
            let m = self.homeward(self.position, speed, self.s0(), 0.3 * speed)?;
            return Some(("rtl", m, format!("in RTL: {} toward home", fmt_mps(self.task_speed()))));
        }
        if Some(mode) == hold || (self.fixed_wing() && mode == "LOITER") {
            let (m, d) = self.holding(&format!("in {mode}"));
            return Some(("hold", m, d));
        }
        None
    }

    /// What the GCS failsafe does for this kind, if it would act.
    fn failsafe_action(&self) -> Option<&'static str> {
        if !self.armed {
            return None;
        }
        match self.kind? {
            Kind::Plane | Kind::Vtol | Kind::Copter => {
                (self.mode != Some("RTL")).then_some("rtl")
            }
            Kind::Boat | Kind::Ugv => {
                (!matches!(self.mode, Some("RTL" | "HOLD"))).then_some("rtl")
            }
            Kind::Blimp => Some("land"),
            Kind::Sub => Some(if self.link_class == Some("acoustic") { "surface" } else { "disarm" }),
            Kind::Tracker | Kind::Unknown => None,
        }
    }
}

fn fmt_m(m: f64) -> String {
    if m >= 1000.0 {
        format!("{:.1} km", m / 1000.0)
    } else {
        format!("{m:.0} m")
    }
}

fn fmt_mps(v: f64) -> String {
    format!("{v:.0} m/s")
}

/// The estimate for a silent vehicle. None when there is nothing to go on (no position).
pub fn estimate(inp: &Inputs) -> Estimate {
    let chosen = inp
        .task_model()
        .or_else(|| inp.mode_model())
        .map(|(basis, motion, detail)| Chosen {
            basis,
            motion,
            detail,
            t0: inp.t0,
            reach_mps: inp.limits.max_mps.max(inp.speed()),
        })
        .unwrap_or_else(|| {
            if inp.limits.drifter {
                return inp.drifting();
            }
            let (basis, motion, detail) = inp
                .velocity()
                .map(|m| {
                    let speed = inp.vel_en.map_or(0.0, |[e, n]| e.hypot(n));
                    ("velocity", m, format!("carried on at its last {} on heading {:.0}°", fmt_mps(speed), inp.heading.unwrap_or(0.0)))
                })
                .unwrap_or_else(|| {
                    let m = inp.stationary();
                    let why = if !inp.armed {
                        "not armed, so probably where it was"
                    } else {
                        "no velocity reported: probably where it was"
                    };
                    ("stationary", m, why.to_string())
                });
            Chosen {
                basis,
                motion,
                detail,
                t0: inp.t0,
                reach_mps: inp.limits.max_mps.max(inp.speed()),
            }
        });
    let Chosen {
        basis,
        motion,
        detail,
        t0,
        reach_mps,
    } = chosen;
    let cap_m = cap_for(&inp.limits.domain);
    let note = (inp.link_class == Some("acoustic") && (inp.submerged || inp.report_s >= 10.0)).then(|| {
        format!(
            "acoustic link: it reports every {:.0} s on its own; silence under water is normal",
            inp.report_s.max(20.0)
        )
    });
    let failsafe = inp.failsafe_action().and_then(|action| {
        let (profile, after_s) = inp.lost_link.clone()?;
        let fires_t = inp.last_rx_t + after_s;
        let (p, region) = eval(&motion, fires_t - t0, reach_mps, cap_m);
        // the first model's uncertainty when the timer fires, plus the turn toward home (a
        // reversal ends two turn radii beside the straight line)
        let s0 = region.radius_about(p) + S0_M + 2.0 * inp.min_turn_m.unwrap_or(0.0);
        // home at the catalog's cruise speed, through the air or water: the last ground speed
        // says how much wind or current there is, and the branch grows by that much a second
        let cruise = inp.limits.cruise_mps.max(0.5);
        let drift = if inp.speed() >= 0.3 { (cruise - inp.speed()).abs() } else { 0.0 };
        let (branch, what) = match action {
            "rtl" => (
                inp.homeward(p, cruise, s0, 0.3 * cruise + drift)?,
                if inp.fixed_wing() {
                    "RTL: toward home at cruise speed, then circling home"
                } else {
                    "RTL: toward home at cruise speed"
                },
            ),
            // surfaced it drifts like a buoy: at the sampled current (plus its growth), else
            // the default bound
            "surface" => (
                inp.point(
                    p,
                    s0,
                    inp.water
                        .map(|w| norm(w.drift_en) + drift_growth_mps(w.current_en, w.wind_en))
                        .unwrap_or(DRIFT_MPS),
                ),
                "surfacing (FS_GCS_ENABLE 4), then drifting on its surface radio",
            ),
            "land" => (inp.point(p, s0, 0.3), "landing where it is"),
            _ => (inp.point(p, s0, 0.0), "disarming where it is"),
        };
        Some(FailsafeBranch {
            action,
            after_s,
            fires_t: round(fires_t, 1),
            detail: format!("lost-link failsafe after {after_s:.0} s ({profile}), if it lost us too: {what}"),
            profile,
            motion: branch,
            fired: false,
            now: None,
        })
    });
    let silent = (inp.now - inp.last_rx_t).max(0.0);
    let t_now = inp.last_rx_t + NOW_STEP_S * (silent / NOW_STEP_S).floor();
    let at = |m: &Motion, t0: f64| {
        let (position, region) = eval(m, t_now - t0, reach_mps, cap_m);
        At {
            t: round(t_now, 1),
            position: [round(position[0], 6), round(position[1], 6)],
            radius_m: round(region.radius_about(position), 0),
        }
    };
    let now = at(&motion, t0);
    let failsafe = failsafe.map(|f| {
        let fired = t_now >= f.fires_t;
        FailsafeBranch {
            now: fired.then(|| at(&f.motion, f.fires_t)),
            fired,
            motion: round_motion(f.motion),
            ..f
        }
    });
    let leading = if failsafe.as_ref().is_some_and(|f| f.fired) {
        "failsafe"
    } else {
        "task"
    };
    Estimate {
        t0: round(t0, 1),
        basis,
        detail,
        motion: round_motion(motion),
        reach_mps: round(reach_mps, 1),
        cap_m,
        note,
        failsafe,
        leading,
        now,
    }
}

/// What an agent reads about a silent vehicle (`mcp::world`): the leading hypothesis now,
/// with the model's start as `as_of`, and the other hypothesis. A display estimate, never
/// a fact: the decision core does not read it.
pub fn agent_view(e: &Estimate, now: f64) -> Value {
    let at = |m: &Motion, t0: f64| {
        let (p, region) = eval(m, now - t0, e.reach_mps, e.cap_m);
        json!({
            "lon": round(p[0], 6),
            "lat": round(p[1], 6),
            "radius_m": round(region.radius_about(p), 0),
        })
    };
    let task = json!({
        "basis": e.basis,
        "detail": e.detail,
        "as_of": e.t0,
        "age_s": round((now - e.t0).max(0.0), 1),
        "now": at(&e.motion, e.t0),
    });
    let failsafe = e.failsafe.as_ref().map(|f| {
        json!({
            "action": f.action,
            "after_s": f.after_s,
            "fires_t": f.fires_t,
            "fired": f.fired,
            "profile": f.profile,
            "detail": f.detail,
            "now": f.fired.then(|| at(&f.motion, f.fires_t)),
        })
    });
    json!({
        "note": "a display estimate of where the vehicle probably is while its link is silent, not an observed fact: the decision core does not read it, and nothing here is a position fix",
        "leading": e.leading,
        "task": task,
        "failsafe": failsafe,
        "acoustic_note": e.note,
        "reach_mps": e.reach_mps,
        "cap_m": e.cap_m,
    })
}

/// Rounded so the serialized model is stable between frames (it is sent only on change).
fn round_motion(m: Motion) -> Motion {
    let p = |p: LngLat| [round(p[0], 6), round(p[1], 6)];
    let r = |x: f64| round(x, 2);
    match m {
        Motion::Ring {
            center,
            radius_m,
            width_m,
            angle0_deg,
            rate_dps,
            spread0_deg,
            spread_dps,
        } => Motion::Ring {
            center: p(center),
            radius_m: r(radius_m),
            width_m: r(width_m),
            angle0_deg: r(angle0_deg),
            rate_dps: round(rate_dps, 4),
            spread0_deg: r(spread0_deg),
            spread_dps: round(spread_dps, 4),
        },
        Motion::Route {
            route,
            lead,
            seg0,
            frac0,
            speed_mps,
            along0_m,
            along_mps,
            across0_m,
            across_mps,
        } => Motion::Route {
            route: route.into_iter().map(p).collect(),
            lead: lead.map(p),
            seg0,
            frac0: round(frac0, 4),
            speed_mps: r(speed_mps),
            along0_m: r(along0_m),
            along_mps: r(along_mps),
            across0_m: r(across0_m),
            across_mps: r(across_mps),
        },
        Motion::Line {
            from,
            bearing_deg,
            speed_mps,
            stop_m,
            along0_m,
            along_mps,
            across0_m,
            across_mps,
            arrive,
        } => Motion::Line {
            from: p(from),
            bearing_deg: r(bearing_deg),
            speed_mps: r(speed_mps),
            stop_m: stop_m.map(r),
            along0_m: r(along0_m),
            along_mps: r(along_mps),
            across0_m: r(across0_m),
            across_mps: r(across_mps),
            arrive: arrive.map(|a| Arrival {
                center: p(a.center),
                radius_m: r(a.radius_m),
                ring: a.ring,
                width_m: r(a.width_m),
                rate_dps: round(a.rate_dps, 4),
            }),
        },
        Motion::Point {
            at,
            radius0_m,
            rate_mps,
        } => Motion::Point {
            at: p(at),
            radius0_m: r(radius0_m),
            rate_mps: r(rate_mps),
        },
    }
}

// ---- per frame -----------------------------------------------------------------------------

/// One push frame's context: the configuration and, read once on the first silent vehicle,
/// each vehicle's latest op.
pub struct FrameContext<'a> {
    world: &'a World,
    wall: f64,
    ordered: OnceCell<HashMap<Uid, Ordered>>,
}

impl<'a> FrameContext<'a> {
    pub fn new(world: &'a World, wall: f64) -> FrameContext<'a> {
        FrameContext {
            world,
            wall,
            ordered: OnceCell::new(),
        }
    }

    fn ordered(&self) -> &HashMap<Uid, Ordered> {
        self.ordered.get_or_init(|| {
            let taskings = self.world.tasks.snapshot();
            let params: HashMap<u64, TaskParams> =
                taskings.into_iter().map(|t| (t.id, t.params)).collect();
            self.world
                .tasks
                .current_ops()
                .into_iter()
                .filter_map(|o| {
                    let p = params.get(&o.tasking)?.clone();
                    Some((
                        o.uid,
                        Ordered {
                            tasking: o.tasking,
                            kind: o.task,
                            state: o.state,
                            params: p,
                        },
                    ))
                })
                .collect()
        })
    }

    /// The estimate for a vehicle whose link is past its stale threshold; None while it is
    /// heard from, or when it never reported a position. Called under the vehicle's lock.
    pub fn estimate(&self, uid: Uid, v: &Vehicle, link: &LinkSnapshot) -> Option<Estimate> {
        let last_rx_t = link.last_rx_t?;
        if self.wall - last_rx_t <= link.stale_after_s() {
            return None;
        }
        let (lat, lon) = v.pos?;
        let cfg = config(self.world);
        let ident = v.identity.as_ref();
        let platform = ident.and_then(|i| i.str("platform"));
        let limits = platform
            .and_then(|p| cfg.limits.platforms.get(p).cloned())
            .unwrap_or_else(|| kind_defaults(v.kind));
        let (limits_link, limits_space) = (limits.link.clone(), limits.space_class.clone());
        let link_class = ident.and_then(|i| i.str("link")).or(limits_link.as_deref());
        let space_class = ident
            .and_then(|i| i.str("space_class"))
            .or(limits_space.as_deref());
        let lost_link = cfg.lost_link.for_vehicle(platform, link_class, space_class);
        let report_s = link
            .telemetry
            .as_ref()
            .map_or(1.0, |p| p.report_s)
            .max(f64::from(link.expected_interval_ms) / 1000.0);
        let ordered = self.ordered();
        let inp = Inputs {
            position: [lon, lat],
            t0: v.seen.position.unwrap_or(last_rx_t).min(last_rx_t),
            last_rx_t,
            now: self.wall,
            vel_en: v.vel_en,
            groundspeed: v.groundspeed,
            heading: v.heading,
            mode: v.mode.as_deref(),
            armed: v.armed,
            kind: v.kind,
            limits,
            link_class,
            report_s,
            submerged: v.alt_msl.is_some_and(|a| a < -crate::telemetry::SUBMERGED_M),
            home: v.home.map(|(la, lo)| [lo, la]),
            nav_target: v.nav_target.zip(v.seen.nav_target).map(|((la, lo), t)| ([lo, la], t)),
            mode_since: v.mode_since,
            loiter_radius_m: v.loiter_radius_m,
            rtl_radius_m: v.rtl_loiter_radius_m(),
            min_turn_m: ident.and_then(|i| i.num("min_turn_radius_m")),
            task: ordered.get(&uid),
            lost_link,
            water: water_track(uid, self.wall),
        };
        Some(estimate(&inp))
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::*;

    const GATE: LngLat = [-122.4780, 37.8180];

    fn limits(cruise: f64, max: f64, domain: &str) -> PlatformLimits {
        PlatformLimits {
            cruise_mps: cruise,
            max_mps: max,
            domain: domain.into(),
            loiter_radius_m: None,
            link: None,
            space_class: None,
            drifter: false,
        }
    }

    fn base(kind: Kind, cruise: f64, max: f64, domain: &str) -> Inputs<'static> {
        Inputs {
            position: GATE,
            t0: 1000.0,
            last_rx_t: 1000.0,
            now: 1047.0,
            vel_en: None,
            groundspeed: None,
            heading: None,
            mode: None,
            armed: true,
            kind: Some(kind),
            limits: limits(cruise, max, domain),
            link_class: Some("los"),
            report_s: 1.0,
            submerged: false,
            home: Some([-122.4460, 37.8068]),
            nav_target: None,
            mode_since: None,
            loiter_radius_m: None,
            rtl_radius_m: None,
            min_turn_m: None,
            task: None,
            lost_link: Some(("los".into(), 5.0)),
            water: None,
        }
    }

    fn moving<'a>(mut inp: Inputs<'a>, bearing: f64, speed: f64) -> Inputs<'a> {
        let r = bearing.to_radians();
        inp.vel_en = Some([speed * r.sin(), speed * r.cos()]);
        inp.groundspeed = Some(speed);
        inp.heading = Some(bearing);
        inp
    }

    fn radius_of(region: &Region) -> f64 {
        match region {
            Region::Circle { radius_m, .. } => *radius_m,
            Region::Ellipse { semi_major_m, .. } => *semi_major_m,
            Region::Arc { spread_deg, .. } => *spread_deg,
            Region::Band { width_m, .. } => *width_m,
        }
    }

    #[test]
    fn velocity_carries_the_vehicle_on_and_the_region_grows_with_kind() {
        // a boat east at 6 m/s and a plane east at 160 m/s, silent 60 s
        let boat = moving(base(Kind::Boat, 6.0, 13.0, "surface"), 90.0, 6.0);
        let plane = moving(base(Kind::Plane, 160.0, 175.0, "air"), 90.0, 160.0);
        let (eb, ep) = (estimate(&boat), estimate(&plane));
        assert_eq!((eb.basis, ep.basis), ("velocity", "velocity"));
        let (pb, rb) = eval(&eb.motion, 60.0, eb.reach_mps, eb.cap_m);
        let (pp, rp) = eval(&ep.motion, 60.0, ep.reach_mps, ep.cap_m);
        assert!((distance_m(GATE, pb) - 360.0).abs() < 2.0, "{pb:?}");
        assert!((distance_m(GATE, pp) - 9600.0).abs() < 20.0, "{pp:?}");
        assert!((bearing(GATE, pp) - 90.0).abs() < 0.5);
        let Region::Ellipse { semi_major_m: ab, semi_minor_m: bb, bearing_deg, .. } = rb else { panic!("{rb:?}") };
        let Region::Ellipse { semi_major_m: ap, semi_minor_m: bp, .. } = rp else { panic!("{rp:?}") };
        assert!((bearing_deg - 90.0).abs() < 0.5);
        // along: 10 + 3 + (0.4 x 6 + 1.3) x 60 = 235 m; across: 13 + 0.3 x 13 x 60 = 247 (a boat
        // may turn as much as it may speed up: across is not clamped to along)
        assert!((ab - 235.0).abs() < 1.0, "{ab}");
        assert!((bb - 247.0).abs() < 1.0, "{bb}");
        // the plane's grows an order of magnitude faster: (64 + 17.5) x 60 + 90 = 4980 m
        assert!(ap > 10.0 * ab, "{ap} vs {ab}");
        assert!((ap - 4980.0).abs() < 2.0, "{ap}");
        assert!((bp - (90.0 + 0.3 * 175.0 * 60.0)).abs() < 2.0, "{bp}");
        // monotone growth
        let r = |dt| radius_of(&eval(&ep.motion, dt, ep.reach_mps, ep.cap_m).1);
        assert!(r(10.0) < r(30.0) && r(30.0) < r(120.0));
    }

    #[test]
    fn growth_is_bounded_by_reach_and_cap() {
        let plane = moving(base(Kind::Plane, 160.0, 175.0, "air"), 0.0, 160.0);
        let e = estimate(&plane);
        // 1 s of silence: the region is at most what it could have flown plus s0
        let (_, r) = eval(&e.motion, 1.0, e.reach_mps, e.cap_m);
        assert!(radius_of(&r) <= 175.0 + 90.0 + 1e-6, "{r:?}");
        // an hour of silence: the cap
        let (_, r) = eval(&e.motion, 3600.0, e.reach_mps, e.cap_m);
        assert_eq!(radius_of(&r), 25_000.0);
        let boat = moving(base(Kind::Boat, 6.0, 13.0, "surface"), 0.0, 6.0);
        let eb = estimate(&boat);
        assert_eq!(radius_of(&eval(&eb.motion, 3600.0, eb.reach_mps, eb.cap_m).1), 5_000.0);
    }

    #[test]
    fn stationary_when_nothing_moves() {
        let mut ugv = base(Kind::Ugv, 4.0, 5.2, "ground");
        ugv.armed = false;
        let e = estimate(&ugv);
        assert_eq!(e.basis, "stationary");
        let (p, r) = eval(&e.motion, 600.0, e.reach_mps, e.cap_m);
        assert_eq!(p, GATE);
        assert_eq!(radius_of(&r), 10.0); // unarmed, on land: it stays
        // armed, it might start moving; on the water it drifts even unarmed
        ugv.armed = true;
        let e = estimate(&ugv);
        assert!((radius_of(&eval(&e.motion, 100.0, e.reach_mps, e.cap_m).1) - 62.0).abs() < 1e-9);
        let mut buoy = base(Kind::Boat, 0.0, 0.0, "surface");
        buoy.armed = false;
        buoy.limits.drifter = true;
        let e = estimate(&buoy);
        // a drifter's reach is the water's: without tidewater, the documented default bound
        assert_eq!((e.basis, e.reach_mps), ("stationary", DRIFT_MPS));
        assert!(e.detail.contains("default"), "{}", e.detail);
        assert_eq!(radius_of(&eval(&e.motion, 100.0, e.reach_mps, e.cap_m).1), 110.0);
        // with its last reported velocity (the water's), carried on
        let buoy = moving(buoy, 270.0, 1.6);
        let e = estimate(&buoy);
        assert_eq!((e.basis, e.reach_mps), ("velocity", 1.6 + DRIFT_MPS));
    }

    #[test]
    fn a_drifter_follows_the_water_sampler_track() {
        // the sampler carried the buoy 300 m east over its last 200 s of silence, where the
        // flood runs 1.5 m/s east with a 5 m/s wind from the south
        let mut buoy = base(Kind::Boat, 0.0, 0.0, "surface");
        buoy.armed = false;
        buoy.limits.drifter = true;
        let track = WaterTrack {
            pos: step(GATE, 90.0, 300.0),
            t: 1200.0,
            sigma_m: 10.0 + 200.0 * drift_growth_mps([1.5, 0.0], [0.0, 5.0]),
            drift_en: [1.5, 0.0 + WINDAGE * 5.0],
            current_en: [1.5, 0.0],
            wind_en: [0.0, 5.0],
        };
        buoy.water = Some(track);
        buoy.now = 1247.0;
        let e = estimate(&buoy);
        assert_eq!(e.basis, "drift");
        assert_eq!(e.t0, 1200.0); // the model starts at the track's last step, not the last report
        assert!(e.detail.contains("tidewater"), "{}", e.detail);
        let Motion::Line { from, speed_mps, bearing_deg, along0_m, along_mps, .. } = &e.motion else { panic!("{:?}", e.motion) };
        assert!(distance_m(*from, track.pos) < 0.2);
        assert!((speed_mps - (1.5f64).hypot(0.1)).abs() < 0.01);
        assert!((bearing_deg - 86.19).abs() < 0.1, "{bearing_deg}");
        assert!((along0_m - track.sigma_m).abs() < 0.01);
        assert!((along_mps - (0.45 + 0.1 + 0.1)).abs() < 1e-9);
        assert!((e.reach_mps - (speed_mps + along_mps)).abs() < 0.1);
        // 100 s on: 150 m further east of the track, the region grown by 65 m
        let (p, r) = eval(&e.motion, 100.0, e.reach_mps, e.cap_m);
        assert!((distance_m(track.pos, p) - 150.3).abs() < 1.0, "{}", distance_m(track.pos, p));
        assert!((radius_of(&r) - (track.sigma_m + 65.0)).abs() < 0.01);
        // the sampler's store: a fresh track is served, an old one is not
        set_water_track(99, track);
        assert_eq!(water_track(99, 1250.0), Some(track));
        assert_eq!(water_track(99, 1200.0 + WATER_FRESH_S + 1.0), None);
        assert_eq!(water_track(98, 1250.0), None);
    }

    fn ordered(kind: TaskKind, state: OpState, params: TaskParams) -> Ordered {
        Ordered {
            tasking: 7,
            kind,
            state,
            params,
        }
    }

    #[test]
    fn orbit_moves_around_its_ring_in_the_direction_it_was_going() {
        let center: LngLat = [-122.5, 37.75];
        let radius = 9000.0;
        // the aircraft on the ring due east of the center, flying south: clockwise
        let on_ring = step(center, 90.0, radius);
        let task = ordered(
            TaskKind::Orbit,
            OpState::Done,
            TaskParams {
                center: Some(center),
                radius: Some(radius),
                ..TaskParams::default()
            },
        );
        let mut inp = moving(base(Kind::Plane, 160.0, 175.0, "air"), 180.0, 160.0);
        inp.position = on_ring;
        inp.mode = Some("AUTO");
        inp.task = Some(&task);
        let e = estimate(&inp);
        assert_eq!(e.basis, "orbit");
        assert!(e.detail.contains("clockwise"), "{}", e.detail);
        let Motion::Ring { rate_dps, angle0_deg, width_m, .. } = &e.motion else { panic!("{:?}", e.motion) };
        assert!(*rate_dps > 0.0 && (angle0_deg - 90.0).abs() < 0.1, "{rate_dps} {angle0_deg}");
        assert!((width_m - 2.0 * 1350.0).abs() < 1e-6); // 0.15 x 9 km each side
        // a quarter turn: 2 pi 9000 / 4 / 160 = 88.4 s, now due south of the center
        let (p, r) = eval(&e.motion, 88.36, e.reach_mps, e.cap_m);
        assert!((bearing(center, p) - 180.0).abs() < 0.5, "{}", bearing(center, p));
        assert!((distance_m(center, p) - radius).abs() < 5.0);
        let Region::Arc { spread_deg, .. } = r else { panic!("{r:?}") };
        // along-ring growth: 0.3 x 160 = 48 m/s = 0.31 deg/s on a 9 km ring, 27 deg after 88 s, plus s0
        assert!((spread_deg - (48.0 * 88.36 + 90.0) / radius * 180.0 / PI).abs() < 0.2, "{spread_deg}");
        // flying north on the east side instead: counter-clockwise, the angle decreases
        let inp2 = moving(inp.clone(), 0.0, 160.0);
        let e2 = estimate(&inp2);
        let Motion::Ring { rate_dps, .. } = &e2.motion else { panic!() };
        assert!(*rate_dps < 0.0);
        let (p2, _) = eval(&e2.motion, 88.36, e2.reach_mps, e2.cap_m);
        assert!((bearing(center, p2) - 0.0).abs() < 0.5 || (bearing(center, p2) - 360.0).abs() < 0.5);
        // long silence: the whole ring
        let (_, r) = eval(&e.motion, 3600.0, e.reach_mps, e.cap_m);
        let Region::Arc { spread_deg, .. } = r else { panic!() };
        assert_eq!(spread_deg, 180.0);
        assert!((r.radius_about(p) - (2.0 * radius + 1350.0)).abs() < 1e-6);
    }

    #[test]
    fn orbit_not_yet_on_station_is_carried_on_its_heading() {
        let center: LngLat = [-122.5, 37.75];
        let task = ordered(
            TaskKind::Orbit,
            OpState::Active,
            TaskParams {
                center: Some(center),
                radius: Some(500.0),
                ..TaskParams::default()
            },
        );
        let mut inp = moving(base(Kind::Copter, 12.0, 15.0, "air"), 270.0, 12.0);
        inp.position = step(center, 90.0, 5000.0); // 4.5 km off the ring
        inp.mode = Some("AUTO");
        inp.task = Some(&task);
        let e = estimate(&inp);
        assert_eq!(e.basis, "velocity");
        assert!(e.detail.contains("still"), "{}", e.detail);
        // in the wrong mode the task is not trusted at all
        inp.mode = Some("LAND");
        assert_eq!(estimate(&inp).basis, "velocity");
    }

    #[test]
    fn patrol_advances_along_its_route_and_wraps() {
        // a 1 km square, the boat on the first leg heading east
        let a: LngLat = [-122.50, 37.80];
        let b = step(a, 90.0, 1000.0);
        let c = step(b, 180.0, 1000.0);
        let d = step(a, 180.0, 1000.0);
        let route = vec![a, b, c, d];
        let task = ordered(
            TaskKind::Patrol,
            OpState::Done,
            TaskParams {
                route: Some(route.clone()),
                ..TaskParams::default()
            },
        );
        let mut inp = moving(base(Kind::Boat, 6.0, 13.0, "surface"), 90.0, 6.0);
        inp.position = step(a, 90.0, 400.0);
        inp.mode = Some("AUTO");
        inp.task = Some(&task);
        let e = estimate(&inp);
        assert_eq!(e.basis, "patrol");
        let Motion::Route { seg0, frac0, .. } = &e.motion else { panic!("{:?}", e.motion) };
        assert_eq!(*seg0, 0);
        assert!((frac0 - 0.4).abs() < 0.01, "{frac0}");
        // 100 s at 6 m/s: 600 m on, past the first corner, 0 m... 400 + 600 = 1000: at b
        let (p, _) = eval(&e.motion, 100.0, e.reach_mps, e.cap_m);
        assert!(distance_m(p, b) < 5.0, "{p:?} vs {b:?}");
        // 200 s: 600 m down the second leg
        let (p, r) = eval(&e.motion, 200.0, e.reach_mps, e.cap_m);
        assert!(distance_m(p, step(b, 180.0, 600.0)) < 5.0);
        let Region::Ellipse { bearing_deg, semi_major_m, .. } = r else { panic!("{r:?}") };
        assert!((bearing_deg - 180.0).abs() < 0.5);
        assert!((semi_major_m - (13.0 + 1.8 * 200.0)).abs() < 1.0, "{semi_major_m}");
        // a whole lap and a bit: back on the first leg
        let (p, _) = eval(&e.motion, (4000.0 + 100.0) / 6.0, e.reach_mps, e.cap_m);
        assert!(distance_m(p, step(a, 90.0, 500.0)) < 5.0, "{p:?}");
        // long enough that it could be anywhere on the loop: the whole route
        let (_, r) = eval(&e.motion, 2000.0, e.reach_mps, e.cap_m);
        assert!(matches!(r, Region::Band { .. }), "{r:?}");
        // the heading picks the leg on a route that doubles back on itself
        let (seg, _, _) = nearest_segment(&route, step(a, 90.0, 500.0), Some(90.0));
        assert_eq!(seg, 0);
        let (seg, _, _) = nearest_segment(&route, step(d, 90.0, 500.0), Some(270.0));
        assert_eq!(seg, 2);
    }

    #[test]
    fn patrol_en_route_leads_in_to_its_first_waypoint() {
        let a: LngLat = [-122.50, 37.80];
        let route = vec![a, step(a, 90.0, 1000.0), step(a, 135.0, 1000.0)];
        let task = ordered(
            TaskKind::Patrol,
            OpState::Active,
            TaskParams {
                route: Some(route.clone()),
                ..TaskParams::default()
            },
        );
        let mut inp = moving(base(Kind::Boat, 6.0, 13.0, "surface"), 0.0, 6.0);
        inp.position = step(a, 180.0, 3000.0); // 3 km south of the route
        inp.mode = Some("AUTO");
        inp.task = Some(&task);
        let e = estimate(&inp);
        let Motion::Route { lead, .. } = &e.motion else { panic!() };
        assert!(lead.is_some_and(|l| distance_m(l, inp.position) < 0.2), "{lead:?}");
        let (p, _) = eval(&e.motion, 100.0, e.reach_mps, e.cap_m);
        assert!(distance_m(p, step(inp.position, 0.0, 600.0)) < 5.0);
        // 3000 m to a, then 600 m along the first leg
        let (p, _) = eval(&e.motion, 600.0, e.reach_mps, e.cap_m);
        assert!(distance_m(p, step(a, 90.0, 600.0)) < 5.0, "{p:?}");
    }

    #[test]
    fn hold_stays_put_and_a_fixed_wing_circles_its_center() {
        let task = ordered(TaskKind::Hold, OpState::Done, TaskParams::default());
        let mut boat = base(Kind::Boat, 6.0, 13.0, "surface");
        boat.mode = Some("LOITER");
        boat.task = Some(&task);
        let e = estimate(&boat);
        assert_eq!(e.basis, "hold");
        let (p, r) = eval(&e.motion, 100.0, e.reach_mps, e.cap_m);
        assert_eq!(p, GATE);
        assert!((radius_of(&r) - 50.0).abs() < 1e-9); // 10 + 10 + 0.3 x 100
        // a plane in LOITER around a fresh navigation target
        let center = step(GATE, 90.0, 150.0);
        let mut plane = moving(base(Kind::Plane, 20.0, 26.0, "air"), 0.0, 20.0);
        plane.mode = Some("LOITER");
        plane.loiter_radius_m = Some(150.0);
        plane.nav_target = Some((center, 999.0));
        plane.task = Some(&task);
        let e = estimate(&plane);
        assert_eq!(e.basis, "hold");
        let Motion::Ring { center: c, radius_m, .. } = &e.motion else { panic!("{:?}", e.motion) };
        assert!(distance_m(*c, center) < 0.2 && *radius_m == 150.0, "{c:?} {radius_m}");
        // the target reported before this mode is the previous mode's
        plane.mode_since = Some(999.5);
        let e = estimate(&plane);
        assert!(matches!(e.motion, Motion::Point { .. }), "{:?}", e.motion);
        assert!(e.detail.contains("center unknown"));
    }

    #[test]
    fn rtl_heads_home_and_a_fixed_wing_circles_there() {
        let mut boat = moving(base(Kind::Boat, 6.0, 13.0, "surface"), 0.0, 6.0);
        boat.mode = Some("RTL");
        let e = estimate(&boat);
        assert_eq!(e.basis, "rtl");
        let home = boat.home.expect("home");
        let dist = distance_m(GATE, home);
        let Motion::Line { stop_m, bearing_deg, .. } = &e.motion else { panic!() };
        assert!(stop_m.is_some_and(|s| (s - dist).abs() < 1.0), "{stop_m:?}");
        assert!((bearing_deg - bearing(GATE, home)).abs() < 0.1);
        // half way after dist / 2 / 6 s; home and stopped long after
        let (p, _) = eval(&e.motion, dist / 12.0, e.reach_mps, e.cap_m);
        assert!((distance_m(p, home) - dist / 2.0).abs() < 2.0);
        let (p, r) = eval(&e.motion, 10_000.0, e.reach_mps, e.cap_m);
        assert!(distance_m(p, home) < 2.0, "{}", distance_m(p, home));
        assert!(matches!(r, Region::Circle { radius_m, .. } if (radius_m - 28.0).abs() < 1e-9), "{r:?}");
        // a plane: stops on its RTL circle and circles home
        let mut plane = moving(base(Kind::Plane, 160.0, 175.0, "air"), 0.0, 160.0);
        plane.mode = Some("RTL");
        plane.rtl_radius_m = Some(1000.0);
        let e = estimate(&plane);
        let (p, r) = eval(&e.motion, 10_000.0, e.reach_mps, e.cap_m);
        assert!((distance_m(p, home) - 1000.0).abs() < 2.0, "{}", distance_m(p, home));
        assert!(matches!(r, Region::Arc { radius_m, spread_deg, .. } if radius_m == 1000.0 && spread_deg == 180.0), "{r:?}");
        // inside its RTL circle (3.1 km from home, a 9 km circle): ArduPlane flies out to the
        // circle (a line away from home, 5.9 km) and circles from there, clockwise
        plane.rtl_radius_m = Some(9000.0);
        let e = estimate(&plane);
        let Motion::Line { bearing_deg, stop_m, arrive: Some(a), .. } = &e.motion else { panic!("{:?}", e.motion) };
        assert!((bearing_deg - bearing(home, GATE)).abs() < 0.1);
        assert!(stop_m.is_some_and(|s| (s - (9000.0 - dist)).abs() < 1.0), "{stop_m:?}");
        assert!(a.ring && a.rate_dps > 0.0 && (a.rate_dps - (160.0f64 / 9000.0).to_degrees()).abs() < 1e-3);
        // 10 s out: 1.6 km along that line
        let (p, _) = eval(&e.motion, 10.0, e.reach_mps, e.cap_m);
        assert!((distance_m(GATE, p) - 1600.0).abs() < 2.0);
        // on the ring at the stop (37 s), then a quarter turn further (88 s) around home
        let t_stop = (9000.0 - dist) / 160.0;
        let (p0, r) = eval(&e.motion, t_stop, e.reach_mps, e.cap_m);
        assert!((distance_m(p0, home) - 9000.0).abs() < 20.0);
        assert!(matches!(r, Region::Ellipse { .. }), "not surely there yet: {r:?}");
        let (p1, _) = eval(&e.motion, t_stop + 88.36, e.reach_mps, e.cap_m);
        assert!((distance_m(p1, home) - 9000.0).abs() < 20.0);
        let turned = (bearing(home, p1) - bearing(home, p0)).rem_euclid(360.0);
        assert!((turned - 90.0).abs() < 1.0, "{turned}");
        // long after: anywhere on the ring
        let (_, r) = eval(&e.motion, 10_000.0, e.reach_mps, e.cap_m);
        assert!(matches!(r, Region::Arc { radius_m, spread_deg, .. } if radius_m == 9000.0 && spread_deg == 180.0), "{r:?}");
    }

    #[test]
    fn the_lost_link_branch_fires_after_the_profiles_timer_and_heads_home() {
        let mut plane = moving(base(Kind::Plane, 90.0, 105.0, "air"), 90.0, 90.0);
        plane.mode = Some("AUTO");
        plane.lost_link = Some(("satcom".into(), 120.0));
        plane.now = 1200.0;
        let e = estimate(&plane);
        let f = e.failsafe.as_ref().expect("a branch");
        assert_eq!((f.action, f.after_s, f.fires_t, f.profile.as_str()), ("rtl", 120.0, 1120.0, "satcom"));
        // 200 s silent: the timer fired 80 s ago, so the branch leads and is evaluated too
        assert!(f.fired && f.now.is_some() && e.leading == "failsafe");
        assert_eq!(f.now.as_ref().map(|n| n.t), Some(1200.0));
        let v = agent_view(&e, 1200.0);
        assert_eq!(v["leading"], "failsafe");
        assert_eq!(v["failsafe"]["fired"], true);
        assert!(v["failsafe"]["now"]["radius_m"].as_f64().is_some_and(|r| r > 0.0));
        assert_eq!(v["task"]["as_of"], 1000.0);
        assert_eq!(v["task"]["age_s"], 200.0);
        assert!(v["note"].as_str().is_some_and(|n| n.contains("not an observed fact")));
        // before the timer: the task model leads
        let mut early = plane.clone();
        early.now = 1047.0;
        let e = estimate(&early);
        let f = e.failsafe.as_ref().expect("a branch");
        assert!(!f.fired && f.now.is_none() && e.leading == "task");
        assert_eq!(agent_view(&e, 1047.0)["failsafe"]["now"], serde_json::Value::Null);
        // the branch starts where the first model had it at 120 s, with its uncertainty then
        let (p120, r120) = eval(&e.motion, 120.0, e.reach_mps, e.cap_m);
        let Motion::Line { from, along0_m, speed_mps, arrive, .. } = &f.motion else { panic!() };
        assert!(distance_m(*from, p120) < 1.0);
        assert!((along0_m - (r120.radius_about(p120) + 10.0)).abs() < 0.01);
        assert_eq!(*speed_mps, 90.0); // cruise, not the last ground speed
        assert!(arrive.as_ref().is_some_and(|a| a.ring));
        // already in RTL, or unarmed: no branch
        plane.mode = Some("RTL");
        assert!(estimate(&plane).failsafe.is_none());
        plane.mode = Some("AUTO");
        plane.armed = false;
        assert!(estimate(&plane).failsafe.is_none());
        // a boat in HOLD does not failsafe (ArduRover); in AUTO it does
        let mut boat = base(Kind::Boat, 6.0, 13.0, "surface");
        boat.mode = Some("HOLD");
        assert!(estimate(&boat).failsafe.is_none());
        boat.mode = Some("AUTO");
        assert_eq!(estimate(&boat).failsafe.map(|f| f.action), Some("rtl"));
        // no timer known: no branch
        boat.lost_link = None;
        assert!(estimate(&boat).failsafe.is_none());
    }

    #[test]
    fn a_line_counts_as_arrived_only_once_a_lagging_vehicle_would_be_there() {
        // 1 km to go at 10 m/s with 100 m of initial uncertainty growing 1 m/s: the point
        // stops at 100 s, the region keeps an ellipse (a vehicle up to `along` behind would
        // still be on its way) until 10 dt >= 1000 + 100 + dt, at 122.2 s
        let m = Motion::Line {
            from: GATE,
            bearing_deg: 0.0,
            speed_mps: 10.0,
            stop_m: Some(1000.0),
            along0_m: 100.0,
            along_mps: 1.0,
            across0_m: 50.0,
            across_mps: 0.0,
            arrive: Some(Arrival {
                center: step(GATE, 0.0, 1000.0),
                radius_m: 15.0,
                ring: false,
                width_m: 0.0,
                rate_dps: 0.0,
            }),
        };
        let (p, r) = eval(&m, 100.0, 100.0, 25_000.0);
        assert!((distance_m(GATE, p) - 1000.0).abs() < 1.0);
        assert!(matches!(r, Region::Ellipse { semi_major_m, .. } if (semi_major_m - 200.0).abs() < 1e-9), "{r:?}");
        let (p, r) = eval(&m, 122.0, 100.0, 25_000.0);
        assert!((distance_m(GATE, p) - 1000.0).abs() < 1.0);
        assert!(matches!(r, Region::Ellipse { .. }), "{r:?}");
        let (_, r) = eval(&m, 123.0, 100.0, 25_000.0);
        assert!(matches!(r, Region::Circle { radius_m, .. } if (radius_m - 115.0).abs() < 1e-9), "{r:?}");
    }

    #[test]
    fn a_branch_ring_keeps_the_sense_of_the_loiter_it_continues() {
        // an RQ-4 loitering counter-clockwise around home (its RTL circle is the same ring):
        // the lost-link branch, judged 120 s on, must not run the other way
        let home: LngLat = [-122.75, 37.75];
        let mut plane = moving(base(Kind::Plane, 160.0, 175.0, "air"), 0.0, 160.0); // north on the east side
        plane.position = step(home, 90.0, 9000.0);
        plane.home = Some(home);
        plane.mode = Some("LOITER");
        plane.loiter_radius_m = Some(9000.0);
        plane.rtl_radius_m = Some(9000.0);
        plane.nav_target = Some((home, 999.0));
        plane.lost_link = Some(("satcom".into(), 120.0));
        plane.now = 1200.0;
        let e = estimate(&plane);
        let Motion::Ring { rate_dps: r0, .. } = e.motion else { panic!("{:?}", e.motion) };
        assert!(r0 < 0.0, "counter-clockwise: {r0}");
        let f = e.failsafe.expect("branch");
        let Motion::Ring { rate_dps: r1, center, .. } = f.motion else { panic!("{:?}", f.motion) };
        assert!(r1 < 0.0 && (r1 - r0).abs() < 1e-9, "{r0} {r1}");
        assert!(distance_m(center, home) < 0.2);
        // the branch continues from where the first model had it when the timer fired
        let (p120, _) = eval(&e.motion, 120.0, e.reach_mps, e.cap_m);
        let (pb, _) = eval(&f.motion, 0.0, e.reach_mps, e.cap_m);
        assert!(distance_m(p120, pb) < 20.0, "{}", distance_m(p120, pb));
        // a ring joined from afar is flown clockwise whatever the old heading
        plane.position = step(home, 90.0, 30_000.0);
        plane.mode = Some("RTL");
        let e = estimate(&plane);
        let Motion::Line { arrive: Some(a), .. } = &e.motion else { panic!("{:?}", e.motion) };
        assert!(a.ring);
    }

    #[test]
    fn an_auv_under_water_is_said_to_be_normal_and_surfaces_on_failsafe() {
        let mut auv = moving(base(Kind::Sub, 2.0, 2.6, "subsurface"), 90.0, 2.0);
        auv.link_class = Some("acoustic");
        auv.report_s = 20.0;
        auv.submerged = true;
        auv.mode = Some("AUTO");
        auv.lost_link = Some(("acoustic".into(), 120.0));
        auv.now = 1100.0;
        let e = estimate(&auv);
        assert!(e.note.as_deref().is_some_and(|n| n.contains("every 20 s")), "{:?}", e.note);
        // s0 allows for where in its 20 s interval it was: 10 + 0.5 x 2 x 20 = 30 m
        let Motion::Line { along0_m, .. } = &e.motion else { panic!() };
        assert_eq!(*along0_m, 30.0);
        let f = e.failsafe.expect("surfaces");
        assert_eq!(f.action, "surface");
        assert!(matches!(f.motion, Motion::Point { rate_mps, .. } if rate_mps == DRIFT_MPS), "{:?}", f.motion);
        // with the water sampled under it: it drifts at the current, growing with it
        auv.water = Some(WaterTrack {
            pos: GATE,
            t: 1000.0,
            sigma_m: 10.0,
            drift_en: [1.8, 0.0],
            current_en: [1.8, 0.0],
            wind_en: [0.0, 0.0],
        });
        let f = estimate(&auv).failsafe.expect("surfaces");
        assert!(matches!(f.motion, Motion::Point { rate_mps, .. } if (rate_mps - (1.8 + 0.54 + 0.1)).abs() < 0.01), "{:?}", f.motion);
        auv.water = None;
        // a tethered ROV disarms instead
        auv.link_class = Some("tether");
        auv.submerged = false;
        auv.report_s = 1.0;
        let e = estimate(&auv);
        assert!(e.note.is_none());
        assert_eq!(e.failsafe.map(|f| f.action), Some("disarm"));
    }

    #[test]
    fn now_is_evaluated_on_a_coarse_step_so_the_push_stays_quiet() {
        let plane = moving(base(Kind::Plane, 160.0, 175.0, "air"), 90.0, 160.0);
        let e47 = estimate(&plane);
        assert_eq!(e47.now.t, 1045.0);
        let mut p2 = plane.clone();
        p2.now = 1049.9;
        assert_eq!(estimate(&p2), e47);
        p2.now = 1050.0;
        let e50 = estimate(&p2);
        assert_eq!(e50.now.t, 1050.0);
        assert!(e50.now.radius_m > e47.now.radius_m);
        assert_eq!(e50.motion, e47.motion);
        // the JSON carries the tag and the model
        let v = serde_json::to_value(&e50).expect("json");
        assert_eq!(v["motion"]["kind"], "line");
        assert_eq!(v["basis"], "velocity");
        assert_eq!(v["failsafe"]["action"], "rtl");
    }

    #[test]
    fn limits_and_timers_parse_the_shipped_tables() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let text = std::fs::read_to_string(root.join("sim/platforms.toml")).expect("catalog");
        let limits = Limits::from_toml(&text).expect("limits");
        let rq4 = &limits.platforms["rq4"];
        assert_eq!((rq4.cruise_mps, rq4.max_mps, rq4.loiter_radius_m), (160.0, 175.0, Some(9000.0)));
        assert_eq!(rq4.domain, "air");
        assert_eq!(rq4.space_class.as_deref(), Some("wgs"));
        let puma = &limits.platforms["puma"];
        assert_eq!(puma.max_mps, 26.0); // a third over cruise
        assert!(limits.platforms["buoy"].drifter);
        let text = std::fs::read_to_string(root.join("sim/links.toml")).expect("links");
        let ll = LostLink::from_toml(&text).expect("timers");
        assert_eq!(ll.for_vehicle(Some("rq4"), Some("satcom"), Some("wgs")), Some(("satcom".into(), 120.0)));
        assert_eq!(ll.for_vehicle(Some("x"), Some("satcom"), Some("ku-geo")), Some(("satcom_ku_geo".into(), 120.0)));
        assert_eq!(ll.for_vehicle(None, Some("satcom"), Some("starlink")), Some(("satcom_leo".into(), 30.0)));
        assert_eq!(ll.for_vehicle(None, Some("los"), None), Some(("los".into(), 5.0)));
        assert_eq!(ll.for_vehicle(None, Some("mesh"), None), Some(("mesh".into(), 10.0)));
        assert_eq!(ll.for_vehicle(None, Some("acoustic"), None), Some(("acoustic".into(), 120.0)));
        assert_eq!(ll.for_vehicle(None, Some("tether"), None), Some(("tether".into(), 5.0)));
        assert_eq!(ll.for_vehicle(None, None, None), None);
        // extends and overrides
        let ll = LostLink::from_toml(
            "[class.a]\nlost_link_s = 7\n[class.b]\nextends = \"a\"\n[platform_profile]\nfoo = \"b\"\n",
        )
        .expect("parse");
        assert_eq!(ll.for_vehicle(Some("foo"), Some("los"), None), Some(("b".into(), 7.0)));
        assert_eq!(ll.for_vehicle(None, Some("los"), None), None);
    }

    #[test]
    fn kind_defaults_cover_a_vehicle_without_a_manifest_entry() {
        assert_eq!(kind_defaults(Some(Kind::Boat)).domain, "surface");
        assert_eq!(kind_defaults(Some(Kind::Tracker)).max_mps, 0.0);
        assert_eq!(cap_for("air"), 25_000.0);
        assert_eq!(cap_for("ground"), 1_000.0);
    }

    #[test]
    fn only_vehicles_past_their_stale_threshold_get_an_estimate() {
        use crate::vehicle::Vehicle;
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let world = World::new(crate::world::Config {
            base_port: 0,
            http_port: 0,
            default_ports: 0,
            manifest: root.join("sim/run/none.json"),
            platforms: root.join("sim/platforms.toml"),
            root: root.clone(),
        });
        let wall = 5000.0;
        let ctx = FrameContext::new(&world, wall);
        let link = |last_rx: f64, every: u32| LinkSnapshot {
            last_rx_t: Some(last_rx),
            loss_pct: None,
            rtt_ms: None,
            expected_interval_ms: every,
            emu: None,
            telemetry: None,
            ack_timeout_ms: 1000,
        };
        let mut v = Vehicle::default();
        v.kind = Some(Kind::Boat);
        v.pos = Some((37.8, -122.5));
        v.armed = true;
        v.seen.position = Some(wall - 10.0);
        // heard 1 s ago: nothing; 10 s ago on a streaming link: an estimate
        assert!(ctx.estimate(1, &v, &link(wall - 1.0, 1000)).is_none());
        let e = ctx.estimate(1, &v, &link(wall - 10.0, 1000)).expect("stale");
        assert_eq!(e.basis, "stationary");
        assert_eq!(e.t0, wall - 10.0);
        // an acoustic AUV reports every 20 s: 10 s of silence is nothing, 70 s is
        assert!(ctx.estimate(1, &v, &link(wall - 10.0, 20_000)).is_none());
        assert!(ctx.estimate(1, &v, &link(wall - 70.0, 20_000)).is_some());
        // never heard, or no position: nothing to estimate from
        assert!(ctx.estimate(1, &v, &LinkSnapshot { last_rx_t: None, ..link(0.0, 1000) }).is_none());
        v.pos = None;
        assert!(ctx.estimate(1, &v, &link(wall - 10.0, 1000)).is_none());
        // the boat's figures come from the kind (no manifest entry): 8 m/s cruise
        v.pos = Some((37.8, -122.5));
        let e = ctx.estimate(1, &v, &link(wall - 10.0, 1000)).expect("stale");
        assert!((e.reach_mps - 10.4).abs() < 1e-9);
        assert_eq!(e.cap_m, 5_000.0);
    }
}
