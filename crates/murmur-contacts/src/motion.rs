//! Simple, deterministic motion for contacts: heading-limited turns toward waypoints at a
//! constant speed, circles, station keeping, and drift with the current. Flat-earth
//! integration (`M_PER_DEG`), as the stand-ins do.

use murmur_standins::geo::{Gauss, M_PER_DEG, bearing_deg, distance_m, wrap180};

use crate::scenario::{Contact, Motion};

/// Close enough to a waypoint to take the next one.
const WAYPOINT_RADIUS_M: f64 = 25.0;
/// Speed-up from a dwell or spawn.
const ACCEL_MPS2: f64 = 0.5;

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
}

fn offset(lat: f64, lon: f64, east_m: f64, north_m: f64) -> (f64, f64) {
    (
        lat + north_m / M_PER_DEG,
        lon + east_m / (M_PER_DEG * lat.to_radians().cos()),
    )
}

impl Mover {
    pub fn new(contact: Contact, seed: u64, index: u64) -> Mover {
        let (lon, lat) = match &contact.motion {
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
            contact,
        }
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
        if !self.st.exists || dt <= 0.0 {
            return self.st.exists;
        }
        match &c.motion {
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
    use crate::scenario::{Affiliation, Domain};

    fn contact(motion: Motion, speed: f64) -> Contact {
        Contact {
            maneuvers: Vec::new(),
            id: "c".into(),
            name: "c".into(),
            affiliation: Affiliation::Neutral,
            domain: Domain::Surface,
            kind: "ferry".into(),
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
