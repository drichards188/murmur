//! Lock-free counters, read by the HTTP endpoint and the periodic log line.

use crate::link::LinkMeta;
use crate::model::{CAUSES, Cause, Dir};
use crate::sat::SatShared;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[derive(Default)]
pub struct DirStats {
    pub in_pkts: AtomicU64,
    pub in_bytes: AtomicU64,
    pub out_pkts: AtomicU64,
    pub out_bytes: AtomicU64,
    pub duplicated: AtomicU64,
    pub dropped: [AtomicU64; CAUSES.len()],
    /// Gauges, refreshed by the link task.
    pub queued_pkts: AtomicU64,
    pub queued_bytes: AtomicU64,
    /// Latency added to forwarded packets (queuing, base, jitter, events), microseconds.
    pub lat_sum_us: AtomicU64,
    pub lat_n: AtomicU64,
    pub lat_max_us: AtomicU64,
}

impl DirStats {
    pub fn drop_total(&self) -> u64 {
        self.dropped.iter().map(|d| d.load(Relaxed)).sum()
    }

    fn json(&self) -> Value {
        let mut dropped = serde_json::Map::new();
        for c in CAUSES {
            let n = self.dropped[c.idx()].load(Relaxed);
            if n > 0 {
                dropped.insert(c.name().into(), n.into());
            }
        }
        let n = self.lat_n.load(Relaxed);
        json!({
            "in_pkts": self.in_pkts.load(Relaxed),
            "in_bytes": self.in_bytes.load(Relaxed),
            "out_pkts": self.out_pkts.load(Relaxed),
            "out_bytes": self.out_bytes.load(Relaxed),
            "duplicated": self.duplicated.load(Relaxed),
            "dropped_total": self.drop_total(),
            "dropped": dropped,
            "queued_pkts": self.queued_pkts.load(Relaxed),
            "queued_bytes": self.queued_bytes.load(Relaxed),
            "latency_n": n,
            "latency_ms_mean": if n == 0 { Value::Null } else { json!(self.lat_sum_us.load(Relaxed) as f64 / n as f64 / 1000.0) },
            "latency_ms_max": self.lat_max_us.load(Relaxed) as f64 / 1000.0,
        })
    }
}

pub struct LinkStats {
    pub meta: LinkMeta,
    pub dir: [DirStats; 2],
    /// 0 unknown, 1 surfaced, 2 submerged.
    pub depth_state: AtomicU64,
    /// Cause name index + 1 while the link is blocked, 0 when open.
    pub blocked: AtomicU64,
    /// f64 bits of lat, lon, alt; NaN until the first position.
    pub pos: [AtomicU64; 3],
    /// Terrain line of sight: 0 not checked, 1 unknown (no answer yet), 2 clear, 3 obstructed.
    pub los_state: AtomicU64,
    /// f64 bits: metres the terrain sticks up above the ray (negative: clear by that much).
    pub los_obstruction: AtomicU64,
    /// Packets decided while the terrain answer was unknown (treated as open).
    pub terrain_unknown: AtomicU64,
    /// SATCOM state (and the profile's static one-way latency, ms) for links that follow a
    /// constellation class.
    pub sat: Option<(Arc<SatShared>, f64)>,
    /// The profile's nominal numbers (average loss, one-way latency, jitter, bandwidth), so a
    /// viewer can compare what it measures with what the profile promises.
    pub nominal: Value,
    /// Names of the scripted events acting on this link now (refreshed with the gauges).
    pub events: Mutex<Vec<String>>,
    /// 1 while the depth gate has moved the traffic to the profile's `gated_profile`.
    pub on_gated: AtomicU64,
}

pub const LOS_NAMES: [&str; 4] = ["off", "unknown", "clear", "obstructed"];

impl LinkStats {
    pub fn new(meta: LinkMeta) -> LinkStats {
        let nan = f64::NAN.to_bits();
        LinkStats {
            meta,
            dir: Default::default(),
            depth_state: AtomicU64::new(0),
            blocked: AtomicU64::new(0),
            pos: [
                AtomicU64::new(nan),
                AtomicU64::new(nan),
                AtomicU64::new(nan),
            ],
            los_state: AtomicU64::new(0),
            los_obstruction: AtomicU64::new(nan),
            terrain_unknown: AtomicU64::new(0),
            sat: None,
            nominal: Value::Null,
            events: Mutex::new(Vec::new()),
            on_gated: AtomicU64::new(0),
        }
    }

    pub fn with_nominal(mut self, nominal: Value) -> LinkStats {
        self.nominal = nominal;
        self
    }

    pub fn set_events(&self, names: Vec<String>) {
        let mut g = self.events.lock().unwrap_or_else(|e| e.into_inner());
        if *g != names {
            *g = names;
        }
    }

    pub fn with_sat(mut self, sat: Option<Arc<SatShared>>, static_latency_ms: f64) -> LinkStats {
        self.sat = sat.map(|s| (s, static_latency_ms));
        self
    }

    pub fn d(&self, dir: Dir) -> &DirStats {
        &self.dir[dir.idx()]
    }

    pub fn record_latency(&self, dir: Dir, us: u64) {
        let d = self.d(dir);
        d.lat_sum_us.fetch_add(us, Relaxed);
        d.lat_n.fetch_add(1, Relaxed);
        d.lat_max_us.fetch_max(us, Relaxed);
    }

    pub fn json(&self) -> Value {
        let f = |i: usize| {
            let v = f64::from_bits(self.pos[i].load(Relaxed));
            if v.is_nan() { Value::Null } else { json!(v) }
        };
        let blocked = match self.blocked.load(Relaxed) {
            0 => Value::Null,
            n => json!(CAUSES[(n - 1) as usize].name()),
        };
        let obstruction = f64::from_bits(self.los_obstruction.load(Relaxed));
        let terrain_blocked: u64 = self
            .dir
            .iter()
            .map(|d| d.dropped[Cause::Terrain.idx()].load(Relaxed))
            .sum();
        let depth =
            ["unknown", "surfaced", "submerged"][self.depth_state.load(Relaxed).min(2) as usize];
        json!({
            "uid": self.meta.uid,
            "sysid": self.meta.sysid,
            "port": self.meta.port,
            "class": self.meta.class,
            "profile": self.meta.profile,
            "domain": self.meta.domain,
            "platform": self.meta.platform,
            "blocked": blocked,
            "depth_state": depth,
            "los": {
                "state": LOS_NAMES[self.los_state.load(Relaxed).min(3) as usize],
                "obstruction_m": if obstruction.is_nan() { Value::Null } else { json!(obstruction) },
                "unknown_pkts": self.terrain_unknown.load(Relaxed),
                "blocked_by_terrain_pkts": terrain_blocked,
            },
            "satcom": self.sat.as_ref().map(|(s, ms)| s.json(*ms)),
            "nominal": self.nominal,
            "active_profile": if self.on_gated.load(Relaxed) == 1 {
                self.nominal["gated_profile"].clone()
            } else {
                json!(self.meta.profile)
            },
            "events": self.events.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            "pos": { "lat": f(0), "lon": f(1), "alt_m": f(2) },
            "up": self.dir[0].json(),
            "down": self.dir[1].json(),
        })
    }
}

pub struct Totals {
    pub in_pkts: u64,
    pub out_pkts: u64,
    pub dropped: u64,
    pub by_cause: [u64; CAUSES.len()],
}

pub fn totals(links: &[std::sync::Arc<LinkStats>]) -> Totals {
    let mut t = Totals {
        in_pkts: 0,
        out_pkts: 0,
        dropped: 0,
        by_cause: [0; CAUSES.len()],
    };
    for l in links {
        for d in &l.dir {
            t.in_pkts += d.in_pkts.load(Relaxed);
            t.out_pkts += d.out_pkts.load(Relaxed);
            for (i, c) in d.dropped.iter().enumerate() {
                let n = c.load(Relaxed);
                t.by_cause[i] += n;
                t.dropped += n;
            }
        }
    }
    t
}

/// User plus system CPU seconds of this process, from /proc (Linux only; 0 elsewhere).
pub fn cpu_seconds() -> f64 {
    let Ok(s) = std::fs::read_to_string("/proc/self/stat") else {
        return 0.0;
    };
    let Some(rest) = s.rsplit_once(')').map(|x| x.1) else {
        return 0.0;
    };
    let f: Vec<&str> = rest.split_whitespace().collect();
    // After the comm field: state is index 0, utime 11, stime 12; clock ticks are 100 Hz.
    let tick = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    (tick(11) + tick(12)) / 100.0
}

pub fn rss_mb() -> f64 {
    let Ok(s) = std::fs::read_to_string("/proc/self/statm") else {
        return 0.0;
    };
    s.split_whitespace()
        .nth(1)
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
        * 4096.0
        / 1e6
}

pub fn summary_json(
    links: &[std::sync::Arc<LinkStats>],
    uptime_s: f64,
    with_links: bool,
    space: Option<&crate::sat::SpaceInfo>,
) -> Value {
    let t = totals(links);
    let mut by = serde_json::Map::new();
    for c in CAUSES {
        if t.by_cause[c.idx()] > 0 {
            by.insert(c.name().into(), t.by_cause[c.idx()].into());
        }
    }
    let mut los = [0u64; 4];
    for l in links {
        los[l.los_state.load(Relaxed).min(3) as usize] += 1;
    }
    let mut v = json!({
        "uptime_s": uptime_s,
        "los_links": { "unknown": los[1], "clear": los[2], "obstructed": los[3] },
        "terrain_blocked_pkts": t.by_cause[Cause::Terrain.idx()],
        "terrain_unknown_pkts": links.iter().map(|l| l.terrain_unknown.load(Relaxed)).sum::<u64>(),
        "links": links.len(),
        "in_pkts": t.in_pkts,
        "out_pkts": t.out_pkts,
        "dropped_total": t.dropped,
        "dropped": by,
        "cpu_seconds": cpu_seconds(),
        "rss_mb": rss_mb(),
    });
    let sat_links: Vec<&Arc<LinkStats>> = links.iter().filter(|l| l.sat.is_some()).collect();
    if !sat_links.is_empty() {
        let (mut live, mut down) = (0u64, 0u64);
        for l in &sat_links {
            if let Some((s, _)) = &l.sat {
                let snap = s.snapshot();
                if snap.mode == crate::sat::SatMode::Live {
                    live += 1;
                    down += u64::from(!snap.up);
                }
            }
        }
        let mut sp = space.map_or_else(|| json!({}), crate::sat::SpaceInfo::json);
        sp["satcom_links"] = sat_links.len().into();
        sp["live_links"] = live.into();
        sp["links_without_satellite"] = down.into();
        v["space"] = sp;
    }
    if with_links {
        v["per_link"] = links.iter().map(|l| l.json()).collect::<Vec<_>>().into();
    }
    v
}
