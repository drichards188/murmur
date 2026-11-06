//! Simple, deterministic motion for contacts: heading-limited turns toward waypoints at a
//! constant speed, circles, station keeping, and drift with the current. Flat-earth
//! integration (`M_PER_DEG`), as the stand-ins do.
//!
//! A node's order ([`crate::node`]) moves a contact instead of its file's motion while it runs:
//! the node sets [`Mover::steer`] (hold, head for a point, a route, an orbit, or back to the
//! file's motion) and the step drives it with the same turn rate and acceleration. Without one
//! the file's motion runs exactly as before.

use murmur_standins::geo::{Gauss, M_PER_DEG, bearing_deg, distance_m, wrap180};

use crate::damage::DamageState;
use crate::scenario::{Contact, Domain, Motion};

/// Close enough to a waypoint to take the next one.
const WAYPOINT_RADIUS_M: f64 = 25.0;
/// Speed-up from a dwell or spawn.
const ACCEL_MPS2: f64 = 0.5;
/// An orbit steering this far off its circle heads for it first.
const ORBIT_JOIN_M: f64 = 30.0;

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
}

/// A node's steering ([`crate::node`]), driven by [`Mover::step`] in place of the file's motion.
#[derive(Debug, Clone, PartialEq)]
pub enum Steer {
    /// Stop where it is and stay there (a boat keeps station against the current: no drift; an
    /// aircraft hovers, a stated simplification).
    Hold,
    /// Head for a point at a speed and stop on it; with `alt_m`, change height in proportion as
    /// it closes (a dive onto its target).
    To { lat: f64, lon: f64, speed_mps: f64, alt_m: Option<f64> },
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
    /// aircraft flies on at half its speed [E: slowed, as a damaged engine]); its weapons out of
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
                    self.st.speed_mps = 0.0;
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
        // a node's order steers it instead of its file's motion (crate::node)
        if let Some(steer) = self.steer.take() {
            self.steer = self.drive(steer, dt);
            return true;
        }
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
                    self.st.speed_mps = 0.0;
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
            Motion::Station { .. } => {
                self.st.speed_mps = 0.0;
            }
            Motion::Drift { .. } => {
                let (ce, cn) = current.unwrap_or((0.0, 0.0));
                let e = ce + self.rng.sample(0.0, 0.02);
                let n = cn + self.rng.sample(0.0, 0.02);
                let (lat, lon) = offset(self.st.lat, self.st.lon, e * dt, n * dt);
                self.st.lat = lat;
                self.st.lon = lon;
                self.st.speed_mps = e.hypot(n);
                if self.st.speed_mps > 0.05 {
                    self.st.course_deg = e.atan2(n).to_degrees().rem_euclid(360.0);
                }
            }
        }
        true
    }

    /// One step of a node's steering; what it steers by next (none: back on the file's motion).
    /// Its propulsion damaged (only an aircraft is still steered), at half the speed it is told
    /// ([`Mover::told`]).
    fn drive(&mut self, steer: Steer, dt: f64) -> Option<Steer> {
        match steer {
            Steer::Hold => {
                self.st.speed_mps = 0.0;
                Some(Steer::Hold)
            }
            Steer::To { lat, lon, speed_mps, alt_m } => {
                self.head(lat, lon, speed_mps, alt_m, dt, true);
                Some(Steer::To { lat, lon, speed_mps, alt_m })
            }
            Steer::Route { points, speed_mps, mut leg, mut done } => {
                if done {
                    self.st.speed_mps = 0.0;
                } else {
                    let last = points.len() - 1;
                    let wp = points[leg];
                    if self.head(wp[1], wp[0], speed_mps, None, dt, leg == last) {
                        if leg == last {
                            done = true;
                        } else {
                            leg += 1;
                            let wp = points[leg];
                            self.head(wp[1], wp[0], speed_mps, None, dt, leg == last);
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
                    self.head(lat, lon, speed_mps, None, dt, false);
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
                if self.head(lat, lon, speed_mps, None, dt, true) {
                    if let Some(p) = phase_deg {
                        self.phase_deg = p;
                    }
                    return None;
                }
                Some(Steer::Rejoin { lat, lon, speed_mps, phase_deg })
            }
        }
    }

    /// Steer for a point with the kind's turn rate at up to `speed`; with `alt`, the height goes
    /// to it in proportion to the ground covered. Within this step's reach it is there: with
    /// `stop` it stands on the point, stopped; without, it has not moved yet (the caller takes
    /// the next point). Returns whether it is there.
    fn head(&mut self, lat: f64, lon: f64, speed: f64, alt: Option<f64>, dt: f64, stop: bool) -> bool {
        let d = distance_m(self.st.lat, self.st.lon, lat, lon);
        let v = (self.st.speed_mps + ACCEL_MPS2 * dt).min(self.told(speed)).max(0.0);
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
        let want = bearing_deg(self.st.lat, self.st.lon, lat, lon);
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

    /// The speed a steering's `speed` makes good: half with its propulsion damaged (an aircraft
    /// flies on slowed, as its file's motion does).
    fn told(&self, speed: f64) -> f64 {
        if self.crippled { speed * 0.5 } else { speed }
    }

    fn accelerate(&mut self, target: f64, dt: f64) -> f64 {
        self.st.speed_mps = (self.st.speed_mps + ACCEL_MPS2 * dt).min(target);
        self.st.speed_mps
    }

    /// Steer for the current waypoint with the kind's turn rate; at the end, loop, bounce
    /// (after a dwell) or hold.
    fn follow(&mut self, points: &[[f64; 2]], dt: f64, route: Option<(bool, f64)>) {
        if self.dwell_left > 0.0 {
            self.dwell_left -= dt;
            self.st.speed_mps = 0.0;
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
                        self.st.speed_mps = 0.0;
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
                        self.st.speed_mps = 0.0;
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
            motion,
            start_s: 0.0,
            end_s: None,
            on_effect: crate::scenario::OnEffect::OutOfPlay,
            strike_outcome: None,
            weapon: None,
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
