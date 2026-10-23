//! The link emulator in memory: murmur-linkemu's own model (`Link::decide`, its delay queues
//! `proxy::{admit, Delay}`, its gauges `proxy::gauge`, its stats and `/stats` JSON, its terrain
//! cache and SATCOM refresh), with the two sockets of each link replaced by calls: a vehicle's
//! frames come from the stand-in after each step, the bridge's from its in-memory `Link`; due
//! packets are delivered in time order (then link, then direction) by one task on the paused
//! clock. What is not linkemu's here is only that socket loop (`proxy::run_link`), restated
//! below, and the order of deliveries due at the same instant on different links (one task
//! here, one task per link there).

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use murmur_linkemu::config::{Config, EventCfg, VehicleSpec};
use murmur_linkemu::geo::Terrain as _;
use murmur_linkemu::link::{Env, Link as ELink};
use murmur_linkemu::model::{Cause, Dir};
use murmur_linkemu::proxy::{Delay, admit, build_links, gauge};
use murmur_linkemu::sat::{RealSpace, SatShared, SpaceInfo, SpaceSource, refresh};
use murmur_linkemu::stats::{LinkStats, summary_json};
use murmur_linkemu::terrain::TidewaterTerrain;
use murmur_linkemu::timeline::Timeline;
use serde_json::Value;
use tokio::sync::Notify;
use tokio::time::Instant;

use super::fleet::Fleet;
use super::space_cache::CachedSpace;
use super::terrain::{BakedSampler, Grid};
use crate::link::Link;
use crate::world::World;

/// One vehicle's link: the emulator's model of it and its two delay queues.
struct Lane {
    link: ELink,
    delay: [Delay; 2],
    /// The vehicle has sent something (`run_link`'s `peer`): until then the bridge's frames
    /// are dropped, `no_peer`.
    peer: bool,
    last_gauge: u64,
}

pub struct Emulator {
    start: Instant,
    pub env: Arc<Env>,
    timeline: Timeline,
    lanes: Mutex<Vec<Lane>>,
    pub stats: Vec<Arc<LinkStats>>,
    notify: Notify,
    events: Mutex<Vec<EventCfg>>,
    bridge: OnceLock<Vec<Arc<Link>>>,
    /// When the bridge is up (`Spec::bridge_start_s`): a frame for it before then is lost.
    bridge_up: OnceLock<Instant>,
    world: OnceLock<Arc<World>>,
    fleet: OnceLock<Arc<Fleet>>,
    terrain: Option<(Arc<TidewaterTerrain>, BakedSampler)>,
    /// `[terrain] interval_s` of the links file: how often the cache asks for what it misses.
    terrain_interval_s: f64,
    space: Option<LiveSpace>,
    space_info: Option<Arc<SpaceInfo>>,
    space_cache: Option<Arc<CachedSpace>>,
    pub uids: Vec<u32>,
}

/// What the emulator is started with (`murmur-linkemu`'s arguments, as the episode has them).
pub struct Setup<'a> {
    pub links_file: &'a std::path::Path,
    pub events: Vec<EventCfg>,
    pub fleet: &'a [VehicleSpec],
    pub seed: u64,
    pub epoch: f64,
    pub grid: Option<Arc<Grid>>,
    pub space: bool,
    pub space_outlook: bool,
    /// Where SATCOM looks are kept between episodes (`space_cache`).
    pub space_cache: Option<std::path::PathBuf>,
    /// A pinned space snapshot's root (`Spec::space_root`): the catalog and element sets under
    /// its `sim/space/`, the same the bridge's space segment reads; none: the links file's.
    pub space_root: Option<&'a std::path::Path>,
}

/// The SATCOM source, its settings and the links that follow a constellation.
type LiveSpace = (Arc<dyn SpaceSource>, murmur_linkemu::config::SpaceCfg, Vec<Arc<SatShared>>);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Emulator {
    /// The emulator as `murmur-linkemu` starts it: the links file and the episode's events,
    /// the fleet (vehicle-side manifest), the seed, the scenario clock from `epoch`, the
    /// terrain from the baked grid (the ground station's altitude settled against it, as the
    /// emulator settles it against tidewater's DEM), SATCOM from murmur-space's data.
    pub fn new(setup: Setup<'_>, start: Instant) -> Result<Arc<Emulator>, String> {
        let Setup { links_file, events, fleet, seed, epoch, grid, space, space_outlook, space_cache, space_root } = setup;
        let mut cfg = Config::load(links_file)?;
        cfg.event.extend(events.iter().cloned());
        if let Some(root) = space_root {
            let dir = root.join("sim/space");
            cfg.space.catalog = dir.join("constellations.toml").to_string_lossy().into_owned();
            cfg.space.cache = dir.join("cache").to_string_lossy().into_owned();
        }
        // the SATCOM look-ahead (next outage, next pass, for `/stats` and the console) is
        // display only, and its propagation over half an hour every minute was most of an
        // episode's time: off unless asked for (docs/HEADLESS.md, what it leaves out)
        if !space_outlook {
            cfg.space.outlook_s = 0.0;
        }
        let timeline = Timeline {
            at_unix: Some(epoch),
            rate: 1.0,
            wall0: epoch,
        };
        let terrain = grid.map(|g| {
            (
                Arc::new(TidewaterTerrain::new(cfg.terrain.clone())),
                BakedSampler(g),
            )
        });
        if let Some(gs) = cfg.ground_station.clone() {
            let dem = terrain.as_ref().and_then(|(_, s)| s.0.height(gs.lat, gs.lon));
            if let Some(check) = cfg.resolve_ground_alt(dem) {
                let (warn, msg) = check.message();
                eprintln!("linkemu: {}{msg}", if warn { "WARNING " } else { "" });
            }
        }
        let dyn_terrain: Arc<dyn murmur_linkemu::geo::Terrain> = match &terrain {
            Some((t, _)) => t.clone(),
            None => Arc::new(murmur_linkemu::geo::NoTerrain),
        };
        let env = Arc::new(Env::with_timeline(&cfg, dyn_terrain, seed, timeline)?);
        let links = build_links(fleet, &cfg, &env)?;
        let sats: Vec<Arc<SatShared>> = links.iter().filter_map(ELink::sat_shared).collect();
        let stats: Vec<Arc<LinkStats>> = links
            .iter()
            .map(|l| {
                Arc::new(
                    LinkStats::new(l.meta.clone())
                        .with_sat(l.sat_shared(), l.params().latency_ns as f64 / 1e6)
                        .with_nominal(l.nominal_json()),
                )
            })
            .collect();
        let uids = links.iter().map(|l| l.meta.uid).collect();
        // SATCOM: load the element sets once, check each class at the start (`sat::spawn`)
        let mut cache_handle: Option<Arc<CachedSpace>> = None;
        let (space_src, space_info) = if sats.is_empty() {
            (None, None)
        } else {
            let info = Arc::new(SpaceInfo::default());
            *info.interval_s.lock().unwrap_or_else(|e| e.into_inner()) = cfg.space.interval_s;
            let loaded = if space {
                RealSpace::load(&cfg.space, timeline.scenario_unix(0.0))
            } else {
                Err("disabled for this episode".to_string())
            };
            if let Ok(r) = &loaded {
                let tm = r.time_map();
                *info.time_map.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some((tm.shift_days, tm.reference.to_rfc3339()));
            }
            let cached = loaded.map(|r| Arc::new(CachedSpace::new(Arc::new(r), space_cache.clone())));
            if let Ok(c) = &cached {
                cache_handle = Some(c.clone());
            }
            let source = cached.map(|c| c as Arc<dyn SpaceSource>);
            let start_unix = timeline.scenario_unix(0.0);
            let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
            for s in &sats {
                *counts.entry(s.class.clone()).or_default() += 1;
            }
            let mut live = Vec::new();
            let mut table = std::collections::BTreeMap::new();
            for (class, n) in counts {
                let verdict = match &source {
                    Ok(src) => src.check(&class, start_unix),
                    Err(e) => Err(e.clone()),
                };
                for s in sats.iter().filter(|s| s.class == class) {
                    match &verdict {
                        Ok(()) => live.push(s.clone()),
                        Err(e) => s.set_static(e),
                    }
                }
                table.insert(class, (n, verdict.err()));
            }
            *info.classes.lock().unwrap_or_else(|e| e.into_inner()) = table;
            if let Err(e) = &source {
                *info.source_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.clone());
            }
            match source {
                Ok(src) if !live.is_empty() => (Some((src, cfg.space.clone(), live)), Some(info)),
                _ => (None, Some(info)),
            }
        };
        let lanes = links
            .into_iter()
            .map(|link| Lane {
                link,
                delay: Default::default(),
                peer: false,
                last_gauge: 0,
            })
            .collect();
        Ok(Arc::new(Emulator {
            start,
            env,
            timeline,
            lanes: Mutex::new(lanes),
            stats,
            notify: Notify::new(),
            events: Mutex::new(cfg.event.clone()),
            bridge: OnceLock::new(),
            bridge_up: OnceLock::new(),
            world: OnceLock::new(),
            fleet: OnceLock::new(),
            terrain,
            terrain_interval_s: cfg.terrain.interval_s.max(0.05),
            space: space_src,
            space_info,
            space_cache: cache_handle,
            uids,
        }))
    }

    /// Wire the two ends: the bridge's in-memory links (by vehicle index) and the stand-ins.
    pub fn connect(&self, bridge: Vec<Arc<Link>>, world: Arc<World>, fleet: Arc<Fleet>, bridge_up: Instant) {
        let _ = self.bridge.set(bridge);
        let _ = self.bridge_up.set(bridge_up);
        let _ = self.world.set(world);
        let _ = self.fleet.set(fleet);
    }

    fn now_ns(&self) -> u64 {
        Instant::now().saturating_duration_since(self.start).as_nanos() as u64
    }

    /// Seconds since the emulator started (its `/stats` `uptime_s`).
    pub fn uptime_s(&self) -> f64 {
        self.now_ns() as f64 / 1e9
    }

    /// Frames vehicle `i` sent now (`run_link`'s vehicle socket).
    pub fn uplink(&self, i: usize, frames: Vec<Vec<u8>>) {
        if frames.is_empty() {
            return;
        }
        let now = self.now_ns();
        {
            let mut lanes = lock(&self.lanes);
            let lane = &mut lanes[i];
            let stats = &self.stats[i];
            for f in &frames {
                lane.peer = true;
                lane.link.observe_up(f);
                if let Some(p) = lane.link.position() {
                    for (k, v) in [p.lat, p.lon, p.alt_m].into_iter().enumerate() {
                        stats.pos[k].store(v.to_bits(), Relaxed);
                    }
                }
                admit(&mut lane.link, stats, &mut lane.delay[0], Dir::Up, f, now);
            }
        }
        self.notify.notify_one();
    }

    /// A frame the bridge sent to vehicle `i` now (`run_link`'s bridge socket).
    pub fn downlink(&self, i: usize, frame: &[u8]) {
        let now = self.now_ns();
        {
            let mut lanes = lock(&self.lanes);
            let lane = &mut lanes[i];
            let stats = &self.stats[i];
            if lane.peer {
                admit(&mut lane.link, stats, &mut lane.delay[1], Dir::Down, frame, now);
            } else {
                let d = stats.d(Dir::Down);
                d.in_pkts.fetch_add(1, Relaxed);
                d.in_bytes.fetch_add(frame.len() as u64, Relaxed);
                d.dropped[Cause::NoPeer.idx()].fetch_add(1, Relaxed);
            }
        }
        self.notify.notify_one();
    }

    fn next_due(&self) -> Option<u64> {
        let lanes = lock(&self.lanes);
        lanes
            .iter()
            .flat_map(|l| l.delay.iter().filter_map(|d| d.heap.peek().map(|p| p.at_ns)))
            .min()
    }

    /// The earliest packet due by `now`: (lane, direction, data).
    fn pop_due(&self, now: u64) -> Option<(usize, Dir, Box<[u8]>)> {
        let mut lanes = lock(&self.lanes);
        let mut best: Option<(u64, usize, usize)> = None;
        for (i, lane) in lanes.iter().enumerate() {
            for (k, d) in lane.delay.iter().enumerate() {
                if let Some(p) = d.heap.peek()
                    && p.at_ns <= now
                    && best.is_none_or(|b| (p.at_ns, i, k) < b)
                {
                    best = Some((p.at_ns, i, k));
                }
            }
        }
        let (_, i, k) = best?;
        let dl = &mut lanes[i].delay[k];
        let p = dl.heap.pop()?;
        dl.bytes -= p.data.len() as u64;
        let dir = if k == 0 { Dir::Up } else { Dir::Down };
        Some((i, dir, p.data))
    }

    /// The delivery task: sleep to the earliest due packet (or a new one), deliver everything
    /// due in time order, refresh the gauges every 250 ms.
    pub async fn run(self: Arc<Self>) {
        loop {
            let next = self.next_due();
            match next {
                Some(at) => {
                    let due = self.start + Duration::from_nanos(at);
                    tokio::select! {
                        () = tokio::time::sleep_until(due) => {}
                        () = self.notify.notified() => {}
                    }
                }
                None => self.notify.notified().await,
            }
            let now = self.now_ns();
            while let Some((i, dir, data)) = self.pop_due(now) {
                self.deliver(i, dir, &data);
            }
            let mut lanes = lock(&self.lanes);
            for (i, lane) in lanes.iter_mut().enumerate() {
                if now.saturating_sub(lane.last_gauge) > 250_000_000 {
                    lane.last_gauge = now;
                    gauge(&mut lane.link, &self.stats[i], &lane.delay, now);
                }
            }
        }
    }

    fn deliver(&self, i: usize, dir: Dir, data: &[u8]) {
        let d = self.stats[i].d(dir);
        d.out_pkts.fetch_add(1, Relaxed);
        d.out_bytes.fetch_add(data.len() as u64, Relaxed);
        match dir {
            Dir::Up => {
                // sent, as the emulator sends it, to a bridge port nobody holds yet: lost
                if self.bridge_up.get().is_some_and(|up| Instant::now() < *up) {
                    return;
                }
                if let (Some(bridge), Some(world)) = (self.bridge.get(), self.world.get()) {
                    let port = self.vehicle_port(i);
                    let from = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
                    bridge[i].receive(world, data, from);
                }
            }
            Dir::Down => {
                if let Some(fleet) = self.fleet.get() {
                    let frames = fleet.deliver(i, data);
                    self.uplink(i, frames);
                }
            }
        }
    }

    fn vehicle_port(&self, i: usize) -> u16 {
        self.stats[i].meta.port
    }

    /// The terrain cache's background round (`TidewaterTerrain::run`), on the paused clock.
    pub async fn run_terrain(self: Arc<Self>) {
        if self.terrain.is_none() {
            return;
        }
        let every = Duration::from_secs_f64(self.terrain_interval_s);
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Some((t, sampler)) = &self.terrain {
                let unix = self.timeline.scenario_unix(self.uptime_s());
                t.tick(sampler, unix).await;
            }
        }
    }

    /// SATCOM: every link that follows a constellation refreshed (`sat::refresh`) every
    /// `interval_s` of scenario time, as `sat::spawn` does at rate 1.
    pub async fn run_space(self: Arc<Self>) {
        let Some((src, cfg, live)) = &self.space else { return };
        let every = (cfg.interval_s / self.timeline.rate).max(cfg.min_wall_s).max(0.05);
        loop {
            let unix = self.timeline.scenario_unix(self.uptime_s());
            let now_ns = self.now_ns();
            for s in live {
                refresh(src.as_ref(), cfg, s, unix, now_ns);
            }
            if let Some(info) = &self.space_info {
                info.rounds.fetch_add(1, Relaxed);
            }
            tokio::time::sleep(Duration::from_secs_f64(every)).await;
        }
    }

    /// The `/stats` answer the bridge's `emu` polls (`http::route`).
    pub fn stats_json(&self) -> Value {
        let mut v = summary_json(&self.stats, self.uptime_s(), true, self.space_info.as_deref());
        if let Some((t, _)) = &self.terrain
            && let Some(s) = t.stats()
        {
            v["terrain"] = s;
        }
        v
    }

    /// A scripted link event from now on (the storyline's `link` step appends to the events
    /// file and the emulator reloads it; here the schedule is reloaded with it).
    /// Keep this episode's SATCOM looks for the next ones.
    pub fn save_space_cache(&self) {
        if let Some(c) = &self.space_cache {
            c.save();
        }
    }

    pub fn add_event(&self, e: EventCfg) -> Result<usize, String> {
        let mut events = lock(&self.events);
        events.push(e);
        self.env.reload_events(&events)
    }

    /// Whether vehicle `i`'s link is blocked now, and by what (`Link::blocked_now`).
    pub fn blocked(&self, i: usize) -> Option<String> {
        let now = self.now_ns();
        let mut lanes = lock(&self.lanes);
        lanes[i].link.blocked_now(now).map(|c| c.name().to_string())
    }
}
