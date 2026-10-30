//! Each vehicle's sensor reach now (bd murmur-4tq; docs/WEATHER.md, "Seeing the effect on
//! sensors"): for each sensor of a type that confirms a contact's presence (radar, EO/IR, sonar:
//! `murmur_decide::engage::sees`) and each domain it confirms it in, how far out the physical
//! model's prediction detects, recognises and identifies the same reference contact the catalog's
//! `physical_reach_m` is derived for (`tests/reaches.rs`), from where the vehicle is and at its
//! height now, in the conditions the running model would sense in there
//! (`physical::Engine::conditions_at`: the scenario's `[conditions]` and the observer's own
//! weather sample, the sun from the scenario clock), beside that clear-day reference, and what
//! keeps it short.
//!
//! Information only, as predicted link coverage is (`crate::coverage`): never logged, never a
//! check, read by nothing that decides. The decision engine's assessment still reads the
//! catalog's clear-day reach. No terrain: each reach is a disc (the contact moved away along the
//! ground east of the observer, as the generator moves it), the sonar's water the scenario's
//! stated depth.
//!
//! `limited_by` is derived from the model's own inputs, never guessed: the reach is computed
//! again with one input at the clear-day reference (visibility 20 km for a camera; the sun 45
//! degrees up for a camera; sea state 2 and 5 m/s of wind for a radar against a surface contact;
//! 5 m/s of wind and moderate shipping, or 50 m of water, for a sonar), and the input whose
//! reference gives back the most (at least a tenth and 20 m more) names it: `fog` (visibility
//! under 1 km) or `haze`, `night` (the sun more than 6 degrees down), `twilight` or `low sun` (the
//! thermal crossover), `sea clutter`, `sea noise`, `shallow water`. With none of those, a reach
//! under nine tenths of the reference that ends where the line of sight does is `horizon` (the
//! vehicle lower than the catalog's operating height). A sensor under water but sonar is
//! `submerged`.

use murmur_decide::{Domain as DDomain, SensorReach};

use super::physical::conditions::Conditions;
use super::physical::eo;
use super::physical::predict::{self, Ask, Prediction};
use super::physical::radar;
use super::sensing::{Observer, SensorSpec, SensorType};
use super::truth::{ContactTruth, Domain, TruthAffiliation};

/// The reference contact's kind: none the physical model knows, so it takes its domain's
/// signature for a contact of no known kind (`signature::kind_signature`).
pub const REFERENCE_KIND: &str = "unknown";

/// Each domain the engine assesses in, the decision layer's name for it, and the reference
/// contact's height: where the contacts process puts a contact that names no altitude
/// (`murmur-contacts` scenario: 300 m for an aircraft, 30 m deep under water, else at the
/// surface).
pub const DOMAINS: [(Domain, DDomain, f64); 4] = [
    (Domain::Air, DDomain::Air, 300.0),
    (Domain::Ground, DDomain::Ground, 0.0),
    (Domain::Surface, DDomain::Surface, 0.0),
    (Domain::Subsurface, DDomain::Subsurface, -30.0),
];

/// The reference conditions: the catalog table's (`catalog::catalog_table`): a clear day (20 km
/// visibility) with the sun 45 degrees up, sea state 2, the sonar's reference environment (50 m
/// of water, 5 m/s of wind, moderate shipping), no terrain.
pub fn reference_conditions() -> Conditions {
    Conditions { sun_elevation_deg: Some(45.0), ..Conditions::default() }
}

pub fn domain_name(d: DDomain) -> &'static str {
    match d {
        DDomain::Air => "air",
        DDomain::Ground => "ground",
        DDomain::Surface => "surface",
        DDomain::Subsurface => "subsurface",
    }
}

/// The sun's state for a display: the physical model's light (a day camera has its full light
/// with the sun up, none from 6 degrees below the horizon).
pub fn light(sun_deg: f64) -> &'static str {
    if sun_deg >= 0.0 {
        "day"
    } else if sun_deg >= -6.0 {
        "twilight"
    } else {
        "night"
    }
}

/// One sensor's reach in one domain now.
#[derive(Debug, Clone, PartialEq)]
pub struct DomainReach {
    pub domain: DDomain,
    pub detect_m: f64,
    pub recognise_m: f64,
    pub identify_m: f64,
    /// The catalog's `physical_reach_m` for this sensor and domain (none: the catalog has none).
    pub clear_day_m: Option<f64>,
    pub limited_by: Option<&'static str>,
    /// Each input away from its clear-day reference that shortens the reach, and the detection
    /// reach with that input at its reference, m.
    pub factors: Vec<(&'static str, f64)>,
    /// The detection reach ends where the line of sight does.
    pub horizon_limited: bool,
}

impl DomainReach {
    pub fn ratio(&self) -> Option<f64> {
        self.clear_day_m.filter(|c| *c > 0.0).map(|c| self.detect_m / c)
    }
}

/// One sensor's reaches now.
#[derive(Debug, Clone, PartialEq)]
pub struct SensorNow {
    pub kind: &'static str,
    /// The domain that matters most for this sensor on this vehicle: a sonar's under water, a
    /// ground vehicle's ground, else the surface (the harbour); the console draws it by default.
    pub primary: DDomain,
    pub domains: Vec<DomainReach>,
}

/// A sensor's chance curves against the reference contact `d` m east along the ground.
struct Line<'a> {
    spec: &'a SensorSpec,
    observer: &'a Observer,
    domain: Domain,
    height_m: f64,
    t: f64,
}

impl Line<'_> {
    fn target(&self, d: f64) -> ContactTruth {
        let m = super::physical::geometry::M_PER_DEG;
        let o = self.observer;
        ContactTruth {
            id: "reference".into(),
            name: "reference".into(),
            affiliation: TruthAffiliation::Unknown,
            domain: self.domain,
            kind: REFERENCE_KIND.into(),
            lat: o.lat,
            lon: o.lon + d / (m * o.lat.to_radians().cos()),
            alt_m: self.height_m,
            speed_mps: 0.0,
            course_deg: 0.0,
            emits: false,
            signature: 1.0,
            ais: None,
        }
    }

    fn at(&self, d: f64, cond: &Conditions) -> Prediction {
        let target = self.target(d);
        predict::predict(&Ask {
            spec: self.spec,
            observer: self.observer,
            target: &target,
            over: None,
            conditions: *cond,
            t: self.t,
            terrain: None,
            bathymetry: None,
            ais_load_per_min: 0.0,
        })
    }

    /// Where the reach's disc starts, m (ground): 1 m, a radar's at its minimum range (as the
    /// generator's).
    fn start_m(&self) -> f64 {
        if self.spec.kind == SensorType::Radar { radar::MIN_RANGE_M } else { 1.0 }
    }

    /// How far from the start the contact is detected at least half the time all the way, m:
    /// the generator's reach (`tests/reaches.rs`, its `inner_edge`), found more cheaply: the
    /// model's one crossing by bisection, then a check of 32 points short of it for a gap (an
    /// airborne radar's sea clutter near its nadir), and the gap's near edge if one is found.
    fn detect_m(&self, cond: &Conditions) -> f64 {
        let p = |d: f64| self.at(d, cond).p_detect;
        let start = self.start_m();
        if p(start) < 0.5 {
            return 0.0;
        }
        let (mut lo, mut hi) = (start, 1.0e6f64);
        if p(hi) >= 0.5 {
            lo = hi;
        } else {
            for _ in 0..60 {
                let mid = (lo * hi).sqrt();
                if p(mid) >= 0.5 { lo = mid } else { hi = mid }
            }
        }
        const N: usize = 32;
        let mut prev = start;
        for i in 1..N {
            let d = start * (lo / start).powf(i as f64 / N as f64);
            if p(d) < 0.5 {
                let (mut a, mut b) = (prev, d);
                for _ in 0..40 {
                    let m = 0.5 * (a + b);
                    if p(m) >= 0.5 { a = m } else { b = m }
                }
                return a;
            }
            prev = d;
        }
        lo
    }
}

/// Which input, away from its clear-day reference, shortens the reach: its name for the display
/// and the conditions with it at the reference.
fn counterfactuals(kind: SensorType, domain: Domain, cond: &Conditions) -> Vec<(&'static str, Conditions)> {
    let r = reference_conditions();
    let mut out = Vec::new();
    match kind {
        SensorType::EoIr => {
            if cond.visibility_m < r.visibility_m {
                let name = if cond.visibility_m < 1_000.0 { "fog" } else { "haze" };
                out.push((name, Conditions { visibility_m: r.visibility_m, ..*cond }));
            }
            let sun = cond.sun_elevation_deg.unwrap_or(45.0);
            if sun < 45.0 {
                let name = match light(sun) {
                    "night" => "night",
                    "twilight" => "twilight",
                    _ => "low sun",
                };
                out.push((name, Conditions { sun_elevation_deg: r.sun_elevation_deg, ..*cond }));
            }
        }
        SensorType::Radar if matches!(domain, Domain::Surface | Domain::Subsurface) => {
            if cond.sea_state > r.sea_state || cond.wind_mps > r.wind_mps {
                out.push((
                    "sea clutter",
                    Conditions { sea_state: cond.sea_state.min(r.sea_state), wind_mps: cond.wind_mps.min(r.wind_mps), ..*cond },
                ));
            }
        }
        SensorType::Sonar => {
            if cond.wind_mps > r.wind_mps || cond.shipping > r.shipping {
                out.push((
                    "sea noise",
                    Conditions { wind_mps: cond.wind_mps.min(r.wind_mps), shipping: cond.shipping.min(r.shipping), ..*cond },
                ));
            }
            if cond.water_depth_m < r.water_depth_m {
                out.push(("shallow water", Conditions { water_depth_m: r.water_depth_m, ..*cond }));
            }
        }
        _ => {}
    }
    out
}

/// The domain a sensor's reach is shown for by default.
fn primary(kind: SensorType, own_domain: &str, domains: &[DDomain]) -> DDomain {
    let want = match kind {
        SensorType::Sonar => DDomain::Subsurface,
        _ if own_domain == "ground" => DDomain::Ground,
        _ => DDomain::Surface,
    };
    if domains.contains(&want) { want } else { domains.first().copied().unwrap_or(want) }
}

/// One sensor's reaches now from observer `o` (its position, height, mast and whether it is
/// under water), in `cond` (the sun stated: the caller puts the clock's in), against the
/// catalog's clear-day reaches `clear`; none for a sensor of a type that confirms no contact's
/// presence (AIS, ESM). `own_domain`: the vehicle's own (`air`, `ground`, ...).
pub fn sensor_now(spec: &SensorSpec, o: &Observer, cond: &Conditions, t: f64, clear: Option<&SensorReach>, own_domain: &str) -> Option<SensorNow> {
    let kind = spec.kind.as_str();
    let domains: Vec<&(Domain, DDomain, f64)> = DOMAINS.iter().filter(|(_, d, _)| murmur_decide::engage::sees(kind, *d)).collect();
    if domains.is_empty() {
        return None;
    }
    let cond = cond.sane();
    let mut out = Vec::new();
    for (td, dd, h) in domains.iter().copied() {
        let clear_day_m = clear.and_then(|c| c.get(*dd));
        if o.submerged && spec.kind != SensorType::Sonar {
            out.push(DomainReach {
                domain: *dd,
                detect_m: 0.0,
                recognise_m: 0.0,
                identify_m: 0.0,
                clear_day_m,
                limited_by: Some("submerged"),
                factors: Vec::new(),
                horizon_limited: false,
            });
            continue;
        }
        let line = Line { spec, observer: o, domain: *td, height_m: *h, t };
        let detect_m = line.detect_m(&cond);
        let recognise_m = eo::range_at(0.5, |d| line.at(d, &cond).p_recognise);
        let identify_m = eo::range_at(0.5, |d| line.at(d, &cond).p_identify);
        let factors: Vec<(&'static str, f64)> = counterfactuals(spec.kind, *td, &cond)
            .into_iter()
            .map(|(name, c)| (name, line.detect_m(&c)))
            .filter(|(_, m)| *m > detect_m * 1.1 && *m - detect_m > 20.0)
            .collect();
        let horizon_limited = detect_m > 0.0 && detect_m < 1.0e6 && !line.at(detect_m * 1.01 + 1.0, &cond).line_of_sight;
        let mut reach = DomainReach { domain: *dd, detect_m, recognise_m, identify_m, clear_day_m, limited_by: None, factors, horizon_limited };
        reach.limited_by = reach
            .factors
            .iter()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(n, _)| *n)
            .or_else(|| (reach.horizon_limited && reach.ratio().is_some_and(|r| r < 0.9)).then_some("horizon"));
        out.push(reach);
    }
    let ds: Vec<DDomain> = out.iter().map(|r| r.domain).collect();
    Some(SensorNow { kind, primary: primary(spec.kind, own_domain, &ds), domains: out })
}
