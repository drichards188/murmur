//! Simple, deterministic motion for contacts: heading-limited turns toward waypoints at a
//! constant speed, circles, station keeping, and drift with the current. Flat-earth
//! integration (`M_PER_DEG`), as the stand-ins do.
//!
//! A node's order ([`crate::node`]) moves a contact instead of its file's motion while it runs:
//! the node sets [`Mover::steer`] (hold, head for a point, a route, an orbit, or back to the
//! file's motion) and the step drives it with the same turn rate and acceleration. Without one
//! the file's motion runs exactly as before.
//!
//! Slowing and stopping (bd murmur-2of4, the contact's [`MotionModel`]). Under the physical model,
//! the default, a contact slows at its kind's `decel_mps2` (sim/contacts.toml, sourced in
//! docs/research/opponent-weapons.md section 6) and speeds up at [`ACCEL_MPS2`]: it begins slowing
//! in time to stand on the last point of its transit, the end of a route it dwells at, or the
//! point an order sends it to ([`Mover::stop_speed`]), so it stops on the point rather than short
//! of it or past it; told to hold, it runs on along its course until it has stopped; its
//! propulsion lost, it coasts down at the same rate before it drifts or stands. Sent to a point it
//! cannot stop on in time, it runs through it and comes back round. A strike run (a node that is
//! its own weapon) runs into its point at speed. An aircraft of a kind that `hovers` stops as a
//! boat does; any other aircraft is a fixed wing: it never stops, and where another would stand
//! (holding, the end of its transit, a dwell) it circles clockwise at the speed it has on its
//! tightest turn (its speed over its turn rate). Under the legacy model (the v1 and v2 headless
//! worlds' and the evaluation stacks' pin, `MURMUR_MOTION_MODEL=legacy`) a contact goes from its
//! speed to a stop in one step, within [`WAYPOINT_RADIUS_M`] of a transit's end, and any aircraft
//! hovers, exactly as before.

use murmur_standins::geo::{Gauss, M_PER_DEG, bearing_deg, distance_m, wrap180};

use crate::damage::DamageState;
use crate::scenario::{Contact, Domain, Motion, MotionModel};

/// Close enough to a waypoint to take the next one.
const WAYPOINT_RADIUS_M: f64 = 25.0;
/// Speed-up from a dwell or spawn, every kind's (unsourced; bd murmur-2of4 gave only slowing its
/// figures).
pub const ACCEL_MPS2: f64 = 0.5;
/// Physical motion: a contact stopping on a point stands on it once the point is within its step
/// and it goes no faster than this many times what its deceleration takes off in a step [E: the
/// rounding of a variable step]; any faster, it runs through and comes back round.
const STAND_STEPS: f64 = 2.0;
/// An orbit steering this far off its circle heads for it first.
const ORBIT_JOIN_M: f64 = 30.0;
/// A node slowing to turn onto a point inside its turning circle goes no slower than this [E]
/// (bd murmur-9js0).
const MIN_TURN_MPS: f64 = 0.5;

/// Where a contact is right now (truth).
#[derive(Debug, Clone)]
pub struct State {
    pub lat: f64,
    pub lon: f64,
    pub alt_m: f64,
    /// Course over ground, degrees true.
    pub course_deg: f64,
    pub speed_mps: f64,
    pub exists: bool,
}

pub struct Mover {
    next_maneuver: usize,
    pub contact: Contact,
    pub st: State,
    /// route: index of the waypoint being steered for
    leg: usize,
    /// route with reverse: -1 heading back toward the first point
    dir: i32,
    /// scenario seconds until a dwell ends
    dwell_left: f64,
    /// transit: done, holding at the end
    holding: bool,
    /// loiter: angle around the centre, degrees
    phase_deg: f64,
    rng: Gauss,
    /// An engagement's abstract effect reached it at this scenario time (unix s).
    pub effect_t: Option<f64>,
    /// Taken out of the scenario by an effect (`on_effect = "out_of_play"`).
    pub out_of_play: bool,
    /// The damage model's worst state put on it so far (none: never damaged), and when.
    pub damage: Option<(DamageState, f64)>,
    /// Its weapon is out of action (a firepower kill): its acts launch nothing.
    pub disarmed: bool,
    /// Its propulsion is lost (a mobility kill): a boat drifts, a ground contact stands, an
    /// aircraft flies on slowed; a node's order can no longer move it (an aircraft: at half speed).
    pub crippled: bool,
    /// What a node's order makes it do instead of its file's motion (none: the file's motion).
    pub steer: Option<Steer>,
    /// Physical motion: a fixed wing standing in the air, circling ([`Mover::stand`]).
    circling: Option<Circle>,
    /// It circled this step (a circle it did not fly this step is dropped).
    circled: bool,
    /// Physical motion: the way it still has of its own after its propulsion was lost (speed,
    /// course), coasting down at its deceleration.
    way: Option<(f64, f64)>,
}

/// A fixed wing's circle where another contact would stand: clockwise round `center` (lat, lon) at
/// `radius_m`, `phase_deg` its bearing from the centre; `anchor` the point it stands at.
#[derive(Debug, Clone, PartialEq)]
struct Circle {
    anchor: (f64, f64),
    center: (f64, f64),
    radius_m: f64,
    speed_mps: f64,
    phase_deg: f64,
}

/// What [`Mover::head`] does at its point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arrive {
    /// A point on the way: within the step's reach it has not moved yet (the caller takes the
    /// next point).
    Pass,
    /// Stand on it, stopped (physical: slowing in time; a fixed wing circles there).
    Stop,
    /// Physical: a strike run, onto it at speed (legacy: as `Stop`).
    Into,
}

/// A node's steering ([`crate::node`]), driven by [`Mover::step`] in place of the file's motion.
#[derive(Debug, Clone, PartialEq)]
pub enum Steer {
    /// Stop where it is and stay there (a boat keeps station against the current: no drift).
    /// Physical: it runs on along its course slowing at its deceleration until it has stopped,
    /// and a fixed wing circles there; legacy: it stops at once and any aircraft hovers.
    Hold,
    /// Head for a point at a speed and stop on it; with `alt_m`, change height in proportion as
    /// it closes (a dive onto its target); with `into`, run into it at speed (a strike run: the
    /// physical model never slows it to stop there).
    To { lat: f64, lon: f64, speed_mps: f64, alt_m: Option<f64>, into: bool },
    /// Once along the points (`[lon, lat]`) at a speed, then hold at the last (`done`).
    Route { points: Vec<[f64; 2]>, speed_mps: f64, leg: usize, done: bool },
    /// Circle a centre (`[lon, lat]`) clockwise, joining the circle at its nearest point first.
    Orbit { center: [f64; 2], radius_m: f64, speed_mps: f64 },
    /// Back to the file's motion: to this point first, then the file's motion from there (a
    /// loiter at this angle round its centre).
    Rejoin { lat: f64, lon: f64, speed_mps: f64, phase_deg: Option<f64> },
}

impl Steer {
    /// A route steering's end reached.
    pub fn done(&self) -> bool {
        matches!(self, Steer::Route { done: true, .. })
    }
}

pub(crate) fn offset(lat: f64, lon: f64, east_m: f64, north_m: f64) -> (f64, f64) {
    (
        lat + north_m / M_PER_DEG,
        lon + east_m / (M_PER_DEG * lat.to_radians().cos()),
    )
}

impl Mover {
    pub fn new(contact: Contact, seed: u64, index: u64) -> Mover {
        let (lon, lat) = match &contact.motion {
            Motion::Recorded { samples, .. } => (samples[0].position[0], samples[0].position[1]),
            Motion::Route { points, .. } | Motion::Transit { points } => {
                (points[0][0], points[0][1])
            }
            Motion::Loiter { center, radius_m } => {
                let (la, lo) = offset(center[1], center[0], 0.0, *radius_m);
                (lo, la)
            }
            Motion::Station { at } | Motion::Drift { at } => (at[0], at[1]),
        };
        let course = match &contact.motion {
            Motion::Route { points, .. } | Motion::Transit { points } => {
                bearing_deg(points[0][1], points[0][0], points[1][1], points[1][0])
            }
            Motion::Loiter { .. } => 90.0,
            _ => 0.0,
        };
        let alt_m = contact.alt_m;
        Mover {
            next_maneuver: 0,
            st: State {
                lat,
                lon,
                alt_m,
                course_deg: course,
                speed_mps: 0.0,
                exists: false,
            },
            leg: 1,
            dir: 1,
            dwell_left: 0.0,
            holding: false,
            phase_deg: 0.0,
            rng: Gauss::new(seed.wrapping_mul(0x9E37_79B9).wrapping_add(index + 1)),
            effect_t: None,
            out_of_play: false,
            damage: None,
            disarmed: false,
            crippled: false,
            steer: None,
            circling: None,
            circled: false,
            way: None,
            contact,
        }
    }

    /// The steering that takes it back to its file's motion after an order moved it, at
    /// `speed_mps`: to a station's point, a finished transit's last point, or the nearest point
    /// of a loiter's circle (and on round it from that angle); none where the file's motion
    /// carries on from anywhere (a route, a transit under way, a drift).
    pub fn rejoin(&self, speed_mps: f64) -> Option<Steer> {
        match &self.contact.motion {
            Motion::Station { at } => Some(Steer::Rejoin { lat: at[1], lon: at[0], speed_mps, phase_deg: None }),
            Motion::Transit { points } if self.holding => {
                let p = points[points.len() - 1];
                Some(Steer::Rejoin { lat: p[1], lon: p[0], speed_mps, phase_deg: None })
            }
            Motion::Loiter { center, radius_m } => {
                let b = bearing_deg(center[1], center[0], self.st.lat, self.st.lon);
                let a = b.to_radians();
                let (lat, lon) = offset(center[1], center[0], radius_m * a.sin(), radius_m * a.cos());
                Some(Steer::Rejoin { lat, lon, speed_mps, phase_deg: Some(b) })
            }
            _ => None,
        }
    }

    /// The damage model's `state` put on it at scenario time `now` (unix s): destroyed takes it
    /// out of play (as an abstract effect does); its propulsion lost stops it where it is (a boat
    /// or a submerged contact drifts with the water from there, a ground contact stands, an
    /// aircraft flies on at half its speed [E: slowed, as a damaged engine]; physical motion: a
    /// boat, a submerged or a ground contact coasts down from its way first); its weapons out of
    /// action silence its acts' launches. A state no worse than one it has changes nothing.
    /// Returns whether it changed anything.
    pub fn damage(&mut self, state: DamageState, now: f64) -> bool {
        if state == DamageState::Undamaged {
            return false;
        }
        let worse = match self.damage {
            Some((DamageState::Destroyed, _)) => false,
            Some((had, _)) if had == state => false,
            _ => true,
        };
        if !worse {
            return false;
        }
        match state {
            DamageState::Undamaged => {}
            DamageState::Destroyed => {
                self.out_of_play = true;
                self.st.exists = false;
            }
            DamageState::Mobility => {
                self.crippled = true;
                if self.contact.domain != Domain::Air {
                    self.steer = None;
                }
                self.contact.maneuvers.clear();
                self.next_maneuver = 0;
                let at = [self.st.lon, self.st.lat];
                match self.contact.domain {
                    Domain::Air => self.contact.speed_mps *= 0.5,
                    Domain::Ground => self.contact.motion = Motion::Station { at },
                    Domain::Surface | Domain::Subsurface => self.contact.motion = Motion::Drift { at },
                }
                if self.contact.domain != Domain::Air {
                    if self.physical() {
                        // it coasts down from the way it has (Station and Drift carry it)
                        self.way = (self.st.speed_mps > 0.0).then_some((self.st.speed_mps, self.st.course_deg));
                    } else {
                        self.st.speed_mps = 0.0;
                    }
                    self.holding = false;
                    self.dwell_left = 0.0;
                }
            }
            DamageState::Firepower => self.disarmed = true,
        }
        // the latest (a mobility kill then a firepower kill leaves it both stopped and disarmed)
        self.damage = Some((state, now));
        true
    }

    /// Advance by `dt` scenario seconds at scenario time `s` (seconds since the scenario
    /// start). `current` is the water's (east, north) velocity in m/s at the contact, if
    /// known. Returns false while the contact doesn't exist (before `start_s`, after `end_s`).
    pub fn step(&mut self, s: f64, dt: f64, current: Option<(f64, f64)>) -> bool {
        if self.contact.maneuvers.get(self.next_maneuver).is_none() {
            return self.advance(s, dt, current);
        }
        let mut cursor = s - dt.max(0.0);
        while let Some(m) = self.contact.maneuvers.get(self.next_maneuver).cloned() {
            if m.at_s > s {
                break;
            }
            self.advance(m.at_s.max(cursor), (m.at_s - cursor).max(0.0), current);
            cursor = cursor.max(m.at_s);
            self.contact.motion = Motion::Transit { points: m.route };
            self.contact.speed_mps = m.speed_mps;
            self.leg = 0;
            self.holding = false;
            self.dwell_left = 0.0;
            self.next_maneuver += 1;
        }
        self.advance(s, (s - cursor).max(0.0), current)
    }

    fn advance(&mut self, s: f64, dt: f64, current: Option<(f64, f64)>) -> bool {
        let c = &self.contact;
        self.st.exists = !self.out_of_play && s >= c.start_s && c.end_s.is_none_or(|e| s < e);
        // Recorded motion reads the absolute scenario clock, even on a zero-length step.
        // It must neither accumulate dt error nor bridge missing observations.
        if let Motion::Recorded { samples, max_gap_s } = &c.motion {
            if !self.st.exists || !s.is_finite() {
                self.st.exists = false;
                return false;
            }
            let next = samples.partition_point(|p| p.at_s <= s);
            if next == 0 {
                self.st.exists = false;
                return false;
            }
            let a = &samples[next - 1];
            let b = samples.get(next).filter(|b| b.at_s - a.at_s <= *max_gap_s);
            if let Some(b) = b {
                let span = b.at_s - a.at_s;
                let f = (s - a.at_s) / span;
                self.st.lon = wrap180(a.position[0] + wrap180(b.position[0] - a.position[0]) * f);
                self.st.lat = a.position[1] + (b.position[1] - a.position[1]) * f;
                let speed = distance_m(a.position[1], a.position[0], b.position[1], b.position[0]) / span;
                let course = bearing_deg(a.position[1], a.position[0], b.position[1], b.position[0]);
                let va = a.speed_mps.unwrap_or(speed);
                let vb = b.speed_mps.unwrap_or(speed);
                self.st.speed_mps = va + (vb - va) * f;
                let ca = a.course_deg.unwrap_or(course);
                let cb = b.course_deg.unwrap_or(course);
                self.st.course_deg = (ca + wrap180(cb - ca) * f).rem_euclid(360.0);
            } else if s - a.at_s < 1.0 {
                self.st.lon = a.position[0];
                self.st.lat = a.position[1];
                self.st.speed_mps = a.speed_mps.unwrap_or(0.0);
                self.st.course_deg = a.course_deg.unwrap_or(0.0);
            } else {
                self.st.exists = false;
            }
            return self.st.exists;
        }
        if !self.st.exists || dt <= 0.0 {
            return self.st.exists;
        }
        self.circled = false;
        // a node's order steers it instead of its file's motion (crate::node)
        if let Some(steer) = self.steer.take() {
            self.steer = self.drive(steer, dt);
        } else {
            self.file_motion(dt, current);
        }
        // a fixed wing's circle lasts while it stands
        if !self.circled {
            self.circling = None;
        }
        true
    }

    /// One step of its file's motion.
    fn file_motion(&mut self, dt: f64, current: Option<(f64, f64)>) {
        let c = &self.contact;
        match &c.motion {
            Motion::Recorded { .. } => unreachable!("recorded motion is evaluated by timestamp above"),
            Motion::Route {
                points,
                reverse,
                dwell_s,
            } => {
                let points = points.clone();
                let (reverse, dwell_s) = (*reverse, *dwell_s);
                self.follow(&points, dt, Some((reverse, dwell_s)));
            }
            Motion::Transit { points } => {
                let points = points.clone();
                if self.holding {
                    self.stand(dt);
                } else {
                    self.follow(&points, dt, None);
                }
            }
            Motion::Loiter { center, radius_m } => {
                let (center, radius_m) = (*center, *radius_m);
                let speed = self.accelerate(c.speed_mps, dt);
                // clockwise, as a loitering aircraft or a patrolling boat would circle
                self.phase_deg = (self.phase_deg + (speed / radius_m).to_degrees() * dt) % 360.0;
                let a = self.phase_deg.to_radians();
                let (lat, lon) = offset(center[1], center[0], radius_m * a.sin(), radius_m * a.cos());
                self.st.course_deg = (self.phase_deg + 90.0).rem_euclid(360.0);
                self.st.lat = lat;
                self.st.lon = lon;
            }
            Motion::Station { .. } => match self.coast(dt) {
                Some((v, course)) => self.run_along(v, course, dt),
                None => self.stand(dt),
            },
            Motion::Drift { .. } => {
                let (ce, cn) = current.unwrap_or((0.0, 0.0));
                let mut e = ce + self.rng.sample(0.0, 0.02);
                let mut n = cn + self.rng.sample(0.0, 0.02);
                // the way it still has through the water, coasting down (physical motion)
                if let Some((v, course)) = self.coast(dt) {
                    let a = course.to_radians();
                    e += v * a.sin();
                    n += v * a.cos();
                }
                let (lat, lon) = offset(self.st.lat, self.st.lon, e * dt, n * dt);
                self.st.lat = lat;
                self.st.lon = lon;
                self.st.speed_mps = e.hypot(n);
                if self.st.speed_mps > 0.05 {
                    self.st.course_deg = e.atan2(n).to_degrees().rem_euclid(360.0);
                }
            }
        }
    }

    /// The contact's motion model is the physical one (bd murmur-2of4).
    fn physical(&self) -> bool {
        self.contact.motion_model == MotionModel::Physical
    }

    /// Physical motion and a fixed-wing aircraft: it never stops.
    fn fixed_wing(&self) -> bool {
        self.physical() && self.contact.domain == Domain::Air && !self.contact.hovers
    }

    /// Physical motion and anything but a fixed wing: it slows in time to stand on its points.
    fn brakes(&self) -> bool {
        self.physical() && !self.fixed_wing()
    }

    /// The fastest it may go `d` metres short of a point it is to stand on, so that slowing at its
    /// deceleration a step of `dt` at a time it stops on the point: the speed `v` from which the
    /// steps at `v`, `v - b dt`, ..., `b dt` cover `d`, v (v + b dt) / 2b = d.
    pub fn stop_speed(&self, d: f64, dt: f64) -> f64 {
        let b = self.contact.decel_mps2;
        let bt = b * dt;
        0.5 * ((bt * bt + 8.0 * b * d.max(0.0)).sqrt() - bt)
    }

    /// Its speed a step of `dt` on from `from` toward `to`: up at [`ACCEL_MPS2`], down at its
    /// deceleration (physical motion).
    fn approach(&self, from: f64, to: f64, dt: f64) -> f64 {
        if to >= from {
            (from + ACCEL_MPS2 * dt).min(to)
        } else {
            (from - self.contact.decel_mps2 * dt).max(to)
        }
    }

    /// Straight on along `course` at `v` for `dt`.
    fn run_along(&mut self, v: f64, course: f64, dt: f64) {
        self.st.speed_mps = v;
        self.st.course_deg = course;
        let a = course.to_radians();
        let (lat, lon) = offset(self.st.lat, self.st.lon, v * a.sin() * dt, v * a.cos() * dt);
        self.st.lat = lat;
        self.st.lon = lon;
    }

    /// The way it still has after its propulsion was lost, this step's (none: none left).
    fn coast(&mut self, dt: f64) -> Option<(f64, f64)> {
        let (v, course) = self.way?;
        let v = (v - self.contact.decel_mps2 * dt).max(0.0);
        self.way = (v > 0.0).then_some((v, course));
        self.way
    }

    /// Where it would stand still: stopped; a fixed wing with way on circles there instead (an
    /// aircraft with none, one that has struck or never flown, stands).
    fn stand(&mut self, dt: f64) {
        if !self.fixed_wing() || (self.circling.is_none() && self.st.speed_mps <= 0.0) {
            self.st.speed_mps = 0.0;
            return;
        }
        if self.circling.is_none() {
            self.begin_circle((self.st.lat, self.st.lon));
        }
        self.circled = true;
        let Some(c) = self.circling.clone() else { return };
        let v = self.approach(self.st.speed_mps, c.speed_mps, dt);
        let phase = (c.phase_deg + (v / c.radius_m).to_degrees() * dt).rem_euclid(360.0);
        let a = phase.to_radians();
        let (lat, lon) = offset(c.center.0, c.center.1, c.radius_m * a.sin(), c.radius_m * a.cos());
        self.st.lat = lat;
        self.st.lon = lon;
        self.st.course_deg = (phase + 90.0).rem_euclid(360.0);
        self.st.speed_mps = v;
        self.circling = Some(Circle { phase_deg: phase, ..c });
    }

    /// A fixed wing's circle from here, standing at `anchor`: clockwise, tangent to its course,
    /// at its speed on its tightest turn (radius its speed over its turn rate).
    fn begin_circle(&mut self, anchor: (f64, f64)) {
        let v = self.st.speed_mps;
        let r = (v / self.contact.turn_dps.to_radians()).max(1.0);
        let a = (self.st.course_deg + 90.0).to_radians();
        let center = offset(self.st.lat, self.st.lon, r * a.sin(), r * a.cos());
        let phase_deg = (self.st.course_deg - 90.0).rem_euclid(360.0);
        self.circling = Some(Circle { anchor, center, radius_m: r, speed_mps: v, phase_deg });
        self.circled = true;
    }

    /// Hold where it is (physical): on along its course, slowing at its deceleration, to a stop;
    /// a fixed wing circles.
    fn hold(&mut self, dt: f64) {
        if self.fixed_wing() {
            return self.stand(dt);
        }
        let v = (self.st.speed_mps - self.contact.decel_mps2 * dt).max(0.0);
        if v > 0.0 {
            self.run_along(v, self.st.course_deg, dt);
        } else {
            self.st.speed_mps = 0.0;
        }
    }

    /// One step of a node's steering; what it steers by next (none: back on the file's motion).
    /// Its propulsion damaged (only an aircraft is still steered), at half the speed it is told
    /// ([`Mover::told`]).
    fn drive(&mut self, steer: Steer, dt: f64) -> Option<Steer> {
        let stop_at = |last: bool| if last { Arrive::Stop } else { Arrive::Pass };
        match steer {
            Steer::Hold => {
                if self.physical() {
                    self.hold(dt);
                } else {
                    self.st.speed_mps = 0.0;
                }
                Some(Steer::Hold)
            }
            Steer::To { lat, lon, speed_mps, alt_m, into } => {
                let arrive = if into { Arrive::Into } else { Arrive::Stop };
                self.head(lat, lon, self.told(speed_mps), alt_m, dt, arrive);
                Some(Steer::To { lat, lon, speed_mps, alt_m, into })
            }
            Steer::Route { points, speed_mps, mut leg, mut done } => {
                if done {
                    self.stand(dt);
                } else {
                    let last = points.len() - 1;
                    let wp = points[leg];
                    if self.head(wp[1], wp[0], self.told(speed_mps), None, dt, stop_at(leg == last)) {
                        if leg == last {
                            done = true;
                        } else {
                            leg += 1;
                            let wp = points[leg];
                            self.head(wp[1], wp[0], self.told(speed_mps), None, dt, stop_at(leg == last));
                        }
                    }
                }
                Some(Steer::Route { points, speed_mps, leg, done })
            }
            Steer::Orbit { center, radius_m, speed_mps } => {
                let d = distance_m(self.st.lat, self.st.lon, center[1], center[0]);
                let b = if d > 1.0 { bearing_deg(center[1], center[0], self.st.lat, self.st.lon) } else { self.st.course_deg };
                if (d - radius_m).abs() > ORBIT_JOIN_M {
                    let a = b.to_radians();
                    let (lat, lon) = offset(center[1], center[0], radius_m * a.sin(), radius_m * a.cos());
                    self.head(lat, lon, self.told(speed_mps), None, dt, Arrive::Pass);
                } else {
                    // clockwise along the circle, as a loiter
                    let v = self.accelerate(self.told(speed_mps), dt);
                    let to = b + (v / radius_m).to_degrees() * dt;
                    let a = to.to_radians();
                    let (lat, lon) = offset(center[1], center[0], radius_m * a.sin(), radius_m * a.cos());
                    self.st.lat = lat;
                    self.st.lon = lon;
                    self.st.course_deg = (to + 90.0).rem_euclid(360.0);
                }
                Some(Steer::Orbit { center, radius_m, speed_mps })
            }
            Steer::Rejoin { lat, lon, speed_mps, phase_deg } => {
                // physical: a loiter's circle is joined at speed, not stopped on
                let arrive = if phase_deg.is_some() && self.physical() { Arrive::Pass } else { Arrive::Stop };
                if self.head(lat, lon, self.told(speed_mps), None, dt, arrive) {
                    if let Some(p) = phase_deg {
                        self.phase_deg = p;
                    }
                    return None;
                }
                Some(Steer::Rejoin { lat, lon, speed_mps, phase_deg })
            }
        }
    }

    /// Steer for a point with the kind's turn rate at up to `speed` (what it makes good: a
    /// steering's speed through [`Mover::told`]); with `alt`, the height goes to it in proportion
    /// to the ground covered. Returns whether it is there ([`Arrive`]). Physical motion
    /// ([`Mover::head_physical`]): it slows in time to stand on a point it stops at. Legacy:
    /// within this step's reach it is there, standing on the point stopped (`Stop`, `Into`) or
    /// not moved yet (`Pass`).
    fn head(&mut self, lat: f64, lon: f64, speed: f64, alt: Option<f64>, dt: f64, arrive: Arrive) -> bool {
        if self.physical() {
            return self.head_physical(lat, lon, speed, alt, dt, arrive);
        }
        let stop = arrive != Arrive::Pass;
        let d = distance_m(self.st.lat, self.st.lon, lat, lon);
        let want = bearing_deg(self.st.lat, self.st.lon, lat, lon);
        let mut v = (self.st.speed_mps + ACCEL_MPS2 * dt).min(speed).max(0.0);
        // bd murmur-9js0: a point inside its turning circle is never reached at speed (it circles
        // it for ever): slow so the circle through here, tangent to its course, that passes through
        // the point (radius d / (2 sin off)) is one its turn rate can fly
        let off = wrap180(want - self.st.course_deg).abs().to_radians();
        if off > 1e-6 {
            let fits = self.contact.turn_dps.to_radians() * d / (2.0 * off.sin().max(1e-6));
            v = v.min(fits.max(MIN_TURN_MPS));
        }
        if d <= (v * dt).max(1.0) {
            if stop {
                self.st.lat = lat;
                self.st.lon = lon;
                self.st.speed_mps = 0.0;
                if let Some(a) = alt {
                    self.st.alt_m = a;
                }
            }
            return true;
        }
        let max_turn = self.contact.turn_dps * dt;
        let delta = wrap180(want - self.st.course_deg).clamp(-max_turn, max_turn);
        self.st.course_deg = (self.st.course_deg + delta).rem_euclid(360.0);
        self.st.speed_mps = v;
        let a = self.st.course_deg.to_radians();
        let (nlat, nlon) = offset(self.st.lat, self.st.lon, v * a.sin() * dt, v * a.cos() * dt);
        self.st.lat = nlat;
        self.st.lon = nlon;
        if let Some(to) = alt {
            self.st.alt_m += (to - self.st.alt_m) * (v * dt / d).min(1.0);
        }
        false
    }

    /// [`Mover::head`] under the physical model. Its speed goes toward `speed` at
    /// [`Mover::approach`]'s rates, no faster than the turning circle allows (bd murmur-9js0) and,
    /// at a point it stops on, than [`Mover::stop_speed`] so that it stands on it. `Stop`: it
    /// stands on the point once it is within the step and it is slow enough to stop there
    /// ([`STAND_STEPS`]); faster, it runs through and comes back round; a fixed wing does not slow,
    /// and over its point circles there ([`Mover::stand`]). `Into`: onto the point at speed (a
    /// strike run). `Pass`: within the step's reach, not moved yet.
    fn head_physical(&mut self, lat: f64, lon: f64, speed: f64, alt: Option<f64>, dt: f64, arrive: Arrive) -> bool {
        let fixed = self.fixed_wing();
        if arrive == Arrive::Stop
            && fixed
            && self.circling.as_ref().is_some_and(|c| distance_m(c.anchor.0, c.anchor.1, lat, lon) < 1.0)
        {
            self.stand(dt);
            return true;
        }
        let stops = arrive == Arrive::Stop && !fixed;
        let d = distance_m(self.st.lat, self.st.lon, lat, lon);
        let want = bearing_deg(self.st.lat, self.st.lon, lat, lon);
        let mut cap = speed;
        let off = wrap180(want - self.st.course_deg).abs().to_radians();
        if off > 1e-6 {
            let fits = self.contact.turn_dps.to_radians() * d / (2.0 * off.sin().max(1e-6));
            cap = cap.min(fits.max(MIN_TURN_MPS));
        }
        if stops {
            cap = cap.min(self.stop_speed(d, dt));
        }
        let v = self.approach(self.st.speed_mps, cap, dt).max(0.0);
        if d <= (v * dt).max(1.0) {
            match arrive {
                Arrive::Pass => return true,
                Arrive::Stop if !fixed => {
                    if v <= STAND_STEPS * self.contact.decel_mps2 * dt {
                        self.st.lat = lat;
                        self.st.lon = lon;
                        self.st.speed_mps = 0.0;
                        if let Some(a) = alt {
                            self.st.alt_m = a;
                        }
                        return true;
                    }
                    // too fast to stand on it: on through it, slowing, and back round
                }
                _ => {
                    // a strike run into its point, or a fixed wing over it: there, at speed
                    self.st.lat = lat;
                    self.st.lon = lon;
                    self.st.speed_mps = v;
                    if let Some(a) = alt {
                        self.st.alt_m = a;
                    }
                    if fixed && arrive == Arrive::Stop {
                        self.begin_circle((lat, lon));
                    }
                    return true;
                }
            }
        }
        let max_turn = self.contact.turn_dps * dt;
        let delta = wrap180(want - self.st.course_deg).clamp(-max_turn, max_turn);
        self.st.course_deg = (self.st.course_deg + delta).rem_euclid(360.0);
        self.st.speed_mps = v;
        let a = self.st.course_deg.to_radians();
        let (nlat, nlon) = offset(self.st.lat, self.st.lon, v * a.sin() * dt, v * a.cos() * dt);
        self.st.lat = nlat;
        self.st.lon = nlon;
        if let Some(to) = alt
            && d > 0.0
        {
            self.st.alt_m += (to - self.st.alt_m) * (v * dt / d).min(1.0);
        }
        false
    }

    /// The speed a steering's `speed` makes good: half with its propulsion damaged (an aircraft
    /// flies on slowed, as its file's motion does).
    fn told(&self, speed: f64) -> f64 {
        if self.crippled { speed * 0.5 } else { speed }
    }

    /// Its speed toward `target` (physical motion: down at its deceleration; legacy: down at once).
    fn accelerate(&mut self, target: f64, dt: f64) -> f64 {
        self.st.speed_mps = if self.physical() {
            self.approach(self.st.speed_mps, target, dt)
        } else {
            (self.st.speed_mps + ACCEL_MPS2 * dt).min(target)
        };
        self.st.speed_mps
    }

    /// The point it steers for now is one it stands on (physical motion): a transit's last, or the
    /// end of a route it dwells at.
    fn stops_at(&self, n: usize, route: Option<(bool, f64)>) -> bool {
        let last = n - 1;
        match route {
            None => self.leg == last,
            Some((_, dwell_s)) if dwell_s <= 0.0 => false,
            Some((true, _)) => (self.dir > 0 && self.leg == last) || (self.dir < 0 && self.leg == 0),
            Some((false, _)) => self.leg == last,
        }
    }

    /// Stopped on a point it stands on ([`Mover::stops_at`]): a transit holds there; a route
    /// dwells and takes its next leg (back the other way, or round to the first).
    fn reached(&mut self, n: usize, route: Option<(bool, f64)>) {
        let last = n - 1;
        match route {
            None => self.holding = true,
            Some((reverse, dwell_s)) => {
                if reverse {
                    self.dir = -self.dir;
                    self.leg = (self.leg as i32 + self.dir).clamp(0, last as i32) as usize;
                } else {
                    self.leg = (self.leg + 1) % n;
                }
                self.dwell_left = dwell_s;
            }
        }
    }

    /// Steer for the current waypoint with the kind's turn rate; at the end, loop, bounce
    /// (after a dwell) or hold. Physical motion: it slows in time to stand on a point it stops at
    /// ([`Mover::stops_at`]), and a fixed wing circles where it would stand.
    fn follow(&mut self, points: &[[f64; 2]], dt: f64, route: Option<(bool, f64)>) {
        if self.dwell_left > 0.0 {
            self.dwell_left -= dt;
            self.stand(dt);
            return;
        }
        if self.brakes() && self.stops_at(points.len(), route) {
            let wp = points[self.leg];
            if self.head(wp[1], wp[0], self.contact.speed_mps, None, dt, Arrive::Stop) {
                self.reached(points.len(), route);
            }
            return;
        }
        let wp = points[self.leg];
        let dist = distance_m(self.st.lat, self.st.lon, wp[1], wp[0]);
        if dist <= WAYPOINT_RADIUS_M.max(self.st.speed_mps * dt) {
            let last = points.len() - 1;
            match route {
                None => {
                    if self.leg == last {
                        self.holding = true;
                        self.stand(dt);
                        return;
                    }
                    self.leg += 1;
                }
                Some((reverse, dwell_s)) => {
                    let at_end = (self.dir > 0 && self.leg == last) || (self.dir < 0 && self.leg == 0);
                    if reverse {
                        if at_end {
                            self.dir = -self.dir;
                            self.dwell_left = dwell_s;
                        }
                        self.leg = (self.leg as i32 + self.dir).clamp(0, last as i32) as usize;
                    } else {
                        if self.leg == last {
                            self.dwell_left = dwell_s;
                        }
                        self.leg = (self.leg + 1) % points.len();
                    }
                    if self.dwell_left > 0.0 {
                        self.stand(dt);
                        return;
                    }
                }
            }
        }
        let wp = points[self.leg];
        let want = bearing_deg(self.st.lat, self.st.lon, wp[1], wp[0]);
        let max_turn = self.contact.turn_dps * dt;
        let delta = wrap180(want - self.st.course_deg).clamp(-max_turn, max_turn);
        self.st.course_deg = (self.st.course_deg + delta).rem_euclid(360.0);
        let speed = self.accelerate(self.contact.speed_mps, dt);
        let a = self.st.course_deg.to_radians();
        let (lat, lon) = offset(self.st.lat, self.st.lon, speed * a.sin() * dt, speed * a.cos() * dt);
        self.st.lat = lat;
        self.st.lon = lon;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::{Affiliation, Domain, RecordedPoint, Scenario};

    fn recorded() -> Mover {
        let sample = |at_s, lon, course| RecordedPoint {
            at_s, position: [lon, 37.8], speed_mps: Some(4.0), course_deg: Some(course),
        };
        Mover::new(contact(Motion::Recorded {
            samples: vec![sample(10.0, -122.5, 350.0), sample(70.0, -122.49, 10.0),
                          sample(500.0, -122.4, 90.0)],
            max_gap_s: 300.0,
        }, 0.0), 1, 0)
    }

    #[test]
    fn recorded_motion_uses_timestamps_and_preserves_gaps() {
        let mut m = recorded();
        assert!(!m.step(9.9, 1.0, None));
        assert!(m.step(10.0, 0.0, None));
        assert_eq!(m.st.lon, -122.5);
        assert!(m.step(40.0, 0.0, Some((100.0, 100.0))));
        assert!((m.st.lon + 122.495).abs() < 1e-10);
        assert!(m.st.course_deg.abs() < 1e-10, "course takes the short arc through north");
        assert_eq!(m.st.speed_mps, 4.0);
        let mut jumped = recorded();
        assert!(jumped.step(40.0, 1000.0, None));
        assert_eq!(jumped.st.lon, m.st.lon, "rate and step history cannot move a recorded fix");
        assert!(m.step(70.5, 1.0, None));
        assert_eq!(m.st.lon, -122.49);
        assert!(!m.step(71.0, 1.0, None));
        assert!(!m.step(499.9, 1.0, None));
        assert!(m.step(500.0, 1.0, None));
        assert_eq!(m.st.lon, -122.4);
        assert!(!m.step(501.0, 1.0, None));
        assert!(!m.step(3600.0, 1.0, None), "never loops or extrapolates");
        m.out_of_play = true;
        assert!(!m.step(40.0, 1.0, None), "effects still remove a recorded contact");
    }

    #[test]
    fn recorded_missing_velocity_is_inferred_only_on_supported_segments() {
        let mut m = recorded();
        if let Motion::Recorded { samples, .. } = &mut m.contact.motion {
            for p in samples { p.speed_mps = None; p.course_deg = None; }
        }
        assert!(m.step(40.0, 1.0, None));
        assert!(m.st.speed_mps > 10.0 && m.st.speed_mps < 20.0);
        assert!((m.st.course_deg - 90.0).abs() < 0.01);
        assert!(m.step(500.0, 1.0, None));
        assert_eq!(m.st.speed_mps, 0.0);
        m.contact.start_s = 50.0;
        assert!(!m.step(40.0, 1.0, None));
        m.contact.end_s = Some(500.0);
        assert!(!m.step(500.0, 1.0, None));
    }

    #[test]
    fn bundled_morning_watch_replays_every_source_fix_without_looping() {
        let scenario = Scenario::from_toml(include_str!("../../../sim/scenarios/morning-harbor-watch.toml"))
            .expect("bundled recording parses");
        let contacts = scenario.contacts().expect("bundled contacts validate");
        assert_eq!(contacts.len(), 109);
        assert!(scenario.acts.is_empty());
        let mut count = 0;
        for c in contacts {
            assert_eq!(c.affiliation, Affiliation::Neutral);
            let Motion::Recorded { samples, .. } = &c.motion else { panic!("recorded contacts only") };
            let samples = samples.clone();
            let mut m = Mover::new(c, 11, 0);
            for p in samples {
                assert!(m.step(p.at_s, 0.0, None));
                assert!((m.st.lon - p.position[0]).abs() < 1e-10);
                assert!((m.st.lat - p.position[1]).abs() < 1e-10);
                count += 1;
            }
            assert!(!m.step(3600.0, 1.0, None));
        }
        assert_eq!(count, 2126);
    }

    fn contact(motion: Motion, speed: f64) -> Contact {
        Contact {
            maneuvers: Vec::new(),
            id: "c".into(),
            name: "c".into(),
            affiliation: Affiliation::Neutral,
            domain: Domain::Surface,
            kind: "ferry".into(),
            variant: None,
            ais: None,
            emits: true,
            signature: 1.0,
            speed_mps: speed,
            alt_m: 0.0,
            turn_dps: 30.0,
            decel_mps2: 1.0,
            hovers: false,
            motion_model: MotionModel::Physical,
            motion,
            start_s: 0.0,
            end_s: None,
            on_effect: crate::scenario::OnEffect::OutOfPlay,
            strike_outcome: None,
            weapon: None,
        }
    }

    /// bd murmur-9js0: a fishing vessel at 4 m/s turning 4 degrees a second (a 57 m turning
    /// radius) told to go to a point 88 m abeam reaches it (it slows to turn onto it) instead of
    /// circling it for ever; a point well off its turning circle it takes at full speed.
    #[test]
    fn a_point_inside_the_turning_circle_is_reached() {
        let at = [-122.45, 37.80];
        let mut m = Mover::new(contact(Motion::Station { at }, 0.0), 1, 0);
        m.contact.turn_dps = 4.0;
        m.st.course_deg = 0.0;
        m.st.speed_mps = 4.0;
        let (lat, lon) = offset(at[1], at[0], 88.0, 0.0);
        m.steer = Some(Steer::To { lat, lon, speed_mps: 4.0, alt_m: None, into: false });
        let mut slowest: f64 = 4.0;
        let there = (1..=120).find(|s| {
            m.step(f64::from(*s), 1.0, None);
            slowest = slowest.min(m.st.speed_mps);
            distance_m(m.st.lat, m.st.lon, lat, lon) < 1.0
        });
        assert!(there.is_some_and(|s| s < 90), "{there:?}, {:?}", m.st);
        assert!(slowest < 4.0);
        // far ahead and a little off its course: no slowing
        let mut m = Mover::new(contact(Motion::Station { at }, 0.0), 1, 0);
        m.contact.turn_dps = 4.0;
        m.st.speed_mps = 4.0;
        let (lat, lon) = offset(at[1], at[0], 200.0, 2000.0);
        m.steer = Some(Steer::To { lat, lon, speed_mps: 4.0, alt_m: None, into: false });
        for s in 1..=10 {
            m.step(f64::from(s), 1.0, None);
            assert_eq!(m.st.speed_mps, 4.0);
        }
    }

    #[test]
    fn route_reaches_waypoints_and_bounces() {
        // 1 km north, back and forth with a 10 s dwell
        let a = [-122.45, 37.80];
        let b = [-122.45, 37.809];
        let mut m = Mover::new(
            contact(
                Motion::Route {
                    points: vec![a, b],
                    reverse: true,
                    dwell_s: 10.0,
                },
                10.0,
            ),
            1,
            0,
        );
        let mut s = 0.0;
        let mut reached_b = None;
        for _ in 0..400 {
            m.step(s, 1.0, None);
            s += 1.0;
            if reached_b.is_none() && distance_m(m.st.lat, m.st.lon, b[1], b[0]) < 30.0 {
                reached_b = Some(s);
            }
        }
        let t = reached_b.expect("reached the far end");
        assert!((95.0..=130.0).contains(&t), "{t}");
        // and came back: after the dwell it heads south
        assert!(distance_m(m.st.lat, m.st.lon, a[1], a[0]) < 400.0 || m.st.course_deg > 170.0);
    }

    #[test]
    fn transit_holds_at_the_end_and_loiter_stays_on_its_circle() {
        let mut m = Mover::new(
            contact(
                Motion::Transit {
                    points: vec![[-122.45, 37.80], [-122.45, 37.802]],
                },
                10.0,
            ),
            1,
            0,
        );
        for i in 0..120 {
            m.step(f64::from(i), 1.0, None);
        }
        assert_eq!(m.st.speed_mps, 0.0);
        assert!(distance_m(m.st.lat, m.st.lon, 37.802, -122.45) < 40.0);

        let center = [-122.50, 37.82];
        let mut l = Mover::new(
            contact(
                Motion::Loiter {
                    center,
                    radius_m: 300.0,
                },
                20.0,
            ),
            1,
            0,
        );
        for i in 0..200 {
            l.step(f64::from(i), 1.0, None);
            let r = distance_m(l.st.lat, l.st.lon, center[1], center[0]);
            assert!((r - 300.0).abs() < 2.0, "{r}");
        }
    }

    #[test]
    fn drift_follows_the_current_and_lifetime_is_respected() {
        let mut d = Mover::new(
            contact(
                Motion::Drift {
                    at: [-122.48, 37.82],
                },
                0.0,
            ),
            4,
            2,
        );
        let mut same = Mover::new(d.contact.clone(), 4, 2);
        for i in 0..100 {
            d.step(f64::from(i), 1.0, Some((0.5, 0.0)));
            same.step(f64::from(i), 1.0, Some((0.5, 0.0)));
        }
        let moved = distance_m(37.82, -122.48, d.st.lat, d.st.lon);
        assert!((moved - 50.0).abs() < 8.0, "{moved}");
        assert!(d.st.course_deg > 60.0 && d.st.course_deg < 120.0);
        // deterministic for the same seed and index
        assert_eq!((d.st.lat, d.st.lon), (same.st.lat, same.st.lon));

        let mut c = contact(
            Motion::Station {
                at: [-122.48, 37.82],
            },
            0.0,
        );
        c.start_s = 10.0;
        c.end_s = Some(20.0);
        let mut m = Mover::new(c, 1, 0);
        assert!(!m.step(5.0, 1.0, None));
        assert!(m.step(10.0, 1.0, None));
        assert!(!m.step(20.0, 1.0, None));
    }
}

#[cfg(test)]
mod maneuver_tests {
    use super::*;
    use crate::scenario::Scenario;
    #[test]
    fn fog_boat_changes_speed_and_route_without_teleporting() {
        let scenario = Scenario::from_toml(include_str!("../../../sim/scenarios/fog-watch.toml"))
            .expect("fog scenario");
        let mut m = Mover::new(scenario.contacts().expect("contacts").remove(0), 611, 0);
        for s in 1..210 {
            m.step(s as f64, 1.0, None);
        }
        let before = m.st.clone();
        m.step(211.0, 2.0, None); // a step straddling the maneuver, including the old motion
        assert!(distance_m(before.lat, before.lon, m.st.lat, m.st.lon) < 12.0);
        assert!(m.st.speed_mps > 4.0 && m.st.speed_mps <= 5.0);
        for s in 212..420 {
            m.step(s as f64, 1.0, None);
        }
        assert!(
            m.st.lon > -122.445,
            "the boat left the old loop: {:?}",
            m.st
        );
        assert_eq!(m.next_maneuver, 1);
    }
}

/// Slowing and stopping (bd murmur-2of4): the physical model against the legacy one.
#[cfg(test)]
mod stopping_tests {
    use super::*;
    use crate::scenario::{Affiliation, Scenario};

    /// A fast boat's figures (sim/contacts.toml `small_fast_boat`).
    const DECEL: f64 = 1.5;
    const V: f64 = 15.0;
    /// 2.2 km due north.
    const A: [f64; 2] = [-122.45, 37.80];
    const B: [f64; 2] = [-122.45, 37.82];

    fn boat(motion: Motion, model: MotionModel) -> Mover {
        Mover::new(
            Contact {
                maneuvers: Vec::new(),
                id: "boat".into(),
                name: "boat".into(),
                affiliation: Affiliation::Hostile,
                domain: Domain::Surface,
                kind: "small_fast_boat".into(),
                variant: None,
                ais: None,
                emits: false,
                signature: 1.0,
                speed_mps: V,
                alt_m: 0.0,
                turn_dps: 12.0,
                decel_mps2: DECEL,
                hovers: false,
                motion_model: model,
                motion,
                start_s: 0.0,
                end_s: None,
                on_effect: crate::scenario::OnEffect::OutOfPlay,
                strike_outcome: None,
                weapon: None,
            },
            1,
            0,
        )
    }

    /// Metres past B along the way north (negative: short of it).
    fn past_b(m: &Mover) -> f64 {
        (m.st.lat - B[1]) * M_PER_DEG
    }

    /// The transit's end: it slows at its deceleration in time to stand on its last point. No
    /// step slows it by more than its deceleration (the last, onto the point, by at most
    /// STAND_STEPS of it), it never passes the point, and the stop takes the time and the distance
    /// v / b and v² / 2b give (10 s and 75 m), within a step.
    #[test]
    fn a_transit_stops_on_its_last_point_at_its_deceleration() {
        let mut m = boat(Motion::Transit { points: vec![A, B] }, MotionModel::Physical);
        let (mut prev_v, mut prev_d) = (0.0, f64::INFINITY);
        let (mut began, mut stopped) = (None, None);
        for s in 1..=400 {
            m.step(f64::from(s), 1.0, None);
            assert!(past_b(&m) <= 1e-6, "past its point at {s} s: {:.2} m", past_b(&m));
            let slowed = prev_v - m.st.speed_mps;
            let most = if m.st.speed_mps > 0.0 { DECEL } else { STAND_STEPS * DECEL };
            assert!(slowed <= most + 1e-9, "slowed {slowed:.2} m/s in a step at {s} s");
            if began.is_none() && slowed > 1e-9 {
                began = Some((s - 1, prev_d));
            }
            if stopped.is_none() && prev_v > 0.0 && m.st.speed_mps == 0.0 {
                stopped = Some(s);
            }
            prev_v = m.st.speed_mps;
            prev_d = -past_b(&m);
        }
        let ((s0, d0), s1) = (began.expect("it slowed"), stopped.expect("it stopped"));
        assert!((f64::from(s1 - s0) - V / DECEL).abs() <= 1.5, "{s0} to {s1} s");
        assert!((d0 - V * V / (2.0 * DECEL)).abs() <= V, "slowing from {d0:.1} m out");
        assert!(past_b(&m).abs() < 0.01 && m.holding && m.st.speed_mps == 0.0, "{:?}", m.st);
    }

    /// Told to hold at speed, it runs on along its course slowing at its deceleration, stands
    /// some v² / 2b on (within a step) after v / b seconds, and stays there.
    #[test]
    fn a_hold_at_speed_runs_on_to_a_stop() {
        let mut m = boat(Motion::Station { at: A }, MotionModel::Physical);
        m.step(0.0, 1.0, None);
        (m.st.speed_mps, m.st.course_deg) = (V, 0.0);
        m.steer = Some(Steer::Hold);
        let mut s = 0;
        while m.st.speed_mps > 0.0 {
            s += 1;
            m.step(f64::from(s), 1.0, None);
            assert!(s < 30 && (m.st.course_deg - 0.0).abs() < 1e-9);
        }
        let ran = distance_m(A[1], A[0], m.st.lat, m.st.lon);
        assert!((f64::from(s) - V / DECEL).abs() <= 1.0, "{s} s");
        assert!((ran - V * V / (2.0 * DECEL)).abs() <= V, "{ran:.1} m");
        let here = (m.st.lat, m.st.lon);
        for t in s + 1..s + 20 {
            m.step(f64::from(t), 1.0, None);
        }
        assert_eq!((m.st.lat, m.st.lon, m.st.speed_mps), (here.0, here.1, 0.0));
    }

    /// The pinned worlds' motion (legacy): the same transit goes from full speed to a stop in
    /// one step, short of its last point (within the waypoint radius), and a hold stops it where
    /// it is, as before bd murmur-2of4.
    #[test]
    fn the_legacy_model_stops_in_one_step() {
        let mut m = boat(Motion::Transit { points: vec![A, B] }, MotionModel::Legacy);
        let mut prev = 0.0;
        let mut drop = None;
        for s in 1..=400 {
            m.step(f64::from(s), 1.0, None);
            if prev > 0.0 && m.st.speed_mps < prev {
                drop.get_or_insert((prev, m.st.speed_mps));
            }
            prev = m.st.speed_mps;
        }
        assert_eq!(drop, Some((V, 0.0)), "one step from its speed to a stop");
        assert!((-WAYPOINT_RADIUS_M..0.0).contains(&past_b(&m)) && m.holding, "{:.1} m", past_b(&m));

        let mut h = boat(Motion::Station { at: A }, MotionModel::Legacy);
        h.step(0.0, 1.0, None);
        h.st.speed_mps = V;
        h.steer = Some(Steer::Hold);
        h.step(1.0, 1.0, None);
        assert_eq!((h.st.lat, h.st.lon, h.st.speed_mps), (A[1], A[0], 0.0));
    }

    /// Its propulsion lost at speed, it coasts on at its deceleration (some v² / 2b) before it
    /// drifts; legacy, it stops where it was struck.
    #[test]
    fn a_mobility_kill_coasts_down() {
        for (model, coasts) in [(MotionModel::Physical, true), (MotionModel::Legacy, false)] {
            let mut m = boat(Motion::Transit { points: vec![A, B] }, model);
            for s in 1..=60 {
                m.step(f64::from(s), 1.0, None);
            }
            assert_eq!(m.st.speed_mps, V);
            let at = (m.st.lat, m.st.lon);
            assert!(m.damage(DamageState::Mobility, 60.0));
            for s in 61..=90 {
                m.step(f64::from(s), 1.0, None);
            }
            let on = distance_m(at.0, at.1, m.st.lat, m.st.lon);
            assert!(matches!(m.contact.motion, Motion::Drift { .. }) && m.st.speed_mps < 0.1);
            if coasts {
                assert!((on - V * V / (2.0 * DECEL)).abs() <= V, "{model:?}: {on:.1} m");
            } else {
                assert!(on < 2.0, "{model:?}: {on:.1} m");
            }
        }
    }

    /// A strike run (`into`) never slows for its point: it is on it at its speed.
    #[test]
    fn a_strike_run_runs_into_its_point() {
        let mut m = boat(Motion::Station { at: A }, MotionModel::Physical);
        m.step(0.0, 1.0, None);
        m.st.speed_mps = V;
        let (lat, lon) = offset(A[1], A[0], 0.0, 200.0);
        m.steer = Some(Steer::To { lat, lon, speed_mps: V, alt_m: None, into: true });
        for s in 1..=14 {
            m.step(f64::from(s), 1.0, None);
            assert_eq!(m.st.speed_mps, V, "{s} s");
        }
        assert!(distance_m(m.st.lat, m.st.lon, lat, lon) < 1e-6);
    }

    /// A fixed wing never stops: at its transit's end, or told to hold, it circles clockwise at
    /// its speed on its tightest turn (a light aircraft's 55 m/s at 6 degrees a second: 525 m),
    /// near where another would stand. A kind that hovers stands on its point as a boat does.
    #[test]
    fn a_fixed_wing_circles_where_another_would_stand() {
        let plane = |hovers: bool, motion: Motion| {
            let mut m = boat(motion, MotionModel::Physical);
            m.contact.domain = Domain::Air;
            m.contact.speed_mps = 55.0;
            m.contact.turn_dps = 6.0;
            m.contact.decel_mps2 = 1.0;
            m.contact.hovers = hovers;
            m
        };
        let r = 55.0 / 6f64.to_radians();
        let mut m = plane(false, Motion::Transit { points: vec![A, B] });
        m.step(0.0, 1.0, None);
        m.st.speed_mps = 55.0;
        let (mut least, mut far, mut turned) = (f64::INFINITY, 0.0f64, 0.0f64);
        let mut course = 0.0;
        for s in 1..=600 {
            m.step(f64::from(s), 1.0, None);
            if s > 200 {
                least = least.min(m.st.speed_mps);
                far = far.max(distance_m(B[1], B[0], m.st.lat, m.st.lon));
                turned += wrap180(m.st.course_deg - course);
            }
            course = m.st.course_deg;
        }
        assert!(m.holding && least > 54.0, "never stops: {least:.1} m/s");
        assert!(far <= 2.0 * r + 60.0, "circles by its last point: {far:.0} m");
        assert!(turned > 2.0 * 360.0, "clockwise, round and round: {turned:.0} degrees");

        let mut h = plane(true, Motion::Transit { points: vec![A, B] });
        for s in 1..=600 {
            h.step(f64::from(s), 1.0, None);
        }
        assert!(h.holding && h.st.speed_mps == 0.0 && distance_m(B[1], B[0], h.st.lat, h.st.lon) < 0.01);

        let mut held = plane(false, Motion::Station { at: A });
        held.step(0.0, 1.0, None);
        (held.st.speed_mps, held.st.course_deg) = (55.0, 90.0);
        held.steer = Some(Steer::Hold);
        for s in 1..=120 {
            held.step(f64::from(s), 1.0, None);
            assert_eq!(held.st.speed_mps, 55.0);
            assert!(distance_m(A[1], A[0], held.st.lat, held.st.lon) <= 2.0 * r + 1.0);
        }
    }

    /// The motion model is the file's `[motion] model` (none: physical), which
    /// MURMUR_MOTION_MODEL overrides; every contact moves by it; a value that is neither is
    /// refused rather than dropping a pin.
    #[test]
    fn the_motion_model_is_the_files_unless_pinned() {
        let file = |motion: &str| {
            format!(
                "title = \"t\"\n{motion}\n[[contact]]\nid = \"b\"\naffiliation = \"neutral\"\ndomain = \"surface\"\nkind = \"small_fast_boat\"\nmotion = \"station\"\nat = [-122.5, 37.8]\n"
            )
        };
        let mut s = Scenario::from_toml(&file("")).expect("scenario");
        assert_eq!(s.contacts().expect("contacts")[0].motion_model, MotionModel::Physical);
        assert_eq!(s.contacts().expect("contacts")[0].decel_mps2, 1.5);
        assert_eq!(s.pin_motion(Some("legacy")), Ok(MotionModel::Legacy));
        assert_eq!(s.contacts().expect("contacts")[0].motion_model, MotionModel::Legacy);
        let mut l = Scenario::from_toml(&file("[motion]\nmodel = \"legacy\"")).expect("scenario");
        assert_eq!(l.pin_motion(None), Ok(MotionModel::Legacy));
        assert_eq!(l.pin_motion(Some("physical")), Ok(MotionModel::Physical));
        assert!(l.pin_motion(Some("braking")).is_err());
        assert!(Scenario::from_toml(&file("[motion]\nmodel = \"fast\"")).is_err());
    }
}
