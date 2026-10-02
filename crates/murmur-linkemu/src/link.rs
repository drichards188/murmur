//! One vehicle's link: its profile, scripted events, position-driven physics and the two
//! direction models.

use crate::config::{Config, DirSel, DownWhen, EventCfg};
use crate::geo::{GeoPoint, Terrain, haversine_m, slant_m};
use crate::mavpos::scan_position;
use crate::model::{Cause, Cond, Dir, DirState, Params, Verdict};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct LinkMeta {
    pub uid: u32,
    pub sysid: u32,
    pub port: u16,
    /// The platform's link class from platforms.toml (los, mesh, ...).
    pub class: String,
    /// The profile in effect (the class, unless platform_profile overrides it).
    pub profile: String,
    pub domain: String,
    pub platform: String,
}

struct Event {
    name: String,
    start_ns: u64,
    end_ns: u64,
    dir: DirSel,
    loss: f64,
    cut: bool,
    extra_latency_ns: u64,
    circle: Option<(GeoPoint, f64)>,
    selectors: Selectors,
}

struct Selectors {
    class: Option<String>,
    profile: Option<String>,
    platform: Option<String>,
    domain: Option<String>,
    uids: Option<Vec<u32>>,
}

impl Selectors {
    fn matches(&self, m: &LinkMeta) -> bool {
        self.class.as_ref().is_none_or(|c| *c == m.class)
            && self.profile.as_ref().is_none_or(|c| *c == m.profile)
            && self.platform.as_ref().is_none_or(|c| *c == m.platform)
            && self.domain.as_ref().is_none_or(|c| *c == m.domain)
            && self.uids.as_ref().is_none_or(|u| u.contains(&m.uid))
    }
}

/// The compiled scenario, shared by all links.
pub struct Schedule {
    events: Vec<Event>,
}

impl Schedule {
    pub fn compile(events: &[EventCfg]) -> Schedule {
        let events = events
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let uids = match (&e.uid, &e.uids) {
                    (Some(u), Some(us)) => Some(us.iter().copied().chain([*u]).collect()),
                    (Some(u), None) => Some(vec![*u]),
                    (None, Some(us)) => Some(us.clone()),
                    (None, None) => None,
                };
                let start_ns = (e.at_s * 1e9) as u64;
                Event {
                    name: e.name.clone().unwrap_or_else(|| format!("event {i}")),
                    start_ns,
                    end_ns: e
                        .duration_s
                        .map_or(u64::MAX, |d| start_ns.saturating_add((d * 1e9) as u64)),
                    dir: e.dir,
                    loss: e.loss.unwrap_or(if e.circle.is_some() && !e.cut {
                        1.0
                    } else {
                        0.0
                    }),
                    cut: e.cut,
                    extra_latency_ns: (e.extra_latency_ms.max(0.0) * 1e6) as u64,
                    circle: e.circle.as_ref().map(|c| {
                        (
                            GeoPoint {
                                lat: c.lat,
                                lon: c.lon,
                                alt_m: 0.0,
                            },
                            c.radius_m,
                        )
                    }),
                    selectors: Selectors {
                        class: e.class.clone(),
                        profile: e.profile.clone(),
                        platform: e.platform.clone(),
                        domain: e.domain.clone(),
                        uids,
                    },
                }
            })
            .collect();
        Schedule { events }
    }

    pub fn matching(&self, m: &LinkMeta) -> Vec<usize> {
        (0..self.events.len())
            .filter(|&i| self.events[i].selectors.matches(m))
            .collect()
    }

    pub fn names(&self) -> Vec<(&str, u64, u64)> {
        self.events
            .iter()
            .map(|e| (e.name.as_str(), e.start_ns, e.end_ns))
            .collect()
    }
}

pub struct Env {
    pub cfg_submerged_depth_m: f64,
    pub ground_station: Option<GeoPoint>,
    pub terrain: Arc<dyn Terrain>,
    pub schedule: Schedule,
    pub seed: u64,
}

impl Env {
    pub fn new(cfg: &Config, terrain: Arc<dyn Terrain>, seed: u64) -> Env {
        Env {
            cfg_submerged_depth_m: cfg.submerged_depth_m,
            ground_station: cfg.ground_station.as_ref().map(|g| GeoPoint {
                lat: g.lat,
                lon: g.lon,
                alt_m: g.alt_m,
            }),
            terrain,
            schedule: Schedule::compile(&cfg.event),
            seed,
        }
    }
}

pub struct Link {
    pub meta: LinkMeta,
    params: Params,
    env: Arc<Env>,
    events: Vec<usize>,
    dirs: [DirState; 2],
    pos: Option<GeoPoint>,
    /// Slant range to the ground station at the last known position.
    gs_range_m: Option<f64>,
    los: bool,
}

impl Link {
    pub fn new(meta: LinkMeta, params: Params, env: Arc<Env>) -> Link {
        let events = env.schedule.matching(&meta);
        let dirs = [
            DirState::new(env.seed, meta.uid, Dir::Up),
            DirState::new(env.seed, meta.uid, Dir::Down),
        ];
        Link {
            meta,
            params,
            env,
            events,
            dirs,
            pos: None,
            gs_range_m: None,
            los: true,
        }
    }

    pub fn position(&self) -> Option<GeoPoint> {
        self.pos
    }

    pub fn params(&self) -> &Params {
        &self.params
    }

    /// Submerged (Some(true)), surfaced (Some(false)) or unknown before the first position.
    pub fn submerged(&self) -> Option<bool> {
        self.pos.map(|p| -p.alt_m > self.env.cfg_submerged_depth_m)
    }

    pub fn set_position(&mut self, p: GeoPoint) {
        self.pos = Some(p);
        if let Some(gs) = &self.env.ground_station {
            self.gs_range_m = Some(slant_m(&p, gs));
            self.los = self.env.terrain.line_of_sight(&p, gs);
        }
    }

    /// Look at an uplink datagram (vehicle to bridge) for a position. Never alters it.
    pub fn observe_up(&mut self, datagram: &[u8]) {
        if let Some(p) = scan_position(datagram) {
            self.set_position(p);
        }
    }

    pub fn cond(&self, dir: Dir, now_ns: u64) -> Cond {
        let mut c = Cond::default();
        if let (Some(rule), Some(sub)) = (self.params.down_when, self.submerged()) {
            let down = match rule {
                DownWhen::Submerged => sub,
                DownWhen::Surfaced => !sub,
            };
            if down {
                c.blocked = Some(Cause::Gate);
            }
        }
        for &i in &self.events {
            let e = &self.env.schedule.events[i];
            if now_ns < e.start_ns || now_ns >= e.end_ns {
                continue;
            }
            if (e.dir == DirSel::Up && dir != Dir::Up)
                || (e.dir == DirSel::Down && dir != Dir::Down)
            {
                continue;
            }
            if let Some((centre, r)) = &e.circle {
                match &self.pos {
                    Some(p) if haversine_m(p, centre) <= *r => {}
                    _ => continue,
                }
            }
            if e.cut {
                c.blocked.get_or_insert(Cause::Cut);
            }
            if e.loss > 0.0 {
                let slot = if e.circle.is_some() {
                    &mut c.jam_loss
                } else {
                    &mut c.event_loss
                };
                *slot = 1.0 - (1.0 - *slot) * (1.0 - e.loss);
            }
            c.extra_latency_ns += e.extra_latency_ns;
        }
        if let (Some((ok, max, exp)), Some(d)) = (self.params.range, self.gs_range_m) {
            c.range_loss = if d <= ok {
                0.0
            } else if d >= max {
                1.0
            } else {
                ((d - ok) / (max - ok)).powf(exp)
            };
            if !self.los {
                c.blocked.get_or_insert(Cause::Terrain);
            }
        }
        if let (Some(mps), Some(d)) = (self.params.propagation_mps, self.gs_range_m) {
            c.extra_latency_ns += (d / mps * 1e9) as u64;
        }
        c
    }

    pub fn decide(&mut self, dir: Dir, now_ns: u64, len: usize, inflight: usize) -> Verdict {
        let c = self.cond(dir, now_ns);
        self.dirs[dir.idx()].decide(&self.params, dir, &c, now_ns, len, inflight)
    }

    pub fn backlog_bytes(&self, dir: Dir, now_ns: u64) -> u64 {
        self.dirs[dir.idx()].backlog_bytes(now_ns, self.params.bw_bps[dir.idx()])
    }

    /// Whether a scripted event or physics currently blocks the link (for the stats view).
    pub fn blocked_now(&self, now_ns: u64) -> Option<Cause> {
        self.cond(Dir::Up, now_ns).blocked
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::geo::NoTerrain;
    use crate::mavpos::position_frame;

    const NS: u64 = 1_000_000_000;

    fn meta(uid: u32, class: &str, domain: &str) -> LinkMeta {
        LinkMeta {
            uid,
            sysid: uid,
            port: 0,
            class: class.into(),
            profile: class.into(),
            domain: domain.into(),
            platform: "x".into(),
        }
    }

    fn build(toml: &str, m: LinkMeta) -> Link {
        let cfg = Config::parse(toml).expect("cfg");
        let params =
            Params::from_profile(&cfg.resolve(&m.profile).expect("profile")).expect("params");
        Link::new(m, params, Arc::new(Env::new(&cfg, Arc::new(NoTerrain), 9)))
    }

    fn lost_fraction(l: &mut Link, dir: Dir, t0: u64, n: u64) -> f64 {
        let lost = (0..n)
            .filter(|i| matches!(l.decide(dir, t0 + i * 1_000_000, 50, 0), Verdict::Drop(_)))
            .count();
        lost as f64 / n as f64
    }

    #[test]
    fn scripted_loss_window_applies_only_inside_the_window_and_to_selected_links() {
        let toml = r#"
            [class.los]
            [class.mesh]
            [[event]]
            at_s = 300
            duration_s = 120
            class = "los"
            loss = 0.3
        "#;
        let mut los = build(toml, meta(1, "los", "air"));
        let mut mesh = build(toml, meta(2, "mesh", "air"));
        assert_eq!(lost_fraction(&mut los, Dir::Up, 299 * NS, 500), 0.0);
        let inside = lost_fraction(&mut los, Dir::Up, 300 * NS, 20_000);
        assert!((inside - 0.3).abs() < 0.02, "{inside}");
        assert_eq!(lost_fraction(&mut los, Dir::Up, 421 * NS, 500), 0.0);
        assert_eq!(lost_fraction(&mut mesh, Dir::Up, 350 * NS, 500), 0.0);
    }

    #[test]
    fn cut_one_uid_for_a_minute_in_one_direction() {
        let toml = r#"
            [class.los]
            [[event]]
            at_s = 10
            duration_s = 60
            uid = 7
            cut = true
            dir = "down"
        "#;
        let mut seven = build(toml, meta(7, "los", "air"));
        let mut eight = build(toml, meta(8, "los", "air"));
        assert_eq!(lost_fraction(&mut seven, Dir::Down, 20 * NS, 100), 1.0);
        assert_eq!(lost_fraction(&mut seven, Dir::Up, 20 * NS, 100), 0.0);
        assert_eq!(lost_fraction(&mut seven, Dir::Down, 71 * NS, 100), 0.0);
        assert_eq!(lost_fraction(&mut eight, Dir::Down, 20 * NS, 100), 0.0);
    }

    #[test]
    fn jam_circle_hits_only_vehicles_inside() {
        let toml = r#"
            [class.los]
            [[event]]
            at_s = 0
            duration_s = 100
            circle = { lat = 37.8, lon = -122.4, radius_m = 1000 }
        "#;
        let mut inside = build(toml, meta(1, "los", "air"));
        let mut outside = build(toml, meta(2, "los", "air"));
        let mut unknown = build(toml, meta(3, "los", "air"));
        inside.observe_up(&position_frame(0, 37.801, -122.4, 10.0));
        outside.observe_up(&position_frame(0, 37.9, -122.4, 10.0));
        assert_eq!(lost_fraction(&mut inside, Dir::Up, NS, 100), 1.0);
        assert_eq!(lost_fraction(&mut outside, Dir::Up, NS, 100), 0.0);
        assert_eq!(lost_fraction(&mut unknown, Dir::Up, NS, 100), 0.0);
        // The jammer is a cause of its own in the counters.
        assert_eq!(inside.decide(Dir::Up, NS, 50, 0), Verdict::Drop(Cause::Jam));
    }

    #[test]
    fn range_loss_rises_with_distance_from_the_ground_station() {
        let toml = r#"
            [ground_station]
            lat = 37.8
            lon = -122.4
            [class.los]
            range_ok_km = 10
            range_max_km = 20
            range_exp = 1
        "#;
        // 1 degree of latitude is about 111 km, so 0.1 degree is ~11.1 km.
        let mut near = build(toml, meta(1, "los", "air"));
        let mut mid = build(toml, meta(2, "los", "air"));
        let mut far = build(toml, meta(3, "los", "air"));
        near.observe_up(&position_frame(0, 37.85, -122.4, 0.0)); // 5.6 km
        mid.observe_up(&position_frame(0, 37.915, -122.4, 0.0)); // 12.8 km, 28% of the ramp
        far.observe_up(&position_frame(0, 38.1, -122.4, 0.0)); // 33 km
        assert_eq!(lost_fraction(&mut near, Dir::Up, NS, 1000), 0.0);
        let m = lost_fraction(&mut mid, Dir::Up, NS, 20_000);
        assert!((m - 0.28).abs() < 0.03, "{m}");
        assert_eq!(lost_fraction(&mut far, Dir::Up, NS, 1000), 1.0);
    }

    #[test]
    fn acoustic_only_while_submerged_and_satcom_only_while_surfaced() {
        let toml = r#"
            [class.acoustic]
            down_when = "surfaced"
            [class.satcom]
            down_when = "submerged"
        "#;
        let mut a = build(toml, meta(1, "acoustic", "subsurface"));
        let mut s = build(toml, meta(2, "satcom", "subsurface"));
        // Unknown depth: do not block.
        assert_eq!(lost_fraction(&mut a, Dir::Down, NS, 10), 0.0);
        assert_eq!(lost_fraction(&mut s, Dir::Down, NS, 10), 0.0);
        for l in [&mut a, &mut s] {
            l.observe_up(&position_frame(0, 37.8, -122.4, -30.0));
        }
        assert_eq!(a.submerged(), Some(true));
        assert_eq!(lost_fraction(&mut a, Dir::Down, NS, 10), 0.0);
        assert_eq!(s.decide(Dir::Down, NS, 50, 0), Verdict::Drop(Cause::Gate));
        for l in [&mut a, &mut s] {
            l.observe_up(&position_frame(1, 37.8, -122.4, 0.2));
        }
        assert_eq!(a.decide(Dir::Down, NS, 50, 0), Verdict::Drop(Cause::Gate));
        assert_eq!(lost_fraction(&mut s, Dir::Down, NS, 10), 0.0);
    }

    #[test]
    fn terrain_hook_blocks_when_there_is_no_line_of_sight() {
        struct Blocked;
        impl Terrain for Blocked {
            fn line_of_sight(&self, _: &GeoPoint, _: &GeoPoint) -> bool {
                false
            }
        }
        let cfg = Config::parse(
            "[ground_station]\nlat = 37.8\nlon = -122.4\n[class.los]\nrange_ok_km = 10\nrange_max_km = 20\n",
        )
        .expect("cfg");
        let m = meta(1, "los", "air");
        let params = Params::from_profile(&cfg.resolve("los").expect("p")).expect("params");
        let mut l = Link::new(m, params, Arc::new(Env::new(&cfg, Arc::new(Blocked), 1)));
        l.observe_up(&position_frame(0, 37.81, -122.4, 0.0));
        assert_eq!(l.decide(Dir::Up, NS, 50, 0), Verdict::Drop(Cause::Terrain));
    }

    #[test]
    fn extra_latency_event_and_propagation_delay() {
        let toml = r#"
            [ground_station]
            lat = 0.0
            lon = 0.0
            [class.acoustic]
            propagation_mps = 1500
            [[event]]
            at_s = 0
            class = "acoustic"
            extra_latency_ms = 100
        "#;
        let mut l = build(toml, meta(1, "acoustic", "subsurface"));
        // 0.0135 degrees of latitude is about 1.5 km: 1 s of propagation.
        l.observe_up(&position_frame(0, 0.0135, 0.0, -50.0));
        let Verdict::Forward { at_ns, .. } = l.decide(Dir::Down, NS, 50, 0) else {
            panic!()
        };
        let delay_ms = (at_ns - NS) as f64 / 1e6;
        assert!(delay_ms > 1090.0 && delay_ms < 1110.0, "{delay_ms}");
    }
}
