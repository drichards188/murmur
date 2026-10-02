//! SATCOM links that follow real satellite visibility, from murmur-space.
//!
//! A profile with `space_class = "starlink"` (or `iridium`, `wgs`, `ku-geo`) is not left to
//! static numbers: for each vehicle, from its reported position and the scenario clock, a
//! background task works out whether any satellite of the class is above the terminal's mask,
//! which one is best, and the one-way latency through it. The packet path only reads the
//! latest answer (like the terrain line-of-sight cache); it never waits for SGP4.
//!
//! * No satellite above the mask: the link is down (cause `no_sat`).
//! * Best satellite changes between two samples while the link stays up: a handover. It is
//!   counted, and the link drops for the profile's `handover_outage_ms` (cause `handover`,
//!   0 by default; the shipped figures are guesses).
//! * Latency: slant range legs plus the class's network latency, from murmur-space. Where
//!   the class has no latency model (Iridium) or there is no answer, the profile's static
//!   `latency_ms` applies, and the stats say which.
//! * No element sets (cache missing), a class with nothing usable, or a failing source: the
//!   profile's static parameters apply, in the log and in `/stats`, never silently.
//!
//! "Best" is the highest-elevation satellite. A real terminal holds its satellite longer than
//! that, so handovers here are somewhat more frequent than operational Starlink's ~15 s.

use crate::config::SpaceCfg;
use crate::geo::GeoPoint;
use crate::model::Cause;
use crate::timeline::Timeline;
use chrono::{DateTime, Duration, Utc};
use murmur_space::geo::Geodetic;
use murmur_space::{EpochMode, LinkQuery, Space, TimeMap};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration as StdDuration;
use tokio::time::Instant;

/// The satellite currently serving a terminal.
#[derive(Clone, Debug, PartialEq)]
pub struct BestSat {
    pub norad_id: u64,
    pub name: String,
    pub elevation_deg: f64,
    pub range_km: f64,
}

/// One instantaneous look at the constellation from a terminal.
#[derive(Clone, Debug, PartialEq)]
pub struct SatObs {
    /// Satellites above the mask.
    pub visible: usize,
    pub best: Option<BestSat>,
    /// One-way latency through the best satellite; `None` if the class has no latency model.
    pub latency_ms: Option<f64>,
}

/// What the next stretch looks like (scenario unix seconds).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Outlook {
    /// Start of the next interval with no satellite (given when the link is up now).
    pub next_outage_unix: Option<f64>,
    /// When a satellite next comes into view (given when the link is down now).
    pub next_up_unix: Option<f64>,
}

/// Where satellite geometry comes from: murmur-space, or a fake in tests.
pub trait SpaceSource: Send + Sync {
    /// Whether the class can be used at all at `unix_s` (known, has usable satellites).
    fn check(&self, class: &str, unix_s: f64) -> Result<(), String>;
    fn sample(&self, class: &str, pos: &GeoPoint, unix_s: f64) -> Result<SatObs, String>;
    fn outlook(
        &self,
        class: &str,
        pos: &GeoPoint,
        unix_s: f64,
        horizon_s: f64,
    ) -> Result<Outlook, String>;
}

fn to_dt(unix_s: f64) -> DateTime<Utc> {
    let secs = unix_s.floor();
    let nanos = ((unix_s - secs) * 1e9) as u32;
    DateTime::from_timestamp(secs as i64, nanos.min(999_999_999)).unwrap_or_default()
}

fn to_unix(t: DateTime<Utc>) -> f64 {
    t.timestamp() as f64 + f64::from(t.timestamp_subsec_nanos()) / 1e9
}

/// Scenario unix seconds as an RFC 3339 string.
pub fn iso(unix_s: f64) -> String {
    to_dt(unix_s).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn geodetic(p: &GeoPoint) -> Geodetic {
    // A terminal below the sea surface (a submerged vehicle) is gated elsewhere; treat it as
    // at the surface here.
    Geodetic::new(p.lat, p.lon, p.alt_m.max(0.0) / 1000.0)
}

/// The real thing: a loaded `murmur_space::Space` and one fixed scenario-to-propagation time
/// map for the whole run (so a day shift cannot flip mid-run).
pub struct RealSpace {
    space: Space,
    tm: TimeMap,
}

impl RealSpace {
    /// Loads the catalog and cached element sets. `start_unix` is the scenario time at start.
    pub fn load(cfg: &SpaceCfg, start_unix: f64) -> Result<RealSpace, String> {
        let space = Space::load(Path::new(&cfg.catalog), Path::new(&cfg.cache)).map_err(|e| {
            format!(
                "{e} (run `murmur-space fetch`; catalog {}, cache {})",
                cfg.catalog, cfg.cache
            )
        })?;
        let mode = match cfg.epoch.as_str() {
            "auto" => EpochMode::Auto,
            "shift" => EpochMode::Shift,
            "direct" => EpochMode::Direct,
            o => return Err(format!("[space] epoch {o:?}: want auto, shift or direct")),
        };
        let from = to_dt(start_unix);
        let tm = space.time_map(mode, from, from + Duration::hours(24));
        Ok(RealSpace { space, tm })
    }

    /// From an already loaded space and a time map (tests, other callers).
    pub fn new(space: Space, tm: TimeMap) -> RealSpace {
        RealSpace { space, tm }
    }

    pub fn time_map(&self) -> TimeMap {
        self.tm
    }

    pub fn space(&self) -> &Space {
        &self.space
    }
}

impl SpaceSource for RealSpace {
    fn check(&self, class: &str, unix_s: f64) -> Result<(), String> {
        let cs = self.space.class(class).map_err(|e| e.to_string())?;
        if cs.class.comms.is_none() {
            return Err(format!("space class {class:?} has no comms model"));
        }
        let t = to_dt(unix_s);
        let (usable, skipped) = self.space.usable(cs, &self.tm, t, t);
        if usable.is_empty() {
            return Err(format!(
                "space class {class:?} has no usable satellites ({} loaded, {} stale, {} not yet launched)",
                cs.sats.len(),
                skipped.stale,
                skipped.not_yet_launched
            ));
        }
        Ok(())
    }

    fn sample(&self, class: &str, pos: &GeoPoint, unix_s: f64) -> Result<SatObs, String> {
        let s = self
            .space
            .link_at(class, geodetic(pos), to_dt(unix_s), &self.tm)
            .map_err(|e| e.to_string())?;
        Ok(SatObs {
            visible: s.visible,
            latency_ms: s.best.as_ref().and_then(|b| b.latency_ms),
            best: s.best.map(|b| BestSat {
                norad_id: b.norad_id,
                name: b.satellite,
                elevation_deg: b.elevation_deg,
                range_km: b.range_km,
            }),
        })
    }

    fn outlook(
        &self,
        class: &str,
        pos: &GeoPoint,
        unix_s: f64,
        horizon_s: f64,
    ) -> Result<Outlook, String> {
        let from = to_dt(unix_s);
        let to = to_dt(unix_s + horizon_s);
        let q = LinkQuery {
            point: geodetic(pos),
            from,
            to,
            classes: vec![class.to_string()],
            epoch_mode: self.tm.mode,
            sample_step_s: horizon_s.max(60.0),
            include_samples: false,
        };
        let report = self
            .space
            .links_with_time_map(&q, self.tm)
            .map_err(|e| e.to_string())?;
        let c = report.classes.first().ok_or("no class in link report")?;
        let mut out = Outlook::default();
        if let Some(o) = c.outages.first() {
            let (s, e) = (to_unix(o.start), to_unix(o.end));
            if s <= unix_s + 1.0 {
                // Down now: back when this gap closes, unless it outlasts the horizon.
                if e < unix_s + horizon_s - 1.0 {
                    out.next_up_unix = Some(e);
                }
            } else {
                out.next_outage_unix = Some(s);
            }
        }
        Ok(out)
    }
}

/// How a link currently gets its numbers.
#[derive(Clone, Debug, PartialEq)]
pub enum SatMode {
    /// No answer yet (before the first position or the first round): static parameters.
    Pending,
    /// Following the constellation.
    Live,
    /// Static parameters, and why (cache missing, class unusable, source error).
    Static(String),
}

impl SatMode {
    fn name(&self) -> &'static str {
        match self {
            SatMode::Pending => "pending",
            SatMode::Live => "live",
            SatMode::Static(_) => "static",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SatSnapshot {
    pub mode: SatMode,
    /// Link up according to the constellation (meaningful when `Live`).
    pub up: bool,
    pub visible: usize,
    pub best: Option<BestSat>,
    pub latency_ms: Option<f64>,
    /// Scenario time of the last sample.
    pub sampled_unix: Option<f64>,
    pub handovers: u64,
    pub last_handover_unix: Option<f64>,
    /// Emulator time (ns) until which a handover outage lasts.
    pub handover_until_ns: u64,
    pub outlook: Outlook,
    pub outlook_unix: Option<f64>,
    /// Source errors since start, and the last one.
    pub errors: u64,
    pub last_error: Option<String>,
    /// Satellite samples taken.
    pub samples: u64,
}

impl SatSnapshot {
    fn new(mode: SatMode) -> SatSnapshot {
        SatSnapshot {
            mode,
            up: true,
            visible: 0,
            best: None,
            latency_ms: None,
            sampled_unix: None,
            handovers: 0,
            last_handover_unix: None,
            handover_until_ns: 0,
            outlook: Outlook::default(),
            outlook_unix: None,
            errors: 0,
            last_error: None,
            samples: 0,
        }
    }
}

/// One vehicle's SATCOM state, shared between its link task (reads, and publishes the
/// position) and the satellite task (reads the position, publishes the answer).
pub struct SatShared {
    pub class: String,
    pub handover_outage_ns: u64,
    pos: Mutex<Option<GeoPoint>>,
    state: RwLock<SatSnapshot>,
}

impl SatShared {
    pub fn new(class: &str, handover_outage_ns: u64) -> SatShared {
        SatShared {
            class: class.to_string(),
            handover_outage_ns,
            pos: Mutex::new(None),
            state: RwLock::new(SatSnapshot::new(SatMode::Pending)),
        }
    }

    pub fn set_position(&self, p: GeoPoint) {
        *self.pos.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
    }

    pub fn position(&self) -> Option<GeoPoint> {
        *self.pos.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn snapshot(&self) -> SatSnapshot {
        self.state.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn with_state<R>(&self, f: impl FnOnce(&mut SatSnapshot) -> R) -> R {
        f(&mut self.state.write().unwrap_or_else(|e| e.into_inner()))
    }

    /// Forces the static fallback with a reason (the source or class cannot be used).
    pub fn set_static(&self, reason: &str) {
        self.with_state(|s| s.mode = SatMode::Static(reason.to_string()));
    }

    /// What this link's state does to a packet at `now_ns`: sets the blocking cause and the
    /// base latency. Static and pending links change nothing.
    pub fn apply(
        &self,
        now_ns: u64,
        blocked: &mut Option<Cause>,
        base_latency_ns: &mut Option<u64>,
    ) {
        let s = self.state.read().unwrap_or_else(|e| e.into_inner());
        if s.mode != SatMode::Live {
            return;
        }
        if !s.up {
            blocked.get_or_insert(Cause::NoSat);
            return;
        }
        if now_ns < s.handover_until_ns {
            blocked.get_or_insert(Cause::Handover);
        }
        if let Some(ms) = s.latency_ms {
            *base_latency_ns = Some((ms.max(0.0) * 1e6) as u64);
        }
    }

    /// For `/stats/<uid>`.
    pub fn json(&self, static_latency_ms: f64) -> Value {
        let s = self.snapshot();
        let rel = |t: Option<f64>| match (t, s.sampled_unix) {
            (Some(t), Some(now)) => json!((t - now).max(0.0)),
            _ => Value::Null,
        };
        let at = |t: Option<f64>| t.map_or(Value::Null, |t| json!(iso(t)));
        let live = s.mode == SatMode::Live;
        let reason = match &s.mode {
            SatMode::Static(r) => json!(r),
            _ => Value::Null,
        };
        let latency = if live && s.up {
            s.latency_ms
                .map_or((json!(static_latency_ms), "profile"), |ms| {
                    (json!(ms), "space")
                })
        } else {
            (json!(static_latency_ms), "profile")
        };
        json!({
            "class": self.class,
            "mode": s.mode.name(),
            "static_reason": reason,
            "up": if live { json!(s.up) } else { Value::Null },
            "visible": if live { json!(s.visible) } else { Value::Null },
            "best_norad_id": s.best.as_ref().map(|b| b.norad_id),
            "best_name": s.best.as_ref().map(|b| b.name.clone()),
            "best_elevation_deg": s.best.as_ref().map(|b| b.elevation_deg),
            "slant_range_km": s.best.as_ref().map(|b| b.range_km),
            "latency_ms": latency.0,
            "latency_source": latency.1,
            "sampled_scenario": at(s.sampled_unix),
            "samples": s.samples,
            "handovers": s.handovers,
            "handover_outage_ms": self.handover_outage_ns as f64 / 1e6,
            "last_handover_scenario": at(s.last_handover_unix),
            "next_outage_scenario": at(s.outlook.next_outage_unix),
            "next_outage_in_s": rel(s.outlook.next_outage_unix),
            "next_pass_scenario": at(s.outlook.next_up_unix),
            "next_pass_in_s": rel(s.outlook.next_up_unix),
            "errors": s.errors,
            "last_error": s.last_error,
        })
    }
}

/// One refresh of one vehicle: sample the constellation at its position, detect handovers,
/// refresh the look-ahead when due. Pure of the clock (times are passed in), so tests drive it
/// with a fake source.
pub fn refresh(
    source: &dyn SpaceSource,
    cfg: &SpaceCfg,
    sat: &SatShared,
    scenario_unix: f64,
    now_ns: u64,
) {
    let Some(pos) = sat.position() else { return };
    let obs = match source.sample(&sat.class, &pos, scenario_unix) {
        Ok(o) => o,
        Err(e) => {
            sat.with_state(|s| {
                s.errors += 1;
                s.last_error = Some(e.clone());
                s.mode = SatMode::Static(format!("space source error: {e}"));
            });
            return;
        }
    };
    let up = obs.best.is_some();
    let (was_live, was_up, prev_best, outlook_unix) = {
        let s = sat.snapshot();
        (
            s.mode == SatMode::Live,
            s.up,
            s.best.as_ref().map(|b| b.norad_id),
            s.outlook_unix,
        )
    };
    let flipped = was_live && was_up != up;
    let due = cfg.outlook_s > 0.0
        && (flipped
            || outlook_unix.is_none_or(|t| {
                scenario_unix < t || scenario_unix - t >= cfg.outlook_every_s.max(1.0)
            }));
    let outlook = if due {
        match source.outlook(&sat.class, &pos, scenario_unix, cfg.outlook_s) {
            Ok(o) => Some(o),
            Err(e) => {
                sat.with_state(|s| {
                    s.errors += 1;
                    s.last_error = Some(e);
                });
                None
            }
        }
    } else {
        None
    };
    let handover = was_live
        && was_up
        && up
        && prev_best.is_some()
        && prev_best != obs.best.as_ref().map(|b| b.norad_id);
    sat.with_state(|s| {
        s.mode = SatMode::Live;
        s.up = up;
        s.visible = obs.visible;
        s.best = obs.best;
        s.latency_ms = obs.latency_ms;
        s.sampled_unix = Some(scenario_unix);
        s.samples += 1;
        if handover {
            s.handovers += 1;
            s.last_handover_unix = Some(scenario_unix);
            if sat.handover_outage_ns > 0 {
                s.handover_until_ns = now_ns + sat.handover_outage_ns;
            }
        }
        if let Some(o) = outlook {
            s.outlook = o;
            s.outlook_unix = Some(scenario_unix);
        }
    });
}

/// Service-level numbers for `/stats`.
#[derive(Default)]
pub struct SpaceInfo {
    /// Set when the whole source could not be used.
    pub source_error: Mutex<Option<String>>,
    /// class -> (vehicles, reason it is static, if so)
    pub classes: Mutex<BTreeMap<String, (usize, Option<String>)>>,
    pub time_map: Mutex<Option<(i64, String)>>,
    pub rounds: AtomicU64,
    pub last_round_ms: AtomicU64,
    pub max_round_ms: AtomicU64,
    pub interval_s: Mutex<f64>,
}

impl SpaceInfo {
    pub fn json(&self) -> Value {
        let lock = |e: std::sync::PoisonError<_>| e.into_inner();
        let classes: serde_json::Map<String, Value> = self
            .classes
            .lock()
            .unwrap_or_else(lock)
            .iter()
            .map(|(c, (n, r))| {
                (
                    c.clone(),
                    json!({
                        "vehicles": n,
                        "mode": if r.is_some() { "static" } else { "live" },
                        "static_reason": r,
                    }),
                )
            })
            .collect();
        let tm = self
            .time_map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        json!({
            "source_error": *self.source_error.lock().unwrap_or_else(|e| e.into_inner()),
            "classes": classes,
            "time_shift_days": tm.as_ref().map(|t| t.0),
            "elements_reference": tm.map(|t| t.1),
            "rounds": self.rounds.load(Relaxed),
            "last_round_ms": self.last_round_ms.load(Relaxed),
            "max_round_ms": self.max_round_ms.load(Relaxed),
        })
    }
}

/// Starts the background refresh of every SATCOM link. `source` is the loaded space data, or
/// why there is none (then every SATCOM link keeps its static parameters, and says so).
/// Returns the info handle for `/stats`; the task runs until the runtime ends.
pub fn spawn(
    source: Result<Arc<dyn SpaceSource>, String>,
    cfg: SpaceCfg,
    timeline: Timeline,
    t0: Instant,
    sats: Vec<Arc<SatShared>>,
    time_map: Option<TimeMap>,
) -> Arc<SpaceInfo> {
    let info = Arc::new(SpaceInfo::default());
    *info.interval_s.lock().unwrap_or_else(|e| e.into_inner()) = cfg.interval_s;
    if let Some(tm) = time_map {
        *info.time_map.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((tm.shift_days, tm.reference.to_rfc3339()));
    }
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for s in &sats {
        *counts.entry(s.class.clone()).or_default() += 1;
    }
    let start_unix = timeline.scenario_unix(0.0);
    let mut live: Vec<Arc<SatShared>> = Vec::new();
    let mut table = BTreeMap::new();
    for (class, n) in counts {
        let verdict = match &source {
            Ok(src) => src.check(&class, start_unix),
            Err(e) => Err(e.clone()),
        };
        match &verdict {
            Ok(()) => eprintln!(
                "linkemu: SATCOM class {class:?} for {n} vehicle(s) follows satellite visibility (murmur-space)"
            ),
            Err(e) => eprintln!(
                "linkemu: SATCOM class {class:?} for {n} vehicle(s) uses the STATIC profile parameters: {e}"
            ),
        }
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
    let (Ok(src), false) = (source, live.is_empty()) else {
        return info;
    };
    let info2 = info.clone();
    tokio::spawn(async move {
        let wall = (cfg.interval_s / timeline.rate)
            .max(cfg.min_wall_s)
            .max(0.05);
        loop {
            let started = std::time::Instant::now();
            let unix = timeline.scenario_unix(t0.elapsed().as_secs_f64());
            let now_ns = t0.elapsed().as_nanos() as u64;
            let (src, cfg2, batch) = (src.clone(), cfg.clone(), live.clone());
            // SGP4 over thousands of satellites: off the async workers.
            let _ = tokio::task::spawn_blocking(move || {
                for s in &batch {
                    refresh(src.as_ref(), &cfg2, s, unix, now_ns);
                }
            })
            .await;
            let ms = started.elapsed().as_millis() as u64;
            info2.rounds.fetch_add(1, Relaxed);
            info2.last_round_ms.store(ms, Relaxed);
            info2.max_round_ms.fetch_max(ms, Relaxed);
            let spent = started.elapsed().as_secs_f64();
            tokio::time::sleep(StdDuration::from_secs_f64((wall - spent).max(0.01))).await;
        }
    });
    info
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as M;

    /// A source whose answer is a function of scenario time.
    struct Fake {
        f: Box<dyn Fn(f64) -> Result<SatObs, String> + Send + Sync>,
        outlook_calls: M<u32>,
    }

    impl Fake {
        fn new(f: impl Fn(f64) -> Result<SatObs, String> + Send + Sync + 'static) -> Fake {
            Fake {
                f: Box::new(f),
                outlook_calls: M::new(0),
            }
        }
    }

    impl SpaceSource for Fake {
        fn check(&self, class: &str, _: f64) -> Result<(), String> {
            if class == "nope" {
                Err("no such class".into())
            } else {
                Ok(())
            }
        }
        fn sample(&self, _: &str, _: &GeoPoint, t: f64) -> Result<SatObs, String> {
            (self.f)(t)
        }
        fn outlook(&self, _: &str, _: &GeoPoint, t: f64, _: f64) -> Result<Outlook, String> {
            *self.outlook_calls.lock().expect("lock") += 1;
            Ok(Outlook {
                next_outage_unix: Some(t + 100.0),
                next_up_unix: None,
            })
        }
    }

    fn obs(norad: u64, range_km: f64, ms: Option<f64>) -> SatObs {
        SatObs {
            visible: 2,
            best: Some(BestSat {
                norad_id: norad,
                name: format!("SAT-{norad}"),
                elevation_deg: 40.0,
                range_km,
            }),
            latency_ms: ms,
        }
    }

    fn none() -> SatObs {
        SatObs {
            visible: 0,
            best: None,
            latency_ms: None,
        }
    }

    fn cfg() -> SpaceCfg {
        SpaceCfg::default()
    }

    fn pos() -> GeoPoint {
        GeoPoint {
            lat: 37.8,
            lon: -122.5,
            alt_m: 0.0,
        }
    }

    fn shared(outage_ms: u64) -> SatShared {
        let s = SatShared::new("starlink", outage_ms * 1_000_000);
        s.set_position(pos());
        s
    }

    fn effect(s: &SatShared, now_ns: u64) -> (Option<Cause>, Option<u64>) {
        let (mut b, mut l) = (None, None);
        s.apply(now_ns, &mut b, &mut l);
        (b, l)
    }

    #[test]
    fn pending_and_static_change_nothing() {
        let s = shared(0);
        assert_eq!(effect(&s, 0), (None, None));
        s.set_static("no cache");
        assert_eq!(effect(&s, 0), (None, None));
        assert_eq!(s.snapshot().mode, SatMode::Static("no cache".into()));
    }

    #[test]
    fn no_position_means_no_sample() {
        let s = SatShared::new("starlink", 0);
        let f = Fake::new(|_| Ok(none()));
        refresh(&f, &cfg(), &s, 0.0, 0);
        assert_eq!(s.snapshot().mode, SatMode::Pending);
    }

    #[test]
    fn up_down_and_latency_follow_the_source() {
        let s = shared(0);
        // Up with a satellite until t=100, then nothing, then back.
        let f = Fake::new(|t| {
            if (100.0..200.0).contains(&t) {
                Ok(none())
            } else {
                Ok(obs(1, 800.0, Some(12.5)))
            }
        });
        refresh(&f, &cfg(), &s, 10.0, 0);
        assert_eq!(effect(&s, 0), (None, Some(12_500_000)));
        refresh(&f, &cfg(), &s, 120.0, 0);
        assert_eq!(effect(&s, 0), (Some(Cause::NoSat), None));
        let snap = s.snapshot();
        assert!(!snap.up && snap.best.is_none());
        refresh(&f, &cfg(), &s, 210.0, 0);
        assert_eq!(effect(&s, 0), (None, Some(12_500_000)));
        // Coming back after an outage is not a handover.
        assert_eq!(s.snapshot().handovers, 0);
    }

    #[test]
    fn unknown_latency_keeps_the_profile_value() {
        let s = shared(0);
        let f = Fake::new(|_| Ok(obs(7, 1500.0, None)));
        refresh(&f, &cfg(), &s, 0.0, 0);
        assert_eq!(effect(&s, 0), (None, None));
        let j = s.json(700.0);
        assert_eq!(j["latency_source"], "profile");
        assert_eq!(j["latency_ms"], 700.0);
        assert_eq!(j["best_norad_id"], 7);
    }

    #[test]
    fn handover_counts_and_drops_for_the_configured_time() {
        let s = shared(100);
        let f = Fake::new(|t| Ok(obs(if t < 15.0 { 1 } else { 2 }, 800.0, Some(10.0))));
        refresh(&f, &cfg(), &s, 10.0, 1_000_000_000);
        assert_eq!(s.snapshot().handovers, 0);
        refresh(&f, &cfg(), &s, 20.0, 2_000_000_000);
        let snap = s.snapshot();
        assert_eq!(snap.handovers, 1);
        assert_eq!(snap.last_handover_unix, Some(20.0));
        // Inside the 100 ms outage the link is blocked; after it, open again.
        assert_eq!(effect(&s, 2_050_000_000).0, Some(Cause::Handover));
        assert_eq!(effect(&s, 2_100_000_000).0, None);
        // Same satellite again: no new handover.
        refresh(&f, &cfg(), &s, 25.0, 3_000_000_000);
        assert_eq!(s.snapshot().handovers, 1);
    }

    #[test]
    fn handover_without_outage_only_counts() {
        let s = shared(0);
        let f = Fake::new(|t| Ok(obs(if t < 15.0 { 1 } else { 2 }, 800.0, Some(10.0))));
        refresh(&f, &cfg(), &s, 10.0, 0);
        refresh(&f, &cfg(), &s, 20.0, 1);
        assert_eq!(s.snapshot().handovers, 1);
        assert_eq!(effect(&s, 2).0, None);
    }

    #[test]
    fn source_errors_fall_back_to_static_and_recover() {
        let s = shared(0);
        let f = Fake::new(|t| {
            if t < 50.0 {
                Err("boom".into())
            } else {
                Ok(obs(1, 800.0, Some(9.0)))
            }
        });
        refresh(&f, &cfg(), &s, 10.0, 0);
        assert_eq!(effect(&s, 0), (None, None));
        let snap = s.snapshot();
        assert_eq!(snap.errors, 1);
        assert!(matches!(snap.mode, SatMode::Static(_)));
        assert_eq!(s.json(25.0)["mode"], "static");
        refresh(&f, &cfg(), &s, 60.0, 0);
        assert_eq!(s.snapshot().mode, SatMode::Live);
    }

    #[test]
    fn outlook_refreshes_on_cadence_and_when_the_state_flips() {
        let s = shared(0);
        let f = Fake::new(|t| {
            if (100.0..110.0).contains(&t) {
                Ok(none())
            } else {
                Ok(obs(1, 800.0, Some(10.0)))
            }
        });
        let c = SpaceCfg {
            outlook_every_s: 60.0,
            ..cfg()
        };
        refresh(&f, &c, &s, 0.0, 0);
        refresh(&f, &c, &s, 5.0, 0);
        refresh(&f, &c, &s, 30.0, 0);
        assert_eq!(*f.outlook_calls.lock().expect("lock"), 1);
        refresh(&f, &c, &s, 61.0, 0);
        assert_eq!(*f.outlook_calls.lock().expect("lock"), 2);
        // Going down forces a fresh look-ahead.
        refresh(&f, &c, &s, 100.0, 0);
        assert_eq!(*f.outlook_calls.lock().expect("lock"), 3);
        let j = s.json(300.0);
        assert_eq!(j["next_outage_in_s"], 100.0);
        let off = SpaceCfg {
            outlook_s: 0.0,
            ..cfg()
        };
        let s2 = shared(0);
        refresh(&f, &off, &s2, 0.0, 0);
        assert_eq!(*f.outlook_calls.lock().expect("lock"), 3);
    }

    #[test]
    fn iso_formats_scenario_time() {
        assert_eq!(iso(1_781_431_200.0), "2026-06-14T10:00:00Z");
    }

    #[tokio::test]
    async fn spawn_without_a_source_marks_links_static() {
        let a = Arc::new(SatShared::new("starlink", 0));
        let info = spawn(
            Err("cache missing".into()),
            cfg(),
            Timeline::default(),
            Instant::now(),
            vec![a.clone()],
            None,
        );
        assert_eq!(a.snapshot().mode, SatMode::Static("cache missing".into()));
        let j = info.json();
        assert_eq!(j["source_error"], "cache missing");
        assert_eq!(j["classes"]["starlink"]["mode"], "static");
    }

    #[tokio::test]
    async fn spawn_with_a_source_goes_live_and_unusable_classes_stay_static() {
        let a = Arc::new(SatShared::new("starlink", 0));
        let b = Arc::new(SatShared::new("nope", 0));
        a.set_position(pos());
        let src: Arc<dyn SpaceSource> = Arc::new(Fake::new(|_| Ok(obs(5, 700.0, Some(11.0)))));
        let cfg = SpaceCfg {
            min_wall_s: 0.05,
            ..cfg()
        };
        let _info = spawn(
            Ok(src),
            cfg,
            Timeline::default(),
            Instant::now(),
            vec![a.clone(), b.clone()],
            None,
        );
        assert!(matches!(b.snapshot().mode, SatMode::Static(_)));
        let until = std::time::Instant::now() + StdDuration::from_secs(5);
        while a.snapshot().mode != SatMode::Live && std::time::Instant::now() < until {
            tokio::time::sleep(StdDuration::from_millis(20)).await;
        }
        assert_eq!(a.snapshot().best.map(|b| b.norad_id), Some(5));
    }
}
