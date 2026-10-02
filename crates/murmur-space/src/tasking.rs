//! Tasking options: given "this capability over this area in this window, product by then",
//! return ranked feasible collects and why every other pass was excluded. A human picks.
//!
//! Each candidate pass is found with a relaxed geometric envelope (the class limit plus
//! 20 deg), then sampled every second and checked against: the sensor's geometric limit,
//! daylight (optical), tasking lead time, the request window and the product deadline. The
//! best feasible sample in a pass becomes an option; a pass with none becomes an exclusion
//! naming the first constraint that could not be met.

use crate::access::{self, Constraint, SearchParams};
use crate::area::Area;
use crate::catalog::Capability;
use crate::geo::{Geodetic, Observer, ViewGeometry, sun_elevation_deg};
use crate::space::{
    ClassSats, Space, SpaceError, effective_resolution, scene_polygon, sensor_constraint,
};
use crate::timemap::{EpochMode, TimeMap};
use chrono::{DateTime, Duration, Utc};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Weather hook. No cloud model exists yet; optical options carry `None` and a note.
pub trait CloudModel: Sync {
    /// Probability that the target is cloud-free at `t` (scenario time), if known.
    fn cloud_free_probability(&self, at: &Geodetic, t: DateTime<Utc>) -> Option<f64>;
    fn name(&self) -> &str;
}

pub struct NoCloudModel;

impl CloudModel for NoCloudModel {
    fn cloud_free_probability(&self, _: &Geodetic, _: DateTime<Utc>) -> Option<f64> {
        None
    }
    fn name(&self) -> &str {
        "none (cloud cover not modelled)"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Rank {
    /// Earliest product in hand first (ties: finer resolution).
    #[default]
    EarliestProduct,
    /// Finest resolution first (ties: earlier product).
    BestResolution,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRequest {
    /// Any of these capabilities will do (e.g. [sar, optical]).
    pub capabilities: Vec<Capability>,
    pub area: Area,
    /// Collection window, scenario time.
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// When the request is made (scenario time); tasking lead time counts from here.
    pub now: DateTime<Utc>,
    pub product_deadline: Option<DateTime<Utc>>,
    /// Reject collects coarser than this, metres.
    pub max_resolution_m: Option<f64>,
    #[serde(default)]
    pub rank: Rank,
    /// Restrict to these class ids (empty = all classes with the capabilities).
    #[serde(default)]
    pub classes: Vec<String>,
    /// Offer routine collects from untaskable sensors (Dove-class).
    #[serde(default = "yes")]
    pub include_untaskable: bool,
    #[serde(default)]
    pub epoch_mode: EpochMode,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskOption {
    pub rank: usize,
    pub class_id: String,
    pub class_name: String,
    pub capability: Capability,
    pub satellite: String,
    pub norad_id: u64,
    pub label: Option<String>,
    pub collect_time: DateTime<Utc>,
    pub geometry: ViewGeometry,
    pub resolution_m: Option<f64>,
    pub sun_elevation_deg: f64,
    pub earliest_collect: DateTime<Utc>,
    /// collect + downlink + processing; None when the class's timing is unknown.
    pub product_time: Option<DateTime<Utc>>,
    pub taskable: bool,
    pub cloud_free_probability: Option<f64>,
    /// Approximate scene on the ground, [lon, lat] ring.
    pub scene: Option<Vec<[f64; 2]>>,
    /// Whether one scene (swath) is likely to cover the whole area.
    pub covers_area: Option<bool>,
    pub element_age_days: f64,
    /// Caveats: guessed timing, unknown figures, no cloud model, not taskable.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    /// Optical, sun below the class minimum.
    Night,
    /// Optical, the pass never comes within the off-nadir limit.
    OffNadir,
    /// SAR, the pass never brings the target into the usable incidence range.
    Incidence,
    /// The pass ends before the tasking lead time has elapsed.
    TooSoon,
    /// Feasible collect, but the product would arrive after the deadline.
    ProductTooLate,
    /// Best feasible resolution is coarser than requested.
    ResolutionTooCoarse,
    /// The sensor cannot be tasked (and untaskable sensors were not requested).
    NotTaskable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exclusion {
    pub class_id: String,
    pub satellite: String,
    pub norad_id: u64,
    /// Best-geometry time of the pass (scenario time).
    pub pass_time: DateTime<Utc>,
    pub reason: ExclusionReason,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExclusionCount {
    pub class_id: String,
    pub reason: ExclusionReason,
    pub passes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskingResult {
    pub request: TaskRequest,
    pub time_map: TimeMap,
    pub options: Vec<TaskOption>,
    pub exclusions: Vec<Exclusion>,
    pub exclusion_summary: Vec<ExclusionCount>,
    /// Classes with the capability that produced no candidate pass at all.
    pub classes_without_passes: Vec<String>,
    pub cloud_model: String,
    pub compute_ms: u64,
}

const SAMPLE_S: i64 = 1;
const ENVELOPE_EXTRA_DEG: f64 = 20.0;

struct Ctx<'a> {
    req: &'a TaskRequest,
    tm: TimeMap,
    target: Observer,
    cloud: &'a dyn CloudModel,
}

enum PassResult {
    Option(Box<TaskOption>),
    Excluded(Exclusion),
}

impl Space {
    pub fn tasking_options(
        &self,
        req: &TaskRequest,
        cloud: &dyn CloudModel,
    ) -> Result<TaskingResult, SpaceError> {
        let started = std::time::Instant::now();
        if req.to <= req.from {
            return Err(SpaceError::Invalid(
                "window end must be after its start".into(),
            ));
        }
        let tm = self.time_map(req.epoch_mode, req.from, req.to);
        let ctx = Ctx {
            req,
            tm,
            target: Observer::new(req.area.centroid()),
            cloud,
        };
        let classes: Vec<&ClassSats> = if req.classes.is_empty() {
            self.classes
                .iter()
                .filter(|c| req.capabilities.contains(&c.class.capability))
                .collect()
        } else {
            req.classes
                .iter()
                .map(|id| self.class(id))
                .collect::<Result<_, _>>()?
        };
        let (pfrom, pto) = (tm.to_prop(req.from), tm.to_prop(req.to));
        let mut options = Vec::new();
        let mut exclusions = Vec::new();
        let mut without = Vec::new();
        for cs in classes {
            if !cs.class.capability.is_imaging() {
                return Err(SpaceError::Invalid(format!(
                    "{} is not an imaging class",
                    cs.class.id
                )));
            }
            let (sats, _) = self.usable(cs, &tm, req.from, req.to);
            let results: Vec<PassResult> = sats
                .par_iter()
                .flat_map_iter(|sat| evaluate_sat(&ctx, cs, sat, pfrom, pto))
                .collect();
            if results.is_empty() {
                without.push(cs.class.id.clone());
            }
            for r in results {
                match r {
                    PassResult::Option(o) => options.push(*o),
                    PassResult::Excluded(e) => exclusions.push(e),
                }
            }
        }
        let far = DateTime::<Utc>::MAX_UTC;
        let res = |o: &TaskOption| o.resolution_m.unwrap_or(f64::INFINITY);
        match req.rank {
            Rank::EarliestProduct => options.sort_by(|a, b| {
                a.product_time
                    .unwrap_or(far)
                    .cmp(&b.product_time.unwrap_or(far))
                    .then(res(a).total_cmp(&res(b)))
            }),
            Rank::BestResolution => options.sort_by(|a, b| {
                res(a).total_cmp(&res(b)).then(
                    a.product_time
                        .unwrap_or(far)
                        .cmp(&b.product_time.unwrap_or(far)),
                )
            }),
        }
        // Taskable options first: an untaskable routine collect is a bonus, not a plan.
        options.sort_by_key(|o| !o.taskable);
        for (i, o) in options.iter_mut().enumerate() {
            o.rank = i + 1;
        }
        exclusions.sort_by_key(|e| e.pass_time);
        let mut counts: std::collections::BTreeMap<(String, ExclusionReason), usize> =
            Default::default();
        for e in &exclusions {
            *counts.entry((e.class_id.clone(), e.reason)).or_default() += 1;
        }
        Ok(TaskingResult {
            request: req.clone(),
            time_map: tm,
            options,
            exclusions,
            exclusion_summary: counts
                .into_iter()
                .map(|((class_id, reason), passes)| ExclusionCount {
                    class_id,
                    reason,
                    passes,
                })
                .collect(),
            classes_without_passes: without,
            cloud_model: cloud.name().to_string(),
            compute_ms: started.elapsed().as_millis() as u64,
        })
    }
}

fn envelope(c: Constraint) -> Constraint {
    match c {
        Constraint::MaxOffNadir(m) => Constraint::MaxOffNadir((m + ENVELOPE_EXTRA_DEG).min(60.0)),
        Constraint::Incidence { max, .. } => Constraint::Incidence {
            min: 0.0,
            max: (max + ENVELOPE_EXTRA_DEG).min(80.0),
        },
        other => other,
    }
}

fn minutes(m: Option<f64>) -> Duration {
    Duration::milliseconds((m.unwrap_or(0.0) * 60_000.0) as i64)
}

fn evaluate_sat(
    ctx: &Ctx,
    cs: &ClassSats,
    sat: &crate::orbit::Satellite,
    pfrom: DateTime<Utc>,
    pto: DateTime<Utc>,
) -> Vec<PassResult> {
    let class = &cs.class;
    let Some(sensor) = class.effective_sensor(&sat.name) else {
        return Vec::new();
    };
    let Some(limit) = sensor_constraint(class.capability, &sensor) else {
        return Vec::new();
    };
    let env = envelope(limit);
    let passes = access::find_windows(
        sat,
        &ctx.target,
        pfrom,
        pto,
        env.psi_max(sat.max_radius_km()),
        SearchParams {
            fine_step_s: 10.0,
            edge_tol_s: 0.5,
        },
        |_, g| env.holds(g),
    );
    let timing = class.timing.clone().unwrap_or_default();
    let lead = if sensor.taskable {
        minutes(timing.tasking_lead_min)
    } else {
        Duration::zero()
    };
    let earliest = ctx.req.now + lead;
    let product_delay = match (timing.downlink_latency_min, timing.processing_min) {
        (None, None) => None,
        (d, p) => Some(minutes(d) + minutes(p)),
    };
    let mut out = Vec::new();
    for p in passes {
        let best_t = access::peak_time(sat, &ctx.target, p.start, p.end);
        let pass_time = ctx.tm.to_scenario(best_t);
        let exclude = |reason, detail: String| {
            PassResult::Excluded(Exclusion {
                class_id: class.id.clone(),
                satellite: sat.name.clone(),
                norad_id: sat.norad_id,
                pass_time,
                reason,
                detail,
            })
        };
        if !sensor.taskable && !ctx.req.include_untaskable {
            out.push(exclude(
                ExclusionReason::NotTaskable,
                "fixed nadir strip; routine collects only".into(),
            ));
            continue;
        }
        // Sample the pass.
        let mut samples = Vec::new();
        let mut t = p.start;
        while t <= p.end {
            if let Some(g) = access::geometry(sat, &ctx.target, &t) {
                let st = ctx.tm.to_scenario(t);
                samples.push((t, st, g, sun_elevation_deg(&ctx.target, &st)));
            }
            t += Duration::seconds(SAMPLE_S);
        }
        let geo_ok: Vec<_> = samples.iter().filter(|s| limit.holds(&s.2)).collect();
        if geo_ok.is_empty() {
            let r = match (class.capability, limit) {
                (Capability::Sar, Constraint::Incidence { min, max }) => {
                    let best = samples
                        .iter()
                        .map(|s| s.2.incidence_deg)
                        .fold(f64::INFINITY, f64::min);
                    let worst = samples
                        .iter()
                        .map(|s| s.2.incidence_deg)
                        .fold(0.0, f64::max);
                    let detail = if best > max {
                        format!("incidence {best:.0} deg at best, usable {min:.0}-{max:.0}")
                    } else {
                        format!(
                            "incidence {best:.0}-{worst:.0} deg sampled, usable {min:.0}-{max:.0}"
                        )
                    };
                    exclude(ExclusionReason::Incidence, detail)
                }
                _ => {
                    let best = samples
                        .iter()
                        .map(|s| s.2.off_nadir_deg)
                        .fold(f64::INFINITY, f64::min);
                    let max = sensor.max_off_nadir_deg.unwrap_or(30.0);
                    exclude(
                        ExclusionReason::OffNadir,
                        format!("off-nadir {best:.1} deg at best, limit {max:.1}"),
                    )
                }
            };
            out.push(r);
            continue;
        }
        let min_sun = sensor.min_sun_elevation_deg.unwrap_or(0.0);
        let lit: Vec<_> = if class.capability == Capability::Optical {
            geo_ok.iter().filter(|s| s.3 >= min_sun).copied().collect()
        } else {
            geo_ok.clone()
        };
        if lit.is_empty() {
            let sun = geo_ok.iter().map(|s| s.3).fold(f64::NEG_INFINITY, f64::max);
            out.push(exclude(
                ExclusionReason::Night,
                format!("sun {sun:.0} deg, needs {min_sun:.0}"),
            ));
            continue;
        }
        let timely: Vec<_> = lit
            .iter()
            .filter(|s| s.1 >= earliest && s.1 >= ctx.req.from && s.1 <= ctx.req.to)
            .copied()
            .collect();
        if timely.is_empty() {
            out.push(exclude(
                ExclusionReason::TooSoon,
                format!(
                    "pass before earliest collect {} (lead {:.0} min)",
                    earliest.format("%H:%MZ"),
                    lead.num_seconds() as f64 / 60.0
                ),
            ));
            continue;
        }
        let res = |s: &&(DateTime<Utc>, DateTime<Utc>, ViewGeometry, f64)| {
            effective_resolution(class.capability, &sensor, &s.2).unwrap_or(f64::INFINITY)
        };
        // Best geometry inside the feasible part: finest resolution, then (SAR) lowest incidence.
        let Some(best) = timely.iter().min_by(|a, b| {
            res(a)
                .total_cmp(&res(b))
                .then(a.2.incidence_deg.total_cmp(&b.2.incidence_deg))
        }) else {
            continue;
        };
        let product_time = product_delay.map(|d| best.1 + d);
        if let (Some(deadline), Some(pt)) = (ctx.req.product_deadline, product_time)
            && pt > deadline
        {
            out.push(exclude(
                ExclusionReason::ProductTooLate,
                format!(
                    "collect {} -> product {} after deadline {}",
                    best.1.format("%H:%MZ"),
                    pt.format("%H:%MZ"),
                    deadline.format("%H:%MZ")
                ),
            ));
            continue;
        }
        let resolution = effective_resolution(class.capability, &sensor, &best.2);
        if let (Some(maxr), Some(r)) = (ctx.req.max_resolution_m, resolution)
            && r > maxr
        {
            out.push(exclude(
                ExclusionReason::ResolutionTooCoarse,
                format!("{r:.2} m at best, requested {maxr:.2} m"),
            ));
            continue;
        }
        let mut notes = Vec::new();
        for f in ["tasking_lead_min", "downlink_latency_min", "processing_min"] {
            if class.is_guessed(f) {
                notes.push(format!("{f} is an estimate"));
            }
        }
        if product_time.is_none() {
            notes.push("product timing unknown".into());
        }
        if ctx.req.product_deadline.is_some() && product_time.is_none() {
            notes.push("deadline not checked (timing unknown)".into());
        }
        if !sensor.taskable {
            notes
                .push("not taskable: routine collect if the sensor is imaging on this pass".into());
        }
        let cloud = if class.capability == Capability::Optical {
            let p = ctx.cloud.cloud_free_probability(&ctx.target.geo, best.1);
            if p.is_none() {
                notes.push("cloud cover not modelled".into());
            }
            p
        } else {
            None
        };
        let heading = ground_heading(sat, &best.0);
        let scene = sensor
            .swath_km
            .map(|w| scene_polygon(&ctx.target.geo, w, heading));
        let covers_area = sensor.swath_km.map(|w| w / 2.0 >= ctx.req.area.radius_km());
        out.push(PassResult::Option(Box::new(TaskOption {
            rank: 0,
            class_id: class.id.clone(),
            class_name: class.name.clone(),
            capability: class.capability,
            satellite: sat.name.clone(),
            norad_id: sat.norad_id,
            label: sensor.label.clone(),
            collect_time: best.1,
            geometry: best.2,
            resolution_m: resolution,
            sun_elevation_deg: best.3,
            earliest_collect: earliest,
            product_time,
            taskable: sensor.taskable,
            cloud_free_probability: cloud,
            scene,
            covers_area,
            element_age_days: sat.element_age_days(&best.0),
            notes,
        })));
    }
    out
}

/// Ground-track heading at `t` (propagation time), degrees from north.
fn ground_heading(sat: &crate::orbit::Satellite, t: &DateTime<Utc>) -> f64 {
    let a = sat.state(t).map(|s| s.geodetic());
    let b = sat
        .state(&(*t + Duration::seconds(1)))
        .map(|s| s.geodetic());
    match (a, b) {
        (Ok(a), Ok(b)) => a.bearing_to(&b),
        _ => 0.0,
    }
}
