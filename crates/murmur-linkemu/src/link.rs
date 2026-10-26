//! One vehicle's link: its profile, scripted events, position-driven physics and the two
//! direction models.

use crate::config::{Config, DirSel, DownWhen, EventCfg};
use crate::geo::{GeoPoint, Terrain, haversine_m, slant_m};
use crate::mavpos::scan;
use crate::model::{Cause, Cond, Dir, DirState, Params, Verdict, range_loss, terrain_effect};
use crate::sat::SatShared;
use crate::timeline::{Timeline, parse_iso_utc};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// A landed, disarmed aircraft (`Link::at_rest`) rests on the surface it came down on: its
/// terrain check puts its antenna this high above the altitude it reports (GUESS: an antenna
/// in a small airframe lying on the ground), at that altitude rather than the cache's band,
/// which can round one on the ground metres under its surface (a Puma down at 3 m read as 0 m).
pub const AT_REST_ANTENNA_M: f64 = 0.5;
/// A disarmed aircraft within this of its home's altitude (GLOBAL_POSITION_INT's relative
/// altitude) is on the ground: ArduPilot's home follows a disarmed plane, so it is where it
/// landed.
pub const AT_REST_REL_ALT_M: f64 = 1.0;

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
    /// The profile that must carry the traffic now (checked per packet, as the circle is: it
    /// changes with the vehicle's depth).
    active_profile: Option<String>,
}

impl Selectors {
    /// The selectors a link's identity decides (once, and at each reload).
    fn matches(&self, m: &LinkMeta) -> bool {
        self.class.as_ref().is_none_or(|c| *c == m.class)
            && self.profile.as_ref().is_none_or(|c| *c == m.profile)
            && self.platform.as_ref().is_none_or(|c| *c == m.platform)
            && self.domain.as_ref().is_none_or(|c| *c == m.domain)
            && self.uids.as_ref().is_none_or(|u| u.contains(&m.uid))
    }

    /// `active_profile`: whether `active`, the profile carrying the link's traffic now, is the
    /// one the event names.
    fn carried_by(&self, active: &str) -> bool {
        self.active_profile.as_deref().is_none_or(|p| p == active)
    }
}

/// The compiled scenario, shared by all links.
pub struct Schedule {
    events: Vec<Event>,
}

impl Schedule {
    /// Compiles events into emulator-relative nanoseconds. Scenario-time events are converted
    /// with `timeline` (so they need `MURMUR_AT`); an event wholly in the past never fires.
    pub fn compile(events: &[EventCfg], timeline: &Timeline) -> Result<Schedule, String> {
        let ns = |s: f64| (s.max(0.0) * 1e9) as u64;
        let mut out = Vec::new();
        for (i, e) in events.iter().enumerate() {
            let name = e.name.clone().unwrap_or_else(|| format!("event {i}"));
            let (start_s, end_s) = match &e.at_scenario {
                Some(at) => {
                    let start = timeline
                        .elapsed_for(parse_iso_utc(at)?)
                        .map_err(|m| format!("event {name:?}: {m}"))?;
                    let end = match (&e.until_scenario, e.duration_s) {
                        (Some(u), _) => Some(
                            timeline
                                .elapsed_for(parse_iso_utc(u)?)
                                .map_err(|m| format!("event {name:?}: {m}"))?,
                        ),
                        (None, Some(d)) => Some(start + timeline.span_elapsed(d)),
                        (None, None) => None,
                    };
                    (start, end)
                }
                None => {
                    let start = e.at_s.unwrap_or(0.0);
                    (start, e.duration_s.map(|d| start + d))
                }
            };
            let uids = match (&e.uid, &e.uids) {
                (Some(u), Some(us)) => Some(us.iter().copied().chain([*u]).collect()),
                (Some(u), None) => Some(vec![*u]),
                (None, Some(us)) => Some(us.clone()),
                (None, None) => None,
            };
            out.push(Event {
                name,
                start_ns: ns(start_s),
                end_ns: end_s.map_or(u64::MAX, ns),
                dir: e.dir,
                loss: e
                    .loss
                    .unwrap_or(if e.circle.is_some() && !e.cut && e.gnss.is_none() {
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
                    active_profile: e.active_profile.clone(),
                },
            });
        }
        Ok(Schedule { events: out })
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
    pub timeline: Timeline,
    pub seed: u64,
    schedule: RwLock<Arc<Schedule>>,
    /// Bumped on every event reload; links re-match their events when it changes.
    generation: AtomicU64,
}

impl Env {
    /// No scenario clock: events must be in emulator seconds.
    pub fn new(cfg: &Config, terrain: Arc<dyn Terrain>, seed: u64) -> Result<Env, String> {
        Env::with_timeline(cfg, terrain, seed, Timeline::default())
    }

    pub fn with_timeline(
        cfg: &Config,
        terrain: Arc<dyn Terrain>,
        seed: u64,
        timeline: Timeline,
    ) -> Result<Env, String> {
        Ok(Env {
            cfg_submerged_depth_m: cfg.submerged_depth_m,
            ground_station: cfg.ground_station.as_ref().map(|g| GeoPoint {
                lat: g.lat,
                lon: g.lon,
                alt_m: g.alt_m.metres(),
            }),
            terrain,
            timeline,
            seed,
            schedule: RwLock::new(Arc::new(Schedule::compile(&cfg.event, &timeline)?)),
            generation: AtomicU64::new(0),
        })
    }

    pub fn schedule(&self) -> Arc<Schedule> {
        self.schedule
            .read()
            .map_or_else(|e| e.into_inner().clone(), |g| g.clone())
    }

    /// Replaces the scripted events. Each link switches at its next packet. On error the
    /// old events stay.
    pub fn reload_events(&self, events: &[EventCfg]) -> Result<usize, String> {
        let s = Arc::new(Schedule::compile(events, &self.timeline)?);
        let n = s.events.len();
        match self.schedule.write() {
            Ok(mut g) => *g = s,
            Err(e) => *e.into_inner() = s,
        }
        self.generation.fetch_add(1, Ordering::Release);
        Ok(n)
    }
}

/// What the terrain check currently says about a link.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LosState {
    /// The profile does no terrain check (or there is no ground station).
    Off,
    /// No answer from tidewater yet; the path is treated as open.
    Unknown,
    /// Clear by this many metres.
    Clear(f64),
    /// Obstructed by this many metres.
    Obstructed(f64),
}

pub struct Link {
    pub meta: LinkMeta,
    params: Params,
    env: Arc<Env>,
    sched: Arc<Schedule>,
    sched_gen: u64,
    events: Vec<usize>,
    dirs: [DirState; 2],
    pos: Option<GeoPoint>,
    /// Slant range to the ground station at the last known position.
    gs_range_m: Option<f64>,
    /// Latest terrain answer (metres the terrain sticks up above the ray); kept while a new
    /// cell is being computed.
    obstruction_m: Option<f64>,
    /// Packets decided while no terrain answer existed.
    pub los_unknown_pkts: u64,
    /// The vehicle's last heartbeat: armed; and its altitude above home with its position.
    armed: Option<bool>,
    rel_alt_m: Option<f64>,
    /// SATCOM state when the profile follows a constellation class (`space_class`).
    sat: Option<Arc<SatShared>>,
    /// The profile that carries the traffic while `down_when` closes this one
    /// (`gated_profile`): its name and parameters.
    gated: Option<(String, Params)>,
}

impl Link {
    pub fn new(meta: LinkMeta, params: Params, env: Arc<Env>) -> Link {
        let sat = params
            .space_class
            .as_ref()
            .map(|c| Arc::new(SatShared::new(c, params.handover_outage_ns)));
        let sched_gen = env.generation.load(Ordering::Acquire);
        let sched = env.schedule();
        let events = sched.matching(&meta);
        let dirs = [
            DirState::new(env.seed, meta.uid, Dir::Up),
            DirState::new(env.seed, meta.uid, Dir::Down),
        ];
        Link {
            meta,
            params,
            env,
            sched,
            sched_gen,
            events,
            dirs,
            pos: None,
            gs_range_m: None,
            obstruction_m: None,
            los_unknown_pkts: 0,
            armed: None,
            rel_alt_m: None,
            sat,
            gated: None,
        }
    }

    /// Carries the traffic on `params` (profile `name`) while the depth gate closes this
    /// link's own profile.
    pub fn with_gated(mut self, name: &str, params: Params) -> Link {
        self.gated = Some((name.to_string(), params));
        self
    }

    /// Whether `down_when` closes the link's own profile now.
    fn gate_closed(&self) -> bool {
        match (self.params.down_when, self.submerged()) {
            (Some(DownWhen::Submerged), Some(sub)) => sub,
            (Some(DownWhen::Surfaced), Some(sub)) => !sub,
            _ => false,
        }
    }

    /// The profile carrying the traffic now: the gated profile while the gate is closed.
    pub fn active_profile(&self) -> &str {
        match &self.gated {
            Some((name, _)) if self.gate_closed() => name,
            _ => &self.meta.profile,
        }
    }

    /// The parameters in effect now (see `active_profile`).
    fn active(&self) -> &Params {
        match &self.gated {
            Some((_, g)) if self.gate_closed() => g,
            _ => &self.params,
        }
    }

    pub fn position(&self) -> Option<GeoPoint> {
        self.pos
    }

    /// The SATCOM state, for the satellite task and the stats.
    pub fn sat_shared(&self) -> Option<Arc<SatShared>> {
        self.sat.clone()
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
        if let Some(s) = &self.sat {
            s.set_position(p);
        }
        if let Some(gs) = &self.env.ground_station {
            self.gs_range_m = Some(slant_m(&p, gs));
            if let Some(t) = &self.params.terrain
                && self.env.terrain.enabled()
            {
                let o = if self.at_rest() {
                    let lift = (AT_REST_ANTENNA_M - t.los.antenna_m).max(0.0);
                    let rest = GeoPoint {
                        alt_m: p.alt_m + lift,
                        ..p
                    };
                    self.env.terrain.obstruction_at_rest_m(&rest, gs, &t.los)
                } else {
                    self.env.terrain.obstruction_m(&p, gs, &t.los)
                };
                if let Some(o) = o {
                    self.obstruction_m = Some(o);
                }
            }
        }
    }

    /// A landed, disarmed aircraft: of the air domain, its last heartbeat disarmed, within
    /// `AT_REST_REL_ALT_M` of its home's altitude. Its terrain check is at rest (see
    /// `AT_REST_ANTENNA_M`); no other vehicle's is.
    pub fn at_rest(&self) -> bool {
        self.meta.domain == "air"
            && self.armed == Some(false)
            && self.rel_alt_m.is_some_and(|r| r.abs() < AT_REST_REL_ALT_M)
    }

    pub fn los_state(&self) -> LosState {
        if self.params.terrain.is_none()
            || self.env.ground_station.is_none()
            || !self.env.terrain.enabled()
        {
            return LosState::Off;
        }
        match self.obstruction_m {
            None => LosState::Unknown,
            Some(o) if o > 0.0 => LosState::Obstructed(o),
            Some(o) => LosState::Clear(-o),
        }
    }

    /// Picks up a reloaded event schedule.
    fn refresh_events(&mut self) {
        let g = self.env.generation.load(Ordering::Acquire);
        if g != self.sched_gen {
            self.sched = self.env.schedule();
            self.events = self.sched.matching(&self.meta);
            self.sched_gen = g;
        }
    }

    /// Look at an uplink datagram (vehicle to bridge) for a position and the armed flag. Never
    /// alters it.
    pub fn observe_up(&mut self, datagram: &[u8]) {
        let seen = scan(datagram);
        if seen.armed.is_some() {
            self.armed = seen.armed;
        }
        if let Some(p) = seen.pos {
            self.rel_alt_m = seen.rel_alt_m;
            self.set_position(p);
        }
    }

    pub fn cond(&self, dir: Dir, now_ns: u64) -> Cond {
        let mut c = Cond::default();
        let gated = self.gate_closed();
        if gated && self.gated.is_none() {
            c.blocked = Some(Cause::Gate);
        }
        // The primary profile's satellites; a gated profile has none (checked at load).
        if !gated && let Some(s) = &self.sat {
            s.apply(now_ns, &mut c.blocked, &mut c.base_latency_ns);
        }
        let p = self.active();
        let active = self.active_profile();
        for &i in &self.events {
            let e = &self.sched.events[i];
            if now_ns < e.start_ns || now_ns >= e.end_ns || !e.selectors.carried_by(active) {
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
        if let (Some(r), Some(d)) = (p.range, self.gs_range_m) {
            c.range_loss = range_loss(r, d);
        }
        if let (Some(t), Some(o)) = (&p.terrain, self.obstruction_m)
            && self.env.ground_station.is_some()
        {
            let (blocked, loss) = terrain_effect(t, o);
            if blocked {
                c.blocked.get_or_insert(Cause::Terrain);
            }
            c.terrain_loss = loss;
        }
        if let (Some(mps), Some(d)) = (p.propagation_mps, self.gs_range_m) {
            c.extra_latency_ns += (d / mps * 1e9) as u64;
        }
        c
    }

    pub fn decide(&mut self, dir: Dir, now_ns: u64, len: usize, inflight: usize) -> Verdict {
        self.refresh_events();
        if self.los_state() == LosState::Unknown {
            self.los_unknown_pkts += 1;
        }
        let c = self.cond(dir, now_ns);
        let use_gated = self.gate_closed();
        let p = match &self.gated {
            Some((_, g)) if use_gated => g,
            _ => &self.params,
        };
        self.dirs[dir.idx()].decide(p, dir, &c, now_ns, len, inflight)
    }

    pub fn backlog_bytes(&self, dir: Dir, now_ns: u64) -> u64 {
        self.dirs[dir.idx()].backlog_bytes(now_ns, self.active().bw_bps[dir.idx()])
    }

    /// Names of the scripted events acting on this link at `now_ns` in either direction
    /// (inside the window, selectors and circle matching), for the stats view.
    pub fn active_events(&mut self, now_ns: u64) -> Vec<String> {
        self.refresh_events();
        let active = self.active_profile();
        self.events
            .iter()
            .map(|&i| &self.sched.events[i])
            .filter(|e| now_ns >= e.start_ns && now_ns < e.end_ns && e.selectors.carried_by(active))
            .filter(|e| match (&e.circle, &self.pos) {
                (None, _) => true,
                (Some((centre, r)), Some(p)) => haversine_m(p, centre) <= *r,
                (Some(_), None) => false,
            })
            .map(|e| e.name.clone())
            .collect()
    }

    /// The profile's nominal numbers for the stats view: long-run average loss (of the
    /// Gilbert-Elliott chain), one-way base latency and jitter, uplink bandwidth and queue.
    pub fn nominal_json(&self) -> serde_json::Value {
        let mut v = nominal_of(&self.params);
        if let Some((name, g)) = &self.gated {
            v["gated_profile"] = name.clone().into();
            v["gated"] = nominal_of(g);
        }
        v
    }

    /// Whether a scripted event or physics currently blocks the link (for the stats view).
    pub fn blocked_now(&mut self, now_ns: u64) -> Option<Cause> {
        self.refresh_events();
        self.cond(Dir::Up, now_ns).blocked
    }
}

/// A profile's nominal numbers: long-run average loss of its Gilbert-Elliott chain, one-way
/// base latency and jitter, uplink bandwidth and queue, and its physics switches.
fn nominal_of(p: &Params) -> serde_json::Value {
    let loss = p.mean_loss();
    serde_json::json!({
        "loss": (loss * 1e4).round() / 1e4,
        "latency_ms": p.latency_ns as f64 / 1e6,
        "jitter_ms": p.jitter_ns as f64 / 1e6,
        "bandwidth_bps": p.bw_bps[Dir::Up.idx()],
        "queue_bytes": p.queue_bytes[Dir::Up.idx()],
        "propagation_mps": p.propagation_mps,
        "space_class": p.space_class,
        "down_when": p.down_when.map(|d| match d {
            DownWhen::Submerged => "submerged",
            DownWhen::Surfaced => "surfaced",
        }),
        "terrain": p.terrain.is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::geo::{LosParams, NoTerrain};
    use crate::mavpos::position_frame;
    use crate::timeline::Timeline;

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
        Link::new(
            m,
            params,
            Arc::new(Env::new(&cfg, Arc::new(NoTerrain), 9).expect("env")),
        )
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

    /// A GNSS-only jammer event (the stand-in engine's `gnss` table) leaves the datalink
    /// alone; with `loss` it jams both.
    #[test]
    fn gnss_jammer_event_spares_the_datalink_unless_told() {
        let gnss_only = r#"
            [class.los]
            [[event]]
            at_s = 0
            circle = { lat = 37.8, lon = -122.4, radius_m = 1000 }
            gnss = { effect = "jam", power_w = 1.0 }
        "#;
        let mut v = build(gnss_only, meta(1, "los", "air"));
        v.observe_up(&position_frame(0, 37.801, -122.4, 10.0));
        assert_eq!(lost_fraction(&mut v, Dir::Up, NS, 100), 0.0);
        let both = gnss_only.replace("gnss =", "loss = 1.0\n            gnss =");
        let mut v = build(&both, meta(1, "los", "air"));
        v.observe_up(&position_frame(0, 37.801, -122.4, 10.0));
        assert_eq!(lost_fraction(&mut v, Dir::Up, NS, 100), 1.0);
    }

    #[test]
    fn active_events_name_what_acts_on_a_link_now() {
        let toml = r#"
            [class.los]
            loss = 0.1
            latency_ms = 15
            bandwidth_bps = 1000
            queue_bytes = 256
            [[event]]
            name = "jammer"
            at_s = 0
            duration_s = 100
            circle = { lat = 37.8, lon = -122.4, radius_m = 1000 }
            [[event]]
            name = "loss window"
            at_s = 50
            duration_s = 10
            class = "los"
            loss = 0.3
        "#;
        let mut l = build(toml, meta(1, "los", "air"));
        assert!(l.active_events(NS).is_empty()); // no position yet: outside every circle
        l.observe_up(&position_frame(0, 37.801, -122.4, 10.0));
        assert_eq!(l.active_events(NS), ["jammer"]);
        assert_eq!(l.active_events(55 * NS), ["jammer", "loss window"]);
        assert!(l.active_events(100 * NS).is_empty());
        let n = l.nominal_json();
        assert!((n["loss"].as_f64().unwrap_or(0.0) - 0.1).abs() < 1e-3, "{n}");
        assert_eq!(n["latency_ms"], 15.0);
        assert_eq!(n["bandwidth_bps"], 1000);
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
    fn gated_profile_carries_the_traffic_at_the_surface() {
        let toml = r#"
            [class.acoustic]
            down_when = "surfaced"
            gated_profile = "surface"
            latency_ms = 100
            [class.surface]
            latency_ms = 700
        "#;
        let cfg = Config::parse(toml).expect("cfg");
        let p = |n: &str| Params::from_profile(&cfg.resolve(n).expect("p")).expect("params");
        let mut l = Link::new(
            meta(1, "acoustic", "subsurface"),
            p("acoustic"),
            Arc::new(Env::new(&cfg, Arc::new(NoTerrain), 9).expect("env")),
        )
        .with_gated("surface", p("surface"));
        let delay = |l: &mut Link, t: u64| match l.decide(Dir::Up, t, 50, 0) {
            Verdict::Forward { at_ns, .. } => (at_ns - t) / 1_000_000,
            Verdict::Drop(c) => panic!("dropped: {c:?}"),
        };
        l.observe_up(&position_frame(0, 37.8, -122.4, 0.0)); // surfaced: not dropped
        assert_eq!(l.active_profile(), "surface");
        assert_eq!(delay(&mut l, NS), 700);
        l.observe_up(&position_frame(1, 37.8, -122.4, -20.0));
        assert_eq!(l.active_profile(), "acoustic");
        assert_eq!(delay(&mut l, 10 * NS), 100);
        assert_eq!(l.nominal_json()["gated"]["latency_ms"], 700.0);
        // a gated profile may not be gated itself
        let bad = "[class.a]\ndown_when = \"surfaced\"\ngated_profile = \"b\"\n[class.b]\ndown_when = \"submerged\"\n";
        assert!(Config::parse(bad).is_err());
    }

    /// murmur-k8i item 3: an event can name the profile carrying the traffic. `active_profile =
    /// "acoustic"` cuts an AUV's modem under water and leaves its radio at the surface alone (a
    /// `class` or `profile` selector names the vehicle's link whichever profile carries it, so
    /// it closes both); `acoustic_surface` the other way round. A profile no link has is refused.
    #[test]
    fn an_event_can_cut_the_profile_carrying_the_traffic_alone() {
        let toml = |sel: &str| {
            format!(
                r#"
                [class.acoustic]
                down_when = "surfaced"
                gated_profile = "acoustic_surface"
                latency_ms = 100
                [class.acoustic_surface]
                latency_ms = 700
                [[event]]
                name = "outage"
                at_s = 10
                duration_s = 100
                cut = true
                {sel}
            "#
            )
        };
        let link = |sel: &str| {
            let cfg = Config::parse(&toml(sel)).expect("cfg");
            let p = |n: &str| Params::from_profile(&cfg.resolve(n).expect("p")).expect("params");
            Link::new(
                meta(1, "acoustic", "subsurface"),
                p("acoustic"),
                Arc::new(Env::new(&cfg, Arc::new(NoTerrain), 9).expect("env")),
            )
            .with_gated("acoustic_surface", p("acoustic_surface"))
        };
        let blocked = |l: &mut Link, depth: f64, t: u64| {
            l.observe_up(&position_frame(0, 37.8, -122.4, depth));
            (l.blocked_now(t).is_some(), l.active_events(t).len())
        };
        // the modem alone: cut under water during the event, the radio at the surface open
        let mut modem = link(r#"active_profile = "acoustic""#);
        assert_eq!(blocked(&mut modem, -20.0, 5 * NS), (false, 0), "before the event");
        assert_eq!(blocked(&mut modem, -20.0, 20 * NS), (true, 1), "the modem is cut");
        assert!(matches!(modem.decide(Dir::Up, 20 * NS, 50, 0), Verdict::Drop(Cause::Cut)));
        assert_eq!(blocked(&mut modem, 0.0, 30 * NS), (false, 0), "surfaced: its radio carries the traffic");
        assert!(matches!(modem.decide(Dir::Up, 30 * NS, 50, 0), Verdict::Forward { .. }));
        assert_eq!(blocked(&mut modem, -20.0, 40 * NS), (true, 1), "under water again");
        assert_eq!(blocked(&mut modem, -20.0, 120 * NS), (false, 0), "after the event");
        // the radio alone
        let mut radio = link(r#"active_profile = "acoustic_surface""#);
        assert_eq!(blocked(&mut radio, -20.0, 20 * NS), (false, 0));
        assert_eq!(blocked(&mut radio, 0.0, 30 * NS), (true, 1));
        // by class: both, as before
        let mut both = link(r#"class = "acoustic""#);
        assert_eq!(blocked(&mut both, -20.0, 20 * NS), (true, 1));
        assert_eq!(blocked(&mut both, 0.0, 30 * NS), (true, 1));
        // a profile no link has is refused, not silently matched by nothing
        let err = Config::parse(&toml(r#"active_profile = "acoustc""#)).expect_err("refused");
        assert!(err.contains("active_profile"), "{err}");
    }

    /// A terrain whose answer the test can change.
    struct Fake(std::sync::Mutex<Option<f64>>);
    impl Terrain for Fake {
        fn obstruction_m(&self, _: &GeoPoint, _: &GeoPoint, _: &LosParams) -> Option<f64> {
            *self.0.lock().expect("lock")
        }
    }

    fn terrain_link(profile: &str, t: Arc<Fake>) -> Link {
        let cfg = Config::parse(&format!(
            "[ground_station]\nlat = 37.8\nlon = -122.4\n[class.los]\nrange_ok_km = 10\nrange_max_km = 20\n{profile}"
        ))
        .expect("cfg");
        let params = Params::from_profile(&cfg.resolve("los").expect("p")).expect("params");
        Link::new(
            meta(1, "los", "air"),
            params,
            Arc::new(Env::new(&cfg, t, 1).expect("env")),
        )
    }

    #[test]
    fn terrain_unknown_is_open_and_counted_then_block_follows_the_latest_answer() {
        let t = Arc::new(Fake(std::sync::Mutex::new(None)));
        let mut l = terrain_link("", t.clone());
        l.observe_up(&position_frame(0, 37.81, -122.4, 0.0));
        assert_eq!(l.los_state(), LosState::Unknown);
        assert_eq!(lost_fraction(&mut l, Dir::Up, NS, 50), 0.0);
        assert_eq!(l.los_unknown_pkts, 50);
        // The answer arrives: obstructed by 25 m. Hard block, cause terrain.
        *t.0.lock().expect("lock") = Some(25.0);
        l.observe_up(&position_frame(1, 37.81, -122.4, 0.0));
        assert_eq!(l.los_state(), LosState::Obstructed(25.0));
        assert_eq!(l.decide(Dir::Up, NS, 50, 0), Verdict::Drop(Cause::Terrain));
        assert_eq!(
            l.decide(Dir::Down, NS, 50, 0),
            Verdict::Drop(Cause::Terrain)
        );
        // The vehicle moves into a cell with no answer yet: the last answer is kept.
        *t.0.lock().expect("lock") = None;
        l.observe_up(&position_frame(2, 37.82, -122.4, 0.0));
        assert_eq!(l.los_state(), LosState::Obstructed(25.0));
        assert_eq!(l.los_unknown_pkts, 50);
        // Clear by 12 m.
        *t.0.lock().expect("lock") = Some(-12.0);
        l.observe_up(&position_frame(3, 37.82, -122.4, 0.0));
        assert_eq!(l.los_state(), LosState::Clear(12.0));
        assert_eq!(lost_fraction(&mut l, Dir::Up, NS, 50), 0.0);
    }

    #[test]
    fn terrain_ramp_mode_loses_a_share_of_packets_that_grows_with_obstruction_depth() {
        let t = Arc::new(Fake(std::sync::Mutex::new(Some(15.0))));
        let mut l = terrain_link("terrain = \"ramp\"\nterrain_ramp_m = 60\n", t.clone());
        l.observe_up(&position_frame(0, 37.81, -122.4, 0.0));
        let a = lost_fraction(&mut l, Dir::Up, NS, 20_000);
        assert!((a - 0.25).abs() < 0.02, "{a}");
        *t.0.lock().expect("lock") = Some(90.0);
        l.observe_up(&position_frame(1, 37.81, -122.4, 0.0));
        assert_eq!(l.decide(Dir::Up, NS, 50, 0), Verdict::Drop(Cause::Terrain));
        assert_eq!(lost_fraction(&mut l, Dir::Up, NS, 1000), 1.0);
        *t.0.lock().expect("lock") = Some(-5.0);
        l.observe_up(&position_frame(2, 37.81, -122.4, 0.0));
        assert_eq!(lost_fraction(&mut l, Dir::Up, NS, 1000), 0.0);
    }

    #[test]
    fn terrain_off_profile_ignores_the_terrain() {
        let t = Arc::new(Fake(std::sync::Mutex::new(Some(500.0))));
        let mut l = terrain_link("terrain = \"off\"\n", t);
        l.observe_up(&position_frame(0, 37.81, -122.4, 0.0));
        assert_eq!(l.los_state(), LosState::Off);
        assert_eq!(lost_fraction(&mut l, Dir::Up, NS, 100), 0.0);
        assert_eq!(l.los_unknown_pkts, 0);
    }

    #[test]
    fn scenario_time_events_and_reload_take_effect_at_the_next_packet() {
        let toml = r#"
            [class.los]
            [[event]]
            at_scenario = "2026-06-14T10:05:00Z"
            duration_s = 120
            class = "los"
            cut = true
        "#;
        let cfg = Config::parse(toml).expect("cfg");
        // No MURMUR_AT: scenario-time events are an error, not silently ignored.
        assert!(Env::new(&cfg, Arc::new(NoTerrain), 1).is_err());
        // Scenario clock reads 10:00 at start, 10x: the event runs from 30 s to 42 s.
        let tl = Timeline::new(Some("2026-06-14T10:00:00Z"), Some("10")).expect("tl");
        let env = Arc::new(Env::with_timeline(&cfg, Arc::new(NoTerrain), 1, tl).expect("env"));
        let params = Params::from_profile(&cfg.resolve("los").expect("p")).expect("params");
        let mut l = Link::new(meta(1, "los", "air"), params, env.clone());
        assert_eq!(lost_fraction(&mut l, Dir::Up, 29 * NS, 50), 0.0);
        assert_eq!(lost_fraction(&mut l, Dir::Up, 31 * NS, 50), 1.0);
        assert_eq!(lost_fraction(&mut l, Dir::Up, 43 * NS, 50), 0.0);
        // Reload: a cut for uid 1 from t = 100 s (emulator seconds) on.
        let next = Config::parse("[class.los]\n[[event]]\nat_s = 100\nuid = 1\ncut = true\n")
            .expect("cfg2");
        assert_eq!(env.reload_events(&next.event), Ok(1));
        assert_eq!(
            lost_fraction(&mut l, Dir::Up, 31 * NS, 50),
            0.0,
            "old event is gone"
        );
        assert_eq!(lost_fraction(&mut l, Dir::Up, 101 * NS, 50), 1.0);
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
    struct Scripted(std::sync::Mutex<crate::sat::SatObs>);

    impl crate::sat::SpaceSource for Scripted {
        fn check(&self, _: &str, _: f64) -> Result<(), String> {
            Ok(())
        }
        fn sample(&self, _: &str, _: &GeoPoint, _: f64) -> Result<crate::sat::SatObs, String> {
            Ok(self.0.lock().expect("lock").clone())
        }
        fn outlook(
            &self,
            _: &str,
            _: &GeoPoint,
            _: f64,
            _: f64,
        ) -> Result<crate::sat::Outlook, String> {
            Ok(crate::sat::Outlook::default())
        }
    }

    #[test]
    fn satcom_follows_the_constellation_on_the_packet_path() {
        use crate::sat::{BestSat, SatObs, refresh};
        let toml = r#"
            [class.satcom_leo]
            space_class = "starlink"
            latency_ms = 25
            handover_outage_ms = 100
        "#;
        let mut l = build(toml, meta(1, "satcom_leo", "surface"));
        let sat = l.sat_shared().expect("a space_class link has SATCOM state");
        let src = Scripted(std::sync::Mutex::new(SatObs {
            visible: 3,
            best: Some(BestSat {
                norad_id: 1,
                name: "A".into(),
                elevation_deg: 50.0,
                range_km: 600.0,
            }),
            latency_ms: Some(40.0),
        }));
        let cfg = crate::config::SpaceCfg::default();
        let latency_of = |l: &mut Link, now: u64| match l.decide(Dir::Up, now, 50, 0) {
            Verdict::Forward { at_ns, .. } => Some((at_ns - now) as f64 / 1e6),
            Verdict::Drop(_) => None,
        };
        // Before any position and any answer: the static profile (25 ms).
        assert_eq!(latency_of(&mut l, NS), Some(25.0));
        l.observe_up(&position_frame(0, 37.8, -122.5, 0.0));
        assert_eq!(latency_of(&mut l, 2 * NS), Some(25.0), "pending: static");
        // Live: the satellite's latency replaces the profile's.
        refresh(&src, &cfg, &sat, 100.0, 3 * NS);
        assert_eq!(latency_of(&mut l, 3 * NS), Some(40.0));
        // Handover to another satellite: dropped for 100 ms, then open again.
        src.0
            .lock()
            .expect("lock")
            .best
            .as_mut()
            .expect("best")
            .norad_id = 2;
        refresh(&src, &cfg, &sat, 105.0, 4 * NS);
        assert_eq!(
            l.decide(Dir::Up, 4 * NS + 50_000_000, 50, 0),
            Verdict::Drop(Cause::Handover)
        );
        assert_eq!(latency_of(&mut l, 4 * NS + 100_000_000), Some(40.0));
        // No satellite above the mask: down, both directions, until one rises.
        *src.0.lock().expect("lock") = SatObs {
            visible: 0,
            best: None,
            latency_ms: None,
        };
        refresh(&src, &cfg, &sat, 110.0, 5 * NS);
        for dir in [Dir::Up, Dir::Down] {
            assert_eq!(l.decide(dir, 5 * NS, 50, 0), Verdict::Drop(Cause::NoSat));
        }
        assert_eq!(l.blocked_now(5 * NS), Some(Cause::NoSat));
    }

    #[test]
    fn a_profile_without_space_class_has_no_satcom_state() {
        let l = build("[class.los]\n", meta(1, "los", "air"));
        assert!(l.sat_shared().is_none());
    }

    /// Records each question the link asks its terrain: (altitude, at rest).
    #[derive(Default)]
    struct Asked(std::sync::Mutex<Vec<(f64, bool)>>);
    impl Terrain for Asked {
        fn obstruction_m(&self, v: &GeoPoint, _: &GeoPoint, _: &LosParams) -> Option<f64> {
            self.0.lock().expect("lock").push((v.alt_m, false));
            Some(17.0)
        }
        fn obstruction_at_rest_m(&self, v: &GeoPoint, _: &GeoPoint, _: &LosParams) -> Option<f64> {
            self.0.lock().expect("lock").push((v.alt_m, true));
            Some(13.0)
        }
    }

    /// An autopilot heartbeat, then a position `rel_m` above home, in one datagram.
    fn heartbeat_and_position(armed: bool, alt_m: f64, rel_m: f64) -> Vec<u8> {
        use mavlink::dialects::ardupilotmega as apm;
        let h = mavlink::MavHeader {
            sequence: 0,
            system_id: 1,
            component_id: 1,
        };
        let hb = apm::MavMessage::HEARTBEAT(apm::HEARTBEAT_DATA {
            autopilot: apm::MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA,
            mavtype: apm::MavType::MAV_TYPE_FIXED_WING,
            base_mode: if armed {
                apm::MavModeFlag::MAV_MODE_FLAG_SAFETY_ARMED
            } else {
                apm::MavModeFlag::empty()
            },
            ..Default::default()
        });
        let pos = apm::MavMessage::GLOBAL_POSITION_INT(apm::GLOBAL_POSITION_INT_DATA {
            lat: (37.81 * 1e7) as i32,
            lon: (-122.4 * 1e7) as i32,
            alt: (alt_m * 1e3) as i32,
            relative_alt: (rel_m * 1e3) as i32,
            ..Default::default()
        });
        let mut out = Vec::new();
        for m in [hb, pos] {
            mavlink::write_versioned_msg(&mut out, mavlink::MavlinkVersion::V2, h, &m)
                .expect("write");
        }
        out
    }

    #[test]
    fn only_a_landed_disarmed_aircraft_is_judged_at_rest_at_its_own_altitude() {
        // (domain, armed, metres above home) -> at rest
        for (domain, armed, rel, rest) in [
            ("air", false, 0.2, true),
            ("air", true, 0.2, false), // armed: a landing still rolling out, a takeoff
            ("air", false, 40.0, false), // a dead glider, not down yet
            ("surface", false, 0.0, false),
        ] {
            let t = Arc::new(Asked::default());
            let cfg = Config::parse(
                "[ground_station]\nlat = 37.8\nlon = -122.4\n[class.los]\nterrain = \"ramp\"\nterrain_ramp_m = 20\n",
            )
            .expect("cfg");
            let params = Params::from_profile(&cfg.resolve("los").expect("p")).expect("params");
            let env = Arc::new(Env::new(&cfg, t.clone(), 1).expect("env"));
            let mut l = Link::new(meta(1, "los", domain), params, env);
            l.observe_up(&heartbeat_and_position(armed, 3.0, rel));
            assert_eq!(l.at_rest(), rest, "{domain} armed {armed} rel {rel}");
            let asked = t.0.lock().expect("lock").clone();
            // at rest: its own 3 m plus the antenna, asked as such
            let want = if rest { 3.0 + AT_REST_ANTENNA_M } else { 3.0 };
            assert_eq!(asked, [(want, rest)]);
            assert_eq!(
                l.los_state(),
                LosState::Obstructed(if rest { 13.0 } else { 17.0 })
            );
        }
    }
}
