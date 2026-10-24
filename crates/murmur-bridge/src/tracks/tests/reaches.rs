//! The decision engine's sensor reaches under the physical model (bd murmur-1u4; docs/SENSING.md,
//! "The decision engine's reaches"). For each catalog sensor of a type that confirms a contact's
//! presence or absence (`murmur_decide::engage::sees`: radar, EO/IR, sonar) and each domain it
//! confirms it in, how far out the physical model's prediction for that sensor, pointed at the
//! domain's reference contact in the reference conditions, detects it at least half the time
//! all the way: the catalog's `physical_reach_m`, which the engine reads in a run on the physical
//! model (`murmur_decide::Catalog::reaching`) where it reads `range_m` on the legacy one.
//! Derived, never tuned (decision W3), and never edited by hand:
//!
//! ```text
//! cargo test -p murmur-bridge write_physical_reaches -- --ignored --nocapture
//! ```
//!
//! rewrites the `physical_reach_m` line after each platform's `sensors` in sim/platforms.toml
//! (and prints the table SENSING.md quotes), and `physical_reaches_are_the_generators` holds the
//! committed catalog to this generator: a change to the physical model or to a sensor's figures
//! that moves a reach fails it until the line above is run again and the catalog committed.

use std::collections::BTreeMap;
use std::path::Path;

use murmur_decide::Domain as DDomain;

use super::super::physical::conditions::Conditions;
use super::super::physical::predict::{self, Ask};
use super::super::sensing::{Observer, SensorCatalog, SensorSpec};
use super::super::truth::{ContactTruth, Domain, TruthAffiliation};
use super::{BASE, at, catalog};

const PLATFORMS: &str = include_str!("../../../../../sim/platforms.toml");

/// The catalog key the reaches are written under.
const KEY: &str = "physical_reach_m";

/// The reference contact's kind: none the physical model knows, so it takes its domain's
/// signature for a contact of no known kind (`signature::kind_signature`).
const REFERENCE_KIND: &str = "unknown";

/// Each domain the engine assesses in, the decision layer's name for it, and the reference
/// contact's height: where the contacts process puts a contact that names no altitude
/// (`murmur-contacts` scenario: 300 m for an aircraft, 30 m deep under water, else at the
/// surface).
const DOMAINS: [(Domain, DDomain, f64); 4] = [
    (Domain::Air, DDomain::Air, 300.0),
    (Domain::Ground, DDomain::Ground, 0.0),
    (Domain::Surface, DDomain::Surface, 0.0),
    (Domain::Subsurface, DDomain::Subsurface, -30.0),
];

/// The reference conditions: the catalog table's (`catalog::catalog_table`): a clear day (20 km
/// visibility) with the sun 45 degrees up, sea state 2, the sonar's reference environment (50 m
/// of water, 5 m/s of wind, moderate shipping), no terrain.
fn conditions() -> Conditions {
    Conditions { sun_elevation_deg: Some(45.0), ..Conditions::default() }
}

/// The scenario clock the predictions are asked at (the sun is the conditions' own).
const T: f64 = 1_781_467_800.0;

fn name(d: DDomain) -> &'static str {
    match d {
        DDomain::Air => "air",
        DDomain::Ground => "ground",
        DDomain::Surface => "surface",
        DDomain::Subsurface => "subsurface",
    }
}

/// A platform's operating height and domain from the catalog: a stand-in's `alt_m`, else the
/// SITL platform's `typical_alt_m` (as the catalog table's).
fn operating(table: &toml::Table, platform: &str) -> (f64, String) {
    let p = table.get(platform).and_then(|p| p.as_table()).expect(platform);
    let num = |k: &str| p.get(k).and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)));
    let alt = num("alt_m").or_else(|| num("typical_alt_m")).unwrap_or(0.0);
    (alt, p.get("domain").and_then(|v| v.as_str()).unwrap_or("surface").to_string())
}

/// One sensor of a platform at its operating height and a contact of `kind` in `domain` at
/// `height_m`, moved east of it along the ground (as `predict::ranges` moves a contact along its
/// line), in the reference conditions.
struct Line<'a> {
    observer: Observer,
    spec: &'a SensorSpec,
    kind: &'a str,
    domain: Domain,
    height_m: f64,
}

impl<'a> Line<'a> {
    fn new(cat: &SensorCatalog, table: &toml::Table, platform: &str, spec: &'a SensorSpec, kind: &'a str, domain: Domain, height_m: f64) -> Line<'a> {
        let ps = cat.get(platform).expect(platform);
        let (alt, own) = operating(table, platform);
        let observer = Observer {
            uid: 1,
            lat: BASE.0,
            lon: BASE.1,
            alt_msl: alt,
            mast_m: ps.mast_m,
            submerged: own == "subsurface" && alt < -0.5,
            sensors: vec![spec.clone()],
            ..Observer::default()
        };
        Line { observer, spec, kind, domain, height_m }
    }

    /// The contact `d` metres east along the ground.
    fn target(&self, d: f64) -> ContactTruth {
        let (lat, lon) = at(BASE.0, BASE.1, d, 0.0);
        ContactTruth {
            id: "reference".into(),
            name: "reference".into(),
            affiliation: TruthAffiliation::Unknown,
            domain: self.domain,
            kind: self.kind.into(),
            lat,
            lon,
            alt_m: self.height_m,
            speed_mps: 0.0,
            course_deg: 0.0,
            emits: false,
            signature: 1.0,
            ais: None,
        }
    }

    fn ask<'b>(&'b self, target: &'b ContactTruth) -> Ask<'b> {
        Ask {
            spec: self.spec,
            observer: &self.observer,
            target,
            over: None,
            conditions: conditions(),
            t: T,
            terrain: None,
            bathymetry: None,
            ais_load_per_min: 0.0,
        }
    }

    /// A pointed sensor's chance to detect the contact `d` metres away.
    fn p_detect(&self, d: f64) -> f64 {
        let t = self.target(d);
        predict::predict(&self.ask(&t)).p_detect
    }

    /// The reach, m: how far out the contact is detected at least half the time all the way (0:
    /// not even at 1 m). Where the detection probability only falls with distance this is the
    /// model's D range (`predict::ranges`, which bisects for one crossing); where it does not,
    /// the disc covered throughout, never a crossing beyond a gap (an airborne radar's sea
    /// clutter rule cuts a small surface contact out in rings near the nadir, and out
    /// altogether beyond).
    fn reach_m(&self) -> f64 {
        inner_edge(|d| self.p_detect(d))
    }

    /// The model's D range (`predict::ranges`), for the table.
    fn model_d_m(&self) -> f64 {
        let t = self.target(1000.0);
        predict::ranges(&self.ask(&t))[0]
    }
}

/// How far from 1 m `p` stays at or above one half, m (0: not at 1 m; at most 1,000 km, as
/// `eo::range_at`): its first fall below it on a 1 % grid, refined by bisection.
fn inner_edge(p: impl Fn(f64) -> f64) -> f64 {
    if p(1.0) < 0.5 {
        return 0.0;
    }
    let mut lo = 1.0f64;
    while lo < 1.0e6 {
        let hi = (lo * 1.01).min(1.0e6);
        if p(hi) < 0.5 {
            let (mut a, mut b) = (lo, hi);
            for _ in 0..50 {
                let m = 0.5 * (a + b);
                if p(m) >= 0.5 {
                    a = m;
                } else {
                    b = m;
                }
            }
            return a;
        }
        lo = hi;
    }
    1.0e6
}

/// A platform's reaches, in its catalog's sensor order: each sensor type the engine assesses
/// with and its reach per domain, m rounded to 10 (a domain it does not reach, under 5 m, left
/// out).
type Reaches = Vec<(String, Vec<(DDomain, i64)>)>;

/// Every platform's reaches (none for a platform with no sensor the engine assesses with).
fn derive(cat: &SensorCatalog, table: &toml::Table) -> BTreeMap<String, Reaches> {
    let mut out = BTreeMap::new();
    for id in table.keys() {
        let Some(ps) = cat.get(id) else { continue };
        let mut reaches: Reaches = Vec::new();
        for s in &ps.sensors {
            let kind = s.kind.as_str();
            let domains: Vec<_> = DOMAINS.iter().filter(|(_, d, _)| murmur_decide::engage::sees(kind, *d)).collect();
            if domains.is_empty() {
                continue;
            }
            assert!(!reaches.iter().any(|(k, _)| k == kind), "[{id}] carries two {kind} sensors: the reaches are by sensor type");
            let by_domain = domains
                .iter()
                .map(|(td, dd, h)| (*dd, ((Line::new(cat, table, id, s, REFERENCE_KIND, *td, *h).reach_m() / 10.0).round() as i64) * 10))
                .filter(|(_, m)| *m > 0)
                .collect();
            reaches.push((kind.to_string(), by_domain));
        }
        if !reaches.is_empty() {
            out.insert(id.clone(), reaches);
        }
    }
    out
}

/// The catalog line for a platform's reaches.
fn line(reaches: &Reaches) -> String {
    let sensors: Vec<String> = reaches
        .iter()
        .map(|(kind, ds)| {
            let ds: Vec<String> = ds.iter().map(|(d, m)| format!("{} = {m}", name(*d))).collect();
            if ds.is_empty() { format!("{kind} = {{}}") } else { format!("{kind} = {{ {} }}", ds.join(", ")) }
        })
        .collect();
    format!("{KEY} = {{ {} }}", sensors.join(", "))
}

/// Whether a catalog line sets `key` (a key at the start of the line, before its `=`).
fn sets(l: &str, key: &str) -> bool {
    l.trim_start().strip_prefix(key).is_some_and(|r| r.trim_start().starts_with('='))
}

/// `text` (sim/platforms.toml) with each platform's `physical_reach_m` line taken out and the
/// one `lines` gives it put right after the end of its `sensors` value.
fn rewrite(text: &str, lines: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut section: Option<&str> = None;
    // the open brackets of a `sensors` value being read (none: not in one)
    let mut depth: Option<i64> = None;
    for l in text.lines() {
        if sets(l, KEY) {
            continue;
        }
        out.push_str(l);
        out.push('\n');
        let t = l.trim();
        if let Some(id) = t.strip_prefix('[').and_then(|r| r.strip_suffix(']')).filter(|id| !id.starts_with('[')) {
            section = Some(id);
        }
        if depth.is_none() && sets(l, "sensors") {
            depth = Some(0);
        }
        if let Some(n) = depth.as_mut() {
            // the value, not a trailing comment (no catalog string holds a bracket or a #)
            let v = l.split('#').next().unwrap_or("");
            *n += v.matches('[').count() as i64 - v.matches(']').count() as i64;
            if *n <= 0 {
                depth = None;
                if let Some(line) = section.and_then(|s| lines.get(s)) {
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// The catalog as this generator writes it, after checking that only `physical_reach_m` lines
/// changed and that every platform with reaches got its line.
fn generated(text: &str) -> String {
    let table: toml::Table = text.parse().expect("platforms.toml");
    let cat = SensorCatalog::from_toml(text).expect("catalog");
    let derived = derive(&cat, &table);
    let lines: BTreeMap<String, String> = derived.iter().map(|(id, r)| (id.clone(), line(r))).collect();
    let out = rewrite(text, &lines);
    let strip = |t: &str| -> toml::Table {
        let mut t: toml::Table = t.parse().expect("toml");
        for (_, p) in t.iter_mut() {
            if let Some(p) = p.as_table_mut() {
                p.remove(KEY);
            }
        }
        t
    };
    assert_eq!(strip(&out), strip(text), "the generator changed more than the {KEY} lines");
    let written: toml::Table = out.parse().expect("the generated catalog");
    for id in derived.keys() {
        assert!(written.get(id).and_then(|p| p.get(KEY)).is_some(), "[{id}]: no {KEY} written (no sensors line found?)");
    }
    out
}

/// The committed catalog is the generator's: every `physical_reach_m` value and line, as
/// `write_physical_reaches` writes them, and the decision layer reads them back as written.
#[test]
fn physical_reaches_are_the_generators() {
    let want = generated(PLATFORMS);
    if want != PLATFORMS {
        let diff: Vec<String> = want
            .lines()
            .filter(|l| sets(l, KEY) && !PLATFORMS.lines().any(|c| c == *l))
            .map(str::to_string)
            .collect();
        panic!(
            "sim/platforms.toml's {KEY} is not the physical model's (run `cargo test -p murmur-bridge write_physical_reaches -- --ignored` and commit it); the generator writes:\n{}",
            diff.join("\n")
        );
    }
    // the decision layer reads each sensor's reaches as written
    let decide = murmur_decide::Catalog::from_toml(PLATFORMS).expect("the decision catalog").reaching(murmur_decide::Reaches::Physical);
    let table: toml::Table = PLATFORMS.parse().expect("platforms.toml");
    let derived = derive(&catalog(), &table);
    for (id, p) in &decide.platforms {
        for s in &p.sensors {
            let want = derived.get(id).and_then(|r| r.iter().find(|(k, _)| *k == s.kind));
            match (want, &s.reach_m) {
                (None, None) => {}
                (Some((_, ds)), Some(r)) => {
                    for (_, d, _) in DOMAINS {
                        let w = ds.iter().find(|(x, _)| *x == d).map(|(_, m)| *m as f64);
                        assert_eq!(r.get(d), w, "[{id}] {}: {} reach", s.kind, name(d));
                    }
                }
                other => panic!("[{id}] {}: {other:?}", s.kind),
            }
        }
    }
    // and the legacy facts carry none
    let legacy = murmur_decide::Catalog::from_toml(PLATFORMS).expect("the decision catalog");
    assert!(legacy.platforms.values().all(|p| p.sensors.iter().all(|s| s.reach_m.is_none())));
}

/// The table SENSING.md quotes: each reach beside `range_m`, the model's D range where it
/// differs (a curve that does not only fall: `*`), and for context the reach for a small fast
/// boat (surface) and a small drone (air, 80 m up) with the same sensor.
fn table(text: &str) {
    let table: toml::Table = text.parse().expect("platforms.toml");
    let cat = SensorCatalog::from_toml(text).expect("catalog");
    let derived = derive(&cat, &table);
    let km = |m: f64| (m / 10.0).round() / 100.0;
    eprintln!("platform:sensor          range_m km    air km    ground km   surface km  subsurface km   (fast boat, small drone km)");
    for (id, reaches) in &derived {
        let ps = cat.get(id).expect(id);
        for (kind, ds) in reaches {
            let s = ps.sensors.iter().find(|s| s.kind.as_str() == kind).expect("sensor");
            let col = |d: DDomain| -> String {
                if !murmur_decide::engage::sees(kind, d) {
                    return "-".into();
                }
                let (td, _, h) = DOMAINS.iter().find(|(_, x, _)| *x == d).expect("domain");
                let r = ds.iter().find(|(x, _)| *x == d).map_or(0.0, |(_, m)| *m as f64);
                let model = Line::new(&cat, &table, id, s, REFERENCE_KIND, *td, *h).model_d_m();
                if (model - r).abs() > 10.0 { format!("{:.2}*{:.2}", km(r), km(model)) } else { format!("{:.2}", km(r)) }
            };
            let boat = murmur_decide::engage::sees(kind, DDomain::Surface).then(|| km(Line::new(&cat, &table, id, s, "small_fast_boat", Domain::Surface, 0.0).reach_m()));
            let drone = murmur_decide::engage::sees(kind, DDomain::Air).then(|| km(Line::new(&cat, &table, id, s, "small_drone", Domain::Air, 80.0).reach_m()));
            eprintln!(
                "{:<26} {:9.2} {:>9} {:>12} {:>12} {:>14}   ({}, {})",
                format!("{id}:{kind}"),
                km(s.range_m),
                col(DDomain::Air),
                col(DDomain::Ground),
                col(DDomain::Surface),
                col(DDomain::Subsurface),
                boat.map_or("-".into(), |v| format!("{v:.2}")),
                drone.map_or("-".into(), |v| format!("{v:.2}")),
            );
        }
    }
}

/// Rewrites sim/platforms.toml's `physical_reach_m` lines from the physical model, and prints
/// the table (`--nocapture`).
#[test]
#[ignore]
fn write_physical_reaches() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sim/platforms.toml");
    let text = std::fs::read_to_string(&path).expect("sim/platforms.toml");
    let out = generated(&text);
    if out != text {
        std::fs::write(&path, &out).expect("write sim/platforms.toml");
        eprintln!("wrote {}", path.display());
    } else {
        eprintln!("{} already holds the physical model's reaches", path.display());
    }
    table(&out);
}

/// The table alone (`--nocapture`), from the catalog as committed.
#[test]
#[ignore]
fn physical_reach_table() {
    table(PLATFORMS);
}

/// The reach is the first fall below one half: the model's crossing where the curve only falls,
/// the near edge of a gap where it does not.
#[test]
fn the_reach_stops_at_the_first_gap() {
    let falling = |d: f64| if d < 2_500.0 { 0.9 } else { 0.1 };
    assert!((inner_edge(falling) - 2_500.0).abs() < 1.0);
    let gap = |d: f64| if !(800.0..1_200.0).contains(&d) && d < 5_000.0 { 0.9 } else { 0.1 };
    assert!((inner_edge(gap) - 800.0).abs() < 1.0);
    assert_eq!(inner_edge(|_| 0.4), 0.0);
    assert_eq!(inner_edge(|_| 0.6), 1.0e6);
}

/// The rewrite puts a platform's line after its sensors however they are laid out, replaces an
/// old one wherever it was, and leaves a platform without one alone.
#[test]
fn the_rewrite_places_the_line_after_the_sensors() {
    let text = "[a]\nsensors = [\n  { type = \"radar\", range_m = 1 },  # [1]\n]\nmast_m = 3\n\n[b]\nphysical_reach_m = { old = {} }\nsensors = [{ type = \"eo_ir\", range_m = 2, channels = [{ band = \"day\", wide_deg = 1, px = 2 }] }]\nx = 1\n\n[c]\nsensors = []\n";
    let lines: BTreeMap<String, String> = [("a", "physical_reach_m = { radar = {} }"), ("b", "physical_reach_m = { eo_ir = { air = 10 } }")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let want = "[a]\nsensors = [\n  { type = \"radar\", range_m = 1 },  # [1]\n]\nphysical_reach_m = { radar = {} }\nmast_m = 3\n\n[b]\nsensors = [{ type = \"eo_ir\", range_m = 2, channels = [{ band = \"day\", wide_deg = 1, px = 2 }] }]\nphysical_reach_m = { eo_ir = { air = 10 } }\nx = 1\n\n[c]\nsensors = []\n";
    assert_eq!(rewrite(text, &lines), want);
    assert_eq!(rewrite(want, &lines), want, "a second run changes nothing");
}
