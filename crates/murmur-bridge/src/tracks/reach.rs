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
//! The display is information only, as predicted link coverage is (`crate::coverage`): never
//! logged, never a check. From decision rules 32 (`murmur_decide::rules::REACH_NOW_SINCE`, bd
//! murmur-ahss) the decision engine reads the same computation: each vehicle's reaches go into its
//! facts as `reach_now` ([`fact`], prepared each tick outside the engine's lock,
//! `Tracks::prepare_reach`), which an identify order's standoff and an engagement's assessment
//! judge by and every such decision logs; an EO/IR's facts also carry its identification reach of
//! each size class's smallest member ([`CLASS_KINDS`], as the catalog's reaches by class are
//! derived). The inputs are rounded first ([`Inputs`]: the height, the visibility, the wind, the
//! sun), so that vehicles alike share one computation and the display and the decisions show the
//! same figures for the same inputs. No terrain: each reach is a disc (the contact moved away along
//! the ground east of the observer, as the generator moves it), the sonar's water the scenario's
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

use murmur_decide::{ClassReach, Domain as DDomain, SensorReach, SizeClass};

use super::physical::conditions::Conditions;
use super::physical::eo;
use super::physical::predict::{self, Ask, Prediction};
use super::physical::radar;
use super::sensing::{Observer, SensorSpec, SensorType};
use super::truth::{ContactTruth, Domain, TruthAffiliation};

/// The reference contact's kind: none the physical model knows, so it takes its domain's
/// signature for a contact of no known kind (`signature::kind_signature`).
pub const REFERENCE_KIND: &str = "unknown";

/// Rules 31 (bd murmur-8vj8): each size class a detection names (`signature::size_class`) that a
/// camera sees (`murmur_decide::engage::sees`: none under water), the reference contact an EO/IR's
/// reach by class is derived for, and its domain (whose height [`DOMAINS`] gives): the class's
/// smallest member among the kinds the physical model knows (`signature::kind_signature`, by
/// critical dimension), so that a reach by class is a camera's for the hardest of the class to make
/// out. The members (`signature::size_class_of`): small drone and unknown UAS (0.35 m); light
/// aircraft (4 m) and helicopter (5 m); small fast boat (3.5 m) and sailboat (5 m); patrol boat (8
/// m), fishing vessel (11 m), ferry (27 m), cargo ship and tanker (84 m); person (0.75 m); vehicle
/// (2.3 m) and truck (3 m). The catalog's reaches by class (`tests/reaches.rs`) and, from rules 32,
/// a camera's identification reach by class now ([`ClassNow`]) are both against these. The
/// headless planner's honest ceiling reads a size class as the same kinds (eval/headless/v3/
/// planner.py `SIZE_KIND`) but for `large_surface`, a cargo ship there.
pub const CLASS_KINDS: [(SizeClass, Domain, &str); 6] = [
    (SizeClass::SmallAir, Domain::Air, "small_drone"),
    (SizeClass::LargeAir, Domain::Air, "light_aircraft"),
    (SizeClass::SmallSurface, Domain::Surface, "small_fast_boat"),
    (SizeClass::LargeSurface, Domain::Surface, "patrol_boat"),
    (SizeClass::SmallGround, Domain::Ground, "person"),
    (SizeClass::LargeGround, Domain::Ground, "vehicle"),
];

/// The reference height of a contact in `domain`, m ([`DOMAINS`]).
pub fn reference_height_m(domain: Domain) -> f64 {
    DOMAINS.iter().find(|(d, _, _)| *d == domain).map_or(0.0, |(_, _, h)| *h)
}

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
    /// Rules 32: the identification reach as slant range from the sensor, m (0: nowhere), as an
    /// identify order's plan reads it.
    pub identify_slant_m: f64,
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

/// Rules 32: an EO/IR's identification reach now of one size class's smallest member
/// ([`CLASS_KINDS`]) at its domain's reference height.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassNow {
    pub class: SizeClass,
    /// Along the ground, m (0: nowhere).
    pub identify_m: f64,
    /// As slant range from the sensor, m (0: nowhere).
    pub identify_slant_m: f64,
}

/// One sensor's reaches now.
#[derive(Debug, Clone, PartialEq)]
pub struct SensorNow {
    pub kind: &'static str,
    /// The domain that matters most for this sensor on this vehicle: a sonar's under water, a
    /// ground vehicle's ground, else the surface (the harbour); the console draws it by default.
    pub primary: DDomain,
    pub domains: Vec<DomainReach>,
    /// Rules 32: an EO/IR's identification reach of each size class it sees (none for any other
    /// sensor).
    pub classes: Vec<ClassNow>,
    /// An EO/IR with a thermal channel (MWIR or LWIR).
    pub thermal: bool,
}

/// A sensor's chance curves against a reference contact of `kind` `d` m east along the ground.
struct Line<'a> {
    spec: &'a SensorSpec,
    observer: &'a Observer,
    domain: Domain,
    kind: &'a str,
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
            kind: self.kind.into(),
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

    /// The slant range from the sensor to the contact `d` m along the ground (0 for none).
    fn slant_m(&self, d: f64, cond: &Conditions) -> f64 {
        if d > 0.0 { self.at(d, cond).range_m } else { 0.0 }
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
                identify_slant_m: 0.0,
                clear_day_m,
                limited_by: Some("submerged"),
                factors: Vec::new(),
                horizon_limited: false,
            });
            continue;
        }
        let line = Line { spec, observer: o, domain: *td, kind: REFERENCE_KIND, height_m: *h, t };
        let detect_m = line.detect_m(&cond);
        let recognise_m = eo::range_at(0.5, |d| line.at(d, &cond).p_recognise);
        let identify_m = eo::range_at(0.5, |d| line.at(d, &cond).p_identify);
        let identify_slant_m = line.slant_m(identify_m, &cond);
        let factors: Vec<(&'static str, f64)> = counterfactuals(spec.kind, *td, &cond)
            .into_iter()
            .map(|(name, c)| (name, line.detect_m(&c)))
            .filter(|(_, m)| *m > detect_m * 1.1 && *m - detect_m > 20.0)
            .collect();
        let horizon_limited = detect_m > 0.0 && detect_m < 1.0e6 && !line.at(detect_m * 1.01 + 1.0, &cond).line_of_sight;
        let mut reach =
            DomainReach { domain: *dd, detect_m, recognise_m, identify_m, identify_slant_m, clear_day_m, limited_by: None, factors, horizon_limited };
        reach.limited_by = reach
            .factors
            .iter()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(n, _)| *n)
            .or_else(|| (reach.horizon_limited && reach.ratio().is_some_and(|r| r < 0.9)).then_some("horizon"));
        out.push(reach);
    }
    let ds: Vec<DDomain> = out.iter().map(|r| r.domain).collect();
    // rules 32: a camera's identification reach of each size class it sees, against the class's
    // smallest member at its domain's height (none from under water)
    let classes = if spec.kind == SensorType::EoIr {
        CLASS_KINDS
            .iter()
            .filter(|(_, td, _)| domains.iter().any(|(d, _, _)| d == td))
            .map(|(class, td, kind)| {
                if o.submerged {
                    return ClassNow { class: *class, identify_m: 0.0, identify_slant_m: 0.0 };
                }
                let line = Line { spec, observer: o, domain: *td, kind, height_m: reference_height_m(*td), t };
                let identify_m = eo::range_at(0.5, |d| line.at(d, &cond).p_identify);
                ClassNow { class: *class, identify_m, identify_slant_m: line.slant_m(identify_m, &cond) }
            })
            .collect()
    } else {
        Vec::new()
    };
    let thermal = spec.kind == SensorType::EoIr && spec.physical.channels.iter().any(|c| c.band.thermal());
    Some(SensorNow { kind, primary: primary(spec.kind, own_domain, &ds), domains: out, classes, thermal })
}

/// Rules 32: what a vehicle's reach now is computed from, rounded so that vehicles alike (a
/// fleet's many of one platform at one height, in one weather sample) share one computation, and
/// the figures are a function of these alone: its platform, its height (to 1 m below 100 m, 10 m
/// below 1 km, 100 m above), whether it is under water, and the conditions there with the sun
/// stated (the visibility and the water's depth to two significant figures, the sea state and the
/// wind to a half, the shipping to a twentieth, the sun to half a degree). The display shows the
/// reach for these inputs, and the decision facts carry it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Inputs {
    pub platform: String,
    alt_m: i64,
    submerged: bool,
    visibility_m: i64,
    sea_state_x2: i64,
    wind_x2: i64,
    shipping_x20: i64,
    depth_m: i64,
    sun_x2: i64,
}

/// `v` to two significant figures (0 for none).
fn two_figures(v: f64) -> i64 {
    if !(v.is_finite() && v > 0.0) {
        return 0;
    }
    let e = 10f64.powi(v.log10().floor() as i32 - 1).max(1.0);
    ((v / e).round() * e) as i64
}

impl Inputs {
    /// The inputs of a vehicle of `platform` `alt_m` m up (`submerged`: under water) in `cond`
    /// with the sun stated (`sun_deg`).
    pub fn of(platform: &str, alt_m: f64, submerged: bool, cond: &Conditions, sun_deg: f64) -> Inputs {
        let c = cond.sane();
        let a = if alt_m.is_finite() { alt_m } else { 0.0 };
        let step = if a.abs() < 100.0 {
            1.0
        } else if a.abs() < 1_000.0 {
            10.0
        } else {
            100.0
        };
        Inputs {
            platform: platform.to_string(),
            alt_m: ((a / step).round() * step) as i64,
            submerged,
            visibility_m: two_figures(c.visibility_m),
            sea_state_x2: (c.sea_state * 2.0).round() as i64,
            wind_x2: (c.wind_mps * 2.0).round() as i64,
            shipping_x20: (c.shipping * 20.0).round() as i64,
            depth_m: two_figures(c.water_depth_m),
            sun_x2: (if sun_deg.is_finite() { sun_deg } else { 45.0 } * 2.0).round() as i64,
        }
    }

    pub fn alt_m(&self) -> f64 {
        self.alt_m as f64
    }

    /// The conditions the reach is computed in, the sun stated.
    pub fn conditions(&self) -> Conditions {
        Conditions {
            visibility_m: self.visibility_m as f64,
            sea_state: self.sea_state_x2 as f64 / 2.0,
            wind_mps: self.wind_x2 as f64 / 2.0,
            shipping: self.shipping_x20 as f64 / 20.0,
            water_depth_m: self.depth_m as f64,
            sun_elevation_deg: Some(self.sun_x2 as f64 / 2.0),
        }
    }

    pub fn sun_deg(&self) -> f64 {
        self.sun_x2 as f64 / 2.0
    }
}

/// Rules 32: one vehicle's reaches now, as [`Inputs`] give them: each sensor's ([`sensor_now`])
/// and the decision fact made of them ([`fact`]).
#[derive(Debug, Clone, PartialEq)]
pub struct VehicleReach {
    pub inputs: Inputs,
    pub sensors: Vec<SensorNow>,
    pub fact: murmur_decide::ReachNow,
}

/// Rules 32: [`VehicleReach`] for a vehicle of `inputs` carrying `specs` on a mast of `mast_m`, its
/// own domain `own_domain`, against the catalog's clear-day reaches `clear` by sensor type. The
/// observer stands at a fixed place (the equator's prime meridian): with the sun stated and no
/// terrain nothing else of where it is moves a reach, so the figures are its inputs' alone.
pub fn vehicle_reach(
    inputs: &Inputs,
    specs: &[SensorSpec],
    mast_m: f64,
    own_domain: &str,
    clear: Option<&std::collections::HashMap<String, SensorReach>>,
    t: f64,
) -> VehicleReach {
    let o = Observer {
        uid: 0,
        lat: 0.0,
        lon: 0.0,
        alt_msl: inputs.alt_m(),
        mast_m,
        submerged: inputs.submerged,
        sensors: specs.to_vec(),
        ..Observer::default()
    };
    let cond = inputs.conditions();
    let sensors: Vec<SensorNow> = specs
        .iter()
        .filter_map(|spec| sensor_now(spec, &o, &cond, t, clear.and_then(|m| m.get(spec.kind.as_str())), own_domain))
        .collect();
    let fact = fact(inputs, &sensors);
    VehicleReach { inputs: inputs.clone(), sensors, fact }
}

/// Rules 32: the decision fact `reach_now` of a vehicle's reaches ([`murmur_decide::ReachNow`]):
/// each sensor's detection reach by domain along the ground and an EO/IR's identification reach by
/// domain and by size class as slant range, each to the metre, with the conditions it is for.
pub fn fact(inputs: &Inputs, sensors: &[SensorNow]) -> murmur_decide::ReachNow {
    fn set(r: &mut SensorReach, d: DDomain, m: f64) {
        let m = Some(m.max(0.0).round());
        match d {
            DDomain::Air => r.air = m,
            DDomain::Ground => r.ground = m,
            DDomain::Surface => r.surface = m,
            DDomain::Subsurface => r.subsurface = m,
        }
    }
    let sensors = sensors
        .iter()
        .map(|s| {
            let mut detect_m = SensorReach::default();
            for d in &s.domains {
                set(&mut detect_m, d.domain, d.detect_m);
            }
            let identify_m = (s.kind == "eo_ir").then(|| {
                let mut r = SensorReach::default();
                for d in &s.domains {
                    set(&mut r, d.domain, d.identify_slant_m);
                }
                if !s.classes.is_empty() {
                    let mut by = ClassReach::default();
                    for c in &s.classes {
                        let m = Some(c.identify_slant_m.max(0.0).round());
                        match c.class {
                            SizeClass::SmallAir => by.small_air = m,
                            SizeClass::LargeAir => by.large_air = m,
                            SizeClass::SmallSurface => by.small_surface = m,
                            SizeClass::LargeSurface => by.large_surface = m,
                            SizeClass::SmallGround => by.small_ground = m,
                            SizeClass::LargeGround => by.large_ground = m,
                            SizeClass::Subsurface => {}
                        }
                    }
                    r.size_class = Some(by);
                }
                r
            });
            murmur_decide::SensorNow { kind: s.kind.to_string(), detect_m, identify_m, thermal: s.thermal }
        })
        .collect();
    murmur_decide::ReachNow {
        alt_m: inputs.alt_m(),
        visibility_m: inputs.visibility_m as f64,
        sun_elevation_deg: inputs.sun_deg(),
        sensors,
    }
}
