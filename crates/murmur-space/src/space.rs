//! The loaded space segment and the queries the rest of Murmur calls.

use crate::access::{self, Constraint, Interval, SearchParams};
use crate::area::Area;
use crate::cache::{Cache, CacheMeta};
use crate::catalog::{Capability, Catalog, EffectiveSensor, SatClass};
use crate::geo::{
    C_KM_S, EARTH_MEAN_RADIUS_KM, Geodetic, Observer, ViewGeometry, sun_elevation_deg, view_geometry,
};
use crate::orbit::{Satellite, Sgp4Mode};
use crate::timemap::{EpochMode, TimeMap};
use chrono::{DateTime, Duration, Utc};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum SpaceError {
    #[error(transparent)]
    Catalog(#[from] crate::catalog::CatalogError),
    #[error(transparent)]
    Cache(#[from] crate::cache::CacheError),
    #[error("element sets for {0}: {1}")]
    Parse(String, String),
    #[error("unknown class {0:?}")]
    UnknownClass(String),
    #[error("{0}")]
    Invalid(String),
}

/// One catalog class with its satellites.
pub struct ClassSats {
    pub class: SatClass,
    pub sats: Vec<Satellite>,
    /// Element sets SGP4 could not initialise.
    pub init_errors: usize,
}

pub struct Space {
    pub catalog: Catalog,
    pub classes: Vec<ClassSats>,
    /// Median element epoch over all satellites: what "now" means for the elements.
    pub reference: DateTime<Utc>,
    pub cache_meta: Vec<CacheMeta>,
    /// When set, every non-direct query uses this shift instead of choosing one per window.
    /// A long-running caller (the bridge's console session) pins one so that passes,
    /// positions and links all describe the same propagation day. See `pin_shift`.
    pub pinned_shift_days: Option<i64>,
}

impl Space {
    /// Load the catalog and every class's cached element sets.
    pub fn load(catalog_path: &Path, cache_dir: &Path) -> Result<Space, SpaceError> {
        let catalog = Catalog::load(catalog_path)?;
        let cache = Cache::new(cache_dir);
        let mut sources = Vec::new();
        let mut metas = Vec::new();
        for q in catalog.queries() {
            let (text, meta) = cache.read(&q)?;
            sources.push((q, text));
            metas.push(meta);
        }
        let mut space = Space::from_sources(catalog, &sources)?;
        space.cache_meta = metas;
        Ok(space)
    }

    /// Build from in-memory GP JSON per CelesTrak query (tests, other callers).
    pub fn from_sources(
        catalog: Catalog,
        sources: &[(String, String)],
    ) -> Result<Space, SpaceError> {
        let mut parsed: Vec<(String, Vec<Satellite>, usize)> = Vec::new();
        for (q, text) in sources {
            let (sats, errs) = Satellite::parse_omm_json(text, Sgp4Mode::Improved)
                .map_err(|e| SpaceError::Parse(q.clone(), e.to_string()))?;
            parsed.push((q.clone(), sats, errs.len()));
        }
        let mut classes = Vec::new();
        for class in &catalog.classes {
            let mut sats: Vec<Satellite> = Vec::new();
            let mut init_errors = 0;
            for q in &class.celestrak {
                if let Some((_, list, errs)) = parsed.iter().find(|(pq, _, _)| pq == q) {
                    init_errors += errs;
                    for s in list {
                        if catalog.accepts(class, &s.name)
                            && !sats.iter().any(|x| x.norad_id == s.norad_id)
                        {
                            sats.push(s.clone());
                        }
                    }
                }
            }
            classes.push(ClassSats {
                class: class.clone(),
                sats,
                init_errors,
            });
        }
        let mut epochs: Vec<i64> = classes
            .iter()
            .flat_map(|c| c.sats.iter().map(|s| s.epoch.timestamp()))
            .collect();
        if epochs.is_empty() {
            return Err(SpaceError::Invalid("no satellites loaded".into()));
        }
        epochs.sort_unstable();
        let reference =
            DateTime::from_timestamp(epochs[epochs.len() / 2], 0).unwrap_or_else(Utc::now);
        Ok(Space {
            catalog,
            classes,
            reference,
            cache_meta: Vec::new(),
            pinned_shift_days: None,
        })
    }

    pub fn class(&self, id: &str) -> Result<&ClassSats, SpaceError> {
        self.classes
            .iter()
            .find(|c| c.class.id == id)
            .ok_or_else(|| SpaceError::UnknownClass(id.to_string()))
    }

    pub fn time_map(&self, mode: EpochMode, from: DateTime<Utc>, to: DateTime<Utc>) -> TimeMap {
        match (mode, self.pinned_shift_days) {
            (EpochMode::Direct, _) | (_, None) => {
                TimeMap::for_window(mode, self.reference, from, to)
            }
            (_, Some(shift_days)) => TimeMap {
                mode,
                shift_days,
                reference: self.reference,
            },
        }
    }

    /// Pin the time shift to the one `Auto` picks for [from, to] (or clear it with `None`).
    /// Without a pin, two windows whose middles fall either side of a half-day boundary get
    /// shifts one day apart, so a timeline and a globe would show different days.
    pub fn pin_shift(&mut self, window: Option<(DateTime<Utc>, DateTime<Utc>)>) -> Option<i64> {
        self.pinned_shift_days = window.map(|(from, to)| {
            TimeMap::for_window(EpochMode::Auto, self.reference, from, to).shift_days
        });
        self.pinned_shift_days
    }

    /// Satellites of a class usable over the scenario interval [from, to]: elements recent
    /// enough at the propagation times, and already launched at the scenario start (current
    /// catalogs include satellites that did not exist on a past scenario date).
    pub fn usable<'a>(
        &self,
        cs: &'a ClassSats,
        tm: &TimeMap,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> (Vec<&'a Satellite>, Skipped) {
        let max = self.catalog.defaults.max_element_age_days;
        let (pfrom, pto) = (tm.to_prop(from), tm.to_prop(to));
        let mut skipped = Skipped::default();
        let ok = cs
            .sats
            .iter()
            .filter(|s| {
                if s.element_age_days(&pfrom).abs() > max || s.element_age_days(&pto).abs() > max {
                    skipped.stale += 1;
                    false
                } else if s.estimated_launch().is_some_and(|l| l > from) {
                    skipped.not_yet_launched += 1;
                    false
                } else {
                    true
                }
            })
            .collect();
        (ok, skipped)
    }

    fn classes_for(
        &self,
        ids: &[String],
        want: impl Fn(Capability) -> bool,
    ) -> Result<Vec<&ClassSats>, SpaceError> {
        if ids.is_empty() {
            return Ok(self
                .classes
                .iter()
                .filter(|c| want(c.class.capability))
                .collect());
        }
        ids.iter().map(|id| self.class(id)).collect()
    }

    // ------------------------------------------------------------ imaging access

    /// Imaging access windows over an area: when each satellite's sensor can point at the
    /// area's centre within its geometric limits. Optical windows carry the sun elevation
    /// at the target and whether it meets the class minimum.
    pub fn imaging_access(&self, q: &AccessQuery) -> Result<AccessReport, SpaceError> {
        let started = std::time::Instant::now();
        let tm = self.time_map(q.epoch_mode, q.from, q.to);
        let target = Observer::new(q.area.centroid());
        let (pfrom, pto) = (tm.to_prop(q.from), tm.to_prop(q.to));
        let mut windows = Vec::new();
        let mut summaries = Vec::new();
        for cs in self.classes_for(&q.classes, Capability::is_imaging)? {
            let (sats, stale) = self.usable(cs, &tm, q.from, q.to);
            let class_windows: Vec<AccessWindow> = sats
                .par_iter()
                .flat_map_iter(|sat| imaging_windows_for(cs, sat, &target, &tm, pfrom, pto))
                .collect();
            let total = class_windows.len();
            let usable: Vec<AccessWindow> = class_windows
                .into_iter()
                .filter(|w| q.include_night || w.daylight_ok != Some(false))
                .collect();
            summaries.push(ClassSummary {
                class_id: cs.class.id.clone(),
                name: cs.class.name.clone(),
                capability: cs.class.capability,
                satellites: cs.sats.len(),
                stale_skipped: stale.stale,
                not_yet_launched: stale.not_yet_launched,
                windows: usable.len(),
                night_excluded: total - usable.len(),
            });
            windows.extend(usable);
        }
        windows.sort_by_key(|w| w.start);
        Ok(AccessReport {
            area_center: q.area.centroid(),
            area_radius_km: q.area.radius_km(),
            from: q.from,
            to: q.to,
            time_map: tm,
            classes: summaries,
            windows,
            compute_ms: started.elapsed().as_millis() as u64,
        })
    }

    // ------------------------------------------------------------ comms links

    /// Link windows from a terminal position for each SATCOM (or reference) class, with
    /// coverage, outages and latency sampled every `sample_step_s`.
    pub fn links(&self, q: &LinkQuery) -> Result<LinkReport, SpaceError> {
        let tm = self.time_map(q.epoch_mode, q.from, q.to);
        self.links_with_time_map(q, tm)
    }

    /// As [`Space::links`] with the scenario-to-propagation time map given instead of derived
    /// from the query window (`q.epoch_mode` is ignored). A caller that also uses
    /// [`Space::link_at`] over a long run passes one fixed map to both, so a short look-ahead
    /// window cannot round to a different day shift than the instantaneous samples.
    pub fn links_with_time_map(
        &self,
        q: &LinkQuery,
        tm: TimeMap,
    ) -> Result<LinkReport, SpaceError> {
        let started = std::time::Instant::now();
        let obs = Observer::new(q.point);
        let (pfrom, pto) = (tm.to_prop(q.from), tm.to_prop(q.to));
        let mut classes = Vec::new();
        for cs in self.classes_for(&q.classes, |c| c == Capability::Satcom)? {
            let comms = cs.class.comms.clone().ok_or_else(|| {
                SpaceError::Invalid(format!("{} has no comms model", cs.class.id))
            })?;
            let (sats, stale) = self.usable(cs, &tm, q.from, q.to);
            let mut wins: Vec<LinkWindow> = sats
                .par_iter()
                .flat_map_iter(|sat| {
                    let alt = sat.semi_major_axis_km() - EARTH_MEAN_RADIUS_KM;
                    let mask = comms.min_elevation_for_alt(alt);
                    let c = Constraint::MinElevation(mask);
                    let raw = access::find_windows(
                        sat,
                        &obs,
                        pfrom,
                        pto,
                        c.psi_max(sat.max_radius_km()),
                        SearchParams::default(),
                        |_, g| c.holds(g),
                    );
                    raw.into_iter()
                        .filter_map(|w| {
                            let peak = access::peak_time(sat, &obs, w.start, w.end);
                            let g = access::geometry(sat, &obs, &peak)?;
                            Some(LinkWindow {
                                class_id: cs.class.id.clone(),
                                satellite: sat.name.clone(),
                                norad_id: sat.norad_id,
                                start: tm.to_scenario(w.start),
                                end: tm.to_scenario(w.end),
                                duration_s: (w.end - w.start).num_milliseconds() as f64 / 1000.0,
                                peak_time: tm.to_scenario(peak),
                                peak_elevation_deg: g.elevation_deg,
                                peak_azimuth_deg: g.azimuth_deg,
                                min_range_km: g.range_km,
                                latency_ms_at_peak: one_way_latency_ms(&comms, g.range_km),
                                mask_deg: mask,
                                clipped: w.clipped_start || w.clipped_end,
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .collect();
            wins.sort_by_key(|w| w.start);
            let covered = access::union(
                wins.iter()
                    .map(|w| Interval {
                        start: w.start,
                        end: w.end,
                    })
                    .collect(),
            );
            let outages = access::gaps(&covered, q.from, q.to);
            let total_s = (q.to - q.from).num_milliseconds() as f64 / 1000.0;
            let covered_s: f64 = covered.iter().map(Interval::duration_s).sum();
            let samples = link_samples(
                &comms,
                &sats,
                &obs,
                &tm,
                &wins,
                q.from,
                q.to,
                q.sample_step_s,
            );
            let visible: Vec<usize> = samples.iter().map(|s| s.visible).collect();
            let mut lat: Vec<f64> = samples
                .iter()
                .filter_map(|s| s.best.as_ref().and_then(|b| b.latency_ms))
                .collect();
            lat.sort_by(f64::total_cmp);
            classes.push(LinkClassReport {
                class_id: cs.class.id.clone(),
                name: cs.class.name.clone(),
                satellites: cs.sats.len(),
                stale_skipped: stale.stale,
                not_yet_launched: stale.not_yet_launched,
                mask_deg: comms.min_elevation_deg,
                path: comms.path.clone(),
                user_down_mbps: comms.user_down_mbps,
                user_up_mbps: comms.user_up_mbps,
                coverage_fraction: if total_s > 0.0 {
                    covered_s / total_s
                } else {
                    0.0
                },
                longest_outage_s: outages.iter().map(Interval::duration_s).fold(0.0, f64::max),
                outages,
                visible_min: visible.iter().copied().min().unwrap_or(0),
                visible_max: visible.iter().copied().max().unwrap_or(0),
                visible_mean: if visible.is_empty() {
                    0.0
                } else {
                    visible.iter().sum::<usize>() as f64 / visible.len() as f64
                },
                latency_ms_min: lat.first().copied(),
                latency_ms_median: lat.get(lat.len() / 2).copied(),
                latency_ms_max: lat.last().copied(),
                windows: wins,
                samples: if q.include_samples {
                    samples
                } else {
                    Vec::new()
                },
            });
        }
        Ok(LinkReport {
            point: q.point,
            from: q.from,
            to: q.to,
            time_map: tm,
            classes,
            compute_ms: started.elapsed().as_millis() as u64,
        })
    }
}

/// One point of an area link query ([`Space::links_area`]): what a coverage judgement
/// reads, no per-window detail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AreaLinkPoint {
    pub point: Geodetic,
    pub windows: usize,
    pub coverage_fraction: f64,
    pub outages: Vec<Interval>,
    pub longest_outage_s: f64,
    pub visible_min: usize,
    pub visible_max: usize,
    pub visible_mean: f64,
    pub latency_ms_min: Option<f64>,
    pub latency_ms_median: Option<f64>,
    pub latency_ms_max: Option<f64>,
}

/// An area link query's answer: one SATCOM class over several points.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AreaLinkReport {
    pub class_id: String,
    pub satellites: usize,
    pub stale_skipped: usize,
    pub not_yet_launched: usize,
    pub mask_deg: f64,
    pub time_map: TimeMap,
    pub points: Vec<AreaLinkPoint>,
    pub compute_ms: u64,
}

/// One point of an area GNSS query ([`Space::gnss_area`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AreaGnssPoint {
    pub point: Geodetic,
    pub samples: usize,
    pub visible_min: usize,
    pub visible_max: usize,
    /// First sample with the fewest satellites in view.
    pub visible_min_at: Option<DateTime<Utc>>,
    pub pdop_max: Option<f64>,
    pub pdop_mean: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AreaGnssReport {
    pub class_id: String,
    pub mask_deg: f64,
    pub satellites: usize,
    pub stale_skipped: usize,
    pub not_yet_launched: usize,
    pub time_map: TimeMap,
    pub points: Vec<AreaGnssPoint>,
    pub compute_ms: u64,
}

impl Space {
    /// SATCOM coverage of one class at several points of an area (the first the centroid)
    /// over [from, to] (scenario time, `tm` fixed): each satellite's windows found with one
    /// search for every point ([`access::find_windows_multi`]), and the visible counts and
    /// latencies sampled every `step_s` with each satellite propagated once per sample for
    /// every point. Costs about one point's [`Space::links`].
    pub fn links_area(
        &self,
        points: &[Geodetic],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        class: &str,
        step_s: f64,
        tm: TimeMap,
    ) -> Result<AreaLinkReport, SpaceError> {
        let started = std::time::Instant::now();
        if points.is_empty() {
            return Err(SpaceError::Invalid("an area query names no point".into()));
        }
        let cs = self.class(class)?;
        // any class with a comms model (a reference class too, as `links` takes it)
        let comms = cs.class.comms.clone().ok_or_else(|| {
            SpaceError::Invalid(format!("{} has no comms model", cs.class.id))
        })?;
        let observers: Vec<Observer> = points.iter().map(|p| Observer::new(*p)).collect();
        let (pfrom, pto) = (tm.to_prop(from), tm.to_prop(to));
        let (sats, stale) = self.usable(cs, &tm, from, to);
        // per satellite, per point: its windows (scenario time)
        let per_sat: Vec<(usize, Vec<Vec<Interval>>)> = sats
            .par_iter()
            .enumerate()
            .map(|(k, sat)| {
                let alt = sat.semi_major_axis_km() - EARTH_MEAN_RADIUS_KM;
                let mask = comms.min_elevation_for_alt(alt);
                let c = Constraint::MinElevation(mask);
                let raw = access::find_windows_multi(
                    sat,
                    &observers,
                    pfrom,
                    pto,
                    c.psi_max(sat.max_radius_km()),
                    SearchParams::default(),
                    |_, g| c.holds(g),
                );
                let wins: Vec<Vec<Interval>> = raw
                    .into_iter()
                    .map(|ws| {
                        ws.into_iter()
                            .map(|w| Interval {
                                start: tm.to_scenario(w.start),
                                end: tm.to_scenario(w.end),
                            })
                            .collect()
                    })
                    .collect();
                (k, wins)
            })
            .collect();
        let n = observers.len();
        // the samples: each satellite in any point's window propagated once, every point
        // judged on that state
        let step = Duration::milliseconds((step_s.max(1.0) * 1000.0) as i64);
        let mut times = Vec::new();
        let mut t = from;
        while t <= to {
            times.push(t);
            t += step;
        }
        let samples: Vec<Vec<(usize, Option<f64>)>> = times
            .par_iter()
            .map(|t| {
                let tp = tm.to_prop(*t);
                let mut per_point: Vec<(usize, Option<f64>)> = vec![(0, None); n];
                let mut best_el: Vec<f64> = vec![f64::NEG_INFINITY; n];
                for (k, wins) in &per_sat {
                    let inside: Vec<bool> = wins
                        .iter()
                        .map(|ws| ws.iter().any(|w| w.start <= *t && *t <= w.end))
                        .collect();
                    if !inside.iter().any(|&b| b) {
                        continue;
                    }
                    let Ok(st) = sats[*k].state(&tp) else {
                        continue;
                    };
                    let alt = st.ecef.norm() - EARTH_MEAN_RADIUS_KM;
                    for (i, obs) in observers.iter().enumerate() {
                        if !inside[i] {
                            continue;
                        }
                        let g = view_geometry(obs, &st.ecef, alt);
                        per_point[i].0 += 1;
                        if g.elevation_deg > best_el[i] {
                            best_el[i] = g.elevation_deg;
                            per_point[i].1 = one_way_latency_ms(&comms, g.range_km);
                        }
                    }
                }
                per_point
            })
            .collect();
        let total_s = (to - from).num_milliseconds() as f64 / 1000.0;
        let mut out = Vec::with_capacity(n);
        for (i, point) in points.iter().enumerate() {
            let all: Vec<Interval> = per_sat.iter().flat_map(|(_, w)| w[i].iter().copied()).collect();
            let windows = all.len();
            let covered = access::union(all);
            let outages = access::gaps(&covered, from, to);
            let covered_s: f64 = covered.iter().map(Interval::duration_s).sum();
            let visible: Vec<usize> = samples.iter().map(|s| s[i].0).collect();
            let mut lat: Vec<f64> = samples.iter().filter_map(|s| s[i].1).collect();
            lat.sort_by(f64::total_cmp);
            out.push(AreaLinkPoint {
                point: *point,
                windows,
                coverage_fraction: if total_s > 0.0 { covered_s / total_s } else { 0.0 },
                longest_outage_s: outages.iter().map(Interval::duration_s).fold(0.0, f64::max),
                outages,
                visible_min: visible.iter().copied().min().unwrap_or(0),
                visible_max: visible.iter().copied().max().unwrap_or(0),
                visible_mean: if visible.is_empty() {
                    0.0
                } else {
                    visible.iter().sum::<usize>() as f64 / visible.len() as f64
                },
                latency_ms_min: lat.first().copied(),
                latency_ms_median: lat.get(lat.len() / 2).copied(),
                latency_ms_max: lat.last().copied(),
            });
        }
        Ok(AreaLinkReport {
            class_id: cs.class.id.clone(),
            satellites: cs.sats.len(),
            stale_skipped: stale.stale,
            not_yet_launched: stale.not_yet_launched,
            mask_deg: comms.min_elevation_deg,
            time_map: tm,
            points: out,
            compute_ms: started.elapsed().as_millis() as u64,
        })
    }

    /// GNSS visibility and DOP at several points of an area over [from, to], sampled every
    /// `step_s` above the class's own mask, each satellite propagated once per sample for
    /// every point.
    pub fn gnss_area(
        &self,
        points: &[Geodetic],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        class: &str,
        step_s: f64,
        tm: TimeMap,
    ) -> Result<AreaGnssReport, SpaceError> {
        let started = std::time::Instant::now();
        if points.is_empty() {
            return Err(SpaceError::Invalid("an area query names no point".into()));
        }
        let cs = self.class(class)?;
        if cs.class.capability != Capability::Gnss {
            return Err(SpaceError::Invalid(format!("{class} is not a GNSS class")));
        }
        let mask = cs.class.comms.as_ref().map_or(5.0, |c| c.min_elevation_deg);
        let observers: Vec<Observer> = points.iter().map(|p| Observer::new(*p)).collect();
        let (sats, stale) = self.usable(cs, &tm, from, to);
        let step = Duration::milliseconds((step_s.max(1.0) * 1000.0) as i64);
        let mut times = Vec::new();
        let mut t = from;
        while t <= to {
            times.push(t);
            t += step;
        }
        let n = observers.len();
        // per sample, per point: in view and PDOP
        let samples: Vec<Vec<(usize, Option<f64>)>> = times
            .par_iter()
            .map(|t| {
                let tp = tm.to_prop(*t);
                let mut los: Vec<Vec<ViewGeometry>> = vec![Vec::new(); n];
                for sat in &sats {
                    let Ok(st) = sat.state(&tp) else {
                        continue;
                    };
                    let alt = st.ecef.norm() - EARTH_MEAN_RADIUS_KM;
                    for (i, obs) in observers.iter().enumerate() {
                        let g = view_geometry(obs, &st.ecef, alt);
                        if g.elevation_deg >= mask {
                            los[i].push(g);
                        }
                    }
                }
                los.iter()
                    .map(|v| (v.len(), crate::gnss::dop(v).map(|d| d.pdop)))
                    .collect()
            })
            .collect();
        let mut out = Vec::with_capacity(n);
        for (i, point) in points.iter().enumerate() {
            let visible: Vec<usize> = samples.iter().map(|s| s[i].0).collect();
            let pdops: Vec<f64> = samples.iter().filter_map(|s| s[i].1).collect();
            let visible_min = visible.iter().copied().min().unwrap_or(0);
            out.push(AreaGnssPoint {
                point: *point,
                samples: visible.len(),
                visible_min,
                visible_max: visible.iter().copied().max().unwrap_or(0),
                visible_min_at: visible
                    .iter()
                    .position(|v| *v == visible_min)
                    .and_then(|k| times.get(k).copied()),
                pdop_max: pdops
                    .iter()
                    .copied()
                    .fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x)))),
                pdop_mean: if pdops.is_empty() {
                    None
                } else {
                    Some(pdops.iter().sum::<f64>() / pdops.len() as f64)
                },
            });
        }
        Ok(AreaGnssReport {
            class_id: cs.class.id.clone(),
            mask_deg: mask,
            satellites: cs.sats.len(),
            stale_skipped: stale.stale,
            not_yet_launched: stale.not_yet_launched,
            time_map: tm,
            points: out,
            compute_ms: started.elapsed().as_millis() as u64,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn link_samples(
    comms: &crate::catalog::Comms,
    sats: &[&Satellite],
    obs: &Observer,
    tm: &TimeMap,
    wins: &[LinkWindow],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    step_s: f64,
) -> Vec<LinkSample> {
    let step = Duration::milliseconds((step_s.max(1.0) * 1000.0) as i64);
    let mut times = Vec::new();
    let mut t = from;
    while t <= to {
        times.push(t);
        t += step;
    }
    times
        .par_iter()
        .map(|t| {
            let mut visible = 0;
            let mut best: Option<LinkSat> = None;
            for w in wins.iter().filter(|w| w.start <= *t && *t <= w.end) {
                let Some(sat) = sats.iter().find(|s| s.norad_id == w.norad_id) else {
                    continue;
                };
                let Some(g) = access::geometry(sat, obs, &tm.to_prop(*t)) else {
                    continue;
                };
                visible += 1;
                if best
                    .as_ref()
                    .is_none_or(|b| g.elevation_deg > b.elevation_deg)
                {
                    best = Some(LinkSat {
                        norad_id: sat.norad_id,
                        satellite: sat.name.clone(),
                        elevation_deg: g.elevation_deg,
                        azimuth_deg: g.azimuth_deg,
                        range_km: g.range_km,
                        latency_ms: one_way_latency_ms(comms, g.range_km),
                    });
                }
            }
            LinkSample {
                t: *t,
                visible,
                best,
            }
        })
        .collect()
}

impl Space {
    /// Instantaneous link state from a terminal (for the link emulator): every satellite of
    /// the class above its mask at `t` (scenario time), best first.
    pub fn link_at(
        &self,
        class_id: &str,
        point: Geodetic,
        t: DateTime<Utc>,
        tm: &TimeMap,
    ) -> Result<LinkSample, SpaceError> {
        let cs = self.class(class_id)?;
        let comms = cs
            .class
            .comms
            .clone()
            .ok_or_else(|| SpaceError::Invalid(format!("{class_id} has no comms model")))?;
        let obs = Observer::new(point);
        let tp = tm.to_prop(t);
        let (sats, _) = self.usable(cs, tm, t, t);
        let vis: Vec<LinkSat> = sats
            .par_iter()
            .filter_map(|sat| {
                let g = access::geometry(sat, &obs, &tp)?;
                let mask =
                    comms.min_elevation_for_alt(sat.semi_major_axis_km() - EARTH_MEAN_RADIUS_KM);
                (g.elevation_deg >= mask).then(|| LinkSat {
                    norad_id: sat.norad_id,
                    satellite: sat.name.clone(),
                    elevation_deg: g.elevation_deg,
                    azimuth_deg: g.azimuth_deg,
                    range_km: g.range_km,
                    latency_ms: one_way_latency_ms(&comms, g.range_km),
                })
            })
            .collect();
        let best = vis
            .iter()
            .max_by(|a, b| a.elevation_deg.total_cmp(&b.elevation_deg))
            .cloned();
        Ok(LinkSample {
            t,
            visible: vis.len(),
            best,
        })
    }

    // ------------------------------------------------------------ GNSS

    pub fn gnss(&self, q: &GnssQuery) -> Result<GnssReport, SpaceError> {
        let started = std::time::Instant::now();
        let tm = self.time_map(q.epoch_mode, q.from, q.to);
        let cs = self.class(&q.class)?;
        let mask = q
            .mask_deg
            .or(cs.class.comms.as_ref().map(|c| c.min_elevation_deg))
            .unwrap_or(5.0);
        let obs = Observer::new(q.point);
        let (sats, stale) = self.usable(cs, &tm, q.from, q.to);
        let step = Duration::milliseconds((q.step_s.max(1.0) * 1000.0) as i64);
        let mut times = Vec::new();
        let mut t = q.from;
        while t <= q.to {
            times.push(t);
            t += step;
        }
        let samples: Vec<GnssSample> = times
            .par_iter()
            .map(|t| {
                let tp = tm.to_prop(*t);
                let los: Vec<ViewGeometry> = sats
                    .iter()
                    .filter_map(|s| access::geometry(s, &obs, &tp))
                    .filter(|g| g.elevation_deg >= mask)
                    .collect();
                let dop = crate::gnss::dop(&los);
                GnssSample {
                    t: *t,
                    visible: los.len(),
                    dop,
                }
            })
            .collect();
        let pdops: Vec<f64> = samples
            .iter()
            .filter_map(|s| s.dop.map(|d| d.pdop))
            .collect();
        Ok(GnssReport {
            point: q.point,
            class_id: q.class.clone(),
            mask_deg: mask,
            satellites: cs.sats.len(),
            stale_skipped: stale.stale,
            not_yet_launched: stale.not_yet_launched,
            time_map: tm,
            visible_min: samples.iter().map(|s| s.visible).min().unwrap_or(0),
            visible_max: samples.iter().map(|s| s.visible).max().unwrap_or(0),
            pdop_max: pdops
                .iter()
                .copied()
                .fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x)))),
            pdop_mean: if pdops.is_empty() {
                None
            } else {
                Some(pdops.iter().sum::<f64>() / pdops.len() as f64)
            },
            samples,
            compute_ms: started.elapsed().as_millis() as u64,
        })
    }

    // ------------------------------------------------------------ map and globe

    /// Positions (and optionally field-of-regard rings) of a class's satellites at `t`
    /// (scenario time), for the theater map and the globe.
    pub fn positions(
        &self,
        class_id: &str,
        t: DateTime<Utc>,
        tm: &TimeMap,
        with_footprint: bool,
    ) -> Result<Vec<SatPosition>, SpaceError> {
        let cs = self.class(class_id)?;
        let tp = tm.to_prop(t);
        let (sats, _) = self.usable(cs, tm, t, t);
        Ok(sats
            .par_iter()
            .filter_map(|sat| {
                let st = sat.state(&tp).ok()?;
                let geo = st.geodetic();
                let footprint = with_footprint.then(|| {
                    let r = st.ecef.norm();
                    let psi = footprint_angle(&cs.class, &sat.name, r);
                    ring(
                        &Geodetic::new(geo.lat_deg, geo.lon_deg, 0.0),
                        psi * EARTH_MEAN_RADIUS_KM,
                        72,
                    )
                });
                Some(SatPosition {
                    class_id: class_id.to_string(),
                    norad_id: sat.norad_id,
                    satellite: sat.name.clone(),
                    t,
                    position: geo,
                    footprint,
                })
            })
            .collect())
    }

    /// Sub-satellite ground track, [lon, lat] points every `step_s`.
    pub fn ground_track(
        &self,
        norad_id: u64,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        step_s: f64,
        tm: &TimeMap,
    ) -> Result<Vec<[f64; 2]>, SpaceError> {
        let sat = self
            .classes
            .iter()
            .flat_map(|c| c.sats.iter())
            .find(|s| s.norad_id == norad_id)
            .ok_or_else(|| SpaceError::Invalid(format!("no satellite {norad_id}")))?;
        let step = Duration::milliseconds((step_s.max(1.0) * 1000.0) as i64);
        let mut out = Vec::new();
        let mut t = from;
        while t <= to {
            if let Ok(st) = sat.state(&tm.to_prop(t)) {
                let g = st.geodetic();
                out.push([g.lon_deg, g.lat_deg]);
            }
            t += step;
        }
        Ok(out)
    }

    /// Find a satellite by NORAD id.
    pub fn satellite(&self, norad_id: u64) -> Option<(&SatClass, &Satellite)> {
        self.classes.iter().find_map(|c| {
            c.sats
                .iter()
                .find(|s| s.norad_id == norad_id)
                .map(|s| (&c.class, s))
        })
    }
}

fn imaging_windows_for(
    cs: &ClassSats,
    sat: &Satellite,
    target: &Observer,
    tm: &TimeMap,
    pfrom: DateTime<Utc>,
    pto: DateTime<Utc>,
) -> Vec<AccessWindow> {
    let Some(sensor) = cs.class.effective_sensor(&sat.name) else {
        return Vec::new();
    };
    let Some(c) = sensor_constraint(cs.class.capability, &sensor) else {
        return Vec::new();
    };
    let raw = access::find_windows(
        sat,
        target,
        pfrom,
        pto,
        c.psi_max(sat.max_radius_km()),
        SearchParams::default(),
        |_, g| c.holds(g),
    );
    raw.into_iter()
        .filter_map(|w| {
            let best_t = access::peak_time(sat, target, w.start, w.end);
            let g = access::geometry(sat, target, &best_t)?;
            let st = tm.to_scenario(best_t);
            let sun = sun_elevation_deg(target, &st);
            let daylight_ok = (cs.class.capability == Capability::Optical)
                .then(|| sun >= sensor.min_sun_elevation_deg.unwrap_or(0.0));
            Some(AccessWindow {
                class_id: cs.class.id.clone(),
                capability: cs.class.capability,
                satellite: sat.name.clone(),
                norad_id: sat.norad_id,
                label: sensor.label.clone(),
                start: tm.to_scenario(w.start),
                end: tm.to_scenario(w.end),
                duration_s: (w.end - w.start).num_milliseconds() as f64 / 1000.0,
                best_time: st,
                best: g,
                resolution_m: effective_resolution(cs.class.capability, &sensor, &g),
                sun_elevation_deg: sun,
                daylight_ok,
                taskable: sensor.taskable,
                element_age_days: sat.element_age_days(&best_t),
                clipped: w.clipped_start || w.clipped_end,
            })
        })
        .collect()
}

/// The class's geometric collection constraint.
pub fn sensor_constraint(cap: Capability, s: &EffectiveSensor) -> Option<Constraint> {
    match cap {
        Capability::Optical => Some(Constraint::MaxOffNadir(s.max_off_nadir_deg.unwrap_or(30.0))),
        Capability::Sar => Some(Constraint::Incidence {
            min: s.incidence_min_deg.unwrap_or(15.0),
            max: s.incidence_max_deg.unwrap_or(45.0),
        }),
        _ => None,
    }
}

/// Resolution at a given geometry. Optical: nadir GSD scaled by slant range over altitude
/// and by 1/sqrt(cos(incidence)) (geometric mean of along- and cross-track stretch; gives
/// WV-3's published 0.34 m at 20 deg off-nadir from 0.31 m at nadir). SAR: the class's
/// stated resolution (independent of range).
pub fn effective_resolution(cap: Capability, s: &EffectiveSensor, g: &ViewGeometry) -> Option<f64> {
    match cap {
        Capability::Optical => {
            let gsd = s.gsd_nadir_m?;
            let cos_inc = g.incidence_deg.to_radians().cos().max(0.05);
            Some(gsd * (g.range_km / g.sat_alt_km.max(1.0)) / cos_inc.sqrt())
        }
        Capability::Sar => s.resolution_m,
        _ => None,
    }
}

/// One-way latency for a terminal at `range_km`: the user leg plus, for a bent pipe, an
/// equal-length gateway leg, plus the class's network allowance.
pub fn one_way_latency_ms(c: &crate::catalog::Comms, range_km: f64) -> Option<f64> {
    let legs = match c.path.as_deref() {
        Some("bent-pipe") => 2.0,
        _ => 1.0,
    };
    let space = legs * range_km / C_KM_S * 1000.0;
    match (c.path.as_deref(), c.network_latency_ms) {
        (_, Some(n)) => Some(space + n),
        (Some("bent-pipe"), None) => Some(space),
        _ => None,
    }
}

fn footprint_angle(class: &SatClass, name: &str, r_km: f64) -> f64 {
    if let Some(s) = class.effective_sensor(name)
        && let Some(c) = sensor_constraint(class.capability, &s)
    {
        return match c {
            Constraint::MaxOffNadir(m) => crate::geo::central_angle_for_off_nadir(r_km, m),
            Constraint::Incidence { max, .. } => {
                crate::geo::central_angle_for_elevation(r_km, 90.0 - max)
            }
            Constraint::MinElevation(e) => crate::geo::central_angle_for_elevation(r_km, e),
        };
    }
    let mask = class.comms.as_ref().map_or(0.0, |c| {
        c.min_elevation_for_alt(r_km - EARTH_MEAN_RADIUS_KM)
    });
    crate::geo::central_angle_for_elevation(r_km, mask)
}

/// Circle on the ground as a [lon, lat] ring.
pub fn ring(center: &Geodetic, radius_km: f64, n: usize) -> Vec<[f64; 2]> {
    (0..n)
        .map(|i| {
            let p = center.destination(360.0 * i as f64 / n as f64, radius_km);
            [p.lon_deg, p.lat_deg]
        })
        .collect()
}

/// Approximate scene footprint: a square of side `swath_km` centred on the target, aligned
/// with the satellite's ground-track heading. [lon, lat] ring.
pub fn scene_polygon(center: &Geodetic, swath_km: f64, heading_deg: f64) -> Vec<[f64; 2]> {
    let half_diag = swath_km / std::f64::consts::SQRT_2;
    [45.0, 135.0, 225.0, 315.0]
        .iter()
        .map(|a| {
            let p = center.destination(heading_deg + a, half_diag);
            [p.lon_deg, p.lat_deg]
        })
        .collect()
}

// ------------------------------------------------------------ query and report types

/// Satellites left out of a computation, and why.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Skipped {
    /// Element set too far from the propagation time.
    pub stale: usize,
    /// Launched after the scenario start (estimated from designator and revolution count).
    pub not_yet_launched: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessQuery {
    pub area: Area,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// Class ids; empty = every imaging class.
    #[serde(default)]
    pub classes: Vec<String>,
    #[serde(default)]
    pub epoch_mode: EpochMode,
    /// Keep optical windows where the sun is below the class minimum.
    #[serde(default)]
    pub include_night: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessWindow {
    pub class_id: String,
    pub capability: Capability,
    pub satellite: String,
    pub norad_id: u64,
    pub label: Option<String>,
    /// Scenario time.
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub duration_s: f64,
    /// Best geometry (highest elevation) inside the window.
    pub best_time: DateTime<Utc>,
    pub best: ViewGeometry,
    pub resolution_m: Option<f64>,
    pub sun_elevation_deg: f64,
    /// Optical only: sun at or above the class minimum.
    pub daylight_ok: Option<bool>,
    pub taskable: bool,
    pub element_age_days: f64,
    /// Cut by the query window.
    pub clipped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassSummary {
    pub class_id: String,
    pub name: String,
    pub capability: Capability,
    pub satellites: usize,
    pub stale_skipped: usize,
    /// In the current catalog but launched after the scenario start.
    pub not_yet_launched: usize,
    pub windows: usize,
    pub night_excluded: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessReport {
    pub area_center: Geodetic,
    pub area_radius_km: f64,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub time_map: TimeMap,
    pub classes: Vec<ClassSummary>,
    pub windows: Vec<AccessWindow>,
    pub compute_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkQuery {
    pub point: Geodetic,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// Class ids; empty = every SATCOM class.
    #[serde(default)]
    pub classes: Vec<String>,
    #[serde(default)]
    pub epoch_mode: EpochMode,
    /// Step for visible-count and latency sampling, seconds.
    pub sample_step_s: f64,
    /// Return the per-sample timeline (for the link emulator).
    #[serde(default)]
    pub include_samples: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkWindow {
    pub class_id: String,
    pub satellite: String,
    pub norad_id: u64,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub duration_s: f64,
    pub peak_time: DateTime<Utc>,
    pub peak_elevation_deg: f64,
    pub peak_azimuth_deg: f64,
    pub min_range_km: f64,
    pub latency_ms_at_peak: Option<f64>,
    pub mask_deg: f64,
    pub clipped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkSat {
    pub norad_id: u64,
    pub satellite: String,
    pub elevation_deg: f64,
    pub azimuth_deg: f64,
    pub range_km: f64,
    pub latency_ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkSample {
    pub t: DateTime<Utc>,
    pub visible: usize,
    /// Highest-elevation satellite in view.
    pub best: Option<LinkSat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkClassReport {
    pub class_id: String,
    pub name: String,
    pub satellites: usize,
    pub stale_skipped: usize,
    /// In the current catalog but launched after the scenario start.
    pub not_yet_launched: usize,
    pub mask_deg: f64,
    pub path: Option<String>,
    pub user_down_mbps: Option<f64>,
    pub user_up_mbps: Option<f64>,
    pub coverage_fraction: f64,
    pub outages: Vec<Interval>,
    pub longest_outage_s: f64,
    pub visible_min: usize,
    pub visible_max: usize,
    pub visible_mean: f64,
    pub latency_ms_min: Option<f64>,
    pub latency_ms_median: Option<f64>,
    pub latency_ms_max: Option<f64>,
    pub windows: Vec<LinkWindow>,
    pub samples: Vec<LinkSample>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkReport {
    pub point: Geodetic,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub time_map: TimeMap,
    pub classes: Vec<LinkClassReport>,
    pub compute_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GnssQuery {
    pub point: Geodetic,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub step_s: f64,
    /// Class id (default "gps").
    pub class: String,
    pub mask_deg: Option<f64>,
    #[serde(default)]
    pub epoch_mode: EpochMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GnssSample {
    pub t: DateTime<Utc>,
    pub visible: usize,
    pub dop: Option<crate::gnss::Dop>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GnssReport {
    pub point: Geodetic,
    pub class_id: String,
    pub mask_deg: f64,
    pub satellites: usize,
    pub stale_skipped: usize,
    /// In the current catalog but launched after the scenario start.
    pub not_yet_launched: usize,
    pub time_map: TimeMap,
    pub visible_min: usize,
    pub visible_max: usize,
    pub pdop_max: Option<f64>,
    pub pdop_mean: Option<f64>,
    pub samples: Vec<GnssSample>,
    pub compute_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SatPosition {
    pub class_id: String,
    pub norad_id: u64,
    pub satellite: String,
    pub t: DateTime<Utc>,
    pub position: Geodetic,
    /// Field of regard (imaging) or coverage circle (comms), [lon, lat] ring.
    pub footprint: Option<Vec<[f64; 2]>>,
}
